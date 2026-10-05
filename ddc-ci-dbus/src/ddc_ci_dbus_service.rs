// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

//! D-Bus interface to libddcutil.
//! 
//! The name DdcCi is an internal naming convention, deliberately
//! different from ddcutil to help with delimiting internal code
//! boundaries.

use base64::Engine;
use base64::engine::general_purpose;
use crossbeam_channel::{Receiver, unbounded};
use ddcutil_backend::connectivity_polling::{PollingController};
use ddcutil_backend::ddcutil::{CapabilitiesData, InternalEvent, VcpFeatureMetadata, extract_edid_base64};
use ddcutil_backend::{ddcutil, is_env_enabled};
use log::{LevelFilter, debug, error, info};
use std::collections::HashMap;
use std::error::Error;
use std::sync::{OnceLock};
use strum::IntoEnumIterator;
use strum::{Display, EnumIter};
use zbus::blocking::{Connection, connection};
use zbus::fdo::Error as FdoError;
use zbus::interface;
use zbus::object_server::SignalEmitter;

#[derive(Debug, Clone, Copy, EnumIter, Display)]
#[repr(u32)]
pub enum ServiceFlags {
    EdidPrefixAllowed = 1,
    /// Deprecated - use environment variable DDC_CI_RETURN_RAW_VALUES.
    ReturnRawValues = 2,
    NoVerify = 4,
    DetectAll = 8,
}

#[derive(Debug, Clone, Copy, EnumIter, Display)]
pub enum DdcCiDbusDisplayEventType {
    DpmsAwake = 0,
    DpmsAsleep = 1,
    DisplayConnected = 2,
    DisplayDisconnected = 3,
}

/// The main service object. Holds all state and (eventually).
pub struct DdcCiDbusService {
    pub(crate) display_manager: ddcutil::DisplayManager,
    pub(crate) polling_controller: PollingController,
    pub(crate) parameters_locked: bool,
    pub(crate) connectivity_signals_enabled: bool,
    /// Callers can elect to drop the high byte of simple non-continuous types.
    /// For simple non-continuous types the high byte may be garbage for some models of VDU.
    pub raw_values: bool,

}


// ── Private helpers (not part of the D-Bus interface) ──────────────
impl DdcCiDbusService {

    /// Well-known bus name this service requests.
    pub const SERVICE_NAME: &'static str = "local.ddc-ci.DdcCiService";

    /// Object path where the interface is served.
    pub const OBJECT_PATH: &'static str = "/local/ddc_ci/DdcCiObject";

    pub const DETECT_ATTRIBUTES: &[&str] = &["display_number", "usb_bus", "usb_device",
        "manufacturer_id", "model_name", "serial_number", "product_code",
        "edid_txt", "binary_serial_number",];

    pub fn new() -> (Self, Receiver<InternalEvent>) {

        let (internal_event_sender, internal_event_receiver) = unbounded();

        let display_manager = ddcutil::DisplayManager::new(internal_event_sender.clone()).expect("dg.new failed");
        let polling_controller = PollingController::new(display_manager.clone(), internal_event_sender.clone());
        let service = Self {
            display_manager,
            polling_controller,
            parameters_locked: is_env_enabled("DDC_CI_PARAMETERS_LOCKED", false),
            connectivity_signals_enabled: is_env_enabled("DDC_CI_CONNECTIVITY_SIGNALS", true),
            raw_values: is_env_enabled("DDC_CI_SIMPLE_RAW_VALUES", true),
        };

        // Register the native callback (C callback).
        // Must run after init()/DisplayManager::new(). Calling it earlier leaves watching disabled
        // and start_watch_displays fails with -3014.
        if let Err(status) = ddcutil::register_callback(Some(ddcutil::native_ddc_event_callback)) {
            error!("Failed to register ddcutil event callback: {:?}", status)
        };

        (service, internal_event_receiver)
    }

