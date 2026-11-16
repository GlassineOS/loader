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

/* The start menu on a firmware-refreshed panel (loader spec 004): white rows with black
 * text across the screen, the focused row inverted, no pressed style, no scrolling, no
 * theme. Picks go to shim_list_choice() like the list's. */
typedef void (*shim_pointer_fn)(int32_t *x, int32_t *y, int32_t *pressed);
/* A pointer input that calls `read` for its state; created once. */
void shim_epd_pointer(shim_pointer_fn read);
void shim_epd_menu(const char *title, const char *const *items, int32_t n, int32_t row_h,
                   const char *foot);
#endif
