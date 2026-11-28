//! The note commands at the layer the frontend reaches (ADR-028 §3).

#[path = "support/app_state.rs"]
mod app_state;
#[path = "support/fed_watchers.rs"]
mod fed_watchers;

use app_state::AppStateBuilder;
use fed_watchers::FedWatchers;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jotra_core::events::bus::JotraEvent;
use jotra_core::hash::sha256_bytes;
use jotra_core::notes::guard::SF_DATALESS;
use jotra_core::notes::line_ending::LineEnding;
use jotra_core::notes::reload::ChangeChoice;
use jotra_core::notes::{WriteOrigin, Writer};
use jotra_core::text_encoding::TextEncoding;
use jotra_core::watcher::change_event::ExternalChange;
use jotra_core::watcher::pending::hold_window;
use jotra_storage::errors::StorageError;
use jotra_storage::notes_index::{self};
#[cfg(unix)]
use jotra_tauri_lib::commands::buffer::ERR_FOLDER_NOT_WRITABLE;
use jotra_tauri_lib::commands::buffer::{
    note_file_unchanged_inner, read_buffer_content_inner, resolve_external_change_at,
    resolve_external_change_inner, restore_note_file_inner, save_buffer_content_inner,
    ERR_FILE_CHANGED_ON_DISK, ERR_FILE_MISSING, ERR_FILE_NOT_DOWNLOADED, ERR_FILE_REMOVED_ON_DISK,
};
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::commands::note_index::resolve_note_link_inner;
use jotra_tauri_lib::commands::notes::{
    delete_note_inner, move_notes_folder_to, new_note_from_link_inner, new_note_in_folder_inner,
    new_note_inner, new_note_named_inner, note_path_for_id, notes_root_text, path_is_inside_notes,
    rename_note_inner, rename_note_recording, save_note_copy_inner, undo_delete_note_inner,
};
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use jotra_tauri_lib::watcher::handler::{start_notes_watcher, NOTES_DEBOUNCE_WINDOW};
use jotra_tauri_lib::watcher::moves::{FileTracking, MoveOutcome};
use jotra_tauri_lib::watcher::open_files::start_open_file_watcher;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// The file a note lives in, as the row records it.
fn note_file(state: &AppState, id: &str) -> std::path::PathBuf {
    let store = state.store.lock().expect("lock");
    let doc = store.get(id).expect("row");
    std::path::PathBuf::from(doc.source_path.expect("the note has no file"))
}

fn title_of(state: &AppState, id: &str) -> String {
    let store = state.store.lock().expect("lock");
    store.get(id).expect("row").title
}

/// Points the state at Markdown, for the tests whose subject is what a
/// Markdown file does.
fn defaults_to_markdown(state: &AppState) {
    state.config.lock().expect("config").files.default_extension =
        jotra_core::config::FileExtension::Md;
}

/// Every stem `jotra_core::notes::minted_stem` gives a moment from `since` to
/// `until`, oldest first. The clock can pass a minute between the app minting
/// a file and a test naming it, so each minute the span touches is a stem the
/// file may carry.
fn minted_stems(
    since: chrono::DateTime<chrono::Utc>,
    until: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let mut stems = Vec::new();
    let mut moment = since;
    while moment <= until {
        stems.push(jotra_core::notes::minted_stem(moment));
        moment += chrono::Duration::seconds(60);
    }
    stems.push(jotra_core::notes::minted_stem(until));
    stems.dedup();
    stems
}

/// Opens a file from outside the notes folder the way the frontend does.
fn open_note_at(state: &AppState, path: &std::path::Path, content: &str) -> String {
    std::fs::write(path, content).expect("write");
    let canonical = canonicalize_for_authorization(path).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("the file opened")
        .id
}

#[test]
fn a_new_note_and_a_copy_wait_for_the_write_lock_without_holding_the_store() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let elsewhere = dir.path().join("report.md");
    let id = open_note_at(&state, &elsewhere, "the text");
    let lock = jotra_storage::write_lock::WriteLock::in_data_folder(&state.jotra_dir);
    let held = lock.hold().expect("the lock is free");

    std::thread::scope(|scope| {
        let created = scope.spawn(|| new_note_named_inner(&state, "Grocery list"));
        let copied = scope.spawn(|| save_note_copy_inner(&state, &id, "the text plus more"));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !created.is_finished() && !copied.is_finished(),
            "a write landed while another writer held the data folder's lock"
        );
        assert!(
            state.store.try_lock().is_ok(),
            "a write waits for the lock holding the buffer store"
        );
        drop(held);
        created
            .join()
            .expect("the create thread")
            .expect("the note is made once the lock is free");
        copied
            .join()
            .expect("the copy thread")
            .expect("the copy is made once the lock is free");
    });
}

#[test]
fn a_named_note_takes_its_file_name_from_the_name() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    let doc = new_note_named_inner(&state, "Grocery list").expect("named note");
    let path = std::path::PathBuf::from(doc.source_path.clone().expect("the note has no file"));

    assert_eq!(
        path.file_name().unwrap().to_string_lossy(),
        "Grocery list.txt",
        "a named file carries the configured format like every other mint"
    );
    assert!(path.starts_with(state.notes_root()), "{}", path.display());
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "");
}

/// A name written with its extension carries that one extension, and the
/// extension it spells is the format it is minted in: `Plain.txt` under a
/// Markdown config is a `.txt` file, not `Plain.txt.md` (ADR-041 §2).
#[test]
fn a_named_note_written_with_the_extension_keeps_that_format() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    defaults_to_markdown(&state);

    for (typed, expected) in [
        ("Note.md", "Note.md"),
        ("Recipes.markdown", "Recipes.markdown"),
        ("Ideas.MD", "Ideas.md"),
        ("Log.md.md", "Log.md.md"),
        ("Plain.txt", "Plain.txt"),
    ] {
        let doc = new_note_named_inner(&state, typed).expect("named note");
        let path = std::path::PathBuf::from(doc.source_path.expect("the note has no file"));
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            expected,
            "typed {typed}"
        );
    }
}

/// The whole round trip the offer makes: the target names no note, the note is
/// created from what the editor sends, and the same target then resolves to
/// the note that was created.
///
/// The second column is what `wikilinkTargetPath` in `src/lib/wikilink.ts`
/// sends: the target's folder and name, alias and heading removed, the
/// extension left on. Removing the extension there as well as here made
/// `Note.md` out of `[[Note.markdown.md]]`, a file the link that offered it
/// does not reach; flattening the folder made `Ideas.md` out of
/// `[[projects/Ideas]]`, which that target does not reach either.
#[test]
fn a_note_created_from_a_link_target_is_what_that_target_resolves_to() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let from = state.notes_root().join("source.md");

    for (target, sent) in [
        ("Grocery list", "Grocery list"),
        ("Recipes.md", "Recipes.md"),
        ("Note.markdown.md", "Note.markdown.md"),
        ("Log.md#Section", "Log.md"),
        ("Plans|later", "Plans"),
        ("projects/Ideas", "projects/Ideas"),
        ("a/b/Deep.md", "a/b/Deep.md"),
        ("../ideas/Relative", "ideas/Relative"),
    ] {
        let doc = new_note_from_link_inner(&state, sent).expect("note from link");
        let created = doc.source_path.expect("the note has no file");
        state
            .notes_index
            .reconcile(&state.notes_root(), &|| false, &|_| false)
            .expect("index the notes folder");

        let resolution = resolve_note_link_inner(
            &state.notes_index,
            from.to_str().expect("utf-8 path"),
            target,
        )
        .expect("resolve");
        assert_eq!(resolution.status, "resolved", "target {target}");
        assert_eq!(
            resolution.path.as_deref(),
            Some(notes_index::index_key(std::path::Path::new(&created)).as_str()),
            "target {target}"
        );
    }
}

/// The folder the target named is where the note goes, and it is created on
/// the way rather than the note being flattened into the notes folder.
#[test]
fn a_link_target_that_names_a_folder_creates_the_note_inside_it() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    let doc = new_note_from_link_inner(&state, "projects/2026/Ideas.md").expect("note from link");
    let path = std::path::PathBuf::from(doc.source_path.expect("the note has no file"));
    assert_eq!(
        path,
        state
            .notes_root()
            .join("projects")
            .join("2026")
            .join("Ideas.md")
    );
    assert!(path.exists(), "{}", path.display());

    let second =
        new_note_from_link_inner(&state, "projects/2026/Ideas.md").expect("note from link");
    assert_eq!(
        std::path::PathBuf::from(second.source_path.expect("the note has no file")),
        state
            .notes_root()
            .join("projects")
            .join("2026")
            .join("Ideas-2.md"),
        "the second note of that name takes the next one in the same folder"
    );
}

/// Anything a target holds that could walk out of the notes folder is gone
/// before a folder name is built from it, and a segment that survives as
/// nothing is dropped rather than minted.
#[test]
fn a_link_target_cannot_create_a_note_outside_the_notes_folder() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    for target in [
        "../../escape.md",
        "/etc/passwd",
        "a/../../../b/Note.md",
        ".../Note.md",
    ] {
        let doc = new_note_from_link_inner(&state, target).expect("note from link");
        let path = std::path::PathBuf::from(doc.source_path.expect("the note has no file"));
        assert!(
            path.starts_with(state.notes_root()),
            "{target:?} left the notes folder: {}",
            path.display()
        );
    }
}

/// The one way a sanitised segment still leaves the folder: something already
/// standing where it lands. The containment check resolves the deepest part of
/// the path that exists, so the symlink is followed before it is compared.
#[cfg(unix)]
#[test]
fn a_link_target_is_refused_when_its_folder_is_a_link_out_of_the_notes_folder() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).expect("create outside");
    std::fs::create_dir_all(state.notes_root()).expect("create notes root");
    std::os::unix::fs::symlink(&outside, state.notes_root().join("projects")).expect("symlink");

    let refusal = new_note_from_link_inner(&state, "projects/Ideas.md")
        .expect_err("a folder outside the notes folder is refused");
    assert!(refusal.contains("points outside your folder"), "{refusal}");
    assert!(
        !outside.join("Ideas.md").exists(),
        "the note was written through the link anyway"
    );
}

/// The configured format is the rule for a name that spells none, and a name
/// that spells one is minted in it (ADR-041 §2).
#[test]
fn a_named_note_takes_the_format_its_own_name_spells() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    for (typed, expected) in [
        ("Notes.md", "Notes.md"),
        ("Notes", "Notes.txt"),
        ("Notes.markdown", "Notes.markdown"),
    ] {
        let doc = new_note_named_inner(&state, typed).expect("named note");
        let path = std::path::PathBuf::from(doc.source_path.expect("the note has no file"));
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            expected,
            "typed {typed}"
        );
    }
}

/// A link target reaches this as typed, so a name that reads like a path must
/// not become one: the note lands in the notes folder whatever separators the
/// name carried.
#[test]
fn a_name_that_reads_like_a_path_stays_one_file_in_the_notes_folder() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    for name in ["../../escape", "/etc/passwd", "sub/deep"] {
        let doc = new_note_named_inner(&state, name).expect("named note");
        let path = std::path::PathBuf::from(doc.source_path.clone().expect("the note has no file"));
        assert_eq!(
            path.parent().expect("parent"),
            state.notes_root(),
            "{name:?} left the notes folder: {}",
            path.display()
        );
    }
}

/// A name nothing survives from falls back to the stem a `New Note` gets, so
/// the offer never fails for want of a usable name.
#[test]
fn a_name_that_sanitises_to_nothing_still_mints_a_note() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    let doc = new_note_named_inner(&state, "   ").expect("named note");
    let path = std::path::PathBuf::from(doc.source_path.clone().expect("the note has no file"));
    assert!(path.exists());
    assert!(path.starts_with(state.notes_root()));
}

#[test]
fn a_new_note_in_a_folder_lands_in_that_folder() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let projects = state.notes_root().join("Projects");
    std::fs::create_dir(&projects).expect("folder");

    let doc = new_note_in_folder_inner(&state, &projects.to_string_lossy()).expect("new note");

    let path = std::path::PathBuf::from(doc.source_path.expect("the note has a file"));
    assert!(path.exists(), "{} was not created", path.display());
    assert_eq!(
        path.parent().map(|p| p.canonicalize().expect("canonical")),
        Some(projects.canonicalize().expect("canonical"))
    );
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("txt"));
}

