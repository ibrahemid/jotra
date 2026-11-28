//! The fields of a buffer row that a report about an open tab reads and
//! writes, on a connection of its own.
//!
//! [`crate::buffer_store::BufferStore`] sits behind the app's store lock, and a
//! command can hold that lock through a read of a file on a share that does
//! not answer. A watcher thread waiting there for a tab's encoding or its
//! scripts opt-in holds every later report behind that read. Each call here is
//! one statement on a connection the store does not share (WAL lets both use
//! the file), so it answers what the store would without waiting on its lock.

use std::sync::{LockResult, Mutex, MutexGuard};

use jotra_core::text_encoding::TextEncoding;
use rusqlite::Connection;

use crate::database::queries;
use crate::errors::StorageResult;

/// Recover a poisoned connection guard rather than cascading the panic, as
/// [`crate::layout_state`] does.
fn recover_conn(result: LockResult<MutexGuard<'_, Connection>>) -> MutexGuard<'_, Connection> {
    result.unwrap_or_else(|poisoned| {
        tracing::error!(location = "tab_rows", "recovered poisoned mutex");
        poisoned.into_inner()
    })
}

/// Repository for the buffer-row fields an open tab's watcher needs.
pub struct TabRowStore {
    conn: Mutex<Connection>,
}

impl TabRowStore {
    /// Wrap an open connection to the database the buffer store uses.
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }

    /// The text encoding the buffer's row reads its file in. `None` for a row
    /// that names none and for an id with no row.
    pub fn encoding(&self, id: &str) -> StorageResult<Option<TextEncoding>> {
        let conn = recover_conn(self.conn.lock());
        Ok(queries::find_buffer(&conn, id)?.and_then(|doc| doc.encoding))
    }

    /// Takes the buffer's preview-scripts opt-in away and answers whether it
    /// was on ([`queries::lapse_preview_scripts`]).
    pub fn lapse_preview_scripts(&self, id: &str) -> StorageResult<bool> {
        let conn = recover_conn(self.conn.lock());
        queries::lapse_preview_scripts(&conn, id)
    }
}
