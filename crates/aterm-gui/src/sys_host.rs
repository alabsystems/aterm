// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE `sys` HOST: `aterm ctl sys` and the machine terms of `await sys`, answered on the control worker from the workers' copy of the vitals snapshot (`docs/DESIGN-system-awareness-2026-10-16.md` §13, §6.4, §3.2).

use std::collections::VecDeque;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_vitals::events::Event;
use aterm_vitals::model::Snapshot;
use aterm_vitals::sys::predicate::{EndedJob, Latch, Parsed, evaluate_with, met_reply};
use aterm_vitals::sys::{
    AwaitSys, Ctx, Cursor, DiskTotals, Fingerprints, Freshness, Gap, Machine, Reply, Root,
    RootSize, ScopeArg, SysQuery, TabFact, Target, View, VolumeFacts, apply_delta, fingerprints,
    render_json, render_text,
};

use crate::session_store::Store;
use crate::vitals_host::Msg;

/// The event ring's size.
pub(crate) const EVENTS_CAP: usize = 256;
/// Reply fingerprints kept at least…
pub(crate) const REPLIES_MIN: usize = 256;
/// …or for at least this long, whichever holds more…
pub(crate) const REPLIES_AGE: Duration = Duration::from_secs(30 * 60);
/// …and never more than this many (about 2 MB).
pub(crate) const REPLIES_MAX: usize = 4096;
/// A pull waits this long for its reading…
pub(crate) const PULL_WAIT: Duration = Duration::from_secs(1);
/// …or this long when the probe reads twice (`vitals_host::COLD_PAIR`).
pub(crate) const PULL_WAIT_COLD: Duration = Duration::from_millis(1500);
/// At most one pull this often (four a second).
pub(crate) const PULL_GAP: Duration = Duration::from_millis(250);
/// An armed `await sys` reads at this cadence.
pub(crate) const AWAIT_PERIOD: Duration = Duration::from_secs(1);
/// The longest rate window a reply stands on (§6.4): a reading after minutes of nothing is an average over those minutes, which says nothing of now.
pub(crate) const WINDOW_MAX: Duration = Duration::from_secs(60);

/// Where one terminal session sits: its window's stable id and its tab (1-based).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct PaneFact {
    /// The session's local id.
    pub(crate) key: u64,
    /// Its window's stable id (`sessions`' `window=`).
    pub(crate) window: u64,
    /// Its tab in that window, 1-based.
    pub(crate) tab: usize,
}

/// One snapshot as the workers hold it.
#[derive(Clone, Debug)]
pub(crate) struct Held {
    /// The snapshot.
    pub(crate) snap: Arc<Snapshot>,
    /// When it landed, wall clock (unix ms).
    pub(crate) unix_ms: u64,
    /// …and on the monotonic clock.
    pub(crate) at: Instant,
}

/// A reply's identity for a later `since=`: what its fingerprints were
/// printed for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReplyKey {
    seq: u64,
    view: View,
    scope: Option<ScopeArg>,
    target: u64,
    /// `top=` and `depth=`: how many rows print.
    top: usize,
    depth: Option<u8>,
    /// The caller's tab, which decides which roots print paths (§13.3).
    caller_panes: Option<Vec<u64>>,
    /// Whether machine-wide rows may print.
    owner_class: bool,
}

struct Kept {
    key: ReplyKey,
    prints: Fingerprints,
    at: Instant,
}

#[derive(Default)]
struct State {
    /// The newest whole snapshot (module docs).
    held: Option<Held>,
    /// The newest snapshot seq published, a machine-only one's included: the
    /// cursor's head.
    head_seq: u64,
    panes: Vec<PaneFact>,
    machine: Machine,
    events: VecDeque<Event>,
    /// The newest snapshot seq whose events left the ring (0: none did).
    evicted_through: u64,
    replies: VecDeque<Kept>,
    tx: Option<SyncSender<Msg>>,
    last_pull: Option<Instant>,
    pacing: Pacing,
}

/// How the workers pull: at most one pull per `gap`, each waited for at most
/// `wait` (`wait_cold` when the probe reads twice).
#[derive(Clone, Copy, Debug)]
struct Pacing {
    gap: Duration,
    wait: Duration,
    wait_cold: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            gap: PULL_GAP,
            wait: PULL_WAIT,
            wait_cold: PULL_WAIT_COLD,
        }
    }
}

/// The workers' copy.
pub(crate) struct Shared {
    state: Mutex<State>,
    landed: Condvar,
}

/// The process's one copy.
pub(crate) static SYS: LazyLock<Shared> = LazyLock::new(Shared::new);

