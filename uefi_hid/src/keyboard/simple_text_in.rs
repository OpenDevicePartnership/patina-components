//! Simple Text In Protocol FFI Support.
//!
//! This module manages the Simple Text In FFI.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!

use alloc::boxed::Box;

use r_efi::{efi, protocols};

use patina::component::service::uefi_services::{
    event::{Event, EventServicesExt, Tpl},
    handle::Handle,
    protocol::ProtocolPtr,
};

use super::KeyboardHidHandler;
use crate::{HidServices, to_status};

/// FFI context for SimpleTextInput protocol.
///
/// A pointer to KeyboardHidHandler is included in the context so that it can be reclaimed in the simple_text_in API
/// implementations. Mutual exclusion on the KeyboardHidHandler is provided by the TplMutex on its key_queue state.
///
/// The simple_text_in protocol element must be the first element in the structure so that the full structure can be
/// recovered by simple casting.
#[repr(C)]
pub(crate) struct SimpleTextInFfi {
    simple_text_in: protocols::simple_text_input::Protocol,
    pub(crate) services: HidServices,
    pub(crate) keyboard_handler: *const KeyboardHidHandler,
}

impl SimpleTextInFfi {
    /// Installs the simple text in protocol. Returns the installed context pointer, which must be passed to
    /// [`Self::uninstall`].
    pub(crate) fn install(
        services: HidServices,
        controller: Handle,
        keyboard_handler: *const KeyboardHidHandler,
    ) -> Result<*mut Self, efi::Status> {
        let wait_for_key_event = services
            .events
            .on_wait_event(Tpl::Notify, move |event| {
                Self::signal_if_key_available(services, keyboard_handler, event);
            })
            .map_err(to_status)?;

        let ctx = Box::into_raw(Box::new(SimpleTextInFfi {
            simple_text_in: protocols::simple_text_input::Protocol {
                reset: Self::simple_text_in_reset,
                read_key_stroke: Self::simple_text_in_read_key_stroke,
                wait_for_key: wait_for_key_event.as_raw(),
            },
            services,
            keyboard_handler,
        }));

        let interface = ProtocolPtr::from_raw(ctx.cast()).expect("Box pointers are non-null");
        match services.protocols.install_interface(
            Some(controller),
            protocols::simple_text_input::PROTOCOL_GUID.into(),
            interface,
        ) {
            Ok(_) => Ok(ctx),
            Err(error) => {
                if let Err(error) = services.events.close_event(wait_for_key_event) {
                    log::error!("Failed to close wait_for_key after install failure: {:?}", error);
                }
                // SAFETY: ctx was created by Box::into_raw above and was not published.
                drop(unsafe { Box::from_raw(ctx) });
                Err(to_status(error))
            }
        }
    }

    /// Uninstalls the simple text in protocol and frees the context created by [`Self::install`].
    pub(crate) fn uninstall(services: HidServices, controller: Handle, ctx: *mut Self) -> Result<(), efi::Status> {
        let interface = ProtocolPtr::from_raw(ctx.cast()).ok_or(efi::Status::INVALID_PARAMETER)?;
        if let Err(error) = services.protocols.uninstall_interface(
            controller,
            protocols::simple_text_input::PROTOCOL_GUID.into(),
            interface,
        ) {
            log::error!("Failed to uninstall simple_text_in interface: {:?}", error);
            // The protocol is still installed, so the context stays allocated. The caller must not free the handler.
            return Err(to_status(error));
        }

        // SAFETY: ctx was created via Box::into_raw in install and is no longer published.
        let ctx = unsafe { Box::from_raw(ctx) };

        if let Some(event) = Event::from_raw(ctx.simple_text_in.wait_for_key)
            && let Err(error) = services.events.close_event(event)
        {
            log::error!("Failed to close simple_text_in.wait_for_key event: {:?}", error);
            // Leak ctx so a still-live event callback doesn't use freed memory.
            core::mem::forget(ctx);
            return Err(to_status(error));
        }

        // ctx drops here, freeing the SimpleTextInFfi allocation.
        Ok(())
    }

