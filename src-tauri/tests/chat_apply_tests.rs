//! What an applied proposal does beyond the file: what it reports, what it
//! tells the tab holding the note, and what the next unclean relaunch makes of
//! it.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::sync::mpsc;
use std::sync::Arc;

use jotra_core::events::bus::JotraEvent;
use jotra_core::hash::{sha256_bytes, sha256_hex};
use jotra_core::watcher::change_event::{modification_is_news, ExternalChange};
use jotra_tauri_lib::commands::chat::{
    announce_applied_note, apply_proposal_inner, apply_proposal_to_state, undo_proposal_to_state,
    EarlierTexts,
};
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use jotra_tauri_lib::watcher::moves::FileTracking;
use jotra_tauri_lib::watcher::open_files::{start_open_file_watcher, NoOpenNotes};
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}
/// A state with the open-file watcher running, and every event it raises.
fn watching_state(dir: &TempDir) -> (Arc<AppState>, mpsc::Receiver<JotraEvent>) {
    let state = Arc::new(make_state(dir));
    let (tx, rx) = mpsc::channel();
    state.event_bus.subscribe(move |event| {
        let _ = tx.send(event.clone());
    });
    *state.file_tracking.lock().expect("tracking slot") = Some(FileTracking::of_state(&state));
    let open_files = start_open_file_watcher(
        state.event_bus.clone(),
        state.watcher_ignore.clone(),
        &state.notes_root(),
        state.file_tracking(),
    )
    .expect("start the open file watcher");
    *state.open_file_watcher.lock().expect("watcher slot") = Some(open_files);
    (state, rx)
}

/// A note on disk under the state's notes folder, and the digest it holds.
fn seed_note(state: &AppState, name: &str, body: &str) -> (std::path::PathBuf, String) {
    let file = state.notes_root().join(name);
    std::fs::write(&file, body).expect("seed a note");
    (file, sha256_hex(body.as_bytes()))
}

#[test]
fn applying_a_proposal_whose_text_the_note_already_holds_reports_no_change() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (file, hash) = seed_note(&state, "Launch.md", "the same text\n");
    let before = std::fs::metadata(&file).expect("metadata").modified().ok();

    let outcome = apply_proposal_inner(
        &state.notes_root(),
        &NoOpenNotes,
        &state.jotra_dir,
        "probe-host",
        &file.to_string_lossy(),
        "the same text\n",
        &hash,
        None,
        |_, _, _| unreachable!(),
    )
    .expect("a proposal the note already holds is not a refusal");

    assert!(
        !outcome.changed,
        "the card would say Applied for a write that never happened"
    );
    assert_eq!(outcome.bytes, "the same text\n".len() as u64);
    assert_eq!(
        std::fs::metadata(&file).expect("metadata").modified().ok(),
        before,
        "the file was rewritten"
    );
}

#[test]
fn applying_a_proposal_that_moves_bytes_reports_a_change() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (file, hash) = seed_note(&state, "Launch.md", "before\n");

    let outcome = apply_proposal_inner(
        &state.notes_root(),
        &NoOpenNotes,
        &state.jotra_dir,
        "probe-host",
        &file.to_string_lossy(),
        "after\n",
        &hash,
        None,
        |_, _, _| unreachable!(),
    )
    .expect("the proposal applies");

    assert!(outcome.changed);
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "after\n");
}

#[test]
fn an_identical_apply_records_no_bytes_written() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (file, hash) = seed_note(&state, "Launch.md", "the same text\n");

    apply_proposal_inner(
        &state.notes_root(),
        &NoOpenNotes,
        &state.jotra_dir,
        "probe-host",
        &file.to_string_lossy(),
        "the same text\n",
        &hash,
        None,
        |_, _, _| unreachable!(),
    )
    .expect("the apply returns");

    let records = jotra_storage::activity_log::read_recent(&state.jotra_dir, usize::MAX);
    let record = records
        .iter()
        .find(|record| record.action == "apply_proposal")
        .expect("a record of the apply");
    assert_eq!(
        record.bytes, None,
        "a write that moved nothing recorded a length"
    );
}

