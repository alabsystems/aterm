// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Boundary between the System State tab and the binary's setup workers.

use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::{Access, Plan, Selection};

use crate::native_app::{AppKind, NativeApp};
use crate::system_state::{
    Admission, PasswordAcceptance, SetupOperation, SetupStatus, SetupStatusRow, SystemStateAction,
};
use crate::workstation_setup::{OfferKind, Request, WorkstationSetupDecision};
use crate::{App, Wake};

/// Provisioning callbacks installed by the shipped binary.
#[derive(Clone, Copy)]
pub struct InitialSetupHandlers {
    /// Durably admit `plan` (with its vault-typed flag and optional join
    /// password) as the next run; the receipt says whose choice it is and
    /// whether it shapes the plan. A choice takes effect at once (design §3.3):
    /// a running pass stops at its next stage boundary and `plan` runs after
    /// it, and a later call overtakes one still waiting, whose dropped receipt
    /// keeps the page's input.
    pub apply: fn(Plan, bool, Option<SecretBytes>, InitialSetupApplyReceipt) -> bool,
    /// Start Check again for `plan`: its forced reading of every row, and the
    /// check itself, which a setup pass holding the worker refuses (design
    /// §3.2). Check again started when either did; the window labels it a
    /// check only when the check did.
    pub check: fn(Plan) -> CheckStarted,
    pub restore: fn() -> bool,
    pub status: fn() -> SetupStatus,
    /// Start one read-only, prompt-free observation of every row for `plan`
    /// (managed rows decide which network reads run). Its result arrives in
    /// a later `status()`. Never holds the setup slot.
    /// Returns whether the worker accepted the observation.
    pub observe: fn(Plan) -> bool,
    /// While the GitHub repositories need a person, look on a worker whether a token renewed
    /// in the vault reached this Mac: the vault's materialization mark, a local reading that
    /// asks nothing of GitHub. Only when it moved since that row was last read is the GitHub
    /// repositories row read again for `plan`, and that row alone (design §3.6). Returns
    /// whether the look started.
    pub look_for_token: fn(Plan) -> bool,
    /// Stop was pressed: the pass going now stops at its next stage boundary,
    /// and so does the run of every plan sent before the press that is still
    /// being admitted, once it registers (design §3.3). Asked once: the worker
    /// keeps it. It touches no file on the UI thread: a worker saves the Stop,
    /// and [`Self::settle`] waits for that save.
    pub stop: fn(),
    /// The app is quitting, or an update's handoff is about to end it: wait,
    /// briefly, for what a Stop still owes the person's records (a plan it
    /// stops still being admitted, the Stop's save), so a quit right after a
    /// Stop never resumes the run (design §3.3). At once when nothing is owed.
    pub settle: fn(),
    /// Turn On Sync on the Secrets vault row: a worker reinstalls this Mac's
    /// subscriber, removes the record of the person's opt-out and reads the
    /// row again (design §3.3). Returns whether that worker started.
    pub sync_on: fn() -> bool,
    /// The window verified its own Full Disk Access grant without an admitted
    /// access revision to record it under. Refresh observations and finish any
    /// previously owed preference work on a worker, without admitting prompts.
    pub fda_granted: fn(),
}

#[cfg(test)]
impl InitialSetupHandlers {
    pub(crate) fn inert_for_test() -> Self {
        Self {
            apply: |_, _, _, _| false,
            check: |_| CheckStarted::default(),
            restore: || false,
            status: SetupStatus::default,
            observe: |_| false,
            look_for_token: |_| false,
            stop: || {},
            settle: || {},
            sync_on: || false,
            fda_granted: || {},
        }
    }
}

/// What a press of Check again started ([`InitialSetupHandlers::check`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckStarted {
    /// The forced reading of every row, GitHub repositories included.
    pub reading: bool,
    /// The check on the setup worker: refused while a pass holds it.
    pub check: bool,
}

/// Receipt owned by a queued Apply until its plan is durably authorized.
#[must_use = "accept only after the plan is durable; dropping preserves the draft"]
#[derive(Debug)]
pub struct InitialSetupApplyReceipt {
    answer: Option<Sender<bool>>,
    admission: Admission,
    chosen: bool,
    changed: Option<Selection>,
    password: Option<PasswordAcceptance>,
}

impl InitialSetupApplyReceipt {
    fn new(
        admission: Admission,
        chosen: bool,
        changed: Option<Selection>,
        password: Option<PasswordAcceptance>,
    ) -> (Self, Receiver<bool>) {
        let (answer, receiver) = mpsc::channel();
        (
            Self {
                answer: Some(answer),
                admission,
                chosen,
                changed,
                password,
            },
            receiver,
        )
    }

    /// A person's receipt for the setup worker's own tests to hand its entry
    /// points, and the answer it sends ([`Self::accept`], or `false` dropped).
    #[cfg(any(test, debug_assertions))]
    pub fn detached(chosen: bool) -> (Self, Receiver<bool>) {
        Self::new(Admission::Person, chosen, None, None)
    }

    /// The settings the page changed since it last took the plan in force, which the worker
    /// applies onto that plan as it admits this one ([`crate::SetupAnswer::changed`]).
    pub fn changed(&self) -> Option<Selection> {
        self.changed
    }

    /// Whether the plan is a group choice (or a Join) made on the page, which
    /// makes it the chooser's for good, rather than the plan as it stood (Set
    /// up now, or the first-run offer), which leaves a provisional plan
    /// provisional (design §3.5; [`crate::SetupAnswer::chosen`]).
    pub fn chosen(&self) -> bool {
        self.chosen
    }

    /// Whose choice the plan is (design §6): the page's own answer to the
    /// first-run offer, a person's press, or a control client's `act` or
    /// `key`. Only the first two are the window's own admission; a client's
    /// never stands as the yes the access steps need, and its plan is not
    /// the provisional one aterm chose by itself.
    pub fn admission(&self) -> Admission {
        self.admission
    }

    /// Confirm that the worker holds the claim and durably saved the reviewed plan.
    pub fn accept(mut self) {
        self.finish(true);
    }

    fn finish(&mut self, accepted: bool) {
        if let Some(answer) = self.answer.take() {
            if accepted && let Some(password) = &self.password {
                password.accept();
            }
            let _ = answer.send(accepted);
            crate::workstation_setup::send_setup_wake(Wake::InitialSetupApplyResolved);
        }
    }
}

impl Drop for InitialSetupApplyReceipt {
    fn drop(&mut self) {
        self.finish(false);
    }
}

struct PendingApply {
    instance: crate::native_app::AppInstanceId,
    admission: Admission,
    plan: Plan,
    answer: Receiver<bool>,
    /// Stop may keep this admission's answer while withdrawing its right to
    /// release automatic permission guidance in this process.
    release_permissions: bool,
}

