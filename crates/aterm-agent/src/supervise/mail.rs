// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mail as the manager's channel: `watch --mail` / `supervise --mail` park one
//! `await inbox` on the manager's own session beside the screen loop, and
//! `aterm drive task` sends a worker its work by mail.
//!
//! A worker's end-of-turn `report` wakes the loop with one line, `EVENT turn
//! seq=<n> report=<id> rows=<n> <summary>`; the manager reads the body with
//! `inbox get <id>`.
//!
//! The lane ([`Lane`]) is its own thread with its own control client. It parks
//! `await inbox since=<id>` with the loop's step, prints `MAIL id=<n> off=<o>
//! from=<sid> kind=<k> len=<n> [re=<o>]` per new row, hands the watched
//! worker's `report` to the loop ([`Delivery`]), and re-arms from the newest id.
//! After an outage it re-baselines, since a successor instance restarts row
//! ids at 1; rows are told apart by bus offset (`off=`), monotone across
//! instances.
//!
//! [`task`] posts `kind=task` (the body travels by mail, never through the
//! PTY) and types the nudge `Inbox: task @<off>` only when the worker is idle,
//! its Enter guarded on the composer's caret row. With `--wait` it parks for
//! the `answer|report|ack` carrying `re=<off>`, re-armed past the host's 600 s
//! clamp.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::phase::{Phase, worker_phase};
use super::run::{
    Ctl, CtlReply, EXIT_TIMEOUT, RECONNECT_PAUSE, RECONNECT_PAUSE_MAX, Session, composer_guard,
};

/// `--report-window`'s default: how long before its `EVENT idle` a worker's
/// `report` mail may have come to be folded into it when the loop cannot
/// tell which turn it belongs to ([`MailOpts::report_window`]).
pub const DEFAULT_REPORT_WINDOW: Duration = Duration::from_secs(120);
/// `--idle-grace`'s default: how long an idle point waits for the worker's
/// report before it is said as `EVENT idle-no-report`. Bound the manager's
/// handoff delay: a late report still arrives on its own `MAIL` line, and
/// callers waiting for a slow reporter can extend this with `--idle-grace`.
pub const DEFAULT_IDLE_GRACE: Duration = Duration::from_secs(5);
/// The selector of the manager's own session, unless `--inbox` names one.
pub const SELF: &str = "@self";
/// Every kind the endpoint delivers, so a lone `note` wakes the lane too
/// (`await inbox` skips notes unless told otherwise); a host that does not
/// know one of them answers a usage line, and the lane re-arms without the
/// list.
const MAIL_KINDS: &str = "ask,answer,task,report,note,control,ack,expired,undeliverable";
/// What `task --wait` waits for: the kinds that answer a task.
const ANSWER_KINDS: &str = "answer,report,ack";
/// The nudge `task` types when the worker is idle: one line, the offset the
/// worker's `inbox` row carries as `off=`.
pub const NUDGE: &str = "Inbox: task @";
/// The nudge's `turn` is not waited on: it settles in `idle=` ms or its
/// verdict says `status=timeout` after this long — `submitted=1` is what
/// counts.
const NUDGE_IDLE: &str = "idle=600";
const NUDGE_TIMEOUT: &str = "timeout=2500";

/// `--mail`'s knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailOpts {
    /// `--inbox @<sid>`: the manager's session; `None` is [`SELF`].
    pub inbox: Option<String>,
    /// `--report-window`: how long before its idle point a report may have
    /// come when the loop never saw the turn begin (its first turn; one too
    /// short to be read busy) — the only thing then known about it. A report
    /// from after the worker was read busy for the turn is the turn's at any
    /// age; one from before the last point handed over never is.
    pub report_window: Duration,
    /// `--idle-grace`.
    pub idle_grace: Duration,
}

impl Default for MailOpts {
    fn default() -> Self {
        Self {
            inbox: None,
            report_window: DEFAULT_REPORT_WINDOW,
            idle_grace: DEFAULT_IDLE_GRACE,
        }
    }
}

/// `key=value` of a row's words.
pub(super) fn field<'r>(row: &'r str, key: &str) -> Option<&'r str> {
    row.split(' ')
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// A sid as the mail rows spell it (`@s-…`, `s-…@n-…`), bare.
pub(super) fn bare_sid(s: &str) -> &str {
    let s = s.trim_start_matches('@');
    s.split_once('@').map_or(s, |(sid, _)| sid)
}

/// Whether `from`, a row's attested sender, is the session `worker` names
/// (`@s-…`/`s-…`, or `@s-…@n-…` for a session on another node) — the WHOLE
/// principal, never the sid alone.
///
/// A session speaks on the bus as `<sid>@<node>`, and only the node half is
/// attested: the broker forces it from the sending node's cap, while the sid is
/// that node's own word about which of its sessions spoke. So `s-w@n-evil` is
/// any node calling one of its posts `s-w`, and matching the sid alone handed it
/// the worker's report and the worker's answer to a task. A worker named with
/// its node must match that node. A worker named bare leaves the node to the
/// bridge, which renders no other node's claim to it: a remote node's claim to
/// speak for a session hosted here, or pinned to another node, is attributed
/// to that node itself (`aterm-link`'s `render_from`), and its reply to a task
/// that went elsewhere arrives demoted. A bare principal (`h-…`, `n-…`) is a
/// human or a node speaking as itself, never a session.
pub(super) fn is_from_session(from: &str, worker: &str) -> bool {
    let worker = worker.strip_prefix('@').unwrap_or(worker);
    let Some((sid, node)) = from.split_once('@') else {
        return false;
    };
    if !node.starts_with("n-") {
        return false;
    }
    match worker.split_once('@') {
        Some((want_sid, want_node)) => sid == want_sid && node == want_node,
        None => sid == worker,
    }
}

fn num(row: &str, key: &str) -> Option<u64> {
    field(row, key)?.parse().ok()
}

/// One `msg` row of `inbox --meta`, as the endpoint prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailRow {
    pub id: u64,
    pub off: u64,
    /// The endpoint's process-clock stamp.
    pub t: u64,
    /// The attested sender, verbatim (`s-…@n-…`, `h-…`).
    pub from: String,
    pub kind: String,
    pub trust: String,
    /// The offset this row answers.
    pub re: Option<u64>,
    pub len: u64,
}

impl MailRow {
    /// `msg <id> off=<n> t=<ms> from=<p> kind=<k> trust=<t> [re=<n>] … len=<n>`;
    /// `None` for any other row.
    pub fn parse(row: &str) -> Option<Self> {
        let rest = row.strip_prefix("msg ")?;
        let id = rest.split(' ').next()?.parse().ok()?;
        Some(MailRow {
            id,
            off: num(rest, "off").unwrap_or(0),
            t: num(rest, "t").unwrap_or(0),
            from: field(rest, "from").unwrap_or("-").to_string(),
            kind: field(rest, "kind").unwrap_or("-").to_string(),
            trust: field(rest, "trust").unwrap_or("-").to_string(),
            re: num(rest, "re"),
            len: num(rest, "len").unwrap_or(0),
        })
    }

    /// The line the loop prints per delivery: `MAIL id=<n> off=<o>
    /// from=<sid> kind=<k> len=<n> [re=<o>]`.
    pub fn line(&self) -> String {
        let mut s = format!(
            "MAIL id={} off={} from={} kind={} len={}",
            self.id, self.off, self.from, self.kind, self.len
        );
        if let Some(re) = self.re {
            s.push_str(&format!(" re={re}"));
        }
        s
    }

    /// Whether this is `worker`'s `report`: from the worker's whole principal,
    /// its node included ([`is_from_session`]).
    pub fn is_report_from(&self, worker: &str) -> bool {
        self.kind == "report" && is_from_session(&self.from, worker)
    }
}

/// The `msg` rows of an `inbox` reply, in the order listed.
pub fn parse_rows(body: &str) -> Vec<MailRow> {
    body.lines().filter_map(MailRow::parse).collect()
}

/// The newest row id an `inbox` reply lists (`0` with none): the `since=`
/// an `await inbox` arms from.
pub fn newest_id(body: &str) -> u64 {
    parse_rows(body).iter().map(|r| r.id).max().unwrap_or(0)
}

/// How many rows a mail body holds (a trailing newline ends the last row, it
/// does not start another). A stamped report's first line (`seq=<n>
/// hash=<hex16>[ busy=1]`) is not a row, so `EVENT <sid> turn … rows=<n>` is
/// the body's row count; an unstamped body is unaffected.
pub fn body_rows(body: &str) -> usize {
    body.lines().filter(|l| !is_report_stamp(l)).count()
}

/// `seq=<digits> hash=<16 hex>[ busy=1]` and nothing else — the stamp line a
/// stamped report body opens with.
fn is_report_stamp(line: &str) -> bool {
    let mut toks = line.split(' ');
    let (Some(seq), Some(hash)) = (toks.next(), toks.next()) else {
        return false;
    };
    let tail = toks.next();
    if toks.next().is_some() || !matches!(tail, None | Some("busy=1")) {
        return false;
    }
    let (Some(seq), Some(hash)) = (seq.strip_prefix("seq="), hash.strip_prefix("hash=")) else {
        return false;
    };
    !seq.is_empty() && seq.chars().all(|c| c.is_ascii_digit()) && aterm_codec::hex::is_hex(hash, 16)
}

/// What the lane hands the loop: a `report` from the watched worker, with
/// when it came and how many rows its body holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub at: Instant,
    pub id: u64,
    pub off: u64,
    pub rows: usize,
}

