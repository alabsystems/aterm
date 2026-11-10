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
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        let out = crate::ledger::GitCli::new(root).git(args).unwrap();
        assert!(out.success(), "{}", out.stderr_utf8());
        out.stdout_utf8().trim().to_owned()
    }

    #[test]
    fn reader_context_ignores_product_version_but_binds_configuration_and_environment() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Fixture"]);
        git(root, &["config", "user.email", "fixture@example.invalid"]);
        git(root, &["commit", "--allow-empty", "-qm", "policy"]);
        let mut command = Command::new("fixed-verifier");
        command
            .current_dir(root)
            .env("INPUT_CONTEXT_CONTROL", "one");
        let before = context(root, root, "fixed-tools", &command).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion='1.0.0'\n",
        )
        .unwrap();
        assert_eq!(
            before,
            context(root, root, "fixed-tools", &command).unwrap()
        );
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion='1.1.0'\n",
        )
        .unwrap();
        assert_eq!(
            before,
            context(root, root, "fixed-tools", &command).unwrap()
        );
        std::fs::create_dir(root.join(".cargo")).unwrap();
        std::fs::write(root.join(".cargo/config.toml"), "[build]\njobs=2\n").unwrap();
        assert_ne!(
            before,
            context(root, root, "fixed-tools", &command).unwrap()
        );
        std::fs::remove_file(root.join(".cargo/config.toml")).unwrap();
        for name in aterm_verify::fmt::CONFIG_FILES {
            std::fs::write(root.join(name), "edition='2024'\n").unwrap();
            assert_ne!(
                before,
                context(root, root, "fixed-tools", &command).unwrap(),
                "the formatter reads {name}"
            );
            std::fs::remove_file(root.join(name)).unwrap();
        }
        command.env("INPUT_CONTEXT_CONTROL", "two");
        assert_ne!(
            before,
            context(root, root, "fixed-tools", &command).unwrap()
        );
        command.env("INPUT_CONTEXT_CONTROL", "one");
        assert_ne!(
            before,
            context(root, root, "changed-tools", &command).unwrap()
        );
    }

    #[test]
    fn one_writer_per_input_key_without_blocking_other_keys() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let other = dir.path().join("other-key");
        let held = claim(dir.path()).unwrap();
        assert!(claim(dir.path()).is_err());
        let independent = claim(&other).unwrap();
        assert!(other.join("input.lock").is_file());
        assert!(claim(&other).is_err());
        drop(held);
        let resumed = claim(dir.path()).unwrap();
        drop((independent, resumed));
    }

    #[test]
    fn stable_checkout_moves_clean_tips_but_preserves_unexpected_work() {
        let dir = aterm_tempfile::Builder::new().heard().tempdir().unwrap();
        assert!(!aterm_tempfile::is_quiet(dir.path()));
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        git(&source, &["init", "-q"]);
        git(&source, &["config", "user.name", "Fixture"]);
        git(
            &source,
            &["config", "user.email", "fixture@example.invalid"],
        );
        std::fs::write(source.join("input"), "first").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-qm", "first"]);
        let first = git(&source, &["rev-parse", "HEAD"]);
        let owned = directory(dir.path());
        let runner = crate::ledger::GitCli::new(&source);
        checkout(&runner, &owned, &first).unwrap();
        assert!(aterm_tempfile::is_quiet(&owned));
        std::fs::write(source.join("input"), "second").unwrap();
        git(&source, &["commit", "-qam", "second"]);
        let second = git(&source, &["rev-parse", "HEAD"]);
        checkout(&runner, &owned, &second).unwrap();
        assert_eq!(
            std::fs::read_to_string(owned.join("input")).unwrap(),
            "second"
        );
        std::fs::write(owned.join("input"), "unexpected work").unwrap();
        assert!(checkout(&runner, &owned, &first).is_err());
        assert_eq!(
            std::fs::read_to_string(owned.join("input")).unwrap(),
            "unexpected work"
        );
        let foreign = dir.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        assert!(checkout(&runner, &foreign, &first).is_err());
        git(
            &source,
            &["worktree", "remove", "--force", owned.to_str().unwrap()],
        );
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
