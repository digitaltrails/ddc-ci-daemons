// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

//! D-Bus interface to libddcutil.
//! 
//! The name Ddcu is an internal naming convention, deliberately
//! different from ddcutil to help with delimiting internal code
//! boundaries.

use strum::IntoEnumIterator;
use base64::Engine;
use base64::engine::general_purpose;
use ddcutil_backend::{connectivity_polling, ddcutil};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use strum::{EnumIter, Display};
use num_enum::TryFromPrimitive;
use log::{debug, error, info};
use crossbeam_channel::{unbounded, Receiver, Sender};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use ddcutil_backend::connectivity_polling::ServiceSharedState;
use ddcutil_backend::ddcutil::{CapabilitiesData, InternalEvent, InternalEventKind, VcpFeatureMetadata};

const DETECT_ALL: u32 = 8;
const EDID_PREFIX_ALLOWED: u32 = 1;
const NO_VERIFY: u32 = 4;


#[derive(Debug, Clone, Copy, EnumIter, Display)]
pub enum DdcCiDbusDisplayEventType {
    DpmsAwake = 0,
    DpmsAsleep = 1,
    DisplayConnected = 2,
    DisplayDisconnected = 3,
}

/// The main service object. Holds all state and (eventually).
pub struct DdcCiDbusService {
    pub(crate) dynamic_sleep: bool,
    pub(crate) output_level: u32,
    /// Single mutex protecting all shared state
    pub state: Arc<Mutex<ServiceSharedState>>,
    /// Channel for sending events from the polling thread and native callback.
    internal_event_sender: Sender<InternalEvent>,
}

// ── Private helpers (not part of the D-Bus interface) ──────────────
impl DdcCiDbusService {
    /// Shared body for `detect` and `list_detected`.
    ///
    /// When `force_redetect` is true, a rescan is triggered before listing.
    /// Returns `(status, displays, error_status, error_message)`.

    /// Well-known bus name this service requests.
    pub const SERVICE_NAME: &'static str = "local.ddc-ci.DdcCiService";

    /// Object path where the interface is served.
    pub const OBJECT_PATH: &'static str = "/local/ddc_ci/DdcCiObject";

    pub const INTERFACE_NAME: &'static str = "local.ddc_ci.DdcCiInterface";

    pub fn new() -> (Self, Receiver<ddcutil::InternalEvent>) {

        ddcutil::init().expect("ddcutil init failed");  // TODO suspect?

        let (internal_event_sender, internal_event_receiver) = unbounded();

        // Store the sender globally for the native C callback
        ddcutil::set_internal_event_sender(internal_event_sender.clone()).unwrap();

        // Register the native callback (C callback)
        if let Err(status) = ddcutil::register_callback(Some(ddcutil::native_ddc_event_callback)) {
            error!("Failed to register ddcutil event callback: {:?}", status)
        };

        let service = Self {
            dynamic_sleep: false,
            output_level: 0,
            state: Arc::new(Mutex::new(ServiceSharedState::default())),
            internal_event_sender: internal_event_sender.clone(),
        };
        (service, internal_event_receiver)
    }

    fn list_displays_impl(
        &self,
        flags: u32,
        detect: bool,
    ) -> (
        i32,
        Vec<(i32, i32, i32, String, String, String, u16, String, u32)>,
        i32,
        String,
    ) {
        let ddc_operation = || -> Result<
            (
                i32,
                Vec<(i32, i32, i32, String, String, String, u16, String, u32)>,
                i32,
                String,
            ),
            ddcutil::Error> {

            if detect {
                ddcutil::redetect()?;
            }
            let list = ddcutil::list_displays(flags & DETECT_ALL != 0)?;

            let info_vec: Vec<_> = list
                .into_iter()
                .map(|disp| {
                    (
                        disp.display_number, disp.usb_bus, disp.usb_device,
                        disp.manufacturer_id, disp.model_name, disp.serial_number, disp.product_code,
                        general_purpose::STANDARD.encode(disp.edid_bytes),
                        ddcutil::edid_serial_number(&disp.edid_bytes),
                    )
                })
                .collect();

            Ok((info_vec.len() as i32, info_vec, 0, "OK".to_string()))
        };

        let op_name = if detect { "Detect" } else { "ListDetected" };

        match ddc_operation() {
            Ok((number_of_displays, info_vec, status, message)) => (
                number_of_displays, info_vec, status, message
            ),
            Err(e) => (0, Vec::new(), error_code(&e), error_message(op_name, &e)),
        }
    }

