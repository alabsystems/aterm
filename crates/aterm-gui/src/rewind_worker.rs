// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rewind fold thread: it lives while a view is open, folds one ask at a time from a cache of forks, and arms no timer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};

use aterm_buffer::{Seq, Ticks};
use aterm_control::replay::ReplayEngine;
use aterm_core::config::TerminalConfig;
use aterm_core::terminal::{RenderCell, Terminal};

use crate::temporal::{Judged, Passed, ReplayPlan, RewindInstant};

/// Lines past its 1000-line ring a view engine's history keeps (keyframes keep 2048), so a fork copies in bounded time.
pub(crate) const STORE_LINES: usize = 2048;

/// At most this many forks are cached.
pub(crate) const MAX_FORKS: usize = 32;

/// At most this many bytes of forks are cached.
pub(crate) const MAX_FORK_BYTES: usize = 32 * 1024 * 1024;

/// Output folded between the forks the cache keeps, and the most an idle pass folds before it looks for an ask.
pub(crate) const FORK_EVERY: usize = 64 * 1024;

/// Which fold of which view a fold is; view ids are unique in the process, so a closed view's late fold names no later view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FoldToken {
    pub(crate) view: u64,
    pub(crate) serial: u64,
}

/// One fold of a view's instant: its plan, the plan of the witness that covers it when unjudged, the chains to keep forks along, and the colours and scrollback to draw it with.
pub(crate) struct Show {
    pub(crate) session: u64,
    pub(crate) token: FoldToken,
    pub(crate) plan: ReplayPlan,
    pub(crate) judge: Option<ReplayPlan>,
    /// The whole chain the instant is on and the replayable one before it ([`crate::temporal::TemporalRecorder::warm_plans`]).
    pub(crate) around: Vec<ReplayPlan>,
    pub(crate) colors: Option<Arc<TerminalConfig>>,
}

/// A search for the last instant at or before `at` whose screen changed cell `cell` ([`crate::App::rewind_changed`]).
pub(crate) struct Changed {
    pub(crate) session: u64,
    pub(crate) token: FoldToken,
    /// Each replayable chain through `at`, oldest first ([`crate::temporal::TemporalRecorder::chain_plans`]).
    pub(crate) chains: Vec<ReplayPlan>,
    /// The steppable instants at or before `at`, oldest first, each with whether a live witness contradicts its replay ([`crate::temporal::TemporalRecorder::contradicted`]).
    pub(crate) instants: Vec<(RewindInstant, bool)>,
    /// The shown instant the search starts from.
    pub(crate) at: Ticks,
    pub(crate) cell: (u16, u16),
    /// Whether the last search for this cell found `at`, so this one looks past the change on it to the one before.
    pub(crate) skip_own: bool,
}

enum Job {
    Show(Show),
    Changed(Changed),
}

/// What the thread hands the main thread ([`crate::App::rewind_done`]).
pub(crate) enum Done {
    /// A fold to show, boxed since it holds a whole engine.
    Shown(Box<Folded>),
    /// Verdicts a fold toward the witness that covers a shown instant reached, a part at a time once the thread had nothing else to do.
    Judged { session: u64, judged: Judged },
    /// Where a cell last changed.
    Changed(Found),
}

impl std::fmt::Debug for Done {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shown(folded) => f
                .debug_struct("Shown")
                .field("session", &folded.session)
                .field("token", &folded.token)
                .field("mid_frame", &folded.mid_frame)
                .finish_non_exhaustive(),
            Self::Judged { session, .. } => f
                .debug_struct("Judged")
                .field("session", session)
                .finish_non_exhaustive(),
            Self::Changed(found) => f
                .debug_struct("Changed")
                .field("session", &found.session)
                .field("at", &found.at)
                .finish_non_exhaustive(),
        }
    }
}

/// A fold the thread finished, for the main thread to show.
pub(crate) struct Folded {
    pub(crate) session: u64,
    pub(crate) token: FoldToken,
    /// The replayed engine at the asked instant.
    pub(crate) term: Terminal,
    /// Whether the fold ended a mode-2026 frame open at the instant, whose half-drawn grid was never on screen.
    pub(crate) mid_frame: bool,
    /// Whether a live witness the fold passed saw another screen ([`ReplayPlan::fold_onto`]).
    pub(crate) differs: bool,
    /// The bytes of output the fold applied after the fork or keyframe it started from.
    pub(crate) folded: usize,
    /// The verdicts of a judging fold run inline (a headless App folds everything inline).
    pub(crate) judged: Option<Judged>,
}

