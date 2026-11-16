// SPDX-License-Identifier: GPL-2.0-or-later
//! The loader's own volume through the firmware's FAT driver, as the core's
//! [`Platform`]. Long names are the driver's.
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use glassine_loader_core::plan::{Platform, ReadErr};
use uefi::proto::media::file::{Directory, File, FileAttribute, FileInfo, FileMode, FileType, RegularFile};
use uefi::{CString16, Status};

pub struct Volume {
    pub root: Directory,
}

/// Reads are made in pieces of this size, so the deadline is checked between them.
const PIECE: usize = 1 << 20;

impl Volume {
    fn open(&mut self, path: &str, mode: FileMode) -> Result<RegularFile, Status> {
        let p = CString16::try_from(path).map_err(|_| Status::INVALID_PARAMETER)?;
        let h = self.root.open(&p, mode, FileAttribute::empty()).map_err(|e| e.status())?;
        h.into_regular_file().ok_or(Status::INVALID_PARAMETER)
    }

    fn try_write(&mut self, path: &str, bytes: &[u8]) -> Result<(), Status> {
        let mut f = self.open(path, FileMode::CreateReadWrite)?;
        let size = f.get_boxed_info::<FileInfo>().map_err(|e| e.status())?.file_size();
        if size > bytes.len() as u64 {
            // Longer than it should be (a slot is always 16384 bytes): made again.
            crate::log!("{} is {} bytes: made again", path, size);
            f.delete().map_err(|e| e.status())?;
            f = self.open(path, FileMode::CreateReadWrite)?;
        }
        f.set_position(0).map_err(|e| e.status())?;
        f.write(bytes).map_err(|e| e.status())?;
        f.flush().map_err(|e| e.status())?;
        f.close();
        Ok(())
    }
}

impl Platform for Volume {
    fn read(&mut self, path: &str, max: u64, deadline: Option<u64>) -> Result<Vec<u8>, ReadErr> {
        let mut f = match self.open(path, FileMode::Read) {
            Ok(f) => f,
            Err(Status::NOT_FOUND) => return Err(ReadErr::Missing),
            Err(_) => return Err(ReadErr::Io),
        };
        let size = f.get_boxed_info::<FileInfo>().map_err(|_| ReadErr::Io)?.file_size();
        if size > max {
            return Err(ReadErr::TooLarge(size));
        }
        let mut buf = alloc::vec![0u8; size as usize];
        let mut off = 0usize;
        while off < buf.len() {
            if deadline.is_some_and(|d| crate::arch::now_ms() > d) {
                return Err(ReadErr::TimedOut);
            }
            let end = (off + PIECE).min(buf.len());
            let n = f.read(&mut buf[off..end]).map_err(|_| ReadErr::Io)?;
            if n == 0 {
                return Err(ReadErr::Io);
            }
            off += n;
        }
        Ok(buf)
    }

    fn write_file(&mut self, path: &str, bytes: &[u8]) -> bool {
        match self.try_write(path, bytes) {
            Ok(()) => true,
            Err(s) => {
                crate::log!("write {}: {:?}", path, s);
                false
            }
        }
    }

    fn remove(&mut self, path: &str) -> bool {
        let f = match self.open(path, FileMode::ReadWrite) {
            Ok(f) => f,
            Err(s) => {
                crate::log!("remove {}: {:?}", path, s);
                return false;
            }
        };
        // Delete() closes the handle; a warning (DELETE_FAILURE) means the file stayed.
        if let Err(e) = f.delete() {
            crate::log!("remove {}: {:?}", path, e.status());
            return false;
        }
        if let Err(e) = self.root.flush() {
            crate::log!("flush after removing {}: {:?}", path, e.status());
            return false;
        }
        true
    }

    fn exists(&mut self, path: &str) -> bool {
        self.open(path, FileMode::Read).is_ok()
    }

    fn dirs(&mut self, path: &str) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(p) = CString16::try_from(path) else { return out };
        let Ok(h) = self.root.open(&p, FileMode::Read, FileAttribute::empty()) else { return out };
        let Ok(FileType::Dir(mut d)) = h.into_type() else { return out };
        while let Ok(Some(info)) = d.read_entry_boxed() {
            if !info.is_directory() {
                continue;
            }
            let name = info.file_name().to_string();
            if name != "." && name != ".." {
                out.push(name);
            }
        }
        out
    }

    fn now_ms(&mut self) -> u64 {
        crate::arch::now_ms()
    }

    fn log(&mut self, line: fmt::Arguments) {
        crate::log::line(line);
    }
}
