#!/usr/bin/env python3

import argparse
import asyncio
import struct
import sys
from pathlib import Path

from bleak import BleakScanner, BleakClient

import flipper_pb2


DEVICE_NAME = "TYECzer0"

TX_UUID = "19ed82ae-ed21-4c9d-4145-228e62fe0000"
RX_UUID = "19ed82ae-ed21-4c9d-4145-228e61fe0000"
FLOW_UUID = "19ed82ae-ed21-4c9d-4145-228e63fe0000"

BATTERY_UUID = "00002a19-0000-1000-8000-00805f9b34fb"

BACKEND_SOCKET = "/tmp/flipper-pcmonitor-rpc.sock"
BACKEND_ACTIVE_MARKER = "/tmp/flipper-pcmonitor-rpc-active"

PROXY_MAX_PAYLOAD = 4 * 1024 * 1024
MAX_CLIENT_COMMAND_ID = 0x7FFFFFFF


def encode_varint(value):
    out = bytearray()

    while True:
        b = value & 0x7F
        value >>= 7

        if value:
            out.append(b | 0x80)
        else:
            out.append(b)
            break

    return bytes(out)


def decode_varint(buf):
    result = 0
    shift = 0

    for i, b in enumerate(buf):
        result |= (b & 0x7F) << shift

        if not (b & 0x80):
            return result, i + 1

        shift += 7

    return None, 0


def next_command_id(value):
    value += 1
    if value <= 0 or value > MAX_CLIENT_COMMAND_ID:
        return 1
    return value


