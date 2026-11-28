//! macOS: one FSEvents stream over every folder an open file sits in.
//!
//! `notify`'s FSEvents backend stops, joins and restarts its stream on every
//! `watch` and `unwatch`, on the calling thread, which is the main thread
//! under the registry's lock, and the new stream starts from "now": a change
//! landing between the stop and the start is never reported, and the tab shows
//! stale text with no prompt (TS-18).
//!
//! Here a thread of its own owns the streams. It folds a burst of requests
//! into one, drops every folder another one in the set already covers, and
//! starts a single stream over what is left before it lets the old one go, so
//! the two overlap rather than leave a gap. The old stream keeps reporting for
//! [`OVERLAP`] after the new one is running, which covers anything the daemon
//! had not yet handed it, and is stopped on a thread of its own. Each stream
//! delivers on a serial dispatch queue of its own. A change both
//! streams saw arrives twice and costs the watcher one `stat`
//! ([`jotra_core::watcher::sighting`]).
//!
//! `watch` waits for no stream. It lists the folder's entries, from metadata
//! alone, and hands that listing to the stream thread. Once a stream that
//! reports the folder is running, the thread lists the folder again and
//! reports every entry that differs, so a change made after `watch` returned
//! and before the stream ran is reported all the same, as it was by `notify`.
//!
//! A stream that fails to start changes nothing that was running: the old
//! stream keeps reporting its folders, and each folder no running stream
//! reports is listed every poll interval from the listing `watch` took, until
//! a later start succeeds. The next request, or [`RETRY_AFTER`] without one,
//! tries again.
//!
//! The new stream starts from "now" rather than from the last event id the old
//! one delivered. A stream started from a past id replays the daemon's history
//! before it reports anything live, and that replay took from 0.6 s to 3.4 s
//! here, holding back every change made after the open for as long.
//!
//! The stream sees whole trees; only an entry directly inside a watched folder,
//! or the folder itself, is passed on, which is the non-recursive watch every
//! other backend gives.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use notify::{Config, Event, EventHandler, EventKind, RecursiveMode, Watcher, WatcherKind};
use objc2_core_foundation::{CFArray, CFRetained, CFString};
use objc2_core_services::{
    kFSEventStreamCreateFlagFileEvents, kFSEventStreamCreateFlagNoDefer,
    kFSEventStreamEventFlagHistoryDone, kFSEventStreamEventIdSinceNow, ConstFSEventStreamRef,
    FSEventStreamContext, FSEventStreamCreate, FSEventStreamEventFlags, FSEventStreamEventId,
    FSEventStreamInvalidate, FSEventStreamRef, FSEventStreamRelease, FSEventStreamSetDispatchQueue,
    FSEventStreamStart, FSEventStreamStop,
};

use super::poll::{changes, snapshot, Snapshot};
use crate::poison::recover_poison;

/// How long a replaced stream keeps reporting after its successor started.
pub const OVERLAP: Duration = Duration::from_secs(1);

/// How often a folder no stream reports is listed, when the watcher's config
/// names no interval.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(4);

/// How long after a failed start the stream is tried again when no request
/// comes sooner.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// The handler every stream reports into, shared across rebuilds.
type SharedHandler = Arc<Mutex<dyn EventHandler>>;

/// Why a stream did not start.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// `FSEventStreamCreate` returned no stream.
    #[error("FSEventStreamCreate failed")]
    Create,
    /// `FSEventStreamStart` refused the stream.
    #[error("FSEventStreamStart failed")]
    Start,
}

/// The folders `folders` names, less every one that sits inside another of
/// them: a stream over a folder already sees everything under it.
pub fn stream_roots(folders: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    // In path order a folder comes before everything inside it.
    for folder in folders {
        if !roots.iter().any(|root| folder.starts_with(root)) {
            roots.push(folder.clone());
        }
    }
    roots
}

/// Whether an event at `path` belongs to a folder in `folders`: the folder
/// itself, or an entry directly inside it.
pub fn is_reported(path: &Path, folders: &BTreeSet<PathBuf>) -> bool {
    folders.contains(path) || path.parent().is_some_and(|parent| folders.contains(parent))
}

/// Starts the stream that reports `folders`, from `roots`.
trait StartStream: Send + 'static {
    type Stream: StopStream;

    fn start(
        &mut self,
        roots: &[PathBuf],
        folders: BTreeSet<PathBuf>,
        handler: SharedHandler,
    ) -> Result<Self::Stream, StreamError>;
}

