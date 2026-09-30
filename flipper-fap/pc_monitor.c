#include <furi_hal_bt.h>

#include <stdio.h>
#include <string.h>
#include "pc_monitor.h"

/*
 * Advertising request flag bits carried in the custom
 * manufacturer-data payload (see the Momentum firmware patch).
 *   bit 0 = PC Monitor wants an RPC connection.
 */
#define PCM_ADV_FLAG_NONE    0x00
#define PCM_ADV_FLAG_CONNECT 0x01

/*
 * Time given to GAP to publish the updated advertising payload
 * before the manual-launch bootstrap instance exits. Named so the
 * value is not a bare magic number scattered in the launch path.
 */
#define PCM_BOOTSTRAP_PUBLISH_MS 2000

/* No-telemetry timeout: RPC session alive but data stopped. */
#define PCM_DATA_LOST_TIMEOUT_MS 5000

/*
 * Set by the BT status callback while the app is on the "Waiting for
 * Mac" screen. Only one PC Monitor FAP instance runs at a time, so
 * file scope is fine. volatile: written from the BT service thread,
 * read from the app thread.
 */
static volatile bool s_host_connected = false;

static void pc_monitor_bt_status_cb(BtStatus status, void* ctx) {
    UNUSED(ctx);
    if(status == BtStatusConnected) {
        s_host_connected = true;
    }
}

/*
 * Backlight "always on" is a ref-counted enforce in the notification
 * service (EnforceOn increments, EnforceAuto decrements; released at
 * 0). We MUST send exactly one EnforceOn and one matching EnforceAuto
 * - re-sending EnforceOn inflates the counter so a single "off" can
 * never release it (and triggers "Incorrect BacklightEnforce use").
 * This single shared flag keeps the pair balanced across both the
 * active screen and the waiting screen.
 */
static bool s_backlight_enforced = false;

static void pcm_backlight_force_on(PcMonitorApp* app) {
    if(!s_backlight_enforced) {
        notification_message_block(
            app->notification, &sequence_display_backlight_enforce_on);
        s_backlight_enforced = true;
    }
    app->backlight_hold = true;
}

static void pcm_backlight_release(PcMonitorApp* app) {
    if(s_backlight_enforced) {
        notification_message_block(
            app->notification, &sequence_display_backlight_enforce_auto);
        s_backlight_enforced = false;
    }
    app->backlight_hold = false;
}



static void pc_monitor_rpc_callback(
    const RpcAppSystemEvent* event,
    void* ctx) {

    PcMonitorApp* app = ctx;

    switch(event->type) {

    case RpcAppEventTypeDataExchange:

        if(event->data.type ==
               RpcAppSystemEventDataTypeBytes &&
           event->data.bytes.size ==
               sizeof(DataStruct)) {

            /*
             * Copy RPC payload BEFORE confirm().
             *
             * confirm() may allow the RPC layer to reuse
             * the underlying event buffer.
             */
            DataStruct incoming;

            memcpy(
                &incoming,
                event->data.bytes.ptr,
                sizeof(DataStruct)
            );

            rpc_system_app_confirm(
                app->rpc,
                true
            );

            /*
             * Publish the complete structure only after
             * we have a safe local copy.
             */
            furi_mutex_acquire(app->app_mutex, FuriWaitForever);

            app->data = incoming;
            app->bt_state = BtStateRecieving;
            app->last_packet = furi_get_tick();

            furi_mutex_release(app->app_mutex);

        } else {

            rpc_system_app_confirm(
                app->rpc,
                false
            );
        }

        break;


    case RpcAppEventTypeAppExit:

        rpc_system_app_confirm(
            app->rpc,
            true
        );

        app->rpc_should_exit = true;

        break;


    case RpcAppEventTypeSessionClose: {

        /*
         * RPC transport is being destroyed.
         *
         * Detach the callback as required by rpc_system_app_free(),
         * forget the dead RPC object and terminate this FAP instance.
         *
         * No mutex.
         * No GUI update.
         * No reattach.
         * No further RPC calls.
         */
        RpcAppSystem* closing_rpc = app->rpc;

        if(closing_rpc) {
            rpc_system_app_set_callback(
                closing_rpc,
                NULL,
                NULL
            );
        }

        app->rpc = NULL;
        app->rpc_mode = false;

        /*
         * Transport disappeared while PC Monitor was active.
         * Keep flags=0x01 so the backend can create a fresh
         * RPC session and App.Start a new instance.
         */
        app->preserve_request_on_exit = true;

        app->rpc_should_exit = true;

        break;
    }


    case RpcAppEventTypeLoadFile:
    case RpcAppEventTypeButtonPress:
    case RpcAppEventTypeButtonRelease:
    case RpcAppEventTypeButtonPressRelease:

        rpc_system_app_confirm(
            app->rpc,
            false
        );

        break;


    case RpcAppEventTypeInvalid:
    default:
        break;
    }
}


