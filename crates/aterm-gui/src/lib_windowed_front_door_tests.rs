// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::windowed_front_door;
use std::path::{Path, PathBuf};

fn install() -> PathBuf {
    PathBuf::from(r"C:\Users\me\AppData\Local\Programs\aterm")
}

fn build_tree() -> PathBuf {
    PathBuf::from(r"C:\src\aterm\target\debug")
}

#[test]
fn an_install_folder_answers_its_aterm_gui() {
    let dir = install();
    let gui = dir.join("aterm-gui.exe");
    let only_gui = |path: &Path| path == gui;
    for running in ["aterm.exe", "aterm-ctl.exe", "atpkg.exe", "aterm-cli.exe"] {
        assert_eq!(
            windowed_front_door(&dir.join(running), only_gui, false),
            Some(gui.clone()),
            "{running}"
        );
    }
    assert_eq!(
        windowed_front_door(&gui, only_gui, false),
        Some(gui.clone()),
        "the windowed image names itself"
    );
    let shouty = dir.join("ATERM-GUI.EXE");
    assert_eq!(
        windowed_front_door(&shouty, |_| false, false),
        Some(shouty.clone()),
        "Windows names are case-insensitive"
    );
}

#[test]
fn a_build_tree_answers_its_aterm_windowed_and_never_the_thin_bin() {
    let dir = build_tree();
    let windowed = dir.join("aterm-windowed.exe");
    let thin = dir.join("aterm-gui.exe");
    let both = |path: &Path| path == windowed || path == thin;
    for running in ["aterm.exe", "aterm-gui.exe", "aterm-windowed.exe"] {
        assert_eq!(
            windowed_front_door(&dir.join(running), both, true),
            Some(windowed.clone()),
            "{running}"
        );
    }
    let only_thin = |path: &Path| path == thin;
    assert_eq!(
        windowed_front_door(&dir.join("aterm.exe"), only_thin, true),
        None,
        "the console image of a build tree without its windowed image"
    );
    assert_eq!(
        windowed_front_door(&thin, only_thin, true),
        None,
        "the thin dev bin is not the front door, even for itself"
    );
}

#[test]
fn a_lone_console_image_has_no_windowed_front_door() {
    assert_eq!(
        windowed_front_door(&install().join("aterm.exe"), |_| false, false),
        None
    );
}
