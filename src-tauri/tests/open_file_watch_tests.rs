//! The open-file and notes watchers against a real filesystem.
//!
//! The unit tests in `watcher::open_files` prove the registry's bookkeeping
//! and the classifier's decisions with injected backends. These write real
//! files and hand the watchers' delivery state ([`OpenFileDeliveries`],
//! [`NotesDeliveries`]) the paths the platform reports after such a write, each
//! delivery twice, so what reaches a tab is decided without waiting on a
//! watcher. One runs the platform's own watcher, because the one thing an
//! injected backend cannot prove is that a temp-plus-rename over a watched file
//! reaches Jotra at all — which is the write every careful program makes, and
//! the reason the folder is watched rather than the file.

use jotra_core::events::bus::{EventBus, JotraEvent};
use jotra_core::hash::sha256_bytes;
use jotra_core::notes::guard::DiskState;
use jotra_core::notes::identity::FileIdentity;
use jotra_core::watcher::budget::DEFAULT_SWEEP_COOLDOWN;
use jotra_core::watcher::change_event::ExternalChange;
use jotra_tauri_lib::security::resolve_for_containment;
use jotra_tauri_lib::watcher::handler::{create_ignore_set, IgnoreSet, NotesDeliveries};
use jotra_tauri_lib::watcher::moves::{FileTracking, MoveOutcome, NoteFiles};
use jotra_tauri_lib::watcher::open_files::{
    start_open_file_watcher, DirWatcher, NoOpenNotes, OpenFileDeliveries, OpenFileRegistry,
    WatchOutcome, WatcherKind,
};
use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// The path both the ignore stamps and the watcher registry key on. The
/// handler's own `ignore_key_path` is crate-private; this is the same
/// resolution, reached through the function it delegates to.
fn resolved(path: &Path) -> String {
    resolve_for_containment(path).unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The notes folder as the app holds it: canonical, the way startup stores it
/// and the way the platform's watcher reports paths under it. A watcher rooted
/// at an uncanonical path drops every event, because the containment check
/// misses.
fn canonical(dir: &Path) -> PathBuf {
    PathBuf::from(resolved(dir))
}

/// Writes `bytes` to `path` the way a careful program does: into a sibling
/// temp file, then renamed over the target. The rename gives the file a new
/// inode, which is what kills a watch bound to the file rather than the
/// folder. Returns the temp file's path, which the platform reports too.
fn write_by_temp_and_rename(path: &Path, bytes: &[u8]) -> PathBuf {
    let temp = path.with_extension("jotra-test-tmp");
    std::fs::write(&temp, bytes).expect("write temp");
    std::fs::rename(&temp, path).expect("rename over target");
    temp
}

/// Tracking for tabs that have read their files, which is what having one open
/// means: the digest the document was loaded from is on record, and a report
/// carrying those same bytes is not a change (ADR-033 §13).
///
/// `FileTracking::untracked()` has no such record, so every report is news to
/// it, including a delivery of the write that seeded the file.
struct TabsThatHaveRead {
    read: HashMap<String, DiskState>,
}

impl TabsThatHaveRead {
    /// Tracking holding `loaded` as what `note_id` read from `path`.
    ///
    /// The bytes are passed rather than read back, so the record is the one the
    /// test states and not whatever the file happens to hold when the harness
    /// runs. A double that samples the file would suppress a change written
    /// after it was built, and the test would pass for the wrong reason.
    fn holding(note_id: &str, path: &Path, loaded: &[u8]) -> FileTracking {
        let state = DiskState {
            hash: sha256_bytes(loaded),
            size: loaded.len() as u64,
            mtime: std::fs::metadata(path).and_then(|it| it.modified()).ok(),
        };
        FileTracking {
            probe: FileTracking::untracked().probe,
            files: Arc::new(Self {
                read: HashMap::from([(note_id.to_string(), state)]),
            }),
            holds: Default::default(),
        }
    }
}

impl NoteFiles for TabsThatHaveRead {
    fn identity_of(&self, _note_id: &str) -> Option<FileIdentity> {
        None
    }

    fn note_file_moved(&self, _note_id: &str, _from: &Path, _to: &Path) -> MoveOutcome {
        MoveOutcome::Followed
    }

    fn note_file_removed(&self, _note_id: &str, _path: &Path) -> bool {
        true
    }

    fn note_file_returned(&self, _note_id: &str, _path: &Path) -> bool {
        false
    }

    fn note_file_changed(&self, _note_id: &str) {}

    fn last_disk_state(&self, note_id: &str) -> Option<DiskState> {
        self.read.get(note_id).copied()
    }

    fn notes_root(&self) -> Option<PathBuf> {
        None
    }
}

/// A backend that delivers nothing and keeps the folders it was asked to watch.
#[derive(Clone, Default)]
struct RecordingDirs(Arc<Mutex<BTreeSet<PathBuf>>>);

impl RecordingDirs {
    fn watched(&self) -> BTreeSet<PathBuf> {
        self.0.lock().expect("watched folders").clone()
    }
}

impl DirWatcher for RecordingDirs {
    fn watch_dir(&mut self, dir: &Path) -> Result<(), String> {
        self.0
            .lock()
            .expect("watched folders")
            .insert(dir.to_path_buf());
        Ok(())
    }

    fn unwatch_dir(&mut self, dir: &Path) -> Result<(), String> {
        self.0.lock().expect("watched folders").remove(dir);
        Ok(())
    }
}

/// The open-file watcher as the app runs it, over backends that record what
/// they were asked to watch instead of watching it.
struct OpenFiles {
    registry: Arc<Mutex<OpenFileRegistry>>,
    native: RecordingDirs,
    deliveries: OpenFileDeliveries,
}

impl OpenFiles {
    fn new(notes_root: &Path, ignore: &IgnoreSet, tracking: FileTracking) -> Self {
        let native = RecordingDirs::default();
        let registry = Arc::new(Mutex::new(OpenFileRegistry::new(
            Box::new(native.clone()),
            Box::new(RecordingDirs::default()),
            notes_root,
        )));
        let deliveries = OpenFileDeliveries::new(registry.clone(), ignore.clone(), tracking);
        Self {
            registry,
            native,
            deliveries,
        }
    }

    fn open(&self, note_id: &str, file: &Path) -> WatchOutcome {
        self.registry
            .lock()
            .expect("registry")
            .watch_parent_of(note_id, file)
    }

    /// The notes watcher, routing to the tabs this registry holds.
    fn notes_watcher(&self, root: PathBuf, ignore: &IgnoreSet, tracking: FileTracking) -> Notes {
        Notes(NotesDeliveries::new(
            root,
            ignore.clone(),
            Arc::new(self.registry.clone()),
            tracking,
        ))
    }

    /// What reaches the tabs when the platform reports a write in `folder`.
    fn reported_in(&mut self, folder: &Path, gone: &[PathBuf]) -> Vec<JotraEvent> {
        let events = platform_report(folder, gone);
        let mut reported = Vec::new();
        for _ in 0..2 {
            reported.extend(self.deliveries.deliver(&events, Instant::now()));
        }
        reported
    }
}

struct Notes(NotesDeliveries);

impl Notes {
    /// What the notes watcher emits when the platform reports a write in
    /// `folder`.
    fn reported_in(&mut self, folder: &Path, gone: &[PathBuf]) -> Vec<JotraEvent> {
        let events = platform_report(folder, gone);
        let mut reported = Vec::new();
        for _ in 0..2 {
            self.0
                .deliver(&events, Instant::now, |event| reported.push(event));
        }
        reported
    }
}

/// One delivery as the platform makes it after a write in `folder`: every file
/// there and each path the write renamed away, in the folder's resolved
/// spelling, with a name that was never Jotra's among them.
fn platform_report(folder: &Path, gone: &[PathBuf]) -> Vec<DebouncedEvent> {
    let resolved_folder = canonical(folder);
    let mut paths = vec![resolved_folder.join(".tmpA1b2C3")];
    for path in gone {
        paths.push(resolved_folder.join(path.file_name().expect("a file name")));
    }
    for entry in std::fs::read_dir(folder).expect("list the folder") {
        paths.push(resolved_folder.join(entry.expect("folder entry").file_name()));
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| DebouncedEvent::new(path, DebouncedEventKind::Any))
        .collect()
}

fn external(events: &[JotraEvent]) -> Vec<&JotraEvent> {
    events
        .iter()
        .filter(|event| matches!(event, JotraEvent::BufferExternal { .. }))
        .collect()
}

/// `seen` is one message telling `note_id` that `file` now holds `bytes`.
fn assert_one_modified(seen: &[&JotraEvent], note_id: &str, file: &Path, bytes: &[u8]) {
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    match seen[0] {
        JotraEvent::BufferExternal {
            buffer_id,
            path,
            change,
            new_path,
            disk_hash,
            ..
        } => {
            assert_eq!(buffer_id, note_id);
            assert_eq!(
                resolved(Path::new(path)),
                resolved(file),
                "the event names the file that changed"
            );
            assert_eq!(*change, ExternalChange::Modified);
            assert_eq!(*new_path, None);
            assert_eq!(
                disk_hash.as_deref(),
                Some(jotra_core::hash::comparison_digest_hex(bytes).as_str()),
                "the tab compares its document against this digest, so it has to be the file's"
            );
        }
        other => panic!("expected BufferExternal, got {other:?}"),
    }
}

fn stamp(ignore: &IgnoreSet, file: &Path, bytes: &[u8]) {
    ignore.lock().expect("ignore set").record(
        jotra_core::watcher::ignore::source_key(Path::new(&resolved(file))),
        bytes,
        Instant::now(),
    );
}

#[test]
fn a_file_opened_from_anywhere_reports_one_change_when_another_program_rewrites_it() {
    let notes = TempDir::new().expect("notes dir");
    let elsewhere = TempDir::new().expect("some other folder");
    let file = elsewhere.path().join("shared.md");
    std::fs::write(&file, b"as another program left it\n").expect("seed file");

    let mut open = OpenFiles::new(
        notes.path(),
        &create_ignore_set(),
        FileTracking::untracked(),
    );
    assert_eq!(
        open.open("note-1", &file),
        WatchOutcome::Watching(WatcherKind::Native)
    );
    assert_eq!(
        open.native.watched(),
        BTreeSet::from([canonical(elsewhere.path())]),
        "the folder is watched, not the file"
    );

    let temp = write_by_temp_and_rename(&file, b"rewritten by another program\n");

    let seen = open.reported_in(elsewhere.path(), &[temp]);
    assert_one_modified(
        &external(&seen),
        "note-1",
        &file,
        b"rewritten by another program\n",
    );
}

#[test]
fn a_write_jotra_stamped_never_comes_back_as_somebody_elses_edit() {
    // The full round trip for the rule W3 exists for: Jotra saves the file it
    // opened, the folder watcher sees the write, and the stamp keyed by the
    // file's resolved path stops it becoming an offer to reload the text the
    // tab is already showing.
    let notes = TempDir::new().expect("notes dir");
    let elsewhere = TempDir::new().expect("some other folder");
    let file = elsewhere.path().join("mine.md");
    std::fs::write(&file, b"before\n").expect("seed file");

    let ignore = create_ignore_set();
    let mut open = OpenFiles::new(notes.path(), &ignore, FileTracking::untracked());
    open.open("note-1", &file);

    let saved = b"saved from jotra\n";
    stamp(&ignore, &file, saved);
    let temp = write_by_temp_and_rename(&file, saved);

    let seen = open.reported_in(elsewhere.path(), &[temp]);
    assert!(
        external(&seen).is_empty(),
        "a stamped write must not arrive back as an external change, saw {seen:?}"
    );
}

#[test]
fn a_folder_stops_being_watched_when_the_last_tab_in_it_closes() {
    let notes = TempDir::new().expect("notes dir");
    let elsewhere = TempDir::new().expect("some other folder");
    let file = elsewhere.path().join("closing.md");
    std::fs::write(&file, b"before\n").expect("seed file");

    let mut open = OpenFiles::new(
        notes.path(),
        &create_ignore_set(),
        FileTracking::untracked(),
    );
    open.open("note-1", &file);
    open.registry
        .lock()
        .expect("registry")
        .unwatch_parent_of("note-1");
    assert!(open
        .registry
        .lock()
        .expect("registry")
        .watched_dirs()
        .is_empty());
    assert!(
        open.native.watched().is_empty(),
        "a closed tab's folder must not keep a watch"
    );

    // A delivery already on its way when the tab closed.
    let temp = write_by_temp_and_rename(&file, b"changed after the tab closed\n");
    let seen = open.reported_in(elsewhere.path(), &[temp]);
    assert!(
        external(&seen).is_empty(),
        "a closed tab must not be told, saw {seen:?}"
    );
}

#[test]
fn a_folder_full_of_churn_never_names_a_file_that_is_not_open() {
    // A plugin or a build tool writing hundreds of files into a watched folder
    // must not turn into hundreds of messages. Two things bound it: only a
    // file that is an open note is ever named, and each note is named at most
    // once per delivered window.
    let notes = TempDir::new().expect("notes dir");
    let busy = TempDir::new().expect("a busy folder");
    let one = busy.path().join("one.md");
    let two = busy.path().join("two.md");
    std::fs::write(&one, b"before\n").expect("seed one");
    std::fs::write(&two, b"before\n").expect("seed two");

    let mut open = OpenFiles::new(
        notes.path(),
        &create_ignore_set(),
        FileTracking::untracked(),
    );
    open.open("note-1", &one);
    open.open("note-2", &two);

    for round in 0..40 {
        std::fs::write(busy.path().join(format!("noise-{round}.log")), b"x").expect("noise");
        std::fs::write(&one, format!("round {round}\n")).expect("rewrite one");
        std::fs::write(&two, format!("round {round}\n")).expect("rewrite two");
    }

    let seen = open.reported_in(busy.path(), &[]);
    let mut named: Vec<&str> = external(&seen)
        .into_iter()
        .map(|event| match event {
            JotraEvent::BufferExternal { buffer_id, .. } => buffer_id.as_str(),
            _ => unreachable!("external returns nothing else"),
        })
        .collect();
    named.sort_unstable();
    assert_eq!(
        named,
        ["note-1", "note-2"],
        "120 writes in one window must cost one message per open note and none for anything else"
    );
}

#[test]
fn a_note_inside_the_notes_folder_reaches_its_tab_when_another_program_rewrites_it() {
    // W1's headline behaviour on the folder that holds nearly every note.
    // The notes watcher is the only thing watching in there — the open-file
    // registry deliberately arms nothing over the notes tree — so the route
    // from a change to the tab holding that file runs through it. Before this,
    // the change reached the index and stopped: the tab went on showing text
    // its file no longer held, and its next save was refused by the write
    // guard.
    let notes = TempDir::new().expect("notes dir");
    let note = notes.path().join("today.md");
    std::fs::write(&note, b"original text\n").expect("seed note");

    let ignore = create_ignore_set();
    let open = OpenFiles::new(notes.path(), &ignore, FileTracking::untracked());
    assert_eq!(
        open.open("note-1", &note),
        WatchOutcome::AlreadyCovered,
        "the notes watcher covers this folder; a second watch would double every event"
    );
    assert!(open.native.watched().is_empty());

    let mut notes_watcher =
        open.notes_watcher(canonical(notes.path()), &ignore, FileTracking::untracked());
    let temp = write_by_temp_and_rename(&note, b"rewritten by another program\n");

    let seen = notes_watcher.reported_in(notes.path(), &[temp]);
    assert_one_modified(
        &external(&seen),
        "note-1",
        &note,
        b"rewritten by another program\n",
    );
}

#[test]
fn a_save_jotra_made_inside_the_notes_folder_never_comes_back_to_the_tab() {
    // The same round trip on the notes side. Jotra stamps a write before it
    // lands; if the stamp missed, every save would return as somebody else's
    // edit and the user would be asked whether to discard their own keystrokes.
    let notes = TempDir::new().expect("notes dir");
    let note = notes.path().join("today.md");
    std::fs::write(&note, b"before\n").expect("seed note");

    let ignore = create_ignore_set();
    let open = OpenFiles::new(notes.path(), &ignore, FileTracking::untracked());
    open.open("note-1", &note);
    let mut notes_watcher =
        open.notes_watcher(canonical(notes.path()), &ignore, FileTracking::untracked());

    let saved = b"what jotra itself wrote\n";
    stamp(&ignore, &note, saved);
    let temp = write_by_temp_and_rename(&note, saved);

    let seen = notes_watcher.reported_in(notes.path(), &[temp]);
    assert!(
        external(&seen).is_empty(),
        "a stamped write must not be reported to the tab that made it, saw {seen:?}"
    );
}

#[test]
fn a_note_nobody_has_open_tells_no_tab() {
    // The registry is the whole filter. A notes folder holds every note the
    // user has; only the ones with a tab may produce an event.
    let notes = TempDir::new().expect("notes dir");
    let open_note = notes.path().join("open.md");
    let closed = notes.path().join("closed.md");
    std::fs::write(&open_note, b"x\n").expect("seed");
    std::fs::write(&closed, b"x\n").expect("seed");

    let ignore = create_ignore_set();
    // The tab on open.md has read it, so a report of open.md carries nothing
    // the tab does not already hold.
    let tracking = TabsThatHaveRead::holding("note-1", &open_note, b"x\n");
    let open = OpenFiles::new(notes.path(), &ignore, tracking.clone());
    open.open("note-1", &open_note);
    let mut notes_watcher = open.notes_watcher(canonical(notes.path()), &ignore, tracking);

    let temp = write_by_temp_and_rename(&closed, b"changed by another program\n");

    let seen = notes_watcher.reported_in(notes.path(), &[temp]);
    assert!(
        external(&seen).is_empty(),
        "a note with no tab open on it has nobody to tell, saw {seen:?}"
    );
}

fn swept(events: &[JotraEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, JotraEvent::NotesSwept { .. }))
        .count()
}

