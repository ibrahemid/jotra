//! The config watcher against a real filesystem.
//!
//! Jotra writes `config.toml` the way it writes a note: into a sibling temp
//! file renamed over the original. The rename gives the config a new inode, so
//! a watch bound to the file stops hearing it on backends that watch inodes.
//!
//! Most of these write real files and hand [`ConfigFolderEvents`] the paths
//! the platform reports after such a write, each twice, so what reaches the
//! app is decided without waiting on a watcher. One runs the platform's own
//! watcher, because only that proves a rename over the config, and an edit
//! made after it, still reach Jotra.

use jotra_core::activity::ClientId;
use jotra_core::config::JotraConfig;
use jotra_core::events::bus::{EventBus, JotraEvent};
use jotra_core::watcher::ignore::{config_key, data_key};
use jotra_storage::config_store::ConfigStore;
use jotra_storage::pending_clients;
use jotra_tauri_lib::security::resolve_for_containment;
use jotra_tauri_lib::watcher::handler::{
    create_ignore_set, start_file_watcher, ConfigFolderEvents, IgnoreSet,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// The path the ignore stamps key on. The handler's own `ignore_key_path` is
/// crate-private; this is the same resolution, reached through the function
/// it delegates to.
fn resolved(path: &Path) -> PathBuf {
    resolve_for_containment(path)
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf())
}

/// Writes `bytes` to `path` the way another editor does: into a sibling temp
/// file, then renamed over the target.
fn write_by_temp_and_rename(path: &Path, bytes: &[u8]) {
    let temp = path.with_extension("jotra-test-tmp");
    std::fs::write(&temp, bytes).expect("write temp");
    std::fs::rename(&temp, path).expect("rename over target");
}

/// Writes `config` the way `commands::config::persist_config` does: the
/// serialized bytes are stamped under the config's resolved path before the
/// store writes them.
fn persist_like_the_app(store: &ConfigStore, ignore: &IgnoreSet, config: &JotraConfig) {
    let contents = store.serialize(config).expect("serialize");
    ignore.lock().expect("ignore set").record(
        config_key(&resolved(store.path())),
        contents.as_bytes(),
        Instant::now(),
    );
    store.write_serialized(&contents).expect("config write");
}

/// What the watcher makes of a write in `folder`, as the platform reports it
/// in the folder's resolved spelling: every file there, the temp file the write
/// renamed away, and each a second time, as the echo of the read that
/// classifies it.
fn reported_in(events: &mut ConfigFolderEvents, folder: &Path) -> Vec<JotraEvent> {
    let mut paths = vec![folder.join(".tmpA1b2C3")];
    for entry in std::fs::read_dir(folder).expect("list the folder") {
        paths.push(entry.expect("folder entry").path());
    }
    paths.sort();
    let mut reported = Vec::new();
    for _ in 0..2 {
        for path in &paths {
            reported.extend(events.report(path, Instant::now()));
        }
    }
    reported
}

fn config_changes(events: &[JotraEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, JotraEvent::ConfigChanged { .. }))
        .count()
}

fn pending_changes(events: &[JotraEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, JotraEvent::McpPendingChanged))
        .count()
}

fn pending_beside(config_path: &Path) -> PathBuf {
    pending_clients::path(config_path.parent().expect("a data folder"))
}

fn config_events(config_path: &Path, ignore: &IgnoreSet) -> ConfigFolderEvents {
    ConfigFolderEvents::new(config_path, &pending_beside(config_path), ignore.clone())
        .expect("the config's folder")
}

/// The round trip for a watcher handed `config_path`, whose events arrive under
/// `folder`: a seeded config, Jotra's own stamped write absorbed, and another
/// program's edit after it reported exactly once.
fn assert_own_write_absorbed_and_later_edit_reported(config_path: &Path, folder: &Path) {
    std::fs::write(config_path, b"[editor]\nfont_size = 14\n").expect("seed config");

    let ignore = create_ignore_set();
    let mut events = config_events(config_path, &ignore);
    assert_eq!(
        events.folders()[0],
        folder,
        "the watcher must watch the folder the platform reports the config in"
    );
    // The seed write is not under test.
    reported_in(&mut events, folder);

    let store = ConfigStore::new(config_path.to_path_buf());
    let mut config = JotraConfig::default();
    config.editor.font_size = 20;
    persist_like_the_app(&store, &ignore, &config);
    let echoed = reported_in(&mut events, folder);
    assert_eq!(
        config_changes(&echoed),
        0,
        "Jotra's own config write must not come back as an external change, saw {echoed:?}"
    );

    write_by_temp_and_rename(config_path, b"[editor]\nfont_size = 22\n");
    let seen = reported_in(&mut events, folder);
    assert_eq!(
        config_changes(&seen),
        1,
        "an edit after Jotra's own write must reach the app exactly once, saw {seen:?}"
    );
}

#[test]
fn an_edit_after_jotra_s_own_config_write_still_reaches_the_app() {
    let data = TempDir::new().expect("data dir");
    let folder = resolved(data.path());
    assert_own_write_absorbed_and_later_edit_reported(&data.path().join("config.toml"), &folder);
}

