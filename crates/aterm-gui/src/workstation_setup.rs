// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-launch Mac setup's offer, which System State answers once it is on screen.

use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::{Duration, Instant};

use aterm_types::macos_setup::{Plan, Selection};
use aterm_window::event_loop::EventLoopProxy;

use crate::{App, Wake};

/// System State's answer to the first-launch offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkstationSetupDecision {
    /// Begin with this plan: the page applied the offer once it was presented
    /// in an active window, or a person changed a group first.
    Begin(Plan),
    /// This process has no live window, or its window closed before a decision.
    Unavailable,
}

/// The page's decision and the optional, ephemeral vault password.
#[derive(Debug)]
pub struct SetupAnswer {
    pub decision: WorkstationSetupDecision,
    pub password: Option<aterm_secrets::SecretBytes>,
    pub receipt: Option<crate::InitialSetupApplyReceipt>,
    /// The plan is a group choice (or a Join) made on the page, not the plan as it stood (the first-run offer applied, or Set up now): it is the chooser's, never the provisional plan aterm chose by itself (§3.5).
    pub chosen: bool,
    /// A Join: the plan names the vault the person typed, which moves the Mac.
    pub vault_typed: bool,
}

impl From<WorkstationSetupDecision> for SetupAnswer {
    fn from(decision: WorkstationSetupDecision) -> Self {
        Self {
            decision,
            password: None,
            receipt: None,
            chosen: false,
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

/// Wait on a worker thread for System State's answer to the offer of `initial`.
/// A delivered Begin calls `on_begin` before the window can handle another input,
/// so a Stop and quit know its admission is under way even before this wait returns.
pub fn await_initial_setup_answer_while(
    initial: Plan,
    finished: impl FnMut() -> bool,
    on_begin: impl FnOnce() + Send + 'static,
) -> SetupAnswer {
    let proxy = PROXY.lock().ok().and_then(|proxy| proxy.clone());
    let Some(proxy) = proxy else {
        return WorkstationSetupDecision::Unavailable.into();
    };
    let (send, receive) = mpsc::channel();
    let request = Request::with_handoff(initial, send, on_begin);
    let pending = Arc::downgrade(&request.reply);
    if proxy.send_event(Wake::WorkstationSetup(request)).is_err() {
        return WorkstationSetupDecision::Unavailable.into();
    }
    wait_for_decision(pending, receive, finished, Duration::from_secs(1))
}

/// Drive the real first-launch answer channel without an event loop or native window.
#[cfg(any(test, debug_assertions))]
pub fn drive_initial_setup_answer_for_test(
    initial: Plan,
    on_begin: impl FnOnce() + Send + 'static,
    drive: impl FnOnce(Box<dyn FnOnce(SetupAnswer) -> bool + Send>),
) -> SetupAnswer {
    let (send, receive) = mpsc::channel();
    let request = Request::with_handoff(initial, send, on_begin);
    let pending = Arc::downgrade(&request.reply);
    drive(Box::new(move |answer| request.finish_answer(answer)));
    wait_for_decision(pending, receive, || false, Duration::from_secs(60))
}

type Reply = Mutex<Option<mpsc::Sender<SetupAnswer>>>;
type BeginHandoff = Box<dyn FnOnce() + Send>;

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

#[derive(Clone)]
pub(crate) struct Request {
    initial: Plan,
    reply: Arc<Reply>,
    on_begin: Arc<Mutex<Option<BeginHandoff>>>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("initial", &self.initial)
            .finish_non_exhaustive()
    }
}

impl Request {
    #[cfg(test)]
    pub(crate) fn new(initial: impl Into<Plan>, reply: mpsc::Sender<SetupAnswer>) -> Self {
        Self::with_handoff(initial.into(), reply, || {})
    }

    fn with_handoff(
        initial: Plan,
        reply: mpsc::Sender<SetupAnswer>,
        on_begin: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            initial,
            reply: Arc::new(Mutex::new(Some(reply))),
            on_begin: Arc::new(Mutex::new(Some(Box::new(on_begin)))),
        }
    }
    /// The page's answer, a closing window and a withdrawn offer compete for this single decision.
    pub(crate) fn finish(&self, decision: WorkstationSetupDecision) {
        self.finish_answer(decision.into());
    }

    pub(crate) fn finish_answer(&self, answer: SetupAnswer) -> bool {
        let begin = matches!(&answer.decision, WorkstationSetupDecision::Begin(_));
        if !finish_reply(&self.reply, answer) {
            return false;
        }
        // The GUI publishes busy and can accept Stop only after this returns.
        // A dead receiver owns no admission; an unanswered or withdrawn offer
        // must never make a quit wait for an answer nobody will give.
        if begin {
            let on_begin = self
                .on_begin
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(on_begin) = on_begin {
                on_begin();
            }
        }
        true
    }

    pub(crate) const fn initial(&self) -> &Plan {
        &self.initial
    }

    pub(crate) fn pending(&self) -> bool {
        self.reply.lock().is_ok_and(|reply| reply.is_some())
    }
}

/// Who the window's own admission of a setup plan came from
/// (`docs/DESIGN-system-state-2026-10-21.md` §6, "The standing yes").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmittedBy {
    /// The page's own automatic answer to the startup offer: the first
    /// launch's admission, with nobody's gesture in it.
    Window,
    /// A person's press on the page: a person touched it and no control
    /// client's gesture came since the page's previous action.
    Person,
}

