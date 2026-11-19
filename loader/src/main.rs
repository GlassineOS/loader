// SPDX-License-Identifier: GPL-2.0-or-later
//! The GlassineOS loader of UEFI PCs: GlassineOS/boot specs 010 and 011, against
//! `docs/boot-partition.md` (format 1). What it decides, reads and writes is in
//! `glassine-loader-core`; this program connects that to the firmware.
#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod display;
mod epdmenu;
mod fs;
mod fwbus;
mod fwgop;
mod handback;
mod kernel;
mod log;
mod probe;
mod touch;
mod ui;
mod watchdog;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;
use glassine_loader_core::deploys::{self, Action};
use glassine_loader_core::gspl;
use glassine_loader_core::plan::{self, Choice, Pick};
use glassine_loader_core::record::InUse;
use uefi::boot;
use uefi::proto::console::text::Key;
use uefi::proto::device_path::{DevicePath, DevicePathNodeEnum};
use uefi::proto::device_path::media::PartitionSignature;
use uefi::proto::loaded_image::LoadedImage;
use uefi::runtime::{self, VariableVendor};
use uefi::{Status, cstr16, entry};

use display::Display;

const RECOVERY_KEY: char = ' ';
const HINT: &str = "Press Space for start options";
/// How long a message stays before the loader returns to the firmware.
const MESSAGE_MS: u64 = 10_000;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log!("PANIC {}", info);
    // A panic must not hang the PC: return to the firmware, which goes on to its next
    // boot entry.
    unsafe {
        let _ = boot::exit(boot::image_handle(), Status::ABORTED, 0, core::ptr::null_mut());
    }
    arch::halt()
}

fn variable_u64(name: &uefi::CStr16) -> Option<u64> {
    let mut buf = [0u8; 8];
    let (d, _) = runtime::get_variable(name, &VariableVendor::GLOBAL_VARIABLE, &mut buf).ok()?;
    let mut v = [0u8; 8];
    v[..d.len().min(8)].copy_from_slice(&d[..d.len().min(8)]);
    Some(u64::from_le_bytes(v))
}

fn secure_boot_on() -> bool {
    variable_u64(cstr16!("SecureBoot")).is_some_and(|v| v & 0xff == 1)
}

/// The boot partition, for messages: its label and its GPT identity.
fn partition_name() -> String {
    let id = (|| {
        let li = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()).ok()?;
        let dev = li.device()?;
        let dp = boot::open_protocol_exclusive::<DevicePath>(dev).ok()?;
        for node in dp.node_iter() {
            if let Ok(DevicePathNodeEnum::MediaHardDrive(hd)) = node.as_enum() {
                if let PartitionSignature::Guid(g) = hd.partition_signature() {
                    return Some(format!("{g}"));
                }
            }
        }
        None
    })();
    match id {
        Some(g) => format!("the boot partition GLASSINE (PARTUUID {g})"),
        None => "the boot partition GLASSINE".into(),
    }
}

struct Screen {
    displays: Vec<Box<dyn Display>>,
}

impl Screen {
    /// Starts the UI on the first display, over what it shows now, if it isn't started.
    fn ui_over_screen(&mut self) -> bool {
        if ui::started() {
            return true;
        }
        let Some(d) = self.displays.first_mut() else { return false };
        let (w, h) = d.size();
        let mut pic = alloc::vec![0u32; (w as usize) * (h as usize)];
        if !d.read(&mut pic) {
            log!("the screen can't be read back; drawing over black");
        }
        ui::start(d.as_mut(), pic);
        true
    }

    /// A message that must be seen; stays `MESSAGE_MS` or until a key.
    fn message(&mut self, text: &str) {
        log!("MESSAGE {}", text.replace('\n', " / "));
        if self.ui_over_screen() {
            ui::message(text);
        }
        let end = arch::now_ms() + MESSAGE_MS;
        while arch::now_ms() < end {
            if ui::read_key().is_some() {
                break;
            }
            boot::stall(Duration::from_millis(20));
        }
    }
}

/// Draws the picture (011 FR-006 to FR-010). Returns whether anything was drawn.
fn draw(screen: &mut Screen, pick: Pick) -> bool {
    let what = match &pick {
        Pick::Override(_) => "override",
        Pick::Splash(_, n) => n,
        Pick::LeaveScreen => "leave-screen",
        Pick::Nothing => "none",
    };
    let pic = match pick {
        Pick::Override(p) | Pick::Splash(p, _) => Some(p),
        _ => None,
    };
    let mut first: Option<Vec<u32>> = None;
    if let Some(p) = &pic {
        for (i, d) in screen.displays.iter_mut().enumerate() {
            let (w, h) = d.size();
            let mut buf = alloc::vec![0u32; (w as usize) * (h as usize)];
            gspl::compose(p, w, h, &mut buf);
            d.draw(0, 0, w, h, &buf, w as usize);
            if i == 0 {
                first = Some(buf);
            }
        }
    }
    log!("SPLASH_DRAWN {}", what);
    if what == "leave-screen" {
        return false;
    }
    match (first, screen.displays.first_mut()) {
        (Some(buf), Some(d)) => {
            ui::start(d.as_mut(), buf);
            true
        }
        (None, Some(_)) => screen.ui_over_screen(),
        _ => false,
    }
}

