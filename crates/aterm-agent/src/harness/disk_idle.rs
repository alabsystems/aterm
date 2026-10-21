// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TIER 3: WHOLE IDLE BUILD DIRECTORIES OF LIVE CHECKOUTS (design
//! `docs/DESIGN-disk-awareness-2026-10-17.md` §4.4) — the one grant over a
//! live checkout's files. Every clause fails closed: a clause that cannot be
//! read keeps the directory. The clauses are the 2026-10-17 hand run's, which
//! freed 864.7 GiB from 104 whole directories, made fd-relative and
//! lock-held:
//!
//! 1. **What it is.** A build directory discovery found on the measured
//!    volume under a git checkout — the worktree root above the build
//!    directory itself, at or under the root discovery found it from (a
//!    `target` link into ANOTHER checkout is never judged by the quiet one it
//!    was reached from) — never an agent's `CARGO_TARGET_DIR` (its agent
//!    runs, and other checkouts may share it), never x.py's bootstrap trees
//!    (hours to rebuild), never one inside another found build directory or
//!    holding one, never one that is its own checkout.
//! 2. **Its marks.** A signed `CACHEDIR.TAG`, no source at its root, every
//!    profile known, and the whole-removal fences (no mount point inside it,
//!    nothing of another user's: `super::whole_fences`).
//! 3. **Idle.** Every profile unwritten for `disk.idle_build_days` (default
//!    seven), on what a compile writes.
//! 4. **The checkout is not being worked in**: (a) no session of this
//!    instance works inside it; (b) no running process of this uid has its
//!    working directory, executable or an open file there
//!    (`super::discover::unused_under`, asked again before the removal and
//!    between profiles); (c) its git `index` and `logs/HEAD` are unwritten for
//!    the window, read, no `git` run; (d) no working-tree file outside build
//!    directories and `.git/` was written within the window, by one bounded
//!    walk ([`tree_quiet`]) — an editor that saves in place moves no git file
//!    and holds no descriptor, but it moves an mtime. A walk cut short keeps
//!    it (in the hand run this clause refused 59 of 104).
//!
//!    Windows sees less of (a) and (b) together than unix does: a shell's
//!    working directory there is (b)'s witness only when the shell moves its
//!    process's own (`cmd`'s `cd` does), and PowerShell's `Set-Location`
//!    does not (measured 2026-10-19), so a PowerShell prompt is seen only
//!    through the OSC 7 report of a tab of THIS instance (a). One sitting in
//!    a checkout in another terminal — Windows Terminal, an editor's, another
//!    aterm instance — is no witness, and only (c) and (d) speak for it: a
//!    prompt that ran nothing in the checkout for the whole window loses only
//!    a rebuild. A tab of this instance whose working directory is not known
//!    refuses every candidate ([`Context::cwd_unknown`]).
//! 5. **Nothing installed points into it** ([`inbound_links`]): no link in
//!    `~/.local/bin`, `~/.cargo/bin`, `$ATPKG_BIN` or rustup's linked
//!    toolchains resolves inside it — on Windows, where atpkg installs by
//!    `.cmd` shim, no shim there runs a program inside it either. Links are
//!    resolved one component at a time, each step only where the mount table
//!    places it on a local, non-automounted volume ([`resolve_local`]): a
//!    link into a stalled network share is never followed, so it cannot hang
//!    the watch.
//!
//! The removal is `super::target::reclaim_profiles`: each profile EMPTIED of
//! cargo's own outputs under all of its locks, its directory and lock files
//! kept, nothing at the build directory's root that is not a profile touched;
//! clause 4 is read again at the removal — 4a from the instance's sessions as
//! they stand then ([`Sessions`], never the look's copy) — and 4a, 4b and 4c
//! once more under each profile's locks, right before its delete. On Windows
//! 4a is the only witness of a PowerShell tab of this instance, so a tab that
//! moved into the checkout during the look, or one whose working directory
//! became unknown, keeps it at the removal.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_sysprobe::volumes::Table;

use super::discover::Origin;
use super::target::{CARGO_LOCKS, TargetDir, human_age};
use super::{MAX_WALK_ENTRIES, RUSTC_INFO, cachedir_tag_signed, mtime_of};

/// `disk.idle_build_days`: a build directory every profile of which is
/// unwritten this many days, its checkout quiet as long, is tier 3's. The
/// hand run used fourteen; atpkg's own cache takes any build directory at
/// thirty. Below one turns tier 3 off.
pub const DEFAULT_IDLE_BUILD_DAYS: i64 = 7;

/// The working-tree walk's wall-clock bound ([`tree_quiet`]).
pub const TREE_WALL: Duration = Duration::from_secs(10);

/// The most entries one listing of a link folder reads ([`inbound_links`]).
pub const MAX_LINKS: usize = 4096;

/// What makes one build directory a tier-3 candidate, as the survey read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleFacts {
    /// The checkout it serves (for a worktree, the worktree's root): its
    /// working tree, its git files and its processes are what was judged.
    pub checkout: PathBuf,
    /// The newest last use of its profiles (unix seconds).
    pub last_used: i64,
    /// How many profiles it holds.
    pub profiles: usize,
    /// The bytes its profiles hold (what emptying them releases, as far as the
    /// walk got) and whether that is a floor.
    pub bytes: (u64, bool),
}