static void pc_monitor_rpc_deinit(
    PcMonitorApp* app,
    bool notify_remote) {

    if(app->rpc) {

        /*
         * Normal exit:
         * notify the remote RPC peer.
         *
         * Emergency exit:
         * skip send_exited completely because the
         * RPC transport/backend may be wedged.
         */
        if(notify_remote) {
            rpc_system_app_send_exited(
                app->rpc
            );
        }

        /*
         * Local callback detach does not require a
         * round trip to the Mac.
         */
        rpc_system_app_set_callback(
            app->rpc,
            NULL,
            NULL
        );

        app->rpc = NULL;
    }

    app->rpc_mode = false;
}

static void render_callback(Canvas* canvas, void* ctx) {
    furi_assert(ctx);
    PcMonitorApp* app = ctx;

    furi_mutex_acquire(app->app_mutex, FuriWaitForever);

    switch(app->bt_state) {
    case BtStateWaiting:
        draw_connect_view(
            canvas,
            app);
        break;

    case BtStateRecieving:
        draw_bars_view(canvas, app);
        break;

    default:
        draw_status_view(canvas, app);
        break;
    }

    furi_mutex_release(app->app_mutex);
}

static void input_callback(InputEvent* input_event, void* ctx) {
    furi_assert(ctx);
    FuriMessageQueue* event_queue = ctx;
    furi_message_queue_put(event_queue, input_event, FuriWaitForever);
}

static PcMonitorApp* pc_monitor_alloc() {
    PcMonitorApp* app = calloc(1, sizeof(PcMonitorApp));

    app->page = 0;
    app->lines_count = 4;
    app->app_mutex = furi_mutex_alloc(FuriMutexTypeNormal);
    furi_check(app->app_mutex);

    app->rpc = NULL;
    app->rpc_mode = false;
    app->rpc_should_exit = false;


    app->view_port = view_port_alloc();
    app->event_queue = furi_message_queue_alloc(8, sizeof(InputEvent));
    app->notification = furi_record_open(RECORD_NOTIFICATION);
    app->gui = furi_record_open(RECORD_GUI);

    gui_add_view_port(app->gui, app->view_port, GuiLayerFullscreen);
    view_port_draw_callback_set(app->view_port, render_callback, app);
    view_port_input_callback_set(app->view_port, input_callback, app->event_queue);
    return app;
}

static void pc_monitor_free(PcMonitorApp* app) {

    gui_remove_view_port(app->gui, app->view_port);
    view_port_free(app->view_port);
    furi_message_queue_free(app->event_queue);
    furi_mutex_free(app->app_mutex);
    furi_record_close(RECORD_NOTIFICATION);
    furi_record_close(RECORD_GUI);
    free(app);
}



typedef enum {
    PcmWaitQuit, /* user pressed Back */
    PcmWaitConnected, /* a BLE central (the Mac backend) connected */
} PcmWaitResult;

/*
 * "Waiting for Mac" screen.
 *
 * Stays open showing the connect view until either the user leaves
 * (Back) or the host connects. Detection is event-driven via the BT
 * status callback - no get_active() polling and no advertising churn,
 * which is what kept earlier "always open" attempts unstable.
 *
 * On connect the caller attaches to the live RPC session itself
 * (rpc_system_app_get_active) and keeps running - no relaunch.
 * UP/DOWN still control the backlight while waiting.
 */