static HANDLERS: Mutex<Option<InitialSetupHandlers>> = Mutex::new(None);

pub fn set_initial_setup_handlers(handlers: InitialSetupHandlers) {
    if let Ok(mut current) = HANDLERS.lock() {
        *current = Some(handlers);
    }
}

pub fn publish_initial_setup_status(status: SetupStatus) {
    crate::workstation_setup::send_setup_wake(Wake::InitialSetupStatus {
        generation: None,
        status,
    });
}

/// The dev seam that shows System State a fixed scenario in place of the
/// setup worker (`crate::system_state::fixture_status`): its actions are inert.
const FIXTURE_SEAM: &str = "ATERM_DEV_SYSTEM_STATE_FIXTURE";

/// A read that met a write of the saved setup ([`SetupStatus::changing`]) is read again this
/// soon: the write is a rename, over in a moment.
const CHANGING_REREAD: Duration = Duration::from_millis(100);

/// Reads in a row that may meet a write before the page shows the next as it is: records that
/// keep changing show as its pending row. The window's own admission keeps the last read that
/// named a plan all the same, since a read that met a write says nothing of one.
const CHANGING_READS: u8 = 10;

/// How long a shown page goes between two looks for a renewed GitHub token while the GitHub
/// repositories need a person ([`SetupStatus::github_needs_person`]): their remedy is the
/// vault's (a token renewed there and synced here), which tells the page nothing. Each look
/// checks the vault's local mark alone, and reads the GitHub repositories row again only when
/// that mark moved ([`InitialSetupHandlers::look_for_token`]); the page reads every row again
/// whenever its window regains focus (§3.6).
pub(crate) const TOKEN_LOOK: Duration = Duration::from_secs(60);

/// How a request to read the shown page's rows went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Observe {
    /// The reading started (or there is nothing to read: a scenario, no worker).
    Started,
    /// The worker did not start it: it is tried again a second later.
    Refused,
    /// The page waits for the plan in force (§3.5): read at the first tick that knows it.
    NoPlan,
}

#[derive(Default)]
pub(crate) struct Host {
    pending: Option<Request>,
    pending_apply: Option<PendingApply>,
    /// An accepted plan's run has not shown up in a fresh status yet: the
    /// page stays busy meanwhile.
    awaiting_run: bool,
    /// A test's own handlers, in place of the binary's ([`App::system_state_handlers`]).
    #[cfg(test)]
    handlers: Option<InitialSetupHandlers>,
    /// An accepted nonrun operation, reserved across controller closure until
    /// a fresh status says it ended or a setup pass takes over.
    operation: Option<SetupOperation>,
    next_status: Option<Instant>,
    status_in_flight: bool,
    /// Reads in a row that met a write of the saved setup ([`SetupStatus::changing`]).
    changing_reads: u8,
    next_tick: Option<Instant>,
    generation: u64,
    offer_presented: bool,
    /// A review was shown in this process: a later one (an offer no window could show the first
    /// time, or the review of the groups the ALab signal adds) opens its tab behind the person's
    /// and waits there, never taking the tab they are in (design §6).
    review_shown: bool,
    /// The launch review's last view closed before the worker's Closed answer/status caught up.
    /// Its unadmitted rows stay calm until durable status says the review has given way.
    closed_launch_review: bool,
    /// A System State view was visible at the last tick: showing it again
    /// asks for a fresh observation (§3.5).
    shown: bool,
    /// A refused observation start is tried again on the page's tick,
    /// no sooner than this deadline.
    observe_retry: Option<Instant>,
    /// When the shown page's rows were last read, or last looked at for a renewed GitHub
    /// token ([`TOKEN_LOOK`]).
    token_looked_at: Option<Instant>,
    /// The plan in force when the shown page's rows were last read: a status
    /// that names another (a Restore in a terminal) has them read again.
    observed_plan: Option<Plan>,
    /// The access approval whose Not now answers the last reading interpreted.
    /// A new approval can keep the same plan and still needs fresh Access rows.
    observed_access_revision: Option<u64>,
    action_refusal: Option<SetupStatusRow>,
    latest_status: SetupStatus,
    /// The access step last published with the status.
    access_step: Option<Access>,
    /// The dev scenario shown in place of the worker, read once.
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fixture: Option<SetupStatus>,
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fixture_read: bool,
    /// The first launch's review the dev scenario shows, until the page takes it.
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fixture_review: Option<crate::system_state::FixtureReview>,
}

impl Host {
    /// The status last published to System State (`system_state_access`'s
    /// menu line reads it).
    pub(crate) const fn latest_status(&self) -> &SetupStatus {
        &self.latest_status
    }

    /// The wake [`App::tick_system_state`] serves: the next tick, and the next
    /// status poll unless one is in flight (its reply wakes the loop itself).
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        self.next_status
            .filter(|_| !self.status_in_flight)
            .into_iter()
            .chain(self.next_tick)
            .min()
            .map(|next| next.max(now))
    }

    /// The dev scenario, when the seam names one (debug and dev-seam builds).
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fn fixture(&mut self) -> Option<&SetupStatus> {
        if !self.fixture_read {
            self.fixture_read = true;
            let mut review = None;
            self.fixture = aterm_types::dev_seam!(FIXTURE_SEAM).and_then(|name| {
                let name = name.to_str()?.to_owned();
                let status = crate::system_state::fixture_status(&name);
                if status.is_none() {
                    aterm_log::warn!("{FIXTURE_SEAM}={name} names no System State scenario");
                }
                review = crate::system_state::fixture_review(&name);
                status
            });
            self.fixture_review = review;
        }
        self.fixture.as_ref()
    }

    /// A release build shows no dev scenario.
    #[cfg(not(any(debug_assertions, feature = "dev-seams")))]
    fn fixture(&mut self) -> Option<&SetupStatus> {
        None
    }

    /// Whether a dev scenario stands in for the worker (its actions inert).
    fn showing_fixture(&self) -> bool {
        #[cfg(any(debug_assertions, feature = "dev-seams"))]
        {
            self.fixture.is_some()
        }
        #[cfg(not(any(debug_assertions, feature = "dev-seams")))]
        {
            false
        }
    }
}

impl App {
    /// The binary's handlers ([`set_initial_setup_handlers`]), or a test's own.
    fn system_state_handlers(&self) -> Option<InitialSetupHandlers> {
        #[cfg(test)]
        if let Some(handlers) = self.system_state_host.handlers {
            return Some(handlers);
        }
        HANDLERS.lock().ok().and_then(|handlers| *handlers)
    }

    /// Hand this App's System State `handlers` in place of the binary's.
    #[cfg(test)]
    pub(crate) fn set_system_state_handlers_for_test(&mut self, handlers: InitialSetupHandlers) {
        self.system_state_host.handlers = Some(handlers);
    }

