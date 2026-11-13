// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Selection-local A19 records for the release's early source judgment.
//! These never answer a whole-workspace qualify question.

use std::path::{Path, PathBuf};

use crate::ledger::GitRunner;
use aterm_uds::spawnfd::Command;

/// One stable input checkout for dry and real runs, quiet for fseventsd and Spotlight.
pub(crate) fn directory(release: &Path) -> PathBuf {
    release
        .join(aterm_tempfile::QUIET)
        .join("input-checkout.noindex")
}

/// Maintain the release's exclusively claimed, stable preflight checkout. An existing path
/// must be registered to this repository and clean; no user work is discarded or adopted.
pub fn checkout(git: &dyn GitRunner, root: &Path, tip: &str) -> Result<(), String> {
    let name = root.to_str().ok_or("non-UTF-8 input checkout path")?;
    let run = |git: &dyn GitRunner, args: &[&str]| {
        let out = git.git(args).map_err(|e| e.to_string())?;
        if out.success() {
            Ok(out.stdout_utf8())
        } else {
            Err(out.stderr_utf8())
        }
    };
    if root.try_exists().map_err(|e| e.to_string())? {
        if root
            .symlink_metadata()
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("input checkout is a symlink".into());
        }
        let listed = run(git, &["worktree", "list", "--porcelain", "-z"])?;
        let physical = root.canonicalize().map_err(|e| e.to_string())?;
        if !listed
            .split('\0')
            .filter_map(|line| line.strip_prefix("worktree "))
            .any(|path| {
                Path::new(path)
                    .canonicalize()
                    .is_ok_and(|path| path == physical)
            })
        {
            return Err("existing input checkout is not owned by this repository".into());
        }
        let own = crate::ledger::GitCli::new(root);
        if !run(&own, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
            return Err("input checkout has tracked edits; no work was discarded".into());
        }
        run(&own, &["checkout", "--detach", tip])?;
    } else {
        run(git, &["worktree", "add", "--detach", name, tip])?;
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Attempt {
    tree: String,
    seal: String,
}

/// The immediately preceding input attempt for this release base. Its A19 package rows are
/// only candidates; a failed or unfinished attempt cannot resurrect an older success.
pub fn previous_items(
    records: &Path,
    base: &str,
) -> Result<Vec<aterm_verify::input_content::Proof>, String> {
    let at = records.join(format!(
        "previous-{}.json",
        crate::check_record::seal_dir(base)
    ));
    let text = match std::fs::read_to_string(at) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let previous: Attempt = aterm_json::from_str(&text).map_err(|e| e.to_string())?;
    if !aterm_codec::hex::is_lower(&previous.tree, 40)
        && !aterm_codec::hex::is_lower(&previous.tree, 64)
    {
        return Err("previous input judgment has an invalid tree".into());
    }
    crate::check_record::input_item_candidates(records, &previous.tree, &previous.seal)
}

/// Remember the current attempt before dispatch, so an interrupted or failed run also replaces
/// the predecessor. A19 retains its begun/FAIL record independently of this pointer.
pub fn attempted(records: &Path, base: &str, tree: &str, seal: &str) -> Result<(), String> {
    let text = aterm_json::to_string(&Attempt {
        tree: tree.into(),
        seal: seal.into(),
    })
    .map_err(|e| e.to_string())?;
    crate::durable::write_atomic_durable(
        &records.join(format!(
            "previous-{}.json",
            crate::check_record::seal_dir(base)
        )),
        text.as_bytes(),
        0o600,
    )
    .map_err(|e| e.to_string())
}

/// Create and claim one record directory without waiting. A concurrent dry or real run must
/// not overwrite its writer's begun record. The guard unlocks explicitly before closing.
pub fn claim(directory: &Path) -> Result<aterm_uds::file_lock::LockFile, String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    aterm_uds::file_lock::LockFile::take(
        &directory.join("input.lock"),
        std::time::Instant::now(),
        "this release-input judgment is already running",
    )
    .map_err(|e| e.to_string())
}

/// This child names its own explicit lane. An outer targo's live authority endpoint and
/// the coding agent's invocation metadata do not belong in the source-check environment.
pub fn detach_launcher(command: &mut Command) {
    let names: std::collections::BTreeSet<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| {
            name.to_str()
                .is_some_and(|name| name.starts_with("TRUST_TARGO_") || name.starts_with("CODEX_"))
        })
        .collect();
    for name in names {
        command.env_remove(name);
    }
}

/// Explain a key without retaining environment values. These digests make a missed reuse
/// attributable to a changed input rather than an opaque change of the combined seal.
pub fn describe(command: &Command, seal: &str) -> Result<String, String> {
    let digest = |bytes: &[u8]| {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(bytes);
        aterm_codec::hex::encode_lower(&hash.finalize())
    };
    let mut env: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            env.insert(name.to_owned(), value.to_owned());
        } else {
            env.remove(name);
        }
    }
    let mut parts: std::collections::BTreeMap<String, String> = env
        .into_iter()
        .map(|(name, value)| {
            (
                format!("env:{}", name.to_string_lossy()),
                digest(value.as_encoded_bytes()),
            )
        })
        .collect();
    parts.insert("build-inputs".into(), digest(seal.as_bytes()));
    parts.insert(
        "driver".into(),
        crate::artifact::file_sha256(&std::env::current_exe().map_err(|e| e.to_string())?)?,
    );
    aterm_json::to_string(&parts).map_err(|e| e.to_string())
}

