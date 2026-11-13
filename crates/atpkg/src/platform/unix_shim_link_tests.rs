// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::symlink;

#[test]
fn relative_links_keep_their_parent_and_final_alias_even_when_dangling() {
    let root = crate::testdir::dir("atpkg-relative-shim-");
    let bin = root.join("bin");
    let store_bin = root.join("store/trust/1/bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&store_bin).unwrap();
    let program = store_bin.join("targo");
    fs::write(&program, b"program").unwrap();
    symlink("targo", store_bin.join("cargo")).unwrap();
    let shim = bin.join("cargo");
    let raw = Path::new("../store/trust/1/bin/cargo");
    symlink(raw, &shim).unwrap();
    let expected = bin.join(raw);
    let read = read_shim(&shim);
    assert_eq!(read.target.as_ref(), Some(&expected));
    assert_eq!(resolve_shim(&shim), Some(expected.clone()));
    assert!(expected.is_absolute());
    assert_eq!(expected.file_name().unwrap(), "cargo");
    assert_eq!(
        expected.canonicalize().unwrap(),
        program.canonicalize().unwrap()
    );
    assert_eq!(read.env, crate::shim_env::ShimEnv::NONE);
    assert_eq!(read.body, None);

    // Relative caller paths use the same native link-parent rule, without
    // changing this test process's working directory.
    let cwd = std::env::current_dir().unwrap();
    let mut relative = PathBuf::new();
    for part in cwd.components() {
        if matches!(part, std::path::Component::Normal(_)) {
            relative.push("..");
        }
    }
    relative.push(shim.strip_prefix("/").unwrap());
    assert_eq!(
        resolve_shim(&relative).unwrap().canonicalize().unwrap(),
        program.canonicalize().unwrap()
    );

    fs::remove_file(&program).unwrap();
    assert_eq!(resolve_shim(&shim), Some(expected.clone()));
    assert_eq!(read_shim(&shim).target, Some(expected));
    assert!(!shim.exists());
    assert_eq!(resolve_shim(&bin.join("missing")), None);
}

#[test]
fn link_dot_parent_uses_native_directory_resolution() {
    let root = crate::testdir::dir("atpkg-shim-native-parent-");
    let bin = root.join("bin");
    let foreign = root.join("foreign");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(foreign.join("nested")).unwrap();
    fs::write(bin.join("tool"), b"lexical wrong target").unwrap();
    fs::write(foreign.join("tool"), b"native target").unwrap();
    symlink(foreign.join("nested"), bin.join("slot")).unwrap();
    let shim = bin.join("shim");
    symlink("slot/../tool", &shim).unwrap();
    let target = resolve_shim(&shim).unwrap();
    assert_eq!(target, bin.join("slot/../tool"));
    assert_eq!(fs::read(&target).unwrap(), b"native target");
    assert_ne!(
        target.canonicalize().unwrap(),
        bin.join("tool").canonicalize().unwrap()
    );
    assert_eq!(read_shim(&shim).target, Some(target));
}

#[test]
fn absolute_links_and_relative_script_execs_keep_their_spelling() {
    let root = crate::testdir::dir("atpkg-shim-spelling-");
    let target = root.join("absent-alias");
    let link = root.join("link");
    symlink(&target, &link).unwrap();
    assert_eq!(resolve_shim(&link), Some(target.clone()));
    assert_eq!(read_shim(&link).target, Some(target));
    let script = root.join("script");
    fs::write(&script, b"#!/bin/sh\nexec '../relative-tool' \"$@\"\n").unwrap();
    assert_eq!(
        resolve_shim(&script),
        Some(PathBuf::from("../relative-tool"))
    );
    assert_eq!(
        read_shim(&script).target,
        Some(PathBuf::from("../relative-tool"))
    );
}
