// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The landing-time half of the harness-version invariant.
//!
//! `ship harness-bump`, `ship release --bump` and the qualify gate compare with the previous
//! public release. This check asks the same question against the newest fetched public `v*` tag
//! in the candidate's ancestry. It catches debt while a change lands without querying the
//! network, and an abandoned ledger claim or workspace-version bump cannot reset the window.
//! The landing workflow's required `git fetch origin` imports those tags; qualify remains the
//! independent remote-tag check.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use aterm_phase::CliHarness;
use aterm_phase::harness_version::{declared_in_source, source_line};

const MANIFEST: &str = "Cargo.toml";
const READ_WAIT: Duration = Duration::from_secs(60);

/// One harness whose sources moved since the previous public release without its compiled version
/// moving past that release's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Debt {
    harness: CliHarness,
    at: (u16, u16),
    current: Option<(u16, u16)>,
    moved: Vec<String>,
}

impl Debt {
    pub fn line(&self) -> String {
        let current = self.current.map_or_else(
            || "no valid HARNESS_VER".to_string(),
            |(major, minor)| format!("{major}.{minor}"),
        );
        let to = (self.at.0, self.at.1.saturating_add(1));
        format!(
            "alab {} harness {} (at the previous public release {}.{}) owes {} in {}: {} changed ({})",
            self.harness.cli(),
            current,
            self.at.0,
            self.at.1,
            source_line(to),
            self.harness.version_file(),
            self.moved.len(),
            self.moved.first().map_or("", String::as_str),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judgement {
    Clear {
        version: String,
        baseline: String,
    },
    Owed {
        version: String,
        baseline: String,
        debts: Vec<Debt>,
    },
}

fn call(root: &Path, path_env: &OsStr, args: &[&str]) -> Result<std::process::Output, String> {
    let mut git = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
    git.current_dir(root)
        .env("PATH", path_env)
        .arg("--no-optional-locks")
        .args(args)
        .stdin(aterm_uds::spawnfd::Stdio::null());
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            git.env_remove(key);
        }
    }
    git.env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    aterm_uds::deadline::output_within(&mut git, READ_WAIT).map_err(|error| {
        format!(
            "git {} did not answer within {} seconds: {error}",
            args.join(" "),
            READ_WAIT.as_secs()
        )
    })
}

fn git(root: &Path, path_env: &OsStr, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = call(root, path_env, args)?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    Err(format!(
        "git {} exited {}: {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn text(root: &Path, path_env: &OsStr, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git(root, path_env, args)?)
        .map_err(|_| format!("git {} returned non-UTF-8 text", args.join(" ")))
}

fn blob_at(
    root: &Path,
    path_env: &OsStr,
    commit: &str,
    path: &str,
) -> Result<Option<String>, String> {
    let listed = git(root, path_env, &["ls-tree", "-z", commit, "--", path])?;
    if listed.is_empty() {
        return Ok(None);
    }
    let object = format!("{commit}:{path}");
    text(root, path_env, &["cat-file", "blob", &object]).map(Some)
}

fn workspace_version(text: &str) -> Option<String> {
    aterm_toml::from_str::<aterm_toml::Value>(text)
        .ok()?
        .get("workspace")?
        .get("package")?
        .get("version")?
        .as_str()
        .map(str::to_string)
}

fn changed_between(
    root: &Path,
    path_env: &OsStr,
    from: &str,
    to: &str,
) -> Result<Vec<String>, String> {
    git(
        root,
        path_env,
        &["diff", "--name-only", "-z", "--no-renames", from, to, "--"],
    )?
    .split(|byte| *byte == 0)
    .filter(|path| !path.is_empty())
    .map(|path| {
        String::from_utf8(path.to_vec())
            .map_err(|_| "git diff returned a non-UTF-8 path".to_string())
    })
    .collect()
}

fn release_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    (parts.next().is_none() && version.2 == 0).then_some(version)
}

/// The newest fetched public release tag no newer than the workspace version and reachable from
/// `head`. Annotated tags contribute their peeled commit; lightweight tags their object.
fn public_baseline(
    root: &Path,
    path_env: &OsStr,
    head: &str,
    current: &str,
) -> Result<(String, String), String> {
    let current = release_version(current)
        .ok_or_else(|| format!("workspace version {current:?} is not MAJOR.MINOR.0"))?;
    let merged = format!("--merged={head}");
    let catalog = text(
        root,
        path_env,
        &[
            "for-each-ref",
            &merged,
            "--format=%(refname:strip=2)%00%(*objectname)%00%(objectname)%00",
            "refs/tags/v*",
        ],
    )?;
    let mut releases = Vec::new();
    for line in catalog.lines() {
        let mut fields = line.split('\0');
        let (Some(tag), Some(peeled), Some(object)) = (fields.next(), fields.next(), fields.next())
        else {
            return Err("git for-each-ref returned a malformed tag row".to_string());
        };
        let Some(version) = release_version(tag) else {
            continue;
        };
        if version <= current {
            let commit = if peeled.is_empty() { object } else { peeled };
            releases.push((version, tag.to_string(), commit.to_string()));
        }
    }
    releases.sort_unstable_by_key(|release| release.0);
    let Some((_, tag, commit)) = releases.pop() else {
        return Err(format!(
            "no fetched public vMAJOR.MINOR.0 tag at or before the workspace version is in {head}"
        ));
    };
    let commit = text(
        root,
        path_env,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )?
    .trim()
    .to_string();
    Ok((tag.trim_start_matches('v').to_string(), commit))
}

fn matches_source(source: &str, path: &str) -> bool {
    source
        .strip_suffix('/')
        .map_or(path == source, |_| path.starts_with(source))
}

fn is_source(harness: CliHarness, path: &str) -> bool {
    harness
        .sources()
        .iter()
        .any(|source| matches_source(source, path))
}

/// Judge `head` against the newest fetched public release tag in its ancestry.
pub fn judge(root: &Path, head: &str, path_env: &OsStr) -> Result<Judgement, String> {
    let head = text(
        root,
        path_env,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?
    .trim()
    .to_string();
    let manifest = blob_at(root, path_env, &head, MANIFEST)?
        .ok_or_else(|| format!("{head} holds no {MANIFEST}"))?;
    let current = workspace_version(&manifest)
        .ok_or_else(|| format!("{head}:{MANIFEST} names no workspace.package.version"))?;
    let (version, baseline) = public_baseline(root, path_env, &head, &current)?;
    let changed = changed_between(root, path_env, &baseline, &head)?;
    let mut debts = Vec::new();
    for harness in CliHarness::ALL {
        let mut moved: Vec<String> = changed
            .iter()
            .filter(|path| is_source(harness, path))
            .cloned()
            .collect();
        if moved.is_empty() {
            continue;
        }
        moved.sort_unstable();
        let Some(before) = blob_at(root, path_env, &baseline, harness.version_file())?
            .as_deref()
            .and_then(declared_in_source)
        else {
            continue;
        };
        let current = blob_at(root, path_env, &head, harness.version_file())?
            .as_deref()
            .and_then(declared_in_source);
        if current.is_none_or(|now| now <= before) {
            debts.push(Debt {
                harness,
                at: before,
                current,
                moved,
            });
        }
    }
    if debts.is_empty() {
        Ok(Judgement::Clear { version, baseline })
    } else {
        Ok(Judgement::Owed {
            version,
            baseline,
            debts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run(root: &Path, args: &[&str]) {
        let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"))
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn commit(root: &Path, message: &str) {
        run(root, &["add", "."]);
        run(root, &["commit", "-qm", message]);
    }

    fn tag(root: &Path, version: &str) {
        run(
            root,
            &["tag", "-am", "public release", &format!("v{version}")],
        );
    }

    fn init(root: &Path) {
        run(root, &["init", "-q"]);
        run(root, &["config", "user.name", "A"]);
        run(root, &["config", "user.email", "a@example.invalid"]);
    }

    fn manifest(version: &str) -> String {
        format!("[workspace]\n[workspace.package]\nversion = \"{version}\"\n")
    }

    #[test]
    fn a_late_harness_change_is_red_until_the_window_has_one_bump() {
        let temp = aterm_tempfile::TempDir::new().unwrap();
        let root = temp.path();
        init(root);
        write(root, MANIFEST, &manifest("0.173.0"));
        write(
            root,
            CliHarness::ClaudeCode.version_file(),
            &format!("{}\n", source_line((0, 1))),
        );
        write(
            root,
            "crates/aterm-agent/src/harness/upgrade_drive.rs",
            "fn drive() {}\n",
        );
        commit(root, "public release");
        tag(root, "0.173.0");

        write(
            root,
            "crates/aterm-agent/src/harness/upgrade_drive.rs",
            "fn drive() { changed(); }\n",
        );
        commit(root, "harness source before the workspace bump");
        write(root, MANIFEST, &manifest("0.174.0"));
        commit(
            root,
            "workspace version bump does not reset the public window",
        );
        let path = std::env::var_os("PATH").unwrap_or_default();
        let Judgement::Owed { debts, .. } = judge(root, "HEAD", &path).unwrap() else {
            panic!("unbumped source change passed");
        };
        assert_eq!(debts.len(), 1);
        assert_eq!(debts[0].harness, CliHarness::ClaudeCode);

        // NEGATIVE CONTROL: reverting the landing check makes the preceding assertion pass the
        // bad tree; moving the compiled version once clears this release window's debt.
        write(
            root,
            CliHarness::ClaudeCode.version_file(),
            &format!("{}\n", source_line((0, 2))),
        );
        commit(root, "bump harness");
        assert!(matches!(
            judge(root, "HEAD", &path).unwrap(),
            Judgement::Clear { .. }
        ));

        write(
            root,
            "crates/aterm-agent/src/harness/upgrade_drive.rs",
            "fn drive() { changed_again(); }\n",
        );
        commit(root, "a second source change in the same public window");
        assert!(matches!(
            judge(root, "HEAD", &path).unwrap(),
            Judgement::Clear { .. }
        ));

        tag(root, "0.174.0");
        write(root, MANIFEST, &manifest("0.175.0"));
        commit(root, "next workspace version");
        write(
            root,
            "crates/aterm-agent/src/harness/upgrade_drive.rs",
            "fn drive() { next_public_window(); }\n",
        );
        commit(root, "change after the next public release");
        assert!(matches!(
            judge(root, "HEAD", &path).unwrap(),
            Judgement::Owed { .. }
        ));
    }

    #[test]
    fn an_unbumped_source_cannot_hide_behind_a_later_version_or_ledger_commit() {
        let temp = aterm_tempfile::TempDir::new().unwrap();
        let root = temp.path();
        init(root);
        write(root, MANIFEST, &manifest("0.8.0"));
        write(
            root,
            CliHarness::ClaudeCode.version_file(),
            &format!("{}\n", source_line((0, 3))),
        );
        write(
            root,
            "crates/aterm-agent/src/harness/upgrade_drive.rs",
            "fn drive() {}\n",
        );
        commit(root, "public release");
        tag(root, "0.8.0");
        write(
            root,
            "crates/aterm-agent/src/harness/upgrade_drive.rs",
            "fn drive() { hidden(); }\n",
        );
        commit(root, "unbumped source");
        write(root, MANIFEST, &manifest("0.9.0"));
        commit(root, "later workspace bump");
        write(root, "RELEASES.ledger", "100 0.9.0\n");
        commit(root, "later abandoned claim");
        let path = std::env::var_os("PATH").unwrap_or_default();
        assert!(matches!(
            judge(root, "HEAD", &path).unwrap(),
            Judgement::Owed { .. }
        ));
    }

    #[test]
    fn a_directory_source_matches_only_inside_its_slash_boundary() {
        assert!(matches_source(
            "crates/harness/",
            "crates/harness/reader.rs"
        ));
        assert!(!matches_source(
            "crates/harness/",
            "crates/harness-extra.rs"
        ));
    }
}
