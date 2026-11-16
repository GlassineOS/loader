// SPDX-License-Identifier: GPL-2.0-or-later
//! A stand-in parent bootloader for the QEMU tests (GlassineOS/boot `docs/handback.md`,
//! "Starting the loader"). It does what the Note Air's ABL does: finds the
//! boot partition, reads the loader (the file in the FAT form; the region `loader`
//! through the region table, header and image checked, in the raw form), starts it from memory
//! with the partition's device path, `DeviceHandle` set to the partition and the hand-back load
//! option, under a 30 s watchdog, and reads the answer from the exit data. It prints, on
//! the firmware's serial line, what it would boot (`PARENT_BOOT baseline|recovery|set:<id>`)
//! and why it fell to the baseline (`PARENT_FLOOR <reason>`), then powers off.
//!
//! The partition is the one whose GPT unique identity is [`PARTUUID`]. Extra load
//! options for a case come from `\parent.cfg` on the parent's own partition: one line,
//! the words to add (`glassine.log=serial` is always added, so the tests see the
//! loader's log). Words that start with `parent.` are the parent's own and not passed:
//! `parent.fakefw=1` installs the stand-in extended graphics output of [`fakefw`],
//! `parent.fakefw=hang` the same with a refresh that never returns, and
//! `parent.fakefw=device[-<var>]` the same in the shape a Note Air reports (a Mode smaller
//! than the panel, and the variable `DisplaySplashBufferInfo`; see [`fakefw::Var`]).
//! `parent.faketouch=<mode>` installs the stand-in I2C and GPIO protocols and touch
//! controller of [`faketouch`].
#![no_std]
#![no_main]

extern crate alloc;

mod fakefw;
mod faketouch;

use alloc::alloc::{Layout, alloc, dealloc};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::c_void;
use glassine_loader_core::answer::{self, Boot};
use glassine_loader_core::gspl::crc32;
use glassine_loader_core::raw;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, SearchType};
use uefi::proto::console::serial::Serial;
use uefi::proto::device_path::media::PartitionSignature;
use uefi::proto::device_path::{DevicePath, DevicePathNodeEnum};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode};
use uefi::runtime::{self, ResetType};
use uefi::{CString16, Guid, Handle, Identify, Status, entry, guid};
use uefi_raw::protocol::loaded_image::LoadedImageProtocol;

const PARTUUID: Guid = guid!("8a1f3c5e-2b4d-4e6f-9a0b-1c2d3e4f5a6b");

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    say(&format!("PARENT_PANIC {info}"));
    runtime::reset(ResetType::SHUTDOWN, Status::ABORTED, None)
}

fn say(s: &str) {
    let Ok(h) = boot::get_handle_for_protocol::<Serial>() else { return };
    let p = OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
    if let Ok(mut ser) = unsafe { boot::open_protocol::<Serial>(p, OpenProtocolAttributes::GetProtocol) } {
        let _ = ser.write(s.as_bytes());
        let _ = ser.write(b"\r\n");
    }
}

fn get<P: uefi::proto::ProtocolPointer + ?Sized>(h: Handle) -> uefi::Result<ScopedProtocol<P>> {
    let params = OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }
}

fn partition() -> Option<Handle> {
    let hs = boot::locate_handle_buffer(SearchType::ByProtocol(&BlockIO::GUID)).ok()?;
    for &h in hs.iter() {
        let Ok(dp) = get::<DevicePath>(h) else { continue };
        for node in dp.node_iter() {
            if let Ok(DevicePathNodeEnum::MediaHardDrive(hd)) = node.as_enum() {
                if hd.partition_signature() == PartitionSignature::Guid(PARTUUID) {
                    return Some(h);
                }
            }
        }
    }
    None
}

fn read(io: &mut BlockIO, off: u64, len: usize) -> Option<Vec<u8>> {
    let bs = io.media().block_size() as u64;
    let n = (len as u64).div_ceil(bs) * bs;
    let l = Layout::from_size_align(n as usize, (io.media().io_align() as usize).max(64)).ok()?;
    let p = unsafe { alloc(l) };
    if p.is_null() {
        return None;
    }
    let b = unsafe { core::slice::from_raw_parts_mut(p, n as usize) };
    let id = io.media().media_id();
    let r = io.read_blocks(id, off / bs, b).ok().map(|_| b[..len].to_vec());
    unsafe { dealloc(p, l) };
    r
}

fn extra_options() -> String {
    let Ok(mut fs) = boot::get_image_file_system(boot::image_handle()) else { return String::new() };
    let Ok(mut root) = fs.open_volume() else { return String::new() };
    let Ok(h) = root.open(&CString16::try_from("\\parent.cfg").unwrap(), FileMode::Read, FileAttribute::empty()) else {
        return String::new();
    };
    let Some(mut f) = h.into_regular_file() else { return String::new() };
    let size = f.get_boxed_info::<FileInfo>().map(|i| i.file_size()).unwrap_or(0) as usize;
    let mut b = alloc::vec![0u8; size.min(512)];
    let n = f.read(&mut b).unwrap_or(0);
    String::from_utf8_lossy(&b[..n]).trim().into()
}

