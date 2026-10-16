// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DOWNLOAD AHEAD: the set completion's next archives, fetched while the pass stages the
//! one in hand.
//!
//! The set completion lands one install unit at a time on the pass's thread — download,
//! digest, extract, flip — and the link sat idle through every digest, extract and flip
//! (measured on a first run: the trust archive's extract and the trust flip alone left it
//! idle for about ten seconds, with the rest of the set still to fetch). One thread of its
//! own now fetches the plan's archives in the order the pass will want them, at most
//! [`AHEAD_BYTES`] ahead of it, and the pass takes each one when it reaches the member
//! ([`claim`]).
//!
//! It moves BYTES only, through the same lane, into the same file, under the same cap as the
//! pass would: [`super::fetch_artifact`] with the signed row's `size`, into
//! `staging/<program>/<asset>` and its `.part`. The pass verifies what it takes exactly as
//! it verifies its own download — the signed digest before any parse, the signed
//! `tree_root` after the extract ([`super::verify_and_stage`]) — so a prefetched archive is
//! never a shortcut past a gate. Everything that moves the store stays on the pass's thread:
//! the stage, the flip, the status rows, the fault tally. A download that failed here comes
//! back to the pass as its own fetch failure and is counted there (`note_fetch_fault`), so
//! the offline verdict reads one tally as before.
//!
//! What it fetches is decided before any byte moves ([`Ahead::plan`]): every member of a
//! unit, admitted as the pass will admit it (the pin, the verified manifest bound to it, the
//! row, its apply strategy, its digest-refusal memo), or nothing of that unit. Which comes
//! next follows the pass: the unit it is in first, then the units it has yet to reach in
//! plan order re-sorted by the bump file — the reorder the pass applies between units — so
//! a bump moves only what has not started. An archive bigger than the budget is fetched
//! only once the pass asks for it, which bounds what this ever holds on disk beyond the
//! pass's own unit ([`Ahead::reserve`]).
//!
//! The moment its last archive is in is news to the pass's other lanes: the link is then
//! free of the set's bytes, and the set completion's agents take it while the pass is still
//! extracting and flipping ([`Ahead::all_fetched`], asked after every download that lands).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use super::{
    Fetcher, LandingBytes, digest_refusal_note, disk_gate, fetch_artifact, make_staging_dir,
    staged_download_path, sweep_foreign_partials, verified_pkg_or_miss,
};
use crate::gate::{ApplyDecision, decide};
use crate::manifest::Channel;
use crate::sig::TrustedIndex;
use crate::store::Layout;

/// The most bytes fetched ahead of the pass and not yet taken by it. Enough for the rest of
/// the ALab set (about 360 MB beside the trust archive) to arrive while the trust archive
/// extracts and the toolchain flips; an archive bigger than this waits for the pass.
const AHEAD_BYTES: u64 = 512 << 20;

/// One archive the pass will want: a member's signed row, admitted as the pass admits it.
struct Job {
    /// The plan unit the member belongs to (its group's index in the plan).
    unit: usize,
    program: String,
    pinned: u64,
    repo: String,
    artifact: crate::manifest::Artifact,
    dl: PathBuf,
}

/// Where one [`Job`] stands.
enum Slot {
    /// Not started.
    Queued,
    /// Not started, and the pass is waiting for it: fetched next, whatever the budget says.
    Wanted,
    /// Downloading.
    Running,
    /// Downloaded, or failed; not yet taken by the pass.
    Done(Result<(), String>),
    /// The pass has it: the answer, or the download itself when it fetched inline.
    Taken,
    /// Never fetched here — skipped (the pass's own gates decide the member), or left behind
    /// by a unit the pass is done with.
    Dropped,
}

struct State {
    slots: Vec<Slot>,
    /// Whether the pass asked for each job: a claimed job's bytes are the pass's own unit's,
    /// no longer ahead of it.
    claimed: Vec<bool>,
    /// The unit of the pass's last claim — the one it is staging.
    current: Option<usize>,
    /// The units the pass is done with.
    passed: BTreeSet<usize>,
    /// Whether this lane's download of each job succeeded ([`Ahead::all_fetched`]).
    fetched: Vec<bool>,
    /// No more downloads start: the pass ended, a download failed, or the thread is gone.
    halted: bool,
}

