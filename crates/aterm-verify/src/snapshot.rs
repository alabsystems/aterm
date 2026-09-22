// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pinned snapshot a run verifies.
//!
//! WHY (2026-09-13). The 14 h `--fast` run happened in the owner's live checkout:
//! auto-pulled four times mid-run, indexed by Spotlight, and sharing `target/`
//! with every other agent's build on the machine. Its test stage relinked
//! binaries that the smokes then drove. None of that was compile work the gate
//! had asked for, and none of it was visible in the ladder.
//!
//! So a run (everything but `--selftest`, unless `--in-place`) happens in a git
//! WORKTREE of the caller's repository at `<caller-root>-verify.noindex` (or
//! `$ATERM_VERIFY_SNAPSHOT`). The `.noindex` suffix keeps Spotlight out of the
//! sources and every target dir without touching macOS defaults. Before
//! anything is planned, the worktree is synced to exactly what the caller has:
//!
//!  1. `git checkout --detach --force <caller HEAD>`;
//!  2. `git clean -fd` — NEVER `-x`, so the ignored lane target dirs stay warm;
//!  3. `git diff --binary HEAD` from the caller, piped to `git apply --binary`;
//!  4. every untracked, non-ignored file copied across;
//!  5. every edit an assume-unchanged or skip-worktree flag hides from `git
//!     diff` copied across ([`identity::flag_hidden_edits`]) — the diff cannot
//!     carry them, and without this the snapshot built HEAD's bytes.
//!
//! and then CHECKED: the snapshot's [`TreeState`] must equal the caller's, or
//! the sync is retried and finally refused. An ignored file in the caller is
//! not copied — a test that silently depended on one is a test to fix.
//!
//! Nothing here writes to the caller's checkout. Every caller-side git call is
//! a read with `--no-optional-locks`; the worktree registration is the one
//! write to the caller's `.git`.
//!
//! A dirty file whose content is already right in the snapshot is set aside
//! across the sync and put back, so its mtime survives and cargo does not
//! rebuild its crate on every run of an unchanged dirty tree.
//!
//! Exclusive: `.aterm-verify/lock` in the snapshot, pid-checked. A second gate
//! on a held snapshot is COULD NOT RUN rather than a run on a tree being synced
//! under it.
//!
//! Lanes: each target dir in the snapshot carries a stamp of the trustc commit
//! and of the build-relevant environment. A new compiler makes a lane's
//! artifacts unreusable, so the lane is moved to `.aterm-verify/trash` and
//! deleted at background priority while the run proceeds; any other change is
//! named, so a cold lane is explained instead of guessed at.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::identity::{self, GATE_STATE_DIR, PathState, TreeState, git};

/// Where the snapshot lives, when not `<caller-root>-verify.noindex`.
pub const SNAPSHOT_ENV: &str = "ATERM_VERIFY_SNAPSHOT";

/// The per-lane stamp, at the top of each lane's target dir.
pub const STAMP_FILE: &str = ".aterm-verify-stamp";

/// The environment recorded in a lane stamp. Each of these reaches a compile
/// (directly, through a build script, or through cargo's own fingerprint), so a
/// change is a reason a lane rebuilt. `PATH` and `RUSTDOC` stay out: no build
/// script in this tree watches them. `CARGO_PROFILE_*` is matched by prefix.
/// `CARGO_INCREMENTAL` left the list on 2026-09-21: the gate sets it to `0` in
/// every child ([`crate::CHILD_ENV`]), so the caller's value never reaches a
/// compile and a stamp that named it would explain a rebuild that did not happen.
pub const LANE_ENV_VARS: [&str; 14] = [
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTUP_TOOLCHAIN",
    "SOURCE_DATE_EPOCH",
    "MACOSX_DEPLOYMENT_TARGET",
    "SDKROOT",
    "CC",
    "CFLAGS",
    "CARGO_HOME",
];

const LANE_ENV_PREFIX: &str = "CARGO_PROFILE_";

/// Where the stages of a run execute.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SourceMode {
    /// The caller's own checkout — `--in-place`, `--selftest`, or a root that
    /// is not a git checkout.
    #[default]
    InPlace,
    /// A synced snapshot of `caller`.
    Snapshot { caller: PathBuf },
}

impl SourceMode {
    /// The tail of the `verify: source …` header line.
    #[must_use]
    pub fn place(&self, root: &Path) -> String {
        match self {
            SourceMode::InPlace => format!("in place {}", root.display()),
            SourceMode::Snapshot { caller } => {
                format!("snapshot {} (of {})", root.display(), caller.display())
            }
        }
    }
}

/// `<caller-root>-verify.noindex`, beside the caller's checkout.
#[must_use]
pub fn default_root(caller: &Path) -> PathBuf {
    let name = caller
        .file_name()
        .map_or_else(|| "repo".into(), OsStr::to_os_string);
    let mut name = name;
    name.push("-verify.noindex");
    caller.with_file_name(name)
}

/// The build-relevant environment, from any `(name, value)` source — pure, so
/// the selection is a test; [`lane_env_from_process`] is the live read.
#[must_use]
pub fn lane_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(String, Option<OsString>)> {
    let mut present: BTreeMap<String, OsString> = BTreeMap::new();
    for (k, v) in vars {
        let Some(k) = k.to_str() else { continue };
        if LANE_ENV_VARS.contains(&k) || k.starts_with(LANE_ENV_PREFIX) {
            present.insert(k.to_string(), v);
        }
    }
    let mut out: Vec<(String, Option<OsString>)> = LANE_ENV_VARS
        .iter()
        .map(|k| ((*k).to_string(), present.remove(*k)))
        .collect();
    out.extend(present.into_iter().map(|(k, v)| (k, Some(v))));
    out
}