static PcmWaitResult pc_monitor_wait_for_host(PcMonitorApp* app) {
    app->bt_state = BtStateWaiting;
    app->rpc_mode = false;
    s_host_connected = false;
    view_port_update(app->view_port);

    Bt* bt = furi_record_open(RECORD_BT);
    bt_set_status_changed_callback(bt, pc_monitor_bt_status_cb, NULL);

    bool quit = false;

    while(!quit && !s_host_connected) {

        InputEvent ev;

        if(furi_message_queue_get(app->event_queue, &ev, 200) == FuriStatusOk) {

            if(ev.key == InputKeyBack &&
               (ev.type == InputTypeShort || ev.type == InputTypeLong)) {
                quit = true;

            } else if(ev.type == InputTypeShort && ev.key == InputKeyUp) {
                /* Keep the screen on. */
                pcm_backlight_force_on(app);

            } else if(ev.type == InputTypeShort && ev.key == InputKeyDown) {
                /* Release and switch off. */
                pcm_backlight_release(app);
                notification_message(
                    app->notification, &sequence_display_backlight_off);
            }
        }

        view_port_update(app->view_port);
    }

    bt_set_status_changed_callback(bt, NULL, NULL);
    furi_record_close(RECORD_BT);

    return quit ? PcmWaitQuit : PcmWaitConnected;
}