class FlipperBLE:

    def __init__(self, name):
        self.name = name

        self.device = None
        self.client = None

        self.rx_buffer = bytearray()

        self.frames = []
        self.finished = asyncio.Event()

        self.command_id = 1

        self.tx_credits = 0
        self.tx_credit_event = asyncio.Event()

        self.proxy_mode = False
        self.proxy_reader = None
        self.proxy_writer = None


    async def advertised_battery(
        self,
        timeout_seconds=5
    ):

        #
        # Custom Momentum firmware advertisement:
        #
        # Manufacturer ID: 0xFFFF
        #
        # CoreBluetooth/Bleak exposes the manufacturer ID
        # separately, so the value here is:
        #
        #   46 5A 01 XX FLAGS
        #   F  Z  ver battery
        #
        devices = await BleakScanner.discover(
            timeout=timeout_seconds,
            return_adv=True
        )

        for _, item in devices.items():

            device, advertisement = item

            name = (
                device.name
                or advertisement.local_name
                or ""
            )

            if name != self.name:
                continue

            data = (
                advertisement
                .manufacturer_data
                .get(0xFFFF)
            )

            if not data:
                continue

            if (
                len(data) >= 5
                and data[0:2] == b"FZ"
                and data[2] == 1
            ):
                level = int(data[3])

                if 0 <= level <= 100:
                    return level

        return None


    async def connect(self):

        #
        # Preferred mode:
        #
        # If PC Monitor backend is alive, reuse its
        # existing BLE RPC connection through the
        # local Unix socket.
        #
        try:
            if not Path(
                BACKEND_ACTIVE_MARKER
            ).exists():
                raise FileNotFoundError

            reader, writer = (
                await asyncio.wait_for(
                    asyncio.open_unix_connection(
                        BACKEND_SOCKET
                    ),
                    timeout=0.5
                )
            )

            self.proxy_reader = reader
            self.proxy_writer = writer
            self.proxy_mode = True

            return

        except (
            FileNotFoundError,
            ConnectionRefusedError,
            asyncio.TimeoutError,
            OSError,
        ):
            self.proxy_mode = False
            self.proxy_reader = None
            self.proxy_writer = None

        #
        # Fallback:
        # original direct CoreBluetooth/Bleak mode.
        #
        self.device = await BleakScanner.find_device_by_name(
            self.name,
            timeout=15
        )

        if not self.device:
            raise RuntimeError(
                f"Flipper '{self.name}' not found"
            )

        self.client = BleakClient(
            self.device,
            timeout=20
        )

        await self.client.connect()

        if not self.client.is_connected:
            raise RuntimeError(
                "Could not connect to Flipper"
            )


    async def disconnect(self):

        if self.proxy_writer is not None:

            self.proxy_writer.close()

            try:
                await self.proxy_writer.wait_closed()
            except Exception:
                pass

            self.proxy_reader = None
            self.proxy_writer = None
            self.proxy_mode = False

        if self.client and self.client.is_connected:
            await self.client.disconnect()


    async def start_rpc(self):

        if self.proxy_mode:
            return

        await self.client.start_notify(
            RX_UUID,
            self.notification_handler
        )

        await self.client.start_notify(
            FLOW_UUID,
            self.flow_control_handler
        )

        raw = await self.client.read_gatt_char(
            FLOW_UUID
        )

        self.tx_credits = self.decode_flow_credit(raw)

        if self.tx_credits > 0:
            self.tx_credit_event.set()


    def decode_flow_credit(self, data):

        if not data:
            return 0

        little = int.from_bytes(
            data,
            byteorder="little",
            signed=False
        )

        big = int.from_bytes(
            data,
            byteorder="big",
            signed=False
        )

        #
        # Normal Flipper values are usually a few
        # hundred bytes. Pick the plausible one.
        #
        if 0 < little <= 4096:
            return little

        if 0 < big <= 4096:
            return big

        return little


    def flow_control_handler(self, sender, data):

        credits = self.decode_flow_credit(data)

        if credits <= 0:
            return

        self.tx_credits = credits
        self.tx_credit_event.set()


    def notification_handler(self, sender, data):

        self.rx_buffer.extend(data)

        while True:

            length, prefix_len = decode_varint(
                self.rx_buffer
            )

            if length is None:
                return

            total = prefix_len + length

            if len(self.rx_buffer) < total:
                return

            protobuf_data = bytes(
                self.rx_buffer[
                    prefix_len:total
                ]
            )

            del self.rx_buffer[:total]

            msg = flipper_pb2.Main()

            try:
                msg.ParseFromString(
                    protobuf_data
                )
            except Exception as e:
                print(
                    f"RPC decode error: {e}",
                    file=sys.stderr
                )
                continue

            if msg.command_id != self.command_id:
                continue

            self.frames.append(msg)

            if not msg.has_next:
                self.finished.set()


    def _consume_proxy_response(
        self,
        raw,
        command_id
    ):

        offset = 0
        frames = []

        while offset < len(raw):

            length, prefix_len = decode_varint(
                raw[offset:]
            )

            if length is None:
                raise RuntimeError(
                    "Incomplete RPC proxy response"
                )

            start = offset + prefix_len
            end = start + length

            if end > len(raw):
                raise RuntimeError(
                    "Truncated RPC proxy response"
                )

            msg = flipper_pb2.Main()

            try:
                msg.ParseFromString(
                    raw[start:end]
                )
            except Exception as e:
                raise RuntimeError(
                    f"RPC proxy decode error: {e}"
                )

            offset = end

            if msg.command_id != command_id:
                continue

            frames.append(msg)

        return frames


    async def _proxy_exchange(
        self,
        packet,
        timeout=30
    ):

        if (
            not self.proxy_mode
            or self.proxy_reader is None
            or self.proxy_writer is None
        ):
            raise RuntimeError(
                "RPC backend proxy is not connected"
            )

        self.proxy_writer.write(
            len(packet).to_bytes(
                4,
                byteorder="little",
                signed=False
            )
        )

        self.proxy_writer.write(packet)

        await self.proxy_writer.drain()

        try:
            status_raw = await asyncio.wait_for(
                self.proxy_reader.readexactly(1),
                timeout=timeout
            )

            length_raw = await asyncio.wait_for(
                self.proxy_reader.readexactly(4),
                timeout=timeout
            )

            length = int.from_bytes(
                length_raw,
                byteorder="little",
                signed=False
            )

            if length > PROXY_MAX_PAYLOAD:
                raise RuntimeError(
                    f"RPC backend proxy response too large: {length} bytes"
                )

            payload = await asyncio.wait_for(
                self.proxy_reader.readexactly(
                    length
                ),
                timeout=timeout
            )

        except asyncio.IncompleteReadError:
            raise RuntimeError(
                "RPC backend proxy disconnected"
            )

        if status_raw[0] != 0:
            raise RuntimeError(
                payload.decode(
                    errors="replace"
                )
                or "RPC backend proxy error"
            )

        return payload


    async def request(self, msg, timeout=15):

        self.frames = []
        self.rx_buffer = bytearray()
        self.finished = asyncio.Event()

        msg.command_id = self.command_id

        payload = msg.SerializeToString()

        packet = (
            encode_varint(len(payload))
            + payload
        )

        if self.proxy_mode:

            raw = await self._proxy_exchange(
                packet,
                timeout=timeout
            )

            result = (
                self._consume_proxy_response(
                    raw,
                    self.command_id
                )
            )

        else:

            await self.client.write_gatt_char(
                TX_UUID,
                packet,
                response=False
            )

            try:
                await asyncio.wait_for(
                    self.finished.wait(),
                    timeout=timeout
                )

            except asyncio.TimeoutError:
                raise RuntimeError(
                    "Timeout waiting for Flipper RPC response"
                )

            result = self.frames

        self.command_id = next_command_id(self.command_id)

        return result



    async def send_packet(self, packet):

        if self.proxy_mode:

            raw = await self._proxy_exchange(
                packet,
                timeout=30
            )

            if raw:

                frames = (
                    self._consume_proxy_response(
                        raw,
                        self.command_id
                    )
                )

                self.frames.extend(frames)

                if any(
                    not frame.has_next
                    for frame in frames
                ):
                    self.finished.set()

            return

        offset = 0

        while offset < len(packet):

            while self.tx_credits <= 0:
                self.tx_credit_event.clear()

                await asyncio.wait_for(
                    self.tx_credit_event.wait(),
                    timeout=10
                )

            chunk_size = min(
                len(packet) - offset,
                243,
                self.tx_credits
            )

            chunk = packet[
                offset:offset + chunk_size
            ]

            await self.client.write_gatt_char(
                TX_UUID,
                chunk,
                response=False
            )

            self.tx_credits -= chunk_size
            offset += chunk_size

            if self.tx_credits <= 0:
                self.tx_credit_event.clear()


    async def put(self, local_path, remote_path):

        local_path = Path(local_path).expanduser()

        if not local_path.is_file():
            raise RuntimeError(
                f"Local file not found: {local_path}"
            )

        data = local_path.read_bytes()

        #
        # Firmware storage RPC supports up to 512 B
        # of file data per StorageWriteRequest.
        #
        # Keep the request comfortably below a typical BLE
        # write payload once protobuf/path overhead is included.
        # This intentionally favors reliability over peak throughput.
        #
        storage_chunk = 480

        self.frames = []
        self.rx_buffer = bytearray()
        self.finished = asyncio.Event()

        command_id = self.command_id

        total = len(data)

        if total == 0:
            chunks = [b""]
        else:
            chunks = [
                data[i:i + storage_chunk]
                for i in range(0, total, storage_chunk)
            ]

        for index, chunk in enumerate(chunks):

            msg = flipper_pb2.Main()

            msg.command_id = command_id

            msg.has_next = (
                index < len(chunks) - 1
            )

            req = msg.storage_write_request

            req.path = remote_path

            req.file.data = chunk

            payload = msg.SerializeToString()

            packet = (
                encode_varint(len(payload))
                + payload
            )

            await self.send_packet(packet)

            progress = (
                min(
                    total,
                    (index + 1) * storage_chunk
                )
            )

            print(
                f"\rUploading: "
                f"{progress}/{total} bytes "
                f"({(progress / max(total, 1)) * 100:5.1f}%)",
                end="",
                flush=True
            )

        print()

        try:
            await asyncio.wait_for(
                self.finished.wait(),
                timeout=30
            )

        except asyncio.TimeoutError:
            raise RuntimeError(
                "Timeout waiting for storage write response"
            )

        for frame in self.frames:

            if frame.command_status != 0:
                raise RuntimeError(
                    "Storage write RPC error "
                    f"{frame.command_status}"
                )

        self.command_id = next_command_id(self.command_id)

        print(
            f"Uploaded {total} bytes -> {remote_path}"
        )



    async def run(self, remote_path):

        msg = flipper_pb2.Main()

        req = msg.app_start_request

        #
        # External FAP files are launched by Loader using
        # the app name "Applications" with the FAP path
        # supplied as args.
        #
        req.name = remote_path
        req.args = ""

        frames = await self.request(
            msg,
            timeout=15
        )

        for frame in frames:
            if frame.command_status != 0:
                raise RuntimeError(
                    "App start RPC error "
                    f"{frame.command_status}"
                )

        print(f"Started: {remote_path}")


    async def info(self):

        msg = flipper_pb2.Main()

        msg.system_device_info_request.SetInParent()

        frames = await self.request(msg)

        result = {}

        for frame in frames:

            if frame.command_status != 0:
                raise RuntimeError(
                    f"RPC error: {frame.command_status}"
                )

            if frame.HasField(
                "system_device_info_response"
            ):

                info = (
                    frame
                    .system_device_info_response
                )

                result[info.key] = info.value

        return result


    async def battery(self):

        if not self.proxy_mode:

            value = await self.client.read_gatt_char(
                BATTERY_UUID
            )

            if not value:
                raise RuntimeError(
                    "Battery level unavailable"
                )

            return value[0]

        #
        # Backend owns BLE, so do not create a second
        # CoreBluetooth connection merely for 2A19.
        # Ask Flipper RPC for power information instead.
        #
        msg = flipper_pb2.Main()
        msg.system_power_info_request.SetInParent()

        frames = await self.request(
            msg,
            timeout=15
        )

        values = {}

        for frame in frames:

            if frame.command_status != 0:
                raise RuntimeError(
                    "Power info RPC error "
                    f"{frame.command_status}"
                )

            if frame.HasField(
                "system_power_info_response"
            ):
                item = (
                    frame
                    .system_power_info_response
                )

                values[
                    item.key.lower()
                ] = item.value

        #
        # Firmware versions use slightly different
        # power-info key naming. Prefer anything that
        # clearly represents charge/percentage.
        #
        preferred = []

        for key, value in values.items():

            k = key.lower()

            if (
                k == "charge_level"
                or k == "battery_level"
                or k == "battery_percent"
                or (
                    "battery" in k
                    and (
                        "charge" in k
                        or "level" in k
                        or "percent" in k
                    )
                )
            ):
                preferred.append(
                    (key, value)
                )

        for key, value in preferred:

            digits = "".join(
                c for c in value
                if c.isdigit()
            )

            if digits:
                level = int(digits)

                if 0 <= level <= 100:
                    return level

        raise RuntimeError(
            "Battery percentage not found in "
            "RPC power info. Keys: "
            + ", ".join(
                sorted(values.keys())
            )
        )



    async def pcmon_state(self):

        msg = flipper_pb2.Main()
        msg.app_lock_status_request.SetInParent()

        frames = await self.request(
            msg,
            timeout=10
        )

        for frame in frames:
            print(frame)

        return frames


    async def pcmon_send(
        self,
        cpu_usage=37,
        ram_max=160,
        ram_usage=52,
        gpu_usage=23,
        battery_usage=88,
        cpu_temp=61,
        gpu_temp=49,
        ssd_temp=42,
        battery_temp=35,
    ):

        # Must match packed DataStruct in pc_monitor.h:
        #
        # uint8_t  cpu_usage
        # uint16_t ram_max
        # uint8_t  ram_usage
        # char     ram_unit[4]
        # uint8_t  gpu_usage
        # uint8_t  battery_usage
        # uint8_t  cpu_temp
        # uint8_t  gpu_temp
        # uint8_t  ssd_temp
        # uint8_t  battery_temp

        data = struct.pack(
            "<BHB4sBBBBBB",
            cpu_usage,
            ram_max,
            ram_usage,
            b"GB\x00\x00",
            gpu_usage,
            battery_usage,
            cpu_temp,
            gpu_temp,
            ssd_temp,
            battery_temp,
        )

        if len(data) != 14:
            raise RuntimeError(
                f"Unexpected telemetry packet size: {len(data)}"
            )

        msg = flipper_pb2.Main()

        req = msg.app_data_exchange_request
        req.data = data

        frames = await self.request(
            msg,
            timeout=10
        )

        for frame in frames:
            if frame.command_status != 0:
                raise RuntimeError(
                    "PC Monitor data RPC error "
                    f"{frame.command_status}"
                )

        print(
            "Telemetry sent: "
            f"CPU={cpu_usage}% "
            f"RAM={ram_usage}% "
            f"GPU={gpu_usage}% "
            f"BAT={battery_usage}%"
        )

        return frames


    async def pcmon_start(self):

        msg = flipper_pb2.Main()

        req = msg.app_start_request

        req.name = "/ext/apps/Bluetooth/pc_monitor.fap"
        req.args = "RPC"

        frames = await self.request(
            msg,
            timeout=15
        )

        for frame in frames:
            if frame.command_status != 0:
                raise RuntimeError(
                    "PC Monitor start RPC error "
                    f"{frame.command_status}"
                )

        print(
            "PC Monitor started in RPC mode"
        )

        # Give the FAP a moment to finish registering its
        # RPC DataExchange callback.
        await asyncio.sleep(1)

        print(
            "Starting RPC telemetry stream for 60 seconds..."
        )

        for second in range(60):

            await self.pcmon_send(
                cpu_usage=37,
                ram_max=160,
                ram_usage=52,
                gpu_usage=23,
                battery_usage=88,
                cpu_temp=61,
                gpu_temp=49,
                ssd_temp=42,
                battery_temp=35,
            )

            await asyncio.sleep(1)

        return frames


    async def rm(self, path, recursive=False):

        msg = flipper_pb2.Main()

        req = msg.storage_delete_request
        req.path = path
        req.recursive = recursive

        frames = await self.request(
            msg,
            timeout=30
        )

        for frame in frames:
            if frame.command_status != 0:
                raise RuntimeError(
                    "Storage delete RPC error "
                    f"{frame.command_status}"
                )

        print(f"Deleted: {path}")


    async def mkdir(self, path):

        msg = flipper_pb2.Main()

        req = msg.storage_mkdir_request
        req.path = path

        frames = await self.request(
            msg,
            timeout=30
        )

        for frame in frames:
            if frame.command_status != 0:
                raise RuntimeError(
                    "Storage mkdir RPC error "
                    f"{frame.command_status}"
                )

        print(f"Created directory: {path}")


    async def mv(self, old_path, new_path):

        msg = flipper_pb2.Main()

        req = msg.storage_rename_request
        req.old_path = old_path
        req.new_path = new_path

        frames = await self.request(
            msg,
            timeout=30
        )

        for frame in frames:
            if frame.command_status != 0:
                raise RuntimeError(
                    "Storage rename RPC error "
                    f"{frame.command_status}"
                )

        print(
            f"Renamed: {old_path} -> {new_path}"
        )


    async def ls(self, path):

        msg = flipper_pb2.Main()

        req = msg.storage_list_request

        req.path = path
        req.include_md5 = False

        frames = await self.request(
            msg,
            timeout=30
        )

        files = []

        for frame in frames:

            if frame.command_status != 0:

                raise RuntimeError(
                    "Storage RPC error "
                    f"{frame.command_status}"
                )

            if frame.HasField(
                "storage_list_response"
            ):

                response = (
                    frame.storage_list_response
                )

                for item in response.file:
                    files.append(item)

        return files


