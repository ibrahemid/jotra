//! A file in another text encoding opens, reads, saves and reopens in it
//! (ADR-049), through the real commands.
//!
//! Every save here is guarded by the record the open or the read left, so a
//! record of the editor's text in place of the file's bytes fails the save.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::path::Path;
use std::sync::Arc;

use jotra_core::file_ops::FileOpenMode;
use jotra_core::hash::comparison_digest_hex;
use jotra_core::notes::line_ending::LineEnding;
use jotra_core::text_encoding::TextEncoding;
use jotra_tauri_lib::commands::buffer::{
    close_buffer_inner, note_disk_state_inner, read_buffer_content_inner,
    reopen_with_encoding_inner, save_buffer_as_utf8_inner, save_buffer_content_inner,
    NoteDiskAnswer,
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

const CP1252_CSV: &[u8] = b"name,note\r\nCaf\xe9 Nord,cr\xe8me br\xfbl\xe9e\r\n";
const CP1252_TEXT: &str = "name,note\r\nCafé Nord,crème brûlée\r\n";

fn utf16le_with_bom(text: &str) -> Vec<u8> {
    b"\xff\xfe"
        .iter()
        .copied()
        .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
        .collect()
}

fn encoding_of(state: &AppState, id: &str) -> Option<String> {
    state
        .store
        .lock()
        .expect("store")
        .get(id)
        .expect("get")
        .encoding
        .map(TextEncoding::label)
}

fn read(state: &AppState, id: &str) -> String {
    String::from_utf8(read_buffer_content_inner(state, id).expect("reads")).expect("UTF-8 text")
}

#[test]
fn a_windows_1252_csv_opens_reads_and_saves_in_windows_1252() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("menu.csv");
    std::fs::write(&file, CP1252_CSV).unwrap();

    let canonical = canonicalize_for_authorization(&file).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    let opened = open_file_from_path(&state, &canonical).expect("opens");
    assert_eq!(opened.mode, FileOpenMode::Normal);
    let doc = opened.doc.expect("a row");
    assert_eq!(
        serde_json::to_value(&doc).expect("the open's row as JSON")["encoding"],
        "windows-1252",
        "the open answer carries the encoding beside the line ending"
    );
    assert_eq!(doc.line_ending, LineEnding::CrLf);

    assert_eq!(read(&state, &doc.id), CP1252_TEXT);
    save_buffer_content_inner(&state, &doc.id, "name,note\nCafé Süd,crème brûlée\n")
        .expect("the save lands under the guard");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"name,note\r\nCaf\xe9 S\xfcd,cr\xe8me br\xfbl\xe9e\r\n"
    );
    save_buffer_content_inner(&state, &doc.id, "name,note\nCafé Ost,crème brûlée\n")
        .expect("and so does the next one");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"name,note\r\nCaf\xe9 Ost,cr\xe8me br\xfbl\xe9e\r\n"
    );
}

#[test]
fn the_digest_the_tab_is_handed_matches_the_text_it_holds() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("menu.csv");
    std::fs::write(&file, CP1252_CSV).unwrap();
    let id = open(&state, &file);

    let NoteDiskAnswer::Described { disk } = note_disk_state_inner(&state, &id).expect("asks")
    else {
        panic!("the file was not described");
    };
    assert_eq!(
        disk.hash,
        comparison_digest_hex(LineEnding::normalise(CP1252_TEXT).as_bytes()),
        "a tab holding the file's text would read as edited"
    );
}

#[test]
fn a_powershell_utf16_file_opens_as_text_and_saves_as_utf16() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("out.txt");
    let listing = "\r\n    Directory: C:\\Users\\ibra\r\n\r\nMode   Name\r\nd----  Documents\r\n";
    std::fs::write(&file, utf16le_with_bom(listing)).unwrap();

    let canonical = canonicalize_for_authorization(&file).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    let opened = open_file_from_path(&state, &canonical).expect("opens");
    assert_eq!(opened.mode, FileOpenMode::Normal, "not the hex view");
    let doc = opened.doc.expect("a row");
    assert!(!doc.read_only);
    assert_eq!(
        encoding_of(&state, &doc.id).as_deref(),
        Some("UTF-16LE BOM")
    );
    assert_eq!(doc.line_ending, LineEnding::CrLf);
    assert_eq!(read(&state, &doc.id), listing);

    let edited = format!("{}-a---  notes.txt\n", LineEnding::normalise(listing));
    save_buffer_content_inner(&state, &doc.id, &edited).expect("saves");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        utf16le_with_bom(&format!("{listing}-a---  notes.txt\r\n"))
    );
}

#[test]
fn a_lossy_save_is_refused_with_the_characters_named() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("menu.csv");
    std::fs::write(&file, CP1252_CSV).unwrap();
    let id = open(&state, &file);
    read(&state, &id);

    let error = save_buffer_content_inner(&state, &id, "name,note\nCafé 😀,中\n")
        .expect_err("Windows-1252 cannot hold these");
    assert!(
        error.starts_with("ERR_ENCODING_LOSSY: windows-1252 has no bytes for U+1F600 U+4E2D"),
        "the refusal is not in the shape the editor reads: {}",
        error
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        CP1252_CSV,
        "nothing was written"
    );
}

