// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One app-owned, read-only operation through the existing protected askpass sheet.
//! No executable, output path, policy selector or command text comes from the caller.
//!
//! An operation stands for its [`Origin`]: the terminal session in front when `aterm ctl @self
//! sys policy collect` or a pinned CPU card's key started it, or the System Resources page's view
//! whose Collect CPU Policy a person pressed (SD-83). It is admitted only while that origin is in
//! front ([`App::cpu_policy_origin_live`]); the password sheet hangs on the origin's window and
//! is withdrawn with it ([`App::withdraw_askpass_view`] beside the window's and the session's).
//! Who started it rides with it ([`Starter`]): a person's own key or press lets its password
//! sheet past the spacing a sheet nobody touched set before it (`askpass::Backoff::refusal`).
//!
//! The machine runs one operation at a time, and each belongs to the [`Owner`] App that started
//! it: an App reads, cancels and reaps only its own, so a test process's many headless Apps never
//! see each other's operation as their pending one.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use crate::tab_model::ViewId;
use crate::{App, Wake, WindowId};
use aterm_json::{Value, json};
use aterm_sysprobe::cpu_policy::{COLLECTOR_VERB, Identity};

const OUTPUT_LIMIT: usize = 3 * 1024 * 1024;
const EXIT_GRACE: Duration = Duration::from_secs(15);
const SUDO_ARGS: [&str; 3] = ["-k", "-A", "--"];
static OPERATION: Mutex<Option<Arc<Operation>>> = Mutex::new(None);

/// The App an operation belongs to ([`App::cpu_policy_owner`]). A running aterm has one App; a
/// test process has many headless ones side by side, and each reads, cancels and reaps only the
/// operation it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner(u64);

impl Owner {
    /// A new App's own.
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Why a press started no operation, in the words the journal and the ctl verb give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No terminal session is in front of the window a session-bound collect names.
    NoSession,
    /// The origin is not in front: its session not visible in its window, or its view not its
    /// window's front tab in a window that is not occluded.
    NotInFront,
    /// The fabric is halted for askpass.
    Halted,
    /// The askpass relay is switched off.
    RelayOff,
    /// An operation is unsettled: no new one until its exact collector exits.
    Pending,
    /// This App has no event loop for the worker to report to.
    NoEventLoop,
    /// No person stands behind the gesture that asked ([`crate::App::gesture_unvouched`]).
    NotAPerson,
    /// A keep-out named no folder, or another step named one.
    NoFolder,
}

impl Refusal {
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::NoSession => "no terminal session is in front",
            Self::NotInFront => "its tab is not in front",
            Self::Halted => "askpass is halted",
            Self::RelayOff => "the askpass relay is off",
            Self::Pending => "a collection is pending",
            Self::NoEventLoop => "this aterm has no event loop",
            Self::NotAPerson => "only a person's own click or key runs it",
            Self::NoFolder => "no folder was chosen",
        }
    }
}

/// What an operation stands for: it is admitted only while its origin is in front, its password
/// sheet hangs on the origin's window, and it is cancelled when the origin goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The terminal session in front of `window` when it started, by its local id and its stable
    /// sid (a replaced session reusing the local id is not the origin).
    Session {
        window: WindowId,
        session: u64,
        sid: String,
    },
    /// A page's view, its window's front tab when a person pressed the page's button.
    View { window: WindowId, view: ViewId },
}

impl Origin {
    /// The window the origin is in, which the password sheet hangs on.
    pub(crate) const fn window(&self) -> WindowId {
        match self {
            Self::Session { window, .. } | Self::View { window, .. } => *window,
        }
    }

    /// What a password sheet raised for it stands for.
    pub(crate) const fn sheet(&self) -> crate::askpass_sheet::SheetOrigin {
        match self {
            Self::Session { session, .. } => crate::askpass_sheet::SheetOrigin::Session(*session),
            Self::View { view, .. } => crate::askpass_sheet::SheetOrigin::View(*view),
        }
    }
}

pub(crate) struct Operation {
    id: u64,
    owner: Owner,
    origin: Origin,
    /// When a person's own click or key started it ([`Starter::Person`]); `None` for a control
    /// client's `sys policy collect`, or a key no person is known to stand behind.
    person_at: Option<Instant>,
    cancelled: AtomicBool,
    claims: Claims,
    retry_refused: AtomicBool,
    residual: Mutex<Option<Residual>>,
    has_residual: AtomicBool,
    reconciling: AtomicBool,
    sudo: Mutex<Option<Identity>>,
    ticket: Mutex<Option<Arc<crate::askpass::AskpassTicket>>>,
}

#[derive(Default)]
struct Claims {
    request: AtomicBool,
    secret: AtomicBool,
}
impl Claims {
    fn request(&self) -> bool {
        self.request
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn secret(&self) -> bool {
        self.request.load(Ordering::Acquire)
            && self
                .secret
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
    fn released(&self) -> bool {
        self.secret.load(Ordering::Acquire)
    }
}

/// Who started a collection ([`App::collect_cpu_policy`], [`App::collect_cpu_policy_from_view`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Starter {
    /// A person's own key in the CPU lens, or their own press of the System Resources page's
    /// Collect CPU Policy, at this moment.
    Person(Instant),
    /// A control client (`sys policy collect`), or a key or press no person is known to stand
    /// behind.
    Control,
}

impl Starter {
    const fn person_at(self) -> Option<Instant> {
        match self {
            Self::Person(at) => Some(at),
            Self::Control => None,
        }
    }
}

impl std::fmt::Debug for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuPolicyOperation")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct PipeState {
    done: AtomicBool,
    eof: AtomicBool,
}

