// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies which checkout a lab run serves when no `CARGO_MANIFEST_DIR` names it, as a lane verify
//! starts: the lab moves to `/` before it reads its paths, so the folder it started in has to survive
//! that. The default state folder is named after the checkout, which is how a test reads it from
//! outside.

#![cfg(target_os = "macos")]

use std::path::Path;

/// The default state folder's name for a lab started in `dir` with nothing naming its checkout.
fn state_name_from(dir: &Path) -> String {
    let out = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("aterm-lab"))
        .arg("paths")
        .current_dir(dir)
        .env_remove("CARGO_MANIFEST_DIR")
        .env_remove("LAB_STATE")
        .output()
        .expect("run aterm-lab paths");
    assert!(out.status.success(), "aterm-lab paths: {out:?}");
    let stdout = String::from_utf8(out.stdout).expect("paths prints text");
    let state = stdout
        .lines()
        .find_map(|line| line.strip_prefix("LAB_STATE="))
        .expect("paths prints LAB_STATE");
    Path::new(state)
        .file_name()
        .expect("a state folder has a name")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn a_run_started_inside_a_checkout_serves_that_checkout() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let lab = scratch.path().join("startedhere/crates/aterm-lab");
    std::fs::create_dir_all(lab.join("src")).expect("the checkout");
    std::fs::write(lab.join("Cargo.toml"), "").expect("the lab's manifest");
    assert!(
        state_name_from(&lab.join("src")).starts_with("startedhere-"),
        "the checkout above the folder it started in"
    );
}

#[test]
fn a_run_started_outside_any_checkout_serves_the_folder_it_started_in() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let nowhere = scratch.path().join("nocheckouthere");
    std::fs::create_dir(&nowhere).expect("the folder");
    assert!(
        state_name_from(&nowhere).starts_with("nocheckouthere-"),
        "never the filesystem root its working directory has moved to"
    );
}
