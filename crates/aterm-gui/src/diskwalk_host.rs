// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK WALKER HOST (design `docs/DESIGN-system-awareness-2026-10-16.md` §10.4–§10.8, phase P2d): one walker thread at Housekeeping QoS that sizes the folders a scope uses and every subtree in them, counts the build folders' entries for the §10.8 hazard, and hands what it found to readers by message.

#![cfg_attr(
    not(all(test, unix)),
    expect(
        dead_code,
        reason = "`sys disk` and `sys tab` read `disk_report` (`sys_host`); the parts \
                  only the other readers use (the System panel, the detail levels past \
                  sizes) land in their own streams of design P2d (and the tests, the other \
                  readers, are unix-only)"
    )
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_types::MutexExt as _;

use aterm_diskwalk::{
    Deny, DirId, HUGE_NOTE, HUGE_WARN, HugeDir, LevelRow, LinkTotals, Marker, Policy, Refusal,
    RootSpec, Sliced, Step, Sums, Ticket, Tree, Walker, build_dir_above, canonical, link_target,
    list_one_level, marker_at, worktree_above,
};

/// How long one read keeps its scope's roots demanded.
pub(crate) const DEMAND_TTL: Duration = Duration::from_secs(60);
/// A root no discovery has named for this long is retired (design §10.4 "lifetime"); the walker counts only discoveries it ran, so roots stay put while nothing is demanded.
pub(crate) const RETIRE_AFTER: Duration = Duration::from_secs(60);
/// Consecutive discoveries that must name a root before it is walked.
pub(crate) const PROMOTE_AFTER: u32 = 2;
/// The shortest age at which a complete walk is walked again while demanded: P2d has no change feed, so freshness is a re-walk.
pub(crate) const REWALK_AFTER: Duration = Duration::from_secs(30);
/// A walk is walked again no sooner than this many times its own duration after it ended: at most a 5 % duty on its root (design §10.6's re-list rate).
pub(crate) const REWALK_COST_FACTOR: u32 = 20;
/// How often the thread derives each demanded pane's roots again.
pub(crate) const DISCOVER_EVERY: Duration = Duration::from_secs(5);
/// The growth window design T3 reads: a tab's roots grew ≥ 1 GiB in ten minutes.
pub(crate) const GROWTH_WINDOW: Duration = Duration::from_secs(600);
/// Subtree rows kept per root, per view ("biggest", "most entries").
pub(crate) const TOP_DIRS: usize = 8;
/// Folders inside a root that were not measured, named per root at most.
const NOT_MEASURED_ROWS: usize = 16;
/// A walk in progress publishes its partial figure at most this often, and
/// reads its inbox between.
const PUBLISH_EVERY: Duration = Duration::from_millis(500);
/// The most descendants of one shell whose cwd is read in one discovery.
const MAX_DESCENDANTS: usize = 256;
/// The most write-open files of one process anchored in one discovery (a
/// database or a build holds dozens; the anchors repeat).
const MAX_WRITES_PER_PROCESS: usize = 64;
/// How often the slow tier counts the entries of the build profiles outside every root (design §10.8): one `ATTR_DIR_ENTRYCOUNT` read each.
pub(crate) const PROFILES_EVERY: Duration = Duration::from_secs(600);
/// The home folder's children the slow tier looks in for a build folder.
const MAX_CHECKOUTS: usize = 512;
/// A profile's `deps/` at the note line is walked this long at most, to learn
/// whether it holds executables (the tccd hazard's sentence)…
const PROFILE_WALK_BUDGET: Duration = Duration::from_secs(5);
/// …and a known answer is kept this long (a walk the budget cut before it met
/// one knows nothing, so it is asked again at the next tier).
const EXEC_KNOWN_FOR: Duration = Duration::from_secs(3600);
/// How often the loop looks at the panes' facts while a scope is demanded…
pub(crate) const FACTS_EVERY: Duration = Duration::from_secs(1);
/// …and while none is (the slow tier needs the home and the deny set).
pub(crate) const FACTS_IDLE_EVERY: Duration = Duration::from_secs(10);

/// Which roots a reader asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DiskScope {
    /// One pane: its session key (the `key` the `sys` host's pane facts carry).
    Tab(u64),
    /// Every pane's roots and aterm's own folders.
    Aterm,
}

/// How much a reader wants measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Detail {
    /// Allocated and logical sizes, entry counts, the biggest subtrees.
    #[default]
    Sizes,
    /// Also each file's private (unshared-clone) size and the hard-link totals: what deleting a root frees at least (`sums.private`) and at most (`links.frees_at_most`).
    Frees,
}

/// Why a folder is a root (design §10.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RootKind {
    /// The shell's cwd (OSC 7, else the shell's process cwd).
    Cwd,
    /// A direct child of the shell's cwd: the foreground or agent leader.
    Agent,
    /// A deeper descendant's cwd.
    Work,
    /// Where a descendant is writing now: the anchor of a regular file it holds open for writing.
    Writes,
    /// The git worktree above a cwd.
    Repo,
    /// A worktree's build output: `target` (a link resolved explicitly), atpkg's build cache for it, and a descendant's `CARGO_TARGET_DIR`.
    Build,
    /// An agent session's temp folder.
    Scratch,
    /// aterm's own folders and the `[sys] disk_roots` opt-ins (the aterm scope only).
    Own,
}

impl RootKind {
    /// The word `sys disk` prints (`kinds=cwd,repo`).
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Cwd => "cwd",
            Self::Agent => "agent",
            Self::Work => "work",
            Self::Writes => "writes",
            Self::Repo => "repo",
            Self::Build => "build",
            Self::Scratch => "scratch",
            Self::Own => "own",
        }
    }
}

/// One pane as the loop last saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabFacts {
    /// Its session key (the [`DiskScope::Tab`] that names it).
    pub(crate) key: u64,
    /// `Tab 2`.
    pub(crate) label: String,
    /// Its shell.
    pub(crate) shell_pid: u32,
    /// The shell-integration (OSC 7) cwd, when the shell reported one.
    pub(crate) reported_cwd: Option<PathBuf>,
}

/// What the loop publishes: the panes, and what the path test needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    /// Every pane with a live shell.
    pub(crate) tabs: Vec<TabFacts>,
    /// `$HOME`.
    pub(crate) home: Option<PathBuf>,
    /// `[privacy] protected_roots`, as written (the walker's deny set adds them to `consent::tcc_prompting_roots`).
    pub(crate) overrides: Vec<String>,
    /// `[sys] disk_roots`, absolute: folders the person lets the walker size in `~/Library` or a protected folder (design O20).
    pub(crate) opt_in: Vec<PathBuf>,
    /// aterm's own folders (`state_dir()`, and on macOS `~/Library/Caches/aterm`).
    pub(crate) own: Vec<PathBuf>,
}

