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

use r_efi::{efi, efi::protocols::usb_io};

/// Creates a stub USB IO protocol with panicking function pointers.
///
/// Callers should replace the function pointers they need before use.
#[coverage(off)]
pub fn usb_io_stub() -> usb_io::Protocol {
    unsafe extern "efiapi" fn stub_control_transfer(
        _this: *mut usb_io::Protocol,
        _request: *mut usb_io::DeviceRequest,
        _direction: usb_io::DataDirection,
        _timeout: u32,
        _data: *mut c_void,
        _data_length: usize,
        _status: *mut u32,
    ) -> efi::Status {
        panic!("unexpected call to control_transfer")
    }
    unsafe extern "efiapi" fn stub_bulk_transfer(
        _this: *mut usb_io::Protocol,
        _device_endpoint: u8,
        _data: *mut c_void,
        _data_length: *mut usize,
        _timeout: usize,
        _status: *mut u32,
    ) -> efi::Status {
        panic!("unexpected call to usb_bulk_transfer")
    }
    unsafe extern "efiapi" fn stub_async_interrupt_transfer(
        _this: *mut usb_io::Protocol,
        _device_endpoint: u8,
        _is_new_transfer: efi::Boolean,
        _polling_interval: usize,
        _data_length: usize,
        _callback: Option<usb_io::AsyncUsbTransferCallback>,
        _context: *mut c_void,
    ) -> efi::Status {
        panic!("unexpected call to async_interrupt_transfer")
    }
    unsafe extern "efiapi" fn stub_sync_interrupt_transfer(
        _this: *mut usb_io::Protocol,
        _device_endpoint: u8,
        _data: *mut c_void,
        _data_length: *mut usize,
        _timeout: usize,
        _status: *mut u32,
    ) -> efi::Status {
        panic!("unexpected call to usb_sync_interrupt_transfer")
    }
    unsafe extern "efiapi" fn stub_isochronous_transfer(
        _this: *mut usb_io::Protocol,
        _device_endpoint: u8,
        _data: *mut c_void,
        _data_length: usize,
        _status: *mut u32,
    ) -> efi::Status {
        panic!("unexpected call to usb_isochronous_transfer")
    }
    unsafe extern "efiapi" fn stub_async_isochronous_transfer(
        _this: *mut usb_io::Protocol,
        _device_endpoint: u8,
        _data: *mut c_void,
        _data_length: usize,
        _isochronous_callback: usb_io::AsyncUsbTransferCallback,
        _context: *mut c_void,
    ) -> efi::Status {
        panic!("unexpected call to usb_async_isochronous_transfer")
    }
    unsafe extern "efiapi" fn stub_get_device_descriptor(
        _this: *mut usb_io::Protocol,
        _device_descriptor: *mut usb_io::DeviceDescriptor,
    ) -> efi::Status {
        panic!("unexpected call to get_device_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_config_descriptor(
        _this: *mut usb_io::Protocol,
        _config_descriptor: *mut usb_io::ConfigDescriptor,
    ) -> efi::Status {
        panic!("unexpected call to get_config_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_interface_descriptor(
        _this: *mut usb_io::Protocol,
        _interface_descriptor: *mut usb_io::InterfaceDescriptor,
    ) -> efi::Status {
        panic!("unexpected call to get_interface_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_endpoint_descriptor(
        _this: *mut usb_io::Protocol,
        _endpoint_index: u8,
        _endpoint_descriptor: *mut usb_io::EndpointDescriptor,
    ) -> efi::Status {
        panic!("unexpected call to get_endpoint_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_string_descriptor(
        _this: *mut usb_io::Protocol,
        _lang_id: u16,
        _string_id: u8,
        _string: *mut *mut u16,
    ) -> efi::Status {
        panic!("unexpected call to get_string_descriptor")
    }
    unsafe extern "efiapi" fn stub_get_supported_languages(
        _this: *mut usb_io::Protocol,
        _lang_id_table: *mut *mut u16,
        _table_size: *mut u16,
    ) -> efi::Status {
        panic!("unexpected call to get_supported_languages")
    }
    unsafe extern "efiapi" fn stub_port_reset(_this: *mut usb_io::Protocol) -> efi::Status {
        panic!("unexpected call to port_reset")
    }
    usb_io::Protocol {
        control_transfer: stub_control_transfer,
        bulk_transfer: stub_bulk_transfer,
        async_interrupt_transfer: stub_async_interrupt_transfer,
        sync_interrupt_transfer: stub_sync_interrupt_transfer,
        isochronous_transfer: stub_isochronous_transfer,
        async_isochronous_transfer: stub_async_isochronous_transfer,
        get_device_descriptor: stub_get_device_descriptor,
        get_config_descriptor: stub_get_config_descriptor,
        get_interface_descriptor: stub_get_interface_descriptor,
        get_endpoint_descriptor: stub_get_endpoint_descriptor,
        get_string_descriptor: stub_get_string_descriptor,
        get_supported_languages: stub_get_supported_languages,
        port_reset: stub_port_reset,
    }
}
