#include "shim.h"
#include "lvgl.h"

static lv_display_t *disp;
static lv_indev_t *kbd;
static lv_group_t *group;
static lv_obj_t *bg_img;
static lv_obj_t *hint;
static lv_obj_t *list;
static lv_obj_t *message;
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

void shim_show_message(const char *text)
{
    shim_hide_hint();
    if(!message) {
        message = lv_label_create(lv_screen_active());
        lv_obj_set_width(message, 640);
        lv_label_set_long_mode(message, LV_LABEL_LONG_MODE_WRAP);
        lv_obj_set_style_text_color(message, lv_color_white(), 0);
        lv_obj_set_style_bg_color(message, lv_color_black(), 0);
        lv_obj_set_style_bg_opa(message, LV_OPA_COVER, 0);
        lv_obj_set_style_pad_all(message, 20, 0);
        lv_obj_set_style_radius(message, 6, 0);
    }
    lv_label_set_text(message, text);
    lv_obj_center(message);
}

void shim_key(uint32_t k)
{
    if(keyq_head - keyq_tail < KEYQ) keyq[keyq_head++ % KEYQ] = k;
}

uint32_t shim_handle(void) { return lv_timer_handler(); }

void shim_refresh_now(void) { lv_refr_now(disp); }

/* ---- the start menu on a firmware-refreshed panel ------------------------------------ */

static lv_indev_t *pointer;
static shim_pointer_fn pointer_fn;
static void pointer_read(lv_indev_t *in, lv_indev_data_t *data)
{
    (void)in;
    int32_t x = 0, y = 0, pressed = 0;
    if(pointer_fn) pointer_fn(&x, &y, &pressed);
    data->point.x = x;
    data->point.y = y;
    data->state = pressed ? LV_INDEV_STATE_PRESSED : LV_INDEV_STATE_RELEASED;
}

void shim_epd_pointer(shim_pointer_fn read)
{
    pointer_fn = read;
    if(pointer) return;
    pointer = lv_indev_create();
    lv_indev_set_type(pointer, LV_INDEV_TYPE_POINTER);
    lv_indev_set_read_cb(pointer, pointer_read);
    /* A tap only: no scrolling, no long press. */
    lv_indev_set_scroll_limit(pointer, 10000);
    lv_indev_set_long_press_time(pointer, 60000);
}

/* The look follows the boards in GlassineOS/ui designs/startmenu (one board pixel is one
 * panel pixel): IBM Plex Sans in two styles (converted, see c/fonts/SOURCES), black and
 * white with two flat greys, a card per entry. */
#include "menu_art.h"
LV_FONT_DECLARE(glassine_sans_semibold_56)
LV_FONT_DECLARE(glassine_sans_regular_40)

#define INK      0x111111
#define INK_SOFT 0x444444
#define PAPER    0xffffff
#define SHADE    0xbbbbbb /* the default card's offset, and dim */
#define LEAF     0xcccccc /* the mark's front sheet (highlight) */
#define SHEET    0xeeeeee /* a card that can't be picked */

#define PAD       72
#define MARK_Y    96
#define REASON_Y  256
#define CARDS_Y   400
#define CARD_H    176
#define CARD_GAP  24
#define CARD_W    (1404 - 2 * PAD)
#define BORDER    4
#define BORDER_D  6
#define RADIUS    24
#define CAP       10
#define ICON      96
#define ICON_X    40
#define TEXT_X    176
#define NAME_LH   64
#define LINE_LH   50
#define FOOT_BOTTOM 72
#define FOOT_ICON 56
#define CARDS_MAX 6

enum card_look { LOOK_NORMAL, LOOK_PICKED, LOOK_OFF };

struct card {
    lv_obj_t *obj, *name, *line, *icon, *leaf, *outline, *pen;
    int dflt;
};
static struct card cards[CARDS_MAX];
static int32_t ncards;
static lv_obj_t *foot_label, *foot_icon, *foot_rule;

static const lv_image_dsc_t *const icon_art[] = {
    NULL, &menu_art_previous, &menu_art_more, &menu_art_kernel, &menu_art_other,
    &menu_art_recovery, &menu_art_deployment, &menu_art_back,
};

/* The foot's dotted rule: 4-pixel dots 8 apart, tiled. */
static const uint8_t dots_px[16] = {
    0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00,
};
static const lv_image_dsc_t dots = {
    .header = { .magic = LV_IMAGE_HEADER_MAGIC, .cf = LV_COLOR_FORMAT_A4, .w = 8, .h = 4, .stride = 4 },
    .data_size = sizeof(dots_px),
    .data = dots_px,
};

static lv_obj_t *plain(lv_obj_t *parent)
{
    lv_obj_t *o = lv_obj_create(parent);
    lv_obj_remove_style_all(o);
    lv_obj_remove_flag(o, LV_OBJ_FLAG_SCROLLABLE | LV_OBJ_FLAG_CLICKABLE);
    return o;
}