    /// A verified grant without a recordable revision still refreshes the rows
    /// and lets the worker finish already-owed preferences. It admits no access step.
    pub(crate) fn notify_initial_setup_fda_granted(&self) {
        if let Some(handlers) = self.system_state_handlers() {
            (handlers.fda_granted)();
        }
    }

    /// The event loop has ended: an offer nobody answered is let go, and setup
    /// resumes at the next launch, unless a Stop was pressed: the quit waits
    /// for what it owes the records ([`InitialSetupHandlers::settle`]).
    pub(crate) fn finish_system_state_shutdown(&mut self) {
        let handlers = self.system_state_handlers();
        self.finish_system_state_shutdown_with(handlers);
    }

    /// The first launch's review is up and unanswered: its headline is the page's one signal
    /// meanwhile, so neither the menu line nor the tab's diamond says anything (§3.3, §3.6).
    pub(crate) fn system_state_review_pending(&self) -> bool {
        self.system_state_host
            .pending
            .as_ref()
            .is_some_and(Request::pending)
    }

    /// Queued setup and Restore still belong to this worker; a live installer also
    /// keeps its prompt guard here until exit or final detachment.
    pub(crate) fn system_state_handoff_pending(&self) -> bool {
        let host = &self.system_state_host;
        host.pending_apply.is_some()
            || host.operation == Some(SetupOperation::Restore)
            || host.latest_status.queued
            || self.developer_tools_install_active()
    }

    /// An update's handoff is about to end this process: what a Stop still
    /// owes the records gets the same bounded wait a quit gives it
    /// ([`InitialSetupHandlers::settle`]); with nothing owed it returns at
    /// once. The handoff asks as it starts, at the Commit's claim and right
    /// before its write, since the window handles input in between.
    #[cfg(target_os = "macos")]
    pub(crate) fn settle_system_state_before_handoff(&self) {
        aterm_types::macos_setup::set_system_state_front(false);
        if let Some(handlers) = self.system_state_handlers() {
            (handlers.settle)();
        }
    }

    fn finish_system_state_shutdown_with(&mut self, handlers: Option<InitialSetupHandlers>) {
        // Cmd-Q can keep the models and windows until after settlement, but
        // their old frontness no longer authorizes an on-screen worker step.
        aterm_types::macos_setup::set_system_state_front(false);
        if let Some(request) = self.system_state_host.pending.take() {
            request.finish(WorkstationSetupDecision::Unavailable);
        }
        if let Some(handlers) = handlers {
            (handlers.settle)();
        }
    }

    pub(crate) fn receive_system_state_status(
        &mut self,
        generation: Option<u64>,
        status: SetupStatus,
    ) {
        let Some(generation) = generation else {
            self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
            self.system_state_host.status_in_flight = false;
            self.system_state_host.next_status = Some(Instant::now());
            self.system_state_host.next_tick = None;
            self.request_redraw_all_windows();
            return;
        };
        if generation != self.system_state_host.generation {
            return;
        }
        // An admission that resolved meanwhile makes this read stale; its
        // resolution asks for a fresh one.
        self.resolve_system_state_apply();
        if generation != self.system_state_host.generation {
            return;
        }
        // A read that met a write under way says nothing of the saved setup: the window's own
        // admission keeps the last read that did, and so does the page until records that keep
        // changing show as such. The next read comes once the write is over.
        let changing = status.changing;
        if changing && self.system_state_host.changing_reads < CHANGING_READS {
            // Keep the displayed plan and access approvals, but an in-flight
            // external mutation supplies no current run for a deferred command.
            self.system_state_host.latest_status.run = None;
            self.system_state_host.changing_reads += 1;
            self.system_state_host.status_in_flight = false;
            self.system_state_host.next_status = Some(Instant::now() + CHANGING_REREAD);
            return;
        }
        self.system_state_host.changing_reads = 0;
        // This read was asked for after any accepted plan's admission, so it
        // shows that run (or its end).
        self.system_state_host.awaiting_run = false;
        if !status.busy {
            self.system_state_host.operation = None;
        }
        if !changing {
            self.observe_initial_setup_permissions(status.permissions);
        }
        self.publish_system_state(status);
    }

