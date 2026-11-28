//! The `open` arguments that start the macOS app on the data folder the
//! command uses.

use std::ffi::OsString;
use std::path::PathBuf;

use jotra_cli::macos_open_args;

const BUNDLE: &str = "com.writ.editor";

fn strings(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn a_data_folder_a_variable_names_goes_to_a_copy_started_for_it() {
    let folder = std::env::temp_dir().join("jotra-data");
    let mut assignment = OsString::from("JOTRA_DATA_DIR=");
    assignment.push(&folder);

    let args = macos_open_args(BUNDLE, Some(&folder), false, &[]);

    let mut expected = strings(&["-n", "--env"]);
    expected.push(assignment);
    expected.extend(strings(&["-b", BUNDLE]));
    assert_eq!(args, expected);
}

#[test]
fn files_for_a_folder_an_app_already_holds_go_to_that_app() {
    let folder = std::env::temp_dir().join("jotra-data");
    let file = std::env::temp_dir().join("a.md");

    let args = macos_open_args(BUNDLE, Some(&folder), true, std::slice::from_ref(&file));

    let mut expected = strings(&["-b", BUNDLE]);
    expected.push(file.into_os_string());
    assert_eq!(args, expected);
}

#[test]
fn without_a_variable_the_app_starts_on_its_own_folder() {
    let files: Vec<PathBuf> = vec![std::env::temp_dir().join("a.md")];

    let args = macos_open_args(BUNDLE, None, false, &files);

    let mut expected = strings(&["-b", BUNDLE]);
    expected.push(files[0].clone().into_os_string());
    assert_eq!(args, expected);
}
