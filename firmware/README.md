# Momentum Firmware Patches

This directory contains the Momentum firmware modifications used by the **flipper-pc-monitor-suite** project.

There are currently two independent firmware-side modifications:

1. **PC Monitor BLE Advertising Patch** — improves BLE advertising state updates used by the macOS PC Monitor backend.
2. **FindMyFlipper Dynamic Battery Patch** — adds live battery reporting and test controls to the FindMyFlipper Extra Beacon.

The patches are separate and can be applied independently.

---

# 1. PC Monitor BLE Advertising Patch

PC Monitor uses a small Momentum firmware patch to provide reliable BLE advertising state updates that the macOS backend can monitor before opening an RPC connection.

## Modified Firmware Files

The patch modifies only these Momentum firmware files:

```text
targets/f7/ble_glue/gap.c
targets/f7/furi_hal/furi_hal_bt.c
```

The PC Monitor FAP itself is not included in this patch. Its source is located separately in the `flipper-fap/` directory of this repository.

## BLE Advertising Protocol

PC Monitor uses manufacturer-specific BLE advertising data containing the following signature:

```text
46 5A 01 BAT FLAGS
```

The fields are:

| Field | Size | Description |
|---|---:|---|
| `46 5A` | 2 bytes | PC Monitor manufacturer signature (`FZ`) |
| `01` | 1 byte | Advertising protocol version |
| `BAT` | 1 byte | Flipper battery percentage |
| `FLAGS` | 1 byte | PC Monitor / RPC request state |

The macOS backend watches this advertising payload and can determine the Flipper battery level and whether PC Monitor is requesting an RPC connection.

## Changes to `gap.c`

The patch adds an additional protocol-version check when updating the battery value in the manufacturer-specific advertising payload.

The payload must match:

```c
gap->service.mfg_data[4] == 0x46 &&
gap->service.mfg_data[5] == 0x5A &&
gap->service.mfg_data[6] == 0x01
```

This makes the battery update specific to PC Monitor advertising protocol version `0x01` and prevents unrelated manufacturer payloads beginning with `46 5A` from being modified.

## Changes to `furi_hal_bt.c`

The implementation of:

```c
furi_hal_bt_update_advertising_flags(uint8_t flags)
```

was hardened to make advertising updates safer.

The modified function:

- always updates the cached manufacturer payload first;
- checks whether the Bluetooth stack is active;
- does not attempt an HCI advertising restart when Bluetooth is inactive;
- remembers the last flags value;
- does not restart advertising when the requested flags value has not changed;
- restarts advertising only when GAP is currently in:
  - `GapStateAdvFast`
  - `GapStateAdvLowPower`
- uses the normal GAP stop/start lifecycle rather than directly manipulating raw HCI scan-response data.

This avoids unnecessary advertising interruptions and reduces the chance of disturbing an RPC connection while PC Monitor is starting, stopping, or reconnecting.

## Patch File

The PC Monitor patch is stored at:

```text
firmware/patches/momentum-mntm-012-pc-monitor.patch
```

## Applying the PC Monitor Patch

From a compatible Momentum firmware source tree, first verify that the patch applies cleanly:

```bash
git apply --check /path/to/momentum-mntm-012-pc-monitor.patch
```

If no errors are displayed:

```bash
git apply /path/to/momentum-mntm-012-pc-monitor.patch
```

Verify:

```bash
git diff --stat
git diff -- targets/f7/ble_glue/gap.c
git diff -- targets/f7/furi_hal/furi_hal_bt.c
```

---

# 2. FindMyFlipper Dynamic Battery Patch

The FindMyFlipper patch adds dynamic battery reporting to the Apple Find My advertisement and provides a small CLI interface for testing the advertised battery state at runtime.

## Patch File

The patch is stored at:

```text
firmware/findmyflipper-dynamic-battery.patch
```

Source commit:

```text
481d3cf4d findmy: add dynamic battery reporting and test CLI
```

It was developed against Momentum firmware:

```text
mntm-012
```

with:

```text
Firmware API 87.1
protobuf 0.25
```

## Modified Firmware Files

The FindMyFlipper patch modifies:

```text
applications/system/findmy/findmy_startup.c
applications/system/findmy/findmy_state.c
applications/system/findmy/findmy_state.h
```

## What It Adds

The patch adds:

- dynamic Find My battery reporting;
- a background battery refresh worker;
- live Extra Beacon payload updates;
- runtime battery test commands through the Flipper CLI;
- no periodic beacon stop/start during normal battery refresh;
- automatic restoration of the real battery category after a temporary test override.

---

## Find My Battery Mapping

The Apple Offline Finding advertisement contains a battery-status byte.

