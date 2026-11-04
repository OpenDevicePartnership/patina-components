//! USB HID driver binding implementation.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
use alloc::boxed::Box;

use patina::{
    component::service::{
        Service,
        uefi_services::{
            driver_binding::DriverBinding,
            event::{EventError, EventServices},
            handle::Handle,
            protocol::{OpenAttributes, ProtocolError, ProtocolPtr, ProtocolServices},
            timer_event::{TimerEventServices, TimerEventServicesExt, TimerType, Tpl},
        },
    },
    error::EfiError,
    uefi::device_path::walker::BorrowedDevicePathWalker,
    vendor_protocols::hid_io,
};
use r_efi::{efi, efi::protocols::usb_io};

use crate::{control_transfers, descriptors, device::UsbHidDevice, hid_io_impl, interrupt_transfers, usb_hid_defs::*};

/// USB HID driver that implements the Patina UEFI Services driver binding.
pub struct UsbHidDriver {
    protocols: Service<dyn ProtocolServices>,
    events: Service<dyn EventServices>,
    timers: Service<dyn TimerEventServices>,
}

impl UsbHidDriver {
    /// Creates a new USB HID driver using the given UEFI services.
    pub fn new(
        protocols: Service<dyn ProtocolServices>,
        events: Service<dyn EventServices>,
        timers: Service<dyn TimerEventServices>,
    ) -> Self {
        Self { protocols, events, timers }
    }

    fn close_usb_io(&self, agent: Handle, controller: Handle) {
        if let Err(error) =
            self.protocols.close_interface(controller, usb_io::PROTOCOL_GUID.into(), agent, Some(controller))
        {
            log::error!("USB HID: error closing USB IO protocol: {error:?}");
        }
    }
}

fn event_to_protocol_error(error: EventError) -> ProtocolError {
    ProtocolError::from(EfiError::from(error))
}

fn is_usb_hid(usb_io_protocol: &usb_io::Protocol) -> bool {
    let mut interface_descriptor = empty_usb_interface_descriptor();
    // SAFETY: usb_io_protocol and interface_descriptor are valid.
    let status = unsafe {
        (usb_io_protocol.get_interface_descriptor)(
            usb_io_protocol as *const usb_io::Protocol as *mut usb_io::Protocol,
            &mut interface_descriptor,
        )
    };
    status == efi::Status::SUCCESS && interface_descriptor.interface_class == CLASS_HID
}

impl DriverBinding for UsbHidDriver {
    /// Tests whether the controller is a USB HID interface.
    fn supported(
        &self,
        agent: Handle,
        controller: Handle,
        _remaining_device_path: Option<BorrowedDevicePathWalker<'_>>,
    ) -> Result<(), ProtocolError> {
        // Open BY_DRIVER to confirm the device is not already managed.
        let usb_io = self.protocols.open_interface(
            controller,
            usb_io::PROTOCOL_GUID.into(),
            agent,
            OpenAttributes::ByDriver { controller: Some(controller) },
        )?;
        // SAFETY: the protocol database returned a USB IO interface that is still opened BY_DRIVER.
        let supported = is_usb_hid(unsafe { &*usb_io.as_raw().cast::<usb_io::Protocol>() });
        self.close_usb_io(agent, controller);
        if supported { Ok(()) } else { Err(ProtocolError::NotFound) }
    }

