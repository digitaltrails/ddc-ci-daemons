# SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
# SPDX-License-Identifier: GPL-2.0-or-later

SERVICE="unix:$XDG_RUNTIME_DIR/ddc-ci-varlink.socket"
INTERFACE=local.ddc-ci.service

varlinkctl list-methods $SERVICE
varlinkctl introspect $SERVICE

varlinkctl call $SERVICE $INTERFACE.Detect '{"include_offline": false}'
varlinkctl call $SERVICE $INTERFACE.GetVcp '{"display_number":1,"vcp_code":16}'
varlinkctl call $SERVICE $INTERFACE.Subscribe '{"use_polling":false}'
varlinkctl call $SERVICE $INTERFACE.GetMultipleVcp '{"display_number":1,"vcp_codes":[16,20]}'
varlinkctl --more --timeout=infinity  call $SERVICE $INTERFACE.Subscribe '{}'
varlinkctl call $SERVICE $INTERFACE.GetMultipleVcp '{"edid_base64":"AP///////wAi8GkoAQEBAQgUAQSlNiN4Lvy","vcp_codes":[16,20], "options": { "allow_edid_prefix": true  } }'
