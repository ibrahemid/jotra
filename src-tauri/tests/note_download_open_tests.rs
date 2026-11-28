//! Opening a note whose bytes a sync provider has not put on this machine.
//!
//! The open must answer from a stat: `classify_path` sniffs the first bytes,
//! and on a placeholder that sniff blocks the IPC thread until the provider
//! has fetched the whole file. These tests drive the arm through the
//! dev-build dataless hook, so no provider is needed.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;

#[cfg(unix)]
use jotra_tauri_lib::commands::file::open_file_confirmed_from_path;
use jotra_tauri_lib::commands::file::{
    authorize_download, clear_dataless_for_test, mark_dataless_for_test, open_file_from_path,
};
use jotra_tauri_lib::commands::materialise::{cancel_download, MaterialiseState};
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// Every test marks paths on its own thread; clear them so a later test on a
/// reused thread does not inherit a placeholder.
struct Marked;

impl Drop for Marked {
    fn drop(&mut self) {
        clear_dataless_for_test();
    }
}

fn mark(canonical: &str) -> Marked {
    mark_dataless_for_test(canonical);
    Marked
}

#[test]
fn opening_a_note_that_is_not_downloaded_reads_nothing_and_names_the_state() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = state.notes_root().join("away.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    let _marked = mark(&canonical);

    let result =
        open_file_from_path(&state, &canonical).expect("the open is answered, not refused");

    assert!(
        result.doc.is_none(),
        "nothing is registered for a placeholder"
    );
    let jotra_core::file_ops::FileOpenMode::NotDownloaded { path, .. } = &result.mode else {
        panic!("expected the not-downloaded mode, got {:?}", result.mode);
    };
    assert_eq!(path, &canonical);
    assert_eq!(
        state.authorized_paths.pending_open_len(),
        0,
        "a note the notes folder already covers gets no token out of this"
    );
}

#[test]
fn a_note_that_is_not_downloaded_leaves_the_watcher_nothing_to_measure() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = state.notes_root().join("waiting.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    let _marked = mark(&canonical);

    let result = open_file_from_path(&state, &canonical).expect("open");
    assert!(result.doc.is_none());

    // The download rewrites the file under the tab. A note recorded here would
    // be measured against a file the provider is still filling in, so the
    // watcher would call the download a change somebody else made, or call the
    // placeholder a removal.
    assert!(
        state
            .last_disk_hash
            .lock()
            .expect("disk state map")
            .is_empty(),
        "a note still waiting on its bytes has no digest to record"
    );
    assert!(
        state
            .source_records
            .lock()
            .expect("source record map")
            .is_empty(),
        "a note still waiting on its bytes has no file to follow"
    );
}

#[test]
fn a_note_that_arrives_is_recorded_the_way_a_plain_open_records_one() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    // The same bytes at two paths, one opened through the download arm and one
    // opened directly, so the records are compared rather than pinned.
    let waited = state.notes_root().join("waited.md");
    let plain = state.notes_root().join("plain.md");
    std::fs::write(&waited, "# here now\n").unwrap();
    std::fs::write(&plain, "# here now\n").unwrap();
    let waited = canonicalize_for_authorization(&waited).unwrap();
    let plain = canonicalize_for_authorization(&plain).unwrap();

    let marked = mark(&waited);
    assert!(open_file_from_path(&state, &waited)
        .expect("open")
        .doc
        .is_none());
    drop(marked);

    let arrived = open_file_from_path(&state, &waited).expect("the bytes landed");
    let arrived = arrived.doc.expect("the note is registered once it is here");
    let control = open_file_from_path(&state, &plain).expect("open");
    let control = control.doc.expect("the file opened");

    let arrived_record = state
        .source_record(&arrived.id)
        .expect("a note that arrived is followed");
    let control_record = state
        .source_record(&control.id)
        .expect("a note opened plainly is followed");
    assert_eq!(
        arrived_record.state, control_record.state,
        "a download that landed is present the way any open file is"
    );
    assert!(
        arrived_record.identity.is_some(),
        "the file the download left behind is described, not left unidentified"
    );
    assert_eq!(
        state.disk_state(&arrived.id).map(|d| d.hash),
        state.disk_state(&control.id).map(|d| d.hash),
        "the same bytes are recorded under the same digest either way"
    );
}

#[test]
fn a_note_that_is_not_downloaded_can_be_opened_again_once_it_arrives() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = dir.path().join("dropped.md");
    std::fs::write(&note, "# here now\n").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    state.authorized_paths.record_for_open(canonical.clone());

    // The first open spends the token and hands back the download state.
    let marked = mark(&canonical);
    let first = open_file_from_path(&state, &canonical).expect("open");
    assert!(first.doc.is_none());
    drop(marked);

    // The bytes land, and the frontend opens the note again. The token the
    // first open recorded is what authorizes this one.
    let second = open_file_from_path(&state, &canonical).expect("the second open succeeds");
    let doc = second.doc.expect("the note is registered once it is here");
    assert_eq!(doc.source_path.as_deref(), Some(canonical.as_str()));
}