#[test]
fn a_burst_that_stops_while_a_sweep_stands_is_swept_once_more() {
    // One sweep stands for two seconds, and every change under it is dropped:
    // the walk that sweep started covers them. It covers only the files it has
    // not read yet, though, and a burst that ends inside the cooldown leaves
    // the last of them behind a walk that has already passed. Nothing else
    // will raise them — the folder has gone quiet — so the cooldown ending is
    // itself the moment to sweep again.
    let notes = TempDir::new().expect("notes dir");
    let mut watcher = NotesDeliveries::new(
        canonical(notes.path()),
        create_ignore_set(),
        Arc::new(NoOpenNotes),
        FileTracking::untracked(),
    );
    for n in 0..40 {
        std::fs::write(notes.path().join(format!("note-{n}.md")), b"x").expect("write note");
    }

    let start = Instant::now();
    let mut burst = Vec::new();
    watcher.deliver(
        &platform_report(notes.path(), &[]),
        || start,
        |event| burst.push(event),
    );
    assert!(
        swept(&burst) >= 1,
        "a burst of forty files was named one by one"
    );

    let due = watcher
        .deadline()
        .expect("the changes the standing sweep dropped are owed a sweep");
    assert_eq!(due, start + DEFAULT_SWEEP_COOLDOWN);

    let mut early = Vec::new();
    watcher.on_deadline(due - Duration::from_millis(1), |event| early.push(event));
    assert_eq!(swept(&early), 0, "the sweep that stands still covers them");

    let mut quiet = Vec::new();
    watcher.on_deadline(due, |event| quiet.push(event));
    assert_eq!(
        swept(&quiet),
        1,
        "the changes the cooldown swallowed were never swept"
    );

    // Nothing else is written from here on.
    assert_eq!(watcher.deadline(), None);
    let mut later = Vec::new();
    watcher.on_deadline(due + DEFAULT_SWEEP_COOLDOWN * 2, |event| later.push(event));
    assert_eq!(swept(&later), 0, "one silence costs one follow-up sweep");
}

