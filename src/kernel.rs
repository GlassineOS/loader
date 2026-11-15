//! Starting a kernel set: kernel + initramfs, checked against the record's SHA-256
//! before anything runs, then started from memory. The initramfs is served through
//! the Linux initrd media device path (LoadFile2), so the kernel never reads the ESP
//! again: what was checked is what runs.
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::c_void;
use sha2::{Digest, Sha256};
use uefi::boot::{self, LoadImageSource};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::file::Directory;
use uefi::{Guid, Handle, Status, guid};
use uefi_raw::protocol::device_path::DevicePathProtocol;
use uefi_raw::protocol::media::LoadFile2Protocol;

use crate::fsutil;
use crate::record::{Record, hex};

const LINUX_EFI_INITRD_MEDIA_GUID: Guid = guid!("5568e427-68fc-4f3d-ac74-ca555231cc68");

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

fn install_initrd(data: &'static [u8]) -> uefi::Result<Handle> {
    let path = Box::leak(Box::new(InitrdPath {
        typ: 4,
        sub: 3,
        len: 20u16.to_le_bytes(),
        guid: LINUX_EFI_INITRD_MEDIA_GUID.to_bytes(),
        end: [0x7f, 0xff, 4, 0],
    }));
    let loader = Box::leak(Box::new(InitrdLoader {
        proto: LoadFile2Protocol { load_file: load_initrd },
        data: data.as_ptr(),
        len: data.len(),
    }));
    unsafe {
        let h = boot::install_protocol_interface(None, &DevicePathProtocol::GUID,
                                                 path as *const _ as *const c_void)?;
        boot::install_protocol_interface(Some(h), &LoadFile2Protocol::GUID,
                                         loader as *const _ as *const c_void)?;
        Ok(h)
    }
}

fn load_checked(root: &mut Directory, path: &str, want: &str) -> Result<Vec<u8>, &'static str> {
    let t = crate::time::now_ms();
    let data = fsutil::read(root, path, 512 << 20, u64::MAX).map_err(|_| "unreadable")?;
    let got = hex(&Sha256::digest(&data));
    crate::log!("read+sha256 {} ({} bytes) in {} ms", path, data.len(), crate::time::now_ms() - t);
    if !want.eq_ignore_ascii_case(&got) {
        crate::log!("checksum mismatch for {}: {}", path, got);
        return Err("checksum");
    }
    Ok(data)
}

/// Starts the record's kernel set with `extra` appended to its command line. Returns
/// only if the set could not be started.
pub fn start(root: &mut Directory, rec: &Record, extra: &str) -> Status {
    let kernel = match load_checked(root, &rec.kernel, &rec.kernel_sha256) {
        Ok(k) => k,
        Err(e) => {
            crate::log!("kernel not started: {}", e);
            return Status::LOAD_ERROR;
        }
    };
    let initrd = match load_checked(root, &rec.initrd, &rec.initrd_sha256) {
        Ok(i) => Vec::leak(i),
        Err(e) => {
            crate::log!("kernel not started: initrd {}", e);
            return Status::LOAD_ERROR;
        }
    };
    if let Err(e) = install_initrd(initrd) {
        crate::log!("initrd protocol: {:?}", e.status());
        return e.status();
    }
    let mut cmd = alloc::string::String::from(rec.cmdline.as_str());
    if !extra.is_empty() {
        cmd.push(' ');
        cmd.push_str(extra);
    }
    // UCS-2, NUL-terminated, size in bytes; leaked so it outlives StartImage.
    let opts: &'static [u16] = Vec::leak(cmd.encode_utf16().chain(Some(0)).collect());
    let child = match boot::load_image(boot::image_handle(),
                                       LoadImageSource::FromBuffer { buffer: &kernel, file_path: None }) {
        Ok(h) => h,
        Err(e) => {
            crate::log!("LoadImage: {:?}", e.status());
            return e.status();
        }
    };
    drop(kernel);
    {
        let mut li = match boot::open_protocol_exclusive::<LoadedImage>(child) {
            Ok(l) => l,
            Err(e) => return e.status(),
        };
        unsafe { li.set_load_options(opts.as_ptr() as *const u8, (opts.len() * 2) as u32) };
    }
    crate::log!("START_KERNEL cmdline=\"{}\"", cmd);
    match boot::start_image(child) {
        Ok(()) => Status::SUCCESS,
        Err(e) => {
            crate::log!("StartImage returned {:?}", e.status());
            e.status()
        }
    }
}
