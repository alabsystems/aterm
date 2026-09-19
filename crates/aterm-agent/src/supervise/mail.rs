// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mail as the manager's channel: `watch --mail` / `supervise --mail` park ONE
//! `await inbox` on the manager's own session beside the screen loop, and
//! `aterm drive task` sends a worker its work by mail.
//!
//! Measured on 2026-09-14: a manager woken by `EVENT idle` read a 689-row
//! `report` to learn what the worker had said, where the worker's own
//! end-of-turn `report` mail was 2 KB. So the loop's wake for a worker turn
//! is now ONE line, `EVENT turn seq=<n> report=<id> rows=<n> <summary>`, and
//! the manager reads the body with `inbox get <id>`.
//!
//! The lane ([`Lane`]) is a thread of its own with a control client of its
//! own — it never touches the worker's socket — and it POLLS NOTHING: it
//! parks `await inbox since=<id>` with the loop's 20 s step, lists what
//! landed when it latches, prints `MAIL id=<n> off=<o> from=<sid> kind=<k>
//! len=<n> [re=<o>]` per row through the loop's one sink, hands a `report`
//! from the watched worker to the loop ([`Delivery`]), and re-arms from the
//! newest id. `since=` is the endpoint's monotone row id, so a row it saw
//! cannot wake it twice, and a delivery between two arms is caught by the
//! next arm (`await inbox` latches at once on a row already above `since`).
//! A request the server did not serve is retried like the loop's, and once
//! one is served again the lane RE-BASELINES: the instance answering now may
//! be another one (an aterm self-update hands every session to its
//! successor, whose inbox is filled again by the bridge and counts its rows
//! from 1), so the rows are listed whole and told apart by their bus offset
//! (`off=`, monotone across instances), and the next `since=` is the
//! successor's newest id — measured 2026-09-14: a lane parked on the old
//! `since=` heard nothing for the rest of the run.
//!
//! `task` ([`task`]) posts `kind=task` from the manager's session — the body
//! travels by mail, never through the PTY — then, unless `--no-nudge`, types
//! the one-line nudge `Inbox: task @<off>` as a `turn` ONLY when the worker
//! is idle, and with `--wait` parks `await inbox` for the `answer|report|ack`
//! that carries `re=<off>` — re-armed while time is left, since the host
//! clamps one wait at 600 s and a bound above that (`--deadline 900`) gets
//! the host's `OK timeout` with time to spare.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::phase::{Phase, worker_phase};
use super::run::{Ctl, CtlReply, EXIT_TIMEOUT, Session};

