// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one clean-tree judge of a release: the paths of a checkout whose working tree
//! differs from its `HEAD`, as a compiler reads them. `ship release` judges its clone with
//! it, the cut its tree and its operator's checkout, so no step calls a tree clean that
//! another calls dirty.
//!
//! What counts:
//! - tracked changes, staged or not (`git diff HEAD`, renames off so a moved file is
//!   listed at both ends, submodules never ignored whatever the config says);
//! - untracked files the tree's own `.gitignore` files do not ignore — never
//!   `.git/info/exclude` or `core.excludesFile`, which are one checkout's config — and an
//!   untracked `.gitignore` those rules ignore, since it hides what it names;
//! - edits an assume-unchanged or skip-worktree flag hides from every git listing;
//! - beneath each submodule, its own such paths (`<gitlink>/<path>`), the gitlink itself
//!   dirty with them; a submodule whose checkout is not there is [`Dirt::Unpopulated`],
//!   and a file or link where one belongs is [`Dirt::Changed`], never followed.

use aterm_uds::spawnfd::command;
use std::collections::BTreeMap;
use std::path::Path;

/// How a path differs from `HEAD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dirt {
    /// Its working tree is not what `HEAD` records.
    Changed,
    /// A submodule `HEAD` records whose checkout is not there (never initialised, or its
    /// `.git` broken): no build of this tree can read that source.
    Unpopulated,
}

/// Every path of the checkout at `root` that differs from its `HEAD` (the module's
/// rule). `None` when git cannot answer.
#[must_use]
pub fn dirty_against_head(root: &Path) -> Option<BTreeMap<String, Dirt>> {
    let index = index_entries(root)?;
    let tracked = out_z(
        root,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--ignore-submodules=none",
            "HEAD",
            "--",
        ],
    )?;
    let untracked = out_z(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--exclude-per-directory=.gitignore",
        ],
    )?;
    let ignored_rules = out_z(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--directory",
            "--exclude-per-directory=.gitignore",
        ],
    )?
    .into_iter()
    .filter(|p| p == ".gitignore" || p.ends_with("/.gitignore"));
    let mut dirty: BTreeMap<String, Dirt> = tracked
        .into_iter()
        .chain(untracked)
        .chain(ignored_rules)
        .chain(flag_hidden_edits(root, &index)?)
        .map(|p| (p, Dirt::Changed))
        .collect();
    for entry in index.iter().filter(|e| e.mode == GITLINK_MODE) {
        let link = &entry.path;
        let full = root.join(link);
        match std::fs::symlink_metadata(&full) {
            Ok(m) if m.is_dir() && is_toplevel(&full) => {}
            Ok(m) if !m.is_dir() => {
                // A file or link where a submodule belongs is dirt as it is, never
                // followed: a link to an ancestor would recurse without end.
                dirty.entry(link.clone()).or_insert(Dirt::Changed);
                continue;
            }
            _ => {
                dirty.insert(link.clone(), Dirt::Unpopulated);
                continue;
            }
        }
        let beneath = dirty_against_head(&full)?;
        if !beneath.is_empty() {
            dirty.entry(link.clone()).or_insert(Dirt::Changed);
        }
        for (path, dirt) in beneath {
            dirty.insert(format!("{link}/{path}"), dirt);
        }
    }
    Some(dirty)
}

/// The strict judge, for a run that files proofs under `HEAD`'s tree: nothing
/// [`dirty_against_head`] lists, and nothing [`raw_differences`] lists. `None` when git
/// cannot answer.
#[must_use]
pub fn strictly_clean(root: &Path) -> Option<bool> {
    Some(dirty_against_head(root)?.is_empty() && raw_differences(root)?.is_empty())
}