/// The set completion's download-ahead lane: the jobs, and where each stands. The pass
/// thread enters it ([`Ahead::enter`]) and one thread works it ([`Ahead::work`]).
pub(crate) struct Ahead {
    jobs: Vec<Job>,
    /// The most bytes fetched ahead and not yet taken ([`AHEAD_BYTES`]).
    budget: u64,
    state: Mutex<State>,
    changed: Condvar,
}

/// What the worker does next ([`Ahead::next`]).
enum Next {
    Fetch(usize),
    /// Woken: read the bump file again and look again.
    Again,
    Exit,
}

impl Ahead {
    /// The jobs for `units` — the plan's `(unit, members)` in plan order, each unit's members
    /// in member order — or `None` when there is nothing to fetch ahead. A unit is fetched
    /// whole or not at all: every member must have its pin to install, a manifest verified
    /// and bound to it, a row for `triple` this client would admit and stage, and no
    /// digest-refusal memo holding it. Any miss leaves the unit to the pass, which meets the
    /// same refusal itself; nothing here can make a member install that would not have.
    pub(crate) fn plan(
        fetcher: &dyn Fetcher,
        layout: &Layout,
        index: &TrustedIndex,
        ch: &Channel,
        triple: &str,
        units: &[(usize, Vec<String>)],
    ) -> Option<Arc<Self>> {
        Self::plan_within(fetcher, layout, index, ch, triple, units, AHEAD_BYTES)
    }

    /// [`Ahead::plan`] under a `budget` of its own.
    pub(super) fn plan_within(
        fetcher: &dyn Fetcher,
        layout: &Layout,
        index: &TrustedIndex,
        ch: &Channel,
        triple: &str,
        units: &[(usize, Vec<String>)],
        budget: u64,
    ) -> Option<Arc<Self>> {
        let jobs: Vec<Job> = units
            .iter()
            .filter_map(|(unit, members)| {
                members
                    .iter()
                    .map(|m| job(fetcher, layout, index, ch, triple, *unit, m))
                    .collect::<Option<Vec<Job>>>()
            })
            .flatten()
            .collect();
        if jobs.is_empty() {
            return None;
        }
        let n = jobs.len();
        Some(Arc::new(Self {
            jobs,
            budget,
            state: Mutex::new(State {
                slots: (0..n).map(|_| Slot::Queued).collect(),
                claimed: vec![false; n],
                current: None,
                passed: BTreeSet::new(),
                fetched: vec![false; n],
                halted: false,
            }),
            changed: Condvar::new(),
        }))
    }

    /// What this lane can hold on disk beyond the pass's own unit, at most: the archives it
    /// may fetch ahead, up to its budget. The set completion starts it only on a volume that
    /// holds the set's peak AND this.
    pub(crate) fn reserve(&self) -> u64 {
        self.jobs
            .iter()
            .map(|j| j.artifact.size)
            .filter(|&size| size <= self.budget)
            .fold(0u64, u64::saturating_add)
            .min(self.budget)
    }

    /// The worker: fetch job after job until none is left to fetch or the lane halts. Run on
    /// a thread of its own inside the pass (its store lock, its progress pass lent to it),
    /// with `fetcher` shared; the pass thread enters the lane before it stages anything.
    /// `landed` is told, on this thread, after each archive this lane fetched is in.
    pub(crate) fn work(&self, fetcher: &dyn Fetcher, layout: &Layout, landed: &dyn Fn(&Self)) {
        let _gone = Gone(self);
        loop {
            // Read outside the lock, every time a job is chosen: a person's bump reorders
            // what has not started, as the pass's own read between units does.
            let bump = crate::progress::read_bump(layout);
            let i = match self.next(&bump) {
                Next::Fetch(i) => i,
                Next::Again => continue,
                Next::Exit => return,
            };
            let answer = fetch_ahead(fetcher, &self.jobs[i]);
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let fetched = matches!(answer, Some(Ok(())));
            state.fetched[i] = fetched;
            state.slots[i] = match answer {
                Some(fetched) => {
                    // A failed download stops the lane: the link is the likeliest cause, and
                    // the pass then fetches the rest itself, as it always did.
                    state.halted |= fetched.is_err();
                    Slot::Done(fetched)
                }
                None => Slot::Dropped,
            };
            drop(state);
            self.changed.notify_all();
            if fetched {
                landed(self);
            }
        }
    }

