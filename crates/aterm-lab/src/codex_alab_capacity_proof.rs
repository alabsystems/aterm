// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CAPACITY LANE (`codex-alab-proof --capacity-only`, and the full proof's last leg):
//! codex-alab's headless capacity and account-quota recovery (its `alab/CAPACITY_RECOVERY.md`,
//! and its `alab-core` glue `responses_quota_retry.rs`) on the REAL package's `codex exec`, in
//! the proof's jail ([`super::policy`]: one loopback port, the owner's homes fenced), against a
//! loopback model of this lane's own. Every case codex-alab's standalone smoke ran (its
//! `alab/smoke/capacity_exec.py` as of codex-alab `767a333a16`, which every `capacity_exec.py`
//! below names: its default run, `--quota`, `--quota-no-reset` and `--quota-terminals`), by its
//! name ([`CASES`]); that run had `codex exec` reach its model with the Internet open, and this
//! lane replaces it:
//!
//! - the package's records agree (version, target, an exact source commit, `bin/codex` inside
//!   the package), `codex exec --help` names the flags this lane passes, and `codex --version`
//!   says the package's version;
//! - RECOVERY (`capacity-recovery`, `quota-reset-recovery`, `quota-no-reset-recovery`): the
//!   model answers the first request with an `update_plan` call, the second (carrying that
//!   call's output) with the case's wall and `Retry-After: 2`, the third with the reply. One
//!   native turn, three requests, the model and the person's prompt unchanged in each, the
//!   retry's input exactly the walled request's (the plan's output kept), the plan run once and
//!   completed, the reply completed, exit 0, and the retry asked no sooner than `Retry-After`
//!   after the wall's headers began (their emission bracketed within
//!   [`HEADER_EMISSION_LIMIT_S`], or the timing is inconclusive). The wall is `503
//!   server_is_overloaded`; or `429 usage_limit_reached` naming a reset a second or more past
//!   `Retry-After` ([`RESET_PAST_RETRY_S`], rounded up to the second), which the retry may not
//!   precede; or that `429` with no reset, after which codex-alab polls no sooner than
//!   [`QUOTA_POLL_S`] later;
//! - REFUSAL (`permanent-refusal`, `quota-permanent-refusal`, `quota-no-reset-refusal`,
//!   `quota-terminal-<kind>`): a `403` with the overloaded or the quota body (with or without its
//!   reset), or a `429` with the quota body, its reset and `x-codex-rate-limit-reached-type` one of
//!   the four credit-depletion and spending-cap [`TERMINAL_KINDS`], is asked once, fails the
//!   turn, and exits non-zero (no signal).
//!
//! EGRESS, as the lab's other proofs check it: before each case a jailed `nc` reaches the
//! lane's model port and is refused a loopback canary port the jail does not name (whose
//! listener then took nothing), and while `codex exec` runs its process tree's sockets are
//! sampled with `lsof` ([`crate::sampler::endpoints`]), on a clock and by the model at each
//! request before it answers (codex waits on that connection then, so the witness must see
//! it: measured live, codex holds no connection between requests, and a clock alone saw none):
//! no endpoint off loopback, the sample at each request seeing the tree's connection to the
//! model's port, and the canary untouched after the run.
//!
//! AN INTERRUPT ENDS IT: a first `INT`, `TERM` or `HUP` ([`stop`]) ends the run going (its whole
//! group killed, [`end_with_group`]) and the lane with [`Stopped`]; a second, which ends the lab
//! at once, kills that group first ([`end_running_group`]).

