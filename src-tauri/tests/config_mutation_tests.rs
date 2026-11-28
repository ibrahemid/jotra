//! Two changes to the config made at the same time both land, in the running
//! copy and in config.toml (TS-29), and a change whose write fails leaves the
//! running copy as it was.

use std::sync::{Arc, Barrier, Mutex as StdMutex};

use jotra_core::config::JotraConfig;
use jotra_tauri_lib::commands::activity::{
    deny_client_inner, record_approvals, return_to_waiting_inner, set_client_permission_inner,
};
use jotra_tauri_lib::commands::ai::{consent_host_inner, seed_model_inner, set_provider_inner};
use jotra_tauri_lib::commands::buffer::{save_failure_message, ERR_DISK_FULL};
use jotra_tauri_lib::commands::config::update_config_inner;
use jotra_tauri_lib::commands::first_run::dismiss_first_run_hint_inner;
use jotra_tauri_lib::commands::inbox::{clear_inbox_inner, set_inbox_path_from_path};
use jotra_tauri_lib::commands::spelling::add_ignored_word_to_state;
use jotra_tauri_lib::commands::workspace::{
    clear_workspace_root_inner, set_workspace_root_from_path,
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

const ROUNDS: usize = 40;

#[test]
fn two_concurrent_mutations_both_land() {
    let data = TempDir::new().expect("data dir");
    let notes = TempDir::new().expect("notes dir");
    let state = Arc::new(launch(data.path(), notes.path()));

    for round in 0..ROUNDS {
        let barrier = Arc::new(Barrier::new(2));
        let first = {
            let (state, barrier) = (Arc::clone(&state), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                if round == 0 {
                    dismiss_first_run_hint_inner(&state).expect("dismiss");
                } else {
                    add_ignored_word_to_state(&state, format!("alpha{round}")).expect("add");
                }
            })
        };
        let second = {
            let (state, barrier) = (Arc::clone(&state), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                add_ignored_word_to_state(&state, format!("beta{round}")).expect("add");
            })
        };
        first.join().expect("first thread");
        second.join().expect("second thread");
    }

    let path = data.path().join("config.toml");
    let on_disk = JotraConfig::parse(&std::fs::read_to_string(path).expect("config written"))
        .expect("config parses");
    let in_memory = state.config.lock().expect("config lock").clone();
    for config in [&in_memory, &on_disk] {
        assert!(config.first_run.hint_dismissed);
        for round in 0..ROUNDS {
            let beta = format!("beta{round}");
            assert!(config.spelling.ignored_words.contains(&beta), "{beta} lost");
            if round > 0 {
                let alpha = format!("alpha{round}");
                assert!(
                    config.spelling.ignored_words.contains(&alpha),
                    "{alpha} lost"
                );
            }
        }
    }
}

fn live(state: &AppState) -> JotraConfig {
    state.config.lock().expect("config lock").clone()
}

/// Puts a folder where config.toml goes, so the next write to it fails.
fn block_config_writes(data_dir: &std::path::Path) {
    let path = data_dir.join("config.toml");
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.join("held")).expect("a folder where the config goes");
}

fn deny(state: &AppState, name: &str) {
    record_approvals(state, |mcp| {
        deny_client_inner(mcp, name).map_err(|e| e.to_string())
    })
    .expect("deny");
}

type ConfigChange<'a> = (
    &'static str,
    Box<dyn Fn(&AppState) -> Result<(), String> + 'a>,
);