#[test]
fn a_new_note_is_made_only_in_a_folder_the_notes_folder_holds() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let outside = TempDir::new().expect("outside");
    let note = state.notes_root().join("Plan.md");
    std::fs::write(&note, "x").expect("write");

    for refused in [
        outside.path().to_string_lossy().into_owned(),
        note.to_string_lossy().into_owned(),
        state.notes_root().join("..").to_string_lossy().into_owned(),
    ] {
        assert_eq!(
            new_note_in_folder_inner(&state, &refused).map(|doc| doc.id),
            Err("ERR_NOT_A_NOTE".to_string()),
            "{refused}"
        );
    }
    assert_eq!(std::fs::read_dir(outside.path()).expect("list").count(), 0);
}

#[test]
fn new_note_produces_a_file_on_disk_before_the_app_quits() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    let since = chrono::Utc::now();
    let doc = new_note_inner(&state).expect("new note");

    let path = std::path::PathBuf::from(doc.source_path.clone().expect("the note has no file"));
    assert!(
        path.exists(),
        "{} was not created; nothing was written until quit",
        path.display()
    );
    assert!(path.starts_with(state.notes_root()), "{}", path.display());
    let minted = minted_stems(since, chrono::Utc::now())
        .into_iter()
        .find(|stem| path == state.notes_root().join(format!("{stem}.txt")))
        .unwrap_or_else(|| panic!("{} is not a minted name", path.display()));
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "");
    assert_eq!(doc.title, path.file_name().unwrap().to_string_lossy());

    // A second one names itself around the first rather than over it. A
    // minute that turned since the first is a name of its own.
    let since = chrono::Utc::now();
    let second = new_note_inner(&state).expect("new note");
    let second_path =
        std::path::PathBuf::from(second.source_path.clone().expect("the note has no file"));
    let names: Vec<std::path::PathBuf> = minted_stems(since, chrono::Utc::now())
        .into_iter()
        .map(|stem| {
            let name = if stem == minted {
                format!("{stem}-2.txt")
            } else {
                format!("{stem}.txt")
            };
            state.notes_root().join(name)
        })
        .collect();
    assert!(
        names.contains(&second_path),
        "{} is not one of {names:?}",
        second_path.display()
    );
    assert!(second_path.exists());
    assert!(path.exists(), "the first note was written over");
}

#[test]
fn rename_note_keeps_the_buffer_id_so_the_tab_keeps_its_content() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    defaults_to_markdown(&state);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "the text").expect("save");
    let before = note_file(&state, &doc.id);

    let renamed = rename_note_inner(&state, &doc.id, "Grocery list").expect("rename");

    assert_eq!(renamed.id, doc.id, "the note's id moved with the rename");
    let after = note_file(&state, &doc.id);
    assert_eq!(after, state.notes_root().join("Grocery list.md"));
    assert!(!before.exists(), "the old name is still there");
    assert_eq!(std::fs::read_to_string(&after).expect("read"), "the text");
    assert_eq!(title_of(&state, &doc.id), "Grocery list.md");

    // The next save lands on the new file rather than recreating the old one.
    save_buffer_content_inner(&state, &doc.id, "more text").expect("save");
    assert_eq!(std::fs::read_to_string(&after).expect("read"), "more text");
    assert!(!before.exists());
}

#[test]
fn rename_keeps_the_extension_a_typed_name_already_carries() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    defaults_to_markdown(&state);
    let doc = new_note_inner(&state).expect("new note");

    rename_note_inner(&state, &doc.id, "Grocery list.md").expect("rename");

    assert_eq!(
        note_file(&state, &doc.id),
        state.notes_root().join("Grocery list.md")
    );
}

#[test]
fn rename_to_a_name_that_spells_a_format_changes_the_file_to_it() {
    // A `.txt` file renamed to `….md` is Markdown, and the row and the tab
    // follow the file to its new name rather than to `….md.txt`.
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "the text").expect("save");
    let before = note_file(&state, &doc.id);
    assert_eq!(before.extension(), Some("txt".as_ref()));

    rename_note_inner(&state, &doc.id, "Grocery list.md").expect("rename");

    let after = note_file(&state, &doc.id);
    assert_eq!(after, state.notes_root().join("Grocery list.md"));
    assert!(!before.exists(), "the old name is still there");
    assert_eq!(std::fs::read_to_string(&after).expect("read"), "the text");
    assert_eq!(title_of(&state, &doc.id), "Grocery list.md");
}

#[test]
fn rename_to_a_name_already_in_the_folder_says_which_one() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    defaults_to_markdown(&state);
    let doc = new_note_inner(&state).expect("new note");
    std::fs::write(
        state.notes_root().join("Grocery list.md"),
        "somebody else's",
    )
    .expect("seed");
    let before = note_file(&state, &doc.id);

    let error = rename_note_inner(&state, &doc.id, "Grocery list").expect_err("collision");

    assert_eq!(error, "A file named \"Grocery list.md\" is already there.");
    assert!(before.exists(), "the note was renamed anyway");
    assert_eq!(
        std::fs::read_to_string(state.notes_root().join("Grocery list.md")).expect("read"),
        "somebody else's"
    );
}

#[test]
fn rename_to_a_name_with_nothing_in_it_is_stopped() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    let before = note_file(&state, &doc.id);

    assert_eq!(
        rename_note_inner(&state, &doc.id, "   ").expect_err("empty"),
        "That name is empty."
    );
    assert_eq!(
        rename_note_inner(&state, &doc.id, "///").expect_err("empty"),
        "That name is empty."
    );
    assert!(before.exists());
}

#[test]
fn rename_refuses_when_the_file_changed_on_disk() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "what Jotra read").expect("save");
    let before = note_file(&state, &doc.id);
    std::fs::write(&before, "what somebody else wrote").expect("outside write");

    let error = rename_note_inner(&state, &doc.id, "Grocery list").expect_err("changed");

    assert!(
        error.starts_with(ERR_FILE_CHANGED_ON_DISK),
        "the editor cannot tell what happened from {error:?}"
    );
    assert!(before.exists(), "the file moved out from under the change");
    assert_eq!(
        std::fs::read_to_string(&before).expect("read"),
        "what somebody else wrote"
    );
    assert!(!state.notes_root().join("Grocery list.md").exists());
    assert_eq!(note_file(&state, &doc.id), before);
}

#[test]
fn delete_moves_to_trash_and_closes_the_tab() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "the text").expect("save");
    let path = note_file(&state, &doc.id);
    assert!(path.exists());

    delete_note_inner(&state, &doc.id).expect("delete");

    // Where the note went is the platform's business; that it left its path
    // and left Jotra is this test's.
    assert!(!path.exists(), "the note is still at its path");
    let store = state.store.lock().expect("lock");
    assert!(store.get(&doc.id).is_err(), "the row outlived the note");
    drop(store);
    assert!(state.disk_state(&doc.id).is_none());
}

#[test]
fn undo_after_delete_puts_the_file_back_where_it_was() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "the text\r\nmore").expect("save");
    let path = note_file(&state, &doc.id);
    let before = std::fs::read(&path).expect("read");

    let token = delete_note_inner(&state, &doc.id)
        .expect("delete")
        .expect("a note with a file has an undo");
    assert!(!path.exists());

    let restored = undo_delete_note_inner(&state, &token).expect("undo");

    assert_eq!(std::path::PathBuf::from(&restored), path);
    assert_eq!(std::fs::read(&path).expect("read back"), before);
    assert!(
        undo_delete_note_inner(&state, &token).is_err(),
        "a token puts the file back once"
    );
}

#[test]
fn undo_after_delete_leaves_a_file_written_since() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "the old text").expect("save");
    let path = note_file(&state, &doc.id);

    let token = delete_note_inner(&state, &doc.id)
        .expect("delete")
        .expect("undo");
    std::fs::write(&path, "written since").expect("write since");

    assert!(undo_delete_note_inner(&state, &token).is_err());
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "written since"
    );
}

#[test]
fn undo_after_delete_refuses_a_token_it_did_not_hand_out() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "the text").expect("save");
    delete_note_inner(&state, &doc.id).expect("delete");

    assert!(undo_delete_note_inner(&state, "not-the-token").is_err());
}

#[test]
fn save_copy_leaves_the_original_untouched() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let elsewhere = dir.path().join("report.md");
    let id = open_note_at(&state, &elsewhere, "the text");

    let copy = save_note_copy_inner(&state, &id, "the text plus more").expect("copy");
    let copy = std::path::PathBuf::from(copy);

    assert_eq!(copy, state.notes_root().join("report.md"));
    assert_eq!(
        std::fs::read_to_string(&copy).expect("read"),
        "the text plus more"
    );
    assert!(elsewhere.exists(), "the file the copy came from is gone");
    assert_eq!(
        std::fs::read_to_string(&elsewhere).expect("read"),
        "the text"
    );
    assert_eq!(
        note_file(&state, &id),
        jotra_tauri_lib::security::canonicalize_root(&elsewhere).expect("canonical"),
        "the note followed its copy instead of staying put"
    );

    // The copy is a file inside the notes folder, so it opens with no further
    // permission and is a note of its own.
    let opened = open_file_from_path(&state, copy.to_str().expect("utf-8")).expect("open");
    assert_ne!(opened.doc.as_ref().expect("the file opened").id, id);
    assert_eq!(
        sha256_bytes(std::fs::read(&copy).expect("read").as_slice()),
        sha256_bytes(b"the text plus more")
    );
}

#[test]
fn the_notes_folder_is_reported_as_the_path_notes_are_written_to() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");

    let root = notes_root_text(&state);

    assert_eq!(root, state.notes_root().to_string_lossy());
    assert!(
        doc.source_path.as_deref().expect("file").starts_with(&root),
        "a note landed outside the folder the sidebar is told about"
    );
}

#[test]
fn a_note_is_shown_by_the_file_its_row_names() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");

    assert_eq!(
        note_path_for_id(&state, &doc.id).expect("path"),
        doc.source_path.expect("file")
    );
    assert!(note_path_for_id(&state, "no-such-note").is_err());
}

#[test]
fn only_a_file_the_notes_folder_holds_is_inside_it() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let inside = state.notes_root().join("2026-08-29.md");
    std::fs::write(&inside, "the text").expect("seed");
    let outside = dir.path().join("elsewhere.md");
    std::fs::write(&outside, "somebody else's").expect("seed");

    assert!(path_is_inside_notes(
        &state,
        inside.to_str().expect("utf-8")
    ));
    assert!(!path_is_inside_notes(
        &state,
        outside.to_str().expect("utf-8")
    ));
    // A walk back out of the folder is not inside it.
    assert!(!path_is_inside_notes(
        &state,
        state
            .notes_root()
            .join("../elsewhere.md")
            .to_str()
            .expect("utf-8")
    ));
    assert!(!path_is_inside_notes(&state, "elsewhere.md"));
}

#[test]
fn a_file_opened_from_elsewhere_is_never_moved_to_the_trash() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let elsewhere = dir.path().join("somebody-elses.md");
    let id = open_note_at(&state, &elsewhere, "not mine to delete");

    let error = delete_note_inner(&state, &id).expect_err("outside the notes folder");

    assert_eq!(
        error,
        "Only files in your folder can be moved to the Trash from here."
    );
    assert!(elsewhere.exists(), "somebody else's file was deleted");
    assert_eq!(
        std::fs::read_to_string(&elsewhere).expect("read"),
        "not mine to delete"
    );
    let store = state.store.lock().expect("lock");
    assert!(store.get(&id).is_ok(), "the tab was closed anyway");
}

#[test]
fn a_note_that_never_reached_a_file_only_loses_its_row() {
    // Containment has nothing to decide for a note with no file, and there is
    // nothing to hand the Trash. The row still goes.
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let id = {
        let store = state.store.lock().expect("lock");
        let mut mgr = jotra_core::buffer::manager::BufferManager::new();
        let doc = mgr.create_buffer(None).expect("mint");
        store.insert(&doc).expect("insert");
        doc.id
    };

    let token = delete_note_inner(&state, &id).expect("delete");

    assert_eq!(token, None, "a note with no file has nothing to put back");
    let store = state.store.lock().expect("lock");
    assert!(store.get(&id).is_err(), "the row outlived the note");
}

