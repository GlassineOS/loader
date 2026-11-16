// SPDX-License-Identifier: GPL-2.0-or-later
//! The loader under a parent bootloader (GlassineOS/boot spec 012, `docs/handback.md`):
//! headless, its record raw on the partition of `DeviceHandle`, its answer in the UEFI
//! exit data. Everything is opened with `GetProtocol`, never exclusively, so no driver
//! of the firmware is disconnected for the parent's later use.
use alloc::alloc::{Layout, alloc, dealloc};
use alloc::string::String;
use alloc::vec::Vec;
use glassine_loader_core::answer::{Answer, Boot};
use glassine_loader_core::plan::{self, Log, ReadErr, RecordStore};
use glassine_loader_core::raw::{self, Blocks, Raw};
use uefi::boot::{self, MemoryType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
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

impl Log for Disk {
    fn log(&mut self, line: core::fmt::Arguments) {
        log::line(line);
    }
}

/// Opens the record partition: the block I/O of our `DeviceHandle`.
fn open() -> Result<Disk, &'static str> {
    let dev = get::<LoadedImage>(boot::image_handle()).ok().and_then(|li| li.device()).ok_or("no DeviceHandle")?;
    let io = get::<BlockIO>(dev).map_err(|_| "no block I/O on DeviceHandle")?;
    if !io.media().is_media_present() {
        return Err("no medium");
    }
    let raw = Raw::new(FwBlocks { io }).map_err(|e| {
        crate::log!("the partition can't be used: {:?}", e);
        "unusable partition"
    })?;
    Ok(Disk { raw })
}

/// Decides, writes what the decision writes, and returns to the parent. Never
/// returns.
pub fn run(opts: Opts) -> ! {
    log!("HANDBACK start serial={} pick_recovery={}", opts.serial as u8, opts.pick_recovery as u8);
    let answer = match open() {
        Err(why) => {
            log!("HANDBACK {}: the baseline", why);
            Answer::new(Boot::Baseline, "no-record")
        }
        Ok(mut d) => {
            let rec = plan::read_record(&mut d);
            if rec.as_ref().is_some_and(|r| r.rec.get("probe") == "1") {
                let report = crate::probe::report();
                let ok = d.raw.write_probe(&report);
                log!("PROBE_WRITTEN ok={} bytes={}", ok as u8, raw::PROBE_BYTES);
            }
            plan::handback(&mut d, rec, opts.pick_recovery)
            // `d` and its block I/O are closed here, before the exit.
        }
    };
    let data = answer.exit_data();
    log!("HANDBACK_ANSWER {}", String::from_utf8_lossy(&answer.text()).replace('\n', " "));
    give_back(&data)
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
