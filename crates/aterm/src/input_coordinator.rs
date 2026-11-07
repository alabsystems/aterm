// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Own an already-admitted input task in a windowless Aqua runtime.

use aterm_cli::macos_doctor::{
    AutomaticSetupClaim, InitialInputPass, InitialSetupParkMark, InputSetupScope,
    PendingInputAdmission, SetupHooks, SetupNext,
};
use aterm_secrets::converge::{Last, RunnerRetry};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Default)]
pub(super) struct Coordinator {
    observed: Option<Result<InputSetupScope, ()>>,
    wanted: Option<u64>,
    worker: Option<JoinHandle<()>>,
    retry: RunnerRetry,
}

impl Coordinator {
    /// A failed sampling round observed no reliable transition. Keep the owned
    /// task and retry clock, and announce its next fresh scope again.
    pub(super) fn invalidate_observation(&mut self) {
        self.observed = None;
    }

    pub(super) fn round_with(
        &mut self,
        scope: &Result<InputSetupScope, String>,
        at_console: bool,
        now: u64,
        mut start: impl FnMut(u64) -> Result<Option<JoinHandle<()>>, String>,
        wake: impl Fn(),
    ) -> Result<(), String> {
        let observed = scope.as_ref().copied().map_err(|_| ());
        if self.observed.as_ref() != Some(&observed) {
            self.observed = Some(observed);
            // Retirement matters too: an unchanged service set must not leave a worker
            // parked after Stop, an external completion, or a newer choice.
            wake();
        }
        if let Ok(scope) = scope {
            self.wanted = match scope {
                InputSetupScope::Pending(revision) => Some(*revision),
                InputSetupScope::Applied(_) | InputSetupScope::Inactive => None,
            };
        }
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.worker.take().expect("finished worker").join();
            // A normal worker stays alive while parked. If it unexpectedly ends while
            // the task remains pending, rearm with the existing bounded runner backoff.
            self.retry.reaped(Last::Unfinished, now);
        }
        if self.worker.is_some() || !at_console || scope.is_err() {
            return Ok(());
        }
        let Some(revision) = self.wanted else {
            return Ok(());
        };
        if self
            .retry
            .remaining(now)
            .is_some_and(|pause| !pause.is_zero())
        {
            return Ok(());
        }
        // Retain wanted before asking for either lease. A busy claim, a held slot, or
        // failure to spawn cannot consume the only hardware notification.
        self.retry.started();
        match start(revision) {
            Ok(Some(worker)) => self.worker = Some(worker),
            Ok(None) => self.retry.reaped(Last::Held, now),
            Err(why) => {
                self.retry.reaped(Last::Failed, now);
                return Err(why);
            }
        }
        Ok(())
    }
}

/// The same setup slot, with no full-profile status read or healing on release.
struct InputBusy;

impl Drop for InputBusy {
    fn drop(&mut self) {
        super::INITIAL_SETUP_BUSY.store(false, std::sync::atomic::Ordering::Release);
    }
}

trait Ports {
    type Claim;
    type Busy;

    fn current(&mut self) -> Result<Option<PendingInputAdmission>, String>;
    fn apply(&mut self, ticket: &PendingInputAdmission)
    -> Result<Option<InitialInputPass>, String>;
    fn mark(&mut self, ticket: &PendingInputAdmission) -> InitialSetupParkMark;
    fn claim(&mut self) -> Result<Option<Self::Claim>, String>;
    fn busy(&mut self) -> Self::Busy;
    fn marks(&mut self) -> super::WakeMarks;
    fn wait(&mut self, seen: super::WakeMarks, facts: u64, limit: Option<Duration>);
    fn elapsed(&mut self) -> Duration;
    fn now(&mut self) -> u64;
}

struct Live {
    clock: std::time::Instant,
}

impl Ports for Live {
    type Claim = AutomaticSetupClaim;
    type Busy = InputBusy;

    fn current(&mut self) -> Result<Option<PendingInputAdmission>, String> {
        aterm_cli::macos_doctor::initial_setup_pending_inputs()
            .map(|ticket| ticket.filter(|ticket| !super::setup_stop_requested(ticket.revision())))
    }
    fn apply(
        &mut self,
        ticket: &PendingInputAdmission,
    ) -> Result<Option<InitialInputPass>, String> {
        struct Registration(u64);
        impl Drop for Registration {
            fn drop(&mut self) {
                super::clear_setup_run(self.0);
            }
        }
        // This registration exists only while the shared slot is held. It cannot clear
        // another whole-plan worker's registration while this task is parked.
        super::set_setup_run(Some(ticket.revision()));
        super::start_stop_worker();
        let _registered = Registration(ticket.revision());
        aterm_cli::macos_doctor::run_initial_input_pass(
            ticket,
            SetupHooks {
                stop: super::setup_stop_requested,
                stage: |_| {},
            },
        )
        .result
    }
    fn mark(&mut self, ticket: &PendingInputAdmission) -> InitialSetupParkMark {
        aterm_cli::macos_doctor::initial_setup_input_park_mark(ticket)
    }
    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        aterm_cli::macos_doctor::claim_automatic_setup()
    }
    fn busy(&mut self) -> Self::Busy {
        super::wait_for_setup_slot(|| InputBusy)
    }
    fn marks(&mut self) -> super::WakeMarks {
        super::SETUP_WAKES.marks()
    }
    fn wait(&mut self, seen: super::WakeMarks, facts: u64, limit: Option<Duration>) {
        super::SETUP_WAKES.wait_for_park(seen, facts, false, limit);
    }
    fn elapsed(&mut self) -> Duration {
        self.clock.elapsed()
    }
    fn now(&mut self) -> u64 {
        super::unix_seconds()
    }
}