/// The line said when the lane cannot go on; the loop goes on without mail.
fn lane_off(why: &str) -> String {
    format!(
        "MAIL off: {} (the loop goes on without mail)",
        one_line(why)
    )
}

fn one_line(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The mail lane: a control client of its own, parked on the manager's
/// inbox.
pub struct Lane<'l, L: Ctl> {
    pub ctl: &'l mut L,
    /// The manager's session selector (`@self`, or `--inbox`'s).
    pub inbox: String,
    /// How long one `await inbox` parks before it is re-armed (the loop's
    /// 20 s step in production; a test's few milliseconds).
    pub step: Duration,
    /// How long a request the server did not serve is retried before the
    /// lane gives up (the loop's reconnect window).
    pub reconnect: Duration,
    /// The first pause between retries, and the longest (each doubles).
    pub pause: Duration,
    pub pause_max: Duration,
}

/// What one lane request came back as.
enum Got {
    /// The reply, and whether a request the server did not serve had to be
    /// retried to get it — an outage ridden out, after which the instance
    /// answering may be another one (`Lane::resync`).
    Reply(CtlReply, bool),
    /// The loop ended while the request was retried.
    Stopped,
}

/// Where the lane stands in the inbox: the newest row id it has listed (the
/// next `since=`) and that row's bus offset (what survives a handoff).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Mark {
    id: u64,
    off: u64,
}

impl Mark {
    /// The newest row of an `inbox` reply (`Mark::default()` with none).
    fn newest(body: &str) -> Self {
        parse_rows(body)
            .iter()
            .max_by_key(|r| r.id)
            .map_or_else(Self::default, |r| Mark {
                id: r.id,
                off: r.off,
            })
    }
    fn advance(&mut self, row: &MailRow) {
        self.id = self.id.max(row.id);
        self.off = self.off.max(row.off);
    }
}

impl<L: Ctl> Lane<'_, L> {
    /// One request of the lane's, a request the server did not serve
    /// ([`CtlReply::unserved`]) retried after a pause that doubles up to
    /// `pause_max`, for at most `reconnect`; `Err` for a client that could not
    /// be launched or a window that lapsed. The reply says whether it took a
    /// retry to get it. `outage`: one is open already (a resync after it), so
    /// `ERR no such session` is a request not served from the first — as it
    /// is once this request was.
    fn call(&mut self, args: &[&str], stop: &AtomicBool, outage: bool) -> Result<Got, String> {
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
        full.push(self.inbox.as_str());
        full.extend_from_slice(args);
        let mut since: Option<Instant> = None;
        let mut pause = self.pause;
        loop {
            let r = self.ctl.call(&full)?;
            if !r.unserved(outage || since.is_some()) {
                return Ok(Got::Reply(r, since.is_some()));
            }
            let started = *since.get_or_insert_with(Instant::now);
            if started.elapsed() >= self.reconnect {
                return Err(format!(
                    "reconnect window lapsed: {} failed: {}",
                    args.join(" "),
                    r.stderr.trim()
                ));
            }
            std::thread::sleep(pause);
            pause = pause.saturating_mul(2).min(self.pause_max);
            if stop.load(Ordering::Relaxed) {
                return Ok(Got::Stopped);
            }
        }
    }

    /// The newest row in the inbox now: `inbox 1 --peek --meta`.
    fn baseline(&mut self, stop: &AtomicBool) -> Result<Option<Mark>, String> {
        match self.call(&["inbox", "1", "--peek", "--meta"], stop, false)? {
            Got::Stopped => Ok(None),
            Got::Reply(r, _) if r.ok() => Ok(Some(Mark::newest(&r.stdout))),
            Got::Reply(r, _) => Err(format!("inbox: {}", r.err_text())),
        }
    }

    /// Print one listed row and, for the watched worker's `report`, hand it
    /// to the loop with its body's row count. `true` when the loop ended
    /// under it. `outage`: as [`Self::call`]'s.
    fn hand(
        &mut self,
        row: &MailRow,
        worker: Option<&str>,
        say: &(dyn Fn(&str) -> Result<(), String> + Sync),
        tx: &Sender<Delivery>,
        stop: &AtomicBool,
        outage: bool,
    ) -> Result<bool, String> {
        let report = worker.is_some_and(|w| row.is_report_from(w));
        let body_rows = if report {
            match self.call(&["inbox", "get", &row.id.to_string()], stop, outage)? {
                Got::Stopped => return Ok(true),
                Got::Reply(r, _) if r.ok() => body_rows(&r.stdout),
                Got::Reply(..) => 0,
            }
        } else {
            0
        };
        say(&row.line())?;
        if report {
            let d = Delivery {
                at: Instant::now(),
                id: row.id,
                off: row.off,
                rows: body_rows,
            };
            if tx.send(d).is_err() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// After an outage: list the inbox whole and take up from what the bus
    /// offset says is new. The instance answering may be the successor of the
    /// one the lane was parked on (a self-update hands every session over; the
    /// successor's inbox is refilled by the bridge on attach, row ids from 1),
    /// so an old `since=` could park until the successor counts that high. The
    /// broker's offset is monotone across instances: a row above the last seen
    /// offset is new, printed and handed over; the next `since=` is the
    /// successor's newest id. On the same instance this is the ordinary listing.
    /// `true` when the loop ended under it.
    fn resync(
        &mut self,
        mark: &mut Mark,
        worker: Option<&str>,
        say: &(dyn Fn(&str) -> Result<(), String> + Sync),
        tx: &Sender<Delivery>,
        stop: &AtomicBool,
    ) -> Result<bool, String> {
        // A handoff is under way (an outage ridden out, or a successor's `ERR
        // bad since`): `ERR no such session` is an instance that does not
        // host the session yet, one more request not served
        // ([`CtlReply::unserved`]). A listing that took a retry is as whole
        // as one that did not, whichever instance answers it now: the lane
        // re-baselines on it, never throws it away to await again from the
        // old `since=`.
        let listed = match self.call(&["inbox", "--peek", "--meta"], stop, true)? {
            Got::Stopped => return Ok(true),
            Got::Reply(r, _) if r.ok() => r.stdout,
            Got::Reply(r, _) => return Err(format!("inbox: {}", r.err_text())),
        };
        let all = parse_rows(&listed);
        let newest_id = all.iter().map(|r| r.id).max().unwrap_or(0);
        let mut rows: Vec<MailRow> = all.into_iter().filter(|r| r.off > mark.off).collect();
        rows.sort_by_key(|row| row.off);
        for row in &rows {
            if self.hand(row, worker, say, tx, stop, true)? {
                return Ok(true);
            }
            mark.off = mark.off.max(row.off);
        }
        mark.id = newest_id;
        Ok(false)
    }

    /// Park on the inbox and print what lands, until `stop` or a failure
    /// (said as `MAIL off: …`, once). `worker` is the watched session's
    /// sid: its `report`s go to `tx` with their bodies' row counts, for the
    /// loop to fold. Every line goes through `say`, the loop's one sink.
    pub fn run(
        mut self,
        worker: Option<&str>,
        say: &(dyn Fn(&str) -> Result<(), String> + Sync),
        tx: Sender<Delivery>,
        stop: &AtomicBool,
    ) {
        if let Err(why) = self.serve(worker, say, &tx, stop) {
            let _ = say(&lane_off(&why));
        }
    }

    fn serve(
        &mut self,
        worker: Option<&str>,
        say: &(dyn Fn(&str) -> Result<(), String> + Sync),
        tx: &Sender<Delivery>,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        let Some(mut mark) = self.baseline(stop)? else {
            return Ok(());
        };
        let step_ms = self.step.as_millis().to_string();
        // `kinds=` names every kind; a host that refuses the list is asked
        // without it from then on (its default skips notes).
        let mut kinds = true;
        // An outage ridden out on the last request: re-baseline first.
        let mut resync = false;
        while !stop.load(Ordering::Relaxed) {
            if std::mem::take(&mut resync) {
                if self.resync(&mut mark, worker, say, tx, stop)? {
                    return Ok(());
                }
                continue;
            }
            let since_arg = format!("since={}", mark.id);
            let kinds_arg = format!("kinds={MAIL_KINDS}");
            let mut args = vec!["await", "inbox", since_arg.as_str()];
            if kinds {
                args.push(kinds_arg.as_str());
            }
            args.extend(["timeout", step_ms.as_str()]);
            let r = match self.call(&args, stop, false)? {
                Got::Stopped => return Ok(()),
                Got::Reply(_, true) => {
                    resync = true;
                    continue;
                }
                Got::Reply(r, false) => r,
            };
            // The loop ended while the wait was parked (cut short by the loop's
            // interrupter, or run out); the client's answer is not the lane's to
            // judge.
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            if r.timed_out() {
                continue;
            }
            if !r.ok() {
                if kinds && r.usage_error() {
                    kinds = false;
                    continue;
                }
                // An anchor the instance never minted (`ERR bad since`): the
                // one answering is a successor that counts its ids anew (a
                // producer before the id carry). Take up from the offset, as
                // after an outage.
                if r.err_text().starts_with("ERR bad since") {
                    resync = true;
                    continue;
                }
                return Err(format!("await inbox: {}", r.err_text()));
            }
            let latched = field(r.stdout.trim(), "hold").is_none();
            if !latched {
                continue;
            }
            let listed = match self.call(&["inbox", &since_arg, "--peek", "--meta"], stop, false)? {
                Got::Stopped => return Ok(()),
                Got::Reply(_, true) => {
                    resync = true;
                    continue;
                }
                Got::Reply(r, false) if r.ok() => r.stdout,
                Got::Reply(r, false) => return Err(format!("inbox: {}", r.err_text())),
            };
            let mut rows = parse_rows(&listed);
            rows.retain(|row| row.id > mark.id);
            rows.sort_by_key(|row| row.id);
            for row in &rows {
                if self.hand(row, worker, say, tx, stop, false)? {
                    return Ok(());
                }
                mark.advance(row);
            }
            // The latch's own id, should the listing have cut it.
            if let Some(id) = r
                .stdout
                .trim()
                .strip_prefix("OK inbox ")
                .and_then(|s| s.parse::<u64>().ok())
            {
                mark.id = mark.id.max(id);
            }
        }
        Ok(())
    }
}

/// `aterm drive task`'s knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOpts {
    /// The worker, `@sid`.
    pub worker: String,
    /// The manager's session selector (`@self`, or `--inbox`'s).
    pub inbox: String,
    /// The task, one line or many.
    pub text: String,
    /// `--deadline`: the advisory `dl=` the post carries.
    pub deadline: Option<Duration>,
    /// `--wait`: how long to park for the answer.
    pub wait: Option<Duration>,
    /// `--reconnect-s`: how long the wait rides out an outage — requests not
    /// served, as across an aterm self-update — before it ends; zero: the
    /// first one ends it.
    pub reconnect: Duration,
    /// Stdout is a terminal: the answer's body is printed escaped
    /// ([`terminal_safe`]), so a reply cannot move the cursor over, or reorder,
    /// the `MAIL … from=` line that says who sent it. Raw bytes otherwise —
    /// what a pipe or a file is given.
    pub escape: bool,
}

