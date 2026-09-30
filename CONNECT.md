# Connecting a Flipper Zero

This guide explains how to connect the macOS PC Monitor backend and `flipperble`
to a Flipper Zero, including cases where the Bluetooth device name changes.

The project does **not** require a fixed Flipper Bluetooth name.

The important configuration variables are:

```text
FLIPPER_DEVICE_NAME
FLIPPER_DEVICE_ID
```

- `FLIPPER_DEVICE_NAME` is used to find the Flipper over Bluetooth.
- `FLIPPER_DEVICE_ID` is used as the logical device identifier for AirBattery.

## 1. Find and verify the Flipper

If you know the current Bluetooth name, test it with:

```bash
flipperble --device "MyFlipper" info
```

A successful response should contain information similar to:

```text
hardware_name                  : MyFlipper
hardware_model                 : Flipper Zero
firmware_version               : mntm-dev
radio_alive                    : true
radio_ble_mac                  : AABBCCDDEEFF
```

The important values are:

```text
hardware_name = MyFlipper
radio_ble_mac = AABBCCDDEEFF
```

Configure them as:

```text
FLIPPER_DEVICE_NAME=MyFlipper
FLIPPER_DEVICE_ID=AABBCCDDEEFF
```

## 2. flipperble commands

Device information:

```bash
flipperble --device "MyFlipper" info
```

Battery:

```bash
flipperble --device "MyFlipper" battery
```

Advertising battery:

```bash
flipperble --device "MyFlipper" advbattery
```

List storage:

```bash
flipperble --device "MyFlipper" ls /ext
```

Upload the PC Monitor FAP:

```bash
flipperble --device "MyFlipper" put \
pc_monitor.fap \
/ext/apps/Bluetooth/pc_monitor.fap
```

`flipperble` requires a command. For example, this is incomplete:

```bash
flipperble --device "MyFlipper"
```

Use `info`, `battery`, `ls`, `put`, `run`, or another supported command after it.

## 3. Configure the PC Monitor backend

The LaunchAgent is normally located at:

```text
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Inspect it with:

```bash
plutil -p ~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Set the Bluetooth name:

```bash
/usr/libexec/PlistBuddy \
-c "Set :EnvironmentVariables:FLIPPER_DEVICE_NAME MyFlipper" \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Set the AirBattery device ID:

```bash
/usr/libexec/PlistBuddy \
-c "Set :EnvironmentVariables:FLIPPER_DEVICE_ID AABBCCDDEEFF" \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Replace `MyFlipper` and `AABBCCDDEEFF` with the values returned by
`flipperble info`.

If the keys do not exist yet, add them instead:

```bash
/usr/libexec/PlistBuddy \
-c "Add :EnvironmentVariables:FLIPPER_DEVICE_NAME string MyFlipper" \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist

/usr/libexec/PlistBuddy \
-c "Add :EnvironmentVariables:FLIPPER_DEVICE_ID string AABBCCDDEEFF" \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

## 4. Restart the backend

After changing the configuration:

```bash
launchctl bootout gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist 2>/dev/null

launchctl bootstrap gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Verify that the backend is running:

```bash
pgrep -fl flipper-pc-monitor
```

## 5. Verify the loaded configuration

Check the environment received by the LaunchAgent:

```bash
launchctl print gui/$(id -u)/com.flipper.pcmonitor \
| grep -A6 -B2 FLIPPER
```

Example:

```text
FLIPPER_DEVICE_NAME => MyFlipper
FLIPPER_DEVICE_ID => AABBCCDDEEFF
```

## 6. Start PC Monitor

Watch the backend log:

```bash
tail -f /tmp/flipper-pcmonitor.log
```

Then manually open **PC Monitor** on the Flipper.

A normal connection looks similar to:

```text
Scanning for Flipper RPC...
Advertising battery=83%, flags=0x00
PC Monitor request detected - connecting RPC (flags=0x01)
Connected to Flipper RPC
RPC ready - waiting for manual PC Monitor (self-attach)
PC Monitor manually opened
Sending REAL telemetry over RPC...
```

Telemetry should then appear approximately once per second:

```text
CPU=16 RAM=71% GPU=29 BAT=100%
CPU=13 RAM=71% GPU=26 BAT=100%
CPU=17 RAM=71% GPU=27 BAT=100%
```

## 7. Verify AirBattery

The backend publishes the latest Flipper battery state to:

```text
/tmp/flipper-airbattery.json
```

Check it with:

```bash
cat /tmp/flipper-airbattery.json
```

Example:

```json
[
  {
    "hasBattery": true,
    "deviceID": "AABBCCDDEEFF",
    "deviceType": "general_bt",
    "deviceName": "MyFlipper",
    "deviceModel": "Flipper Zero",
    "batteryLevel": 83
  }
]
```

## 8. If the Bluetooth name changes

Renaming the Flipper does **not** require recompiling the backend.

For example, if the new name is `LabFlipper`:

```bash
flipperble --device "LabFlipper" info
```

Update the backend:

```bash
/usr/libexec/PlistBuddy \
-c "Set :EnvironmentVariables:FLIPPER_DEVICE_NAME LabFlipper" \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Restart it:

```bash
launchctl bootout gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist 2>/dev/null

launchctl bootstrap gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

If only the Bluetooth name changed, the physical device's `radio_ble_mac`
normally remains the same, so `FLIPPER_DEVICE_ID` does not need to change.

## 9. If the physical Flipper changes

Query the replacement device:

```bash
flipperble --device "NewFlipper" info
```

Use its reported `radio_ble_mac` for `FLIPPER_DEVICE_ID`:

```bash
/usr/libexec/PlistBuddy \
-c "Set :EnvironmentVariables:FLIPPER_DEVICE_NAME NewFlipper" \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist

/usr/libexec/PlistBuddy \
-c "Set :EnvironmentVariables:FLIPPER_DEVICE_ID AABBCCDDEEFF" \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

Restart the backend afterward.

## 10. Configure the flipperble default name

`flipperble` supports the `FLIPPER_DEVICE_NAME` environment variable.

For the current shell:

```bash
export FLIPPER_DEVICE_NAME="MyFlipper"
```

Then:

```bash
flipperble info
flipperble battery
flipperble ls /ext
```

can be used without `--device`.

For persistent zsh configuration:

```bash
echo 'export FLIPPER_DEVICE_NAME="MyFlipper"' >> ~/.zshrc
source ~/.zshrc
```

## 11. Manual backend test

To test another Flipper without permanently changing the LaunchAgent,
first stop the service:

```bash
launchctl bootout gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist 2>/dev/null
```

Run the backend manually:

```bash
FLIPPER_DEVICE_NAME="MyFlipper" \
FLIPPER_DEVICE_ID="AABBCCDDEEFF" \
/Applications/Flipper/flipper-pc-monitor-backend
```

After testing, start the LaunchAgent again:

```bash
launchctl bootstrap gui/$(id -u) \
~/Library/LaunchAgents/com.flipper.pcmonitor.plist
```

## Troubleshooting

### Flipper not found

If you see:

```text
ERROR: Flipper 'MyFlipper' not found
```

verify the current Bluetooth name:

```bash
flipperble --device "ActualDeviceName" info
```

### Check backend process

```bash
pgrep -fl flipper-pc-monitor
```

### Check backend activity

```bash
tail -100 /tmp/flipper-pcmonitor.log
```

### Check backend errors

```bash
tail -100 /tmp/flipper-pcmonitor-error.log
```

### Check AirBattery data

```bash
cat /tmp/flipper-airbattery.json
```

### Check LaunchAgent environment

```bash
launchctl print gui/$(id -u)/com.flipper.pcmonitor \
| grep -A6 -B2 FLIPPER
```

## Configuration summary

| Variable | Purpose | Example |
|---|---|---|
| `FLIPPER_DEVICE_NAME` | Bluetooth device matching | `MyFlipper` |
| `FLIPPER_DEVICE_ID` | AirBattery device identity | `AABBCCDDEEFF` |

In short:

```text
FLIPPER_DEVICE_NAME
        |
        +--> Used to FIND the Flipper over Bluetooth

FLIPPER_DEVICE_ID
        |
        +--> Used to IDENTIFY the Flipper in AirBattery
```

Changing either value does not require rebuilding the backend.