static lv_obj_t *mask(lv_obj_t *parent, const lv_image_dsc_t *art, uint32_t colour, int32_t x, int32_t y)
{
    lv_obj_t *m = lv_image_create(parent);
    lv_obj_remove_style_all(m);
    lv_obj_remove_flag(m, LV_OBJ_FLAG_CLICKABLE);
    lv_image_set_src(m, art);
    lv_obj_set_style_image_recolor(m, lv_color_hex(colour), 0);
    lv_obj_set_style_image_recolor_opa(m, LV_OPA_COVER, 0);
    lv_obj_set_pos(m, x, y);
    return m;
}

static lv_obj_t *text(lv_obj_t *parent, const char *s, const lv_font_t *font, int32_t lh, uint32_t colour,
                      int32_t x, int32_t y, int32_t w, lv_label_long_mode_t mode)
{
    lv_obj_t *l = lv_label_create(parent);
    lv_obj_remove_style_all(l);
    lv_obj_set_style_text_font(l, font, 0);
    lv_obj_set_style_text_color(l, lv_color_hex(colour), 0);
    /* The boards' line height: the font's, spread to `lh`, the first line centred in it. */
    lv_obj_set_style_text_line_space(l, lh - font->line_height, 0);
    lv_obj_set_width(l, w);
    lv_label_set_long_mode(l, mode);
    lv_label_set_text(l, s);
    lv_obj_set_pos(l, x, y + (lh - font->line_height) / 2);
    return l;
}

/* The mark: its front sheet in the leaf grey, the outline, the pen stroke. */
static void mark(lv_obj_t *parent, int32_t x, int32_t y, int head, struct card *c)
{
    lv_obj_t *leaf = mask(parent, head ? &menu_art_mark_head_leaf : &menu_art_mark_card_leaf, LEAF, x, y);
    lv_obj_t *outline = mask(parent, head ? &menu_art_mark_head_outline : &menu_art_mark_card_outline, INK, x, y);
    lv_obj_t *pen = mask(parent, head ? &menu_art_mark_head_pen : &menu_art_mark_card_pen, INK, x, y);
    if(c) {
        c->leaf = leaf;
        c->outline = outline;
        c->pen = pen;
    }
}

static void card_look(struct card *c, enum card_look look)
{
    uint32_t bg = PAPER, edge = INK, name = INK, line = INK_SOFT, icon = INK;
    if(look == LOOK_PICKED) {
        bg = INK;
        name = line = icon = PAPER;
    }
    else if(look == LOOK_OFF) {
        bg = SHEET;
        edge = icon = SHADE;
        name = line = INK_SOFT;
    }
    lv_obj_set_style_bg_color(c->obj, lv_color_hex(bg), 0);
    lv_obj_set_style_border_color(c->obj, lv_color_hex(edge), 0);
    lv_obj_set_style_text_color(c->name, lv_color_hex(name), 0);
    lv_obj_set_style_text_color(c->line, lv_color_hex(line), 0);
    if(c->icon) lv_obj_set_style_image_recolor(c->icon, lv_color_hex(icon), 0);
    /* On a picked card the mark's outline turns white; its sheet and pen stay. */
    if(c->outline) lv_obj_set_style_image_recolor(c->outline, lv_color_hex(look == LOOK_PICKED ? PAPER : INK), 0);
}

static void card_clicked(lv_event_t *e)
{
    int32_t i = (int32_t)(intptr_t)lv_event_get_user_data(e);
    if(choice >= 0) return;
    card_look(&cards[i], LOOK_PICKED);
    choice = i;
}

static void place_foot(const char *foot)
{
    lv_label_set_text(foot_label, foot);
    lv_obj_update_layout(foot_label);
    int32_t h = lv_display_get_vertical_resolution(disp);
    int32_t space = lv_obj_get_style_text_line_space(foot_label, 0);
    /* The block of lines, each LINE_LH high, ends FOOT_BOTTOM above the panel's foot. */
    int32_t ty = h - FOOT_BOTTOM - (lv_obj_get_height(foot_label) + space);
    lv_obj_set_y(foot_label, ty + space / 2);
    lv_obj_set_y(foot_icon, ty - 3);
    lv_obj_set_y(foot_rule, ty - 40 - 4);
}

