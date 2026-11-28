//! How `new`, `append` and `today` treat stdin, driven through the built
//! `jotra` binary: read without limit after `-`, within
//! [`PIPE_FIRST_BYTE_WAIT`] for its first byte otherwise, and never written
//! from before the read has ended.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use jotra_cli::PIPE_FIRST_BYTE_WAIT;
use tempfile::TempDir;

/// Long enough that a test waiting on it has failed, short of hanging the suite.
const TEST_LIMIT: Duration = Duration::from_secs(60);

/// What stderr says when a `-` read has waited [`PIPE_FIRST_BYTE_WAIT`].
const WAITING: &str = "jotra: waiting for stdin to close";

/// More than any pipe buffers, so a write of it returns only once the reader
/// has taken most of it.
const PAST_THE_PIPE_BUFFER: usize = 4 * 1024 * 1024;

struct Fixture {
    _dir: TempDir,
    data: PathBuf,
    notes: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let data = dir.path().join("data");
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(&data).expect("data");
        std::fs::create_dir_all(&notes).expect("notes");
        std::fs::write(notes.join("Two.md"), "Back to [[One]].\n").expect("two");
        Self {
            _dir: dir,
            data,
            notes,
        }
    }

    fn spawn(&self, args: &[&str], stdin: Stdio) -> Child {
        Command::new(env!("CARGO_BIN_EXE_jotra"))
            .args(args)
            .env_remove("JOTRA_GUI_BIN")
            .env("JOTRA_DATA_DIR", &self.data)
            .env("JOTRA_NOTES_DIR", &self.notes)
            .current_dir(&self.notes)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run jotra")
    }

    fn two(&self) -> String {
        read(&self.notes.join("Two.md"))
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.notes)
            .expect("list notes")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read")
}

/// A running `jotra` whose stdin the test holds, whose stderr a thread reads
/// line by line, and whose end a thread waits on.
struct Running {
    pid: u32,
    started: Instant,
    stdin: Option<ChildStdin>,
    stderr: mpsc::Receiver<String>,
    ended: mpsc::Receiver<(ExitStatus, Instant)>,
}

impl Running {
    fn start(fixture: &Fixture, args: &[&str]) -> Self {
        let started = Instant::now();
        let mut child = fixture.spawn(args, Stdio::piped());
        let stdin = child.stdin.take().expect("stdin");
        let stderr = child.stderr.take().expect("stderr");
        let (line, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for text in BufReader::new(stderr).lines() {
                let Ok(text) = text else { break };
                if line.send(text).is_err() {
                    break;
                }
            }
        });
        let pid = child.id();
        let (end, ended) = mpsc::channel();
        std::thread::spawn(move || {
            let status = child.wait().expect("wait for jotra");
            let _ = end.send((status, Instant::now()));
        });
        Self {
            pid,
            started,
            stdin: Some(stdin),
            stderr: lines,
            ended,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.stdin
            .as_mut()
            .expect("stdin is open")
            .write_all(bytes)
            .expect("write stdin");
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Waits for `line` on stderr, the sign the command is inside its wait.
    fn wait_for_line(&self, line: &str) {
        let deadline = Instant::now() + TEST_LIMIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let text = self
                .stderr
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("stderr never said {line}"));
            if text == line {
                return;
            }
        }
    }

    /// Waits for the command to end, and says how long it ran.
    fn wait(&self) -> (ExitStatus, Duration) {
        let (status, at) = self
            .ended
            .recv_timeout(TEST_LIMIT)
            .expect("jotra never ended");
        (status, at.duration_since(self.started))
    }

    fn stderr_lines(&self) -> Vec<String> {
        self.stderr.try_iter().collect()
    }

    fn interrupt(&self) {
        let pid = self.pid.to_string();
        #[cfg(unix)]
        let sent = Command::new("kill").args(["-INT", &pid]).status();
        #[cfg(not(unix))]
        let sent = Command::new("taskkill").args(["/F", "/PID", &pid]).status();
        assert!(sent.expect("run kill").success(), "could not stop {pid}");
    }
}

fn assert_interrupted(status: ExitStatus) {
    assert!(!status.success(), "{status}");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(2), "{status}");
    }
}

#[test]
fn a_verb_that_takes_no_piped_text_never_reads_stdin() {
    let fixture = Fixture::new();
    let running = Running::start(&fixture, &["today", "--no-open"]);

    let (status, _) = running.wait();

    assert!(status.success(), "{status}: {:?}", running.stderr_lines());
}