#[test]
fn a_rename_the_row_cannot_follow_puts_the_file_back() {
    // The file moves before the row does. If the row write fails in that
    // window, leaving the file moved would leave a row pointing at nothing,
    // which is a note nobody can open again.
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "the text").expect("save");
    let before = note_file(&state, &doc.id);

    let refuse = |_: &jotra_storage::buffer_store::BufferStore,
                  _: &str,
                  _: &str,
                  _: &str,
                  _: &str|
     -> Result<(), StorageError> {
        Err(StorageError::Consistency {
            message: "the row could not be written".to_string(),
        })
    };

    let error =
        rename_note_recording(&state, &doc.id, "Grocery list", &refuse).expect_err("row failed");

    assert!(error.contains("the row could not be written"), "{error}");
    assert!(before.exists(), "the note is not back under its own name");
    assert_eq!(
        std::fs::read_to_string(&before).expect("read"),
        "the text",
        "the note came back without its text"
    );
    assert!(
        !state.notes_root().join("Grocery list.md").exists(),
        "the file was left under the name the row never took"
    );
    assert_eq!(note_file(&state, &doc.id), before);

    // The tab is still usable: the next save lands on the file the row names.
    save_buffer_content_inner(&state, &doc.id, "more text").expect("save");
    assert_eq!(std::fs::read_to_string(&before).expect("read"), "more text");
}

/// Long enough for a loaded machine; only a failing run waits it out.
const WAIT_BOUND: Duration = Duration::from_secs(15);

/// A state with both watchers running, as the app runs them: the open-file
/// registry first, because it is how the notes watcher answers "which tab
/// holds this file", then the notes watcher over the notes folder.
///
/// The handles are held by the state, so they live as long as it does.
fn watching_state(dir: &TempDir) -> (Arc<AppState>, mpsc::Receiver<JotraEvent>) {
    let state = Arc::new(make_state(dir));
    let (tx, rx) = mpsc::channel();
    state.event_bus.subscribe(move |event| {
        let _ = tx.send(event.clone());
    });

    // The real tracking, reached through the state rather than through an
    // application: a delete here is decided exactly as it is in the app, and
    // the row and the marks it moves are this state's.
    *state.file_tracking.lock().expect("tracking slot") = Some(FileTracking::of_state(&state));

    let open_files = start_open_file_watcher(
        state.event_bus.clone(),
        state.watcher_ignore.clone(),
        &state.notes_root(),
        state.file_tracking(),
    )
    .expect("start the open file watcher");
    *state.open_file_watcher.lock().expect("watcher slot") = Some(open_files);

    let notes = start_notes_watcher(
        state.event_bus.clone(),
        state.notes_root(),
        state.watcher_ignore.clone(),
        state.open_notes(),
        state.file_tracking(),
    )
    .expect("start the notes watcher");
    *state.notes_watcher.lock().expect("notes watcher slot") = Some(notes);

    (state, rx)
}

/// Writes `bytes` the way another program does: a sibling temp file renamed
/// over the target, which gives the file a new inode.
fn rewrite_from_outside(path: &std::path::Path, bytes: &[u8]) {
    let temp = path.with_extension("other-program-tmp");
    std::fs::write(&temp, bytes).expect("write temp");
    std::fs::rename(&temp, path).expect("rename over target");
}

/// A state with both watchers installed as the app installs them, and the
/// watchers the test hands what the platform reports.
fn fed_state(dir: &TempDir) -> (Arc<AppState>, FedWatchers) {
    let state = Arc::new(make_state(dir));
    let fed = FedWatchers::install(&state);
    (state, fed)
}

/// The `BufferExternal` events in `told`.
fn externals(told: Vec<JotraEvent>) -> Vec<JotraEvent> {
    told.into_iter()
        .filter(|event| matches!(event, JotraEvent::BufferExternal { .. }))
        .collect()
}

/// The `BufferExternal` events one platform report of `folders` and `gone`
/// brings, and those of every removal it holds once the hold passes.
fn reported(
    fed: &mut FedWatchers,
    folders: &[&std::path::Path],
    gone: &[&std::path::Path],
) -> Vec<JotraEvent> {
    let mut told = externals(fed.platform_reports(folders, gone));
    told.extend(externals(fed.holds_pass()));
    told
}

/// Delivers the platform's report of `folder` as it stands, in a batch of its
/// own, as time between Jotra's write or open and the next change gives it.
fn delivered_apart(fed: &mut FedWatchers, folder: &std::path::Path) {
    let told = externals(fed.platform_reports(&[folder], &[]));
    assert!(
        told.is_empty(),
        "what Jotra did itself was reported: {told:?}"
    );
}

/// The buffer id and path one `BufferExternal` names.
fn named_by(event: &JotraEvent) -> (&str, &str) {
    match event {
        JotraEvent::BufferExternal {
            buffer_id, path, ..
        } => (buffer_id.as_str(), path.as_str()),
        other => panic!("not an external change: {other:?}"),
    }
}

#[test]
fn a_note_created_this_session_hears_that_another_program_rewrote_it() {
    // Cmd+N is how nearly every note comes into existence, and its file is in
    // the notes folder, which the notes watcher covers. Covering the folder is
    // not enough: the watcher has to be able to say which tab a changed path
    // belongs to, and only the registry answers that. A created note that
    // never reached the registry went on showing text its file no longer held
    // until a save was refused by the write guard.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    rewrite_from_outside(&path, b"rewritten by another program\n");

    let seen = externals(fed.platform_reports(&[&root], &[]));
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    let (buffer_id, named) = named_by(&seen[0]);
    assert_eq!(buffer_id, doc.id);
    assert_eq!(std::path::Path::new(named), path);
}

/// The one test here that runs the platform's watchers. Delivery lags on a
/// loaded machine, so a rewrite that has not been reported is made again, each
/// time with new bytes.
#[test]
fn the_platform_watcher_reports_another_programs_rewrite_of_a_note() {
    let dir = TempDir::new().expect("temp dir");
    let (state, rx) = watching_state(&dir);

    let doc = new_note_inner(&state).expect("new note");
    let path = note_file(&state, &doc.id);

    let heard = (0..4).any(|attempt| {
        rewrite_from_outside(
            &path,
            format!("rewritten by another program, take {attempt}\n").as_bytes(),
        );
        let deadline = Instant::now() + WAIT_BOUND;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(event @ JotraEvent::BufferExternal { .. }) if named_by(&event).0 == doc.id => {
                    return true
                }
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    });
    assert!(heard, "no rewrite of the note reached its tab");
}

/// What one `BufferExternal` says wrote the file.
fn writer_named_by(event: &JotraEvent) -> Option<Writer> {
    match event {
        JotraEvent::BufferExternal { writer, .. } => writer.clone(),
        other => panic!("not an external change: {other:?}"),
    }
}

#[test]
fn a_programs_write_reaches_the_tab_with_the_programs_name_and_an_unknown_one_without() {
    // `jotra mcp` keeps the text a program's write landed as the note's newest
    // version, under the program's name, before the watcher reports the
    // change. The tab is told who wrote it from that entry.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    // The app tells its version store where the notes are at startup.
    state.note_history.set_notes_root(state.notes_root());
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    let written = b"rewritten by a program\n";
    rewrite_from_outside(&path, written);
    let key = state
        .note_history
        .key_for(&path)
        .expect("a note in the folder");
    state
        .note_history
        .capture(
            &key,
            written,
            std::time::SystemTime::now(),
            &WriteOrigin::Mcp {
                client: "claude-code".to_string(),
            },
        )
        .expect("the program's version");

    let seen = externals(fed.platform_reports(&[&root], &[]));
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(named_by(&seen[0]).0, doc.id);
    assert_eq!(
        writer_named_by(&seen[0]),
        Some(Writer::Program {
            name: "claude-code".to_string()
        })
    );

    rewrite_from_outside(&path, b"then another program nobody named\n");
    let seen = externals(fed.platform_reports(&[&root], &[]));
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(writer_named_by(&seen[0]), None);
}

#[test]
fn a_programs_version_kept_after_the_watchers_window_still_names_the_program() {
    // A program keeps the version of what it landed before it lets go of the
    // write lock, so the watcher waits for the lock rather than report the
    // change with nobody behind it.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    state.note_history.set_notes_root(state.notes_root());
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    let written = b"rewritten by a program that keeps its version slowly\n";
    let lock = jotra_storage::write_lock::WriteLock::in_data_folder(&state.jotra_dir);
    let held = lock.hold().expect("the write lock");
    rewrite_from_outside(&path, written);
    let (told, telling) = mpsc::channel();
    let reporting = std::thread::spawn(move || {
        let seen = externals(fed.platform_reports(&[&root], &[]));
        told.send(()).expect("the test is listening");
        seen
    });
    let deadline = Instant::now() + WAIT_BOUND;
    while lock.waiting() == 0 {
        assert!(
            telling.try_recv().is_err(),
            "the change was reported before the program let go of the lock"
        );
        assert!(
            Instant::now() < deadline,
            "the watcher never waited for the lock"
        );
        std::thread::yield_now();
    }
    let key = state
        .note_history
        .key_for(&path)
        .expect("a note in the folder");
    state
        .note_history
        .capture(
            &key,
            written,
            std::time::SystemTime::now(),
            &WriteOrigin::Mcp {
                client: "claude-code".to_string(),
            },
        )
        .expect("the program's version");
    drop(held);

    let seen = reporting.join().expect("the watcher's delivery");
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(named_by(&seen[0]).0, doc.id);
    assert_eq!(
        writer_named_by(&seen[0]),
        Some(Writer::Program {
            name: "claude-code".to_string()
        })
    );
}

#[test]
fn a_note_renamed_this_session_hears_about_its_new_file_and_not_its_old_name() {
    // The rename moves the file and re-keys the row. A registry left holding
    // the old path answers for the wrong file twice over: changes to the
    // renamed file reach no tab, and a later note taking the freed name has
    // them delivered to this one.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    let before = note_file(&state, &doc.id);
    rename_note_inner(&state, &doc.id, "Grocery list").expect("rename");
    let after = note_file(&state, &doc.id);
    assert_ne!(after, before);
    let mut seen = externals(fed.platform_reports(&[&root], &[&before]));
    seen.extend(externals(fed.holds_pass()));
    assert!(seen.is_empty(), "Jotra's own rename was reported: {seen:?}");

    // Something else takes the name the rename freed, and the renamed file is
    // rewritten. Only the second is this tab's.
    std::fs::write(&before, b"a different note under the old name\n").expect("write old name");
    rewrite_from_outside(&after, b"rewritten by another program\n");

    let seen = externals(fed.platform_reports(&[&root], &[]));
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    let (buffer_id, named) = named_by(&seen[0]);
    assert_eq!(buffer_id, doc.id);
    assert_eq!(
        std::path::Path::new(named),
        after,
        "the tab was told about the name it no longer has"
    );
}

#[test]
fn a_note_given_its_file_on_first_save_is_followed_from_that_save() {
    // A note with no file yet gets one on its first save, which is the other
    // way a tab comes to hold a file this session.
    let dir = TempDir::new().expect("temp dir");
    let (state, _fed) = fed_state(&dir);

    let doc = {
        let store = state.store.lock().expect("lock");
        let doc = jotra_core::buffer::manager::BufferManager::new()
            .create_buffer(Some("Fileless".to_string()))
            .expect("create");
        store.insert(&doc).expect("insert");
        doc
    };
    assert_eq!(state.open_notes().note_at(&state.notes_root()), None);

    save_buffer_content_inner(&state, &doc.id, "first text").expect("save");

    let path = note_file(&state, &doc.id);
    assert_eq!(
        state.open_notes().note_at(&path).as_deref(),
        Some(doc.id.as_str()),
        "the file the save attached is not followed"
    );
}

#[test]
fn open_tabs_follow_their_files_when_the_notes_folder_moves() {
    let dir = TempDir::new().expect("temp dir");
    let (state, _fed) = fed_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    let before = note_file(&state, &doc.id);

    let elsewhere = TempDir::new().expect("somewhere else");
    let destination = elsewhere.path().join("Moved");
    move_notes_folder_to(&state, &destination).expect("move the notes folder");

    let after = note_file(&state, &doc.id);
    assert_ne!(after, before);
    assert_eq!(
        state.open_notes().note_at(&after).as_deref(),
        Some(doc.id.as_str()),
        "the tab is still following the folder Jotra left"
    );
    assert_eq!(
        state.open_notes().note_at(&before),
        None,
        "the tab still answers for a file that is not there"
    );
}

