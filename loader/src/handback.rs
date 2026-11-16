// SPDX-License-Identifier: GPL-2.0-or-later
//! The loader under a parent bootloader (GlassineOS/boot spec 012, `docs/handback.md`):
//! headless (unless the record asks for the firmware-refreshed display of spec 003 and
//! the firmware has one), its record on the boot partition of `DeviceHandle` (FAT or
//! raw), its answer in the UEFI exit data. Everything is opened with `GetProtocol`, never exclusively, so no driver
//! of the firmware is disconnected for the parent's later use.
use alloc::alloc::{Layout, alloc, dealloc};
use alloc::string::String;
use alloc::vec::Vec;
use glassine_loader_core::answer::{Answer, Boot};
use glassine_loader_core::plan::{self, Log, Platform, ReadErr, RecordStore};
use glassine_loader_core::raw::{Blocks, Raw};
use uefi::boot::{self, MemoryType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{Handle, Status};

use crate::log;

/// What the parent asked for in the load options.
pub struct Opts {
    pub serial: bool,
    pub pick_recovery: bool,
}

fn get<P: uefi::proto::ProtocolPointer + ?Sized>(h: Handle) -> uefi::Result<ScopedProtocol<P>> {
    let params = OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }
}

/// The load options' words, if they put the loader in hand-back mode.
pub fn options() -> Option<Opts> {
    let li = get::<LoadedImage>(boot::image_handle()).ok()?;
    let text: String = alloc::format!("{}", li.load_options_as_cstr16().ok()?);
    let words: Vec<&str> = text.split(' ').collect();
    if !words.contains(&"glassine.handback=1") {
        return None;
    }
    Some(Opts { serial: words.contains(&"glassine.log=serial"), pick_recovery: words.contains(&"glassine.pick=recovery") })
}

/// The partition's block I/O. Reads and writes go through a buffer aligned as the
/// medium asks (`IoAlign`).
struct FwBlocks {
    io: ScopedProtocol<BlockIO>,
}

impl FwBlocks {
    fn layout(&self, len: usize) -> Option<Layout> {
        let a = (self.io.media().io_align() as usize).max(64);
        if !a.is_power_of_two() || len == 0 {
            return None;
        }
        Layout::from_size_align(len, a).ok()
    }
}

impl Blocks for FwBlocks {
    fn block_size(&self) -> u32 {
        self.io.media().block_size()
    }
    fn blocks(&self) -> u64 {
        self.io.media().last_block().saturating_add(1)
    }
    fn read_only(&self) -> bool {
        self.io.media().is_read_only()
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let Some(l) = self.layout(buf.len()) else { return false };
        let p = unsafe { alloc(l) };
        if p.is_null() {
            return false;
        }
        let b = unsafe { core::slice::from_raw_parts_mut(p, buf.len()) };
        let id = self.io.media().media_id();
        let ok = self.io.read_blocks(id, lba, b).is_ok();
        if ok {
            buf.copy_from_slice(b);
        }
        unsafe { dealloc(p, l) };
        ok
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> bool {
        let Some(l) = self.layout(buf.len()) else { return false };
        let p = unsafe { alloc(l) };
        if p.is_null() {
            return false;
        }
        let b = unsafe { core::slice::from_raw_parts_mut(p, buf.len()) };
        b.copy_from_slice(buf);
        let id = self.io.media().media_id();
        let r = self.io.write_blocks(id, lba, b);
        unsafe { dealloc(p, l) };
        if let Err(e) = &r {
            crate::log!("write at block {}: {:?}", lba, e.status());
        }
        r.is_ok()
    }
    fn now_ms(&mut self) -> u64 {
        crate::arch::now_ms()
    }
    fn flush(&mut self) -> bool {
        match self.io.flush_blocks() {
            Ok(()) => true,
            Err(e) => {
                crate::log!("flush: {:?}", e.status());
                false
            }
        }
    }
}

struct Disk {
    raw: Raw<FwBlocks>,
}

impl RecordStore for Disk {
    fn read_slot(&mut self, slot: usize) -> Result<Vec<u8>, ReadErr> {
        self.raw.read_slot(slot)
    }
    fn write_slot(&mut self, slot: usize, bytes: &[u8]) -> bool {
        self.raw.write_slot(slot, bytes)
    }
}

impl glassine_loader_core::plan::SplashStore for Disk {
    fn read_image(&mut self, which: plan::Image, max: u64, deadline_ms: Option<u64>) -> Result<Vec<u8>, ReadErr> {
        self.raw.read_image(which, max, deadline_ms)
    }
    fn has_override(&mut self) -> bool {
        self.raw.has_override()
    }
    fn remove_override(&mut self) -> bool {
        self.raw.remove_override()
    }
    fn now_ms(&mut self) -> u64 {
        crate::arch::now_ms()
    }
}

impl Log for Disk {
    fn log(&mut self, line: core::fmt::Arguments) {
        log::line(line);
    }
}

/// The boot partition in the form the loader found it in (boot `docs/boot-partition.md`,
/// "Two forms"): FAT when the firmware offers a file system on it, raw otherwise.
enum Store {
    Fat(crate::fs::Volume),
    Raw(Disk),
}

/// Opens the boot partition: the partition of our `DeviceHandle`.
fn open() -> Result<(Store, Handle), &'static str> {
    let dev = get::<LoadedImage>(boot::image_handle()).ok().and_then(|li| li.device()).ok_or("no DeviceHandle")?;
    if let Ok(mut fs) = get::<SimpleFileSystem>(dev) {
        if let Ok(root) = fs.open_volume() {
            log!("HANDBACK form=fat");
            return Ok((Store::Fat(crate::fs::Volume { root }), dev));
        }
    }
    let io = get::<BlockIO>(dev).map_err(|_| "no block I/O on DeviceHandle")?;
    if !io.media().is_media_present() {
        return Err("no medium");
    }
    let raw = Raw::new(FwBlocks { io }).map_err(|e| {
        crate::log!("the partition can't be used: {:?}", e);
        "unusable partition"
    })?;
    match &raw.regions {
        Some((copy, regs)) => log!("HANDBACK form=raw table={:?} regions={}", copy, regs.len()),
        None => log!("HANDBACK form=raw no table: the partition holds nothing"),
    }
    Ok((Store::Raw(Disk { raw }), dev))
}