void shim_epd_menu(const char *title, const char *reason, const char *const *names, const char *const *lines,
                   const int32_t *icons, int32_t n, int32_t dflt, const char *foot)
{
    shim_hide_hint();
    shim_hide_list();
    choice = -1;
    int32_t w = lv_display_get_horizontal_resolution(disp);
    int32_t h = lv_display_get_vertical_resolution(disp);
    list = plain(lv_screen_active());
    lv_obj_set_size(list, w, h);
    lv_obj_set_pos(list, 0, 0);
    lv_obj_set_style_bg_color(list, lv_color_hex(PAPER), 0);
    lv_obj_set_style_bg_opa(list, LV_OPA_COVER, 0);

    mark(list, PAD, MARK_Y, 1, NULL);
    const lv_font_t *big = &glassine_sans_semibold_56, *small = &glassine_sans_regular_40;
    int32_t mh = menu_art_mark_head_leaf.header.h;
    text(list, title, big, 68, INK, PAD + mh + 40, MARK_Y + (mh - 68) / 2, w - 2 * PAD - mh - 40, LV_LABEL_LONG_MODE_DOTS);
    lv_obj_t *r = text(list, reason, small, LINE_LH, INK_SOFT, PAD, REASON_Y, w - 2 * PAD, LV_LABEL_LONG_MODE_WRAP);
    lv_obj_set_style_max_height(r, 2 * LINE_LH, 0);

    ncards = n < CARDS_MAX ? n : CARDS_MAX;
    for(int32_t i = 0; i < ncards; i++) {
        struct card *c = &cards[i];
        lv_memzero(c, sizeof(*c));
        c->dflt = i == dflt;
        int32_t y = CARDS_Y + i * (CARD_H + CARD_GAP);
        int32_t b = c->dflt ? BORDER_D : BORDER;
        if(c->dflt) {
            /* White's flat keycap offset, under the card. */
            lv_obj_t *cap = plain(list);
            lv_obj_set_size(cap, CARD_W, CARD_H);
            lv_obj_set_pos(cap, PAD + CAP, y + CAP);
            lv_obj_set_style_radius(cap, RADIUS, 0);
            lv_obj_set_style_bg_color(cap, lv_color_hex(SHADE), 0);
            lv_obj_set_style_bg_opa(cap, LV_OPA_COVER, 0);
        }
        c->obj = lv_button_create(list);
        lv_obj_remove_style_all(c->obj);
        lv_obj_remove_flag(c->obj, LV_OBJ_FLAG_SCROLLABLE | LV_OBJ_FLAG_SCROLL_ON_FOCUS);
        lv_obj_set_size(c->obj, CARD_W, CARD_H);
        lv_obj_set_pos(c->obj, PAD, y);
        lv_obj_set_style_radius(c->obj, RADIUS, 0);
        lv_obj_set_style_bg_opa(c->obj, LV_OPA_COVER, 0);
        lv_obj_set_style_border_width(c->obj, b, 0);
        lv_obj_set_style_border_opa(c->obj, LV_OPA_COVER, 0);
        int32_t inner = CARD_H - 2 * b;
        if(icons[i] == 0) mark(c->obj, ICON_X, (inner - ICON) / 2, 0, c);
        else if(icons[i] > 0 && icons[i] < (int32_t)(sizeof(icon_art) / sizeof(icon_art[0])))
            c->icon = mask(c->obj, icon_art[icons[i]], INK, ICON_X, (inner - ICON) / 2);
        int32_t ty = (inner - NAME_LH - LINE_LH) / 2, tw = CARD_W - 2 * b - TEXT_X - 32;
        c->name = text(c->obj, names[i], big, NAME_LH, INK, TEXT_X, ty, tw, LV_LABEL_LONG_MODE_DOTS);
        c->line = text(c->obj, lines[i], small, LINE_LH, INK_SOFT, TEXT_X, ty + NAME_LH, tw, LV_LABEL_LONG_MODE_DOTS);
        card_look(c, LOOK_NORMAL);
        lv_obj_add_event_cb(c->obj, card_clicked, LV_EVENT_CLICKED, (void *)(intptr_t)i);
    }

    foot_rule = lv_image_create(list);
    lv_obj_remove_style_all(foot_rule);
    lv_image_set_src(foot_rule, &dots);
    lv_image_set_inner_align(foot_rule, LV_IMAGE_ALIGN_TILE);
    lv_obj_set_size(foot_rule, w - 2 * PAD, 4);
    lv_obj_set_style_image_recolor(foot_rule, lv_color_hex(INK), 0);
    lv_obj_set_style_image_recolor_opa(foot_rule, LV_OPA_COVER, 0);
    lv_obj_set_x(foot_rule, PAD);
    foot_icon = mask(list, &menu_art_clock, INK, PAD, 0);
    foot_label = text(list, "", small, LINE_LH, INK, PAD + FOOT_ICON + 24, 0, w - 2 * PAD - FOOT_ICON - 24,
                      LV_LABEL_LONG_MODE_WRAP);
    place_foot(foot);
    lv_obj_invalidate(lv_screen_active());
}

void shim_epd_pick(int32_t i)
{
    if(i >= 0 && i < ncards && choice < 0) card_look(&cards[i], LOOK_PICKED);
}

void shim_epd_notouch(const char *default_line, const char *foot)
{
    for(int32_t i = 0; i < ncards; i++) {
        if(cards[i].dflt) lv_label_set_text(cards[i].line, default_line);
        else {
            card_look(&cards[i], LOOK_OFF);
            lv_obj_add_state(cards[i].obj, LV_STATE_DISABLED);
        }
    }
    place_foot(foot);
}
