// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-launch Mac setup's offer, which System State answers once it is on screen.

use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};
use std::time::{Duration, Instant};

use aterm_types::macos_setup::{Plan, Selection};
use aterm_window::event_loop::EventLoopProxy;

use crate::{App, Wake};

/// System State's answer to the first-launch offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkstationSetupDecision {
    /// Begin with this plan: the page's own answer on its first tick (no review, no countdown,
    /// no confirmation and no wait for a person: design revision 4, init-on-by-default design
    /// §6.1), or a press on the page.
    Begin(Plan),
    /// No window could show the offer, its page closed before it answered, or the setup it
    /// offers ended meanwhile: the worker offers it again once a window can.
    Unavailable,
}

/// The page's decision and the optional, ephemeral vault password.
#[derive(Debug)]
pub struct SetupAnswer {
    pub decision: WorkstationSetupDecision,
    pub password: Option<aterm_secrets::SecretBytes>,
    pub receipt: Option<crate::InitialSetupApplyReceipt>,
    /// The plan is a group choice (or a Join) made on the page, not the plan as it stood
    /// (the first-run offer applied, or Set up now), so never a provisional plan aterm chose.
    pub chosen: bool,
    /// A Join: the plan names the vault the person typed, which moves the Mac.
    pub vault_typed: bool,
    /// The settings of the groups the person changed on the page since it last took the plan
    /// in force (a group pressed and pressed back counts): the worker applies only these, as
    /// the plan sets them, onto the plan in force as it admits it, so a page left open while
    /// a Restore or another window changed the plan never undoes that change (design §3.3).
    /// Empty: the plan in force as it stands (Set up now). `None`: the plan as sent (the
    /// first-run offer's answer).
    pub changed: Option<Selection>,
}

impl From<WorkstationSetupDecision> for SetupAnswer {
    fn from(decision: WorkstationSetupDecision) -> Self {
        Self {
            decision,
            password: None,
            receipt: None,
            chosen: false,
            vault_typed: false,
            changed: None,
        }
    }
}

static PROXY: Mutex<Option<EventLoopProxy<Wake>>> = Mutex::new(None);

pub(crate) fn send_setup_wake(wake: Wake) -> bool {
    #[cfg(any(test, debug_assertions))]
    if let Some(window) = SetupLoopForTest::standing_in() {
        return window.offer(&wake);
    }
    PROXY
        .lock()
        .ok()
        .and_then(|proxy| proxy.clone())
        .is_some_and(|proxy| proxy.send_event(wake).is_ok())
}

/// A test's stand-in, on its own thread, for the window's loop as the setup worker's
/// publications meet it. While it lives, every [`send_setup_wake`] of its thread goes to it and
/// not to the registered proxy, and that thread's record of the readings the loop took
/// ([`crate::initial_setup_readings_undelivered`]) is the stand-in's own, begun as a launch's
/// is. No other thread is touched, so a test that publishes beside it changes nothing it
/// counts.
#[cfg(any(test, debug_assertions))]
pub struct SetupLoopForTest(std::rc::Rc<LoopStandIn>);

/// What a [`SetupLoopForTest`] keeps for its thread.
#[cfg(any(test, debug_assertions))]
pub(crate) struct LoopStandIn {
    /// Whether the loop has registered its proxy and not closed since.
    registered: std::cell::Cell<bool>,
    statuses: std::cell::Cell<u32>,
    readings: std::cell::Cell<u32>,
    pub(crate) delivery: crate::system_state_host::ReadingsDelivery,
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static LOOP_STAND_IN: std::cell::RefCell<Option<std::rc::Rc<LoopStandIn>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(any(test, debug_assertions))]
impl LoopStandIn {
    /// Count `wake` if it is a setup publication, and say whether the loop took it.
    fn offer(&self, wake: &Wake) -> bool {
        match wake {
            Wake::InitialSetupStatus { .. } => self.statuses.set(self.statuses.get() + 1),
            Wake::InitialSetupReadings(_) => self.readings.set(self.readings.get() + 1),
            _ => {}
        }
        self.registered.get()
    }
}

#[cfg(any(test, debug_assertions))]
impl SetupLoopForTest {
    /// Stand in on this thread for a window whose loop has not registered its proxy: each
    /// publication is dropped until [`Self::register`].
    ///
    /// # Panics
    /// If this thread already has a stand-in.
    #[must_use]
    pub fn before_its_proxy() -> Self {
        let window = std::rc::Rc::new(LoopStandIn {
            registered: std::cell::Cell::new(false),
            statuses: std::cell::Cell::new(0),
            readings: std::cell::Cell::new(0),
            delivery: crate::system_state_host::ReadingsDelivery::new(),
        });
        LOOP_STAND_IN.with(|standing| {
            let earlier = standing.borrow_mut().replace(std::rc::Rc::clone(&window));
            assert!(earlier.is_none(), "one stand-in for the loop a thread");
        });
        Self(window)
    }