/// Every path of `HEAD`'s tree whose working copy is not the blob it records, by
/// identity: a regular file hashed raw (`hash-object --no-filters`) with the kind and
/// exec bit `HEAD` records, a link by the hash of its target, any other mode, and every
/// assume-unchanged or skip-worktree entry (a proof of the whole tree reads every file);
/// beneath each submodule too. `git diff` misses some of these: it trusts the index's
/// stat cache, which `core.checkStat`, `core.trustctime` and `core.fsmonitor` loosen,
/// and compares bytes after the clean filters `core.autocrlf` and attributes apply,
/// where a compiler reads them raw. `None` when git cannot answer.
#[must_use]
pub fn raw_differences(root: &Path) -> Option<Vec<String>> {
    let listing = out(root, &["ls-tree", "-r", "-z", "--full-tree", "HEAD"])?;
    let mut differ: Vec<String> = index_entries(root)?
        .into_iter()
        .filter(|e| e.tag.is_ascii_lowercase() || e.tag == 'S')
        .map(|e| e.path)
        .collect();
    // (path, blob) of each regular file, hashed in one call below.
    let mut files: Vec<(String, String)> = Vec::new();
    for rec in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = rec.iter().position(|b| *b == b'\t')?;
        let meta = String::from_utf8_lossy(&rec[..tab]);
        let path = String::from_utf8_lossy(&rec[tab + 1..]).into_owned();
        let mut fields = meta.split(' ');
        let (mode, blob) = (fields.next()?, fields.nth(1)?);
        let full = root.join(&path);
        let md = std::fs::symlink_metadata(&full).ok();
        match (mode, md) {
            ("100644" | "100755", Some(m)) if m.is_file() && (mode == "100755") == is_exec(&m) => {
                files.push((path, blob.to_string()));
            }
            ("120000", Some(m)) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&full).ok()?;
                let id = hash(
                    root,
                    &["hash-object", "--stdin"],
                    target.as_os_str().as_encoded_bytes(),
                )?;
                if id != blob {
                    differ.push(path);
                }
            }
            (GITLINK_MODE, Some(m)) if m.is_dir() && is_toplevel(&full) => {
                for p in raw_differences(&full)? {
                    differ.push(format!("{path}/{p}"));
                }
            }
            _ => differ.push(path),
        }
    }
    // `--stdin-paths` reads one path a line and C-unquotes one that starts with a
    // quote: a path it would misread is hashed alone.
    let (odd, plain): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|(p, _)| p.contains('\n') || p.starts_with('"'));
    for (path, blob) in odd {
        if hash(root, &["hash-object", "--no-filters", "--", &path], b"")? != blob {
            differ.push(path);
        }
    }
    if !plain.is_empty() {
        let input: String = plain.iter().map(|(p, _)| format!("{p}\n")).collect();
        let ids = git(
            root,
            &["hash-object", "--no-filters", "--stdin-paths"],
            input.as_bytes(),
        )?;
        let ids = String::from_utf8_lossy(&ids);
        let mut got = ids.lines();
        for (path, blob) in plain {
            if got.next() != Some(blob.as_str()) {
                differ.push(path);
            }
        }
    }
    differ.sort();
    differ.dedup();
    Some(differ)
}

/// The index mode of a gitlink: a submodule's recorded commit.
const GITLINK_MODE: &str = "160000";

/// One index entry, as `git ls-files -v -s` prints it.
struct IndexEntry {
    /// The `-v` tag: lowercase for assume-unchanged, `S` for skip-worktree.
    tag: char,
    mode: String,
    blob: String,
    path: String,
}

/// Git's variables that point a call at another repository, index or work tree: a
/// caller started from a git hook carries them, and the judge reads the root it is given.
const GIT_REDIRECTS: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// `git -C <root> --no-optional-locks <args>` fed `input`, [`GIT_REDIRECTS`] removed: its
/// stdout on exit 0. Checking never rewrites the index under a caller who is using it.
fn git(root: &Path, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut cmd = command("git");
    cmd.arg("-C").arg(root).arg("--no-optional-locks");
    for key in GIT_REDIRECTS {
        cmd.env_remove(key);
    }
    crate::ledger::run_with_input(cmd, args, input)
        .ok()
        .filter(crate::ledger::RunOut::success)
        .map(|o| o.stdout)
}

fn out(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    git(root, args, b"")
}

fn out_z(root: &Path, args: &[&str]) -> Option<Vec<String>> {
    Some(
        out(root, args)?
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect(),
    )
}

/// Every index entry: `<tag> <mode> <blob> <stage>\t<path>`.
fn index_entries(root: &Path) -> Option<Vec<IndexEntry>> {
    let listing = out(root, &["ls-files", "-v", "-s", "-z"])?;
    listing
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|rec| {
            let tab = rec.iter().position(|b| *b == b'\t')?;
            let meta = String::from_utf8_lossy(&rec[..tab]).into_owned();
            let mut fields = meta.split(' ');
            Some(IndexEntry {
                tag: fields.next()?.chars().next()?,
                mode: fields.next()?.to_string(),
                blob: fields.next()?.to_string(),
                path: String::from_utf8_lossy(&rec[tab + 1..]).into_owned(),
            })
        })
        .collect()
}