#[test]
fn a_folder_move_keeps_the_record_of_a_note_another_program_changed() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "what the tab holds").expect("save");
    let outside = "another program wrote more than this\n";
    std::fs::write(note_file(&state, &doc.id), outside).expect("write in place");

    let elsewhere = TempDir::new().expect("somewhere else");
    move_notes_folder_to(&state, &elsewhere.path().join("Moved")).expect("move the notes folder");

    assert!(
        !note_file_unchanged_inner(&state, &doc.id),
        "the switch back still finds the change"
    );
    let refused = save_buffer_content_inner(&state, &doc.id, "what the tab holds, typed")
        .expect_err("the next save must not land over the change");
    assert!(refused.starts_with(ERR_FILE_CHANGED_ON_DISK), "{refused}");
    assert_eq!(
        std::fs::read_to_string(note_file(&state, &doc.id)).expect("read the moved file"),
        outside
    );
}

#[test]
fn a_file_opened_from_outside_the_notes_folder_survives_the_move() {
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);

    let workspace = TempDir::new().expect("a folder Jotra does not own");
    let outside = workspace.path().join("outside.md");
    let id = open_note_at(&state, &outside, "opened from elsewhere");
    delivered_apart(&mut fed, workspace.path());

    let elsewhere = TempDir::new().expect("somewhere else");
    move_notes_folder_to(&state, &elsewhere.path().join("Moved")).expect("move the notes folder");
    assert!(
        fed.watches(workspace.path()),
        "the move took a watch on a folder that has nothing to do with it"
    );

    rewrite_from_outside(&outside, b"another program got there first");

    let events = externals(fed.platform_reports(&[workspace.path()], &[]));
    assert!(
        events.iter().any(|event| named_by(event).0 == id),
        "the move took a watch on a folder that has nothing to do with it: {events:?}"
    );
}

/// The change one `BufferExternal` reports, and where it says the file went.
fn change_of(event: &JotraEvent) -> (&ExternalChange, Option<&str>) {
    match event {
        JotraEvent::BufferExternal {
            change, new_path, ..
        } => (change, new_path.as_deref()),
        other => panic!("not an external change: {other:?}"),
    }
}

#[test]
fn a_note_moved_inside_the_notes_folder_keeps_its_tab_and_leaves_nothing_behind() {
    // The headline of W4. Moving a note in Finder must not make the tab a
    // stale window onto a path nothing holds: the next autosave would put the
    // file back where it was, and in a synced folder every device would get
    // the duplicate.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    let sub = root.join("archive");
    std::fs::create_dir_all(&sub).expect("subfolder");
    let after = sub.join("moved-by-finder.md");
    delivered_apart(&mut fed, &root);

    std::fs::rename(&before, &after).expect("move the file the way Finder does");

    let seen = reported(&mut fed, &[&root, &sub], &[&before]);
    assert_eq!(
        seen.len(),
        1,
        "exactly one path update, and no other news, saw {seen:?}"
    );
    let (buffer_id, named) = named_by(&seen[0]);
    assert_eq!(buffer_id, doc.id);
    assert_eq!(
        std::path::Path::new(named),
        before,
        "the message names the path the tab knows, so the tab can recognise it"
    );
    let (change, new_path) = change_of(&seen[0]);
    assert_eq!(change, &ExternalChange::Moved);
    assert_eq!(new_path.map(std::path::Path::new), Some(after.as_path()));

    assert_eq!(
        note_file(&state, &doc.id),
        after,
        "the row still points at the path the file left"
    );
    assert_eq!(
        title_of(&state, &doc.id),
        "moved-by-finder.md",
        "the tab keeps the name the file had before it moved"
    );

    save_buffer_content_inner(&state, &doc.id, "text worth keeping, edited").expect("save again");
    assert!(
        !before.exists(),
        "the save recreated the note at the path the user moved it away from"
    );
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the moved file"),
        "text worth keeping, edited"
    );
}

#[test]
fn a_note_deleted_outside_jotra_is_not_recreated_by_the_next_save() {
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note the way Finder does");

    let seen = reported(&mut fed, &[&root], &[&path]);
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(named_by(&seen[0]).0, doc.id);
    assert_eq!(change_of(&seen[0]), (&ExternalChange::Removed, None));

    let refused = save_buffer_content_inner(&state, &doc.id, "text worth keeping, edited")
        .expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the refusal has to carry its own code, got {refused}"
    );
    assert!(
        !path.exists(),
        "the save put back the file the user threw away"
    );
}

#[test]
fn a_note_deleted_outside_jotra_goes_back_to_its_path_when_asked_for() {
    // The refusal above is about a keystroke. This is the person asking, and
    // the text in the tab is the last copy of the note (ADR-028 §1), so the
    // one thing that must not happen is Jotra having nowhere to put it.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note the way Finder does");
    let seen = reported(&mut fed, &[&root], &[&path]);
    assert_eq!(change_of(&seen[0]), (&ExternalChange::Removed, None));

    restore_note_file_inner(&state, &doc.id, "text worth keeping, edited")
        .expect("the restore must land");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the file back"),
        "text worth keeping, edited"
    );

    // The record follows the file, so the next keystroke saves the ordinary
    // way rather than being refused over a file that is there.
    save_buffer_content_inner(&state, &doc.id, "and edited again").expect("the next save");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the file back"),
        "and edited again"
    );
}

#[test]
fn a_restore_with_nowhere_to_write_says_so_and_leaves_the_note_removed() {
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note the way Finder does");
    let seen = reported(&mut fed, &[&root], &[&path]);
    assert_eq!(change_of(&seen[0]), (&ExternalChange::Removed, None));

    let folder = path.parent().expect("the note's folder").to_path_buf();
    std::fs::remove_dir_all(&folder).expect("take the folder away too");

    let refused = restore_note_file_inner(&state, &doc.id, "text worth keeping, edited")
        .expect_err("a write with nowhere to land must be refused");
    assert!(
        refused.starts_with(ERR_FILE_MISSING),
        "the refusal has to carry its own code, got {refused}"
    );
    // A restore that did not land leaves the note removed, so nothing later
    // recreates the file quietly.
    let after = save_buffer_content_inner(&state, &doc.id, "and edited again")
        .expect_err("the next save must still be refused");
    assert!(
        after.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the note stopped being marked removed, got {after}"
    );
}

#[test]
fn a_sync_client_replacing_a_file_is_an_external_modification_and_not_a_move() {
    // Delete plus create at the same path, which is how more than one sync
    // client lands an update. The file is a different file, and the tab is
    // told what every other rewrite tells it.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "as jotra left it").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("remove");
    std::fs::write(&path, b"as the sync client left it\n").expect("create in its place");

    let seen = reported(&mut fed, &[&root], &[]);
    assert!(!seen.is_empty(), "the tab heard nothing about the rewrite");
    for event in &seen {
        assert_eq!(named_by(event).0, doc.id);
        assert_eq!(
            change_of(event).0,
            &ExternalChange::Modified,
            "a replaced file is a modification, not a move and not a delete: {event:?}"
        );
    }
    assert!(
        !state.is_removed_on_disk(&doc.id),
        "the tab must still be writing to the file that took the path"
    );
    // And W2 governs from here, which is the whole of what the verdict
    // decides: the write guard stops the save, writes the text beside the note
    // and says why, rather than the tab refusing to write at all.
    let refused = save_buffer_content_inner(&state, &doc.id, "edited after the sync landed")
        .expect_err("a file rewritten under jotra is the write guard's business");
    assert!(
        refused.starts_with(ERR_FILE_CHANGED_ON_DISK),
        "got {refused}"
    );
}

#[test]
fn a_note_put_back_from_the_trash_re_attaches_to_its_file() {
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    let trash = dir.path().join("trash-stand-in.md");
    delivered_apart(&mut fed, &root);

    std::fs::rename(&path, &trash).expect("move to the trash");
    let removal = reported(&mut fed, &[&root], &[&path]);
    assert_eq!(
        removal.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Removed],
        "saw {removal:?}"
    );
    assert!(state.is_removed_on_disk(&doc.id));

    std::fs::rename(&trash, &path).expect("put it back");
    let restored = reported(&mut fed, &[&root], &[]);
    assert!(
        restored
            .iter()
            .any(|event| change_of(event).0 == &ExternalChange::Modified),
        "a file that came back must reach its tab, saw {restored:?}"
    );
    assert!(
        !state.is_removed_on_disk(&doc.id),
        "the tab is still refusing to write to a file that is there"
    );
    save_buffer_content_inner(&state, &doc.id, "edited after the restore").expect("save");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "edited after the restore"
    );
}

#[test]
fn a_tab_restored_at_launch_onto_a_file_that_is_gone_writes_nothing() {
    // Nothing about a deleted file survives a relaunch, so the state is read
    // back from the file when the tab is restored. Without that, a note whose
    // file was deleted while Jotra was closed comes back looking ordinary and
    // recreates the file on its first save.
    let dir = TempDir::new().expect("temp dir");
    let (state, _fed) = fed_state(&dir);

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    std::fs::remove_file(&path).expect("delete while jotra is not looking");
    state.forget_source_record(&doc.id);

    let row = {
        let store = state.store.lock().expect("lock");
        store.get(&doc.id).expect("row")
    };
    state.follow_note_file(&row);

    let refused = save_buffer_content_inner(&state, &doc.id, "edited after the relaunch")
        .expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "got {refused}"
    );
    assert!(!path.exists());
}

#[test]
fn a_file_opened_from_outside_the_notes_folder_follows_its_move_too() {
    // The other watcher, and the case the notes watcher cannot see: a file in
    // a folder Jotra follows only because a tab holds it.
    let dir = TempDir::new().expect("temp dir");
    let elsewhere = TempDir::new().expect("some other folder");
    let (state, mut fed) = fed_state(&dir);

    let file = elsewhere.path().join("shared.md");
    std::fs::write(&file, b"as another program left it\n").expect("seed");
    let canonical = canonicalize_for_authorization(&file).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    let opened = open_file_from_path(&state, &canonical).expect("open");
    let renamed = elsewhere.path().join("renamed-by-somebody.md");
    delivered_apart(&mut fed, elsewhere.path());

    std::fs::rename(&file, &renamed).expect("rename");

    let seen = externals(fed.platform_reports(&[elsewhere.path()], &[&file]));
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(
        named_by(&seen[0]).0,
        opened.doc.as_ref().expect("the file opened").id
    );
    let (change, new_path) = change_of(&seen[0]);
    assert_eq!(change, &ExternalChange::Moved);
    let landed = new_path.expect("a move names where the file went");
    assert_eq!(
        canonicalize_for_authorization(std::path::Path::new(landed)).expect("canonical"),
        canonicalize_for_authorization(&renamed).expect("canonical")
    );
    assert_eq!(
        canonicalize_for_authorization(&note_file(
            &state,
            &opened.doc.as_ref().expect("the file opened").id
        ))
        .expect("canonical"),
        canonicalize_for_authorization(&renamed).expect("canonical"),
        "the row still points at the path the file left"
    );
}

#[test]
fn a_rename_after_another_program_rewrote_the_file_is_still_a_move() {
    // Nearly every program that writes a file writes a sibling temp and
    // renames it over the target: vim, VS Code, git checkout, rsync, every
    // sync client. The path is unchanged and the file behind it is a different
    // file. A tab still holding the id it read at open then reads its own next
    // rename as a deletion, marks itself removed and refuses every later save
    // over a file sitting at its new path with the user's text in it.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    rewrite_from_outside(&before, b"rewritten by another program\n");
    let rewrite = reported(&mut fed, &[&root], &[]);
    assert_eq!(
        rewrite.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Modified],
        "the rewrite has to reach the tab before the rename, saw {rewrite:?}"
    );
    // The tab reloads, which is what the editor does with that news and what
    // leaves the write guard satisfied. It reads the file; it says nothing
    // about the file's id.
    read_buffer_content_inner(&state, &doc.id).expect("reload");

    let after = root.join("renamed-by-finder.md");
    std::fs::rename(&before, &after).expect("rename the way Finder does");

    let seen = reported(&mut fed, &[&root], &[&before]);
    assert_eq!(
        seen.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Moved],
        "a rename after an external rewrite must still be a move, saw {seen:?}"
    );
    assert_eq!(
        change_of(&seen[0]).1.map(std::path::Path::new),
        Some(after.as_path())
    );
    assert!(
        !state.is_removed_on_disk(&doc.id),
        "the tab stopped writing to a file that is there"
    );
    assert_eq!(note_file(&state, &doc.id), after);

    save_buffer_content_inner(&state, &doc.id, "edited after the rename").expect("save");
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the moved file"),
        "edited after the rename"
    );
    assert!(
        !before.exists(),
        "the save recreated the file at the old path"
    );
}

