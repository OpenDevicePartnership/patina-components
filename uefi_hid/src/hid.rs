//! HID driver binding implementation.
//!
//! The [`HidDriver`] implements [`DriverBinding`] to
//! manage HID instances on controllers that support the HidIo protocol.
//!
//! When started on a controller, it creates a [`crate::hid_io::UefiHidIo`]
//! instance, instantiates keyboard and pointer receivers, initializes them
//! via `&dyn HidIo`, and starts report reception through the device.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!

use alloc::{boxed::Box, vec::Vec};

use patina::{
    BinaryGuid,
    component::service::uefi_services::{
        driver_binding::DriverBinding,
        handle::Handle,
        protocol::{ProtocolError, ProtocolPtr},
    },
    error::EfiError,
    uefi::device_path::walker::BorrowedDevicePathWalker,
};

use crate::{
    HidServices,
    hid_io::{HidReportReceiver, ReceiverFactory, UefiHidIo},
    keyboard::KeyboardReceiver,
    pointer::PointerReceiver,
};

/// GUID of the private protocol used to track the per-controller [`UefiHidIo`] device.
const HID_INSTANCE_GUID: BinaryGuid = BinaryGuid::from_string("0a87cfdb-c482-48e4-ade7-d9f99620e169");

/// HID driver that implements [`DriverBinding`].
///
/// Creates and manages HID instances on controllers that expose the HidIo
/// protocol. Directly constructs [`UefiHidIo`] devices and creates keyboard
/// and pointer handlers as report receivers.
pub struct HidDriver {
    services: HidServices,
}

impl HidDriver {
    /// Creates a new HID driver using the given UEFI services.
    pub fn new(services: HidServices) -> Self {
        Self { services }
    }

    // Creates factory functions for HID report receivers.
    fn new_receiver_factories(&self) -> Vec<ReceiverFactory> {
        let services = self.services;
        alloc::vec![
            Box::new(move |controller, hid_io| {
                Ok(Box::new(PointerReceiver::new(services, controller, hid_io)?) as Box<dyn HidReportReceiver>)
            }),
            Box::new(move |controller, hid_io| {
                Ok(Box::new(KeyboardReceiver::new(services, controller, hid_io)?) as Box<dyn HidReportReceiver>)
            }),
        ]
    }
}

impl DriverBinding for HidDriver {
    /// Tests if the given controller supports the HidIo protocol and is not already managed.
    fn supported(
        &self,
        agent: Handle,
        controller: Handle,
        _remaining_device_path: Option<BorrowedDevicePathWalker<'_>>,
    ) -> Result<(), ProtocolError> {
        UefiHidIo::supports(self.services, agent, controller)
    }

    /// Starts HID support for the given controller.
    ///
    /// Creates a UefiHidIo device with keyboard and pointer receivers, and
    /// installs a private protocol to track the instance context.
    fn start(
        &self,
        agent: Handle,
        controller: Handle,
        _remaining_device_path: Option<BorrowedDevicePathWalker<'_>>,
    ) -> Result<(), ProtocolError> {
        log::trace!("HidDriver::start: starting HID on controller {:?}", controller);
        let device = UefiHidIo::new(self.services, agent, controller, self.new_receiver_factories())
            .map_err(|status| ProtocolError::from(EfiError::from(status)))?;

        let device = Box::into_raw(Box::new(device));
        let interface = ProtocolPtr::from_raw(device.cast()).expect("Box pointers are non-null");
        if let Err(error) = self.services.protocols.install_interface(Some(controller), HID_INSTANCE_GUID, interface) {
            log::error!("HidDriver::start: failed to install HID instance protocol: {error:?}");
            // SAFETY: device was created by Box::into_raw above and was not published.
            drop(unsafe { Box::from_raw(device) });
            return Err(error);
        }
        Ok(())
    }

    /// Stops HID support for the given controller.
    ///
    /// Retrieves and drops the HID instance context, reclaiming all resources (including closing the HidIo
    /// protocol opened by start).
    fn stop(&self, _agent: Handle, controller: Handle, _children: &[Handle]) -> Result<(), ProtocolError> {
        log::trace!("HidDriver::stop: stopping HID on controller {:?}", controller);
        let interface = self.services.protocols.interface_on_handle(controller, HID_INSTANCE_GUID)?;

        if let Err(error) = self.services.protocols.uninstall_interface(controller, HID_INSTANCE_GUID, interface) {
            log::error!("HidDriver::stop: failed to uninstall HID instance protocol: {error:?}");
            return Err(error);
        }

        // SAFETY: the interface was created via Box::into_raw in start and is no longer published.
        drop(unsafe { Box::from_raw(interface.as_raw() as *mut UefiHidIo) });
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use patina::component::service::uefi_services::{
        driver_binding::DriverBinding,
        protocol::{ProtocolError, ProtocolPtr},
    };

    use super::{HID_INSTANCE_GUID, HidDriver};
    use crate::{
        hid_io::protocol::HID_IO_PROTOCOL_GUID,
        test_stubs::{hid_io_stub, mock_services, test_handle},
    };

    fn stub_ptr() -> ProtocolPtr {
        ProtocolPtr::from_raw((hid_io_stub() as *mut _ as *mut core::ffi::c_void).cast()).unwrap()
    }

    #[test]
    fn supported_reflects_by_driver_open() {
        let services = mock_services(|protocols, _, _| {
            protocols.expect_open_interface().returning(|controller, guid, agent, _| {
                assert_eq!(guid, HID_IO_PROTOCOL_GUID);
                assert_eq!(agent, test_handle(0x1));
                match controller {
                    c if c == test_handle(0x3) => Ok(stub_ptr()),
                    c if c == test_handle(0x4) => Err(ProtocolError::AccessDenied),
                    _ => Err(ProtocolError::NotFound),
                }
            });
            protocols.expect_close_interface().returning(|_, _, _, _| Ok(()));
        });

        let hid_driver = HidDriver::new(services);

        assert_eq!(hid_driver.supported(test_handle(0x1), test_handle(0x2), None), Err(ProtocolError::NotFound));
        assert_eq!(hid_driver.supported(test_handle(0x1), test_handle(0x3), None), Ok(()));
        assert_eq!(hid_driver.supported(test_handle(0x1), test_handle(0x4), None), Err(ProtocolError::AccessDenied));
    }

    #[test]
    fn start_returns_unsupported_with_no_receivers() {
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, _, _| {
            protocols.expect_open_interface().returning(|controller, _, agent, _| {
                assert_eq!(controller, test_handle(0x2));
                assert_eq!(agent, test_handle(0x1));
                Ok(stub_ptr())
            });
            protocols.expect_close_interface().returning(|_, _, _, _| {
                CLOSED.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            protocols.expect_install_interface().never();
        });

        let hid_driver = HidDriver::new(services);

        // All receiver factories fail (stub protocol has no real descriptor).
        assert_eq!(hid_driver.start(test_handle(0x1), test_handle(0x2), None), Err(ProtocolError::Unsupported));
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stop_returns_error_when_not_started() {
        let services = mock_services(|protocols, _, _| {
            protocols.expect_interface_on_handle().returning(|_, guid| {
                assert_eq!(guid, HID_INSTANCE_GUID);
                Err(ProtocolError::NotFound)
            });
            protocols.expect_uninstall_interface().never();
        });

        let hid_driver = HidDriver::new(services);
        assert_eq!(hid_driver.stop(test_handle(0x1), test_handle(0x2), &[]), Err(ProtocolError::NotFound));
    }
}
