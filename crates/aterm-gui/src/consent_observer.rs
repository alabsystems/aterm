// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS `tccd` log consent observer ([`ObserverState`]) and `EPERM` attention gate.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Absolute path to `/usr/bin/log` from `aterm_containment::consent`.
#[cfg(any(target_os = "macos", test))]
pub(crate) use aterm_containment::consent::LOG_TOOL;
#[cfg(any(target_os = "macos", test))]
const _: () = assert!(LOG_TOOL.as_bytes()[0] == b'/');

/// Whether this account can read the unified log at all (the admin-group test).
#[cfg(target_os = "macos")]
use aterm_containment::consent::log_store_readable;

/// Full-mode `log stream` predicate matching all `tccd` messages.
#[cfg(any(target_os = "macos", test))]
pub(crate) const STREAM_PREDICATE: &str = "process == \"tccd\"";

/// Sentinel-mode `log stream` predicate matching `tccd` `AUTHREQ_PROMPTING` lines.
#[cfg(any(target_os = "macos", test))]
pub(crate) const SENTINEL_PREDICATE: &str =
    "process == \"tccd\" AND eventMessage CONTAINS \"AUTHREQ_PROMPTING\"";

/// Duration after which each sentinel `log stream` generation rotates (`--timeout`).
#[cfg(any(target_os = "macos", test))]
pub(crate) const SENTINEL_TIMEOUT: &str = "15m";

/// A sentinel generation that ran at least this long ended because of its
/// timeout (one second of slack under fifteen minutes).
#[cfg(any(target_os = "macos", test))]
const ROTATION_MIN: Duration = Duration::from_secs(14 * 60 + 59);

/// How many early ends the sentinel restarts from in one hour.
#[cfg(any(target_os = "macos", test))]
const EARLY_RESTARTS_PER_HOUR: usize = 3;

/// The on-demand attribution lookup is killed after this long.
#[cfg(any(target_os = "macos", test))]
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(4);

/// At most this many lookups run at once; a burst of dialogs beyond it counts
/// as needs without a tab.
#[cfg(target_os = "macos")]
const MAX_LOOKUPS: usize = 2;

/// Prompts waiting for the host to take them (bounded like everything fed by
/// the stream).
#[cfg(any(target_os = "macos", test))]
const PROMPTS_CAP: usize = 16;

/// Configured `tccd` observer mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObserverMode {
    /// `observer = false`: nothing is spawned or observed.
    Off,
    /// Default sentinel stream (`AUTHREQ_PROMPTING`) feeding the access card.
    Sentinel,
    /// `observer = true`: full `tccd` stream with a tab mark for each pending dialog.
    Full,
}

const EVENT_QUEUE_CAP: usize = 256;

/// Bound on the in-flight correlation table, a ring fed by a stream this process does not
/// control: the oldest entry is evicted, and an evicted entry produces no verdict.
#[cfg(any(target_os = "macos", test))]
const PENDING_CAP: usize = 512;

/// Maximum age (`10m`) before an uncorrelated in-flight request is expired.
pub(crate) const PENDING_MAX_AGE: Duration = Duration::from_secs(600);

/// Minimum unanswered duration (`3s`) before an `AUTHREQ_CTX` counts as a pending prompt.
pub(crate) const PENDING_PROMPT_THRESHOLD: Duration = Duration::from_secs(3);

const ANNOUNCED_CAP: usize = 64;

const FORMAT_DRIFT_LIMIT: u32 = 64;

/// Reason the `tccd` observer is unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnavailableReason {
    /// Headless or unit-test inert probe arm.
    Inert,
    /// Non-macOS platform without `tccd`.
    #[cfg(any(not(target_os = "macos"), test))]
    UnsupportedPlatform,
    /// Current account cannot read `aterm_containment::consent::DIAGNOSTICS_DIR`.
    #[cfg(any(target_os = "macos", test))]
    NoLogAccess,
    /// [`LOG_TOOL`] is missing or failed to spawn.
    LogToolUnavailable,
    /// Log stream exited unexpectedly.
    StreamEnded,
    /// `AUTHREQ_*` lines arrived in an unrecognized format.
    FormatUnrecognized,
}

impl UnavailableReason {
    /// Wire token for this unavailability reason.
    #[cfg(test)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Inert => "inert",
            Self::UnsupportedPlatform => "unsupported-platform",
            Self::NoLogAccess => "no-log-access",
            Self::LogToolUnavailable => "log-tool-unavailable",
            Self::StreamEnded => "stream-ended",
            Self::FormatUnrecognized => "format-unrecognized",
        }
    }
}

/// Current `observer log=` availability state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ObserverAvailability {
    /// Not started or [`ObserverMode::Off`].
    #[default]
    Off,
    /// Stream is live and parsing lines.
    #[cfg(any(target_os = "macos", test))]
    Ok,
    /// Observer was requested but cannot run.
    Unavailable(UnavailableReason),
}

impl ObserverAvailability {
    const fn is_live(self) -> bool {
        #[cfg(any(target_os = "macos", test))]
        {
            matches!(self, Self::Ok)
        }
        #[cfg(not(any(target_os = "macos", test)))]
        {
            false
        }
    }

    /// Wire token for `observer log=`.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            #[cfg(any(target_os = "macos", test))]
            Self::Ok => "ok",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// Underlying [`UnavailableReason`], if unavailable.
    #[cfg(test)]
    pub(crate) const fn reason(self) -> Option<UnavailableReason> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            Self::Off | Self::Ok => None,
        }
    }
}

/// Parsed `tccd` `AUTHREQ_*` log message correlated by `msg_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum TccEvent {
    /// `AUTHREQ_CTX`: request opened for `service`.
    Ctx {
        msg_id: String,
        service: String,
        /// Whether `preflight=yes` was set (`None` if absent).
        preflight: Option<bool>,
    },
    /// `AUTHREQ_ATTRIBUTION`: responsible, accessing, and requesting PIDs.
    Attribution {
        msg_id: String,
        accessing_pid: Option<i32>,
        requesting_pid: Option<i32>,
    },
    /// `AUTHREQ_SUBJECT`: code identity subject of the request.
    Subject { msg_id: String, subject: String },
    /// `AUTHREQ_RESULT`: request completed.
    Result { msg_id: String },
    /// `AUTHREQ_PROMPTING`: consent dialog is on screen.
    Prompting {
        msg_id: String,
        service: String,
        subject: String,
        resp_pid: i32,
    },
    /// Unrecognized `AUTHREQ_*` line shape.
    Unparsed,
}

/// The value after `key`, up to the first `,`, `}` or whitespace. Deliberately naive: the
/// format is undocumented, so only the shapes recorded in the tests are claimed, and everything
/// else is [`TccEvent::Unparsed`].
#[cfg(any(target_os = "macos", test))]
fn field_after<'a>(haystack: &'a str, key: &str) -> Option<&'a str> {
    let start = haystack.find(key)? + key.len();
    let rest = haystack.get(start..)?;
    let end = rest.find([',', '}', ' ', '\t']).unwrap_or(rest.len());
    let value = rest.get(..end)?.trim();
    (!value.is_empty()).then_some(value)
}