struct Residual {
    child: aterm_uds::spawnfd::Child,
    collector: Option<Identity>,
    stdout: Arc<PipeState>,
    stderr: Arc<PipeState>,
    reader: Option<std::thread::JoinHandle<()>>,
    error_reader: Option<std::thread::JoinHandle<()>>,
    report: Value,
}

fn readers_finished(
    stdout: &PipeState,
    stderr: &PipeState,
    reader: &Option<std::thread::JoinHandle<()>>,
    error_reader: &Option<std::thread::JoinHandle<()>>,
) -> bool {
    stdout.done.load(Ordering::Acquire)
        && stderr.done.load(Ordering::Acquire)
        && reader.as_ref().is_none_or(|h| h.is_finished())
        && error_reader.as_ref().is_none_or(|h| h.is_finished())
}

/// The fixed collector never forks or closes/redirects its output descriptors.
/// Even a collector blocked before its hello retains both writers. Only real
/// EOF on both owned pipes, completed readers and reaped sudo prove an unbound
/// operation drained; a read error or wrapper exit alone cannot do so.
fn owned_pipes_drained(
    child_reaped: bool,
    stdout: &PipeState,
    stderr: &PipeState,
    reader: &Option<std::thread::JoinHandle<()>>,
    error_reader: &Option<std::thread::JoinHandle<()>>,
) -> bool {
    child_reaped
        && readers_finished(stdout, stderr, reader, error_reader)
        && stdout.eof.load(Ordering::Acquire)
        && stderr.eof.load(Ordering::Acquire)
}

fn owns_no_live_collector(
    collector: Option<Identity>,
    gone: bool,
    secret_released: bool,
    request_seen: bool,
    retry_refused: bool,
    unbound_pipes_drained: bool,
) -> bool {
    collector.is_some() && gone
        || collector.is_none()
            && ((request_seen && !secret_released) || retry_refused || unbound_pipes_drained)
}

/// Single worker continuation, never called on the App thread.
fn reconcile(op: &Operation) -> Option<Value> {
    let mut residual = op.residual.lock().unwrap_or_else(|e| e.into_inner());
    let r = residual.as_mut()?;
    let status = r.child.try_wait().ok().flatten();
    if status.is_none()
        || !readers_finished(&r.stdout, &r.stderr, &r.reader, &r.error_reader)
        || !owns_no_live_collector(
            r.collector,
            r.collector.is_some_and(gone),
            op.claims.released(),
            op.claims.request.load(Ordering::Acquire),
            op.retry_refused.load(Ordering::Acquire),
            owned_pipes_drained(
                status.is_some(),
                &r.stdout,
                &r.stderr,
                &r.reader,
                &r.error_reader,
            ),
        )
    {
        return None;
    }
    let mut r = residual.take()?;
    if let Some(reader) = r.reader.take() {
        let _ = reader.join();
    }
    if let Some(reader) = r.error_reader.take() {
        let _ = reader.join();
    }
    if let Some(report) = r.report.as_object_mut() {
        report.insert(
            "collector_exit_observed".into(),
            json!(r.collector.is_some()),
        );
        report.insert("operation_drained".into(), json!(true));
        report.insert("revision".into(), json!(2));
        report.insert(
            "state".into(),
            json!("cancelled_or_failed_collector_now_drained"),
        );
    }
    drop(residual);
    record_exit_status(&mut r.report, status);
    op.has_residual.store(false, Ordering::Release);
    Some(r.report)
}

/// `owner`'s operation, if the slot holds one of its.
fn owned(owner: Owner) -> Option<Arc<Operation>> {
    OPERATION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|op| op.owner == owner)
        .cloned()
}

fn request_reconcile(owner: Owner, proxy: &aterm_window::event_loop::EventLoopProxy<Wake>) {
    let Some(op) = owned(owner) else {
        return;
    };
    if !op.has_residual.load(Ordering::Acquire)
        || op
            .reconciling
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    let proxy = proxy.clone();
    let worker = Arc::clone(&op);
    if crate::qos::spawn_lane_thread(
        "aterm-cpu-policy-reap",
        crate::qos::Role::Responsive,
        move || {
            if let Some(report) = reconcile(&worker) {
                vacate(worker.id);
                let _ = proxy.send_event(Wake::CpuPolicyReport(Box::new(report)));
            }
            worker.reconciling.store(false, Ordering::Release);
        },
    )
    .is_err()
    {
        op.reconciling.store(false, Ordering::Release);
    }
}

impl Operation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(ticket) = self
            .ticket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            ticket.cancel();
        }
    }

    pub(crate) const fn origin(&self) -> &Origin {
        &self.origin
    }

    fn new(owner: Owner, origin: Origin, starter: Starter) -> Self {
        Self {
            id: crate::alert_keys::next_confirm_id(),
            owner,
            origin,
            person_at: starter.person_at(),
            cancelled: AtomicBool::new(false),
            claims: Claims::default(),
            retry_refused: AtomicBool::new(false),
            residual: Mutex::new(None),
            has_residual: AtomicBool::new(false),
            reconciling: AtomicBool::new(false),
            sudo: Mutex::new(None),
            ticket: Mutex::new(None),
        }
    }

    /// When a person's own key or press started this collection; `None` for a control client's.
    pub(crate) const fn person_at(&self) -> Option<Instant> {
        self.person_at
    }

    fn bound(&self) -> bool {
        !self.cancelled.load(Ordering::Acquire)
            && self
                .sudo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
    }

    /// Control/operation worker only: no process observation runs on the App thread.
    pub(crate) fn matches_live_sudo(&self) -> bool {
        let expected = *self.sudo.lock().unwrap_or_else(|e| e.into_inner());
        expected.is_some_and(|id| read_identity(id.pid) == Some(id))
            && !self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn release_secret(&self) -> bool {
        if !self.bound() {
            return false;
        }
        self.claims.secret()
    }
}

