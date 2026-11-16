// SPDX-License-Identifier: GPL-2.0-or-later
//! A graphics output protocol (GOP) read as raw words, without trusting its layout.
//!
//! The standard interface is four words: QueryMode, SetMode, Blt, Mode. Some firmware
//! extends it to six: QueryMode, SetMode, Blt, a panel refresh
//! `Refresh(This, fb, x, y, w, h, stride, 0)`, `GetFrameBuffer(This, &fb, &w, &h)`, and
//! Mode last (+0x28). Code built against the standard header reads the refresh's code
//! address as the Mode pointer there. A console splitter built for that firmware has the
//! six-word layout with the two extra words NULL.
//!
//! So nothing here goes through `uefi::proto::console::gop::GraphicsOutput`: every word is
//! read only after the memory map says the bytes are RAM, a Mode pointer is taken only
//! after it passes [`read_mode`], and only the words a check has placed in a loaded
//! image are ever called.
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use uefi::boot::{self, MemoryType, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::mem::memory_map::MemoryMap;
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::{Guid, Handle, Identify, Status};

/// The GOP GUID (UEFI specification).
pub const GOP_GUID: Guid = uefi::guid!("9042a9de-23dc-4a38-96fb-7aded080516a");
/// The vendor GUID of Qualcomm's display variables (`gQcomTokenSpaceGuid`,
/// `QcomModulePkg.dec`; ABL reads `DisplaySplashBufferInfo` and
/// `DisplayPanelConfiguration` under it).
pub const QCOM_TOKEN_SPACE: Guid = uefi::guid!("882f8c2b-9646-435f-8de5-f208ff80c1bd");
/// The size of the mode information structure (UEFI specification).
pub const INFO_SIZE: usize = 0x24;
/// The largest `MaxMode` taken for real. QEMU's own GOP has 30 modes and PCs have
/// similar counts, so the limit is above those; a code address or other garbage read as
/// `MaxMode` is far larger.
pub const MAX_MODES: u32 = 256;
/// The two places a Mode pointer can be, tried in this order.
pub const MODE_AT: [usize; 2] = [0x18, 0x28];

fn get<P: uefi::proto::ProtocolPointer + ?Sized>(h: Handle) -> uefi::Result<boot::ScopedProtocol<P>> {
    let params = OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }
}

/// The RAM ranges of the memory map: types the firmware backs with memory, which are
/// mapped while boot services run.
pub struct Ram {
    ranges: Vec<(u64, u64)>,
}

fn is_ram(t: MemoryType) -> bool {
    matches!(
        t,
        MemoryType::LOADER_CODE
            | MemoryType::LOADER_DATA
            | MemoryType::BOOT_SERVICES_CODE
            | MemoryType::BOOT_SERVICES_DATA
            | MemoryType::RUNTIME_SERVICES_CODE
            | MemoryType::RUNTIME_SERVICES_DATA
            | MemoryType::CONVENTIONAL
            | MemoryType::ACPI_RECLAIM
            | MemoryType::ACPI_NON_VOLATILE
            | MemoryType::PERSISTENT_MEMORY
    )
}

