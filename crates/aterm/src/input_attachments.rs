// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keep accepted keyboard defaults across service arrival and effective mapping drift.

mod watch;

use aterm_cli::macos_doctor::{InputServiceSet, InputSetupScope};
use aterm_machines::init::{KeyboardAttachmentActivation, KeyboardAttachmentReceipt};
use aterm_machines::port::KeyboardServiceObservation;
use aterm_secrets::converge::{Last, RunnerRetry};
use aterm_types::macos_setup::LiveKeyboardMapping;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Keeper {
    settled: Option<(u64, KeyboardAttachmentReceipt)>,
    live: Option<(u64, LiveKeyboardMapping)>,
    attempted: Option<(u64, Vec<KeyboardServiceObservation>)>,
    retry: RunnerRetry,
}

#[derive(Default)]
struct Inputs {
    keeper: Keeper,
    live: Option<(u64, LiveKeyboardMapping)>,
    pending: Option<(u64, InputServiceSet, InputServiceSet)>,
    seen: Option<Sample>,
}

struct Sample {
    scope: InputSetupScope,
    services: Option<InputServiceSet>,
    mappings: Option<SampleMappings>,
}

#[derive(PartialEq, Eq)]
enum SampleMappings {
    Observed(Vec<KeyboardServiceObservation>),
    Verified(KeyboardAttachmentReceipt),
}

/// Mutable task ownership outlives a failed sampling thread. Only observations are
/// invalidated after a panic; finite input/FDA workers retain their one owner.
#[derive(Default)]
struct Actor {
    inputs: Inputs,
    console: ConsoleReturn,
    follow_up: super::withheld_preferences::Coordinator,
    follow_up_said: Option<String>,
    said: Option<String>,
}

