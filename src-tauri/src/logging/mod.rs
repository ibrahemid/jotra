pub mod panic_handler;

use jotra_core::log_line::redact_paths;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use tracing::Metadata;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

/// Daily log files kept in the logs folder; older ones are deleted when a new
/// one opens (ADR-045).
const MAX_LOG_FILES: usize = 14;

/// What a release build logs. Debug lines name notes and folders, so they
/// stay behind `RUST_LOG`.
const RELEASE_DIRECTIVES: &str = "info";

/// What a debug build logs.
const DEBUG_DIRECTIVES: &str = "info,jotra_core=debug,jotra_storage=debug,jotra_tauri_lib=debug";

/// The log file writer, held for the life of the process so an exit outside
/// the run loop can write the queued lines too ([`flush_logs`]).
static LOG_WRITER: LogWriter = LogWriter::empty();

/// Holds the log file writer open. Dropping it, or calling
/// [`LogWriter::flush`], writes the lines still queued.
struct LogWriter(Mutex<Option<WorkerGuard>>);

impl LogWriter {
    const fn empty() -> Self {
        Self(Mutex::new(None))
    }

    fn hold(&self, guard: Option<WorkerGuard>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = guard;
    }

    fn flush(&self) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

/// What [`init_logging`] hands back; [`LogGuard::flush`] writes the lines
/// still queued.
///
/// The event loop ends the process with `exit`, which runs no destructors, so
/// the run loop calls `flush` on its way out. The Windows installer ends it
/// from the updater's hook instead, which calls [`flush_logs`].
pub struct LogGuard(());

impl LogGuard {
    /// Writes the queued lines and closes the file writer. Lines logged after
    /// this are dropped.
    pub fn flush(&mut self) {
        flush_logs();
    }
}

/// Writes the queued log lines and closes the file writer, from anywhere in
/// the process. Lines logged after this are dropped.
pub fn flush_logs() {
    LOG_WRITER.flush();
}

/// A log writer whose lines name files, never their full paths
/// ([`redact_paths`]). Error values carry paths in their own text, so the
/// cut is made here rather than at each call site.
struct FileNamesOnly<M>(M);

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for FileNamesOnly<M> {
    type Writer = FileNamesOnlyWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        FileNamesOnlyWriter(self.0.make_writer())
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        FileNamesOnlyWriter(self.0.make_writer_for(meta))
    }
}

/// Takes one formatted log line per write, as the fmt layer hands them over.
struct FileNamesOnlyWriter<W>(W);

impl<W: Write> Write for FileNamesOnlyWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let line = String::from_utf8_lossy(buf);
        self.0.write_all(redact_paths(&line).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

pub fn init_logging(logs_dir: &Path) -> LogGuard {
    let directives = filter_directives(
        std::env::var("RUST_LOG").ok().as_deref(),
        !cfg!(debug_assertions),
    );
    let env_filter = EnvFilter::try_new(&directives)
        .unwrap_or_else(|_| EnvFilter::new(default_directives(!cfg!(debug_assertions))));

    let (file_layer, guard) = match file_layer(logs_dir) {
        Some((layer, guard)) => (Some(layer), Some(guard)),
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .with_writer(FileNamesOnly(std::io::stdout))
                .with_target(true),
        )
        .with(file_layer)
        .init();

    LOG_WRITER.hold(guard);
    LogGuard(())
}

/// The filter a build starts with when `RUST_LOG` is unset.
fn default_directives(release: bool) -> &'static str {
    if release {
        RELEASE_DIRECTIVES
    } else {
        DEBUG_DIRECTIVES
    }
}

/// The filter to install: `RUST_LOG` when it is set and not blank, otherwise
/// the build's default.
fn filter_directives(env: Option<&str>, release: bool) -> String {
    match env.map(str::trim) {
        Some(directives) if !directives.is_empty() => directives.to_string(),
        _ => default_directives(release).to_string(),
    }
}

/// Builds the rolling-file layer and its guard, or `None` when the logs
/// directory cannot take a log file.
///
/// The convenience constructor (`rolling::daily`) panics in that case, which
/// aborted the process before Jotra could report why — and the likeliest
/// reason for it is the same unwritable `~/.jotra` that makes startup fail.
fn file_layer<S>(logs_dir: &Path) -> Option<(impl Layer<S>, WorkerGuard)>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let appender = build_appender(logs_dir)?;
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    Some((
        fmt::layer()
            .with_writer(FileNamesOnly(non_blocking))
            .with_ansi(false),
        guard,
    ))
}

