// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The landing-time half of the harness-version invariant.
//!
//! `ship harness-bump`, `ship release --bump` and the qualify gate compare with the previous
//! public release. This check asks the same question against the newest fetched public `v*` tag
//! in the candidate's ancestry. It catches debt while a change lands without querying the
//! network for releases, and an abandoned ledger claim or workspace-version bump cannot reset
//! the window. The landing workflow's required `git fetch origin` imports those tags; qualify
//! remains the independent remote-tag check. A blobless partial clone holds the baseline's
//! trees but not its files, so the few version-file blobs the judgement reads and the clone
//! lacks are imported in one explicit, bounded request to its promisor remote, through the
//! verifier's remote boundary; no read ever fetches lazily.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_phase::CliHarness;
use aterm_phase::harness_version::{declared_in_source, source_line};

const MANIFEST: &str = "Cargo.toml";
const READ_WAIT: Duration = Duration::from_secs(60);
/// The hang detector on the one import of the blobs a partial clone lacks; a test's import is a
/// local fixture, held to the one-minute hang detector every test wait gets.
const IMPORT_WAIT: Duration = Duration::from_secs(if cfg!(test) { 60 } else { 600 });

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

/// One local git read. A missing object is an error, never a fetch: `GIT_NO_LAZY_FETCH`, and an
/// empty `GIT_ALLOW_PROTOCOL` for a git too old to obey it, keep a partial clone's read from
/// reaching its promisor remote by itself, outside the verifier's remote boundary
/// ([`crate::remote`]: GitHub only through agh's credential route). The blobs a judgement needs
/// and the clone lacks are imported explicitly instead ([`Reads::files`]).
fn call(
    root: &Path,
    path_env: &OsStr,
    args: &[&str],
    input: Option<&[u8]>,
) -> Result<std::process::Output, String> {
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
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    let ran = match input {
        None => aterm_uds::deadline::output_within(&mut git, READ_WAIT).map_err(|e| e.to_string()),
        Some(input) => aterm_uds::deadline::output_with_input_until(
            &mut git,
            aterm_uds::secret::Secret::new(input),
            Instant::now() + READ_WAIT,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        )
        .map_err(|e| e.to_string()),
    };
    ran.map_err(|error| {
        format!(
            "git {} did not answer within {} seconds: {error}",
            args.join(" "),
            READ_WAIT.as_secs()
        )
    })
}

fn git(root: &Path, path_env: &OsStr, args: &[&str]) -> Result<Vec<u8>, String> {
    succeeded(args, call(root, path_env, args, None)?)
}

fn succeeded(args: &[&str], out: std::process::Output) -> Result<Vec<u8>, String> {
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

/// The file reads one judgement makes, local apart from one explicit import.
struct Reads<'a> {
    root: &'a Path,
    path_env: &'a OsStr,
}

