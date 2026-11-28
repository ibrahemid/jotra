//! The preview-scripts checks against a file whose reads never return.
//!
//! A tab open on a network share whose server went away holds a file nothing
//! can read: a read blocks until the share answers. A FIFO with no writer is
//! the same file on any Unix machine, as in `heartbeat_stall_tests.rs`. The
//! render of an opted-in document and the status bar's read of its opt-in
//! compare digests Jotra already holds (ADR-043), so neither reads the file,
//! and neither holds the store across a read that a save of another tab would
//! then wait behind.

#![cfg(unix)]

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use jotra_core::preview::RenderRequest;
use jotra_tauri_lib::commands::buffer::save_buffer_content_inner;
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::commands::preview::{
    preview_get_scripts_inner, preview_set_scripts_inner, render_into_cache, CachedRender,
};
use jotra_tauri_lib::preview::renderers::HtmlRenderer;
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

/// Long enough for a render, a save and a read on an idle machine many times
/// over, and short enough that a stalled run fails rather than hangs the
/// suite.
const DEADLINE: Duration = Duration::from_secs(5);

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// Opens `path`, holding `content`, the way the frontend reaches a file.
fn open_at(state: &AppState, path: &Path, content: &str) -> String {
    std::fs::write(path, content).expect("write");
    let canonical = canonicalize_for_authorization(path).expect("canonical");
    state.authorized_paths.record_for_open(canonical.clone());
    open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("the file opened")
        .id
}

/// Puts a FIFO nobody writes to where the file at `path` was.
fn stall(path: &Path) {
    std::fs::remove_file(path).expect("remove the file");
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed for {}", path.display());
}

/// Lets go of every read still waiting on `path`, so a run that stalled ends.
/// Each open of the write end releases the readers waiting then; it runs on a
/// thread of its own, since one with no reader waits for one.
fn release_readers(path: PathBuf) {
    std::thread::spawn(move || loop {
        drop(std::fs::OpenOptions::new().write(true).open(&path));
        std::thread::sleep(Duration::from_millis(20));
    });
}

/// Runs `call` on its own thread and answers whether it returned within the
/// deadline.
fn returns_on_its_own<T: Send + 'static>(
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

#[test]
fn an_opted_in_tab_on_a_stalled_file_holds_up_no_save_and_no_status_read() {
    let dir = TempDir::new().expect("temp dir");
    let state = Arc::new(make_state(&dir));
    state
        .preview_registry
        .write()
        .expect("registry")
        .register(Box::new(HtmlRenderer))
        .expect("register the HTML renderer");
    let share = dir.path().join("share");
    std::fs::create_dir_all(&share).expect("share folder");
    let stalled = share.join("Stalled.html");
    let body = "<p>approved</p><script>one()</script>";
    let stalled_id = open_at(&state, &stalled, body);
    preview_set_scripts_inner(&state, &stalled_id, true).expect("opt the file in");
    let typing = open_at(
        &state,
        &state.notes_root().join("Typing.md"),
        "first line\n",
    );
    stall(&stalled);

    let render_id = stalled_id.clone();
    let rendered = returns_on_its_own(&state, move |state| {
        render_into_cache(
            state,
            &render_id,
            RenderRequest {
                content_type: HtmlRenderer::content_type_id(),
                buffer_text: body.to_string(),
                theme: Default::default(),
                zoom: 1.0,
                assets: None,
            },
        )
    });
    let typing_id = typing.clone();
    let saved = returns_on_its_own(&state, move |state| {
        save_buffer_content_inner(state, &typing_id, "typed meanwhile\n")
    });
    let field_id = stalled_id.clone();
    let field = returns_on_its_own(&state, move |state| {
        preview_get_scripts_inner(state, &field_id)
    });

    let stalled_somewhere = rendered.is_none() || saved.is_none() || field.is_none();
    if stalled_somewhere {
        release_readers(stalled.clone());
    }
    assert!(
        rendered.is_some(),
        "the render of the opted-in tab read its file and waited on it"
    );
    assert!(
        saved.is_some(),
        "a save of another tab waited behind the opted-in tab's file"
    );
    assert!(
        field.is_some(),
        "the status bar's read of the opt-in waited on the file"
    );

    assert!(matches!(
        rendered,
        Some(CachedRender::Rendered {
            scripts_enabled: true,
            ..
        })
    ));
    assert_eq!(field, Some(Ok(true)));
    assert!(saved.expect("saved").is_ok());
    assert_eq!(
        std::fs::read_to_string(state.notes_root().join("Typing.md")).expect("read"),
        "typed meanwhile\n"
    );
}
