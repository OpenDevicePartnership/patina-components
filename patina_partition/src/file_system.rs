//! File reads via `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
extern crate alloc;

use alloc::vec::Vec;
use core::ptr;

use patina::{
    boot_services::BootServices,
    device_path::paths::DevicePathBuf,
    error::{EfiError, Result},
};
use r_efi::{
    efi,
    protocols::{file, simple_file_system},
};

/// Read the entire contents of `file_path` from the partition addressed by `device_path`.
///
/// Resolves the partition handle via `locate_device_path` against `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`,
/// opens the volume, opens the file at `file_path` for reading, slurps the file into a `Vec<u8>`,
/// and closes the handles before returning.
///
/// `file_path` is interpreted as a partition-relative UEFI file path (e.g. `\EFI\BOOT\BOOTX64.EFI`).
/// It is converted to UCS-2 with a trailing null and passed to the protocol's open call.
///
/// # Arguments
///
/// * `boot_services` - Boot services interface
/// * `device_path` - Device path resolving to a partition exposing SimpleFileSystem
/// * `file_path` - Partition-relative file path
///
/// # Returns
///
/// Returns `Ok(Vec<u8>)` containing the file contents on success. Returns an error if the path
/// doesn't resolve to a SimpleFileSystem handle, the volume can't be opened, the file can't be
/// opened, or any read fails.
pub fn read_partition_file<B: BootServices>(
    boot_services: &B,
    device_path: &DevicePathBuf,
    file_path: &str,
) -> Result<Vec<u8>> {
    let mut path_ptr = device_path.as_ref() as *const _ as *mut efi::protocols::device_path::Protocol;

    // SAFETY: path_ptr points into a valid DevicePathBuf for the duration of this call.
    let handle = unsafe { boot_services.locate_device_path(&simple_file_system::PROTOCOL_GUID, &mut path_ptr) }
        .map_err(EfiError::from)?;

    // SAFETY: handle was returned by locate_device_path for the SimpleFileSystem GUID.
    let fs = unsafe { boot_services.handle_protocol::<simple_file_system::Protocol>(handle).map_err(EfiError::from)? };

    let path_utf16 = encode_ucs2_null_terminated(file_path);

    // SAFETY: fs is valid for the lifetime of the call; path_utf16 is owned by us and outlives the call.
    unsafe { read_partition_file_inner(fs, &path_utf16) }
}

/// Encode a Rust `&str` as a null-terminated UCS-2 buffer suitable for UEFI file-protocol calls.
fn encode_ucs2_null_terminated(s: &str) -> Vec<u16> {
    let mut out: Vec<u16> = s.encode_utf16().collect();
    out.push(0);
    out
}

