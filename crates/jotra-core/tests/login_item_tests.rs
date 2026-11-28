use std::path::Path;

use jotra_core::login_item::{
    may_point_login_item_at, parse_login_item_record, plan_login_reconcile,
    render_login_item_record, LoginReconcile,
};

#[test]
fn a_config_and_an_os_that_agree_are_left_alone() {
    for recorded in [None, Some(true), Some(false)] {
        assert_eq!(
            plan_login_reconcile(true, recorded, true),
            LoginReconcile::Leave
        );
        assert_eq!(
            plan_login_reconcile(false, recorded, false),
            LoginReconcile::Leave
        );
    }
}

#[test]
fn a_config_edited_away_from_the_record_moves_the_os_item() {
    assert_eq!(
        plan_login_reconcile(true, Some(false), false),
        LoginReconcile::SetOs(true)
    );
    assert_eq!(
        plan_login_reconcile(false, Some(true), true),
        LoginReconcile::SetOs(false)
    );
}

#[test]
fn an_os_item_changed_in_the_system_settings_moves_the_config() {
    assert_eq!(
        plan_login_reconcile(true, Some(true), false),
        LoginReconcile::FollowOs(false)
    );
    assert_eq!(
        plan_login_reconcile(false, Some(false), true),
        LoginReconcile::FollowOs(true)
    );
}

#[test]
fn with_no_record_the_config_wins() {
    assert_eq!(
        plan_login_reconcile(true, None, false),
        LoginReconcile::SetOs(true)
    );
    assert_eq!(
        plan_login_reconcile(false, None, true),
        LoginReconcile::SetOs(false)
    );
}

#[test]
fn the_record_reads_back_what_it_wrote() {
    for on in [true, false] {
        assert_eq!(
            parse_login_item_record(render_login_item_record(on)),
            Some(on)
        );
    }
    assert_eq!(parse_login_item_record("on\r\n"), Some(true));
    assert_eq!(parse_login_item_record(""), None);
    assert_eq!(parse_login_item_record("yes"), None);
}

#[test]
fn a_login_item_never_points_into_a_disk_image_or_a_translocated_copy() {
    assert!(may_point_login_item_at(
        Path::new("/Applications/Jotra.app/Contents/MacOS/jotra-app"),
        false
    ));
    assert!(!may_point_login_item_at(
        Path::new("/Volumes/Jotra/Jotra.app/Contents/MacOS/jotra-app"),
        true
    ));
    assert!(!may_point_login_item_at(
        Path::new(
            "/private/var/folders/x/T/AppTranslocation/0A1B/d/Jotra.app/Contents/MacOS/jotra-app"
        ),
        false
    ));
    assert!(may_point_login_item_at(
        Path::new(r"C:\Program Files\Jotra\jotra-app.exe"),
        false
    ));
}
