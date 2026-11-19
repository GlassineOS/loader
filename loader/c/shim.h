/* The C side of the loader's LVGL use. Rust calls only these functions and passes
 * only scalars and pointers; it never mirrors an LVGL struct. */
#ifndef GLASSINE_SHIM_H
#define GLASSINE_SHIM_H
#include <stdint.h>

typedef void (*shim_flush_fn)(int32_t x, int32_t y, int32_t w, int32_t h,
                              const uint8_t *px, uint32_t stride);
typedef uint32_t (*shim_tick_fn)(void);
typedef void (*shim_log_fn)(const char *line);

void shim_init(uint32_t w, uint32_t h, shim_flush_fn flush, shim_tick_fn tick,
               shim_log_fn log, void *draw_buf, uint32_t draw_buf_bytes);
/* Full-screen XRGB8888 picture, w*h pixels, stride w*4. */
void shim_set_background(const uint8_t *px, uint32_t w, uint32_t h);
void shim_show_hint(const char *text);
void shim_hide_hint(void);
void shim_show_list(const char *title, const char *const *items, int32_t n);
void shim_hide_list(void);
int32_t shim_list_choice(void);
/* A centred block of text over the picture (a failure the person must see). */
void shim_show_message(const char *text);
/* LV_KEY_* codes; each call is one press + release. */
void shim_key(uint32_t lv_key);
uint32_t shim_handle(void);
void shim_refresh_now(void);

/* The start menu on a firmware-refreshed panel (loader spec 004), as the boards in
 * GlassineOS/ui designs/startmenu draw it: the mark and the title, a reason line, a card
 * per entry (an icon, the name, one line), the default card with a heavier keyline and
 * an offset, a foot line. No pressed style, no scrolling, no theme: a tap turns only its
 * card black. Picks go to shim_list_choice() like the list's. */
typedef void (*shim_pointer_fn)(int32_t *x, int32_t *y, int32_t *pressed);
/* A pointer input that calls `read` for its state; created once. */
void shim_epd_pointer(shim_pointer_fn read);
/* The cards' icons. */
enum { SHIM_ICON_MARK, SHIM_ICON_PREVIOUS, SHIM_ICON_MORE, SHIM_ICON_KERNEL, SHIM_ICON_OTHER,
       SHIM_ICON_RECOVERY, SHIM_ICON_DEPLOYMENT, SHIM_ICON_BACK };
/* n cards (six at most are drawn), `dflt` the default's index or -1. */
void shim_epd_menu(const char *title, const char *reason, const char *const *names, const char *const *lines,
                   const int32_t *icons, int32_t n, int32_t dflt, const char *foot);
/* Card i drawn as picked (the default when the minute is up). */
void shim_epd_pick(int32_t i);
/* No taps any more: every card but the default drawn as one that can't be picked, the
 * default's line and the foot replaced. */
void shim_epd_notouch(const char *default_line, const char *foot);
#endif