#[test]
fn a_write_from_a_connected_program_reaches_the_tab_as_somebody_elses_edit() {
    // The route a tool call takes to a tab, end to end, with no channel of its
    // own: the client's process writes the file through the guarded facade
    // with no ignore stamp, the notes watcher sees the write, and the tab
    // holding that note is told once (ADR-031 rule 4 and ADR-033). A stamp on
    // that write would be swallowed by the very mechanism that keeps Jotra's
    // own saves from returning, so its absence is the thing under test.
    let notes = TempDir::new().expect("notes dir");
    let data = TempDir::new().expect("data dir");
    // One spelling of the folder for every side. A tab keyed by the path
    // `TempDir` hands back and a watcher rooted at the canonical one are two
    // folders as far as the registry can tell: Windows hands out the short
    // `RUNNER~1` form, `ToolHost::open` canonicalises whatever it is given,
    // and an event under one spelling never finds a tab under the other.
    let root = canonical(notes.path());
    let note = root.join("Launch.md");
    let read_by_the_tab = b"as the tab read it\n";
    std::fs::write(&note, read_by_the_tab).expect("seed note");
    std::fs::write(
        data.path().join("config.toml"),
        "[mcp]\nenabled = true\n\n[[mcp.approved_clients]]\nname = \"Test Client\"\nread = true\nwrite = true\n",
    )
    .expect("seed the settings the gate reads");

    let ignore = create_ignore_set();
    let tracking = TabsThatHaveRead::holding("note-1", &note, read_by_the_tab);
    let open = OpenFiles::new(&root, &ignore, tracking.clone());
    open.open("note-1", &note);
    let mut notes_watcher = open.notes_watcher(root.clone(), &ignore, tracking);

    // The server's own host, over the same folder, holding no index and no
    // ignore set — which is everything the process a client launches has.
    let host = jotra_mcp::tools::ToolHost::open(
        &root,
        &data.path().join("jotra.db"),
        data.path(),
        Box::new(jotra_mcp::tools::FixedAppFolder(root.clone())),
        Box::new(jotra_mcp::consent::ConfigGate::new(data.path())),
    )
    .expect("open the folder the way the served process does");
    let written = b"as a program left it\n";
    // The hash of the text the program read, which is what a client passes.
    let read_by_the_program = jotra_core::hash::sha256_hex(read_by_the_tab);
    std::thread::spawn(move || {
        host.write_note(
            &jotra_core::activity::ClientId::named("Test Client"),
            "Launch.md",
            std::str::from_utf8(written).expect("utf-8"),
            Some(&read_by_the_program),
            false,
        )
        .expect("the write is made");
    })
    .join()
    .expect("the writing thread finished");

    let seen = notes_watcher.reported_in(&root, &[]);
    assert_one_modified(&external(&seen), "note-1", &note, written);
    assert_eq!(
        std::fs::read(&note).expect("read the note back"),
        written,
        "the write landed on the file, not on a copy of it"
    );
}