#[test]
fn a_second_look_at_a_note_that_is_still_away_leaves_one_token() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = dir.path().join("still-away.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    state.authorized_paths.record_for_open(canonical.clone());

    // An open that answers with no note leaves the token where it was, so
    // asking again neither loses the authorization nor stacks a second one up.
    let _marked = mark(&canonical);
    for _ in 0..3 {
        let result = open_file_from_path(&state, &canonical).expect("open");
        assert!(result.doc.is_none());
        assert_eq!(state.authorized_paths.pending_open_len(), 1);
    }
    assert!(state.authorized_paths.is_pending_open(&canonical));
}

#[test]
fn a_downloaded_note_opens_the_way_it_always_did() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = state.notes_root().join("here.md");
    std::fs::write(&note, "# here\n").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();

    let result = open_file_from_path(&state, &canonical).expect("open");
    assert_eq!(result.mode, jotra_core::file_ops::FileOpenMode::Normal);
    assert!(result.doc.is_some());
}

#[test]
fn the_download_gate_keeps_the_open_token_for_the_open_that_follows() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = dir.path().join("outside.md");
    std::fs::write(&note, "bytes").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    state.authorized_paths.record_for_open(canonical.clone());

    let gated = authorize_download(&state, &canonical).expect("a pending open authorizes it");
    assert_eq!(gated, canonical);

    // Still spendable: the download did not consume it.
    let opened = open_file_from_path(&state, &canonical).expect("open");
    assert!(opened.doc.is_some());
}

#[test]
fn an_open_that_failed_after_the_bytes_landed_leaves_the_retry_authorized() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    // Outside the notes, workspace and inbox roots, so the token is the only
    // thing that can authorize this note.
    let note = dir.path().join("outside-away.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    state.authorized_paths.record_for_open(canonical.clone());

    let marked = mark(&canonical);
    let away = open_file_from_path(&state, &canonical).expect("open");
    assert!(away.doc.is_none());
    assert_eq!(state.authorized_paths.pending_open_len(), 1);
    drop(marked);

    // The bytes land and the open the frontend performs fails. The pane keeps
    // the note and says to open it again, so the permission that second
    // attempt needs has to survive the failure.
    std::fs::remove_file(&note).unwrap();
    assert!(open_file_from_path(&state, &canonical).is_err());
    assert_eq!(state.authorized_paths.pending_open_len(), 1);

    // The retry, and it opens the note.
    std::fs::write(&note, "# here now\n").unwrap();
    let opened = open_file_from_path(&state, &canonical).expect("the retry opens the note");
    assert!(opened.doc.is_some());
    // Spent by the open it was granted for, and only by that one.
    assert_eq!(state.authorized_paths.pending_open_len(), 0);
}

#[test]
fn cancelling_a_download_stops_the_wait_and_gives_the_permission_back() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let downloads = MaterialiseState::default();

    let note = dir.path().join("outside.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    state.authorized_paths.record_for_open(canonical.clone());

    cancel_download(&state, &downloads, &canonical).expect("the tab's token authorizes the cancel");

    assert!(!state.authorized_paths.is_pending_open(&canonical));
    // And with the permission gone, the path is a stranger again.
    assert!(cancel_download(&state, &downloads, &canonical).is_err());
}

#[test]
fn a_note_outside_every_root_can_still_be_dismissed() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    // The token is the only thing that authorizes this path, and closing the
    // tab is what gives it back, so the dismissal has to pass the same gate
    // the download did.
    let note = dir.path().join("outside.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    state.authorized_paths.record_for_open(canonical.clone());

    let gated = authorize_download(&state, &canonical).expect("the tab's token authorizes it");
    assert_eq!(gated, canonical);
    assert!(state.authorized_paths.discard_pending_open(&gated));
    assert_eq!(state.authorized_paths.pending_open_len(), 0);

    // And with the tab gone, the path is a stranger again.
    assert!(authorize_download(&state, &canonical).is_err());
}

#[test]
fn the_download_gate_refuses_a_path_nothing_authorized() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "not yours").unwrap();

    let result = authorize_download(&state, &secret.to_string_lossy());
    assert!(
        result.is_err(),
        "an unauthorized path may not be downloaded"
    );
    assert!(result
        .unwrap_err()
        .contains("cannot open that file from here"));
}