/// THE WINDOW'S OWN ADMISSION: the gate that reads who touched System State
/// and which revision the window's own Apply admitted ([`PermissionGate`]), and
/// the token it alone mints ([`AdmittedAccess`]), live in a leaf module of
/// their own, whose private fields nothing else in the crate can reach: not
/// this module, nor its tests (the aterm-census guard `devtools-door` holds the
/// leaf to that shape).
#[path = "workstation_setup_gate.rs"]
mod permission_gate;
pub(crate) use permission_gate::AdmittedAccess;
use permission_gate::PermissionGate;

/// Whose gestures reached System State since its previous action was taken
/// ([`PermissionGate::take_press`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PagePress {
    /// A control client's gesture came (anywhere): never a person's press.
    Controller,
    /// A person touched the page, and no control client's gesture came.
    Person,
    /// Nobody touched it: the page acted by itself.
    Nobody,
}

/// What a real gate gives the debugger step's Tier-1 rig
/// (`debugger_access::conformance`, the model's `Admitted`): the gate takes an
/// Apply — the page's own answer to the startup offer (`window`), or a control
/// client's — and the fresh status naming revision 1 with its Permissions group
/// admitted, and answers the standing yes it then holds.
#[cfg(test)]
pub(crate) fn gate_admission_for_test(window: bool) -> Option<AdmittedAccess> {
    let mut gate = PermissionGate::default();
    gate.hold();
    let press = if window {
        PagePress::Nobody
    } else {
        PagePress::Controller
    };
    gate.apply_started(true, press);
    gate.accepted();
    gate.observe(Some(crate::InitialSetupPermissionPolicy {
        revision: 1,
        allowed: true,
    }));
    gate.admitted(false)
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
    pub(crate) fn suppresses_permissions(&self) -> bool {
        self.unified && !self.permissions.allowed()
    }

    /// The approved setup revision whose access steps may run
    /// (`system_state_access`): the admitted plan's revision while the gate
    /// allows Permissions, else none.
    pub(crate) fn access_revision(&self) -> Option<u64> {
        self.permissions.access_revision()
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

const fn preview_mode(requested: bool, headless: bool, private: bool) -> PreviewMode {
    if !requested {
        PreviewMode::Ordinary
    } else if !headless && private {
        PreviewMode::Preview
    } else {
        PreviewMode::Refused
    }
}

/// Exercise the real offer with an inert worker.
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
            let mut answer = await_initial_setup_answer_while(initial.into(), || false, || {});
            if matches!(answer.decision, WorkstationSetupDecision::Begin(_))
                && let Some(receipt) = answer.receipt.take()
            {
                receipt.accept();
            }
            if let Some(root) = aterm_types::dirs::state_dir() {
                let word = match answer.decision {
                    WorkstationSetupDecision::Begin(plan) => {
                        format!("begin\nselection_bits={}\n", plan.selection.bits())
                    }
                    WorkstationSetupDecision::Unavailable => "unavailable\n".to_owned(),
                };
                let result = format!("{word}elapsed_ms={}\n", started.elapsed().as_millis());
                if let Err(error) =
                    std::fs::write(root.join("workstation-setup-preview-result"), result)
                {
                    aterm_log::warn!("Mac setup preview could not write its result: {error}");
                }
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

/// First paint and Commit are independent requirements.
const fn may_start(headless: bool, painted: bool, incoming_pending: bool) -> bool {
    !headless && painted && !incoming_pending
}

/// A carried command is inert in a validated running-window handoff.
pub(crate) const fn eligible_launch_command(
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
        if self.workstation_setup.permissions.observe(policy)
            && let Some(policy) = policy
        {
            // The window's own admission: the debugger's step, parked for want
            // of one, or over for an earlier revision, looks again (design §6:
            // a new approval asks again; once per Mac, so a Mac that answered
            // has the new revision's row settled with no second dialog). While
            // a change System State started is under way, macOS's dialog
            // perhaps unanswered, the new revision joins that change instead:
            // handed now, its outcome when it ends.
            if self.debugger_launch.started().is_some() && self.debugger_change.under_way() {
                self.debugger_launch.join_started(policy.revision);
            } else {
                self.debugger_launch.unpark(policy.revision);
            }
        }
    }

    /// A gesture reached the System State page (before its reducer runs). A
    /// control client's is never a person's press; the App's count of those
    /// ([`crate::app_take_over::AgentsState::controller_gestures`]) is read
    /// when the page's action is taken, so a path into the page that skips
    /// this call still cannot pass for a person.
    pub(crate) fn note_system_state_touch(&mut self) {
        let controller = self.person_gesture_refused();
        self.workstation_setup.permissions.note_touch(controller);
    }

    /// Whose gestures led to the page's action being handled now (design §6):
    /// a person's press only when a person touched the page and no control
    /// client's gesture came since the page's previous action. Call it once
    /// per action the page queued, as that action is taken: a row's
    /// `Turn On…` passes `== PagePress::Person` as its `by_person`. A key an
    /// `hwkey` burst posted counts as a control client's gesture too
    /// (`hwkey::posted_keys`): its real `NSEvent` carries no gesture mark, and
    /// one already queued as the page came to the front reaches it before the
    /// burst stops.
    pub(crate) fn take_system_state_press(&mut self) -> PagePress {
        let controller_gestures = self
            .agents
            .controller_gestures
            .wrapping_add(crate::hwkey::posted_keys());
        self.workstation_setup
            .permissions
            .take_press(controller_gestures)
    }

    /// The page's Apply is being handled, with the page's own word on whose
    /// it is (`admission`). Its acceptance becomes the window's own admission
    /// only for a person's press that the gate's evidence agrees with (no
    /// control client's gesture since the page's previous action), or the
    /// page's own answer to the startup offer, which no gesture makes.
    pub(crate) fn note_system_state_apply(&mut self, admission: crate::system_state::Admission) {
        use crate::system_state::Admission;
        let page_touched_by_controller = self.workstation_setup.permissions.controller_touched();
        let press = self.take_system_state_press();
        let (answers_offer, press) = match admission {
            // The page answers the offer by itself on its first presented
            // tick, so a control client's gesture elsewhere in the app does not
            // taint it; one that reached the page itself does.
            Admission::Automatic if page_touched_by_controller => (true, PagePress::Controller),
            Admission::Automatic => (true, PagePress::Nobody),
            Admission::Person if press == PagePress::Person => (false, PagePress::Person),
            Admission::Person | Admission::Client => (false, PagePress::Controller),
        };
        self.workstation_setup
            .permissions
            .apply_started(answers_offer, press);
    }

    /// THE STANDING YES for an access step the window starts by itself: the
    /// plan with its Permissions group admitted, while that plan is the
    /// window's own admission (design §6). `None` otherwise.
    pub(crate) fn admitted_access(&self) -> Option<AdmittedAccess> {
        self.workstation_setup.permissions.admitted(false)
    }

    /// The yes for a person's press on a Permissions row's own button
    /// (`Turn On…`), while a plan with its Permissions group is admitted.
    /// `by_person` false — a control client's `act` or `key` — is never one.
    pub(crate) fn admitted_by_press(&self, by_person: bool) -> Option<AdmittedAccess> {
        if !by_person {
            return None;
        }
        self.workstation_setup.permissions.admitted(true)
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
            // No setup here; a dev scenario's page still runs (lab captures).
            self.tick_system_state_fixture(now);
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
            "an old durable plan cannot undo a pending Apply"
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

    fn policy(revision: u64, allowed: bool) -> Option<crate::InitialSetupPermissionPolicy> {
        Some(crate::InitialSetupPermissionPolicy { revision, allowed })
    }

    /// One Apply through the gate as the host runs it: the hold, who it came
    /// from, the receipt, and the fresh status naming `revision`. Returns
    /// whether that status established the window's own admission.
    fn apply(gate: &mut PermissionGate, offer: bool, press: PagePress, revision: u64) -> bool {
        gate.hold();
        gate.apply_started(offer, press);
        gate.accepted();
        gate.observe(policy(revision, true))
    }

    /// THE STANDING YES (design §6) is the window's own admission only: the
    /// page's automatic answer to the startup offer, with nobody's gesture in
    /// it, or a person's press on the page — and only for the revision that
    /// admission named, while that plan stands.
    #[test]
    fn the_standing_yes_is_the_windows_own_admission_of_the_plan_in_force() {
        let mut gate = PermissionGate::default();
        assert!(apply(&mut gate, true, PagePress::Nobody, 3));
        let yes = gate
            .admitted(false)
            .expect("the window's first automatic one");
        assert_eq!((yes.revision(), yes.by()), (3, AdmittedBy::Window));

        let mut gate = PermissionGate::default();
        assert!(apply(&mut gate, false, PagePress::Person, 4));
        let yes = gate.admitted(false).expect("a person's press on the page");
        assert_eq!((yes.revision(), yes.by()), (4, AdmittedBy::Person));

        // A newer status naming another plan, a held intent, a plan withdrawn
        // or an unreadable status each end it.
        gate.observe(policy(5, true));
        assert!(
            gate.allowed() && gate.admitted(false).is_none(),
            "another plan"
        );
        assert!(apply(&mut gate, true, PagePress::Nobody, 6));
        gate.hold();
        assert!(gate.admitted(false).is_none(), "a newer intent held");
        gate.accepted();
        gate.observe(policy(6, true));
        assert!(gate.admitted(false).is_none(), "an Apply of nobody's own");
        assert!(apply(&mut gate, true, PagePress::Nobody, 7));
        gate.observe(policy(7, false));
        assert!(gate.admitted(false).is_none(), "Permissions not admitted");
        assert!(apply(&mut gate, true, PagePress::Nobody, 8));
        gate.observe(None);
        assert!(gate.admitted(false).is_none(), "an unreadable status");
    }

    /// NEVER FROM A CONTROL CLIENT, A STATUS POLL OR A HAND-EDITED RECORD: a
    /// plan admitted by a control client's Apply, or one a status names that no
    /// Apply of this window admitted (a poll, a record edited by hand), allows
    /// the page's guidance but is no standing yes. A person's press on a row's
    /// own button is one while the plan stands; a control client's is never one
    /// (`App::admitted_by_press`).
    #[test]
    fn no_control_client_status_poll_or_record_mints_the_standing_yes() {
        let mut gate = PermissionGate::default();
        assert!(!apply(&mut gate, true, PagePress::Controller, 2));
        assert!(gate.allowed(), "the plan is admitted");
        assert!(gate.admitted(false).is_none(), "a control client's Apply");

        let mut gate = PermissionGate::default();
        assert!(!gate.observe(policy(2, true)), "a status poll");
        assert!(gate.allowed() && gate.admitted(false).is_none());
        let pressed = gate
            .admitted(true)
            .expect("a person's press while it stands");
        assert_eq!((pressed.revision(), pressed.by()), (2, AdmittedBy::Person));
        gate.observe(policy(3, false));
        assert!(
            gate.admitted(true).is_none(),
            "not while Permissions is out"
        );

        let mut gate = PermissionGate::default();
        assert!(!apply(&mut gate, false, PagePress::Nobody, 2));
        assert!(
            gate.admitted(false).is_none(),
            "an Apply that answers no offer"
        );
    }

    /// Whose gesture an action came from: a control client's gesture anywhere
    /// since the page's previous action, or one on the page itself, taints it,
    /// whatever else touched the page; a person's touch alone is a person's
    /// press; nothing is nobody's. Each take starts the evidence again.
    #[test]
    fn a_control_clients_gesture_is_never_a_persons_press() {
        let mut gate = PermissionGate::default();
        assert_eq!(gate.take_press(0), PagePress::Nobody);
        gate.note_touch(false);
        assert_eq!(gate.take_press(0), PagePress::Person);
        assert_eq!(gate.take_press(0), PagePress::Nobody, "taken once");
        gate.note_touch(false);
        assert_eq!(gate.take_press(1), PagePress::Controller, "the count moved");
        gate.note_touch(true);
        assert_eq!(
            gate.take_press(1),
            PagePress::Controller,
            "a controller's touch on the page"
        );
        assert_eq!(gate.take_press(1), PagePress::Nobody, "taken once");
        assert_eq!(gate.take_press(u64::MAX), PagePress::Controller);
        assert_eq!(gate.take_press(u64::MAX), PagePress::Nobody);
    }

    /// THE PAGE'S OWN AUTOMATIC ANSWER, END TO END THROUGH THE HOST: the startup
    /// offer on System State, its first frame presented, its first tick in an
    /// active window with nobody touching it — the page queues its Apply, the
    /// host answers the offer (`App::run_system_state_action` →
    /// `App::note_system_state_apply` with the offer pending and no gesture),
    /// the worker accepts it, and the fresh status naming the plan with its
    /// Permissions group makes the window's own admission (`AdmittedBy::Window`),
    /// the standing yes the debugger step spends. It holds when a control
    /// client drove another tab meanwhile: agents at work elsewhere never cost a
    /// new Mac its yes. CONTROL: a control client's gesture on the page itself
    /// before that answer leaves the same plan admitted and no standing yes.
    /// Any rework of the automatic run must keep this path.
    #[test]
    fn the_pages_automatic_answer_is_the_windows_own_admission() {
        for (elsewhere, on_page) in [(false, false), (true, false), (false, true)] {
            let mut app = App::headless_for_test();
            app.headless = false;
            let wid = crate::WindowId(0);
            app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
            let (send, receive) = std::sync::mpsc::channel();
            app.offer_system_state(Request::new(Plan::default(), send));
            assert!(app.prepare_native_input_scratch(wid));
            assert!(app.commit_glassless_native_frame(wid), "the offer on glass");
            if elsewhere {
                app.with_gesture_source(true, |_| ());
            }
            if on_page {
                app.with_gesture_source(true, App::note_system_state_touch);
            }
            let start = Instant::now();
            app.tick_system_state(start);
            let mut answer = receive
                .try_recv()
                .expect("the page answered the offer by itself");
            assert!(matches!(
                answer.decision,
                WorkstationSetupDecision::Begin(_)
            ));
            answer.receipt.take().expect("an Apply's receipt").accept();
            app.tick_system_state(start + Duration::from_secs(1));
            app.observe_initial_setup_permissions(policy(3, true));
            let admitted = app.admitted_access().map(|yes| (yes.revision(), yes.by()));
            if on_page {
                assert_eq!(admitted, None, "a control client touched the page first");
                assert!(
                    app.workstation_setup.permissions.allowed(),
                    "the plan stands"
                );
            } else {
                assert_eq!(admitted, Some((3, AdmittedBy::Window)));
            }
        }
    }

    /// The rig's admission comes from a real gate: the window's own answer
    /// holds one, a control client's holds none.
    #[test]
    fn the_rigs_admission_is_the_gates() {
        assert!(gate_admission_for_test(true).is_some());
        assert!(gate_admission_for_test(false).is_none());
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
    fn a_closed_window_cannot_be_overtaken_by_a_late_choice() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Selection::all(), send);
        request.finish(WorkstationSetupDecision::Unavailable);
        request.finish(WorkstationSetupDecision::Begin(Selection::all().into()));
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(!request.pending());
    }

    #[test]
    fn the_first_choice_cannot_be_replaced_by_a_closing_window_or_another_choice() {
        let (send, receive) = mpsc::channel();
        let request = Request::new(Selection::all(), send);
        let chosen = preview_subset();
        assert!(request.finish_answer(WorkstationSetupDecision::Begin(chosen.into()).into()));
        assert!(!request.finish_answer(WorkstationSetupDecision::Unavailable.into()));
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
    fn a_delivered_answer_handoff_precedes_the_return_and_preserves_the_receipt() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let handoffs = Arc::new(AtomicUsize::new(0));
        let count = handoffs.clone();
        let (send, receive) = mpsc::channel();
        let request = Request::with_handoff(Plan::default(), send, move || {
            count.fetch_add(1, Ordering::AcqRel);
        });
        let (receipt, accepted) = crate::InitialSetupApplyReceipt::detached(true);
        let mut answer: SetupAnswer = WorkstationSetupDecision::Begin(Plan::default()).into();
        answer.receipt = Some(receipt);
        assert!(request.finish_answer(answer));
        assert_eq!(handoffs.load(Ordering::Acquire), 1);
        let mut answer = receive.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(matches!(
            answer.decision,
            WorkstationSetupDecision::Begin(_)
        ));
        assert_eq!(accepted.try_recv(), Err(mpsc::TryRecvError::Empty));
        answer.receipt.take().unwrap().accept();
        assert_eq!(accepted.recv_timeout(Duration::from_secs(60)), Ok(true));
        assert!(
            !request
                .clone()
                .finish_answer(WorkstationSetupDecision::Unavailable.into())
        );
        assert!(!request.finish_answer(WorkstationSetupDecision::Begin(Plan::default()).into()));
        assert_eq!(handoffs.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_withdrawn_or_disconnected_answer_owns_no_handoff() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        for disconnected in [false, true] {
            let handoffs = Arc::new(AtomicUsize::new(0));
            let count = handoffs.clone();
            let (send, receive) = mpsc::channel();
            let request = Request::with_handoff(Plan::default(), send, move || {
                count.fetch_add(1, Ordering::AcqRel);
            });
            if disconnected {
                drop(receive);
            } else {
                request.finish(WorkstationSetupDecision::Unavailable);
                assert_eq!(
                    receive
                        .recv_timeout(Duration::from_secs(60))
                        .unwrap()
                        .decision,
                    WorkstationSetupDecision::Unavailable
                );
            }
            assert!(
                !request.finish_answer(WorkstationSetupDecision::Begin(Plan::default()).into())
            );
            assert!(!request.pending());
            assert_eq!(handoffs.load(Ordering::Acquire), 0);
        }
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
