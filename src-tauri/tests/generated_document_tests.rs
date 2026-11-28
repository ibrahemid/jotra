//! Acceptance coverage for `commands::file::open_generated_document`
//! (ADR-028 §1): a document Jotra writes rather than the user must never mint
//! a file in the notes folder, must land at a fixed, rewritable path under
//! the data directory, and must refuse a save through the ordinary read-only
//! mechanism. A plain user note is unaffected and still mints into the notes
//! folder on its first keystroke.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::sync::{Arc, Mutex};

use jotra_tauri_lib::commands::buffer::{
    close_buffer_inner, read_buffer_content_inner, save_buffer_content_inner,
};
use jotra_tauri_lib::commands::file::open_generated_document;
use jotra_tauri_lib::commands::notices::{
    retire_old_notices, RETIRED_NOTICES_TITLE, THIRD_PARTY_NOTICES_TITLE,
};
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

const NOTICES_TITLE: &str = "Third-party licences";

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

fn count_external_events(state: &AppState) -> Arc<Mutex<u32>> {
    let count = Arc::new(Mutex::new(0u32));
    let count_clone = count.clone();
    state.event_bus.subscribe(move |event| {
        if let jotra_core::events::bus::JotraEvent::BufferExternal { .. } = event {
            *count_clone.lock().unwrap() += 1;
        }
    });
    count
}

fn is_empty_dir(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true)
}

#[test]
fn opening_a_generated_document_creates_no_file_in_the_notes_folder() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let result = open_generated_document(&state, NOTICES_TITLE, "# Third-party notices\nMIT\n")
        .expect("open");

    assert!(
        is_empty_dir(&state.notes_root()),
        "a generated document must never mint into the notes folder"
    );
    let expected = state
        .jotra_dir
        .join("generated")
        .join(format!("{NOTICES_TITLE}.md"));
    assert_eq!(
        std::fs::read_to_string(&expected).unwrap(),
        "# Third-party notices\nMIT\n"
    );
    let expected_canonical = jotra_tauri_lib::security::canonicalize_for_authorization(&expected)
        .expect("canonicalize expected path");
    assert_eq!(
        result
            .doc
            .as_ref()
            .expect("the file opened")
            .source_path
            .as_deref(),
        Some(expected_canonical.as_str()),
        "the buffer points at the file under the data directory"
    );
    assert!(
        result.doc.as_ref().expect("the file opened").read_only,
        "a generated document is read-only"
    );
}

#[test]
fn a_second_open_rewrites_the_same_file_in_place() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let first = open_generated_document(&state, NOTICES_TITLE, "first version").expect("open");
    let second = open_generated_document(&state, NOTICES_TITLE, "second version").expect("reopen");

    assert_eq!(
        first.doc.as_ref().expect("the file opened").id,
        second.doc.as_ref().expect("the file opened").id,
        "the same buffer is reused"
    );
    let generated_dir = state.jotra_dir.join("generated");
    let entries: Vec<_> = std::fs::read_dir(&generated_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "reopening must not mint a dedupe sibling: {entries:?}"
    );
    let expected = generated_dir.join(format!("{NOTICES_TITLE}.md"));
    assert_eq!(
        std::fs::read_to_string(&expected).unwrap(),
        "second version",
        "the file is rewritten in place"
    );

    let store = state.store.lock().unwrap();
    let active = store
        .list_by_status(jotra_core::buffer::document::BufferStatus::Active)
        .unwrap();
    assert_eq!(
        active.len(),
        1,
        "reopening must not leave a second buffer row behind"
    );
}

#[test]
fn saving_a_generated_document_is_refused() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let opened = open_generated_document(&state, NOTICES_TITLE, "original text").expect("open");

    let err = save_buffer_content_inner(
        &state,
        &opened.doc.as_ref().expect("the file opened").id,
        "hijacked",
    )
    .expect_err("a read-only buffer must refuse a save");
    assert!(err.contains("read-only"), "unexpected error: {err}");

    let expected = state
        .jotra_dir
        .join("generated")
        .join(format!("{NOTICES_TITLE}.md"));
    assert_eq!(
        std::fs::read_to_string(&expected).unwrap(),
        "original text",
        "the refused save must not have touched the file"
    );
}

#[test]
fn a_plain_new_note_still_mints_a_dated_file_in_the_notes_folder() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let doc = {
        let store = state.store.lock().unwrap();
        let mut mgr = jotra_core::buffer::manager::BufferManager::new();
        let doc = mgr.create_buffer(None).expect("mint");
        store.insert(&doc).expect("persist");
        doc
    };

    save_buffer_content_inner(&state, &doc.id, "just notes").expect("save");

    let expected = state.notes_root().join(format!(
        "{}.{}",
        jotra_core::notes::date_stem(doc.created_at),
        state.default_extension().as_str()
    ));
    assert_eq!(std::fs::read_to_string(&expected).unwrap(), "just notes");
    assert!(
        is_empty_dir(&state.jotra_dir.join("generated")),
        "a user note has no business under the generated documents folder"
    );
}

