# Flipper PC Monitor macOS Backend

Rust backend for the **Flipper PC Monitor Suite**.

The backend runs on macOS, collects system telemetry, discovers the Flipper Zero through CoreBluetooth, manages the BLE/RPC lifecycle, launches PC Monitor in RPC mode, streams telemetry to the FAP, and publishes the Flipper battery state for the AirBattery integration.

## What the Backend Does

The backend is responsible for:

- scanning for the PC Monitor Flipper over BLE;
- parsing the custom PC Monitor advertising payload;
- detecting when the Flipper requests an RPC connection;
- connecting to the Flipper RPC GATT service;
- launching `/ext/apps/Bluetooth/pc_monitor.fap` in RPC mode;
- collecting Mac CPU, RAM, GPU, battery and temperature telemetry;
- sending a compact 14-byte telemetry packet to the FAP;
- recovering from BLE/RPC disconnects without normally terminating the process;
- restarting BLE scanning after PC Monitor exits;
- publishing the Flipper battery level for AirBattery;
- exposing a local RPC proxy socket for companion tooling.

## Architecture

```text
macOS telemetry
     │
     ├── CPU
     ├── RAM
     ├── GPU
     ├── Mac battery
     ├── CPU temperature
     ├── GPU temperature
     ├── SSD temperature
     └── Battery temperature
     │
     ▼
flipper-pc-monitor-backend
     │
     ├── CoreBluetooth scanner
     ├── advertising parser
     ├── RPC connection manager
     ├── telemetry encoder
     ├── AirBattery publisher
     └── local RPC proxy
     │
     ▼
BLE / Flipper RPC
     │
     ▼
Flipper Zero
     │
     ▼
PC Monitor FAP
```

## Requirements

- macOS
- Apple Silicon tested
- Rust / Cargo
- Bluetooth enabled
- Flipper Zero paired/available to macOS
- compatible PC Monitor FAP
- compatible Momentum firmware patch

The development environment uses Momentum `mntm-012` with firmware API `87.1`.

## Build

From the backend directory:

```bash
cargo check
cargo build --release
```

The release binary is created at:

```text
target/release/flipper-pc-monitor-backend
```

Run it manually with:

```bash
./target/release/flipper-pc-monitor-backend
```

Typical startup output:

```text
RPC proxy listening on /tmp/flipper-pcmonitor-rpc.sock
Found "CoreBluetooth" adapter
Scanning for Flipper RPC...
```

## Installation

The installed backend used during development is:

```text
/Applications/Flipper/flipper-pc-monitor-backend
```

Install a newly built release with:

```bash
sudo mkdir -p /Applications/Flipper

sudo cp \
target/release/flipper-pc-monitor-backend \
/Applications/Flipper/flipper-pc-monitor-backend
```

## LaunchAgent

The backend can run automatically as a per-user LaunchAgent.

Development service identifier:

```text
com.flipper.pcmonitor
```

LaunchAgent file:

```text
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Restart it with:

```bash
launchctl bootout \
gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist \
2>/dev/null

launchctl bootstrap \
gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Verify the process:

```bash
pgrep -fl flipper-pc-monitor
```

Follow the log:

```bash
tail -f /tmp/flipper-pcmonitor.log
```

## BLE Advertising Protocol

PC Monitor uses a manufacturer-specific advertising extension.

The PC Monitor-specific bytes are:

```text
46 5A 01 BAT FLAGS
```

| Field | Size | Description |
|---|---:|---|
| `46 5A` | 2 bytes | PC Monitor signature |
| `01` | 1 byte | Advertising protocol version |
| `BAT` | 1 byte | Flipper battery percentage |
| `FLAGS` | 1 byte | PC Monitor request/state flags |

The backend validates the PC Monitor advertising signature before treating a BLE peripheral as the target Flipper.

This is also important for the reconnect watchdog: unrelated nearby BLE devices must not become the remembered Flipper target.

## PC Monitor Request

While idle, the backend scans for the Flipper advertising payload.

When the PC Monitor request flag is detected, the backend transitions from scanning to connection establishment.

Typical log sequence:

```text
Advertising battery=83%, flags=0x00
State Scanning -> Connecting (advertisement requested RPC)
PC Monitor request detected - connecting RPC (flags=0x01)
State Connecting -> RpcReady (BLE RPC services ready)
Connected to Flipper RPC
```

## Connection State Machine

The backend uses a centralized lifecycle:

