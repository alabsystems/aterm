// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The upstream Lean runtime must prove and build through its own relocated Lake.

use std::path::Path;

use super::Runner;
use crate::error::{Error, Result};

pub(super) fn qualify(lake: &Path, version: &str, work: &Path, run: &Runner<'_>) -> Result<()> {
    let lean = lake.with_file_name("lean");
    let answer = run(&lean, &["--version"], work)?;
    if !answer.status.success()
        || !String::from_utf8_lossy(&answer.stdout)
            .starts_with(&format!("Lean (version {version},"))
    {
        return Err(Error::refused("Lean version differs from the package"));
    }
    let answer = run(lake, &["--version"], work)?;
    if !answer.status.success()
        || !String::from_utf8_lossy(&answer.stdout).contains(&format!("(Lean version {version})"))
    {
        return Err(Error::refused("Lake uses a different Lean version"));
    }
    let project = work.join("Lean project with spaces");
    std::fs::create_dir(&project).map_err(|error| Error::io("create Lean probe", &error))?;
    std::fs::write(
        project.join("lakefile.toml"),
        "name = \"atpkg_probe\"\nversion = \"0.1.0\"\ndefaultTargets = [\"atpkg-probe\"]\n\n[[lean_exe]]\nname = \"atpkg-probe\"\nroot = \"Main\"\n",
    )
    .map_err(|error| Error::io("write Lake project", &error))?;
    std::fs::write(
        project.join("Main.lean"),
        "import Std.Tactic.BVDecide\ntheorem package_probe (x : BitVec 8) : x ^^^ x = 0 := by bv_decide\ndef main : IO Unit := IO.println \"atpkg Lean runtime qualified\"\n",
    )
    .map_err(|error| Error::io("write Lean proof", &error))?;
    // No external `require`: the compiler, solver, Std and C toolchain all come from
    // this package, while Lake's build outputs belong to the separate project.
    let answer = run(lake, &["build"], &project)?;
    if !answer.status.success() {
        return Err(Error::refused(format!(
            "relocated Lake could not build the Std proof and native program: {}{}",
            String::from_utf8_lossy(&answer.stdout),
            String::from_utf8_lossy(&answer.stderr)
        )));
    }
    let answer = run(&project.join(".lake/build/bin/atpkg-probe"), &[], &project)?;
    if !answer.status.success()
        || String::from_utf8_lossy(&answer.stdout).trim() != "atpkg Lean runtime qualified"
    {
        return Err(Error::refused(
            "Lake's native program did not run correctly",
        ));
    }
    std::fs::write(
        project.join("Bad.lean"),
        "import Std.Tactic.BVDecide\ntheorem false_probe (x : BitVec 8) : x ^^^ x = 1 := by bv_decide\n",
    )
    .map_err(|error| Error::io("write false Lean proof", &error))?;
    let answer = run(&lean, &["Bad.lean"], &project)?;
    if answer.status.code().is_none()
        || answer.status.success()
        || !(String::from_utf8_lossy(&answer.stdout).contains("error:")
            || String::from_utf8_lossy(&answer.stderr).contains("error:"))
    {
        return Err(Error::refused(
            "Lean did not reject the false proof normally",
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;

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
}
