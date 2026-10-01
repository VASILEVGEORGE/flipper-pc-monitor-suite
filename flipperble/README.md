# flipperble

`flipperble` is a macOS-oriented command-line client for Flipper Zero RPC over BLE, USB CDC, or the shared PC Monitor backend proxy.

It is part of the **flipper-pc-monitor-suite** and is used for:

- Flipper device information
- battery queries
- advertised battery monitoring
- PC Monitor RPC control
- file and storage operations
- starting Flipper applications
- Marauder RPC/UART control
- interactive Marauder terminal access
- passive Apple Find My BLE monitoring through an attached Marauder-compatible ESP32

---

## Overview

`flipperble` can communicate with the Flipper through three transport paths.

```text
                     +----------------------+
                     |      flipperble      |
                     +----------+-----------+
                                |
              +-----------------+------------------+
              |                 |                  |
              v                 v                  v
        Direct BLE RPC      USB CDC RPC      PC Monitor proxy
              |                 |                  |
              |                 |      /tmp/flipper-pcmonitor-rpc.sock
              |                 |                  |
              +-----------------+------------------+
                                |
                                v
                         Flipper Zero RPC
```

When the PC Monitor backend is active, `flipperble` prefers the existing shared RPC connection through the Unix socket instead of creating another BLE connection.

---

# Requirements

## macOS

The current implementation is primarily developed and tested on macOS.

Recommended:

```text
macOS
Python 3
Bleak
protobuf-generated flipper_pb2.py
pyserial for USB RPC
```

For BLE mode, Bluetooth must be enabled on the Mac.

For USB mode, the Flipper must expose its USB CDC serial interface.

---

# Basic Usage

General syntax:

```bash
flipperble [--device NAME] [--usb PORT] COMMAND [ARGS...]
```

Show help:

```bash
flipperble -h
```

Example:

```bash
flipperble --device ZER0TYEC info
```

---

# Device Selection

By default, the device name can be supplied through:

```bash
--device NAME
```

Example:

```bash
flipperble --device ZER0TYEC info
```

The script also supports the environment variable:

```bash
FLIPPER_DEVICE_NAME
```

Example:

```bash
export FLIPPER_DEVICE_NAME=ZER0TYEC
flipperble info
```

This is useful when using the same Flipper regularly.

---

# Transport Modes

## 1. Shared PC Monitor RPC Proxy

When the PC Monitor backend is active, `flipperble` can reuse its existing Flipper RPC connection.

Main socket:

```text
/tmp/flipper-pcmonitor-rpc.sock
```

Active marker:

```text
/tmp/flipper-pcmonitor-rpc-active
```

The flow is:

```text
flipperble
    |
    | Unix socket
    v
flipper-pc-monitor-backend
    |
    | existing BLE RPC connection
    v
Flipper Zero
```

This avoids opening a second competing BLE connection.

Check whether the socket exists:

```bash
ls -l /tmp/flipper-pcmonitor-rpc.sock
```

Check the backend process:

```bash
ps aux | grep '[f]lipper-pc-monitor-backend'
```

Check who owns the socket:

```bash
lsof -U | grep flipper-pcmonitor
```

---

## 2. Direct BLE RPC

If the shared backend proxy is unavailable, `flipperble` can connect directly over BLE.

Example:

```bash
flipperble --device ZER0TYEC info
```

Direct BLE mode uses the Flipper RPC GATT service and its TX/RX/flow-control characteristics.

Only one active BLE RPC owner should normally be used at a time.

---

## 3. USB CDC RPC

USB mode uses the Flipper USB CDC serial port and switches it from CLI mode into protobuf RPC mode using:

```text
start_rpc_session
```

Example:

```bash
flipperble \
  --usb /dev/cu.usbmodemflip_ZER0TYEC1 \
  info
```

Find the Flipper serial device:

```bash
ls /dev/cu.usbmodem*
```

Typical result:

```text
/dev/cu.usbmodemflip_ZER0TYEC1
```

USB mode requires `pyserial`.

Install it if needed:

```bash
python3 -m pip install pyserial
```

---

# Commands

The current command set is:

```text
info
battery
advbattery
pcmon-start
marauder
marauder-terminal
marauder-findmy
cat
log
ls
put
run
rm
mkdir
mv
```

---

# `info`

Show Flipper device and firmware information.

```bash
flipperble --device ZER0TYEC info
```

Example output:

```text
hardware_name                  : ZER0TYEC
hardware_model                 : Flipper Zero
hardware_region_provisioned    : BG
hardware_ver                   : 12
firmware_version               : mntm-dev
firmware_build_date            : 01-10-2026
firmware_commit                : 24302ed6
firmware_origin_fork           : Momentum
firmware_api_major             : 87
firmware_api_minor             : 1
protobuf_version_major         : 0
protobuf_version_minor         : 25
radio_alive                    : true
radio_ble_mac                  : 5A455226E180
```

Useful for confirming:

- firmware version
- RPC API compatibility
- Flipper identity
- BLE radio state
- current custom Momentum build

---

# `battery`

Read the actual Flipper battery level through RPC.

```bash
flipperble --device ZER0TYEC battery
```

Use this when an RPC connection is available.

This is different from `advbattery`, which reads a custom BLE advertisement instead of querying the Flipper through RPC.

---

# `advbattery`

Read the battery level from the custom Momentum BLE manufacturer advertisement.

```bash
flipperble --device ZER0TYEC advbattery
```

The custom advertisement uses manufacturer ID:

```text
0xFFFF
```

with payload:

```text
46 5A 01 XX FLAGS
F  Z  ver battery
```

Where:

```text
46 5A = "FZ"
01    = format version
XX    = battery percentage
FLAGS = custom state flags
```

The implementation uses a persistent Bleak scanner during the scan window instead of repeatedly calling discovery.

This is more reliable on macOS because CoreBluetooth does not need to repeatedly tear down and recreate the scanner.

Example:

```text
Advertising battery: 100%
```

If the packet is not seen during the scan window:

```text
Advertising battery: unavailable
```

---

# `pcmon-start`

Start or request PC Monitor using the shared Flipper RPC session.

```bash
flipperble --device ZER0TYEC pcmon-start
```

This is intended for the PC Monitor integration in this repository.

Depending on the current PC Monitor/FAP state, the backend may already own the RPC connection.

---

# `marauder`

Send one Marauder CLI command through:

```text
Host
  -> Flipper RPC
  -> marauder_rpc_bridge.fap
  -> UART
  -> ESP32 Marauder
```

Example:

```bash
flipperble --device ZER0TYEC marauder "help"
```

Another example:

```bash
flipperble --device ZER0TYEC marauder "scanap"
```

The client starts:

```text
/ext/apps/GPIO/marauder_rpc_bridge.fap
```

in RPC mode and uses `App.DataExchange` to pass command data.

---

# `marauder-terminal`

Open an interactive persistent Marauder terminal.

```bash
flipperble --device ZER0TYEC marauder-terminal
```

Data path:

```text
Terminal
   <->
flipperble
   <->
Flipper RPC
   <->
Marauder RPC Bridge
   <->
UART
   <->
ESP32 Marauder
```

Exit with:

```text
exit
```

or:

```text
quit
```

This mode is useful for interactive Marauder development and debugging.

---

# `marauder-findmy`

Passively monitor Apple Find My / Offline Finding BLE advertisements using the attached Marauder-capable ESP32.

```bash
flipperble --device ZER0TYEC marauder-findmy
```

The command:

1. starts `marauder_rpc_bridge.fap`;
2. sends `sniffbt` to Marauder;
3. receives UART output through Flipper RPC;
4. reconstructs complete UART lines;
5. extracts BLE advertisement fields;
6. filters for Apple Find My advertisements.

