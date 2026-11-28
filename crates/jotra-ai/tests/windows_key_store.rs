//! A key keyring 3 stored in the Credential Manager is read, replaced in place
//! and removed.
//!
//! Runs on Windows when `JOTRA_CREDENTIAL_MANAGER_TEST=1`, which CI's Windows
//! job sets. Anywhere else the test returns at once, so a developer's
//! credentials are not written to.
#![cfg(target_os = "windows")]

use std::ptr;

use jotra_ai::keys::platform_key_store;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_ENTERPRISE,
    CRED_TYPE_GENERIC,
};

/// The service provider keys are stored under.
const SERVICE: &str = "com.writ.ai";
/// An account no provider row uses, so the run touches nobody's key.
const ACCOUNT: &str = "jotra-ci-credential-manager";
const KEY: &str = "sk-written-by-keyring-3";
const REPLACED_KEY: &str = "sk-written-by-keyring-core";

fn enabled() -> bool {
    std::env::var("JOTRA_CREDENTIAL_MANAGER_TEST").as_deref() == Ok("1")
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// The target keyring 3 named an entry's credential by: user, a dot, service.
fn target_name() -> Vec<u16> {
    wide(&format!("{ACCOUNT}.{SERVICE}"))
}

/// The credential keyring 3 wrote: generic, enterprise persistence, the
/// account as user name, its version as comment, the key as UTF-16.
fn write_as_keyring_3(key: &str) {
    let mut target = target_name();
    let mut user = wide(ACCOUNT);
    let mut comment = wide("keyring v3.6.3");
    let mut alias = wide("");
    let mut blob = utf16_bytes(key);
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: comment.as_mut_ptr(),
        LastWritten: FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        CredentialBlobSize: u32::try_from(blob.len()).expect("a short key"),
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_ENTERPRISE,
        AttributeCount: 0,
        Attributes: ptr::null_mut(),
        TargetAlias: alias.as_mut_ptr(),
        UserName: user.as_mut_ptr(),
    };
    // SAFETY: every pointer in `credential` borrows a buffer that outlives the
    // call, and each string is NUL-terminated.
    let written = unsafe { CredWriteW(&credential, 0) };
    assert_ne!(written, 0, "the keyring 3 credential was written");
}

/// The blob stored under the test target, when there is one.
fn stored() -> Option<Vec<u8>> {
    let target = target_name();
    let mut found: *mut CREDENTIALW = ptr::null_mut();
    // SAFETY: `target` is NUL-terminated and outlives the call; on success
    // `found` points at a credential the system allocated.
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut found) } == 0 {
        return None;
    }
    // SAFETY: a successful read leaves `found` pointing at a credential whose
    // blob holds `CredentialBlobSize` bytes; it is copied out, then freed once.
    unsafe {
        let credential = &*found;
        let blob = if credential.CredentialBlob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(
                credential.CredentialBlob,
                credential.CredentialBlobSize as usize,
            )
            .to_vec()
        };
        CredFree(found.cast_const().cast());
        Some(blob)
    }
}

fn remove() {
    let target = target_name();
    // SAFETY: `target` is NUL-terminated and outlives the call. A credential
    // that is not there fails the call, which leaves nothing to remove.
    unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
}

#[test]
fn a_key_stored_by_keyring_3_is_read_replaced_in_place_and_removed() {
    if !enabled() {
        return;
    }
    remove();
    write_as_keyring_3(KEY);

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
        after_replace == Some(utf16_bytes(REPLACED_KEY)),
        "the replacing key did not update the credential in place"
    );
    assert!(removed.is_ok(), "the key was not removed");
    assert!(!left, "a credential is left after the key was removed");
}
