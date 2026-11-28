use jotra_storage::capture_draft;

#[test]
fn a_draft_reads_back_what_was_kept() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(capture_draft::read(dir.path()), "");

    capture_draft::write(dir.path(), "call the bank\r\nthen lunch").unwrap();
    assert_eq!(
        capture_draft::read(dir.path()),
        "call the bank\r\nthen lunch"
    );

    capture_draft::write(dir.path(), "مرحبا").unwrap();
    assert_eq!(capture_draft::read(dir.path()), "مرحبا");
}

#[test]
fn an_empty_draft_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    capture_draft::write(dir.path(), "half a thought").unwrap();
    capture_draft::write(dir.path(), " \n").unwrap();
    assert!(!capture_draft::path(dir.path()).exists());

    capture_draft::write(dir.path(), "").unwrap();
    assert_eq!(capture_draft::read(dir.path()), "");
}
