//! The work ADR-047 moved off the main thread, as the functions behind the
//! commands do it: startup maintenance after the window shows, the note
//! search, closes and chat edits that wait for their notes' turns, and a
//! rename that reads its file with the store free.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use jotra_core::config::JotraConfig;
use jotra_core::events::bus::EventBus;
use jotra_core::preview::ContentRendererRegistry;
use jotra_core::update::UpdatePhase;
use jotra_core::watcher::reconcile::ReconcileGate;
use jotra_plugin::transform::TransformRegistry;
use jotra_storage::buffer_store::BufferStore;
use jotra_storage::config_store::ConfigStore;
use jotra_storage::database::connection::{
    open_database, open_database_for_maintenance, yielding_to_writes,
};
use jotra_storage::database::migrations::run_migrations;
use jotra_storage::layout_state::LayoutStateStore;
use jotra_storage::notes_index::NotesIndexStore;
use jotra_tauri_lib::commands::buffer::{
    close_buffers_inner, get_buffer, record_unsaved_notes, record_unsaved_notes_inner,
    reindex_settled_buffer, save_buffer_content_inner, save_in_turn, UnsavedNote,
};
use jotra_tauri_lib::commands::chat::apply_proposal_to_state;
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::commands::history::search_buffers_inner;
use jotra_tauri_lib::preview::handler::RenderCache;
use jotra_tauri_lib::quit::QuitState;
use jotra_tauri_lib::security::{canonicalize_for_authorization, AuthorizedPaths};
use jotra_tauri_lib::state::AppState;
use jotra_tauri_lib::watcher::handler::create_ignore_set;
use tauri::test::{mock_builder, mock_context, noop_assets};
use tauri::Manager;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    let jotra_dir = dir.path().join("data");
    let buffers_dir = jotra_dir.join("buffers");
    std::fs::create_dir_all(&buffers_dir).expect("buffers dir");
    let notes_root = dir.path().join("Jotra");
    std::fs::create_dir_all(&notes_root).expect("notes folder");
    let notes_root = jotra_tauri_lib::security::canonicalize_root(&notes_root).expect("canonical");

    let db_path = jotra_dir.join("jotra.db");
    let conn = open_database(&db_path).expect("open db");
    run_migrations(&conn).expect("migrations");
    let notes_index = Arc::new(NotesIndexStore::open(&db_path).expect("notes index db"));
    let mut store = BufferStore::new(conn, buffers_dir.clone());
    store.set_notes_root(notes_root.clone());
    store.set_notes_index(Arc::clone(&notes_index));
    let note_history = Arc::new(
        jotra_storage::note_history::NoteHistoryStore::open(&jotra_dir).expect("version store"),
    );

    AppState {
        store: Mutex::new(store),
        config_store: ConfigStore::new(jotra_dir.join("config.toml")),
        config: Mutex::new(JotraConfig::default()),
        jotra_dir,
        buffers_dir,
        notes_root: RwLock::new(notes_root),
        first_run: false,
        first_run_finished: AtomicBool::new(false),
        menu_apps: Mutex::new(None),
        retitle_watch: Arc::new(jotra_tauri_lib::first_run::RetitleWatch::new()),
        notes_root_fallback: RwLock::new(None),
        watcher_ignore: create_ignore_set(),
        watcher: Mutex::new(None),
        notes_watcher: Mutex::new(None),
        open_file_watcher: Mutex::new(None),
        file_tracking: Mutex::new(None),
        notes_index,
        note_history,
        notes_index_cancel: Arc::new(AtomicBool::new(false)),
        notes_reconcile: Arc::new(ReconcileGate::new()),
        quit: Arc::new(QuitState::new()),
        removal_holds: Default::default(),
        pending_opens: Mutex::new(Vec::new()),
        pending_lines: Mutex::new(Default::default()),
        cli_waits: Mutex::new(Default::default()),
        frontend_ready: AtomicBool::new(false),
        window_revealed: AtomicBool::new(false),
        window_dismissed: AtomicBool::new(false),
        transforms: RwLock::new(TransformRegistry::new()),
        event_bus: Arc::new(EventBus::new()),
        update_phase: Mutex::new(UpdatePhase::default()),
        authorized_paths: AuthorizedPaths::new(),
        preview_registry: Arc::new(RwLock::new(ContentRendererRegistry::new())),
        preview_render_cache: Arc::new(RenderCache::new()),
        layout_state: LayoutStateStore::new(open_database(&db_path).expect("layout db")),
        command_usage: jotra_storage::command_usage::CommandUsageStore::new(
            open_database(&db_path).expect("usage db"),
        ),
        config_diagnostics: Mutex::new(Vec::new()),
        recovered_buffers: Mutex::new(Vec::new()),
        was_dirty_shutdown: false,
        workspace_root: Mutex::new(None),
        workspace_watcher: Mutex::new(None),
        inbox_root: Mutex::new(None),
        inbox_watcher: Mutex::new(None),
        fts_scheduler: jotra_tauri_lib::fts_scheduler::FtsScheduler::new(),
        workspace_index: Arc::new(RwLock::new(
            jotra_tauri_lib::workspace_index::WorkspaceIndex::new(None),
        )),
        search_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        last_disk_hash: Mutex::new(std::collections::HashMap::new()),
        source_records: Mutex::new(std::collections::HashMap::new()),
        buffer_writes: Default::default(),
    }
}

