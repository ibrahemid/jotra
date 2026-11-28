//! Reading a pipe: without limit when `-` asks, within a bound on the first
//! byte otherwise, and to its end once a byte has come.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use jotra_cli::{read_pipe, PipeReadError, PipeWait};

/// Long enough that a test waiting on it has failed, short of hanging the suite.
const TEST_LIMIT: Duration = Duration::from_secs(30);

/// A wait no test reaches.
const FOREVER: Duration = Duration::from_secs(3_600);

/// Runs `read_pipe` on a thread of its own, so a test can watch it from outside.
fn read_in_background(
    source: impl Read + Send + 'static,
    wait: PipeWait,
) -> mpsc::Receiver<Result<Vec<u8>, PipeReadError>> {
    let (finished, result) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = finished.send(read_pipe(source, wait, || {}));
    });
    result
}

#[test]
fn with_a_dash_a_writer_whose_first_byte_comes_after_the_notice_is_read_whole() {
    let (reader, mut writer) = io::pipe().expect("pipe");
    let (noticed, notice) = mpsc::channel();
    let writing = std::thread::spawn(move || {
        notice.recv_timeout(TEST_LIMIT).expect("the wait notice");
        writer.write_all(b"late ").expect("write");
        writer.write_all(b"text").expect("write");
    });

    let read = read_pipe(
        reader,
        PipeWait::UntilClosed {
            notice_after: Duration::ZERO,
        },
        move || noticed.send(()).expect("notice the writer"),
    );
    writing.join().expect("writer");

    assert_eq!(read.expect("the pipe is read"), b"late text");
}

#[test]
fn without_a_dash_a_silent_open_pipe_ends_within_the_bound() {
    let (reader, writer) = io::pipe().expect("pipe");
    let bound = Duration::from_millis(50);
    let started = Instant::now();

    let read = read_pipe(reader, PipeWait::FirstByteWithin(bound), || {
        panic!("no notice without a dash")
    });

    let waited = started.elapsed();
    assert!(
        matches!(read, Err(PipeReadError::NothingInTime(limit)) if limit == bound),
        "a silent pipe is not a text"
    );
    assert!(waited >= bound && waited < TEST_LIMIT, "waited {waited:?}");
    drop(writer);
}

#[test]
fn without_a_dash_a_writer_slower_than_the_bound_fails_and_its_text_is_not_taken() {
    let (reader, mut writer) = io::pipe().expect("pipe");
    let (gave_up, given_up) = mpsc::channel();
    let writing = std::thread::spawn(move || {
        given_up.recv_timeout(TEST_LIMIT).expect("the read gave up");
        let _ = writer.write_all(b"late text");
    });

    let read = read_pipe(reader, PipeWait::FirstByteWithin(Duration::ZERO), || {});
    gave_up.send(()).expect("tell the writer");
    writing.join().expect("writer");

    assert!(matches!(read, Err(PipeReadError::NothingInTime(_))));
}

#[test]
fn without_a_dash_a_first_byte_in_time_lifts_the_bound_on_the_rest() {
    let (reader, mut writer) = io::pipe().expect("pipe");
    let bound = Duration::from_millis(500);
    writer.write_all(b"first ").expect("write");

    let result = read_in_background(reader, PipeWait::FirstByteWithin(bound));
    assert!(
        matches!(
            result.recv_timeout(bound * 3),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the read ended at the bound with the pipe still open"
    );
    writer.write_all(b"second").expect("write");
    drop(writer);

    let read = result.recv_timeout(TEST_LIMIT).expect("the read ends");
    assert_eq!(read.expect("the pipe is read"), b"first second");
}

#[test]
fn a_pipe_closed_with_nothing_in_it_ends_at_once_with_no_notice() {
    for wait in [
        PipeWait::UntilClosed {
            notice_after: FOREVER,
        },
        PipeWait::FirstByteWithin(FOREVER),
    ] {
        let (reader, writer) = io::pipe().expect("pipe");
        drop(writer);
        let noticed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&noticed);
        let (finished, result) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = finished.send(read_pipe(reader, wait, move || {
                flag.store(true, Ordering::SeqCst);
            }));
        });

        let read = result.recv_timeout(TEST_LIMIT).expect("the read ends");
        assert!(read.expect("the pipe is read").is_empty(), "{wait:?}");
        assert!(!noticed.load(Ordering::SeqCst), "{wait:?}");
    }
}

/// A source that answers each read from a script, then reports the end.
struct Scripted(VecDeque<io::Result<&'static [u8]>>);

impl Read for Scripted {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.pop_front() {
            None => Ok(0),
            Some(Ok(bytes)) => {
                buf[..bytes.len()].copy_from_slice(bytes);
                Ok(bytes.len())
            }
            Some(Err(error)) => Err(error),
        }
    }
}

#[test]
fn a_read_cut_short_by_a_signal_or_a_nonblocking_pipe_is_tried_again() {
    for wait in [
        PipeWait::UntilClosed {
            notice_after: FOREVER,
        },
        PipeWait::FirstByteWithin(FOREVER),
    ] {
        let source = Scripted(VecDeque::from([
            Err(io::Error::from(io::ErrorKind::Interrupted)),
            Ok(&b"a"[..]),
            Err(io::Error::from(io::ErrorKind::Interrupted)),
            Err(io::Error::from(io::ErrorKind::WouldBlock)),
            Ok(&b"b"[..]),
        ]));

        let read = read_pipe(source, wait, || {});

        assert_eq!(read.expect("the pipe is read"), b"ab", "{wait:?}");
    }
}

#[test]
fn a_read_that_fails_mid_pipe_is_an_error_not_a_shorter_text() {
    for wait in [
        PipeWait::UntilClosed {
            notice_after: FOREVER,
        },
        PipeWait::FirstByteWithin(FOREVER),
    ] {
        let source = Scripted(VecDeque::from([
            Ok(&b"partial"[..]),
            Err(io::Error::from(io::ErrorKind::BrokenPipe)),
        ]));

        let read = read_pipe(source, wait, || {});

        assert!(
            matches!(&read, Err(PipeReadError::Failed(error)) if error.kind() == io::ErrorKind::BrokenPipe),
            "{wait:?}"
        );
    }
}

#[test]
fn the_message_for_a_silent_pipe_names_both_ways_out() {
    let message = PipeReadError::NothingInTime(Duration::from_secs(5)).to_string();

    assert!(!message.contains('\n'));
    assert!(message.contains("5 seconds"), "{message}");
    assert!(message.contains("pass - to wait"), "{message}");
    assert!(message.contains("close stdin"), "{message}");
}
