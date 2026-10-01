# Marauder RPC UART Bridge

A lightweight Flipper Zero FAP that bridges the Flipper RPC
`App.DataExchange` interface to a UART-connected ESP32 running Marauder.

It is designed to work with the `flipperble` client in this repository
and allows a host to control an attached Marauder board through the
Flipper's BLE or USB RPC connection.

## Architecture

``` text
Mac / Host
   |
   | flipperble
   | BLE RPC or USB RPC
   v
Flipper Zero
   |
   | marauder_rpc_bridge.fap
   | App.DataExchange <-> UART
   v
ESP32 / Apex 5
   |
   | Marauder CLI
   v
Wi-Fi / BLE operations
```

The Flipper acts as a transport bridge. Marauder itself continues to run
on the attached ESP32 board.

## Components

``` text
marauder-rpc-bridge/
├── application.fam
├── marauder_rpc_bridge.c
└── README.md
```

The host-side implementation is in `flipperble/flipperble.py`.

## Requirements

-   Flipper Zero
-   Momentum firmware compatible with the RPC APIs used by this FAP
-   ESP32 Marauder-compatible board connected to the Flipper UART
-   This repository's `flipperble` client
-   BLE RPC or USB RPC connectivity between the host and Flipper

The development setup uses an Apex 5 ESP32-C5 running a Marauder build
that exposes its CLI over UART.

## How It Works

`flipperble` starts the bridge through the Flipper RPC App System:

``` python
req.name = "/ext/apps/GPIO/marauder_rpc_bridge.fap"
req.args = "RPC"
```

Commands sent by the host arrive as `App.DataExchange` packets. The
bridge forwards their data to the ESP32 over UART.

UART data received from Marauder is returned to the host with:

``` c
rpc_system_app_exchange_data(
    bridge->rpc,
    buffer,
    count
);
```

The complete data path is:

``` text
Host command
    |
    v
Flipper RPC App.DataExchange
    |
    v
Marauder RPC Bridge
    |
    v
UART TX
    |
    v
ESP32 Marauder

ESP32 Marauder
    |
    v
UART RX
    |
    v
Marauder RPC Bridge
    |
    v
rpc_system_app_exchange_data()
    |
    v
Host / flipperble
```

The bridge transports arbitrary byte data and does not need to
understand individual Marauder commands.

## Building

Place the source directory in the Momentum firmware tree:

``` text
applications_user/marauder_rpc_bridge/
```

For example:

``` text
Momentum-Firmware/
└── applications_user/
    └── marauder_rpc_bridge/
        ├── application.fam
        └── marauder_rpc_bridge.c
```

Build it using the normal Momentum/FBT workflow for user applications.

The resulting FAP must be available on the Flipper at:

``` text
/ext/apps/GPIO/marauder_rpc_bridge.fap
```

This is the path currently used by `flipperble`.

## Host Commands

### Send a Marauder command

``` bash
flipperble --device ZER0TYEC marauder "help"
```

The client connects to Flipper RPC, starts the bridge in RPC mode, sends
the Marauder command through `App.DataExchange`, and receives UART
output through the same RPC session.

Replace `ZER0TYEC` with the Bluetooth name of your own Flipper.

### Interactive Marauder terminal

``` bash
flipperble --device ZER0TYEC marauder-terminal
```

This opens a persistent interactive path:

``` text
terminal <-> flipperble <-> Flipper RPC <-> Marauder RPC Bridge <-> UART <-> ESP32 Marauder
```

Type `exit` or `quit` to close the terminal.

### USB RPC

The same implementation can use Flipper USB CDC RPC:

``` bash
flipperble --usb /dev/cu.usbmodemflip_ZER0TYEC1 marauder "help"
```

Replace the serial device with the USB CDC device presented by your
Flipper.

## Find My BLE Monitor

`flipperble` also provides:

``` bash
flipperble --device ZER0TYEC marauder-findmy
```

This is a passive BLE monitoring mode.

The command sends:

``` text
sniffbt
```

through the bridge to Marauder. The ESP32 performs the BLE scan and
reports advertisements over UART.

Example raw Marauder output:

``` text
BLEADV mac=de:43:f3:5e:0a:2b rssi=-64 len=31 data=1EFF4C001219...
```

`flipperble` reconstructs complete UART lines and extracts the
advertiser MAC address, RSSI, advertisement length, and raw
advertisement data.

It then filters for Apple Offline Finding / Find My manufacturer
advertisements containing:

``` text
FF4C001219
```

Example displayed output:

``` text
15:43:56.239  DE:43:F3:5E:0A:2B   -64 dBm  len=31  4C00121950CB60...
```

The monitor does not read the Flipper FindMy application state and does
not require a particular stored Find My payload. It passively displays
matching advertisements observed by the ESP32 scanner.

Press `Ctrl+C` to stop the monitor. `flipperble` then attempts to send:

``` text
stopscan
```

to Marauder.

## RPC Data Handling

Marauder UART output is asynchronous and does not necessarily align with
RPC packet boundaries.

`flipperble` therefore handles unsolicited `app_data_exchange_request`
messages before normal command-ID response filtering and places their
data into an asynchronous queue.

This supports normal request/response Marauder commands, continuous
terminal output, and continuous BLE scan output.

For BLE monitoring, RPC chunks are accumulated until complete UART lines
can be parsed.

## Relationship to PC Monitor

The Marauder bridge is separate from the PC Monitor FAP:

``` text
flipper-fap/
    PC Monitor application

marauder-rpc-bridge/
    RPC <-> UART transport for Marauder
```

Both use Flipper RPC functionality, but they serve different purposes.

The macOS backend in this repository also contains shared RPC
infrastructure used by host-side components.

## Repository Layout

``` text
flipper-pc-monitor-suite/
├── flipper-fap/
│   └── PC Monitor FAP
├── flipperble/
│   └── flipperble.py
├── mac-backend/
│   └── macOS backend and shared RPC infrastructure
└── marauder-rpc-bridge/
    ├── application.fam
    ├── marauder_rpc_bridge.c
    └── README.md
```

## Troubleshooting

### Marauder bridge start RPC error

Check that `marauder_rpc_bridge.fap` exists under `/ext/apps/GPIO/`, the
Flipper RPC connection is active, and no incompatible application
currently owns the required RPC App System session.

### No UART response received

Check that the ESP32/Marauder board is powered, UART wiring is correct,
UART parameters match the bridge implementation, and Marauder is running
and accepting CLI commands.

### BLE works but Marauder commands do not

Verify basic RPC communication first:

``` bash
flipperble --device ZER0TYEC info
```

Then test the bridge:

``` bash
flipperble --device ZER0TYEC marauder "help"
```

### USB RPC

Find the Flipper CDC device:

``` bash
ls /dev/cu.usbmodem*
```

Then pass the discovered device to `--usb`.

## Security and Privacy

The bridge provides access to the UART-connected Marauder CLI through
the Flipper RPC transport. Access should be limited to trusted host
devices and hardware you control.

`marauder-findmy` is intended as a passive diagnostic and development
tool for inspecting BLE advertisements. Use wireless monitoring
functionality only where you are authorized to do so.

## License

See the repository license and the licenses of the upstream Momentum and
Marauder projects for the terms applicable to their respective
components.
