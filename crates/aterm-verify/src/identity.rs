// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a run is verifying, captured once and re-checked while it runs.
//!
//! WHY (2026-09-13). A `--fast` run took 14 h in the owner's live checkout, and
//! that checkout was pulled FOUR times while the gate was running. Whatever it
//! printed at the end was about no single tree: the build saw one HEAD, the
//! tests another, the doctests a third. The ladder had no way to notice, so
//! the verdict read exactly like one about the commit the caller started it on.
//!
//! Two identities close that:
//!
//!  * [`SourceIdentity`] — HEAD plus the working-tree state of every path that
//!    differs from it (tracked changes, untracked non-ignored files, edits an
//!    assume-unchanged or skip-worktree flag hides from git, and each
//!    submodule's checkout — its commit and, recursively, its own such paths),
//!    each as a blob id and exec bit, a link target, a commit, or "absent". It is a statement
//!    about the CONTENT a compiler would read, so the same change reads the same
//!    whether it is staged or not, and in the caller's checkout or a snapshot
//!    synced from it ([`crate::snapshot`] compares exactly these values).
//!  * [`ToolchainIdentity`] — `(dev, ino, len, mtime)` of `targo`, `trustc`,
//!    `trustdoc` and `tippy`, plus `trustc -vV`'s commit hash. A re-seal or an
//!    atpkg update mid-run swaps the compiler under the run the same way a pull
//!    swaps the source.
//!
//! A [`Tripwire`] holds both. The ladder re-checks it before every stage that
//! touches a target dir and once more before the verdict; a mismatch makes the
//! run COULD NOT RUN and names what moved. It never reads as a FAIL of the tree:
//! nothing about the change was decided, and a verdict about a tree that no
//! longer exists is not a finding.
//!
//! A root that is not the top of a git checkout, and has no `.git` of its own,
//! has no source identity:
//! [`SourceIdentity::Unavailable`] prints nothing and checks nothing, exactly as
//! the gate behaved before (`gate_contract`'s synthetic repos are such roots).
//! A git checkout whose state cannot be read — or whose `.git` git cannot open
//! at all — is [`SourceIdentity::Unreadable`],
//! which is COULD NOT RUN before any stage — never the silent `Unavailable`.
//! Every git call runs with `--no-optional-locks`, so checking never rewrites
//! `.git/index` under a caller who is using it.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The gate's own state directory inside a checkout it runs in (the snapshot
/// lock, the lane trash). Never part of a source identity, and never cleaned.
pub const GATE_STATE_DIR: &str = ".aterm-verify";

/// The regular files this process's own stdout and stderr are open on, as
/// `(dev, ino)` — see [`claim_own_output`]. Empty (or unset) means "exclude
/// nothing", which is exactly how the gate behaved before.
static OWN_OUTPUT: OnceLock<Vec<(u64, u64)>> = OnceLock::new();

/// Record where this run's own output is going, so a gate log written INSIDE
/// the checkout cannot read as the tree moving under the run.
///
/// WHY (2026-09-17), and it cost a whole `--fast` run. `bash tools/verify.sh
/// --fast > gate.log` inside the worktree makes `gate.log` an untracked,
/// non-ignored file — which is part of a [`TreeState`] by design, because an
/// untracked `.rs` file IS something a compiler reads. It then GROWS, one
/// ladder line at a time, for the length of the run. In place, every
/// [`Tripwire::check`] after the first therefore reports `source: moved paths:
/// gate.log` and every stage that would build something answers `not run` — a
/// run that prints thirty stage lines and decides nothing. Through the
/// snapshot the same file churns between the sync's capture and its copy, so
/// the sync exhausts its attempts and the run is COULD NOT RUN. Both failures
/// are caused by the gate's own output, and neither says so.
///
/// The gate can tell its own output from the tree's source exactly: it holds
/// the descriptors. `fstat` on stdout and stderr names the FILE behind a
/// redirect, and an untracked path with that `(dev, ino)` is this run's log,
/// not a source change. Only
/// UNTRACKED paths are ever excluded — a redirect that lands on a tracked file
/// is a real edit to a real source file and stays visible.
///
/// It cannot see through a `| tee gate.log`: there fd 1 is a pipe and the file
/// belongs to another process. That case is caught the other way round, by
/// [`tripped_label`], which names the remedy when everything that moved is
/// untracked.
///
/// Called once, from `main`, before anything reads the tree. Not calling it
/// leaves every path visible, which is the old behaviour.
pub fn claim_own_output() {
    let _ = OWN_OUTPUT.set(own_output_ids());
}