use std::io::{BufRead as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::pyjson::{J, PY};
use crate::stop::{self, Stopped};

/// The model every request must name.
pub(super) const MODEL: &str = "gpt-5";
/// The person's prompt, which every request must carry once.
pub(super) const PROMPT: &str = "Keep this headless capacity smoke task and its completed plan.";
/// The `update_plan` call's id, whose output the retry must keep.
pub(super) const CALL_ID: &str = "capacity-smoke-plan";
/// The plan's one step.
pub(super) const PLAN: &str = "Completed work survives a capacity retry";
/// The reply after the retry.
pub(super) const ANSWER: &str = "Headless capacity recovery succeeded.";
/// `Retry-After` on the wall's answer, in seconds.
const RETRY_AFTER_S: u64 = 2;
/// How far a quota wall's reset lies past `Retry-After` at least, in seconds: the wall names the
/// moment it was asked, rounded up to the second, this much past `Retry-After`, so a retry that
/// keeps `Retry-After` alone comes a second or more before the reset, whatever the fraction of
/// the second it was asked at, and only one that keeps the reset passes the reset's row.
const RESET_PAST_RETRY_S: u64 = 1;
/// The most the wall's headers may take to write and still time the retry.
const HEADER_EMISSION_LIMIT_S: f64 = 0.050;
/// How long codex-alab waits before it asks again past a quota wall that names no reset
/// (`responses_quota_retry.rs`, `UNKNOWN_RESET_WAIT`), in seconds.
const QUOTA_POLL_S: u64 = 30;
/// The most one `codex` run may take beyond what its case waits by design: a hang bound.
const RUN_LIMIT: Duration = Duration::from_secs(60);
/// The `x-codex-rate-limit-reached-type`s codex-alab keeps terminal however the reset reads:
/// `capacity_exec.py --quota-terminals`' four credit-depletion and spending-cap types.
pub(super) const TERMINAL_KINDS: [&str; 4] = [
    "workspace_owner_credits_depleted",
    "workspace_member_credits_depleted",
    "workspace_owner_usage_limit_reached",
    "workspace_member_usage_limit_reached",
];

/// One case of the lane, each one of `capacity_exec.py`'s, by its name ([`Case::name`]): the wall
/// the model answers with, and whether the turn must recover from it or fail at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Case {
    /// `503 server_is_overloaded`, recovered after `Retry-After`.
    CapacityRecovery,
    /// `403` with the overloaded body: terminal.
    PermanentRefusal,
    /// `429 usage_limit_reached` naming its reset, recovered no sooner than the reset.
    QuotaResetRecovery,
    /// `403` with the quota body and its reset: terminal.
    QuotaPermanentRefusal,
    /// `429 usage_limit_reached` naming no reset, recovered by a poll [`QUOTA_POLL_S`] on.
    QuotaNoResetRecovery,
    /// `403` with the quota body and no reset: terminal.
    QuotaNoResetRefusal,
    /// `429` with the quota body, its reset and `x-codex-rate-limit-reached-type` this kind (one
    /// of [`TERMINAL_KINDS`]): terminal.
    QuotaTerminal(&'static str),
}

/// Every case the lane runs, in `capacity_exec.py`'s order.
pub(super) const CASES: [Case; 10] = [
    Case::CapacityRecovery,
    Case::PermanentRefusal,
    Case::QuotaResetRecovery,
    Case::QuotaPermanentRefusal,
    Case::QuotaNoResetRecovery,
    Case::QuotaNoResetRefusal,
    Case::QuotaTerminal(TERMINAL_KINDS[0]),
    Case::QuotaTerminal(TERMINAL_KINDS[1]),
    Case::QuotaTerminal(TERMINAL_KINDS[2]),
    Case::QuotaTerminal(TERMINAL_KINDS[3]),
];

impl Case {
    /// Its name, `capacity_exec.py`'s: its output folder and its verdict's `case`.
    pub(super) fn name(self) -> String {
        match self {
            Self::CapacityRecovery => "capacity-recovery".into(),
            Self::PermanentRefusal => "permanent-refusal".into(),
            Self::QuotaResetRecovery => "quota-reset-recovery".into(),
            Self::QuotaPermanentRefusal => "quota-permanent-refusal".into(),
            Self::QuotaNoResetRecovery => "quota-no-reset-recovery".into(),
            Self::QuotaNoResetRefusal => "quota-no-reset-refusal".into(),
            Self::QuotaTerminal(kind) => format!("quota-terminal-{kind}"),
        }
    }

    /// Whether only codex-alab's quota recovery can pass the case ([`holds_quota_recovery`]).
    const fn needs_quota_recovery(self) -> bool {
        matches!(self, Self::QuotaResetRecovery | Self::QuotaNoResetRecovery)
    }

    /// Whether the turn must fail at the wall, asked once (else it must recover from it).
    pub(super) fn refused(self) -> bool {
        !matches!(
            self,
            Self::CapacityRecovery | Self::QuotaResetRecovery | Self::QuotaNoResetRecovery
        )
    }

    /// The wall's status.
    fn status(self) -> u16 {
        match self {
            Self::CapacityRecovery => 503,
            Self::QuotaResetRecovery | Self::QuotaNoResetRecovery | Self::QuotaTerminal(_) => 429,
            Self::PermanentRefusal | Self::QuotaPermanentRefusal | Self::QuotaNoResetRefusal => 403,
        }
    }

    /// Whether the wall is the account quota's (`usage_limit_reached`), not the model's capacity.
    fn quota(self) -> bool {
        !matches!(self, Self::CapacityRecovery | Self::PermanentRefusal)
    }

    /// Whether the quota wall names its reset (`resets_at`).
    fn names_reset(self) -> bool {
        matches!(
            self,
            Self::QuotaResetRecovery | Self::QuotaPermanentRefusal | Self::QuotaTerminal(_)
        )
    }

    /// How long its run waits by design, which its hang bound ([`RUN_LIMIT`]) comes on top of.
    fn waits(self) -> Duration {
        match self {
            Self::QuotaNoResetRecovery => Duration::from_secs(QUOTA_POLL_S),
            Self::CapacityRecovery => Duration::from_secs(RETRY_AFTER_S),
            // The reset is [`RESET_PAST_RETRY_S`] past `Retry-After`, and up to a second more:
            // rounded up to the second.
            Self::QuotaResetRecovery => Duration::from_secs(RETRY_AFTER_S + RESET_PAST_RETRY_S + 1),
            _ => Duration::ZERO,
        }
    }

    /// What its passing verdict says.
    fn promise(self) -> String {
        match self {
            Self::CapacityRecovery => {
                "one turn recovers from the capacity answer after Retry-After, the plan kept".into()
            }
            Self::QuotaResetRecovery => {
                "one turn recovers from the account quota answer no sooner \
                                         than its reset and Retry-After, the plan kept"
                    .into()
            }
            Self::QuotaNoResetRecovery => format!(
                "one turn recovers from a quota answer that names no reset, asked again no sooner \
                 than {QUOTA_POLL_S} s after it, the plan kept"
            ),
            Self::QuotaTerminal(kind) => format!(
                "a 429 typed {kind} is asked once, fails the turn and exits non-zero, its reset \
                 notwithstanding"
            ),
            Self::PermanentRefusal | Self::QuotaPermanentRefusal | Self::QuotaNoResetRefusal => {
                "a 403 is asked once, fails the turn and exits non-zero".into()
            }
        }
    }
}
/// How often the running `codex exec`'s sockets are sampled.
const SAMPLE_EVERY: Duration = Duration::from_millis(200);
/// The flags of `codex exec` each case passes, which its `--help` must name.
const EXEC_FLAGS: [&str; 5] = [
    "--json",
    "--color",
    "--skip-git-repo-check",
    "--cd",
    "--sandbox",
];

/// Why a jailed run ([`Jail::run`]) gave no exit.
#[derive(Debug)]
enum Cut {
    /// It could not be run or reaped: the lane's check of it fails.
    Failed(String),
    /// An interrupt of the lab ended it, its group killed first: the lane ends as one.
    Stopped(Stopped),
}

impl From<std::io::Error> for Cut {
    fn from(e: std::io::Error) -> Self {
        Self::Failed(e.to_string())
    }
}

/// The process group of the jailed run going now ([`end_with_group`]), whose leader is not
/// reaped while it is held here.
static RUNNING: Mutex<Option<i32>> = Mutex::new(None);

/// Hold `group` as the run going now ([`RUNNING`]), and make killing it one of the lab's acts at
/// a second interrupt ([`stop::at_hard_exit`], which holds every act registered beside it),
/// registered once a process however many runs it holds. `codex` leads a group of its own,
/// which a terminal's `^C` never reaches, so without that act a second interrupt would end the
/// lab and leave the group running.
fn hold(group: Option<i32>) {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    *RUNNING.lock().unwrap_or_else(PoisonError::into_inner) = group;
    REGISTERED.call_once(|| stop::at_hard_exit(end_running_group));
}

/// Kill the held group ([`hold`]) until it is empty, and hold none. The lock is kept while it is
/// killed, so its leader, reaped only once this has returned, still holds the group's id: it
/// cannot have been reused. Both [`end_with_group`] and the lab's act at a second interrupt end a
/// run through this; the first to come kills the group.
fn end_running_group() {
    let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(group) = running.take() {
        // ESRCH or EPERM once only the unreaped leader is left: nothing to stop.
        let _ = aterm_uds::process::kill_group_until_empty(group);
    }
}

/// One request the lane's model took, on the lane's clock (seconds since it began).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Taken {
    /// When its first byte arrived: no later than the server read it.
    pub arrived: f64,
    /// The same moment on the wall clock (seconds since the epoch), which a quota reset names.
    pub arrived_unix: f64,
    pub body: Option<J>,
    /// Just before its answer's headers were written.
    pub header_started: Option<f64>,
    /// Once they were written and flushed, before the body.
    pub header_sent: Option<f64>,
    /// The reset its answer named (`resets_at`, seconds since the epoch).
    pub resets_at: Option<i64>,
}