/// What the assessment reads beyond one build directory ([`assess`]).
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// Unix seconds.
    pub now: i64,
    /// The idle window, in seconds (`disk.idle_build_days`).
    pub window_s: i64,
    /// The measured volume's device: a build directory on another is not one.
    pub device: Option<u64>,
    /// The working directory of every session of this instance (clause 4a),
    /// each as it could be resolved ([`SessionCwd`]). Only this instance's:
    /// on Windows a PowerShell prompt in another terminal is seen by no
    /// clause but 4c and 4d (the module doc says why).
    pub session_cwds: &'a [SessionCwd],
    /// Why the working directory of some session of this instance is not
    /// known (on Windows a live tab that reported no absolute native one):
    /// while it stands no build directory is a candidate, as that session
    /// may work in any of them.
    pub cwd_unknown: Option<&'a str>,
    /// What the installed links resolve to ([`inbound_links`]), or why they
    /// could not be read (then no build directory is a candidate).
    pub links: &'a Result<Vec<PathBuf>, String>,
    /// Every build directory the survey holds: a candidate is inside none and
    /// holds none, and the working-tree walk passes over them.
    pub found: &'a [PathBuf],
    /// `$HOME`: never a checkout (a search from it, or a dotfiles repository
    /// there, would make every folder of it one working tree).
    pub home: Option<&'a Path>,
    /// The mount table: a git directory is read only where it places it on a
    /// local, non-automounted volume ([`git_quiet`]), and on Windows every
    /// path of two sources is placed by it ([`under`]).
    pub mounts: Option<&'a Table>,
}

/// This instance's sessions as clause 4a reads them at one moment: the
/// working directory each reported, as reported, and why some session's is
/// not known. The survey reads them once ([`Context`]); the removal reads
/// them AGAIN, from a reader the instance hands it
/// (`super::remover_among`), before it opens anything and under each
/// profile's locks — a look lasts the survey's wall and every walk before
/// the delete, and a session can move into the checkout meanwhile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sessions {
    /// Each session's working directory, as it reported it.
    pub cwds: Vec<PathBuf>,
    /// Why the working directory of some session is not known (on Windows a
    /// live tab that reported no absolute native one): while it stands no
    /// build directory is a candidate.
    pub unknown: Option<String>,
}

impl Sessions {
    /// Clause 4a for the build directory `dir` of `checkout` over these
    /// sessions ([`sessions_clear`]): a session whose working directory is
    /// unknown refuses before any folder is opened; else each working
    /// directory is resolved now, only where `table` places every step of it
    /// on a local, non-automounted volume ([`SessionCwd::on`]). One not
    /// resolved is compared as written on unix; on Windows it keeps every
    /// candidate of its volume, and all of them when it names none
    /// ([`SessionCwd::may_work_in`]).
    ///
    /// # Errors
    ///
    /// Why a session may work in either.
    pub fn clear(&self, checkout: &Path, dir: &Path, table: Option<&Table>) -> Result<(), String> {
        if let Some(why) = &self.unknown {
            return Err(why.clone());
        }
        let cwds: Vec<SessionCwd> = self.cwds.iter().map(|c| SessionCwd::on(c, table)).collect();
        sessions_clear(&cwds, None, checkout, dir, table)
    }
}

/// Clause 4a for the build directory `dir` of `checkout`: no session whose
/// working directory is unknown stands (`unknown`, the reason), and none of
/// `cwds` may work in either ([`SessionCwd::may_work_in`]: one that cannot be
/// placed may). The one rule of the survey ([`assess`]) and the removal
/// ([`Sessions::clear`]).
///
/// # Errors
///
/// Why a session may work in either.
pub fn sessions_clear(
    cwds: &[SessionCwd],
    unknown: Option<&str>,
    checkout: &Path,
    dir: &Path,
    table: Option<&Table>,
) -> Result<(), String> {
    if let Some(why) = unknown {
        return Err(why.to_owned());
    }
    if let Some(cwd) = cwds
        .iter()
        .find(|c| c.may_work_in(checkout, table) || c.may_work_in(dir, table))
    {
        return Err(format!("a session works in {}", cwd.path().display()));
    }
    Ok(())
}

/// One session's working directory as clause 4a reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCwd {
    /// Resolved — by [`SessionCwd::on`] link by link on local volumes
    /// ([`resolve_local`]: on Windows from the drive as the session wrote
    /// it, plain `C:\…` where a canonical path is `\\?\C:\…`, and verbatim
    /// once an 8.3 short name in it was spelled long, as the long spelling
    /// answers: `\\?\C:\…\Long Folder\…`), by [`SessionCwd::of`] through
    /// `std::fs::canonicalize` — and placed against a candidate by the one
    /// path rule ([`under`]), never by its text, so either spelling places
    /// alike; one it cannot place keeps the candidate.
    Placed(PathBuf),
    /// As written, not resolved: gone since, unreadable, on a volume the
    /// watch never opens or reached through a link onto one
    /// ([`SessionCwd::on`]: a network share, an automount), or read with no
    /// mount table to say which volumes those are. Unix compares it as
    /// written; Windows, where the text may be any spelling of the folder
    /// (an 8.3 name, a `subst` letter, a junction) that does not resolve,
    /// keeps every candidate on its volume
    /// ([`aterm_sysprobe::volumes::mount_of`]), and every candidate when its
    /// volume cannot be named (a share's cannot).
    Unresolved(PathBuf),
}

