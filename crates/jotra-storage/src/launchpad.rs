//! The Launchpad's row for the renamed app (ADR-059 decision 8).
//!
//! Through macOS 15 the Dock keeps Launchpad in a SQLite file of its own,
//! written while it runs. Its schema is private, so the one statement here is
//! also the check of the file's shape: a file it does not prepare against is
//! left alone.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, ErrorCode, OpenFlags, TransactionBehavior};

use crate::errors::StorageResult;

/// How long the update waits for another connection to let go of the file.
pub const LAUNCHPAD_BUSY_TIMEOUT: Duration = Duration::from_secs(1);

const RETITLE_APP: &str =
    "UPDATE apps SET title = ?1, bookmark = ?2 WHERE bundleid = ?3 AND title = ?4";

/// The Launchpad row to change and what it becomes.
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
}

/// What [`retitle_launchpad_app`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchpadUpdate {
    /// The statement ran and changed this many rows.
    Changed(usize),
    /// No file is at the path.
    NoDatabase,
    /// The file has no `apps` table with the columns the statement names.
    UnknownShape,
    /// Another connection held the file past [`LAUNCHPAD_BUSY_TIMEOUT`].
    Busy,
}

impl LaunchpadUpdate {
    /// Whether the file changed.
    pub fn changed_anything(self) -> bool {
        matches!(self, Self::Changed(rows) if rows > 0)
    }
}

impl fmt::Display for LaunchpadUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed(rows) => write!(f, "rows changed: {rows}"),
            Self::NoDatabase => f.write_str("no database"),
            Self::UnknownShape => f.write_str("unknown shape"),
            Self::Busy => f.write_str("busy"),
        }
    }
}

/// Gives the row of `retitle.bundle_id` under `retitle.from_title` in the
/// Launchpad file at `database` the new title and bookmark, in one immediate
/// transaction. Opens the file read-write without creating it and sets no
/// pragma.
///
/// A missing file, a file the statement does not prepare against, and a file
/// another connection holds past [`LAUNCHPAD_BUSY_TIMEOUT`] are results, not
/// errors. Any other SQLite or file system failure is an error.
pub fn retitle_launchpad_app(
    database: &Path,
    retitle: &LaunchpadRetitle<'_>,
) -> StorageResult<LaunchpadUpdate> {
    if !database.try_exists()? {
        return Ok(LaunchpadUpdate::NoDatabase);
    }
    let mut connection = match Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(connection) => connection,
        Err(error)
            if error.sqlite_error_code() == Some(ErrorCode::CannotOpen)
                && !database.try_exists()? =>
        {
            return Ok(LaunchpadUpdate::NoDatabase);
        }
        Err(error) => return Err(error.into()),
    };
    connection.busy_timeout(LAUNCHPAD_BUSY_TIMEOUT)?;
    let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(transaction) => transaction,
        Err(error) => return busy_or_error(error),
    };
    let changed = {
        let mut statement = match transaction.prepare(RETITLE_APP) {
            Ok(statement) => statement,
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::Unknown) => {
                return Ok(LaunchpadUpdate::UnknownShape);
            }
            Err(error) => return busy_or_error(error),
        };
        match statement.execute(params![
            retitle.to_title,
            retitle.bookmark,
            retitle.bundle_id,
            retitle.from_title
        ]) {
            Ok(changed) => changed,
            Err(error) => return busy_or_error(error),
        }
    };
    match transaction.commit() {
        Ok(()) => Ok(LaunchpadUpdate::Changed(changed)),
        Err(error) => busy_or_error(error),
    }
}

fn busy_or_error(error: rusqlite::Error) -> StorageResult<LaunchpadUpdate> {
    match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Ok(LaunchpadUpdate::Busy),
        _ => Err(error.into()),
    }
}