/// The loader's bytes from the boot partition: in the FAT form the file
/// `\EFI\glassine\loader.efi`, in the raw form the region `loader` through the region
/// table (the primary, else the backup), as the ABL does it.
fn loader_bytes(part: Handle) -> Result<Vec<u8>, String> {
    if let Ok(mut fs) = get::<SimpleFileSystem>(part) {
        say("PARENT_FORM fat");
        let mut root = fs.open_volume().map_err(|_| "no volume")?;
        let h = root
            .open(&CString16::try_from("\\EFI\\glassine\\loader.efi").unwrap(), FileMode::Read, FileAttribute::empty())
            .map_err(|_| "no loader.efi")?;
        let mut f = h.into_regular_file().ok_or("loader.efi is no file")?;
        let size = f.get_boxed_info::<FileInfo>().map_err(|_| "no file info")?.file_size();
        if size == 0 || size > raw::LOADER_MAX {
            return Err(format!("loader.efi size {size}"));
        }
        let mut b = alloc::vec![0u8; size as usize];
        let n = f.read(&mut b).map_err(|_| "loader.efi read")?;
        if n as u64 != size {
            return Err("loader.efi short read".into());
        }
        return Ok(b);
    }
    say("PARENT_FORM raw");
    let mut io = get::<BlockIO>(part).map_err(|_| "no block I/O")?;
    let size = (io.media().last_block() + 1) * io.media().block_size() as u64;
    let mut regions = None;
    for off in [0, size.saturating_sub(raw::TABLE_BYTES as u64)] {
        let t = read(&mut io, off, raw::TABLE_BYTES).ok_or("table read")?;
        if let Ok(r) = raw::parse_table(&t, size) {
            say(&format!("PARENT_TABLE at={off}"));
            regions = Some(r);
            break;
        }
    }
    let regions = regions.ok_or("no region table")?;
    let r = regions.iter().find(|r| r.name == raw::LOADER && r.flags == 0).ok_or("no loader region")?;
    let h = read(&mut io, r.off, raw::LOADER_HEADER).ok_or("header read")?;
    let (len, want) = raw::check_header(&h, r.size).map_err(|e| format!("header {e:?}"))?;
    let img = read(&mut io, r.off + raw::LOADER_HEADER as u64, len as usize).ok_or("image read")?;
    if crc32(&img) != want {
        return Err("image crc".into());
    }
    Ok(img)
}

/// The parent's path, as the ABL patch does it. `Err`: why it boots its baseline.
fn hand_back() -> Result<Boot, String> {
    let part = partition().ok_or("no boot partition")?;
    let img = loader_bytes(part)?;
    let bt = unsafe { uefi::table::system_table_raw().unwrap().as_ref().boot_services.as_ref().unwrap() };

    // LoadImage from memory, with the boot partition's device path as the image's path
    // (docs/handback.md step 2): an EDK2 with image verification refuses a buffer with
    // none (EFI_INVALID_PARAMETER), Secure Boot off or on. The firmware reads nothing
    // through it.
    let dpp = get::<DevicePath>(part).map_err(|_| "no device path")?;
    let dp_ptr = dpp.as_ffi_ptr() as *const uefi_raw::protocol::device_path::DevicePathProtocol;
    let mut child: uefi_raw::Handle = core::ptr::null_mut();
    let st = unsafe {
        (bt.load_image)(false.into(), boot::image_handle().as_ptr(), dp_ptr, img.as_ptr(), img.len(), &mut child)
    };
    if st.is_error() {
        if st == Status::SECURITY_VIOLATION && !child.is_null() {
            let _ = unsafe { (bt.unload_image)(child) };
        }
        return Err(format!("LoadImage {st:?}"));
    }
    let child_h = unsafe { Handle::from_ptr(child) }.ok_or("no image handle")?;

    // DeviceHandle and the load options.
    let cfg = extra_options();
    let mine: Vec<&str> = cfg.split(' ').filter(|w| !w.is_empty() && !w.starts_with("parent.")).collect();
    let words = format!("glassine.handback=1 glassine.log=serial {}", mine.join(" "));
    let words = words.trim_end();
    let mut opts: Vec<u16> = words.encode_utf16().chain([0u16]).collect();
    {
        let li = get::<LoadedImage>(child_h).map_err(|_| "no LoadedImage")?;
        let raw_li = &*li as *const LoadedImage as *mut LoadedImageProtocol;
        unsafe {
            (*raw_li).device_handle = part.as_ptr();
            (*raw_li).load_options = opts.as_mut_ptr() as *const c_void;
            (*raw_li).load_options_size = (opts.len() * 2) as u32;
        }
    }
    say(&format!("PARENT_START options=\"{words}\""));
    let _ = boot::set_watchdog_timer(30, 0x10000, None);
    let mut size: usize = 0;
    let mut data: *mut uefi_raw::Char16 = core::ptr::null_mut();
    let st = unsafe { (bt.start_image)(child, &mut size, &mut data) };
    let _ = boot::set_watchdog_timer(0, 0x10000, None);
    let bytes: Vec<u8> = if data.is_null() { Vec::new() } else { unsafe { core::slice::from_raw_parts(data as *const u8, size) }.to_vec() };
    if !data.is_null() {
        let _ = unsafe { (bt.free_pool)(data as *mut u8) };
    }
    drop(opts);
    say(&format!("PARENT_RETURNED status={st:?} exit_data={}", bytes.len()));
    let a = answer::parse(&bytes).map_err(|e| format!("answer {e:?}"))?;
    if let Some(c) = &a.cmdline_add {
        say(&format!("PARENT_CMDLINE_ADD {c}"));
    }
    Ok(a.boot)
}

