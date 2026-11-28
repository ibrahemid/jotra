//! The buffer-row fields an open tab's watcher reads and writes, on a
//! connection of their own, answer what the buffer store holds.

use chrono::Utc;
use jotra_core::buffer::document::{BufferDocument, BufferStatus};
use jotra_core::notes::line_ending::LineEnding;
use jotra_core::text_encoding::TextEncoding;
use jotra_storage::buffer_store::BufferStore;
use jotra_storage::database::connection::open_database;
use jotra_storage::database::migrations::run_migrations;
use jotra_storage::database::queries::{get_preview_scripts_opt_in, PreviewScriptsOptIn};
use jotra_storage::tab_rows::TabRowStore;
use tempfile::TempDir;

const ID: &str = "tab-1";

fn setup() -> (TempDir, BufferStore, TabRowStore) {
    let dir = TempDir::new().expect("temp dir");
    let db_path = dir.path().join("test.db");
    let conn = open_database(&db_path).expect("open db");
    run_migrations(&conn).expect("migrations");
    let buffers_dir = dir.path().join("buffers");
    std::fs::create_dir_all(&buffers_dir).expect("buffers dir");
    let store = BufferStore::new(conn, buffers_dir);
    let rows = TabRowStore::new(open_database(&db_path).expect("second connection"));
    (dir, store, rows)
}

fn open_tab(store: &BufferStore, dir: &TempDir, encoding: Option<TextEncoding>) {
    let path = dir.path().join("prices.csv");
    std::fs::write(&path, b"a,b\n").expect("write");
    let now = Utc::now();
    let doc = BufferDocument {
        id: ID.to_string(),
        title: "prices".to_string(),
        filename: format!("{ID}.txt"),
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
        size_bytes: 4,
        line_ending: LineEnding::Lf,
        encoding,
    };
    store.open_from_path(&doc, "").expect("open");
}

#[test]
fn the_encoding_is_the_one_the_store_last_recorded() {
    let (dir, store, rows) = setup();
    open_tab(&store, &dir, Some(TextEncoding::windows_1252()));
    assert_eq!(
        rows.encoding(ID).expect("read"),
        Some(TextEncoding::windows_1252())
    );

    let utf16 = TextEncoding::from_label("UTF-16LE BOM").expect("known");
    store.set_encoding(ID, Some(utf16)).expect("record");
    assert_eq!(rows.encoding(ID).expect("read"), Some(utf16));

    store.set_encoding(ID, None).expect("record");
    assert_eq!(rows.encoding(ID).expect("read"), None);
}

#[test]
fn an_id_with_no_row_has_no_encoding() {
    let (_dir, _store, rows) = setup();
    assert_eq!(rows.encoding("no-such-tab").expect("read"), None);
}

#[test]
fn a_lapse_answers_whether_the_scripts_were_on_and_turns_them_off() {
    let (dir, store, rows) = setup();
    open_tab(&store, &dir, None);
    store
        .enable_preview_scripts(ID, Some(jotra_core::hash::sha256_bytes(b"a,b\n")))
        .expect("opt in");

    assert!(rows.lapse_preview_scripts(ID).expect("lapse"));
    assert!(!store.preview_scripts(ID).expect("read the row"));
    assert!(!rows.lapse_preview_scripts(ID).expect("lapse again"));
    assert!(!rows.lapse_preview_scripts("no-such-tab").expect("lapse"));
}

#[test]
fn a_lapse_forgets_the_digest_the_opt_in_covered() {
    let (dir, store, rows) = setup();
    open_tab(&store, &dir, None);
    store
        .enable_preview_scripts(ID, Some(jotra_core::hash::sha256_bytes(b"a,b\n")))
        .expect("opt in");
    assert!(rows.lapse_preview_scripts(ID).expect("lapse"));

    let conn = open_database(&dir.path().join("test.db")).expect("third connection");
    assert_eq!(
        get_preview_scripts_opt_in(&conn, ID).expect("read"),
        Some(PreviewScriptsOptIn {
            enabled: false,
            recorded_hash: None,
        })
    );
}
