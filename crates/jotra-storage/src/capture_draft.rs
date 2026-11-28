//! The quick capture note's unsent text, in `<data dir>/capture-draft`, so a
//! quit or a relaunch gives the note back what it held (ADR-055).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::errors::StorageResult;

/// The draft's file name inside the data folder.
pub const CAPTURE_DRAFT_FILE: &str = "capture-draft";

/// Where the draft sits inside `dir`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(CAPTURE_DRAFT_FILE)
}

/// The draft, or an empty one for a file that is not there or does not read
/// as UTF-8.
pub fn read(dir: &Path) -> String {
    std::fs::read_to_string(path(dir)).unwrap_or_default()
}

/// Keeps `text` as the draft. A draft with nothing but whitespace removes the
/// file, so a sent note leaves none behind.
pub fn write(dir: &Path, text: &str) -> StorageResult<()> {
    if text.trim().is_empty() {
        return match std::fs::remove_file(path(dir)) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        };
    }
    crate::atomic::write_atomic(&path(dir), text.as_bytes())?;
    Ok(())
}