impl SessionCwd {
    /// `cwd` resolved where it can be (`std::fs::canonicalize`, an open of
    /// the folder), with no system box on the desktop for a folder on an
    /// empty removable drive ([`aterm_sysprobe::quiet_errors`]). It opens
    /// the folder wherever it is — a share, an automount — so the survey and
    /// the removal read through [`SessionCwd::on`]; this is for a caller
    /// that holds no mount table.
    #[must_use]
    pub fn of(cwd: &Path) -> SessionCwd {
        let _quiet = aterm_sysprobe::quiet_errors();
        std::fs::canonicalize(cwd).map_or_else(
            |_| SessionCwd::Unresolved(cwd.to_path_buf()),
            SessionCwd::Placed,
        )
    }

    /// `cwd` as clause 4a reads it, in the survey and at the removal alike:
    /// resolved ONLY on local, non-automounted volumes, link by link as
    /// clause 5 resolves an installed link ([`resolve_local`]) — each
    /// component looked at only once `table` places the path so far on such
    /// a volume, a link onto a share or an automount never followed, each
    /// 8.3 short name spelled long by the folder it is in — with no system
    /// box for an empty drive ([`aterm_sysprobe::quiet_errors`]). Anything
    /// else is [`SessionCwd::Unresolved`], never opened: a folder the table
    /// places (as written, or spelled long) on a share or an automount, one
    /// reached through a link onto one, one the table cannot place (a share
    /// spelled `\\nas\share\PROJEC~1`, whose short name only the server
    /// could spell), and every one when there is no table, or one not read
    /// whole, to say which volumes are local.
    ///
    /// Why never `std::fs::canonicalize`: it opens the folder, and the
    /// removal reads its sessions again under each profile's locks — an open
    /// on a stalled share waits out the redirector's timeout (on a hard NFS
    /// mount, for good) with cargo's locks held, one on an automount mounts
    /// it, and a folder that only LOOKS local (a short name, a link) can
    /// lead to either. The cost: such a working directory is not resolved.
    /// On Windows it keeps every candidate of its volume, and every
    /// candidate when it names none (a share's); on unix it is compared as
    /// written, placed by its text, and a link on the share back into a
    /// local checkout is not followed — there the shell's own working
    /// directory, which the kernel resolved, is clause 4b's witness. With no
    /// table, or one not read whole, nothing is a candidate anyway: the
    /// survey's fences refuse on such a table (`super::whole_fences`: no
    /// mount point inside it is ruled out), and so does the removal, on the
    /// very table it reads its sessions on (`super::remover`).
    #[must_use]
    pub fn on(cwd: &Path, table: Option<&Table>) -> SessionCwd {
        let _quiet = aterm_sysprobe::quiet_errors();
        table.and_then(|t| resolve_local(cwd, t)).map_or_else(
            || SessionCwd::Unresolved(cwd.to_path_buf()),
            SessionCwd::Placed,
        )
    }

    /// The path, for a line.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            SessionCwd::Placed(p) | SessionCwd::Unresolved(p) => p,
        }
    }

    /// May a session working here work inside `dir` (clause 4a)? Unknown is
    /// yes.
    #[must_use]
    pub fn may_work_in(&self, dir: &Path, table: Option<&Table>) -> bool {
        match self {
            SessionCwd::Placed(cwd) => under(cwd, dir, table) != Some(false),
            SessionCwd::Unresolved(cwd) if cfg!(windows) => {
                if under(cwd, dir, table) == Some(true) {
                    return true;
                }
                let volume = |p: &Path| {
                    table
                        .and_then(|t| aterm_sysprobe::volumes::mount_of(p, t))
                        .and_then(aterm_sysprobe::volumes::Mount::key)
                };
                match (volume(cwd), volume(dir)) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                }
            }
            SessionCwd::Unresolved(cwd) => cwd.starts_with(dir),
        }
    }
}

/// Is `path` at or under `dir`, by the one rule for paths of two sources?
/// On Windows the mount table's, when it was read
/// ([`Table::is_under`]: any spelling, any case, a `subst` letter followed),
/// else the key's ([`aterm_sysprobe::paths::is_under`]); by component
/// elsewhere, as before. `None`: it cannot be placed, which every caller
/// reads as IN USE — never as "not under".
#[must_use]
pub fn under(path: &Path, dir: &Path, table: Option<&Table>) -> Option<bool> {
    match table {
        Some(t) if cfg!(windows) => t.is_under(path, dir),
        _ => aterm_sysprobe::paths::is_under(path, dir),
    }
}

/// Are `a` and `b` one path, by [`under`] both ways? `None`: either way
/// cannot be placed.
fn same_path(a: &Path, b: &Path, table: Option<&Table>) -> Option<bool> {
    Some(under(a, b, table)? && under(b, a, table)?)
}

