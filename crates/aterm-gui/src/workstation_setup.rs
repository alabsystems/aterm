// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-launch Mac setup's cancellable, foreground-only countdown.
//! Machine changes belong to the binary's worker. This module can only return a
//! decision, after an explicit Apply or the complete cancellation interval.

use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::{Duration, Instant};

use aterm_types::macos_setup::{Plan, Selection};
use aterm_window::event_loop::EventLoopProxy;

use crate::{App, Wake};

/// The result of the visible first-launch countdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkstationSetupDecision {
    /// Apply was selected, or thirty foreground seconds elapsed, with these boxes checked.
    Begin(Plan),
    /// The person cancelled setup before it began.
    Cancel,
    /// This process has no live window, or its window closed before a decision.
    Unavailable,
}

/// The review's decision and the optional, ephemeral vault password.
#[derive(Debug)]
pub struct SetupAnswer {
    pub decision: WorkstationSetupDecision,
    pub password: Option<aterm_secrets::SecretBytes>,
    pub receipt: Option<crate::InitialSetupApplyReceipt>,
    pub cancel_receipt: Option<crate::InitialSetupCancelReceipt>,
    /// The vault field no longer reads as System State showed it: the person typed the
    /// vault, which moves the Mac. A field left as shown asks for the vault setup shows
    /// when the Apply is admitted, which a vault joined since may have changed.
    pub vault_typed: bool,
}

impl From<WorkstationSetupDecision> for SetupAnswer {
    fn from(decision: WorkstationSetupDecision) -> Self {
        Self {
            decision,
            password: None,
            receipt: None,
            cancel_receipt: None,
            vault_typed: false,
        }
    }
}

static PROXY: Mutex<Option<EventLoopProxy<Wake>>> = Mutex::new(None);

pub(crate) fn send_setup_wake(wake: Wake) -> bool {
    PROXY
        .lock()
        .ok()
        .and_then(|proxy| proxy.clone())
        .is_some_and(|proxy| proxy.send_event(wake).is_ok())
}

/// Wait on a worker thread for the Mac setup countdown. Without a registered
/// window event loop this returns immediately; never call it on the UI thread.
/// This decision-only adapter drops admission receipts. Provisioning callers
/// must use [`await_initial_setup_countdown_while`] and acknowledge durability.
pub fn await_workstation_setup_countdown(initial: Selection) -> WorkstationSetupDecision {
    await_workstation_setup_countdown_while(initial, || false)
}

/// Withdraw the offer if another process finishes setup while it is displayed.
/// `finished` runs on the waiting worker, never on the application event loop.
pub fn await_workstation_setup_countdown_while(
    initial: Selection,
    finished: impl FnMut() -> bool,
) -> WorkstationSetupDecision {
    await_initial_setup_countdown_while(
        Plan {
            selection: initial,
            ..Plan::default()
        },
        finished,
    )
    .decision
}

/// Wait for the unified setup review. The password is returned once in scrubbed
/// storage and is never part of the durable plan.
pub fn await_initial_setup_countdown_while(
    initial: Plan,
    finished: impl FnMut() -> bool,
) -> SetupAnswer {
    let proxy = PROXY.lock().ok().and_then(|proxy| proxy.clone());
    let Some(proxy) = proxy else {
        return WorkstationSetupDecision::Unavailable.into();
    };
    let (send, receive) = mpsc::channel();
    let request = Request::new(initial, send);
    let pending = Arc::downgrade(&request.reply);
    if proxy.send_event(Wake::WorkstationSetup(request)).is_err() {
        return WorkstationSetupDecision::Unavailable.into();
    }
    wait_for_decision(pending, receive, finished, Duration::from_secs(1))
}

type Reply = Mutex<Option<mpsc::Sender<SetupAnswer>>>;

