// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background disk walker host thread: the Disk scopes from the tabs' live directories (flow
//! record §3, step F2a), sized by walks, and the build folder hazards.

#![cfg_attr(
    not(all(test, unix)),
    expect(
        dead_code,
        reason = "`sys disk` and `sys tab` read `disk_reports` (`sys_host`) and a drawn deck \
                  demands its scopes; the bar that draws them is step F2b's, the parts only \
                  the other readers use (the System panel, the detail levels past sizes) land \
                  in their own streams of design P2d (and the tests, the other readers, are \
                  unix-only)"
    )
)]

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use aterm_diskwalk::{
    Deny, DirId, HUGE_NOTE, HUGE_WARN, HugeDir, LevelRow, LinkTotals, Marker, Policy, Refusal,
    RootSpec, Sliced, Step, Sums, Ticket, Tree, Walker, canonical, directory_identity, link_target,
    list_one_level, marker_at, refuse, within, worktree_above,
};
use aterm_sysprobe::volumes::{Table, VolumeKey};
use aterm_types::MutexExt as _;

use crate::tab_model::TabId;

mod scopes;

pub(crate) use scopes::{
    CwdReport, DeckWindow, GlanceBudget, Live, LiveFrom, Membership, PaneFacts, Pool, ScopeFigure,
    Standing, clamp, deck_asks, live_dir, own_folders, own_paths, pool_key, scope_figure,
};

/// How long one read keeps its scope demanded.
pub(crate) const DEMAND_TTL: Duration = Duration::from_secs(60);
/// How often a drawn deck asks again: half [`DEMAND_TTL`].
pub(crate) const DECK_ASK_EVERY: Duration = Duration::from_secs(30);
/// How long a root's record (its last walk) is kept after it leaves every demanded scope; it is
/// not walked in that time, and a tab back within it draws again with no walk.
pub(crate) const RETIRE_AFTER: Duration = Duration::from_secs(600);
/// The shortest age at which a complete walk is walked again at look pace.
pub(crate) const REWALK_AFTER: Duration = Duration::from_secs(30);
/// At look pace a walk is walked again no sooner than this many times its own duration.
pub(crate) const REWALK_COST_FACTOR: u32 = 20;
/// The shortest age at which a complete walk is walked again at glance pace alone.
pub(crate) const GLANCE_REWALK_AFTER: Duration = Duration::from_secs(600);
/// At glance pace a walk is walked again no sooner than this many times its own duration.
pub(crate) const GLANCE_COST_FACTOR: u32 = 50;
/// How often the thread derives each demanded pane's folders again.
pub(crate) const DISCOVER_EVERY: Duration = Duration::from_secs(5);
/// The most build-start lines one pane's session names roots from, the newest first: a tab
/// that built in more worktrees than this holds the newest.
const MAX_BUILT_PER_PANE: usize = 64;
/// How long a live directory's lift to its repository and build output is kept.
pub(crate) const LIFT_FOR: Duration = Duration::from_secs(60);
/// The growth window design T3 reads: a tab's roots grew ≥ 1 GiB in ten minutes.
pub(crate) const GROWTH_WINDOW: Duration = Duration::from_secs(600);
/// Subtree rows kept per root, per view ("biggest", "most entries").
pub(crate) const TOP_DIRS: usize = 8;
const NOT_MEASURED_ROWS: usize = 16;
const PUBLISH_EVERY: Duration = Duration::from_millis(500);
/// How long a walk in flight keeps no directory open between its steps on Windows, where an open
/// directory fails a rename of every folder above it: a rename in the walk's way that is tried
/// again at least this often gets through within one step ([`PUBLISH_EVERY`]) and this.
#[cfg(windows)]
const RELEASED_FOR: Duration = Duration::from_millis(100);
/// How often the slow tier counts the entries of the build profiles outside every root.
pub(crate) const PROFILES_EVERY: Duration = Duration::from_secs(600);
const MAX_CHECKOUTS: usize = 512;
const PROFILE_WALK_BUDGET: Duration = Duration::from_secs(5);
const EXEC_KNOWN_FOR: Duration = Duration::from_secs(3600);
/// How often the loop looks at the panes' facts while a scope is demanded…
pub(crate) const FACTS_EVERY: Duration = Duration::from_secs(1);
/// …and while none is (the slow tier needs the home and the deny set).
pub(crate) const FACTS_IDLE_EVERY: Duration = Duration::from_secs(10);

/// Which roots a reader asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DiskScope {
    /// One tab: the union of its panes' projects and their build output, and of the workspaces
    /// the builds started in its panes compiled and theirs, keyed by its `TabId` (never reused
    /// while aterm runs).
    Tab(TabId),
    /// Every open tab's folders and aterm's own.
    Aterm,
}

/// How soon a reader wants a scope's figures fresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Pace {
    /// A drawn deck: a complete root is walked again at its end + max(600 s, 50 × took), and a
    /// glance walk starts only inside [`GLANCE_BUDGET`].
    Glance,
    /// An open Disk lens, `sys disk`, `sys tab`: at its end + max(30 s, 20 × took).
    Look,
}

impl Pace {
    /// The shortest age, and the multiple of a walk's own duration, before a walk is owed again.
    const fn rewalk(self) -> (Duration, u32) {
        match self {
            Self::Glance => (GLANCE_REWALK_AFTER, GLANCE_COST_FACTOR),
            Self::Look => (REWALK_AFTER, REWALK_COST_FACTOR),
        }
    }
}

/// How much a reader wants measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Detail {
    /// Allocated and logical sizes, entry counts, the biggest subtrees.
    #[default]
    Sizes,
    /// Also each file's private (unshared-clone) size and the hard-link totals.
    Frees,
}

/// Why a folder is a root (record §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RootKind {
    /// A pane's repository: the nearest folder above its live directory, or above a workspace
    /// a build started in it compiled, holding `.git`.
    Project,
    /// A pane's live directory, or a workspace a build started in it compiled, with no
    /// repository above it.
    Folder,
    /// A project's build output: its `target` linked out of it, or its build mirror; and the
    /// build folder a build-start line names outside both.
    Build,
    /// aterm's own folders.
    Own,
    /// A `[sys] disk_roots` opt-in: walked, and in no scope.
    OptIn,
}

impl RootKind {
    /// The word `sys disk` prints (`kinds=project,build`).
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Folder => "folder",
            Self::Build => "build",
            Self::Own => "own",
            Self::OptIn => "opt-in",
        }
    }
}

/// One tab as the loop last saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabFacts {
    /// Its identity.
    pub(crate) id: TabId,
    /// `Tab 2`, or `Workers ▸ <label>` for a parked worker tab (`App::tab_name`).
    pub(crate) label: String,
    /// Its place on its window's strip, 1-based; `None` for a parked worker tab.
    pub(crate) number: Option<usize>,
    /// Its window's stable id (`sessions`' `window=`).
    pub(crate) window: u64,
    /// Its terminal panes' session keys, in layout order.
    pub(crate) panes: Vec<u64>,
}

/// What the loop publishes: the panes and tabs, and what the path test needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    /// Every terminal pane with a live shell, each once.
    pub(crate) panes: Vec<PaneFacts>,
    /// Every tab with a terminal pane.
    pub(crate) tabs: Vec<TabFacts>,
    /// `$HOME`.
    pub(crate) home: Option<PathBuf>,
    /// `[privacy] protected_roots`, as written.
    pub(crate) overrides: Vec<String>,
    /// `[sys] disk_roots`, absolute.
    pub(crate) opt_in: Vec<PathBuf>,
    /// aterm's own folders ([`own_folders`]).
    pub(crate) own: Vec<PathBuf>,
}

impl Facts {
    /// What a discovery at once follows: which panes there are and which tab holds each, in no
    /// order (a focus moved between splits, or a tab or pane moved, opens and closes nothing).
    fn shape(&self) -> (BTreeSet<u64>, BTreeMap<TabId, BTreeSet<u64>>) {
        let keys = self.panes.iter().map(|p| p.key).collect();
        let tabs = self
            .tabs
            .iter()
            .map(|t| (t.id, t.panes.iter().copied().collect()))
            .collect();
        (keys, tabs)
    }

    /// What the deny set is built from.
    fn policy_inputs(&self) -> (Option<&PathBuf>, &[String], &[PathBuf]) {
        (self.home.as_ref(), &self.overrides, &self.opt_in)
    }
}

/// One folder a scope claims, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claim {
    /// The canonical path.
    pub(crate) path: PathBuf,
    /// Why it is a root.
    pub(crate) kinds: BTreeSet<RootKind>,
    /// A linked git worktree (its `.git` is a file).
    pub(crate) linked_worktree: bool,
}

impl Claim {
    fn of(path: PathBuf, kind: RootKind, linked: bool) -> Self {
        Self {
            path,
            kinds: BTreeSet::from([kind]),
            linked_worktree: linked,
        }
    }
}

/// Why a pane's live directory adds nothing (`sys disk`'s pane `why=`). A pane with a why still
/// holds what the builds started in it compiled ([`Engine::built_by`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneWhy {
    /// Its live directory or project failed the path test.
    Refused(Refusal),
    /// Its live directory is on another pool than home's.
    OtherPool,
    /// It is on another machine (a report from another host while a program is in front).
    Elsewhere,
    /// Nothing says where it is.
    Unknown,
    /// Its project was named once and joins at its next discovery.
    Joining,
}

/// One pane as the last discovery that read it saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneView {
    /// Its session key.
    pub(crate) key: u64,
    /// Where its live directory came from.
    pub(crate) from: Option<LiveFrom>,
    /// The name of the program whose directory it is.
    pub(crate) exe: Option<String>,
    /// The host another machine's report named.
    pub(crate) host: Option<String>,
    /// Its live directory L.
    pub(crate) live: Option<PathBuf>,
    /// Its project P.
    pub(crate) project: Option<PathBuf>,
    /// Why its live directory adds nothing, where it adds nothing.
    pub(crate) why: Option<PaneWhy>,
    /// The folders it holds now, once joined: its project and the project's build output, and
    /// each workspace a build started in it compiled with that workspace's build output.
    pub(crate) members: Vec<Claim>,
}

impl PaneView {
    fn unseen(key: u64) -> Self {
        Self {
            key,
            from: None,
            exe: None,
            host: None,
            live: None,
            project: None,
            why: Some(PaneWhy::Unknown),
            members: Vec::new(),
        }
    }

    /// Its project is a member.
    pub(crate) fn member(&self) -> bool {
        self.project
            .as_ref()
            .is_some_and(|p| self.members.iter().any(|c| &c.path == p))
    }
}

/// Where a root's measurement stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootState {
    /// Claimed, not walked yet.
    Queued,
    /// A walk is running.
    Walking,
    /// The last walk reached its end.
    Complete,
    /// Not measured, and why (`protected`, `remote`, …).
    NotMeasured(Refusal),
    /// Too broad to walk (`/`, the home, a volume root): a one-level listing.
    OneLevel,
}

impl RootState {
    /// The word `sys disk` prints: a refusal's own word, so one fact has one spelling.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Walking => "walking",
            Self::Complete => "complete",
            Self::NotMeasured(why) => why.word(),
            Self::OneLevel => Refusal::TooBroad.word(),
        }
    }
}

/// One subtree inside a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirRow {
    /// Its path relative to the root.
    pub(crate) rel: PathBuf,
    /// Allocated bytes, hard links shared fairly.
    pub(crate) alloc: u64,
    /// Logical bytes.
    pub(crate) logical: u64,
    /// Entries in its subtree (files and folders).
    pub(crate) entries: u64,
    /// Entries directly in it.
    pub(crate) direct_entries: u32,
}

/// How much a root's allocation moved over a span (design T3: Δ allocated).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Growth {
    /// Bytes gained (negative: freed).
    pub(crate) bytes: i64,
    /// The span between the two walks compared.
    pub(crate) over: Duration,
}

/// A walk that reached its end.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Complete {
    /// The root's sums (`private` set by a [`Detail::Frees`] walk).
    pub(crate) sums: Sums,
    /// What the walk measured.
    pub(crate) detail: Detail,
    /// The hard-link totals of a [`Detail::Frees`] walk.
    pub(crate) links: Option<LinkTotals>,
    /// When it ended.
    pub(crate) at: Instant,
    /// How long it took.
    pub(crate) took: Duration,
    /// The largest direct entry count of any folder in it.
    pub(crate) direct_entries_max: u32,
    /// Where the space is ([`Tree::biggest_in`]).
    pub(crate) biggest: Vec<DirRow>,
    /// The folders with the most direct entries.
    pub(crate) most_entries: Vec<DirRow>,
    /// The §10.8 rows: folders at 75,000 direct entries or more.
    pub(crate) huge: Vec<HugeDir>,
    /// Folders inside that were not measured, and why (at most [`NOT_MEASURED_ROWS`]).
    pub(crate) not_measured: Vec<(PathBuf, Refusal)>,
    /// The allocation's move over [`GROWTH_WINDOW`] (or the span walked so far), from complete walks.
    pub(crate) growth: Option<Growth>,
}

impl Complete {
    /// When it is owed another walk at `pace`.
    pub(crate) fn rewalk_at(&self, pace: Pace) -> Instant {
        let (floor, factor) = pace.rewalk();
        self.at + floor.max(self.took.saturating_mul(factor))
    }
}

/// One root as the thread last published it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RootRecord {
    /// The canonical path.
    pub(crate) path: PathBuf,
    /// Where it stands.
    pub(crate) state: RootState,
    /// Its directory's `(dev, ino)` once opened: two spellings of one folder are one root.
    pub(crate) id: Option<(u64, u64)>,
    /// What the running walk has summed so far (a lower bound), while [`RootState::Walking`].
    pub(crate) partial: Option<Sums>,
    /// Entries the running walk has listed.
    pub(crate) walked_entries: u64,
    /// The last complete walk (behind a running walk, the one it keeps until it ends).
    pub(crate) complete: Option<Complete>,
    /// A too-broad root's one-level listing.
    pub(crate) level: Vec<LevelRow>,
    /// The root whose walk these figures were read from, when it lies inside another root.
    pub(crate) read_from: Option<PathBuf>,
    /// When the path was last asked: a walk began, or a refusal or a one-level listing came back.
    pub(crate) asked: Option<Instant>,
}

