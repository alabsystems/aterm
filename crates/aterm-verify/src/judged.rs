// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The main lane's append-only, compare-and-swap Git journal. No verdict is a gate.

use aterm_uds::spawnfd::Command;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const REF: &str = "refs/aterm/judged";
pub const RETENTION: u64 = 14 * 86400;
pub const LEASE: u64 = 120;

#[cfg(unix)]
thread_local! {
    static INTERRUPTIBLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A lease renewal may use its whole remaining ownership budget because its child I/O can
/// stop on the controller's interrupt. Leave the interrupt for that controller to report;
/// terminal publication outside this scope must still run while the interrupt is pending.
#[cfg(unix)]
pub fn interruptible<T>(work: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            INTERRUPTIBLE.set(self.0);
        }
    }
    let _restore = Restore(INTERRUPTIBLE.replace(true));
    work()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub outcome: String,
    pub log: String,
    pub red_items: Vec<String>,
    pub suspects: Vec<String>,
    pub fixed_by: Option<String>,
    pub decisions: Vec<Decision>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Decision {
    pub id: String,
    pub outcome: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Record {
    pub commit: String,
    pub tree: String,
    pub host: String,
    pub verifier: String,
    /// Controller image at claim time, independent of the sampled verifier.
    #[serde(default)]
    pub controller: String,
    pub inputs: String,
    pub token: String,
    pub state: String,
    /// Terminal interruption or infrastructure evidence; absent only in older journal entries.
    #[serde(default)]
    pub cause: String,
    pub started: u64,
    pub updated: u64,
    pub lease_until: u64,
    pub rows: Vec<Row>,
}

impl Record {
    /// An expired lease is reclaimable; cancellation and infrastructure failures are retryable.
    #[must_use]
    pub fn occupied(&self, now: u64) -> bool {
        matches!(self.state.as_str(), "pass" | "red")
            || (self.state == "judging" && self.lease_until > now)
    }
}

pub fn command(root: &Path, args: &[&str]) -> Command {
    let mut cmd = crate::git_provider::command();
    cmd.current_dir(root).args(args).env_remove("GITHUB_TOKEN");
    cmd
}

pub fn deadline() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(60)
}

fn capture_until(
    cmd: &mut Command,
    input: Option<&[u8]>,
    until: std::time::Instant,
) -> Result<std::process::Output, String> {
    if std::time::Instant::now() >= until {
        return Err("journal operation exhausted its deadline".into());
    }
    #[cfg(unix)]
    if INTERRUPTIBLE.get() {
        let watch = aterm_toolchain::interrupt::Watch::enter();
        return aterm_uds::deadline::output_with_input_until_watching(
            cmd,
            aterm_uds::secret::Secret::new(input.unwrap_or_default()),
            until,
            512 * 1024 * 1024,
            || watch.caught().is_some(),
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "journal renewal interrupted by controller signal".into());
    }
    aterm_uds::deadline::output_with_input_until(
        cmd,
        aterm_uds::secret::Secret::new(input.unwrap_or_default()),
        until,
        512 * 1024 * 1024,
    )
    .map_err(|e| e.to_string())
}

pub fn output(cmd: &mut Command, input: Option<&[u8]>) -> Result<String, String> {
    output_until(cmd, input, deadline())
}

fn output_until(
    cmd: &mut Command,
    input: Option<&[u8]>,
    until: std::time::Instant,
) -> Result<String, String> {
    let out = capture_until(cmd, input, until)?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

pub fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    output(&mut command(root, args), None).map(|s| s.trim().to_owned())
}

fn git_until(root: &Path, args: &[&str], until: std::time::Instant) -> Result<String, String> {
    output_until(&mut command(root, args), None, until).map(|s| s.trim().to_owned())
}

/// Refresh only this private journal, never the checkout's branch or index.
pub fn head(
    root: &Path,
    remote: &str,
    until: std::time::Instant,
) -> Result<Option<String>, String> {
    let git = |root: &Path| command(root, &[]);
    let run = |tool, args: &[std::ffi::OsString]| {
        capture_until(
            &mut crate::remote::command(root, &git, tool, args)?,
            None,
            until,
        )
        .map_err(|why| format!("{tool:?} journal transport: {why}"))
    };
    crate::remote::Remote::open(root, remote, &run)?.fetch(REF)
}

pub fn records(
    root: &Path,
    head: Option<&str>,
    until: std::time::Instant,
) -> Result<Vec<Record>, String> {
    let git = |root: &Path, args: &[&str]| git_until(root, args, until);
    let output = |cmd: &mut Command, input: Option<&[u8]>| output_until(cmd, input, until);
    let Some(head) = head else {
        return Ok(Vec::new());
    };
    let names = git(
        root,
        &["ls-tree", "-r", "--format=%(objectname)", head, "verdicts/"],
    )?;
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let objects = output(
        &mut command(root, &["cat-file", "--batch"]),
        Some(format!("{names}\n").as_bytes()),
    )?;
    decode_objects(objects.as_bytes())
}