/// This instance's epoch, the `<ep>` of every cursor: new at every start (a live upgrade's successor included), from the start's wall clock and pid.
pub(crate) static EPOCH: LazyLock<u64> = LazyLock::new(|| {
    let ms = aterm_time::unix_millis().unwrap_or(0);
    let mixed = (ms ^ (u64::from(std::process::id()) << 40)) & 0xffff_ffff_ffff;
    mixed.max(1)
});

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            landed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The probe thread hands over its sender, so workers can pull.
    pub(crate) fn install(&self, tx: SyncSender<Msg>) {
        self.lock().tx = Some(tx);
    }

    /// The quit takes the sender back, so the probe thread hears the hang-up and ends (`VitalsHost::stop_at_exit`).
    pub(crate) fn uninstall(&self) {
        self.lock().tx = None;
    }

    /// The probe thread's newest layout.
    pub(crate) fn set_facts(&self, panes: Vec<PaneFact>) {
        self.lock().panes = panes;
    }

    /// The probe thread's newest snapshot: its events rung, the head moved, held when it is whole (module docs), every waiting worker woken.
    pub(crate) fn publish(&self, snap: Arc<Snapshot>, machine: &Machine) {
        let mut g = self.lock();
        if g.machine != *machine {
            g.machine.clone_from(machine);
        }
        for e in &snap.events {
            if g.events.len() == EVENTS_CAP
                && let Some(old) = g.events.pop_front()
            {
                g.evicted_through = g.evicted_through.max(old.seq);
            }
            g.events.push_back(e.clone());
        }
        g.head_seq = g.head_seq.max(snap.seq);
        if !snap.machine_only || g.held.is_none() {
            g.held = Some(Held {
                snap,
                unix_ms: aterm_time::unix_millis().unwrap_or(0),
                at: Instant::now(),
            });
        }
        drop(g);
        self.landed.notify_all();
    }

    /// The newest whole snapshot as held, without pulling: the occupancy board's ([`crate::occupancy_host::look`]), which reads the reading the disk look asked for at its next look.
    pub(crate) fn held_now(&self) -> Option<Held> {
        self.lock().held.clone()
    }

    /// The cursor's head seq.
    fn head_seq(&self) -> u64 {
        self.lock().head_seq
    }

    /// A whole snapshot at most `fresh` old (`force`: one taken after this call), pulling one when needed and waiting a bounded time for it.
    pub(crate) fn fresh(&self, fresh: Duration, force: bool) -> (Option<Held>, Freshness) {
        let now = Instant::now();
        let mut g = self.lock();
        let have = g.held.clone();
        if let Some(h) = have.as_ref()
            && !force
            && h.at.elapsed() <= fresh
            && whole(&h.snap)
        {
            return (have.clone(), Freshness::Fresh);
        }
        let old_seq = have.as_ref().map(|h| h.snap.seq);
        let cold = have
            .as_ref()
            .is_none_or(|h| !whole(&h.snap) || h.at.elapsed() > crate::vitals_host::COLD_AFTER);
        let pacing = g.pacing;
        let recent_pull = g.last_pull.filter(|t| t.elapsed() < pacing.gap);
        match recent_pull {
            Some(t) => {
                if let Some(h) = have.as_ref()
                    && h.at >= t
                    && whole(&h.snap)
                {
                    return (have.clone(), Freshness::Fresh);
                }
            }
            None => {
                let sent = match g.tx.as_ref().map(|tx| tx.try_send(Msg::Pull)) {
                    Some(Ok(()) | Err(TrySendError::Full(_))) => true,
                    Some(Err(TrySendError::Disconnected(_))) => {
                        g.tx = None;
                        false
                    }
                    None => false,
                };
                if sent {
                    g.last_pull = Some(now);
                }
            }
        }
        if g.tx.is_none() {
            // No probe thread (none started yet, or it ended): what is held.
            return stale(have);
        }
        let deadline = now + if cold { pacing.wait_cold } else { pacing.wait };
        loop {
            if let Some(h) = g.held.as_ref()
                && Some(h.snap.seq) != old_seq
                && whole(&h.snap)
            {
                return (Some(h.clone()), Freshness::Fresh);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || crate::control::caller_hung_up() {
                break;
            }
            g = self
                .landed
                .wait_timeout(g, crate::control::hangup_park(left))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        stale(g.held.clone())
    }

    /// Park until a snapshot other than `seq` lands or `until`, at most one
    /// hang-up poll at a time.
    fn wait_newer(&self, seq: Option<u64>, until: Instant) {
        let mut g = self.lock();
        loop {
            if g.held.as_ref().map(|h| h.snap.seq) != seq {
                return;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || crate::control::caller_hung_up() {
                return;
            }
            g = self
                .landed
                .wait_timeout(g, crate::control::hangup_park(left))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The newest snapshot's jobs view and when that snapshot landed, without pulling (`brief`'s jobs line), and whether its reader runs (the probe thread's sender is installed: the quit takes it back).
    pub(crate) fn jobs_view(&self) -> Option<(Arc<aterm_vitals::jobs::JobsView>, Instant, bool)> {
        let g = self.lock();
        let h = g.held.as_ref()?;
        Some((Arc::clone(h.snap.jobs.as_ref()?), h.at, g.tx.is_some()))
    }

    /// The layout and the machine's words.
    fn facts(&self) -> (Vec<PaneFact>, Machine) {
        let g = self.lock();
        (g.panes.clone(), g.machine.clone())
    }

    /// The jobs the ring remembers exiting, oldest first: a wait on one that
    /// already left the engine's view reads its end from here.
    fn ended_jobs(&self) -> Vec<EndedJob> {
        self.lock().events.iter().filter_map(EndedJob::of).collect()
    }

    /// The events after snapshot `seq`, and whether the ring still holds all
    /// of them.
    fn events_since(&self, seq: u64) -> (Vec<Event>, bool) {
        let g = self.lock();
        let events = g.events.iter().filter(|e| e.seq > seq).cloned().collect();
        (events, g.evicted_through <= seq)
    }

    /// The fingerprints of the newest kept reply with `key`.
    fn recall(&self, key: &ReplyKey) -> Option<Fingerprints> {
        self.lock()
            .replies
            .iter()
            .rev()
            .find(|k| k.key == *key)
            .map(|k| k.prints.clone())
    }

    /// Keep a reply's fingerprints, dropping the oldest past the ring's rule.
    fn remember(&self, key: ReplyKey, prints: Fingerprints) {
        let mut g = self.lock();
        g.replies.push_back(Kept {
            key,
            prints,
            at: Instant::now(),
        });
        while g.replies.len() > REPLIES_MAX
            || (g.replies.len() > REPLIES_MIN
                && g.replies
                    .front()
                    .is_some_and(|k| k.at.elapsed() > REPLIES_AGE))
        {
            g.replies.pop_front();
        }
    }

    /// A copy whose pulls are `gap` apart and waited for `wait` (a test's: a long gap makes "within the gap" certain, a long wait is a hang detector, never a latency budget).
    #[cfg(test)]
    pub(crate) fn paced(gap: Duration, wait: Duration) -> Self {
        let shared = Self::new();
        shared.lock().pacing = Pacing {
            gap,
            wait,
            wait_cold: wait,
        };
        shared
    }

    /// Hold `snap` as if the probe left it (a test's).
    #[cfg(test)]
    pub(crate) fn publish_for_test(&self, snap: Snapshot, panes: Vec<PaneFact>) {
        self.set_facts(panes);
        self.publish(Arc::new(snap), &Machine::default());
    }
}

/// Whether `snap` measured what a reply prints (§6.4 step 1): a whole reading (not a hidden Deck's machine counters), with CPU, aterm and process rates, over a window of at most [`WINDOW_MAX`].
pub(crate) fn whole(snap: &Snapshot) -> bool {
    !snap.machine_only
        && snap.cpu.is_some()
        && snap.aterm.is_some()
        && snap
            .procs_window_ns
            .is_some_and(|w| u128::from(w) <= WINDOW_MAX.as_nanos())
}

/// `otherwise`, unless the snapshot is a first sample without rates.
fn freshness(h: &Held, otherwise: Freshness) -> Freshness {
    if h.snap.cpu.is_none() || h.snap.aterm.is_none() {
        Freshness::Warming
    } else {
        otherwise
    }
}

/// What a pull that did not land answers: the last snapshot, labelled.
fn stale(held: Option<Held>) -> (Option<Held>, Freshness) {
    match held {
        Some(h) => {
            let f = freshness(&h, Freshness::Stale);
            (Some(h), f)
        }
        None => (None, Freshness::Warming),
    }
}

/// A session's working directory as its shell reported it; `None` when the
/// engine is busy (never park behind a PTY reader for a path).
fn cwd_of(term: &Arc<Mutex<Terminal>>) -> Option<String> {
    use crate::cwd_native::ReportedCwd as _;
    let read = |t: &Terminal| {
        t.native_working_directory()
            .map(std::borrow::Cow::into_owned)
            .filter(|p| !p.is_empty())
    };
    match term.try_lock() {
        Ok(t) => read(&t),
        Err(std::sync::TryLockError::Poisoned(p)) => read(&p.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// The terminal's foreground process group.
fn fg_pgid(master: i32) -> Option<u32> {
    #[cfg(unix)]
    {
        if master < 0 {
            return None;
        }
        // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
        let g = unsafe { libc::tcgetpgrp(master) };
        u32::try_from(g).ok().filter(|g| *g > 0)
    }
    #[cfg(not(unix))]
    {
        let _ = master;
        None
    }
}

/// What one request reads besides the snapshot.
struct Gathered {
    target: Target,
    tabs: Vec<TabFact>,
    roots: Vec<Root>,
    disk: Option<DiskTotals>,
    volume: Option<VolumeFacts>,
    /// The sessions of the caller's own tab; `None` with no caller session
    /// (`Ctx::caller_panes`).
    caller_panes: Option<Vec<u64>>,
    home: Option<String>,
}

/// A pane's session key and its terminal.
type PaneTerm = (u64, Arc<Mutex<Terminal>>);

/// The target, every session's place, and (for `disk`) the scope's folders
/// and the target's volume.
fn gather(
    store: &Store,
    session: u64,
    term: &Arc<Mutex<Terminal>>,
    master: i32,
    panes: &[PaneFact],
    q: &SysQuery,
    snap: Option<&Snapshot>,
) -> Gathered {
    let (sids, terms, caller_key): (Vec<(u64, String)>, Vec<PaneTerm>, Option<u64>) = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        let mut sids = Vec::new();
        let mut terms = Vec::new();
        for p in panes {
            if let Some(h) = g.by_local(p.key) {
                sids.push((p.key, h.sid.as_str().to_owned()));
                terms.push((p.key, Arc::clone(&h.term)));
            }
        }
        if !sids.iter().any(|(k, _)| *k == session)
            && let Some(h) = g.by_local(session)
        {
            sids.push((session, h.sid.as_str().to_owned()));
        }
        let caller_key = q.caller.as_deref().and_then(|c| {
            g.by_sid(&aterm_session::SessionId::new(c))
                .map(|h| h.local_id)
        });
        (sids, terms, caller_key)
    };
    let sid_of = |k: u64| {
        sids.iter()
            .find(|(key, _)| *key == k)
            .map(|(_, s)| s.clone())
            .unwrap_or_default()
    };
    let me = panes.iter().find(|p| p.key == session).copied();
    let panes_of_tab: Vec<u64> = me.map_or_else(
        || vec![session],
        |m| {
            panes
                .iter()
                .filter(|p| p.window == m.window && p.tab == m.tab)
                .map(|p| p.key)
                .collect()
        },
    );
    let window_sessions: Vec<u64> = me.map_or_else(
        || vec![session],
        |m| {
            panes
                .iter()
                .filter(|p| p.window == m.window)
                .map(|p| p.key)
                .collect()
        },
    );
    let shell_cwd = |k: u64| {
        snap.and_then(|s| s.tabs.iter().find(|t| t.key == k))
            .map(|t| t.shell_pid)
            .filter(|&pid| pid > 0)
            .and_then(aterm_sysprobe::process_cwd)
            .map(|p| p.display().to_string())
    };
    let cwd = cwd_of(term).or_else(|| shell_cwd(session));
    let target = Target {
        key: session,
        sid: sid_of(session),
        window: me.and_then(|m| usize::try_from(m.window).ok()),
        tab: me.map(|m| m.tab),
        panes: panes_of_tab.clone(),
        window_sessions: window_sessions.clone(),
        cwd: cwd.clone(),
        fg_pgid: fg_pgid(master),
    };
    let tabs: Vec<TabFact> = panes
        .iter()
        .map(|p| TabFact {
            key: p.key,
            sid: sid_of(p.key),
            window: usize::try_from(p.window).ok(),
            tab: Some(p.tab),
        })
        .collect();
    let caller_panes = q.caller.as_ref().map(|_| {
        caller_key.map_or_else(Vec::new, |c| {
            panes.iter().find(|p| p.key == c).map_or_else(
                || vec![c],
                |me| {
                    panes
                        .iter()
                        .filter(|p| p.window == me.window && p.tab == me.tab)
                        .map(|p| p.key)
                        .collect()
                },
            )
        })
    });
    let home = aterm_types::dirs::home_dir().map(|h| h.display().to_string());
    let (roots, disk, volume) = if q.view == View::Disk || q.view == View::Tab {
        let scope = q.effective_scope().unwrap_or(ScopeArg::Tab);
        let keys: Vec<u64> = match scope {
            ScopeArg::Pane => vec![session],
            ScopeArg::Tab => panes_of_tab,
            ScopeArg::Window => window_sessions,
            ScopeArg::Aterm | ScopeArg::Global => panes.iter().map(|p| p.key).collect(),
        };
        let mut cwds: Vec<PaneCwd> = Vec::new();
        for &k in &keys {
            let path = if k == session {
                cwd.clone()
            } else {
                terms
                    .iter()
                    .find(|(key, _)| *key == k)
                    .and_then(|(_, t)| cwd_of(t))
                    .or_else(|| shell_cwd(k))
            };
            if let Some(path) = path {
                cwds.push(PaneCwd {
                    key: k,
                    tab: panes.iter().find(|p| p.key == k).map(|p| p.tab),
                    path,
                });
            }
        }
        let tab_of = |k: u64| panes.iter().find(|p| p.key == k).map(|p| p.tab);
        let (roots, disk) = disk_roots(scope, &keys, &cwds, &tab_of);
        let volume = if q.view == View::Disk {
            cwd.as_deref()
                .and_then(|c| volume_facts(std::path::Path::new(c)))
        } else {
            None
        };
        (roots, disk, volume)
    } else {
        (Vec::new(), None, None)
    };
    Gathered {
        target,
        tabs,
        roots,
        disk,
        volume,
        caller_panes,
        home,
    }
}

/// One pane of the asked scope and the folder its shell works in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneCwd {
    /// The session's local id (`diskwalk`'s tab key).
    pub(crate) key: u64,
    /// Its tab, 1-based.
    pub(crate) tab: Option<usize>,
    /// Its working directory.
    pub(crate) path: String,
}

/// THE WALKER SEAM (design §10.4, §10.5, §13.2): the asked scope's roots and what the disk walker (`crate::diskwalk_host`) knows of them, read from a control worker without waiting for any walk.
pub(crate) fn disk_roots(
    scope: ScopeArg,
    keys: &[u64],
    cwds: &[PaneCwd],
    tab_of: &dyn Fn(u64) -> Option<usize>,
) -> (Vec<Root>, Option<DiskTotals>) {
    use crate::diskwalk_host::{DISKWALK, Detail, DiskScope, WalkerState, disk_report};
    if DISKWALK.walker_state() != WalkerState::Running {
        return (cwd_roots(cwds, RootSize::NotSampled), None);
    }
    let now = Instant::now();
    let scopes: Vec<DiskScope> = match scope {
        ScopeArg::Aterm | ScopeArg::Global => vec![DiskScope::Aterm],
        _ => keys.iter().map(|&k| DiskScope::Tab(k)).collect(),
    };
    let reports: Vec<_> = scopes
        .into_iter()
        .map(|sc| disk_report(sc, Detail::Sizes, now))
        .collect();
    walker_roots(&reports, cwds, tab_of, now)
}

/// Each pane's working directory as one `cwd` root of `size` (two panes in
/// one folder are one root).
fn cwd_roots(cwds: &[PaneCwd], size: RootSize) -> Vec<Root> {
    let mut roots: Vec<Root> = Vec::new();
    for c in cwds {
        if let Some(r) = roots.iter_mut().find(|r| r.path == c.path) {
            r.keys.push(c.key);
            continue;
        }
        roots.push(Root {
            kinds: vec!["cwd"],
            path: c.path.clone(),
            keys: vec![c.key],
            tab: c.tab,
            size: size.clone(),
            ..Root::default()
        });
    }
    roots
}

/// The walker's reports as `sys` rows and the scope's balance.
pub(crate) fn walker_roots(
    reports: &[crate::diskwalk_host::DiskReport],
    cwds: &[PaneCwd],
    tab_of: &dyn Fn(u64) -> Option<usize>,
    now: Instant,
) -> (Vec<Root>, Option<DiskTotals>) {
    use crate::diskwalk_host::DiskScope;
    let mut roots: Vec<Root> = Vec::new();
    let mut warnings: Vec<aterm_vitals::sys::HugeDirFact> = Vec::new();
    let mut walking = false;
    let mut exclusive = 0u64;
    let mut growth: Option<i64> = None;
    for report in reports {
        walking |= report.walking;
        exclusive = exclusive.saturating_add(report.exclusive_bytes);
        if let Some(g) = report.growth
            && let Some(bps) = growth_bps(g)
        {
            growth = Some(growth.unwrap_or(0).saturating_add(bps));
        }
        if report.unknown_tab
            && let DiskScope::Tab(key) = report.scope
        {
            let queued = RootSize::Walking {
                walked_entries: 0,
                partial_alloc: None,
                last: None,
            };
            for r in cwd_roots(
                &cwds
                    .iter()
                    .filter(|c| c.key == key)
                    .cloned()
                    .collect::<Vec<_>>(),
                queued,
            ) {
                if !roots.iter().any(|have| have.path == r.path) {
                    roots.push(r);
                }
            }
            walking = true;
            continue;
        }
        let base = roots.len();
        let mut renumber: Vec<Option<usize>> = Vec::with_capacity(report.roots.len());
        for row in &report.roots {
            let path = row.claim.path.display().to_string();
            let mut claimers: Vec<u64> = match report.scope {
                DiskScope::Tab(k) => vec![k],
                DiskScope::Aterm => row.counted_in.into_iter().collect(),
            };
            claimers.extend(row.shared_with.iter().copied());
            claimers.sort_unstable();
            claimers.dedup();
            if let Some(i) = roots.iter().position(|r| r.path == path) {
                for k in claimers {
                    if !roots[i].keys.contains(&k) {
                        roots[i].keys.push(k);
                    }
                }
                renumber.push(Some(i + 1));
                continue;
            }
            renumber.push(Some(roots.len() + 1));
            let owner = match report.scope {
                DiskScope::Tab(k) => Some(k),
                DiskScope::Aterm => row.counted_in,
            };
            let mut tabs: Vec<usize> = row
                .shared_with
                .iter()
                .filter(|k| Some(**k) != owner)
                .filter_map(|&k| tab_of(k))
                .collect();
            tabs.sort_unstable();
            tabs.dedup();
            roots.push(Root {
                kinds: row.claim.kinds.iter().map(|k| k.word()).collect(),
                path,
                keys: claimers,
                tab: owner.and_then(tab_of),
                size: root_size(&row.record, now),
                dirs: row
                    .record
                    .complete
                    .as_ref()
                    .map(|c| {
                        c.biggest
                            .iter()
                            .map(|d| aterm_vitals::sys::DirSize {
                                rel: d.rel.display().to_string(),
                                alloc: d.alloc,
                                entries: d.entries,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                inside: None,
                shared_with: tabs,
                counted_in: row.counted_in.and_then(tab_of),
            });
        }
        for (row, at) in report.roots.iter().zip(&renumber) {
            if let (Some(inside), Some(at)) = (row.inside, at)
                && let Some(Some(parent)) = renumber.get(inside.saturating_sub(1))
                && *at > base
            {
                roots[*at - 1].inside = Some(*parent);
            }
        }
        for h in &report.warnings {
            let path = h.path.display().to_string();
            if warnings.iter().any(|w| w.path == path) {
                continue;
            }
            let root = roots
                .iter()
                .position(|r| h.path.starts_with(&r.path))
                .map(|i| i + 1);
            warnings.push(aterm_vitals::sys::HugeDirFact {
                path,
                entries: u64::from(h.entries),
                root,
            });
        }
    }
    let total = roots
        .iter()
        .filter(|r| r.inside.is_none())
        .filter(|r| {
            !roots
                .iter()
                .any(|o| o.path != r.path && std::path::Path::new(&r.path).starts_with(&o.path))
        })
        .map(|r| match &r.size {
            RootSize::Complete(c) => c.alloc,
            RootSize::Walking {
                partial_alloc,
                last,
                ..
            } => last
                .as_ref()
                .map(|l| l.alloc)
                .or(*partial_alloc)
                .unwrap_or(0),
            _ => 0,
        })
        .fold(0u64, u64::saturating_add);
    let (total, exclusive) = match reports {
        [one] if !one.unknown_tab => (one.total_bytes, one.exclusive_bytes.min(one.total_bytes)),
        _ => (total, exclusive.min(total)),
    };
    warnings.sort_by_key(|w| std::cmp::Reverse(w.entries));
    let totals = DiskTotals {
        total,
        exclusive,
        shared: total - exclusive,
        walking,
        growth_bps: growth,
        warnings,
    };
    (roots, Some(totals))
}

/// A walk's growth over its span, bytes per second; `None` over no span.
fn growth_bps(g: crate::diskwalk_host::Growth) -> Option<i64> {
    let ms = i64::try_from(g.over.as_millis())
        .ok()
        .filter(|&ms| ms > 0)?;
    Some(g.bytes.saturating_mul(1000) / ms)
}

/// One root's record as the seam's [`RootSize`].
fn root_size(r: &crate::diskwalk_host::RootRecord, now: Instant) -> RootSize {
    use crate::diskwalk_host::RootState;
    let sizes = |c: &crate::diskwalk_host::Complete| aterm_vitals::sys::RootSizes {
        alloc: c.sums.alloc_fair,
        logical: c.sums.logical,
        entries: c.sums.files.saturating_add(c.sums.dirs),
        direct_entries_max: c.direct_entries_max,
        growth_bps: c.growth.and_then(growth_bps),
        age_s: Some(now.saturating_duration_since(c.at).as_secs()),
    };
    match r.state {
        RootState::Complete => r
            .complete
            .as_ref()
            .map_or(RootSize::NotSampled, |c| RootSize::Complete(sizes(c))),
        RootState::Queued | RootState::Walking => RootSize::Walking {
            walked_entries: r.walked_entries,
            partial_alloc: r.partial.map(|p| p.alloc_fair),
            last: r.complete.as_ref().map(sizes),
        },
        RootState::OneLevel => RootSize::NotMeasured("too_broad"),
        RootState::NotMeasured(why) => RootSize::NotMeasured(refusal_word(why)),
    }
}

/// A walker refusal as one of `sys`'s disk unavailable words (§13.4).
fn refusal_word(why: aterm_diskwalk::Refusal) -> &'static str {
    use aterm_diskwalk::Refusal;
    match why {
        Refusal::Protected => "protected",
        Refusal::OtherUser => "other_user",
        Refusal::Remote | Refusal::Automounted | Refusal::MountsUnknown => "remote",
        Refusal::Removable => "removable",
        Refusal::Dataless => "dataless",
        Refusal::TooBroad => "too_broad",
        Refusal::Gone => "gone",
        Refusal::Unsupported => "unsupported",
        Refusal::TooDeep
        | Refusal::Link
        | Refusal::NotDirectory
        | Refusal::Unreadable(_)
        | Refusal::Relative => "refused",
    }
}

/// The volume holding `path`, read only where the bars would read it
/// (`vitals_host::tab_volume`: a complete mount table calls it local; on
/// Windows a fixed or RAM drive), with the disk watch's level for it.
fn volume_facts(path: &std::path::Path) -> Option<VolumeFacts> {
    if !path.is_absolute() {
        return None;
    }
    let reading = crate::vitals_host::tab_volume(path)?;
    let level = crate::disk_board::SHELF
        .board_lock()
        .row_at(&reading.mount)
        .map(|r| r.level.as_str().to_owned());
    Some(VolumeFacts {
        mount: reading.mount.display().to_string(),
        fs: reading.fs,
        total: reading.total,
        free: reading.free,
        available: reading.available,
        level,
    })
}

/// How a `since=` is answered.
struct Since {
    base: Option<Fingerprints>,
    events: Vec<Event>,
    gap: Option<Gap>,
}

fn plan_since(
    shared: &Shared,
    q: &SysQuery,
    head: Cursor,
    key: &ReplyKey,
) -> Result<Since, String> {
    let Some(c) = q.since else {
        return Ok(Since {
            base: None,
            events: Vec::new(),
            gap: None,
        });
    };
    if c.epoch != head.epoch {
        return Ok(Since {
            base: None,
            events: Vec::new(),
            gap: Some(Gap::Reset),
        });
    }
    if c.seq > head.seq {
        return Err(format!("ERR bad since head={head}\n"));
    }
    let (events, covered) = shared.events_since(c.seq);
    let events = if covered { events } else { Vec::new() };
    let base = shared.recall(&ReplyKey {
        seq: c.seq,
        ..key.clone()
    });
    let gap = base.is_none().then_some(Gap::Evicted);
    Ok(Since { base, events, gap })
}

/// `sys` on the control socket.
pub(crate) fn cmd_sys(
    store: &Store,
    session: u64,
    term: &Arc<Mutex<Terminal>>,
    master: i32,
    owner_class: bool,
    rest: &str,
) -> String {
    let q = match SysQuery::parse(rest) {
        Ok(q) => q,
        Err(usage) => return usage.to_owned(),
    };
    if q.name.is_some() {
        // `named=` stays not_sampled and so does the request.
        return "ERR unavailable name why=not_sampled\n".to_owned();
    }
    let fresh_ms = q.fresh_ms.unwrap_or_else(|| q.freshness_default_ms());
    let (held, state) = SYS.fresh(Duration::from_millis(fresh_ms), fresh_ms == 0);
    let (panes, machine) = SYS.facts();
    let g = gather(
        store,
        session,
        term,
        master,
        &panes,
        &q,
        held.as_ref().map(|h| h.snap.as_ref()),
    );
    answer(&SYS, &q, held.as_ref(), state, &machine, &g, owner_class)
}

/// The reply, from what was gathered.
fn answer(
    shared: &Shared,
    q: &SysQuery,
    held: Option<&Held>,
    state: Freshness,
    machine: &Machine,
    g: &Gathered,
    owner_class: bool,
) -> String {
    let head = Cursor {
        epoch: *EPOCH,
        seq: held.map_or(0, |h| h.snap.seq).max(shared.head_seq()),
    };
    let key = ReplyKey {
        seq: head.seq,
        view: q.view,
        scope: q.effective_scope(),
        target: g.target.key,
        top: q.top,
        depth: q.depth,
        caller_panes: g.caller_panes.clone(),
        owner_class,
    };
    let since = match plan_since(shared, q, head, &key) {
        Ok(s) => s,
        Err(refusal) => return refusal,
    };
    let ctx = Ctx {
        snap: held.map(|h| h.snap.as_ref()),
        epoch: *EPOCH,
        as_of_unix_ms: held.map_or(0, |h| h.unix_ms),
        age_ms: held.map_or(0, |h| {
            u64::try_from(h.at.elapsed().as_millis()).unwrap_or(u64::MAX)
        }),
        state,
        machine,
        target: &g.target,
        tabs: &g.tabs,
        caller_panes: g.caller_panes.as_deref(),
        owner_class,
        volume: g.volume.as_ref(),
        roots: &g.roots,
        disk: g.disk.as_ref(),
        home: g.home.as_deref(),
        events: &since.events,
        gap: since.gap,
        head_seq: head.seq,
    };
    if q.json {
        let mut reply = Reply {
            view: q.view,
            scope: q.view.default_scope().and(q.effective_scope()),
            lines: Vec::new(),
            json: Some(render_json(q, &ctx)),
        };
        if q.instances_all
            && let Some(j) = reply.json.as_mut()
        {
            j.set("instances", aterm_vitals::sys::json::Jv::s("this"));
        }
        let body = reply
            .json
            .as_ref()
            .map(aterm_vitals::sys::json::Jv::to_text);
        return format!(
            "{}\n{}\n",
            reply.header(&ctx, false),
            body.unwrap_or_default()
        );
    }
    let mut reply = render_text(q, &ctx);
    if q.instances_all {
        reply.lines.push(aterm_vitals::sys::view::Line {
            key: "note".into(),
            text: "note   instances=all is folded by aterm ctl; this reply is one instance's"
                .into(),
            value: false,
        });
    }
    if held.is_some() {
        shared.remember(key, fingerprints(&reply));
    }
    let delta = since.base.as_ref().is_some_and(|base| {
        apply_delta(&mut reply, base);
        true
    });
    let mut out = reply.header(&ctx, delta);
    out.push('\n');
    for l in &reply.lines {
        out.push_str(&l.text);
        out.push('\n');
    }
    out
}

/// `await sys …` on the control socket: the machine terms park here; `free` and `disk=` alone go to the volume wait.
pub(crate) fn cmd_await_sys(
    term: &Arc<Mutex<Terminal>>,
    store: &Store,
    session: u64,
    scope: crate::control::Scope,
    rest: &str,
) -> String {
    match AwaitSys::parse(rest) {
        Ok(Parsed::Volume(wait)) => {
            crate::control::control_disk::cmd_await_sys(term, store, session, scope, wait)
        }
        Ok(Parsed::Machine(wait)) => {
            let was_registered = store
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .by_local(session)
                .is_some();
            let exited = || {
                let g = store.read().unwrap_or_else(PoisonError::into_inner);
                match g.by_local(session).map(|h| h.state) {
                    Some(crate::session_store::SessionState::Exited) => true,
                    None => was_registered,
                    _ => false,
                }
            };
            await_machine(&SYS, session, &wait, &exited)
        }
        Err(reply) => reply,
    }
}

/// How many engine passes after the one the wait armed on may go by with a
/// named job in neither the view nor the ring before the wait says it is
/// gone: a job still starting (an agent's task id the engine has not bound
/// yet) gets a pass to show up in.
const GONE_AFTER_PASSES: u32 = 2;

/// One machine wait's reading of the snapshots it is handed.
#[derive(Default)]
struct MachineWait {
    latch: Latch,
    /// The jobs view last read, held so a new pass is told apart by
    /// identity, and how many distinct views were read.
    view: Option<Arc<aterm_vitals::jobs::JobsView>>,
    views: u32,
}

impl MachineWait {
    /// Read one new snapshot, the wait's sessions being `keys` and the jobs
    /// that left the view `ended`: the reply once the wait is answered (met,
    /// refused, or a named job gone).
    fn step(
        &mut self,
        wait: &AwaitSys,
        snap: &Snapshot,
        unix_ms: u64,
        keys: &[u64],
        ended: &[EndedJob],
    ) -> Option<String> {
        if let Some(v) = snap.jobs.as_ref()
            && !self.view.as_ref().is_some_and(|old| Arc::ptr_eq(old, v))
        {
            self.view = Some(Arc::clone(v));
            self.views = self.views.saturating_add(1);
        }
        let outcome = evaluate_with(wait, snap, keys, ended);
        if let Some(refusal) = outcome.refused {
            return Some(refusal);
        }
        if let Some(word) = outcome.absent.first()
            && self.views > GONE_AFTER_PASSES
        {
            return Some(format!("ERR gone {word}\n"));
        }
        let held_ms = self.latch.step(outcome.holds, snap.at_ns, wait.for_ms)?;
        let cursor = Cursor {
            epoch: *EPOCH,
            seq: snap.seq,
        };
        Some(met_reply(&outcome, held_ms, unix_ms, &cursor.to_string()))
    }
}

/// The sessions a machine wait from `session` reads: its tab's panes, or
/// every pane with `scope=aterm` (Owner-only, the access arm's).
fn wait_keys(panes: &[PaneFact], session: u64, every_tab: bool) -> Vec<u64> {
    if every_tab {
        return panes.iter().map(|p| p.key).collect();
    }
    panes.iter().find(|p| p.key == session).map_or_else(
        || vec![session],
        |me| {
            panes
                .iter()
                .filter(|p| p.window == me.window && p.tab == me.tab)
                .map(|p| p.key)
                .collect()
        },
    )
}

/// Whether `await <rest>` is a machine wait that reads every tab's jobs (`scope=aterm`): Owner-only (`control::argument_escalation`).
pub(crate) fn await_reads_every_tab(rest: &str) -> bool {
    matches!(AwaitSys::parse(rest), Ok(Parsed::Machine(w)) if w.scope_aterm)
}

/// THE MACHINE WAIT: each new snapshot re-evaluates the predicate; a pull
/// keeps them coming once a second while armed; `for=` holds over their
/// stamps.
fn names_slots(wait: &AwaitSys) -> bool {
    wait.terms
        .iter()
        .any(|t| t.metric == aterm_vitals::sys::Metric::Slots)
}

fn await_machine(
    shared: &Shared,
    session: u64,
    wait: &AwaitSys,
    exited: &dyn Fn() -> bool,
) -> String {
    let deadline = Instant::now() + Duration::from_millis(wait.timeout_ms);
    let _slots = names_slots(wait).then(|| crate::occupancy_host::SLOTS_ARMED.arm());
    let mut state = MachineWait::default();
    let mut seen: Option<u64> = None;
    loop {
        let (held, freshness) = shared.fresh(AWAIT_PERIOD, false);
        if freshness != Freshness::Fresh {
            // hold cannot bridge the interval no fresh sample measured.
            state.latch = Latch::default();
        }
        if let Some(h) = held.as_ref()
            && seen != Some(h.snap.seq)
        {
            seen = Some(h.snap.seq);
            if freshness == Freshness::Fresh {
                let (panes, _) = shared.facts();
                let keys = wait_keys(&panes, session, wait.scope_aterm);
                if let Some(reply) =
                    state.step(wait, &h.snap, h.unix_ms, &keys, &shared.ended_jobs())
                {
                    return reply;
                }
            }
        }
        if crate::control::caller_hung_up() {
            return crate::control::HUNG_UP_REPLY.to_string();
        }
        if exited() {
            return "ERR exited\n".to_owned();
        }
        let now = Instant::now();
        if now >= deadline {
            return "OK timeout\n".to_owned();
        }
        let next_pull = held
            .as_ref()
            .map_or(now + AWAIT_PERIOD, |h| h.at + AWAIT_PERIOD)
            .max(now + Duration::from_millis(50));
        shared.wait_newer(seen, next_pull.min(deadline));
    }
}

/// The timeline row a job's moved verdict is recorded as; `subscribe … events` pushes it as `EVENT <local> job …` (`subscribe::timeline_wire_kind`).
pub(crate) const JOB_CHANGE: &str = "job-change";

/// The `job-change` row's payload, the push's words after `EVENT <local> job`: `<verdict> job=<id> class=<class> rev=<n>`, then `task=<id>` for an agent's call and `code=<n>` once it exited.
pub(crate) fn job_event_payload(
    verdict: aterm_vitals::jobs::Verdict,
    id: &str,
    class: aterm_vitals::jobs::Class,
    rev: u64,
    task: Option<&str>,
    exit: Option<i32>,
) -> String {
    let mut out = format!(
        "{} job={} class={} rev={rev}",
        verdict.word(),
        aterm_vitals::sys::pct(id),
        class.word()
    );
    if let Some(task) = task.map(aterm_vitals::sys::exe_word).filter(|t| *t != "-") {
        out.push_str(&format!(" task={task}"));
    }
    if let Some(code) = exit {
        out.push_str(&format!(" code={code}"));
    }
    out
}

/// The jobs view that may speak for now, from when it was read and whether
/// its reader runs (`reader`): one at most [`crate::vitals_jobs::JOBS_FRESH`]
/// old, or a parked one while its reader runs
/// ([`crate::vitals_jobs::speaks_for_now`]).
fn fresh_jobs(
    view: Option<(Arc<aterm_vitals::jobs::JobsView>, Instant)>,
    now: Instant,
    reader: bool,
) -> Option<Arc<aterm_vitals::jobs::JobsView>> {
    view.filter(|(v, at)| {
        crate::vitals_jobs::speaks_for_now(v, now.saturating_duration_since(*at), reader)
    })
    .map(|(v, _)| v)
}

/// `brief`'s jobs line for `session` (`jobs 2 jobs · 1 running · 1 stuck · worst: …`), from the workers' copy without pulling; `None` when no fresh view says the session has a live job.
pub(crate) fn brief_jobs_line(session: u64) -> Option<String> {
    let (view, at, reader) = SYS.jobs_view()?;
    let view = fresh_jobs(Some((view, at)), Instant::now(), reader)?;
    aterm_vitals::sys::jobs_view::brief_line(&view, &[session])
}

/// Put `line` into the brief reply `text` as its last line before the trailer's `gap why=…` and `more=` lines, its header saying the new line count, the budget asked (`budget`) and the whole reply's token estimate (`aterm_slog::tokens`, header included, as the brief's own assembly counts it).
pub(crate) fn splice_brief_line(text: &str, line: &str, budget: usize) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    let Some(more) = lines.iter().rposition(|l| l.starts_with("more=")) else {
        return text.to_owned();
    };
    let at = if more > 1 && lines[more - 1].starts_with("gap why=") {
        more - 1
    } else {
        more
    };
    if at == 0 {
        return text.to_owned();
    }
    lines.insert(at, line);
    let body = lines[1..].join("\n");
    let n = lines.len() - 1;
    let fields: Vec<&str> = lines[0]
        .split(' ')
        .skip(2)
        .filter(|f| !f.starts_with("used=") && !f.starts_with("budget="))
        .collect();
    let head = |used: usize| format!("OK {n} {} budget={budget} used={used}", fields.join(" "));
    let mut used = 0usize;
    for _ in 0..4 {
        let next = aterm_slog::tokens(head(used).len() + 1 + body.len());
        if next == used {
            break;
        }
        used = next;
    }
    format!("{}\n{body}", head(used))
}

impl crate::App {
    /// `status`'s `job=<verdict|none|-> job_quiet_ms=<ms|->` for `session`: its worst live job in the snapshot the loop adopted, when that is at most [`crate::vitals_jobs::JOBS_FRESH`] old, or parked while the probe thread runs (`-` otherwise: nothing read lately speaks for now).
    pub(crate) fn status_job_fields(&self, session: u64) -> String {
        let now = Instant::now();
        let view = self
            .vitals
            .snapshot
            .as_ref()
            .and_then(|s| s.jobs.clone())
            .zip(self.vitals.adopted_at);
        let age_ms = self.vitals.adopted_at.map_or(0, |at| {
            u64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(u64::MAX)
        });
        let view = fresh_jobs(view, now, self.vitals.reader_runs());
        aterm_vitals::sys::jobs_view::status_fields(view.as_deref(), &[session], age_ms)
    }
}

#[cfg(test)]
#[path = "sys_host_tests.rs"]
mod tests;
