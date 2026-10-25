// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RETIRED VERIFY GATE'S LEFTOVERS (`docs/DESIGN-adaptive-build-2026-10-24.md`, DISK-ADMIT).
//!
//! Until 42e8a9a273 (2026-10-15) `aterm-verify` built a snapshot of the caller's tree in a git
//! worktree beside the main checkout, `<checkout>-verify.noindex`, type-checked the foreign
//! cells into `<checkout>-verify-cells.noindex`, and queued runs on a per-user machine lock,
//! `aterm-verify` in the user's cache. The rewrite deleted that code and not what it wrote:
//! 34 GiB on the Mac it ran on, which no code reads, and a worktree git still lists.
//!
//! [`plan`] finds them by the gate's own names and judges each; [`retire`] removes those that
//! go, once, from `aterm pkg machine apply`. The snapshot goes through `git worktree remove`,
//! and only while it is a worktree of that checkout, detached at a commit `origin/main`
//! contains, clean (no change and no untracked file, its submodules' included), not locked, and
//! nothing in it written for [`UNWRITTEN`]; git's own bookkeeping inside it (a submodule's git
//! directory) is not a write. The cells lane goes with its snapshot, and the lock directory once
//! nobody holds its lock, each unwritten as long.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Nothing in a leftover was written for this long before it goes.
pub const UNWRITTEN: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The gate's snapshot of `<checkout>`: `<checkout>-verify.noindex`.
const SNAPSHOT: &str = "-verify.noindex";

/// The snapshot's cells lane: `<checkout>-verify-cells.noindex`.
const CELLS: &str = "-verify-cells.noindex";

/// The machine lock's directory in the user's cache.
const LOCK_DIR: &str = "aterm-verify";

/// What one leftover was to the gate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The snapshot worktree of the main checkout `checkout`.
    Snapshot {
        /// The main checkout.
        checkout: PathBuf,
    },
    /// The snapshot's foreign cells lane.
    Cells,
    /// The machine lock's directory.
    Lock,
}

/// One leftover and what a pass does with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leftover {
    /// Where it is.
    pub path: PathBuf,
    /// What it was.
    pub kind: Kind,
    /// `Ok(why it goes)` or `Err(why it stays)`.
    pub verdict: Result<String, String>,
}

impl Leftover {
    /// Whether a pass removes it.
    #[must_use]
    pub const fn goes(&self) -> bool {
        self.verdict.is_ok()
    }

    /// Its line under `aterm pkg machine cache`.
    #[must_use]
    pub fn words(&self) -> String {
        match &self.verdict {
            Ok(why) => format!("goes: {why}"),
            Err(why) => format!("stays: {why}"),
        }
    }
}

/// What git answered: its exit code (`None`: no code, or it did not start) and its output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// Its exit code.
    pub code: Option<i32>,
    /// Its standard output.
    pub out: String,
    /// Its standard error, or why it did not start.
    pub err: String,
}

/// How git is asked: the live one ([`GitCli`]) in production, the same in tests on scratch repos.
pub trait Git {
    /// `git -C <dir> <args>`, with git's optional locks off so no read writes its index.
    fn run(&self, dir: &Path, args: &[&str]) -> Answer;
}

/// `git` on `PATH`.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitCli;

impl Git for GitCli {
    fn run(&self, dir: &Path, args: &[&str]) -> Answer {
        let out = aterm_uds::spawnfd::command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output();
        match out {
            Ok(o) => Answer {
                code: o.status.code(),
                out: String::from_utf8_lossy(&o.stdout).into_owned(),
                err: String::from_utf8_lossy(&o.stderr).trim().to_owned(),
            },
            Err(e) => Answer {
                code: None,
                out: String::new(),
                err: format!("git did not start: {e}"),
            },
        }
    }
}

/// Where the gate's leftovers are looked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    /// The home whose children may be checkouts with a snapshot beside them.
    pub home: PathBuf,
    /// The user's cache, where the gate kept its machine lock.
    pub cache: PathBuf,
    /// The build cache's workspaces, which may be checkouts with a snapshot beside them.
    pub workspaces: Vec<PathBuf>,
}

