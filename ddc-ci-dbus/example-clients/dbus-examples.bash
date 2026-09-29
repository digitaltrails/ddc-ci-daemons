# SPDX-FileCopyrightText: 2026 Contributors to ddcutil-varlink <https://github.com/digitaltrails/ddcutil-varlink>
# SPDX-License-Identifier: GPL-2.0-or-later
DBUS_SERVICE_NAME=local.ddc-ci.DdcCiService
DBUS_OBJECT=/local/ddc_ci/DdcCiObject
DBUS_INTERFACE_NAME=local.ddc_ci.DdcCiInterface
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME Detect u 0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME SetVcp isyqu 1 "" 0x10  90  0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME SetVcpWithContext isyqsu 1 "" 0x10  60 "my_app" 0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME GetVcp isyu 1 "" 0x10 0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME GetMultipleVcp isayu 1 "" 2 0x10 0x12 0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME GetVcpMetadata isyu 1 "" 0x10 0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME GetCapabilitiesString isu 1 ""  0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME GetCapabilitiesMetadata isu 1 ""  0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME GetDisplayState isu 1 ""  0
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT org.freedesktop.DBus.Properties Get ss "$DBUS_INTERFACE_NAME" StatusValues
busctl --user call $DBUS_SERVICE_NAME $DBUS_OBJECT org.freedesktop.DBus.Properties Set ssv "$DBUS_INTERFACE_NAME" ServiceEmitConnectivitySignals b 1