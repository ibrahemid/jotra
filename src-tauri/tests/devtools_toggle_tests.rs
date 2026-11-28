//! A debug build's devtools toggle: one runtime grant, never in the
//! capability file a release build ships.

use std::collections::BTreeMap;

use jotra_tauri_lib::devtools::{devtools_toggle_capability, DEVTOOLS_TOGGLE_PERMISSION};
use tauri::ipc::RuntimeCapability;
use tauri::utils::acl::capability::{CapabilityFile, PermissionEntry};
use tauri::utils::acl::manifest::Manifest;
use tauri::utils::acl::resolved::Resolved;
use tauri::utils::platform::Target;

const ACL_MANIFESTS: &str = include_str!("../gen/schemas/acl-manifests.json");
const CAPABILITIES: &str = include_str!("../capabilities/default.json");
const LIB_RS: &str = include_str!("../src/lib.rs");

fn capability() -> tauri::utils::acl::capability::Capability {
    match devtools_toggle_capability().build() {
        CapabilityFile::Capability(capability) => capability,
        _ => panic!("the devtools grant is one capability"),
    }
}

#[test]
fn the_grant_is_the_toggle_alone_for_the_main_window() {
    let capability = capability();
    assert_eq!(capability.windows, vec!["main".to_string()]);
    assert!(capability.webviews.is_empty());
    assert!(capability.local);
    assert!(capability.remote.is_none());
    assert_eq!(capability.permissions.len(), 1);
    match &capability.permissions[0] {
        PermissionEntry::PermissionRef(id) => assert_eq!(id.get(), DEVTOOLS_TOGGLE_PERMISSION),
        PermissionEntry::ExtendedPermission { .. } => panic!("the grant carries no scope"),
    }
}

#[test]
fn the_grant_resolves_to_the_toggle_command_only() {
    let acl: BTreeMap<String, Manifest> =
        serde_json::from_str(ACL_MANIFESTS).expect("acl-manifests.json parses");
    let capability = capability();
    let resolved = Resolved::resolve(
        &acl,
        BTreeMap::from([(capability.identifier.clone(), capability)]),
        Target::current(),
    )
    .expect("the devtools grant resolves against the app's permissions");
    let allowed: Vec<&str> = resolved
        .allowed_commands
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(allowed, vec!["plugin:webview|internal_toggle_devtools"]);
    assert!(resolved.denied_commands.is_empty());
}

#[test]
fn only_a_debug_build_adds_the_grant() {
    assert!(LIB_RS.contains(
        "#[cfg(debug_assertions)]\n            if let Err(e) = app.add_capability(devtools::devtools_toggle_capability())"
    ));
    assert!(!CAPABILITIES.contains("devtools"));
    assert!(!CAPABILITIES.contains("core:webview:default"));
}
