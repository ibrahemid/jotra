use std::path::Path;

use jotra_core::maintenance::needs_vacuum;
use rusqlite::Connection;
use tracing::{info, warn};

use crate::database::connection::{open_database_for_maintenance, yielding_to_writes};
use crate::errors::StorageResult;

/// Page accounting for a database file, read from SQLite's pragmas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseStats {
    /// Total pages in the main database file.
    pub page_count: u64,
    /// Pages on the freelist, holding no live data.
    pub freelist_count: u64,
    /// Size of one page in bytes.
    pub page_size: u64,
}

impl DatabaseStats {
    /// File size in bytes implied by the page count.
    pub fn file_bytes(&self) -> u64 {
        self.page_count * self.page_size
    }

    /// Bytes held by freelist pages.
    pub fn free_bytes(&self) -> u64 {
        self.freelist_count * self.page_size
    }
}

/// What a maintenance pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceOutcome {
    /// Page accounting before the pass.
    pub before: DatabaseStats,
    /// Page accounting after the pass.
    pub after: DatabaseStats,
    /// Whether a `VACUUM` ran.
    pub vacuumed: bool,
}

/// Reads page counts and page size from `conn`.
pub fn read_stats(conn: &Connection) -> StorageResult<DatabaseStats> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let freelist_count: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    Ok(DatabaseStats {
        page_count: page_count.max(0) as u64,
        freelist_count: freelist_count.max(0) as u64,
        page_size: page_size.max(0) as u64,
    })
}

/// Folds the write-ahead log back into the database and truncates it to zero.
///
/// Returns `false` when SQLite reported the checkpoint as busy, which leaves
/// the log in place until the next attempt.
pub fn checkpoint_truncate(conn: &Connection) -> StorageResult<bool> {
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
    Ok(busy == 0)
}

/// Reclaims free pages when they dominate the file, then truncates the log.
///
/// `VACUUM` rewrites the database, so it needs free disk space of roughly the
/// live data size and exclusive access. A failure is logged and reported, not
/// propagated as fatal: an unvacuumed database is still fully usable.
pub fn run_maintenance(conn: &Connection) -> StorageResult<MaintenanceOutcome> {
    if let Err(e) = checkpoint_truncate(conn) {
        warn!(error = %e, "wal checkpoint before maintenance failed");
    }

    let before = read_stats(conn)?;
    let mut vacuumed = false;

    if needs_vacuum(before.page_count, before.freelist_count) {
        info!(
            file_bytes = before.file_bytes(),
            free_bytes = before.free_bytes(),
            "database is mostly free pages; vacuuming"
        );
        #[cfg(feature = "test-hooks")]
        if let Some(hook) = BEFORE_VACUUM.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        match conn.execute_batch("VACUUM") {
            Ok(()) => vacuumed = true,
            Err(e) => warn!(error = %e, "vacuum failed; leaving the database as is"),
        }
        if let Err(e) = checkpoint_truncate(conn) {
            warn!(error = %e, "wal checkpoint after vacuum failed");
        }
    }

    let after = read_stats(conn)?;
    if vacuumed {
        info!(
            before_bytes = before.file_bytes(),
            after_bytes = after.file_bytes(),
            "database vacuumed"
        );
    }

    Ok(MaintenanceOutcome {
        before,
        after,
        vacuumed,
    })
}

/// [`run_maintenance`] on a connection of its own to the database at `path`,
/// so no lock around the app's connection is held while it runs.
///
/// A write on an app connection that meets the `VACUUM` cuts it short rather
/// than waiting for it ([`yielding_to_writes`]): the `VACUUM` rolls back and
/// the database stays as it was until the next pass.
pub fn run_maintenance_at(path: &Path) -> StorageResult<MaintenanceOutcome> {
    let conn = open_database_for_maintenance(path)?;
    yielding_to_writes(&conn, || run_maintenance(&conn))
}

#[cfg(feature = "test-hooks")]
thread_local! {
    static BEFORE_VACUUM: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `hook` on this thread at the next `VACUUM` it reaches, before the
/// statement starts, so a test can hold the maintenance there.
#[cfg(feature = "test-hooks")]
pub fn before_next_vacuum(hook: impl FnOnce() + 'static) {
    BEFORE_VACUUM.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