pub(crate) fn read_identity(pid: u32) -> Option<Identity> {
    let r = aterm_sysprobe::kproc::record_read(pid).ok()??;
    Some(Identity {
        pid,
        start_s: r.start_s,
        start_us: r.start_us,
    })
}

fn gone(identity: Identity) -> bool {
    // A refused/unread kernel observation never proves that the collector exited.
    match aterm_sysprobe::kproc::record_read(identity.pid) {
        Ok(None) => true,
        Ok(Some(r)) => r.start_s != identity.start_s || r.start_us != identity.start_us || r.zombie,
        Err(_) => false,
    }
}

/// An owned operation paired with its one password prompt.
pub(crate) type OwnedPrompt = (Arc<Operation>, Arc<crate::askpass::AskpassTicket>);

/// Called only after the ordinary owner/halt/relay and Apple sudo attestation checks.
pub(crate) fn request(sudo_pid: i32) -> Result<Option<OwnedPrompt>, ()> {
    let Some(operation) = OPERATION.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return Ok(None);
    };
    let expected = *operation.sudo.lock().unwrap_or_else(|e| e.into_inner());
    if expected.is_none_or(|id| Some(id.pid) != u32::try_from(sudo_pid).ok()) {
        return Ok(None);
    }
    if !operation.matches_live_sudo() {
        return Err(());
    }
    if !operation.claims.request() {
        operation.retry_refused.store(true, Ordering::Release);
        operation.cancel();
        return Err(());
    }
    let ticket = Arc::new(crate::askpass::AskpassTicket::new(
        crate::alert_keys::next_confirm_id(),
        Instant::now() + crate::askpass::ASKPASS_DEADLINE,
    ));
    *operation.ticket.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&ticket));
    Ok(Some((operation, ticket)))
}

/// Whether `owner` has an unsettled operation.
pub(crate) fn pending(owner: Owner) -> bool {
    owned(owner).is_some()
}

/// Cancel `owner`'s operation if `gone` names its origin.
fn cancel_where(owner: Owner, gone: impl FnOnce(&Origin) -> bool) {
    if let Some(op) = owned(owner)
        && gone(&op.origin)
    {
        op.cancel();
    }
}

/// Cancel `owner`'s operation whose origin is in `window`, a session's or a view's.
pub(crate) fn cancel_window(owner: Owner, window: WindowId) {
    cancel_where(owner, |origin| origin.window() == window);
}

/// Cancel `owner`'s operation the terminal session `session` started.
pub(crate) fn cancel_session(owner: Owner, session: u64) {
    cancel_where(
        owner,
        |origin| matches!(origin, Origin::Session { session: s, .. } if *s == session),
    );
}

/// Cancel `owner`'s operation the page's view `view` started.
pub(crate) fn cancel_view(owner: Owner, view: ViewId) {
    cancel_where(
        owner,
        |origin| matches!(origin, Origin::View { view: v, .. } if *v == view),
    );
}

/// The view whose operation of `owner`'s is pending, if a view started it: the host cancels it
/// once the view is not in front ([`App::withdraw_askpass_view`]).
pub(crate) fn pending_view(owner: Owner) -> Option<(WindowId, ViewId)> {
    match owned(owner).map(|op| (op.origin.clone(), op.cancelled.load(Ordering::Acquire))) {
        Some((Origin::View { window, view }, false)) => Some((window, view)),
        _ => None,
    }
}

fn issue_ack(
    mut admitted: impl FnMut() -> bool,
    mut cancelled: impl FnMut() -> bool,
    mut write: impl FnMut() -> bool,
) -> bool {
    admitted() && !cancelled() && write()
}

/// What refuses admission, the origin first.
const fn admission_refusal(origin_live: bool, halted: bool, relay_off: bool) -> Option<Refusal> {
    if !origin_live {
        Some(Refusal::NotInFront)
    } else if halted {
        Some(Refusal::Halted)
    } else if relay_off {
        Some(Refusal::RelayOff)
    } else {
        None
    }
}

fn origin_matches(expected: &str, actual: Option<&str>, still_hosted: bool) -> bool {
    still_hosted && actual == Some(expected)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Status,
    Collect,
    Cancel,
}

fn admit(scope: crate::control::Scope, words: &str) -> Result<Action, &'static str> {
    const USAGE: &str =
        "ERR usage: sys policy [status|collect|cancel] [--json] (aterm help sys cpu)\n";
    if scope != crate::control::Scope::Owner {
        return Err("ERR CPU policy authorization is owner-only\n");
    }
    let mut caller_seen = false;
    let mut arguments = Vec::new();
    for word in words.split_whitespace() {
        if word.starts_with("caller=") {
            if caller_seen || aterm_vitals::sys::SysQuery::parse(word).is_err() {
                return Err(USAGE);
            }
            // The ctl client adds attribution metadata to every sys request.
            // Authorization uses the resolved session, never this supplied ID.
            caller_seen = true;
        } else {
            arguments.push(word);
        }
    }
    match arguments.as_slice() {
        ["policy"] | ["policy", "status"] | ["policy", "status", "--json"] => Ok(Action::Status),
        ["policy", "collect"] | ["policy", "collect", "--json"] => Ok(Action::Collect),
        ["policy", "cancel"] | ["policy", "cancel", "--json"] => Ok(Action::Cancel),
        _ => Err(USAGE),
    }
}