#[derive(Clone)]
struct Notifications {
    changed: fn(),
    invalidated: fn(),
    console_return: Option<fn() -> Result<(), String>>,
    /// What a pending scope's new keyboard nudges: a window's parked run. `None` in a background
    /// keeper's process, which runs no setup pass: the login job's park reads the Mac's keyboards
    /// itself (`aterm_cli::macos_doctor::initial_setup_park_mark`), and a window's follow-up
    /// rounds are left to the window.
    pending: Option<fn()>,
    ended: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct Supervisor {
    clock: Instant,
    actor: Arc<Mutex<Actor>>,
    stop: Arc<AtomicBool>,
    wake: Arc<watch::Control>,
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
            wake: Arc::default(),
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
        let wake = Arc::clone(&self.wake);
        self.maintain_with(|actor, stop, ending| {
            spawn_worker(actor, stop, ending, clock, notifications, wake)
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
                // The joined sampler holds no actor guard now. Keep its follow-up
                // handles, discard cached observations, and start with fresh authority.
                let mut actor = self.actor.lock().unwrap_or_else(PoisonError::into_inner);
                actor.inputs = Inputs::default();
                actor.console = ConsoleReturn {
                    away: true,
                    pending: false,
                };
                actor.said = None;
                actor.follow_up_said = None;
                self.actor.clear_poison();
                drop(actor);
                (self.notifications.invalidated)();
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
        self.wake.wake();
    }
}

impl Inputs {
    #[expect(
        clippy::too_many_arguments,
        reason = "one input sample keeps its data, activation, wake and invalidation ports explicit"
    )]
    fn round(
        &mut self,
        at_console: bool,
        now_ms: u64,
        scope: impl FnOnce() -> Result<InputSetupScope, String>,
        services: impl FnOnce() -> Result<Vec<u64>, String>,
        observations: impl FnOnce() -> Result<Vec<KeyboardServiceObservation>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<KeyboardAttachmentActivation>, String>,
        pending_changed: &dyn Fn(),
        invalidated: &dyn Fn(),
    ) -> Result<bool, String> {
        self.live = None;
        if !at_console {
            self.observe(InputSetupScope::Inactive, None, None, invalidated);
            self.keeper = Keeper::default();
            self.pending = None;
            return Ok(false);
        }
        let scope = scope().inspect_err(|_| {
            self.seen = None;
            invalidated();
        })?;
        if scope == InputSetupScope::Inactive {
            self.observe(scope, None, None, invalidated);
            self.keeper = Keeper::default();
            self.pending = None;
            return Ok(false);
        }
        match scope {
            InputSetupScope::Inactive => {
                unreachable!("inactive scope returned before the hardware sample")
            }
            InputSetupScope::Applied(revision) => {
                let mut observed = observations().inspect_err(|_| {
                    self.seen = None;
                    invalidated();
                })?;
                observed.sort_unstable_by_key(KeyboardServiceObservation::registry);
                let services = InputServiceSet::new(
                    observed
                        .iter()
                        .map(KeyboardServiceObservation::registry)
                        .collect(),
                )
                .inspect_err(|_| {
                    self.seen = None;
                    invalidated();
                })?;
                // Retire changed live facts even if the profile lock defers their repair.
                self.observe(scope, Some(services), Some(observed.clone()), invalidated);
                self.pending = None;
                let result = self
                    .keeper
                    .round(now_ms, Some(revision), || Ok(observed), activate);
                if result.is_ok() {
                    self.live = self.keeper.live;
                }
                if matches!(result, Ok(true))
                    && let Some((_, receipt)) = &self.keeper.settled
                    && let Some(seen) = &mut self.seen
                {
                    // Our own write is not later drift. A reset back to its before-image
                    // must still invalidate the successful pass's newer readback.
                    seen.mappings = Some(SampleMappings::Verified(receipt.clone()));
                }
                result
            }
            InputSetupScope::Pending(revision) => {
                let (services, mut readable) = observations()
                    .and_then(|rows| InputServiceSet::observed(&rows))
                    .or_else(|_| {
                        services()
                            .and_then(InputServiceSet::new)
                            .map(|ids| (ids, InputServiceSet::default()))
                    })
                    .inspect_err(|_| {
                        self.seen = None;
                        invalidated();
                    })?;
                self.observe(scope, Some(services.clone()), None, invalidated);
                self.keeper = Keeper::default();
                if let Some((before, _, known)) = &self.pending
                    && *before == revision
                {
                    readable.retain_present_from(known, &services);
                }
                let current = (revision, services, readable);
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

    fn observe(
        &mut self,
        scope: InputSetupScope,
        services: Option<InputServiceSet>,
        observations: Option<Vec<KeyboardServiceObservation>>,
        invalidated: &dyn Fn(),
    ) {
        let current = Sample {
            scope,
            services,
            mappings: observations.map(SampleMappings::Observed),
        };
        let unchanged = self.seen.as_ref().is_some_and(|before| {
            before.scope == current.scope
                && before.services == current.services
                && match (&before.mappings, &current.mappings) {
                    (
                        Some(SampleMappings::Verified(receipt)),
                        Some(SampleMappings::Observed(rows)),
                    ) => rows
                        .iter()
                        .all(|row| row.unread().is_none() && receipt.matches(row)),
                    (before, current) => before == current,
                }
        });
        if !unchanged {
            let completed = self.seen.as_ref().is_some_and(|before| {
                matches!((before.scope, scope), (InputSetupScope::Pending(a), InputSetupScope::Applied(b)) if a == b)
                    && before.services == current.services
                    && matches!(&current.mappings, Some(SampleMappings::Observed(rows)) if rows.iter().all(|row| row.unread().is_none()))
            });
            // The owned pass just supplied the full cohort. Recording its
            // completion with the same identities is no new hardware/source.
            if !completed {
                invalidated();
            }
            self.seen = Some(current);
        }
    }
}

/// Retire an actual activation's readings for its whole effect/failed-read
/// batch. A real no-op produces no lease and owes no redundant observation.
pub(super) fn activate_observing<T>(activate: impl FnOnce(&dyn Fn()) -> T) -> (T, bool) {
    let activity = std::cell::RefCell::new(None);
    let result = activate(&|| {
        if activity.borrow().is_none() {
            *activity.borrow_mut() = Some(super::InputReadbackActivity::begin());
        }
    });
    let retired = activity.borrow().is_some();
    drop(activity.into_inner());
    (result, retired)
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
        services: impl FnOnce() -> Result<Vec<KeyboardServiceObservation>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<KeyboardAttachmentActivation>, String>,
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
        services: impl FnOnce() -> Result<Vec<KeyboardServiceObservation>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<KeyboardAttachmentActivation>, String>,
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
        services: impl FnOnce() -> Result<Vec<KeyboardServiceObservation>, String>,
        activate: impl FnOnce(u64, &[u64]) -> Result<Option<KeyboardAttachmentActivation>, String>,
    ) -> Result<bool, String> {
        self.live = None;
        let Some(revision) = revision else {
            *self = Self::default();
            return Ok(false);
        };
        let mut observed = services()?;
        observed.sort_unstable_by_key(KeyboardServiceObservation::registry);
        let registries = InputServiceSet::new(
            observed
                .iter()
                .map(KeyboardServiceObservation::registry)
                .collect(),
        )?
        .into_vec();
        let current = (revision, observed);
        let changed: Vec<_> = current
            .1
            .iter()
            .filter(|observation| {
                self.settled.as_ref().is_none_or(|(before, receipt)| {
                    *before != revision || !receipt.matches(observation)
                })
            })
            .map(KeyboardServiceObservation::registry)
            .collect();
        if changed.is_empty() {
            if let Some((_, receipt)) = &mut self.settled {
                receipt.retain(&registries);
                self.live = receipt
                    .live_mapping(&current.1)
                    .map(|live| (revision, live));
            }
            self.attempted = None;
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
        self.retry.started();
        let result = activate(revision, &changed);
        self.retry.reaped(
            match &result {
                Ok(Some(KeyboardAttachmentActivation::Verified(_))) => Last::Ended,
                Ok(None) => Last::Held,
                Ok(Some(KeyboardAttachmentActivation::Unfinished)) => Last::Unfinished,
                Err(_) => Last::Failed,
            },
            now_ms,
        );
        self.attempted = Some(current);
        if let Ok(Some(KeyboardAttachmentActivation::Verified(mut verified))) = result {
            if let Some((before, receipt)) = &mut self.settled
                && *before == revision
            {
                receipt.merge(verified);
                receipt.retain(&registries);
            } else {
                verified.retain(&registries);
                self.settled = Some((revision, verified));
            }
            // The sample preceded activation. An own write cannot certify that before-image;
            // the existing next passive round will match the verified final receipt.
            if let Some((_, receipt)) = &self.settled {
                self.live = receipt
                    .live_mapping(&self.attempted.as_ref().expect("attempted cohort").1)
                    .map(|live| (revision, live));
            }
            return Ok(true);
        }
        result.map(|_| false)
    }
}

/// Only the subscriber's local-input lane maintains the keeper, whose end rings `rung`. The node
/// service already owns its maintenance worker for its full runtime, and the window its own.
pub(super) fn convergence_inputs(
    paths: &aterm_secrets::client::Paths,
    role: aterm_secrets::client::ConvergenceRole,
    rung: Option<Arc<aterm_uds::ready::Doorbell>>,
) -> aterm_secrets::client::ConvergenceInputs {
    let inputs = super::converge_inputs::observe(paths, role, rung.clone());
    if role != aterm_secrets::client::ConvergenceRole::Subscriber {
        return inputs;
    }
    match spawn_background(rung) {
        Some(maintain) => inputs.with_maintenance(Box::new(move || maintain())),
        None => inputs,
    }
}

/// The keeper's upkeep on a lane that is not the window's: when it is next owed with nothing
/// rung ([`Supervisor::maintain`]).
type Upkeep = Arc<dyn Fn() -> Option<Instant> + Send + Sync>;

/// Start only inside the node job's owned startup, after its boot and exclusive lease.
pub(super) fn node_serve(startup: impl FnOnce() -> Option<aterm_gui::NodeMaintenance>) -> ExitCode {
    node_serve_with(
        aterm_gui::run_node_serve_owning,
        node_background,
        |inputs| match (inputs, startup()) {
            (Some(inputs), Some(commons)) => Some(Arc::new(move || {
                inputs();
                commons();
            }) as aterm_gui::NodeMaintenance),
            (inputs, commons) => inputs.or(commons),
        },
    )
}

/// The keeper's upkeep for the node's own maintenance, which runs at its own pace.
fn node_background() -> Option<aterm_gui::NodeMaintenance> {
    spawn_background(None).map(|upkeep| {
        Arc::new(move || {
            let _ = upkeep();
        }) as aterm_gui::NodeMaintenance
    })
}

fn node_serve_with<'a, R, S: 'a>(
    own: impl FnOnce(Box<dyn FnOnce() -> S + 'a>) -> R,
    start: impl FnOnce() -> S + 'a,
    runtime: impl FnOnce(S) -> S + 'a,
) -> R {
    own(Box::new(move || {
        let maintenance = start();
        runtime(maintenance)
    }))
}

/// Start the keeper off the window's lane, its end ringing `rung` where there is one.
fn spawn_background(rung: Option<Arc<aterm_uds::ready::Doorbell>>) -> Option<Upkeep> {
    background_maintenance(
        supervisor_allowed(),
        aterm_machines::port::Live::new,
        move || background_supervisor(rung.clone()),
        Supervisor::maintain,
    )
}

/// The keeper's upkeep once this login's session admits it ([`BackgroundStartup`]): its
/// maintenance's deadline once an owner runs. An admission still unread retries at each call and
/// names no deadline of its own, nor does a keeper just started: its end rings.
fn background_maintenance<S: aterm_machines::port::System>(
    permitted: bool,
    system: impl Fn() -> S + Send + Sync + 'static,
    start: impl Fn() -> Option<Supervisor> + Send + Sync + 'static,
    maintain: impl Fn(&mut Supervisor) -> Option<Instant> + Send + Sync + 'static,
) -> Option<Upkeep> {
    if !permitted {
        return None;
    }
    let mut startup = BackgroundStartup::default();
    let owner = startup.take(&system()).then(&start).flatten();
    if owner.is_none() && startup.decision.is_some() {
        return None;
    }
    // Unknown session admission retains only this existing maintenance callback. No
    // sampler, notification watch or preference/HID read exists until a positive read.
    let background = Mutex::new((startup, owner));
    Some(Arc::new(move || {
        // Another caller holds the keeper's upkeep: its own call names the deadline.
        let Ok(mut background) = background.try_lock() else {
            return None;
        };
        let (startup, owner) = &mut *background;
        if let Some(owner) = owner {
            return maintain(owner);
        }
        if startup.decision.is_none() && startup.take(&system()) {
            *owner = start();
        }
        None
    }) as Upkeep)
}

/// The keeper of a lane that is not the window's, its end ringing `rung` where there is one.
fn background_supervisor(rung: Option<Arc<aterm_uds::ready::Doorbell>>) -> Option<Supervisor> {
    supervisor(Notifications {
        changed: || {},
        invalidated: || {},
        console_return: None,
        pending: None,
        ended: rung.map(|rung| Arc::new(move || rung.ring()) as Arc<dyn Fn() + Send + Sync>),
    })
}

#[derive(Default)]
struct BackgroundStartup {
    decision: Option<bool>,
}

impl BackgroundStartup {
    /// Consume one admitted startup. Console ownership is a round's changing fact:
    /// this process's own Aqua session survives Fast User Switching. An unread session
    /// stays pending; a successful non-Aqua reading, root or SSH resolves refusal.
    fn take(&mut self, sys: &dyn aterm_machines::port::System) -> bool {
        if self.decision.is_some() {
            return false;
        }
        self.decision = if sys.euid() == 0 || sys.remote_login() {
            Some(false)
        } else {
            sys.graphic_session()
        };
        self.decision == Some(true)
    }
}

pub(super) fn spawn() -> Option<aterm_gui::HostMaintenance> {
    supervisor(Notifications {
        invalidated: super::invalidate_input_observations,
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
        pending: Some(super::wake_setup_facts),
        ended: Some(Arc::new(aterm_gui::wake_host_maintenance)),
    })
    .map(|mut owner| Box::new(move || owner.maintain()) as aterm_gui::HostMaintenance)
}

fn supervisor_allowed() -> bool {
    !aterm_types::dirs::runs_under_private_state_root() && std::env::var_os("CI").is_none()
}

fn supervisor(notifications: Notifications) -> Option<Supervisor> {
    if !supervisor_allowed() {
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
    wake: Arc<watch::Control>,
) -> std::io::Result<JoinHandle<()>> {
    spawn_worker_with(
        actor,
        stop,
        ending,
        clock,
        notifications,
        move |actor, stop, clock, notifications| {
            use aterm_machines::port::System as _;
            let _ = aterm_log::wait_for_logger(super::EARLY_LOG_WAIT);
            let sys = aterm_machines::port::Live::new();
            // Arm lifecycle notifications before the first full census. This worker
            // owns their run loop and context; events only interrupt its fallback wait.
            let mut watch = watch::Watch::new(wake, &stop);
            while !stop.load(Ordering::Acquire) {
                if notifications.pending.is_some() {
                    super::maintain_observation_dispatch();
                }
                let live_scope = notifications
                    .pending
                    .is_some()
                    .then(super::capture_caps_live_scope)
                    .flatten();
                let mut actor = actor.lock().unwrap_or_else(PoisonError::into_inner);
                let Actor {
                    inputs,
                    console,
                    follow_up,
                    follow_up_said,
                    said,
                } = &mut *actor;
                // One current console sample and one canonical input-scope read select accepted
                // activation or the narrowly scoped observation of still-owed Caps work.
                let at_console = sys.euid() != 0 && !aterm_machines::console::remote(&sys);
                if let Some(wake) = notifications.console_return {
                    let _ = console.recover(at_console, wake);
                }
                let now = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
                if notifications.pending.is_some() {
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
                let needs_observation = std::cell::Cell::new(false);
                let invalidate = || {
                    needs_observation.set(true);
                    (notifications.invalidated)();
                };
                let result = inputs.round(
                    at_console,
                    now,
                    || scope.clone(),
                    || sys.keyboard_service_ids(),
                    || sys.keyboard_service_observations(),
                    |revision, ids| {
                        let (result, retired) = activate_observing(|changing| {
                            aterm_cli::macos_doctor::activate_input_attachments(
                                revision,
                                ids,
                                super::setup_stop_requested,
                                changing,
                            )
                        });
                        needs_observation.set(needs_observation.get() || retired);
                        result
                    },
                    &notifications.pending.unwrap_or(|| {}),
                    &invalidate,
                );
                let live = result.as_ref().ok().and(inputs.live);
                match result {
                    Ok(true) => {
                        if needs_observation.get() {
                            (notifications.changed)();
                        }
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
                if notifications.pending.is_some() {
                    super::publish_caps_live(live_scope, live);
                }
                // Property drift and console changes need the complete original timer
                // even when a device family emits no usable lifecycle notification.
                watch.wait_until(&stop, Instant::now() + Duration::from_secs(2));
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
#[path = "input_attachments_tests.rs"]
mod tests;
