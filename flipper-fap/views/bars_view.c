#include "bars_view.h"

static void draw_percent_bar(
    Canvas* canvas,
    uint8_t line,
    const char* label,
    uint8_t value) {

    const uint8_t spacing = SCREEN_HEIGHT / 4;
    const uint8_t margin_top = (spacing - LINE_HEIGHT) / 2;

    char str[16];

    canvas_draw_str(
        canvas,
        1,
        margin_top + line * spacing + 9,
        label
    );

    if(value <= 100) {
        snprintf(
            str,
            sizeof(str),
            "%u%%",
            value
        );

        elements_progress_bar_with_text(
            canvas,
            BAR_X,
            margin_top + line * spacing,
            BAR_WIDTH,
            value / 100.0f,
            str
        );
    } else {
        elements_progress_bar_with_text(
            canvas,
            BAR_X,
            margin_top + line * spacing,
            BAR_WIDTH,
            0.0f,
            "--"
        );
    }
}

static void draw_main_page(
    Canvas* canvas,
    PcMonitorApp* app) {

    char ram[32];

    /*
     * DataStruct::ram_unit is exactly 4 bytes on the wire.
     * Never pass it directly to %s because the RPC packet
     * does not guarantee a trailing NUL byte.
     */
    char ram_unit[5];

    memcpy(
        ram_unit,
        app->data.ram_unit,
        sizeof(app->data.ram_unit)
    );

    ram_unit[4] = '\0';

    draw_percent_bar(
        canvas,
        0,
        "CPU",
        app->data.cpu_usage
    );

    canvas_draw_str(
        canvas,
        1,
        9 + SCREEN_HEIGHT / 4,
        "RAM"
    );

    /*
     * ram_max is stored in tenths of ram_unit.
     *
     * Example:
     *   ram_max = 160
     *   ram_unit = "GB"
     *   => 16.0 GB total
     *
     * Avoid floating-point printf on Flipper.
     */
    if(app->data.ram_usage <= 100 &&
       app->data.ram_max > 0) {

        uint32_t total_tenths =
            app->data.ram_max;

        uint32_t used_tenths =
            (
                total_tenths *
                app->data.ram_usage +
                50
            ) / 100;

        snprintf(
            ram,
            sizeof(ram),
            "%lu.%lu/%lu.%lu %s",
            (unsigned long)(
                used_tenths / 10
            ),
            (unsigned long)(
                used_tenths % 10
            ),
            (unsigned long)(
                total_tenths / 10
            ),
            (unsigned long)(
                total_tenths % 10
            ),
            ram_unit
        );

        elements_progress_bar_with_text(
            canvas,
            BAR_X,
            SCREEN_HEIGHT / 4,
            BAR_WIDTH,
            app->data.ram_usage / 100.0f,
            ram
        );

    } else {

        elements_progress_bar_with_text(
            canvas,
            BAR_X,
            SCREEN_HEIGHT / 4,
            BAR_WIDTH,
            0.0f,
            "--"
        );
    }

    draw_percent_bar(
        canvas,
        2,
        "GPU",
        app->data.gpu_usage
    );

    draw_percent_bar(
        canvas,
        3,
        "BAT",
        app->data.battery_usage
    );
}

static void draw_temperature_row(
    Canvas* canvas,
    uint8_t y,
    const char* label,
    uint8_t temp) {

    char str[16];

    canvas_draw_str(
        canvas,
        10,
        y,
        label
    );

    if(temp != UINT8_MAX) {
        snprintf(
            str,
            sizeof(str),
            "%u C",
            temp
        );
    } else {
        snprintf(
            str,
            sizeof(str),
            "-- C"
        );
    }

    canvas_draw_str(
        canvas,
        72,
        y,
        str
    );
}

/*
 * Small sun glyph, top-right corner. Drawn only while the
 * "keep backlight on" hold is active so the state is visible
 * at a glance on the temperatures page.
 */
static void draw_backlight_sun(Canvas* canvas) {
    const uint8_t cx = 121; /* centre X */
    const uint8_t cy = 7;   /* centre Y */
    const uint8_t core = 2; /* core radius */

    /* Core disc */
    canvas_draw_disc(canvas, cx, cy, core);

    /* 8 rays around the core */
    const int8_t rays[8][2] = {
        {0, -1}, {0, 1}, {-1, 0}, {1, 0},
        {-1, -1}, {1, -1}, {-1, 1}, {1, 1},
    };

    const uint8_t inner = core + 1;
    const uint8_t outer = core + 3;

    for(uint8_t i = 0; i < 8; i++) {
        int8_t dx = rays[i][0];
        int8_t dy = rays[i][1];
        canvas_draw_line(
            canvas,
            cx + dx * inner,
            cy + dy * inner,
            cx + dx * outer,
            cy + dy * outer);
    }
}

static void draw_temperature_page(
    Canvas* canvas,
    PcMonitorApp* app) {

    canvas_set_font(canvas, FontPrimary);

    canvas_draw_str(
        canvas,
        1,
        10,
        "TEMPERATURES"
    );

    if(app->backlight_hold) {
        draw_backlight_sun(canvas);
    }

    canvas_set_font(canvas, FontKeyboard);

    draw_temperature_row(
        canvas,
        22,
        "CPU",
        app->data.cpu_temp
    );

    draw_temperature_row(
        canvas,
        34,
        "GPU",
        app->data.gpu_temp
    );

    draw_temperature_row(
        canvas,
        46,
        "SSD",
        app->data.ssd_temp
    );

    draw_temperature_row(
        canvas,
        58,
        "BAT",
        app->data.battery_temp
    );
}

void draw_bars_view(Canvas* canvas, void* ctx) {
    PcMonitorApp* app = ctx;

    canvas_clear(canvas);
    canvas_set_color(canvas, ColorBlack);
    canvas_set_font(canvas, FontKeyboard);

    if(app->page == 0) {
        draw_main_page(canvas, app);
    } else {
        draw_temperature_page(canvas, app);
    }
}
