//! The Dock's tiles for the bundle under the old name lead to the new one,
//! with one pin left, and no other tile is touched (ADR-059 decision 8).

use std::path::PathBuf;

use jotra_core::bundle_rename::{
    bundle_file_url, plan_dock_tiles, DockFacts, DockPlan, DockTile, DockTileAction,
    DOCK_PATH_STRING, DOCK_URL_STRING,
};

use DockTileAction::{Drop, Keep, Retarget};

const APPS: &str = "/Applications";
const USER_APPS: &str = "/Users/u/Applications";
const LEGACY_URL: &str = "file:///Applications/Writ.app/";
const CURRENT_URL: &str = "file:///Applications/Jotra.app/";

fn url(location: &str) -> DockTile {
    DockTile {
        location: Some(location.to_string()),
        location_type: Some(DOCK_URL_STRING),
    }
}

fn posix(location: &str) -> DockTile {
    DockTile {
        location: Some(location.to_string()),
        location_type: Some(DOCK_PATH_STRING),
    }
}

fn facts_in(folder: &str, pinned: Vec<DockTile>, recent: Vec<DockTile>) -> DockFacts {
    DockFacts {
        pinned,
        recent,
        legacy_bundle: PathBuf::from(format!("{folder}/Writ.app")),
        bundle: PathBuf::from(format!("{folder}/Jotra.app")),
    }
}

fn plan(pinned: Vec<DockTile>, recent: Vec<DockTile>) -> DockPlan {
    plan_dock_tiles(&facts_in(APPS, pinned, recent))
}

#[test]
fn a_bundles_file_url_ends_in_a_slash() {
    assert_eq!(
        bundle_file_url(&PathBuf::from("/Applications/Jotra.app")),
        CURRENT_URL
    );
    assert_eq!(
        bundle_file_url(&PathBuf::from("/Applications/Jotra.app/")),
        CURRENT_URL
    );
}

#[test]
fn a_bundles_file_url_encodes_what_a_url_path_cannot_hold() {
    assert_eq!(
        bundle_file_url(&PathBuf::from("/Users/Jane Doe/Apps #1%?/Jotra.app")),
        "file:///Users/Jane%20Doe/Apps%20%231%25%3F/Jotra.app/"
    );
    assert_eq!(
        bundle_file_url(&PathBuf::from("/Users/Zoë/Applications/Jotra.app")),
        "file:///Users/Zo%C3%AB/Applications/Jotra.app/"
    );
    assert_eq!(
        bundle_file_url(&PathBuf::from("/Volumes/A&B (1)/+~!$',;=@:/Jotra.app")),
        "file:///Volumes/A&B%20(1)/+~!$',%3B=@:/Jotra.app/"
    );
}

#[test]
fn a_pinned_tile_of_the_old_bundle_is_retargeted() {
    let plan = plan(
        vec![url("file:///Applications/Notes.app/"), url(LEGACY_URL)],
        vec![],
    );
    assert_eq!(plan.pinned, [Keep, Retarget]);
    assert!(plan.recent.is_empty());
    assert_eq!(plan.bundle_url, CURRENT_URL);
    assert!(plan.changes_pinned());
    assert!(!plan.changes_recent());
    assert!(plan.changes_anything());
}

#[test]
fn every_spelling_of_the_old_bundles_location_is_read() {
    for tile in [
        url("file:///Applications/Writ.app"),
        url("file://localhost/Applications/Writ.app/"),
        url("FILE://LOCALHOST/Applications/Writ.app/"),
        url("file:///Applications/Wr%69t.app/"),
        url("file:///Applications/Wr%69t%2Eapp/"),
        posix("/Applications/Writ.app"),
        posix("/Applications/Writ.app/"),
    ] {
        let location = tile.location.clone().unwrap_or_default();
        assert_eq!(plan(vec![tile], vec![]).pinned, [Retarget], "{location}");
    }
}

#[test]
fn a_user_level_install_with_an_encoded_folder_is_retargeted() {
    let folder = "/Users/Jane Doe/Applications";
    let plan = plan_dock_tiles(&facts_in(
        folder,
        vec![url("file:///Users/Jane%20Doe/Applications/Writ.app/")],
        vec![url("file:///Users/Jane%20Doe/Applications/Writ.app/")],
    ));
    assert_eq!(plan.pinned, [Retarget]);
    assert_eq!(plan.recent, [Drop]);
    assert_eq!(
        plan.bundle_url,
        "file:///Users/Jane%20Doe/Applications/Jotra.app/"
    );
}

#[test]
fn a_non_ascii_folder_matches_through_its_escapes_in_either_case() {
    let folder = "/Users/Zoë/Applications";
    for location in [
        "file:///Users/Zo%C3%AB/Applications/Writ.app/",
        "file:///Users/Zo%c3%ab/Applications/Writ.app/",
    ] {
        let plan = plan_dock_tiles(&facts_in(folder, vec![url(location)], vec![]));
        assert_eq!(plan.pinned, [Retarget], "{location}");
    }
}

#[test]
fn an_old_name_bundle_in_another_folder_is_kept() {
    let plan = plan_dock_tiles(&facts_in(
        USER_APPS,
        vec![
            url(LEGACY_URL),
            url("file:///Users/u/Applications/Writ.app/"),
        ],
        vec![url(LEGACY_URL)],
    ));
    assert_eq!(plan.pinned, [Keep, Retarget]);
    assert_eq!(plan.recent, [Keep]);
    assert_eq!(plan.bundle_url, "file:///Users/u/Applications/Jotra.app/");
}