/// Parses a single `tccd` log line into a [`TccEvent`], if it is an `AUTHREQ_*` message.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_line(line: &str) -> Option<TccEvent> {
    // `log stream` opens every generation with a header echoing the predicate (`Filtering the
    // log data using "… CONTAINS "AUTHREQ_PROMPTING""`). Counted as `Unparsed` once per
    // rotation, it would trip the drift limit after 64 quiet generations (16 h).
    if line.trim_start().starts_with("Filtering the log data") {
        return None;
    }
    let marker = line.find("AUTHREQ_")?;
    let body = line.get(marker..)?;
    let kind_end = body.find(|c: char| !(c.is_ascii_uppercase() || c == '_'))?;
    if body.as_bytes().get(kind_end) != Some(&b':') {
        return None;
    }
    let msg_id = field_after(body, "msgID=").map(str::to_owned);
    if body.starts_with("AUTHREQ_ATTRIBUTION") {
        let Some(msg_id) = msg_id else {
            return Some(TccEvent::Unparsed);
        };
        if !body.contains("pid=") {
            return Some(TccEvent::Unparsed);
        }
        return Some(TccEvent::Attribution {
            msg_id,
            accessing_pid: pid_in_block(body, "accessing="),
            requesting_pid: pid_in_block(body, "requesting="),
        });
    }
    if body.starts_with("AUTHREQ_SUBJECT") {
        return Some(match (msg_id, field_after(body, "subject=")) {
            (Some(msg_id), Some(subject)) => TccEvent::Subject {
                msg_id,
                subject: subject.to_owned(),
            },
            _ => TccEvent::Unparsed,
        });
    }
    if body.starts_with("AUTHREQ_PROMPTING") {
        let subject = body
            .split_once("Sub:{")
            .and_then(|(_, rest)| rest.split_once('}'))
            .map(|(subject, _)| subject.trim())
            .filter(|subject| !subject.is_empty());
        return Some(
            match (
                msg_id,
                field_after(body, "service="),
                subject,
                pid_in_block(body, "Resp:{"),
            ) {
                (Some(msg_id), Some(service), Some(subject), Some(resp_pid)) => {
                    TccEvent::Prompting {
                        msg_id,
                        service: service.to_owned(),
                        subject: subject.to_owned(),
                        resp_pid,
                    }
                }
                _ => TccEvent::Unparsed,
            },
        );
    }
    if body.starts_with("AUTHREQ_CTX") {
        return Some(match (msg_id, field_after(body, "service=")) {
            (Some(msg_id), Some(service)) => TccEvent::Ctx {
                msg_id,
                service: service.to_owned(),
                preflight: field_after(body, "preflight=").map(parse_yes_no),
            },
            _ => TccEvent::Unparsed,
        });
    }
    if body.starts_with("AUTHREQ_RESULT") {
        return Some(msg_id.map_or(TccEvent::Unparsed, |msg_id| TccEvent::Result { msg_id }));
    }
    None
}

/// The first `pid=` inside one named attribution block. The blocks share one line and the first
/// is `responsible=` (the terminal application, the same pid for every session), so a pid is
/// never read off the line as a whole.
#[cfg(any(target_os = "macos", test))]
fn pid_in_block(body: &str, block: &str) -> Option<i32> {
    let at = body.find(block)?;
    let rest = body.get(at..)?;
    field_after(rest, "pid=")
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))?
        .parse::<i32>()
        .ok()
}

/// `yes`/`true`/`1` are true; anything else is false. Case-insensitive.
#[cfg(any(target_os = "macos", test))]
fn parse_yes_no(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "yes" | "true" | "1"
    )
}

/// One in-flight `tccd` request.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingEntry {
    msg_id: String,
    service: Option<String>,
    accessing_pid: Option<i32>,
    requesting_pid: Option<i32>,
    subject: Option<String>,
    preflight: Option<bool>,
    first_seen: Instant,
}

/// Attribution block from which a prompt's PID was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PidSource {
    /// `accessing={…}` block.
    Accessing,
    /// `requesting={…}` fallback block.
    Requesting,
}

/// Open `tccd` request that has exceeded [`PENDING_PROMPT_THRESHOLD`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingPrompt<'a> {
    /// Correlation `msgID`.
    pub(crate) msg_id: &'a str,
    /// PID of the process parked in the syscall.
    pub(crate) accessing_pid: i32,
    /// Attribution block that provided [`Self::accessing_pid`].
    pub(crate) pid_source: PidSource,
    /// Code identity subject, if received.
    pub(crate) subject: Option<&'a str>,
    /// Elapsed time since the request opened.
    pub(crate) age: Duration,
}

impl PendingEntry {
    fn signature(&self) -> Option<(i32, PidSource)> {
        if self.preflight == Some(true) || self.service.is_none() {
            return None;
        }
        match (self.accessing_pid, self.requesting_pid) {
            (Some(pid), _) => Some((pid, PidSource::Accessing)),
            (None, Some(pid)) => Some((pid, PidSource::Requesting)),
            (None, None) => None,
        }
    }
}

/// Bounded table of in-flight `tccd` requests keyed by `msgID`.
#[derive(Debug, Default)]
pub(crate) struct PendingTable {
    entries: VecDeque<PendingEntry>,
    /// Consecutive [`TccEvent::Unparsed`] since the last understood event.
    unparsed_run: u32,
    /// How many events this table has ever understood. Zero plus a long
    /// unparsed run is the format-change signature.
    parsed_total: u64,
}

impl PendingTable {
    /// Creates an empty [`PendingTable`].
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Folds a single [`TccEvent`] into the table at `now`.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn note(&mut self, event: TccEvent, now: Instant) {
        if matches!(event, TccEvent::Unparsed) {
            self.unparsed_run = self.unparsed_run.saturating_add(1);
            return;
        }
        self.unparsed_run = 0;
        self.parsed_total = self.parsed_total.saturating_add(1);
        match event {
            TccEvent::Ctx {
                msg_id,
                service,
                preflight,
            } => {
                let slot = self.slot(&msg_id, now);
                slot.service = Some(service);
                slot.preflight = preflight;
            }
            TccEvent::Attribution {
                msg_id,
                accessing_pid,
                requesting_pid,
            } => {
                let slot = self.slot(&msg_id, now);
                slot.accessing_pid = accessing_pid.or(slot.accessing_pid);
                slot.requesting_pid = requesting_pid.or(slot.requesting_pid);
            }
            TccEvent::Subject { msg_id, subject } => {
                self.slot(&msg_id, now).subject = Some(subject);
            }
            TccEvent::Prompting { .. } => {}
            TccEvent::Result { msg_id } => self.entries.retain(|e| e.msg_id != msg_id),
            TccEvent::Unparsed => unreachable!("handled above"),
        }
    }

