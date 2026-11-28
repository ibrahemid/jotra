#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use jotra_core::events::bus::JotraEvent;
use jotra_tauri_lib::commands::notes::move_notes_folder_to;
use jotra_tauri_lib::commands::workspace::{
    clear_workspace_root_inner, list_workspace_dir_inner, set_workspace_root_from_path,
    workspace_reveal_target, ERR_NOT_IN_FOLDER,
};
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

#[test]
fn set_root_canonicalizes_persists_and_starts_watcher() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();

    let root = set_workspace_root_from_path(&state, ws.path()).expect("set root");

    assert_eq!(
        root,
        jotra_tauri_lib::security::canonicalize_root(ws.path())
            .unwrap()
            .to_string_lossy()
    );
    assert!(state.workspace_watcher.lock().unwrap().is_some());
    assert_eq!(
        state.config.lock().unwrap().workspace.root.as_deref(),
        Some(root.as_str())
    );

    let persisted = state.config_store.read().expect("read config back");
    assert_eq!(persisted.workspace.root.as_deref(), Some(root.as_str()));
}

#[test]
fn set_root_rejects_file_path() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    let file = ws.path().join("a.txt");
    std::fs::write(&file, "x").unwrap();

    assert!(set_workspace_root_from_path(&state, &file).is_err());
}

#[test]
fn set_root_rejects_missing_path() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    assert!(
        set_workspace_root_from_path(&state, std::path::Path::new("/nonexistent/jotra-ws"))
            .is_err()
    );
}

#[test]
fn clear_root_drops_state_watcher_and_config() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();

    set_workspace_root_from_path(&state, ws.path()).expect("set root");
    clear_workspace_root_inner(&state).expect("clear root");

    assert!(state.workspace_root.lock().unwrap().is_none());
    assert!(state.workspace_watcher.lock().unwrap().is_none());
    assert!(state.config.lock().unwrap().workspace.root.is_none());

    let persisted = state.config_store.read().expect("read config back");
    assert!(persisted.workspace.root.is_none());
}

#[test]
fn list_dir_without_root_errors() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    assert!(list_workspace_dir_inner(&state, "/tmp").is_err());
}

#[test]
fn list_dir_inside_root_returns_sorted_entries() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    std::fs::create_dir(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("a.md"), "x").unwrap();
    std::fs::create_dir(ws.path().join("node_modules")).unwrap();

    let root = set_workspace_root_from_path(&state, ws.path()).expect("set root");

    let entries = list_workspace_dir_inner(&state, &root)
        .expect("list")
        .entries;
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["src", "a.md"]);
}

#[test]
fn a_listed_entry_carries_the_conflict_flag_over_the_wire() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    std::fs::write(ws.path().join("a.md"), "x").unwrap();
    std::fs::write(
        ws.path().join("a.sync-conflict-20260822-120000-ABCD.md"),
        "x",
    )
    .unwrap();
    std::fs::write(ws.path().join(".a.md.icloud"), "").unwrap();

    let root = set_workspace_root_from_path(&state, ws.path()).expect("set root");
    let entries = list_workspace_dir_inner(&state, &root)
        .expect("list")
        .entries;

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["a.md", "a.sync-conflict-20260822-120000-ABCD.md"]);

    let payload = serde_json::to_value(&entries).expect("serialize");
    assert_eq!(payload[0]["conflict_copy"], serde_json::Value::Null);
    assert_eq!(payload[1]["conflict_copy"], "sync_client");
}

#[test]
fn list_dir_outside_root_is_rejected() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();

    set_workspace_root_from_path(&state, ws.path()).expect("set root");

    assert!(list_workspace_dir_inner(&state, &outside.path().to_string_lossy()).is_err());
}

