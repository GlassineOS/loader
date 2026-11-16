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
//! One picture, one refresh of the whole panel at (0, 0), synchronous, before the loader
//! hands back: never after `ExitBootServices` (the loader never calls it), never
//! `SetMode`, `Blt` or a power protocol.
use alloc::string::String;
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
    Ok(EpdFw { gop, fb, w, h, bound })
}

fn rgb565(p: u32) -> u16 {
    let (r, g, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
    (((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)) as u16
}

impl EpdFw {
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
        fwgop::refresh(&self.gop, self.fb, 0, 0, self.w, self.h, self.w).unwrap_or(Status::UNSUPPORTED)
    }
}