pub(crate) fn control(
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
    scope: crate::control::Scope,
    session: u64,
    words: &str,
) -> String {
    let action = match admit(scope, words) {
        Ok(a) => a,
        Err(e) => return e.into(),
    };
    crate::control::control_media::call_main(proxy, |reply| Wake::CpuPolicyAction {
        session,
        action,
        reply,
    })
    .unwrap_or_else(|error| format!("ERR {error}\n"))
}

impl App {
    pub(crate) fn cpu_policy_action(&mut self, session: u64, action: Action) -> String {
        self.reconcile_cpu_policy();
        match action {
            Action::Status => {}
            Action::Collect => {
                let Some(window) = self.window_of_session(session) else {
                    return "ERR originating window is gone\n".into();
                };
                if self.focused_session_id(window) != Some(session)
                    || !self.is_visible_session(session)
                {
                    return "ERR focus the originating tab before authorizing diagnostics\n".into();
                }
                if let Err(refusal) = self.collect_cpu_policy(window, Starter::Control) {
                    return format!("ERR CPU policy not collected: {}\n", refusal.words());
                }
            }
            Action::Cancel => cancel_session(self.cpu_policy_owner(), session),
        }
        let body = if let Some(op) = owned(self.cpu_policy_owner()) {
            let mut body = json!({"operation":"cpu_policy","pending":true,"request_id":op.id,
                "cancel_requested":op.cancelled.load(Ordering::Acquire),
                "cause":"not_established"});
            if let Some(fields) = body.as_object_mut() {
                match &op.origin {
                    Origin::Session { session, .. } => {
                        fields.insert("originating_session".into(), json!(session));
                    }
                    Origin::View { view, .. } => {
                        fields.insert("originating_view".into(), json!(view.get()));
                    }
                }
            }
            body
        } else {
            json!({"operation":"cpu_policy","pending":false,"report":"sys report --json","cause":"not_established"})
        };
        aterm_control::wire::json_ok(&aterm_json::to_string(&body).unwrap_or_else(|_| "{}".into()))
    }

    /// Whether `view` is the front tab's of `window` and the window is not occluded: what
    /// [`App::view_on_screen`] asks, of one window.
    pub(crate) fn view_in_front(&self, window: WindowId, view: ViewId) -> bool {
        self.windows
            .get(&window)
            .is_some_and(|ws| !ws.occluded && crate::active_tab_shows(ws, |shown| shown == view))
    }

    /// Whether this App's CPU policy collection is pending.
    pub(crate) fn cpu_policy_pending(&self) -> bool {
        pending(self.cpu_policy_owner())
    }

    /// Whether the operation's origin is still in front: its session in its window and visible,
    /// or its view its window's front tab in a window that is not occluded; under the same halt
    /// and relay refusals either way.
    pub(crate) fn cpu_policy_ui_admission(&self, operation: &Operation) -> bool {
        self.cpu_policy_ui_refusal(operation).is_none()
    }

    /// What refuses [`Self::cpu_policy_ui_admission`], if anything does. Another App's
    /// operation has no origin in this one.
    fn cpu_policy_ui_refusal(&self, operation: &Operation) -> Option<Refusal> {
        let live = operation.owner == self.cpu_policy_owner()
            && !operation.cancelled.load(Ordering::Acquire)
            && match &operation.origin {
                Origin::Session {
                    window,
                    session,
                    sid,
                } => {
                    let same_session = {
                        let store = self.store.read().unwrap_or_else(|e| e.into_inner());
                        origin_matches(sid, store.by_local(*session).map(|s| s.sid.as_str()), true)
                    };
                    // End the store read before app_halt_refusal takes its own read lock.
                    // A queued writer must not turn recursive read acquisition into a UI stall.
                    same_session
                        && self.window_contains_session(*window, *session)
                        && self.is_visible_session(*session)
                }
                Origin::View { window, view } => self.view_in_front(*window, *view),
            };
        admission_refusal(
            live,
            crate::fabric::app_halt_refusal(&self.store, "askpass").is_some(),
            crate::askpass::relay_off(),
        )
    }

    pub(crate) fn cpu_policy_origin_live(&self, operation: &Operation) -> bool {
        operation.bound() && self.cpu_policy_ui_admission(operation)
    }

    pub(crate) fn cpu_policy_permit(&self, operation: &Operation) -> bool {
        self.cpu_policy_origin_live(operation)
    }

    /// One explicit action from the terminal session in front of `window`; repeated clicks
    /// while any generation is unsettled do nothing. With no terminal session in front (a page
    /// is) it is refused: a page collects from its view ([`Self::collect_cpu_policy_from_view`]).
    /// `starter` says whether a person's own key started it, which lets its password sheet past
    /// the spacing a sheet nobody touched set (`askpass::Backoff::refusal`).
    pub(crate) fn collect_cpu_policy(
        &mut self,
        window: WindowId,
        starter: Starter,
    ) -> Result<(), Refusal> {
        self.reconcile_cpu_policy();
        let session = self.focused_session_id(window).ok_or(Refusal::NoSession)?;
        let sid = self
            .store
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_local(session)
            .map(|s| s.sid.as_str().to_owned())
            .ok_or(Refusal::NoSession)?;
        self.start_cpu_policy(
            Origin::Session {
                window,
                session,
                sid,
            },
            starter,
        )
    }