#[test]
fn a_reveal_names_an_entry_of_the_open_folder_and_nothing_else() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::create_dir(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("src").join("main.rs"), "").unwrap();
    std::fs::write(outside.path().join("secret.txt"), "").unwrap();

    assert_eq!(
        workspace_reveal_target(&state, &ws.path().join("src").to_string_lossy()),
        Err(ERR_NOT_IN_FOLDER.to_string()),
        "no folder is open yet"
    );

    let root = set_workspace_root_from_path(&state, ws.path()).expect("set root");
    let (folder, folder_is_dir) =
        workspace_reveal_target(&state, &format!("{root}/src")).expect("a folder in it");
    assert!(folder_is_dir);
    assert!(folder.ends_with("src"));
    let (file, file_is_dir) =
        workspace_reveal_target(&state, &format!("{root}/src/main.rs")).expect("a file in it");
    assert!(!file_is_dir);
    assert!(file.ends_with("main.rs"));

    for refused in [
        outside
            .path()
            .join("secret.txt")
            .to_string_lossy()
            .into_owned(),
        format!("{root}/../"),
    ] {
        assert_eq!(
            workspace_reveal_target(&state, &refused),
            Err(ERR_NOT_IN_FOLDER.to_string()),
            "{refused} is not in the open folder"
        );
    }
}

/// Long enough for a loaded machine; only a failing run waits it out.
const DELIVERY_BOUND: Duration = Duration::from_secs(15);

/// What the workspace watcher says on the bus, in order: each changed path,
/// and `swept:<root>` for a sweep.
fn workspace_reports(state: &AppState) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    state.event_bus.subscribe(move |event| match event {
        JotraEvent::WorkspaceChanged { path, .. } => {
            let _ = tx.send(path.clone());
        }
        JotraEvent::WorkspaceSwept { root, .. } => {
            let _ = tx.send(format!("swept:{root}"));
        }
        _ => {}
    });
    rx
}

/// The reports received up to and including the first one `stop` accepts, or
/// `None` when none came within [`DELIVERY_BOUND`].
fn reports_through(
    rx: &mpsc::Receiver<String>,
    stop: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let deadline = Instant::now() + DELIVERY_BOUND;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let report = rx.recv_timeout(left).ok()?;
        let done = stop(&report);
        seen.push(report);
        if done {
            return Some(seen);
        }
    }
    None
}

/// Hands the workspace watcher `paths` as one delivery, as the platform would
/// after its debounce.
fn deliver_to_workspace(state: &AppState, paths: &[PathBuf]) {
    state
        .workspace_watcher
        .lock()
        .unwrap()
        .as_ref()
        .expect("the workspace watcher")
        .deliver(paths)
        .expect("the workspace watcher takes the delivery");
}

/// The one test here that runs the platform's watcher. Delivery lags on a
/// loaded machine, so a file that has not been reported is followed by
/// another, each under a new name.
#[test]
fn the_platform_watcher_reports_a_new_file_in_the_workspace() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    let rx = workspace_reports(&state);

    set_workspace_root_from_path(&state, ws.path()).expect("set root");

    let heard = (0..4).any(|attempt| {
        let name = format!("fresh-{attempt}.md");
        std::fs::write(ws.path().join(&name), "hello").unwrap();
        reports_through(&rx, |report| report.ends_with(&name)).is_some()
    });
    assert!(heard, "a file written into the workspace never arrived");
}

/// The notes folder opened as the workspace lists what the notes walk reads:
/// a note under `vendor/` is in the tree as it is in search and backlinks, and
/// only what another tool keeps for itself is left out (RC-D4).
#[test]
fn the_notes_folder_as_the_workspace_lists_the_notes_under_vendor() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let notes = state.notes_root();
    std::fs::create_dir_all(notes.join("vendor")).unwrap();
    std::fs::write(notes.join("vendor").join("Acme.md"), "supplier").unwrap();
    std::fs::create_dir(notes.join("build")).unwrap();
    std::fs::create_dir(notes.join(".obsidian")).unwrap();
    std::fs::write(notes.join("Home.md"), "see [[Acme]]").unwrap();

    let root = set_workspace_root_from_path(&state, &notes).expect("set root");

    let entries = list_workspace_dir_inner(&state, &root)
        .expect("list")
        .entries;
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["build", "vendor", "Home.md"]);
    let vendor = &entries[1].path;
    let entries = list_workspace_dir_inner(&state, vendor)
        .expect("list vendor")
        .entries;
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Acme.md"]);
}

