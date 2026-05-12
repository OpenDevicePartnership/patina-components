//! RAM disk install helper for Patina firmware.
//!
//! Thin wrapper around the UEFI RAM Disk Protocol binding in
//! [`patina::uefi_protocol::ram_disk`]: takes a byte buffer, publishes it as a virtual block
//! device the firmware can boot from, and returns the resulting device path.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use core::ptr;

use patina::{
    boot_services::BootServices,
    device_path::paths::{DevicePath, DevicePathBuf},
    error::{EfiError, Result},
    uefi_protocol::ram_disk::{Protocol, RAM_DISK_VIRTUAL_DISK_GUID},
};
use r_efi::efi;

/// Install `bytes` as a virtual block device using the UEFI RAM Disk Protocol.
///
/// Leaks `bytes` into a stable heap allocation, locates the RAM Disk protocol, and asks the
/// firmware to register the buffer as a virtual disk of type `RAM_DISK_VIRTUAL_DISK_GUID`. The
/// returned device path can be passed to consumers of UEFI boot device paths.
///
/// The backing memory lives for the rest of the firmware's lifetime — there is no `Drop` impl
/// that frees it. The caller may later invoke the protocol's `unregister` to remove the RAM
/// disk's device-path entry, but the memory itself stays leaked.
///
/// # Arguments
///
/// * `boot_services` - Boot services interface
/// * `bytes` - Bytes to publish as the virtual block device's contents
///
/// # Returns
///
/// Returns `Ok(DevicePathBuf)` containing an owned copy of the device path the firmware assigns
/// to the new RAM disk. Returns an error if the RAM Disk protocol is not present, or the
/// firmware rejects the register call.
pub fn install<B: BootServices>(boot_services: &B, bytes: &[u8]) -> Result<DevicePathBuf> {
    let buffer: Box<[u8]> = bytes.into();
    let leaked: &'static mut [u8] = Box::leak(buffer);
    let base = leaked.as_ptr() as u64;
    let size = leaked.len() as u64;

    // SAFETY: locate_protocol with None returns a static mut reference for the registered
    // protocol; we treat it as &mut for the single register call below.
    let proto = unsafe { boot_services.locate_protocol::<Protocol>(None) }.map_err(EfiError::from)?;

    let mut type_guid = RAM_DISK_VIRTUAL_DISK_GUID;
    let mut device_path_out: *mut efi::protocols::device_path::Protocol = ptr::null_mut();

    let status = (proto.register)(base, size, &mut type_guid, ptr::null_mut(), &mut device_path_out);
    if status != efi::Status::SUCCESS {
        return Err(EfiError::from(status));
    }

    if device_path_out.is_null() {
        return Err(EfiError::DeviceError);
    }

    // SAFETY: device_path_out was just written by the firmware and is null-terminated per spec.
    let dp_ref =
        unsafe { DevicePath::try_from_ptr(device_path_out as *const u8) }.map_err(|_| EfiError::DeviceError)?;
    Ok(DevicePathBuf::from(dp_ref))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::sync::atomic::{AtomicU64, Ordering};

    use patina::{
        boot_services::MockBootServices,
        uefi_protocol::ram_disk::{Protocol, RAM_DISK_VIRTUAL_DISK_GUID, RegisterFn},
    };

    use super::*;

    static CAPTURED_REGISTER_BASE: AtomicU64 = AtomicU64::new(0);
    static CAPTURED_REGISTER_SIZE: AtomicU64 = AtomicU64::new(0);

    /// Static EndEntire-only device path bytes (type=0x7F, subtype=0xFF, length=4), used as
    /// the return value of mock register fns.
    static MOCK_REGISTER_DEVICE_PATH: [u8; 4] = [0x7F, 0xFF, 0x04, 0x00];

    extern "efiapi" fn mock_register_returns_path(
        ram_disk_base: u64,
        ram_disk_size: u64,
        _ram_disk_type: *mut efi::Guid,
        _parent_device_path: *mut efi::protocols::device_path::Protocol,
        device_path: *mut *mut efi::protocols::device_path::Protocol,
    ) -> efi::Status {
        CAPTURED_REGISTER_BASE.store(ram_disk_base, Ordering::SeqCst);
        CAPTURED_REGISTER_SIZE.store(ram_disk_size, Ordering::SeqCst);
        // SAFETY: caller-provided out-ptr is valid; static path lives forever.
        unsafe {
            *device_path = MOCK_REGISTER_DEVICE_PATH.as_ptr() as *mut efi::protocols::device_path::Protocol;
        }
        efi::Status::SUCCESS
    }

    extern "efiapi" fn mock_register_returns_error(
        _ram_disk_base: u64,
        _ram_disk_size: u64,
        _ram_disk_type: *mut efi::Guid,
        _parent_device_path: *mut efi::protocols::device_path::Protocol,
        _device_path: *mut *mut efi::protocols::device_path::Protocol,
    ) -> efi::Status {
        efi::Status::OUT_OF_RESOURCES
    }

    extern "efiapi" fn mock_unregister(_device_path: *mut efi::protocols::device_path::Protocol) -> efi::Status {
        efi::Status::SUCCESS
    }

    fn leaked_ram_disk_protocol(register: RegisterFn) -> &'static mut Protocol {
        Box::leak(Box::new(Protocol { register, unregister: mock_unregister }))
    }

    #[test]
    fn install_locate_failure() {
        let mut mock = MockBootServices::new();
        mock.expect_locate_protocol::<Protocol>().returning(|_| Err(efi::Status::NOT_FOUND));

        let result = install(&mock, &[0xAB; 16]);
        assert!(result.is_err(), "missing RAM Disk Protocol must surface as Err");
    }

    #[test]
    fn install_register_failure_propagates() {
        let mut mock = MockBootServices::new();
        mock.expect_locate_protocol::<Protocol>()
            .returning(|_| Ok(leaked_ram_disk_protocol(mock_register_returns_error)));

        let result = install(&mock, &[0xAB; 16]);
        assert!(result.is_err(), "register OUT_OF_RESOURCES must surface as Err");
    }

    #[test]
    fn install_forwards_base_and_size_to_register() {
        let mut mock = MockBootServices::new();
        mock.expect_locate_protocol::<Protocol>()
            .returning(|_| Ok(leaked_ram_disk_protocol(mock_register_returns_path)));

        let payload = [0xAB; 256];
        let _result = install(&mock, &payload).expect("register should succeed");

        assert!(CAPTURED_REGISTER_BASE.load(Ordering::SeqCst) != 0, "register received non-zero heap base");
        assert_eq!(CAPTURED_REGISTER_SIZE.load(Ordering::SeqCst), 256, "register received buffer length");
        // Reference RAM_DISK_VIRTUAL_DISK_GUID to ensure the import is exercised; the install
        // function uses it internally as the register call's type GUID.
        let _ = RAM_DISK_VIRTUAL_DISK_GUID;
    }
}
