//! The preview's renders reach the backend through one command.

const LIB_RS: &str = include_str!("../src/lib.rs");
const PREVIEW_RS: &str = include_str!("../src/commands/preview.rs");
const TAURI_TS: &str = include_str!("../../src/services/tauri.ts");

#[test]
fn renders_are_asked_for_by_one_command() {
    assert!(LIB_RS.contains("commands::preview::preview_force_render,"));
    assert!(!LIB_RS.contains("commands::preview::preview_render,"));
    assert!(!PREVIEW_RS.contains("pub async fn preview_render<"));
    assert!(TAURI_TS.contains("invoke(\"preview_force_render\""));
    assert!(!TAURI_TS.contains("invoke(\"preview_render\""));
}