    // Resets the keyboard state — part of the simple_text_in protocol interface.
    extern "efiapi" fn simple_text_in_reset(
        this: *mut protocols::simple_text_input::Protocol,
        extended_verification: efi::Boolean,
    ) -> efi::Status {
        // SAFETY: `this` points to the first field of Self (#[repr(C)]), so the cast recovers
        // the full SimpleTextInFfi context. Null is handled by the check below.
        let context = unsafe { (this as *const Self).as_ref() };
        let Some(context) = context else { return efi::Status::INVALID_PARAMETER };
        // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
        // accessed through shared references; null is handled by the check below.
        let keyboard_handler = unsafe { context.keyboard_handler.as_ref() };
        let Some(keyboard_handler) = keyboard_handler else {
            log::error!("simple_text_in_reset invoked after keyboard dropped.");
            return efi::Status::DEVICE_ERROR;
        };
        keyboard_handler.reset(extended_verification.into());
        log::trace!("simple_text_in_reset: extended_verification={:?}", bool::from(extended_verification));
        efi::Status::SUCCESS
    }

    // Reads a key stroke — part of the simple_text_in protocol interface.
    extern "efiapi" fn simple_text_in_read_key_stroke(
        this: *mut protocols::simple_text_input::Protocol,
        key: *mut protocols::simple_text_input::InputKey,
    ) -> efi::Status {
        if this.is_null() || key.is_null() {
            return efi::Status::INVALID_PARAMETER;
        }
        // SAFETY: `this` points to the first field of Self (#[repr(C)]), so the cast recovers
        // the full SimpleTextInFfi context. Null is checked by the preceding guard.
        let Some(context) = (unsafe { (this as *const Self).as_ref() }) else {
            return efi::Status::INVALID_PARAMETER;
        };
        // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
        // accessed through shared references; null is handled by the check below.
        let keyboard_handler = unsafe { context.keyboard_handler.as_ref() };
        let Some(keyboard_handler) = keyboard_handler else {
            log::error!("simple_text_in_read_key_stroke invoked after keyboard dropped.");
            return efi::Status::DEVICE_ERROR;
        };

        let mut state = keyboard_handler.state.lock();
        let kq = &mut state.key_queue;
        loop {
            if let Some(mut key_data) = kq.pop_key() {
                // skip partials
                if key_data.key.unicode_char == 0 && key_data.key.scan_code == 0 {
                    continue;
                }
                const CONTROL_PRESSED: u32 = protocols::simple_text_input_ex::RIGHT_CONTROL_PRESSED
                    | protocols::simple_text_input_ex::LEFT_CONTROL_PRESSED;
                const LOWERCASE_A: u16 = 0x0061;
                const LOWERCASE_Z: u16 = 0x007a;
                const UPPERCASE_A: u16 = 0x0041;
                const UPPERCASE_Z: u16 = 0x005a;
                if (key_data.key_state.key_shift_state & CONTROL_PRESSED) != 0 {
                    if key_data.key.unicode_char >= LOWERCASE_A && key_data.key.unicode_char <= LOWERCASE_Z {
                        key_data.key.unicode_char = (key_data.key.unicode_char - LOWERCASE_A) + 1;
                    }
                    if key_data.key.unicode_char >= UPPERCASE_A && key_data.key.unicode_char <= UPPERCASE_Z {
                        key_data.key.unicode_char = (key_data.key.unicode_char - UPPERCASE_A) + 1;
                    }
                }
                // SAFETY: key output pointer was null-checked at the top of this function;
                // write_unaligned is used to avoid any alignment requirements.
                unsafe { key.write_unaligned(key_data.key) }
                log::trace!(
                    "simple_text_in_read_key_stroke: unicode=0x{:04X} scan=0x{:04X}",
                    key_data.key.unicode_char,
                    key_data.key.scan_code,
                );
                return efi::Status::SUCCESS;
            } else {
                return efi::Status::NOT_READY;
            }
        }
    }

