// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WORKTREE STEP'S PINS (DC-02b): before a linked worktree whose session is gone goes, what
//! it holds that git keeps a copy of once it is on a ref is put on refs of its own checkout, and
//! those refs are pushed to the repository's own origin.
//!
//! - Its edits, staged or not, and its untracked files are one snapshot commit, whose tree is
//!   what `git add -A` reads in it through a private index in a folder of the system's temporary
//!   one (neither its tree, its index nor any other file of git's for it is written: only
//!   objects and the refs below), whose first parent is its HEAD, whose second is its index when
//!   that differs from both, and whose further parents are the commits only its reflogs held (at
//!   most [`MAX_TIPS`]): `refs/snapshots/<name>/<the commit's first 12 hex digits>`. The commit
//!   is the same for the same worktree read twice (its author and committer are `aterm`, dated
//!   at its HEAD's commit time), so a second pass writes no second ref.
//! - A clean worktree whose HEAD `origin/main` lacks has its HEAD on
//!   `refs/orphans/<name>/<12 hex digits>`.
//! - Files it ignores, and each file larger than [`BIG_FILE`] (which GitHub refuses), go on one
//!   more commit, over the snapshot, on `refs/snapshots-local/<name>/<12 hex digits>`, which is
//!   never pushed: a `.env` or local notes stay on this machine. A folder that holds a
//!   `CACHEDIR.TAG` goes on no ref.
//! - A commit a submodule checked out in it holds that the checkout's module witness lacks is
//!   fetched into the witness, its objects alone, and rooted there on
//!   `refs/snapshots/<name>/<12 hex digits>`.
//!
//! Every pin is a ref created only where none stands by its name (`git update-ref <ref> <oid>
//! ""`; one that names the same commit is the same pin), and the removal reads each one back,
//! with every object it reaches in the shared repository, before anything goes
//! (`retired::remove_pinned_worktree`). They are pushed, each under its own name, to the origin
//! of the repository that holds them and nowhere else, never by force and only where no ref of
//! that name stands there, and read back after: a local origin by `git push`, a GitHub one by
//! `aterm agh repo push` from the all-zero commit (aterm-verify's `Remote::push` is private to
//! the release tool, so these are the step's own two arms). A push that fails is owed
//! (`disk-owed.json` in aterm's state folder, written before the worktree goes) and tried again
//! at a later step, backing off to a week; the removal never waits on it, and the refs stand in
//! the checkout meanwhile. One origin refuses because a ref of its name there names another
//! commit is parked: journalled, and tried no more.
//!
//! A worktree stays whose untracked, ignored and changed files are more than [`MAX_BYTES`] or
//! [`MAX_FILES`], whose volume has less free than twice their bytes and [`ROOM`], or once the
//! step's snapshots read [`STEP_BYTES`]: a session's uncommitted work is text a person or an
//! agent wrote, and a quarter of a gibibyte beside it is a build product, a dataset or a model,
//! which git is the wrong store for and a push would carry to every clone of its origin. It
//! stays too where a file to be read has a `filter` attribute: git would run a program on it,
//! and Git LFS's would leave its content off every ref.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant, SystemTime};

use atpkg::build_cache::retired::{self, Admitted, Git, Pinned};

/// The most bytes of untracked, ignored and changed files one worktree's snapshot reads.
pub const MAX_BYTES: u64 = 256 << 20;

/// The most files and folders under one worktree's untracked, ignored and changed entries its
/// snapshot reads.
pub const MAX_FILES: usize = 20_000;

/// The largest file a pushed snapshot holds: GitHub refuses a push with a file over 100 MiB, so
/// a larger one goes on the local ref alone.
pub const BIG_FILE: u64 = 100 << 20;

/// The free space a snapshot leaves its volume beyond twice the bytes it reads.
pub const ROOM: u64 = 1 << 30;

/// The most bytes one step's snapshots read together; a worktree past it is pinned at a later
/// step.
pub const STEP_BYTES: u64 = 1 << 30;

/// The most commits only a worktree's reflogs hold that its snapshot takes as parents.
pub const MAX_TIPS: usize = 16;

/// The most pushes the owed file holds; past it the oldest is dropped (its refs stand in its
/// checkout, unpushed) and journalled.
const MAX_OWED: usize = 1024;

/// The most bytes the owed file may hold for a read.
const OWED_BYTES: usize = 4 << 20;

/// The owed file's name in aterm's state folder.
pub const OWED_FILE: &str = "disk-owed.json";

/// How long a push that failed first waits before its next try; each failure doubles it, up to
/// [`BACKOFF_MOST`].
const BACKOFF_FIRST: u64 = 60 * 60;

/// The longest a failed push waits before its next try.
const BACKOFF_MOST: u64 = 7 * 24 * 60 * 60;

/// How many paths one `git check-attr` reads.
const ATTR_BATCH: usize = 512;

/// The config every git run here is given: no file system monitor or untracked cache answers
/// for a file git did not read itself, no hook runs, no split index, no submodule recursion, no
/// automatic maintenance and no signing program.
const CONFIG: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "submodule.recurse=false",
    "-c",
    "maintenance.auto=false",
    "-c",
    "gc.auto=0",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "core.splitIndex=false",
    "-c",
    "core.untrackedCache=false",
];

/// What git answered.
struct Ran {
    code: Option<i32>,
    out: String,
    err: String,
}

impl Ran {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// Its output, trimmed, when it succeeded.
    fn line(self, what: &str) -> Result<String, String> {
        if self.ok() {
            Ok(self.out.trim().to_owned())
        } else {
            Err(format!("git could not {what}: {}", self.err))
        }
    }
}

/// `git <CONFIG> <args>` in `dir` through `git` (whose runs end by its own bound and clear this
/// process's `GIT_*` environment), with `index` as its index when given and `env` set, in the C
/// locale.
fn run(
    git: &dyn Git,
    dir: &Path,
    args: &[&str],
    index: Option<&Path>,
    env: &[(&str, &str)],
) -> Ran {
    let mut all = CONFIG.to_vec();
    all.extend_from_slice(args);
    let mut vars = vec![("LC_ALL", "C")];
    vars.extend_from_slice(env);
    let a = git.run_env(dir, &all, index, &vars);
    Ran {
        code: a.code,
        out: a.out,
        err: a.err,
    }
}

/// [`run`] with the worktree's own index and no more environment.
fn read(git: &dyn Git, dir: &Path, args: &[&str]) -> Ran {
    run(git, dir, args, None, &[])
}

/// `path` as one argument.
fn arg(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("{} is no path git takes as an argument", path.display()))
}