fn wait_for_decision(
    pending: Weak<Reply>,
    receive: mpsc::Receiver<SetupAnswer>,
    mut finished: impl FnMut() -> bool,
    poll: Duration,
) -> SetupAnswer {
    loop {
        match receive.recv_timeout(poll) {
            Ok(decision) => return decision,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return WorkstationSetupDecision::Unavailable.into();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if finished()
                    && let Some(reply) = pending.upgrade()
                {
                    finish_reply(&reply, WorkstationSetupDecision::Unavailable.into());
                }
            }
        }
    }
}

fn finish_reply(reply: &Reply, answer: SetupAnswer) -> bool {
    if let Ok(mut reply) = reply.lock()
        && let Some(reply) = reply.take()
    {
        return reply.send(answer).is_ok();
    }
    false
}

#[derive(Clone, Debug)]
pub(crate) struct Request {
    initial: Plan,
    reply: Arc<Reply>,
}

impl Request {
    pub(crate) fn new(initial: impl Into<Plan>, reply: mpsc::Sender<SetupAnswer>) -> Self {
        let initial = initial.into();
        Self {
            initial,
            reply: Arc::new(Mutex::new(Some(reply))),
        }
    }
    /// The native callback and expiry compete for this single decision. A Cancel
    /// already delivered by AppKit cannot be overtaken by the next timer tick.
    pub(crate) fn finish(&self, decision: WorkstationSetupDecision) {
        self.finish_answer(decision.into());
    }

    pub(crate) fn finish_answer(&self, answer: SetupAnswer) -> bool {
        finish_reply(&self.reply, answer)
    }

    pub(crate) fn initial(&self) -> &Plan {
        &self.initial
    }

    pub(crate) fn pending(&self) -> bool {
        self.reply.lock().is_ok_and(|reply| reply.is_some())
    }

    pub(crate) fn same_offer(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.reply, &other.reply)
    }
}

const COUNTDOWN: Duration = Duration::from_secs(30);

/// Only intervals during which the sheet is visible in the active application
/// count. Backgrounding the app pauses, rather than silently consuming, time.
#[derive(Debug)]
pub(crate) struct Clock {
    remaining: Duration,
    active_since: Option<Instant>,
    modal_epoch: u64,
    interacted: bool,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            remaining: COUNTDOWN,
            active_since: None,
            modal_epoch: modal_epoch(),
            interacted: false,
        }
    }
}

impl Clock {
    pub(crate) fn tick(&mut self, now: Instant, active: bool) -> Duration {
        self.tick_at_epoch(now, active, modal_epoch())
    }

    fn tick_at_epoch(&mut self, now: Instant, active: bool, modal_epoch: u64) -> Duration {
        if self.interacted {
            return self.remaining;
        }
        if self.modal_epoch != modal_epoch {
            self.active_since = None;
            self.modal_epoch = modal_epoch;
        }
        if let Some(since) = self.active_since.take() {
            self.remaining = self
                .remaining
                .saturating_sub(now.saturating_duration_since(since));
        }
        if active {
            self.active_since = Some(now);
        }
        self.remaining
    }

    pub(crate) fn seconds(&self) -> u64 {
        self.remaining.as_secs() + u64::from(self.remaining.subsec_nanos() != 0)
    }

    pub(crate) fn interact(&mut self) {
        self.interacted = true;
        self.active_since = None;
    }

    pub(crate) fn expired(&self) -> bool {
        !self.interacted && self.remaining.is_zero()
    }

    pub(crate) fn interacted(&self) -> bool {
        self.interacted
    }
}

fn modal_epoch() -> u64 {
    #[cfg(any(target_os = "macos", test))]
    {
        crate::watchdog::modal_epoch()
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        0
    }
}

#[derive(Default)]
enum PermissionHold {
    #[default]
    None,
    Decision,
    FreshStatus,
}

#[derive(Default)]
struct PermissionGate {
    policy: Option<crate::InitialSetupPermissionPolicy>,
    latest_revision: u64,
    hold: PermissionHold,
}

impl PermissionGate {
    fn allowed(&self) -> bool {
        matches!(self.hold, PermissionHold::None)
            && self.policy.is_some_and(|policy| policy.allowed)
    }

    fn hold(&mut self) {
        self.hold = PermissionHold::Decision;
    }