impl Roots {
    /// This user's: the home, its cache as the gate placed its lock (`~/Library/Caches` on
    /// macOS, else `$XDG_CACHE_HOME` or `~/.cache`), and the build cache's workspaces.
    #[must_use]
    pub fn live() -> Option<Roots> {
        let home = aterm_types::dirs::home_dir().filter(|h| !h.as_os_str().is_empty())?;
        let cache = if cfg!(target_os = "macos") {
            home.join("Library/Caches")
        } else {
            std::env::var_os("XDG_CACHE_HOME")
                .filter(|c| !c.is_empty())
                .map_or_else(|| home.join(".cache"), PathBuf::from)
        };
        let workspaces = super::workspaces(&super::cache_root(&home));
        Some(Roots {
            home,
            cache,
            workspaces,
        })
    }
}

/// Whether `dir` is a main checkout: it holds a `.git` directory.
fn main_checkout(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok_and(|m| m.is_dir())
}

/// `checkout`'s sibling named `<checkout's name><suffix>`.
fn beside(checkout: &Path, suffix: &str) -> Option<PathBuf> {
    let mut name = checkout.file_name()?.to_os_string();
    name.push(suffix);
    Some(checkout.with_file_name(name))
}

/// The main checkouts whose snapshot may be on disk: `home`'s own children, and `workspaces`
/// (the build cache's), each with a `<name>-verify.noindex` or `-verify-cells.noindex` beside it.
fn checkouts(home: &Path, workspaces: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            let stem = name
                .strip_suffix(CELLS)
                .or_else(|| name.strip_suffix(SNAPSHOT))?;
            Some(home.join(stem))
        })
        .chain(workspaces.iter().cloned())
        .filter(|c| main_checkout(c))
        .filter(|c| {
            [SNAPSHOT, CELLS]
                .iter()
                .filter_map(|s| beside(c, s))
                .any(|p| p.symlink_metadata().is_ok())
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The first file or directory under `dir` (itself included) written at or after `since`, with
/// git's own directories inside it passed over (a `.git` and any directory laid out as one, as a
/// submodule's is): git writes its bookkeeping on a read. `None` when nothing was.
#[must_use]
pub fn written_since(dir: &Path, since: SystemTime) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&at) else {
            continue;
        };
        let children: Vec<std::fs::DirEntry> = if meta.is_dir() {
            std::fs::read_dir(&at)
                .map(|entries| entries.flatten().collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let named = |n: &str| children.iter().any(|e| e.file_name() == n);
        if at != dir && named("HEAD") && named("objects") && named("refs") {
            continue;
        }
        if meta.modified().is_ok_and(|m| m >= since) {
            return Some(at);
        }
        stack.extend(
            children
                .iter()
                .filter(|e| e.file_name() != ".git")
                .map(std::fs::DirEntry::path),
        );
    }
    None
}

/// `d` as a person reads an age: `5h`, `13d`.
fn age(d: Duration) -> String {
    let h = d.as_secs() / 3600;
    if h < 48 {
        format!("{h}h")
    } else {
        format!("{}d", h / 24)
    }
}

/// Why `path` stays for a write since `since` (`None`: nothing written, it may go), at `now`.
fn written_words(path: &Path, since: SystemTime, now: SystemTime) -> Option<String> {
    let hit = written_since(path, since)?;
    let when = std::fs::symlink_metadata(&hit)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| now.duration_since(m).ok())
        .map_or_else(|| "just now".to_owned(), |d| format!("{} ago", age(d)));
    Some(format!(
        "{} was written {when}; it goes once nothing in it is written for {}",
        hit.display(),
        age(UNWRITTEN)
    ))
}

/// One worktree's entry in `git worktree list --porcelain`.
#[derive(Default)]
struct Listed {
    head: String,
    detached: bool,
    locked: bool,
    branch: Option<String>,
}

