//! A file another program changes on disk loses its preview-scripts opt-in, and
//! a save Jotra makes keeps it (ADR-043).
//!
//! The app's open-file watcher runs over a real folder with the app's tracking
//! and ignore set, and turns the opt-in off through that tracking before it
//! reports the change; the events it returns are what the frontend would hear.
//! The test hands it what the platform reports. A change no watcher sees, made
//! while the tab is closed or while Jotra is not running, is caught when the
//! editor reads the file back: the digest it records is not the one the opt-in
//! covers. Those cases run through the app's own close, History reopen, read
//! and render.

#[path = "support/app_state.rs"]
mod app_state;
#[path = "support/fed_watchers.rs"]
mod fed_watchers;

use app_state::AppStateBuilder;
use fed_watchers::FedWatchers;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use jotra_core::events::bus::JotraEvent;
use jotra_core::preview::RenderRequest;
use jotra_core::watcher::change_event::ExternalChange;
use jotra_tauri_lib::commands::buffer::{
    close_buffer_inner, read_buffer_content_inner, save_buffer_content_inner,
};
use jotra_tauri_lib::commands::chat::announce_note_change;
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::commands::history::restore_buffer_inner;
use jotra_tauri_lib::commands::preview::{
    lapse_scripts_opt_in_now, preview_get_scripts_inner, preview_set_scripts_inner,
    render_into_cache,
};
use jotra_tauri_lib::preview::handler::RenderedDoc;
use jotra_tauri_lib::preview::renderers::HtmlRenderer;
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

/// Long enough for a loaded machine; only a failing run waits it out.
const DELIVERY_BOUND: Duration = Duration::from_secs(15);

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// A state with the open-file watcher following its tabs through the app's
/// tracking, and the watcher the test hands deliveries to.
fn watching_state(dir: &TempDir) -> (Arc<AppState>, FedWatchers) {
    let state = Arc::new(make_state(dir));
    let fed = FedWatchers::install(&state);
    (state, fed)
}

/// `name` in `dir` opened as a tab, its scripts turned on and a render of it
/// cached with them on.
fn opted_in(state: &AppState, dir: &Path, name: &str, body: &str) -> (String, PathBuf) {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("seed the file");
    let canonical = canonicalize_for_authorization(&path).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    let id = open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("the file opened")
        .id;
    preview_set_scripts_inner(state, &id, true).expect("turn the scripts on");
    state.preview_render_cache.put(
        id.clone(),
        RenderedDoc {
            html: body.to_string(),
            assets: None,
            scripts_enabled: true,
        },
    );
    (id, PathBuf::from(canonical))
}

fn scripts_on(state: &AppState, id: &str) -> bool {
    preview_get_scripts_inner(state, id).expect("read the opt-in")
}

/// The opt-in as the row records it, before any check.
fn row_opted_in(state: &AppState, id: &str) -> bool {
    state
        .store
        .lock()
        .expect("store")
        .preview_scripts(id)
        .expect("read the row")
}

/// The read the editor makes of a tab's file when it shows it, which is what
/// records the bytes Jotra holds for it.
fn editor_reads(state: &AppState, id: &str) -> Vec<u8> {
    read_buffer_content_inner(state, id).expect("the editor reads the file")
}

fn cached_scripts_on(state: &AppState, id: &str) -> bool {
    state
        .preview_render_cache
        .get(id)
        .expect("a cached render")
        .scripts_enabled
}

/// Whether a render of `text` for `id`, through the preview's render path, is
/// cached with the file's scripts on.
fn rendered_with_scripts(state: &AppState, id: &str, text: &str) -> bool {
    {
        let mut registry = state.preview_registry.write().expect("registry");
        if registry.get(&HtmlRenderer::content_type_id()).is_none() {
            registry
                .register(Box::new(HtmlRenderer))
                .expect("register the HTML renderer");
        }
    }
    render_into_cache(
        state,
        id,
        RenderRequest {
            content_type: HtmlRenderer::content_type_id(),
            buffer_text: text.to_string(),
            theme: Default::default(),
            zoom: 1.0,
            assets: None,
        },
    );
    cached_scripts_on(state, id)
}

/// The changes `told` carries for `id`.
fn changes_to(told: &[JotraEvent], id: &str) -> Vec<ExternalChange> {
    told.iter()
        .filter_map(|event| match event {
            JotraEvent::BufferExternal {
                buffer_id, change, ..
            } if buffer_id == id => Some(change.clone()),
            _ => None,
        })
        .collect()
}

/// Delivers the platform's report of the file as it was opened in a batch of
/// its own, as time between the open and the next change gives it, so that
/// change is judged on what it did alone.
fn the_open_is_delivered_apart(fed: &mut FedWatchers, folder: &Path, id: &str) {
    let told = fed.platform_reports(&[folder], &[]);
    assert_eq!(
        changes_to(&told, id),
        Vec::<ExternalChange>::new(),
        "opening the file was reported as a change to it"
    );
}