/// What a cell search learned of the instant it shows ([`Found::change`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// The slices on the instant changed the cell.
    Yes,
    /// Nothing kept changed it: the instant is the oldest kept state.
    No,
    /// A live witness contradicts the replay before the instant, so where the cell changed before it is unknown.
    Unknown,
}

impl Change {
    /// The `rewind changed` reply's `changed=` word.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unknown => "unknown",
        }
    }
}

/// Where a cell last changed ([`Changed`]).
pub(crate) struct Found {
    pub(crate) session: u64,
    pub(crate) token: FoldToken,
    pub(crate) cell: (u16, u16),
    /// The instant to show: the last at or before the asked one whose slices changed the cell, else the oldest the search could trust; `None` with nothing replayable.
    pub(crate) at: Option<Ticks>,
    pub(crate) change: Change,
    /// The bytes of the slices on that instant, and the resize on it, when the change is theirs.
    pub(crate) bytes: Vec<u8>,
    pub(crate) resize: Option<(u16, u16)>,
    /// Whether `bytes` are every slice on that instant: a live keyframe stands in for one the spine keeps no bytes of.
    pub(crate) whole: bool,
    /// Whether the search passed over the asked instant's own change ([`Changed::skip_own`]).
    pub(crate) shown_changed: bool,
}

/// A running fold thread; dropping it ends the thread once its fold in hand is done, and ends a cell search in hand at once.
pub(crate) struct RewindWorker {
    tx: SyncSender<Job>,
    /// The newest search serial the main thread gave up on: a search at or below it stops and answers nothing.
    cancelled: Arc<AtomicU64>,
    /// Whether the thread has work in hand: a job, a judge or a warm not yet done.
    busy: Arc<AtomicBool>,
}

impl RewindWorker {
    /// Start the thread, which hands each result to `post` (the main thread's wake) and stops once `post` says no one listens.
    pub(crate) fn start(post: impl Fn(Done) -> bool + Send + 'static) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicU64::new(0));
        let busy = Arc::new(AtomicBool::new(false));
        let (seen, working) = (cancelled.clone(), busy.clone());
        std::thread::Builder::new()
            .name("aterm-rewind".to_string())
            .spawn(move || run(&rx, &post, &seen, &working))
            .map_err(|e| format!("cannot start the Rewind thread: {e}"))?;
        Ok(Self {
            tx,
            cancelled,
            busy,
        })
    }

    /// Whether the thread still has work in hand, so its verdicts or its fork cache are not yet done.
    pub(crate) fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    /// Ask the thread to fold `show`.
    pub(crate) fn show(&self, show: Show) -> Result<(), String> {
        self.send(Job::Show(show))
    }

    /// Ask the thread where a cell last changed.
    pub(crate) fn changed(&self, changed: Changed) -> Result<(), String> {
        self.send(Job::Changed(changed))
    }

    /// Stop the cell search `token` names and every earlier one, if the thread is still on it.
    pub(crate) fn cancel_search(&self, token: FoldToken) {
        self.cancelled.fetch_max(token.serial, Ordering::Release);
    }

    fn send(&self, job: Job) -> Result<(), String> {
        match self.tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("a Rewind fold is already running".to_string()),
            Err(TrySendError::Disconnected(_)) => Err("the Rewind thread stopped".to_string()),
        }
    }
}

impl Drop for RewindWorker {
    fn drop(&mut self) {
        self.cancelled.store(u64::MAX, Ordering::Release);
    }
}

