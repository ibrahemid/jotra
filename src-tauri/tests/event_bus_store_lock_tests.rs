//! No subscriber on the app's bus takes the buffer store (ADR-043 §1).
//!
//! The bus runs its subscribers on the emitting thread, and some emitters hold
//! the store while they emit, so a subscriber that took it would wait on its
//! own thread for good, with every later save and journal write behind it.
//! These tests hang the app's own subscribers on a state built here, hold the
//! store and emit one of every event; and they drive the two opens that
//! announce a change from under the store, a reopen of a tab whose file
//! another program changed and Today's note on a changed file, which also turn
//! the file's preview scripts off.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use jotra_core::events::bus::JotraEvent;
use jotra_core::preview::RenderRequest;
use jotra_core::watcher::change_event::ExternalChange;
use jotra_tauri_lib::bus_subscribers::{attach_bus_subscribers, BusHost};
use jotra_tauri_lib::commands::activity::announce_waiting_inner;
use jotra_tauri_lib::commands::buffer::read_buffer_content_inner;
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::commands::notes::open_or_mint_dated_note;
use jotra_tauri_lib::commands::preview::{
    preview_set_scripts_inner, render_into_cache, CachedRender,
};
use jotra_tauri_lib::events::JotraFrontendEvent;
use jotra_tauri_lib::preview::renderers::HtmlRenderer;
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

/// Long enough for an emit or an open on an idle machine many times over, and
/// short enough that one waiting on the store fails rather than hangs the
/// suite.
const DEADLINE: Duration = Duration::from_secs(5);

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// The application, as its subscribers reach it, with the frontend and the
/// menu bar left out.
#[derive(Clone)]
struct Headless {
    state: Arc<AppState>,
}

impl BusHost for Headless {
    fn app_state(&self) -> &AppState {
        &self.state
    }

    fn to_frontend(&self, _event: JotraFrontendEvent) {}

    fn redraw_menu_bar(&self) {}

    fn announce_waiting_clients(&self) {
        let _ = announce_waiting_inner(&self.state);
    }
}

/// A state with every subscriber the app hangs on its bus, and the HTML
/// renderer registered.
fn running(dir: &TempDir) -> Arc<AppState> {
    let state = Arc::new(make_state(dir));
    attach_bus_subscribers(&Headless {
        state: Arc::clone(&state),
    });
    state
        .preview_registry
        .write()
        .expect("registry")
        .register(Box::new(HtmlRenderer))
        .expect("register the HTML renderer");
    state
}

/// Renders `text` for `id` through the preview's render path, answering whether
/// the document was cached with its scripts on.
fn render(state: &AppState, id: &str, text: &str) -> bool {
    match render_into_cache(
        state,
        id,
        RenderRequest {
            content_type: HtmlRenderer::content_type_id(),
            buffer_text: text.to_string(),
            theme: Default::default(),
            zoom: 1.0,
            assets: None,
        },
    ) {
        CachedRender::Rendered {
            scripts_enabled, ..
        } => scripts_enabled,
        other => panic!("expected a render, got {other:?}"),
    }
}

/// A tab the editor has read, its scripts turned on and its preview rendered
/// with them.
fn shown_with_scripts_on(state: &AppState, id: &str) {
    let text = read_buffer_content_inner(state, id).expect("the editor reads the file");
    preview_set_scripts_inner(state, id, true).expect("turn the scripts on");
    assert!(render(state, id, &String::from_utf8(text).expect("utf-8")));
}

/// `name` in the notes folder holding `body`, opened as a tab and shown with
/// its scripts on.
fn opted_in_note(state: &AppState, name: &str, body: &str) -> (String, String) {
    let path = state.notes_root().join(name);
    std::fs::write(&path, body).expect("seed the file");
    let canonical = canonicalize_for_authorization(&path).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    let id = open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("the file opened")
        .id;
    shown_with_scripts_on(state, &id);
    (id, canonical)
}

fn row_opted_in(state: &AppState, id: &str) -> bool {
    state
        .store
        .lock()
        .expect("store")
        .preview_scripts(id)
        .expect("read the row")
}

fn cached_scripts_on(state: &AppState, id: &str) -> bool {
    state
        .preview_render_cache
        .get(id)
        .expect("a cached render")
        .scripts_enabled
}

