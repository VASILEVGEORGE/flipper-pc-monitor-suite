use btleplug::api::{
    Central, CentralEvent, Peripheral as _, ScanFilter, ValueNotification, WriteType,
};
use btleplug::platform::{Manager, Peripheral, PeripheralId};
use futures::{stream::Stream, StreamExt};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time::{sleep, timeout, Duration};

mod flipper_manager;
mod helpers;
mod system_info;

static BLE_CONNECTED_EVENT: AtomicBool = AtomicBool::new(false);

/*
 * After PC Monitor intentionally closes, CoreBluetooth may briefly
 * replay an older advertisement containing flags=0x01.
 *
 * Do not reconnect until we have observed a fresh flags=0x00
 * advertisement after the disconnect.
 */
static WAIT_FOR_IDLE_ADVERTISEMENT: AtomicBool = AtomicBool::new(false);
static CONNECT_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/*
 * Absolute UNIX timestamp in milliseconds.
 * While WAIT_FOR_IDLE_ADVERTISEMENT is true, the guard
 * automatically expires after this deadline.
 */
static IDLE_ADVERTISEMENT_DEADLINE_MS: AtomicU64 = AtomicU64::new(0);

/*
 * Prevent repeated "Ignoring stale..." messages for every
 * CoreBluetooth DeviceUpdated event.
 */
static STALE_ADVERTISEMENT_LOGGED: AtomicBool = AtomicBool::new(false);

/*
 * True when the user explicitly closes PC Monitor
 * from the Flipper UI.
 *
 * While set, reconnecting BLE must NOT auto-launch
 * the FAP again.
 */
/*
 * --------------------------------------------------------------------------
 * Local RPC proxy
 *
 * flipperble -> Unix socket -> this backend -> existing BLE RPC session
 *
 * Only this backend owns the BLE connection.
 * --------------------------------------------------------------------------
 */

const RPC_SOCKET_PATH: &str = "/tmp/flipper-pcmonitor-rpc.sock";


const RPC_EVENT_SOCKET_PATH: &str = "/tmp/flipper-pcmonitor-events.sock";

/*
 * Every decoded inbound PB.Main frame is copied onto this broadcast
 * channel. Normal RPC handling continues unchanged.
 *
 * This gives long-lived tools such as marauder-findmy a passive RX
 * path without letting them own the BLE notification stream.
 */
static RPC_EVENT_TX: OnceLock<broadcast::Sender<Vec<u8>>> = OnceLock::new();

fn publish_rpc_event(frame: &[u8]) {
    if let Some(tx) = RPC_EVENT_TX.get() {
        let _ = tx.send(frame.to_vec());
    }
}


const RPC_ACTIVE_MARKER: &str = "/tmp/flipper-pcmonitor-rpc-active";

const RPC_SOCKET_MAX_PACKET: usize = 64 * 1024;

/*
 * Shared BLE connect timeout.
 *
 * Previously the main event handler used 8s while the sleep/wake
 * watchdog used only 2s. A 2s ceiling is too tight for a real
 * CoreBluetooth connect and produced spurious watchdog failures,
 * which in turn escalated to a full process restart. One value is
 * used everywhere so both paths behave identically.
 */
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/*
 * Exit code used for the launchd "let it crash" safety net.
 *
 * Reserved for genuinely wedged CoreBluetooth state that cannot be
 * recovered in-process (event stream ended, repeated watchdog
 * failures). Ordinary BLE/RPC errors are now recovered in-process
 * by tearing the session down and letting scanning resume.
 */
#[allow(dead_code)]
const EXIT_RESTART: i32 = 75;

/*
 * Monotonic millisecond clock.
 *
 * The idle-advertisement guard must not use wall-clock time: on
 * macOS sleep/wake the wall clock can jump, which could make the
 * guard expire instantly or never. A process-start Instant gives a
 * steady reference that is immune to clock changes.
 */
fn mono_now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

struct ProxyRequest {
    packet: Vec<u8>,
    reply: oneshot::Sender<Result<Vec<u8>, String>>,
}

type ProxyReceiver = Arc<Mutex<mpsc::Receiver<ProxyRequest>>>;

async fn handle_proxy_client(mut stream: UnixStream, proxy_tx: mpsc::Sender<ProxyRequest>) {
    loop {
        /*
         * Request wire format:
         *
         *   u32 little-endian packet length
         *   packet bytes
         *
         * Response:
         *
         *   u8  status
         *       0 = OK
         *       1 = error
         *
         *   u32 little-endian payload length
         *   payload
         *
         * Successful payload contains the raw,
         * length-delimited Flipper RPC response
         * frame(s).
         */
        let mut len_buf = [0u8; 4];

        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}

            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                return;
            }

            Err(_) => {
                return;
            }
        }

        let packet_len = u32::from_le_bytes(len_buf) as usize;

        if packet_len == 0 || packet_len > RPC_SOCKET_MAX_PACKET {
            let message = b"invalid RPC packet length";

            let _ = stream.write_all(&[1u8]).await;

            let _ = stream
                .write_all(&(message.len() as u32).to_le_bytes())
                .await;

            let _ = stream.write_all(message).await;

            return;
        }

        let mut packet = vec![0u8; packet_len];

        if stream.read_exact(&mut packet).await.is_err() {
            return;
        }

        let (reply_tx, reply_rx) = oneshot::channel();

        if proxy_tx
            .send(ProxyRequest {
                packet,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return;
        }

        let result = match reply_rx.await {
            Ok(result) => result,

            Err(_) => Err("RPC broker unavailable".to_string()),
        };

        match result {
            Ok(payload) => {
                if stream.write_all(&[0u8]).await.is_err() {
                    return;
                }

                if stream
                    .write_all(&(payload.len() as u32).to_le_bytes())
                    .await
                    .is_err()
                {
                    return;
                }

                if stream.write_all(&payload).await.is_err() {
                    return;
                }
            }

            Err(message) => {
                let payload = message.as_bytes();

                if stream.write_all(&[1u8]).await.is_err() {
                    return;
                }

                if stream
                    .write_all(&(payload.len() as u32).to_le_bytes())
                    .await
                    .is_err()
                {
                    return;
                }

                if stream.write_all(payload).await.is_err() {
                    return;
                }
            }
        }
    }
}


async fn handle_event_client(
    mut stream: UnixStream,
    mut event_rx: broadcast::Receiver<Vec<u8>>,
) {
    loop {
        let frame = match event_rx.recv().await {
            Ok(frame) => frame,

            /*
             * A slow observer must never affect BLE/RPC.
             * Drop old monitoring frames and continue with fresh data.
             */
            Err(broadcast::error::RecvError::Lagged(_)) => {
                continue;
            }

            Err(broadcast::error::RecvError::Closed) => {
                return;
            }
        };

        /*
         * Event wire format:
         *
         *   u32 little-endian protobuf frame length
         *   raw serialized PB.Main bytes
         *
         * Unlike the normal RPC socket, this is one-way.
         */
        let len = frame.len() as u32;

        if stream.write_all(&len.to_le_bytes()).await.is_err() {
            return;
        }

        if stream.write_all(&frame).await.is_err() {
            return;
        }
    }
}