#[test]
fn a_save_its_encoding_refuses_lands_as_utf8_and_the_note_stays_utf8() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("menu.csv");
    std::fs::write(&file, CP1252_CSV).unwrap();
    let id = open(&state, &file);
    read(&state, &id);
    let mine = "name,note\nCafé 😀,中\n";
    save_buffer_content_inner(&state, &id, mine).expect_err("Windows-1252 cannot hold these");

    let digest = save_buffer_as_utf8_inner(&state, &id, mine).expect("UTF-8 holds them");

    assert_eq!(digest, Some(comparison_digest_hex(mine.as_bytes())));
    assert_eq!(
        std::fs::read(&file).unwrap(),
        "name,note\r\nCafé 😀,中\r\n".as_bytes(),
        "the file keeps its line endings"
    );
    assert_eq!(encoding_of(&state, &id).as_deref(), Some("UTF-8"));
    assert_eq!(read(&state, &id), "name,note\r\nCafé 😀,中\r\n");
    save_buffer_content_inner(&state, &id, "name,note\nCafé 😀,中,¥\n")
        .expect("the next save writes UTF-8 too");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        "name,note\r\nCafé 😀,中,¥\r\n".as_bytes()
    );
}

#[test]
fn a_save_as_utf8_the_guard_stops_leaves_the_encoding_as_it_was() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("menu.csv");
    std::fs::write(&file, CP1252_CSV).unwrap();
    let id = open(&state, &file);
    read(&state, &id);
    std::fs::write(
        &file,
        b"name,note\r\nCaf\xe9 Nord,cr\xe8me br\xfbl\xe9e et plus\r\n",
    )
    .unwrap();

    let error = save_buffer_as_utf8_inner(&state, &id, "name,note\nCafé 😀\n")
        .expect_err("the file changed outside Jotra");

    assert!(
        error.starts_with("ERR_FILE_CHANGED_ON_DISK"),
        "the refusal is the guard's: {}",
        error
    );
    assert_eq!(
        encoding_of(&state, &id).as_deref(),
        Some("windows-1252"),
        "the file is still read the way it is written"
    );
}

#[test]
fn reopening_with_another_encoding_reads_the_file_in_it() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("menu.csv");
    std::fs::write(&file, CP1252_CSV).unwrap();
    let id = open(&state, &file);
    assert_eq!(encoding_of(&state, &id).as_deref(), Some("windows-1252"));

    let reopened = reopen_with_encoding_inner(&state, &id, "windows-1250").expect("reopens");
    assert_eq!(
        reopened.encoding.map(TextEncoding::label).as_deref(),
        Some("windows-1250")
    );
    assert_eq!(
        read(&state, &id),
        "name,note\r\nCaf\u{e9} Nord,cr\u{10d}me br\u{171}l\u{e9}e\r\n"
    );

    let refused = reopen_with_encoding_inner(&state, &id, "UTF-8").expect_err("not UTF-8");
    assert!(refused.starts_with("ERR_READ_NOT_UTF8:"), "{refused}");
    assert_eq!(
        encoding_of(&state, &id).as_deref(),
        Some("windows-1250"),
        "a refused reopen keeps the encoding the note had"
    );
    let unknown = reopen_with_encoding_inner(&state, &id, "klingon").expect_err("unknown");
    assert!(unknown.starts_with("ERR_UNKNOWN_ENCODING:"), "{unknown}");

    reopen_with_encoding_inner(&state, &id, "windows-1252").expect("back");
    assert_eq!(read(&state, &id), CP1252_TEXT);
    assert_eq!(
        std::fs::read(&file).unwrap(),
        CP1252_CSV,
        "a reopen writes nothing"
    );
}

#[test]
fn the_version_kept_before_a_save_is_the_files_own_bytes() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let root = state.notes_root();
    state.note_history.set_notes_root(root.clone());
    state.note_history.set_probe(Arc::new(
        jotra_tauri_lib::watcher::identity::PlatformIdentity,
    ));
    {
        let mut store = state.store.lock().expect("store");
        store.set_notes_root(root.clone());
        store.set_history(Arc::clone(&state.note_history));
    }
    let file = root.join("Menu.txt");
    std::fs::write(&file, CP1252_CSV).unwrap();
    let id = open(&state, &file);
    read(&state, &id);
    save_buffer_content_inner(&state, &id, "name,note\nCafé Süd,crème brûlée\n").expect("saves");

    let key = state
        .note_history
        .key_for(&file)
        .expect("a note in the notes folder");
    let kept: Vec<Vec<u8>> = state
        .note_history
        .versions(&key)
        .expect("versions")
        .into_iter()
        .map(|entry| state.note_history.content(entry.id).expect("content"))
        .collect();
    assert!(
        kept.iter().any(|bytes| bytes.as_slice() == CP1252_CSV),
        "the version from before the save is the file as it was, byte for byte"
    );
}

#[test]
fn a_utf16_file_kept_as_a_hex_view_opens_as_text_when_it_is_opened_again() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("out.txt");
    std::fs::write(&file, utf16le_with_bom("dir\r\n")).unwrap();
    let id = open(&state, &file);
    // The row as a build before UTF-16 was text left it: a hex view.
    {
        let store = state.store.lock().expect("store");
        store.set_read_only(&id, true).expect("record");
        store.set_encoding(&id, None).expect("record");
    }
    close_buffer_inner(&state, &id).expect("close");

    assert_eq!(open(&state, &file), id, "the closed tab comes back");
    let row = state.store.lock().expect("store").get(&id).expect("get");
    assert!(!row.read_only);
    assert_eq!(
        row.encoding.map(TextEncoding::label).as_deref(),
        Some("UTF-16LE BOM")
    );
    assert_eq!(read(&state, &id), "dir\r\n");
}

#[test]
fn reopening_a_file_that_is_gone_says_so() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("menu.csv");
    std::fs::write(&file, CP1252_CSV).unwrap();
    let id = open(&state, &file);
    std::fs::remove_file(&file).unwrap();
    let refused = reopen_with_encoding_inner(&state, &id, "UTF-8").expect_err("no file");
    assert!(refused.starts_with("ERR_READ_FILE_MISSING:"), "{refused}");
}
