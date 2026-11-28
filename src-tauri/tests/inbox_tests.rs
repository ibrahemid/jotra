#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use jotra_core::events::bus::JotraEvent;
use jotra_tauri_lib::commands::buffer::save_buffer_content_inner;
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::commands::inbox::{clear_inbox_inner, set_inbox_path_from_path};
use jotra_tauri_lib::security::{canonicalize_for_authorization, canonicalize_root};
use jotra_tauri_lib::state::AppState;
use jotra_tauri_lib::watcher::handler::{create_ignore_set, InboxEvents};
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

#[test]
fn set_inbox_canonicalizes_persists_and_starts_watcher() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let inbox = TempDir::new().unwrap();

    let root = set_inbox_path_from_path(&state, inbox.path()).expect("set inbox");

    assert_eq!(
        root,
        jotra_tauri_lib::security::canonicalize_root(inbox.path())
            .unwrap()
            .to_string_lossy()
    );
    assert!(state.inbox_watcher.lock().unwrap().is_some());
    assert_eq!(
        state.config.lock().unwrap().inbox.path.as_deref(),
        Some(root.as_str())
    );

    let persisted = state.config_store.read().expect("read config back");
    assert_eq!(persisted.inbox.path.as_deref(), Some(root.as_str()));
    assert!(persisted.inbox.focus, "focus default must survive persist");
}

#[test]
fn set_inbox_rejects_file_path() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let inbox = TempDir::new().unwrap();
    let file = inbox.path().join("a.txt");
    std::fs::write(&file, "x").unwrap();

    assert!(set_inbox_path_from_path(&state, &file).is_err());
}

#[test]
fn set_inbox_rejects_missing_path() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    assert!(
        set_inbox_path_from_path(&state, std::path::Path::new("/nonexistent/jotra-inbox")).is_err()
    );
}

#[test]
fn clear_inbox_drops_state_watcher_and_config() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let inbox = TempDir::new().unwrap();

    set_inbox_path_from_path(&state, inbox.path()).expect("set inbox");
    clear_inbox_inner(&state).expect("clear inbox");

    assert!(state.inbox_root.lock().unwrap().is_none());
    assert!(state.inbox_watcher.lock().unwrap().is_none());
    assert!(state.config.lock().unwrap().inbox.path.is_none());

    let persisted = state.config_store.read().expect("read config back");
    assert!(persisted.inbox.path.is_none());
}

/// Long enough for a loaded machine; only a failing run waits it out.
const DELIVERY_BOUND: Duration = Duration::from_secs(15);

/// Every arrival the bus carries, in the order the watcher's thread said them.
fn arrivals_on(state: &AppState) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    state.event_bus.subscribe(move |event| {
        if let JotraEvent::InboxFileArrived { path } = event {
            let _ = tx.send(path.clone());
        }
    });
    rx
}

/// The arrivals received up to and including the first one for a path ending
/// in `name`, or `None` when it did not come within [`DELIVERY_BOUND`].
fn arrivals_through(rx: &mpsc::Receiver<String>, name: &str) -> Option<Vec<String>> {
    let deadline = Instant::now() + DELIVERY_BOUND;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let path = rx.recv_timeout(left).ok()?;
        let done = path.ends_with(name);
        seen.push(path);
        if done {
            return Some(seen);
        }
    }
    None
}

/// The one test here that runs the platform's watcher. Delivery lags on a
/// loaded machine, so a file that has not been reported is followed by
/// another, each under a new name.
#[test]
fn the_platform_watcher_reports_a_new_file_in_the_inbox() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let inbox = TempDir::new().unwrap();
    let rx = arrivals_on(&state);

    set_inbox_path_from_path(&state, inbox.path()).expect("set inbox");

    let heard = (0..4).any(|attempt| {
        let name = format!("report-{attempt}.md");
        std::fs::write(inbox.path().join(&name), "# finished").unwrap();
        arrivals_through(&rx, &name).is_some()
    });
    assert!(heard, "a file written into the inbox never arrived");
}