/// The probe report's place in the FAT form (`docs/handback.md`, "The probe report").
const PROBE_FILE: &str = "\\glassine\\probe.txt";

/// Decides, writes what the decision writes, and returns to the parent. Never
/// returns. Headless, the splash store is left as it is: such a start draws nothing, so
/// it uses up no override (boot spec 012 FR-015a); with `display=epd-fw` it is used as
/// on a PC (spec 003).
pub fn run(opts: Opts) -> ! {
    log!("HANDBACK start serial={} pick_recovery={}", opts.serial as u8, opts.pick_recovery as u8);
    let answer = match open() {
        Err(why) => {
            log!("HANDBACK {}: the baseline", why);
            Answer::new(Boot::Baseline, "no-record")
        }
        Ok((Store::Raw(mut d), dev)) => {
            let rec = plan::read_record(&mut d);
            if rec.as_ref().is_some_and(|r| r.rec.get("probe") == "1") {
                let report = crate::probe::report(Some(dev));
                let ok = d.raw.write_probe(&report);
                log!("PROBE_WRITTEN ok={} bytes={} form=raw", ok as u8, report.len());
            }
            if wants_epd(rec.as_ref()) {
                epd_splash(&mut d);
            }
            plan::handback(&mut d, rec, opts.pick_recovery)
        }
        Ok((Store::Fat(mut v), dev)) => {
            let rec = plan::read_record(&mut v);
            if rec.as_ref().is_some_and(|r| r.rec.get("probe") == "1") {
                let report = crate::probe::report(Some(dev));
                let ok = Platform::write_file(&mut v, PROBE_FILE, &report);
                log!("PROBE_WRITTEN ok={} bytes={} form=fat", ok as u8, report.len());
            }
            if wants_epd(rec.as_ref()) {
                epd_splash(&mut v);
            }
            plan::handback(&mut v, rec, opts.pick_recovery)
        }
    };
    let data = answer.exit_data();
    log!("HANDBACK_ANSWER {}", String::from_utf8_lossy(&answer.text()).replace('\n', " "));
    give_back(&data)
}

/// Whether the record asks for the firmware-refreshed display (`display=epd-fw`).
fn wants_epd(rec: Option<&glassine_loader_core::record::InUse>) -> bool {
    rec.is_some_and(|r| r.rec.get("display") == "epd-fw")
}

/// The splash through the firmware's panel refresh (boot `docs/handback.md`, "A display
/// through the firmware's refresh"): the override used up and the picture picked as on
/// a PC, composed in portrait, written and refreshed once, before the answer. With no
/// such firmware, headless: the store is left as it is. No key line and no list: which
/// key a person would press at power-on is not defined on such a machine.
fn epd_splash<S: plan::SplashStore + Log>(store: &mut S) {
    let mut epd = match crate::display::epdfw::find() {
        Ok(e) => e,
        Err(why) => {
            log!("EPD_FW_NONE {}: headless", why);
            return;
        }
    };
    log!("EPD_FW {}", epd.describe());
    let (w, h) = epd.size();
    let (pic, what) = match plan::pick_picture(store, &[(w, h)]) {
        plan::Pick::Override(p) => (p, "override"),
        plan::Pick::Splash(p, n) => (p, n),
        plan::Pick::LeaveScreen => {
            log!("SPLASH_DRAWN leave-screen");
            return;
        }
        plan::Pick::Nothing => {
            log!("SPLASH_DRAWN none");
            return;
        }
    };
    let mut buf = alloc::vec![0u32; (w as usize) * (h as usize)];
    glassine_loader_core::gspl::compose(&pic, w, h, &mut buf);
    drop(pic);
    let t0 = crate::arch::now_ms();
    let st = epd.show(&buf);
    log!("SPLASH_DRAWN {} refresh={:?} in {} ms", what, st, crate::arch::now_ms() - t0);
}

/// `Exit` with `data` as the exit data, in a buffer from `AllocatePool` that the parent
/// frees. UEFI keeps exit data only with a status other than `EFI_SUCCESS`.
fn give_back(data: &[u8]) -> ! {
    let (ptr, size) = match boot::allocate_pool(MemoryType::LOADER_DATA, data.len()) {
        Ok(p) => {
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), p.as_ptr(), data.len()) };
            (p.as_ptr() as *mut uefi::Char16, data.len())
        }
        Err(_) => {
            log!("no pool for the answer: none given");
            (core::ptr::null_mut(), 0)
        }
    };
    unsafe {
        let _ = boot::exit(boot::image_handle(), Status::ABORTED, size, ptr);
    }
    crate::arch::halt()
}
