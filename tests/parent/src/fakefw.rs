// SPDX-License-Identifier: GPL-2.0-or-later
//! A stand-in for firmware with an extended graphics output (`parent.fakefw=1` in
//! `\parent.cfg`), installed by the parent before it starts the loader, from the
//! interface description only:
//!
//! - an extended GOP: six words, QueryMode, SetMode, Blt, a refresh
//!   `Refresh(This, fb, x, y, w, h, stride, mode)` at +0x18, `GetFrameBuffer(This, &fb,
//!   &w, &h)` at +0x20, Mode at +0x28; a 1872x1404 mode, its framebuffer packed RGB565;
//!   device path Vendor(HW, the GOP GUID)/End;
//! - a console splitter's GOP: the same six words with NULL at +0x18 and +0x20, no device
//!   path;
//! - three GOPs a standard reader must not trust: NULL at +0x18, a mapped Mode with a
//!   MaxMode of 0x77770000, an unmapped Mode pointer.
//!
//! Every function counts its calls. After the loader returns, [`report`] prints the counts
//! and each refresh: its rectangle and the CRC-32 of the framebuffer's packed RGB565
//! rows at the time of the call.
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::SeqCst};
use glassine_loader_core::gspl::crc32;
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::proto::device_path::DevicePath;
use uefi::{Guid, Identify, Status, guid};

pub const GOP_GUID: Guid = guid!("9042a9de-23dc-4a38-96fb-7aded080516a");
pub const W: u32 = 1872;
pub const H: u32 = 1404;
const INFO_SIZE: usize = 0x24;

static QUERY: AtomicU32 = AtomicU32::new(0);
static SET_MODE: AtomicU32 = AtomicU32::new(0);
static BLT: AtomicU32 = AtomicU32::new(0);
static GET_FB: AtomicU32 = AtomicU32::new(0);
static GARBAGE_QUERY: AtomicU32 = AtomicU32::new(0);
static EXT: AtomicU64 = AtomicU64::new(0);
static FB: AtomicU64 = AtomicU64::new(0);
/// The refresh calls: `[this_ok, fb, x, y, w, h, stride, mode, crc]`.
static mut REFRESHES: Vec<[u64; 9]> = Vec::new();

#[repr(C)]
struct Info {
    version: u32,
    w: u32,
    h: u32,
    format: u32,
    masks: [u32; 4],
    stride: u32,
}

#[repr(C)]
struct Mode {
    max_mode: u32,
    mode: u32,
    info: *const Info,
    size_of_info: usize,
    fb_base: u64,
    fb_size: usize,
}

fn info() -> Info {
    Info { version: 0, w: W, h: H, format: 1, masks: [0; 4], stride: W }
}

unsafe extern "efiapi" fn query_mode(_this: u64, n: u32, size: *mut usize, out: *mut *mut Info) -> Status {
    QUERY.fetch_add(1, SeqCst);
    if n != 0 {
        return Status::INVALID_PARAMETER;
    }
    let Ok(p) = boot::allocate_pool(MemoryType::BOOT_SERVICES_DATA, INFO_SIZE) else { return Status::OUT_OF_RESOURCES };
    unsafe {
        core::ptr::write(p.as_ptr() as *mut Info, info());
        *size = INFO_SIZE;
        *out = p.as_ptr() as *mut Info;
    }
    Status::SUCCESS
}

unsafe extern "efiapi" fn garbage_query_mode(_this: u64, _n: u32, _size: *mut usize, _out: *mut *mut Info) -> Status {
    GARBAGE_QUERY.fetch_add(1, SeqCst);
    Status::DEVICE_ERROR
}

unsafe extern "efiapi" fn set_mode(_this: u64, _n: u32) -> Status {
    SET_MODE.fetch_add(1, SeqCst);
    Status::UNSUPPORTED
}

unsafe extern "efiapi" fn blt(_this: u64, _b: u64, _op: u64, _sx: u64, _sy: u64, _dx: u64, _dy: u64, _w: u64, _h: u64, _d: u64) -> Status {
    BLT.fetch_add(1, SeqCst);
    Status::UNSUPPORTED
}