/// The first 12 hex digits of `oid`.
fn short(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

/// Whether `oid` is a full lowercase object name of either of git's formats.
fn oid_shaped(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The name a worktree's pins carry: its folder's name less `.noindex`, each character git
/// does not take in a ref name's part made `-`.
#[must_use]
pub fn ref_name(wt: &Path) -> String {
    let folder = wt
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = folder.strip_suffix(".noindex").unwrap_or(&folder);
    let mut name: String = folder
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    name.truncate(96);
    let name = name.trim_matches('-');
    if name.is_empty() {
        "worktree".to_owned()
    } else {
        name.to_owned()
    }
}

/// One ref a pin wrote, with the repository whose origin it goes to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Push {
    /// The repository that holds the ref: a main checkout, or a submodule's git directory in
    /// one (its module witness).
    pub repo: PathBuf,
    /// Each ref, `[its full name, the commit it names]`.
    pub refs: Vec<[String; 2]>,
}

impl Push {
    /// Each ref, for a record: `name@<12 hex digits>`.
    #[must_use]
    pub fn words(&self) -> Vec<String> {
        self.refs
            .iter()
            .map(|[name, oid]| format!("{name}@{}", short(oid)))
            .collect()
    }
}

/// What pinning one worktree wrote.
#[derive(Debug, Clone)]
pub struct Pins {
    /// What the remover holds the worktree to.
    pub pinned: Pinned,
    /// The refs owed to an origin.
    pub pushes: Vec<Push>,
    /// The refs never pushed (its ignored and large files'), `(name, commit)`.
    pub local: Vec<(String, String)>,
    /// The bytes of untracked, ignored and changed files its snapshot read.
    pub bytes: u64,
}

impl Pins {
    /// Each ref the pins wrote, for a record: `name@<12 hex digits>`.
    #[must_use]
    pub fn words(&self) -> String {
        let mut all: Vec<String> = self
            .pinned
            .roots
            .iter()
            .map(|(name, oid)| format!("{name}@{}", short(oid)))
            .collect();
        for [name, oid] in self.pushes.iter().flat_map(|p| p.refs.iter()) {
            if !self.pinned.roots.iter().any(|(n, o)| n == name && o == oid) {
                all.push(format!("{name}@{} (a submodule's)", short(oid)));
            }
        }
        all.join(", ")
    }
}

/// Create `name` at `oid` in the repository `dir` (a `--git-dir` in `prefix` when given) only
/// where no ref stands by that name; one that already names `oid` is the same pin.
fn create_ref(
    git: &dyn Git,
    dir: &Path,
    prefix: &[&str],
    name: &str,
    oid: &str,
) -> Result<(), String> {
    let mut args = prefix.to_vec();
    args.extend(["update-ref", "--no-deref", name, oid, ""]);
    if read(git, dir, &args).ok() {
        return Ok(());
    }
    let mut args = prefix.to_vec();
    args.extend(["rev-parse", "--verify", "--quiet", name]);
    let now = read(git, dir, &args);
    if now.ok() && now.out.trim() == oid {
        return Ok(());
    }
    Err(format!(
        "its pin {name} could not be made at {}: a ref of that name names another commit",
        short(oid)
    ))
}

/// The untracked (`??`) and ignored (`!!`) rows of `git status` in `wt` (submodules not read),
/// each named as status names it.
fn rows(git: &dyn Git, wt: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let status = read(
        git,
        wt,
        &[
            "status",
            "--porcelain",
            "-z",
            "--ignored",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ],
    );
    if !status.ok() {
        return Err(format!("git status could not read it: {}", status.err));
    }
    let mut untracked = Vec::new();
    let mut ignored = Vec::new();
    let mut fields = status.out.split('\0').filter(|r| !r.is_empty());
    while let Some(row) = fields.next() {
        if let Some(rel) = row.strip_prefix("?? ") {
            untracked.push(rel.to_owned());
        } else if let Some(rel) = row.strip_prefix("!! ") {
            ignored.push(rel.to_owned());
        } else if row.starts_with('R') || row.starts_with('C') {
            fields.next();
        }
    }
    Ok((untracked, ignored))
}

/// The tracked files of `wt` git reads as changed or deleted (`git ls-files -m`).
fn changed(git: &dyn Git, wt: &Path) -> Result<Vec<String>, String> {
    let ls = read(git, wt, &["ls-files", "-z", "-m"]);
    if !ls.ok() {
        return Err(format!("git could not list its changed files: {}", ls.err));
    }
    Ok(ls
        .out
        .split('\0')
        .filter(|r| !r.is_empty())
        .map(str::to_owned)
        .collect())
}

/// What a snapshot reads of a worktree beyond its tracked files as they are.
#[derive(Debug, Default)]
struct Weighed {
    /// The bytes of the files under the entries walked.
    bytes: u64,
    /// The files and folders under them.
    count: usize,
    /// Each file larger than [`BIG_FILE`], as a path of the worktree.
    big: Vec<String>,
    /// Each folder under them that holds a `CACHEDIR.TAG`, as a path of the worktree: no ref
    /// holds it.
    caches: Vec<String>,
    /// Every file walked, as a path of the worktree.
    files: Vec<String>,
}

/// The files and folders under each of `rels` in `wt` (a deleted file is none), links counted
/// as files and never followed, a folder that holds a `CACHEDIR.TAG` not entered, stopping past
/// [`MAX_BYTES`] or [`MAX_FILES`] or at `until`.
fn weigh(wt: &Path, rels: &[String], until: Option<Instant>) -> Result<Weighed, String> {
    let mut w = Weighed::default();
    let mut stack: Vec<String> = rels
        .iter()
        .map(|r| r.trim_end_matches('/').to_owned())
        .collect();
    stack.sort();
    stack.dedup();
    while let Some(rel) = stack.pop() {
        if until.is_some_and(|until| Instant::now() >= until) {
            return Err("the walk of its files ran out of time, so it stays".to_owned());
        }
        let at = wt.join(&rel);
        let meta = match std::fs::symlink_metadata(&at) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{} could not be read: {e}", at.display())),
        };
        if meta.is_dir() {
            if retired::cache_tagged_row(wt, &format!("{rel}/")) {
                w.caches.push(rel);
                continue;
            }
            let entries = std::fs::read_dir(&at)
                .map_err(|e| format!("{} could not be listed: {e}", at.display()))?;
            for entry in entries {
                let entry =
                    entry.map_err(|e| format!("{} could not be listed: {e}", at.display()))?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| format!("{} holds a name git cannot pin", at.display()))?;
                stack.push(format!("{rel}/{name}"));
            }
        } else {
            if meta.is_file() && meta.len() > BIG_FILE {
                w.big.push(rel.clone());
            }
            w.bytes = w.bytes.saturating_add(meta.len());
            w.files.push(rel);
        }
        w.count += 1;
        if w.bytes > MAX_BYTES || w.count > MAX_FILES {
            return Err(format!(
                "it holds more untracked, ignored and changed files than a snapshot takes (more \
                 than {MAX_FILES} files or {} MiB)",
                MAX_BYTES >> 20
            ));
        }
    }
    Ok(w)
}

