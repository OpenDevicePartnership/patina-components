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
#[coverage(off)]
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
        _callback: HidIoReportCallback,
        _context: *mut c_void,
    ) -> efi::Status {
        efi::Status::SUCCESS
    }
    unsafe extern "efiapi" fn unregister_report_callback(
        _this: *const HidIoProtocol,
        _callback: HidIoReportCallback,
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