```text
Scanning
   │
   ▼
Connecting
   │
   ▼
RpcReady
   │
   ▼
AppRunning
   │
   ├────────────► Recovering ───────────► Scanning
   │
   ▼
WaitingForIdle
   │
   ▼
Scanning
```

The purpose of the state machine is to prevent the scanner, RPC connection, application lifecycle and watchdog from independently attempting conflicting BLE operations.

### Scanning

The backend waits for a valid PC Monitor advertising payload.

### Connecting

A matching advertisement requested an RPC connection and CoreBluetooth connection setup is in progress.

### RpcReady

The BLE RPC service is available.

The backend allows the bootstrap FAP lifecycle to settle before launching the RPC-mode application.

### AppRunning

PC Monitor is running through RPC and telemetry is being streamed.

### WaitingForIdle

PC Monitor has closed or the RPC application session is ending.

The backend waits for the Flipper to return to a stable idle advertising state.

### Recovering

A transient BLE/RPC failure occurred.

The session is cleaned up and the backend returns to scanning instead of normally exiting the process.

## FAP Launch

The backend launches:

```text
/ext/apps/Bluetooth/pc_monitor.fap
```

with the RPC launch argument.

An old development path such as:

```text
/ext/apps/Bluetooth/pc_monitor_dev.fap
```

must not remain hardcoded in the backend.

Using the wrong path can cause an old FAP to start or make the lifecycle appear to close/reopen unexpectedly.

## Bootstrap Timing

The FAP/bootstrap and backend include short settling delays so that the initial manually launched application can hand control to the RPC-launched instance cleanly.

The PC Monitor bootstrap delay was reduced during development to approximately:

```text
600 ms
```

The backend then allows additional settling time before issuing `App.Start`.

These values are implementation details and may need adjustment if the FAP lifecycle changes.

## Telemetry Update Interval

The intended telemetry update interval is approximately:

```text
1 second
```

This provides responsive monitoring without requiring unnecessarily aggressive BLE traffic or macOS sensor polling.

## Telemetry Packet

The current packet sent to PC Monitor is exactly **14 bytes**.

```text
Offset  Size  Type       Field
------  ----  ---------  -------------------
0       1     uint8      CPU usage %
1       2     uint16 LE  RAM total
3       1     uint8      RAM usage %
4       4     byte[4]    RAM unit
8       1     uint8      GPU usage %
9       1     uint8      Mac battery %
10      1     uint8      CPU temperature
11      1     uint8      GPU temperature
12      1     uint8      SSD temperature
13      1     uint8      Battery temperature
```

Rust serialization is equivalent to:

```rust
let mut data = [0u8; 14];

data[0] = info.cpu_usage;

let ram = info.ram_max.to_le_bytes();
data[1] = ram[0];
data[2] = ram[1];

data[3] = info.ram_usage;
data[4..8].copy_from_slice(&info.ram_unit);

data[8] = info.gpu_usage;
data[9] = info.battery_usage;

data[10] = info.cpu_temp;
data[11] = info.gpu_temp;
data[12] = info.ssd_temp;
data[13] = info.battery_temp;
```

The FAP parser and backend serializer must always use the same layout.

## RAM Encoding

RAM total is stored as a little-endian `u16`.

The backend represents the total in tenths of the selected unit.

For example:

```text
160 + "GB"
```

represents:

```text
16.0 GB
```

The current protocol reserves four bytes for the unit, typically equivalent to:

```text
GB\0\0
```

RAM usage percentage is calculated on the Mac before transmission.

## Sensor Sentinel Values

Some telemetry paths use:

```text
255
```

as an unavailable/not-yet-sampled `u8` sentinel.

The backend includes telemetry warm-up behavior to reduce invalid first samples, but consumers should still treat `255` as unavailable rather than as a real percentage or temperature.

## CPU Sampling

CPU utilization requires a meaningful sampling interval.

A first or immediately repeated CPU refresh can produce misleading values because CPU usage is calculated over a time delta.

If CPU remains unexpectedly near 100%, inspect the sampling interval and the `sysinfo` refresh behavior before assuming the Flipper protocol is at fault.

## Temperatures

The telemetry packet supports:

- CPU temperature
- GPU temperature
- SSD temperature
- battery temperature

The backend uses the available macOS telemetry implementation, including vendored `macmon` functionality where applicable.

A missing sensor should be represented as unavailable rather than replaced with an invented temperature.

## RPC Proxy

The backend exposes a local Unix socket:

```text
/tmp/flipper-pcmonitor-rpc.sock
```

Typical startup log:

```text
RPC proxy listening on /tmp/flipper-pcmonitor-rpc.sock
```

This allows companion local tooling to coordinate with the backend rather than independently competing for the same BLE connection.

## BLE Ownership

The backend is designed to be the primary owner of the Flipper BLE/RPC connection.

Other tools should avoid opening competing CoreBluetooth connections while PC Monitor is active.

This is especially relevant to the AirBattery integration: AirBattery does not connect directly to the Flipper.

## AirBattery Integration

The backend publishes the Flipper battery state to:

```text
/tmp/flipper-airbattery.json
```

using:

```rust
fn update_airbattery(battery_level: u8)
```

Values above 100 are rejected.

The JSON contains an AirBattery-compatible logical device entry similar to:

```json
[
  {
    "hasBattery": true,
    "deviceID": "YOUR_FLIPPER_DEVICE_ID",
    "deviceType": "general_bt",
    "deviceName": "Flipper Zero",
    "deviceModel": "Flipper Zero",
    "batteryLevel": 83,
    "isCharging": 0,
    "isCharged": false,
    "isPaused": false,
    "acPowered": false,
    "isHidden": false,
    "lowPower": false,
    "parentName": "",
    "lastUpdate": 0,
    "realUpdate": 0
  }
]
```

`lastUpdate` and `realUpdate` are populated with the current Unix timestamp.

### Atomic AirBattery publishing

The backend first writes:

```text
/tmp/flipper-airbattery.json.tmp
```

and then renames it to:

```text
/tmp/flipper-airbattery.json
```

This prevents the helper from observing a partially written JSON document.

## AirBattery: Disconnected Mode

When the Flipper is not actively connected through RPC, its battery percentage can be obtained from the PC Monitor advertising payload:

```text
46 5A 01 BAT FLAGS
```

The advertising battery value is passed to `update_airbattery()`.

This allows AirBattery to continue displaying the Flipper battery while no persistent RPC connection exists.

## AirBattery: Connected Mode

During an active BLE/RPC connection, advertising is not relied upon as the live battery source.

The backend obtains the available Flipper battery level through the active connection path and publishes it through the same:

```rust
update_airbattery(...)
```

function.

Therefore AirBattery sees one stable logical device regardless of whether the Flipper battery value came from advertising or from the connected session.

## Flipper AirBattery Helper

The backend intentionally does **not** write directly into another application's sandbox.

A separate component:

```text
Flipper AirBattery Helper.app
```

reads:

```text
/tmp/flipper-airbattery.json
```

and publishes it to:

```text
~/Library/Containers/com.lihaoyun6.AirBattery.widget/
Data/Documents/NearcastData/FlipperZero.json
```

The helper source is maintained separately in the suite under:

```text
airbattery-helper/
```

The helper removes stale AirBattery data when the backend JSON has not been updated for 60 seconds.

## Recovery and Reconnection

Transient BLE/RPC errors should normally result in local session recovery rather than process termination.

Recovery includes:

- disconnecting a broken RPC session;
- returning to scanning;
- restarting BLE discovery;
- ignoring duplicate disconnect callbacks;
- rejecting stale advertisements;
- avoiding unrelated BLE devices;
- allowing PC Monitor to be opened again without manually restarting the backend.

A process restart remains a last-resort watchdog action after repeated CoreBluetooth failures.

## Duplicate Disconnect Protection

CoreBluetooth can produce more than one disconnect-related event for a session.

The backend contains duplicate-disconnect protection so that a single physical disconnect does not trigger repeated scanner resets.

Typical log:

```text
Duplicate BLE disconnect event ignored
```

## Stale Advertising Protection

Immediately after PC Monitor closes, an old request advertisement may briefly remain observable.

The backend protects against immediately reconnecting to a stale request and allows the Flipper to return to its idle state.

## Debugging

Run manually for direct logs:

```bash
./target/release/flipper-pc-monitor-backend
```

Or inspect the LaunchAgent log:

```bash
tail -f /tmp/flipper-pcmonitor.log
```

Useful messages include:

```text
Scanning for Flipper RPC...
Advertising battery=83%, flags=0x01
State Scanning -> Connecting
Connected to Flipper RPC
State Connecting -> RpcReady
PC Monitor App.Start sent (RPC mode)
PC Monitor closed - backend idle
Flipper RPC disconnected
BLE scan restarted after disconnect
```

## Check AirBattery Output

Backend source JSON:

```bash
cat /tmp/flipper-airbattery.json
```

Metadata/freshness:

```bash
stat /tmp/flipper-airbattery.json
```

