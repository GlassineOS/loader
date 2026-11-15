/* LVGL's TLSF pool comes from the Rust side (UEFI boot-services pool memory). */
#ifndef SHIM_POOL_H
#define SHIM_POOL_H
#include <stddef.h>
void *glassine_pool_alloc(size_t size);
#endif
