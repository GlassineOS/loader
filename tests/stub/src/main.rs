// SPDX-License-Identifier: GPL-2.0-or-later
//! A stand-in kernel for the QEMU tests, started by the loader like a Linux EFI stub.
//! It prints, on the firmware's serial line, its load options and the size and sha256
//! of the initramfs it gets through the Linux initrd media device path, then powers the
//! machine off. It draws nothing, so the loader's last frame stays on screen.
//!
//! Words of its command line it reads: `stub.hold=<ms>` waits before the power-off (for
//! frame captures); `stub.return` returns to the loader instead, as a kernel that
//! fails to start would; `stub.answer=garbage|badcrc|unknown`, started as a loader by
//! a parent (`glassine.handback=1`), exits with exit data that a parent must refuse.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::fmt::Write;
use core::time::Duration;
use sha2::{Digest, Sha256};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams};
use uefi::proto::console::serial::Serial;
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::load_file::LoadFile2;
use uefi::runtime::{self, ResetType};
use uefi::{Status, entry};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
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

/// VenMedia(LINUX_EFI_INITRD_MEDIA_GUID)/End.
const INITRD_PATH: [u8; 24] = [
    4, 3, 20, 0, 0x27, 0xe4, 0x68, 0x55, 0xfc, 0x68, 0x3d, 0x4f, 0xac, 0x74, 0xca, 0x55, 0x52, 0x31, 0xcc, 0x68, 0x7f, 0xff, 4, 0,
];

fn initrd() -> String {
    let Ok(dp) = <&DevicePath>::try_from(&INITRD_PATH[..]) else { return "bad path".into() };
    let mut rest = dp;
    let Ok(h) = boot::locate_device_path::<LoadFile2>(&mut rest) else { return "none".into() };
    let Ok(mut lf) = boot::open_protocol_exclusive::<LoadFile2>(h) else { return "unopened".into() };
    match lf.load_file(dp) {
        Ok(b) => {
            let mut s = String::new();
            for x in Sha256::digest(&b) {
                let _ = write!(s, "{x:02x}");
            }
            format!("size={} sha256={s}", b.len())
        }
        Err(e) => format!("error {:?}", e.status()),
    }
}

/// Exits as a broken loader would, with exit data a parent must not act on.
fn bad_answer(kind: &str) -> ! {
    use glassine_loader_core::answer::{Answer, Boot};
    use glassine_loader_core::gspl::crc32;
    let good = Answer::new(Boot::Recovery, "limit").exit_data();
    let data: alloc::vec::Vec<u8> = match kind {
        "garbage" => b"\x13\x37not an answer at all, long enough to pass the size check of a parent".to_vec(),
        "badcrc" => {
            let mut d = good.clone();
            let i = d.iter().rposition(|c| *c == b'=').unwrap() + 1;
            d[i] = if d[i] == b'0' { b'1' } else { b'0' };
            d
        }
        _ => {
            let body = "format=1\nboot=reboot\n";
            let mut d = good[..glassine_loader_core::answer::PREFIX_BYTES].to_vec();
            d.extend_from_slice(format!("{body}crc32={:08x}\n", crc32(body.as_bytes())).as_bytes());
            d
        }
    };
    let p = boot::allocate_pool(boot::MemoryType::LOADER_DATA, data.len()).unwrap();
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), p.as_ptr(), data.len());
        let _ = boot::exit(boot::image_handle(), Status::ABORTED, data.len(), p.as_ptr() as *mut uefi::Char16);
    }
    loop {}
}

#[entry]
fn main() -> Status {
    let cmdline: String = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
        .ok()
        .and_then(|li| li.load_options_as_cstr16().ok().map(|c| format!("{c}")))
        .unwrap_or_default();
    say("STUB_START");
    say(&format!("STUB_CMDLINE: {cmdline}"));
    say(&format!("STUB_INITRD: {}", initrd()));
    if let Some(kind) = cmdline.split(' ').find_map(|w| w.strip_prefix("stub.answer=")) {
        say(&format!("STUB_ANSWER {kind}"));
        bad_answer(kind);
    }
    if cmdline.split(' ').any(|w| w == "stub.return") {
        say("STUB_RETURN");
        return Status::LOAD_ERROR;
    }
    let hold = cmdline.split(' ').find_map(|w| w.strip_prefix("stub.hold=")).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    boot::stall(Duration::from_millis(hold));
    say("STUB_POWEROFF");
    runtime::reset(ResetType::SHUTDOWN, Status::SUCCESS, None)
}