    /// The entry for `msg_id`, created if new; a full ring evicts its oldest ([`PENDING_CAP`]).
    #[cfg(any(target_os = "macos", test))]
    fn slot(&mut self, msg_id: &str, now: Instant) -> &mut PendingEntry {
        if let Some(index) = self.entries.iter().position(|e| e.msg_id == msg_id) {
            return &mut self.entries[index];
        }
        if self.entries.len() >= PENDING_CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(PendingEntry {
            msg_id: msg_id.to_owned(),
            service: None,
            accessing_pid: None,
            requesting_pid: None,
            subject: None,
            preflight: None,
            first_seen: now,
        });
        self.entries.back_mut().expect("an entry was just pushed")
    }

    /// Removes entries older than `max_age` ([`PENDING_MAX_AGE`]).
    pub(crate) fn expire(&mut self, now: Instant, max_age: Duration) {
        self.entries
            .retain(|e| now.saturating_duration_since(e.first_seen) < max_age);
    }

    /// Returns non-preflight requests open for at least `threshold` with known service and PID.
    pub(crate) fn pending_prompts(
        &self,
        now: Instant,
        threshold: Duration,
    ) -> Vec<PendingPrompt<'_>> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let age = now.saturating_duration_since(entry.first_seen);
                if age < threshold {
                    return None;
                }
                let (accessing_pid, pid_source) = entry.signature()?;
                Some(PendingPrompt {
                    msg_id: &entry.msg_id,
                    accessing_pid,
                    pid_source,
                    subject: entry.subject.as_deref(),
                    age,
                })
            })
            .collect()
    }

    fn next_crossing(&self, now: Instant, threshold: Duration) -> Option<Instant> {
        self.entries
            .iter()
            .filter(|entry| entry.signature().is_some())
            .filter_map(|entry| entry.first_seen.checked_add(threshold))
            .filter(|crossing| *crossing > now)
            .min()
    }

    /// Whether the stream has produced only unparsed `AUTHREQ_*` lines past the format-change threshold.
    pub(crate) const fn format_looks_changed(&self) -> bool {
        self.parsed_total == 0 && self.unparsed_run >= FORMAT_DRIFT_LIMIT
    }

    /// Number of in-flight requests.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no requests are in flight.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Resolves which session in `sessions` owns `accessing_pid` by PID or process group.
pub(crate) fn owning_session(
    accessing_pid: i32,
    pgid: Option<i32>,
    sessions: &[(u64, i32)],
) -> Option<u64> {
    if accessing_pid <= 0 {
        return None;
    }
    sessions
        .iter()
        .find(|(_, shell)| *shell == accessing_pid)
        .or_else(|| {
            pgid.filter(|g| *g > 0)
                .and_then(|g| sessions.iter().find(|(_, shell)| *shell == g))
        })
        .map(|(session, _)| *session)
}

/// Resolves which session owns `pid` by walking its parent chain via `ppid_of`.
pub(crate) fn session_of_pid(
    pid: i32,
    sessions: &[(u64, i32)],
    parent_of: impl Fn(i32) -> Option<i32>,
    pgid_of: impl Fn(i32) -> Option<i32>,
) -> Option<u64> {
    let mut current = pid;
    for _ in 0..32 {
        if current <= 1 {
            return None;
        }
        if let Some(session) = owning_session(current, pgid_of(current), sessions) {
            return Some(session);
        }
        current = parent_of(current)?;
    }
    None
}

/// Message sent from the background `log stream` worker to [`ObserverState`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObserverEvent {
    /// Child process started and is streaming lines.
    #[cfg(any(target_os = "macos", test))]
    Started,
    /// Observer could not start or continue.
    Unavailable(UnavailableReason),
    /// Parsed `tccd` `AUTHREQ_*` event.
    #[cfg(any(target_os = "macos", test))]
    Line(TccEvent),
    /// On-screen consent dialog (`AUTHREQ_PROMPTING`).
    #[cfg(any(target_os = "macos", test))]
    Prompt(PromptNeed),
    /// Stream exited and child process was reaped.
    #[cfg(any(target_os = "macos", test))]
    Ended,
}

/// Observed on-screen consent dialog (`AUTHREQ_PROMPTING`) with PID attribution.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) struct PromptNeed {
    /// Dialog correlation `msgID`.
    pub(crate) msg_id: String,
    /// TCC service name.
    pub(crate) service: String,
    /// Code identity subject (`com.aterm.aterm`).
    pub(crate) subject: String,
    /// Responsible process PID (`Resp:{…}`).
    pub(crate) resp_pid: i32,
    /// Accessing process PID, if resolved.
    pub(crate) accessing_pid: Option<i32>,
}

/// Channel and child-process state shared with the streaming worker thread.
#[derive(Clone)]
pub(crate) struct StreamSink {
    tx: SyncSender<ObserverEvent>,
    poke: Arc<std::sync::Mutex<Box<dyn Fn() + Send>>>,
    /// Set by the post that pokes, cleared by [`ObserverState::drain`]
    /// before it reads the queue: one wake per drain, however many events a
    /// burst queues.
    poked: Arc<AtomicBool>,
    /// The child's pid while it lives and the stop flag, shared with
    /// [`ObserverState::stop`] so a stop can SIGNAL the child without ever
    /// holding its handle — the event loop must never own something it could
    /// be tempted to `wait()` on.
    #[cfg(target_os = "macos")]
    child: Arc<ChildSlot>,
    /// Lookups in flight ([`MAX_LOOKUPS`]).
    #[cfg(target_os = "macos")]
    lookups: Arc<std::sync::atomic::AtomicUsize>,
}

impl StreamSink {
    fn post(&self, event: ObserverEvent) {
        match self.tx.try_send(event) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(dropped)) => {
                aterm_log::debug!("consent observer dropped a full queue's message: {dropped:?}");
            }
        }
        self.poke_once();
    }

    fn poke_once(&self) {
        if self.poked.swap(true, Ordering::AcqRel) {
            return;
        }
        let poke = self.poke.lock().unwrap_or_else(|p| p.into_inner());
        (poke)();
    }
}

/// Injected OS probe arm for the `tccd` log observer.
#[derive(Clone, Copy)]
pub(crate) struct ObserverProbe {
    run: fn(&StreamSink, ObserverMode),
    parent_of: fn(i32) -> Option<i32>,
    pgid_of: fn(i32) -> Option<i32>,
    live: bool,
}

impl ObserverProbe {
    /// Live macOS probe arm for windowed instances.
    pub(crate) const fn live() -> Self {
        Self {
            run: live_run_stream,
            parent_of: crate::session_status::program::parent_of,
            pgid_of: live_pgid_of,
            live: true,
        }
    }