/// The checkout a build directory found as `origin` serves: the nearest git
/// worktree root above the (canonical) build directory itself (a search from
/// `$HOME` finds `~/aterm/target-x` under `~`, and its checkout is
/// `~/aterm`), else the root discovery found it under — which [`assess`]
/// then refuses unless it is that very worktree ([`fits_root`]). `None`: an
/// origin tier 3 never takes from — an agent's `CARGO_TARGET_DIR` (only a
/// running agent names one, and other checkouts may build there), x.py's
/// trees, a link's, a session's scratch build, a named one, a build cache
/// mirror (those are tier 1's when their owner is gone).
#[must_use]
pub fn checkout_of(origin: &Origin, build_dir: &Path) -> Option<PathBuf> {
    match origin {
        Origin::Checkout { root } => Some(
            build_dir
                .parent()
                .and_then(super::discover::worktree_root_of)
                .unwrap_or_else(|| root.clone()),
        ),
        Origin::AgentTargetDir
        | Origin::Named
        | Origin::BuildCache { .. }
        | Origin::Bootstrap { .. }
        | Origin::ClaudeScratch { .. }
        | Origin::Linked { .. } => None,
    }
}

/// Clause 1's check of the checkout tier 3 judges: a git worktree root
/// (clause 4c has git files to read), not the build directory itself, and
/// the one discovery reached the build directory from — at or under its
/// root, or that root's own worktree. A `~/a-wt/target` linking into
/// `~/z-main/target.noindex` is canonical under `~/z-main`, which `~/a-wt`'s
/// quiet says nothing of.
///
/// Paths are compared by the one path rule without a table
/// ([`aterm_sysprobe::paths::is_under`]: on Windows any case, a verbatim or a
/// plain spelling); one it cannot place does not fit.
///
/// # Errors
///
/// Why the checkout is not one tier 3 can judge.
pub fn fits_root(origin: &Origin, build_dir: &Path, checkout: &Path) -> Result<(), String> {
    if same_path(checkout, build_dir, None) != Some(false)
        || under(build_dir, checkout, None) != Some(true)
    {
        return Err(format!(
            "its checkout would be {}, which does not hold it as a build directory",
            checkout.display()
        ));
    }
    if super::discover::worktree_root_of(checkout).as_deref() != Some(checkout) {
        return Err(format!(
            "no git checkout holds it ({} is none), so whether it is worked in cannot be read",
            checkout.display()
        ));
    }
    if let Origin::Checkout { root } = origin {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        let at = super::discover::worktree_root_of(&root);
        if under(checkout, &root, None) != Some(true) && at.as_deref() != Some(checkout) {
            return Err(format!(
                "it lies in the checkout {}, not the one it was found from ({}): reached through \
                 a link, it is not judged by that one's quiet",
                checkout.display(),
                root.display()
            ));
        }
    }
    Ok(())
}

/// Clauses 1-3, 4a, 4c, 4d and 5 for the build directory `t` (clause 4b, the
/// running processes, is one scan for every candidate: [`processes_clear`]),
/// with the whole-removal fences `fences`.
///
/// # Errors
///
/// The first clause it fails, as why tier 3 keeps it.
pub fn assess(
    t: &TargetDir,
    cx: &Context<'_>,
    fences: &dyn Fn(&TargetDir) -> Result<(), String>,
) -> Result<IdleFacts, String> {
    let checkout = checkout_of(&t.origin, &t.path)
        .ok_or("tier 3 takes only a git checkout's build directory")?;
    // `$HOME` is never a checkout: one that cannot be told apart from it
    // is taken for it.
    let is_home = |h: &Path| {
        let h = std::fs::canonicalize(h).unwrap_or_else(|_| h.to_path_buf());
        same_path(&h, &checkout, cx.mounts) != Some(false)
    };
    if cx.home.is_some_and(is_home) || checkout.parent().is_none() {
        return Err(format!(
            "its checkout would be {}, which is no checkout",
            checkout.display()
        ));
    }
    fits_root(&t.origin, &t.path, &checkout)?;
    if cx.device.is_none() || t.device != cx.device {
        return Err("it is not on the measured volume".to_owned());
    }
    // Nested either way, or not to be told apart: a build directory one
    // cannot place against another is taken for nested.
    if cx.found.iter().any(|o| {
        same_path(o, &t.path, cx.mounts) != Some(true)
            && (under(&t.path, o, cx.mounts) != Some(false)
                || under(o, &t.path, cx.mounts) != Some(false))
    }) {
        return Err("a build directory is inside it, or it is inside one".to_owned());
    }
    let marker = t.fences()?;
    if marker != "CACHEDIR.TAG" {
        return Err(format!(
            "only a signed CACHEDIR.TAG lets a whole build directory go, and it carries {marker}"
        ));
    }
    fences(t)?;
    if t.profiles.is_empty() {
        return Err("it holds no cargo profile".to_owned());
    }
    let mut newest = i64::MIN;
    for p in &t.profiles {
        let Some(m) = p.last_used else {
            return Err(format!(
                "{}: its caches' clock could not be read",
                p.path.display()
            ));
        };
        newest = newest.max(m);
    }
    let age = cx.now.saturating_sub(newest);
    if age < cx.window_s {
        return Err(format!(
            "a profile was written into {} ago, inside the {} window",
            human_age(age),
            human_age(cx.window_s)
        ));
    }
    sessions_clear(
        cx.session_cwds,
        cx.cwd_unknown,
        &checkout,
        &t.path,
        cx.mounts,
    )?;
    match cx.links {
        Err(why) => return Err(why.clone()),
        Ok(targets) => {
            if let Some(to) = targets
                .iter()
                .find(|to| under(to, &t.path, cx.mounts) != Some(false))
            {
                return Err(format!(
                    "an installed link resolves into it ({})",
                    to.display()
                ));
            }
        }
    }
    git_quiet(&checkout, cx.now, cx.window_s, cx.mounts)?;
    tree_quiet(&checkout, cx.found, cx.now, cx.window_s, TREE_WALL)?;
    let mut bytes = (0u64, false);
    for p in &t.profiles {
        let (b, partial) = super::target::total_bytes(&p.path);
        bytes = (bytes.0.saturating_add(b), bytes.1 || partial);
    }
    Ok(IdleFacts {
        checkout,
        last_used: newest,
        profiles: t.profiles.len(),
        bytes,
    })
}

