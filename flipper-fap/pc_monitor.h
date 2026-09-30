#pragma once

#include <furi.h>
#include <furi_hal.h>
#include <furi_hal_bt.h>
#include "helpers/ble_serial.h"
#include <bt/bt_service/bt.h>
#include <gui/gui.h>
#include <gui/elements.h>
#include <notification/notification_messages.h>
#include <input/input.h>
#include <storage/storage.h>
#include <rpc/rpc_app.h>

#include "views/bars_view.h"
#include "views/connect_view.h"
#include "views/status_view.h"

#define TAG                   "PCMonitor"
#define BT_SERIAL_BUFFER_SIZE 128

#define SCREEN_HEIGHT 64
#define LINE_HEIGHT   11

#define BAR_X     30
#define BAR_WIDTH 97

typedef enum {
    BtStateChecking,
    BtStateInactive,
    BtStateWaiting,
    BtStateRecieving,
    BtStateNoData,
    BtStateLost
} BtState;

#pragma pack(push, 1)
typedef struct {
    uint8_t cpu_usage;
    uint16_t ram_max;
    uint8_t ram_usage;
    char ram_unit[4];

    uint8_t gpu_usage;
    uint8_t battery_usage;

    uint8_t cpu_temp;
    uint8_t gpu_temp;
    uint8_t ssd_temp;
    uint8_t battery_temp;
} DataStruct;
#pragma pack(pop)

typedef struct PcMonitorApp {
    Gui* gui;
    ViewPort* view_port;
    FuriMutex* app_mutex;
    FuriMessageQueue* event_queue;
    NotificationApp* notification;

    RpcAppSystem* rpc;
    bool rpc_mode;
    bool rpc_should_exit;

    /*
     * Set when the RPC transport disappears from under us
     * (SessionClose) while PC Monitor is still open. When true,
     * the advertising request flag is kept ON at exit so the
     * backend can rebuild a fresh RPC session and relaunch the
     * FAP. On a clean user-initiated exit it stays false and the
     * flag is cleared.
     */
    bool preserve_request_on_exit;

    BtState bt_state;

    /*
     * True while the "keep backlight on" hold is active (UP arrow).
     * Mirrors the notification enforce state so the temperatures
     * page can show the little sun indicator.
     */
    bool backlight_hold;

    DataStruct data;
    uint8_t lines_count;
    uint8_t page;
    uint32_t last_packet;
} PcMonitorApp;