fn write_by_temp_and_rename(path: &Path, bytes: &[u8]) {
    let temp = path.with_extension("jotra-test-tmp");
    std::fs::write(&temp, bytes).expect("write temp");
    std::fs::rename(&temp, path).expect("rename over target");
}

#[test]
fn a_file_another_program_rewrites_loses_its_scripts_opt_in() {
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let (state, mut fed) = watching_state(&dir);
    let (id, file) = opted_in(
        &state,
        downloads.path(),
        "report.html",
        "<p>read</p><script>one()</script>",
    );
    the_open_is_delivered_apart(&mut fed, downloads.path(), &id);

    write_by_temp_and_rename(&file, b"<p>downloaded again</p><script>two()</script>");
    let told = fed.platform_reports(&[downloads.path()], &[]);

    assert_eq!(changes_to(&told, &id), vec![ExternalChange::Modified]);
    assert!(
        !scripts_on(&state, &id),
        "the replaced file kept the opt-in given to the one it replaced"
    );
    assert!(
        !cached_scripts_on(&state, &id),
        "the cached render would still be served with the file's scripts on"
    );
}

#[test]
fn a_rewrite_is_reported_and_lapses_the_opt_in_while_a_command_holds_the_store() {
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let (state, mut fed) = watching_state(&dir);
    let (id, file) = opted_in(
        &state,
        downloads.path(),
        "report.html",
        "<p>read</p><script>one()</script>",
    );
    the_open_is_delivered_apart(&mut fed, downloads.path(), &id);

    let held = state.store.lock().expect("store");
    write_by_temp_and_rename(&file, b"<p>downloaded again</p><script>two()</script>");
    let (reported, told) = mpsc::channel();
    let folder = downloads.path().to_path_buf();
    let watcher = std::thread::spawn(move || {
        let _ = reported.send(fed.platform_reports(&[&folder], &[]));
    });
    let told = told.recv_timeout(DELIVERY_BOUND);
    drop(held);
    watcher.join().expect("the watcher's thread");

    let told = told.expect("the watcher waited on the store's lock to report the change");
    assert_eq!(changes_to(&told, &id), vec![ExternalChange::Modified]);
    assert!(!row_opted_in(&state, &id));
    assert!(!cached_scripts_on(&state, &id));
}

#[test]
fn a_save_jotra_makes_keeps_the_scripts_opt_in() {
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let (state, mut fed) = watching_state(&dir);
    let (id, file) = opted_in(&state, downloads.path(), "report.html", "<p>read</p>");
    the_open_is_delivered_apart(&mut fed, downloads.path(), &id);

    save_buffer_content_inner(&state, &id, "<p>edited in Jotra</p>").expect("save");
    let told = fed.platform_reports(&[downloads.path()], &[]);

    assert_eq!(
        std::fs::read_to_string(&file).expect("read the file"),
        "<p>edited in Jotra</p>"
    );
    assert_eq!(
        changes_to(&told, &id),
        Vec::<ExternalChange>::new(),
        "Jotra's own save came back as somebody else's change"
    );
    assert!(
        scripts_on(&state, &id),
        "a save Jotra made turned the scripts off"
    );
    assert!(cached_scripts_on(&state, &id));
}

#[test]
fn a_file_another_program_moves_keeps_its_scripts_opt_in() {
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let (state, mut fed) = watching_state(&dir);
    let (id, file) = opted_in(&state, downloads.path(), "report.html", "<p>read</p>");
    the_open_is_delivered_apart(&mut fed, downloads.path(), &id);

    std::fs::rename(&file, file.with_file_name("renamed.html")).expect("move the file");
    let told = fed.platform_reports(&[downloads.path()], &[&file]);

    assert_eq!(changes_to(&told, &id), vec![ExternalChange::Moved]);
    assert!(
        scripts_on(&state, &id),
        "a move changes no bytes, and it turned the scripts off"
    );
    assert!(cached_scripts_on(&state, &id));
}

#[test]
fn lapsing_answers_whether_the_scripts_were_on() {
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let state = make_state(&dir);
    let (id, _file) = opted_in(&state, downloads.path(), "report.html", "<p>read</p>");

    assert!(lapse_scripts_opt_in_now(&state, &id));
    assert!(!cached_scripts_on(&state, &id));
    assert!(!lapse_scripts_opt_in_now(&state, &id));
    assert!(!lapse_scripts_opt_in_now(&state, "no-such-buffer"));
    assert!(!scripts_on(&state, &id));
}

