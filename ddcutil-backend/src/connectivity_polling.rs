// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

//! Polling loop (runs in a background thread)
//! Alternative way of detecting connectivity changes and DPMS events.
//! (libddcutil does not handle DPMS and on some hardware cannot detect
//! connectivity changes)

use crate::ddcutil;
use crate::ddcutil::{
    DisplayRef,
    InternalEvent,
    InternalEventType,
};

use base64::{engine::general_purpose, Engine as _};
use crossbeam_channel::{Receiver, Sender};
use log::{debug, error, info};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;


// ============================================================================
// ServiceState – everything protected by the single lock
// ============================================================================

/// All state that must be protected by the single mutex.
/// This includes configuration, polling thread handles, and any other shared data.
///
/// The poll_do_redetect is probably only ever needed if linked against libddcutil
/// version <= 2.1. From 2.2 onward libddcutil events for hotplugging of monitors
/// seems to be reliable for all drivers.  This option is provided in incase there
/// is someone out there that still has issues or wants to use an old libddutil.
pub struct ServiceSharedState {
    // Configuration
    pub poll_interval_secs: u32,
    pub poll_cascade_secs: f64,
    pub poll_do_redetect: bool,  // This is probably only ever needed if linked against libddcutil version <= 2.1
    pub events_enabled: bool,
    // Polling thread management
    pub poll_thread: Option<thread::JoinHandle<()>>,
    pub shutdown_dispatcher: Option<Sender<()>>,
}

impl Default for ServiceSharedState {
    fn default() -> Self {
        let poll_do_detect = std::env::var("DDC_CI_POLL_DO_REDETECT")
        .map(|val| val.to_lowercase() == "true" || val == "1")
        .unwrap_or(false); // Fallback default if env var is not set
        info!("Environment variable DDC_CI_POLL_DO_REDETECT={} (not needed for libddcutil >= 2.2)",
              poll_do_detect);
        Self {
            poll_interval_secs: 30,
            poll_cascade_secs: 0.5,
            poll_do_redetect: poll_do_detect,
            events_enabled: false,
            poll_thread: None,
            shutdown_dispatcher: None,
        }
    }
}



/// State of a single display for the polling loop.
#[derive(Debug, Clone, Copy)]
struct DisplayState {
    #[allow(dead_code)]
    display_ref: DisplayRef, // for potential future use
    awake: bool,
}

/// The main polling loop. Runs in its own thread.
pub fn polling_loop(
    state: Arc<Mutex<ServiceSharedState>>,
    internal_event_sender: Sender<InternalEvent>,
    shutdown_request_receiver: Receiver<()>,
) {
    debug!("Starting polling loop");

    let mut previous_states: HashMap<String, DisplayState> = HashMap::new();
    let mut initializing = true;

    loop {
        if initializing {
            debug!("polling_loop: Polling loop top initilizing");
        }
        
        // Check for shutdown signal
        if shutdown_request_receiver.try_recv().is_ok() {
            info!("Polling thread received shutdown signal, stopping polling thread.");
            break;
        }

        // ---- Acquire the lock and read config ----
        let guard = state.lock().unwrap();
        let (interval, cascade, do_redetect, events_enabled) = {
            let cfg = &*guard;
            (
                cfg.poll_interval_secs,
                cfg.poll_cascade_secs,
                cfg.poll_do_redetect,
                cfg.events_enabled,
            )
        };

        if interval == 0 {
            info!("Polling interval set to zero, stopping polling thread.");
            break;
        }

        if !events_enabled {
            drop(guard);
            ddcutil::sleep_interruptible(Duration::from_secs(5));
            debug!("polling_loop: Polling event enabled={}", events_enabled);
            continue;  // TODO - should we not exit now - or might events be re-enabled?
        }

        // ---- Call libddcutil (safe because we hold the lock) ----


        if do_redetect {
            // This code is provided in incase there is someone out there that still
            // has issues with detect or someone who wants to use an old libddutil.
            if let Err(e) = ddcutil::redetect() {
                error!("redetect failed: {}", e);
                drop(guard);
                ddcutil::sleep_interruptible(Duration::from_secs(interval as u64));
                continue;
            }
        }

        let current_displays = match ddcutil::get_display_info_list(true) {
            Ok(list) => list,
            Err(e) => {
                error!("get_display_info_list failed: {}", e);
                drop(guard);
                ddcutil::sleep_interruptible(Duration::from_secs(interval as u64));
                continue;
            }
        };

        // Build current state (also needs libddcutil for DPMS check)
        let mut current_states = HashMap::with_capacity(current_displays.len());
        for display in &current_displays {
            let edid = general_purpose::STANDARD.encode(display.edid_bytes);
            let awake = match ddcutil::is_dpms_awake(display.display_ref) {
                Ok(a) => a,
                Err(e) => {
                    debug!(
                        "DPMS query failed for display {}: {}",
                        display.display_number, e
                    );
                    false  // assume its asleep.
                }
            };
            current_states.insert(
                edid,
                DisplayState {
                    display_ref: display.display_ref,
                    awake,
                },
            );
        }

        // ---- Release the lock before comparing states and sending events ----
        drop(guard);

        // Compare states (no lock needed)
        let current_edids: HashSet<_> = current_states.keys().collect();
        let previous_edids: HashSet<_> = previous_states.keys().collect();

        let some_newly_detected = current_edids.difference(&previous_edids).next().is_some();
        let some_lost = previous_edids.difference(&current_edids).next().is_some();
        let connection_change = some_newly_detected || some_lost;
        let newly_detected: Vec<_> = current_edids.difference(&previous_edids).cloned().collect();
        let lost_connection: Vec<_> = previous_edids.difference(&current_edids).cloned().collect();

        if !initializing {
            for lost_edid in lost_connection {
                let internal_event = ddcutil::build_hotplug_event(lost_edid, InternalEventType::Disconnected);
                info!("poll: sending connection change event {:?}", internal_event);
                let _ = internal_event_sender.send(internal_event);
            }

            for new_edid in newly_detected {
                let internal_event = ddcutil::build_hotplug_event(new_edid, InternalEventType::Connected);
                info!("poll: sending connection change event {:?}", internal_event);
                let _ = internal_event_sender.send(internal_event);
            }

            // Detect DPMS changes
            for (edid, state) in &current_states {
                if let Some(prev_state) = previous_states.get(edid) {
                    if prev_state.awake != state.awake {
                        let event_type = if state.awake {
                            InternalEventType::DpmsAwake
                        } else {
                            InternalEventType::DpmsAsleep
                        };
                        let internal_event = ddcutil::build_dpms_event(edid, event_type);
                        debug!("poll: sending DPMS change event {:?}", internal_event);
                        let _ = internal_event_sender.send(internal_event);
                    }
                }
            }
        }

        previous_states = current_states;
        initializing = false;

        // Sleep without holding the lock
        let sleep_duration = if connection_change {
            Duration::from_millis((cascade * 1000.0) as u64)
        } else {
            Duration::from_secs(interval as u64)
        };
        ddcutil::sleep_interruptible(sleep_duration);
    }
}