/// Runs `call` on its own thread, as a command runs, and answers what it
/// returned, or `None` when it did not return within the deadline.
fn on_a_worker<T: Send + 'static>(
    state: &Arc<AppState>,
    call: impl FnOnce(&AppState) -> T + Send + 'static,
) -> Option<T> {
    let (done, finished) = mpsc::channel();
    let state = Arc::clone(state);
    std::thread::spawn(move || {
        let _ = done.send(call(&state));
    });
    finished.recv_timeout(DEADLINE).ok()
}

fn external(buffer_id: &str, path: &str, change: ExternalChange) -> JotraEvent {
    JotraEvent::BufferExternal {
        buffer_id: buffer_id.to_string(),
        path: path.to_string(),
        change,
        new_path: None,
        disk_hash: None,
        writer: None,
    }
}

/// One of every event the bus carries, about a tab that is open with its
/// scripts on.
fn every_event(state: &AppState, id: &str, path: &str) -> Vec<JotraEvent> {
    let root = state.notes_root().to_string_lossy().into_owned();
    let events = vec![
        JotraEvent::BufferOpened {
            id: id.to_string(),
            title: "report.html".to_string(),
        },
        JotraEvent::ConfigChanged {
            keys: vec!["editor.font_size".to_string()],
        },
        external(id, path, ExternalChange::Modified),
        external(id, path, ExternalChange::Removed),
        external(id, path, ExternalChange::Moved),
        JotraEvent::WorkspaceChanged {
            path: path.to_string(),
            removed: false,
        },
        JotraEvent::NotesChanged {
            path: path.to_string(),
            removed: false,
        },
        JotraEvent::NotesChanged {
            path: path.to_string(),
            removed: true,
        },
        JotraEvent::WorkspaceSwept {
            root: root.clone(),
            paths: Some(vec![path.to_string()]),
        },
        JotraEvent::WorkspaceSwept {
            root: root.clone(),
            paths: None,
        },
        JotraEvent::NotesSwept { root },
        JotraEvent::NotesIndexed {
            paths: vec![path.to_string()],
        },
        JotraEvent::InboxFileArrived {
            path: path.to_string(),
        },
        JotraEvent::HotkeyToggle,
        JotraEvent::MenuAction {
            action: "file.save".to_string(),
        },
        JotraEvent::McpPendingChanged,
        JotraEvent::McpCalledWhileOff,
        JotraEvent::FlushBeforeQuit,
        JotraEvent::PluginEvent {
            plugin_id: "jotra.test".to_string(),
            data: serde_json::json!({}),
        },
    ];
    for event in &events {
        listed(event);
    }
    events
}

/// Names every kind of event with no wildcard, so a kind added to `JotraEvent`
/// does not compile here until [`every_event`] emits one too.
fn listed(event: &JotraEvent) {
    match event {
        JotraEvent::BufferOpened { .. }
        | JotraEvent::ConfigChanged { .. }
        | JotraEvent::BufferExternal { .. }
        | JotraEvent::WorkspaceChanged { .. }
        | JotraEvent::WorkspaceSwept { .. }
        | JotraEvent::NotesChanged { .. }
        | JotraEvent::NotesSwept { .. }
        | JotraEvent::NotesIndexed { .. }
        | JotraEvent::InboxFileArrived { .. }
        | JotraEvent::HotkeyToggle
        | JotraEvent::MenuAction { .. }
        | JotraEvent::McpPendingChanged
        | JotraEvent::McpCalledWhileOff
        | JotraEvent::FlushBeforeQuit
        | JotraEvent::PluginEvent { .. } => {}
    }
}

#[test]
fn no_event_waits_on_the_buffer_store() {
    let dir = TempDir::new().expect("temp dir");
    let state = running(&dir);
    let (id, path) = opted_in_note(&state, "report.html", "<p>one</p><script>one()</script>");
    let events = every_event(&state, &id, &path);

    let held = state.store.lock().expect("store");
    for event in events {
        let named = format!("{event:?}");
        let bus = Arc::clone(&state.event_bus);
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            bus.emit(event);
            let _ = done.send(());
        });
        assert!(
            finished.recv_timeout(DEADLINE).is_ok(),
            "a subscriber waited on the buffer store while {named} was emitted"
        );
    }
    drop(held);
}

/// Every `.rs` file under the library's `src`, relative to it, with its text
/// up to its unit-test module.
fn library_sources() -> Vec<(PathBuf, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    let mut folders = vec![src.clone()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).expect("read the source folder") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path)
                    .expect("read the source")
                    .replace("\r\n", "\n");
                let shipped = match text.find("#[cfg(test)]\nmod ") {
                    Some(at) => text[..at].to_string(),
                    None => text,
                };
                sources.push((
                    path.strip_prefix(&src).expect("inside src").to_path_buf(),
                    shipped,
                ));
            }
        }
    }
    sources.sort();
    sources
}

