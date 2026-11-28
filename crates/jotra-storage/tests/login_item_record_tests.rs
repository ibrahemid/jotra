use jotra_storage::login_item_record;

#[test]
fn the_record_reads_back_what_was_written() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(login_item_record::read(dir.path()), None);

    login_item_record::write(dir.path(), true).unwrap();
    assert_eq!(login_item_record::read(dir.path()), Some(true));

    login_item_record::write(dir.path(), false).unwrap();
    assert_eq!(login_item_record::read(dir.path()), Some(false));
}

#[test]
fn a_record_that_does_not_read_is_no_record() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(login_item_record::path(dir.path()), "maybe").unwrap();
    assert_eq!(login_item_record::read(dir.path()), None);
}
