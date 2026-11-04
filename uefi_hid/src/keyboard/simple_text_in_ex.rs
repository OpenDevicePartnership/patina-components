//! Simple Text In Ex Protocol FFI Support.
//!
//! This module manages the Simple Text In Ex FFI.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!

use alloc::boxed::Box;
use core::ffi::c_void;

use r_efi::{efi, protocols};

use patina::component::service::uefi_services::{
    event::{Event, EventServicesExt, Tpl as ServiceTpl},
    handle::Handle,
    protocol::ProtocolPtr,
};

use super::KeyboardHidHandler;
use crate::{HidServices, to_status};

/// FFI context for SimpleTextInputEx protocol.
///
/// A pointer to KeyboardHidHandler is included in the context so that it can be reclaimed in the simple_text_in_ex API
/// implementations. Mutual exclusion on the KeyboardHidHandler is provided by the TplMutex on its key_queue state.
///
/// The simple_text_in_ex protocol element must be the first element in the structure so that the full structure can be
/// recovered by simple casting.
#[repr(C)]
pub(crate) struct SimpleTextInExFfi {
    simple_text_in_ex: protocols::simple_text_input_ex::Protocol,
    pub(crate) services: HidServices,
    pub(crate) keyboard_handler: *const KeyboardHidHandler,
}

impl SimpleTextInExFfi {
    /// Installs the simple text in ex protocol. Returns the installed context pointer, which must be passed to
    /// [`Self::uninstall`], and the key notify event, which the caller must store in `key_notify_event`.
    ///
    /// `keyboard_handler` is stored in the context but not dereferenced here. It must be the handler's root pointer and
    /// remain valid until the protocol and its events are released.
    pub(crate) fn install(
        services: HidServices,
        controller: Handle,
        keyboard_handler: *const KeyboardHidHandler,
    ) -> Result<(*mut Self, Event), efi::Status> {
        let wait_for_key_event = services
            .events
            .on_wait_event(ServiceTpl::Notify, move |event| {
                Self::signal_if_key_available(services, keyboard_handler, event);
            })
            .map_err(to_status)?;

        let ctx = Box::into_raw(Box::new(SimpleTextInExFfi {
            simple_text_in_ex: protocols::simple_text_input_ex::Protocol {
                reset: Self::simple_text_in_ex_reset,
                read_key_stroke_ex: Self::simple_text_in_ex_read_key_stroke,
                wait_for_key_ex: wait_for_key_event.as_raw(),
                set_state: Self::simple_text_in_ex_set_state,
                register_key_notify: Self::simple_text_in_ex_register_key_notify,
                unregister_key_notify: Self::simple_text_in_ex_unregister_key_notify,
            },
            services,
            keyboard_handler,
        }));

        let cleanup = |ctx: *mut Self| {
            if let Err(error) = services.events.close_event(wait_for_key_event) {
                log::error!("Failed to close wait_for_key_ex after install failure: {:?}", error);
            }
            // SAFETY: ctx was created by Box::into_raw above and was not published.
            drop(unsafe { Box::from_raw(ctx) });
        };

        // Key notifies dispatch at TPL_CALLBACK per UEFI spec 2.10 section 12.2.5.
        let ctx_addr = ctx as usize;
        let key_notify_event = match services.events.on_event(ServiceTpl::Callback, move || {
            // SAFETY: uninstall closes this event before freeing ctx. On failed installation the event handle
            // is never published, so no caller can signal it after the context is reclaimed.
            unsafe { Self::process_key_notifies(ctx_addr as *const Self) }
        }) {
            Ok(event) => event,
            Err(error) => {
                cleanup(ctx);
                return Err(to_status(error));
            }
        };

        let interface = ProtocolPtr::from_raw(ctx.cast()).expect("Box pointers are non-null");
        match services.protocols.install_interface(
            Some(controller),
            protocols::simple_text_input_ex::PROTOCOL_GUID.into(),
            interface,
        ) {
            Ok(_) => Ok((ctx, key_notify_event)),
            Err(error) => {
                if let Err(error) = services.events.close_event(key_notify_event) {
                    log::error!("Failed to close key_notify_event after install failure: {:?}", error);
                }
                cleanup(ctx);
                Err(to_status(error))
            }
        }
    }

