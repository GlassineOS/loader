//! Reading and removing files on the loader's own volume.
use alloc::vec::Vec;
use uefi::proto::media::file::{Directory, File, FileAttribute, FileInfo, FileMode, RegularFile};
use uefi::{CString16, Status};

pub enum ReadError {
    Missing,
    TooLarge(u64),
    TimedOut,
    Io(Status),
}

fn open(root: &mut Directory, path: &str, mode: FileMode) -> Result<RegularFile, Status> {
    let p = CString16::try_from(path).map_err(|_| Status::INVALID_PARAMETER)?;
    let h = root.open(&p, mode, FileAttribute::empty()).map_err(|e| e.status())?;
    h.into_regular_file().ok_or(Status::INVALID_PARAMETER)
}

/// Reads a whole file, at most `max` bytes, giving up once `deadline_ms` has passed.
pub fn read(root: &mut Directory, path: &str, max: u64, deadline_ms: u64) -> Result<Vec<u8>, ReadError> {
    let mut f = match open(root, path, FileMode::Read) {
        Ok(f) => f,
        Err(Status::NOT_FOUND) => return Err(ReadError::Missing),
        Err(s) => return Err(ReadError::Io(s)),
    };
    let size = f.get_boxed_info::<FileInfo>().map_err(|e| ReadError::Io(e.status()))?.file_size();
    if size > max {
        return Err(ReadError::TooLarge(size));
    }
    let mut buf = alloc::vec![0u8; size as usize];
    let mut off = 0usize;
    while off < buf.len() {
        if crate::time::now_ms() > deadline_ms {
            return Err(ReadError::TimedOut);
        }
        let end = (off + (1 << 20)).min(buf.len());
        let n = f.read(&mut buf[off..end]).map_err(|e| ReadError::Io(e.status()))?;
        if n == 0 {
            return Err(ReadError::Io(Status::END_OF_FILE));
        }
        off += n;
    }
    Ok(buf)
}

pub fn exists(root: &mut Directory, path: &str) -> bool {
    open(root, path, FileMode::Read).is_ok()
}

/// Deletes a file and flushes the volume. Ok only when both succeeded.
pub fn remove_durably(root: &mut Directory, path: &str) -> Result<(), Status> {
    let f = open(root, path, FileMode::ReadWrite)?;
    // Delete() closes the handle; a warning (DELETE_FAILURE) means the file stayed.
    f.delete().map_err(|e| e.status())?;
    root.flush().map_err(|e| e.status())?;
    if exists(root, path) {
        return Err(Status::ABORTED);
    }
    Ok(())
}