    pub fn serve_clients(self) -> zbus::Result<Connection> {
        // Blocking builder to bind and serve the interface
        let result = connection::Builder::session()?
            .name(Self::SERVICE_NAME)?
            .serve_at(Self::OBJECT_PATH, self)?
            .build();
        result
    }

    pub fn start_event_monitoring(&self) {
        let dg = self.display_manager.acquire();
        if is_env_enabled("DDC_CI_WATCH_DISPLAYS", true) {

            match dg.start_watch_displays() {
                Ok(()) => info!("Enabled libddcutil watch_displays."),
                Err(e) => error!("Failed to enable libddcutil watch_displays, continuing anyway: {:?}", e),
            }
        }
        if is_env_enabled("DDC_CI_POLL_DISPLAYS", true) {
            self.polling_controller.enable_events();
            debug!("DDC_CI_POLL_DISPLAYS enabled");
            self.polling_controller.start()
        }
    }

    /// Shared body for `detect` and `list_detected`.
    ///
    /// When `force_redetect` is true, a rescan is triggered before listing.
    /// Returns `(status, displays, error_status, error_message)`.
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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<
            (
                i32,
                Vec<(i32, i32, i32, String, String, String, u16, String, u32)>,
                i32,
                String,
            ),
            ddcutil::Error> {

            if detect {
                dg.redetect()?;
            }
            let list = dg.list_displays(flags & ServiceFlags::DetectAll as u32 != 0)?;

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
        data: CapabilitiesData,
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
            .map(|cmd| (cmd.code, cmd.description))
            .collect();

        let capabilities: HashMap<u8, (String, String, HashMap<u8, String>)> = data
            .features
            .into_iter()
            .map(|feature| {
                // Inner map: a{ys} -> HashMap<u8, String>
                let values: HashMap<u8, String> = feature
                    .values
                    .into_iter()
                    .map(|val| (val.code, val.name)) // Keep as u8 instead of formatting to String
                    .collect();

                (
                    feature.code, // Keep as u8 instead of formatting to String
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
}

const DDC_CI_DBUS_SERVICE_VERSION: &str = "1.0.0";

#[interface(name = "local.ddc_ci.DdcCiInterface", spawn = false)]
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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<(u16, u16, String, i32, String), ddcutil::Error> {
            let dref = dg.find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & ServiceFlags::EdidPrefixAllowed as u32 as u32 != 0,
            )?;
            let handle = dg.open_display(dref)?;
            let want_raw_values = self.raw_values || flags & ServiceFlags::ReturnRawValues as u32 as u32 != 0;
            let (current, max, formatted) = dg.get_vcp(&handle, vcp_code, want_raw_values)?;
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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<(Vec<(u8, u16, u16, String)>, i32, String), ddcutil::Error> {
            let dref = dg.find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & ServiceFlags::EdidPrefixAllowed as u32 != 0,
            )?;
            let want_raw_values = self.raw_values || flags & ServiceFlags::ReturnRawValues as u32 as u32 != 0;
            let handle = dg.open_display(dref)?;
            let mut values = Vec::new();
            for &code in vcp_codes {
                let (current, max, formatted) = dg.get_vcp(&handle, code as u8, want_raw_values)?;
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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<(i32, String), ddcutil::Error> {
            let dref = dg.find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & ServiceFlags::EdidPrefixAllowed as u32 != 0,
            )?;
            let handle = dg.open_display(dref)?;
            dg.set_vcp(&handle, vcp_code as u8, vcp_new_value, flags & ServiceFlags::NoVerify as u32 != 0)?;

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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<(VcpFeatureMetadata, i32, String), ddcutil::Error> {
            let dref = dg.find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & ServiceFlags::EdidPrefixAllowed as u32 != 0,
            )?;
            let handle = dg.open_display(dref)?;
            let metadata = dg.get_vcp_metadata(&handle, vcp_code.into())?;
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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<(String, i32, String), ddcutil::Error> {
            let dref = dg.find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & ServiceFlags::EdidPrefixAllowed as u32 != 0,
            )?;
            let handle = dg.open_display(dref)?;
            let caps_str = dg.get_capabilities_string(&handle)?;
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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<CapabilitiesData, ddcutil::Error> {
            let dref = dg.find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & ServiceFlags::EdidPrefixAllowed as u32 != 0,
            )?;
            let handle = dg.open_display(dref)?;
            let caps_data = dg.get_capabilities_data(handle)?;
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
        let dg = self.display_manager.acquire();
        match dg.get_display_state(
            Option::Some(display_number.into()),
            Option::Some(edid_txt),
            flags & ServiceFlags::EdidPrefixAllowed as u32 != 0,
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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<(f64, i32, String), ddcutil::Error> {
            let dref = dg.find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & ServiceFlags::EdidPrefixAllowed as u32 != 0,
            )?;
            let multiplier = dg.get_sleep_multiplier(dref)?;
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
        let dg = self.display_manager.acquire();
        let ddc_operation = || -> Result<(i32, String), ddcutil::Error> {
            let dref = dg.find_display(
                Option::Some(display_number.into()),
                Option::Some(edid_txt),
                flags & ServiceFlags::EdidPrefixAllowed as u32 != 0,
            )?;
            dg.set_sleep_multiplier(dref, new_multiplier)?;
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
        Self::DETECT_ATTRIBUTES.iter().map(|s| s.to_string()).collect()
    }

    #[zbus(property)]
    fn status_values(&self) -> HashMap<i32, String> {
        let dg = self.display_manager.acquire();
        dg.get_status_values()
    }

    #[zbus(property)]
    fn ddcutil_version(&self) -> &str {
        static VERSION_CACHE: OnceLock<String> = OnceLock::new();
        VERSION_CACHE.get_or_init(|| {
            let dg = self.display_manager.acquire();
            dg.get_ddcutil_version()
        }) }

    #[zbus(property)]
    fn ddcutil_dynamic_sleep(&self) -> bool {
        let dg = self.display_manager.acquire();
        dg.is_dynamic_sleep_enabled()
    }

    #[zbus(property)]
    fn set_ddcutil_dynamic_sleep(&mut self, value: bool) -> Result<(), FdoError> {
        if self.parameters_locked {
            return Err(FdoError::AccessDenied("configuration locked".to_string()));
        }
        let dg = self.display_manager.acquire();
        dg.enable_dynamic_sleep(value);
        Ok(())
    }

    #[zbus(property)]
    fn ddcutil_output_level(&self) -> u32 {
        let dg = self.display_manager.acquire();
        dg.get_output_level()
    }

    #[zbus(property)]
    fn set_ddcutil_output_level(&mut self, value: u32) -> Result<(), FdoError> {
        if self.parameters_locked {
            return Err(FdoError::AccessDenied("configuration locked".to_string()));
        }
        let dg = self.display_manager.acquire();
        dg.set_output_level(value);
        Ok(())
    }

    #[zbus(property)]
    fn display_event_types(&self) -> HashMap<i32, String> {
        DdcCiDbusDisplayEventType::iter().map(|f| (f as i32, f.to_string())).collect()
    }

    #[zbus(property)]
    fn service_interface_version(&self) -> &str {
        DDC_CI_DBUS_SERVICE_VERSION
    }

    #[zbus(property)]
    fn service_info_logging(&self) -> bool {
        log::max_level() == log::Level::Debug  // Matches C-coded ddcutil-service
    }

    #[zbus(property)]
    fn set_service_info_logging(&mut self, _value: bool) -> Result<(), FdoError> {
        if self.parameters_locked {
            return Err(FdoError::AccessDenied("configuration locked".to_string()));
        }
        log::set_max_level(LevelFilter::Debug);
        Ok(())
    }

    #[zbus(property)]
    fn service_emit_connectivity_signals(&self) -> bool {
        self.connectivity_signals_enabled
    }

    #[zbus(property)]
    fn set_service_emit_connectivity_signals(&mut self, enable: bool) -> Result<(), FdoError> {
        if self.parameters_locked {
            return Err(FdoError::AccessDenied("configuration locked".to_string()));
        }
        let dg = self.display_manager.acquire();
        self.connectivity_signals_enabled = enable;
        if enable {
            match dg.start_watch_displays() {
                Ok(()) => debug!("Enabled libddcutil watch_displays."),
                Err(e) => error!("Failed to enable libddcutil start_watch_displays: {:?}", e),
            }
        } else {
            match dg.start_watch_displays() {
                Ok(()) => debug!("Disabled libddcutil watch_displays."),
                Err(e) => error!("Failed to disable libddcutil watch_displays: {:?}", e),
            }
        }
        Ok(())
    }

    #[zbus(property)]
    fn service_flag_options(&self) -> HashMap<i32, String> {
        ServiceFlags::iter().map(|f| (f as i32, f.to_string())).collect()
    }

    #[zbus(property)]
    fn service_parameters_locked(&self) -> bool { self.parameters_locked }

    #[zbus(property)]
    fn service_poll_interval(&self) -> u32 {
        self.polling_controller.get_interval()
    }

    #[zbus(property)]
    fn set_service_poll_interval(&mut self, seconds: u32) -> Result<(), FdoError> {
        if self.parameters_locked {
            return Err(FdoError::AccessDenied("configuration locked".to_string()));
        }
        if seconds > 0 && seconds < 10 {
            return Err(FdoError::InvalidArgs("poll interval too small".to_string()));
        }
        self.polling_controller.set_interval(seconds);
        Ok(())
    }

    #[zbus(property)]
    fn service_poll_cascade_interval(&self) -> f64 {
        self.polling_controller.get_cascade_seconds()
    }

    #[zbus(property)]
    fn set_service_poll_cascade_interval(&mut self, seconds: f64) -> Result<(), FdoError>{
        if self.parameters_locked {
            return Err(FdoError::AccessDenied("configuration locked".to_string()));
        }
        if seconds < 0.0 || (seconds > 0.0 && seconds < 1.0) {
            return Err(FdoError::InvalidArgs("poll cascade interval too small".to_string()));
        }
        self.polling_controller.set_cascade_seconds(seconds);
        Ok(())}
}

pub fn forward_events_as_signals(internal_event_receiver: Receiver<InternalEvent>, connection: Connection) -> Result<(), Box<dyn Error>> {
    let object_server = connection.object_server();
    let interface_ref = object_server
        .interface::<_, DdcCiDbusService>(DdcCiDbusService::OBJECT_PATH)?;
    let signal_emitter = interface_ref.signal_emitter();

    info!("Service initialized. Processing background events...");

    // Process libddcutil watch events into signals. This only handles libddcutil watch events,
    // DPMS and vcp changes are handled elsewhere because the service detects them itself, they
    // are not generated by libddcutil.  The work internal here, means internal to this service,
    // not sent directly out to clients.
    while let Ok(event) = internal_event_receiver.recv() {
        let edid_base64 = extract_edid_base64(&event);
        let event_type = if edid_base64.is_empty() {
            DdcCiDbusDisplayEventType::DisplayDisconnected as i32
        } else {
            DdcCiDbusDisplayEventType::DisplayConnected as i32
        };
        info!("Internal display change event detected. Broadcasting signal event_type={} edid_base64={}",
            event_type, edid_base64);
        if let Err(e) = zbus::block_on(DdcCiDbusService::connected_displays_changed(
            signal_emitter,
            &edid_base64,
            event_type,
            0,
        )) {
            log::error!("Failed to broadcast display change event over bus: {}", e);
        }
    }
    Ok(())
}

fn error_code(e: &ddcutil::Error) -> i32 {
    e.status_code().try_into().unwrap_or(0)
}

fn error_message(prefix: &str, e: &ddcutil::Error) -> String {
    format!("{}: {}", prefix, e)
}