impl Ram {
    pub fn read() -> Ram {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        if let Ok(m) = boot::memory_map(MemoryType::LOADER_DATA) {
            for d in m.entries() {
                if is_ram(d.ty) {
                    let end = d.phys_start.saturating_add(d.page_count.saturating_mul(4096));
                    ranges.push((d.phys_start, end));
                }
            }
        }
        // Neighbouring ranges of different types are one readable range.
        ranges.sort();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (s, e) in ranges {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        Ram { ranges: merged }
    }

    /// Whether all `len` bytes from `addr` are RAM.
    pub fn holds(&self, addr: u64, len: u64) -> bool {
        let Some(end) = addr.checked_add(len) else { return false };
        addr != 0 && self.ranges.iter().any(|&(s, e)| s <= addr && end <= e)
    }

    pub fn ranges(&self) -> usize {
        self.ranges.len()
    }
}

/// Reads a `u64` the caller has checked with [`Ram::holds`].
unsafe fn rd64(addr: u64) -> u64 {
    unsafe { core::ptr::read_unaligned(addr as *const u64) }
}

unsafe fn rd32(addr: u64) -> u32 {
    unsafe { core::ptr::read_unaligned(addr as *const u32) }
}

/// One loaded image: where it is and the last node of its file path.
pub struct Image {
    pub index: usize,
    pub base: u64,
    pub size: u64,
    pub path: String,
}

pub struct Images {
    pub list: Vec<Image>,
}

impl Images {
    pub fn read(ram: &Ram) -> Images {
        let mut list = Vec::new();
        let Ok(hs) = boot::locate_handle_buffer(SearchType::ByProtocol(&LoadedImage::GUID)) else { return Images { list } };
        for (i, &h) in hs.iter().enumerate() {
            let Ok(li) = get::<LoadedImage>(h) else { continue };
            let (base, size) = li.info();
            let path = match li.file_path() {
                Some(dp) => path_text(ram, dp.as_ffi_ptr() as u64),
                None => String::from("none"),
            };
            list.push(Image { index: i + 1, base: base as u64, size, path });
        }
        Images { list }
    }

    /// The image `addr` lies in.
    pub fn of(&self, addr: u64) -> Option<&Image> {
        self.list.iter().find(|i| addr >= i.base && addr - i.base < i.size)
    }
}

fn guid_at(b: &[u8]) -> Guid {
    let mut a = [0u8; 16];
    a.copy_from_slice(&b[..16]);
    Guid::from_bytes(a)
}

/// A device path as text, node by node from its bytes; no firmware text protocol.
/// Known nodes by name, the others as `<type>.<subtype>:<length>`.
pub fn path_text(ram: &Ram, mut p: u64) -> String {
    let mut out: Vec<String> = Vec::new();
    for _ in 0..32 {
        if !ram.holds(p, 4) {
            out.push(String::from("unreadable"));
            break;
        }
        let (t, s) = unsafe { (*(p as *const u8), *((p + 1) as *const u8)) };
        let len = unsafe { core::ptr::read_unaligned((p + 2) as *const u16) } as u64;
        if t == 0x7f && s == 0xff {
            break;
        }
        if len < 4 || !ram.holds(p, len) {
            out.push(format!("{t}.{s}:bad-length-{len}"));
            break;
        }
        let d = unsafe { core::slice::from_raw_parts((p + 4) as *const u8, (len - 4) as usize) };
        out.push(match (t, s) {
            (1, 4) if d.len() >= 16 => format!("VenHw({}{})", guid_at(d), if d.len() > 16 { format!(",+{}", d.len() - 16) } else { String::new() }),
            (1, 1) if d.len() >= 2 => format!("Pci({},{})", d[1], d[0]),
            (4, 1) if d.len() >= 4 => format!("HD({})", u32::from_le_bytes([d[0], d[1], d[2], d[3]])),
            (4, 6) if d.len() >= 16 => format!("FvFile({})", guid_at(d)),
            (4, 7) if d.len() >= 16 => format!("Fv({})", guid_at(d)),
            (4, 4) => {
                let s: String = d
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&c| c != 0)
                    .map(|c| char::from_u32(c as u32).filter(|c| c.is_ascii_graphic()).unwrap_or('?'))
                    .collect();
                format!("File({s})")
            }
            _ => format!("{t}.{s}:{len}"),
        });
        p += len;
    }
    if out.is_empty() { String::from("end") } else { out.join("/") }
}

/// The fields of a Mode that passed every check, and of its `Info`.
#[derive(Clone, Copy)]
pub struct Mode {
    pub max_mode: u32,
    pub mode: u32,
    pub fb_base: u64,
    pub fb_size: u64,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub stride: u32,
}

