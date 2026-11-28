//! What Jotra last left the OS login item at, in `<data dir>/login-item`
//! ([`jotra_core::login_item`]).

use std::path::{Path, PathBuf};

use jotra_core::login_item::{
    parse_login_item_record, render_login_item_record, LOGIN_ITEM_RECORD_FILE,
};

use crate::errors::StorageResult;

/// Where the record sits inside `dir`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(LOGIN_ITEM_RECORD_FILE)
}

/// The record, or `None` for a file that is not there or does not read.
pub fn read(dir: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(path(dir)).ok()?;
    parse_login_item_record(&text)
}

/// Records that the OS login item was left `on` or off.
pub fn write(dir: &Path, on: bool) -> StorageResult<()> {
    crate::atomic::write_atomic(&path(dir), render_login_item_record(on).as_bytes())?;
    Ok(())
}
