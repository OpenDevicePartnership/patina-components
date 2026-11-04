//! Absolute Pointer Protocol FFI Support.
//!
//! This module manages the Absolute Pointer Protocol FFI.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
use alloc::boxed::Box;

use r_efi::{efi, protocols};

use hidparser::report_data_types::Usage;

use patina::component::service::uefi_services::{
    event::{Event, EventServicesExt, Tpl},
    handle::Handle,
    protocol::ProtocolPtr,
};

use super::{
    AXIS_RESOLUTION, BUTTON_MAX, BUTTON_MIN, DIGITIZER_SWITCH_MAX, DIGITIZER_SWITCH_MIN, GENERIC_DESKTOP_WHEEL,
    GENERIC_DESKTOP_X, GENERIC_DESKTOP_Y, GENERIC_DESKTOP_Z, PointerHidHandler,
};
use crate::{HidServices, to_status};

/// FFI context
/// # Safety
/// The handler must outlive this context and be accessed only through shared references derived from its root
/// pointer. Mutable input state is protected by a TPL_NOTIFY mutex. If uninstall fails, both allocations stay alive.
///
/// The absolute_pointer element must be the first element in the structure so that the full structure can be
/// recovered by simple casting.
#[repr(C)]
pub(crate) struct AbsolutePointerFfi {
    absolute_pointer: protocols::absolute_pointer::Protocol,
    services: HidServices,
    pointer_handler: *const PointerHidHandler,
}

impl Drop for AbsolutePointerFfi {
    fn drop(&mut self) {
        if !self.absolute_pointer.mode.is_null() {
            // SAFETY: mode was created via Box::into_raw during install and is non-null (checked above).
            drop(unsafe { Box::from_raw(self.absolute_pointer.mode) });
        }
    }
}

impl AbsolutePointerFfi {
    /// Installs the absolute pointer protocol. If successful, returns the installed context pointer, which must be
    /// passed to [`Self::uninstall`].
    pub(crate) fn install(
        services: HidServices,
        controller: Handle,
        pointer_handler: *const PointerHidHandler,
        mode: protocols::absolute_pointer::Mode,
    ) -> Result<*mut Self, efi::Status> {
        let wait_for_input_event = services
            .events
            .on_wait_event(Tpl::Notify, move |event| {
                Self::signal_if_input_available(services, pointer_handler, event);
            })
            .map_err(to_status)?;

        let pointer_ctx = Box::into_raw(Box::new(AbsolutePointerFfi {
            absolute_pointer: protocols::absolute_pointer::Protocol {
                reset: Self::absolute_pointer_reset,
                get_state: Self::absolute_pointer_get_state,
                mode: Box::into_raw(Box::new(mode)),
                wait_for_input: wait_for_input_event.as_raw(),
            },
            services,
            pointer_handler,
        }));

        let interface = ProtocolPtr::from_raw(pointer_ctx.cast()).expect("Box pointers are non-null");
        match services.protocols.install_interface(
            Some(controller),
            protocols::absolute_pointer::PROTOCOL_GUID.into(),
            interface,
        ) {
            Ok(_) => Ok(pointer_ctx),
            Err(error) => {
                if let Err(error) = services.events.close_event(wait_for_input_event) {
                    log::error!("Failed to close wait_for_input after install failure: {:?}", error);
                }
                // SAFETY: pointer_ctx was created by Box::into_raw above and was not published.
                drop(unsafe { Box::from_raw(pointer_ctx) });
                Err(to_status(error))
            }
        }
    }