#[cfg(unix)]
fn own_output_ids() -> Vec<(u64, u64)> {
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;

    // `fstat` OF THE DESCRIPTOR, and not `stat("/dev/fd/<n>")`, which was the
    // first spelling and is WRONG on macOS: measured 2026-09-17, the fdesc
    // filesystem answers with its OWN `st_dev` (413786866) while the file's is
    // 16777232, so every comparison missed and the exclusion did nothing. The
    // inode alone matched — a pair of numbers that agrees by half is exactly
    // the kind of identity that reads as working.
    //
    // No `unsafe`: `try_clone_to_owned` DUPs the descriptor, the `File` owns
    // only the duplicate, and dropping it leaves the gate's own stdout open.
    let stat = |fd: std::os::fd::BorrowedFd<'_>| -> Option<(u64, u64)> {
        let owned = fd.try_clone_to_owned().ok()?;
        let m = std::fs::File::from(owned).metadata().ok()?;
        m.is_file().then(|| (m.dev(), m.ino()))
    };
    let mut ids = Vec::new();
    for id in [
        stat(std::io::stdout().as_fd()),
        stat(std::io::stderr().as_fd()),
    ]
    .into_iter()
    .flatten()
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

#[cfg(not(unix))]
fn own_output_ids() -> Vec<(u64, u64)> {
    Vec::new()
}

/// Is `full` a file this run's own output is being written to?
#[cfg(unix)]
fn is_own_output(full: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(ids) = OWN_OUTPUT.get().filter(|v| !v.is_empty()) else {
        return false;
    };
    std::fs::metadata(full).is_ok_and(|m| ids.contains(&(m.dev(), m.ino())))
}

#[cfg(not(unix))]
fn is_own_output(_full: &Path) -> bool {
    false
}

/// The `verify:` note for output the gate excluded from the source identity —
/// printed only when there was some, so a run that redirects nowhere near the
/// checkout reads exactly as it always did.
#[must_use]
pub fn own_output_note(paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    Some(format!(
        "verify: this run's own output is being written to {} inside the checkout — \
         excluded from the source identity, which a growing log would otherwise read as \
         the tree moving",
        name_some(&paths.iter().map(String::as_str).collect::<Vec<_>>())
    ))
}

/// Git's own environment, which would redirect every call below at a
/// different repository: a gate started from inside a git hook inherits
/// `GIT_DIR` and `GIT_INDEX_FILE`, and must still read the root it was given.
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

/// `git --no-optional-locks <args>` in `dir`, with the redirects removed.
#[must_use]
pub fn git(dir: &Path, path_env: &OsStr) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .env("PATH", path_env)
        .arg("--no-optional-locks")
        .stdin(Stdio::null());
    for k in GIT_REDIRECTS {
        c.env_remove(k);
    }
    c
}