fn run(rx: &Receiver<Job>, post: &impl Fn(Done) -> bool, cancelled: &AtomicU64, busy: &AtomicBool) {
    let mut cache = Cache::default();
    let mut judging: Option<Judging> = None;
    let mut warm: Option<Warm> = None;
    loop {
        let job = match rx.try_recv() {
            Ok(job) => job,
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {
                if let Some(j) = judging.as_mut() {
                    let (judged, more) = j.step();
                    let session = j.session;
                    if !more {
                        judging = None;
                    }
                    if !judged.is_empty() && !post(Done::Judged { session, judged }) {
                        return;
                    }
                    continue;
                }
                if let Some(w) = warm.as_mut() {
                    if !cache.warm(w) {
                        warm = None;
                    }
                    continue;
                }
                busy.store(false, Ordering::Release);
                let job = rx.recv();
                busy.store(true, Ordering::Release);
                match job {
                    Ok(job) => job,
                    Err(_) => return,
                }
            }
        };
        let done = match job {
            Job::Show(show) => {
                let session = show.session;
                let (folded, judge, around, near) = cache.show(show);
                if let Some(plan) = judge {
                    Judging::extend(&mut judging, session, plan);
                }
                warm = Some(Warm::new(around, near));
                Done::Shown(Box::new(folded))
            }
            Job::Changed(changed) => {
                let serial = changed.token.serial;
                let go_on = || cancelled.load(Ordering::Acquire) < serial;
                match search(changed, &go_on) {
                    Some(found) => Done::Changed(found),
                    None => continue,
                }
            }
        };
        if !post(done) {
            return;
        }
    }
}

/// A judging fold toward the witness that covers a shown instant, on an engine that keeps its whole history, [`FORK_EVERY`] bytes a pass.
struct Judging {
    session: u64,
    plan: ReplayPlan,
    engine: ReplayEngine,
    at: Seq,
}

impl Judging {
    /// Judge `plan` too: on `judging`'s chain, carry its fold on to the further witness; on another, start over.
    fn extend(judging: &mut Option<Self>, session: u64, plan: ReplayPlan) {
        match judging.as_mut() {
            Some(j) if j.plan.base_seq() == plan.base_seq() => {
                if plan.end_seq() > j.plan.end_seq() {
                    j.plan = plan;
                }
            }
            _ => {
                *judging = Some(Self {
                    session,
                    engine: plan.restore(),
                    at: plan.base_seq(),
                    plan,
                });
            }
        }
    }

    /// One pass: the verdicts it reached and whether steps are left.
    fn step(&mut self) -> (Judged, bool) {
        let (judged, last, more) = self.plan.judge_onto(&mut self.engine, self.at, FORK_EVERY);
        self.at = last;
        (judged, more)
    }
}

/// The cache's fill around a shown instant: the forks nearest it that [`Cache::keep`] would hold, one fold pass at a time while the thread has nothing else to do.
struct Warm {
    plans: Vec<ReplayPlan>,
    /// The shown instant's seq, which the forks kept are nearest.
    near: Seq,
    /// The forks wanted, nearest `near` first: the plan, the seq, and whether a pass has reached it.
    wanted: Vec<(usize, Seq, bool)>,
}

impl Warm {
    fn new(plans: Vec<ReplayPlan>, near: Seq) -> Self {
        let mut wanted: Vec<(usize, Seq, bool)> = plans
            .iter()
            .enumerate()
            .flat_map(|(i, plan)| {
                plan.fork_marks(FORK_EVERY)
                    .into_iter()
                    .map(move |seq| (i, seq, false))
            })
            .collect();
        wanted.sort_by_key(|(_, seq, _)| seq.0.abs_diff(near.0));
        wanted.truncate(MAX_FORKS);
        Self {
            plans,
            near,
            wanted,
        }
    }
}

/// [`Show`] folded in place with its verdicts, for an App with no event-loop proxy (and for tests).
pub(crate) fn show_inline(show: Show) -> Folded {
    let (mut folded, judge, _, _) = Cache::default().show(show);
    folded.judged = judge.map(|plan| plan.fold().judged);
    folded
}

/// [`Changed`] searched in place, for an App with no event-loop proxy.
pub(crate) fn changed_inline(changed: Changed) -> Found {
    let (session, token, cell) = (changed.session, changed.token, changed.cell);
    search(changed, &|| true).unwrap_or(Found {
        session,
        token,
        cell,
        at: None,
        change: Change::No,
        bytes: Vec::new(),
        resize: None,
        whole: true,
        shown_changed: false,
    })
}

