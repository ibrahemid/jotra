//! The devtools toggle of a debug build.

use tauri::ipc::CapabilityBuilder;

/// Tauri's devtools toggle (Cmd+Option+I on macOS, Ctrl+Shift+I elsewhere).
/// Only a debug build has the toggle and its command, and only a debug build
/// adds [`devtools_toggle_capability`].
pub const DEVTOOLS_TOGGLE_PERMISSION: &str = "core:webview:allow-internal-toggle-devtools";

/// The one grant a debug build adds: [`DEVTOOLS_TOGGLE_PERMISSION`], for the
/// main window's own pages. `capabilities/default.json` keeps to the calls the
/// frontend makes.
pub fn devtools_toggle_capability() -> CapabilityBuilder {
    CapabilityBuilder::new("debug-devtools-toggle")
        .window("main")
        .permission(DEVTOOLS_TOGGLE_PERMISSION)
}
