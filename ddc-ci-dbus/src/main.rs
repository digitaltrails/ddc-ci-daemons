// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

//! DDC CI D-Bus service

mod ddc_ci_dbus_service;

use ddc_ci_dbus_service::DdcCiDbusService;

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