    /// The loop registers its proxy, for the first time or again after [`Self::close`]: it
    /// takes every publication from here on.
    pub fn register(&self) {
        self.0.registered.set(true);
    }

    /// The loop closed: a publication finds nobody, as one made before the proxy registered.
    pub fn close(&self) {
        self.0.registered.set(false);
    }

    /// The status publications sent on this thread since the stand-in began, taken or dropped.
    #[must_use]
    pub fn statuses_sent(&self) -> u32 {
        self.0.statuses.get()
    }

    /// The readings publications sent on this thread since the stand-in began, taken or
    /// dropped.
    #[must_use]
    pub fn readings_sent(&self) -> u32 {
        self.0.readings.get()
    }

    /// This thread's stand-in, if a test holds one. A thread that is ending, its thread-locals
    /// gone, has none: a wake sent from another thread-local's drop goes to the proxy.
    pub(crate) fn standing_in() -> Option<std::rc::Rc<LoopStandIn>> {
        LOOP_STAND_IN
            .try_with(|standing| standing.borrow().clone())
            .ok()
            .flatten()
    }
}

#[cfg(any(test, debug_assertions))]
impl Drop for SetupLoopForTest {
    fn drop(&mut self) {
        let _ = LOOP_STAND_IN.try_with(|standing| standing.borrow_mut().take());
    }
}

/// Wake nonblocking maintenance after its worker ends, including while unwinding.
/// A closed or not-yet-registered window simply has no receiver.
pub fn wake_host_maintenance() {
    let _ = send_setup_wake(Wake::HostMaintenance);
}

