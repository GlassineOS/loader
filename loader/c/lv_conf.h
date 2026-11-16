/* LVGL v9.6.0 configuration for the GlassineOS loader (UEFI, freestanding).
 * Every option not named here takes LVGL's default from lv_conf_internal.h;
 * widgets and libraries default to on there, so the unused ones are listed as 0. */
#ifndef LV_CONF_H
#define LV_CONF_H

/* Memory: LVGL's own TLSF heap, its pool taken from UEFI boot-services pool memory. */
#define LV_USE_STDLIB_MALLOC   LV_STDLIB_BUILTIN
#define LV_USE_STDLIB_STRING   LV_STDLIB_BUILTIN
#define LV_USE_STDLIB_SPRINTF  LV_STDLIB_BUILTIN
#define LV_MEM_SIZE            (4u * 1024u * 1024u)
#define LV_MEM_POOL_INCLUDE    "shim_pool.h"
#define LV_MEM_POOL_ALLOC      glassine_pool_alloc

#define LV_USE_OS              LV_OS_NONE
#define LV_USE_FLOAT           0
#define LV_USE_MATRIX          0
#define LV_COLOR_DEPTH         32
#define LV_DEF_REFR_PERIOD     16
#define LV_DPI_DEF             130

/* Software renderer, only the formats the loader uses. */
#define LV_USE_DRAW_SW                     1
#define LV_DRAW_SW_SUPPORT_RGB565          0
#define LV_DRAW_SW_SUPPORT_RGB565_SWAPPED  0
#define LV_DRAW_SW_SUPPORT_RGB565A8        0
#define LV_DRAW_SW_SUPPORT_RGB888          0
#define LV_DRAW_SW_SUPPORT_XRGB8888        1
#define LV_DRAW_SW_SUPPORT_ARGB8888        1
#define LV_DRAW_SW_SUPPORT_ARGB8888_PREMULTIPLIED 0
#define LV_DRAW_SW_SUPPORT_L8              0
#define LV_DRAW_SW_SUPPORT_AL88            0
#define LV_DRAW_SW_SUPPORT_A8              1
#define LV_DRAW_SW_SUPPORT_I1              0
#define LV_USE_DRAW_SW_ASM                 LV_DRAW_SW_ASM_NONE
#define LV_USE_DRAW_SW_COMPLEX_GRADIENTS   0
#define LV_USE_THORVG_INTERNAL             0
#define LV_USE_VECTOR_GRAPHIC              0

/* Logging goes to the Rust serial writer (never to ConOut, which is on screen). */
#define LV_USE_LOG             1
#define LV_LOG_LEVEL           LV_LOG_LEVEL_WARN
#define LV_LOG_PRINTF          0
#define LV_USE_ASSERT_NULL          1
#define LV_USE_ASSERT_MALLOC        1
#define LV_USE_ASSERT_STYLE         0
#define LV_USE_ASSERT_MEM_INTEGRITY 0
#define LV_USE_ASSERT_OBJ           0
#define LV_USE_SYSMON          0
#define LV_USE_PROFILER        0
#define LV_USE_OBSERVER        0
#define LV_USE_OBJ_ID          0
#define LV_USE_OBJ_NAME        0
#define LV_USE_OBJ_PROPERTY    0
#define LV_USE_TEST            0
#define LV_USE_TRANSLATION     0

/* Fonts */
#define LV_FONT_MONTSERRAT_14  0
#define LV_FONT_MONTSERRAT_20  1
/* The start menu on an e-ink panel (spec 004): readable at about 230 dpi. */
#define LV_FONT_MONTSERRAT_40  1
#define LV_FONT_DEFAULT        LV_FONT_DEFAULT_MONTSERRAT_20
#define LV_USE_FONT_PLACEHOLDER 1
#define LV_USE_FONT_COMPRESSED 0

/* Widgets: label, image, button, list (list needs button + label). */
#define LV_USE_LABEL           1
#define LV_LABEL_TEXT_SELECTION 0
#define LV_LABEL_LONG_TXT_HINT 0
#define LV_USE_IMAGE           1
#define LV_USE_BUTTON          1
#define LV_USE_LIST            1
#define LV_USE_ANIMIMG         0
#define LV_USE_ARC             0
#define LV_USE_ARCLABEL        0
#define LV_USE_BAR             0
#define LV_USE_BUTTONMATRIX    0
#define LV_USE_CALENDAR        0
#define LV_USE_CANVAS          0
#define LV_USE_CHART           0
#define LV_USE_CHECKBOX        0
#define LV_USE_DROPDOWN        0
#define LV_USE_IMAGEBUTTON     0
#define LV_USE_KEYBOARD        0
#define LV_USE_LED             0
#define LV_USE_LINE            0
#define LV_USE_MENU            0
#define LV_USE_MSGBOX          0
#define LV_USE_ROLLER          0
#define LV_USE_SCALE           0
#define LV_USE_SLIDER          0
#define LV_USE_SPAN            0
#define LV_USE_SPINBOX         0
#define LV_USE_SPINNER         0
#define LV_USE_SWITCH          0
#define LV_USE_TEXTAREA        0
#define LV_USE_TABLE           0
#define LV_USE_TABVIEW         0
#define LV_USE_TILEVIEW        0
#define LV_USE_WIN             0
#define LV_USE_3DTEXTURE       0
#define LV_USE_LOTTIE          0
#define LV_USE_GLTF            0

#define LV_USE_THEME_DEFAULT   1
#define LV_THEME_DEFAULT_DARK  1
#define LV_THEME_DEFAULT_GROW  0
#define LV_THEME_DEFAULT_TRANSITION_TIME 0
#define LV_USE_THEME_SIMPLE    0
#define LV_USE_THEME_MONO      0
#define LV_USE_FLEX            1
#define LV_USE_GRID            0

/* No file systems, decoders or third-party libraries: the loader reads files itself. */
#define LV_USE_FS_STDIO        0
#define LV_USE_FS_POSIX        0
#define LV_USE_FS_WIN32        0
#define LV_USE_FS_FATFS        0
#define LV_USE_FS_MEMFS        0
#define LV_USE_LODEPNG         0
#define LV_USE_LIBPNG          0
#define LV_USE_BMP             0
#define LV_USE_TJPGD           0
#define LV_USE_LIBJPEG_TURBO   0
#define LV_USE_GIF             0
#define LV_USE_QRCODE          0
#define LV_USE_BARCODE         0
#define LV_USE_FREETYPE        0
#define LV_USE_TINY_TTF        0
#define LV_USE_RLOTTIE         0
#define LV_USE_FFMPEG          0
#define LV_USE_LZ4_INTERNAL    0
#define LV_USE_SVG             0
#define LV_USE_SNAPSHOT        0
#define LV_USE_MONKEY          0
#define LV_USE_GRIDNAV         0
#define LV_USE_FRAGMENT        0
#define LV_USE_IMGFONT         0
#define LV_USE_IME_PINYIN      0
#define LV_USE_FILE_EXPLORER   0
#define LV_USE_FONT_MANAGER    0
#define LV_USE_XML             0
#define LV_USE_COLOR_FILTER    0

#define LV_BUILD_EXAMPLES      0
#define LV_BUILD_DEMOS         0

#endif /* LV_CONF_H */