#[test]
fn a_note_inside_the_notes_folder_may_be_downloaded() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = state.notes_root().join("mine.md");
    std::fs::write(&note, "bytes").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();

    assert_eq!(
        authorize_download(&state, &canonical).expect("containment authorizes it"),
        canonical
    );
}

#[test]
fn the_not_downloaded_result_names_the_provider_the_note_syncs_through() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    // An .stfolder marker is what a Syncthing folder is known by, and it says
    // so without depending on where this machine's home directory is.
    let synced = dir.path().join("Sync");
    std::fs::create_dir_all(synced.join(".stfolder")).unwrap();
    let note = synced.join("away.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    state.authorized_paths.record_for_open(canonical.clone());
    let _marked = mark(&canonical);

    let result = open_file_from_path(&state, &canonical).expect("open");
    let jotra_core::file_ops::FileOpenMode::NotDownloaded { provider, .. } = &result.mode else {
        panic!("expected the not-downloaded mode, got {:?}", result.mode);
    };
    assert_eq!(provider.as_deref(), Some("Syncthing"));
}

#[test]
fn a_note_in_no_sync_folder_names_no_provider() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = state.notes_root().join("away.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    let _marked = mark(&canonical);

    let result = open_file_from_path(&state, &canonical).expect("open");
    let jotra_core::file_ops::FileOpenMode::NotDownloaded { provider, .. } = &result.mode else {
        panic!("expected the not-downloaded mode, got {:?}", result.mode);
    };
    assert_eq!(*provider, None);
}

/// A file whose stat answers and whose read does not.
///
/// The discriminator for the ordering below: a marked file that reads fine
/// answers `NotDownloaded` whichever side of `classify_path` the gate sits on,
/// so it proves nothing. `classify_path` opens the file to sniff its first
/// bytes (`crates/jotra-core/src/file_ops.rs`), and on a file with no
/// permissions that open fails, so a gate placed after it returns the io error
/// instead of the download state.
#[cfg(unix)]
fn unreadable_note(state: &AppState, name: &str) -> String {
    use std::os::unix::fs::PermissionsExt;

    let note = state.notes_root().join(name);
    std::fs::write(&note, "placeholder stand-in").unwrap();
    std::fs::set_permissions(&note, std::fs::Permissions::from_mode(0o000)).unwrap();
    canonicalize_for_authorization(&note).unwrap()
}

#[cfg(unix)]
#[test]
fn an_open_answers_the_download_state_before_anything_sniffs_the_file() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let canonical = unreadable_note(&state, "unreadable.md");
    let _marked = mark(&canonical);

    let result = open_file_from_path(&state, &canonical)
        .expect("the stat answers the open; nothing reads the file");

    assert!(
        matches!(
            result.mode,
            jotra_core::file_ops::FileOpenMode::NotDownloaded { .. }
        ),
        "expected the not-downloaded mode, got {:?}",
        result.mode
    );
    assert!(result.doc.is_none());
}

#[cfg(unix)]
#[test]
fn a_confirmed_open_answers_the_download_state_before_anything_sniffs_the_file() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let canonical = unreadable_note(&state, "unreadable-confirmed.md");
    let _marked = mark(&canonical);

    // The tier that reaches this entry point is confirmed by hand, and the
    // provider can evict the file while the dialog is up.
    let result = open_file_confirmed_from_path(&state, &canonical)
        .expect("the stat answers the confirmed open too");

    assert!(
        matches!(
            result.mode,
            jotra_core::file_ops::FileOpenMode::NotDownloaded { .. }
        ),
        "expected the not-downloaded mode, got {:?}",
        result.mode
    );
    assert!(result.doc.is_none());
}

#[cfg(unix)]
#[test]
fn a_confirmed_open_of_a_note_that_is_away_keeps_the_grant_for_the_open_that_follows() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    // Outside every root, so the one-shot token is the only authorization and
    // the answer has to hand it back for the retry.
    let note = dir.path().join("outside-confirmed.md");
    std::fs::write(&note, "placeholder stand-in").unwrap();
    let canonical = canonicalize_for_authorization(&note).unwrap();
    state.authorized_paths.record_for_open(canonical.clone());

    let marked = mark(&canonical);
    let away = open_file_confirmed_from_path(&state, &canonical).expect("open");
    assert!(away.doc.is_none());
    assert_eq!(state.authorized_paths.pending_open_len(), 1);
    drop(marked);

    let here = open_file_confirmed_from_path(&state, &canonical)
        .expect("the token the first answer left authorizes this one");
    let doc = here.doc.expect("the note is registered once it is here");
    assert_eq!(doc.source_path.as_deref(), Some(canonical.as_str()));
}
