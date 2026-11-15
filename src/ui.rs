//! FFI to c/shim.c. Only scalars and pointers cross; no LVGL struct is mirrored here.
use core::ffi::{c_char, c_void};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput};

pub type FlushFn = extern "C" fn(i32, i32, i32, i32, *const u8, u32);

unsafe extern "C" {
    pub fn shim_init(w: u32, h: u32, flush: FlushFn, tick: extern "C" fn() -> u32,
                     log: extern "C" fn(*const c_char), buf: *mut c_void, bytes: u32);
    pub fn shim_set_background(px: *const u8, w: u32, h: u32);
    pub fn shim_show_hint(text: *const c_char);
    pub fn shim_hide_hint();
    pub fn shim_show_list(title: *const c_char, items: *const *const c_char, n: i32);
    pub fn shim_hide_list();
    pub fn shim_list_choice() -> i32;
    pub fn shim_key(k: u32);
    pub fn shim_handle() -> u32;
    pub fn shim_refresh_now();
}

// LVGL's lv_key_t values.
pub const LV_KEY_ENTER: u32 = 10;
pub const LV_KEY_NEXT: u32 = 9;
pub const LV_KEY_PREV: u32 = 11;

pub static mut GOP: *mut GraphicsOutput = core::ptr::null_mut();

/// LVGL's XRGB8888 is GOP's BltPixel byte for byte (B, G, R, reserved), so the flush is
/// one Blt with no conversion; the firmware converts to the framebuffer's own layout.
pub extern "C" fn flush(x: i32, y: i32, w: i32, h: i32, px: *const u8, stride: u32) {
    let gop = unsafe { &mut *GOP };
    let px_stride = (stride / 4) as usize;
    let buf = unsafe { core::slice::from_raw_parts(px as *const BltPixel, px_stride * h as usize) };
    let _ = gop.blt(BltOp::BufferToVideo {
        buffer: buf,
        src: BltRegion::SubRectangle { coords: (0, 0), px_stride },
        dest: (x as usize, y as usize),
        dims: (w as usize, h as usize),
    });
}

pub extern "C" fn log(line: *const c_char) {
    let s = unsafe { core::ffi::CStr::from_ptr(line) };
    crate::log!("lvgl: {}", s.to_str().unwrap_or("?").trim_end());
}

#[unsafe(no_mangle)]
pub extern "C" fn glassine_pool_alloc(size: usize) -> *mut c_void {
    match uefi::boot::allocate_pool(uefi::boot::MemoryType::LOADER_DATA, size) {
        Ok(p) => p.as_ptr().cast(),
        Err(_) => core::ptr::null_mut(),
    }
}

pub fn set_gop(g: &mut GraphicsOutput) {
    unsafe { GOP = g as *mut _ };
}