    fn convert_capabilities_data(
        data: ddcutil::CapabilitiesData,
    ) -> (
        String,
        u8,
        u8,
        HashMap<u8, String>,
        HashMap<u8, (String, String, HashMap<u8, String>)>,
        i32,
        String,
    ) {
        let commands = data
            .commands
            .into_iter()
            .map(|cmd| (cmd.code as u8, cmd.description))
            .collect();

        let capabilities: HashMap<u8, (String, String, HashMap<u8, String>)> = data
            .features
            .into_iter()
            .map(|feature| {
                // Inner map: a{ys} -> HashMap<u8, String>
                let values: HashMap<u8, String> = feature
                    .values
                    .into_iter()
                    .map(|val| (val.code as u8, val.name)) // Keep as u8 instead of formatting to String
                    .collect();

                (
                    feature.code as u8, // Keep as u8 instead of formatting to String
                    (
                        feature.name,
                        feature.description,
                        values,
                    ),
                )
            })
            .collect();

        (
            data.model_name,
            data.mccs_major,
            data.mccs_minor,
            commands,
            capabilities,
            0,
            "OK".to_string(),
        )
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

}

#[interface(name = "local.ddc_ci.DdcCiInterface")]
impl DdcCiDbusService {
    // ── Methods ────────────────────────────────────────────────────────

    /// Restarts the service.
    fn restart(
        &mut self,
        text_options: &str,
        syslog_level: u32,
        flags: u32,
    ) -> (i32, String) {
        // TODO:
        println!(
            "Restart called: options={}, level={}, flags={}",
            text_options, syslog_level, flags
        );
        (0, "OK".to_string(),)
    }

    /// Detects connected displays.
    fn detect(
        &self,
        flags: u32,
    ) -> (
        i32,
        Vec<(i32, i32, i32, String, String, String, u16, String, u32)>,
        i32,
        String,
    ) {
        self.list_displays_impl(flags, true)
    }

    /// Lists already-detected displays.
    fn list_detected(
        &self,
        flags: u32,
    ) -> (
        i32,
        Vec<(i32, i32, i32, String, String, String, u16, String, u32)>,
        i32,
        String,
    ) {
        self.list_displays_impl(flags, false)
    }

    /// Gets a single VCP value.
    fn get_vcp(
        &self,
        display_number: i32,
        edid_txt: &str,
        vcp_code: u8,
        flags: u32,
    ) -> (u16, u16, String, i32, String) {

        let ddc_operation = || -> Result<(u16, u16, String, i32, String), ddcutil::Error> {
            let dref = ddcutil::find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & EDID_PREFIX_ALLOWED != 0,
            )?;
            let handle = ddcutil::open_display(dref)?;
            let (current, max, formatted) = ddcutil::get_vcp(&handle, vcp_code)?;
            Ok((current as u16, max as u16, formatted, 0, "OK".to_string()))
        };

        match ddc_operation() {
            Ok((current, max, formatted, status, message)) => (current, max, formatted, status, message),
            Err(e) => (0, 0, String::new(), error_code(&e), error_message("GetVcp", &e)),
        }
    }

    /// Gets multiple VCP values in one call.
    fn get_multiple_vcp(
        &self,
        display_number: i32,
        edid_txt: &str,
        vcp_codes: &[u8],
        flags: u32,
    ) -> (Vec<(u8, u16, u16, String)>, i32, String) {

        let ddc_operation = || -> Result<(Vec<(u8, u16, u16, String)>, i32, String), ddcutil::Error> {
            let dref = ddcutil::find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & EDID_PREFIX_ALLOWED != 0,
            )?;
            let handle = ddcutil::open_display(dref)?;
            let mut values = Vec::new();
            for &code in vcp_codes {
                let (current, max, formatted) = ddcutil::get_vcp(&handle, code as u8)?;
                values.push((code, current as u16, max as u16, formatted));
            }
            Ok((values, 0, "OK".to_string()))
        };

