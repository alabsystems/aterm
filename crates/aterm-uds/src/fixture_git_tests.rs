// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

/// This host's Git itself, as [`underlying`] resolves the first `git` on `PATH` to the executable
/// it keeps beside its helpers, or COULD NOT RUN on a host with no Git or one that keeps none
/// there.
fn host_git() -> PathBuf {
    let found = on_path();
    if !found.is_absolute() {
        crate::host::could_not_run("no git on PATH");
    }
    let Some(git) = exec_path_of(&found).as_deref().and_then(own_executable) else {
        crate::host::could_not_run(format!(
            "{} keeps no git beside its helpers (`git --exec-path`)",
            found.display()
        ));
    };
    assert!(
        !starts_script(&git),
        "{} is a script, not Git",
        git.display()
    );
    git
}

/// Whether the file at `path` starts `#!`.
fn starts_script(path: &Path) -> bool {
    use std::io::Read as _;
    let mut head = [0; 2];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok_and(|()| head == *b"#!")
}

/// `body`, an executable `#!/bin/sh` script named `git` in a directory of its own under `root`.
fn script(root: &Path, dir: &str, body: &str) -> PathBuf {
    let dir = root.join(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = dir.join("git");
    std::fs::write(&git, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    git
}

/// A WRAPPER IN FRONT OF GIT RESOLVES TO GIT ITSELF: a `#!/bin/sh` script that only execs Git, as
/// atpkg's `PATH` entry and a package's `bin/git` do, resolves to the executable Git keeps beside
/// its helpers, at its real path; that Git resolves to itself.
/// NEGATIVE CONTROLS: a `git` that does not answer `--exec-path` stays as found, and so does one
/// whose answer names a directory whose `git` is itself a script.
#[test]
fn a_wrapper_in_front_of_git_resolves_to_git_itself() {
    let git = host_git();
    let root = aterm_tempfile::tempdir().unwrap();
    let quoted = git.display().to_string().replace('\'', r"'\''");
    let wrapper = script(root.path(), "wrap", &format!("exec '{quoted}' \"$@\""));
    let through = underlying(wrapper);
    assert_eq!(through.program, git);
    assert!(through.exec_path.is_some_and(|dir| dir.is_dir()));
    assert_eq!(underlying(git.clone()).program, git);

    let mute = script(root.path(), "mute", "exit 3");
    assert_eq!(underlying(mute.clone()).program, mute);
    let helpers = root.path().join("helpers");
    let inner = script(&helpers, "", "exit 0");
    let answer = helpers.display().to_string().replace('\'', r"'\''");
    let pointing = script(root.path(), "point", &format!("echo '{answer}'"));
    assert_eq!(underlying(pointing.clone()).program, pointing);
    assert!(inner.is_file());
}

/// THE FIXTURE RUNS GIT ITSELF ON MACOS: [`git`]'s program is no `#!` file, which the kernel
/// would start with a leaked element (ATERM-KI-1116-1024), and it runs.
#[cfg(target_os = "macos")]
#[test]
fn the_fixture_git_is_no_script() {
    host_git();
    assert!(!starts_script(path()), "{}", path().display());
    let out = git()
        .arg("--version")
        .output()
        .expect("run the fixture git");
    assert!(out.status.success(), "{out:?}");
    assert!(exec_path().is_some_and(Path::is_dir));
}