/// A running stream, stopped once and never again.
trait StopStream: Send + 'static {
    fn stop(self);
}

/// A [`Watcher`] over FSEvents whose caller never waits for a stream.
pub struct FsEventsWatcher {
    /// Each folder as the caller named it, and as the stream sees it.
    folders: HashMap<PathBuf, PathBuf>,
    requests: mpsc::Sender<Request>,
}

/// The folders to watch, and the folder this request adds with what it held
/// when `watch` was asked.
struct Request {
    folders: BTreeSet<PathBuf>,
    added: Option<(PathBuf, Snapshot)>,
}

impl FsEventsWatcher {
    fn spawn<S: StartStream>(
        handler: SharedHandler,
        starter: S,
        poll_interval: Duration,
    ) -> notify::Result<Self> {
        let (requests, received) = mpsc::channel::<Request>();
        let keeper = StreamKeeper::new(starter, handler, poll_interval);
        std::thread::Builder::new()
            .name("jotra fsevents".to_string())
            .spawn(move || keeper.run(&received))
            .map_err(notify::Error::io)?;
        Ok(Self {
            folders: HashMap::new(),
            requests,
        })
    }

    fn request(&self, added: Option<(PathBuf, Snapshot)>) {
        let _ = self.requests.send(Request {
            folders: self.folders.values().cloned().collect(),
            added,
        });
    }
}

impl Watcher for FsEventsWatcher {
    fn new<F: EventHandler>(event_handler: F, config: Config) -> notify::Result<Self> {
        let poll_interval = config.poll_interval().unwrap_or(DEFAULT_POLL_INTERVAL);
        Self::spawn(Arc::new(Mutex::new(event_handler)), FsEvents, poll_interval)
    }

    fn watch(&mut self, path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
        let resolved = path
            .canonicalize()
            .map_err(|e| notify::Error::io(e).add_path(path.to_path_buf()))?;
        if !resolved.is_dir() {
            return Err(notify::Error::path_not_found().add_path(path.to_path_buf()));
        }
        if self.folders.contains_key(path) {
            return Ok(());
        }
        let before = snapshot(&resolved);
        self.folders.insert(path.to_path_buf(), resolved.clone());
        self.request(Some((resolved, before)));
        Ok(())
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        if self.folders.remove(path).is_none() {
            return Err(notify::Error::watch_not_found().add_path(path.to_path_buf()));
        }
        // A folder left in the running stream only reports a little more
        // than asked for until the rebuild.
        self.request(None);
        Ok(())
    }

    fn kind() -> WatcherKind {
        WatcherKind::Fsevent
    }
}

/// The stream thread's state: one stream at a time, replaced whenever the set
/// of folders changes, and a listing of every folder no stream reports.
struct StreamKeeper<S: StartStream> {
    starter: S,
    handler: SharedHandler,
    poll_interval: Duration,
    /// Every folder the caller wants watched.
    wanted: BTreeSet<PathBuf>,
    /// The stream reporting, and the folders it reports.
    running: Option<(S::Stream, BTreeSet<PathBuf>)>,
    /// Each folder no running stream reports, with what it held when it was
    /// last listed.
    polled: HashMap<PathBuf, Snapshot>,
    poll_due: Option<Instant>,
    retry_due: Option<Instant>,
}

impl<S: StartStream> StreamKeeper<S> {
    fn new(starter: S, handler: SharedHandler, poll_interval: Duration) -> Self {
        Self {
            starter,
            handler,
            poll_interval,
            wanted: BTreeSet::new(),
            running: None,
            polled: HashMap::new(),
            poll_due: None,
            retry_due: None,
        }
    }