async fn rpc_event_socket_server() {
    let _ = std::fs::remove_file(RPC_EVENT_SOCKET_PATH);

    let listener = match UnixListener::bind(RPC_EVENT_SOCKET_PATH) {
        Ok(listener) => listener,

        Err(err) => {
            eprintln!("RPC event socket bind failed: {}", err);
            return;
        }
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Err(err) = std::fs::set_permissions(
            RPC_EVENT_SOCKET_PATH,
            std::fs::Permissions::from_mode(0o600),
        ) {
            eprintln!("RPC event socket permission warning: {}", err);
        }
    }

    println!("RPC event stream listening on {}", RPC_EVENT_SOCKET_PATH);

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let Some(tx) = RPC_EVENT_TX.get() else {
                    continue;
                };

                let rx = tx.subscribe();

                tokio::spawn(handle_event_client(stream, rx));
            }

            Err(err) => {
                eprintln!("RPC event socket accept failed: {}", err);
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn rpc_socket_server(proxy_tx: mpsc::Sender<ProxyRequest>) {
    let _ = std::fs::remove_file(RPC_SOCKET_PATH);

    let listener = match UnixListener::bind(RPC_SOCKET_PATH) {
        Ok(listener) => listener,

        Err(err) => {
            eprintln!("RPC proxy socket bind failed: {}", err);

            return;
        }
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if let Err(err) =
            std::fs::set_permissions(RPC_SOCKET_PATH, std::fs::Permissions::from_mode(0o600))
        {
            eprintln!("RPC proxy socket permission warning: {}", err);
        }
    }

    println!("RPC proxy listening on {}", RPC_SOCKET_PATH);

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let tx = proxy_tx.clone();

                tokio::spawn(handle_proxy_client(stream, tx));
            }

            Err(err) => {
                eprintln!("RPC proxy accept failed: {}", err);

                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/*
 * --------------------------------------------------------------------------
 * Minimal protobuf encoder
 * --------------------------------------------------------------------------
 */

fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;

        value >>= 7;

        if value != 0 {
            byte |= 0x80;
        }

        out.push(byte);

        if value == 0 {
            break;
        }
    }
}

fn encode_key(field: u32, wire_type: u8, out: &mut Vec<u8>) {
    encode_varint(((field as u64) << 3) | wire_type as u64, out);
}

fn encode_length_delimited(field: u32, data: &[u8], out: &mut Vec<u8>) {
    encode_key(field, 2, out);

    encode_varint(data.len() as u64, out);

    out.extend_from_slice(data);
}

/*
 * Main {
 *   uint32 command_id = 1;
 *   ...
 *   App.DataExchangeRequest app_data_exchange_request = 65;
 * }
 *
 * DataExchangeRequest {
 *   bytes data = 1;
 * }
 */
fn build_data_exchange_request(command_id: u32, telemetry: &[u8]) -> Vec<u8> {
    let mut request = Vec::new();

    encode_length_delimited(1, telemetry, &mut request);

    let mut main = Vec::new();

    encode_key(1, 0, &mut main);

    encode_varint(command_id as u64, &mut main);

    encode_length_delimited(65, &request, &mut main);

    let mut packet = Vec::new();

    encode_varint(main.len() as u64, &mut packet);

    packet.extend_from_slice(&main);

    packet
}

/*
 * --------------------------------------------------------------------------
 * Minimal protobuf decoder
 *
 * We only care about these Main fields:
 *
 *   command_id     = 1
 *   command_status = 2
 *   has_next       = 3
 * --------------------------------------------------------------------------
 */

fn decode_varint(data: &[u8], offset: &mut usize) -> Option<u64> {
    let mut result = 0u64;
    let mut shift = 0u32;

    while *offset < data.len() && shift < 64 {
        let byte = data[*offset];

        *offset += 1;

        result |= ((byte & 0x7f) as u64) << shift;

        if byte & 0x80 == 0 {
            return Some(result);
        }

        shift += 7;
    }

    None
}

fn skip_field(wire_type: u8, data: &[u8], offset: &mut usize) -> Option<()> {
    match wire_type {
        0 => {
            decode_varint(data, offset)?;

            Some(())
        }

        1 => {
            *offset = offset.checked_add(8)?;

            if *offset <= data.len() {
                Some(())
            } else {
                None
            }
        }

        2 => {
            let len = decode_varint(data, offset)? as usize;

            *offset = offset.checked_add(len)?;

            if *offset <= data.len() {
                Some(())
            } else {
                None
            }
        }

        5 => {
            *offset = offset.checked_add(4)?;

            if *offset <= data.len() {
                Some(())
            } else {
                None
            }
        }

        _ => None,
    }
}

fn parse_main_status(data: &[u8]) -> Option<(u32, u32, bool)> {
    let mut offset = 0usize;

    let mut command_id = 0u32;
    let mut command_status = 0u32;
    let mut has_next = false;

    while offset < data.len() {
        let key = decode_varint(data, &mut offset)?;

        let field = (key >> 3) as u32;

        let wire_type = (key & 0x07) as u8;

        match (field, wire_type) {
            (1, 0) => {
                command_id = decode_varint(data, &mut offset)? as u32;
            }

            (2, 0) => {
                command_status = decode_varint(data, &mut offset)? as u32;
            }

            (3, 0) => {
                has_next = decode_varint(data, &mut offset)? != 0;
            }

            _ => {
                skip_field(wire_type, data, &mut offset)?;
            }
        }
    }

    Some((command_id, command_status, has_next))
}

fn packet_command_id(packet: &[u8]) -> Option<u32> {
    /*
     * Incoming packet already has the Flipper
     * protobuf varint length prefix.
     */
    let mut outer_offset = 0usize;

    let frame_len = decode_varint(packet, &mut outer_offset)? as usize;

    let frame_end = outer_offset.checked_add(frame_len)?;

    if frame_end > packet.len() {
        return None;
    }

    let data = &packet[outer_offset..frame_end];

    let mut offset = 0usize;

    while offset < data.len() {
        let key = decode_varint(data, &mut offset)?;

        let field = (key >> 3) as u32;

        let wire = (key & 0x07) as u8;

        if field == 1 && wire == 0 {
            return Some(decode_varint(data, &mut offset)? as u32);
        }

        skip_field(wire, data, &mut offset)?;
    }

    None
}

fn take_delimited_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut offset = 0usize;

    let len = decode_varint(buffer, &mut offset)? as usize;

    let total = offset.checked_add(len)?;

    if buffer.len() < total {
        return None;
    }

    let frame = buffer[offset..total].to_vec();

    buffer.drain(0..total);

    Some(frame)
}

/*
 * --------------------------------------------------------------------------
 * Manual PC Monitor application state
 *
 * PB_Main field 58 = app_state_response
 *
 * application.proto:
 *   APP_CLOSED  = 0
 *   APP_STARTED = 1
 * --------------------------------------------------------------------------
 */

