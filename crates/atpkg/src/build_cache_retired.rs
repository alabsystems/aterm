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
//! go, once, from `aterm pkg machine apply`. The snapshot goes only while it is a worktree of
//! that checkout, detached at a commit `origin/main` contains, clean (no change and no untracked
//! file, its submodules' included), not locked, and nothing in it written for [`UNWRITTEN`];
//! git's own bookkeeping inside it (a submodule's git directory) is not a write. A snapshot half
//! removed (its `.git` names the checkout's worktree entry for it, which a removal that stopped
//! part way took: git's `worktree remove` drops the entry even when it cannot delete the tree)
//! goes once no file in it is written for as long; its folders' own times are the removal's.
//! Its `.git` goes first, and while that stays nothing else is touched; then its tree goes
//! through the build cache's remover, which no one entry stops, and git's entry for it, and that
//! entry alone, goes after it ([`remove_snapshot`]): no other worktree's entry is touched. The
//! cells lane goes with its snapshot, and the lock directory once nobody holds its lock, each
//! unwritten as long. Removal also checks process use of each leftover and the snapshot's own
//! git entry; a running image, working directory, open file or unreadable owner keeps it.
//! A durable receipt binds an admitted snapshot and its Git entry until both are gone. A
//! missing tree authorizes metadata cleanup only after the remover recorded its whole removal;
//! a merely prepared tree might have been moved. New source or Git metadata writes keep either
//! phase, and a changed clock cannot turn a future receipt into authority for cleanup.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[path = "build_cache_retired_pending.rs"]
mod pending;

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

/// One leftover and its observed cleanup eligibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leftover {
    /// Where it is.
    pub path: PathBuf,
    /// What it was.
    pub kind: Kind,
    /// `Ok(why it is a candidate)` or `Err(why it is kept at this observation)`.
    pub verdict: Result<String, String>,
}

impl Leftover {
    /// Whether this observation nominates it for cleanup, subject to later revalidation.
    #[must_use]
    pub const fn goes(&self) -> bool {
        self.verdict.is_ok()
    }