/// Clause 4b for every candidate at once: one scan of the running processes
/// (`processes`) over each candidate's checkout and build directory, each
/// candidate answered with who uses it — or, for every candidate, why the
/// scan could not rule anyone out. A use is placed against a candidate by
/// the one path rule over `table` ([`under`]: on Windows the kernel's
/// `C:\X\` against a canonical `\\?\C:\x`), in the spelling the scan placed
/// it by ([`aterm_uds::holders::PathUse::path`]: on Windows normalized and
/// each 8.3 short name spelled long), so a working directory spelled short
/// inside one candidate keeps that one alone; one it cannot place is counted
/// for every candidate.
#[must_use]
pub fn processes_clear(
    candidates: &[(PathBuf, PathBuf)],
    processes: super::discover::Processes<'_>,
    table: Option<&Table>,
) -> Vec<Result<(), String>> {
    let dirs: Vec<PathBuf> = candidates
        .iter()
        .flat_map(|(checkout, dir)| [checkout.clone(), dir.clone()])
        .collect();
    if dirs.is_empty() {
        return Vec::new();
    }
    let scan = match processes(&dirs, &[std::process::id()]) {
        Ok(scan) => scan,
        Err(e) => {
            let why = format!("the running processes could not be listed: {e}");
            return candidates.iter().map(|_| Err(why.clone())).collect();
        }
    };
    let unread = scan
        .unreadable
        .iter()
        .find(|(_, exe)| !super::discover::platform_service(exe.as_deref()));
    candidates
        .iter()
        .map(|(checkout, dir)| {
            if let Some(u) = scan.users.iter().find(|u| {
                under(&u.path, checkout, table) != Some(false)
                    || under(&u.path, dir, table) != Some(false)
            }) {
                return Err(format!(
                    "process {} uses {} (its {})",
                    u.pid,
                    u.path.display(),
                    match u.how {
                        "cwd" => "working directory",
                        "exe" => "executable",
                        _ => "open file",
                    }
                ));
            }
            if let Some((pid, exe)) = unread {
                return Err(format!(
                    "process {pid}{} could not be read, so it is not ruled out",
                    exe.as_ref()
                        .map(|e| format!(" ({})", e.display()))
                        .unwrap_or_default()
                ));
            }
            Ok(())
        })
        .collect()
}