/// The flagged (assume-unchanged or skip-worktree) entries whose working tree is not
/// their index entry: `git status` and `git diff` take the entry as the working tree, a
/// compiler does not.
fn flag_hidden_edits(root: &Path, index: &[IndexEntry]) -> Option<Vec<String>> {
    let flagged: Vec<&IndexEntry> = index
        .iter()
        .filter(|e| (e.tag.is_ascii_lowercase() || e.tag == 'S') && e.mode != GITLINK_MODE)
        .collect();
    let mut edited = Vec::new();
    for entry in flagged {
        let full = root.join(&entry.path);
        let same = match std::fs::symlink_metadata(&full) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&full).ok()?;
                entry.mode == "120000"
                    && hash(
                        root,
                        &["hash-object", "--stdin"],
                        target.as_os_str().as_encoded_bytes(),
                    )? == entry.blob
            }
            Ok(m) if m.is_file() => {
                entry.mode == if is_exec(&m) { "100755" } else { "100644" }
                    && hash(
                        root,
                        &["hash-object", "--no-filters", "--", &entry.path],
                        b"",
                    )? == entry.blob
            }
            _ => false,
        };
        if !same {
            edited.push(entry.path.clone());
        }
    }
    Some(edited)
}

/// A blob id git prints for `args` fed `input`.
fn hash(root: &Path, args: &[&str], input: &[u8]) -> Option<String> {
    let id = git(root, args, input)?;
    Some(String::from_utf8_lossy(&id).trim().to_string())
}

