// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later

//! # ddcutil-varlink
//!
//! A varlink service for _libddcutil_.
//!
//! Start a service on a unix socket, in priority order use:
//!
//! 1. If the environment variable `LISTEN_FDS` is set, accept
//!    the socket set by systemd (assumed to be on fd 3).
//! 2. If the environment variable `XDG_RUNTIME_DIR` is set,
//!
//!    use `unix:$XDG_RUNTIME_DIR/ddc-ci-varlink.socket`,
//! 3. Fallback to `/tmp/ddc-ci-varlink.socket`.

use log::{error, info, warn};
use std::os::unix::net::UnixListener;
use std::os::unix::io::FromRawFd;
use varlink::*;

use ddc_ci_varlink_impl::DdcCiVarlinkService;

// Our varlink generated interface for com_ddcutil_service.
#[allow(nonstandard_style, dead_code, clippy::all, clippy::nursery)]
mod local_ddc_ci_service {
    include!(concat!(env!("OUT_DIR"), "/local.ddc-ci.service.rs"));
}

// Our modules
mod ddc_ci_varlink_service;
mod ddc_ci_varlink_subscribers;
mod ddc_ci_varlink_impl;

/// Start the service on its unix socket.
///
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Set up panic hook for better logging
    std::panic::set_hook(Box::new(|panic_info| {
        let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "unknown"
        };
        let location = panic_info
            .location()
            .unwrap_or_else(|| std::panic::Location::caller());
        error!(
            "PANIC at {}:{}: {}",
            location.file(),
            location.line(),
            payload
        );
    }));

    env_logger::init();
    info!(
        "Running with user privileges (UID: {})",
        rustix::process::getuid().as_raw()
    );

    // Create our implementation of the service.
    let service_implementation= DdcCiVarlinkService::new();

    // Build the Varlink interface
    let interface = local_ddc_ci_service::new(Box::new(service_implementation));
    let varlink_service = VarlinkService::new(
        DdcCiVarlinkService::VENDOR,
        DdcCiVarlinkService::PRODUCT,
        DdcCiVarlinkService::VERSION,
        DdcCiVarlinkService::PRODUCT_URL,
        vec![Box::new(interface)],
    );

    // Check for systemd Socket Activation (LISTEN_FDS environment variable)
    // Will most likely be bound to unix:$XDG_RUNTIME_DIR/ddc-ci-varlink.socket
    if let Ok(fds) = std::env::var("LISTEN_FDS") {
        // Systemd handles binding the file descriptor for us.
        // We pass an empty/dummy address string because varlink crate
        // automatically prioritizes the systemd FD when LISTEN_FDS exists.

        // SAFETY: We assume fd 3 is a valid socket passed by systemd.
        info!("LISTEN_FDS is set to {}. Activated via systemd. Assuming file descriptor 3.", fds);

        let listener = unsafe { UnixListener::from_raw_fd(3) };
        if let Ok(addr) = listener.local_addr() {
            if let Some(path) = addr.as_pathname() {
                info!("Listening on systemd assigned socket: {}", path.display());  // prints the path
            } else {
                warn!("Listening on abstract or unnamed socket.");
            }
        }

        varlink::listen(
            varlink_service,
            "unix:",
            &varlink::ListenConfig {
                idle_timeout: 600,
                ..Default::default()
            },
        )?;
    } else {
        // Fallback for manual local debugging/development
        // Dynamically build the path using XDG_RUNTIME_DIR safely

        // Determine socket address
        // Default to unix:$XDG_RUNTIME_DIR/ddc-ci-varlink.socket or /tmp/ddc-ci-varlink.socket
        // if XDG_RUNTIME_DIR isn't set.
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_owned());
        let socket_address = format!("unix:{}/{}", runtime_dir, DdcCiVarlinkService::FALLBACK_SOCKET_FILENAME);

        warn!("LISTEN_FDS is not set. Running in manual mode.");
        info!("Listening on socket: {}", socket_address);
        varlink::listen(
            varlink_service,
            &socket_address,
            &varlink::ListenConfig {
                idle_timeout: 0,
                ..Default::default()
            },
        )?;
    }

    Ok(())
}
