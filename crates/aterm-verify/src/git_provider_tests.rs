// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
fn executable(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir(dir).unwrap();
    let path = dir.join("git");
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn relative_empty_nonexecutable_and_directory_candidates_cannot_shadow_git() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = aterm_tempfile::tempdir().unwrap();
    let noexec = temp.path().join("noexec");
    std::fs::create_dir(&noexec).unwrap();
    std::fs::write(noexec.join("git"), b"not executable").unwrap();
    std::fs::set_permissions(noexec.join("git"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let directory = temp.path().join("directory");
    std::fs::create_dir_all(directory.join("git")).unwrap();
    let first = executable(&temp.path().join("first"));
    let second = executable(&temp.path().join("second"));
    let path = std::env::join_paths([
        Path::new("relative"),
        Path::new(""),
        &noexec,
        &directory,
        first.parent().unwrap(),
        second.parent().unwrap(),
    ])
    .unwrap();
    assert_eq!(resolve(&path).as_deref(), Some(first.as_path()));
}

#[cfg(unix)]
#[test]
fn selection_pins_the_original_provider_across_later_path_changes() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let first = executable(&temp.path().join("first"));
    let second = executable(&temp.path().join("second"));
    let cell = OnceLock::new();
    assert_eq!(
        selected(&cell, first.parent().unwrap().as_os_str()),
        Some(first.as_path())
    );
    assert_eq!(
        selected(&cell, second.parent().unwrap().as_os_str()),
        Some(first.as_path())
    );
}

#[cfg(unix)]
#[test]
fn missing_provider_stays_missing_instead_of_adopting_a_later_tool() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let later = temp.path().join("later");
    let path = std::env::join_paths([Path::new(""), Path::new("relative"), &later]).unwrap();
    let cell = OnceLock::new();
    assert!(selected(&cell, &path).is_none());
    executable(&later);
    assert!(selected(&cell, &path).is_none());
}

#[test]
fn genuine_provider_lists_a_real_checkout() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut command = command();
        command.current_dir(temp.path()).args(args);
        for name in crate::changed::GIT_REDIRECTS {
            command.env_remove(name);
        }
        let output =
            aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    run(&["init", "--quiet"]);
    std::fs::write(temp.path().join("provider-input"), b"real input\n").unwrap();
    run(&["add", "--", "provider-input"]);
    assert_eq!(run(&["ls-files", "-z"]), b"provider-input\0");
    assert_eq!(
        crate::checkout_root(temp.path()),
        Some(temp.path().canonicalize().unwrap())
    );
}

#[cfg(unix)]
#[test]
fn missing_provider_refuses_landing_before_checkout_or_publication() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "git_provider::tests::missing_provider_actor",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("ATV_GIT_PROVIDER_MISSING_CASE", "1")
        .env("PATH", temp.path());
    let output =
        aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("COULD NOT RUN — no absolute executable Git on PATH")
    );
}

#[cfg(unix)]
#[test]
fn missing_provider_actor() {
    if std::env::var_os("ATV_GIT_PROVIDER_MISSING_CASE").is_none() {
        return;
    }
    assert!(pin().is_none());
    let temp = aterm_tempfile::tempdir().unwrap();
    assert_eq!(
        crate::land::run(&[], temp.path()),
        crate::exit::COULD_NOT_RUN
    );
}

#[cfg(unix)]
#[test]
fn a_callers_tool_path_cannot_select_the_provider_for_the_process() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let invocation = executable(&temp.path().join("invocation"));
    let caller = executable(&temp.path().join("caller"));
    // Self-contained providers (shell builtins only) report which one ran and the PATH it ran
    // under, so the actor judges selection apart from what an installed provider needs on its
    // PATH. The genuine-provider test separately exercises real Git against a real checkout.
    std::fs::write(
        &invocation,
        b"#!/bin/sh\nprintf 'invocation-provider PATH=%s\\n' \"${PATH-unset}\"\n",
    )
    .unwrap();
    std::fs::write(
        &caller,
        b"#!/bin/sh\nprintf 'caller-provider PATH=%s\\n' \"${PATH-unset}\"\n",
    )
    .unwrap();
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "git_provider::tests::callers_tool_path_actor",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("ATV_GIT_PROVIDER_CALLER_PATH_CASE", "1")
        .env("PATH", invocation.parent().unwrap())
        .env("ATV_GIT_PROVIDER_EXPECTED", &invocation)
        .env("ATV_GIT_PROVIDER_CALLER_PATH", caller.parent().unwrap());
    let output =
        aterm_uds::deadline::output_within(&mut child, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn callers_tool_path_actor() {
    if std::env::var_os("ATV_GIT_PROVIDER_CALLER_PATH_CASE").is_none() {
        return;
    }
    let temp = aterm_tempfile::tempdir().unwrap();
    let expected = PathBuf::from(std::env::var_os("ATV_GIT_PROVIDER_EXPECTED").unwrap());
    let caller = std::env::var_os("ATV_GIT_PROVIDER_CALLER_PATH").unwrap();
    // The process's first Git command carries a tool PATH with no Git on it; the next one a tool
    // PATH whose Git is another provider. Both run the pinned provider, under the PATH given.
    for path in [std::ffi::OsString::new(), caller] {
        let output = crate::changed::git(temp.path(), &path)
            .args(["init", "--quiet"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("invocation-provider PATH={}\n", path.to_string_lossy())
        );
        assert_eq!(pin(), Some(expected.as_path()));
    }
}