/// The lane's model: one case's answers and what it took.
struct Model {
    case: Case,
    clock: Instant,
    /// The loopback port it listens on, which each request's connection is to.
    port: u16,
    taken: Mutex<Vec<Taken>>,
    /// Sampled at each request it takes, before its answer: the client is waiting on its
    /// connection then, so a witness that is not blind sees that connection.
    witness: Arc<Witness>,
}

/// The sockets of a running `codex`'s process tree, sampled with `lsof`
/// ([`crate::sampler::endpoints`]) on a clock and at each request the model takes.
#[derive(Default)]
struct Witness {
    /// The tree's root once spawned (`sandbox-exec` execs `codex` in place).
    root: Mutex<Option<i32>>,
    seen: Mutex<Seen>,
}

#[derive(Default)]
struct Seen {
    endpoints: std::collections::BTreeSet<String>,
    pids: std::collections::BTreeSet<i32>,
    samples: usize,
    /// Samples taken at a request, while the client waited for its answer.
    at_requests: usize,
    /// Samples taken at a request that saw the tree connected to the model's port then.
    connected_at_requests: usize,
}

impl Witness {
    /// Sample the tree once (`request_to`: the model's port, sampled at a request while the
    /// client waits on it for the answer).
    fn sample(&self, request_to: Option<u16>) {
        let Some(root) = *self
            .root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        else {
            return;
        };
        let table = crate::procs::table();
        let mut pids = vec![root];
        pids.extend(crate::procs::descendants_in(&table, root));
        let list: Vec<String> = pids.iter().map(ToString::to_string).collect();
        let Ok(out) = aterm_uds::spawnfd::command("/usr/sbin/lsof")
            .args(["-nP", "-i", "-a", "-p", &list.join(","), "-F", "pcPtn"])
            .stdin(Stdio::null())
            .output()
        else {
            return;
        };
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(&pids, &String::from_utf8_lossy(&out.stdout), request_to);
    }
}

impl Seen {
    /// Record one sample of the tree `pids`, `lsof -F pcPtn`'s output `lsof_f`, taken at a
    /// request to the model on `request_to` (else on the clock): its endpoints, and whether that
    /// request's sample saw one of `pids` connected to the model's port.
    fn record(&mut self, pids: &[i32], lsof_f: &str, request_to: Option<u16>) {
        let found = crate::sampler::endpoints(lsof_f);
        if let Some(port) = request_to {
            self.at_requests += 1;
            let log: String = found.iter().map(|l| format!("{l}\n")).collect();
            if crate::agy_proof::talked(&log, pids, &port.to_string()) {
                self.connected_at_requests += 1;
            }
        }
        self.pids.extend(pids);
        self.endpoints.extend(found);
        self.samples += 1;
    }

    /// Its socket log: each endpoint seen, a line each.
    fn log(&self) -> String {
        self.endpoints.iter().map(|l| format!("{l}\n")).collect()
    }

    /// The endpoints seen off loopback.
    fn external(&self) -> Vec<&String> {
        self.endpoints
            .iter()
            .filter(|l| l.ends_with("EXTERNAL"))
            .collect()
    }

    /// Whether one of the tree's pids was seen connected to the model's `port`.
    fn talked(&self, port: u16) -> bool {
        let pids: Vec<i32> = self.pids.iter().copied().collect();
        crate::agy_proof::talked(&self.log(), &pids, &port.to_string())
    }

    /// THE EGRESS VERDICT on a run whose model on `port` took `requests`: no socket of the tree
    /// off loopback, a sample at each request (and at least one request, so the witness saw
    /// something it had to), each of those samples seeing the tree connected to the model, that
    /// connection to `port`, and the canary untouched (`canary_touched`).
    fn egress_held(&self, port: u16, requests: usize, canary_touched: bool) -> bool {
        self.external().is_empty()
            && requests > 0
            && self.at_requests == requests
            && self.connected_at_requests == requests
            && self.talked(port)
            && !canary_touched
    }
}

impl Model {
    fn now(&self) -> f64 {
        self.clock.elapsed().as_secs_f64()
    }
}

/// One answer of the lane's model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Answer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
    /// `Retry-After`, in seconds.
    pub retry_after: Option<u64>,
    /// `x-codex-rate-limit-reached-type`.
    pub reached_type: Option<&'static str>,
    /// The reset its body names (`resets_at`, seconds since the epoch).
    pub resets_at: Option<i64>,
}

/// What the model answers the `number`th request (from 1) of `case`, asked at `now` (seconds
/// since the epoch), as `capacity_exec.py`'s model does: the plan call, the case's wall with
/// `Retry-After`, the reply, then a refusal of any extra request; a refused case meets its wall
/// at every request. A quota wall that names its reset names `now` rounded up to the second,
/// [`RETRY_AFTER_S`] and [`RESET_PAST_RETRY_S`] on (the smoke's named it [`RETRY_AFTER_S`] on,
/// which a retry that kept `Retry-After` alone could reach).
pub(super) fn answer(case: Case, number: usize, now: f64) -> Answer {
    let ok = |body| Answer {
        status: 200,
        content_type: "text/event-stream",
        body,
        retry_after: None,
        reached_type: None,
        resets_at: None,
    };
    if case.refused() || number == 2 {
        let resets_at = (case.quota() && case.names_reset())
            .then(|| now.ceil() as i64 + (RETRY_AFTER_S + RESET_PAST_RETRY_S) as i64);
        let error = if case.quota() {
            let mut error = J::obj([
                ("type", J::s("usage_limit_reached")),
                ("message", J::s("Fixture account quota exhausted")),
            ]);
            if let Some(reset) = resets_at {
                error.set("resets_at", J::Int(reset));
            }
            error
        } else {
            J::obj([
                ("code", J::s("server_is_overloaded")),
                ("message", J::s("Model is at capacity.")),
            ])
        };
        return Answer {
            status: case.status(),
            content_type: "application/json",
            body: J::obj([("error", error)]).dump(PY),
            retry_after: Some(RETRY_AFTER_S),
            reached_type: match case {
                Case::QuotaTerminal(kind) => Some(kind),
                _ => None,
            },
            resets_at,
        };
    }
    match number {
        1 => {
            let args = J::obj([(
                "plan",
                J::Arr(vec![J::obj([
                    ("step", J::s(PLAN)),
                    ("status", J::s("completed")),
                ])]),
            )]);
            let call = J::obj([
                ("type", J::s("function_call")),
                ("id", J::s("fc_capacity")),
                ("call_id", J::s(CALL_ID)),
                ("name", J::s("update_plan")),
                ("status", J::s("completed")),
                ("arguments", J::s(args.py())),
            ]);
            ok(crate::codex_rig::sse(vec![call]))
        }
        3 => ok(crate::codex_rig::sse(vec![crate::codex_rig::message_item(
            ANSWER,
        )])),
        _ => Answer {
            status: 400,
            content_type: "application/json",
            body: r#"{"error":{"message":"Unexpected extra request"}}"#.to_string(),
            retry_after: None,
            reached_type: None,
            resets_at: None,
        },
    }
}