fn parse_app_state_response(data: &[u8]) -> Option<u32> {
    let mut offset = 0usize;

    while offset < data.len() {
        let key = decode_varint(data, &mut offset)?;

        let field = (key >> 3) as u32;

        let wire_type = (key & 0x07) as u8;

        if field == 58 && wire_type == 2 {
            let len = decode_varint(data, &mut offset)? as usize;

            let end = offset.checked_add(len)?;

            if end > data.len() {
                return None;
            }

            let nested = &data[offset..end];

            let mut nested_offset = 0usize;

            while nested_offset < nested.len() {
                let nested_key = decode_varint(nested, &mut nested_offset)?;

                let nested_field = (nested_key >> 3) as u32;

                let nested_wire = (nested_key & 0x07) as u8;

                /*
                 * AppStateResponse.state = field 1 enum
                 */
                if nested_field == 1 && nested_wire == 0 {
                    return Some(decode_varint(nested, &mut nested_offset)? as u32);
                }

                skip_field(nested_wire, nested, &mut nested_offset)?;
            }

            return None;
        }

        skip_field(wire_type, data, &mut offset)?;
    }

    None
}

async fn wait_for_pc_monitor<S>(
    notifications: &mut S,
    rx_buffer: &mut Vec<u8>,
    id: &PeripheralId,
    flipper: &Peripheral,
    tx_char: &btleplug::api::Characteristic,
    proxy_rx: &ProxyReceiver,
) -> Result<(), String>
where
    S: Stream<Item = ValueNotification> + Unpin,
{
    println!("[{}] PC Monitor closed - backend idle", id.to_string());

    loop {
        /*
         * Give flipperble access even while
         * PC Monitor FAP is closed.
         */
        if try_proxy_request(proxy_rx, flipper, tx_char, notifications, rx_buffer).await {
            continue;
        }

        let notification = match timeout(Duration::from_millis(100), notifications.next()).await {
            Ok(Some(notification)) => notification,

            Ok(None) => {
                return Err("BLE notification stream ended".to_string());
            }

            Err(_) => {
                continue;
            }
        };

        if notification.uuid != flipper_manager::FLIPPER_RX_UUID {
            continue;
        }

        rx_buffer.extend_from_slice(&notification.value);

        while let Some(frame) = take_delimited_frame(rx_buffer) {
                publish_rpc_event(&frame);
            if let Some(state) = parse_app_state_response(&frame) {
                /*
                 * APP_STARTED = 1
                 */
                if state == 1 {
                    println!("[{}] PC Monitor manually opened", id.to_string());

                    return Ok(());
                }

                /*
                 * APP_CLOSED = 0.
                 * Stay idle and keep waiting.
                 */
                if state == 0 {
                    println!("[{}] PC Monitor closed", id.to_string());
                }
            }
        }
    }
}

async fn wait_rpc_response<S>(
    notifications: &mut S,
    rx_buffer: &mut Vec<u8>,
    command_id: u32,
) -> Result<(), String>
where
    S: Stream<Item = ValueNotification> + Unpin,
{
    let result = timeout(Duration::from_secs(10), async {
        loop {
            let notification = notifications
                .next()
                .await
                .ok_or_else(|| "BLE notification stream ended".to_string())?;

            if notification.uuid != flipper_manager::FLIPPER_RX_UUID {
                continue;
            }

            rx_buffer.extend_from_slice(&notification.value);

            while let Some(frame) = take_delimited_frame(rx_buffer) {
                publish_rpc_event(&frame);
                if let Some((response_id, status, has_next)) = parse_main_status(&frame) {
                    if response_id != command_id {
                        continue;
                    }

                    if status != 0 {
                        return Err(format!(
                            "RPC command {} failed with status {}",
                            command_id, status
                        ));
                    }

                    if !has_next {
                        return Ok(());
                    }
                }
            }
        }
    })
    .await;

    match result {
        Ok(inner) => inner,

        Err(_) => Err(format!("RPC command {} timed out", command_id)),
    }
}

async fn wait_proxy_response<S>(
    notifications: &mut S,
    rx_buffer: &mut Vec<u8>,
    command_id: u32,
) -> Result<Vec<u8>, String>
where
    S: Stream<Item = ValueNotification> + Unpin,
{
    let result = timeout(Duration::from_secs(15), async {
        let mut output = Vec::<u8>::new();

        loop {
            let notification = notifications
                .next()
                .await
                .ok_or_else(|| "BLE notification stream ended".to_string())?;

            if notification.uuid != flipper_manager::FLIPPER_RX_UUID {
                continue;
            }

            rx_buffer.extend_from_slice(&notification.value);

            while let Some(frame) = take_delimited_frame(rx_buffer) {
                publish_rpc_event(&frame);
                if let Some((response_id, _status, has_next)) = parse_main_status(&frame) {
                    if response_id != command_id {
                        continue;
                    }

                    /*
                     * Restore protobuf-delimited
                     * framing before returning the
                     * response to flipperble.
                     */
                    encode_varint(frame.len() as u64, &mut output);

                    output.extend_from_slice(&frame);

                    if !has_next {
                        return Ok(output);
                    }
                }
            }
        }
    })
    .await;

    match result {
        Ok(inner) => inner,

        Err(_) => Err(format!("RPC proxy command {} timed out", command_id)),
    }
}

fn packet_has_next(packet: &[u8]) -> Option<bool> {
    /*
     * packet = varint(length) + serialized PB.Main
     */
    let mut outer_offset = 0usize;

    let message_len = decode_varint(packet, &mut outer_offset)? as usize;

    let message_end = outer_offset.checked_add(message_len)?;

    if message_end > packet.len() {
        return None;
    }

    let data = &packet[outer_offset..message_end];

    let mut offset = 0usize;

    while offset < data.len() {
        let key = decode_varint(data, &mut offset)?;

        let field = (key >> 3) as u32;

        let wire_type = (key & 0x07) as u8;

        /*
         * PB.Main.has_next = field 3, varint.
         */
        if field == 3 && wire_type == 0 {
            return Some(decode_varint(data, &mut offset)? != 0);
        }

        skip_field(wire_type, data, &mut offset)?;
    }

    /*
     * proto3 bool default = false.
     */
    Some(false)
}

/*
 * Proxy RPC packets may be larger than one BLE write.
 *
 * The Flipper BLE serial transport is a byte stream, so protobuf
 * framing may safely span several GATT WriteWithoutResponse writes.
 *
 * 240 bytes stays below the normal 243-byte payload we already use
 * in the native flipperble BLE path.
 */
const PROXY_BLE_WRITE_CHUNK: usize = 240;

async fn proxy_ble_write_packet(
    flipper: &Peripheral,
    tx_char: &btleplug::api::Characteristic,
    packet: &[u8],
) -> Result<(), String> {
    for chunk in packet.chunks(PROXY_BLE_WRITE_CHUNK) {
        timeout(
            Duration::from_secs(5),
            flipper.write(tx_char, chunk, WriteType::WithoutResponse),
        )
        .await
        .map_err(|_| "BLE RPC proxy write timed out".to_string())?
        .map_err(|err| format!("BLE RPC proxy write failed: {}", err))?;
    }

    Ok(())
}

