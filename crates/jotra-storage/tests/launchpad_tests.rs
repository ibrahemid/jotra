//! The Launchpad's row for the renamed app (ADR-059 decision 8).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jotra_core::bundle_rename::BUNDLE_IDENTIFIER;
use jotra_core::legacy_names::PRODUCT_NAME;
use jotra_storage::launchpad::{
    read_launchpad_row, retitle_launchpad_app, LaunchpadRetitle, LaunchpadRow, LaunchpadUpdate,
    LAUNCHPAD_BUSY_TIMEOUT,
};
use rusqlite::{params, Connection, TransactionBehavior};
use tempfile::TempDir;

const NEW_TITLE: &str = "Jotra";
const OLD_BOOKMARK: &[u8] = b"old bookmark";
const NEW_BOOKMARK: &[u8] = b"new bookmark";
const OTHER_APP: &str = "com.example.notes";
const EVENT_MARK: i64 = 500;
const NO_EVENT: i64 = -1;

const APPS_TABLE: &str = "CREATE TABLE apps (item_id INTEGER PRIMARY KEY, title VARCHAR, \
     bundleid VARCHAR, storeid VARCHAR, category_id INTEGER, moddate REAL, bookmark BLOB);";
const SOURCES_TABLE: &str = "CREATE TABLE app_sources (rowid INTEGER PRIMARY KEY, uuid VARCHAR, \
     flags INTEGER, bookmark BLOB, last_fsevent_id INTEGER, fsevent_uuid VARCHAR);";

type Row = (i64, String, String, Option<String>, i64, f64, Vec<u8>);

fn retitle() -> LaunchpadRetitle<'static> {
    LaunchpadRetitle {
        bundle_id: BUNDLE_IDENTIFIER,
        from_title: PRODUCT_NAME,
        to_title: NEW_TITLE,
        bookmark: NEW_BOOKMARK,
        event_mark: u64::try_from(EVENT_MARK).expect("mark"),
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

fn insert_apps(database: &Path, rows: &[(i64, &str, &str)]) {
    let connection = Connection::open(database).expect("open");
    for (id, title, bundle_id) in rows {
        connection
            .execute(
                "INSERT INTO apps VALUES (?1, ?2, ?3, NULL, 4, 1.5, ?4)",
                params![id, title, bundle_id, OLD_BOOKMARK],
            )
            .expect("row");
    }
}

fn insert_sources(database: &Path, event_ids: &[i64]) {
    let connection = Connection::open(database).expect("open");
    for (index, event_id) in event_ids.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO app_sources VALUES (?1, ?2, 0, ?3, ?4, ?5)",
                params![
                    i64::try_from(index).expect("index"),
                    format!("source {index}"),
                    OLD_BOOKMARK,
                    event_id,
                    format!("stream {index}")
                ],
            )
            .expect("source");
    }
}

fn launchpad_with_sources(rows: &[(i64, &str, &str)], event_ids: &[i64]) -> (TempDir, PathBuf) {
    let (folder, database) = database_with(&format!("{APPS_TABLE}{SOURCES_TABLE}"));
    insert_apps(&database, rows);
    insert_sources(&database, event_ids);
    (folder, database)
}