#[test]
fn an_applied_note_that_is_open_gets_one_external_change_for_its_tab() {
    let dir = TempDir::new().expect("temp dir");
    let (state, events) = watching_state(&dir);
    let (file, _) = seed_note(&state, "Launch.md", "before\n");
    state.follow_note_path("note-1", &file);
    while events.try_recv().is_ok() {}

    let applied = b"after\n";
    std::fs::write(&file, applied).expect("the apply writes");
    let told = announce_applied_note(&state, &file, applied);

    assert_eq!(told.as_deref(), Some("note-1"));
    let event = events.try_recv().expect("the tab was told");
    match event {
        JotraEvent::BufferExternal {
            buffer_id, change, ..
        } => {
            assert_eq!(buffer_id, "note-1");
            assert_eq!(change, ExternalChange::Modified);
        }
        other => panic!("expected an external change, got {other:?}"),
    }

    // What the watcher will deliver for the same write, judged the way the
    // watcher judges it: with the bytes on record it is not news, so the tab
    // gets one event rather than a reload followed by a bar.
    let recorded = state.disk_state("note-1").expect("the bytes are on record");
    assert_eq!(recorded.hash, sha256_bytes(applied));
    assert!(
        !modification_is_news(Some(recorded.hash), Some(sha256_bytes(applied)), false),
        "the watcher's own delivery would reach the tab a second time"
    );
}

#[test]
fn an_undo_puts_the_text_back_and_tells_the_tab_holding_the_note() {
    let dir = TempDir::new().expect("temp dir");
    let (state, events) = watching_state(&dir);
    state.note_history.set_notes_root(state.notes_root());
    let (file, before) = seed_note(&state, "Launch.md", "before\n");
    state.follow_note_path("note-1", &file);

    let applied = apply_proposal_inner(
        &state.notes_root(),
        &NoOpenNotes,
        &state.jotra_dir,
        "probe-host",
        "Launch.md",
        "after\n",
        &before,
        Some(&state.note_history),
        |_, _, _| unreachable!(),
    )
    .expect("applied");
    assert!(applied.undoable);
    while events.try_recv().is_ok() {}

    let undone = undo_proposal_to_state(
        &state,
        &EarlierTexts::default(),
        "probe-host",
        "Launch.md",
        &before,
        &applied.hash,
    )
    .expect("undone");

    assert!(undone.changed);
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "before\n");
    let told = std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(
            event,
            JotraEvent::BufferExternal { ref buffer_id, change: ExternalChange::Modified, .. }
                if buffer_id == "note-1"
        )
    });
    assert!(told, "the tab holding the note was not told");
    assert_eq!(
        state.disk_state("note-1").expect("on record").hash,
        sha256_bytes(b"before\n")
    );
}

#[test]
fn an_undo_puts_back_a_file_outside_the_folder_through_its_tab() {
    let dir = TempDir::new().expect("temp dir");
    let (state, events) = watching_state(&dir);
    let file = dir.path().join("README.md");
    std::fs::write(&file, "the readme text\n").expect("seed the file");
    let path = canonicalize_for_authorization(&file).expect("canonical");
    state.authorized_paths.record_for_open(path.clone());
    let tab = open_file_from_path(&state, &path)
        .expect("open")
        .doc
        .expect("a row")
        .id;
    let earlier = EarlierTexts::default();
    let before = sha256_hex(b"the readme text\n");

    let applied = apply_proposal_to_state(
        &state,
        &earlier,
        "probe-host",
        &path,
        "a tighter readme\n",
        &before,
    )
    .expect("applied");
    assert!(applied.undoable, "the text the apply replaced is kept");
    while events.try_recv().is_ok() {}

    let undone = undo_proposal_to_state(
        &state,
        &earlier,
        "probe-host",
        &path,
        &before,
        &applied.hash,
    )
    .expect("undone");

    assert!(undone.changed);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "the readme text\n"
    );
    let told = std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(
            event,
            JotraEvent::BufferExternal { ref buffer_id, change: ExternalChange::Modified, .. }
                if *buffer_id == tab
        )
    });
    assert!(told, "the tab holding the file was not told");
    assert_eq!(
        state.disk_state(&tab).expect("on record").hash,
        sha256_bytes(b"the readme text\n")
    );
}