/// The window's wake once no macOS dialog of this process is up
/// ([`aterm_types::macos_setup::on_no_macos_dialog_for_window`]): it runs in the last dialog's
/// drop, on whichever thread let it go, so it only posts [`Wake::NoMacosDialog`].
fn wake_for_no_dialog() {
    let _ = send_setup_wake(Wake::NoMacosDialog);
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

/// Focus gains of this process's windows ([`window_focus_gains`]).
static FOCUS_GAINS: FocusGains = FocusGains::new();

/// The worker notified when a window returns to the person.
static FOCUS_WAKE: Mutex<Option<fn()>> = Mutex::new(None);

/// Register the setup worker's wake alongside its other recovery signals.
pub fn on_window_focus_gained(wake: fn()) {
    *FOCUS_WAKE.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
}

/// A count of focus gains.
struct FocusGains {
    count: Mutex<u64>,
}

impl FocusGains {
    const fn new() -> Self {
        Self {
            count: Mutex::new(0),
        }
    }

    fn count(&self) -> u64 {
        *self.count.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn gain(&self) {
        let mut count = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        *count = count.wrapping_add(1);
    }
}

/// How many times a window of this process has gained focus.
pub fn window_focus_gains() -> u64 {
    FOCUS_GAINS.count()
}

/// A window of this process gained focus.
pub(crate) fn window_focus_gained() {
    FOCUS_GAINS.gain();
    let wake = *FOCUS_WAKE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(wake) = wake {
        wake();
    }
}

/// This login's screen came back to a person (unlocked, awake, switched back to this login) while
/// the setup run's sign-outs wait on it (`App::watch_session_return`): the worker's focus wake
/// ([`on_window_focus_gained`]), which ends a park waiting on the sign-outs, so they look again
/// at once rather than at their next timed look. No focus gain is counted.
pub(crate) fn session_returned() {
    #[cfg(test)]
    SESSION_RETURNS.with(|returns| returns.set(returns.get() + 1));
    let wake = *FOCUS_WAKE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(wake) = wake {
        wake();
    }
}

#[cfg(test)]
thread_local! {
    /// The returns [`session_returned`] said on this thread.
    pub(crate) static SESSION_RETURNS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Count a focus gain, as a window's `Focused(true)` does.
#[cfg(any(test, debug_assertions))]
pub fn gain_window_focus_for_test() {
    window_focus_gained();
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
    /// An inert handoff for tests and the development-only System State gallery.
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
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

    /// The offer goes with no answer of the page's: its page closed first. It is made again once
    /// a window can show it ([`WorkstationSetupDecision::Unavailable`]), where the page answers
    /// it at its first tick.
    pub(crate) fn let_go(&self) {
        self.finish(WorkstationSetupDecision::Unavailable);
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
                .unwrap_or_else(PoisonError::into_inner)
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

    #[cfg(any(unix, test))]
    pub(crate) fn same_offer(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.reply, &other.reply)
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

/// The window's own admission: the gate that reads who touched System State and which
/// revision the window's own Apply admitted ([`PermissionGate`]), lives in a leaf module whose
/// private fields nothing else in the crate reaches, this module and its tests included. The
/// aterm-census guard `devtools-door` holds its transitions to their callers here.
#[path = "workstation_setup_gate.rs"]
mod permission_gate;
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

/// When a reader of the elevation's other steps whose thread could not start is started again
/// ([`Host::start_root_steps_read`]): a minute after the first failed start, doubling, never more
/// than ten minutes. A launch reads them afresh, so the schedule lives with the window.
const ROOT_STEPS_READ_AGAIN: aterm_types::retry_schedule::Backoff =
    aterm_types::retry_schedule::Backoff::new(Duration::from_secs(60), Duration::from_secs(600));

/// How long the setup page's own answer to a launch's offer (its answer, a queued page action, a
/// Stop under way) may hold an update: the page settles each at its next 250 ms tick, and a
/// Stop's save within its own settle, so one standing this long is a page that will not settle
/// it. Past it the offer holds only for a person's draft in it.
pub(crate) const SETUP_ANSWER_BACKSTOP: Duration = Duration::from_secs(30);

const _: () = assert!(
    SETUP_ANSWER_BACKSTOP.as_secs() < crate::native_update_auto_intent::LANDS_WITHIN.as_secs()
);

/// What the offer's hold `raw` is once the page's own answer has held past
/// [`SETUP_ANSWER_BACKSTOP`] (`overdue`): nothing, unless a person's draft is in it (`draft`),
/// which stays theirs. Any other hold, or one inside the backstop, is `raw`.
#[must_use]
#[cfg(any(unix, test))]
pub(crate) fn offer_hold_past_backstop(
    raw: Option<crate::native_update_lane_hold::LaneHold>,
    overdue: bool,
    draft: impl FnOnce() -> bool,
) -> Option<crate::native_update_lane_hold::LaneHold> {
    use crate::native_update_lane_hold::LaneHold;
    match raw {
        Some(LaneHold::SetupAnswer) if overdue => draft().then_some(LaneHold::SetupDraft),
        raw => raw,
    }
}

#[derive(Default)]
pub(crate) struct Host {
    startup: Option<fn() -> bool>,
    startup_retry: Option<Instant>,
    maintenance: Option<crate::HostMaintenance>,
    maintenance_due: Option<Instant>,
    request: Option<Request>,
    unified: bool,
    permissions: PermissionGate,
    proxy_registered: bool,
    /// The Command Line Tools install a setup run asked for this launch.
    pub(crate) developer_tools: crate::clt_install::Install,
    /// Its dedicated children, including recovered ones: a live child owns the prompt slot while
    /// it may still ask ([`App::developer_tools_install_may_ask`]).
    pub(crate) developer_tools_session: Vec<crate::clt_install::Terminal>,
    pub(crate) developer_tools_stop: crate::clt_install::StopReach,
    /// The attempt whose result may change the install's state. Kept after its terminal leaves,
    /// so an older child's delayed status cannot become current again.
    pub(crate) developer_tools_attempt: Option<u64>,
    /// The account's record of setup's administrator ask, which its tab and the package
    /// follow-up's `aterm init` share ([`crate::clt_install::Mark`]): aterm asks by itself only
    /// where it lets an automatic ask go now, and again by itself after an attempt that did not
    /// finish, once its next try is due. Updates keep it; a person's own Install… claims it
    /// whatever it says.
    pub(crate) developer_tools_mark: crate::clt_install::Mark,
    /// What this Mac shows of an install under way ([`crate::clt_install::installing_now`]), probed
    /// live at a dispatch boundary; none in a headless instance or a test, which never look at
    /// this Mac's installers.
    pub(crate) developer_tools_probe: Option<fn() -> crate::clt_install::Reading>,
    /// The newest reading of this Mac's installers while the watch runs: the one its last second
    /// carried ([`App::developer_tools_watch`]), or a dispatch boundary's since, which the
    /// window's other looks read in place of a probe on its loop. `None` where no watch runs.
    pub(crate) developer_tools_seen: Option<crate::clt_install::Reading>,
    /// Read-only C compiler presence, asked only while its missing-tools exception is
    /// unresolved; none in headless instances and tests, which inject their own reading.
    pub(crate) developer_tools_installed: Option<fn() -> Option<bool>>,
    /// Whether this process's keeper holds a session's master and keeps it past a quit whose
    /// BYE names it ([`crate::keeper_link::carries_at_quit`]), which lets a quit leave a Mac
    /// Setup tab's program running for the next launch
    /// ([`App::developer_tools_custody_at_exit`]); none in headless instances and tests, which
    /// carry none unless they inject one.
    pub(crate) developer_tools_keeper_carries: Option<fn(u64) -> bool>,
    /// What the page says of the install beyond its tab and this Mac's installers: the tab said
    /// the tools are installed, or a person's press opened nothing, and why.
    pub(crate) developer_tools_after: crate::clt_install::Aftermath,
    /// This launch's want of the Command Line Tools, or the newest setup pass that ended wanting
    /// them, until a later pass, a Stop, a plan without the GitHub repositories or the tools'
    /// arrival withdraws it: what makes the git-and-friends shim ([`crate::clt_shim`]) say they are
    /// on their way, which outlives the tab.
    pub(crate) developer_tools_want: Option<u64>,
    /// That shim, reconciled ahead of the launch's first shell
    /// ([`crate::clt_shim::Shim::for_launch`]) and settled at each change of the want or of the
    /// install, and at a watch's second that moves its word or finds the tools
    /// ([`App::developer_tools_watch`]).
    pub(crate) developer_tools_shim: crate::clt_shim::Shim,
    /// Whether the shim's last settle said the tools are on their way
    /// ([`App::settle_developer_tools_shim`]): a watch's second that would say the same settles
    /// nothing. `None` before the first.
    pub(crate) developer_tools_shim_said: std::cell::Cell<Option<bool>>,
    /// An install of the tools ended unfinished (the tab said why, or ended without saying), or
    /// the account's record held the automatic ask at launch (another asker's attempt, whose own
    /// install may be under way now, a try not due yet, a person's decline) and no tab of this
    /// window's has started one since: the shim says they are missing and not on their way,
    /// though a want stands, unless an install of them is under way on this Mac
    /// ([`App::developer_tools_on_their_way`]). A tab that starts the install, or one adopted
    /// alive from an earlier aterm, clears it.
    pub(crate) developer_tools_unfinished: bool,
    /// A package pass said `c-toolchain: missing` while an install of the tools was on its way
    /// ([`App::developer_tools_on_their_way`]): its only-you entry waits, since it would ask a
    /// person to start what aterm is doing, and is filed only if they are confirmed absent
    /// after that install ends
    /// ([`App::settle_missing_tools_exception`]).
    pub(crate) c_toolchain_missing_held: bool,
    /// Whether Low Data Mode holds the tools' download back, as the network watch reads it or the
    /// tab's program said: the automatic tab waits while it does, unspent. `None`: unread.
    pub(crate) developer_tools_low_data: Option<bool>,
    /// The watch on Low Data Mode, kept while a pass wants the tools or the mode holds them back.
    pub(crate) developer_tools_network: Option<crate::clt_install::NetworkWatch>,
    /// This account is none of the Mac's administrators, whom alone macOS lets install the tools,
    /// as its groups read when a pass first wants them or the tab's program said: no tab opens by
    /// itself, and the row says so in place of Install….
    pub(crate) developer_tools_not_admin: bool,
    /// Whether the window reads this Mac's network and account for the install; none in a
    /// headless instance or a test.
    pub(crate) developer_tools_reads_mac: bool,
    /// What setup's one early elevation owes this Mac beside the tools (developer mode off, no
    /// rule to name the Mac by after admission, the LAN broker), as the last reading off the loop
    /// said ([`crate::clt_install::read_root_steps`]: at launch, and again after an attempt that
    /// did not finish): a Mac that has the tools still gets the automatic tab, whose one password
    /// does those steps ([`App::root_steps_owed`]).
    pub(crate) developer_tools_root_owed: crate::clt_install::RootSteps,
    /// The launch runs in a scratch world ([`aterm_types::world::MARKER`] or `CI` set, read as
    /// [`crate::converge_lane::runs`] reads them): a check's or a lab's bundle, whose automatic
    /// tab would put a password ask on the desk and take the keys the check types into its own
    /// session, so the tab never opens by itself there and the root steps go unread.
    pub(crate) developer_tools_scratch_world: bool,
    /// The elevation's other steps are still to be read ([`Self::start_root_steps_read`]): set at
    /// launch where the window reads this Mac, outside a scratch world, and this build's tab
    /// never opened for the account.
    root_steps_unread: bool,
    /// A reader whose thread could not start is tried again at this host deadline
    /// ([`ROOT_STEPS_READ_AGAIN`] after [`Self::root_steps_misses`] failed starts in a row).
    root_steps_retry: Option<Instant>,
    /// Starts of the reader that failed in a row; none once one starts.
    root_steps_misses: u32,
    /// An earlier aterm's adopted tab whose attempt could not be carried into the account's
    /// record is carried again at this host deadline
    /// ([`crate::App::carry_earlier_developer_tools_tabs`], after
    /// [`Self::developer_tools_carry_misses`] carries that failed in a row).
    pub(crate) developer_tools_carry_retry: Option<Instant>,
    /// Passes in a row whose carry of such a tab failed; none once every due one is carried.
    pub(crate) developer_tools_carry_misses: u32,
    /// The launch reads the elevation's other steps and their answer has not landed
    /// ([`crate::App::root_steps_owed`]): set where [`Self::root_steps_unread`] is, cleared by
    /// that answer or a reading that could not start. Until then the Mac Setup tab may still be
    /// the window's next macOS ask on a Mac that has the tools
    /// ([`crate::App::mac_setup_tab_comes_next`]).
    pub(crate) root_steps_awaited: bool,
    /// The last reason the automatic tab was held that the window logged
    /// ([`crate::App::developer_tools_install_held`]), so each new one is said once.
    pub(crate) developer_tools_held_said: std::cell::Cell<Option<&'static str>>,
    /// The record's due (unix seconds, [`crate::clt_install::Mark::due`]) whose passing the
    /// window last looked at ([`App::tick_workstation_setup`]), so each due wakes one look.
    developer_tools_due_looked: Option<u64>,
}

impl Host {
    pub(crate) fn with_maintenance(mut self, maintenance: Option<crate::HostMaintenance>) -> Self {
        self.maintenance = maintenance;
        self
    }
    /// Start the elevation's prompt-free reading ([`crate::clt_install::read_root_steps`])
    /// once its thread can start, only when the window's wake proxy is
    /// registered (`registered`), as the reading's answer is a wake that a window without its
    /// proxy drops, which left a Mac that has the tools with no automatic tab (round 9, the CLT
    /// base: the reading landed 55 ms before the window's first present registered the proxy).
    /// A failed spawn retains the reading for a later host deadline on [`ROOT_STEPS_READ_AGAIN`],
    /// with no answer in flight holding the window's other macOS asks while it waits.
    fn start_root_steps_read(
        &mut self,
        registered: bool,
        now: Instant,
        start: impl FnOnce() -> bool,
    ) {
        if !registered
            || !self.root_steps_unread
            || self.root_steps_retry.is_some_and(|retry| retry > now)
        {
            return;
        }
        self.root_steps_awaited = start();
        if self.root_steps_awaited {
            self.root_steps_unread = false;
            self.root_steps_retry = None;
            self.root_steps_misses = 0;
        } else {
            self.root_steps_misses = self.root_steps_misses.saturating_add(1);
            self.root_steps_retry = Some(now + ROOT_STEPS_READ_AGAIN.after(self.root_steps_misses));
        }
    }

    /// Read the elevation's other steps again ([`Self::start_root_steps_read`]), where the window
    /// reads this Mac outside a scratch world: an attempt that did not finish may have done some
    /// of them, so what is owed now comes from a fresh reading, never the launch's. That is a new
    /// fact, so the reader starts at the window's next pass, whatever an earlier failed start's
    /// pause still said ([`Self::root_steps_retry`]); a start that fails again waits on
    /// [`ROOT_STEPS_READ_AGAIN`] for the failed starts in a row.
    pub(crate) fn read_root_steps_again(&mut self) {
        let read = self.developer_tools_reads_mac && !self.developer_tools_scratch_world;
        self.developer_tools_root_owed = crate::clt_install::RootSteps::default();
        self.root_steps_unread = read;
        self.root_steps_awaited = read;
        if read {
            self.root_steps_retry = None;
        }
    }

    /// Whether a due of the record passed that the window has not looked at since
    /// ([`Self::developer_tools_due_looked`]), for a want the record holds: the due's wake
    /// ([`Self::deadline`]) then looks again, with no status or watch to ride.
    fn developer_tools_due_passed(&mut self) -> bool {
        if !matches!(self.developer_tools, crate::clt_install::Install::Wanted(_)) {
            return false;
        }
        let Some(due) = self.developer_tools_mark.due() else {
            return false;
        };
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        if due > unix || self.developer_tools_due_looked == Some(due) {
            return false;
        }
        self.developer_tools_due_looked = Some(due);
        true
    }

    /// The window's setup host at launch, keeping the Command Line Tools `shim` the launch
    /// reconciled ahead of its first shell ([`crate::clt_shim::Shim::for_launch`]).
    pub(crate) fn for_launch(
        startup: Option<fn() -> bool>,
        headless: bool,
        shim: crate::clt_shim::Shim,
        successor: bool,
    ) -> Self {
        Self::for_launch_marked(
            startup,
            headless,
            crate::clt_install::Mark::for_launch(successor),
            shim,
        )
    }

    /// [`Self::for_launch`] with this account's ask record `mark` as the launch read it: a record
    /// that holds the automatic ask now (another asker's attempt, a try not due yet, a person's
    /// decline, a record that cannot be read) leaves the tools unfinished
    /// ([`Self::developer_tools_unfinished`]) until an install starts. A window whose `shim` finds
    /// the tools missing wants them from here, as the launch's early step with no pass and no page
    /// ([`crate::clt_install::LAUNCH_WANT`]), unless no automatic ask may come (a decline, a
    /// record with no place: [`crate::clt_install::Mark::may_come`]), and watches for their
    /// arrival where it reads this Mac.
    pub(crate) fn for_launch_marked(
        startup: Option<fn() -> bool>,
        headless: bool,
        mark: crate::clt_install::Mark,
        shim: crate::clt_shim::Shim,
    ) -> Self {
        let reads_mac = !headless && !cfg!(test);
        let launch = crate::clt_install::launch_install(
            !headless,
            reads_mac,
            shim.tools_missing(),
            !mark.may_come(),
        );
        if launch.watch {
            crate::clt_install::watch_for_tools();
        }
        let scratch_world = !cfg!(test)
            && (crate::converge_lane::env_set(aterm_types::world::MARKER)
                || crate::converge_lane::env_set("CI"));
        let root_steps_unread = reads_mac && !scratch_world && mark.may_come();
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
            maintenance: None,
            maintenance_due: None,
            request: None,
            permissions: PermissionGate::default(),
            proxy_registered: false,
            developer_tools: launch.install(),
            developer_tools_session: Vec::new(),
            developer_tools_stop: crate::clt_install::StopReach::for_launch(headless),
            developer_tools_attempt: None,
            developer_tools_unfinished: mark.held().is_some(),
            c_toolchain_missing_held: false,
            developer_tools_mark: mark,
            developer_tools_probe: (!headless && !cfg!(test)).then_some(
                crate::clt_install::installing_now as fn() -> crate::clt_install::Reading,
            ),
            developer_tools_seen: None,
            developer_tools_installed: (!headless && !cfg!(test))
                .then_some(crate::clt_install::c_toolchain_installed as fn() -> Option<bool>),
            developer_tools_keeper_carries: (!headless && !cfg!(test))
                .then_some(crate::keeper_link::carries_at_quit as fn(u64) -> bool),
            developer_tools_after: crate::clt_install::Aftermath::default(),
            developer_tools_want: launch.want(),
            developer_tools_shim: shim,
            developer_tools_shim_said: std::cell::Cell::new(None),
            developer_tools_low_data: None,
            developer_tools_network: None,
            developer_tools_not_admin: false,
            developer_tools_reads_mac: reads_mac,
            developer_tools_root_owed: crate::clt_install::RootSteps::default(),
            developer_tools_scratch_world: scratch_world,
            root_steps_unread,
            root_steps_retry: None,
            developer_tools_carry_retry: None,
            developer_tools_carry_misses: 0,
            root_steps_misses: 0,
            root_steps_awaited: root_steps_unread,
            developer_tools_held_said: std::cell::Cell::new(None),
            developer_tools_due_looked: None,
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
            .chain(self.developer_tools_due(now))
            .chain(self.root_steps_retry)
            .chain(self.developer_tools_carry_retry)
            .chain(self.maintenance_due)
            .min()
    }

    /// When a want of the tools held only by the record may go (its next try, or the next look at
    /// a record another asker holds or that cannot be read): the window wakes then to look again
    /// ([`crate::clt_install::Mark::due`], [`App::tick_workstation_setup`]), so an attempt that
    /// did not finish is asked again by itself with no other wake (a Mac whose tools are there
    /// has no watch).
    fn developer_tools_due(&self, now: Instant) -> Option<Instant> {
        if !matches!(self.developer_tools, crate::clt_install::Install::Wanted(_)) {
            return None;
        }
        let due = self.developer_tools_mark.due()?;
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        (due > unix).then(|| now + Duration::from_secs(due - unix))
    }

    pub(crate) fn pending(&self) -> bool {
        self.request.as_ref().is_some_and(Request::pending)
    }
    /// Whether setup's hold keeps the permission lanes from admitting anything new.
    #[cfg(test)]
    pub(crate) fn suppresses_permissions(&self) -> bool {
        self.unified && !self.permissions.allowed()
    }

    /// The approved setup revision whose access steps may run
    /// (`system_state_access`): the admitted plan's revision while the gate
    /// allows Permissions, else none.
    pub(crate) fn access_revision(&self) -> Option<u64> {
        self.permissions.access_revision()
    }

    /// No local decision or Stop holds deferred work for the current setup run.
    pub(crate) fn setup_unheld(&self) -> bool {
        self.permissions.setup_unheld()
    }

    fn start(&mut self, now: Instant) {
        self.maintenance_due = self.maintenance.as_mut().and_then(|maintain| maintain());
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

/// The `saved-subset` preview's selection: every row but Dock, Messages and FaceTime.
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
                if let Err(error) = aterm_uds::atomic_file::write(
                    &root.join("workstation-setup-preview-result"),
                    result.as_bytes(),
                    0o600,
                ) {
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

/// Whether this launch may offer the first-run setup: it runs from an installed copy
/// (`aterm_update::first_launch::setup_runs_from_posture`; design §6.8 — a copy that goes away
/// sets nothing up, installs itself and relaunches instead), AND it is a launch with no
/// command, or a relaunch of a running window through a validated handoff where a carried
/// command is inert.
pub(crate) const fn eligible_launch_command(
    posture_allows_setup: bool,
    commandless: bool,
    relaunch: bool,
    admitted_handoff: bool,
) -> bool {
    posture_allows_setup && (commandless || (relaunch && admitted_handoff))
}

impl App {
    /// Show the offer in System State, focused or not: the page answers it by itself once a person
    /// is at this Mac, so nothing waits for a window to come to the front.
    pub(crate) fn offer_workstation_setup(&mut self, request: Request) {
        if self.headless
            || self.update_handoff_in_flight()
            || !self
                .windows
                .values()
                .any(|window| crate::has_os_window(window))
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

    /// An untouched offer lets the handoff go: the successor reads the plan still pending and
    /// admits it at once, as every launch does (init-on-by-default design §6.1). A draft or an
    /// answer still being processed belongs to this process until it has settled, and only a
    /// System State page holds one: with no page there is nothing to settle (a page's close
    /// lets its offer go, `SystemStateApp::closed`), so nothing holds the handoff.
    #[cfg(unix)]
    pub(crate) fn workstation_setup_holds_handoff(&self) -> bool {
        self.workstation_setup_handoff_hold().is_some()
    }

    /// What of the offer holds the handoff ([`Self::workstation_setup_holds_handoff`]), as the
    /// update lane names it: a person's draft is theirs ([`LaneHold::SetupDraft`]), and an
    /// answer the page is still taking is the page's own ([`LaneHold::SetupAnswer`]).
    ///
    /// [`LaneHold::SetupDraft`]: crate::native_update_lane_hold::LaneHold::SetupDraft
    /// [`LaneHold::SetupAnswer`]: crate::native_update_lane_hold::LaneHold::SetupAnswer
    ///
    /// The page's own answer holds at most [`SETUP_ANSWER_BACKSTOP`] of the automatic lane's
    /// attempts in a row ([`crate::native_update_lane_hold::LaneHoldClocks::setup_answer`]): a
    /// page that has not settled it by then will not, and past it the offer holds only for a
    /// person's draft in it, which stays theirs.
    #[cfg(unix)]
    pub(crate) fn workstation_setup_handoff_hold(
        &self,
    ) -> Option<crate::native_update_lane_hold::LaneHold> {
        let overdue = self
            .lane_hold_clocks
            .setup_answer
            .overdue(SETUP_ANSWER_BACKSTOP, std::time::Instant::now());
        offer_hold_past_backstop(self.workstation_setup_offer_hold(), overdue, || {
            self.workstation_setup_offer_draft()
        })
    }

    /// The offer's hold as the page states it, before [`SETUP_ANSWER_BACKSTOP`].
    #[cfg(unix)]
    pub(crate) fn workstation_setup_offer_hold(
        &self,
    ) -> Option<crate::native_update_lane_hold::LaneHold> {
        use crate::native_update_lane_hold::LaneHold;
        use crate::system_state::OfferHold;
        let request = self.pending_setup_offer()?;
        let state = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .and_then(|instance| self.native_runtime.app(instance))?;
        let crate::native_app::NativeApp::SystemState(state) = state else {
            return Some(LaneHold::SetupAnswer);
        };
        state.offer_hold(request).map(|hold| match hold {
            OfferHold::Draft => LaneHold::SetupDraft,
            OfferHold::Answer => LaneHold::SetupAnswer,
        })
    }

    /// Whether a person's draft is in the pending offer's page ([`SystemStateApp::offer_draft`]).
    ///
    /// [`SystemStateApp::offer_draft`]: crate::system_state::SystemStateApp::offer_draft
    #[cfg(unix)]
    fn workstation_setup_offer_draft(&self) -> bool {
        let Some(request) = self.pending_setup_offer() else {
            return false;
        };
        matches!(
            self.native_runtime
                .instance_by_kind(crate::native_app::AppKind::SystemState)
                .and_then(|instance| self.native_runtime.app(instance)),
            Some(crate::native_app::NativeApp::SystemState(state)) if state.offer_draft(request)
        )
    }

    #[cfg(unix)]
    fn pending_setup_offer(&self) -> Option<&Request> {
        self.workstation_setup
            .request
            .as_ref()
            .filter(|request| request.pending())
    }

    pub(crate) fn hold_initial_setup_permissions(&mut self) {
        self.workstation_setup.unified = true;
        self.workstation_setup.permissions.hold();
        self.retire_launch_warmup();
    }

    pub(crate) fn stop_initial_setup_permissions(&mut self) {
        self.workstation_setup.unified = true;
        self.workstation_setup.permissions.stop();
        // A Command Line Tools install the stopped run asked for opens no tab, and its shim goes.
        self.workstation_setup.developer_tools = self.workstation_setup.developer_tools.withdrawn();
        self.withdraw_developer_tools_want();
        self.stop_developer_tools_install();
        self.retire_launch_warmup();
    }

    pub(crate) fn accepted_initial_setup_apply(&mut self) {
        self.workstation_setup.permissions.accepted();
    }

    /// A person's own Set up now ([`Self::note_system_state_apply`]) is their yes to setup's
    /// password ask: a decline the account's record holds (their Stop, a Ctrl-C at the prompt, a
    /// closed Mac Setup tab) is lifted, and the elevation's steps are read again, so the automatic
    /// ask comes by itself where anything is owed ([`crate::clt_install::Mark::lift`]).
    fn persons_yes_lifts_a_decline(&mut self) {
        if !self.workstation_setup.developer_tools_mark.lift() {
            return;
        }
        aterm_log::info!("System State: a person's Set up now lifted the declined password ask");
        let setup = &mut self.workstation_setup;
        setup.developer_tools_unfinished = setup.developer_tools_mark.held().is_some();
        if setup.developer_tools_shim.tools_missing() && setup.developer_tools_want.is_none() {
            setup.developer_tools_want = Some(crate::clt_install::LAUNCH_WANT);
        }
        if setup.developer_tools == crate::clt_install::Install::Unasked
            && setup.developer_tools_shim.tools_missing()
        {
            setup.developer_tools = crate::clt_install::Install::Wanted(
                setup
                    .developer_tools_want
                    .unwrap_or(crate::clt_install::LAUNCH_WANT),
            );
        }
        setup.read_root_steps_again();
        self.settle_developer_tools_want();
    }

    pub(crate) fn recheck_initial_setup_permissions(&mut self) {
        self.workstation_setup.permissions.await_fresh_status();
    }

    pub(crate) fn observe_initial_setup_permissions(
        &mut self,
        policy: Option<crate::InitialSetupPermissionPolicy>,
    ) {
        self.workstation_setup.permissions.observe(policy);
        if self.access_revision().is_none() {
            self.retire_launch_warmup();
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

    /// Whose gestures led to the page's action being taken now (design §6): a person's press
    /// only when a person touched the page and no control client's gesture came since the
    /// page's previous action. Call it once per queued action, as it is taken (a row's
    /// `Open …` passes `== PagePress::Person` as its `by_person`). Keys an `hwkey` burst
    /// posted count as a control client's gestures (`hwkey::posted_keys`): their real
    /// `NSEvent`s carry no gesture mark, and one queued as the page came to the front reaches
    /// it before the burst stops.
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
        if press == PagePress::Person {
            self.persons_yes_lifts_a_decline();
        }
    }

    /// Whether the Apply being handled is a person's own press ([`Self::note_system_state_apply`]).
    pub(crate) const fn system_state_apply_is_a_persons(&self) -> bool {
        self.workstation_setup.permissions.apply_is_a_persons()
    }

    /// A person's press asked for macOS's grants, and the record says so: the next status read
    /// makes this press the window's own admission of the plan in force.
    pub(crate) fn person_asked_for_access(&mut self) {
        self.workstation_setup.permissions.person_asked();
    }

    /// Whether the admitted setup can start its permission steps automatically.
    pub(crate) fn permission_steps_admitted(&self) -> bool {
        self.workstation_setup.permissions.steps_admitted()
            && !self.system_state_host.stopped_access()
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
        // An earlier aterm's live Mac Setup tab, adopted in the handoff now in, holds the
        // account's record before anything here may ask beside it.
        self.carry_earlier_developer_tools_tabs(now);
        if !self.workstation_setup.proxy_registered {
            if let Ok(mut proxy) = PROXY.lock() {
                *proxy = self.proxy.clone();
            }
            self.workstation_setup.proxy_registered = self.proxy.is_some();
            aterm_types::macos_setup::on_no_macos_dialog_for_window(wake_for_no_dialog);
        }
        // The elevation's other steps read off the loop, answered by a wake: a Mac that has the
        // tools gets the automatic tab only for what that reading says is owed.
        let registered = self.workstation_setup.proxy_registered;
        self.workstation_setup.start_root_steps_read(
            registered,
            now,
            crate::clt_install::read_root_steps,
        );
        // The record's due passed (its next try, or the next look at it): look again now, as no
        // status or watch may come to (a finished run polls none; a Mac with the tools has no
        // watch).
        if self.workstation_setup.developer_tools_due_passed() {
            self.tick_developer_tools_install();
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
#[path = "workstation_setup_tests.rs"]
mod tests;
