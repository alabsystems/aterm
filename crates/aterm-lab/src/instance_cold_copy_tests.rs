// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;

fn version_output(code: i32, stdout: &[u8]) -> std::process::Output {
    std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.to_vec(),
        stderr: Vec::new(),
    }
}

/// A timed-out probe, a failed process that printed a banner, and a wrong banner all
/// leave the world in place. No executable is started by these injected failure effects.
#[test]
fn an_unproven_cold_version_keeps_its_world() {
    for result in [
        Err(aterm_uds::deadline::RunError::TimedOut),
        Ok(version_output(1, b"aterm 0.0.0\n")),
        Ok(version_output(0, b"another program\n")),
    ] {
        let mut world = World::new(CONFIG_OFF).unwrap();
        let dir = world.dir.clone();
        let exe = dir.join("unstarted");
        std::fs::write(&exe, b"not an executable").unwrap();
        let failed = prove_cold_version(&mut world, &exe, |cmd| {
            assert_eq!(cmd.get_current_dir(), Some(dir.as_path()));
            assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["--version"]);
            result
        });
        assert!(failed.is_err());
        drop(world);
        assert!(exe.is_file(), "an unproven image's directory was removed");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Successful proof restores the caller's keep choice, rather than clearing an explicit
/// request to retain the world. The ordinary cleanup arm is the negative control.
#[test]
fn a_proven_cold_version_preserves_the_callers_keep_choice() {
    for keep in [false, true] {
        let mut world = World::new(CONFIG_OFF).unwrap();
        let dir = world.dir.clone();
        world.keep = keep;
        prove_cold_version(&mut world, &dir.join("unstarted"), |_| {
            Ok(version_output(0, b"aterm 0.0.0\n"))
        })
        .unwrap();
        drop(world);
        assert_eq!(dir.exists(), keep);
        if keep {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn a_cold_copy_of_a_link_is_the_file_it_names_with_no_page_resident() {
    let world_source = World::new(CONFIG_OFF).expect("fixture world");
    let real = world_source.dir.join("aterm-real");
    let script = b"#!/bin/sh\necho 'aterm 0.0.0'\n";
    std::fs::write(&real, script).expect("stand-in");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).expect("mode");
    let link = world_source.dir.join("aterm");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let mut world = World::new(CONFIG_OFF).expect("fresh private world");
    let copy = cold_copy(&mut world, &link).expect("a cold copy");
    let meta = std::fs::symlink_metadata(&copy.exe).expect("the copy");
    assert!(meta.is_file(), "a file, not a link: {meta:?}");
    assert!(copy.exe.starts_with(&world.dir), "{}", copy.exe.display());
    assert_eq!(std::fs::read(&copy.exe).expect("read"), script);
}