/// [`lane_env`] over this process's environment. Main thread only.
#[must_use]
pub fn lane_env_from_process() -> Vec<(String, Option<OsString>)> {
    lane_env(std::env::vars_os())
}

/// What [`prepare`] needs.
#[derive(Clone, Debug)]
pub struct Options<'a> {
    pub caller: &'a Path,
    pub snapshot: PathBuf,
    pub path_env: &'a OsStr,
    pub lane_env: Vec<(String, Option<OsString>)>,
    pub trustc_commit: Option<String>,
}

/// A prepared, locked snapshot. Dropping it releases the lock;
/// [`Snapshot::finish`] also waits for the background lane deletion.
#[derive(Debug)]
pub struct Snapshot {
    pub root: PathBuf,
    pub caller: PathBuf,
    /// The state the snapshot was verified to hold.
    pub tree: TreeState,
    /// `verify: lane …` lines for the ladder header.
    pub notes: Vec<String>,
    trash: Option<Child>,
    _lock: Lock,
}

impl Snapshot {
    /// Wait for any pruned lane to finish deleting, then release the lock.
    pub fn finish(mut self) {
        if let Some(mut c) = self.trash.take() {
            let _ = c.wait();
        }
    }
}

/// Prepare the snapshot: create or repair the worktree, take the lock, sync
/// and verify, stamp the lanes.
///
/// # Errors
/// A sentence saying why no snapshot could be had. The caller must not run the
/// ladder: nothing has been decided, and running in place instead would bring
/// back exactly the hazards the snapshot exists to remove.
pub fn prepare(o: &Options<'_>) -> Result<Snapshot, String> {
    let caller = std::fs::canonicalize(o.caller)
        .map_err(|e| format!("cannot resolve the checkout {}: {e}", o.caller.display()))?;
    if !identity::is_git_toplevel(&caller, o.path_env) {
        return Err(format!(
            "{} is not the top of a git checkout, so it cannot be snapshotted (pass --in-place)",
            caller.display()
        ));
    }
    let snap = std::path::absolute(&o.snapshot).map_err(|e| {
        format!(
            "cannot resolve the snapshot path {}: {e}",
            o.snapshot.display()
        )
    })?;
    let snap_resolved = resolve_lexically(&snap);
    if snap_resolved.starts_with(&caller) || caller.starts_with(&snap_resolved) {
        return Err(format!(
            "the snapshot {} and the checkout {} contain one another; a snapshot must live \
             beside the checkout, never inside it",
            snap.display(),
            caller.display()
        ));
    }
    refuse_other_checkouts(&caller, &snap_resolved, o.path_env)?;
    ensure_worktree(&caller, &snap, o.path_env)?;
    let snap = std::fs::canonicalize(&snap)
        .map_err(|e| format!("cannot resolve the snapshot {}: {e}", snap.display()))?;
    refuse_symlinked_dir(&snap, &snap.join(GATE_STATE_DIR)).map_err(|e| e.to_string())?;
    let lock = Lock::acquire(&snap.join(GATE_STATE_DIR), &caller)?;
    let tree = sync(&caller, &snap, o.path_env)?;
    let (notes, trash) = stamp_lanes(&snap, &o.lane_env, o.trustc_commit.as_deref());
    Ok(Snapshot {
        root: snap,
        caller,
        tree,
        notes,
        trash,
        _lock: lock,
    })
}

/// The ladder text for a run that could not get its snapshot.
#[must_use]
pub fn could_not_run_text(why: &str) -> String {
    format!(
        "  FAIL  snapshot: {why}\n  VERIFY: COULD NOT RUN — the gate could not prepare the \
         snapshot it verifies, so it decided nothing. This is NOT a finding about your change.\n"
    )
}

