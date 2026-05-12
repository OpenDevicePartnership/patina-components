//! NVMe protocol helpers for Patina firmware.
//!
//! This crate exposes thin wrappers over `EFI_NVM_EXPRESS_PASS_THRU_PROTOCOL` admin
//! commands that orchestrators or platform components may need to invoke directly.
//! It deliberately stays narrow: no boot orchestration, no partition I/O — just
//! NVMe-specific operations.
//!
//! ## Functions
//!
//! - [`lock_partition_write`] — write-protect the NVMe boot partitions until the next power
//!   cycle via `Set Features` (FID 0x11, BPWPS).
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
#![cfg_attr(not(feature = "std"), no_std)]

pub mod pass_thru;

use core::ptr;

use patina::{
    boot_services::BootServices,
    device_path::paths::DevicePathBuf,
    error::{EfiError, Result},
};
use r_efi::efi;

/// Write-protect the NVMe boot partition addressed by `device_path` until the next power cycle.
///
/// Resolves the controller handle by walking `device_path` for the NVMe Pass-Thru protocol,
/// then issues an NVMe Set Features admin command for FID 0x11 (Boot Partition Write Protection
/// Configuration). Both BP0 and BP1 are placed in "Write Protect Until Power Cycle" state (001b).
///
/// The lock is volatile: a controller reset or power cycle clears it.
///
/// # Arguments
///
/// * `boot_services` - Boot services interface
/// * `device_path` - Device path resolving to (or descending from) an NVMe controller. The path
///   is consumed for protocol lookup only; partition or namespace nodes are tolerated.
///
/// # Returns
///
/// Returns `Ok(())` once the controller acknowledges the Set Features command. Returns an error
/// if no NVMe Pass-Thru protocol is reachable on the path, or if the controller rejects the
/// command.
pub fn lock_partition_write<B: BootServices>(boot_services: &B, device_path: &DevicePathBuf) -> Result<()> {
    let mut path_ptr = device_path.as_ref() as *const _ as *mut efi::protocols::device_path::Protocol;

    // SAFETY: path_ptr points into a valid DevicePathBuf for the duration of this call.
    let handle = unsafe { boot_services.locate_device_path(&pass_thru::PROTOCOL_GUID, &mut path_ptr) }
        .map_err(EfiError::from)?;

    // SAFETY: handle was returned by locate_device_path for the NVMe Pass-Thru GUID.
    let protocol = unsafe {
        boot_services.handle_protocol_unchecked(handle, &pass_thru::PROTOCOL_GUID).map_err(EfiError::from)?
    } as *mut pass_thru::Protocol;

    // SAFETY: protocol is a non-null, properly aligned pointer to a Protocol owned by the controller.
    unsafe { lock_partition_write_inner(protocol) }
}

