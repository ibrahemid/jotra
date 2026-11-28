//! Moves the macOS bundle to its new name before the Tauri builder runs, and
//! runs the per-user steps that follow it (ADR-059 decision 8).
//!
//! `jotra_core::bundle_rename` decides; this module gathers the facts, does
//! the file system work and records each outcome in the name migration record.
//! Nothing here stops the launch: a step that fails is logged and recorded,
//! and the app runs from where it is.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use core_foundation::base::{kCFAllocatorDefault, Boolean, TCFType};
use core_foundation::data::CFData;
use core_foundation::error::{CFError, CFErrorRef};
use core_foundation::propertylist::{
    create_data, create_with_data, kCFPropertyListBinaryFormat_v1_0, kCFPropertyListImmutable,
    CFPropertyList, CFPropertyListRef,
};
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::{CFURLCreateBookmarkData, CFURL};
use jotra_core::bundle_rename::{
    bundle_of_executable, executable_in_bundle, info_plist_path, is_own_login_agent_program,
    is_rename_skipped, per_user_steps_apply, plan_bundle_rename, plan_command_links,
    plan_dock_tiles, plan_login_item, relaunch_arguments, relaunch_environment, sibling_path,
    BundleFacts, BundleInfo, BundlePlan, CommandLinkFacts, CommandLinkPlan, DockFacts, DockPlan,
    DockTile, DockTileAction, LegacyLoginAgent, Privileges, RenamePlan, SiblingState,
    BUNDLE_IDENTIFIER, DOCK_TILE_LABEL, DOCK_URL_STRING, LAUNCH_AGENTS_FOLDER, SKIP_VAR,
};
use jotra_core::cli_install::{COMMAND_NAME, SYSTEM_LINK};
use jotra_core::legacy_names;
use jotra_core::name_migration::{MigrationStep, NameMigrationRecord};
use jotra_storage::errors::{StorageError, StorageResult};
use jotra_storage::instance::InstanceLock;
use jotra_storage::launchpad::{retitle_launchpad_app, LaunchpadRetitle, LaunchpadUpdate};
use jotra_storage::name_migration::{
    read_name_migration_record, run_migration_step, StepCompletion, StepRun,
};
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;
use tracing::{info, warn};

use crate::commands::cli::{applescript_escape, inspect_entry, link_directly, shell_single_quote};
use crate::commands::login_item::{LoginItem, LoginItemError};
use crate::commands::update::dir_is_writable;
use crate::logging::LogGuard;
use crate::poison::recover_poison;
use crate::state::AppState;

const OSASCRIPT: &str = "/usr/bin/osascript";
const OPEN: &str = "/usr/bin/open";
const KILLALL: &str = "/usr/bin/killall";
const DOCK_PROCESS: &str = "Dock";
const DOCK_DOMAIN: &str = "com.apple.dock";
const DOCK_PINNED_KEY: &str = "persistent-apps";
const DOCK_RECENT_KEY: &str = "recent-apps";
const TILE_GUID_KEY: &str = "GUID";
const TILE_DATA_KEY: &str = "tile-data";
const TILE_BOOKMARK_KEY: &str = "book";
const TILE_LABEL_KEY: &str = "file-label";
const TILE_FILE_DATA_KEY: &str = "file-data";
const TILE_URL_KEY: &str = "_CFURLString";
const TILE_URL_TYPE_KEY: &str = "_CFURLStringType";
const DOCK_TILES_OPERATION: &str = "dock tiles";
const LAUNCHPAD_OPERATION: &str = "launchpad retitle";
/// The Launchpad file inside this user's `_CS_DARWIN_USER_DIR` folder.
const LAUNCHPAD_DATABASE: &str = "com.apple.dock.launchpad/db/db";

/// Why a step of the bundle rename or the per-user steps did not finish.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BundleRenameError {
    /// A file system call failed.
    #[error("{operation} {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A bundle's `Info.plist` does not read.
    #[error("{} does not read: {reason}", path.display())]
    InfoPlist { path: PathBuf, reason: String },
    /// The rename under an administrator's password failed.
    #[error("the administrator rename failed: {0}")]
    Administrator(String),
    /// A bundle did not move to the Trash.
    #[error("{} did not move to the Trash: {reason}", path.display())]
    Trash { path: PathBuf, reason: String },
    /// Launch Services refused the bundle.
    #[error("Launch Services did not register {}: status {status}", path.display())]
    Register { path: PathBuf, status: i32 },
    /// A bundle did not open.
    #[error("{} did not open: {reason}", path.display())]
    Open { path: PathBuf, reason: String },
    /// A command or the link to it does not lead to a file.
    #[error("{} does not lead to a file", path.display())]
    Unresolved { path: PathBuf },
    /// The login item service refused.
    #[error("the login item: {0}")]
    LoginItem(#[from] LoginItemError),
    /// A Dock setting cannot be read or written as it stands.
    #[error("the Dock setting {key} {problem}")]
    DockSetting {
        key: &'static str,
        problem: DockSettingProblem,
    },
    /// The Dock's settings did not save.
    #[error("the Dock settings did not save")]
    DockNotSaved,
    /// A Dock setting would lose or gain tiles the plan does not drop.
    #[error("the Dock setting {key} would hold {actual} tiles, not {expected}")]
    DockTileCount {
        key: &'static str,
        expected: usize,
        actual: usize,
    },
    /// The Dock did not restart.
    #[error("the Dock did not restart: {reason}")]
    DockRestart { reason: String },
}

/// Why a Dock setting is left as it is.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DockSettingProblem {
    /// A management profile sets it.
    #[error("is managed")]
    Managed,
    /// It holds something other than a list.
    #[error("is not a list")]
    NotList,
    /// It does not convert to or from a property list.
    #[error("does not convert: {0}")]
    Conversion(String),
    /// It reads back different after a conversion there and back.
    #[error("changes when converted")]
    RoundTrip,
}

fn dock_setting(key: &'static str, problem: DockSettingProblem) -> BundleRenameError {
    BundleRenameError::DockSetting { key, problem }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> BundleRenameError {
    BundleRenameError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

/// The system calls the rename makes that tests cannot make on this machine.
pub(crate) trait BundleHost {
    /// Moves `path` to the Trash.
    fn trash(&self, path: &Path) -> Result<(), BundleRenameError>;
    /// Renames `from` to `to` and leaves the hidden link at `from`, under an
    /// administrator's password. A cancelled prompt is a decline.
    fn rename_as_administrator(
        &self,
        from: &Path,
        to: &Path,
    ) -> Result<StepCompletion, BundleRenameError>;
    /// Registers `bundle` with Launch Services.
    fn register(&self, bundle: &Path) -> Result<(), BundleRenameError>;
    /// Opens a new instance of `bundle` with the variables `env` and `args`.
    fn open(
        &self,
        bundle: &Path,
        env: &[(OsString, OsString)],
        args: &[OsString],
    ) -> Result<(), BundleRenameError>;
}

/// The host that makes the real calls.
pub(crate) struct MacHost;

#[allow(non_snake_case)]
#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn LSRegisterURL(inURL: core_foundation::url::CFURLRef, inUpdate: u8) -> i32;
}

impl BundleHost for MacHost {
    fn trash(&self, path: &Path) -> Result<(), BundleRenameError> {
        use objc2_foundation::{NSFileManager, NSString, NSURL};

        let text = path.to_str().ok_or_else(|| BundleRenameError::Trash {
            path: path.to_path_buf(),
            reason: "the path is not UTF-8".to_string(),
        })?;
        objc2::rc::autoreleasepool(|_| {
            let url = NSURL::fileURLWithPath(&NSString::from_str(text));
            NSFileManager::defaultManager()
                .trashItemAtURL_resultingItemURL_error(&url, None)
                .map_err(|error| BundleRenameError::Trash {
                    path: path.to_path_buf(),
                    reason: error.localizedDescription().to_string(),
                })
        })
    }

    fn rename_as_administrator(
        &self,
        from: &Path,
        to: &Path,
    ) -> Result<StepCompletion, BundleRenameError> {
        let script = administrator_rename_script(from, to)?;
        let output = Command::new(OSASCRIPT)
            .arg("-e")
            .arg(&script)
            .output()
            .map_err(|source| io_error("run", Path::new(OSASCRIPT), source))?;
        if output.status.success() {
            return Ok(StepCompletion::Done);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_cancelled(&stderr) {
            Ok(StepCompletion::Declined)
        } else {
            Err(BundleRenameError::Administrator(stderr.trim().to_string()))
        }
    }

    fn register(&self, bundle: &Path) -> Result<(), BundleRenameError> {
        use core_foundation::base::TCFType;
        use core_foundation::url::CFURL;

        let url = CFURL::from_path(bundle, true).ok_or_else(|| BundleRenameError::Register {
            path: bundle.to_path_buf(),
            status: -1,
        })?;
        let status = unsafe { LSRegisterURL(url.as_concrete_TypeRef(), 1) };
        if status == 0 {
            Ok(())
        } else {
            Err(BundleRenameError::Register {
                path: bundle.to_path_buf(),
                status,
            })
        }
    }

    fn open(
        &self,
        bundle: &Path,
        env: &[(OsString, OsString)],
        args: &[OsString],
    ) -> Result<(), BundleRenameError> {
        let status = Command::new(OPEN)
            .args(open_arguments(bundle, env, args))
            .status()
            .map_err(|source| io_error("run", Path::new(OPEN), source))?;
        if status.success() {
            Ok(())
        } else {
            Err(BundleRenameError::Open {
                path: bundle.to_path_buf(),
                reason: status.to_string(),
            })
        }
    }
}

/// The error number AppleScript reports for a cancelled prompt, as osascript
/// prints it.
const CANCELLED_TOKEN: &str = "(-128)";

/// Whether osascript's error output is a cancelled password prompt.
fn is_cancelled(stderr: &str) -> bool {
    stderr
        .split_whitespace()
        .any(|token| token == CANCELLED_TOKEN)
}

/// The arguments `open` takes to start a new instance of `bundle` with the
/// variables `env` and `args`.
pub(crate) fn open_arguments(
    bundle: &Path,
    env: &[(OsString, OsString)],
    args: &[OsString],
) -> Vec<OsString> {
    let mut all = vec![OsString::from("-n")];
    for (name, value) in env {
        let mut entry = name.clone();
        entry.push("=");
        entry.push(value);
        all.push(OsString::from("--env"));
        all.push(entry);
    }
    all.push(OsString::from("-a"));
    all.push(bundle.as_os_str().to_os_string());
    if !args.is_empty() {
        all.push(OsString::from("--args"));
        all.extend(args.iter().cloned());
    }
    all
}

/// The AppleScript that renames `from` to `to` and leaves the hidden link at
/// `from`, in one administrator prompt. It moves nothing when `to` exists.
pub(crate) fn administrator_rename_script(
    from: &Path,
    to: &Path,
) -> Result<String, BundleRenameError> {
    let quoted_to = shell_single_quote(&to.to_string_lossy());
    let shell = format!(
        "test ! -e {quoted_to} && test ! -L {quoted_to} && {}",
        move_and_link_shell(from, to)?
    );
    Ok(format!(
        "do shell script \"{}\" with administrator privileges",
        applescript_escape(&shell)
    ))
}

/// The shell commands that move `from` to `to` and leave the hidden link at
/// `from`. A move that lands inside a folder made at `to` after the check is
/// moved back and fails, and the link is left only once `to` holds the
/// bundle's `Info.plist`.
pub(crate) fn move_and_link_shell(from: &Path, to: &Path) -> Result<String, BundleRenameError> {
    let target = link_target(to)?;
    let nested = to.join(link_target(from)?);
    let plist = info_plist_path(to);
    let [from, to, target, nested, plist] = [from, to, target, nested.as_path(), plist.as_path()]
        .map(|path| shell_single_quote(&path.to_string_lossy()));
    Ok(format!(
        "/bin/mv {from} {to} && \
         if test -e {nested} || test -L {nested}; then \
         test ! -e {from} && test ! -L {from} && /bin/mv {nested} {from}; exit 1; \
         fi && \
         test -f {plist} && /bin/ln -s {target} {from} && /usr/bin/chflags -h hidden {from}"
    ))
}

/// The relative target of the link left beside `bundle`: its folder name.
fn link_target(bundle: &Path) -> Result<&Path, BundleRenameError> {
    bundle.file_name().map(Path::new).ok_or_else(|| {
        io_error(
            "name the link target",
            bundle,
            io::ErrorKind::InvalidInput.into(),
        )
    })
}

/// The identifier and version in `bundle`'s `Info.plist`.
pub(crate) fn read_bundle_info(bundle: &Path) -> Result<BundleInfo, BundleRenameError> {
    let path = info_plist_path(bundle);
    let invalid = |reason: String| BundleRenameError::InfoPlist {
        path: path.clone(),
        reason,
    };
    let value = plist::Value::from_file(&path).map_err(|error| invalid(error.to_string()))?;
    let dictionary = value
        .as_dictionary()
        .ok_or_else(|| invalid("the top level is not a dictionary".to_string()))?;
    let field = |key: &str| {
        dictionary
            .get(key)
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
            .ok_or_else(|| invalid(format!("{key} is missing")))
    };
    Ok(BundleInfo {
        identifier: field("CFBundleIdentifier")?,
        version: field("CFBundleShortVersionString")?,
    })
}

fn inspect_sibling(path: &Path) -> SiblingState {
    match path.symlink_metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => SiblingState::Absent,
        Err(_) => SiblingState::Unreadable,
        Ok(metadata) if metadata.file_type().is_symlink() => SiblingState::Link,
        Ok(metadata) if metadata.is_dir() => read_bundle_info(path)
            .map(SiblingState::Bundle)
            .unwrap_or(SiblingState::Unreadable),
        Ok(_) => SiblingState::Unreadable,
    }
}

fn runs_through_link(exe: &Path) -> bool {
    exe.ancestors().any(|ancestor| {
        ancestor
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
    })
}

fn is_read_only_volume(path: &Path) -> bool {
    rustix::fs::statvfs(path)
        .is_ok_and(|stats| stats.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY))
}

/// What the launch started as `exe` knows about its bundle.
pub(crate) fn gather_facts(
    exe: &Path,
    record: NameMigrationRecord,
    skip_requested: bool,
) -> BundleFacts {
    let bundle = bundle_of_executable(exe);
    let running = bundle.as_deref().and_then(|bundle| {
        read_bundle_info(bundle)
            .inspect_err(|error| warn!(operation = "read bundle info", %error))
            .ok()
    });
    BundleFacts {
        exe_through_link: runs_through_link(exe),
        canonical_bundle: exe
            .canonicalize()
            .ok()
            .as_deref()
            .and_then(bundle_of_executable),
        sibling: bundle
            .as_deref()
            .and_then(sibling_path)
            .map_or(SiblingState::Absent, |sibling| inspect_sibling(&sibling)),
        read_only_volume: bundle.as_deref().is_some_and(is_read_only_volume),
        folder_writable: bundle
            .as_deref()
            .and_then(Path::parent)
            .is_some_and(dir_is_writable),
        bundle,
        running,
        skip_requested,
        record,
    }
}

/// Renames `from` to `to`, failing rather than replacing anything at `to`.
pub(crate) fn rename_exclusive(from: &Path, to: &Path) -> Result<(), BundleRenameError> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|errno| io_error("rename", from, errno.into()))
}