/// Any other folder opened as the workspace keeps the code-workspace list.
#[test]
fn a_code_workspace_still_hides_vendor() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    std::fs::create_dir(ws.path().join("vendor")).unwrap();
    std::fs::write(ws.path().join("vendor").join("Acme.md"), "supplier").unwrap();
    std::fs::write(ws.path().join("main.rs"), "fn main() {}").unwrap();

    let root = set_workspace_root_from_path(&state, ws.path()).expect("set root");

    let entries = list_workspace_dir_inner(&state, &root)
        .expect("list")
        .entries;
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["main.rs"]);
}

/// A change to `vendor/Acme.md` delivered with one to `marker.md` beside it,
/// and what the watcher said up to the marker, which every kind of folder
/// reports.
fn reports_for_a_vendor_note(state: &AppState, root: &Path) -> Vec<String> {
    let rx = workspace_reports(state);
    let note = root.join("vendor").join("Acme.md");
    let marker = root.join("marker.md");
    std::fs::write(&note, "supplier").unwrap();
    std::fs::write(&marker, "written last").unwrap();
    deliver_to_workspace(state, &[note, marker]);
    reports_through(&rx, |report| report.ends_with("marker.md")).expect("the marker arrived")
}

/// The tree of the notes folder opened as the workspace hears about a note
/// written under `vendor/`, so the folder it lists stays current.
#[test]
fn the_notes_folder_as_the_workspace_hears_a_note_under_vendor() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let notes = state.notes_root();
    std::fs::create_dir_all(notes.join("vendor")).unwrap();

    let root = set_workspace_root_from_path(&state, &notes).expect("set root");

    let seen = reports_for_a_vendor_note(&state, Path::new(&root));
    assert!(
        seen.iter().any(|p| p.ends_with("Acme.md")),
        "the change under vendor/ never reached the tree: {seen:?}"
    );
}

/// When the notes folder moves into the folder open as the workspace, that
/// folder's tree lists and hears what the notes walk reads from then on.
#[test]
fn the_workspace_the_notes_folder_moves_into_lists_and_hears_vendor() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    std::fs::create_dir(ws.path().join("vendor")).unwrap();

    let root = set_workspace_root_from_path(&state, ws.path()).expect("set root");
    let names = |state: &AppState| -> Vec<String> {
        list_workspace_dir_inner(state, &root)
            .expect("list")
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect()
    };
    assert!(names(&state).is_empty(), "a code workspace hides vendor/");

    move_notes_folder_to(&state, Path::new(&root)).expect("move");
    assert_eq!(names(&state), ["vendor"]);

    let seen = reports_for_a_vendor_note(&state, Path::new(&root));
    assert!(
        seen.iter().any(|p| p.ends_with("Acme.md")),
        "the change under vendor/ never reached the tree after the move: {seen:?}"
    );
}

/// An install into `.venv` inside an opened code folder reaches nothing: no
/// event for any of its files, while a file delivered beside it afterwards
/// still arrives, which shows the watcher was listening the whole time (TS-12).
#[test]
fn an_install_into_a_venv_inside_the_workspace_raises_no_event() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let ws = TempDir::new().unwrap();
    let rx = workspace_reports(&state);

    let root = PathBuf::from(set_workspace_root_from_path(&state, ws.path()).expect("set root"));

    let site = root.join(".venv/lib/python3.12/site-packages");
    let install: Vec<PathBuf> = (0..40)
        .flat_map(|package| {
            let folder = site.join(format!("pkg{package}"));
            (0..10).map(move |module| folder.join(format!("m{module}.py")))
        })
        .collect();
    deliver_to_workspace(&state, &install);
    std::fs::write(root.join("after.md"), "written last").unwrap();
    deliver_to_workspace(&state, &[root.join("after.md")]);

    let from_the_install = |report: &str| report.contains(".venv") || report.starts_with("swept:");
    let seen = reports_through(&rx, |report| {
        report.ends_with("after.md") || from_the_install(report)
    })
    .expect("the file delivered after the install arrived");
    let from_the_install: Vec<&String> = seen.iter().filter(|p| from_the_install(p)).collect();
    assert!(from_the_install.is_empty(), "{from_the_install:?}");
}
