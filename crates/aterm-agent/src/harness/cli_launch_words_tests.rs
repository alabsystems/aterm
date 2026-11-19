// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `.cmd` twin's ask, `aterm harness tap --launch --into <folder> -- <args>`: its writer of
//! the twin's words.

use super::*;

/// THE ASK WRITES THE TWIN'S WORDS WHOLE OR NOT AT ALL: a value lands in the folder's value file
/// as `atpkg::launch_tap::cmd_words` spells it, with no temporary file left; no value, a value
/// the twin cannot carry, an empty folder name (the twin made none) and a folder that is not
/// there get nothing written.
#[test]
fn the_into_ask_writes_the_twins_words_whole_or_not_at_all() {
    let tmp = tmp_dir("launch-words");
    let into = tmp.path().join("atpkg-tap-1-2");
    std::fs::create_dir(&into).unwrap();
    let value = r#"{"statusLine":{"type":"command","command":"C:\a & b\aterm.exe harness tap"}}"#;
    let file = into.join(atpkg::launch_tap::INTO_VALUE);
    let listed = || {
        let mut names: Vec<String> = std::fs::read_dir(&into)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    write_launch_words(&into, Some(value));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        atpkg::launch_tap::cmd_words(value).unwrap()
    );
    assert_eq!(listed(), [atpkg::launch_tap::INTO_VALUE]);
    std::fs::remove_file(&file).unwrap();
    for (into, value, why) in [
        (into.as_path(), None, "no value"),
        (
            into.as_path(),
            Some("caf\u{e9}"),
            "a value the twin cannot carry",
        ),
        (Path::new(""), Some(value), "no folder"),
    ] {
        write_launch_words(into, value);
        assert_eq!(listed(), Vec::<String>::new(), "{why}");
    }
    let gone = tmp.path().join("gone");
    write_launch_words(&gone, Some(value));
    assert!(!gone.exists(), "a folder that is not there is not made");
}