fn open(state: &AppState, path: &std::path::Path) -> String {
    let canonical = canonicalize_for_authorization(path).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("a row")
        .id
}

#[test]
fn the_wait_for_the_window_ends_when_it_is_revealed() {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(make_state(&dir));
    let revealer = Arc::clone(&state);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        revealer.window_revealed.store(true, Ordering::SeqCst);
    });
    assert!(state.wait_for_reveal(Duration::from_secs(10)));
}

#[test]
fn the_wait_for_a_window_nothing_shows_gives_up_at_its_limit() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    assert!(!state.wait_for_reveal(Duration::from_millis(60)));
}

/// Leaves the state's database mostly free pages, which a startup `VACUUM`
/// reclaims.
fn fill_with_free_pages(state: &AppState) {
    let side = open_database(&state.jotra_dir.join("jotra.db")).expect("db");
    side.execute_batch(
        "CREATE TABLE bloat (body BLOB);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2400)
         INSERT INTO bloat SELECT randomblob(4096) FROM n;
         DROP TABLE bloat;
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .expect("bloat");
}

#[test]
fn startup_maintenance_vacuums_a_database_that_is_mostly_free_pages() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    fill_with_free_pages(&state);
    let before = state.store.lock().unwrap().database_stats().expect("stats");
    assert!(before.freelist_count > jotra_core::maintenance::VACUUM_MIN_FREE_PAGES);

    state.run_startup_maintenance();
    let after = state.store.lock().unwrap().database_stats().expect("stats");
    assert_eq!(after.freelist_count, 0);
    assert!(after.page_count < before.page_count);
}

#[test]
fn startup_maintenance_leaves_the_store_free_while_it_runs() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    fill_with_free_pages(&state);

    std::thread::scope(|scope| {
        let maintenance = state.notes_index.holding_writes(|| {
            let maintenance = scope.spawn(|| state.run_startup_maintenance());
            std::thread::sleep(Duration::from_millis(300));
            assert!(
                !maintenance.is_finished(),
                "it waits for the index's writer"
            );
            assert!(
                state.store.try_lock().is_ok(),
                "the store is held while the maintenance waits to run"
            );
            maintenance
        });
        maintenance.join().expect("the maintenance ran");
    });
    let after = state.store.lock().unwrap().database_stats().expect("stats");
    assert_eq!(after.freelist_count, 0);
}

/// A write that holds the database's write lock for seconds, as a `VACUUM`
/// of a large file does, until it is cut short.
const LONG_WRITE: &str = "INSERT INTO held SELECT count(*) FROM (
     WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 30000000)
     SELECT i FROM n)";

/// Waits until another connection holds the write lock of the database at
/// `path`.
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

