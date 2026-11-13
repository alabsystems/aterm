// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

const MODE: &str = "ATERM_TEST_REMOTE_HOST_IMAGE_MODE";
const FIXTURE: &str = "ATERM_TEST_REMOTE_HOST_FIXTURE";
const CASE: &str =
    "remote_host::tests::loaded_image_survives_launch_path_replacement_and_is_cleaned";

#[test]
fn loaded_image_survives_launch_path_replacement_and_is_cleaned() {
    if let Some(mode) = std::env::var_os(MODE) {
        assert!(
            matches!(
                mode.to_str(),
                Some(
                    "exec-child" | "retained-child" | "during-copy" | "after-copy" | "before-exec"
                )
            ),
            "unknown fixture mode"
        );
        if mode == "exec-child" {
            assert_eq!(
                std::env::var("ATERM_TEST_REMOTE_HOST_EXEC_PID").unwrap(),
                std::process::id().to_string()
            );
            assert!(
                !Path::new(&std::env::var_os("ATERM_TEST_REMOTE_HOST_OLD_DIR").unwrap()).exists()
            );
            return;
        }
        if mode == "retained-child" {
            println!("retained loaded host");
            return;
        }
        let launch = std::env::current_exe().unwrap();
        assert_eq!(launch.file_name(), Some(std::ffi::OsStr::new("launch")));
        assert_eq!(
            std::fs::canonicalize(&launch).unwrap(),
            std::fs::canonicalize(PathBuf::from(
                std::env::var_os(FIXTURE).expect("owned copied fixture")
            ))
            .unwrap()
        );
        assert!(
            launch
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("verify-host-fixture-")
        );
        let replace = || {
            let next = launch.with_extension("replacement");
            std::fs::write(&next, b"#!/bin/sh\necho wrong-replacement-host\nexit 71\n").unwrap();
            std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o500)).unwrap();
            std::fs::rename(next, &launch).unwrap();
        };
        if mode == "during-copy" {
            assert!(capture(&launch, replace).is_err());
            return;
        }
        let host = pin().unwrap();
        let path = image().unwrap().to_owned();
        assert_eq!(
            path.file_name(),
            launch.file_name(),
            "keep the actual launched host name"
        );
        let directory = path.parent().unwrap().to_owned();
        if mode == "before-exec" {
            release_before_exec().unwrap();
            assert!(!directory.exists());
            let error = aterm_uds::spawnfd::command(&launch)
                .args(["--exact", CASE, "--nocapture"])
                .env(MODE, "exec-child")
                .env(
                    "ATERM_TEST_REMOTE_HOST_EXEC_PID",
                    std::process::id().to_string(),
                )
                .env("ATERM_TEST_REMOTE_HOST_OLD_DIR", &directory)
                .exec();
            panic!("native exec failed: {error}");
        }
        replace();
        let out = aterm_uds::spawnfd::command(&path)
            .args(["--exact", CASE, "--nocapture"])
            .env(MODE, "retained-child")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("retained loaded host"));
        assert!(!String::from_utf8_lossy(&out.stdout).contains("wrong-replacement-host"));
        assert!(
            capture(&launch, || {}).is_err(),
            "a replacement cannot be recaptured"
        );
        drop(host);
        assert!(
            !directory.exists(),
            "ordinary return removes the owned host"
        );
        return;
    }
    for mode in ["after-copy", "during-copy", "before-exec"] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("verify-host-fixture-")
            .tempdir()
            .unwrap();
        let launch = dir.path().join("launch");
        std::fs::copy(std::env::current_exe().unwrap(), &launch).unwrap();
        let out = aterm_uds::spawnfd::command(&launch)
            .args(["--exact", CASE, "--nocapture"])
            .env(MODE, mode)
            .env(FIXTURE, &launch)
            .output()
            .unwrap();
        assert!(out.status.success(), "{mode}: {out:?}");
    }
}

#[test]
fn a_launch_symlink_uses_the_canonical_loaded_host_name() {
    let source = std::env::current_exe().unwrap().canonicalize().unwrap();
    let dir = aterm_tempfile::tempdir().unwrap();
    let alias = dir.path().join("launch-alias");
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    assert_ne!(
        alias.file_name(),
        source.file_name(),
        "raw-name negative control"
    );
    let host = capture(&alias, || {}).unwrap();
    assert_eq!(host.path().file_name(), source.file_name());
    assert!(!host.path().is_symlink());
    let other = dir.path().join("another-file");
    std::fs::write(&other, b"not the loaded executable").unwrap();
    let retargeted = capture(&alias, || {
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&other, &alias).unwrap();
    });
    assert!(
        retargeted.is_err(),
        "a retargeted raw launch link cannot complete capture"
    );
}

#[test]
fn a_foreign_guard_drop_keeps_the_original_owners_image() {
    let mut host = capture(&std::env::current_exe().unwrap(), || {}).unwrap();
    let path = host.path();
    let directory = path.parent().unwrap().to_owned();
    host.owner = std::process::id().wrapping_add(1);
    drop(host);
    assert!(
        path.is_file(),
        "a copied foreign guard cannot release its owner"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn an_unloaded_copy_cannot_become_the_transport_host() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let copy = dir.path().join("unloaded");
    std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
    assert!(capture(&copy, || {}).is_err());
}
