//! Open at login: the OS entry that starts Jotra hidden when the person logs
//! in, so the window chord works after a restart.
//!
//! The entry is made through tauri-plugin-autostart from Rust only; the
//! plugin's own commands are not granted to the webview. `general.launch_at_login`
//! records the choice. The entry is named after the product, so a launch that
//! cannot hold it (a debug build, or an app run from a disk image) keeps the
//! choice in the config only, and a later launch that can makes the entry
//! ([`reconcile_login_item`]).

use std::path::{Path, PathBuf};

use jotra_core::login_item::{may_point_login_item_at, plan_login_reconcile, LoginReconcile};
use jotra_storage::login_item_record;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::{AutoLaunchManager, ManagerExt};
use tracing::{info, warn};

use crate::commands::config::mutate_config;
use crate::commands::offload::run_detached;
use crate::poison::recover_poison;
use crate::state::AppState;

/// The code an IPC failure of these commands carries.
pub const ERR_LOGIN_ITEM: &str = "ERR_LOGIN_ITEM";
/// The code of a change the OS made that config.toml could not record.
pub const ERR_LOGIN_ITEM_UNSAVED: &str = "ERR_LOGIN_ITEM_UNSAVED";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginItemError {
    #[error("{ERR_LOGIN_ITEM}: the OS did not change the login item ({0})")]
    Os(String),
    #[error("{ERR_LOGIN_ITEM}: the choice could not be written to config.toml, so the login item was put back ({0})")]
    Config(String),
    #[error("{ERR_LOGIN_ITEM_UNSAVED}: the login item changed, but config.toml could not record it ({0})")]
    Unsaved(String),
}

/// What the OS is asked about the login item. A trait so the commands can be
/// tested without writing a real one.
pub trait LoginItem {
    fn is_enabled(&self) -> Result<bool, LoginItemError>;
    fn set(&self, on: bool) -> Result<(), LoginItemError>;
}

impl LoginItem for AutoLaunchManager {
    fn is_enabled(&self) -> Result<bool, LoginItemError> {
        AutoLaunchManager::is_enabled(self).map_err(|e| LoginItemError::Os(e.to_string()))
    }

    fn set(&self, on: bool) -> Result<(), LoginItemError> {
        let outcome = if on { self.enable() } else { self.disable() };
        outcome.map_err(|e| LoginItemError::Os(e.to_string()))
    }
}

/// Whether Jotra opens at login, as the OS has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LoginItemStatus {
    pub enabled: bool,
}

/// Whether this launch may change the OS entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginLaunch {
    /// A debug build: the entry it made would start the development binary.
    pub debug_build: bool,
    /// The app runs from a place an entry may point at.
    pub may_point_here: bool,
}

impl LoginLaunch {
    /// A release build run from where it is installed.
    pub const INSTALLED: Self = Self {
        debug_build: false,
        may_point_here: true,
    };

    /// What the running app is.
    pub fn current() -> Self {
        let target = login_item_target();
        Self {
            debug_build: cfg!(debug_assertions),
            may_point_here: target
                .as_deref()
                .is_some_and(|path| may_point_login_item_at(path, on_read_only_volume(path))),
        }
    }

    /// Whether this launch changes the OS entry.
    pub fn holds_entry(self) -> bool {
        !self.debug_build && self.may_point_here
    }
}

/// The path the plugin points the entry at: the AppImage on Linux when there
/// is one, otherwise the executable.
fn login_item_target() -> Option<PathBuf> {
    let appimage = cfg!(target_os = "linux")
        .then(|| std::env::var_os("APPIMAGE"))
        .flatten()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    appimage.or_else(|| std::env::current_exe().ok())
}

#[cfg(unix)]
fn on_read_only_volume(path: &Path) -> bool {
    rustix::fs::statvfs(path)
        .is_ok_and(|stats| stats.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY))
}

#[cfg(not(unix))]
fn on_read_only_volume(_path: &Path) -> bool {
    false
}

fn wanted(state: &AppState) -> bool {
    recover_poison(state.config.lock(), "commands::login_item::wanted")
        .general
        .launch_at_login
}

fn write_wanted(state: &AppState, on: bool) -> Result<bool, String> {
    mutate_config(state, |config| {
        if config.general.launch_at_login == on {
            return false;
        }
        config.general.launch_at_login = on;
        true
    })
}

fn note_left_at(state: &AppState, on: bool) {
    if let Err(error) = login_item_record::write(&state.jotra_dir, on) {
        warn!(%error, "what the login item was left at could not be recorded");
    }
}