/// The tree-independent execution context of preflight readers. File content belongs in
/// each reader's key; external configuration, policy and the finalized environment do not.
pub fn context(
    root: &Path,
    engine: &Path,
    seal: &str,
    command: &Command,
) -> Result<String, String> {
    let driver =
        crate::artifact::file_sha256(&std::env::current_exe().map_err(|e| e.to_string())?)?;
    let mut hash = aterm_digest::Sha256::new();
    let mut part = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part(b"aterm-preflight-context-v1");
    part(
        crate::check_content::context(
            &driver,
            seal,
            command,
            crate::check_content::ReleaseHalf::Tree,
            crate::check_content::Admission::WaitDefault,
        )
        .as_bytes(),
    );
    for directory in root.ancestors() {
        for name in [
            ".cargo/config",
            ".cargo/config.toml",
            "trustfmt.toml",
            ".trustfmt.toml",
            "rustfmt.toml",
            ".rustfmt.toml",
            "clippy.toml",
            ".clippy.toml",
        ] {
            file(&mut part, &directory.join(name))?;
        }
    }
    if let Some(home) = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")))
    {
        for name in ["config", "config.toml"] {
            file(&mut part, &home.join(name))?;
        }
    }
    let policy = crate::ledger::GitCli::new(engine)
        .git(&["rev-parse", "HEAD"])
        .map_err(|e| e.to_string())?;
    if !policy.success() {
        return Err(policy.stderr_utf8());
    }
    part(policy.stdout_utf8().as_bytes());
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}

/// Bind the tree, discovered input, complete command environment, executable and build seal.
/// Configuration outside the tree and the publication policy have their own identities.
pub fn identity(
    git: &dyn GitRunner,
    root: &Path,
    engine: &Path,
    tip: &str,
    base: &str,
    seal: &str,
    command: &Command,
) -> Result<(String, String), String> {
    let tree = crate::check_record::tree_of(git, tip)?;
    let heads = git
        .git(&["rev-parse", "HEAD", &format!("{tip}^{{commit}}")])
        .map_err(|e| e.to_string())?;
    let names = heads.stdout_utf8();
    let names: Vec<_> = names.split_whitespace().collect();
    if !heads.success() || names.len() != 2 || names[0] != names[1] {
        return Err("release-input checkout moved away from the judged commit".into());
    }
    let status = git
        .git(&["status", "--porcelain", "--untracked-files=no"])
        .map_err(|e| e.to_string())?;
    if !status.success() || !status.stdout_utf8().is_empty() {
        return Err("release-input checkout has tracked changes".into());
    }
    let mut hash = aterm_digest::Sha256::new();
    let mut part = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part(b"aterm-release-input-v1");
    part(tree.as_bytes());
    part(base.as_bytes());
    let base_commit = git
        .git(&["rev-parse", "--verify", &format!("{base}^{{commit}}")])
        .map_err(|e| e.to_string())?;
    if !base_commit.success() {
        return Err(base_commit.stderr_utf8());
    }
    part(base_commit.stdout_utf8().as_bytes());
    part(seal.as_bytes());
    let image = std::env::current_exe().map_err(|e| e.to_string())?;
    let driver = crate::artifact::file_sha256(&image)?;
    part(driver.as_bytes());
    part(
        crate::check_content::context(
            &driver,
            seal,
            command,
            crate::check_content::ReleaseHalf::Tree,
            crate::check_content::Admission::WaitDefault,
        )
        .as_bytes(),
    );
    let input = aterm_verify::release_input::discover(
        root,
        &std::env::var_os("PATH").unwrap_or_default(),
        base,
        tip,
    )?;
    for check in aterm_verify::release_input::CHECKS {
        part(check.name().as_bytes());
    }
    for path in &input.paths {
        part(path.as_bytes());
    }
    for commit in &input.commits {
        part(commit.as_bytes());
    }
    // A19's tree key assumes a clean checkout. Include all nonignored additions too:
    // fmt and Cargo can read them even though they are absent from the commit tree.
    let listed = git
        .git(&["ls-files", "--others", "--exclude-standard", "-z"])
        .map_err(|e| e.to_string())?;
    if !listed.success() {
        return Err(listed.stderr_utf8());
    }
    for name in listed.stdout_utf8().split('\0').filter(|s| !s.is_empty()) {
        part(name.as_bytes());
        part(crate::artifact::file_sha256(&root.join(name))?.as_bytes());
    }
    for directory in root.ancestors() {
        for name in [
            ".cargo/config",
            ".cargo/config.toml",
            "trustfmt.toml",
            ".trustfmt.toml",
            "rustfmt.toml",
            ".rustfmt.toml",
            "clippy.toml",
            ".clippy.toml",
        ] {
            file(&mut part, &directory.join(name))?;
        }
    }
    if let Some(home) = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")))
    {
        for name in ["config", "config.toml"] {
            file(&mut part, &home.join(name))?;
        }
    }
    // Export's policy comes from the engine's immutable HEAD, not the aterm tree.
    let policy = crate::ledger::GitCli::new(engine)
        .git(&["rev-parse", "HEAD"])
        .map_err(|e| e.to_string())?;
    if !policy.success() {
        return Err(policy.stderr_utf8());
    }
    part(policy.stdout_utf8().as_bytes());
    Ok((tree, aterm_codec::hex::encode_lower(&hash.finalize())))
}

fn file(part: &mut impl FnMut(&[u8]), path: &Path) -> Result<(), String> {
    part(path.as_os_str().as_encoded_bytes());
    match std::fs::symlink_metadata(path) {
        Ok(_) => part(crate::artifact::file_sha256(path)?.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => part(b"absent"),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    }
    Ok(())
}

#[cfg(test)]
#[path = "input_record_tests.rs"]
mod tests;
