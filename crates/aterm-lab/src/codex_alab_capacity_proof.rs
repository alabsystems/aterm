// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CAPACITY LANE (`codex-alab-proof --capacity-only`, and the full proof's last leg):
//! codex-alab's headless capacity recovery (its `alab/CAPACITY_RECOVERY.md`) on the REAL
//! package's `codex exec`, in the proof's jail ([`super::policy`]: one loopback port, the
//! owner's homes fenced), against a loopback model of this lane's own. What codex-alab's
//! `alab/smoke/capacity_exec.py` proves, which runs `codex exec` with the Internet open:
//!
//! - the package's records agree (version, target, an exact source commit, `bin/codex` inside
//!   the package), `codex exec --help` names the flags this lane passes, and `codex --version`
//!   says the package's version;
//! - CAPACITY RECOVERY: the model answers the first request with an `update_plan` call, the
//!   second (carrying that call's output) with `503 server_is_overloaded` and `Retry-After: 2`,
//!   the third with the reply. One native turn, three requests, the model and the person's
//!   prompt unchanged in each, the retry's input exactly the overloaded request's (the plan's
//!   output kept), the plan run once and completed, the reply completed, exit 0, and the retry
//!   asked no sooner than `Retry-After` after the overloaded answer's headers began (their
//!   emission bracketed within [`HEADER_EMISSION_LIMIT_S`], or the timing is inconclusive);
//! - PERMANENT REFUSAL: a `403` is asked once, fails the turn, and exits non-zero (no signal).
//!
//! EGRESS, as the lab's other proofs check it: before each case a jailed `nc` reaches the
//! lane's model port and is refused a loopback canary port the jail does not name (whose
//! listener then took nothing), and while `codex exec` runs its process tree's sockets are
//! sampled with `lsof` ([`crate::sampler::endpoints`]), on a clock and by the model at each
//! request before it answers (codex waits on that connection then, so the witness must see
//! it: measured live, codex holds no connection between requests, and a clock alone saw none):
//! no endpoint off loopback, each request's connection seen, and the canary untouched after
//! the run.
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
/// `Retry-After` on the overloaded answer, in seconds.
const RETRY_AFTER_S: f64 = 2.0;
/// The most the overloaded answer's headers may take to write and still time the retry.
const HEADER_EMISSION_LIMIT_S: f64 = 0.050;
/// The most one `codex` run may take: a hang bound.
const RUN_LIMIT: Duration = Duration::from_secs(60);
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

/// Why a jailed run ([`jailed`]) gave no exit.
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

/// Hold `group` as the run going now ([`RUNNING`]), and make killing it the lab's act at a second
/// interrupt ([`stop::at_hard_exit`], which replaces any act registered before it: the
/// codex-alab proof, which runs this lane, registers none). `codex` leads a group of its own,
/// which a terminal's `^C` never reaches, so without that act a second interrupt would end the
/// lab and leave the group running.
fn hold(group: Option<i32>) {
    *RUNNING.lock().unwrap_or_else(PoisonError::into_inner) = group;
    stop::at_hard_exit(end_running_group);
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
    pub body: Option<J>,
    /// Just before its answer's headers were written.
    pub header_started: Option<f64>,
    /// Once they were written and flushed, before the body.
    pub header_sent: Option<f64>,
}

/// The lane's model: one case's answers and what it took.
struct Model {
    negative: bool,
    clock: Instant,
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
}

impl Witness {
    /// Sample the tree once (`at_request`: while the client waits on the model).
    fn sample(&self, at_request: bool) {
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
        let found = crate::sampler::endpoints(&String::from_utf8_lossy(&out.stdout));
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        seen.pids.extend(pids);
        seen.endpoints.extend(found);
        seen.samples += 1;
        if at_request {
            seen.at_requests += 1;
        }
    }
}

impl Seen {
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
    /// something it had to), the tree's connection to the model seen, and the canary untouched
    /// (`canary_touched`).
    fn egress_held(&self, port: u16, requests: usize, canary_touched: bool) -> bool {
        self.external().is_empty()
            && requests > 0
            && self.at_requests == requests
            && self.talked(port)
            && !canary_touched
    }
}

impl Model {
    fn now(&self) -> f64 {
        self.clock.elapsed().as_secs_f64()
    }
}

/// What the model answers the `number`th request (from 1) of a case: `(status, content type,
/// body, Retry-After seconds)`.
pub(super) fn answer(negative: bool, number: usize) -> (u16, &'static str, String, Option<u64>) {
    let overloaded = J::obj([(
        "error",
        J::obj([
            ("code", J::s("server_is_overloaded")),
            ("message", J::s("Model is at capacity.")),
        ]),
    )])
    .dump(PY);
    if negative {
        return (403, "application/json", overloaded, Some(2));
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
            (
                200,
                "text/event-stream",
                crate::codex_rig::sse(vec![call]),
                None,
            )
        }
        2 => (503, "application/json", overloaded, Some(2)),
        3 => (
            200,
            "text/event-stream",
            crate::codex_rig::sse(vec![crate::codex_rig::message_item(ANSWER)]),
            None,
        ),
        _ => (
            400,
            "application/json",
            r#"{"error":{"message":"Unexpected extra request"}}"#.to_string(),
            None,
        ),
    }
}