    // Initializes the absolute_pointer mode structure.
    pub(super) fn initialize_mode(pointer_handler: &PointerHidHandler) -> protocols::absolute_pointer::Mode {
        let mut mode: protocols::absolute_pointer::Mode = Default::default();

        if pointer_handler.processor.supported_usages.contains(&Usage::from(GENERIC_DESKTOP_X)) {
            mode.absolute_max_x = AXIS_RESOLUTION;
            mode.absolute_min_x = 0;
        } else {
            log::warn!("No x-axis usages found in the report descriptor.");
        }

        if pointer_handler.processor.supported_usages.contains(&Usage::from(GENERIC_DESKTOP_Y)) {
            mode.absolute_max_y = AXIS_RESOLUTION;
            mode.absolute_min_y = 0;
        } else {
            log::warn!("No y-axis usages found in the report descriptor.");
        }

        if pointer_handler.processor.supported_usages.contains(&Usage::from(GENERIC_DESKTOP_Z))
            || pointer_handler.processor.supported_usages.contains(&Usage::from(GENERIC_DESKTOP_WHEEL))
        {
            mode.absolute_max_z = AXIS_RESOLUTION;
            mode.absolute_min_z = 0;
        }

        let has_multiple_buttons = pointer_handler
            .processor
            .supported_usages
            .iter()
            .filter(|x| matches!((**x).into(), BUTTON_MIN..=BUTTON_MAX | DIGITIZER_SWITCH_MIN..=DIGITIZER_SWITCH_MAX))
            .nth(1)
            .is_some();

        if has_multiple_buttons {
            mode.attributes |= protocols::absolute_pointer::SUPPORTS_ALT_ACTIVE;
        }

        mode
    }

    /// Uninstalls the absolute pointer protocol and frees the context created by [`Self::install`].
    pub(crate) fn uninstall(
        services: HidServices,
        controller: Handle,
        pointer_ctx: *mut Self,
    ) -> Result<(), efi::Status> {
        let interface = ProtocolPtr::from_raw(pointer_ctx.cast()).ok_or(efi::Status::INVALID_PARAMETER)?;
        if let Err(error) = services.protocols.uninstall_interface(
            controller,
            protocols::absolute_pointer::PROTOCOL_GUID.into(),
            interface,
        ) {
            log::error!("Failed to uninstall absolute_pointer interface: {:?}", error);
            // The caller must retain the handler while this context is still installed.
            return Err(to_status(error));
        }

        // SAFETY: pointer_ctx was created via Box::into_raw in install and is no longer published.
        let pointer_ctx = unsafe { Box::from_raw(pointer_ctx) };

        if let Some(event) = Event::from_raw(pointer_ctx.absolute_pointer.wait_for_input)
            && let Err(error) = services.events.close_event(event)
        {
            log::error!("Failed to close absolute_pointer.wait_for_input event: {:?}", error);
            // Leak pointer_ctx so a still-live event callback doesn't use freed memory.
            core::mem::forget(pointer_ctx);
            return Err(to_status(error));
        }

        // pointer_ctx drops here, freeing the mode allocation via AbsolutePointerFfi::Drop.
        Ok(())
    }

    /// Signals `event` if pointer input is available.
    ///
    /// This is the notify logic for the wait_for_input event.
    fn signal_if_input_available(services: HidServices, pointer_handler: *const PointerHidHandler, event: Event) {
        // SAFETY: pointer_handler validity is ensured by the protocol lifecycle.
        if let Some(pointer_handler) = unsafe { pointer_handler.as_ref() } {
            if pointer_handler.state.lock().state_changed
                && let Err(error) = services.events.signal_event(event)
            {
                log::error!("Failed to signal wait_for_input: {:?}", error);
            }
        } else {
            log::error!("absolute_pointer_wait_for_input invoked after pointer dropped.");
        }
    }

    // Resets the pointer state — part of the absolute_pointer protocol interface.
    extern "efiapi" fn absolute_pointer_reset(
        this: *mut protocols::absolute_pointer::Protocol,
        _extended_verification: bool,
    ) -> efi::Status {
        if this.is_null() {
            return efi::Status::INVALID_PARAMETER;
        }
        // SAFETY: this is the first field of #[repr(C)] AbsolutePointerFfi, non-null (checked above).
        let context =
            unsafe { (this as *const AbsolutePointerFfi).as_ref() }.expect("context pointer should not be null");
        // SAFETY: pointer_handler validity is ensured by the protocol lifecycle.
        if let Some(pointer_handler) = unsafe { context.pointer_handler.as_ref() } {
            pointer_handler.state.lock().reset();
            efi::Status::SUCCESS
        } else {
            log::error!("absolute_pointer_reset invoked after pointer dropped.");
            efi::Status::DEVICE_ERROR
        }
    }