/// Warm a fresh cache all the way around `show`'s instant, as an idle thread does, then fold `show` from it: the seqs of the forks it kept, their bytes, and the fold.
#[cfg(test)]
pub(crate) fn warmed_show(mut show: Show) -> (Vec<Seq>, usize, Folded) {
    let mut cache = Cache::default();
    let mut warm = Warm::new(std::mem::take(&mut show.around), show.plan.end_seq());
    while cache.warm(&mut warm) {}
    show.around = warm.plans;
    let seqs = cache.forks.iter().map(|c| c.seq).collect();
    let bytes = cache.bytes;
    let (folded, _, _, _) = cache.show(show);
    (seqs, bytes, folded)
}

/// A view that opens on the first of `shows` and steps to each of the rest, an idle thread warming the cache around each state before the next step: each step's fold.
#[cfg(test)]
pub(crate) fn warmed_shows(shows: Vec<Show>) -> Vec<Folded> {
    let mut cache = Cache::default();
    let mut out = Vec::new();
    for show in shows {
        let (folded, _, around, near) = cache.show(show);
        let mut warm = Warm::new(around, near);
        while cache.warm(&mut warm) {}
        out.push(folded);
    }
    out.split_off(1.min(out.len()))
}

/// A fresh cache on `plan`'s chain holding a fork after `fork` and a warm front after `front`, warmed around `near` to its end or `cap` passes: the passes it took, `None` past the cap.
#[cfg(test)]
pub(crate) fn warm_passes(
    plan: ReplayPlan,
    fork: Seq,
    front: Seq,
    near: Seq,
    cap: usize,
) -> Option<usize> {
    let chain = plan.base_seq();
    let after = |to: Seq| {
        let mut engine = plan.restore_bounded(STORE_LINES);
        let _ = plan.fold_onto(&mut engine, chain, |passed, _| passed.seq < to);
        engine
    };
    let mut cache = Cache::default();
    if let Some(engine) = after(fork).fork() {
        cache.keep(chain, fork, engine, near);
    }
    cache.fronts.push((chain, after(front), front));
    let mut warm = Warm::new(vec![plan], near);
    (1..=cap).find(|_| !cache.warm(&mut warm))
}

/// A fresh cache told to keep a fork of `engine` after each of `seqs` on one chain, nearest `near`: the seqs it still holds, ascending, the bytes it books, and the sum of its forks' sizes.
#[cfg(test)]
pub(crate) fn kept_after(
    engine: &ReplayEngine,
    seqs: &[Seq],
    near: Seq,
) -> (Vec<Seq>, usize, usize) {
    let mut cache = Cache::default();
    for &seq in seqs {
        if let Some(fork) = engine.fork() {
            cache.keep(Seq(0), seq, fork, near);
        }
    }
    let mut kept: Vec<Seq> = cache.forks.iter().map(|c| c.seq).collect();
    kept.sort_unstable();
    let summed = cache.forks.iter().map(|c| c.bytes).sum();
    (kept, cache.bytes, summed)
}

/// The memory [`Cache`] books for `engine`.
#[cfg(test)]
pub(crate) fn fork_bytes(engine: &ReplayEngine) -> usize {
    engine_bytes(engine)
}

/// One cached fork: the chain it folds (its base keyframe's seq), the last step it applied, and its size.
struct Cached {
    chain: Seq,
    seq: Seq,
    engine: ReplayEngine,
    bytes: usize,
}

/// The forks a view's folds start from, at most [`MAX_FORKS`] and [`MAX_FORK_BYTES`], and the engines the warm last folded on each chain it warms, which it carries on from across asks.
#[derive(Default)]
struct Cache {
    forks: Vec<Cached>,
    bytes: usize,
    /// The warm's own engines, one a chain: the chain, the engine and the seq it stands after.
    fronts: Vec<(Seq, ReplayEngine, Seq)>,
}