/// The lane's model on `listener`, a thread per connection, each connection (and its writing
/// half) kept out of the children this lane spawns meanwhile ([`aterm_uds::accept`],
/// [`aterm_uds::kept_out`]): a `codex`, `nc` or `lsof` that took a copy could hold the model's
/// side of a connection open past its answer.
fn serve(listener: TcpListener, model: Arc<Model>) {
    while let Ok((conn, _)) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    ) {
        let model = Arc::clone(&model);
        std::thread::spawn(move || serve_one(conn, &model));
    }
}

fn serve_one(conn: TcpStream, model: &Model) {
    let Ok(mut w) = aterm_uds::kept_out(|| conn.try_clone()) else {
        return;
    };
    let mut r = std::io::BufReader::new(conn);
    loop {
        // The first byte's arrival, before the request is parsed (a keep-alive connection
        // waits here between requests).
        match r.fill_buf() {
            Ok([]) | Err(_) => return,
            Ok(_) => {}
        }
        let (arrived, arrived_unix) = (model.now(), crate::util::now());
        let Ok(Some(req)) = crate::stub::read_request(&mut r) else {
            return;
        };
        if req.method != "POST" || req.target != "/v1/responses" {
            let body = r#"{"error":{"message":"not here"}}"#;
            let head = format!(
                "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            if w.write_all(head.as_bytes())
                .and_then(|()| w.write_all(body.as_bytes()))
                .and_then(|()| w.flush())
                .is_err()
            {
                return;
            }
            continue;
        }
        let number = {
            let mut taken = model
                .taken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            taken.push(Taken {
                arrived,
                arrived_unix,
                body: J::parse_bytes(&req.body).ok(),
                ..Taken::default()
            });
            taken.len()
        };
        model.witness.sample(Some(model.port));
        let Answer {
            status,
            content_type,
            body,
            retry_after,
            reached_type,
            resets_at,
        } = answer(model.case, number, crate::util::now());
        let reason = match status {
            200 => "OK",
            403 => "Forbidden",
            429 => "Too Many Requests",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let retry = retry_after.map_or_else(String::new, |s| format!("Retry-After: {s}\r\n"));
        let typed = reached_type.map_or_else(String::new, |k| {
            format!("x-codex-rate-limit-reached-type: {k}\r\n")
        });
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n{retry}{typed}\r\n",
            body.len()
        );
        let header_started = model.now();
        if w.write_all(head.as_bytes())
            .and_then(|()| w.flush())
            .is_err()
        {
            return;
        }
        let header_sent = model.now();
        // Bound to this exact answer: Retry-After counts from its headers, whatever its body
        // and another request do after.
        if let Some(t) = model
            .taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(number - 1)
        {
            t.header_started = Some(header_started);
            t.header_sent = Some(header_sent);
            t.resets_at = resets_at;
        }
        if w.write_all(body.as_bytes())
            .and_then(|()| w.flush())
            .is_err()
        {
            return;
        }
    }
}

/// HOW LONG AFTER THE WALL'S HEADERS THE RETRY CAME (the overloaded or quota answer's), measured from its headers
/// and not from the request's handling before them: the client may have read them anywhere
/// between their two stamps, so only that bracket is used, and a bracket wider than
/// [`HEADER_EMISSION_LIMIT_S`] is inconclusive. The retry's arrival is its first byte.
/// # Errors
/// A stamp missing or not finite, an inconclusive bracket, or a retry before
/// `Retry-After` from the headers' start.
pub(super) fn retry_after_delay(overload: &Taken, retry: &Taken) -> Result<f64, String> {
    let (Some(start), Some(header)) = (overload.header_started, overload.header_sent) else {
        return Err("missing or invalid Retry-After timing".into());
    };
    if ![start, header, retry.arrived].iter().all(|v| v.is_finite()) {
        return Err("missing or invalid Retry-After timing".into());
    }
    let emission = header - start;
    if !(0.0..=HEADER_EMISSION_LIMIT_S).contains(&emission) {
        return Err(format!(
            "Retry-After timing inconclusive: header emission took {emission:.6}s"
        ));
    }
    let delay = retry.arrived - header;
    if retry.arrived < (start + RETRY_AFTER_S as f64).next_down() {
        return Err(format!("Retry-After bypassed: {delay:.6}s after headers"));
    }
    Ok(delay)
}

/// THE QUOTA'S RESET KEPT: the walled request's answer named its reset (`resets_at`, seconds
/// since the epoch), and the retry's first byte came on the wall clock no sooner. The reset.
/// # Errors
/// No reset named, a retry stamp not finite, or a retry before the reset.
pub(super) fn quota_reset_kept(walled: &Taken, retry: &Taken) -> Result<i64, String> {
    let Some(reset) = walled.resets_at else {
        return Err("the quota answer named no reset".into());
    };
    if !retry.arrived_unix.is_finite() {
        return Err("missing or invalid quota retry time".into());
    }
    if retry.arrived_unix < reset as f64 {
        return Err(format!(
            "account quota was retried before its reset ({:.6}s early)",
            reset as f64 - retry.arrived_unix
        ));
    }
    Ok(reset)
}

/// THE POLL PAST A QUOTA WALL THAT NAMES NO RESET: the retry's first byte came no sooner than
/// [`QUOTA_POLL_S`] after the walled answer's headers were written (codex-alab could read its
/// body, which says it is the quota's, only after them). How long after.
/// # Errors
/// A stamp missing or not finite, or a retry sooner.
pub(super) fn quota_poll_kept(walled: &Taken, retry: &Taken) -> Result<f64, String> {
    let Some(header) = walled.header_sent.filter(|h| h.is_finite()) else {
        return Err("missing or invalid quota poll timing".into());
    };
    if !retry.arrived.is_finite() {
        return Err("missing or invalid quota poll timing".into());
    }
    let poll = retry.arrived - header;
    if poll < QUOTA_POLL_S as f64 {
        return Err(format!(
            "unknown account quota reset bypassed its polling backoff ({poll:.6}s after headers)"
        ));
    }
    Ok(poll)
}

/// JSON equality with objects compared as key sets: a parsed object's keys come back in
/// the parser's order, not the writer's.
fn same(a: &J, b: &J) -> bool {
    match (a, b) {
        (J::Obj(x), J::Obj(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| b.get(k).is_some_and(|w| same(v, w)))
        }
        (J::Arr(x), J::Arr(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same(v, w)),
        _ => a == b,
    }
}

/// The user texts of a request body's input messages.
fn user_texts(body: &J) -> Vec<String> {
    body.get("input")
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .filter(|item| {
            item.get("type").and_then(J::as_str) == Some("message")
                && item.get("role").and_then(J::as_str) == Some("user")
        })
        .flat_map(|item| item.get("content").and_then(J::as_arr).unwrap_or_default())
        .filter_map(|part| part.get("text").and_then(J::as_str).map(str::to_string))
        .collect()
}

/// THE VERDICT ON ONE CASE, from its exit (`None`: a signal), what the model took and the
/// JSON events `codex exec --json` wrote: the summary `result.json` keeps.
/// # Errors
/// The first thing the case did not do, in `capacity_exec.py`'s words.
pub(super) fn judge(
    case: Case,
    exit: Option<i32>,
    taken: &[Taken],
    events: &[J],
) -> Result<J, String> {
    let name = case.name();
    let types: Vec<&str> = events
        .iter()
        .map(|e| e.get("type").and_then(J::as_str).unwrap_or(""))
        .collect();
    let count = |t: &str| types.iter().filter(|x| **x == t).count();
    if count("turn.started") != 1 {
        return Err(format!("{name}: expected one native turn"));
    }
    if !taken.iter().all(|t| {
        t.body
            .as_ref()
            .and_then(|b| b.get("model"))
            .and_then(J::as_str)
            == Some(MODEL)
    }) {
        return Err(format!("{name}: model changed"));
    }
    for t in taken {
        let texts = t.body.as_ref().map(user_texts).unwrap_or_default();
        if texts.iter().filter(|x| *x == PROMPT).count() != 1 {
            return Err(format!(
                "{name}: original user input was lost or duplicated"
            ));
        }
    }
    let mut result = J::obj([
        ("case", J::s(name.as_str())),
        ("exit", exit.map_or(J::Null, |c| J::Int(i64::from(c)))),
        (
            "requestCount",
            J::Int(i64::try_from(taken.len()).unwrap_or(i64::MAX)),
        ),
        ("events", J::Arr(types.iter().map(|t| J::s(*t)).collect())),
    ]);
    if case.refused() {
        if !exit.is_some_and(|c| c > 0) || taken.len() != 1 {
            return Err(format!(
                "{name}: the refusal was retried, signaled or reported success"
            ));
        }
        if count("turn.failed") != 1 || count("turn.completed") != 0 {
            return Err(format!("{name}: the refusal did not fail the native turn"));
        }
    } else {
        if exit != Some(0) || taken.len() != 3 {
            return Err(format!(
                "{name}: did not recover in three requests ({} request(s), exit {exit:?})",
                taken.len()
            ));
        }
        if count("turn.completed") != 1 || count("turn.failed") != 0 {
            return Err(format!("{name}: turn did not recover"));
        }
        let input = |t: &Taken| t.body.as_ref().and_then(|b| b.get("input")).cloned();
        let (retained, retried) = (input(&taken[1]), input(&taken[2]));
        if retained.is_none()
            || !same(
                retained.as_ref().unwrap_or(&J::Null),
                retried.as_ref().unwrap_or(&J::Null),
            )
        {
            return Err(format!("{name}: retry changed retained input/tool output"));
        }
        let outputs: Vec<J> = input(&taken[1])
            .and_then(|i| i.as_arr().map(<[J]>::to_vec))
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.get("type").and_then(J::as_str) == Some("function_call_output"))
            .collect();
        if outputs.len() != 1 || outputs[0].get("call_id").and_then(J::as_str) != Some(CALL_ID) {
            return Err(format!("{name}: completed tool output lost"));
        }
        let output = match outputs[0].get("output") {
            Some(J::Str(s)) => s.clone(),
            Some(other) => other.py(),
            None => String::new(),
        };
        if !output.contains("Plan updated") {
            return Err(format!("{name}: plan tool did not complete"));
        }
        let plans: Vec<&J> = events
            .iter()
            .filter(|e| {
                e.get("item")
                    .and_then(|i| i.get("type"))
                    .and_then(J::as_str)
                    == Some("todo_list")
            })
            .collect();
        let plan_types: Vec<&str> = plans
            .iter()
            .map(|e| e.get("type").and_then(J::as_str).unwrap_or(""))
            .collect();
        if plan_types != ["item.started", "item.completed"] {
            return Err(format!(
                "{name}: plan was missing or executed more than once"
            ));
        }
        let want = J::Arr(vec![J::obj([
            ("text", J::s(PLAN)),
            ("completed", J::Bool(true)),
        ])]);
        if !plans[1]
            .get("item")
            .and_then(|i| i.get("items"))
            .is_some_and(|items| same(items, &want))
        {
            return Err(format!("{name}: plan changed"));
        }
        let replied = events.iter().any(|e| {
            e.get("type").and_then(J::as_str) == Some("item.completed")
                && e.get("item")
                    .and_then(|i| i.get("type"))
                    .and_then(J::as_str)
                    == Some("agent_message")
                && e.get("item")
                    .and_then(|i| i.get("text"))
                    .and_then(J::as_str)
                    == Some(ANSWER)
        });
        if !replied {
            return Err(format!("{name}: reply missing"));
        }
        let delay = retry_after_delay(&taken[1], &taken[2]).map_err(|e| format!("{name}: {e}"))?;
        let emission = taken[1].header_sent.unwrap_or(0.0) - taken[1].header_started.unwrap_or(0.0);
        match case {
            Case::QuotaResetRecovery => {
                let reset =
                    quota_reset_kept(&taken[1], &taken[2]).map_err(|e| format!("{name}: {e}"))?;
                result.set("quotaResetUnix", J::Int(reset));
                result.set("retryUnix", J::Float(taken[2].arrived_unix));
            }
            Case::QuotaNoResetRecovery => {
                let poll =
                    quota_poll_kept(&taken[1], &taken[2]).map_err(|e| format!("{name}: {e}"))?;
                result.set("pollDelaySeconds", J::Float(poll));
            }
            _ => {}
        }
        result.set("retryDelaySeconds", J::Float(delay));
        result.set("retryHeaderEmissionSeconds", J::Float(emission));
        result.set(
            "retryTimingSampleLimitSeconds",
            J::Float(HEADER_EMISSION_LIMIT_S),
        );
    }
    result.set("passed", J::Bool(true));
    Ok(result)
}

/// THE PACKAGE'S RECORDS (`alab-build.json`, `codex-package.json`): agreed, and the
/// entrypoint inside the package. The version, the exact source commit and the executable.
/// # Errors
/// What disagrees, or what is missing.
pub(super) fn package_records(pkg: &Path) -> Result<(String, String, PathBuf), String> {
    let read = |name: &str| -> Result<J, String> {
        let path = pkg.join(name);
        let meta =
            std::fs::symlink_metadata(&path).map_err(|_| format!("missing regular {name}"))?;
        if !meta.file_type().is_file() {
            return Err(format!("missing regular {name}"));
        }
        if meta.len() > 1024 * 1024 {
            return Err(format!("oversized {name}"));
        }
        let text = crate::util::read_regular_text(&path).map_err(|e| format!("{name}: {e}"))?;
        J::parse(&text).map_err(|e| format!("{name}: {e}"))
    };
    let (build, manifest) = (read("alab-build.json")?, read("codex-package.json")?);
    let field = |j: &J, k: &str| j.get(k).and_then(J::as_str).unwrap_or("").to_string();
    if field(&build, "version") != field(&manifest, "version") {
        return Err("package versions disagree".into());
    }
    if field(&build, "target") != field(&manifest, "target") {
        return Err("package targets disagree".into());
    }
    let source = field(&build, "source");
    if source.len() != 40
        || !source
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("missing exact source commit".into());
    }
    if field(&manifest, "entrypoint") != "bin/codex" {
        return Err("unexpected package entrypoint".into());
    }
    let root = std::fs::canonicalize(pkg).map_err(|e| format!("{}: {e}", pkg.display()))?;
    let exe = std::fs::canonicalize(root.join("bin/codex"))
        .map_err(|_| "entrypoint escapes package".to_string())?;
    if !exe.starts_with(&root) || !exe.is_file() {
        return Err("entrypoint escapes package".into());
    }
    Ok((field(&manifest, "version"), source, exe))
}

