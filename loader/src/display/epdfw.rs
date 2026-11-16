// SPDX-License-Identifier: GPL-2.0-or-later
//! The `epd-fw` display backend: a panel the firmware refreshes through an extended
//! graphics output (boot `docs/handback.md`, "Graphics outputs read raw"). Off unless the
//! record asks for it with `display=epd-fw` and the firmware has such a GOP.
//!
//! The panel is landscape in the firmware's framebuffer, packed RGB565 from its start,
//! rows `w` pixels apart. The loader draws in portrait: logical size `(h, w)`, a portrait
//! pixel (u, v) at native (x, y) = (w − 1 − v, u). That mapping comes from how the
//! firmware stores its own picture and is not yet confirmed on a device.
//!
//! The splash is one picture, one refresh of the whole panel at (0, 0). The start menu
//! (spec 004) draws through LVGL as a [`Display`]: each flush writes its rectangle into
//! the framebuffer and is remembered, and [`EpdFw::refresh_dirty`] refreshes what changed
//! as a few native rectangles, x and width multiples of 8 (or the whole panel with
//! `display_partial=0`). Every refresh is synchronous, before the loader hands back:
//! never after `ExitBootServices` (the loader never calls it), never `SetMode`, `Blt` or
//! a power protocol. The firmware watchdog is re-armed before each one.
use alloc::string::String;
use alloc::vec::Vec;
use glassine_loader_core::menu::{self, Rect};
use glassine_loader_core::plan;
use uefi::runtime::{self, VariableVendor};
use uefi::{Status, cstr16};

use crate::fwgop::{self, Gop, Images, Layout, Ram};

/// The panel bus moves 8 pixels at a time: a rectangle's x and width are multiples of 8.
const ALIGN: u32 = 8;

pub struct EpdFw {
    gop: Gop,
    fb: u64,
    w: u32,
    h: u32,
    bound: plan::EpdBound,
    /// Portrait rectangles written since the last refresh.
    dirty: Vec<Rect>,
    /// Refresh only what changed (`display_partial` not `0`).
    pub partial: bool,
    /// Refreshes made, for the log.
    pub refreshes: u32,
}

/// The first extended GOP whose framebuffer holds a packed RGB565 panel, or why none.
pub fn find() -> Result<EpdFw, String> {
    let ram = Ram::read();
    let images = Images::read(&ram);
    let Some(gop) = fwgop::all(&ram, &images).into_iter().find(|g| g.layout == Layout::Extended) else {
        return Err("no extended graphics output".into());
    };
    let Some((st, fb, w, h)) = fwgop::get_frame_buffer(&gop) else { return Err("no GetFrameBuffer".into()) };
    if st != Status::SUCCESS {
        return Err(alloc::format!("GetFrameBuffer {st:?}"));
    }
    if fb == 0 || w == 0 || h == 0 || w % ALIGN != 0 {
        return Err(alloc::format!("framebuffer 0x{fb:x} {w}x{h}"));
    }
    // The RGB565 picture must lie inside the framebuffer the Mode names, or inside the
    // region of DisplaySplashBufferInfo (the variable read only when the Mode's is too
    // small).
    let mode = gop.mode().map(|m| (m.fb_base, m.fb_size));
    let bound = match plan::epd_bound(fb, w, h, mode, None) {
        Ok(b) => b,
        Err(_) => {
            let mut buf = [0u8; 64];
            let name = cstr16!("DisplaySplashBufferInfo");
            let var: Option<&[u8]> = match runtime::get_variable(name, &VariableVendor(fwgop::QCOM_TOKEN_SPACE), &mut buf) {
                Ok((d, _)) => Some(d),
                // Longer than 64 bytes: not the 12 of version 1, so no region either.
                Err(e) if e.status() == Status::BUFFER_TOO_SMALL => Some(&[]),
                Err(_) => None,
            };
            plan::epd_bound(fb, w, h, mode, var)?
        }
    };
    Ok(EpdFw { gop, fb, w, h, bound, dirty: Vec::new(), partial: true, refreshes: 0 })
}

