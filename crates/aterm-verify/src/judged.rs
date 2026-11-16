// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The append-only, compare-and-swap Git journal the main lane's controllers filed their verdicts
//! in (`verdicts/`; `verify watch` is now the follower, which posts a commit status and writes
//! here no more, though a controller installed before it may), which also files `verify land`'s
//! records under [`LAND`]: its per-check verdict records ([`crate::land_record`]) and, under
//! [`RUNS`], its run records ([`crate::land_run`]), one store with a retention per kind. No
//! verdict is a gate: nothing reads a land record to decide a landing yet, and nothing refuses a
//! push made without `verify land`.

use aterm_uds::spawnfd::Command;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const REF: &str = "refs/aterm/judged";
pub const RETENTION: u64 = 14 * 86400;
/// Seconds a claim holds its tree after the renewal that last reached the journal. A controller
/// renews at each stage end and at least every [`RENEW_EVERY`] seconds, so one renewal that fails
/// does not lapse the lease. A lapsed lease ends no judgment: it only lets another host claim
/// the tree. The lapsed attempt's last record is then filed beside the slot while that host's
/// live claim or its pass or red holds it, and otherwise at the slot, in place of what that
/// host left there ([`Place`]).
pub const LEASE: u64 = 20 * 60;
/// The longest a controller leaves its claim unrenewed, in seconds.
pub const RENEW_EVERY: u64 = 10 * 60;
// One renewal may fail, and its retries with it, before the lease lapses.
const _: () = assert!(LEASE >= 2 * RENEW_EVERY);

#[cfg(unix)]
thread_local! {
    static INTERRUPTIBLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A lease renewal runs while its sample does, so its child I/O stops on the controller's
/// interrupt. Leave the interrupt for that controller to report;
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

/// Where a record is filed under `verdicts/`. A reader of a tree's verdict asks its slot alone;
/// [`records`] and [`history`] read both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// `verdicts/<tree>.toml`: the tree's claim, and then the record that attempt ended with.
    Slot,
    /// `verdicts/<tree>/<token>.toml`: the last record of an attempt whose lease lapsed, where
    /// another attempt's live claim or verdict holds the slot. Filing it there replaces neither.
    Beside,
}

impl Place {
    fn path(self, record: &Record) -> String {
        match self {
            Self::Slot => format!("verdicts/{}.toml", record.tree),
            Self::Beside => format!("verdicts/{}/{}.toml", record.tree, record.token),
        }
    }
}

pub fn command(root: &Path, args: &[&str]) -> Command {
    let mut cmd = crate::git_provider::command();
    // Journal identities name stored objects and exact paths, never local replacements
    // or caller-selected glob patterns.
    cmd.current_dir(root)
        .arg("--literal-pathspecs")
        .arg("--no-replace-objects")
        .args(args)
        .env_remove("GITHUB_TOKEN");
    // Match remote reads: inherited Git state must not redirect this checkout's
    // journal reads or object writes into another repository or object store.
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(key);
        }
    }
    // Missing objects are an incomplete local snapshot, not permission for an implicit
    // promisor fetch. Remote imports have their own explicit transport and work budget.
    cmd.env("GIT_NO_LAZY_FETCH", "1");
    cmd
}

pub fn deadline() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(60)
}

/// Initial reads and assembly must admit publication before `read_until`. The first admitted
/// writer starts one shared publication window, including race readback and later attempts.
/// No race renews that window; each writer retains its owner through settlement.
struct AppendBudget {
    read_until: std::time::Instant,
    publication_until: std::cell::Cell<Option<std::time::Instant>>,
}

struct MutationBudget(std::time::Instant);

impl AppendBudget {
    fn new(read_until: std::time::Instant) -> Self {
        Self {
            read_until,
            publication_until: std::cell::Cell::new(None),
        }
    }

    fn deadline(&self) -> std::time::Instant {
        self.publication_until.get().unwrap_or(self.read_until)
    }

    fn admit(&self, now: std::time::Instant) -> Result<MutationBudget, String> {
        if now >= self.deadline() {
            return Err("journal mutation not started: append budget expired".into());
        }
        if INTERRUPTIBLE.get() && crate::exec::group::taken().is_some() {
            return Err("journal mutation not started: landing interrupted".into());
        }
        if let Some(until) = self.publication_until.get() {
            return Ok(MutationBudget(until));
        }
        let until = now
            .checked_add(agh::git::GRANT_MAX)
            .ok_or("journal mutation work deadline overflow")?;
        self.publication_until.set(Some(until));
        Ok(MutationBudget(until))
    }
}

