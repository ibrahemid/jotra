//! One place that builds an [`AppState`] over a temp data folder for the
//! integration tests, so a new `AppState` field is added here and in
//! `state.rs` and nowhere else. Each test binary includes it and uses the part
//! it needs.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, RwLock};

use jotra_core::config::JotraConfig;
use jotra_core::events::bus::EventBus;
use jotra_core::preview::ContentRendererRegistry;
use jotra_core::update::UpdatePhase;
use jotra_core::watcher::reconcile::ReconcileGate;
use jotra_plugin::transform::TransformRegistry;
use jotra_storage::buffer_store::BufferStore;
use jotra_storage::command_usage::CommandUsageStore;
use jotra_storage::config_store::ConfigStore;
use jotra_storage::database::connection::open_database;
use jotra_storage::database::migrations::run_migrations;
use jotra_storage::layout_state::LayoutStateStore;
use jotra_storage::note_history::NoteHistoryStore;
use jotra_storage::notes_index::NotesIndexStore;
use jotra_storage::tab_rows::TabRowStore;
use jotra_tauri_lib::fts_scheduler::FtsScheduler;
use jotra_tauri_lib::preview::handler::RenderCache;
use jotra_tauri_lib::quit::QuitState;
use jotra_tauri_lib::security::{canonicalize_root, AuthorizedPaths};
use jotra_tauri_lib::state::{AppState, NotesRootFallback};
use jotra_tauri_lib::watcher::handler::create_ignore_set;
use jotra_tauri_lib::workspace_index::WorkspaceIndex;

/// Builds an `AppState` with every field at its default over `jotra_dir`.
/// The notes folder is `<jotra_dir>/Jotra` unless [`notes_dir`] says otherwise.
///
/// [`notes_dir`]: AppStateBuilder::notes_dir
pub struct AppStateBuilder {
    jotra_dir: PathBuf,
    notes_dir: PathBuf,
    notes_root_fallback: Option<NotesRootFallback>,
    workspace_root: Option<PathBuf>,
    store_notes_root: bool,
    shared_notes_index: bool,
    builtin_renderers: bool,
    recorded_history: bool,
}

impl AppStateBuilder {
    pub fn new(jotra_dir: &Path) -> Self {
        Self {
            jotra_dir: jotra_dir.to_path_buf(),
            notes_dir: jotra_dir.join("Jotra"),
            notes_root_fallback: None,
            workspace_root: None,
            store_notes_root: false,
            shared_notes_index: false,
            builtin_renderers: false,
            recorded_history: false,
        }
    }

    /// The notes folder, created and canonicalised at build.
    pub fn notes_dir(mut self, notes_dir: PathBuf) -> Self {
        self.notes_dir = notes_dir;
        self
    }

    pub fn notes_root_fallback(mut self, fallback: Option<NotesRootFallback>) -> Self {
        self.notes_root_fallback = fallback;
        self
    }

    /// The open workspace folder, held by both the state and its file index.
    pub fn workspace_root(mut self, root: Option<PathBuf>) -> Self {
        self.workspace_root = root;
        self
    }

    /// The buffer store knows the notes folder.
    pub fn store_notes_root(mut self) -> Self {
        self.store_notes_root = true;
        self
    }

    /// The buffer store and the state share one notes index, and the store
    /// knows the notes folder.
    pub fn shared_notes_index(mut self) -> Self {
        self.store_notes_root = true;
        self.shared_notes_index = true;
        self
    }

    /// The preview registry holds the built-in renderers over the shared notes
    /// index.
    pub fn builtin_renderers(mut self) -> Self {
        self.shared_notes_index = true;
        self.store_notes_root = true;
        self.builtin_renderers = true;
        self
    }

    /// The version store and the buffer store record note history: the version
    /// store knows the notes folder and a platform identity probe, and the
    /// buffer store writes through it.
    pub fn recorded_history(mut self) -> Self {
        self.store_notes_root = true;
        self.recorded_history = true;
        self
    }

    pub fn build(self) -> AppState {
        let jotra_dir = self.jotra_dir;
        let buffers_dir = jotra_dir.join("buffers");
        std::fs::create_dir_all(&buffers_dir).expect("buffers dir");

        std::fs::create_dir_all(&self.notes_dir).expect("notes folder");
        let notes_root = canonicalize_root(&self.notes_dir).expect("canonical");

        let db_path = jotra_dir.join("jotra.db");
        let conn = open_database(&db_path).expect("open db");
        run_migrations(&conn).expect("migrations");

        let note_history = Arc::new(NoteHistoryStore::open(&jotra_dir).expect("version store"));
        if self.recorded_history {
            note_history.set_notes_root(notes_root.clone());
            note_history.set_probe(Arc::new(
                jotra_tauri_lib::watcher::identity::PlatformIdentity,
            ));
        }

        let notes_index = Arc::new(NotesIndexStore::open(&db_path).expect("notes index db"));

        let mut store = BufferStore::new(conn, buffers_dir.clone());
        if self.store_notes_root {
            store.set_notes_root(notes_root.clone());
        }
        if self.shared_notes_index {
            store.set_notes_index(Arc::clone(&notes_index));
        }
        if self.recorded_history {
            store.set_history(Arc::clone(&note_history));
        }

        let mut registry = ContentRendererRegistry::new();
        if self.builtin_renderers {
            jotra_tauri_lib::preview::renderers::register_builtins(
                &mut registry,
                Arc::clone(&notes_index),
            )
            .expect("renderers");
        }

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
            notes_root_fallback: RwLock::new(self.notes_root_fallback),
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
            preview_registry: Arc::new(RwLock::new(registry)),
            preview_render_cache: Arc::new(RenderCache::new()),
            layout_state: LayoutStateStore::new(open_database(&db_path).expect("layout db")),
            tab_rows: TabRowStore::new(open_database(&db_path).expect("tab rows db")),
            command_usage: CommandUsageStore::new(open_database(&db_path).expect("usage db")),
            config_diagnostics: Mutex::new(Vec::new()),
            recovered_buffers: Mutex::new(Vec::new()),
            was_dirty_shutdown: false,
            workspace_root: Mutex::new(self.workspace_root.clone()),
            workspace_watcher: Mutex::new(None),
            inbox_root: Mutex::new(None),
            inbox_watcher: Mutex::new(None),
            fts_scheduler: FtsScheduler::new(),
            workspace_index: Arc::new(RwLock::new(WorkspaceIndex::new(self.workspace_root))),
            search_generation: Arc::new(AtomicU64::new(0)),
            last_disk_hash: Mutex::new(std::collections::HashMap::new()),
            source_records: Mutex::new(std::collections::HashMap::new()),
            buffer_writes: Default::default(),
        }
    }
}