/// Run `cmd`, returning stdout on exit 0.
fn stdout_bytes(cmd: &mut Command) -> Option<Vec<u8>> {
    let out = cmd.stderr(Stdio::null()).output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn stdout_line(cmd: &mut Command) -> Option<String> {
    stdout_bytes(cmd)
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Is `root` the TOP of a git checkout? A directory merely nested inside some
/// other repository is not: its identity would be that repository's.
#[must_use]
pub fn is_git_toplevel(root: &Path, path_env: &OsStr) -> bool {
    let Some(top) = stdout_line(git(root, path_env).args(["rev-parse", "--show-toplevel"])) else {
        return false;
    };
    match (std::fs::canonicalize(&top), std::fs::canonicalize(root)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Does `root` hold a `.git` entry of its own — a directory, a worktree's
/// gitdir file, or even a dangling link? Such a root is a git checkout whether
/// or not git can open it: a directory merely nested inside another repository
/// has none, and neither do `gate_contract`'s synthetic roots.
#[must_use]
pub fn has_git_entry(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join(".git")).is_ok()
}

/// Why a root with a `.git` of its own is not a checkout git could open.
#[must_use]
pub fn unopenable_reason(root: &Path) -> String {
    format!(
        "{} holds a .git but git could not open it (git missing from PATH, a dangling \
         worktree gitdir, or refused ownership)",
        root.display()
    )
}

/// One path's working-tree state, as a compiler would read it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathState {
    Absent,
    File {
        blob: String,
        exec: bool,
    },
    Symlink {
        target: PathBuf,
    },
    /// A directory that is the top of a git checkout of its own — a submodule
    /// checkout — at this commit (or `(unborn)`). Only its HEAD: what its
    /// working tree holds beyond HEAD is captured path by path, under
    /// `<path>/`, by [`TreeState::capture`].
    ///
    /// WHY (2026-09-24, when `vendor/astream` became a submodule). A gitlink
    /// used to read as [`PathState::Other`] — "a directory" — so two checkouts
    /// whose submodules sat at DIFFERENT commits, or held different edits,
    /// compared equal, and a snapshot whose submodule was never populated at
    /// all compared equal to a caller whose was: the sync's proof proved
    /// nothing about the source cargo actually reads from there.
    Gitlink {
        head: String,
    },
    /// A submodule the index records whose checkout is not there — never
    /// initialised, deinitialised, or its `.git` broken. A build of this tree
    /// cannot read that source, so it is NOT the tree HEAD names.
    Unpopulated,
    /// Anything else that is neither a file nor a link (a plain directory
    /// where a path was listed).
    Other,
}

impl PathState {
    fn render(&self) -> String {
        match self {
            PathState::Absent => "absent".to_string(),
            PathState::File { blob, exec } => {
                format!("{} {blob}", if *exec { "100755" } else { "100644" })
            }
            PathState::Symlink { target } => format!("120000 {}", target.display()),
            PathState::Gitlink { head } => format!("160000 {head}"),
            PathState::Unpopulated => "unpopulated".to_string(),
            PathState::Other => "other".to_string(),
        }
    }
}

/// HEAD plus every path whose working-tree content differs from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeState {
    /// The full commit id, or `(unborn)`.
    pub head: String,
    pub dirty: BTreeMap<String, PathState>,
}

impl TreeState {
    /// Capture `root`'s state. `None` when git cannot answer.
    ///
    /// SUBMODULES ARE PART OF THE TREE. Every gitlink the index records is
    /// either populated — and then its own state is captured the same way,
    /// recursively, each of its dirty paths entered as `<gitlink>/<path>`,
    /// and the gitlink itself entered as [`PathState::Gitlink`] whenever git
    /// lists it as differing from HEAD (another commit checked out, or dirty
    /// content) — or it is [`PathState::Unpopulated`]. A populated submodule
    /// exactly at its gitlink with nothing dirty adds nothing, so a clean
    /// checkout with submodules still has no dirty state at all.
    #[must_use]
    pub fn capture(root: &Path, path_env: &OsStr) -> Option<Self> {
        let head = head_of(root, path_env);
        // Tracked paths whose working tree differs from HEAD — staged or not,
        // which is the point: the compiler reads the working tree.
        let tracked = if head == "(unborn)" {
            stdout_bytes(git(root, path_env).args(["ls-files", "-z"]))?
        } else {
            stdout_bytes(git(root, path_env).args([
                "diff",
                "HEAD",
                "--name-only",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--ignore-submodules=none",
            ]))?
        };
        let untracked = untracked_paths(root, path_env)?;
        let index = index_entries(root, path_env)?;
        let hidden = hidden_among(root, path_env, &index)?;
        let paths: Vec<String> = split_z(&tracked)
            .into_iter()
            .chain(untracked)
            .chain(hidden)
            .filter(|p| !is_gate_state(p))
            .collect();
        let mut dirty = path_states(root, path_env, &paths)?;
        for link in gitlinks_among(&index) {
            if is_gate_state(&link) {
                continue;
            }
            let full = root.join(&link);
            match std::fs::symlink_metadata(&full) {
                // Missing: git's diff lists it as deleted, which is the same
                // fact from the other side — the source is not there.
                Err(_) => {
                    dirty.insert(link, PathState::Unpopulated);
                }
                Ok(m) if m.is_dir() => {
                    if !is_git_toplevel(&full, path_env) {
                        dirty.insert(link, PathState::Unpopulated);
                        continue;
                    }
                    let sub = Self::capture(&full, path_env)?;
                    for (p, s) in sub.dirty {
                        dirty.insert(format!("{link}/{p}"), s);
                    }
                }
                // A file or link where a submodule belongs is already listed by
                // the diff, as what it is. Never followed: a link to an
                // ancestor would recurse forever.
                Ok(_) => {}
            }
        }
        Some(Self { head, dirty })
    }

    /// A short digest of the dirty state, via `git hash-object --stdin` — the
    /// same value for the same content in any checkout. `None` when clean.
    #[must_use]
    pub fn dirty_digest(&self, path_env: &OsStr) -> Option<String> {
        if self.dirty.is_empty() {
            return None;
        }
        let mut text = String::new();
        for (p, s) in &self.dirty {
            text.push_str(p);
            text.push('\0');
            text.push_str(&s.render());
            text.push('\n');
        }
        hash_stdin(path_env, text.as_bytes())
    }

    /// What moved from `self` to `now`, in words, or `None` if nothing did.
    #[must_use]
    pub fn moved_to(&self, now: &Self) -> Option<String> {
        if self == now {
            return None;
        }
        let mut parts = Vec::new();
        if self.head != now.head {
            parts.push(format!("HEAD {} -> {}", self.head, now.head));
        }
        let moved = self.moved_paths(now);
        if !moved.is_empty() {
            let moved: Vec<&str> = moved.iter().map(String::as_str).collect();
            parts.push(format!("moved paths: {}", name_some(&moved)));
        }
        Some(parts.join("; "))
    }

    /// Every path whose state differs between `self` and `now`, sorted. The
    /// names behind [`Self::moved_to`]'s `moved paths:` clause — which shows at
    /// most eight of them, and is a sentence rather than a list.
    #[must_use]
    pub fn moved_paths(&self, now: &Self) -> Vec<String> {
        self.dirty
            .keys()
            .chain(now.dirty.keys())
            .filter(|p| self.dirty.get(*p) != now.dirty.get(*p))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// At most eight names, then a count — a pull can move thousands of paths, and
/// the label is a ladder line.
fn name_some(paths: &[&str]) -> String {
    const SHOWN: usize = 8;
    let mut s = paths
        .iter()
        .take(SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        s.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    s
}

fn is_gate_state(path: &str) -> bool {
    path == GATE_STATE_DIR
        || path
            .strip_prefix(GATE_STATE_DIR)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn split_z(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

/// Tracked paths an index flag hides — assume-unchanged (a lowercase `ls-files
/// -v` tag) or skip-worktree (`S`) — whose working tree differs from their index
/// entry (never the gate's own state).
///
/// WHY (2026-09-13). `git status` and `git diff HEAD` take such a path's index
/// entry as its working tree, so an edited flagged file was in no listing: the
/// identity, the sync and the tripwire all skipped it, and a snapshot run built
/// HEAD's bytes with no `+dirty`. A compiler reads the working tree whatever the
/// index says. An unedited flagged file matches its entry and is not listed.
#[must_use]
pub fn flag_hidden_edits(root: &Path, path_env: &OsStr) -> Option<Vec<String>> {
    hidden_among(root, path_env, &index_entries(root, path_env)?)
}

/// One index entry, as `git ls-files -v -s` prints it.
struct IndexEntry {
    /// The `-v` tag: lowercase for assume-unchanged, `S` for skip-worktree.
    tag: char,
    mode: String,
    blob: String,
    path: String,
}

/// Every index entry of `root`, one `git ls-files -v -s -z` — the one listing
/// both the flag census and the submodule census read.
fn index_entries(root: &Path, path_env: &OsStr) -> Option<Vec<IndexEntry>> {
    let out = stdout_bytes(git(root, path_env).args(["ls-files", "-v", "-s", "-z"]))?;
    // `<tag> <mode> <blob> <stage>\t<path>`
    let mut entries = Vec::new();
    for rec in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = rec.iter().position(|b| *b == b'\t')?;
        let (meta, path) = (String::from_utf8_lossy(&rec[..tab]), &rec[tab + 1..]);
        let mut fields = meta.split(' ');
        let (tag, mode, blob) = (fields.next()?, fields.next()?, fields.next()?);
        entries.push(IndexEntry {
            tag: tag.chars().next()?,
            mode: mode.to_string(),
            blob: blob.to_string(),
            path: String::from_utf8_lossy(path).into_owned(),
        });
    }
    Some(entries)
}

/// The gitlinks (mode 160000) among `entries`, each once, sorted.
fn gitlinks_among(entries: &[IndexEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.mode == GITLINK_MODE)
        .map(|e| e.path.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The index mode of a gitlink — a submodule's recorded commit.
pub const GITLINK_MODE: &str = "160000";

/// Every submodule path `root`'s index records (never the gate's own state).
/// `None` when git cannot answer.
#[must_use]
pub fn gitlinks(root: &Path, path_env: &OsStr) -> Option<Vec<String>> {
    Some(
        gitlinks_among(&index_entries(root, path_env)?)
            .into_iter()
            .filter(|p| !is_gate_state(p))
            .collect(),
    )
}

/// `root`'s HEAD commit, or `(unborn)`.
#[must_use]
pub fn head_of(root: &Path, path_env: &OsStr) -> String {
    stdout_line(git(root, path_env).args(["rev-parse", "--verify", "-q", "HEAD"]))
        .unwrap_or_else(|| "(unborn)".to_string())
}

/// [`flag_hidden_edits`] over an index already listed.
fn hidden_among(root: &Path, path_env: &OsStr, entries: &[IndexEntry]) -> Option<Vec<String>> {
    let mut flagged: BTreeMap<String, (String, String)> = BTreeMap::new();
    for e in entries {
        let hidden = e.tag.is_ascii_lowercase() || e.tag == 'S';
        if hidden && !is_gate_state(&e.path) {
            flagged.insert(e.path.clone(), (e.mode.clone(), e.blob.clone()));
        }
    }
    if flagged.is_empty() {
        return Some(Vec::new());
    }
    let paths: Vec<String> = flagged.keys().cloned().collect();
    let states = path_states(root, path_env, &paths)?;
    Some(
        flagged
            .into_iter()
            .filter(|(p, (mode, blob))| !matches_index_entry(states.get(p), mode, blob, path_env))
            .map(|(p, _)| p)
            .collect(),
    )
}

/// Does a working-tree state equal an index entry's mode and blob?
fn matches_index_entry(
    state: Option<&PathState>,
    mode: &str,
    blob: &str,
    path_env: &OsStr,
) -> bool {
    match state {
        Some(PathState::File { blob: b, exec }) => {
            b == blob && mode == if *exec { "100755" } else { "100644" }
        }
        Some(PathState::Symlink { target }) => {
            mode == "120000"
                && hash_stdin(path_env, target.as_os_str().as_encoded_bytes()).as_deref()
                    == Some(blob)
        }
        Some(PathState::Gitlink { head }) => mode == GITLINK_MODE && head == blob,
        _ => false,
    }
}

/// Untracked, non-ignored files under `root` (never the gate's own state, and
/// never the file this run's own output is going to — see [`claim_own_output`]).
#[must_use]
pub fn untracked_paths(root: &Path, path_env: &OsStr) -> Option<Vec<String>> {
    Some(untracked_split(root, path_env)?.0)
}

/// `(the tree's untracked files, the ones that are this run's own output)`.
///
/// One `git ls-files --others` for both, because every caller needs the first
/// and the ladder needs to be able to SAY the second.
#[must_use]
pub fn untracked_split(root: &Path, path_env: &OsStr) -> Option<(Vec<String>, Vec<String>)> {
    let out = stdout_bytes(git(root, path_env).args([
        "ls-files",
        "--others",
        "--exclude-standard",
        "-z",
    ]))?;
    let mut tree = Vec::new();
    let mut own = Vec::new();
    for p in split_z(&out) {
        if is_gate_state(&p) {
            continue;
        }
        if is_own_output(&root.join(&p)) {
            own.push(p);
        } else {
            tree.push(p);
        }
    }
    Some((tree, own))
}

/// Each path's [`PathState`], hashing the regular files in one `git
/// hash-object` call. `--no-filters`: the raw bytes, so the value cannot depend
/// on attributes that differ between two checkouts.
#[must_use]
pub fn path_states(
    root: &Path,
    path_env: &OsStr,
    paths: &[String],
) -> Option<BTreeMap<String, PathState>> {
    let mut states = BTreeMap::new();
    let mut files: Vec<(String, bool)> = Vec::new();
    for p in paths {
        let full = root.join(p);
        let state = match std::fs::symlink_metadata(&full) {
            Err(_) => PathState::Absent,
            Ok(m) if m.file_type().is_symlink() => PathState::Symlink {
                target: std::fs::read_link(&full).unwrap_or_default(),
            },
            Ok(m) if m.is_file() => {
                files.push((p.clone(), is_exec(&m)));
                continue;
            }
            Ok(m) if m.is_dir() && is_git_toplevel(&full, path_env) => PathState::Gitlink {
                head: head_of(&full, path_env),
            },
            Ok(_) => PathState::Other,
        };
        states.insert(p.clone(), state);
    }
    let blobs = hash_paths(root, path_env, files.iter().map(|(p, _)| p.as_str()))?;
    for ((p, exec), blob) in files.into_iter().zip(blobs) {
        states.insert(p, PathState::File { blob, exec });
    }
    Some(states)
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

/// Blob ids of `paths` (relative to `root`), in order.
///
/// # Errors
/// `None` when git fails or answers with the wrong number of ids.
pub fn hash_paths<'a>(
    root: &Path,
    path_env: &OsStr,
    paths: impl IntoIterator<Item = &'a str>,
) -> Option<Vec<String>> {
    let paths: Vec<&str> = paths.into_iter().collect();
    if paths.is_empty() {
        return Some(Vec::new());
    }
    // `--stdin-paths` is line-based; a path with a newline in it goes alone.
    if paths.iter().any(|p| p.contains('\n')) {
        return paths
            .iter()
            .map(|p| {
                stdout_line(
                    git(root, path_env)
                        .args(["hash-object", "--no-filters", "--"])
                        .arg(p),
                )
            })
            .collect();
    }
    let mut child = git(root, path_env)
        .args(["hash-object", "--no-filters", "--stdin-paths"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = String::new();
    for p in &paths {
        input.push_str(p);
        input.push('\n');
    }
    // Write on a thread: a large list can fill the stdout pipe before stdin
    // is fully written, and then both sides wait on each other.
    let mut stdin = child.stdin.take()?;
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().ok()?;
    writer.join().ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    (ids.len() == paths.len()).then_some(ids)
}

/// `git hash-object --stdin` over `bytes`, outside any repository's filters.
fn hash_stdin(path_env: &OsStr, bytes: &[u8]) -> Option<String> {
    let mut child = Command::new("git")
        .env("PATH", path_env)
        .args(["hash-object", "--no-filters", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(bytes).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The source a run verifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceIdentity {
    /// Not the top of a git checkout, and no `.git` of its own: nothing to
    /// capture, nothing printed.
    Unavailable,
    Git(TreeState),
    /// The top of a git checkout whose state git could not read — an
    /// unreadable dirty or untracked file, or one that vanished between the
    /// listing and the hash — or a root with a `.git` git could not open at all
    /// ([`has_git_entry`]). NOT the same as [`SourceIdentity::Unavailable`]
    /// (2026-09-13): read as that, the run went ahead with no source tripwire
    /// and no source line, and an `--in-place` run could go green on a tree
    /// nothing was watching. [`crate::run`] runs no stage on it.
    Unreadable(String),
}

/// How many times a git checkout's state is read before it is called unreadable:
/// a file that vanishes mid-listing is usually gone by the next read.
pub const CAPTURE_ATTEMPTS: usize = 3;

impl SourceIdentity {
    #[must_use]
    pub fn capture(root: &Path, path_env: &OsStr) -> Self {
        if !is_git_toplevel(root, path_env) {
            // A `.git` git cannot open is still a checkout (2026-09-13): read as
            // Unavailable, a worktree whose main checkout was renamed — or any
            // checkout with git absent from PATH — ran with no source tripwire,
            // and a driver that edited the tree mid-run went VERIFY: PASS.
            return if has_git_entry(root) {
                SourceIdentity::Unreadable(unopenable_reason(root))
            } else {
                SourceIdentity::Unavailable
            };
        }
        for _ in 0..CAPTURE_ATTEMPTS {
            if let Some(tree) = TreeState::capture(root, path_env) {
                return SourceIdentity::Git(tree);
            }
        }
        SourceIdentity::Unreadable(format!(
            "git could not read the tree at {} in {CAPTURE_ATTEMPTS} attempts — an unreadable \
             or vanishing dirty or untracked file stops `git hash-object`",
            root.display()
        ))
    }
}

/// `(dev, ino, len, mtime)` — what changes when a file is rewritten or
/// replaced, without reading it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub mtime_ns: i128,
}

impl FileStamp {
    /// Follows symlinks: the stamp is of the file that would be executed, so
    /// repointing a link is a change too (a different inode).
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        let m = std::fs::metadata(path).ok()?;
        Some(Self::from_metadata(&m))
    }

    #[cfg(unix)]
    fn from_metadata(m: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
        }
    }

    #[cfg(not(unix))]
    fn from_metadata(m: &std::fs::Metadata) -> Self {
        let mtime_ns = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i128);
        Self {
            dev: 0,
            ino: 0,
            len: m.len(),
            mtime_ns,
        }
    }
}

/// The compiler a run uses. Built by [`crate::Toolchain::identity`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolchainIdentity {
    /// Each tool's path and its stamp (`None` = absent at capture).
    pub files: Vec<(PathBuf, Option<FileStamp>)>,
    /// `commit-hash:` from `trustc -vV`, when trustc answered.
    pub commit: Option<String>,
}

impl ToolchainIdentity {
    /// The first tool whose stamp changed since capture, in words.
    ///
    /// Re-stats only. A file whose `(dev, ino, len, mtime)` are all unchanged
    /// has not been rewritten or replaced, so asking `trustc -vV` again would
    /// spawn a compiler per stage to learn nothing.
    #[must_use]
    pub fn moved(&self) -> Option<String> {
        let moved: Vec<String> = self
            .files
            .iter()
            .filter_map(|(path, then)| {
                let now = FileStamp::of(path);
                (now != *then).then(|| {
                    let word = match (then, now) {
                        (None, Some(_)) => "appeared",
                        (Some(_), None) => "vanished",
                        _ => "was rewritten or replaced",
                    };
                    format!("{} {word}", path.display())
                })
            })
            .collect();
        (!moved.is_empty()).then(|| moved.join("; "))
    }
}

/// The `commit-hash:` line of a `rustc -vV`-shaped answer.
#[must_use]
pub fn commit_hash_in(verbose_version: &str) -> Option<String> {
    verbose_version
        .lines()
        .find_map(|l| l.trim().strip_prefix("commit-hash:"))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty() && h != "unknown")
}

/// The two identities, armed at the start of a run.
#[derive(Debug)]
pub struct Tripwire {
    root: PathBuf,
    path_env: std::ffi::OsString,
    pub source: SourceIdentity,
    pub toolchain: ToolchainIdentity,
    state: Mutex<TripState>,
}

#[derive(Debug, Default)]
struct TripState {
    /// When the last clean check ran, and how many stages had finished then.
    last_clean: Option<(Instant, u64)>,
    tripped: Option<String>,
    finished: u64,
}

/// How long a clean check stands for the stages that start right after it.
/// Stages start in bursts; one `git diff` + `ls-files` per burst is enough,
/// and the check before the verdict is never cached.
///
/// A clean result also stops standing the moment ANY stage finishes
/// ([`Tripwire::stage_finished`]). Measured 2026-09-13 on this crate's own
/// fixture: with the age alone, a commit made DURING a 0.1 s build was
/// invisible to the test stage that started right after it — the check at the
/// build's start was still under 2 s old — so the test ran on the moved tree.
pub const CHECK_CACHE: Duration = Duration::from_secs(2);

impl Tripwire {
    /// Arm on whatever `root` holds now.
    #[must_use]
    pub fn arm(root: &Path, path_env: &OsStr, toolchain: ToolchainIdentity) -> Self {
        Self::arm_against(root, path_env, None, toolchain)
    }

    /// Arm on `baseline` when there is one — the state a snapshot's sync
    /// VERIFIED equal to the caller's — and compare it with a fresh capture
    /// now, so anything that moved between the sync and the ladder trips the
    /// run instead of silently becoming its baseline.
    #[must_use]
    pub fn arm_against(
        root: &Path,
        path_env: &OsStr,
        baseline: Option<TreeState>,
        toolchain: ToolchainIdentity,
    ) -> Self {
        let fresh = SourceIdentity::capture(root, path_env);
        let (source, tripped) = match (baseline, fresh) {
            (None, fresh) => (fresh, None),
            (Some(then), SourceIdentity::Git(now)) => {
                let moved = then.moved_to(&now).map(|m| format!("source: {m}"));
                (SourceIdentity::Git(then), moved)
            }
            (Some(_), SourceIdentity::Unreadable(why)) => (SourceIdentity::Unreadable(why), None),
            (Some(_), SourceIdentity::Unavailable) => (
                SourceIdentity::Unreadable(format!(
                    "{} was synced as a git checkout and no longer is one",
                    root.display()
                )),
                None,
            ),
        };
        Self {
            root: root.to_path_buf(),
            path_env: path_env.to_os_string(),
            source,
            toolchain,
            state: Mutex::new(TripState {
                tripped,
                ..TripState::default()
            }),
        }
    }

    /// `Some(what moved)` once the source or the compiler has moved; sticky,
    /// so every later stage reads the same answer. A clean result younger than
    /// `max_age` is reused. Serialised: concurrent stage starts share one check.
    #[must_use]
    pub fn check(&self, max_age: Duration) -> Option<String> {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(why) = &st.tripped {
            return Some(why.clone());
        }
        let finished = st.finished;
        if st
            .last_clean
            .is_some_and(|(t, gen_then)| gen_then == finished && t.elapsed() < max_age)
        {
            return None;
        }
        let mut why = Vec::new();
        if let SourceIdentity::Git(then) = &self.source {
            match TreeState::capture(&self.root, &self.path_env) {
                Some(now) => {
                    if let Some(m) = then.moved_to(&now) {
                        let mut m = format!("source: {m}");
                        m.push_str(&self.own_log_hint(&then.moved_paths(&now)));
                        why.push(m);
                    }
                }
                None => why.push("source: git could no longer read the tree".to_string()),
            }
        }
        why.extend(self.toolchain.moved().map(|m| format!("toolchain: {m}")));
        if why.is_empty() {
            st.last_clean = Some((Instant::now(), finished));
            None
        } else {
            let why = why.join("; ");
            st.tripped = Some(why.clone());
            Some(why)
        }
    }

    /// The remedy sentence for the one cause of a trip the operator can fix in
    /// one move: THE RUN'S OWN LOG, written inside the checkout.
    ///
    /// [`claim_own_output`] already excludes a plain `> gate.log` redirect,
    /// because fd 1 is then the file itself. It cannot see a `| tee gate.log`:
    /// fd 1 is a pipe and the file belongs to `tee`. So when everything that
    /// moved is UNTRACKED — a state no `git pull`, rebase or edit of a source
    /// file produces on its own — the tool says what to do instead of leaving
    /// the reader to guess which of their files is churning.
    ///
    /// Empty when a tracked path moved too: that is a real edit, and the
    /// remedy would be a lie.
    fn own_log_hint(&self, moved: &[String]) -> String {
        if moved.is_empty() {
            return String::new();
        }
        let Some((untracked, _)) = untracked_split(&self.root, &self.path_env) else {
            return String::new();
        };
        if !moved.iter().all(|p| untracked.contains(p)) {
            return String::new();
        }
        format!(
            " — every one of those is UNTRACKED. If one is this run's own log (a \
             `| tee` or an editor scratch file inside {}), write it outside the checkout or \
             under {GATE_STATE_DIR}/, which the gate never reads as source; the gate already \
             keeps its own copy of this ladder (see the `verify: log` line on stderr).",
            self.root.display()
        )
    }

    /// A stage finished: whatever it did happened after any cached check.
    pub fn stage_finished(&self) {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.finished += 1;
    }

    /// `verify: source <sha>[+dirty <d12>] <where>` — or nothing for a root
    /// with no source identity.
    #[must_use]
    pub fn header_line(&self, place: &str) -> Option<String> {
        let SourceIdentity::Git(tree) = &self.source else {
            return None;
        };
        let dirty = tree
            .dirty_digest(&self.path_env)
            .map(|d| format!("+dirty {}", &d[..d.len().min(12)]))
            .unwrap_or_default();
        Some(format!("verify: source {}{dirty} {place}\n", tree.head))
    }
}

/// The ladder label for a git checkout whose state could not be read.
#[must_use]
pub fn unreadable_label(why: &str) -> String {
    format!(
        "{why} — the gate cannot name the tree it would verify, so no stage ran and \
         nothing can be claimed"
    )
}

/// The ladder label for a tripped run.
#[must_use]
pub fn tripped_label(why: &str) -> String {
    format!(
        "the tree or the toolchain moved during this run ({why}) — what ran was not \
         one tree built by one compiler, so nothing below can be claimed"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(blob: &str) -> PathState {
        PathState::File {
            blob: blob.to_string(),
            exec: false,
        }
    }

    #[test]
    fn a_move_names_the_new_head_and_every_moved_path_but_not_the_unmoved_ones() {
        let then = TreeState {
            head: "a".repeat(40),
            dirty: BTreeMap::from([
                ("same.rs".to_string(), file("1")),
                ("edited.rs".to_string(), file("2")),
                ("gone.rs".to_string(), file("3")),
            ]),
        };
        let now = TreeState {
            head: "b".repeat(40),
            dirty: BTreeMap::from([
                ("same.rs".to_string(), file("1")),
                ("edited.rs".to_string(), file("9")),
                ("new.rs".to_string(), PathState::Absent),
            ]),
        };
        assert_eq!(then.moved_to(&then.clone()), None);
        let m = then.moved_to(&now).expect("moved");
        assert!(
            m.contains(&format!("HEAD {} -> {}", "a".repeat(40), "b".repeat(40))),
            "{m}"
        );
        assert!(
            m.contains("edited.rs") && m.contains("gone.rs") && m.contains("new.rs"),
            "{m}"
        );
        assert!(!m.contains("same.rs"), "{m}");
    }

    #[test]
    fn a_pull_that_moves_thousands_of_paths_is_still_one_ladder_line() {
        let many: Vec<String> = (0..500).map(|i| format!("p{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let s = name_some(&refs);
        assert!(s.ends_with(" and 492 more"), "{s}");
        assert!(!s.contains('\n'));
    }

    #[test]
    fn the_gates_own_state_dir_is_never_part_of_the_source() {
        assert!(is_gate_state(".aterm-verify"));
        assert!(is_gate_state(".aterm-verify/lock"));
        assert!(!is_gate_state(".aterm-verify-stamp"));
        assert!(!is_gate_state("src/.aterm-verify/x"));
    }

    #[test]
    fn the_commit_hash_is_read_from_the_verbose_version_and_unknown_is_none() {
        let vv = "rustc 1.99.0-dev (43f8b339f 2026-09-12) (trustc 0.1.0)\nbinary: trustc\n\
                  commit-hash: 43f8b339f8322f6b4cd1d9f7ae9a914eda5fef22\nhost: aarch64-apple-darwin\n";
        assert_eq!(
            commit_hash_in(vv).as_deref(),
            Some("43f8b339f8322f6b4cd1d9f7ae9a914eda5fef22")
        );
        assert_eq!(commit_hash_in("commit-hash: unknown\n"), None);
        assert_eq!(commit_hash_in("rustc 1.0\n"), None);
    }

    #[test]
    fn a_rewritten_or_vanished_tool_is_named_and_an_untouched_one_is_not() {
        let tmp = crate::mktemp_dir("atv-ident").expect("mktemp");
        let (kept, rewritten, vanishing) =
            (tmp.join("targo"), tmp.join("trustc"), tmp.join("tippy"));
        for p in [&kept, &rewritten, &vanishing] {
            std::fs::write(p, b"v1").expect("write");
        }
        let id = ToolchainIdentity {
            files: [&kept, &rewritten, &vanishing, &tmp.join("trustdoc")]
                .into_iter()
                .map(|p| (p.clone(), FileStamp::of(p)))
                .collect(),
            commit: None,
        };
        assert_eq!(id.moved(), None);
        // A replacement by rename is a new inode even at the same length.
        std::fs::write(tmp.join("trustc.new"), b"v2").expect("write");
        std::fs::rename(tmp.join("trustc.new"), &rewritten).expect("rename");
        std::fs::remove_file(&vanishing).expect("rm");
        let m = id.moved().expect("moved");
        assert!(m.contains("trustc was rewritten or replaced"), "{m}");
        assert!(m.contains("tippy vanished"), "{m}");
        assert!(!m.contains("targo"), "{m}");
        assert!(
            !m.contains("trustdoc"),
            "an absent tool that stays absent is no move: {m}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }
}
