<!-- 
SPDX-FileCopyrightText: 2026 Contributors to ddc-ci-daemons <https://github.com/digitaltrails/ddc-ci-daemons>
SPDX-License-Identifier: GPL-2.0-or-later
-->

# DDC-CI daemons

> [!IMPORTANT]
> 2026/09/26: The project name, executable names, D-Bus handle names and varlink handle names 
> have been changed to properly indicate that this is <u>not</u> a project 
> under the official [com.ddutil](https://www.ddcutil.com/) umbrella. All the client 
> examples have been updated to the new handles. 

> [!CAUTION]
> When using these daemons, avoid excessively writing VCP values because each VDU's
> NVRAM likely has a write-cycle limit/lifespan. The suggested guideline is to limit
> updates to rates comparable to those observed when using the VDU's onboard controls.
> Avoid coding that might rapidly or infinitely loop, including when recovering
> from errors and bugs.
>
> Non-standard manufacturer specific features should only be experimented with caution,
> some may have irreversible consequences, including bricking the hardware.

DDC-CI-daemons is a project that implements Varlink and D-Bus DDC-CI daemons for control of 
displays/monitors/VDUs:


__ddc-ci-varlink__
: A varlink service that implements the interfaces specified by [local.ddc-ci.service.varlink](ddc-ci-varlink/varlink/local.ddc-ci.service.varlink).

__ddc-ci-dbus__
: A D-Bus implementation compatible with the C coded [ddcutil-service](https://github.com/digitaltrails/ddcutil-service).

This project aims to make it easier to create widgets
and applications that can alter display/monitor/VDU features such
as brightness and contrast.  The capabilities of these two daemons are 
similar to those provided by [ddcutil-service](https://github.com/digitaltrails/ddcutil-service), an older C-coded 
D-Bus service.

The project is coded in Rust using varlink-crate and zbus-crate.  The daemons use
a common Rust backend.  The backend wraps [libddcutil](https://www.ddcutil.com/), a C-library 
that robustly supports numerous OEM DDC implementations and GPU drivers.

The daemons are designed to run as user session-bus services. Providing libddcutil 
is installed with the correct privileges, the daemons need no additional 
privileges.  It's not recommended to run them as system-bus services, they
have never been tested in system space. 

> [!NOTE]
> ### ddc-ci-varlink
> All methods in [local.ddc-ci.service.varlink](ddc-ci-varlink/varlink/local.ddc-ci.service.varlink) 
> have now been implemented.
> 
> The [vdu_controls](https://digitaltrails.github.io/vdu_controls/), a control panel for 
> displays, can be configured to use ddc-ci-varlink.
>
> ddc-ci-Varlink development is more or less complete at this point.  Possible 
> future work:
> - Packaging, probably initially targeting openSUSE Tumbleweed.
> - Replace varlink-crate by zlink-crate when zlink-crate reaches 1.0.
>
> ### ddc-ci-dbus
> The ddc-ci-dbus daemon is now fully implemented. 
> Most methods and properties are now fully functional. Event handing
> is also complete. 
>
> The daemon now fully delivers
> the functionality required by [vdu_controls]([vdu_controls](https://digitaltrails.github.io/vdu_controls/)). 


An attempt has been made to keep the code compact and the abstractions relatively
shallow. Providing you know Rust and a little about [varlink](https://varlink.org/) or D-Bus, the
code should be quite easy to follow.  

# Building the daemons

## Compilation

Use the normal Rust cargo command to build the executables
```
# Build everything
cargo build --release
# Or individually
cargo build --release --package ddcutil-backend
cargo build --release --bin ddc-ci-varlink
cargo build --release --bin ddc-ci-dbus

# Normally the release binaries wind up in $HOME/.cargo/bin:
ls -1 $HOME/.cargo/bin/ddc-ci-*
.../.cargo/bin/ddc-ci-dbus
.../.cargo/bin/ddc-ci-varlink
```

## Installation of the executables

Cargo can also be used to install the executables for a single user:
```
cargo install --path ddc-ci-varlink
cargo install --path ddc-ci-sbus

# The above normally installs to $HOME/.cargo/bin
ls -l $HOME/.cargo/bin
```
These or the target directory outputs can be copied to anywhere
appropriate.

# Using the ddc-ci-varlink daemon

Once built, the service should be run under a user account (assuming 
libddcutil is installed the required permissions).  For example, the 
service can be run out of the build directory:
```
RUST_LOG=debug ./target/release/ddc-ci-varlink

[2026-09-24T21:55:31Z INFO  ddc-ci_varlink] Running with user privileges (UID: 500)
[2026-09-24T21:55:31Z INFO  ddcutil_backend::ddcutil] Initializing ddcutil
(ddci_init                     ) Calling ldbus_start_sleep_watch_thread...
[2026-09-24T21:55:32Z DEBUG ddcutil_backend::ddcutil] Redetect displays
...
```
A socket should then be available on `unix:$XDG_RUNTIME_DIR/ddc-ci-varlink.socket`. 
Any type of varlink client can be used to interact with the service. For example, 
systemd commandline client `varlinkctl` may be used as follows:

```
SERVICE="unix:$XDG_RUNTIME_DIR/ddc-ci-varlink.socket"
INTERFACE="local.ddc-ci.service"
varlinkctl list-methods $SERVICE
varlinkctl introspect $SERVICE
varlinkctl call $SERVICE "${INTERFACE}.Detect" '{"include_offline":false}'
varlinkctl call $SERVICE "${INTERFACE}.GetVcp" '{"display_number":1,"vcp_code":16}'
varlinkctl call $SERVICE "${INTERFACE}.SetVcp" '{"display_number":5,"vcp_code":16,"new_value":50}'
varlinkctl call $SERVICE "${INTERFACE}.SetVcp" '{"display_number":2,"vcp_code":16,"new_value":70,"options":{"no_verify":true}}'
varlinkctl call $SERVICE "${INTERFACE}.GetMultipleVcp" '{"edid_base64":"AP///////wAi8Gk","vcp_codes":[16,20],"options":{"allow_edid_prefix":true}}'
varlinkctl --more --timeout=infinity call $SERVICE "${INTERFACE}.Subscribe" '{}'
```

## Building and running ddc-ci-varlink for debugging

```
# Build and run for debugging
cargo build --debug
RUST_BACKTRACE=1 RUST_LOG=debug ./target/debug/ddc-ci-varlink
```


## Installing ddc-ci-varlink as a systemd auto-started service

To run the varlink service via systemd for a single user, create the following 
service files and confirm the executable `ExecStart` location. 

```
# $HOME/.local/share/systemd/user/user/ddc-ci-varlink.service                                                                                                                      ✔  10664  09:32:53
[Unit]
Description=DDC CI Varlink Service
Requires=ddc-ci-varlink.socket
After=ddc-ci-varlink.socket

[Service]
Type=simple
ExecStart=%h/.cargo/bin/ddc-ci-varlink
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
```

```
# $HOME/.local/share/systemd/user/user/ddc-ci-varlink.socket
[Unit]
Description=DDC CI Varlink Service Socket

[Socket]
ListenStream=%t/ddc-ci-varlink.socket
SocketMode=0600

[Install]
WantedBy=sockets.target
```

Install the systemd unit and socket for a single user:
```
# Reload the user systemd manager daemon
systemctl --user daemon-reload

# Enable and start the socket unit immediately
systemctl --user enable --now ddc-ci-varlink.socket

# Verify running
systemctl --user status ddc-ci-varlink.socket
```

To install for all users, modify the above for installation
in your distro's shared user-unit location, and relocate 
the executable to somewhere like `/usr/bin` or `/usr/local/bin`.

## ddc-ci-varlink clients

The GUI [vdu_controls](https://digitaltrails.github.io/vdu_controls/) can optionally use ddc-ci-varlink as
its ddcutil interface.

The project's [example-clients](ddc-ci-varlink/example-clients) 
directory contains some example clients.

## Rust varlink implementations

I evaluated two Rust libraries _varlink_ and _zlink_.
I found varlink to generate code that worked out of the box, so I've
stuck with it. Trying to work with zlink generated code seemed to 
result in some difficult to resolve build errors.  I may further experiment 
with zlink at some point when it further matures.

# Using ddc-ci-dbus daemon

The ddc-ci-dbus daemon implements a subset of same interface as the C based
[ddcutil-service](https://github.com/digitaltrails/ddcutil-service).
Any existing client can be used, this includes the GUI [vdu_controls](https://digitaltrails.github.io/vdu_controls/).

However, note that the d-bus handles have changed:
```
DBUS_BUS_NAME=local.ddc-ci.DdcCiService
DBUS_OBJECT=/local/ddc_ci/DdcCiObject
DBUS_INTERFACE_NAME=local.ddc_ci.DdcCiInterface
busctl --user call $DBUS_BUS_NAME $DBUS_OBJECT $DBUS_INTERFACE_NAME Detect u 0
```


Once built, running the executable should make a ddc-ci-dbus service findable
via the normal broker, For example, we might run out of the build directory 
with debugging info:
```
RUST_LOG=debug ./target/release/ddc-ci-dbus

DdcCiService running on D-Bus...
```
Then we could use busctl to check if it's registered.
```
busctl --user | grep DdcutilService
```

## Installing ddc-ci-dbus as a pre-registered D-Bus service

TODO. 

# Acknowledgements

Thanks go out to Sanford Rockowitz ([rockowitz](https://github.com/rockowitz)) 
for [libddcutil, ddcutil](https://www.ddcutil.com/).