    /// A press of Collect CPU Policy on the page's view `view` in `window`, by `starter` (a
    /// person's own, as the page's dispatch admits no other): the operation is bound to that
    /// view, admitted while it is its window's front tab, and cancelled when it is not
    /// ([`Self::withdraw_askpass_view`]).
    pub(crate) fn collect_cpu_policy_from_view(
        &mut self,
        window: WindowId,
        view: ViewId,
        starter: Starter,
    ) -> Result<(), Refusal> {
        self.reconcile_cpu_policy();
        self.start_cpu_policy(Origin::View { window, view }, starter)
    }

    /// A person's Cancel on the System Resources page: end this App's pending collection,
    /// whatever started it, and withdraw its password sheet. The page's pending line and its live
    /// Cancel stand for that one operation, so the press ends it; a cancel only lowers what runs,
    /// so no origin need be in front for it.
    pub(crate) fn cancel_cpu_policy_from_page(&mut self) {
        cancel_where(self.cpu_policy_owner(), |_| true);
        self.withdraw_askpass_cpu_policy();
    }

    /// Who starts a collection from the gesture being dispatched: a person, now, where a person
    /// stands behind it ([`App::gesture_unvouched`]); a control client otherwise (a control
    /// client's `hwkey` or `act`, an accessibility client's key, a relayed one).
    pub(crate) fn cpu_policy_starter(&self) -> Starter {
        if self.gesture_unvouched().is_none() {
            Starter::Person(Instant::now())
        } else {
            Starter::Control
        }
    }

    /// Every press asks first that a settled operation's residual be reaped, whether or not it
    /// then starts one.
    fn reconcile_cpu_policy(&self) {
        if let Some(proxy) = self.proxy.as_ref() {
            request_reconcile(self.cpu_policy_owner(), proxy);
        }
    }

    /// Start one operation for `origin`, by `starter`, if it is admitted and none is unsettled.
    fn start_cpu_policy(&mut self, origin: Origin, starter: Starter) -> Result<(), Refusal> {
        let operation = Arc::new(Operation::new(self.cpu_policy_owner(), origin, starter));
        if let Some(refusal) = self.cpu_policy_ui_refusal(&operation) {
            return Err(refusal);
        }
        let proxy = self.proxy.clone();
        // A headless test's App has no event loop: its operation stands in the slot, with no
        // worker and so no sudo, until it is cancelled and the test empties the slot.
        #[cfg(not(test))]
        let Some(proxy) = proxy else {
            return Err(Refusal::NoEventLoop);
        };
        {
            let mut active = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
            if active.is_some() {
                return Err(Refusal::Pending);
            }
            *active = Some(Arc::clone(&operation));
        }

        self.begin_cpu_policy_report(operation.id);
        #[cfg(test)]
        let Some(proxy) = proxy else {
            return Ok(());
        };
        let worker = Arc::clone(&operation);
        let spawn = crate::qos::spawn_lane_thread(
            "aterm-cpu-policy",
            crate::qos::Role::Responsive,
            move || {
                let (result, drained) = run(&worker, &proxy);
                if drained {
                    vacate(worker.id);
                }
                let _ = proxy.send_event(Wake::CpuPolicyReport(Box::new(result)));
            },
        );
        if let Err(error) = spawn {
            vacate(operation.id);
            self.accept_cpu_policy_report(status_report(
                &operation,
                &format!("worker_unavailable:{error}"),
                true,
            ));
        }
        Ok(())
    }

    pub(crate) fn cancel_cpu_policy(&mut self, window: WindowId) {
        cancel_window(self.cpu_policy_owner(), window);
    }
}

/// Empty the slot if it still holds the operation `id`.
fn vacate(id: u64) {
    let mut active = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
    if active.as_ref().is_some_and(|op| op.id == id) {
        *active = None;
    }
}

