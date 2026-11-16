// SPDX-License-Identifier: GPL-2.0-or-later
//! Starting a checked set from memory (boot-partition.md, "How the loader checks and
//! starts a set", step 3): the kernel is loaded from the buffer that was checked, and
//! the initramfs is served from its checked buffer through the Linux initrd media
//! device path (LoadFile2), so the kernel reads nothing from the boot partition.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::c_void;
use glassine_loader_core::plan::{Loaded, Starter};
use uefi::boot::{self, LoadImageSource};
use uefi::proto::device_path::DevicePath;
use uefi::proto::device_path::build::{DevicePathBuilder, media::FilePath};
use uefi::proto::loaded_image::LoadedImage;
use uefi::{Guid, Handle, Status, guid};
use uefi_raw::protocol::device_path::DevicePathProtocol;
use uefi_raw::protocol::media::LoadFile2Protocol;

pub const LINUX_EFI_INITRD_MEDIA_GUID: Guid = guid!("5568e427-68fc-4f3d-ac74-ca555231cc68");

/// VenMedia(LINUX_EFI_INITRD_MEDIA_GUID)/End.
#[repr(C, packed)]
struct InitrdPath {
    typ: u8,
    sub: u8,
    len: [u8; 2],
    guid: [u8; 16],
    end: [u8; 4],
}

#[repr(C)]
struct InitrdLoader {
    proto: LoadFile2Protocol,
    data: *const u8,
    len: usize,
}

unsafe extern "efiapi" fn load_initrd(
    this: *mut LoadFile2Protocol,
    _path: *const DevicePathProtocol,
    boot_policy: uefi_raw::Boolean,
    size: *mut usize,
    buf: *mut c_void,
) -> uefi_raw::Status {
    let me = unsafe { &*(this as *const InitrdLoader) };
    if size.is_null() {
        return Status::INVALID_PARAMETER;
    }
    if bool::from(boot_policy) {
        return Status::UNSUPPORTED;
    }
    unsafe {
        if buf.is_null() || *size < me.len {
            *size = me.len;
            return Status::BUFFER_TOO_SMALL;
        }
        core::ptr::copy_nonoverlapping(me.data, buf as *mut u8, me.len);
        *size = me.len;
    }
    Status::SUCCESS
}

/// The initrd's handle and its two interfaces, removed again when dropped.
struct Initrd {
    handle: Handle,
    path: *mut InitrdPath,
    loader: *mut InitrdLoader,
}

impl Initrd {
    fn install(data: &[u8]) -> uefi::Result<Initrd> {
        let path = Box::into_raw(Box::new(InitrdPath {
            typ: 4,
            sub: 3,
            len: 20u16.to_le_bytes(),
            guid: LINUX_EFI_INITRD_MEDIA_GUID.to_bytes(),
            end: [0x7f, 0xff, 4, 0],
        }));
        let loader = Box::into_raw(Box::new(InitrdLoader {
            proto: LoadFile2Protocol { load_file: load_initrd },
            data: data.as_ptr(),
            len: data.len(),
        }));
        unsafe {
            let handle = boot::install_protocol_interface(None, &DevicePathProtocol::GUID, path as *const c_void)?;
            if let Err(e) = boot::install_protocol_interface(Some(handle), &LoadFile2Protocol::GUID, loader as *const c_void) {
                let _ = boot::uninstall_protocol_interface(handle, &DevicePathProtocol::GUID, path as *const c_void);
                return Err(e);
            }
            Ok(Initrd { handle, path, loader })
        }
    }
}

impl Drop for Initrd {
    fn drop(&mut self) {
        unsafe {
            let a = boot::uninstall_protocol_interface(self.handle, &LoadFile2Protocol::GUID, self.loader as *const c_void);
            let b = boot::uninstall_protocol_interface(self.handle, &DevicePathProtocol::GUID, self.path as *const c_void);
            if a.is_ok() && b.is_ok() {
                drop(Box::from_raw(self.loader));
                drop(Box::from_raw(self.path));
            } else {
                crate::log!("initrd interfaces not removed: {:?} {:?}", a, b);
            }
        }
    }
}

/// The device path of `file` on the partition the loader was started from: that
/// partition's path, then a file path node.
fn kernel_device_path<'a>(file: &str, buf: &'a mut Vec<u8>) -> Option<&'a DevicePath> {
    let dev = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()).ok()?.device()?;
    // shared, not exclusive: the partition's drivers stay as they are
    let params = boot::OpenProtocolParams { handle: dev, agent: boot::image_handle(), controller: None };
    let part = unsafe { boot::open_protocol::<DevicePath>(params, boot::OpenProtocolAttributes::GetProtocol) }.ok()?;
    let mut wide: Vec<u16> = file.encode_utf16().chain(Some(0)).collect();
    let name = uefi::CStr16::from_u16_with_nul(&mut wide).ok()?;
    let mut b = DevicePathBuilder::with_vec(buf);
    for node in part.node_iter() {
        b = b.push(&node).ok()?;
    }
    b.push(&FilePath { path_name: name }).ok()?.finalize().ok()
}

pub struct Firmware;

impl Starter for Firmware {
    fn start(&mut self, set: &Loaded, cmdline: &str) -> bool {
        let _initrd = match &set.initrd {
            Some(i) => match Initrd::install(i) {
                Ok(x) => Some(x),
                Err(e) => {
                    crate::log!("initrd interfaces: {:?}", e.status());
                    return false;
                }
            },
            None => None,
        };
        // UCS-2, NUL-terminated, its size in bytes.
        let opts: Vec<u16> = cmdline.encode_utf16().chain(Some(0)).collect();
        // The checked buffer, with the kernel's file path on this partition: some firmware
        // refuses an image from a buffer without one (edk2-stable202605's image
        // verification handler: EFI_INVALID_PARAMETER for a NULL file path, Secure Boot
        // on or off). The firmware doesn't read the file; the path names the image.
        let mut dp = Vec::new();
        let path = kernel_device_path(&set.kernel_path, &mut dp);
        if path.is_none() {
            crate::log!("no device path for {}: loading without one", set.kernel_path);
        }
        let child = match boot::load_image(boot::image_handle(), LoadImageSource::FromBuffer { buffer: &set.kernel, file_path: path }) {
            Ok(h) => h,
            Err(e) => {
                crate::log!("LoadImage: {:?}", e.status());
                return false;
            }
        };
        match boot::open_protocol_exclusive::<LoadedImage>(child) {
            Ok(mut li) => unsafe { li.set_load_options(opts.as_ptr() as *const u8, (opts.len() * 2) as u32) },
            Err(e) => {
                crate::log!("LoadedImage: {:?}", e.status());
                let _ = boot::unload_image(child);
                return false;
            }
        }
        crate::log!("START_KERNEL {} cmdline=\"{}\"", set.set.id, cmdline);
        let r = boot::start_image(child);
        crate::log!("StartImage returned {:?}", r.as_ref().map_err(|e| e.status()));
        let _ = boot::unload_image(child);
        false
    }
}