#[test]
fn inbox_watcher_never_emits_for_preexisting_backlog() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let inbox = TempDir::new().unwrap();

    let backlog = inbox.path().join("old-report.md");
    std::fs::write(&backlog, "stale").unwrap();

    let rx = arrivals_on(&state);
    let root = PathBuf::from(set_inbox_path_from_path(&state, inbox.path()).expect("set inbox"));
    let watcher = state.inbox_watcher.lock().unwrap();
    let watcher = watcher.as_ref().expect("the inbox watcher");

    std::fs::write(&backlog, "stale but touched").unwrap();
    watcher
        .deliver(&[root.join("old-report.md")])
        .expect("deliver the touch");
    // Delivered after the touch on the same channel, so the touch has been
    // judged by the time this arrives.
    std::fs::write(root.join("fresh.md"), "new").unwrap();
    watcher
        .deliver(&[root.join("fresh.md")])
        .expect("deliver the new file");

    let seen = arrivals_through(&rx, "fresh.md").expect("the new file arrived");
    assert_eq!(
        seen.iter().filter(|p| p.ends_with("old-report.md")).count(),
        0,
        "pre-existing files must never auto-open, even when modified: {seen:?}"
    );
}

#[test]
fn an_arrival_delivered_again_opens_once() {
    let inbox = TempDir::new().unwrap();
    let root = canonicalize_root(inbox.path()).unwrap();
    let mut events = InboxEvents::new(root.clone(), create_ignore_set());
    assert_eq!(events.preexisting(), 0);

    let file = root.join("report.md");
    std::fs::write(&file, "# finished").unwrap();
    // The second delivery is the echo of the reads that classify the first.
    let said: Vec<JotraEvent> = [&file, &root, &file]
        .into_iter()
        .filter_map(|path| events.report(path, Instant::now()))
        .collect();

    assert_eq!(said.len(), 1, "{said:?}");
    assert!(matches!(
        &said[0],
        JotraEvent::InboxFileArrived { path } if path.ends_with("report.md")
    ));
}

#[test]
fn open_file_allows_path_inside_inbox() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let inbox = TempDir::new().unwrap();
    let note = inbox.path().join("arrived.md");
    std::fs::write(&note, "inbox file").unwrap();

    set_inbox_path_from_path(&state, inbox.path()).expect("set inbox");

    let result = open_file_from_path(&state, &note.to_string_lossy());
    assert!(result.is_ok(), "inbox-contained open must pass: {result:?}");
}

#[test]
fn open_file_still_rejects_path_outside_inbox() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let inbox = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "shhh").unwrap();

    set_inbox_path_from_path(&state, inbox.path()).expect("set inbox");

    let result = open_file_from_path(&state, &secret.to_string_lossy());
    assert!(result.is_err(), "outside-inbox open must stay rejected");
}

#[test]
fn open_file_rejects_inbox_path_after_clear() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let inbox = TempDir::new().unwrap();
    let note = inbox.path().join("late.md");
    std::fs::write(&note, "x").unwrap();

    set_inbox_path_from_path(&state, inbox.path()).expect("set inbox");
    clear_inbox_inner(&state).expect("clear inbox");

    let result = open_file_from_path(&state, &note.to_string_lossy());
    assert!(
        result.is_err(),
        "clearing the inbox must revoke folder-derived authorization"
    );
}

/// Opens the file at `path` through the command layer and saves `content` into
/// it, which is the only way a save reaches the ignore set.
fn save_through_jotra(state: &AppState, path: &std::path::Path, content: &str) {
    let canonical = canonicalize_for_authorization(path).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    let opened = open_file_from_path(state, &canonical).expect("open");
    save_buffer_content_inner(
        state,
        &opened.doc.as_ref().expect("the file opened").id,
        content,
    )
    .expect("save");
}