/// The operation in the slot, for a test to read.
#[cfg(test)]
pub(crate) fn active_for_test() -> Option<Arc<Operation>> {
    OPERATION.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
impl Operation {
    /// Whether it was cancelled.
    pub(crate) fn cancelled_for_test(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Bind it to `sudo` as the worker binds the `sudo` it spawned, so its sheet may be raised.
    pub(crate) fn bind_sudo_for_test(&self, sudo: Identity) {
        *self.sudo.lock().unwrap_or_else(|e| e.into_inner()) = Some(sudo);
    }
}

/// The one operation slot, held by one test at a time and empty while it is: tests that start an
/// operation take it, since the slot is the process's.
#[cfg(test)]
pub(crate) struct SlotForTest {
    _held: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
pub(crate) fn slot_for_test() -> SlotForTest {
    static HELD: Mutex<()> = Mutex::new(());
    let held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    SlotForTest { _held: held }
}

#[cfg(test)]
impl SlotForTest {
    /// Empty the slot, as a worker does once its operation settles.
    pub(crate) fn empty(&self) {
        *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[cfg(test)]
impl Drop for SlotForTest {
    fn drop(&mut self) {
        *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn record_exit_status(report: &mut Value, status: Option<std::process::ExitStatus>) {
    use std::os::unix::process::ExitStatusExt;
    let Some(fields) = report.as_object_mut() else {
        return;
    };
    if let Some(status) = status {
        if let Some(code) = status.code() {
            fields.insert("sudo_exit_code".into(), json!(code));
            fields.insert(
                "exit_code_interpretation".into(),
                json!(match code {
                    aterm_sysprobe::cpu_policy::DEADLINE_EXIT => "collector_deadline_code",
                    aterm_sysprobe::cpu_policy::PARENT_LOSS_EXIT => "collector_parent_loss_code",
                    _ => "observed_exit_code",
                }),
            );
        } else if let Some(signal) = status.signal() {
            fields.insert("sudo_exit_signal".into(), json!(signal));
        } else {
            fields.insert("sudo_exit_unavailable".into(), json!(true));
        }
    }
}

fn status_report(op: &Operation, state: &str, drained: bool) -> Value {
    json!({"schema": 1, "operation": "cpu_policy", "request_id": op.id,
        "state": state, "revision": if drained {2} else {1}, "operation_drained": drained, "collector_exit_observed": false,
        "observed_unix_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .ok().and_then(|v| u64::try_from(v.as_millis()).ok()).unwrap_or(0),
        "cause": "not_established", "coverage": "no_policy_observations"})
}

pub(crate) fn fixed_paths() -> Result<(PathBuf, PathBuf), String> {
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let helper = exe
        .parent()
        .ok_or("binary has no parent")?
        .join("aterm-askpass");
    if helper.canonicalize().map_err(|e| e.to_string())? != exe {
        return Err("installed askpass alias does not resolve to this executable".into());
    }
    Ok((exe, helper))
}

/// The suspended `sudo` of the fixed collector, whatever the origin. Its askpass helper asks
/// this instance by its own control socket, the one whose operation stands for this `sudo`: no
/// inherited session id may route it to another aterm, which would hold no operation for it.
fn sudo_command(exe: &Path, helper: &Path, sock: &str) -> aterm_uds::spawnfd::Command {
    let mut command = aterm_uds::spawnfd::command("/usr/bin/sudo");
    command
        .args(SUDO_ARGS)
        .arg(exe)
        .arg(COLLECTOR_VERB)
        .env("SUDO_ASKPASS", helper)
        .env(aterm_uds::sudo::ASKPASS_SOCK_ENV, sock)
        .env_remove("ATERM_PARENT_SESSION_ID")
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .start_suspended(true);
    command
}

fn run(
    op: &Arc<Operation>,
    proxy: &aterm_window::event_loop::EventLoopProxy<Wake>,
) -> (Value, bool) {
    let (exe, helper) = match fixed_paths() {
        Ok(paths) => paths,
        Err(error) => {
            return (
                status_report(op, &format!("unavailable:{error}"), true),
                true,
            );
        }
    };
    let Some(sock) = crate::proxy::self_sock_path() else {
        return (
            status_report(op, "unavailable:this instance has no control socket", true),
            true,
        );
    };
    let mut command = sudo_command(&exe, &helper, &sock);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return (
                status_report(op, &format!("spawn_refused:{error}"), true),
                true,
            );
        }
    };
    // Spawn suspended: no sudo instruction can ask for a password before this binding exists.
    let Some(sudo) = read_identity(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return (status_report(op, "sudo_identity_unavailable", true), true);
    };
    *op.sudo.lock().unwrap_or_else(|e| e.into_inner()) = Some(sudo);
    if let Err(error) = child.resume() {
        let _ = child.kill();
        let _ = child.wait();
        return (
            status_report(op, &format!("resume_refused:{error}"), true),
            true,
        );
    }
    let mut input = child.stdin.take();
    let Some(output) = child.stdout.take() else {
        unreachable!("requested piped stdout")
    };
    let Some(error_output) = child.stderr.take() else {
        unreachable!("requested piped stderr")
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout_state = Arc::new(PipeState::default());
    let output_state = Arc::clone(&stdout_state);
    let reader = crate::qos::spawn_lane_thread(
        "aterm-cpu-policy-output",
        crate::qos::Role::Responsive,
        move || {
            read_output(output, &tx, &output_state, OUTPUT_LIMIT);
        },
    );
    let stderr_state = Arc::new(PipeState::default());
    let error_state = Arc::clone(&stderr_state);
    let stderr_tail = Arc::new(Mutex::new(Vec::new()));
    let error_tail = Arc::clone(&stderr_tail);
    let stderr_reader = crate::qos::spawn_lane_thread(
        "aterm-cpu-policy-errors",
        crate::qos::Role::Responsive,
        move || {
            let mut reader = error_output;
            let mut kept = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        error_state.eof.store(true, Ordering::Release);
                        break;
                    }
                    Err(_) => break,
                    Ok(n) => {
                        let room = 16_384usize.saturating_sub(kept.len());
                        kept.extend_from_slice(&buffer[..n.min(room)]);
                    }
                }
            }
            *error_tail.lock().unwrap_or_else(|e| e.into_inner()) = kept;
            error_state.done.store(true, Ordering::Release);
        },
    );
    if reader.is_err() {
        stdout_state.done.store(true, Ordering::Release);
        op.cancel();
    }
    if stderr_reader.is_err() {
        stderr_state.done.store(true, Ordering::Release);
        op.cancel();
    }
    let mut reader = reader.ok();
    let mut stderr_reader = stderr_reader.ok();
    let started = Instant::now();
    let mut collector = None;
    let mut collector_parent = 0;
    let mut capture = None;
    let mut failure = None;
    let mut status = None;
    let mut cancellation_at = None;
    let mut output_messages_drained = false;

    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(value)) if collector.is_none() => match bind_collector(&value, sudo, &exe) {
                Some(identity) => {
                    collector = Some(identity);
                    collector_parent = value["parent_pid"].as_u64().unwrap_or(0) as u32;
                    let (permit_tx, permit_rx) = std::sync::mpsc::channel();
                    if !issue_ack(
                        || {
                            proxy
                                .send_event(Wake::CpuPolicyPermit {
                                    operation: Arc::clone(op),
                                    reply: permit_tx.clone(),
                                })
                                .is_ok()
                                && permit_rx.recv_timeout(Duration::from_secs(3)) == Ok(true)
                        },
                        || op.cancelled.load(Ordering::Acquire),
                        || input.as_mut().is_some_and(|i| i.write_all(&[1]).is_ok()),
                    ) {
                        op.cancel();
                    }
                }
                None => {
                    failure = Some("collector_identity_refused");
                    op.cancel();
                }
            },
            Ok(Ok(value)) => {
                if collector.is_some_and(|id| capture_matches(&value, id, collector_parent))
                    && capture.is_none()
                {
                    capture = Some(value);
                } else {
                    failure = Some("collector_capture_refused");
                    op.cancel();
                }
            }
            Ok(Err(why)) => {
                failure = Some(why);
                op.cancel();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Disconnection is observed only after every queued hello,
                // capture and error was received. Finished readers alone do
                // not prove that their queued capture has been consumed.
                output_messages_drained = true;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(_) => {
                    failure = Some("sudo_wait_unavailable");
                    op.cancel();
                }
            }
        }
        if started.elapsed() >= crate::askpass::ASKPASS_DEADLINE + EXIT_GRACE {
            op.cancel();
        }
        if op.cancelled.load(Ordering::Acquire) {
            input.take(); // EOF is the collector's intrinsic cancellation path.
            cancellation_at.get_or_insert_with(Instant::now);
        }
        let collector_drained = collector.is_some_and(gone);
        let never_authorized = !op.claims.released();
        let no_collector = owns_no_live_collector(
            collector,
            collector_drained,
            !never_authorized,
            op.claims.request.load(Ordering::Acquire),
            op.retry_refused.load(Ordering::Acquire),
            owned_pipes_drained(
                status.is_some(),
                &stdout_state,
                &stderr_state,
                &reader,
                &stderr_reader,
            ),
        );
        if status.is_some()
            && output_messages_drained
            && readers_finished(&stdout_state, &stderr_state, &reader, &stderr_reader)
            && no_collector
        {
            let mut report = status_report(
                op,
                if capture
                    .as_ref()
                    .and_then(aterm_sysprobe::cpu_policy::decode_capture)
                    .is_some_and(|c| c.complete())
                    && failure.is_none()
                    && !op.cancelled.load(Ordering::Acquire)
                    && stdout_state.eof.load(Ordering::Acquire)
                    && stderr_state.eof.load(Ordering::Acquire)
                    && status.is_some_and(|s| s.success())
                {
                    "observed"
                } else if never_authorized {
                    "authorization_declined_or_unavailable"
                } else {
                    "partial_or_failed"
                },
                true,
            );
            if let Some(object) = report.as_object_mut() {
                if let Some(capture) = capture {
                    object.insert("capture".into(), capture);
                }
                if let Some(failure) = failure {
                    object.insert("error".into(), json!(failure));
                }
                object.insert(
                    "cancelled".into(),
                    json!(op.cancelled.load(Ordering::Acquire)),
                );
                object.insert(
                    "sudo_success".into(),
                    json!(status.is_some_and(|s| s.success())),
                );
            }
            if let Some(object) = report.as_object_mut() {
                object.insert(
                    "collector_exit_observed".into(),
                    json!(collector.is_some() && collector_drained),
                );
                object.insert(
                    "stdout_eof".into(),
                    json!(stdout_state.eof.load(Ordering::Acquire)),
                );
                object.insert(
                    "stderr_eof".into(),
                    json!(stderr_state.eof.load(Ordering::Acquire)),
                );
                object.insert(
                    "stderr".into(),
                    json!(crate::askpass::escape_display(&String::from_utf8_lossy(
                        &stderr_tail.lock().unwrap_or_else(|e| e.into_inner())
                    ))),
                );
            }
            record_exit_status(&mut report, status);
            // is_finished() above proves these joins cannot wait for a pipe writer.
            if let Some(reader) = reader.take() {
                let _ = reader.join();
            }
            if let Some(reader) = stderr_reader.take() {
                let _ = reader.join();
            }
            return (report, true);
        }
        if cancellation_at.is_some_and(|at| at.elapsed() >= EXIT_GRACE) {
            // Do not declare the root collector gone because its sudo wrapper exited.
            // Keep the operation slot occupied, so resampling cannot start another one.
            let mut report = status_report(op, "cancellation_unresolved", false);
            if let Some(object) = report.as_object_mut()
                && let Some(c) = collector
            {
                object.insert("collector_pid".into(), json!(c.pid));
                object.insert("collector_start_s".into(), json!(c.start_s.to_string()));
                object.insert("collector_start_us".into(), json!(c.start_us));
            }
            if let Some(object) = report.as_object_mut() {
                if let Some(capture) = capture {
                    object.insert("capture".into(), capture);
                }
                if let Some(failure) = failure {
                    object.insert("error".into(), json!(failure));
                }
            }
            record_exit_status(&mut report, status);
            *op.residual.lock().unwrap_or_else(|e| e.into_inner()) = Some(Residual {
                child,
                collector,
                stdout: stdout_state,
                stderr: stderr_state,
                reader,
                error_reader: stderr_reader,
                report: report.clone(),
            });
            op.has_residual.store(true, Ordering::Release);
            return (report, false);
        }
    }
}

