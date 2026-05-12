//! Minimal FFI bindings for `EFI_NVM_EXPRESS_PASS_THRU_PROTOCOL`.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
use core::ffi::c_void;

use r_efi::efi;

/// `EFI_NVM_EXPRESS_PASS_THRU_PROTOCOL` GUID.
pub const PROTOCOL_GUID: efi::Guid =
    efi::Guid::from_fields(0x52c78312, 0x8edc, 0x4233, 0x98, 0xf2, &[0x1a, 0x1a, 0xa5, 0xe3, 0x88, 0xa5]);

/// NVMe admin opcode for `Set Features`.
pub const OPCODE_SET_FEATURES: u8 = 0x09;

/// NVMe Feature Identifier for Boot Partition Write Protection Configuration (BPWPS).
pub const FID_BOOT_PARTITION_WRITE_PROTECTION: u8 = 0x11;

/// Command-flag bit indicating CDW10 carries a valid value.
pub const CMD_FLAG_CDW10_VALID: u8 = 1 << 2;
/// Command-flag bit indicating CDW11 carries a valid value.
pub const CMD_FLAG_CDW11_VALID: u8 = 1 << 3;

/// Queue-type selector for the admin queue.
pub const QUEUE_TYPE_ADMIN: u8 = 0;

/// 1-second timeout in 100-ns units, per the protocol contract.
pub const TIMEOUT_NS_1_SEC: u64 = 10_000_000;

/// CDW11 value placing both BP0 and BP1 in "Write Protect Until Power Cycle" (state 001b).
/// Layout: BP0WPS in bits 2:0, BP1WPS in bits 5:3.
pub const BPWPS_LOCK_BP0_BP1: u32 = (0b001 << 3) | 0b001;

/// FFI type for the protocol's `PassThru` function pointer.
pub type PassThruFn = extern "efiapi" fn(
    this: *mut Protocol,
    namespace_id: u32,
    packet: *mut CommandPacket,
    event: *mut c_void,
) -> efi::Status;

/// FFI binding for `EFI_NVM_EXPRESS_PASS_THRU_PROTOCOL`.
///
/// Only the `pass_thru` function pointer is exercised here; the remaining members are typed
/// as opaque pointers because their layouts aren't needed for the BPWPS Set Features path.
#[repr(C)]
pub struct Protocol {
    /// Pointer to the mode structure (opaque).
    pub mode: *mut c_void,
    /// Issues an NVMe command on the supplied namespace.
    pub pass_thru: PassThruFn,
    /// Walks the controller's namespace list (opaque).
    pub get_next_namespace: *mut c_void,
    /// Builds a device path for a namespace (opaque).
    pub build_device_path: *mut c_void,
    /// Resolves a device path to a namespace ID (opaque).
    pub get_namespace: *mut c_void,
}

/// NVMe command packet passed to `Protocol::pass_thru`.
#[repr(C)]
pub struct CommandPacket {
    /// Command timeout in 100-ns units (0 disables timeout).
    pub command_timeout: u64,
    /// Optional data-in/data-out buffer.
    pub transfer_buffer: *mut c_void,
    /// Length of `transfer_buffer` in bytes.
    pub transfer_length: u32,
    /// Optional metadata buffer.
    pub metadata_buffer: *mut c_void,
    /// Length of `metadata_buffer` in bytes.
    pub metadata_length: u32,
    /// Queue selector: `QUEUE_TYPE_ADMIN` (0) or I/O queue (1).
    pub queue_type: u8,
    /// Pointer to the NVMe submission command.
    pub nvme_cmd: *mut Command,
    /// Pointer to the NVMe completion structure.
    pub nvme_completion: *mut Completion,
}

/// 64-byte NVMe submission command (16 dwords).
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Command {
    /// Command Dword 0 (opcode + command identifier).
    pub cdw0: u32,
    /// Command flags (CDW10..CDW15 validity bits).
    pub flags: u8,
    /// Namespace ID.
    pub nsid: u32,
    /// Command Dword 2.
    pub cdw2: u32,
    /// Command Dword 3.
    pub cdw3: u32,
    /// Command Dword 10.
    pub cdw10: u32,
    /// Command Dword 11.
    pub cdw11: u32,
    /// Command Dword 12.
    pub cdw12: u32,
    /// Command Dword 13.
    pub cdw13: u32,
    /// Command Dword 14.
    pub cdw14: u32,
    /// Command Dword 15.
    pub cdw15: u32,
}

impl Command {
    /// Zero-initialized `Command`. Useful as a base when setting only a few fields.
    pub const fn zero() -> Self {
        Self {
            cdw0: 0,
            flags: 0,
            nsid: 0,
            cdw2: 0,
            cdw3: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }
}

/// 16-byte NVMe completion structure (4 dwords).
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Completion {
    /// Completion Dword 0.
    pub dw0: u32,
    /// Completion Dword 1.
    pub dw1: u32,
    /// Completion Dword 2.
    pub dw2: u32,
    /// Completion Dword 3 (carries the Status Field in bits 31:17).
    pub dw3: u32,
}

impl Completion {
    /// Zero-initialized `Completion`. Useful as a destination buffer for `pass_thru`.
    pub const fn zero() -> Self {
        Self { dw0: 0, dw1: 0, dw2: 0, dw3: 0 }
    }
}
