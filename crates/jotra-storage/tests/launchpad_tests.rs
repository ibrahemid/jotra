//! The Launchpad's row for the renamed app (ADR-059 decision 8).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jotra_core::bundle_rename::BUNDLE_IDENTIFIER;
use jotra_core::legacy_names::PRODUCT_NAME;
use jotra_storage::launchpad::{
    retitle_launchpad_app, LaunchpadRetitle, LaunchpadUpdate, LAUNCHPAD_BUSY_TIMEOUT,
};
use rusqlite::{params, Connection, TransactionBehavior};
use tempfile::TempDir;

const NEW_TITLE: &str = "Jotra";
const OLD_BOOKMARK: &[u8] = b"old bookmark";
const NEW_BOOKMARK: &[u8] = b"new bookmark";
const OTHER_APP: &str = "com.example.notes";

const APPS_TABLE: &str = "CREATE TABLE apps (item_id INTEGER PRIMARY KEY, title VARCHAR, \
     bundleid VARCHAR, storeid VARCHAR, category_id INTEGER, moddate REAL, bookmark BLOB)";

type Row = (i64, String, String, Option<String>, i64, f64, Vec<u8>);

fn retitle() -> LaunchpadRetitle<'static> {
    LaunchpadRetitle {
        bundle_id: BUNDLE_IDENTIFIER,
        from_title: PRODUCT_NAME,
        to_title: NEW_TITLE,
        bookmark: NEW_BOOKMARK,
    }
}

fn database_with(schema: &str) -> (TempDir, PathBuf) {
    let folder = tempfile::tempdir().expect("folder");
    let database = folder.path().join("db");
    let connection = Connection::open(&database).expect("open");
    let mode: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .expect("wal");
    assert_eq!(mode, "wal");
    connection.execute_batch(schema).expect("schema");
    (folder, database)
}

fn launchpad(rows: &[(i64, &str, &str)]) -> (TempDir, PathBuf) {
    let (folder, database) = database_with(APPS_TABLE);
    let connection = Connection::open(&database).expect("open");
    for (id, title, bundle_id) in rows {
        connection
            .execute(
                "INSERT INTO apps VALUES (?1, ?2, ?3, NULL, 4, 1.5, ?4)",
                params![id, title, bundle_id, OLD_BOOKMARK],
            )
            .expect("row");
    }
    (folder, database)
}

fn rows(database: &Path) -> Vec<Row> {
    let connection = Connection::open(database).expect("open");
    let mut statement = connection
        .prepare(
            "SELECT item_id, title, bundleid, storeid, category_id, moddate, bookmark \
             FROM apps ORDER BY item_id",
        )
        .expect("select");
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn row(id: i64, title: &str, bundle_id: &str, bookmark: &[u8]) -> Row {
    (
        id,
        title.to_string(),
        bundle_id.to_string(),
        None,
        4,
        1.5,
        bookmark.to_vec(),
    )
}

#[test]
fn the_apps_row_under_the_old_title_gets_the_new_title_and_bookmark() {
    let (_folder, database) = launchpad(&[
        (1, OTHER_APP, OTHER_APP),
        (2, PRODUCT_NAME, BUNDLE_IDENTIFIER),
    ]);
    assert_eq!(
        retitle_launchpad_app(&database, &retitle()).expect("update"),
        LaunchpadUpdate::Changed(1)
    );
    assert_eq!(
        rows(&database),
        [
            row(1, OTHER_APP, OTHER_APP, OLD_BOOKMARK),
            row(2, NEW_TITLE, BUNDLE_IDENTIFIER, NEW_BOOKMARK),
        ]
    );
}

#[test]
fn rows_of_another_app_or_title_are_kept() {
    let kept = [
        (1, NEW_TITLE, BUNDLE_IDENTIFIER),
        (2, PRODUCT_NAME, OTHER_APP),
        (3, OTHER_APP, BUNDLE_IDENTIFIER),
    ];
    let (_folder, database) = launchpad(&kept);
    let before = rows(&database);
    let update = retitle_launchpad_app(&database, &retitle()).expect("update");
    assert_eq!(update, LaunchpadUpdate::Changed(0));
    assert!(!update.changed_anything());
    assert_eq!(rows(&database), before);
}

#[test]
fn the_update_keeps_the_files_journal_mode() {
    let (_folder, database) = launchpad(&[(1, PRODUCT_NAME, BUNDLE_IDENTIFIER)]);
    let update = retitle_launchpad_app(&database, &retitle()).expect("update");
    assert!(update.changed_anything());
    let connection = Connection::open(&database).expect("open");
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("mode");
    assert_eq!(mode, "wal");
}

#[test]
fn a_missing_file_is_not_created() {
    let folder = tempfile::tempdir().expect("folder");
    for database in [
        folder.path().join("db"),
        folder.path().join("missing").join("db"),
    ] {
        assert_eq!(
            retitle_launchpad_app(&database, &retitle()).expect("update"),
            LaunchpadUpdate::NoDatabase
        );
        assert!(!database.exists());
    }
    assert_eq!(std::fs::read_dir(folder.path()).expect("folder").count(), 0);
}

#[test]
fn a_file_without_the_apps_table_or_a_column_is_an_unknown_shape() {
    for schema in [
        "CREATE TABLE items (rowid INTEGER PRIMARY KEY, title VARCHAR)",
        "CREATE TABLE apps (item_id INTEGER PRIMARY KEY, title VARCHAR, bundleid VARCHAR)",
        "CREATE TABLE apps (item_id INTEGER PRIMARY KEY, name VARCHAR, bundleid VARCHAR, \
         bookmark BLOB)",
    ] {
        let (_folder, database) = database_with(schema);
        assert_eq!(
            retitle_launchpad_app(&database, &retitle()).expect("update"),
            LaunchpadUpdate::UnknownShape,
            "{schema}"
        );
    }
}

#[test]
fn a_file_another_connection_is_writing_is_busy_after_about_a_second() {
    let (_folder, database) = launchpad(&[(1, PRODUCT_NAME, BUNDLE_IDENTIFIER)]);
    let before = rows(&database);
    let mut holder = Connection::open(&database).expect("open");
    let held = holder
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("hold");

    let started = Instant::now();
    let update = retitle_launchpad_app(&database, &retitle()).expect("update");
    let waited = started.elapsed();

    assert_eq!(update, LaunchpadUpdate::Busy);
    assert!(waited >= LAUNCHPAD_BUSY_TIMEOUT - Duration::from_millis(100));
    assert!(waited < LAUNCHPAD_BUSY_TIMEOUT * 3);
    held.rollback().expect("release");
    assert_eq!(rows(&database), before);
}

#[test]
fn a_file_that_is_not_a_database_is_an_error() {
    let folder = tempfile::tempdir().expect("folder");
    let database = folder.path().join("db");
    std::fs::write(&database, b"not a database, only some text long enough").expect("file");
    assert!(retitle_launchpad_app(&database, &retitle()).is_err());
}

#[test]
fn each_result_reads_plainly_in_the_log() {
    assert_eq!(LaunchpadUpdate::Changed(1).to_string(), "rows changed: 1");
    assert_eq!(LaunchpadUpdate::NoDatabase.to_string(), "no database");
    assert_eq!(LaunchpadUpdate::UnknownShape.to_string(), "unknown shape");
    assert_eq!(LaunchpadUpdate::Busy.to_string(), "busy");
    assert!(LaunchpadUpdate::Changed(2).changed_anything());
    assert!(!LaunchpadUpdate::Busy.changed_anything());
}
