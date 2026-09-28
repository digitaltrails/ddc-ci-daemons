// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

///! DDC CI D-Bus service

mod ddc_ci_dbus_service;

use std::env::VarError;
use std::error::Error;
use crossbeam_channel::Receiver;
use log::{debug, error, info};
use zbus::blocking::{connection, Connection};
use ddc_ci_dbus_service::DdcCiDbusService;
use ddcutil_backend::ddcutil::{extract_edid_base64, start_watch_displays, InternalEventKind, InternalEvent};
use crate::ddc_ci_dbus_service::DdcCiDbusDisplayEventType;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // Create our implementation of the service and obtain the receiver channel
    let (service, internal_event_receiver) = DdcCiDbusService::new();

    service.start_event_monitoring();

    // Serve the interface
    let connection = service.serve_clients()?;

    // Loops until the channel closes, which only happens at service shutdown.
    ddc_ci_dbus_service::forward_events_as_signals(internal_event_receiver, connection)?;

    Ok(())
}

fn serve_clients(service: DdcCiDbusService) -> zbus::Result<Connection> {
    let connection = connection::Builder::session()?
        .name(DdcCiDbusService::SERVICE_NAME)?
        .serve_at(DdcCiDbusService::OBJECT_PATH, service)?
        .build();

    let interface_name = <DdcCiDbusService as zbus::object_server::Interface>::name();
    info!("Running D-Bus Service: {}; Object: {}; Interface: {}",
        DdcCiDbusService::SERVICE_NAME,
        DdcCiDbusService::OBJECT_PATH,
        interface_name);

    connection
}


fn is_env_enabled(env_variable_name: &str, default_value: bool) -> bool {
    let value = std::env::var(env_variable_name)
        .map(|v| ["yes", "1", "true", "on"].iter().any(|s| v.eq_ignore_ascii_case(s)))
        .unwrap_or(default_value);
    info!("Environment variable: {}={} (default={})", env_variable_name, value, default_value);
    value
}


