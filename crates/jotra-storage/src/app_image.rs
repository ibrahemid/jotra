//! The AppImage the app last ran from, kept in the data folder so a copy of
//! the command, which has no app beside it, can start the app.

use std::path::{Path, PathBuf};

use crate::atomic::{write_atomic, AtomicWriteError};

/// The file inside the data folder that names the image.
pub const FILE: &str = "app-image";

/// Where the record sits inside `jotra_dir`, the data folder.
pub fn path(jotra_dir: &Path) -> PathBuf {
    jotra_dir.join(FILE)
}

/// Records `image` as the AppImage the app runs from, writing only when the
/// record names another file. Answers whether it wrote.
///
/// # Errors
///
/// [`AtomicWriteError`] when the record cannot be written; an earlier record
/// is left as it was.
pub fn record_app_image(jotra_dir: &Path, image: &Path) -> Result<bool, AtomicWriteError> {
    if recorded_app_image(jotra_dir).as_deref() == Some(image) {
        return Ok(false);
    }
    write_atomic(&path(jotra_dir), &encode(image))?;
    Ok(true)
}

/// The AppImage the record in `jotra_dir` names, when there is one and it
/// names an absolute path.
pub fn recorded_app_image(jotra_dir: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(path(jotra_dir)).ok()?;
    decode(bytes).filter(|image| image.is_absolute())
}

#[cfg(unix)]
fn encode(image: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    image.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn encode(image: &Path) -> Vec<u8> {
    image.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn decode(bytes: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn decode(bytes: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(bytes)
        .ok()
        .filter(|text| !text.is_empty())
        .map(PathBuf::from)
}
