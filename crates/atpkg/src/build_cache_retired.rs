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
//!
//! The same judge and remover are open to a linked worktree aterm's agents make
//! ([`judge_worktree`], [`remove_worktree`]): the disk watch's worktree step finds them. Only
//! one of the two shapes an agent's worktree has is a candidate ([`agent_made`]): a folder named
//! `*.noindex` with a `.ubd` folder above it (AGENTS.md's shape), or a folder in its checkout's
//! `.claude/worktrees/` (Claude Code's own isolation worktrees); a person's worktree in any other
//! place, and every worktree of a bare repository, stays. Such a worktree may be on a branch;
//! beyond the snapshot's rule it goes only while git has no operation in progress in it, it
//! holds no per-worktree ref, no other worktree lies inside it, neither its HEAD's reflog nor
//! its branch's names a commit `origin/main` lacks, no tracked file is one git is told not to
//! look at, git status read it without a warning, every entry git ignores in it or in any
//! submodule checked out in it is a tagged cache folder, no submodule folder that is not checked
//! out holds anything, and git wrote none of its HEAD, its HEAD's reflog or its branch's reflog
//! for [`UNWRITTEN`]. Its receipt lies in the checkout's git directory (`.git/aterm-retiring/`),
//! one per worktree, with what it ignored; a receipt is withdrawn when its removal stops before
//! its first entry goes, and a tree that did not go whole keeps git's entry for it, so the next
//! look finds it again and holds it to every rule that does not need its tree whole, and to git
//! seeing nothing in it or in its submodules, ignored entries included, but what the removal did
//! (a tracked file gone, an entry it ignored). A tree that stands once git's entry for it went
//! (a `git worktree prune` or `git gc` dropped it) stays: nothing can then say what was put in
//! it since.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

#[path = "build_cache_retired_pending.rs"]
mod pending;

#[path = "build_cache_retired_registry.rs"]
mod registry;

#[path = "build_cache_retired_history.rs"]
mod history;

#[path = "build_cache_retired_lazy_fetch.rs"]
mod lazy_fetch;

/// Nothing in a leftover was written for this long before it goes.
pub const UNWRITTEN: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The gate's snapshot of `<checkout>`: `<checkout>-verify.noindex`.
const SNAPSHOT: &str = "-verify.noindex";

/// The snapshot's cells lane: `<checkout>-verify-cells.noindex`.
const CELLS: &str = "-verify-cells.noindex";

/// The machine lock's directory in the user's cache.
const LOCK_DIR: &str = "aterm-verify";

/// The most reflog entries one worktree's judgment reads, its HEAD's and its branch's each.
const MAX_REFLOG: usize = 4096;

/// Which rule a worktree is judged by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// The retired gate's own snapshot: detached, as the gate left it, and judged half removed
    /// when a removal that stopped part way took git's entry for it.
    Snapshot,
    /// Any linked worktree of the checkout, on a branch or detached, which git lists.
    Linked,
}

/// A linked worktree the judge admits ([`judge_worktree`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
    /// Why it goes.
    pub why: String,
    /// The commit its HEAD names.
    pub head: String,
    /// The branch it is on (the full `refs/heads/…`), `None` when detached or when its removal
    /// resumes from a receipt.
    pub branch: Option<String>,
}

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

impl Answer {
    /// No run started, for `why`.
    fn unrun(why: String) -> Self {
        Self {
            code: None,
            out: String::new(),
            err: why,
        }
    }
}

/// How git is asked: the live one ([`GitCli`]) in production, the same in tests on scratch repos.
pub trait Git {
    /// `git -C <dir> <args>`, with git's optional locks off so no read writes its index.
    fn run(&self, dir: &Path, args: &[&str]) -> Answer;

    /// `git -C <dir> <args>`, adding `alt` to `GIT_ALTERNATE_OBJECT_DIRECTORIES` when given.
    fn run_with_alt(&self, dir: &Path, args: &[&str], alt: Option<&Path>) -> Answer {
        let _ = alt;
        self.run(dir, args)
    }

    /// A local original-object history read with complete bounded output and input. `objects`
    /// replaces object lookup with an independently bound witness, never an added alternate.
    /// An implementation without this contract cannot authorize private-store cleanup.
    fn read_history(
        &self,
        dir: &Path,
        args: &[&str],
        input: &[u8],
        objects: Option<&Path>,
        deadline: std::time::Instant,
    ) -> Answer {
        let _ = (dir, args, input, objects, deadline);
        Answer {
            code: None,
            out: String::new(),
            err: "bounded original Git history reads are unavailable".to_owned(),
        }
    }

    /// When this git's runs end: a run still going then is killed and answers no code, and the
    /// walks a judgment makes beside them ([`written`]) stop there too, keeping what they judge.
    /// `None`: they run to their end.
    fn until(&self) -> Option<Instant> {
        None
    }
}

/// The first `git` on `PATH`, run as [`GitProgram`].
#[derive(Clone, Copy, Debug, Default)]
pub struct GitCli;

impl GitCli {
    /// The first `git` on `PATH`, or no run, for why.
    fn program() -> Result<PathBuf, Answer> {
        crate::vendor::first_on_path("git", std::env::var_os("PATH").as_deref())
            .ok_or_else(|| Answer::unrun("git did not start: no git on PATH".to_owned()))
    }
}

/// The git program at a path, its reads kept from fetching an object a partial clone lacks:
/// `GIT_NO_LAZY_FETCH=1` on every run, which a child git inherits, and `--no-lazy-fetch` where
/// git takes it; a git that refuses the option runs only where it was seen to obey the variable
/// ([`lazy_fetch::guard`]). Any other git starts no run, so it cannot authorize snapshot
/// retirement. No run inherits a `GIT_*` variable this process holds
/// ([`lazy_fetch::without_git_environment`]).
struct GitProgram<'a>(&'a Path);

