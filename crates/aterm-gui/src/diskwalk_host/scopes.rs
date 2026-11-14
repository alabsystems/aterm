// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Disk scopes' pure parts (flow record §3, step F2a): where a pane is, when a folder joins
//! or leaves a pane, what a scope's bytes are from one board, the clamp to the Mac's used, the
//! glance walks' budget, aterm's own folders and what a drawn deck asks.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_diskwalk::{rel_within, within};

use super::{Claim, DiskScope, Pace, RootRecord, RootState};

/// Discoveries in a row that must name a folder before it joins a pane.
pub(crate) const PROMOTE_AFTER: u32 = 2;
/// Discoveries in a row that must miss a member before it leaves its pane.
pub(crate) const LEAVE_AFTER: u32 = 2;
/// A pane not discovered for this long starts again: its next discovery is its first.
pub(crate) const PANE_STALE_AFTER: Duration = Duration::from_secs(15);
/// The window over which glance walks are metered.
pub(crate) const GLANCE_WINDOW: Duration = Duration::from_secs(600);
/// What glance walks may take of [`GLANCE_WINDOW`] before another starts: 2 % of a core.
pub(crate) const GLANCE_BUDGET: Duration = Duration::from_secs(12);

/// Where a pane's live directory came from (`sys disk`'s `from=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveFrom {
    /// The kernel's cwd of the program in front.
    Fg,
    /// The kernel's cwd of the shell.
    Shell,
    /// The shell's OSC 7 report.
    Osc7,
}

impl LiveFrom {
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Fg => "fg",
            Self::Shell => "shell",
            Self::Osc7 => "osc7",
        }
    }
}

/// A shell's OSC 7 report, as the loop read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CwdReport {
    /// The reported path, native.
    pub(crate) path: PathBuf,
    /// This machine made it ([`crate::cwd_native::ReportedCwd::reported_cwd_here`]).
    pub(crate) here: bool,
    /// The host it named, when it named one that is not this machine.
    pub(crate) host: Option<String>,
}

/// One terminal pane as the loop last saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneFacts {
    /// Its session key.
    pub(crate) key: u64,
    /// Its session id: what a program started in it reads as `ATERM_PARENT_SESSION_ID`, and so
    /// the `session` of a build-start line ([`atpkg::build_start`]) a build in it left.
    pub(crate) session: String,
    /// Its shell, s.
    pub(crate) shell_pid: u32,
    /// The program in front, f: `tcgetpgrp(master)` where it is above 0, else s.
    pub(crate) front_pid: u32,
    /// The newest OSC 7 report.
    pub(crate) report: Option<CwdReport>,
}

/// Where a pane is, by the first rule of record §3 that applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Live {
    /// A report from another machine while a program other than the shell is in front (ssh,
    /// mosh, a container shell): nothing of the pane is on this Mac.
    Elsewhere { host: Option<String> },
    /// A directory on this machine, the pid whose cwd it is (the shell's for a report).
    At {
        path: PathBuf,
        from: LiveFrom,
        pid: u32,
    },
    /// Nothing to read.
    Unknown,
}

/// The pane's live directory: (1) a report another machine made while f ≠ s is elsewhere;
/// (2) the kernel's cwd of f; (3) the shell's when f ≠ s; (4) a report this machine made; (5)
/// none. The kernel comes first: OSC 7 carries the logical `$PWD`, a link the walker refuses,
/// in the case it was typed. A remote report is never a local path, and once the shell is back
/// in front it is dropped.
pub(crate) fn live_dir(p: &PaneFacts, cwd: &dyn Fn(u32) -> Option<PathBuf>) -> Live {
    let f = if p.front_pid > 0 {
        p.front_pid
    } else {
        p.shell_pid
    };
    let s = p.shell_pid;
    if let Some(r) = p.report.as_ref().filter(|r| !r.here)
        && f != s
    {
        return Live::Elsewhere {
            host: r.host.clone(),
        };
    }
    if f > 0
        && let Some(path) = cwd(f)
    {
        let from = if f == s {
            LiveFrom::Shell
        } else {
            LiveFrom::Fg
        };
        return Live::At { path, from, pid: f };
    }
    if f != s
        && s > 0
        && let Some(path) = cwd(s)
    {
        return Live::At {
            path,
            from: LiveFrom::Shell,
            pid: s,
        };
    }
    match p.report.as_ref().filter(|r| r.here) {
        Some(r) => Live::At {
            path: r.path.clone(),
            from: LiveFrom::Osc7,
            pid: s,
        },
        None => Live::Unknown,
    }
}