impl Reads<'_> {
    /// The blob `commit`'s tree names at each of `paths`, by path; a path it lacks is absent.
    /// Only trees are read, which a blobless partial clone holds.
    fn entries(&self, commit: &str, paths: &[&str]) -> Result<BTreeMap<String, String>, String> {
        let mut args = vec!["ls-tree", "-z", commit, "--"];
        args.extend(paths);
        let listed = git(self.root, self.path_env, &args)?;
        let mut entries = BTreeMap::new();
        for row in listed
            .split(|byte| *byte == 0)
            .filter(|row| !row.is_empty())
        {
            let row = std::str::from_utf8(row)
                .map_err(|_| format!("git ls-tree {commit} returned a non-UTF-8 row"))?;
            let (meta, path) = row
                .split_once('\t')
                .ok_or_else(|| format!("git ls-tree {commit} returned a malformed row"))?;
            let mut words = meta.split(' ');
            let (Some(_mode), Some(kind), Some(oid), None) =
                (words.next(), words.next(), words.next(), words.next())
            else {
                return Err(format!("git ls-tree {commit} returned a malformed row"));
            };
            if !paths.contains(&path) {
                continue;
            }
            if kind != "blob" || !aterm_types::land_trailer::object_id(oid) {
                return Err(format!("{commit}:{path} is a {kind}, not a file"));
            }
            entries.insert(path.to_string(), oid.to_string());
        }
        Ok(entries)
    }

    /// Which of `oids` this repository lacks, answered by one local batch read.
    fn missing(&self, oids: &BTreeSet<&str>) -> Result<Vec<String>, String> {
        const ARGS: &[&str] = &["cat-file", "--batch-check=%(objectname) %(objecttype)"];
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let input: String = oids.iter().map(|oid| format!("{oid}\n")).collect();
        let out = succeeded(
            ARGS,
            call(self.root, self.path_env, ARGS, Some(input.as_bytes()))?,
        )?;
        let out = String::from_utf8(out).map_err(|_| "git cat-file returned non-UTF-8 rows")?;
        let mut rows = out.lines();
        let mut missing = Vec::new();
        for oid in oids {
            match rows.next().and_then(|row| row.split_once(' ')) {
                Some((found, "blob")) if found == *oid => {}
                Some((found, "missing")) if found == *oid => missing.push((*oid).to_string()),
                _ => return Err(format!("git cat-file did not describe blob {oid}")),
            }
        }
        if rows.next().is_some() {
            return Err("git cat-file described an object nobody asked for".to_string());
        }
        Ok(missing)
    }

    /// The text of each `(commit, path)` in `wanted`, `None` where that commit has no such
    /// path. The blobs this partial clone lacks are imported first, in one explicit request to
    /// its promisor remote ([`Reads::import`]); the reads themselves never fetch.
    ///
    /// # Errors
    /// A git read failed, a path names no file, or a blob is absent and could not be imported:
    /// the message names each absent file and the remedy.
    fn files(&self, wanted: &[(&str, &str)]) -> Result<Vec<Option<String>>, String> {
        let mut by_commit: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (commit, path) in wanted {
            by_commit.entry(commit).or_default().push(path);
        }
        let mut blobs = BTreeMap::new();
        for (commit, paths) in by_commit {
            for (path, oid) in self.entries(commit, &paths)? {
                blobs.insert((commit, path), oid);
            }
        }
        let oids: BTreeSet<&str> = blobs.values().map(String::as_str).collect();
        let missing = self.missing(&oids)?;
        if !missing.is_empty() {
            let named: Vec<String> = blobs
                .iter()
                .filter(|(_, oid)| missing.contains(oid))
                .map(|((commit, path), _)| format!("{path} at {}", &commit[..12.min(commit.len())]))
                .collect();
            let why = self.import(&missing).and_then(|()| {
                let left = self.missing(&oids)?;
                if left.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "the promisor remote did not supply {}",
                        left.join(" ")
                    ))
                }
            });
            if let Err(why) = why {
                return Err(format!(
                    "{} blob{} this partial clone lacks ({}): {why}\nfix: re-run where this \
                     clone's promisor remote is reachable (GitHub through an agh with `repo \
                     fetch --objects`), which imports exactly these objects; or import them \
                     by hand: git fetch --no-tags --no-write-fetch-head <promisor remote> {}",
                    missing.len(),
                    if missing.len() == 1 { "" } else { "s" },
                    named.join(", "),
                    missing.join(" ")
                ));
            }
        }
        wanted
            .iter()
            .map(|(commit, path)| {
                let Some(oid) = blobs.get(&(*commit, (*path).to_string())) else {
                    return Ok(None);
                };
                text(self.root, self.path_env, &["cat-file", "blob", oid]).map(Some)
            })
            .collect()
    }

    /// The remotes that promise this partial clone's missing objects, in the order git's own
    /// lazy fetch asks them: `extensions.partialClone`'s, then each `remote.<name>.promisor`.
    fn promisors(&self) -> Result<Vec<String>, String> {
        let read = |args: &[&str]| -> Result<String, String> {
            let out = call(self.root, self.path_env, args, None)?;
            if out.status.code() == Some(1) {
                return Ok(String::new());
            }
            String::from_utf8(succeeded(args, out)?)
                .map_err(|_| format!("git {} returned non-UTF-8 text", args.join(" ")))
        };
        let mut names: Vec<String> = read(&["config", "--get", "extensions.partialClone"])?
            .lines()
            .map(str::to_string)
            .collect();
        for row in read(&[
            "config",
            "--type=bool",
            "--get-regexp",
            r"^remote\..*\.promisor$",
        ])?
        .lines()
        {
            let Some((key, value)) = row.rsplit_once(' ') else {
                continue;
            };
            let name = key
                .strip_prefix("remote.")
                .and_then(|key| key.strip_suffix(".promisor"));
            if let Some(name) = name.filter(|_| value == "true")
                && !names.iter().any(|known| known == name)
            {
                names.push(name.to_string());
            }
        }
        Ok(names)
    }

    /// Import exactly `objects` from the remote that promised them ([`Reads::promisors`],
    /// the first that answers), through the verifier's remote boundary: git for a local
    /// repository, agh for GitHub. No ref, index or worktree changes.
    fn import(&self, objects: &[String]) -> Result<(), String> {
        let promisors = self.promisors()?;
        if promisors.is_empty() {
            return Err(
                "this repository is not a partial clone, so no remote promises them".to_string(),
            );
        }
        let make = |root: &Path| {
            let mut git = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
            git.current_dir(root).env("PATH", self.path_env);
            git
        };
        let until = Instant::now() + IMPORT_WAIT;
        let run = |tool, args: &[std::ffi::OsString]| {
            crate::remote::run(self.root, &make, tool, args, until)
        };
        let mut failed = Vec::new();
        for name in &promisors {
            match crate::remote::Remote::open(self.root, name, &run)
                .and_then(|remote| remote.import(objects))
            {
                Ok(()) => return Ok(()),
                Err(why) => failed.push(format!("importing them from `{name}` failed: {why}")),
            }
        }
        Err(failed.join("; "))
    }
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

