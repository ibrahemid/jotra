//! A folder that takes no new file, the way a read-only mount or a folder the
//! person may not write to refuses one.

use std::path::{Path, PathBuf};

/// Keeps `folder` from taking new files until it is dropped.
pub struct LockedFolder {
    folder: PathBuf,
}

impl LockedFolder {
    /// Locks `folder`, and checks it now refuses a new file.
    pub fn new(folder: &Path) -> Self {
        set_locked(folder, true);
        let locked = Self {
            folder: folder.to_path_buf(),
        };
        assert!(
            std::fs::File::create(folder.join("lock-probe")).is_err(),
            "{} still takes new files",
            folder.display()
        );
        locked
    }
}

impl Drop for LockedFolder {
    fn drop(&mut self) {
        set_locked(&self.folder, false);
    }
}

#[cfg(unix)]
fn set_locked(folder: &Path, locked: bool) {
    use std::os::unix::fs::PermissionsExt;

    let mode = if locked { 0o555 } else { 0o755 };
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode))
        .expect("set the folder's mode");
}

/// Denies Everyone (`S-1-1-0`) adding a file or a folder to `folder`, or takes
/// that denial off again.
#[cfg(windows)]
fn set_locked(folder: &Path, locked: bool) {
    let shown = folder.to_string_lossy();
    let plain = shown
        .strip_prefix(r"\\?\")
        .filter(|rest| !rest.starts_with(r"UNC\"))
        .unwrap_or(&shown);
    let change: [&str; 2] = if locked {
        ["/deny", "*S-1-1-0:(WD,AD)"]
    } else {
        ["/remove:d", "*S-1-1-0"]
    };
    let status = std::process::Command::new("icacls")
        .arg(plain)
        .args(change)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run icacls");
    assert!(
        status.success(),
        "icacls {} {change:?} answered {status}",
        folder.display()
    );
}