The patch maps the real Flipper battery percentage to four states:

| Flipper battery | Find My byte | State |
|---|---:|---|
| 81–100% | `0x00` | Full |
| 51–80% | `0x50` | Medium |
| 21–50% | `0xA0` | Low |
| 0–20% | `0xF0` | Critical |

Example advertisement:

```text
4C00121900CB60BFE2325E9A423BEF5BC4F13E393EB77704AD2D7B036E
```

The battery byte is:

```text
4C 00 12 19 00 ...
            ^^
```

For example:

```text
00 = Full
50 = Medium
A0 = Low
F0 = Critical
```

A live update can therefore look like:

```text
before:
4C00121900CB60...

after:
4C00121950CB60...
```

Only the battery-status byte changes.

---

## Background Battery Worker

The startup component creates a background worker named:

```text
FindMyBattery
```

The current refresh interval is:

```text
60 seconds
```

The worker:

1. loads the persisted FindMy state;
2. confirms that the configured tag type supports the battery update;
3. confirms that the beacon is configured as active;
4. confirms that the Extra Beacon is currently active;
5. recalculates the battery category;
6. updates the live advertising payload when the category changes.

---

## Live Extra Beacon Update

The patch updates the active Extra Beacon through:

```c
furi_hal_bt_extra_beacon_set_data(...)
```

The Momentum BLE implementation ultimately updates the active additional beacon data through the GAP layer.

This allows the Find My battery byte to change while the Extra Beacon remains active.

A periodic:

```text
stop beacon
set data
start beacon
```

cycle is therefore not required for the battery refresh.

---

## Battery Refresh API

The patch adds:

```c
bool findmy_state_refresh_battery(FindMyState* state);
```

The function recalculates the battery state and updates the active Extra Beacon when required.

---

# FindMyFlipper CLI

The patch registers the firmware CLI command:

```text
findmy_battery
```

Supported commands:

```text
findmy_battery show
findmy_battery auto
findmy_battery 00
findmy_battery 50
findmy_battery A0
findmy_battery F0
```

## Show Current Value

```text
findmy_battery show
```

Example:

```text
Current FindMy battery byte: 0x00
```

## Force Full

```text
findmy_battery 00
```

## Force Medium

```text
findmy_battery 50
```

## Force Low

```text
findmy_battery A0
```

## Force Critical

```text
findmy_battery F0
```

Lowercase hexadecimal input is also accepted where applicable.

## Restore Automatic Mode

```text
findmy_battery auto
```

This returns the active Find My advertisement to the battery category calculated from the real Flipper battery.

The background worker also periodically recalculates the real value, so a temporary forced test value is not intended to remain permanent.

---

# Validating FindMyFlipper with `flipperble`

The `flipperble` utility can use an attached Marauder-compatible ESP32 as a passive BLE scanner.

Start:

```bash
flipperble --device ZER0TYEC marauder-findmy
```

Example:

```text
15:42:26.560  DE:43:F3:5E:0A:2B  -69 dBm  len=31  4C00121900CB60...
```

Then force another battery category from the Flipper CLI:

```text
findmy_battery 50
```

The passive monitor should observe:

```text
15:43:56.239  DE:43:F3:5E:0A:2B  -64 dBm  len=31  4C00121950CB60...
```

This confirms that the active Extra Beacon payload changed without rebooting the Flipper.

If the real battery is above 80%, the background worker should later restore:

```text
0x50 -> 0x00
```

---

# FindMyFlipper State Storage

The active FindMy configuration is stored on the Flipper SD card at:

```text
/ext/apps_data/findmy/findmy_state.txt
```

The firmware defines:

```c
#define FINDMY_STATE_DIR  EXT_PATH("apps_data/findmy")
#define FINDMY_STATE_PATH FINDMY_STATE_DIR "/findmy_state.txt"
```

The specific Find My identity/payload is runtime state and is not hardcoded into this patch.

It can be inspected with:

```bash
flipperble \
  --device ZER0TYEC \
  cat /ext/apps_data/findmy/findmy_state.txt
```

---

# Applying the FindMyFlipper Patch

From the Momentum firmware root, first verify:

```bash
git apply --check /path/to/findmyflipper-dynamic-battery.patch
```

Then apply:

```bash
git apply /path/to/findmyflipper-dynamic-battery.patch
```

Verify:

```bash
git diff --stat

git diff -- \
  applications/system/findmy/findmy_startup.c \
  applications/system/findmy/findmy_state.c \
  applications/system/findmy/findmy_state.h
```

Because this file was generated with `git format-patch`, it can also be applied as a commit with:

```bash
git am /path/to/findmyflipper-dynamic-battery.patch
```

Use either `git apply` or `git am`, depending on whether you want only the file changes or also the original commit metadata.

---

# Applying Both Patches

The two patches modify different firmware files and are intended to remain logically independent.

From the Momentum firmware root:

```bash
git apply --check \
  /path/to/momentum-mntm-012-pc-monitor.patch

git apply --check \
  /path/to/findmyflipper-dynamic-battery.patch
```

If both checks succeed:

```bash
git apply /path/to/momentum-mntm-012-pc-monitor.patch
git apply /path/to/findmyflipper-dynamic-battery.patch
```

Then inspect everything:

```bash
git status --short
git diff --stat
```

Expected modified areas:

```text
applications/system/findmy/
targets/f7/ble_glue/gap.c
targets/f7/furi_hal/furi_hal_bt.c
```

---

# Building Momentum

After applying the required patches:

```bash
./fbt
```

For a full USB flash when appropriate:

```bash
./fbt flash_usb_full
```

Check the USB CDC device if needed:

```bash
ls -l /dev/cu.usbmodem*
```

---

# Reverting the PC Monitor Patch

If the changes have not been committed:

```bash
git restore \
  targets/f7/ble_glue/gap.c \
  targets/f7/furi_hal/furi_hal_bt.c
```

---

# Reverting the FindMyFlipper Patch

If the changes were applied but not committed:

```bash
git restore \
  applications/system/findmy/findmy_startup.c \
  applications/system/findmy/findmy_state.c \
  applications/system/findmy/findmy_state.h
```

If the original FindMyFlipper commit exists in the local repository and should be reverted as a Git commit:

```bash
git revert 481d3cf4d
```

---

# Exporting the FindMyFlipper Patch Again

The patch can be regenerated from the original commit with:

```bash
git format-patch -1 481d3cf4d --stdout > \
findmyflipper-dynamic-battery.patch
```

---

# Compatibility

Both modifications were developed against:

```text
Momentum Firmware mntm-012
```

Momentum's Bluetooth, GAP, FindMy, CLI, and Extra Beacon implementations can change between releases.

Before applying either patch to another Momentum revision, always run:

```bash
git apply --check PATCH_FILE
```

A failed check does not automatically mean the feature is incompatible. It means the patch must be reviewed against the newer source before being applied.

The FindMyFlipper CLI implementation also depends on the newer Momentum CLI registry API using:

```c
CliRegistry*
cli_registry_add_command(...)
PipeSide*
```

rather than the older `Cli*` / `cli_add_command(...)` API.

---

# Runtime Notes

## Flipper Lock

Interactive RPC can become unavailable while the Flipper itself is locked.

Observed behavior:

```text
Flipper unlocked -> RPC works
Flipper locked   -> RPC timeout
Flipper unlocked -> RPC works again
```

This does not necessarily stop passive BLE advertising or the Find My Extra Beacon.

---

# Project Layout

The relevant project structure is:

```text
flipper-pc-monitor-suite/
├── flipper-fap/
│   └── PC Monitor application for Flipper Zero
├── mac-backend/
│   └── macOS telemetry and shared BLE/RPC backend
├── flipperble/
│   └── BLE/USB RPC command-line client
├── marauder-rpc-bridge/
│   └── Flipper RPC <-> UART bridge for ESP32 Marauder
└── firmware/
    ├── README.md
    ├── findmyflipper-dynamic-battery.patch
    └── patches/
        └── momentum-mntm-012-pc-monitor.patch
```

The PC Monitor firmware patch provides the BLE advertising behavior used by the macOS backend.

The FindMyFlipper patch provides independent dynamic battery reporting for the Find My Extra Beacon.

`flipperble` and `marauder-rpc-bridge` can be used to observe and validate the resulting BLE advertisements.

---

# Summary

## PC Monitor patch

```text
targets/f7/ble_glue/gap.c
targets/f7/furi_hal/furi_hal_bt.c
```

Provides:

```text
46 5A 01 BAT FLAGS
```

advertising support and safer advertising flag updates.

## FindMyFlipper patch

```text
applications/system/findmy/findmy_startup.c
applications/system/findmy/findmy_state.c
applications/system/findmy/findmy_state.h
```

Provides:

- real Flipper battery -> Find My battery mapping;
- 60-second background refresh;
- live Extra Beacon updates;
- `findmy_battery` test CLI;
- passive validation through `flipperble marauder-findmy`.

---

# License

These patches modify code from the Momentum firmware project.

Refer to Momentum and upstream Flipper Zero licensing for the original firmware source. Repository-specific components remain subject to the license terms included with this project.