/// The lane's model on `listener`, a thread per connection, each connection (and its writing
/// half) kept out of the children this lane spawns meanwhile ([`aterm_uds::accept`],
/// [`aterm_uds::kept_out`]): a `codex`, `nc` or `lsof` that took a copy could hold the model's
/// side of a connection open past its answer.
fn serve(listener: TcpListener, model: Arc<Model>) {
    while let Ok((conn, _)) = aterm_uds::accept(&listener) {
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
        let arrived = model.now();
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
                body: J::parse_bytes(&req.body).ok(),
                ..Taken::default()
            });
            taken.len()
        };
        model.witness.sample(true);
        let (status, kind, body, retry_after) = answer(model.negative, number);
        let reason = match status {
            200 => "OK",
            403 => "Forbidden",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let retry = retry_after.map_or_else(String::new, |s| format!("Retry-After: {s}\r\n"));
        let head = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n{retry}\r\n",
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
        }
        if w.write_all(body.as_bytes())
            .and_then(|()| w.flush())
            .is_err()
        {
            return;
        }
    }
}

/// HOW LONG AFTER THE OVERLOADED ANSWER'S HEADERS THE RETRY CAME, measured from its headers
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
    if retry.arrived < (start + RETRY_AFTER_S).next_down() {
        return Err(format!("Retry-After bypassed: {delay:.6}s after headers"));
    }
    Ok(delay)
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
    negative: bool,
    exit: Option<i32>,
    taken: &[Taken],
    events: &[J],
) -> Result<J, String> {
    let name = if negative {
        "permanent-refusal"
    } else {
        "capacity-recovery"
    };
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
        ("case", J::s(name)),
        ("exit", exit.map_or(J::Null, |c| J::Int(i64::from(c)))),
        (
            "requestCount",
            J::Int(i64::try_from(taken.len()).unwrap_or(i64::MAX)),
        ),
        ("events", J::Arr(types.iter().map(|t| J::s(*t)).collect())),
    ]);
    if negative {
        if !exit.is_some_and(|c| c > 0) || taken.len() != 1 {
            return Err(format!(
                "{name}: 403 was retried, signaled or reported success"
            ));
        }
        if count("turn.failed") != 1 || count("turn.completed") != 0 {
            return Err(format!("{name}: 403 did not fail the native turn"));
        }
    } else {
        if exit != Some(0) || taken.len() != 3 {
            return Err(format!(
                "{name}: capacity did not recover in three requests"
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

/// Run `exe args` under the jail `profile` in `root/work` with `env`, its stdout and stderr
/// kept as `root/<label>-stdout.jsonl` and `root/<label>-stderr.txt` and its command as
/// `root/<label>-command.json`, its process tree's sockets sampled into `witness` while it
/// runs. Its exit (`None`: a signal or the hang bound).
///
/// THE RUN'S GROUP ENDS WITH IT: `exe` leads a process group of its own (`sandbox-exec` execs
/// it in place), which [`end_with_group`] kills once `exe` exits, at the hang bound, or at an
/// interrupt of the lab.
/// # Errors
/// [`Cut::Failed`]: its files could not be made, or it could not be spawned or reaped;
/// [`Cut::Stopped`]: an interrupt ended it.
fn jailed(
    exe: &Path,
    args: &[&str],
    root: &Path,
    env: &[(String, String)],
    profile: &str,
    label: &str,
    witness: &Witness,
) -> Result<Option<i32>, Cut> {
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
    let exit = end_with_group(&mut child, witness, RUN_LIMIT);
    *witness
        .root
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    exit
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
        witness.sample(false);
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

/// Whether anything connected to the canary since it was bound (`canary` is nonblocking, so the
/// accept answers at once).
fn canary_touched(canary: &TcpListener) -> bool {
    !matches!(aterm_uds::kept_out(|| canary.accept()), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
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

/// Run one case in `out`; the checks it made, `(label, held)`.
/// # Errors
/// An interrupt of the lab, which ended the case's run (its group killed) or came before it.
fn case(
    exe: &Path,
    out: &Path,
    negative: bool,
    profile_for: &dyn Fn(u16) -> Result<String, String>,
) -> Result<Vec<(String, bool)>, Stopped> {
    stop::check()?;
    let name = if negative {
        "permanent-refusal"
    } else {
        "capacity-recovery"
    };
    let mut checks = Vec::new();
    let mut check = |label: String, held: bool| {
        println!(
            "  {} capacity {name}: {label}",
            if held { "ok  " } else { "FAIL" }
        );
        checks.push((label, held));
    };
    let root = out.join(name);
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
        negative,
        clock: Instant::now(),
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
    let ran = jailed(
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
        &root,
        &env,
        &profile,
        "exec",
        &witness,
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
                    (
                        "headerStartedAt",
                        t.header_started.map_or(J::Null, J::Float),
                    ),
                    ("headerAt", t.header_sent.map_or(J::Null, J::Float)),
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
    // witness that is not blind saw that connection.
    check(
        format!(
            "no socket of codex exec's tree left loopback ({} sample(s), {} at a request; its connection to the model {}; external: {:?}), and the canary took nothing",
            seen.samples,
            seen.at_requests,
            if seen.talked(port) {
                "seen"
            } else {
                "not seen"
            },
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
    match judge(negative, exit, &taken, &events) {
        Ok(result) => {
            let _ = crate::util::write_fresh(
                &root.join("result.json"),
                format!("{}\n", result.dump(PY)),
            );
            check(
                format!(
                    "{} ({} request(s), exit {:?}{})",
                    if negative {
                        "a 403 is asked once, fails the turn and exits non-zero"
                    } else {
                        "one turn recovers from the capacity answer after Retry-After, the plan kept"
                    },
                    taken.len(),
                    exit,
                    result
                        .get("retryDelaySeconds")
                        .and_then(J::as_f64)
                        .map_or_else(String::new, |d| format!(
                            ", retried {d:.3}s after the headers"
                        ))
                ),
                true,
            );
        }
        Err(why) => check(why, false),
    }
    Ok(checks)
}

/// THE LANE: the package's records and CLI, then both cases, each under the jail
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
        let help = jailed(
            &exe,
            &["exec", "--help"],
            &cli,
            &env,
            &profile,
            "help",
            &witness,
        )?;
        let text =
            crate::util::read_regular_text(&cli.join("help-stdout.jsonl")).unwrap_or_default();
        let said = jailed(
            &exe,
            &["--version"],
            &cli,
            &env,
            &profile,
            "version",
            &witness,
        )?;
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
    for negative in [false, true] {
        checks.extend(case(&exe, out, negative, profile_for)?);
    }
    Ok(checks)
}

#[cfg(test)]
mod tests {
    use aterm_spec::wait::{MUST_HAPPEN, PARKED_SECS, until_within, within};

    use super::*;

    fn request(input: Vec<J>) -> Option<J> {
        Some(J::obj([("model", J::s(MODEL)), ("input", J::Arr(input))]))
    }

    fn user() -> J {
        J::obj([
            ("type", J::s("message")),
            ("role", J::s("user")),
            (
                "content",
                J::Arr(vec![J::obj([
                    ("type", J::s("input_text")),
                    ("text", J::s(PROMPT)),
                ])]),
            ),
        ])
    }

    fn output() -> J {
        J::obj([
            ("type", J::s("function_call_output")),
            ("call_id", J::s(CALL_ID)),
            ("output", J::s("Plan updated")),
        ])
    }

    /// The requests of a recovered case: the overloaded answer's headers at
    /// `header_start..header_at`, the retry's first byte at `retry_at`.
    fn taken(header_start: f64, header_at: f64, retry_at: f64) -> Vec<Taken> {
        vec![
            Taken {
                arrived: 0.0,
                body: request(vec![user()]),
                header_started: Some(0.01),
                header_sent: Some(0.011),
            },
            Taken {
                arrived: 10.0,
                body: request(vec![user(), output()]),
                header_started: Some(header_start),
                header_sent: Some(header_at),
            },
            Taken {
                arrived: retry_at,
                body: request(vec![user(), output()]),
                header_started: None,
                header_sent: None,
            },
        ]
    }

    fn events(reply_type: &str, reply_event: &str) -> Vec<J> {
        let plan = J::obj([
            ("type", J::s("todo_list")),
            (
                "items",
                J::Arr(vec![J::obj([
                    ("text", J::s(PLAN)),
                    ("completed", J::Bool(true)),
                ])]),
            ),
        ]);
        vec![
            J::obj([("type", J::s("turn.started"))]),
            J::obj([("type", J::s("item.started")), ("item", plan.clone())]),
            J::obj([("type", J::s("item.completed")), ("item", plan)]),
            J::obj([
                ("type", J::s(reply_event)),
                (
                    "item",
                    J::obj([("type", J::s(reply_type)), ("text", J::s(ANSWER))]),
                ),
            ]),
            J::obj([("type", J::s("turn.completed"))]),
        ]
    }

    fn recovered(header_start: f64, header_at: f64, retry_at: f64) -> Result<J, String> {
        judge(
            false,
            Some(0),
            &taken(header_start, header_at, retry_at),
            &events("agent_message", "item.completed"),
        )
    }

    /// The smoke's own timing rows (`alab/tests/test_capacity_exec.py`): a retry two
    /// seconds after the headers began passes, measured from the headers; one that request
    /// handling before them would have excused, or a fifty-millisecond shortfall, is a bypass;
    /// a bracket of header emission past the limit, backwards, or missing is no measure.
    #[test]
    fn the_retry_is_timed_from_the_overloaded_answers_headers() {
        let passed = recovered(13.0, 13.001, 15.001).expect("a retry after Retry-After");
        assert_eq!(passed.get("passed"), Some(&J::Bool(true)));
        let delay = passed.get("retryDelaySeconds").and_then(J::as_f64).unwrap();
        assert!((delay - 2.0).abs() < 1e-9, "{delay}");
        let overlap = recovered(13.0, 13.04, 15.0).expect("the actual bracket only");
        let delay = overlap
            .get("retryDelaySeconds")
            .and_then(J::as_f64)
            .unwrap();
        let emission = overlap
            .get("retryHeaderEmissionSeconds")
            .and_then(J::as_f64)
            .unwrap();
        assert!((delay - 1.96).abs() < 1e-9 && (emission - 0.04).abs() < 1e-9);
        for retry_at in [14.0, 14.96] {
            let refused = recovered(13.0, 13.001, retry_at).unwrap_err();
            assert!(
                refused.contains("Retry-After bypassed"),
                "{retry_at}: {refused}"
            );
        }
        assert!(
            recovered(13.0, 13.001, 15.0).is_ok(),
            "exactly at the deadline"
        );
        for (start, at) in [
            (13.0, 13.1),
            (13.1, 13.0),
            (13.0, f64::NAN),
            (13.0, f64::INFINITY),
        ] {
            assert!(recovered(start, at, 20.0).is_err(), "{start} {at}");
        }
        let mut missing = taken(13.0, 13.001, 20.0);
        missing[1].header_sent = None;
        let refused = judge(
            false,
            Some(0),
            &missing,
            &events("agent_message", "item.completed"),
        )
        .unwrap_err();
        assert!(refused.contains("missing or invalid"), "{refused}");
    }

    /// The smoke's verdict rows: the completed assistant reply passes; reasoning text
    /// or a started reply does not stand in for it; a plan run twice, a retry that changed the
    /// input, or a lost prompt fails; a refusal passes only with one request, one failed turn
    /// and a non-zero exit, never a signal.
    #[test]
    fn the_verdict_holds_the_smokes_rows() {
        let good = events("agent_message", "item.completed");
        assert!(judge(false, Some(0), &taken(13.0, 13.001, 15.001), &good).is_ok());
        for (kind, event) in [
            ("reasoning", "item.completed"),
            ("agent_message", "item.started"),
        ] {
            let refused = judge(
                false,
                Some(0),
                &taken(13.0, 13.001, 15.001),
                &events(kind, event),
            )
            .unwrap_err();
            assert!(
                refused.contains("reply missing"),
                "{kind} {event}: {refused}"
            );
        }
        // As codex writes them and the parser reads them back: keys in the parser's order.
        let parsed: Vec<J> = [
            r#"{"type":"turn.started"}"#.to_string(),
            format!(
                r#"{{"type":"item.started","item":{{"id":"item_1","type":"todo_list","items":[{{"text":"{PLAN}","completed":true}}]}}}}"#
            ),
            format!(
                r#"{{"type":"item.completed","item":{{"id":"item_2","type":"agent_message","text":"{ANSWER}"}}}}"#
            ),
            format!(
                r#"{{"type":"item.completed","item":{{"id":"item_1","type":"todo_list","items":[{{"text":"{PLAN}","completed":true}}]}}}}"#
            ),
            r#"{"type":"turn.completed"}"#.to_string(),
        ]
        .iter()
        .map(|l| J::parse(l).unwrap())
        .collect();
        assert!(judge(false, Some(0), &taken(13.0, 13.001, 15.001), &parsed).is_ok());
        let mut twice = good.clone();
        twice.insert(3, good[2].clone());
        assert!(
            judge(false, Some(0), &taken(13.0, 13.001, 15.001), &twice)
                .unwrap_err()
                .contains("executed more than once")
        );
        let mut changed = taken(13.0, 13.001, 15.001);
        changed[2].body = request(vec![user()]);
        assert!(
            judge(false, Some(0), &changed, &good)
                .unwrap_err()
                .contains("retry changed")
        );
        let mut lost = taken(13.0, 13.001, 15.001);
        lost[0].body = request(Vec::new());
        assert!(
            judge(false, Some(0), &lost, &good)
                .unwrap_err()
                .contains("lost or duplicated")
        );
        assert!(
            judge(false, Some(1), &taken(13.0, 13.001, 15.001), &good)
                .unwrap_err()
                .contains("three requests")
        );

        let refusal = vec![
            J::obj([("type", J::s("turn.started"))]),
            J::obj([("type", J::s("turn.failed"))]),
        ];
        let one = &taken(13.0, 13.001, 15.001)[..1];
        let passed = judge(true, Some(1), one, &refusal).expect("a refusal");
        assert_eq!(passed.get("exit"), Some(&J::Int(1)));
        for exit in [None, Some(0)] {
            assert!(
                judge(true, exit, one, &refusal)
                    .unwrap_err()
                    .contains("403 was retried"),
                "{exit:?}"
            );
        }
        assert!(
            judge(true, Some(1), &taken(13.0, 13.001, 15.001)[..2], &refusal)
                .unwrap_err()
                .contains("403 was retried")
        );
    }

    /// The verdict rows the smoke's own test never reached (parity-6 review R7): a
    /// request under another model; a refusal whose turn completed, or never failed; a plan
    /// whose completed form changed. CONTROL: each case passes with the row's subject restored.
    #[test]
    fn the_verdict_holds_the_rows_the_smokes_test_left_out() {
        let good = events("agent_message", "item.completed");
        let mut other = taken(13.0, 13.001, 15.001);
        other[1].body = Some(J::obj([
            ("model", J::s("another-model")),
            ("input", J::Arr(vec![user(), output()])),
        ]));
        assert!(
            judge(false, Some(0), &other, &good)
                .unwrap_err()
                .contains("model changed")
        );
        assert!(judge(false, Some(0), &taken(13.0, 13.001, 15.001), &good).is_ok());

        let one = &taken(13.0, 13.001, 15.001)[..1];
        let turn = |t: &str| J::obj([("type", J::s(t))]);
        for (said, ends) in [
            ("completed", vec!["turn.completed"]),
            (
                "failed and completed",
                vec!["turn.failed", "turn.completed"],
            ),
            ("never failed", Vec::new()),
            ("failed twice", vec!["turn.failed", "turn.failed"]),
        ] {
            let mut refusal = vec![turn("turn.started")];
            refusal.extend(ends.into_iter().map(turn));
            assert!(
                judge(true, Some(1), one, &refusal)
                    .unwrap_err()
                    .contains("403 did not fail the native turn"),
                "{said}"
            );
        }
        assert!(
            judge(
                true,
                Some(1),
                one,
                &[turn("turn.started"), turn("turn.failed")]
            )
            .is_ok()
        );

        for (said, items) in [
            (
                "left incomplete",
                J::Arr(vec![J::obj([
                    ("text", J::s(PLAN)),
                    ("completed", J::Bool(false)),
                ])]),
            ),
            (
                "another step",
                J::Arr(vec![J::obj([
                    ("text", J::s("another step")),
                    ("completed", J::Bool(true)),
                ])]),
            ),
            ("no steps", J::Arr(Vec::new())),
        ] {
            let mut changed = good.clone();
            changed[2] = J::obj([
                ("type", J::s("item.completed")),
                (
                    "item",
                    J::obj([("type", J::s("todo_list")), ("items", items)]),
                ),
            ]);
            assert!(
                judge(false, Some(0), &taken(13.0, 13.001, 15.001), &changed)
                    .unwrap_err()
                    .contains("plan changed"),
                "{said}"
            );
        }
    }

    /// The model's answers: the plan call, the overloaded answer with Retry-After, the reply,
    /// then a refusal of any extra request; a negative case is refused with 403 every time.
    #[test]
    fn the_lanes_model_answers_as_the_smokes_did() {
        let (status, kind, body, retry) = answer(false, 1);
        assert_eq!((status, kind, retry), (200, "text/event-stream", None));
        assert!(body.contains(CALL_ID) && body.contains("update_plan"));
        assert_eq!(answer(false, 2).0, 503);
        assert_eq!(answer(false, 2).3, Some(2));
        assert!(answer(false, 2).2.contains("server_is_overloaded"));
        assert!(answer(false, 3).2.contains(ANSWER));
        assert_eq!(answer(false, 4).0, 400);
        assert_eq!(answer(true, 1).0, 403);
        assert_eq!(answer(true, 2).0, 403);
    }

    /// The model stamps each answer's headers on that answer, between its request's arrival
    /// and its body, over a real loopback connection.
    #[test]
    fn the_model_stamps_each_answers_headers_before_its_body() {
        let listener = aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).unwrap();
        let port = listener.local_addr().unwrap().port();
        let model = Arc::new(Model {
            negative: false,
            clock: Instant::now(),
            taken: Mutex::new(Vec::new()),
            witness: Arc::new(Witness::default()),
        });
        {
            let model = Arc::clone(&model);
            std::thread::spawn(move || serve(listener, model));
        }
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        conn.set_read_timeout(Some(MUST_HAPPEN)).unwrap();
        let body = J::obj([("model", J::s(MODEL))]).dump(PY);
        for _ in 0..2 {
            write!(
                conn,
                "POST /v1/responses HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut r = std::io::BufReader::new(conn.try_clone().unwrap());
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut rest = vec![0; length];
            std::io::Read::read_exact(&mut r, &mut rest).unwrap();
        }
        let taken = model.taken.lock().unwrap().clone();
        assert_eq!(taken.len(), 2);
        for t in &taken {
            let (start, sent) = (t.header_started.unwrap(), t.header_sent.unwrap());
            assert!(t.arrived <= start && start <= sent, "{t:?}");
        }
    }

    /// The `sleep` a run left: the pid its script wrote to `root/left.pid`.
    fn left(root: &Path) -> i32 {
        crate::util::read_regular_text(&root.join("left.pid"))
            .expect("the run wrote its sleep's pid")
            .trim()
            .parse()
            .expect("a pid")
    }

    /// Wait until `pid` ended: gone, or a zombie its new parent has yet to reap.
    fn ends(pid: i32, at: &str) {
        // Each look runs `ps`: 50 ms apart, not `until_within`'s 10.
        let ended = within(MUST_HAPPEN, Duration::from_millis(50), || {
            crate::procs::state_of(pid)
                .is_none_or(|stat| stat.starts_with('Z'))
                .then_some(())
        });
        assert!(
            ended.is_some(),
            "{at}: the sleep it left ({pid}) outlived the run"
        );
    }

    /// A run in `root` that leads a process group of its own and waits on a `sleep` it started
    /// in it (living [`PARKED_SECS`]), once that `sleep`'s pid is in `root/left.pid`. Neither
    /// holds this process's stdout or stderr, so a probe's parent reads the probe's end when the
    /// probe ends, not when what it left does.
    fn waiting_run(root: &Path) -> aterm_uds::spawnfd::Child {
        let child = aterm_uds::spawnfd::command("/bin/sh")
            .args([
                "-c",
                &format!(
                    "/bin/sleep {PARKED_SECS} & echo $! > left.new && /bin/mv left.new left.pid; wait"
                ),
            ])
            .current_dir(root)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the run starts");
        until_within(MUST_HAPPEN, "the run's start of its sleep", || {
            root.join("left.pid").exists().then_some(())
        });
        child
    }

    /// THE RUN'S GROUP ENDS WITH IT: a `sleep` a run left in its group is killed once the run
    /// exits (through [`jailed`] itself: `sandbox-exec`, a profile that fences nothing), and
    /// once a run that waits on it reaches its bound (through [`end_with_group`], called with
    /// the bound already past once the `sleep` has started), which reads as no exit. CONTROL:
    /// with only the leader killed at the bound, as before, the `sleep` outlives the run in
    /// both cases and the wait for its end runs out.
    #[test]
    fn what_a_run_leaves_in_its_group_ends_at_its_exit_and_at_its_bound() {
        let sleep = format!("/bin/sleep {PARKED_SECS}");

        let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
        let root = tmp.path();
        let env = fresh_environment(root).expect("the run's scratch");
        let ran = jailed(
            Path::new("/bin/sh"),
            &["-c", &format!("{sleep} & echo $! > ../left.pid")],
            root,
            &env,
            "(version 1)(allow default)",
            "group",
            &Witness::default(),
        )
        .expect("the run starts");
        assert_eq!(ran, Some(0), "the run's own exit");
        ends(left(root), "at the run's exit");

        let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
        let root = tmp.path();
        let mut child = waiting_run(root);
        let ran = end_with_group(&mut child, &Witness::default(), Duration::ZERO)
            .expect("the run is reaped");
        assert_eq!(ran, None, "a run stopped at its bound has no exit");
        ends(left(root), "at the run's bound");
    }

    const VERSION: &str = "0.1.0-dev.1+codex.0.166.0.gabcdef012";
    const SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";
    const TARGET: &str = "aarch64-apple-darwin";

    /// A package in a fresh scratch dir with `build` as its `alab-build.json`, `manifest` as its
    /// `codex-package.json` (each a JSON object of strings) and a regular `bin/codex`.
    fn records(build: [(&str, &str); 3], manifest: [(&str, &str); 3]) -> aterm_tempfile::TempDir {
        let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("bin")).expect("its bin");
        let obj = |fields: [(&str, &str); 3]| J::obj(fields.map(|(k, v)| (k, J::s(v)))).dump(PY);
        crate::util::write_fresh(&root.join("alab-build.json"), obj(build)).expect("its build");
        crate::util::write_fresh(&root.join("codex-package.json"), obj(manifest))
            .expect("its manifest");
        crate::util::write_fresh(&root.join("bin/codex"), "#!/bin/sh\n").expect("its codex");
        tmp
    }

    /// A package whose records agree.
    fn agreed() -> aterm_tempfile::TempDir {
        records(
            [("version", VERSION), ("target", TARGET), ("source", SOURCE)],
            [
                ("version", VERSION),
                ("target", TARGET),
                ("entrypoint", "bin/codex"),
            ],
        )
    }

    /// THE PACKAGE'S RECORDS (review F1): records that agree give the version, the exact source
    /// and the entrypoint inside the package; a version or target that disagrees, a source that
    /// is not 40 lowercase hex, another entrypoint, a `bin/codex` that is missing, a directory or
    /// a link out of the package, and a record that is missing, a link, oversized or not JSON
    /// are each refused, by name.
    #[test]
    fn the_package_records_agree_and_name_an_entrypoint_inside_the_package() {
        let refused =
            |pkg: &aterm_tempfile::TempDir| package_records(pkg.path()).expect_err("refused");
        let good = agreed();
        let (version, source, exe) = package_records(good.path()).expect("records that agree");
        assert_eq!((version.as_str(), source.as_str()), (VERSION, SOURCE));
        assert_eq!(
            exe,
            std::fs::canonicalize(good.path().join("bin/codex")).expect("its codex")
        );

        let pkg = |build_version, target, source, entry| {
            records(
                [
                    ("version", build_version),
                    ("target", target),
                    ("source", source),
                ],
                [
                    ("version", VERSION),
                    ("target", TARGET),
                    ("entrypoint", entry),
                ],
            )
        };
        let other = pkg("0.1.0-dev.2", TARGET, SOURCE, "bin/codex");
        assert_eq!(refused(&other), "package versions disagree");
        let other = pkg(VERSION, "x86_64-apple-darwin", SOURCE, "bin/codex");
        assert_eq!(refused(&other), "package targets disagree");
        let (longer, upper, not_hex) = (
            format!("{SOURCE}0"),
            SOURCE.to_ascii_uppercase(),
            SOURCE.replace('a', "g"),
        );
        let sources: [&str; 5] = [&SOURCE[..39], &longer, &upper, &not_hex, ""];
        for source in sources {
            let other = pkg(VERSION, TARGET, source, "bin/codex");
            assert_eq!(refused(&other), "missing exact source commit", "{source:?}");
        }
        let other = pkg(VERSION, TARGET, SOURCE, "bin/other");
        assert_eq!(refused(&other), "unexpected package entrypoint");

        let outside = aterm_tempfile::tempdir().expect("a scratch dir");
        let elsewhere = outside.path().join("codex");
        crate::util::write_fresh(&elsewhere, "#!/bin/sh\n").expect("a codex elsewhere");
        let linked = agreed();
        std::fs::remove_file(linked.path().join("bin/codex")).expect("its codex");
        std::os::unix::fs::symlink(&elsewhere, linked.path().join("bin/codex"))
            .expect("a link out");
        assert_eq!(refused(&linked), "entrypoint escapes package");
        let missing = agreed();
        std::fs::remove_file(missing.path().join("bin/codex")).expect("its codex");
        assert_eq!(refused(&missing), "entrypoint escapes package");
        std::fs::create_dir(missing.path().join("bin/codex")).expect("a directory");
        assert_eq!(refused(&missing), "entrypoint escapes package");

        let gone = agreed();
        std::fs::remove_file(gone.path().join("alab-build.json")).expect("its build");
        assert_eq!(refused(&gone), "missing regular alab-build.json");
        let link = agreed();
        let manifest = link.path().join("codex-package.json");
        std::fs::rename(&manifest, link.path().join("real.json")).expect("moved");
        std::os::unix::fs::symlink("real.json", &manifest).expect("a link");
        assert_eq!(refused(&link), "missing regular codex-package.json");
        let big = agreed();
        crate::util::write_fresh(
            &big.path().join("alab-build.json"),
            vec![b' '; 1024 * 1024 + 1],
        )
        .expect("oversized");
        assert_eq!(refused(&big), "oversized alab-build.json");
        let torn = agreed();
        crate::util::write_fresh(&torn.path().join("alab-build.json"), "{").expect("torn");
        assert!(
            refused(&torn).starts_with("alab-build.json: "),
            "{}",
            refused(&torn)
        );
    }

    /// The witness's view of a run, from `lsof -F pcPtn` output.
    fn seen(lsof: &str, pids: &[i32], at_requests: usize) -> Seen {
        Seen {
            endpoints: crate::sampler::endpoints(lsof),
            pids: pids.iter().copied().collect(),
            samples: at_requests + 2,
            at_requests,
        }
    }

    /// THE EGRESS VERDICT (review F1): a tree seen only on loopback, sampled at each of the
    /// model's requests, its connection to the model seen and the canary untouched, holds; a
    /// socket off loopback, a request the witness did not sample at, a run with no request at
    /// all, a connection to another port or by a pid outside the tree, and a touched canary each
    /// fail it.
    #[test]
    fn the_egress_verdict_holds_a_loopback_tree_seen_at_every_request_alone() {
        let conn = "p4100\nccodex\nPTCP\nn127.0.0.1:61000->127.0.0.1:5000\n";
        let good = seen(conn, &[4100], 3);
        assert!(good.egress_held(5000, 3, false));
        assert_eq!(good.log(), "codex[4100] TCP conn 127.0.0.1:5000\n");

        let out = seen(
            &format!("{conn}n192.168.1.9:61001->17.253.144.10:443\n"),
            &[4100],
            3,
        );
        assert_eq!(out.external().len(), 1, "{:?}", out.endpoints);
        assert!(!out.egress_held(5000, 3, false), "a socket off loopback");
        assert!(
            !good.egress_held(5000, 4, false),
            "a request not sampled at"
        );
        assert!(
            !seen(conn, &[4100], 0).egress_held(5000, 0, false),
            "no request at all"
        );
        assert!(
            !good.egress_held(5001, 3, false),
            "the model's port not seen"
        );
        assert!(
            !seen(conn, &[4200], 3).egress_held(5000, 3, false),
            "a pid outside the tree"
        );
        assert!(!good.egress_held(5000, 3, true), "the canary touched");
    }

    /// THE JAIL PROBE (review F1): under the lane's network rule ([`crate::codex_rig::jail_policy`])
    /// a jailed `nc` reaches the model's port and is refused the canary's, which took nothing;
    /// with the jail opened to every loopback port (parity-6's live negative control) it reaches
    /// the canary, which saw it; with no network at all it does not reach the model either. And
    /// the canary reads untouched until something connects to it.
    #[test]
    fn the_jail_probe_holds_a_jail_that_names_the_models_port_alone() {
        let listen = || {
            let listener = aterm_uds::kept_out(|| TcpListener::bind("127.0.0.1:0")).unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            (listener, port)
        };
        let (_model, port) = listen();
        let jail = crate::codex_rig::jail_policy(port);
        let (canary, _) = listen();
        assert!(jail_holds(&jail, port, &canary), "the lane's jail");
        assert!(!canary_touched(&canary), "the lane's jail kept the canary");

        let open = format!("{jail}(allow network-outbound (remote ip \"localhost:*\"))");
        let (canary, _) = listen();
        assert!(!jail_holds(&open, port, &canary), "a jail open to loopback");
        assert!(canary_touched(&canary), "the open jail's probe reached it");
        let (canary, _) = listen();
        assert!(
            !jail_holds("(version 1)(allow default)(deny network*)", port, &canary),
            "no network"
        );

        let (canary, canary_port) = listen();
        assert!(!canary_touched(&canary));
        let _conn = aterm_uds::kept_out(|| TcpStream::connect(("127.0.0.1", canary_port)))
            .expect("a connection");
        assert!(canary_touched(&canary), "a connection to it");
    }

    /// THE CLI CHECKS (review F1): `exec --help` holds only on exit 0 with every flag the cases
    /// pass named, `--version` only on exit 0 saying the package's exact version.
    #[test]
    fn the_cli_checks_hold_a_clean_exit_naming_the_flags_and_the_version() {
        let help = "Usage: codex exec [OPTIONS] [PROMPT]\n      --json\n      --color <COLOR>\n      \
                    --skip-git-repo-check\n  -C, --cd <DIR>\n  -s, --sandbox <MODE>\n";
        assert!(help_names_the_flags(Some(0), help));
        for exit in [Some(1), Some(2), None] {
            assert!(!help_names_the_flags(exit, help), "{exit:?}");
        }
        for flag in EXEC_FLAGS {
            assert!(
                !help_names_the_flags(Some(0), &help.replace(flag, "--gone")),
                "{flag}"
            );
        }

        let said = format!("codex-cli {VERSION}\n");
        assert!(says_version(Some(0), &said, VERSION));
        for exit in [Some(1), None] {
            assert!(!says_version(exit, &said, VERSION), "{exit:?}");
        }
        for text in [
            "codex-cli 0.1.0-dev.2+codex.0.166.0.gabcdef012\n".to_string(),
            format!("codex-cli {VERSION}-extra\n"),
            format!("codex {VERSION}\n"),
            String::new(),
        ] {
            assert!(!says_version(Some(0), &text, VERSION), "{text:?}");
        }
    }

    /// EACH CASE'S CODEX (review F1) asks the lane's loopback model alone, with no login, and
    /// retries nothing itself, so the retry the lane times is codex-alab's capacity recovery;
    /// its updater, analytics, feedback and telemetry are off.
    #[test]
    fn each_cases_codex_asks_the_lanes_model_alone_and_leaves_retrying_to_codex_alab() {
        let doc: aterm_toml::Value = aterm_toml::from_str(&case_config(5000)).expect("valid TOML");
        assert_eq!(doc["model"].as_str(), Some(MODEL));
        assert_eq!(doc["model_provider"].as_str(), Some("capacity-smoke"));
        assert_eq!(doc["check_for_update_on_startup"].as_bool(), Some(false));
        let provider = &doc["model_providers"]["capacity-smoke"];
        assert_eq!(
            provider["base_url"].as_str(),
            Some("http://127.0.0.1:5000/v1")
        );
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(false));
        assert_eq!(provider["request_max_retries"].as_integer(), Some(0));
        assert_eq!(provider["stream_max_retries"].as_integer(), Some(0));
        for table in ["analytics", "feedback"] {
            assert_eq!(doc[table]["enabled"].as_bool(), Some(false), "{table}");
        }
        for exporter in ["exporter", "metrics_exporter", "trace_exporter"] {
            assert_eq!(doc["otel"][exporter].as_str(), Some("none"), "{exporter}");
        }
    }

    /// The verdict rows no test reached before (review F3): a second native turn, or none; a
    /// recovered case whose turn did not complete, failed too or completed twice; a tool output
    /// lost, under another call, or kept twice; and a plan tool whose output does not say it
    /// updated the plan. CONTROL: the plan's output as a JSON value saying so passes, as the
    /// unchanged case does in every test above.
    #[test]
    fn the_verdict_fails_a_turn_too_many_an_unrecovered_turn_a_lost_output_and_an_unrun_plan() {
        let good = events("agent_message", "item.completed");
        let ok = taken(13.0, 13.001, 15.001);
        let turn = |t: &str| J::obj([("type", J::s(t))]);
        let mut two = good.clone();
        two.insert(1, turn("turn.started"));
        let none: Vec<J> = good[1..].to_vec();
        for (said, events) in [("two", &two), ("none", &none)] {
            assert!(
                judge(false, Some(0), &ok, events)
                    .unwrap_err()
                    .contains("expected one native turn"),
                "{said}"
            );
        }
        let refusal = [
            turn("turn.started"),
            turn("turn.started"),
            turn("turn.failed"),
        ];
        assert!(
            judge(true, Some(1), &ok[..1], &refusal)
                .unwrap_err()
                .contains("expected one native turn")
        );

        let mut uncompleted = good.clone();
        uncompleted.pop();
        let mut failed = good.clone();
        failed.push(turn("turn.failed"));
        let mut twice = good.clone();
        twice.push(turn("turn.completed"));
        for (said, events) in [
            ("not completed", &uncompleted),
            ("failed too", &failed),
            ("completed twice", &twice),
        ] {
            assert!(
                judge(false, Some(0), &ok, events)
                    .unwrap_err()
                    .contains("turn did not recover"),
                "{said}"
            );
        }

        let with = |outputs: Vec<J>| {
            let mut t = taken(13.0, 13.001, 15.001);
            let mut input = vec![user()];
            input.extend(outputs);
            t[1].body = request(input.clone());
            t[2].body = request(input);
            t
        };
        let other_call = J::obj([
            ("type", J::s("function_call_output")),
            ("call_id", J::s("another-call")),
            ("output", J::s("Plan updated")),
        ]);
        for (said, outputs) in [
            ("lost", Vec::new()),
            ("another call's", vec![other_call]),
            ("kept twice", vec![output(), output()]),
        ] {
            assert!(
                judge(false, Some(0), &with(outputs), &good)
                    .unwrap_err()
                    .contains("completed tool output lost"),
                "{said}"
            );
        }

        let said = |out: J| {
            with(vec![J::obj([
                ("type", J::s("function_call_output")),
                ("call_id", J::s(CALL_ID)),
                ("output", out),
            ])])
        };
        for out in [
            J::s("Plan failed"),
            J::Null,
            J::obj([("ok", J::Bool(false))]),
        ] {
            assert!(
                judge(false, Some(0), &said(out.clone()), &good)
                    .unwrap_err()
                    .contains("plan tool did not complete"),
                "{out:?}"
            );
        }
        assert!(
            judge(
                false,
                Some(0),
                &said(J::obj([("content", J::s("Plan updated"))])),
                &good
            )
            .is_ok()
        );
    }

    const STOP_PROBE: &str = "ATERM_LAB_CAPACITY_STOP_PROBE";
    const STOP_DIR: &str = "ATERM_LAB_CAPACITY_STOP_DIR";

    /// AN INTERRUPT OF THE LAB ENDS THE RUN GOING AND WHAT IT LEFT IN ITS GROUP (review F4), each
    /// probed in a copy of this test binary, since an interrupt is the whole process's: a first
    /// ([`stop::note`], as the handler [`stop::install`] sets notes one) ends the wait at once
    /// with [`Cut::Stopped`], its bound a minute off, the `sleep` the run left killed; a second
    /// real `SIGINT`, which ends the lab at once (128 + 2), kills the held group first.
    /// CONTROL: with the wait a plain sleep, the first runs to its bound and reads as no exit;
    /// with no act at a second interrupt, the `sleep` outlives the lab.
    #[test]
    fn an_interrupt_ends_the_run_and_what_it_left_in_its_group() {
        const TEST: &str = "codex_alab_proof::capacity_proof::tests::an_interrupt_ends_the_run_and_what_it_left_in_its_group";
        if let Some(mode) = std::env::var_os(STOP_PROBE) {
            let root = PathBuf::from(std::env::var_os(STOP_DIR).expect("the probe's scratch"));
            let mut child = waiting_run(&root);
            if mode == "first" {
                stop::note(libc::SIGINT);
                let ran = end_with_group(&mut child, &Witness::default(), MUST_HAPPEN);
                assert!(
                    matches!(ran, Err(Cut::Stopped(Stopped(libc::SIGINT)))),
                    "{ran:?}"
                );
                println!("PROBE the first interrupt ended the run");
                std::process::exit(0);
            }
            crate::sys::start_signal(libc::SIGINT, false);
            stop::install();
            hold(i32::try_from(child.id()).ok());
            let me = i32::try_from(std::process::id()).expect("pid");
            crate::sys::kill(me, libc::SIGINT).expect("the first interrupt");
            assert_eq!(
                stop::sleep(MUST_HAPPEN),
                Err(Stopped(libc::SIGINT)),
                "the first interrupt is noted"
            );
            crate::sys::kill(me, libc::SIGINT).expect("the second interrupt");
            std::thread::sleep(MUST_HAPPEN);
            println!("PROBE still running");
            std::process::exit(0);
        }
        for (mode, code, said) in [
            ("first", 0, "PROBE the first interrupt ended the run"),
            ("second", 128 + libc::SIGINT, ""),
        ] {
            let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
            let mut cmd =
                aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
            cmd.args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                .env(STOP_PROBE, mode)
                .env(STOP_DIR, tmp.path());
            let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + MUST_HAPPEN * 2)
                .expect("the probe ends within its hang detector");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert_eq!(out.status.code(), Some(code), "{mode}: {stdout}");
            assert!(stdout.contains(said), "{mode}: {stdout}");
            ends(left(tmp.path()), &format!("at the {mode} interrupt"));
        }
    }
}
