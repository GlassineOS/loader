// SPDX-License-Identifier: GPL-2.0-or-later
//! The firmware's graphics output protocol. Every handle with a GOP and a device path is
//! one display; the firmware's console splitter puts a GOP on a handle with no device
//! path, which is no display of its own.
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol, SearchType};
use uefi::proto::console::gop::{BltOp, BltPixel, BltRegion, GraphicsOutput, PixelFormat};
use uefi::proto::device_path::DevicePath;
use uefi::{Identify, Handle};

pub struct Gop {
    gop: ScopedProtocol<GraphicsOutput>,
    w: u32,
    h: u32,
    handle: Handle,
}

pub fn all() -> Vec<Gop> {
    let mut out = Vec::new();
    let Ok(handles) = boot::locate_handle_buffer(SearchType::ByProtocol(&GraphicsOutput::GUID)) else {
        return out;
    };
    for &h in handles.iter() {
        let params = OpenProtocolParams { handle: h, agent: boot::image_handle(), controller: None };
        if !boot::test_protocol::<DevicePath>(params).unwrap_or(false) {
            continue;
        }
        // Shared with the firmware's console: opening it exclusively would disconnect it,
        // and the console draws nothing while the loader writes nothing to it.
        let Ok(gop) = (unsafe { boot::open_protocol::<GraphicsOutput>(params, OpenProtocolAttributes::GetProtocol) }) else {
            continue;
        };
        let (w, h2) = gop.current_mode_info().resolution();
        out.push(Gop { gop, w: w as u32, h: h2 as u32, handle: h });
    }
    out
}

/// `0x00RRGGBB` as a u32 in memory is B, G, R, 0: GOP's `BltPixel` byte for byte.
fn as_blt(px: &[u32]) -> &[BltPixel] {
    unsafe { core::slice::from_raw_parts(px.as_ptr() as *const BltPixel, px.len()) }
}

impl super::Display for Gop {
    fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    fn draw(&mut self, x: u32, y: u32, w: u32, h: u32, px: &[u32], stride: usize) {
        let _ = self.gop.blt(BltOp::BufferToVideo {
            buffer: as_blt(px),
            src: BltRegion::SubRectangle { coords: (0, 0), px_stride: stride },
            dest: (x as usize, y as usize),
            dims: (w as usize, h as usize),
        });
    }

    fn read(&mut self, buf: &mut [u32]) -> bool {
        let b = unsafe { core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut BltPixel, buf.len()) };
        self.gop
            .blt(BltOp::VideoToBltBuffer { buffer: b, src: (0, 0), dest: BltRegion::Full, dims: (self.w as usize, self.h as usize) })
            .is_ok()
    }

    fn describe(&self) -> String {
        let mi = self.gop.current_mode_info();
        let fmt = match mi.pixel_format() {
            PixelFormat::Rgb => "RGBX",
            PixelFormat::Bgr => "BGRX",
            PixelFormat::Bitmask => "bitmask",
            PixelFormat::BltOnly => "blt-only",
        };
        format!("GOP {}x{} {} stride {} handle {:?}", self.w, self.h, fmt, mi.stride(), self.handle.as_ptr())
    }
}