    /// Inert probe arm for headless and unit-test instances.
    pub(crate) const fn inert() -> Self {
        Self {
            run: inert_run_stream,
            parent_of: inert_process_probe,
            pgid_of: inert_process_probe,
            live: false,
        }
    }

    /// Returns [`Self::live`] when `!headless`, or [`Self::inert`] when `headless`.
    pub(crate) const fn for_instance(headless: bool) -> Self {
        if headless {
            Self::inert()
        } else {
            Self::live()
        }
    }

    /// Whether this probe arm can access the OS log.
    #[cfg(test)]
    pub(crate) const fn is_live(self) -> bool {
        self.live
    }
}

const fn inert_process_probe(_pid: i32) -> Option<i32> {
    None
}

fn inert_run_stream(sink: &StreamSink, _mode: ObserverMode) {
    sink.post(ObserverEvent::Unavailable(UnavailableReason::Inert));
}

/// Builds `log stream` CLI arguments for `mode`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn stream_args(mode: ObserverMode) -> Vec<String> {
    let mut args = vec!["stream".to_owned(), "--predicate".to_owned()];
    match mode {
        ObserverMode::Sentinel => {
            args.push(SENTINEL_PREDICATE.to_owned());
            args.push("--info".to_owned());
            args.push("--timeout".to_owned());
            args.push(SENTINEL_TIMEOUT.to_owned());
        }
        ObserverMode::Full | ObserverMode::Off => {
            args.push(STREAM_PREDICATE.to_owned());
            args.push("--info".to_owned());
        }
    }
    args
}

/// Builds `log show` CLI arguments to look up attribution for `msg_id`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn lookup_args(msg_id: &str) -> Option<Vec<String>> {
    if msg_id.is_empty() || !msg_id.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some(vec![
        "show".to_owned(),
        "--last".to_owned(),
        "2m".to_owned(),
        "--info".to_owned(),
        "--predicate".to_owned(),
        format!(
            "process == \"tccd\" AND eventMessage CONTAINS \"AUTHREQ_ATTRIBUTION: msgID={msg_id},\""
        ),
        "--style".to_owned(),
        "compact".to_owned(),
    ])
}

/// Extracts the accessing (or requesting) PID for `msg_id` from `log show` `output`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn accessing_pid_in(output: &str, msg_id: &str) -> Option<i32> {
    output.lines().find_map(|line| match parse_line(line)? {
        TccEvent::Attribution {
            msg_id: id,
            accessing_pid,
            requesting_pid,
        } if id == msg_id => accessing_pid.or(requesting_pid),
        _ => None,
    })
}

/// Action to take after a sentinel stream child process exits.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamEnd {
    /// Normal timeout rotation; start the next generation immediately.
    Rotate,
    /// Early exit within the hourly restart budget; restart.
    Restart,
    /// Early-restart budget exhausted; stop and report `Unavailable(StreamEnded)`.
    GiveUp,
}

/// Restart budget policy for sentinel stream generations.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
pub(crate) struct RotationPolicy {
    early: VecDeque<Instant>,
}

#[cfg(any(target_os = "macos", test))]
impl RotationPolicy {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Evaluates [`StreamEnd`] when a generation of `mode` exits after `ran_for` at `now`.
    pub(crate) fn on_end(
        &mut self,
        mode: ObserverMode,
        ran_for: Duration,
        now: Instant,
    ) -> StreamEnd {
        if mode != ObserverMode::Sentinel {
            return StreamEnd::GiveUp;
        }
        if ran_for >= ROTATION_MIN {
            return StreamEnd::Rotate;
        }
        let hour = Duration::from_secs(60 * 60);
        self.early
            .retain(|at| now.saturating_duration_since(*at) < hour);
        if self.early.len() >= EARLY_RESTARTS_PER_HOUR {
            return StreamEnd::GiveUp;
        }
        self.early.push_back(now);
        StreamEnd::Restart
    }
}

/// Run [`LOG_TOOL`] with `args`, feeding every stdout line to `on_line`, then kill and reap the
/// child here, on the worker that spawned it: this module's one spawn and one `wait()`. The
/// child is tethered: a window killed outright takes its stream with it, which would otherwise
/// run out its `--timeout`.
#[cfg(target_os = "macos")]
fn run_log_tool(
    args: &[String],
    slot: &Arc<ChildSlot>,
    kill_after: Option<Duration>,
    on_spawn: impl FnOnce(),
    mut on_line: impl FnMut(&str),
) -> Result<(), UnavailableReason> {
    use aterm_uds::spawnfd::{Stdio, Tether as _};
    use std::io::BufRead;

    let Ok((stdout, stdout_end)) = aterm_uds::pipe() else {
        return Err(UnavailableReason::LogToolUnavailable);
    };
    let Ok(mut child) = aterm_uds::spawnfd::command(LOG_TOOL)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_end))
        .stderr(Stdio::null())
        .tether()
        .spawn()
    else {
        return Err(UnavailableReason::LogToolUnavailable);
    };
    let reading = slot.publish(i32::try_from(child.id()).unwrap_or(0), send_signal);
    if let Some(limit) = kill_after {
        let slot = Arc::clone(slot);
        let _ = std::thread::Builder::new()
            .name("consent-lookup-watchdog".into())
            .spawn(move || slot.kill_unless_retired_within(limit, send_signal));
    }
    if reading {
        on_spawn();
        for line in std::io::BufReader::new(stdout).lines() {
            if slot.stopped() {
                break;
            }
            let Ok(line) = line else { break };
            on_line(&line);
        }
    }
    slot.retire();
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// One dialog on screen: post it, with the accessing pid where it can be found.
#[cfg(target_os = "macos")]
fn note_prompting(
    sink: &StreamSink,
    msg_id: String,
    service: String,
    subject: String,
    resp_pid: i32,
) {
    let ours = i32::try_from(std::process::id()).unwrap_or(-1);
    let need = PromptNeed {
        msg_id,
        service,
        subject,
        resp_pid,
        accessing_pid: (resp_pid != ours).then_some(resp_pid),
    };
    let Some(args) = (resp_pid == ours)
        .then(|| lookup_args(&need.msg_id))
        .flatten()
    else {
        sink.post(ObserverEvent::Prompt(need));
        return;
    };
    if sink.lookups.fetch_add(1, Ordering::AcqRel) >= MAX_LOOKUPS {
        sink.lookups.fetch_sub(1, Ordering::AcqRel);
        sink.post(ObserverEvent::Prompt(need));
        return;
    }
    let lookup_sink = sink.clone();
    let fallback = need.clone();
    let spawned = std::thread::Builder::new()
        .name("consent-lookup".into())
        .spawn(move || {
            let mut output = String::new();
            let _ = run_log_tool(
                &args,
                &Arc::new(ChildSlot::default()),
                Some(LOOKUP_TIMEOUT),
                || {},
                |line| {
                    output.push_str(line);
                    output.push('\n');
                },
            );
            let mut need = need;
            need.accessing_pid = accessing_pid_in(&output, &need.msg_id);
            lookup_sink.lookups.fetch_sub(1, Ordering::AcqRel);
            lookup_sink.post(ObserverEvent::Prompt(need));
        });
    if spawned.is_err() {
        sink.lookups.fetch_sub(1, Ordering::AcqRel);
        sink.post(ObserverEvent::Prompt(fallback));
    }
}