/// Post the task, nudge an idle worker, and with `--wait` print the answer.
/// Prints `task @<off> nudged=<0|1>` as soon as the post landed (and, with
/// `--wait`, the answer's `MAIL` line and its body); returns the exit code:
/// `0`, or [`EXIT_TIMEOUT`] when `--wait` ran out (its last line then says
/// `TIMEOUT …`). The wait is the BOUND's, not one request's: the host clamps
/// an `await` at 600 s, so its `OK timeout` with time left is re-armed from
/// the same `since=`, and only the bound spent is the TIMEOUT. An outage in
/// the wait — an aterm self-update ends the parked `await inbox since=<id>`
/// with `aterm-ctl`'s exit 75, its id being the old process's — is ridden
/// out within the bound and `--reconnect-s` ([`Outage`]). `Err` when the
/// post did not land — the server's own words, `queued=1` or `no-bridge=1`
/// among them, are the instruction — or was cut before its answer (it may
/// have landed),
/// when the worker could not be read, when the nudge's turn was refused, or
/// when an outage outlasted `--reconnect-s`.
pub fn task<C: Ctl>(ctl: &mut C, opts: &TaskOpts, out: &mut dyn Write) -> Result<u8, String> {
    task_with_clock(ctl, opts, out, Instant::now, std::thread::sleep)
}

/// An outage `task --wait` rides out: a request not served
/// ([`CtlReply::lost`]) at `since`, as an aterm self-update's handoff leaves
/// the wait parked on the old instance. While it lasts, `ERR no such session`
/// is one more request not served ([`CtlReply::unserved`]): the successor may
/// not host the manager's session yet. After a pause that doubles up to
/// [`RECONNECT_PAUSE_MAX`], the inbox is listed WHOLE: the instance that
/// answers may be the successor, whose row ids start over ([`Lane::resync`]),
/// so the answer is told by its `re=` (the post's bus offset, which a handoff
/// keeps) and the next `since=` is the newest id listed. `ERR bad since` is
/// that successor too (a build before the id carry). It is over when a wait
/// is served again; a wait dropped again after the listing is the same
/// outage, and the same `--reconnect-s` window.
struct Outage {
    since: Instant,
    pause: Duration,
    /// The inbox was listed whole since the last request not served.
    listed: bool,
}

impl Outage {
    /// A request `what` was not served at `at`: the outage begins, or goes
    /// on and lists the inbox again. `Err` once it has lasted `window` — at
    /// once with a zero window, in the request's own words.
    fn ride(
        outage: &mut Option<Self>,
        at: Instant,
        window: Duration,
        what: &str,
        r: &CtlReply,
    ) -> Result<(), String> {
        let o = outage.get_or_insert(Outage {
            since: at,
            pause: RECONNECT_PAUSE,
            listed: false,
        });
        o.listed = false;
        if at.saturating_duration_since(o.since) < window {
            return Ok(());
        }
        Err(if window.is_zero() {
            format!("{what}: {}", r.err_text())
        } else {
            format!("reconnect window lapsed: {what}: {}", r.err_text())
        })
    }

    /// Whether `r` is a request the wait rides out: one not served
    /// ([`CtlReply::unserved`], `ERR no such session` among them while
    /// `in_outage`), or `ERR bad since`.
    fn begins(r: &CtlReply, in_outage: bool) -> bool {
        r.unserved(in_outage) || r.err_text().starts_with("ERR bad since")
    }
}