/// `--report-window`'s default: how long before its `EVENT idle` a worker's
/// `report` mail may have come to be folded into it when the loop cannot
/// tell which turn it belongs to ([`MailOpts::report_window`]).
pub const DEFAULT_REPORT_WINDOW: Duration = Duration::from_secs(120);
/// `--idle-grace`'s default: how long an idle point waits for the worker's
/// report before it is said as `EVENT idle-no-report`.
pub const DEFAULT_IDLE_GRACE: Duration = Duration::from_secs(180);
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

    /// Whether this is `worker`'s `report` (the sender's bare sid, the node
    /// aside, is the worker's).
    pub fn is_report_from(&self, worker: &str) -> bool {
        self.kind == "report" && bare_sid(&self.from) == bare_sid(worker)
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
/// does not start another).
pub fn body_rows(body: &str) -> usize {
    body.lines().count()
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
        "MAIL lane off: {} (the loop goes on without mail)",
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
    /// ([`CtlReply::lost`]) retried after a pause that doubles up to
    /// `pause_max`, for at most `reconnect`; `Err` for a client that could not
    /// be launched or a window that lapsed. The reply says whether it took a
    /// retry to get it.
    fn call(&mut self, args: &[&str], stop: &AtomicBool) -> Result<Got, String> {
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
        full.push(self.inbox.as_str());
        full.extend_from_slice(args);
        let mut since: Option<Instant> = None;
        let mut pause = self.pause;
        loop {
            let r = self.ctl.call(&full)?;
            if !r.lost() {
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
        match self.call(&["inbox", "1", "--peek", "--meta"], stop)? {
            Got::Stopped => Ok(None),
            Got::Reply(r, _) if r.ok() => Ok(Some(Mark::newest(&r.stdout))),
            Got::Reply(r, _) => Err(format!("inbox: {}", r.err_text())),
        }
    }

    /// Print one listed row and, for the watched worker's `report`, hand it
    /// to the loop with its body's row count. `true` when the loop ended
    /// under it.
    fn hand(
        &mut self,
        row: &MailRow,
        worker: Option<&str>,
        say: &(dyn Fn(&str) -> Result<(), String> + Sync),
        tx: &Sender<Delivery>,
        stop: &AtomicBool,
    ) -> Result<bool, String> {
        let report = worker.is_some_and(|w| row.is_report_from(w));
        let body_rows = if report {
            match self.call(&["inbox", "get", &row.id.to_string()], stop)? {
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

    /// After an outage: list the inbox WHOLE and take up from what the bus
    /// offset says is new. The instance answering now may be the successor
    /// of the one the lane was parked on (an aterm self-update hands every
    /// session over; the successor's inbox is `SessionFabric::default()`,
    /// filled again by the bridge on attach, its row ids from 1), so an id
    /// from before says nothing — `since=<old id>` on it parks until the
    /// successor has counted that high, which may be never. The offset is
    /// the broker's and monotone across instances: a row above the last
    /// offset seen is new, printed and handed over; the next `since=` is the
    /// successor's newest id, whatever it is. On the same instance (a socket
    /// hiccup) this is the ordinary listing under another name. `true` when
    /// the loop ended under it.
    fn resync(
        &mut self,
        mark: &mut Mark,
        worker: Option<&str>,
        say: &(dyn Fn(&str) -> Result<(), String> + Sync),
        tx: &Sender<Delivery>,
        stop: &AtomicBool,
    ) -> Result<bool, String> {
        let listed = match self.call(&["inbox", "--peek", "--meta"], stop)? {
            Got::Stopped => return Ok(true),
            // Lost again mid-resync: the loop's next turn resyncs afresh.
            Got::Reply(_, true) => return Ok(false),
            Got::Reply(r, false) if r.ok() => r.stdout,
            Got::Reply(r, false) => return Err(format!("inbox: {}", r.err_text())),
        };
        let all = parse_rows(&listed);
        let newest_id = all.iter().map(|r| r.id).max().unwrap_or(0);
        let mut rows: Vec<MailRow> = all.into_iter().filter(|r| r.off > mark.off).collect();
        rows.sort_by_key(|row| row.off);
        for row in &rows {
            if self.hand(row, worker, say, tx, stop)? {
                return Ok(true);
            }
            mark.off = mark.off.max(row.off);
        }
        mark.id = newest_id;
        Ok(false)
    }

    /// Park on the inbox and print what lands, until `stop` or a failure
    /// (said as `MAIL lane off: …`, once). `worker` is the watched session's
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
            let r = match self.call(&args, stop)? {
                Got::Stopped => return Ok(()),
                Got::Reply(_, true) => {
                    resync = true;
                    continue;
                }
                Got::Reply(r, false) => r,
            };
            // The loop ended while the wait was parked — cut short by the
            // loop's interrupter, or run out — and whatever the client
            // answered to that is not the lane's to judge.
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
                return Err(format!("await inbox: {}", r.err_text()));
            }
            let latched = field(r.stdout.trim(), "hold").is_none();
            if !latched {
                continue;
            }
            let listed = match self.call(&["inbox", &since_arg, "--peek", "--meta"], stop)? {
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
                if self.hand(row, worker, say, tx, stop)? {
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
    /// Type the nudge when the worker is idle (off with `--no-nudge`).
    pub nudge: bool,
    /// `--wait`: how long to park for the answer.
    pub wait: Option<Duration>,
}

/// Post the task, nudge an idle worker, and with `--wait` print the answer.
/// Prints `task @<off> nudged=<0|1>` as soon as the post landed (and, with
/// `--wait`, the answer's `MAIL` line and its body); returns the exit code:
/// `0`, or [`EXIT_TIMEOUT`] when `--wait` ran out (its last line then says
/// `TIMEOUT …`). The wait is the BOUND's, not one request's: the host clamps
/// an `await` at 600 s, so its `OK timeout` with time left is re-armed from
/// the same `since=`, and only the bound spent is the TIMEOUT. `Err` when the
/// post did not land — the server's own words, `queued=1` or `no-bridge=1`
/// among them, are the instruction — when the worker could not be read, or
/// when the nudge's turn was refused.
pub fn task<C: Ctl>(ctl: &mut C, opts: &TaskOpts, out: &mut dyn Write) -> Result<u8, String> {
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
    args.push(opts.text.as_str());
    let r = ctl.call(&args)?;
    if !r.ok() {
        return Err(format!("task not posted: {}", r.err_text()));
    }
    let off = num(r.stdout.trim(), "off").ok_or_else(|| {
        format!(
            "task not landed: the post answered `{}` with no offset",
            r.stdout.trim()
        )
    })?;
    // The post LANDED: whatever the nudge comes to, the offset is printed —
    // it is the one thing the manager cannot recover (a worker that cannot
    // be read, a turn the server refused, are the error AFTER it).
    let mut nudged = false;
    let mut refused = None;
    if opts.nudge {
        match Session::new(ctl, Some(opts.worker.clone())).read_screen() {
            Err(why) => refused = Some(format!("cannot read the worker for the nudge: {why}")),
            Ok(screen) if worker_phase(&screen.rows) == Phase::Idle => {
                let nudge = format!("{NUDGE}{off}");
                let r = ctl.call(&[&opts.worker, "turn", NUDGE_IDLE, NUDGE_TIMEOUT, &nudge])?;
                nudged = r.stdout.contains("submitted=1");
                if !nudged && !r.timed_out() {
                    refused = Some(format!("nudge refused: {}", r.err_text()));
                }
            }
            Ok(_) => {}
        }
    }
    emit(out, &format!("task @{off} nudged={}", u8::from(nudged)))?;
    if let Some(why) = refused {
        return Err(why);
    }
    let Some(bound) = opts.wait else {
        return Ok(0);
    };
    let deadline = Instant::now() + bound;
    let mut since = since;
    loop {
        // The wire's unit is the millisecond: a remainder under one is the
        // bound spent, not a `timeout 0` to arm and re-arm until it passes.
        let left = deadline.saturating_duration_since(Instant::now());
        let ms = left.as_millis();
        if ms == 0 {
            break;
        }
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
        // The host's own step running out (600 s at most a wait), not the
        // bound's: re-arm while time is left.
        if r.timed_out() {
            continue;
        }
        if !r.ok() {
            return Err(format!("await inbox: {}", r.err_text()));
        }
        let r = ctl.call(&[&opts.inbox, "inbox", &since_arg, "--peek", "--meta"])?;
        if !r.ok() {
            return Err(format!("inbox: {}", r.err_text()));
        }
        let mut rows = parse_rows(&r.stdout);
        rows.retain(|row| row.id > since);
        rows.sort_by_key(|row| row.id);
        for row in &rows {
            since = since.max(row.id);
        }
        let answer = rows
            .iter()
            .find(|row| row.re == Some(off) && ANSWER_KINDS.split(',').any(|k| k == row.kind));
        if let Some(row) = answer {
            emit(out, &row.line())?;
            let r = ctl.call(&[&opts.inbox, "inbox", "get", &row.id.to_string()])?;
            if !r.ok() {
                return Err(format!("inbox get {}: {}", row.id, r.err_text()));
            }
            out.write_all(r.stdout.as_bytes())
                .and_then(|()| {
                    if r.stdout.ends_with('\n') || r.stdout.is_empty() {
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

/// Write one line and flush it.
fn emit(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|e| format!("cannot write a line to stdout: {e}"))
}

#[cfg(test)]
mod tests {
    use super::super::prompt::fixtures::{composer, rows};
    use super::*;
    use std::collections::VecDeque;

    /// A scripted client: every request pops the next reply (`OK` with none
    /// left) and is recorded. An `await` answered with a timeout PARKS first,
    /// for the timeout asked or [`Script::PARK`], whichever is shorter — as
    /// the host does — so a wait's bound is spent by waiting, not by spinning.
    struct Script {
        requests: Vec<String>,
        replies: VecDeque<CtlReply>,
    }

    impl Script {
        const PARK: Duration = Duration::from_millis(25);

        fn new(replies: Vec<CtlReply>) -> Self {
            Self {
                requests: Vec::new(),
                replies: replies.into(),
            }
        }
    }

    impl Ctl for Script {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let line = args.join(" ");
            self.requests.push(line.clone());
            let reply = self.replies.pop_front().unwrap_or_else(|| ok("OK\n"));
            if reply.timed_out() && line.contains(" await ") {
                let asked = num(&line, "timeout")
                    .or_else(|| line.rsplit(' ').next().and_then(|ms| ms.parse().ok()))
                    .map_or(Self::PARK, Duration::from_millis);
                std::thread::sleep(asked.min(Self::PARK));
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
    fn task_opts(nudge: bool, wait: Option<Duration>) -> TaskOpts {
        TaskOpts {
            worker: "@s-1".to_string(),
            inbox: "@self".to_string(),
            text: "run the suite and report".to_string(),
            deadline: Some(Duration::from_secs(600)),
            nudge,
            wait,
        }
    }
    fn run_task(s: &mut Script, opts: &TaskOpts) -> (Result<u8, String>, Vec<String>) {
        let mut out: Vec<u8> = Vec::new();
        let code = task(s, opts, &mut out);
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
    /// worker is nudged with ONE turn naming the offset; the line says so.
    #[test]
    fn task_posts_by_mail_and_nudges_an_idle_worker() {
        let mut s = Script::new(vec![
            ok("OK 3 off=91\n"),
            screen(&idle_screen()),
            ok("turn 5 submitted=1 status=timeout seq=9 dur_ms=2500 hash=0000000000000000\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(true, None));
        assert_eq!(code, Ok(0));
        assert_eq!(lines, ["task @91 nudged=1"]);
        assert_eq!(
            s.requests,
            [
                "@self post to=@s-1 kind=task dl=600000 run the suite and report",
                "@s-1 text --json tail=40",
                "@s-1 turn idle=600 timeout=2500 Inbox: task @91",
            ]
        );
    }

    /// A busy worker gets the mail only: no nudge is typed into a running
    /// turn, and the line says `nudged=0`.
    #[test]
    fn a_busy_worker_is_not_nudged() {
        let mut s = Script::new(vec![ok("OK 3 off=91\n"), screen(&busy_screen())]);
        let (code, lines) = run_task(&mut s, &task_opts(true, None));
        assert_eq!(code, Ok(0));
        assert_eq!(lines, ["task @91 nudged=0"]);
        assert_eq!(s.requests.len(), 2, "{:?}", s.requests);
        assert!(!s.requests.iter().any(|r| r.contains("turn")));
    }

    /// `--no-nudge` (a worker with the wake hook installed): the post alone,
    /// not even a read of the worker's screen.
    #[test]
    fn no_nudge_posts_and_reads_nothing() {
        let mut s = Script::new(vec![ok("OK 3 off=91\n")]);
        let (code, lines) = run_task(&mut s, &task_opts(false, None));
        assert_eq!(code, Ok(0));
        assert_eq!(lines, ["task @91 nudged=0"]);
        assert_eq!(
            s.requests,
            ["@self post to=@s-1 kind=task dl=600000 run the suite and report"]
        );
        // No deadline: no `dl=`.
        let mut s = Script::new(vec![ok("OK 3 off=91\n")]);
        let opts = TaskOpts {
            deadline: None,
            ..task_opts(false, None)
        };
        assert_eq!(run_task(&mut s, &opts).0, Ok(0));
        assert_eq!(
            s.requests,
            ["@self post to=@s-1 kind=task run the suite and report"]
        );
    }

    /// `--wait`: the newest inbox id is read BEFORE the post (an answer that
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
        let (code, lines) = run_task(&mut s, &task_opts(true, Some(Duration::from_secs(30))));
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
                "@self post to=@s-1 kind=task dl=600000 run the suite and report",
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
            timeout(),
            timeout(),
            timeout(),
            timeout(),
        ]);
        let bound = Duration::from_millis(60);
        let started = Instant::now();
        let (code, lines) = run_task(&mut s, &task_opts(false, Some(bound)));
        // THE BOUND IS SPENT BY WAITING, measured against a clock that rounds.
        // A sleep of `bound` can return a hair early — 59.714 ms against 60 ms in
        // this release's gate — because the sleep's own timer and `Instant` are
        // not the same clock and neither promises the other's resolution. The
        // slop is a TIMER tolerance, not a licence to return early: a waiter that
        // skipped its wait misses by milliseconds, not microseconds, and still
        // fails here.
        const TIMER_SLOP: Duration = Duration::from_millis(1);
        assert!(
            started.elapsed() + TIMER_SLOP >= bound,
            "the wait returned {:?} short of its {bound:?} bound",
            bound.saturating_sub(started.elapsed())
        );
        assert_eq!(code, Ok(EXIT_TIMEOUT));
        assert_eq!(
            lines,
            [
                "task @91 nudged=0",
                "TIMEOUT no answer, report or ack re=91 within 0.06 s",
            ]
        );
        let awaits: Vec<&String> = s
            .requests
            .iter()
            .filter(|r| r.contains("await inbox since=4 "))
            .collect();
        assert!(
            (2..=4).contains(&awaits.len()),
            "re-armed until the bound: {:?}",
            s.requests
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
            timeout(),
            ok("OK inbox 6\n"),
            ok(
                "OK 1 hold=0 holder=- seen=3 bus_head=96 dropped=0 pending=0\n\
                msg 6 off=95 t=4 from=s-1@n-1 kind=answer trust=agent re=91 len=5\n",
            ),
            ok("done\n"),
        ]);
        let (code, lines) = run_task(&mut s, &task_opts(false, Some(Duration::from_secs(30))));
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

    /// A lane client whose instance is handed over while the wait is parked
    /// (a self-update): the parked request comes back lost once, and the
    /// successor's inbox — a `SessionFabric::default()`, the bridge
    /// re-delivering on attach — counts its rows from 1 again. The lane must
    /// hear the successor's rows: told apart by their bus offset, and the
    /// wait re-armed from the successor's own newest id.
    struct Handoff {
        requests: Vec<String>,
        lost: bool,
    }
    impl Ctl for Handoff {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let line = args.join(" ");
            self.requests.push(line.clone());
            let since = num(&line, "since").unwrap_or(0);
            Ok(if line.starts_with("@self await inbox") {
                if !self.lost {
                    self.lost = true;
                    CtlReply {
                        code: 1,
                        stdout: String::new(),
                        stderr: "aterm-ctl: server closed the connection without responding\n"
                            .to_string(),
                    }
                } else if since < 1 {
                    ok("OK inbox 1\n")
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                    timeout()
                }
            } else if line.starts_with("@self inbox get") {
                ok("Suite green.\n")
            } else if self.lost {
                // The successor's inbox: the row the old instance had (off
                // 80, redelivered as id 1) and the worker's report (id 2).
                ok(
                    "OK 2 hold=0 holder=- seen=0 bus_head=91 dropped=0 pending=0\n\
                    msg 1 off=80 t=1000 from=h-owner kind=note trust=human len=3\n\
                    msg 2 off=91 t=1 from=s-1@n-1 kind=report trust=agent len=12\n",
                )
            } else {
                ok(INBOX_1)
            })
        }
    }

    #[test]
    fn the_lane_hears_the_successors_rows_after_a_handoff() {
        let mut h = Handoff {
            requests: Vec::new(),
            lost: false,
        };
        let said = std::sync::Mutex::new(Vec::<String>::new());
        let say = |line: &str| -> Result<(), String> {
            said.lock().expect("said").push(line.to_string());
            Ok(())
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = AtomicBool::new(false);
        let got = std::thread::scope(|s| {
            let lane = Lane {
                ctl: &mut h,
                inbox: "@self".to_string(),
                step: Duration::from_millis(20),
                reconnect: Duration::from_secs(5),
                pause: Duration::from_millis(1),
                pause_max: Duration::from_millis(5),
            };
            s.spawn(|| lane.run(Some("@s-1"), &say, tx, &stop));
            let got = rx.recv_timeout(Duration::from_millis(500));
            stop.store(true, Ordering::Relaxed);
            got
        });
        let said = said.into_inner().expect("said");
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
        let asked: Vec<String> = h
            .requests
            .iter()
            .map(|r| {
                r.find(" timeout ")
                    .map_or(r.clone(), |i| r[..i].to_string())
            })
            .collect();
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

    /// A post that did not land is the error, in the server's own words —
    /// `queued=1` says it will land when a bridge drains the outbox, and
    /// must not be re-posted — and nothing is nudged.
    #[test]
    fn a_post_that_did_not_land_is_the_servers_words() {
        let mut s = Script::new(vec![err("fabric absent id=3 queued=1 no-bridge=1")]);
        let (code, lines) = run_task(&mut s, &task_opts(true, None));
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
        let (code, lines) = run_task(&mut s, &task_opts(true, None));
        assert_eq!(code, Err("nudge refused: ERR halted".to_string()));
        assert_eq!(lines, ["task @91 nudged=0"]);
        // A worker that cannot be read for the nudge: the offset is still
        // printed — the post landed — and the read is the error.
        let mut s = Script::new(vec![ok("OK 3 off=91\n"), err("no such session")]);
        let (code, lines) = run_task(&mut s, &task_opts(true, None));
        assert_eq!(lines, ["task @91 nudged=0"]);
        let why = code.expect_err("the worker could not be read");
        assert!(
            why.starts_with("cannot read the worker for the nudge: ")
                && why.contains("no such session"),
            "{why}"
        );
    }
}