/// Whether any of `files` in `wt` has a `filter` attribute, which `git add` would run a
/// program on (Git LFS's among them, which would leave its content off every ref).
fn unfiltered(git: &dyn Git, wt: &Path, files: &[String]) -> Result<(), String> {
    for batch in files.chunks(ATTR_BATCH) {
        let mut args = vec!["check-attr", "-z", "filter", "--"];
        args.extend(batch.iter().map(String::as_str));
        let attrs = read(git, wt, &args);
        if !attrs.ok() {
            return Err(format!(
                "git could not read its files' attributes: {}",
                attrs.err
            ));
        }
        let fields: Vec<&str> = attrs.out.split('\0').collect();
        for row in fields.chunks(3) {
            if let [path, _, value] = row
                && !matches!(*value, "unspecified" | "unset")
            {
                return Err(format!(
                    "{path} has the filter {value}, which git would run a program on to pin it, \
                     so it stays"
                ));
            }
        }
    }
    Ok(())
}

/// Each gitlink path of `tree` in `checkout` (`git ls-tree -r`).
fn gitlinks(git: &dyn Git, checkout: &Path, tree: &str) -> Result<Vec<String>, String> {
    let ls = read(git, checkout, &["ls-tree", "-r", "-z", "--full-tree", tree]);
    if !ls.ok() {
        return Err(format!("git could not list a tree it wrote: {}", ls.err));
    }
    let mut out = Vec::new();
    for row in ls.out.split('\0').filter(|r| !r.is_empty()) {
        let Some((meta, path)) = row.split_once('\t') else {
            return Err(format!("git ls-tree gave a row it does not write ({row})"));
        };
        if meta.starts_with("160000 ") {
            out.push(path.to_owned());
        }
    }
    Ok(out)
}

/// Whether `tree` holds no gitlink `head_tree` lacks: `git add` takes a repository inside the
/// worktree as one, and no pin holds its files.
fn no_new_gitlink(
    git: &dyn Git,
    checkout: &Path,
    head_tree: &str,
    tree: &str,
) -> Result<(), String> {
    if tree == head_tree {
        return Ok(());
    }
    let had = gitlinks(git, checkout, head_tree)?;
    let now = gitlinks(git, checkout, tree)?;
    match now.into_iter().find(|p| !had.contains(p)) {
        Some(path) => Err(format!(
            "{path} is a repository inside it, whose files no pin can hold, so it stays"
        )),
        None => Ok(()),
    }
}

/// The commits only `wt`'s reflogs hold, its HEAD's (through its own entry) and its branch's
/// (`branch`, the full `refs/heads/…`): each one neither `head` nor `origin/main` holds, the
/// independent ones of them, at most [`MAX_TIPS`].
fn reflog_tips(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    head: &str,
    branch: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut oids = Vec::new();
    let mut logs = vec![(wt, "HEAD")];
    if let Some(branch) = branch {
        logs.push((checkout, branch));
    }
    for (dir, name) in logs {
        let shown = read(git, dir, &["reflog", "show", "--format=%H", name, "--"]);
        if !shown.ok() {
            return Err(format!(
                "git could not read its {name} reflog: {}",
                shown.err
            ));
        }
        oids.extend(
            shown
                .out
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned),
        );
    }
    oids.sort();
    oids.dedup();
    if let Some(odd) = oids.iter().find(|o| !oid_shaped(o)) {
        return Err(format!(
            "its reflog names {odd}, which is no commit name git writes"
        ));
    }
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["rev-list"];
    args.extend(oids.iter().map(String::as_str));
    args.extend(["--not", "refs/remotes/origin/main", head]);
    let lacking = read(git, checkout, &args);
    if !lacking.ok() {
        return Err(format!(
            "git could not say which commits only its reflogs hold: {}",
            lacking.err
        ));
    }
    let lacking: std::collections::BTreeSet<&str> = lacking.out.lines().map(str::trim).collect();
    let only: Vec<&str> = oids
        .iter()
        .map(String::as_str)
        .filter(|o| lacking.contains(o))
        .collect();
    if only.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["merge-base", "--independent"];
    args.extend(only.iter().copied());
    let tips = read(git, checkout, &args);
    if !tips.ok() {
        return Err(format!(
            "git could not reduce its reflogs' commits: {}",
            tips.err
        ));
    }
    let mut tips: Vec<String> = tips
        .out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    tips.sort();
    if tips.len() > MAX_TIPS {
        return Err(format!(
            "its reflogs hold {} commits nothing else does, more than a snapshot takes \
             ({MAX_TIPS})",
            tips.len()
        ));
    }
    Ok(tips)
}