/// `path` without a Windows verbatim drive prefix (`\\?\C:\…`), which git reads as a host name.
fn git_path(path: &Path) -> &Path {
    path.to_str()
        .and_then(|s| s.strip_prefix(r"\\?\"))
        .filter(|s| !s.starts_with(r"UNC\"))
        .map_or(path, Path::new)
}

impl Git for GitCli {
    fn run(&self, dir: &Path, args: &[&str]) -> Answer {
        self.run_with_alt(dir, args, None)
    }

    fn run_with_alt(&self, dir: &Path, args: &[&str], alt: Option<&Path>) -> Answer {
        match Self::program() {
            Ok(program) => GitProgram(&program).run_with_alt(dir, args, alt),
            Err(unrun) => unrun,
        }
    }

    fn read_history(
        &self,
        dir: &Path,
        args: &[&str],
        input: &[u8],
        objects: Option<&Path>,
        deadline: std::time::Instant,
    ) -> Answer {
        match Self::program() {
            Ok(program) => GitProgram(&program).read_history(dir, args, input, objects, deadline),
            Err(unrun) => unrun,
        }
    }
}

/// [`GitCli`] whose every run ends by the instant it holds: a run still going then is killed
/// and answers no code, so the judgment it serves keeps what it judges, and the judgment's own
/// walks stop there too ([`Git::until`]). An unattended caller that must not block (the disk
/// look's worktree step) asks git through it.
#[derive(Clone, Copy, Debug)]
pub struct GitUntil(pub Instant);

impl Git for GitUntil {
    fn run(&self, dir: &Path, args: &[&str]) -> Answer {
        self.run_with_alt(dir, args, None)
    }

    fn run_with_alt(&self, dir: &Path, args: &[&str], alt: Option<&Path>) -> Answer {
        match GitCli::program() {
            Ok(program) => GitProgram(&program).run_until(dir, args, alt, Some(self.0)),
            Err(unrun) => unrun,
        }
    }

    fn read_history(
        &self,
        dir: &Path,
        args: &[&str],
        input: &[u8],
        objects: Option<&Path>,
        deadline: std::time::Instant,
    ) -> Answer {
        GitCli.read_history(dir, args, input, objects, deadline.min(self.0))
    }

    fn until(&self) -> Option<Instant> {
        Some(self.0)
    }
}

impl GitProgram<'_> {
    /// [`Git::run_with_alt`], killed at `until` when one is given.
    fn run_until(
        &self,
        dir: &Path,
        args: &[&str],
        alt: Option<&Path>,
        until: Option<Instant>,
    ) -> Answer {
        let guard = match lazy_fetch::guard(self.0) {
            Ok(guard) => guard,
            Err(why) => return Answer::unrun(why),
        };
        let mut cmd = aterm_uds::spawnfd::command(self.0);
        lazy_fetch::without_git_environment(&mut cmd);
        cmd.args(guard.option())
            .arg("-C")
            .arg(git_path(dir))
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(aterm_uds::spawnfd::Stdio::null());
        if let Some(alt) = alt {
            cmd.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", git_path(alt));
        }
        let out = match until {
            None => cmd.output(),
            Some(deadline) => aterm_uds::deadline::output_with_input_until(
                &mut cmd,
                aterm_uds::secret::Secret::new(&[]),
                deadline,
                aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
            ),
        };
        match out {
            Ok(o) => Answer {
                code: o.status.code(),
                out: String::from_utf8_lossy(&o.stdout).into_owned(),
                err: String::from_utf8_lossy(&o.stderr).trim().to_owned(),
            },
            Err(e) => Answer::unrun(format!("git did not start or did not finish: {e}")),
        }
    }
}

impl Git for GitProgram<'_> {
    fn run(&self, dir: &Path, args: &[&str]) -> Answer {
        self.run_with_alt(dir, args, None)
    }

    fn run_with_alt(&self, dir: &Path, args: &[&str], alt: Option<&Path>) -> Answer {
        self.run_until(dir, args, alt, None)
    }

    fn read_history(
        &self,
        dir: &Path,
        args: &[&str],
        input: &[u8],
        objects: Option<&Path>,
        deadline: std::time::Instant,
    ) -> Answer {
        let guard = match lazy_fetch::guard(self.0) {
            Ok(guard) => guard,
            Err(why) => return Answer::unrun(why),
        };
        let mut cmd = aterm_uds::spawnfd::command(self.0);
        lazy_fetch::without_git_environment(&mut cmd);
        cmd.args(guard.option())
            .arg("--no-pager")
            .arg("--no-optional-locks")
            .arg("--no-replace-objects")
            .args(["-c", "protocol.allow=never", "-c", "core.commitGraph=false"])
            .args(["-c", "pack.useBitmaps=false", "-c", "gc.auto=0"])
            .arg("-C")
            .arg(git_path(dir))
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_ALLOW_PROTOCOL", "")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0");
        if let Some(objects) = objects {
            cmd.env("GIT_OBJECT_DIRECTORY", git_path(objects));
        }
        let result = aterm_uds::deadline::output_with_input_until(
            &mut cmd,
            aterm_uds::secret::Secret::new(input),
            deadline,
            aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES,
        );
        match result {
            Ok(output) => match (
                String::from_utf8(output.stdout),
                String::from_utf8(output.stderr),
            ) {
                (Ok(out), Ok(err)) => Answer {
                    code: output.status.code(),
                    out,
                    err,
                },
                _ => Answer::unrun("Git history metadata is not complete UTF-8".to_owned()),
            },
            Err(error) => Answer::unrun(format!("Git history read did not complete: {error}")),
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

/// Whether a pass may look at `checkout` at all: it lies in no folder that is never looked
/// into (`aterm_toolchain::checkout::never_looked_at`: Documents, Desktop, Downloads, a volume)
/// and under no root macOS guards with a consent dialog
/// ([`crate::protected::under_protected_root`]). Read off the path alone, before the first
/// stat: the daily pass runs with nobody asking, and judging a snapshot there (a `git` child,
/// a walk of its tree) would put macOS's question on the person's screen (owner ruling
/// 2026-11-09). What the retired gate left in such a folder stays for its owner.
fn looked_into(home: &Path, checkout: &Path) -> bool {
    crate::build_cache::checkout::never_looked_at(checkout, Some(home)).is_none()
        && !crate::protected::under_protected_root(home, checkout)
}

/// The main checkouts whose snapshot may be on disk: `home`'s own children, and `workspaces`
/// (the build cache's), each with a `<name>-verify.noindex` or `-verify-cells.noindex` beside it,
/// leaving out one a pass never looks into ([`looked_into`]).
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
        .filter(|c| looked_into(home, c))
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
/// submodule's is): git writes its bookkeeping on a read. `Ok(None)` when nothing was.
///
/// # Errors
/// An entry that could not be read or listed, or `until` passed during the walk: whether it was
/// written is then unknown, and what the caller judges stays.
pub fn written_since(
    dir: &Path,
    since: SystemTime,
    until: Option<Instant>,
) -> Result<Option<PathBuf>, String> {
    written(dir, since, Times::All, until)
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

/// [`written_since`], the entries whose times count as `times` says. An entry gone during the
/// walk is passed over; one that cannot be read or listed, or `until` passing, is an `Err`: an
/// unreadable folder may hold anything, and the remover would take it whole.
fn written(
    dir: &Path,
    since: SystemTime,
    times: Times,
    until: Option<Instant>,
) -> Result<Option<PathBuf>, String> {
    let unreadable = |at: &Path, e: &std::io::Error| {
        format!(
            "{} could not be read ({e}), so whether it was written is unknown",
            at.display()
        )
    };
    let gone = |e: &std::io::Error| e.kind() == std::io::ErrorKind::NotFound;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        if until.is_some_and(|until| Instant::now() >= until) {
            return Err(format!(
                "the walk of {} ran out of time, so whether it was written is unknown",
                dir.display()
            ));
        }
        let meta = match std::fs::symlink_metadata(&at) {
            Ok(meta) => meta,
            Err(e) if gone(&e) => continue,
            Err(e) => return Err(unreadable(at.as_path(), &e)),
        };
        let children: Vec<std::fs::DirEntry> = if meta.is_dir() {
            match entries(&at) {
                Ok(children) => children,
                Err(e) if gone(&e) => continue,
                Err(e) => return Err(unreadable(at.as_path(), &e)),
            }
        } else {
            Vec::new()
        };
        let named = |n: &str| children.iter().any(|e| e.file_name() == n);
        if at != dir && named("HEAD") && named("objects") && named("refs") {
            continue;
        }
        let fresh = |m: &std::fs::Metadata| m.modified().is_ok_and(|t| t >= since);
        if (times == Times::All || !meta.is_dir()) && fresh(&meta) {
            return Ok(Some(at));
        }
        for e in children.into_iter().filter(|e| e.file_name() != ".git") {
            if e.file_type().is_ok_and(|ft| ft.is_dir()) {
                stack.push(e.path());
                continue;
            }
            match e.metadata() {
                Ok(m) if fresh(&m) => return Ok(Some(e.path())),
                Ok(_) => {}
                Err(err) if gone(&err) => {}
                Err(err) => return Err(unreadable(e.path().as_path(), &err)),
            }
        }
    }
    Ok(None)
}

/// Every entry of the folder `dir`.
fn entries(dir: &Path) -> std::io::Result<Vec<std::fs::DirEntry>> {
    std::fs::read_dir(dir)?.collect()
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
/// the entries whose times count as `times` says, the walk ending at `until`: an entry it could
/// not read, or time running out, keeps it too.
fn written_words(
    path: &Path,
    since: SystemTime,
    now: SystemTime,
    times: Times,
    until: Option<Instant>,
) -> Option<String> {
    let hit = match written(path, since, times, until) {
        Ok(hit) => hit?,
        Err(why) => return Some(why),
    };
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

/// Every worktree `git worktree list --porcelain` in `checkout` gives (the main checkout's
/// first), each with its path as git lists it.
fn listing(git: &dyn Git, checkout: &Path) -> Result<Vec<(PathBuf, Listed)>, String> {
    let a = git.run(checkout, &["worktree", "list", "--porcelain"]);
    if a.code != Some(0) {
        return Err(format!("git worktree list failed: {}", a.err));
    }
    let mut out = Vec::new();
    for block in a.out.split("\n\n") {
        let mut lines = block.lines();
        let Some(path) = lines.next().and_then(|l| l.strip_prefix("worktree ")) else {
            continue;
        };
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
        out.push((PathBuf::from(path), entry));
    }
    Ok(out)
}

/// `path` resolved where it can be, as written where it cannot.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The entry `git worktree list --porcelain` in `checkout` gives `wt`.
fn listed(git: &dyn Git, checkout: &Path, wt: &Path) -> Result<Option<Listed>, String> {
    let want = resolved(wt);
    Ok(listing(git, checkout)?
        .into_iter()
        .find(|(at, _)| resolved(at) == want)
        .map(|(_, entry)| entry))
}

/// The path of every worktree `git worktree list --porcelain` in `checkout` gives, the main
/// checkout's included, each resolved where it can be.
fn listed_paths(git: &dyn Git, checkout: &Path) -> Result<Vec<PathBuf>, String> {
    Ok(listing(git, checkout)?
        .into_iter()
        .map(|(at, _)| resolved(&at))
        .collect())
}

/// The bytes of `gitfile`, a worktree's `.git`, while it is a file as git writes one (a line
/// naming its entry): `None` for a folder, a link, a file too long to be one, or none.
fn gitfile_text(gitfile: &Path) -> Option<Vec<u8>> {
    crate::metadata_io::read_bounded_regular(gitfile, 4096).ok()
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
        .then(|| git_path(&common).join("worktrees").join(id))
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
    judge_tree(git, checkout, wt, now, Shape::Snapshot).map(|admitted| admitted.why)
}

/// The verdict on the worktree `wt` of `checkout` at `now`, by the rule `shape` names.
fn judge_tree(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
    shape: Shape,
) -> Result<Admitted, String> {
    let until = git.until();
    if let Some(removal) = pending::read_at(checkout, wt, now)? {
        if removal.entry.join("locked").symlink_metadata().is_ok() {
            return Err(LOCKED.to_owned());
        }
        let now_listed = listed(git, checkout, wt)?;
        if let Some(entry) = &now_listed {
            if entry.locked {
                return Err(LOCKED.to_owned());
            }
            if (shape == Shape::Snapshot && !entry.detached) || entry.head != removal.head {
                return Err("snapshot HEAD changed after removal began".to_owned());
            }
        }
        in_main(git, checkout, &removal.head)?;
        // A receipt binds what was admitted, not what a person did in the tree since: a linked
        // worktree whose removal stopped part way is held again to every rule that does not
        // need its tree whole, and what git sees in what is left of it must be the removal's.
        // Its removal keeps git's entry while any of its tree stays, so a tree that stands
        // without it lost it to someone else (a `git worktree prune`, a `git gc`): with no entry
        // nothing can say what was put in it since, and what is left stays.
        if shape == Shape::Linked {
            if removal.entry.symlink_metadata().is_ok() {
                let branch = now_listed.as_ref().and_then(|e| e.branch.as_deref());
                linked_clear(git, checkout, wt, &removal.entry, branch)?;
                if wt.symlink_metadata().is_ok() {
                    removal_only(git, wt, &removal.entry, &removal.ignored)?;
                }
            } else if wt.symlink_metadata().is_ok() {
                return Err(format!(
                    "git's entry for it ({}) went while its tree stands (a git worktree prune or \
                     git gc dropped it), so nothing can say what was put in it since: what is \
                     left stays",
                    removal.entry.display()
                ));
            }
        }
        history::clear(git, checkout, wt, Some(&removal.entry))?;
        let why = match shape {
            Shape::Snapshot => {
                "the exact snapshot's interrupted removal, bound by its durable receipt"
            }
            Shape::Linked => {
                "the exact worktree's interrupted removal, bound by its durable receipt"
            }
        };
        return Ok(Admitted {
            why: why.to_owned(),
            head: removal.head,
            branch: None,
        });
    }
    let Some(entry) = listed(git, checkout, wt)? else {
        if shape == Shape::Linked {
            return Err(unlisted(checkout));
        }
        let Some(gone) = half_removed(checkout, wt) else {
            return Err(unlisted(checkout));
        };
        history::clear(git, checkout, wt, None)?;
        if let Some(why) = now
            .checked_sub(UNWRITTEN)
            .and_then(|since| written_words(wt, since, now, Times::NotFolders, until))
        {
            return Err(why);
        }
        return Ok(Admitted {
            why: format!(
                "the retired verify gate's snapshot of {} (its code went in 42e8a9a273), half \
                 removed: a removal that stopped part way took git's entry for it ({}), which \
                 its .git still names, no file in it written in the last {}",
                checkout.display(),
                gone.display(),
                age(UNWRITTEN)
            ),
            head: String::new(),
            branch: None,
        });
    };
    if entry.locked {
        return Err(LOCKED.to_owned());
    }
    if shape == Shape::Snapshot && !entry.detached {
        return Err(format!(
            "it is on {}, not detached as the gate left it",
            entry.branch.as_deref().unwrap_or("a branch")
        ));
    }
    let own = gitfile_text(&wt.join(".git")).and_then(|text| own_entry(checkout, wt, &text));
    if shape == Shape::Linked && own.is_none() {
        return Err(format!(
            "its .git is not a file naming git's entry for it in {}: a main checkout or another \
             repository's",
            checkout.display()
        ));
    }
    let short = in_main(git, checkout, &entry.head)?;
    let changed = snapshot_changes(git, checkout, wt, shape == Shape::Linked)?;
    if let Some(first) = changed.first() {
        return Err(format!(
            "it has {} uncommitted change(s) ({}{})",
            changed.len(),
            first.trim(),
            if changed.len() > 1 { ", …" } else { "" }
        ));
    }
    if shape == Shape::Linked
        && let Some(admin) = own.as_deref()
    {
        let since = now
            .checked_sub(UNWRITTEN)
            .ok_or("the clock reads too early to date the last seven days")?;
        hidden_edits(git, wt, wt, &[])?;
        linked_clear(git, checkout, wt, admin, entry.branch.as_deref())?;
        ignored_entries(git, wt)?;
        git_files_quiet(checkout, admin, entry.branch.as_deref(), since, now, until)?;
    }
    history::clear(git, checkout, wt, own.as_deref())?;
    if let Some(why) = now
        .checked_sub(UNWRITTEN)
        .and_then(|since| written_words(wt, since, now, Times::All, until))
    {
        return Err(why);
    }
    let why = match shape {
        Shape::Snapshot => format!(
            "the retired verify gate's snapshot of {} (its code went in 42e8a9a273): a clean \
             worktree detached at {short}, which origin/main contains, nothing in it written in \
             the last {}",
            checkout.display(),
            age(UNWRITTEN)
        ),
        Shape::Linked => format!(
            "a clean linked worktree of {} at {short}{}, which origin/main contains, nothing in \
             it written in the last {}",
            checkout.display(),
            entry
                .branch
                .as_deref()
                .map_or_else(String::new, |b| format!(" on {b}")),
            age(UNWRITTEN)
        ),
    };
    Ok(Admitted {
        why,
        head: entry.head,
        branch: entry.branch,
    })
}

/// The names git gives an operation in progress in a worktree's entry: a rebase, a merge, a
/// cherry-pick, a revert, a bisect, a sequence of picks.
const IN_PROGRESS: &[&str] = &[
    "rebase-merge",
    "rebase-apply",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "sequencer",
];

/// What a linked worktree must hold beyond the snapshot's rule before it goes, its entry
/// `admin` and `branch` the one git lists it on, whether or not its tree stands: git has no
/// operation in progress in it; it holds no per-worktree ref; no other worktree of `checkout`
/// lies inside it; and no commit its HEAD's or its branch's reflog names is missing from
/// `origin/main` (removing the entry and the branch drops those reflogs, which may be such a
/// commit's only root).
fn linked_clear(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    admin: &Path,
    branch: Option<&str>,
) -> Result<(), String> {
    for name in IN_PROGRESS {
        match std::fs::symlink_metadata(admin.join(name)) {
            Ok(_) => {
                return Err(format!("git has an operation in progress in it ({name})"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "whether git has an operation in progress in it ({name}) could not be read: \
                     {e}"
                ));
            }
        }
    }
    if let Some(held) = first_file(&admin.join("refs"))? {
        return Err(format!(
            "it holds a per-worktree ref ({}), which goes with git's entry for it",
            held.display()
        ));
    }
    let inside = resolved(wt);
    if let Some(nested) = listed_paths(git, checkout)?
        .into_iter()
        .find(|p| *p != inside && p.starts_with(&inside))
    {
        return Err(format!(
            "another worktree of {}, {}, lies inside it",
            checkout.display(),
            nested.display()
        ));
    }
    let mut oids = reflog(git, checkout, Some(admin), "HEAD")?;
    if let Some(branch) = branch {
        oids.extend(reflog(git, checkout, None, branch)?);
    }
    oids.sort();
    oids.dedup();
    if !oids.is_empty() {
        let mut args = vec!["rev-list", "-n1"];
        args.extend(oids.iter().map(String::as_str));
        args.extend(["--not", "refs/remotes/origin/main"]);
        let lacking = git.run(checkout, &args);
        if lacking.code != Some(0) {
            return Err(format!(
                "git could not say whether its reflog names a commit origin/main lacks: {}",
                lacking.err
            ));
        }
        if let Some(first) = lacking.out.lines().map(str::trim).find(|l| !l.is_empty()) {
            return Err(format!(
                "its reflog names a commit origin/main lacks (first {}), whose only root that \
                 reflog may be",
                first.get(..10).unwrap_or(first)
            ));
        }
    }
    Ok(())
}

/// Whether git wrote none of a linked worktree's own files at or after `since`, at `now`, the
/// walk ending at `until`: its HEAD, its HEAD's reflog, its branch's reflog (`branch`, the one
/// git lists it on) and a per-worktree reftable in its entry `admin` of `checkout`. A reset or a
/// checkout writes those, not always a file in the tree.
fn git_files_quiet(
    checkout: &Path,
    admin: &Path,
    branch: Option<&str>,
    since: SystemTime,
    now: SystemTime,
    until: Option<Instant>,
) -> Result<(), String> {
    let mut written = vec![admin.join("HEAD"), admin.join("logs").join("HEAD")];
    if let Some(branch) = branch {
        let rel = Path::new(branch);
        if !rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return Err(format!("its branch {branch} is no name git writes"));
        }
        written.push(checkout.join(".git").join("logs").join(rel));
    }
    if let Some(table) = std::fs::symlink_metadata(admin.join("reftable"))
        .ok()
        .filter(std::fs::Metadata::is_dir)
        .map(|_| admin.join("reftable"))
    {
        written.push(table);
    }
    for file in &written {
        match std::fs::symlink_metadata(file) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{} could not be read: {e}", file.display())),
        }
        if let Some(why) = written_words(file, since, now, Times::All, until) {
            return Err(why);
        }
    }
    Ok(())
}

/// `--git-dir=<admin>`: git's own entry of a linked worktree, through which git reads the
/// worktree whether or not its tree and its `.git` stand.
fn git_dir_arg(admin: &Path) -> Result<String, String> {
    git_path(admin)
        .to_str()
        .map(|admin| format!("--git-dir={admin}"))
        .ok_or_else(|| format!("{} is no path git takes as an argument", admin.display()))
}

/// The tracked files of the linked worktree `wt` that git is told not to look at, and that
/// stand in its tree: one marked assume-unchanged (`git ls-files -v` tags it in lower case) or
/// skip-worktree (`S`) hides its edits from `git status`, so it keeps the worktree. A sparse
/// checkout's skip-worktree files are not in the tree, and do not. Git runs in `dir` with
/// `prefix` before its command (a `--git-dir` and `--work-tree` when the tree's `.git` may be
/// gone).
fn hidden_edits(git: &dyn Git, dir: &Path, wt: &Path, prefix: &[&str]) -> Result<(), String> {
    let mut args = prefix.to_vec();
    args.extend(["ls-files", "-v", "-z"]);
    let ls = git.run(dir, &args);
    if ls.code != Some(0) {
        return Err(format!(
            "git could not list which of its files it is told not to look at: {}",
            ls.err
        ));
    }
    for row in ls.out.split('\0').filter(|r| !r.is_empty()) {
        let Some((tag, rel)) = row.split_once(' ') else {
            return Err(format!("git ls-files gave a row it does not write ({row})"));
        };
        let hidden = tag == "S" || tag.chars().all(|c| c.is_ascii_lowercase());
        if hidden && std::fs::symlink_metadata(wt.join(rel)).is_ok() {
            return Err(format!(
                "git is told not to look at its file {rel} (`{tag}`: assume-unchanged or \
                 skip-worktree), so an edit to it would not show"
            ));
        }
    }
    Ok(())
}

/// One level of a linked worktree git reads on its own: the worktree, or a submodule checked out
/// in it (nested ones included).
#[derive(Clone, Debug)]
struct Level {
    /// Its folder.
    dir: PathBuf,
    /// Its path in the worktree with a trailing `/` (`vendor/astream/`), empty for the worktree.
    prefix: String,
    /// The commit its parent's index records for it, `None` for the worktree.
    recorded: Option<String>,
}

impl Level {
    /// Its path in the worktree, or `the worktree` for the worktree itself.
    fn prefix_or_root(&self) -> &str {
        if self.prefix.is_empty() {
            "the worktree"
        } else {
            &self.prefix
        }
    }
}

/// The most levels of nested submodules [`levels`] descends.
const MAX_DEPTH: usize = 8;

/// The most submodules checked out in one worktree that [`levels`] reads.
const MAX_LEVELS: usize = 64;

/// The linked worktree `wt` and every submodule checked out in it, nested ones included, each
/// read through its own index (`git ls-files --stage`); git runs at the worktree with `args`
/// before its command (a `--git-dir` and `--work-tree` when the tree's `.git` may be gone). A
/// gitlink whose folder holds a `.git` is checked out, and git reads it as a level of its own:
/// the superproject's `git status`, under any `--ignore-submodules`, reports no file git ignores
/// in it. A gitlink folder with no `.git` that holds anything is an `Err`: git status sees
/// nothing in it at all, and the remover would take it whole. So is an unmerged gitlink, a path
/// git does not write, a gitlink that is no folder, and more than [`MAX_LEVELS`] levels or
/// [`MAX_DEPTH`] deep.
fn levels(git: &dyn Git, wt: &Path, args: &[&str]) -> Result<Vec<Level>, String> {
    let mut out = vec![Level {
        dir: wt.to_path_buf(),
        prefix: String::new(),
        recorded: None,
    }];
    let mut depth = vec![0usize];
    let mut next = 0;
    while let Some(level) = out.get(next).cloned() {
        let at = depth[next];
        next += 1;
        let mut ls_args: Vec<&str> = if level.prefix.is_empty() {
            args.to_vec()
        } else {
            Vec::new()
        };
        ls_args.extend(["ls-files", "-z", "--stage"]);
        let ls = git.run(&level.dir, &ls_args);
        if ls.code != Some(0) || !ls.err.is_empty() {
            return Err(format!(
                "git could not list the submodules of {}: {}",
                level.dir.display(),
                ls.err
            ));
        }
        for row in ls.out.split('\0').filter(|r| !r.is_empty()) {
            let Some((meta, rel)) = row.split_once('\t') else {
                return Err(format!("git ls-files gave a row it does not write ({row})"));
            };
            let mut parts = meta.split_whitespace();
            let (Some(mode), Some(sha), Some(stage), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(format!("git ls-files gave a row it does not write ({row})"));
            };
            if mode != "160000" {
                continue;
            }
            let path = format!("{}{rel}", level.prefix);
            if stage != "0" {
                return Err(format!("its submodule {path} is unmerged"));
            }
            if !Path::new(rel)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
            {
                return Err(format!("its submodule path {path} is no path git writes"));
            }
            let dir = level.dir.join(rel);
            match std::fs::symlink_metadata(&dir) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(format!("its submodule {path} is no folder")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("its submodule {path} could not be read: {e}")),
            }
            match std::fs::symlink_metadata(dir.join(".git")) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let held = entries(&dir)
                        .map_err(|e| format!("its submodule {path} could not be listed: {e}"))?;
                    if !held.is_empty() {
                        return Err(format!(
                            "its submodule folder {path}/ holds files but no .git, so git sees \
                             nothing in it: it stays"
                        ));
                    }
                    continue;
                }
                Err(e) => return Err(format!("its submodule {path} could not be read: {e}")),
            }
            if at + 1 > MAX_DEPTH || out.len() == MAX_LEVELS {
                return Err(format!(
                    "it holds more submodules than a judgment reads (at most {MAX_LEVELS}, \
                     {MAX_DEPTH} deep)"
                ));
            }
            out.push(Level {
                dir,
                prefix: format!("{path}/"),
                recorded: Some(sha.to_owned()),
            });
            depth.push(at + 1);
        }
    }
    Ok(out)
}

/// The ignored entries of the linked worktree `wt` and of every submodule checked out in it
/// ([`levels`]; `git status --ignored`'s `!!` rows, a folder as one, each named by its path in
/// `wt`), recorded in its removal's receipt: once the removal takes a `.gitignore`, what it
/// ignored reads as untracked, and only these may ([`removal_only`]). Any other row means the
/// tree changed since it was judged, and it stays. At most [`MAX_IGNORED`] in all.
///
/// Each must be a folder that says it is a cache ([`cache_tagged`], read in its own level): an
/// ignored file or other folder (a `.env`, local notes, `node_modules`, a submodule's `.claude/`)
/// may be what nobody can regenerate, and git holds no copy of it, so the worktree stays.
fn ignored_entries(git: &dyn Git, wt: &Path) -> Result<Vec<String>, String> {
    let mut ignored = Vec::new();
    for level in levels(git, wt, &[])? {
        // A submodule's own index can hide an edit too: its superproject's status and ls-files
        // show it as one clean gitlink.
        hidden_edits(git, &level.dir, &level.dir, &[])
            .map_err(|why| format!("in {}: {why}", level.prefix_or_root()))?;
        let status = git.run(
            &level.dir,
            &[
                "status",
                "--porcelain",
                "-z",
                "--ignored",
                "--untracked-files=normal",
                "--ignore-submodules=all",
            ],
        );
        if status.code != Some(0) || !status.err.is_empty() {
            return Err(format!(
                "git status could not list what it ignores in {}: {}",
                level.dir.display(),
                status.err
            ));
        }
        for row in status.out.split('\0').filter(|r| !r.is_empty()) {
            let Some(rel) = row.strip_prefix("!! ") else {
                return Err(format!(
                    "it changed since it was judged ({}{row})",
                    level.prefix
                ));
            };
            let path = format!("{}{rel}", level.prefix);
            if !cache_tagged(&level.dir, rel) {
                return Err(format!(
                    "it holds {path}, which git ignores and which is no tagged cache folder; \
                     git keeps no copy of it"
                ));
            }
            if ignored.len() == MAX_IGNORED {
                return Err(format!(
                    "it holds more than {MAX_IGNORED} ignored entries, too many to record"
                ));
            }
            ignored.push(path);
        }
    }
    Ok(ignored)
}

/// Whether `rel`, an ignored row of `wt`'s `git status`, is a folder (git ends a folder's row
/// with `/`), not a link, that holds a `CACHEDIR.TAG` file whose first line is the cache
/// directory tag's signature, as Cargo writes in every target folder it makes.
fn cache_tagged(wt: &Path, rel: &str) -> bool {
    use std::io::Read as _;
    const SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
    let Some(folder) = rel.strip_suffix('/') else {
        return false;
    };
    let folder = wt.join(folder);
    let is_folder = std::fs::symlink_metadata(&folder).is_ok_and(|m| m.file_type().is_dir());
    let tag = folder.join("CACHEDIR.TAG");
    let Ok((file, meta)) = crate::metadata_io::open_regular_with_meta(&tag) else {
        return false;
    };
    let mut head = Vec::with_capacity(SIGNATURE.len());
    is_folder
        && meta.len() >= SIGNATURE.len() as u64
        && file
            .take(SIGNATURE.len() as u64)
            .read_to_end(&mut head)
            .is_ok()
        && head == SIGNATURE
}

/// The most ignored entries a linked worktree's receipt records ([`ignored_entries`]).
const MAX_IGNORED: usize = 256;

/// Whether all git sees changed in `wt`, a linked worktree whose removal stopped part way, and
/// in every submodule still checked out in it ([`levels`]), read through its entry `admin` (its
/// `.git` may be gone), is the removal's: a tracked file deleted, an entry untracked or ignored
/// under one of `ignored`, what it and its submodules ignored when its removal began (a
/// `.gitignore` may be among what went), or a submodule still at the commit its parent records
/// (what is in it is read as a level of its own). Anything else (a file put in it since, ignored
/// or not, an edit, a staged change, a submodule moved, a file git is told not to look at, a
/// submodule folder that holds files but no `.git`) keeps it, whatever its modification time
/// says: a move or a copy keeps an old one. A removal that stopped among a submodule's files,
/// having taken its `.git` and not the rest, so leaves what stays for a person to look at.
fn removal_only(git: &dyn Git, wt: &Path, admin: &Path, ignored: &[String]) -> Result<(), String> {
    let git_dir = git_dir_arg(admin)?;
    let work_tree = git_path(wt)
        .to_str()
        .map(|wt| format!("--work-tree={wt}"))
        .ok_or_else(|| format!("{} is no path git takes as an argument", wt.display()))?;
    let top = [git_dir.as_str(), work_tree.as_str()];
    let under = |rel: &str| {
        ignored
            .iter()
            .any(|i| rel == i.as_str() || (i.ends_with('/') && rel.starts_with(i.as_str())))
    };
    let levels = levels(git, wt, &top)?;
    // A submodule git reads as a level of its own stays where its removal began while its HEAD
    // is the commit its parent's index records: `M` in its parent's status then says only that
    // something in it changed, which its own level's read judges.
    let unmoved = |path: &str| -> Result<bool, String> {
        let Some(level) = levels.iter().find(|l| l.prefix == path) else {
            return Ok(false);
        };
        let head = git.run(&level.dir, &["rev-parse", "--verify", "HEAD^{commit}"]);
        if head.code != Some(0) {
            return Err(format!(
                "git cannot read the HEAD of its submodule {path} ({}), so it stays",
                head.err
            ));
        }
        Ok(level.recorded.as_deref() == Some(head.out.trim()))
    };
    for level in &levels {
        let mut args: Vec<&str> = if level.prefix.is_empty() {
            top.to_vec()
        } else {
            Vec::new()
        };
        args.extend([
            "status",
            "--porcelain",
            "-z",
            "--ignored",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ]);
        let status = git.run(&level.dir, &args);
        if status.code != Some(0) || !status.err.is_empty() {
            return Err(format!(
                "git status cannot read what its stopped removal left in {} ({}), so it stays",
                level.dir.display(),
                status.err
            ));
        }
        for row in status.out.split('\0').filter(|r| !r.is_empty()) {
            let theirs = match row.split_at_checked(3) {
                Some((" D ", _)) => false,
                Some(("?? " | "!! ", rel)) => !under(&format!("{}{rel}", level.prefix)),
                Some((" M ", rel)) => !unmoved(&format!("{}{rel}/", level.prefix))?,
                _ => true,
            };
            if theirs {
                let at = if level.prefix.is_empty() {
                    String::new()
                } else {
                    format!(" in {}", level.prefix)
                };
                return Err(format!(
                    "its removal stopped part way and it now holds what no removal makes \
                     ({row}{at}), so it stays"
                ));
            }
        }
        let prefix: &[&str] = if level.prefix.is_empty() { &top } else { &[] };
        hidden_edits(git, &level.dir, &level.dir, prefix)?;
    }
    Ok(())
}

/// The first file (anything but a folder) under `dir`, `None` when it holds none or is not
/// there. A folder that cannot be listed is an `Err`.
fn first_file(dir: &Path) -> Result<Option<PathBuf>, String> {
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0usize;
    while let Some(at) = stack.pop() {
        let entries = match std::fs::read_dir(&at) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && at == dir => return Ok(None),
            Err(e) => return Err(format!("{} could not be listed: {e}", at.display())),
        };
        for e in entries {
            let e = e.map_err(|e| format!("{} could not be listed: {e}", at.display()))?;
            seen += 1;
            if seen > MAX_REFLOG {
                return Err(format!("{} holds too many entries to read", dir.display()));
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(e.path());
            } else {
                return Ok(Some(e.path()));
            }
        }
    }
    Ok(None)
}

