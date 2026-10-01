# Firmware Patches

This directory contains firmware-side patches used by the **flipper-pc-monitor-suite** project.

The current FindMyFlipper patch adds dynamic battery reporting to the Apple Find My advertisement and a small CLI test interface for validating the advertised battery state at runtime.

---

# FindMyFlipper Dynamic Battery Patch

File:

```text
findmyflipper-dynamic-battery.patch
```

Source commit:

```text
481d3cf4d findmy: add dynamic battery reporting and test CLI
```

The patch was created from a Momentum firmware tree based on:

```text
mntm-012
```

and developed against:

```text
Firmware API 87.1
protobuf 0.25
```

The patch modifies:

```text
applications/system/findmy/findmy_startup.c
applications/system/findmy/findmy_state.c
applications/system/findmy/findmy_state.h
```

---

## What the Patch Adds

The patch adds:

- dynamic Find My battery reporting;
- a background battery refresh worker;
- live Extra Beacon payload updates;
- runtime battery test commands through the Flipper CLI;
- no periodic beacon stop/start during battery refresh;
- automatic restoration of the real battery category after a test override.

---

# Battery Reporting

The Find My advertisement contains a battery-status byte in the Apple Offline Finding payload.

The patch maps the real Flipper battery percentage to four Find My battery states:

| Flipper battery | Find My byte | State |
|---|---:|---|
| 81-100% | `0x00` | Full |
| 51-80% | `0x50` | Medium |
| 21-50% | `0xA0` | Low |
| 0-20% | `0xF0` | Critical |

The relevant constants are:

```c
#define BATTERY_FULL     0x00
#define BATTERY_MEDIUM   0x50
#define BATTERY_LOW      0xA0
#define BATTERY_CRITICAL 0xF0
```

---

# Example Find My Advertisement

A captured Apple Find My advertisement can look like:

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

A live test may therefore show:

```text
before:
4C00121900CB60...

after:
4C00121950CB60...
```

Only the battery-status byte changes.

---

# Background Battery Worker

The startup component starts a background worker named:

```text
FindMyBattery
```

The worker checks the battery periodically.

Current interval:

```c
#define FINDMY_BATTERY_REFRESH_MS (60 * 1000)
```

That is:

```text
60 seconds
```

The worker:

1. checks that BLE/GATT support is available;
2. loads the persisted FindMy state;
3. confirms that the beacon is configured as active;
4. recalculates the battery category;
5. updates the live advertisement only when required.

---

# Live Advertisement Update

The patch updates the active Extra Beacon through:

```c
furi_hal_bt_extra_beacon_set_data(...)
```

without restarting the beacon.

The underlying Momentum implementation calls:

```c
gap_extra_beacon_set_data(...)
```

which ultimately calls:

```c
aci_gap_additional_beacon_set_data(...)
```

This means the advertising payload can be updated while the Extra Beacon is running.

No periodic:

```text
stop
set data
start
```

cycle is required for battery refresh.

---

# Battery Refresh Function

The patch adds:

```c
bool findmy_state_refresh_battery(FindMyState* state);
```

The function:

- supports Apple Find My tags;
- requires the persisted beacon to be active;
- requires the Extra Beacon to currently be active;
- calculates the battery category from the real Flipper battery;
- updates only the advertising payload;
- returns `true` when the advertised battery category was changed.

---

# Runtime CLI Test Command

The patch registers:

```text
findmy_battery
```

in the Flipper CLI.

Show it with:

```text
help
```

Expected entry:

```text
findmy_battery
```

---

## Show Current Advertised Battery

```text
findmy_battery show
```

Example:

```text
Current FindMy battery byte: 0x00
```

---

## Force Full

```text
findmy_battery 00
```

Expected result:

```text
FindMy battery byte set to 0x00
```

---

## Force Medium

```text
findmy_battery 50
```

Expected result:

```text
FindMy battery byte set to 0x50
```

---

## Force Low

```text
findmy_battery A0
```

Lowercase is also accepted:

```text
findmy_battery a0
```

---

## Force Critical

```text
findmy_battery F0
```

Lowercase is also accepted:

```text
findmy_battery f0
```

---

## Return to Automatic Battery Reporting

```text
findmy_battery auto
```

This reloads the normal FindMy state and recalculates the battery category from the real Flipper battery.

Example:

```text
FindMy battery returned to automatic mode: 0x00
```

---

# Validating the Patch

A convenient validation setup is the Marauder Find My monitor included in `flipperble`.

Start:

```bash
flipperble --device ZER0TYEC marauder-findmy
```

Example initial output:

```text
15:42:26.560  DE:43:F3:5E:0A:2B   -69 dBm  len=31  4C00121900CB60...
```

Then from the Flipper CLI:

```text
findmy_battery 50
```

Run or continue the monitor.

Expected result:

```text
15:43:56.239  DE:43:F3:5E:0A:2B   -64 dBm  len=31  4C00121950CB60...
```

This confirms that the Extra Beacon payload was updated live.

If the real Flipper battery is above 80%, the background worker should later restore:

```text
0x50 -> 0x00
```

without rebooting the Flipper and without manually reopening FindMy.

---

# FindMy State Storage

The active FindMy configuration is stored on the Flipper SD card at:

```text
/ext/apps_data/findmy/findmy_state.txt
```

This path comes from:

```c
#define FINDMY_STATE_DIR  EXT_PATH("apps_data/findmy")
#define FINDMY_STATE_PATH FINDMY_STATE_DIR "/findmy_state.txt"
```

The specific Find My payload is runtime state and is not hardcoded in the firmware source.

You can inspect the state with:

```bash
flipperble \
  --device ZER0TYEC \
  cat /ext/apps_data/findmy/findmy_state.txt
```

---

# Applying the Patch

From the Momentum firmware source root:

```bash
git apply /path/to/findmyflipper-dynamic-battery.patch
```

Alternatively:

```bash
patch -p1 < /path/to/findmyflipper-dynamic-battery.patch
```

Verify:

```bash
git diff -- \
  applications/system/findmy/findmy_startup.c \
  applications/system/findmy/findmy_state.c \
  applications/system/findmy/findmy_state.h
```

---

# Building

From the Momentum firmware root:

```bash
./fbt
```

If the build succeeds, flash through USB:

```bash
./fbt flash_usb_full
```

The exact USB device may reconnect during flashing.

If needed, check:

```bash
ls -l /dev/cu.usbmodem*
```

---

# CLI API Compatibility

The current Momentum branch uses the newer CLI registry API.

The patch therefore uses:

```c
CliRegistry*
```

and:

```c
cli_registry_add_command(...)
```

instead of the older:

```c
Cli*
cli_add_command(...)
```

The callback uses:

```c
PipeSide* pipe
```

This is important when porting the patch to another Momentum or upstream firmware revision.

---

# Known Runtime Behavior

## Flipper Locked

Interactive RPC may time out while the Flipper itself is locked.

Observed:

```text
unlocked -> RPC works
locked   -> RPC timeout
unlocked -> RPC works again
```

This does not necessarily stop the Find My Extra Beacon.

The beacon can continue advertising while interactive RPC is unavailable.

---

# Reverting the Patch

If the patch was applied with Git and has not been committed:

```bash
git restore \
  applications/system/findmy/findmy_startup.c \
  applications/system/findmy/findmy_state.c \
  applications/system/findmy/findmy_state.h
```

If it was committed, revert the commit instead:

```bash
git revert 481d3cf4d
```

Or reset/cherry-pick as appropriate for your branch workflow.

---

# Exporting the Patch Again

To regenerate the patch from the original commit:

```bash
git format-patch -1 481d3cf4d --stdout > \
findmyflipper-dynamic-battery.patch
```

---

# Related Project Components

The repository also contains:

```text
flipperble/
    Host-side Flipper BLE/USB RPC CLI

marauder-rpc-bridge/
    Flipper RPC <-> UART bridge for ESP32 Marauder

mac-backend/
    Shared macOS BLE RPC backend

flipper-fap/
    PC Monitor Flipper application
```

The FindMy battery patch is firmware-side functionality and can be used independently, but `flipperble marauder-findmy` is useful for validating the resulting Apple Find My advertisement.

---

# Notes

The test CLI changes only the currently advertised battery byte.

It is intended for development and verification.

The normal background worker always derives the automatic value from the real Flipper battery level.

---

# License

This patch modifies code from the Momentum firmware project.

Refer to the Momentum and upstream Flipper Zero repositories for the licenses that apply to the original firmware source. Repository-specific additions remain subject to the license terms included with this project.