#[test]
fn a_row_write_under_the_store_cuts_short_the_maintenance_it_meets() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let path = state.notes_root().join("Plan.md");
    std::fs::write(&path, "# Plan\n").unwrap();
    let id = open(&state, &path);
    let database = state.jotra_dir.join("jotra.db");
    let side = open_database_for_maintenance(&database).expect("maintenance connection");
    side.execute_batch("CREATE TABLE held (n INTEGER)")
        .expect("table");

    std::thread::scope(|scope| {
        let work =
            scope.spawn(move || yielding_to_writes(&side, || side.execute_batch(LONG_WRITE)));
        wait_until_the_write_lock_is_held(&database);
        let started = Instant::now();
        record_unsaved_notes_inner(
            &state,
            vec![UnsavedNote {
                id: id.clone(),
                content: "# Plan\n\nNot saved yet.\n".into(),
            }],
        )
        .expect("journaled");
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(2),
            "the write held the store for {} ms while the maintenance ran",
            waited.as_millis()
        );
        assert!(
            work.join().expect("the long write ran").is_err(),
            "the write cut the long write short"
        );
    });
}

/// How long the startup maintenance is held at its `VACUUM` at most.
const VACUUM_HELD_FOR: Duration = Duration::from_secs(6);

fn index_hits(state: &AppState, word: &str) -> usize {
    let query = jotra_core::search::to_prefix_match(word).expect("query");
    let terms = jotra_core::search::search_terms(word);
    state
        .notes_index
        .search_hits(&query, &terms, 10)
        .expect("search")
        .len()
}

#[test]
fn an_edit_during_the_startup_vacuum_leaves_the_store_free_and_reaches_the_index_after() {
    let dir = TempDir::new().unwrap();
    let app = mock_builder()
        .manage(make_state(&dir))
        .build(mock_context(noop_assets()))
        .expect("mock app");
    let state: &AppState = app.state::<AppState>().inner();
    let path = state.notes_root().join("Plan.md");
    std::fs::write(&path, "# Plan\n").unwrap();
    let id = open(state, &path);
    fill_with_free_pages(state);
    let bloated = state.store.lock().unwrap().database_stats().expect("stats");
    let (reached_tx, reached_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    std::thread::scope(|scope| {
        let maintenance = scope.spawn(move || {
            jotra_storage::maintenance::before_next_vacuum(move || {
                let _ = reached_tx.send(());
                let _ = release_rx.recv_timeout(VACUUM_HELD_FOR);
            });
            state.run_startup_maintenance();
        });
        reached_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the maintenance reached its VACUUM");

        save_buffer_content_inner(state, &id, "# Plan\n\nkestrel\n").expect("the save lands");
        let reindex = scope.spawn(|| reindex_settled_buffer(state, &id));
        let settled = Instant::now() + Duration::from_millis(500);
        while !reindex.is_finished() && Instant::now() < settled {
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        get_buffer(app.state(), id.clone()).expect("the row");
        let read = started.elapsed();
        let started = Instant::now();
        record_unsaved_notes(
            app.state(),
            vec![UnsavedNote {
                id: id.clone(),
                content: "# Plan\n\nkestrel, not saved yet\n".into(),
            }],
        )
        .expect("journaled");
        let journaled = started.elapsed();
        let indexed_during = index_hits(state, "kestrel");
        let _ = release_tx.send(());
        maintenance.join().expect("the maintenance ran");
        reindex.join().expect("the reindex ran");

        assert!(
            read < Duration::from_secs(2),
            "get_buffer waited {} ms for the store during the VACUUM",
            read.as_millis()
        );
        assert!(
            journaled < Duration::from_secs(2),
            "record_unsaved_notes waited {} ms for the store during the VACUUM",
            journaled.as_millis()
        );
        assert_eq!(indexed_during, 0, "the index was written during the VACUUM");
    });
    let after = state.store.lock().unwrap().database_stats().expect("stats");
    assert!(after.page_count < bloated.page_count, "the VACUUM ran");
    assert_eq!(
        index_hits(state, "kestrel"),
        1,
        "the edit reached the index once the VACUUM ended"
    );
}

#[test]
fn the_note_search_names_the_tab_that_shows_a_hit() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let path = state.notes_root().join("Plan.md");
    std::fs::write(&path, "# Plan\n\nThe launch checklist.\n").unwrap();
    state
        .notes_index
        .reconcile(&state.notes_root(), &|| false, &|_| false)
        .expect("reconcile");
    let id = open(&state, &path);

    let found = search_buffers_inner(&state, "checkl").expect("search");
    assert_eq!(found.total, 1);
    assert_eq!(found.hits[0].buffer_id, id);
    assert_eq!(search_buffers_inner(&state, "").expect("empty").total, 0);
}

#[test]
fn the_note_search_finds_text_saved_a_moment_ago() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let path = state.notes_root().join("Plan.md");
    std::fs::write(&path, "# Plan\n\nThe launch checklist.\n").unwrap();
    state
        .notes_index
        .reconcile(&state.notes_root(), &|| false, &|_| false)
        .expect("reconcile");
    let id = open(&state, &path);

    // What the save IPC does: the write lands and the reindex is deferred to
    // the end of a debounce window that has not passed yet.
    save_buffer_content_inner(&state, &id, "# Plan\n\nThe launch checklist.\nmarzipan\n")
        .expect("the save lands");
    assert!(state.fts_scheduler.on_edit(&id).is_some());

    let found = search_buffers_inner(&state, "marzip").expect("search");
    assert_eq!(found.total, 1, "the text saved a moment ago is searchable");
    assert_eq!(found.hits[0].buffer_id, id);
    assert!(state.fts_scheduler.take_pending().is_empty());
}