impl Cache {
    /// Fold `show` from a copy of the nearest fork at or before its end, taking no fork on the way: [`Self::warm`] fills the cache after. Returns the fold, the plan to judge, and what to warm around.
    fn show(&mut self, show: Show) -> (Folded, Option<ReplayPlan>, Vec<ReplayPlan>, Seq) {
        let Show {
            session,
            token,
            plan,
            judge,
            around,
            colors,
        } = show;
        let (mut replay, from) = self.start(&plan);
        let mut folded = 0usize;
        let (differs, _) = plan.fold_onto(&mut replay, from, |passed, _| {
            folded = folded.saturating_add(passed.bytes.map_or(0, <[u8]>::len));
            true
        });
        let mut term = replay.into_terminal();
        if let Some(colors) = colors.as_deref() {
            term.apply_config_colors(colors);
            if colors.scrollback_limit.is_some() {
                term.set_scrollback_line_limit(colors.scrollback_limit);
            }
        }
        let mid_frame = term.end_synchronized_output();
        let folded = Folded {
            session,
            token,
            term,
            mid_frame,
            differs,
            folded,
            judged: None,
        };
        (folded, judge, around, plan.end_seq())
    }

    /// One pass of `warm`: fold at most [`FORK_EVERY`] bytes toward the nearest wanted fork not yet held, from its chain's warm front when that stands nearer than any fork, keeping each wanted fork it reaches; whether more is left. A pass that stops short of its fork leaves its engine as the chain's front, so the next carries on from it.
    fn warm(&mut self, warm: &mut Warm) -> bool {
        self.fronts
            .retain(|(chain, _, _)| warm.plans.iter().any(|p| p.base_seq() == *chain));
        let Some(&(idx, to, _)) = warm.wanted.iter().find(|&&(i, seq, reached)| {
            !reached
                && self.would_keep(seq, warm.near)
                && warm
                    .plans
                    .get(i)
                    .is_some_and(|plan| !self.holds(plan.base_seq(), seq))
        }) else {
            return false;
        };
        let Some(plan) = warm.plans.get(idx) else {
            return false;
        };
        let chain = plan.base_seq();
        let from_fork = self.nearest(chain, to).map(|c| c.seq);
        let front = self
            .fronts
            .iter()
            .position(|&(c, _, at)| c == chain && at <= to && from_fork.is_none_or(|f| at > f));
        let (mut engine, at) = match front {
            Some(i) => {
                let (_, engine, at) = self.fronts.swap_remove(i);
                (engine, at)
            }
            None => self.start_before(plan, to),
        };
        if at == to {
            if let Some(fork) = engine.fork() {
                self.keep(chain, to, fork, warm.near);
            }
            for entry in &mut warm.wanted {
                entry.2 |= entry.0 == idx && entry.1 == to;
            }
            self.fronts.push((chain, engine, at));
            return true;
        }
        let mut folded = 0usize;
        let mut forks = Vec::new();
        let (wanted, near) = (&warm.wanted, warm.near);
        let held = &*self;
        let (_, last) = plan.fold_onto(&mut engine, at, |passed: Passed<'_>, engine| {
            folded = folded.saturating_add(passed.bytes.map_or(0, <[u8]>::len));
            let is_wanted = wanted
                .iter()
                .any(|&(i, seq, _)| i == idx && seq == passed.seq);
            if is_wanted && !held.holds(chain, passed.seq) && held.would_keep(passed.seq, near) {
                forks.push((passed.seq, engine.fork()));
            }
            passed.seq < to && folded < FORK_EVERY
        });
        for entry in &mut warm.wanted {
            if entry.0 == idx
                && ((entry.1 > at && entry.1 <= last) || (last <= at && entry.1 == to))
            {
                entry.2 = true;
            }
        }
        for (seq, fork) in forks {
            if let Some(fork) = fork {
                self.keep(chain, seq, fork, warm.near);
            }
        }
        let ahead = self
            .fronts
            .iter()
            .any(|&(c, _, seq)| c == chain && seq >= last);
        if last > at && (last < to || !ahead) {
            self.fronts.retain(|(c, _, _)| *c != chain);
            self.fronts.push((chain, engine, last));
        }
        true
    }

    /// Whether a fork after `seq`, as large as the largest held, would outlast [`Self::keep`]'s eviction around `near`.
    fn would_keep(&self, seq: Seq, near: Seq) -> bool {
        let size = self.forks.iter().map(|c| c.bytes).max().unwrap_or(0);
        let full =
            self.forks.len() >= MAX_FORKS || self.bytes.saturating_add(size) > MAX_FORK_BYTES;
        let distance = seq.0.abs_diff(near.0);
        !full
            || self
                .forks
                .iter()
                .any(|c| c.seq.0.abs_diff(near.0) > distance)
    }

    /// Whether the cache holds a fork of `chain` after `seq`.
    fn holds(&self, chain: Seq, seq: Seq) -> bool {
        self.forks.iter().any(|c| c.chain == chain && c.seq == seq)
    }

    /// The engine a fold of `plan` starts on, and the seq it stands after: a copy of the nearest cached fork, else the base restored.
    fn start(&self, plan: &ReplayPlan) -> (ReplayEngine, Seq) {
        self.start_before(plan, plan.end_seq())
    }

    /// [`Self::start`] for a fold of `plan` that ends at `end`, from a copy of the chain's warm front when it stands nearer than any fork.
    fn start_before(&self, plan: &ReplayPlan, end: Seq) -> (ReplayEngine, Seq) {
        let chain = plan.base_seq();
        let fork = self.nearest(chain, end).map(|c| (&c.engine, c.seq));
        let front = self
            .fronts
            .iter()
            .filter(|&&(c, _, at)| c == chain && at <= end)
            .map(|(_, engine, at)| (engine, *at));
        let mut from: Vec<(&ReplayEngine, Seq)> = fork.into_iter().chain(front).collect();
        from.sort_by_key(|&(_, at)| std::cmp::Reverse(at));
        from.into_iter()
            .find_map(|(engine, at)| engine.fork().map(|engine| (engine, at)))
            .unwrap_or_else(|| (plan.restore_bounded(STORE_LINES), chain))
    }

    fn nearest(&self, chain: Seq, end: Seq) -> Option<&Cached> {
        self.forks
            .iter()
            .filter(|c| c.chain == chain && c.seq <= end)
            .max_by_key(|c| c.seq)
    }

    /// Keep `engine` as `chain`'s fork after `seq`, then drop the forks farthest from `near` on the spine, whatever their chain, while over the caps.
    fn keep(&mut self, chain: Seq, seq: Seq, engine: ReplayEngine, near: Seq) {
        if self.holds(chain, seq) {
            return;
        }
        let bytes = engine_bytes(&engine);
        self.bytes = self.bytes.saturating_add(bytes);
        self.forks.push(Cached {
            chain,
            seq,
            engine,
            bytes,
        });
        while self.forks.len() > MAX_FORKS || self.bytes > MAX_FORK_BYTES {
            let far = self
                .forks
                .iter()
                .enumerate()
                .max_by_key(|(_, c)| c.seq.0.abs_diff(near.0))
                .map(|(i, _)| i);
            let Some(far) = far else {
                break;
            };
            let gone = self.forks.swap_remove(far);
            self.bytes = self.bytes.saturating_sub(gone.bytes);
        }
    }
}