#[test]
fn another_apps_tile_is_kept() {
    let tiles = vec![
        url("file:///Applications/Writer.app/"),
        url("file:///Applications/Writ.app.old/"),
        url("file:///Applications/Writ.app/Contents/"),
        url("file:///Applications/writ.app/"),
        url("file:///Applications/Old/Writ.app/"),
        url("file:///Applications//Writ.app/"),
        posix("/Applications/Writer.app"),
    ];
    let count = tiles.len();
    let plan = plan(tiles.clone(), tiles);
    assert_eq!(plan.pinned, vec![Keep; count]);
    assert_eq!(plan.recent, vec![Keep; count]);
    assert!(!plan.changes_anything());
}

#[test]
fn a_tile_without_a_readable_location_is_kept() {
    let tiles = vec![
        DockTile::default(),
        DockTile {
            location: Some(LEGACY_URL.to_string()),
            location_type: None,
        },
        DockTile {
            location: None,
            location_type: Some(DOCK_URL_STRING),
        },
        DockTile {
            location: Some(LEGACY_URL.to_string()),
            location_type: Some(7),
        },
        posix(LEGACY_URL),
        url("/Applications/Writ.app/"),
        url("https:///Applications/Writ.app/"),
        url("file:/Applications/Writ.app/"),
        url("file://otherhost/Applications/Writ.app/"),
        url("file://localhostx/Applications/Writ.app/"),
        url("file:///Applications/Wr%6t.app/"),
        url("file:///Applications/Writ.app/%"),
        url("file:///Applications/Writ.app/%2"),
        url("file:///Applications/Writ%zz.app/"),
        url(""),
    ];
    let count = tiles.len();
    let plan = plan(tiles.clone(), tiles);
    assert_eq!(plan.pinned, vec![Keep; count]);
    assert_eq!(plan.recent, vec![Keep; count]);
    assert!(!plan.changes_anything());
}

#[test]
fn a_second_pin_of_the_old_bundle_is_dropped() {
    let plan = plan(
        vec![
            url(LEGACY_URL),
            url("file:///Applications/Notes.app/"),
            posix("/Applications/Writ.app"),
        ],
        vec![],
    );
    assert_eq!(plan.pinned, [Retarget, Keep, Drop]);
}

#[test]
fn an_old_pin_is_dropped_when_the_new_bundle_is_pinned_already() {
    let after = plan(vec![url(LEGACY_URL), url(CURRENT_URL)], vec![]);
    assert_eq!(after.pinned, [Drop, Keep]);
    let before = plan(vec![url(CURRENT_URL), url(LEGACY_URL)], vec![]);
    assert_eq!(before.pinned, [Keep, Drop]);
}

#[test]
fn a_recent_tile_of_the_old_bundle_is_dropped() {
    let plan = plan(
        vec![url("file:///Applications/Notes.app/")],
        vec![url(LEGACY_URL), url("file:///Applications/Mail.app/")],
    );
    assert_eq!(plan.pinned, [Keep]);
    assert_eq!(plan.recent, [Drop, Keep]);
    assert!(!plan.changes_pinned());
    assert!(plan.changes_recent());
    assert!(plan.changes_anything());
}

#[test]
fn a_recent_tile_of_the_new_bundle_goes_once_the_new_bundle_is_pinned() {
    let retargeted = plan(vec![url(LEGACY_URL)], vec![url(CURRENT_URL)]);
    assert_eq!(retargeted.recent, [Drop]);
    let pinned = plan(vec![url(CURRENT_URL)], vec![url(CURRENT_URL)]);
    assert_eq!(pinned.pinned, [Keep]);
    assert_eq!(pinned.recent, [Drop]);
}

#[test]
fn a_recent_tile_of_the_new_bundle_stays_when_it_is_not_pinned() {
    let plan = plan(
        vec![url("file:///Applications/Notes.app/")],
        vec![url(CURRENT_URL), url(LEGACY_URL)],
    );
    assert_eq!(plan.recent, [Keep, Drop]);
}

#[test]
fn a_dock_without_the_old_bundle_changes_nothing() {
    let settled = plan(
        vec![url(CURRENT_URL)],
        vec![url("file:///Applications/Mail.app/")],
    );
    assert_eq!(settled.pinned, [Keep]);
    assert_eq!(settled.recent, [Keep]);
    assert!(!settled.changes_anything());

    let empty = plan(vec![], vec![]);
    assert!(empty.pinned.is_empty() && empty.recent.is_empty());
    assert!(!empty.changes_anything());
}

#[test]
fn an_old_path_equal_to_the_bundle_drops_no_pin() {
    let facts = DockFacts {
        pinned: vec![url(CURRENT_URL), url(CURRENT_URL)],
        recent: vec![],
        legacy_bundle: PathBuf::from("/Applications/Jotra.app"),
        bundle: PathBuf::from("/Applications/Jotra.app/"),
    };
    let plan = plan_dock_tiles(&facts);
    assert_eq!(plan.pinned, [Keep, Keep]);
    assert!(!plan.changes_anything());
}