async fn run_proxy_request<S>(
    flipper: &Peripheral,
    tx_char: &btleplug::api::Characteristic,
    notifications: &mut S,
    rx_buffer: &mut Vec<u8>,
    request: ProxyRequest,
) where
    S: Stream<Item = ValueNotification> + Unpin,
{
    let command_id = match packet_command_id(&request.packet) {
        Some(id) if id != 0 => id,

        _ => {
            let _ = request
                .reply
                .send(Err("could not parse RPC command_id".to_string()));

            return;
        }
    };

    let has_next = packet_has_next(&request.packet).unwrap_or(false);

    let write_result = proxy_ble_write_packet(flipper, tx_char, &request.packet).await;

    let result = match write_result {
        /*
         * Continuous RPC command:
         *
         * There is no final Flipper response yet.
         * ACK locally so flipperble can submit
         * the next StorageWriteRequest.
         */
        Ok(()) if has_next => Ok(Vec::new()),

        /*
         * Single/final request:
         * wait for the real RPC response.
         */
        Ok(()) => wait_proxy_response(notifications, rx_buffer, command_id).await,

        Err(err) => Err(err),
    };

    let _ = request.reply.send(result);
}

async fn try_proxy_request<S>(
    proxy_rx: &ProxyReceiver,
    flipper: &Peripheral,
    tx_char: &btleplug::api::Characteristic,
    notifications: &mut S,
    rx_buffer: &mut Vec<u8>,
) -> bool
where
    S: Stream<Item = ValueNotification> + Unpin,
{
    let request = {
        let mut rx = proxy_rx.lock().await;

        rx.try_recv().ok()
    };

    if let Some(request) = request {
        run_proxy_request(flipper, tx_char, notifications, rx_buffer, request).await;

        true
    } else {
        false
    }
}

async fn rpc_request<S>(
    flipper: &Peripheral,
    tx_char: &btleplug::api::Characteristic,
    notifications: &mut S,
    rx_buffer: &mut Vec<u8>,
    command_id: u32,
    packet: Vec<u8>,
) -> Result<(), String>
where
    S: Stream<Item = ValueNotification> + Unpin,
{
    timeout(
        Duration::from_secs(5),
        flipper.write(tx_char, &packet, WriteType::WithoutResponse),
    )
    .await
    .map_err(|_| "BLE RPC write timed out".to_string())?
    .map_err(|err| format!("BLE RPC write failed: {}", err))?;

    wait_rpc_response(notifications, rx_buffer, command_id).await
}

/*
 * --------------------------------------------------------------------------
 * Exact PC Monitor telemetry wire packet
 *
 * Matches packed C DataStruct:
 *
 *   uint8_t  cpu_usage
 *   uint16_t ram_max
 *   uint8_t  ram_usage
 *   char     ram_unit[4]
 *   uint8_t  gpu_usage
 *   uint8_t  battery_usage
 *   uint8_t  cpu_temp
 *   uint8_t  gpu_temp
 *   uint8_t  ssd_temp
 *   uint8_t  battery_temp
 *
 * Total: 14 bytes.
 * --------------------------------------------------------------------------
 */

/*
 * --------------------------------------------------------------------------
 * AirBattery NearCast bridge
 *
 * AirBattery must not open its own BLE connection to the Flipper because
 * this backend is the sole BLE/RPC owner. Instead, publish the Mac battery
 * value already collected by SystemInfo into AirBattery's NearCast cache.
 * --------------------------------------------------------------------------
 */

/*
 * --------------------------------------------------------------------------
 * Flipper battery over existing RPC session
 *
 * PB.Main:
 *   command_id                  = 1
 *   system_power_info_request   = 44
 *   system_power_info_response  = 45
 *
 * PowerInfoResponse:
 *   key   = 1
 *   value = 2
 * --------------------------------------------------------------------------
 */

fn build_power_info_request(command_id: u32) -> Vec<u8> {
    let mut main = Vec::new();

    encode_key(1, 0, &mut main);
    encode_varint(command_id as u64, &mut main);

    /* Empty PB_System.PowerInfoRequest */
    encode_length_delimited(44, &[], &mut main);

    let mut packet = Vec::new();

    encode_varint(main.len() as u64, &mut packet);
    packet.extend_from_slice(&main);

    packet
}

fn parse_power_info_level(data: &[u8]) -> Option<u8> {
    let mut offset = 0usize;

    while offset < data.len() {
        let key = decode_varint(data, &mut offset)?;

        let field = (key >> 3) as u32;
        let wire_type = (key & 0x07) as u8;

        /*
         * PB.Main.system_power_info_response = 45
         */
        if field == 45 && wire_type == 2 {
            let len = decode_varint(data, &mut offset)? as usize;
            let end = offset.checked_add(len)?;

            if end > data.len() {
                return None;
            }

            let nested = &data[offset..end];
            let mut nested_offset = 0usize;

            let mut item_key: Option<String> = None;
            let mut item_value: Option<String> = None;

            while nested_offset < nested.len() {
                let nested_key = decode_varint(nested, &mut nested_offset)?;

                let nested_field = (nested_key >> 3) as u32;

                let nested_wire = (nested_key & 0x07) as u8;

                if nested_wire == 2 && (nested_field == 1 || nested_field == 2) {
                    let str_len = decode_varint(nested, &mut nested_offset)? as usize;

                    let str_end = nested_offset.checked_add(str_len)?;

                    if str_end > nested.len() {
                        return None;
                    }

                    let value =
                        String::from_utf8_lossy(&nested[nested_offset..str_end]).to_string();

                    if nested_field == 1 {
                        item_key = Some(value);
                    } else {
                        item_value = Some(value);
                    }

                    nested_offset = str_end;

                    continue;
                }

                skip_field(nested_wire, nested, &mut nested_offset)?;
            }

            if let (Some(key), Some(value)) = (item_key, item_value) {
                let k = key.to_lowercase();

                if k == "charge_level" || k == "battery_level" || k == "battery_percent" {
                    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();

                    if let Ok(level) = digits.parse::<u8>() {
                        if level <= 100 {
                            return Some(level);
                        }
                    }
                }
            }

            offset = end;
            continue;
        }

        skip_field(wire_type, data, &mut offset)?;
    }

    None
}

async fn wait_power_info_response<S>(
    notifications: &mut S,
    rx_buffer: &mut Vec<u8>,
    command_id: u32,
) -> Result<u8, String>
where
    S: Stream<Item = ValueNotification> + Unpin,
{
    let result = timeout(Duration::from_secs(8), async {
        let mut battery_level: Option<u8> = None;

        loop {
            let notification = notifications
                .next()
                .await
                .ok_or_else(|| "BLE notification stream ended".to_string())?;

            if notification.uuid != flipper_manager::FLIPPER_RX_UUID {
                continue;
            }

            rx_buffer.extend_from_slice(&notification.value);

            while let Some(frame) = take_delimited_frame(rx_buffer) {
                publish_rpc_event(&frame);
                if let Some((response_id, status, has_next)) = parse_main_status(&frame) {
                    if response_id != command_id {
                        continue;
                    }

                    if status != 0 {
                        return Err(format!(
                            "Power info RPC command {} failed with status {}",
                            command_id, status
                        ));
                    }

                    if let Some(level) = parse_power_info_level(&frame) {
                        battery_level = Some(level);
                    }

                    if !has_next {
                        return battery_level.ok_or_else(|| {
                            "charge_level not found in PowerInfo response".to_string()
                        });
                    }
                }
            }
        }
    })
    .await;

    match result {
        Ok(inner) => inner,

        Err(_) => Err(format!("Power info RPC command {} timed out", command_id)),
    }
}

