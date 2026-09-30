#include "../pc_monitor.h"
#include "connect_view.h"

void draw_connect_view(
    Canvas* canvas,
    PcMonitorApp* app) {

    canvas_clear(canvas);
    canvas_set_color(canvas, ColorBlack);

    /* Title */
    canvas_set_font(canvas, FontPrimary);
    canvas_draw_str(canvas, 2, 11, "PC Monitor");

    /*
     * Link indicator, top-right.
     * Filled disc = attached to the RPC session, hollow = still
     * connecting.
     */
    if(app->rpc_mode) {
        canvas_draw_disc(canvas, 122, 8, 3);
    } else {
        canvas_draw_circle(canvas, 122, 8, 3);
    }

    canvas_set_font(canvas, FontSecondary);

    /* BLE stack state */
    canvas_draw_str(
        canvas,
        2,
        26,
        furi_hal_bt_is_active() ? "BLE:  Active" : "BLE:  Off");

    /* Mac / RPC link state */
    canvas_draw_str(
        canvas,
        2,
        37,
        app->rpc_mode ? "Mac:  Linked" : "Mac:  Connecting...");

    /* Data state (with age when the link went quiet) */
    char data_line[32];

    if(app->bt_state == BtStateRecieving) {
        snprintf(data_line, sizeof(data_line), "Data: Streaming");
    } else if(app->bt_state == BtStateLost) {
        uint32_t per_sec = furi_ms_to_ticks(1000);
        uint32_t age = per_sec ? (furi_get_tick() - app->last_packet) / per_sec : 0;
        snprintf(data_line, sizeof(data_line), "Data: Lost %lus", (unsigned long)age);
    } else if(app->rpc_mode) {
        snprintf(data_line, sizeof(data_line), "Data: waiting...");
    } else {
        snprintf(data_line, sizeof(data_line), "Data: --");
    }

    canvas_draw_str(canvas, 2, 48, data_line);

    /* Context-appropriate button hint at the bottom. */
    if(app->rpc_mode) {
        canvas_draw_str(canvas, 2, 62, "^ on  v off  <> page");
    } else {
        canvas_draw_str(canvas, 2, 62, "Back = cancel");
    }
}