#[test]
fn an_applied_proposal_reaches_the_tab_as_somebody_elses_edit() {
    // The chat pane's Apply takes the same route a connected program's write
    // takes: through the guarded facade with no ignore stamp, so the tab
    // holding that note reconciles through the folder watcher rather than
    // sitting on text the file no longer has (ADR-033). A stamp here would be
    // swallowed by the mechanism that keeps Jotra's own saves from returning,
    // and the tab would learn nothing, so its absence is what is under test.
    let notes = TempDir::new().expect("notes dir");
    let data = TempDir::new().expect("data dir");
    let root = canonical(notes.path());
    let note = root.join("Launch.md");
    let read_by_the_tab = b"as the tab read it\n";
    std::fs::write(&note, read_by_the_tab).expect("seed note");

    let ignore = create_ignore_set();
    let tracking = TabsThatHaveRead::holding("note-1", &note, read_by_the_tab);
    let open = OpenFiles::new(&root, &ignore, tracking.clone());
    open.open("note-1", &note);
    let mut notes_watcher = open.notes_watcher(root.clone(), &ignore, tracking);

    let written = "as the pane applied it\n";
    let outcome = jotra_tauri_lib::commands::chat::apply_proposal_inner(
        &root,
        &NoOpenNotes,
        data.path(),
        "api.example.com",
        "Launch.md",
        written,
        &jotra_core::hash::sha256_hex(read_by_the_tab),
        None,
        |_, _, _| unreachable!(),
    )
    .expect("the proposal is applied");
    assert_eq!(outcome.path, "Launch.md");

    let seen = notes_watcher.reported_in(&root, &[]);
    assert_one_modified(&external(&seen), "note-1", &note, written.as_bytes());
    assert_eq!(
        std::fs::read_to_string(&note).expect("read the note back"),
        written,
        "the proposal landed on the file, not on a copy of it"
    );
}

