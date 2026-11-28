//! A key stored through the Secret Service outlives the process that stored it,
//! and a key keyring 3 stored is read, replaced in place and removed.
//!
//! Runs on Linux when `JOTRA_SECRET_SERVICE_TEST=1`, which CI's Linux job sets
//! inside a D-Bus session with an unlocked GNOME Keyring. Anywhere else each
//! test returns at once: the ordinary test step has no session bus, and a
//! developer's machine is not written to. With the variable set, a Secret
//! Service that does not answer fails the test rather than skipping it.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::process::Command;

use dbus_secret_service::{EncryptionType, SecretService};
use jotra_ai::keys::platform_key_store;
use jotra_ai::AiState;

/// An account no provider row uses, so the run touches nobody's key.
const ACCOUNT: &str = "jotra-ci-secret-service";
const KEY: &str = "sk-written-by-one-process";

/// The service provider keys are stored under.
const SERVICE: &str = "com.writ.ai";
/// Apart from [`ACCOUNT`], so the two tests run side by side.
const KEYRING_3_ACCOUNT: &str = "jotra-ci-keyring-3-layout";
const REPLACED_KEY: &str = "sk-written-by-keyring-core";

/// Set on the child run that plays the relaunched app.
const RELAUNCH: &str = "JOTRA_SECRET_SERVICE_RELAUNCH";

fn enabled() -> bool {
    std::env::var("JOTRA_SECRET_SERVICE_TEST").as_deref() == Ok("1")
}

#[test]
fn a_key_saved_in_one_process_is_read_by_the_next() {
    if !enabled() || std::env::var_os(RELAUNCH).is_some() {
        return;
    }

    let saved = AiState::new()
        .set_key(ACCOUNT, KEY)
        .expect("a key that is not empty is taken");
    assert!(
        saved.is_set && !saved.memory_only,
        "the Secret Service did not take the key, so the key row would say it is not saved"
    );

    let relaunch = Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "the_relaunched_process_reads_the_key",
            "--nocapture",
        ])
        .env(RELAUNCH, "1")
        .output()
        .expect("the relaunch ran");

    let cleared = platform_key_store().delete(ACCOUNT);
    assert!(
        relaunch.status.success(),
        "the relaunched process did not read the key:\n{}{}",
        String::from_utf8_lossy(&relaunch.stdout),
        String::from_utf8_lossy(&relaunch.stderr)
    );
    assert!(
        String::from_utf8_lossy(&relaunch.stdout).contains("1 passed"),
        "the relaunched process ran no test:\n{}",
        String::from_utf8_lossy(&relaunch.stdout)
    );
    cleared.expect("the test key was removed");
}

#[test]
fn the_relaunched_process_reads_the_key() {
    if !enabled() || std::env::var_os(RELAUNCH).is_none() {
        return;
    }
    let relaunched = AiState::new();
    let state = relaunched.key_state(ACCOUNT);
    assert!(state.is_set && !state.memory_only, "{state:?}");
    assert_eq!(relaunched.key(ACCOUNT).as_deref(), Some(KEY));
}

/// The secret of every item stored for `account`, in any collection.
fn stored_secrets(service: &SecretService, account: &str) -> Vec<Vec<u8>> {
    let found = service
        .search_items(HashMap::from([("service", SERVICE), ("username", account)]))
        .expect("the Secret Service searched its items");
    found
        .unlocked
        .iter()
        .chain(found.locked.iter())
        .map(|item| item.get_secret().expect("an item's secret was read"))
        .collect()
}

fn remove_items(service: &SecretService, account: &str) {
    let found = service
        .search_items(HashMap::from([("service", SERVICE), ("username", account)]))
        .expect("the Secret Service searched its items");
    for item in found.unlocked.iter().chain(found.locked.iter()) {
        item.delete().expect("a test item was removed");
    }
}

#[test]
fn a_key_stored_by_keyring_3_is_read_replaced_in_place_and_removed() {
    if !enabled() || std::env::var_os(RELAUNCH).is_some() {
        return;
    }
    let service = SecretService::connect(EncryptionType::Dh).expect("the Secret Service answers");
    remove_items(&service, KEYRING_3_ACCOUNT);
    // The item keyring 3 created for an entry: these attributes, label and
    // content type, in the collection behind the `default` alias.
    service
        .get_default_collection()
        .expect("the default collection")
        .create_item(
            &format!("{KEYRING_3_ACCOUNT}@{SERVICE}:default (keyring v3.6.3)"),
            HashMap::from([
                ("service", SERVICE),
                ("username", KEYRING_3_ACCOUNT),
                ("target", "default"),
                ("application", "rust-keyring"),
            ]),
            KEY.as_bytes(),
            true,
            "text/plain",
        )
        .expect("the keyring 3 item was written");

    let store = platform_key_store();
    let read = store.get(KEYRING_3_ACCOUNT);
    let replaced = store.set(KEYRING_3_ACCOUNT, REPLACED_KEY);
    let after_replace = stored_secrets(&service, KEYRING_3_ACCOUNT);
    let removed = store.delete(KEYRING_3_ACCOUNT);
    let left = stored_secrets(&service, KEYRING_3_ACCOUNT).len();
    remove_items(&service, KEYRING_3_ACCOUNT);

    assert!(
        matches!(read.as_ref().map(Option::as_deref), Ok(Some(KEY))),
        "the key keyring 3 stored was not read"
    );
    assert!(replaced.is_ok(), "the replacing key was not stored");
    assert!(
        after_replace.len() == 1 && after_replace[0] == REPLACED_KEY.as_bytes(),
        "the replacing key did not update the one item in place"
    );
    assert!(removed.is_ok(), "the key was not removed");
    assert_eq!(left, 0, "an item is left after the key was removed");
}