fn capture_until(
    cmd: &mut Command,
    input: Option<&[u8]>,
    until: std::time::Instant,
) -> Result<std::process::Output, String> {
    let started = std::time::Instant::now();
    let remaining = until.saturating_duration_since(started);
    let failure = |error: std::io::Error| capture_failure(error, started, remaining);
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
        .map_err(failure)?
        .ok_or_else(|| "journal renewal interrupted by controller signal".into());
    }
    aterm_uds::deadline::output_with_input_until(
        cmd,
        aterm_uds::secret::Secret::new(input.unwrap_or_default()),
        until,
        512 * 1024 * 1024,
    )
    .map_err(failure)
}

fn capture_failure(
    error: std::io::Error,
    started: std::time::Instant,
    remaining: std::time::Duration,
) -> String {
    if error.kind() == std::io::ErrorKind::TimedOut {
        // This is the whole call, including retirement; the owned-input error retains the
        // completion facts observed at the deadline before cleanup can change them.
        format!(
            "{error}; return_elapsed_ms={} inherited_remaining_ms={}",
            started.elapsed().as_millis(),
            remaining.as_millis(),
        )
    } else {
        error.to_string()
    }
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
    head_in(root, remote, until, ReadContext::Read)
}

#[derive(Clone, Copy)]
enum ReadContext {
    Read,
    AppendHead,
    AfterPushFailure,
}

impl ReadContext {
    const fn word(self) -> &'static str {
        match self {
            Self::Read => "journal-read",
            Self::AppendHead => "journal-append-head",
            Self::AfterPushFailure => "journal-after-push-failure",
        }
    }
}

fn head_in(
    root: &Path,
    remote: &str,
    until: std::time::Instant,
    context: ReadContext,
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
    read_head(root, remote, context, &run)
}

fn read_head(
    root: &Path,
    remote: &str,
    context: ReadContext,
    run: &crate::remote::Runner<'_>,
) -> Result<Option<String>, String> {
    (|| crate::remote::Remote::open(root, remote, run)?.fetch(REF))()
        .map_err(|why| format!("{}: {why}", context.word()))
}

pub fn records(
    root: &Path,
    head: Option<&str>,
    until: std::time::Instant,
) -> Result<Vec<Record>, String> {
    Ok(filed(root, head, "verdicts/", until)?
        .into_iter()
        .map(|(_, record)| record)
        .collect())
}

/// The records filed beside `tree`'s slot at `head` ([`Place::Beside`]).
pub fn beside(
    root: &Path,
    head: Option<&str>,
    tree: &str,
    until: std::time::Instant,
) -> Result<Vec<Record>, String> {
    Ok(filed(root, head, &format!("verdicts/{tree}/"), until)?
        .into_iter()
        .map(|(_, record)| record)
        .collect())
}

/// Each record under the directory `under` at `head`, with the path it is filed at.
fn filed(
    root: &Path,
    head: Option<&str>,
    under: &str,
    until: std::time::Instant,
) -> Result<Vec<(String, Record)>, String> {
    let output = |cmd: &mut Command, input: Option<&[u8]>| output_until(cmd, input, until);
    let Some(head) = head else {
        return Ok(Vec::new());
    };
    let listing = output(
        &mut command(
            root,
            &["ls-tree", "-r", "-z", "--full-tree", head, "--", under],
        ),
        None,
    )?;
    if listing.is_empty() {
        return Ok(Vec::new());
    }
    let entries = journal_listing(listing.as_bytes())?
        .map(|line| {
            let (header, path) = line.split_once('\t')?;
            let mut fields = header.split(' ');
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some(_), Some(_), Some(object), None) => Some((object, path)),
                _ => None,
            }
        })
        .collect::<Option<Vec<_>>>()
        .ok_or("invalid journal listing")?;
    let names: String = entries
        .iter()
        .map(|(object, _)| format!("{object}\n"))
        .collect();
    let objects = output(
        &mut command(root, &["cat-file", "--batch"]),
        Some(names.as_bytes()),
    )?;
    let requested: Vec<_> = entries.iter().map(|(object, _)| *object).collect();
    let records = decode_objects(objects.as_bytes(), &requested)?;
    Ok(entries
        .into_iter()
        .map(|(_, path)| path.to_owned())
        .zip(records)
        .collect())
}