/// The shipping deadline loop with its monotonic clock and its pause
/// supplied separately so scripted host timeouts and outages can exercise it
/// without depending on OS scheduling.
fn task_with_clock<C: Ctl>(
    ctl: &mut C,
    opts: &TaskOpts,
    out: &mut dyn Write,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> Result<u8, String> {
    let since = match opts.wait {
        Some(_) => {
            let r = ctl.call(&[&opts.inbox, "inbox", "1", "--peek", "--meta"])?;
            if !r.ok() {
                return Err(format!(
                    "cannot read the manager's inbox ({}): {}",
                    opts.inbox,
                    r.err_text()
                ));
            }
            newest_id(&r.stdout)
        }
        None => 0,
    };
    let to = format!("to={}", opts.worker);
    let dl = opts.deadline.map(|d| format!("dl={}", d.as_millis()));
    let mut args = vec![opts.inbox.as_str(), "post", to.as_str(), "kind=task"];
    if let Some(dl) = &dl {
        args.push(dl.as_str());
    }
    // `--` ENDS THE OPTIONS: the text is whatever the manager was handed (a
    // peer's mail, an issue), and one opening `to=@s-x kind=answer re=<n>`
    // would otherwise be read as the post's own envelope — another recipient,
    // kind and correlation id, under this session's name.
    args.push("--");
    args.push(opts.text.as_str());
    let r = ctl.call(&args)?;
    // A post cut before its answer may have landed (an update carries a
    // queued post to its successor, which sends it): never "not posted", and
    // never posted again here. One turned away unread or never sent
    // ([`CtlReply::untouched`]: busy, a stale token, a refused socket) did not
    // land, and says so.
    if r.lost() && !r.untouched() {
        return Err(format!(
            "task post got no answer and may have landed — look before posting it again \
             (`outbox`, `timeline`): {}",
            r.err_text()
        ));
    }
    if !r.ok() {
        return Err(format!("task not posted: {}", r.err_text()));
    }
    let off = num(r.stdout.trim(), "off").ok_or_else(|| {
        format!(
            "task not landed: the post answered `{}` with no offset",
            r.stdout.trim()
        )
    })?;
    // The post landed: the offset is printed whatever the nudge comes to (the
    // manager cannot recover it otherwise). Nothing wakes a worker for mail: it
    // is typed to, as a person would, with the one-line nudge over an idle
    // screen only.
    let mut nudged = false;
    let mut refused = None;
    match Session::new(ctl, Some(opts.worker.clone())).read_screen() {
        Err(why) => refused = Some(format!("cannot read the worker for the nudge: {why}")),
        Ok(screen) if worker_phase(&screen.rows) == Phase::Idle => {
            // The Enter is guarded on the composer's caret row holding the
            // nudge: a box the read missed, or that came up since, has no such
            // row, so the Enter can never choose its focused option.
            let nudge = format!("{NUDGE}{off}");
            // Claude Code's caret: the nudge is typed only over the idle
            // screen `worker_phase` (Claude Code's grammar) reads.
            let submit = format!("submit=guarded:{}", composer_guard('❯', &nudge));
            let r = ctl.call(&[
                &opts.worker,
                "turn",
                &submit,
                NUDGE_IDLE,
                NUDGE_TIMEOUT,
                &nudge,
            ])?;
            nudged = r.submitted();
            if !nudged && !r.timed_out() {
                refused = Some(if r.unknown_form() {
                    format!(
                        "nudge not typed: the worker's aterm is too old for a guarded nudge: {}",
                        r.err_text()
                    )
                } else if r.ok() {
                    format!(
                        "nudge not submitted; `{nudge}` may be left typed: {}",
                        r.turn_verdict().unwrap_or(r.stdout.trim())
                    )
                } else {
                    format!("nudge refused: {}", r.err_text())
                });
            }
        }
        Ok(_) => {}
    }
    emit(out, &format!("task @{off} nudged={}", u8::from(nudged)))?;
    if let Some(why) = refused {
        return Err(why);
    }
    let Some(bound) = opts.wait else {
        return Ok(0);
    };
    let deadline = now() + bound;
    let mut since = since;
    let mut outage: Option<Outage> = None;
    loop {
        // The wire's unit is the millisecond: a remainder under one is the
        // bound spent, not a `timeout 0` to arm and re-arm until it passes.
        let left = deadline.saturating_duration_since(now());
        let ms = left.as_millis();
        if ms == 0 {
            break;
        }
        let rows = match outage.as_mut() {
            Some(o) if !o.listed => {
                sleep(o.pause.min(left));
                o.pause = o.pause.saturating_mul(2).min(RECONNECT_PAUSE_MAX);
                let r = ctl.call(&[&opts.inbox, "inbox", "--peek", "--meta"])?;
                if r.unserved(true) {
                    Outage::ride(&mut outage, now(), opts.reconnect, "inbox", &r)?;
                    continue;
                }
                if !r.ok() {
                    return Err(format!("inbox: {}", r.err_text()));
                }
                o.listed = true;
                since = newest_id(&r.stdout);
                parse_rows(&r.stdout)
            }
            _ => {
                let since_arg = format!("since={since}");
                let kinds_arg = format!("kinds={ANSWER_KINDS}");
                let ms = ms.to_string();
                let r = ctl.call(&[
                    &opts.inbox,
                    "await",
                    "inbox",
                    &since_arg,
                    &kinds_arg,
                    "timeout",
                    &ms,
                ])?;
                if Outage::begins(&r, outage.is_some()) {
                    Outage::ride(&mut outage, now(), opts.reconnect, "await inbox", &r)?;
                    continue;
                }
                // Served: whatever the outage was, it is over.
                outage = None;
                // The host's own step running out (600 s at most a wait), not
                // the bound's: re-arm while time is left.
                if r.timed_out() {
                    continue;
                }
                if !r.ok() {
                    return Err(format!("await inbox: {}", r.err_text()));
                }
                let r = ctl.call(&[&opts.inbox, "inbox", &since_arg, "--peek", "--meta"])?;
                if Outage::begins(&r, outage.is_some()) {
                    Outage::ride(&mut outage, now(), opts.reconnect, "inbox", &r)?;
                    continue;
                }
                if !r.ok() {
                    return Err(format!("inbox: {}", r.err_text()));
                }
                let mut rows = parse_rows(&r.stdout);
                rows.retain(|row| row.id > since);
                for row in &rows {
                    since = since.max(row.id);
                }
                rows
            }
        };
        // FROM THE WORKER, whole principal: a reply naming the task's offset
        // from anyone else answers nothing (the offset is the broker's, and a
        // peer can guess it), so it is listed like any mail and never printed
        // as the task's result.
        let answer = rows
            .iter()
            .filter(|row| {
                row.re == Some(off)
                    && ANSWER_KINDS.split(',').any(|k| k == row.kind)
                    && is_from_session(&row.from, &opts.worker)
            })
            .min_by_key(|row| row.id);
        if let Some(row) = answer {
            let r = ctl.call(&[&opts.inbox, "inbox", "get", &row.id.to_string()])?;
            if r.unserved(outage.is_some()) {
                Outage::ride(&mut outage, now(), opts.reconnect, "inbox get", &r)?;
                continue;
            }
            emit(out, &row.line())?;
            if !r.ok() {
                return Err(format!("inbox get {}: {}", row.id, r.err_text()));
            }
            let body = if opts.escape {
                std::borrow::Cow::Owned(terminal_safe(&r.stdout))
            } else {
                std::borrow::Cow::Borrowed(r.stdout.as_str())
            };
            out.write_all(body.as_bytes())
                .and_then(|()| {
                    if body.ends_with('\n') || body.is_empty() {
                        Ok(())
                    } else {
                        out.write_all(b"\n")
                    }
                })
                .and_then(|()| out.flush())
                .map_err(|e| format!("cannot write a line to stdout: {e}"))?;
            return Ok(0);
        }
    }
    emit(
        out,
        &format!(
            "TIMEOUT no answer, report or ack re={off} within {} s",
            secs(bound)
        ),
    )?;
    Ok(EXIT_TIMEOUT)
}

/// A bound in seconds: whole when it is (`30`), else to the millisecond
/// (`0.06`).
pub(super) fn secs(d: Duration) -> String {
    if d.subsec_millis() == 0 {
        d.as_secs().to_string()
    } else {
        format!("{:.3}", d.as_secs_f64())
            .trim_end_matches('0')
            .to_string()
    }
}

/// A mail body for a terminal: its lines kept, and in them every C0 control
/// but the tab, DEL, every C1 control and every bidi override escaped —
/// `\r` by name, the rest as `\xNN` / `\u{…}`, the classes
/// `aterm_link::render::safe` escapes. A body is the sender's bytes, and on a
/// terminal those are instructions: a cursor-up and an erase would wipe the
/// `MAIL … from=` line above it and draw a forged one, and a bidi override
/// would reorder what is left.
fn terminal_safe(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for c in body.chars() {
        match c {
            '\n' | '\t' => out.push(c),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if ('\u{80}'..='\u{9f}').contains(&c)
                || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
            {
                out.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Write one line and flush it.
fn emit(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|e| format!("cannot write a line to stdout: {e}"))
}

#[cfg(test)]
mod tests {

    /// The stamp line is not a row: a stamped report body's first line
    /// (`seq=<n> hash=<hex16>[ busy=1]`) is skipped, so `rows=<n>` is the
    /// body's row count. Only a real stamp is skipped; prose that merely
    /// starts with `seq=` is a row.
    #[test]
    fn body_rows_does_not_count_the_report_stamp() {
        assert_eq!(
            body_rows("one\ntwo\n"),
            2,
            "a body with no stamp is unchanged"
        );
        assert_eq!(
            body_rows("seq=12 hash=0123456789abcdef\n⏺ done\n  next\n"),
            2,
            "the stamp is not a row"
        );
        assert_eq!(
            body_rows("seq=12 hash=0123456789abcdef busy=1\n⏺ mid-turn\n"),
            1,
            "busy=1 rides the stamp"
        );
        // Not a stamp: prose that only looks like one.
        assert_eq!(body_rows("seq=12 hash=nothex\nrow\n"), 2);
        assert_eq!(body_rows("seq=x hash=0123456789abcdef\nrow\n"), 2);
        assert_eq!(body_rows("seq=12 hash=0123456789abcdef and more\nrow\n"), 2);
        assert_eq!(body_rows("seq=12\nrow\n"), 2);
    }
    use super::super::prompt::fixtures::{composer, rows};
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// How long a wait on a real thread for something that MUST happen may take
    /// before it is read as a hang (AGENTS.md: a hang detector is a minute,
    /// never a latency budget).
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// A scripted client: every request pops the next reply and is recorded;
    /// an unexpected request fails instead of spinning on an exhausted script.
    /// An `await` answered with a timeout advances the
    /// shared monotonic clock by the timeout asked or [`Script::PARK`], whichever
    /// is shorter — the host's bounded wait, independent of test scheduling.
    struct Script {
        requests: Vec<String>,
        replies: VecDeque<CtlReply>,
        now: Rc<Cell<Instant>>,
    }

    impl Script {
        const PARK: Duration = Duration::from_millis(25);

        fn new(replies: Vec<CtlReply>) -> Self {
            Self {
                requests: Vec::new(),
                replies: replies.into(),
                now: Rc::new(Cell::new(Instant::now())),
            }
        }
    }

    impl Ctl for Script {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let line = args.join(" ");
            self.requests.push(line.clone());
            let reply = self
                .replies
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected control request: {line}"));
            if reply.timed_out() && line.contains(" await ") {
                let asked = num(&line, "timeout")
                    .or_else(|| line.rsplit(' ').next().and_then(|ms| ms.parse().ok()))
                    .map_or(Self::PARK, Duration::from_millis);
                self.now.set(self.now.get() + asked.min(Self::PARK));
            }
            Ok(reply)
        }
    }

    fn ok(stdout: &str) -> CtlReply {
        CtlReply {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }
    fn timeout() -> CtlReply {
        CtlReply {
            code: 124,
            stdout: "OK timeout\n".to_string(),
            stderr: String::new(),
        }
    }
    fn err(text: &str) -> CtlReply {
        CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: format!("aterm-ctl: ERR {text}\n"),
        }
    }
    /// `aterm-ctl`'s exit 75: its server went away under the request.
    fn replaced(note: &str) -> CtlReply {
        CtlReply {
            code: i32::from(aterm_ctl::EXIT_REPLACED),
            stdout: String::new(),
            stderr: format!("aterm-ctl: {note}\n"),
        }
    }
    /// `aterm-ctl`'s note for the wait an update cut (`since=4` is the old
    /// process's row id).
    const CUT_SINCE: &str = "aterm was replaced (pid 10 -> 11) before it answered; \
                             `since=4` names a position in the old process — re-issue it \
                             (exit 75)";
    /// `aterm-ctl`'s note for a request nothing answered in its server's place.
    const CUT_GONE: &str = "aterm (pid 10) went away and nothing replaced it within 30.0 s; the \
                            exchange cannot be resumed (exit 75)";
    /// A connection nothing takes yet: the successor has not bound.
    fn refused() -> CtlReply {
        CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: "aterm-ctl: connect /d/aterm.sock: Connection refused (os error 61)\n"
                .to_string(),
        }
    }
    /// A `text --json` reply of these rows.
    fn screen(rows: &[String]) -> CtlReply {
        let rows: Vec<String> = rows
            .iter()
            .map(|r| format!("\"{}\"", r.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        ok(&format!(
            "{{\"rows\":[{}],\"cursor\":{{\"row\":10,\"col\":2,\"visible\":true,\"style\":\"block\"}},\"dims\":{{\"rows\":40,\"cols\":120}},\"seq\":7}}\n",
            rows.join(",")
        ))
    }
    fn idle_screen() -> Vec<String> {
        let mut r = rows(&["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    }
    fn busy_screen() -> Vec<String> {
        let mut r = rows(&["⏺ Working.", "", "✻ Synthesizing… (18s)", ""]);
        r.extend(composer("  esc to interrupt"));
        r
    }
    const INBOX_1: &str = "OK 1 hold=0 holder=- seen=3 bus_head=90 dropped=0 pending=0\n\
                           msg 4 off=80 t=1000 from=h-owner kind=note trust=human len=3\n";
    fn task_opts(wait: Option<Duration>) -> TaskOpts {
        TaskOpts {
            worker: "@s-1".to_string(),
            inbox: "@self".to_string(),
            text: "run the suite and report".to_string(),
            deadline: Some(Duration::from_secs(600)),
            wait,
            reconnect: super::super::run::DEFAULT_RECONNECT,
            escape: false,
        }
    }
    /// [`task_with_clock`] on the script's clock: a pause moves it on.
    fn run_task(s: &mut Script, opts: &TaskOpts) -> (Result<u8, String>, Vec<String>) {
        let mut out: Vec<u8> = Vec::new();
        let now = Rc::clone(&s.now);
        let paused = Rc::clone(&s.now);
        let code = task_with_clock(
            s,
            opts,
            &mut out,
            || now.get(),
            |d| paused.set(paused.get() + d),
        );
        let text = String::from_utf8(out).expect("utf-8");
        (code, text.lines().map(str::to_string).collect())
    }

    /// The row parser reads every field the endpoint prints, and the MAIL
    /// line is the spec's: `id= off= from= kind= len=` and `re=` only when
    /// the row answers something.
    #[test]
    fn a_row_parses_and_prints_as_the_mail_line() {
        let row = MailRow::parse(
            "msg 5 off=91 t=2000 from=s-1@n-1 kind=report trust=agent re=88 re-id=2 dl=5 len=2048 more=1",
        )
        .expect("a msg row");
        assert_eq!(
            row,
            MailRow {
                id: 5,
                off: 91,
                t: 2000,
                from: "s-1@n-1".to_string(),
                kind: "report".to_string(),
                trust: "agent".to_string(),
                re: Some(88),
                len: 2048,
            }
        );
        assert_eq!(
            row.line(),
            "MAIL id=5 off=91 from=s-1@n-1 kind=report len=2048 re=88"
        );
        assert!(row.is_report_from("@s-1") && row.is_report_from("s-1"));
        assert!(!row.is_report_from("@s-2"));
        assert!(row.is_report_from("@s-1@n-1"));
        assert!(
            !row.is_report_from("@s-1@n-2"),
            "the node half is the attested half: another node's `s-1` is not the worker"
        );
        let plain = MailRow::parse("msg 6 off=92 t=1 from=h-owner kind=ask trust=human len=3")
            .expect("a msg row");
        assert_eq!(plain.line(), "MAIL id=6 off=92 from=h-owner kind=ask len=3");
        assert!(!plain.is_report_from("s-1"), "an ask is not a report");
        assert_eq!(MailRow::parse("post 3 to=@s-1 kind=task off=- len=4"), None);
        assert_eq!(MailRow::parse("OK 2 hold=0"), None);
        assert_eq!(newest_id(INBOX_1), 4);
        assert_eq!(newest_id("OK 0 hold=0 holder=- seen=0\n"), 0);
        assert_eq!(body_rows("a\nb\nc\n"), 3);
        assert_eq!(body_rows("a\nb"), 2);
        assert_eq!(body_rows(""), 0);
    }

    /// `task`: the post carries the deadline and the text by mail; an idle
    /// worker is nudged with one turn naming the offset; the line says so.
    #[test]
    fn task_posts_by_mail_and_nudges_an_idle_worker() {
        let mut s = Script::new(vec![
            ok("OK 3 off=91\n"),
            screen(&idle_screen()),
            ok("turn 5 submitted=1 status=timeout seq=9 dur_ms=2500 hash=0000000000000000\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(None));
        assert_eq!(code, Ok(0));
        assert_eq!(lines, ["task @91 nudged=1"]);
        let guard = composer_guard('❯', "Inbox: task @91");
        assert!(guard.starts_with("^❯\\s"), "{guard}");
        assert_eq!(
            s.requests,
            [
                "@self post to=@s-1 kind=task dl=600000 -- run the suite and report".to_string(),
                "@s-1 text --json tail=40".to_string(),
                format!("@s-1 turn submit=guarded:{guard} idle=600 timeout=2500 Inbox: task @91"),
            ]
        );
    }

    /// A live box whose body holds a `⎿` row over a blank row (a heredoc
    /// writing a transcript, an option's description) is read as a box, so no
    /// nudge is typed, whole or through the 40-row tail; and where a nudge is
    /// typed, a guard that misses (a box came up between read and Enter) is
    /// `nudged=0` and said, not an Enter. Control: the idle screen is nudged.
    #[test]
    fn a_nudge_never_lands_in_a_live_box() {
        let rule = "─".repeat(120);
        let mut heredoc = rows(&[
            "⏺ Writing the fixture.",
            "",
            &rule,
            " Bash command",
            "",
            "   cat > fixture.txt <<'EOF'",
        ]);
        for k in 0..30 {
            heredoc.push(format!("   line {k}"));
        }
        heredoc.extend(rows(&[
            "   ⏺ Bash(ls)",
            "     ⎿  a.txt",
            "",
            "   ⏺ Done.",
            "   EOF",
            "   Write the transcript fixture",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. No",
            "",
            " Esc to cancel · Tab to amend",
        ]));
        let tail = heredoc[heredoc.len() - 40..].to_vec();
        for (name, r) in [("whole", heredoc), ("tail 40", tail)] {
            assert_ne!(worker_phase(&r), Phase::Idle, "{name}");
            let mut s = Script::new(vec![ok("OK 3 off=91\n"), screen(&r)]);
            let (code, lines) = run_task(&mut s, &task_opts(None));
            assert_eq!(code, Ok(0), "{name}");
            assert_eq!(lines, ["task @91 nudged=0"], "{name}");
            assert!(
                !s.requests.iter().any(|q| q.contains("turn")),
                "{name}: {:?}",
                s.requests
            );
        }
        // The guard missed: nothing submitted, said, and no second Enter.
        let mut s = Script::new(vec![
            ok("OK 3 off=91\n"),
            screen(&idle_screen()),
            ok("OK 0 turn skipped reason=guard submitted=0 seq=9 id=1\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(None));
        assert_eq!(lines, ["task @91 nudged=0"]);
        let why = code.expect_err("a missed guard is said");
        assert!(
            why.contains("not submitted") && why.contains("reason=guard"),
            "{why}"
        );
        assert_eq!(s.requests.len(), 3, "{:?}", s.requests);
    }

    /// A busy worker gets the mail only: no nudge is typed into a running
    /// turn, and the line says `nudged=0`.
    #[test]
    fn a_busy_worker_is_not_nudged() {
        let mut s = Script::new(vec![ok("OK 3 off=91\n"), screen(&busy_screen())]);
        let (code, lines) = run_task(&mut s, &task_opts(None));
        assert_eq!(code, Ok(0));
        assert_eq!(lines, ["task @91 nudged=0"]);
        assert_eq!(s.requests.len(), 2, "{:?}", s.requests);
        assert!(!s.requests.iter().any(|r| r.contains("turn")));
    }

    /// No deadline: no `dl=` on the post (a busy worker, so no nudge either).
    #[test]
    fn a_task_without_a_deadline_carries_no_dl() {
        let mut s = Script::new(vec![ok("OK 3 off=91\n"), screen(&busy_screen())]);
        let opts = TaskOpts {
            deadline: None,
            ..task_opts(None)
        };
        assert_eq!(run_task(&mut s, &opts).0, Ok(0));
        assert_eq!(
            s.requests[0],
            "@self post to=@s-1 kind=task -- run the suite and report"
        );
    }

    /// **A REPLY COUNTS ONLY FROM THE WORKER, WHOLE.** The task's offset is the
    /// broker's and a peer can guess it: an `answer re=<off>` from another node
    /// (rendered `n-evil` — the bridge attributes a remote claim to a session
    /// hosted here to the node itself), or from another session, is not the
    /// task's result — the wait goes on to the worker's own. And on a terminal
    /// the body is escaped: it cannot erase the `MAIL … from=` line above it and
    /// draw another.
    #[test]
    fn only_the_workers_own_reply_answers_the_task_and_a_terminal_sees_it_escaped() {
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            ok("OK inbox 6\n"),
            ok(
                "OK 2 hold=0 holder=- seen=3 bus_head=95 dropped=0 pending=0\n\
                msg 5 off=93 t=3 from=n-evil kind=answer trust=agent re=91 len=9\n\
                msg 6 off=94 t=3 from=s-9@n-1 kind=report trust=agent re=91 len=9\n",
            ),
            ok("OK inbox 7\n"),
            ok(
                "OK 1 hold=0 holder=- seen=3 bus_head=96 dropped=0 pending=0\n\
                msg 7 off=95 t=4 from=s-1@n-1 kind=answer trust=agent re=91 len=5\n",
            ),
            ok("\u{1b}[1A\u{1b}[2Kdone\u{202e}\r\nok\tfine\n"),
        ]);
        let opts = TaskOpts {
            escape: true,
            ..task_opts(Some(Duration::from_secs(30)))
        };
        let (code, lines) = run_task(&mut s, &opts);
        assert_eq!(code, Ok(0));
        assert_eq!(
            lines,
            [
                "task @91 nudged=0",
                "MAIL id=7 off=95 from=s-1@n-1 kind=answer len=5 re=91",
                "\\x1b[1A\\x1b[2Kdone\\u{202e}\\r",
                "ok\tfine",
            ]
        );
        assert_eq!(
            s.requests.last().map(String::as_str),
            Some("@self inbox get 7")
        );
        // Whole principals: the node half is what the broker attests.
        assert!(is_from_session("s-1@n-1", "@s-1"));
        assert!(is_from_session("s-1@n-1", "@s-1@n-1"));
        assert!(!is_from_session("s-1@n-evil", "@s-1@n-1"));
        assert!(!is_from_session("s-2@n-1", "@s-1"));
        assert!(
            !is_from_session("s-1", "@s-1"),
            "no node: not a session's row"
        );
        assert!(!is_from_session("s-1@h-x", "@s-1"));
        assert!(!is_from_session("h-owner", "@s-1"));
    }

    /// `--wait`: the newest inbox id is read before the post (an answer that
    /// lands between the post and the wait is above it), then `await inbox`
    /// parks for `answer|report|ack`; a row of the right kind that does not
    /// answer this offset re-arms the wait from its id; the one that does is
    /// printed as its MAIL line and its body.
    #[test]
    fn wait_parks_for_the_answer_that_names_the_offset() {
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            ok("OK inbox 5\n"),
            ok(
                "OK 1 hold=0 holder=- seen=3 bus_head=95 dropped=0 pending=0\n\
                msg 5 off=93 t=3 from=s-1@n-1 kind=answer trust=agent re=70 len=2\n",
            ),
            ok("OK inbox 6\n"),
            ok(
                "OK 1 hold=0 holder=- seen=3 bus_head=96 dropped=0 pending=0\n\
                msg 6 off=95 t=4 from=s-1@n-1 kind=answer trust=agent re=91 len=5\n",
            ),
            ok("done\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(Some(Duration::from_secs(30))));
        assert_eq!(code, Ok(0));
        assert_eq!(
            lines,
            [
                "task @91 nudged=0",
                "MAIL id=6 off=95 from=s-1@n-1 kind=answer len=5 re=91",
                "done",
            ]
        );
        let reqs: Vec<String> = s
            .requests
            .iter()
            .map(|r| {
                // The wait's timeout counts down: not asserted to the ms.
                match r.find(" timeout ") {
                    Some(i) => format!("{} timeout <ms>", &r[..i]),
                    None => r.clone(),
                }
            })
            .collect();
        assert_eq!(
            reqs,
            [
                "@self inbox 1 --peek --meta",
                "@self post to=@s-1 kind=task dl=600000 -- run the suite and report",
                "@s-1 text --json tail=40",
                "@self await inbox since=4 kinds=answer,report,ack timeout <ms>",
                "@self inbox since=4 --peek --meta",
                "@self await inbox since=5 kinds=answer,report,ack timeout <ms>",
                "@self inbox since=5 --peek --meta",
                "@self inbox get 6",
            ]
        );
    }

    /// A wait that runs out is the TIMEOUT: exit 124, the task line already
    /// printed, the last line saying what never came. The bound is spent by
    /// waiting: every `OK timeout` short of it re-arms the same `since=`.
    #[test]
    fn a_wait_that_runs_out_is_the_timeout() {
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            timeout(),
            timeout(),
            timeout(),
            timeout(),
        ]);
        let bound = Duration::from_millis(60);
        let started = s.now.get();
        let (code, lines) = run_task(&mut s, &task_opts(Some(bound)));
        // The bound is spent by host waits. A real 25 ms sleep may resume after
        // all 60 ms have passed on a loaded machine, legitimately needing just
        // one await; this clock instead makes all three waits observable.
        assert_eq!(
            s.now.get().duration_since(started),
            bound,
            "the timeout must consume the bound, not just the first host wait"
        );
        assert_eq!(code, Ok(EXIT_TIMEOUT));
        assert_eq!(
            lines,
            [
                "task @91 nudged=0",
                "TIMEOUT no answer, report or ack re=91 within 0.06 s",
            ]
        );
        let awaits: Vec<&str> = s
            .requests
            .iter()
            .filter(|r| r.contains(" await "))
            .map(String::as_str)
            .collect();
        assert_eq!(
            awaits,
            [
                "@self await inbox since=4 kinds=answer,report,ack timeout 60",
                "@self await inbox since=4 kinds=answer,report,ack timeout 35",
                "@self await inbox since=4 kinds=answer,report,ack timeout 10",
            ],
            "re-arm the same watermark with only the remaining bound"
        );
        assert_eq!(secs(Duration::from_secs(30)), "30");
        assert_eq!(secs(Duration::from_millis(1500)), "1.5");
    }

    /// The host clamps every `await` at 600 s (`control_session.rs`:
    /// `timeout_ms.min(600_000)`), so a `--wait` bound above it — `--deadline
    /// 900` — gets `OK timeout` with time left. That is the host's step
    /// running out, not the wait: it must re-arm.
    #[test]
    fn a_wait_re_arms_on_the_hosts_own_timeout_while_time_is_left() {
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            timeout(),
            ok("OK inbox 6\n"),
            ok(
                "OK 1 hold=0 holder=- seen=3 bus_head=96 dropped=0 pending=0\n\
                msg 6 off=95 t=4 from=s-1@n-1 kind=answer trust=agent re=91 len=5\n",
            ),
            ok("done\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(Some(Duration::from_secs(30))));
        assert_eq!(
            lines,
            [
                "task @91 nudged=0",
                "MAIL id=6 off=95 from=s-1@n-1 kind=answer len=5 re=91",
                "done",
            ],
            "requests: {:?}",
            s.requests
        );
        assert_eq!(code, Ok(0));
        let awaits: Vec<&String> = s
            .requests
            .iter()
            .filter(|r| r.contains("await inbox since=4 "))
            .collect();
        assert_eq!(
            awaits.len(),
            2,
            "the same since= re-armed: {:?}",
            s.requests
        );
    }

    /// ROUND EIGHT, FINDING 13, on `aterm drive task --wait`: its wait parks on
    /// `await inbox since=<id>` through `aterm-ctl`, which ends a wait an
    /// update's Commit cut with exit 75 — the id is the old process's, so it
    /// is not asked again. The wait rides it out: the inbox listed WHOLE on
    /// the instance that answers now (the successor, whose ids start over),
    /// the answer told by its `re=` — here it landed during the update, as
    /// the successor's id 2 — and its body read under that id. It exited 1
    /// with `await inbox: aterm was replaced …` instead.
    #[test]
    fn a_wait_the_update_cut_finds_the_answer_that_landed_meanwhile() {
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            replaced(CUT_SINCE),
            ok(
                "OK 2 hold=0 holder=- seen=0 bus_head=96 dropped=0 pending=0\n\
                msg 1 off=80 t=1000 from=h-owner kind=note trust=human len=3\n\
                msg 2 off=95 t=4 from=s-1@n-1 kind=answer trust=agent re=91 len=5\n",
            ),
            ok("done\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(Some(Duration::from_secs(30))));
        assert_eq!(
            lines,
            [
                "task @91 nudged=0",
                "MAIL id=2 off=95 from=s-1@n-1 kind=answer len=5 re=91",
                "done",
            ],
            "requests: {:?}",
            s.requests
        );
        assert_eq!(code, Ok(0));
        assert_eq!(
            s.requests[3..],
            [
                "@self await inbox since=4 kinds=answer,report,ack timeout 30000",
                "@self inbox --peek --meta",
                "@self inbox get 2",
            ]
        );
    }

    /// The same wait cut with nothing in the old server's place yet (exit 75,
    /// then a socket nothing takes): ridden out — 0.5 s, then 1 s — until the
    /// successor lists its inbox, which holds no answer yet; the wait is
    /// re-armed from the successor's newest id, with what the bound has left,
    /// and the answer that lands then is read.
    #[test]
    fn a_wait_the_update_cut_is_re_armed_on_the_successors_ids() {
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            replaced(CUT_GONE),
            refused(),
            ok(
                "OK 1 hold=0 holder=- seen=0 bus_head=90 dropped=0 pending=0\n\
                msg 1 off=80 t=1000 from=h-owner kind=note trust=human len=3\n",
            ),
            ok("OK inbox 2\n"),
            ok(
                "OK 1 hold=0 holder=- seen=1 bus_head=96 dropped=0 pending=0\n\
                msg 2 off=95 t=4 from=s-1@n-1 kind=answer trust=agent re=91 len=5\n",
            ),
            ok("done\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(Some(Duration::from_secs(30))));
        assert_eq!(
            lines,
            [
                "task @91 nudged=0",
                "MAIL id=2 off=95 from=s-1@n-1 kind=answer len=5 re=91",
                "done",
            ],
            "requests: {:?}",
            s.requests
        );
        assert_eq!(code, Ok(0));
        assert_eq!(
            s.requests[3..],
            [
                "@self await inbox since=4 kinds=answer,report,ack timeout 30000",
                "@self inbox --peek --meta",
                "@self inbox --peek --meta",
                "@self await inbox since=1 kinds=answer,report,ack timeout 28500",
                "@self inbox since=1 --peek --meta",
                "@self inbox get 2",
            ]
        );
    }

    /// Bounded: an outage that outlasts `--reconnect-s` (5 s here: pauses of
    /// 0.5, 1, 2 and 4 s) is the error, naming the last request not served —
    /// never a wait spun out to the bound. With `--reconnect-s 0` the first
    /// one is, in its own words, as before. A post cut before its answer is
    /// never "not posted": it may have landed, and is not posted again. One
    /// turned away unread or never sent ([`CtlReply::untouched`]) did not
    /// land, and is "not posted", as before: read as one that may have, a
    /// clean refusal was reported as an outcome nobody could know.
    #[test]
    fn an_outage_past_reconnect_s_is_the_error() {
        let opts = TaskOpts {
            reconnect: Duration::from_secs(5),
            ..task_opts(Some(Duration::from_secs(600)))
        };
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            replaced(CUT_GONE),
            refused(),
            refused(),
            refused(),
            refused(),
        ]);
        let (code, lines) = run_task(&mut s, &opts);
        assert_eq!(lines, ["task @91 nudged=0"]);
        assert_eq!(
            code,
            Err(
                "reconnect window lapsed: inbox: connect /d/aterm.sock: Connection refused (os \
                 error 61)"
                    .to_string()
            ),
            "requests: {:?}",
            s.requests
        );
        assert_eq!(s.requests.len(), 8, "{:?}", s.requests);

        let opts = TaskOpts {
            reconnect: Duration::ZERO,
            ..task_opts(Some(Duration::from_secs(600)))
        };
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            replaced(CUT_SINCE),
        ]);
        let (code, _) = run_task(&mut s, &opts);
        assert_eq!(code, Err(format!("await inbox: {CUT_SINCE}")));

        let post_cut = "aterm was replaced (pid 10 -> 11) before it answered; the \
                        `post` may already be queued (exit 75)";
        let said = |text: &str| CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: format!("aterm-ctl: {text}\n"),
        };
        for cut in [
            replaced(post_cut),
            said("server closed the connection without responding"),
            said("server hung up before the complete response"),
            said(
                "no reply from /d/aterm.sock within 5s (connected; the server did not answer) \
                 — the request timed out",
            ),
        ] {
            let mut s = Script::new(vec![cut.clone()]);
            let (code, lines) = run_task(&mut s, &task_opts(None));
            assert_eq!(
                code,
                Err(format!(
                    "task post got no answer and may have landed — look before posting it \
                     again (`outbox`, `timeline`): {}",
                    cut.err_text()
                ))
            );
            assert!(lines.is_empty());
            assert_eq!(s.requests.len(), 1, "never posted again: {:?}", s.requests);
        }
        for unread in [
            err("control server busy; retry"),
            err("auth"),
            err("main thread stalled 7s; retry"),
            refused(),
            said("connect /d/aterm.sock: No such file or directory (os error 2)"),
            said(
                "no answer from /d/aterm.sock within 5s: the instance took no request, so none \
                 was sent — its control listener is not taking connections",
            ),
        ] {
            let mut s = Script::new(vec![unread.clone()]);
            let (code, lines) = run_task(&mut s, &task_opts(None));
            assert_eq!(code, Err(format!("task not posted: {}", unread.err_text())));
            assert!(lines.is_empty());
            assert_eq!(s.requests.len(), 1, "never posted again: {:?}", s.requests);
        }
    }

    /// While the wait rides out an outage, the instance that answers may not
    /// host the manager's session yet: `ERR no such session` on the whole
    /// listing, on a wait re-armed after it, or on the answer's `inbox get`
    /// is one more request not served ([`CtlReply::unserved`]), as the
    /// loop's `Session::unserved` takes it. It ended the wait (`inbox: ERR
    /// no such session`) in the very handoff it rides out. With no outage
    /// open the same answer is the error, as before.
    #[test]
    fn an_outage_rides_out_a_successor_that_does_not_host_the_inbox_yet() {
        let answer = "OK 2 hold=0 holder=- seen=0 bus_head=96 dropped=0 pending=0\n\
                      msg 1 off=80 t=1000 from=h-owner kind=note trust=human len=3\n\
                      msg 2 off=95 t=4 from=s-1@n-1 kind=answer trust=agent re=91 len=5\n";
        let done = [
            "task @91 nudged=0",
            "MAIL id=2 off=95 from=s-1@n-1 kind=answer len=5 re=91",
            "done",
        ];
        // On the listing.
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            replaced(CUT_SINCE),
            err("no such session"),
            ok(answer),
            ok("done\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(Some(Duration::from_secs(30))));
        assert_eq!(lines, done, "requests: {:?}", s.requests);
        assert_eq!(code, Ok(0));
        assert_eq!(
            s.requests[3..],
            [
                "@self await inbox since=4 kinds=answer,report,ack timeout 30000",
                "@self inbox --peek --meta",
                "@self inbox --peek --meta",
                "@self inbox get 2",
            ]
        );

        // On the wait re-armed after a listing with no answer yet, and on
        // the answer's body: each lists the inbox again.
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            replaced(CUT_GONE),
            ok(
                "OK 1 hold=0 holder=- seen=0 bus_head=90 dropped=0 pending=0\n\
                msg 1 off=80 t=1000 from=h-owner kind=note trust=human len=3\n",
            ),
            err("no such session"),
            ok(answer),
            err("no such session"),
            ok(answer),
            ok("done\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(Some(Duration::from_secs(30))));
        assert_eq!(lines, done, "requests: {:?}", s.requests);
        assert_eq!(code, Ok(0));
        assert_eq!(
            s.requests[3..],
            [
                "@self await inbox since=4 kinds=answer,report,ack timeout 30000",
                "@self inbox --peek --meta",
                "@self await inbox since=1 kinds=answer,report,ack timeout 29500",
                "@self inbox --peek --meta",
                "@self inbox get 2",
                "@self inbox --peek --meta",
                "@self inbox get 2",
            ]
        );

        // No outage open: the session is gone, and that is the error.
        let mut s = Script::new(vec![
            ok(INBOX_1),
            ok("OK 3 off=91\n"),
            screen(&busy_screen()),
            err("no such session"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(Some(Duration::from_secs(30))));
        assert_eq!(lines, ["task @91 nudged=0"]);
        assert_eq!(code, Err("await inbox: ERR no such session".to_string()));
        assert_eq!(s.requests.len(), 4, "{:?}", s.requests);
    }

    /// A lane client whose instance is handed over while the wait is parked
    /// (a self-update): `first` answers the first requests, whatever they are
    /// — the baseline, the parked request the handoff cut, what the
    /// successor says while it takes the session over — and then the
    /// successor answers from its inbox `listing`, a `SessionFabric::default()`
    /// the bridge re-delivered on attach, whose rows count from 1 again. The
    /// lane must hear the successor's rows: told apart by their bus offset,
    /// and the wait re-armed from the successor's own newest id.
    struct Handoff {
        requests: Vec<String>,
        first: VecDeque<CtlReply>,
        /// The successor's whole `inbox --peek --meta`.
        listing: &'static str,
    }
    impl Handoff {
        /// The parked wait cut as `cut`, then the successor's inbox: the
        /// row the old instance had (off 80, redelivered as id 1) and the
        /// worker's report (id 2, off 91).
        fn new(cut: CtlReply) -> Self {
            Self::after(vec![ok(INBOX_1), cut], SUCCESSOR_INBOX)
        }
        fn after(first: Vec<CtlReply>, listing: &'static str) -> Self {
            Self {
                requests: Vec::new(),
                first: first.into(),
                listing,
            }
        }
    }
    const SUCCESSOR_INBOX: &str = "OK 2 hold=0 holder=- seen=0 bus_head=91 dropped=0 pending=0\n\
                                   msg 1 off=80 t=1000 from=h-owner kind=note trust=human len=3\n\
                                   msg 2 off=91 t=1 from=s-1@n-1 kind=report trust=agent len=12\n";
    impl Ctl for Handoff {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let line = args.join(" ");
            self.requests.push(line.clone());
            if let Some(r) = self.first.pop_front() {
                return Ok(r);
            }
            let newest = newest_id(self.listing);
            let since = num(&line, "since");
            Ok(if line.starts_with("@self await inbox") {
                if since.unwrap_or(0) < newest {
                    ok(&format!("OK inbox {newest}\n"))
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                    timeout()
                }
            } else if line.starts_with("@self inbox get") {
                ok("Suite green.\n")
            } else if let Some(since) = since {
                let mut body: Vec<&str> = vec![self.listing.lines().next().unwrap_or("OK 0")];
                body.extend(
                    self.listing
                        .lines()
                        .filter(|l| MailRow::parse(l).is_some_and(|r| r.id > since)),
                );
                ok(&(body.join("\n") + "\n"))
            } else {
                ok(self.listing)
            })
        }
    }

    /// Run the lane over `ctl`, watching the worker `@s-1`, until its first
    /// delivery, then stop it: what it said, and the delivery. The bound is
    /// a hang detector, not a latency one: the delivery returns the moment
    /// it is sent, and a lane that is off returns at once (its sender
    /// dropped); only a lane parked for good meets it.
    fn first_delivery<C: Ctl + Send>(
        ctl: &mut C,
    ) -> (
        Vec<String>,
        Result<Delivery, std::sync::mpsc::RecvTimeoutError>,
    ) {
        let said = std::sync::Mutex::new(Vec::<String>::new());
        let say = |line: &str| -> Result<(), String> {
            said.lock().expect("said").push(line.to_string());
            Ok(())
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = AtomicBool::new(false);
        let got = std::thread::scope(|s| {
            // The outage window is a hang detector too: every handoff here
            // is served on the next ask.
            let lane = Lane {
                ctl,
                inbox: "@self".to_string(),
                step: Duration::from_millis(20),
                reconnect: HANG_DETECTOR,
                pause: Duration::from_millis(1),
                pause_max: Duration::from_millis(5),
            };
            s.spawn(|| lane.run(Some("@s-1"), &say, tx, &stop));
            // 500 ms here was ~7 ms of work plus however long a loaded gate
            // left this thread unscheduled (the load-sensitive test audit of
            // 2026-09-27).
            let got = rx.recv_timeout(HANG_DETECTOR);
            stop.store(true, Ordering::Relaxed);
            got
        });
        (said.into_inner().expect("said"), got)
    }

    /// A lane's requests without their `timeout <ms>` tails.
    fn asked(requests: &[String]) -> Vec<String> {
        requests
            .iter()
            .map(|r| {
                r.find(" timeout ")
                    .map_or(r.clone(), |i| r[..i].to_string())
            })
            .collect()
    }

    /// The parked wait cut as `aterm-ctl` before 0.94 said it: a connection
    /// closed with no reply.
    #[test]
    fn the_lane_hears_the_successors_rows_after_a_handoff() {
        lane_hears_the_successor(CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: "aterm-ctl: server closed the connection without responding\n".to_string(),
        });
    }

    /// ROUND EIGHT, FINDING 13: the same handoff as `aterm-ctl` says it since
    /// its redial — exit 75 and a note, for an `await inbox since=` it does not ask
    /// the successor (the anchor is the old process's row id), or for a server
    /// nothing replaced within its wait. The lane read either as the server's
    /// answer and switched itself off (`MAIL off: await inbox: …`).
    #[test]
    fn the_lane_rides_out_an_await_inbox_the_update_cut_with_exit_75() {
        for note in [
            "aterm was replaced (pid 10 -> 11) before it answered; `since=4` names a \
             position in the old process — re-issue it (exit 75)",
            "aterm (pid 10) went away and nothing replaced it within 30.0 s; the exchange \
             cannot be resumed (exit 75)",
        ] {
            lane_hears_the_successor(CtlReply {
                code: i32::from(aterm_ctl::EXIT_REPLACED),
                stdout: String::new(),
                stderr: format!("aterm-ctl: {note}\n"),
            });
        }
    }

    fn lane_hears_the_successor(cut: CtlReply) {
        let mut h = Handoff::new(cut);
        let (said, got) = first_delivery(&mut h);
        // The redelivered row (off 80, seen before the handoff) is not said
        // again; the report (off 91) is new.
        assert_eq!(
            said,
            ["MAIL id=2 off=91 from=s-1@n-1 kind=report len=12"],
            "requests: {:?}",
            h.requests
        );
        let d = got.expect("the successor's report reached the loop");
        assert_eq!((d.id, d.off, d.rows), (2, 91, 1));
        let asked = asked(&h.requests);
        assert_eq!(
            &asked[..5],
            [
                "@self inbox 1 --peek --meta",
                &format!("@self await inbox since=4 kinds={MAIL_KINDS}"),
                &format!("@self await inbox since=4 kinds={MAIL_KINDS}"),
                "@self inbox --peek --meta",
                "@self inbox get 2",
            ]
        );
        assert!(
            asked[5..]
                .iter()
                .all(|r| r == &format!("@self await inbox since=2 kinds={MAIL_KINDS}")),
            "re-armed from the successor's newest id: {asked:?}"
        );
    }

    /// While the lane resyncs after an outage, the instance that answers the
    /// listing may not host the manager's session yet: `ERR no such session`
    /// there is one more request not served ([`CtlReply::unserved`]), as the
    /// loop's own `Session::unserved` takes it, and the lane hears the
    /// successor once it does. It went off instead (`MAIL off: inbox:
    /// ERR no such session`) in the very handoff it rides out. Outside an
    /// outage the same answer is final.
    #[test]
    fn a_resync_rides_out_a_successor_that_does_not_host_the_session_yet() {
        let mut h = Handoff::after(
            vec![
                ok(INBOX_1),
                replaced(CUT_SINCE),
                timeout(),
                err("no such session"),
            ],
            SUCCESSOR_INBOX,
        );
        let (said, got) = first_delivery(&mut h);
        assert_eq!(
            said,
            ["MAIL id=2 off=91 from=s-1@n-1 kind=report len=12"],
            "requests: {:?}",
            h.requests
        );
        let d = got.expect("the successor's report reached the loop");
        assert_eq!((d.id, d.off, d.rows), (2, 91, 1));
        assert_eq!(
            asked(&h.requests)[..6],
            [
                "@self inbox 1 --peek --meta".to_string(),
                format!("@self await inbox since=4 kinds={MAIL_KINDS}"),
                format!("@self await inbox since=4 kinds={MAIL_KINDS}"),
                "@self inbox --peek --meta".to_string(),
                "@self inbox --peek --meta".to_string(),
                "@self inbox get 2".to_string(),
            ]
        );

        // No outage open: the session is gone, and the lane says so.
        let mut h = Handoff::after(vec![ok(INBOX_1), err("no such session")], SUCCESSOR_INBOX);
        let (said, got) = first_delivery(&mut h);
        assert!(got.is_err(), "{said:?}");
        assert_eq!(
            said,
            ["MAIL off: await inbox: ERR no such session (the loop goes on without mail)"]
        );
    }

    /// A resync listing that took a retry (the handoff cut it too) is the
    /// instance's whole inbox all the same, and the lane takes up from it by
    /// offset. It was thrown away instead, and the lane awaited again from
    /// the OLD `since=4` on a successor that counts its ids anew: its rows
    /// 1 to 4 were never looked at, so the worker's report at off 91 (the
    /// successor's id 3) was never heard, and the first delivery was the
    /// later report at id 5.
    #[test]
    fn a_resync_listing_that_took_a_retry_is_taken_up_not_thrown_away() {
        const OLD: &str = "OK 4 hold=0 holder=- seen=4 bus_head=85 dropped=0 pending=0\n\
                           msg 1 off=70 t=1 from=h-owner kind=note trust=human len=3\n\
                           msg 2 off=75 t=2 from=h-owner kind=note trust=human len=3\n\
                           msg 3 off=80 t=3 from=h-owner kind=note trust=human len=3\n\
                           msg 4 off=85 t=4 from=h-owner kind=note trust=human len=3\n";
        const SUCCESSOR: &str = "OK 5 hold=0 holder=- seen=0 bus_head=99 dropped=0 pending=0\n\
                                 msg 1 off=80 t=3 from=h-owner kind=note trust=human len=3\n\
                                 msg 2 off=85 t=4 from=h-owner kind=note trust=human len=3\n\
                                 msg 3 off=91 t=5 from=s-1@n-1 kind=report trust=agent len=12\n\
                                 msg 4 off=95 t=6 from=h-owner kind=note trust=human len=3\n\
                                 msg 5 off=99 t=7 from=s-1@n-1 kind=report trust=agent len=12\n";
        let mut h = Handoff::after(
            vec![ok(OLD), replaced(CUT_SINCE), timeout(), replaced(CUT_GONE)],
            SUCCESSOR,
        );
        let (said, got) = first_delivery(&mut h);
        let d = got.expect("a report reached the loop");
        assert_eq!(
            (d.id, d.off),
            (3, 91),
            "said: {said:?}, requests: {:?}",
            h.requests
        );
        // The lane goes on handing rows while the test stops it: only the
        // first line is the delivery's.
        assert_eq!(
            said.first().map(String::as_str),
            Some("MAIL id=3 off=91 from=s-1@n-1 kind=report len=12")
        );
    }

    /// A post that did not land is the error, in the server's own words —
    /// `queued=1` says it will land when a bridge drains the outbox, and
    /// must not be re-posted — and nothing is nudged.
    #[test]
    fn a_post_that_did_not_land_is_the_servers_words() {
        let mut s = Script::new(vec![err("fabric absent id=3 queued=1 no-bridge=1")]);
        let (code, lines) = run_task(&mut s, &task_opts(None));
        assert_eq!(
            code,
            Err("task not posted: ERR fabric absent id=3 queued=1 no-bridge=1".to_string())
        );
        assert!(lines.is_empty());
        assert_eq!(s.requests.len(), 1);
        // A nudge the server refused: the task line stands, the refusal is
        // the error.
        let mut s = Script::new(vec![
            ok("OK 3 off=91\n"),
            screen(&idle_screen()),
            err("halted"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(None));
        assert_eq!(code, Err("nudge refused: ERR halted".to_string()));
        assert_eq!(lines, ["task @91 nudged=0"]);
        // A worker that cannot be read for the nudge: the offset is still
        // printed — the post landed — and the read is the error.
        let mut s = Script::new(vec![ok("OK 3 off=91\n"), err("no such session")]);
        let (code, lines) = run_task(&mut s, &task_opts(None));
        assert_eq!(lines, ["task @91 nudged=0"]);
        let why = code.expect_err("the worker could not be read");
        assert!(
            why.starts_with("cannot read the worker for the nudge: ")
                && why.contains("no such session"),
            "{why}"
        );
    }
}
