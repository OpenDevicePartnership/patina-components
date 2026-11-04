//! HidIo Support.
//!
//! Abstractions for interacting with HID devices via the UEFI HidIo protocol.
//!
//! ## Architecture
//!
//! Incoming reports are buffered through a `ReportQueue` rather than being
//! processed inline from the HidIo producer's callback. This ensures all report
//! processing occurs at a consistent TPL_CALLBACK regardless of the producer's
//! calling TPL:
//!
//! 1. **Report callback** (any TPL): locks [`TplMutex`] at TPL_NOTIFY, pushes
//!    raw bytes onto a [`Vec`], signals a TPL_CALLBACK event.
//! 2. **Event handler** (TPL_CALLBACK): locks the same mutex, dequeues all
//!    pending reports, then dispatches them to all receivers. The receivers are
//!    owned by the event's notify closure, so they are only ever touched at
//!    TPL_CALLBACK and are dropped when the event is closed.
//!
//! ## Traits
//!
//! - [`HidIo`] — narrow, receiver-facing interface for reading descriptors and
//!   sending output reports.
//! - [`HidReportReceiver`] — interface for logic that receives HID reports.
//!
//! ## License
//!
//! Copyright (c) Microsoft Corporation.
//!
//! SPDX-License-Identifier: Apache-2.0
//!
pub mod protocol;
use alloc::{boxed::Box, vec::Vec};
use core::{
    ffi::c_void,
    ptr::NonNull,
    slice::from_raw_parts,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(test)]
use mockall::automock;
use r_efi::efi;

use self::protocol::{HID_IO_PROTOCOL_GUID, HidIoProtocol};
use crate::HidServices;
use hidparser::ReportDescriptor;
use patina::{
    component::service::{
        Service,
        uefi_services::{
            event::{Event, EventServicesExt, Tpl as ServiceTpl},
            handle::Handle,
            protocol::{OpenAttributes, ProtocolError},
            tpl::TplServices,
        },
    },
    error::EfiError,
    uefi::{boot_services::tpl::Tpl, tpl_mutex::TplMutex},
};

/// Interface for logic that receives HID reports from a device.
#[cfg_attr(test, automock)]
pub trait HidReportReceiver {
    /// Passes an incoming report to the receiver for processing.
    fn receive_report(&mut self, report: &[u8], hid_io: &dyn HidIo);
}

/// Factory function that creates a fully initialized HidReportReceiver for a controller.
///
/// Returns the receiver on success, or an error status if the device is not supported
/// by this receiver type (e.g. wrong report descriptor).
pub type ReceiverFactory = Box<dyn FnOnce(Handle, &dyn HidIo) -> Result<Box<dyn HidReportReceiver>, efi::Status>>;

/// Receiver-facing interface for interacting with a HID device.
///
/// Provides read-only access to the report descriptor and the ability to send
/// output reports (e.g. keyboard LED state). Does not manage device lifecycle.
#[cfg_attr(test, automock)]
pub trait HidIo {
    /// Returns the parsed report descriptor for the device.
    fn get_report_descriptor(&self) -> Result<ReportDescriptor, efi::Status>;
    /// Sends an output report to the device.
    fn set_output_report(&self, id: Option<u8>, report: &[u8]) -> Result<(), efi::Status>;
}

// -- ReportQueue ----------------------------------------------------------

/// State shared between the HidIo report callback and the TPL_CALLBACK drain event.
///
/// Heap-allocated via [`Box`] for address stability and owned through a raw pointer by [`UefiHidIo`]. Because the
/// report callback can preempt the drain event at a higher TPL, only shared references are ever formed once the
/// queue is published; all mutation goes through interior mutability.
struct ReportQueue {
    /// Pending report bytes, protected at TPL_NOTIFY.
    queue: TplMutex<Vec<Vec<u8>>, Service<dyn TplServices>>,
    /// Services used for signaling the drain event.
    services: HidServices,
    /// TPL_CALLBACK event that drains the queue and dispatches reports to the receivers. Written only before the
    /// report callback is registered.
    process_queue_event: Option<Event>,
    /// Set to `true` during drop if unregister_report_callback fails. Checked by
    /// `report_callback` to bail early, preventing access to freed resources.
    ///
    /// Atomic because `report_callback` can preempt `drop` at a higher TPL. `Relaxed` ordering suffices: this is a
    /// standalone one-way flag with no other state to synchronize, and boot services run on a single processor.
    poisoned: AtomicBool,
}