/// Fold the chains in `changed` newest first, reading the cell at each steppable instant, back to the instant that changed it: the start of the run of instants that show it as the asked one does, or of the run before when the asked instant starts its own and the last search found it. The walk stops at an instant a live witness contradicts, and gives up with `None` once `go_on` says no.
fn search(changed: Changed, go_on: &dyn Fn() -> bool) -> Option<Found> {
    let Changed {
        session,
        token,
        chains,
        instants,
        at,
        cell,
        skip_own,
    } = changed;
    let mut found = Found {
        session,
        token,
        cell,
        at: None,
        change: Change::No,
        bytes: Vec::new(),
        resize: None,
        whole: true,
        shown_changed: false,
    };
    let mut run: Option<(Option<CellWords>, Reading)> = None;
    for (i, plan) in chains.iter().enumerate().rev() {
        let newer = chains.get(i + 1).map(ReplayPlan::base_ts);
        let (readings, tail) = read_chain(
            plan,
            &instants,
            newer.unwrap_or(Ticks(u64::MAX)),
            cell,
            go_on,
        )?;
        if let Some((_, oldest)) = run.as_mut()
            && newer == Some(oldest.at)
        {
            let mut bytes = tail.bytes;
            bytes.extend_from_slice(&oldest.bytes);
            oldest.bytes = bytes;
            oldest.resize = oldest.resize.or(tail.resize);
        }
        for reading in readings.into_iter().rev() {
            let Some((value, oldest)) = run.as_mut() else {
                run = Some((reading.cell.clone(), reading));
                continue;
            };
            if reading.contradicted {
                found.at = Some(oldest.at);
                found.change = Change::Unknown;
                return Some(found);
            }
            if reading.cell == *value {
                *oldest = reading;
                continue;
            }
            if oldest.at != at || found.shown_changed || !skip_own {
                let (_, start) = run.take().unwrap_or((None, reading));
                found.at = Some(start.at);
                found.change = Change::Yes;
                found.bytes = start.bytes;
                found.resize = start.resize;
                found.whole = start.whole;
                return Some(found);
            }
            found.shown_changed = true;
            run = Some((reading.cell.clone(), reading));
        }
    }
    if let Some((_, oldest)) = run {
        found.at = Some(oldest.at);
    }
    Some(found)
}

