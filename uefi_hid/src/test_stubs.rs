//! Test stubs for protocol types whose `stub()` methods are not exposed
//! from the upstream patina crate (they are `#[cfg(test)]` internal to patina).
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!

use alloc::boxed::Box;
use core::ffi::c_void;

use r_efi::efi;

use patina::vendor_protocols::hid_io::{HidIoProtocol, HidIoReportCallback, HidReportType};

/// Creates a stub `HidIoProtocol` with no-op function pointers for testing.
#[cfg_attr(coverage, coverage(off))]
pub fn hid_io_stub() -> &'static mut HidIoProtocol {
    unsafe extern "efiapi" fn get_report_descriptor(
        _this: *const HidIoProtocol,
        report_descriptor_size: *mut usize,
        _report_descriptor_buffer: *mut c_void,
    ) -> efi::Status {
        // SAFETY: report_descriptor_size is a valid pointer provided by the caller in the test stub.
        unsafe { *report_descriptor_size = 0 };
        efi::Status::BUFFER_TOO_SMALL
    }
    unsafe extern "efiapi" fn get_report(
        _this: *const HidIoProtocol,
        _report_id: u8,
        _report_type: HidReportType,
        _report_buffer_size: usize,
        _report_buffer: *mut c_void,
    ) -> efi::Status {
        efi::Status::SUCCESS
    }
    unsafe extern "efiapi" fn set_report(
        _this: *const HidIoProtocol,
        _report_id: u8,
        _report_type: HidReportType,
        _report_buffer_size: usize,
        _report_buffer: *mut c_void,
    ) -> efi::Status {
        efi::Status::SUCCESS
    }
    unsafe extern "efiapi" fn register_report_callback(
        _this: *const HidIoProtocol,
        _callback: Option<HidIoReportCallback>,
        _context: *mut c_void,
    ) -> efi::Status {
        efi::Status::SUCCESS
    }
    unsafe extern "efiapi" fn unregister_report_callback(
        _this: *const HidIoProtocol,
        _callback: Option<HidIoReportCallback>,
    ) -> efi::Status {
        efi::Status::SUCCESS
    }

    let protocol = HidIoProtocol {
        get_report_descriptor,
        get_report,
        set_report,
        register_report_callback,
        unregister_report_callback,
    };
    // SAFETY: Leaked for 'static lifetime in tests.
    unsafe { Box::into_raw(Box::new(protocol)).as_mut().unwrap() }
}

use patina::component::service::{
    Service,
    uefi_services::{
        event::{Event, MockEventServices},
        handle::Handle,
        protocol::MockProtocolServices,
        timer_event::MockTimerEventServices,
        tpl::{MockTplServices, PreviousTpl},
    },
};

use crate::HidServices;

/// Returns a distinct, non-null sentinel event for tests.
pub fn test_event(value: usize) -> Event {
    Event::from_raw(value as *mut c_void).unwrap()
}

/// Returns a non-null handle for tests.
pub fn test_handle(value: usize) -> Handle {
    Handle::from_raw(value as efi::Handle).unwrap()
}

/// Builds a [`HidServices`] backed by leaked mocks.
///
/// `setup` runs first so its expectations take precedence (mockall matches in FIFO order). Permissive defaults are
/// then added for event, timer, and TPL operations. Protocol services have no defaults; any unexpected protocol call
/// panics.
#[cfg_attr(coverage, coverage(off))]
pub fn mock_services(
    setup: impl FnOnce(&mut MockProtocolServices, &mut MockEventServices, &mut MockTimerEventServices),
) -> HidServices {
    let mut protocols = MockProtocolServices::new();
    let mut events = MockEventServices::new();
    let mut timers = MockTimerEventServices::new();
    setup(&mut protocols, &mut events, &mut timers);

    events.expect_create_event().returning(|_, _| Ok(test_event(0xE0E)));
    events.expect_create_wait_event().returning(|_, _| Ok(test_event(0xE4E)));
    events.expect_create_event_for_group().returning(|_, _, _| Ok(test_event(0xE1E)));
    events.expect_signal_event().returning(|_| Ok(()));
    events.expect_check_event().returning(|_| Ok(false));
    events.expect_close_event().returning(|_| Ok(()));
    timers.expect_create_timer_event().returning(|_, _| Ok(test_event(0xE2E)));
    timers.expect_create_timer_event_no_notify().returning(|| Ok(test_event(0xE3E)));
    timers.expect_set_timer().returning(|_, _| Ok(()));

    let mut tpl = MockTplServices::new();
    tpl.expect_raise_tpl().returning(|_| PreviousTpl::from_raw(4));
    tpl.expect_restore_tpl().returning(|_| ());

    HidServices {
        protocols: Service::mock(Box::new(protocols)),
        events: Service::mock(Box::new(events)),
        timers: Service::mock(Box::new(timers)),
        tpl: Service::mock(Box::new(tpl)),
    }
}