/// The live arm: one `log stream` child for [`ObserverMode::Full`], a rotating series for the
/// sentinel ([`RotationPolicy`]).
#[cfg(target_os = "macos")]
fn live_run_stream(sink: &StreamSink, mode: ObserverMode) {
    if !log_store_readable() {
        sink.post(ObserverEvent::Unavailable(UnavailableReason::NoLogAccess));
        return;
    }
    let mut policy = RotationPolicy::new();
    let mut started_posted = false;
    while !sink.child.stopped() {
        let generation = Instant::now();
        let generation_wall = std::time::SystemTime::now();
        let ran = run_log_tool(
            &stream_args(mode),
            &sink.child,
            None,
            || {
                if !started_posted {
                    started_posted = true;
                    sink.post(ObserverEvent::Started);
                }
            },
            |line| match parse_line(line) {
                Some(TccEvent::Prompting {
                    msg_id,
                    service,
                    subject,
                    resp_pid,
                }) => note_prompting(sink, msg_id, service, subject, resp_pid),
                Some(event) => sink.post(ObserverEvent::Line(event)),
                None => {}
            },
        );
        if let Err(reason) = ran {
            sink.post(ObserverEvent::Unavailable(reason));
            return;
        }
        let ran_for = generation
            .elapsed()
            .max(generation_wall.elapsed().unwrap_or_default());
        match policy.on_end(mode, ran_for, Instant::now()) {
            StreamEnd::Rotate => {}
            StreamEnd::Restart => {
                if sink.child.pause_unless_stopped(Duration::from_secs(1)) {
                    break;
                }
            }
            StreamEnd::GiveUp => break,
        }
    }
    sink.post(ObserverEvent::Ended);
}

/// Off macOS there is no `tccd` to stream: the live arm reports `unsupported-platform`.
#[cfg(not(target_os = "macos"))]
fn live_run_stream(sink: &StreamSink, _mode: ObserverMode) {
    sink.post(ObserverEvent::Unavailable(
        UnavailableReason::UnsupportedPlatform,
    ));
}

/// Outcome of [`ObserverState::start`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    /// Streaming worker thread was spawned.
    Started,
    /// Mode is [`ObserverMode::Off`]; nothing was spawned.
    Disabled,
    /// Worker was already running.
    AlreadyLive,
    /// Spawning the worker thread failed.
    SpawnFailed,
}

/// Per-instance `tccd` observer state and correlation table.
pub(crate) struct ObserverState {
    probe: ObserverProbe,
    availability: ObserverAvailability,
    /// What the host last asked for: [`Self::start`] records its mode, `Off`
    /// and a failed spawn included, and [`Self::stop`] records `Off`.
    requested: ObserverMode,
    /// The mode of the stream that is running: its worker's `Started` sets
    /// it to `requested`, and [`Self::stop`] and the disconnect edge in
    /// [`Self::drain`] return it to `Off`.
    running: ObserverMode,
    rx: Option<Receiver<ObserverEvent>>,
    /// The worker's child and stop flag ([`ChildSlot`]), new with each
    /// channel.
    child: Arc<ChildSlot>,
    /// The worker's poke latch ([`StreamSink::poke_once`]), cleared by
    /// [`Self::drain`].
    poked: Arc<AtomicBool>,
    table: PendingTable,
    /// Whether an event was folded since the last verdict pass
    /// ([`Self::take_new_verdicts`]).
    verdicts_dirty: bool,
    /// The roster generation the last verdict pass judged against.
    judged_roster: Option<u64>,
    /// When the next request crosses [`PENDING_PROMPT_THRESHOLD`], as the last
    /// verdict pass found it: the pass's deadline on a loop nothing else
    /// wakes.
    verdict_due: Option<Instant>,
    /// Announced prompts and their tab owner. Only a matching result answers
    /// a pending prompt; lost observations and expired correlations do not.
    announced: VecDeque<AnnouncedPrompt>,
    /// A tab contribution moved since the host last pushed its chrome.
    attention_dirty: bool,
    /// Observed dialogs waiting for the host ([`Self::take_prompting`]).
    prompts: VecDeque<PromptNeed>,
    /// `msgID`s of dialogs already queued, so one dialog is one need however
    /// often its line repeats.
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    prompt_seen: VecDeque<String>,
}

struct AnnouncedPrompt {
    id: String,
    session: u64,
    /// Whether this prompt still marks its tab and is kept from eviction, from its announcement
    /// until macOS's result or the end of its stream ([`ObserverState::end_marks`]) ends that.
    pending: bool,
}

impl ObserverState {
    /// Creates an [`ObserverState`] for a windowed (`!headless`) or headless instance.
    pub(crate) fn new(headless: bool) -> Self {
        Self {
            probe: ObserverProbe::for_instance(headless),
            availability: ObserverAvailability::Off,
            requested: ObserverMode::Off,
            running: ObserverMode::Off,
            rx: None,
            child: Arc::new(ChildSlot::default()),
            poked: Arc::new(AtomicBool::new(false)),
            table: PendingTable::new(),
            verdicts_dirty: false,
            judged_roster: None,
            verdict_due: None,
            announced: VecDeque::new(),
            attention_dirty: false,
            prompts: VecDeque::new(),
            prompt_seen: VecDeque::new(),
        }
    }

    /// Creates an inert [`ObserverState`] for headless and unit tests.
    pub(crate) fn inert() -> Self {
        Self::new(true)
    }

    /// Whether the underlying probe arm is live.
    #[cfg(test)]
    pub(crate) const fn probe_is_live(&self) -> bool {
        self.probe.is_live()
    }

    /// Current `observer log=` availability.
    pub(crate) const fn availability(&self) -> ObserverAvailability {
        self.availability
    }

    /// Whether a log stream worker is currently running.
    #[cfg(test)]
    pub(crate) const fn is_live(&self) -> bool {
        self.rx.is_some()
    }

    /// Mode last requested via [`Self::start`] or [`Self::reconcile`].
    pub(crate) const fn requested(&self) -> ObserverMode {
        self.requested
    }

    /// Mode of the currently running stream.
    pub(crate) const fn running(&self) -> ObserverMode {
        self.running
    }