    pub(crate) fn offer_system_state(&mut self, request: Request) {
        let quiet = self.system_state_host.review_shown
            || matches!(request.kind(), OfferKind::Widen { .. });
        if !request.pending()
            || self.headless
            || !self.can_offer_system_state()
            || !(if quiet {
                self.open_system_state_tab_quietly()
            } else {
                self.open_system_state_tab()
            })
        {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        self.attach_system_state_offer(request);
    }

    fn can_offer_system_state(&self) -> bool {
        // The host keeps accepted work and admissions after their final view
        // closes. A queued startup offer must yield before opening a new
        // review, just as it does while a busy controller survives (§3.3).
        self.system_state_host.operation.is_none()
            && !self.system_state_host.awaiting_run
            && !self.system_state_host.latest_status.busy
            && self.system_state_host.pending.is_none()
            && self.system_state_host.pending_apply.is_none()
            && self
                .native_runtime
                .instance_by_kind(AppKind::SystemState)
                .is_none_or(|instance| {
                    matches!(self.native_runtime.app(instance), Some(NativeApp::SystemState(state)) if state.can_offer())
                })
    }

    fn attach_system_state_offer(&mut self, request: Request) {
        if !request.pending() || !self.can_offer_system_state() {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        if !self.native_runtime.offer_system_state(request.clone()) {
            request.finish(WorkstationSetupDecision::Unavailable);
            return;
        }
        // The review of the groups the ALab signal adds holds none of aterm's own permissions:
        // they are in the plan in force, whose run carries them (design §3.5).
        let widen = matches!(request.kind(), OfferKind::Widen { .. });
        self.system_state_host.pending = Some(request);
        self.system_state_host.review_shown = true;
        if !widen {
            self.hold_initial_setup_permissions();
        }
        self.refresh_system_state_tab_indicators();
        self.system_state_host.offer_presented = false;
        self.system_state_host.action_refusal = None;
        self.system_state_host.latest_status = SetupStatus::default();
        let views: Vec<_> = self
            .view_store
            .iter()
            .filter_map(|(view, state)| match state {
                crate::tab_model::View::Native(native)
                    if self
                        .native_runtime
                        .app(native.instance)
                        .is_some_and(|app| app.kind() == AppKind::SystemState) =>
                {
                    Some(view)
                }
                _ => None,
            })
            .collect();
        for view in views {
            if let Some(state) = self.native_runtime.view_state_mut(view) {
                state.common_mut().last_focus = None;
                state.common_mut().focus_visible = false;
            }
        }
        self.system_state_host.next_status = Some(Instant::now());
        self.request_redraw_all_windows();
    }

    /// Project the worker's status for a controller without changing its
    /// cached reading or poll state. The active step is returned separately
    /// because a fixture can keep its own displayed step.
    pub(crate) fn project_system_state_status(
        &self,
        mut status: SetupStatus,
    ) -> (SetupStatus, Option<Access>) {
        if self.closed_launch_is_unanswered(&status) {
            status.unanswered = true;
        }
        if self.system_state_host.operation.is_some()
            || self.system_state_host.awaiting_run
            || self.system_state_host.pending_apply.is_some()
        {
            status.busy = true;
        }
        // A dev scenario keeps the operation it names (a Restore waiting for another setup), as
        // its actions are inert and the window asks for none.
        let named = if self.system_state_host.showing_fixture() {
            status.operation
        } else {
            None
        };
        status.operation = if status.busy && status.running.is_empty() {
            self.system_state_host.operation.or(named)
        } else {
            None
        };
        if let Some(refusal) = &self.system_state_host.action_refusal {
            status.rows.insert(0, refusal.clone());
        }
        let step = self.system_state_access_step();
        if step.is_some() || !self.system_state_host.showing_fixture() {
            status.access_step = step;
        }
        (status, step)
    }

    pub(crate) fn publish_system_state(&mut self, mut status: SetupStatus) {
        if status.cancelled || status.finished || status.run.is_some_and(|run| run.allowed) {
            self.system_state_host.closed_launch_review = false;
        }
        if self.closed_launch_is_unanswered(&status) {
            status.unanswered = true;
        }
        self.system_state_host.latest_status = status.clone();
        let (status, step) = self.project_system_state_status(status);
        // A stage running is a setup pass, whatever the window asked for last:
        // Check again and Restore never run one. A pass that took the worker
        // as a Check ended (or woke by it) is a run, with its Stop and meter.
        if !status.running.is_empty() {
            self.system_state_host.operation = None;
        }
        self.system_state_host.access_step = step;
        self.refresh_system_state_menu_line();
        let changed = self.native_runtime.publish_system_state(status);
        self.system_state_host.status_in_flight = false;
        self.system_state_host.next_status = Some(Instant::now() + Duration::from_secs(1));
        // A Command Line Tools install a pass asked for opens its tab once it is due.
        self.tick_developer_tools_install();
        // A tab indicator that changed redraws its own window (its tab chrome).
        self.refresh_system_state_tab_indicators();
        if changed {
            self.request_redraw_system_state_windows();
        }
    }

    /// The page follows the window's access step at once (design §3.2, §6): when
    /// [`Self::system_state_access_step`] moved since the last publish, publish
    /// the status again so the next frame names macOS's dialog, or stops naming
    /// it. The status poll is left as it was.
    pub(crate) fn republish_system_state_access_step(&mut self) {
        if self.system_state_host.access_step == self.system_state_access_step() {
            return;
        }
        let (in_flight, next) = (
            self.system_state_host.status_in_flight,
            self.system_state_host.next_status,
        );
        self.publish_system_state(self.system_state_host.latest_status.clone());
        self.system_state_host.status_in_flight = in_flight;
        self.system_state_host.next_status = next;
    }

    /// A Messages press that names System State (`Intent::ReviewVaultSync`, on a vault line an
    /// earlier build filed): open or focus the page in `wid`, whose Secrets row is the remedy.
    pub(crate) fn review_vault_sync_in_window(&mut self, wid: crate::WindowId) -> bool {
        self.open_system_state_tab_in_window(wid)
    }

    /// The access step the page was last given ([`SetupStatus::access_step`]).
    #[cfg(test)]
    pub(crate) fn published_system_state_access_step(&self) -> Option<Access> {
        self.system_state_host.access_step
    }

    /// Redraw only the windows that show a System State page: its status and
    /// tick (the village's walk, an offer let go) change only what it draws.
    fn request_redraw_system_state_windows(&self) {
        for wid in self.system_state_showing_windows() {
            if let Some(os_window) = self.windows.get(&wid).and_then(|w| w.os_window.as_ref()) {
                os_window.request_redraw();
            }
        }
    }

    /// The on-screen windows whose active tab's visible plan has a System State
    /// leaf, focused or not. A window where the page is a background tab is not
    /// among them: switching to that tab draws the page as it stands then.
    fn system_state_showing_windows(&self) -> Vec<crate::WindowId> {
        self.system_state_on_screen()
            .filter(|wid| {
                self.active_visible_leaf_plan(*wid).is_some_and(|plan| {
                    plan.leaves.iter().any(|leaf| {
                        matches!(self.view_store.get(leaf.view), Some(crate::tab_model::View::Native(native))
                            if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_))))
                    })
                })
            })
            .collect()
    }

    fn refresh_system_state_tab_indicators(&mut self) -> bool {
        // The menu line says what the diamond says, the review's end or start included.
        self.refresh_system_state_menu_line();
        let mut tabs = Vec::new();
        for (wid, window) in &self.windows {
            for tab in window.tab_set.tabs() {
                if tab.root.any_leaf(&mut |view| {
                    matches!(self.view_store.get(*view), Some(crate::tab_model::View::Native(native))
                        if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_))))
                }) {
                    tabs.push((*wid, tab.id));
                }
            }
        }
        let mut windows = Vec::new();
        for (wid, tab) in tabs {
            if self.refresh_native_tab_indicators(wid, tab) && !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        let changed = !windows.is_empty();
        self.refresh_tab_chrome_windows(windows);
        changed
    }

    /// A click, key, text or scroll reached the page: note whether it is a
    /// person's or a control client's, before the page's reducer acts on it,
    /// so what it submits or opens carries who asked (design §6).
    pub(crate) fn interact_system_state(&mut self, wid: crate::WindowId) {
        if let Some((_, view)) = self.active_native_view(wid) {
            self.interact_system_state_view(wid, view);
        }
    }

    /// Pointer input can reach a visible split without moving keyboard focus.
    /// Record only a System State leaf that this window currently shows.
    pub(crate) fn interact_system_state_view(
        &mut self,
        wid: crate::WindowId,
        view: crate::tab_model::ViewId,
    ) {
        let ours = self
            .active_visible_leaf_plan(wid)
            .is_some_and(|plan| plan.leaf(view).is_some())
            && matches!(
                self.view_store.get(view),
                Some(crate::tab_model::View::Native(native))
                    if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_)))
            );
        if ours {
            let by_person = !self.person_gesture_refused();
            if self.native_runtime.system_state_gesture(by_person) {
                // Stopping changes only the stable-width Apply label. Dirty
                // every copy, retaining its presented target through release.
                let views = self
                    .view_store
                    .iter()
                    .filter_map(|(view, link)| {
                        matches!(link, crate::tab_model::View::Native(native)
                            if matches!(self.native_runtime.app(native.instance), Some(NativeApp::SystemState(_))))
                        .then_some(view)
                    })
                    .collect::<Vec<_>>();
                for view in views {
                    if let Ok((wid, _)) = self.locate_native_view(view) {
                        self.invalidate_native_view_cache(
                            wid,
                            view,
                            crate::native_app::DamageRegion::All,
                        );
                    }
                }
                self.request_redraw_system_state_windows();
            }
            // The permission gate's own evidence of whose gesture this is.
            self.note_system_state_touch();
        }
    }

    pub(crate) fn tick_system_state(&mut self, now: Instant) {
        // The setup worker runs its on-screen steps (the sign-outs) only while
        // System State is in front (design §6); its arrival wakes a parked run.
        self.publish_system_state_front();
        if self.publish_access_opt_outs() {
            // The worker's rows were read with other opt-outs (a `[privacy]`
            // switch, or a debugger turned off that the record read at launch
            // names): a shown page reads them again, so its rows and the menu
            // line agree now.
            self.system_state_host.shown = false;
        }
        self.resolve_withdrawn_system_state_offer(now);
        self.resolve_system_state_apply();
        if self
            .system_state_host
            .next_tick
            .is_some_and(|next| next > now)
        {
            // The status keeps its own clock: one due before the next tick is asked
            // for now, or its deadline would wake the loop every turn until the tick.
            self.poll_system_state_status(now);
            return;
        }
        let instance = self
            .native_runtime
            .instance_by_kind(AppKind::SystemState)
            .filter(|instance| self.system_state_instance_has_view(*instance));
        let Some(instance) = instance else {
            // The review's last view went: closed, not this launch.
            if let Some(request) = self.system_state_host.pending.take() {
                let launch = matches!(request.kind(), OfferKind::Launch) && request.pending();
                request.finish(WorkstationSetupDecision::Closed);
                if launch {
                    self.note_launch_review_closed();
                }
            }
            self.system_state_host.next_tick = None;
            self.system_state_host.shown = false;
            self.system_state_host.observe_retry = None;
            self.poll_system_state_status(now);
            return;
        };
        self.system_state_host.next_tick = Some(now + Duration::from_millis(250));
        // A page opened by hand learns the plan in force from the last status
        // the window read, so it never shows or submits a guess (§3.5).
        if self.system_state_host.latest_status.plan.is_some()
            && matches!(self.native_runtime.app(instance),
                Some(NativeApp::SystemState(state)) if !state.plan_known())
        {
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
        let shown = self.system_state_shown_views(instance);
        // A plan or access approval changed outside the page reaches it in the status.
        // The same plan can have a new approval whose Not now answers differ, so both
        // changes read the rows again, as when the page is shown.
        let host = &mut self.system_state_host;
        if host.shown
            && ((host.latest_status.plan.is_some()
                && host.observed_plan.is_some()
                && host.observed_plan != host.latest_status.plan)
                || (!host.latest_status.changing
                    && host.latest_status.permissions.is_some_and(|policy| {
                        Some(policy.revision) != host.observed_access_revision
                    })))
        {
            host.shown = false;
        }
        // While the GitHub repositories need a person, a page someone sees looks once a minute
        // whether a renewed token reached this Mac; only that look's worker reads, and only
        // that row, and only once the vault's mark moved (§3.6).
        if host.shown
            && !shown.is_empty()
            && host
                .token_looked_at
                .is_some_and(|at| now.saturating_duration_since(at) >= TOKEN_LOOK)
            && host.latest_status.github_needs_person()
        {
            host.token_looked_at = Some(now);
            self.look_for_github_token(instance);
        }
        if shown.is_empty() {
            self.system_state_host.shown = false;
            self.system_state_host.observe_retry = None;
        } else if !self.system_state_host.shown
            && self
                .system_state_host
                .observe_retry
                .is_none_or(|retry| retry <= now)
        {
            // A page waiting for the plan reads at the first tick that knows it; only a
            // refused start waits a second before it is tried again.
            let observe = self.observe_system_state(instance);
            let host = &mut self.system_state_host;
            host.shown = observe == Observe::Started;
            host.observe_retry =
                (observe == Observe::Refused).then_some(now + Duration::from_secs(1));
            if observe == Observe::Started {
                host.token_looked_at = Some(now);
                host.observed_plan = host.latest_status.plan.clone();
                if !host.latest_status.changing
                    && let Some(policy) = host.latest_status.permissions
                {
                    host.observed_access_revision = Some(policy.revision);
                }
            }
        }
        if self.system_state_access_step() != self.system_state_host.access_step {
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
        let active = self.system_state_presented_active(instance);
        // The village walks only on a page someone can see, and only where a
        // view someone sees draws it.
        let seen = self.system_state_seen_views(instance);
        let changed =
            self.native_runtime
                .tick_system_state_at(now, active, !shown.is_empty(), &seen);
        self.drain_system_state_actions();
        if !self.system_state_host.status_in_flight {
            self.system_state_host.next_status.get_or_insert(now);
        }
        self.poll_system_state_status(now);
        self.refresh_system_state_tab_indicators();
        if changed {
            self.request_redraw_system_state_windows();
        }
    }

    /// A dev scenario's first-launch review ([`crate::system_state::fixture_review`]): once a
    /// System State page is open, it is offered the plan a fresh launch offers, as a first
    /// launch's window offers it but in a headless instance too, so a lab capture sees it; a
    /// click then stops its countdown where the scenario says so. Its answer goes nowhere.
    #[cfg(any(debug_assertions, feature = "dev-seams"))]
    fn offer_fixture_review(&mut self) {
        let Some(review) = self.system_state_host.fixture_review else {
            return;
        };
        if self
            .native_runtime
            .instance_by_kind(AppKind::SystemState)
            .is_none()
        {
            return;
        }
        self.system_state_host.fixture_review = None;
        self.attach_system_state_offer(Request::unanswered(Plan::default()));
        if review == crate::system_state::FixtureReview::Stopped {
            self.native_runtime.system_state_gesture(true);
        }
    }

    /// Where setup may not start (a headless instance), the page still runs
    /// while the dev seam shows it a scenario, so a lab capture sees it.
    pub(crate) fn tick_system_state_fixture(&mut self, now: Instant) {
        if self.system_state_host.fixture().is_some() {
            self.tick_system_state(now);
        }
    }

    fn resolve_withdrawn_system_state_offer(&mut self, now: Instant) {
        if !self
            .system_state_host
            .pending
            .as_ref()
            .is_some_and(|request| !request.pending())
        {
            return;
        }
        self.system_state_host.pending = None;
        self.recheck_initial_setup_permissions();
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
        self.system_state_host.next_status = Some(now);
        self.system_state_host.next_tick = None;
    }

    fn closed_launch_is_unanswered(&self, status: &SetupStatus) -> bool {
        self.system_state_host.closed_launch_review
            && !status.cancelled
            && !status.finished
            && status.run.is_none_or(|run| !run.allowed)
    }

    fn note_launch_review_closed(&mut self) {
        self.system_state_host.closed_launch_review = true;
        let unanswered = self.closed_launch_is_unanswered(&self.system_state_host.latest_status);
        self.system_state_host.latest_status.unanswered |= unanswered;
        self.refresh_system_state_tab_indicators();
    }

    /// Publish visibility before another event or worker step can use it.
    /// Closing a passive copy keeps the visible page's authority; losing the
    /// last front page withdraws it even while a hidden controller survives.
    pub(crate) fn publish_system_state_front(&mut self) {
        let front = self.system_state_frontmost();
        if !front {
            self.native_runtime.pause_system_state_review();
        }
        aterm_types::macos_setup::set_system_state_front(front);
    }

    /// Whether a System State view is the visible, active view of a focused,
    /// on-screen window with no overlay: the only time a setup run's access
    /// steps, the sign-outs and the debugger step may put a macOS gesture on
    /// screen (design §6). A focused window macOS reports occluded (the screen
    /// locked or asleep, a screen saver up) stays key but shows nothing.
    pub(crate) fn system_state_frontmost(&self) -> bool {
        self.windows
            .keys()
            .any(|wid| self.system_state_frontmost_in(*wid))
    }

    /// Revalidate the particular window a deferred setup gesture was booked in.
    pub(crate) fn system_state_frontmost_in(&self, wid: crate::WindowId) -> bool {
        self.windows.get(&wid).is_some_and(|window| {
            crate::os_window_focused(window)
                && !window.occluded
                && window.overlay.is_none()
                && self
                    .active_native_view(wid)
                    .is_some_and(|(instance, view)| {
                        matches!(
                            self.native_runtime.app(instance),
                            Some(crate::native_app::NativeApp::SystemState(_))
                        ) && self
                            .active_visible_leaf_plan(wid)
                            .is_some_and(|plan| plan.leaf(view).is_some())
                    })
        })
    }

    /// The windows someone can see: a minimized, hidden or fully covered
    /// window (occluded) shows nothing, as Settings' `settings_view_on_screen`
    /// reads it.
    fn system_state_on_screen(&self) -> impl Iterator<Item = crate::WindowId> + '_ {
        self.windows
            .iter()
            .filter(|(_, window)| !window.occluded)
            .map(|(wid, _)| *wid)
    }

    /// The views of `instance` that are a leaf of some on-screen window's active
    /// tab's visible plan, focused or not: those in the windows
    /// [`Self::system_state_showing_windows`] redraws.
    fn system_state_seen_views(
        &self,
        instance: crate::native_app::AppInstanceId,
    ) -> Vec<crate::tab_model::ViewId> {
        self.system_state_on_screen()
            .filter_map(|wid| self.active_visible_leaf_plan(wid))
            .flat_map(|plan| plan.leaves.into_iter().map(|leaf| leaf.view))
            .filter(|view| {
                matches!(self.view_store.get(*view),
                    Some(crate::tab_model::View::Native(native)) if native.instance == instance)
            })
            .collect()
    }

    /// The views of `instance` that are the visible, active view of some
    /// on-screen window, focused or not.
    fn system_state_shown_views(
        &self,
        instance: crate::native_app::AppInstanceId,
    ) -> Vec<crate::tab_model::ViewId> {
        self.system_state_on_screen()
            .filter_map(|wid| {
                self.active_native_view(wid).and_then(|(current, view)| {
                    (current == instance
                        && self
                            .active_visible_leaf_plan(wid)
                            .is_some_and(|plan| plan.leaf(view).is_some()))
                    .then_some(view)
                })
            })
            .collect()
    }

    /// A person returning from System Settings may have changed this Mac
    /// while the page stayed visible. Its accepted reading is stale then;
    /// the next tick reads again, keeping a refused start's retry deadline.
    pub(crate) fn system_state_focus_returned(&mut self, wid: crate::WindowId) {
        let visible = self
            .windows
            .get(&wid)
            .is_some_and(|window| !window.occluded && window.overlay.is_none())
            && self
                .active_native_view(wid)
                .is_some_and(|(instance, view)| {
                    matches!(
                        self.native_runtime.app(instance),
                        Some(NativeApp::SystemState(_))
                    ) && self
                        .active_visible_leaf_plan(wid)
                        .is_some_and(|plan| plan.leaf(view).is_some())
                });
        if visible {
            self.system_state_host.shown = false;
            self.system_state_host.next_tick = Some(Instant::now());
        }
    }

    /// Whether the page has been presented in a focused window and is still
    /// in front there: only then can the first-launch countdown advance.
    fn system_state_presented_active(
        &mut self,
        instance: crate::native_app::AppInstanceId,
    ) -> bool {
        if !self.system_state_frontmost() {
            return false;
        }
        let active = self.system_state_host.offer_presented
            || self.windows.iter().any(|(wid, window)| {
                crate::os_window_focused(window)
                    && window.overlay.is_none()
                    && self
                        .active_native_view(*wid)
                        .is_some_and(|(current, view)| {
                            current == instance
                                && self.active_visible_leaf_plan(*wid).is_some_and(|plan| {
                                    plan.leaf(view).is_some()
                                        && self
                                            .retained_native_leaf_artifact_from_plan(
                                                *wid, view, true, &plan,
                                            )
                                            .is_some()
                                })
                        })
            });
        if active {
            self.system_state_host.offer_presented = true;
        }
        active
    }

    /// Ask the worker for a fresh, prompt-free reading of every row for the
    /// plan in force (§3.5) when the page is shown: its managed rows decide
    /// which network reads run, so a page that does not know the plan yet
    /// waits for it. `false` while it waits.
    fn observe_system_state(&mut self, instance: crate::native_app::AppInstanceId) -> Observe {
        let handlers = self.system_state_handlers();
        self.observe_system_state_with(instance, handlers)
    }

    fn observe_system_state_with(
        &mut self,
        instance: crate::native_app::AppInstanceId,
        handlers: Option<InitialSetupHandlers>,
    ) -> Observe {
        if self.system_state_host.fixture().is_some() {
            return Observe::Started;
        }
        let Some(handlers) = handlers else {
            return Observe::Started;
        };
        let plan = match self.native_runtime.app(instance) {
            Some(NativeApp::SystemState(_)) => match self.system_state_observation_plan(instance) {
                Some(plan) => plan,
                // A newer aterm's setup names no plan this aterm reads, now or later; reading
                // writes nothing, so its rows are read as they stand with nothing set up: no
                // account and no network read but on Check again (§3.2, §3.5).
                None if self.system_state_host.latest_status.newer() => {
                    Plan::from(Selection::none())
                }
                None => return Observe::NoPlan,
            },
            _ => return Observe::Started,
        };
        if (handlers.observe)(plan) {
            Observe::Started
        } else {
            aterm_log::info!("system state: the observation did not start");
            Observe::Refused
        }
    }

    /// The plan the shown page's rows are read for: the page's own, once it knows the plan in
    /// force, else that plan from the last status. `None` while neither knows it.
    fn system_state_observation_plan(
        &self,
        instance: crate::native_app::AppInstanceId,
    ) -> Option<Plan> {
        match self.native_runtime.app(instance) {
            Some(NativeApp::SystemState(state)) if state.plan_known() => {
                Some(state.observation_plan())
            }
            Some(NativeApp::SystemState(_)) => self.system_state_host.latest_status.plan.clone(),
            _ => None,
        }
    }

    /// One look for a renewed GitHub token while the GitHub repositories need a person
    /// ([`TOKEN_LOOK`]): the worker reads that row again, alone, only if the vault's mark moved.
    fn look_for_github_token(&mut self, instance: crate::native_app::AppInstanceId) {
        if self.system_state_host.fixture().is_some() {
            return;
        }
        let Some(handlers) = self.system_state_handlers() else {
            return;
        };
        if let Some(plan) = self.system_state_observation_plan(instance)
            && !(handlers.look_for_token)(plan)
        {
            aterm_log::info!("system state: the look for a renewed GitHub token did not start");
        }
    }

    fn poll_system_state_status(&mut self, now: Instant) {
        if !self.system_state_host.status_in_flight
            && self
                .system_state_host
                .next_status
                .is_some_and(|next| next <= now)
        {
            if let Some(status) = self.system_state_host.fixture().cloned() {
                self.system_state_host.next_status = None;
                #[cfg(any(debug_assertions, feature = "dev-seams"))]
                self.offer_fixture_review();
                self.publish_system_state(status);
                return;
            }
            let Some(handlers) = self.system_state_handlers() else {
                self.system_state_host.next_status = None;
                return;
            };
            self.system_state_host.status_in_flight = true;
            self.system_state_host.next_status = None;
            let generation = self.system_state_host.generation;
            let started = std::thread::Builder::new()
                .name("aterm-setup-status".into())
                .spawn(move || {
                    crate::workstation_setup::send_setup_wake(Wake::InitialSetupStatus {
                        generation: Some(generation),
                        status: (handlers.status)(),
                    });
                });
            if started.is_err() {
                self.system_state_host.status_in_flight = false;
                self.system_state_host.next_status = Some(now + Duration::from_secs(1));
            }
        }
    }

    /// Hand off a page's intents before another input can dispose of it. The
    /// same path serves automatic admission from the periodic tick.
    pub(crate) fn drain_system_state_actions(&mut self) {
        while let Some(action) = self.native_runtime.take_system_state_action() {
            self.run_system_state_action(action);
        }
    }

    fn run_system_state_action(&mut self, action: SystemStateAction) {
        if self.system_state_host.fixture().is_some() {
            aterm_log::info!("system state: {action:?} is inert under {FIXTURE_SEAM}");
            return;
        }
        let handlers = self.system_state_handlers();
        self.run_system_state_action_with_handlers(action, handlers);
    }

    /// The final view has gone: an offer it still held is let go; closing never cancels setup.
    pub(crate) fn finish_system_state_close(
        &mut self,
        instance: crate::native_app::AppInstanceId,
        scope: crate::native_app::CloseScope,
    ) {
        if matches!(
            self.native_runtime.app(instance),
            Some(NativeApp::SystemState(_))
        ) {
            let launch = self
                .system_state_host
                .pending
                .as_ref()
                .is_some_and(|request| {
                    matches!(request.kind(), OfferKind::Launch) && request.pending()
                });
            self.system_state_host.shown = false;
            self.system_state_host.observe_retry = None;
            self.native_runtime
                .finish_system_state_close(instance, scope);
            // Retire the ended offer before a reopened page can submit a new
            // intent; that intent belongs to its manual handler, not this request.
            self.resolve_withdrawn_system_state_offer(Instant::now());
            if launch {
                self.note_launch_review_closed();
            }
        }
    }

    fn run_system_state_action_with_handlers(
        &mut self,
        action: SystemStateAction,
        handlers: Option<InitialSetupHandlers>,
    ) {
        match action {
            SystemStateAction::CancelReview => {
                let _ = self.take_system_state_press();
                let mut widen = false;
                if let Some(request) = self.system_state_host.pending.take() {
                    widen = matches!(request.kind(), OfferKind::Widen { .. });
                    request.finish(WorkstationSetupDecision::Cancelled);
                }
                self.begin_system_state_action(false, false);
                // The launch review's hold goes with it: a fresh read of the records, which
                // keep the Cancel, decides. The widen's review held nothing.
                if !widen {
                    self.recheck_initial_setup_permissions();
                }
                self.finish_system_state_action(true, "Setup");
                self.refresh_system_state_tab_indicators();
            }
            // Stop changes no plan or status generation. It withdraws future
            // automatic gestures locally even when the durable run already
            // finished, and the worker keeps its ordinary durable Stop.
            SystemStateAction::Stop => {
                let _ = self.take_system_state_press();
                self.stop_initial_setup_permissions();
                if let Some(pending) = &mut self.system_state_host.pending_apply {
                    pending.release_permissions = false;
                }
                if let Some(handlers) = handlers {
                    (handlers.stop)();
                }
            }
            SystemStateAction::OpenAccess { access, by_person } => {
                let press = self.take_system_state_press();
                let by_person = by_person && press == crate::workstation_setup::PagePress::Person;
                if !self.open_access_from_system_state(access, by_person) {
                    aterm_log::info!(
                        "system state: {} did not open (by a person: {by_person})",
                        access.name()
                    );
                }
            }
            SystemStateAction::TurnSyncOn { by_person } => {
                let press = self.take_system_state_press();
                if !by_person || press != crate::workstation_setup::PagePress::Person {
                    aterm_log::info!("system state: Turn On Sync refused: not a person's press");
                    return;
                }
                if !handlers.is_some_and(|handlers| (handlers.sync_on)()) {
                    aterm_log::info!("system state: Turn On Sync did not start");
                }
            }
            SystemStateAction::Apply(mut answer, admission) => {
                let WorkstationSetupDecision::Begin(plan) = &answer.decision else {
                    return;
                };
                let plan = plan.clone();
                let Some(instance) = self.native_runtime.instance_by_kind(AppKind::SystemState)
                else {
                    return;
                };
                // A run going now, or a plan still being admitted, stops for
                // this one at its next step (§3.3): the worker does that as it
                // takes the plan, so the window never sends a Stop that could
                // reach this plan's own run.
                self.begin_system_state_action(true, true);
                self.note_system_state_apply(admission);
                self.system_state_host.operation = None;
                aterm_log::info!("system state: submitting a plan ({admission:?})");
                let password = answer.password.as_ref().and_then(|_| {
                    match self.native_runtime.app(instance) {
                        Some(NativeApp::SystemState(state)) => {
                            state.pending_password_acceptance(&plan)
                        }
                        _ => None,
                    }
                });
                let (receipt, receiver) = InitialSetupApplyReceipt::new(
                    admission,
                    answer.chosen,
                    answer.changed,
                    password,
                );
                self.system_state_host.pending_apply = Some(PendingApply {
                    instance,
                    admission,
                    plan: plan.clone(),
                    answer: receiver,
                    release_permissions: true,
                });
                let started = if let Some(request) = self.system_state_host.pending.take() {
                    answer.receipt = Some(receipt);
                    request.finish_answer(answer)
                } else if let Some(handlers) = handlers {
                    (handlers.apply)(plan, answer.vault_typed, answer.password, receipt)
                } else {
                    false
                };
                if started {
                    // Sending a startup answer and spawning a manual worker
                    // both await the same durable admission before consuming
                    // the review.
                    self.publish_system_state(self.system_state_host.latest_status.clone());
                } else {
                    let pending = self.system_state_host.pending_apply.take().unwrap();
                    self.refuse_system_state_apply(pending);
                }
                self.finish_system_state_action(started, "Setup");
            }
            SystemStateAction::Check(plan) => {
                let _ = self.take_system_state_press();
                // A read-only Check cannot revoke its durable permission decision.
                self.begin_system_state_action(false, false);
                let started = handlers.map_or_else(CheckStarted::default, |handlers| {
                    let started = (handlers.check)(plan.clone());
                    // Check again is a check and a forced observation (§3.4).
                    if !(handlers.observe)(plan) {
                        aterm_log::info!("system state: the observation did not start");
                    }
                    started
                });
                // Only its own check is labelled one: a pass that held the
                // worker is a run, with its Stop and meter (§3.2). Its forced
                // reading ran all the same, so Check again did start. A Restore
                // waiting for another setup holds the worker, and stays the
                // Restore it is (§3.4).
                self.system_state_host.operation = if started.check {
                    Some(SetupOperation::Check)
                } else {
                    self.system_state_host
                        .operation
                        .filter(|operation| *operation == SetupOperation::Restore)
                };
                self.finish_system_state_action(started.reading || started.check, "Check again");
            }
            SystemStateAction::Restore => {
                let _ = self.take_system_state_press();
                // A Restore puts back Mac preferences and never withdraws aterm's own
                // permissions (§3.4): the access steps go on as they were.
                self.begin_system_state_action(true, false);
                if let Some(request) = self.system_state_host.pending.take() {
                    request.finish(WorkstationSetupDecision::Unavailable);
                    // The offer's hold goes with it: a fresh read of the records decides.
                    self.recheck_initial_setup_permissions();
                    self.native_runtime.tick_system_state(false, false, &[]);
                    self.refresh_system_state_tab_indicators();
                }
                let started = handlers.is_some_and(|handlers| (handlers.restore)());
                self.system_state_host.operation = started.then_some(SetupOperation::Restore);
                self.finish_system_state_action(started, "Restore previous settings");
            }
        }
    }

    /// What every Apply, Check and Restore does first: a `mutating` one retires the review and
    /// forgets any pending Apply, and one that `holds_access` (an Apply, whose plan may change
    /// the Permissions group) holds the permission gate until its plan's fresh status; each
    /// clears a refused action's note and starts a new status generation.
    fn begin_system_state_action(&mut self, mutating: bool, holds_access: bool) {
        if mutating {
            if let Some(instance) = self.native_runtime.instance_by_kind(AppKind::SystemState) {
                self.retire_native_review(instance);
            }
            self.system_state_host.pending_apply = None;
        }
        if holds_access {
            self.hold_initial_setup_permissions();
        }
        self.system_state_host.action_refusal = None;
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
    }

    fn finish_system_state_action(&mut self, started: bool, action_name: &str) {
        if !started {
            self.system_state_host.action_refusal = Some(SetupStatusRow {
                name: action_name.into(),
                status: "not started".into(),
                detail: "Try again.".into(),
            });
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
        self.system_state_host.next_status = Some(Instant::now());
        self.request_redraw_all_windows();
    }

    /// A failed automatic offer releases its temporary hold after a fresh
    /// status. A local choice or Stop keeps its hold.
    fn refuse_system_state_apply(&mut self, pending: PendingApply) {
        if pending.admission == Admission::Automatic && pending.release_permissions {
            self.recheck_initial_setup_permissions();
        }
        self.native_runtime.refuse_system_state_apply();
    }

    pub(crate) fn resolve_system_state_apply(&mut self) {
        // A superseded Join can still be accepted. Consume only its exact
        // password, never its old plan or permission decision.
        if self.native_runtime.forget_accepted_system_state_password() {
            self.request_redraw_system_state_windows();
        }
        let Some(pending) = self.system_state_host.pending_apply.as_ref() else {
            return;
        };
        let accepted = match pending.answer.try_recv() {
            Ok(accepted) => accepted,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => false,
        };
        let pending = self.system_state_host.pending_apply.take().unwrap();
        self.system_state_host.generation = self.system_state_host.generation.wrapping_add(1);
        self.system_state_host.status_in_flight = false;
        if accepted {
            self.system_state_host.awaiting_run = true;
            // The worker holds this plan now. The last read was taken before
            // it and names the old one, which must not undo the choice when
            // it is shown again below.
            self.system_state_host.latest_status.plan = Some(pending.plan.clone());
            if pending.release_permissions {
                self.accepted_initial_setup_apply();
            }
            if self.system_state_instance_has_view(pending.instance) {
                self.native_runtime.accept_system_state_apply(pending.plan);
                self.refresh_system_state_tab_indicators();
            }
        } else {
            self.refuse_system_state_apply(pending);
        }
        // A plan the worker took and then let go says why in the worker's own rows (a step
        // that failed, in the page's words), or says nothing, overtaken by a later choice: the
        // host adds no row of its own beside them, so one press is said once.
        self.finish_system_state_action(true, "Setup");
        if accepted {
            self.publish_system_state(self.system_state_host.latest_status.clone());
        }
    }

    fn system_state_instance_has_view(&self, instance: crate::native_app::AppInstanceId) -> bool {
        self.native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == AppKind::SystemState)
            && self.view_store.iter().any(|(_, view)| {
                matches!(view, crate::tab_model::View::Native(native) if native.instance == instance)
            })
    }
}

#[cfg(test)]
mod tests;
