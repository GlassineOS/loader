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
//! With `parent.fakefw=hang` the refresh never returns, as a refresh that hangs inside
//! the firmware would; the parent's watchdog then resets the machine.
//!
//! With `parent.fakefw=device` the extended GOP has the shape a Note Air's probe report
//! shows: `GetFrameBuffer` gives the 1872x1404 panel, but the Mode names a 325x1423
//! framebuffer (stride 325, 1,849,900 bytes) at the same base, too small for the
//! panel's RGB565 picture; the volatile variable `DisplaySplashBufferInfo` (version 1,
//! the base, 0x02300000 bytes; attributes 0x6) names the region that holds it. The
//! framebuffer is that whole region, below 4 GiB (the variable's words are 32 bits).
//! `device-<var>` changes the variable, see [`Var`].
//!
//! With each shape the stand-in also installs an absolute pointer
//! (`EFI_ABSOLUTE_POINTER_PROTOCOL`) on a handle of its own with the device path
//! Vendor(HW, [`POINTER_PATH`])/End and the Mode of [`POINTER_MODE`]; its `Reset` and
//! `GetState` count their calls, and `GetState` gives `EFI_NOT_READY` (nothing touched).
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
use uefi::runtime::{self, VariableAttributes, VariableVendor};
use uefi::{Guid, Identify, Status, cstr16, guid};

pub const GOP_GUID: Guid = guid!("9042a9de-23dc-4a38-96fb-7aded080516a");
pub const W: u32 = 1872;
pub const H: u32 = 1404;
const INFO_SIZE: usize = 0x24;

/// The stand-in pointer's device path vendor GUID.
pub const POINTER_PATH: Guid = guid!("5c8a3f1e-7b2d-4e9a-8c6f-1d3e5a7b9c0f");
/// The stand-in pointer's Mode: `[min_x, min_y, min_z, max_x, max_y, max_z]`, attributes.
pub const POINTER_MODE: ([u64; 6], u32) = ([0, 0, 0, 4095, 3071, 0], 0);
const ABS_POINTER_GUID: Guid = guid!("8d59d32b-c655-4ae9-9b15-f25904992a43");
static PTR_RESET: AtomicU32 = AtomicU32::new(0);
static PTR_STATE: AtomicU32 = AtomicU32::new(0);

static QUERY: AtomicU32 = AtomicU32::new(0);
static SET_MODE: AtomicU32 = AtomicU32::new(0);
static BLT: AtomicU32 = AtomicU32::new(0);
static GET_FB: AtomicU32 = AtomicU32::new(0);
static GARBAGE_QUERY: AtomicU32 = AtomicU32::new(0);
static EXT: AtomicU64 = AtomicU64::new(0);
static FB: AtomicU64 = AtomicU64::new(0);
/// `parent.fakefw=hang`: the refresh never returns (the parent's watchdog resets).
static HANG: AtomicU32 = AtomicU32::new(0);
/// The Mode's picture: width, height, stride.
static MODE_W: AtomicU32 = AtomicU32::new(W);
static MODE_H: AtomicU32 = AtomicU32::new(H);
static MODE_STRIDE: AtomicU32 = AtomicU32::new(W);

/// The device's Mode: 325x1423, stride 325, 325x4x1423 bytes.
const DEVICE_MODE: (u32, u32, u32) = (325, 1423, 325);
const DEVICE_MODE_BYTES: usize = 1_849_900;
/// The size `DisplaySplashBufferInfo` gives on the device.
const DEVICE_REGION: u32 = 0x0230_0000;
/// `gQcomTokenSpaceGuid`.
const QCOM_TOKEN_SPACE: Guid = guid!("882f8c2b-9646-435f-8de5-f208ff80c1bd");

/// `DisplaySplashBufferInfo` in the device shape.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Var {
    /// `device`: version 1, the framebuffer's base, 0x02300000 bytes.
    Good,
    /// `device-novar`: none.
    Absent,
    /// `device-v2`: version 2, else as `Good`.
    Version2,
    /// `device-short`: the first 11 bytes of `Good`.
    Short,
    /// `device-small`: version 1, the base, one byte less than the panel's picture.
    Small,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `1`: the Mode is the panel's.
    Panel,
    /// `hang`: as `Panel`, the refresh never returns.
    Hang,
    /// `device[-<var>]`.
    Device(Var),
}

impl Shape {
    pub fn parse(w: &str) -> Option<Shape> {
        Some(match w {
            "1" => Shape::Panel,
            "hang" => Shape::Hang,
            "device" => Shape::Device(Var::Good),
            "device-novar" => Shape::Device(Var::Absent),
            "device-v2" => Shape::Device(Var::Version2),
            "device-short" => Shape::Device(Var::Short),
            "device-small" => Shape::Device(Var::Small),
            _ => return None,
        })
    }
}
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
    Info { version: 0, w: MODE_W.load(SeqCst), h: MODE_H.load(SeqCst), format: 1, masks: [0; 4], stride: MODE_STRIDE.load(SeqCst) }
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
    crate::say("PARENT_FAKEFW_REFRESH_START");
    if HANG.load(SeqCst) != 0 {
        crate::say("PARENT_FAKEFW_HANG");
        loop {
            boot::stall(core::time::Duration::from_millis(100));
        }
    }
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

#[repr(C)]
struct PointerMode {
    min: [u64; 3],
    max: [u64; 3],
    attributes: u32,
}