/// A case's homes, and the environment its codex runs in: no login, provider, proxy, aterm
/// or agent variable of this process's.
fn fresh_environment(root: &Path) -> std::io::Result<Vec<(String, String)>> {
    for name in ["home", "codex-home", "work", "xdg", "tmp"] {
        std::fs::create_dir_all(root.join(name))?;
    }
    let at = |n: &str| root.join(n).to_string_lossy().into_owned();
    Ok([
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
        ("HOME", at("home")),
        ("CODEX_HOME", at("codex-home")),
        ("XDG_RUNTIME_DIR", at("xdg")),
        ("TMPDIR", at("tmp")),
        ("TERM", "dumb".to_string()),
        ("NO_COLOR", "1".to_string()),
        ("LANG", "en_US.UTF-8".to_string()),
    ]
    .map(|(k, v)| (k.to_string(), v))
    .to_vec())
}

/// A scratch jail's execution environment and socket witness.
struct Jail<'a> {
    root: &'a Path,
    env: &'a [(String, String)],
    profile: &'a str,
    witness: &'a Witness,
}

impl Jail<'_> {
    /// Run `exe args` under the jail `profile` in `root/work` with `env`, its stdout and stderr
    /// kept as `root/<label>-stdout.jsonl` and `root/<label>-stderr.txt` and its command as
    /// `root/<label>-command.json`, its process tree's sockets sampled into `witness` while it
    /// runs, for at most `limit` (a hang bound). Its exit (`None`: a signal or the hang bound).
    ///
    /// THE RUN'S GROUP ENDS WITH IT: `exe` leads a process group of its own (`sandbox-exec` execs
    /// it in place), which [`end_with_group`] kills once `exe` exits, at the hang bound, or at an
    /// interrupt of the lab.
    /// # Errors
    /// [`Cut::Failed`]: its files could not be made, or it could not be spawned or reaped;
    /// [`Cut::Stopped`]: an interrupt ended it.
    fn run(
        &self,
        exe: &Path,
        args: &[&str],
        label: &str,
        limit: Duration,
    ) -> Result<Option<i32>, Cut> {
        let Self {
            root,
            env,
            profile,
            witness,
        } = *self;
        let mut command = vec![J::s(exe.to_string_lossy())];
        command.extend(args.iter().map(|a| J::s(*a)));
        crate::util::write_fresh(
            &root.join(format!("{label}-command.json")),
            format!("{}\n", J::Arr(command).dump(PY)),
        )?;
        let stdout = crate::util::create_stdio_file(&root.join(format!("{label}-stdout.jsonl")))?;
        let stderr = crate::util::create_stdio_file(&root.join(format!("{label}-stderr.txt")))?;
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
        cmd.arg("-p")
            .arg(profile)
            .arg(exe)
            .args(args)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(root.join("work"))
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let mut child = cmd.spawn()?;
        *witness
            .root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = i32::try_from(child.id()).ok();
        let exit = end_with_group(&mut child, witness, limit);
        *witness
            .root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        exit
    }
}

