//! Async interrupt transfer management for USB HID.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
use core::{ffi::c_void, time::Duration};

use r_efi::{efi, efi::protocols::usb_io};

use patina::component::service::uefi_services::timer_event::TimerType;

use crate::{control_transfers, device::UsbHidDevice};

/// Delay before re-submitting after a transfer error.
/// 100ms matches the standard EDKII `EFI_USB_INTERRUPT_DELAY`.
const RECOVERY_DELAY: Duration = Duration::from_millis(100);

/// Recovery timer notify handler that re-submits the async interrupt transfer
/// after a transfer error.
///
/// Registered as the closure body of the device's recovery timer event.
///
/// # Safety
///
/// `device` must be null or point to a valid [`UsbHidDevice`]. The recovery event
/// must be closed before the device is freed.
pub(crate) unsafe fn recovery_timer_notify(device: *mut UsbHidDevice) {
    // SAFETY: caller guarantees device is null or valid.
    let Some(device) = (unsafe { device.as_mut() }) else {
        return;
    };

    // If the callback was unregistered while the timer was armed, do not re-submit.
    if device.report_callback.callback.is_none() {
        return;
    }

    if let Err(status) = initiate_async_interrupt_input_transfers(device) {
        log::warn!("USB HID: recovery re-submit failed: {status:x?}");
    }
}

/// Initiates input reports from the endpoint by scheduling an async interrupt
/// transfer to poll the device.
pub fn initiate_async_interrupt_input_transfers(device: &mut UsbHidDevice) -> Result<(), efi::Status> {
    // SAFETY: usb_io was opened BY_DRIVER and is valid for the device's lifetime.
    let usb_io = unsafe { &*device.usb_io };

    // SAFETY: usb_io was opened BY_DRIVER and is valid; transfer parameters are valid.
    let status = unsafe {
        (usb_io.async_interrupt_transfer)(
            device.usb_io,
            device.descriptors.int_in_endpoint_descriptor.endpoint_address,
            true.into(),
            device.descriptors.int_in_endpoint_descriptor.interval as usize,
            device.descriptors.int_in_endpoint_descriptor.max_packet_size as usize,
            Some(on_report_interrupt_complete),
            device as *mut UsbHidDevice as *mut c_void,
        )
    };
    if status != efi::Status::SUCCESS {
        return Err(status);
    }

    Ok(())
}

/// Shuts down async interrupt transfers.
pub fn shutdown_async_interrupt_input_transfers(device: &mut UsbHidDevice) -> Result<(), efi::Status> {
    // SAFETY: usb_io is valid for the device's lifetime.
    let usb_io = unsafe { &*device.usb_io };

    // Cancel the async interrupt transfer.
    // SAFETY: usb_io is valid; cancellation parameters are valid.
    let status = unsafe {
        (usb_io.async_interrupt_transfer)(
            device.usb_io,
            device.descriptors.int_in_endpoint_descriptor.endpoint_address,
            false.into(),
            0,
            0,
            None,
            core::ptr::null_mut(),
        )
    };
    if status != efi::Status::SUCCESS && status != efi::Status::NOT_FOUND {
        log::warn!("USB HID: unexpected error shutting down async transfer: {status:x?}");
    }

    Ok(())
}

