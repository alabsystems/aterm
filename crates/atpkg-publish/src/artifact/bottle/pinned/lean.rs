// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The upstream Lean runtime must prove and build through its own relocated Lake.

use std::path::Path;

use super::Runner;
use crate::error::{Error, Result};

pub(super) fn qualify(lake: &Path, version: &str, work: &Path, run: &Runner<'_>) -> Result<()> {
    let legacy = match version {
        "4.8.0" => true,
        "4.30.0-rc2" | "4.31.0" => false,
        _ => {
            return Err(Error::refused(
                "Lean version has no pinned runtime qualifier",
            ));
        }
    };
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
    let (project_name, project_source, positive, negative) = if legacy {
        (
            "lakefile.lean",
            "import Lake\nopen Lake DSL\npackage atpkg_probe\n@[default_target]\nlean_exe «atpkg-probe» where\n  root := `Main\n",
            "import Lean\ntheorem package_probe (x : Nat) : x = x := by rfl\ndef main : IO Unit := IO.println \"atpkg Lean runtime qualified\"\n",
            "import Lean\ntheorem false_probe : False := by decide\n",
        )
    } else {
        (
            "lakefile.toml",
            "name = \"atpkg_probe\"\nversion = \"0.1.0\"\ndefaultTargets = [\"atpkg-probe\"]\n\n[[lean_exe]]\nname = \"atpkg-probe\"\nroot = \"Main\"\n",
            "import Std.Tactic.BVDecide\ntheorem package_probe (x : BitVec 8) : x ^^^ x = 0 := by bv_decide\ndef main : IO Unit := IO.println \"atpkg Lean runtime qualified\"\n",
            "import Std.Tactic.BVDecide\ntheorem false_probe (x : BitVec 8) : x ^^^ x = 1 := by bv_decide\n",
        )
    };
    std::fs::write(project.join(project_name), project_source)
        .map_err(|error| Error::io("write Lake project", &error))?;
    std::fs::write(project.join("Main.lean"), positive)
        .map_err(|error| Error::io("write Lean proof", &error))?;
    // No external `require`: use this package's exact Lean import world. Native
    // compilation may also use the host SDK; outputs belong to the separate project.
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
    std::fs::write(project.join("Bad.lean"), negative)
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
#[path = "lean_tests.rs"]
mod tests;