    /// Whether every archive this lane holds is in, each fetched by this lane — whatever the
    /// pass has made of them since — and it holds archives of each of `units`. Never for a
    /// unit it holds nothing of, or an archive it skipped, left behind or failed: the pass
    /// fetches those itself, and only it knows when they are in.
    pub(crate) fn all_fetched(&self, units: &[usize]) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        state.fetched.iter().all(|fetched| *fetched)
            && units
                .iter()
                .all(|unit| self.jobs.iter().any(|job| job.unit == *unit))
    }

    /// The job to fetch next, marked running; waits once when the budget is spent or the
    /// pass has not caught up.
    fn next(&self, bump: &[String]) -> Next {
        let Ok(mut state) = self.state.lock() else {
            return Next::Exit;
        };
        if state.halted {
            return Next::Exit;
        }
        let Some(i) = self.pick(&state, bump) else {
            return Next::Exit;
        };
        let fits = || {
            let ahead = (0..self.jobs.len())
                .filter(|&k| {
                    !state.claimed[k] && matches!(state.slots[k], Slot::Running | Slot::Done(_))
                })
                .map(|k| self.jobs[k].artifact.size)
                .fold(0u64, u64::saturating_add);
            ahead.saturating_add(self.jobs[i].artifact.size) <= self.budget
        };
        if matches!(state.slots[i], Slot::Wanted) || fits() {
            state.slots[i] = Slot::Running;
            return Next::Fetch(i);
        }
        match self.changed.wait(state) {
            Ok(_) => Next::Again,
            Err(_) => Next::Exit,
        }
    }

    /// The job the pass will want soonest among those not started: the one it is waiting
    /// for; else the rest of the unit it is in, in member order; else the first member of the
    /// next unit it will reach — plan order, re-sorted by `bump` as the pass re-sorts its
    /// remainder (bumped units first, earliest mention first, plan order the tiebreak).
    /// `None` when nothing is left to start.
    fn pick(&self, state: &State, bump: &[String]) -> Option<usize> {
        if let Some(i) = state.slots.iter().position(|s| matches!(s, Slot::Wanted)) {
            return Some(i);
        }
        let queued_in = |unit: usize| {
            (0..self.jobs.len())
                .find(|&i| self.jobs[i].unit == unit && matches!(state.slots[i], Slot::Queued))
        };
        let open = |unit: &usize| !state.passed.contains(unit);
        if let Some(i) = state.current.filter(open).and_then(queued_in) {
            return Some(i);
        }
        let mut units: Vec<usize> = Vec::new();
        for job in &self.jobs {
            if Some(job.unit) != state.current && open(&job.unit) && !units.contains(&job.unit) {
                units.push(job.unit);
            }
        }
        let rank = |unit: &usize| {
            self.jobs
                .iter()
                .filter(|j| j.unit == *unit)
                .filter_map(|j| bump.iter().position(|b| *b == j.program))
                .min()
                .unwrap_or(usize::MAX)
        };
        units.sort_by_key(rank);
        units.into_iter().find_map(queued_in)
    }

    /// On the pass thread: take `program`'s archive at `dl` from this lane. `Some` with the
    /// download's own answer when this lane fetched it — waiting for it when it is still
    /// moving, or when it had not started and the lane is idle, which then fetches it at
    /// once. `None` when the pass fetches it itself, as it would with no lane: no such job,
    /// a lane that halted or is busy with another archive, or a job it skipped.
    ///
    /// A job for `program` at ANOTHER path — a singleton's own resolve found a newer pin
    /// than the plan's — never stands in for the pass's archive, and never moves beside it:
    /// both would land in `staging/<program>/`, where each download's partial sweep takes
    /// the other's `.part`. One not started never will; one moving is waited out first.
    fn claim(&self, program: &str, dl: &Path) -> Option<Result<(), String>> {
        let i = self.jobs.iter().position(|j| j.program == program)?;
        let ours = self.jobs[i].dl == dl;
        let mut state = self.state.lock().ok()?;
        state.claimed[i] = true;
        state.current = Some(self.jobs[i].unit);
        // The budget moved (this job's bytes are the pass's now), and so did the unit.
        self.changed.notify_all();
        loop {
            match std::mem::replace(&mut state.slots[i], Slot::Taken) {
                Slot::Done(fetched) => return ours.then_some(fetched),
                Slot::Running => state.slots[i] = Slot::Running,
                Slot::Queued | Slot::Wanted
                    if ours
                        && !state.halted
                        && !state.slots.iter().any(|s| matches!(s, Slot::Running)) =>
                {
                    state.slots[i] = Slot::Wanted;
                    self.changed.notify_all();
                }
                // Left `Taken`: the pass fetches it, and this lane never will.
                Slot::Queued | Slot::Wanted | Slot::Taken | Slot::Dropped => return None,
            }
            state = self.changed.wait(state).ok()?;
        }
    }

    /// On the pass thread, once it is done with `unit` — landed, refused, skipped or
    /// aborted: what of it has not started never will.
    pub(crate) fn passed(&self, unit: usize) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.passed.insert(unit);
        for (i, job) in self.jobs.iter().enumerate() {
            if job.unit == unit && matches!(state.slots[i], Slot::Queued | Slot::Wanted) {
                state.slots[i] = Slot::Dropped;
            }
        }
        drop(state);
        self.changed.notify_all();
    }

    /// `program → asset` for every archive this lane fetched, or began to, that the pass never
    /// took — a unit it aborted before reaching the member. Read after the worker is joined:
    /// the caller adds them to the pass's resolved assets, so the pass-end gc spares the
    /// archive (or its `.part`) and the next pass reuses it ([`super::carried_archive`]).
    pub(crate) fn untaken(&self) -> Vec<(String, String)> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        self.jobs
            .iter()
            .zip(&state.slots)
            .filter(|(_, slot)| matches!(slot, Slot::Done(_) | Slot::Running))
            .map(|(job, _)| (job.program.clone(), job.artifact.asset.clone()))
            .collect()
    }

    /// On the pass thread: route its member fetches through this lane ([`claim`]) until the
    /// guard drops, which halts the lane.
    pub(crate) fn enter(self: &Arc<Self>) -> Entered {
        PASS.with(|pass| *pass.borrow_mut() = Some(Arc::clone(self)));
        Entered(Arc::clone(self))
    }

    fn halt(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.halted = true;
        }
        self.changed.notify_all();
    }
}

