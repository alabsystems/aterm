// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clean-tree verification for release cuts.

use aterm_uds::spawnfd::command;
use std::collections::{BTreeMap, BTreeSet};
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
    dirty_against_head_with_index(root, &index)
}

fn dirty_against_head_with_index(
    root: &Path,
    index: &[IndexEntry],
) -> Option<BTreeMap<String, Dirt>> {
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
        .chain(flag_hidden_edits(root, index)?)
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

/// Every file the checkout at `root` tracks, its initialised submodules' own included, by its
/// path from `root`: what is neither untracked nor ignored there.
#[must_use]
pub fn tracked_files(root: &Path) -> Option<BTreeSet<String>> {
    let mut tracked = BTreeSet::new();
    for entry in index_entries(root)? {
        if entry.mode != GITLINK_MODE {
            tracked.insert(entry.path);
            continue;
        }
        let full = root.join(&entry.path);
        // Reject a symlink gitlink before asking Git to inspect it, as the dirt checks do.
        if std::fs::symlink_metadata(&full).is_ok_and(|meta| meta.is_dir()) && is_toplevel(&full) {
            for path in tracked_files(&full)? {
                tracked.insert(format!("{}/{path}", entry.path));
            }
        }
    }
    Some(tracked)
}

/// The strict judge, for a run that files proofs under `HEAD`'s tree.
#[must_use]
pub fn strictly_clean(root: &Path) -> Option<bool> {
    let index = index_entries(root)?;
    Some(
        dirty_against_head_with_index(root, &index)?.is_empty()
            && raw_differences_with_index(root, &index)?.is_empty(),
    )
}

/// Every path of `HEAD`'s tree whose working copy is not the blob it records, by identity.
#[must_use]
pub fn raw_differences(root: &Path) -> Option<Vec<String>> {
    let index = index_entries(root)?;
    raw_differences_with_index(root, &index)
}

fn raw_differences_with_index(root: &Path, index: &[IndexEntry]) -> Option<Vec<String>> {
    let listing = out(root, &["ls-tree", "-r", "-z", "--full-tree", "HEAD"])?;
    let mut differ: Vec<String> = index
        .iter()
        .filter(|e| e.tag.is_ascii_lowercase() || e.tag == 'S')
        .map(|e| e.path.clone())
        .collect();
    let mut files: Vec<(std::borrow::Cow<'_, str>, &str)> = Vec::new();
    for rec in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = rec.iter().position(|b| *b == b'\t')?;
        let meta = std::str::from_utf8(&rec[..tab]).ok()?;
        let path = String::from_utf8_lossy(&rec[tab + 1..]);
        let mut fields = meta.split(' ');
        let (mode, blob) = (fields.next()?, fields.nth(1)?);
        let full = root.join(path.as_ref());
        let md = std::fs::symlink_metadata(&full).ok();
        match (mode, md) {
            ("100644" | "100755", Some(m)) if m.is_file() && (mode == "100755") == is_exec(&m) => {
                files.push((path, blob));
            }
            ("120000", Some(m)) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&full).ok()?;
                let id = hash(
                    root,
                    &["hash-object", "--stdin"],
                    target.as_os_str().as_encoded_bytes(),
                )?;
                if id != blob {
                    differ.push(path.into_owned());
                }
            }
            (GITLINK_MODE, Some(m)) if m.is_dir() && is_toplevel(&full) => {
                if hash(&full, &["rev-parse", "HEAD"], b"")?.as_str() != blob {
                    differ.push(path.as_ref().to_owned());
                }
                for p in raw_differences(&full)? {
                    differ.push(format!("{path}/{p}"));
                }
            }
            _ => differ.push(path.into_owned()),
        }
    }
    let (odd, plain): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|(p, _)| p.contains('\n') || p.starts_with('"'));
    for (path, blob) in odd {
        if hash(
            root,
            &["hash-object", "--no-filters", "--", path.as_ref()],
            b"",
        )? != blob
        {
            differ.push(path.into_owned());
        }
    }
    if !plain.is_empty() {
        let mut input =
            String::with_capacity(plain.iter().map(|(p, _)| p.len() + 1).sum::<usize>());
        for (p, _) in &plain {
            input.push_str(p);
            input.push('\n');
        }
        let ids = git(
            root,
            &["hash-object", "--no-filters", "--stdin-paths"],
            input.as_bytes(),
        )?;
        let ids = std::str::from_utf8(&ids).ok()?;
        let mut got = ids.lines();
        for (path, blob) in plain {
            if got.next() != Some(blob) {
                differ.push(path.into_owned());
            }
        }
    }
    differ.sort_unstable();
    differ.dedup();
    Some(differ)
}

const GITLINK_MODE: &str = "160000";

struct IndexEntry {
    tag: char,
    mode: std::borrow::Cow<'static, str>,
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
            let meta = std::str::from_utf8(&rec[..tab]).ok()?;
            let mut fields = meta.split(' ');
            let tag = fields.next()?.chars().next()?;
            let mode = match fields.next()? {
                "100644" => std::borrow::Cow::Borrowed("100644"),
                "100755" => std::borrow::Cow::Borrowed("100755"),
                "120000" => std::borrow::Cow::Borrowed("120000"),
                GITLINK_MODE => std::borrow::Cow::Borrowed(GITLINK_MODE),
                other => std::borrow::Cow::Owned(other.to_string()),
            };
            let blob = fields.next()?;
            let blob = if tag.is_ascii_lowercase() || tag == 'S' {
                blob.to_string()
            } else {
                String::new()
            };
            Some(IndexEntry {
                tag,
                mode,
                blob,
                path: String::from_utf8_lossy(&rec[tab + 1..]).into_owned(),
            })
        })
        .collect()
}

fn flag_hidden_edits(root: &Path, index: &[IndexEntry]) -> Option<Vec<String>> {
    let mut edited = Vec::new();
    for entry in index
        .iter()
        .filter(|e| e.tag.is_ascii_lowercase() || e.tag == 'S')
    {
        let full = root.join(&entry.path);
        let same = match std::fs::symlink_metadata(&full) {
            Ok(m) if entry.mode == GITLINK_MODE => {
                m.is_dir()
                    && is_toplevel(&full)
                    && hash(&full, &["rev-parse", "HEAD"], b"")? == entry.blob
            }
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
    let top = String::from_utf8_lossy(&top);
    match (
        std::fs::canonicalize(top.trim()),
        std::fs::canonicalize(dir),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
#[path = "clean_tests.rs"]
mod tests;
