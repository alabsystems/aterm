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

/// A tool whose PATH entry would only exec its store program, a native executable, is a link to
/// that program where the kernel starts a program at its real path. An entry that exports an
/// environment keeps its script, and so does one whose program is a `#!` script or has no
/// native header, one that carries another name than the program's, and every entry where the
/// kernel names a program by the link's path.
#[test]
fn a_tool_that_only_execs_its_program_is_linked_on_path() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = crate::testdir::dir("atpkg-path-entry-");
    let bin = root.join("pkg/bin");
    fs::create_dir_all(&bin).unwrap();
    let program = |build: u32, bytes: &[u8]| {
        let store_bin = root.join(format!("pkg/store/ay/{build}/bin"));
        fs::create_dir_all(&store_bin).unwrap();
        let path = store_bin.join("ay");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
        path
    };
    let header = if cfg!(target_os = "macos") {
        let mut bytes = 0xCFFA_EDFE_u32.to_be_bytes().to_vec();
        bytes.resize(64, 0);
        bytes
    } else {
        crate::testdir::elf_fixture()
    };
    let native = program(7, &header[..]);
    let script = program(8, &b"#!/bin/sh\necho ay\n"[..]);
    let headless = program(9, &b"ay of build 9\n"[..]);
    let shim = bin.join("ay");
    let none = crate::shim_env::ShimEnv::NONE;
    let rendered = |target: &Path, env: &crate::shim_env::ShimEnv| {
        shim_executable_to_env(&shim, target, env).unwrap()
    };

    // The tool only execs its native program: its entry is a link to it, which the one writer
    // lays and every shim reader follows to the program.
    let entry = path_entry(rendered(native.as_path(), &none), &native, true);
    assert_eq!(entry.link.as_deref(), Some(native.as_path()));
    assert!(!entry.is_laid(), "nothing stands at the entry yet");
    crate::lay::write_in_process(&entry).unwrap();
    assert_eq!(fs::read_link(&shim).unwrap(), native);
    assert!(entry.is_laid());
    let read = read_shim(&shim);
    assert_eq!(read.target.as_deref(), Some(native.as_path()));
    assert_eq!(read.env, none);
    assert_eq!(read.body, None);
    assert_eq!(fs::read(&shim).unwrap(), header);

    // Where the kernel names a program by the path it was started by (macOS), the same tool keeps
    // the shim that execs the store path, and each entry tells the other from itself.
    let kept = path_entry(rendered(native.as_path(), &none), &native, false);
    assert_eq!(kept.link, None);
    assert_eq!(kept.body, super::super::sh_exec_only(&native).into_bytes());
    assert!(!kept.is_laid(), "the link standing is not the shim");
    crate::lay::write_in_process(&kept).unwrap();
    assert!(kept.is_laid());
    assert!(!entry.is_laid(), "the shim standing is not the link");

    // Negative control: an entry that exports an environment keeps its script.
    let env = crate::shim_env::ShimEnv::admit(&["AY_HOME=/opt/ay".to_string()]).unwrap();
    let exported = path_entry(rendered(native.as_path(), &env), &native, true);
    assert_eq!(exported.link, None);
    assert!(
        String::from_utf8(exported.body)
            .unwrap()
            .contains("export AY_HOME='/opt/ay'\n")
    );

    // Negative control: a `#!` program keeps its exec shim, and so does a program with no
    // native header, which a shell would run as a script: either would read the link's path as
    // its own.
    for program in [&script, &headless] {
        let interpreted = path_entry(rendered(program.as_path(), &none), program, true);
        assert_eq!(interpreted.link, None, "{}", program.display());
        assert_eq!(
            interpreted.body,
            super::super::sh_exec_only(program).into_bytes()
        );
    }

    // An alias's name is not the program's, which `argv[0]` would then carry: it keeps its shim.
    let alias = bin.join("alab-ay");
    let aliased = path_entry(
        shim_executable_to_env(&alias, &native, &none).unwrap(),
        &native,
        true,
    );
    assert_eq!(aliased.link, None);

    // Where this kernel starts a program at its real path, activation's own route lays the link.
    if STARTS_AT_REAL_PATH {
        let tool = crate::store::ToolName::new("ay").unwrap();
        crate::platform::install_shim_env(native.parent().unwrap(), &tool, &shim, &none).unwrap();
        assert_eq!(fs::read_link(&shim).unwrap(), native);
    }
}
