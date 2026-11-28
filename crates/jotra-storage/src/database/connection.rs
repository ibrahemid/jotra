use crate::errors::StorageResult;
use rusqlite::{Connection, InterruptHandle, OpenFlags};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Upper bound, in bytes, that the write-ahead log is truncated to on
/// checkpoint.
const JOURNAL_SIZE_LIMIT_BYTES: i64 = 64 * 1024 * 1024;

/// How long a write the database refused waits before it is tried again.
const BUSY_RETRY_INTERVAL: Duration = Duration::from_millis(1);

/// How long a refused write keeps being tried again before the refusal
/// reaches the caller: the five seconds SQLite's own timeout allowed.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Work running through [`yielding_to_writes`] in this process.
static YIELDING: Mutex<Vec<YieldingWork>> = Mutex::new(Vec::new());

/// The id the next [`yielding_to_writes`] call registers its work under.
static NEXT_YIELDING_ID: AtomicU64 = AtomicU64::new(0);

/// Work on a connection of its own that a refused write on an
/// [`open_database`] connection to the same file cuts short.
struct YieldingWork {
    id: u64,
    database: PathBuf,
    interrupt: InterruptHandle,
}

thread_local! {
    /// When the write this thread is trying again was first refused. SQLite
    /// calls the busy handler on the thread making the write, counting the
    /// tries from zero for each write.
    static REFUSED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Opens a SQLite database at `path` using Jotra's pragma defaults.
///
/// The returned connection has:
///
/// - `journal_mode = WAL` for concurrent read/write performance,
/// - `synchronous = NORMAL` for durability within a transaction while
///   avoiding the fsync cost of `FULL` on every commit,
/// - `foreign_keys = ON` so referential constraints are enforced,
/// - `journal_size_limit = 64 MiB` so a checkpoint truncates the
///   write-ahead log back to that cap instead of leaving it at its
///   high-water mark,
/// - a write refused because another connection holds the write lock tried
///   again every millisecond, for up to five seconds, cutting short any work
///   running on the file through [`yielding_to_writes`].
pub fn open_database(path: &Path) -> StorageResult<Connection> {
    let mut conn = Connection::open(path)?;
    let database = database_file(&conn);
    conn.busy_handler(Some(move |attempts| {
        retry_every_millisecond(attempts, database.as_deref())
    }))?;
    conn.pragma_update(None, "journal_mode", "wal")?;
    conn.pragma_update(None, "synchronous", "normal")?;
    conn.pragma_update(None, "foreign_keys", "on")?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
    Ok(conn)
}

/// The file `conn` has open, as SQLite names it; `None` for a database in
/// memory.
fn database_file(conn: &Connection) -> Option<PathBuf> {
    conn.path()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// Waits before a write on `database` that was refused is tried again,
/// `attempts` refusals in; `false` hands the refusal to the caller.
///
/// Each try first cuts short the work running on `database` through
/// [`yielding_to_writes`], so a write never waits it out.
///
/// SQLite's own handler waits longer after every refusal, up to 100 ms between
/// tries. The notes walk takes the write lock once per file, a millisecond or
/// so at a time, on a connection of its own, so a write on the app's connection
/// that waits 25 or 100 ms between tries keeps waking up in the middle of a
/// later file: Cmd+N's row waited 295 ms while a walk went on through 293 more
/// entries. Trying every millisecond finds the moment between two files.
fn retry_every_millisecond(attempts: i32, database: Option<&Path>) -> bool {
    #[cfg(test)]
    tests::retried_on_this_thread();
    if let Some(database) = database {
        cut_short_work_on(database);
    }
    let now = Instant::now();
    let Ok(refused_at) = REFUSED_AT.try_with(|refused_at| match refused_at.get() {
        Some(at) if attempts > 0 => at,
        _ => {
            refused_at.set(Some(now));
            now
        }
    }) else {
        return false;
    };
    if now.duration_since(refused_at) >= BUSY_TIMEOUT {
        return false;
    }
    std::thread::sleep(BUSY_RETRY_INTERVAL);
    true
}

/// Interrupts the statement each piece of [`yielding_to_writes`] work on
/// `database` is running, which rolls its transaction back.
fn cut_short_work_on(database: &Path) {
    let yielding = YIELDING.lock().unwrap_or_else(PoisonError::into_inner);
    for work in yielding.iter().filter(|work| work.database == database) {
        work.interrupt.interrupt();
    }
}

/// Runs `f`, the work of `conn`, giving way to the app's writes: a write on an
/// [`open_database`] connection to the same file that `f` holds up interrupts
/// the statement `f` is running, rather than waiting for it to finish. For
/// work that holds the database's write lock for as long as it takes, the
/// startup `VACUUM`; an interrupted statement fails with
/// `SQLITE_INTERRUPT` and its transaction rolls back.
///
/// `conn` must not be one [`open_database`] opened, or a write it meets would
/// cut short its own work.
pub fn yielding_to_writes<R>(conn: &Connection, f: impl FnOnce() -> R) -> R {
    struct Registered(u64);
    impl Drop for Registered {
        fn drop(&mut self) {
            YIELDING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|work| work.id != self.0);
        }
    }
    let Some(database) = database_file(conn) else {
        return f();
    };
    let id = NEXT_YIELDING_ID.fetch_add(1, Ordering::SeqCst);
    YIELDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(YieldingWork {
            id,
            database,
            interrupt: conn.get_interrupt_handle(),
        });
    let _registered = Registered(id);
    f()
}

/// Opens the database at `path` for work that holds its write lock for as
/// long as it takes, beside the app's own connections.
///
/// A write this connection meets held up is tried again for five seconds and
/// no longer, and cuts nothing short. The file's pragmas were set by
/// [`open_database`]; `journal_mode` stays with the file.
pub fn open_database_for_maintenance(path: &Path) -> StorageResult<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

/// Opens the database at `path` for reading only.
///
/// For a second process that must not change what the app owns: the `jotra`
/// command reads the note index this way. No pragma is set, because every one
/// of the defaults above is a write — `journal_mode` in particular — and a
/// read-only connection refuses them. An absent file is an error here rather
/// than a fresh empty database, which is what [`open_database`] would create.
pub fn open_database_read_only(path: &Path) -> StorageResult<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError};
    use std::thread::ThreadId;
    use std::time::{Duration, Instant};

    use super::{
        open_database, open_database_for_maintenance, retry_every_millisecond, yielding_to_writes,
        BUSY_RETRY_INTERVAL, BUSY_TIMEOUT, REFUSED_AT,
    };

    /// A write that holds the database's write lock for seconds, as a `VACUUM`
    /// of a large file does, until it is cut short.
    const LONG_WRITE: &str = "INSERT INTO held SELECT count(*) FROM (
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 30000000)
         SELECT i FROM n)";

    /// The threads [`retry_every_millisecond`] has run on in this test binary.
    static RETRIED_ON: Mutex<Vec<ThreadId>> = Mutex::new(Vec::new());

    pub(super) fn retried_on_this_thread() {
        let id = std::thread::current().id();
        let mut threads = RETRIED_ON.lock().unwrap_or_else(PoisonError::into_inner);
        if !threads.contains(&id) {
            threads.push(id);
        }
    }

    fn retried_on(id: ThreadId) -> bool {
        RETRIED_ON
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id)
    }

    /// Each try waits a millisecond first, and the tries stop five seconds
    /// after the write was first refused, however long each wait really took.
    #[test]
    fn a_refused_write_is_tried_every_millisecond_for_five_seconds() {
        let start = Instant::now();
        assert!(retry_every_millisecond(0, None));
        assert!(start.elapsed() >= BUSY_RETRY_INTERVAL);
        assert!(retry_every_millisecond(1, None));

        let long_ago = Instant::now()
            .checked_sub(BUSY_TIMEOUT)
            .expect("the clock reaches five seconds back");
        REFUSED_AT.with(|refused_at| refused_at.set(Some(long_ago)));
        assert!(
            !retry_every_millisecond(2, None),
            "five seconds after the first refusal"
        );
        assert!(
            retry_every_millisecond(0, None),
            "a new write's first refusal starts its own five seconds"
        );
    }

    /// Waits until a connection other than the app's holds the write lock of
    /// the database at `path`.
    fn wait_until_the_write_lock_is_held(path: &std::path::Path) {
        let probe = open_database_for_maintenance(path).expect("probe");
        probe.busy_timeout(Duration::ZERO).expect("no busy wait");
        let deadline = Instant::now() + Duration::from_secs(10);
        while probe.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_ok() {
            assert!(
                Instant::now() < deadline,
                "the long write never took the lock"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Starts [`LONG_WRITE`] on a connection of its own to `path`, giving way
    /// to the app's writes, once it holds the write lock.
    fn start_long_write(path: &std::path::Path) -> std::thread::JoinHandle<rusqlite::Result<()>> {
        let side = open_database_for_maintenance(path).expect("side connection");
        side.execute_batch("CREATE TABLE IF NOT EXISTS held (n INTEGER)")
            .expect("table");
        let work = std::thread::spawn(move || {
            yielding_to_writes(&side, || side.execute_batch(LONG_WRITE))
        });
        wait_until_the_write_lock_is_held(path);
        work
    }

    /// A write held up by work that gives way to writes interrupts it and
    /// lands at once, rather than waiting it out.
    #[test]
    fn a_held_up_write_cuts_short_the_work_that_gives_way_to_it() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("jotra.db");
        let app = open_database(&path).expect("app connection");
        app.execute_batch("CREATE TABLE notes (id INTEGER)")
            .expect("table");
        let work = start_long_write(&path);

        let started = Instant::now();
        app.execute("INSERT INTO notes VALUES (1)", [])
            .expect("the write lands");
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(2),
            "the write waited {} ms for the work",
            waited.as_millis()
        );
        let cut_short = work
            .join()
            .expect("work thread")
            .expect_err("the work was cut short");
        assert_eq!(
            cut_short.sqlite_error_code(),
            Some(rusqlite::ErrorCode::OperationInterrupted)
        );
        let held: i64 = app
            .query_row("SELECT COUNT(*) FROM held", [], |row| row.get(0))
            .expect("count");
        assert_eq!(held, 0, "the cut-short write rolled back");
    }

    /// A write held up on one file leaves alone the work that gives way to
    /// writes on another.
    #[test]
    fn a_held_up_write_leaves_work_on_another_file_alone() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let other = dir.path().join("other.db");
        let path = dir.path().join("jotra.db");
        let app = open_database(&path).expect("app connection");
        let other_app = open_database(&other).expect("app connection to the other file");
        let work = start_long_write(&other);

        let holder = open_database(&path).expect("holder");
        holder
            .execute_batch(
                "CREATE TABLE notes (id INTEGER); BEGIN IMMEDIATE; INSERT INTO notes VALUES (0);",
            )
            .expect("hold the write lock");
        let writer = std::thread::spawn(move || app.execute("INSERT INTO notes VALUES (1)", []));
        let writer_thread = writer.thread().id();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !retried_on(writer_thread) {
            assert!(Instant::now() < deadline, "the write was never held up");
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(50));
        holder.execute_batch("COMMIT").expect("let the lock go");
        writer
            .join()
            .expect("writer thread")
            .expect("the write lands");
        assert!(
            !work.is_finished(),
            "the work on the other file was cut short"
        );

        other_app
            .execute("INSERT INTO held VALUES (1)", [])
            .expect("a write on the other file cuts its work short");
        assert!(work.join().expect("work thread").is_err());
    }

    /// A write on one connection while another holds the write lock is tried
    /// again through Jotra's handler, and lands once the lock is let go.
    #[test]
    fn a_write_another_connection_holds_up_is_retried_until_it_lands() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("jotra.db");
        let holder = open_database(&path).expect("holder");
        holder
            .execute_batch(
                "CREATE TABLE notes (id INTEGER); BEGIN IMMEDIATE; INSERT INTO notes VALUES (0);",
            )
            .expect("hold the write lock");
        let writer = std::thread::spawn({
            let path = path.clone();
            move || {
                open_database(&path)
                    .expect("writer")
                    .execute("INSERT INTO notes VALUES (1)", [])
            }
        });
        let writer_thread = writer.thread().id();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !retried_on(writer_thread) {
            assert!(
                Instant::now() < deadline,
                "the refused write was never tried again through Jotra's handler"
            );
            std::thread::yield_now();
        }
        holder.execute_batch("COMMIT").expect("let the lock go");

        writer
            .join()
            .expect("writer thread")
            .expect("the write lands once the lock is let go");
        let rows: i64 = holder
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 2);
    }
}
