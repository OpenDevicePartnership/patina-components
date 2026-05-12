//! Raw block-level writes via `EFI_BLOCK_IO_PROTOCOL`.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
extern crate alloc;

use alloc::vec::Vec;

use patina::{
    boot_services::BootServices,
    device_path::paths::DevicePathBuf,
    error::{EfiError, Result},
};
use r_efi::{efi, protocols::block_io};

/// Write `data` to the partition addressed by `device_path` starting at LBA 0.
///
/// Resolves the partition handle via `locate_device_path` against `EFI_BLOCK_IO_PROTOCOL`,
/// then issues `WriteBlocks` followed by `FlushBlocks`. If `data.len()` is not a multiple of
/// the media block size, the trailing partial block is zero-padded.
///
/// Empty `data` is a no-op (returns `Ok(())` without touching the device).
///
/// # Arguments
///
/// * `boot_services` - Boot services interface
/// * `device_path` - Device path resolving to a partition or block device exposing BlockIo
/// * `data` - Bytes to write at LBA 0; must fit within the partition (`data.len() <= (last_block + 1) * block_size`)
///
/// # Returns
///
/// Returns `Ok(())` once `WriteBlocks` and `FlushBlocks` both succeed. Returns an error if
/// the device path doesn't resolve to a BlockIo handle, the media is read-only or absent, or
/// either of the write/flush calls fails.
pub fn write_partition_raw<B: BootServices>(boot_services: &B, device_path: &DevicePathBuf, data: &[u8]) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    let mut path_ptr = device_path.as_ref() as *const _ as *mut efi::protocols::device_path::Protocol;

    // SAFETY: path_ptr points into a valid DevicePathBuf for the duration of this call.
    let handle =
        unsafe { boot_services.locate_device_path(&block_io::PROTOCOL_GUID, &mut path_ptr) }.map_err(EfiError::from)?;

    // SAFETY: handle was returned by locate_device_path for the BlockIo GUID.
    let protocol = unsafe { boot_services.handle_protocol::<block_io::Protocol>(handle).map_err(EfiError::from)? };

    // SAFETY: `protocol` is a valid reference returned by handle_protocol; lifetime is bound by the controller.
    unsafe { write_partition_raw_inner(protocol, data) }
}

