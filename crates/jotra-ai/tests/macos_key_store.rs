//! A key keyring 3 stored in the login keychain is read, replaced in place and
//! removed.
//!
//! Runs on macOS when `JOTRA_KEYCHAIN_TEST=1`, which CI's macOS job sets.
//! Anywhere else the test returns at once, so a developer's keychain is not
//! written to.
#![cfg(target_os = "macos")]

use jotra_ai::keys::platform_key_store;
use security_framework::os::macos::keychain::{SecKeychain, SecPreferencesDomain};
use security_framework::os::macos::passwords::find_generic_password;

/// The service provider keys are stored under.
const SERVICE: &str = "com.writ.ai";
/// An account no provider row uses, so the run touches nobody's key.
const ACCOUNT: &str = "jotra-ci-keychain";
const KEY: &str = "sk-written-by-keyring-3";
const REPLACED_KEY: &str = "sk-written-by-keyring-core";

fn enabled() -> bool {
    std::env::var("JOTRA_KEYCHAIN_TEST").as_deref() == Ok("1")
}

/// The keychain keyring 3 wrote to: the user domain's default.
fn user_keychain() -> SecKeychain {
    SecKeychain::default_for_domain(SecPreferencesDomain::User).expect("the user keychain")
}

/// The password stored for the test account, when there is one.
fn stored() -> Option<Vec<u8>> {
    find_generic_password(Some(&[user_keychain()]), SERVICE, ACCOUNT)
        .ok()
        .map(|(password, _)| password.to_vec())
}

fn remove() {
    if let Ok((_, item)) = find_generic_password(Some(&[user_keychain()]), SERVICE, ACCOUNT) {
        item.delete();
    }
}

#[test]
fn a_key_stored_by_keyring_3_is_read_replaced_in_place_and_removed() {
    if !enabled() {
        return;
    }
    remove();
    // keyring 3 stored a generic password under the service and account as
    // given.
    user_keychain()
        .set_generic_password(SERVICE, ACCOUNT, KEY.as_bytes())
        .expect("the keyring 3 item was written");

    let store = platform_key_store();
    let read = store.get(ACCOUNT);
    let replaced = store.set(ACCOUNT, REPLACED_KEY);
    let after_replace = stored();
    let removed = store.delete(ACCOUNT);
    let left = stored().is_some();
    remove();

    assert!(
        matches!(read.as_ref().map(Option::as_deref), Ok(Some(KEY))),
        "the key keyring 3 stored was not read"
    );
    assert!(replaced.is_ok(), "the replacing key was not stored");
    assert!(
        after_replace.as_deref() == Some(REPLACED_KEY.as_bytes()),
        "the replacing key did not update the item in place"
    );
    assert!(removed.is_ok(), "the key was not removed");
    assert!(!left, "an item is left after the key was removed");
}