/// One folder a scope claims, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claim {
    /// The canonical path.
    pub(crate) path: PathBuf,
    /// Why it is a root.
    pub(crate) kinds: BTreeSet<RootKind>,
    /// A linked git worktree (its `.git` is a file): its object store is the main checkout's, counted there.
    pub(crate) linked_worktree: bool,
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
    /// The word `sys disk` prints: a refusal's own word, so one fact has one spelling (`too-broad` here and as a nested row's reason).
    pub(crate) fn word(self) -> &'static str {
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
    /// When it is owed another walk: [`REWALK_AFTER`] after it ended, or [`REWALK_COST_FACTOR`] times its own duration, whichever is later.
    pub(crate) fn rewalk_at(&self) -> Instant {
        self.at + REWALK_AFTER.max(self.took.saturating_mul(REWALK_COST_FACTOR))
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
    /// The last complete walk.
    pub(crate) complete: Option<Complete>,
    /// A too-broad root's one-level listing.
    pub(crate) level: Vec<LevelRow>,
    /// The root whose walk these figures were read from, when it lies inside another root.
    pub(crate) read_from: Option<PathBuf>,
    /// When the path was last asked: a walk began, or a refusal or a one-level listing came back.
    pub(crate) asked: Option<Instant>,
}

impl RootRecord {
    fn queued(path: PathBuf) -> Self {
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

    /// The bytes it stands for: the last complete walk's, else what the running walk has summed.
    pub(crate) fn alloc(&self) -> u64 {
        self.complete
            .as_ref()
            .map(|c| c.sums.alloc_fair)
            .or(self.partial.map(|p| p.alloc_fair))
            .unwrap_or(0)
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
    /// The other panes (session keys) that claim it, a folder above it or a folder in it.
    pub(crate) shared_with: Vec<u64>,
    /// The pane whose total counts it whole: the lowest key claiming it or a folder above it.
    pub(crate) counted_in: Option<u64>,
    /// The bytes of it that two or more panes claim (an outermost row's; 0 on a pinned child).
    pub(crate) shared_alloc: u64,
    /// What the board holds for it.
    pub(crate) record: RootRecord,
}

impl RootRow {
    /// The bytes the row stands for: the last complete walk's, else what the running walk has summed.
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
    /// The pane's label (tab scope).
    pub(crate) label: Option<String>,
    /// Its roots, outermost first by size, each pinned child after its root.
    pub(crate) roots: Vec<RootRow>,
    /// Bytes of the outermost rows, each folder once.
    pub(crate) total_bytes: u64,
    /// Bytes of them no other pane claims.
    pub(crate) exclusive_bytes: u64,
    /// Bytes of them two or more panes claim.
    pub(crate) shared_bytes: u64,
    /// A walk is queued or running for one of its roots.
    pub(crate) walking: bool,
    /// The §10.8 rows across its roots (and, for the aterm scope, the build profiles outside every root), most entries first.
    pub(crate) warnings: Vec<HugeDir>,
    /// Σ growth of the outermost complete roots.
    pub(crate) growth: Option<Growth>,
    /// The pane is unknown (closed, or the loop has not published it yet).
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
        at: Instant,
    },
    /// End the thread (tests: the process's walker lives as long as it).
    #[cfg(test)]
    Stop,
}

/// The walker's whole answer, sent after each change.
#[derive(Debug, Default)]
pub(crate) struct Published {
    /// `(session key, label)` of every pane the facts named.
    pub(crate) tabs: Vec<(u64, String)>,
    /// Pane key → its claims.
    pub(crate) tab_claims: BTreeMap<u64, Vec<Claim>>,
    /// aterm's own claims (and the opt-ins).
    pub(crate) own_claims: Vec<Claim>,
    /// Every root's record.
    pub(crate) records: BTreeMap<PathBuf, Arc<RootRecord>>,
    /// The raised §10.8 entries, with their hysteresis.
    pub(crate) huge: BTreeMap<PathBuf, HugeDir>,
    /// Which of them the slow tier raised: build profiles outside every root.
    pub(crate) outside: BTreeSet<PathBuf>,
    /// Moves whenever `huge` does.
    pub(crate) huge_seq: u64,
}

type Proxy = aterm_window::event_loop::EventLoopProxy<crate::Wake>;

/// The monotonic origin of [`Shared::demanded_until_ms`].
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
    /// When each scope was last asked (readers only): a scope asked anew
    /// makes the loop send fresh facts.
    asked: Mutex<BTreeMap<DiskScope, Instant>>,
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

    /// Send the walker `msg`; nothing when it is not running.
    fn send(&self, msg: ToWalker) {
        if let Some(tx) = self.inbox.get() {
            let _ = tx.send(msg);
        }
    }

    /// Demand `scope` now: the walker hears it, and a scope asked anew asks the loop for the panes' current facts.
    pub(crate) fn demand(&self, scope: DiskScope, detail: Detail, now: Instant) {
        let fresh = {
            let mut asked = self.asked.lock_or_recover();
            let fresh = asked
                .get(&scope)
                .is_none_or(|at| now.saturating_duration_since(*at) >= DEMAND_TTL);
            asked.insert(scope, now);
            fresh
        };
        self.demanded_until_ms
            .fetch_max(anchor_ms(now + DEMAND_TTL), Ordering::Relaxed);
        self.send(ToWalker::Demand {
            scope,
            detail,
            at: now,
        });
        if fresh {
            self.facts_wanted.store(true, Ordering::Relaxed);
            let proxy = self.proxy.lock_or_recover().clone();
            if let Some(proxy) = proxy {
                // The loop answers with the panes' facts; a closed loop has none.
                let _ = proxy.send_event(crate::Wake::DiskWalk);
            }
        }
    }

    /// The report for `scope` from the newest publication.
    pub(crate) fn report(&self, scope: DiskScope) -> DiskReport {
        let published = self.board.lock_or_recover().clone().unwrap_or_default();
        build_report(scope, &published, self.walker_state())
    }

    /// The loop's (or a test's) take of what the walker sent: the newest publication goes on the board and is returned.
    pub(crate) fn absorb(&self, from: &Receiver<Arc<Published>>) -> Option<Arc<Published>> {
        let newest = from.try_iter().last()?;
        *self.board.lock_or_recover() = Some(Arc::clone(&newest));
        Some(newest)
    }
}

