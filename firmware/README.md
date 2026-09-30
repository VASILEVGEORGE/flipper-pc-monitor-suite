# Momentum Firmware Patch

PC Monitor uses a small Momentum firmware patch to provide reliable BLE
advertising state updates that the macOS backend can monitor before opening
an RPC connection.

## Modified Firmware Files

The patch modifies only these Momentum firmware files:

- `targets/f7/ble_glue/gap.c`
- `targets/f7/furi_hal/furi_hal_bt.c`

The PC Monitor FAP itself is not included in this patch. Its source is located
separately in the `flipper-fap/` directory of this repository.

## BLE Advertising Protocol

PC Monitor uses manufacturer-specific BLE advertising data containing the
following signature:

```text
46 5A 01 BAT FLAGS
```

The fields are:

| Field | Size | Description |
|---|---:|---|
| `46 5A` | 2 bytes | PC Monitor manufacturer signature |
| `01` | 1 byte | Advertising protocol version |
| `BAT` | 1 byte | Flipper battery percentage |
| `FLAGS` | 1 byte | PC Monitor / RPC request state |

The macOS backend watches this advertising payload and can determine the
Flipper battery level and whether PC Monitor is requesting an RPC connection.

## Changes to `gap.c`

The patch adds an additional protocol-version check when updating the battery
value in the manufacturer-specific advertising payload.

The payload must match:

```c
gap->service.mfg_data[4] == 0x46 &&
gap->service.mfg_data[5] == 0x5A &&
gap->service.mfg_data[6] == 0x01
```

This makes the battery update specific to PC Monitor advertising protocol
version `0x01` and prevents unrelated manufacturer payloads beginning with
`46 5A` from being modified.

## Changes to `furi_hal_bt.c`

The implementation of:

```c
furi_hal_bt_update_advertising_flags(uint8_t flags)
```

was hardened to make advertising updates safer.

The modified function:

1. Always updates the cached manufacturer payload first.
2. Checks whether the Bluetooth stack is active.
3. Does not attempt an HCI advertising restart when Bluetooth is inactive.
4. Remembers the last flags value.
5. Does not restart advertising when the requested flags value has not changed.
6. Restarts advertising only when GAP is currently in:
   - `GapStateAdvFast`
   - `GapStateAdvLowPower`
7. Uses the normal GAP stop/start lifecycle rather than directly manipulating
   the raw HCI scan-response data.

This avoids unnecessary advertising interruptions and reduces the chance of
disturbing an RPC connection while PC Monitor is starting, stopping or
reconnecting.

## Patch File

The patch is stored at:

```text
firmware/patches/momentum-mntm-012-pc-monitor.patch
```

## Applying the Patch

Clone or enter a compatible Momentum Firmware source tree.

First verify that the patch applies cleanly:

```bash
git apply --check /path/to/momentum-mntm-012-pc-monitor.patch
```

If no errors are displayed, apply it:

```bash
git apply /path/to/momentum-mntm-012-pc-monitor.patch
```

Then verify the modifications:

```bash
git diff --stat
git diff -- targets/f7/ble_glue/gap.c
git diff -- targets/f7/furi_hal/furi_hal_bt.c
```

Build Momentum normally:

```bash
./fbt
```

## Reverting the Patch

If the patched files have not been committed, restore the original Momentum
versions with:

```bash
git restore targets/f7/ble_glue/gap.c
git restore targets/f7/furi_hal/furi_hal_bt.c
```

## Compatibility

The patch was developed against Momentum Firmware `mntm-012`.

Because the Momentum Bluetooth and GAP implementation may change between
firmware releases, always run:

```bash
git apply --check firmware/patches/momentum-mntm-012-pc-monitor.patch
```

before applying the patch to another Momentum revision.

A failure from `git apply --check` on a newer release does not necessarily mean
that PC Monitor is incompatible. It means that the patch needs to be reviewed
against the newer implementation before being applied.

## PC Monitor Components

The complete project consists of three main userspace components plus this
firmware modification:

```text
flipper-pc-monitor-suite/
├── flipper-fap/       PC Monitor application for Flipper Zero
├── mac-backend/       macOS telemetry and BLE/RPC backend
├── flipperble/        macOS BLE RPC command-line client
└── firmware/
    ├── README.md
    └── patches/
        └── momentum-mntm-012-pc-monitor.patch
```

The firmware patch provides the BLE advertising behavior used by the higher
level PC Monitor components.
