// SPDX-License-Identifier: GPL-2.0-or-later
//! A stand-in parent bootloader for the QEMU tests (GlassineOS/boot `docs/handback.md`,
//! "Starting the loader"). It does what the Note Air's ABL does: finds the hand-back
//! partition, checks the loader image header and the image, starts the image from memory
//! with no device path, `DeviceHandle` set to the partition and the hand-back load
//! option, under a 30 s watchdog, and reads the answer from the exit data. It prints, on
//! the firmware's serial line, what it would boot (`PARENT_BOOT baseline|recovery|set:<id>`)
//! and why it fell to the baseline (`PARENT_FLOOR <reason>`), then powers off.
//!
//! The partition is the one whose GPT unique identity is [`PARTUUID`]. Extra load
//! options for a case come from `\parent.cfg` on the parent's own partition: one line,
//! the words to add (`glassine.log=serial` is always added, so the tests see the
//! loader's log).
#![no_std]
#![no_main]

extern crate alloc;

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

/// The parent's path, as the ABL patch does it. `Err`: why it boots its baseline.
fn hand_back() -> Result<Boot, String> {
    let part = partition().ok_or("no hand-back partition")?;
    let mut io = get::<BlockIO>(part).map_err(|_| "no block I/O")?;
    let size = (io.media().last_block() + 1) * io.media().block_size() as u64;
    let h = read(&mut io, raw::LOADER_HEADER_OFFSET, 4096).ok_or("header read")?;
    let (len, want) = raw::check_header(&h, size).map_err(|e| format!("header {e:?}"))?;
    let img = read(&mut io, raw::LOADER_IMAGE_OFFSET, len as usize).ok_or("image read")?;
    if crc32(&img) != want {
        return Err("image crc".into());
    }
    drop(io);
    let bt = unsafe { uefi::table::system_table_raw().unwrap().as_ref().boot_services.as_ref().unwrap() };

    // LoadImage from memory, no device path.
    let mut child: uefi_raw::Handle = core::ptr::null_mut();
    let st = unsafe {
        (bt.load_image)(false.into(), boot::image_handle().as_ptr(), core::ptr::null(), img.as_ptr(), img.len(), &mut child)
    };
    if st.is_error() {
        if st == Status::SECURITY_VIOLATION && !child.is_null() {
            let _ = unsafe { (bt.unload_image)(child) };
        }
        return Err(format!("LoadImage {st:?}"));
    }
    let child_h = unsafe { Handle::from_ptr(child) }.ok_or("no image handle")?;

    // DeviceHandle and the load options.
    let words = format!("glassine.handback=1 glassine.log=serial {}", extra_options());
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

#[entry]
fn main() -> Status {
    say("PARENT_START_PARENT");
    match hand_back() {
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
