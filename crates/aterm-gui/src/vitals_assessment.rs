// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Serialized job and leftover assessment, independent of the machine publisher.
//! Only the newest pending observation is retained. Naming replies hold the engine
//! until their exact view has been published; no process probe runs under the mailbox lock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Instant;

use aterm_sysprobe::procs::ProcessReading;
use aterm_vitals::jobs::JobsView;
use aterm_vitals::left::LeftView;

use super::{Msg, NameReply, Request, TabReads};
use crate::vitals_jobs::JobsPass;
use crate::vitals_left::LeftPass;

/// Work the machine reader must service after draining any control message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Work {
    None,
    Input,
    Publish,
}

/// Immutable evidence belongs to the job view that read it, never a later machine frame.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct ProcessIdentity {
    pid: u32,
    start: Option<u64>,
    version: Option<i32>,
}

struct Evidence {
    processes: Option<Vec<ProcessIdentity>>,
    roots: Vec<(u64, u32)>,
    words: Vec<super::TtyWord>,
    feeds: Vec<(u64, crate::vitals_jobs::TabFeed)>,
}

impl Evidence {
    fn new(req: &Request, reads: &TabReads, sweep: Option<&[ProcessReading]>) -> Self {
        let processes = sweep
            .filter(|rows| {
                rows.iter()
                    .all(|p| p.start.is_some() || p.version.is_some())
            })
            .map(|rows| {
                let mut ids: Vec<_> = rows
                    .iter()
                    .map(|p| ProcessIdentity {
                        pid: p.pid,
                        start: p.start,
                        version: p.version,
                    })
                    .collect();
                ids.sort_unstable();
                ids
            });
        let mut roots: Vec<_> = req.tabs.iter().map(|t| (t.key, t.shell_pid)).collect();
        roots.sort_unstable();
        let mut feeds = reads.feeds.clone();
        feeds.sort_unstable_by_key(|(key, _)| *key);
        let mut words = req.ttys.clone();
        words.sort_unstable_by_key(|word| word.key);
        Self {
            processes,
            roots,
            words,
            feeds,
        }
    }

    fn matches(&self, current: &Self) -> bool {
        self.processes
            .as_ref()
            .zip(current.processes.as_ref())
            .is_some_and(|(a, b)| a == b)
            && self.roots == current.roots
            && self.feeds == current.feeds
            && self.words.len() == current.words.len()
            && self
                .words
                .iter()
                .zip(&current.words)
                .all(|(a, b)| a.seen() == b.seen())
            && self.words_current()
    }

    fn words_current(&self) -> bool {
        self.words
            .iter()
            .all(|word| word.current().is_some_and(|(_, mode)| mode.stamped_ms != 0))
    }
}

pub(super) struct Input {
    pub(super) reads: TabReads,
    pub(super) sweep: Option<Vec<ProcessReading>>,
    pub(super) complete: bool,
    pub(super) req: Request,
    pub(super) unix_ms: u64,
    pub(super) thrashing: bool,
    evidence: Arc<Evidence>,
    serial: u64,
    coalesced: u64,
}