    // Returns the current pointer state — part of the absolute_pointer protocol interface.
    extern "efiapi" fn absolute_pointer_get_state(
        this: *mut protocols::absolute_pointer::Protocol,
        state: *mut protocols::absolute_pointer::State,
    ) -> efi::Status {
        if this.is_null() || state.is_null() {
            return efi::Status::INVALID_PARAMETER;
        }

        // SAFETY: this is the first field of #[repr(C)] AbsolutePointerFfi, non-null (checked above).
        let context =
            unsafe { (this as *const AbsolutePointerFfi).as_ref() }.expect("context pointer should not be null");
        // SAFETY: pointer_handler validity is ensured by the protocol lifecycle.
        if let Some(pointer_handler) = unsafe { context.pointer_handler.as_ref() } {
            let mut pointer_state = pointer_handler.state.lock();
            if pointer_state.state_changed {
                // SAFETY: state is non-null (checked above), using write_unaligned to avoid any alignment issues.
                unsafe {
                    state.write_unaligned(pointer_state.current_state);
                }
                pointer_state.state_changed = false;
                efi::Status::SUCCESS
            } else {
                efi::Status::NOT_READY
            }
        } else {
            log::error!("absolute_pointer_get_state invoked after pointer dropped.");
            efi::Status::DEVICE_ERROR
        }
    }
}