#[test]
fn a_search_reads_a_note_saved_a_moment_ago_after_its_write_lands() {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(make_state(&dir));
    let path = state.notes_root().join("Plan.md");
    std::fs::write(&path, "# Plan\n").unwrap();
    state
        .notes_index
        .reconcile(&state.notes_root(), &|| false, &|_| false)
        .expect("reconcile");
    let id = open(&state, &path);
    save_buffer_content_inner(&state, &id, "# Plan\nmarzipan\n").expect("the save lands");
    assert!(state.fts_scheduler.on_edit(&id).is_some());

    let held = state.buffer_writes.turn(&id);
    let (done, searched) = mpsc::channel();
    let searcher = Arc::clone(&state);
    std::thread::spawn(move || {
        done.send(search_buffers_inner(&searcher, "marzip"))
            .unwrap();
    });
    assert!(
        searched.recv_timeout(Duration::from_millis(100)).is_err(),
        "the search read the note while a write of it held the turn"
    );
    drop(held);
    let found = searched
        .recv_timeout(Duration::from_secs(5))
        .expect("the search finished")
        .expect("search");
    assert_eq!(found.total, 1);
    assert_eq!(state.buffer_writes.outstanding(&id), 0);
}

#[test]
fn a_thread_that_panicked_holding_the_store_leaves_saves_and_closes_working() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let path = dir.path().join("a.md");
    std::fs::write(&path, "before\n").unwrap();
    let id = open(&state, &path);

    std::thread::scope(|scope| {
        let panicked = scope
            .spawn(|| {
                let _store = state.store.lock();
                panic!("a worker failed while it held the store");
            })
            .join();
        assert!(panicked.is_err());
    });
    assert!(state.store.is_poisoned());

    save_buffer_content_inner(&state, &id, "after\n").expect("the save lands");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
    assert!(!state.store.is_poisoned());
    close_buffers_inner(&state, std::slice::from_ref(&id)).expect("the close lands");
}

#[test]
fn a_close_waits_for_its_notes_turns_and_takes_each_once() {
    let dir = TempDir::new().unwrap();
    let state = Arc::new(make_state(&dir));
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.md");
    std::fs::write(&a, "a\n").unwrap();
    std::fs::write(&b, "b\n").unwrap();
    let a = open(&state, &a);
    let b = open(&state, &b);

    let held = state.buffer_writes.turn(&b);
    let (done, closed) = mpsc::channel();
    let closer = Arc::clone(&state);
    let ids = vec![b.clone(), a.clone(), b.clone()];
    std::thread::spawn(move || {
        done.send(close_buffers_inner(&closer, &ids)).unwrap();
    });
    assert!(
        closed.recv_timeout(Duration::from_millis(100)).is_err(),
        "the close ran while a write of one of its notes held the turn"
    );
    drop(held);
    closed
        .recv_timeout(Duration::from_secs(5))
        .expect("the close finished")
        .expect("closed");
    assert_eq!(state.buffer_writes.outstanding(&a), 0);
    assert_eq!(state.buffer_writes.outstanding(&b), 0);
}