/// Judge `head` against the newest fetched public release tag in its ancestry. Every file it
/// reads comes from the local object store; where a partial clone lacks some of them (a new
/// public baseline's version files, which nobody here has checked out), exactly those blobs
/// are imported first in one explicit request to the clone's promisor remote ([`Reads`]).
///
/// # Errors
/// A git read failed, the workspace version or public baseline is unreadable, or a blob the
/// judgement needs is absent and could not be imported (offline, say): the message names
/// each such file and the remedy.
pub fn judge(root: &Path, head: &str, path_env: &OsStr) -> Result<Judgement, String> {
    let reads = Reads { root, path_env };
    let head = text(
        root,
        path_env,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?
    .trim()
    .to_string();
    let manifest = reads
        .files(&[(&head, MANIFEST)])?
        .pop()
        .flatten()
        .ok_or_else(|| format!("{head} holds no {MANIFEST}"))?;
    let current = workspace_version(&manifest)
        .ok_or_else(|| format!("{head}:{MANIFEST} names no workspace.package.version"))?;
    let (version, baseline) = public_baseline(root, path_env, &head, &current)?;
    let changed = changed_between(root, path_env, &baseline, &head)?;
    let moved: Vec<(CliHarness, Vec<String>)> = CliHarness::ALL
        .into_iter()
        .filter_map(|harness| {
            let mut moved: Vec<String> = changed
                .iter()
                .filter(|path| is_source(harness, path))
                .cloned()
                .collect();
            moved.sort_unstable();
            (!moved.is_empty()).then_some((harness, moved))
        })
        .collect();
    let wanted: Vec<(&str, &str)> = moved
        .iter()
        .flat_map(|(harness, _)| {
            [
                (baseline.as_str(), harness.version_file()),
                (head.as_str(), harness.version_file()),
            ]
        })
        .collect();
    let mut texts = reads.files(&wanted)?.into_iter();
    let mut debts = Vec::new();
    for (harness, moved) in moved {
        let (Some(before), Some(now)) = (texts.next(), texts.next()) else {
            return Err("a version file read returned fewer files than were asked".to_string());
        };
        let Some(before) = before.as_deref().and_then(declared_in_source) else {
            continue;
        };
        let current = now.as_deref().and_then(declared_in_source);
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
        let mut git = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
        git.current_dir(root).args(args);
        let out = aterm_uds::deadline::output_within(&mut git, READ_WAIT).unwrap();
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

    /// Host git under no system or global config and no inherited repository redirect.
    fn fixture_git(root: &Path, args: &[&str]) -> String {
        let mut git = aterm_uds::spawnfd::command(aterm_uds::os_path!("git"));
        git.current_dir(root).args(args);
        for (key, _) in std::env::vars_os() {
            if key.as_encoded_bytes().starts_with(b"GIT_") {
                git.env_remove(key);
            }
        }
        git.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.invalid")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.invalid");
        let out = aterm_uds::deadline::output_within(&mut git, READ_WAIT).unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Whether `root` holds `oid` itself, asked with no lazy fetch.
    fn holds(root: &Path, oid: &str) -> bool {
        let path = std::env::var_os("PATH").unwrap_or_default();
        call(root, &path, &["cat-file", "-e", oid], None)
            .unwrap()
            .status
            .success()
    }

    /// A public release whose version file the candidate edits without bumping, served by a
    /// local bare repository that answers filtered and exact-object requests, and two blobless
    /// partial clones of it: the shape of a checkout whose newest public baseline's files
    /// nobody here has read.
    fn partial_clones(root: &Path) -> (std::path::PathBuf, [std::path::PathBuf; 2], String) {
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        fixture_git(&work, &["init", "-q", "-b", "main"]);
        let version_file = CliHarness::ClaudeCode.version_file();
        write(&work, MANIFEST, &manifest("0.8.0"));
        write(&work, version_file, &format!("{}\n", source_line((0, 3))));
        write(
            &work,
            "crates/aterm-agent/src/harness/upgrade_drive.rs",
            "fn drive() {}\n",
        );
        fixture_git(&work, &["add", "."]);
        fixture_git(&work, &["commit", "-qm", "public release"]);
        fixture_git(&work, &["tag", "-am", "public release", "v0.8.0"]);
        let baseline_blob = fixture_git(&work, &["rev-parse", &format!("v0.8.0:{version_file}")]);
        write(
            &work,
            "crates/aterm-agent/src/harness/upgrade_drive.rs",
            "fn drive() { changed(); }\n",
        );
        write(
            &work,
            version_file,
            &format!("{}\n// edited, not bumped\n", source_line((0, 3))),
        );
        write(&work, MANIFEST, &manifest("0.9.0"));
        fixture_git(&work, &["add", "."]);
        fixture_git(&work, &["commit", "-qm", "unbumped source"]);
        let server = root.join("server.git");
        let server_text = server.to_str().unwrap();
        fixture_git(root, &["clone", "-q", "--bare", "work", server_text]);
        fixture_git(&server, &["config", "uploadpack.allowFilter", "true"]);
        fixture_git(
            &server,
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
        );
        let clones = ["partial", "offline"].map(|name| {
            let clone = root.join(name);
            fixture_git(
                root,
                &[
                    "clone",
                    "-q",
                    "--filter=blob:none",
                    &format!("file://{server_text}"),
                    clone.to_str().unwrap(),
                ],
            );
            // The verifier's remote boundary admits a local repository by its path.
            fixture_git(&clone, &["remote", "set-url", "origin", server_text]);
            clone
        });
        (server, clones, baseline_blob)
    }

    #[test]
    fn a_partial_clone_imports_exactly_the_baseline_blobs_it_lacks_or_names_them() {
        let temp = aterm_tempfile::TempDir::new().unwrap();
        let (server, [partial, offline], baseline_blob) = partial_clones(temp.path());
        let path = std::env::var_os("PATH").unwrap_or_default();

        // NEGATIVE CONTROL: the clone lacks the baseline's version file, so a read with lazy
        // fetch off (the judgement before the import, which reported COULD NOT RUN) fails.
        assert!(!holds(&partial, &baseline_blob));
        let refs = fixture_git(&partial, &["for-each-ref"]);
        let Judgement::Owed { debts, .. } = judge(&partial, "HEAD", &path).unwrap() else {
            panic!("an edited, unbumped version file passed");
        };
        assert_eq!(debts.len(), 1);
        assert_eq!(debts[0].harness, CliHarness::ClaudeCode);
        assert!(holds(&partial, &baseline_blob), "the import supplied it");
        assert_eq!(
            fixture_git(&partial, &["for-each-ref"]),
            refs,
            "an import publishes no ref"
        );

        // With the promisor remote gone the blob stays absent, and the verdict says which file
        // and how to get it rather than judging without it.
        fs::rename(&server, temp.path().join("gone.git")).unwrap();
        let why = judge(&offline, "HEAD", &path).unwrap_err();
        assert!(!holds(&offline, &baseline_blob));
        assert!(
            why.contains("1 blob this partial clone lacks")
                && why.contains(&format!("{} at ", CliHarness::ClaudeCode.version_file()))
                && why.contains("importing them from `origin` failed")
                && why.contains("fix: re-run where this clone's promisor remote is reachable")
                && why.contains(&format!("<promisor remote> {baseline_blob}")),
            "{why}"
        );
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