/// The board's answer for `scope` now, demanding it: `sys disk`, `sys tab` and the System panel read this, from any thread (a control worker or the loop).
pub(crate) fn disk_report(scope: DiskScope, detail: Detail, now: Instant) -> DiskReport {
    DISKWALK.demand(scope, detail, now);
    DISKWALK.report(scope)
}

/// Start the walker thread for `shared`: the receiver is the loop's end of
/// its publications, and `proxy` wakes the loop after each.
fn spawn_walker(
    shared: &'static Shared,
    proxy: Option<Proxy>,
) -> std::io::Result<Receiver<Arc<Published>>> {
    let (inbox_tx, inbox) = std::sync::mpsc::channel();
    let (out, from_walker) = std::sync::mpsc::channel();
    if shared.inbox.set(inbox_tx).is_err() {
        return Err(std::io::Error::other("the disk walker already runs"));
    }
    match crate::qos::spawn_lane_thread(
        "aterm-diskwalk",
        crate::qos::Role::Housekeeping,
        move || walker_main(inbox, out, proxy),
    ) {
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

/// The bytes of `path` (a claimed folder) that two or more panes claim, given
/// `above`, the panes that claim it whole (it or a folder above it).
fn shared_in(
    path: &Path,
    above: &BTreeSet<u64>,
    claimers: &BTreeMap<PathBuf, BTreeSet<u64>>,
    alloc_of: &dyn Fn(&Path) -> u64,
) -> u64 {
    if above.len() >= 2 {
        return alloc_of(path);
    }
    let inner: Vec<&PathBuf> = claimers
        .keys()
        .filter(|k| k.as_path() != path && k.starts_with(path))
        .collect();
    inner
        .iter()
        .filter(|k| {
            !inner
                .iter()
                .any(|q| q.as_path() != k.as_path() && k.starts_with(q))
        })
        .map(|k| {
            let mut with = above.clone();
            with.extend(claimers.get(*k).into_iter().flatten().copied());
            shared_in(k, &with, claimers, alloc_of)
        })
        .sum()
}

fn build_report(scope: DiskScope, p: &Published, walker: WalkerState) -> DiskReport {
    let label = match scope {
        DiskScope::Tab(key) => p
            .tabs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, l)| l.clone()),
        DiskScope::Aterm => None,
    };
    let unknown_tab = matches!(scope, DiskScope::Tab(_)) && label.is_none();
    let mut by_id: BTreeMap<(u64, u64), PathBuf> = BTreeMap::new();
    let mut key_of: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    let mut all_paths: Vec<&PathBuf> = p
        .tab_claims
        .values()
        .flatten()
        .chain(&p.own_claims)
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
    let mut claimers: BTreeMap<PathBuf, BTreeSet<u64>> = BTreeMap::new();
    for (pane, claims) in &p.tab_claims {
        for c in claims {
            claimers.entry(key(&c.path)).or_default().insert(*pane);
        }
    }
    let alloc_of = |k: &Path| p.records.get(k).map_or(0, |r| r.alloc());
    let mut scope_claims: Vec<&Claim> = match scope {
        DiskScope::Tab(k) => p.tab_claims.get(&k).into_iter().flatten().collect(),
        DiskScope::Aterm => p
            .tab_claims
            .values()
            .flatten()
            .chain(&p.own_claims)
            .collect(),
    };
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
                record,
            },
        ));
    }
    let me = match scope {
        DiskScope::Tab(k) => Some(k),
        DiskScope::Aterm => None,
    };
    for (k, row) in &mut rows {
        let whole: BTreeSet<u64> = claimers
            .iter()
            .filter(|(q, _)| k.starts_with(q))
            .flat_map(|(_, panes)| panes.iter().copied())
            .collect();
        let overlap: BTreeSet<u64> = claimers
            .iter()
            .filter(|(q, _)| k.starts_with(q) || q.starts_with(&*k))
            .flat_map(|(_, panes)| panes.iter().copied())
            .collect();
        row.counted_in = whole.iter().copied().min();
        row.shared_with = match me {
            Some(me) => overlap.iter().copied().filter(|o| *o != me).collect(),
            None if overlap.len() >= 2 => overlap.into_iter().collect(),
            None => Vec::new(),
        };
        row.shared_alloc = shared_in(k, &whole, &claimers, &alloc_of);
    }
    let paths: Vec<PathBuf> = rows.iter().map(|(k, _)| k.clone()).collect();
    let parent_of: Vec<Option<usize>> = paths
        .iter()
        .map(|p| {
            paths
                .iter()
                .enumerate()
                .filter(|(_, q)| *q != p && p.starts_with(q))
                .max_by_key(|(_, q)| q.components().count())
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
    let mut total_bytes = 0u64;
    let mut shared_bytes = 0u64;
    let mut walking = false;
    let mut warnings: Vec<HugeDir> = Vec::new();
    let mut growth: Option<Growth> = None;
    for row in &out {
        walking |= matches!(row.record.state, RootState::Queued | RootState::Walking);
        if let Some(c) = &row.record.complete {
            for h in &c.huge {
                if !warnings.iter().any(|w| w.path == h.path) {
                    warnings.push(h.clone());
                }
            }
        }
        if row.inside.is_some() {
            continue;
        }
        total_bytes += row.alloc();
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
    DiskReport {
        scope,
        label,
        roots: out,
        total_bytes,
        exclusive_bytes: total_bytes - shared_bytes,
        shared_bytes,
        walking,
        warnings,
        growth,
        unknown_tab,
        walker,
    }
}

/// Add `kind` to the claim on `path` (canonical), creating it.
fn claim(out: &mut Vec<Claim>, path: &Path, kind: RootKind, linked: bool) {
    let Some(path) = canonical(path) else {
        return;
    };
    if let Some(c) = out.iter_mut().find(|c| c.path == path) {
        c.kinds.insert(kind);
        c.linked_worktree |= linked;
        return;
    }
    out.push(Claim {
        path,
        kinds: BTreeSet::from([kind]),
        linked_worktree: linked,
    });
}

/// The processes below `root`, breadth first: `(pid, ppid)`, at most
/// [`MAX_DESCENDANTS`].
fn descendants(procs: &[(u32, u32)], root: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut frontier = VecDeque::from([root]);
    while let Some(parent) = frontier.pop_front() {
        for &(pid, ppid) in procs {
            if ppid == parent && pid != parent && !out.iter().any(|&(p, _)| p == pid) {
                if out.len() >= MAX_DESCENDANTS {
                    return out;
                }
                out.push((pid, ppid));
                frontier.push_back(pid);
            }
        }
    }
    out
}

/// The agent session folder a cwd under `base` (`/private/tmp/claude-<uid>`)
/// sits in: `base/<project>/<session>`.
fn scratch_anchor(cwd: &Path, base: &Path) -> Option<PathBuf> {
    let rest = cwd.strip_prefix(base).ok()?;
    let mut parts = rest.components();
    let project = parts.next()?;
    let session = parts.next()?;
    Some(base.join(project).join(session))
}

/// Where agent sessions keep their temp folders on this machine.
fn scratch_base() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        // SAFETY: `getuid` has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let tmp = if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        };
        Some(PathBuf::from(format!("{tmp}/claude-{uid}")))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// What discovery reads of a process, injected so a test names its own processes.
pub(crate) struct ProcReads<'a> {
    /// Its working directory.
    pub(crate) cwd: &'a dyn Fn(u32) -> Option<PathBuf>,
    /// `CARGO_TARGET_DIR` as its environment held it at exec, where set and readable (`targo`, `cargo`, `claude`; a platform binary hides it).
    pub(crate) target_dir: &'a dyn Fn(u32) -> Option<PathBuf>,
    /// The regular files it holds open for writing, as the kernel names them.
    pub(crate) writes: &'a dyn Fn(u32) -> Vec<PathBuf>,
}

/// The firmlinked spelling of a kernel path on macOS's data volume
/// (`/System/Volumes/Data/Users//a/x` is `/Users//a/x`), so one folder is one
/// root whichever spelling the kernel gave; any other path as it is.
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

/// Where a write lands, as a root (design §10.4 `writes`): the build folder
/// above the written file, else its git worktree, else its agent session
/// temp folder, else its folder.
fn write_anchor(
    file: &Path,
    policy: &Policy,
    scratch: Option<&Path>,
    seen: &mut BTreeMap<PathBuf, PathBuf>,
) -> Option<PathBuf> {
    let dir = canonical(&firmlinked(file))?.parent()?.to_path_buf();
    if let Some(known) = seen.get(&dir) {
        return Some(known.clone());
    }
    let anchor = build_dir_above(&dir, policy)
        .or_else(|| worktree_above(&dir, policy).map(|w| w.root))
        .or_else(|| scratch.and_then(|base| scratch_anchor(&dir, base)))
        .unwrap_or_else(|| dir.clone());
    seen.insert(dir, anchor.clone());
    Some(anchor)
}

/// A `CARGO_TARGET_DIR` as an absolute folder: a relative one is cargo's
/// reading of it, against the process's cwd.
fn target_dir_path(dir: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
    if dir.is_absolute() {
        canonical(dir)
    } else {
        cwd.and_then(|c| canonical(&c.join(dir)))
    }
}

/// Claim `path` as `kind` when it is a directory, or the directory a link
/// there names (read, never followed); nothing when it is absent or refused.
fn claim_dir(out: &mut Vec<Claim>, path: &Path, kind: RootKind, policy: &Policy) {
    let (Some(parent), Some(leaf)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    match marker_at(parent, leaf, policy) {
        Ok(Some(Marker::Dir)) => claim(out, path, kind, false),
        Ok(Some(Marker::Link)) => {
            if let Ok(Some(to)) = link_target(parent, leaf, policy) {
                claim(out, &to, kind, false);
            }
        }
        _ => {}
    }
}

/// One pane's roots (design §10.4): its cwd, its processes' cwds, where they
/// write, the git worktree above each cwd, the worktree's build output and
/// the processes' `CARGO_TARGET_DIR`, and agent session temp folders.
fn discover_tab(
    tab: &TabFacts,
    procs: &[(u32, u32)],
    reads: &ProcReads<'_>,
    policy: &Policy,
    home: Option<&Path>,
    scratch: Option<&Path>,
) -> Vec<Claim> {
    let mut out = Vec::new();
    let mut anchors: Vec<PathBuf> = Vec::new();
    let mut seen = BTreeMap::new();
    if let Some(cwd) = tab
        .reported_cwd
        .clone()
        .or_else(|| (reads.cwd)(tab.shell_pid))
        .and_then(|c| canonical(&c))
    {
        claim(&mut out, &cwd, RootKind::Cwd, false);
        anchors.push(cwd);
    }
    for (pid, ppid) in descendants(procs, tab.shell_pid) {
        let cwd = (reads.cwd)(pid).and_then(|c| canonical(&c));
        if let Some(dir) = (reads.target_dir)(pid).and_then(|t| target_dir_path(&t, cwd.as_deref()))
        {
            claim_dir(&mut out, &dir, RootKind::Build, policy);
        }
        for file in (reads.writes)(pid).iter().take(MAX_WRITES_PER_PROCESS) {
            if let Some(anchor) = write_anchor(file, policy, scratch, &mut seen) {
                claim(&mut out, &anchor, RootKind::Writes, false);
            }
        }
        let Some(cwd) = cwd else {
            continue;
        };
        let kind = if ppid == tab.shell_pid {
            RootKind::Agent
        } else {
            RootKind::Work
        };
        if let Some(anchor) = scratch.and_then(|base| scratch_anchor(&cwd, base)) {
            claim(&mut out, &anchor, RootKind::Scratch, false);
            claim(&mut out, &anchor, kind, false);
            continue;
        }
        claim(&mut out, &cwd, kind, false);
        if !anchors.contains(&cwd) {
            anchors.push(cwd);
        }
    }
    let mut worktrees: Vec<PathBuf> = Vec::new();
    for anchor in &anchors {
        let Some(wt) = worktree_above(anchor, policy) else {
            continue;
        };
        claim(&mut out, &wt.root, RootKind::Repo, wt.linked);
        if !worktrees.contains(&wt.root) {
            worktrees.push(wt.root);
        }
    }
    for w in &worktrees {
        build_roots(&mut out, w, policy, home);
    }
    out
}

/// A worktree's build output: `W/target` (a `target → target.noindex` link
/// resolved by reading it, since the walker follows no link) and atpkg's
/// build cache for `W`.
fn build_roots(out: &mut Vec<Claim>, w: &Path, policy: &Policy, home: Option<&Path>) {
    claim_dir(out, &w.join("target"), RootKind::Build, policy);
    let Some(home) = home else {
        return;
    };
    let cache = atpkg::build_cache::build_dir(&atpkg::build_cache::cache_root(home), w);
    if let (Some(parent), Some(leaf)) = (cache.parent(), cache.file_name().and_then(|n| n.to_str()))
        && matches!(marker_at(parent, leaf, policy), Ok(Some(Marker::Dir)))
    {
        claim(out, &cache, RootKind::Build, false);
    }
}

/// aterm's own folders and the `[sys] disk_roots` opt-ins, claimed for the
/// aterm scope.
fn own_claims(own: &[PathBuf], opt_in: &[PathBuf]) -> Vec<Claim> {
    let mut out = Vec::new();
    for p in own.iter().chain(opt_in) {
        claim(&mut out, p, RootKind::Own, false);
    }
    out
}

/// A root's place in the lifetime law (§10.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Life {
    /// Consecutive discoveries that named it, through the one that last did.
    pub(crate) streak: u32,
    /// The discovery that last named it.
    pub(crate) seen_in: u64,
    /// When that was.
    pub(crate) seen_at: Instant,
    /// Walked: named by [`PROMOTE_AFTER`] consecutive discoveries, or by the first discovery of a newly demanded scope.
    pub(crate) promoted: bool,
}

/// The lifetime law, pure: `life` after discovery `n`, at `now`, named `claimed`, of which `at_once` are the claims of a scope demanded anew (promoted at once).
pub(crate) fn age_roots(
    life: &mut BTreeMap<PathBuf, Life>,
    claimed: &BTreeSet<PathBuf>,
    at_once: &BTreeSet<PathBuf>,
    n: u64,
    now: Instant,
) -> Vec<PathBuf> {
    for path in claimed {
        let l = life.entry(path.clone()).or_insert(Life {
            streak: 0,
            seen_in: 0,
            seen_at: now,
            promoted: false,
        });
        l.streak = if l.seen_in + 1 == n { l.streak + 1 } else { 1 };
        l.seen_in = n;
        l.seen_at = now;
        l.promoted |= l.streak >= PROMOTE_AFTER || at_once.contains(path);
    }
    let retired: Vec<PathBuf> = life
        .iter()
        .filter(|(_, l)| now.saturating_duration_since(l.seen_at) >= RETIRE_AFTER)
        .map(|(p, _)| p.clone())
        .collect();
    for p in &retired {
        life.remove(p);
    }
    retired
}

/// What the thread keeps between walks: each root's last complete tree (a
/// nested root reads from it) and its walk history (growth).
#[derive(Default)]
struct Kept {
    trees: BTreeMap<PathBuf, Arc<Tree>>,
    history: BTreeMap<PathBuf, VecDeque<(Instant, u64)>>,
}

/// What one demanded scope asked for.
#[derive(Clone, Copy, Debug)]
struct Ask {
    at: Instant,
    frees_at: Option<Instant>,
}

/// The walk in flight.
struct Active {
    path: PathBuf,
    ticket: Ticket,
    started: Instant,
    detail: Detail,
    prior: Option<Arc<RootRecord>>,
    walked: u64,
}

/// The walker thread's whole state: it owns everything it derives.
struct Engine {
    inbox: Receiver<ToWalker>,
    out: Sender<Arc<Published>>,
    proxy: Option<Proxy>,
    facts: Facts,
    facts_seq: u64,
    demand: BTreeMap<DiskScope, Ask>,
    policy: Option<Policy>,
    tab_claims: BTreeMap<u64, Vec<Claim>>,
    own_claims: Vec<Claim>,
    life: BTreeMap<PathBuf, Life>,
    discoveries: u64,
    /// When the last discovery ran, on which facts, for which scopes.
    discovered: Option<(Instant, u64, BTreeSet<DiskScope>)>,
    records: BTreeMap<PathBuf, Arc<RootRecord>>,
    kept: Kept,
    huge: BTreeMap<PathBuf, HugeDir>,
    outside: BTreeSet<PathBuf>,
    huge_seq: u64,
    profiles_at: Option<Instant>,
    /// A profile's executables answer and when it was read.
    exec_known: BTreeMap<PathBuf, (Instant, bool)>,
    /// `CARGO_TARGET_DIR` by `(pid, start)`: one environment read per process.
    env_cache: HashMap<(u32, u64), Option<PathBuf>>,
    walker: Walker,
    active: Option<Active>,
    dirty: bool,
}

fn walker_main(inbox: Receiver<ToWalker>, out: Sender<Arc<Published>>, proxy: Option<Proxy>) {
    let policy_out = aterm_diskwalk::set_walker_thread_policy();
    if !policy_out.failed.is_empty() {
        aterm_log::warn!("disk walker: I/O policy not taken: {:?}", policy_out.failed);
    }
    Engine::new(inbox, out, proxy).run();
}

impl Engine {
    fn new(inbox: Receiver<ToWalker>, out: Sender<Arc<Published>>, proxy: Option<Proxy>) -> Self {
        Self {
            inbox,
            out,
            proxy,
            facts: Facts::default(),
            facts_seq: 0,
            demand: BTreeMap::new(),
            policy: None,
            tab_claims: BTreeMap::new(),
            own_claims: Vec::new(),
            life: BTreeMap::new(),
            discoveries: 0,
            discovered: None,
            records: BTreeMap::new(),
            kept: Kept::default(),
            huge: BTreeMap::new(),
            outside: BTreeSet::new(),
            huge_seq: 0,
            profiles_at: None,
            exec_known: BTreeMap::new(),
            env_cache: HashMap::new(),
            walker: Walker::new(),
            active: None,
            dirty: false,
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
        }
    }

    /// Take what is in the inbox. False: stop.
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

    /// One message. False: stop.
    fn apply(&mut self, msg: ToWalker) -> bool {
        match msg {
            ToWalker::Facts(facts) => {
                if facts != self.facts {
                    self.facts = facts;
                    self.facts_seq += 1;
                    self.policy = self.facts.home.as_deref().map(|home| {
                        Policy::new(
                            Deny::for_home(home, &self.facts.overrides, &self.facts.opt_in),
                            aterm_sysprobe::volumes::table(),
                        )
                    });
                }
            }
            ToWalker::Demand { scope, detail, at } => {
                let ask = self
                    .demand
                    .entry(scope)
                    .or_insert(Ask { at, frees_at: None });
                ask.at = ask.at.max(at);
                if detail == Detail::Frees {
                    ask.frees_at = Some(at);
                }
            }
            #[cfg(test)]
            ToWalker::Stop => return false,
        }
        true
    }

    /// Sleep until a message, or until the next thing owed is due. False: stop.
    fn sleep(&mut self, now: Instant) -> bool {
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

    /// The scopes demanded at `now`.
    fn live(&self, now: Instant) -> BTreeSet<DiskScope> {
        self.demand
            .iter()
            .filter(|(_, a)| now.saturating_duration_since(a.at) < DEMAND_TTL)
            .map(|(s, _)| *s)
            .collect()
    }

    /// The paths a live scope asked [`Detail::Frees`] for, with every promoted
    /// root above each (a nested root's figures are its parent's walk's).
    fn frees_wanted(&self, now: Instant) -> BTreeSet<PathBuf> {
        let mut out = BTreeSet::new();
        for (scope, ask) in &self.demand {
            let wanted = ask
                .frees_at
                .is_some_and(|t| now.saturating_duration_since(t) < DEMAND_TTL);
            if !wanted || now.saturating_duration_since(ask.at) >= DEMAND_TTL {
                continue;
            }
            let claims: Vec<&Claim> = match scope {
                DiskScope::Tab(k) => self.tab_claims.get(k).into_iter().flatten().collect(),
                DiskScope::Aterm => self
                    .tab_claims
                    .values()
                    .flatten()
                    .chain(&self.own_claims)
                    .collect(),
            };
            for c in claims {
                out.insert(c.path.clone());
                for (p, l) in &self.life {
                    if l.promoted && c.path.starts_with(p) {
                        out.insert(p.clone());
                    }
                }
            }
        }
        out
    }

    /// The roots walked: promoted, outermost first.
    fn plan(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .life
            .iter()
            .filter(|(_, l)| l.promoted)
            .map(|(p, _)| p.clone())
            .collect();
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
        if live.is_empty() {
            // stay for the next read, which derives the roots again first.
            return;
        }
        let before = self.discovered.as_ref();
        let due = before.is_none_or(|(at, seq, scopes)| {
            *seq != self.facts_seq
                || *scopes != live
                || now.saturating_duration_since(*at) >= DISCOVER_EVERY
        });
        if !due {
            return;
        }
        let newly: BTreeSet<DiskScope> = live
            .iter()
            .filter(|s| before.is_none_or(|(_, _, scopes)| !scopes.contains(s)))
            .copied()
            .collect();
        let (tabs, own) = match self.policy.clone() {
            Some(p) => self.discover(&live, &p),
            None => (BTreeMap::new(), Vec::new()),
        };
        self.discoveries += 1;
        let claimed: BTreeSet<PathBuf> = tabs
            .values()
            .flatten()
            .chain(&own)
            .map(|c| c.path.clone())
            .collect();
        let at_once: BTreeSet<PathBuf> = newly
            .iter()
            .flat_map(|scope| -> Vec<PathBuf> {
                match scope {
                    DiskScope::Tab(k) => tabs
                        .get(k)
                        .into_iter()
                        .flatten()
                        .map(|c| c.path.clone())
                        .collect(),
                    DiskScope::Aterm => claimed.iter().cloned().collect(),
                }
            })
            .collect();
        for gone in age_roots(&mut self.life, &claimed, &at_once, self.discoveries, now) {
            self.records.remove(&gone);
            self.kept.trees.remove(&gone);
            self.kept.history.remove(&gone);
        }
        let life = &self.life;
        self.records.retain(|p, _| life.contains_key(p));
        self.tab_claims = tabs;
        self.own_claims = own;
        self.rederive_nested();
        self.discovered = Some((now, self.facts_seq, live));
        self.dirty = true;
    }

    /// Every demanded pane's claims and aterm's own, derived now.
    fn discover(
        &mut self,
        live: &BTreeSet<DiskScope>,
        policy: &Policy,
    ) -> (BTreeMap<u64, Vec<Claim>>, Vec<Claim>) {
        let aterm = live.contains(&DiskScope::Aterm);
        let wanted: Vec<TabFacts> = self
            .facts
            .tabs
            .iter()
            .filter(|t| aterm || live.contains(&DiskScope::Tab(t.key)))
            .cloned()
            .collect();
        let procs: Vec<(u32, u32)> = if wanted.is_empty() {
            Vec::new()
        } else {
            aterm_sysprobe::processes()
                .map(|s| s.procs.iter().map(|p| (p.pid, p.ppid)).collect())
                .unwrap_or_default()
        };
        let scratch = scratch_base();
        let cache = RefCell::new(std::mem::take(&mut self.env_cache));
        let seen: RefCell<BTreeSet<(u32, u64)>> = RefCell::new(BTreeSet::new());
        let target_dir = |pid: u32| -> Option<PathBuf> {
            let read = || {
                atpkg::caller_shell::process_args(pid)
                    .and_then(|a| a.env_var("CARGO_TARGET_DIR").map(PathBuf::from))
            };
            let Some(start) = i32::try_from(pid)
                .ok()
                .and_then(aterm_uds::process::read_process_start)
            else {
                return read();
            };
            let key = (pid, start.boot.0);
            seen.borrow_mut().insert(key);
            if let Some(known) = cache.borrow().get(&key) {
                return known.clone();
            }
            let value = read();
            cache.borrow_mut().insert(key, value.clone());
            value
        };
        let writes = |pid: u32| aterm_uds::holders::write_open_files(pid).unwrap_or_default();
        let reads = ProcReads {
            cwd: &aterm_sysprobe::process_cwd,
            target_dir: &target_dir,
            writes: &writes,
        };
        let mut tabs = BTreeMap::new();
        for t in &wanted {
            tabs.insert(
                t.key,
                discover_tab(
                    t,
                    &procs,
                    &reads,
                    policy,
                    self.facts.home.as_deref(),
                    scratch.as_deref(),
                ),
            );
        }
        let mut cache = cache.into_inner();
        let seen = seen.into_inner();
        cache.retain(|k, _| seen.contains(k));
        self.env_cache = cache;
        let own = if aterm {
            own_claims(&self.facts.own, &self.facts.opt_in)
        } else {
            Vec::new()
        };
        (tabs, own)
    }

    /// Read every promoted root inside another walked root from that root's
    /// tree; drop a derived record whose parent's tree no longer holds it.
    fn rederive_nested(&mut self) {
        let plan = self.plan();
        let derived: Vec<(PathBuf, Option<RootRecord>)> = plan
            .iter()
            .map(|p| (p.clone(), derive_nested(p, &plan, &self.kept)))
            .collect();
        for (p, d) in derived {
            match d {
                Some(r) => {
                    self.records.insert(p, Arc::new(r));
                }
                None => {
                    if self.records.get(&p).is_some_and(|r| r.read_from.is_some()) {
                        self.records.remove(&p);
                    }
                }
            }
        }
    }

    /// The paths claimed as `writes` roots now.
    fn writes_roots(&self) -> BTreeSet<PathBuf> {
        self.tab_claims
            .values()
            .flatten()
            .filter(|c| c.kinds.contains(&RootKind::Writes))
            .map(|c| c.path.clone())
            .collect()
    }

    /// The next root owed a walk (design §10.4, §10.6): one never walked,
    /// those holding a `writes` root first, then outermost first; else the
    /// stalest walk past its [`Complete::rewalk_at`], or one a live scope
    /// wants [`Detail::Frees`] for that has only sizes.
    fn next_root(&self, now: Instant) -> Option<PathBuf> {
        if self.live(now).is_empty() || self.policy.is_none() {
            return None;
        }
        let plan = self.plan();
        let writes = self.writes_roots();
        let frees = self.frees_wanted(now);
        pick_root(&plan, &self.records, &writes, &frees, now)
    }

    /// When the next walk falls due (for the sleep).
    fn next_due(&self, now: Instant) -> Option<Instant> {
        self.plan()
            .iter()
            .filter_map(|p| self.records.get(p))
            .filter(|r| r.read_from.is_none())
            .filter_map(|r| match r.state {
                RootState::Queued => Some(now),
                RootState::Walking => None,
                RootState::Complete => r.complete.as_ref().map(Complete::rewalk_at),
                RootState::NotMeasured(_) | RootState::OneLevel => {
                    r.asked.map(|t| t + REWALK_AFTER)
                }
            })
            .min()
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
        match self.walker.start(&root, &policy) {
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
                    prior,
                    walked: 0,
                });
                self.put_walking();
            }
        }
    }

    /// The walk in flight's record, as it stands.
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
            complete: a.prior.as_ref().and_then(|r| r.complete.clone()),
            level: Vec::new(),
            read_from: None,
            asked: Some(a.started),
        };
        self.put(rec);
    }

    /// Slices of the walk in flight for up to [`PUBLISH_EVERY`], then its
    /// partial figure; a walk no live scope's plan holds any more is
    /// cancelled.
    fn step_walk(&mut self) {
        let Some(ticket) = self.active.as_ref().map(|a| a.ticket) else {
            return;
        };
        let until = Instant::now() + PUBLISH_EVERY;
        let mut clock = Instant::now;
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
                    self.finish_walk();
                    return;
                }
                Sliced::Step(Step::Cancelled) | Sliced::Stale => {
                    self.abandon_walk();
                    return;
                }
            }
        }
        let now = Instant::now();
        let held = self
            .active
            .as_ref()
            .is_some_and(|a| self.plan().contains(&a.path));
        if self.live(now).is_empty() || !held {
            self.walker.cancel();
            self.abandon_walk();
            return;
        }
        self.put_walking();
    }

    /// A walk that stopped before its end leaves what was there before it:
    /// the last complete figures, or nothing (the root is queued again).
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

    /// A walk reached its end: its record, its tree for the roots inside it,
    /// its history for growth, and its §10.8 rows folded into the raised set.
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
        let life = &self.life;
        self.kept.trees.retain(|p, _| life.contains_key(p));
        self.kept.history.retain(|p, _| life.contains_key(p));
        self.rederive_nested();
        let found: BTreeMap<PathBuf, HugeDir> = tree
            .huge_dirs()
            .into_iter()
            .map(|h| (h.path.clone(), h))
            .collect();
        let next = fold_raised(&self.huge, tree.root_path(), &found);
        self.set_huge(next, self.outside.clone());
    }

    /// Replace the raised set; a move is a new `huge_seq`.
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

    /// The slow tier (§10.8), with no demand: every [`PROFILES_EVERY`], one
    /// entry count per build profile of the home folder's checkouts outside
    /// every walked root, folded into the raised set.
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
        let covered = self.plan();
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

    /// Whether a profile's `deps/` holds executables: a bounded walk of it,
    /// kept [`EXEC_KNOWN_FOR`] when it knows.
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

    fn publish_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let published = Arc::new(Published {
            tabs: self
                .facts
                .tabs
                .iter()
                .map(|t| (t.key, t.label.clone()))
                .collect(),
            tab_claims: self.tab_claims.clone(),
            own_claims: self.own_claims.clone(),
            records: self.records.clone(),
            huge: self.huge.clone(),
            outside: self.outside.clone(),
            huge_seq: self.huge_seq,
        });
        if self.out.send(published).is_ok()
            && let Some(proxy) = self.proxy.as_ref()
        {
            let _ = proxy.send_event(crate::Wake::DiskWalk);
        }
    }
}