#[cfg(unix)]
#[test]
fn a_config_reached_through_a_symlinked_folder_is_still_followed() {
    // The data folder Jotra is handed can be a link, and the platform reports
    // events under the folder's real path.
    let root = TempDir::new().expect("root");
    let real = root.path().join("real");
    std::fs::create_dir(&real).expect("real folder");
    let linked = root.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).expect("link the folder");

    let folder = real.canonicalize().expect("the real folder");
    assert_own_write_absorbed_and_later_edit_reported(&linked.join("config.toml"), &folder);
}

#[test]
fn a_config_created_after_the_watcher_started_is_followed() {
    let data = TempDir::new().expect("data dir");
    let folder = resolved(data.path());
    let config_path = data.path().join("config.toml");
    let mut events = config_events(&config_path, &create_ignore_set());
    let before = reported_in(&mut events, &folder);
    assert!(
        before.is_empty(),
        "an empty folder has nothing to say, saw {before:?}"
    );

    write_by_temp_and_rename(&config_path, b"[editor]\nfont_size = 22\n");
    let seen = reported_in(&mut events, &folder);
    assert_eq!(
        config_changes(&seen),
        1,
        "a config another program creates must reach the app exactly once, saw {seen:?}"
    );
}

#[test]
fn a_program_the_server_puts_on_the_waiting_list_reaches_the_app() {
    let data = TempDir::new().expect("data dir");
    let folder = resolved(data.path());
    let mut events = config_events(&data.path().join("config.toml"), &create_ignore_set());

    pending_clients::note_calls(data.path(), &ClientId::named("Claude Code"), 1)
        .expect("the server notes a waiting program");

    assert_eq!(
        pending_changes(&reported_in(&mut events, &folder)),
        1,
        "the server's write to the waiting file must reach the app exactly once"
    );
}

#[test]
fn the_app_s_own_write_to_the_waiting_list_does_not_come_back() {
    let data = TempDir::new().expect("data dir");
    let folder = resolved(data.path());
    pending_clients::note_calls(data.path(), &ClientId::named("Claude Code"), 1)
        .expect("seed the waiting file");
    let ignore = create_ignore_set();
    let mut events = config_events(&data.path().join("config.toml"), &ignore);
    reported_in(&mut events, &folder);

    let key = data_key(&resolved(&pending_clients::path(data.path())));
    pending_clients::forget(data.path(), "Claude Code", |bytes| {
        ignore
            .lock()
            .expect("ignore set")
            .record(key, bytes, Instant::now());
    })
    .expect("the app takes the program off the list");

    assert_eq!(
        pending_changes(&reported_in(&mut events, &folder)),
        0,
        "the app's own stamped write must not come back as a change"
    );
}

#[test]
fn a_waiting_file_written_in_place_reaches_the_app_once() {
    let data = TempDir::new().expect("data dir");
    let folder = resolved(data.path());
    pending_clients::note_calls(data.path(), &ClientId::named("Claude Code"), 1)
        .expect("seed the waiting file");
    let mut events = config_events(&data.path().join("config.toml"), &create_ignore_set());
    reported_in(&mut events, &folder);

    std::fs::write(pending_clients::path(data.path()), b"{\"clients\":[]}")
        .expect("rewrite the waiting file in place");

    assert_eq!(
        pending_changes(&reported_in(&mut events, &folder)),
        1,
        "a write in place must reach the app exactly once"
    );
}

/// Long enough for a loaded machine; only a failing run waits it out.
const DELIVERY_BOUND: Duration = Duration::from_secs(15);

/// The one test here that runs the platform's watcher. Delivery lags on a
/// loaded machine, so an edit that has not been reported is made again, each
/// time with new bytes.
#[test]
fn the_platform_watcher_hears_an_edit_after_jotra_s_own_config_write() {
    let data = TempDir::new().expect("data dir");
    let config_path = data.path().join("config.toml");
    std::fs::write(&config_path, b"[editor]\nfont_size = 14\n").expect("seed config");
    let ignore = create_ignore_set();
    let bus = Arc::new(EventBus::new());
    let (tx, rx) = mpsc::channel();
    bus.subscribe(move |event| {
        let _ = tx.send(event.clone());
    });
    let _watcher = start_file_watcher(
        bus,
        config_path.clone(),
        pending_beside(&config_path),
        ignore.clone(),
    )
    .expect("start the config watcher");

    let store = ConfigStore::new(config_path.clone());
    let mut config = JotraConfig::default();
    config.editor.font_size = 20;
    persist_like_the_app(&store, &ignore, &config);
    while rx.try_recv().is_ok() {}

    let heard = (0..4).any(|attempt| {
        let edit = format!("[editor]\nfont_size = {}\n", 22 + attempt * 10);
        write_by_temp_and_rename(&config_path, edit.as_bytes());
        let deadline = Instant::now() + DELIVERY_BOUND;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(JotraEvent::ConfigChanged { .. }) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    });
    assert!(
        heard,
        "an edit after Jotra renamed its own write over the config never reached the app"
    );
}