/// Interrupt completion callback. Invoked by the USB bus driver when data
/// arrives on the interrupt-in endpoint (or on error).
///
/// # Safety
///
/// `context` must be a valid pointer to a [`UsbHidDevice`] that was set
/// during transfer initiation. `data` must be valid for `data_length` bytes
/// when `result` indicates success.
unsafe extern "efiapi" fn on_report_interrupt_complete(
    data: *mut c_void,
    data_length: usize,
    context: *mut c_void,
    result: u32,
) -> efi::Status {
    if context.is_null() {
        return efi::Status::INVALID_PARAMETER;
    }

    // SAFETY: context is a pointer to UsbHidDevice set during transfer initiation.
    let device = unsafe { &mut *(context as *mut UsbHidDevice) };

    if result != usb_io::NOERROR {
        // Handle stall by clearing the endpoint halt.
        if (result & usb_io::ERR_STALL) != 0 {
            // SAFETY: usb_io is valid for the device's lifetime.
            let usb_io = unsafe { &*device.usb_io };
            let _ = control_transfers::usb_clear_endpoint_halt(
                usb_io,
                device.descriptors.int_in_endpoint_descriptor.endpoint_address,
            );
        }

        // Cancel the current async transfer and re-submit.
        // SAFETY: usb_io is valid for the device's lifetime.
        let usb_io = unsafe { &*device.usb_io };
        // SAFETY: usb_io is valid; cancelling the current async transfer.
        let _ = unsafe {
            (usb_io.async_interrupt_transfer)(
                device.usb_io,
                device.descriptors.int_in_endpoint_descriptor.endpoint_address,
                false.into(),
                0,
                0,
                None,
                core::ptr::null_mut(),
            )
        };

        // Arm the recovery timer for delayed re-submission.
        if let Some(event) = device.recovery_event {
            let _ = device.timer_services.set_timer(event, TimerType::Relative(RECOVERY_DELAY));
        }

        return efi::Status::DEVICE_ERROR;
    }

    if data_length > u16::MAX as usize {
        return efi::Status::DEVICE_ERROR;
    }

    if data_length == 0 || data.is_null() {
        return efi::Status::SUCCESS;
    }

    if let Some(callback) = device.report_callback.callback {
        // SAFETY: callback was registered via HidIo protocol; data and context are valid.
        unsafe { callback(data_length as u16, data, device.report_callback.context) };
    }

    efi::Status::SUCCESS
}

#[cfg(test)]
mod test {
    use super::*;
    use alloc::vec;
    use core::{
        cell::Cell,
        sync::atomic::{AtomicU16, AtomicUsize, Ordering},
    };

    use patina::component::service::{
        Service,
        uefi_services::{
            event::Event,
            timer_event::{MockTimerEventServices, TimerEventServices},
        },
    };

    use crate::{
        device::{ReportCallbackState, UsbHidDescriptors, UsbHidDevice},
        hid_io_impl,
        usb_hid_defs::{empty_usb_endpoint_descriptor, empty_usb_interface_descriptor},
    };

    // ---- Mock USB IO ----

    /// Mock USB IO context. `protocol` must be the first field so extern mock
    /// functions can recover mock state from the `this` pointer.
    #[repr(C)]
    struct MockUsbIo {
        protocol: usb_io::Protocol,
        control_transfer_status: efi::Status,
        async_transfer_status: efi::Status,
        async_call_count: Cell<usize>,
        control_call_count: Cell<usize>,
    }

