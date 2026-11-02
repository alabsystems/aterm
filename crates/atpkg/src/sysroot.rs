// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dynamic loader resolution check for `sysroot-bundle` members.

use std::path::Path;

/// Runs `bin --version` and requires a non-signal exit code.
///
/// # Errors
/// When the build cannot be held, or the binary cannot be spawned or is killed by a signal.
/// The build is held from before the spawn (`store::hold_first_exec`) and released only once
/// the child exited on its own or never started.
pub fn resolve_check(bin: &Path) -> Result<(), String> {
    use aterm_uds::spawnfd::Stdio;
    let hold = crate::store::hold_first_exec(bin).map_err(|e| {
        format!(
            "resolve check: cannot hold {} for its first exec: {e}",
            bin.display()
        )
    })?;
    let mut child = match aterm_uds::spawnfd::command(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            hold.ran();
            return Err(format!(
                "resolve check: cannot spawn {}: {e}",
                bin.display()
            ));
        }
    };
    let why = match child.wait() {
        // Any exit code proves the loader resolved the bundle; a signal death is the
        // dyld/ld.so failure.
        Ok(status) if status.code().is_some() => {
            hold.ran();
            return Ok(());
        }
        Ok(_) => String::from("was killed by a signal (unresolved dynamic library?)"),
        Err(e) => format!("could not be waited for ({e})"),
    };
    // Nothing showed it ran, so macOS may still be assessing its first exec: `hold` stands.
    Err(format!(
        "resolve check: {} {why} — refusing a broken toolchain install; its build stays on \
         disk while macOS may still be assessing that first exec",
        bin.display()
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::resolve_check;
    use crate::store::{FIRST_EXEC_MARKS_SUFFIX, held_unproven, sidecar_path};
    use std::path::{Path, PathBuf};

    fn build_with(root: &Path, name: &str, script: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let bin = root.join("store/tool").join(name).join("bin").join("tool");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(mode)).unwrap();
        bin
    }

    /// The build is held from before the spawn: an exit of the program's own releases it, a
    /// signal death leaves it held for every process, and a spawn the kernel refused holds
    /// nothing.
    #[test]
    fn a_resolve_check_holds_its_build_until_the_program_exited_on_its_own() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atpkg-resolve-hold-")
            .tempdir()
            .unwrap();
        let build = |name: &str| dir.path().join("store/tool").join(name);
        let mark = |name: &str| sidecar_path(&build(name), FIRST_EXEC_MARKS_SUFFIX).unwrap();

        let ran = build_with(
            dir.path(),
            "1",
            &format!(
                "#!/bin/sh\ntest -d '{}' || kill -KILL $$\n",
                mark("1").display()
            ),
            0o755,
        );
        assert_eq!(
            resolve_check(&ran),
            Ok(()),
            "the hold is on disk while it runs"
        );
        assert!(!mark("1").exists(), "an exit of its own releases it");
        assert!(!held_unproven(&build("1")));

        let killed = build_with(dir.path(), "2", "#!/bin/sh\nkill -KILL $$\n", 0o755);
        assert!(resolve_check(&killed).is_err());
        assert!(mark("2").is_dir(), "a signal death leaves its build held");
        assert!(held_unproven(&build("2")));

        let refused = build_with(dir.path(), "3", "#!/bin/sh\nexit 0\n", 0o644);
        assert!(resolve_check(&refused).is_err());
        assert!(
            !mark("3").exists(),
            "a program that never started holds nothing"
        );
    }
}