impl RootRecord {
    const fn queued(path: PathBuf) -> Self {
        Self {
            path,
            state: RootState::Queued,
            id: None,
            partial: None,
            walked_entries: 0,
            complete: None,
            level: Vec::new(),
            read_from: None,
            asked: None,
        }
    }

    /// The bytes it stands for: a complete figure's (a walk that reached its end, kept behind a
    /// running rewalk), never a running walk's partial sum, else 0.
    pub(crate) fn alloc(&self) -> u64 {
        self.complete.as_ref().map_or(0, |c| c.sums.alloc_fair)
    }

    /// Whether a walk may measure it: not refused.
    const fn walkable(&self) -> bool {
        matches!(
            self.state,
            RootState::Queued | RootState::Walking | RootState::Complete
        )
    }
}

/// One root row of a report.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RootRow {
    /// Its number in the report (1-based).
    pub(crate) n: usize,
    /// The claim (path, kinds).
    pub(crate) claim: Claim,
    /// The row it is a pinned child of: counted there, not again.
    pub(crate) inside: Option<usize>,
    /// The other tabs that claim it, a folder above it or a folder in it, each by its least pane
    /// key.
    pub(crate) shared_with: Vec<u64>,
    /// The tab whose total counts it whole, by its least pane key: the lowest claiming it or a
    /// folder above it.
    pub(crate) counted_in: Option<u64>,
    /// The bytes of it that two or more tabs claim (an outermost row's; 0 on a pinned child).
    pub(crate) shared_alloc: u64,
    /// Whether it lies on home's pool.
    pub(crate) pool: Pool,
    /// The scopes that hold it (none for an opt-in).
    pub(crate) scopes: Vec<DiskScope>,
    /// What the board holds for it.
    pub(crate) record: RootRecord,
}

impl RootRow {
    /// The bytes the row stands for: its complete figure's, never a running walk's partial sum.
    pub(crate) fn alloc(&self) -> u64 {
        self.record.alloc()
    }
}

/// Whether the walker thread runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WalkerState {
    /// Not started: no event loop yet (a test's `App`, a CLI).
    NotStarted,
    /// Running.
    Running,
    /// The thread did not start (logged): nothing will be measured.
    Failed,
}

impl WalkerState {
    const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Running,
            2 => Self::Failed,
            _ => Self::NotStarted,
        }
    }

    const fn to_u8(self) -> u8 {
        match self {
            Self::NotStarted => 0,
            Self::Running => 1,
            Self::Failed => 2,
        }
    }
}

/// What a reader gets: the scope's roots and their sums.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DiskReport {
    /// The scope asked about.
    pub(crate) scope: DiskScope,
    /// The tab's label (tab scope).
    pub(crate) label: Option<String>,
    /// Its roots, outermost first by size, each pinned child after its root.
    pub(crate) roots: Vec<RootRow>,
    /// The scope's bytes: [`ScopeFigure::bytes`].
    pub(crate) total_bytes: u64,
    /// Bytes of them no other tab claims.
    pub(crate) exclusive_bytes: u64,
    /// Bytes of them two or more tabs claim.
    pub(crate) shared_bytes: u64,
    /// The scope's figure from this board (what the bar draws, before the clamp).
    pub(crate) figure: ScopeFigure,
    /// A walk is queued or running for one of its roots, or its roots are [`Self::undiscovered`].
    pub(crate) walking: bool,
    /// No discovery has derived the scope's roots from the panes' current facts yet (a scope
    /// asked for the first time, before the walker heard the ask): its roots are owed, not absent,
    /// so `roots` and the byte totals are lower bounds.
    pub(crate) undiscovered: bool,
    /// The §10.8 rows across its roots (and, for the aterm scope, the build profiles outside every root), most entries first.
    pub(crate) warnings: Vec<HugeDir>,
    /// Σ growth of the outermost complete roots.
    pub(crate) growth: Option<Growth>,
    /// The tab is unknown (closed, or the loop has not published it yet).
    pub(crate) unknown_tab: bool,
    /// Whether anything is measuring.
    pub(crate) walker: WalkerState,
}

/// Down the walker's inbox.
#[derive(Debug)]
pub(crate) enum ToWalker {
    /// The loop's facts (sent when they change).
    Facts(Facts),
    /// A reader asked about `scope` at `at`.
    Demand {
        scope: DiskScope,
        detail: Detail,
        pace: Pace,
        at: Instant,
    },
    /// End the thread (tests: the process's walker lives as long as it).
    #[cfg(test)]
    Stop,
    /// Answer with the walker thread's own disk policy, read on that thread (a test's read of what [`walker_main`] set).
    #[cfg(test)]
    ReadDiskPolicy(Sender<Option<bool>>),
    /// Answer with what the glance measurement reads on the walker thread ([`Probe`]).
    #[cfg(test)]
    Probe(Sender<Probe>),
}

/// What the glance measurement reads on the walker thread.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Probe {
    /// The thread's CPU time ([`thread_cpu`]).
    pub(crate) cpu: Option<Duration>,
    /// What glance walks took of the last [`scopes::GLANCE_WINDOW`].
    pub(crate) glance_taken: Duration,
    /// A glance walk is owed now and the budget holds it back.
    pub(crate) held_back: bool,
    /// Times the thread went to sleep.
    pub(crate) sleeps: u64,
    /// Walks it started.
    pub(crate) starts: u64,
}

/// The walker's whole answer, stored after each change: one board every scope is read from.
/// Two boards that compare equal tell every reader the same thing, so the walker stores (and
/// wakes the loop for) only one that differs from the last it stored ([`Engine::publish_if_dirty`]).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Published {
    /// Every tab the facts named.
    pub(crate) tabs: Vec<TabFacts>,
    /// Every pane a discovery read, by session key.
    pub(crate) panes: BTreeMap<u64, PaneView>,
    /// aterm's own folders.
    pub(crate) own_claims: Vec<Claim>,
    /// The `[sys] disk_roots` opt-ins.
    pub(crate) opt_in: Vec<Claim>,
    /// Each root's pool.
    pub(crate) pools: BTreeMap<PathBuf, Pool>,
    /// Home's pool.
    pub(crate) home_pool: Option<VolumeKey>,
    /// Every root's record.
    pub(crate) records: BTreeMap<PathBuf, Arc<RootRecord>>,
    /// The raised §10.8 entries, with their hysteresis.
    pub(crate) huge: BTreeMap<PathBuf, HugeDir>,
    /// Which of them the slow tier raised: build profiles outside every root.
    pub(crate) outside: BTreeSet<PathBuf>,
    /// Moves whenever `huge` does.
    pub(crate) huge_seq: u64,
    /// The scopes the last discovery derived on the facts `tabs` and `panes` were published
    /// from (else none): a scope outside it has no roots *yet*.
    pub(crate) discovered: BTreeSet<DiskScope>,
    /// Whether paths nest ignoring ASCII case (APFS).
    pub(crate) fold_case: bool,
}

impl Published {
    /// The tab that holds pane `key`, the first that lists it.
    pub(crate) fn tab_of(&self, key: u64) -> Option<&TabFacts> {
        self.tabs.iter().find(|t| t.panes.contains(&key))
    }

    /// The roots `scope` holds, each once.
    pub(crate) fn members(&self, scope: DiskScope) -> Vec<&Claim> {
        scopes::members_of(scope, &self.tabs, &self.panes, &self.own_claims)
    }

    /// A root's pool.
    pub(crate) fn pool(&self, path: &Path) -> Pool {
        self.pools.get(path).copied().unwrap_or(Pool::Unknown)
    }

    /// `scope`'s figure from this board.
    pub(crate) fn figure(&self, scope: DiskScope) -> ScopeFigure {
        let members = self.members(scope);
        let standing: Vec<Standing<'_>> = members
            .iter()
            .map(|c| Standing {
                path: &c.path,
                pool: self.pool(&c.path),
                record: self.records.get(&c.path).map(|r| &**r),
            })
            .collect();
        scope_figure(&standing, self.fold_case)
    }

    /// The scopes that hold `path`: each tab whose panes hold it, and aterm when anything does
    /// but the opt-ins.
    pub(crate) fn scopes_of(&self, path: &Path) -> Vec<DiskScope> {
        let mut out: Vec<DiskScope> = self
            .tabs
            .iter()
            .filter(|t| {
                t.panes.iter().any(|k| {
                    self.panes
                        .get(k)
                        .is_some_and(|p| p.members.iter().any(|c| c.path == path))
                })
            })
            .map(|t| DiskScope::Tab(t.id))
            .collect();
        out.dedup();
        if !out.is_empty() || self.own_claims.iter().any(|c| c.path == path) {
            out.push(DiskScope::Aterm);
        }
        out
    }
}

/// The walker's newest publication until the loop takes it (`diskwalk-mailbox`): the loop only
/// ever wants the newest, and a receive from std's channel waits on a send part-way through its
/// slot.
#[derive(Default)]
pub(crate) struct Mailbox(Mutex<Option<Arc<Published>>>);

impl Mailbox {
    /// The walker's store, which replaces a publication the loop has not taken, and then its
    /// `wake` of the loop. It holds the lock for that one store and lets it go before it drops
    /// the older publication and before `wake`, so the absorb a wake brings takes the store,
    /// and a loop that found the mailbox held comes round again for it.
    fn store(&self, published: Arc<Published>, wake: impl FnOnce()) {
        let older = self.0.lock_or_recover().replace(published);
        drop(older);
        wake();
    }

    /// The loop's take of the newest publication stored since its last take. It never waits
    /// on the walker, which runs at the lowest class (clippy.toml: a wait that wakes): while the
    /// walker holds the lock it takes nothing, and the wake the walker sends after that store
    /// brings the loop round again.
    fn take(&self) -> Option<Arc<Published>> {
        match self.0.try_lock() {
            Ok(mut newest) => newest.take(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().take(),
            Err(TryLockError::WouldBlock) => None,
        }
    }
}

type Proxy = aterm_window::event_loop::EventLoopProxy<crate::Wake>;

static ANCHOR: LazyLock<Instant> = LazyLock::new(Instant::now);

fn anchor_ms(at: Instant) -> u64 {
    u64::try_from(at.saturating_duration_since(*ANCHOR).as_millis()).unwrap_or(u64::MAX)
}

/// What the walker's readers, the loop and the walker's inbox share.
pub(crate) struct Shared {
    /// The walker's inbox, set when its thread starts.
    inbox: OnceLock<Sender<ToWalker>>,
    /// The newest publication: the loop stores it, readers clone it. A leaf.
    board: Mutex<Option<Arc<Published>>>,
    /// When each scope was last asked, at what pace and detail (readers only): a scope asked
    /// anew makes the loop send fresh facts, and a walker that starts hears what was asked
    /// before it ran.
    asked: Mutex<BTreeMap<DiskScope, (Instant, Pace, Detail)>>,
    /// [`ANCHOR`]-relative ms until which some scope is demanded.
    demanded_until_ms: AtomicU64,
    /// A reader asked a scope anew: the loop sends facts at its next wake.
    facts_wanted: AtomicBool,
    /// The loop's proxy, for a reader's poke (readers and the loop only).
    proxy: Mutex<Option<Proxy>>,
    /// [`WalkerState`].
    state: AtomicU8,
}

/// The process's.
pub(crate) static DISKWALK: Shared = Shared::new();

impl Shared {
    pub(crate) const fn new() -> Self {
        Self {
            inbox: OnceLock::new(),
            board: Mutex::new(None),
            asked: Mutex::new(BTreeMap::new()),
            demanded_until_ms: AtomicU64::new(0),
            facts_wanted: AtomicBool::new(false),
            proxy: Mutex::new(None),
            state: AtomicU8::new(0),
        }
    }

    /// Whether any scope is demanded at `now` (lock-free: the loop asks on every wake).
    pub(crate) fn demanded(&self, now: Instant) -> bool {
        anchor_ms(now) < self.demanded_until_ms.load(Ordering::Relaxed)
    }

    /// Whether anything is measuring.
    pub(crate) fn walker_state(&self) -> WalkerState {
        WalkerState::from_u8(self.state.load(Ordering::Relaxed))
    }

    fn set_walker_state(&self, s: WalkerState) {
        self.state.store(s.to_u8(), Ordering::Relaxed);
    }

    fn send(&self, msg: ToWalker) {
        if let Some(tx) = self.inbox.get() {
            let _ = tx.send(msg);
        }
    }

    /// Demand `scope` now at `pace`: the walker hears it, and a scope asked anew asks the loop
    /// for the panes' current facts.
    pub(crate) fn demand(&self, scope: DiskScope, detail: Detail, pace: Pace, now: Instant) {
        let fresh = {
            let mut asked = self.asked.lock_or_recover();
            let fresh = asked
                .get(&scope)
                .is_none_or(|(at, _, _)| now.saturating_duration_since(*at) >= DEMAND_TTL);
            asked.insert(scope, (now, pace, detail));
            fresh
        };
        self.demanded_until_ms
            .fetch_max(anchor_ms(now + DEMAND_TTL), Ordering::Relaxed);
        self.send(ToWalker::Demand {
            scope,
            detail,
            pace,
            at: now,
        });
        if fresh {
            self.facts_wanted.store(true, Ordering::Relaxed);
            let proxy = self.proxy.lock_or_recover().clone();
            if let Some(proxy) = proxy {
                let _ = proxy.send_event(crate::Wake::DiskWalk);
            }
        }
    }

    /// The newest publication: one board, so every scope read from it nests.
    pub(crate) fn board(&self) -> Arc<Published> {
        self.board.lock_or_recover().clone().unwrap_or_default()
    }

    /// The report for `scope` from the newest publication.
    pub(crate) fn report(&self, scope: DiskScope) -> DiskReport {
        build_report(scope, &self.board(), self.walker_state())
    }