/// Issue `WriteBlocks` + `FlushBlocks` against the supplied BlockIo protocol.
///
/// Separated from `write_partition_raw` because it dereferences the protocol's `media` raw
/// pointer and invokes function pointers directly. Tests use mock function pointers to
/// exercise the dispatch path.
///
/// # Safety
///
/// `protocol.media` must point to a valid `Media` instance owned by the controller, and the
/// `write_blocks`/`flush_blocks` function pointers must remain valid for the duration of the call.
unsafe fn write_partition_raw_inner(protocol: &mut block_io::Protocol, data: &[u8]) -> Result<()> {
    // SAFETY: protocol.media is set by the controller when BlockIo is published; non-null per spec.
    let media = unsafe { &*protocol.media };

    if !media.media_present {
        return Err(EfiError::from(efi::Status::NO_MEDIA));
    }
    if media.read_only {
        return Err(EfiError::from(efi::Status::WRITE_PROTECTED));
    }

    let block_size = media.block_size as usize;
    if block_size == 0 {
        return Err(EfiError::from(efi::Status::DEVICE_ERROR));
    }

    let aligned_size = data.len().div_ceil(block_size) * block_size;
    let mut buffer = Vec::with_capacity(aligned_size);
    buffer.extend_from_slice(data);
    buffer.resize(aligned_size, 0);

    let media_id = media.media_id;
    let write_blocks = protocol.write_blocks;
    let flush_blocks = protocol.flush_blocks;

    let status = write_blocks(protocol, media_id, 0, aligned_size, buffer.as_mut_ptr() as *mut core::ffi::c_void);
    if status != efi::Status::SUCCESS {
        return Err(EfiError::from(status));
    }

    let status = flush_blocks(protocol);
    if status != efi::Status::SUCCESS {
        return Err(EfiError::from(status));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use patina::{
        boot_services::MockBootServices,
        device_path::{node_defs::EndEntire, paths::DevicePathBuf},
    };

    use super::*;

    fn create_test_device_path() -> DevicePathBuf {
        DevicePathBuf::from_device_path_node_iter(core::iter::once(EndEntire))
    }

    #[test]
    fn test_write_partition_raw_empty_data_is_noop() {
        let device_path = create_test_device_path();
        // No mock expectations: function must short-circuit before touching boot_services.
        let mock = MockBootServices::new();

        let result = write_partition_raw(&mock, &device_path, &[]);
        assert!(result.is_ok(), "empty data must be a no-op");
    }

    #[test]
    fn test_write_partition_raw_locate_failure() {
        let device_path = create_test_device_path();
        let mut mock = MockBootServices::new();

        mock.expect_locate_device_path().returning(|_, _| Err(efi::Status::NOT_FOUND));

        let result = write_partition_raw(&mock, &device_path, &[0xAB; 16]);
        assert!(result.is_err(), "missing BlockIo on path must surface as Err");
    }

    #[test]
    fn test_write_partition_raw_handle_protocol_failure() {
        let device_path = create_test_device_path();
        let mut mock = MockBootServices::new();

        let handle_addr: usize = 1;
        mock.expect_locate_device_path().returning(move |_, _| Ok(handle_addr as efi::Handle));
        mock.expect_handle_protocol::<block_io::Protocol>().returning(|_| Err(efi::Status::UNSUPPORTED));

        let result = write_partition_raw(&mock, &device_path, &[0xAB; 16]);
        assert!(result.is_err(), "missing BlockIo interface on located handle must surface as Err");
    }

    // Tests for write_partition_raw_inner — exercise the unsafe FFI dispatch path with mock
    // BlockIo function pointers.

    static CAPTURED_WRITE_MEDIA_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    static CAPTURED_WRITE_LBA: AtomicU64 = AtomicU64::new(0xFFFF_FFFF_FFFF_FFFF);
    static CAPTURED_WRITE_SIZE: AtomicUsize = AtomicUsize::new(0);
    static FLUSH_BLOCKS_INVOKED: AtomicBool = AtomicBool::new(false);

    fn make_test_media(read_only: bool, present: bool) -> block_io::Media {
        block_io::Media {
            media_id: 0xCAFEBABE,
            removable_media: false,
            media_present: present,
            logical_partition: false,
            read_only,
            write_caching: false,
            block_size: 512,
            io_align: 0,
            last_block: 1024,
            lowest_aligned_lba: 0,
            logical_blocks_per_physical_block: 1,
            optimal_transfer_length_granularity: 0,
        }
    }

    extern "efiapi" fn mock_block_io_reset(_this: *mut block_io::Protocol, _ext: efi::Boolean) -> efi::Status {
        efi::Status::SUCCESS
    }

    extern "efiapi" fn mock_block_io_read_blocks(
        _this: *mut block_io::Protocol,
        _media_id: u32,
        _lba: efi::Lba,
        _buffer_size: usize,
        _buffer: *mut core::ffi::c_void,
    ) -> efi::Status {
        efi::Status::UNSUPPORTED
    }

    extern "efiapi" fn mock_write_blocks_capture(
        _this: *mut block_io::Protocol,
        media_id: u32,
        lba: efi::Lba,
        buffer_size: usize,
        _buffer: *mut core::ffi::c_void,
    ) -> efi::Status {
        CAPTURED_WRITE_MEDIA_ID.store(media_id, Ordering::SeqCst);
        CAPTURED_WRITE_LBA.store(lba, Ordering::SeqCst);
        CAPTURED_WRITE_SIZE.store(buffer_size, Ordering::SeqCst);
        efi::Status::SUCCESS
    }

    extern "efiapi" fn mock_write_blocks_returns_error(
        _this: *mut block_io::Protocol,
        _media_id: u32,
        _lba: efi::Lba,
        _buffer_size: usize,
        _buffer: *mut core::ffi::c_void,
    ) -> efi::Status {
        efi::Status::DEVICE_ERROR
    }

    extern "efiapi" fn mock_flush_blocks_success(_this: *mut block_io::Protocol) -> efi::Status {
        FLUSH_BLOCKS_INVOKED.store(true, Ordering::SeqCst);
        efi::Status::SUCCESS
    }

    fn make_test_protocol(
        media: *const block_io::Media,
        write_blocks: block_io::ProtocolWriteBlocks,
    ) -> block_io::Protocol {
        block_io::Protocol {
            revision: block_io::REVISION,
            media,
            reset: mock_block_io_reset,
            read_blocks: mock_block_io_read_blocks,
            write_blocks,
            flush_blocks: mock_flush_blocks_success,
        }
    }

    #[test]
    fn test_write_partition_raw_inner_writes_to_lba_zero_with_aligned_size() {
        FLUSH_BLOCKS_INVOKED.store(false, Ordering::SeqCst);
        let media = make_test_media(false, true);
        let mut protocol = make_test_protocol(&media, mock_write_blocks_capture);

        // Data size 600 should round up to 1024 (two 512-byte blocks).
        let data = [0xAB; 600];
        // SAFETY: protocol and media are valid for the lifetime of the call.
        let result = unsafe { write_partition_raw_inner(&mut protocol, &data) };

        assert!(result.is_ok(), "successful write+flush must produce Ok");
        assert_eq!(CAPTURED_WRITE_MEDIA_ID.load(Ordering::SeqCst), 0xCAFEBABE, "media_id forwarded");
        assert_eq!(CAPTURED_WRITE_LBA.load(Ordering::SeqCst), 0, "writes start at LBA 0");
        assert_eq!(CAPTURED_WRITE_SIZE.load(Ordering::SeqCst), 1024, "600 bytes round up to 2 blocks of 512");
        assert!(FLUSH_BLOCKS_INVOKED.load(Ordering::SeqCst), "FlushBlocks must run after WriteBlocks");
    }

    #[test]
    fn test_write_partition_raw_inner_read_only_rejected() {
        let media = make_test_media(true, true);
        let mut protocol = make_test_protocol(&media, mock_write_blocks_capture);

        // SAFETY: protocol and media are valid for the lifetime of the call.
        let result = unsafe { write_partition_raw_inner(&mut protocol, &[0xAB; 16]) };
        assert!(result.is_err(), "read-only media must surface as Err before WriteBlocks runs");
    }

    #[test]
    fn test_write_partition_raw_inner_writeblocks_failure_propagates() {
        let media = make_test_media(false, true);
        let mut protocol = make_test_protocol(&media, mock_write_blocks_returns_error);

        // SAFETY: protocol and media are valid for the lifetime of the call.
        let result = unsafe { write_partition_raw_inner(&mut protocol, &[0xAB; 16]) };
        assert!(result.is_err(), "WriteBlocks DEVICE_ERROR must surface as Err");
    }
}
