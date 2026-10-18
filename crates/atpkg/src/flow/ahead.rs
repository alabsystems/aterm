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
//! the stage, the flip, the status rows, the fault tally. A download that failed here does
//! not stop the lane, and is not the pass's answer: the lane goes on to the next archive, and
//! the pass fetches the failed one itself when it reaches it — continuing its `.part` — and
//! counts that download's own fault, if any (`note_fetch_fault`), so the offline verdict
//! reads one tally as before. One dropped transfer on a flaky link costs that archive a
//! second try, not the rest of the set its download ahead.
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
//! extracting and flipping ([`Ahead::all_fetched`], asked after every download that lands —
//! the pass's own of an archive this lane failed included, [`fetched_inline`]).

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
    /// Downloaded; not yet taken by the pass.
    Done,
    /// This lane's download failed; not yet taken. The pass fetches it itself.
    Failed,
    /// The pass is fetching it itself, after this lane's download failed: its success counts
    /// toward [`Ahead::all_fetched`] ([`fetched_inline`]).
    Inline,
    /// The pass has it: the archive, or the download itself when it fetched inline.
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
    /// Whether each job's archive is in, fetched by this lane — or by the pass after this
    /// lane's download of it failed ([`Ahead::all_fetched`]).
    fetched: Vec<bool>,
    /// No more downloads start: the pass ended, or the thread is gone.
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
    /// `landed` is told, on this thread, after each archive this lane fetched is in, and
    /// after each the pass fetched itself once this lane's download of it failed.
    pub(crate) fn work(&self, fetcher: &dyn Fetcher, layout: &Layout, landed: &dyn Fn(&Self)) {
        self.work_with_bump(fetcher, landed, || crate::progress::read_bump(layout));
    }

    fn work_with_bump(
        &self,
        fetcher: &dyn Fetcher,
        landed: &dyn Fn(&Self),
        read_bump: impl Fn() -> Vec<String>,
    ) {
        let _gone = Gone(self);
        let mut told = 0usize;
        loop {
            let now_in = self.archives_in();
            if now_in > told {
                told = now_in;
                landed(self);
            }
            // Read outside the lock, every time a job is chosen: a person's bump reorders
            // what has not started, as the pass's own read between units does.
            let bump = read_bump();
            let i = match self.next(&bump) {
                Next::Fetch(i) => i,
                Next::Again => continue,
                Next::Exit => {
                    // An inline success can land after the loop's count and before next's
                    // final locked decision. Tell it before leaving, outside the lock.
                    if self.archives_in() > told {
                        landed(self);
                    }
                    return;
                }
            };
            let answer = fetch_ahead(fetcher, &self.jobs[i]);
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state.fetched[i] = matches!(answer, Some(Ok(())));
            state.slots[i] = match answer {
                Some(Ok(())) => Slot::Done,
                // NOT A HALT: the next archive is fetched as planned, and this one is the
                // pass's to fetch when it reaches it ([`Ahead::claim`]), its own fault counted
                // there if its download fails too.
                Some(Err(_)) => Slot::Failed,
                None => Slot::Dropped,
            };
            drop(state);
            self.changed.notify_all();
        }
    }

    /// How many of the jobs' archives are in ([`State::fetched`]).
    fn archives_in(&self) -> usize {
        self.state
            .lock()
            .map_or(0, |state| state.fetched.iter().filter(|f| **f).count())
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
        self.next_with_wait(bump, |state| match self.changed.wait(state) {
            Ok(_) => Next::Again,
            Err(_) => Next::Exit,
        })
    }

    fn next_with_wait<'a>(
        &'a self,
        bump: &[String],
        wait: impl FnOnce(std::sync::MutexGuard<'a, State>) -> Next,
    ) -> Next {
        let Ok(mut state) = self.state.lock() else {
            return Next::Exit;
        };
        if state.halted {
            return Next::Exit;
        }
        let Some(i) = self.pick(&state, bump) else {
            // Nothing left to start. The pass's own download of an archive this lane failed
            // is still news to tell ([`fetched_inline`]): wait for it, until the pass is done.
            if state
                .slots
                .iter()
                .any(|s| matches!(s, Slot::Failed | Slot::Inline))
            {
                return wait(state);
            }
            return Next::Exit;
        };
        let fits = || {
            let ahead = (0..self.jobs.len())
                .filter(|&k| {
                    !state.claimed[k]
                        && matches!(state.slots[k], Slot::Running | Slot::Done | Slot::Failed)
                })
                .map(|k| self.jobs[k].artifact.size)
                .fold(0u64, u64::saturating_add);
            ahead.saturating_add(self.jobs[i].artifact.size) <= self.budget
        };
        if matches!(state.slots[i], Slot::Wanted) || fits() {
            state.slots[i] = Slot::Running;
            return Next::Fetch(i);
        }
        wait(state)
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

    /// On the pass thread: take `program`'s archive at `dl` from this lane. `Some(Ok(()))` when
    /// this lane fetched it — waiting for it when it is still moving, or when it had not
    /// started and the lane is idle, which then fetches it at once. `None` when the pass
    /// fetches it itself, as it would with no lane: no such job, a lane that halted or is
    /// busy with another archive, a job it skipped — or one whose download here failed,
    /// which the pass then tries itself (reporting a success, [`fetched_inline`]).
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
                Slot::Done => return ours.then_some(Ok(())),
                Slot::Failed if ours => {
                    state.slots[i] = Slot::Inline;
                    return None;
                }
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
                Slot::Queued
                | Slot::Wanted
                | Slot::Failed
                | Slot::Inline
                | Slot::Taken
                | Slot::Dropped => return None,
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
            .filter(|(_, slot)| matches!(slot, Slot::Done | Slot::Failed | Slot::Running))
            .map(|(job, _)| (job.program.clone(), job.artifact.asset.clone()))
            .collect()
    }

    /// On the pass thread: its own download of `program`'s archive at `dl` — one this lane's
    /// download failed ([`Slot::Inline`]) — is in. The archive counts as fetched, and the
    /// worker tells `landed` so ([`Ahead::work`]).
    fn fetched_inline(&self, program: &str, dl: &Path) {
        let Some(i) = self
            .jobs
            .iter()
            .position(|j| j.program == program && j.dl == dl)
        else {
            return;
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if matches!(state.slots[i], Slot::Inline) {
            state.slots[i] = Slot::Taken;
            state.fetched[i] = true;
            drop(state);
            self.changed.notify_all();
        }
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

/// On the pass thread, after its own download of `program`'s archive to `dl` succeeded: news
/// for the lane when its download of that archive had failed ([`Ahead::fetched_inline`]);
/// nothing anywhere else.
pub(crate) fn fetched_inline(program: &str, dl: &Path) {
    if let Some(lane) = PASS.with(|pass| pass.borrow().clone()) {
        lane.fetched_inline(program, dl);
    }
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

#[cfg(test)]
mod callback_tests {
    use super::*;
    use std::cell::Cell;

    struct NoFetch;

    impl Fetcher for NoFetch {
        fn index_candidates(&self) -> Result<Vec<crate::select::Candidate>, String> {
            panic!("the prepared lane never resolves an index");
        }

        fn pkg_manifest(
            &self,
            _repo: &str,
            _program: &str,
            _build: u64,
        ) -> Result<(Vec<u8>, Vec<u8>), String> {
            panic!("the prepared lane never resolves a package");
        }

        fn download(&self, _repo: &str, _asset: &str, _dest: &Path) -> Result<(), String> {
            panic!("these completed or inline jobs never download on the worker");
        }
    }

    fn lane(slots: Vec<Slot>, sizes: &[u64], budget: u64) -> Ahead {
        assert_eq!(slots.len(), sizes.len());
        let jobs = sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| Job {
                unit: i,
                program: format!("p{i}"),
                pinned: 1,
                repo: "model/repository".to_string(),
                artifact: crate::manifest::Artifact {
                    target: "model-target".to_string(),
                    kind: "binary".to_string(),
                    protocol: "github-release".to_string(),
                    asset: format!("p{i}.tar.zst"),
                    sha256: String::new(),
                    sha512: String::new(),
                    tree_root: String::new(),
                    size,
                    reloc: "self-contained".to_string(),
                    cost: crate::manifest::Cost::default(),
                    url: String::new(),
                    payload: String::new(),
                    entry: String::new(),
                    strip_components: 0,
                    links: Default::default(),
                    vendor: String::new(),
                    provenance: None,
                },
                dl: PathBuf::from(format!("/model/p{i}/p{i}.tar.zst")),
            })
            .collect();
        let n = sizes.len();
        Ahead {
            jobs,
            budget,
            state: Mutex::new(State {
                slots,
                claimed: vec![false; n],
                current: None,
                passed: BTreeSet::new(),
                fetched: vec![false; n],
                halted: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn notices(lane: &Ahead, read_bump: impl Fn() -> Vec<String>) -> usize {
        let count = Cell::new(0);
        lane.work_with_bump(
            &NoFetch,
            &|seen| {
                // A callback may re-enter this exact lane: no state lock is held.
                assert!(seen.all_fetched(&[0]));
                count.set(count.get() + 1);
            },
            read_bump,
        );
        count.get()
    }

    #[test]
    fn a_last_inline_archive_is_announced_before_the_worker_leaves() {
        let already_in = lane(vec![Slot::Failed], &[7], 7);
        let dl = already_in.jobs[0].dl.clone();
        assert!(already_in.claim("p0", &dl).is_none());
        already_in.fetched_inline("p0", &dl);
        assert_eq!(notices(&already_in, Vec::new), 1);

        let refused = lane(vec![Slot::Failed], &[7], 7);
        assert_eq!(
            notices(&refused, || {
                assert!(refused.claim("p0", &refused.jobs[0].dl).is_none());
                refused.halt();
                Vec::new()
            }),
            0
        );

        let wrong_path = lane(vec![Slot::Failed], &[7], 7);
        assert_eq!(
            notices(&wrong_path, || {
                assert!(wrong_path.claim("p0", &wrong_path.jobs[0].dl).is_none());
                wrong_path.fetched_inline("p0", Path::new("/model/another.tar.zst"));
                wrong_path.halt();
                Vec::new()
            }),
            0
        );

        let last_inline = lane(vec![Slot::Failed], &[7], 7);
        let bump_reads = Cell::new(0);
        let count = notices(&last_inline, || {
            bump_reads.set(bump_reads.get() + 1);
            assert!(last_inline.claim("p0", &last_inline.jobs[0].dl).is_none());
            last_inline.fetched_inline("p0", &last_inline.jobs[0].dl);
            Vec::new()
        });
        assert_eq!(bump_reads.get(), 1);
        assert!(last_inline.all_fetched(&[0]));
        assert!(last_inline.state.lock().unwrap().halted);
        assert_eq!(
            count, 1,
            "LAST_INLINE_ARCHIVE_MUST_BE_ANNOUNCED_BEFORE_EXIT"
        );
    }
}

#[cfg(test)]
mod reservation_tests {
    use super::*;
    use std::cell::Cell;

    fn lane(slots: Vec<Slot>, sizes: &[u64], budget: u64) -> Ahead {
        assert_eq!(slots.len(), sizes.len());
        let jobs = sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| Job {
                unit: i,
                program: format!("p{i}"),
                pinned: 1,
                repo: "model/repository".to_string(),
                artifact: crate::manifest::Artifact {
                    target: "model-target".to_string(),
                    kind: "binary".to_string(),
                    protocol: "github-release".to_string(),
                    asset: format!("p{i}.tar.zst"),
                    sha256: String::new(),
                    sha512: String::new(),
                    tree_root: String::new(),
                    size,
                    reloc: "self-contained".to_string(),
                    cost: crate::manifest::Cost::default(),
                    url: String::new(),
                    payload: String::new(),
                    entry: String::new(),
                    strip_components: 0,
                    links: Default::default(),
                    vendor: String::new(),
                    provenance: None,
                },
                dl: PathBuf::from(format!("/model/p{i}/p{i}.tar.zst")),
            })
            .collect();
        let n = sizes.len();
        Ahead {
            jobs,
            budget,
            state: Mutex::new(State {
                slots,
                claimed: vec![false; n],
                current: None,
                passed: BTreeSet::new(),
                fetched: vec![false; n],
                halted: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn next_without_wait(lane: &Ahead) -> (Next, bool) {
        let waited = Cell::new(false);
        let next = lane.next_with_wait(&[], |state| {
            waited.set(true);
            drop(state);
            Next::Again
        });
        (next, waited.get())
    }

    #[test]
    fn failed_partial_bytes_hold_the_budget_until_the_pass_takes_them() {
        const MIB: u64 = 1 << 20;
        let complete = lane(vec![Slot::Done, Slot::Queued], &[400 * MIB; 2], 512 * MIB);
        let (next, waited) = next_without_wait(&complete);
        assert!(matches!(next, Next::Again) && waited);
        assert!(matches!(
            complete.state.lock().unwrap().slots[1],
            Slot::Queued
        ));

        let taken = lane(vec![Slot::Failed, Slot::Queued], &[400 * MIB; 2], 512 * MIB);
        assert!(taken.claim("p0", &taken.jobs[0].dl).is_none());
        assert!(taken.state.lock().unwrap().claimed[0]);
        let (next, waited) = next_without_wait(&taken);
        assert!(matches!(next, Next::Fetch(1)) && !waited);

        let room = lane(
            vec![Slot::Failed, Slot::Queued],
            &[100 * MIB, 400 * MIB],
            512 * MIB,
        );
        let (next, waited) = next_without_wait(&room);
        assert!(matches!(next, Next::Fetch(1)) && !waited);

        let stopped = lane(vec![Slot::Failed, Slot::Queued], &[400 * MIB; 2], 512 * MIB);
        stopped.halt();
        let (next, waited) = next_without_wait(&stopped);
        assert!(matches!(next, Next::Exit) && !waited);

        let partial = lane(vec![Slot::Failed, Slot::Queued], &[400 * MIB; 2], 512 * MIB);
        let (next, waited) = next_without_wait(&partial);
        let still_queued = matches!(partial.state.lock().unwrap().slots[1], Slot::Queued);
        let retained = partial.untaken() == vec![("p0".to_string(), "p0.tar.zst".to_string())];
        assert!(
            matches!(next, Next::Again) && waited && still_queued && retained,
            "FAILED_PARTIAL_BYTES_MUST_HOLD_THE_AHEAD_RESERVATION"
        );
    }
}
