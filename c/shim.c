#include "shim.h"
#include "lvgl.h"

static lv_display_t *disp;
static lv_indev_t *kbd;
static lv_group_t *group;
static lv_obj_t *bg_img;
static lv_obj_t *hint;
static lv_obj_t *list;
static lv_image_dsc_t bg_dsc;
static shim_flush_fn flush_fn;
static shim_log_fn log_fn;
static volatile int32_t choice = -1;

#define KEYQ 16
static uint32_t keyq[KEYQ];
static uint32_t keyq_head, keyq_tail;
static uint32_t key_down; /* key reported pressed, release pending */

static void flush_cb(lv_display_t *d, const lv_area_t *a, uint8_t *px)
{
    int32_t w = lv_area_get_width(a);
    int32_t h = lv_area_get_height(a);
    uint32_t stride = lv_draw_buf_width_to_stride(w, lv_display_get_color_format(d));
    flush_fn(a->x1, a->y1, w, h, px, stride);
    lv_display_flush_ready(d);
}

static void log_cb(lv_log_level_t level, const char *buf)
{
    (void)level;
    if(log_fn) log_fn(buf);
}

static void kbd_read(lv_indev_t *in, lv_indev_data_t *data)
{
    (void)in;
    if(key_down) {
        data->key = key_down;
        data->state = LV_INDEV_STATE_RELEASED;
        key_down = 0;
        data->continue_reading = keyq_head != keyq_tail;
        return;
    }
    if(keyq_head != keyq_tail) {
        key_down = keyq[keyq_tail++ % KEYQ];
        data->key = key_down;
        data->state = LV_INDEV_STATE_PRESSED;
        data->continue_reading = true;
        return;
    }
    data->state = LV_INDEV_STATE_RELEASED;
}

void shim_init(uint32_t w, uint32_t h, shim_flush_fn flush, shim_tick_fn tick,
               shim_log_fn log, void *draw_buf, uint32_t draw_buf_bytes)
{
    flush_fn = flush;
    log_fn = log;
    lv_init();
    lv_log_register_print_cb(log_cb);
    lv_tick_set_cb(tick);
    disp = lv_display_create((int32_t)w, (int32_t)h);
    lv_display_set_color_format(disp, LV_COLOR_FORMAT_XRGB8888);
    lv_display_set_buffers(disp, draw_buf, NULL, draw_buf_bytes,
                           LV_DISPLAY_RENDER_MODE_PARTIAL);
    lv_display_set_flush_cb(disp, flush_cb);

    lv_obj_t *scr = lv_screen_active();
    lv_obj_set_scrollable(scr, false);
    lv_obj_set_style_bg_color(scr, lv_color_black(), 0);
    lv_obj_set_style_bg_opa(scr, LV_OPA_COVER, 0);
    lv_obj_set_style_pad_all(scr, 0, 0);

    group = lv_group_create();
    kbd = lv_indev_create();
    lv_indev_set_type(kbd, LV_INDEV_TYPE_KEYPAD);
    lv_indev_set_read_cb(kbd, kbd_read);
    lv_indev_set_group(kbd, group);
}

void shim_set_background(const uint8_t *px, uint32_t w, uint32_t h)
{
    lv_memzero(&bg_dsc, sizeof(bg_dsc));
    bg_dsc.header.magic = LV_IMAGE_HEADER_MAGIC;
    bg_dsc.header.cf = LV_COLOR_FORMAT_XRGB8888;
    bg_dsc.header.w = w;
    bg_dsc.header.h = h;
    bg_dsc.header.stride = w * 4;
    bg_dsc.data = px;
    bg_dsc.data_size = w * h * 4;
    if(!bg_img) {
        bg_img = lv_image_create(lv_screen_active());
        lv_obj_set_pos(bg_img, 0, 0);
        lv_obj_set_clickable(bg_img, false);
    }
    lv_image_set_src(bg_img, &bg_dsc);
    lv_obj_invalidate(lv_screen_active());
}

void shim_show_hint(const char *text)
{
    if(!hint) {
        hint = lv_label_create(lv_screen_active());
        lv_obj_set_style_text_color(hint, lv_color_white(), 0);
        lv_obj_set_style_bg_color(hint, lv_color_black(), 0);
        lv_obj_set_style_bg_opa(hint, LV_OPA_70, 0);
        lv_obj_set_style_pad_hor(hint, 14, 0);
        lv_obj_set_style_pad_ver(hint, 6, 0);
        lv_obj_set_style_radius(hint, 6, 0);
    }
    lv_label_set_text(hint, text);
    lv_obj_align(hint, LV_ALIGN_BOTTOM_MID, 0, -24);
}

void shim_hide_hint(void)
{
    if(hint) {
        lv_obj_delete(hint);
        hint = NULL;
    }
}

static void item_clicked(lv_event_t *e)
{
    choice = (int32_t)(intptr_t)lv_event_get_user_data(e);
}

void shim_show_list(const char *title, const char *const *items, int32_t n)
{
    shim_hide_hint();
    choice = -1;
    list = lv_list_create(lv_screen_active());
    lv_obj_set_size(list, 520, LV_SIZE_CONTENT);
    lv_obj_set_style_max_height(list, lv_pct(90), 0);
    lv_obj_set_style_pad_top(list, 10, 0);
    lv_obj_center(list);
    lv_list_add_text(list, title);
    for(int32_t i = 0; i < n; i++) {
        lv_obj_t *b = lv_list_add_button(list, NULL, items[i]);
        lv_obj_add_event_cb(b, item_clicked, LV_EVENT_CLICKED, (void *)(intptr_t)i);
        lv_group_add_obj(group, b);
    }
    lv_group_focus_obj(lv_obj_get_child(list, 1));
}

void shim_hide_list(void)
{
    if(list) {
        lv_group_remove_all_objs(group);
        lv_obj_delete(list);
        list = NULL;
    }
}

int32_t shim_list_choice(void) { return choice; }

void shim_key(uint32_t k)
{
    if(keyq_head - keyq_tail < KEYQ) keyq[keyq_head++ % KEYQ] = k;
}

uint32_t shim_handle(void) { return lv_timer_handler(); }

void shim_refresh_now(void) { lv_refr_now(disp); }