AirBattery destination:

```bash
cat \
~/Library/Containers/com.lihaoyun6.AirBattery.widget/Data/Documents/NearcastData/FlipperZero.json
```

## Troubleshooting

### Backend connects but PC Monitor does not start

Confirm that the backend launches:

```text
/ext/apps/Bluetooth/pc_monitor.fap
```

and that the current FAP exists at that path.

### `PC Monitor closed - backend idle` immediately after start

Check:

- FAP path;
- whether an old FAP is installed;
- RPC bootstrap timing;
- FAP crash logs;
- whether multiple `pc_monitor` manifests exist in the firmware tree.

### Backend reconnects repeatedly

Inspect advertising flags and verify that stale request advertisements are not being interpreted as a new user request.

### Wrong BLE device selected

Only remember a Flipper after validating the expected manufacturer payload/signature.

### CoreBluetooth encryption timeout

Verify that the Flipper is correctly paired and that another process is not competing for the BLE connection.

### AirBattery value disappears

Check:

```bash
stat /tmp/flipper-airbattery.json
```

The AirBattery helper intentionally removes the entry when the backend source has been stale for more than 60 seconds.

### AirBattery shows an old value

Verify that the backend is still updating:

```bash
watch -n 2 'cat /tmp/flipper-airbattery.json'
```

If `watch` is unavailable on macOS:

```bash
while true; do
    clear
    date
    cat /tmp/flipper-airbattery.json
    sleep 2
done
```

## Development Workflow

After changing the backend:

```bash
cargo check
cargo build --release
```

Stop the installed LaunchAgent:

```bash
launchctl bootout \
gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist \
2>/dev/null
```

Install the new binary:

```bash
sudo cp \
target/release/flipper-pc-monitor-backend \
/Applications/Flipper/flipper-pc-monitor-backend
```

Start the LaunchAgent:

```bash
launchctl bootstrap \
gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Watch:

```bash
tail -f /tmp/flipper-pcmonitor.log
```

## Source Backups

During development, temporary files such as:

```text
main.rs.before-*
*.bak
```

may be useful locally, but they should not be committed to the public repository.

Keep `.gitignore` configured to exclude development backups and Rust build output.

## Security / Privacy

The backend processes telemetry locally.

The AirBattery bridge is file-based and uses `/tmp` plus AirBattery's local NearCast container.

No cloud service is required by the PC Monitor backend for telemetry transport.

The Flipper device identifier and name currently present in the AirBattery JSON are implementation-specific constants. If the project is generalized for multiple users or multiple Flippers, these should become configurable or be discovered dynamically.

## Known Development Configuration

```text
Host:                 macOS / Apple Silicon
Flipper:              Flipper Zero
Firmware:             Momentum mntm-012
Firmware API:         87.1
Telemetry interval:   ~1 second
Telemetry packet:     14 bytes
FAP path:             /ext/apps/Bluetooth/pc_monitor.fap
RPC proxy:            /tmp/flipper-pcmonitor-rpc.sock
AirBattery IPC:       /tmp/flipper-airbattery.json
```

## Related Suite Components

The backend is one part of the complete repository:

```text
flipper-pc-monitor-suite/
├── flipper-fap/
├── mac-backend/
├── flipperble/
├── airbattery-helper/
├── firmware/
└── docs/
```

See the repository root `README.md` for the complete project architecture.

## Future Improvements

Potential backend improvements include:

- explicit telemetry protocol version;
- packet sequence counter;
- CRC-8 or application-level integrity field;
- validity bitmask instead of `255` sentinels;
- dynamic Flipper identity instead of hardcoded AirBattery identifiers;
- additional sensor telemetry;
- improved CPU sampling;
- native BLE firmware-log bridge;
- automated reconnect regression tests;
- automated compatibility tests against newer Momentum versions.

## License

See the repository and component license files.

Preserve the applicable license and attribution for vendored dependencies such as `macmon`.

## Configuration

The backend supports the following environment variables:

```text
FLIPPER_DEVICE_NAME
FLIPPER_DEVICE_ID
```

Example:

```bash
export FLIPPER_DEVICE_NAME="My Flipper"
export FLIPPER_DEVICE_ID="my-flipper-01"
```

`FLIPPER_DEVICE_NAME` is used for BLE device matching.

`FLIPPER_DEVICE_ID` is used as the logical AirBattery device identifier.

For persistent LaunchAgent configuration, define these values under
`EnvironmentVariables` in the LaunchAgent plist instead of exporting them
manually in a shell.