    fn accepted(&mut self) {
        // The receipt establishes durable admission, but the cached snapshot
        // can still describe the previous plan. Only a new read releases it.
        self.await_fresh_status();
    }

    fn await_fresh_status(&mut self) {
        self.hold = PermissionHold::FreshStatus;
    }

    fn observe(&mut self, policy: Option<crate::InitialSetupPermissionPolicy>) {
        let Some(policy) = policy else {
            self.policy = None;
            return;
        };
        if policy.allowed && policy.revision < self.latest_revision {
            return;
        }
        self.latest_revision = self.latest_revision.max(policy.revision);
        self.policy = Some(policy);
        if matches!(self.hold, PermissionHold::FreshStatus) {
            self.hold = PermissionHold::None;
        }
    }
}

#[derive(Default)]
pub(crate) struct Host {
    startup: Option<fn() -> bool>,
    startup_retry: Option<Instant>,
    request: Option<Request>,
    unified: bool,
    permissions: PermissionGate,
    proxy_registered: bool,
}

impl Host {
    pub(crate) fn for_launch(startup: Option<fn() -> bool>, headless: bool) -> Self {
        let preview = aterm_types::dev_seam!("ATERM_DEBUG_WORKSTATION_SETUP");
        let mode = preview_mode(
            preview.is_some(),
            headless,
            aterm_types::dirs::runs_under_private_state_root(),
        );
        let startup = match mode {
            PreviewMode::Ordinary => startup,
            PreviewMode::Preview => Some(start_preview as fn() -> bool),
            PreviewMode::Refused => {
                aterm_log::warn!("Mac setup preview requires a window and a private state root");
                None
            }
        };
        Self {
            unified: startup.is_some(),
            startup,
            startup_retry: None,
            request: None,
            permissions: PermissionGate::default(),
            proxy_registered: false,
        }
    }

    fn offer(&mut self, request: Request) {
        if self.pending() {
            request.finish(WorkstationSetupDecision::Unavailable);
        } else {
            self.request = Some(request);
        }
    }