/// Whether the journal has held exactly `record`, at its tree's slot or beside it, at `head` or
/// in any commit before it. A record a later claim replaced is still one the journal took, so a
/// caller that asks before it appends never files one record twice.
pub fn published(
    root: &Path,
    head: Option<&str>,
    record: &Record,
    until: std::time::Instant,
) -> Result<bool, String> {
    let Some(head) = head else {
        return Ok(false);
    };
    let text = aterm_toml::to_string(record).map_err(|e| e.to_string())?;
    let blob = output_until(
        &mut command(root, &["hash-object", "--stdin"]),
        Some(text.as_bytes()),
        until,
    )?;
    let changes = git_until(
        root,
        &[
            "log",
            "--root",
            "--format=",
            "--raw",
            "--no-abbrev",
            head,
            "--",
            &Place::Slot.path(record),
            &Place::Beside.path(record),
        ],
        until,
    )?;
    Ok(changes
        .lines()
        .filter(|line| line.starts_with(':'))
        .any(|line| line.split_whitespace().nth(3) == Some(blob.trim())))
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
            "--root",
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
        for record in decode_objects(bytes.as_bytes(), batch)? {
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

fn decode_objects(bytes: &[u8], requested: &[&str]) -> Result<Vec<Record>, String> {
    blobs(bytes, requested)?
        .into_iter()
        .map(|text| aterm_toml::from_str(text).map_err(|e| e.to_string()))
        .collect()
}

/// The text of each blob in a `git cat-file --batch` answer, in the order asked.
fn blobs<'a>(mut bytes: &'a [u8], requested: &[&str]) -> Result<Vec<&'a str>, String> {
    let mut blobs = Vec::new();
    for expected in requested {
        let end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("truncated object header")?;
        let header = std::str::from_utf8(&bytes[..end]).map_err(|e| e.to_string())?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.len() != 3 || fields[0] != *expected || fields[1] != "blob" {
            return Err(format!("invalid object in a git batch answer: {header}"));
        }
        let length = fields[2].parse::<usize>().map_err(|e| e.to_string())?;
        bytes = &bytes[end + 1..];
        blobs.push(
            std::str::from_utf8(
                bytes
                    .get(..length)
                    .ok_or("truncated blob in a git batch answer")?,
            )
            .map_err(|e| e.to_string())?,
        );
        bytes = bytes
            .get(length..)
            .and_then(|b| b.strip_prefix(b"\n"))
            .ok_or("missing object terminator")?;
    }
    if !bytes.is_empty() {
        return Err("unexpected trailing bytes in a git batch answer".into());
    }
    Ok(blobs)
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
    let (entries, _) = edited(base, &node_of(edits)?, trees)?;
    (trees.make)(&entries)
}

/// [`edit_tree_with`], `None` where no entry differs from `base`'s.
/// # Errors
/// As [`edit_tree_with`].
fn edit_tree_if_changed(
    base: Option<&str>,
    edits: &[Edit],
    trees: &mut Trees<'_>,
) -> Result<Option<String>, String> {
    let (entries, changed) = edited(base, &node_of(edits)?, trees)?;
    if !changed && base.is_some() {
        return Ok(None);
    }
    (trees.make)(&entries).map(Some)
}

/// `edits` by directory, in order.
fn node_of(edits: &[Edit]) -> Result<Node, String> {
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
    Ok(node)
}

