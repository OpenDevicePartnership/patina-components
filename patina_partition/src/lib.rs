//! Generic UEFI partition I/O helpers for Patina firmware.
//!
//! Two protocol-shaped modules:
//!
//! - [`block_io`] — raw block-level writes via `EFI_BLOCK_IO_PROTOCOL`.
//! - [`file_system`] — file reads via `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`.
//!
//! All helpers operate on partitions identified by a [`patina::device_path::paths::DevicePathBuf`]
//! and use UEFI protocols already published by the platform's storage stack. They are intended
//! for orchestrators implementing custom boot or recovery flows.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod block_io;
pub mod file_system;

pub use block_io::write_partition_raw;
pub use file_system::read_partition_file;