    /// Signals `event` if a (non-partial) key is available, discarding any leading partial keys.
    ///
    /// This is the notify logic for the wait_for_key event.
    fn signal_if_key_available(services: HidServices, keyboard_handler: *const KeyboardHidHandler, event: Event) {
        // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
        // accessed through shared references; null is handled by the check below.
        let keyboard_handler = unsafe { keyboard_handler.as_ref() };
        let Some(keyboard_handler) = keyboard_handler else { return };
        let mut state = keyboard_handler.state.lock();
        let kq = &mut state.key_queue;
        while let Some(key_data) = kq.peek_key() {
            if key_data.key.unicode_char == 0 && key_data.key.scan_code == 0 {
                let _ = kq.pop_key();
                continue;
            } else {
                if let Err(error) = services.events.signal_event(event) {
                    log::error!("Failed to signal wait_for_key: {:?}", error);
                }
                break;
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test {
    use alloc::rc::Rc;
    use core::{cell::Cell, ptr};

    use alloc::boxed::Box;
    use r_efi::{efi, protocols};

    use core::sync::atomic::{AtomicUsize, Ordering};

    use patina::{
        component::service::uefi_services::{event::EventError, protocol::ProtocolError},
        error::EfiError,
    };

    use super::*;
    use crate::test_stubs::{mock_services, test_event, test_handle};

    fn mock_boot_services() -> HidServices {
        mock_services(|_, _, _| {})
    }

    fn test_context(boot_services: HidServices, handler: &mut KeyboardHidHandler) -> SimpleTextInFfi {
        SimpleTextInFfi {
            simple_text_in: protocols::simple_text_input::Protocol {
                reset: SimpleTextInFfi::simple_text_in_reset,
                read_key_stroke: SimpleTextInFfi::simple_text_in_read_key_stroke,
                wait_for_key: ptr::null_mut(),
            },
            services: boot_services,
            keyboard_handler: handler as *mut KeyboardHidHandler,
        }
    }

    pub(crate) fn leaked_context(services: HidServices, handler: &mut KeyboardHidHandler) -> *mut SimpleTextInFfi {
        Box::into_raw(Box::new(SimpleTextInFfi {
            simple_text_in: protocols::simple_text_input::Protocol {
                reset: SimpleTextInFfi::simple_text_in_reset,
                read_key_stroke: SimpleTextInFfi::simple_text_in_read_key_stroke,
                wait_for_key: test_event(0x42).as_raw(),
            },
            services,
            keyboard_handler: handler as *mut _,
        }))
    }

    // --- install/uninstall ---

    #[test]
    fn installed_wait_callback_signals_only_when_key_available() {
        use hidparser::report_data_types::Usage;
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
        let handler = KeyboardHidHandler::new_for_test(services);
        handler.set_layout(Some(crate::keyboard::layout::get_default_keyboard_layout()));
        let ctx = SimpleTextInFfi::install(services, test_handle(0x2), &handler).unwrap();
        let mut notify = callback.borrow_mut().take().unwrap();
        notify(test_event(0x42));
        assert_eq!(signals.get(), 0);
        handler
            .state
            .lock()
            .key_queue
            .keystroke(Usage::from(0x00070028u32), super::super::key_queue::KeyAction::KeyDown);
        notify(test_event(0x42));
        assert_eq!(signals.get(), 1);
        handler.pop_key();
        notify(test_event(0x42));
        assert_eq!(signals.get(), 1);
        drop(notify);
        SimpleTextInFfi::uninstall(services, test_handle(0x2), ctx).unwrap();
    }

    #[test]
    fn install_returns_protocol_error_even_when_close_fails() {
        let services = mock_services(|protocols, events, _| {
            protocols.expect_install_interface().returning(|_, _, _| Err(ProtocolError::AccessDenied));
            events.expect_close_event().returning(|_| Err(EventError::InvalidParameter));
            events.expect_signal_event().never();
        });
        let handler = Box::new(KeyboardHidHandler::new_for_test(services));
        assert_eq!(SimpleTextInFfi::install(services, test_handle(0x2), &*handler), Err(efi::Status::ACCESS_DENIED));
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
                assert_eq!(guid, protocols::simple_text_input::PROTOCOL_GUID);
                // SAFETY: interface is the SimpleTextInFfi context created by install.
                let ctx = unsafe { &*(interface.as_raw() as *const SimpleTextInFfi) };
                assert_eq!(ctx.simple_text_in.wait_for_key, test_event(0x42).as_raw());
                Ok(test_handle(0x2))
            });
        });

        let handler = KeyboardHidHandler::new_for_test(services);
        let ctx = SimpleTextInFfi::install(services, test_handle(0x2), &handler).unwrap();

        // SAFETY: Reclaiming the Box leaked by install.
        drop(unsafe { Box::from_raw(ctx) });
    }

