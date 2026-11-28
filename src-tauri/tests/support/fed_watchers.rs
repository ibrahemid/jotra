//! The open-file and notes watchers installed on an app's own state the way
//! startup installs them, with no platform watcher behind either. The registry
//! is the app's, over backends that record the folders it asks for, and each
//! delivery is the one the platform makes after a change, handed only to the
//! watcher that would be covering the folder. Nothing waits on a debounce.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use jotra_core::events::bus::JotraEvent;
use jotra_tauri_lib::security::resolve_for_containment;
use jotra_tauri_lib::state::AppState;
use jotra_tauri_lib::watcher::handler::NotesDeliveries;
use jotra_tauri_lib::watcher::moves::FileTracking;
use jotra_tauri_lib::watcher::open_files::{
    DirWatcher, OpenFileDeliveries, OpenFileRegistry, OpenFileWatcher,
};
use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};

/// The spelling the registry and the ignore stamps key a path on, which is the
/// spelling the platform reports it in.
pub fn resolved(path: &Path) -> PathBuf {
    resolve_for_containment(path)
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf())
}

/// A backend that delivers nothing and keeps the folders it was asked to watch.
#[derive(Clone, Default)]
pub struct RecordingDirs(Arc<Mutex<BTreeSet<PathBuf>>>);

impl RecordingDirs {
    pub fn watched(&self) -> BTreeSet<PathBuf> {
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

/// Both watchers' delivery state over one app state.
pub struct FedWatchers {
    native: RecordingDirs,
    polled: RecordingDirs,
    notes_root: PathBuf,
    open_files: OpenFileDeliveries,
    notes: NotesDeliveries,
}

impl FedWatchers {
    /// Installs the app's tracking and the open-file registry on `state`, then
    /// the notes watcher over the notes folder routing by that registry.
    pub fn install(state: &Arc<AppState>) -> Self {
        *state.file_tracking.lock().expect("tracking slot") = Some(FileTracking::of_state(state));
        let native = RecordingDirs::default();
        let polled = RecordingDirs::default();
        let notes_root = state.notes_root();
        let registry = Arc::new(Mutex::new(OpenFileRegistry::new(
            Box::new(native.clone()),
            Box::new(polled.clone()),
            &notes_root,
        )));
        *state.open_file_watcher.lock().expect("watcher slot") =
            Some(OpenFileWatcher::over(registry.clone()));
        let open_files = OpenFileDeliveries::new(
            registry,
            state.watcher_ignore.clone(),
            state.file_tracking(),
        );
        let notes = NotesDeliveries::new(
            notes_root.clone(),
            state.watcher_ignore.clone(),
            state.open_notes(),
            state.file_tracking(),
        );
        Self {
            native,
            polled,
            notes_root: resolved(&notes_root),
            open_files,
            notes,
        }
    }

    /// Whether either backend is watching `folder`.
    pub fn watches(&self, folder: &Path) -> bool {
        let folder = resolved(folder);
        self.native.watched().contains(&folder) || self.polled.watched().contains(&folder)
    }

    /// What reaches the tabs when the platform reports a change in `folders`:
    /// every entry of each, each path in `gone` and a name that was never
    /// Jotra's, as one delivery to the open-file watcher for the folders its
    /// backends watch and one to the notes watcher for what is under its root.
    pub fn platform_reports(&mut self, folders: &[&Path], gone: &[&Path]) -> Vec<JotraEvent> {
        self.platform_reports_at(folders, gone, Instant::now())
    }

    /// [`Self::platform_reports`], delivered at `at` rather than now.
    pub fn platform_reports_at(
        &mut self,
        folders: &[&Path],
        gone: &[&Path],
        at: Instant,
    ) -> Vec<JotraEvent> {
        let paths = platform_report(folders, gone);
        let watched: BTreeSet<PathBuf> = self
            .native
            .watched()
            .union(&self.polled.watched())
            .cloned()
            .collect();
        let to_open_files: Vec<DebouncedEvent> = paths
            .iter()
            .filter(|path| path.parent().is_some_and(|dir| watched.contains(dir)))
            .map(|path| DebouncedEvent::new(path.clone(), DebouncedEventKind::Any))
            .collect();
        let to_notes: Vec<DebouncedEvent> = paths
            .iter()
            .filter(|path| path.starts_with(&self.notes_root))
            .map(|path| DebouncedEvent::new(path.clone(), DebouncedEventKind::Any))
            .collect();

        let mut told = Vec::new();
        if !to_open_files.is_empty() {
            told.extend(self.open_files.deliver(&to_open_files, at));
        }
        if !to_notes.is_empty() {
            self.notes
                .deliver(&to_notes, || at, |event| told.push(event));
        }
        told
    }

    /// The first moment either watcher owes something with no delivery.
    pub fn next_deadline(&self) -> Option<Instant> {
        [self.open_files.deadline(), self.notes.deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    /// What both watchers say when every held removal's window passes with
    /// nothing delivered, each answered at its own deadline.
    pub fn holds_pass(&mut self) -> Vec<JotraEvent> {
        let mut told = Vec::new();
        for _ in 0..16 {
            let Some(due) = self.open_files.deadline() else {
                break;
            };
            told.extend(self.open_files.on_deadline(due));
        }
        for _ in 0..16 {
            let Some(due) = self.notes.deadline() else {
                break;
            };
            self.notes.on_deadline(due, |event| told.push(event));
        }
        told
    }
}

/// The paths the platform reports after a change in `folders`, in their
/// resolved spelling, sorted.
fn platform_report(folders: &[&Path], gone: &[&Path]) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    for folder in folders {
        let spelled = resolved(folder);
        paths.insert(spelled.join(".tmpA1b2C3"));
        for entry in std::fs::read_dir(folder).expect("list the folder") {
            paths.insert(spelled.join(entry.expect("folder entry").file_name()));
        }
    }
    for path in gone {
        let folder = resolved(path.parent().expect("a gone path has a folder"));
        paths.insert(folder.join(path.file_name().expect("a gone path has a name")));
    }
    paths.into_iter().collect()
}