    /// Serves requests until the watcher is dropped, listing the polled
    /// folders and retrying a failed start in between.
    fn run(mut self, requests: &mpsc::Receiver<Request>) {
        loop {
            let deadline = match (self.poll_due, self.retry_due) {
                (Some(poll), Some(retry)) => Some(poll.min(retry)),
                (poll, retry) => poll.or(retry),
            };
            let first = match deadline {
                None => match requests.recv() {
                    Ok(request) => request,
                    Err(_) => break,
                },
                Some(due) => {
                    match requests.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(request) => request,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            self.on_deadline(Instant::now());
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            };
            // A burst of opens is one rebuild, for the newest set.
            let mut wanted = first.folders;
            let mut added: HashMap<PathBuf, Snapshot> = first.added.into_iter().collect();
            while let Ok(newer) = requests.try_recv() {
                wanted = newer.folders;
                if let Some((folder, before)) = newer.added {
                    added.entry(folder).or_insert(before);
                }
            }
            self.apply(wanted, added);
        }
        if let Some((old, _)) = self.running.take() {
            old.stop();
        }
    }

    fn on_deadline(&mut self, now: Instant) {
        if self.retry_due.is_some_and(|due| now >= due) {
            self.rebuild();
        }
        if self.poll_due.is_some_and(|due| now >= due) {
            self.poll();
        }
    }

    /// Takes `wanted` as the folder set, each folder in `added` starting from
    /// the listing `watch` took, and rebuilds the stream over it.
    fn apply(&mut self, wanted: BTreeSet<PathBuf>, added: HashMap<PathBuf, Snapshot>) {
        self.wanted = wanted;
        self.polled.retain(|folder, _| self.wanted.contains(folder));
        for (folder, before) in added {
            if self.wanted.contains(&folder) && !self.is_streamed(&folder) {
                self.polled.entry(folder).or_insert(before);
            }
        }
        self.rebuild();
    }

    fn is_streamed(&self, folder: &Path) -> bool {
        self.running
            .as_ref()
            .is_some_and(|(_, folders)| folders.contains(folder))
    }

    /// Starts one stream over every wanted folder FSEvents can take. On
    /// success it catches up the folders it took over from the listing and
    /// retires the old stream; on failure the old stream and the listing stay.
    fn rebuild(&mut self) {
        self.retry_due = None;
        let streamable: BTreeSet<PathBuf> = self
            .wanted
            .iter()
            .filter(|folder| folder.to_str().is_some())
            .cloned()
            .collect();
        let roots = stream_roots(&streamable);
        if roots.is_empty() {
            if let Some((old, _)) = self.running.take() {
                retire(old);
            }
        } else {
            match self
                .starter
                .start(&roots, streamable.clone(), self.handler.clone())
            {
                Ok(stream) => {
                    let mut caught_up = Vec::new();
                    self.polled.retain(|folder, before| {
                        if !streamable.contains(folder) {
                            return true;
                        }
                        caught_up.extend(changes(before, &snapshot(folder)));
                        false
                    });
                    deliver(&self.handler, caught_up);
                    if let Some((old, _)) = self.running.replace((stream, streamable)) {
                        retire(old);
                    }
                }
                Err(error) => {
                    tracing::error!(
                        %error,
                        polled = self.polled.len(),
                        "the open files' folders could not be watched; listing them instead"
                    );
                    self.retry_due = Some(Instant::now() + RETRY_AFTER);
                }
            }
        }
        if self.polled.is_empty() {
            self.poll_due = None;
        } else if self.poll_due.is_none() {
            self.poll_due = Some(Instant::now() + self.poll_interval);
        }
    }

    /// Lists every polled folder and reports each entry that changed since
    /// the last listing.
    fn poll(&mut self) {
        let mut changed = Vec::new();
        for (folder, before) in &mut self.polled {
            let now = snapshot(folder);
            changed.extend(changes(before, &now));
            *before = now;
        }
        deliver(&self.handler, changed);
        self.poll_due = (!self.polled.is_empty()).then(|| Instant::now() + self.poll_interval);
    }
}

/// Stops `old` once it has overlapped its successor, on a thread of its own.
fn retire<T: StopStream>(old: T) {
    let _ = std::thread::Builder::new()
        .name("jotra fsevents retire".to_string())
        .spawn(move || {
            std::thread::sleep(OVERLAP);
            old.stop();
        });
}

/// Reports each of `paths` to the handler.
fn deliver(handler: &SharedHandler, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let mut handler = recover_poison(handler.lock(), "watcher::fsevents::deliver");
    for path in paths {
        handler.handle_event(Ok(Event::new(EventKind::Any).add_path(path)));
    }
}

/// The real streams, each delivering on a serial dispatch queue of its own.
struct FsEvents;

impl StartStream for FsEvents {
    type Stream = RunningStream;