    /// Number of in-flight request correlations.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.table.len()
    }

    /// Starts the log stream worker in `mode` if not already running.
    pub(crate) fn start<P>(&mut self, mode: ObserverMode, poke: P) -> StartOutcome
    where
        P: Fn() + Send + 'static,
    {
        if mode == ObserverMode::Off {
            if self.rx.is_some() {
                self.stop();
            }
            self.requested = ObserverMode::Off;
            self.availability = ObserverAvailability::Off;
            return StartOutcome::Disabled;
        }
        if self.rx.is_some() {
            return StartOutcome::AlreadyLive;
        }
        self.requested = mode;
        self.table = PendingTable::new();
        self.verdict_due = None;
        let (sink, rx) = self.channel(poke);
        let run = self.probe.run;
        let spawned = std::thread::Builder::new()
            .name("consent-observer".into())
            .spawn(move || run(&sink, mode));
        match spawned {
            Ok(_handle) => {}
            Err(err) => {
                aterm_log::warn!("consent observer could not start its worker: {err}");
                self.availability =
                    ObserverAvailability::Unavailable(UnavailableReason::LogToolUnavailable);
                return StartOutcome::SpawnFailed;
            }
        }
        self.rx = Some(rx);
        StartOutcome::Started
    }

    fn channel<P>(&mut self, poke: P) -> (StreamSink, Receiver<ObserverEvent>)
    where
        P: Fn() + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::sync_channel::<ObserverEvent>(EVENT_QUEUE_CAP);
        self.child = Arc::new(ChildSlot::default());
        self.poked = Arc::new(AtomicBool::new(false));
        let sink = StreamSink {
            tx,
            poke: Arc::new(std::sync::Mutex::new(Box::new(poke))),
            poked: Arc::clone(&self.poked),
            #[cfg(target_os = "macos")]
            child: Arc::clone(&self.child),
            #[cfg(target_os = "macos")]
            lookups: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        (sink, rx)
    }

    /// Reconciles the running stream with the desired `mode`.
    pub(crate) fn reconcile<P>(&mut self, mode: ObserverMode, poke: P) -> Option<StartOutcome>
    where
        P: Fn() + Send + 'static,
    {
        if mode == self.requested {
            return None;
        }
        self.stop();
        Some(self.start(mode, poke))
    }

    /// Signals the running log stream child to exit and drops the receiver.
    pub(crate) fn stop(&mut self) {
        self.child.stop(send_signal);
        self.rx = None;
        self.requested = ObserverMode::Off;
        self.running = ObserverMode::Off;
        self.end_marks();
        if self.availability.is_live() {
            self.availability = ObserverAvailability::Unavailable(UnavailableReason::StreamEnded);
        }
    }

    /// Drains queued worker events into the table at `now`, returning the number folded.
    pub(crate) fn drain(&mut self, now: Instant) -> usize {
        self.poked.swap(false, Ordering::AcqRel);
        let mut queued: Vec<ObserverEvent> = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = self.rx.take() {
            loop {
                match rx.try_recv() {
                    Ok(event) => queued.push(event),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if !disconnected {
                self.rx = Some(rx);
            }
        }
        let applied = queued.len();
        for event in queued {
            self.apply(event, now);
        }
        self.settle(now);
        if disconnected {
            self.running = ObserverMode::Off;
            self.end_marks();
            if self.availability.is_live() {
                self.availability =
                    ObserverAvailability::Unavailable(UnavailableReason::StreamEnded);
            }
        }
        applied
    }

    fn settle(&mut self, now: Instant) {
        self.table.expire(now, PENDING_MAX_AGE);
        let drifting = self.table.format_looks_changed();
        if drifting {
            self.availability =
                ObserverAvailability::Unavailable(UnavailableReason::FormatUnrecognized);
        }
        #[cfg(any(target_os = "macos", test))]
        if !drifting
            && self.availability
                == ObserverAvailability::Unavailable(UnavailableReason::FormatUnrecognized)
        {
            self.availability = ObserverAvailability::Ok;
        }
    }

    fn apply(&mut self, event: ObserverEvent, now: Instant) {
        #[cfg(not(any(target_os = "macos", test)))]
        let _ = now;
        self.verdicts_dirty = true;
        match event {
            #[cfg(any(target_os = "macos", test))]
            ObserverEvent::Started => {
                self.availability = ObserverAvailability::Ok;
                self.running = self.requested;
            }
            ObserverEvent::Unavailable(reason) => {
                self.availability = ObserverAvailability::Unavailable(reason);
                self.end_marks();
            }
            #[cfg(any(target_os = "macos", test))]
            ObserverEvent::Line(line) => {
                if let TccEvent::Result { msg_id } = &line {
                    for prompt in &mut self.announced {
                        if &prompt.id == msg_id && std::mem::take(&mut prompt.pending) {
                            self.attention_dirty = true;
                        }
                    }
                }
                self.table.note(line, now);
            }
            #[cfg(any(target_os = "macos", test))]
            ObserverEvent::Prompt(need) => {
                self.table.note(
                    TccEvent::Prompting {
                        msg_id: need.msg_id.clone(),
                        service: need.service.clone(),
                        subject: need.subject.clone(),
                        resp_pid: need.resp_pid,
                    },
                    now,
                );
                if !self.prompt_seen.contains(&need.msg_id) {
                    if self.prompt_seen.len() >= ANNOUNCED_CAP {
                        self.prompt_seen.pop_front();
                    }
                    self.prompt_seen.push_back(need.msg_id.clone());
                    if self.prompts.len() >= PROMPTS_CAP {
                        self.prompts.pop_front();
                    }
                    self.prompts.push_back(need);
                }
            }
            #[cfg(any(target_os = "macos", test))]
            ObserverEvent::Ended => {
                if self.availability.is_live() {
                    self.availability =
                        ObserverAvailability::Unavailable(UnavailableReason::StreamEnded);
                }
                self.end_marks();
            }
        }
    }

    /// Ends every announced prompt's pending state, so no tab stays marked by a stream that has
    /// stopped: with no stream left, no result can answer the prompt. A stop, the worker's
    /// `Ended` or `Unavailable` (each the last thing it posts) and the channel's disconnect each
    /// call it.
    fn end_marks(&mut self) {
        for prompt in &mut self.announced {
            self.attention_dirty |= std::mem::take(&mut prompt.pending);
        }
    }

    /// Resolves which session in `sessions` owns `pid` using this instance's probe arm.
    pub(crate) fn session_for_pid(&self, pid: i32, sessions: &[(u64, i32)]) -> Option<u64> {
        session_of_pid(pid, sessions, self.probe.parent_of, self.probe.pgid_of)
    }

    /// Whether `session` currently has an announced pending prompt.
    pub(crate) fn marks_session(&self, session: u64) -> bool {
        self.announced
            .iter()
            .any(|p| p.session == session && p.pending)
    }

    pub(crate) fn take_attention_changed(&mut self) -> bool {
        std::mem::take(&mut self.attention_dirty)
    }

    /// Clears any announced prompts attributed to retired `session`.
    pub(crate) fn retire_session(&mut self, session: u64) {
        self.attention_dirty |= self.marks_session(session);
        self.announced.retain(|p| p.session != session);
    }

    /// [`Self::take_new_verdicts`] using this instance's live process-group resolver.
    pub(crate) fn take_new_verdicts_for_instance(
        &mut self,
        now: Instant,
        roster: u64,
        sessions: impl FnOnce() -> Vec<(u64, i32)>,
    ) -> Vec<u64> {
        let pgid_of = self.probe.pgid_of;
        self.take_new_verdicts(now, roster, sessions, pgid_of)
    }

    /// Drains newly observed consent dialogs whose subject matches `bundle_id`.
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    pub(crate) fn take_prompting(&mut self, bundle_id: Option<&str>) -> Vec<PromptNeed> {
        let Some(bundle_id) = bundle_id else {
            return Vec::new();
        };
        self.prompts
            .drain(..)
            .filter(|need| need.subject == bundle_id)
            .collect()
    }

    /// Returns the session of each newly observed pending prompt attributed to `sessions`, marking
    /// the prompt announced.
    pub(crate) fn take_new_verdicts(
        &mut self,
        now: Instant,
        roster: u64,
        sessions: impl FnOnce() -> Vec<(u64, i32)>,
        pgid_of: impl Fn(i32) -> Option<i32>,
    ) -> Vec<u64> {
        let due = self.verdicts_dirty
            || self.judged_roster != Some(roster)
            || self.verdict_due.is_some_and(|due| now >= due);
        if !due {
            return Vec::new();
        }
        self.verdicts_dirty = false;
        self.judged_roster = Some(roster);
        self.verdict_due = self.table.next_crossing(now, PENDING_PROMPT_THRESHOLD);
        if !self.availability.is_live() {
            return Vec::new();
        }
        let announced = &self.announced;
        let open: Vec<PendingPrompt<'_>> = self
            .table
            .pending_prompts(now, PENDING_PROMPT_THRESHOLD)
            .into_iter()
            .filter(|prompt| !announced.iter().any(|p| p.id == prompt.msg_id))
            .collect();
        if open.is_empty() {
            return Vec::new();
        }
        let sessions = sessions();
        let mut fresh: Vec<u64> = Vec::new();
        for prompt in open {
            let pgid = pgid_of(prompt.accessing_pid);
            let Some(session) = owning_session(prompt.accessing_pid, pgid, &sessions) else {
                continue;
            };
            if self.announced.len() >= ANNOUNCED_CAP {
                let Some(done) = self.announced.iter().position(|p| !p.pending) else {
                    continue;
                };
                self.announced.remove(done);
            }
            self.announced.push_back(AnnouncedPrompt {
                id: prompt.msg_id.to_owned(),
                session,
                pending: true,
            });
            self.attention_dirty = true;
            fresh.push(session);
        }
        fresh
    }

    /// Earliest instant when an in-flight request will cross [`PENDING_PROMPT_THRESHOLD`].
    pub(crate) const fn verdict_deadline(&self) -> Option<Instant> {
        self.verdict_due
    }
}

/// Strips the `kTCCService` prefix from `service`.
pub(crate) fn short_service(service: &str) -> &str {
    let service = service.trim();
    service.strip_prefix("kTCCService").unwrap_or(service)
}

/// Human-readable description of the protected resource for short TCC service ID `short`.
pub(crate) fn service_words(short: &str) -> Option<&'static str> {
    use aterm_containment::consent::Folder;
    match Folder::ALL.iter().find(|f| f.tcc_service() == short) {
        Some(folder) => Some(folder.label()),
        None => match short {
            "SystemPolicyNetworkVolumes" => Some("network drives"),
            "SystemPolicyRemovableVolumes" => Some("removable drives"),
            "SystemPolicyAllFiles" => Some("all your files"),
            "SystemPolicyAppBundles" => Some("other apps' bundles"),
            "MediaLibrary" => Some("the music and media library"),
            "Photos" => Some("the Photos library"),
            "AddressBook" => Some("your contacts"),
            "Calendar" => Some("your calendars"),
            "Reminders" => Some("your reminders"),
            "Camera" => Some("the camera"),
            "Microphone" => Some("the microphone"),
            "ScreenCapture" => Some("your screen"),
            "ListenEvent" => Some("your keyboard input"),
            "Accessibility" | "PostEvent" => Some("accessibility features"),
            "AppleEvents" => Some("another app"),
            _ => None,
        },
    }
}