The filter looks for:

```text
FF4C001219
```

Meaning:

```text
FF       Manufacturer Specific Data AD type
4C 00    Apple company identifier
12 19    Apple Offline Finding / Find My format
```

Example:

```text
15:43:56.239  DE:43:F3:5E:0A:2B   -64 dBm  len=31  4C00121950CB60...
```

Displayed fields:

```text
timestamp
advertiser MAC
RSSI
advertisement length
Apple manufacturer payload
```

Important:

`marauder-findmy` does **not** read the Flipper FindMy application's saved state.

The actual scanner is the attached ESP32/Marauder device.

The Flipper acts as the RPC-to-UART transport.

Press:

```text
Ctrl+C
```

to stop.

`flipperble` then attempts to send:

```text
stopscan
```

to Marauder.

---

# `ls`

List a Flipper storage directory.

```bash
flipperble --device ZER0TYEC ls /ext
```

Example:

```bash
flipperble --device ZER0TYEC ls /ext/apps/Bluetooth
```

If no path is given, the default is:

```text
/ext
```

---

# `cat`

Print a remote file from Flipper storage.

```bash
flipperble --device ZER0TYEC cat /ext/apps_data/findmy/findmy_state.txt
```

Useful for:

- configuration inspection
- logs
- application state
- debug output

Example for FindMy:

```bash
flipperble \
  --device ZER0TYEC \
  cat /ext/apps_data/findmy/findmy_state.txt
```

---

# `log`

Display the PC Monitor debug log from the Flipper.

```bash
flipperble --device ZER0TYEC log
```

This command is specific to the PC Monitor integration.

---

# `put`

Upload a local file to Flipper storage.

Syntax:

```bash
flipperble --device ZER0TYEC put LOCAL_FILE REMOTE_FILE
```

Example:

```bash
flipperble \
  --device ZER0TYEC \
  put ./pc_monitor.fap \
  /ext/apps/Bluetooth/pc_monitor.fap
```

The implementation uses chunked storage RPC writes.

The current chunk size is intentionally conservative for reliable BLE transport.

---

# `run`

Start a Flipper application from a remote FAP path.

```bash
flipperble \
  --device ZER0TYEC \
  run /ext/apps/Bluetooth/pc_monitor.fap
```

Another example:

```bash
flipperble \
  --device ZER0TYEC \
  run /ext/apps/GPIO/marauder_rpc_bridge.fap
```

---

# `rm`

Delete a file.

```bash
flipperble \
  --device ZER0TYEC \
  rm /ext/apps/Bluetooth/test.fap
```

Recursive directory deletion:

```bash
flipperble \
  --device ZER0TYEC \
  rm -r /ext/apps_data/test
```

Use recursive deletion carefully.

---

# `mkdir`

Create a directory on Flipper storage.

```bash
flipperble \
  --device ZER0TYEC \
  mkdir /ext/apps_data/myapp
```

---

# `mv`

Move or rename a file or directory.

Example:

```bash
flipperble \
  --device ZER0TYEC \
  mv /ext/apps/Bluetooth/old.fap /ext/apps/Bluetooth/new.fap
```

---

# RPC Application Data

Some Flipper RPC applications send unsolicited `App.DataExchange` packets.

Examples include:

- Marauder UART output
- continuous bridge output
- application-generated asynchronous data

`flipperble` handles these packets before normal command-ID response filtering.

They are placed into:

```python
app_data_queue
```

This is important because asynchronous application data can use a command ID unrelated to the currently outstanding host request.

This design is what makes continuous Marauder terminal and BLE monitoring possible.

---

# Shared RPC Event Stream

The macOS backend also provides an event stream socket:

```text
/tmp/flipper-pcmonitor-events.sock
```

The backend publishes decoded inbound `PB.Main` frames to this socket.

The event socket is separate from the normal request/response proxy:

```text
/tmp/flipper-pcmonitor-rpc.sock
```

