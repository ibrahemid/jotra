//! Open at login: which side moves when `general.launch_at_login` and the OS
//! login item disagree.
//!
//! Jotra records what it last left the OS item at. A config that moved away
//! from that record was edited, so the OS item follows it. A config that still
//! matches the record while the OS item moved was changed in the system's own
//! settings, so the config follows the OS.

use std::path::Path;

use crate::cli_install::is_translocated;

/// The record's file name inside the data folder.
pub const LOGIN_ITEM_RECORD_FILE: &str = "login-item";

const RECORD_ON: &str = "on";
const RECORD_OFF: &str = "off";

/// What [`plan_login_reconcile`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginReconcile {
    /// The config and the OS agree.
    Leave,
    /// The config was edited: make the OS item match it.
    SetOs(bool),
    /// The OS item was changed outside Jotra: write its state to the config.
    FollowOs(bool),
}

/// Which side moves, given what the config asks for, what Jotra last left the
/// OS item at, and what the OS has now. With no record yet, the config wins.
pub fn plan_login_reconcile(wanted: bool, recorded: Option<bool>, os: bool) -> LoginReconcile {
    if wanted == os {
        return LoginReconcile::Leave;
    }
    match recorded {
        Some(recorded) if recorded == wanted => LoginReconcile::FollowOs(os),
        _ => LoginReconcile::SetOs(wanted),
    }
}

/// Reads the record's text. Anything other than the two words is no record.
pub fn parse_login_item_record(text: &str) -> Option<bool> {
    match text.trim() {
        RECORD_ON => Some(true),
        RECORD_OFF => Some(false),
        _ => None,
    }
}

/// The record's text for `on`.
pub fn render_login_item_record(on: bool) -> &'static str {
    if on {
        RECORD_ON
    } else {
        RECORD_OFF
    }
}

/// Whether a login item may point at the app started as `exe`.
///
/// An app opened from a mounted disk image or from App Translocation runs
/// from a place that is gone after the image is ejected or the next launch,
/// so a login item pointing there would start nothing.
pub fn may_point_login_item_at(exe: &Path, on_read_only_volume: bool) -> bool {
    !on_read_only_volume && !is_translocated(exe)
}
