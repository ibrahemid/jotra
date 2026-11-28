//! The claim that decides who shows the window Jotra starts hidden.
//!
//! The frontend signals its first paint and a timer stands behind it, so both
//! reach the window on a slow launch. These pin what each of them comes away
//! with.

#[path = "support/app_state.rs"]
mod app_state;

use app_state::AppStateBuilder;

use jotra_tauri_lib::claim_reveal;
use jotra_tauri_lib::state::AppState;
use jotra_tauri_lib::window_state::RevealAction;
use tempfile::TempDir;

fn make_state(dir: &TempDir) -> AppState {
    AppStateBuilder::new(dir.path()).build()
}

// The frontend's signal arrives first on a healthy launch and shows the
// window; the timer behind it finds the window up and stands down.
#[test]
fn the_timer_stands_down_behind_a_window_that_is_up() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    assert_eq!(claim_reveal(&state, || Ok(false)), RevealAction::Show);
    assert_eq!(claim_reveal(&state, || Ok(true)), RevealAction::Skip);
}

// The launch this exists for: the show was taken and the window is not there.
// A claim nobody can see is worth nothing, so the timer shows it again.
#[test]
fn the_timer_shows_again_when_the_claimed_window_is_not_up() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    assert_eq!(claim_reveal(&state, || Ok(false)), RevealAction::Show);
    assert_eq!(claim_reveal(&state, || Ok(false)), RevealAction::Show);
}

// A visibility read that failed used to count as "already up", which is how a
// launch ended with a running app, no window, and nothing in the log.
#[test]
fn a_failed_visibility_read_does_not_stand_the_timer_down() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    assert_eq!(claim_reveal(&state, || Ok(false)), RevealAction::Show);
    assert_eq!(claim_reveal(&state, || Err(())), RevealAction::Show);
}

// Hiding Jotra in the seconds after opening it is still hiding it.
#[test]
fn a_window_the_user_put_away_stays_away() {
    let dir = TempDir::new().expect("temp dir");
    let state = make_state(&dir);

    assert_eq!(claim_reveal(&state, || Ok(false)), RevealAction::Show);
    state
        .window_dismissed
        .store(true, std::sync::atomic::Ordering::SeqCst);

    assert_eq!(claim_reveal(&state, || Ok(false)), RevealAction::Skip);
}