fn rgb565(p: u32) -> u16 {
    let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
    (((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)) as u16
}

impl EpdFw {
    /// The panel's native size.
    pub fn size_native(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// The portrait size the loader draws in.
    pub fn size(&self) -> (u32, u32) {
        (self.h, self.w)
    }

    pub fn describe(&self) -> String {
        let bound = match self.bound {
            plan::EpdBound::Mode => "mode",
            plan::EpdBound::SplashBufferInfo => "DisplaySplashBufferInfo",
        };
        alloc::format!("epd-fw {}x{} (panel {}x{} RGB565) fb 0x{:x} bound={bound}", self.h, self.w, self.w, self.h, self.fb)
    }

    /// Writes the portrait picture `px` (`size()`, `0x00RRGGBB`, row by row) into the
    /// framebuffer and refreshes the whole panel once. Returns the refresh's status,
    /// which the interface always gives as 0: success shows only on the panel.
    pub fn show(&mut self, px: &[u32]) -> Status {
        let (lw, lh) = self.size();
        assert!(px.len() >= (lw * lh) as usize);
        let fb = unsafe { core::slice::from_raw_parts_mut(self.fb as *mut u16, (self.w * self.h) as usize) };
        for y in 0..self.h {
            let row = &mut fb[(y * self.w) as usize..((y + 1) * self.w) as usize];
            for (x, out) in row.iter_mut().enumerate() {
                // native (x, y) = (w − 1 − v, u): u = y, v = w − 1 − x
                let v = self.w - 1 - x as u32;
                *out = rgb565(px[(v * lw + y) as usize]);
            }
        }
        self.dirty.clear();
        self.refresh(Rect { x: 0, y: 0, w: self.w, h: self.h })
    }

    /// One refresh of native rectangle `r`, the watchdog re-armed first, logged with its
    /// time.
    fn refresh(&mut self, r: Rect) -> Status {
        crate::watchdog::arm();
        let t0 = crate::arch::now_ms();
        let st = fwgop::refresh(&self.gop, self.fb, r.x, r.y, r.w, r.h, self.w).unwrap_or(Status::UNSUPPORTED);
        self.refreshes += 1;
        crate::log!("EPD_REFRESH {} x={} y={} w={} h={} status={:?} in {} ms", self.refreshes, r.x, r.y, r.w, r.h, st, crate::arch::now_ms() - t0);
        st
    }

    /// Refreshes what was drawn since the last refresh: the whole panel the first time
    /// and with `partial` off, else the changed native rectangles. Returns how many.
    /// How many changed rectangles wait for a refresh.
    pub fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    pub fn refresh_dirty(&mut self) -> usize {
        if self.dirty.is_empty() {
            return 0;
        }
        let full = Rect { x: 0, y: 0, w: self.w, h: self.h };
        let rects: Vec<Rect> = if !self.partial || self.refreshes == 0 {
            alloc::vec![full]
        } else {
            menu::merge(&self.dirty, menu::MAX_RECTS).into_iter().filter_map(|r| menu::native(r, self.w, self.h)).collect()
        };
        self.dirty.clear();
        for r in &rects {
            let _ = self.refresh(*r);
        }
        rects.len()
    }
}

impl super::Display for EpdFw {
    fn size(&self) -> (u32, u32) {
        (self.h, self.w)
    }

    /// Writes portrait rectangle (`x`, `y`, `w`, `h`) of `px` into the framebuffer, at
    /// native (W − 1 − v, u), and remembers it for the next refresh.
    fn draw(&mut self, x: u32, y: u32, w: u32, h: u32, px: &[u32], stride: usize) {
        let (lw, lh) = (self.h, self.w);
        let (w, h) = (w.min(lw.saturating_sub(x)), h.min(lh.saturating_sub(y)));
        let fb = unsafe { core::slice::from_raw_parts_mut(self.fb as *mut u16, (self.w * self.h) as usize) };
        for j in 0..h {
            let v = y + j;
            let row = &px[j as usize * stride..];
            let nx = (self.w - 1 - v) as usize;
            for i in 0..w {
                let u = (x + i) as usize;
                fb[u * self.w as usize + nx] = rgb565(row[i as usize]);
            }
        }
        if w > 0 && h > 0 {
            self.dirty.push(Rect { x, y, w, h });
        }
    }

    fn read(&mut self, _buf: &mut [u32]) -> bool {
        false
    }

    fn describe(&self) -> String {
        EpdFw::describe(self)
    }
}
