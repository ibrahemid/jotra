//! The language chosen for a file with Change Language.
//!
//! An open records no language: the editor reads the file's name every time.
//! `set_buffer_language` records a choice, clears it with `None`, refuses an
//! id no picker could have sent, and the choice stays with the file across a
//! close and reopen.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;

use jotra_tauri_lib::commands::buffer::{close_buffer_inner, set_buffer_language_inner};
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

fn open(state: &AppState, path: &std::path::Path) -> jotra_core::buffer::document::BufferDocument {
    let canonical = canonicalize_for_authorization(path).expect("canonical");
    open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("an opened file has a row")
}

#[test]
fn an_opened_file_records_no_language() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = state.notes_root().join("page.tsx");
    std::fs::write(&file, "export const A = () => <div />;\n").unwrap();

    let doc = open(&state, &file);

    assert_eq!(doc.language, None);
    let stored = state.store.lock().unwrap().get(&doc.id).unwrap();
    assert_eq!(stored.language, None);
}

#[test]
fn a_chosen_language_is_recorded_and_answered() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = state.notes_root().join("snippet.txt");
    std::fs::write(&file, "print('hi')\n").unwrap();
    let doc = open(&state, &file);

    let answered = set_buffer_language_inner(&state, &doc.id, Some("python")).unwrap();

    assert_eq!(answered.id, doc.id);
    assert_eq!(answered.language.as_deref(), Some("python"));
    let stored = state.store.lock().unwrap().get(&doc.id).unwrap();
    assert_eq!(stored.language.as_deref(), Some("python"));
}

#[test]
fn clearing_the_choice_hands_the_file_back_to_its_name() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = state.notes_root().join("main.rs");
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let doc = open(&state, &file);
    set_buffer_language_inner(&state, &doc.id, Some("plaintext")).unwrap();

    let answered = set_buffer_language_inner(&state, &doc.id, None).unwrap();

    assert_eq!(answered.language, None);
}

#[test]
fn the_choice_stays_with_the_file_across_a_close_and_reopen() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = state.notes_root().join("settings.conf");
    std::fs::write(&file, "key = 1\n").unwrap();
    let doc = open(&state, &file);
    set_buffer_language_inner(&state, &doc.id, Some("toml")).unwrap();
    close_buffer_inner(&state, &doc.id).unwrap();

    let reopened = open(&state, &file);

    assert_eq!(reopened.id, doc.id);
    assert_eq!(reopened.language.as_deref(), Some("toml"));
}

#[test]
fn an_id_no_picker_could_send_is_refused_and_nothing_is_recorded() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = state.notes_root().join("notes.txt");
    std::fs::write(&file, "text\n").unwrap();
    let doc = open(&state, &file);

    for bad in ["", " python", "py\u{7}thon", &"a".repeat(65)] {
        let error = set_buffer_language_inner(&state, &doc.id, Some(bad)).unwrap_err();
        assert!(
            error.starts_with("invalid language: "),
            "{bad:?} gave {error}"
        );
    }

    let stored = state.store.lock().unwrap().get(&doc.id).unwrap();
    assert_eq!(stored.language, None);
}

#[test]
fn a_buffer_that_does_not_exist_is_an_error() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    assert!(set_buffer_language_inner(&state, "no-such-buffer", Some("python")).is_err());
}