/// The entry `git worktree list --porcelain` in `checkout` gives `wt`.
fn listed(git: &dyn Git, checkout: &Path, wt: &Path) -> Result<Option<Listed>, String> {
    let a = git.run(checkout, &["worktree", "list", "--porcelain"]);
    if a.code != Some(0) {
        return Err(format!("git worktree list failed: {}", a.err));
    }
    let want = std::fs::canonicalize(wt).unwrap_or_else(|_| wt.to_path_buf());
    for block in a.out.split("\n\n") {
        let mut lines = block.lines();
        let Some(path) = lines.next().and_then(|l| l.strip_prefix("worktree ")) else {
            continue;
        };
        let at = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
        if at != want {
            continue;
        }
        let mut entry = Listed::default();
        for line in lines {
            if let Some(h) = line.strip_prefix("HEAD ") {
                entry.head = h.to_owned();
            } else if line == "detached" {
                entry.detached = true;
            } else if line == "locked" || line.starts_with("locked ") {
                entry.locked = true;
            } else if let Some(b) = line.strip_prefix("branch ") {
                entry.branch = Some(b.to_owned());
            }
        }
        return Ok(Some(entry));
    }
    Ok(None)
}

/// The verdict on the snapshot `wt` of `checkout` at `now`.
fn judge_snapshot(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
) -> Result<String, String> {
    let entry = listed(git, checkout, wt)?.ok_or_else(|| {
        format!(
            "git does not list it as a worktree of {}",
            checkout.display()
        )
    })?;
    if entry.locked {
        return Err("git worktree list has it locked".to_owned());
    }
    if !entry.detached {
        return Err(format!(
            "it is on {}, not detached as the gate left it",
            entry.branch.as_deref().unwrap_or("a branch")
        ));
    }
    let short = entry.head.get(..10).unwrap_or(&entry.head).to_owned();
    let main = git.run(
        checkout,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/remotes/origin/main",
        ],
    );
    if main.code != Some(0) {
        return Err(format!(
            "{} has no origin/main to hold {short} to",
            checkout.display()
        ));
    }
    let ancestor = git.run(
        checkout,
        &[
            "merge-base",
            "--is-ancestor",
            &entry.head,
            "refs/remotes/origin/main",
        ],
    );
    match ancestor.code {
        Some(0) => {}
        Some(1) => return Err(format!("{short} is not in origin/main")),
        _ => {
            return Err(format!(
                "git could not say whether {short} is in origin/main: {}",
                ancestor.err
            ));
        }
    }
    let status = git.run(
        wt,
        &[
            "status",
            "--porcelain",
            "--ignore-submodules=none",
            "--untracked-files=normal",
        ],
    );
    if status.code != Some(0) {
        return Err(format!("git status failed in it: {}", status.err));
    }
    let changed: Vec<&str> = status
        .out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if let Some(first) = changed.first() {
        return Err(format!(
            "it has {} uncommitted change(s) ({}{})",
            changed.len(),
            first.trim(),
            if changed.len() > 1 { ", …" } else { "" }
        ));
    }
    if let Some(why) = now
        .checked_sub(UNWRITTEN)
        .and_then(|since| written_words(wt, since, now))
    {
        return Err(why);
    }
    Ok(format!(
        "the retired verify gate's snapshot of {} (its code went in 42e8a9a273): a clean worktree \
         detached at {short}, which origin/main contains, nothing in it written in the last {}",
        checkout.display(),
        age(UNWRITTEN)
    ))
}

/// Whether a process holds the gate's machine lock in `dir`: `Some(true)` held, `Some(false)` free
/// or no lock file, `None` unreadable.
fn lock_held(dir: &Path) -> Option<bool> {
    let file = match std::fs::File::open(dir.join("lock")) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(false),
        Err(_) => return None,
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "a probe of the retired gate's lock: let go at once below"
    )]
    let tried = file.try_lock();
    match tried {
        Ok(()) => {
            let _ = file.unlock();
            Some(false)
        }
        Err(std::fs::TryLockError::WouldBlock) => Some(true),
        Err(std::fs::TryLockError::Error(_)) => None,
    }
}

