# Flipper AirBattery Helper

A small macOS background helper that bridges the Flipper Zero battery state
published by the PC Monitor backend into AirBattery's NearCast data directory.

## Purpose

The PC Monitor backend is the sole owner of the Flipper BLE/RPC connection.

AirBattery therefore does not connect to the Flipper directly. Instead:

```text
Flipper Zero
    │
    ▼
PC Monitor backend
    │
    ▼
/tmp/flipper-airbattery.json
    │
    ▼
Flipper AirBattery Helper
    │
    ▼
AirBattery NearCast cache
```

## Source file

```text
main.swift
```

## Input

The helper reads:

```text
/tmp/flipper-airbattery.json
```

If the file does not exist, the helper removes the AirBattery entry and exits.

## Output

The helper writes:

```text
~/Library/Containers/com.lihaoyun6.AirBattery.widget/
Data/Documents/NearcastData/FlipperZero.json
```

It first writes:

```text
FlipperZero.json.tmp
```

and then renames it to the final file.

This prevents AirBattery from observing a partially-written JSON file.

## Offline handling

The source JSON must have been updated within the last 60 seconds.

If it is older than 60 seconds, the helper removes the AirBattery entry and
exits successfully.

## Connected vs disconnected behavior

The helper itself does not care whether the Flipper is connected.

That decision is handled by the PC Monitor backend:

- **Disconnected:** battery is read from the BLE advertising payload
  `46 5A 01 BAT FLAGS`.
- **Connected:** battery is obtained through the active BLE/RPC/GATT session.

Both paths call the same backend `update_airbattery()` function and therefore
produce the same JSON schema.

## macOS application bundle

Development bundle metadata:

```text
CFBundleDisplayName:        Flipper AirBattery Helper
CFBundleExecutable:         FlipperAirBatteryHelper
CFBundleIdentifier:         io.github.vasilevgeorge.FlipperAirBatteryHelper
CFBundleShortVersionString: 1.0
CFBundleVersion:            1
LSUIElement:                true
```

`LSUIElement=true` makes the helper a background application without a normal
Dock icon.

## Build example

A simple Swift build can be created with:

```bash
swiftc main.swift -o FlipperAirBatteryHelper
```

For the full `.app` bundle, place the executable under:

```text
Flipper AirBattery Helper.app/Contents/MacOS/FlipperAirBatteryHelper
```

and include the corresponding `Info.plist`.

## Installed location used during development

```text
/Applications/Flipper AirBattery Helper.app
```

## Troubleshooting

Check whether AirBattery is running:

```bash
pgrep -fl -i AirBattery
```

Check whether the helper is running:

```bash
pgrep -fl FlipperAirBatteryHelper
```

Inspect the backend JSON:

```bash
cat /tmp/flipper-airbattery.json
```

Check its freshness:

```bash
stat /tmp/flipper-airbattery.json
```

Inspect the AirBattery entry:

```bash
cat \
~/Library/Containers/com.lihaoyun6.AirBattery.widget/Data/Documents/NearcastData/FlipperZero.json
```

If the source file is older than 60 seconds, the helper intentionally removes
the destination entry.