/// Issue the NVMe Set Features admin command for BPWPS via the supplied Pass-Thru protocol.
///
/// Separated from `lock_partition_write` because it dereferences a raw protocol pointer and
/// invokes its function pointer directly. Tests use mock protocol function pointers to exercise
/// the dispatch path.
///
/// # Safety
///
/// `protocol` must be a valid, non-null pointer to an `EFI_NVM_EXPRESS_PASS_THRU_PROTOCOL`
/// instance owned by an NVMe controller for the duration of this call.
unsafe fn lock_partition_write_inner(protocol: *mut pass_thru::Protocol) -> Result<()> {
    use pass_thru::{
        BPWPS_LOCK_BP0_BP1, CMD_FLAG_CDW10_VALID, CMD_FLAG_CDW11_VALID, Command, CommandPacket, Completion,
        FID_BOOT_PARTITION_WRITE_PROTECTION, OPCODE_SET_FEATURES, QUEUE_TYPE_ADMIN, TIMEOUT_NS_1_SEC,
    };

    let mut nvme_cmd = Command { cdw0: OPCODE_SET_FEATURES as u32, ..Command::zero() };
    nvme_cmd.flags = CMD_FLAG_CDW10_VALID | CMD_FLAG_CDW11_VALID;
    nvme_cmd.cdw10 = FID_BOOT_PARTITION_WRITE_PROTECTION as u32;
    nvme_cmd.cdw11 = BPWPS_LOCK_BP0_BP1;

    let mut completion = Completion::zero();

    let mut packet = CommandPacket {
        command_timeout: TIMEOUT_NS_1_SEC,
        transfer_buffer: ptr::null_mut(),
        transfer_length: 0,
        metadata_buffer: ptr::null_mut(),
        metadata_length: 0,
        queue_type: QUEUE_TYPE_ADMIN,
        nvme_cmd: &mut nvme_cmd,
        nvme_completion: &mut completion,
    };

    // SAFETY: caller guarantees `protocol` is valid; packet pointers are kept alive across the call.
    let pass_thru_fn = unsafe { (*protocol).pass_thru };
    let status = pass_thru_fn(protocol, 0, &mut packet, ptr::null_mut());
    if status != efi::Status::SUCCESS {
        return Err(EfiError::from(status));
    }

    // NVMe completion DW3 carries Status Field in bits 31:17. Non-zero indicates the controller
    // rejected the command (e.g., feature unsupported or BP already permanently locked).
    let status_field = (completion.dw3 >> 17) & 0x7FFF;
    if status_field != 0 {
        log::error!("NVMe Set Features BPWPS rejected: status field {:#x}", status_field);
        return Err(EfiError::from(efi::Status::DEVICE_ERROR));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::sync::atomic::{AtomicUsize, Ordering};

    use patina::{
        boot_services::MockBootServices,
        device_path::{node_defs::EndEntire, paths::DevicePathBuf},
    };

    use super::*;

    fn create_test_device_path() -> DevicePathBuf {
        DevicePathBuf::from_device_path_node_iter(core::iter::once(EndEntire))
    }

    #[test]
    fn test_lock_partition_write_locate_failure() {
        let device_path = create_test_device_path();
        let mut mock = MockBootServices::new();

        mock.expect_locate_device_path().returning(|_, _| Err(efi::Status::NOT_FOUND));

        let result = lock_partition_write(&mock, &device_path);
        assert!(result.is_err(), "missing NVMe Pass-Thru on path must surface as Err");
    }

    #[test]
    fn test_lock_partition_write_handle_protocol_failure() {
        let device_path = create_test_device_path();
        let mut mock = MockBootServices::new();

        let handle_addr: usize = 1;
        mock.expect_locate_device_path().returning(move |_, _| Ok(handle_addr as efi::Handle));
        mock.expect_handle_protocol_unchecked().returning(|_, _| Err(efi::Status::UNSUPPORTED));

        let result = lock_partition_write(&mock, &device_path);
        assert!(result.is_err(), "unsupported protocol on located handle must surface as Err");
    }

    #[test]
    fn test_lock_partition_write_set_features_payload() {
        use pass_thru::{
            BPWPS_LOCK_BP0_BP1, FID_BOOT_PARTITION_WRITE_PROTECTION, OPCODE_SET_FEATURES, QUEUE_TYPE_ADMIN,
        };

        assert_eq!(OPCODE_SET_FEATURES, 0x09, "Set Features admin opcode");
        assert_eq!(FID_BOOT_PARTITION_WRITE_PROTECTION, 0x11, "BPWPS feature identifier");
        assert_eq!(QUEUE_TYPE_ADMIN, 0, "admin queue selector");
        assert_eq!(BPWPS_LOCK_BP0_BP1, 0x09, "CDW11 must encode BP0WPS=001b in bits 2:0 and BP1WPS=001b in bits 5:3");
    }

    // Tests for lock_partition_write_inner — exercise the unsafe FFI dispatch path with mock
    // protocol function pointers.

    static CAPTURED_PASS_THRU_OPCODE: AtomicUsize = AtomicUsize::new(0);
    static CAPTURED_PASS_THRU_FLAGS: AtomicUsize = AtomicUsize::new(0);
    static CAPTURED_PASS_THRU_CDW10: AtomicUsize = AtomicUsize::new(0);
    static CAPTURED_PASS_THRU_CDW11: AtomicUsize = AtomicUsize::new(0);
    static CAPTURED_PASS_THRU_QUEUE_TYPE: AtomicUsize = AtomicUsize::new(0);

    extern "efiapi" fn mock_pass_thru_capture_success(
        _this: *mut pass_thru::Protocol,
        _namespace_id: u32,
        packet: *mut pass_thru::CommandPacket,
        _event: *mut core::ffi::c_void,
    ) -> efi::Status {
        // SAFETY: caller (helper) constructs a valid CommandPacket whose nvme_cmd points to a
        // valid Command. Test storage of captured values is single-threaded.
        unsafe {
            let pkt = &*packet;
            let cmd = &*pkt.nvme_cmd;
            CAPTURED_PASS_THRU_OPCODE.store((cmd.cdw0 & 0xFF) as usize, Ordering::SeqCst);
            CAPTURED_PASS_THRU_FLAGS.store(cmd.flags as usize, Ordering::SeqCst);
            CAPTURED_PASS_THRU_CDW10.store(cmd.cdw10 as usize, Ordering::SeqCst);
            CAPTURED_PASS_THRU_CDW11.store(cmd.cdw11 as usize, Ordering::SeqCst);
            CAPTURED_PASS_THRU_QUEUE_TYPE.store(pkt.queue_type as usize, Ordering::SeqCst);
        }
        efi::Status::SUCCESS
    }

    extern "efiapi" fn mock_pass_thru_returns_error(
        _this: *mut pass_thru::Protocol,
        _namespace_id: u32,
        _packet: *mut pass_thru::CommandPacket,
        _event: *mut core::ffi::c_void,
    ) -> efi::Status {
        efi::Status::DEVICE_ERROR
    }

    extern "efiapi" fn mock_pass_thru_nonzero_completion_status(
        _this: *mut pass_thru::Protocol,
        _namespace_id: u32,
        packet: *mut pass_thru::CommandPacket,
        _event: *mut core::ffi::c_void,
    ) -> efi::Status {
        // SAFETY: caller-provided pointers are valid for the lifetime of the call.
        unsafe {
            let pkt = &*packet;
            (*pkt.nvme_completion).dw3 = 0x2 << 17; // Status Field = 0x2 (Invalid Field in Command)
        }
        efi::Status::SUCCESS
    }

    #[test]
    fn test_lock_partition_write_inner_issues_correct_set_features_command() {
        let mut protocol = pass_thru::Protocol {
            mode: ptr::null_mut(),
            pass_thru: mock_pass_thru_capture_success,
            get_next_namespace: ptr::null_mut(),
            build_device_path: ptr::null_mut(),
            get_namespace: ptr::null_mut(),
        };

        // SAFETY: protocol is a valid Protocol kept alive on the test stack.
        let result = unsafe { lock_partition_write_inner(&mut protocol) };

        assert!(result.is_ok(), "successful pass-thru must produce Ok");
        assert_eq!(CAPTURED_PASS_THRU_OPCODE.load(Ordering::SeqCst), 0x09, "Set Features opcode");
        assert_eq!(CAPTURED_PASS_THRU_FLAGS.load(Ordering::SeqCst), 0b0000_1100, "CDW10 + CDW11 marked valid");
        assert_eq!(CAPTURED_PASS_THRU_CDW10.load(Ordering::SeqCst), 0x11, "FID = BPWPS");
        assert_eq!(CAPTURED_PASS_THRU_CDW11.load(Ordering::SeqCst), 0x09, "BP0WPS=001b, BP1WPS=001b");
        assert_eq!(CAPTURED_PASS_THRU_QUEUE_TYPE.load(Ordering::SeqCst), 0, "admin queue");
    }

    #[test]
    fn test_lock_partition_write_inner_passthru_failure_propagates() {
        let mut protocol = pass_thru::Protocol {
            mode: ptr::null_mut(),
            pass_thru: mock_pass_thru_returns_error,
            get_next_namespace: ptr::null_mut(),
            build_device_path: ptr::null_mut(),
            get_namespace: ptr::null_mut(),
        };

        // SAFETY: protocol is a valid Protocol kept alive on the test stack.
        let result = unsafe { lock_partition_write_inner(&mut protocol) };
        assert!(result.is_err(), "pass-thru DEVICE_ERROR must surface as Err");
    }

    #[test]
    fn test_lock_partition_write_inner_nonzero_completion_status_rejected() {
        let mut protocol = pass_thru::Protocol {
            mode: ptr::null_mut(),
            pass_thru: mock_pass_thru_nonzero_completion_status,
            get_next_namespace: ptr::null_mut(),
            build_device_path: ptr::null_mut(),
            get_namespace: ptr::null_mut(),
        };

        // SAFETY: protocol is a valid Protocol kept alive on the test stack.
        let result = unsafe { lock_partition_write_inner(&mut protocol) };
        assert!(result.is_err(), "non-zero completion status field must surface as Err");
    }
}