impl Input {
    pub(super) fn new(
        reads: TabReads,
        sweep: Option<Vec<ProcessReading>>,
        complete: bool,
        req: &Request,
        unix_ms: u64,
        thrashing: bool,
    ) -> Self {
        let evidence = Arc::new(Evidence::new(
            req,
            &reads,
            if complete { sweep.as_deref() } else { None },
        ));
        Self {
            reads,
            sweep,
            complete,
            req: req.clone(),
            unix_ms,
            thrashing,
            evidence,
            serial: 0,
            coalesced: 0,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Reading {
    pub(super) jobs: Option<Arc<JobsView>>,
    pub(super) left: Option<Arc<LeftView>>,
    pub(super) jobs_at: Option<Instant>,
    pub(super) left_at: Option<Instant>,
    evidence: Option<Arc<Evidence>>,
}

pub(super) struct Core {
    jobs: JobsPass,
    left: LeftPass,
    reading: Reading,
}

impl Core {
    fn new() -> Self {
        Self {
            jobs: JobsPass::new(),
            left: LeftPass::new(),
            reading: Reading::default(),
        }
    }

    pub(super) fn read(&mut self, input: &Input) -> Reading {
        self.left.keep_told(&input.req.left_told, input.unix_ms);
        self.jobs.tie_held(self.left.ties());
        let jobs = super::jobs_step(
            &mut self.jobs,
            &input.reads,
            input.sweep.as_deref(),
            &input.req,
            input.thrashing,
        );
        let age = super::jobs_age_ms(jobs.as_deref(), input.reads.now_ms());
        let left = self.left.tick(
            input.unix_ms,
            input.sweep.as_deref(),
            input.complete,
            &input.req,
            jobs.as_deref(),
            age,
        );
        let jobs_changed = match (&self.reading.jobs, &jobs) {
            (Some(old), Some(new)) => !Arc::ptr_eq(old, new),
            (None, Some(_)) => true,
            _ => false,
        };
        let changed = match (&self.reading.left, &left) {
            (Some(old), Some(new)) => !Arc::ptr_eq(old, new),
            (None, Some(_)) => true,
            _ => false,
        };
        self.reading = Reading {
            jobs,
            left,
            jobs_at: self.jobs.observed_at(),
            evidence: if jobs_changed {
                Some(Arc::clone(&input.evidence))
            } else {
                self.reading.evidence.clone()
            },
            left_at: if changed {
                Some(input.reads.observed_at)
            } else {
                self.reading.left_at
            },
        };
        self.reading.clone()
    }
}

/// The direct arm is an inert unit-test seam; the installed reader always uses the worker.
pub(super) enum Lane {
    Worker(Worker),
    #[cfg(test)]
    Direct(Box<Core>),
}

impl Lane {
    pub(super) fn new() -> Self {
        #[cfg(test)]
        {
            Self::Direct(Box::new(Core::new()))
        }
        #[cfg(not(test))]
        {
            Self::Worker(Worker::new(Core::read))
        }
    }

    #[cfg(test)]
    pub(super) fn threaded(
        read: impl FnMut(&mut Core, &Input) -> Reading + Send + 'static,
    ) -> Self {
        Self::Worker(Worker::new(read))
    }

    pub(super) fn install_wake(&mut self, wake: Weak<SyncSender<Msg>>) {
        match self {
            Self::Worker(worker) => worker.shared.lock().wake = wake,
            #[cfg(test)]
            Self::Direct(_) => {}
        }
    }

    pub(super) fn work(&self) -> Work {
        match self {
            Self::Worker(worker) => worker.work(),
            #[cfg(test)]
            Self::Direct(_) => Work::None,
        }
    }

    pub(super) fn read(&mut self, input: Input, offer: bool) -> Reading {
        match self {
            Self::Worker(worker) => {
                worker.poll();
                worker.observe(Arc::clone(&input.evidence));
                if offer {
                    worker.submit(input);
                }
                worker.reading.clone()
            }
            #[cfg(test)]
            Self::Direct(core) => {
                if offer {
                    core.read(&input)
                } else {
                    core.reading.clone()
                }
            }
        }
    }

    pub(super) fn carried(&mut self) -> Reading {
        match self {
            Self::Worker(worker) => {
                worker.poll();
                worker.reading.clone()
            }
            #[cfg(test)]
            Self::Direct(core) => core.reading.clone(),
        }
    }

    pub(super) fn reading(&self) -> &Reading {
        match self {
            Self::Worker(worker) => &worker.reading,
            #[cfg(test)]
            Self::Direct(core) => &core.reading,
        }
    }

    pub(super) fn continuous(&self) -> bool {
        match self {
            Self::Worker(worker) => {
                worker.shared.alive.load(Ordering::Acquire)
                    && worker.admitted
                    && worker
                        .reading
                        .evidence
                        .as_ref()
                        .is_some_and(|e| e.words_current())
            }
            #[cfg(test)]
            Self::Direct(_) => true,
        }
    }

    pub(super) fn words(&self) -> Vec<super::WordSeen> {
        self.reading()
            .evidence
            .as_ref()
            .map(|e| e.words.iter().map(super::TtyWord::seen).collect())
            .unwrap_or_default()
    }

    pub(super) fn has_name_replies(&self) -> bool {
        match self {
            Self::Worker(worker) => !worker.replies.is_empty(),
            #[cfg(test)]
            Self::Direct(_) => false,
        }
    }

    pub(super) fn note_told(&mut self, told: &[u64], at_ms: u64) {
        match self {
            Self::Worker(worker) => {
                let mut state = worker.shared.lock();
                state.told = Some((told.to_vec(), at_ms));
                worker.shared.changed.notify_one();
            }
            #[cfg(test)]
            Self::Direct(core) => core.left.keep_told(told, at_ms),
        }
    }

    pub(super) fn at_stop(&mut self, told: &[u64], at_ms: u64) {
        match self {
            Self::Worker(worker) => worker.stop(Some((told.to_vec(), at_ms))),
            #[cfg(test)]
            Self::Direct(core) => core.left.at_stop(told, at_ms),
        }
    }

    pub(super) fn schedule_names(
        &mut self,
        names: Vec<crate::sys_naming::Request>,
    ) -> Vec<NameReply> {
        match self {
            Self::Worker(worker) => worker.schedule_names(names),
            #[cfg(test)]
            Self::Direct(core) => super::prepare_names_on(
                &mut core.jobs,
                names,
                JobsPass::capture_name,
                JobsPass::apply_name,
            ),
        }
    }

    pub(super) fn publish_names(&mut self, replies: Vec<NameReply>) {
        match self {
            Self::Worker(worker) => worker.published(replies),
            #[cfg(test)]
            Self::Direct(core) => super::respond_names(super::validate_names_on(
                &core.jobs,
                replies,
                JobsPass::name_is_shown,
            )),
        }
    }
}

struct Named {
    at: Instant,
    request: crate::sys_naming::Request,
}
struct Output {
    #[cfg(test)]
    serial: u64,
    publication: Arc<()>,
    reading: Reading,
    replies: Vec<NameReply>,
}

#[derive(Default)]
struct State {
    input: Option<Input>,
    names: Vec<Named>,
    ready: Option<Output>,
    acknowledged: Option<Arc<()>>,
    required_after: Option<Instant>,
    need_input: bool,
    need_publication: bool,
    closing: bool,
    stop: Option<(Vec<u64>, u64)>,
    told: Option<(Vec<u64>, u64)>,
    discarded: u64,
    wake: Weak<SyncSender<Msg>>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    alive: AtomicBool,
}
impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn wake(&self, input: bool) {
        let wake = self.lock().wake.clone();
        if let Some(tx) = wake.upgrade() {
            let _ = tx.try_send(if input {
                Msg::Pull
            } else {
                Msg::AssessmentReady
            });
        }
    }
}

/// The owner holds no native job state and never waits for an assessment.
pub(super) struct Worker {
    shared: Arc<Shared>,
    reading: Reading,
    replies: Vec<NameReply>,
    publication: Option<Arc<()>>,
    next: u64,
    reported_dead: bool,
    current: Option<Arc<Evidence>>,
    admitted: bool,
}

impl Worker {
    fn new(mut read: impl FnMut(&mut Core, &Input) -> Reading + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            alive: AtomicBool::new(true),
        });
        let thread_shared = Arc::clone(&shared);
        let result = crate::qos::spawn_lane_thread(
            "aterm-vitals-jobs",
            crate::qos::Role::Background,
            move || {
                let _ended = Ended(Arc::clone(&thread_shared));
                let mut core = Core::new();
                run(&thread_shared, &mut core, &mut read);
            },
        );
        if let Err(error) = result {
            shared.alive.store(false, Ordering::Release);
            aterm_log::warn!("system vitals: the jobs reader did not start: {error}");
        }
        Self {
            shared,
            reading: Reading::default(),
            replies: Vec::new(),
            publication: None,
            next: 0,
            reported_dead: false,
            current: None,
            admitted: false,
        }
    }