/// Creates a link at `link` to the relative `target` and hides the link
/// itself from the Finder.
pub(crate) fn leave_hidden_link(link: &Path, target: &Path) -> Result<(), BundleRenameError> {
    std::os::unix::fs::symlink(target, link).map_err(|source| io_error("link", link, source))?;
    let path = CString::new(link.as_os_str().as_bytes())
        .map_err(|_| io_error("hide", link, io::ErrorKind::InvalidInput.into()))?;
    let status = unsafe { libc::lchflags(path.as_ptr(), libc::UF_HIDDEN) };
    if status == 0 {
        Ok(())
    } else {
        Err(io_error("hide", link, io::Error::last_os_error()))
    }
}

/// What the launch does once the plan has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Execution {
    /// Carry on starting from this bundle.
    Continue,
    /// Open this bundle and end this launch.
    Relaunch(PathBuf),
}

fn rename_bundle(
    rename: &RenamePlan,
    host: &impl BundleHost,
) -> Result<StepCompletion, BundleRenameError> {
    if rename.trash_older {
        host.trash(&rename.to)?;
    }
    match rename.privileges {
        Privileges::Administrator => host.rename_as_administrator(&rename.from, &rename.to),
        Privileges::User => {
            rename_exclusive(&rename.from, &rename.to)?;
            Ok(StepCompletion::Done)
        }
    }
}

/// Leaves the hidden link to `bundle` at `link`, or finds it there already.
fn restore_link(link: &Path, bundle: &Path) -> Result<StepCompletion, BundleRenameError> {
    let target = link_target(bundle)?;
    if std::fs::read_link(link).is_ok_and(|existing| existing == target) {
        return Ok(StepCompletion::Done);
    }
    leave_hidden_link(link, target)?;
    Ok(StepCompletion::Done)
}

fn is_real_folder(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
}

fn is_absent(path: &Path) -> bool {
    path.symlink_metadata()
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
}

/// Carries out `plan` for app `version`, recording each step's outcome in the
/// record in `data_dir`: the move under the rename's entry and the hidden
/// link under its own.
///
/// A rename that moved the bundle relaunches from the new name even when the
/// link or the registration after it failed: the old path no longer holds a
/// bundle to carry on from.
pub(crate) fn execute_plan(
    plan: &BundlePlan,
    host: &impl BundleHost,
    data_dir: &Path,
    version: &str,
) -> Execution {
    match plan {
        BundlePlan::OpenCanonical(bundle) | BundlePlan::OpenSibling(bundle) => {
            Execution::Relaunch(bundle.clone())
        }
        BundlePlan::Rename(rename) => {
            run_migration_step(data_dir, MigrationStep::BundleRename, version, || {
                rename_bundle(rename, host)
            });
            if is_real_folder(&rename.from) || !is_real_folder(&rename.to) {
                return Execution::Continue;
            }
            run_migration_step(data_dir, MigrationStep::BundleLink, version, || {
                restore_link(&rename.from, &rename.to)
            });
            if let Err(error) = host.register(&rename.to) {
                warn!(operation = "register bundle", version, %error);
            }
            Execution::Relaunch(rename.to.clone())
        }
        BundlePlan::ReplaceLegacy { legacy, bundle } => {
            run_migration_step(data_dir, MigrationStep::BundleRename, version, || {
                host.trash(legacy).map(|()| StepCompletion::Done)
            });
            if is_absent(legacy) {
                run_migration_step(data_dir, MigrationStep::BundleLink, version, || {
                    restore_link(legacy, bundle)
                });
            }
            Execution::Continue
        }
        BundlePlan::RestoreLink { link, bundle } => {
            run_migration_step(data_dir, MigrationStep::BundleLink, version, || {
                restore_link(link, bundle)
            });
            Execution::Continue
        }
    }
}

/// What the per-user steps need from the launch that runs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PerUserSteps {
    bundle: PathBuf,
    version: String,
    data_dir: PathBuf,
}

fn per_user_steps(facts: &BundleFacts, data_dir: &Path) -> Option<PerUserSteps> {
    if !per_user_steps_apply(facts) {
        return None;
    }
    Some(PerUserSteps {
        bundle: facts.bundle.clone()?,
        version: facts.running.as_ref()?.version.clone(),
        data_dir: data_dir.to_path_buf(),
    })
}

/// Plans and carries out the rename for the launch started as `exe`, and
/// whether the per-user steps run once the app is set up.
pub(crate) fn plan_and_execute(
    exe: &Path,
    data_dir: &Path,
    skip_requested: bool,
    host: &impl BundleHost,
) -> (Execution, Option<PerUserSteps>) {
    let facts = gather_facts(exe, read_name_migration_record(data_dir), skip_requested);
    let per_user = per_user_steps(&facts, data_dir);
    let plan = match plan_bundle_rename(&facts) {
        Ok(plan) => plan,
        Err(reason) => {
            info!(%reason, "the bundle stays where it is");
            return (Execution::Continue, per_user);
        }
    };
    info!(?plan, "bundle rename");
    let version = facts
        .running
        .as_ref()
        .map_or(env!("CARGO_PKG_VERSION"), |running| {
            running.version.as_str()
        });
    (execute_plan(&plan, host, data_dir, version), per_user)
}

/// Runs the bundle rename at launch, in a release build, with the data folder
/// `jotra_dir` locked by `lock`.
///
/// A plan that opens another bundle releases the lock first, so the new
/// process does not hand its launch back to this one, and ends the process
/// once `open` has answered. When `open` fails the lock is claimed again and
/// the launch carries on here.
pub(crate) fn rename_at_launch(
    jotra_dir: &Path,
    lock: Option<InstanceLock>,
    log_guard: &mut LogGuard,
) -> (Option<InstanceLock>, Option<PerUserSteps>) {
    if cfg!(debug_assertions) {
        return (lock, None);
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            warn!(operation = "bundle rename", %error, "the executable's path is unknown");
            return (lock, None);
        }
    };
    let skip = is_rename_skipped(std::env::var_os(SKIP_VAR).as_deref());
    let host = MacHost;
    let (execution, per_user) = plan_and_execute(&exe, jotra_dir, skip, &host);
    let Execution::Relaunch(bundle) = execution else {
        return (lock, per_user);
    };
    let env = relaunch_environment(std::env::vars_os());
    let args = relaunch_arguments(std::env::args_os().skip(1));
    match relaunch(&host, &bundle, &env, &args, lock, || {
        reclaim_lock(jotra_dir)
    }) {
        RelaunchOutcome::Opened => {
            info!(bundle = %bundle.display(), "opened the bundle; this launch ends");
            log_guard.flush();
            std::process::exit(0);
        }
        RelaunchOutcome::CarriedOn(lock) => (lock, per_user),
    }
}

/// How a relaunch ended for the launch that asked for it.
pub(crate) enum RelaunchOutcome {
    /// The bundle opened; this launch ends.
    Opened,
    /// The bundle did not open; the launch carries on here with this lock.
    CarriedOn(Option<InstanceLock>),
}

/// Opens `bundle` with `env` and `args`, releasing `lock` first so the new
/// process does not hand its launch back to this one. When `open` fails and
/// the lock was held, `reclaim` claims it again.
pub(crate) fn relaunch(
    host: &impl BundleHost,
    bundle: &Path,
    env: &[(OsString, OsString)],
    args: &[OsString],
    lock: Option<InstanceLock>,
    reclaim: impl FnOnce() -> Option<InstanceLock>,
) -> RelaunchOutcome {
    let held = lock.is_some();
    drop(lock);
    match host.open(bundle, env, args) {
        Ok(()) => RelaunchOutcome::Opened,
        Err(error) => {
            warn!(operation = "open bundle", %error, "the launch carries on here");
            RelaunchOutcome::CarriedOn(if held { reclaim() } else { None })
        }
    }
}

fn reclaim_lock(jotra_dir: &Path) -> Option<InstanceLock> {
    crate::instance::claim_or_hand_over(jotra_dir)
        .inspect_err(|error| warn!(%error, "running without the data folder lock"))
        .ok()
}

/// What sits at `agent`, the login agent's path under the old name, judged by
/// the program it starts. An agent that does not read is not the app's own.
fn inspect_login_agent(agent: &Path, app_binary_name: Option<&OsStr>) -> LegacyLoginAgent {
    match agent.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return LegacyLoginAgent::Absent,
        Err(_) => return LegacyLoginAgent::Foreign,
    }
    let program = plist::Value::from_file(agent)
        .ok()
        .as_ref()
        .and_then(login_agent_program);
    match program {
        Some(program) if is_own_login_agent_program(&program, app_binary_name) => {
            LegacyLoginAgent::Own
        }
        _ => LegacyLoginAgent::Foreign,
    }
}

/// The program a launch agent starts: its `Program`, or else the first of its
/// `ProgramArguments`.
fn login_agent_program(agent: &plist::Value) -> Option<PathBuf> {
    let dictionary = agent.as_dictionary()?;
    dictionary
        .get("Program")
        .and_then(plist::Value::as_string)
        .or_else(|| {
            dictionary
                .get("ProgramArguments")
                .and_then(plist::Value::as_array)
                .and_then(|arguments| arguments.first())
                .and_then(plist::Value::as_string)
        })
        .map(PathBuf::from)
}

/// Removes the login item made under the old name when it starts this app
/// and, when the config asks for one, registers it under the new name. An
/// agent at the old name that starts something else stays, as a decline.
pub(crate) fn migrate_login_item(
    login: &impl LoginItem,
    legacy_agent: &Path,
    launch_at_login: bool,
    app_binary_name: Option<&OsStr>,
) -> Result<StepCompletion, BundleRenameError> {
    let plan = plan_login_item(
        inspect_login_agent(legacy_agent, app_binary_name),
        launch_at_login,
    );
    if plan.remove_legacy_agent {
        match std::fs::remove_file(legacy_agent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("remove", legacy_agent, source)),
        }
    }
    if plan.register {
        login.set(true)?;
    }
    if plan.keep_foreign_agent {
        return Ok(StepCompletion::Declined);
    }
    Ok(StepCompletion::Done)
}

/// Points `link` at `source`, failing when `source` is not a file or the link
/// does not lead to one afterwards.
fn link_command(source: &Path, link: &Path) -> Result<(), BundleRenameError> {
    if !source.is_file() {
        return Err(BundleRenameError::Unresolved {
            path: source.to_path_buf(),
        });
    }
    link_directly(source, link).map_err(|error| io_error("link", link, error))?;
    if link.is_file() {
        Ok(())
    } else {
        Err(BundleRenameError::Unresolved {
            path: link.to_path_buf(),
        })
    }
}

/// Points the command's legacy name in `folder` at `bundle` and adds the
/// command's own name beside it, when the legacy name is Jotra's command, a
/// link left dangling by the rename among them, and the folder is writable
/// without a prompt. A folder that needs a password is a decline: the install
/// in Settings asks for it. So is a bundle the rename stays out of, which no
/// link is pointed into.
pub(crate) fn migrate_command_links(
    folder: &Path,
    bundle: &Path,
    app_binary_name: Option<&OsStr>,
) -> Result<StepCompletion, BundleRenameError> {
    let legacy = folder.join(legacy_names::COMMAND_NAME);
    let current = folder.join(COMMAND_NAME);
    let facts = CommandLinkFacts {
        legacy: inspect_entry(&legacy, app_binary_name),
        current: inspect_entry(&current, app_binary_name),
        folder_writable: dir_is_writable(folder),
        bundle: bundle.to_path_buf(),
        bundle_read_only: is_read_only_volume(bundle),
    };
    match plan_command_links(&facts) {
        CommandLinkPlan::BundleGuarded | CommandLinkPlan::NeedsPermission => {
            Ok(StepCompletion::Declined)
        }
        CommandLinkPlan::Untouched => Ok(StepCompletion::Done),
        CommandLinkPlan::Relink { add_current } => {
            let command = executable_in_bundle(bundle, COMMAND_NAME);
            let legacy_command = executable_in_bundle(bundle, legacy_names::COMMAND_NAME);
            let legacy_source = if legacy_command.is_file() {
                &legacy_command
            } else {
                &command
            };
            link_command(legacy_source, &legacy)?;
            if add_current {
                link_command(&command, &current)?;
            }
            Ok(StepCompletion::Done)
        }
    }
}