/// Wait for `child`, the leader of a process group of its own, for at most `limit` (a hang
/// bound) or until an interrupt of the lab ([`stop::sleep`]), its sockets sampled into `witness`
/// meanwhile; then kill its whole group and reap it. Its exit (`None`: a signal or the bound).
///
/// The group is held ([`hold`]) and killed ([`end_running_group`]) before the leader is reaped,
/// so its group id cannot have been reused, as codex-alab's `command_guard.py` stops a smoke
/// command's group: nothing the leader started that stayed in its group outlives it. A
/// descendant that left the group (its own `setsid` or `setpgid`) is not reached.
/// # Errors
/// [`Cut::Failed`]: the leader could not be read or reaped; [`Cut::Stopped`]: an interrupt came
/// before it exited.
fn end_with_group(
    child: &mut aterm_uds::spawnfd::Child,
    witness: &Witness,
    limit: Duration,
) -> Result<Option<i32>, Cut> {
    hold(i32::try_from(child.id()).ok());
    let deadline = Instant::now() + limit;
    let mut stopped = None;
    loop {
        // Read without reaping: until the wait below, the leader's pid keeps its group's id.
        let ended = child.exited()?;
        if ended || stopped.is_some() || Instant::now() >= deadline {
            end_running_group();
            let status = child.wait()?;
            if let Some(s) = stopped {
                return Err(Cut::Stopped(s));
            }
            return Ok(if ended { status.code() } else { None });
        }
        witness.sample(None);
        stopped = stop::sleep(SAMPLE_EVERY).err();
    }
}