/// When folders join and leave one pane: a folder joins at the [`PROMOTE_AFTER`]th discovery in
/// a row that names it and leaves at the [`LEAVE_AFTER`]th in a row that misses it; a pane's
/// first discovery joins what it names at once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Membership {
    /// Members, each with the misses in a row it has had.
    members: BTreeMap<PathBuf, u32>,
    /// Named but not joined, each with the discoveries in a row that named it.
    pending: BTreeMap<PathBuf, u32>,
    /// The periodic discovery that last saw the pane, and when.
    seen: Option<(u64, Instant)>,
}

impl Membership {
    /// Whether the pane's next discovery continues this one: it was seen within
    /// [`PANE_STALE_AFTER`].
    pub(crate) fn fresh(&self, now: Instant) -> bool {
        self.seen
            .is_some_and(|(_, at)| now.saturating_duration_since(at) < PANE_STALE_AFTER)
    }

    /// Fold in discovery `round` at `now`, which named `named`: the first discovery (none
    /// before, one gone stale, or one that did not immediately precede it) joins what it
    /// names; a later one applies the hysteresis.
    pub(crate) fn observe(&mut self, named: &BTreeSet<PathBuf>, round: u64, now: Instant) {
        let continues =
            self.seen.is_some_and(|(r, _)| r.saturating_add(1) >= round) && self.fresh(now);
        if !continues {
            self.members = named.iter().map(|p| (p.clone(), 0)).collect();
            self.pending.clear();
            self.seen = Some((round, now));
            return;
        }
        if self.seen.is_some_and(|(r, _)| r == round) {
            self.seen = Some((round, now));
            return;
        }
        let mut pending = BTreeMap::new();
        for p in named {
            if let Some(miss) = self.members.get_mut(p) {
                *miss = 0;
                continue;
            }
            let n = self.pending.get(p).copied().unwrap_or(0) + 1;
            if n >= PROMOTE_AFTER {
                self.members.insert(p.clone(), 0);
            } else {
                pending.insert(p.clone(), n);
            }
        }
        self.pending = pending;
        self.members.retain(|p, miss| {
            if named.contains(p) {
                return true;
            }
            *miss += 1;
            *miss < LEAVE_AFTER
        });
        self.seen = Some((round, now));
    }

    /// The pane's members now.
    pub(crate) fn members(&self) -> impl Iterator<Item = &PathBuf> {
        self.members.keys()
    }

    /// Whether `path` is a member.
    pub(crate) fn holds(&self, path: &Path) -> bool {
        self.members.contains_key(path)
    }

    /// Whether `path` was named and waits to join.
    pub(crate) fn joining(&self, path: &Path) -> bool {
        self.pending.contains_key(path)
    }
}

/// Whether a root lies on the shown disk's pool (home's volume's [`VolumeKey`]).
///
/// [`VolumeKey`]: aterm_sysprobe::volumes::VolumeKey
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pool {
    /// Home's pool: another volume of the same APFS container counts, its blocks being in the
    /// container's used.
    Same,
    /// Another pool: named, and adds nothing.
    Other,
    /// Its pool, or home's, could not be read: it adds nothing.
    Unknown,
}

impl Pool {
    /// `pool` of a key and home's.
    pub(crate) fn of(
        key: Option<&aterm_sysprobe::volumes::VolumeKey>,
        home: Option<&aterm_sysprobe::volumes::VolumeKey>,
    ) -> Self {
        match (key, home) {
            (Some(k), Some(h)) if k == h => Self::Same,
            (Some(_), Some(_)) => Self::Other,
            _ => Self::Unknown,
        }
    }

    /// `sys disk`'s `pool=`.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }
}

