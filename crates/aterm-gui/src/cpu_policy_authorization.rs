// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One app-owned, read-only operation through the existing protected askpass sheet.
//! No executable, output path, policy selector or command text comes from the caller.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use crate::{App, Wake, WindowId};
use aterm_json::{Value, json};
use aterm_sysprobe::cpu_policy::{COLLECTOR_VERB, Identity};

const OUTPUT_LIMIT: usize = 3 * 1024 * 1024;
const EXIT_GRACE: Duration = Duration::from_secs(15);
const SUDO_ARGS: [&str; 3] = ["-k", "-A", "--"];
static OPERATION: Mutex<Option<Arc<Operation>>> = Mutex::new(None);

pub(crate) struct Operation {
    id: u64,
    window: WindowId,
    session: u64,
    sid: String,
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

fn request_reconcile(proxy: &aterm_window::event_loop::EventLoopProxy<Wake>) {
    let Some(op) = OPERATION.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
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
                let mut active = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
                if active.as_ref().is_some_and(|op| op.id == worker.id) {
                    *active = None;
                }
                drop(active);
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

    pub(crate) fn origin(&self) -> (WindowId, u64) {
        (self.window, self.session)
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

fn read_identity(pid: u32) -> Option<Identity> {
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

pub(crate) fn pending() -> bool {
    OPERATION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

pub(crate) fn cancel_window(window: WindowId) {
    if let Some(op) = OPERATION.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && op.window == window
    {
        op.cancel();
    }
}

pub(crate) fn cancel_session(session: u64) {
    if let Some(op) = OPERATION.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && op.session == session
    {
        op.cancel();
    }
}

fn issue_ack(
    mut admitted: impl FnMut() -> bool,
    mut cancelled: impl FnMut() -> bool,
    mut write: impl FnMut() -> bool,
) -> bool {
    admitted() && !cancelled() && write()
}

fn admission_allowed(origin_live: bool, halted: bool, relay_off: bool) -> bool {
    origin_live && !halted && !relay_off
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
    const USAGE: &str = "ERR usage: sys policy [status|collect|cancel] [--json]\n";
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
        if let Some(proxy) = self.proxy.as_ref() {
            request_reconcile(proxy);
        }
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
                if !self.collect_cpu_policy(window) {
                    return "ERR CPU policy operation pending or unavailable\n".into();
                }
            }
            Action::Cancel => cancel_session(session),
        }
        let active = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
        let body = if let Some(op) = active.as_ref() {
            json!({"operation":"cpu_policy","pending":true,"request_id":op.id,
                "originating_session":op.session,"cancel_requested":op.cancelled.load(Ordering::Acquire),
                "cause":"not_established"})
        } else {
            json!({"operation":"cpu_policy","pending":false,"report":"sys report --json","cause":"not_established"})
        };
        aterm_control::wire::json_ok(&aterm_json::to_string(&body).unwrap_or_else(|_| "{}".into()))
    }

    fn cpu_policy_ui_admission(&self, operation: &Operation) -> bool {
        let same_session = {
            let store = self.store.read().unwrap_or_else(|e| e.into_inner());
            origin_matches(
                &operation.sid,
                store.by_local(operation.session).map(|s| s.sid.as_str()),
                true,
            )
        };
        // End the store read before app_halt_refusal takes its own read lock.
        // A queued writer must not turn recursive read acquisition into a UI stall.
        admission_allowed(
            !operation.cancelled.load(Ordering::Acquire)
                && same_session
                && self.window_contains_session(operation.window, operation.session)
                && self.is_visible_session(operation.session),
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

    /// One explicit action; repeated clicks while any generation is unsettled do nothing.
    pub(crate) fn collect_cpu_policy(&mut self, window: WindowId) -> bool {
        if let Some(proxy) = self.proxy.as_ref() {
            request_reconcile(proxy);
        }
        let Some(session) = self.focused_session_id(window) else {
            return false;
        };
        let Some(proxy) = self.proxy.clone() else {
            return false;
        };
        let Some(sid) = self
            .store
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_local(session)
            .map(|s| s.sid.as_str().to_owned())
        else {
            return false;
        };
        let operation = Arc::new(Operation {
            id: crate::alert_keys::next_confirm_id(),
            window,
            session,
            sid,
            cancelled: AtomicBool::new(false),
            claims: Claims::default(),
            retry_refused: AtomicBool::new(false),
            residual: Mutex::new(None),
            has_residual: AtomicBool::new(false),
            reconciling: AtomicBool::new(false),
            sudo: Mutex::new(None),
            ticket: Mutex::new(None),
        });
        if !self.cpu_policy_ui_admission(&operation) {
            return false;
        }
        {
            let mut active = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
            if active.is_some() {
                return false;
            }
            *active = Some(Arc::clone(&operation));
        }

        self.begin_cpu_policy_report(operation.id);
        let worker = Arc::clone(&operation);
        let spawn = crate::qos::spawn_lane_thread(
            "aterm-cpu-policy",
            crate::qos::Role::Responsive,
            move || {
                let (result, drained) = run(&worker, &proxy);
                if drained {
                    let mut active = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
                    if active.as_ref().is_some_and(|o| o.id == worker.id) {
                        *active = None;
                    }
                }
                let _ = proxy.send_event(Wake::CpuPolicyReport(Box::new(result)));
            },
        );
        if let Err(error) = spawn {
            *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.accept_cpu_policy_report(status_report(
                &operation,
                &format!("worker_unavailable:{error}"),
                true,
            ));
        }
        true
    }

    pub(crate) fn cancel_cpu_policy(&mut self, window: WindowId) {
        cancel_window(window);
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

fn fixed_paths() -> Result<(PathBuf, PathBuf), String> {
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
    let mut command = aterm_uds::spawnfd::command("/usr/bin/sudo");
    command
        .args(SUDO_ARGS)
        .arg(&exe)
        .arg(COLLECTOR_VERB)
        .env("SUDO_ASKPASS", helper)
        .env("ATERM_PARENT_SESSION_ID", &op.sid)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .start_suspended(true);
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
    let id = Identity {
        pid: u32::try_from(value[pid].as_u64()?).ok()?,
        start_s: value[sec].as_str()?.parse().ok()?,
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
        window: WindowId(1),
        session: 1,
        sid: "test-origin".into(),
        cancelled: AtomicBool::new(false),
        claims: Claims::default(),
        retry_refused: AtomicBool::new(false),
        residual: Mutex::new(None),
        has_residual: AtomicBool::new(false),
        reconciling: AtomicBool::new(false),
        sudo: Mutex::new(None),
        ticket: Mutex::new(None),
    };
    let mut report = status_report(&operation, "partial_or_failed", true);
    record_exit_status(&mut report, Some(status));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_policy_fixed_sudo_arguments_reach_authentication_without_a_prompt() {
        let mut command = aterm_uds::spawnfd::command("/usr/bin/sudo");
        command
            .args(SUDO_ARGS.map(|arg| if arg == "-A" { "-n" } else { arg }))
            .arg("/usr/bin/true")
            .env("LC_ALL", "C");
        let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60))
            .expect("fixed noninteractive sudo invocation completed");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !error.contains("usage:") && !error.contains("Only one of"),
            "sudo rejected its fixed arguments: {error}"
        );
        assert!(
            output.status.success()
                || error.contains("a password is required")
                || error.contains("not allowed to execute")
                || error.contains("not in the sudoers file")
                || error.contains("authentication failure"),
            "sudo did not reach command authorization: {error}"
        );
    }

    #[test]
    fn cpu_policy_no_askpass_ack_cannot_bypass_current_admission() {
        let claims = Claims::default();
        assert!(!claims.released());
        let writes = std::cell::Cell::new(0);
        for (live, halted, off) in [
            (false, false, false),
            (true, true, false),
            (true, false, true),
        ] {
            assert!(!issue_ack(
                || admission_allowed(live, halted, off),
                || false,
                || {
                    writes.set(writes.get() + 1);
                    true
                }
            ));
        }
        assert_eq!(writes.get(), 0, "NOPASSWD still cannot reach the ACK write");
        assert!(!issue_ack(
            || true,
            || true,
            || {
                writes.set(writes.get() + 1);
                true
            }
        ));
        assert_eq!(writes.get(), 0, "cancellation after permit forbids ACK");
        assert!(issue_ack(
            || true,
            || false,
            || {
                writes.set(writes.get() + 1);
                true
            }
        ));
        assert_eq!(writes.get(), 1);
    }

    #[test]
    fn cpu_policy_no_askpass_still_requires_live_origin_halt_and_relay_admission() {
        assert!(admission_allowed(true, false, false));
        assert!(!admission_allowed(false, false, false));
        assert!(!admission_allowed(true, true, false));
        assert!(!admission_allowed(true, false, true));
        assert!(
            !owns_no_live_collector(None, false, false, false, false, false),
            "NOPASSWD may start a collector without any secret"
        );
    }

    #[test]
    fn cpu_policy_control_is_owner_only_and_has_no_command_or_path_payload() {
        use crate::control::Scope;
        assert_eq!(admit(Scope::Owner, "policy"), Ok(Action::Status));
        assert_eq!(
            admit(Scope::Owner, "policy collect --json"),
            Ok(Action::Collect)
        );
        assert_eq!(admit(Scope::Owner, "policy cancel"), Ok(Action::Cancel));
        assert!(admit(Scope::Bridge, "policy collect").is_err());
        for (words, action) in [
            ("policy caller=s-origin_1", Action::Status),
            ("policy status caller=s-origin_1 --json", Action::Status),
            ("policy status --json caller=s-origin_1", Action::Status),
            ("policy collect caller=s-origin_1 --json", Action::Collect),
            ("policy collect --json caller=s-origin_1", Action::Collect),
            ("policy cancel caller=s-origin_1 --json", Action::Cancel),
            ("policy cancel --json caller=s-origin_1", Action::Cancel),
        ] {
            assert_eq!(admit(Scope::Owner, words), Ok(action), "{words}");
            assert!(admit(Scope::Bridge, words).is_err(), "{words}");
        }
        for arg in [
            "policy collect /bin/sh",
            "policy collect path=/tmp/report",
            "policy collect selector=7",
            "policy collect instances=all",
            "policy collect --force",
            "policy collect caller=",
            "policy collect caller=s/other",
            "policy collect caller=a%20b",
            "policy collect caller=s-1 caller=s-2",
            "policy collect caller=s-1 --json caller=s-1",
        ] {
            assert!(admit(Scope::Owner, arg).is_err(), "{arg}");
        }
        let boundary = format!("policy collect caller={}", "a".repeat(64));
        assert_eq!(admit(Scope::Owner, &boundary), Ok(Action::Collect));
        let too_long = format!("policy collect caller={}", "a".repeat(65));
        assert!(admit(Scope::Owner, &too_long).is_err());
    }

    #[test]
    fn cpu_policy_closed_or_replaced_origin_cannot_authorize() {
        assert!(origin_matches("origin", Some("origin"), true));
        assert!(!origin_matches("origin", Some("replacement"), true));
        assert!(!origin_matches("origin", Some("origin"), false));
        assert!(!origin_matches("origin", None, true));
    }

    #[test]
    fn cpu_policy_request_and_secret_are_each_claimed_once() {
        let claims = Arc::new(Claims::default());
        assert!(!claims.secret(), "no unrequested secret release");
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let claims = Arc::clone(&claims);
                std::thread::spawn(move || claims.request())
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
        assert!(claims.secret());
        assert!(!claims.secret());
        assert!(
            !claims.request(),
            "sudo password retries do not create another consent"
        );
    }

    #[test]
    fn cpu_policy_wrapper_exit_does_not_prove_collector_exit() {
        let id = Identity {
            pid: 1,
            start_s: 100,
            start_us: 0,
        };
        assert!(!owns_no_live_collector(
            Some(id),
            false,
            true,
            true,
            false,
            false
        ));
        assert!(owns_no_live_collector(
            Some(id),
            true,
            true,
            true,
            false,
            false
        ));
        assert!(!owns_no_live_collector(
            None, false, true, true, false, false
        ));
        assert!(owns_no_live_collector(
            None, false, false, true, false, false
        ));
        assert!(
            owns_no_live_collector(None, false, true, true, true, false),
            "a refused retry occurs before sudo can execute the collector"
        );
        let stdout = PipeState::default();
        let stderr = PipeState::default();
        stdout.done.store(true, Ordering::Release);
        assert!(
            !readers_finished(&stdout, &stderr, &None, &None),
            "stderr may still have a writer"
        );
        stderr.done.store(true, Ordering::Release);
        assert!(readers_finished(&stdout, &stderr, &None, &None));
        assert!(
            !stderr.eof.load(Ordering::Acquire),
            "completion and true EOF remain distinct"
        );
    }

    #[test]
    fn cpu_policy_unbound_sudo_refusal_requires_reaping_and_true_owned_pipe_eof() {
        let stdout = PipeState::default();
        let stderr = PipeState::default();
        let drained = || owned_pipes_drained(true, &stdout, &stderr, &None, &None);
        assert!(!drained(), "wrapper exit alone leaves an unknown collector");
        stdout.done.store(true, Ordering::Release);
        stderr.done.store(true, Ordering::Release);
        assert!(!drained(), "read errors may finish without EOF");
        stdout.eof.store(true, Ordering::Release);
        assert!(!drained(), "one pipe still has an unknown writer");
        stderr.eof.store(true, Ordering::Release);
        assert!(!owned_pipes_drained(false, &stdout, &stderr, &None, &None));
        assert!(drained());
        assert!(
            owns_no_live_collector(None, false, false, false, false, drained()),
            "sudo can refuse before requesting any password"
        );
        assert!(
            owns_no_live_collector(None, false, true, true, false, drained()),
            "an authorized collector may exit before its hello"
        );
        let id = Identity {
            pid: 1,
            start_s: 100,
            start_us: 0,
        };
        assert!(
            !owns_no_live_collector(Some(id), false, true, true, false, drained()),
            "a bound generation still needs its independent exit observation"
        );
    }

    #[test]
    fn cpu_policy_output_limit_and_partial_line_do_not_forge_eof_or_capture() {
        let (tx, rx) = std::sync::mpsc::channel();
        let state = PipeState::default();
        read_output(
            std::io::Cursor::new(b"{\"x\":1}\n{\"x\":2}\n"),
            &tx,
            &state,
            9,
        );
        assert!(state.eof.load(Ordering::Acquire));
        assert!(state.done.load(Ordering::Acquire));
        assert_eq!(rx.try_recv().unwrap(), Err("collector_output_limit"));
        assert!(rx.try_recv().is_err());
        let (tx, rx) = std::sync::mpsc::channel();
        let state = PipeState::default();
        read_output(std::io::Cursor::new(b"{\"x\":1}"), &tx, &state, 100);
        assert_eq!(rx.try_recv().unwrap(), Err("collector_output_incomplete"));
    }

    #[test]
    fn cpu_policy_identity_parser_rejects_missing_reused_and_bad_generation() {
        let value = json!({"pid":1,"sec":"100","usec":5});
        assert_eq!(
            parsed_identity(&value, "pid", "sec", "usec"),
            Some(Identity {
                pid: 1,
                start_s: 100,
                start_us: 5
            })
        );
        assert!(
            parsed_identity(&json!({"pid":0,"sec":"100","usec":5}), "pid", "sec", "usec").is_none()
        );
        assert!(
            parsed_identity(&json!({"pid":1,"sec":"0","usec":5}), "pid", "sec", "usec").is_none()
        );
        assert!(
            parsed_identity(
                &json!({"pid":1,"sec":"100","usec":1000000}),
                "pid",
                "sec",
                "usec"
            )
            .is_none()
        );
    }
}