async fn request_flipper_battery<S>(
    flipper: &Peripheral,
    tx_char: &btleplug::api::Characteristic,
    notifications: &mut S,
    rx_buffer: &mut Vec<u8>,
    command_id: u32,
) -> Result<u8, String>
where
    S: Stream<Item = ValueNotification> + Unpin,
{
    let packet = build_power_info_request(command_id);

    timeout(
        Duration::from_secs(5),
        flipper.write(tx_char, &packet, WriteType::WithoutResponse),
    )
    .await
    .map_err(|_| "Flipper battery RPC write timed out".to_string())?
    .map_err(|err| format!("Flipper battery RPC write failed: {}", err))?;

    wait_power_info_response(notifications, rx_buffer, command_id).await
}

fn update_airbattery(battery_level: u8) {
    /*
     * Publish Flipper battery data only to our own IPC file.
     *
     * A separate stable helper copies this into AirBattery's
     * NearCast container. The backend itself never accesses
     * another application's sandbox.
     */
    if battery_level > 100 {
        return;
    }

    let device_id = std::env::var("FLIPPER_DEVICE_ID")
        .unwrap_or_else(|_| "FlipperZero".to_string());

    let device_name = std::env::var("FLIPPER_DEVICE_NAME")
        .unwrap_or_else(|_| "Flipper Zero".to_string());

    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(value) => value.as_secs_f64(),
        Err(_) => return,
    };

    let json = format!(
        concat!(
            "[{{",
            "\"hasBattery\":true,",
            "\"deviceID\":\"{}\",",
            "\"deviceType\":\"general_bt\",",
            "\"deviceName\":\"{}\",",
            "\"deviceModel\":\"Flipper Zero\",",
            "\"batteryLevel\":{},",
            "\"isCharging\":0,",
            "\"isCharged\":false,",
            "\"isPaused\":false,",
            "\"acPowered\":false,",
            "\"isHidden\":false,",
            "\"lowPower\":false,",
            "\"parentName\":\"\",",
            "\"lastUpdate\":{},",
            "\"realUpdate\":{}",
            "}}]"
        ),
        device_id, device_name, battery_level, now, now
    );

    let tmp_path = "/tmp/flipper-airbattery.json";
    let tmp_write = "/tmp/flipper-airbattery.json.tmp";

    if fs::write(tmp_write, json.as_bytes()).is_ok() {
        let _ = fs::rename(tmp_write, tmp_path);
    }
}

/*
 * --------------------------------------------------------------------------
 * AirBattery NearCast bridge
 *
 * AirBattery must not open its own BLE connection to the Flipper because
 * this backend is the sole BLE/RPC owner. Instead, publish the Mac battery
 * value already collected by SystemInfo into AirBattery's NearCast cache.
 * --------------------------------------------------------------------------
 */
fn telemetry_packet(info: &system_info::SystemInfo) -> [u8; 14] {
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

    data
}

/*
 * --------------------------------------------------------------------------
 * Persistent RPC PC Monitor session
 * --------------------------------------------------------------------------
 */

/*
 * Graceful in-process session teardown.
 *
 * Replaces the old std::process::exit(75) calls inside the data
 * worker. Instead of killing the whole backend (and with it the
 * flipperble Unix-socket proxy) on any BLE/RPC hiccup, we drop just
 * this session and disconnect. The disconnect surfaces as a
 * DeviceDisconnected event in the main loop, which restarts scanning
 * and reconnects on the next advertising request flag.
 */
async fn teardown_session(flipper: &Peripheral, reason: &str) {
    let id = flipper.id();

    eprintln!("[{}] Session teardown: {}", id.to_string(), reason);

    system_info::set_telemetry_enabled(false);

    BLE_CONNECTED_EVENT.store(false, Ordering::Relaxed);

    let _ = fs::remove_file(RPC_ACTIVE_MARKER);

    let _ = timeout(Duration::from_secs(5), flipper.disconnect()).await;
}

