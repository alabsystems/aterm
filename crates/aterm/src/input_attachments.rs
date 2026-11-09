// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keep accepted keyboard defaults across service arrival and effective mapping drift.

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
    coordinator: super::input_coordinator::Coordinator,
    follow_up: super::withheld_preferences::Coordinator,
    follow_up_said: Option<String>,
    said: Option<String>,
}

#[derive(Clone)]
struct Notifications {
    changed: fn(),
    invalidated: fn(),
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

/// Only the subscriber's local-input lane maintains the keeper. The node service
/// already owns its maintenance worker for its full runtime.
pub(super) fn convergence_inputs(
    paths: &aterm_secrets::client::Paths,
    role: aterm_secrets::client::ConvergenceRole,
) -> aterm_secrets::client::ConvergenceInputs {
    let inputs = super::converge_inputs::observe(paths, role);
    if role == aterm_secrets::client::ConvergenceRole::Node {
        return inputs;
    }
    let maintenance = spawn_background();
    match maintenance {
        Some(maintain) => inputs.with_maintenance(Box::new(move || maintain())),
        None => inputs,
    }
}

/// Start only inside the node job's owned startup, after its boot and exclusive lease.
pub(super) fn node_serve(startup: impl FnOnce() -> Option<aterm_gui::NodeMaintenance>) -> ExitCode {
    node_serve_with(
        aterm_gui::run_node_serve_owning,
        spawn_background,
        |inputs| match (inputs, startup()) {
            (Some(inputs), Some(commons)) => Some(Arc::new(move || {
                inputs();
                commons();
            }) as aterm_gui::NodeMaintenance),
            (inputs, commons) => inputs.or(commons),
        },
    )
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

fn spawn_background() -> Option<aterm_gui::NodeMaintenance> {
    let mut maintenance = None;
    spawn_background_with(
        &aterm_machines::port::Live::new(),
        |changed, console_return, pending| {
            maintenance = supervisor(Notifications {
                changed,
                invalidated: || {},
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
                if matches!(notifications.pending, PendingOwner::Window(_)) {
                    super::maintain_observation_dispatch();
                }
                let live_scope = matches!(notifications.pending, PendingOwner::Window(_))
                    .then(super::capture_caps_live_scope)
                    .flatten();
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
                let needs_observation = std::cell::Cell::new(false);
                let invalidate = || {
                    needs_observation.set(true);
                    (notifications.invalidated)();
                };
                let observed = inputs.round(
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
                    &notifications.pending.changed(),
                    &invalidate,
                );
                // Retain/reap the real scoped task even when this hardware sample
                // failed; the source read still controls its ordinary admission.
                let owned = notifications.pending.coordinate(
                    coordinator,
                    &scope,
                    at_console,
                    now,
                    super::input_coordinator::start,
                    super::wake_setup_facts,
                );
                let result = observed.and_then(|changed| owned.map(|()| changed));
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
                if matches!(notifications.pending, PendingOwner::Window(_)) {
                    super::publish_caps_live(live_scope, live);
                }
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
    use aterm_machines::port::System as _;

    fn keyboard_dump(registries: &[u64], native: &[(i64, i64)]) -> String {
        let pairs = native
            .iter()
            .map(|(source, destination)| {
                format!("<dict><key>Src</key><integer>{source}</integer><key>Dst</key><integer>{destination}</integer></dict>")
            })
            .collect::<String>();
        let services = registries
            .iter()
            .map(|registry| {
                format!(
                    "<dict><key>IORegistryEntryID</key><integer>{registry}</integer>\
                     <key>PrimaryUsagePage</key><integer>1</integer>\
                     <key>PrimaryUsage</key><integer>6</integer>\
                     <key>ServiceFilterDebug</key><array><dict><key>plugin</key><dict>\
                     <key>Class</key><string>IOHIDKeyboardFilter</string>\
                     <key>HIDKeyboardModifierMappingPairs</key><array>{pairs}</array>\
                     <key>UserKeyMapping</key><array/>\
                     <key>FnFunctionUsageMap</key><array/>\
                     <key>FnKeyboardUsageMap</key><array/>\
                     <key>NumLockKeyboardUsageMap</key><array/>\
                     </dict></dict></array></dict>"
                )
            })
            .collect::<String>();
        format!(
            "<plist version=\"1.0\"><dict><key>ServiceRecords</key><array>{services}</array></dict></plist>"
        )
    }

    /// Reuse the real Fake input ports for logical keeper cases. Receipts still come
    /// from the ordinary final probe; no success flag can manufacture their state.
    fn mapped_keyboard_system(registries: &[u64]) -> aterm_machines::port::Fake {
        use aterm_machines::items::keyboard::{CAPS_LOCK, LEFT_CONTROL};
        use aterm_machines::port::Ran;
        let mut sys =
            aterm_cli::macos_doctor::InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
        sys.keyboard_services = Some(Ok(registries.to_vec()));
        let mut census = format!(
            "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n",
            "VendorID", "ProductID", "RegistryID", "Product"
        );
        let mut properties = String::from("RegistryID Key Value\n");
        for id in registries {
            census.push_str(&format!(
                "{:<12}{:<12}{:<16}{:<24}0\n",
                "0xfffe",
                "0xa9",
                format!("0x{id:x}"),
                "Fixture keyboard"
            ));
            properties.push_str(&format!("{id:x} UserKeyMapping (null)\n"));
        }
        census.push_str("\nDevices:\n");
        let dump = keyboard_dump(registries, &[(CAPS_LOCK, LEFT_CONTROL)]);
        sys.simulate(move |_, cmd| {
            let args: Vec<_> = cmd.args.iter().filter_map(|arg| arg.to_str()).collect();
            match cmd.program.file_name().and_then(|name| name.to_str()) {
                Some("hidutil") if args.first() == Some(&"list") => Some(Ran::ok(&census)),
                Some("hidutil") if args.first() == Some(&"property") => Some(Ran::ok(&properties)),
                Some("hidutil") if args.first() == Some(&"dump") => Some(Ran::ok(&dump)),
                Some("defaults") => Some(Ran::ok(&aterm_machines::plist::document(
                    &aterm_machines::model::PValue::Dict(Vec::new()),
                ))),
                _ => None,
            }
        });
        sys
    }

    fn observations(registries: &[u64]) -> Result<Vec<KeyboardServiceObservation>, String> {
        mapped_keyboard_system(registries).keyboard_service_observations()
    }

    fn verified(registries: &[u64]) -> Result<Option<KeyboardAttachmentActivation>, String> {
        let sys = mapped_keyboard_system(registries);
        let result = aterm_machines::init::activate_keyboard_attachments(
            &sys,
            registries,
            &|_| false,
            &|| true,
            &|| panic!("the modeled mapping is already in place"),
        )?;
        assert!(sys.keyboard_changes().is_empty());
        Ok(Some(result))
    }

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
                Box::new(InitialSetupAdmissionStopTest::keyboard_system(false, &[]))
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
            invalidated: || {},
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
                                || observations(&[41]),
                                |_, _| panic!("pending input has no setter authority"),
                                &|| {},
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
            invalidated: || {},
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
        use std::sync::atomic::Ordering;

        let positive = aterm_tempfile::tempdir().unwrap();
        let (ready, revision) = native_pending_caps(positive.path());
        let (report, next) = ready
            .native_workstation_pass(revision, || {
                Box::new(InitialSetupAdmissionStopTest::keyboard_system(true, &[]))
            })
            .unwrap()
            .unwrap();
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert_eq!(next, SetupNext::Done);
        assert_eq!(ready.input_activation_revision().unwrap(), Some(revision));

        for same_ids in [false, true] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let (setup, revision) = native_pending_caps(dir.path());
            let mut actor = InitialSetupAdmissionStopTest::keyboard_system(false, &[]);
            actor.terminal = false;
            let (native, events) = fixture::Native::new(dir.path().to_path_buf());
            let mut coordinator = Coordinator::default();
            let mut inputs = Inputs::default();
            let nudges = std::cell::Cell::new(0);
            spawn_background_with(&actor, |_, _, owner| {
                for ready in [false, true] {
                    native.attached.store(same_ids || ready, Ordering::Release);
                    native.unread.store(same_ids && !ready, Ordering::Release);
                    let sys = native.system();
                    if same_ids {
                        assert_eq!(sys.keyboard_service_ids().unwrap(), [41, 42]);
                        assert_eq!(
                            sys.keyboard_service_observations()
                                .unwrap()
                                .iter()
                                .all(|row| row.unread().is_none()),
                            ready,
                            "the same native records become positively readable"
                        );
                    }
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
                    let before = nudges.get();
                    let hand_off = || {
                        nudges.set(nudges.get() + 1);
                        native.wake();
                    };
                    assert!(
                        !inputs
                            .round(
                                true,
                                0,
                                || scope,
                                || sys.keyboard_service_ids(),
                                || sys.keyboard_service_observations(),
                                |_, _| panic!("a pending pass has no attachment-setter authority"),
                                &hand_off,
                                &|| {},
                            )
                            .unwrap()
                    );
                    if !ready {
                        fixture::waiting(&events);
                    } else if same_ids && nudges.get() == before {
                        // The old producer loses this real same-ID readiness edge. Retire and
                        // join its owned worker before reporting the short causal failure.
                        setup.stop_revision(revision).unwrap();
                        native.wake();
                        fixture::finished(&events, &mut coordinator);
                        assert_ne!(
                            nudges.get(),
                            before,
                            "readable filters wake the pending input task"
                        );
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
        let actor = InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
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
                    || panic!("Stop withholds the mapping read"),
                    |_, _| panic!("Stop withholds activation"),
                    &stopped_handoff,
                    &|| {},
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
        let actor =
            aterm_cli::macos_doctor::InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
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
            let actor = InitialSetupAdmissionStopTest::keyboard_system(false, &[]);
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
                            Box::new(InitialSetupAdmissionStopTest::keyboard_system(true, &[]))
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
        let invalidations = std::cell::Cell::new(0);
        let invalidate = || invalidations.set(invalidations.get() + 1);
        let mut inputs = Inputs::default();
        let observe = |inputs: &mut Inputs, ids: Vec<u64>| {
            inputs.round(
                true,
                0,
                scope,
                || Ok(ids.clone()),
                || observations(&ids),
                |_, _| panic!("pending observation has no setter authority"),
                &changed,
                &invalidate,
            )
        };
        // Readability is keyed by service, not a count or an error string. Losing a
        // positive read cannot repeatedly rearm the same pending service's retry budget.
        let mut partial = Inputs::default();
        let partial_wakes = crate::SetupWakes::new();
        for (unread, expected_nudge) in [
            (&[41, 42][..], true),
            (&[41, 42][..], false),
            (&[42][..], true),
            (&[41][..], true),
            (&[][..], false),
            (&[41, 42][..], false),
            (&[41][..], false),
        ] {
            let sys = InitialSetupAdmissionStopTest::keyboard_system(true, unread);
            let before = partial_wakes.marks();
            assert!(
                !partial
                    .round(
                        true,
                        0,
                        scope,
                        || sys.keyboard_service_ids(),
                        || sys.keyboard_service_observations(),
                        |_, _| panic!("readability authorizes no attachment setter"),
                        &|| partial_wakes.raise_facts(),
                        &|| {},
                    )
                    .unwrap()
            );
            assert_eq!(partial_wakes.marks().facts != before.facts, expected_nudge);
        }
        let before_error = partial_wakes.marks();
        assert!(
            !partial
                .round(
                    true,
                    0,
                    scope,
                    || Ok(vec![41, 42]),
                    || Err("native properties unavailable".into()),
                    |_, _| panic!("an unread native source authorizes no setter"),
                    &|| partial_wakes.raise_facts(),
                    &|| {},
                )
                .unwrap()
        );
        assert_eq!(partial_wakes.marks(), before_error);

        let before = wakes.marks();
        assert!(!observe(&mut inputs, vec![41]).unwrap());
        assert_eq!(
            invalidations.get(),
            1,
            "first native sample retires old facts"
        );
        let seeded = wakes.marks();
        assert_ne!(
            seeded.facts, before.facts,
            "the first sample closes the pass/observer gap"
        );
        assert!(!seeded.refreshed_since(before));
        assert!(!seeded.checked_since(before));
        assert!(!observe(&mut inputs, vec![41]).unwrap());
        assert_eq!(
            invalidations.get(),
            1,
            "an unchanged sample invalidates nothing"
        );
        assert_eq!(wakes.marks(), seeded, "unchanged samples owe no nudge");
        assert!(!observe(&mut inputs, vec![42, 41]).unwrap());
        assert_eq!(invalidations.get(), 2);
        let attached = wakes.marks();
        assert_ne!(attached.facts, seeded.facts);
        assert!(!attached.refreshed_since(before));
        assert!(!observe(&mut inputs, vec![41, 42]).unwrap());
        assert_eq!(
            invalidations.get(),
            2,
            "enumeration order changes no source"
        );
        assert_eq!(
            wakes.marks(),
            attached,
            "a different enumeration order is no new device"
        );
        assert!(observe(&mut inputs, vec![41, 41]).is_err());
        assert_eq!(invalidations.get(), 3, "an unread cohort retires old facts");
        assert_eq!(wakes.marks(), attached, "ambiguous IDs supply no fact");

        // A new keyboard can already be present when this observer first sees pending setup.
        let mut late = Inputs::default();
        assert!(!observe(&mut late, vec![41, 42]).unwrap());
        let first_after_attach = wakes.marks();
        assert_ne!(first_after_attach.facts, attached.facts);
        assert!(!first_after_attach.refreshed_since(before));
        assert!(!first_after_attach.checked_since(before));

        assert!(!observe(&mut inputs, vec![41, 42]).unwrap());
        let before_completion = invalidations.get();
        setup
            .native_workstation_pass(revision, || {
                Box::new(InitialSetupAdmissionStopTest::keyboard_system(true, &[]))
            })
            .unwrap()
            .unwrap();
        assert_eq!(scope().unwrap(), InputSetupScope::Applied(revision));
        let no_op = InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
        assert!(
            inputs
                .round(
                    true,
                    0,
                    scope,
                    || panic!("applied scope uses one passive mapping observation"),
                    || no_op.keyboard_service_observations(),
                    |_, ids| aterm_machines::init::activate_keyboard_attachments(
                        &no_op,
                        ids,
                        &|_| false,
                        &|| true,
                        &invalidate,
                    )
                    .map(Some),
                    &changed,
                    &invalidate,
                )
                .unwrap()
        );
        assert_eq!(
            invalidations.get(),
            before_completion,
            "the same owned task's completion does not retire its readback"
        );
        assert!(
            no_op.keyboard_changes().is_empty(),
            "completion's real activation was a no-op"
        );

        assert!(
            !inputs
                .round(
                    false,
                    0,
                    || panic!("another console reads no setup scope"),
                    || panic!("another console reads no hardware"),
                    || panic!("another console reads no mappings"),
                    |_, _| panic!("another console activates nothing"),
                    &changed,
                    &invalidate,
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
                    || panic!("Stop withdraws mapping observation"),
                    |_, _| panic!("Stop withdraws activation"),
                    &changed,
                    &invalidate,
                )
                .unwrap()
        );
        assert_eq!(wakes.marks(), first_after_attach);
        let before = invalidations.get();
        let mut applied = Inputs::default();
        assert!(
            applied
                .round(
                    true,
                    0,
                    || Ok(InputSetupScope::Applied(revision)),
                    || Ok(vec![41]),
                    || observations(&[41]),
                    |_, _| {
                        assert_eq!(
                            invalidations.get(),
                            before + 1,
                            "retire before a failed activation"
                        );
                        Err("injected activation refusal".into())
                    },
                    &changed,
                    &invalidate,
                )
                .is_err()
        );
        assert!(
            applied
                .round(
                    true,
                    1,
                    || Err("injected source read failure".into()),
                    || panic!("no hardware read after source refusal"),
                    || panic!("no mapping read after source refusal"),
                    |_, _| panic!("no activation after source refusal"),
                    &changed,
                    &invalidate,
                )
                .is_err()
        );
        assert_eq!(invalidations.get(), before + 2);
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
                    |()| {
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
                            observations(&ids)
                        },
                        |revision, new| {
                            assert_eq!(revision, 7);
                            activated.borrow_mut().push(new.to_vec());
                            verified(new)
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
                        || observations(&ids),
                        |revision, new| {
                            assert_eq!(revision, 7);
                            assert_eq!(new, expected);
                            verified(new)
                        }
                    )
                    .unwrap()
            );
        }
        assert!(
            !keeper
                .round(
                    0,
                    Some(7),
                    || observations(&[1]),
                    |_, _| panic!("removal only")
                )
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    0,
                    Some(7),
                    || observations(&[1]),
                    |_, _| panic!("unchanged")
                )
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
                    || observations(&[1]),
                    |revision, ids| {
                        assert_eq!((revision, ids), (8, &[1][..]));
                        verified(ids)
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
                .round(0, Some(7), || observations(&[1]), |_, _| Ok(None))
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    4999,
                    Some(7),
                    || observations(&[1]),
                    |_, _| panic!("lock retry paced")
                )
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    5000,
                    Some(7),
                    || observations(&[1]),
                    |_, _| Ok(Some(KeyboardAttachmentActivation::Unfinished))
                )
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    9999,
                    Some(7),
                    || observations(&[1]),
                    |_, _| panic!("unfinished retry paced")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10000,
                    Some(7),
                    || observations(&[1]),
                    |_, ids| verified(ids)
                )
                .unwrap()
        );
        assert!(
            !keeper
                .round(
                    10001,
                    Some(7),
                    || observations(&[1]),
                    |_, _| panic!("success stops")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10001,
                    Some(7),
                    || observations(&[1, 2]),
                    |_, ids| {
                        assert_eq!(ids, [2]);
                        verified(ids)
                    }
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    10002,
                    Some(7),
                    || observations(&[1, 2, 2]),
                    |_, _| panic!("duplicate identity")
                )
                .is_err()
        );
    }

    #[test]
    fn an_unseen_model_retries_the_same_native_service_and_applies_when_its_census_recovers() {
        use aterm_machines::items::keyboard::{CAPS_LOCK, LEFT_CONTROL};
        use aterm_machines::port::{Cmd, Fake, Ran, Stat};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        const REGISTRY: u64 = 41;

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
        sys.keyboard_services = Some(Ok(vec![REGISTRY]));
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
        let written_before_reset = Arc::new(AtomicUsize::new(0));
        let forgotten = Arc::clone(&written_before_reset);
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
                    let changes: Vec<_> = sys
                        .keyboard_changes()
                        .into_iter()
                        .filter(|change| change.registry == REGISTRY)
                        .collect();
                    let native = changes
                        .into_iter()
                        .skip(forgotten.load(Ordering::SeqCst))
                        .next_back()
                        .and_then(|change| change.native)
                        .unwrap_or_default();
                    Some(Ran::ok(&keyboard_dump(&[REGISTRY], &native)))
                }
                Some("defaults") => Some(Ran::ok(&aterm_machines::plist::document(
                    &aterm_machines::model::PValue::Dict(Vec::new()),
                ))),
                _ => None,
            }
        });
        let invalidations = std::cell::Cell::new(0);
        let writes_before = std::cell::Cell::new(0);
        let activate = |revision, ids: &[u64]| {
            assert_eq!(revision, 7);
            aterm_machines::init::activate_keyboard_attachments(
                &sys,
                ids,
                &|_| false,
                &|| true,
                &|| {
                    assert_eq!(
                        sys.keyboard_changes().len(),
                        writes_before.get(),
                        "retire before the next real native setter"
                    );
                    invalidations.set(invalidations.get() + 1);
                },
            )
            .map(Some)
        };
        let mut keeper = Keeper::default();
        assert!(
            !keeper
                .round(0, Some(7), || sys.keyboard_service_observations(), activate)
                .unwrap()
        );
        assert!(sys.keyboard_changes().is_empty());
        assert_eq!(
            invalidations.get(),
            1,
            "the missing requested cohort retired the old fact"
        );
        assert!(
            keeper.live.is_none(),
            "unseen native coverage supplies no live diagnostic"
        );
        available.store(true, Ordering::SeqCst);
        assert!(
            !keeper
                .round(
                    4999,
                    Some(7),
                    || sys.keyboard_service_observations(),
                    |_, _| panic!("unfinished retry remains paced")
                )
                .unwrap()
        );
        assert!(
            keeper
                .round(
                    5000,
                    Some(7),
                    || sys.keyboard_service_observations(),
                    activate
                )
                .unwrap()
        );
        let changed = sys.keyboard_changes();
        assert_eq!(
            invalidations.get(),
            2,
            "the actual native mutation retired before its write"
        );
        assert_eq!(
            changed.len(),
            1,
            "the unrelated model's NotApplicable row does not block coverage"
        );
        assert_eq!(changed[0].registry, REGISTRY);
        assert_eq!(changed[0].native, Some(vec![(CAPS_LOCK, LEFT_CONTROL)]));
        assert!(
            keeper.live.is_none(),
            "a setter's before-image is not its verified live state"
        );
        assert!(sys.calls().iter().all(|call| {
            !call.contains("defaults") || (!call.contains(" write ") && !call.contains(" delete "))
        }));
        assert!(
            !keeper
                .round(
                    5001,
                    Some(7),
                    || sys.keyboard_service_observations(),
                    |_, _| panic!("verified cohort settles")
                )
                .unwrap()
        );
        assert_eq!(
            keeper.live,
            Some((
                7,
                LiveKeyboardMapping {
                    control: 1,
                    explicit_other: 0,
                    held: 0,
                }
            ))
        );
        assert!(matches!(
            aterm_machines::init::activate_keyboard_attachments(
                &sys,
                &[REGISTRY],
                &|_| false,
                &|| true,
                &|| panic!("a real no-op must retain an existing valid observation"),
            )
            .unwrap(),
            KeyboardAttachmentActivation::Verified(_)
        ));
        assert_eq!(sys.keyboard_changes().len(), 1);

        // The same live service loses its map after a successfully verified activation.
        // Neither its registry identity nor the accepted setup revision changes.
        written_before_reset.store(changed.len(), Ordering::SeqCst);
        writes_before.set(changed.len());
        let reset = sys
            .run_complete_stdout(&Cmd::new("hidutil").args(["dump", "services", "-f", "xml"]))
            .unwrap();
        assert!(reset.success());
        assert_eq!(reset.out(), keyboard_dump(&[REGISTRY], &[]));
        assert_eq!(sys.keyboard_changes().len(), 1, "no reset-time setter");
        assert!(
            keeper
                .round(
                    5002,
                    Some(7),
                    || sys.keyboard_service_observations(),
                    activate
                )
                .unwrap(),
            "an unchanged service identity must not hide a lost effective mapping"
        );
        let repaired = sys.keyboard_changes();
        assert_eq!(repaired.len(), 2);
        assert_eq!(repaired[1].registry, REGISTRY);
        assert_eq!(repaired[1].native, Some(vec![(CAPS_LOCK, LEFT_CONTROL)]));
        assert_eq!(invalidations.get(), 3);
        let paid = sys.calls().len();
        assert!(
            !keeper
                .round(
                    5003,
                    Some(7),
                    || sys.keyboard_service_observations(),
                    |_, _| panic!("the verified repaired state needs no further activation")
                )
                .unwrap()
        );
        assert_eq!(sys.keyboard_changes().len(), 2);
        assert!(
            sys.calls()[paid..]
                .iter()
                .all(|call| call == "keyboard-service-ids")
        );

        let mut inputs = Inputs::default();
        let positive_readback = std::cell::Cell::new(false);
        let invalidate = || positive_readback.set(false);
        let round = |inputs: &mut Inputs, now, held| {
            inputs.round(
                true,
                now,
                || Ok(InputSetupScope::Applied(7)),
                || panic!("accepted inputs read one native mapping sample"),
                || sys.keyboard_service_observations(),
                |revision, ids| {
                    if held {
                        Ok(None)
                    } else {
                        activate(revision, ids)
                    }
                },
                &|| panic!("accepted inputs are not a pending stage"),
                &invalidate,
            )
        };
        assert!(round(&mut inputs, 6000, false).unwrap());
        assert_eq!(
            inputs.live,
            Some((
                7,
                LiveKeyboardMapping {
                    control: 1,
                    explicit_other: 0,
                    held: 0,
                }
            )),
            "an actual final-probe no-op preserves its known matching passive state"
        );
        assert_eq!(
            sys.keyboard_changes().len(),
            2,
            "the initial receipt is a no-op"
        );
        written_before_reset.store(2, Ordering::SeqCst);
        writes_before.set(2);
        assert!(round(&mut inputs, 6001, false).unwrap());
        assert!(inputs.live.is_none());
        assert_eq!(sys.keyboard_changes().len(), 3);
        positive_readback.set(true);
        assert!(!round(&mut inputs, 6002, false).unwrap());
        assert!(inputs.live.is_some());
        assert!(
            positive_readback.get(),
            "our verified write is not new drift"
        );
        written_before_reset.store(3, Ordering::SeqCst);
        assert!(!round(&mut inputs, 6003, true).unwrap());
        assert!(
            inputs.live.is_none(),
            "held repair retains retry custody, not a positive diagnostic"
        );
        assert!(
            !positive_readback.get(),
            "a reset to the old before-image retires the newer fact even while repair is held"
        );
        assert_eq!(
            sys.keyboard_changes().len(),
            3,
            "held repair writes nothing"
        );
        inputs
            .round(
                false,
                6004,
                || panic!("away console reads no scope"),
                || panic!("away console reads no IDs"),
                || panic!("away console reads no maps"),
                |_, _| panic!("away console never activates"),
                &|| {},
                &invalidate,
            )
            .unwrap();
        assert!(inputs.live.is_none());
        inputs
            .round(
                true,
                6005,
                || Ok(InputSetupScope::Inactive),
                || panic!("withdrawn scope reads no IDs"),
                || panic!("withdrawn scope reads no maps"),
                |_, _| panic!("withdrawn scope never activates"),
                &|| {},
                &invalidate,
            )
            .unwrap();
        assert!(inputs.live.is_none());
    }
}