pub fn record(
    root: &Path,
    head: Option<&str>,
    tree: &str,
    until: std::time::Instant,
) -> Result<Option<Record>, String> {
    let git = |root: &Path, args: &[&str]| git_until(root, args, until);
    let Some(head) = head else {
        return Ok(None);
    };
    let path = format!("verdicts/{tree}.toml");
    if git(root, &["ls-tree", head, &path])?.is_empty() {
        return Ok(None);
    }
    let text = git(root, &["show", &format!("{head}:{path}")])?;
    let record: Record = aterm_toml::from_str(&text).map_err(|e| e.to_string())?;
    if record.tree != tree {
        return Err("journal record is filed under another tree".into());
    }
    Ok(Some(record))
}

/// Attempts updated since `since`, including attempts overwritten by a later tree claim.
pub fn history(root: &Path, head: &str, since: u64) -> Result<Vec<Record>, String> {
    let until = deadline();
    // Git commit dates are not record times. Read the immutable chain, including pruned
    // paths, then apply the window to the lane's timestamps. Newest occurrence wins even
    // when a host's wall clock went backwards. A heartbeat keeps the same attempt token.
    let changes = git_until(
        root,
        &[
            "log",
            "--format=",
            "--raw",
            "--no-abbrev",
            head,
            "--",
            "verdicts/",
        ],
        until,
    )?;
    let mut seen = std::collections::BTreeSet::new();
    let mut objects = Vec::new();
    for line in changes.lines().filter(|line| line.starts_with(':')) {
        let fields: Vec<_> = line.split_whitespace().collect();
        let Some(object) = fields.get(3) else {
            return Err("invalid journal change header".into());
        };
        if object.bytes().all(|b| b == b'0') || !seen.insert(*object) {
            continue;
        }
        objects.push(*object);
    }
    let mut attempts = std::collections::BTreeMap::new();
    // Bound each batch's decoded allocation; output_until bounds the shared wall clock
    // and byte budget. No per-blob child process, even for thousands of renewals.
    for batch in objects.chunks(64) {
        let input = format!("{}\n", batch.join("\n"));
        let bytes = output_until(
            &mut command(root, &["cat-file", "--batch"]),
            Some(input.as_bytes()),
            until,
        )?;
        for record in decode_objects(bytes.as_bytes())? {
            attempts
                .entry((record.tree.clone(), record.token.clone()))
                .or_insert(record);
        }
    }
    Ok(attempts
        .into_values()
        .filter(|record| record.updated >= since)
        .collect())
}

fn decode_objects(mut bytes: &[u8]) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    while !bytes.is_empty() {
        let end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("truncated object header")?;
        let header = std::str::from_utf8(&bytes[..end]).map_err(|e| e.to_string())?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.len() != 3 || fields[1] != "blob" {
            return Err(format!("invalid journal object: {header}"));
        }
        let length = fields[2].parse::<usize>().map_err(|e| e.to_string())?;
        bytes = &bytes[end + 1..];
        let text = std::str::from_utf8(bytes.get(..length).ok_or("truncated journal blob")?)
            .map_err(|e| e.to_string())?;
        records.push(aterm_toml::from_str(text).map_err(|e| e.to_string())?);
        bytes = bytes
            .get(length..)
            .and_then(|b| b.strip_prefix(b"\n"))
            .ok_or("missing object terminator")?;
    }
    Ok(records)
}

/// One change to the journal's tree, by a `/`-separated path from its root.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Edit {
    /// The blob `blob` at `path`, added or in place of the file there.
    Put { path: String, blob: String },
    /// Nothing at `path`: the file or directory there removed, and nothing done where there is
    /// none, as through a component no tree entry has (empty, `.` or `..`: [`journal_path`]),
    /// below a file, or below a path an earlier edit puts or removes.
    Remove { path: String },
}

/// One entry of a journal tree, as `git ls-tree -z` lists it and `git mktree -z` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    mode: String,
    kind: String,
    id: String,
}

impl Entry {
    fn blob(id: &str) -> Self {
        Self {
            mode: "100644".into(),
            kind: "blob".into(),
            id: id.into(),
        }
    }

    fn tree(id: String) -> Self {
        Self {
            mode: "040000".into(),
            kind: "tree".into(),
            id,
        }
    }

    fn is_tree(&self) -> bool {
        self.kind == "tree"
    }
}

/// A directory's entries, by name.
type Entries = std::collections::BTreeMap<String, Entry>;