/// The commits the reflog of `name` (`HEAD`, or a full branch ref) names, read in `checkout`,
/// through the linked worktree entry `admin` when one is given (a worktree's own HEAD, whose
/// tree may be gone): at most [`MAX_REFLOG`], each a whole object name. A read that fails, or
/// any line that is not an object name, is an `Err`.
fn reflog(
    git: &dyn Git,
    checkout: &Path,
    admin: Option<&Path>,
    name: &str,
) -> Result<Vec<String>, String> {
    let limit = format!("-n{}", MAX_REFLOG + 1);
    let git_dir = admin.map(git_dir_arg).transpose()?;
    let mut args: Vec<&str> = git_dir.as_deref().into_iter().collect();
    args.extend([
        "log",
        "-g",
        "--no-show-signature",
        "--format=%H",
        limit.as_str(),
        name,
        "--",
    ]);
    let read = git.run(checkout, &args);
    if read.code != Some(0) {
        return Err(format!(
            "the reflog of {name} could not be read: {}",
            read.err
        ));
    }
    let oids: Vec<String> = read
        .out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    if oids.len() > MAX_REFLOG {
        return Err(format!(
            "the reflog of {name} names more than {MAX_REFLOG} commits, too many to hold to \
             origin/main"
        ));
    }
    if let Some(bad) = oids
        .iter()
        .find(|o| !matches!(o.len(), 40 | 64) || !o.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(format!(
            "the reflog of {name} read as something other than commit names ({bad})"
        ));
    }
    Ok(oids)
}

