//! UEFI HID - Human Interface Device support as a Patina component.
//!
//! This crate provides a Patina component that consumes the HidIo protocol and
//! produces UEFI input protocols (SimpleTextInput, SimpleTextInputEx,
//! AbsolutePointer) for keyboard and pointer HID devices.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![cfg_attr(coverage, feature(coverage_attribute))]

extern crate alloc;

pub mod hid;
pub mod hid_io;
pub mod keyboard;
pub mod pointer;

#[cfg(test)]
pub(crate) mod test_stubs;

use r_efi::efi;

use patina::{
    component::{
        component,
        service::{
            Service,
            uefi_services::{
                event::EventServices,
                protocol::{ProtocolServices, ProtocolServicesExt},
                timer_event::TimerEventServices,
                tpl::TplServices,
            },
        },
    },
    error::Result,
};

#[cfg(feature = "ctrl-alt-del")]
use core::sync::atomic::AtomicPtr;

#[cfg(feature = "ctrl-alt-del")]
use patina::uefi::runtime_services::StandardRuntimeServices;

/// Global pointer to UEFI Runtime Services, used by the Ctrl-Alt-Del reset callback.
#[cfg(feature = "ctrl-alt-del")]
pub static RUNTIME_SERVICES: AtomicPtr<efi::RuntimeServices> = AtomicPtr::new(core::ptr::null_mut());

/// The set of UEFI services used by the HID driver and its keyboard and pointer handlers.
#[derive(Clone, Copy)]
pub struct HidServices {
    pub(crate) protocols: Service<dyn ProtocolServices>,
    pub(crate) events: Service<dyn EventServices>,
    pub(crate) timers: Service<dyn TimerEventServices>,
    pub(crate) tpl: Service<dyn TplServices>,
}

/// Converts a UEFI service error (e.g. `ProtocolError`, `EventError`) into an `efi::Status`.
pub(crate) fn to_status(error: impl Into<patina::error::EfiError>) -> efi::Status {
    error.into().into()
}

/// UEFI HID Patina component.
///
/// When dispatched, installs a UEFI Driver Binding that consumes HidIo
/// protocol instances and produces keyboard and pointer input protocols.
pub struct UefiHidComponent;

#[component]
impl UefiHidComponent {
    #[cfg(feature = "ctrl-alt-del")]
    fn entry_point(
        self,
        protocols: Service<dyn ProtocolServices>,
        events: Service<dyn EventServices>,
        timers: Service<dyn TimerEventServices>,
        tpl: Service<dyn TplServices>,
        runtime_services: StandardRuntimeServices,
    ) -> Result<()> {
        RUNTIME_SERVICES.store(runtime_services.as_mut_ptr(), core::sync::atomic::Ordering::SeqCst);
        install_hid_driver_binding(HidServices { protocols, events, timers, tpl })
    }

    #[cfg(not(feature = "ctrl-alt-del"))]
    fn entry_point(
        self,
        protocols: Service<dyn ProtocolServices>,
        events: Service<dyn EventServices>,
        timers: Service<dyn TimerEventServices>,
        tpl: Service<dyn TplServices>,
    ) -> Result<()> {
        install_hid_driver_binding(HidServices { protocols, events, timers, tpl })
    }
}

/// Installs the HID driver binding using the provided services.
///
/// Separated from the component entry point to allow testing with mock services.
fn install_hid_driver_binding(services: HidServices) -> Result<()> {
    services.protocols.install_driver_binding(hid::HidDriver::new(services))?;
    Ok(())
}

#[cfg(test)]
mod test {
    use patina::{
        component::service::uefi_services::{handle::Handle, protocol::ProtocolError},
        error::EfiError,
    };

    use super::*;

    #[test]
    fn install_hid_binding_should_install_a_binding() {
        let services = test_stubs::mock_services(|protocols, _, _| {
            protocols.expect_register_agent().returning(|| Ok(Handle::from_raw(0x5678 as efi::Handle).unwrap()));
            protocols.expect_image_handle().times(1).returning(|| Handle::from_raw(0x1234 as efi::Handle).unwrap());
            protocols.expect_install_interface().returning(|handle, guid, interface| {
                assert_eq!(guid, efi::protocols::driver_binding::PROTOCOL_GUID);
                assert_eq!(handle, Handle::from_raw(0x5678 as efi::Handle));
                assert!(!interface.as_raw().is_null());
                Ok(handle.unwrap())
            });
        });
        install_hid_driver_binding(services).expect("install should succeed");
    }

    #[test]
    fn install_hid_binding_handles_failure() {
        let services = test_stubs::mock_services(|protocols, _, _| {
            protocols.expect_register_agent().returning(|| Err(ProtocolError::OutOfResources));
        });
        assert_eq!(install_hid_driver_binding(services), Err(EfiError::OutOfResources));

        let services = test_stubs::mock_services(|protocols, _, _| {
            protocols.expect_register_agent().returning(|| Ok(Handle::from_raw(0x5678 as efi::Handle).unwrap()));
            protocols.expect_image_handle().times(1).returning(|| Handle::from_raw(0x1234 as efi::Handle).unwrap());
            protocols.expect_install_interface().returning(|_, _, _| Err(ProtocolError::OutOfResources));
        });
        assert_eq!(install_hid_driver_binding(services), Err(EfiError::OutOfResources));
    }
}