/// Pin what `wt`, a linked worktree of `checkout` the pinned rule admitted (`admitted`), holds
/// (the module doc): its snapshot, its HEAD's orphan pin, its ignored and large files' local
/// commit and its submodules' commits the witness lacks, read after the file clock `since` this
/// takes first, with git's runs through `git`. Writes objects and refs of `checkout` (and of its
/// module witnesses) only: nothing of the worktree, its index or its entry. `quiet` is what its
/// judgment held it to; `free` is its volume's free bytes as read just before, `None` unread;
/// `budget` is what the step's snapshots may still read.
///
/// # Errors
/// Why it stays, nothing of it pinned or some of it pinned (a pin that stands is harmless: the
/// next pass makes the same).
pub fn pin(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    admitted: &Admitted,
    quiet: Duration,
    free: Option<u64>,
    budget: u64,
) -> Result<Pins, String> {
    let since = retired::file_clock_now();
    let name = ref_name(wt);
    let head = admitted.head.as_str();
    if !oid_shaped(head) {
        return Err("it has no HEAD commit to pin".to_owned());
    }
    let now_head =
        read(git, wt, &["rev-parse", "--verify", "HEAD^{commit}"]).line("read its HEAD")?;
    if now_head != head {
        return Err(format!(
            "its HEAD moved to {} after it was judged",
            short(&now_head)
        ));
    }
    let entry = read(git, wt, &["rev-parse", "--absolute-git-dir"]).line("name its git entry")?;
    let (untracked, ignored) = rows(git, wt)?;
    let pinnable: Vec<String> = ignored
        .into_iter()
        .filter(|rel| !retired::cache_tagged_row(wt, rel))
        .collect();
    let mut read_rels = untracked;
    read_rels.extend(pinnable.iter().cloned());
    read_rels.extend(changed(git, wt)?);
    let weighed = weigh(wt, &read_rels, git.until())?;
    if weighed.bytes > budget {
        return Err(format!(
            "this step's snapshots read their share ({} MiB) already, so it is pinned at a later \
             step",
            STEP_BYTES >> 20
        ));
    }
    let need = weighed.bytes.saturating_mul(2).saturating_add(ROOM);
    match free {
        Some(free) if free >= need => {}
        Some(free) => {
            return Err(format!(
                "its volume has {} MiB free, less than its snapshot leaves ({} MiB), so it stays",
                free >> 20,
                need >> 20
            ));
        }
        None => return Err("its volume's free space could not be read, so it stays".to_owned()),
    }
    unfiltered(git, wt, &weighed.files)?;
    let folder = aterm_tempfile::Builder::new()
        .prefix("aterm-pin-")
        .tempdir()
        .map_err(|e| format!("a private index folder could not be made: {e}"))?;
    let index = folder.path().join("index");
    let own_index = Path::new(&entry).join("index");
    match std::fs::copy(&own_index, &index) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            run(git, wt, &["read-tree", head], Some(&index), &[])
                .line("read its HEAD into a private index")?;
        }
        Err(e) => return Err(format!("its index could not be copied: {e}")),
    }
    let staged = run(git, wt, &["write-tree"], Some(&index), &[])
        .line("write its index as a tree (a path is unmerged)")?;
    let caches: Vec<String> = weighed
        .caches
        .iter()
        .map(|r| format!(":(exclude,literal){r}"))
        .collect();
    let big: Vec<String> = weighed
        .big
        .iter()
        .map(|r| format!(":(exclude,literal){r}"))
        .collect();
    let mut add = vec!["add", "-A", "--", "."];
    add.extend(caches.iter().chain(&big).map(String::as_str));
    run(git, wt, &add, Some(&index), &[]).line("read its files into a private index")?;
    let tree =
        run(git, wt, &["write-tree"], Some(&index), &[]).line("write its files as a tree")?;
    let head_tree = read(
        git,
        wt,
        &["rev-parse", "--verify", &format!("{head}^{{tree}}")],
    )
    .line("read its HEAD's tree")?;
    no_new_gitlink(git, checkout, &head_tree, &tree)?;
    let tips = reflog_tips(git, checkout, wt, head, admitted.branch.as_deref())?;
    let at = read(git, wt, &["log", "-1", "--format=%ct", head]).line("date its HEAD")?;
    let date = format!("@{at} +0000");
    let who = [
        ("GIT_AUTHOR_NAME", "aterm"),
        ("GIT_AUTHOR_EMAIL", "aterm@localhost.invalid"),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_NAME", "aterm"),
        ("GIT_COMMITTER_EMAIL", "aterm@localhost.invalid"),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    let commit = |tree: &str, parents: &[&str], message: &str| -> Result<String, String> {
        let mut args = vec!["commit-tree", "--no-gpg-sign", tree];
        for &parent in parents {
            args.extend(["-p", parent]);
        }
        args.extend(["-m", message]);
        run(git, wt, &args, None, &who).line("write a snapshot commit")
    };
    let mut roots: Vec<(String, String)> = Vec::new();
    let mut owed: Vec<[String; 2]> = Vec::new();
    let index_apart = staged != head_tree && staged != tree;
    let snapshot = if tree != head_tree || index_apart || !tips.is_empty() {
        let mut parents = vec![head.to_owned()];
        if index_apart {
            parents.push(commit(
                &staged,
                &[head],
                &format!("aterm: the index of {} at {}", wt.display(), short(head)),
            )?);
        }
        parents.extend(tips.iter().cloned());
        let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
        let snapshot = commit(
            &tree,
            &parents,
            &format!(
                "aterm: the edits of {} at {}\n\nIts session was gone; the disk watch kept what \
                 it held here before the worktree went (DC-02b).",
                wt.display(),
                short(head)
            ),
        )?;
        let pin = format!("refs/snapshots/{name}/{}", short(&snapshot));
        create_ref(git, checkout, &[], &pin, &snapshot)?;
        roots.push((pin.clone(), snapshot.clone()));
        owed.push([pin, snapshot.clone()]);
        Some(snapshot)
    } else {
        let merged = read(
            git,
            checkout,
            &[
                "merge-base",
                "--is-ancestor",
                head,
                "refs/remotes/origin/main",
            ],
        );
        match merged.code {
            Some(0) => {}
            Some(1) => {
                let pin = format!("refs/orphans/{name}/{}", short(head));
                create_ref(git, checkout, &[], &pin, head)?;
                roots.push((pin.clone(), head.to_owned()));
                owed.push([pin, head.to_owned()]);
            }
            _ => {
                return Err(format!(
                    "git could not say whether origin/main holds its HEAD: {}",
                    merged.err
                ));
            }
        }
        None
    };
    let mut local = Vec::new();
    let mut held_tree = tree;
    if !pinnable.is_empty() || !weighed.big.is_empty() {
        let literal: Vec<String> = pinnable
            .iter()
            .chain(&weighed.big)
            .map(|r| format!(":(literal){}", r.trim_end_matches('/')))
            .collect();
        let mut args = vec!["add", "-f", "--"];
        args.extend(literal.iter().chain(&caches).map(String::as_str));
        run(git, wt, &args, Some(&index), &[]).line("read its ignored and large files")?;
        let kept = run(git, wt, &["write-tree"], Some(&index), &[])
            .line("write its ignored and large files as a tree")?;
        no_new_gitlink(git, checkout, &head_tree, &kept)?;
        let over = snapshot.as_deref().unwrap_or(head);
        let held = commit(
            &kept,
            &[over],
            &format!(
                "aterm: the ignored and large files of {} at {}\n\nNever pushed: they stay on \
                 this machine.",
                wt.display(),
                short(head)
            ),
        )?;
        let pin = format!("refs/snapshots-local/{name}/{}", short(&held));
        create_ref(git, checkout, &[], &pin, &held)?;
        roots.push((pin.clone(), held.clone()));
        local.push((pin, held));
        held_tree = kept;
    }
    drop(folder);
    let mut pushes = Vec::new();
    if !owed.is_empty() {
        pushes.push(Push {
            repo: checkout.to_path_buf(),
            refs: owed,
        });
    }
    pushes.extend(pin_modules(git, checkout, wt, &name)?);
    Ok(Pins {
        pinned: Pinned {
            roots,
            ignored: pinnable,
            tree: held_tree,
            since,
            quiet,
        },
        pushes,
        local,
        bytes: weighed.bytes,
    })
}