fn read_output(
    mut output: impl Read,
    tx: &std::sync::mpsc::Sender<Result<Value, &'static str>>,
    state: &PipeState,
    limit: usize,
) {
    let mut chunk = [0; 4096];
    let mut line = Vec::new();
    let mut total = 0usize;
    let mut exceeded = false;
    loop {
        match output.read(&mut chunk) {
            Ok(0) => {
                state.eof.store(true, Ordering::Release);
                break;
            }
            Ok(n) => {
                total = total.saturating_add(n);
                if total > limit {
                    if !exceeded {
                        let _ = tx.send(Err("collector_output_limit"));
                    }
                    exceeded = true;
                    line.clear();
                    continue;
                }
                if exceeded {
                    continue;
                }
                for byte in &chunk[..n] {
                    if *byte == b'\n' {
                        let _ = tx.send(
                            aterm_json::from_slice::<Value>(&line)
                                .map_err(|_| "collector_output_invalid"),
                        );
                        line.clear();
                    } else {
                        line.push(*byte);
                    }
                }
            }
            Err(_) => {
                let _ = tx.send(Err("collector_output_unread"));
                break;
            }
        }
    }
    if !line.is_empty() {
        let _ = tx.send(Err("collector_output_incomplete"));
    }
    state.done.store(true, Ordering::Release);
}