/// The entries of the tree `id` names (a commit names its own tree).
fn ls_tree(root: &Path, id: &str, until: std::time::Instant) -> Result<Entries, String> {
    let listing = output_until(&mut command(root, &["ls-tree", "-z", id]), None, until)?;
    let mut entries = Entries::new();
    for record in journal_listing(listing.as_bytes())? {
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

/// Journal paths are UTF-8; preserve their bytes and require every Git record's terminator.
fn journal_listing(bytes: &[u8]) -> Result<impl Iterator<Item = &str>, String> {
    let listing = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    if !listing.is_empty() && !listing.ends_with('\0') {
        return Err("unterminated journal tree listing".into());
    }
    if listing.split_terminator('\0').any(str::is_empty) {
        return Err("empty record in journal tree listing".into());
    }
    Ok(listing.split_terminator('\0'))
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
/// `verdicts/`, whatever else the journal holds. The record is filed at its tree's slot.
pub fn append(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    record: &Record,
    until: std::time::Instant,
) -> Result<bool, String> {
    append_at(root, remote, parent, record, Place::Slot, until)
}

/// [`append`], with the record filed at `place`.
pub fn append_at(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    record: &Record,
    place: Place,
    until: std::time::Instant,
) -> Result<bool, String> {
    append_with(
        root,
        remote,
        parent,
        record,
        place,
        until,
        &mut Trees {
            list: &mut |id| ls_tree(root, id, until),
            make: &mut |entries| mktree(root, entries, until),
        },
    )
}

/// [`append_at`], its tree objects read and written through `trees`.
fn append_with(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    record: &Record,
    place: Place,
    until: std::time::Instant,
    trees: &mut Trees<'_>,
) -> Result<bool, String> {
    // Reads and pruning share the admission deadline; publication owns one work window.
    let git = |root: &Path, args: &[&str]| git_until(root, args, until);
    let output = |cmd: &mut Command, input: Option<&[u8]>| output_until(cmd, input, until);
    let last_pruned = parent
        .and_then(|p| git(root, &["show", &format!("{p}:pruned-at")]).ok())
        .and_then(|s| s.parse::<u64>().ok());
    let prune = last_pruned.is_none_or(|day| day < record.updated / 86400);
    let mut edits = Vec::new();
    if prune {
        for (path, old) in filed(root, parent, "verdicts/", until)? {
            if old.updated.saturating_add(RETENTION) < record.updated {
                // A record beside its tree's slot goes by its own path, and leaves the slot's
                // record to its own age. Any other goes by the slot its tree names.
                let beside = Place::Beside.path(&old);
                edits.push(Edit::Remove {
                    path: if path == beside {
                        beside
                    } else {
                        Place::Slot.path(&old)
                    },
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
        path: place.path(record),
        blob: blob.trim().to_owned(),
    });
    let tree = edit_tree_with(parent, &edits, trees)?;
    let message = format!("verify watch: {} {}", record.state, record.commit);
    publish(
        root,
        remote,
        parent,
        &tree,
        &message,
        &AppendBudget::new(until),
    )
    .map(|pushed| matches!(pushed, Appended::Holds(_)))
}

/// Commit `tree` over `parent` (none: the journal's first commit) and push it as the journal's
/// head, a normal fast-forward that is the compare-and-swap: the commit, or the imported head
/// observed after a competing append moved the journal first.
/// # Errors
/// A local object or remote step failed, or the push was refused while the journal stayed at
/// `parent`.
fn publish(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    tree: &str,
    message: &str,
    budget: &AppendBudget,
) -> Result<Appended, String> {
    let started = std::time::Instant::now();
    let timed = |why| {
        format!(
            "{why}; journal_publish_elapsed_ms={}",
            started.elapsed().as_millis()
        )
    };
    let mut commit = command(root, &["commit-tree", tree, "-m", message]);
    if let Some(parent) = parent {
        commit.args(["-p", parent]);
    }
    let sha = output_until(&mut commit, None, budget.deadline())
        .map_err(timed)?
        .trim()
        .to_owned();
    let git = |root: &Path| command(root, &[]);
    let run = |tool, args: &[std::ffi::OsString]| {
        capture_until(
            &mut crate::remote::command(root, &git, tool, args)?,
            None,
            budget.deadline(),
        )
        .map_err(|why| format!("{tool:?} journal transport: {why}"))
    };
    let write = |tool, args: &[std::ffi::OsString]| {
        let MutationBudget(until) = budget.admit(std::time::Instant::now())?;
        let scoped = INTERRUPTIBLE.get();
        let watch = scoped.then(aterm_toolchain::interrupt::Watch::enter);
        crate::remote::run_mutation(root, &git, tool, args, until, &mut || {
            scoped
                && (crate::exec::group::taken().is_some()
                    || watch.as_ref().is_some_and(|watch| watch.caught().is_some()))
        })
        .map_err(|why| format!("{tool:?} journal mutation: {why}"))
    };
    push_journal(
        root,
        remote,
        parent,
        &sha,
        &run,
        crate::remote::WriteRunner(&write),
    )
    .map_err(timed)
}

fn push_journal(
    root: &Path,
    name: &str,
    parent: Option<&str>,
    sha: &str,
    run: &crate::remote::Runner<'_>,
    write: crate::remote::WriteRunner<'_>,
) -> Result<Appended, String> {
    let context = |why| {
        format!(
            "{why}; journal_ref={REF} journal_before={} journal_after={sha}",
            parent.unwrap_or("missing")
        )
    };
    let remote = crate::remote::Remote::open(root, name, run).map_err(context)?;
    let why = match remote.push_with(write, REF, parent, sha).map_err(context)? {
        crate::remote::Pushed::Done => return Ok(Appended::Holds(sha.to_owned())),
        crate::remote::Pushed::Rejected(why) => why,
        crate::remote::Pushed::Blocked { why, .. } | crate::remote::Pushed::Unsent(why) => {
            return Err(context(why));
        }
    };
    // A race is retryable only when the remote actually moved. Authentication/network failures
    // remain errors, rather than turning into a tight retry loop.
    let head = remote.fetch(REF).map_err(|read| {
        context(format!(
            "{why}; {}: {read}",
            ReadContext::AfterPushFailure.word()
        ))
    })?;
    if head.as_deref() != parent {
        return Ok(Appended::Raced(head));
    }
    Err(context(why))
}

/// The directory of `verify land`'s verdict records ([`crate::land_record`]), beside `verdicts/`:
/// `land/<utc-day>/<T>/<check>.<key>.<word>.toml`.
pub const LAND: &str = crate::land_record::DIR;

/// The UTC day [`LAND`]'s last prune went by. Only writers that know `land/` read or write it, so
/// an older writer's append, which writes the lane's `pruned-at`, never takes the day's land prune
/// away.
pub const LAND_PRUNED_AT: &str = "land/pruned-at";

/// The directory of `verify land`'s run records ([`crate::land_run`]) inside [`LAND`]:
/// `land/runs/<utc-day>/<run>.<seq>.<kind>.toml`.
pub const RUNS: &str = crate::land_run::DIR;

/// How long a verdict record's day directory stays at least once the day has ended
/// ([`expired`]); longer where the lane's clock is behind the appender's ([`land_append`]).
pub const LAND_RETENTION: u64 = 2 * 86_400;

/// How long a run record's day directory stays at least once the day has ended: the census's
/// window ([`crate::land_census`]).
pub const RUN_RETENTION: u64 = 14 * 86_400;

/// Which kind of land record a day directory holds, by which its retention goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// A verdict record's, [`LAND_RETENTION`].
    Verdict,
    /// A run record's, [`RUN_RETENTION`].
    Run,
}

impl Kept {
    /// How long its day directory stays at least.
    #[must_use]
    pub const fn retention(self) -> u64 {
        match self {
            Self::Verdict => LAND_RETENTION,
            Self::Run => RUN_RETENTION,
        }
    }
}

/// Whether the land prune by UTC day `today` drops the day directory `day` of records of `kind`:
/// at least its retention has passed since that day ended, so every record in it ended that
/// long ago.
#[must_use]
pub fn expired(kind: Kept, day: u64, today: u64) -> bool {
    day.saturating_add(1 + kind.retention() / 86_400) <= today
}

/// Every verdict record filed on the journal's `head` under one of `trees`, in every day
/// directory (so no writer's clock skew hides one), each held to
/// [`crate::land_record::Entry::read`]'s checks; and each entry of `land/` that is no day
/// directory, but the run records' [`RUNS`] ([`run_records`]), named as unreadable. Only those
/// trees' directories are listed. One bad entry never fails the read: one that is no regular
/// file, or is longer than [`crate::land_record::MAX_BYTES`], is unreadable, and is not read.
/// # Errors
/// A git read failed.
pub fn land_records(
    root: &Path,
    head: Option<&str>,
    trees: &[&str],
    until: std::time::Instant,
) -> Result<Vec<crate::land_record::Entry>, String> {
    use crate::land_record::{Entry, Origin};
    let Some(head) = head else {
        return Ok(Vec::new());
    };
    let top = ls_tree(root, head, until)?;
    let Some(land) = top.get(LAND) else {
        return Ok(Vec::new());
    };
    if !land.is_tree() {
        return Ok(vec![Entry::unread(
            LAND,
            Origin::Journal,
            "it is no directory".into(),
        )]);
    }
    let wanted: std::collections::BTreeSet<&str> = trees
        .iter()
        .copied()
        .filter(|tree| crate::land_record::tree_id(tree))
        .collect();
    let mut entries = Vec::new();
    let mut dirs = Vec::new();
    for (name, entry) in ls_tree(root, &land.id, until)? {
        let path = format!("{LAND}/{name}");
        if (path == LAND_PRUNED_AT && !entry.is_tree()) || (path == RUNS && entry.is_tree()) {
            continue;
        }
        if !entry.is_tree() || crate::land_record::day_of(&name).is_none() {
            entries.push(Entry::unread(
                &path,
                Origin::Journal,
                "it is no day directory".into(),
            ));
            continue;
        }
        dirs.extend(wanted.iter().map(|tree| format!("{path}/{tree}")));
    }
    if dirs.is_empty() {
        return Ok(entries);
    }
    let (read, unread) = blobs_under(root, head, &dirs, until)?;
    entries.extend(
        unread
            .into_iter()
            .map(|(path, why)| Entry::unread(&path, Origin::Journal, why)),
    );
    entries.extend(
        read.iter()
            .map(|(path, bytes)| Entry::read(path, bytes, Origin::Journal)),
    );
    Ok(entries)
}

/// Every run record filed on the journal's `head` in a day directory of [`RUNS`] from UTC day
/// `since` on, each held to [`crate::land_run::Entry::read`]'s checks; and each entry of
/// [`RUNS`] that is no day directory, named as unreadable. One bad entry never fails the read.
/// # Errors
/// A git read failed.
pub fn run_records(
    root: &Path,
    head: Option<&str>,
    since: u64,
    until: std::time::Instant,
) -> Result<Vec<crate::land_run::Entry>, String> {
    use crate::land_record::Origin;
    use crate::land_run::Entry;
    let Some(head) = head else {
        return Ok(Vec::new());
    };
    let top = ls_tree(root, head, until)?;
    let Some(land) = top.get(LAND) else {
        return Ok(Vec::new());
    };
    if !land.is_tree() {
        return Ok(vec![Entry::unread(
            LAND,
            Origin::Journal,
            "it is no directory".into(),
        )]);
    }
    let Some(runs) = ls_tree(root, &land.id, until)?
        .get(&RUNS[LAND.len() + 1..])
        .cloned()
    else {
        return Ok(Vec::new());
    };
    if !runs.is_tree() {
        return Ok(vec![Entry::unread(
            RUNS,
            Origin::Journal,
            "it is no directory".into(),
        )]);
    }
    let mut entries = Vec::new();
    let mut dirs = Vec::new();
    for (name, entry) in ls_tree(root, &runs.id, until)? {
        let path = format!("{RUNS}/{name}");
        match crate::land_record::day_of(&name) {
            Some(day) if entry.is_tree() => {
                if day >= since {
                    dirs.push(format!("{path}/"));
                }
            }
            _ => entries.push(Entry::unread(
                &path,
                Origin::Journal,
                "it is no day directory".into(),
            )),
        }
    }
    if dirs.is_empty() {
        return Ok(entries);
    }
    let (read, unread) = blobs_under(root, head, &dirs, until)?;
    entries.extend(
        unread
            .into_iter()
            .map(|(path, why)| Entry::unread(&path, Origin::Journal, why)),
    );
    entries.extend(
        read.iter()
            .map(|(path, bytes)| Entry::read(path, bytes, Origin::Journal)),
    );
    Ok(entries)
}

/// Each file selected by `paths` on the journal's `head`, recursively under directories, by one
/// listing and one batch read: those read, with their bytes, and those not read, with why (one
/// that is no regular file, longer than [`crate::land_record::MAX_BYTES`], or whose object is
/// missing).
/// # Errors
/// A git read failed, or its output did not parse.
fn blobs_under(
    root: &Path,
    head: &str,
    paths: &[String],
    until: std::time::Instant,
) -> Result<ReadBlobs, String> {
    let mut args = vec!["ls-tree", "-r", "-z", "--full-tree", "--long", head, "--"];
    args.extend(paths.iter().map(String::as_str));
    let listing = capture_until(&mut command(root, &args), None, until)?;
    if !listing.status.success() {
        return Err(String::from_utf8_lossy(&listing.stderr).into_owned());
    }
    let mut unread = Vec::new();
    let mut blobs = Vec::new();
    for item in journal_listing(&listing.stdout)? {
        let invalid = || format!("invalid journal tree entry: {item:?}");
        let (fields, path) = item.split_once('\t').ok_or_else(invalid)?;
        // The standard long format pads sizes; only the metadata is whitespace-separated.
        let mut fields = fields.split_whitespace();
        let (Some(mode), Some(kind), Some(id), Some(size), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(invalid());
        };
        if kind != "blob" || !matches!(mode, "100644" | "100755") {
            unread.push((path.to_owned(), "it is no regular file".to_owned()));
        } else if size
            .parse::<u64>()
            .is_ok_and(|size| size <= crate::land_record::MAX_BYTES)
        {
            blobs.push((path.to_owned(), id.to_owned()));
        } else {
            unread.push((
                path.to_owned(),
                format!("it is longer than {} bytes", crate::land_record::MAX_BYTES),
            ));
        }
    }
    if blobs.is_empty() {
        return Ok((Vec::new(), unread));
    }
    let input: String = blobs.iter().map(|(_, id)| format!("{id}\n")).collect();
    let objects = capture_until(
        &mut command(root, &["cat-file", "--batch"]),
        Some(input.as_bytes()),
        until,
    )?;
    if !objects.status.success() {
        return Err(String::from_utf8_lossy(&objects.stderr).into_owned());
    }
    let (read, missing) = parse_blobs(&blobs, &objects.stdout)?;
    unread.extend(missing);
    Ok((read, unread))
}

/// Bind every batch response to its requested object before accepting its body.
fn parse_blobs(blobs: &[(String, String)], mut bytes: &[u8]) -> Result<ReadBlobs, String> {
    let mut read = Vec::new();
    let mut unread = Vec::new();
    for (path, expected) in blobs {
        let end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("truncated object header")?;
        let header = String::from_utf8_lossy(&bytes[..end]).into_owned();
        bytes = &bytes[end + 1..];
        let fields: Vec<&str> = header.split(' ').collect();
        match fields[..] {
            [id, "blob", size] if id == expected => {
                let size = size.parse::<usize>().map_err(|e| e.to_string())?;
                if size as u64 > crate::land_record::MAX_BYTES {
                    return Err("journal blob exceeds the record size limit".into());
                }
                let body = bytes.get(..size).ok_or("truncated journal blob")?;
                read.push((path.clone(), body.to_vec()));
                bytes = bytes
                    .get(size..)
                    .and_then(|b| b.strip_prefix(b"\n"))
                    .ok_or("missing object terminator")?;
            }
            [id, "missing"] if id == expected => {
                unread.push((path.clone(), "its object is missing".to_owned()));
            }
            _ => return Err(format!("invalid journal object: {header}")),
        }
    }
    if !bytes.is_empty() {
        return Err("unexpected bytes after journal objects".into());
    }
    Ok((read, unread))
}

/// What [`blobs_under`] read: each file's path and bytes, and each it did not read, with why.
type ReadBlobs = (Vec<(String, Vec<u8>)>, Vec<(String, String)>);

/// What one land append attempt came to ([`land_append`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Appended {
    /// The journal's head, which holds a record at each path given, but those a prune drops.
    Holds(String),
    /// The imported head observed after a competing write, or `None` if it removed the ref.
    /// Retry against this exact state; a subsequent change still fails the compare-and-swap.
    Raced(Option<String>),
}

/// Append land `records` (verdict records and run records) over the journal's `parent` at Unix
/// time `now`: one compare-and-swap attempt, whose new tree edits only the root, `land/`, each
/// `land/<day>/` and each `<T>/` it writes, `land/runs/` and each `land/runs/<day>/` it writes
/// (and the day directories it prunes). A verdict FAIL's path keeps the first record filed there:
/// a FAIL already on `parent`, or earlier in `records`, is not replaced, while an `ok`, `skip` or
/// `NOT RUN` record, and a run record, replaces the one at its path. The prune goes by the
/// earlier of `now`'s UTC day and the lane's own day (its `pruned-at`, the clock of the Mac that
/// judges main, which no land writes), so no one Mac's clock drops a record early: a land Mac
/// whose clock runs ahead prunes no further than the lane's day, and with no lane day in the
/// journal nothing is pruned. On the first land append past [`LAND_PRUNED_AT`]'s day by that
/// bound, the day directories [`expired`] drops by it go, each kind by its own retention
/// ([`LAND_RETENTION`] for `land/<day>/`, [`RUN_RETENTION`] for `land/runs/<day>/`), no blob
/// read, and [`LAND_PRUNED_AT`] takes that day, so it never moves back nor past the lane's. A
/// record filed under a day its kind's retention has expired by `now` is not appended. With
/// nothing to change, nothing is pushed.
/// # Errors
/// A record's path does not parse, a git step failed, or the push was refused while the
/// journal stayed at `parent`.
pub fn land_append(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    records: &[crate::land_record::Signed],
    now: u64,
    until: std::time::Instant,
) -> Result<Appended, String> {
    let budget = AppendBudget::new(until);
    land_append_with(root, remote, parent, records, now, &budget, &mut |bytes| {
        write_blob(root, bytes, budget.deadline())
    })
}

fn write_blob(root: &Path, bytes: &[u8], until: std::time::Instant) -> Result<String, String> {
    output_until(
        &mut command(root, &["hash-object", "-w", "--stdin"]),
        Some(bytes),
        until,
    )
    .map(|id| id.trim().to_owned())
}

/// Share only successful blob writes, by exact bytes, within one append to one repository.
/// A race rebuilds trees against its new parent but does not need to write those blobs again.
fn cached_blobs(
    mut write: impl FnMut(&[u8]) -> Result<String, String>,
) -> impl FnMut(&[u8]) -> Result<String, String> {
    let mut written = std::collections::BTreeMap::<Vec<u8>, String>::new();
    move |bytes| {
        if let Some(id) = written.get(bytes) {
            return Ok(id.clone());
        }
        let id = write(bytes)?;
        written.insert(bytes.to_vec(), id.clone());
        Ok(id)
    }
}

/// [`land_append`], with blob publication shared across attempts by the caller.
fn land_append_with(
    root: &Path,
    remote: &str,
    parent: Option<&str>,
    records: &[crate::land_record::Signed],
    now: u64,
    budget: &AppendBudget,
    blob: &mut dyn FnMut(&[u8]) -> Result<String, String>,
) -> Result<Appended, String> {
    let until = budget.deadline();
    let today = now / 86_400;
    let mut trees = Trees {
        list: &mut |id| ls_tree(root, id, until),
        make: &mut |entries| mktree(root, entries, until),
    };
    let mut edits = Vec::new();
    let day_at = |path: &str| {
        parent
            .and_then(|p| git_until(root, &["show", &format!("{p}:{path}")], until).ok())
            .and_then(|day| day.parse::<u64>().ok())
    };
    let pruned = day_at(LAND_PRUNED_AT);
    // Read the lane's day only where this Mac's would prune: the bound is never past it.
    let by = if pruned.is_none_or(|day| day < today) {
        day_at("pruned-at")
            .map(|lane| lane.min(today))
            .filter(|by| pruned.is_none_or(|day| day < *by))
    } else {
        None
    };
    if let Some(by) = by {
        if let Some(parent) = parent
            && let Some(land) = (trees.list)(parent)?.get(LAND).filter(|e| e.is_tree())
        {
            let listed = (trees.list)(&land.id)?;
            let days = |dir: &str, entries: &Entries, kind: Kept| -> Vec<Edit> {
                entries
                    .iter()
                    .filter(|(name, entry)| {
                        entry.is_tree()
                            && crate::land_record::day_of(name)
                                .is_some_and(|day| expired(kind, day, by))
                    })
                    .map(|(name, _)| Edit::Remove {
                        path: format!("{dir}/{name}"),
                    })
                    .collect()
            };
            edits.extend(days(LAND, &listed, Kept::Verdict));
            if let Some(runs) = listed.get(&RUNS[LAND.len() + 1..]).filter(|e| e.is_tree()) {
                edits.extend(days(RUNS, &(trees.list)(&runs.id)?, Kept::Run));
            }
        }
        edits.push(Edit::Put {
            path: LAND_PRUNED_AT.into(),
            blob: blob(by.to_string().as_bytes())?,
        });
    }
    let mut fails = Vec::new();
    let mut appended = Vec::new();
    for record in records {
        let (_, expired, fail) = crate::land_spool::kept_as(&record.path, today)?;
        if expired {
            continue;
        }
        if fail {
            if fails.contains(&record.path.as_str()) {
                continue;
            }
            fails.push(record.path.as_str());
        }
        appended.push(record);
    }
    let kept: std::collections::BTreeSet<String> = match parent {
        Some(parent) if !fails.is_empty() => {
            let mut args = vec!["ls-tree", "-z", "--full-tree", "--name-only", parent, "--"];
            args.extend(fails.iter().copied());
            let listing = output_until(&mut command(root, &args), None, until)?;
            journal_listing(listing.as_bytes())?
                .map(str::to_owned)
                .collect()
        }
        _ => std::collections::BTreeSet::new(),
    };
    for record in appended {
        if kept.contains(&record.path) {
            continue;
        }
        edits.push(Edit::Put {
            path: record.path.clone(),
            blob: blob(&record.bytes)?,
        });
    }
    let Some(tree) = edit_tree_if_changed(parent, &edits, &mut trees)? else {
        return parent
            .map(|head| Appended::Holds(head.to_owned()))
            .ok_or_else(|| "an empty journal holds nothing".to_owned());
    };
    let message = format!("verify land: {} records", records.len());
    publish(root, remote, parent, &tree, &message, budget)
}

/// Append `records` until an append holds them ([`land_append`]): each race carries the observed
/// journal head into the next attempt. Initial reads and assembly share `until`; the first
/// admitted mutation starts one work window shared by race readback and all later attempts.
/// A writer retains its owner through settlement. The head that holds
/// them (`None`: no records, and no journal).
/// # Errors
/// As [`land_append`], or the deadline passed.
pub fn append_land(
    root: &Path,
    remote: &str,
    records: &[crate::land_record::Signed],
    now: u64,
    until: std::time::Instant,
) -> Result<Option<String>, String> {
    let budget = AppendBudget::new(until);
    let mut blob = cached_blobs(|bytes| write_blob(root, bytes, budget.deadline()));
    append_land_through(
        records.is_empty(),
        || head_in(root, remote, budget.deadline(), ReadContext::AppendHead),
        |parent| land_append_with(root, remote, parent, records, now, &budget, &mut blob),
    )
}

fn append_land_through(
    empty: bool,
    mut read: impl FnMut() -> Result<Option<String>, String>,
    mut append: impl FnMut(Option<&str>) -> Result<Appended, String>,
) -> Result<Option<String>, String> {
    let started = std::time::Instant::now();
    let mut attempt = 0_u64;
    let mut observed = None;
    loop {
        attempt = attempt.saturating_add(1);
        let attempt_started = std::time::Instant::now();
        let parent = match observed.take() {
            Some(head) => Ok(head),
            None => read(),
        };
        let head_ms = attempt_started.elapsed().as_millis();
        let timed = |why| {
            format!(
                "{why}; journal_append_elapsed_ms={} journal_attempt={} \
                 journal_attempt_elapsed_ms={} journal_head_elapsed_ms={head_ms}",
                started.elapsed().as_millis(),
                attempt,
                attempt_started.elapsed().as_millis(),
            )
        };
        let parent = parent.map_err(timed)?;
        if empty {
            return Ok(parent);
        }
        match append(parent.as_deref()).map_err(timed)? {
            Appended::Holds(head) => return Ok(Some(head)),
            Appended::Raced(head) => observed = Some(head),
        }
    }
}

#[cfg(test)]
#[path = "judged_tests.rs"]
mod tests;
