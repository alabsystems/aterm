// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
#[cfg(unix)]
#[test]
fn fallback_trash_does_not_follow_a_link_to_a_foreign_folder() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("theme-trash-link-")
        .tempdir()
        .unwrap();
    let root = scratch.path();
    let themes = root.join("themes");
    let foreign = root.join("foreign");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::create_dir_all(&foreign).unwrap();
    let file = themes.join("mine.conf");
    std::fs::write(&file, "theme to retain").unwrap();
    std::os::unix::fs::symlink(&foreign, themes.join(".trash")).unwrap();
    assert!(move_to_trash_fallback(&file, |_| {}).is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "theme to retain");
    assert_eq!(std::fs::read_dir(&foreign).unwrap().count(), 0);
}

#[test]
fn fallback_trash_keeps_a_recovery_file_that_wins_publication() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("theme-trash-collision-")
        .tempdir()
        .unwrap();
    let root = scratch.path();
    let file = root.join("mine.conf");
    std::fs::write(&file, "new theme").unwrap();
    let mut raced = false;
    let mut winner = None;
    move_to_trash_fallback(&file, |dest| {
        if !raced {
            std::fs::write(dest, "earlier recoverable theme").unwrap();
            winner = Some(dest.to_path_buf());
            raced = true;
        }
    })
    .unwrap();
    assert!(raced);
    assert_eq!(
        std::fs::read_to_string(winner.unwrap()).unwrap(),
        "earlier recoverable theme"
    );
    assert!(!file.exists());
    let mut paths = vec![root.join(".trash")];
    let mut recovered = Vec::new();
    while let Some(path) = paths.pop() {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                paths.push(path);
            } else {
                recovered.push(std::fs::read_to_string(path).unwrap());
            }
        }
    }
    recovered.sort();
    assert_eq!(recovered, ["earlier recoverable theme", "new theme"]);
}

#[test]
fn fallback_trash_does_not_discard_a_replaced_source() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("theme-trash-replaced-")
        .tempdir()
        .unwrap();
    let root = scratch.path();
    let file = root.join("mine.conf");
    let earlier = root.join("earlier.conf");
    std::fs::write(&file, "original theme").unwrap();
    let result = move_to_trash_fallback(&file, |_| {
        std::fs::rename(&file, &earlier).unwrap();
        std::fs::write(&file, "new unrelated theme").unwrap();
    });
    assert!(
        result.is_err(),
        "a replaced source is not the admitted theme"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "new unrelated theme"
    );
    assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "original theme");
}