/// The walk owed next, pure (see [`Engine::next_root`]).
pub(crate) fn pick_root(
    plan: &[PathBuf],
    records: &BTreeMap<PathBuf, Arc<RootRecord>>,
    writes: &BTreeSet<PathBuf>,
    frees: &BTreeSet<PathBuf>,
    now: Instant,
) -> Option<PathBuf> {
    let parent_pending = |p: &Path| {
        plan.iter().any(|q| {
            q.as_path() != p
                && p.starts_with(q)
                && records
                    .get(q)
                    .is_none_or(|r| matches!(r.state, RootState::Queued | RootState::Walking))
        })
    };
    let holds_writes = |p: &Path| writes.iter().any(|w| w.starts_with(p));
    let mut fresh: Vec<&PathBuf> = plan
        .iter()
        .filter(|p| {
            records
                .get(*p)
                .is_none_or(|r| r.state == RootState::Queued && r.read_from.is_none())
                && !parent_pending(p)
        })
        .collect();
    fresh.sort_by_key(|p| (!holds_writes(p), p.components().count(), (*p).clone()));
    if let Some(p) = fresh.first() {
        return Some((*p).clone());
    }
    let mut stalest: Option<(Instant, &PathBuf)> = None;
    for p in plan {
        let Some(r) = records.get(p) else {
            continue;
        };
        if r.read_from.is_some() {
            continue;
        }
        let due = match r.state {
            RootState::Queued | RootState::Walking => None,
            RootState::Complete => r.complete.as_ref().map(|c| {
                if frees.contains(p) && c.detail != Detail::Frees {
                    now
                } else {
                    c.rewalk_at()
                }
            }),
            RootState::NotMeasured(_) | RootState::OneLevel => r.asked.map(|t| t + REWALK_AFTER),
        };
        if let Some(t) = due.filter(|t| *t <= now)
            && stalest.is_none_or(|(s, _)| t < s)
        {
            stalest = Some((t, p));
        }
    }
    stalest.map(|(_, p)| p.clone())
}