/// Copy each commit a private module store of `wt` holds that its module witness in `checkout`
/// lacks into that witness (`retired::unrooted_module_roots`), its objects alone (a fetch that
/// writes no ref and no `FETCH_HEAD`), then root it there on
/// `refs/snapshots/<name>/<12 hex digits>` once the witness holds it: the pushes they owe the
/// witness's origin.
fn pin_modules(git: &dyn Git, checkout: &Path, wt: &Path, name: &str) -> Result<Vec<Push>, String> {
    let mut pushes = Vec::new();
    for unrooted in retired::unrooted_module_roots(git, checkout, wt)? {
        let git_dir = format!("--git-dir={}", arg(&unrooted.witness)?);
        let store = arg(&unrooted.store)?;
        let mut refs = Vec::new();
        for root in &unrooted.roots {
            if !oid_shaped(root) {
                return Err(format!(
                    "its submodule store names {root}, no commit git writes"
                ));
            }
            let pin = format!("refs/snapshots/{name}/{}", short(root));
            let had = read(
                git,
                &unrooted.witness,
                &[&git_dir, "rev-parse", "--verify", "--quiet", &pin],
            );
            if !(had.ok() && had.out.trim() == root) {
                read(
                    git,
                    &unrooted.witness,
                    &[
                        git_dir.as_str(),
                        "-c",
                        "protocol.version=2",
                        "-c",
                        "protocol.allow=never",
                        "-c",
                        "protocol.file.allow=always",
                        "-c",
                        "uploadpack.allowAnySHA1InWant=true",
                        "-c",
                        "fetch.writeCommitGraph=false",
                        "fetch",
                        "--quiet",
                        "--no-write-fetch-head",
                        "--no-tags",
                        "--recurse-submodules=no",
                        store,
                        root,
                    ],
                )
                .line("copy a submodule commit into the checkout's module store")?;
                let commit = format!("{root}^{{commit}}");
                read(
                    git,
                    &unrooted.witness,
                    &[git_dir.as_str(), "cat-file", "-e", &commit],
                )
                .line("find the submodule commit it copied")?;
                create_ref(git, &unrooted.witness, &[git_dir.as_str()], &pin, root)?;
            }
            refs.push([pin, root.clone()]);
        }
        pushes.push(Push {
            repo: unrooted.witness,
            refs,
        });
    }
    Ok(pushes)
}

/// Where `repo`'s origin is: a GitHub repository's `owner/name`, or a path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    GitHub(String),
    Local,
}

/// The `--git-dir` a git run in `repo` takes: one for a bare repository (a module witness),
/// none for a checkout.
fn repo_prefix(repo: &Path) -> Result<Vec<String>, String> {
    let bare = repo.file_name() != Some(".git".as_ref()) && !repo.join(".git").exists();
    Ok(if bare {
        vec![format!("--git-dir={}", arg(repo)?)]
    } else {
        Vec::new()
    })
}

/// The one URL `repo`'s `origin` fetches from and pushes to, and what it is.
fn origin_of(git: &dyn Git, repo: &Path) -> Result<Origin, String> {
    let prefix = repo_prefix(repo)?;
    let mut urls = Vec::new();
    for push in [false, true] {
        let mut args: Vec<&str> = prefix.iter().map(String::as_str).collect();
        args.extend(["remote", "get-url", "--all"]);
        if push {
            args.push("--push");
        }
        args.push("origin");
        let got = read(git, repo, &args);
        if !got.ok() {
            return Err(format!("it has no origin to push to: {}", got.err));
        }
        let lines: Vec<String> = got
            .out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        if lines.len() != 1 {
            return Err("its origin names more than one URL".to_owned());
        }
        urls.extend(lines);
    }
    if urls[0] != urls[1] {
        return Err("its origin pushes elsewhere than it fetches from".to_owned());
    }
    kind(&urls[0]).ok_or_else(|| format!("its origin {} is no URL this pushes to", urls[0]))
}

/// What `url` is: a GitHub repository (`https://github.com/o/n`, `git@github.com:o/n`,
/// `ssh://git@github.com/o/n`, each with or without `.git`) or a local path.
fn kind(url: &str) -> Option<Origin> {
    for lead in [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ] {
        if let Some(rest) = url.strip_prefix(lead) {
            let rest = rest.trim_end_matches('/');
            let rest = rest.strip_suffix(".git").unwrap_or(rest);
            let parts: Vec<&str> = rest.split('/').collect();
            let fine = |p: &str| {
                !p.is_empty()
                    && !p.starts_with(['-', '.'])
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            };
            return (parts.len() == 2 && parts.iter().all(|p| fine(p)))
                .then(|| Origin::GitHub(rest.to_ascii_lowercase()));
        }
    }
    let local = !url.is_empty()
        && !url.starts_with('-')
        && !url.contains(':')
        && !url.chars().any(char::is_control);
    local.then_some(Origin::Local)
}

/// `aterm agh <args>`, its output when it ran.
pub type Agh<'a> = &'a dyn Fn(&[&str]) -> Result<Output, String>;

/// `aterm agh` run by `until`, where this program is aterm: the GitHub arm's runner.
#[must_use]
pub fn agh_until(until: Instant) -> impl Fn(&[&str]) -> Result<Output, String> {
    move |args: &[&str]| {
        let exe = crate::own_exe()?;
        if exe.file_stem() != Some("aterm".as_ref()) {
            return Err("this program has no agh verb to push with".to_owned());
        }
        let mut cmd = aterm_uds::spawnfd::command(&exe);
        cmd.arg("agh").args(args);
        aterm_uds::deadline::output_with_input_until(
            &mut cmd,
            aterm_uds::secret::Secret::new(&[]),
            until,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        )
        .map_err(|e| format!("agh did not start or did not finish: {e}"))
    }
}

/// What one try of an owed push came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tried {
    /// Every ref stands at origin, read back as the commit it names here.
    Pushed,
    /// It is owed still, for why.
    Owed(String),
    /// A ref of one of its names at origin names another commit: it is tried no more.
    Parked(String),
}