/// Clause 4c: the checkout's git `index` and `logs/HEAD` (a worktree's in
/// the git directory its `.git` file names) unwritten for `window_s` at
/// `now`. Read, no `git` run. A checkout with no `.git` has nothing here to
/// say (clause 4d still walks it). A git directory a `.git` file names is
/// read only once [`resolve_local`] places it, every step, on a local,
/// non-automounted volume of `mounts` (a stalled share must not hang the
/// watch); one it cannot place keeps the directory.
///
/// # Errors
///
/// One was written within the window, or the `.git` could not be read, or
/// the git directory it names could not be placed on a local volume.
pub fn git_quiet(
    checkout: &Path,
    now: i64,
    window_s: i64,
    mounts: Option<&Table>,
) -> Result<(), String> {
    let dot = checkout.join(".git");
    let gitdir = match std::fs::symlink_metadata(&dot) {
        Ok(m) if m.is_dir() => dot,
        Ok(m) if m.is_file() => {
            let text = std::fs::read(&dot)
                .ok()
                .filter(|b| b.len() <= 4096)
                .and_then(|b| String::from_utf8(b).ok())
                .ok_or_else(|| format!("{} could not be read", dot.display()))?;
            let named = text
                .lines()
                .find_map(|l| l.strip_prefix("gitdir:"))
                .map(|p| PathBuf::from(p.trim()))
                .ok_or_else(|| format!("{} names no git directory", dot.display()))?;
            let named = if named.is_absolute() {
                named
            } else {
                checkout.join(named)
            };
            mounts
                .and_then(|t| resolve_local(&named, t))
                .ok_or_else(|| {
                    format!(
                        "its git directory {} could not be placed on a local volume, so it is \
                         not read",
                        named.display()
                    )
                })?
        }
        Ok(_) => {
            return Err(format!(
                "{} is a link, which is not followed",
                dot.display()
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{} could not be read: {e}", dot.display())),
    };
    for f in ["index", "logs/HEAD"] {
        let path = gitdir.join(f);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let Some(m) = mtime_of(&path) else {
                    return Err(format!("{} could not be dated", path.display()));
                };
                let age = now.saturating_sub(m);
                if age < window_s {
                    return Err(format!(
                        "its git {f} was written {} ago, inside the {} window",
                        human_age(age),
                        human_age(window_s)
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{} could not be read: {e}", path.display())),
        }
    }
    Ok(())
}

/// Clause 4d: nothing of `checkout`'s working tree outside build directories
/// and `.git` — no file, no folder — written within `window_s` at `now`. One
/// bounded walk: at most [`MAX_WALK_ENTRIES`] entries and `wall` of clock,
/// never through a link, never onto another device, never into `skip` (the
/// survey's build directories) or a folder carrying a build tool's marker.
///
/// # Errors
///
/// What was written within the window, or why the walk could not finish: a
/// walk cut short keeps the directory.
pub fn tree_quiet(
    checkout: &Path,
    skip: &[PathBuf],
    now: i64,
    window_s: i64,
    wall: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    let root = std::fs::symlink_metadata(checkout)
        .map_err(|e| format!("{} could not be read: {e}", checkout.display()))?;
    let dev = device(checkout, &root)
        .ok_or_else(|| format!("{}'s volume could not be named", checkout.display()))?;
    let mut seen = 0usize;
    let mut stack = vec![checkout.to_path_buf()];
    let fresh = |path: &Path, meta: &std::fs::Metadata| -> Result<(), String> {
        let m = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .ok_or_else(|| format!("{} could not be dated", path.display()))?;
        let age = now.saturating_sub(m);
        if age < window_s {
            return Err(format!(
                "{} in its working tree was written {} ago, inside the {} window",
                path.display(),
                human_age(age),
                human_age(window_s)
            ));
        }
        Ok(())
    };
    while let Some(dir) = stack.pop() {
        if started.elapsed() >= wall {
            return Err("the walk of its working tree ran out of time".to_owned());
        }
        let rd = std::fs::read_dir(&dir)
            .map_err(|e| format!("{} could not be read: {e}", dir.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| format!("{} could not be read: {e}", dir.display()))?;
            seen += 1;
            super::beat::tick(1);
            if seen > MAX_WALK_ENTRIES {
                return Err(format!(
                    "its working tree holds more than {MAX_WALK_ENTRIES} entries"
                ));
            }
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("{} could not be read: {e}", path.display()))?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                let name = entry.file_name();
                if name == ".git" {
                    continue;
                }
                // The device first, before anything under the folder is
                // opened (a build marker): a folder on another volume — an
                // NFS, FUSE or autofs mount inside the tree — is passed over
                // without a look inside, which could mount it or block. One
                // whose volume cannot be named is not walked past: what it
                // holds could have been written.
                match device(&path, &meta) {
                    Some(d) if d == dev => {}
                    Some(_) => continue,
                    None => {
                        return Err(format!("{}'s volume could not be named", path.display()));
                    }
                }
                if skip.iter().any(|s| s == &path) || build_marked(&path) {
                    continue;
                }
                fresh(&path, &meta)?;
                stack.push(path);
            } else if meta.is_file() {
                if CARGO_LOCKS.iter().any(|l| entry.file_name() == *l) {
                    continue;
                }
                fresh(&path, &meta)?;
            }
        }
    }
    Ok(())
}

/// The device the entry at `path` (read as `meta`, a link not followed) is
/// on: `st_dev`; on Windows the volume serial of the one identity export
/// ([`aterm_sysprobe::volumes::file_id`], read through a handle, the width
/// every device fence compares). `None`: it could not be read (a FAT volume
/// keeps no file ids).
fn device(path: &Path, meta: &std::fs::Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = path;
        Some(meta.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        aterm_sysprobe::volumes::file_id(path, false).map(|id| id.dev)
    }
}

/// Does the folder at `dir` carry a build tool's marker (it is a build
/// directory, whose writes are a build's, not an edit)?
fn build_marked(dir: &Path) -> bool {
    cachedir_tag_signed(&dir.join("CACHEDIR.TAG"))
        || std::fs::symlink_metadata(dir.join(RUSTC_INFO)).is_ok_and(|m| m.is_file())
}

/// The most links [`resolve_local`] follows for one path.
const MAX_HOPS: usize = 40;

/// `path` with every link on it resolved, ONE COMPONENT AT A TIME, each
/// component looked at (`lstat`, `readlink`) only once `table` — read whole
/// — places the path so far on a local, non-automounted mount: a stalled
/// network share or an automount trigger is never touched, so this cannot
/// block where `std::fs::canonicalize` would. What it answers lies on such a
/// mount too (a link onto a share's root is no local folder). `None`: a step
/// lands on such a mount, the table was not read whole, a component is
/// missing or could not be read, or more than [`MAX_HOPS`] links.
///
/// On Windows a component spelled as an 8.3 short name (`PROJEC~1`) is
/// spelled long before it is placed ([`Table::long_spelling`]), asked of the
/// folder the walk stands in — already placed local and resolved, so the
/// listing that spells it touches no other volume. Unspelled, the mount
/// table cannot place the name at all: a folder a session reported short
/// would never resolve. The answer is spelled long throughout.
#[must_use]
pub fn resolve_local(path: &Path, table: &Table) -> Option<PathBuf> {
    resolve_local_reading(path, table).ok().flatten()
}

/// A local resolution, a positively remote/automounted path (`Ok(None)`),
/// or an unknown reading. The inbound-link fence must keep unknown distinct.
fn resolve_local_reading(path: &Path, table: &Table) -> std::io::Result<Option<PathBuf>> {
    use std::collections::VecDeque;
    use std::path::Component;
    if !table.complete || !path.is_absolute() {
        return Err(std::io::Error::other(
            "the path or complete mount table could not be read",
        ));
    }
    let mut todo: VecDeque<std::ffi::OsString> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_os_string()),
            Component::ParentDir => Some("..".into()),
            _ => None,
        })
        .collect();
    // The root the walk starts from, and goes back to for an absolute link:
    // `/`; on Windows the path's own drive or share and its root separator
    // (`C:\`, `\\?\C:\`, `\\srv\share\`).
    let root_of = |p: &Path| -> PathBuf {
        p.components()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .collect()
    };
    let mut at = root_of(path);
    let mut hops = 0usize;
    while let Some(part) = todo.pop_front() {
        if part == ".." {
            at.pop();
            continue;
        }
        // `at` is placed local and holds no link: the one folder an 8.3 name
        // in `part` is spelled long by is asked (identity off Windows).
        let next = table.long_spelling(&at.join(&part)).into_owned();
        if !local_at(&next, table)? {
            return Ok(None);
        }
        let meta = std::fs::symlink_metadata(&next).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("{} could not be resolved: {error}", next.display()),
            )
        })?;
        if !meta.file_type().is_symlink() {
            at = next;
            continue;
        }
        hops += 1;
        if hops > MAX_HOPS {
            return Err(std::io::Error::other(format!(
                "{} has too many link hops",
                path.display()
            )));
        }
        let to = std::fs::read_link(&next).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("{} could not be read: {error}", next.display()),
            )
        })?;
        if to.is_absolute() {
            at = root_of(&to);
        } else if to.has_root() {
            // Windows' `\x`: the root of the drive the link is on.
            at = root_of(&at);
        } else if matches!(to.components().next(), Some(Component::Prefix(_))) {
            // Windows' `C:x`: relative to a drive's current directory,
            // which is the reading process's, not the link's.
            return Err(std::io::Error::other(format!(
                "{} names a drive-relative path",
                next.display()
            )));
        }
        for c in to.components().rev() {
            match c {
                Component::Normal(n) => todo.push_front(n.to_os_string()),
                Component::ParentDir => todo.push_front("..".into()),
                _ => {}
            }
        }
    }
    // A link onto a root (`\\srv\share\`), or a `..` back up onto one, ends
    // the walk on a folder no step placed.
    Ok(local_at(&at, table)?.then_some(at))
}