/// Whether a jailed `nc` reaches `port` on loopback.
fn reaches(profile: &str, port: u16) -> bool {
    aterm_uds::spawnfd::command("/usr/bin/sandbox-exec")
        .args([
            "-p",
            profile,
            "/usr/bin/nc",
            "-n",
            "-z",
            "-G",
            "2",
            "127.0.0.1",
        ])
        .arg(port.to_string())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether anything connected to the canary since it was bound (the sanctioned `Ready` accept
/// answers at once). A loopback `connect` can return before the listener's queue holds
/// its connection, so `false` means untouched only where nothing can still be on its way, as
/// after a probe whose connect failed ([`jail_holds`]).
fn canary_touched(canary: &TcpListener) -> bool {
    !matches!(
        aterm_uds::accept(
            canary,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
    )
}

/// THE JAIL PROBE: whether a child under `profile` reaches the model's `port` and is refused
/// `canary`'s, which took nothing.
fn jail_holds(profile: &str, port: u16, canary: &TcpListener) -> bool {
    let Ok(canary_port) = canary.local_addr().map(|a| a.port()) else {
        return false;
    };
    reaches(profile, port) && !reaches(profile, canary_port) && !canary_touched(canary)
}

/// Whether `codex exec --help` exited 0 (`exit`) and its stdout (`text`) names every flag the
/// cases pass ([`EXEC_FLAGS`]).
fn help_names_the_flags(exit: Option<i32>, text: &str) -> bool {
    exit == Some(0) && EXEC_FLAGS.iter().all(|f| text.contains(f))
}

/// Whether `codex --version` exited 0 (`exit`) and its stdout (`text`) is the package's
/// `version`, as codex spells it.
fn says_version(exit: Option<i32>, text: &str, version: &str) -> bool {
    exit == Some(0) && text.trim() == format!("codex-cli {version}")
}

/// One case's config: the lane's model on `port` as its only provider, no retries of its own
/// (codex-alab's capacity recovery is what retries), updater, analytics, feedback and
/// telemetry off.
fn case_config(port: u16) -> String {
    format!(
        "model = \"{MODEL}\"\nmodel_provider = \"capacity-smoke\"\napproval_policy = \"never\"\n\
         sandbox_mode = \"read-only\"\ncheck_for_update_on_startup = false\n\
         [analytics]\nenabled = false\n[feedback]\nenabled = false\n\
         [otel]\nexporter = \"none\"\nmetrics_exporter = \"none\"\ntrace_exporter = \"none\"\n\
         [tools.update_plan]\nenabled = true\n\
         [model_providers.capacity-smoke]\nname = \"local capacity smoke\"\n\
         base_url = \"http://127.0.0.1:{port}/v1\"\nwire_api = \"responses\"\n\
         requires_openai_auth = false\nrequest_max_retries = 0\nstream_max_retries = 0\n"
    )
}

/// Run `case` in `out/<its name>`, its run bound by what it waits by design and [`RUN_LIMIT`]
/// on top; the checks it made, `(label, held)`.
/// # Errors
/// An interrupt of the lab, which ended the case's run (its group killed) or came before it.
fn run_case(
    exe: &Path,
    out: &Path,
    case: Case,
    profile_for: &dyn Fn(u16) -> Result<String, String>,
) -> Result<Vec<(String, bool)>, Stopped> {
    stop::check()?;
    let name = case.name();
    let mut checks = Vec::new();
    let mut check = |label: String, held: bool| {
        println!(
            "  {} capacity {name}: {label}",
            if held { "ok  " } else { "FAIL" }
        );
        checks.push((label, held));
    };
    let root = out.join(&name);
    let laid = (|| -> Result<_, String> {
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let env = fresh_environment(&root).map_err(|e| e.to_string())?;
        let listener =
            aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let canary =
            aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).map_err(|e| e.to_string())?;
        canary.set_nonblocking(true).map_err(|e| e.to_string())?;
        let canary_port = canary.local_addr().map_err(|e| e.to_string())?.port();
        crate::util::write_fresh(&root.join("codex-home/config.toml"), case_config(port))
            .map_err(|e| e.to_string())?;
        let profile = profile_for(port)?;
        Ok((env, listener, port, canary, canary_port, profile))
    })();
    let (env, listener, port, canary, canary_port, profile) = match laid {
        Ok(laid) => laid,
        Err(why) => {
            check(format!("its scratch and model are laid out ({why})"), false);
            return Ok(checks);
        }
    };
    let witness = Arc::new(Witness::default());
    let model = Arc::new(Model {
        case,
        clock: Instant::now(),
        port,
        taken: Mutex::new(Vec::new()),
        witness: Arc::clone(&witness),
    });
    {
        let model = Arc::clone(&model);
        std::thread::spawn(move || serve(listener, model));
    }
    check(
        format!(
            "the jail lets a child reach the model's port {port} and no other loopback port ({canary_port})"
        ),
        jail_holds(&profile, port, &canary),
    );
    stop::check()?;
    let work = root.join("work").to_string_lossy().into_owned();
    let ran = Jail {
        root: &root,
        env: &env,
        profile: &profile,
        witness: &witness,
    }
    .run(
        exe,
        &[
            "exec",
            "--json",
            "--color",
            "never",
            "--skip-git-repo-check",
            "--cd",
            &work,
            "--sandbox",
            "read-only",
            PROMPT,
        ],
        "exec",
        RUN_LIMIT + case.waits(),
    );
    let exit = match ran {
        Ok(exit) => exit,
        Err(Cut::Failed(e)) => {
            check(format!("codex exec ran in the jail ({e})"), false);
            return Ok(checks);
        }
        Err(Cut::Stopped(s)) => return Err(s),
    };
    let taken = model
        .taken
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let requests = J::Arr(
        taken
            .iter()
            .map(|t| {
                J::obj([
                    ("arrivedAt", J::Float(t.arrived)),
                    ("arrivedUnix", J::Float(t.arrived_unix)),
                    (
                        "headerStartedAt",
                        t.header_started.map_or(J::Null, J::Float),
                    ),
                    ("headerAt", t.header_sent.map_or(J::Null, J::Float)),
                    ("resetsAt", t.resets_at.map_or(J::Null, J::Int)),
                    ("body", t.body.clone().unwrap_or(J::Null)),
                ])
            })
            .collect(),
    );
    let _ = crate::util::write_fresh(
        &root.join("requests.json"),
        format!("{}\n", requests.dump(PY)),
    );
    let seen = witness
        .seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = crate::util::write_fresh(&root.join("sockets.log"), seen.log());
    // Each request was sampled while codex waited on its connection for the answer, so a
    // witness that is not blind saw that connection at each.
    check(
        format!(
            "no socket of codex exec's tree left loopback ({} sample(s), {} at a request of {}; its connection to the model seen at {}; external: {:?}), and the canary took nothing",
            seen.samples,
            seen.at_requests,
            taken.len(),
            seen.connected_at_requests,
            seen.external()
        ),
        seen.egress_held(port, taken.len(), canary_touched(&canary)),
    );
    drop(seen);
    let events: Vec<J> = crate::util::read_regular_text(&root.join("exec-stdout.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| J::parse(l).ok())
        .collect();
    match judge(case, exit, &taken, &events) {
        Ok(result) => {
            let _ = crate::util::write_fresh(
                &root.join("result.json"),
                format!("{}\n", result.dump(PY)),
            );
            let secs = |key: &str| result.get(key).and_then(J::as_f64);
            let mut timing = String::new();
            if let Some(d) = secs("retryDelaySeconds") {
                timing.push_str(&format!(", retried {d:.3}s after the headers"));
            }
            if let Some(d) = secs("pollDelaySeconds") {
                timing.push_str(&format!(" (the poll {d:.3}s)"));
            }
            if let (Some(reset), Some(at)) = (
                result.get("quotaResetUnix").and_then(J::as_i64),
                secs("retryUnix"),
            ) {
                timing.push_str(&format!(", {:.3}s past the reset", at - reset as f64));
            }
            check(
                format!(
                    "{} ({} request(s), exit {:?}{timing})",
                    case.promise(),
                    taken.len(),
                    exit,
                ),
                true,
            );
        }
        Err(why) => check(why, false),
    }
    Ok(checks)
}

/// What codex-alab's quota recovery tells the turn while it waits (`alab-core` glue
/// `responses_quota_retry.rs`). A package without these words predates that recovery: a quota wall
/// ends its turn, so the cases that need it have nothing to observe. The package installed on
/// 2026-11-05 (built 2026-11-02 from `ef71b0aa49`) is one.
const QUOTA_WAIT_WORDS: &[u8] = b"Waiting for account quota";

/// Whether `program`'s bytes hold [`QUOTA_WAIT_WORDS`].
fn holds_quota_recovery(program: &[u8]) -> bool {
    program
        .windows(QUOTA_WAIT_WORDS.len())
        .any(|window| window == QUOTA_WAIT_WORDS)
}

/// Whether the package's CLI holds its quota recovery. One that cannot be read is judged on every
/// case, so it fails rather than going unobserved.
fn cli_holds_quota_recovery(exe: &Path) -> bool {
    crate::util::read_program(exe).map_or(true, |program| holds_quota_recovery(&program))
}

/// The cases [`lane`] runs on a package: all of them, less those only a recovery it lacks can pass.
fn cases_for(holds_recovery: bool) -> impl Iterator<Item = Case> {
    CASES
        .into_iter()
        .filter(move |case| holds_recovery || !case.needs_quota_recovery())
}

/// The wants [`lane`] cannot observe on the package at `pkg`, for the run's verdict. A package
/// whose records do not agree names none: the lane fails it on its records.
pub(super) fn unobservable(pkg: &Path) -> Vec<String> {
    match package_records(pkg) {
        Ok((version, _, exe)) if !cli_holds_quota_recovery(&exe) => vec![format!(
            "capacity: quota recovery (package {version} holds none, so its recovery cases did \
             not run)"
        )],
        _ => Vec::new(),
    }
}

/// THE LANE: the package's records and CLI, then each case it can observe ([`cases_for`]), each under the jail
/// `profile_for(port)` makes for its model's port, every output kept in `out`. The checks it
/// made, `(label, held)`.
/// # Errors
/// An interrupt of the lab, which ended the run going (its group killed) or came between runs.
pub(super) fn lane(
    pkg: &Path,
    out: &Path,
    profile_for: &dyn Fn(u16) -> Result<String, String>,
) -> Result<Vec<(String, bool)>, Stopped> {
    stop::check()?;
    println!("== capacity (codex exec, jailed, against the lane's own loopback model)");
    let mut checks = Vec::new();
    let (version, source, exe) = match package_records(pkg) {
        Ok(found) => found,
        Err(why) => {
            checks.push((
                format!("capacity: the package's records agree ({why})"),
                false,
            ));
            println!("  FAIL capacity: the package's records agree ({why})");
            return Ok(checks);
        }
    };
    println!(
        "  (package {version}, source {source}, executable {})",
        exe.display()
    );
    let _ = std::fs::create_dir_all(out);
    let _ = crate::util::write_fresh(
        &out.join("package.json"),
        format!(
            "{}\n",
            J::obj([
                ("package", J::s(pkg.to_string_lossy())),
                ("executable", J::s(exe.to_string_lossy())),
                ("version", J::s(version.as_str())),
                ("source", J::s(source.as_str())),
            ])
            .dump(PY)
        ),
    );
    let cli = out.join("cli");
    let cli_checks = (|| -> Result<(bool, bool), Cut> {
        let env = fresh_environment(&cli)?;
        // No model here: a port nothing listens on.
        let profile = profile_for(9).map_err(Cut::Failed)?;
        let witness = Witness::default();
        let jail = Jail {
            root: &cli,
            env: &env,
            profile: &profile,
            witness: &witness,
        };
        let help = jail.run(&exe, &["exec", "--help"], "help", RUN_LIMIT)?;
        let text =
            crate::util::read_regular_text(&cli.join("help-stdout.jsonl")).unwrap_or_default();
        let said = jail.run(&exe, &["--version"], "version", RUN_LIMIT)?;
        let version_text =
            crate::util::read_regular_text(&cli.join("version-stdout.jsonl")).unwrap_or_default();
        Ok((
            help_names_the_flags(help, &text),
            says_version(said, &version_text, &version),
        ))
    })();
    let (last, rest) = EXEC_FLAGS.split_last().unwrap_or((&"", &[]));
    for (label, held) in match cli_checks {
        Ok((flags, said)) => vec![
            (
                format!(
                    "capacity: codex exec --help names {} and {last}",
                    rest.join(", ")
                ),
                flags,
            ),
            (
                format!("capacity: codex --version says codex-cli {version}"),
                said,
            ),
        ],
        Err(Cut::Failed(why)) => {
            vec![(format!("capacity: the CLI ran in the jail ({why})"), false)]
        }
        Err(Cut::Stopped(s)) => return Err(s),
    } {
        println!("  {} {label}", if held { "ok  " } else { "FAIL" });
        checks.push((label, held));
    }
    for case in cases_for(cli_holds_quota_recovery(&exe)) {
        checks.extend(run_case(&exe, out, case, profile_for)?);
    }
    Ok(checks)
}

#[cfg(test)]
#[path = "codex_alab_capacity_proof_tests.rs"]
mod tests;