/// Open the volume, open the file, slurp it into a `Vec<u8>`, and clean up.
///
/// Separated from `read_partition_file` because it dereferences the protocol's function-pointer
/// fields directly. Tests use mock function pointers to exercise the dispatch path.
///
/// # Safety
///
/// `fs` must remain valid for the duration of the call. `path_utf16` must be a null-terminated
/// UCS-2 buffer.
unsafe fn read_partition_file_inner(fs: &mut simple_file_system::Protocol, path_utf16: &[u16]) -> Result<Vec<u8>> {
    let mut root: *mut file::Protocol = ptr::null_mut();
    // SAFETY: fs is valid; root pointer is written by the protocol on success.
    let status = (fs.open_volume)(fs, &mut root);
    if status != efi::Status::SUCCESS {
        return Err(EfiError::from(status));
    }

    let mut handle: *mut file::Protocol = ptr::null_mut();
    // SAFETY: root is valid from open_volume; path is null-terminated.
    let status = unsafe { ((*root).open)(root, &mut handle, path_utf16.as_ptr() as *mut u16, file::MODE_READ, 0) };
    if status != efi::Status::SUCCESS {
        // SAFETY: root is valid; close consumes it.
        let _ = unsafe { ((*root).close)(root) };
        return Err(EfiError::from(status));
    }

    // Helper closure: close both handles, used on every exit path.
    // SAFETY: handle and root are both valid for the lifetime of this fn; close consumes them.
    let close_both = || unsafe {
        let _ = ((*handle).close)(handle);
        let _ = ((*root).close)(root);
    };

    // Determine file size by seeking to end + reading position.
    // SAFETY: handle is valid from open.
    let status = unsafe { ((*handle).set_position)(handle, u64::MAX) };
    if status != efi::Status::SUCCESS {
        close_both();
        return Err(EfiError::from(status));
    }

    let mut size: u64 = 0;
    // SAFETY: handle is valid from open.
    let status = unsafe { ((*handle).get_position)(handle, &mut size) };
    if status != efi::Status::SUCCESS {
        close_both();
        return Err(EfiError::from(status));
    }

    // Reset position to start of file before reading.
    // SAFETY: handle is valid from open.
    let status = unsafe { ((*handle).set_position)(handle, 0) };
    if status != efi::Status::SUCCESS {
        close_both();
        return Err(EfiError::from(status));
    }

    let size_usize = size as usize;
    let mut buffer: Vec<u8> = alloc::vec![0u8; size_usize];

    let mut buffer_size = size_usize;
    // SAFETY: handle is valid; buffer outlives the call.
    let status = unsafe { ((*handle).read)(handle, &mut buffer_size, buffer.as_mut_ptr() as *mut core::ffi::c_void) };
    if status != efi::Status::SUCCESS {
        close_both();
        return Err(EfiError::from(status));
    }

    buffer.truncate(buffer_size);

    close_both();

    Ok(buffer)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use patina::{
        boot_services::MockBootServices,
        device_path::{node_defs::EndEntire, paths::DevicePathBuf},
    };

    use super::*;

    fn create_test_device_path() -> DevicePathBuf {
        DevicePathBuf::from_device_path_node_iter(core::iter::once(EndEntire))
    }

    #[test]
    fn test_read_partition_file_locate_failure() {
        let device_path = create_test_device_path();
        let mut mock = MockBootServices::new();

        mock.expect_locate_device_path().returning(|_, _| Err(efi::Status::NOT_FOUND));

        let result = read_partition_file(&mock, &device_path, r"\EFI\BOOT\BOOTX64.EFI");
        assert!(result.is_err(), "missing SimpleFileSystem on path must surface as Err");
    }

    #[test]
    fn test_read_partition_file_handle_protocol_failure() {
        let device_path = create_test_device_path();
        let mut mock = MockBootServices::new();

        let handle_addr: usize = 1;
        mock.expect_locate_device_path().returning(move |_, _| Ok(handle_addr as efi::Handle));
        mock.expect_handle_protocol::<simple_file_system::Protocol>().returning(|_| Err(efi::Status::UNSUPPORTED));

        let result = read_partition_file(&mock, &device_path, r"\EFI\BOOT\BOOTX64.EFI");
        assert!(result.is_err(), "missing SimpleFileSystem interface on located handle must surface as Err");
    }

    #[test]
    fn test_encode_ucs2_null_terminated_basic() {
        let encoded = encode_ucs2_null_terminated("AB");
        assert_eq!(encoded, alloc::vec![b'A' as u16, b'B' as u16, 0]);
    }

    #[test]
    fn test_encode_ucs2_null_terminated_path() {
        // \EFI\BOOT\BOOTX64.EFI — backslash 0x5C, characters as ASCII, trailing null.
        let encoded = encode_ucs2_null_terminated(r"\EFI\BOOT\BOOTX64.EFI");
        assert_eq!(encoded.len(), 22, "21 chars + null terminator");
        assert_eq!(encoded[0], 0x5C, "starts with backslash");
        assert_eq!(*encoded.last().unwrap(), 0, "null-terminated");
    }

    #[test]
    fn test_encode_ucs2_null_terminated_empty() {
        let encoded = encode_ucs2_null_terminated("");
        assert_eq!(encoded, alloc::vec![0u16]);
    }
}
