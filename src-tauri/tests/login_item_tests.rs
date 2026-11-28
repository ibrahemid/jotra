//! Open at login: the OS is asked first, and config.toml records only what it
//! gave; the config and the OS entry are brought together at launch and after
//! a hand edit (U44).

use std::cell::{Cell, RefCell};
use std::sync::Mutex as StdMutex;

use jotra_core::config::JotraConfig;
use jotra_core::login_item::LoginReconcile;
use jotra_storage::login_item_record;
use jotra_tauri_lib::commands::login_item::{
    apply_first_run_choice, reconcile_login_item, set_launch_at_login_inner, LoginItem,
    LoginItemError, LoginItemStatus, LoginLaunch, ERR_LOGIN_ITEM_UNSAVED,
};
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

static ENV_LOCK: StdMutex<()> = StdMutex::new(());

fn launch(data_dir: &std::path::Path, notes_dir: &std::path::Path) -> AppState {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("JOTRA_DATA_DIR", data_dir);
    std::env::set_var("JOTRA_NOTES_DIR", notes_dir);
    let state = AppState::initialize().expect("app state");
    std::env::remove_var("JOTRA_DATA_DIR");
    std::env::remove_var("JOTRA_NOTES_DIR");
    state
}

const INSTALLED: LoginLaunch = LoginLaunch::INSTALLED;
const DEBUG: LoginLaunch = LoginLaunch {
    debug_build: true,
    may_point_here: true,
};
const FROM_DISK_IMAGE: LoginLaunch = LoginLaunch {
    debug_build: false,
    may_point_here: false,
};

/// A login item that records each change and refuses when told to.
#[derive(Default)]
struct FakeLoginItem {
    refuse: bool,
    /// Changes after this many are refused.
    takes: Option<usize>,
    enabled: Cell<bool>,
    asks: RefCell<Vec<bool>>,
}

impl LoginItem for FakeLoginItem {
    fn is_enabled(&self) -> Result<bool, LoginItemError> {
        Ok(self.enabled.get())
    }

    fn set(&self, on: bool) -> Result<(), LoginItemError> {
        self.asks.borrow_mut().push(on);
        if self.refuse
            || self
                .takes
                .is_some_and(|takes| self.asks.borrow().len() > takes)
        {
            return Err(LoginItemError::Os("not permitted".to_string()));
        }
        self.enabled.set(on);
        Ok(())
    }
}

fn on_disk(data: &TempDir) -> JotraConfig {
    let text = std::fs::read_to_string(data.path().join("config.toml")).expect("config written");
    JotraConfig::parse(&text).expect("config parses")
}

fn with_config(data: &TempDir, text: &str) {
    std::fs::write(data.path().join("config.toml"), text).expect("seed config");
}

#[test]
fn turning_it_off_asks_the_os_then_records_the_choice() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&data, "[general]\nlaunch_at_login = true\n");
    let state = launch(data.path(), notes.path());
    let login = FakeLoginItem::default();
    login.enabled.set(true);

    let status = set_launch_at_login_inner(&state, &login, INSTALLED, false).expect("turned off");

    assert_eq!(status, LoginItemStatus { enabled: false });
    assert_eq!(*login.asks.borrow(), [false]);
    assert!(!login.enabled.get());
    assert!(!state.config.lock().unwrap().general.launch_at_login);
    assert!(!on_disk(&data).general.launch_at_login);
}

#[test]
fn a_refusal_from_the_os_leaves_the_config_as_it_was() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&data, "[general]\nlaunch_at_login = false\n");
    let state = launch(data.path(), notes.path());
    let login = FakeLoginItem {
        refuse: true,
        ..FakeLoginItem::default()
    };

    let error = set_launch_at_login_inner(&state, &login, INSTALLED, true).expect_err("refused");

    assert!(matches!(error, LoginItemError::Os(_)));
    assert!(error.to_string().starts_with("ERR_LOGIN_ITEM"), "{error}");
    assert!(!state.config.lock().unwrap().general.launch_at_login);
    assert!(!on_disk(&data).general.launch_at_login);
}

#[test]
fn a_first_launch_turns_it_on() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let state = launch(data.path(), notes.path());
    assert!(state.first_run, "no config file is a first launch");
    let login = FakeLoginItem::default();

    assert!(apply_first_run_choice(&state, &login, INSTALLED).expect("applied"));
    assert!(login.enabled.get());
}

#[test]
fn a_debug_build_never_makes_the_entry() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let state = launch(data.path(), notes.path());
    let login = FakeLoginItem::default();

    assert!(!apply_first_run_choice(&state, &login, DEBUG).expect("skipped"));
    assert!(login.asks.borrow().is_empty());
}

#[test]
fn a_config_that_says_off_makes_no_entry() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&data, "[general]\nlaunch_at_login = false\n");
    let state = launch(data.path(), notes.path());
    let login = FakeLoginItem::default();

    assert!(!apply_first_run_choice(&state, &login, INSTALLED).expect("skipped"));
    assert!(login.asks.borrow().is_empty());
}

fn block_config_writes(data: &TempDir) {
    let config = data.path().join("config.toml");
    std::fs::remove_file(&config).expect("config written by the launch");
    std::fs::create_dir_all(config.join("held")).expect("a folder where the file was");
}

