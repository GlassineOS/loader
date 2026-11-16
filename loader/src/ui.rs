// SPDX-License-Identifier: GPL-2.0-or-later
//! The key line, the list and messages, drawn with LVGL (through `c/shim.c`) over the
//! picture on the first display. Only scalars and pointers cross to C. LVGL keeps the
//! picture as its background, so taking a line or the list off redraws the picture
//! under it pixel for pixel (011 FR-016a).
use alloc::boxed::Box;
use alloc::ffi::CString;
use alloc::vec::Vec;
use core::ffi::{c_char, c_void};
use core::time::Duration;
use uefi::proto::console::text::{Key, ScanCode};

use crate::display::Display;

type FlushFn = extern "C" fn(i32, i32, i32, i32, *const u8, u32);

unsafe extern "C" {
    fn shim_init(w: u32, h: u32, flush: FlushFn, tick: extern "C" fn() -> u32, log: extern "C" fn(*const c_char), buf: *mut c_void, bytes: u32);
    fn shim_set_background(px: *const u8, w: u32, h: u32);
    fn shim_show_hint(text: *const c_char);
    fn shim_hide_hint();
    fn shim_show_list(title: *const c_char, items: *const *const c_char, n: i32);
    fn shim_hide_list();
    fn shim_list_choice() -> i32;
    fn shim_show_message(text: *const c_char);
    fn shim_key(k: u32);
    fn shim_handle() -> u32;
    fn shim_refresh_now();
}

// LVGL's lv_key_t values.
const LV_KEY_ENTER: u32 = 10;
const LV_KEY_NEXT: u32 = 9;
const LV_KEY_PREV: u32 = 11;

static mut DISPLAY: Option<*mut dyn Display> = None;
static mut STARTED: bool = false;

extern "C" fn flush(x: i32, y: i32, w: i32, h: i32, px: *const u8, stride: u32) {
    // SAFETY: LVGL calls this only from shim calls made while `DISPLAY` is set.
    unsafe {
        let Some(d) = DISPLAY else { return };
        let stride_px = (stride / 4) as usize;
        let buf = core::slice::from_raw_parts(px as *const u32, stride_px * h as usize);
        (*d).draw(x as u32, y as u32, w as u32, h as u32, buf, stride_px);
    }
}

extern "C" fn tick() -> u32 {
    crate::arch::now_ms() as u32
}

extern "C" fn lv_log(line: *const c_char) {
    let s = unsafe { core::ffi::CStr::from_ptr(line) };
    crate::log!("lvgl: {}", s.to_str().unwrap_or("?").trim_end());
}

/// LVGL's TLSF pool comes from boot-services pool memory.
#[unsafe(no_mangle)]
pub extern "C" fn glassine_pool_alloc(size: usize) -> *mut c_void {
    match uefi::boot::allocate_pool(uefi::boot::MemoryType::LOADER_DATA, size) {
        Ok(p) => p.as_ptr().cast(),
        Err(_) => core::ptr::null_mut(),
    }
}

/// Starts LVGL on `d` with `picture` (the whole screen, `0x00RRGGBB`) as what it draws
/// over. `d` must outlive every later call of this module.
pub fn start(d: &mut (dyn Display + 'static), picture: Vec<u32>) {
    let (w, h) = d.size();
    unsafe {
        DISPLAY = Some(d as *mut dyn Display);
        let pic = Box::leak(picture.into_boxed_slice());
        if !STARTED {
            STARTED = true;
            let draw_bytes = (w as usize) * 64 * 4;
            let draw_buf = Box::leak(alloc::vec![0u8; draw_bytes].into_boxed_slice());
            shim_init(w, h, flush, tick, lv_log, draw_buf.as_mut_ptr().cast(), draw_bytes as u32);
        }
        shim_set_background(pic.as_ptr() as *const u8, w, h);
    }
}

pub fn started() -> bool {
    unsafe { STARTED }
}

fn c(s: &str) -> CString {
    CString::new(s.replace('\0', " ")).unwrap()
}

pub fn show_hint(text: &str) {
    let t = c(text);
    unsafe {
        shim_show_hint(t.as_ptr());
        shim_refresh_now();
    }
}

/// Takes the line off: the picture under it comes back.
pub fn hide_hint() {
    unsafe {
        shim_hide_hint();
        shim_refresh_now();
    }
}

/// Lets LVGL run its timers.
pub fn handle() {
    unsafe {
        shim_handle();
    }
}

pub fn read_key() -> Option<Key> {
    uefi::system::with_stdin(|i| i.read_key().ok().flatten())
}

/// Shows a list and waits for a pick. `Some(i)`: entry `i`; `None`: Escape.
/// Takes the list off again before it returns.
pub fn list(title: &str, items: &[&str]) -> Option<usize> {
    let ct = c(title);
    let cs: Vec<CString> = items.iter().map(|s| c(s)).collect();
    let ptrs: Vec<*const c_char> = cs.iter().map(|s| s.as_ptr()).collect();
    unsafe {
        shim_show_list(ct.as_ptr(), ptrs.as_ptr(), ptrs.len() as i32);
        shim_refresh_now();
    }
    crate::log!("LIST_SHOWN {} ({} entries)", title, items.len());
    let r = loop {
        match read_key() {
            Some(Key::Special(ScanCode::UP)) => unsafe { shim_key(LV_KEY_PREV) },
            Some(Key::Special(ScanCode::DOWN)) => unsafe { shim_key(LV_KEY_NEXT) },
            Some(Key::Special(ScanCode::ESCAPE)) => break None,
            Some(Key::Printable(ch)) if u16::from(ch) == 13 || u16::from(ch) == 10 => unsafe { shim_key(LV_KEY_ENTER) },
            _ => {}
        }
        unsafe { shim_handle() };
        let i = unsafe { shim_list_choice() };
        if i >= 0 {
            break Some(i as usize);
        }
        uefi::boot::stall(Duration::from_millis(5));
    };
    unsafe {
        shim_hide_list();
        shim_refresh_now();
    }
    r
}

/// A message over the picture, left on screen.
pub fn message(text: &str) {
    let t = c(text);
    unsafe {
        shim_show_message(t.as_ptr());
        shim_refresh_now();
    }
}
