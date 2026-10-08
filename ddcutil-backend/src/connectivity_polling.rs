// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

//! Polling loop (runs in a background thread)
//! Alternative way of detecting connectivity changes and DPMS events.
//! (libddcutil does not handle DPMS and on some hardware cannot detect
//! connectivity changes)

use crate::{ddcutil, is_env_enabled};
use crate::ddcutil::{DisplayManager, DisplayRef, InternalEvent, InternalEventType};

use base64::{engine::general_purpose, Engine as _};
use crossbeam_channel::{unbounded, Receiver, Sender};
use log::{debug, error, info};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone)]
pub struct PollingController {
    state: Arc<Mutex<PollingSharedState>>,
}

impl PollingController {

    pub fn new() -> Self {
        PollingController {
            state: Arc::new(Mutex::new(PollingSharedState::default())),
        }
    }

    pub fn shared_state(&self) -> Arc<Mutex<PollingSharedState>> {
        self.state.clone()
    }

    pub fn start(&self, display_manager: &DisplayManager,) {
        let mut state = self.state.lock().unwrap();
        if state.poll_thread.is_some() {
            debug!("Polling thread already running");
            return;
        }

        // Create an unbounded message channel to receive shutdown messages
        let (shutdown_dispatcher, shutdown_listener) = unbounded();

        let state_arc = self.state.clone();
        let display_manager_clone = display_manager.clone();

        let handle = thread::spawn(move || {
            polling_loop(state_arc, &display_manager_clone, shutdown_listener);
        });

        state.poll_thread = Some(handle);
        state.shutdown_dispatcher = Some(shutdown_dispatcher);
        info!("Polling thread started");
    }

    pub fn stop(&self)  {
        let mut state = self.state.lock().unwrap();
        if let Some(shutdown_dispatcher) = state.shutdown_dispatcher.take() {
            let _ = shutdown_dispatcher.send(());
        }
        if let Some(handle) = state.poll_thread.take() {
            let _ = handle.join();
        }
        info!("Polling thread stopped");
    }

    pub fn enable_events(&self) {
        let mut state = self.state.lock().unwrap();
        state.events_enabled = true;
    }

    pub fn get_interval(&self) -> u32 {
        let state = self.state.lock().unwrap();
        state.poll_interval_secs
    }

    pub fn set_interval(&self, seconds: u32) {
        let mut state = self.state.lock().unwrap();
        state.poll_interval_secs = seconds;
        if seconds == 0 {
            self.stop()
        }
    }

    pub fn get_cascade_seconds(&self) -> f64 {
        let state = self.state.lock().unwrap();
        state.poll_cascade_secs
    }

    pub fn set_cascade_seconds(&self, seconds: f64) {
        let mut state = self.state.lock().unwrap();
        state.poll_cascade_secs = seconds;
      }
}

// ============================================================================
// PollingSharedState – everything protected by the single lock
// ============================================================================

/// All state that must be protected by the single mutex.
/// This includes configuration, polling thread handles, and any other shared data.
///
/// The poll_do_redetect is probably only ever needed if linked against libddcutil
/// version <= 2.1. From 2.2 onward libddcutil events for hotplugging of monitors
/// seems to be reliable for all drivers.  This option is provided in incase there
/// is someone out there that still has issues or wants to use an old libddutil.
pub struct PollingSharedState {
    // Configuration
    pub poll_interval_secs: u32,
    pub poll_cascade_secs: f64,
    pub poll_do_redetect: bool,  // This is probably only ever needed if linked against libddcutil version <= 2.1
    pub events_enabled: bool,
    pub ignore_duplicates: bool,
    // Polling thread management
    pub poll_thread: Option<thread::JoinHandle<()>>,
    pub shutdown_dispatcher: Option<Sender<()>>,
}