int32_t pc_monitor_app(void* p) {
    PcMonitorApp* app =
        pc_monitor_alloc();

    app->preserve_request_on_exit = false;

    /*
     * Tell passive host scanners that PC Monitor
     * wants an RPC connection.
     *
     * FZ advertising flags:
     *   bit 0 = PC Monitor connection request.
     */
    furi_hal_bt_update_advertising_flags(PCM_ADV_FLAG_CONNECT);

    FURI_LOG_I(
        TAG,
        "Advertising RPC request flag ON"
    );

    /*
     * If the BLE stack is not up, no scanner will ever see the
     * request flag. Reflect that on screen instead of silently
     * waiting forever.
     */
    if(!furi_hal_bt_is_active()) {
        FURI_LOG_W(
            TAG,
            "BLE stack inactive - request flag will not be seen"
        );
    }

    app->bt_state =
        BtStateWaiting;

    /* Backlight enforce starts released on every fresh launch. */
    s_backlight_enforced = false;

    /*
     * Manual-only, self-attach model (no App.Start ping-pong).
     *
     * PC Monitor is launched by the user from the Flipper. It raises
     * the advertising request flag so the Mac backend connects, then
     * attaches to that live BLE RPC session itself via
     * rpc_system_app_get_active(). The backend does NOT launch a
     * second instance - the instance you opened is the one that runs.
     *
     * It stays open across backend restarts: on SessionClose it goes
     * back to the "Waiting for Mac" screen and re-attaches when the
     * backend returns. Only Back leaves the app.
     *
     * A legacy "RPC <ptr>" launch argument is still honoured so an old
     * App.Start-based backend keeps working too.
     */
    if(p && strncmp((const char*)p, "RPC", 3) == 0) {
        unsigned long rpc_ctx = 0;
        if(sscanf((const char*)p, "RPC %lX", &rpc_ctx) == 1 && rpc_ctx != 0) {
            app->rpc = (RpcAppSystem*)rpc_ctx;
            FURI_LOG_I(TAG, "Legacy RPC-launch pointer: %p", app->rpc);
        }
    }

    InputEvent event;
    bool emergency_exit = false;
    bool user_quit = false;

    while(!user_quit) {

        /*
         * Obtain a live RPC session. Try an already-open one first
         * (backend connected before us); otherwise wait on the
         * "Waiting for Mac" screen (BT status callback, no polling),
         * then grab the session once the host connects - retrying
         * briefly for the publish delay.
         */
        if(app->rpc == NULL) {
            app->rpc = rpc_system_app_get_active();
        }

        if(app->rpc == NULL) {
            if(pc_monitor_wait_for_host(app) == PcmWaitQuit) {
                user_quit = true;
                break;
            }

            for(uint8_t i = 0; i < 20 && app->rpc == NULL; i++) {
                app->rpc = rpc_system_app_get_active();
                if(app->rpc == NULL) {
                    furi_delay_ms(100);
                }
            }

            if(app->rpc == NULL) {
                FURI_LOG_W(TAG, "Connected but no RPC session yet - waiting again");
                continue;
            }
        }

        /*
         * Attach to the session and stream.
         */
        app->rpc_should_exit = false;
        app->rpc_mode = true;

        rpc_system_app_set_callback(app->rpc, pc_monitor_rpc_callback, app);
        rpc_system_app_send_started(app->rpc);

        /*
         * APP_STARTED can race the backend subscribing to BLE
         * notifications; re-announce every second until telemetry
         * starts flowing.
         */
        uint32_t last_started_send = furi_get_tick();

        view_port_update(app->view_port);

        FURI_LOG_I(TAG, "Attached to BLE RPC session: %p", app->rpc);

        while(!app->rpc_should_exit) {

            if(furi_message_queue_get(app->event_queue, &event, 100) == FuriStatusOk) {

                /* Emergency local exit - never depends on the transport. */
                if(event.type == InputTypeLong && event.key == InputKeyBack) {
                    FURI_LOG_W(TAG, "Emergency Long Back exit - local only");
                    emergency_exit = true;
                    user_quit = true;
                    app->rpc_should_exit = true;
                    break;
                }

                if(event.type == InputTypeShort) {

                    if(event.key == InputKeyBack) {
                        user_quit = true;
                        break;
                    }

                    /* UP = keep backlight permanently ON. */
                    if(event.key == InputKeyUp) {
                        pcm_backlight_force_on(app);
                        continue;
                    }

                    /* DOWN = release always-on and switch display off. */
                    if(event.key == InputKeyDown) {
                        pcm_backlight_release(app);
                        notification_message(
                            app->notification, &sequence_display_backlight_off);
                        continue;
                    }

                    if(event.key == InputKeyLeft || event.key == InputKeyRight) {
                        /* Momentary wake if the screen is in auto/off mode. */
                        if(!s_backlight_enforced) {
                            notification_message(
                                app->notification, &sequence_display_backlight_on);
                        }
                        if(app->bt_state == BtStateRecieving) {
                            app->page ^= 1;
                            view_port_update(app->view_port);
                        }
                        continue;
                    }
                }
            }

            uint32_t now = furi_get_tick();

            /* Re-announce APP_STARTED until telemetry starts flowing. */
            if(app->rpc && app->bt_state != BtStateRecieving &&
               (now - last_started_send >= furi_ms_to_ticks(1000))) {
                rpc_system_app_send_started(app->rpc);
                last_started_send = now;
            }

            /* Telemetry stopped while the RPC session is still alive. */
            if(app->bt_state == BtStateRecieving &&
               (now - app->last_packet > furi_ms_to_ticks(PCM_DATA_LOST_TIMEOUT_MS))) {
                app->bt_state = BtStateLost;
                view_port_update(app->view_port);
            }
        }

        /*
         * Session ended. Detach (no-op if SessionClose already cleared
         * app->rpc). If the user left, quit; otherwise loop back and
         * wait for the backend to return - the app stays open.
         */
        pc_monitor_rpc_deinit(app, !emergency_exit);
        app->rpc = NULL;
        app->rpc_mode = false;

        if(!user_quit) {
            app->bt_state = BtStateWaiting;
            view_port_update(app->view_port);
            FURI_LOG_I(TAG, "RPC session lost - waiting for backend to return");
        }
    }

    /*
     * User exit - clear the request flag so the backend stops
     * connecting, and release any backlight enforcement.
     */
    furi_hal_bt_update_advertising_flags(PCM_ADV_FLAG_NONE);
    FURI_LOG_I(TAG, "Advertising RPC request flag OFF");

    /* Release backlight enforcement if it was left on. */
    pcm_backlight_release(app);

    pc_monitor_free(app);

    return 0;
}