/// Opens `jotra.log.<date>` in `logs_dir`, deleting the oldest files past
/// [`MAX_LOG_FILES`]. Returns `None` on any failure.
fn build_appender(logs_dir: &Path) -> Option<RollingFileAppender> {
    std::fs::create_dir_all(logs_dir).ok()?;
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("jotra.log")
        .max_log_files(MAX_LOG_FILES)
        .build(logs_dir)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tracing::level_filters::LevelFilter;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn a_warn_line_names_the_file_an_error_value_carries_the_path_of() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .with_writer(FileNamesOnly(captured.clone()))
                .with_ansi(false),
        );
        let error = jotra_storage::errors::StorageError::DestinationReadOnly {
            path: "/Users/someone/Notes/Plans/launch.md".to_string(),
        };

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(%error, "the save was refused");
        });

        let line = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(line.contains("error=launch.md cannot be written"));
        assert!(!line.contains("/Users/someone"));
        assert!(!line.contains("Plans"));
    }

    #[test]
    fn appender_opens_a_writable_logs_dir_that_does_not_exist_yet() {
        let parent = tempfile::tempdir().unwrap();
        let logs = parent.path().join("logs");

        assert!(build_appender(&logs).is_some());
        assert!(logs.is_dir());
    }

    #[test]
    #[cfg(unix)]
    fn appender_returns_none_instead_of_panicking_on_an_unwritable_logs_dir() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let logs = parent.path().join("logs");
        std::fs::create_dir(&logs).unwrap();
        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o555)).unwrap();

        assert!(build_appender(&logs).is_none());

        std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_release_build_logs_at_info_and_nothing_finer() {
        let filter = EnvFilter::new(filter_directives(None, true));

        assert_eq!(filter.max_level_hint(), Some(LevelFilter::INFO));
    }

    #[test]
    fn a_debug_build_logs_writs_own_crates_at_debug() {
        let filter = EnvFilter::new(filter_directives(None, false));

        assert_eq!(filter.max_level_hint(), Some(LevelFilter::DEBUG));
    }

    #[test]
    fn rust_log_turns_debug_on_in_a_release_build() {
        assert_eq!(
            filter_directives(Some("jotra_tauri_lib=debug"), true),
            "jotra_tauri_lib=debug"
        );
        assert_eq!(filter_directives(Some("  "), true), RELEASE_DIRECTIVES);
    }

    #[test]
    fn the_logs_folder_keeps_the_newest_fourteen_log_files() {
        let logs = tempfile::tempdir().unwrap();
        // Pruning orders by creation time; the pause keeps the order strict.
        for day in 1..=20 {
            std::fs::write(
                logs.path().join(format!("jotra.log.2000-01-{day:02}")),
                "old\n",
            )
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::fs::write(
            logs.path().join("jotra-crash-20000101-000000.log"),
            "report\n",
        )
        .unwrap();

        let _appender = build_appender(logs.path()).expect("appender");

        let log_files = std::fs::read_dir(logs.path())
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("jotra.log"))
            .count();
        assert_eq!(log_files, MAX_LOG_FILES);
        assert!(!logs.path().join("jotra.log.2000-01-01").exists());
        assert!(logs.path().join("jotra.log.2000-01-20").exists());
        assert!(logs.path().join("jotra-crash-20000101-000000.log").exists());
    }

    #[test]
    fn flushing_the_guard_writes_the_queued_lines() {
        let logs = tempfile::tempdir().unwrap();
        let (mut writer, guard) =
            tracing_appender::non_blocking(build_appender(logs.path()).unwrap());
        let held = LogWriter::empty();
        held.hold(Some(guard));

        writer.write_all(b"the last line before quit\n").unwrap();
        held.flush();

        let written: String = std::fs::read_dir(logs.path())
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        assert!(written.contains("the last line before quit"));
    }

    #[test]
    fn the_windows_installer_exit_writes_the_queued_log_lines() {
        const UPDATE_RS: &str = include_str!("../commands/update.rs");
        let hook = UPDATE_RS
            .split("on_before_exit(")
            .nth(1)
            .expect("the updater's exit hook");
        let hook = &hook[..hook.find("})").expect("the hook's end")];

        assert!(hook.contains("crate::logging::flush_logs();"));
        assert!(
            hook.find("flush_logs").unwrap() > hook.find("cleanup_before_exit").unwrap(),
            "the lines the shutdown logs are written too"
        );
    }
}