#[repr(C)]
struct AbsPointer {
    reset: unsafe extern "efiapi" fn(u64, u8) -> Status,
    get_state: unsafe extern "efiapi" fn(u64, *mut u64) -> Status,
    wait_for_input: u64,
    mode: *const PointerMode,
}

unsafe extern "efiapi" fn ptr_reset(_this: u64, _ext: u8) -> Status {
    PTR_RESET.fetch_add(1, SeqCst);
    Status::SUCCESS
}

unsafe extern "efiapi" fn ptr_get_state(_this: u64, _state: *mut u64) -> Status {
    PTR_STATE.fetch_add(1, SeqCst);
    Status::NOT_READY
}

/// Installs the absolute pointer on a handle of its own. Returns the interface.
fn install_pointer() -> u64 {
    let ([a, b, c, d, e, f], attributes) = POINTER_MODE;
    let mode = leak(PointerMode { min: [a, b, c], max: [d, e, f], attributes });
    let iface = leak(AbsPointer { reset: ptr_reset, get_state: ptr_get_state, wait_for_input: 0, mode: mode as *const PointerMode });
    let h = unsafe { boot::install_protocol_interface(None, &ABS_POINTER_GUID, iface as *const c_void) }.expect("install pointer");
    let mut dp: Vec<u8> = alloc::vec![1, 4, 20, 0];
    dp.extend_from_slice(&POINTER_PATH.to_bytes());
    dp.extend_from_slice(&[0x7f, 0xff, 4, 0]);
    let p = Box::leak(dp.into_boxed_slice()).as_ptr();
    unsafe { boot::install_protocol_interface(Some(h), &DevicePath::GUID, p as *const c_void) }.expect("install pointer path");
    iface
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

/// Installs the five GOPs (and, in the device shape, the variable). Returns a line for
/// the log.
pub fn install_all(shape: Shape) -> String {
    HANG.store((shape == Shape::Hang) as u32, SeqCst);
    let pattern = (W * H * 4) as usize;
    let (bytes, mode_bytes, below_4g) = match shape {
        Shape::Device(_) => {
            let (w, h, stride) = DEVICE_MODE;
            MODE_W.store(w, SeqCst);
            MODE_H.store(h, SeqCst);
            MODE_STRIDE.store(stride, SeqCst);
            (DEVICE_REGION as usize, DEVICE_MODE_BYTES, true)
        }
        _ => (pattern, pattern, false),
    };
    let at = if below_4g { AllocateType::MaxAddress(0xffff_f000) } else { AllocateType::AnyPages };
    let fb = boot::allocate_pages(at, MemoryType::BOOT_SERVICES_DATA, bytes.div_ceil(4096)).expect("framebuffer");
    // A pattern the loader must overwrite.
    unsafe { core::ptr::write_bytes(fb.as_ptr(), 0x5a, pattern) };
    FB.store(fb.as_ptr() as u64, SeqCst);
    let mut var = String::new();
    if let Shape::Device(v) = shape {
        var = set_splash_info(v, fb.as_ptr() as u64);
    }
    let info = leak(info());
    let mode = leak(Mode { max_mode: 1, mode: 0, info: info as *const Info, size_of_info: INFO_SIZE, fb_base: fb.as_ptr() as u64, fb_size: mode_bytes });
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
    let ptr = install_pointer();
    alloc::format!("PARENT_FAKEFW installed ext=0x{ext:x} fb=0x{:x}{var} pointer=0x{ptr:x}", fb.as_ptr() as u64)
}

/// Sets `DisplaySplashBufferInfo` as `v` says, volatile (attributes 0x6: it leaves no
/// trace in the variable store). Returns the words for the log line.
fn set_splash_info(v: Var, base: u64) -> String {
    if v == Var::Absent {
        return " var=absent".into();
    }
    let base = u32::try_from(base).expect("framebuffer below 4 GiB");
    let size = match v {
        Var::Small => W * H * 2 - 1,
        _ => DEVICE_REGION,
    };
    let version: u32 = if v == Var::Version2 { 2 } else { 1 };
    let mut b: Vec<u8> = Vec::new();
    for x in [version, base, size] {
        b.extend_from_slice(&x.to_le_bytes());
    }
    if v == Var::Short {
        b.truncate(11);
    }
    let name = cstr16!("DisplaySplashBufferInfo");
    let vendor = VariableVendor(QCOM_TOKEN_SPACE);
    let attrs = VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;
    runtime::set_variable(name, &vendor, attrs, &b).expect("set DisplaySplashBufferInfo");
    let hex: String = b.iter().map(|x| alloc::format!("{x:02x}")).collect();
    alloc::format!(" var={hex}")
}

/// The counts and the refresh calls.
pub fn report() -> Vec<String> {
    let mut out = alloc::vec![alloc::format!(
        "PARENT_FAKEFW calls query={} set_mode={} blt={} get_fb={} garbage_query={} refresh={} ptr_reset={} ptr_state={}",
        QUERY.load(SeqCst),
        SET_MODE.load(SeqCst),
        BLT.load(SeqCst),
        GET_FB.load(SeqCst),
        GARBAGE_QUERY.load(SeqCst),
        unsafe { (*core::ptr::addr_of!(REFRESHES)).len() },
        PTR_RESET.load(SeqCst),
        PTR_STATE.load(SeqCst)
    )];
    for c in unsafe { (*core::ptr::addr_of!(REFRESHES)).iter() } {
        out.push(alloc::format!(
            "PARENT_FAKEFW_REFRESH this={} fb={} rect={},{},{},{} stride={} mode={} crc={:08x}",
            c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], c[8]
        ));
    }
    out
}
