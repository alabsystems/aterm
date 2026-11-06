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
    PROXY
        .lock()
        .ok()
        .and_then(|proxy| proxy.clone())
        .is_some_and(|proxy| proxy.send_event(wake).is_ok())
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

#[derive(Default)]
pub(crate) struct Host {
    startup: Option<fn() -> bool>,
    startup_retry: Option<Instant>,
    request: Option<Request>,
    unified: bool,
    permissions: PermissionGate,
    proxy_registered: bool,
    /// The Command Line Tools install a setup run asked for this launch.
    pub(crate) developer_tools: crate::clt_install::Install,
    /// Its dedicated children, including recovered ones: any live child owns the prompt slot.
    pub(crate) developer_tools_session: Vec<crate::clt_install::Terminal>,
    pub(crate) developer_tools_stop: crate::clt_install::StopReach,
    /// The attempt whose result may change the install's state. Kept after its terminal leaves,
    /// so an older child's delayed status cannot become current again.
    pub(crate) developer_tools_attempt: Option<u64>,
    /// Whether aterm asked for the tools by itself for this account on this build, its tab or
    /// the package follow-up's `aterm init` ([`crate::clt_install::Mark`]): aterm asks by itself
    /// once per account and build, so an update retries it once.
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
    /// aterm asked for them for this account on this build already (the tab at an earlier launch,
    /// or the package follow-up, whose own install may be under way now) and no tab of this
    /// window's has started one since: no tab opens by itself until a person presses Install…,
    /// so the shim says they are missing and not on their way, though a pass still wants them,
    /// unless an install of them is under way on this Mac ([`App::developer_tools_on_their_way`]).
    /// A tab that starts the install, or one adopted alive from an earlier aterm, clears it; a
    /// newer build's launch starts without it, as its own automatic tab may open.
    pub(crate) developer_tools_unfinished: bool,
    /// A package pass said `c-toolchain: missing` while an install of the tools was on its way
    /// ([`App::developer_tools_on_their_way`]): its only-you entry waits, since it would ask a
    /// person to start what aterm is doing, and is filed only if that install goes without them
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
    /// Whether setup's one early elevation owes this Mac a root step beside the tools (developer
    /// mode off, or no rule to name the Mac by after admission), as the launch read it off the loop
    /// ([`crate::clt_install::read_root_steps`]): a Mac that has the tools still gets the
    /// automatic tab, whose one password does those steps ([`App::root_steps_owed`]).
    pub(crate) developer_tools_root_owed: bool,
    /// The launch runs in a scratch world ([`aterm_types::world::MARKER`] or `CI` set, read as
    /// [`crate::converge_lane::runs`] reads them): a check's or a lab's bundle, whose automatic
    /// tab would put a password ask on the desk and take the keys the check types into its own
    /// session, so the tab never opens by itself there and the root steps go unread.
    pub(crate) developer_tools_scratch_world: bool,
    /// The elevation's other steps are still to be read ([`Self::take_root_steps_read`]): set at
    /// launch where the window reads this Mac, outside a scratch world, and this build's tab
    /// never opened for the account.
    root_steps_unread: bool,
    /// The launch reads the elevation's other steps and their answer has not landed
    /// ([`crate::App::root_steps_owed`]): set where [`Self::root_steps_unread`] is, cleared by
    /// that answer or a reading that could not start. Until then the Mac Setup tab may still be
    /// the window's next macOS ask on a Mac that has the tools
    /// ([`crate::App::mac_setup_tab_comes_next`]).
    pub(crate) root_steps_awaited: bool,
    /// The last reason the automatic tab was held that the window logged
    /// ([`crate::App::developer_tools_install_held`]), so each new one is said once.
    pub(crate) developer_tools_held_said: std::cell::Cell<Option<&'static str>>,
}

impl Host {
    /// Whether the elevation's other steps are to be read now
    /// ([`crate::clt_install::read_root_steps`]), once: only when the window's wake proxy is
    /// registered (`registered`), as the reading's answer is a wake that a window without its
    /// proxy drops, which left a Mac that has the tools with no automatic tab (round 9, the CLT
    /// base: the reading landed 55 ms before the window's first present registered the proxy).
    fn take_root_steps_read(&mut self, registered: bool) -> bool {
        let due = registered && self.root_steps_unread;
        if due {
            self.root_steps_unread = false;
        }
        due
    }