impl ReportQueue {
    fn new(services: HidServices) -> Self {
        Self {
            queue: TplMutex::new(services.tpl, Tpl::NOTIFY, Vec::new()),
            services,
            process_queue_event: None,
            poisoned: AtomicBool::new(false),
        }
    }

    /// Removes and returns all pending reports.
    fn take_reports(&self) -> Vec<Vec<u8>> {
        core::mem::take(&mut *self.queue.lock())
    }
}

/// Dispatches each report to every receiver.
///
/// Called from the TPL_CALLBACK drain event, ensuring that report processing happens at the lowest possible TPL
/// regardless of the TPL the report was received at.
fn dispatch_reports(reports: &[Vec<u8>], receivers: &mut [Box<dyn HidReportReceiver>], hid_io: &dyn HidIo) {
    log::trace!("dispatch_reports: dispatching {:?} queued reports", reports.len());
    for report in reports {
        for receiver in receivers.iter_mut() {
            receiver.receive_report(report, hid_io);
        }
    }
}

// -- UefiHidIo --------------------------------------------------------------

/// HID device using UEFI services to interact with HidIo controllers.
///
/// Reports are buffered in a [`TplMutex`]-protected queue and processed at
/// TPL_CALLBACK via a UEFI event, rather than inline from the producer's
/// callback. The device is fully active from construction; teardown happens on
/// drop.
pub struct UefiHidIo {
    hid_io: *const HidIoProtocol,
    services: HidServices,
    controller: Handle,
    agent: Handle,
    /// Owned allocation from `Box::into_raw`; freed in drop only if the report callback was unregistered.
    report_queue: NonNull<ReportQueue>,
}

impl UefiHidIo {
    /// Returns `Ok(())` if the given controller exposes the HidIo protocol and is not already managed by a driver.
    ///
    /// Opens HidIo `BY_DRIVER` (and immediately closes it) so that controllers already managed by this or another
    /// driver are rejected, per the UEFI Driver Model `Supported()` convention.
    pub fn supports(services: HidServices, agent: Handle, controller: Handle) -> Result<(), ProtocolError> {
        services.protocols.open_interface(
            controller,
            HID_IO_PROTOCOL_GUID,
            agent,
            OpenAttributes::ByDriver { controller: Some(controller) },
        )?;
        Self::close_hid_io(services, agent, controller);
        Ok(())
    }

    fn close_hid_io(services: HidServices, agent: Handle, controller: Handle) {
        if let Err(error) =
            services.protocols.close_interface(controller, HID_IO_PROTOCOL_GUID, agent, Some(controller))
        {
            log::error!("Unexpected error closing HidIo protocol: {error:?}");
        }
    }