    impl MockUsbIo {
        /// # Safety
        /// `this` must point to the `protocol` field of a valid `MockUsbIo`.
        unsafe fn from_this(this: *mut usb_io::Protocol) -> &'static Self {
            // SAFETY: MockUsbIo is #[repr(C)] with protocol as first field.
            unsafe { &*(this as *const MockUsbIo) }
        }
    }

    extern "efiapi" fn mock_control_transfer(
        this: *mut usb_io::Protocol,
        _request: *mut usb_io::DeviceRequest,
        _direction: usb_io::DataDirection,
        _timeout: u32,
        _data: *mut c_void,
        _data_length: usize,
        _status: *mut u32,
    ) -> efi::Status {
        // SAFETY: this points to a valid MockUsbIo on the test stack.
        let mock = unsafe { MockUsbIo::from_this(this) };
        mock.control_call_count.set(mock.control_call_count.get() + 1);
        mock.control_transfer_status
    }

    extern "efiapi" fn mock_async_interrupt_transfer(
        this: *mut usb_io::Protocol,
        _endpoint: u8,
        _is_new_transfer: efi::Boolean,
        _polling_interval: usize,
        _data_length: usize,
        _callback: Option<usb_io::AsyncUsbTransferCallback>,
        _context: *mut c_void,
    ) -> efi::Status {
        // SAFETY: this points to a valid MockUsbIo on the test stack.
        let mock = unsafe { MockUsbIo::from_this(this) };
        mock.async_call_count.set(mock.async_call_count.get() + 1);
        mock.async_transfer_status
    }

    fn make_mock_usb_io(control_status: efi::Status, async_status: efi::Status) -> MockUsbIo {
        let mut protocol = crate::test_stubs::usb_io_stub();
        protocol.control_transfer = mock_control_transfer;
        protocol.async_interrupt_transfer = mock_async_interrupt_transfer;
        MockUsbIo {
            protocol,
            control_transfer_status: control_status,
            async_transfer_status: async_status,
            async_call_count: Cell::new(0),
            control_call_count: Cell::new(0),
        }
    }

    // ---- Timer services for tests ----

    fn noop_timer_services() -> Service<dyn TimerEventServices> {
        let mut mock = MockTimerEventServices::new();
        mock.expect_set_timer().returning(|_, _| Ok(()));
        Service::mock(Box::new(mock))
    }

    fn make_device(usb_io: &MockUsbIo) -> Box<UsbHidDevice> {
        Box::new(UsbHidDevice {
            hid_io: hid_io_impl::new_hid_io_protocol(),
            usb_io: &usb_io.protocol as *const usb_io::Protocol as *mut usb_io::Protocol,
            descriptors: UsbHidDescriptors {
                interface_descriptor: empty_usb_interface_descriptor(),
                int_in_endpoint_descriptor: usb_io::EndpointDescriptor {
                    endpoint_address: 0x81,
                    interval: 10,
                    max_packet_size: 8,
                    ..empty_usb_endpoint_descriptor()
                },
                report_descriptor: vec![0x05, 0x01],
            },
            report_callback: ReportCallbackState::default(),
            timer_services: noop_timer_services(),
            recovery_event: None,
        })
    }

    // ---- initiate tests ----

    #[test]
    fn initiate_transfers_succeeds() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        assert!(initiate_async_interrupt_input_transfers(&mut device).is_ok());
        assert_eq!(mock_usb.async_call_count.get(), 1);
        core::mem::forget(device);
    }

    #[test]
    fn initiate_transfers_returns_error_on_failure() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::DEVICE_ERROR);
        let mut device = make_device(&mock_usb);
        assert_eq!(initiate_async_interrupt_input_transfers(&mut device), Err(efi::Status::DEVICE_ERROR),);
        core::mem::forget(device);
    }

    // ---- shutdown tests ----

    #[test]
    fn shutdown_transfers_succeeds() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        assert!(shutdown_async_interrupt_input_transfers(&mut device).is_ok());
        assert_eq!(mock_usb.async_call_count.get(), 1);
        core::mem::forget(device);
    }

    #[test]
    fn shutdown_transfers_tolerates_not_found() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::NOT_FOUND);
        let mut device = make_device(&mock_usb);
        assert!(shutdown_async_interrupt_input_transfers(&mut device).is_ok());
        core::mem::forget(device);
    }

    #[test]
    fn shutdown_transfers_tolerates_unexpected_error() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::DEVICE_ERROR);
        let mut device = make_device(&mock_usb);
        // shutdown always returns Ok even on unexpected errors (it just logs a warning).
        assert!(shutdown_async_interrupt_input_transfers(&mut device).is_ok());
        core::mem::forget(device);
    }

    // ---- on_report_interrupt_complete tests ----

    #[test]
    fn callback_null_context_returns_invalid_parameter() {
        let report = [0u8; 8];
        // SAFETY: testing null context handling.
        let status = unsafe {
            on_report_interrupt_complete(
                report.as_ptr() as *mut c_void,
                report.len(),
                core::ptr::null_mut(),
                usb_io::NOERROR,
            )
        };
        assert_eq!(status, efi::Status::INVALID_PARAMETER);
    }

    #[test]
    fn callback_usb_error_returns_device_error_and_arms_timer() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        let device_ptr = &mut *device as *mut UsbHidDevice;
        // SAFETY: device_ptr is a valid UsbHidDevice.
        let status = unsafe {
            on_report_interrupt_complete(
                core::ptr::null_mut(),
                0,
                device_ptr as *mut c_void,
                0x01, // non-zero = error, but not stall
            )
        };
        assert_eq!(status, efi::Status::DEVICE_ERROR);
        // Should have called async transfer once (cancel only; re-submit is via recovery timer).
        assert_eq!(mock_usb.async_call_count.get(), 1);
        // Should not have called control transfer (no stall).
        assert_eq!(mock_usb.control_call_count.get(), 0);
        core::mem::forget(device);
    }

    #[test]
    fn callback_stall_error_clears_halt_and_arms_timer() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        let device_ptr = &mut *device as *mut UsbHidDevice;
        // SAFETY: device_ptr is a valid UsbHidDevice.
        let status = unsafe {
            on_report_interrupt_complete(core::ptr::null_mut(), 0, device_ptr as *mut c_void, usb_io::ERR_STALL)
        };
        assert_eq!(status, efi::Status::DEVICE_ERROR);
        // Should have called control transfer to clear endpoint halt.
        assert_eq!(mock_usb.control_call_count.get(), 1);
        // Should have called async transfer once (cancel only; re-submit is via recovery timer).
        assert_eq!(mock_usb.async_call_count.get(), 1);
        core::mem::forget(device);
    }

    #[test]
    fn callback_data_length_exceeds_u16_max_returns_device_error() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        let device_ptr = &mut *device as *mut UsbHidDevice;
        // SAFETY: device_ptr is a valid UsbHidDevice.
        let status = unsafe {
            on_report_interrupt_complete(
                0x1000 as *mut c_void, // non-null
                u16::MAX as usize + 1,
                device_ptr as *mut c_void,
                usb_io::NOERROR,
            )
        };
        assert_eq!(status, efi::Status::DEVICE_ERROR);
        core::mem::forget(device);
    }

    #[test]
    fn callback_zero_length_returns_success() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        let device_ptr = &mut *device as *mut UsbHidDevice;
        // SAFETY: device_ptr is a valid UsbHidDevice.
        let status = unsafe {
            on_report_interrupt_complete(core::ptr::null_mut(), 0, device_ptr as *mut c_void, usb_io::NOERROR)
        };
        assert_eq!(status, efi::Status::SUCCESS);
        core::mem::forget(device);
    }

    #[test]
    fn callback_null_data_returns_success() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        let device_ptr = &mut *device as *mut UsbHidDevice;
        // SAFETY: device_ptr is a valid UsbHidDevice.
        let status = unsafe {
            on_report_interrupt_complete(core::ptr::null_mut(), 8, device_ptr as *mut c_void, usb_io::NOERROR)
        };
        assert_eq!(status, efi::Status::SUCCESS);
        core::mem::forget(device);
    }

    #[test]
    fn callback_no_registered_callback_returns_success() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        let device_ptr = &mut *device as *mut UsbHidDevice;
        let report = [0xAAu8; 4];
        // No callback registered — should silently succeed.
        // SAFETY: device_ptr is a valid UsbHidDevice.
        let status = unsafe {
            on_report_interrupt_complete(
                report.as_ptr() as *mut c_void,
                report.len(),
                device_ptr as *mut c_void,
                usb_io::NOERROR,
            )
        };
        assert_eq!(status, efi::Status::SUCCESS);
        core::mem::forget(device);
    }

    // Shared atomic counters for the test callback below.
    static CALLBACK_INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static CALLBACK_REPORT_SIZE: AtomicU16 = AtomicU16::new(0);

    unsafe extern "efiapi" fn counting_callback(
        report_buffer_size: u16,
        _report_buffer: *mut c_void,
        _context: *mut c_void,
    ) {
        CALLBACK_INVOCATIONS.fetch_add(1, Ordering::SeqCst);
        CALLBACK_REPORT_SIZE.store(report_buffer_size, Ordering::SeqCst);
    }

    #[test]
    fn callback_invokes_registered_callback_with_report() {
        CALLBACK_INVOCATIONS.store(0, Ordering::SeqCst);
        CALLBACK_REPORT_SIZE.store(0, Ordering::SeqCst);

        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        device.report_callback.callback = Some(counting_callback);
        let device_ptr = &mut *device as *mut UsbHidDevice;
        let report = [0x10u8, 0x20, 0x30];
        // SAFETY: device_ptr is a valid UsbHidDevice; report is valid.
        let status = unsafe {
            on_report_interrupt_complete(
                report.as_ptr() as *mut c_void,
                report.len(),
                device_ptr as *mut c_void,
                usb_io::NOERROR,
            )
        };
        assert_eq!(status, efi::Status::SUCCESS);
        assert_eq!(CALLBACK_INVOCATIONS.load(Ordering::SeqCst), 1);
        assert_eq!(CALLBACK_REPORT_SIZE.load(Ordering::SeqCst), 3);
        core::mem::forget(device);
    }

    // ---- Timer-based recovery tests ----

    fn sentinel_event() -> Event {
        Event::from_raw(0xBEEF as *mut c_void).unwrap()
    }

    /// Builds timer services that record each `set_timer` call into `calls`.
    fn recording_timer_services(calls: &'static AtomicUsize) -> Service<dyn TimerEventServices> {
        let mut mock = MockTimerEventServices::new();
        mock.expect_set_timer()
            .withf(|event, timer_type| *event == sentinel_event() && *timer_type == TimerType::Relative(RECOVERY_DELAY))
            .returning(move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        Service::mock(Box::new(mock))
    }

    fn make_device_with_timer(
        usb_io: &MockUsbIo,
        timer_services: Service<dyn TimerEventServices>,
    ) -> Box<UsbHidDevice> {
        let mut device = make_device(usb_io);
        device.timer_services = timer_services;
        device.recovery_event = Some(sentinel_event());
        device
    }

    #[test]
    fn callback_error_arms_recovery_timer_instead_of_immediate_resubmit() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let mut device = make_device_with_timer(&mock_usb, recording_timer_services(&CALLS));
        let device_ptr = &mut *device as *mut UsbHidDevice;
        // SAFETY: device_ptr is a valid UsbHidDevice.
        let status = unsafe { on_report_interrupt_complete(core::ptr::null_mut(), 0, device_ptr as *mut c_void, 0x01) };
        assert_eq!(status, efi::Status::DEVICE_ERROR);
        // Should have called async transfer once (cancel only, no immediate re-submit).
        assert_eq!(mock_usb.async_call_count.get(), 1);
        // Recovery timer should have been armed.
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        core::mem::forget(device);
    }

    #[test]
    fn callback_stall_error_with_timer_clears_halt_and_arms_timer() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let mut device = make_device_with_timer(&mock_usb, recording_timer_services(&CALLS));
        let device_ptr = &mut *device as *mut UsbHidDevice;
        // SAFETY: device_ptr is a valid UsbHidDevice.
        let status = unsafe {
            on_report_interrupt_complete(core::ptr::null_mut(), 0, device_ptr as *mut c_void, usb_io::ERR_STALL)
        };
        assert_eq!(status, efi::Status::DEVICE_ERROR);
        // Should have cleared endpoint halt via control transfer.
        assert_eq!(mock_usb.control_call_count.get(), 1);
        // Should have called async transfer once (cancel only).
        assert_eq!(mock_usb.async_call_count.get(), 1);
        // Recovery timer should have been armed.
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        core::mem::forget(device);
    }

    #[test]
    fn recovery_timer_does_not_resubmit_after_callback_unregistered() {
        let mock_usb = make_mock_usb_io(efi::Status::SUCCESS, efi::Status::SUCCESS);
        let mut device = make_device(&mock_usb);
        // No callback registered — simulates unregister having cleared it.
        assert!(device.report_callback.callback.is_none());
        let device_ptr = &mut *device as *mut UsbHidDevice;
        // Simulate the recovery timer firing.
        // SAFETY: device_ptr is a valid UsbHidDevice.
        unsafe { recovery_timer_notify(device_ptr) };
        // Should NOT have attempted to re-submit async transfers.
        assert_eq!(mock_usb.async_call_count.get(), 0);
        core::mem::forget(device);
    }
}