/// Dirty or untracked entries in `wt` and its submodules, reading `checkout`'s own `.git/modules`
/// object store when a submodule's `objects/info/alternates` points at a since-removed worktree.
///
/// `strict` (a linked worktree's judgment) also holds a warning git wrote while it exited 0 to
/// mean it may not have seen every file: git status warns and goes on past a folder it cannot
/// read, which the remover would take whole.
fn snapshot_changes(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    strict: bool,
) -> Result<Vec<String>, String> {
    let status = git.run(
        wt,
        &[
            "status",
            "--porcelain",
            "--ignore-submodules=none",
            "--untracked-files=normal",
        ],
    );
    if status.code == Some(0) && strict && !status.err.is_empty() {
        return Err(format!(
            "git status warned ({}), so it may not have seen every file in it",
            status.err.lines().next().unwrap_or_default()
        ));
    }
    if status.code == Some(0) {
        return Ok(status
            .out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect());
    }
    let unread = Unreadable::read(&status.err);
    if unread.submodule.is_none() || unread.missing_store.is_none() {
        return Err(unread.words());
    }
    let top = git.run(
        wt,
        &[
            "status",
            "--porcelain",
            "--ignore-submodules=all",
            "--untracked-files=normal",
        ],
    );
    if top.code != Some(0) {
        return Err(unread.words());
    }
    let mut changed: Vec<String> = top
        .out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let cached = git.run(wt, &["diff-index", "--cached", "--name-only", "HEAD"]);
    if cached.code != Some(0) {
        return Err(unread.words());
    }
    for staged in cached.out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let entry = format!("M  {staged}");
        if !changed.iter().any(|existing| existing.ends_with(staged)) {
            changed.push(entry);
        }
    }
    let ls = git.run(wt, &["ls-files", "-z", "--stage"]);
    if ls.code != Some(0) {
        return Err(unread.words());
    }
    for row in ls.out.split('\0').filter(|r| !r.is_empty()) {
        let Some((meta, rel_path)) = row.split_once('\t') else {
            return Err(unread.words());
        };
        let mut parts = meta.split_whitespace();
        let (Some(mode), Some(sha), Some(stage), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(unread.words());
        };
        if mode != "160000" {
            continue;
        }
        if stage != "0" {
            changed.push(format!("U {rel_path}"));
            continue;
        }
        if !Path::new(rel_path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return Err(unread.words());
        }
        let sub_dir = wt.join(rel_path);
        if std::fs::symlink_metadata(sub_dir.join(".git")).is_err() {
            match std::fs::read_dir(&sub_dir) {
                Ok(mut d) => {
                    if d.next().is_some() {
                        changed.push(format!("?? {rel_path}/"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(unread.words()),
            }
            continue;
        }
        let module_objs = checkout.join(".git/modules").join(rel_path).join("objects");
        let alt = module_objs.is_dir().then_some(module_objs.as_path());
        let head = git.run_with_alt(&sub_dir, &["rev-parse", "--verify", "HEAD^{commit}"], alt);
        if head.code != Some(0) {
            return Err(unread.words());
        }
        let sub_status = git.run_with_alt(
            &sub_dir,
            &[
                "status",
                "--porcelain",
                "--ignore-submodules=none",
                "--untracked-files=normal",
            ],
            alt,
        );
        if sub_status.code != Some(0) {
            return Err(unread.words());
        }
        if head.out.trim() != sha || sub_status.out.lines().any(|l| !l.trim().is_empty()) {
            changed.push(format!("M {rel_path}"));
        }
    }
    Ok(changed)
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
    let file: std::fs::File =
        match aterm_uds::kept_out(|| crate::metadata_io::open_regular(&dir.join("lock"))) {
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
            } else if let Some(why) = written_words(&cells, since, now, Times::All, git.until()) {
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
            Some(false) => match written_words(&lock, since, now, Times::All, git.until()) {
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
    remove_tree(git, checkout, wt, now, Shape::Snapshot, fence)
}

/// [`remove_snapshot`] for the worktree `wt` of `checkout` judged by the rule `shape` names. A
/// linked worktree ([`Shape::Linked`]) is never taken for one half removed, and when an entry
/// of its tree stays git's entry for it stays too, its `.git` written back: discovery finds a
/// linked worktree only through git's entry, so the next look judges what is left again.
fn remove_tree(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
    shape: Shape,
    fence: &super::fence::Fence<'_>,
) -> Result<(), String> {
    let mut recovery = pending::read(checkout, wt)?;
    // The receipt's start, taken before the judgment's first git read: a write git's last read
    // missed (it had read that folder already) is dated at or after it, and custody below keeps
    // the tree for it.
    let cutoff = pending::file_clock_now();
    judge_tree(git, checkout, wt, now, shape)?;
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
        (None, None) if shape == Shape::Snapshot && half_removed(checkout, wt).is_some() => None,
        (None, None) => return Err(unlisted(checkout)),
    };
    // What a linked worktree ignores now, before its removal can take a `.gitignore`: the one
    // kind of untracked entry a stopped removal may leave ([`removal_only`]).
    let ignored = if shape == Shape::Linked && recovery.is_none() {
        ignored_entries(git, wt)?
    } else {
        Vec::new()
    };
    let prepared = if recovery.is_none()
        && let (Some(entry), Some(listed)) = (&own, &now_listed)
    {
        Some(pending::prepare(
            wt,
            entry,
            &listed.head,
            &text,
            cutoff,
            ignored,
        )?)
    } else {
        None
    };
    if !fence.clear(wt, own.as_deref().unwrap_or(wt)) {
        return Err(USED.to_owned());
    }
    // A receipt this call commits binds a removal that has not begun: whatever keeps the tree
    // before its first entry goes withdraws it, so no later look resumes on its authority
    // (the receipt branch admits less than a whole judgment does).
    let fresh = prepared.is_some();
    if let Some(prepared) = prepared {
        pending::commit(checkout, &prepared)?;
        recovery = Some(prepared);
    }
    let custody: Result<(), String> = 'custody: {
        if fresh && !fence.clear(wt, own.as_deref().unwrap_or(wt)) {
            break 'custody Err(USED.to_owned());
        }
        // The private-history check may read for seconds (up to its own bound): it goes first, so
        // the time and binding checks just below are the last thing before the first deletion.
        if let Err(why) = history::clear(git, checkout, wt, own.as_deref()) {
            break 'custody Err(why);
        }
        if let Some(removal) = &recovery {
            if let Err(why) = pending::revalidate(checkout, wt, removal) {
                break 'custody Err(why);
            }
        } else {
            if let Err(why) = pending::unchanged_since(wt, cutoff, true) {
                break 'custody Err(why);
            }
            if gitfile_text(&gitfile).as_ref() != Some(&text) {
                break 'custody Err("snapshot Git binding changed during admission".to_owned());
            }
        }
        let took = super::remove::tree(&gitfile);
        if !took.whole() {
            let why = super::remove::named(&gitfile, 1)
                .first
                .first()
                .map_or(super::remove::Why::StillRefusing, |(_, why)| *why);
            let kept = super::remove::Named {
                count: took.stayed,
                first: vec![(PathBuf::from(".git"), why)],
            };
            break 'custody Err(format!(
                "{}, so nothing else of it was touched",
                kept.words(wt)
            ));
        }
        Ok(())
    };
    if let Err(why) = custody {
        return Err(match &recovery {
            Some(removal) if fresh => match pending::retract(checkout, wt, removal) {
                Ok(()) => why,
                Err(stayed) => format!("{why}; {stayed}"),
            },
            _ => why,
        });
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
        } else if let Err(reason) = history::clear(git, checkout, wt, Some(entry)) {
            Some(reason)
        } else if left.whole() {
            let wt_arg = git_path(wt).to_string_lossy();
            let dropped = git.run(checkout, &["worktree", "remove", "--force", &wt_arg]);
            (dropped.code != Some(0)).then_some(dropped.err)
        } else if shape == Shape::Linked {
            Some("part of its tree stayed, so git's entry stays for the next look".to_owned())
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

/// Why a pass under `home` does not judge `wt` as a linked worktree of `checkout` at all: a
/// folder it never looks into ([`looked_into`]), a `checkout` that is no main checkout, the
/// retired gate's own snapshot, which `aterm pkg machine apply` judges by its own rule, or a
/// worktree no agent made ([`agent_made`]).
fn worktree_refused(home: &Path, checkout: &Path, wt: &Path) -> Option<String> {
    if !looked_into(home, checkout) || !looked_into(home, wt) {
        return Some(format!(
            "{} lies where a pass never looks (`aterm_toolchain::checkout::never_looked_at`)",
            wt.display()
        ));
    }
    if !main_checkout(checkout) {
        return Some(format!("{} is not a main checkout", checkout.display()));
    }
    let snapshot = beside(checkout, SNAPSHOT);
    let at = pending::location(wt);
    let is_snapshot = snapshot.as_deref() == Some(wt)
        || (at.is_some() && snapshot.as_deref().and_then(pending::location) == at);
    if is_snapshot {
        return Some("the retired gate's snapshot: `aterm pkg machine apply` judges it".to_owned());
    }
    if !agent_made(checkout, wt) {
        return Some(format!(
            "{} is no worktree an agent makes (a `*.noindex` folder with a `.ubd` folder above \
             it, or one in its checkout's .claude/worktrees/): a person's worktree stays",
            wt.display()
        ));
    }
    None
}

/// Whether `wt`, a worktree of `checkout`, has the shape of one aterm's agents make, read off its
/// path alone: its name ends `.noindex` and a folder named `.ubd` lies above it (AGENTS.md's
/// shape for an agent's worktree), or it is a folder of `checkout`'s `.claude/worktrees/`
/// (Claude Code's own isolation worktrees). Only those are the worktree step's to judge: a
/// person's worktree elsewhere (`~/myapp-develop`, a bare repository's `main/`) is their working
/// folder, merged or not.
fn agent_made(checkout: &Path, wt: &Path) -> bool {
    let at = pending::location(wt).unwrap_or_else(|| wt.to_path_buf());
    let Some(name) = at.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let noindex = name.len() > ".noindex".len() && name.ends_with(".noindex");
    let ubd = at.parent().is_some_and(|p| {
        p.ancestors()
            .any(|a| a.file_name() == Some(".ubd".as_ref()))
    });
    if noindex && ubd {
        return true;
    }
    let isolation = resolved(checkout).join(".claude").join("worktrees");
    at.parent() == Some(isolation.as_path())
}

/// Why `checkout`'s repository is no candidate's: a bare one (`git rev-parse
/// --is-bare-repository`), whose every worktree is a person's working folder (the
/// `git clone --bare … proj/.git` layout), or one git cannot say that of.
fn bare_refused(git: &dyn Git, checkout: &Path) -> Result<(), String> {
    let bare = git.run(checkout, &["rev-parse", "--is-bare-repository"]);
    match (bare.code, bare.out.trim()) {
        (Some(0), "false") => Ok(()),
        (Some(0), "true") => Err(format!(
            "{} is a bare repository's folder, whose every worktree is a person's working folder",
            checkout.display()
        )),
        _ => Err(format!(
            "git could not say whether {} is a bare repository: {}",
            checkout.display(),
            bare.err
        )),
    }
}

/// Judge `wt`, a linked worktree of the main checkout `checkout`, as a pass under `home` may:
/// only one an agent made ([`agent_made`]) of a repository that is not bare, by the snapshot's
/// rule ([`plan`]) on a branch or detached, and beyond it no operation in
/// progress, no per-worktree ref, no worktree inside it, no commit only its reflogs hold, and
/// none of git's own files for it written for [`UNWRITTEN`]. Read only: nothing is written.
///
/// # Errors
/// Why it stays, at this observation.
pub fn judge_worktree(
    git: &dyn Git,
    home: &Path,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
) -> Result<Admitted, String> {
    if let Some(why) = worktree_refused(home, checkout, wt) {
        return Err(why);
    }
    bare_refused(git, checkout)?;
    judge_tree(git, checkout, wt, now, Shape::Linked)
}

/// Remove `wt`, a linked worktree of the main checkout `checkout`, judged again just before as
/// [`judge_worktree`] does, under its own durable receipt (`<checkout>/.git/aterm-retiring/`)
/// and the live process-use fence, as [`remove_snapshot`] removes the snapshot. Its branch is
/// not touched. `Ok(())` once its tree is gone and git no longer lists it.
///
/// # Errors
/// Why it, or what is left of it, stays.
pub fn remove_worktree(
    git: &dyn Git,
    home: &Path,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
) -> Result<(), String> {
    remove_worktree_on(
        git,
        home,
        checkout,
        wt,
        now,
        &super::fence::Fence::default(),
    )
}

/// [`remove_worktree`] with a process probe a headless fixture supplies.
///
/// # Errors
/// As [`remove_worktree`].
#[cfg(any(test, debug_assertions))]
pub fn remove_worktree_with_processes(
    git: &dyn Git,
    home: &Path,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
    processes: super::fence::Processes<'_>,
) -> Result<(), String> {
    remove_worktree_on(
        git,
        home,
        checkout,
        wt,
        now,
        &super::fence::Fence::new(processes),
    )
}

fn remove_worktree_on(
    git: &dyn Git,
    home: &Path,
    checkout: &Path,
    wt: &Path,
    now: SystemTime,
    fence: &super::fence::Fence<'_>,
) -> Result<(), String> {
    if let Some(why) = worktree_refused(home, checkout, wt) {
        return Err(why);
    }
    bare_refused(git, checkout)?;
    remove_tree(git, checkout, wt, now, Shape::Linked, fence)
}

/// The worktrees of `checkout` an interrupted removal holds a receipt for
/// (`<checkout>/.git/aterm-retiring/`), at most 64: a removal that took the tree and stopped
/// before git's entry for it leaves no folder discovery can find it by. An unreadable receipt
/// names none, and its worktree stays.
#[must_use]
pub fn retiring(checkout: &Path) -> Vec<PathBuf> {
    pending::listed(checkout)
}

/// Whether a receipt in `checkout`'s git directory names the worktree `wt` ([`retiring`]): its
/// removal stopped part way, maybe before its `.git` was written back, so what is left is judged
/// by its receipt ([`judge_worktree`]), not by what its `.git` says.
#[must_use]
pub fn in_retirement(checkout: &Path, wt: &Path) -> bool {
    pending::location(wt).is_some_and(|at| retiring(checkout).contains(&at))
}

#[cfg(test)]
#[path = "build_cache_retired_tests.rs"]
mod tests;