impl Default for ObserverState {
    fn default() -> Self {
        Self::inert()
    }
}

impl Drop for ObserverState {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for ObserverState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObserverState")
            .field("probe_live", &self.probe.live)
            .field("availability", &self.availability)
            .field("live", &self.rx.is_some())
            .field("pending", &self.table.len())
            .finish()
    }
}

/// A signal the observer sends a child it spawned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signal {
    /// [`ObserverState::stop`]'s request that the stream exit.
    Term,
    /// The lookup watchdog's, at [`LOOKUP_TIMEOUT`].
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    Kill,
}

/// What every thread that may signal a worker's child knows of it.
#[derive(Debug, Default)]
struct ChildCell {
    /// The child's pid from its publication until its worker retires it to
    /// reap, `0` otherwise.
    pid: i32,
    /// Set by [`ChildSlot::stop`], never cleared.
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    stopped: bool,
}

impl ChildCell {
    fn signal_if_unreaped(&self, signal: Signal, kill: impl Fn(i32, Signal)) {
        if self.pid > 0 {
            kill(self.pid, signal);
        }
    }
}

/// One worker's child, shared by the worker that spawned it, the host's
/// [`ObserverState::stop`] and a lookup's watchdog.
#[derive(Debug, Default)]
struct ChildSlot {
    cell: Mutex<ChildCell>,
    /// Notified when the pid is retired and when a stop is asked.
    changed: Condvar,
}

impl ChildSlot {
    fn lock(&self) -> MutexGuard<'_, ChildCell> {
        self.cell.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stop(&self, kill: impl Fn(i32, Signal)) {
        let mut cell = self.lock();
        cell.stopped = true;
        cell.signal_if_unreaped(Signal::Term, kill);
        drop(cell);
        self.changed.notify_all();
    }
}

/// The worker's side, which only the macOS arm runs.
#[cfg(any(target_os = "macos", test))]
impl ChildSlot {
    fn publish(&self, pid: i32, kill: impl Fn(i32, Signal)) -> bool {
        let mut cell = self.lock();
        cell.pid = pid;
        if cell.stopped {
            cell.signal_if_unreaped(Signal::Term, kill);
            return false;
        }
        true
    }

    fn stopped(&self) -> bool {
        self.lock().stopped
    }

    fn retire(&self) {
        self.lock().pid = 0;
        self.changed.notify_all();
    }

    fn kill_unless_retired_within(&self, limit: Duration, kill: impl Fn(i32, Signal)) {
        let (cell, _) = self
            .changed
            .wait_timeout_while(self.lock(), limit, |cell| cell.pid != 0)
            .unwrap_or_else(PoisonError::into_inner);
        cell.signal_if_unreaped(Signal::Kill, kill);
    }