type SetWatchdog = unsafe extern "efiapi" fn(usize, u64, usize, *const u16) -> Status;
static mut WATCHDOG: Option<SetWatchdog> = None;

/// `SetWatchdogTimer` through the stand-in: every call is logged
/// (`PARENT_WATCHDOG <seconds>`), then made.
unsafe extern "efiapi" fn watchdog(secs: usize, code: u64, size: usize, data: *const u16) -> Status {
    say(&format!("PARENT_WATCHDOG {secs}"));
    match unsafe { WATCHDOG } {
        Some(f) => unsafe { f(secs, code, size, data) },
        None => Status::UNSUPPORTED,
    }
}

/// Puts [`watchdog`] in the boot services table (and its CRC), so the loader's calls are
/// seen.
fn hook_watchdog() {
    let st = uefi::table::system_table_raw().unwrap();
    let bt = unsafe { st.as_ref().boot_services };
    unsafe {
        let orig = (*bt).set_watchdog_timer;
        WATCHDOG = Some(core::mem::transmute::<_, SetWatchdog>(orig));
        (*bt).set_watchdog_timer = core::mem::transmute::<SetWatchdog, _>(watchdog as SetWatchdog);
        (*bt).header.crc = 0;
        let mut crc = 0u32;
        let _ = ((*bt).calculate_crc32)(bt as *const core::ffi::c_void, (*bt).header.size as usize, &mut crc);
        (*bt).header.crc = crc;
    }
}

#[entry]
fn main() -> Status {
    say("PARENT_START_PARENT");
    let cfg = extra_options();
    let word = cfg.split(' ').find_map(|w| w.strip_prefix("parent.fakefw="));
    let fake = word.is_some();
    if let Some(w) = word {
        fakefw::set_nopointer(cfg.split(' ').any(|w| w == "parent.nopointer=1"));
        say(&fakefw::install_all(fakefw::Shape::parse(w).expect("parent.fakefw")));
        fakefw::set_taps(cfg.split(' ').find_map(|w| w.strip_prefix("parent.taps=")).unwrap_or(""));
        fakefw::set_fbdump(cfg.split(' ').any(|w| w == "parent.fbdump=1"));
    }
    let touch = cfg.split(' ').find_map(|w| w.strip_prefix("parent.faketouch="));
    if let Some(w) = touch {
        say(&faketouch::install(faketouch::Mode::parse(w).expect("parent.faketouch")));
        faketouch::set_taps(cfg.split(' ').find_map(|w| w.strip_prefix("parent.touchtaps=")).unwrap_or(""));
    }
    hook_watchdog();
    let r = hand_back();
    if touch.is_some() {
        for l in faketouch::report() {
            say(&l);
        }
    }
    if fake {
        for l in fakefw::report() {
            say(&l);
        }
        fakefw::dump();
    }
    match r {
        Ok(Boot::Baseline) => say("PARENT_BOOT baseline"),
        Ok(Boot::Recovery) => say("PARENT_BOOT recovery"),
        Ok(Boot::Set { id, rec }) => {
            // No block-list reader here: a set is reported, then the baseline.
            say(&format!("PARENT_ANSWER set:{id} rec={rec}"));
            say("PARENT_BOOT baseline");
        }
        Err(why) => {
            say(&format!("PARENT_FLOOR {why}"));
            say("PARENT_BOOT baseline");
        }
    }
    say("PARENT_POWEROFF");
    runtime::reset(ResetType::SHUTDOWN, Status::SUCCESS, None)
}