/// A file outside the notes folder holding `seeded`, open in a tab, with
/// `proposed` applied to it through its card: the state, the file's path, the
/// texts the card keeps for Undo, the digest the offer was made against and the
/// digest the apply reported.
fn applied_to_an_outside_file(
    dir: &TempDir,
    seeded: &[u8],
    proposed: &str,
) -> (AppState, String, EarlierTexts, String, String) {
    let state = make_state(dir);
    let file = dir.path().join("README.md");
    std::fs::write(&file, seeded).expect("seed the file");
    let path = canonicalize_for_authorization(&file).expect("canonical");
    state.authorized_paths.record_for_open(path.clone());
    open_file_from_path(&state, &path)
        .expect("open")
        .doc
        .expect("a row");
    let earlier = EarlierTexts::default();
    let before = sha256_hex(seeded);
    let applied = apply_proposal_to_state(&state, &earlier, "probe-host", &path, proposed, &before)
        .expect("applied");
    assert!(applied.undoable, "the text the apply replaced is kept");
    assert_ne!(
        std::fs::read(&path).expect("read"),
        proposed.as_bytes(),
        "the file keeps its own form"
    );
    (state, path, earlier, before, applied.hash)
}

#[test]
fn an_undo_restores_the_crlf_bytes_of_a_file_outside_the_folder() {
    let dir = TempDir::new().expect("temp dir");
    let seeded = b"first line\r\nsecond line\r\n";
    let (state, path, earlier, before, applied) =
        applied_to_an_outside_file(&dir, seeded, "first line\nsecond line, tightened\n");

    undo_proposal_to_state(&state, &earlier, "probe-host", &path, &before, &applied)
        .expect("undone");

    assert_eq!(std::fs::read(&path).expect("read"), seeded);
}

#[test]
fn an_undo_restores_the_byte_order_mark_of_a_file_outside_the_folder() {
    let dir = TempDir::new().expect("temp dir");
    let seeded = b"\xef\xbb\xbfthe readme text\n";
    let (state, path, earlier, before, applied) =
        applied_to_an_outside_file(&dir, seeded, "a tighter readme\n");

    undo_proposal_to_state(&state, &earlier, "probe-host", &path, &before, &applied)
        .expect("undone");

    assert_eq!(std::fs::read(&path).expect("read"), seeded);
}

#[test]
fn an_undo_restores_a_file_outside_the_folder_in_a_legacy_encoding() {
    let dir = TempDir::new().expect("temp dir");
    let seeded = b"Le caf\xe9 est ouvert. Une r\xe9union demain.\n";
    let (state, path, earlier, before, applied) =
        applied_to_an_outside_file(&dir, seeded, "Le caf\u{e9} est ferm\u{e9}.\n");

    undo_proposal_to_state(&state, &earlier, "probe-host", &path, &before, &applied)
        .expect("undone");

    assert_eq!(std::fs::read(&path).expect("read"), seeded);
}

#[test]
fn an_undo_of_a_file_outside_the_folder_another_program_edited_is_refused() {
    let dir = TempDir::new().expect("temp dir");
    let seeded = b"Le caf\xe9 est ouvert. Une r\xe9union demain.\n";
    let (state, path, earlier, before, applied) =
        applied_to_an_outside_file(&dir, seeded, "Le caf\u{e9} est ferm\u{e9}.\n");
    let landed = std::fs::read(&path).expect("read");
    let edited = b"Le caf\xe9 est ferm\xe9 le lundi.\n";
    std::fs::write(&path, edited).expect("another program writes");

    let refused = undo_proposal_to_state(&state, &earlier, "probe-host", &path, &before, &applied)
        .expect_err("the undo is refused");

    assert!(refused.contains("changed since the change was applied"));
    assert_eq!(std::fs::read(&path).expect("read"), edited);
    std::fs::write(&path, &landed).expect("the applied bytes come back");
    undo_proposal_to_state(&state, &earlier, "probe-host", &path, &before, &applied)
        .expect("undone once the file holds what the apply wrote");
    assert_eq!(std::fs::read(&path).expect("read"), seeded);
}

#[test]
fn an_applied_note_nobody_has_open_tells_no_tab() {
    let dir = TempDir::new().expect("temp dir");
    let (state, events) = watching_state(&dir);
    let (file, _) = seed_note(&state, "Launch.md", "before\n");
    while events.try_recv().is_ok() {}

    let applied = b"after\n";
    std::fs::write(&file, applied).expect("the apply writes");

    assert_eq!(announce_applied_note(&state, &file, applied), None);
    assert!(
        events.try_recv().is_err(),
        "a note nobody has open was announced to a tab"
    );
    assert!(state.disk_state("note-1").is_none());
}