#[test]
fn a_proposal_the_chat_applies_to_an_opted_in_tab_turns_its_scripts_off() {
    // The pane writes a file open in a tab through the editor's own save,
    // which carries the opt-in over its write; the announcement is what takes
    // it back, as for any write that is not the editor's.
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let state = make_state(&dir);
    let (id, file) = opted_in(&state, downloads.path(), "report.html", "<p>read</p>");
    let proposed = "<p>read</p><script>fetch('/leak')</script>";

    save_buffer_content_inner(&state, &id, proposed).expect("the apply's save");
    assert!(
        row_opted_in(&state, &id),
        "the editor's save path carries the opt-in over the write"
    );
    announce_note_change(&state, &id, &file, proposed.as_bytes());

    assert!(!row_opted_in(&state, &id));
    assert!(!scripts_on(&state, &id));
    assert!(!cached_scripts_on(&state, &id));
    assert!(!rendered_with_scripts(&state, &id, proposed));
}

#[test]
fn a_file_another_program_rewrites_while_its_tab_is_closed_loses_its_scripts_opt_in() {
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let state = make_state(&dir);
    let (id, file) = opted_in(
        &state,
        downloads.path(),
        "report.html",
        "<p>read</p><script>one()</script>",
    );
    close_buffer_inner(&state, &id).expect("close the tab");

    write_by_temp_and_rename(&file, b"<p>downloaded again</p><script>two()</script>");

    restore_buffer_inner(&state, &id).expect("reopen from History");
    assert_eq!(
        editor_reads(&state, &id),
        b"<p>downloaded again</p><script>two()</script>"
    );
    assert!(
        !scripts_on(&state, &id),
        "the reopened file kept the opt-in given to the one it replaced"
    );
    assert!(!row_opted_in(&state, &id), "the lapse is recorded");
    assert!(!rendered_with_scripts(
        &state,
        &id,
        "<p>downloaded again</p><script>two()</script>"
    ));
}

#[test]
fn a_file_saved_in_jotra_keeps_its_scripts_opt_in_when_its_tab_is_closed_and_reopened() {
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let state = make_state(&dir);
    let (id, _file) = opted_in(&state, downloads.path(), "report.html", "<p>read</p>");

    save_buffer_content_inner(
        &state,
        &id,
        "<p>edited in Jotra</p><script>three()</script>",
    )
    .expect("save");
    close_buffer_inner(&state, &id).expect("close the tab");
    restore_buffer_inner(&state, &id).expect("reopen from History");
    editor_reads(&state, &id);

    assert!(scripts_on(&state, &id));
    assert!(rendered_with_scripts(
        &state,
        &id,
        "<p>edited in Jotra</p><script>three()</script>"
    ));
}

#[test]
fn a_file_changed_while_jotra_is_not_running_loses_its_scripts_opt_in() {
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let (id, file) = {
        let state = make_state(&dir);
        opted_in(
            &state,
            downloads.path(),
            "report.html",
            "<p>read</p><script>one()</script>",
        )
    };

    let relaunched = make_state(&dir);
    editor_reads(&relaunched, &id);
    assert!(
        scripts_on(&relaunched, &id),
        "a relaunch over a file nothing changed turned the scripts off"
    );
    drop(relaunched);

    write_by_temp_and_rename(&file, b"<p>downloaded again</p><script>two()</script>");

    let relaunched = make_state(&dir);
    assert!(
        !scripts_on(&relaunched, &id),
        "a relaunch holds none of the file's bytes until the editor reads them"
    );
    assert!(row_opted_in(&relaunched, &id));
    editor_reads(&relaunched, &id);
    assert!(!scripts_on(&relaunched, &id));
    assert!(!row_opted_in(&relaunched, &id), "the lapse is recorded");
    assert!(!rendered_with_scripts(
        &relaunched,
        &id,
        "<p>downloaded again</p><script>two()</script>"
    ));
}

#[test]
fn a_crlf_file_saved_and_read_back_keeps_its_scripts_opt_in() {
    // The opt-in's digest and the bytes Jotra records are both the file's raw
    // bytes: the open's, the save's after the file's own line ending goes back
    // on, and the editor's read. A file that keeps CRLF breaks none of them.
    let dir = TempDir::new().expect("temp dir");
    let downloads = TempDir::new().expect("a folder outside the notes folder");
    let state = make_state(&dir);
    let (id, file) = opted_in(
        &state,
        downloads.path(),
        "report.html",
        "<p>read</p>\r\n<script>one()</script>\r\n",
    );
    assert!(scripts_on(&state, &id));

    save_buffer_content_inner(&state, &id, "<p>edited</p>\n<script>two()</script>\n")
        .expect("save");
    assert_eq!(
        std::fs::read(&file).expect("read the file"),
        b"<p>edited</p>\r\n<script>two()</script>\r\n"
    );
    assert!(scripts_on(&state, &id));

    close_buffer_inner(&state, &id).expect("close the tab");
    restore_buffer_inner(&state, &id).expect("reopen from History");
    editor_reads(&state, &id);
    assert!(scripts_on(&state, &id));
    assert!(rendered_with_scripts(
        &state,
        &id,
        "<p>edited</p>\n<script>two()</script>\n"
    ));
}