        match ddc_operation() {
            Ok((values, status, message)) => (values, status, message),
            Err(e) => (vec![], error_code(&e), error_message("GetMultipleVcp", &e)),
        }
    }

    /// Sets a VCP value.
    fn set_vcp(
        &mut self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        display_number: i32,
        edid_txt: &str,
        vcp_code: u8,
        vcp_new_value: u16,
        flags: u32,
    ) -> (i32, String) {
        // Delegate directly by passing "" for the context string.
        // We pass the emitter through cleanly by value.
        self.set_vcp_with_context(
            hdr,
            emitter,
            display_number,
            edid_txt,
            vcp_code,
            vcp_new_value,
            "",
            flags,
        )
    }

    /// Sets a VCP value with a client context string.
    fn set_vcp_with_context(
        &mut self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        display_number: i32,
        edid_txt: &str,
        vcp_code: u8,
        vcp_new_value: u16,
        client_context: &str,
        flags: u32,
    ) -> (i32, String) {

        let ddc_operation = || -> Result<(i32, String), ddcutil::Error> {
            let dref = ddcutil::find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & EDID_PREFIX_ALLOWED != 0,
            )?;
            let handle = ddcutil::open_display(dref)?;
            ddcutil::set_vcp(&handle, vcp_code as u8, vcp_new_value, flags & NO_VERIFY != 0)?;

            let sender_str: String = hdr.sender()
                .map(|name| name.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let _ = zbus::block_on(async { Self::vcp_value_changed(
                &emitter,
                display_number,
                edid_txt,
                vcp_code,
                vcp_new_value,
                &sender_str,
                client_context,
                flags,
            ).await}).map_err(|e| eprintln!("SetVcp: error on signaling change {}", e));

            Ok((0, "OK".to_string()))
        };

        match ddc_operation() {
            Ok((status, message)) => (status, message),
            Err(e) => (error_code(&e), error_message("SetVcpWithContext", &e)),
        }
    }

    /// Gets metadata for a VCP code.
    fn get_vcp_metadata(
        &self,
        display_number: i32,
        edid_txt: &str,
        vcp_code: u8,
        flags: u32,
    ) -> (String, String, bool, bool, bool, bool, bool, i32, String) {

        let ddc_operation = || -> Result<(VcpFeatureMetadata, i32, String), ddcutil::Error> {
            let dref = ddcutil::find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & EDID_PREFIX_ALLOWED != 0,
            )?;
            let handle = ddcutil::open_display(dref)?;
            let metadata = ddcutil::get_vcp_metadata(&handle, vcp_code.into())?;
            Ok((metadata, 0, "OK".to_string()))
        };

        match ddc_operation() {
            Ok((metadata, status, message)) => (
                metadata.feature_name,
                metadata.description,
                metadata.is_read_only,
                metadata.is_write_only,
                metadata.is_rw,
                metadata.is_complex,
                metadata.is_continuous,
                status,
                message,),
            Err(e) => (
                "Feature".into(),
                "Description".into(),
                false,
                false,
                true,
                false,
                false,
                error_code(&e),
                error_message("GetVcp", &e)),
        }
    }

    /// Gets the capabilities string for a display.
    fn get_capabilities_string(
        &self,
        display_number: i32,
        edid_txt: &str,
        flags: u32,
    ) -> (String, i32, String) {

        let ddc_operation = || -> Result<(String, i32, String), ddcutil::Error> {
            let dref = ddcutil::find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & EDID_PREFIX_ALLOWED != 0,
            )?;
            let handle = ddcutil::open_display(dref)?;
            let caps_str = ddcutil::get_capabilities_string(&handle)?;
            Ok((caps_str, 0, "OK".to_string()))
        };

        match ddc_operation() {
            Ok((caps_str, status, message)) => (caps_str, status, message),
            Err(e) => (String::new(), error_code(&e), error_message("GetCapabilitiesString", &e)),
        }
    }

    /// Gets parsed capabilities metadata.
    fn get_capabilities_metadata(
        &self,
        display_number: i32,
        edid_txt: &str,
        flags: u32,
    ) -> (
        String,
        u8,
        u8,
        HashMap<u8, String>,
        HashMap<u8, (String, String, HashMap<u8, String>)>,
        i32,
        String,
    ) {

        let ddc_operation = || -> Result<CapabilitiesData, ddcutil::Error> {
            let dref = ddcutil::find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & EDID_PREFIX_ALLOWED != 0,
            )?;
            let handle = ddcutil::open_display(dref)?;
            let caps_data = ddcutil::get_capabilities_data(handle)?;
            Ok(caps_data)
        };

        match ddc_operation() {
            Ok(caps_data) => Self::convert_capabilities_data(caps_data),
            Err(e) => (
                String::new(),
                0,
                0,
                HashMap::new(),
                HashMap::new(),
                error_code(&e),
                error_message("GetCapabilitiesMetadata", &e),
            ),
        }
    }

    /// Gets the current state of a display.
    fn get_display_state(
        &self,
        display_number: i32,
        edid_txt: &str,
        flags: u32,
    ) -> (i32, String) {

        match ddcutil::get_display_state(
            Option::Some(display_number.into()),
            Option::Some(edid_txt),
            flags & EDID_PREFIX_ALLOWED != 0,
        ) {
            Ok((status, text)) => (status, text),
            Err(e) => (error_code(&e), error_message("getDisplayState", &e)),
        }
    }

    /// Gets the current sleep multiplier.
    fn get_sleep_multiplier(
        &self,
        display_number: i32,
        edid_txt: &str,
        flags: u32,
    ) -> (f64, i32, String) {

        let ddc_operation = || -> Result<(f64, i32, String), ddcutil::Error> {
            let dref = ddcutil::find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & EDID_PREFIX_ALLOWED != 0,
            )?;
            let multiplier = ddcutil::get_sleep_multiplier(dref)?;
            Ok((multiplier, 0, "OK".to_string()))
        };

        match ddc_operation() {
            Ok((multiplier, status, message)) => (multiplier, status, message),
            Err(e) => (0.0, error_code(&e), error_message("GetSleepMultiplier", &e)),
        }
    }

    /// Sets the sleep multiplier.
    fn set_sleep_multiplier(
        &mut self,
        display_number: i32,
        edid_txt: &str,
        new_multiplier: f64,
        flags: u32,
    ) -> (i32, String) {
        let ddc_operation = || -> Result<(i32, String), ddcutil::Error> {
            let dref = ddcutil::find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & EDID_PREFIX_ALLOWED != 0,
            )?;
            ddcutil::set_sleep_multiplier(dref, new_multiplier)?;
            Ok((0, "OK".to_string()))
        };

        match ddc_operation() {
            Ok((status, message)) => (status, message),
            Err(e) => (error_code(&e), error_message("SetSleepMultiplier", &e)),
        }
    }

    // ── Signals ────────────────────────────────────────────────────────
    // Signals must be declared async, even with the blocking API.
    // The `&SignalEmitter<'_>` first parameter is mandatory.

    #[zbus(signal)]
    pub async fn connected_displays_changed(
        signal_emitter: &SignalEmitter<'_>,
        edid_txt: &str,
        event_type: i32,
        flags: u32,
    ) -> zbus::Result<()> {}

    #[zbus(signal)]
    async fn vcp_value_changed(
        signal_emitter: &SignalEmitter<'_>,
        display_number: i32,
        edid_txt: &str,
        vcp_code: u8,
        vcp_new_value: u16,
        source_client_name: &str,
        source_client_context: &str,
        flags: u32,
    ) -> zbus::Result<()> {}

    #[zbus(signal)]
    async fn service_initialized(
        signal_emitter: &SignalEmitter<'_>,
        flags: u32,
    ) -> zbus::Result<()> {}

    // ── Properties ─────────────────────────────────────────────────────

    #[zbus(property)]
    fn attributes_returned_by_detect(&self) -> Vec<String> {
        vec![]
    }

    #[zbus(property)]
    fn status_values(&self) -> HashMap<i32, String> {
        HashMap::new()
    }

    #[zbus(property)]
    fn ddcutil_version(&self) -> &str {
        "0.0.0"
    }

    #[zbus(property)]
    fn ddcutil_dynamic_sleep(&self) -> bool {
        self.dynamic_sleep
    }

    #[zbus(property)]
    fn set_ddcutil_dynamic_sleep(&mut self, value: bool) {
        self.dynamic_sleep = value;
    }

    #[zbus(property)]
    fn ddcutil_output_level(&self) -> u32 {
        self.output_level
    }

    #[zbus(property)]
    fn set_ddcutil_output_level(&mut self, value: u32) {
        self.output_level = value;
    }

    #[zbus(property)]
    fn display_event_types(&self) -> HashMap<i32, String> {
        DdcCiDbusDisplayEventType::iter().map(|f| (f as i32, f.to_string())).collect()
    }

    #[zbus(property)]
    fn service_interface_version(&self) -> &str {
        "1.0.0"
    }

    #[zbus(property)]
    fn service_info_logging(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn set_service_info_logging(&mut self, _value: bool) {}

    #[zbus(property)]
    fn service_emit_connectivity_signals(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn set_service_emit_connectivity_signals(&mut self, enable: bool) {
        if enable {
            match ddcutil::start_watch_displays() {
                Ok(()) => debug!("Enabled libddcutil watch_displays."),
                Err(e) => error!("Failed to enable libddcutil start_watch_displays: {:?}", e),
            }
        } else {
            match ddcutil::start_watch_displays() {
                Ok(()) => debug!("Disabled libddcutil watch_displays."),
                Err(e) => error!("Failed to disable libddcutil watch_displays: {:?}", e),
            }
        }
    }

    #[zbus(property)]
    fn service_emit_signals(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn set_service_emit_signals(&mut self, _value: bool) {}

    #[zbus(property)]
    fn service_flag_options(&self) -> HashMap<i32, String> {
        HashMap::new()
    }

    #[zbus(property)]
    fn service_parameters_locked(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn service_poll_interval(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn set_service_poll_interval(&mut self, _value: u32) {}

    #[zbus(property)]
    fn service_poll_cascade_interval(&self) -> f64 {
        0.0
    }

    #[zbus(property)]
    fn set_service_poll_cascade_interval(&mut self, _value: f64) {}
}


fn error_code(e: &ddcutil::Error) -> i32 {
    let code: i32 = e.status_code().try_into().unwrap_or(0);
    return code;
}

fn error_message(prefix: &str, e: &ddcutil::Error) -> String {
    return format!("{}: {}", prefix, e);
}
