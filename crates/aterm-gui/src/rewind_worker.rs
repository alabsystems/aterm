// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rewind fold thread: it lives while a view is open, folds one ask at a time from a cache of forks, and arms no timer.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};

use aterm_buffer::{Seq, Ticks};
use aterm_control::replay::ReplayEngine;
use aterm_core::config::TerminalConfig;
use aterm_core::terminal::Terminal;

use crate::temporal::{Judged, Passed, ReplayPlan, RewindInstant};

/// Lines past its 1000-line ring a view engine's history keeps (keyframes keep 2048), so a fork copies in bounded time.
pub(crate) const STORE_LINES: usize = 2048;

/// At most this many forks are cached.
pub(crate) const MAX_FORKS: usize = 32;

/// At most this many bytes of forks are cached.
pub(crate) const MAX_FORK_BYTES: usize = 32 * 1024 * 1024;

/// Output folded between the forks the cache keeps.
pub(crate) const FORK_EVERY: usize = 64 * 1024;

/// Which fold of which view a fold is; view ids are unique in the process, so a closed view's late fold names no later view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FoldToken {
    pub(crate) view: u64,
    pub(crate) serial: u64,
}

/// One fold of a view's instant: its plan, the plan of the witness that covers it when unjudged, and the colours to draw it in.
pub(crate) struct Show {
    pub(crate) session: u64,
    pub(crate) token: FoldToken,
    pub(crate) plan: ReplayPlan,
    pub(crate) judge: Option<ReplayPlan>,
    pub(crate) colors: Option<Arc<TerminalConfig>>,
}

/// A search for the last instant at or before `at` whose screen changed cell `cell` ([`crate::App::rewind_changed`]).
pub(crate) struct Changed {
    pub(crate) session: u64,
    pub(crate) token: FoldToken,
    /// Each replayable chain through `at`, oldest first ([`crate::temporal::TemporalRecorder::chain_plans`]).
    pub(crate) chains: Vec<ReplayPlan>,
    /// The steppable instants at or before `at`, oldest first.
    pub(crate) instants: Vec<RewindInstant>,
    /// The shown instant the search starts from.
    pub(crate) at: Ticks,
    pub(crate) cell: (u16, u16),
}

enum Job {
    Show(Show),
    Changed(Changed),
}

/// What the thread hands the main thread ([`crate::App::rewind_done`]).
pub(crate) enum Done {
    /// A fold to show, boxed since it holds a whole engine.
    Shown(Box<Folded>),
    /// The verdicts of a fold to the witness that covers a shown instant, run once the thread had nothing else to do.
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
    /// The verdicts of a judging fold run inline (a headless App folds everything inline).
    pub(crate) judged: Option<Judged>,
}

/// Where a cell last changed ([`Changed`]).
pub(crate) struct Found {
    pub(crate) session: u64,
    pub(crate) token: FoldToken,
    pub(crate) cell: (u16, u16),
    /// The instant to show: the last at or before the asked one whose slices changed the cell (the one before that when the asked instant is it), else the oldest kept one; `None` with nothing replayable.
    pub(crate) at: Option<Ticks>,
    /// Whether the slices on `at` changed the cell (`false`: it is as at the oldest kept state).
    pub(crate) changed: bool,
    /// The bytes of the slices on that instant, and the resize on it.
    pub(crate) bytes: Vec<u8>,
    pub(crate) resize: Option<(u16, u16)>,
}

/// A running fold thread; dropping it ends the thread once its fold in hand is done.
pub(crate) struct RewindWorker {
    tx: SyncSender<Job>,
}

impl RewindWorker {
    /// Start the thread, which hands each result to `post` (the main thread's wake) and stops once `post` says no one listens.
    pub(crate) fn start(post: impl Fn(Done) -> bool + Send + 'static) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("aterm-rewind".to_string())
            .spawn(move || run(&rx, &post))
            .map_err(|e| format!("cannot start the Rewind thread: {e}"))?;
        Ok(Self { tx })
    }

    /// Ask the thread to fold `show`.
    pub(crate) fn show(&self, show: Show) -> Result<(), String> {
        self.send(Job::Show(show))
    }

    /// Ask the thread where a cell last changed.
    pub(crate) fn changed(&self, changed: Changed) -> Result<(), String> {
        self.send(Job::Changed(changed))
    }

    fn send(&self, job: Job) -> Result<(), String> {
        match self.tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("a Rewind fold is already running".to_string()),
            Err(TrySendError::Disconnected(_)) => Err("the Rewind thread stopped".to_string()),
        }
    }
}