    /// Its observed eligibility, shown by the preview and alongside cleanup outcomes.
    #[must_use]
    pub fn words(&self) -> String {
        match &self.verdict {
            Ok(why) => format!("eligible when checked: {why}"),
            Err(why) => format!("kept when checked: {why}"),
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
                .or_else(|| name.strip_suffix(SNAPSHOT))
                .or_else(|| name.strip_suffix(pending::SUFFIX))?;
            Some(home.join(stem))
        })
        .chain(workspaces.iter().cloned())
        .filter(|c| main_checkout(c))
        .filter(|c| {
            [SNAPSHOT, CELLS, pending::SUFFIX]
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
    written(dir, since, Times::All)
}

/// Whose own modification times say a tree was written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Times {
    /// Every entry's, its folders' included.
    All,
    /// Every entry's but its folders': the remains of a removal that stopped part way, whose
    /// folders the removal itself rewrote (each one it took an entry from, and the snapshot's own
    /// when its `.git` was written back), so only a file, a link or a new folder's contents say
    /// someone wrote it since.
    NotFolders,
}

/// [`written_since`], the entries whose times count as `times` says.
fn written(dir: &Path, since: SystemTime, times: Times) -> Option<PathBuf> {
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
        let counted = times == Times::All || !meta.is_dir();
        if counted && meta.modified().is_ok_and(|m| m >= since) {
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

/// Why `path` stays for a write since `since` (`None`: nothing written, it may go), at `now`,
/// the entries whose times count as `times` says.
fn written_words(path: &Path, since: SystemTime, now: SystemTime, times: Times) -> Option<String> {
    let hit = written(path, since, times)?;
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

/// The bytes of `gitfile`, a worktree's `.git`, while it is a file as git writes one (a line
/// naming its entry): `None` for a folder, a link, a file too long to be one, or none.
fn gitfile_text(gitfile: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(gitfile).ok()?;
    if !meta.is_file() || meta.len() > 4096 {
        return None;
    }
    std::fs::read(gitfile).ok()
}

/// The worktree entry of `checkout`, `<checkout>/.git/worktrees/<id>`, that `text`, the tree
/// `wt`'s `.git` (a relative name read from `wt`), names, whether or not that entry is there.
/// `None` when it names none of `checkout`'s: another repository's, or no `gitdir:` line.
fn entry_named(checkout: &Path, wt: &Path, text: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(text).ok()?;
    let named = Path::new(text.lines().next()?.strip_prefix("gitdir:")?.trim());
    let entry = if named.is_absolute() {
        named.to_path_buf()
    } else {
        wt.join(named)
    };
    let id = entry.file_name()?;
    let worktrees = entry.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = std::fs::canonicalize(worktrees.parent()?).ok()?;
    (common == std::fs::canonicalize(checkout.join(".git")).ok()?)
        .then(|| common.join("worktrees").join(id))
}

/// Git's entry for the worktree `wt` of `checkout`, as `text`, `wt`'s `.git`, names it, while
/// that entry is there and is `wt`'s: its own `gitdir` file names `wt`'s `.git` (an id git gave
/// another worktree since is not `wt`'s). `None` for anything else.
fn own_entry(checkout: &Path, wt: &Path, text: &[u8]) -> Option<PathBuf> {
    let entry = entry_named(checkout, wt, text)?;
    let back = gitfile_text(&entry.join("gitdir"))?;
    let back = std::str::from_utf8(&back)
        .ok()?
        .lines()
        .next()?
        .trim()
        .to_owned();
    let back = Path::new(&back);
    let back = if back.is_absolute() {
        back.to_path_buf()
    } else {
        entry.join(back)
    };
    let names_wt = back.file_name()? == ".git"
        && std::fs::canonicalize(back.parent()?).ok()? == std::fs::canonicalize(wt).ok()?;
    names_wt.then_some(entry)
}

/// The worktree entry of `checkout` (`<checkout>/.git/worktrees/<id>`) that `wt`'s `.git` file
/// names, when that entry is gone: `wt` is then a snapshot half removed, by a removal that
/// stopped part way after the entry went (git's `worktree remove --force` deletes the entry even
/// when it could not delete the tree; [`remove_snapshot`] drops it, and writes the `.git` back,
/// when an entry of the tree stays). `None` for anything else: no `.git` file, one naming
/// another repository's entry, or an entry still there.
fn half_removed(checkout: &Path, wt: &Path) -> Option<PathBuf> {
    let entry = entry_named(checkout, wt, &gitfile_text(&wt.join(".git"))?)?;
    match std::fs::symlink_metadata(&entry) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(entry),
        _ => None,
    }
}

/// Why a snapshot git does not list, and that is not half removed, stays.
fn unlisted(checkout: &Path) -> String {
    format!(
        "git does not list it as a worktree of {}",
        checkout.display()
    )
}

/// Why a snapshot git has locked stays.
const LOCKED: &str = "git worktree list has it locked";

/// The shared process probe could not establish that the leftover is unused.
const USED: &str = "active process use could not be ruled out";

/// Why `git status` could not read a snapshot, as far as git's own words say.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Unreadable {
    /// The submodule git failed in (`failed in submodule <path>`), when it names one.
    submodule: Option<String>,
    /// An object store an `objects/info/alternates` entry names, which is not on disk: the
    /// objects git borrowed from it are gone with it.
    missing_store: Option<PathBuf>,
    /// Git's first line, or that it said nothing.
    first: String,
}

impl Unreadable {
    /// What `err`, git's standard error, says, its object store read on disk.
    fn read(err: &str) -> Unreadable {
        let mut found = Unreadable {
            submodule: None,
            missing_store: None,
            first: err
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("git said nothing")
                .to_owned(),
        };
        for line in err.lines().map(str::trim) {
            if let Some((_, sub)) = line.split_once("failed in submodule ") {
                found.submodule = Some(sub.trim().to_owned());
            }
            let store = line
                .split_once("unable to normalize alternate object path: ")
                .map(|(_, p)| p)
                .or_else(|| {
                    line.split_once("object directory ")
                        .and_then(|(_, rest)| rest.split_once(" does not exist"))
                        .map(|(p, _)| p)
                })
                .map(|p| PathBuf::from(p.trim()));
            if let Some(store) = store.filter(|p| std::fs::symlink_metadata(p).is_err()) {
                found.missing_store.get_or_insert(store);
            }
        }
        found
    }

    /// Why the snapshot stays, and what a person does about it.
    fn words(&self) -> String {
        let Some(store) = &self.missing_store else {
            return format!(
                "git status cannot read it ({}), so nothing proves it holds no work: it stays \
                 until a person repairs it, after which the next apply judges it, or removes it",
                self.first
            );
        };
        let (who, whose) = match &self.submodule {
            Some(sub) => (format!("its submodule {sub}"), "that submodule's"),
            None => ("it".to_owned(), "its"),
        };
        format!(
            "git status cannot read it: {who} borrows objects from {}, which is gone (an \
             objects/info/alternates entry naming a removed object store, as a removed \
             worktree's is), so nothing proves it holds no work: it stays until a person repairs \
             {whose} objects/info/alternates, after which the next apply judges it, or removes it",
            store.display()
        )
    }
}

/// The verdict on the snapshot `wt` of `checkout` at `now`.
fn judge_snapshot(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
) -> Result<String, String> {
    if let Some(removal) = pending::read_at(checkout, wt, now)? {
        if removal.entry.join("locked").symlink_metadata().is_ok() {
            return Err(LOCKED.to_owned());
        }
        if let Some(entry) = listed(git, checkout, wt)? {
            if entry.locked {
                return Err(LOCKED.to_owned());
            }
            if !entry.detached || entry.head != removal.head {
                return Err("snapshot HEAD changed after removal began".to_owned());
            }
        }
        in_main(git, checkout, &removal.head)?;
        return Ok(
            "the exact snapshot's interrupted removal, bound by its durable receipt".to_owned(),
        );
    }
    let Some(entry) = listed(git, checkout, wt)? else {
        let Some(gone) = half_removed(checkout, wt) else {
            return Err(unlisted(checkout));
        };
        if let Some(why) = now
            .checked_sub(UNWRITTEN)
            .and_then(|since| written_words(wt, since, now, Times::NotFolders))
        {
            return Err(why);
        }
        return Ok(format!(
            "the retired verify gate's snapshot of {} (its code went in 42e8a9a273), half \
             removed: a removal that stopped part way took git's entry for it ({}), which its \
             .git still names, no file in it written in the last {}",
            checkout.display(),
            gone.display(),
            age(UNWRITTEN)
        ));
    };
    if entry.locked {
        return Err(LOCKED.to_owned());
    }
    if !entry.detached {
        return Err(format!(
            "it is on {}, not detached as the gate left it",
            entry.branch.as_deref().unwrap_or("a branch")
        ));
    }
    let short = in_main(git, checkout, &entry.head)?;
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
        return Err(Unreadable::read(&status.err).words());
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
        .and_then(|since| written_words(wt, since, now, Times::All))
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

fn in_main(git: &dyn Git, checkout: &Path, head: &str) -> Result<String, String> {
    let short = head.get(..10).unwrap_or(head).to_owned();
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
            head,
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
    Ok(short)
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
        if let Some(wt) = snapshot.filter(|p| {
            p.symlink_metadata().is_ok()
                || beside(&checkout, pending::SUFFIX).is_some_and(|r| r.symlink_metadata().is_ok())
        }) {
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
            } else if let Some(why) = written_words(&cells, since, now, Times::All) {
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
            Some(false) => match written_words(&lock, since, now, Times::All) {
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

/// REMOVE THE SNAPSHOT `wt` OF `checkout`, which the plan judged goes, and git's entry for it
/// alone. Git is asked again just before: a lock taken since the plan keeps it, and so does a
/// `.git` that no longer names git's entry for it (or, for a snapshot git does not list, one
/// that is not half removed). Its `.git` goes first (through [`super::remove::tree`]): while that
/// stays, nothing else of it is touched, so a snapshot git lists is never left gutted and still
/// listed. Then its tree goes through the remover, which no one entry stops, and then git's entry
/// for it: by `git worktree remove --force` once the tree went whole (git drops that one entry
/// of a tree that is gone, and refuses a locked one); by the remover, after its `locked` file is
/// looked for, when an entry of the tree stayed (git will not drop the entry of a tree that has
/// no `.git` and is still there). No other worktree's entry is touched: `git worktree prune`
/// would drop every one whose tree is away, and the submodules' repositories under it, with what
/// they hold that was never pushed. `Ok(())` once the tree is gone and git no longer lists it.
/// When an entry of the tree stays, the `Err` is how many stayed and the first of them
/// ([`super::remove::stayed_words`]), and the `.git` is written back as it was. A durable
/// identity-bound receipt lets a later pass resume even when the tree went but a held or
/// locked Git entry stayed. It admits no newly written source or Git metadata. Older snapshots
/// half removed without a receipt retain their existing [`half_removed`] age check.
/// Process use of the tree or its own git entry is checked before the first removal, and
/// again before dropping that entry after the tree: an entry acquired meanwhile stays.
fn remove_snapshot(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
    fence: &super::fence::Fence<'_>,
) -> Result<(), String> {
    let mut recovery = pending::read(checkout, wt)?;
    judge_snapshot(git, checkout, wt, now)?;
    let now_listed = listed(git, checkout, wt)?;
    if now_listed.as_ref().is_some_and(|entry| entry.locked) {
        return Err(LOCKED.to_owned());
    }
    let gitfile = wt.join(".git");
    let Some(text) = recovery
        .as_ref()
        .map(|removal| removal.gitfile.clone())
        .or_else(|| gitfile_text(&gitfile))
    else {
        return Err(format!(
            "{} is not the .git file git wrote, so nothing of it was touched",
            gitfile.display()
        ));
    };
    let own = match (&recovery, &now_listed) {
        (Some(removal), _) => removal
            .entry
            .symlink_metadata()
            .is_ok()
            .then(|| removal.entry.clone()),
        (None, Some(_)) => Some(own_entry(checkout, wt, &text).ok_or_else(|| {
            format!(
                "its .git does not name git's entry for it in {}, so nothing of it was touched",
                checkout.display()
            )
        })?),
        (None, None) if half_removed(checkout, wt).is_some() => None,
        (None, None) => return Err(unlisted(checkout)),
    };
    let cutoff = SystemTime::now();
    let prepared = if recovery.is_none()
        && let (Some(entry), Some(listed)) = (&own, &now_listed)
    {
        Some(pending::prepare(wt, entry, &listed.head, &text, cutoff)?)
    } else {
        None
    };
    if !fence.clear(wt, own.as_deref().unwrap_or(wt)) {
        return Err(USED.to_owned());
    }
    if let Some(prepared) = prepared {
        pending::commit(checkout, &prepared)?;
        recovery = Some(prepared);
        if !fence.clear(wt, own.as_deref().unwrap_or(wt)) {
            return Err(USED.to_owned());
        }
    }
    if let Some(removal) = &recovery {
        pending::revalidate(checkout, wt, removal)?;
    } else {
        pending::unchanged_since(wt, cutoff, true)?;
        if gitfile_text(&gitfile).as_ref() != Some(&text) {
            return Err("snapshot Git binding changed during admission".to_owned());
        }
    }
    let took = super::remove::tree(&gitfile);
    if !took.whole() {
        let kept = super::remove::Named {
            count: took.stayed,
            first: vec![PathBuf::from(".git")],
        };
        return Err(format!(
            "{}, so nothing else of it was touched",
            kept.words(wt)
        ));
    }
    let left = super::remove::tree(wt);
    if left.whole()
        && let Some(removal) = &mut recovery
        && !removal.tree_gone
    {
        pending::tree_gone(checkout, wt, removal)?;
    }
    let mut why = Vec::new();
    if !left.whole() {
        why.push(super::remove::stayed_words(wt, left.stayed));
    }
    if let Some(entry) = &own {
        let refused = if !fence.clear(wt, entry) {
            Some(USED.to_owned())
        } else if let Some(removal) = &recovery
            && let Err(reason) = pending::revalidate(checkout, wt, removal)
        {
            Some(reason)
        } else if left.whole() {
            let wt_arg = wt.to_string_lossy();
            let dropped = git.run(checkout, &["worktree", "remove", "--force", &wt_arg]);
            (dropped.code != Some(0)).then_some(dropped.err)
        } else if entry.join("locked").symlink_metadata().is_ok() {
            Some(LOCKED.to_owned())
        } else {
            let gone = super::remove::tree(entry);
            (!gone.whole()).then(|| super::remove::stayed_words(entry, gone.stayed))
        };
        match (listed(git, checkout, wt), refused) {
            (Ok(None), None) => {}
            (Ok(None), Some(refused)) => {
                why.push(format!("git's entry for it did not go whole: {refused}"));
            }
            (Ok(Some(_)), refused) => why.push(format!(
                "git still lists it as a worktree of {}{}",
                checkout.display(),
                refused.map_or_else(String::new, |r| format!(" ({r})"))
            )),
            (Err(e), _) => why.push(e),
        }
    }
    let remains = !left.whole() && std::fs::symlink_metadata(wt).is_ok_and(|m| m.is_dir());
    if remains && std::fs::symlink_metadata(&gitfile).is_err() {
        let written = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&gitfile)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &text));
        if let Err(e) = written {
            why.push(format!(
                "its .git could not be written back ({e}), so no later apply knows it"
            ));
        }
    }
    if why.is_empty() {
        if let Some(removal) = &recovery {
            pending::finish(checkout, wt, removal)?;
        }
        Ok(())
    } else {
        Err(why.join("; "))
    }
}

/// Remove each leftover [`plan`] says goes, judged again just before: the snapshot as
/// [`remove_snapshot`] says, the rest by the build cache's remover ([`super::remove::tree`]),
/// which no one entry stops: every entry that can go goes, and one that stays keeps the
/// leftover, kept with how many stayed and the first of them ([`super::remove::stayed_words`])
/// for the next apply to finish. Active or unknown process use keeps a leftover before
/// removal. A cells lane goes only once its own snapshot did.
/// Each leftover with what became of it: `Ok(())` removed, `Err(why)` kept.
pub fn retire(
    roots: &Roots,
    now: SystemTime,
    git: &dyn Git,
) -> Vec<(Leftover, Result<(), String>)> {
    retire_on(roots, now, git, &super::fence::Fence::default())
}

fn retire_on(
    roots: &Roots,
    now: SystemTime,
    git: &dyn Git,
    fence: &super::fence::Fence<'_>,
) -> Vec<(Leftover, Result<(), String>)> {
    let mut done = Vec::new();
    let mut kept_with_snapshot: Option<PathBuf> = None;
    for left in plan(roots, now, git) {
        let result = match (&left.verdict, &left.kind) {
            (Err(why), _) => Err(why.clone()),
            (Ok(_), Kind::Snapshot { checkout }) => {
                let removed = remove_snapshot(git, checkout, &left.path, now, fence);
                if removed.is_err() {
                    kept_with_snapshot = beside(checkout, CELLS);
                }
                removed
            }
            (Ok(_), Kind::Cells) if kept_with_snapshot.as_deref() == Some(left.path.as_path()) => {
                Err("its snapshot could not be removed, and it goes with it".to_owned())
            }
            (Ok(_), Kind::Cells | Kind::Lock) => {
                if !fence.clear(&left.path, &left.path) {
                    Err(USED.to_owned())
                } else {
                    let stayed = super::remove::tree(&left.path);
                    if stayed.whole() {
                        Ok(())
                    } else {
                        Err(super::remove::stayed_words(&left.path, stayed.stayed))
                    }
                }
            }
        };
        done.push((left, result));
    }
    done
}

#[cfg(test)]
#[path = "build_cache_retired_tests.rs"]
mod tests;