/// The pool key of the volume `path` lies on: its mount's APFS container (or volume GUID), else
/// the file system id the volume's own read gives. Only a local volume the automounter did not
/// mount is read, and only its mount point.
pub(crate) fn pool_key(
    path: &Path,
    mounts: &aterm_sysprobe::volumes::Table,
) -> Option<aterm_sysprobe::volumes::VolumeKey> {
    let m = aterm_sysprobe::volumes::mount_of(path, mounts)?;
    m.key().or_else(|| {
        (m.local && !m.automounted && m.fstype != "autofs" && !m.removable)
            .then(|| aterm_sysprobe::volumes::read(m).ok().map(|r| r.key))
            .flatten()
    })
}

/// One root's standing on a board, as a scope's figure reads it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Standing<'a> {
    /// The root.
    pub(crate) path: &'a Path,
    /// Its pool.
    pub(crate) pool: Pool,
    /// Its record, where the board has one (none: claimed, not walked yet).
    pub(crate) record: Option<&'a RootRecord>,
}

/// A scope's bytes from one board (record §3, "Bytes, each folder once").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScopeFigure {
    /// Its roots (`roots=`).
    pub(crate) roots: usize,
    /// Those that count, C: on home's pool, with a complete figure (`counted=`).
    pub(crate) counted: usize,
    /// B: each counted folder once.
    pub(crate) bytes: u64,
    /// Every walkable root on home's pool counts.
    pub(crate) complete: bool,
    /// A walkable root on home's pool has no figure yet: it is queued or walking.
    pub(crate) owed: bool,
    /// When the oldest figure B sums was measured.
    pub(crate) oldest: Option<Instant>,
}

impl ScopeFigure {
    /// The scope draws: some root counts (O ≠ ∅).
    pub(crate) const fn drawn(&self) -> bool {
        self.counted > 0
    }

    /// Why it draws nothing (`why=`): a root is owed a walk, or none counts.
    pub(crate) const fn why_not_drawn(&self) -> Option<&'static str> {
        if self.drawn() {
            None
        } else if self.owed {
            Some("walking")
        } else {
            Some("none_counted")
        }
    }
}

struct Counted<'a> {
    path: &'a Path,
    alloc: u64,
    at: Instant,
    source: &'a Path,
}

/// The scope's figure from `roots`, every one of the scope's roots on one board, nesting folded
/// by `fold`. C is the roots on home's pool with a complete figure (a walk that reached its end,
/// the one a running rewalk keeps, or the root's subtree in an enclosing walk); a running walk's
/// partial sum never counts. After a `(dev, ino)` dedupe, O is C's members inside no other
/// member, and B = Σ over O of each one's figure plus, recursively, the figures of the
/// outermost members inside it measured by another walk than its own (a folder created after
/// the enclosing walk, walked alone): so a scope whose roots are a subset of another's never
/// has more bytes on one board.
pub(crate) fn scope_figure(roots: &[Standing<'_>], fold: bool) -> ScopeFigure {
    let walkable = |r: &Standing<'_>| {
        r.record.is_none_or(|rec| {
            matches!(
                rec.state,
                RootState::Queued | RootState::Walking | RootState::Complete
            )
        })
    };
    let mut ids: Vec<(u64, u64)> = Vec::new();
    let mut counted: Vec<Counted<'_>> = Vec::new();
    let mut owed = false;
    let mut complete = true;
    for r in roots {
        if r.pool != Pool::Same || !walkable(r) {
            continue;
        }
        let Some((rec, c)) = r
            .record
            .and_then(|rec| rec.complete.as_ref().map(|c| (rec, c)))
        else {
            owed = true;
            complete = false;
            continue;
        };
        if rec.read_from.is_none()
            && let Some(id) = rec.id
        {
            if ids.contains(&id) {
                continue;
            }
            ids.push(id);
        }
        if counted
            .iter()
            .any(|o| within(o.path, r.path, fold) && within(r.path, o.path, fold))
        {
            continue;
        }
        counted.push(Counted {
            path: r.path,
            alloc: c.sums.alloc_fair,
            at: c.at,
            source: rec.read_from.as_deref().unwrap_or(r.path),
        });
    }
    let inside = |a: &Counted<'_>, b: &Counted<'_>| {
        !std::ptr::eq(a, b) && within(a.path, b.path, fold) && !within(b.path, a.path, fold)
    };
    let outer: Vec<&Counted<'_>> = counted
        .iter()
        .filter(|c| !counted.iter().any(|o| inside(c, o)))
        .collect();
    let mut bytes = 0u64;
    let mut oldest: Option<Instant> = None;
    let mut stack: Vec<&Counted<'_>> = outer;
    while let Some(o) = stack.pop() {
        bytes = bytes.saturating_add(o.alloc);
        oldest = Some(oldest.map_or(o.at, |t| t.min(o.at)));
        let source = o.source;
        let other: Vec<&Counted<'_>> = counted
            .iter()
            .filter(|e| inside(e, o) && !same_root(e.source, source, fold))
            .collect();
        stack.extend(
            other
                .iter()
                .copied()
                .filter(|e| !other.iter().any(|d| inside(e, d))),
        );
    }
    ScopeFigure {
        roots: roots.len(),
        counted: counted.len(),
        bytes,
        complete,
        owed,
        oldest,
    }
}