#[test]
fn with_a_dash_a_writer_slower_than_the_wait_is_read_whole() {
    let fixture = Fixture::new();
    let before = fixture.two();
    let mut runs = [
        Running::start(&fixture, &["new", "Slow", "-"]),
        Running::start(&fixture, &["append", "Two.md", "-"]),
        Running::start(&fixture, &["today", "--append", "--no-open", "-"]),
    ];

    for running in &runs {
        running.wait_for_line(WAITING);
    }
    assert_eq!(fixture.names(), ["Two.md"]);
    assert_eq!(fixture.two(), before);
    for running in &mut runs {
        running.write(b"late text\n");
        running.close_stdin();
    }

    for running in &runs {
        let (status, _) = running.wait();
        assert!(status.success(), "{status}: {:?}", running.stderr_lines());
    }
    assert_eq!(read(&fixture.notes.join("Slow.txt")), "late text\n");
    assert_eq!(fixture.two(), format!("{before}late text\n"));
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert_eq!(
        read(&fixture.notes.join(format!("{today}.txt"))),
        "late text\n"
    );
}

#[test]
fn without_a_dash_a_pipe_silent_past_the_wait_writes_nothing_and_exits_1() {
    let fixture = Fixture::new();
    let before = fixture.two();
    let mut runs = [
        Running::start(&fixture, &["new", "Slow"]),
        Running::start(&fixture, &["append", "Two.md"]),
        Running::start(&fixture, &["today", "--append", "--no-open"]),
    ];

    for running in &runs {
        let (status, ran) = running.wait();
        assert_eq!(status.code(), Some(1), "{status}");
        assert!(
            ran >= PIPE_FIRST_BYTE_WAIT && ran < 2 * PIPE_FIRST_BYTE_WAIT,
            "ran {ran:?}"
        );
        let lines = running.stderr_lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("pass - to wait"), "{lines:?}");
        assert!(lines[0].contains("close stdin"), "{lines:?}");
    }
    for running in &mut runs {
        let _ = running
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(b"late text\n");
    }

    assert_eq!(fixture.names(), ["Two.md"]);
    assert_eq!(fixture.two(), before);
}

#[test]
fn ctrl_c_during_any_wait_on_stdin_leaves_no_file() {
    let fixture = Fixture::new();
    let before = fixture.two();
    let filler = vec![b'x'; PAST_THE_PIPE_BUFFER];
    let silent = Running::start(&fixture, &["new", "Silent"]);
    let mut started = Running::start(&fixture, &["new", "Started"]);
    let mut started_dash = Running::start(&fixture, &["append", "Two.md", "-"]);
    let silent_dash = Running::start(&fixture, &["new", "SilentDash", "-"]);

    started.write(&filler);
    started_dash.write(&filler);
    for running in [&silent, &started, &started_dash] {
        running.interrupt();
        let (status, ran) = running.wait();
        assert_interrupted(status);
        assert!(ran < PIPE_FIRST_BYTE_WAIT, "ran {ran:?}");
    }
    silent_dash.wait_for_line(WAITING);
    silent_dash.interrupt();
    assert_interrupted(silent_dash.wait().0);

    assert_eq!(fixture.names(), ["Two.md"]);
    assert_eq!(fixture.two(), before);
}

#[test]
fn new_and_append_run_without_input_on_a_closed_stdin() {
    let fixture = Fixture::new();
    let before = fixture.two();
    for (name, closed, append_code) in [
        ("Null", Stdio::null as fn() -> Stdio, 2),
        ("Empty", Stdio::piped as fn() -> Stdio, 0),
    ] {
        let mut child = fixture.spawn(&["new", name], closed());
        drop(child.stdin.take());
        let output = child.wait_with_output().expect("output");
        assert!(output.status.success(), "{name}: {}", output.status);
        assert_eq!(read(&fixture.notes.join(format!("{name}.txt"))), "");

        let mut child = fixture.spawn(&["append", "Two.md"], closed());
        drop(child.stdin.take());
        let output = child.wait_with_output().expect("output");
        assert_eq!(output.status.code(), Some(append_code), "{name}");
        assert_eq!(fixture.two(), before);
    }
}

#[test]
fn a_dash_on_a_closed_stdin_reads_what_was_piped_and_on_none_exits_2() {
    let fixture = Fixture::new();

    let mut child = fixture.spawn(&["new", "Empty", "-"], Stdio::piped());
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("output");
    assert!(output.status.success(), "{}", output.status);
    assert_eq!(read(&fixture.notes.join("Empty.txt")), "");

    for args in [&["new", "Null", "-"][..], &["append", "Two.md", "-"]] {
        let output = fixture
            .spawn(args, Stdio::null())
            .wait_with_output()
            .expect("output");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("nothing is piped"),
            "{args:?}"
        );
    }
    assert!(!fixture.notes.join("Null.txt").exists());
}