#[cfg(unix)]
fn is_exec(m: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_exec(_: &std::fs::Metadata) -> bool {
    false
}

/// Whether `dir` is the top of a git checkout of its own (a populated submodule), not a
/// directory inside the superproject.
fn is_toplevel(dir: &Path) -> bool {
    let Some(top) = out(dir, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let top = String::from_utf8_lossy(&top).trim().to_string();
    match (std::fs::canonicalize(top), std::fs::canonicalize(dir)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::testing;

    fn run(dir: &Path, args: &[&str]) {
        let out = testing::git()
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo(at: &Path) {
        testing::init(
            at,
            &[
                ("user.name", "Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        std::fs::write(at.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(at.join("a.rs"), "a").unwrap();
        std::fs::write(at.join("flagged.rs"), "f").unwrap();
        run(at, &["add", "."]);
        run(at, &["commit", "-q", "-m", "base"]);
    }

    fn keys(root: &Path) -> Vec<String> {
        dirty_against_head(root).unwrap().into_keys().collect()
    }

    /// Each kind of dirt the judge counts is listed; a clean checkout lists nothing, and
    /// what only the tree's own `.gitignore` files ignore stays out.
    #[test]
    fn every_kind_of_dirt_is_listed_and_a_clean_tree_lists_none() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("w");
        repo(&root);
        assert!(keys(&root).is_empty(), "{:?}", keys(&root));

        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join("ignored/x"), "x").unwrap();
        std::fs::write(root.join(".git/info/exclude"), "local.rs\n").unwrap();
        std::fs::write(root.join("local.rs"), "only excluded by this checkout").unwrap();
        std::fs::write(root.join("a.rs"), "edited").unwrap();
        run(&root, &["update-index", "--skip-worktree", "flagged.rs"]);
        std::fs::write(root.join("flagged.rs"), "hidden edit").unwrap();
        assert_eq!(keys(&root), ["a.rs", "flagged.rs", "local.rs"]);
    }

    /// The strict judge reads the bytes: a CRLF copy of an LF file under
    /// `core.autocrlf=input`, and a same-size edit that kept its mtime under
    /// `core.checkStat=minimal`, read clean to `git diff` and differ raw; a flag, a lost
    /// exec bit and a retargeted link differ too.
    #[test]
    fn the_strict_judge_reads_the_bytes_git_diff_trusts() {
        use std::time::{Duration, SystemTime};
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().join("w");
        repo(&root);
        let old = SystemTime::now() - Duration::from_secs(100);
        let set_old = |p: &Path| {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        std::fs::write(root.join("lf.txt"), "a\nb\n").unwrap();
        std::fs::write(root.join("same.rs"), "aaaa").unwrap();
        set_old(&root.join("same.rs"));
        std::fs::write(root.join("run.sh"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            std::os::unix::fs::symlink("a.rs", root.join("link")).unwrap();
        }
        run(&root, &["add", "."]);
        run(&root, &["commit", "-q", "-m", "more"]);
        assert_eq!(strictly_clean(&root), Some(true));

        // autocrlf: the CRLF copy reads unchanged to the diff.
        run(&root, &["config", "core.autocrlf", "input"]);
        std::fs::write(root.join("lf.txt"), "a\r\nb\r\n").unwrap();
        assert!(keys(&root).is_empty(), "the case: {:?}", keys(&root));
        assert_eq!(raw_differences(&root).unwrap(), ["lf.txt"]);
        assert_eq!(strictly_clean(&root), Some(false));
        std::fs::write(root.join("lf.txt"), "a\nb\n").unwrap();
        assert_eq!(strictly_clean(&root), Some(true));

        // checkStat=minimal: a same-size edit that kept its mtime.
        run(&root, &["config", "core.checkStat", "minimal"]);
        run(&root, &["config", "core.trustctime", "false"]);
        std::fs::write(root.join("same.rs"), "bbbb").unwrap();
        set_old(&root.join("same.rs"));
        assert!(keys(&root).is_empty(), "the case: {:?}", keys(&root));
        assert_eq!(raw_differences(&root).unwrap(), ["same.rs"]);
        std::fs::write(root.join("same.rs"), "aaaa").unwrap();
        assert_eq!(strictly_clean(&root), Some(true));

        // any flag, even over HEAD's own bytes
        run(&root, &["update-index", "--assume-unchanged", "a.rs"]);
        assert_eq!(raw_differences(&root).unwrap(), ["a.rs"]);
        run(&root, &["update-index", "--no-assume-unchanged", "a.rs"]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            run(&root, &["config", "core.fileMode", "false"]);
            std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o644))
                .unwrap();
            assert_eq!(raw_differences(&root).unwrap(), ["run.sh"]);
            std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            std::fs::remove_file(root.join("link")).unwrap();
            std::os::unix::fs::symlink("flagged.rs", root.join("link")).unwrap();
            assert_eq!(raw_differences(&root).unwrap(), ["link"]);
        }
    }

    /// A submodule the index records but nobody checked out is Unpopulated; one checked
    /// out with an edit inside lists the edit beneath its gitlink, and the gitlink; a file
    /// or a link (even to an ancestor) where it belongs is Changed and never followed.
    #[test]
    fn a_submodule_is_judged_by_its_own_checkout() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        repo(&sub);
        let root = dir.path().join("w");
        repo(&root);
        run(
            &root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                sub.to_str().unwrap(),
                "vendor/sub",
            ],
        );
        run(&root, &["commit", "-q", "-m", "sub"]);
        assert!(keys(&root).is_empty(), "{:?}", keys(&root));
        assert_eq!(strictly_clean(&root), Some(true));

        std::fs::write(root.join("vendor/sub/a.rs"), "edited beneath").unwrap();
        assert_eq!(keys(&root), ["vendor/sub", "vendor/sub/a.rs"]);
        assert_eq!(raw_differences(&root).unwrap(), ["vendor/sub/a.rs"]);

        run(&root, &["submodule", "deinit", "-q", "-f", "vendor/sub"]);
        let found = dirty_against_head(&root).unwrap();
        assert_eq!(
            found.get("vendor/sub"),
            Some(&Dirt::Unpopulated),
            "{found:?}"
        );

        let link = root.join("vendor/sub");
        std::fs::remove_dir(&link).unwrap();
        std::fs::write(&link, "a file").unwrap();
        let found = dirty_against_head(&root).unwrap();
        assert_eq!(found.get("vendor/sub"), Some(&Dirt::Changed), "{found:?}");

        #[cfg(unix)]
        {
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink("../..", &link).unwrap();
            let found = dirty_against_head(&root).unwrap();
            assert_eq!(found.get("vendor/sub"), Some(&Dirt::Changed), "{found:?}");
            assert!(
                found.keys().all(|p| !p.starts_with("vendor/sub/")),
                "{found:?}"
            );
        }
    }
}
