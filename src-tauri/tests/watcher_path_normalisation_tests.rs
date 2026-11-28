//! The stamp a write records and the key the watcher looks up have to be the
//! same string for the same file, whatever the filesystem does to the name on
//! the way through (ADR-028 section 6).

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;

use jotra_core::buffer::manager::BufferManager;
use jotra_core::watcher::ignore::source_key;
use jotra_tauri_lib::commands::buffer::save_buffer_content_inner;
#[cfg(target_os = "macos")]
use jotra_tauri_lib::commands::file::open_file_from_path;
#[cfg(target_os = "macos")]
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::security::canonicalize_root;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// `true` when the ignore set holds a stamp for `path`'s canonical form, which
/// is the key the watcher builds from a delivered event.
fn stamp_is_findable_from_the_event_path(state: &AppState, path: &std::path::Path) -> bool {
    let canonical = canonicalize_root(path).expect("canonical");
    let key = source_key(&canonical);
    state.watcher_ignore.lock().unwrap().contains(&key)
}

/// Saves into a note that has no file yet, which mints one in the notes folder
/// and stamps it before it exists, and returns the file it minted.
fn save_a_new_note(state: &AppState, content: &str) -> std::path::PathBuf {
    let doc = BufferManager::new().create_buffer(None).expect("mint");
    state
        .store
        .lock()
        .unwrap()
        .insert(&doc)
        .expect("insert the row");
    save_buffer_content_inner(state, &doc.id, content).expect("save");

    let written: Vec<std::path::PathBuf> = std::fs::read_dir(state.notes_root())
        .expect("read notes folder")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(written.len(), 1, "{written:?}");
    written.into_iter().next().unwrap()
}

#[test]
fn source_key_is_stable_for_one_canonical_path() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("note.md");
    std::fs::write(&file, "x").unwrap();

    let once = canonicalize_root(&file).expect("canonical");
    let again = canonicalize_root(&file).expect("canonical");

    assert_eq!(source_key(&once), source_key(&again));
    assert_eq!(
        source_key(&once),
        source_key(std::path::Path::new(&once.to_string_lossy().into_owned())),
        "a key must survive the trip through a String and back"
    );
}

#[test]
fn a_stamp_for_a_file_that_does_not_exist_yet_matches_the_event_path_once_it_exists() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    // The first keystroke stamps the path it is about to create, so the key
    // has to come from the canonical folder plus the name rather than from a
    // canonicalisation that cannot run yet.
    let minted = save_a_new_note(&state, "the first keystroke");

    assert!(
        stamp_is_findable_from_the_event_path(&state, &minted),
        "the stamp for {} must match the path the watcher delivers",
        minted.display()
    );
}

/// Opens the file at `path` through the command layer and saves `content`,
/// which is the only way a save reaches the ignore set.
#[cfg(target_os = "macos")]
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

/// A real APFS path is the point of these two: the normalisation and case
/// behaviour they exercise belongs to the filesystem, not to Rust.
#[cfg(target_os = "macos")]
#[test]
fn arabic_filename_stamp_matches_the_watcher_event_path() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    let note = state.notes_root().join("ملاحظات المشروع.md");
    std::fs::write(&note, "قبل").unwrap();
    save_through_jotra(&state, &note, "بعد");

    assert!(stamp_is_findable_from_the_event_path(&state, &note));
}

#[cfg(target_os = "macos")]
#[test]
fn nfd_accented_filename_stamp_matches_the_watcher_event_path() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);

    // Created decomposed, reached composed: APFS accepts both spellings for
    // the one file, and a key built from the spelling the caller happened to
    // use would not match the one the watcher delivers.
    let decomposed = state.notes_root().join("cafe\u{0301}.md");
    std::fs::write(&decomposed, "avant").unwrap();
    let composed = state.notes_root().join("caf\u{00e9}.md");

    save_through_jotra(&state, &composed, "après");

    assert!(stamp_is_findable_from_the_event_path(&state, &decomposed));
    assert!(stamp_is_findable_from_the_event_path(&state, &composed));
}