fn run(rx: &Receiver<Job>, post: &impl Fn(Done) -> bool) {
    let mut cache = Cache::default();
    let mut judging: Vec<(u64, ReplayPlan)> = Vec::new();
    let mut warm: Option<Warm> = None;
    loop {
        let job = match rx.try_recv() {
            Ok(job) => job,
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {
                if let Some((session, plan)) = judging.pop() {
                    let judged = plan.fold().judged;
                    if !post(Done::Judged { session, judged }) {
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
                match rx.recv() {
                    Ok(job) => job,
                    Err(_) => return,
                }
            }
        };
        let done = match job {
            Job::Show(show) => {
                let session = show.session;
                let (folded, judge, plan) = cache.show(show);
                judging.retain(|(s, _)| *s != session);
                judging.extend(judge.map(|plan| (session, plan)));
                warm = Some(Warm { plan, cursor: None });
                Done::Shown(Box::new(folded))
            }
            Job::Changed(changed) => Done::Changed(cache.changed(changed)),
        };
        if !post(done) {
            return;
        }
    }
}

/// The cache's fill toward a shown instant, a fork at a time while the thread has nothing else to do.
struct Warm {
    plan: ReplayPlan,
    cursor: Option<(ReplayEngine, Seq)>,
}

/// [`Show`] folded in place with its verdicts, for an App with no event-loop proxy (and for tests).
pub(crate) fn show_inline(show: Show) -> Folded {
    let (mut folded, judge, _) = Cache::default().show(show);
    folded.judged = judge.map(|plan| plan.fold().judged);
    folded
}

/// [`Changed`] searched in place, for an App with no event-loop proxy.
pub(crate) fn changed_inline(changed: Changed) -> Found {
    Cache::default().changed(changed)
}

/// Warm a fresh cache all the way toward `warm`'s instant, as an idle thread does, then fold `show` from it: the seqs of the forks it kept, their bytes, and the fold.
#[cfg(test)]
pub(crate) fn warmed_show(warm: ReplayPlan, show: Show) -> (Vec<Seq>, usize, Folded) {
    let mut cache = Cache::default();
    let mut warm = Warm {
        plan: warm,
        cursor: None,
    };
    while cache.warm(&mut warm) {}
    let seqs = cache.forks.iter().map(|c| c.seq).collect();
    let bytes = cache.bytes;
    let (folded, _, _) = cache.show(show);
    (seqs, bytes, folded)
}

/// A fresh cache told to keep a fork of `engine` after each of `seqs` on one chain, nearest `near`: the seqs it still holds, ascending.
#[cfg(test)]
pub(crate) fn kept_after(engine: &ReplayEngine, seqs: &[Seq], near: Seq) -> Vec<Seq> {
    let mut cache = Cache::default();
    for &seq in seqs {
        if let Some(fork) = engine.fork() {
            cache.keep(Seq(0), seq, fork, near);
        }
    }
    let mut kept: Vec<Seq> = cache.forks.iter().map(|c| c.seq).collect();
    kept.sort_unstable();
    kept
}

/// One cached fork: the chain it folds (its base keyframe's seq), the last step it applied, and its size.
struct Cached {
    chain: Seq,
    seq: Seq,
    engine: ReplayEngine,
    bytes: usize,
}

/// The forks a view's folds start from, at most [`MAX_FORKS`] and [`MAX_FORK_BYTES`].
#[derive(Default)]
struct Cache {
    forks: Vec<Cached>,
    bytes: usize,
}

impl Cache {
    /// Fold `show` from a copy of the nearest fork at or before its end, taking no fork on the way: [`Self::warm`] fills the cache after.
    fn show(&mut self, show: Show) -> (Folded, Option<ReplayPlan>, ReplayPlan) {
        let Show {
            session,
            token,
            plan,
            judge,
            colors,
        } = show;
        let (mut replay, from) = self.start(&plan);
        let (differs, _) = plan.fold_onto(&mut replay, from, |_, _| true);
        let mut term = replay.into_terminal();
        if let Some(colors) = colors.as_deref() {
            term.apply_config_colors(colors);
        }
        let mid_frame = term.end_synchronized_output();
        let folded = Folded {
            session,
            token,
            term,
            mid_frame,
            differs,
            judged: None,
        };
        (folded, judge, plan)
    }

    /// One step of `warm`: fold [`FORK_EVERY`] bytes on from the nearest fork toward its instant and keep a fork there; whether more is left.
    fn warm(&mut self, warm: &mut Warm) -> bool {
        let chain = warm.plan.base_seq();
        let end = warm.plan.end_seq();
        let (mut engine, at) = match warm.cursor.take() {
            Some(cursor) => cursor,
            None => {
                if self.nearest(chain, end).is_some_and(|c| c.seq == end) {
                    return false;
                }
                self.start(&warm.plan)
            }
        };
        let mut folded = 0;
        let (_, last) = warm.plan.fold_onto(&mut engine, at, |passed, _| {
            folded += passed.bytes.map_or(0, <[u8]>::len);
            folded < FORK_EVERY
        });
        if let Some(fork) = engine.fork() {
            self.keep(chain, last, fork, end);
        }
        let more = last < end && last > at;
        if more {
            warm.cursor = Some((engine, last));
        }
        more
    }

    /// The engine a fold of `plan` starts on, and the seq it stands after: a copy of the nearest cached fork, else the base restored.
    fn start(&self, plan: &ReplayPlan) -> (ReplayEngine, Seq) {
        self.nearest(plan.base_seq(), plan.end_seq())
            .and_then(|c| c.engine.fork().map(|engine| (engine, c.seq)))
            .unwrap_or_else(|| (plan.restore_bounded(STORE_LINES), plan.base_seq()))
    }

    fn nearest(&self, chain: Seq, end: Seq) -> Option<&Cached> {
        self.forks
            .iter()
            .filter(|c| c.chain == chain && c.seq <= end)
            .max_by_key(|c| c.seq)
    }

    /// Keep `engine` as `chain`'s fork after `seq`, then drop the forks farthest from `near` (another chain's first) while over the caps.
    fn keep(&mut self, chain: Seq, seq: Seq, engine: ReplayEngine, near: Seq) {
        if self.forks.iter().any(|c| c.chain == chain && c.seq == seq) {
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
                .max_by_key(|(_, c)| {
                    if c.chain == chain {
                        c.seq.0.abs_diff(near.0)
                    } else {
                        u64::MAX
                    }
                })
                .map(|(i, _)| i);
            let Some(far) = far else {
                break;
            };
            let gone = self.forks.swap_remove(far);
            self.bytes = self.bytes.saturating_sub(gone.bytes);
        }
    }

    /// Fold the chains in `changed` newest first, reading the cell at each steppable instant, back to the instant that changed it: the start of the run of instants that show it as the asked one does, or of the run before when the asked instant starts its own.
    fn changed(&mut self, changed: Changed) -> Found {
        let Changed {
            session,
            token,
            chains,
            instants,
            at,
            cell,
        } = changed;
        let mut found = Found {
            session,
            token,
            cell,
            at: None,
            changed: false,
            bytes: Vec::new(),
            resize: None,
        };
        let mut run: Option<(Option<CellWords>, Reading)> = None;
        let mut passed_asked = false;
        for (i, plan) in chains.iter().enumerate().rev() {
            let until = chains
                .get(i + 1)
                .map_or(Ticks(u64::MAX), ReplayPlan::base_ts);
            for reading in read_chain(plan, &instants, until, cell).into_iter().rev() {
                let Some((value, oldest)) = run.as_mut() else {
                    run = Some((reading.cell, reading));
                    continue;
                };
                if reading.cell == *value {
                    *oldest = reading;
                    continue;
                }
                if oldest.at != at || passed_asked {
                    let (_, start) = run.take().unwrap_or((None, reading));
                    found.at = Some(start.at);
                    found.changed = true;
                    found.bytes = start.bytes;
                    found.resize = start.resize;
                    return found;
                }
                passed_asked = true;
                run = Some((reading.cell, reading));
            }
        }
        if let Some((_, oldest)) = run {
            found.at = Some(oldest.at);
        }
        found
    }
}

/// The cell at one instant, and what the slices and resize on that instant were.
struct Reading {
    at: Ticks,
    cell: Option<CellWords>,
    bytes: Vec<u8>,
    resize: Option<(u16, u16)>,
}

/// Fold `plan`'s chain from its base, reading `cell` at each of `instants` in `[plan.base_ts(), until)`.
fn read_chain(
    plan: &ReplayPlan,
    instants: &[RewindInstant],
    until: Ticks,
    cell: (u16, u16),
) -> Vec<Reading> {
    let mut ahead = instants
        .iter()
        .filter(|i| i.at >= plan.base_ts() && i.at < until)
        .peekable();
    let mut readings = Vec::new();
    let mut bytes = Vec::new();
    let mut resize = None;
    let mut read = |before: Ticks,
                    engine: &ReplayEngine,
                    bytes: &mut Vec<u8>,
                    resize: &mut Option<(u16, u16)>| {
        while let Some(instant) = ahead.next_if(|i| i.at < before) {
            readings.push(Reading {
                at: instant.at,
                cell: cell_words(engine.terminal(), cell),
                bytes: std::mem::take(bytes),
                resize: resize.take(),
            });
        }
    };
    let mut replay = plan.restore_bounded(STORE_LINES);
    plan.fold_before(&mut replay, |passed: Passed<'_>, engine| {
        read(passed.ts, engine, &mut bytes, &mut resize);
        if let Some(b) = passed.bytes {
            bytes.extend_from_slice(b);
        }
        if passed.resize.is_some() {
            resize = passed.resize;
        }
    });
    read(Ticks(u64::MAX), &replay, &mut bytes, &mut resize);
    readings
}

/// A cell as the screen shows it: its glyph and its look.
type CellWords = aterm_core::terminal::RenderCell;

/// Cell `(row, col)` of `term`'s screen, `None` off it.
fn cell_words(term: &Terminal, (row, col): (u16, u16)) -> Option<CellWords> {
    if row >= term.rows() {
        return None;
    }
    term.render_row(usize::from(row))
        .get(usize::from(col))
        .copied()
}

/// The memory `engine` holds: both grids and their stores.
fn engine_bytes(engine: &ReplayEngine) -> usize {
    engine.terminal().grid().memory_used()
}