async fn data_sender(flipper: Peripheral, proxy_rx: ProxyReceiver) {
    let id = flipper.id();

    let chars = flipper.characteristics();

    let tx_char = match chars
        .iter()
        .find(|c| c.uuid == flipper_manager::FLIPPER_TX_UUID)
        .cloned()
    {
        Some(c) => c,

        None => {
            teardown_session(&flipper, "RPC TX characteristic not found").await;
            return;
        }
    };

    let rx_char = match chars
        .iter()
        .find(|c| c.uuid == flipper_manager::FLIPPER_RX_UUID)
        .cloned()
    {
        Some(c) => c,

        None => {
            teardown_session(&flipper, "RPC RX characteristic not found").await;
            return;
        }
    };

    let flow_char = chars
        .iter()
        .find(|c| c.uuid == flipper_manager::FLIPPER_FLOW_UUID)
        .cloned();

    if let Err(err) = flipper.subscribe(&rx_char).await {
        teardown_session(&flipper, &format!("RX subscribe failed: {}", err)).await;
        return;
    }

    if let Some(flow_char) = flow_char.as_ref() {
        if let Err(err) = flipper.subscribe(flow_char).await {
            eprintln!("[{}] FLOW subscribe warning: {}", id.to_string(), err);
        }
    }

    let mut notifications = match flipper.notifications().await {
        Ok(stream) => stream,

        Err(err) => {
            teardown_session(
                &flipper,
                &format!("failed to open BLE notification stream: {}", err),
            )
            .await;
            return;
        }
    };

    let mut rx_buffer = Vec::<u8>::new();

    /*
     * Reserve the upper half of the command-ID
     * space for backend telemetry. flipperble
     * normally uses low IDs beginning at 1.
     */
    let mut command_id = 0x8000_0000u32;

    /*
     * Telemetry stays OFF until the FAP reports APP_STARTED.
     * We never stream DataExchange packets into a session that
     * has no PC Monitor instance attached yet.
     */
    system_info::set_telemetry_enabled(false);

    /*
     * Launch flow (request-flag driven):
     *
     * The Flipper advertises flags bit0 = "PC Monitor requests
     * RPC" (set either by the manual bootstrap instance or kept
     * ON after a transport loss). That flag is what brought us
     * into this connected session, so the backend now issues
     * App.Start below to spawn the RPC-bound FAP instance. The
     * patched Momentum firmware binds that instance to THIS live
     * BLE RpcAppSystem (resolved app-side via
     * rpc_system_app_get_active()).
     *
     * The backend then owns the single BLE RPC connection and
     * waits for the asynchronous APP_STARTED frame.
     */
    println!(
        "[{}] RPC ready - waiting for manual PC Monitor (self-attach)",
        id.to_string()
    );

    /*
     * Manual-only, self-attach model - the backend does NOT launch the
     * FAP.
     *
     * PC Monitor is opened by the user on the Flipper. On this GAP
     * connection the firmware opens the BLE RPC session, and the
     * already-open FAP attaches to it itself via
     * rpc_system_app_get_active(), then announces APP_STARTED. We just
     * wait for that frame below - no App.Start, no second instance, no
     * ping-pong.
     *
     * Do not issue any RPC request before APP_STARTED: a synchronous
     * request could consume the asynchronous APP_STARTED frame. The FAP
     * re-announces APP_STARTED until telemetry flows, so a missed first
     * frame is recovered without a reconnect.
     */

    loop {
        if let Err(err) = wait_for_pc_monitor(
            &mut notifications,
            &mut rx_buffer,
            &id,
            &flipper,
            &tx_char,
            &proxy_rx,
        )
        .await
        {
            teardown_session(&flipper, &format!("waiting for PC Monitor failed: {}", err)).await;
            return;
        }

        /*
         * FAP is now attached to the existing RPC session.
         */
        system_info::set_telemetry_enabled(true);

        let mut system = sysinfo::System::new_all();

        /*
         * Query the real Flipper battery immediately,
         * then once per minute through the same RPC session.
         */
        let mut last_flipper_battery_poll: Option<Instant> = None;

        println!("[{}] Sending REAL telemetry over RPC...", id.to_string());

        /*
         * Stream telemetry until the application disappears.
         */
        loop {
            /*
             * Serialize external flipperble RPC
             * requests with PC Monitor telemetry.
             * There is still only one BLE RPC owner.
             */
            if try_proxy_request(
                &proxy_rx,
                &flipper,
                &tx_char,
                &mut notifications,
                &mut rx_buffer,
            )
            .await
            {
                continue;
            }

            let info = system_info::SystemInfo::get_system_info(&mut system).await;

            let telemetry = telemetry_packet(&info);

            let request = build_data_exchange_request(command_id, &telemetry);

            match rpc_request(
                &flipper,
                &tx_char,
                &mut notifications,
                &mut rx_buffer,
                command_id,
                request,
            )
            .await
            {
                Ok(()) => {
                    println!(
                        "[{}] CPU={} RAM={}% GPU={} BAT={}%",
                        id.to_string(),
                        info.cpu_usage,
                        info.ram_usage,
                        info.gpu_usage,
                        info.battery_usage,
                    );

                    /*
                     * PC Monitor already received this telemetry packet.
                     *
                     * Flipper battery polling is intentionally secondary:
                     * it must never delay the first DataExchange packet.
                     */
                    let battery_poll_due = last_flipper_battery_poll
                        .map(|last| last.elapsed() >= Duration::from_secs(30))
                        .unwrap_or(true);

                    if battery_poll_due {
                        /*
                         * Use a separate RPC command ID from the telemetry
                         * request that just completed.
                         */
                        command_id = command_id.wrapping_add(1);

                        if command_id < 0x8000_0000 {
                            command_id = 0x8000_0000;
                        }

                        match request_flipper_battery(
                            &flipper,
                            &tx_char,
                            &mut notifications,
                            &mut rx_buffer,
                            command_id,
                        )
                        .await
                        {
                            Ok(level) => {
                                update_airbattery(level);

                                println!("[{}] Flipper battery={}%", id.to_string(), level);
                            }

                            Err(err) => {
                                /*
                                 * Battery integration is optional.
                                 * Never kill PC Monitor telemetry because
                                 * a battery query failed.
                                 */
                                eprintln!(
                                    "[{}] Flipper battery query warning: {}",
                                    id.to_string(),
                                    err
                                );
                            }
                        }

                        last_flipper_battery_poll = Some(Instant::now());
                    }
                }

                Err(err) if err.contains("status 21") => {
                    /*
                     * PC Monitor is no longer attached.
                     *
                     * This is normal when the user presses
                     * Back. Do NOT disconnect BLE, restart
                     * the backend, or reopen the FAP.
                     */
                    println!("[{}] PC Monitor closed - disconnecting RPC", id.to_string());

                    system_info::set_telemetry_enabled(false);

                    BLE_CONNECTED_EVENT.store(false, Ordering::Relaxed);

                    /*
                     * CoreBluetooth may briefly replay an old
                     * flags=0x01 advertisement after this
                     * intentional disconnect.
                     *
                     * Do not reconnect until a fresh flags=0x00
                     * advertisement has been observed.
                     */
                    WAIT_FOR_IDLE_ADVERTISEMENT.store(true, Ordering::Relaxed);

                    STALE_ADVERTISEMENT_LOGGED.store(false, Ordering::Relaxed);

                    IDLE_ADVERTISEMENT_DEADLINE_MS.store(mono_now_ms() + 2000, Ordering::Relaxed);

                    /*
                     * Allow the Flipper-side RPC/FAP teardown to finish
                     * before CoreBluetooth drops the transport.
                     */
                    sleep(Duration::from_millis(500)).await;

                    let _ = timeout(Duration::from_secs(5), flipper.disconnect()).await;

                    return;
                }

                Err(err) => {
                    /*
                     * Real BLE/RPC transport failure.
                     *
                     * Recover in-process: tear down this session and
                     * disconnect. The main loop sees DeviceDisconnected,
                     * restarts scanning and reconnects on the next
                     * advertising request flag - without killing the
                     * backend or the flipperble proxy socket.
                     */
                    teardown_session(
                        &flipper,
                        &format!("RPC telemetry transport failed: {}", err),
                    )
                    .await;

                    return;
                }
            }

            command_id = command_id.wrapping_add(1);

            if command_id < 0x8000_0000 {
                command_id = 0x8000_0000;
            }

            sleep(Duration::from_secs(1)).await;
        }
    }
}

/*
 * --------------------------------------------------------------------------
 * Reconnect watchdog
 * --------------------------------------------------------------------------
 */