#[test]
fn a_choice_the_config_cannot_record_puts_the_os_entry_back() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&data, "[general]\nlaunch_at_login = false\n");
    let state = launch(data.path(), notes.path());
    block_config_writes(&data);
    let login = FakeLoginItem::default();

    let error = set_launch_at_login_inner(&state, &login, INSTALLED, true).expect_err("unsaved");

    assert!(matches!(error, LoginItemError::Config(_)), "{error}");
    assert_eq!(*login.asks.borrow(), [true, false]);
    assert!(!login.enabled.get(), "the entry it made is gone again");
    assert!(!state.config.lock().unwrap().general.launch_at_login);
}

#[test]
fn an_entry_that_cannot_be_put_back_says_it_changed() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&data, "[general]\nlaunch_at_login = false\n");
    let state = launch(data.path(), notes.path());
    block_config_writes(&data);
    let login = FakeLoginItem {
        takes: Some(1),
        ..FakeLoginItem::default()
    };

    let error = set_launch_at_login_inner(&state, &login, INSTALLED, true).expect_err("unsaved");

    assert!(matches!(error, LoginItemError::Unsaved(_)), "{error}");
    assert!(error.to_string().starts_with(ERR_LOGIN_ITEM_UNSAVED));
    assert!(login.enabled.get());
}

#[test]
fn a_debug_build_or_a_disk_image_records_the_switch_and_leaves_the_entry() {
    for launched in [DEBUG, FROM_DISK_IMAGE] {
        let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        with_config(&data, "[general]\nlaunch_at_login = false\n");
        let state = launch(data.path(), notes.path());
        let login = FakeLoginItem::default();

        let status = set_launch_at_login_inner(&state, &login, launched, true).expect("recorded");

        assert_eq!(status, LoginItemStatus { enabled: true });
        assert!(login.asks.borrow().is_empty(), "{launched:?}");
        assert!(on_disk(&data).general.launch_at_login);
        assert_eq!(login_item_record::read(data.path()), None);
    }
}

#[test]
fn a_first_launch_from_a_disk_image_makes_no_entry() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let state = launch(data.path(), notes.path());
    let login = FakeLoginItem::default();

    assert!(!apply_first_run_choice(&state, &login, FROM_DISK_IMAGE).expect("skipped"));
    assert!(login.asks.borrow().is_empty());
}

#[test]
fn the_launch_after_one_from_a_disk_image_makes_the_entry() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&data, "[general]\nlaunch_at_login = true\n");
    let state = launch(data.path(), notes.path());
    let login = FakeLoginItem::default();

    let plan = reconcile_login_item(&state, &login, INSTALLED).expect("reconciled");

    assert_eq!(plan, LoginReconcile::SetOs(true));
    assert!(login.enabled.get());
    assert_eq!(login_item_record::read(data.path()), Some(true));
}

#[test]
fn a_hand_edit_of_the_config_moves_the_entry() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&data, "[general]\nlaunch_at_login = true\n");
    let state = launch(data.path(), notes.path());
    let login = FakeLoginItem::default();
    login.enabled.set(true);
    login_item_record::write(data.path(), true).unwrap();

    with_config(&data, "[general]\nlaunch_at_login = false\n");
    assert!(state.reload_config_from_disk());
    let plan = reconcile_login_item(&state, &login, INSTALLED).expect("reconciled");

    assert_eq!(plan, LoginReconcile::SetOs(false));
    assert!(!login.enabled.get());
    assert_eq!(login_item_record::read(data.path()), Some(false));
}

#[test]
fn an_entry_removed_in_the_system_settings_turns_the_switch_off() {
    let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&data, "[general]\nlaunch_at_login = true\n");
    let state = launch(data.path(), notes.path());
    let login = FakeLoginItem::default();
    login_item_record::write(data.path(), true).unwrap();

    let plan = reconcile_login_item(&state, &login, INSTALLED).expect("reconciled");

    assert_eq!(plan, LoginReconcile::FollowOs(false));
    assert!(login.asks.borrow().is_empty());
    assert!(!state.config.lock().unwrap().general.launch_at_login);
    assert!(!on_disk(&data).general.launch_at_login);
}

#[test]
fn nothing_moves_on_a_first_launch_a_held_config_or_a_launch_without_the_entry() {
    let first = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let state = launch(first.0.path(), first.1.path());
    assert!(state.first_run);
    let login = FakeLoginItem::default();
    assert_eq!(
        reconcile_login_item(&state, &login, INSTALLED).expect("left"),
        LoginReconcile::Leave
    );

    let held = (TempDir::new().unwrap(), TempDir::new().unwrap());
    with_config(&held.0, "[general]\nlaunch_at_login = \"yes\"\n");
    let state = launch(held.0.path(), held.1.path());
    login.enabled.set(true);
    assert_eq!(
        reconcile_login_item(&state, &login, INSTALLED).expect("left"),
        LoginReconcile::Leave
    );

    for launched in [DEBUG, FROM_DISK_IMAGE] {
        let (data, notes) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        with_config(&data, "[general]\nlaunch_at_login = false\n");
        let state = launch(data.path(), notes.path());
        assert_eq!(
            reconcile_login_item(&state, &login, launched).expect("left"),
            LoginReconcile::Leave
        );
    }
    assert!(login.asks.borrow().is_empty());
    assert!(login.enabled.get());
}
