//! USB HID class-specific constants and descriptor structures.
//!
//! These definitions are specific to the USB HID class and are used by this
//! component to communicate with HID devices via the USB IO protocol.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!

/// USB interface class for HID devices.
pub const CLASS_HID: u8 = 3;
/// USB interface subclass for boot devices.
pub const SUBCLASS_BOOT: u8 = 1;

/// HID report protocol mode.
pub const REPORT_PROTOCOL: u8 = 1;

/// USB descriptor type for HID.
pub const USB_DESC_TYPE_HID: u8 = 0x21;
/// USB descriptor type for HID report.
pub const USB_DESC_TYPE_REPORT: u8 = 0x22;

/// USB HID class-specific request: GET_REPORT.
pub const USB_HID_GET_REPORT_REQUEST: u8 = 0x01;
/// USB HID class-specific request: SET_REPORT.
pub const USB_HID_SET_REPORT_REQUEST: u8 = 0x09;
/// USB HID class-specific request: SET_PROTOCOL.
pub const USB_HID_SET_PROTOCOL_REQUEST: u8 = 0x0B;

/// USB request type: class, interface, host-to-device.
pub const USB_REQ_TYPE_CLASS_INTERFACE_OUT: u8 = 0x21;
/// USB request type: class, interface, device-to-host.
pub const USB_REQ_TYPE_CLASS_INTERFACE_IN: u8 = 0xA1;
/// USB request type: standard, endpoint, host-to-device.
pub const USB_REQ_TYPE_STANDARD_ENDPOINT_OUT: u8 = 0x02;
/// USB request type: standard, device, device-to-host.
pub const USB_REQ_TYPE_STANDARD_DEVICE_IN: u8 = 0x80;

/// USB standard request: CLEAR_FEATURE.
pub const USB_REQ_CLEAR_FEATURE: u8 = 0x01;
/// USB feature selector: ENDPOINT_HALT.
pub const USB_FEATURE_ENDPOINT_HALT: u16 = 0;

/// USB standard request: GET_DESCRIPTOR.
pub const USB_REQ_GET_DESCRIPTOR: u8 = 0x06;

/// Timeout for USB control transfers (in milliseconds).
pub const USB_TRANSFER_TIMEOUT_MS: u32 = 3000;

/// HID class descriptor entry (type + length pair).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C, packed)]
pub struct HidClassDescriptor {
    pub descriptor_type: u8,
    pub descriptor_length: u16,
}

/// USB HID descriptor.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct EfiUsbHidDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub bcd_hid: u16,
    pub country_code: u8,
    pub num_descriptors: u8,
    // Followed by variable-length array of HidClassDescriptor.
}