#[test]
fn a_rewrite_and_a_rename_in_one_window_are_a_removal_that_replaces_no_text() {
    // Two writes inside one window are reported as one: a program rewrote the
    // file and renamed it, and the only event is the path going empty. The
    // rewrite is never reported, so the id on record is the one it retired and
    // nothing carries it anywhere. Nothing else names the file either, so the
    // tab is told its file went rather than being pointed at a path on
    // evidence that cannot tell a copy from a rename.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    rewrite_from_outside(&before, b"text worth keeping");
    let after = root.join("renamed-by-finder.md");
    std::fs::rename(&before, &after).expect("rename the way Finder does");

    let seen = reported(&mut fed, &[&root], &[&before]);
    assert_eq!(seen.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(named_by(&seen[0]).0, doc.id);
    assert_eq!(change_of(&seen[0]), (&ExternalChange::Removed, None));
    assert!(state.is_removed_on_disk(&doc.id));
    assert!(
        matches!(
            &seen[0],
            JotraEvent::BufferExternal {
                disk_hash: None,
                ..
            }
        ),
        "a removal carrying a digest has the editor replace the text it holds"
    );
    assert_eq!(
        note_file(&state, &doc.id),
        before,
        "the row followed a file the note was never given"
    );

    let refused = save_buffer_content_inner(&state, &doc.id, "edited after the rename")
        .expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the refusal has to carry its own code, got {refused}"
    );
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the file at the new path"),
        "text worth keeping",
        "the save wrote into a file the tab was never given"
    );
}

#[test]
fn a_note_dragged_out_of_the_notes_folder_leaves_a_twin_of_its_bytes_alone() {
    // A file that leaves every watched folder carries its id away with it, so
    // nothing in the window carries it and the note is gone as far as Jotra can
    // see. A file holding the same bytes in the same window is a copy, a
    // template, or a sync client's conflicted copy as easily as it is the
    // note, and taking it would let the next save replace its content.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    let outside = dir.path().join("outside-every-watched-folder");
    std::fs::create_dir_all(&outside).expect("a folder no watcher covers");
    let twin = state.notes_root().join("aaa-twin.md");
    std::fs::write(&twin, "text worth keeping").expect("the twin");
    let dragged = outside.join("dragged-out.md");
    std::fs::rename(&before, &dragged).expect("drag it out of the notes folder");

    let mut seen = externals(fed.platform_reports(&[&root], &[&before]));
    seen.extend(externals(fed.holds_pass()));
    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(told.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(change_of(told[0]), (&ExternalChange::Removed, None));
    assert_eq!(
        note_file(&state, &doc.id),
        before,
        "the row followed a file the note was never in"
    );

    let refused = save_buffer_content_inner(&state, &doc.id, "edited after the move")
        .expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the refusal has to carry its own code, got {refused}"
    );
    assert_eq!(
        std::fs::read_to_string(&twin).expect("read the twin"),
        "text worth keeping",
        "the save wrote over a file the note was never in"
    );
    assert_eq!(
        std::fs::read_to_string(&dragged).expect("read the file the user dragged out"),
        "text worth keeping"
    );
}

#[test]
fn a_note_moved_away_leaves_a_second_note_with_the_same_bytes_on_its_own_file() {
    // Two notes from one template hold the same bytes, which makes the file at
    // risk as likely to be another note as anything else. Both rows landed on
    // one path: the second note's text was overwritten by the first note's
    // next save while its tab still believed it owned that file.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let one = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &one.id, "text worth keeping").expect("save");
    let one_before = note_file(&state, &one.id);
    let two = new_note_inner(&state).expect("a second note");
    save_buffer_content_inner(&state, &two.id, "text worth keeping").expect("save");
    let two_file = note_file(&state, &two.id);
    delivered_apart(&mut fed, &root);

    let outside = dir.path().join("outside-every-watched-folder");
    std::fs::create_dir_all(&outside).expect("a folder no watcher covers");
    rewrite_from_outside(&two_file, b"text worth keeping");
    let one_moved = outside.join("dragged-out.md");
    std::fs::rename(&one_before, &one_moved).expect("drag the first note out");

    let seen = reported(&mut fed, &[&root], &[&one_before]);
    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == one.id).collect();
    assert_eq!(
        told.len(),
        1,
        "the moved note must be told once, saw {seen:?}"
    );
    assert_eq!(change_of(told[0]), (&ExternalChange::Removed, None));
    assert_eq!(note_file(&state, &one.id), one_before);
    assert_eq!(
        note_file(&state, &two.id),
        two_file,
        "the second note's row followed the first note's move"
    );

    let refused = save_buffer_content_inner(&state, &one.id, "edited after the move")
        .expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the refusal has to carry its own code, got {refused}"
    );
    assert_eq!(
        std::fs::read_to_string(&two_file).expect("read the second note's file"),
        "text worth keeping",
        "the first note's save wrote over the second note's file"
    );
}

#[test]
fn a_deleted_note_leaves_a_file_holding_the_same_bytes_alone() {
    // No move at all. The note's bytes exist at no path the note was ever at,
    // and a file that happens to hold the same bytes is still somebody else's
    // file: writing the note into it would replace its content with no event
    // and no error.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    let twin = root.join("aaa-twin.md");
    std::fs::write(&twin, "text worth keeping").expect("the twin");
    std::fs::remove_file(&path).expect("delete the note the way Finder does");

    let seen = reported(&mut fed, &[&root], &[&path]);
    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(told.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(change_of(told[0]), (&ExternalChange::Removed, None));

    let refused = save_buffer_content_inner(&state, &doc.id, "text worth keeping, edited")
        .expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the refusal has to carry its own code, got {refused}"
    );
    assert_eq!(
        std::fs::read_to_string(&twin).expect("read the twin"),
        "text worth keeping",
        "the save wrote over a file the note was never in"
    );
    assert!(
        !path.exists(),
        "the save put back the file the user threw away"
    );
}

#[test]
fn a_note_replaced_by_a_folder_of_the_same_name_reads_as_a_file_that_went() {
    // A path holding a directory holds no note. The event was dropped for
    // being about something that is not a file, so the tab heard nothing, kept
    // the dead file's id, and its next save came back as a raw
    // `Is a directory` instead of saying the file is gone.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note's file");
    std::fs::create_dir(&path).expect("a folder takes its name");

    let seen = reported(&mut fed, &[&root], &[]);
    assert_eq!(
        seen.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Removed],
        "a folder where the file was is a file that went, saw {seen:?}"
    );
    assert!(state.is_removed_on_disk(&doc.id));

    let refused = save_buffer_content_inner(&state, &doc.id, "edited after the folder appeared")
        .expect_err("the save has to refuse");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the tab must say the file is gone rather than pass on an errno, saw {refused}"
    );
    assert!(path.is_dir(), "the save wrote over the folder");
}

#[test]
fn a_file_outside_the_notes_folder_re_attaches_when_it_comes_back() {
    // The notes watcher covers the restore inside the notes folder. A file
    // opened from anywhere else has only the open-file watcher to hear it come
    // back, and that call site is the whole of the case.
    let dir = TempDir::new().expect("temp dir");
    let elsewhere = TempDir::new().expect("some other folder");
    let trash = TempDir::new().expect("somewhere the watcher cannot see");
    let (state, mut fed) = fed_state(&dir);

    let file = elsewhere.path().join("shared.md");
    std::fs::write(&file, b"as another program left it\n").expect("seed");
    let canonical = canonicalize_for_authorization(&file).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    let opened = open_file_from_path(&state, &canonical).expect("open");
    let held = trash.path().join("shared.md");
    delivered_apart(&mut fed, elsewhere.path());

    std::fs::rename(&file, &held).expect("move to the trash");
    let mut removal = externals(fed.platform_reports(&[elsewhere.path()], &[&file]));
    removal.extend(externals(fed.holds_pass()));
    assert_eq!(
        removal.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Removed],
        "saw {removal:?}"
    );
    assert!(state.is_removed_on_disk(&opened.doc.as_ref().expect("the file opened").id));

    std::fs::rename(&held, &file).expect("put it back");
    let restored = externals(fed.platform_reports(&[elsewhere.path()], &[]));
    assert!(
        restored
            .iter()
            .any(|event| change_of(event).0 == &ExternalChange::Modified),
        "a file that came back must reach its tab, saw {restored:?}"
    );
    assert!(
        !state.is_removed_on_disk(&opened.doc.as_ref().expect("the file opened").id),
        "the tab is still refusing to write to a file that is there"
    );
    read_buffer_content_inner(&state, &opened.doc.as_ref().expect("the file opened").id)
        .expect("reload");
    save_buffer_content_inner(
        &state,
        &opened.doc.as_ref().expect("the file opened").id,
        "edited after the restore",
    )
    .expect("save");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "edited after the restore"
    );
}

#[test]
fn deleting_one_name_of_a_hard_linked_file_follows_the_name_that_is_left() {
    // A hard link is one file with two names. Deleting one of them deletes a
    // name, not the file: the bytes the tab is editing are still there under
    // the other name, in a folder Jotra watches. Reporting a removal would
    // refuse every later save over a file that exists (ADR-033 §12).
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    let survivor = root.join("second-name.md");
    std::fs::hard_link(&path, &survivor).expect("a second name for the same file");
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete one of the two names");

    let seen = reported(&mut fed, &[&root], &[&path]);
    assert_eq!(
        seen.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Moved],
        "the file is still there under its other name, saw {seen:?}"
    );
    assert_eq!(
        change_of(&seen[0]).1.map(std::path::Path::new),
        Some(survivor.as_path())
    );
    assert!(!state.is_removed_on_disk(&doc.id));
    assert_eq!(note_file(&state, &doc.id), survivor);

    save_buffer_content_inner(&state, &doc.id, "edited after the name went").expect("save");
    assert_eq!(
        std::fs::read_to_string(&survivor).expect("read"),
        "edited after the name went"
    );
    assert!(
        !path.exists(),
        "the save recreated the name that was deleted"
    );
}

#[test]
fn a_move_another_program_made_keeps_the_record_of_a_change_no_watcher_reported() {
    let dir = TempDir::new().expect("temp dir");
    let state = Arc::new(make_state(&dir));
    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "what the tab holds").expect("save");
    assert!(note_file_unchanged_inner(&state, &doc.id));
    let before = note_file(&state, &doc.id);
    let outside = "another program wrote more than this\n";
    std::fs::write(&before, outside).expect("write in place");
    let after = state.notes_root().join("moved-by-finder.md");
    std::fs::rename(&before, &after).expect("move the file the way Finder does");

    let files = FileTracking::of_state(&state).files;
    assert_eq!(
        files.note_file_moved(&doc.id, &before, &after),
        MoveOutcome::Followed
    );

    assert!(
        !note_file_unchanged_inner(&state, &doc.id),
        "the switch back still finds the change"
    );
    let refused = save_buffer_content_inner(&state, &doc.id, "what the tab holds, typed")
        .expect_err("the next save must not land over the change");
    assert!(refused.starts_with(ERR_FILE_CHANGED_ON_DISK), "{refused}");
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the moved file"),
        outside
    );
}

#[test]
fn one_move_seen_by_both_watchers_moves_the_row_once() {
    // A file inside the notes folder that a tab also holds is reported by both
    // watchers, and each deduplicates only its own batch. The row is what the
    // next save reads its destination from, so the second report has to find
    // the move already applied and say nothing.
    let dir = TempDir::new().expect("temp dir");
    let state = Arc::new(make_state(&dir));

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    let after = state.notes_root().join("moved-by-finder.md");
    std::fs::rename(&before, &after).expect("move the file the way Finder does");

    let files = FileTracking::of_state(&state).files;
    assert_eq!(
        files.note_file_moved(&doc.id, &before, &after),
        MoveOutcome::Followed,
        "the first watcher to see the move applies it"
    );
    assert_eq!(note_file(&state, &doc.id), after);
    assert_eq!(title_of(&state, &doc.id), "moved-by-finder.md");

    assert_eq!(
        files.note_file_moved(&doc.id, &before, &after),
        MoveOutcome::AlreadyThere,
        "the second watcher's copy of the same move is not news"
    );
    assert_eq!(note_file(&state, &doc.id), after);
    assert_eq!(title_of(&state, &doc.id), "moved-by-finder.md");
}

/// The dated copies sitting beside a note, newest name last.
fn copies_beside(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("(conflict "))
        })
        .collect();
    paths.sort();
    paths
}