/// Reads `p` as a Mode: non-zero, 8-byte aligned, its 40 bytes in RAM, `SizeOfInfo`
/// 0x24, `MaxMode` at most [`MAX_MODES`], and `Info`'s 0x24 bytes in RAM. `Err`: the first check that failed, for the report.
pub fn read_mode(ram: &Ram, p: u64) -> Result<Mode, String> {
    if p == 0 {
        return Err("null".into());
    }
    if p % 8 != 0 {
        return Err("unaligned".into());
    }
    if !ram.holds(p, 40) {
        return Err("not-ram".into());
    }
    let (max_mode, mode, info, size_of_info, fb_base, fb_size) =
        unsafe { (rd32(p), rd32(p + 4), rd64(p + 8), rd64(p + 16), rd64(p + 24), rd64(p + 32)) };
    if size_of_info != INFO_SIZE as u64 {
        return Err(format!("size_of_info=0x{size_of_info:x}"));
    }
    if max_mode > MAX_MODES {
        return Err(format!("max_mode=0x{max_mode:x}"));
    }
    if !ram.holds(info, INFO_SIZE as u64) {
        return Err(format!("info=0x{info:x}"));
    }
    let (width, height, format, stride) = unsafe { (rd32(info + 4), rd32(info + 8), rd32(info + 12), rd32(info + 32)) };
    Ok(Mode { max_mode, mode, fb_base, fb_size, width, height, format, stride })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// Mode at +0x18.
    Standard,
    /// Mode at +0x28 and the refresh and GetFrameBuffer words in the image of QueryMode,
    /// SetMode and Blt.
    Extended,
    /// Mode at +0x28, the two extra words not both in that image (the console splitter's).
    ExtendedNoSlots,
    /// No Mode passed its checks.
    Unknown,
}

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Layout::Standard => "standard",
            Layout::Extended => "extended",
            Layout::ExtendedNoSlots => "extended-no-slots",
            Layout::Unknown => "unknown",
        }
    }
}

/// One GOP instance, read raw.
pub struct Gop {
    pub handle: Handle,
    pub iface: u64,
    /// The six words, when all 48 bytes are RAM.
    pub words: Option<[u64; 6]>,
    /// The check at +0x18 and at +0x28: the Mode, or why it was refused.
    pub checks: [Result<Mode, String>; 2],
    pub layout: Layout,
}

impl Gop {
    pub fn mode(&self) -> Option<&Mode> {
        match self.layout {
            Layout::Standard => self.checks[0].as_ref().ok(),
            Layout::Extended | Layout::ExtendedNoSlots => self.checks[1].as_ref().ok(),
            Layout::Unknown => None,
        }
    }
}

/// The interface pointer of the GOP on `h`, by `HandleProtocol`'s rule (`GetProtocol`).
pub fn interface(h: Handle) -> Option<u64> {
    let bt = unsafe { uefi::table::system_table_raw()?.as_ref().boot_services.as_ref()? };
    let mut iface: *mut c_void = core::ptr::null_mut();
    let st = unsafe { (bt.open_protocol)(h.as_ptr(), &GOP_GUID, &mut iface, boot::image_handle().as_ptr(), core::ptr::null_mut(), 0x02) };
    (st == Status::SUCCESS && !iface.is_null()).then_some(iface as u64)
}

/// Reads the GOP on `h`. Calls nothing of it.
pub fn inspect(ram: &Ram, images: &Images, h: Handle) -> Option<Gop> {
    let iface = interface(h)?;
    let words = ram.holds(iface, 48).then(|| {
        let mut w = [0u64; 6];
        for (i, x) in w.iter_mut().enumerate() {
            *x = unsafe { rd64(iface + 8 * i as u64) };
        }
        w
    });
    let Some(w) = words else {
        return Some(Gop { handle: h, iface, words, checks: [Err("not-ram".into()), Err("not-ram".into())], layout: Layout::Unknown });
    };
    let checks = [read_mode(ram, w[3]), read_mode(ram, w[5])];
    let img = |a: u64| images.of(a).map(|i| i.index);
    let layout = match (&checks[0], &checks[1]) {
        (Ok(_), _) => Layout::Standard,
        (Err(_), Ok(_)) => {
            let home = img(w[0]);
            let slots = home.is_some() && [w[1], w[2], w[3], w[4]].iter().all(|&a| img(a) == home);
            if slots { Layout::Extended } else { Layout::ExtendedNoSlots }
        }
        _ => Layout::Unknown,
    };
    Some(Gop { handle: h, iface, words, checks, layout })
}

