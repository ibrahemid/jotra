//! The heartbeat against a file whose reads never return.
//!
//! A tab open on a network share whose server went away holds a file that
//! nothing can read: a read blocks until the share answers. A FIFO with no
//! writer is the same file on any Unix machine, which is what lets the stall
//! be built here rather than waited for.
//!
//! Every save takes the store, and the save commands run on the main thread,
//! so a heartbeat that holds the store across that read freezes the window
//! until the share comes back.

#![cfg(unix)]

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use jotra_core::buffer::document::{BufferDocument, BufferStatus};
use jotra_tauri_lib::commands::buffer::save_buffer_content_inner;
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::heartbeat;
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

/// Long enough for a beat and a save on an idle machine many times over, and
/// short enough that a stalled run fails rather than hangs the suite.
const DEADLINE: Duration = Duration::from_secs(5);

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// A file whose reads block until something writes to it, which nothing will.
fn stalled_file(dir: &Path) -> PathBuf {
    let share = dir.join("share");
    std::fs::create_dir_all(&share).expect("share folder");
    let path = share.join("Stalled.md");
    let made = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed for {}", path.display());
    path
}

/// The row of a tab open on the stalled file, inserted directly: opening it
/// through the command layer would read it, and the read would block there.
fn tab_on(id: &str, path: &Path) -> BufferDocument {
    let now = chrono::Utc::now();
    BufferDocument {
        id: id.to_string(),
        title: "Stalled".to_string(),
        filename: format!("{id}.md"),
        status: BufferStatus::Active,
        language: None,
        source_path: Some(path.to_string_lossy().into_owned()),
        cursor_pos: 0,
        scroll_pos: 0,
        tab_order: 0,
        created_at: now,
        updated_at: now,
        closed_at: None,
        read_only: false,
        size_bytes: 0,
        line_ending: jotra_core::notes::line_ending::LineEnding::Lf,
        encoding: None,
    }
}

/// Opens a note in the notes folder the way the frontend reaches one.
fn open_note(state: &AppState, name: &str, content: &str) -> String {
    let path = state.notes_root().join(name);
    std::fs::write(&path, content).expect("write");
    let canonical = canonicalize_for_authorization(&path).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("the file opened")
        .id
}

#[test]
fn a_save_completes_while_a_heartbeat_read_is_hung() {
    let dir = TempDir::new().expect("temp dir");
    let state = Arc::new(make_state(&dir));

    let stalled = stalled_file(dir.path());
    state
        .store
        .lock()
        .expect("store")
        .insert(&tab_on("stalled", &stalled))
        .expect("insert the stalled tab");
    let typing = open_note(&state, "Typing.md", "first line\n");

    let (beat_done, beat_finished) = mpsc::channel();
    let beat_state = Arc::clone(&state);
    std::thread::spawn(move || {
        heartbeat::beat(&beat_state);
        let _ = beat_done.send(());
    });
    let beat_returned_on_its_own = beat_finished.recv_timeout(DEADLINE).is_ok();
    let store_held_by_the_beat = !beat_returned_on_its_own
        && matches!(
            state.store.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        );

    let (save_done, save_finished) = mpsc::channel();
    let save_state = Arc::clone(&state);
    let typing_id = typing.clone();
    std::thread::spawn(move || {
        let outcome = save_buffer_content_inner(&save_state, &typing_id, "typed meanwhile\n");
        let _ = save_done.send(outcome);
    });
    let saved = save_finished.recv_timeout(DEADLINE);

    // Lets a stalled beat go, so the run ends either way. Only a beat that is
    // still waiting is reading the file: opening the write end with nobody
    // reading would block this thread instead.
    if !beat_returned_on_its_own {
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&stalled)
                .expect("open the write end"),
        );
        let _ = beat_finished.recv_timeout(DEADLINE);
        let _ = save_finished.recv_timeout(DEADLINE);
    }

    let save_waited = saved.is_err();
    assert!(
        beat_returned_on_its_own,
        "the heartbeat read a tab's file and waited on it \
         (holding the store: {store_held_by_the_beat}; a save waited behind it: {save_waited})"
    );
    let saved = saved.expect("a save waited behind the heartbeat");
    assert!(saved.is_ok(), "the save failed: {saved:?}");
    assert_eq!(
        std::fs::read_to_string(state.notes_root().join("Typing.md")).expect("read"),
        "typed meanwhile\n"
    );
}