/// A note holding `mine`, whose file another program has rewritten to
/// `theirs` since Jotra last read it.
fn note_changed_under_jotra(state: &AppState, mine: &str, theirs: &str) -> (String, PathBuf) {
    let doc = new_note_inner(state).expect("new note");
    save_buffer_content_inner(state, &doc.id, mine).expect("save");
    let path = note_file(state, &doc.id);
    std::fs::write(&path, theirs).expect("the other program's write");
    (doc.id, path)
}

#[test]
fn keeping_mine_writes_the_file_s_text_beside_the_note_before_the_save_lands() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (id, path) = note_changed_under_jotra(&state, "what I typed", "what they wrote");

    let outcome =
        resolve_external_change_inner(&state, &id, ChangeChoice::KeepMine, "what I typed, still")
            .expect("resolve");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "what I typed, still"
    );
    let copies = copies_beside(&state.notes_root());
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        std::fs::read_to_string(&copies[0]).expect("read"),
        "what they wrote",
        "the text the choice did not keep has to be on disk"
    );
    assert_eq!(
        outcome.conflict_copy_path,
        Some(copies[0].to_string_lossy().into_owned())
    );
    assert_eq!(outcome.content, None, "the tab keeps what it is holding");
    assert_eq!(
        outcome.disk_hash,
        jotra_core::hash::comparison_digest_hex(b"what I typed, still")
    );

    // The record moved with the resolve, so the next ordinary save is not
    // refused by the guard over the change the person has already answered.
    save_buffer_content_inner(&state, &id, "typed after").expect("the next save was refused");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "typed after");
}

#[test]
fn taking_the_file_writes_the_document_s_text_beside_the_note() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (id, path) = note_changed_under_jotra(&state, "what I typed", "what they wrote");

    let outcome =
        resolve_external_change_inner(&state, &id, ChangeChoice::UseDisk, "what I typed, still")
            .expect("resolve");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "what they wrote",
        "the file is left exactly as the other program wrote it"
    );
    let copies = copies_beside(&state.notes_root());
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        std::fs::read_to_string(&copies[0]).expect("read"),
        "what I typed, still"
    );
    assert_eq!(outcome.content.as_deref(), Some("what they wrote"));
    assert_eq!(
        outcome.disk_hash,
        jotra_core::hash::comparison_digest_hex(b"what they wrote")
    );

    save_buffer_content_inner(&state, &id, "what they wrote, edited").expect("save");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "what they wrote, edited"
    );
}

/// How long a step on another thread waits for the answer to a hold: longer
/// than that thread can take to reach the hold on a loaded host, so only the
/// answer releases it.
const HOLD_AHEAD: Duration = Duration::from_secs(60);

/// Delivers the report of `gone` leaving `root` at `at`, and checks that
/// `id`'s removal is held rather than answered. `RemovalHolds::holds` is the
/// read the save path makes on its way in (ADR-033 §14).
fn held_at(
    fed: &mut FedWatchers,
    state: &AppState,
    root: &std::path::Path,
    gone: &std::path::Path,
    id: &str,
    at: Instant,
) -> Vec<JotraEvent> {
    let told = externals(fed.platform_reports_at(&[root], &[gone], at));
    assert!(
        told.iter().all(|event| named_by(event).0 != id),
        "the removal of {id} was answered in the delivery that raised it: {told:?}"
    );
    assert!(
        state.removal_holds.holds(id),
        "the removal of {id} was never held"
    );
    told
}

/// Runs `step` on its own thread and returns once it is waiting on `id`'s
/// hold, with the channel its result comes back on.
///
/// The hold is first given `HOLD_AHEAD` to be answered in, the way a second
/// watcher holding the same note extends it, so how long the thread takes to
/// get there cannot run the wait out.
fn waiting_on_hold<T: Send + 'static>(
    state: &Arc<AppState>,
    id: &str,
    step: impl FnOnce(&AppState) -> T + Send + 'static,
) -> mpsc::Receiver<T> {
    assert!(state.removal_holds.holds(id), "nothing holds {id}");
    state.removal_holds.hold(id, Instant::now() + HOLD_AHEAD);
    let (tx, rx) = mpsc::channel();
    let on_thread = Arc::clone(state);
    std::thread::spawn(move || {
        let _ = tx.send(step(&on_thread));
    });
    let deadline = Instant::now() + WAIT_BOUND;
    while state.removal_holds.waiting_on(id) == 0 {
        match rx.try_recv() {
            Ok(_) => panic!("the step came back without waiting on the hold of {id}"),
            Err(mpsc::TryRecvError::Disconnected) => panic!("the step on its own thread panicked"),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        assert!(
            Instant::now() < deadline,
            "nothing waited on the hold of {id}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    rx
}

/// What the step on the other thread came back with once the hold let it go.
fn released<T>(rx: &mpsc::Receiver<T>) -> T {
    rx.recv_timeout(WAIT_BOUND)
        .expect("the step was not released by the answer to its hold")
}

#[test]
fn a_rename_whose_halves_land_in_two_windows_is_still_a_move() {
    // What a rename looks like when it straddles a debounce deadline: the
    // notes folder is delivered the path going empty with nothing that could
    // answer for it, and the file only turns up at its new name in a later
    // delivery. Answering the first delivery on its own marks the tab off a
    // file that is sitting one folder away (ADR-033 §14).
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    // Out of the watched folder and back into it, a delivery apart: the hold
    // is proof the first half was read on its own, so the second cannot join
    // it. A rename keeps the inode, so what comes back is the file the record
    // names.
    let elsewhere = dir.path().join("outside-every-watched-folder");
    std::fs::create_dir_all(&elsewhere).expect("a folder nothing watches");
    let parked = elsewhere.join("in-flight.md");
    std::fs::rename(&before, &parked).expect("the first half");
    let first_half = Instant::now();
    let mut seen = externals(fed.platform_reports_at(&[&root], &[&before], first_half));
    assert!(
        state.removal_holds.holds(&doc.id),
        "the removal of the first half was never held"
    );
    let after = root.join("renamed-across-two-windows.md");
    std::fs::rename(&parked, &after).expect("the second half");
    seen.extend(externals(fed.platform_reports_at(
        &[&root],
        &[],
        first_half + NOTES_DEBOUNCE_WINDOW,
    )));
    seen.extend(externals(fed.holds_pass()));

    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(
        told.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Moved],
        "a rename delivered in two halves is still one move, saw {seen:?}"
    );
    assert_eq!(
        change_of(told[0]).1.map(std::path::Path::new),
        Some(after.as_path())
    );
    assert!(
        !state.is_removed_on_disk(&doc.id),
        "the tab stopped writing to a file that is there"
    );
    assert_eq!(note_file(&state, &doc.id), after);

    save_buffer_content_inner(&state, &doc.id, "edited after the rename").expect("save");
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the moved file"),
        "edited after the rename"
    );
}

#[test]
fn showing_both_leaves_a_file_the_second_tab_can_open() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (id, path) = note_changed_under_jotra(&state, "what I typed", "what they wrote");

    let outcome =
        resolve_external_change_inner(&state, &id, ChangeChoice::KeepBoth, "what I typed, still")
            .expect("resolve");

    let copy = PathBuf::from(outcome.conflict_copy_path.expect("a copy to open"));
    assert!(copy.exists(), "{}", copy.display());
    assert_eq!(
        std::fs::read_to_string(&copy).expect("read"),
        "what I typed, still"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "what they wrote"
    );
    assert_eq!(outcome.content.as_deref(), Some("what they wrote"));

    // The second tab is an ordinary note opened from an ordinary file. The
    // copy is written beside the note and holds its bytes, so it opens a
    // buffer rather than the dataless arm that carries no row.
    let opened = open_file_from_path(&state, &copy.to_string_lossy()).expect("open the copy");
    let doc = opened.doc.expect("the copy carries a buffer row");
    assert_eq!(
        read_buffer_content_inner(&state, &doc.id).expect("read"),
        b"what I typed, still".to_vec()
    );
}

#[test]
fn two_texts_that_are_the_same_text_leave_no_copy_behind() {
    // The dirty predicate fails closed, so the question is asked over a note
    // whose document has not been hashed yet. Answering it must not put a
    // duplicate of the note in the folder.
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (id, path) = note_changed_under_jotra(&state, "one line\n", "one line\n");

    for choice in [
        ChangeChoice::KeepMine,
        ChangeChoice::UseDisk,
        ChangeChoice::KeepBoth,
    ] {
        let outcome =
            resolve_external_change_inner(&state, &id, choice, "one line\n").expect("resolve");
        assert_eq!(outcome.conflict_copy_path, None, "{choice:?}");
        assert_eq!(outcome.content, None, "{choice:?}");
        assert_eq!(
            outcome.disk_hash,
            jotra_core::hash::comparison_digest_hex(b"one line\n")
        );
    }
    assert_eq!(copies_beside(&state.notes_root()), Vec::<PathBuf>::new());
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "one line\n");
}

#[test]
fn the_copy_of_the_document_carries_the_note_s_own_line_ending() {
    // The editor hands its text over in LF whatever the file uses, so a copy
    // written straight out of it turns a Windows note's afternoon of typing
    // into a file none of that note's other lines match.
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    let path = note_file(&state, &doc.id);

    // The row learns the ending the way it does in the app: off a file that
    // was read.
    std::fs::write(&path, "one\r\ntwo\r\n").expect("a file written on Windows");
    read_buffer_content_inner(&state, &doc.id).expect("read");
    std::fs::write(&path, "what they wrote\r\n").expect("the other program's write");

    resolve_external_change_inner(&state, &doc.id, ChangeChoice::UseDisk, "one\ntwo\nthree\n")
        .expect("resolve");

    let copies = copies_beside(&state.notes_root());
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        std::fs::read_to_string(&copies[0]).expect("read"),
        "one\r\ntwo\r\nthree\r\n",
        "the copy holds the document's text in the note's own line ending"
    );
}

#[test]
fn the_copy_of_an_lf_note_gains_no_carriage_returns() {
    // The other half of the pin: the ending is the note's and not a constant,
    // so a note that never had a carriage return does not acquire one.
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (id, _) = note_changed_under_jotra(&state, "one\ntwo\n", "what they wrote\n");

    resolve_external_change_inner(&state, &id, ChangeChoice::UseDisk, "one\ntwo\nthree\n")
        .expect("resolve");

    let copies = copies_beside(&state.notes_root());
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        std::fs::read_to_string(&copies[0]).expect("read"),
        "one\ntwo\nthree\n"
    );
}

#[test]
fn the_copy_of_the_file_keeps_the_bytes_the_file_was_found_holding() {
    // The copy of the disk side is not re-encoded. The recorded ending is the
    // note's, and the program that just rewrote this file used whichever one
    // it liked; a copy meant to preserve what was found there has to preserve
    // it exactly.
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let doc = new_note_inner(&state).expect("new note");
    let path = note_file(&state, &doc.id);
    std::fs::write(&path, "one\r\ntwo\r\n").expect("a file written on Windows");
    read_buffer_content_inner(&state, &doc.id).expect("read");
    std::fs::write(&path, "they wrote\nthis in lf\n").expect("the other program's write");

    resolve_external_change_inner(&state, &doc.id, ChangeChoice::KeepMine, "one\ntwo\n")
        .expect("resolve");

    let copies = copies_beside(&state.notes_root());
    assert_eq!(copies.len(), 1, "{copies:?}");
    assert_eq!(
        std::fs::read_to_string(&copies[0]).expect("read"),
        "they wrote\nthis in lf\n"
    );
}

#[test]
fn a_file_that_is_not_downloaded_is_refused_without_being_read() {
    // Reading an evicted file is what makes the provider fetch it over the
    // network (ADR-028 §5), and this is a path that reads the file twice over.
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (id, path) = note_changed_under_jotra(&state, "what I typed", "what they wrote");

    let error = resolve_external_change_at(
        &state,
        &id,
        &path,
        Some(SF_DATALESS),
        LineEnding::Lf,
        TextEncoding::utf8(),
        ChangeChoice::KeepMine,
        "what I typed, still",
    )
    .expect_err("a file that is not there to read");

    assert!(error.starts_with(ERR_FILE_NOT_DOWNLOADED), "{error}");
    assert_eq!(copies_beside(&state.notes_root()), Vec::<PathBuf>::new());
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "what they wrote"
    );
}