/// Every GOP handle and its raw reading, in handle order.
pub fn all(ram: &Ram, images: &Images) -> Vec<Gop> {
    let hs: Vec<Handle> =
        boot::locate_handle_buffer(SearchType::ByProtocol(&GOP_GUID)).map(|b| b.iter().copied().collect()).unwrap_or_default();
    hs.into_iter().filter_map(|h| inspect(ram, images, h)).collect()
}

type QueryModeFn = unsafe extern "efiapi" fn(u64, u32, *mut usize, *mut u64) -> Status;
type GetFrameBufferFn = unsafe extern "efiapi" fn(u64, *mut u64, *mut u64, *mut u64) -> Status;
type RefreshFn = unsafe extern "efiapi" fn(u64, u64, u64, u64, u64, u64, u64, u64) -> Status;

/// One mode by QueryMode through the word at +0x00: `(width, height, format, stride)`.
/// Only for a GOP with a Mode that passed, `n` below its `MaxMode`, and QueryMode in a
/// loaded image. The firmware's `Info` buffer is freed.
pub fn query_mode(g: &Gop, images: &Images, ram: &Ram, n: u32) -> Option<(u32, u32, u32, u32)> {
    let m = g.mode()?;
    let w = g.words?;
    if n >= m.max_mode || images.of(w[0]).is_none() {
        return None;
    }
    let f: QueryModeFn = unsafe { core::mem::transmute(w[0] as usize) };
    let mut size: usize = 0;
    let mut info: u64 = 0;
    let st = unsafe { f(g.iface, n, &mut size, &mut info) };
    if st != Status::SUCCESS || info == 0 {
        return None;
    }
    let r = (size >= INFO_SIZE && ram.holds(info, INFO_SIZE as u64))
        .then(|| unsafe { (rd32(info + 4), rd32(info + 8), rd32(info + 12), rd32(info + 32)) });
    if let Some(p) = core::ptr::NonNull::new(info as *mut u8) {
        let _ = unsafe { boot::free_pool(p) };
    }
    r
}

/// GetFrameBuffer (+0x20), for an [`Layout::Extended`] GOP only: `(status, fb, w, h)`.
/// The width and height are taken as 32-bit values; the out slots are 64-bit and zeroed,
/// so either width of write reads back right.
pub fn get_frame_buffer(g: &Gop) -> Option<(Status, u64, u32, u32)> {
    if g.layout != Layout::Extended {
        return None;
    }
    let w = g.words?;
    let f: GetFrameBufferFn = unsafe { core::mem::transmute(w[4] as usize) };
    let (mut fb, mut ww, mut hh) = (0u64, 0u64, 0u64);
    let st = unsafe { f(g.iface, &mut fb, &mut ww, &mut hh) };
    Some((st, fb, ww as u32, hh as u32))
}

/// The panel refresh (+0x18), for an [`Layout::Extended`] GOP only. Every argument is
/// passed zero-extended to 64 bits, so a callee that reads 32 bits reads the same.
/// The probe never calls this.
pub fn refresh(g: &Gop, fb: u64, x: u32, y: u32, w: u32, h: u32, stride: u32) -> Option<Status> {
    if g.layout != Layout::Extended {
        return None;
    }
    let words = g.words?;
    let f: RefreshFn = unsafe { core::mem::transmute(words[3] as usize) };
    Some(unsafe { f(g.iface, fb, x as u64, y as u64, w as u64, h as u64, stride as u64, 0) })
}

/// The device path on `h`, as text, or `none`.
pub fn handle_path(ram: &Ram, h: Handle) -> String {
    match get::<DevicePath>(h) {
        Ok(dp) => path_text(ram, dp.as_ffi_ptr() as u64),
        Err(_) => String::from("none"),
    }
}