/// The refresh: always 0, as the interface says.
unsafe extern "efiapi" fn refresh(this: u64, fb: u64, x: u64, y: u64, w: u64, h: u64, stride: u64, mode: u64) -> Status {
    let base = FB.load(SeqCst);
    // The rows the call names, packed RGB565 from the framebuffer's start.
    let rows = (stride.min(W as u64) * (y + h).min(H as u64) * 2) as usize;
    let crc = crc32(unsafe { core::slice::from_raw_parts(base as *const u8, rows) });
    let call = [(this == EXT.load(SeqCst)) as u64, (fb == base) as u64, x, y, w, h, stride, mode, crc as u64];
    unsafe { (*core::ptr::addr_of_mut!(REFRESHES)).push(call) };
    Status::SUCCESS
}

unsafe extern "efiapi" fn get_frame_buffer(this: u64, fb: *mut u64, w: *mut u32, h: *mut u32) -> Status {
    GET_FB.fetch_add(1, SeqCst);
    if this != EXT.load(SeqCst) {
        return Status::INVALID_PARAMETER;
    }
    unsafe {
        *fb = FB.load(SeqCst);
        *w = W;
        *h = H;
    }
    Status::SUCCESS
}

fn leak<T>(v: T) -> u64 {
    Box::leak(Box::new(v)) as *mut T as u64
}

fn install(words: [u64; 6], path: Option<Guid>) -> u64 {
    let iface = leak(words);
    let h = unsafe { boot::install_protocol_interface(None, &GOP_GUID, iface as *const c_void) }.expect("install GOP");
    if let Some(g) = path {
        let mut dp: Vec<u8> = alloc::vec![1, 4, 20, 0];
        dp.extend_from_slice(&g.to_bytes());
        dp.extend_from_slice(&[0x7f, 0xff, 4, 0]);
        let p = Box::leak(dp.into_boxed_slice()).as_ptr();
        unsafe { boot::install_protocol_interface(Some(h), &DevicePath::GUID, p as *const c_void) }.expect("install device path");
    }
    iface
}

/// Installs the five GOPs. Returns a line for the log.
pub fn install_all() -> String {
    let bytes = (W * H * 4) as usize;
    let fb = boot::allocate_pages(AllocateType::AnyPages, MemoryType::BOOT_SERVICES_DATA, bytes.div_ceil(4096)).expect("framebuffer");
    // A pattern the loader must overwrite.
    unsafe { core::ptr::write_bytes(fb.as_ptr(), 0x5a, bytes) };
    FB.store(fb.as_ptr() as u64, SeqCst);
    let info = leak(info());
    let mode = leak(Mode { max_mode: 1, mode: 0, info: info as *const Info, size_of_info: INFO_SIZE, fb_base: fb.as_ptr() as u64, fb_size: bytes });
    let (qm, sm, bl) = (query_mode as *const () as u64, set_mode as *const () as u64, blt as *const () as u64);
    let ext = install([qm, sm, bl, refresh as *const () as u64, get_frame_buffer as *const () as u64, mode], Some(GOP_GUID));
    EXT.store(ext, SeqCst);
    install([qm, sm, bl, 0, 0, mode], None);
    let gq = garbage_query_mode as *const () as u64;
    install([gq, sm, bl, 0, 0, 0], None);
    // A Mode in RAM whose fields are nonsense: what a standard reader sees at +0x18 of
    // an extended GOP.
    let garbage = leak([0x0000_1234_7777_0000_u64, 0x0123_4567_89ab_cdef, 0x9999, 0, 0]);
    install([gq, sm, bl, garbage, 0, 0], None);
    install([gq, sm, bl, 0x0000_7ead_0000_0000, 0, 0], None);
    alloc::format!("PARENT_FAKEFW installed ext=0x{ext:x} fb=0x{:x}", fb.as_ptr() as u64)
}

/// The counts and the refresh calls.
pub fn report() -> Vec<String> {
    let mut out = alloc::vec![alloc::format!(
        "PARENT_FAKEFW calls query={} set_mode={} blt={} get_fb={} garbage_query={} refresh={}",
        QUERY.load(SeqCst),
        SET_MODE.load(SeqCst),
        BLT.load(SeqCst),
        GET_FB.load(SeqCst),
        GARBAGE_QUERY.load(SeqCst),
        unsafe { (*core::ptr::addr_of!(REFRESHES)).len() }
    )];
    for c in unsafe { (*core::ptr::addr_of!(REFRESHES)).iter() } {
        out.push(alloc::format!(
            "PARENT_FAKEFW_REFRESH this={} fb={} rect={},{},{},{} stride={} mode={} crc={:08x}",
            c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], c[8]
        ));
    }
    out
}