    fn finish(&mut self, decision: WorkstationSetupDecision) {
        if let Some(request) = self.request.take() {
            request.finish(decision);
        }
    }

    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        self.request
            .as_ref()
            .map(|_| now + Duration::from_millis(250))
            .into_iter()
            .chain(self.startup_retry)
            .min()
    }

    pub(crate) fn pending(&self) -> bool {
        self.request.as_ref().is_some_and(Request::pending)
    }
    pub(crate) fn unified(&self) -> bool {
        self.unified
    }
    pub(crate) fn suppresses_permissions(&self) -> bool {
        self.unified && !self.permissions.allowed()
    }

    fn start(&mut self, now: Instant) {
        if self.startup_retry.is_some_and(|retry| retry > now) {
            return;
        }
        if let Some(startup) = self.startup {
            if startup() {
                self.startup = None;
                self.startup_retry = None;
            } else {
                self.startup_retry = Some(now + Duration::from_secs(60));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewMode {
    Ordinary,
    Preview,
    Refused,
}

fn preview_mode(requested: bool, headless: bool, private: bool) -> PreviewMode {
    if !requested {
        PreviewMode::Ordinary
    } else if !headless && private {
        PreviewMode::Preview
    } else {
        PreviewMode::Refused
    }
}

/// Exercise the real countdown with an inert worker. Its only write is a result
/// beneath the already-private instance state directory.
fn preview_subset() -> Selection {
    use aterm_types::macos_setup::Setting;
    let mut selected = Selection::all();
    for row in [Setting::Dock, Setting::Messages, Setting::FaceTime] {
        selected.set(row, false);
    }
    selected
}

fn start_preview() -> bool {
    crate::qos::spawn_lane_thread(
        "workstation-setup-preview",
        crate::qos::Role::Background,
        || {
            let started = Instant::now();
            let initial = if aterm_types::dev_seam!("ATERM_DEBUG_WORKSTATION_SETUP").as_deref()
                == Some(std::ffi::OsStr::new("saved-subset"))
            {
                preview_subset()
            } else {
                Selection::all()
            };
            let mut answer = await_initial_setup_countdown_while(initial.into(), || false);
            if matches!(answer.decision, WorkstationSetupDecision::Begin(_))
                && let Some(receipt) = answer.receipt.take()
            {
                // This private preview never provisions anything. Acknowledge
                // its inert answer so it exercises the production UI lifecycle.
                receipt.accept();
            }
            let cancel_receipt = answer.cancel_receipt.take();
            let saved = if let Some(root) = aterm_types::dirs::state_dir() {
                let word = match answer.decision {
                    WorkstationSetupDecision::Begin(plan) => {
                        format!("begin\nselection_bits={}\n", plan.selection.bits())
                    }
                    WorkstationSetupDecision::Cancel => "cancel\n".to_owned(),
                    WorkstationSetupDecision::Unavailable => "unavailable\n".to_owned(),
                };
                let result = format!("{word}elapsed_ms={}\n", started.elapsed().as_millis());
                match std::fs::write(root.join("workstation-setup-preview-result"), result) {
                    Ok(()) => true,
                    Err(error) => {
                        aterm_log::warn!("Mac setup preview could not write its result: {error}");
                        false
                    }
                }
            } else {
                false
            };
            if saved && let Some(receipt) = cancel_receipt {
                receipt.complete();
            }
        },
    )
    .is_ok()
}

impl Drop for Host {
    fn drop(&mut self) {
        self.finish(WorkstationSetupDecision::Unavailable);
    }
}

/// First paint and Commit are independent requirements: a candidate can paint
/// before it is allowed to start workers or change the host machine.
fn may_start(headless: bool, painted: bool, incoming_pending: bool) -> bool {
    !headless && painted && !incoming_pending
}

/// A carried command is inert in a validated running-window handoff. A lone
/// relaunch environment variable cannot turn a scripted launch into setup.
pub(crate) fn eligible_launch_command(
    commandless: bool,
    relaunch: bool,
    admitted_handoff: bool,
) -> bool {
    commandless || (relaunch && admitted_handoff)
}

impl App {
    pub(crate) fn offer_workstation_setup(&mut self, request: Request) {
        if self.headless
            || self
                .windows
                .values()
                .all(|window| window.os_window.is_none())
            || !self.any_os_window_focused()
        {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        if self.workstation_setup.pending() {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        self.workstation_setup.offer(request.clone());
        self.offer_system_state(request);
    }

    pub(crate) fn hold_initial_setup_permissions(&mut self) {
        self.workstation_setup.unified = true;
        self.workstation_setup.permissions.hold();
    }

    pub(crate) fn accepted_initial_setup_apply(&mut self) {
        self.workstation_setup.permissions.accepted();
    }

    pub(crate) fn recheck_initial_setup_permissions(&mut self) {
        self.workstation_setup.permissions.await_fresh_status();
    }

    pub(crate) fn observe_initial_setup_permissions(
        &mut self,
        policy: Option<crate::InitialSetupPermissionPolicy>,
    ) {
        self.workstation_setup.permissions.observe(policy);
    }

    pub(crate) fn tick_workstation_setup(&mut self, now: Instant) {
        if self.workstation_setup.pending()
            && !self
                .windows
                .values()
                .any(|window| window.os_window.is_some())
        {
            self.workstation_setup
                .finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        if !may_start(
            self.headless,
            self.first_present_done,
            self.incoming_handoff_pending,
        ) {
            return;
        }
        if !self.workstation_setup.proxy_registered {
            if let Ok(mut proxy) = PROXY.lock() {
                *proxy = self.proxy.clone();
            }
            self.workstation_setup.proxy_registered = self.proxy.is_some();
        }
        self.workstation_setup.start(now);
        self.tick_system_state(now);
        if self
            .workstation_setup
            .request
            .as_ref()
            .is_some_and(|request| !request.pending())
        {
            self.workstation_setup.request = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_policy_holds_until_admission_and_a_fresh_authoritative_snapshot() {
        let allowed = Some(crate::InitialSetupPermissionPolicy {
            revision: 7,
            allowed: true,
        });
        let mut gate = PermissionGate::default();
        assert!(!gate.allowed());
        gate.observe(allowed);
        assert!(
            gate.allowed(),
            "completed startup can restore guidance without an offer"
        );
        gate.hold();
        gate.observe(allowed);
        assert!(
            !gate.allowed(),
            "an old durable plan cannot undo a local Cancel or pending Apply"
        );
        gate.accepted();
        assert!(
            !gate.allowed(),
            "admission cannot release a cached earlier snapshot"
        );
        gate.observe(allowed);
        assert!(gate.allowed());
        gate.observe(None);
        assert!(!gate.allowed(), "an unreadable snapshot holds guidance");
        gate.observe(Some(crate::InitialSetupPermissionPolicy {
            revision: 8,
            allowed: false,
        }));
        gate.observe(allowed);
        assert!(!gate.allowed(), "an older run cannot re-enable guidance");
    }

    #[test]
    fn startup_waits_for_both_first_paint_and_commit() {
        assert!(!may_start(false, false, false));
        assert!(!may_start(false, true, true));
        assert!(!may_start(true, true, false));
        assert!(may_start(false, true, false));
    }

    #[test]
    fn command_payload_only_qualifies_when_carried_by_a_validated_handoff() {
        assert!(eligible_launch_command(true, false, false));
        assert!(eligible_launch_command(false, true, true));
        assert!(!eligible_launch_command(false, true, false));
        assert!(!eligible_launch_command(false, false, true));
        assert!(!eligible_launch_command(false, false, false));
    }

    #[test]
    fn preview_replaces_production_only_in_a_private_window() {
        assert_eq!(preview_mode(true, false, true), PreviewMode::Preview);
        assert_eq!(preview_mode(true, true, true), PreviewMode::Refused);
        assert_eq!(preview_mode(true, false, false), PreviewMode::Refused);
        assert_eq!(preview_mode(false, false, false), PreviewMode::Ordinary);
    }

    #[test]
    fn countdown_needs_thirty_visible_seconds_and_pauses_in_background() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.tick(start, true);
        assert_eq!(
            clock.tick(start + Duration::from_secs(12), false),
            Duration::from_secs(18)
        );
        assert_eq!(
            clock.tick(start + Duration::from_secs(100), true),
            Duration::from_secs(18)
        );
        assert_eq!(
            clock.tick(start + Duration::from_millis(117_999), true),
            Duration::from_millis(1)
        );
        assert_eq!(clock.seconds(), 1);
        assert_eq!(
            clock.tick(start + Duration::from_secs(118), true),
            Duration::ZERO
        );
    }

    #[test]
    fn fresh_interaction_permanently_stops_the_offer_even_after_long_waits() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.tick(start, true);
        clock.tick(start + Duration::from_secs(4), true);
        clock.interact();
        assert_eq!(
            clock.tick(start + Duration::from_secs(90), true),
            Duration::from_secs(26)
        );
        clock.tick(start + Duration::from_secs(100), false);
        clock.tick(start + Duration::from_secs(200), true);
        assert!(!clock.expired());
    }

    #[test]
    fn a_blocking_native_modal_cannot_consume_the_cancellation_interval() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.tick_at_epoch(start, true, 10);
        assert_eq!(
            clock.tick_at_epoch(start + Duration::from_secs(2), true, 10),
            Duration::from_secs(28)
        );
        // A nested runModal owns the event loop for forty seconds and returns
        // to the same focused sheet. Its entry and exit changed the epoch.
        assert_eq!(
            clock.tick_at_epoch(start + Duration::from_secs(42), true, 12),
            Duration::from_secs(28)
        );
        assert_eq!(
            clock.tick_at_epoch(start + Duration::from_secs(69), true, 12),
            Duration::from_secs(1)
        );
        assert_eq!(
            clock.tick_at_epoch(start + Duration::from_secs(70), true, 12),
            Duration::ZERO
        );
    }

    #[test]
    fn a_failed_worker_spawn_is_retried_after_backoff_and_success_consumes_startup() {
        thread_local! { static ATTEMPTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) }; }
        fn attempt() -> bool {
            ATTEMPTS.set(ATTEMPTS.get() + 1);
            ATTEMPTS.get() > 1
        }
        ATTEMPTS.set(0);
        let now = Instant::now();
        let mut host = Host::default();
        host.startup = Some(attempt);
        host.start(now);
        assert_eq!(ATTEMPTS.get(), 1);
        assert_eq!(host.deadline(now), Some(now + Duration::from_secs(60)));
        host.start(now + Duration::from_secs(59));
        assert_eq!(ATTEMPTS.get(), 1);
        host.start(now + Duration::from_secs(60));
        host.start(now + Duration::from_secs(120));
        assert_eq!(ATTEMPTS.get(), 2);
        assert!(host.startup.is_none());
        assert_eq!(host.deadline(now), None);
    }

    #[test]
    fn completion_elsewhere_withdraws_a_still_waiting_offer() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(preview_subset(), send);
        assert_eq!(request.initial.selection, preview_subset());
        assert_eq!(
            wait_for_decision(
                Arc::downgrade(&request.reply),
                receive,
                || true,
                Duration::ZERO
            )
            .decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(!request.pending());
        request.finish(WorkstationSetupDecision::Begin(Selection::all().into()));
    }

    #[test]
    fn closing_the_event_loop_before_dispatch_does_not_leave_the_worker_waiting() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Selection::all(), send);
        let pending = Arc::downgrade(&request.reply);
        drop(request); // the queued Wake was discarded when the event loop closed
        assert_eq!(
            wait_for_decision(pending, receive, || false, Duration::from_secs(60)).decision,
            WorkstationSetupDecision::Unavailable
        );
    }

    #[test]
    fn cancel_delivered_at_expiry_cannot_be_replaced_by_begin() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Selection::all(), send);
        request.finish(WorkstationSetupDecision::Cancel);
        request.finish(WorkstationSetupDecision::Begin(Selection::all().into()));
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .decision,
            WorkstationSetupDecision::Cancel
        );
        assert!(!request.pending());
    }

    #[test]
    fn apply_now_cannot_be_replaced_by_later_cancel_or_expiry() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Selection::all(), send);
        let chosen = preview_subset();
        assert!(request.finish_answer(WorkstationSetupDecision::Begin(chosen.into()).into()));
        assert!(!request.finish_answer(WorkstationSetupDecision::Cancel.into()));
        assert!(
            !request
                .finish_answer(WorkstationSetupDecision::Begin(Selection::none().into()).into())
        );
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .decision,
            WorkstationSetupDecision::Begin(chosen.into())
        );
        assert!(!request.pending());

        let (send, receive) = mpsc::channel();
        let request = Request::new(Selection::all(), send);
        drop(receive);
        assert!(!request.finish_answer(WorkstationSetupDecision::Begin(chosen.into()).into()));
        assert!(!request.pending());
    }

    #[test]
    fn closing_window_releases_a_waiting_worker() {
        let (send, receive) = mpsc::channel();
        let mut host = Host::default();
        host.offer(Request::new(Selection::all(), send));
        drop(host);
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .decision,
            WorkstationSetupDecision::Unavailable
        );
    }

    #[test]
    fn losing_the_last_window_releases_the_worker_while_the_app_lives() {
        let (send, receive) = mpsc::channel();
        let mut app = App::headless_for_test();
        app.workstation_setup
            .offer(Request::new(Selection::all(), send));
        // Headless test windows have no OS window: the offer cannot be
        // answered, so the worker is released instead of parked.
        app.tick_workstation_setup(Instant::now());
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(!app.workstation_setup.pending());
    }

    #[test]
    fn an_offer_after_the_last_window_closed_is_unavailable() {
        let (send, receive) = mpsc::channel();
        let mut app = App::headless_for_test();
        app.headless = false;
        app.offer_workstation_setup(Request::new(Selection::all(), send));
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(!app.workstation_setup.pending());
    }
}