    /// Creates a new UefiHidIo bound to the given controller.
    ///
    /// Opens the device `by_driver`, runs each factory to create receivers (keeping
    /// only those that succeed), creates the report queue, and registers the
    /// protocol callback. Returns `UNSUPPORTED` if no receivers initialize
    /// successfully. The device is released on drop.
    pub fn new(
        services: HidServices,
        agent: Handle,
        controller: Handle,
        receiver_factories: Vec<ReceiverFactory>,
    ) -> Result<Self, efi::Status> {
        // Open BY_DRIVER to ensure exclusive access to the underlying protocol. It is closed explicitly on drop.
        let hid_io = services
            .protocols
            .open_interface(
                controller,
                HID_IO_PROTOCOL_GUID,
                agent,
                OpenAttributes::ByDriver { controller: Some(controller) },
            )
            .map_err(|error| efi::Status::from(EfiError::from(error)))?
            .as_raw() as *const HidIoProtocol;

        // SAFETY: hid_io points to a protocol opened BY_DRIVER above; valid for our lifetime.
        let hid_io_ref = unsafe { &*hid_io };

        // Create receivers from factories, keeping only those that succeed.
        let mut receivers: Vec<_> =
            receiver_factories.into_iter().filter_map(|factory| factory(controller, hid_io_ref).ok()).collect();

        if receivers.is_empty() {
            // No receivers initialized — close the protocol and report unsupported.
            log::trace!("UefiHidIo::new: no receivers initialized, returning UNSUPPORTED");
            Self::close_hid_io(services, agent, controller);
            return Err(efi::Status::UNSUPPORTED);
        }

        let receiver_count = receivers.len();
        // Hand ownership to a raw pointer immediately; from here on the queue is only accessed through it.
        let report_queue = NonNull::from(Box::leak(Box::new(ReportQueue::new(services))));

        // The drain closure owns the receivers: they are only used at TPL_CALLBACK and are dropped when the event
        // is closed, which happens before HidIo is closed.
        let process_queue_event = services
            .events
            .on_event(ServiceTpl::Callback, move || {
                // SAFETY: report_queue remains valid for the event's lifetime (the event is closed before the
                // allocation is freed).
                let reports = unsafe { report_queue.as_ref() }.take_reports();
                // Any reports queued while dispatching re-signal this event, so none are missed.
                // SAFETY: hid_io points to a protocol opened BY_DRIVER, which is closed only after this event.
                dispatch_reports(&reports, &mut receivers, unsafe { &*hid_io });
            })
            .map_err(|error| {
                // SAFETY: report_queue was allocated via Box above. Event creation failed and the report callback
                // is not registered, so nothing else references it.
                drop(unsafe { Box::from_raw(report_queue.as_ptr()) });
                Self::close_hid_io(services, agent, controller);
                efi::Status::from(EfiError::from(error))
            })?;

        // Set before the queue is published to report_callback so it can safely signal the event.
        // SAFETY: the queue is not yet shared with report_callback, and the event has not been signaled, so no
        // other reference to it exists.
        unsafe { (*report_queue.as_ptr()).process_queue_event = Some(process_queue_event) };

        // Register the HidIo callback. This also configures HidIo to start sending reports.
        // SAFETY: hid_io points to a valid protocol; callback and context are valid.
        match unsafe {
            (hid_io_ref.register_report_callback)(hid_io, Some(Self::report_callback), report_queue.as_ptr().cast())
        } {
            efi::Status::SUCCESS => (),
            err => {
                let _ = services.events.close_event(process_queue_event);
                Self::close_hid_io(services, agent, controller);
                // SAFETY: report_queue was allocated via Box above. The report callback was not registered and
                // process_queue_event is closed, so nothing else references it.
                drop(unsafe { Box::from_raw(report_queue.as_ptr()) });
                return Err(err);
            }
        }

        log::trace!("UefiHidIo::new: initialized with {:?} receivers on controller {:?}", receiver_count, controller);

        Ok(Self { hid_io, services, controller, agent, report_queue })
    }

    /// HidIo protocol report callback. Enqueues the report and signals the drain event. This runs at
    /// whatever TPL the HidIo instance generating the report runs at, which may vary by controller and report type.
    ///
    /// # Safety
    ///
    /// `context` must be a valid pointer to the [`ReportQueue`] that was passed during
    /// `register_report_callback`. `report_buffer` must be valid for `report_buffer_size` bytes.
    unsafe extern "efiapi" fn report_callback(
        report_buffer_size: u16,
        report_buffer: *mut c_void,
        context: *mut c_void,
    ) {
        // SAFETY: context is a valid *mut ReportQueue set during register_report_callback in new().
        // The HidIo protocol callback signature requires *mut c_void, but this function only takes a
        // shared reference; no mutable aliasing occurs because mutation goes through the TplMutex on the queue.
        let report_queue = unsafe { (context as *const ReportQueue).as_ref() }.expect("null report_callback context");
        if report_queue.poisoned.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: report_buffer is valid for report_buffer_size bytes per HidIo protocol contract.
        let report = unsafe { from_raw_parts(report_buffer as *const u8, report_buffer_size as usize) };
        log::trace!("report_callback: received report, size: {:?}", report_buffer_size);
        {
            let mut queue = report_queue.queue.lock();
            queue.push(report.to_vec());
        }
        if let Some(event) = report_queue.process_queue_event {
            let _ = report_queue.services.events.signal_event(event);
        }
    }
}