#[cfg(unix)]
#[test]
fn a_copy_that_cannot_be_written_stops_the_note_being_touched() {
    // The order is the guarantee: a write that lands and then fails to be
    // copied has already destroyed the text it covered.
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);
    let (id, path) = note_changed_under_jotra(&state, "what I typed", "what they wrote");
    let folder = state.notes_root();
    let before = std::fs::metadata(&folder).expect("metadata").permissions();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).expect("read-only");

    let error =
        resolve_external_change_inner(&state, &id, ChangeChoice::KeepMine, "what I typed, still")
            .expect_err("nowhere to write the copy");

    std::fs::set_permissions(&folder, before).expect("restore");
    // The folder is what refuses, not the file, and the answer stops there
    // with the note untouched either way.
    assert!(error.starts_with(ERR_FOLDER_NOT_WRITABLE), "{error}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "what they wrote",
        "the note was written over with no copy of what it held"
    );
    assert_eq!(copies_beside(&folder), Vec::<PathBuf>::new());
}

#[test]
fn a_removal_nothing_answers_is_announced_when_the_hold_passes() {
    // The other side of the hold: waiting for a second delivery must not cost
    // the deletion itself. Nothing turns up carrying the file's id, so the
    // removal is announced once its window passes, and the tab is marked off
    // its file at that point and not before.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note the way Finder does");
    let deleted_at = Instant::now();
    let mut seen = held_at(&mut fed, &state, &root, &path, &doc.id, deleted_at);
    assert!(
        !state.is_removed_on_disk(&doc.id),
        "the tab was marked off its file before the hold passed"
    );
    let due = fed
        .next_deadline()
        .expect("the removal is waiting on a deadline");
    assert!(
        due - deleted_at >= hold_window(NOTES_DEBOUNCE_WINDOW),
        "the removal was announced without waiting out the hold"
    );
    seen.extend(externals(fed.holds_pass()));

    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(told.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(change_of(told[0]), (&ExternalChange::Removed, None));
    assert!(state.is_removed_on_disk(&doc.id));

    let refused = save_buffer_content_inner(&state, &doc.id, "text worth keeping, edited")
        .expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the refusal has to carry its own code, got {refused}"
    );
}

/// What a save is allowed to take when it lands inside a hold: the hold itself
/// plus the window the announcement is delivered on.
fn the_longest_a_held_save_may_wait() -> Duration {
    hold_window(NOTES_DEBOUNCE_WINDOW) + NOTES_DEBOUNCE_WINDOW
}

/// How far apart the answer that releases a held save and the announcement of
/// the same removal may land. Both come out of one pass over the expired hold.
/// A save the watcher never answers waits out its own deadline instead, a
/// debounce window behind the announcement.
fn released_together() -> Duration {
    NOTES_DEBOUNCE_WINDOW / 2
}

/// The two windows decision 14's consequences bullet quotes by number, read
/// off the constants the watcher runs on. A failure here is the documented
/// numbers having gone stale, not the watcher having gone wrong.
#[test]
fn the_adr_quotes_the_windows_the_notes_watcher_runs_on() {
    assert_eq!(NOTES_DEBOUNCE_WINDOW, Duration::from_millis(500));
    assert_eq!(
        hold_window(NOTES_DEBOUNCE_WINDOW),
        Duration::from_millis(1000)
    );
}

#[test]
fn a_save_inside_the_hold_of_a_deletion_recreates_nothing() {
    // The hold is the window in which the record still says the note has a
    // file, so nothing in it refuses a write on its own. A save landing there
    // would put the deleted file back holding the tab's text, and in a synced
    // folder put it back on every device (ADR-033 §12).
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note the way Finder does");
    let mut seen = held_at(&mut fed, &state, &root, &path, &doc.id, Instant::now());

    let id = doc.id.clone();
    let saving = waiting_on_hold(&state, &doc.id, move |state| {
        save_buffer_content_inner(state, &id, "written during the hold")
    });
    seen.extend(externals(fed.holds_pass()));
    let refused = released(&saving).expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "a save inside the hold is refused the same way as one after the announcement, got {refused}"
    );
    assert!(
        !path.exists(),
        "the file the person deleted is back on disk"
    );

    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(
        told.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Removed],
        "the deletion is announced once and the save says nothing, saw {seen:?}"
    );
    assert!(state.is_removed_on_disk(&doc.id));
}

#[test]
fn an_answer_inside_the_hold_of_a_deletion_puts_nothing_back() {
    // An answer is a write, so it waits out a held removal like a save does.
    // Without the wait it takes the record at face value, finds a note that
    // still has a file, and recreates the file the person deleted holding the
    // text they were being asked about.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note the way Finder does");
    held_at(&mut fed, &state, &root, &path, &doc.id, Instant::now());

    let id = doc.id.clone();
    let answering = waiting_on_hold(&state, &doc.id, move |state| {
        resolve_external_change_inner(state, &id, ChangeChoice::KeepMine, "answered anyway")
    });
    fed.holds_pass();
    let refused = released(&answering).expect_err("the answer must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the answer is refused the same way a save is, got {refused}"
    );
    assert!(
        !path.exists(),
        "the file the person deleted is back on disk"
    );
    assert_eq!(copies_beside(&state.notes_root()), Vec::<PathBuf>::new());
    assert!(state.is_removed_on_disk(&doc.id));
}

#[test]
fn an_answer_inside_the_hold_of_a_split_rename_lands_at_the_new_path() {
    // The other half of the wait, and the reason it sits above the store lock:
    // the answer reads the note's path after the move has landed on the row,
    // so a rename under an unanswered question leaves one file and not two.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    let elsewhere = dir.path().join("outside-every-watched-folder");
    std::fs::create_dir_all(&elsewhere).expect("a folder nothing watches");
    let parked = elsewhere.join("in-flight.md");
    std::fs::rename(&before, &parked).expect("the first half");
    held_at(&mut fed, &state, &root, &before, &doc.id, Instant::now());

    let id = doc.id.clone();
    let answering = waiting_on_hold(&state, &doc.id, move |state| {
        resolve_external_change_inner(state, &id, ChangeChoice::KeepMine, "answered anyway")
    });
    let after = root.join("renamed-under-the-question.md");
    std::fs::rename(&parked, &after).expect("the second half");
    fed.platform_reports(&[&root], &[]);
    let outcome = released(&answering).expect("the answer");

    assert_eq!(note_file(&state, &doc.id), after);
    assert_eq!(
        std::fs::read_to_string(&after).expect("read"),
        "answered anyway"
    );
    assert!(
        !before.exists(),
        "the answer wrote a second file at the old path"
    );
    let copies = copies_beside(&state.notes_root());
    assert_eq!(copies.len(), 1, "what the file held has to be beside it");
    assert_eq!(
        outcome.conflict_copy_path.as_deref(),
        Some(copies[0].to_string_lossy().as_ref())
    );
}

#[test]
fn a_save_inside_the_hold_of_a_split_rename_lands_at_the_new_path() {
    // The variant that costs a file rather than resurrecting one: the save
    // writes a new file at the path the rename emptied, the move is never
    // announced, and the person is left with two files where they renamed one.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    let elsewhere = dir.path().join("outside-every-watched-folder");
    std::fs::create_dir_all(&elsewhere).expect("a folder nothing watches");
    let parked = elsewhere.join("in-flight.md");
    std::fs::rename(&before, &parked).expect("the first half");
    let mut seen = held_at(&mut fed, &state, &root, &before, &doc.id, Instant::now());

    // The save blocks until the hold is answered, and the second half of the
    // rename is what answers it.
    let id = doc.id.clone();
    let saving = waiting_on_hold(&state, &doc.id, move |state| {
        save_buffer_content_inner(state, &id, "written during the hold")
    });
    let after = root.join("renamed-across-two-windows.md");
    std::fs::rename(&parked, &after).expect("the second half");
    seen.extend(externals(fed.platform_reports(&[&root], &[])));
    released(&saving).expect("the save");

    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(
        told.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Moved],
        "the rename is still one move with a save inside it, saw {seen:?}"
    );
    assert_eq!(note_file(&state, &doc.id), after);
    assert!(
        !before.exists(),
        "the save put a second file where the rename emptied"
    );
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the moved file"),
        "written during the hold",
        "the text typed during the hold has to reach the file the note is on"
    );
}

#[test]
fn a_stranger_at_the_path_during_the_hold_is_still_a_deletion() {
    // Something else at the path is not the note's file coming back. Reading
    // the path alone cannot tell them apart, and forgetting the hold for a
    // stranger leaves the tab bound to a file it never opened.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note the way Finder does");
    let deleted_at = Instant::now();
    let mut seen = held_at(&mut fed, &state, &root, &path, &doc.id, deleted_at);
    // A new file at the same path: another program, a sync client, a restore.
    std::fs::write(&path, b"somebody else's file").expect("the stranger's write");
    seen.extend(externals(fed.platform_reports_at(
        &[&root],
        &[],
        deleted_at + NOTES_DEBOUNCE_WINDOW,
    )));
    seen.extend(externals(fed.holds_pass()));
    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert!(
        told.iter()
            .any(|e| change_of(e).0 == &ExternalChange::Removed),
        "the note's own file is gone and nothing said so, saw {seen:?}"
    );
    assert!(
        !told
            .iter()
            .any(|e| change_of(e).0 == &ExternalChange::Moved),
        "the tab followed a file it never opened, saw {seen:?}"
    );
    assert!(state.is_removed_on_disk(&doc.id));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the stranger's file"),
        "somebody else's file",
        "the stranger's file was written over"
    );
}

#[test]
fn the_same_file_back_at_its_path_during_the_hold_lets_the_save_through() {
    // A rename out and straight back: the file the record names is at the path
    // it left, so there was never anything to announce, and the save that was
    // waiting on the hold lands where it always would have.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    let elsewhere = dir.path().join("outside-every-watched-folder");
    std::fs::create_dir_all(&elsewhere).expect("a folder nothing watches");
    let parked = elsewhere.join("in-flight.md");
    std::fs::rename(&path, &parked).expect("out of the folder");
    let mut seen = held_at(&mut fed, &state, &root, &path, &doc.id, Instant::now());

    let id = doc.id.clone();
    let saving = waiting_on_hold(&state, &doc.id, move |state| {
        save_buffer_content_inner(state, &id, "written while the file was away")
    });
    std::fs::rename(&parked, &path).expect("and back to the same path");
    seen.extend(externals(fed.platform_reports(&[&root], &[])));
    released(&saving).expect("the save");
    seen.extend(externals(fed.holds_pass()));
    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert!(
        !told
            .iter()
            .any(|e| change_of(e).0 == &ExternalChange::Removed),
        "a file that never left was announced as deleted, saw {seen:?}"
    );
    assert!(!state.is_removed_on_disk(&doc.id));
    assert_eq!(note_file(&state, &doc.id), path);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the note"),
        "written while the file was away"
    );
}

#[test]
fn a_save_inside_a_hold_waits_for_the_answer_and_no_longer() {
    // Three bounds. Below: the save cannot write before the hold is answered,
    // or it writes to a path nobody can classify yet. Above: the wait ends on
    // the hold's own deadline, so a note whose watcher stopped costs a save one
    // window and not a hung tab. Between: the answer that releases the save and
    // the announcement of the removal come out of the same pass over the
    // expired hold, so they land together, and a save that is instead left to
    // time out lands a debounce window behind the announcement.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    std::fs::remove_file(&path).expect("delete the note the way Finder does");
    let window = hold_window(NOTES_DEBOUNCE_WINDOW);
    let delivered_at = Instant::now();
    held_at(&mut fed, &state, &root, &path, &doc.id, delivered_at);
    // The hold is taken while the delivery is read, so it starts inside this.
    let read_in = delivered_at.elapsed();
    let announced_at = fed
        .next_deadline()
        .expect("the removal is waiting on a deadline");
    let gives_up_at = state
        .removal_holds
        .answer_by(&doc.id)
        .expect("a save has a deadline to wait to");
    let held_for = announced_at.duration_since(delivered_at);
    assert!(
        held_for >= window,
        "the save can be released before the hold it is waiting on could end"
    );
    assert!(
        held_for <= window + read_in,
        "the removal is held {held_for:?}, longer than its window"
    );
    let behind = gives_up_at.saturating_duration_since(announced_at);
    assert!(
        window + behind <= the_longest_a_held_save_may_wait(),
        "a save inside a hold waits {:?}, longer than the hold and one window",
        window + behind
    );
    assert!(
        behind > released_together(),
        "a save left to its own deadline would come back with the announcement, so returning \
         with it does not show the answer released it"
    );

    // Blocked on the hold until the announcement, whose pass over the expired
    // hold is what answers it; its own deadline is a minute away.
    let id = doc.id.clone();
    let saving = waiting_on_hold(&state, &doc.id, move |state| {
        save_buffer_content_inner(state, &id, "written during the hold")
    });
    let told = externals(fed.holds_pass());
    assert!(
        told.iter().any(|event| named_by(event).0 == doc.id),
        "the hold passed and the removal was not announced, saw {told:?}"
    );
    let refused = released(&saving).expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the save returned on the answer, not on a timeout that let it through: {refused}"
    );
}