    fn start(
        &mut self,
        roots: &[PathBuf],
        folders: BTreeSet<PathBuf>,
        handler: SharedHandler,
    ) -> Result<RunningStream, StreamError> {
        let paths: Vec<CFRetained<CFString>> = roots
            .iter()
            .filter_map(|root| root.to_str())
            .map(CFString::from_str)
            .collect();
        let paths = CFArray::from_retained_objects(&paths);
        let queue = DispatchQueue::new("jotra fsevents", DispatchQueueAttr::SERIAL);
        let info = Box::into_raw(Box::new(StreamContext { folders, handler }));
        let mut stream_context = FSEventStreamContext {
            version: 0,
            info: info.cast::<c_void>(),
            retain: None,
            release: None,
            copyDescription: None,
        };
        // SAFETY: the callback matches FSEvents' contract; `paths` is an array
        // of CFStrings, as FSEvents takes it; `info` is taken back only once
        // the stream is invalidated and its queue drained.
        unsafe {
            let stream = FSEventStreamCreate(
                None,
                Some(callback),
                &mut stream_context,
                paths.as_opaque(),
                kFSEventStreamEventIdSinceNow,
                0.0,
                kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer,
            );
            if stream.is_null() {
                drop(Box::from_raw(info));
                return Err(StreamError::Create);
            }
            FSEventStreamSetDispatchQueue(stream, Some(&queue));
            let running = RunningStream {
                stream: StreamRef(stream),
                queue,
                info: ContextRef(info),
            };
            if !FSEventStreamStart(stream) {
                running.tear_down(false);
                return Err(StreamError::Start);
            }
            Ok(running)
        }
    }
}

/// What the stream's callback reads.
struct StreamContext {
    folders: BTreeSet<PathBuf>,
    handler: SharedHandler,
}

/// A stream, moved to the thread that stops it.
struct StreamRef(FSEventStreamRef);

// SAFETY: a stream scheduled on a dispatch queue may be stopped, invalidated
// and released from any thread, and nothing else is done with it here.
unsafe impl Send for StreamRef {}

/// The context a stream's callback reads, moved to the thread that frees it.
struct ContextRef(*mut StreamContext);

// SAFETY: `StreamContext` is `Send`; the pointer is freed once, after the
// last callback that reads it.
unsafe impl Send for ContextRef {}

/// One running stream and the queue it delivers on.
struct RunningStream {
    stream: StreamRef,
    queue: DispatchRetained<DispatchQueue>,
    info: ContextRef,
}

impl RunningStream {
    /// Stops the stream when it was started, invalidates and releases it, and
    /// frees its context once no callback can still be reading it.
    fn tear_down(self, started: bool) {
        let stream = self.stream.0;
        // SAFETY: `stream` is the live stream `FsEvents::start` created; it is
        // released once, here.
        unsafe {
            if started {
                FSEventStreamStop(stream);
            }
            FSEventStreamInvalidate(stream);
        }
        // A callback FSEvents queued before the invalidation runs before this.
        self.queue.exec_sync(|| {});
        // SAFETY: as above; no callback runs after the queue drained, so the
        // context is read by nothing.
        unsafe {
            FSEventStreamRelease(stream);
            drop(Box::from_raw(self.info.0));
        }
    }
}

impl StopStream for RunningStream {
    fn stop(self) {
        self.tear_down(true);
    }
}

extern "C-unwind" fn callback(
    _stream: ConstFSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: NonNull<c_void>,
    flags: NonNull<FSEventStreamEventFlags>,
    _ids: NonNull<FSEventStreamEventId>,
) {
    // A panic must not cross into CoreServices.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: FSEvents hands `count` entries in each array, C strings in
        // `paths` (no `UseCFTypes` flag), and `info` is the context
        // `RunningStream::tear_down` frees only after the last callback.
        unsafe { deliver_stream_events(info, count, paths, flags) }
    }));
}