#[test]
fn three_open_and_close_cycles_leave_one_row() {
    // Closing the tab and opening the document again must find the row it
    // left in History, or every open mints another and the user's History
    // fills with a document they never wrote (ADR-028 §1).
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let mut ids = std::collections::HashSet::new();
    for _ in 0..3 {
        let opened = open_generated_document(&state, NOTICES_TITLE, "# Third-party notices\nMIT\n")
            .expect("open");
        ids.insert(opened.doc.as_ref().expect("the file opened").id.clone());
        close_buffer_inner(&state, &opened.doc.as_ref().expect("the file opened").id)
            .expect("close the tab");
    }

    assert_eq!(ids.len(), 1, "each open must reuse the one row: {ids:?}");

    let store = state.store.lock().unwrap();
    let active = store
        .list_by_status(jotra_core::buffer::document::BufferStatus::Active)
        .unwrap();
    let history = store
        .list_by_status(jotra_core::buffer::document::BufferStatus::History)
        .unwrap();
    assert!(active.is_empty(), "the last cycle left the tab closed");
    assert_eq!(history.len(), 1, "one row, not one per open: {history:?}");
}

#[test]
fn a_generated_document_reopened_after_a_restart_reports_no_outside_change() {
    let dir = TempDir::new().unwrap();
    let text = "# Third-party notices\nMIT\n";

    let first_launch = make_state(&dir);
    let opened = open_generated_document(&first_launch, NOTICES_TITLE, text).expect("open");
    drop(first_launch);

    // A relaunch over the same data folder: the row is restored, and the
    // frontend loading its tab is the read that records what the file holds.
    let state = make_state(&dir);
    read_buffer_content_inner(&state, &opened.doc.as_ref().expect("the file opened").id)
        .expect("load the restored tab");

    let count = count_external_events(&state);
    let reopened = open_generated_document(&state, NOTICES_TITLE, text).expect("reopen");

    assert_eq!(
        reopened.doc.as_ref().expect("the file opened").id,
        opened.doc.as_ref().expect("the file opened").id
    );
    assert_eq!(
        *count.lock().unwrap(),
        0,
        "nothing changed under the document, so nothing may be announced"
    );
}

fn generated_names(state: &AppState) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(state.jotra_dir.join("generated"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn rows(state: &AppState) -> usize {
    let store = state.store.lock().unwrap();
    [
        jotra_core::buffer::document::BufferStatus::Active,
        jotra_core::buffer::document::BufferStatus::History,
    ]
    .into_iter()
    .map(|status| store.list_by_status(status).unwrap().len())
    .sum()
}

#[test]
fn the_listing_an_earlier_build_left_is_removed_with_its_closed_tab() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let old = open_generated_document(&state, RETIRED_NOTICES_TITLE, "old").expect("open");
    close_buffer_inner(&state, &old.doc.expect("the file opened").id).expect("close the tab");
    open_generated_document(&state, THIRD_PARTY_NOTICES_TITLE, "new").expect("open");

    assert!(retire_old_notices(&state).expect("retire"));

    assert_eq!(
        generated_names(&state),
        [format!("{THIRD_PARTY_NOTICES_TITLE}.md")]
    );
    assert_eq!(rows(&state), 1, "only the current listing's tab is left");
}

#[test]
fn an_earlier_listing_still_open_in_a_tab_is_kept() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    open_generated_document(&state, RETIRED_NOTICES_TITLE, "old").expect("open");

    assert!(!retire_old_notices(&state).expect("retire"));

    assert_eq!(
        generated_names(&state),
        [format!("{RETIRED_NOTICES_TITLE}.md")]
    );
    assert_eq!(rows(&state), 1);
}

#[test]
fn a_data_folder_with_no_earlier_listing_is_left_alone() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    open_generated_document(&state, THIRD_PARTY_NOTICES_TITLE, "new").expect("open");

    assert!(!retire_old_notices(&state).expect("retire"));
    assert_eq!(
        generated_names(&state),
        [format!("{THIRD_PARTY_NOTICES_TITLE}.md")]
    );
}

#[test]
fn startup_retires_the_earlier_listing() {
    let lib_rs = include_str!("../src/lib.rs");
    assert!(lib_rs.contains("commands::notices::retire_old_notices(&state)"));
}