/// The cell at one instant, what the slices and resize on that instant were, and whether a live witness contradicts its replay.
struct Reading {
    at: Ticks,
    cell: Option<CellWords>,
    bytes: Vec<u8>,
    resize: Option<(u16, u16)>,
    whole: bool,
    contradicted: bool,
}

/// The slices and resize a chain folds after its last instant read, which land on the next chain's first instant.
struct Tail {
    bytes: Vec<u8>,
    resize: Option<(u16, u16)>,
}

/// Fold `plan`'s chain from its base, reading `cell` at each of `instants` in `[plan.base_ts(), until)`, and what it folds after the last of them; `None` once `go_on` says no.
fn read_chain(
    plan: &ReplayPlan,
    instants: &[(RewindInstant, bool)],
    until: Ticks,
    cell: (u16, u16),
    go_on: &dyn Fn() -> bool,
) -> Option<(Vec<Reading>, Tail)> {
    let mut ahead = instants
        .iter()
        .filter(|(i, _)| i.at >= plan.base_ts() && i.at < until)
        .peekable();
    let mut readings = Vec::new();
    let mut tail = Tail {
        bytes: Vec::new(),
        resize: None,
    };
    let base = (plan.base_ts(), plan.live_base());
    let mut read = |before: Ticks, engine: &ReplayEngine, tail: &mut Tail| {
        while let Some((instant, contradicted)) = ahead.next_if(|(i, _)| i.at < before) {
            readings.push(Reading {
                at: instant.at,
                cell: cell_words(engine.terminal(), cell),
                bytes: std::mem::take(&mut tail.bytes),
                resize: tail.resize.take(),
                whole: !(base.1 && instant.at == base.0),
                contradicted: *contradicted,
            });
        }
    };
    let mut replay = plan.restore_bounded(STORE_LINES);
    let whole = plan.fold_before(&mut replay, |passed: Passed<'_>, engine| {
        if !go_on() {
            return false;
        }
        read(passed.ts, engine, &mut tail);
        if let Some(b) = passed.bytes {
            tail.bytes.extend_from_slice(b);
        }
        if passed.resize.is_some() {
            tail.resize = passed.resize;
        }
        true
    });
    if !whole {
        return None;
    }
    read(until, &replay, &mut tail);
    Some((readings, tail))
}

/// A cell as the screen shows it: its look and its whole grapheme, marks and sequence tails included, and for the right half of a wide glyph the lead cell's too.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CellWords {
    look: RenderCell,
    text: String,
    lead: Option<(RenderCell, String)>,
}

/// Cell `(row, col)` of `term`'s screen, a blank past the written part of its row reading as the blank the screen draws there; `None` off the screen.
fn cell_words(term: &Terminal, (row, col): (u16, u16)) -> Option<CellWords> {
    if row >= term.rows() || col >= term.cols() {
        return None;
    }
    let cells = term.render_row(usize::from(row));
    let at = |col: u16| {
        let look = cells
            .get(usize::from(col))
            .copied()
            .unwrap_or_else(|| term.implicit_blank_render_cell());
        let text = term
            .cell_grapheme(usize::from(row), usize::from(col))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| " ".to_string());
        (look, text)
    };
    let (look, text) = at(col);
    let lead = (look.wide && col > 0).then(|| at(col - 1));
    Some(CellWords { look, text, lead })
}

/// The memory `engine` holds: both grids and their stores.
fn engine_bytes(engine: &ReplayEngine) -> usize {
    let term = engine.terminal();
    term.grid().memory_used().saturating_add(
        term.inactive_grid()
            .map_or(0, aterm_core::grid::Grid::memory_used),
    )
}