/// The record of `path` read from the walk of a promoted root it lies
/// inside, when that walk holds it.
fn derive_nested(path: &Path, paths: &[PathBuf], kept: &Kept) -> Option<RootRecord> {
    let parent = paths
        .iter()
        .filter(|q| q.as_path() != path && path.starts_with(q))
        .filter(|q| kept.trees.contains_key(q.as_path()))
        .max_by_key(|q| q.components().count())?;
    let tree = kept.trees.get(parent)?;
    let rel = path.strip_prefix(parent).ok()?;
    let id = tree.find(rel)?;
    let rec = tree.get(id)?;
    if rec.refused.is_some() || rec.mount_boundary {
        return None;
    }
    let history = kept.history.get(parent);
    let at = history
        .and_then(|h| h.back())
        .map_or_else(Instant::now, |&(t, _)| t);
    Some(RootRecord {
        path: path.to_path_buf(),
        state: RootState::Complete,
        id: None,
        partial: None,
        walked_entries: 0,
        complete: Some(complete_of(tree, id, at, Duration::ZERO, None)),
        level: Vec::new(),
        read_from: Some(parent.clone()),
        asked: Some(at),
    })
}

/// The figures of `within` in `tree`.
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

/// The allocation's move over [`GROWTH_WINDOW`]: against the newest walk at
/// least a window old, else the oldest walk kept (a shorter span, said).
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

