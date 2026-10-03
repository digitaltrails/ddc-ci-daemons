use log::info;

// SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
// SPDX-License-Identifier: GPL-2.0-or-later
pub mod ffi_wrapper;
pub mod ddcutil;
pub mod connectivity_polling;

pub fn is_env_enabled(env_variable_name: &str, default_value: bool) -> bool {
    let Ok(raw) = std::env::var(env_variable_name) else {
        info!("Environment variable: {} using default={}", env_variable_name, default_value);
        return default_value;
    };

    let value = raw.trim().to_lowercase();
    info!("Environment variable: {}={}", env_variable_name, value);

    match value.as_str() {
        "yes" | "1" | "true" | "on" => true,
        "no" | "0" | "false" | "off" => false,
        _ => {
            info!(
                "Environment variable: {} failed to interpret value, default={}",
                env_variable_name, default_value
            );
            default_value
        }
    }
}