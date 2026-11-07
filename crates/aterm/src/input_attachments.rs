// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Activate accepted keyboard defaults when this login gets a new HID service.

use aterm_cli::macos_doctor::{InputServiceSet, InputSetupScope};
use aterm_secrets::converge::{Last, RunnerRetry};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Keeper {
    settled: Option<(u64, Vec<u64>)>,
    attempted: Option<(u64, Vec<u64>)>,
    retry: RunnerRetry,
}

#[derive(Default)]
struct Inputs {
    keeper: Keeper,
    pending: Option<(u64, InputServiceSet)>,
}

/// Mutable task ownership outlives a failed sampling thread. Only observations are
/// invalidated after a panic; finite input/FDA workers retain their one owner.
#[derive(Default)]
struct Actor {
    inputs: Inputs,
    console: ConsoleReturn,
    coordinator: super::input_coordinator::Coordinator,
    follow_up: super::withheld_preferences::Coordinator,
    follow_up_said: Option<String>,
    said: Option<String>,
}

#[derive(Clone)]
struct Notifications {
    changed: fn(),
    console_return: Option<fn() -> Result<(), String>>,
    pending: PendingOwner,
    ended: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct Supervisor {
    clock: Instant,
    actor: Arc<Mutex<Actor>>,
    stop: Arc<AtomicBool>,
    ending: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    retry: RunnerRetry,
    notifications: Notifications,
}

impl Supervisor {
    fn new(notifications: Notifications) -> Self {
        Self {
            clock: Instant::now(),
            actor: Arc::default(),
            stop: Arc::new(AtomicBool::new(false)),
            ending: Arc::new(AtomicBool::new(false)),
            worker: None,
            retry: RunnerRetry::default(),
            notifications,
        }
    }

    /// Healthy host ticks inspect only the owned thread handle. No actor lock or
    /// preference/HID read runs on a window's event loop.
    fn maintain(&mut self) -> Option<Instant> {
        let clock = self.clock;
        let notifications = self.notifications.clone();
        self.maintain_with(|actor, stop, ending| {
            spawn_worker(actor, stop, ending, clock, notifications)
        })
    }

