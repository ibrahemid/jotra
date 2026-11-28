//! The record of the AppImage the app runs from.

use jotra_storage::app_image::{path, record_app_image, recorded_app_image};

#[test]
fn the_image_the_app_ran_from_is_read_back_and_rewritten_only_when_it_moves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let image = dir.path().join("Jotra.AppImage");
    let moved = dir.path().join("Apps").join("Jotra.AppImage");

    assert_eq!(recorded_app_image(dir.path()), None);
    assert!(record_app_image(dir.path(), &image).expect("record"));
    assert_eq!(recorded_app_image(dir.path()), Some(image.clone()));
    assert!(!record_app_image(dir.path(), &image).expect("again"));
    assert!(record_app_image(dir.path(), &moved).expect("moved"));
    assert_eq!(recorded_app_image(dir.path()), Some(moved));
}

#[test]
fn a_record_that_names_no_absolute_path_is_not_an_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(path(dir.path()), b"").expect("empty");
    assert_eq!(recorded_app_image(dir.path()), None);
    std::fs::write(path(dir.path()), b"Jotra.AppImage").expect("relative");
    assert_eq!(recorded_app_image(dir.path()), None);
}