/// Try `owed`'s push: its refs to its repository's origin, each under its own name, never by
/// force and only where none of that name stands there, read back after. A GitHub write whose
/// outcome agh could not tell leaves its intent in `owed.intent`, reconciled first at the next
/// try.
#[must_use]
pub fn push(git: &dyn Git, agh: Agh<'_>, owed: &mut Owed) -> Tried {
    let refs = &owed.push.refs;
    if refs.is_empty() {
        return Tried::Pushed;
    }
    for [name, oid] in refs {
        if !name.starts_with("refs/") || !oid_shaped(oid) {
            return Tried::Parked(format!("{name} is no ref and commit this pushes"));
        }
    }
    match origin_of(git, &owed.push.repo) {
        Ok(Origin::Local) => push_local(git, &owed.push),
        Ok(Origin::GitHub(slug)) => push_github(agh, &slug, owed),
        Err(why) => Tried::Owed(why),
    }
}

/// Which of `refs` to push, given what origin's refs read as (`there`): each it lacks, or
/// parked when one names another commit there.
fn to_send(
    refs: &[[String; 2]],
    there: &BTreeMap<String, String>,
) -> Result<Vec<[String; 2]>, String> {
    let mut send = Vec::new();
    for [name, oid] in refs {
        match there.get(name) {
            None => send.push([name.clone(), oid.clone()]),
            Some(at) if at == oid => {}
            Some(at) => {
                return Err(format!(
                    "origin's {name} names {}, not {}, so it is pushed no more",
                    short(at),
                    short(oid)
                ));
            }
        }
    }
    Ok(send)
}

/// Whether every ref of `refs` reads at origin (`there`) as the commit it names here.
fn all_there(refs: &[[String; 2]], there: &BTreeMap<String, String>) -> bool {
    refs.iter()
        .all(|[name, oid]| there.get(name).is_some_and(|at| at == oid))
}

/// `push` to a local origin by `git push`, each ref origin lacks as `<commit>:<ref>`.
fn push_local(git: &dyn Git, push: &Push) -> Tried {
    let repo = &push.repo;
    let prefix = match repo_prefix(repo) {
        Ok(prefix) => prefix,
        Err(why) => return Tried::Owed(why),
    };
    let lead: Vec<&str> = prefix
        .iter()
        .map(String::as_str)
        .chain([
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
        ])
        .collect();
    let names: Vec<&str> = push.refs.iter().map(|[name, _]| name.as_str()).collect();
    let origin_refs = || -> Result<BTreeMap<String, String>, String> {
        let mut args = lead.clone();
        args.extend(["ls-remote", "--refs", "origin"]);
        args.extend(names.iter().copied());
        let listed = read(git, repo, &args);
        if !listed.ok() {
            return Err(format!(
                "git could not read its origin's refs: {}",
                listed.err
            ));
        }
        Ok(listed
            .out
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .filter(|(_, name)| names.contains(name))
            .map(|(oid, name)| (name.to_owned(), oid.to_owned()))
            .collect())
    };
    let before = match origin_refs() {
        Ok(before) => before,
        Err(why) => return Tried::Owed(why),
    };
    let send = match to_send(&push.refs, &before) {
        Ok(send) => send,
        Err(why) => return Tried::Parked(why),
    };
    if !send.is_empty() {
        let specs: Vec<String> = send
            .iter()
            .map(|[name, oid]| format!("{oid}:{name}"))
            .collect();
        let mut args = lead.clone();
        args.extend([
            "-c",
            "push.followTags=false",
            "-c",
            "push.gpgSign=false",
            "push",
            "--porcelain",
            "--no-verify",
            "--no-follow-tags",
            "--no-recurse-submodules",
            "origin",
        ]);
        args.extend(specs.iter().map(String::as_str));
        let pushed = read(git, repo, &args);
        if !pushed.ok() {
            return Tried::Owed(format!("git push to its origin failed: {}", pushed.err));
        }
    }
    match origin_refs() {
        Ok(after) if all_there(&push.refs, &after) => Tried::Pushed,
        Ok(_) => Tried::Owed("its origin's refs did not read back as pushed".to_owned()),
        Err(why) => Tried::Owed(why),
    }
}

/// agh's one machine diagnostic of a failed command: the fields a caller decides by.
#[derive(serde::Deserialize)]
struct Refusal {
    v: u8,
    exit: u8,
    kind: String,
    sub: Option<String>,
    side_effect: String,
    repo: Option<String>,
    intent: Option<String>,
}

/// The machine diagnostic in `out`'s stderr, never its human words: `None` unless it holds
/// exactly one, for this exit.
fn refusal(out: &Output) -> Option<Refusal> {
    #[derive(serde::Deserialize)]
    struct Diagnostic {
        error: Refusal,
    }
    let mut records = out
        .stderr
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| line.starts_with(b"{"))
        .map(aterm_json::from_slice::<Diagnostic>);
    let error = records.next()?.ok()?.error;
    if records.next().is_some() {
        return None;
    }
    (error.v == 1 && out.status.code() == Some(i32::from(error.exit))).then_some(error)
}

/// Whether `id` is an intent id agh writes (no option, no separator).
fn intent_shaped(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// The human words of agh's `out`.
fn agh_words(out: &Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    let words: Vec<&str> = err
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('{'))
        .collect();
    format!(
        "agh exited {}{}",
        out.status
            .code()
            .map_or_else(|| "on a signal".to_owned(), |c| c.to_string()),
        if words.is_empty() {
            String::new()
        } else {
            format!(": {}", words.join("; "))
        }
    )
}

/// What each of `refs` reads as on GitHub's `slug` (`agh repo refs`, one exact ref a read): a
/// ref absent there is absent from the map.
fn github_refs(
    agh: Agh<'_>,
    slug: &str,
    refs: &[[String; 2]],
) -> Result<BTreeMap<String, String>, String> {
    let mut there = BTreeMap::new();
    for [name, _] in refs {
        let out = agh(&["repo", "refs", slug, "--ref", name])?;
        if !out.status.success() {
            let words = agh_words(&out);
            return Err(format!("its origin's {name} could not be read: {words}"));
        }
        let value: aterm_json::Value = aterm_json::from_slice(&out.stdout)
            .map_err(|e| format!("agh repo refs answered no JSON: {e}"))?;
        let value = value.get("answer").unwrap_or(&value);
        let rows = value
            .as_array()
            .ok_or("agh repo refs answered no ref array")?;
        let mut found = rows
            .iter()
            .filter(|row| row["ref"].as_str() == Some(name.as_str()));
        if let Some(row) = found.next() {
            let oid = row["oid"].as_str().filter(|o| oid_shaped(o));
            match (oid, found.next()) {
                (Some(oid), None) => {
                    there.insert(name.clone(), oid.to_owned());
                }
                _ => return Err(format!("agh read {name} as no one commit")),
            }
        }
    }
    Ok(there)
}