    /// The window's setup host at launch, keeping the Command Line Tools `shim` the launch
    /// reconciled ahead of its first shell ([`crate::clt_shim::Shim::for_launch`]).
    pub(crate) fn for_launch(
        startup: Option<fn() -> bool>,
        headless: bool,
        shim: crate::clt_shim::Shim,
    ) -> Self {
        Self::for_launch_marked(
            startup,
            headless,
            crate::clt_install::Mark::for_launch(),
            shim,
        )
    }

    /// [`Self::for_launch`] with this account's Mac Setup tab `mark` as the launch read
    /// it: a tab that opened at an earlier launch of this build, with nothing installing the tools
    /// since, leaves them unfinished ([`Self::developer_tools_unfinished`]). A window whose `shim`
    /// finds the tools missing wants them from here, as the launch's early step with no pass and no
    /// page ([`crate::clt_install::LAUNCH_WANT`]), unless the mark says its tab opened on this
    /// build before, and watches for their arrival where it reads this Mac.
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
            mark.opened(),
        );
        if launch.watch {
            crate::clt_install::watch_for_tools();
        }
        let scratch_world = !cfg!(test)
            && (crate::converge_lane::env_set(aterm_types::world::MARKER)
                || crate::converge_lane::env_set("CI"));
        let root_steps_unread = reads_mac && !scratch_world && !mark.opened();
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
            developer_tools: launch.install(),
            developer_tools_session: Vec::new(),
            developer_tools_stop: crate::clt_install::StopReach::for_launch(headless),
            developer_tools_attempt: None,
            developer_tools_unfinished: mark.opened(),
            c_toolchain_missing_held: false,
            developer_tools_mark: mark,
            developer_tools_probe: (!headless && !cfg!(test)).then_some(
                crate::clt_install::installing_now as fn() -> crate::clt_install::Reading,
            ),
            developer_tools_seen: None,
            developer_tools_installed: (!headless && !cfg!(test))
                .then_some(crate::clt_install::c_toolchain_installed as fn() -> Option<bool>),
            developer_tools_after: crate::clt_install::Aftermath::default(),
            developer_tools_want: launch.want(),
            developer_tools_shim: shim,
            developer_tools_shim_said: std::cell::Cell::new(None),
            developer_tools_low_data: None,
            developer_tools_network: None,
            developer_tools_not_admin: false,
            developer_tools_reads_mac: reads_mac,
            developer_tools_root_owed: false,
            developer_tools_scratch_world: scratch_world,
            root_steps_unread,
            root_steps_awaited: root_steps_unread,
            developer_tools_held_said: std::cell::Cell::new(None),
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
        let Some(request) = self
            .workstation_setup
            .request
            .as_ref()
            .filter(|request| request.pending())
        else {
            return false;
        };
        let Some(state) = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .and_then(|instance| self.native_runtime.app(instance))
        else {
            return false;
        };
        !matches!(state, crate::native_app::NativeApp::SystemState(state)
            if state.offer_can_restart(request))
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
            aterm_types::macos_setup::on_no_macos_dialog_for_window(wake_for_no_dialog);
        }
        // The elevation's other steps read off the loop, answered by a wake: a Mac that has the
        // tools gets the automatic tab only for what that reading says is owed.
        let registered = self.workstation_setup.proxy_registered;
        if self.workstation_setup.take_root_steps_read(registered)
            && !crate::clt_install::read_root_steps()
        {
            self.workstation_setup.root_steps_awaited = false;
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

    /// The elevation's reading starts only once the window can hear its answer, and only once:
    /// before the wake proxy registers it waits (its wake would be dropped), and a launch that
    /// owes no reading starts none.
    #[test]
    fn the_root_steps_read_waits_for_the_wake_proxy_and_runs_once() {
        let mut host = Host::default();
        host.root_steps_unread = true;
        assert!(!host.take_root_steps_read(false), "no proxy: no read yet");
        assert!(host.take_root_steps_read(true), "the proxy is up: read now");
        assert!(!host.take_root_steps_read(true), "once");
        assert!(
            !Host::default().take_root_steps_read(true),
            "nothing to read"
        );
    }

    #[test]
    fn automatic_setup_starts_permission_steps() {
        let mut gate = PermissionGate::default();
        gate.apply_started(true, PagePress::Nobody);
        gate.accepted();
        gate.observe(policy(7, true));
        assert!(gate.own_admission_stands());
        assert!(gate.steps_admitted());

        gate.apply_started(false, PagePress::Person);
        gate.accepted();
        gate.observe(policy(8, true));
        assert!(gate.steps_admitted());
    }

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

    /// A pending offer with no System State page has no draft and no answer in flight: a
    /// page's close lets its offer go, and the successor offers the plan again (§6.1).
    #[cfg(unix)]
    #[test]
    fn a_pending_setup_offer_with_no_page_holds_no_handoff() {
        let mut app = App::headless_for_test();
        let (send, _receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.workstation_setup.request = Some(request.clone());
        assert!(request.pending(), "PRECONDITION: the offer is unanswered");
        assert!(
            app.native_runtime
                .instance_by_kind(crate::native_app::AppKind::SystemState)
                .is_none(),
            "PRECONDITION: no System State page"
        );
        assert!(!app.workstation_setup_holds_handoff());

        let mut app = App::headless_for_test();
        app.headless = false;
        app.user_input_recent = |_| false;
        app.windows
            .get_mut(&crate::WindowId(0))
            .expect("window 0")
            .os_window_for_test = true;
        let (send, receive) = mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.offer_workstation_setup(request.clone());
        let instance = app
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .expect("the offer opened its page");
        assert!(request.pending());
        app.finish_system_state_close(instance, crate::native_app::CloseScope::Tab);
        assert!(!request.pending(), "a page's close lets its offer go");
        assert_eq!(
            receive.try_recv().map(|answer| answer.decision),
            Ok(WorkstationSetupDecision::Unavailable),
            "the launch's offer is made again where a page can answer it"
        );
        assert!(!app.workstation_setup_holds_handoff());
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

    /// A person's admission is the window's own Apply of a person's press, for the revision it
    /// named, while that plan stands; the page's automatic answer is the window's own admission
    /// but no person's.
    #[test]
    fn the_standing_yes_is_the_windows_own_admission_of_the_plan_in_force() {
        let mut gate = PermissionGate::default();
        assert!(apply(&mut gate, true, PagePress::Nobody, 3));
        assert!(
            gate.own_admission_stands() && gate.steps_admitted(),
            "automatic setup starts permission acquisition"
        );

        let mut gate = PermissionGate::default();
        assert!(apply(&mut gate, false, PagePress::Person, 4));
        assert!(gate.steps_admitted(), "a person's press on the page");

        // A newer status naming another plan, a held intent, a plan withdrawn
        // or an unreadable status each end it.
        gate.observe(policy(5, true));
        assert!(
            gate.allowed() && !gate.own_admission_stands(),
            "another plan"
        );
        assert!(apply(&mut gate, true, PagePress::Nobody, 6));
        gate.hold();
        assert!(!gate.own_admission_stands(), "a newer intent held");
        gate.accepted();
        gate.observe(policy(6, true));
        assert!(!gate.own_admission_stands(), "an Apply of nobody's own");
        assert!(apply(&mut gate, true, PagePress::Nobody, 7));
        gate.observe(policy(7, false));
        assert!(!gate.own_admission_stands(), "Permissions not admitted");
        assert!(apply(&mut gate, true, PagePress::Nobody, 8));
        gate.observe(None);
        assert!(!gate.own_admission_stands(), "an unreadable status");
    }

    /// A control client's Apply, a status poll or a hand-edited record allows the page's
    /// guidance but makes no admission of the window's own.
    #[test]
    fn no_control_client_status_poll_or_record_mints_the_standing_yes() {
        let mut gate = PermissionGate::default();
        assert!(!apply(&mut gate, true, PagePress::Controller, 2));
        assert!(gate.allowed(), "the plan is admitted");
        assert!(!gate.own_admission_stands(), "a control client's Apply");

        let mut gate = PermissionGate::default();
        assert!(!gate.observe(policy(2, true)), "a status poll");
        assert!(gate.allowed() && !gate.own_admission_stands());

        let mut gate = PermissionGate::default();
        assert!(!apply(&mut gate, false, PagePress::Nobody, 2));
        assert!(
            !gate.own_admission_stands(),
            "an Apply that answers no offer"
        );
    }

    /// A control client's gesture since the page's previous action, anywhere or on the page,
    /// taints the press; a person's touch alone is a person's press, and no touch is nobody's.
    /// Each take starts afresh.
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

    /// The page's own answer to the startup offer, end to end through the host, comes on the
    /// first tick, as the offered plan stands, even after a control
    /// client touched the page: a touch holds no answer since design revision 4. It makes the
    /// window's own admission (`AdmittedBy::Window`) while a control client drives another tab;
    /// a control client's touch on the page itself leaves the plan standing with no standing
    /// yes for the access steps.
    #[test]
    fn the_pages_own_answer_is_the_windows_own_admission() {
        for (elsewhere, on_page) in [(false, false), (true, false), (false, true)] {
            let mut app = App::headless_for_test();
            app.headless = false;
            app.user_input_recent = |_| true;
            let wid = crate::WindowId(0);
            app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
            let (send, receive) = std::sync::mpsc::channel();
            app.offer_system_state(Request::new(Plan::default(), send));
            if elsewhere {
                app.with_gesture_source(true, |_| ());
            }
            if on_page {
                app.with_gesture_source(true, |app| app.interact_system_state(wid));
            }
            let start = Instant::now();
            app.tick_system_state(start);
            let mut answer = receive
                .try_recv()
                .expect("the first tick answers the offer");
            assert_eq!(
                answer.decision,
                WorkstationSetupDecision::Begin(Plan::default())
            );
            let receipt = answer.receipt.take().expect("an answer's receipt");
            assert_eq!(
                receipt.admission(),
                crate::system_state::Admission::Automatic
            );
            receipt.accept();
            app.tick_system_state(start + Duration::from_secs(1));
            app.observe_initial_setup_permissions(policy(3, true));
            let admitted = app.permission_steps_admitted();
            if on_page {
                assert!(admitted, "the admitted plan starts automatically");
                assert!(
                    app.workstation_setup.permissions.allowed(),
                    "the plan stands"
                );
            } else {
                assert!(admitted, "elsewhere {elsewhere}");
            }
        }
    }

    fn stopped_launch_offer() -> (App, Request, std::sync::mpsc::Receiver<SetupAnswer>) {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = crate::WindowId(0);
        app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
        app.workstation_setup =
            Host::for_launch(Some(|| true), false, crate::clt_shim::Shim::default());
        app.observe_initial_setup_permissions(policy(4, true));
        assert_eq!(app.access_revision(), Some(4));
        app.stop_initial_setup_permissions();
        assert!(app.access_revision().is_none());
        let (send, receive) = std::sync::mpsc::channel();
        let request = Request::new(Plan::default(), send);
        app.offer_system_state(request.clone());
        assert!(app.prepare_native_input_scratch(wid));
        assert!(app.commit_glassless_native_frame(wid));
        assert!(request.pending());
        assert!(app.access_revision().is_none());
        (app, request, receive)
    }

    #[test]
    fn a_withdrawn_launch_offer_cannot_release_an_earlier_local_stop() {
        let (mut app, request, receive) = stopped_launch_offer();
        request.finish(WorkstationSetupDecision::Unavailable);
        app.tick_system_state(Instant::now());
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert!(!request.pending());
        // This is the unchanged policy from before Stop, read after the
        // withdrawn offer released its temporary hold. It admits no choice.
        app.observe_initial_setup_permissions(policy(4, true));
        assert!(
            app.access_revision().is_none(),
            "a withdrawn offer must not release the earlier local Stop"
        );
        assert!(!app.permission_steps_admitted());
        assert!(app.workstation_setup.suppresses_permissions());
    }

    #[test]
    fn an_accepted_press_after_a_stop_rearms_after_its_fresh_policy() {
        let (mut app, _, receive) = stopped_launch_offer();
        let wid = crate::WindowId(0);
        let (_, view) = app.active_native_view(wid).unwrap();
        app.with_gesture_source(false, |app| {
            app.interact_system_state(wid);
            app.dispatch_native_view_event(
                wid,
                view,
                crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new("system-state/group/1/leave"),
                    value: None,
                }),
            )
        })
        .unwrap();
        let mut answer = receive.try_recv().unwrap();
        assert!(matches!(
            answer.decision,
            WorkstationSetupDecision::Begin(_)
        ));
        let receipt = answer.receipt.take().unwrap();
        assert_eq!(receipt.admission(), crate::system_state::Admission::Person);
        app.observe_initial_setup_permissions(policy(4, true));
        assert!(
            app.access_revision().is_none(),
            "delivery is not acceptance"
        );
        receipt.accept();
        app.resolve_system_state_apply();
        assert!(
            app.access_revision().is_none(),
            "acceptance still needs a fresh read"
        );
        app.observe_initial_setup_permissions(policy(5, true));
        assert_eq!(app.access_revision(), Some(5));
        assert!(app.permission_steps_admitted(), "a person's own admission");
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
        assert!(eligible_launch_command(true, true, false, false));
        assert!(eligible_launch_command(true, false, true, true));
        assert!(!eligible_launch_command(true, false, true, false));
        assert!(!eligible_launch_command(true, false, false, true));
        assert!(!eligible_launch_command(true, false, false, false));
    }

    /// A copy that goes away (its posture is not Installed) never offers setup, however it was
    /// launched — it installs itself and relaunches first (design §6.8).
    #[test]
    fn a_copy_that_goes_away_never_offers_setup() {
        assert!(!eligible_launch_command(false, true, false, false));
        assert!(!eligible_launch_command(false, false, true, true));
        assert!(!eligible_launch_command(false, true, true, true));
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

    /// An offer its page let go ([`Request::let_go`]) admits nothing: it comes back as one no
    /// window showed, so the worker makes it again and the page answers it at its first tick,
    /// and no Begin handoff runs.
    #[test]
    fn an_offer_its_page_let_go_admits_nothing() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let (send, receive) = mpsc::channel();
        let request = Request::with_handoff(Plan::default(), send, move || {
            called.fetch_add(1, Ordering::AcqRel);
        });
        request.let_go();
        assert_eq!(
            receive.try_recv().unwrap().decision,
            WorkstationSetupDecision::Unavailable
        );
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert!(!request.finish_answer(WorkstationSetupDecision::Begin(Plan::default()).into()));
        assert!(receive.try_recv().is_err());
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
    fn a_window_gaining_focus_counts_as_a_focus_gain() {
        let mut app = App::headless_for_test();
        let seen = window_focus_gains();
        app.window_focus_event(crate::WindowId(0), true);
        assert!(window_focus_gains() > seen);
    }

    /// An offer that reaches a window nobody has focused is shown in System State at once, not
    /// answered Unavailable for the worker to try again a minute later, and the page answers it
    /// on its first tick.
    #[test]
    fn an_offer_to_an_unfocused_window_is_shown_and_answered_at_once() {
        let (send, receive) = mpsc::channel();
        let mut app = App::headless_for_test();
        app.headless = false;
        app.user_input_recent = |_| true;
        let wid = crate::WindowId(0);
        app.windows.get_mut(&wid).unwrap().os_window_for_test = true;
        app.window_focus_event(wid, false);
        assert!(!app.any_os_window_focused());
        let request = Request::new(Plan::default(), send);
        app.offer_workstation_setup(request.clone());
        assert!(receive.try_recv().is_err(), "not refused for want of focus");
        assert!(app.workstation_setup.pending());
        assert!(
            app.native_runtime
                .instance_by_kind(crate::native_app::AppKind::SystemState)
                .is_some(),
            "System State shows it"
        );
        app.tick_system_state(Instant::now());
        let mut answer = receive.try_recv().expect("the first tick answers it");
        assert_eq!(
            answer.decision,
            WorkstationSetupDecision::Begin(Plan::default())
        );
        answer.receipt.take().unwrap().accept();
        app.resolve_system_state_apply();
        assert!(!request.pending());
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