/// A note the last session had open, with its row stamped where the launch
/// before this one left it.
fn open_row(state: &AppState, id: &str, file: &std::path::Path, content: &str) {
    let then = chrono::Utc::now() - chrono::Duration::seconds(30);
    let doc = jotra_core::buffer::document::BufferDocument {
        id: id.to_string(),
        title: "Launch".to_string(),
        filename: format!("{id}.md"),
        status: jotra_core::buffer::document::BufferStatus::Active,
        language: None,
        source_path: Some(file.to_string_lossy().into_owned()),
        cursor_pos: 0,
        scroll_pos: 0,
        tab_order: 0,
        created_at: then,
        updated_at: then,
        closed_at: None,
        read_only: false,
        size_bytes: content.len() as u64,
        line_ending: jotra_core::notes::line_ending::LineEnding::Lf,
        encoding: None,
    };
    let store = state.store.lock().expect("store");
    store.open_from_path(&doc, content).expect("open the note");
}

#[test]
fn an_applied_edit_is_not_reverted_by_recovery() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (file, hash) = seed_note(&state, "Launch.md", "before\n");
    open_row(&state, "note-1", &file, "before\n");

    // The 120 s heartbeat, which used to copy every open note's file into an
    // unclean snapshot that the next launch wrote back.
    jotra_tauri_lib::heartbeat::beat(&state);

    apply_proposal_inner(
        &state.notes_root(),
        &NoOpenNotes,
        &state.jotra_dir,
        "probe-host",
        &file.to_string_lossy(),
        "after\n",
        &hash,
        None,
        |_, _, _| unreachable!(),
    )
    .expect("the proposal applies");

    // What the tab does with the change: it reads the file back, which is
    // also where the row learns what the file now holds.
    let read = jotra_tauri_lib::commands::buffer::read_buffer_content_inner(&state, "note-1")
        .expect("the tab reloads");
    assert_eq!(String::from_utf8(read).expect("text"), "after\n");

    // The next launch, after a crash: nothing it restores may land over the
    // edit, because nothing but text a save could not write is kept for it.
    let restored = {
        let store = state.store.lock().expect("store");
        jotra_tauri_lib::state::restore_unsaved_text(
            &store,
            &state.notes_root(),
            state.default_extension(),
        )
    };
    assert!(
        restored.recovered.is_empty(),
        "the launch would write something back over the applied edit"
    );
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "after\n");
}

#[test]
fn an_apply_to_an_open_note_in_the_folder_waits_for_its_tabs_turn() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (file, hash) = seed_note(&state, "Launch.md", "before\n");
    open_row(&state, "note-1", &file, "before\n");
    let earlier = EarlierTexts::default();

    let held = state.buffer_writes.turn("note-1");
    std::thread::scope(|scope| {
        let apply = scope.spawn(|| {
            apply_proposal_to_state(
                &state,
                &earlier,
                "probe-host",
                "Launch.md",
                "after\n",
                &hash,
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !apply.is_finished(),
            "the apply wrote while a save of the tab held its turn"
        );
        assert_eq!(std::fs::read_to_string(&file).expect("read"), "before\n");
        drop(held);
        let outcome = apply
            .join()
            .expect("the apply thread")
            .expect("the apply lands once the turn is free");
        assert!(outcome.changed);
    });
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "after\n");
}

#[test]
fn an_undo_of_an_open_note_in_the_folder_waits_for_its_tabs_turn() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    state.note_history.set_notes_root(state.notes_root());
    let (file, before) = seed_note(&state, "Launch.md", "before\n");
    open_row(&state, "note-1", &file, "before\n");
    let earlier = EarlierTexts::default();
    let applied = apply_proposal_to_state(
        &state,
        &earlier,
        "probe-host",
        "Launch.md",
        "after\n",
        &before,
    )
    .expect("applied");

    let held = state.buffer_writes.turn("note-1");
    std::thread::scope(|scope| {
        let undo = scope.spawn(|| {
            undo_proposal_to_state(
                &state,
                &earlier,
                "probe-host",
                "Launch.md",
                &before,
                &applied.hash,
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !undo.is_finished(),
            "the undo wrote while a save of the tab held its turn"
        );
        assert_eq!(std::fs::read_to_string(&file).expect("read"), "after\n");
        drop(held);
        undo.join()
            .expect("the undo thread")
            .expect("the undo lands once the turn is free");
    });
    assert_eq!(std::fs::read_to_string(&file).expect("read"), "before\n");
}