impl Drop for UefiHidIo {
    fn drop(&mut self) {
        // SAFETY: hid_io points to a protocol opened BY_DRIVER in new; valid until this drop.
        let hid_io_protocol = unsafe { &*self.hid_io };
        // SAFETY: hid_io points to a valid protocol opened BY_DRIVER.
        let unregister_status =
            unsafe { (hid_io_protocol.unregister_report_callback)(self.hid_io, Some(Self::report_callback)) };

        // SAFETY: report_queue is valid until freed below; only shared references are formed.
        let report_queue = unsafe { self.report_queue.as_ref() };

        if unregister_status != efi::Status::SUCCESS {
            // Callback may still fire — poison the queue so report_callback becomes a no-op, and leak the queue so
            // the memory it dereferences remains valid. The receivers are owned by the event closure (not the
            // queue), so they are still released when the event is closed below.
            report_queue.poisoned.store(true, Ordering::Relaxed);
            log::error!(
                "Failed to unregister report callback: {unregister_status:x?}. \
                 Leaking ReportQueue to prevent use-after-free."
            );
        }

        if let Some(event) = report_queue.process_queue_event {
            let _ = self.services.events.close_event(event);
        }
        Self::close_hid_io(self.services, self.agent, self.controller);

        if unregister_status == efi::Status::SUCCESS {
            // SAFETY: the callback is unregistered and the event is closed, so nothing else references the queue;
            // it was allocated via Box in new.
            drop(unsafe { Box::from_raw(self.report_queue.as_ptr()) });
        }
    }
}

#[cfg(test)]
mod test {
    use alloc::{boxed::Box, vec};
    use core::{
        ffi::c_void,
        sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
    };

    use r_efi::efi;

    use super::protocol::{HidIoReportCallback, HidReportType};
    use crate::test_stubs::{mock_services, test_handle};
    use patina::component::service::uefi_services::{
        event::EventError,
        protocol::{ProtocolError, ProtocolPtr},
    };

    use super::*;