/// How a tree edit reads and writes tree objects: `list` the entries of a tree (or of a commit's
/// tree), `make` a tree of entries and say its id.
struct Trees<'a> {
    list: &'a mut dyn FnMut(&str) -> Result<Entries, String>,
    make: &'a mut dyn FnMut(&Entries) -> Result<String, String>,
}

/// The edits below one directory, by the name of each entry they change.
#[derive(Default)]
struct Node(std::collections::BTreeMap<String, Change>);

enum Change {
    Put(String),
    Remove,
    Dir(Node),
}

/// Whether `path` can name a journal entry: no component of it is empty, `.` or `..`, or holds a
/// NUL. A put must name one; a removal through a component no entry has finds nothing there.
fn journal_path(path: &str) -> bool {
    path.split('/')
        .all(|name| !name.is_empty() && name != "." && name != ".." && !name.contains('\0'))
}

impl Node {
    /// Add `change` at `path` below this directory. A later edit of a path replaces an earlier
    /// edit of it or below it. Below a path an earlier edit puts or removes, a removal does
    /// nothing, as nothing is there, and a put is refused.
    fn add(&mut self, path: &str, change: Change) -> Result<(), String> {
        let (name, rest) = path
            .split_once('/')
            .map_or((path, None), |(n, r)| (n, Some(r)));
        match rest {
            None => {
                self.0.insert(name.to_owned(), change);
                Ok(())
            }
            Some(rest) => match self
                .0
                .entry(name.to_owned())
                .or_insert_with(|| Change::Dir(Node::default()))
            {
                Change::Dir(node) => node.add(rest, change),
                Change::Put(_) | Change::Remove if matches!(change, Change::Remove) => Ok(()),
                Change::Put(_) | Change::Remove => Err(format!(
                    "journal edits name {name:?} both as an entry and as a directory"
                )),
            },
        }
    }

    /// Whether any edit at or below this directory puts a blob.
    fn puts(&self) -> bool {
        self.0.values().any(|change| match change {
            Change::Put(_) => true,
            Change::Remove => false,
            Change::Dir(below) => below.puts(),
        })
    }
}

/// `base`'s entries (none for no base) with `node`'s edits applied, and whether any entry
/// differs from `base`'s. Only the directories on an edited path are listed; a directory below
/// is made again only when its entries changed, and dropped when none are left, as an index
/// keeps no empty directory. Removals below a file remove nothing. Every other entry keeps its
/// object id.
fn edited(
    base: Option<&str>,
    node: &Node,
    trees: &mut Trees<'_>,
) -> Result<(Entries, bool), String> {
    let mut entries = match base {
        Some(id) => (trees.list)(id)?,
        None => Entries::new(),
    };
    let mut changed = false;
    for (name, change) in &node.0 {
        let now = match change {
            Change::Put(blob) => {
                if entries.get(name).is_some_and(Entry::is_tree) {
                    return Err(format!("journal path {name:?} is a directory"));
                }
                Some(Entry::blob(blob))
            }
            Change::Remove => None,
            Change::Dir(below) => {
                let sub = match entries.get(name) {
                    None => None,
                    Some(entry) if entry.is_tree() => Some(entry.id.clone()),
                    Some(_) if !below.puts() => continue,
                    Some(_) => return Err(format!("journal path {name:?} is a file")),
                };
                let (dir, dir_changed) = edited(sub.as_deref(), below, trees)?;
                if !dir_changed {
                    continue;
                }
                if dir.is_empty() {
                    None
                } else {
                    Some(Entry::tree((trees.make)(&dir)?))
                }
            }
        };
        let was = match now.clone() {
            Some(entry) => entries.insert(name.clone(), entry),
            None => entries.remove(name),
        };
        changed |= was != now;
    }
    Ok((entries, changed))
}

/// The tree of `base` (a commit or a tree; `None` for an empty journal) with `edits` applied,
/// in order, through `trees`: a path edit that lists and makes only the root and the directories
/// on the edited paths (`git mktree` over each), reusing every other subtree by its id, so its
/// cost follows the edited paths and not the journal's size.
/// A removal where there is nothing to remove ([`Edit::Remove`]) does nothing.
/// # Errors
/// A put's path has an empty, `.` or `..` component or one holding a NUL; a put goes below a
/// path an earlier edit puts or removes, or below a file; a file is put where a directory is; or
/// `trees` fails.
fn edit_tree_with(
    base: Option<&str>,
    edits: &[Edit],
    trees: &mut Trees<'_>,
) -> Result<String, String> {
    let mut node = Node::default();
    for edit in edits {
        match edit {
            Edit::Put { path, .. } if !journal_path(path) => {
                return Err(format!("invalid journal path: {path:?}"));
            }
            Edit::Put { path, blob } => node.add(path, Change::Put(blob.clone()))?,
            Edit::Remove { path } => node.add(path, Change::Remove)?,
        }
    }
    let (entries, _) = edited(base, &node, trees)?;
    (trees.make)(&entries)
}