async def cmd_put(
    flipper,
    local_path,
    remote_path
):

    await flipper.put(
        local_path,
        remote_path
    )



async def cmd_run(flipper, remote_path):

    await flipper.run(remote_path)



async def cmd_rm(
    flipper,
    path,
    recursive=False
):

    await flipper.rm(
        path,
        recursive=recursive
    )


async def cmd_mkdir(flipper, path):

    await flipper.mkdir(path)


async def cmd_mv(
    flipper,
    old_path,
    new_path
):

    await flipper.mv(
        old_path,
        new_path
    )


async def cmd_info(flipper):

    info = await flipper.info()

    important = [
        "hardware_name",
        "hardware_model",
        "hardware_region_provisioned",
        "hardware_ver",
        "firmware_version",
        "firmware_build_date",
        "firmware_commit",
        "firmware_origin_fork",
        "firmware_api_major",
        "firmware_api_minor",
        "protobuf_version_major",
        "protobuf_version_minor",
        "radio_alive",
        "radio_ble_mac",
    ]

    print()

    for key in important:

        if key in info:
            print(
                f"{key:30} : {info[key]}"
            )

    print()


async def cmd_battery(flipper):

    battery = await flipper.battery()

    print(f"Battery: {battery}%")


async def cmd_ls(flipper, path):

    files = await flipper.ls(path)

    print()
    print(f"Directory: {path}")
    print()

    if not files:
        print("(empty)")
        return

    for item in files:

        if item.type == 1:
            type_name = "DIR "
            size = ""
        else:
            type_name = "FILE"
            size = f"{item.size:>10} B"

        print(
            f"{type_name:4} "
            f"{size:>12}  "
            f"{item.name}"
        )

    print()


