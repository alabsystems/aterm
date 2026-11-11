// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::process::ExitStatusExt;

use super::*;

#[test]
fn historical_lean_uses_its_own_import_world_and_still_refuses_false_proofs() {
    for sound in [true, false] {
        let work = aterm_tempfile::tempdir().unwrap();
        let result = qualify(
            Path::new("/store/bin/lake"),
            "4.8.0",
            work.path(),
            &|program, args, cwd| {
                let (code, stdout) = match (program.file_name().unwrap().to_str().unwrap(), args) {
                    ("lean", ["--version"]) => (0, "Lean (version 4.8.0, arm64)"),
                    ("lake", ["--version"]) => (0, "Lake (Lean version 4.8.0)"),
                    ("lake", ["build"]) => {
                        assert!(
                            std::fs::read_to_string(cwd.join("Main.lean"))
                                .unwrap()
                                .starts_with("import Lean\n")
                        );
                        assert!(
                            std::fs::read_to_string(cwd.join("lakefile.lean"))
                                .unwrap()
                                .contains("@[default_target]")
                        );
                        (0, "")
                    }
                    ("atpkg-probe", []) => (0, "atpkg Lean runtime qualified\n"),
                    ("lean", ["Bad.lean"]) => {
                        assert!(
                            std::fs::read_to_string(cwd.join("Bad.lean"))
                                .unwrap()
                                .contains("False := by decide")
                        );
                        (i32::from(sound), "Bad.lean: error: false proof")
                    }
                    _ => panic!("unexpected command"),
                };
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(code << 8),
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: vec![],
                })
            },
        );
        assert_eq!(result.is_ok(), sound);
    }
}

#[test]
fn qualification_requires_versions_a_native_build_and_sound_rejection() {
    for fault in [
        "none",
        "lean-version",
        "lake-version",
        "build",
        "run",
        "unsound",
        "crash",
    ] {
        let work = aterm_tempfile::tempdir().unwrap();
        let lake = Path::new("/store with spaces/bin/lake");
        let result = qualify(lake, "4.31.0", work.path(), &|program, args, cwd| {
            let (code, stdout) = match (program.file_name().unwrap().to_str().unwrap(), args) {
                ("lean", ["--version"]) => (
                    0,
                    if fault == "lean-version" {
                        "Lean (version 4.30.0, arm64)"
                    } else {
                        "Lean (version 4.31.0, arm64)"
                    },
                ),
                ("lake", ["--version"]) => (
                    0,
                    if fault == "lake-version" {
                        "Lake (Lean version 4.30.0)"
                    } else {
                        "Lake (Lean version 4.31.0)"
                    },
                ),
                ("lake", ["build"]) => {
                    assert!(
                        std::fs::read_to_string(cwd.join("Main.lean"))
                            .unwrap()
                            .contains("Std.Tactic.BVDecide")
                    );
                    let project = std::fs::read_to_string(cwd.join("lakefile.toml")).unwrap();
                    assert!(!project.contains("require"));
                    assert!(project.contains("[[lean_exe]]"));
                    (i32::from(fault == "build"), "")
                }
                ("atpkg-probe", []) => (
                    0,
                    if fault == "run" {
                        "wrong"
                    } else {
                        "atpkg Lean runtime qualified\n"
                    },
                ),
                ("lean", ["Bad.lean"]) => (
                    if fault == "unsound" { 0 } else { 1 },
                    "Bad.lean: error: false proof",
                ),
                _ => panic!("unexpected command {} {args:?}", program.display()),
            };
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(
                    if fault == "crash" && args == ["Bad.lean"] {
                        9
                    } else {
                        code << 8
                    },
                ),
                stdout: stdout.as_bytes().to_vec(),
                stderr: vec![],
            })
        });
        assert_eq!(result.is_ok(), fault == "none", "{fault}: {result:?}");
    }
}