    /// The loop's (or a test's) take of what the walker stored. It never waits on the walker
    /// (clippy.toml: a wait that wakes; [`Mailbox::take`]).
    pub(crate) fn absorb(&self, from: &Mailbox) -> Option<Arc<Published>> {
        let newest = from.take()?;
        *self.board.lock_or_recover() = Some(Arc::clone(&newest));
        Some(newest)
    }
}

/// Demand each of `scopes` at `pace` and read them all from the newest board, with the board.
pub(crate) fn disk_reports(
    scopes: &[DiskScope],
    detail: Detail,
    pace: Pace,
    now: Instant,
) -> (Arc<Published>, Vec<DiskReport>) {
    for &scope in scopes {
        DISKWALK.demand(scope, detail, pace, now);
    }
    let board = DISKWALK.board();
    let walker = DISKWALK.walker_state();
    let reports = scopes
        .iter()
        .map(|&s| build_report(s, &board, walker))
        .collect();
    (board, reports)
}

/// How the walker thread is scheduled: its CPU class and its disk reads.
#[derive(Clone, Copy, Debug)]
struct WalkerClass {
    role: crate::qos::Role,
    disk: aterm_diskwalk::DiskIo,
}

const WALKER_CLASS: WalkerClass = WalkerClass {
    role: crate::qos::Role::Housekeeping,
    disk: aterm_diskwalk::DiskIo::Throttled,
};

fn spawn_walker(
    shared: &'static Shared,
    proxy: Option<Proxy>,
    class: WalkerClass,
) -> std::io::Result<Arc<Mailbox>> {
    let (inbox_tx, inbox) = std::sync::mpsc::channel();
    let out = Arc::new(Mailbox::default());
    let from_walker = Arc::clone(&out);
    if shared.inbox.set(inbox_tx).is_err() {
        return Err(std::io::Error::other("the disk walker already runs"));
    }
    // A reader that asked before the thread ran (a headless instance starts it on the first
    // ask) is heard, not dropped.
    let asked: Vec<(DiskScope, (Instant, Pace, Detail))> = shared
        .asked
        .lock_or_recover()
        .iter()
        .map(|(s, a)| (*s, *a))
        .collect();
    for (scope, (at, pace, detail)) in asked {
        shared.send(ToWalker::Demand {
            scope,
            detail,
            pace,
            at,
        });
    }
    match crate::qos::spawn_lane_thread("aterm-diskwalk", class.role, move || {
        walker_main(inbox, out, proxy, class.disk)
    }) {
        Ok(_) => {
            shared.set_walker_state(WalkerState::Running);
            Ok(from_walker)
        }
        Err(error) => {
            shared.set_walker_state(WalkerState::Failed);
            Err(error)
        }
    }
}

fn shared_in(
    path: &Path,
    above: &BTreeSet<u64>,
    claimers: &BTreeMap<PathBuf, BTreeSet<u64>>,
    alloc_of: &dyn Fn(&Path) -> u64,
    fold: bool,
) -> u64 {
    if above.len() >= 2 {
        return alloc_of(path);
    }
    let strictly = |a: &Path, b: &Path| within(a, b, fold) && !within(b, a, fold);
    let inner: Vec<&PathBuf> = claimers.keys().filter(|k| strictly(k, path)).collect();
    inner
        .iter()
        .filter(|k| !inner.iter().any(|q| strictly(k, q)))
        .map(|k| {
            let mut with = above.clone();
            with.extend(claimers.get(*k).into_iter().flatten().copied());
            shared_in(k, &with, claimers, alloc_of, fold)
        })
        .sum()
}

pub(crate) fn build_report(scope: DiskScope, p: &Published, walker: WalkerState) -> DiskReport {
    let fold = p.fold_case;
    let label = match scope {
        DiskScope::Tab(id) => p.tabs.iter().find(|t| t.id == id).map(|t| t.label.clone()),
        DiskScope::Aterm => None,
    };
    let unknown_tab = matches!(scope, DiskScope::Tab(_)) && label.is_none();
    let undiscovered = !(p.discovered.contains(&scope) || p.discovered.contains(&DiskScope::Aterm));
    let mut scope_claims: Vec<&Claim> = p.members(scope);
    if scope == DiskScope::Aterm {
        scope_claims.extend(p.opt_in.iter());
    }
    let mut by_id: BTreeMap<(u64, u64), PathBuf> = BTreeMap::new();
    let mut key_of: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let mut all_paths: Vec<&PathBuf> = p
        .panes
        .values()
        .flat_map(|v| v.members.iter())
        .chain(&p.own_claims)
        .chain(&p.opt_in)
        .map(|c| &c.path)
        .collect();
    all_paths.sort();
    all_paths.dedup();
    for path in all_paths {
        let key = match p.records.get(path).and_then(|r| r.id) {
            Some(id) => by_id.entry(id).or_insert_with(|| path.clone()).clone(),
            None => path.clone(),
        };
        key_of.insert(path.clone(), key);
    }
    let key = |path: &Path| {
        key_of
            .get(path)
            .cloned()
            .unwrap_or_else(|| path.to_path_buf())
    };
    // Who claims a folder is counted by tab: each tab stands as its least pane key, so two panes
    // of one tab at one folder share nothing.
    let stand_in = |pane: u64| {
        p.tab_of(pane)
            .and_then(|t| t.panes.iter().min().copied())
            .unwrap_or(pane)
    };
    let mut claimers: BTreeMap<PathBuf, BTreeSet<u64>> = BTreeMap::new();
    for (pane, view) in &p.panes {
        for c in &view.members {
            claimers
                .entry(key(&c.path))
                .or_default()
                .insert(stand_in(*pane));
        }
    }
    let alloc_of = |k: &Path| p.records.get(k).map_or(0, |r| r.alloc());
    scope_claims.sort_by(|a, b| a.path.cmp(&b.path));
    let mut rows: Vec<(PathBuf, RootRow)> = Vec::new();
    for c in scope_claims {
        let k = key(&c.path);
        if let Some((_, row)) = rows.iter_mut().find(|(rk, _)| *rk == k) {
            row.claim.kinds.extend(c.kinds.iter().copied());
            row.claim.linked_worktree |= c.linked_worktree;
            continue;
        }
        let record = p
            .records
            .get(&k)
            .or_else(|| p.records.get(&c.path))
            .map_or_else(|| RootRecord::queued(c.path.clone()), |r| (**r).clone());
        rows.push((
            k,
            RootRow {
                n: 0,
                claim: c.clone(),
                inside: None,
                shared_with: Vec::new(),
                counted_in: None,
                shared_alloc: 0,
                pool: p.pool(&c.path),
                scopes: p.scopes_of(&c.path),
                record,
            },
        ));
    }
    let me: Option<Vec<u64>> = match scope {
        DiskScope::Tab(id) => Some(
            p.tabs
                .iter()
                .filter(|t| t.id == id)
                .flat_map(|t| t.panes.iter().copied())
                .collect(),
        ),
        DiskScope::Aterm => None,
    };
    for (k, row) in &mut rows {
        let whole: BTreeSet<u64> = claimers
            .iter()
            .filter(|(q, _)| within(k, q, fold))
            .flat_map(|(_, panes)| panes.iter().copied())
            .collect();
        let overlap: BTreeSet<u64> = claimers
            .iter()
            .filter(|(q, _)| within(k, q, fold) || within(q, k, fold))
            .flat_map(|(_, panes)| panes.iter().copied())
            .collect();
        row.counted_in = whole.iter().copied().min();
        row.shared_with = match &me {
            Some(mine) => overlap
                .iter()
                .copied()
                .filter(|o| !mine.contains(o))
                .collect(),
            None if overlap.len() >= 2 => overlap.into_iter().collect(),
            None => Vec::new(),
        };
        row.shared_alloc = shared_in(k, &whole, &claimers, &alloc_of, fold);
    }
    // A row parents only rows inside it when a walk may measure it: a refused or too-broad
    // root holds no figure, so a root inside it still counts on its own.
    let paths: Vec<(PathBuf, bool)> = rows
        .iter()
        .map(|(k, r)| (k.clone(), r.record.walkable()))
        .collect();
    let parent_of: Vec<Option<usize>> = paths
        .iter()
        .map(|(p, _)| {
            paths
                .iter()
                .enumerate()
                .filter(|(_, (q, walkable))| {
                    *walkable && q != p && within(p, q, fold) && !within(q, p, fold)
                })
                .max_by_key(|(_, (q, _))| q.components().count())
                .map(|(i, _)| i)
        })
        .collect();
    let rows: Vec<RootRow> = rows.into_iter().map(|(_, r)| r).collect();
    let mut order: Vec<usize> = (0..rows.len())
        .filter(|&i| parent_of[i].is_none())
        .collect();
    order.sort_by(|&a, &b| {
        rows[b]
            .alloc()
            .cmp(&rows[a].alloc())
            .then(rows[a].claim.path.cmp(&rows[b].claim.path))
    });
    fn push_with_children(
        i: usize,
        parent_of: &[Option<usize>],
        rows: &[RootRow],
        out: &mut Vec<usize>,
    ) {
        out.push(i);
        let mut kids: Vec<usize> = (0..rows.len())
            .filter(|&k| parent_of[k] == Some(i))
            .collect();
        kids.sort_by(|&a, &b| rows[b].alloc().cmp(&rows[a].alloc()));
        for k in kids {
            push_with_children(k, parent_of, rows, out);
        }
    }
    let mut ordered: Vec<usize> = Vec::with_capacity(rows.len());
    for i in order {
        push_with_children(i, &parent_of, &rows, &mut ordered);
    }
    let number: BTreeMap<usize, usize> = ordered
        .iter()
        .enumerate()
        .map(|(at, &i)| (i, at + 1))
        .collect();
    let mut out: Vec<RootRow> = Vec::with_capacity(rows.len());
    for &i in &ordered {
        let mut row = rows[i].clone();
        row.n = number[&i];
        row.inside = parent_of[i].map(|p| number[&p]);
        if row.inside.is_some() {
            row.shared_alloc = 0;
        }
        out.push(row);
    }
    let figure = p.figure(scope);
    let mut shared_bytes = 0u64;
    let mut walking = undiscovered;
    let mut warnings: Vec<HugeDir> = Vec::new();
    let mut growth: Option<Growth> = None;
    for row in &out {
        let walked = row.pool != Pool::Other || row.claim.kinds.contains(&RootKind::OptIn);
        walking |= walked && matches!(row.record.state, RootState::Queued | RootState::Walking);
        if let Some(c) = &row.record.complete {
            for h in &c.huge {
                if !warnings.iter().any(|w| w.path == h.path) {
                    warnings.push(h.clone());
                }
            }
        }
        if row.inside.is_some() || row.pool != Pool::Same {
            continue;
        }
        shared_bytes += row.shared_alloc.min(row.alloc());
        if let Some(g) = row.record.complete.as_ref().and_then(|c| c.growth) {
            let sum = growth.get_or_insert(Growth {
                bytes: 0,
                over: g.over,
            });
            sum.bytes += g.bytes;
            sum.over = sum.over.min(g.over);
        }
    }
    if scope == DiskScope::Aterm {
        for path in &p.outside {
            if let Some(h) = p.huge.get(path)
                && !warnings.iter().any(|w| w.path == h.path)
            {
                warnings.push(h.clone());
            }
        }
    }
    warnings.sort_by(|a, b| b.entries.cmp(&a.entries).then(a.path.cmp(&b.path)));
    let total_bytes = figure.bytes;
    let shared_bytes = shared_bytes.min(total_bytes);
    DiskReport {
        scope,
        label,
        roots: out,
        total_bytes,
        exclusive_bytes: total_bytes - shared_bytes,
        shared_bytes,
        figure,
        walking,
        undiscovered,
        warnings,
        growth,
        unknown_tab,
        walker,
    }
}

/// `path` read the way the kernel names a firmlinked folder (`/System/Volumes/Data/Users//a` is
/// `/Users//a`).
fn firmlinked(path: &Path) -> PathBuf {
    const FIRMLINKED: [&str; 8] = [
        "Users",
        "private",
        "Applications",
        "Library",
        "Volumes",
        "opt",
        "cores",
        "usr/local",
    ];
    match path.strip_prefix("/System/Volumes/Data") {
        Ok(rest) if FIRMLINKED.iter().any(|f| rest.starts_with(f)) => Path::new("/").join(rest),
        _ => path.to_path_buf(),
    }
}

/// A project's build mirror: the string join of the build cache's root and the project
/// (`build_dir` without its `.cargo-build` leaf), holding the build directory of every cargo
/// workspace at or under it.
pub(crate) fn build_mirror(cache_root: &Path, project: &Path) -> PathBuf {
    let dir = atpkg::build_cache::build_dir(cache_root, project);
    dir.parent().map_or(dir.clone(), Path::to_path_buf)
}

/// A live directory's lift: its project and the project's build output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lift {
    /// P: the repository `git rev-parse --show-toplevel` names, bounded by home and mounts, else
    /// the live directory.
    pub(crate) project: PathBuf,
    /// [`RootKind::Project`] for a repository, [`RootKind::Folder`] for none.
    pub(crate) kind: RootKind,
    /// A linked git worktree.
    pub(crate) linked: bool,
    /// B(P): `P/target`'s target where it is a link out of P, and the build mirror where it is
    /// a directory. A `target` inside P is a pinned child of P, counted there.
    pub(crate) builds: Vec<PathBuf>,
}

/// `live`'s [`Lift`] under `policy` (`live` passed the path test already).
pub(crate) fn lift(live: &Path, home: Option<&Path>, policy: &Policy) -> Lift {
    let fold = policy.deny.fold_case();
    let (project, kind, linked) = match worktree_above(live, policy) {
        Some(w) => (w.root, RootKind::Project, w.linked),
        None => (live.to_path_buf(), RootKind::Folder, false),
    };
    let mut builds = Vec::new();
    if refuse(None, &project, &policy.mounts, &policy.deny).is_none() {
        if let Ok(Some(Marker::Link)) = marker_at(&project, "target", policy)
            && let Ok(Some(to)) = link_target(&project, "target", policy)
            && let Some(to) = canonical(&to)
            && !within(&to, &project, fold)
        {
            builds.push(to);
        }
        if let Some(home) = home {
            let mirror = build_mirror(&atpkg::build_cache::cache_root(home), &project);
            if let (Some(parent), Some(leaf)) =
                (mirror.parent(), mirror.file_name().and_then(|n| n.to_str()))
                && matches!(marker_at(parent, leaf, policy), Ok(Some(Marker::Dir)))
            {
                builds.push(mirror);
            }
        }
    }
    Lift {
        project,
        kind,
        linked,
        builds,
    }
}

