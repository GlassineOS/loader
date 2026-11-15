//! GlassineOS loader spike: splash and one-time override (011), the recovery key and
//! its list (010 FR-039), a one-shot deployment pick (010 FR-042), starting a kernel set.
#![no_std]
#![no_main]

extern crate alloc;

mod fsutil;
mod kernel;
mod record;
mod serial;
mod splash;
mod time;
mod ui;

use alloc::vec::Vec;
use core::ffi::c_char;
use core::time::Duration;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput, PixelFormat};
use uefi::proto::console::text::{Key, ScanCode};
use uefi::proto::media::file::Directory;
use uefi::{Status, entry};

const SPLASH: &str = "\\glassine\\splash\\splash.gspl";
const DEFAULT_SPLASH: &str = "\\glassine\\splash\\default.gspl";
const OVERRIDE: &str = "\\glassine\\splash\\override.gspl";
const RECORD: &str = "\\glassine\\loader.conf";
/// 011 FR-005: reading and checking one image has a time limit.
const IMAGE_TIME_LIMIT_MS: u64 = 1000;
const RECOVERY_KEY: char = ' ';
const HINT: &[u8] = b"Press Space for start options\0";

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log!("PANIC {}", info);
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

fn load_image(root: &mut Directory, path: &str) -> Option<Result<splash::Kind, ()>> {
    let t0 = time::now_ms();
    let r = match fsutil::read(root, path, splash::MAX_BYTES + splash::HEADER as u64, t0 + IMAGE_TIME_LIMIT_MS) {
        Err(fsutil::ReadError::Missing) => return None,
        Err(fsutil::ReadError::TooLarge(n)) => {
            log!("image {} skipped: {} bytes", path, n);
            return Some(Err(()));
        }
        Err(fsutil::ReadError::TimedOut) => {
            log!("image {} skipped: time limit", path);
            return Some(Err(()));
        }
        Err(fsutil::ReadError::Io(s)) => {
            log!("image {} skipped: {:?}", path, s);
            return Some(Err(()));
        }
        Ok(b) => b,
    };
    Some(parse_image(path, &r, t0))
}

fn parse_image(path: &str, b: &[u8], t0: u64) -> Result<splash::Kind, ()> {
    match splash::parse(b) {
        Ok(k) if time::now_ms() <= t0 + IMAGE_TIME_LIMIT_MS => {
            log!("image {} ok ({} bytes, {} ms)", path, b.len(), time::now_ms() - t0);
            Ok(k)
        }
        Ok(_) => {
            log!("image {} skipped: time limit", path);
            Err(())
        }
        Err(e) => {
            log!("image {} skipped: {:?}", path, e);
            Err(())
        }
    }
}