    /// Uninstalls the simple text in ex protocol, closes `key_notify_event` (whose callback uses the context), and
    /// frees the context created by [`Self::install`].
    pub(crate) fn uninstall(
        services: HidServices,
        controller: Handle,
        ctx: *mut Self,
        key_notify_event: Option<Event>,
    ) -> Result<(), efi::Status> {
        let interface = ProtocolPtr::from_raw(ctx.cast()).ok_or(efi::Status::INVALID_PARAMETER)?;
        if let Err(error) = services.protocols.uninstall_interface(
            controller,
            protocols::simple_text_input_ex::PROTOCOL_GUID.into(),
            interface,
        ) {
            log::error!("Failed to uninstall simple_text_in_ex interface: {:?}", error);
            // The protocol is still installed, so the context stays allocated. The caller must not free the handler.
            return Err(to_status(error));
        }

        // SAFETY: ctx was created via Box::into_raw in install and is no longer published.
        let ctx = unsafe { Box::from_raw(ctx) };

        if let Some(event) = Event::from_raw(ctx.simple_text_in_ex.wait_for_key_ex)
            && let Err(error) = services.events.close_event(event)
        {
            log::error!("Failed to close simple_text_in_ex.wait_for_key_ex event: {:?}", error);
            // Leak ctx so a still-live event callback doesn't use freed memory.
            core::mem::forget(ctx);
            return Err(to_status(error));
        }

        if let Some(event) = key_notify_event
            && let Err(error) = services.events.close_event(event)
        {
            log::error!("Failed to close key_notify_event: {:?}", error);
            // Leak ctx so the still-live event callback doesn't use freed memory.
            core::mem::forget(ctx);
            return Err(to_status(error));
        }
        // ctx drops here, freeing the SimpleTextInExFfi allocation.
        Ok(())
    }

    // Resets the keyboard state — part of the simple_text_in_ex protocol interface.
    extern "efiapi" fn simple_text_in_ex_reset(
        this: *mut protocols::simple_text_input_ex::Protocol,
        extended_verification: efi::Boolean,
    ) -> efi::Status {
        // SAFETY: casting `this` (first field of #[repr(C)] struct) to recover full context; null handled below.
        let context = unsafe { (this as *const Self).as_ref() };
        let Some(context) = context else { return efi::Status::INVALID_PARAMETER };
        // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
        // accessed through shared references; null is handled by the check below.
        let keyboard_handler = unsafe { context.keyboard_handler.as_ref() };
        let Some(keyboard_handler) = keyboard_handler else {
            log::error!("simple_text_in_ex_reset invoked after keyboard dropped.");
            return efi::Status::DEVICE_ERROR;
        };
        keyboard_handler.reset(extended_verification.into());
        log::trace!("simple_text_in_ex_reset: extended_verification={:?}", bool::from(extended_verification));
        efi::Status::SUCCESS
    }

    // Reads a key stroke — part of the simple_text_in_ex protocol interface.
    extern "efiapi" fn simple_text_in_ex_read_key_stroke(
        this: *mut protocols::simple_text_input_ex::Protocol,
        key_data: *mut protocols::simple_text_input_ex::KeyData,
    ) -> efi::Status {
        if this.is_null() || key_data.is_null() {
            return efi::Status::INVALID_PARAMETER;
        }
        // SAFETY: casting `this` (first field of #[repr(C)] struct) to recover full context; null handled below.
        let Some(context) = (unsafe { (this as *const Self).as_ref() }) else {
            return efi::Status::INVALID_PARAMETER;
        };
        // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
        // accessed through shared references; null is handled by the check below.
        let keyboard_handler = unsafe { context.keyboard_handler.as_ref() };
        let Some(keyboard_handler) = keyboard_handler else {
            log::error!("simple_text_in_ex_read_key_stroke invoked after keyboard dropped.");
            return efi::Status::DEVICE_ERROR;
        };

        let mut state = keyboard_handler.state.lock();
        let kq = &mut state.key_queue;
        if let Some(key) = kq.pop_key() {
            log::trace!(
                "simple_text_in_ex_read_key_stroke: unicode=0x{:04X} scan=0x{:04X}",
                key.key.unicode_char,
                key.key.scan_code,
            );
            // SAFETY: writing through output pointer; null-checked at top of function.
            unsafe { key_data.write_unaligned(key) }
            efi::Status::SUCCESS
        } else {
            let key = protocols::simple_text_input_ex::KeyData { key_state: kq.init_key_state(), ..Default::default() };
            // SAFETY: writing through output pointer; null-checked at top of function.
            unsafe { key_data.write_unaligned(key) };
            efi::Status::NOT_READY
        }
    }

