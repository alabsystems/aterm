// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Selection-local A19 records for the release's early source judgment.
//! These never answer a whole-workspace qualify question.

use std::path::Path;

use crate::ledger::GitRunner;
use aterm_uds::spawnfd::Command;

/// Claim one existing record directory without waiting. A concurrent dry or real run must
/// not overwrite its writer's begun record. The guard unlocks explicitly before closing.
pub fn claim(directory: &Path) -> Result<aterm_uds::file_lock::LockFile, String> {
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
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        let out = crate::ledger::GitCli::new(root).git(args).unwrap();
        assert!(out.success(), "{}", out.stderr_utf8());
        out.stdout_utf8().trim().to_owned()
    }

    #[test]
    fn one_writer_per_input_key_without_blocking_other_keys() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let other = dir.path().join("other-key");
        std::fs::create_dir(&other).unwrap();
        let held = claim(dir.path()).unwrap();
        assert!(claim(dir.path()).is_err());
        let independent = claim(&other).unwrap();
        drop(held);
        let resumed = claim(dir.path()).unwrap();
        drop((independent, resumed));
    }

    #[test]
    fn sixty_eight_commits_and_377_paths_bind_tree_cone_tools_and_environment() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Fixture"]);
        git(root, &["config", "user.email", "fixture@example.invalid"]);
        git(root, &["commit", "--allow-empty", "-qm", "base"]);
        let base = git(root, &["rev-parse", "HEAD"]);
        for commit in 0..68 {
            for index in (commit..377).step_by(68) {
                std::fs::write(root.join(format!("source-{index}.txt")), "source\n").unwrap();
            }
            git(root, &["add", "."]);
            git(root, &["commit", "-qm", "unreceipted"]);
        }
        let tip = git(root, &["rev-parse", "HEAD"]);
        let input = aterm_verify::release_input::discover(
            root,
            &std::env::var_os("PATH").unwrap_or_default(),
            &base,
            &tip,
        )
        .unwrap();
        assert_eq!((input.commits.len(), input.paths.len()), (68, 377));
        let runner = crate::ledger::GitCli::new(root);
        let mut command = aterm_uds::spawnfd::command("fixture-verifier");
        command.current_dir(root);
        detach_launcher(&mut command);
        let key = |command: &Command, seal: &str| {
            identity(&runner, root, root, &tip, &base, seal, command).unwrap()
        };
        let start = std::time::Instant::now();
        let first = key(&command, "tools-one");
        assert_eq!(first, key(&command, "tools-one"));
        command.env("TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT", "first-invocation");
        command.env("CODEX_TOOL_CALL_ID", "first-call");
        detach_launcher(&mut command);
        let detached = key(&command, "tools-one");
        assert_eq!(first, detached);
        command.env("TRUST_TARGO_NESTED_UNVERIFIED_ENDPOINT", "next-invocation");
        command.env("CODEX_TOOL_CALL_ID", "next-call");
        detach_launcher(&mut command);
        assert_eq!(detached, key(&command, "tools-one"));
        assert_ne!(first, key(&command, "tools-two"));
        command.env("INPUT_FIXTURE_FLAG", "changed");
        assert_ne!(first, key(&command, "tools-one"));
        let explanation = describe(&command, "tools-one").unwrap();
        assert!(explanation.contains("env:INPUT_FIXTURE_FLAG"));
        assert!(!explanation.contains("changed"));
        assert!(!explanation.contains("CODEX_TOOL_CALL_ID"));
        let before_source = key(&command, "tools-one");
        std::fs::write(root.join("source-0.txt"), "changed\n").unwrap();
        assert!(identity(&runner, root, root, &tip, &base, "tools-one", &command).is_err());
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "changed source"]);
        let changed = git(root, &["rev-parse", "HEAD"]);
        assert_ne!(
            before_source.1,
            identity(&runner, root, root, &changed, &base, "tools-one", &command)
                .unwrap()
                .1
        );
        git(
            root,
            &[
                "commit",
                "--allow-empty",
                "-qm",
                "same tree, different history",
            ],
        );
        assert!(identity(&runner, root, root, &changed, &base, "tools-one", &command).is_err());
        println!(
            "release-input key fixture: 68 commits, 377 paths; repeated_and_invalidated_s={:.3}",
            start.elapsed().as_secs_f64()
        );
    }
}