fn same_root(a: &Path, b: &Path, fold: bool) -> bool {
    within(a, b, fold) && within(b, a, fold)
}

/// What the bar draws of aterm's and the tab's bytes against the Mac's used: A = min(B(aterm),
/// used) and T = min(B(tab), A), and whether each was cut (APFS clones count each copy's
/// blocks; a walk older than a large free counts what is gone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Clamped {
    /// What the aterm segment draws.
    pub(crate) aterm: u64,
    /// What the tab segment draws.
    pub(crate) tab: u64,
    /// aterm's bytes were above the Mac's used.
    pub(crate) aterm_clamped: bool,
    /// The tab's bytes were above what aterm's segment draws.
    pub(crate) tab_clamped: bool,
}

/// [`Clamped`] of `used` (home's volume's total less its free, `VolumeReading::used`) and the two
/// scopes' bytes.
pub(crate) fn clamp(used: Option<u64>, aterm: u64, tab: u64) -> Clamped {
    let a = used.map_or(aterm, |u| aterm.min(u));
    Clamped {
        aterm: a,
        tab: tab.min(a),
        aterm_clamped: a < aterm,
        tab_clamped: tab > a,
    }
}

/// What glance walks took of the last [`GLANCE_WINDOW`]: a glance walk starts only while they
/// took under [`GLANCE_BUDGET`] (a started walk may run past it).
#[derive(Clone, Debug, Default)]
pub(crate) struct GlanceBudget {
    spent: VecDeque<(Instant, Duration)>,
}

impl GlanceBudget {
    /// Charge `cost` at `at` (each slice's share of a glance walk, or a refusal's look), and
    /// forget the charges the window ending at `at` no longer holds.
    pub(crate) fn charge(&mut self, at: Instant, cost: Duration) {
        self.forget(at);
        if !cost.is_zero() {
            self.spent.push_back((at, cost));
        }
    }

    fn forget(&mut self, now: Instant) {
        while self
            .spent
            .front()
            .is_some_and(|&(t, _)| now.saturating_duration_since(t) >= GLANCE_WINDOW)
        {
            self.spent.pop_front();
        }
    }

    /// What glance walks took in the window ending at `now` (the measurement's reading).
    #[cfg(test)]
    pub(crate) fn taken(&mut self, now: Instant) -> Duration {
        self.forget(now);
        self.spent.iter().map(|&(_, c)| c).sum()
    }

    /// Whether a glance walk may start at `now`: the law [`Self::opens_at`] answers, read
    /// directly.
    #[cfg(test)]
    pub(crate) fn allows(&mut self, now: Instant) -> bool {
        self.taken(now) < GLANCE_BUDGET
    }

