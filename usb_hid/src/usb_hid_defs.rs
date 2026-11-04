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

use r_efi::efi::protocols::usb_io;

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
/// USB descriptor type for configuration.
pub const USB_DESC_TYPE_CONFIG: u8 = 0x02;
/// USB descriptor type for interface.
pub const USB_DESC_TYPE_INTERFACE: u8 = 0x04;

/// USB endpoint transfer type mask.
pub const USB_ENDPOINT_XFER_TYPE_MASK: u8 = 0x03;
/// USB endpoint type: interrupt.
pub const USB_ENDPOINT_INTERRUPT: u8 = 0x03;
/// USB endpoint direction: IN.
pub const USB_ENDPOINT_DIR_IN: u8 = 0x80;

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

/// Common header for USB descriptors.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct UsbDescHead {
    pub len: u8,
    pub desc_type: u8,
}

/// Returns a zeroed USB interface descriptor.
pub const fn empty_usb_interface_descriptor() -> usb_io::InterfaceDescriptor {
    usb_io::InterfaceDescriptor {
        length: 0,
        descriptor_type: 0,
        interface_number: 0,
        alternate_setting: 0,
        num_endpoints: 0,
        interface_class: 0,
        interface_sub_class: 0,
        interface_protocol: 0,
        interface: 0,
    }
}

/// Returns a zeroed USB endpoint descriptor.
pub const fn empty_usb_endpoint_descriptor() -> usb_io::EndpointDescriptor {
    usb_io::EndpointDescriptor {
        length: 0,
        descriptor_type: 0,
        endpoint_address: 0,
        attributes: 0,
        max_packet_size: 0,
        interval: 0,
    }
}

/// Returns a zeroed USB configuration descriptor.
pub const fn empty_usb_config_descriptor() -> usb_io::ConfigDescriptor {
    usb_io::ConfigDescriptor {
        length: 0,
        descriptor_type: 0,
        total_length: 0,
        num_interfaces: 0,
        configuration_value: 0,
        configuration: 0,
        attributes: 0,
        max_power: 0,
    }
}