impl Default for PollingSharedState {
    fn default() -> Self {
        let poll_do_redetect = is_env_enabled("DDC_CI_POLL_DO_REDETECT", false);
        // Polling can detect some of the same events as libddcutil, but with more info, such
        // as the edid of a display that has disconnected, should duplicates be ignored or forwarded.
        let ignore_duplicates = is_env_enabled("DDC_CI_POLL_IGNORE_DUPLICATES", true);
        Self {
            poll_interval_secs: 30,
            poll_cascade_secs: 0.5,
            poll_do_redetect,
            events_enabled: false,
            ignore_duplicates,
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
    state: Arc<Mutex<PollingSharedState>>,
    display_manager: &DisplayManager,
    shutdown_request_receiver: Receiver<()>,
) {
    debug!("Starting polling loop");

    let mut previous_states: HashMap<String, DisplayState> = HashMap::new();
    let mut initializing = true;

    loop {
        if initializing {
            debug!("polling_loop: Polling loop top initializing");
        }
        
        // Check for shutdown signal
        if shutdown_request_receiver.try_recv().is_ok() {
            info!("Polling thread received shutdown signal, stopping polling thread.");
            break;
        }

        // ---- Acquire the lock and read config ----

        let (interval, cascade, do_redetect, events_enabled, ignore_duplicates) = {
            let cfg = state.lock().unwrap();
            (
                cfg.poll_interval_secs,
                cfg.poll_cascade_secs,
                cfg.poll_do_redetect,
                cfg.events_enabled,
                cfg.ignore_duplicates,
            )
        };

        if interval == 0 {
            info!("Polling interval set to zero, stopping polling thread.");
            break;
        }

        if !events_enabled {
            ddcutil::sleep_interruptible(Duration::from_secs(5));
            debug!("polling_loop: Polling event enabled={}", events_enabled);
            continue;  // TODO - should we not exit now - or might events be re-enabled?
        }

        // ---- Call libddcutil (safe because we hold the lock) ----
        let dg = display_manager.acquire();

        if do_redetect {
            // This code is provided in incase there is someone out there that still
            // has issues with detect or someone who wants to use an old libddutil.
            if let Err(e) = dg.redetect() {
                error!("redetect failed: {}", e);
                ddcutil::sleep_interruptible(Duration::from_secs(interval as u64));
                continue;
            }
        }

        let current_displays = match dg.get_display_info_list(true) {
            Ok(list) => list,
            Err(e) => {
                error!("get_display_info_list failed: {}", e);
                ddcutil::sleep_interruptible(Duration::from_secs(interval as u64));
                continue;
            }
        };

        // Build current state (also needs libddcutil for DPMS check)
        let mut current_states = HashMap::with_capacity(current_displays.len());
        for display in &current_displays {
            let edid = general_purpose::STANDARD.encode(display.edid_bytes);
            let awake = match dg.is_dpms_awake(display.display_ref) {
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
        ;
        // ---- Release the lock before comparing states and sending events ----
        drop(dg);

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
                send_event(ddcutil::build_hotplug_event(lost_edid, InternalEventType::Disconnected), ignore_duplicates);
            }

            for new_edid in newly_detected {
                send_event(ddcutil::build_hotplug_event(new_edid, InternalEventType::Connected), ignore_duplicates);
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
                        send_event(ddcutil::build_dpms_event(edid, event_type), ignore_duplicates);
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

fn send_event(internal_event: InternalEvent, ignore_duplicates: bool) {
    if let Some(native_context) = crate::ddcutil::NATIVE_CONTEXT_INSTANCE.get() {
        let unique = native_context.internal_event_tracker.lock().unwrap().record(internal_event.clone());
        if unique || !ignore_duplicates {
            info!("polling: sending event (unique={}) {:?}", unique, internal_event);
            let _ = native_context.internal_event_sender.send(internal_event);
        } else {
            info!("polling: ignoring duplicate event {:?}", internal_event);
        }
    } else {
        error!("polling: cannot send event: internal event sender not set.");
    }
}
