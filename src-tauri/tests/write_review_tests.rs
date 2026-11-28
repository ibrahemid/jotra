//! What the app does with a program's held write (ADR-053): it shows only a
//! write the settings hold for the person, answers only from the main window
//! while the write still waits, and turns an Apply into a discard for a
//! program that may no longer write.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::sync::atomic::Ordering;
use std::time::Instant;

use jotra_core::config::mcp::ClientApproval;
use jotra_core::instance::ReviewAsk;
use jotra_core::write_review::ReviewAnswer;
use jotra_storage::write_review::take_answer;
use jotra_tauri_lib::state::AppState;
use jotra_tauri_lib::write_review::{
    answer_inner, route_review, ReviewAnswerError, ReviewAsks, ReviewRoute,
};
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

fn set(state: &AppState, program: &str, write: bool, review: bool) {
    let mut config = state.config.lock().expect("config");
    config.mcp.enabled = true;
    config.mcp.approved_clients = vec![ClientApproval {
        name: program.to_string(),
        first_seen: chrono::Utc::now(),
        read: true,
        write,
        context: false,
        review,
    }];
}

fn ask(token: &str, program: &str) -> ReviewAsk {
    ReviewAsk {
        token: token.to_string(),
        program: program.to_string(),
    }
}

#[test]
fn a_held_write_is_shown_only_for_a_program_set_to_ask_first() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    state.frontend_ready.store(true, Ordering::SeqCst);

    assert_eq!(
        route_review(&state, &ask("r-1", "Claude Code")),
        ReviewRoute::NotHeld
    );

    set(&state, "Claude Code", true, false);
    assert_eq!(
        route_review(&state, &ask("r-1", "Claude Code")),
        ReviewRoute::NotHeld
    );

    set(&state, "Claude Code", false, true);
    assert_eq!(
        route_review(&state, &ask("r-1", "Claude Code")),
        ReviewRoute::NotHeld,
        "a program that cannot write has nothing to hold"
    );

    set(&state, "Claude Code", true, true);
    assert_eq!(
        route_review(&state, &ask("r-1", "Claude Code")),
        ReviewRoute::Show
    );
    assert_eq!(
        route_review(&state, &ask("r-1", "Codex")),
        ReviewRoute::NotHeld
    );
    assert_eq!(
        route_review(&state, &ask("../config", "Claude Code")),
        ReviewRoute::NotHeld
    );

    state.frontend_ready.store(false, Ordering::SeqCst);
    assert_eq!(
        route_review(&state, &ask("r-1", "Claude Code")),
        ReviewRoute::NotReady
    );

    state.frontend_ready.store(true, Ordering::SeqCst);
    state.config.lock().unwrap().mcp.enabled = false;
    assert_eq!(
        route_review(&state, &ask("r-1", "Claude Code")),
        ReviewRoute::NotHeld
    );
}

#[test]
fn only_the_main_window_answers_and_only_a_write_still_waiting() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    set(&state, "Claude Code", true, true);
    let asks = ReviewAsks::default();
    asks.hold(
        "r-2",
        "Claude Code",
        jotra_core::write_review::REVIEW_WAIT,
        Instant::now(),
    );

    assert_eq!(
        answer_inner(&state, &asks, "capture", "r-2", true),
        Err(ReviewAnswerError::NotMainWindow)
    );
    assert_eq!(
        answer_inner(&state, &asks, "main", "r-9", true),
        Err(ReviewAnswerError::NotAsked)
    );
    assert_eq!(take_answer(&state.jotra_dir, "r-2"), None);

    answer_inner(&state, &asks, "main", "r-2", true).expect("applied");
    assert_eq!(
        take_answer(&state.jotra_dir, "r-2"),
        Some(ReviewAnswer::Apply)
    );
    assert_eq!(
        answer_inner(&state, &asks, "main", "r-2", true),
        Err(ReviewAnswerError::NotAsked),
        "answered once"
    );

    asks.hold(
        "r-3",
        "Claude Code",
        jotra_core::write_review::REVIEW_WAIT,
        Instant::now(),
    );
    answer_inner(&state, &asks, "main", "r-3", false).expect("discarded");
    assert_eq!(
        take_answer(&state.jotra_dir, "r-3"),
        Some(ReviewAnswer::Discard)
    );
}

#[test]
fn an_apply_for_a_program_that_may_no_longer_write_is_a_discard() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    set(&state, "Claude Code", true, true);
    let asks = ReviewAsks::default();
    asks.hold(
        "r-4",
        "Claude Code",
        jotra_core::write_review::REVIEW_WAIT,
        Instant::now(),
    );

    set(&state, "Claude Code", false, true);
    answer_inner(&state, &asks, "main", "r-4", true).expect("answered");
    assert_eq!(
        take_answer(&state.jotra_dir, "r-4"),
        Some(ReviewAnswer::Discard)
    );
}