    fn pause_unless_stopped(&self, pause: Duration) -> bool {
        let (cell, _) = self
            .changed
            .wait_timeout_while(self.lock(), pause, |cell| !cell.stopped)
            .unwrap_or_else(PoisonError::into_inner);
        cell.stopped
    }
}

/// Deliver `signal` to `pid`. NON-BLOCKING by construction: it signals and
/// returns, and the worker that spawned the child is what reaps it.
#[cfg(unix)]
fn send_signal(pid: i32, signal: Signal) {
    if pid <= 1 {
        return;
    }
    let signal = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` with `pid > 1` delivers a signal to our unreaped child process without memory effects.
    unsafe {
        libc::kill(pid, signal);
    }
}

/// No child is ever spawned off unix, so there is never one to signal.
#[cfg(not(unix))]
fn send_signal(_pid: i32, _signal: Signal) {}

/// Process group ID of `pid` via `getpgid(2)`, or `None` on failure.
#[cfg(unix)]
pub(crate) fn live_pgid_of(pid: i32) -> Option<i32> {
    if pid <= 0 {
        return None;
    }
    // SAFETY: `getpgid` reads kernel process-table state for `pid` and writes no memory.
    let pgid = unsafe { libc::getpgid(pid) };
    (pgid > 0).then_some(pgid)
}

/// Non-Unix stub returning `None`.
#[cfg(not(unix))]
pub(crate) fn live_pgid_of(_pid: i32) -> Option<i32> {
    None
}

/// Observed consent or permission failure event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttentionEvent {
    /// aterm's own file access returned `EPERM` under a protected root.
    ProtectedEperm { session: Option<u64> },
}

/// Host actions decided by [`AttentionGate::note`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AttentionOutcome {
    /// Session whose tab attention mark should be raised, if any.
    pub(crate) raise_tab: Option<u64>,
    /// Whether the status-bar menu glance needs refreshing.
    pub(crate) refresh_status_item: bool,
}

impl AttentionOutcome {
    /// Filters this outcome according to `mode`.
    pub(crate) fn for_mode(self, mode: ObserverMode) -> Self {
        if mode == ObserverMode::Full {
            self
        } else {
            Self {
                raise_tab: None,
                refresh_status_item: self.refresh_status_item,
            }
        }
    }
}

/// Per-instance first-refusal latch and session tracker for `EPERM` attention events.
#[derive(Debug, Default)]
pub(crate) struct AttentionGate {
    /// Whether a refusal has been seen: only the first refreshes the menu-bar glance, so a storm
    /// of refusals rebuilds nothing.
    first_refusal_seen: bool,
    /// Live sessions whose own protected read was refused. This contribution
    /// ends on a verified grant or session retirement, not on a dismissal.
    denied_sessions: Vec<DeniedSession>,
    /// A refusal not attributed to a session still needs the same grant.
    unattributed_at: Option<Instant>,
}

#[derive(Debug)]
struct DeniedSession {
    id: u64,
    at: Instant,
    marked: bool,
}

impl AttentionGate {
    /// Creates a fresh, armed [`AttentionGate`].
    pub(crate) const fn new() -> Self {
        Self {
            first_refusal_seen: false,
            denied_sessions: Vec::new(),
            unattributed_at: None,
        }
    }

    pub(crate) fn mark_session(&mut self, session: u64, now: Instant) -> bool {
        if let Some(denied) = self
            .denied_sessions
            .iter_mut()
            .find(|denied| denied.id == session)
        {
            denied.at = now;
            return !std::mem::replace(&mut denied.marked, true);
        }
        self.denied_sessions.push(DeniedSession {
            id: session,
            at: now,
            marked: true,
        });
        true
    }

    pub(crate) fn marks_session(&self, session: u64) -> bool {
        self.denied_sessions
            .iter()
            .any(|denied| denied.id == session && denied.marked)
    }

    pub(crate) const fn has_denials(&self) -> bool {
        !self.denied_sessions.is_empty() || self.unattributed_at.is_some()
    }

    pub(crate) fn recover(&mut self, observed: Instant) -> bool {
        let before = self.denied_sessions.len();
        self.denied_sessions.retain(|denied| denied.at > observed);
        let unattributed_ended = self.unattributed_at.is_some_and(|at| at <= observed);
        if unattributed_ended {
            self.unattributed_at = None;
        }
        self.denied_sessions.len() != before || unattributed_ended
    }

    pub(crate) fn retire_session(&mut self, session: u64) {
        self.denied_sessions.retain(|denied| denied.id != session);
    }

    /// Whether this instance has seen a refusal.
    #[cfg(all(test, unix))]
    pub(crate) const fn first_refusal_seen(&self) -> bool {
        self.first_refusal_seen
    }

    /// Records `event` and returns the resulting [`AttentionOutcome`].
    pub(crate) fn note(&mut self, event: AttentionEvent) -> AttentionOutcome {
        self.note_at(event, Instant::now())
    }

    fn note_at(&mut self, event: AttentionEvent, now: Instant) -> AttentionOutcome {
        match event {
            AttentionEvent::ProtectedEperm { session } => self.note_eperm(session, now),
        }
    }

    fn note_eperm(&mut self, session: Option<u64>, now: Instant) -> AttentionOutcome {
        if let Some(session) = session {
            if let Some(denied) = self
                .denied_sessions
                .iter_mut()
                .find(|denied| denied.id == session)
            {
                denied.at = now;
            } else {
                self.denied_sessions.push(DeniedSession {
                    id: session,
                    at: now,
                    marked: false,
                });
            }
        } else {
            self.unattributed_at = Some(now);
        }
        AttentionOutcome {
            raise_tab: session,
            refresh_status_item: !std::mem::replace(&mut self.first_refusal_seen, true),
        }
    }
}

impl crate::App {
    /// Returns the `observer log=` token for the `privacy` control verb.
    pub(crate) const fn privacy_observer_log(&self) -> &'static str {
        self.consent_observer.availability().as_str()
    }
}

#[cfg(test)]
impl ObserverState {
    /// Folds a single [`ObserverEvent`] directly for tests.
    pub(crate) fn feed_for_test(&mut self, event: ObserverEvent, now: Instant) {
        self.apply(event, now);
        self.settle(now);
    }

    /// Overrides the streaming worker function for tests.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn set_arm_for_test(&mut self, run: fn(&StreamSink, ObserverMode)) {
        self.probe = ObserverProbe {
            run,
            live: false,
            ..ObserverProbe::inert()
        };
    }
}

#[cfg(test)]
impl StreamSink {
    /// Posts `event` through the sink for tests, returning whether the receiver is still open.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn post_for_test(&self, event: ObserverEvent) -> bool {
        let held = !matches!(self.tx.try_send(event), Err(TrySendError::Disconnected(_)));
        self.poke_once();
        held
    }
}

#[cfg(test)]
#[path = "consent_observer_tests.rs"]
mod tests;