    fn work(&self) -> Work {
        let state = self.shared.lock();
        if state.need_input {
            Work::Input
        } else if state.need_publication
            || (!self.reported_dead && !self.shared.alive.load(Ordering::Acquire))
        {
            Work::Publish
        } else {
            Work::None
        }
    }

    fn observe(&mut self, evidence: Arc<Evidence>) {
        if self
            .reading
            .evidence
            .as_ref()
            .is_none_or(|old| !old.matches(&evidence))
        {
            self.admitted = false;
        }
        self.current = Some(evidence);
    }

    fn submit(&mut self, mut input: Input) {
        if !self.shared.alive.load(Ordering::Acquire) {
            return;
        }
        self.next = self.next.saturating_add(1);
        input.serial = self.next;
        let mut state = self.shared.lock();
        if state.closing {
            return;
        }
        if state
            .required_after
            .is_none_or(|after| input.reads.observed_at >= after)
        {
            state.need_input = false;
        }
        input.coalesced = std::mem::take(&mut state.discarded);
        if let Some(old) = state.input.take() {
            input.coalesced = input
                .coalesced
                .saturating_add(old.coalesced)
                .saturating_add(1);
        }
        state.input = Some(input);
        self.shared.changed.notify_one();
    }

    fn poll(&mut self) {
        let ready = {
            let mut state = self.shared.lock();
            state.need_publication = false;
            state.ready.take()
        };
        self.reported_dead = !self.shared.alive.load(Ordering::Acquire);
        if let Some(output) = ready {
            let same = self
                .reading
                .jobs
                .as_ref()
                .zip(output.reading.jobs.as_ref())
                .is_some_and(|(a, b)| Arc::ptr_eq(a, b));
            if !same {
                self.admitted = output
                    .reading
                    .jobs_at
                    .is_some_and(|at| at.elapsed() <= crate::vitals_jobs::JOBS_FRESH)
                    && output
                        .reading
                        .evidence
                        .as_ref()
                        .zip(self.current.as_ref())
                        .is_some_and(|(old, now)| old.matches(now));
            }
            self.reading = output.reading;
            assert!(
                self.publication.is_none(),
                "an unpublished assessment cannot be replaced"
            );
            self.publication = Some(output.publication);
            self.replies = output.replies;
        }
    }