/// What an intent an earlier write left open came to.
enum Reconciled {
    /// Its refs read as written: it landed, and its intent is retracted.
    Landed,
    /// None of its refs stands: it sent nothing, and its intent is retracted (or was closed).
    Unsent,
    /// It is open and its refs read as neither: it stays for a later try.
    Open,
}

/// Reconcile the intent `id` an earlier write of `refs` to `slug` left open: once its refs read
/// as written or as absent, retract it (agh's own order: `intent show`, a look at GitHub, then
/// `intent retract`), so no other write to `slug` stays blocked behind it.
fn reconcile(
    agh: Agh<'_>,
    slug: &str,
    id: &str,
    refs: &[[String; 2]],
) -> Result<Reconciled, String> {
    if !intent_shaped(id) {
        return Err(format!("{id} is no intent agh writes"));
    }
    let shown = agh(&["intent", "show", slug, id, "--format", "json"])?;
    if shown.status.code() == Some(6) {
        return Ok(Reconciled::Unsent);
    }
    if !shown.status.success() {
        let words = agh_words(&shown);
        return Err(format!("its intent {id} could not be read: {words}"));
    }
    #[derive(serde::Deserialize)]
    struct Intent {
        id: String,
        repo: String,
        op: String,
        open: bool,
    }
    let doc: Intent = aterm_json::from_slice(&shown.stdout)
        .map_err(|e| format!("agh intent show answered no intent: {e}"))?;
    if doc.id != id || doc.repo != slug || doc.op != "ref.update-atomic" {
        return Err(format!("agh's intent {id} is not the write this made"));
    }
    if !doc.open {
        return Ok(Reconciled::Unsent);
    }
    let there = github_refs(agh, slug, refs)?;
    let outcome = if all_there(refs, &there) {
        Reconciled::Landed
    } else if refs.iter().all(|[name, _]| !there.contains_key(name)) {
        Reconciled::Unsent
    } else {
        return Ok(Reconciled::Open);
    };
    let retracted = agh(&["intent", "retract", slug, id, "--confirm", id])?;
    if !retracted.status.success() {
        let words = agh_words(&retracted);
        return Err(format!("its intent {id} could not be retracted: {words}"));
    }
    Ok(outcome)
}

/// `owed`'s push to GitHub's `slug` through `aterm agh repo push`, each ref GitHub lacks a new
/// ref (from the all-zero commit), read back after.
fn push_github(agh: Agh<'_>, slug: &str, owed: &mut Owed) -> Tried {
    let refs = owed.push.refs.clone();
    if let Some(id) = owed.intent.clone() {
        match reconcile(agh, slug, &id, &refs) {
            Ok(Reconciled::Landed) => {
                owed.intent = None;
                return Tried::Pushed;
            }
            Ok(Reconciled::Unsent) => owed.intent = None,
            Ok(Reconciled::Open) => {
                return Tried::Owed(format!("its earlier write's intent {id} is still open"));
            }
            Err(why) => return Tried::Owed(why),
        }
    }
    let before = match github_refs(agh, slug, &refs) {
        Ok(before) => before,
        Err(why) => return Tried::Owed(why),
    };
    let send = match to_send(&refs, &before) {
        Ok(send) => send,
        Err(why) => return Tried::Parked(why),
    };
    if send.is_empty() {
        return Tried::Pushed;
    }
    /// One ref `agh repo push` sets, as its `--input` names it.
    #[derive(serde::Serialize)]
    struct Row<'a> {
        #[serde(rename = "ref")]
        name: &'a str,
        before: String,
        after: &'a str,
    }
    let rows: Vec<Row<'_>> = send
        .iter()
        .map(|[name, oid]| Row {
            name,
            before: "0".repeat(oid.len()),
            after: oid,
        })
        .collect();
    let staged = aterm_tempfile::tempdir()
        .map_err(|e| format!("its push could not be staged: {e}"))
        .and_then(|folder| {
            let input = folder.path().join("refs.json");
            let body = aterm_json::to_string(&rows).map_err(|e| e.to_string())?;
            std::fs::write(&input, body)
                .map_err(|e| format!("its push could not be staged: {e}"))?;
            Ok((folder, input))
        });
    let (_folder, input) = match staged {
        Ok(staged) => staged,
        Err(why) => return Tried::Owed(why),
    };
    let (Ok(repo), Ok(input)) = (arg(&owed.push.repo), arg(&input)) else {
        return Tried::Owed("its repository or its push is no path agh takes".to_owned());
    };
    let out = match agh(&["repo", "push", slug, repo, "--input", input]) {
        Ok(out) => out,
        Err(why) => return Tried::Owed(why),
    };
    let read_back = |why: String, parked: bool| match github_refs(agh, slug, &refs) {
        Ok(after) if all_there(&refs, &after) => Tried::Pushed,
        Ok(_) if parked => Tried::Parked(why),
        Ok(_) => Tried::Owed(why),
        Err(read) => Tried::Owed(format!("{why}; {read}")),
    };
    if out.status.success() {
        return read_back(
            "its origin's refs did not read back as pushed".to_owned(),
            false,
        );
    }
    let words = agh_words(&out);
    let why = format!("agh repo push {slug} failed: {words}");
    if out.status.code() == Some(9) {
        return read_back(why, true);
    }
    let Some(error) = refusal(&out) else {
        return Tried::Owed(why);
    };
    let ours = error.repo.as_deref().is_none_or(|named| named == slug);
    if error.side_effect == "none" || !ours {
        // Sent nothing (another write's open intent refused it, or it could not run): the same
        // write goes again at a later try.
        return Tried::Owed(why);
    }
    let unknown = error.exit == 13
        && error.kind == "outcome_unknown"
        && error.sub.as_deref() != Some("intent_open");
    match error.intent {
        Some(id) if unknown => match reconcile(agh, slug, &id, &refs) {
            Ok(Reconciled::Landed) => Tried::Pushed,
            Ok(Reconciled::Unsent) => Tried::Owed(why),
            Ok(Reconciled::Open) => {
                owed.intent = Some(id);
                Tried::Owed(format!(
                    "{why}; its intent stays open until its refs read back"
                ))
            }
            Err(more) => {
                owed.intent = Some(id);
                Tried::Owed(format!("{why}; {more}"))
            }
        },
        _ => Tried::Owed(why),
    }
}