/// The pass thread's hold on its lane ([`Ahead::enter`]): dropped — the loop over, or a
/// panic unwinding it — it halts the lane, so the worker ends after the archive it has in
/// hand and the scope that runs it can join it.
pub(crate) struct Entered(Arc<Ahead>);

impl Drop for Entered {
    fn drop(&mut self) {
        PASS.with(|pass| *pass.borrow_mut() = None);
        self.0.halt();
    }
}

/// The worker's exit, however it leaves: the lane halts, and an archive it was moving when
/// it panicked is the pass's to fetch, so no claim waits on it forever.
struct Gone<'a>(&'a Ahead);

impl Drop for Gone<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.halted = true;
            for slot in &mut state.slots {
                if matches!(slot, Slot::Running) {
                    *slot = Slot::Dropped;
                }
            }
        }
        self.0.changed.notify_all();
    }
}

thread_local! {
    /// The lane the pass on this thread has entered ([`Ahead::enter`]); none elsewhere.
    static PASS: std::cell::RefCell<Option<Arc<Ahead>>> = const { std::cell::RefCell::new(None) };
}

/// On the pass thread, where it would fetch `program`'s archive to `dl`: the answer of the
/// lane's download of it ([`Ahead::claim`]), or `None` to fetch it here as always — which is
/// every call off a thread that entered a lane.
pub(crate) fn claim(program: &str, dl: &Path) -> Option<Result<(), String>> {
    PASS.with(|pass| pass.borrow().clone())?.claim(program, dl)
}