    // Sets the keyboard toggle state — part of the simple_text_in_ex protocol interface.
    extern "efiapi" fn simple_text_in_ex_set_state(
        this: *mut protocols::simple_text_input_ex::Protocol,
        key_toggle_state: *mut protocols::simple_text_input_ex::KeyToggleState,
    ) -> efi::Status {
        if this.is_null() || key_toggle_state.is_null() {
            return efi::Status::INVALID_PARAMETER;
        }
        // SAFETY: casting `this` (first field of #[repr(C)] struct) to recover full context; null handled below.
        let Some(context) = (unsafe { (this as *const Self).as_ref() }) else {
            return efi::Status::INVALID_PARAMETER;
        };
        // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
        // accessed through shared references; null is handled by the check below.
        let keyboard_handler = unsafe { context.keyboard_handler.as_ref() };
        let Some(keyboard_handler) = keyboard_handler else {
            log::error!("simple_text_in_ex_set_state invoked after keyboard dropped.");
            return efi::Status::DEVICE_ERROR;
        };
        // SAFETY: reading through pointer; null-checked at top of function.
        keyboard_handler.set_key_toggle_state(unsafe { key_toggle_state.read() });
        log::trace!("simple_text_in_ex_set_state: toggle state set");
        efi::Status::SUCCESS
    }

    // Registers a key notification callback — part of the simple_text_in_ex protocol interface.
    extern "efiapi" fn simple_text_in_ex_register_key_notify(
        this: *mut protocols::simple_text_input_ex::Protocol,
        key_data_ptr: *mut protocols::simple_text_input_ex::KeyData,
        key_notification_function: protocols::simple_text_input_ex::KeyNotifyFunction,
        notify_handle: *mut *mut c_void,
    ) -> efi::Status {
        if this.is_null()
            || key_data_ptr.is_null()
            || notify_handle.is_null()
            || key_notification_function as usize == 0
        {
            return efi::Status::INVALID_PARAMETER;
        }

        // SAFETY: casting `this` (first field of #[repr(C)] struct) to recover full context; null handled below.
        let Some(context) = (unsafe { (this as *const Self).as_ref() }) else {
            return efi::Status::INVALID_PARAMETER;
        };
        // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
        // accessed through shared references; null is handled by the check below.
        let keyboard_handler = unsafe { context.keyboard_handler.as_ref() };
        let Some(keyboard_handler) = keyboard_handler else {
            log::error!("simple_text_in_ex_register_key_notify invoked after keyboard dropped.");
            return efi::Status::DEVICE_ERROR;
        };

        // SAFETY: reading through pointer; null-checked at top of function.
        let key_data = unsafe { key_data_ptr.read() };
        let handle = keyboard_handler.insert_key_notify_callback(key_data, key_notification_function);
        log::trace!("simple_text_in_ex_register_key_notify: handle=0x{:X}", handle);
        // SAFETY: writing through output pointer; null-checked at top of function.
        unsafe { notify_handle.write(handle as *mut c_void) };
        efi::Status::SUCCESS
    }

    // Unregisters a key notification callback — part of the simple_text_in_ex protocol interface.
    extern "efiapi" fn simple_text_in_ex_unregister_key_notify(
        this: *mut protocols::simple_text_input_ex::Protocol,
        notification_handle: *mut c_void,
    ) -> efi::Status {
        if this.is_null() {
            return efi::Status::INVALID_PARAMETER;
        }
        // SAFETY: casting `this` (first field of #[repr(C)] struct) to recover full context; null handled below.
        let Some(context) = (unsafe { (this as *const Self).as_ref() }) else {
            return efi::Status::INVALID_PARAMETER;
        };
        // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
        // accessed through shared references; null is handled by the check below.
        let keyboard_handler = unsafe { context.keyboard_handler.as_ref() };
        let Some(keyboard_handler) = keyboard_handler else {
            log::error!("simple_text_in_ex_unregister_key_notify invoked after keyboard dropped.");
            return efi::Status::DEVICE_ERROR;
        };
        match keyboard_handler.remove_key_notify_callback(notification_handle as usize) {
            Ok(()) => {
                log::trace!("simple_text_in_ex_unregister_key_notify: handle=0x{:X}", notification_handle as usize);
                efi::Status::SUCCESS
            }
            Err(status) => status,
        }
    }