    /// Starts USB HID support for the controller and installs HidIo on it.
    fn start(
        &self,
        agent: Handle,
        controller: Handle,
        _remaining_device_path: Option<BorrowedDevicePathWalker<'_>>,
    ) -> Result<(), ProtocolError> {
        log::trace!("USB HID: start on controller {:?}", controller);

        // Open USB IO BY_DRIVER for the lifetime of the device. It is closed explicitly in stop
        // (or on any error path below), so the scoped ProtocolGuard is not used here.
        let usb_io_ptr = self
            .protocols
            .open_interface(
                controller,
                usb_io::PROTOCOL_GUID.into(),
                agent,
                OpenAttributes::ByDriver { controller: Some(controller) },
            )?
            .as_raw() as *mut usb_io::Protocol;
        // SAFETY: the protocol database returned a non-null USB IO interface that remains valid while opened BY_DRIVER.
        let usb_io = unsafe { &*usb_io_ptr };

        let descriptors = match descriptors::read_descriptors(usb_io) {
            Ok(descriptors) => descriptors,
            Err(status) => {
                log::error!("USB HID: failed to read descriptors: {status:x?}");
                self.close_usb_io(agent, controller);
                return Err(EfiError::from(status).into());
            }
        };

        // Boot devices: explicitly set report protocol mode.
        if descriptors.interface_descriptor.interface_sub_class == SUBCLASS_BOOT
            && let Err(status) = control_transfers::set_protocol_request(
                usb_io,
                descriptors.interface_descriptor.interface_number,
                REPORT_PROTOCOL,
            )
        {
            log::warn!("USB HID: failed to set report protocol: {status:x?}");
        }

        let device_ptr = Box::into_raw(Box::new(UsbHidDevice {
            hid_io: hid_io_impl::new_hid_io_protocol(),
            usb_io: usb_io_ptr,
            descriptors,
            report_callback: crate::device::ReportCallbackState::default(),
            timer_services: self.timers,
            recovery_event: None,
        }));

        // Recovery timer for delayed re-submission on transfer errors. It runs at TPL_CALLBACK, below the
        // typical interrupt transfer TPL.
        let recovery_event = self.timers.on_timer_event(Tpl::Callback, move || {
            // SAFETY: the event is closed in stop (or the error path below) before the device is freed.
            unsafe { interrupt_transfers::recovery_timer_notify(device_ptr) };
        });
        let recovery_event = match recovery_event {
            Ok(event) => event,
            Err(error) => {
                log::error!("USB HID: failed to create recovery event: {error:?}");
                // SAFETY: device_ptr was created by Box::into_raw above and has not been published.
                drop(unsafe { Box::from_raw(device_ptr) });
                self.close_usb_io(agent, controller);
                return Err(event_to_protocol_error(error));
            }
        };
        // SAFETY: device_ptr is valid and not yet shared.
        unsafe { (*device_ptr).recovery_event = Some(recovery_event) };

        // hid_io is the first field in #[repr(C)] UsbHidDevice, so device_ptr is also the HidIo interface pointer.
        let interface = ProtocolPtr::from_raw(device_ptr.cast()).ok_or(ProtocolError::InvalidParameter)?;
        if let Err(error) = self.protocols.install_interface(Some(controller), hid_io::HID_IO_PROTOCOL_GUID, interface)
        {
            log::error!("USB HID: failed to install HidIo protocol: {error:?}");
            let _ = self.events.close_event(recovery_event);
            // SAFETY: device_ptr was created by Box::into_raw above and was not published.
            drop(unsafe { Box::from_raw(device_ptr) });
            self.close_usb_io(agent, controller);
            return Err(error);
        }

        Ok(())
    }