/// One push owed to an origin.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Owed {
    /// What is owed.
    pub push: Push,
    /// The worktree whose pins they are.
    pub worktree: PathBuf,
    /// When it was first owed, in seconds since the Unix epoch.
    pub since: u64,
    /// Why its last try failed, empty before its first.
    #[serde(default)]
    pub why: String,
    /// The agh intent its last GitHub write left open, its outcome unknown: reconciled before
    /// another write.
    #[serde(default)]
    pub intent: Option<String>,
    /// How many of its tries failed.
    #[serde(default)]
    pub tries: u32,
    /// The second of the Unix epoch before which it is not tried again.
    #[serde(default)]
    pub next: u64,
    /// Whether its worktree went: until then a worktree that stands holds what its pins do, and
    /// its push waits.
    #[serde(default)]
    pub removed: bool,
}

/// The pushes the file at `path` owes. A missing file owes none.
///
/// # Errors
/// Why it could not be read: it is then never written over.
pub fn load(path: &Path) -> Result<Vec<Owed>, String> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(OWED_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{} could not be read: {e}", path.display()))?;
    if bytes.len() > OWED_BYTES {
        return Err(format!(
            "{} is larger than it is ever written",
            path.display()
        ));
    }
    aterm_json::from_slice(&bytes)
        .map_err(|e| format!("{} is not what it writes: {e}", path.display()))
}

/// Write `owed` to `path`, the newest [`MAX_OWED`] of them: the older ones it drops.
///
/// # Errors
/// Why it could not be written.
pub fn save(path: &Path, owed: &[Owed]) -> Result<Vec<Owed>, String> {
    let cut = owed.len().saturating_sub(MAX_OWED);
    let (dropped, keep) = owed.split_at(cut);
    let text = aterm_json::to_string(keep).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("{} could not be made: {e}", parent.display()))?;
    }
    crate::harness::atomic_write::write_atomic_ordered(path, &text)
        .map_err(|e| format!("{} could not be written: {e}", path.display()))?;
    Ok(dropped.to_vec())
}

/// Add `pushes` of `worktree` at `at` (seconds since the epoch) to the file at `path`, before
/// the worktree goes: the pushes the file then had no room for.
///
/// # Errors
/// Why they could not be recorded: the worktree then stays.
pub fn owe(path: &Path, worktree: &Path, pushes: &[Push], at: u64) -> Result<Vec<Owed>, String> {
    if pushes.iter().all(|p| p.refs.is_empty()) {
        return Ok(Vec::new());
    }
    let mut owed = load(path)?;
    for push in pushes.iter().filter(|p| !p.refs.is_empty()) {
        if !owed.iter().any(|o| o.push == *push) {
            owed.push(Owed {
                push: push.clone(),
                worktree: worktree.to_path_buf(),
                since: at,
                why: String::new(),
                intent: None,
                tries: 0,
                next: 0,
                removed: false,
            });
        }
    }
    save(path, &owed)
}

/// Drop from the file at `path` the pushes of `worktree` among `pushes` whose worktree has not
/// gone: its removal never began, so what they hold stands in it still.
///
/// # Errors
/// Why the file could not be read or written.
pub fn disown(path: &Path, worktree: &Path, pushes: &[Push]) -> Result<(), String> {
    let owed = load(path)?;
    let before = owed.len();
    let owed: Vec<Owed> = owed
        .into_iter()
        .filter(|o| o.removed || o.worktree != worktree || !pushes.contains(&o.push))
        .collect();
    if owed.len() == before {
        return Ok(());
    }
    save(path, &owed).map(drop)
}

/// Mark in the file at `path` every push of `worktree` as its worktree's gone: each is tried
/// from now on.
///
/// # Errors
/// Why the file could not be read or written.
pub fn went(path: &Path, worktree: &Path) -> Result<(), String> {
    let mut owed = load(path)?;
    let mut changed = false;
    for o in &mut owed {
        if o.worktree == worktree && !o.removed {
            o.removed = true;
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    save(path, &owed).map(drop)
}

/// What one round of owed pushes did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Round {
    /// The pushes that went.
    pub pushed: Vec<Owed>,
    /// The pushes still owed, each with why.
    pub owed: Vec<Owed>,
    /// The owed ones whose why changed at this round's try (a record of each, not of each try).
    pub changed: Vec<Owed>,
    /// The pushes origin refuses for good (a ref of one of their names there names another
    /// commit), each with why: tried no more.
    pub parked: Vec<Owed>,
    /// The pushes the owed file had no room for.
    pub dropped: Vec<Owed>,
    /// Why the owed file could not be read or written, when it could not.
    pub unrecorded: Option<String>,
}

/// Try each push the file at `path` owes that is due at `now` (seconds since the epoch) and
/// whose worktree went or no longer stands, by `until`: what went or is parked leaves it, and
/// what failed waits twice as long as before, up to a week, before its next try.
#[must_use]
pub fn push_owed(git: &dyn Git, agh: Agh<'_>, path: &Path, now: u64, until: Instant) -> Round {
    let owed = match load(path) {
        Ok(owed) => owed,
        Err(why) => {
            return Round {
                unrecorded: Some(why),
                ..Round::default()
            };
        }
    };
    if owed.is_empty() {
        return Round::default();
    }
    let mut round = Round::default();
    for mut entry in owed {
        let waits = !entry.removed && std::fs::symlink_metadata(&entry.worktree).is_ok();
        if waits || entry.next > now || Instant::now() >= until {
            round.owed.push(entry);
            continue;
        }
        match push(git, agh, &mut entry) {
            Tried::Pushed => round.pushed.push(entry),
            Tried::Parked(why) => {
                entry.why = why;
                round.parked.push(entry);
            }
            Tried::Owed(why) => {
                entry.tries = entry.tries.saturating_add(1);
                let wait = BACKOFF_FIRST
                    .saturating_mul(1u64 << entry.tries.saturating_sub(1).min(16))
                    .min(BACKOFF_MOST);
                entry.next = now.saturating_add(wait);
                if entry.why != why {
                    entry.why = why;
                    round.changed.push(entry.clone());
                }
                round.owed.push(entry);
            }
        }
    }
    match save(path, &round.owed) {
        Ok(dropped) => {
            round.owed = round.owed.split_off(dropped.len());
            round.dropped = dropped;
        }
        Err(why) => round.unrecorded = Some(why),
    }
    round
}

/// Seconds since the Unix epoch at `now`.
#[must_use]
pub fn epoch_secs(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