/// The files whose shipped code holds `needle`, each with how many times.
fn calls_of(sources: &[(PathBuf, String)], needle: &str) -> Vec<(PathBuf, usize)> {
    sources
        .iter()
        .map(|(path, text)| (path.clone(), text.matches(needle).count()))
        .filter(|(_, count)| *count > 0)
        .collect()
}

#[test]
fn the_app_subscribes_to_its_bus_in_one_place() {
    let sources = library_sources();
    let subscribing: Vec<PathBuf> = calls_of(&sources, ".subscribe(")
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        subscribing,
        vec![
            PathBuf::from("bus_subscribers.rs"),
            PathBuf::from("events").join("bus_bridge.rs"),
        ],
        "a subscriber outside bus_subscribers.rs is one the store test does not hang"
    );
    assert_eq!(
        calls_of(&sources, "bus_subscribers::attach_bus_subscribers("),
        vec![(PathBuf::from("lib.rs"), 1)]
    );
    assert_eq!(
        calls_of(&sources, "attach_bridge_behind_notes_index("),
        vec![(PathBuf::from("bus_subscribers.rs"), 1)],
        "a bridge attached outside attach_bus_subscribers is one the store test does not hang"
    );
    assert_eq!(
        calls_of(&sources, "attach_bridge("),
        vec![(PathBuf::from("events").join("bus_bridge.rs"), 1)],
        "a bridge attached outside attach_bus_subscribers is one the store test does not hang"
    );
}

/// A Rust listener runs on the thread that emits its event, which may hold the
/// store; the one the app has listens for an event only the frontend sends.
#[test]
fn the_app_listens_to_itself_only_for_what_the_frontend_sends() {
    let sources = library_sources();
    let listening: Vec<(PathBuf, usize)> = [".listen(", ".listen_any(", ".once(", ".once_any("]
        .into_iter()
        .flat_map(|needle| calls_of(&sources, needle))
        .collect();
    assert_eq!(
        listening,
        vec![(PathBuf::from("lib.rs"), 1)],
        "a listener the store test does not see"
    );
    assert_eq!(
        calls_of(&sources, "\"frontend-ready\""),
        vec![(PathBuf::from("lib.rs"), 1)],
        "the Rust side sends the event its listener waits for"
    );
    let lib = &sources
        .iter()
        .find(|(path, _)| path == Path::new("lib.rs"))
        .expect("lib.rs")
        .1;
    assert!(lib.contains(".listen(\"frontend-ready\","));
}

#[test]
fn reopening_a_tab_whose_file_another_program_changed_answers_and_turns_its_scripts_off() {
    let dir = TempDir::new().expect("temp dir");
    let state = running(&dir);
    let (id, path) = opted_in_note(&state, "a.html", "<p>one</p><script>one()</script>");
    std::fs::write(
        &path,
        "<p>two, from another program</p><script>two()</script>",
    )
    .expect("rewrite the file");

    let reopen_path = path.clone();
    let reopened = on_a_worker(&state, move |state| {
        open_file_from_path(state, &reopen_path).map(|opened| opened.doc.map(|doc| doc.id))
    })
    .expect("the reopen never answered: the store was held across the bus");

    assert_eq!(reopened.expect("the reopen"), Some(id.clone()));
    assert!(
        !row_opted_in(&state, &id),
        "the changed file kept its opt-in"
    );
    assert!(
        !cached_scripts_on(&state, &id),
        "the cached render would still be served with the file's scripts on"
    );
}

#[test]
fn todays_note_on_a_file_another_program_changed_answers_and_turns_its_scripts_off() {
    let dir = TempDir::new().expect("temp dir");
    let state = running(&dir);
    let now = chrono::Utc::now();
    let minted = open_or_mint_dated_note(&state, now).expect("mint today's note");
    let id = minted.opened.doc.expect("the note opened").id;
    shown_with_scripts_on(&state, &id);
    std::fs::write(&minted.path, "changed by a sync from another machine\n")
        .expect("rewrite the file");

    let today = on_a_worker(&state, move |state| {
        open_or_mint_dated_note(state, now).map(|today| today.path)
    })
    .expect("Today's note never answered: the store was held across the bus");

    assert_eq!(today.expect("today's note"), minted.path);
    assert!(!row_opted_in(&state, &id));
    assert!(!cached_scripts_on(&state, &id));
}