/// The date a file was created, where the platform records one.
///
/// `None` off macOS, which is the only platform whose saves carry the date
/// across the rename that replaces the file.
#[cfg(target_os = "macos")]
fn birth_time(path: &std::path::Path) -> Option<(i64, i64)> {
    use std::os::macos::fs::MetadataExt;

    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.st_birthtime(), metadata.st_birthtime_nsec()))
}

#[cfg(not(target_os = "macos"))]
fn birth_time(_path: &std::path::Path) -> Option<(i64, i64)> {
    None
}

#[test]
fn a_save_of_the_text_the_file_already_holds_still_waits_out_the_hold() {
    // The seam between the wait and the guard that stops a save of unchanged
    // text. The wait comes first and has to: the guard answers on the file at
    // the path the row names, and inside a hold that path is empty, so a
    // reader that went first would call a no-op save a change and write.
    // Waiting costs an untouched note the hold once; not waiting costs it the
    // deleted file back.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    let written_at = std::fs::metadata(&path)
        .expect("stat the note")
        .modified()
        .expect("the modification time");
    delivered_apart(&mut fed, &root);

    let elsewhere = dir.path().join("outside-every-watched-folder");
    std::fs::create_dir_all(&elsewhere).expect("a folder nothing watches");
    let parked = elsewhere.join("in-flight.md");
    std::fs::rename(&path, &parked).expect("out of the folder");
    held_at(&mut fed, &state, &root, &path, &doc.id, Instant::now());

    // A save that skipped the hold comes back before it is seen waiting, and
    // one still waiting when the file returns cannot have read the emptied
    // path.
    let id = doc.id.clone();
    let saving = waiting_on_hold(&state, &doc.id, move |state| {
        save_buffer_content_inner(state, &id, "text worth keeping")
    });
    std::fs::rename(&parked, &path).expect("and back to the same path");
    assert!(
        saving.try_recv().is_err(),
        "the save came back before the file did, so it read a path the hold had emptied"
    );
    fed.platform_reports(&[&root], &[]);
    released(&saving).expect("the save");
    assert_eq!(
        std::fs::metadata(&path)
            .expect("stat the note")
            .modified()
            .expect("the modification time"),
        written_at,
        "a save of the text the file already holds replaced the file"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the note"),
        "text worth keeping"
    );
}

#[test]
fn a_save_inside_a_hold_carries_the_files_dates_to_the_path_it_lands_on() {
    // A write the hold released is a write like any other: it lands at the
    // path the answer moved the row to, through the guard that carries what
    // the file was carrying. A note started last year that is renamed and
    // typed into in the same second is still a note started last year.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    let elsewhere = dir.path().join("outside-every-watched-folder");
    std::fs::create_dir_all(&elsewhere).expect("a folder nothing watches");
    let parked = elsewhere.join("in-flight.md");
    std::fs::rename(&before, &parked).expect("the first half");
    // Read off the file itself while it is parked: a rename carries the date
    // with the inode, so this is the date the note has had all along.
    let born = birth_time(&parked);
    held_at(&mut fed, &state, &root, &before, &doc.id, Instant::now());

    let id = doc.id.clone();
    let saving = waiting_on_hold(&state, &doc.id, move |state| {
        save_buffer_content_inner(state, &id, "written during the hold")
    });
    let after = root.join("carried-across-the-hold.md");
    std::fs::rename(&parked, &after).expect("the second half");
    fed.platform_reports(&[&root], &[]);
    released(&saving).expect("the save");

    assert_eq!(note_file(&state, &doc.id), after);
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the moved file"),
        "written during the hold",
        "the text typed during the hold has to reach the file the note is on"
    );
    assert!(!before.exists(), "the save put a second file back");
    assert_eq!(
        birth_time(&after),
        born,
        "the write the hold released reports the note as created moments ago"
    );
}

#[cfg(unix)]
#[test]
fn a_folder_that_will_not_take_the_write_leaves_the_hold_where_it_was() {
    // The save that reaches the folder is the one whose hold expired with no
    // answer, and a hold with no answer is still the watcher's to announce.
    // Taking it away with the refusal would leave the removal with nothing to
    // come out of, and the tab would never hear that its file had gone.
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    let folder = path
        .parent()
        .expect("the folder holding the note")
        .to_path_buf();

    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let probe = folder.join("writable.probe");
    if std::fs::write(&probe, b"").is_ok() {
        // Root, for whom no folder refuses a write.
        std::fs::remove_file(&probe).expect("remove the probe");
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        return;
    }

    // A hold nothing will answer: the save waits out its deadline and then
    // writes, which is the one order in which the folder is reached at all.
    state
        .removal_holds
        .hold(&doc.id, Instant::now() + Duration::from_millis(300));

    let answer = save_buffer_content_inner(&state, &doc.id, "written during the hold");
    // Before anything that can panic, so a failure leaves a folder the temp
    // directory can still delete itself out of.
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    let refused = answer.expect_err("the folder must stop the save");

    assert!(
        refused.starts_with(ERR_FOLDER_NOT_WRITABLE),
        "the save that waited out the hold was refused as something else: {refused}"
    );
    assert!(
        state.removal_holds.holds(&doc.id),
        "the refusal took the hold with it, leaving the removal with nothing to come out of"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read the note"),
        "text worth keeping",
        "a refused save wrote anyway"
    );
}

/// Sets a file's modification time to the start of 2020, the way a metadata
/// pass over a restored folder does.
///
/// On APFS that pulls the birth time back with it, on the same inode, which is
/// the whole point of the tests below.
#[cfg(target_os = "macos")]
fn backdate(path: &std::path::Path) {
    let status = std::process::Command::new("touch")
        .arg("-t")
        .arg("202001010000")
        .arg(path)
        .status()
        .expect("run touch");
    assert!(status.success(), "touch -t failed on {}", path.display());
}

#[test]
#[cfg(target_os = "macos")]
fn a_note_whose_birth_time_was_pulled_back_still_follows_its_rename() {
    // A metadata pass that backdates a note in place — `touch -t`, `SetFile
    // -d`, a restore tool — leaves the inode alone and, on APFS, drags the
    // birth time down to the modification time it set. It writes no bytes, so
    // the ignore stamp Jotra's own save left takes the event for Jotra's and
    // nothing refreshes the id. The record then holds a birth time later than
    // the file's, and reading that as a different file leaves the tab refusing
    // every save over a file sitting at its new name (ADR-033 §12).
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    backdate(&before);
    delivered_apart(&mut fed, &root);

    let after = root.join("renamed-after-the-backdate.md");
    std::fs::rename(&before, &after).expect("rename the way Finder does");

    let seen = reported(&mut fed, &[&root], &[&before]);
    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(
        told.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Moved],
        "a rename after a backdating touch is still a move, saw {seen:?}"
    );
    assert_eq!(
        change_of(told[0]).1.map(std::path::Path::new),
        Some(after.as_path())
    );
    assert!(
        !state.is_removed_on_disk(&doc.id),
        "the tab stopped writing to a file that is there"
    );
    assert_eq!(note_file(&state, &doc.id), after);

    save_buffer_content_inner(&state, &doc.id, "edited after the backdate").expect("save");
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the moved file"),
        "edited after the backdate"
    );
    assert!(
        !before.exists(),
        "the save recreated the file at the old path"
    );
}

/// Writes a file's creation date forward, the way an unarchiver or a restore
/// tool stamping an archive's own date does.
///
/// `SetFile -d` reaches `setattrlist` with `ATTR_CMN_CRTIME`, which moves a
/// live inode's birth time in either direction on APFS. It is the mirror of
/// [`backdate`], and the pair is why macOS reads no birth time: a value that
/// moves both ways cannot be compared against a record to tell a reused inode
/// number from the file that still holds it.
#[cfg(target_os = "macos")]
fn forward_date(path: &std::path::Path) {
    let status = std::process::Command::new("/usr/bin/SetFile")
        .arg("-d")
        .arg("01/01/2090 00:00:00")
        .arg(path)
        .status()
        .expect("run SetFile");
    assert!(status.success(), "SetFile -d failed on {}", path.display());
}

#[test]
#[cfg(target_os = "macos")]
fn a_note_whose_birth_time_was_pushed_forward_still_follows_its_rename() {
    // The mirror of the test above. A restore tool stamping the creation date
    // it read out of an archive writes the birth time forward on the inode the
    // tab is editing, and writes no bytes, so the ignore stamp Jotra's own save
    // left takes the event for Jotra's and nothing refreshes the id. A record
    // that compared birth times would read the file as a different one and
    // leave the tab refusing every save over a file at its new name.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let before = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    forward_date(&before);
    delivered_apart(&mut fed, &root);

    let after = root.join("renamed-after-the-forward-date.md");
    std::fs::rename(&before, &after).expect("rename the way Finder does");

    let seen = reported(&mut fed, &[&root], &[&before]);
    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(
        told.iter().map(|e| change_of(e).0).collect::<Vec<_>>(),
        vec![&ExternalChange::Moved],
        "a rename after a forward-dating SetFile is still a move, saw {seen:?}"
    );
    assert_eq!(
        change_of(told[0]).1.map(std::path::Path::new),
        Some(after.as_path())
    );
    assert!(
        !state.is_removed_on_disk(&doc.id),
        "the tab stopped writing to a file that is there"
    );
    assert_eq!(note_file(&state, &doc.id), after);

    save_buffer_content_inner(&state, &doc.id, "edited after the forward date").expect("save");
    assert_eq!(
        std::fs::read_to_string(&after).expect("read the moved file"),
        "edited after the forward date"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn a_backdated_note_that_is_deleted_is_still_gone() {
    // The control for the test above. Reading an earlier birth time as the
    // same file must not soften the deletion: the note goes, another file is
    // created in the same window, and that file is younger than the record
    // whatever the record's birth time was pulled back to. Anything else puts
    // the tab on a file nobody opened.
    let dir = TempDir::new().expect("temp dir");
    let (state, mut fed) = fed_state(&dir);
    let root = state.notes_root();

    let doc = new_note_inner(&state).expect("new note");
    save_buffer_content_inner(&state, &doc.id, "text worth keeping").expect("save");
    let path = note_file(&state, &doc.id);
    delivered_apart(&mut fed, &root);

    backdate(&path);
    delivered_apart(&mut fed, &root);

    let newcomer = root.join("aaa-created-in-the-same-window.md");
    std::fs::write(&newcomer, "text worth keeping").expect("the newcomer");
    std::fs::remove_file(&path).expect("delete the note the way Finder does");

    let seen = reported(&mut fed, &[&root], &[&path]);
    let told: Vec<_> = seen.iter().filter(|e| named_by(e).0 == doc.id).collect();
    assert_eq!(told.len(), 1, "the tab must be told once, saw {seen:?}");
    assert_eq!(change_of(told[0]), (&ExternalChange::Removed, None));

    let refused = save_buffer_content_inner(&state, &doc.id, "text worth keeping, edited")
        .expect_err("the save must be refused");
    assert!(
        refused.starts_with(ERR_FILE_REMOVED_ON_DISK),
        "the refusal has to carry its own code, got {refused}"
    );
    assert_eq!(
        std::fs::read_to_string(&newcomer).expect("read the newcomer"),
        "text worth keeping",
        "the save wrote over a file the note was never in"
    );
}

/// The frontend reaches these by name through `invoke`, so a command dropped
/// from the handler compiles and every test here still passes.
#[test]
fn every_note_command_is_registered() {
    const LIB_RS: &str = include_str!("../src/lib.rs");

    for command in [
        "commands::notes::new_note,",
        "commands::notes::new_note_in_folder,",
        "commands::notes::new_note_from_link,",
        "commands::notes::rename_note,",
        "commands::notes::delete_note,",
        "commands::notes::undo_delete_note,",
        "commands::notes::save_note_copy,",
        "commands::notes::show_note_in_file_manager,",
        "commands::notes::show_notes_file_in_file_manager,",
        "commands::notes::get_notes_root,",
        "commands::notes::get_notes_folder,",
        "commands::notes::pick_notes_folder,",
    ] {
        assert!(
            LIB_RS.contains(command),
            "{} is not in the invoke handler, so the frontend cannot call it",
            command.trim_end_matches(',')
        );
    }
}