    fn schedule_names(&mut self, names: Vec<crate::sys_naming::Request>) -> Vec<NameReply> {
        if names.is_empty() {
            return Vec::new();
        }
        let mut errors = Vec::new();
        {
            let mut state = self.shared.lock();
            for request in names {
                let error = if state.closing || !self.shared.alive.load(Ordering::Acquire) {
                    Some(crate::sys_naming::Error::Unavailable)
                } else if state.names.len() >= 2 {
                    Some(crate::sys_naming::Error::Busy)
                } else {
                    None
                };
                if let Some(error) = error {
                    errors.push((request, Err(error)));
                } else {
                    let at = Instant::now();
                    state.required_after = Some(state.required_after.map_or(at, |old| old.max(at)));
                    state.need_input = true;
                    state.names.push(Named { at, request });
                }
            }
        }
        self.shared.changed.notify_one();
        self.shared.wake(true);
        errors
    }

    fn published(&mut self, errors: Vec<NameReply>) {
        super::respond_names(errors);
        let replies = std::mem::take(&mut self.replies);
        super::respond_names(replies);
        if let Some(publication) = self.publication.take() {
            self.shared.lock().acknowledged = Some(publication);
            self.shared.changed.notify_one();
        }
    }

    fn stop(&mut self, stop: Option<(Vec<u64>, u64)>) {
        let mut state = self.shared.lock();
        state.closing = true;
        if stop.is_some() {
            state.stop = stop;
        }
        self.shared.changed.notify_one();
        let until = Instant::now() + super::STOP_BOUND;
        while self.shared.alive.load(Ordering::Acquire) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                aterm_log::warn!(
                    "system vitals: the jobs reader is still stopping; its closed-tab checkpoint is not confirmed"
                );
                break;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.closing = true;
        self.shared.changed.notify_one();
    }
}