/// The calls the Dock step makes that tests cannot make on this machine.
pub(crate) trait DockHost {
    /// The tiles under `key` in this user's Dock settings, or `None` when the
    /// key is absent. A key a management profile sets, or one that does not
    /// read back unchanged, is an error.
    fn read_tiles(&self, key: &'static str)
        -> Result<Option<Vec<plist::Value>>, BundleRenameError>;
    /// Sets each key to its tiles in this user's Dock settings and saves them.
    fn write_tiles(
        &self,
        tiles: &[(&'static str, Vec<plist::Value>)],
    ) -> Result<(), BundleRenameError>;
    /// Restarts the Dock, which reads its settings only when it starts.
    fn restart(&self) -> Result<(), BundleRenameError>;
    /// This user's `_CS_DARWIN_USER_DIR` folder, or `None` when the system
    /// gives none.
    fn user_folder(&self) -> Option<PathBuf>;
    /// The bookmark data of `bundle` with the default options, or `None` when
    /// the system makes none.
    fn bookmark(&self, bundle: &Path) -> Option<Vec<u8>>;
    /// Gives the Launchpad row in `database` the new title and bookmark.
    fn retitle_launchpad(
        &self,
        database: &Path,
        retitle: &LaunchpadRetitle<'_>,
    ) -> StorageResult<LaunchpadUpdate>;
}

/// The Dock host that makes the real calls.
pub(crate) struct MacDock;

#[allow(non_upper_case_globals)]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFPreferencesCurrentUser: CFStringRef;
    static kCFPreferencesAnyHost: CFStringRef;
    fn CFPreferencesCopyValue(
        key: CFStringRef,
        application_id: CFStringRef,
        user_name: CFStringRef,
        host_name: CFStringRef,
    ) -> CFPropertyListRef;
    fn CFPreferencesSetValue(
        key: CFStringRef,
        value: CFPropertyListRef,
        application_id: CFStringRef,
        user_name: CFStringRef,
        host_name: CFStringRef,
    );
    fn CFPreferencesSynchronize(
        application_id: CFStringRef,
        user_name: CFStringRef,
        host_name: CFStringRef,
    ) -> Boolean;
    fn CFPreferencesAppValueIsForced(key: CFStringRef, application_id: CFStringRef) -> Boolean;
}

impl DockHost for MacDock {
    fn read_tiles(
        &self,
        key: &'static str,
    ) -> Result<Option<Vec<plist::Value>>, BundleRenameError> {
        let domain = CFString::new(DOCK_DOMAIN);
        let name = CFString::new(key);
        let managed = unsafe {
            CFPreferencesAppValueIsForced(name.as_concrete_TypeRef(), domain.as_concrete_TypeRef())
        };
        if managed != 0 {
            return Err(dock_setting(key, DockSettingProblem::Managed));
        }
        let stored = unsafe {
            CFPreferencesCopyValue(
                name.as_concrete_TypeRef(),
                domain.as_concrete_TypeRef(),
                kCFPreferencesCurrentUser,
                kCFPreferencesAnyHost,
            )
        };
        if stored.is_null() {
            return Ok(None);
        }
        let stored = unsafe { CFPropertyList::wrap_under_create_rule(stored) };
        tiles_from_property_list(key, &stored).map(Some)
    }

    fn write_tiles(
        &self,
        tiles: &[(&'static str, Vec<plist::Value>)],
    ) -> Result<(), BundleRenameError> {
        let lists = tiles
            .iter()
            .map(|(key, tiles)| {
                property_list_from_value(&plist::Value::Array(tiles.clone()))
                    .map(|list| (CFString::new(key), list))
                    .map_err(|reason| dock_setting(key, DockSettingProblem::Conversion(reason)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let domain = CFString::new(DOCK_DOMAIN);
        for (name, list) in &lists {
            unsafe {
                CFPreferencesSetValue(
                    name.as_concrete_TypeRef(),
                    list.as_concrete_TypeRef(),
                    domain.as_concrete_TypeRef(),
                    kCFPreferencesCurrentUser,
                    kCFPreferencesAnyHost,
                );
            }
        }
        let saved = unsafe {
            CFPreferencesSynchronize(
                domain.as_concrete_TypeRef(),
                kCFPreferencesCurrentUser,
                kCFPreferencesAnyHost,
            )
        };
        if saved == 0 {
            return Err(BundleRenameError::DockNotSaved);
        }
        Ok(())
    }

    fn restart(&self) -> Result<(), BundleRenameError> {
        let output = Command::new(KILLALL)
            .arg(DOCK_PROCESS)
            .output()
            .map_err(|source| io_error("run", Path::new(KILLALL), source))?;
        if output.status.success() {
            return Ok(());
        }
        Err(BundleRenameError::DockRestart {
            reason: format!(
                "{} {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )
            .trim()
            .to_string(),
        })
    }

    fn user_folder(&self) -> Option<PathBuf> {
        let length = unsafe { libc::confstr(libc::_CS_DARWIN_USER_DIR, std::ptr::null_mut(), 0) };
        if length == 0 {
            return None;
        }
        let mut buffer = vec![0_u8; length];
        let written = unsafe {
            libc::confstr(
                libc::_CS_DARWIN_USER_DIR,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if written == 0 || written > buffer.len() {
            return None;
        }
        let folder = CStr::from_bytes_until_nul(&buffer).ok()?.to_bytes();
        if folder.is_empty() {
            return None;
        }
        Some(PathBuf::from(OsStr::from_bytes(folder)))
    }

    fn bookmark(&self, bundle: &Path) -> Option<Vec<u8>> {
        let url = CFURL::from_path(bundle, true)?;
        let mut error: CFErrorRef = std::ptr::null_mut();
        let data = unsafe {
            CFURLCreateBookmarkData(
                kCFAllocatorDefault,
                url.as_concrete_TypeRef(),
                0,
                std::ptr::null(),
                std::ptr::null(),
                &mut error,
            )
        };
        if !error.is_null() {
            drop(unsafe { CFError::wrap_under_create_rule(error) });
        }
        if data.is_null() {
            return None;
        }
        let data = unsafe { CFData::wrap_under_create_rule(data) };
        Some(data.bytes().to_vec()).filter(|bytes| !bytes.is_empty())
    }

    fn retitle_launchpad(
        &self,
        database: &Path,
        retitle: &LaunchpadRetitle<'_>,
    ) -> StorageResult<LaunchpadUpdate> {
        retitle_launchpad_app(database, retitle)
    }
}

fn cf_error_text(error: &core_foundation::error::CFError) -> String {
    error.description().to_string()
}

/// `value` as a Core Foundation property list, through its binary encoding.
fn property_list_from_value(value: &plist::Value) -> Result<CFPropertyList, String> {
    let mut bytes = Vec::new();
    value
        .to_writer_binary(&mut bytes)
        .map_err(|error| error.to_string())?;
    let (list, _) = create_with_data(CFData::from_buffer(&bytes), kCFPropertyListImmutable)
        .map_err(|error| cf_error_text(&error))?;
    Ok(unsafe { CFPropertyList::wrap_under_create_rule(list) })
}

/// `list` as a property list value, through its binary encoding.
fn value_from_property_list(list: &CFPropertyList) -> Result<plist::Value, String> {
    let data = create_data(list.as_concrete_TypeRef(), kCFPropertyListBinaryFormat_v1_0)
        .map_err(|error| cf_error_text(&error))?;
    plist::Value::from_reader(io::Cursor::new(data.bytes())).map_err(|error| error.to_string())
}

/// The tiles `stored` under `key` holds, only when it is a list that converts
/// back to exactly `stored`, so writing the tiles back keeps every key of every
/// tile the step does not edit.
fn tiles_from_property_list(
    key: &'static str,
    stored: &CFPropertyList,
) -> Result<Vec<plist::Value>, BundleRenameError> {
    let conversion = |reason| dock_setting(key, DockSettingProblem::Conversion(reason));
    let value = value_from_property_list(stored).map_err(conversion)?;
    if value.as_array().is_none() {
        return Err(dock_setting(key, DockSettingProblem::NotList));
    }
    if property_list_from_value(&value).map_err(conversion)? != *stored {
        return Err(dock_setting(key, DockSettingProblem::RoundTrip));
    }
    value
        .into_array()
        .ok_or_else(|| dock_setting(key, DockSettingProblem::NotList))
}

fn tile_file_data(tile: &plist::Value) -> Option<&plist::Dictionary> {
    tile.as_dictionary()?
        .get(TILE_DATA_KEY)?
        .as_dictionary()?
        .get(TILE_FILE_DATA_KEY)?
        .as_dictionary()
}

/// Where `tile` leads, as the Dock plan reads it.
fn dock_tile(tile: &plist::Value) -> DockTile {
    let file_data = tile_file_data(tile);
    DockTile {
        location: file_data
            .and_then(|data| data.get(TILE_URL_KEY))
            .and_then(plist::Value::as_string)
            .map(str::to_string),
        location_type: file_data
            .and_then(|data| data.get(TILE_URL_TYPE_KEY))
            .and_then(plist::Value::as_signed_integer),
    }
}

/// Points `tile` at `url` under the new label. The Dock resolves a tile by its
/// `GUID` and bookmark before its URL, so both go and it resolves the URL
/// afresh.
fn retarget_tile(tile: &mut plist::Value, url: &str) {
    let Some(tile) = tile.as_dictionary_mut() else {
        return;
    };
    tile.remove(TILE_GUID_KEY);
    let Some(data) = tile
        .get_mut(TILE_DATA_KEY)
        .and_then(plist::Value::as_dictionary_mut)
    else {
        return;
    };
    data.remove(TILE_BOOKMARK_KEY);
    data.insert(
        TILE_LABEL_KEY.to_string(),
        plist::Value::String(DOCK_TILE_LABEL.to_string()),
    );
    if let Some(file_data) = data
        .get_mut(TILE_FILE_DATA_KEY)
        .and_then(plist::Value::as_dictionary_mut)
    {
        file_data.insert(
            TILE_URL_KEY.to_string(),
            plist::Value::String(url.to_string()),
        );
        file_data.insert(
            TILE_URL_TYPE_KEY.to_string(),
            plist::Value::Integer(DOCK_URL_STRING.into()),
        );
    }
}

/// The tiles left once each of `actions` is applied, in order.
fn apply_tile_actions(
    tiles: Vec<plist::Value>,
    actions: &[DockTileAction],
    url: &str,
) -> Vec<plist::Value> {
    tiles
        .into_iter()
        .zip(actions)
        .filter_map(|(mut tile, action)| match action {
            DockTileAction::Keep => Some(tile),
            DockTileAction::Retarget => {
                retarget_tile(&mut tile, url);
                Some(tile)
            }
            DockTileAction::Drop => None,
        })
        .collect()
}

/// Points the Dock's tiles for `legacy` at `bundle` as [`plan_dock_tiles`]
/// plans and writes the keys that change, both read before either is written.
/// Whether anything was written.
fn retarget_dock_tiles(
    dock: &impl DockHost,
    legacy: &Path,
    bundle: &Path,
) -> Result<bool, BundleRenameError> {
    let pinned = dock.read_tiles(DOCK_PINNED_KEY)?.unwrap_or_default();
    let recent = dock.read_tiles(DOCK_RECENT_KEY)?.unwrap_or_default();
    let plan = plan_dock_tiles(&DockFacts {
        pinned: pinned.iter().map(dock_tile).collect(),
        recent: recent.iter().map(dock_tile).collect(),
        legacy_bundle: legacy.to_path_buf(),
        bundle: bundle.to_path_buf(),
    });
    write_planned_tiles(dock, pinned, recent, &plan)
}

/// How many of `tiles` tiles are left once `actions` drop theirs.
fn kept_tile_count(tiles: usize, actions: &[DockTileAction]) -> usize {
    let dropped = actions
        .iter()
        .filter(|action| **action == DockTileAction::Drop)
        .count();
    tiles.saturating_sub(dropped)
}

/// Writes the keys `plan` changes, each only when it holds exactly its tiles
/// less the ones the plan drops; otherwise writes nothing. Whether anything
/// was written.
fn write_planned_tiles(
    dock: &impl DockHost,
    pinned: Vec<plist::Value>,
    recent: Vec<plist::Value>,
    plan: &DockPlan,
) -> Result<bool, BundleRenameError> {
    if !plan.changes_anything() {
        return Ok(false);
    }
    let (pinned_before, recent_before) = (pinned.len(), recent.len());
    let mut writes = Vec::new();
    let mut expected = Vec::new();
    for (key, tiles, actions, changes) in [
        (DOCK_PINNED_KEY, pinned, &plan.pinned, plan.changes_pinned()),
        (DOCK_RECENT_KEY, recent, &plan.recent, plan.changes_recent()),
    ] {
        if changes {
            expected.push(kept_tile_count(tiles.len(), actions));
            writes.push((key, apply_tile_actions(tiles, actions, &plan.bundle_url)));
        }
    }
    let after = |key, before| {
        writes
            .iter()
            .find(|(written, _)| *written == key)
            .map_or(before, |(_, tiles)| tiles.len())
    };
    info!(
        operation = DOCK_TILES_OPERATION,
        pinned_before,
        pinned_after = after(DOCK_PINNED_KEY, pinned_before),
        recent_before,
        recent_after = after(DOCK_RECENT_KEY, recent_before),
    );
    for ((key, tiles), expected) in writes.iter().zip(expected) {
        if tiles.len() != expected {
            return Err(BundleRenameError::DockTileCount {
                key,
                expected,
                actual: tiles.len(),
            });
        }
    }
    dock.write_tiles(&writes)?;
    Ok(true)
}

/// What the Launchpad update came to, for the log.
enum LaunchpadRun {
    NoUserFolder,
    NoBookmark,
    Ran(LaunchpadUpdate),
    Failed(StorageError),
}

impl fmt::Display for LaunchpadRun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoUserFolder => f.write_str("no user folder"),
            Self::NoBookmark => f.write_str("no bookmark"),
            Self::Ran(update) => update.fmt(f),
            Self::Failed(error) => write!(f, "failed: {error}"),
        }
    }
}

fn launchpad_run(dock: &impl DockHost, bundle: &Path) -> LaunchpadRun {
    let Some(folder) = dock.user_folder() else {
        return LaunchpadRun::NoUserFolder;
    };
    let Some(bookmark) = dock.bookmark(bundle) else {
        return LaunchpadRun::NoBookmark;
    };
    let retitle = LaunchpadRetitle {
        bundle_id: BUNDLE_IDENTIFIER,
        from_title: legacy_names::PRODUCT_NAME,
        to_title: DOCK_TILE_LABEL,
        bookmark: &bookmark,
    };
    match dock.retitle_launchpad(&folder.join(LAUNCHPAD_DATABASE), &retitle) {
        Ok(update) => LaunchpadRun::Ran(update),
        Err(error) => LaunchpadRun::Failed(error),
    }
}

/// Gives the Launchpad's row for the old name the new title and `bundle`'s
/// bookmark, best effort: no outcome fails the step. Whether a row changed.
fn retitle_launchpad_row(dock: &impl DockHost, bundle: &Path) -> bool {
    let run = launchpad_run(dock, bundle);
    match &run {
        LaunchpadRun::Failed(_) => warn!(operation = LAUNCHPAD_OPERATION, outcome = %run),
        _ => info!(operation = LAUNCHPAD_OPERATION, outcome = %run),
    }
    matches!(run, LaunchpadRun::Ran(update) if update.changed_anything())
}

/// Points the Dock's tiles for the bundle at `legacy`, the old name, at
/// `bundle`, retitles its Launchpad row, and restarts the Dock once when
/// either changed anything. A tile write that fails restarts nothing.
pub(crate) fn migrate_dock(
    dock: &impl DockHost,
    legacy: &Path,
    bundle: &Path,
) -> Result<StepCompletion, BundleRenameError> {
    let tiles_written = retarget_dock_tiles(dock, legacy, bundle)?;
    let launchpad_changed = retitle_launchpad_row(dock, bundle);
    if tiles_written || launchpad_changed {
        dock.restart()?;
    }
    Ok(StepCompletion::Done)
}

/// Whether `link` is a link that resolves to `bundle`.
fn is_link_to(link: &Path, bundle: &Path) -> bool {
    let is_link = link
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink());
    is_link
        && matches!(
            (std::fs::canonicalize(link), std::fs::canonicalize(bundle)),
            (Ok(resolved), Ok(bundle)) if resolved == bundle
        )
}

/// The old name's path when the Dock step may run for `bundle`: the bundle is
/// a real folder under the new name and the old name beside it holds nothing
/// or the link to it.
fn dock_step_legacy(bundle: &Path) -> Option<PathBuf> {
    let legacy = sibling_path(bundle)?;
    let ready = legacy.file_name() == Some(OsStr::new(legacy_names::BUNDLE_FOLDER))
        && is_real_folder(bundle)
        && (is_absent(&legacy) || is_link_to(&legacy, bundle));
    ready.then_some(legacy)
}

/// What each per-user step did; `None` for a step that waits for a later
/// launch and recorded nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PerUserRuns {
    pub(crate) login: Option<StepRun>,
    pub(crate) links: StepRun,
    pub(crate) dock: Option<StepRun>,
}

/// Runs the login item, command links and Dock steps, each once per the
/// record.
pub(crate) fn run_per_user_steps(
    steps: &PerUserSteps,
    login: &impl LoginItem,
    dock: &impl DockHost,
    home: Option<&Path>,
    launch_at_login: bool,
    command_folder: &Path,
    app_binary_name: Option<&OsStr>,
) -> PerUserRuns {
    let login_run = match home {
        Some(home) => {
            let legacy_agent = home
                .join(LAUNCH_AGENTS_FOLDER)
                .join(legacy_names::LOGIN_AGENT_FILE);
            Some(run_migration_step(
                &steps.data_dir,
                MigrationStep::LoginItem,
                &steps.version,
                || migrate_login_item(login, &legacy_agent, launch_at_login, app_binary_name),
            ))
        }
        None => {
            warn!(
                step = MigrationStep::LoginItem.key(),
                "the home folder is unknown"
            );
            None
        }
    };
    let links_run = run_migration_step(
        &steps.data_dir,
        MigrationStep::CommandLinks,
        &steps.version,
        || migrate_command_links(command_folder, &steps.bundle, app_binary_name),
    );
    let dock_run = match dock_step_legacy(&steps.bundle) {
        Some(legacy) => Some(run_migration_step(
            &steps.data_dir,
            MigrationStep::Dock,
            &steps.version,
            || migrate_dock(dock, &legacy, &steps.bundle),
        )),
        None => {
            info!(
                step = MigrationStep::Dock.key(),
                "waits for the old name to hold nothing or the link"
            );
            None
        }
    };
    PerUserRuns {
        login: login_run,
        links: links_run,
        dock: dock_run,
    }
}

/// Runs [`run_per_user_steps`] off the main thread once the app is set up.
pub(crate) fn spawn_per_user_steps(handle: &AppHandle, steps: PerUserSteps) {
    let handle = handle.clone();
    std::thread::spawn(move || {
        let launch_at_login = recover_poison(
            handle.state::<AppState>().config.lock(),
            "bundle rename per-user steps",
        )
        .general
        .launch_at_login;
        let login = handle.autolaunch();
        let home = dirs::home_dir();
        let exe = std::env::current_exe().ok();
        let command_folder = Path::new(SYSTEM_LINK)
            .parent()
            .unwrap_or_else(|| Path::new("/"));
        run_per_user_steps(
            &steps,
            login.inner(),
            &MacDock,
            home.as_deref(),
            launch_at_login,
            command_folder,
            exe.as_deref().and_then(Path::file_name),
        );
    });
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::macos::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime};

    use core_foundation::array::CFArray;
    use core_foundation::date::CFDate;
    use core_foundation::propertylist::CFPropertyListSubClass;
    use core_foundation::url::CFURL;
    use jotra_core::bundle_rename::{bundle_file_url, BUNDLE_FOLDER};
    use jotra_core::cli_install::{InstalledEntry, CLI_SIGNATURE};
    use jotra_core::name_migration::StepOutcome;
    use tempfile::TempDir;

    use super::*;

    const VERSION: &str = "1.1.0";
    const APP_BINARY: &str = "jotra-app";

    struct Folder {
        _dir: TempDir,
        root: PathBuf,
    }

    fn folder() -> Folder {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().canonicalize().expect("canonical temp dir");
        Folder { _dir: dir, root }
    }

    fn info_plist(identifier: &str, version: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>{identifier}</string>\
             <key>CFBundleShortVersionString</key><string>{version}</string>\
             </dict></plist>\n"
        )
    }

    fn make_bundle(path: &Path, identifier: &str, version: &str) -> PathBuf {
        let exe = executable_in_bundle(path, APP_BINARY);
        std::fs::create_dir_all(exe.parent().expect("executables folder")).expect("bundle");
        std::fs::write(&exe, b"app").expect("executable");
        std::fs::write(info_plist_path(path), info_plist(identifier, version)).expect("plist");
        exe
    }

    fn legacy_bundle(root: &Path) -> PathBuf {
        root.join(legacy_names::BUNDLE_FOLDER)
    }

    fn current_bundle(root: &Path) -> PathBuf {
        root.join(BUNDLE_FOLDER)
    }

    fn entry(data_dir: &Path, step: MigrationStep) -> Option<StepOutcome> {
        read_name_migration_record(data_dir)
            .entry(step)
            .map(|entry| entry.outcome)
    }

    fn is_hidden_link(path: &Path) -> bool {
        let metadata = path.symlink_metadata().expect("link metadata");
        metadata.file_type().is_symlink() && metadata.st_flags() & libc::UF_HIDDEN != 0
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Admin {
        Perform,
        /// Moves the bundle, leaves no link and leaves the folder read-only.
        MoveAndLock,
        Decline,
        Fail,
    }

    type Opened = (PathBuf, Vec<(OsString, OsString)>, Vec<OsString>);

    struct FakeHost {
        trash_folder: PathBuf,
        fail_trash: bool,
        fail_register: bool,
        fail_open: bool,
        admin: Admin,
        lock_dir: Option<PathBuf>,
        trashed: RefCell<Vec<PathBuf>>,
        registered: RefCell<Vec<PathBuf>>,
        administrator_renames: RefCell<usize>,
        opened: RefCell<Vec<Opened>>,
        lock_free_at_open: RefCell<Vec<bool>>,
    }

    impl FakeHost {
        fn new(root: &Path) -> Self {
            let trash_folder = root.join("Trash");
            std::fs::create_dir_all(&trash_folder).expect("trash");
            Self {
                trash_folder,
                fail_trash: false,
                fail_register: false,
                fail_open: false,
                admin: Admin::Perform,
                lock_dir: None,
                trashed: RefCell::new(Vec::new()),
                registered: RefCell::new(Vec::new()),
                administrator_renames: RefCell::new(0),
                opened: RefCell::new(Vec::new()),
                lock_free_at_open: RefCell::new(Vec::new()),
            }
        }
    }

    impl BundleHost for FakeHost {
        fn trash(&self, path: &Path) -> Result<(), BundleRenameError> {
            if self.fail_trash {
                return Err(BundleRenameError::Trash {
                    path: path.to_path_buf(),
                    reason: "refused".to_string(),
                });
            }
            let name = path.file_name().expect("trashed name");
            std::fs::rename(path, self.trash_folder.join(name))
                .map_err(|source| io_error("trash", path, source))?;
            self.trashed.borrow_mut().push(path.to_path_buf());
            Ok(())
        }

        fn rename_as_administrator(
            &self,
            from: &Path,
            to: &Path,
        ) -> Result<StepCompletion, BundleRenameError> {
            *self.administrator_renames.borrow_mut() += 1;
            match self.admin {
                Admin::Perform => {
                    rename_exclusive(from, to)?;
                    leave_hidden_link(from, link_target(to)?)?;
                    Ok(StepCompletion::Done)
                }
                Admin::MoveAndLock => {
                    rename_exclusive(from, to)?;
                    let folder = to.parent().expect("folder");
                    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o555))
                        .map_err(|source| io_error("lock", folder, source))?;
                    Ok(StepCompletion::Done)
                }
                Admin::Decline => Ok(StepCompletion::Declined),
                Admin::Fail => Err(BundleRenameError::Administrator("refused".to_string())),
            }
        }

        fn register(&self, bundle: &Path) -> Result<(), BundleRenameError> {
            self.registered.borrow_mut().push(bundle.to_path_buf());
            if self.fail_register {
                return Err(BundleRenameError::Register {
                    path: bundle.to_path_buf(),
                    status: -10814,
                });
            }
            Ok(())
        }

        fn open(
            &self,
            bundle: &Path,
            env: &[(OsString, OsString)],
            args: &[OsString],
        ) -> Result<(), BundleRenameError> {
            if let Some(lock_dir) = &self.lock_dir {
                let free = jotra_storage::instance::acquire(lock_dir).is_ok();
                self.lock_free_at_open.borrow_mut().push(free);
            }
            self.opened
                .borrow_mut()
                .push((bundle.to_path_buf(), env.to_vec(), args.to_vec()));
            if self.fail_open {
                return Err(BundleRenameError::Open {
                    path: bundle.to_path_buf(),
                    reason: "refused".to_string(),
                });
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeLogin {
        fail: bool,
        set_calls: RefCell<Vec<bool>>,
    }

    impl LoginItem for FakeLogin {
        fn is_enabled(&self) -> Result<bool, LoginItemError> {
            Ok(self.set_calls.borrow().last().copied().unwrap_or(false))
        }

        fn set(&self, on: bool) -> Result<(), LoginItemError> {
            if self.fail {
                return Err(LoginItemError::Os("refused".to_string()));
            }
            self.set_calls.borrow_mut().push(on);
            Ok(())
        }
    }

    fn rename_plan(root: &Path, trash_older: bool, privileges: Privileges) -> BundlePlan {
        BundlePlan::Rename(RenamePlan {
            from: legacy_bundle(root),
            to: current_bundle(root),
            trash_older,
            privileges,
        })
    }

    #[test]
    fn a_bundles_identifier_and_version_read_from_its_info_plist() {
        let folder = folder();
        let bundle = current_bundle(&folder.root);
        make_bundle(&bundle, BUNDLE_IDENTIFIER, VERSION);
        let info = read_bundle_info(&bundle).expect("info");
        assert_eq!(info.identifier, BUNDLE_IDENTIFIER);
        assert_eq!(info.version, VERSION);
    }

    #[test]
    fn a_malformed_or_incomplete_info_plist_is_an_error() {
        let folder = folder();
        let bundle = current_bundle(&folder.root);
        make_bundle(&bundle, BUNDLE_IDENTIFIER, VERSION);
        std::fs::write(info_plist_path(&bundle), "not a plist").expect("plist");
        assert!(matches!(
            read_bundle_info(&bundle),
            Err(BundleRenameError::InfoPlist { .. })
        ));
        std::fs::write(
            info_plist_path(&bundle),
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .expect("plist");
        let error = read_bundle_info(&bundle).expect_err("missing version");
        assert!(error.to_string().contains("CFBundleShortVersionString"));
        assert!(read_bundle_info(&folder.root.join("Missing.app")).is_err());
    }

    #[test]
    fn the_facts_describe_a_bundle_under_the_old_name() {
        let folder = folder();
        let exe = make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let facts = gather_facts(&exe, NameMigrationRecord::default(), true);
        assert!(!facts.exe_through_link);
        assert_eq!(facts.bundle, Some(legacy_bundle(&folder.root)));
        assert_eq!(facts.canonical_bundle, Some(legacy_bundle(&folder.root)));
        assert_eq!(
            facts.running,
            Some(BundleInfo {
                identifier: BUNDLE_IDENTIFIER.to_string(),
                version: VERSION.to_string(),
            })
        );
        assert_eq!(facts.sibling, SiblingState::Absent);
        assert!(!facts.read_only_volume);
        assert!(facts.folder_writable);
        assert!(facts.skip_requested);
    }

    #[test]
    fn the_facts_see_an_executable_reached_through_a_link() {
        let folder = folder();
        make_bundle(&current_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        leave_hidden_link(&legacy_bundle(&folder.root), Path::new(BUNDLE_FOLDER)).expect("link");
        let exe = executable_in_bundle(&legacy_bundle(&folder.root), APP_BINARY);
        let facts = gather_facts(&exe, NameMigrationRecord::default(), false);
        assert!(facts.exe_through_link);
        assert_eq!(facts.canonical_bundle, Some(current_bundle(&folder.root)));
        assert_eq!(
            plan_bundle_rename(&facts),
            Ok(BundlePlan::OpenCanonical(current_bundle(&folder.root)))
        );
    }

    #[test]
    fn the_facts_tell_each_kind_of_sibling_apart() {
        let folder = folder();
        let exe = make_bundle(&current_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let legacy = legacy_bundle(&folder.root);

        std::os::unix::fs::symlink(BUNDLE_FOLDER, &legacy).expect("link");
        let facts = gather_facts(&exe, NameMigrationRecord::default(), false);
        assert_eq!(facts.sibling, SiblingState::Link);
        std::fs::remove_file(&legacy).expect("remove link");

        std::fs::write(&legacy, b"file").expect("file");
        let facts = gather_facts(&exe, NameMigrationRecord::default(), false);
        assert_eq!(facts.sibling, SiblingState::Unreadable);
        std::fs::remove_file(&legacy).expect("remove file");

        std::fs::create_dir(&legacy).expect("empty folder");
        let facts = gather_facts(&exe, NameMigrationRecord::default(), false);
        assert_eq!(facts.sibling, SiblingState::Unreadable);
        std::fs::remove_dir(&legacy).expect("remove folder");

        make_bundle(&legacy, BUNDLE_IDENTIFIER, "1.0.0");
        let facts = gather_facts(&exe, NameMigrationRecord::default(), false);
        assert_eq!(
            facts.sibling,
            SiblingState::Bundle(BundleInfo {
                identifier: BUNDLE_IDENTIFIER.to_string(),
                version: "1.0.0".to_string(),
            })
        );
    }

    #[test]
    fn the_facts_see_a_folder_that_is_not_writable() {
        let folder = folder();
        let apps = folder.root.join("Apps");
        let exe = make_bundle(&legacy_bundle(&apps), BUNDLE_IDENTIFIER, VERSION);
        std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o555)).expect("lock");
        let facts = gather_facts(&exe, NameMigrationRecord::default(), false);
        std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o755)).expect("unlock");
        assert!(!facts.folder_writable);
    }

    #[test]
    fn an_executable_outside_a_bundle_has_no_bundle_facts() {
        let folder = folder();
        let exe = folder.root.join(APP_BINARY);
        std::fs::write(&exe, b"app").expect("executable");
        let facts = gather_facts(&exe, NameMigrationRecord::default(), false);
        assert_eq!(facts.bundle, None);
        assert_eq!(facts.running, None);
        assert_eq!(facts.sibling, SiblingState::Absent);
        assert!(!facts.folder_writable);
    }

    #[test]
    fn an_exclusive_rename_never_replaces_what_is_there() {
        let folder = folder();
        let from = folder.root.join("a");
        let to = folder.root.join("b");
        std::fs::create_dir(&from).expect("from");
        std::fs::create_dir(&to).expect("to");
        let error = rename_exclusive(&from, &to).expect_err("refused");
        assert!(matches!(
            error,
            BundleRenameError::Io {
                operation: "rename",
                ..
            }
        ));
        assert!(from.is_dir());
        std::fs::remove_dir(&to).expect("clear");
        rename_exclusive(&from, &to).expect("renamed");
        assert!(!from.exists());
        assert!(to.is_dir());
    }

    #[test]
    fn the_link_left_behind_is_relative_and_hidden() {
        let folder = folder();
        std::fs::create_dir(current_bundle(&folder.root)).expect("bundle");
        let link = legacy_bundle(&folder.root);
        leave_hidden_link(&link, Path::new(BUNDLE_FOLDER)).expect("link");
        assert_eq!(
            std::fs::read_link(&link).expect("target"),
            PathBuf::from(BUNDLE_FOLDER)
        );
        assert!(is_hidden_link(&link));
        assert_eq!(
            link.canonicalize().expect("resolves"),
            current_bundle(&folder.root)
        );
        assert!(leave_hidden_link(&link, Path::new(BUNDLE_FOLDER)).is_err());
    }

    #[test]
    fn a_rename_moves_the_bundle_leaves_the_link_and_relaunches() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let host = FakeHost::new(&folder.root);
        let plan = rename_plan(&folder.root, false, Privileges::User);

        let execution = execute_plan(&plan, &host, &data, VERSION);

        assert_eq!(execution, Execution::Relaunch(current_bundle(&folder.root)));
        assert!(is_real_folder(&current_bundle(&folder.root)));
        assert!(is_hidden_link(&legacy_bundle(&folder.root)));
        assert_eq!(
            std::fs::read_link(legacy_bundle(&folder.root)).expect("target"),
            PathBuf::from(BUNDLE_FOLDER)
        );
        assert_eq!(
            *host.registered.borrow(),
            vec![current_bundle(&folder.root)]
        );
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Done)
        );
        assert_eq!(
            entry(&data, MigrationStep::BundleLink),
            Some(StepOutcome::Done)
        );
    }

    #[test]
    fn a_link_that_fails_after_the_move_is_recorded_on_its_own_and_the_launch_relaunches() {
        let folder = folder();
        let apps = folder.root.join("Apps");
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&apps), BUNDLE_IDENTIFIER, VERSION);
        let mut host = FakeHost::new(&folder.root);
        host.admin = Admin::MoveAndLock;
        let plan = rename_plan(&apps, false, Privileges::Administrator);

        let execution = execute_plan(&plan, &host, &data, VERSION);
        std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o755)).expect("unlock");

        assert_eq!(execution, Execution::Relaunch(current_bundle(&apps)));
        assert!(is_absent(&legacy_bundle(&apps)));
        assert_eq!(*host.registered.borrow(), vec![current_bundle(&apps)]);
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Done)
        );
        let link = read_name_migration_record(&data)
            .entry(MigrationStep::BundleLink)
            .expect("link entry");
        assert_eq!(link.outcome, StepOutcome::Failed);
        assert_eq!(link.version, VERSION);
        assert!(link.reason.is_some());
    }

    #[test]
    fn an_administrator_rename_finds_its_link_and_leaves_no_second_one() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let host = FakeHost::new(&folder.root);
        let plan = rename_plan(&folder.root, false, Privileges::Administrator);

        execute_plan(&plan, &host, &data, VERSION);

        assert!(is_hidden_link(&legacy_bundle(&folder.root)));
        assert_eq!(
            std::fs::read_link(legacy_bundle(&folder.root)).expect("target"),
            PathBuf::from(BUNDLE_FOLDER)
        );
        assert!(is_absent(&current_bundle(&folder.root).join(BUNDLE_FOLDER)));
        assert_eq!(
            entry(&data, MigrationStep::BundleLink),
            Some(StepOutcome::Done)
        );
    }

    #[test]
    fn a_link_that_did_not_land_is_left_on_a_later_launch_from_the_new_name() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        let exe = make_bundle(&current_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        for (step, outcome) in [
            (MigrationStep::BundleRename, StepOutcome::Done),
            (MigrationStep::BundleLink, StepOutcome::Failed),
        ] {
            let failed = jotra_core::name_migration::StepEntry {
                outcome,
                version: "1.0.0".to_string(),
                at: chrono::Utc::now(),
                reason: None,
            };
            jotra_storage::name_migration::record_migration_step(&data, step, &failed)
                .expect("record");
        }
        let host = FakeHost::new(&folder.root);

        let (execution, _) = plan_and_execute(&exe, &data, false, &host);

        assert_eq!(execution, Execution::Continue);
        assert!(is_hidden_link(&legacy_bundle(&folder.root)));
        assert_eq!(
            legacy_bundle(&folder.root)
                .canonicalize()
                .expect("resolves"),
            current_bundle(&folder.root)
        );
        let link = read_name_migration_record(&data)
            .entry(MigrationStep::BundleLink)
            .expect("link entry");
        assert_eq!(link.outcome, StepOutcome::Done);
        assert_eq!(link.version, VERSION);
        assert!(host.registered.borrow().is_empty());
        assert!(host.trashed.borrow().is_empty());
    }

    #[test]
    fn a_link_restored_in_a_locked_folder_fails_again() {
        let folder = folder();
        let apps = folder.root.join("Apps");
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&current_bundle(&apps), BUNDLE_IDENTIFIER, VERSION);
        let host = FakeHost::new(&folder.root);
        let plan = BundlePlan::RestoreLink {
            link: legacy_bundle(&apps),
            bundle: current_bundle(&apps),
        };

        std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o555)).expect("lock");
        let execution = execute_plan(&plan, &host, &data, VERSION);
        std::fs::set_permissions(&apps, std::fs::Permissions::from_mode(0o755)).expect("unlock");

        assert_eq!(execution, Execution::Continue);
        assert!(is_absent(&legacy_bundle(&apps)));
        assert_eq!(
            entry(&data, MigrationStep::BundleLink),
            Some(StepOutcome::Failed)
        );
    }

    #[test]
    fn a_registration_that_fails_still_relaunches_from_the_new_name() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let mut host = FakeHost::new(&folder.root);
        host.fail_register = true;
        let plan = rename_plan(&folder.root, false, Privileges::User);

        assert_eq!(
            execute_plan(&plan, &host, &data, VERSION),
            Execution::Relaunch(current_bundle(&folder.root))
        );
        assert_eq!(
            *host.registered.borrow(),
            vec![current_bundle(&folder.root)]
        );
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Done)
        );
    }

    #[test]
    fn a_rename_that_stays_put_registers_nothing() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let mut host = FakeHost::new(&folder.root);
        host.admin = Admin::Decline;
        let plans = [
            rename_plan(&folder.root, false, Privileges::Administrator),
            BundlePlan::OpenCanonical(current_bundle(&folder.root)),
            BundlePlan::OpenSibling(current_bundle(&folder.root)),
        ];
        for plan in &plans {
            execute_plan(plan, &host, &data, VERSION);
        }
        assert!(host.registered.borrow().is_empty());
    }

    #[test]
    fn an_older_bundle_at_the_new_name_goes_to_the_trash_first() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        make_bundle(&current_bundle(&folder.root), BUNDLE_IDENTIFIER, "1.0.0");
        let host = FakeHost::new(&folder.root);
        let plan = rename_plan(&folder.root, true, Privileges::User);

        let execution = execute_plan(&plan, &host, &data, VERSION);

        assert_eq!(execution, Execution::Relaunch(current_bundle(&folder.root)));
        assert_eq!(*host.trashed.borrow(), vec![current_bundle(&folder.root)]);
        assert_eq!(
            read_bundle_info(&current_bundle(&folder.root))
                .expect("info")
                .version,
            VERSION
        );
        assert!(is_hidden_link(&legacy_bundle(&folder.root)));
    }

    #[test]
    fn a_rename_that_finds_the_new_name_taken_fails_and_carries_on() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        std::fs::create_dir(current_bundle(&folder.root)).expect("taken");
        let host = FakeHost::new(&folder.root);
        let plan = rename_plan(&folder.root, false, Privileges::User);

        let execution = execute_plan(&plan, &host, &data, VERSION);

        assert_eq!(execution, Execution::Continue);
        assert!(is_real_folder(&legacy_bundle(&folder.root)));
        assert!(host.registered.borrow().is_empty());
        let recorded = read_name_migration_record(&data)
            .entry(MigrationStep::BundleRename)
            .expect("entry");
        assert_eq!(recorded.outcome, StepOutcome::Failed);
        assert_eq!(recorded.version, VERSION);
        assert!(recorded.reason.is_some());
    }

    #[test]
    fn a_trash_that_fails_leaves_both_bundles() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        make_bundle(&current_bundle(&folder.root), BUNDLE_IDENTIFIER, "1.0.0");
        let mut host = FakeHost::new(&folder.root);
        host.fail_trash = true;
        let plan = rename_plan(&folder.root, true, Privileges::User);

        assert_eq!(
            execute_plan(&plan, &host, &data, VERSION),
            Execution::Continue
        );
        assert!(is_real_folder(&legacy_bundle(&folder.root)));
        assert!(is_real_folder(&current_bundle(&folder.root)));
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Failed)
        );
    }

    #[test]
    fn a_declined_administrator_prompt_is_recorded_and_the_launch_carries_on() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let mut host = FakeHost::new(&folder.root);
        host.admin = Admin::Decline;
        let plan = rename_plan(&folder.root, false, Privileges::Administrator);

        assert_eq!(
            execute_plan(&plan, &host, &data, VERSION),
            Execution::Continue
        );
        assert_eq!(*host.administrator_renames.borrow(), 1);
        assert!(is_real_folder(&legacy_bundle(&folder.root)));
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Declined)
        );
    }

    #[test]
    fn a_failed_administrator_rename_is_recorded_as_failed() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let mut host = FakeHost::new(&folder.root);
        host.admin = Admin::Fail;
        let plan = rename_plan(&folder.root, false, Privileges::Administrator);

        assert_eq!(
            execute_plan(&plan, &host, &data, VERSION),
            Execution::Continue
        );
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Failed)
        );
    }

    #[test]
    fn an_administrator_rename_relaunches_from_the_new_name() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let host = FakeHost::new(&folder.root);
        let plan = rename_plan(&folder.root, false, Privileges::Administrator);

        assert_eq!(
            execute_plan(&plan, &host, &data, VERSION),
            Execution::Relaunch(current_bundle(&folder.root))
        );
        assert_eq!(
            *host.registered.borrow(),
            vec![current_bundle(&folder.root)]
        );
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Done)
        );
    }

    #[test]
    fn an_old_name_bundle_beside_the_new_one_is_replaced_by_the_link() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&current_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, "1.0.0");
        let host = FakeHost::new(&folder.root);
        let plan = BundlePlan::ReplaceLegacy {
            legacy: legacy_bundle(&folder.root),
            bundle: current_bundle(&folder.root),
        };

        assert_eq!(
            execute_plan(&plan, &host, &data, VERSION),
            Execution::Continue
        );
        assert_eq!(*host.trashed.borrow(), vec![legacy_bundle(&folder.root)]);
        assert!(is_hidden_link(&legacy_bundle(&folder.root)));
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Done)
        );
        assert_eq!(
            entry(&data, MigrationStep::BundleLink),
            Some(StepOutcome::Done)
        );
        assert!(host.registered.borrow().is_empty());
    }

    #[test]
    fn a_replacement_whose_trash_fails_leaves_the_old_bundle() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        make_bundle(&current_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, "1.0.0");
        let mut host = FakeHost::new(&folder.root);
        host.fail_trash = true;
        let plan = BundlePlan::ReplaceLegacy {
            legacy: legacy_bundle(&folder.root),
            bundle: current_bundle(&folder.root),
        };

        assert_eq!(
            execute_plan(&plan, &host, &data, VERSION),
            Execution::Continue
        );
        assert!(is_real_folder(&legacy_bundle(&folder.root)));
        assert_eq!(
            entry(&data, MigrationStep::BundleRename),
            Some(StepOutcome::Failed)
        );
        assert_eq!(entry(&data, MigrationStep::BundleLink), None);
    }

    #[test]
    fn opening_another_bundle_records_nothing() {
        let folder = folder();
        let host = FakeHost::new(&folder.root);
        for plan in [
            BundlePlan::OpenCanonical(current_bundle(&folder.root)),
            BundlePlan::OpenSibling(current_bundle(&folder.root)),
        ] {
            assert_eq!(
                execute_plan(&plan, &host, &folder.root, VERSION),
                Execution::Relaunch(current_bundle(&folder.root))
            );
        }
        assert!(read_name_migration_record(&folder.root)
            .entry(MigrationStep::BundleRename)
            .is_none());
    }

    #[test]
    fn a_launch_from_the_old_name_renames_once() {
        let folder = folder();
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        let exe = make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let host = FakeHost::new(&folder.root);

        let (execution, per_user) = plan_and_execute(&exe, &data, false, &host);
        assert_eq!(execution, Execution::Relaunch(current_bundle(&folder.root)));
        assert_eq!(per_user, None);
        assert_eq!(
            *host.registered.borrow(),
            vec![current_bundle(&folder.root)]
        );

        let exe = executable_in_bundle(&current_bundle(&folder.root), APP_BINARY);
        let (execution, per_user) = plan_and_execute(&exe, &data, false, &host);
        assert_eq!(execution, Execution::Continue);
        assert_eq!(host.registered.borrow().len(), 1);
        assert_eq!(
            per_user,
            Some(PerUserSteps {
                bundle: current_bundle(&folder.root),
                version: VERSION.to_string(),
                data_dir: data.clone(),
            })
        );
    }

    #[test]
    fn a_launch_with_the_skip_variable_leaves_the_old_name() {
        let folder = folder();
        let exe = make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        let host = FakeHost::new(&folder.root);
        let (execution, per_user) = plan_and_execute(&exe, &folder.root, true, &host);
        assert_eq!(execution, Execution::Continue);
        assert_eq!(per_user, None);
        assert!(is_real_folder(&legacy_bundle(&folder.root)));
    }

    #[test]
    fn open_starts_a_new_instance_and_passes_arguments_only_when_there_are_some() {
        let bundle = Path::new("/Applications").join(BUNDLE_FOLDER);
        assert_eq!(
            open_arguments(&bundle, &[], &[]),
            vec![
                OsString::from("-n"),
                OsString::from("-a"),
                bundle.clone().into_os_string(),
            ]
        );
        let args = [OsString::from("/tmp/a b.md")];
        assert_eq!(
            open_arguments(&bundle, &[], &args),
            vec![
                OsString::from("-n"),
                OsString::from("-a"),
                bundle.clone().into_os_string(),
                OsString::from("--args"),
                OsString::from("/tmp/a b.md"),
            ]
        );
    }

    #[test]
    fn open_hands_each_variable_to_the_new_instance_whole() {
        let bundle = Path::new("/Applications").join(BUNDLE_FOLDER);
        let env = relaunch_environment([
            (
                OsString::from("JOTRA_DATA_DIR"),
                OsString::from("/Users/a b/x=y"),
            ),
            (OsString::from("PATH"), OsString::from("/usr/bin")),
            (
                OsString::from(legacy_names::NOTES_DIR_VAR),
                OsString::from(""),
            ),
        ]);
        assert_eq!(
            open_arguments(&bundle, &env, &[OsString::from("note.md")]),
            vec![
                OsString::from("-n"),
                OsString::from("--env"),
                OsString::from("JOTRA_DATA_DIR=/Users/a b/x=y"),
                OsString::from("--env"),
                OsString::from(format!("{}=", legacy_names::NOTES_DIR_VAR)),
                OsString::from("-a"),
                bundle.clone().into_os_string(),
                OsString::from("--args"),
                OsString::from("note.md"),
            ]
        );
    }

    #[test]
    fn the_administrator_script_quotes_both_paths_for_the_shell_and_applescript() {
        let from = Path::new("/Apps/O'Brien \"x\"").join(legacy_names::BUNDLE_FOLDER);
        let to = Path::new("/Apps/O'Brien \"x\"").join(BUNDLE_FOLDER);
        let script = administrator_rename_script(&from, &to).expect("script");
        assert!(script.starts_with("do shell script \""));
        assert!(script.ends_with("\" with administrator privileges"));
        assert!(script.contains("/bin/mv "));
        assert!(script.contains("/bin/ln -s 'Jotra.app' "));
        assert!(script.contains("/usr/bin/chflags -h hidden "));
        assert!(script.contains("O'\\\\''Brien \\\"x\\\""));
        assert!(administrator_rename_script(&from, Path::new("/")).is_err());
    }

    /// The text of the AppleScript string literal in `do shell script "…"`,
    /// read the way AppleScript reads it. Any quote the script does not escape
    /// ends the literal early and fails the test.
    fn shell_text_of(script: &str) -> String {
        let body = script
            .strip_prefix("do shell script \"")
            .and_then(|rest| rest.strip_suffix("\" with administrator privileges"))
            .expect("script shape");
        let mut text = String::new();
        let mut chars = body.chars();
        while let Some(character) = chars.next() {
            match character {
                '\\' => match chars.next() {
                    Some(escaped @ ('\\' | '"')) => text.push(escaped),
                    other => panic!("unexpected escape {other:?} in {script}"),
                },
                '"' => panic!("an unescaped quote ends the literal early: {script}"),
                other => text.push(other),
            }
        }
        text
    }

    fn run_shell(text: &str, cwd: &Path) -> std::process::ExitStatus {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(text)
            .current_dir(cwd)
            .status()
            .expect("sh")
    }

    #[test]
    fn the_administrator_script_runs_on_paths_with_quotes_slashes_and_substitutions() {
        let folder = folder();
        let apps = folder
            .root
            .join("a \"b\" 'c' \\d $(touch pwned) `touch tick`");
        make_bundle(&legacy_bundle(&apps), BUNDLE_IDENTIFIER, VERSION);
        let script = administrator_rename_script(&legacy_bundle(&apps), &current_bundle(&apps))
            .expect("script");

        let status = run_shell(&shell_text_of(&script), &folder.root);

        assert!(status.success(), "{status}");
        assert!(is_real_folder(&current_bundle(&apps)));
        assert!(info_plist_path(&current_bundle(&apps)).is_file());
        assert!(is_hidden_link(&legacy_bundle(&apps)));
        assert_eq!(
            std::fs::read_link(legacy_bundle(&apps)).expect("target"),
            PathBuf::from(BUNDLE_FOLDER)
        );
        for place in [&folder.root, &apps] {
            assert!(!place.join("pwned").exists());
            assert!(!place.join("tick").exists());
        }
    }

    #[test]
    fn the_administrator_script_moves_nothing_when_the_new_name_is_taken() {
        let folder = folder();
        make_bundle(&legacy_bundle(&folder.root), BUNDLE_IDENTIFIER, VERSION);
        std::fs::create_dir(current_bundle(&folder.root)).expect("taken");
        let script = administrator_rename_script(
            &legacy_bundle(&folder.root),
            &current_bundle(&folder.root),
        )
        .expect("script");

        let status = run_shell(&shell_text_of(&script), &folder.root);

        assert!(!status.success());
        assert!(is_real_folder(&legacy_bundle(&folder.root)));
        assert!(is_absent(
            &current_bundle(&folder.root).join(legacy_names::BUNDLE_FOLDER)
        ));
    }

    #[test]
    fn a_move_into_a_folder_made_at_the_new_name_after_the_check_is_undone() {
        let folder = folder();
        let elsewhere = folder.root.join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("elsewhere");
        let make_targets: [fn(&Path, &Path); 2] = [
            |to, _| std::fs::create_dir(to).expect("folder"),
            |to, elsewhere| std::os::unix::fs::symlink(elsewhere, to).expect("link to a folder"),
        ];
        for make_target in make_targets {
            let from = legacy_bundle(&folder.root);
            let to = current_bundle(&folder.root);
            make_bundle(&from, BUNDLE_IDENTIFIER, VERSION);
            make_target(&to, &elsewhere);

            let shell = move_and_link_shell(&from, &to).expect("shell");
            let status = run_shell(&shell, &folder.root);

            assert!(!status.success());
            assert!(is_real_folder(&from));
            assert!(info_plist_path(&from).is_file());
            assert!(is_absent(&to.join(legacy_names::BUNDLE_FOLDER)));
            assert!(is_absent(&elsewhere.join(legacy_names::BUNDLE_FOLDER)));

            std::fs::remove_dir_all(&from).expect("clear bundle");
            if to.is_symlink() {
                std::fs::remove_file(&to).expect("clear link");
            } else {
                std::fs::remove_dir(&to).expect("clear folder");
            }
        }
    }

    #[test]
    fn a_cancelled_prompt_reads_as_cancelled() {
        assert!(is_cancelled(
            "0:120: execution error: User canceled. (-128)\n"
        ));
        assert!(is_cancelled("execution error: User canceled. (-128)"));
        assert!(!is_cancelled("error -128"));
        assert!(!is_cancelled("execution error: an error occurred. (-1280)"));
        assert!(!is_cancelled("execution error: mv: permission denied (1)"));
    }

    #[test]
    fn a_relaunch_lets_go_of_the_lock_before_opening_and_ends_when_it_opens() {
        let folder = folder();
        let lock_dir = folder.root.join("data");
        let mut host = FakeHost::new(&folder.root);
        host.lock_dir = Some(lock_dir.clone());
        let lock = jotra_storage::instance::acquire(&lock_dir).expect("lock");
        let env = vec![(OsString::from("JOTRA_DATA_DIR"), OsString::from("/x"))];
        let args = vec![OsString::from("note.md")];
        let reclaimed = std::cell::Cell::new(false);

        let outcome = relaunch(
            &host,
            &current_bundle(&folder.root),
            &env,
            &args,
            Some(lock),
            || {
                reclaimed.set(true);
                None
            },
        );

        assert!(matches!(outcome, RelaunchOutcome::Opened));
        assert_eq!(*host.lock_free_at_open.borrow(), vec![true]);
        assert_eq!(
            *host.opened.borrow(),
            vec![(current_bundle(&folder.root), env, args)]
        );
        assert!(!reclaimed.get());
    }

    #[test]
    fn a_relaunch_that_does_not_open_claims_the_lock_again() {
        let folder = folder();
        let lock_dir = folder.root.join("data");
        let mut host = FakeHost::new(&folder.root);
        host.lock_dir = Some(lock_dir.clone());
        host.fail_open = true;
        let lock = jotra_storage::instance::acquire(&lock_dir).expect("lock");

        let outcome = relaunch(
            &host,
            &current_bundle(&folder.root),
            &[],
            &[],
            Some(lock),
            || jotra_storage::instance::acquire(&lock_dir).ok(),
        );

        assert_eq!(*host.lock_free_at_open.borrow(), vec![true]);
        let RelaunchOutcome::CarriedOn(Some(_lock)) = outcome else {
            panic!("the lock is claimed again");
        };
        assert!(matches!(
            jotra_storage::instance::acquire(&lock_dir),
            Err(jotra_storage::instance::InstanceLockError::Held)
        ));
    }

    #[test]
    fn a_relaunch_without_the_lock_claims_none_when_it_does_not_open() {
        let folder = folder();
        let mut host = FakeHost::new(&folder.root);
        host.fail_open = true;
        let reclaimed = std::cell::Cell::new(false);

        let outcome = relaunch(&host, &current_bundle(&folder.root), &[], &[], None, || {
            reclaimed.set(true);
            None
        });

        assert!(matches!(outcome, RelaunchOutcome::CarriedOn(None)));
        assert!(!reclaimed.get());
        assert_eq!(host.opened.borrow().len(), 1);
    }

    /// Writes a launch agent at `path` whose first program argument is
    /// `program`, the shape the login item service writes.
    fn write_login_agent(path: &Path, program: &Path) {
        let mut arguments = plist::Dictionary::new();
        arguments.insert(
            "Label".to_string(),
            plist::Value::from(legacy_names::PRODUCT_NAME),
        );
        arguments.insert(
            "ProgramArguments".to_string(),
            plist::Value::Array(vec![plist::Value::from(
                program.to_string_lossy().into_owned(),
            )]),
        );
        arguments.insert("RunAtLoad".to_string(), plist::Value::Boolean(true));
        plist::Value::Dictionary(arguments)
            .to_file_xml(path)
            .expect("agent");
    }

    fn own_legacy_agent(root: &Path) -> PathBuf {
        let agent = root.join(legacy_names::LOGIN_AGENT_FILE);
        let program = executable_in_bundle(&legacy_bundle(root), legacy_names::GUI_BINARY_STEM);
        write_login_agent(&agent, &program);
        agent
    }

    #[test]
    fn the_login_item_step_removes_the_old_agent_and_registers_when_asked() {
        let folder = folder();
        let agent = own_legacy_agent(&folder.root);
        let login = FakeLogin::default();
        assert_eq!(
            migrate_login_item(&login, &agent, true, None).expect("step"),
            StepCompletion::Done
        );
        assert!(!agent.exists());
        assert_eq!(*login.set_calls.borrow(), vec![true]);
    }

    #[test]
    fn the_login_item_step_registers_nothing_the_config_does_not_ask_for() {
        let folder = folder();
        let agent = own_legacy_agent(&folder.root);
        let login = FakeLogin::default();
        assert_eq!(
            migrate_login_item(&login, &agent, false, None).expect("step"),
            StepCompletion::Done
        );
        assert!(!agent.exists());
        assert!(login.set_calls.borrow().is_empty());
        assert_eq!(
            migrate_login_item(&login, &agent, false, None).expect("step"),
            StepCompletion::Done
        );
    }

    #[test]
    fn an_agent_that_starts_the_app_from_the_new_name_is_removed_too() {
        let folder = folder();
        let agent = folder.root.join(legacy_names::LOGIN_AGENT_FILE);
        write_login_agent(
            &agent,
            &executable_in_bundle(&current_bundle(&folder.root), APP_BINARY),
        );
        let login = FakeLogin::default();
        assert_eq!(
            migrate_login_item(&login, &agent, false, Some(OsStr::new(APP_BINARY))).expect("step"),
            StepCompletion::Done
        );
        assert!(is_absent(&agent));
    }

    #[test]
    fn an_agent_at_the_old_name_that_starts_something_else_stays() {
        let folder = folder();
        let agent = folder.root.join(legacy_names::LOGIN_AGENT_FILE);
        let login = FakeLogin::default();
        for program in [
            folder
                .root
                .join("bin")
                .join(format!("{}-sync", legacy_names::COMMAND_NAME)),
            executable_in_bundle(
                &folder.root.join("Other.app"),
                legacy_names::GUI_BINARY_STEM,
            ),
            executable_in_bundle(&legacy_bundle(&folder.root), "helper"),
        ] {
            write_login_agent(&agent, &program);
            assert_eq!(
                migrate_login_item(&login, &agent, true, Some(OsStr::new(APP_BINARY)))
                    .expect("step"),
                StepCompletion::Declined,
                "{}",
                program.display()
            );
            assert!(agent.is_file(), "{}", program.display());
        }
        assert_eq!(*login.set_calls.borrow(), vec![true, true, true]);
    }

    #[test]
    fn an_agent_at_the_old_name_that_does_not_read_stays() {
        let folder = folder();
        let agent = folder.root.join(legacy_names::LOGIN_AGENT_FILE);
        std::fs::write(&agent, b"not a plist").expect("agent");
        let login = FakeLogin::default();
        assert_eq!(
            migrate_login_item(&login, &agent, false, None).expect("step"),
            StepCompletion::Declined
        );
        assert_eq!(std::fs::read(&agent).expect("agent"), b"not a plist");
    }

    #[test]
    fn a_refused_login_item_fails_the_step() {
        let folder = folder();
        let agent = folder.root.join(legacy_names::LOGIN_AGENT_FILE);
        let login = FakeLogin {
            fail: true,
            ..FakeLogin::default()
        };
        assert!(matches!(
            migrate_login_item(&login, &agent, true, None),
            Err(BundleRenameError::LoginItem(_))
        ));
    }

    fn command_bundle_at(bundle: &Path) -> PathBuf {
        for name in [COMMAND_NAME, legacy_names::COMMAND_NAME] {
            let command = executable_in_bundle(bundle, name);
            std::fs::create_dir_all(command.parent().expect("folder")).expect("bundle");
            std::fs::write(&command, CLI_SIGNATURE).expect("command");
        }
        bundle.to_path_buf()
    }

    fn command_bundle(root: &Path) -> PathBuf {
        command_bundle_at(&current_bundle(root))
    }

    fn command_folder(root: &Path) -> PathBuf {
        let bin = root.join("bin");
        std::fs::create_dir(&bin).expect("bin");
        bin
    }

    #[test]
    fn a_legacy_command_is_relinked_and_the_new_name_added() {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        let bin = command_folder(&folder.root);
        std::fs::write(bin.join(legacy_names::COMMAND_NAME), CLI_SIGNATURE).expect("legacy");

        assert_eq!(
            migrate_command_links(&bin, &bundle, None).expect("step"),
            StepCompletion::Done
        );
        assert_eq!(
            std::fs::read_link(bin.join(legacy_names::COMMAND_NAME)).expect("legacy link"),
            executable_in_bundle(&bundle, legacy_names::COMMAND_NAME)
        );
        assert_eq!(
            std::fs::read_link(bin.join(COMMAND_NAME)).expect("command link"),
            executable_in_bundle(&bundle, COMMAND_NAME)
        );
    }

    #[test]
    fn another_programs_command_name_is_left_alone() {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        let bin = command_folder(&folder.root);
        std::fs::write(bin.join(legacy_names::COMMAND_NAME), CLI_SIGNATURE).expect("legacy");
        std::fs::write(bin.join(COMMAND_NAME), b"another program").expect("foreign");

        assert_eq!(
            migrate_command_links(&bin, &bundle, None).expect("step"),
            StepCompletion::Done
        );
        assert_eq!(
            inspect_entry(&bin.join(COMMAND_NAME), None),
            InstalledEntry::File {
                is_jotra_command: false
            }
        );
        assert!(bin
            .join(legacy_names::COMMAND_NAME)
            .symlink_metadata()
            .expect("legacy")
            .file_type()
            .is_symlink());
    }

    #[test]
    fn a_legacy_name_that_is_not_jotras_command_is_untouched() {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        let bin = command_folder(&folder.root);
        std::fs::write(bin.join(legacy_names::COMMAND_NAME), b"another program").expect("legacy");

        assert_eq!(
            migrate_command_links(&bin, &bundle, None).expect("step"),
            StepCompletion::Done
        );
        assert_eq!(
            std::fs::read(bin.join(legacy_names::COMMAND_NAME)).expect("legacy"),
            b"another program"
        );
        assert!(!bin.join(COMMAND_NAME).exists());
    }

    #[test]
    fn a_command_folder_that_needs_a_password_is_a_decline() {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        let bin = command_folder(&folder.root);
        std::fs::write(bin.join(legacy_names::COMMAND_NAME), CLI_SIGNATURE).expect("legacy");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).expect("lock");
        let result = migrate_command_links(&bin, &bundle, None);
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("unlock");
        assert_eq!(result.expect("step"), StepCompletion::Declined);
        assert!(!bin.join(COMMAND_NAME).exists());
    }

    #[test]
    fn a_legacy_link_left_dangling_by_the_rename_is_pointed_at_the_new_name() {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        let bin = command_folder(&folder.root);
        let gone = executable_in_bundle(&legacy_bundle(&folder.root), legacy_names::COMMAND_NAME);
        std::os::unix::fs::symlink(&gone, bin.join(legacy_names::COMMAND_NAME)).expect("link");

        assert_eq!(
            migrate_command_links(&bin, &bundle, None).expect("step"),
            StepCompletion::Done
        );
        assert_eq!(
            std::fs::read_link(bin.join(legacy_names::COMMAND_NAME)).expect("legacy link"),
            executable_in_bundle(&bundle, legacy_names::COMMAND_NAME)
        );
        assert!(bin.join(legacy_names::COMMAND_NAME).is_file());
        assert!(bin.join(COMMAND_NAME).is_file());
    }

    #[test]
    fn a_bundle_without_the_command_fails_the_step_and_keeps_the_legacy_name() {
        let folder = folder();
        let bundle = current_bundle(&folder.root);
        make_bundle(&bundle, BUNDLE_IDENTIFIER, VERSION);
        let bin = command_folder(&folder.root);
        std::fs::write(bin.join(legacy_names::COMMAND_NAME), CLI_SIGNATURE).expect("legacy");

        assert!(matches!(
            migrate_command_links(&bin, &bundle, None),
            Err(BundleRenameError::Unresolved { .. })
        ));
        assert_eq!(
            std::fs::read(bin.join(legacy_names::COMMAND_NAME)).expect("legacy"),
            CLI_SIGNATURE
        );
        assert!(is_absent(&bin.join(COMMAND_NAME)));
    }

    #[test]
    fn no_command_is_pointed_into_a_translocated_or_cask_bundle() {
        let folder = folder();
        let bin = command_folder(&folder.root);
        let legacy = bin.join(legacy_names::COMMAND_NAME);
        std::fs::write(&legacy, CLI_SIGNATURE).expect("legacy");
        for place in [
            folder.root.join("AppTranslocation").join("A1B2").join("d"),
            folder.root.join("Caskroom").join("jotra").join(VERSION),
        ] {
            let bundle = command_bundle_at(&current_bundle(&place));
            assert_eq!(
                migrate_command_links(&bin, &bundle, None).expect("step"),
                StepCompletion::Declined,
                "{}",
                bundle.display()
            );
            assert_eq!(std::fs::read(&legacy).expect("legacy"), CLI_SIGNATURE);
            assert!(is_absent(&bin.join(COMMAND_NAME)));
        }
    }

    #[test]
    fn the_per_user_steps_run_once_and_are_recorded() {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        let bin = command_folder(&folder.root);
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        let home = folder.root.join("home");
        let agents = home.join(LAUNCH_AGENTS_FOLDER);
        std::fs::create_dir_all(&agents).expect("agents");
        write_login_agent(
            &agents.join(legacy_names::LOGIN_AGENT_FILE),
            &executable_in_bundle(&legacy_bundle(&folder.root), legacy_names::GUI_BINARY_STEM),
        );
        std::fs::write(bin.join(legacy_names::COMMAND_NAME), CLI_SIGNATURE).expect("legacy");
        let steps = PerUserSteps {
            bundle,
            version: VERSION.to_string(),
            data_dir: data.clone(),
        };
        let login = FakeLogin::default();
        let dock = FakeDock::default();

        let runs = run_per_user_steps(&steps, &login, &dock, Some(&home), true, &bin, None);
        assert_eq!(
            runs,
            PerUserRuns {
                login: Some(StepRun::Done),
                links: StepRun::Done,
                dock: Some(StepRun::Done),
            }
        );
        assert!(!agents.join(legacy_names::LOGIN_AGENT_FILE).exists());
        assert_eq!(
            entry(&data, MigrationStep::LoginItem),
            Some(StepOutcome::Done)
        );
        assert_eq!(
            entry(&data, MigrationStep::CommandLinks),
            Some(StepOutcome::Done)
        );

        assert_eq!(entry(&data, MigrationStep::Dock), Some(StepOutcome::Done));

        let runs = run_per_user_steps(&steps, &login, &dock, Some(&home), true, &bin, None);
        assert_eq!(
            runs,
            PerUserRuns {
                login: Some(StepRun::AlreadySettled),
                links: StepRun::AlreadySettled,
                dock: Some(StepRun::AlreadySettled),
            }
        );
        assert_eq!(*login.set_calls.borrow(), vec![true]);
        assert_eq!(
            *dock.calls.borrow(),
            [
                DockCall::Read(DOCK_PINNED_KEY),
                DockCall::Read(DOCK_RECENT_KEY)
            ]
        );
    }

    #[test]
    fn the_login_step_waits_without_a_home_folder() {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        let bin = command_folder(&folder.root);
        let steps = PerUserSteps {
            bundle,
            version: VERSION.to_string(),
            data_dir: folder.root.clone(),
        };
        let runs = run_per_user_steps(
            &steps,
            &FakeLogin::default(),
            &FakeDock::default(),
            None,
            true,
            &bin,
            None,
        );
        assert_eq!(runs.login, None);
        assert_eq!(entry(&folder.root, MigrationStep::LoginItem), None);
    }

    #[derive(Debug, Clone, PartialEq)]
    enum DockCall {
        Read(&'static str),
        Write(Vec<(&'static str, Vec<plist::Value>)>),
        Restart,
        Bookmark(PathBuf),
        Launchpad {
            database: PathBuf,
            bundle_id: String,
            from_title: String,
            to_title: String,
            bookmark: Vec<u8>,
        },
    }

    #[derive(Default)]
    struct FakeDock {
        pinned: Option<Vec<plist::Value>>,
        recent: Option<Vec<plist::Value>>,
        unreadable: Option<&'static str>,
        fail_write: bool,
        fail_restart: bool,
        user_folder: Option<PathBuf>,
        bookmark: Option<Vec<u8>>,
        /// What the Launchpad update returns; `None` returns an error.
        launchpad: Option<LaunchpadUpdate>,
        calls: RefCell<Vec<DockCall>>,
    }

    const LAUNCHPAD_USER_FOLDER: &str = "/private/var/folders/zz/user/0";
    const NEW_BOOKMARK: &[u8] = b"bookmark of the new bundle";

    impl FakeDock {
        fn with_launchpad(self, update: Option<LaunchpadUpdate>) -> Self {
            Self {
                user_folder: Some(PathBuf::from(LAUNCHPAD_USER_FOLDER)),
                bookmark: Some(NEW_BOOKMARK.to_vec()),
                launchpad: update,
                ..self
            }
        }

        fn restarts(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|call| **call == DockCall::Restart)
                .count()
        }

        fn retitled_launchpad(&self) -> bool {
            self.calls
                .borrow()
                .iter()
                .any(|call| matches!(call, DockCall::Launchpad { .. }))
        }
    }

    impl DockHost for FakeDock {
        fn read_tiles(
            &self,
            key: &'static str,
        ) -> Result<Option<Vec<plist::Value>>, BundleRenameError> {
            self.calls.borrow_mut().push(DockCall::Read(key));
            if self.unreadable == Some(key) {
                return Err(dock_setting(key, DockSettingProblem::Managed));
            }
            Ok(match key {
                DOCK_PINNED_KEY => self.pinned.clone(),
                DOCK_RECENT_KEY => self.recent.clone(),
                _ => None,
            })
        }

        fn write_tiles(
            &self,
            tiles: &[(&'static str, Vec<plist::Value>)],
        ) -> Result<(), BundleRenameError> {
            self.calls
                .borrow_mut()
                .push(DockCall::Write(tiles.to_vec()));
            if self.fail_write {
                return Err(BundleRenameError::DockNotSaved);
            }
            Ok(())
        }

        fn restart(&self) -> Result<(), BundleRenameError> {
            self.calls.borrow_mut().push(DockCall::Restart);
            if self.fail_restart {
                return Err(BundleRenameError::DockRestart {
                    reason: "exit status: 1".to_string(),
                });
            }
            Ok(())
        }

        fn user_folder(&self) -> Option<PathBuf> {
            self.user_folder.clone()
        }

        fn bookmark(&self, bundle: &Path) -> Option<Vec<u8>> {
            self.calls
                .borrow_mut()
                .push(DockCall::Bookmark(bundle.to_path_buf()));
            self.bookmark.clone()
        }

        fn retitle_launchpad(
            &self,
            database: &Path,
            retitle: &LaunchpadRetitle<'_>,
        ) -> StorageResult<LaunchpadUpdate> {
            self.calls.borrow_mut().push(DockCall::Launchpad {
                database: database.to_path_buf(),
                bundle_id: retitle.bundle_id.to_string(),
                from_title: retitle.from_title.to_string(),
                to_title: retitle.to_title.to_string(),
                bookmark: retitle.bookmark.to_vec(),
            });
            self.launchpad
                .ok_or_else(|| StorageError::Io(io::Error::other("the file did not open")))
        }
    }

    const NOTES_IDENTIFIER: &str = "com.apple.Notes";
    const MAIL_IDENTIFIER: &str = "com.apple.mail";

    fn dict(entries: Vec<(&str, plist::Value)>) -> plist::Value {
        let mut dictionary = plist::Dictionary::new();
        for (key, value) in entries {
            dictionary.insert(key.to_string(), value);
        }
        plist::Value::Dictionary(dictionary)
    }

    fn plist_date(seconds: u64) -> plist::Value {
        plist::Value::Date((SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)).into())
    }

    /// A tile as the Dock writes one for an app; `guid` also adds the bookmark
    /// the Dock keeps beside it.
    fn app_tile(bundle: &Path, label: &str, identifier: &str, guid: Option<i64>) -> plist::Value {
        let mut data = vec![
            ("bundle-identifier", identifier.into()),
            ("dock-extra", false.into()),
            (
                TILE_FILE_DATA_KEY,
                dict(vec![
                    (TILE_URL_KEY, bundle_file_url(bundle).into()),
                    (TILE_URL_TYPE_KEY, DOCK_URL_STRING.into()),
                ]),
            ),
            (TILE_LABEL_KEY, label.into()),
            ("file-mod-date", 3_784_000_000_i64.into()),
            ("file-type", 41.into()),
            ("is-beta", false.into()),
            ("parent-mod-date", 3_783_000_000_i64.into()),
        ];
        let mut tile = vec![("tile-type", "file-tile".into())];
        if let Some(guid) = guid {
            data.push((
                TILE_BOOKMARK_KEY,
                plist::Value::Data(vec![0x62, 0x6f, 0x6f, 0x6b, 0x00, 0x01, 0xfe, 0xff]),
            ));
            tile.push((TILE_GUID_KEY, guid.into()));
        }
        tile.push((TILE_DATA_KEY, dict(data)));
        dict(tile)
    }

    struct DockApps {
        legacy: PathBuf,
        bundle: PathBuf,
        notes: plist::Value,
        mail: plist::Value,
    }

    fn dock_apps() -> DockApps {
        let apps = Path::new("/Applications");
        DockApps {
            legacy: apps.join(legacy_names::BUNDLE_FOLDER),
            bundle: apps.join(BUNDLE_FOLDER),
            notes: app_tile(&apps.join("Notes.app"), "Notes", NOTES_IDENTIFIER, Some(11)),
            mail: app_tile(&apps.join("Mail.app"), "Mail", MAIL_IDENTIFIER, Some(12)),
        }
    }

    impl DockApps {
        fn legacy_tile(&self, guid: i64) -> plist::Value {
            app_tile(
                &self.legacy,
                legacy_names::PRODUCT_NAME,
                BUNDLE_IDENTIFIER,
                Some(guid),
            )
        }

        fn current_tile(&self, guid: i64) -> plist::Value {
            app_tile(&self.bundle, DOCK_TILE_LABEL, BUNDLE_IDENTIFIER, Some(guid))
        }

        fn retargeted_tile(&self) -> plist::Value {
            app_tile(&self.bundle, DOCK_TILE_LABEL, BUNDLE_IDENTIFIER, None)
        }
    }

    fn dock_reads() -> Vec<DockCall> {
        vec![
            DockCall::Read(DOCK_PINNED_KEY),
            DockCall::Read(DOCK_RECENT_KEY),
        ]
    }

    #[test]
    fn a_dock_without_the_old_bundle_is_neither_written_nor_restarted() {
        let apps = dock_apps();
        for dock in [
            FakeDock {
                pinned: Some(vec![apps.notes.clone(), apps.current_tile(21)]),
                recent: Some(vec![apps.mail.clone()]),
                ..FakeDock::default()
            },
            FakeDock::default(),
        ] {
            assert_eq!(
                migrate_dock(&dock, &apps.legacy, &apps.bundle).expect("step"),
                StepCompletion::Done
            );
            assert_eq!(*dock.calls.borrow(), dock_reads());
        }
    }

    #[test]
    fn the_dock_is_written_before_it_restarts_and_keeps_every_other_key() {
        let apps = dock_apps();
        let dock = FakeDock {
            pinned: Some(vec![apps.notes.clone(), apps.legacy_tile(21)]),
            recent: Some(vec![
                apps.legacy_tile(22),
                apps.mail.clone(),
                apps.current_tile(23),
            ]),
            ..FakeDock::default()
        };

        assert_eq!(
            migrate_dock(&dock, &apps.legacy, &apps.bundle).expect("step"),
            StepCompletion::Done
        );
        let mut expected = dock_reads();
        expected.push(DockCall::Write(vec![
            (
                DOCK_PINNED_KEY,
                vec![apps.notes.clone(), apps.retargeted_tile()],
            ),
            (DOCK_RECENT_KEY, vec![apps.mail.clone()]),
        ]));
        expected.push(DockCall::Restart);
        assert_eq!(*dock.calls.borrow(), expected);
    }

    #[test]
    fn only_the_dock_setting_that_changes_is_written() {
        let apps = dock_apps();
        let pinned_only = FakeDock {
            pinned: Some(vec![apps.legacy_tile(21)]),
            recent: Some(vec![apps.mail.clone()]),
            ..FakeDock::default()
        };
        migrate_dock(&pinned_only, &apps.legacy, &apps.bundle).expect("step");
        assert_eq!(
            pinned_only.calls.borrow()[2],
            DockCall::Write(vec![(DOCK_PINNED_KEY, vec![apps.retargeted_tile()])])
        );

        let recent_only = FakeDock {
            pinned: Some(vec![apps.notes.clone()]),
            recent: Some(vec![apps.legacy_tile(22), apps.mail.clone()]),
            ..FakeDock::default()
        };
        migrate_dock(&recent_only, &apps.legacy, &apps.bundle).expect("step");
        assert_eq!(
            recent_only.calls.borrow()[2],
            DockCall::Write(vec![(DOCK_RECENT_KEY, vec![apps.mail.clone()])])
        );
    }

    #[test]
    fn an_absent_dock_setting_reads_as_empty_and_is_not_written() {
        let apps = dock_apps();
        let without_recent = FakeDock {
            pinned: Some(vec![apps.legacy_tile(21)]),
            ..FakeDock::default()
        };
        migrate_dock(&without_recent, &apps.legacy, &apps.bundle).expect("step");
        assert_eq!(
            without_recent.calls.borrow()[2..],
            [
                DockCall::Write(vec![(DOCK_PINNED_KEY, vec![apps.retargeted_tile()])]),
                DockCall::Restart
            ]
        );

        let without_pinned = FakeDock {
            recent: Some(vec![apps.legacy_tile(22)]),
            ..FakeDock::default()
        };
        migrate_dock(&without_pinned, &apps.legacy, &apps.bundle).expect("step");
        assert_eq!(
            without_pinned.calls.borrow()[2..],
            [
                DockCall::Write(vec![(DOCK_RECENT_KEY, vec![])]),
                DockCall::Restart
            ]
        );
    }

    #[test]
    fn a_failed_dock_write_restarts_nothing_and_fails() {
        let apps = dock_apps();
        let dock = FakeDock {
            pinned: Some(vec![apps.legacy_tile(21)]),
            fail_write: true,
            ..FakeDock::default()
        };
        assert!(matches!(
            migrate_dock(&dock, &apps.legacy, &apps.bundle),
            Err(BundleRenameError::DockNotSaved)
        ));
        let calls = dock.calls.borrow();
        assert!(matches!(calls.last(), Some(DockCall::Write(_))));
        assert!(!calls.contains(&DockCall::Restart));
    }

    #[test]
    fn a_failed_dock_restart_fails_the_step() {
        let apps = dock_apps();
        let dock = FakeDock {
            pinned: Some(vec![apps.legacy_tile(21)]),
            fail_restart: true,
            ..FakeDock::default()
        };
        assert!(matches!(
            migrate_dock(&dock, &apps.legacy, &apps.bundle),
            Err(BundleRenameError::DockRestart { .. })
        ));
        assert_eq!(dock.calls.borrow().last(), Some(&DockCall::Restart));
    }

    #[test]
    fn an_unreadable_dock_setting_writes_nothing() {
        let apps = dock_apps();
        for key in [DOCK_PINNED_KEY, DOCK_RECENT_KEY] {
            let dock = FakeDock {
                pinned: Some(vec![apps.legacy_tile(21)]),
                recent: Some(vec![apps.legacy_tile(22)]),
                unreadable: Some(key),
                ..FakeDock::default()
            };
            assert!(matches!(
                migrate_dock(&dock, &apps.legacy, &apps.bundle),
                Err(BundleRenameError::DockSetting {
                    key: failed,
                    problem: DockSettingProblem::Managed,
                }) if failed == key
            ));
            assert!(dock
                .calls
                .borrow()
                .iter()
                .all(|call| matches!(call, DockCall::Read(_))));
        }
    }

    fn launchpad_calls(apps: &DockApps) -> [DockCall; 2] {
        [
            DockCall::Bookmark(apps.bundle.clone()),
            DockCall::Launchpad {
                database: Path::new(LAUNCHPAD_USER_FOLDER)
                    .join("com.apple.dock.launchpad")
                    .join("db")
                    .join("db"),
                bundle_id: BUNDLE_IDENTIFIER.to_string(),
                from_title: legacy_names::PRODUCT_NAME.to_string(),
                to_title: DOCK_TILE_LABEL.to_string(),
                bookmark: NEW_BOOKMARK.to_vec(),
            },
        ]
    }

    #[test]
    fn a_stale_launchpad_row_alone_restarts_the_dock_once() {
        let apps = dock_apps();
        let dock = FakeDock::default().with_launchpad(Some(LaunchpadUpdate::Changed(1)));
        assert_eq!(
            migrate_dock(&dock, &apps.legacy, &apps.bundle).expect("step"),
            StepCompletion::Done
        );
        let mut expected = dock_reads();
        expected.extend(launchpad_calls(&apps));
        expected.push(DockCall::Restart);
        assert_eq!(*dock.calls.borrow(), expected);
    }

    #[test]
    fn the_tiles_and_the_launchpad_row_share_one_restart() {
        let apps = dock_apps();
        let dock = FakeDock {
            pinned: Some(vec![apps.legacy_tile(21)]),
            ..FakeDock::default()
        }
        .with_launchpad(Some(LaunchpadUpdate::Changed(1)));
        migrate_dock(&dock, &apps.legacy, &apps.bundle).expect("step");
        let mut expected = dock_reads();
        expected.push(DockCall::Write(vec![(
            DOCK_PINNED_KEY,
            vec![apps.retargeted_tile()],
        )]));
        expected.extend(launchpad_calls(&apps));
        expected.push(DockCall::Restart);
        assert_eq!(*dock.calls.borrow(), expected);
    }

    fn unchanged_launchpad_outcomes() -> [Option<LaunchpadUpdate>; 5] {
        [
            Some(LaunchpadUpdate::Changed(0)),
            Some(LaunchpadUpdate::NoDatabase),
            Some(LaunchpadUpdate::UnknownShape),
            Some(LaunchpadUpdate::Busy),
            None,
        ]
    }

    #[test]
    fn nothing_changed_anywhere_restarts_nothing() {
        let apps = dock_apps();
        for update in unchanged_launchpad_outcomes() {
            let dock = FakeDock {
                pinned: Some(vec![apps.notes.clone(), apps.current_tile(21)]),
                ..FakeDock::default()
            }
            .with_launchpad(update);
            assert_eq!(
                migrate_dock(&dock, &apps.legacy, &apps.bundle).expect("step"),
                StepCompletion::Done,
                "{update:?}"
            );
            let mut expected = dock_reads();
            expected.extend(launchpad_calls(&apps));
            assert_eq!(*dock.calls.borrow(), expected, "{update:?}");
        }
    }

    #[test]
    fn no_launchpad_outcome_fails_the_step() {
        let apps = dock_apps();
        for update in unchanged_launchpad_outcomes() {
            let dock = FakeDock {
                pinned: Some(vec![apps.legacy_tile(21)]),
                ..FakeDock::default()
            }
            .with_launchpad(update);
            assert_eq!(
                migrate_dock(&dock, &apps.legacy, &apps.bundle).expect("step"),
                StepCompletion::Done,
                "{update:?}"
            );
            assert!(dock.retitled_launchpad(), "{update:?}");
            assert_eq!(dock.restarts(), 1, "{update:?}");
            assert_eq!(dock.calls.borrow().last(), Some(&DockCall::Restart));
        }
    }

    #[test]
    fn without_a_bookmark_or_a_user_folder_the_launchpad_is_left_alone() {
        let apps = dock_apps();
        let without_bookmark = FakeDock {
            bookmark: None,
            ..FakeDock::default().with_launchpad(Some(LaunchpadUpdate::Changed(1)))
        };
        migrate_dock(&without_bookmark, &apps.legacy, &apps.bundle).expect("step");
        let mut expected = dock_reads();
        expected.push(DockCall::Bookmark(apps.bundle.clone()));
        assert_eq!(*without_bookmark.calls.borrow(), expected);

        let without_folder = FakeDock {
            user_folder: None,
            ..FakeDock::default().with_launchpad(Some(LaunchpadUpdate::Changed(1)))
        };
        migrate_dock(&without_folder, &apps.legacy, &apps.bundle).expect("step");
        assert_eq!(*without_folder.calls.borrow(), dock_reads());
    }

    #[test]
    fn a_failed_tile_write_leaves_the_launchpad_alone() {
        let apps = dock_apps();
        let dock = FakeDock {
            pinned: Some(vec![apps.legacy_tile(21)]),
            fail_write: true,
            ..FakeDock::default()
        }
        .with_launchpad(Some(LaunchpadUpdate::Changed(1)));
        assert!(migrate_dock(&dock, &apps.legacy, &apps.bundle).is_err());
        assert!(!dock.retitled_launchpad());
        assert_eq!(dock.restarts(), 0);
    }

    #[test]
    fn a_plan_that_would_lose_or_gain_a_tile_writes_nothing() {
        let apps = dock_apps();
        let url = bundle_file_url(&apps.bundle);
        let recent = vec![apps.legacy_tile(22)];
        let recent_actions = vec![DockTileAction::Drop];
        for (pinned, actions, expected, actual) in [
            (
                vec![apps.notes.clone(), apps.legacy_tile(21)],
                vec![DockTileAction::Retarget],
                2,
                1,
            ),
            (
                vec![apps.legacy_tile(21)],
                vec![DockTileAction::Retarget, DockTileAction::Drop],
                0,
                1,
            ),
        ] {
            let dock = FakeDock::default();
            let plan = DockPlan {
                pinned: actions,
                recent: recent_actions.clone(),
                bundle_url: url.clone(),
            };
            let error = write_planned_tiles(&dock, pinned, recent.clone(), &plan)
                .expect_err("a tile count that does not add up");
            assert!(matches!(
                error,
                BundleRenameError::DockTileCount {
                    key: DOCK_PINNED_KEY,
                    expected: want,
                    actual: got,
                } if want == expected && got == actual
            ));
            assert_eq!(
                error.to_string(),
                format!(
                    "the Dock setting {DOCK_PINNED_KEY} would hold {actual} tiles, not {expected}"
                )
            );
            assert!(dock.calls.borrow().is_empty());
        }
    }

    #[test]
    fn this_users_folder_is_an_existing_folder() {
        let folder = MacDock.user_folder().expect("a user folder");
        assert!(folder.is_absolute());
        assert!(folder.is_dir());
    }

    #[test]
    fn a_bundle_on_disk_has_a_bookmark_and_a_missing_one_has_none() {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        assert!(MacDock
            .bookmark(&bundle)
            .is_some_and(|data| !data.is_empty()));
        assert_eq!(MacDock.bookmark(&folder.root.join("missing.app")), None);
    }

    fn realistic_dock_list(apps: &DockApps) -> Vec<plist::Value> {
        let folder = dict(vec![
            (TILE_GUID_KEY, 31.into()),
            ("tile-type", "directory-tile".into()),
            (
                TILE_DATA_KEY,
                dict(vec![
                    ("arrangement", 2.into()),
                    ("displayas", 1.into()),
                    ("preferreditemsize", (-1.0_f64).into()),
                    ("showas", 0.into()),
                    ("added", plist_date(1_700_000_000)),
                    (
                        "items",
                        plist::Value::Array(vec![
                            plist::Value::Data(vec![0, 1, 2, 3]),
                            plist_date(1_600_000_000),
                            dict(vec![("nested", true.into())]),
                        ]),
                    ),
                ]),
            ),
        ]);
        vec![apps.notes.clone(), apps.legacy_tile(21), folder]
    }

    #[test]
    fn a_dock_list_converts_there_and_back_unchanged_but_for_the_edited_tile() {
        let apps = dock_apps();
        let tiles = realistic_dock_list(&apps);
        let stored =
            property_list_from_value(&plist::Value::Array(tiles.clone())).expect("to a CF list");
        let read = tiles_from_property_list(DOCK_PINNED_KEY, &stored).expect("read");
        assert_eq!(read, tiles);

        let plan = plan_dock_tiles(&DockFacts {
            pinned: read.iter().map(dock_tile).collect(),
            recent: Vec::new(),
            legacy_bundle: apps.legacy.clone(),
            bundle: apps.bundle.clone(),
        });
        let edited = apply_tile_actions(read, &plan.pinned, &plan.bundle_url);
        assert_eq!(
            edited,
            [tiles[0].clone(), apps.retargeted_tile(), tiles[2].clone()]
        );

        let written =
            property_list_from_value(&plist::Value::Array(edited.clone())).expect("to a CF list");
        assert_eq!(
            tiles_from_property_list(DOCK_PINNED_KEY, &written).expect("read back"),
            edited
        );
    }

    #[test]
    fn a_dock_setting_that_changes_in_conversion_is_left_alone() {
        let dates = CFArray::from_CFTypes(&[CFDate::new(0.123_456_789_012_3)]);
        let stored = unsafe { CFPropertyList::wrap_under_get_rule(dates.as_CFTypeRef()) };
        assert!(matches!(
            tiles_from_property_list(DOCK_PINNED_KEY, &stored),
            Err(BundleRenameError::DockSetting {
                problem: DockSettingProblem::RoundTrip,
                ..
            })
        ));
    }

    #[test]
    fn a_dock_setting_that_is_not_a_list_is_left_alone() {
        let stored = CFString::new("file-tile").to_CFPropertyList();
        assert!(matches!(
            tiles_from_property_list(DOCK_RECENT_KEY, &stored),
            Err(BundleRenameError::DockSetting {
                key: DOCK_RECENT_KEY,
                problem: DockSettingProblem::NotList,
            })
        ));
    }

    #[test]
    fn a_bundles_file_url_is_the_one_core_foundation_makes() {
        for path in [
            "/Applications/Jotra.app",
            "/Users/Jane Doe/Apps #1%?/Jotra.app",
            "/Users/Zoë/Applications/Jotra.app",
            "/Volumes/A&B (1)/+~!$',;=@:/Jotra.app",
            "/Volumes/tab\there/Jotra.app",
        ] {
            let made = CFURL::from_path(path, true)
                .expect("a file URL")
                .get_string()
                .to_string();
            assert_eq!(bundle_file_url(Path::new(path)), made, "{path}");
        }
    }

    #[test]
    fn a_retargeted_tile_without_file_data_gets_only_the_label() {
        let mut tile = dict(vec![
            (TILE_GUID_KEY, 41.into()),
            (
                TILE_DATA_KEY,
                dict(vec![(TILE_LABEL_KEY, legacy_names::PRODUCT_NAME.into())]),
            ),
        ]);
        retarget_tile(&mut tile, &bundle_file_url(&dock_apps().bundle));
        assert_eq!(
            tile,
            dict(vec![(
                TILE_DATA_KEY,
                dict(vec![(TILE_LABEL_KEY, DOCK_TILE_LABEL.into())])
            )])
        );
    }

    struct DockStepSetup {
        folder: Folder,
        steps: PerUserSteps,
        bin: PathBuf,
    }

    fn dock_step_setup() -> DockStepSetup {
        let folder = folder();
        let bundle = command_bundle(&folder.root);
        let bin = command_folder(&folder.root);
        let data = folder.root.join("data");
        std::fs::create_dir(&data).expect("data");
        let steps = PerUserSteps {
            bundle,
            version: VERSION.to_string(),
            data_dir: data,
        };
        DockStepSetup { folder, steps, bin }
    }

    impl DockStepSetup {
        fn run(&self, dock: &FakeDock) -> Option<StepRun> {
            run_per_user_steps(
                &self.steps,
                &FakeLogin::default(),
                dock,
                None,
                true,
                &self.bin,
                None,
            )
            .dock
        }

        fn dock_with_the_old_pin(&self) -> FakeDock {
            let legacy = legacy_bundle(&self.folder.root);
            FakeDock {
                pinned: Some(vec![app_tile(
                    &legacy,
                    legacy_names::PRODUCT_NAME,
                    BUNDLE_IDENTIFIER,
                    Some(21),
                )]),
                ..FakeDock::default()
            }
        }
    }

    #[test]
    fn the_dock_step_runs_once_the_old_name_is_the_link() {
        let setup = dock_step_setup();
        leave_hidden_link(&legacy_bundle(&setup.folder.root), Path::new(BUNDLE_FOLDER))
            .expect("link");
        let dock = setup.dock_with_the_old_pin();

        assert_eq!(setup.run(&dock), Some(StepRun::Done));
        assert_eq!(
            entry(&setup.steps.data_dir, MigrationStep::Dock),
            Some(StepOutcome::Done)
        );
        let calls = dock.calls.borrow().clone();
        assert_eq!(
            calls[2],
            DockCall::Write(vec![(
                DOCK_PINNED_KEY,
                vec![app_tile(
                    &setup.steps.bundle,
                    DOCK_TILE_LABEL,
                    BUNDLE_IDENTIFIER,
                    None
                )]
            )])
        );
        assert_eq!(calls[3..], [DockCall::Restart]);

        assert_eq!(setup.run(&dock), Some(StepRun::AlreadySettled));
        assert_eq!(dock.calls.borrow().len(), calls.len());
    }

    #[test]
    fn the_dock_step_runs_when_the_old_name_is_gone() {
        let setup = dock_step_setup();
        let dock = setup.dock_with_the_old_pin();
        assert_eq!(setup.run(&dock), Some(StepRun::Done));
        assert_eq!(dock.calls.borrow().last(), Some(&DockCall::Restart));
    }

    #[test]
    fn the_dock_step_waits_while_the_old_name_holds_a_bundle() {
        let setup = dock_step_setup();
        make_bundle(
            &legacy_bundle(&setup.folder.root),
            BUNDLE_IDENTIFIER,
            VERSION,
        );
        let dock = setup.dock_with_the_old_pin();
        assert_eq!(setup.run(&dock), None);
        assert_eq!(entry(&setup.steps.data_dir, MigrationStep::Dock), None);
        assert!(dock.calls.borrow().is_empty());
    }

    #[test]
    fn the_dock_step_waits_on_a_link_to_another_folder() {
        let setup = dock_step_setup();
        let elsewhere = setup.folder.root.join("elsewhere");
        std::fs::create_dir(&elsewhere).expect("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, legacy_bundle(&setup.folder.root)).expect("link");
        let dock = setup.dock_with_the_old_pin();
        assert_eq!(setup.run(&dock), None);
        assert_eq!(entry(&setup.steps.data_dir, MigrationStep::Dock), None);
        assert!(dock.calls.borrow().is_empty());
    }

    #[test]
    fn the_dock_step_waits_for_a_bundle_that_is_not_a_real_folder() {
        let setup = dock_step_setup();
        let moved = setup.folder.root.join("moved.app");
        std::fs::rename(&setup.steps.bundle, &moved).expect("move");
        std::os::unix::fs::symlink(&moved, &setup.steps.bundle).expect("link");
        let dock = setup.dock_with_the_old_pin();
        assert_eq!(setup.run(&dock), None);
        assert!(dock.calls.borrow().is_empty());
    }

    #[test]
    fn a_failed_dock_step_is_recorded_as_failed() {
        let setup = dock_step_setup();
        let dock = FakeDock {
            fail_write: true,
            ..setup.dock_with_the_old_pin()
        };
        assert_eq!(setup.run(&dock), Some(StepRun::Failed));
        assert_eq!(
            entry(&setup.steps.data_dir, MigrationStep::Dock),
            Some(StepOutcome::Failed)
        );
        assert!(!dock.calls.borrow().contains(&DockCall::Restart));
    }
}