    #[test]
    fn install_returns_error_on_create_event_failure() {
        let services = mock_services(|protocols, events, _| {
            events.expect_create_wait_event().returning(|_, _| Err(EventError::Internal));
            protocols.expect_install_interface().never();
        });

        let handler = KeyboardHidHandler::new_for_test(services);
        let result = SimpleTextInFfi::install(services, test_handle(0x2), &handler);
        assert_eq!(result.err(), Some(efi::Status::UNSUPPORTED));
    }

    #[test]
    fn install_cleans_up_event_on_install_protocol_failure() {
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            protocols.expect_install_interface().returning(|_, _, _| Err(ProtocolError::AccessDenied));
            events.expect_close_event().returning(|_| {
                CLOSED.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let handler = KeyboardHidHandler::new_for_test(services);
        let result = SimpleTextInFfi::install(services, test_handle(0x2), &handler);
        assert_eq!(result.err(), Some(efi::Status::ACCESS_DENIED));
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn uninstall_succeeds() {
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            protocols.expect_uninstall_interface().returning(|handle, guid, _| {
                assert_eq!(handle, test_handle(0x2));
                assert_eq!(guid, protocols::simple_text_input::PROTOCOL_GUID);
                Ok(())
            });
            events.expect_close_event().returning(|event| {
                assert_eq!(event, test_event(0x42));
                CLOSED.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let mut handler = KeyboardHidHandler::new_for_test(services);
        let ctx = leaked_context(services, &mut handler);
        assert_eq!(SimpleTextInFfi::uninstall(services, test_handle(0x2), ctx), Ok(()));
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn uninstall_failure_keeps_context() {
        let services = mock_services(|protocols, _, _| {
            protocols.expect_uninstall_interface().returning(|_, _, _| Err(ProtocolError::AccessDenied));
        });

        let mut handler = KeyboardHidHandler::new_for_test(services);
        let ctx = leaked_context(services, &mut handler);

        let result = SimpleTextInFfi::uninstall(services, test_handle(0x2), ctx);
        assert_eq!(result, Err(efi::Status::ACCESS_DENIED));

        // The context is still installed and still references the handler, which the caller must therefore leak.
        // SAFETY: ctx points to the still-live leaked context (uninstall failed).
        assert_eq!(unsafe { &*ctx }.keyboard_handler, &raw const handler);

        // SAFETY: Reclaiming the leaked context Box for cleanup.
        drop(unsafe { Box::from_raw(ctx) });
    }

    #[test]
    fn uninstall_returns_error_on_close_event_failure() {
        let services = mock_services(|protocols, events, _| {
            protocols.expect_uninstall_interface().returning(|_, _, _| Ok(()));
            events.expect_close_event().returning(|_| Err(EfiError::InvalidParameter.into()));
        });

        let mut handler = KeyboardHidHandler::new_for_test(services);
        let ctx = leaked_context(services, &mut handler);
        let result = SimpleTextInFfi::uninstall(services, test_handle(0x2), ctx);
        assert_eq!(result, Err(efi::Status::INVALID_PARAMETER));
    }

    // --- FFI callbacks ---

    #[test]
    fn reset_returns_invalid_parameter_on_null() {
        let status = SimpleTextInFfi::simple_text_in_reset(ptr::null_mut(), false.into());
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn reset_returns_device_error_when_handler_null() {
        let boot_services = mock_boot_services();
        let mut ctx = SimpleTextInFfi {
            simple_text_in: protocols::simple_text_input::Protocol {
                reset: SimpleTextInFfi::simple_text_in_reset,
                read_key_stroke: SimpleTextInFfi::simple_text_in_read_key_stroke,
                wait_for_key: ptr::null_mut(),
            },
            services: boot_services,
            keyboard_handler: ptr::null_mut(),
        };
        let status = SimpleTextInFfi::simple_text_in_reset(&mut ctx.simple_text_in as *mut _, false.into());
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn read_key_stroke_returns_not_ready_on_empty_queue() {
        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &mut handler);
        let mut key = protocols::simple_text_input::InputKey::default();
        let status = SimpleTextInFfi::simple_text_in_read_key_stroke(&mut ctx.simple_text_in as *mut _, &mut key);
        assert_eq!(status, efi::Status::NOT_READY);
    }

    #[test]
    fn read_key_stroke_returns_invalid_parameter_on_null() {
        let status = SimpleTextInFfi::simple_text_in_read_key_stroke(ptr::null_mut(), ptr::null_mut());
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn reset_succeeds_with_valid_handler() {
        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &mut handler);
        let status = SimpleTextInFfi::simple_text_in_reset(&mut ctx.simple_text_in as *mut _, false.into());
        assert_eq!(status, efi::Status::SUCCESS);
    }

    #[test]
    fn read_key_stroke_returns_success_when_key_available() {
        use hidparser::report_data_types::Usage;

        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);

        {
            let mut state = handler.state.lock();
            let kq = &mut state.key_queue;
            kq.set_layout(Some(crate::keyboard::layout::get_default_keyboard_layout()));
            kq.keystroke(Usage::from(0x00070028u32), super::super::key_queue::KeyAction::KeyDown);
        }

        let mut ctx = test_context(boot_services, &mut handler);
        let mut key = protocols::simple_text_input::InputKey::default();
        let status = SimpleTextInFfi::simple_text_in_read_key_stroke(&mut ctx.simple_text_in as *mut _, &mut key);
        assert_eq!(status, efi::Status::SUCCESS);
        assert!(key.scan_code != 0 || key.unicode_char != 0);
    }

    #[test]
    fn read_key_stroke_returns_device_error_when_handler_null() {
        let boot_services = mock_boot_services();
        let mut ctx = SimpleTextInFfi {
            simple_text_in: protocols::simple_text_input::Protocol {
                reset: SimpleTextInFfi::simple_text_in_reset,
                read_key_stroke: SimpleTextInFfi::simple_text_in_read_key_stroke,
                wait_for_key: ptr::null_mut(),
            },
            services: boot_services,
            keyboard_handler: ptr::null_mut(),
        };
        let mut key = protocols::simple_text_input::InputKey::default();
        let status = SimpleTextInFfi::simple_text_in_read_key_stroke(&mut ctx.simple_text_in as *mut _, &mut key);
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn read_key_stroke_skips_partial_keys() {
        use hidparser::report_data_types::Usage;

        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);

        {
            let mut state = handler.state.lock();
            let kq = &mut state.key_queue;
            kq.set_layout(Some(crate::keyboard::layout::get_default_keyboard_layout()));
            // Enable partial key support so modifier keys are enqueued.
            kq.set_key_toggle_state(
                protocols::simple_text_input_ex::TOGGLE_STATE_VALID
                    | protocols::simple_text_input_ex::KEY_STATE_EXPOSED,
            );
            // Press left ctrl — produces a partial key (scan=0, unicode=0).
            kq.keystroke(Usage::from(0x000700E0u32), super::super::key_queue::KeyAction::KeyDown);
            // Press Enter — produces a real key (unicode=0x0D).
            kq.keystroke(Usage::from(0x00070028u32), super::super::key_queue::KeyAction::KeyDown);
        }

        let mut ctx = test_context(boot_services, &mut handler);
        let mut key = protocols::simple_text_input::InputKey::default();
        let status = SimpleTextInFfi::simple_text_in_read_key_stroke(&mut ctx.simple_text_in as *mut _, &mut key);
        assert_eq!(status, efi::Status::SUCCESS);
        assert!(key.scan_code != 0 || key.unicode_char != 0);
    }

    fn counting_signal_services(signals: &'static AtomicUsize) -> HidServices {
        mock_services(move |_, events, _| {
            events.expect_signal_event().returning(move |event| {
                assert_eq!(event, test_event(0x1234));
                signals.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        })
    }

    #[test]
    fn signal_if_key_available_signals_event_when_key_available() {
        use hidparser::report_data_types::Usage;

        static SIGNALS: AtomicUsize = AtomicUsize::new(0);
        let services = counting_signal_services(&SIGNALS);
        let mut handler = KeyboardHidHandler::new_for_test(services);

        {
            let mut state = handler.state.lock();
            let kq = &mut state.key_queue;
            kq.set_layout(Some(crate::keyboard::layout::get_default_keyboard_layout()));
            kq.keystroke(Usage::from(0x00070028u32), super::super::key_queue::KeyAction::KeyDown);
        }

        let ctx = test_context(services, &mut handler);
        SimpleTextInFfi::signal_if_key_available(ctx.services, ctx.keyboard_handler, test_event(0x1234));
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn signal_if_key_available_does_not_signal_on_empty_queue() {
        static SIGNALS: AtomicUsize = AtomicUsize::new(0);
        let services = counting_signal_services(&SIGNALS);
        let mut handler = KeyboardHidHandler::new_for_test(services);
        let ctx = test_context(services, &mut handler);
        SimpleTextInFfi::signal_if_key_available(ctx.services, ctx.keyboard_handler, test_event(0x1234));
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn signal_if_key_available_skips_partial_and_signals_for_real_key() {
        use hidparser::report_data_types::Usage;

        static SIGNALS: AtomicUsize = AtomicUsize::new(0);
        let services = counting_signal_services(&SIGNALS);
        let mut handler = KeyboardHidHandler::new_for_test(services);

        {
            let mut state = handler.state.lock();
            let kq = &mut state.key_queue;
            kq.set_layout(Some(crate::keyboard::layout::get_default_keyboard_layout()));
            // Enable partial key support so modifier keys are enqueued.
            kq.set_key_toggle_state(
                protocols::simple_text_input_ex::TOGGLE_STATE_VALID
                    | protocols::simple_text_input_ex::KEY_STATE_EXPOSED,
            );
            // Press left ctrl — produces a partial key (scan=0, unicode=0).
            kq.keystroke(Usage::from(0x000700E0u32), super::super::key_queue::KeyAction::KeyDown);
            // Press Enter — produces a real key (unicode=0x0D).
            kq.keystroke(Usage::from(0x00070028u32), super::super::key_queue::KeyAction::KeyDown);
        }

        let ctx = test_context(services, &mut handler);
        SimpleTextInFfi::signal_if_key_available(ctx.services, ctx.keyboard_handler, test_event(0x1234));
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);
    }
}