    fn mock_hid_io_protocol() -> &'static mut HidIoProtocol {
        crate::test_stubs::hid_io_stub()
    }

    fn protocol_ptr(protocol: &'static mut HidIoProtocol) -> ProtocolPtr {
        ProtocolPtr::from_raw((protocol as *mut HidIoProtocol).cast()).unwrap()
    }

    /// A factory that always succeeds, returning a no-op receiver.
    fn ok_factory() -> ReceiverFactory {
        Box::new(|_, _| Ok(Box::new(MockHidReportReceiver::new()) as Box<dyn HidReportReceiver>))
    }

    /// A factory that always fails.
    fn err_factory() -> ReceiverFactory {
        Box::new(|_, _| Err(efi::Status::UNSUPPORTED))
    }

    /// Builds a UefiHidIo directly around the given protocol, bypassing `new`.
    fn make_hid_device(services: HidServices, hid_io: *const HidIoProtocol) -> UefiHidIo {
        let mut report_queue = Box::new(ReportQueue::new(services));
        report_queue.process_queue_event = Some(crate::test_stubs::test_event(0xE0E));
        UefiHidIo {
            hid_io,
            services,
            controller: test_handle(0x2),
            agent: test_handle(0x1),
            report_queue: NonNull::from(Box::leak(report_queue)),
        }
    }

    /// Returns a mutable pointer to the mock protocol for setting up test stubs.
    ///
    /// # Safety
    ///
    /// Only safe in tests where the protocol was leaked from `mock_hid_io_protocol()`.
    /// The caller must ensure no aliasing references exist.
    unsafe fn mock_protocol(hid_io: *const HidIoProtocol) -> &'static mut HidIoProtocol {
        // SAFETY: hid_io was leaked from mock_hid_io_protocol and is valid for the test lifetime.
        unsafe { &mut *(hid_io as *mut HidIoProtocol) }
    }

    #[test]
    fn supports_opens_by_driver_and_closes() {
        static CLOSES: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, _, _| {
            protocols.expect_open_interface().returning(|controller, guid, agent, attributes| {
                assert_eq!(guid, HID_IO_PROTOCOL_GUID);
                assert_eq!(agent, test_handle(0x1));
                assert_eq!(attributes, OpenAttributes::ByDriver { controller: Some(controller) });
                if controller == test_handle(0x3) {
                    Ok(protocol_ptr(mock_hid_io_protocol()))
                } else {
                    Err(ProtocolError::NotFound)
                }
            });
            protocols.expect_close_interface().returning(|controller, guid, agent, by_controller| {
                assert_eq!(controller, test_handle(0x3));
                assert_eq!(guid, HID_IO_PROTOCOL_GUID);
                assert_eq!(agent, test_handle(0x1));
                assert_eq!(by_controller, Some(test_handle(0x3)));
                CLOSES.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });
        assert_eq!(UefiHidIo::supports(services, test_handle(0x1), test_handle(0x2)), Err(ProtocolError::NotFound));
        assert_eq!(CLOSES.load(Ordering::SeqCst), 0);
        assert_eq!(UefiHidIo::supports(services, test_handle(0x1), test_handle(0x3)), Ok(()));
        assert_eq!(CLOSES.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn supports_rejects_already_managed_controller() {
        let services = mock_services(|protocols, _, _| {
            protocols.expect_open_interface().returning(|_, _, _, _| Err(ProtocolError::AlreadyStarted));
            protocols.expect_close_interface().never();
        });
        assert_eq!(
            UefiHidIo::supports(services, test_handle(0x1), test_handle(0x2)),
            Err(ProtocolError::AlreadyStarted)
        );
    }

    #[test]
    fn new_returns_error_when_open_protocol_fails() {
        let services = mock_services(|protocols, _, _| {
            protocols.expect_open_interface().returning(|_, _, _, _| Err(ProtocolError::NotFound));
        });
        let result = UefiHidIo::new(services, test_handle(0x1), test_handle(0x2), vec![ok_factory()]);
        assert_eq!(result.err(), Some(efi::Status::NOT_FOUND));
    }

    #[test]
    fn new_opens_by_driver_with_agent() {
        static CLOSES: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, _, _| {
            protocols.expect_open_interface().returning(|controller, guid, agent, attributes| {
                assert_eq!(guid, HID_IO_PROTOCOL_GUID);
                assert_eq!(agent, test_handle(0x1));
                assert_eq!(attributes, OpenAttributes::ByDriver { controller: Some(controller) });
                Ok(protocol_ptr(mock_hid_io_protocol()))
            });
            protocols.expect_close_interface().returning(|controller, guid, agent, by_controller| {
                assert_eq!(guid, HID_IO_PROTOCOL_GUID);
                assert_eq!(agent, test_handle(0x1));
                assert_eq!(by_controller, Some(controller));
                CLOSES.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let device = UefiHidIo::new(services, test_handle(0x1), test_handle(0x2), vec![ok_factory()]).unwrap();
        assert_eq!(CLOSES.load(Ordering::SeqCst), 0);
        drop(device);
        assert_eq!(CLOSES.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn new_returns_unsupported_when_no_receivers_initialize() {
        static CLOSES: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, _, _| {
            protocols.expect_open_interface().returning(|_, _, _, _| Ok(protocol_ptr(mock_hid_io_protocol())));
            protocols.expect_close_interface().returning(|_, _, _, _| {
                CLOSES.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let result = UefiHidIo::new(services, test_handle(0x1), test_handle(0x2), vec![err_factory()]);
        assert_eq!(result.err(), Some(efi::Status::UNSUPPORTED));
        let result = UefiHidIo::new(services, test_handle(0x1), test_handle(0x2), vec![]);
        assert_eq!(result.err(), Some(efi::Status::UNSUPPORTED));
        assert_eq!(CLOSES.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn new_closes_protocol_when_event_creation_fails() {
        static CLOSES: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            protocols.expect_open_interface().returning(|_, _, _, _| Ok(protocol_ptr(mock_hid_io_protocol())));
            protocols.expect_close_interface().returning(|_, _, _, _| {
                CLOSES.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            events.expect_create_event().returning(|_, _| Err(EventError::Internal));
        });

        let result = UefiHidIo::new(services, test_handle(0x1), test_handle(0x2), vec![ok_factory()]);
        assert_eq!(result.err(), Some(efi::Status::UNSUPPORTED));
        assert_eq!(CLOSES.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn hid_io_set_output_report_calls_protocol() {
        let services = mock_services(|protocols, _, _| {
            protocols.expect_close_interface().returning(|_, _, _, _| Ok(()));
        });
        let device = make_hid_device(services, mock_hid_io_protocol());

        extern "efiapi" fn mock_set_report(
            _this: *const HidIoProtocol,
            report_id: u8,
            report_type: HidReportType,
            report_buffer_size: usize,
            report_buffer: *mut c_void,
        ) -> efi::Status {
            assert_eq!(report_id, 5);
            assert_eq!(report_type, HidReportType::OutputReport);
            assert_eq!(report_buffer_size, 4);
            // SAFETY: report_buffer is valid for report_buffer_size bytes, as guaranteed by the HID I/O protocol contract.
            let report = unsafe { core::slice::from_raw_parts(report_buffer as *const u8, report_buffer_size) };
            assert_eq!(report, [0x00, 0x01, 0x02, 0x03]);
            efi::Status::SUCCESS
        }
        // SAFETY: device.hid_io was leaked from mock_hid_io_protocol; no aliasing references exist.
        unsafe { mock_protocol(device.hid_io) }.set_report = mock_set_report;

        // SAFETY: device.hid_io is a valid pointer leaked from mock_hid_io_protocol.
        assert_eq!(unsafe { &*device.hid_io }.set_output_report(Some(5), &[0x00, 0x01, 0x02, 0x03]), Ok(()));
    }

    #[test]
    fn report_callback_enqueues_and_event_delivers_to_all() {
        type EventCallback = Box<dyn FnMut(Event)>;
        static CALLBACK_CONTEXT: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
        static SIGNALS: AtomicUsize = AtomicUsize::new(0);
        std::thread_local! {
            static NOTIFY: core::cell::RefCell<Option<EventCallback>> = const { core::cell::RefCell::new(None) };
        }

        extern "efiapi" fn register_cb(
            _this: *const HidIoProtocol,
            _callback: Option<HidIoReportCallback>,
            context: *mut c_void,
        ) -> efi::Status {
            CALLBACK_CONTEXT.store(context, Ordering::SeqCst);
            efi::Status::SUCCESS
        }

        let services = mock_services(|protocols, events, _| {
            protocols.expect_open_interface().returning(|_, _, _, _| {
                let protocol = mock_hid_io_protocol();
                protocol.register_report_callback = register_cb;
                Ok(protocol_ptr(protocol))
            });
            protocols.expect_close_interface().returning(|_, _, _, _| Ok(()));
            events.expect_create_event().returning(|tpl, callback| {
                assert_eq!(tpl, ServiceTpl::Callback);
                NOTIFY.with(|notify| *notify.borrow_mut() = Some(callback));
                Ok(crate::test_stubs::test_event(0xE0E))
            });
            events.expect_signal_event().returning(|_| {
                SIGNALS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let factories: Vec<ReceiverFactory> = vec![
            Box::new(|_, _| {
                let mut r = MockHidReportReceiver::new();
                r.expect_receive_report().withf(|report, _| report == [0x10u8, 0x20, 0x30]).times(1).return_const(());
                Ok(Box::new(r) as Box<dyn HidReportReceiver>)
            }),
            Box::new(|_, _| {
                let mut r = MockHidReportReceiver::new();
                r.expect_receive_report().withf(|report, _| report == [0x10u8, 0x20, 0x30]).times(1).return_const(());
                Ok(Box::new(r) as Box<dyn HidReportReceiver>)
            }),
        ];
        let device = UefiHidIo::new(services, test_handle(0x1), test_handle(0x2), factories).unwrap();

        // Simulate the HidIo producer calling report_callback.
        let report_data = [0x10u8, 0x20, 0x30];
        // SAFETY: test-only call with valid report data and the registered context.
        unsafe {
            UefiHidIo::report_callback(
                report_data.len() as u16,
                report_data.as_ptr() as *mut c_void,
                CALLBACK_CONTEXT.load(Ordering::SeqCst),
            );
        }
        assert_eq!(SIGNALS.load(Ordering::SeqCst), 1);

        // Fire the process-queue event (normally dispatched by the UEFI event system).
        NOTIFY.with(|notify| (notify.borrow_mut().as_mut().unwrap())(crate::test_stubs::test_event(0xE0E)));

        drop(device);
        // The receivers are owned by the event closure; dropping it verifies the MockHidReportReceiver expectations.
        drop(NOTIFY.with(|notify| notify.borrow_mut().take()));
    }

    #[test]
    fn report_callback_is_noop_when_poisoned() {
        let services = mock_services(|protocols, events, _| {
            protocols.expect_close_interface().returning(|_, _, _, _| Ok(()));
            events.expect_signal_event().never();
        });
        let device = make_hid_device(services, mock_hid_io_protocol());
        // SAFETY: report_queue is valid for the device's lifetime.
        let report_queue = unsafe { device.report_queue.as_ref() };
        report_queue.poisoned.store(true, Ordering::Relaxed);
        let report_data = [0x1u8];
        // SAFETY: test-only call with valid report data and context.
        unsafe {
            UefiHidIo::report_callback(1, report_data.as_ptr() as *mut c_void, device.report_queue.as_ptr().cast())
        };
        assert!(report_queue.queue.lock().is_empty());
    }

    #[test]
    fn drop_closes_event_and_protocol() {
        static CLOSED_EVENTS: AtomicUsize = AtomicUsize::new(0);
        static CLOSED_PROTOCOLS: AtomicUsize = AtomicUsize::new(0);
        let services = mock_services(|protocols, events, _| {
            protocols.expect_close_interface().returning(|_, _, _, _| {
                CLOSED_PROTOCOLS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            events.expect_close_event().returning(|_| {
                CLOSED_EVENTS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });
        drop(make_hid_device(services, mock_hid_io_protocol()));
        assert_eq!(CLOSED_EVENTS.load(Ordering::SeqCst), 1);
        assert_eq!(CLOSED_PROTOCOLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn new_cleans_up_on_register_callback_failure() {
        extern "efiapi" fn failing_register(
            _this: *const HidIoProtocol,
            _callback: Option<HidIoReportCallback>,
            _context: *mut c_void,
        ) -> efi::Status {
            efi::Status::DEVICE_ERROR
        }
        static CLOSED_EVENTS: AtomicUsize = AtomicUsize::new(0);
        static CLOSED_PROTOCOLS: AtomicUsize = AtomicUsize::new(0);

        let services = mock_services(|protocols, events, _| {
            protocols.expect_open_interface().returning(|_, _, _, _| {
                let protocol = mock_hid_io_protocol();
                protocol.register_report_callback = failing_register;
                Ok(protocol_ptr(protocol))
            });
            protocols.expect_close_interface().returning(|_, _, _, _| {
                CLOSED_PROTOCOLS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            events.expect_close_event().returning(|_| {
                CLOSED_EVENTS.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
        });

        let result = UefiHidIo::new(services, test_handle(0x1), test_handle(0x2), vec![ok_factory()]);
        assert_eq!(result.err(), Some(efi::Status::DEVICE_ERROR));
        assert_eq!(CLOSED_EVENTS.load(Ordering::SeqCst), 1);
        assert_eq!(CLOSED_PROTOCOLS.load(Ordering::SeqCst), 1);
    }
}
