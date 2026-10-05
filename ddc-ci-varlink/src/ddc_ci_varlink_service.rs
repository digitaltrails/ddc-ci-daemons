// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

//! DdcCiVarlinkService – service implementation

use ddcutil_backend::ddcutil::{DisplayManager, InternalEvent};
use ddcutil_backend::{ddcutil, is_env_enabled};
use crate::ddc_ci_varlink_subscribers;
use crossbeam_channel::{unbounded, Sender};
use log::{error, info};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc};

use ddcutil_backend::connectivity_polling::{PollingController};

pub struct DdcCiVarlinkService {
    pub display_manager: DisplayManager,
    pub polling_controller: PollingController,
    pub configuration_locked: Arc<AtomicBool>,
    pub raw_values: bool,
}

impl DdcCiVarlinkService {

    pub const VENDOR: &'static str = "digitaltrails";
    pub const PRODUCT: &'static str = "ddc-ci-varlink";
    pub const VERSION: &'static str ="1.0.0";
    pub const PRODUCT_URL: &'static str = "https://github.com/digitaltrails/ddc-ci-daemons";
    pub const FALLBACK_SOCKET_FILENAME: &'static str = "ddc-ci-varlink.socket";

    /// Create a new service instance. Initializes libddcutil and starts the native callback.
    /// Returns a receiver for internal events, other modules should use the receiver
    /// to forward events for dispatch to external varlink subscribers.
    pub fn new() -> Self {

        // Create event channel
        let (internal_event_sender, internal_event_receiver) = unbounded();

        let display_manager = DisplayManager::new(internal_event_sender.clone()).expect("ddcutil::new failed");
        let polling_controller = PollingController::new(display_manager.clone(), internal_event_sender.clone());
        let service = Self {
            display_manager,
            polling_controller,
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
}