#[cfg(test)]
mod test {
    use alloc::rc::Rc;
    use core::{
        cell::Cell,
        ptr,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use alloc::boxed::Box;
    use r_efi::{efi, protocols};

    use hidparser::report_data_types::Usage;
    use patina::{
        component::service::uefi_services::{event::EventError, protocol::ProtocolError},
        error::EfiError,
    };

    use super::*;
    use crate::{
        pointer::{
            AXIS_RESOLUTION, BUTTON_MIN, CENTER, GENERIC_DESKTOP_WHEEL, GENERIC_DESKTOP_X, GENERIC_DESKTOP_Y,
            GENERIC_DESKTOP_Z, PointerHidHandler,
        },
        test_stubs::{mock_services, test_event, test_handle},
    };

    fn mock_boot_services() -> HidServices {
        mock_services(|_, _, _| {})
    }

    /// Builds a test AbsolutePointerFfi context wired to the given handler. The caller must ensure
    /// `handler` outlives the returned context. The returned context has a null `wait_for_input`
    /// event (not needed for callback unit tests) and a heap-allocated mode.
    fn test_context(boot_services: HidServices, handler: &PointerHidHandler) -> AbsolutePointerFfi {
        AbsolutePointerFfi {
            absolute_pointer: protocols::absolute_pointer::Protocol {
                reset: AbsolutePointerFfi::absolute_pointer_reset,
                get_state: AbsolutePointerFfi::absolute_pointer_get_state,
                mode: Box::into_raw(Box::new(AbsolutePointerFfi::initialize_mode(handler))),
                wait_for_input: ptr::null_mut(),
            },
            services: boot_services,
            pointer_handler: handler,
        }
    }

    // --- initialize_mode tests ---

    #[test]
    fn initialize_mode_sets_xy_axes() {
        let boot_services = mock_boot_services();
        let mut handler = PointerHidHandler::new_for_test(boot_services);
        handler.processor.supported_usages.insert(Usage::from(GENERIC_DESKTOP_X));
        handler.processor.supported_usages.insert(Usage::from(GENERIC_DESKTOP_Y));

        let mode = AbsolutePointerFfi::initialize_mode(&handler);

        assert_eq!(mode.absolute_max_x, AXIS_RESOLUTION);
        assert_eq!(mode.absolute_min_x, 0);
        assert_eq!(mode.absolute_max_y, AXIS_RESOLUTION);
        assert_eq!(mode.absolute_min_y, 0);
        assert_eq!(mode.absolute_max_z, 0);
        assert_eq!(mode.attributes, 0);
    }

    #[test]
    fn initialize_mode_sets_z_axis_for_z_usage() {
        let boot_services = mock_boot_services();
        let mut handler = PointerHidHandler::new_for_test(boot_services);
        handler.processor.supported_usages.insert(Usage::from(GENERIC_DESKTOP_Z));

        let mode = AbsolutePointerFfi::initialize_mode(&handler);

        assert_eq!(mode.absolute_max_z, AXIS_RESOLUTION);
        assert_eq!(mode.absolute_min_z, 0);
    }

    #[test]
    fn initialize_mode_sets_z_axis_for_wheel_usage() {
        let boot_services = mock_boot_services();
        let mut handler = PointerHidHandler::new_for_test(boot_services);
        handler.processor.supported_usages.insert(Usage::from(GENERIC_DESKTOP_WHEEL));

        let mode = AbsolutePointerFfi::initialize_mode(&handler);

        assert_eq!(mode.absolute_max_z, AXIS_RESOLUTION);
        assert_eq!(mode.absolute_min_z, 0);
    }

    #[test]
    fn initialize_mode_sets_alt_active_for_multiple_buttons() {
        let boot_services = mock_boot_services();
        let mut handler = PointerHidHandler::new_for_test(boot_services);
        handler.processor.supported_usages.insert(Usage::from(BUTTON_MIN));
        handler.processor.supported_usages.insert(Usage::from(BUTTON_MIN + 1));

        let mode = AbsolutePointerFfi::initialize_mode(&handler);

        assert_ne!(mode.attributes & protocols::absolute_pointer::SUPPORTS_ALT_ACTIVE, 0);
    }

    #[test]
    fn initialize_mode_no_alt_active_for_single_button() {
        let boot_services = mock_boot_services();
        let mut handler = PointerHidHandler::new_for_test(boot_services);
        handler.processor.supported_usages.insert(Usage::from(BUTTON_MIN));

        let mode = AbsolutePointerFfi::initialize_mode(&handler);

        assert_eq!(mode.attributes & protocols::absolute_pointer::SUPPORTS_ALT_ACTIVE, 0);
    }

    #[test]
    fn initialize_mode_no_usages_returns_zeroed_mode() {
        let boot_services = mock_boot_services();
        let handler = PointerHidHandler::new_for_test(boot_services);

        let mode = AbsolutePointerFfi::initialize_mode(&handler);

        assert_eq!(mode.absolute_max_x, 0);
        assert_eq!(mode.absolute_max_y, 0);
        assert_eq!(mode.absolute_max_z, 0);
        assert_eq!(mode.attributes, 0);
    }

    // --- FFI callback tests ---

    #[test]
    fn reset_returns_invalid_parameter_on_null() {
        let status = AbsolutePointerFfi::absolute_pointer_reset(ptr::null_mut(), false);
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn reset_clears_state() {
        let boot_services = mock_boot_services();
        let handler = PointerHidHandler::new_for_test(boot_services);
        // Set up some non-default state.
        {
            let mut state = handler.state.lock();
            state.current_state.current_x = 100;
            state.state_changed = true;
        }

        let mut ctx = test_context(boot_services, &handler);
        let status = AbsolutePointerFfi::absolute_pointer_reset(&mut ctx.absolute_pointer as *mut _, false);

        assert_eq!(status, efi::Status::SUCCESS);
        let state = handler.state.lock();
        assert_eq!(state.current_state.current_x, CENTER);
        assert_eq!(state.current_state.current_y, CENTER);
        assert!(!state.state_changed);
    }

    #[test]
    fn reset_returns_device_error_after_pointer_dropped() {
        let boot_services = mock_boot_services();
        let mut ctx = AbsolutePointerFfi {
            absolute_pointer: protocols::absolute_pointer::Protocol {
                reset: AbsolutePointerFfi::absolute_pointer_reset,
                get_state: AbsolutePointerFfi::absolute_pointer_get_state,
                mode: ptr::null_mut(),
                wait_for_input: ptr::null_mut(),
            },
            services: boot_services,
            pointer_handler: ptr::null_mut(),
        };

        let status = AbsolutePointerFfi::absolute_pointer_reset(&mut ctx.absolute_pointer as *mut _, false);
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn get_state_returns_invalid_parameter_on_null_this() {
        let status = AbsolutePointerFfi::absolute_pointer_get_state(
            ptr::null_mut(),
            &mut protocols::absolute_pointer::State::default(),
        );
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn get_state_returns_invalid_parameter_on_null_state() {
        let boot_services = mock_boot_services();
        let handler = PointerHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &handler);

        let status =
            AbsolutePointerFfi::absolute_pointer_get_state(&mut ctx.absolute_pointer as *mut _, ptr::null_mut());
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn get_state_returns_not_ready_when_unchanged() {
        let boot_services = mock_boot_services();
        let handler = PointerHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &handler);
        let mut state = protocols::absolute_pointer::State::default();

        let status = AbsolutePointerFfi::absolute_pointer_get_state(&mut ctx.absolute_pointer as *mut _, &mut state);
        assert_eq!(status, efi::Status::NOT_READY);
    }

    #[test]
    fn get_state_returns_state_and_clears_flag() {
        let boot_services = mock_boot_services();
        let handler = PointerHidHandler::new_for_test(boot_services);
        {
            let mut state = handler.state.lock();
            state.current_state.current_x = 200;
            state.current_state.current_y = 300;
            state.current_state.active_buttons = 1;
            state.state_changed = true;
        }

        let mut ctx = test_context(boot_services, &handler);
        let mut out_state = protocols::absolute_pointer::State::default();

        let status =
            AbsolutePointerFfi::absolute_pointer_get_state(&mut ctx.absolute_pointer as *mut _, &mut out_state);

        assert_eq!(status, efi::Status::SUCCESS);
        assert_eq!(out_state.current_x, 200);
        assert_eq!(out_state.current_y, 300);
        assert_eq!(out_state.active_buttons, 1);
        // Flag should be cleared after read.
        assert!(!handler.state.lock().state_changed);
    }

    #[test]
    fn get_state_returns_device_error_after_pointer_dropped() {
        let boot_services = mock_boot_services();
        let mut ctx = AbsolutePointerFfi {
            absolute_pointer: protocols::absolute_pointer::Protocol {
                reset: AbsolutePointerFfi::absolute_pointer_reset,
                get_state: AbsolutePointerFfi::absolute_pointer_get_state,
                mode: ptr::null_mut(),
                wait_for_input: ptr::null_mut(),
            },
            services: boot_services,
            pointer_handler: ptr::null_mut(),
        };

        let status = AbsolutePointerFfi::absolute_pointer_get_state(
            &mut ctx.absolute_pointer as *mut _,
            &mut protocols::absolute_pointer::State::default(),
        );
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn signal_if_input_available_signals_event_when_state_changed() {
        static SIGNALS: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|_, events, _| {
            events.expect_signal_event().returning(|event| {
                assert_eq!(event, test_event(0x1234));
                SIGNALS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let handler = PointerHidHandler::new_for_test(services);
        handler.state.lock().state_changed = true;

        let ctx = test_context(services, &handler);
        AbsolutePointerFfi::signal_if_input_available(ctx.services, ctx.pointer_handler, test_event(0x1234));
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn signal_if_input_available_does_not_signal_when_unchanged() {
        let services = mock_services(|_, events, _| {
            events.expect_signal_event().never();
        });

        let handler = PointerHidHandler::new_for_test(services);
        // state_changed defaults to false.

        let ctx = test_context(services, &handler);
        AbsolutePointerFfi::signal_if_input_available(ctx.services, ctx.pointer_handler, test_event(0x1234));
    }

    #[test]
    fn signal_if_input_available_handles_dropped_pointer() {
        let services = mock_services(|_, events, _| {
            events.expect_signal_event().never();
        });
        let handler = PointerHidHandler::new_for_test(services);
        let mut ctx = test_context(services, &handler);
        ctx.pointer_handler = ptr::null_mut();
        // Should log an error but not panic.
        AbsolutePointerFfi::signal_if_input_available(ctx.services, ctx.pointer_handler, test_event(0x1234));
    }

    // --- install/uninstall tests ---

    #[test]
    fn installed_wait_callback_signals_only_when_input_available() {
        let callback = Rc::new(core::cell::RefCell::new(None));
        let captured = callback.clone();
        let signals = Rc::new(Cell::new(0));
        let counted = signals.clone();
        let services = mock_services(|protocols, events, timers| {
            events.expect_create_wait_event().returning_st(move |tpl, notify| {
                assert_eq!(tpl, Tpl::Notify);
                *captured.borrow_mut() = Some(notify);
                Ok(test_event(0x42))
            });
            events.expect_signal_event().returning_st(move |event| {
                assert_eq!(event, test_event(0x42));
                counted.set(counted.get() + 1);
                Ok(())
            });
            protocols.expect_install_interface().returning(|handle, _, _| Ok(handle.unwrap()));
            protocols.expect_uninstall_interface().returning(|_, _, _| Ok(()));
            timers.expect_create_timer_event_no_notify().never();
        });
        let handler = PointerHidHandler::new_for_test(services);
        let ctx = AbsolutePointerFfi::install(
            services,
            test_handle(0x2),
            &handler,
            AbsolutePointerFfi::initialize_mode(&handler),
        )
        .unwrap();
        let mut notify = callback.borrow_mut().take().unwrap();
        notify(test_event(0x42));
        assert_eq!(signals.get(), 0);
        handler.state.lock().state_changed = true;
        notify(test_event(0x42));
        assert_eq!(signals.get(), 1);
        handler.state.lock().state_changed = false;
        notify(test_event(0x42));
        assert_eq!(signals.get(), 1);
        drop(notify);
        AbsolutePointerFfi::uninstall(services, test_handle(0x2), ctx).unwrap();
    }

    /// Creates a leaked `AbsolutePointerFfi` box, simulating the state after a successful `install`. The caller is
    /// responsible for ensuring the leaked allocation is cleaned up.
    fn leaked_context(services: HidServices, handler: &PointerHidHandler) -> *mut AbsolutePointerFfi {
        Box::into_raw(Box::new(AbsolutePointerFfi {
            absolute_pointer: protocols::absolute_pointer::Protocol {
                reset: AbsolutePointerFfi::absolute_pointer_reset,
                get_state: AbsolutePointerFfi::absolute_pointer_get_state,
                mode: Box::into_raw(Box::new(protocols::absolute_pointer::Mode::default())),
                wait_for_input: test_event(0x42).as_raw(),
            },
            services,
            pointer_handler: handler,
        }))
    }

    #[test]
    fn install_succeeds_with_wait_event() {
        let services = mock_services(|protocols, events, timers| {
            events.expect_create_wait_event().returning(|tpl, _| {
                assert_eq!(tpl, Tpl::Notify);
                Ok(test_event(0x42))
            });
            timers.expect_set_timer().never();
            protocols.expect_install_interface().returning(|handle, guid, interface| {
                assert_eq!(handle, Some(test_handle(0x2)));
                assert_eq!(guid, protocols::absolute_pointer::PROTOCOL_GUID);
                // SAFETY: interface is the AbsolutePointerFfi context created by install.
                let ctx = unsafe { &*(interface.as_raw() as *const AbsolutePointerFfi) };
                assert_eq!(ctx.absolute_pointer.wait_for_input, test_event(0x42).as_raw());
                Ok(test_handle(0x2))
            });
        });

        let handler = PointerHidHandler::new_for_test(services);
        let ctx = AbsolutePointerFfi::install(
            services,
            test_handle(0x2),
            &handler,
            AbsolutePointerFfi::initialize_mode(&handler),
        )
        .unwrap();

        // SAFETY: Reclaiming the Box leaked by install.
        drop(unsafe { Box::from_raw(ctx) });
    }

    #[test]
    fn install_returns_error_on_create_event_failure() {
        let services = mock_services(|protocols, events, _| {
            events.expect_create_wait_event().returning(|_, _| Err(EventError::Internal));
            protocols.expect_install_interface().never();
        });

        let handler = PointerHidHandler::new_for_test(services);
        let result = AbsolutePointerFfi::install(
            services,
            test_handle(0x2),
            &handler,
            AbsolutePointerFfi::initialize_mode(&handler),
        );
        assert_eq!(result.err(), Some(efi::Status::UNSUPPORTED));
    }

    #[test]
    fn install_cleans_up_event_on_install_protocol_failure() {
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            protocols.expect_install_interface().returning(|_, _, _| Err(ProtocolError::AccessDenied));
            // close_event must be called to clean up the event on install failure.
            events.expect_close_event().returning(|_| {
                CLOSED.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let handler = PointerHidHandler::new_for_test(services);
        let result = AbsolutePointerFfi::install(
            services,
            test_handle(0x2),
            &handler,
            AbsolutePointerFfi::initialize_mode(&handler),
        );
        assert_eq!(result.err(), Some(efi::Status::ACCESS_DENIED));
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn uninstall_succeeds() {
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            protocols.expect_uninstall_interface().returning(|handle, guid, _| {
                assert_eq!(handle, test_handle(0x2));
                assert_eq!(guid, protocols::absolute_pointer::PROTOCOL_GUID);
                Ok(())
            });
            events.expect_close_event().returning(|event| {
                assert_eq!(event, test_event(0x42));
                CLOSED.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let handler = PointerHidHandler::new_for_test(services);
        let ctx = leaked_context(services, &handler);

        assert_eq!(AbsolutePointerFfi::uninstall(services, test_handle(0x2), ctx), Ok(()));
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn uninstall_failure_keeps_context() {
        let services = mock_services(|protocols, _, _| {
            protocols.expect_uninstall_interface().returning(|_, _, _| Err(ProtocolError::AccessDenied));
        });

        let handler = PointerHidHandler::new_for_test(services);
        let ctx = leaked_context(services, &handler);

        let result = AbsolutePointerFfi::uninstall(services, test_handle(0x2), ctx);
        assert_eq!(result, Err(efi::Status::ACCESS_DENIED));

        // SAFETY: ctx points to the still-live leaked context (uninstall failed).
        assert_eq!(unsafe { &*ctx }.pointer_handler, &raw const handler);

        // SAFETY: Reclaiming the leaked context Box for cleanup.
        drop(unsafe { Box::from_raw(ctx) });
    }

    #[test]
    fn uninstall_returns_error_on_close_event_failure() {
        let services = mock_services(|protocols, events, _| {
            protocols.expect_uninstall_interface().returning(|_, _, _| Ok(()));
            events.expect_close_event().returning(|_| Err(EfiError::InvalidParameter.into()));
        });

        let handler = PointerHidHandler::new_for_test(services);
        let ctx = leaked_context(services, &handler);

        let result = AbsolutePointerFfi::uninstall(services, test_handle(0x2), ctx);
        assert_eq!(result, Err(efi::Status::INVALID_PARAMETER));
    }
}