/// The build folders of the home folder's checkouts (its direct children):
/// `C/target` (a link to `target.noindex` read, never followed) and atpkg's
/// build cache for `C`, each looked for with the path test first.
fn outside_build_dirs(home: &Path, policy: &Policy) -> Vec<PathBuf> {
    let Ok(rows) = list_one_level(&RootSpec::new(home), policy) else {
        return Vec::new();
    };
    let mut claims = Vec::new();
    for row in rows
        .iter()
        .filter(|r| r.dir && r.refused.is_none() && !r.mount_boundary)
        .take(MAX_CHECKOUTS)
    {
        build_roots(&mut claims, &home.join(&row.name), policy, Some(home));
    }
    let mut out: Vec<PathBuf> = claims.into_iter().map(|c| c.path).collect();
    out.sort();
    out.dedup();
    out
}

/// Each build profile's `deps/` in the build folder `target` and its direct
/// entry count: `target/<profile>/deps`, or for a cross build
/// `target/<triple>/<profile>/deps` (a triple's name has a `-`; `doc`,
/// `tmp` and `package` are not looked into).
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

/// Whether a profile's `deps/` holds executables, from a walk of it bounded by
/// `budget`: yes at the first one met (the walk stops there), no once the
/// whole folder was listed without one, and `None` (not known) when the
/// budget ran out first or the path test refused the folder.
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