fn parsed_identity(value: &Value, pid: &str, sec: &str, usec: &str) -> Option<Identity> {
    let sec_str = value[sec].as_str()?;
    (!sec_str.is_empty() && sec_str.bytes().all(|b| b.is_ascii_digit())).then_some(())?;
    let id = Identity {
        pid: u32::try_from(value[pid].as_u64()?).ok()?,
        start_s: sec_str.parse().ok()?,
        start_us: u32::try_from(value[usec].as_u64()?).ok()?,
    };
    (id.pid > 0 && id.start_s > 0 && id.start_us < 1_000_000).then_some(id)
}

fn bind_collector(value: &Value, sudo: Identity, exe: &std::path::Path) -> Option<Identity> {
    if value["schema"].as_u64() != Some(1)
        || value["phase"].as_str() != Some("collector_started")
        || value["effective_uid"].as_u64() != Some(0)
    {
        return None;
    }
    let id = parsed_identity(value, "pid", "start_s", "start_us")?;
    let record = aterm_sysprobe::kproc::record_read(id.pid).ok()??;
    if read_identity(id.pid) != Some(id)
        || record.uid != 0
        || value["parent_pid"].as_u64() != Some(u64::from(record.ppid))
    {
        return None;
    }
    let parent = if record.ppid == sudo.pid {
        sudo
    } else {
        // sudo may use its fixed monitor process between itself and the command.
        let monitor = aterm_sysprobe::kproc::record_read(record.ppid).ok()??;
        if monitor.ppid != sudo.pid
            || aterm_uds::sudo::attest_sudo(monitor.pid.cast_signed()).is_err()
        {
            return None;
        }
        Identity {
            pid: monitor.pid,
            start_s: monitor.start_s,
            start_us: monitor.start_us,
        }
    };
    if read_identity(sudo.pid) != Some(sudo)
        || read_identity(parent.pid) != Some(parent)
        || aterm_uds::sudo::exe_path(id.pid.cast_signed())?
            .canonicalize()
            .ok()?
            .as_path()
            != exe
    {
        return None;
    }
    Some(id)
}

fn capture_matches(value: &Value, id: Identity, parent_pid: u32) -> bool {
    aterm_sysprobe::cpu_policy::decode_capture(value).is_some_and(|capture| {
        capture.collector == id && capture.parent_pid == parent_pid && capture.effective_uid == 0
    })
}

#[cfg(test)]
pub(crate) fn terminal_report_for_test(status: std::process::ExitStatus) -> Value {
    let operation = Operation {
        id: 1,
        ..Operation::new(
            Owner::next(),
            Origin::Session {
                window: WindowId(1),
                session: 1,
                sid: "test-origin".into(),
            },
            Starter::Control,
        )
    };
    let mut report = status_report(&operation, "partial_or_failed", true);
    record_exit_status(&mut report, Some(status));
    report
}

/// `owner`'s operation for `session` in `window`, whose store sid is `sid`, started by
/// `starter`, its sudo already bound (to a pid no test signals), as a collection's is once that
/// sudo starts.
#[cfg(test)]
pub(crate) fn bound_operation_for_test(
    owner: Owner,
    window: WindowId,
    session: u64,
    sid: &str,
    starter: Starter,
) -> Arc<Operation> {
    let operation = Operation::new(
        owner,
        Origin::Session {
            window,
            session,
            sid: sid.to_owned(),
        },
        starter,
    );
    operation.bind_sudo_for_test(Identity {
        pid: u32::MAX,
        start_s: 0,
        start_us: 0,
    });
    Arc::new(operation)
}

#[cfg(test)]
#[path = "cpu_policy_authorization_tests.rs"]
mod tests;