/// Does `table` place `path` on a local, non-automounted mount? An error when
/// it places it on none.
fn local_at(path: &Path, table: &Table) -> std::io::Result<bool> {
    let m = aterm_sysprobe::volumes::mount_of(path, table).ok_or_else(|| {
        std::io::Error::other(format!("{} could not be placed on a mount", path.display()))
    })?;
    Ok(m.local && !m.automounted)
}

/// Clause 5's facts: where every link in `dirs` (`~/.local/bin`,
/// `~/.cargo/bin`, `$ATPKG_BIN`, `~/.rustup/toolchains`) points — as written
/// (a relative one against its folder) and, where [`resolve_local`] resolves
/// it on local volumes of `table`, resolved (a link that leads onto a share
/// or an automount is not followed there: what it reaches is not on the
/// measured volume). On Windows an atpkg `.cmd` shim is such a link to the
/// program it runs ([`cmd_shim_target`]), recorded resolved, as the
/// candidates are spelled ([`shim_link`]). A folder that is not there says
/// nothing; each listing reads at most [`MAX_LINKS`] entries.
///
/// # Errors
///
/// A folder that is there could not be listed, held more than [`MAX_LINKS`],
/// an entry, local link or shim could not be read/resolved (a shim naming a
/// relative program too: where it lands is the caller's), or no mount table was
/// read whole: then no build directory is a candidate. Each enumeration folder
/// is resolved before it is listed; a positively missing folder is skipped,
/// while an unreadable, remote or automounted folder keeps every candidate.
/// Relative targets use that resolved physical folder. A remote or automounted
/// component is not followed either: it cannot prove the final destination is
/// elsewhere (later `..` components or a link can lead back to a local path).
pub fn inbound_links(dirs: &[PathBuf], table: Option<&Table>) -> Result<Vec<PathBuf>, String> {
    let Some(table) = table.filter(|t| t.complete) else {
        return Err(
            "the mount table was not read whole, so no installed link is followed".to_owned(),
        );
    };
    let mut out = Vec::new();
    for dir in dirs {
        // Classify every component, including symlink destinations, before
        // listing the folder: read_dir would otherwise follow a remote link.
        let placed = match resolve_local_reading(dir, table) {
            Ok(Some(placed)) => placed,
            Ok(None) => {
                return Err(format!(
                    "{} could not be resolved without following a remote or automounted directory",
                    dir.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        // The installed directory may itself point into a build profile;
        // its ordinary files must keep that profile even without any symlink.
        out.push(placed.clone());
        let rd = match std::fs::read_dir(&placed) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{} could not be listed: {e}", dir.display())),
        };
        for (n, entry) in rd.enumerate() {
            if n >= MAX_LINKS {
                return Err(format!(
                    "{} holds more than {MAX_LINKS} entries, too many to read",
                    dir.display()
                ));
            }
            let entry = entry.map_err(|e| format!("{} could not be listed: {e}", dir.display()))?;
            let kind = entry.file_type().map_err(|error| {
                format!(
                    "{} could not be classified: {error}",
                    entry.path().display()
                )
            })?;
            if !kind.is_symlink() {
                // Windows installs by SHIM, not by link: atpkg's `bin\<tool>.cmd`
                // runs `@"<program>" %*`, and nothing in that folder is a symlink.
                // A shim naming a program counts as a link to it.
                #[cfg(windows)]
                if let Some(to) = cmd_shim_target(&entry.path(), kind)? {
                    let shim = entry.path();
                    if !to.is_absolute() {
                        return Err(format!(
                            "{} runs {}, whose place is wherever it is run from",
                            shim.display(),
                            to.display()
                        ));
                    }
                    // Placed first, so no remote or automounted step is
                    // followed; then recorded as the candidates are spelled.
                    if resolve_local_reading(&to, table)
                        .map_err(|error| error.to_string())?
                        .is_none()
                    {
                        return Err(format!(
                            "{} could not be resolved without following a remote or \
                             automounted path",
                            shim.display()
                        ));
                    }
                    out.push(shim_link(&shim, &to)?);
                }
                continue;
            }
            let link = entry.path();
            let Ok(to) = std::fs::read_link(&link) else {
                return Err(format!("{} could not be read", link.display()));
            };
            out.push(if to.is_absolute() {
                to
            } else {
                placed.join(to)
            });
            match resolve_local_reading(&link, table).map_err(|error| error.to_string())? {
                Some(real) => out.push(real),
                None => {
                    return Err(format!(
                        "{} could not be resolved without following a remote or automounted path",
                        link.display()
                    ));
                }
            }
        }
    }
    Ok(out)
}

/// The program the `.cmd` shim at `path` (listed as `kind`) runs, as atpkg
/// itself lays and reads it ([`atpkg::platform::read_shim_target`]):
/// `Ok(None)` for an entry that is no shim — not a regular file, not named
/// `*.cmd` (in any case: the folder's names are case-insensitive), a tombstone
/// or any other batch file without atpkg's forward line.
///
/// # Errors
///
/// A `.cmd` that is there but could not be read (over atpkg's bound, a
/// reparse point, a sharing refusal): what it runs is unknown, so clause 5
/// keeps every candidate.
#[cfg(windows)]
fn cmd_shim_target(path: &Path, kind: std::fs::FileType) -> Result<Option<PathBuf>, String> {
    let named_cmd = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd"));
    if !kind.is_file() || !named_cmd {
        return Ok(None);
    }
    atpkg::platform::read_shim_target(path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()))
}

/// The link clause 5 records for the shim at `shim` running the absolute
/// program `to`: `to` RESOLVED, as `std::fs::canonicalize` spells it — the
/// spelling the candidates' own paths have (on Windows the verbatim
/// `\\?\C:\…`, each name in its case on disk), so the clause's `starts_with`
/// compares like with like; the shim's text (`C:\…`, any case) never matches
/// one. A program that cannot be resolved (gone, unreadable) is unknown, so
/// every candidate is kept.
///
/// # Errors
///
/// The program could not be resolved.
#[cfg(windows)]
fn shim_link(shim: &Path, to: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(to).map_err(|error| {
        format!(
            "{} runs {}, which could not be resolved: {error}",
            shim.display(),
            to.display()
        )
    })
}

/// The folders clause 5 reads for `home` ([`inbound_links`]): `~/.local/bin`,
/// `~/.cargo/bin`, atpkg's `bin/` beside its `store` (`$ATPKG_BIN`) and
/// rustup's toolchains (a linked one is a link there).
#[must_use]
pub fn link_dirs(home: &Path, store: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        home.join(".local").join("bin"),
        home.join(".cargo").join("bin"),
        home.join(".rustup").join("toolchains"),
    ];
    if let Some(prefix) = store.parent() {
        dirs.push(prefix.join("bin"));
    }
    dirs
}

// Unix-only: its fixtures are symlinks and mode bits.
#[cfg(all(test, unix))]
#[path = "disk_idle_tests.rs"]
mod tests;

// Windows' installed form: atpkg's `.cmd` shims, laid by atpkg itself.
#[cfg(all(test, windows))]
#[path = "disk_idle_windows_tests.rs"]
mod windows_tests;
