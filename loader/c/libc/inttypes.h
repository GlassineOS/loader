/* Freestanding inttypes.h: clang ships only a wrapper that includes the C library's.
 * LVGL needs the PRI* macros; the target is LLP64 (int32_t = int, int64_t = long long). */
#ifndef GLASSINE_INTTYPES_H
#define GLASSINE_INTTYPES_H
#include <stdint.h>
#define PRId8 "d"
#define PRIu8 "u"
#define PRId16 "d"
#define PRIu16 "u"
#define PRId32 "d"
#define PRIi32 "i"
#define PRIu32 "u"
#define PRIx32 "x"
#define PRIX32 "X"
#define PRId64 "lld"
#define PRIi64 "lli"
#define PRIu64 "llu"
#define PRIx64 "llx"
#define PRIX64 "llX"
#define PRIdPTR "lld"
#define PRIuPTR "llu"
#define PRIxPTR "llx"
#endif