#[test]
fn a_chat_edit_of_an_open_file_checks_it_after_a_save_still_landing() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = dir.path().join("Plan.md");
    std::fs::write(&file, "X\n").unwrap();
    let file = std::path::PathBuf::from(canonicalize_for_authorization(&file).expect("canonical"));
    let id = open(&state, &file);
    let offered_against = jotra_core::hash::sha256_hex(b"X\n");

    let landing = state.buffer_writes.turn(&id);
    std::thread::scope(|scope| {
        let applying = scope.spawn(|| {
            apply_proposal_to_state(
                &state,
                "test-host",
                &file.to_string_lossy(),
                "AI\n",
                &offered_against,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.buffer_writes.outstanding(&id) < 2 && !applying.is_finished() {
            assert!(Instant::now() < deadline, "the edit never queued");
            std::thread::sleep(Duration::from_millis(1));
        }
        save_in_turn(&state, &landing, "Y\n").expect("the save lands");
        drop(landing);
        let refused = applying
            .join()
            .expect("edit thread")
            .expect_err("the edit was checked against the file before the save landed");
        assert!(
            refused.contains("changed since this was proposed"),
            "{refused}"
        );
    });
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "Y\n");
}

#[test]
fn a_rename_that_waited_for_the_store_is_stamped_after_the_wait() {
    use jotra_core::watcher::ignore::{source_key, SuppressDecision};
    use jotra_tauri_lib::commands::notes::rename_note_inner;

    let dir = TempDir::new().unwrap();
    let state = Arc::new(make_state(&dir));
    let file = dir.path().join("stamped.log");
    std::fs::write(&file, "X\n").unwrap();
    let file = std::path::PathBuf::from(canonicalize_for_authorization(&file).expect("canonical"));
    let id = open(&state, &file);

    let ignore = state.watcher_ignore.lock().unwrap();
    let renamer = Arc::clone(&state);
    let renamed_id = id.clone();
    let renaming = std::thread::spawn(move || rename_note_inner(&renamer, &renamed_id, "renamed"));
    // The rename reads the file and blocks on its stamps.
    std::thread::sleep(Duration::from_millis(200));
    let store = state.store.lock().unwrap();
    drop(ignore);
    std::thread::sleep(Duration::from_millis(600));
    drop(store);
    let renamed = renaming
        .join()
        .expect("rename thread")
        .expect("the rename lands");

    let to = file.with_file_name("renamed.log");
    assert_eq!(renamed.source_path.as_deref(), to.to_str());
    let decision = state.watcher_ignore.lock().unwrap().decide(
        &source_key(&to),
        Some(b"X\n"),
        Instant::now(),
        Duration::from_millis(300),
    );
    assert_eq!(
        decision,
        SuppressDecision::Suppress,
        "the new name's stamp aged out while the rename waited for the store"
    );
}

#[cfg(unix)]
#[test]
fn a_rename_reads_its_file_with_the_store_free() {
    use jotra_tauri_lib::commands::notes::rename_note_inner;
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let state = Arc::new(make_state(&dir));
    let file = dir.path().join("big.log");
    std::fs::write(&file, "X\n").unwrap();
    let file = std::path::PathBuf::from(canonicalize_for_authorization(&file).expect("canonical"));
    let id = open(&state, &file);
    std::fs::remove_file(&file).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(&file)
        .status()
        .expect("mkfifo");
    assert!(made.success(), "mkfifo failed");

    let (reading, read_started) = mpsc::channel();
    let (go_on, let_go) = mpsc::channel::<()>();
    let pipe = file.clone();
    std::thread::spawn(move || {
        // Fed once: a second read of the file waits for a writer that never comes.
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&pipe)
            .expect("open the pipe");
        reading.send(()).unwrap();
        let_go.recv().unwrap();
        writer.write_all(b"X\n").expect("feed the pipe");
    });
    let (done, renamed) = mpsc::channel();
    let renamer = Arc::clone(&state);
    let renamed_id = id.clone();
    std::thread::spawn(move || {
        done.send(rename_note_inner(&renamer, &renamed_id, "renamed"))
            .unwrap();
    });

    read_started
        .recv_timeout(Duration::from_secs(10))
        .expect("the rename read its file");
    let store_was_free = state.store.try_lock().is_ok();
    go_on.send(()).unwrap();
    let renamed = renamed
        .recv_timeout(Duration::from_secs(10))
        .expect("the rename finished");
    assert!(
        store_was_free,
        "the rename held the store while it read the file"
    );
    let renamed = renamed.expect("renamed");
    assert_eq!(
        renamed.source_path.as_deref(),
        Some(
            file.with_file_name("renamed.log")
                .to_string_lossy()
                .as_ref()
        )
    );
}