fn launchpad(rows: &[(i64, &str, &str)]) -> (TempDir, PathBuf) {
    launchpad_with_sources(rows, &[100, NO_EVENT, 900, NO_EVENT])
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

fn event_ids(database: &Path) -> Vec<i64> {
    let connection = Connection::open(database).expect("open");
    let mut statement = connection
        .prepare("SELECT last_fsevent_id FROM app_sources ORDER BY rowid")
        .expect("select");
    statement
        .query_map([], |row| row.get(0))
        .expect("sources")
        .collect::<Result<_, _>>()
        .expect("sources")
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

fn read_row(database: &Path) -> LaunchpadRow {
    read_launchpad_row(database, BUNDLE_IDENTIFIER, PRODUCT_NAME).expect("read")
}

#[test]
fn the_apps_row_under_the_old_title_gets_the_new_title_and_bookmark() {
    let (_folder, database) = launchpad(&[
        (1, OTHER_APP, OTHER_APP),
        (2, PRODUCT_NAME, BUNDLE_IDENTIFIER),
    ]);
    assert_eq!(
        retitle_launchpad_app(&database, &retitle()).expect("update"),
        LaunchpadUpdate::Changed {
            rows: 1,
            sources: 1
        }
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
fn the_event_mark_raises_only_lower_ids_and_never_touches_minus_one() {
    let (_folder, database) = launchpad_with_sources(
        &[(1, PRODUCT_NAME, BUNDLE_IDENTIFIER)],
        &[0, 100, EVENT_MARK - 1, EVENT_MARK, EVENT_MARK + 1, NO_EVENT],
    );
    assert_eq!(
        retitle_launchpad_app(&database, &retitle()).expect("update"),
        LaunchpadUpdate::Changed {
            rows: 1,
            sources: 3
        }
    );
    assert_eq!(
        event_ids(&database),
        [
            EVENT_MARK,
            EVENT_MARK,
            EVENT_MARK,
            EVENT_MARK,
            EVENT_MARK + 1,
            NO_EVENT
        ]
    );
}

#[test]
fn the_event_mark_is_raised_when_no_row_has_the_old_title() {
    let kept = [
        (1, NEW_TITLE, BUNDLE_IDENTIFIER),
        (2, PRODUCT_NAME, OTHER_APP),
        (3, OTHER_APP, BUNDLE_IDENTIFIER),
    ];
    let (_folder, database) = launchpad(&kept);
    let before = rows(&database);
    assert_eq!(
        retitle_launchpad_app(&database, &retitle()).expect("update"),
        LaunchpadUpdate::Changed {
            rows: 0,
            sources: 1
        }
    );
    assert_eq!(rows(&database), before);
    assert_eq!(event_ids(&database), [EVENT_MARK, NO_EVENT, 900, NO_EVENT]);
}

#[test]
fn the_update_keeps_the_files_journal_mode() {
    let (_folder, database) = launchpad(&[(1, PRODUCT_NAME, BUNDLE_IDENTIFIER)]);
    retitle_launchpad_app(&database, &retitle()).expect("update");
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
        assert_eq!(read_row(&database), LaunchpadRow::NoDatabase);
        assert!(!database.exists());
    }
    assert_eq!(std::fs::read_dir(folder.path()).expect("folder").count(), 0);
}

const SHAPES_WITHOUT_A_TABLE_OR_COLUMN: [&str; 5] = [
    "CREATE TABLE items (rowid INTEGER PRIMARY KEY, title VARCHAR);",
    "CREATE TABLE apps (item_id INTEGER PRIMARY KEY, title VARCHAR, bundleid VARCHAR, \
     bookmark BLOB);",
    "CREATE TABLE apps (item_id INTEGER PRIMARY KEY, title VARCHAR, bundleid VARCHAR, \
     bookmark BLOB); CREATE TABLE app_sources (rowid INTEGER PRIMARY KEY, uuid VARCHAR);",
    "CREATE TABLE apps (item_id INTEGER PRIMARY KEY, title VARCHAR, bundleid VARCHAR); \
     CREATE TABLE app_sources (rowid INTEGER PRIMARY KEY, last_fsevent_id INTEGER);",
    "CREATE TABLE app_sources (rowid INTEGER PRIMARY KEY, last_fsevent_id INTEGER);",
];

#[test]
fn a_file_without_either_table_or_a_column_is_an_unknown_shape() {
    for schema in SHAPES_WITHOUT_A_TABLE_OR_COLUMN {
        let (_folder, database) = database_with(schema);
        assert_eq!(
            retitle_launchpad_app(&database, &retitle()).expect("update"),
            LaunchpadUpdate::UnknownShape,
            "{schema}"
        );
        assert_eq!(read_row(&database), LaunchpadRow::UnknownShape, "{schema}");
    }
}

#[test]
fn a_file_without_the_sources_table_keeps_its_apps_rows() {
    let (_folder, database) = database_with(APPS_TABLE);
    insert_apps(&database, &[(1, PRODUCT_NAME, BUNDLE_IDENTIFIER)]);
    let before = rows(&database);
    assert_eq!(
        retitle_launchpad_app(&database, &retitle()).expect("update"),
        LaunchpadUpdate::UnknownShape
    );
    assert_eq!(rows(&database), before);
}

#[test]
fn a_file_without_the_event_column_keeps_its_apps_rows() {
    let (_folder, database) = database_with(&format!(
        "{APPS_TABLE}CREATE TABLE app_sources (rowid INTEGER PRIMARY KEY, uuid VARCHAR);"
    ));
    insert_apps(&database, &[(1, PRODUCT_NAME, BUNDLE_IDENTIFIER)]);
    let before = rows(&database);
    assert_eq!(
        retitle_launchpad_app(&database, &retitle()).expect("update"),
        LaunchpadUpdate::UnknownShape
    );
    assert_eq!(rows(&database), before);
}

#[test]
fn a_file_another_connection_is_writing_is_busy_after_about_a_second() {
    let (_folder, database) = launchpad(&[(1, PRODUCT_NAME, BUNDLE_IDENTIFIER)]);
    let before = (rows(&database), event_ids(&database));
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
    assert_eq!((rows(&database), event_ids(&database)), before);
}

#[test]
fn a_mark_past_the_integer_range_is_an_error_and_writes_nothing() {
    let (_folder, database) = launchpad(&[(1, PRODUCT_NAME, BUNDLE_IDENTIFIER)]);
    let before = (rows(&database), event_ids(&database));
    let past = LaunchpadRetitle {
        event_mark: u64::MAX,
        ..retitle()
    };
    assert!(retitle_launchpad_app(&database, &past).is_err());
    assert_eq!((rows(&database), event_ids(&database)), before);
}

#[test]
fn a_file_that_is_not_a_database_is_an_error() {
    let folder = tempfile::tempdir().expect("folder");
    let database = folder.path().join("db");
    std::fs::write(&database, b"not a database, only some text long enough").expect("file");
    assert!(retitle_launchpad_app(&database, &retitle()).is_err());
    assert!(read_launchpad_row(&database, BUNDLE_IDENTIFIER, PRODUCT_NAME).is_err());
}

#[test]
fn the_read_finds_the_row_under_the_old_title_and_writes_nothing() {
    let (_folder, database) = launchpad(&[
        (1, OTHER_APP, OTHER_APP),
        (2, PRODUCT_NAME, BUNDLE_IDENTIFIER),
    ]);
    let before = (rows(&database), event_ids(&database));
    assert_eq!(read_row(&database), LaunchpadRow::Stale);
    assert!(read_row(&database).is_stale());
    assert_eq!((rows(&database), event_ids(&database)), before);

    retitle_launchpad_app(&database, &retitle()).expect("update");
    assert_eq!(read_row(&database), LaunchpadRow::UpToDate);
}

#[test]
fn the_read_ignores_rows_of_another_app_or_title() {
    let (_folder, database) = launchpad(&[
        (1, NEW_TITLE, BUNDLE_IDENTIFIER),
        (2, PRODUCT_NAME, OTHER_APP),
        (3, OTHER_APP, BUNDLE_IDENTIFIER),
    ]);
    assert_eq!(read_row(&database), LaunchpadRow::UpToDate);
    assert!(!read_row(&database).is_stale());
}

#[test]
fn each_result_reads_plainly_in_the_log() {
    assert_eq!(
        LaunchpadUpdate::Changed {
            rows: 1,
            sources: 3
        }
        .to_string(),
        "rows changed: 1, folder marks raised: 3"
    );
    assert_eq!(LaunchpadUpdate::NoDatabase.to_string(), "no database");
    assert_eq!(LaunchpadUpdate::UnknownShape.to_string(), "unknown shape");
    assert_eq!(LaunchpadUpdate::Busy.to_string(), "busy");
    assert_eq!(LaunchpadRow::Stale.to_string(), "old title");
    assert_eq!(LaunchpadRow::UpToDate.to_string(), "no old title");
    assert_eq!(LaunchpadRow::NoDatabase.to_string(), "no database");
    assert_eq!(LaunchpadRow::UnknownShape.to_string(), "unknown shape");
    assert_eq!(LaunchpadRow::Busy.to_string(), "busy");
}