    /// Stops USB HID support for the controller.
    fn stop(&self, agent: Handle, controller: Handle, _children: &[Handle]) -> Result<(), ProtocolError> {
        log::trace!("USB HID: stop on controller {:?}", controller);
        let hid_ptr = self.protocols.interface_on_handle(controller, hid_io::HID_IO_PROTOCOL_GUID)?;
        let device = UsbHidDevice::from_hid_io_protocol(hid_ptr.as_raw() as *const hid_io::HidIoProtocol);
        // SAFETY: the protocol was installed by start and points to the hid_io field of a UsbHidDevice.
        let device_ref = unsafe { &mut *device };

        // Shutdown async transfers first so that no further interrupt callbacks can
        // fire and attempt to arm the recovery timer after it is closed.
        let _ = interrupt_transfers::shutdown_async_interrupt_input_transfers(device_ref);

        if let Some(event) = device_ref.recovery_event.take() {
            let _ = self.timers.set_timer(event, TimerType::Cancel);
            let _ = self.events.close_event(event);
        }

        if let Err(error) = self.protocols.uninstall_interface(controller, hid_io::HID_IO_PROTOCOL_GUID, hid_ptr) {
            log::error!("USB HID: failed to uninstall HidIo: {error:?}");
            return Err(error);
        }

        // SAFETY: device was created via Box::into_raw in start and is no longer published.
        drop(unsafe { Box::from_raw(device) });

        self.close_usb_io(agent, controller);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

    use patina::component::service::uefi_services::{
        event::MockEventServices, protocol::MockProtocolServices, timer_event::MockTimerEventServices,
    };

    // Interface class returned by the stub get_interface_descriptor; 0 means "fail the call".
    static INTERFACE_CLASS: AtomicU8 = AtomicU8::new(0);

    extern "efiapi" fn stub_get_interface_descriptor(
        _this: *mut usb_io::Protocol,
        descriptor: *mut usb_io::InterfaceDescriptor,
    ) -> efi::Status {
        match INTERFACE_CLASS.load(Ordering::SeqCst) {
            0 => efi::Status::DEVICE_ERROR,
            class => {
                // SAFETY: descriptor is a valid pointer supplied by the caller.
                unsafe { (*descriptor).interface_class = class };
                efi::Status::SUCCESS
            }
        }
    }

    fn leaked_usb_io() -> ProtocolPtr {
        let mut usb_io = crate::test_stubs::usb_io_stub();
        usb_io.get_interface_descriptor = stub_get_interface_descriptor;
        ProtocolPtr::from_raw(Box::into_raw(Box::new(usb_io)).cast()).unwrap()
    }

    fn handle(value: usize) -> Handle {
        Handle::from_raw(value as efi::Handle).unwrap()
    }

    fn driver(protocols: MockProtocolServices) -> UsbHidDriver {
        UsbHidDriver::new(
            Service::mock(Box::new(protocols)),
            Service::mock(Box::new(MockEventServices::new())),
            Service::mock(Box::new(MockTimerEventServices::new())),
        )
    }

    fn protocols_with_usb_io(close_count: &'static AtomicUsize) -> MockProtocolServices {
        let mut protocols = MockProtocolServices::new();
        protocols.expect_open_interface().returning(|controller, guid, agent, attributes| {
            assert_eq!(guid, usb_io::PROTOCOL_GUID);
            assert_eq!(agent, handle(0x1));
            assert_eq!(attributes, OpenAttributes::ByDriver { controller: Some(controller) });
            Ok(leaked_usb_io())
        });
        protocols.expect_close_interface().returning(move |controller, guid, agent, by_controller| {
            assert_eq!(guid, usb_io::PROTOCOL_GUID);
            assert_eq!(agent, handle(0x1));
            assert_eq!(by_controller, Some(controller));
            close_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        protocols
    }

    // Serializes tests that depend on INTERFACE_CLASS.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn supported_returns_error_when_no_usb_io() {
        let mut protocols = MockProtocolServices::new();
        protocols.expect_open_interface().returning(|_, _, _, _| Err(ProtocolError::NotFound));
        assert_eq!(driver(protocols).supported(handle(0x1), handle(0x2), None), Err(ProtocolError::NotFound));
    }

    #[test]
    fn supported_accepts_hid_and_rejects_non_hid() {
        let _lock = LOCK.lock().unwrap();
        static CLOSES: AtomicUsize = AtomicUsize::new(0);
        let driver = driver(protocols_with_usb_io(&CLOSES));

        INTERFACE_CLASS.store(CLASS_HID, Ordering::SeqCst);
        assert_eq!(driver.supported(handle(0x1), handle(0x2), None), Ok(()));

        INTERFACE_CLASS.store(CLASS_HID + 1, Ordering::SeqCst);
        assert_eq!(driver.supported(handle(0x1), handle(0x2), None), Err(ProtocolError::NotFound));

        INTERFACE_CLASS.store(0, Ordering::SeqCst);
        assert_eq!(driver.supported(handle(0x1), handle(0x2), None), Err(ProtocolError::NotFound));

        // Every supported() probe must release its BY_DRIVER open.
        assert_eq!(CLOSES.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn start_propagates_open_failure() {
        let mut protocols = MockProtocolServices::new();
        protocols.expect_open_interface().returning(|_, _, _, _| Err(ProtocolError::AccessDenied));
        assert_eq!(driver(protocols).start(handle(0x1), handle(0x2), None), Err(ProtocolError::AccessDenied));
    }

    #[test]
    fn start_closes_usb_io_when_descriptor_read_fails() {
        let _lock = LOCK.lock().unwrap();
        static CLOSES: AtomicUsize = AtomicUsize::new(0);
        INTERFACE_CLASS.store(0, Ordering::SeqCst);

        let result = driver(protocols_with_usb_io(&CLOSES)).start(handle(0x1), handle(0x2), None);
        assert!(result.is_err());
        assert_eq!(CLOSES.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stop_propagates_missing_hid_io() {
        let mut protocols = MockProtocolServices::new();
        protocols.expect_interface_on_handle().returning(|_, _| Err(ProtocolError::NotFound));
        assert_eq!(driver(protocols).stop(handle(0x1), handle(0x2), &[]), Err(ProtocolError::NotFound));
    }
}