/// What discovery reads of a process, injected so a test names its own: two `proc_pidinfo`
/// reads at most for a pane's directory, and its program's name. No process table, no
/// environment and no open files. And what it reads of the builds started in the tabs: one
/// bounded read of the build-start record for each discovery that reads a pane, however many.
pub(crate) struct PaneReads {
    /// A process's working directory.
    pub(crate) cwd: Box<dyn Fn(u32) -> Option<PathBuf>>,
    /// A process's program name.
    pub(crate) exe: Box<dyn Fn(u32) -> Option<String>>,
    /// A path's pool key in a mount table.
    pub(crate) pool: Box<PoolOf>,
    /// The build-start record's lines ([`atpkg::build_start::read`]): none where there is no
    /// record.
    pub(crate) starts: Box<dyn Fn() -> Vec<atpkg::build_start::Start>>,
}

/// A path's pool key read from a mount table.
pub(crate) type PoolOf = dyn Fn(&Path, &Table) -> Option<VolumeKey>;

impl PaneReads {
    fn live() -> Self {
        Self {
            cwd: Box::new(aterm_sysprobe::process_cwd),
            exe: Box::new(|pid| {
                aterm_sysprobe::process_path(pid)
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            }),
            pool: Box::new(pool_key),
            starts: Box::new(|| {
                atpkg::build_start::record()
                    .map(|record| atpkg::build_start::read(&record))
                    .unwrap_or_default()
            }),
        }
    }
}

/// What a discovery saw of one pane: the view without its members, and what it named.
struct Seen {
    view: PaneView,
    named: Vec<Claim>,
}

/// One pane's membership and its last view.
#[derive(Default)]
struct PaneLife {
    membership: Membership,
    /// The newest claim of each folder it named (kinds, linked).
    claims: BTreeMap<PathBuf, Claim>,
    view: Option<PaneView>,
}

impl PaneLife {
    fn view(&self, key: u64) -> PaneView {
        let mut v = self.view.clone().unwrap_or_else(|| PaneView::unseen(key));
        v.members = self
            .membership
            .members()
            .filter_map(|p| self.claims.get(p).cloned())
            .collect();
        if v.why.is_none()
            && let Some(p) = v.project.as_ref()
            && !self.membership.holds(p)
        {
            v.why = Some(PaneWhy::Joining);
        }
        v
    }
}

/// What the thread keeps between walks: each root's last complete tree (a
/// nested root reads from it) and its walk history (growth), and what it read
/// from those trees, read again only when a tree changes: a reading scans its
/// whole tree, and every discovery asks for one.
#[derive(Default)]
struct Kept {
    trees: BTreeMap<PathBuf, Arc<Tree>>,
    history: BTreeMap<PathBuf, VecDeque<(Instant, u64)>>,
    /// Each planned root's reading from the kept walks around it ([`derive_nested`]).
    derived: BTreeMap<PathBuf, Derived>,
    /// Each kept walk's rows found by their path ([`Tree::find`]), for the snapshot check.
    found: BTreeMap<PathBuf, (Arc<Tree>, Rows)>,
}

/// A walk's rows by their path relative to its root: what [`Tree::find`] gave.
type Rows = BTreeMap<PathBuf, Option<DirId>>;

/// The enclosing planned roots with a kept walk, outermost first: each walk and when it ended.
type Enclosing = Vec<(PathBuf, Arc<Tree>, Option<Instant>)>;

/// A planned root's reading from the walks around it, and the walks it read.
struct Derived {
    from: Enclosing,
    record: Option<Arc<RootRecord>>,
}

impl Derived {
    /// Whether `from` names the same walks this reading read.
    fn read(&self, from: &Enclosing) -> bool {
        self.from.len() == from.len()
            && self
                .from
                .iter()
                .zip(from)
                .all(|(a, b)| a.0 == b.0 && Arc::ptr_eq(&a.1, &b.1) && a.2 == b.2)
    }
}

/// What one demanded scope asked for.
#[derive(Clone, Copy, Debug, Default)]
struct Ask {
    glance_at: Option<Instant>,
    look_at: Option<Instant>,
    frees_at: Option<Instant>,
}

impl Ask {
    fn pace(&self, now: Instant) -> Option<Pace> {
        let live =
            |t: Option<Instant>| t.is_some_and(|t| now.saturating_duration_since(t) < DEMAND_TTL);
        if live(self.look_at) {
            Some(Pace::Look)
        } else if live(self.glance_at) {
            Some(Pace::Glance)
        } else {
            None
        }
    }
}

struct Active {
    path: PathBuf,
    ticket: Ticket,
    started: Instant,
    detail: Detail,
    pace: Pace,
    prior: Option<Arc<RootRecord>>,
    walked: u64,
}

/// The calling thread's CPU time, where the platform reads it, else `None`.
fn thread_cpu() -> Option<Duration> {
    #[cfg(unix)]
    {
        let mut stamp = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `stamp` is a valid, writable `timespec` that outlives the call.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut stamp) } != 0 {
            return None;
        }
        let secs = u64::try_from(stamp.tv_sec).ok()?;
        let nanos = u32::try_from(stamp.tv_nsec).ok()?;
        Some(Duration::new(secs, nanos))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// What a stretch of the walker's work cost: its thread's CPU time where read, else the wall
/// time it took.
struct Cost {
    cpu: Option<Duration>,
    wall: Instant,
}

impl Cost {
    fn start() -> Self {
        Self {
            cpu: thread_cpu(),
            wall: Instant::now(),
        }
    }

    fn spent(&self) -> Duration {
        match (self.cpu, thread_cpu()) {
            (Some(a), Some(b)) => b.saturating_sub(a),
            _ => self.wall.elapsed(),
        }
    }
}

struct Engine {
    inbox: Receiver<ToWalker>,
    out: Arc<Mailbox>,
    proxy: Option<Proxy>,
    facts: Facts,
    /// Moves when a pane opens or closes or a tab's panes change: a discovery runs at once.
    shape_seq: u64,
    demand: BTreeMap<DiskScope, Ask>,
    policy: Option<Policy>,
    reads: PaneReads,
    panes: BTreeMap<u64, PaneLife>,
    own_claims: Vec<Claim>,
    opt_in_claims: Vec<Claim>,
    pools: BTreeMap<PathBuf, Pool>,
    home_pool: Option<VolumeKey>,
    lifts: HashMap<PathBuf, (Instant, Lift)>,
    /// Periodic discoveries so far, and when the last ran.
    round: u64,
    periodic_at: Option<Instant>,
    /// The facts' shape and the scopes the last discovery covered.
    discovered: Option<(u64, BTreeSet<DiskScope>)>,
    /// Since when each root no demanded scope holds has been left.
    left: BTreeMap<PathBuf, Instant>,
    records: BTreeMap<PathBuf, Arc<RootRecord>>,
    kept: Kept,
    huge: BTreeMap<PathBuf, HugeDir>,
    outside: BTreeSet<PathBuf>,
    huge_seq: u64,
    profiles_at: Option<Instant>,
    /// A profile's executables answer and when it was read.
    exec_known: BTreeMap<PathBuf, (Instant, bool)>,
    budget: GlanceBudget,
    walker: Walker,
    active: Option<Active>,
    /// Something the board reads may have moved since the last publication.
    dirty: bool,
    /// The board last stored: a publication equal to it is not stored again.
    last: Option<Arc<Published>>,
    /// Sleeps and walk starts, for [`Probe`].
    #[cfg(test)]
    tally: (u64, u64),
}

fn walker_main(
    inbox: Receiver<ToWalker>,
    out: Arc<Mailbox>,
    proxy: Option<Proxy>,
    disk: aterm_diskwalk::DiskIo,
) {
    let policy_out = aterm_diskwalk::set_walker_thread_policy(disk);
    if !policy_out.failed.is_empty() {
        aterm_log::warn!("disk walker: I/O policy not taken: {:?}", policy_out.failed);
    }
    Engine::new(inbox, out, proxy).run();
}

impl Engine {
    fn new(inbox: Receiver<ToWalker>, out: Arc<Mailbox>, proxy: Option<Proxy>) -> Self {
        Self {
            inbox,
            out,
            proxy,
            facts: Facts::default(),
            shape_seq: 0,
            demand: BTreeMap::new(),
            policy: None,
            reads: PaneReads::live(),
            panes: BTreeMap::new(),
            own_claims: Vec::new(),
            opt_in_claims: Vec::new(),
            pools: BTreeMap::new(),
            home_pool: None,
            lifts: HashMap::new(),
            round: 0,
            periodic_at: None,
            discovered: None,
            left: BTreeMap::new(),
            records: BTreeMap::new(),
            kept: Kept::default(),
            huge: BTreeMap::new(),
            outside: BTreeSet::new(),
            huge_seq: 0,
            profiles_at: None,
            exec_known: BTreeMap::new(),
            budget: GlanceBudget::default(),
            walker: Walker::new(),
            active: None,
            dirty: false,
            last: None,
            #[cfg(test)]
            tally: (0, 0),
        }
    }

    fn run(&mut self) {
        loop {
            if !self.drain() {
                return;
            }
            let now = Instant::now();
            self.discover_if_due(now);
            self.slow_tier_if_due(now);
            if self.active.is_some() {
                self.step_walk();
            } else if let Some(path) = self.next_root(now) {
                self.start_walk(&path);
            } else {
                self.publish_if_dirty();
                if !self.sleep(now) {
                    return;
                }
            }
            self.publish_if_dirty();
            #[cfg(windows)]
            if self.active.is_some() && !self.rest_released() {
                return;
            }
        }
    }

