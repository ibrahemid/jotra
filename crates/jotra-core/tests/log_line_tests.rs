use std::borrow::Cow;

use jotra_core::log_line::redact_paths;

fn redact(line: &str) -> String {
    redact_paths(line).into_owned()
}

#[test]
fn a_line_with_no_path_is_handed_back_as_it_is() {
    let line = "the config watcher stopped error=the channel closed";
    assert!(matches!(redact_paths(line), Cow::Borrowed(_)));
    assert_eq!(
        redact("Projects/plan.md was saved"),
        "Projects/plan.md was saved"
    );
    assert_eq!(redact("3 / 4 files moved"), "3 / 4 files moved");
}

#[test]
fn an_error_value_names_its_file_and_not_the_folders_above_it() {
    assert_eq!(
        redact("WARN save refused error=the file changed on disk: /Users/ibra/Notes/plan.md"),
        "WARN save refused error=the file changed on disk: plan.md"
    );
    assert_eq!(
        redact("error=/home/ibra/Notes/plan.md cannot be written"),
        "error=plan.md cannot be written"
    );
}

#[test]
fn a_path_with_spaces_keeps_the_message_after_its_file_name() {
    assert_eq!(
        redact("error=/Users/ibra/My Notes/Ibra's plan.md could not be moved to the trash: busy"),
        "error=Ibra's plan.md could not be moved to the trash: busy"
    );
    assert_eq!(
        redact("error=the folder holding /Users/ibra/Notes (old)/plan.md cannot be written"),
        "error=the folder holding plan.md cannot be written"
    );
}

#[test]
fn a_field_after_the_path_is_left_whole() {
    assert_eq!(
        redact("path=/Users/ibra/Library/jotra/write.lock error=busy waited_ms=40"),
        "path=write.lock error=busy waited_ms=40"
    );
}

#[test]
fn two_paths_in_one_line_each_keep_their_file_name() {
    assert_eq!(
        redact("the archive /Users/ibra/Notes/.archive and the folder /Users/ibra/Notes are not separate folders"),
        "the archive .archive and the folder Notes are not separate folders"
    );
    assert_eq!(
        redact("moved \"/Users/ibra/a.md\" to \"/Users/ibra/Done/a.md\""),
        "moved \"a.md\" to \"a.md\""
    );
}

#[test]
fn a_folder_path_keeps_the_folder_name() {
    assert_eq!(
        redact("not a directory: /Users/ibra/Notes/"),
        "not a directory: Notes/"
    );
    assert_eq!(redact("watching ~/Notes"), "watching Notes");
    assert_eq!(redact("the root is /"), "the root is /");
}

#[test]
fn windows_paths_keep_only_the_file_name() {
    assert_eq!(
        redact(r"error=C:\Users\ibra\Notes\plan.md cannot be written"),
        "error=plan.md cannot be written"
    );
    assert_eq!(
        redact(r"error=\\?\C:\Users\ibra\Notes\plan.md cannot be written"),
        "error=plan.md cannot be written"
    );
    assert_eq!(
        redact(r"error=\\server\share\Notes\plan.md cannot be written"),
        "error=plan.md cannot be written"
    );
    assert_eq!(
        redact(r#"path="C:\\Users\\ibra\\Notes\\plan.md" error=denied"#),
        r#"path="plan.md" error=denied"#
    );
    assert_eq!(redact("error=C:/Users/ibra/plan.md"), "error=plan.md");
}

#[test]
fn a_line_break_ends_the_path() {
    assert_eq!(
        redact("error=/Users/ibra/Notes/plan.md\r\nnext line /Users/ibra/b.md\n"),
        "error=plan.md\r\nnext line b.md\n"
    );
}

#[test]
fn addresses_and_relative_names_are_not_paths() {
    let line = "fetch https://example.com/a/b.png failed at ./src/lib.rs and ../up";
    assert_eq!(redact(line), line);
    assert_eq!(redact("rate 10/s at 12:30"), "rate 10/s at 12:30");
}