/// The wait for the recovery key (010 FR-017, FR-039). `true`: the key was pressed.
fn wait_for_key(ms: u64, drawn: bool) -> bool {
    log!("WAIT_START {} ms", ms);
    let end = arch::now_ms() + ms;
    let mut key = false;
    while arch::now_ms() < end {
        if let Some(Key::Printable(c)) = ui::read_key() {
            if u16::from(c) == RECOVERY_KEY as u16 {
                key = true;
                break;
            }
        }
        if drawn {
            ui::handle();
        }
        boot::stall(Duration::from_millis(10));
    }
    log!("WAIT_END key={}", key);
    key
}

/// The list and its "More" list (010 FR-039, FR-040). Returns the person's choice.
fn run_list(rec: Option<&InUse>) -> Choice {
    // A PC has no parent and no other system: no such entry (handback.md, "The other
    // system").
    let main = deploys::main_list(rec.map(|r| &r.rec), None);
    loop {
        let names: Vec<&str> = main.iter().map(|e| e.label.as_str()).collect();
        let Some(i) = ui::list("Start options", &names) else {
            log!("CHOICE escape: GlassineOS");
            return if rec.is_some() { Choice::Ordinary } else { Choice::Recovery };
        };
        log!("CHOICE {} {}", i, main[i].label);
        match &main[i].action {
            Action::Continue => return Choice::Ordinary,
            Action::Deploy(id) => return Choice::Deploy(id.clone()),
            Action::PreviousKernel => return Choice::PreviousKernel,
            Action::Recovery => return Choice::Recovery,
            Action::Other(_) | Action::Back => {}
            Action::More => {
                let Some(r) = rec else { continue };
                let more = deploys::more_list(&r.rec);
                let names: Vec<&str> = more.iter().map(|e| e.label.as_str()).collect();
                if let Some(j) = ui::list("Deployments", &names) {
                    log!("CHOICE more {} {}", j, more[j].label);
                    if let Action::Deploy(id) = &more[j].action {
                        return Choice::Deploy(id.clone());
                    }
                }
            }
        }
    }
}

#[entry]
fn main() -> Status {
    arch::init_clock();
    // Under a parent bootloader (boot spec 012): headless, before any display is opened.
    if let Some(o) = handback::options() {
        log::set_serial(o.serial);
        log::keep();
        log!("LOADER_START {} {} handback", arch::NAME, env!("CARGO_PKG_VERSION"));
        handback::run(o);
    }
    log!("LOADER_START {} {}", arch::NAME, env!("CARGO_PKG_VERSION"));
    let mut screen = Screen { displays: display::all() };
    for (i, d) in screen.displays.iter().enumerate() {
        log!("DISPLAY {} {}", i, d.describe());
    }

    // 010 FR-036: GlassineOS starts only with Secure Boot off. Nothing is read or written.
    if secure_boot_on() {
        log!("SECURE_BOOT on: refusing");
        screen.message(
            "GlassineOS starts only with Secure Boot off.\n\n\
             Switch Secure Boot off in the firmware's set-up, then start the PC again.",
        );
        return Status::SECURITY_VIOLATION;
    }

    let root = boot::get_image_file_system(boot::image_handle()).ok().and_then(|mut f| f.open_volume().ok());
    let Some(root) = root else {
        screen.message("GlassineOS can't read the partition it was started from.");
        return Status::NOT_FOUND;
    };
    let mut vol = fs::Volume { root };

    let rec = plan::read_record(&mut vol);
    let modes: Vec<(u32, u32)> = screen.displays.iter().map(|d| d.size()).collect();
    let pick = plan::pick_picture(&mut vol, &modes);
    let drawn = draw(&mut screen, pick);
    if drawn {
        ui::show_hint(HINT);
        log!("HINT_SHOWN");
    }

    let key = wait_for_key(plan::wait_ms(rec.as_ref()), drawn);
    if drawn {
        ui::hide_hint();
        log!("HINT_HIDDEN");
    }
    let choice = if !key {
        Choice::Ordinary
    } else if screen.ui_over_screen() {
        let c = run_list(rec.as_ref());
        log!("LIST_CLOSED");
        c
    } else {
        log!("no display: the list can't be shown; the ordinary start");
        Choice::Ordinary
    };

    if plan::start(&mut vol, &mut kernel::Firmware, rec, choice) {
        return Status::SUCCESS;
    }
    let name = partition_name();
    screen.message(&format!(
        "GlassineOS can't start: nothing on {name} passed its check.\n\n\
         The firmware goes on to its next boot entry. The installer's repair, started \
         from its medium, rebuilds the boot partition."
    ));
    Status::LOAD_ERROR
}