/// `path` with its longest existing ancestor canonicalised — the comparison
/// the nesting refusal needs before the directory exists.
fn resolve_lexically(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(c) = std::fs::canonicalize(cur) {
            return tail
                .iter()
                .rev()
                .fold(c, |acc: PathBuf, n: &&OsStr| acc.join(n));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(p), Some(n)) => {
                tail.push(n);
                cur = p;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn run_git(mut cmd: Command, what: &str) -> Result<Vec<u8>, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("{what}: cannot run git: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// `git rev-parse <flag>` in `dir`, resolved against `dir` and canonicalised.
fn rev_parse_path(dir: &Path, path_env: &OsStr, flag: &str) -> Option<PathBuf> {
    let out = git(dir, path_env).args(["rev-parse", flag]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    std::fs::canonicalize(dir.join(rel)).ok()
}

/// The gate's claim on a worktree it created: written by [`ensure_worktree`]
/// right after its own `git worktree add`, inside the state dir the sync never
/// cleans. A directory without it was not made by the gate, whatever else it is.
pub const MARKER_FILE: &str = "snapshot-of";

/// The marker's first line. Then `caller <checkout>` and `snapshot <the
/// snapshot's own canonical path>`.
const MARKER_MAGIC: &str = "aterm-verify snapshot\n";

fn marker_path(snap: &Path) -> PathBuf {
    snap.join(GATE_STATE_DIR).join(MARKER_FILE)
}

/// The `snapshot <path>` line a marker at `snap` must carry.
fn marker_self_line(snap: &Path) -> Option<String> {
    std::fs::canonicalize(snap)
        .ok()
        .map(|c| format!("snapshot {}", c.display()))
}

/// What the marker at `snap` says: `None` for no gate marker at all,
/// `Some(true)` when it names exactly this directory, `Some(false)` when it
/// names another one or none.
///
/// The path matters (2026-09-13): a `cp -R` of a marked snapshot carries the
/// marker AND a `.git` file still pointing at the original's admin dir, so it
/// passed as a linked worktree of the caller and was synced over. A marker
/// written before it named its own path (round 2) is refused the same way.
///
/// TRUSTED, NOT PROVEN. The marker is a claim the gate wrote, not evidence it
/// could check: anyone who can write a file can write one naming its own
/// directory. A marker forged into another checkout is out of scope — only the
/// gate writes `.aterm-verify/snapshot-of` — and what this guards against is
/// the accidents measured above: a path that is not the gate's, a copy, a move.
fn marker_state(snap: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(marker_path(snap)).ok()?;
    if !text.starts_with(MARKER_MAGIC) {
        return None;
    }
    let own = marker_self_line(snap);
    Some(own.is_some_and(|own| text.lines().any(|l| l == own)))
}

fn write_marker(snap: &Path, caller: &Path) -> Result<(), String> {
    let path = marker_path(snap);
    let own = marker_self_line(snap)
        .ok_or_else(|| format!("cannot resolve the new snapshot {}", snap.display()))?;
    refuse_symlinked_dir(snap, &snap.join(GATE_STATE_DIR))
        .and_then(|()| std::fs::create_dir_all(snap.join(GATE_STATE_DIR)))
        .and_then(|()| {
            std::fs::write(
                &path,
                format!("{MARKER_MAGIC}caller {}\n{own}\n", caller.display()),
            )
        })
        .map_err(|e| format!("cannot write the snapshot marker {}: {e}", path.display()))
}

/// A LINKED worktree (its own git dir differs from the common one) of the
/// caller's repository. The main checkout shares the common dir too, and is
/// never a snapshot.
fn is_linked_worktree_of(caller: &Path, snap: &Path, path_env: &OsStr) -> bool {
    if !identity::is_git_toplevel(snap, path_env) {
        return false;
    }
    let (Some(own), Some(common)) = (
        rev_parse_path(snap, path_env, "--absolute-git-dir"),
        rev_parse_path(snap, path_env, "--git-common-dir"),
    ) else {
        return false;
    };
    own != common && rev_parse_path(caller, path_env, "--git-common-dir") == Some(common)
}

/// Every checkout `git worktree list` knows for the caller's repository.
fn listed_worktrees(caller: &Path, path_env: &OsStr) -> Result<Vec<PathBuf>, String> {
    let mut list = git(caller, path_env);
    list.args(["worktree", "list", "--porcelain", "-z"]);
    let out = run_git(list, "git worktree list")?;
    Ok(out
        .split(|b| *b == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .map(path_from_bytes)
        .collect())
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Refuse a snapshot path that is, contains, or lies inside any checkout of
/// the caller's repository the gate did not create — the main checkout, the
/// caller itself, another agent's worktree. The sync's `checkout --force` and
/// `clean -fd` would wipe that checkout's uncommitted work (reproduced
/// 2026-09-13: a sibling worktree lost an unstaged edit and an untracked file).
/// Only a directory at exactly this path carrying a gate marker is let through
/// — to [`ensure_worktree`], which refuses, before writing anything, a marker
/// that does not name that path; a registration whose directory is gone holds
/// nothing to lose.
///
/// The marker is TRUSTED, not proven (see [`marker_state`]): a sibling
/// checkout carrying a forged marker at exactly the snapshot path would be let
/// through. That is out of scope, because only the gate writes the marker.
fn refuse_other_checkouts(caller: &Path, snap: &Path, path_env: &OsStr) -> Result<(), String> {
    for listed in listed_worktrees(caller, path_env)? {
        if std::fs::symlink_metadata(&listed).is_err() {
            continue;
        }
        let listed = std::fs::canonicalize(&listed).unwrap_or(listed);
        if listed == snap && marker_state(&listed).is_some() {
            continue;
        }
        if snap.starts_with(&listed) || listed.starts_with(snap) {
            return Err(format!(
                "the snapshot path {} {} the checkout {}, which the gate did not create \
                 (no {GATE_STATE_DIR}/{MARKER_FILE}) — refusing to touch it: a sync would \
                 wipe its uncommitted work; point {SNAPSHOT_ENV} at a path of its own",
                snap.display(),
                if listed == snap {
                    "is"
                } else if listed.starts_with(snap) {
                    "contains"
                } else {
                    "lies inside"
                },
                listed.display()
            ));
        }
    }
    Ok(())
}

/// Create the worktree, or accept (and if needed repair) an existing one.
/// An existing directory is accepted only when it is a linked worktree of this
/// repository AND carries the gate's marker; anything else is refused, never
/// overwritten.
fn ensure_worktree(caller: &Path, snap: &Path, path_env: &OsStr) -> Result<(), String> {
    let empty_dir = std::fs::read_dir(snap).is_ok_and(|mut d| d.next().is_none());
    if !snap.exists() || empty_dir {
        if let Some(parent) = snap.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        // `--force`: a registration left behind by a deleted snapshot would
        // otherwise refuse the path. `--detach`: no branch is checked out, so
        // the snapshot can never hold a branch the caller wants.
        let mut add = git(caller, path_env);
        add.args(["worktree", "add", "--detach", "--force"])
            .arg(snap)
            .arg("HEAD");
        run_git(add, "git worktree add")?;
        write_marker(snap, caller)?;
    }
    let refusal = |what: &str| {
        Err(format!(
            "{} exists and {what} — refusing to touch it; remove it or point {SNAPSHOT_ENV} \
             elsewhere",
            snap.display()
        ))
    };
    match marker_state(snap) {
        None => {
            return refusal(&format!(
                "has no {GATE_STATE_DIR}/{MARKER_FILE}, so the gate did not create it"
            ));
        }
        Some(false) => {
            return refusal(&format!(
                "carries a {GATE_STATE_DIR}/{MARKER_FILE} that does not name this path — a \
                 copy or a move of a snapshot, or one marked by an older gate"
            ));
        }
        Some(true) => {}
    }
    if is_linked_worktree_of(caller, snap, path_env) {
        return Ok(());
    }
    if snap.join(".git").is_file() {
        let mut repair = git(caller, path_env);
        repair.args(["worktree", "repair"]).arg(snap);
        let _ = run_git(repair, "git worktree repair");
        if is_linked_worktree_of(caller, snap, path_env) {
            return Ok(());
        }
    }
    refusal(&format!("is not a linked worktree of {}", caller.display()))
}

/// How many times the sync may find the caller moving before it gives up.
const SYNC_ATTEMPTS: usize = 3;

/// Sync `snap` to `caller`'s HEAD, diff and untracked files, and prove it.
fn sync(caller: &Path, snap: &Path, path_env: &OsStr) -> Result<TreeState, String> {
    let keep_dir = snap.join(GATE_STATE_DIR).join("keep");
    let mut last = String::new();
    let mut last_paths: Vec<String> = Vec::new();
    let keep_ok = || refuse_symlinked_dir(snap, &keep_dir).map_err(|e| e.to_string());
    for _ in 0..SYNC_ATTEMPTS {
        keep_ok()?;
        let _ = std::fs::remove_dir_all(&keep_dir);
        let want = TreeState::capture(caller, path_env)
            .ok_or_else(|| format!("git could not read the checkout {}", caller.display()))?;
        if want.head == "(unborn)" {
            return Err(format!("{} has no commit to snapshot", caller.display()));
        }
        let have = TreeState::capture(snap, path_env)
            .ok_or_else(|| format!("git could not read the snapshot {}", snap.display()))?;

        let kept = set_aside(snap, &keep_dir, &have, &want);

        let mut checkout = git(snap, path_env);
        checkout
            .args([
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "-q",
                "--detach",
                "--force",
            ])
            .arg(&want.head);
        run_git(checkout, "git checkout --detach --force in the snapshot")?;

        // `-e`: the gate's own lock and trash are not the caller's to lose.
        let mut clean = git(snap, path_env);
        clean.args(["clean", "-fdq", "-e", &format!("/{GATE_STATE_DIR}/")]);
        run_git(clean, "git clean -fd in the snapshot")?;

        apply_callers_diff(caller, snap, path_env)?;

        for p in identity::untracked_paths(caller, path_env)
            .ok_or_else(|| "git could not list the checkout's untracked files".to_string())?
        {
            copy_entry(snap, &caller.join(&p), &snap.join(&p)).map_err(|e| {
                format!("cannot copy the untracked file {p} into the snapshot: {e}")
            })?;
        }

        mirror_flag_hidden_edits(caller, snap, path_env, &want)?;

        put_back(snap, path_env, kept);
        keep_ok()?;
        let _ = std::fs::remove_dir_all(&keep_dir);

        let got = TreeState::capture(snap, path_env)
            .ok_or_else(|| format!("git could not read the snapshot {}", snap.display()))?;
        match want.moved_to(&got) {
            None => return Ok(got),
            Some(m) => {
                last = m;
                last_paths = want.moved_paths(&got);
            }
        }
    }
    Err(format!(
        "the snapshot still differed from the checkout after {SYNC_ATTEMPTS} syncs ({last}) — \
         the checkout is changing faster than it can be copied{}",
        churn_remedy(caller, path_env, &last_paths)
    ))
}

/// The remedy for the ONE cause of sync churn an operator can fix in one move:
/// a file the run itself is writing inside the checkout.
///
/// A growing untracked file — `verify.sh --fast | tee gate.log`, an editor's
/// scratch file, another agent's output — differs between the capture and the
/// copy on every attempt, so the sync exhausts its attempts and the run decides
/// nothing. A plain `> gate.log` redirect is already excluded
/// ([`identity::claim_own_output`]); this is what covers the rest. When a
/// TRACKED path churned too, somebody is really editing source and the remedy
/// would be a lie, so nothing is added.
fn churn_remedy(caller: &Path, path_env: &OsStr, moved: &[String]) -> String {
    if moved.is_empty() {
        return String::new();
    }
    let Some((untracked, _)) = identity::untracked_split(caller, path_env) else {
        return String::new();
    };
    if !moved.iter().all(|p| untracked.contains(p)) {
        return String::new();
    }
    format!(
        ". Every path that churned is UNTRACKED: if one of them is this run's own log, \
         write it outside {} or under {GATE_STATE_DIR}/, which the gate never copies or \
         reads as source",
        caller.display()
    )
}

/// `git diff --binary HEAD | git apply --binary`, with every config knob that
/// changes the patch's shape pinned off.
fn apply_callers_diff(caller: &Path, snap: &Path, path_env: &OsStr) -> Result<(), String> {
    let mut diff = git(caller, path_env);
    diff.args([
        "-c",
        "diff.noprefix=false",
        "-c",
        "diff.mnemonicPrefix=false",
        "diff",
        "--binary",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-relative",
        "--ignore-submodules=all",
        "HEAD",
    ]);
    let patch = run_git(diff, "git diff --binary HEAD in the checkout")?;
    if patch.is_empty() {
        return Ok(());
    }
    let mut apply = git(snap, path_env);
    let mut child = apply
        .args(["apply", "--binary", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git apply: cannot run git: {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "git apply: no stdin".to_string())?;
    let writer = std::thread::spawn(move || stdin.write_all(&patch));
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git apply: {e}"))?;
    let wrote = writer
        .join()
        .map_err(|_| "git apply: writer panicked".to_string())?;
    if !out.status.success() || wrote.is_err() {
        return Err(format!(
            "the checkout's uncommitted diff did not apply to the snapshot: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Carry each edit an index flag hides from the caller's `git diff` (see
/// [`identity::flag_hidden_edits`]): the file or link copied, a deletion
/// removed. Anything else (a gitlink) is left alone for the verification to
/// refuse.
fn mirror_flag_hidden_edits(
    caller: &Path,
    snap: &Path,
    path_env: &OsStr,
    want: &TreeState,
) -> Result<(), String> {
    let hidden = identity::flag_hidden_edits(caller, path_env)
        .ok_or_else(|| "git could not list the checkout's index flags".to_string())?;
    for p in hidden {
        let dst = snap.join(&p);
        let done = match want.dirty.get(&p) {
            Some(PathState::File { .. } | PathState::Symlink { .. }) => {
                copy_entry(snap, &caller.join(&p), &dst)
            }
            Some(PathState::Absent) => refuse_symlinked_ancestors(snap, &dst).and_then(|()| {
                if std::fs::symlink_metadata(&dst).is_ok_and(|m| !m.is_dir()) {
                    std::fs::remove_file(&dst)
                } else {
                    Ok(())
                }
            }),
            _ => Ok(()),
        };
        done.map_err(|e| {
            format!("cannot carry the index-flagged edit {p} into the snapshot: {e}")
        })?;
    }
    Ok(())
}

/// Refuse a write beneath a symbolic link: every ancestor of `dst` strictly
/// between `snap` and `dst` must be a real directory (or not exist yet).
///
/// WHY (2026-09-13, review of batch B, round 3): HEAD committed `link` as a
/// symlink to a directory outside both checkouts; the caller hid, under
/// `--skip-worktree`, that `link` was now a real directory holding an
/// untracked `link/x`. The diff could not carry the typechange, so the
/// snapshot kept the symlink, and copying `link/x` into it followed the link
/// and overwrote the file outside. `create_dir_all`, `File::create`,
/// `remove_file` and `rename` all follow a symlinked ancestor, so every write
/// the sync makes inside the snapshot asks this first, and the sync fails
/// closed on the first one it refuses. `dst` itself may be a link: it is
/// replaced or removed, never written through.
///
/// A check, then a write: the snapshot is under the gate's lock and git's own
/// writes (checkout, clean, apply) refuse paths beyond a symlink themselves, so
/// nothing else is expected to plant a link between the two.
fn refuse_symlinked_ancestors(snap: &Path, dst: &Path) -> std::io::Result<()> {
    use std::path::Component;
    let rel = dst.strip_prefix(snap).map_err(|_| {
        std::io::Error::other(format!(
            "{} is not inside the snapshot {}",
            dst.display(),
            snap.display()
        ))
    })?;
    let parts: Vec<Component<'_>> = rel.components().collect();
    let mut at = snap.to_path_buf();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        let Component::Normal(name) = part else {
            return Err(std::io::Error::other(format!(
                "{} is not a plain path inside the snapshot",
                dst.display()
            )));
        };
        at.push(name);
        if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(std::io::Error::other(format!(
                "{} is a symbolic link in the snapshot, so writing {} would write through it \
                 — refusing (the checkout replaced it with a directory an index flag hides?)",
                at.display(),
                dst.display()
            )));
        }
    }
    Ok(())
}

/// [`refuse_symlinked_ancestors`] for a DIRECTORY the sync creates, empties or
/// moves: the directory itself must not be a link either.
fn refuse_symlinked_dir(snap: &Path, dir: &Path) -> std::io::Result<()> {
    refuse_symlinked_ancestors(snap, &dir.join("_"))
}

/// A file or link, recreated at `dst` (permissions kept, mtime NOW). A source
/// that vanished since it was listed is skipped; the verification after the
/// sync is what decides whether that mattered.
///
/// NOW, not the source's mtime (2026-09-13): `std::fs::copy` keeps the source
/// mtime on macOS, and a file copied in with different content and an older
/// mtime than the lane's last build was judged fresh by cargo — measured, the
/// build kept the old code. A file whose content is already right never comes
/// through here: [`set_aside`] keeps it, mtime and all.
fn copy_entry(snap: &Path, src: &Path, dst: &Path) -> std::io::Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(src) else {
        return Ok(());
    };
    refuse_symlinked_ancestors(snap, dst)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::symlink_metadata(dst).is_ok_and(|m| !m.is_dir()) {
        std::fs::remove_file(dst)?;
    }
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)?;
        Ok(())
    } else {
        // Write, stamp, THEN apply the source's permissions: a read-only
        // source must not stop the stamp.
        let mut from = std::fs::File::open(src)?;
        let mut to = std::fs::File::create(dst)?;
        std::io::copy(&mut from, &mut to)?;
        to.set_modified(std::time::SystemTime::now())?;
        to.set_permissions(meta.permissions())
    }
}

/// Dirty paths whose snapshot content already equals the caller's, moved into
/// `keep_dir` (a rename keeps the inode, and with it the mtime).
fn set_aside(
    snap: &Path,
    keep_dir: &Path,
    have: &TreeState,
    want: &TreeState,
) -> Vec<(String, PathState, PathBuf)> {
    let mut kept = Vec::new();
    if refuse_symlinked_dir(snap, keep_dir).is_err() || std::fs::create_dir_all(keep_dir).is_err() {
        return kept;
    }
    for (i, (p, st)) in have.dirty.iter().enumerate() {
        let settable = matches!(st, PathState::File { .. } | PathState::Symlink { .. });
        if settable && want.dirty.get(p) == Some(st) {
            let (from, aside) = (snap.join(p), keep_dir.join(i.to_string()));
            // A path beneath a link names a file outside the snapshot: never
            // moved, so the sync recreates (or refuses) it instead.
            if refuse_symlinked_ancestors(snap, &from).is_ok()
                && std::fs::rename(&from, &aside).is_ok()
            {
                kept.push((p.clone(), st.clone(), aside));
            }
        }
    }
    kept
}

/// Put each set-aside file back where the sync recreated it with the SAME
/// content; anything else is dropped and left to the verification.
fn put_back(snap: &Path, path_env: &OsStr, kept: Vec<(String, PathState, PathBuf)>) {
    if kept.is_empty() {
        return;
    }
    let paths: Vec<String> = kept.iter().map(|(p, _, _)| p.clone()).collect();
    let now = identity::path_states(snap, path_env, &paths).unwrap_or_default();
    for (p, st, aside) in kept {
        let to = snap.join(&p);
        if now.get(&p) == Some(&st) && refuse_symlinked_ancestors(snap, &to).is_ok() {
            let _ = std::fs::rename(&aside, to);
        }
    }
}

/// The exclusive hold on a snapshot.
#[derive(Debug)]
struct Lock {
    path: PathBuf,
    content: String,
}

impl Lock {
    /// Take `<dir>/lock`, atomically (a hard link of a complete file either
    /// lands or finds one already there). A lock whose pid is gone is stale
    /// and is broken; a live one is refused.
    fn acquire(dir: &Path, caller: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let path = dir.join("lock");
        let pid = std::process::id();
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let content = format!("pid {pid}\nstarted {since}\ncaller {}\n", caller.display());
        let tmp = dir.join(format!("lock.{pid}.tmp"));
        std::fs::write(&tmp, &content)
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        let taken = Self::link(dir, &tmp, &path);
        let _ = std::fs::remove_file(&tmp);
        taken.map(|()| Self { path, content })
    }

    fn link(dir: &Path, tmp: &Path, path: &Path) -> Result<(), String> {
        for _ in 0..3 {
            match std::fs::hard_link(tmp, path) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(format!("cannot take {}: {e}", path.display())),
            }
            let held = std::fs::read_to_string(path).unwrap_or_default();
            match holder_pid(&held) {
                Some(p) if pid_alive(p) => {
                    return Err(format!(
                        "the snapshot {} is held by a running gate (pid {p}) — two gates on \
                         one snapshot would sync a tree under each other's stages",
                        dir.parent().unwrap_or(dir).display()
                    ));
                }
                Some(_) => {
                    // Stale. Move it aside first, so a gate that re-took the
                    // lock in the meantime is noticed rather than deleted.
                    let aside = dir.join(format!("lock.stale.{}", std::process::id()));
                    if std::fs::rename(path, &aside).is_ok() {
                        let moved = std::fs::read_to_string(&aside).unwrap_or_default();
                        if moved != held {
                            let _ = std::fs::hard_link(&aside, path);
                        }
                        let _ = std::fs::remove_file(&aside);
                    }
                }
                None if held.is_empty() => {}
                None => {
                    return Err(format!(
                        "{} names no pid; remove it by hand if no gate is running",
                        path.display()
                    ));
                }
            }
        }
        Err(format!("{} kept changing hands; retry", path.display()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|c| c == self.content) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn holder_pid(content: &str) -> Option<u32> {
    content
        .lines()
        .find_map(|l| l.strip_prefix("pid "))
        .and_then(|p| p.trim().parse().ok())
}

/// `kill -0`, through `sh`: no libc in this crate. Our own pid is alive.
fn pid_alive(pid: u32) -> bool {
    pid == std::process::id()
        || Command::new("/bin/sh")
            .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", &pid.to_string()])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// The lane target dirs that exist in the snapshot: `target`, every
/// top-level `target-*`, and the nested workspaces' own.
fn lane_dirs(snap: &Path) -> Vec<PathBuf> {
    let mut dirs = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(snap) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            if is_dir && (name == "target" || name.starts_with("target-")) {
                dirs.insert(e.path());
            }
        }
    }
    // ONE LIST, read from where the disk preflight keeps it: these are the same
    // dirs [`crate::disk::remedy`] tells a refused operator to delete, and a
    // second copy here drifted once already (`libc-oracle/target-symgate` was
    // stamped as a lane and left out of the remedy, 2026-09-21).
    for rel in crate::disk::NESTED_LANE_DIRS {
        let p = snap.join(rel);
        // A lane beneath a link is a directory outside the snapshot: never
        // stamped, never pruned.
        if refuse_symlinked_dir(snap, &p).is_ok()
            && std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir())
        {
            dirs.insert(p);
        }
    }
    dirs.into_iter().collect()
}

/// FNV-1a: a stamp names THAT a value changed, and must not store the value.
fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A stamp's text.
#[must_use]
pub fn stamp_text(commit: Option<&str>, env: &[(String, Option<OsString>)]) -> String {
    let mut s = format!("trustc-commit {}\n", commit.unwrap_or("unknown"));
    for (k, v) in env {
        let v = v.as_ref().map_or_else(
            || "unset".to_string(),
            |v| format!("{:016x}", fnv64(v.as_encoded_bytes())),
        );
        s.push_str(&format!("env {k} {v}\n"));
    }
    s
}

fn parse_stamp(text: &str) -> (Option<String>, BTreeMap<String, String>) {
    let mut commit = None;
    let mut env = BTreeMap::new();
    for l in text.lines() {
        if let Some(c) = l.strip_prefix("trustc-commit ") {
            commit = Some(c.trim().to_string()).filter(|c| c != "unknown");
        } else if let Some(rest) = l.strip_prefix("env ")
            && let Some((k, v)) = rest.split_once(' ')
        {
            env.insert(k.to_string(), v.to_string());
        }
    }
    (commit, env)
}

/// What a lane's old stamp says about this run: `Prune` for a different
/// compiler, `Cold` naming the variables that changed, or nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneVerdict {
    Warm,
    Cold(Vec<String>),
    Prune { was: String, now: String },
}

#[must_use]
pub fn judge_stamp(old: &str, new: &str) -> LaneVerdict {
    let (old_commit, old_env) = parse_stamp(old);
    let (new_commit, new_env) = parse_stamp(new);
    if let (Some(was), Some(now)) = (old_commit, new_commit)
        && was != now
    {
        return LaneVerdict::Prune { was, now };
    }
    // A KEY IN ONLY ONE STAMP IS A ROSTER CHANGE, NOT AN ENVIRONMENT CHANGE —
    // for the roster keys, which [`stamp_text`] ALWAYS writes, recording an
    // absent variable as the literal `unset`. So a roster key that appears in
    // one stamp and not the other can only mean [`LANE_ENV_VARS`] itself
    // changed between the two runs, and a roster edit rebuilds nothing.
    // Measured 2026-09-21: dropping `CARGO_INCREMENTAL` from the roster made
    // every already-stamped lane on this machine announce `may rebuild cold —
    // CARGO_INCREMENTAL changed since its last run`, after which nothing
    // rebuilt — the exact false note the drop was made to prevent.
    //
    // The prefix keys (`CARGO_PROFILE_*`) are the opposite: they are written
    // only when SET, so one appearing or disappearing IS the variable being set
    // or unset, and it still counts.
    let roster = |k: &String| !k.starts_with(LANE_ENV_PREFIX);
    let in_both = |k: &String| old_env.contains_key(k) && new_env.contains_key(k);
    let changed: Vec<String> = old_env
        .keys()
        .chain(new_env.keys())
        .filter(|k| in_both(k) || !roster(k))
        .filter(|k| old_env.get(*k) != new_env.get(*k))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if changed.is_empty() {
        LaneVerdict::Warm
    } else {
        LaneVerdict::Cold(changed)
    }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

/// Stamp every lane, pruning the ones a new compiler made unreusable. Returns
/// the header notes and the background deletion to wait on.
fn stamp_lanes(
    snap: &Path,
    env: &[(String, Option<OsString>)],
    commit: Option<&str>,
) -> (Vec<String>, Option<Child>) {
    let new = stamp_text(commit, env);
    let trash = snap.join(GATE_STATE_DIR).join("trash");
    let trash_ok = refuse_symlinked_dir(snap, &trash).is_ok();
    let mut notes = Vec::new();
    for (n, dir) in lane_dirs(snap).into_iter().enumerate() {
        let rel = dir.strip_prefix(snap).unwrap_or(&dir).display().to_string();
        if let Ok(old) = std::fs::read_to_string(dir.join(STAMP_FILE)) {
            match judge_stamp(&old, &new) {
                LaneVerdict::Warm => {}
                LaneVerdict::Cold(vars) => notes.push(format!(
                    "verify: lane {rel} may rebuild cold — {} changed since its last run",
                    vars.join(", ")
                )),
                LaneVerdict::Prune { was, now } => {
                    let dest = trash.join(format!(
                        "{}.{}.{n}",
                        rel.replace('/', "_"),
                        std::process::id()
                    ));
                    let moved = trash_ok
                        && std::fs::create_dir_all(&trash).is_ok()
                        && std::fs::rename(&dir, &dest).is_ok();
                    if moved {
                        let _ = std::fs::create_dir_all(&dir);
                        notes.push(format!(
                            "verify: lane {rel} pruned — trustc {} -> {}, so none of its \
                             artifacts could be reused",
                            short(&was),
                            short(&now)
                        ));
                    }
                }
            }
        }
        let _ = std::fs::write(dir.join(STAMP_FILE), &new);
    }
    // `rm -rf` through a linked trash dir would delete whatever it points at.
    let deleting = if trash_ok {
        delete_in_background(&trash)
    } else {
        None
    };
    (notes, deleting)
}

/// `taskpolicy -b rm -rf` every entry of `trash` (plain `rm -rf` where there is
/// no taskpolicy): background QoS, so a pruned lane's gigabytes never compete
/// with the stages for the disk.
fn delete_in_background(trash: &Path) -> Option<Child> {
    let entries: Vec<PathBuf> = std::fs::read_dir(trash)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    if entries.is_empty() {
        return None;
    }
    let taskpolicy = Path::new("/usr/sbin/taskpolicy");
    let mut cmd = if taskpolicy.exists() {
        let mut c = Command::new(taskpolicy);
        c.args(["-b", "rm", "-rf"]);
        c
    } else {
        let mut c = Command::new("rm");
        c.arg("-rf");
        c
    };
    cmd.args(&entries)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_snapshot_sits_beside_the_checkout_and_is_never_indexed() {
        assert_eq!(
            default_root(Path::new("/Users//x/aterm")),
            PathBuf::from("/Users//x/aterm-verify.noindex")
        );
    }

    #[test]
    fn the_lane_environment_is_the_named_list_plus_every_profile_override() {
        let env = lane_env([
            (OsString::from("RUSTFLAGS"), OsString::from("-Zx")),
            (
                OsString::from("CARGO_PROFILE_DEV_DEBUG"),
                OsString::from("1"),
            ),
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("RUSTDOC"), OsString::from("/s2/trustdoc")),
        ]);
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names.len(), LANE_ENV_VARS.len() + 1);
        assert!(names.contains(&"CARGO_PROFILE_DEV_DEBUG"));
        assert!(!names.contains(&"PATH") && !names.contains(&"RUSTDOC"));
        assert_eq!(
            env[0],
            ("RUSTFLAGS".to_string(), Some(OsString::from("-Zx")))
        );
        assert!(env.iter().any(|(k, v)| k == "CC" && v.is_none()));
    }

    #[test]
    fn a_stamp_names_a_changed_variable_without_storing_its_value() {
        let a = lane_env([(OsString::from("RUSTFLAGS"), OsString::from("secret-ish"))]);
        let b = lane_env([(OsString::from("RUSTFLAGS"), OsString::from("other"))]);
        let old = stamp_text(Some("c1"), &a);
        assert!(!old.contains("secret-ish"), "{old}");
        assert_eq!(judge_stamp(&old, &old), LaneVerdict::Warm);
        assert_eq!(
            judge_stamp(&old, &stamp_text(Some("c1"), &b)),
            LaneVerdict::Cold(vec!["RUSTFLAGS".to_string()])
        );
        assert_eq!(
            judge_stamp(&old, &stamp_text(Some("c2"), &b)),
            LaneVerdict::Prune {
                was: "c1".to_string(),
                now: "c2".to_string()
            },
            "a new compiler outranks an env change"
        );
        assert_eq!(
            judge_stamp(&old, &stamp_text(None, &a)),
            LaneVerdict::Warm,
            "an unknown commit is not evidence of a new compiler"
        );
    }

    /// A ROSTER EDIT REBUILDS NOTHING, so it must not announce a cold rebuild.
    ///
    /// [`stamp_text`] writes every roster key on every run, an absent variable
    /// as the literal `unset`, so a roster key in one stamp and not the other
    /// can only mean [`LANE_ENV_VARS`] itself changed. Measured 2026-09-21:
    /// dropping `CARGO_INCREMENTAL` from the roster made every already-stamped
    /// lane here print `may rebuild cold — CARGO_INCREMENTAL changed since its
    /// last run`, and then nothing rebuilt. The prefix keys are the opposite
    /// case and still count, because they are written only when set.
    #[test]
    fn dropping_a_variable_from_the_roster_is_not_a_cold_lane() {
        let env = lane_env([(OsString::from("RUSTFLAGS"), OsString::from("-Zx"))]);
        let new = stamp_text(Some("c1"), &env);
        // The same run, stamped by a client whose roster still carried one more
        // variable: an extra `env` line, everything else identical.
        let old = new.replace(
            "env RUSTFLAGS",
            "env CARGO_INCREMENTAL unset\nenv RUSTFLAGS",
        );
        assert!(old.contains("CARGO_INCREMENTAL") && !new.contains("CARGO_INCREMENTAL"));
        assert_eq!(
            judge_stamp(&old, &new),
            LaneVerdict::Warm,
            "a key the new roster does not write is a roster change, not an env change"
        );
        assert_eq!(
            judge_stamp(&new, &old),
            LaneVerdict::Warm,
            "and symmetrically"
        );
        // A PROFILE key is written only when set, so its appearance is real.
        let with_profile = lane_env([
            (OsString::from("RUSTFLAGS"), OsString::from("-Zx")),
            (
                OsString::from("CARGO_PROFILE_DEV_DEBUG"),
                OsString::from("1"),
            ),
        ]);
        assert_eq!(
            judge_stamp(&new, &stamp_text(Some("c1"), &with_profile)),
            LaneVerdict::Cold(vec!["CARGO_PROFILE_DEV_DEBUG".to_string()]),
            "setting a profile variable still rebuilds the lane"
        );
    }

    /// Unix-pinned at the test, not at the guard: `refuse_symlinked_ancestors`
    /// itself is portable (`symlink_metadata().file_type().is_symlink()` answers
    /// on every target), but PLANTING the two links the law is about needs
    /// `std::os::unix::fs::symlink`, whose Windows counterparts are two
    /// different functions behind a privilege the test runner may not hold.
    #[cfg(unix)]
    #[test]
    fn a_write_beneath_a_symlink_in_the_snapshot_is_refused() {
        let base = crate::mktemp_dir("atv-snap-guard").expect("mktemp");
        let snap = base.join("snap");
        std::fs::create_dir_all(snap.join("real/dir")).expect("mkdir");
        std::fs::create_dir_all(base.join("outside")).expect("mkdir");
        std::os::unix::fs::symlink(base.join("outside"), snap.join("link")).expect("link");
        std::os::unix::fs::symlink("../outside", snap.join("real/rel")).expect("link");

        assert!(refuse_symlinked_ancestors(&snap, &snap.join("real/dir/new/f")).is_ok());
        assert!(
            refuse_symlinked_ancestors(&snap, &snap.join("link")).is_ok(),
            "the link itself is replaced, not written through"
        );
        for dst in ["link/x", "link/deeper/x", "real/rel/x"] {
            let err = refuse_symlinked_ancestors(&snap, &snap.join(dst)).expect_err(dst);
            assert!(err.to_string().contains("symbolic link"), "{dst}: {err}");
        }
        assert!(refuse_symlinked_dir(&snap, &snap.join("link")).is_err());
        assert!(refuse_symlinked_ancestors(&snap, &base.join("outside/x")).is_err());

        // copy_entry refuses before it creates or writes anything.
        std::fs::write(base.join("src"), "caller\n").expect("src");
        assert!(copy_entry(&snap, &base.join("src"), &snap.join("link/sub/x")).is_err());
        assert_eq!(
            std::fs::read_dir(base.join("outside"))
                .expect("dir")
                .count(),
            0
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_lock_names_its_pid_and_a_garbled_one_is_not_silently_broken() {
        assert_eq!(holder_pid("pid 42\nstarted 1\n"), Some(42));
        assert_eq!(holder_pid("garbage"), None);
        assert!(pid_alive(std::process::id()));
    }
}