/*
 * --------------------------------------------------------------------------
 * Main
 * --------------------------------------------------------------------------
 */

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();

    BLE_CONNECTED_EVENT.store(false, Ordering::Relaxed);

    /*
     * A previous backend crash must never leave
     * flipperble believing RPC is still active.
     */
    let _ = fs::remove_file(RPC_ACTIVE_MARKER);

    system_info::set_telemetry_enabled(false);

    /*
     * Local flipperble -> backend RPC bridge.
     */
    let (proxy_tx, proxy_rx_raw) = mpsc::channel::<ProxyRequest>(32);

    let proxy_rx: ProxyReceiver = Arc::new(Mutex::new(proxy_rx_raw));

    tokio::spawn(rpc_socket_server(proxy_tx));

    /*
     * Passive multi-client RPC event stream.
     *
     * Capacity is intentionally generous; lagging observers drop old
     * frames rather than slowing PC Monitor or normal RPC traffic.
     */
    let (event_tx, _) = broadcast::channel::<Vec<u8>>(256);

    RPC_EVENT_TX
        .set(event_tx)
        .map_err(|_| "RPC event channel already initialized")?;

    tokio::spawn(rpc_event_socket_server());

    /*
     * BLE supervision loop.
     *
     * The whole CoreBluetooth session (Manager, adapter, scan) is
     * (re)created here. When it becomes unusable - the event stream
     * ends, the sleep/wake watchdog gives up, or the user toggles the
     * Mac's Bluetooth off and on - we tear it down and rebuild it
     * in-process instead of exiting, so the backend keeps running and
     * reconnects on its own (no launchd restart required).
     */
    let mut ble_backoff_ms: u64 = 0;

    'ble: loop {
        if ble_backoff_ms > 0 {
            sleep(Duration::from_millis(ble_backoff_ms)).await;
        }

        BLE_CONNECTED_EVENT.store(false, Ordering::Relaxed);
        let _ = fs::remove_file(RPC_ACTIVE_MARKER);
        system_info::set_telemetry_enabled(false);

        let manager = match Manager::new().await {
            Ok(m) => m,
            Err(err) => {
                eprintln!("BLE Manager init failed: {} - retrying", err);
                ble_backoff_ms = (ble_backoff_ms + 2000).min(10000);
                continue 'ble;
            }
        };

        let central = flipper_manager::get_central(&manager).await;

        match central.adapter_info().await {
            Ok(info) => println!("Found {:?} adapter", info),
            Err(err) => {
                eprintln!("BLE adapter not ready: {} - retrying", err);
                ble_backoff_ms = (ble_backoff_ms + 2000).min(10000);
                continue 'ble;
            }
        }

        let mut events = match central.events().await {
            Ok(e) => e,
            Err(err) => {
                eprintln!("BLE events() failed: {} - retrying", err);
                ble_backoff_ms = (ble_backoff_ms + 2000).min(10000);
                continue 'ble;
            }
        };

        println!("Scanning for Flipper RPC...");

        if let Err(err) = central.start_scan(ScanFilter::default()).await {
            eprintln!("BLE start_scan failed: {} - retrying", err);
            ble_backoff_ms = (ble_backoff_ms + 2000).min(10000);
            continue 'ble;
        }

        let mut reconnect_tick = tokio::time::interval(Duration::from_secs(2));
        reconnect_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut last_flipper_id: Option<PeripheralId> = None;
        let mut watchdog_failures: u8 = 0;
        let mut data_workers: HashMap<PeripheralId, tokio::task::JoinHandle<()>> = HashMap::new();
        let mut advertised_battery_cache: HashMap<PeripheralId, (u8, Instant)> = HashMap::new();

        /*
         * Last time CoreBluetooth delivered ANY event. The Flipper
         * advertises continuously, so a long silence while we are not
         * connected means the central is wedged (typically after the
         * Mac's Bluetooth was toggled off/on) - see the watchdog.
         */
        let mut last_event_at = Instant::now();

        loop {
            tokio::select! {

                maybe_event = events.next() => {

                    last_event_at = Instant::now();

                    let Some(event) = maybe_event else {
                        /*
                         * The CoreBluetooth event stream itself ended.
                         * This is unrecoverable in-process; fall back to
                         * the launchd restart safety net.
                         */
                        eprintln!(
                            "CoreBluetooth event stream ended - restarting backend"
                        );

                        std::process::exit(EXIT_RESTART);
                    };

                    match event {
                CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => {

                    last_flipper_id = Some(id.clone());

                    if let Some(flp) = flipper_manager::get_flipper(&central, &id).await {
                        let mut connect_requested = false;
                        let mut request_flags: u8 = 0;

                        /*
                         * Passive FZ advertisement:
                         *
                         *   46 5A 01 BAT FLAGS
                         *
                         * FLAGS bit 0:
                         *   0 = idle
                         *   1 = PC Monitor requests RPC
                         */
                        if let Ok(Some(properties)) = flp.properties().await {
                            if let Some(data) = properties.manufacturer_data.get(&0xFFFF) {
                                if data.len() >= 5
                                    && data[0] == 0x46
                                    && data[1] == 0x5A
                                    && data[2] == 0x01
                                {
                                    let battery = data[3];
                                    let flags = data[4];

                                    /*
                                     * After an intentional disconnect,
                                     * CoreBluetooth may briefly replay
                                     * a stale flags=0x01 advertisement.
                                     *
                                     * Ignore request flags until a fresh
                                     * flags=0x00 advertisement is seen.
                                     */
                                    if WAIT_FOR_IDLE_ADVERTISEMENT.load(Ordering::Relaxed) {
                                        let now_ms = mono_now_ms();

                                        let deadline_ms =
                                            IDLE_ADVERTISEMENT_DEADLINE_MS.load(Ordering::Relaxed);

                                        if (flags & 0x01) == 0 {
                                            /*
                                             * Best case:
                                             * we observed the real idle advertisement.
                                             */
                                            WAIT_FOR_IDLE_ADVERTISEMENT.store(false, Ordering::Relaxed);

                                            IDLE_ADVERTISEMENT_DEADLINE_MS.store(0, Ordering::Relaxed);

                                            STALE_ADVERTISEMENT_LOGGED.store(false, Ordering::Relaxed);

                                            println!(
                                                "[{}] Fresh idle advertisement confirmed",
                                                id.to_string()
                                            );
                                        } else if deadline_ms != 0 && now_ms >= deadline_ms {
                                            /*
                                             * Fail-safe:
                                             * do not remain locked forever if
                                             * CoreBluetooth never delivers flags=0.
                                             */
                                            WAIT_FOR_IDLE_ADVERTISEMENT.store(false, Ordering::Relaxed);

                                            IDLE_ADVERTISEMENT_DEADLINE_MS.store(0, Ordering::Relaxed);

                                            STALE_ADVERTISEMENT_LOGGED.store(false, Ordering::Relaxed);

                                            println!(
                                                "[{}] Idle advertisement guard timed out after 2s",
                                                id.to_string()
                                            );
                                        } else {
                                            /*
                                             * Ignore stale flags=1.
                                             * Log only the first stale event.
                                             */
                                            if !STALE_ADVERTISEMENT_LOGGED.swap(true, Ordering::Relaxed)
                                            {
                                                println!(
                                                    "[{}] Ignoring stale PC Monitor request after disconnect",
                                                    id.to_string()
                                                );
                                            }

                                            continue;
                                        }
                                    }

                                    connect_requested = (flags & 0x01) != 0;
                                    request_flags = flags;

                                    if battery <= 100 {
                                        let refresh = match advertised_battery_cache.get(&id) {
                                            Some((old_level, last)) => {
                                                *old_level != battery
                                                    || last.elapsed() >= Duration::from_secs(30)
                                            }

                                            None => true,
                                        };

                                        if refresh {
                                            update_airbattery(battery);

                                            advertised_battery_cache
                                                .insert(id.clone(), (battery, Instant::now()));

                                            println!(
                                                "[{}] Advertising battery={}%, flags=0x{:02X}",
                                                id.to_string(),
                                                battery,
                                                flags
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        /*
                         * Idle means passive scan only.
                         */
                        if !connect_requested {
                            continue;
                        }

                        if matches!(flp.is_connected().await, Ok(true)) {
                            continue;
                        }

                        if CONNECT_IN_PROGRESS
                            .compare_exchange(
                                false,
                                true,
                                Ordering::AcqRel,
                                Ordering::Relaxed
                            )
                            .is_err()
                        {
                            continue;
                        }

                        println!(
                            "[{}] PC Monitor request detected - connecting RPC (flags=0x{:02X})",
                            id.to_string(),
                            request_flags
                        );

                        match timeout(CONNECT_TIMEOUT, flp.connect()).await {
                            Ok(Ok(())) => {}

                            Ok(Err(err)) => {
                                println!(
                                    "[{}] Connect attempt failed: {}",
                                    id.to_string(),
                                    err
                                );
                            }

                            Err(_) => {
                                println!(
                                    "[{}] Connect attempt timed out",
                                    id.to_string()
                                );
                            }
                        }

                        CONNECT_IN_PROGRESS.store(
                            false,
                            Ordering::Release
                        );
                    }
                }

                CentralEvent::DeviceConnected(id) => {
                    watchdog_failures = 0;

                    CONNECT_IN_PROGRESS.store(false, Ordering::Release);
                    BLE_CONNECTED_EVENT.store(true, Ordering::Relaxed);

                    if let Some(worker) = data_workers.remove(&id) {
                        worker.abort();
                    }

                    if let Some(flp) = flipper_manager::get_flipper(&central, &id).await {
                        sleep(Duration::from_millis(500)).await;

                        match timeout(Duration::from_secs(8), flp.discover_services()).await {
                            Ok(Ok(())) => {}

                            Ok(Err(err)) => {
                                eprintln!("[{}] Service discovery failed: {}", id.to_string(), err);

                                continue;
                            }

                            Err(_) => {
                                eprintln!("[{}] Service discovery timed out", id.to_string());

                                continue;
                            }
                        }

                        /*
                         * From this point the backend owns the
                         * active Flipper RPC transport.
                         */
                        let _ = fs::write(RPC_ACTIVE_MARKER, b"active\n");

                        println!("[{}] Connected to Flipper RPC", id.to_string());

                        data_workers
                            .insert(id.clone(), tokio::spawn(data_sender(flp, proxy_rx.clone())));
                    }
                }

                CentralEvent::DeviceDisconnected(id) => {
                    CONNECT_IN_PROGRESS.store(false, Ordering::Release);
                    BLE_CONNECTED_EVENT.store(false, Ordering::Relaxed);

                    let _ = fs::remove_file(RPC_ACTIVE_MARKER);

                    system_info::set_telemetry_enabled(false);

                    if let Some(worker) = data_workers.remove(&id) {
                        worker.abort();
                    }

                    println!("[{}] Flipper RPC disconnected", id.to_string());

                    /*
                     * A Flipper reboot invalidates the complete
                     * GATT/RPC session.
                     *
                     * Do not attempt to reuse it from another task.
                     * Restart discovery and let DeviceDiscovered /
                     * DeviceUpdated create one clean connection.
                     */
                    let _ = central.stop_scan().await;

                    sleep(Duration::from_millis(500)).await;

                    match central.start_scan(ScanFilter::default()).await {
                        Ok(()) => {
                            println!("[{}] BLE scan restarted after disconnect", id.to_string());
                        }

                        Err(err) => {
                            eprintln!("[{}] Failed to restart BLE scan: {}", id.to_string(), err);
                        }
                    }
                }

                        _ => {}
                    }
                }

                /*
                 * Sleep/wake reconnect watchdog.
                 *
                 * This executes in the SAME main task as the normal
                 * CoreBluetooth event handler. It cannot create a second
                 * independent BLE worker.
                 */
                _ = reconnect_tick.tick() => {

                    /*
                     * Stale-central watchdog.
                     *
                     * The Flipper advertises continuously, so if we are
                     * NOT connected and have seen no BLE events at all
                     * for a while, CoreBluetooth is wedged - typically
                     * after the Mac's Bluetooth was toggled off/on. In-
                     * process recovery of a wedged central is unreliable
                     * on macOS, so exit and let launchd restart us with a
                     * completely fresh CoreBluetooth stack.
                     */
                    if !BLE_CONNECTED_EVENT.load(Ordering::Relaxed)
                        && last_event_at.elapsed() > Duration::from_secs(12)
                    {
                        eprintln!(
                            "No BLE events for 12s while disconnected - CoreBluetooth wedged, restarting"
                        );
                        std::process::exit(EXIT_RESTART);
                    }

                    if BLE_CONNECTED_EVENT.load(Ordering::Relaxed) {
                        continue;
                    }

                    /*
                     * During intentional PC Monitor shutdown we deliberately
                     * suppress stale flags=1 advertisements.
                     */
                    if WAIT_FOR_IDLE_ADVERTISEMENT.load(Ordering::Relaxed) {
                        continue;
                    }

                    if CONNECT_IN_PROGRESS.load(Ordering::Acquire) {
                        continue;
                    }

                    let Some(id) = last_flipper_id.clone() else {
                        continue;
                    };

                    let Some(flp) =
                        flipper_manager::get_flipper(&central, &id).await
                    else {
                        continue;
                    };

                    if matches!(flp.is_connected().await, Ok(true)) {
                        continue;
                    }

                    let properties = match flp.properties().await {
                        Ok(Some(properties)) => properties,
                        _ => continue,
                    };

                    let Some(data) =
                        properties.manufacturer_data.get(&0xFFFF)
                    else {
                        continue;
                    };

                    if data.len() < 5
                        || data[0] != 0x46
                        || data[1] != 0x5A
                        || data[2] != 0x01
                    {
                        continue;
                    }

                    let flags = data[4];

                    if (flags & 0x01) == 0 {
                        continue;
                    }

                    if CONNECT_IN_PROGRESS
                        .compare_exchange(
                            false,
                            true,
                            Ordering::AcqRel,
                            Ordering::Relaxed
                        )
                        .is_err()
                    {
                        continue;
                    }

                    println!(
                        "[{}] Reconnect watchdog detected PC Monitor request",
                        id.to_string()
                    );

                    match timeout(
                        CONNECT_TIMEOUT,
                        flp.connect()
                    )
                    .await
                    {
                        Ok(Ok(())) => {
                            watchdog_failures = 0;

                            println!(
                                "[{}] Reconnect watchdog BLE connect requested",
                                id.to_string()
                            );
                        }

                        Ok(Err(err)) => {
                            watchdog_failures =
                                watchdog_failures.saturating_add(1);

                            println!(
                                "[{}] Reconnect watchdog connect failed ({}/3): {}",
                                id.to_string(),
                                watchdog_failures,
                                err
                            );

                            if watchdog_failures >= 3 {
                                eprintln!(
                                    "[{}] CoreBluetooth recovery failed 3 times - restarting backend",
                                    id.to_string()
                                );

                                ble_backoff_ms = 2000;
                        break;
                            }
                        }

                        Err(_) => {
                            watchdog_failures =
                                watchdog_failures.saturating_add(1);

                            println!(
                                "[{}] Reconnect watchdog connect timed out ({}/3)",
                                id.to_string(),
                                watchdog_failures
                            );

                            if watchdog_failures >= 3 {
                                eprintln!(
                                    "[{}] CoreBluetooth recovery timed out 3 times - restarting backend",
                                    id.to_string()
                                );

                                ble_backoff_ms = 2000;
                        break;
                            }
                        }
                    }

                    CONNECT_IN_PROGRESS.store(
                        false,
                        Ordering::Release
                    );
                }
            }
        }

        /*
         * The inner event loop ended (event stream closed, watchdog
         * gave up, or Bluetooth was toggled). Drop the workers and the
         * scan, then rebuild CoreBluetooth on the next outer iteration.
         */
        for (_, worker) in data_workers.drain() {
            worker.abort();
        }
        let _ = central.stop_scan().await;
        BLE_CONNECTED_EVENT.store(false, Ordering::Relaxed);
        let _ = fs::remove_file(RPC_ACTIVE_MARKER);
        system_info::set_telemetry_enabled(false);

        eprintln!("Reinitializing CoreBluetooth session");
    }
}