/// Asks the OS for the login item, then records the choice. A refusal leaves
/// config.toml as it was; a choice config.toml cannot record puts the OS
/// entry back. A launch that cannot hold the entry records the choice only.
pub fn set_launch_at_login_inner(
    state: &AppState,
    login: &impl LoginItem,
    launch: LoginLaunch,
    on: bool,
) -> Result<LoginItemStatus, LoginItemError> {
    if !launch.holds_entry() {
        write_wanted(state, on).map_err(LoginItemError::Config)?;
        info!(
            on,
            "open at login recorded; this launch leaves the OS entry alone"
        );
        return Ok(LoginItemStatus { enabled: on });
    }
    let was = login.is_enabled().ok();
    login.set(on)?;
    if let Err(error) = write_wanted(state, on) {
        let put_back = was.is_some_and(|was| login.set(was).is_ok());
        return Err(if put_back {
            LoginItemError::Config(error)
        } else {
            LoginItemError::Unsaved(error)
        });
    }
    note_left_at(state, on);
    info!(on, "open at login changed");
    Ok(LoginItemStatus { enabled: on })
}

/// Makes the login item a first launch's config asks for (decision OD-11),
/// answering whether one was made. A launch that cannot hold the entry makes
/// none; the first one that can does ([`reconcile_login_item`]).
pub fn apply_first_run_choice(
    state: &AppState,
    login: &impl LoginItem,
    launch: LoginLaunch,
) -> Result<bool, LoginItemError> {
    if !wanted(state) || !launch.holds_entry() {
        return Ok(false);
    }
    login.set(true)?;
    note_left_at(state, true);
    info!("open at login turned on by the first launch");
    Ok(true)
}

/// Brings the config and the OS entry together: an edited config moves the
/// entry, an entry changed in the system's own settings moves the config.
/// Nothing moves on a first launch, which asks ([`apply_first_run_choice`]),
/// while config.toml has values that did not read, or in a launch that cannot
/// hold the entry.
pub fn reconcile_login_item(
    state: &AppState,
    login: &impl LoginItem,
    launch: LoginLaunch,
) -> Result<LoginReconcile, LoginItemError> {
    let held = !recover_poison(
        state.config_diagnostics.lock(),
        "commands::login_item::reconcile_login_item",
    )
    .is_empty();
    if state.first_run || held || !launch.holds_entry() {
        return Ok(LoginReconcile::Leave);
    }
    let recorded = login_item_record::read(&state.jotra_dir);
    let os = login.is_enabled()?;
    let plan = plan_login_reconcile(wanted(state), recorded, os);
    match plan {
        LoginReconcile::Leave => {
            if recorded != Some(os) {
                note_left_at(state, os);
            }
        }
        LoginReconcile::SetOs(on) => {
            login.set(on)?;
            note_left_at(state, on);
            info!(on, "open at login follows config.toml");
        }
        LoginReconcile::FollowOs(on) => {
            write_wanted(state, on).map_err(LoginItemError::Config)?;
            note_left_at(state, on);
            info!(on, "config.toml follows the login item");
        }
    }
    Ok(plan)
}

/// Runs [`reconcile_login_item`] off the calling thread. A failure is logged
/// and costs the login item only.
pub fn spawn_reconcile(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let login = app.autolaunch();
        if let Err(error) = reconcile_login_item(
            &app.state::<AppState>(),
            login.inner(),
            LoginLaunch::current(),
        ) {
            warn!(%error, "open at login could not be brought in line with config.toml");
        }
    });
}

/// IPC: whether Jotra opens at login. The OS's answer rather than the config's,
/// since the person can remove the entry from the system's own settings; a
/// launch that cannot hold the entry answers the config's.
#[tauri::command]
pub async fn launch_at_login_status(app: AppHandle) -> Result<LoginItemStatus, String> {
    run_detached(ERR_LOGIN_ITEM, move || {
        if !LoginLaunch::current().holds_entry() {
            return Ok(LoginItemStatus {
                enabled: wanted(&app.state::<AppState>()),
            });
        }
        let enabled = app
            .autolaunch()
            .inner()
            .is_enabled()
            .map_err(|e| e.to_string())?;
        Ok(LoginItemStatus { enabled })
    })
    .await
}

/// IPC: turns open at login on or off.
#[tauri::command]
pub async fn set_launch_at_login(app: AppHandle, on: bool) -> Result<LoginItemStatus, String> {
    run_detached(ERR_LOGIN_ITEM, move || {
        let login = app.autolaunch();
        set_launch_at_login_inner(
            &app.state::<AppState>(),
            login.inner(),
            LoginLaunch::current(),
            on,
        )
        .map_err(|e| e.to_string())
    })
    .await
}

/// Runs [`apply_first_run_choice`] off the main thread, after the first
/// launch's answers are written. A failure costs the login item only.
pub async fn apply_first_run_choice_detached(app: AppHandle) {
    let outcome = run_detached(ERR_LOGIN_ITEM, move || {
        let login = app.autolaunch();
        apply_first_run_choice(
            &app.state::<AppState>(),
            login.inner(),
            LoginLaunch::current(),
        )
        .map_err(|e| e.to_string())
    })
    .await;
    if let Err(error) = outcome {
        warn!(%error, "the first launch could not turn on open at login");
    }
}
