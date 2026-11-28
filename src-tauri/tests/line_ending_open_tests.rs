//! The line ending a note carries is read off its file by the open path.
//!
//! Every one of these goes through a real command rather than building a
//! `BufferDocument` with the ending already set: the detection lives in
//! `commands::file` and `commands::buffer`, so a test that hands the store a
//! finished row proves nothing about it.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::path::Path;

use jotra_core::notes::line_ending::LineEnding;
use jotra_tauri_lib::commands::buffer::{
    close_buffer_inner, read_buffer_content_inner, save_buffer_content_inner,
};
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// Opens `path`, taking the single-use authorization the gate wants.
fn open(state: &AppState, path: &Path) -> String {
    let canonical = canonicalize_for_authorization(path).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("the file opened")
        .id
}

fn recorded_ending(state: &AppState, id: &str) -> LineEnding {
    state
        .store
        .lock()
        .expect("store")
        .get(id)
        .expect("get")
        .line_ending
}

#[test]
fn a_windows_file_records_its_ending_when_it_is_first_opened() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("windows.md");
    std::fs::write(&file, "alpha\r\nbeta\r\n").unwrap();

    let id = open(&state, &file);

    assert_eq!(
        recorded_ending(&state, &id),
        LineEnding::CrLf,
        "the first open did not read the file's ending"
    );
}

#[test]
fn reopening_a_closed_tab_reads_the_files_ending_again() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "alpha\nbeta\n").unwrap();

    let id = open(&state, &file);
    assert_eq!(recorded_ending(&state, &id), LineEnding::Lf);
    close_buffer_inner(&state, &id).expect("close");

    // A checkout with core.autocrlf, or the same note edited on a Windows
    // machine, while the tab was closed.
    std::fs::write(&file, "alpha\r\nbeta\r\n").unwrap();
    let reopened = open(&state, &file);

    assert_eq!(reopened, id, "the same row should have come back");
    assert_eq!(
        recorded_ending(&state, &id),
        LineEnding::CrLf,
        "reopening the tab did not read the file again"
    );
}

#[test]
fn opening_a_note_that_is_already_open_follows_the_file() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "alpha\nbeta\n").unwrap();

    let id = open(&state, &file);
    std::fs::write(&file, "alpha\r\nbeta\r\n").unwrap();
    let again = open(&state, &file);

    assert_eq!(again, id, "the open tab should have been reused");
    assert_eq!(
        recorded_ending(&state, &id),
        LineEnding::CrLf,
        "reopening an open tab did not resync the file's ending"
    );
}

#[test]
fn a_reload_follows_a_file_that_gained_carriage_returns() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "alpha\nbeta\n").unwrap();

    let id = open(&state, &file);
    assert_eq!(recorded_ending(&state, &id), LineEnding::Lf);

    // Something outside Jotra rewrote the file while the tab was open, and the
    // user said yes to the reload.
    std::fs::write(&file, "alpha\r\nbeta\r\n").unwrap();
    read_buffer_content_inner(&state, &id).expect("reload");

    assert_eq!(
        recorded_ending(&state, &id),
        LineEnding::CrLf,
        "the reload did not follow the file's ending"
    );

    // What the row is for: the next save keeps the convention the file gained
    // instead of stripping every carriage return back out of it.
    save_buffer_content_inner(&state, &id, "alpha\nBETA\n").expect("save");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"alpha\r\nBETA\r\n",
        "the save wrote the ending the row held before the reload"
    );
}

/// The record the watcher measures a file against after a save.
///
/// `AppState::disk_state` is what `NoteFiles::last_disk_state` answers with,
/// and `modification_is_news` compares it to a fresh digest of the file. A
/// digest of the editor's LF text would differ from every byte on disk, so
/// Jotra's own save would come back to the tab as somebody else's edit.
#[test]
fn a_save_records_the_windows_files_own_digest_for_the_watcher() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "alpha\r\nbeta\r\n").unwrap();

    let id = open(&state, &file);
    assert_eq!(recorded_ending(&state, &id), LineEnding::CrLf);

    save_buffer_content_inner(&state, &id, "alpha\nBETA\n").expect("save");

    let on_disk = jotra_core::hash::sha256_bytes(&std::fs::read(&file).unwrap());
    let recorded = state.disk_state(&id).expect("the save recorded nothing");
    assert_eq!(recorded.hash, on_disk);
    assert!(
        !jotra_core::watcher::change_event::modification_is_news(
            Some(recorded.hash),
            Some(on_disk),
            false
        ),
        "the tab would be told its own save was an external change"
    );
}

/// A save that writes nothing still has to leave the record answering for the
/// file, because the watcher goes on comparing against it.
#[test]
fn a_save_of_the_text_the_file_holds_leaves_the_record_answering_for_it() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "alpha\r\nbeta\r\n").unwrap();

    let id = open(&state, &file);
    let before = std::fs::metadata(&file).unwrap().modified().ok();

    // Cmd+S with nothing typed.
    save_buffer_content_inner(&state, &id, "alpha\nbeta\n").expect("save");

    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().ok(),
        before,
        "nothing was typed, so nothing can have been written"
    );
    let on_disk = jotra_core::hash::sha256_bytes(&std::fs::read(&file).unwrap());
    assert_eq!(
        state.disk_state(&id).expect("no record").hash,
        on_disk,
        "the no-op save left the watcher a digest the file does not match"
    );
}