#[test]
fn a_change_whose_write_fails_leaves_the_running_config_as_it_was() {
    let data = TempDir::new().expect("data dir");
    let notes = TempDir::new().expect("notes dir");
    let folder = TempDir::new().expect("a folder to open");
    let state = launch(data.path(), notes.path());
    set_provider_inner(&state, "deepseek", "").expect("provider");
    deny(&state, "Zed");
    block_config_writes(data.path());
    let before = live(&state);

    let changes: Vec<ConfigChange<'_>> = vec![
        (
            "workspace",
            Box::new(|s| set_workspace_root_from_path(s, folder.path()).map(|_| ())),
        ),
        (
            "inbox",
            Box::new(|s| set_inbox_path_from_path(s, folder.path()).map(|_| ())),
        ),
        (
            "consent",
            Box::new(|s| consent_host_inner(s, "api.deepseek.com").map(|_| ())),
        ),
        (
            "provider",
            Box::new(|s| set_provider_inner(s, "openai", "gpt").map(|_| ())),
        ),
        (
            "seed",
            Box::new(|s| seed_model_inner(s, "deepseek", "deepseek-chat").map(|_| ())),
        ),
        (
            "approval",
            Box::new(|s| {
                record_approvals(s, |mcp| {
                    set_client_permission_inner(mcp, "Cursor", true, false)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
                .map(|_| ())
            }),
        ),
        (
            "undo denial",
            Box::new(|s| return_to_waiting_inner(s, "Zed", || {}).map(|_| ())),
        ),
    ];
    for (what, change) in changes {
        let error = change(&state).expect_err(what);
        assert!(
            error.starts_with("ERR_"),
            "{what}: the failure carries no code"
        );
        assert_eq!(live(&state), before, "{what} changed the running config");
    }
}

#[test]
fn a_cleared_folder_whose_write_fails_stays_set() {
    let data = TempDir::new().expect("data dir");
    let notes = TempDir::new().expect("notes dir");
    let folder = TempDir::new().expect("a folder to open");
    let state = launch(data.path(), notes.path());
    set_workspace_root_from_path(&state, folder.path()).expect("workspace");
    set_inbox_path_from_path(&state, folder.path()).expect("inbox");
    block_config_writes(data.path());
    let before = live(&state);

    assert!(clear_workspace_root_inner(&state).is_err());
    assert!(clear_inbox_inner(&state).is_err());
    assert_eq!(live(&state), before);
}

#[test]
fn a_provider_or_approval_change_and_a_settings_write_at_once_both_land() {
    let data = TempDir::new().expect("data dir");
    let notes = TempDir::new().expect("notes dir");
    let state = Arc::new(launch(data.path(), notes.path()));

    for round in 0..ROUNDS {
        let barrier = Arc::new(Barrier::new(2));
        let first = {
            let (state, barrier) = (Arc::clone(&state), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                if round % 2 == 0 {
                    set_provider_inner(&state, "deepseek", "").expect("provider");
                } else {
                    deny(&state, &format!("client{round}"));
                }
            })
        };
        let second = {
            let (state, barrier) = (Arc::clone(&state), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                add_ignored_word_to_state(&state, format!("gamma{round}")).expect("add");
            })
        };
        first.join().expect("first thread");
        second.join().expect("second thread");
    }

    let path = data.path().join("config.toml");
    let on_disk = JotraConfig::parse(&std::fs::read_to_string(path).expect("config written"))
        .expect("config parses");
    assert_eq!(on_disk.ai.provider, "deepseek");
    for round in 0..ROUNDS {
        let gamma = format!("gamma{round}");
        assert!(
            on_disk.spelling.ignored_words.contains(&gamma),
            "{gamma} lost"
        );
        if round % 2 == 1 {
            let client = format!("client{round}");
            assert!(on_disk.mcp.approval_for(&client).is_some(), "{client} lost");
        }
    }
}

#[test]
fn a_config_write_that_fails_names_why() {
    // The string the setup screen matches on, written out: the frontend holds
    // a copy of it.
    assert_eq!(ERR_DISK_FULL, "ERR_DISK_FULL");
    let full = jotra_storage::errors::StorageError::Io(std::io::Error::new(
        std::io::ErrorKind::StorageFull,
        "no space left on device",
    ));
    assert!(save_failure_message(&full).starts_with("ERR_DISK_FULL: "));
    let full = jotra_storage::errors::StorageError::AtomicWrite(
        jotra_storage::atomic::AtomicWriteError::Io(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "no space left on device",
        )),
    );
    assert!(save_failure_message(&full).starts_with("ERR_DISK_FULL: "));

    let data = TempDir::new().expect("data dir");
    let notes = TempDir::new().expect("notes dir");
    let state = launch(data.path(), notes.path());
    block_config_writes(data.path());
    let error = update_config_inner(&state, live(&state)).expect_err("nowhere to write");
    assert!(error.starts_with("ERR_"), "{error}");
}

#[cfg(unix)]
#[test]
fn a_settings_folder_that_cannot_be_written_says_so() {
    use std::os::unix::fs::PermissionsExt;
    let data = TempDir::new().expect("data dir");
    let notes = TempDir::new().expect("notes dir");
    let state = launch(data.path(), notes.path());
    let _ = std::fs::remove_file(data.path().join("config.toml"));
    std::fs::set_permissions(data.path(), std::fs::Permissions::from_mode(0o555))
        .expect("read-only");

    let written = update_config_inner(&state, live(&state));
    std::fs::set_permissions(data.path(), std::fs::Permissions::from_mode(0o755))
        .expect("writable again");

    let error = written.expect_err("the folder is read-only");
    assert!(error.starts_with("ERR_FOLDER_NOT_WRITABLE: "), "{error}");
}
