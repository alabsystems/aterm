// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clean-tree verification for release cuts.

use aterm_uds::spawnfd::command;
use std::collections::BTreeMap;
use std::path::Path;

/// How a path differs from `HEAD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dirt {
    /// Its working tree is not what `HEAD` records.
    Changed,
    /// A submodule `HEAD` records whose checkout is not there (never initialised, or its `.git`
    /// broken): no build of this tree can read that source.
    Unpopulated,
}

/// Every path of the checkout at `root` that differs from its `HEAD`, submodules' own included.
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

/// The strict judge, for a run that files proofs under `HEAD`'s tree.
#[must_use]
pub fn strictly_clean(root: &Path) -> Option<bool> {
    Some(dirty_against_head(root)?.is_empty() && raw_differences(root)?.is_empty())
}

/// Every path of `HEAD`'s tree whose working copy is not the blob it records, by identity.
#[must_use]
pub fn raw_differences(root: &Path) -> Option<Vec<String>> {
    let listing = out(root, &["ls-tree", "-r", "-z", "--full-tree", "HEAD"])?;
    let mut differ: Vec<String> = index_entries(root)?
        .into_iter()
        .filter(|e| e.tag.is_ascii_lowercase() || e.tag == 'S')
        .map(|e| e.path)
        .collect();
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

const GITLINK_MODE: &str = "160000";

struct IndexEntry {
    tag: char,
    mode: String,
    blob: String,
    path: String,
}

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

fn git(root: &Path, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut cmd = command(aterm_uds::os_path!("git"));
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

        run(&root, &["config", "core.autocrlf", "input"]);
        std::fs::write(root.join("lf.txt"), "a\r\nb\r\n").unwrap();
        assert!(keys(&root).is_empty(), "the case: {:?}", keys(&root));
        assert_eq!(raw_differences(&root).unwrap(), ["lf.txt"]);
        assert_eq!(strictly_clean(&root), Some(false));
        std::fs::write(root.join("lf.txt"), "a\nb\n").unwrap();
        assert_eq!(strictly_clean(&root), Some(true));

        run(&root, &["config", "core.checkStat", "minimal"]);
        run(&root, &["config", "core.trustctime", "false"]);
        std::fs::write(root.join("same.rs"), "bbbb").unwrap();
        set_old(&root.join("same.rs"));
        assert!(keys(&root).is_empty(), "the case: {:?}", keys(&root));
        assert_eq!(raw_differences(&root).unwrap(), ["same.rs"]);
        std::fs::write(root.join("same.rs"), "aaaa").unwrap();
        assert_eq!(strictly_clean(&root), Some(true));

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