Architecture:

```text
                           one BLE RPC session
                                  |
                          macOS backend RX
                            /           \
                           /             \
              normal RPC responses    event copies
                       |                   |
                rpc.sock clients      events.sock clients
```

The event stream is useful for tools that need passive access to inbound RPC traffic without owning the BLE notification stream.

---

# PC Monitor Backend Integration

When the backend is connected, it owns the active BLE RPC transport.

`flipperble` can then use:

```text
/tmp/flipper-pcmonitor-rpc.sock
```

instead of creating another BLE connection.

This is especially useful for commands such as:

```bash
flipperble --device ZER0TYEC info
flipperble --device ZER0TYEC battery
flipperble --device ZER0TYEC ls /ext
```

while the backend is already connected.

---

# FindMy Battery Testing

Custom Momentum builds in this project may include the Flipper CLI command:

```text
findmy_battery
```

This command is part of the firmware, not `flipperble`, but it is useful alongside `marauder-findmy`.

Supported test values:

```text
findmy_battery show
findmy_battery auto
findmy_battery 00
findmy_battery 50
findmy_battery A0
findmy_battery F0
```

Battery categories:

```text
00 = Full
50 = Medium
A0 = Low
F0 = Critical
```

Example test:

```text
findmy_battery show
findmy_battery 50
findmy_battery show
```

Then monitor the advertisement:

```bash
flipperble --device ZER0TYEC marauder-findmy
```

Example transition:

```text
4C00121900CB60...
        ^^

4C00121950CB60...
        ^^
```

The background FindMy battery worker can then restore the correct real battery category automatically.

---

# Flipper Lock Behavior

When the Flipper itself is locked, RPC commands may time out.

Observed behavior:

```text
Flipper unlocked -> RPC works
Flipper locked   -> RPC timeout
Flipper unlocked -> RPC works again
```

Example:

```bash
flipperble --device ZER0TYEC info
```

may return:

```text
ERROR: Timeout waiting for Flipper RPC response
```

while the Flipper is locked.

This does not necessarily mean BLE advertising has stopped.

Passive Extra Beacon / Find My advertising can continue independently of interactive RPC availability.

---

# Troubleshooting

## `Flipper 'NAME' not found`

Example:

```text
ERROR: Flipper 'ZER0TYEC' not found
```

Check:

```bash
flipperble --device ZER0TYEC info
```

and verify:

- Bluetooth is enabled;
- the Flipper is advertising;
- another host is not already connected;
- the PC Monitor backend is not in a stale state;
- the correct Bluetooth device name is used.

---

## RPC timeout

Example:

```text
ERROR: Timeout waiting for Flipper RPC response
```

Check whether the Flipper is locked.

Then inspect the backend:

```bash
ps aux | grep '[f]lipper-pc-monitor-backend'
```

Check sockets:

```bash
ls -l \
  /tmp/flipper-pcmonitor-rpc.sock \
  /tmp/flipper-pcmonitor-events.sock
```

Check logs:

```bash
tail -n 100 /tmp/flipper-pcmonitor.log
tail -n 100 /tmp/flipper-pcmonitor-error.log
```

---

## USB RPC cannot open device

Example:

```text
Could not open USB port
```

Check the port:

```bash
ls /dev/cu.usbmodem*
```

Check whether another process owns it:

```bash
lsof /dev/cu.usbmodem* 2>/dev/null
```

A `screen` session can keep the serial device open.

Find it:

```bash
ps aux | grep '[s]creen /dev/cu.usbmodem'
```

Then terminate the stale session if needed.

---

## `pyserial` missing

Example:

```text
ModuleNotFoundError: No module named 'serial'
```

Install:

```bash
python3 -m pip install pyserial
```

or install it inside the virtual environment used by `flipperble`.

---

## Marauder bridge start RPC error

Example:

```text
ERROR: Marauder bridge start RPC error 17
```