/// The entries of the tree `id` names (a commit names its own tree).
fn ls_tree(root: &Path, id: &str, until: std::time::Instant) -> Result<Entries, String> {
    let listing = output_until(&mut command(root, &["ls-tree", "-z", id]), None, until)?;
    let mut entries = Entries::new();
    for record in listing.split('\0').filter(|record| !record.is_empty()) {
        let invalid = || format!("invalid journal tree entry: {record:?}");
        let (head, name) = record.split_once('\t').ok_or_else(invalid)?;
        let mut fields = head.split(' ');
        let (Some(mode), Some(kind), Some(object), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(invalid());
        };
        entries.insert(
            name.to_owned(),
            Entry {
                mode: mode.to_owned(),
                kind: kind.to_owned(),
                id: object.to_owned(),
            },
        );
    }
    Ok(entries)
}

/// Write the tree of `entries`: its id.
fn mktree(root: &Path, entries: &Entries, until: std::time::Instant) -> Result<String, String> {
    let input: String = entries
        .iter()
        .map(|(name, entry)| format!("{} {} {}\t{name}\0", entry.mode, entry.kind, entry.id))
        .collect();
    output_until(
        &mut command(root, &["mktree", "-z"]),
        Some(input.as_bytes()),
        until,
    )
    .map(|id| id.trim().to_owned())
}

/// A normal fast-forward push is the CAS: a competing append rejects this parent.
/// The caller rereads and redecides; it never force-pushes a stale decision. The new tree is
/// `parent`'s with only the paths the append changes edited (`edit_tree_with`): the root and
/// `verdicts/`, whatever else the journal holds.
pub fn append(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    record: &Record,
    until: std::time::Instant,
) -> Result<bool, String> {
    append_with(
        root,
        remote,
        parent,
        record,
        until,
        &mut Trees {
            list: &mut |id| ls_tree(root, id, until),
            make: &mut |entries| mktree(root, entries, until),
        },
    )
}

/// [`append`], its tree objects read and written through `trees`.
fn append_with(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    record: &Record,
    until: std::time::Instant,
    trees: &mut Trees<'_>,
) -> Result<bool, String> {
    // One absolute deadline bounds the whole append, including CAS races and pruning.
    let git = |root: &Path, args: &[&str]| git_until(root, args, until);
    let output = |cmd: &mut Command, input: Option<&[u8]>| output_until(cmd, input, until);
    let capture = |cmd: &mut Command, input: Option<&[u8]>| capture_until(cmd, input, until);
    let last_pruned = parent
        .and_then(|p| git(root, &["show", &format!("{p}:pruned-at")]).ok())
        .and_then(|s| s.parse::<u64>().ok());
    let prune = last_pruned.is_none_or(|day| day < record.updated / 86400);
    let mut edits = Vec::new();
    if prune {
        for old in records(root, parent, until)? {
            if old.updated.saturating_add(RETENTION) < record.updated {
                edits.push(Edit::Remove {
                    path: format!("verdicts/{}.toml", old.tree),
                });
            }
        }
        let blob = output(
            &mut command(root, &["hash-object", "-w", "--stdin"]),
            Some((record.updated / 86400).to_string().as_bytes()),
        )?;
        edits.push(Edit::Put {
            path: "pruned-at".into(),
            blob: blob.trim().to_owned(),
        });
    }
    let text = aterm_toml::to_string(record).map_err(|e| e.to_string())?;
    let blob = output(
        &mut command(root, &["hash-object", "-w", "--stdin"]),
        Some(text.as_bytes()),
    )?;
    edits.push(Edit::Put {
        path: format!("verdicts/{}.toml", record.tree),
        blob: blob.trim().to_owned(),
    });
    let tree = edit_tree_with(parent, &edits, trees)?;
    let mut commit = command(
        root,
        &[
            "commit-tree",
            &tree,
            "-m",
            &format!("verify watch: {} {}", record.state, record.commit),
        ],
    );
    if let Some(parent) = parent {
        commit.args(["-p", parent]);
    }
    let sha = output(&mut commit, None)?;
    let pushed = capture(
        &mut command(
            root,
            &[
                "push",
                "--porcelain",
                remote,
                &format!("{}:{REF}", sha.trim()),
            ],
        ),
        None,
    )?;
    if pushed.status.success() {
        return Ok(true);
    }
    // A race is retryable only when the remote actually moved. Authentication/network failures
    // remain errors, rather than turning into a tight retry loop.
    if head(root, remote, until)?.as_deref() != parent {
        return Ok(false);
    }
    Err(String::from_utf8_lossy(&pushed.stderr).into_owned())
}

#[cfg(test)]
#[path = "judged_tests.rs"]
mod tests;