    /// Signals `event` if a (non-partial) key is available, discarding any leading partial keys.
    ///
    /// This is the notify logic for the wait_for_key_ex event.
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
                    log::error!("Failed to signal wait_for_key_ex: {:?}", error);
                }
                break;
            }
        }
    }

    /// Dispatches registered key notification callbacks.
    ///
    /// # Safety
    /// `context` must be null or point to a live `SimpleTextInExFfi`.
    unsafe fn process_key_notifies(context: *const Self) {
        // SAFETY: caller guarantees context is null or valid.
        let Some(context) = (unsafe { context.as_ref() }) else {
            return;
        };
        loop {
            // SAFETY: the handler outlives the installed protocol (it is leaked if uninstall fails) and is only
            // accessed through shared references; null is handled by the check below.
            let keyboard_handler = unsafe { context.keyboard_handler.as_ref() };
            let Some(keyboard_handler) = keyboard_handler else {
                log::error!("process_key_notifies event called without a valid keyboard_handler");
                return;
            };
            let (pending_key, pending_callbacks) = keyboard_handler.pending_callbacks();
            if let Some(mut pending_key) = pending_key {
                let key_ptr = &mut pending_key as *mut protocols::simple_text_input_ex::KeyData;
                for callback in pending_callbacks {
                    // SAFETY: key_ptr points to a valid KeyData value for the duration of the callback.
                    let _ = unsafe { callback(key_ptr) };
                }
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use alloc::rc::Rc;
    use core::{cell::Cell, ptr};

    use alloc::boxed::Box;
    use r_efi::{efi, protocols};

    use core::sync::atomic::{AtomicUsize, Ordering};

    use patina::component::service::uefi_services::{event::EventError, protocol::ProtocolError};

    use super::*;
    use crate::test_stubs::{mock_services, test_event, test_handle};

    fn mock_boot_services() -> HidServices {
        mock_services(|_, _, _| {})
    }

    fn test_context(boot_services: HidServices, handler: &mut KeyboardHidHandler) -> SimpleTextInExFfi {
        SimpleTextInExFfi {
            simple_text_in_ex: protocols::simple_text_input_ex::Protocol {
                reset: SimpleTextInExFfi::simple_text_in_ex_reset,
                read_key_stroke_ex: SimpleTextInExFfi::simple_text_in_ex_read_key_stroke,
                wait_for_key_ex: ptr::null_mut(),
                set_state: SimpleTextInExFfi::simple_text_in_ex_set_state,
                register_key_notify: SimpleTextInExFfi::simple_text_in_ex_register_key_notify,
                unregister_key_notify: SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify,
            },
            services: boot_services,
            keyboard_handler: handler as *mut KeyboardHidHandler,
        }
    }

    fn leaked_context(services: HidServices, handler: &mut KeyboardHidHandler) -> *mut SimpleTextInExFfi {
        let mut ctx = test_context(services, handler);
        ctx.simple_text_in_ex.wait_for_key_ex = test_event(0x42).as_raw();
        Box::into_raw(Box::new(ctx))
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
                assert_eq!(tpl, ServiceTpl::Notify);
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
        let (ctx, key_notify_event) = SimpleTextInExFfi::install(services, test_handle(0x2), &handler).unwrap();
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
        SimpleTextInExFfi::uninstall(services, test_handle(0x2), ctx, Some(key_notify_event)).unwrap();
    }

    #[test]
    fn install_succeeds() {
        let services = mock_services(|protocols, events, timers| {
            events.expect_create_wait_event().returning(|tpl, _| {
                assert_eq!(tpl, ServiceTpl::Notify);
                Ok(test_event(0x42))
            });
            timers.expect_set_timer().never();
            events.expect_create_event().returning(|tpl, _| {
                assert_eq!(tpl, ServiceTpl::Callback);
                Ok(test_event(0x99))
            });
            protocols.expect_install_interface().returning(|handle, guid, interface| {
                assert_eq!(handle, Some(test_handle(0x2)));
                assert_eq!(guid, protocols::simple_text_input_ex::PROTOCOL_GUID);
                // SAFETY: interface is the SimpleTextInExFfi context created by install.
                let ctx = unsafe { &*(interface.as_raw() as *const SimpleTextInExFfi) };
                assert_eq!(ctx.simple_text_in_ex.wait_for_key_ex, test_event(0x42).as_raw());
                Ok(test_handle(0x2))
            });
        });

        let handler = KeyboardHidHandler::new_for_test(services);
        let (ctx, key_notify_event) = SimpleTextInExFfi::install(services, test_handle(0x2), &handler).unwrap();
        assert_eq!(key_notify_event, test_event(0x99));

        // SAFETY: Reclaiming the Box leaked by install.
        drop(unsafe { Box::from_raw(ctx) });
    }

    #[test]
    fn install_returns_error_on_wait_event_failure() {
        let services = mock_services(|protocols, events, _| {
            events.expect_create_wait_event().returning(|_, _| Err(EventError::Internal));
            events.expect_create_event().never();
            protocols.expect_install_interface().never();
        });

        let handler = KeyboardHidHandler::new_for_test(services);
        let result = SimpleTextInExFfi::install(services, test_handle(0x2), &handler);
        assert_eq!(result.err(), Some(efi::Status::UNSUPPORTED));
    }

    #[test]
    fn install_cleans_up_wait_event_on_key_notify_event_failure() {
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            events.expect_create_event().returning(|_, _| Err(EventError::Internal));
            events.expect_close_event().returning(|_| {
                CLOSED.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            protocols.expect_install_interface().never();
        });

        let handler = KeyboardHidHandler::new_for_test(services);
        let result = SimpleTextInExFfi::install(services, test_handle(0x2), &handler);
        assert_eq!(result.err(), Some(efi::Status::UNSUPPORTED));
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn install_cleans_up_events_on_install_protocol_failure() {
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            protocols.expect_install_interface().returning(|_, _, _| Err(ProtocolError::AccessDenied));
            events.expect_close_event().returning(|_| {
                CLOSED.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let handler = KeyboardHidHandler::new_for_test(services);
        let result = SimpleTextInExFfi::install(services, test_handle(0x2), &handler);
        assert_eq!(result.err(), Some(efi::Status::ACCESS_DENIED));
        // Both wait_for_key_ex and key_notify events are closed.
        assert_eq!(CLOSED.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn uninstall_succeeds() {
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            protocols.expect_uninstall_interface().returning(|handle, guid, _| {
                assert_eq!(handle, test_handle(0x2));
                assert_eq!(guid, protocols::simple_text_input_ex::PROTOCOL_GUID);
                Ok(())
            });
            events.expect_close_event().returning(|_| {
                CLOSED.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let mut handler = KeyboardHidHandler::new_for_test(services);
        let ctx = leaked_context(services, &mut handler);
        assert_eq!(SimpleTextInExFfi::uninstall(services, test_handle(0x2), ctx, Some(test_event(0x99))), Ok(()));
        // Two close_event calls: wait_for_key_ex and key_notify_event.
        assert_eq!(CLOSED.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn uninstall_failure_keeps_context() {
        let services = mock_services(|protocols, _, _| {
            protocols.expect_uninstall_interface().returning(|_, _, _| Err(ProtocolError::AccessDenied));
        });

        let mut handler = KeyboardHidHandler::new_for_test(services);
        let ctx = leaked_context(services, &mut handler);

        let result = SimpleTextInExFfi::uninstall(services, test_handle(0x2), ctx, None);
        assert_eq!(result, Err(efi::Status::ACCESS_DENIED));

        // The context is still installed and still references the handler, which the caller must therefore leak.
        // SAFETY: ctx points to the still-live leaked context (uninstall failed).
        assert_eq!(unsafe { &*ctx }.keyboard_handler, &raw const handler);

        // SAFETY: Reclaiming the leaked context Box for cleanup.
        drop(unsafe { Box::from_raw(ctx) });
    }

    // --- FFI callbacks ---

    #[test]
    fn reset_returns_invalid_parameter_on_null() {
        let status = SimpleTextInExFfi::simple_text_in_ex_reset(ptr::null_mut(), false.into());
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn reset_returns_device_error_when_handler_null() {
        let boot_services = mock_boot_services();
        let mut ctx = SimpleTextInExFfi {
            simple_text_in_ex: protocols::simple_text_input_ex::Protocol {
                reset: SimpleTextInExFfi::simple_text_in_ex_reset,
                read_key_stroke_ex: SimpleTextInExFfi::simple_text_in_ex_read_key_stroke,
                wait_for_key_ex: ptr::null_mut(),
                set_state: SimpleTextInExFfi::simple_text_in_ex_set_state,
                register_key_notify: SimpleTextInExFfi::simple_text_in_ex_register_key_notify,
                unregister_key_notify: SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify,
            },
            services: boot_services,
            keyboard_handler: ptr::null_mut(),
        };
        let status = SimpleTextInExFfi::simple_text_in_ex_reset(&mut ctx.simple_text_in_ex as *mut _, false.into());
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn read_key_stroke_returns_not_ready_on_empty_queue() {
        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &mut handler);
        let mut key_data = protocols::simple_text_input_ex::KeyData::default();
        let status =
            SimpleTextInExFfi::simple_text_in_ex_read_key_stroke(&mut ctx.simple_text_in_ex as *mut _, &mut key_data);
        assert_eq!(status, efi::Status::NOT_READY);
    }

    #[test]
    fn read_key_stroke_returns_invalid_parameter_on_null() {
        let status = SimpleTextInExFfi::simple_text_in_ex_read_key_stroke(ptr::null_mut(), ptr::null_mut());
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn set_state_returns_invalid_parameter_on_null() {
        let status = SimpleTextInExFfi::simple_text_in_ex_set_state(ptr::null_mut(), ptr::null_mut());
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn register_key_notify_returns_invalid_parameter_on_null_this() {
        // All parameters must be non-null. Testing with null `this` is sufficient
        // since the first check short-circuits.
        extern "efiapi" fn noop(_key: *mut protocols::simple_text_input_ex::KeyData) -> efi::Status {
            efi::Status::SUCCESS
        }
        let mut key_data = protocols::simple_text_input_ex::KeyData::default();
        let mut handle: *mut c_void = ptr::null_mut();
        let status =
            SimpleTextInExFfi::simple_text_in_ex_register_key_notify(ptr::null_mut(), &mut key_data, noop, &mut handle);
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn unregister_key_notify_returns_invalid_parameter_on_null() {
        let status = SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify(ptr::null_mut(), ptr::null_mut());
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn reset_succeeds_with_valid_handler() {
        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &mut handler);
        let status = SimpleTextInExFfi::simple_text_in_ex_reset(&mut ctx.simple_text_in_ex as *mut _, false.into());
        assert_eq!(status, efi::Status::SUCCESS);
    }

    #[test]
    fn read_key_stroke_returns_success_when_key_available() {
        use hidparser::report_data_types::Usage;

        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);
        handler.set_layout(Some(crate::keyboard::layout::get_default_keyboard_layout()));
        // Push an Enter keystroke (usage 0x00070028).
        handler
            .state
            .lock()
            .key_queue
            .keystroke(Usage::from(0x00070028u32), super::super::key_queue::KeyAction::KeyDown);
        let mut ctx = test_context(boot_services, &mut handler);
        let mut key_data = protocols::simple_text_input_ex::KeyData::default();
        let status =
            SimpleTextInExFfi::simple_text_in_ex_read_key_stroke(&mut ctx.simple_text_in_ex as *mut _, &mut key_data);
        assert_eq!(status, efi::Status::SUCCESS);
        assert!(key_data.key.unicode_char != 0 || key_data.key.scan_code != 0);
    }

    #[test]
    fn read_key_stroke_returns_device_error_when_handler_null() {
        let boot_services = mock_boot_services();
        let mut ctx = SimpleTextInExFfi {
            simple_text_in_ex: protocols::simple_text_input_ex::Protocol {
                reset: SimpleTextInExFfi::simple_text_in_ex_reset,
                read_key_stroke_ex: SimpleTextInExFfi::simple_text_in_ex_read_key_stroke,
                wait_for_key_ex: ptr::null_mut(),
                set_state: SimpleTextInExFfi::simple_text_in_ex_set_state,
                register_key_notify: SimpleTextInExFfi::simple_text_in_ex_register_key_notify,
                unregister_key_notify: SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify,
            },
            services: boot_services,
            keyboard_handler: ptr::null_mut(),
        };
        let mut key_data = protocols::simple_text_input_ex::KeyData::default();
        let status =
            SimpleTextInExFfi::simple_text_in_ex_read_key_stroke(&mut ctx.simple_text_in_ex as *mut _, &mut key_data);
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn set_state_succeeds_with_valid_handler() {
        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &mut handler);
        let mut toggle_state: protocols::simple_text_input_ex::KeyToggleState =
            protocols::simple_text_input_ex::TOGGLE_STATE_VALID;
        let status =
            SimpleTextInExFfi::simple_text_in_ex_set_state(&mut ctx.simple_text_in_ex as *mut _, &mut toggle_state);
        assert_eq!(status, efi::Status::SUCCESS);
    }

    #[test]
    fn set_state_returns_device_error_when_handler_null() {
        let boot_services = mock_boot_services();
        let mut ctx = SimpleTextInExFfi {
            simple_text_in_ex: protocols::simple_text_input_ex::Protocol {
                reset: SimpleTextInExFfi::simple_text_in_ex_reset,
                read_key_stroke_ex: SimpleTextInExFfi::simple_text_in_ex_read_key_stroke,
                wait_for_key_ex: ptr::null_mut(),
                set_state: SimpleTextInExFfi::simple_text_in_ex_set_state,
                register_key_notify: SimpleTextInExFfi::simple_text_in_ex_register_key_notify,
                unregister_key_notify: SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify,
            },
            services: boot_services,
            keyboard_handler: ptr::null_mut(),
        };
        let mut toggle_state: protocols::simple_text_input_ex::KeyToggleState =
            protocols::simple_text_input_ex::TOGGLE_STATE_VALID;
        let status =
            SimpleTextInExFfi::simple_text_in_ex_set_state(&mut ctx.simple_text_in_ex as *mut _, &mut toggle_state);
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn register_key_notify_succeeds() {
        extern "efiapi" fn noop(_key: *mut protocols::simple_text_input_ex::KeyData) -> efi::Status {
            efi::Status::SUCCESS
        }

        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &mut handler);
        let mut key_data = protocols::simple_text_input_ex::KeyData::default();
        let mut handle: *mut c_void = ptr::null_mut();
        let status = SimpleTextInExFfi::simple_text_in_ex_register_key_notify(
            &mut ctx.simple_text_in_ex as *mut _,
            &mut key_data,
            noop,
            &mut handle,
        );
        assert_eq!(status, efi::Status::SUCCESS);
        assert!(!handle.is_null());
    }

    #[test]
    fn register_key_notify_returns_device_error_when_handler_null() {
        extern "efiapi" fn noop(_key: *mut protocols::simple_text_input_ex::KeyData) -> efi::Status {
            efi::Status::SUCCESS
        }

        let boot_services = mock_boot_services();
        let mut ctx = SimpleTextInExFfi {
            simple_text_in_ex: protocols::simple_text_input_ex::Protocol {
                reset: SimpleTextInExFfi::simple_text_in_ex_reset,
                read_key_stroke_ex: SimpleTextInExFfi::simple_text_in_ex_read_key_stroke,
                wait_for_key_ex: ptr::null_mut(),
                set_state: SimpleTextInExFfi::simple_text_in_ex_set_state,
                register_key_notify: SimpleTextInExFfi::simple_text_in_ex_register_key_notify,
                unregister_key_notify: SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify,
            },
            services: boot_services,
            keyboard_handler: ptr::null_mut(),
        };
        let mut key_data = protocols::simple_text_input_ex::KeyData::default();
        let mut handle: *mut c_void = ptr::null_mut();
        let status = SimpleTextInExFfi::simple_text_in_ex_register_key_notify(
            &mut ctx.simple_text_in_ex as *mut _,
            &mut key_data,
            noop,
            &mut handle,
        );
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn unregister_key_notify_returns_device_error_when_handler_null() {
        let boot_services = mock_boot_services();
        let mut ctx = SimpleTextInExFfi {
            simple_text_in_ex: protocols::simple_text_input_ex::Protocol {
                reset: SimpleTextInExFfi::simple_text_in_ex_reset,
                read_key_stroke_ex: SimpleTextInExFfi::simple_text_in_ex_read_key_stroke,
                wait_for_key_ex: ptr::null_mut(),
                set_state: SimpleTextInExFfi::simple_text_in_ex_set_state,
                register_key_notify: SimpleTextInExFfi::simple_text_in_ex_register_key_notify,
                unregister_key_notify: SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify,
            },
            services: boot_services,
            keyboard_handler: ptr::null_mut(),
        };
        let status = SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify(
            &mut ctx.simple_text_in_ex as *mut _,
            core::ptr::dangling_mut::<c_void>(),
        );
        assert_eq!(status, efi::Status::DEVICE_ERROR);
    }

    #[test]
    fn unregister_key_notify_succeeds_after_register() {
        extern "efiapi" fn noop(_key: *mut protocols::simple_text_input_ex::KeyData) -> efi::Status {
            efi::Status::SUCCESS
        }

        let boot_services = mock_boot_services();
        let mut handler = KeyboardHidHandler::new_for_test(boot_services);
        let mut ctx = test_context(boot_services, &mut handler);
        let mut key_data = protocols::simple_text_input_ex::KeyData::default();
        let mut handle: *mut c_void = ptr::null_mut();
        let status = SimpleTextInExFfi::simple_text_in_ex_register_key_notify(
            &mut ctx.simple_text_in_ex as *mut _,
            &mut key_data,
            noop,
            &mut handle,
        );
        assert_eq!(status, efi::Status::SUCCESS);

        let status =
            SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify(&mut ctx.simple_text_in_ex as *mut _, handle);
        assert_eq!(status, efi::Status::SUCCESS);
    }

    #[test]
    fn signal_if_key_available_signals_event_when_key_available() {
        use hidparser::report_data_types::Usage;

        static SIGNALS: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|_, events, _| {
            events.expect_signal_event().returning(|event| {
                assert_eq!(event, test_event(0x1234));
                SIGNALS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let mut handler = KeyboardHidHandler::new_for_test(services);
        handler.set_layout(Some(crate::keyboard::layout::get_default_keyboard_layout()));
        handler
            .state
            .lock()
            .key_queue
            .keystroke(Usage::from(0x00070028u32), super::super::key_queue::KeyAction::KeyDown);
        let ctx = test_context(services, &mut handler);
        SimpleTextInExFfi::signal_if_key_available(ctx.services, ctx.keyboard_handler, test_event(0x1234));
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn process_key_notifies_handles_null_context_and_handler() {
        let services = mock_boot_services();
        let ctx = SimpleTextInExFfi {
            simple_text_in_ex: protocols::simple_text_input_ex::Protocol {
                reset: SimpleTextInExFfi::simple_text_in_ex_reset,
                read_key_stroke_ex: SimpleTextInExFfi::simple_text_in_ex_read_key_stroke,
                wait_for_key_ex: ptr::null_mut(),
                set_state: SimpleTextInExFfi::simple_text_in_ex_set_state,
                register_key_notify: SimpleTextInExFfi::simple_text_in_ex_register_key_notify,
                unregister_key_notify: SimpleTextInExFfi::simple_text_in_ex_unregister_key_notify,
            },
            services,
            keyboard_handler: ptr::null_mut(),
        };
        // Should return without panic when context or keyboard_handler is null.
        // SAFETY: null and a live context are both valid inputs.
        unsafe {
            SimpleTextInExFfi::process_key_notifies(ptr::null_mut());
            SimpleTextInExFfi::process_key_notifies(&ctx);
        }
    }
}
