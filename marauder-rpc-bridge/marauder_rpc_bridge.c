#include <furi.h>
#include <furi_hal.h>
#include <rpc/rpc_app.h>
#include <expansion/expansion.h>

#include <string.h>
#include <stdio.h>

#define TAG "MarauderRPC"
#define UART_BAUD 115200
#define RX_BUFFER_SIZE 1024

typedef struct {
    RpcAppSystem* rpc;
    FuriHalSerialHandle* serial;
    Expansion* expansion;
    FuriStreamBuffer* rx_stream;
    FuriThread* worker;
    volatile bool running;
} MarauderBridge;

static void uart_rx_callback(
    FuriHalSerialHandle* handle,
    FuriHalSerialRxEvent event,
    void* context) {

    MarauderBridge* bridge = context;

    if(event & FuriHalSerialRxEventData) {
        while(furi_hal_serial_async_rx_available(handle)) {
            uint8_t byte = furi_hal_serial_async_rx(handle);
            furi_stream_buffer_send(bridge->rx_stream, &byte, 1, 0);
        }
    }
}

static int32_t uart_worker(void* context) {
    MarauderBridge* bridge = context;
    uint8_t buffer[128];

    while(bridge->running) {
        size_t count = furi_stream_buffer_receive(
            bridge->rx_stream, buffer, sizeof(buffer), 100);

        if(count && bridge->rpc) {
            rpc_system_app_exchange_data(bridge->rpc, buffer, count);
        }
    }

    return 0;
}

static void rpc_callback(
    const RpcAppSystemEvent* event,
    void* context) {

    MarauderBridge* bridge = context;

    switch(event->type) {
    case RpcAppEventTypeDataExchange: {
        if(event->data.type != RpcAppSystemEventDataTypeBytes) {
            rpc_system_app_confirm(bridge->rpc, false);
            break;
        }

        const uint8_t* data = event->data.bytes.ptr;
        size_t size = event->data.bytes.size;

        if(size) {
            furi_hal_serial_tx(bridge->serial, data, size);

            static const uint8_t newline = '\n';
            if(data[size - 1] != '\n') {
                furi_hal_serial_tx(bridge->serial, &newline, 1);
            }

            furi_hal_serial_tx_wait_complete(bridge->serial);
        }

        rpc_system_app_confirm(bridge->rpc, true);
        break;
    }

    case RpcAppEventTypeAppExit:
        rpc_system_app_confirm(bridge->rpc, true);
        bridge->running = false;
        break;

    case RpcAppEventTypeSessionClose:
        bridge->rpc = NULL;
        bridge->running = false;
        break;

    default:
        rpc_system_app_confirm(bridge->rpc, false);
        break;
    }
}

int32_t marauder_rpc_bridge_app(void* p) {
    uint32_t rpc_ctx = 0;

    if(!p || sscanf(p, "RPC %lX", &rpc_ctx) != 1) {
        FURI_LOG_E(TAG, "Must be started through RPC");
        return -1;
    }

    MarauderBridge* bridge = malloc(sizeof(MarauderBridge));
    memset(bridge, 0, sizeof(MarauderBridge));

    bridge->running = true;
    bridge->rpc = (RpcAppSystem*)rpc_ctx;

    bridge->expansion = furi_record_open(RECORD_EXPANSION);
    expansion_disable(bridge->expansion);

    /* Apex 5 V1: Extra UART pins 15/16 = LPUART */
    bridge->serial = furi_hal_serial_control_acquire(FuriHalSerialIdLpuart);

    if(!bridge->serial) {
        expansion_enable(bridge->expansion);
        furi_record_close(RECORD_EXPANSION);
        free(bridge);
        return -2;
    }

    furi_hal_serial_init(bridge->serial, UART_BAUD);

    bridge->rx_stream = furi_stream_buffer_alloc(RX_BUFFER_SIZE, 1);

    bridge->worker = furi_thread_alloc_ex(
        "MarauderUartRX", 1536, uart_worker, bridge);
    furi_thread_start(bridge->worker);

    furi_hal_serial_async_rx_start(
        bridge->serial, uart_rx_callback, bridge, false);

    rpc_system_app_set_callback(bridge->rpc, rpc_callback, bridge);
    rpc_system_app_send_started(bridge->rpc);

    while(bridge->running) {
        furi_delay_ms(100);
    }

    if(bridge->rpc) {
        rpc_system_app_set_callback(bridge->rpc, NULL, NULL);
        rpc_system_app_send_exited(bridge->rpc);
    }

    furi_hal_serial_async_rx_stop(bridge->serial);

    furi_thread_join(bridge->worker);
    furi_thread_free(bridge->worker);
    furi_stream_buffer_free(bridge->rx_stream);

    furi_hal_serial_deinit(bridge->serial);
    furi_hal_serial_control_release(bridge->serial);

    expansion_enable(bridge->expansion);
    furi_record_close(RECORD_EXPANSION);

    free(bridge);
    return 0;
}
