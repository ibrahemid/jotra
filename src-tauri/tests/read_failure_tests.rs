//! A read that fails reaches the editor as a stable code, never as an empty
//! note.
//!
//! The editor decides from the code alone whether it may mount the file: a
//! rejection it cannot tell apart from "the file is empty" is how a file that
//! is not UTF-8 opened as a blank page whose first keystroke replaced it.
//! These go through the real open and read commands, because the failure is
//! the composition of the two: the open records the file and succeeds, and
//! only the read finds the bytes it cannot hand over.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::path::Path;

use jotra_core::file_ops::FileOpenMode;
use jotra_core::notes::line_ending::LineEnding;
use jotra_tauri_lib::commands::buffer::{
    close_buffer_inner, read_buffer_content_inner, reopen_with_encoding_inner,
    save_buffer_content_inner,
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

fn read_error(state: &AppState, id: &str) -> String {
    match read_buffer_content_inner(state, id) {
        Ok(bytes) => panic!(
            "the read handed over {} bytes instead of failing",
            bytes.len()
        ),
        Err(message) => message,
    }
}

#[test]
fn a_file_that_is_no_longer_utf8_fails_its_read_with_a_code() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("prices.csv");
    std::fs::write(&file, "cafe,3.50\n").unwrap();
    let id = open(&state, &file);

    // Another program saves it as Windows-1252, where 0xE9 is "é" and on its
    // own is not UTF-8. The tab is still open and reads it on the next switch
    // or launch.
    let cp1252: &[u8] = b"caf\xe9,3.50\r\nth\xe9,2.00\r\n";
    std::fs::write(&file, cp1252).unwrap();
    let error = read_error(&state, &id);

    assert!(
        error.starts_with("ERR_READ_NOT_UTF8:"),
        "the editor cannot tell this read from an empty file: {error}"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        cp1252,
        "a failed read must leave the file as it was"
    );
}

/// The line ending the note's row holds.
fn line_ending_of(state: &AppState, id: &str) -> LineEnding {
    state.store.lock().unwrap().get(id).unwrap().line_ending
}

#[test]
fn a_file_that_is_not_utf8_opens_in_its_own_encoding_and_reads() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("prices-1252.csv");
    let cp1252: &[u8] = b"caf\xe9,3.50\r\nth\xe9,2.00\r\n";
    std::fs::write(&file, cp1252).unwrap();

    // The open detects the encoding (ADR-049), so the tab's read hands over
    // the text rather than failing on bytes that are not UTF-8.
    let canonical = canonicalize_for_authorization(&file).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    let opened = open_file_from_path(&state, &canonical).expect("a file that is not UTF-8 opens");
    assert!(
        matches!(opened.mode, FileOpenMode::Normal),
        "a text file that is not UTF-8 is not a binary or large file"
    );
    let doc = opened.doc.expect("the open carries its note");
    assert_eq!(
        doc.encoding.map(|encoding| encoding.label()).as_deref(),
        Some("windows-1252")
    );
    let bytes = read_buffer_content_inner(&state, &doc.id).expect("the file reads");

    assert_eq!(bytes, "café,3.50\r\nthé,2.00\r\n".as_bytes());
    assert_eq!(
        std::fs::read(&file).unwrap(),
        cp1252,
        "opening and reading must leave the file as it was"
    );
}

#[test]
fn a_closed_file_reopened_after_it_stopped_being_utf8_reads_in_its_new_encoding() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("menu.txt");
    std::fs::write(&file, "café, in UTF-8\n").unwrap();
    let id = open(&state, &file);
    close_buffer_inner(&state, &id).expect("close");

    let cp1252: &[u8] = b"caf\xe9, in Windows-1252\r\n";
    std::fs::write(&file, cp1252).unwrap();
    let reopened = open(&state, &file);
    let bytes = read_buffer_content_inner(&state, &reopened).expect("the reopened file reads");

    assert_eq!(reopened, id, "the closed note comes back, not a second one");
    assert_eq!(bytes, "café, in Windows-1252\r\n".as_bytes());
    assert_eq!(line_ending_of(&state, &id), LineEnding::CrLf);
    assert_eq!(
        std::fs::read(&file).unwrap(),
        cp1252,
        "reopening and reading must leave the file as it was"
    );
}

#[test]
fn a_file_converted_to_another_encoding_while_open_reads_once_reopened_in_it() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("prices.csv");
    std::fs::write(&file, "cafe,3.50\r\n").unwrap();
    let id = open(&state, &file);

    // Another program saves it as Windows-1252 while the tab holds it as
    // UTF-8: the read fails with its code, and the note is reopened in the
    // encoding the file is in now.
    std::fs::write(&file, b"caf\xe9,3.50\r\nth\xe9,2.00\r\n").unwrap();
    assert!(read_error(&state, &id).starts_with("ERR_READ_NOT_UTF8:"));
    reopen_with_encoding_inner(&state, &id, "windows-1252").expect("reopens");
    let bytes = read_buffer_content_inner(&state, &id).expect("the file reads in its encoding");
    assert_eq!(bytes, "café,3.50\r\nthé,2.00\r\n".as_bytes());

    save_buffer_content_inner(&state, &id, "café,3.75\nthé,2.00\n")
        .expect("the reopened file saves");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"caf\xe9,3.75\r\nth\xe9,2.00\r\n",
        "written back in its own encoding and ending"
    );
}

#[test]
fn a_file_that_went_away_fails_its_read_with_a_code() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("gone.txt");
    std::fs::write(&file, "here when it was opened\n").unwrap();

    let id = open(&state, &file);
    std::fs::remove_file(&file).unwrap();
    let error = read_error(&state, &id);

    assert!(
        error.starts_with("ERR_READ_FILE_MISSING:"),
        "a missing file read as an uncoded failure: {error}"
    );
}

#[cfg(unix)]
#[test]
fn a_file_jotra_may_not_read_fails_its_read_with_a_code() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("locked.txt");
    std::fs::write(&file, "somebody else's\n").unwrap();

    let id = open(&state, &file);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A process that may read anything (root in a container) reads it anyway,
    // and there is no refusal to check.
    if std::fs::read(&file).is_ok() {
        return;
    }
    let error = read_error(&state, &id);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    assert!(
        error.starts_with("ERR_READ_PERMISSION_DENIED:"),
        "a refused read reached the editor without its code: {error}"
    );
}

#[test]
fn a_utf8_file_still_reads() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("plain.txt");
    std::fs::write(&file, "café, in UTF-8\n").unwrap();

    let id = open(&state, &file);
    let bytes = read_buffer_content_inner(&state, &id).expect("a UTF-8 file reads");

    assert_eq!(bytes, "café, in UTF-8\n".as_bytes());
}

#[test]
fn a_binary_file_still_opens_as_its_hex_view() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("blob.bin");
    std::fs::write(&file, b"\x00\x01\x02\xff\xfe binary").unwrap();

    let id = open(&state, &file);
    let bytes = read_buffer_content_inner(&state, &id).expect("a binary file reads read-only");

    assert!(!bytes.is_empty(), "the hex view came back empty");
}
