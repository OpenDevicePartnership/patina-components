//! Test stubs for protocol types whose `stub()` methods are not exposed
//! from the upstream patina crate.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!

use core::ffi::c_void;

use r_efi::efi;

use patina::uefi_protocol::usb_io::{EfiAsyncUsbTransferCallback, EfiUsbIoProtocol, types::*};

/// Creates a stub `EfiUsbIoProtocol` with panicking function pointers.
///
/// Callers should replace the function pointers they need before use.
#[coverage(off)]
pub fn usb_io_stub() -> EfiUsbIoProtocol {
    unsafe extern "efiapi" fn stub_control_transfer(
        _this: *const EfiUsbIoProtocol,
        _request: *const EfiUsbDeviceRequest,
        _direction: EfiUsbDataDirection,
        _timeout: u32,
        _data: *mut c_void,
        _data_length: usize,
        _status: *mut u32,
    ) -> efi::Status {
        panic!("unexpected call to usb_control_transfer")
    }
    unsafe extern "efiapi" fn stub_bulk_transfer(
        _this: *const EfiUsbIoProtocol,
        _device_endpoint: u8,
        _data: *mut c_void,
        _data_length: *mut usize,
        _timeout: usize,
        _status: *mut u32,
    ) -> efi::Status {
        panic!("unexpected call to usb_bulk_transfer")
    }
    unsafe extern "efiapi" fn stub_async_interrupt_transfer(
        _this: *const EfiUsbIoProtocol,
        _device_endpoint: u8,
        _is_new_transfer: efi::Boolean,
        _polling_interval: usize,
        _data_length: usize,
        _callback: Option<EfiAsyncUsbTransferCallback>,
        _context: *mut c_void,
    ) -> efi::Status {
        panic!("unexpected call to usb_async_interrupt_transfer")
    }
    unsafe extern "efiapi" fn stub_sync_interrupt_transfer(
        _this: *const EfiUsbIoProtocol,
        _device_endpoint: u8,
        _data: *mut c_void,
        _data_length: *mut usize,
        _timeout: usize,
        _status: *mut u32,
    ) -> efi::Status {
        panic!("unexpected call to usb_sync_interrupt_transfer")
    }
    unsafe extern "efiapi" fn stub_isochronous_transfer(
        _this: *const EfiUsbIoProtocol,
        _device_endpoint: u8,
        _data: *mut c_void,
        _data_length: usize,
        _status: *mut u32,
    ) -> efi::Status {
        panic!("unexpected call to usb_isochronous_transfer")
    }
    unsafe extern "efiapi" fn stub_async_isochronous_transfer(
        _this: *const EfiUsbIoProtocol,
        _device_endpoint: u8,
        _data: *mut c_void,
        _data_length: usize,
        _isochronous_callback: EfiAsyncUsbTransferCallback,
        _context: *mut c_void,
    ) -> efi::Status {
        panic!("unexpected call to usb_async_isochronous_transfer")
    }
    unsafe extern "efiapi" fn stub_get_device_descriptor(
        _this: *const EfiUsbIoProtocol,
        _device_descriptor: *mut EfiUsbDeviceDescriptor,
    ) -> efi::Status {
        panic!("unexpected call to usb_get_device_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_config_descriptor(
        _this: *const EfiUsbIoProtocol,
        _config_descriptor: *mut EfiUsbConfigDescriptor,
    ) -> efi::Status {
        panic!("unexpected call to usb_get_config_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_interface_descriptor(
        _this: *const EfiUsbIoProtocol,
        _interface_descriptor: *mut EfiUsbInterfaceDescriptor,
    ) -> efi::Status {
        panic!("unexpected call to usb_get_interface_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_endpoint_descriptor(
        _this: *const EfiUsbIoProtocol,
        _endpoint_index: u8,
        _endpoint_descriptor: *mut EfiUsbEndpointDescriptor,
    ) -> efi::Status {
        panic!("unexpected call to usb_get_endpoint_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_string_descriptor(
        _this: *const EfiUsbIoProtocol,
        _lang_id: u16,
        _string_id: u8,
        _string: *mut *mut u16,
    ) -> efi::Status {
        panic!("unexpected call to usb_get_string_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_supported_languages(
        _this: *const EfiUsbIoProtocol,
        _lang_id_table: *mut *mut u16,
        _table_size: *mut u16,
    ) -> efi::Status {
        panic!("unexpected call to usb_get_supported_languages")
    }
    unsafe extern "efiapi" fn stub_port_reset(_this: *const EfiUsbIoProtocol) -> efi::Status {
        panic!("unexpected call to usb_port_reset")
    }
    EfiUsbIoProtocol {
        usb_control_transfer: stub_control_transfer,
        usb_bulk_transfer: stub_bulk_transfer,
        usb_async_interrupt_transfer: stub_async_interrupt_transfer,
        usb_sync_interrupt_transfer: stub_sync_interrupt_transfer,
        usb_isochronous_transfer: stub_isochronous_transfer,
        usb_async_isochronous_transfer: stub_async_isochronous_transfer,
        usb_get_device_descriptor: stub_get_device_descriptor,
        usb_get_config_descriptor: stub_get_config_descriptor,
        usb_get_interface_descriptor: stub_get_interface_descriptor,
        usb_get_endpoint_descriptor: stub_get_endpoint_descriptor,
        usb_get_string_descriptor: stub_get_string_descriptor,
        usb_get_supported_languages: stub_get_supported_languages,
        usb_port_reset: stub_port_reset,
    }
}