struct Ended(Arc<Shared>);
impl Drop for Ended {
    fn drop(&mut self) {
        let names = {
            let mut state = self.0.lock();
            let names = std::mem::take(&mut state.names);
            state.need_input = false;
            state.need_publication = true;
            self.0.alive.store(false, Ordering::Release);
            self.0.changed.notify_all();
            names
        };
        for name in names {
            name.request
                .respond(Err(crate::sys_naming::Error::Unavailable));
        }
        self.0.wake(false);
    }
}

fn run(shared: &Shared, core: &mut Core, read: &mut impl FnMut(&mut Core, &Input) -> Reading) {
    let mut pending: Vec<NameReply> = Vec::new();
    let mut applied_after = None;
    loop {
        let (mut input, names) = {
            let mut state = shared.lock();
            loop {
                if let Some((told, at)) = state.told.take() {
                    drop(state);
                    core.left.keep_told(&told, at);
                    state = shared.lock();
                }
                if state.closing {
                    let stop = state.stop.take();
                    drop(state);
                    if let Some((told, at)) = stop {
                        core.left.at_stop(&told, at);
                    }
                    super::respond_names(
                        pending
                            .into_iter()
                            .map(|(request, _)| (request, Err(crate::sys_naming::Error::Canceled)))
                            .collect(),
                    );
                    return;
                }
                if let Some(input) = state.input.take() {
                    let required = state
                        .names
                        .iter()
                        .map(|name| name.at)
                        .chain(applied_after)
                        .max();
                    if required.is_some_and(|after| input.reads.observed_at < after) {
                        state.need_input = true;
                        state.discarded = state
                            .discarded
                            .saturating_add(input.coalesced)
                            .saturating_add(1);
                        shared.changed.notify_all();
                        drop(state);
                        shared.wake(true);
                        state = shared.lock();
                        continue;
                    }
                    let names = std::mem::take(&mut state.names);
                    break (input, names);
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        input.req.jobs_now |= !names.is_empty() || !pending.is_empty();
        let reading = read(core, &input);
        let mut replies = super::validate_names_on(
            &core.jobs,
            std::mem::take(&mut pending),
            JobsPass::name_is_shown,
        );
        applied_after = None;
        let answers = {
            let state = shared.lock();
            if state.closing {
                names
                    .into_iter()
                    .map(|name| (name.request, Err(crate::sys_naming::Error::Canceled)))
                    .collect()
            } else {
                super::prepare_names_on(
                    &mut core.jobs,
                    names.into_iter().map(|name| name.request).collect(),
                    JobsPass::capture_name,
                    JobsPass::apply_name,
                )
            }
        };
        for reply in answers {
            if matches!(&reply.1, Ok(crate::sys_naming::Answer::Applied { .. })) {
                pending.push(reply);
            } else {
                replies.push(reply);
            }
        }
        if !pending.is_empty() {
            let after = Instant::now();
            applied_after = Some(after);
            let mut state = shared.lock();
            state.required_after = Some(state.required_after.map_or(after, |old| old.max(after)));
            state.need_input = true;
        }
        let wake = !replies.is_empty() || (input.req.pull && input.req.jobs_now);
        if input.coalesced > 0 {
            aterm_log::info!(
                "system vitals: jobs observation {} followed {} coalesced pending observations; its original observation time is retained",
                input.serial,
                input.coalesced
            );
        }
        let publication = Arc::new(());
        {
            let mut state = shared.lock();
            assert!(
                state.ready.is_none(),
                "each assessment waits for publication"
            );
            state.ready = Some(Output {
                #[cfg(test)]
                serial: input.serial,
                publication: Arc::clone(&publication),
                reading,
                replies,
            });
            state.need_publication |= wake;
            shared.changed.notify_all();
        }
        if wake {
            shared.wake(false);
        }
        if applied_after.is_some() {
            shared.wake(true);
        }
        let mut state = shared.lock();
        while !state.closing
            && !state
                .acknowledged
                .as_ref()
                .is_some_and(|ack| Arc::ptr_eq(ack, &publication))
        {
            state = shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

#[cfg(test)]
#[path = "vitals_assessment_tests.rs"]
mod tests;