/// The slow tier's hysteresis, pure: each counted profile is raised at
/// [`HUGE_WARN`], kept at or above [`HUGE_NOTE`] and lowered under it, as a
/// walk's are; one the slow tier raised and no longer counts is lowered (its
/// checkout gone), unless a walked root now holds it (that walk's fold owns it
/// from now on).
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

/// The hysteresis, pure: `raised` after a complete walk of `root` found
/// `found` (its folders at [`HUGE_NOTE`] or more).
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

/// `104211` → `104,211`.
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

/// What the loop keeps: the walker's publications, when it next looks at the panes' facts and what it last sent, and the §10.8 set it last stated.
#[derive(Default)]
pub(crate) struct DiskwalkHost {
    from_walker: Option<Receiver<Arc<Published>>>,
    started: bool,
    facts_due: Option<Instant>,
    sent_facts: Option<Facts>,
    huge_seen: u64,
}

impl crate::App {
    /// The board's answer for `scope` (the System panel's read), demanding it.
    #[cfg_attr(
        test,
        expect(
            dead_code,
            reason = "tests read their own board (`Shared::new`); the process's is the readers'"
        )
    )]
    pub(crate) fn disk_roots(&self, scope: DiskScope, detail: Detail) -> DiskReport {
        disk_report(scope, detail, Instant::now())
    }

    /// A level judged every wake: start the walker once the loop has a proxy (the shipped loop; a test's `App` has none), take what it published, state a moved §10.8 set to the digest, and send the panes' facts when they changed (looked at every [`FACTS_EVERY`] while a scope is demanded, every [`FACTS_IDLE_EVERY`] otherwise, and at once when a reader asked a scope anew).
    pub(crate) fn service_diskwalk(&mut self, now: Instant) {
        if !self.diskwalk.started
            && let Some(proxy) = self.proxy.clone()
        {
            self.diskwalk.started = true;
            *DISKWALK.proxy.lock_or_recover() = Some(proxy.clone());
            match spawn_walker(&DISKWALK, Some(proxy)) {
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
        let wanted = DISKWALK.facts_wanted.swap(false, Ordering::Relaxed);
        if wanted || self.diskwalk.facts_due.is_none_or(|due| now >= due) {
            let every = if DISKWALK.demanded(now) {
                FACTS_EVERY
            } else {
                FACTS_IDLE_EVERY
            };
            self.diskwalk.facts_due = Some(now + every);
            let facts = self.diskwalk_facts();
            if self.diskwalk.sent_facts.as_ref() != Some(&facts) {
                DISKWALK.send(ToWalker::Facts(facts.clone()));
                self.diskwalk.sent_facts = Some(facts);
            }
        }
    }

    /// `Wake::DiskWalk`: the walker published, or a reader asked a scope anew.
    pub(crate) fn on_disk_walk_wake(&mut self) {
        self.service_diskwalk(Instant::now());
    }

    /// Every pane with a live shell, and what the path test needs.
    fn diskwalk_facts(&self) -> Facts {
        let mut tabs = Vec::new();
        for ws in self.windows.values() {
            for (i, tab) in ws.tab_set.tabs().iter().enumerate() {
                let Some(crate::tab_model::View::Terminal(view)) =
                    self.view_store.get(tab.focus).copied()
                else {
                    continue;
                };
                let Some(s) = self.pool.get(view.session) else {
                    continue;
                };
                let Ok(pid) = u32::try_from(s.pid) else {
                    continue;
                };
                if pid == 0 {
                    continue;
                }
                tabs.push(TabFacts {
                    key: view.session,
                    label: format!("Tab {}", i + 1),
                    shell_pid: pid,
                    reported_cwd: self
                        .session_reported_cwd(view.session)
                        .map(PathBuf::from)
                        .filter(|p| p.is_absolute()),
                });
            }
        }
        let home = aterm_types::dirs::home_dir();
        let mut own: Vec<PathBuf> = aterm_types::dirs::state_dir().into_iter().collect();
        if cfg!(target_os = "macos")
            && let Some(h) = home.as_ref()
        {
            own.push(h.join("Library/Caches/aterm"));
        }
        Facts {
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

/// Unix only: the tests build scratch trees with links and modes, and on any
/// other platform every walk is `Refusal::Unsupported`.
#[cfg(all(test, unix))]
#[path = "diskwalk_host_tests.rs"]
mod tests;