pub(super) fn start(revision: u64) -> Result<Option<JoinHandle<()>>, String> {
    let mut ports = Live {
        clock: std::time::Instant::now(),
    };
    let Some(ticket) = ports
        .current()?
        .filter(|ticket| ticket.revision() == revision)
    else {
        return Ok(None);
    };
    if !super::take_initial_setup_slot_now() {
        return Ok(None);
    }
    let busy = InputBusy;
    let Some(claim) = ports.claim()? else {
        return Ok(None);
    };
    spawn(ports, ticket, claim, busy).map(Some)
}

fn spawn<P: Ports + Send + 'static>(
    ports: P,
    ticket: PendingInputAdmission,
    claim: P::Claim,
    busy: P::Busy,
) -> Result<JoinHandle<()>, String>
where
    P::Claim: Send + 'static,
    P::Busy: Send + 'static,
{
    std::thread::Builder::new()
        .name("aterm-input-setup".into())
        .spawn(move || {
            let revision = ticket.revision();
            let mut host = InputRetry {
                ports,
                ticket,
                seen: super::WakeMarks::default(),
                due: super::SetupRetryDeadlines::default(),
                before: None,
                last_failure: None,
            };
            super::run_setup_retry(&mut host, Some(claim), busy, revision);
        })
        .map_err(|why| format!("keyboard setup worker could not start: {why}"))
}

struct InputRetry<P> {
    ports: P,
    ticket: PendingInputAdmission,
    seen: super::WakeMarks,
    due: super::SetupRetryDeadlines,
    before: Option<InitialSetupParkMark>,
    last_failure: Option<String>,
}

impl<P: Ports> super::SetupRetryHost for InputRetry<P> {
    type Claim = P::Claim;
    type Busy = P::Busy;

    fn revision(&mut self) -> Result<u64, String> {
        Ok(self.ports.current()?.map_or(0, |ticket| ticket.revision()))
    }
    fn finished(&mut self) -> Result<bool, String> {
        Ok(self.ports.current()?.as_ref() != Some(&self.ticket))
    }
    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        self.ports.claim()
    }
    fn busy(&mut self) -> Self::Busy {
        self.ports.busy()
    }
    fn register(&mut self) {}
    fn apply(&mut self) -> Result<super::PassNext, String> {
        self.seen = self.ports.marks();
        self.before = Some(self.ports.mark(&self.ticket));
        let Some(pass) = self.ports.apply(&self.ticket)? else {
            return Ok(super::PassNext::Done);
        };
        Ok(if pass.applied {
            super::PassNext::Done
        } else {
            match pass.next {
                SetupNext::Done | SetupNext::Retry => super::PassNext::Retry,
                SetupNext::Park => super::PassNext::Park,
            }
        })
    }
    fn backoff(&mut self) -> bool {
        self.wait(Some(Duration::from_secs(60)))
    }
    fn park(&mut self) -> bool {
        self.wait(None)
    }
    fn retrying(&mut self, _: bool) {}
    fn failure(&mut self, why: &str) {
        if self.last_failure.as_deref() != Some(why) {
            aterm_log::info!("keyboard setup waits: {why}");
            self.last_failure = Some(why.into());
        }
    }
    fn completed(&mut self) {}
}

impl<P: Ports> InputRetry<P> {
    fn wait(&mut self, backoff: Option<Duration>) -> bool {
        let started = self.ports.elapsed();
        let seen = self.seen;
        let due_seen = self.due;
        let before = self.before.take();
        let ports = std::cell::RefCell::new(&mut self.ports);
        let facts = std::cell::Cell::new(seen.facts);
        let limit = std::cell::Cell::new(backoff);
        let mut previous = None;
        let mark = super::wait_for_setup_park_change_since(
            before.as_ref(),
            || {
                let mut ports = ports.borrow_mut();
                facts.set(ports.marks().facts);
                let mut mark = ports.mark(&self.ticket);
                if let Some(known) = &previous {
                    mark.retain_known_from(known);
                }
                // The existing keeper observes scope/console/HID facts. Unavailable
                // observations retain a minute fallback, without observing other stages.
                let parked = super::park_wait_limit(mark.next_due(), ports.now(), false, false);
                let remaining = backoff.map(|wait| wait.saturating_sub(ports.elapsed() - started));
                limit.set(match (parked, remaining) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                });
                previous = Some(mark.clone());
                mark
            },
            |mark| {
                due_seen.unseen(None, mark.mac_settings_retry_due())
                    || backoff.is_some_and(|wait| ports.borrow_mut().elapsed() - started >= wait)
            },
            InitialSetupParkMark::facts_changed_since,
            || !ports.borrow_mut().marks().ends_run_wait(seen, false),
            || ports.borrow_mut().wait(seen, facts.get(), limit.get()),
        );
        self.due.consume(None, mark.mac_settings_retry_due());
        self.seen = ports.borrow_mut().marks();
        before
            .as_ref()
            .is_some_and(|before| mark.facts_changed_since(before))
    }
}

