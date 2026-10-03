// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

//! DdcCiVarlinkService – service implementation

use ddcutil_backend::ddcutil::{InternalEvent};
use ddcutil_backend::{ddcutil, connectivity_polling, is_env_enabled};
use crate::ddc_ci_varlink_subscribers;
use crossbeam_channel::{unbounded, Receiver, Sender};
use log::{debug, error, info};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use ddcutil_backend::connectivity_polling::ServiceSharedState;

pub struct DdcCiVarlinkService {
    /// Single mutex protecting all shared state and libddcutil access.
    pub state: Arc<Mutex<ServiceSharedState>>,
    /// Channel for sending events from the polling thread and native callback.
    internal_event_sender: Sender<ddcutil::InternalEvent>,
    /// If true, configuration‑changing methods are rejected.
    pub configuration_locked: Arc<AtomicBool>,
    pub raw_values: bool,
}

impl DdcCiVarlinkService {
    /// Create a new service instance. Initializes libddcutil and starts the native callback.
    /// Returns a receiver for internal events, other modules should use the receiver
    /// to forward events for dispatch to external varlink subscribers.

    pub const VENDOR: &'static str = "digitaltrails";
    pub const PRODUCT: &'static str = "ddc-ci-varlink";
    pub const VERSION: &'static str ="1.0.0";
    pub const PRODUCT_URL: &'static str = "https://github.com/digitaltrails/ddc-ci-daemons";
    pub const FALLBACK_SOCKET_FILENAME: &'static str = "ddc-ci-varlink.socket";

    pub fn new() -> Self {

        // Initialize libddcutil
        ddcutil::init().expect("ddcutil init failed"); // TODO suspect?

        // Create event channel
        let (internal_event_sender, internal_event_receiver) = unbounded();

        // Store the sender globally for the native C callback
        ddcutil::set_internal_event_sender(internal_event_sender.clone()).unwrap();

        // Register the native callback (C callback)
        if let Err(status) = ddcutil::register_callback(Some(ddcutil::native_ddc_event_callback)) {
            error!("Failed to register ddcutil event callback: {:?}", status)
        };

        let service = Self {
            state: Arc::new(Mutex::new(ServiceSharedState::default())),
            internal_event_sender,
            configuration_locked: Arc::new(AtomicBool::new(false)),
            raw_values: is_env_enabled("DDC_CI_SIMPLE_RAW_VALUES", true)
        };

        // InternalEvents are forwarded to the subscribers module which converts 
        // them to external varlink events and dispatches them to
        // external subscribers.
        std::thread::spawn(move || {
            info!("Started thread to broadcast internal events to subscribers.");
            // This will loop reading events and forwarding to varlink subscribers
            ddc_ci_varlink_subscribers::forward_to_all_subscribers(internal_event_receiver);
        });

        service
    }

    // ----- Subscriptions control -----

    pub fn subscribe_to_internal_events(event_sender: Sender<InternalEvent>) -> usize {
        ddc_ci_varlink_subscribers::subscribe_to_internal_events(event_sender)
    }

    pub fn unsubscribe_from_events(id: usize) {
        ddc_ci_varlink_subscribers::unsubscribe_from_events(id)
    }

    pub fn broadcast_set_vcp(
        display_number: Option<i64>,
        edid_base64: Option<&str>,
        vcp_code: i64,
        new_value: i64,
        client_context: Option<String>,
    ) {
        let internal_event = ddcutil::build_vcp_changed_event(
            display_number,
            edid_base64,
            vcp_code,
            new_value,
            client_context.unwrap_or_default(),
        );
        ddc_ci_varlink_subscribers::broadcast_to_subscribers(internal_event);
    }

    // ----- Polling control -----

    /// Start the polling thread if it's not already running.
    pub fn start_polling(&self) {
        let mut state = self.state.lock().unwrap();
        if state.poll_thread.is_some() {
            debug!("Polling thread already running");
            return;
        }

        // Create an unbounded message channel to receive shutdown messages
        let (shutdown_dispatcher, shutdown_listener) = unbounded();

        let state_arc = self.state.clone();
        let internal_event_sender = self.internal_event_sender.clone();

        let handle = thread::spawn(move || {
            connectivity_polling::polling_loop(state_arc, internal_event_sender, shutdown_listener);
        });

        state.poll_thread = Some(handle);
        state.shutdown_dispatcher = Some(shutdown_dispatcher);
        info!("Polling thread started");
    }

    /// Stop the polling thread if it's running.
    pub fn stop_polling(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(shutdown_dispatcher) = state.shutdown_dispatcher.take() {
            let _ = shutdown_dispatcher.send(());
        }
        if let Some(handle) = state.poll_thread.take() {
            let _ = handle.join();
        }
        info!("Polling thread stopped");
    }

    /// Enable or disable event watching. Calls libddcutil to start/stop watching.
    /// # Safety
    /// This calls unsafe FFI functions. The caller must hold the lock.
    pub fn set_events_enabled(&self, enable: bool) -> varlink::Result<()> {
        let mut state = self.state.lock().unwrap();
        if enable == state.events_enabled && enable {
            debug!("Events for libddcutil already {}.", {
                if state.events_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            });
        } else {
            state.events_enabled = enable;
            if enable {
                ddcutil::start_watch_displays()?;
                debug!("Enabled libddcutil events.");
            } else {
                ddcutil::stop_watch_displays()?;
                debug!("Disabled libddcutil events.");
            }
        }
        Ok(())
    }
}