This usually means the Flipper RPC App System is already occupied by another incompatible application/session.

Check whether PC Monitor or another RPC-mode FAP is already active.

The bridge FAP must exist at:

```text
/ext/apps/GPIO/marauder_rpc_bridge.fap
```

---

## No Marauder UART output

Check:

- ESP32 board is powered;
- correct GPIO/UART wiring;
- Marauder firmware is running;
- correct UART parameters;
- `marauder_rpc_bridge.fap` is running;
- Flipper RPC connection is active.

Test:

```bash
flipperble --device ZER0TYEC marauder "help"
```

---

## `advbattery` returns unavailable

Example:

```text
Advertising battery: unavailable
```

This means the custom manufacturer advertisement was not observed during the scan window.

Possible causes include:

- Flipper currently connected instead of advertising;
- CoreBluetooth timing;
- competing BLE activity;
- FindMy/Extra Beacon scheduling;
- PC Monitor reconnect activity.

Retrying may succeed.

---

# Useful Paths

## macOS

```text
/Applications/Flipper/flipperble/flipperble.py
/Applications/Flipper/flipper-pc-monitor-backend
/tmp/flipper-pcmonitor-rpc.sock
/tmp/flipper-pcmonitor-events.sock
/tmp/flipper-pcmonitor-rpc-active
/tmp/flipper-pcmonitor.log
/tmp/flipper-pcmonitor-error.log
```

## Flipper

```text
/ext/apps/GPIO/marauder_rpc_bridge.fap
/ext/apps/Bluetooth/pc_monitor.fap
/ext/apps_data/findmy/findmy_state.txt
```

---

# Useful Checks

Show running backend:

```bash
ps aux | grep '[f]lipper-pc-monitor-backend'
```

Check proxy socket:

```bash
lsof -U | grep flipper-pcmonitor
```

Check Flipper USB device:

```bash
ls -l /dev/cu.usbmodem*
```

Check device information:

```bash
flipperble --device ZER0TYEC info
```

Check battery:

```bash
flipperble --device ZER0TYEC battery
```

Check advertised battery:

```bash
flipperble --device ZER0TYEC advbattery
```

List SD card:

```bash
flipperble --device ZER0TYEC ls /ext
```

Monitor Find My advertisements:

```bash
flipperble --device ZER0TYEC marauder-findmy
```

---

# Recommended Workflow

For normal PC Monitor use:

```text
1. Start the macOS backend
2. Let the backend own the BLE RPC connection
3. Use flipperble commands through the shared proxy
```

For file operations:

```bash
flipperble --device ZER0TYEC ls /ext
flipperble --device ZER0TYEC cat /ext/path/file.txt
flipperble --device ZER0TYEC put local.file /ext/path/file
```

For Marauder:

```bash
flipperble --device ZER0TYEC marauder "help"
flipperble --device ZER0TYEC marauder-terminal
flipperble --device ZER0TYEC marauder-findmy
```

For direct USB debugging:

```bash
flipperble --usb /dev/cu.usbmodemflip_ZER0TYEC1 info
```

---

# Security Notes

`flipperble` can:

- read device information;
- query battery and firmware state;
- access Flipper storage;
- upload and delete files;
- start applications;
- interact with a UART-connected Marauder board.

Use it only with devices and systems you control or are authorized to access.

The `marauder-findmy` feature is intended for passive BLE diagnostics and development.

---

# Related Components

This repository also contains:

```text
mac-backend/
    macOS PC Monitor backend and shared RPC proxy

flipper-fap/
    PC Monitor Flipper application

marauder-rpc-bridge/
    Flipper RPC <-> UART bridge for Marauder

airbattery-helper/
    AirBattery integration helpers
```

Together they provide a shared BLE/USB RPC environment for Flipper development and monitoring.

---

# License

See the repository license and the licenses of the upstream Flipper Zero, Momentum, Bleak, and Marauder projects for the terms applicable to their respective components.
