//! The Launchpad's row for the renamed app (ADR-059 decision 8).
//!
//! Through macOS 15 the Dock keeps Launchpad in a SQLite file of its own,
//! written while it runs. Its schema is private, so the statements here are
//! also the check of the file's shape: a file they do not all prepare against
//! is left alone.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, ErrorCode, OpenFlags, TransactionBehavior};

use crate::errors::StorageResult;

/// How long a call waits for another connection to let go of the file.
pub const LAUNCHPAD_BUSY_TIMEOUT: Duration = Duration::from_secs(1);

const RETITLE_APP: &str =
    "UPDATE apps SET title = ?1, bookmark = ?2 WHERE bundleid = ?3 AND title = ?4";

const RAISE_EVENT_MARK: &str = "UPDATE app_sources SET last_fsevent_id = ?1 \
     WHERE last_fsevent_id >= 0 AND last_fsevent_id < ?1";

const HAS_TITLE: &str = "SELECT EXISTS (SELECT 1 FROM apps WHERE bundleid = ?1 AND title = ?2)";

/// The Launchpad row to change, what it becomes, and the file-system event
/// the Dock's application folders resume from.
#[derive(Debug, Clone, Copy)]
pub struct LaunchpadRetitle<'a> {
    /// The app's bundle identifier.
    pub bundle_id: &'a str,
    /// The title the row holds now; a row under any other title is kept.
    pub from_title: &'a str,
    /// The title the row gets.
    pub to_title: &'a str,
    /// The bookmark the row gets.
    pub bookmark: &'a [u8],
    /// The event id each application folder's source resumes from. A source
    /// at a higher id, or at -1, keeps its own.
    pub event_mark: u64,
}

/// What [`retitle_launchpad_app`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchpadUpdate {
    /// Both statements ran and committed.
    Changed {
        /// The `apps` rows that got the new title.
        rows: usize,
        /// The `app_sources` rows whose event id was raised.
        sources: usize,
    },
    /// No file is at the path.
    NoDatabase,
    /// The file lacks a table or column the statements name.
    UnknownShape,
    /// Another connection held the file past [`LAUNCHPAD_BUSY_TIMEOUT`].
    Busy,
}

impl fmt::Display for LaunchpadUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed { rows, sources } => {
                write!(f, "rows changed: {rows}, folder marks raised: {sources}")
            }
            Self::NoDatabase => f.write_str("no database"),
            Self::UnknownShape => f.write_str("unknown shape"),
            Self::Busy => f.write_str("busy"),
        }
    }
}

/// What [`read_launchpad_row`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchpadRow {
    /// A row of the bundle identifier carries the old title.
    Stale,
    /// No row of the bundle identifier carries the old title.
    UpToDate,
    /// No file is at the path.
    NoDatabase,
    /// The file lacks a table or column the statements name.
    UnknownShape,
    /// Another connection held the file past [`LAUNCHPAD_BUSY_TIMEOUT`].
    Busy,
}

impl LaunchpadRow {
    /// Whether the row still carries the old title.
    pub fn is_stale(self) -> bool {
        self == Self::Stale
    }
}

impl fmt::Display for LaunchpadRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stale => "old title",
            Self::UpToDate => "no old title",
            Self::NoDatabase => "no database",
            Self::UnknownShape => "unknown shape",
            Self::Busy => "busy",
        })
    }
}

/// In one immediate transaction, gives the row of `retitle.bundle_id` under
/// `retitle.from_title` the new title and bookmark, then raises every
/// application folder source's event id below `retitle.event_mark` to it.
/// Opens the file read-write without creating it and sets no pragma.
///
/// Every statement here is prepared before either update runs, so a file
/// without both tables and their columns gets no write. A missing
/// file, such a file, and a file another connection holds past
/// [`LAUNCHPAD_BUSY_TIMEOUT`] are results, not errors. A mark past SQLite's
/// integer range and any other SQLite or file system failure are errors.
pub fn retitle_launchpad_app(
    database: &Path,
    retitle: &LaunchpadRetitle<'_>,
) -> StorageResult<LaunchpadUpdate> {
    let event_mark = i64::try_from(retitle.event_mark)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let Some(mut connection) = open_existing(
        database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?
    else {
        return Ok(LaunchpadUpdate::NoDatabase);
    };
    let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(transaction) => transaction,
        Err(error) => return busy_or_error(error, LaunchpadUpdate::Busy),
    };
    match has_known_shape(&transaction) {
        Ok(true) => {}
        Ok(false) => return Ok(LaunchpadUpdate::UnknownShape),
        Err(error) => return busy_or_error(error, LaunchpadUpdate::Busy),
    }
    let changed = transaction
        .prepare_cached(RETITLE_APP)
        .and_then(|mut statement| {
            statement.execute(params![
                retitle.to_title,
                retitle.bookmark,
                retitle.bundle_id,
                retitle.from_title
            ])
        })
        .and_then(|rows| {
            let sources = transaction
                .prepare_cached(RAISE_EVENT_MARK)?
                .execute(params![event_mark])?;
            Ok(LaunchpadUpdate::Changed { rows, sources })
        });
    let update = match changed {
        Ok(update) => update,
        Err(error) => return busy_or_error(error, LaunchpadUpdate::Busy),
    };
    match transaction.commit() {
        Ok(()) => Ok(update),
        Err(error) => busy_or_error(error, LaunchpadUpdate::Busy),
    }
}

/// Whether a row of `bundle_id` in the Launchpad file at `database` carries
/// `old_title`. Opens the file read-only without creating it, after the same
/// check of its shape as [`retitle_launchpad_app`].
pub fn read_launchpad_row(
    database: &Path,
    bundle_id: &str,
    old_title: &str,
) -> StorageResult<LaunchpadRow> {
    let Some(connection) = open_existing(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?
    else {
        return Ok(LaunchpadRow::NoDatabase);
    };
    match has_known_shape(&connection) {
        Ok(true) => {}
        Ok(false) => return Ok(LaunchpadRow::UnknownShape),
        Err(error) => return busy_or_error(error, LaunchpadRow::Busy),
    }
    match connection.query_row(HAS_TITLE, params![bundle_id, old_title], |row| {
        row.get::<_, bool>(0)
    }) {
        Ok(true) => Ok(LaunchpadRow::Stale),
        Ok(false) => Ok(LaunchpadRow::UpToDate),
        Err(error) => busy_or_error(error, LaunchpadRow::Busy),
    }
}

/// The file at `database` opened with `flags`, or `None` when no file is
/// there. Never creates one.
fn open_existing(database: &Path, flags: OpenFlags) -> StorageResult<Option<Connection>> {
    if !database.try_exists()? {
        return Ok(None);
    }
    let connection = match Connection::open_with_flags(database, flags) {
        Ok(connection) => connection,
        Err(error)
            if error.sqlite_error_code() == Some(ErrorCode::CannotOpen)
                && !database.try_exists()? =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    connection.busy_timeout(LAUNCHPAD_BUSY_TIMEOUT)?;
    Ok(Some(connection))
}

/// Whether every statement prepares: `false` when a table or column it names
/// is missing.
fn has_known_shape(connection: &Connection) -> rusqlite::Result<bool> {
    for statement in [RETITLE_APP, RAISE_EVENT_MARK, HAS_TITLE] {
        match connection.prepare_cached(statement) {
            Ok(_) => {}
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::Unknown) => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

fn busy_or_error<T>(error: rusqlite::Error, busy: T) -> StorageResult<T> {
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Ok(busy),
        _ => Err(error.into()),
    }
}
