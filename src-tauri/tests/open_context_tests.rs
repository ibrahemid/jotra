//! What the app does with a `get_context` ask (ADR-053): it answers only for a
//! program the settings let see the open file, only from the main window, and
//! names the file from its own record of the tab.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::sync::atomic::Ordering;

use jotra_core::config::mcp::ClientApproval;
use jotra_core::instance::ContextAsk;
use jotra_core::open_context::{OpenContext, Selected, TextPoint};
use jotra_storage::open_context::take_answer;
use jotra_tauri_lib::commands::file::open_file_from_path;
use jotra_tauri_lib::open_context::{
    answer_inner, resolve_answer, route_ask, AskRoute, ContextAnswerError, ContextAsks,
    WindowContext,
};
use jotra_tauri_lib::security::canonicalize_for_authorization;
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

fn open(state: &AppState, path: &std::path::Path) -> String {
    let canonical = canonicalize_for_authorization(path).expect("canonical");
    open_file_from_path(state, &canonical)
        .expect("open")
        .doc
        .expect("an opened file has a row")
        .id
}

fn allow(state: &AppState, program: &str, context: bool) {
    let mut config = state.config.lock().expect("config");
    config.mcp.enabled = true;
    config.mcp.approved_clients = vec![ClientApproval {
        name: program.to_string(),
        first_seen: chrono::Utc::now(),
        read: true,
        write: true,
        context,
        review: false,
    }];
}

fn ask(token: &str, program: &str) -> ContextAsk {
    ContextAsk {
        token: token.to_string(),
        program: program.to_string(),
    }
}

fn selection() -> Selected {
    Selected {
        text: "launch the site".to_string(),
        from: TextPoint { line: 2, column: 1 },
        to: TextPoint {
            line: 2,
            column: 16,
        },
    }
}

#[test]
fn an_ask_is_answered_only_for_a_program_allowed_to_see_the_open_file() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    state.frontend_ready.store(true, Ordering::SeqCst);

    assert_eq!(
        route_ask(&state, &ask("t-1", "Claude Code")),
        AskRoute::Refused
    );

    allow(&state, "Claude Code", false);
    assert_eq!(
        route_ask(&state, &ask("t-1", "Claude Code")),
        AskRoute::Refused,
        "reading and writing do not carry it"
    );

    allow(&state, "Claude Code", true);
    assert_eq!(
        route_ask(&state, &ask("t-1", "Claude Code")),
        AskRoute::AskWindow
    );
    assert_eq!(route_ask(&state, &ask("t-1", "Codex")), AskRoute::Refused);
    assert_eq!(
        route_ask(&state, &ask("../config", "Claude Code")),
        AskRoute::Refused
    );

    state.config.lock().unwrap().mcp.enabled = false;
    assert_eq!(
        route_ask(&state, &ask("t-1", "Claude Code")),
        AskRoute::Refused
    );
}

#[test]
fn a_window_that_is_not_ready_answers_that_no_file_is_open() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    allow(&state, "Claude Code", true);
    assert_eq!(
        route_ask(&state, &ask("t-1", "Claude Code")),
        AskRoute::NothingOpen
    );
}

#[test]
fn the_file_is_named_from_the_apps_record_of_the_tab() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let file = state.notes_root().join("Plan.md");
    std::fs::write(&file, "# Plan\nlaunch the site\n").unwrap();
    let id = open(&state, &file);

    let answer = resolve_answer(
        &state,
        WindowContext {
            buffer_id: Some(id),
            selections: vec![selection()],
            cursor: Some(TextPoint {
                line: 2,
                column: 16,
            }),
            unsaved: true,
            truncated: false,
        },
    );
    let named = canonicalize_for_authorization(&file).unwrap();
    assert_eq!(answer.path.as_deref(), Some(named.as_str()));
    assert_eq!(answer.selections, vec![selection()]);
    assert!(answer.unsaved);

    let unknown = resolve_answer(
        &state,
        WindowContext {
            buffer_id: Some("not-a-tab".to_string()),
            selections: vec![selection()],
            cursor: None,
            unsaved: true,
            truncated: true,
        },
    );
    assert_eq!(unknown, OpenContext::nothing_open());
}

#[test]
fn only_the_main_window_answers_and_only_an_ask_still_waited_on() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let asks = ContextAsks::default();
    asks.hold("t-2", std::time::Instant::now());

    assert_eq!(
        answer_inner(&state, &asks, "capture", "t-2", WindowContext::default()),
        Err(ContextAnswerError::NotMainWindow)
    );
    assert_eq!(
        answer_inner(&state, &asks, "main", "t-9", WindowContext::default()),
        Err(ContextAnswerError::NotAsked)
    );
    assert_eq!(take_answer(&state.jotra_dir, "t-2"), None);

    answer_inner(&state, &asks, "main", "t-2", WindowContext::default()).expect("answered");
    assert_eq!(
        take_answer(&state.jotra_dir, "t-2"),
        Some(OpenContext::nothing_open())
    );
    assert_eq!(
        answer_inner(&state, &asks, "main", "t-2", WindowContext::default()),
        Err(ContextAnswerError::NotAsked),
        "an ask is answered once"
    );
}