fn classify_arrival(
    state: &AppState,
    path: &std::path::Path,
    root: &std::path::Path,
) -> Option<jotra_core::events::bus::JotraEvent> {
    jotra_tauri_lib::watcher::handler::classify_inbox_event(
        path,
        root,
        &std::collections::HashSet::new(),
        &state.watcher_ignore,
        jotra_core::watcher::ignore::DEFAULT_IGNORE_TTL,
        std::time::Instant::now(),
    )
}

#[test]
fn a_jotra_save_of_notes_a_index_md_does_not_suppress_an_external_change_to_notes_b_index_md() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let watched = TempDir::new().unwrap();
    let root = canonicalize_root(watched.path()).expect("canonical");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();

    let a = root.join("a").join("index.md");
    std::fs::write(&a, "what a held").unwrap();
    save_through_jotra(&state, &a, "what jotra wrote");

    // b holds exactly the bytes Jotra wrote into a, so the fingerprint cannot
    // tell the two files apart. Only the key can, and a bare `index.md` key
    // was shared by both.
    let b = root.join("b").join("index.md");
    std::fs::write(&b, "what jotra wrote").unwrap();

    assert!(
        classify_arrival(&state, &b, &root).is_some(),
        "a save of a/index.md must not swallow b/index.md"
    );
    assert!(
        classify_arrival(&state, &a, &root).is_none(),
        "the file Jotra saved is still its own write"
    );
}

#[test]
fn one_jotra_save_fanning_out_into_several_events_is_fully_suppressed() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let watched = TempDir::new().unwrap();
    let root = canonicalize_root(watched.path()).expect("canonical");

    let note = root.join("agent-output.md");
    std::fs::write(&note, "# from somewhere").unwrap();
    save_through_jotra(&state, &note, "# edited in jotra");

    for attempt in 1..=3 {
        assert!(
            classify_arrival(&state, &note, &root).is_none(),
            "event {attempt} of one write must stay suppressed"
        );
    }
}

#[test]
fn editing_a_file_named_config_toml_in_the_watched_folder_does_not_suppress_a_real_config_reload() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let watched = TempDir::new().unwrap();
    let root = canonicalize_root(watched.path()).expect("canonical");

    let note = root.join("config.toml");
    std::fs::write(&note, "theme = \"dark\"\n").unwrap();
    save_through_jotra(&state, &note, "theme = \"light\"\n");

    // The config file now holds what Jotra wrote into the note, so the bytes
    // match the stamp and the namespace is all that separates them.
    let config_path = state.config_store.path().to_path_buf();
    std::fs::write(&config_path, "theme = \"light\"\n").unwrap();

    let event = jotra_tauri_lib::watcher::handler::classify_watch_event(
        &config_path,
        &config_path,
        &state.watcher_ignore,
        jotra_core::watcher::ignore::DEFAULT_IGNORE_TTL,
        std::time::Instant::now(),
    );
    assert!(
        matches!(
            event,
            Some(jotra_core::events::bus::JotraEvent::ConfigChanged { .. })
        ),
        "a note named config.toml must not stand in for the config file: {event:?}"
    );
}

#[test]
fn a_jotra_config_write_does_not_suppress_an_external_edit_to_config_toml_in_the_watched_folder() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let watched = TempDir::new().unwrap();
    let root = canonicalize_root(watched.path()).expect("canonical");

    // Setting the folder persists the config, which stamps the data folder's
    // own config.toml.
    set_inbox_path_from_path(&state, &root).expect("set inbox");
    let written = std::fs::read(state.config_store.path()).expect("read the config back");

    // Somebody else's file of the same name holding the same bytes: the
    // fingerprint matches, so only the key namespace tells the two apart.
    let lookalike = root.join("config.toml");
    std::fs::write(&lookalike, &written).unwrap();

    assert!(
        classify_arrival(&state, &lookalike, &root).is_some(),
        "a config write must not swallow an edit to a config.toml in the watched folder"
    );
}