    /// The first instant from `now` at which a glance walk may start: `now` while the budget
    /// allows one, else when enough of the window's charges age out of it for the rest to take
    /// under [`GLANCE_BUDGET`].
    pub(crate) fn opens_at(&self, now: Instant) -> Instant {
        let mut live = self
            .spent
            .iter()
            .filter(|&&(t, _)| now.saturating_duration_since(t) < GLANCE_WINDOW);
        let mut left: Duration = live.clone().map(|&(_, c)| c).sum();
        let mut opens = now;
        while left >= GLANCE_BUDGET {
            let Some(&(t, c)) = live.next() else {
                break;
            };
            left = left.saturating_sub(c);
            opens = opens.max(t + GLANCE_WINDOW);
        }
        opens
    }
}

/// Where aterm keeps its own files on this platform: its state folder on every unix, the build
/// cache's folder (`~/Library/Caches/aterm`) on macOS, and atpkg's package prefix on every unix.
pub(crate) fn own_paths(home: Option<&Path>, state: Option<PathBuf>, macos: bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = state.into_iter().collect();
    if let Some(home) = home {
        if macos {
            out.push(home.join("Library/Caches/aterm"));
        }
        #[cfg(unix)]
        out.push(atpkg::platform::default_prefix(home));
    }
    out
}

/// aterm's own folders, each once: a folder inside another is that one's (atpkg's prefix lies
/// inside the state folder on macOS, outside it on Linux).
pub(crate) fn own_folders(folders: &[PathBuf], fold: bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in folders {
        if !out.iter().any(|o| within(p, o, fold)) {
            out.retain(|o| !within(o, p, fold));
            out.push(p.to_path_buf());
        }
    }
    out
}

/// One window as the deck's ask reads it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeckWindow {
    /// It draws the deck.
    pub(crate) drawn: bool,
    /// It is hidden, minimised or covered.
    pub(crate) occluded: bool,
    /// Its front tab.
    pub(crate) front: Option<crate::tab_model::TabId>,
    /// Its lens is the Disk lens.
    pub(crate) disk_lens: bool,
}

/// What a deck asks of the walker: each window that draws it in view keeps its front tab and
/// aterm demanded, at look pace while its Disk lens is open and at glance pace otherwise; a deck
/// that is off, hidden or occluded asks nothing.
pub(crate) fn deck_asks(bars_shown: bool, windows: &[DeckWindow]) -> Vec<(DiskScope, Pace)> {
    let mut out: BTreeMap<DiskScope, Pace> = BTreeMap::new();
    if !bars_shown {
        return Vec::new();
    }
    for w in windows.iter().filter(|w| w.drawn && !w.occluded) {
        let pace = if w.disk_lens {
            Pace::Look
        } else {
            Pace::Glance
        };
        for scope in w
            .front
            .map(DiskScope::Tab)
            .into_iter()
            .chain([DiskScope::Aterm])
        {
            let at = out.entry(scope).or_insert(pace);
            *at = (*at).max(pace);
        }
    }
    out.into_iter().collect()
}

/// `path` with its ancestor `root`'s spelling of the components they share, for a lookup in
/// `root`'s walk ([`rel_within`]).
pub(crate) fn rel_in(path: &Path, root: &Path, fold: bool) -> Option<PathBuf> {
    rel_within(path, root, fold)
}

/// The members a scope holds, each once, from the board's panes (and, for aterm, its own).
pub(crate) fn members_of<'a>(
    scope: DiskScope,
    tabs: &'a [super::TabFacts],
    panes: &'a BTreeMap<u64, super::PaneView>,
    own: &'a [Claim],
) -> Vec<&'a Claim> {
    let mut out: Vec<&Claim> = Vec::new();
    let mut push = |c: &'a Claim| {
        if !out.iter().any(|o| o.path == c.path) {
            out.push(c);
        }
    };
    match scope {
        DiskScope::Tab(id) => {
            for t in tabs.iter().filter(|t| t.id == id) {
                for k in &t.panes {
                    for c in panes.get(k).map(|p| p.members.as_slice()).unwrap_or(&[]) {
                        push(c);
                    }
                }
            }
        }
        DiskScope::Aterm => {
            for p in panes.values() {
                for c in &p.members {
                    push(c);
                }
            }
            for c in own {
                push(c);
            }
        }
    }
    out
}
