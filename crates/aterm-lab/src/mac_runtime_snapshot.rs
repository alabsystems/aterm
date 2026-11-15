// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Portable shallow source repositories retain the commits the measured bundle names.

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::command;

use crate::Fail;

/// Local Git operations ignore inherited redirects, replacement objects and global/system
/// configuration. The selected repositories still supply their local configuration.
pub(super) fn git<I, S>(cwd: &Path, args: I) -> Result<String, Fail>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = command("git");
    for (name, _) in std::env::vars_os() {
        if name
            .as_encoded_bytes()
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
        {
            cmd.env_remove(name);
        }
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "--no-replace-objects",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(cwd);
    let output = completed(&mut cmd, "snapshot git")?;
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|error| Fail::die(format!("snapshot git returned non-UTF-8: {error}")))
}

fn completed(
    command: &mut aterm_uds::spawnfd::Command,
    what: &str,
) -> Result<std::process::Output, Fail> {
    // Retain the runtime's normal step bound; fixture waits use a one-minute hang detector.
    let limit = if cfg!(test) {
        Duration::from_secs(60)
    } else {
        crate::mac::STEP
    };
    let output = aterm_uds::deadline::output_with_input_until(
        command,
        aterm_uds::secret::Secret::new(&[]),
        Instant::now() + limit,
        aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
    )
    .map_err(|error| format!("{what}: complete output unavailable: {error}"))?;
    if !output.status.success() {
        return Err(Fail::die(format!(
            "{what}: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

/// The submodule commit belongs to the selected superproject tree, not a second HEAD capture.
pub(super) fn astream_pin(repo: &Path, source: &str) -> Result<String, Fail> {
    let entry = git(repo, ["ls-tree", source, "--", "vendor/astream"])?;
    let pin = entry
        .strip_prefix("160000 commit ")
        .and_then(|entry| entry.split_once('\t'))
        .filter(|(oid, path)| {
            matches!(oid.len(), 40 | 64)
                && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
                && *path == "vendor/astream"
        })
        .map(|(oid, _)| oid.to_owned())
        .ok_or_else(|| Fail::die("runtime source has no pinned vendor/astream gitlink"))?;
    let submodule = repo.join("vendor/astream");
    require_head(&submodule, &pin)?;
    if !git(
        &submodule,
        ["status", "--porcelain", "--untracked-files=no"],
    )?
    .is_empty()
    {
        return Err(Fail::die("runtime submodule has tracked modifications"));
    }
    Ok(pin)
}

fn require_head(repo: &Path, expected: &str) -> Result<(), Fail> {
    let actual = git(repo, ["rev-parse", "HEAD"])?;
    if actual != expected {
        return Err(Fail::die(format!(
            "runtime source changed: {} has HEAD {actual}, expected {expected}",
            repo.display()
        )));
    }
    Ok(())
}

fn clone(repo: &Path, target: &Path, expected: &str) -> Result<(), Fail> {
    git(
        repo,
        [
            OsStr::new("clone"),
            OsStr::new("--quiet"),
            OsStr::new("--no-local"),
            OsStr::new("--depth=1"),
            OsStr::new("--no-tags"),
            OsStr::new("--single-branch"),
            OsStr::new("--no-recurse-submodules"),
            OsStr::new("--"),
            repo.as_os_str(),
            target.as_os_str(),
        ],
    )?;
    require_head(target, expected)?;
    // No later guest operation can fetch from the host path, even accidentally.
    git(
        target,
        ["update-ref", "--no-deref", "-d", "refs/remotes/origin/HEAD"],
    )?;
    git(target, ["remote", "remove", "origin"])?;
    git(target, ["fsck", "--full", "--strict"])?;
    Ok(())
}

/// Archive independent object stores and their checked-out trees, including export-ignored files.
pub(super) fn archive(
    repo: &Path,
    source: &str,
    submodule: &str,
    share: &Path,
) -> Result<(), Fail> {
    let copied = share.join("source");
    clone(repo, &copied, source)?;
    let entry = git(&copied, ["rev-parse", "HEAD:vendor/astream"])?;
    if entry != submodule {
        return Err(Fail::die("runtime source gitlink changed during snapshot"));
    }
    let origin_submodule = repo.join("vendor/astream");
    require_head(&origin_submodule, submodule)?;
    if !git(
        &origin_submodule,
        ["status", "--porcelain", "--untracked-files=no"],
    )?
    .is_empty()
    {
        return Err(Fail::die("runtime submodule has tracked modifications"));
    }
    clone(&origin_submodule, &copied.join("vendor/astream"), &entry)?;
    completed(
        command("/usr/bin/tar")
            .arg("-cf")
            .arg(share.join("source.tar"))
            .arg("-C")
            .arg(&copied)
            .arg(".")
            .current_dir(repo),
        "snapshot tar",
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "mac_runtime_snapshot_tests.rs"]
mod tests;