/// One member's [`Job`], or `None` when the pass would not fetch its archive as it stands.
fn job(
    fetcher: &dyn Fetcher,
    layout: &Layout,
    index: &TrustedIndex,
    ch: &Channel,
    triple: &str,
    unit: usize,
    program: &str,
) -> Option<Job> {
    if index.program(program)?.names_requires()
        || decide(ch, program, None) != ApplyDecision::Install
    {
        return None;
    }
    let (pinned, repo, pkg) = verified_pkg_or_miss(fetcher, index, ch, program).ok()?;
    if !pkg.is_for(program) || pkg.build_number != pinned {
        return None;
    }
    let artifact = pkg.artifact_for(triple)?;
    // Legacy github-release rows may have no compressed size. Their cap of
    // zero means the fetcher's own ceiling, so they cannot spend zero bytes
    // of this lane's reserve. Leave their whole unit to the inline pass.
    if artifact.size == 0 {
        return None;
    }
    crate::vendor::check_row(artifact, &pkg.exposes).ok()?;
    match crate::dispatch::strategy_for(&artifact.kind, &artifact.protocol) {
        crate::dispatch::ApplyStrategy::Shim
        | crate::dispatch::ApplyStrategy::VendorApp
        | crate::dispatch::ApplyStrategy::SysrootBundle => {}
        crate::dispatch::ApplyStrategy::AppBundle | crate::dispatch::ApplyStrategy::Unknown => {
            return None;
        }
    }
    if digest_refusal_note(layout, program, pinned, artifact, false).is_some() {
        return None;
    }
    let dl = staged_download_path(layout, program, &artifact.asset).ok()?;
    Some(Job {
        unit,
        program: program.to_string(),
        pinned,
        repo,
        artifact: artifact.clone(),
        dl,
    })
}

/// Fetch one job's archive as the pass would, on this lane's thread: `None` (skipped, the
/// pass decides) when a file already stands at the path — an archive an earlier attempt
/// carried, or a stale entry the pass must remove itself — or the volume would not hold it
/// beside its tree; else the download's answer, with `dl` absent or complete as the pass's
/// own download leaves it.
fn fetch_ahead(fetcher: &dyn Fetcher, job: &Job) -> Option<Result<(), String>> {
    make_staging_dir(&job.dl).ok()?;
    if std::fs::symlink_metadata(&job.dl).is_ok() {
        return None;
    }
    disk_gate(
        LandingBytes::of(&job.artifact).need(),
        crate::freespace::available_bytes(&job.dl),
    )
    .ok()?;
    // At most one partial and one archive per program between passes, and they are the
    // ones this download continues or replaces — the sweep the pass makes before its own.
    sweep_foreign_partials(&job.dl);
    crate::progress::note_build(&job.program, job.pinned);
    let watch = crate::progress::watch_download(&job.program, &job.dl, job.artifact.size);
    let fetched = fetch_artifact(fetcher, &job.program, &job.repo, &job.artifact, &job.dl);
    drop(watch);
    match &fetched {
        // The row waits for the pass at its whole size: the poller's last look was before
        // the end, or after the `.part` became the archive (which it reads as nothing).
        Ok(()) => crate::progress::note_downloaded(&job.program, job.artifact.size),
        Err(_) => {
            let _ = std::fs::remove_file(&job.dl);
        }
    }
    Some(fetched)
}