/// # Safety
///
/// The arguments are the ones FSEvents passed [`callback`].
unsafe fn deliver_stream_events(
    info: *mut c_void,
    count: usize,
    paths: NonNull<c_void>,
    flags: NonNull<FSEventStreamEventFlags>,
) {
    // SAFETY: see the function's contract.
    let context = unsafe { &*info.cast::<StreamContext>() };
    let paths = paths.cast::<*const c_char>();
    let mut reported = Vec::new();
    for i in 0..count {
        // SAFETY: `i < count`, the length of both arrays.
        let (flag, raw) = unsafe { (*flags.as_ptr().add(i), *paths.as_ptr().add(i)) };
        if flag & kFSEventStreamEventFlagHistoryDone != 0 {
            continue;
        }
        // SAFETY: FSEvents passes NUL-terminated paths.
        let Ok(path) = unsafe { CStr::from_ptr(raw) }.to_str() else {
            continue;
        };
        let path = PathBuf::from(path);
        if is_reported(&path, &context.folders) {
            reported.push(path);
        }
    }
    deliver(&context.handler, reported);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn set(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_folder_inside_another_needs_no_stream_of_its_own() {
        let folders = set(&["/a", "/a/b", "/a/b/c", "/ab", "/z"]);
        assert_eq!(
            stream_roots(&folders),
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/ab"),
                PathBuf::from("/z")
            ]
        );
    }

    #[test]
    fn only_an_entry_directly_inside_a_watched_folder_is_reported() {
        let folders = set(&["/a", "/a/b/c"]);
        assert!(is_reported(Path::new("/a/note.md"), &folders));
        assert!(is_reported(Path::new("/a"), &folders));
        assert!(is_reported(Path::new("/a/b/c/x.md"), &folders));
        assert!(!is_reported(Path::new("/a/b/note.md"), &folders));
        assert!(!is_reported(Path::new("/elsewhere/x.md"), &folders));
    }

    fn wait_for(rx: &mpsc::Receiver<notify::Result<Event>>, path: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Ok(event)) if event.paths.iter().any(|p| p == path) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    #[test]
    fn a_write_right_after_watch_returns_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher = FsEventsWatcher::new(tx, Config::default()).unwrap();

        watcher.watch(&root, RecursiveMode::NonRecursive).unwrap();
        let file = root.join("a.md");
        std::fs::write(&file, "x").unwrap();
        assert!(wait_for(&rx, &file), "the write was never reported");
    }

    #[test]
    fn a_write_landing_while_the_stream_is_rebuilt_is_still_reported() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_root = first.path().canonicalize().unwrap();
        let second_root = second.path().canonicalize().unwrap();
        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let watcher = Arc::new(Mutex::new(
            FsEventsWatcher::new(tx, Config::default()).unwrap(),
        ));
        watcher
            .lock()
            .unwrap()
            .watch(&first_root, RecursiveMode::NonRecursive)
            .unwrap();

        // Adding a folder replaces the stream; the write races the swap.
        let adder = {
            let watcher = watcher.clone();
            let second_root = second_root.clone();
            std::thread::spawn(move || {
                watcher
                    .lock()
                    .unwrap()
                    .watch(&second_root, RecursiveMode::NonRecursive)
                    .unwrap();
            })
        };
        let file = first_root.join("during.md");
        std::fs::write(&file, "x").unwrap();
        adder.join().unwrap();
        assert!(wait_for(&rx, &file), "a write during the rebuild was lost");

        let later = second_root.join("after.md");
        std::fs::write(&later, "y").unwrap();
        assert!(wait_for(&rx, &later), "the added folder is not watched");
    }

    #[test]
    fn an_unwatched_folder_is_refused_a_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher = FsEventsWatcher::new(tx, Config::default()).unwrap();
        watcher
            .watch(dir.path(), RecursiveMode::NonRecursive)
            .unwrap();
        watcher.unwatch(dir.path()).unwrap();
        assert!(watcher.unwatch(dir.path()).is_err());
        assert!(watcher
            .watch(&dir.path().join("missing"), RecursiveMode::NonRecursive)
            .is_err());
    }

    /// What one scripted start does.
    enum Outcome {
        Start,
        Fail,
        /// Starts once the sender side says so.
        StartWhen(mpsc::Receiver<()>),
    }

    /// Starts that report nothing of their own, so whatever the handler hears
    /// came from a listing.
    struct ScriptedStarts {
        outcomes: VecDeque<Outcome>,
        asked: mpsc::Sender<BTreeSet<PathBuf>>,
        stopped: Arc<Mutex<Vec<usize>>>,
        started: usize,
    }

    struct SilentStream {
        id: usize,
        stopped: Arc<Mutex<Vec<usize>>>,
    }

    impl StopStream for SilentStream {
        fn stop(self) {
            self.stopped.lock().unwrap().push(self.id);
        }
    }

    impl StartStream for ScriptedStarts {
        type Stream = SilentStream;

        fn start(
            &mut self,
            _roots: &[PathBuf],
            folders: BTreeSet<PathBuf>,
            _handler: SharedHandler,
        ) -> Result<SilentStream, StreamError> {
            let _ = self.asked.send(folders);
            match self.outcomes.pop_front().unwrap_or(Outcome::Start) {
                Outcome::Fail => return Err(StreamError::Start),
                Outcome::StartWhen(gate) => {
                    let _ = gate.recv();
                }
                Outcome::Start => {}
            }
            let id = self.started;
            self.started += 1;
            Ok(SilentStream {
                id,
                stopped: self.stopped.clone(),
            })
        }
    }

    struct Scripted {
        watcher: FsEventsWatcher,
        events: mpsc::Receiver<notify::Result<Event>>,
        asked: mpsc::Receiver<BTreeSet<PathBuf>>,
        stopped: Arc<Mutex<Vec<usize>>>,
    }

    const TEST_POLL: Duration = Duration::from_millis(50);

    fn scripted(outcomes: Vec<Outcome>) -> Scripted {
        let (tx, events) = mpsc::channel::<notify::Result<Event>>();
        let (asked_tx, asked) = mpsc::channel();
        let stopped = Arc::new(Mutex::new(Vec::new()));
        let starter = ScriptedStarts {
            outcomes: outcomes.into(),
            asked: asked_tx,
            stopped: stopped.clone(),
            started: 0,
        };
        let watcher = FsEventsWatcher::spawn(Arc::new(Mutex::new(tx)), starter, TEST_POLL).unwrap();
        Scripted {
            watcher,
            events,
            asked,
            stopped,
        }
    }

    fn asked_for(scripted: &Scripted) -> BTreeSet<PathBuf> {
        scripted
            .asked
            .recv_timeout(Duration::from_secs(10))
            .expect("a start was asked for")
    }

    fn folder() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    #[test]
    fn a_failed_start_keeps_the_running_stream_and_lists_the_new_folder() {
        let mut s = scripted(vec![Outcome::Start, Outcome::Fail]);
        let (_a, a) = folder();
        let (_b, b) = folder();

        s.watcher.watch(&a, RecursiveMode::NonRecursive).unwrap();
        asked_for(&s);
        s.watcher.watch(&b, RecursiveMode::NonRecursive).unwrap();
        assert_eq!(asked_for(&s), [a.clone(), b.clone()].into_iter().collect());

        let file = b.join("listed.md");
        std::fs::write(&file, "x").unwrap();
        assert!(
            wait_for(&s.events, &file),
            "a folder whose stream failed to start is not listed"
        );
        std::thread::sleep(OVERLAP + Duration::from_millis(300));
        assert!(
            s.stopped.lock().unwrap().is_empty(),
            "a failed start retired the stream that was running"
        );
    }

    #[test]
    fn watch_returns_while_the_stream_is_still_starting() {
        let (release, gate) = mpsc::channel();
        let mut s = scripted(vec![Outcome::StartWhen(gate)]);
        let (_a, a) = folder();

        let asked = Instant::now();
        s.watcher.watch(&a, RecursiveMode::NonRecursive).unwrap();
        assert!(
            asked.elapsed() < Duration::from_secs(1),
            "watch waited for the stream"
        );
        asked_for(&s);

        let file = a.join("before-the-stream.md");
        std::fs::write(&file, "x").unwrap();
        release.send(()).unwrap();
        assert!(
            wait_for(&s.events, &file),
            "a write before the stream ran was never reported"
        );
    }

    #[test]
    fn a_start_after_a_failure_takes_over_the_listed_folder() {
        let mut s = scripted(vec![Outcome::Fail, Outcome::Start]);
        let (_a, a) = folder();
        let (_b, b) = folder();

        s.watcher.watch(&a, RecursiveMode::NonRecursive).unwrap();
        asked_for(&s);
        let listed = a.join("listed.md");
        std::fs::write(&listed, "x").unwrap();
        assert!(wait_for(&s.events, &listed), "the folder is not listed");

        s.watcher.watch(&b, RecursiveMode::NonRecursive).unwrap();
        assert_eq!(
            asked_for(&s),
            [a.clone(), b.clone()].into_iter().collect(),
            "the retry left out the folder whose start failed"
        );
        // The stream reports nothing here; the listing it took over is gone.
        std::thread::sleep(TEST_POLL * 4);
        while s.events.try_recv().is_ok() {}
        std::fs::write(a.join("after.md"), "y").unwrap();
        std::thread::sleep(TEST_POLL * 6);
        assert!(
            s.events.try_recv().is_err(),
            "a folder a stream reports is still listed"
        );
    }

    #[test]
    fn an_empty_set_retires_the_running_stream() {
        let mut s = scripted(vec![Outcome::Start]);
        let (_a, a) = folder();

        s.watcher.watch(&a, RecursiveMode::NonRecursive).unwrap();
        asked_for(&s);
        s.watcher.unwatch(&a).unwrap();
        let deadline = Instant::now() + OVERLAP + Duration::from_secs(5);
        while s.stopped.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(*s.stopped.lock().unwrap(), vec![0]);
    }
}