#[cfg(test)]
pub(super) mod fixture {
    use super::*;
    use aterm_cli::macos_doctor::InitialSetupAdmissionStopTest;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};

    pub enum Event {
        Waiting,
        Finished,
    }

    pub struct Native {
        root: PathBuf,
        pub attached: AtomicBool,
        wakes: super::super::SetupWakes,
        events: mpsc::Sender<Event>,
    }

    impl Native {
        pub fn new(root: PathBuf) -> (Arc<Self>, mpsc::Receiver<Event>) {
            let (events, receiver) = mpsc::channel();
            (
                Arc::new(Self {
                    root,
                    attached: AtomicBool::new(false),
                    wakes: super::super::SetupWakes::new(),
                    events,
                }),
                receiver,
            )
        }

        pub fn start(self: &Arc<Self>, revision: u64) -> Result<Option<JoinHandle<()>>, String> {
            let setup = InitialSetupAdmissionStopTest::startup(&self.root)?;
            let Some(ticket) = setup
                .pending_inputs()?
                .filter(|ticket| ticket.revision() == revision)
            else {
                return Ok(None);
            };
            let Some(claim) = aterm_cli::macos_doctor::claim_initial_setup_rerun_at(&self.root)?
            else {
                return Ok(None);
            };
            spawn(
                NativePorts {
                    native: Arc::clone(self),
                    clock: std::time::Instant::now(),
                },
                ticket,
                claim,
                (),
            )
            .map(Some)
        }

        pub fn wake(&self) {
            self.wakes.raise_facts();
        }
    }

    struct NativePorts {
        native: Arc<Native>,
        clock: std::time::Instant,
    }

    impl Drop for NativePorts {
        fn drop(&mut self) {
            let _ = self.native.events.send(Event::Finished);
        }
    }

    impl Ports for NativePorts {
        type Claim = AutomaticSetupClaim;
        type Busy = ();

        fn current(&mut self) -> Result<Option<PendingInputAdmission>, String> {
            InitialSetupAdmissionStopTest::startup(&self.native.root)?.pending_inputs()
        }
        fn apply(
            &mut self,
            ticket: &PendingInputAdmission,
        ) -> Result<Option<InitialInputPass>, String> {
            InitialSetupAdmissionStopTest::startup(&self.native.root)?.native_input_pass(
                ticket,
                || {
                    Box::new(InitialSetupAdmissionStopTest::keyboard_system(
                        self.native.attached.load(Ordering::Acquire),
                    ))
                },
            )
        }
        fn mark(&mut self, ticket: &PendingInputAdmission) -> InitialSetupParkMark {
            InitialSetupAdmissionStopTest::startup(&self.native.root)
                .unwrap()
                .input_park_mark(
                    ticket,
                    &InitialSetupAdmissionStopTest::keyboard_system(
                        self.native.attached.load(Ordering::Acquire),
                    ),
                )
        }
        fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
            aterm_cli::macos_doctor::claim_initial_setup_rerun_at(&self.native.root)
        }
        fn busy(&mut self) -> Self::Busy {}
        fn marks(&mut self) -> super::super::WakeMarks {
            self.native.wakes.marks()
        }
        fn wait(&mut self, seen: super::super::WakeMarks, facts: u64, limit: Option<Duration>) {
            let _ = self.native.events.send(Event::Waiting);
            self.native.wakes.wait_for_park(seen, facts, false, limit);
        }
        fn elapsed(&mut self) -> Duration {
            self.clock.elapsed()
        }
        fn now(&mut self) -> u64 {
            super::super::unix_seconds()
        }
    }

    pub fn waiting(events: &mpsc::Receiver<Event>) {
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(60)).unwrap(),
            Event::Waiting
        ));
    }

    pub fn finished(events: &mpsc::Receiver<Event>, coordinator: &mut Coordinator) {
        loop {
            if matches!(
                events.recv_timeout(Duration::from_secs(60)).unwrap(),
                Event::Finished
            ) {
                break;
            }
        }
        coordinator.worker.take().unwrap().join().unwrap();
    }
}