    /// Close the walk in flight's directories and keep them closed for [`RELEASED_FOR`], taking
    /// messages as they come; `false` when the thread is to stop.
    #[cfg(windows)]
    fn rest_released(&mut self) -> bool {
        self.walker.release();
        let until = Instant::now() + RELEASED_FOR;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            match self.inbox.recv_timeout(left) {
                Ok(msg) => {
                    if !self.apply(msg) {
                        return false;
                    }
                }
                Err(RecvTimeoutError::Timeout) => return true,
                Err(RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    fn drain(&mut self) -> bool {
        loop {
            match self.inbox.try_recv() {
                Ok(msg) => {
                    if !self.apply(msg) {
                        return false;
                    }
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn apply(&mut self, msg: ToWalker) -> bool {
        match msg {
            ToWalker::Facts(facts) => self.take_facts(facts),
            ToWalker::Demand {
                scope,
                detail,
                pace,
                at,
            } => {
                let ask = self.demand.entry(scope).or_default();
                let slot = match pace {
                    Pace::Glance => &mut ask.glance_at,
                    Pace::Look => &mut ask.look_at,
                };
                *slot = Some(slot.map_or(at, |t| t.max(at)));
                if detail == Detail::Frees {
                    ask.frees_at = Some(at);
                }
            }
            #[cfg(test)]
            ToWalker::Stop => return false,
            #[cfg(test)]
            ToWalker::ReadDiskPolicy(reply) => {
                let _ = reply.send(aterm_diskwalk::thread_disk_throttled());
            }
            #[cfg(test)]
            ToWalker::Probe(reply) => {
                let now = Instant::now();
                let owed = self.owed(now);
                let unmetered = Pick {
                    opens: now,
                    ..owed.pick()
                };
                let held_back = self.active.is_none()
                    && self.next_root(now).is_none()
                    && self.policy.is_some()
                    && !self.live(now).is_empty()
                    && pick_root(&unmetered, &self.records, now).is_some();
                let _ = reply.send(Probe {
                    cpu: thread_cpu(),
                    glance_taken: self.budget.taken(now),
                    held_back,
                    sleeps: self.tally.0,
                    starts: self.tally.1,
                });
            }
        }
        true
    }

    /// Take new facts. A changed front program or report waits for the next discovery and
    /// rebuilds nothing; a pane that opens or closes asks for a discovery at once; a changed home,
    /// deny override or opt-in rebuilds the policy.
    fn take_facts(&mut self, facts: Facts) {
        if facts == self.facts {
            return;
        }
        let policy_moved = facts.policy_inputs() != self.facts.policy_inputs();
        let shape_moved = facts.shape() != self.facts.shape();
        self.facts = facts;
        if shape_moved {
            self.shape_seq += 1;
        }
        if policy_moved || self.policy.is_none() {
            self.policy = self.facts.home.as_deref().map(|home| {
                Policy::new(
                    Deny::for_home(home, &self.facts.overrides, &self.facts.opt_in),
                    aterm_sysprobe::volumes::table(),
                )
            });
            self.lifts.clear();
            self.home_pool = None;
        }
    }

    fn sleep(&mut self, now: Instant) -> bool {
        #[cfg(test)]
        {
            self.tally.0 += 1;
        }
        let live = !self.live(now).is_empty();
        let mut until = self
            .profiles_at
            .map_or(now + PROFILES_EVERY, |t| t + PROFILES_EVERY);
        if live {
            until = until.min(now + DISCOVER_EVERY);
            if let Some(t) = self.next_due(now) {
                until = until.min(t);
            }
        }
        match self.inbox.recv_timeout(
            until
                .saturating_duration_since(now)
                .max(Duration::from_millis(1)),
        ) {
            Ok(msg) => self.apply(msg),
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => false,
        }
    }

    /// The scopes demanded at `now`, each at the pace its freshest ask names.
    fn live(&self, now: Instant) -> BTreeMap<DiskScope, Pace> {
        self.demand
            .iter()
            .filter_map(|(s, a)| a.pace(now).map(|p| (*s, p)))
            .collect()
    }

    fn fold(&self) -> bool {
        self.policy
            .as_ref()
            .map_or(cfg!(target_vendor = "apple"), |p| p.deny.fold_case())
    }

    /// The panes a demanded scope holds: every pane for aterm, a tab's own otherwise.
    fn wanted_panes(&self, live: &BTreeMap<DiskScope, Pace>) -> Vec<PaneFacts> {
        let aterm = live.contains_key(&DiskScope::Aterm);
        self.facts
            .panes
            .iter()
            .filter(|p| {
                aterm
                    || self.facts.tabs.iter().any(|t| {
                        t.panes.contains(&p.key) && live.contains_key(&DiskScope::Tab(t.id))
                    })
            })
            .cloned()
            .collect()
    }

    /// The members of `scope` now.
    fn members_now(&self, scope: DiskScope) -> Vec<PathBuf> {
        match scope {
            DiskScope::Tab(id) => self
                .facts
                .tabs
                .iter()
                .filter(|t| t.id == id)
                .flat_map(|t| t.panes.iter())
                .filter_map(|k| self.panes.get(k))
                .flat_map(|l| l.membership.members().cloned())
                .collect(),
            DiskScope::Aterm => self
                .panes
                .values()
                .flat_map(|l| l.membership.members().cloned())
                .chain(self.own_claims.iter().map(|c| c.path.clone()))
                .collect(),
        }
    }

    /// The roots the demanded scopes hold now, each at the fastest pace a scope holding it asks,
    /// with the opt-ins at aterm's. A member on another pool than home's adds nothing to a scope
    /// and is not walked; an opt-in is walked wherever it lies.
    fn held(&self, now: Instant) -> BTreeMap<PathBuf, Pace> {
        let mut out: BTreeMap<PathBuf, Pace> = BTreeMap::new();
        for (scope, pace) in self.live(now) {
            let mut roots = self.members_now(scope);
            roots.retain(|r| self.pools.get(r) != Some(&Pool::Other));
            if scope == DiskScope::Aterm {
                roots.extend(self.opt_in_claims.iter().map(|c| c.path.clone()));
            }
            for r in roots {
                let at = out.entry(r).or_insert(pace);
                *at = (*at).max(pace);
            }
        }
        out
    }

    /// The roots a demanded tab holds (the front tabs').
    fn tab_held(&self, now: Instant) -> BTreeSet<PathBuf> {
        self.live(now)
            .into_keys()
            .filter(|s| matches!(s, DiskScope::Tab(_)))
            .flat_map(|s| self.members_now(s))
            .collect()
    }

    fn frees_wanted(&self, now: Instant) -> BTreeSet<PathBuf> {
        let fold = self.fold();
        let held = self.plan(now);
        let mut out = BTreeSet::new();
        for (scope, ask) in &self.demand {
            let wanted = ask
                .frees_at
                .is_some_and(|t| now.saturating_duration_since(t) < DEMAND_TTL);
            if !wanted || ask.pace(now).is_none() {
                continue;
            }
            for c in self.members_now(*scope) {
                for p in &held {
                    if within(&c, p, fold) {
                        out.insert(p.clone());
                    }
                }
                out.insert(c);
            }
        }
        out
    }

    /// The walk plan: only the roots a demanded scope holds now, outermost first.
    fn plan(&self, now: Instant) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.held(now).into_keys().collect();
        paths.sort_by(|a, b| {
            a.components()
                .count()
                .cmp(&b.components().count())
                .then(a.cmp(b))
        });
        paths
    }

    fn discover_if_due(&mut self, now: Instant) {
        let live = self.live(now);
        if live.is_empty() || self.policy.is_none() {
            return;
        }
        let scopes: BTreeSet<DiskScope> = live.keys().copied().collect();
        let periodic = self
            .periodic_at
            .is_none_or(|t| now.saturating_duration_since(t) >= DISCOVER_EVERY);
        let prompt = self.discovered.as_ref() != Some(&(self.shape_seq, scopes.clone()));
        if periodic || prompt {
            self.discover(now, periodic);
        }
    }

    /// One discovery. A periodic one reads every demanded pane and applies the join and leave
    /// hysteresis; one at once (a pane opened or closed, a scope newly demanded) reads only the
    /// panes no recent discovery read, as their first. A pane names what its live directory
    /// lifts to ([`Self::see_pane`]) and what the builds started in it compiled
    /// ([`Self::built_by`]), wherever the pane is now.
    fn discover(&mut self, now: Instant, periodic: bool) {
        let live = self.live(now);
        let Some(mut policy) = self.policy.clone() else {
            return;
        };
        if periodic {
            policy.mounts = aterm_sysprobe::volumes::table();
            self.policy = Some(policy.clone());
            self.round += 1;
            self.periodic_at = Some(now);
        }
        let home = self.facts.home.clone();
        if periodic || self.home_pool.is_none() {
            self.home_pool = home
                .as_deref()
                .and_then(|h| (self.reads.pool)(h, &policy.mounts));
        }
        self.lifts
            .retain(|_, (at, _)| now.saturating_duration_since(*at) < LIFT_FOR);
        let wanted = self.wanted_panes(&live);
        // One bounded read of the record, and none where no pane is read.
        let starts = if wanted.is_empty() {
            Vec::new()
        } else {
            (self.reads.starts)()
        };
        for pane in wanted {
            let fresh = self
                .panes
                .get(&pane.key)
                .is_some_and(|l| l.membership.fresh(now));
            if !periodic && fresh {
                continue;
            }
            let mut seen = self.see_pane(&pane, &policy, home.as_deref(), now);
            for claim in self.built_by(&pane.session, &starts, &policy, home.as_deref(), now) {
                if !seen.named.iter().any(|c| c.path == claim.path) {
                    seen.named.push(claim);
                }
            }
            let named: BTreeSet<PathBuf> = seen.named.iter().map(|c| c.path.clone()).collect();
            let round = self.round;
            let life = self.panes.entry(pane.key).or_default();
            life.membership.observe(&named, round, now);
            for c in seen.named {
                life.claims.insert(c.path.clone(), c);
            }
            let keep: BTreeSet<PathBuf> = life
                .membership
                .members()
                .cloned()
                .chain(named.iter().cloned())
                .collect();
            life.claims.retain(|p, _| keep.contains(p));
            life.view = Some(seen.view);
        }
        let open: BTreeSet<u64> = self.facts.panes.iter().map(|p| p.key).collect();
        self.panes.retain(|k, _| open.contains(k));
        let fold = policy.deny.fold_case();
        let own = own_folders_claims(&self.facts.own, fold);
        self.own_claims = own;
        self.opt_in_claims = self
            .facts
            .opt_in
            .iter()
            .filter_map(|p| canonical(p))
            .map(|p| Claim::of(p, RootKind::OptIn, false))
            .collect();
        self.refresh_pools(&policy);
        self.age_records(now);
        self.rederive_nested(now);
        self.invalidate_changed_snapshots(now);
        self.discovered = Some((self.shape_seq, live.into_keys().collect()));
        self.dirty = true;
    }

    /// What one pane's live directory is now: the directory, its project, why it adds nothing,
    /// and what it names (what the builds started in the pane compiled is
    /// [`Self::built_by`]'s). L and P each pass the path test before any syscall touches them; a
    /// refused one is named, not counted, and nothing under it is opened.
    fn see_pane(
        &mut self,
        pane: &PaneFacts,
        policy: &Policy,
        home: Option<&Path>,
        now: Instant,
    ) -> Seen {
        let mut view = PaneView::unseen(pane.key);
        let (path, from, pid) = match live_dir(pane, &*self.reads.cwd) {
            Live::Elsewhere { host } => {
                view.from = Some(LiveFrom::Osc7);
                view.host = host;
                view.why = Some(PaneWhy::Elsewhere);
                return Seen {
                    view,
                    named: Vec::new(),
                };
            }
            Live::Unknown => {
                return Seen {
                    view,
                    named: Vec::new(),
                };
            }
            Live::At { path, from, pid } => (path, from, pid),
        };
        view.from = Some(from);
        view.exe = (self.reads.exe)(pid);
        let Some(live) = canonical(&firmlinked(&path)) else {
            view.why = Some(PaneWhy::Refused(Refusal::Relative));
            return Seen {
                view,
                named: Vec::new(),
            };
        };
        view.live = Some(live.clone());
        if let Some(why) = refuse(None, &live, &policy.mounts, &policy.deny) {
            view.why = Some(PaneWhy::Refused(why));
            return Seen {
                view,
                named: Vec::new(),
            };
        }
        let pool = Pool::of(
            (self.reads.pool)(&live, &policy.mounts).as_ref(),
            self.home_pool.as_ref(),
        );
        if pool == Pool::Other {
            view.why = Some(PaneWhy::OtherPool);
            return Seen {
                view,
                named: Vec::new(),
            };
        }
        let lift = match self.lifts.get(&live) {
            Some((_, l)) => l.clone(),
            None => {
                let l = lift(&live, home, policy);
                self.lifts.insert(live.clone(), (now, l.clone()));
                l
            }
        };
        view.project = Some(lift.project.clone());
        if let Some(why) = refuse(None, &lift.project, &policy.mounts, &policy.deny) {
            view.why = Some(PaneWhy::Refused(why));
            return Seen {
                view,
                named: Vec::new(),
            };
        }
        view.why = None;
        let mut named = vec![Claim::of(lift.project.clone(), lift.kind, lift.linked)];
        named.extend(
            lift.builds
                .iter()
                .map(|b| Claim::of(b.clone(), RootKind::Build, false)),
        );
        Seen { view, named }
    }

    /// `path` as a root a build-start line may name: canonical, past the path test, a directory
    /// by one no-follow `stat` in its parent, and on home's pool or one unknown. A line is what
    /// some build wrote, so its paths get every test a pane's live directory gets before
    /// anything under them is opened.
    fn built_root(&self, path: &Path, policy: &Policy) -> Option<PathBuf> {
        let path = canonical(&firmlinked(path))?;
        if refuse(None, &path, &policy.mounts, &policy.deny).is_some() {
            return None;
        }
        let leaf = path.file_name().and_then(|n| n.to_str())?;
        if !matches!(
            marker_at(path.parent()?, leaf, policy),
            Ok(Some(Marker::Dir))
        ) {
            return None;
        }
        let pool = Pool::of(
            (self.reads.pool)(&path, &policy.mounts).as_ref(),
            self.home_pool.as_ref(),
        );
        (pool != Pool::Other).then_some(path)
    }

    /// What the builds started in the pane whose session is `session` compiled, from the
    /// build-start lines `starts` (oldest first): for each of that session's newest
    /// [`MAX_BUILT_PER_PANE`] lines whose workspace is still a root a line may name
    /// ([`Self::built_root`]), the workspace's project and the project's build output ([`lift`],
    /// as a live directory's), and the line's build folder where it is such a root too and lies
    /// in neither. Each path once. "Newest" is the record's own order, the order the lines were
    /// appended in, never a line's `at_ms`: that is its writer's word, and a line dated ahead of
    /// the clock (a forged one, or one a clock that stepped back wrote) would outrank every line
    /// written after it. A line names roots and nothing else: it gives no leave to remove
    /// anything.
    fn built_by(
        &mut self,
        session: &str,
        starts: &[atpkg::build_start::Start],
        policy: &Policy,
        home: Option<&Path>,
        now: Instant,
    ) -> Vec<Claim> {
        let fold = policy.deny.fold_case();
        let mine = starts
            .iter()
            .rev()
            .filter(|s| s.session == session)
            .take(MAX_BUILT_PER_PANE);
        let mut named: Vec<Claim> = Vec::new();
        for start in mine {
            let Some(workspace) = self.built_root(&start.workspace, policy) else {
                continue;
            };
            let lift = match self.lifts.get(&workspace) {
                Some((_, l)) => l.clone(),
                None => {
                    let l = lift(&workspace, home, policy);
                    self.lifts.insert(workspace, (now, l.clone()));
                    l
                }
            };
            if refuse(None, &lift.project, &policy.mounts, &policy.deny).is_some() {
                continue;
            }
            let mut claims = vec![Claim::of(lift.project.clone(), lift.kind, lift.linked)];
            claims.extend(
                lift.builds
                    .iter()
                    .map(|b| Claim::of(b.clone(), RootKind::Build, false)),
            );
            if let Some(build) = start
                .build
                .as_deref()
                .and_then(|b| self.built_root(b, policy))
                && !within(&build, &lift.project, fold)
                && !lift.builds.iter().any(|b| within(&build, b, fold))
            {
                claims.push(Claim::of(build, RootKind::Build, false));
            }
            for claim in claims {
                if !named.iter().any(|c| c.path == claim.path) {
                    named.push(claim);
                }
            }
        }
        named
    }

    /// Each claimed root's pool, from the mount table the discovery read.
    fn refresh_pools(&mut self, policy: &Policy) {
        let paths: BTreeSet<PathBuf> = self
            .panes
            .values()
            .flat_map(|l| l.claims.keys().cloned())
            .chain(self.own_claims.iter().map(|c| c.path.clone()))
            .chain(self.opt_in_claims.iter().map(|c| c.path.clone()))
            .collect();
        let mut pools = BTreeMap::new();
        for p in paths {
            let key = (self.reads.pool)(&p, &policy.mounts);
            pools.insert(p, Pool::of(key.as_ref(), self.home_pool.as_ref()));
        }
        self.pools = pools;
    }

    /// Keep each root's record [`RETIRE_AFTER`] after it leaves every demanded scope, then drop it.
    fn age_records(&mut self, now: Instant) {
        let held: BTreeSet<PathBuf> = self.held(now).into_keys().collect();
        let known: Vec<PathBuf> = self
            .records
            .keys()
            .chain(self.kept.trees.keys())
            .cloned()
            .collect();
        for p in known {
            if held.contains(&p) {
                self.left.remove(&p);
            } else {
                self.left.entry(p).or_insert(now);
            }
        }
        self.left.retain(|p, _| !held.contains(p));
        let gone: Vec<PathBuf> = self
            .left
            .iter()
            .filter(|(_, t)| now.saturating_duration_since(**t) >= RETIRE_AFTER)
            .map(|(p, _)| p.clone())
            .collect();
        for p in gone {
            self.left.remove(&p);
            if self.active.as_ref().is_some_and(|a| a.path == p) {
                continue;
            }
            self.records.remove(&p);
            self.kept.trees.remove(&p);
            self.kept.history.remove(&p);
        }
    }

    fn rederive_nested(&mut self, now: Instant) {
        let plan = self.plan(now);
        let fold = self.fold();
        let active = self.active.as_ref().map(|a| a.path.clone());
        rederive(
            &plan,
            &mut self.kept,
            &mut self.records,
            active.as_deref(),
            fold,
        );
    }

    /// A long walk's backoff must not keep a deleted cache on screen for hours.
    /// Check only retained roots and the subtrees their rows expose, on this
    /// background thread, with the same no-follow and privacy fences as a walk.
    fn invalidate_changed_snapshots(&mut self, now: Instant) {
        let Some(policy) = self.policy.as_ref() else {
            return;
        };
        let mut changed = Vec::new();
        let found = &mut self.kept.found;
        found.retain(|root, (t, _)| self.kept.trees.get(root).is_some_and(|k| Arc::ptr_eq(k, t)));
        for (root, tree) in &self.kept.trees {
            if self
                .active
                .as_ref()
                .is_some_and(|a| root.starts_with(&a.path) || a.path.starts_with(root))
            {
                continue;
            }
            let Some(record) = self.records.get(root) else {
                continue;
            };
            let (Some((dev, ino)), Some(complete)) = (record.id, record.complete.as_ref()) else {
                continue;
            };
            if record.state != RootState::Complete || record.read_from.is_some() {
                continue;
            }
            let mut visible: BTreeSet<PathBuf> = complete
                .biggest
                .iter()
                .chain(&complete.most_entries)
                .map(|row| row.rel.clone())
                .collect();
            visible.extend(
                self.records
                    .iter()
                    .filter(|(_, row)| row.read_from.as_ref() == Some(root))
                    .filter_map(|(path, _)| path.strip_prefix(root).ok().map(Path::to_path_buf)),
            );
            let ids = &mut found
                .entry(root.clone())
                .or_insert_with(|| (Arc::clone(tree), BTreeMap::new()))
                .1;
            let stale = directory_identity(root, policy) != Ok((dev, ino))
                || visible.iter().any(|rel| {
                    let id = *ids.entry(rel.clone()).or_insert_with(|| tree.find(rel));
                    let Some(row) = id.and_then(|id| tree.get(id)) else {
                        return true;
                    };
                    row.refused.is_none()
                        && !row.mount_boundary
                        && directory_identity(&root.join(rel), policy) != Ok((dev, row.ino))
                });
            if stale {
                changed.push(root.clone());
            }
        }
        if changed.is_empty() {
            return;
        }
        // Drop covering snapshots too, or rederive_nested could resurrect the
        // removed row from an older ancestor's tree. Unrelated roots stay warm.
        let overlaps = |path: &Path| {
            changed
                .iter()
                .any(|root| path.starts_with(root) || root.starts_with(path))
        };
        self.records.retain(|path, _| !overlaps(path));
        self.kept.trees.retain(|path, _| !overlaps(path));
        self.kept.history.retain(|path, _| !overlaps(path));
        let huge = self
            .huge
            .iter()
            .filter(|(path, _)| !overlaps(path))
            .map(|(path, row)| (path.clone(), row.clone()))
            .collect();
        let outside = self
            .outside
            .iter()
            .filter(|path| !overlaps(path))
            .cloned()
            .collect();
        self.set_huge(huge, outside);
        self.rederive_nested(now);
        self.dirty = true;
    }

    /// What the schedule reads at `now` ([`Pick`]).
    fn owed(&self, now: Instant) -> Owed {
        Owed {
            plan: self.plan(now),
            paces: self.held(now),
            tab_held: self.tab_held(now),
            frees: self.frees_wanted(now),
            opens: self.budget.opens_at(now),
            fold: self.fold(),
        }
    }

    fn next_root(&self, now: Instant) -> Option<PathBuf> {
        if self.live(now).is_empty() || self.policy.is_none() {
            return None;
        }
        pick_root(&self.owed(now).pick(), &self.records, now)
    }

    /// When the next walk is owed ([`wake_at`]): the thread sleeps until then.
    fn next_due(&self, now: Instant) -> Option<Instant> {
        if self.live(now).is_empty() || self.policy.is_none() {
            return None;
        }
        wake_at(&self.owed(now).pick(), &self.records, now)
    }

    fn put(&mut self, rec: RootRecord) {
        self.records.insert(rec.path.clone(), Arc::new(rec));
        self.dirty = true;
    }

    fn start_walk(&mut self, path: &Path) {
        let Some(base) = self.policy.as_ref() else {
            return;
        };
        let now = Instant::now();
        let pace = self.held(now).get(path).copied().unwrap_or(Pace::Glance);
        let detail = if self.frees_wanted(now).contains(path) {
            Detail::Frees
        } else {
            Detail::Sizes
        };
        let mut policy = base.clone();
        policy.private = detail == Detail::Frees;
        policy.links_out = detail == Detail::Frees;
        let prior = self.records.get(path).cloned();
        let root = RootSpec::new(path);
        let cost = Cost::start();
        #[cfg(test)]
        {
            self.tally.1 += 1;
        }
        let started = self.walker.start(&root, &policy);
        if pace == Pace::Glance {
            self.budget.charge(now, cost.spent());
        }
        match started {
            Err(why) => {
                let (state, level) = match why {
                    Refusal::TooBroad => (
                        RootState::OneLevel,
                        list_one_level(&root, &policy).unwrap_or_default(),
                    ),
                    other => (RootState::NotMeasured(other), Vec::new()),
                };
                self.put(RootRecord {
                    state,
                    level,
                    asked: Some(now),
                    ..RootRecord::queued(path.to_path_buf())
                });
            }
            Ok(ticket) => {
                self.active = Some(Active {
                    path: path.to_path_buf(),
                    ticket,
                    started: now,
                    detail,
                    pace,
                    prior,
                    walked: 0,
                });
                self.put_walking();
            }
        }
    }

    /// The running walk's record: its partial sums for progress, and the figure it keeps until
    /// it ends, so a fill never blanks while it is measured.
    fn put_walking(&mut self) {
        let (Some(a), Some(walk)) = (self.active.as_ref(), self.walker.walk()) else {
            return;
        };
        let rec = RootRecord {
            path: a.path.clone(),
            state: RootState::Walking,
            id: Some((walk.root_dev(), walk.tree().root().ino)),
            partial: Some(walk.partial()),
            walked_entries: a.walked,
            complete: a
                .prior
                .as_ref()
                .filter(|r| r.read_from.is_none())
                .and_then(|r| r.complete.clone()),
            level: Vec::new(),
            read_from: None,
            asked: Some(a.started),
        };
        self.put(rec);
    }

    fn step_walk(&mut self) {
        let Some((ticket, pace)) = self.active.as_ref().map(|a| (a.ticket, a.pace)) else {
            return;
        };
        let until = Instant::now() + PUBLISH_EVERY;
        let cost = Cost::start();
        let mut clock = Instant::now;
        let mut ended = None;
        loop {
            match self.walker.slice(ticket, &mut clock) {
                Sliced::Step(Step::More { walked }) => {
                    if let Some(a) = self.active.as_mut() {
                        a.walked = walked;
                    }
                    if Instant::now() >= until {
                        break;
                    }
                }
                Sliced::Step(Step::Done) => {
                    ended = Some(true);
                    break;
                }
                Sliced::Step(Step::Cancelled) | Sliced::Stale => {
                    ended = Some(false);
                    break;
                }
            }
        }
        // The thread turns to other work now (publishing, discovery, the slow tier's profile
        // walks): the walk keeps no directory open meanwhile, which on Windows would fail a
        // rename of every folder above its cursor.
        self.walker.release();
        let now = Instant::now();
        if pace == Pace::Glance {
            self.budget.charge(now, cost.spent());
        }
        match ended {
            Some(true) => return self.finish_walk(),
            Some(false) => return self.abandon_walk(),
            None => {}
        }
        let held = self
            .active
            .as_ref()
            .is_some_and(|a| self.held(now).contains_key(&a.path));
        if self.live(now).is_empty() || !held {
            self.walker.cancel();
            self.abandon_walk();
            return;
        }
        self.put_walking();
    }

    fn abandon_walk(&mut self) {
        let Some(a) = self.active.take() else {
            return;
        };
        let _ = self.walker.take();
        match a.prior {
            Some(r) => {
                self.records.insert(a.path, r);
            }
            None => {
                self.records.remove(&a.path);
            }
        }
        self.dirty = true;
    }

    fn finish_walk(&mut self) {
        let Some(a) = self.active.take() else {
            return;
        };
        let Some(walk) = self.walker.take() else {
            self.records.remove(&a.path);
            self.dirty = true;
            return;
        };
        let id = (walk.root_dev(), walk.tree().root().ino);
        let links = walk.links();
        let tree = Arc::new(walk.into_tree());
        let at = Instant::now();
        let history = self.kept.history.entry(a.path.clone()).or_default();
        history.push_back((at, tree.root().subtree.alloc_fair));
        while history
            .front()
            .is_some_and(|&(t, _)| at.saturating_duration_since(t) > GROWTH_WINDOW * 2)
        {
            history.pop_front();
        }
        let growth = growth_of(history, at);
        let mut complete = complete_of(
            &tree,
            DirId::ROOT,
            at,
            at.saturating_duration_since(a.started),
            growth,
        );
        complete.detail = a.detail;
        complete.links = links;
        self.put(RootRecord {
            state: RootState::Complete,
            id: Some(id),
            walked_entries: tree.root().subtree.files + tree.root().subtree.dirs,
            complete: Some(complete),
            asked: Some(a.started),
            ..RootRecord::queued(a.path.clone())
        });
        self.kept.trees.insert(a.path.clone(), Arc::clone(&tree));
        let records = &self.records;
        self.kept.history.retain(|p, _| records.contains_key(p));
        self.rederive_nested(at);
        let found: BTreeMap<PathBuf, HugeDir> = tree
            .huge_dirs()
            .into_iter()
            .map(|h| (h.path.clone(), h))
            .collect();
        let next = fold_raised(&self.huge, tree.root_path(), &found);
        self.set_huge(next, self.outside.clone());
        self.invalidate_changed_snapshots(at);
    }

    fn set_huge(&mut self, huge: BTreeMap<PathBuf, HugeDir>, outside: BTreeSet<PathBuf>) {
        if huge != self.huge {
            self.huge_seq += 1;
            self.dirty = true;
        }
        if outside != self.outside {
            self.dirty = true;
        }
        self.huge = huge;
        self.outside = outside;
    }

    fn slow_tier_if_due(&mut self, now: Instant) {
        let (Some(policy), Some(home)) = (self.policy.clone(), self.facts.home.clone()) else {
            return;
        };
        if self
            .profiles_at
            .is_some_and(|t| now.saturating_duration_since(t) < PROFILES_EVERY)
        {
            return;
        }
        self.profiles_at = Some(now);
        let covered = self.plan(now);
        let mut counted: BTreeMap<PathBuf, Option<HugeDir>> = BTreeMap::new();
        for target in outside_build_dirs(&home, &policy) {
            if covered.iter().any(|r| target.starts_with(r)) {
                continue;
            }
            for (deps, n) in profile_deps(&target, &policy) {
                let row = (n >= HUGE_NOTE).then(|| {
                    let executables = self.executables_of(&deps, &policy, now);
                    HugeDir {
                        path: deps.clone(),
                        entries: n,
                        executables,
                        warning: n >= HUGE_WARN,
                    }
                });
                counted.insert(deps, row);
            }
        }
        let exec_known = &mut self.exec_known;
        exec_known.retain(|p, _| counted.get(p).is_some_and(Option::is_some));
        let (next, outside) = fold_outside(&self.huge, &self.outside, &counted, &covered);
        self.set_huge(next, outside);
    }

    fn executables_of(&mut self, deps: &Path, policy: &Policy, now: Instant) -> bool {
        if let Some(&(at, known)) = self.exec_known.get(deps)
            && now.saturating_duration_since(at) < EXEC_KNOWN_FOR
        {
            return known;
        }
        match profile_executables(deps, policy, PROFILE_WALK_BUDGET) {
            Some(known) => {
                self.exec_known.insert(deps.to_path_buf(), (now, known));
                known
            }
            None => false,
        }
    }

    fn published(&self) -> Published {
        Published {
            tabs: self.facts.tabs.clone(),
            panes: self.panes.iter().map(|(k, l)| (*k, l.view(*k))).collect(),
            own_claims: self.own_claims.clone(),
            opt_in: self.opt_in_claims.clone(),
            pools: self.pools.clone(),
            home_pool: self.home_pool.clone(),
            records: self.records.clone(),
            huge: self.huge.clone(),
            outside: self.outside.clone(),
            huge_seq: self.huge_seq,
            discovered: match &self.discovered {
                Some((seq, scopes)) if *seq == self.shape_seq => scopes.clone(),
                _ => BTreeSet::new(),
            },
            fold_case: self.fold(),
        }
    }

    /// Store the board and wake the loop, when something may have moved and the board differs
    /// from the one last stored: a periodic discovery that finds the same panes, claims, pools
    /// and records stores nothing and wakes nothing, so a deck at rest does not wake the loop
    /// every [`DISCOVER_EVERY`] for a board it already holds.
    fn publish_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let published = self.published();
        if self.last.as_deref() == Some(&published) {
            return;
        }
        let published = Arc::new(published);
        self.last = Some(Arc::clone(&published));
        let proxy = self.proxy.as_ref();
        self.out.store(published, || {
            if let Some(proxy) = proxy {
                let _ = proxy.send_event(crate::Wake::DiskWalk);
            }
        });
    }
}

/// aterm's own folders as claims.
fn own_folders_claims(own: &[PathBuf], fold: bool) -> Vec<Claim> {
    let canon: Vec<PathBuf> = own.iter().filter_map(|p| canonical(p)).collect();
    own_folders(&canon, fold)
        .into_iter()
        .map(|p| Claim::of(p, RootKind::Own, false))
        .collect()
}

/// What [`Engine::owed`] reads for a [`Pick`] at one instant.
struct Owed {
    plan: Vec<PathBuf>,
    paces: BTreeMap<PathBuf, Pace>,
    tab_held: BTreeSet<PathBuf>,
    frees: BTreeSet<PathBuf>,
    opens: Instant,
    fold: bool,
}

impl Owed {
    fn pick(&self) -> Pick<'_> {
        Pick {
            plan: &self.plan,
            paces: &self.paces,
            tab_held: &self.tab_held,
            frees: &self.frees,
            opens: self.opens,
            fold: self.fold,
        }
    }
}

/// What [`pick_root`] and [`wake_at`] read.
pub(crate) struct Pick<'a> {
    /// The walk plan ([`Engine::plan`]).
    pub(crate) plan: &'a [PathBuf],
    /// Each planned root's pace.
    pub(crate) paces: &'a BTreeMap<PathBuf, Pace>,
    /// The roots a demanded tab holds: their first walks are exempt from the glance budget, and
    /// walked first.
    pub(crate) tab_held: &'a BTreeSet<PathBuf>,
    /// The roots whose frees a reader wants.
    pub(crate) frees: &'a BTreeSet<PathBuf>,
    /// When the glance budget next lets a glance walk start ([`GlanceBudget::opens_at`]): at or
    /// before `now` while it allows one.
    pub(crate) opens: Instant,
    /// Whether paths nest ignoring case.
    pub(crate) fold: bool,
}

impl Pick<'_> {
    fn pace(&self, r: &Path) -> Pace {
        self.paces.get(r).copied().unwrap_or(Pace::Glance)
    }

    /// The first instant from `due` at which a walk of `r` may start: `due` at look pace, and
    /// no sooner than the budget opens at glance pace.
    fn metered(&self, r: &Path, due: Instant) -> Instant {
        match self.pace(r) {
            Pace::Look => due,
            Pace::Glance => due.max(self.opens),
        }
    }

    /// Whether `r` lies inside a planned root still being walked, or never walked yet.
    fn parent_pending(&self, r: &Path, records: &BTreeMap<PathBuf, Arc<RootRecord>>) -> bool {
        self.plan.iter().any(|q| {
            q.as_path() != r
                && within(r, q, self.fold)
                && !within(q, r, self.fold)
                && records
                    .get(q)
                    .is_none_or(|rec| matches!(rec.state, RootState::Queued | RootState::Walking))
        })
    }

    /// Whether `r` is owed its first walk: no record, or one queued with no walk to read.
    fn first(r: Option<&Arc<RootRecord>>) -> bool {
        r.is_none_or(|rec| rec.state == RootState::Queued && rec.read_from.is_none())
    }

    /// When a root its own walk measured is owed again at its pace: at once when its frees are
    /// asked and its walk kept none; `None` while it is queued or walking.
    fn due_again(&self, r: &Path, rec: &RootRecord, now: Instant) -> Option<Instant> {
        match rec.state {
            RootState::Queued | RootState::Walking => None,
            RootState::Complete => rec.complete.as_ref().map(|c| {
                if self.frees.contains(r) && c.detail != Detail::Frees {
                    now
                } else {
                    c.rewalk_at(self.pace(r))
                }
            }),
            RootState::NotMeasured(_) | RootState::OneLevel => {
                rec.asked.map(|t| t + self.pace(r).rewalk().0)
            }
        }
    }
}

/// The walk owed next, pure (see [`Engine::next_root`]): a root never walked first, a demanded
/// tab's ahead of the rest and outermost first, a root inside a root still being walked waiting
/// for it; then the stalest walk owed again at its pace. A glance walk starts only while the
/// budget allows, but a demanded tab's first walk of a root that just joined.
pub(crate) fn pick_root(
    p: &Pick<'_>,
    records: &BTreeMap<PathBuf, Arc<RootRecord>>,
    now: Instant,
) -> Option<PathBuf> {
    let may = |r: &Path| p.metered(r, now) <= now;
    let mut fresh: Vec<&PathBuf> = p
        .plan
        .iter()
        .filter(|r| {
            Pick::first(records.get(*r))
                && !p.parent_pending(r, records)
                && (p.tab_held.contains(*r) || may(r))
        })
        .collect();
    fresh.sort_by_key(|r| {
        (
            !p.tab_held.contains(*r),
            r.components().count(),
            (*r).clone(),
        )
    });
    if let Some(r) = fresh.first() {
        return Some((*r).clone());
    }
    let mut stalest: Option<(Instant, &PathBuf)> = None;
    for r in p.plan {
        let Some(rec) = records.get(r) else {
            continue;
        };
        if rec.read_from.is_some() || !may(r) {
            continue;
        }
        if let Some(t) = p.due_again(r, rec, now).filter(|t| *t <= now)
            && stalest.is_none_or(|(s, _)| t < s)
        {
            stalest = Some((t, r));
        }
    }
    stalest.map(|(_, r)| r.clone())
}

/// When [`pick_root`] next owes a walk on these inputs, pure (see [`Engine::next_due`]): at or
/// before `now` exactly while it owes one now; else the first instant a root comes due at its
/// pace, a glance walk no sooner than the budget opens; `None` while every root it could owe
/// waits on another's walk. The thread sleeps until then, so a glance root the budget holds
/// back wakes it once, when the budget opens.
pub(crate) fn wake_at(
    p: &Pick<'_>,
    records: &BTreeMap<PathBuf, Arc<RootRecord>>,
    now: Instant,
) -> Option<Instant> {
    p.plan
        .iter()
        .filter_map(|r| {
            let rec = records.get(r);
            if Pick::first(rec) {
                if p.parent_pending(r, records) {
                    return None;
                }
                return Some(if p.tab_held.contains(r) {
                    now
                } else {
                    p.metered(r, now)
                });
            }
            let rec = rec.filter(|rec| rec.read_from.is_none())?;
            p.due_again(r, rec, now).map(|due| p.metered(r, due))
        })
        .min()
}

/// Read every planned root that an enclosing planned root's kept walk holds from that walk
/// ([`derive_nested`]), drop a derived record no walk holds any more, and keep only the trees of
/// roots measured by their own walk. The running walk's root (`active`) keeps its record.
fn rederive(
    plan: &[PathBuf],
    kept: &mut Kept,
    records: &mut BTreeMap<PathBuf, Arc<RootRecord>>,
    active: Option<&Path>,
    fold: bool,
) {
    let mut derived: Vec<(PathBuf, Option<Arc<RootRecord>>)> = Vec::with_capacity(plan.len());
    for p in plan {
        let from = enclosing_walks(p, plan, kept, records, fold);
        if !kept.derived.get(p).is_some_and(|d| d.read(&from)) {
            let record = derive_nested(p, &from, fold).map(Arc::new);
            kept.derived.insert(p.clone(), Derived { from, record });
        }
        derived.push((
            p.clone(),
            kept.derived.get(p).and_then(|d| d.record.clone()),
        ));
    }
    for (p, d) in derived {
        if active == Some(p.as_path()) {
            continue;
        }
        match d {
            Some(r) => {
                records.insert(p, r);
            }
            None => {
                if records.get(&p).is_some_and(|r| r.read_from.is_some()) {
                    records.remove(&p);
                }
            }
        }
    }
    kept.trees
        .retain(|p, _| records.get(p).is_some_and(|r| r.read_from.is_none()));
    let trees = &kept.trees;
    kept.derived.retain(|p, d| {
        plan.contains(p)
            && d.from
                .iter()
                .all(|(q, t, _)| trees.get(q).is_some_and(|k| Arc::ptr_eq(k, t)))
    });
}

/// `path`'s enclosing planned roots with a kept walk, outermost first ([`Enclosing`]).
fn enclosing_walks(
    path: &Path,
    plan: &[PathBuf],
    kept: &Kept,
    records: &BTreeMap<PathBuf, Arc<RootRecord>>,
    fold: bool,
) -> Enclosing {
    let mut out: Enclosing = plan
        .iter()
        .filter(|q| q.as_path() != path && within(path, q, fold) && !within(q, path, fold))
        .filter_map(|q| {
            let tree = kept.trees.get(q)?;
            let at = records
                .get(q)
                .and_then(|r| r.complete.as_ref())
                .map(|c| c.at)
                .or_else(|| kept.history.get(q).and_then(|h| h.back()).map(|&(t, _)| t));
            Some((q.clone(), Arc::clone(tree), at))
        })
        .collect();
    out.sort_by_key(|(q, _, _)| q.components().count());
    out
}

/// `path`'s figure read from the outermost enclosing planned root's walk that holds it
/// ([`enclosing_walks`]), so every root inside one walk reads that one walk.
fn derive_nested(path: &Path, from: &Enclosing, fold: bool) -> Option<RootRecord> {
    for (parent, tree, at) in from {
        let Some(rel) = scopes::rel_in(path, parent, fold) else {
            continue;
        };
        let Some(id) = tree.find_folded(&rel, fold) else {
            continue;
        };
        let Some(rec) = tree.get(id) else {
            continue;
        };
        if rec.refused.is_some() || rec.mount_boundary {
            continue;
        }
        let at = at.unwrap_or_else(Instant::now);
        return Some(RootRecord {
            path: path.to_path_buf(),
            state: RootState::Complete,
            id: None,
            partial: None,
            walked_entries: 0,
            complete: Some(complete_of(tree, id, at, Duration::ZERO, None)),
            level: Vec::new(),
            read_from: Some(parent.clone()),
            asked: Some(at),
        });
    }
    None
}

fn complete_of(
    tree: &Tree,
    within: DirId,
    at: Instant,
    took: Duration,
    growth: Option<Growth>,
) -> Complete {
    let row = |id: DirId| {
        let r = tree.get(id);
        let rel = tree.rel_path(id);
        let base = tree.rel_path(within);
        DirRow {
            rel: rel
                .strip_prefix(&base)
                .map(Path::to_path_buf)
                .unwrap_or(rel),
            alloc: r.map_or(0, |r| r.subtree.alloc_fair),
            logical: r.map_or(0, |r| r.subtree.logical),
            entries: r.map_or(0, |r| r.subtree.files + r.subtree.dirs),
            direct_entries: r.map_or(0, |r| r.direct_entries),
        }
    };
    let base = tree.path(within);
    let sums = tree.get(within).map(|r| r.subtree).unwrap_or_default();
    Complete {
        detail: if sums.private.is_some() {
            Detail::Frees
        } else {
            Detail::Sizes
        },
        sums,
        links: None,
        at,
        took,
        direct_entries_max: tree.direct_entries_max_in(within),
        biggest: tree
            .biggest_in(within, TOP_DIRS)
            .into_iter()
            .map(row)
            .collect(),
        most_entries: tree
            .most_entries_in(within, TOP_DIRS)
            .into_iter()
            .map(row)
            .collect(),
        huge: tree.huge_dirs_in(within),
        not_measured: tree
            .refused_in(within)
            .into_iter()
            .take(NOT_MEASURED_ROWS)
            .map(|(id, why)| {
                let p = tree.path(id);
                (
                    p.strip_prefix(&base).map(Path::to_path_buf).unwrap_or(p),
                    why,
                )
            })
            .collect(),
        growth,
    }
}

fn growth_of(history: &VecDeque<(Instant, u64)>, now: Instant) -> Option<Growth> {
    let &(_, latest) = history.back()?;
    let base = history
        .iter()
        .rev()
        .find(|&&(t, _)| now.saturating_duration_since(t) >= GROWTH_WINDOW)
        .or_else(|| history.front())?;
    let over = now.saturating_duration_since(base.0);
    if over.is_zero() {
        return None;
    }
    let bytes =
        i64::try_from(latest).unwrap_or(i64::MAX) - i64::try_from(base.1).unwrap_or(i64::MAX);
    Some(Growth { bytes, over })
}

/// A checkout's build folders the slow tier counts: `W/target` (a link read, never followed)
/// and its build cache directory.
fn checkout_build_dirs(w: &Path, policy: &Policy, home: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match marker_at(w, "target", policy) {
        Ok(Some(Marker::Dir)) => out.extend(canonical(&w.join("target"))),
        Ok(Some(Marker::Link)) => {
            if let Ok(Some(to)) = link_target(w, "target", policy) {
                out.extend(canonical(&to));
            }
        }
        _ => {}
    }
    let cache = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(home), w);
    if let (Some(parent), Some(leaf)) = (cache.parent(), cache.file_name().and_then(|n| n.to_str()))
        && matches!(marker_at(parent, leaf, policy), Ok(Some(Marker::Dir)))
    {
        out.extend(canonical(&cache));
    }
    out
}

fn outside_build_dirs(home: &Path, policy: &Policy) -> Vec<PathBuf> {
    let Ok(rows) = list_one_level(&RootSpec::new(home), policy) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for row in rows
        .iter()
        .filter(|r| r.dir && r.refused.is_none() && !r.mount_boundary)
        .take(MAX_CHECKOUTS)
    {
        out.extend(checkout_build_dirs(&home.join(&row.name), policy, home));
    }
    out.sort();
    out.dedup();
    out
}

fn profile_deps(target: &Path, policy: &Policy) -> Vec<(PathBuf, u32)> {
    let dirs_of = |dir: &Path| -> Vec<PathBuf> {
        list_one_level(&RootSpec::new(dir), policy)
            .map(|rows| {
                rows.iter()
                    .filter(|r| r.dir && r.refused.is_none() && !r.mount_boundary)
                    .map(|r| dir.join(&r.name))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for profile in dirs_of(target) {
        let deps = profile.join("deps");
        if let Ok(n) = aterm_diskwalk::entry_count_of(&deps, policy) {
            out.push((deps, n));
            continue;
        }
        let triple = profile
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains('-'));
        if !triple {
            continue;
        }
        for nested in dirs_of(&profile) {
            let deps = nested.join("deps");
            if let Ok(n) = aterm_diskwalk::entry_count_of(&deps, policy) {
                out.push((deps, n));
            }
        }
    }
    out
}

fn profile_executables(deps: &Path, policy: &Policy, budget: Duration) -> Option<bool> {
    let mut walker = Walker::new();
    let ticket = walker.start(&RootSpec::new(deps), policy).ok()?;
    let started = Instant::now();
    let mut clock = Instant::now;
    loop {
        let sliced = walker.slice(ticket, &mut clock);
        if walker
            .walk()
            .is_some_and(|w| w.tree().root().direct_exec > 0)
        {
            return Some(true);
        }
        match sliced {
            Sliced::Step(Step::Done) => return Some(false),
            Sliced::Step(Step::Cancelled) | Sliced::Stale => return None,
            Sliced::Step(Step::More { .. })
                if Instant::now().saturating_duration_since(started) >= budget =>
            {
                return None;
            }
            Sliced::Step(Step::More { .. }) => {}
        }
    }
}

/// The digest entry for one raised folder (`vitals.huge-dir.<hash>.entries`).
pub(crate) fn huge_entry(h: &HugeDir) -> aterm_messages::exceptions::Entry {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Resurface, Then, Why};
    let subject = format!(
        "{:016x}",
        aterm_hash::fnv1a_64(h.path.as_os_str().as_encoded_bytes())
    );
    let (reason, step) = if h.executables {
        (
            "a binary launched from it makes tccd read the whole folder",
            "Remove old build outputs in it with targo clean",
        )
    } else {
        (
            "anything that lists it is slow",
            "Move the files it no longer needs out of it",
        )
    };
    Entry::new(
        Family::HugeDir,
        &subject,
        "entries",
        &format!("{} entries in one folder", group(h.entries)),
        Why::OnlyYou(Reason::new(reason)),
        Then::step(step),
    )
    .severity(aterm_messages::Severity::Warn)
    .resurface(Resurface::WAIT)
    .more(h.sentence())
    .log_line(format!(
        "vitals huge-dir path={} entries={} executables={}",
        h.path.display(),
        h.entries,
        h.executables
    ))
}

fn fold_outside(
    raised: &BTreeMap<PathBuf, HugeDir>,
    outside: &BTreeSet<PathBuf>,
    counted: &BTreeMap<PathBuf, Option<HugeDir>>,
    covered: &[PathBuf],
) -> (BTreeMap<PathBuf, HugeDir>, BTreeSet<PathBuf>) {
    let held = |p: &Path| covered.iter().any(|r| p.starts_with(r));
    let mut next: BTreeMap<PathBuf, HugeDir> = raised
        .iter()
        .filter(|(p, _)| !outside.contains(*p) || counted.contains_key(*p) || held(p))
        .map(|(p, h)| (p.clone(), h.clone()))
        .collect();
    for (p, row) in counted {
        let found: BTreeMap<PathBuf, HugeDir> =
            row.iter().map(|h| (p.clone(), h.clone())).collect();
        next = fold_raised(&next, p, &found);
    }
    let mine = counted
        .keys()
        .filter(|p| next.contains_key(*p))
        .cloned()
        .collect();
    (next, mine)
}

fn fold_raised(
    raised: &BTreeMap<PathBuf, HugeDir>,
    root: &Path,
    found: &BTreeMap<PathBuf, HugeDir>,
) -> BTreeMap<PathBuf, HugeDir> {
    let mut out: BTreeMap<PathBuf, HugeDir> = raised
        .iter()
        .filter(|(p, _)| !p.starts_with(root))
        .map(|(p, h)| (p.clone(), h.clone()))
        .collect();
    for (p, h) in found {
        let was = raised.contains_key(p);
        if h.entries >= HUGE_WARN || (was && h.entries >= HUGE_NOTE) {
            out.insert(p.clone(), h.clone());
        }
    }
    out
}

fn group(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What the loop keeps: the walker's publications, when it next looks at the panes' facts and
/// what it last sent, what the deck last asked, and the §10.8 set it last stated.
#[derive(Default)]
pub(crate) struct DiskwalkHost {
    from_walker: Option<Arc<Mailbox>>,
    proxy_installed: bool,
    started: bool,
    facts_due: Option<Instant>,
    /// Something that can move a pane's facts (input, output, a desk change) came after the
    /// last read: the loop owes a read at `facts_due` while a scope is demanded.
    facts_owed: bool,
    sent_facts: Option<Facts>,
    huge_seen: u64,
    /// When the deck last asked, and what.
    deck_asked: Option<(Instant, Vec<(DiskScope, Pace)>)>,
    /// Each pane's last OSC 7 report read, kept while its terminal is busy.
    reports: HashMap<u64, Option<CwdReport>>,
}

impl DiskwalkHost {
    /// Something that was not a timer woke the loop: a pane's front program, its OSC 7 report
    /// or the tabs may have moved, so a read of the facts is owed ([`Self::facts_deadline`]).
    pub(crate) const fn note_activity(&mut self) {
        self.facts_owed = true;
    }

    /// Whether this service reads the panes' facts (asked by a fresh demand, or due), and when
    /// it does, the next read's time: [`FACTS_EVERY`] while a scope is `demanded`, else
    /// [`FACTS_IDLE_EVERY`]. A read pays what was owed.
    pub(crate) fn facts_now(&mut self, now: Instant, wanted: bool, demanded: bool) -> bool {
        if !wanted && self.facts_due.is_some_and(|due| now < due) {
            return false;
        }
        let every = if demanded {
            FACTS_EVERY
        } else {
            FACTS_IDLE_EVERY
        };
        self.facts_due = Some(now + every);
        self.facts_owed = false;
        true
    }

    /// The loop's deadline for an owed read while a scope is `demanded`: the walker no longer
    /// wakes the loop after each discovery, so a change heard between reads (a `cd`'s OSC 7, a
    /// program brought to the front) reaches the walker at `facts_due`, not at whatever wake
    /// comes next. Nothing owed, nothing demanded: no deadline, so a window at rest wakes for
    /// none.
    pub(crate) fn facts_deadline(&self, demanded: bool) -> Option<Instant> {
        if self.facts_owed && demanded {
            self.facts_due
        } else {
            None
        }
    }
}

/// A pane's OSC 7 report, or `Err` while its terminal is locked.
fn cwd_report(term: &Mutex<aterm_core::terminal::Terminal>) -> Result<Option<CwdReport>, ()> {
    use crate::cwd_native::ReportedCwd as _;
    let read = |t: &aterm_core::terminal::Terminal| {
        let path = t
            .native_working_directory()
            .map(|p| PathBuf::from(p.into_owned()))
            .filter(|p| p.is_absolute())?;
        let here = t.reported_cwd_here();
        Some(CwdReport {
            path,
            here,
            host: (!here)
                .then(|| t.reported_cwd_host().map(str::to_owned))
                .flatten(),
        })
    };
    match term.try_lock() {
        Ok(t) => Ok(read(&t)),
        Err(std::sync::TryLockError::Poisoned(p)) => Ok(read(&p.into_inner())),
        Err(std::sync::TryLockError::WouldBlock) => Err(()),
    }
}

impl crate::App {
    /// The board's answer for `scope` (the System panel's read), demanding it at look pace.
    #[cfg_attr(
        test,
        expect(
            dead_code,
            reason = "tests read their own board (`Shared::new`); the process's is the readers'"
        )
    )]
    pub(crate) fn disk_roots(&self, scope: DiskScope, detail: Detail) -> DiskReport {
        let (_, mut reports) = disk_reports(&[scope], detail, Pace::Look, Instant::now());
        reports.pop().unwrap_or_else(|| DISKWALK.report(scope))
    }

    /// A level judged every wake.
    pub(crate) fn service_diskwalk(&mut self, now: Instant) {
        if !self.diskwalk.proxy_installed
            && let Some(proxy) = self.proxy.clone()
        {
            self.diskwalk.proxy_installed = true;
            *DISKWALK.proxy.lock_or_recover() = Some(proxy);
        }
        if !self.diskwalk.started
            && (!self.headless || DISKWALK.demanded(now))
            && let Some(proxy) = self.proxy.clone()
        {
            self.diskwalk.started = true;
            match spawn_walker(&DISKWALK, Some(proxy), WALKER_CLASS) {
                Ok(rx) => self.diskwalk.from_walker = Some(rx),
                Err(error) => aterm_log::warn!("disk walker: the thread did not start: {error}"),
            }
        }
        let Some(rx) = self.diskwalk.from_walker.as_ref() else {
            return;
        };
        if let Some(p) = DISKWALK.absorb(rx)
            && p.huge_seq != self.diskwalk.huge_seen
        {
            self.diskwalk.huge_seen = p.huge_seq;
            let entries: Vec<_> = p.huge.values().map(huge_entry).collect();
            self.except_replace(aterm_messages::exceptions::Family::HugeDir, &entries);
        }
        self.ask_for_deck(now);
        let wanted = DISKWALK.facts_wanted.swap(false, Ordering::Relaxed);
        if self.diskwalk.facts_now(now, wanted, DISKWALK.demanded(now)) {
            let facts = self.diskwalk_facts();
            if self.diskwalk.sent_facts.as_ref() != Some(&facts) {
                DISKWALK.send(ToWalker::Facts(facts.clone()));
                self.diskwalk.sent_facts = Some(facts);
            }
        }
    }

    /// The loop's deadline for the panes' facts ([`DiskwalkHost::facts_deadline`]).
    pub(crate) fn diskwalk_deadline(&self, now: Instant) -> Option<Instant> {
        self.diskwalk.facts_deadline(DISKWALK.demanded(now))
    }

    /// `Wake::DiskWalk`: the walker published, or a reader asked a scope anew.
    pub(crate) fn on_disk_walk_wake(&mut self) {
        self.service_diskwalk(Instant::now());
    }

    /// The deck's ask ([`deck_asks`]): each window drawing it in view keeps its front tab and
    /// aterm demanded, again every [`DECK_ASK_EVERY`] and at once when what it asks changes.
    fn ask_for_deck(&mut self, now: Instant) {
        let windows: Vec<DeckWindow> = self
            .windows
            .values()
            .map(|ws| DeckWindow {
                drawn: crate::stats_deck::stats_drawn_on(ws),
                occluded: ws.occluded,
                front: ws.tab_set.active().map(|t| t.id),
                disk_lens: ws
                    .stats_lens
                    .open
                    .is_some_and(|l| l.focus == crate::stats_paint::Instrument::Disk),
            })
            .collect();
        let asks = deck_asks(self.stats_bars_form().shown(), &windows);
        let due = match &self.diskwalk.deck_asked {
            Some((at, was)) => *was != asks || now.saturating_duration_since(*at) >= DECK_ASK_EVERY,
            None => !asks.is_empty(),
        };
        if !due {
            return;
        }
        for &(scope, pace) in &asks {
            DISKWALK.demand(scope, Detail::Sizes, pace, now);
        }
        self.diskwalk.deck_asked = Some((now, asks));
    }

    /// Every terminal pane of every tab: its shell, the program in front (`tcgetpgrp`), its
    /// newest OSC 7 report and where that was made; and the tabs that hold them.
    fn diskwalk_facts(&mut self) -> Facts {
        let mut tabs = Vec::new();
        let mut panes = Vec::new();
        let mut read: Vec<(u64, Option<CwdReport>)> = Vec::new();
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        for (wid, ws) in &self.windows {
            for (i, tab) in ws.tab_set.tabs().iter().enumerate() {
                let mut mine = Vec::new();
                for key in self.tab_layout_sessions(tab) {
                    let Some(s) = self.pool.get(key) else {
                        continue;
                    };
                    let Some(shell) = u32::try_from(s.pid).ok().filter(|p| *p > 0) else {
                        continue;
                    };
                    mine.push(key);
                    if !seen.insert(key) {
                        continue;
                    }
                    let front =
                        u32::try_from(crate::quit_safety::foreground_pgrp(s.ctx.sink.master()))
                            .ok()
                            .filter(|g| *g > 0)
                            .unwrap_or(shell);
                    let report = match cwd_report(&s.term) {
                        Ok(r) => {
                            read.push((key, r.clone()));
                            r
                        }
                        Err(()) => self.diskwalk.reports.get(&key).cloned().flatten(),
                    };
                    panes.push(PaneFacts {
                        key,
                        session: s.ctx.self_id.as_str().to_owned(),
                        shell_pid: shell,
                        front_pid: front,
                        report,
                    });
                }
                if mine.is_empty() {
                    continue;
                }
                tabs.push(TabFacts {
                    id: tab.id,
                    label: self.tab_name(tab, i),
                    number: (!tab.parked).then_some(i + 1),
                    window: wid.0,
                    panes: mine,
                });
            }
        }
        self.diskwalk.reports.extend(read);
        self.diskwalk.reports.retain(|k, _| seen.contains(k));
        let home = aterm_types::dirs::home_dir();
        let own = own_paths(
            home.as_deref(),
            aterm_types::dirs::state_dir(),
            cfg!(target_os = "macos"),
        );
        Facts {
            panes,
            tabs,
            opt_in: self.config.sys_disk_roots(home.as_deref()),
            home,
            overrides: self
                .config
                .privacy
                .as_ref()
                .and_then(|p| p.protected_roots.clone())
                .unwrap_or_default(),
            own,
        }
    }
}

/// The board one discovery at `now` derives from `facts` with `reads`, each of `scopes`
/// demanded at look pace: an engine with no thread, which walks nothing. Another module's
/// tests read what a discovery names through it (`sys disk`'s).
#[cfg(all(test, unix))]
pub(crate) fn discovered_board(
    facts: Facts,
    reads: PaneReads,
    scopes: &[DiskScope],
    now: Instant,
) -> Published {
    let (_to, inbox) = std::sync::mpsc::channel();
    let mut engine = Engine::new(inbox, Arc::default(), None);
    engine.reads = reads;
    engine.take_facts(facts);
    for &scope in scopes {
        engine.apply(ToWalker::Demand {
            scope,
            detail: Detail::Sizes,
            pace: Pace::Look,
            at: now,
        });
    }
    engine.discover_if_due(now);
    engine.published()
}

/// Unix only: the tests build scratch trees with links and modes, and on any
/// other platform every walk is `Refusal::Unsupported`.
#[cfg(all(test, unix))]
#[path = "diskwalk_host_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "diskwalk_host/freshness_tests.rs"]
mod freshness_tests;

/// Every platform: the mailbox's tests walk no disk.
#[cfg(test)]
#[path = "diskwalk_host/mailbox_tests.rs"]
mod mailbox_tests;