/// Every leftover of the retired gate under `roots` at `now`, each judged: the snapshots and
/// cells lanes of the home's checkouts and of the build cache's workspaces, and the lock
/// directory. Read only: git is asked with its optional locks off, and nothing is written.
#[must_use]
pub fn plan(roots: &Roots, now: SystemTime, git: &dyn Git) -> Vec<Leftover> {
    let since = now.checked_sub(UNWRITTEN).unwrap_or(SystemTime::UNIX_EPOCH);
    let mut out = Vec::new();
    for checkout in checkouts(&roots.home, &roots.workspaces) {
        let snapshot = beside(&checkout, SNAPSHOT);
        let mut snapshot_gone = true;
        if let Some(wt) = snapshot.filter(|p| p.symlink_metadata().is_ok()) {
            let verdict = judge_snapshot(git, &checkout, &wt, now);
            snapshot_gone = verdict.is_ok();
            out.push(Leftover {
                path: wt,
                kind: Kind::Snapshot {
                    checkout: checkout.clone(),
                },
                verdict,
            });
        }
        if let Some(cells) = beside(&checkout, CELLS).filter(|p| p.is_dir()) {
            let verdict = if !snapshot_gone {
                Err("its snapshot stays, and it goes with it".to_owned())
            } else if let Some(why) = written_words(&cells, since, now) {
                Err(why)
            } else {
                Ok(format!(
                    "the retired verify gate's foreign cells lane of {}, nothing in it written \
                     in the last {}",
                    checkout.display(),
                    age(UNWRITTEN)
                ))
            };
            out.push(Leftover {
                path: cells,
                kind: Kind::Cells,
                verdict,
            });
        }
    }
    let lock = roots.cache.join(LOCK_DIR);
    if lock.is_dir() {
        let verdict = match lock_held(&lock) {
            Some(true) => Err("a process holds its lock".to_owned()),
            None => Err("its lock could not be tried".to_owned()),
            Some(false) => match written_words(&lock, since, now) {
                Some(why) => Err(why),
                None => Ok(format!(
                    "the retired verify gate's machine lock, which nothing holds, nothing in it \
                     written in the last {}",
                    age(UNWRITTEN)
                )),
            },
        };
        out.push(Leftover {
            path: lock,
            kind: Kind::Lock,
            verdict,
        });
    }
    out
}

/// Remove each leftover [`plan`] says goes, judged again just before: the snapshot by `git
/// worktree remove --force` in its checkout (git refuses a worktree with a populated submodule
/// otherwise, and the plan has proven it clean; one `--force` still refuses a locked worktree,
/// so a lock taken since the plan keeps it), the rest by removing the directory.
/// Each leftover with what became of it: `Ok(())` removed, `Err(why)` kept.
pub fn retire(
    roots: &Roots,
    now: SystemTime,
    git: &dyn Git,
) -> Vec<(Leftover, Result<(), String>)> {
    let mut done = Vec::new();
    let mut kept_snapshot = false;
    for left in plan(roots, now, git) {
        let result = match (&left.verdict, &left.kind) {
            (Err(why), _) => Err(why.clone()),
            (Ok(_), Kind::Snapshot { checkout }) => {
                let path = left.path.to_string_lossy().into_owned();
                let a = git.run(checkout, &["worktree", "remove", "--force", &path]);
                let removed = a.code == Some(0) && left.path.symlink_metadata().is_err();
                kept_snapshot = !removed;
                if removed {
                    Ok(())
                } else {
                    Err(format!("git worktree remove failed: {}", a.err))
                }
            }
            (Ok(_), Kind::Cells) if kept_snapshot => {
                Err("its snapshot could not be removed, and it goes with it".to_owned())
            }
            (Ok(_), Kind::Cells | Kind::Lock) => {
                std::fs::remove_dir_all(&left.path).map_err(|e| e.to_string())
            }
        };
        done.push((left, result));
    }
    done
}

#[cfg(test)]
#[path = "build_cache_retired_tests.rs"]
mod tests;