    fn maintain_with(
        &mut self,
        start: impl FnOnce(
            Arc<Mutex<Actor>>,
            Arc<AtomicBool>,
            Arc<AtomicBool>,
        ) -> std::io::Result<JoinHandle<()>>,
    ) -> Option<Instant> {
        let now = u64::try_from(self.clock.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Err(why) = self.round_with(now, start) {
            super::warn_when_logging(format!("keyboard attachment activation waits: {why}"));
        }
        if self.worker.is_some() {
            // The exit wake is necessarily sent before JoinHandle becomes finished.
            // Keep its completion handoff alive until the handle can be reaped.
            return self.ending.load(Ordering::Acquire).then(|| {
                Instant::now() + Duration::from_millis(aterm_secrets::converge::FIRST_RETRY_MS)
            });
        }
        self.retry.remaining(now).map(|left| Instant::now() + left)
    }

    fn round_with(
        &mut self,
        now: u64,
        start: impl FnOnce(
            Arc<Mutex<Actor>>,
            Arc<AtomicBool>,
            Arc<AtomicBool>,
        ) -> std::io::Result<JoinHandle<()>>,
    ) -> std::io::Result<()> {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let panicked = self
                .worker
                .take()
                .expect("finished sampler")
                .join()
                .is_err();
            if panicked {
                // The joined sampler holds no actor guard now. Keep its coordinator
                // handles, discard cached observations, and start with fresh authority.
                let mut actor = self.actor.lock().unwrap_or_else(PoisonError::into_inner);
                actor.inputs = Inputs::default();
                actor.coordinator.invalidate_observation();
                actor.console = ConsoleReturn {
                    away: true,
                    pending: false,
                };
                actor.said = None;
                actor.follow_up_said = None;
                self.actor.clear_poison();
            }
            self.retry.reaped(Last::Failed, now);
        }
        if self.worker.is_some()
            || self
                .retry
                .remaining(now)
                .is_some_and(|left| !left.is_zero())
        {
            return Ok(());
        }
        self.retry.started();
        self.ending.store(false, Ordering::Release);
        match start(
            Arc::clone(&self.actor),
            Arc::clone(&self.stop),
            Arc::clone(&self.ending),
        ) {
            Ok(worker) => self.worker = Some(worker),
            Err(why) => {
                self.retry.reaped(Last::Failed, now);
                return Err(why);
            }
        }
        Ok(())
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
enum PendingOwner {
    Window(fn()),
    Background,
}

impl PendingOwner {
    fn coordinate(
        self,
        coordinator: &mut super::input_coordinator::Coordinator,
        scope: &Result<InputSetupScope, String>,
        at_console: bool,
        now: u64,
        start: impl FnMut(u64) -> Result<Option<std::thread::JoinHandle<()>>, String>,
        wake: impl Fn(),
    ) -> Result<(), String> {
        match self {
            Self::Background => coordinator.round_with(scope, at_console, now, start, wake),
            Self::Window(_) => Ok(()),
        }
    }

    fn changed(self) -> fn() {
        match self {
            Self::Window(wake) => wake,
            Self::Background => super::wake_setup_facts,
        }
    }
}

impl Inputs {
    fn round(
        &mut self,
        at_console: bool,
        now_ms: u64,
        scope: impl FnOnce() -> Result<InputSetupScope, String>,
        services: impl FnOnce() -> Result<Vec<u64>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<bool>, String>,
        pending_changed: &dyn Fn(),
    ) -> Result<bool, String> {
        if !at_console {
            *self = Self::default();
            return Ok(false);
        }
        match scope()? {
            InputSetupScope::Inactive => {
                *self = Self::default();
                Ok(false)
            }
            InputSetupScope::Applied(revision) => {
                self.pending = None;
                self.keeper
                    .round(now_ms, Some(revision), services, activate)
            }
            InputSetupScope::Pending(revision) => {
                self.keeper = Keeper::default();
                let current = (revision, InputServiceSet::new(services()?)?);
                if self.pending.as_ref() != Some(&current) {
                    self.pending = Some(current);
                    // The first sample also nudges: a device can arrive after the parked pass
                    // read its mark but before this observer saw its pending scope. Only the
                    // worker's before/current facts may turn this nudge into another pass.
                    pending_changed();
                }
                Ok(false)
            }
        }
    }
}

#[derive(Default)]
struct ConsoleReturn {
    away: bool,
    pending: bool,
}

impl ConsoleReturn {
    fn recover(
        &mut self,
        at_console: bool,
        wake: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        if !at_console {
            self.away = true;
            self.pending = false;
            return Ok(());
        }
        self.pending |= std::mem::take(&mut self.away);
        if self.pending {
            // A setup pass can hold its records as the console returns. Keep this one
            // observation until its current scope can be read, rather than lose the return.
            wake()?;
            self.pending = false;
        }
        Ok(())
    }
}

impl Keeper {
    #[cfg(test)]
    fn round_at_console(
        &mut self,
        sys: &dyn aterm_machines::port::System,
        now_ms: u64,
        revision: impl FnOnce() -> Result<Option<u64>, String>,
        services: impl FnOnce() -> Result<Vec<u64>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<bool>, String>,
    ) -> Result<bool, String> {
        self.round_with_console(
            sys.euid() != 0 && !aterm_machines::console::remote(sys),
            now_ms,
            revision,
            services,
            activate,
        )
    }

    #[cfg(test)]
    fn round_with_console(
        &mut self,
        at_console: bool,
        now_ms: u64,
        revision: impl FnOnce() -> Result<Option<u64>, String>,
        services: impl FnOnce() -> Result<Vec<u64>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<bool>, String>,
    ) -> Result<bool, String> {
        if !at_console {
            *self = Self::default();
            return Ok(false);
        }
        self.round(now_ms, revision()?, services, activate)
    }

    fn round(
        &mut self,
        now_ms: u64,
        revision: Option<u64>,
        services: impl FnOnce() -> Result<Vec<u64>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<bool>, String>,
    ) -> Result<bool, String> {
        let Some(revision) = revision else {
            *self = Self::default();
            return Ok(false);
        };
        let services = InputServiceSet::new(services()?)?.into_vec();
        let current = (revision, services);
        if self.settled.as_ref() == Some(&current) {
            return Ok(false);
        }
        let is_new = |id: &u64| {
            self.settled
                .as_ref()
                .is_none_or(|(before, ids)| *before != revision || ids.binary_search(id).is_err())
        };
        if !current.1.iter().any(&is_new) {
            self.settled = Some(current);
            return Ok(false);
        }
        if self.attempted.as_ref() != Some(&current) {
            self.retry = RunnerRetry::default();
        } else if self
            .retry
            .remaining(now_ms)
            .is_some_and(|pause| !pause.is_zero())
        {
            return Ok(false);
        }
        let new: Vec<_> = current.1.iter().copied().filter(is_new).collect();
        self.retry.started();
        let result = activate(revision, &new);
        self.retry.reaped(
            match &result {
                Ok(Some(true)) => Last::Ended,
                Ok(None) => Last::Held,
                Ok(Some(false)) => Last::Unfinished,
                Err(_) => Last::Failed,
            },
            now_ms,
        );
        if matches!(result, Ok(Some(true))) {
            if self.attempted.as_ref() != Some(&current) {
                self.attempted = Some(current.clone());
            }
            self.settled = Some(current);
            return Ok(true);
        }
        if self.attempted.as_ref() != Some(&current) {
            self.attempted = Some(current);
        }
        result.map(|_| false)
    }
}

/// Constructed only by the validated subscriber runtime. Its existing local-input
/// lane maintains the keeper even when sharing holds repository observation.
pub(super) fn subscriber_inputs(
    paths: &aterm_secrets::client::Paths,
) -> aterm_secrets::client::SubscriberInputs {
    let maintenance = spawn_background();
    let inputs = super::converge_inputs::observe(paths);
    match maintenance {
        Some(maintain) => inputs.with_maintenance(Box::new(move || maintain())),
        None => inputs,
    }
}

/// Start only inside the node job's owned startup, after its boot and exclusive lease.
pub(super) fn node_serve(startup: impl FnOnce()) -> ExitCode {
    node_serve_with(aterm_gui::run_node_serve_owning, spawn_background, startup)
}

fn node_serve_with<'a, R, S: 'a>(
    own: impl FnOnce(Box<dyn FnOnce() -> S + 'a>) -> R,
    start: impl FnOnce() -> S + 'a,
    runtime: impl FnOnce() + 'a,
) -> R {
    own(Box::new(move || {
        let maintenance = start();
        runtime();
        maintenance
    }))
}

fn spawn_background() -> Option<aterm_gui::NodeMaintenance> {
    let mut maintenance = None;
    spawn_background_with(
        &aterm_machines::port::Live::new(),
        |changed, console_return, pending| {
            maintenance = supervisor(Notifications {
                changed,
                console_return,
                pending,
                ended: None,
            })
            .map(|owner| {
                let owner = Mutex::new(owner);
                Arc::new(move || {
                    if let Ok(mut owner) = owner.try_lock() {
                        let _ = owner.maintain();
                    }
                }) as aterm_gui::NodeMaintenance
            });
        },
    );
    maintenance
}

fn spawn_background_with(
    sys: &dyn aterm_machines::port::System,
    start: impl FnOnce(fn(), Option<fn() -> Result<(), String>>, PendingOwner),
) {
    start_background_with(sys, || start(|| {}, None, PendingOwner::Background));
}

fn start_background_with(sys: &dyn aterm_machines::port::System, start: impl FnOnce()) {
    // This process's Aqua session survives Fast User Switching. Console ownership is a
    // round's changing fact, not a permanent reason to omit its keeper at process birth.
    if sys.euid() != 0 && !sys.remote_login() && sys.graphic_session() == Some(true) {
        start();
    }
}

pub(super) fn spawn() -> Option<aterm_gui::HostMaintenance> {
    supervisor(Notifications {
        changed: || {
            super::request_observation(
                None,
                vec![aterm_types::macos_setup::Subject::Setting(
                    aterm_types::macos_setup::Setting::CapsLock,
                )],
                false,
            );
        },
        console_return: Some(super::wake_setup_for_console),
        pending: PendingOwner::Window(super::wake_setup_facts),
        ended: Some(Arc::new(aterm_gui::wake_host_maintenance)),
    })
    .map(|mut owner| Box::new(move || owner.maintain()) as aterm_gui::HostMaintenance)
}

fn supervisor(notifications: Notifications) -> Option<Supervisor> {
    if aterm_types::dirs::runs_under_private_state_root() || std::env::var_os("CI").is_some() {
        return None;
    }
    let mut owner = Supervisor::new(notifications);
    let _ = owner.maintain();
    Some(owner)
}

fn spawn_worker(
    actor: Arc<Mutex<Actor>>,
    stop: Arc<AtomicBool>,
    ending: Arc<AtomicBool>,
    clock: Instant,
    notifications: Notifications,
) -> std::io::Result<JoinHandle<()>> {
    spawn_worker_with(
        actor,
        stop,
        ending,
        clock,
        notifications,
        |actor, stop, clock, notifications| {
            use aterm_machines::port::System as _;
            let _ = aterm_log::wait_for_logger(super::EARLY_LOG_WAIT);
            let sys = aterm_machines::port::Live::new();
            while !stop.load(Ordering::Acquire) {
                let mut actor = actor.lock().unwrap_or_else(PoisonError::into_inner);
                let Actor {
                    inputs,
                    console,
                    coordinator,
                    follow_up,
                    follow_up_said,
                    said,
                } = &mut *actor;
                // One current console sample and one canonical input-scope read select accepted
                // activation or the GUI's narrowly scoped observation of still-owed Caps work.
                let at_console = sys.euid() != 0 && !aterm_machines::console::remote(&sys);
                if let Some(wake) = notifications.console_return {
                    let _ = console.recover(at_console, wake);
                }
                let now = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
                if matches!(notifications.pending, PendingOwner::Window(_)) {
                    match follow_up.round(now) {
                        Ok(()) => *follow_up_said = None,
                        Err(why) if follow_up_said.as_ref() != Some(&why) => {
                            aterm_log::info!("deferred preferences wait for a worker: {why}");
                            *follow_up_said = Some(why);
                        }
                        Err(_) => {}
                    }
                }
                let scope = aterm_cli::macos_doctor::input_setup_scope();
                let owned = notifications.pending.coordinate(
                    coordinator,
                    &scope,
                    at_console,
                    now,
                    super::input_coordinator::start,
                    super::wake_setup_facts,
                );
                let result = owned.and_then(|()| {
                    inputs.round(
                        at_console,
                        now,
                        || scope,
                        || sys.keyboard_service_ids(),
                        |revision, ids| {
                            aterm_cli::macos_doctor::activate_input_attachments(
                                revision,
                                ids,
                                super::setup_stop_requested,
                            )
                        },
                        &notifications.pending.changed(),
                    )
                });
                match result {
                    Ok(true) => {
                        (notifications.changed)();
                        *said = None;
                    }
                    Ok(false) => {}
                    Err(why) if said.as_ref() != Some(&why) => {
                        aterm_log::info!("keyboard attachment activation waits: {why}");
                        *said = Some(why);
                    }
                    Err(_) => {}
                }
                drop(actor);
                std::thread::sleep(Duration::from_secs(2));
            }
        },
    )
}

fn spawn_worker_with(
    actor: Arc<Mutex<Actor>>,
    stop: Arc<AtomicBool>,
    ending: Arc<AtomicBool>,
    clock: Instant,
    notifications: Notifications,
    run: impl FnOnce(Arc<Mutex<Actor>>, Arc<AtomicBool>, Instant, Notifications) + Send + 'static,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("aterm-input-attachments".into())
        .spawn(move || {
            struct Ended {
                notice: Arc<AtomicBool>,
                wake: Option<Arc<dyn Fn() + Send + Sync>>,
            }
            impl Drop for Ended {
                fn drop(&mut self) {
                    self.notice.store(true, Ordering::Release);
                    if let Some(wake) = &self.wake {
                        wake();
                    }
                }
            }
            let _ended = Ended {
                notice: ending,
                wake: notifications.ended.clone(),
            };
            run(actor, stop, clock, notifications);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_pending_caps(
        root: &std::path::Path,
    ) -> (
        aterm_cli::macos_doctor::InitialSetupAdmissionStopTest<'_>,
        u64,
    ) {
        use aterm_cli::macos_doctor::{
            InitialSetupAdmissionFault, InitialSetupAdmissionStopTest, SetupNext,
            input_setup_scope_at,
        };
        use aterm_types::macos_setup::{Plan, Selection, Setting};

        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        let mut setup = InitialSetupAdmissionStopTest::startup(root)
            .unwrap()
            .with_plan(Plan::from(selected));
        let revision = setup
            .persist(InitialSetupAdmissionFault::None)
            .unwrap()
            .unwrap();
        let (report, next) = setup
            .native_workstation_pass(revision, || {
                Box::new(InitialSetupAdmissionStopTest::keyboard_system(false))
            })
            .unwrap()
            .unwrap();
        assert!(report.needs_attention(), "{}", report.to_text());
        assert_eq!(next, SetupNext::Retry);
        assert_eq!(
            input_setup_scope_at(root).unwrap(),
            InputSetupScope::Pending(revision)
        );
        assert_eq!(setup.input_activation_revision().unwrap(), None);
        (setup, revision)
    }

    #[test]
    fn a_refused_keeper_start_does_not_abandon_the_native_pending_caps_task() {
        use super::super::input_coordinator::fixture;
        use aterm_cli::macos_doctor::input_setup_scope_at;
        use aterm_secrets::converge::FIRST_RETRY_MS;

        let dir = aterm_tempfile::tempdir().unwrap();
        let (setup, revision) = native_pending_caps(dir.path());
        let (native, events) = fixture::Native::new(dir.path().to_path_buf());
        let notifications = Notifications {
            changed: || {},
            console_return: None,
            pending: PendingOwner::Background,
            ended: None,
        };
        let mut owner = Supervisor::new(notifications.clone());
        assert!(
            owner
                .round_with(0, |_, _, _| Err(std::io::Error::other(
                    "injected keeper spawn refusal"
                )))
                .is_err()
        );
        owner
            .round_with(FIRST_RETRY_MS - 1, |_, _, _| {
                panic!("retained start backoff")
            })
            .unwrap();
        let root = dir.path().to_path_buf();
        let native_waiting = Arc::clone(&native);
        let clock = owner.clock;
        owner
            .round_with(FIRST_RETRY_MS, |actor, stop, ending| {
                spawn_worker_with(
                    actor,
                    stop,
                    ending,
                    clock,
                    notifications.clone(),
                    move |actor, _, observed_clock, notifications| {
                        assert_eq!(observed_clock, clock, "one retained timebase");
                        let mut actor = actor.lock().unwrap();
                        notifications
                            .pending
                            .coordinate(
                                &mut actor.coordinator,
                                &input_setup_scope_at(&root),
                                true,
                                FIRST_RETRY_MS,
                                |revision| native_waiting.start(revision),
                                || native_waiting.wake(),
                            )
                            .unwrap();
                        actor
                            .inputs
                            .round(
                                true,
                                FIRST_RETRY_MS,
                                || input_setup_scope_at(&root),
                                || Ok(vec![41]),
                                |_, _| panic!("pending input has no setter authority"),
                                &|| {},
                            )
                            .unwrap();
                        panic!("injected sampler panic while its input worker remains owned");
                    },
                )
            })
            .unwrap();
        fixture::waiting(&events);
        let until = Instant::now() + Duration::from_secs(60);
        while !owner.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < until, "finite sampler did not end");
            std::thread::sleep(Duration::from_millis(1));
        }
        owner
            .round_with(FIRST_RETRY_MS, |_, _, _| panic!("reap before retry"))
            .unwrap();
        assert!(
            owner.actor.lock().unwrap().inputs.pending.is_none(),
            "discard failed observations"
        );
        let root = dir.path().to_path_buf();
        let now = FIRST_RETRY_MS * 3;
        owner
            .round_with(now, |actor, stop, ending| {
                spawn_worker_with(
                    actor,
                    stop,
                    ending,
                    clock,
                    notifications,
                    move |actor, _, observed_clock, notifications| {
                        assert_eq!(observed_clock, clock);
                        let mut actor = actor.lock().unwrap();
                        notifications
                            .pending
                            .coordinate(
                                &mut actor.coordinator,
                                &input_setup_scope_at(&root),
                                true,
                                now,
                                |_| panic!("the retained input worker still owns this task"),
                                || native.wake(),
                            )
                            .unwrap();
                        native.attached.store(true, Ordering::Release);
                        native.wake();
                        fixture::finished(&events, &mut actor.coordinator);
                    },
                )
            })
            .unwrap();
        owner.worker.take().unwrap().join().unwrap();
        assert_eq!(setup.input_activation_revision().unwrap(), Some(revision));
    }

    #[test]
    fn a_window_keeps_the_exit_handoff_until_the_sampler_is_joinable() {
        use std::sync::mpsc;

        let (sampled, sampled_rx) = mpsc::channel();
        let (finish, finish_rx) = mpsc::channel();
        let (woke, woke_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let notifications = Notifications {
            changed: || {},
            console_return: None,
            pending: PendingOwner::Window(|| {}),
            ended: Some(Arc::new(move || {
                woke.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap();
            })),
        };
        let mut owner = Supervisor::new(notifications.clone());
        let clock = owner.clock;
        assert!(
            owner
                .maintain_with(|actor, stop, ending| {
                    spawn_worker_with(
                        actor,
                        stop,
                        ending,
                        clock,
                        notifications,
                        move |_, _, _, _| {
                            sampled.send(()).unwrap();
                            finish_rx.recv_timeout(Duration::from_secs(60)).unwrap();
                        },
                    )
                })
                .is_none()
        );
        sampled_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(
            owner
                .maintain_with(|_, _, _| panic!("one live sampler"))
                .is_none(),
            "healthy idle windows gain no timer"
        );
        finish.send(()).unwrap();
        woke_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(
            !owner.worker.as_ref().unwrap().is_finished(),
            "the actual exit callback still holds the worker"
        );
        assert!(
            owner
                .maintain_with(|_, _, _| panic!("join before replacement"))
                .is_some(),
            "an early exit wake must leave a deadline"
        );
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        while !owner.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < until, "exit handoff did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            owner
                .maintain_with(|_, _, _| panic!("retained restart backoff"))
                .is_some()
        );
        assert!(owner.worker.is_none(), "the host reaped its owned handle");
    }

    #[test]
    fn headless_watch_hands_the_original_pending_caps_pass_back_to_a_coordinator_on_attach() {
        use super::super::input_coordinator::{Coordinator, fixture};
        use aterm_cli::macos_doctor::{
            InitialSetupAdmissionStopTest, SetupNext, input_setup_scope_at,
        };
        use aterm_machines::port::System as _;
        use std::sync::atomic::Ordering;

        let positive = aterm_tempfile::tempdir().unwrap();
        let (ready, revision) = native_pending_caps(positive.path());
        let (report, next) = ready
            .native_workstation_pass(revision, || {
                Box::new(InitialSetupAdmissionStopTest::keyboard_system(true))
            })
            .unwrap()
            .unwrap();
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert_eq!(next, SetupNext::Done);
        assert_eq!(ready.input_activation_revision().unwrap(), Some(revision));

        let dir = aterm_tempfile::tempdir().unwrap();
        let (setup, revision) = native_pending_caps(dir.path());
        let mut actor = InitialSetupAdmissionStopTest::keyboard_system(false);
        actor.terminal = false;
        let (native, events) = fixture::Native::new(dir.path().to_path_buf());
        let mut coordinator = Coordinator::default();
        let mut inputs = Inputs::default();
        spawn_background_with(&actor, |_, _, owner| {
            for attached in [false, true] {
                native.attached.store(attached, Ordering::Release);
                let scope = input_setup_scope_at(dir.path());
                owner
                    .coordinate(
                        &mut coordinator,
                        &scope,
                        true,
                        0,
                        |revision| native.start(revision),
                        || native.wake(),
                    )
                    .unwrap();
                let hand_off = || native.wake();
                assert!(
                    !inputs
                        .round(
                            true,
                            0,
                            || scope,
                            || {
                                InitialSetupAdmissionStopTest::keyboard_system(attached)
                                    .keyboard_service_ids()
                            },
                            |_, _| panic!("a pending pass has no attachment-setter authority"),
                            &hand_off,
                        )
                        .unwrap()
                );
                if !attached {
                    fixture::waiting(&events);
                }
            }
        });
        fixture::finished(&events, &mut coordinator);
        assert_eq!(
            input_setup_scope_at(dir.path()).unwrap(),
            InputSetupScope::Applied(revision),
            "the actual owned retry coordinator checkpoints verified Caps in the original run"
        );
        assert_eq!(setup.input_activation_revision().unwrap(), Some(revision));
    }

    #[test]
    fn headless_watch_never_resumes_a_stopped_native_pending_caps_pass() {
        use aterm_cli::macos_doctor::{InitialSetupAdmissionStopTest, input_setup_scope_at};
        use std::cell::Cell;

        let dir = aterm_tempfile::tempdir().unwrap();
        let (setup, revision) = native_pending_caps(dir.path());
        setup.stop_revision(revision).unwrap();
        assert_eq!(
            input_setup_scope_at(dir.path()).unwrap(),
            InputSetupScope::Inactive
        );
        let actor = InitialSetupAdmissionStopTest::keyboard_system(true);
        let started = Cell::new(false);
        let mut inputs = Inputs::default();
        spawn_background_with(&actor, |_, _, owner| {
            started.set(true);
            owner
                .coordinate(
                    &mut super::super::input_coordinator::Coordinator::default(),
                    &input_setup_scope_at(dir.path()),
                    true,
                    0,
                    |_| panic!("Stop withholds coordinator startup"),
                    || {},
                )
                .unwrap();
            let stopped_handoff = || -> () {
                panic!("Stop withholds the coordinator handoff");
            };
            inputs
                .round(
                    true,
                    0,
                    || input_setup_scope_at(dir.path()),
                    || panic!("Stop withholds the pending hardware read"),
                    |_, _| panic!("Stop withholds activation"),
                    &stopped_handoff,
                )
                .unwrap();
        });
        assert!(started.get(), "the valid local Aqua startup branch ran");
    }

    #[test]
    fn the_headless_owner_retains_unchanged_work_until_the_actual_claim_is_released() {
        use super::super::input_coordinator::{Coordinator, fixture};
        use aterm_cli::macos_doctor::{claim_initial_setup_rerun_at, input_setup_scope_at};
        use std::sync::atomic::Ordering;

        let dir = aterm_tempfile::tempdir().unwrap();
        let (setup, revision) = native_pending_caps(dir.path());
        let claim = claim_initial_setup_rerun_at(dir.path()).unwrap().unwrap();
        let (native, events) = fixture::Native::new(dir.path().to_path_buf());
        native.attached.store(true, Ordering::Release);
        let actor = aterm_cli::macos_doctor::InitialSetupAdmissionStopTest::keyboard_system(true);
        let mut coordinator = Coordinator::default();
        spawn_background_with(&actor, |_, _, owner| {
            let scope = input_setup_scope_at(dir.path());
            owner
                .coordinate(
                    &mut coordinator,
                    &scope,
                    true,
                    0,
                    |revision| native.start(revision),
                    || native.wake(),
                )
                .unwrap();
            drop(claim);
            // No second attachment or scope change: the existing keeper round retains duty.
            owner
                .coordinate(
                    &mut coordinator,
                    &scope,
                    true,
                    5000,
                    |revision| native.start(revision),
                    || native.wake(),
                )
                .unwrap();
        });
        fixture::finished(&events, &mut coordinator);
        assert_eq!(setup.input_activation_revision().unwrap(), Some(revision));
    }

    #[test]
    fn a_waiting_headless_owner_retires_on_stop_or_completion_without_new_hardware() {
        use super::super::input_coordinator::{Coordinator, fixture};
        use aterm_cli::macos_doctor::{InitialSetupAdmissionStopTest, input_setup_scope_at};

        for stopped in [false, true] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let (setup, revision) = native_pending_caps(dir.path());
            let (native, events) = fixture::Native::new(dir.path().to_path_buf());
            let actor = InitialSetupAdmissionStopTest::keyboard_system(false);
            let mut coordinator = Coordinator::default();
            spawn_background_with(&actor, |_, _, owner| {
                owner
                    .coordinate(
                        &mut coordinator,
                        &input_setup_scope_at(dir.path()),
                        true,
                        0,
                        |revision| native.start(revision),
                        || native.wake(),
                    )
                    .unwrap();
                fixture::waiting(&events);
                if stopped {
                    setup.stop_revision(revision).unwrap();
                } else {
                    setup
                        .native_workstation_pass(revision, || {
                            Box::new(InitialSetupAdmissionStopTest::keyboard_system(true))
                        })
                        .unwrap()
                        .unwrap();
                }
                owner
                    .coordinate(
                        &mut coordinator,
                        &input_setup_scope_at(dir.path()),
                        true,
                        0,
                        |_| panic!("retired work must never start again"),
                        || native.wake(),
                    )
                    .unwrap();
            });
            fixture::finished(&events, &mut coordinator);
            assert_eq!(
                setup.input_activation_revision().unwrap(),
                (!stopped).then_some(revision)
            );
        }
    }

    #[test]
    fn pending_first_caps_nudges_only_facts_for_its_actual_admitted_scope() {
        use aterm_cli::macos_doctor::{
            InitialSetupAdmissionFault, InitialSetupAdmissionStopTest, input_setup_scope_at,
        };
        use aterm_types::macos_setup::{Plan, Selection, Setting};

        let dir = aterm_tempfile::tempdir().unwrap();
        let mut selected = Selection::none();
        selected.set(Setting::CapsLock, true);
        let mut setup = InitialSetupAdmissionStopTest::startup(dir.path())
            .unwrap()
            .with_plan(Plan::from(selected));
        let revision = setup
            .persist(InitialSetupAdmissionFault::None)
            .unwrap()
            .unwrap();
        let scope = || input_setup_scope_at(dir.path());
        assert_eq!(scope().unwrap(), InputSetupScope::Pending(revision));
        let wakes = crate::SetupWakes::new();
        let changed = || wakes.raise_facts();
        let mut inputs = Inputs::default();
        let observe = |inputs: &mut Inputs, ids| {
            inputs.round(
                true,
                0,
                scope,
                || Ok(ids),
                |_, _| panic!("pending observation has no setter authority"),
                &changed,
            )
        };
        let before = wakes.marks();
        assert!(!observe(&mut inputs, vec![41]).unwrap());
        let seeded = wakes.marks();
        assert_ne!(
            seeded.facts, before.facts,
            "the first sample closes the pass/observer gap"
        );
        assert!(!seeded.refreshed_since(before));
        assert!(!seeded.checked_since(before));
        assert!(!observe(&mut inputs, vec![41]).unwrap());
        assert_eq!(wakes.marks(), seeded, "unchanged samples owe no nudge");
        assert!(!observe(&mut inputs, vec![42, 41]).unwrap());
        let attached = wakes.marks();
        assert_ne!(attached.facts, seeded.facts);
        assert!(!attached.refreshed_since(before));
        assert!(!observe(&mut inputs, vec![41, 42]).unwrap());
        assert_eq!(
            wakes.marks(),
            attached,
            "a different enumeration order is no new device"
        );
        assert!(observe(&mut inputs, vec![41, 41]).is_err());
        assert_eq!(wakes.marks(), attached, "ambiguous IDs supply no fact");

        // A new keyboard can already be present when this observer first sees pending setup.
        let mut late = Inputs::default();
        assert!(!observe(&mut late, vec![41, 42]).unwrap());
        let first_after_attach = wakes.marks();
        assert_ne!(first_after_attach.facts, attached.facts);
        assert!(!first_after_attach.refreshed_since(before));
        assert!(!first_after_attach.checked_since(before));

        assert!(
            !inputs
                .round(
                    false,
                    0,
                    || panic!("another console reads no setup scope"),
                    || panic!("another console reads no hardware"),
                    |_, _| panic!("another console activates nothing"),
                    &changed
                )
                .unwrap()
        );
        setup.stop_revision(revision).unwrap();
        assert_eq!(scope().unwrap(), InputSetupScope::Inactive);
        assert!(
            !inputs
                .round(
                    true,
                    0,
                    scope,
                    || panic!("Stop withdraws even pending hardware observation"),
                    |_, _| panic!("Stop withdraws activation"),
                    &changed
                )
                .unwrap()
        );
        assert_eq!(wakes.marks(), first_after_attach);
    }

    #[test]
    fn a_console_return_reads_pending_setup_once_and_retains_only_an_unread_return() {
        use std::cell::Cell;
        let mut console = ConsoleReturn::default();
        let reads = Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            Ok(())
        };
        console
            .recover(true, || panic!("initial console ownership is no return"))
            .unwrap();
        console
            .recover(false, || panic!("another console reads no setup scope"))
            .unwrap();
        assert!(
            console
                .recover(true, || Err("records held".into()))
                .is_err()
        );
        console.recover(true, read).unwrap();
        console
            .recover(true, || panic!("stable ownership polls no setup records"))
            .unwrap();
        assert_eq!(reads.get(), 1);
        console
            .recover(false, || panic!("no console reads no setup scope"))
            .unwrap();
        console
            .recover(false, || panic!("still no console"))
            .unwrap();
        console.recover(true, read).unwrap();
        assert_eq!(reads.get(), 2);
        console
            .recover(true, || panic!("a handled return is spent"))
            .unwrap();
    }

    #[test]
    fn node_start_keeps_inputs_only_inside_its_owned_startup() {
        use std::cell::RefCell;

        for owns in [false, true] {
            let events = RefCell::new(Vec::new());
            assert_eq!(
                node_serve_with(
                    |startup| {
                        events.borrow_mut().push("lease");
                        if owns {
                            startup();
                        }
                        ExitCode::from(75)
                    },
                    || events.borrow_mut().push("keeper"),
                    || {
                        assert_eq!(*events.borrow(), ["lease", "keeper"]);
                        events.borrow_mut().push("runtime");
                    },
                ),
                ExitCode::from(75)
            );
            assert_eq!(
                events.into_inner(),
                if owns {
                    vec!["lease", "keeper", "runtime"]
                } else {
                    vec!["lease"]
                }
            );
        }
    }

    #[test]
    fn background_keeper_requires_a_local_aqua_birth_but_no_terminal_or_vault() {
        use aterm_machines::port::{Fake, Stat};
        use std::cell::Cell;

        for (actor, console, graphic, remote, starts) in [
            (501, 501, Some(true), false, true),
            (0, 0, Some(true), false, false),
            (501, 502, Some(true), false, true),
            (501, 0, Some(true), false, true),
            (501, 501, Some(false), false, false),
            (501, 501, None, false, false),
            (501, 501, Some(true), true, false),
        ] {
            let mut sys = Fake::root();
            sys.euid = actor;
            sys.terminal = false;
            sys.graphic_session = graphic;
            sys.remote_login = remote;
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid: console,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
            let started = Cell::new(false);
            start_background_with(&sys, || started.set(true));
            assert_eq!(started.get(), starts);
            assert!(
                sys.calls().is_empty(),
                "startup needs no vault or hardware command"
            );
        }
    }

    #[test]
    fn an_aqua_keeper_parks_without_hardware_reads_and_resumes_after_user_switching() {
        use aterm_machines::port::{Fake, Stat};
        use std::cell::{Cell, RefCell};

        let mut sys = Fake::root();
        sys.euid = 501;
        sys.terminal = false;
        let console = |uid| {
            sys.stat_of(
                "/dev/console",
                Stat {
                    uid,
                    gid: 20,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: 0,
                },
            );
        };
        // launchd can start this user's Aqua job while another login owns the console.
        console(502);
        let started = Cell::new(false);
        start_background_with(&sys, || started.set(true));
        assert!(started.get());

        let mut keeper = Keeper::default();
        let reads = Cell::new((0, 0));
        let activated = RefCell::new(Vec::new());
        for (owner, ids, active) in [
            (502, vec![10], false),
            (501, vec![11], true),
            (502, vec![12], false),
            (501, vec![12], true),
        ] {
            console(owner);
            let before = reads.get();
            let applied = activated.borrow().len();
            assert_eq!(
                keeper
                    .round_at_console(
                        &sys,
                        0,
                        || {
                            reads.set((reads.get().0 + 1, reads.get().1));
                            Ok(Some(7))
                        },
                        || {
                            reads.set((reads.get().0, reads.get().1 + 1));
                            Ok(ids.clone())
                        },
                        |revision, new| {
                            assert_eq!(revision, 7);
                            activated.borrow_mut().push(new.to_vec());
                            Ok(Some(true))
                        },
                    )
                    .unwrap(),
                active
            );
            if !active {
                assert_eq!(
                    reads.get(),
                    before,
                    "no setup or hardware reading while away"
                );
                assert_eq!(activated.borrow().len(), applied);
            }
        }
        assert_eq!(reads.get(), (2, 2));
        assert_eq!(*activated.borrow(), [vec![11], vec![12]]);
        assert!(sys.calls().is_empty());

        // Returning to the console cannot borrow a held or withdrawn Caps choice.
        assert!(
            !keeper
                .round_at_console(
                    &sys,
                    0,
                    || Ok(None),
                    || panic!("held Caps choice reads no hardware"),
                    |_, _| panic!("held Caps choice activates nothing"),
                )
                .unwrap()
        );
    }

    #[test]
    fn attach_and_reattach_activate_only_new_services_and_off_reads_no_hardware() {
        let mut keeper = Keeper::default();
        for (ids, expected) in [
            (vec![1], vec![1]),
            (vec![1, 2], vec![2]),
            (vec![1, 3], vec![3]),
        ] {
            assert!(
                keeper
                    .round(
                        0,
                        Some(7),
                        || Ok(ids),
                        |revision, new| {
                            assert_eq!(revision, 7);
                            assert_eq!(new, expected);
                            Ok(Some(true))
                        }
                    )
                    .unwrap()
            );
        }
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| panic!("removal only"))
                .unwrap()
        );
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| panic!("unchanged"))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    0,
                    None,
                    || panic!("Off reads no HID state"),
                    |_, _| panic!("Off")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    0,
                    Some(8),
                    || Ok(vec![1]),
                    |revision, ids| {
                        assert_eq!((revision, ids), (8, &[1][..]));
                        Ok(Some(true))
                    }
                )
                .unwrap()
        );
    }

    #[test]
    fn failed_or_held_attachments_retry_without_a_hot_loop_and_new_devices_wake_now() {
        let mut keeper = Keeper::default();
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![1]), |_, _| Ok(None))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    4999,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("lock retry paced")
                )
                .unwrap()
        );
        assert!(
            !keeper
                .round(5000, Some(7), || Ok(vec![1]), |_, _| Ok(Some(false)))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    9999,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("unfinished retry paced")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(10000, Some(7), || Ok(vec![1]), |_, _| Ok(Some(true)))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    10001,
                    Some(7),
                    || Ok(vec![1]),
                    |_, _| panic!("success stops")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10001,
                    Some(7),
                    || Ok(vec![1, 2]),
                    |_, ids| {
                        assert_eq!(ids, [2]);
                        Ok(Some(true))
                    }
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10002,
                    Some(7),
                    || Ok(vec![1, 2, 2]),
                    |_, _| panic!("duplicate identity")
                )
                .is_err()
        );
    }

    #[test]
    fn an_unseen_model_retries_the_same_native_service_and_applies_when_its_census_recovers() {
        use aterm_machines::items::keyboard::{CAPS_LOCK, LEFT_CONTROL};
        use aterm_machines::port::{Fake, Ran, Stat};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        const REGISTRY: u64 = 41;
        fn dump(native: &[(i64, i64)]) -> String {
            let pairs = native
                .iter()
                .map(|(source, destination)| {
                    format!("<dict><key>Src</key><integer>{source}</integer><key>Dst</key><integer>{destination}</integer></dict>")
                })
                .collect::<String>();
            format!(
                "<plist version=\"1.0\"><dict><key>ServiceRecords</key><array><dict>\
                 <key>IORegistryEntryID</key><integer>{REGISTRY}</integer>\
                 <key>PrimaryUsagePage</key><integer>1</integer>\
                 <key>PrimaryUsage</key><integer>6</integer>\
                 <key>ServiceFilterDebug</key><array><dict><key>plugin</key><dict>\
                 <key>Class</key><string>IOHIDKeyboardFilter</string>\
                 <key>HIDKeyboardModifierMappingPairs</key><array>{pairs}</array>\
                 <key>UserKeyMapping</key><array/>\
                 <key>FnFunctionUsageMap</key><array/>\
                 <key>FnKeyboardUsageMap</key><array/>\
                 <key>NumLockKeyboardUsageMap</key><array/>\
                 </dict></dict></array></dict></array></dict></plist>"
            )
        }

        let who = aterm_machines::model::User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        };
        let mut sys = Fake::root();
        sys.euid = who.uid;
        sys.logged_in(&who);
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: who.uid,
                gid: who.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        let census = format!(
            "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n{:<12}{:<12}{:<16}{:<24}0\n{:<12}{:<12}{:<16}{:<24}0\n\nDevices:\n",
            "VendorID",
            "ProductID",
            "RegistryID",
            "Product",
            "0x5ac",
            "0x378",
            "0x2a",
            "Other keyboard",
            "0xfffe",
            "0xa9",
            "0x29",
            "Fixture keyboard"
        );
        let available = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&available);
        sys.simulate(move |sys, cmd| {
            let args: Vec<_> = cmd.args.iter().filter_map(|arg| arg.to_str()).collect();
            match cmd.program.file_name().and_then(|name| name.to_str()) {
                Some("hidutil") if args.first() == Some(&"list") => {
                    Some(Ran::ok(if observed.load(Ordering::SeqCst) {
                        &census
                    } else {
                        "Services:\n\nDevices:\n"
                    }))
                }
                Some("hidutil") if args.first() == Some(&"property") => {
                    let matching: aterm_json::Value = aterm_json::from_str(args[2]).unwrap();
                    let external = matching
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["VendorID"].as_i64() == Some(65534));
                    Some(Ran::ok(&format!(
                        "RegistryID Key Value\n{:x} UserKeyMapping (null)\n",
                        if external { REGISTRY } else { 10 }
                    )))
                }
                Some("hidutil") if args.first() == Some(&"dump") => {
                    let native = sys
                        .keyboard_changes()
                        .into_iter()
                        .find(|change| change.registry == REGISTRY)
                        .and_then(|change| change.native)
                        .unwrap_or_default();
                    Some(Ran::ok(&dump(&native)))
                }
                Some("defaults") => Some(Ran::ok(&aterm_machines::plist::document(
                    &aterm_machines::model::PValue::Dict(Vec::new()),
                ))),
                _ => None,
            }
        });
        let activate = |revision, ids: &[u64]| {
            assert_eq!(revision, 7);
            aterm_machines::init::activate_keyboard_attachments(&sys, ids, &|_| false, &|| true)
                .map(Some)
        };
        let mut keeper = Keeper::default();
        assert!(
            !keeper
                .round(0, Some(7), || Ok(vec![REGISTRY]), activate)
                .unwrap()
        );
        assert!(sys.keyboard_changes().is_empty());
        available.store(true, Ordering::SeqCst);
        assert!(
            !keeper
                .round(
                    4999,
                    Some(7),
                    || Ok(vec![REGISTRY]),
                    |_, _| panic!("unfinished retry remains paced")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(5000, Some(7), || Ok(vec![REGISTRY]), activate)
                .unwrap()
        );
        let changed = sys.keyboard_changes();
        assert_eq!(
            changed.len(),
            1,
            "the unrelated model's NotApplicable row does not block coverage"
        );
        assert_eq!(changed[0].registry, REGISTRY);
        assert_eq!(changed[0].native, Some(vec![(CAPS_LOCK, LEFT_CONTROL)]));
        assert!(sys.calls().iter().all(|call| {
            !call.contains("defaults") || (!call.contains(" write ") && !call.contains(" delete "))
        }));
        assert!(
            !keeper
                .round(
                    5001,
                    Some(7),
                    || Ok(vec![REGISTRY]),
                    |_, _| panic!("verified cohort settles")
                )
                .unwrap()
        );
    }
}