async def main():

    parser = argparse.ArgumentParser(
        prog="flipperble",
        description=(
            "Flipper Zero BLE RPC client for macOS"
        )
    )

    parser.add_argument(
        "--device",
        default=DEVICE_NAME,
        help=(
            "Bluetooth name "
            f"(default: {DEVICE_NAME})"
        )
    )

    sub = parser.add_subparsers(
        dest="command",
        required=True
    )

    sub.add_parser(
        "info",
        help="Show Flipper device information"
    )

    sub.add_parser(
        "battery",
        help="Show GATT/RPC battery level"
    )

    sub.add_parser(
        "advbattery",
        help="Show battery level from BLE advertising"
    )

    sub.add_parser(
        "pcmon-start",
        help="Start PC Monitor using shared Flipper RPC session"
    )

    ls_parser = sub.add_parser(
        "ls",
        help="List files/directories"
    )

    ls_parser.add_argument(
        "path",
        nargs="?",
        default="/ext"
    )


    put_parser = sub.add_parser(
        "put",
        help="Upload file to Flipper storage"
    )

    put_parser.add_argument(
        "local_file"
    )

    put_parser.add_argument(
        "remote_file"
    )


    run_parser = sub.add_parser(
        "run",
        help="Start an application on Flipper"
    )

    run_parser.add_argument(
        "remote_file"
    )

    rm_parser = sub.add_parser(
        "rm",
        help="Delete file or directory"
    )

    rm_parser.add_argument(
        "-r",
        "--recursive",
        action="store_true",
        help="Recursively delete directory contents"
    )

    rm_parser.add_argument(
        "path"
    )


    mkdir_parser = sub.add_parser(
        "mkdir",
        help="Create directory"
    )

    mkdir_parser.add_argument(
        "path"
    )


    mv_parser = sub.add_parser(
        "mv",
        help="Rename or move file/directory"
    )

    mv_parser.add_argument(
        "old_path"
    )

    mv_parser.add_argument(
        "new_path"
    )


    args = parser.parse_args()

    flipper = FlipperBLE(args.device)

    try:

        if args.command == "battery":

            await flipper.connect()

            rpc_level = await flipper.battery()

            print(
                f"GATT/RPC battery:    "
                f"{rpc_level}%"
            )

        elif args.command == "advbattery":

            adv_level = await flipper.advertised_battery()

            if adv_level is not None:
                print(
                    f"Advertising battery: "
                    f"{adv_level}%"
                )
            else:
                print(
                    "Advertising battery: unavailable"
                )

        else:

            await flipper.connect()
            await flipper.start_rpc()

            if args.command == "info":
                await cmd_info(flipper)

            elif args.command == "ls":
                await cmd_ls(
                    flipper,
                    args.path
                )

            elif args.command == "pcmon-start":
                await flipper.pcmon_start()
                print("PC Monitor RPC mode started")


            elif args.command == "put":
                await cmd_put(
                    flipper,
                    args.local_file,
                    args.remote_file
                )


            elif args.command == "rm":
                await cmd_rm(
                    flipper,
                    args.path,
                    recursive=args.recursive
                )


            elif args.command == "mkdir":
                await cmd_mkdir(
                    flipper,
                    args.path
                )


            elif args.command == "mv":
                await cmd_mv(
                    flipper,
                    args.old_path,
                    args.new_path
                )


            elif args.command == "run":
                await cmd_run(
                    flipper,
                    args.remote_file
                )

    finally:

        await flipper.disconnect()


if __name__ == "__main__":

    try:
        asyncio.run(main())

    except KeyboardInterrupt:
        pass

    except Exception as e:

        print(
            f"ERROR: {e}",
            file=sys.stderr
        )

        sys.exit(1)
