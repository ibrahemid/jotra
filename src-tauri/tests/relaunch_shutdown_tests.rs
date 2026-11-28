//! The shutdown a restart or an update install runs before the process goes.
//!
//! "Restart to update" used to restart without asking the window to write
//! what was typed inside the autosave debounce, and on Windows the installer
//! ends the process from inside the updater plugin. Both now run the quit's
//! handshake first: ask the frontend to flush, wait for its answer, then write
//! what Rust holds. The shutdown work itself (`finish_shutdown`) needs a live
//! `AppHandle`, so it is passed in, and these pin the order around it.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jotra_core::events::bus::JotraEvent;
use jotra_core::recovery::QUIT_FLUSH_TIMEOUT;
use jotra_tauri_lib::quit::QuitDecision;
use jotra_tauri_lib::relaunch::{shut_down_for_relaunch, RelaunchShutdown};
use jotra_tauri_lib::state::AppState;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

/// Answers the flush request the way the webview does: the editors write,
/// then `confirm_quit_flush` arrives, a moment later and from another thread.
fn answer_flush_requests(state: &AppState, log: Arc<Mutex<Vec<&'static str>>>) {
    let quit = state.quit.clone();
    state.event_bus.subscribe(move |event| {
        if !matches!(event, JotraEvent::FlushBeforeQuit) {
            return;
        }
        let quit = quit.clone();
        let log = log.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            log.lock().unwrap().push("editors flushed");
            quit.confirm_flush();
        });
    });
}

#[test]
fn a_relaunch_flushes_the_editors_before_the_shutdown_work() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    state.frontend_ready.store(true, Ordering::SeqCst);
    let log = Arc::new(Mutex::new(Vec::new()));
    answer_flush_requests(&state, log.clone());

    let started = Instant::now();
    let outcome = shut_down_for_relaunch(&state, || {
        log.lock().unwrap().push("finish_shutdown");
    });

    assert_eq!(outcome, RelaunchShutdown::Finished);
    assert_eq!(
        log.lock().unwrap().as_slice(),
        ["editors flushed", "finish_shutdown"],
        "the shutdown work ran before the editors had written"
    );
    assert!(
        started.elapsed() < QUIT_FLUSH_TIMEOUT,
        "the relaunch sat through the timeout after the answer arrived"
    );
    assert!(state.quit.is_complete());
    assert!(
        state.notes_index_cancel.load(Ordering::SeqCst),
        "the reconcile walk is left running into the relaunch"
    );
}

#[test]
fn a_relaunch_leaves_nothing_for_the_exit_path_to_redo() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    state.frontend_ready.store(true, Ordering::SeqCst);
    answer_flush_requests(&state, Arc::new(Mutex::new(Vec::new())));

    shut_down_for_relaunch(&state, || {});

    // The restart raises `ExitRequested` and `Exit` after this; both have to
    // find the work done rather than snapshot a second time.
    assert!(!state.quit.claim_final_shutdown());
    assert_eq!(
        state.quit.begin(Some(tauri::RESTART_EXIT_CODE)),
        QuitDecision::Proceed
    );
}

#[test]
fn a_relaunch_before_the_window_is_ready_shuts_down_without_asking() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    let asked = Arc::new(AtomicBool::new(false));
    let seen = asked.clone();
    state.event_bus.subscribe(move |event| {
        if matches!(event, JotraEvent::FlushBeforeQuit) {
            seen.store(true, Ordering::SeqCst);
        }
    });
    let ran = AtomicBool::new(false);

    let started = Instant::now();
    let outcome = shut_down_for_relaunch(&state, || ran.store(true, Ordering::SeqCst));

    assert_eq!(outcome, RelaunchShutdown::Finished);
    assert!(ran.load(Ordering::SeqCst), "the shutdown work did not run");
    assert!(
        !asked.load(Ordering::SeqCst),
        "a window with nothing loaded was asked to flush"
    );
    assert!(started.elapsed() < QUIT_FLUSH_TIMEOUT);
}

#[test]
fn a_relaunch_whose_window_never_answers_still_shuts_down() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    state.frontend_ready.store(true, Ordering::SeqCst);
    let ran = AtomicBool::new(false);

    let started = Instant::now();
    let outcome = shut_down_for_relaunch(&state, || ran.store(true, Ordering::SeqCst));

    assert_eq!(outcome, RelaunchShutdown::Finished);
    assert!(ran.load(Ordering::SeqCst));
    let waited = started.elapsed();
    assert!(waited >= QUIT_FLUSH_TIMEOUT, "gave up early: {waited:?}");
    assert!(
        waited < QUIT_FLUSH_TIMEOUT * 2,
        "hung on the window: {waited:?}"
    );
}

#[test]
fn a_relaunch_during_a_quit_waits_for_that_quit_and_writes_nothing_twice() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    state.frontend_ready.store(true, Ordering::SeqCst);
    assert_eq!(state.quit.begin(None), QuitDecision::StartFlush);
    let quit = state.quit.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(80));
        quit.finish();
    });
    let ran = AtomicBool::new(false);

    let started = Instant::now();
    let outcome = shut_down_for_relaunch(&state, || ran.store(true, Ordering::SeqCst));

    assert_eq!(outcome, RelaunchShutdown::AlreadyLeaving);
    assert!(
        !ran.load(Ordering::SeqCst),
        "the shutdown work ran a second time"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "returned while the quit was still writing; the installer would cut its snapshot"
    );
}

#[test]
fn a_second_relaunch_after_the_first_does_nothing() {
    let dir = TempDir::new().unwrap();
    let state = make_state(&dir);
    shut_down_for_relaunch(&state, || {});
    let ran = AtomicBool::new(false);

    let outcome = shut_down_for_relaunch(&state, || ran.store(true, Ordering::SeqCst));

    assert_eq!(outcome, RelaunchShutdown::AlreadyLeaving);
    assert!(!ran.load(Ordering::SeqCst));
}