/// Long enough for a loaded machine; only a failing run waits it out.
const DELIVERY_BOUND: Duration = Duration::from_secs(15);

/// The one test here that runs the platform's watcher. Delivery lags on a
/// loaded machine, so a rewrite that has not been reported is made again, each
/// time with new bytes.
#[test]
fn the_platform_watcher_reports_a_rename_over_an_open_file() {
    let notes = TempDir::new().expect("notes dir");
    let elsewhere = TempDir::new().expect("some other folder");
    let file = elsewhere.path().join("shared.md");
    let seed = b"as another program left it\n";
    std::fs::write(&file, seed).expect("seed file");

    let bus = Arc::new(EventBus::new());
    let (tx, rx) = mpsc::channel();
    bus.subscribe(move |event| {
        let _ = tx.send(event.clone());
    });
    // The tab has read the seed, so a late report of the seed write is not a
    // change and only a rewrite can answer.
    let watcher = start_open_file_watcher(
        bus,
        create_ignore_set(),
        notes.path(),
        TabsThatHaveRead::holding("note-1", &file, seed),
    )
    .expect("start the open file watcher");
    let outcome = watcher
        .registry()
        .lock()
        .expect("registry")
        .watch_parent_of("note-1", &file);
    assert_eq!(outcome, WatchOutcome::Watching(WatcherKind::Native));

    let heard = (0..4).any(|attempt| {
        write_by_temp_and_rename(
            &file,
            format!("rewritten by another program, take {attempt}\n").as_bytes(),
        );
        let deadline = Instant::now() + DELIVERY_BOUND;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(JotraEvent::BufferExternal { buffer_id, .. }) if buffer_id == "note-1" => {
                    return true
                }
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    });
    assert!(
        heard,
        "a temp-plus-rename over an open file never reached its tab"
    );
}