enum Picture {
    Image(splash::Image, &'static str),
    LeaveScreen,
    None,
}

/// 011 FR-012: the override is read, removed durably, and only then drawn.
fn pick_picture(root: &mut Directory, mode: (u32, u32)) -> Picture {
    if fsutil::exists(root, OVERRIDE) {
        let t0 = time::now_ms();
        let bytes = fsutil::read(root, OVERRIDE, splash::MAX_BYTES + splash::HEADER as u64,
                                 t0 + IMAGE_TIME_LIMIT_MS);
        match fsutil::remove_durably(root, OVERRIDE) {
            Ok(()) => {
                log!("OVERRIDE_REMOVED");
                if let Ok(b) = bytes {
                    match parse_image(OVERRIDE, &b, t0) {
                        Ok(splash::Kind::LeaveScreen) => return Picture::LeaveScreen,
                        Ok(splash::Kind::Image(img)) if img.mode == mode => {
                            return Picture::Image(img, "override");
                        }
                        Ok(splash::Kind::Image(img)) => {
                            log!("override made for {:?}, display is {:?}: not shown", img.mode, mode);
                        }
                        Err(()) => {}
                    }
                }
            }
            Err(s) => log!("OVERRIDE_NOT_REMOVED {:?}: not shown", s),
        }
    }
    for (path, name) in [(SPLASH, "splash"), (DEFAULT_SPLASH, "default")] {
        match load_image(root, path) {
            Some(Ok(splash::Kind::Image(img))) => return Picture::Image(img, name),
            Some(Ok(splash::Kind::LeaveScreen)) => log!("{} is no image: skipped", path),
            _ => {}
        }
    }
    Picture::None
}

fn read_key() -> Option<Key> {
    uefi::system::with_stdin(|i| i.read_key().ok().flatten())
}

/// The list of 010 FR-039. Returns the index of the chosen entry.
fn run_list(names: &[&str]) -> usize {
    let cstrs: Vec<Vec<u8>> = names.iter().map(|n| n.bytes().chain(Some(0)).collect()).collect();
    let ptrs: Vec<*const c_char> = cstrs.iter().map(|c| c.as_ptr() as *const c_char).collect();
    unsafe {
        ui::shim_show_list(c"Start options".as_ptr(), ptrs.as_ptr(), ptrs.len() as i32);
        ui::shim_refresh_now();
    }
    log!("LIST_SHOWN");
    loop {
        match read_key() {
            Some(Key::Special(ScanCode::UP)) => unsafe { ui::shim_key(ui::LV_KEY_PREV) },
            Some(Key::Special(ScanCode::DOWN)) => unsafe { ui::shim_key(ui::LV_KEY_NEXT) },
            Some(Key::Printable(c)) if u16::from(c) == 13 || u16::from(c) == 10 => unsafe {
                ui::shim_key(ui::LV_KEY_ENTER)
            },
            _ => {}
        }
        unsafe { ui::shim_handle() };
        let c = unsafe { ui::shim_list_choice() };
        if c >= 0 {
            unsafe {
                ui::shim_hide_list();
                ui::shim_refresh_now();
            }
            return c as usize;
        }
        boot::stall(Duration::from_millis(5));
    }
}

fn read_screen(g: &mut GraphicsOutput, w: u32, h: u32, screen: &mut [u32]) {
    let buf = unsafe { core::slice::from_raw_parts_mut(screen.as_mut_ptr() as *mut BltPixel, screen.len()) };
    let _ = g.blt(BltOp::VideoToBltBuffer { buffer: buf, src: (0, 0), dest: BltRegion::Full,
                                            dims: (w as usize, h as usize) });
}

/// Starts LVGL on a `w`x`h` display with `screen` (XRGB8888) as the background picture.
fn ui_start(w: u32, h: u32, screen: Vec<u32>) {
    let draw_bytes = (w * 64 * 4) as usize;
    let draw_buf = Vec::leak(alloc::vec![0u8; draw_bytes]);
    let screen = Vec::leak(screen);
    unsafe {
        ui::shim_init(w, h, ui::flush, time::lv_tick, ui::log, draw_buf.as_mut_ptr().cast(), draw_bytes as u32);
        ui::shim_set_background(screen.as_ptr() as *const u8, w, h);
    }
}

#[entry]
fn main() -> Status {
    time::calibrate();
    log!("LOADER_START");

    let image = boot::image_handle();
    let mut root = match boot::get_image_file_system(image).map(|mut fs| fs.open_volume()) {
        Ok(Ok(r)) => r,
        _ => {
            log!("no file system on the loader's own volume");
            return Status::NOT_FOUND;
        }
    };
    let rec = match fsutil::read(&mut root, RECORD, 64 << 10, u64::MAX) {
        Ok(b) => record::parse(core::str::from_utf8(&b).unwrap_or("")),
        Err(_) => {
            log!("no loader record");
            record::parse("")
        }
    };

    let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>().ok();
    let mut gop: Option<ScopedProtocol<GraphicsOutput>> = gop_handle.and_then(|h| unsafe {
        let attr = if rec.exclusive_gop { OpenProtocolAttributes::Exclusive } else { OpenProtocolAttributes::GetProtocol };
        boot::open_protocol::<GraphicsOutput>(OpenProtocolParams { handle: h, agent: image, controller: None }, attr).ok()
    });

    let mut drawn = false;
    let mut dims: Option<(u32, u32)> = None;
    if let Some(g) = gop.as_mut() {
        let mi = g.current_mode_info();
        let (w, h) = mi.resolution();
        let (w, h) = (w as u32, h as u32);
        let fmt = match mi.pixel_format() {
            PixelFormat::Rgb => "RGBX",
            PixelFormat::Bgr => "BGRX",
            PixelFormat::Bitmask => "bitmask",
            PixelFormat::BltOnly => "blt-only",
        };
        log!("GOP {}x{} {} stride {}", w, h, fmt, mi.stride());
        ui::set_gop(g);
        let pic = pick_picture(&mut root, (w, h));
        let mut screen: Vec<u32> = alloc::vec![0; (w * h) as usize];
        let what = match &pic {
            Picture::Image(img, name) => {
                splash::compose(img, w, h, &mut screen);
                *name
            }
            Picture::None => {
                // Nothing to draw: keep what the screen shows, draw the hint over it.
                read_screen(g, w, h, &mut screen);
                "none"
            }
            Picture::LeaveScreen => "leave-screen",
        };
        drop(pic);
        dims = Some((w, h));
        if !matches!(what, "leave-screen") {
            ui_start(w, h, screen);
            unsafe {
                ui::shim_show_hint(HINT.as_ptr() as *const c_char);
                ui::shim_refresh_now();
            }
            drawn = true;
        }
        log!("SPLASH_DRAWN {}", what);
    } else {
        log!("no GOP: nothing drawn");
    }

    // The wait for the recovery key (010 FR-017, FR-039).
    log!("WAIT_START {} ms", rec.wait_ms);
    let end = time::now_ms() + rec.wait_ms;
    let mut open_list = false;
    while time::now_ms() < end {
        if let Some(Key::Printable(c)) = read_key() {
            if u16::from(c) == RECOVERY_KEY as u16 {
                open_list = true;
                break;
            }
        }
        if drawn {
            unsafe { ui::shim_handle() };
        }
        boot::stall(Duration::from_millis(10));
    }
    log!("WAIT_END key={}", open_list);

    let mut extra = alloc::string::String::new();
    if open_list && !drawn {
        // Nothing was drawn (leave-screen override): the key still works (010 FR-017).
        // LVGL starts now, over what the screen shows.
        match (gop.as_mut(), dims) {
            (Some(g), Some((w, h))) => {
                let mut screen = alloc::vec![0u32; (w * h) as usize];
                read_screen(g, w, h, &mut screen);
                ui_start(w, h, screen);
                drawn = true;
            }
            _ => log!("no display: the list can't be shown; starting GlassineOS"),
        }
    }
    if open_list && drawn {
        let prev = rec.previous_deploy.as_ref();
        let prev_name = match prev {
            Some((_, label)) => alloc::format!("Previous deployment: {}", label),
            None => "Previous deployment (none)".into(),
        };
        let names = ["GlassineOS", prev_name.as_str(), "Previous kernel", "Recovery", "Firmware set-up"];
        let c = run_list(&names);
        log!("CHOICE {} {}", c, names[c]);
        match (c, prev) {
            (1, Some((id, _))) => extra = alloc::format!("glassine.deploy.once={}", id),
            (2..=4, _) => log!("CHOICE_NOT_IN_SPIKE {}: starting GlassineOS", names[c]),
            _ => {}
        }
    } else if drawn {
        // 010 FR-039 draws the line only during the wait; take it off again.
        unsafe {
            ui::shim_hide_hint();
            ui::shim_refresh_now();
        }
        log!("HINT_HIDDEN");
    }

    let st = kernel::start(&mut root, &rec, &extra);
    log!("no kernel set started: {:?}", st);
    st
}
