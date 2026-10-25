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
    /// `observer = true`: full `tccd` stream with tab mark and notification banner.
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
    field_after(rest, "pid=")?.parse::<i32>().ok()
}

/// `yes`/`true`/`1` are true; anything else is false. Case-insensitive.
#[cfg(any(target_os = "macos", test))]
fn parse_yes_no(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "yes" | "true" | "1"
    )
}

/// Formats the `blocked_on=` token for `service` by stripping `kTCCService`.
pub(crate) fn blocked_on_token(service: &str) -> String {
    let short = service
        .trim()
        .strip_prefix("kTCCService")
        .unwrap_or(service.trim());
    if short.is_empty() {
        return "macos-permission:unknown".to_owned();
    }
    format!("macos-permission:{short}")
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
    /// TCC service name.
    pub(crate) service: &'a str,
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
    fn signature(&self) -> Option<(i32, PidSource, &str)> {
        if self.preflight == Some(true) {
            return None;
        }
        let (pid, source) = match (self.accessing_pid, self.requesting_pid) {
            (Some(pid), _) => (pid, PidSource::Accessing),
            (None, Some(pid)) => (pid, PidSource::Requesting),
            (None, None) => return None,
        };
        Some((pid, source, self.service.as_deref()?))
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
                let (accessing_pid, pid_source, service) = entry.signature()?;
                Some(PendingPrompt {
                    msg_id: &entry.msg_id,
                    service,
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
/// child here, on the worker that spawned it: this module's one spawn and one `wait()`.
#[cfg(target_os = "macos")]
fn run_log_tool(
    args: &[String],
    slot: &Arc<ChildSlot>,
    kill_after: Option<Duration>,
    on_spawn: impl FnOnce(),
    mut on_line: impl FnMut(&str),
) -> Result<(), UnavailableReason> {
    use aterm_uds::spawnfd::{ChildStdio, LaneCommand};
    use std::io::BufRead;

    let Ok((stdout, stdout_end)) = std::io::pipe() else {
        return Err(UnavailableReason::LogToolUnavailable);
    };
    let mut cmd = LaneCommand::new(LOG_TOOL);
    cmd.args(args)
        .stdin(ChildStdio::Null)
        .stdout(ChildStdio::Fd(stdout_end.into()))
        .stderr(ChildStdio::Null);
    let Ok(mut child) = aterm_uds::spawnfd::spawn(cmd) else {
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
    pending: Option<Arc<()>>,
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
        for prompt in &mut self.announced {
            self.attention_dirty |= prompt.pending.take().is_some();
        }
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
            }
            #[cfg(any(target_os = "macos", test))]
            ObserverEvent::Line(line) => {
                if let TccEvent::Result { msg_id } = &line {
                    for prompt in &mut self.announced {
                        if &prompt.id == msg_id && prompt.pending.take().is_some() {
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
            }
        }
    }

    /// Returns all currently pending prompts attributed to `sessions` without marking them announced.
    #[cfg(test)]
    pub(crate) fn verdicts(
        &self,
        now: Instant,
        sessions: &[(u64, i32)],
        pgid_of: impl Fn(i32) -> Option<i32>,
    ) -> Vec<(u64, String)> {
        if !self.availability.is_live() {
            return Vec::new();
        }
        let mut out: Vec<(u64, String)> = Vec::new();
        for prompt in self.table.pending_prompts(now, PENDING_PROMPT_THRESHOLD) {
            let pgid = pgid_of(prompt.accessing_pid);
            if let Some(session) = owning_session(prompt.accessing_pid, pgid, sessions)
                && !out.iter().any(|(existing, _)| *existing == session)
            {
                out.push((session, blocked_on_token(prompt.service)));
            }
        }
        out
    }

    /// Resolves which session in `sessions` owns `pid` using this instance's probe arm.
    pub(crate) fn session_for_pid(&self, pid: i32, sessions: &[(u64, i32)]) -> Option<u64> {
        session_of_pid(pid, sessions, self.probe.parent_of, self.probe.pgid_of)
    }

    /// Whether `session` currently has an announced pending prompt.
    pub(crate) fn marks_session(&self, session: u64) -> bool {
        self.announced
            .iter()
            .any(|p| p.session == session && p.pending.is_some())
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
    ) -> Vec<ObservedPrompt> {
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

    /// Returns newly observed pending prompts attributed to `sessions`, marking them announced.
    pub(crate) fn take_new_verdicts(
        &mut self,
        now: Instant,
        roster: u64,
        sessions: impl FnOnce() -> Vec<(u64, i32)>,
        pgid_of: impl Fn(i32) -> Option<i32>,
    ) -> Vec<ObservedPrompt> {
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
        let mut fresh: Vec<ObservedPrompt> = Vec::new();
        for prompt in open {
            let pgid = pgid_of(prompt.accessing_pid);
            let Some(session) = owning_session(prompt.accessing_pid, pgid, &sessions) else {
                continue;
            };
            if self.announced.len() >= ANNOUNCED_CAP {
                let Some(done) = self.announced.iter().position(|p| p.pending.is_none()) else {
                    continue;
                };
                self.announced.remove(done);
            }
            let owner = Arc::new(());
            let validity = crate::notify::NoticeValidity::new(&owner);
            self.announced.push_back(AnnouncedPrompt {
                id: prompt.msg_id.to_owned(),
                session,
                pending: Some(owner),
            });
            self.attention_dirty = true;
            fresh.push(ObservedPrompt {
                session,
                blocked_on: blocked_on_token(prompt.service),
                service: prompt.service.to_owned(),
                validity,
            });
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
        Some(Folder::AppData) => Some("other applications' data"),
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

/// Newly observed pending prompt attributed to a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedPrompt {
    /// Session ID whose process is parked.
    pub(crate) session: u64,
    /// Rendered `blocked_on=` token.
    pub(crate) blocked_on: String,
    /// Raw TCC service name.
    pub(crate) service: String,
    /// Permission borrowed from the still-pending announced request.
    pub(crate) validity: crate::notify::NoticeValidity,
}

impl ObservedPrompt {
    /// Builds the [`AttentionNotice`] for this prompt.
    pub(crate) fn notice(&self) -> AttentionNotice {
        let asked = service_words(short_service(&self.service)).map_or_else(
            || "macOS is asking for a permission.".to_string(),
            |what| format!("macOS is asking for access to {what}."),
        );
        AttentionNotice {
            session: self.session,
            title: PROMPT_TITLE,
            body: format!("{asked} The program that asked waits until you answer."),
            validity: self.validity.clone(),
            decided: (),
        }
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
    let signal = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `kill` with a positive PID delivers a signal to our unreaped child process without memory effects.
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

/// Native notification payload produced by an [`AttentionEvent`] or [`ObservedPrompt`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttentionNotice {
    /// Originating session ID (`u64::MAX` when instance-wide).
    pub(crate) session: u64,
    /// Notification title.
    pub(crate) title: &'static str,
    /// Notification body.
    pub(crate) body: String,
    /// Permission borrowed from the condition that still needs an answer.
    pub(crate) validity: crate::notify::NoticeValidity,
    /// Private: only this module's gate and prompts build a notice, so
    /// [`crate::notify::NotifyMsg::consent`] posts nothing they did not decide.
    decided: (),
}

/// Host actions decided by [`AttentionGate::note`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AttentionOutcome {
    /// Session whose tab attention mark should be raised, if any.
    pub(crate) raise_tab: Option<u64>,
    /// Native notification to post, if any.
    pub(crate) notice: Option<AttentionNotice>,
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
                notice: None,
                refresh_status_item: self.refresh_status_item,
            }
        }
    }
}

const DENIED_TITLE: &str = "aterm · file access denied";

const PROMPT_TITLE: &str = "aterm · permission waiting";

/// Per-instance rate limiter and session tracker for `EPERM` attention events.
#[derive(Debug, Default)]
pub(crate) struct AttentionGate {
    /// Whether the one notification has been spent.
    spent: bool,
    /// Live sessions whose own protected read was refused. This contribution
    /// ends on a verified grant or session retirement, not on a dismissal.
    denied_sessions: Vec<DeniedSession>,
    /// A refusal not attributed to a session still needs the same grant.
    unattributed_at: Option<Instant>,
    /// Only the gate owns the one notice's lifetime; queued copies borrow it.
    notice_owner: Option<Arc<()>>,
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
            spent: false,
            denied_sessions: Vec::new(),
            unattributed_at: None,
            notice_owner: None,
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
        self.revoke_ended_notice();
        self.denied_sessions.len() != before || unattributed_ended
    }

    pub(crate) fn retire_session(&mut self, session: u64) {
        self.denied_sessions.retain(|denied| denied.id != session);
        self.revoke_ended_notice();
    }

    fn revoke_ended_notice(&mut self) {
        if !self.has_denials() {
            self.notice_owner = None;
        }
    }

    /// Whether the per-instance notification has already been sent.
    #[cfg(all(test, unix))]
    pub(crate) const fn is_spent(&self) -> bool {
        self.spent
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
        if self.spent {
            return AttentionOutcome {
                raise_tab: session,
                notice: None,
                refresh_status_item: false,
            };
        }
        self.spent = true;
        let owner = Arc::new(());
        let validity = crate::notify::NoticeValidity::new(&owner);
        self.notice_owner = Some(owner);
        AttentionOutcome {
            raise_tab: session,
            notice: Some(AttentionNotice {
                session: session.unwrap_or(u64::MAX),
                title: DENIED_TITLE,
                body: format!(
                    "Turn on aterm in {} (+ adds a missing app).",
                    crate::menu::privacy_settings_path_words(
                        crate::menu::PrivacyPane::FullDiskAccess
                    )
                ),
                validity,
                decided: (),
            }),
            refresh_status_item: true,
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
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    const REC_ATTRIBUTION: &str = "2026-08-31 15:58:45.494008-0700 0x8de257   Default     \
        0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: \
        msgID=378.385, attribution={responsible={TCCDProcess: identifier=com.example.term, \
        pid=4059, auid=501, euid=501, responsible_path=/Applications/Example.app/Contents/\
        MacOS/Example, binary_path=/Applications/Example.app/Contents/MacOS/Example}, \
        accessing={TCCDProcess: identifier=com.apple.mdfind, pid=18584, auid=501, euid=501, \
        binary_path=/usr/bin/mdfind}, requesting={TCCDProcess: identifier=com.apple.mds, \
        pid=378, auid=0, euid=0, binary_path=/System/Library/Frameworks/CoreServices.\
        framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Support/mds}, },";

    const REC_ATTRIBUTION_SELF: &str = "2026-08-31 15:30:50.039 Df tccd[66444:8d06f0] \
        [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: msgID=3956.1, \
        attribution={requesting={TCCDProcess: identifier=com.apple.PerfPowerServices, \
        pid=3956, auid=0, euid=0, binary_path=/usr/libexec/PerfPowerServices}, },";

    const REC_SUBJECT: &str = "2026-08-31 15:30:50.049 Df tccd[66444:8d0f0d] \
        [com.apple.TCC:access] AUTHREQ_SUBJECT: msgID=378.385, \
        subject=/usr/bin/mdfind,";

    const REC_CTX: &str = "2026-08-31 15:58:45.493995-0700 0x8de257   Default     \
        0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_CTX: \
        msgID=378.385, function=<private>, service=kTCCServiceSystemPolicyDocumentsFolder, \
        preflight=no, query=1, client_dict=(null), daemon_dict=<private>";

    const REC_CTX_PREFLIGHT: &str = "2026-08-31 15:58:45.493995-0700 0x8de257   Default     \
        0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_CTX: \
        msgID=378.385, function=<private>, service=kTCCServiceAddressBook, preflight=yes, \
        query=1, client_dict=(null), daemon_dict=<private>";

    const REC_RESULT: &str = "2026-08-31 15:41:28.194 Df tccd[66389:8d871a] \
        [com.apple.TCC:access] AUTHREQ_RESULT: msgID=378.385, authValue=2, authReason=11, \
        authVersion=1, desired_auth=0, error=(null),";

    #[test]
    fn the_recorded_log_lines_parse_into_their_events() {
        let cases: &[(&str, TccEvent)] = &[
            (
                REC_ATTRIBUTION,
                TccEvent::Attribution {
                    msg_id: "378.385".to_owned(),
                    accessing_pid: Some(18584),
                    requesting_pid: Some(378),
                },
            ),
            (
                REC_ATTRIBUTION_SELF,
                TccEvent::Attribution {
                    msg_id: "3956.1".to_owned(),
                    accessing_pid: None,
                    requesting_pid: Some(3956),
                },
            ),
            (
                REC_SUBJECT,
                TccEvent::Subject {
                    msg_id: "378.385".to_owned(),
                    subject: "/usr/bin/mdfind".to_owned(),
                },
            ),
            (
                REC_CTX,
                TccEvent::Ctx {
                    msg_id: "378.385".to_owned(),
                    service: "kTCCServiceSystemPolicyDocumentsFolder".to_owned(),
                    preflight: Some(false),
                },
            ),
            (
                REC_CTX_PREFLIGHT,
                TccEvent::Ctx {
                    msg_id: "378.385".to_owned(),
                    service: "kTCCServiceAddressBook".to_owned(),
                    preflight: Some(true),
                },
            ),
            (
                REC_RESULT,
                TccEvent::Result {
                    msg_id: "378.385".to_owned(),
                },
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(
                parse_line(line).as_ref(),
                Some(expected),
                "recorded line must parse: {line}"
            );
        }
    }

    #[test]
    fn a_non_authreq_line_is_not_a_format_change() {
        for line in [
            "",
            "2026-08-31 10:22:31 tccd: [com.apple.TCC:access] Received request",
            "some unrelated process line",
        ] {
            assert_eq!(parse_line(line), None, "not an AUTHREQ line: {line}");
        }
    }

    #[test]
    fn a_recognized_kind_with_missing_fields_is_unparsed_never_guessed() {
        let mangled = [
            "tccd: AUTHREQ_ATTRIBUTION: msgID=475.2921, attribution={accessing={}}",
            "tccd: AUTHREQ_ATTRIBUTION: attribution={accessing={pid=17}}",
            "tccd: AUTHREQ_SUBJECT: msgID=475.2921,",
            "tccd: AUTHREQ_CTX: msgID=475.2921, preflight=no",
            "tccd: AUTHREQ_RESULT: authValue=2",
        ];
        for line in mangled {
            assert_eq!(
                parse_line(line),
                Some(TccEvent::Unparsed),
                "a mangled recognized kind must be Unparsed: {line}"
            );
        }
    }

    #[test]
    fn the_preflight_bit_is_read_on_its_recorded_spellings() {
        let yes = "tccd: AUTHREQ_CTX: msgID=1.1, service=kTCCServiceX, preflight=yes";
        assert_eq!(
            parse_line(yes),
            Some(TccEvent::Ctx {
                msg_id: "1.1".to_owned(),
                service: "kTCCServiceX".to_owned(),
                preflight: Some(true),
            })
        );
        let absent = "tccd: AUTHREQ_CTX: msgID=1.1, service=kTCCServiceX";
        assert_eq!(
            parse_line(absent),
            Some(TccEvent::Ctx {
                msg_id: "1.1".to_owned(),
                service: "kTCCServiceX".to_owned(),
                preflight: None,
            }),
            "an absent field is None, never a guessed false"
        );
    }

    fn feed(table: &mut PendingTable, line: &str, now: Instant) {
        let event = parse_line(line).expect("recorded line parses");
        table.note(event, now);
    }

    #[test]
    fn an_unanswered_ctx_with_an_accessing_pid_is_a_pending_prompt() {
        let t0 = Instant::now();
        let mut table = PendingTable::new();
        feed(&mut table, REC_CTX, t0);
        feed(&mut table, REC_ATTRIBUTION, t0);
        feed(&mut table, REC_SUBJECT, t0);

        assert!(
            table
                .pending_prompts(t0, PENDING_PROMPT_THRESHOLD)
                .is_empty(),
            "a request that just opened is not yet a prompt"
        );

        let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        let prompts = table.pending_prompts(later, PENDING_PROMPT_THRESHOLD);
        assert_eq!(prompts.len(), 1, "exactly one open request");
        assert_eq!(prompts[0].accessing_pid, 18584);
        assert_eq!(prompts[0].pid_source, PidSource::Accessing);
        assert_eq!(prompts[0].service, "kTCCServiceSystemPolicyDocumentsFolder");
        assert_eq!(prompts[0].subject, Some("/usr/bin/mdfind"));
    }

    #[test]
    fn a_result_closes_the_request_and_no_prompt_is_reported() {
        let t0 = Instant::now();
        let mut table = PendingTable::new();
        feed(&mut table, REC_CTX, t0);
        feed(&mut table, REC_ATTRIBUTION, t0);
        feed(&mut table, REC_RESULT, t0 + Duration::from_secs(1));
        let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(5);
        assert!(
            table
                .pending_prompts(later, PENDING_PROMPT_THRESHOLD)
                .is_empty()
        );
        assert!(table.is_empty(), "the entry is gone, not merely hidden");
    }

    #[test]
    fn a_ctx_without_an_accessing_pid_is_never_a_prompt() {
        let t0 = Instant::now();
        let mut table = PendingTable::new();
        feed(&mut table, REC_CTX, t0);
        let later = t0 + Duration::from_secs(60);
        assert!(
            table
                .pending_prompts(later, PENDING_PROMPT_THRESHOLD)
                .is_empty()
        );
        assert_eq!(table.len(), 1, "still tracked, just not a verdict");
    }

    #[test]
    fn a_preflight_request_is_never_a_pending_prompt() {
        let t0 = Instant::now();
        let mut table = PendingTable::new();
        feed(
            &mut table,
            "tccd: AUTHREQ_CTX: msgID=9.9, service=kTCCServiceX, preflight=yes",
            t0,
        );
        feed(
            &mut table,
            "tccd: AUTHREQ_ATTRIBUTION: msgID=9.9, attribution={accessing={pid=1234}}",
            t0,
        );
        let later = t0 + Duration::from_secs(60);
        assert!(
            table
                .pending_prompts(later, PENDING_PROMPT_THRESHOLD)
                .is_empty()
        );
    }

    #[test]
    fn the_table_is_bounded_and_evicts_the_oldest() {
        let t0 = Instant::now();
        let mut table = PendingTable::new();
        for i in 0..(PENDING_CAP + 32) {
            feed(
                &mut table,
                &format!("tccd: AUTHREQ_CTX: msgID=1.{i}, service=kTCCServiceX, preflight=no"),
                t0,
            );
        }
        assert_eq!(table.len(), PENDING_CAP, "capped, never grown");
    }

    #[test]
    fn an_old_entry_expires_rather_than_standing_as_evidence() {
        let t0 = Instant::now();
        let mut table = PendingTable::new();
        feed(&mut table, REC_CTX, t0);
        feed(&mut table, REC_ATTRIBUTION, t0);
        table.expire(
            t0 + PENDING_MAX_AGE - Duration::from_secs(1),
            PENDING_MAX_AGE,
        );
        assert_eq!(table.len(), 1, "not yet old enough");
        table.expire(
            t0 + PENDING_MAX_AGE + Duration::from_secs(1),
            PENDING_MAX_AGE,
        );
        assert!(table.is_empty(), "expired");
    }

    #[test]
    fn owning_session_matches_the_pid_or_its_process_group_and_nothing_else() {
        let sessions = &[(7u64, 4711i32), (9, 4802)];
        let cases: &[(i32, Option<i32>, Option<u64>, &str)] = &[
            (4711, None, Some(7), "the shell pid itself"),
            (51234, Some(4802), Some(9), "a child in a session's group"),
            (51234, Some(9999), None, "a group we do not own"),
            (51234, None, None, "no group, no match"),
            (
                0,
                Some(4711),
                None,
                "a nonsense pid is refused before the join",
            ),
            (-1, Some(4711), None, "a negative pid is refused too"),
            (
                4802,
                Some(1),
                Some(9),
                "the pid wins over an unrelated group",
            ),
        ];
        for (pid, pgid, expected, why) in cases {
            assert_eq!(owning_session(*pid, *pgid, sessions), *expected, "{why}");
        }
    }

    #[test]
    fn the_blocked_on_token_is_the_short_service_name() {
        let cases: &[(&str, &str)] = &[
            (
                "kTCCServiceSystemPolicyDocumentsFolder",
                "macos-permission:SystemPolicyDocumentsFolder",
            ),
            (
                "SystemPolicyAllFiles",
                "macos-permission:SystemPolicyAllFiles",
            ),
            ("  kTCCServiceCamera ", "macos-permission:Camera"),
            ("kTCCService", "macos-permission:unknown"),
            ("", "macos-permission:unknown"),
        ];
        for (service, expected) in cases {
            assert_eq!(blocked_on_token(service), *expected);
        }
    }

    #[test]
    fn unavailable_off_and_ok_are_three_distinct_values() {
        assert_eq!(ObserverAvailability::Off.as_str(), "off");
        assert_eq!(ObserverAvailability::Ok.as_str(), "ok");
        for reason in [
            UnavailableReason::Inert,
            UnavailableReason::UnsupportedPlatform,
            UnavailableReason::NoLogAccess,
            UnavailableReason::LogToolUnavailable,
            UnavailableReason::StreamEnded,
            UnavailableReason::FormatUnrecognized,
        ] {
            let value = ObserverAvailability::Unavailable(reason);
            assert_eq!(value.as_str(), "unavailable");
            assert_ne!(value.as_str(), ObserverAvailability::Off.as_str());
            assert_ne!(value.as_str(), ObserverAvailability::Ok.as_str());
            assert_eq!(value.reason(), Some(reason));
            assert!(!reason.as_str().is_empty());
        }
        assert_eq!(ObserverAvailability::Off.reason(), None);
        assert_eq!(ObserverAvailability::Ok.reason(), None);
    }

    #[test]
    fn a_fresh_observer_is_off_and_inert() {
        let observer = ObserverState::default();
        assert_eq!(observer.availability(), ObserverAvailability::Off);
        assert!(!observer.probe_is_live());
        assert!(!observer.is_live());
        assert_eq!(observer.pending_len(), 0);
    }

    #[test]
    fn a_disabled_observer_starts_nothing() {
        let pokes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&pokes);
        let mut observer = ObserverState::inert();
        assert_eq!(
            observer.start(ObserverMode::Off, move || {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
            StartOutcome::Disabled
        );
        assert_eq!(observer.availability(), ObserverAvailability::Off);
        assert!(!observer.is_live());
        assert_eq!(pokes.load(Ordering::Relaxed), 0, "nothing was posted");
    }

    #[test]
    fn the_inert_arm_degrades_to_unavailable_and_never_to_a_verdict() {
        let mut observer = ObserverState::inert();
        assert_eq!(
            observer.start(ObserverMode::Full, || {}),
            StartOutcome::Started
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        while observer.availability() == ObserverAvailability::Off && Instant::now() < deadline {
            observer.drain(Instant::now());
            std::thread::yield_now();
        }
        assert_eq!(
            observer.availability(),
            ObserverAvailability::Unavailable(UnavailableReason::Inert)
        );
        assert!(
            observer
                .verdicts(Instant::now(), &[(1, 100)], |_| None)
                .is_empty(),
            "an unavailable observer publishes nothing"
        );
    }

    #[test]
    fn a_second_start_while_live_is_a_no_op() {
        let mut observer = ObserverState::inert();
        assert_eq!(
            observer.start(ObserverMode::Full, || {}),
            StartOutcome::Started
        );
        assert_eq!(
            observer.start(ObserverMode::Full, || {}),
            StartOutcome::AlreadyLive
        );
        observer.stop();
        assert!(!observer.is_live());
    }

    #[test]
    fn reconcile_restarts_only_on_a_changed_mode() {
        let mut observer = ObserverState::inert();
        assert_eq!(
            observer.reconcile(ObserverMode::Off, || {}),
            None,
            "a fresh observer already has what Off asks for"
        );
        assert_eq!(
            observer.reconcile(ObserverMode::Full, || {}),
            Some(StartOutcome::Started)
        );
        assert_eq!(observer.requested(), ObserverMode::Full);
        let deadline = Instant::now() + Duration::from_secs(60);
        while observer.is_live() && Instant::now() < deadline {
            observer.drain(Instant::now());
            std::thread::yield_now();
        }
        assert!(!observer.is_live(), "the inert worker ended by itself");
        assert_eq!(
            observer.availability(),
            ObserverAvailability::Unavailable(UnavailableReason::Inert)
        );
        assert_eq!(
            observer.reconcile(ObserverMode::Full, || {}),
            None,
            "an ended stream is not respawned by the same wish"
        );
        assert!(!observer.is_live());
        assert_eq!(
            observer.reconcile(ObserverMode::Sentinel, || {}),
            Some(StartOutcome::Started),
            "a changed wish starts the new mode"
        );
        assert!(observer.is_live());
        assert_eq!(
            observer.reconcile(ObserverMode::Off, || {}),
            Some(StartOutcome::Disabled)
        );
        assert!(!observer.is_live());
        assert_eq!(observer.requested(), ObserverMode::Off);
        assert_eq!(observer.availability(), ObserverAvailability::Off);
    }

    #[test]
    fn the_running_mode_is_set_by_started_and_cleared_by_an_end_or_a_stop() {
        let now = Instant::now();
        let mut observer = ObserverState::inert();
        assert_eq!(
            observer.start(ObserverMode::Full, || {}),
            StartOutcome::Started
        );
        assert_eq!(
            observer.running(),
            ObserverMode::Off,
            "nothing runs until the worker says so"
        );
        observer.feed_for_test(ObserverEvent::Started, now);
        assert_eq!(observer.running(), ObserverMode::Full);
        let deadline = Instant::now() + Duration::from_secs(60);
        while observer.is_live() && Instant::now() < deadline {
            observer.drain(Instant::now());
            std::thread::yield_now();
        }
        assert!(!observer.is_live());
        assert_eq!(observer.running(), ObserverMode::Off, "the end clears it");
        assert_eq!(
            observer.requested(),
            ObserverMode::Full,
            "an end by itself keeps the request"
        );

        assert_eq!(
            observer.start(ObserverMode::Sentinel, || {}),
            StartOutcome::Started
        );
        observer.feed_for_test(ObserverEvent::Started, now);
        assert_eq!(observer.running(), ObserverMode::Sentinel);
        observer.stop();
        assert_eq!(observer.running(), ObserverMode::Off, "a stop clears it");
        assert_eq!(observer.requested(), ObserverMode::Off);
    }

    #[test]
    fn a_quiet_stream_publishes_nothing_and_stays_ok() {
        let now = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, now);
        assert_eq!(observer.availability(), ObserverAvailability::Ok);
        assert!(observer.verdicts(now, &[(1, 4711)], |_| None).is_empty());
        assert_eq!(observer.availability(), ObserverAvailability::Ok);
    }

    #[test]
    fn an_observed_pending_prompt_publishes_blocked_on_for_its_session() {
        let t0 = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, t0);
        for line in [REC_CTX, REC_ATTRIBUTION, REC_SUBJECT] {
            let event = parse_line(line).expect("recorded line parses");
            observer.feed_for_test(ObserverEvent::Line(event), t0);
        }
        let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        let verdicts = observer.verdicts(later, &[(4, 4711)], |pid| (pid == 18584).then_some(4711));
        assert_eq!(
            verdicts,
            vec![(
                4u64,
                "macos-permission:SystemPolicyDocumentsFolder".to_owned()
            )]
        );
        assert!(
            observer.verdicts(later, &[(4, 9999)], |_| None).is_empty(),
            "a prompt we cannot attribute is nobody's prompt"
        );
    }

    #[test]
    fn the_instance_owns_its_process_attribution_probes() {
        fn parent(pid: i32) -> Option<i32> {
            match pid {
                18585 => Some(4711),
                _ => None,
            }
        }
        fn group(pid: i32) -> Option<i32> {
            match pid {
                18584 => Some(4711),
                _ => None,
            }
        }
        let now = Instant::now();
        let later = now + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        let sessions = &[(4_u64, 4711_i32)];
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, now);
        for line in [REC_CTX, REC_ATTRIBUTION, REC_SUBJECT] {
            observer.feed_for_test(
                ObserverEvent::Line(parse_line(line).expect("fixture line")),
                now,
            );
        }
        assert_eq!(observer.session_for_pid(18584, sessions), None);
        assert_eq!(observer.session_for_pid(18585, sessions), None);
        assert!(
            observer
                .take_new_verdicts_for_instance(later, 1, || sessions.to_vec())
                .is_empty()
        );
        observer.probe.parent_of = parent;
        observer.probe.pgid_of = group;
        assert!(!observer.probe_is_live(), "the stream stays inert");
        assert_eq!(observer.session_for_pid(18584, sessions), Some(4), "group");
        assert_eq!(
            observer.session_for_pid(18585, sessions),
            Some(4),
            "ancestor"
        );
        assert_eq!(observer.session_for_pid(9999, sessions), None, "unrelated");
        let fresh = observer.take_new_verdicts_for_instance(later, 2, || sessions.to_vec());
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].session, 4);
    }

    #[test]
    fn one_observed_prompt_pages_once_and_a_second_dialog_pages_again() {
        let t0 = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, t0);
        for line in [REC_CTX, REC_ATTRIBUTION, REC_SUBJECT] {
            let event = parse_line(line).expect("recorded line parses");
            observer.feed_for_test(ObserverEvent::Line(event), t0);
        }
        let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        let sessions = || vec![(4u64, 4711i32)];
        let pgid = |pid: i32| (pid == 18584).then_some(4711);

        let first = observer.take_new_verdicts(later, 1, sessions, pgid);
        assert_eq!(first.len(), 1, "the observed dialog pages");
        assert_eq!(first[0].session, 4);
        assert_eq!(
            first[0].blocked_on,
            "macos-permission:SystemPolicyDocumentsFolder"
        );
        assert_eq!(first[0].notice().session, 4);
        let unknown_owner = Arc::new(());
        let unknown = ObservedPrompt {
            session: 4,
            blocked_on: blocked_on_token("kTCCServiceSomethingNew"),
            service: "kTCCServiceSomethingNew".to_owned(),
            validity: crate::notify::NoticeValidity::new(&unknown_owner),
        };
        assert!(!unknown.notice().body.contains("kTCCService"));

        assert!(
            observer
                .take_new_verdicts(later, 2, sessions, pgid)
                .is_empty(),
            "the same dialog never pages twice"
        );
        assert_eq!(observer.verdicts(later, &sessions(), pgid).len(), 1);

        for line in [
            "tccd: AUTHREQ_CTX: msgID=378.999, service=kTCCServiceSystemPolicyDesktopFolder, \
             preflight=no",
            "tccd: AUTHREQ_ATTRIBUTION: msgID=378.999, attribution={accessing={TCCDProcess: \
             identifier=x, pid=18584}}",
        ] {
            let event = parse_line(line).expect("line parses");
            observer.feed_for_test(ObserverEvent::Line(event), later);
        }
        let later2 = later + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        let second = observer.take_new_verdicts(later2, 2, sessions, pgid);
        assert_eq!(second.len(), 1, "a distinct dialog is distinct news");
        assert_eq!(
            second[0].blocked_on,
            "macos-permission:SystemPolicyDesktopFolder"
        );
    }

    #[test]
    fn an_unattributable_prompt_is_never_marked_announced() {
        let t0 = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, t0);
        for line in [REC_CTX, REC_ATTRIBUTION] {
            let event = parse_line(line).expect("recorded line parses");
            observer.feed_for_test(ObserverEvent::Line(event), t0);
        }
        let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        assert!(
            observer
                .take_new_verdicts(later, 1, || vec![(4, 9999)], |_| None)
                .is_empty(),
            "not ours"
        );
        let ours = observer.take_new_verdicts(
            later,
            2,
            || vec![(4, 4711)],
            |pid| (pid == 18584).then_some(4711),
        );
        assert_eq!(ours.len(), 1, "and once it IS ours, it pages");
    }

    #[test]
    fn an_unattributable_prompt_is_judged_once_per_roster() {
        let t0 = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, t0);
        for line in [REC_CTX, REC_ATTRIBUTION] {
            let event = parse_line(line).expect("recorded line parses");
            observer.feed_for_test(ObserverEvent::Line(event), t0);
        }
        let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        let asked = std::cell::Cell::new(0usize);
        let pgid = |_pid: i32| {
            asked.set(asked.get() + 1);
            None
        };
        let roster = || vec![(4u64, 9999i32)];
        for park in 0..3 {
            assert!(
                observer
                    .take_new_verdicts(later + Duration::from_secs(park), 1, roster, pgid)
                    .is_empty()
            );
        }
        assert_eq!(asked.get(), 1, "three parks over one roster judge once");
        assert!(
            observer
                .take_new_verdicts(later, 2, roster, pgid)
                .is_empty()
        );
        assert_eq!(asked.get(), 2, "a moved roster judges it again");
        let event = parse_line(
            "tccd: AUTHREQ_ATTRIBUTION: msgID=378.385, attribution={requesting={TCCDProcess: \
             identifier=x, pid=4242}}",
        )
        .expect("line parses");
        observer.feed_for_test(ObserverEvent::Line(event), later);
        assert!(
            observer
                .take_new_verdicts(later, 2, roster, pgid)
                .is_empty()
        );
        assert_eq!(asked.get(), 3, "a folded line judges it again");
    }

    #[test]
    fn the_threshold_crossing_is_judged_at_its_deadline() {
        let opened_before = Instant::now();
        let t0 = opened_before + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, opened_before);
        for line in [
            "tccd: AUTHREQ_CTX: msgID=378.400, service=kTCCServiceSystemPolicyDesktopFolder, \
             preflight=no",
            "tccd: AUTHREQ_ATTRIBUTION: msgID=378.400, attribution={accessing={TCCDProcess: \
             identifier=x, pid=5150}}",
        ] {
            let event = parse_line(line).expect("line parses");
            observer.feed_for_test(ObserverEvent::Line(event), opened_before);
        }
        for line in [REC_CTX, REC_ATTRIBUTION] {
            let event = parse_line(line).expect("recorded line parses");
            observer.feed_for_test(ObserverEvent::Line(event), t0);
        }
        let asked = std::cell::Cell::new(0usize);
        let pgid = |pid: i32| {
            asked.set(asked.get() + 1);
            (pid == 18584).then_some(4711)
        };
        let sessions = || vec![(4u64, 4711i32)];
        assert!(observer.take_new_verdicts(t0, 1, sessions, pgid).is_empty());
        assert_eq!(asked.get(), 1, "the pass judges the request nobody owns");
        let crossing = t0 + PENDING_PROMPT_THRESHOLD;
        assert_eq!(observer.verdict_deadline(), Some(crossing));
        assert!(
            observer
                .take_new_verdicts(crossing - Duration::from_millis(1), 1, sessions, pgid)
                .is_empty(),
            "nothing is judged before the crossing"
        );
        assert_eq!(asked.get(), 1, "and the OS is not asked");
        let paged = observer.take_new_verdicts(crossing, 1, sessions, pgid);
        assert_eq!(asked.get(), 3, "the crossing judges both requests");
        assert_eq!(paged.len(), 1, "the crossing pages on its own");
        assert_eq!(paged[0].session, 4);
        assert_eq!(observer.verdict_deadline(), None, "nothing left to cross");
    }

    #[test]
    fn pokes_coalesce_until_the_queue_is_drained() {
        let pokes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&pokes);
        let mut observer = ObserverState::inert();
        let (sink, rx) = observer.channel(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        observer.rx = Some(rx);
        for _ in 0..5 {
            sink.post(ObserverEvent::Started);
        }
        assert_eq!(pokes.load(Ordering::Relaxed), 1, "a burst is one wake");
        assert_eq!(observer.drain(Instant::now()), 5, "every message is kept");
        sink.post(ObserverEvent::Started);
        assert_eq!(
            pokes.load(Ordering::Relaxed),
            2,
            "a post after the drain wakes again"
        );
        assert_eq!(observer.drain(Instant::now()), 1);
    }

    #[test]
    fn an_unavailable_observer_pages_nobody() {
        let t0 = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, t0);
        for line in [REC_CTX, REC_ATTRIBUTION] {
            let event = parse_line(line).expect("recorded line parses");
            observer.feed_for_test(ObserverEvent::Line(event), t0);
        }
        observer.feed_for_test(
            ObserverEvent::Unavailable(UnavailableReason::NoLogAccess),
            t0,
        );
        let later = t0 + PENDING_PROMPT_THRESHOLD + Duration::from_secs(1);
        assert!(
            observer
                .take_new_verdicts(
                    later,
                    1,
                    || vec![(4, 4711)],
                    |pid| (pid == 18584).then_some(4711)
                )
                .is_empty()
        );
    }

    #[test]
    fn a_format_change_degrades_to_unavailable() {
        let now = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, now);
        for _ in 0..FORMAT_DRIFT_LIMIT {
            observer.feed_for_test(ObserverEvent::Line(TccEvent::Unparsed), now);
        }
        assert_eq!(
            observer.availability(),
            ObserverAvailability::Unavailable(UnavailableReason::FormatUnrecognized)
        );
        assert!(observer.verdicts(now, &[(1, 4711)], |_| None).is_empty());
    }

    #[test]
    fn the_stream_header_lines_are_not_messages_and_never_count_as_drift() {
        let filtering = "Filtering the log data using \"process == \"tccd\" AND \
            composedMessage CONTAINS \"AUTHREQ_PROMPTING\"\"";
        let columns = "Timestamp                       Thread     Type        Activity             \
            PID    TTL  ";
        assert_eq!(parse_line(filtering), None);
        assert_eq!(parse_line(columns), None);
        assert_eq!(parse_line("x CONTAINS \"AUTHREQ_PROMPTING\"\""), None);

        let now = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, now);
        for _ in 0..(FORMAT_DRIFT_LIMIT * 100) {
            for header in [filtering, columns] {
                if let Some(event) = parse_line(header) {
                    observer.feed_for_test(ObserverEvent::Line(event), now);
                }
            }
        }
        assert_eq!(observer.availability(), ObserverAvailability::Ok);
    }

    #[test]
    fn an_understood_line_lifts_a_format_drift() {
        let now = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, now);
        for _ in 0..FORMAT_DRIFT_LIMIT {
            observer.feed_for_test(ObserverEvent::Line(TccEvent::Unparsed), now);
        }
        assert_eq!(
            observer.availability(),
            ObserverAvailability::Unavailable(UnavailableReason::FormatUnrecognized)
        );
        observer.feed_for_test(
            ObserverEvent::Line(TccEvent::Result {
                msg_id: "1.1".to_owned(),
            }),
            now,
        );
        assert_eq!(observer.availability(), ObserverAvailability::Ok);
    }

    #[test]
    fn a_dropped_worker_ends_the_stream_honestly() {
        let now = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, now);
        observer.feed_for_test(ObserverEvent::Ended, now);
        assert_eq!(
            observer.availability(),
            ObserverAvailability::Unavailable(UnavailableReason::StreamEnded)
        );
    }

    #[test]
    fn consent_marks_recover_only_denials_the_grant_observed_after() {
        let now = Instant::now();
        let mut gate = AttentionGate::new();
        assert!(gate.mark_session(1, now));
        assert!(gate.mark_session(2, now + Duration::from_secs(2)));
        assert!(gate.recover(now + Duration::from_secs(1)));
        assert!(!gate.marks_session(1));
        assert!(gate.marks_session(2));
        assert!(!gate.mark_session(2, now + Duration::from_secs(4)));
        assert!(!gate.recover(now + Duration::from_secs(3)));
        assert!(gate.marks_session(2), "the repeated refusal is newer");
        assert!(gate.recover(now + Duration::from_secs(4)));
        assert!(!gate.has_denials(), "an equal-time observation suffices");
    }

    #[test]
    fn an_eperm_storm_posts_exactly_one_notification() {
        let mut gate = AttentionGate::new();
        let first = gate.note(AttentionEvent::ProtectedEperm { session: Some(3) });
        assert_eq!(first.raise_tab, Some(3));
        assert!(first.notice.is_some(), "the first denial is news");
        assert!(first.refresh_status_item);
        for _ in 0..50 {
            let again = gate.note(AttentionEvent::ProtectedEperm { session: Some(3) });
            assert_eq!(
                again.raise_tab,
                Some(3),
                "the tab mark is per-tab and cheap"
            );
            assert!(again.notice.is_none(), "and the notification is spent");
            assert!(!again.refresh_status_item);
        }
    }

    #[test]
    fn a_sentinel_instance_posts_no_banner_and_a_full_instance_still_does() {
        for mode in [ObserverMode::Sentinel, ObserverMode::Off] {
            let mut gate = AttentionGate::new();
            let outcome = gate
                .note(AttentionEvent::ProtectedEperm { session: Some(3) })
                .for_mode(mode);
            assert_eq!(outcome.raise_tab, None, "{mode:?}: no tab mark");
            assert!(outcome.notice.is_none(), "{mode:?}: no banner");
            assert!(
                outcome.refresh_status_item,
                "{mode:?}: the glance refreshes"
            );
        }
        let mut gate = AttentionGate::new();
        let outcome = gate
            .note(AttentionEvent::ProtectedEperm { session: Some(3) })
            .for_mode(ObserverMode::Full);
        assert_eq!(outcome.raise_tab, Some(3));
        assert!(outcome.notice.is_some());
    }

    #[test]
    fn an_instance_level_eperm_marks_no_tab_but_still_notifies() {
        let mut gate = AttentionGate::new();
        let outcome = gate.note(AttentionEvent::ProtectedEperm { session: None });
        assert_eq!(outcome.raise_tab, None);
        let notice = outcome.notice.expect("instance denials still notify");
        assert_eq!(
            notice.session,
            u64::MAX,
            "an instance-level notice names no session, so focus suppresses nothing"
        );
    }

    const REC_PROMPTING_WINDOW: &str = "2026-09-28 17:28:00.090411-0700 0x20dffbe  Default     \
        0x7265e15            3501   0    tccd: [com.apple.TCC:access] AUTHREQ_PROMPTING: \
        msgID=69745.137, service=kTCCServiceSystemPolicyAppData, \
        subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, \
        pid=70374, auid=501, euid=501, \
        responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
        binary_path=/Applications/aterm.app/Contents/MacOS/aterm},";
    const REC_PROMPTING_ADOPTED: &str = "2026-09-28 17:55:12.310902-0700 0x20dffbe  Default     \
        0x7265e15            3501   0    tccd: [com.apple.TCC:access] AUTHREQ_PROMPTING: \
        msgID=69745.190, service=kTCCServiceSystemPolicyAppData, \
        subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, \
        pid=74756, auid=501, euid=501, \
        responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
        binary_path=/Users//someone/Library/Application Support/aterm/pkg/store/claude/1/bin/claude},";
    const REC_PROMPTING_ADOPTED_2: &str = "2026-09-28 18:26:50.661120-0700 0x20dffbe  Default     \
        0x7265e15            3501   0    tccd: [com.apple.TCC:access] AUTHREQ_PROMPTING: \
        msgID=69745.222, service=kTCCServiceSystemPolicyAppData, \
        subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, \
        pid=99705, auid=501, euid=501, \
        responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
        binary_path=/Users//someone/Library/Application Support/aterm/pkg/store/claude/1/bin/claude},";

    const REC_PROMPTING_OTHER_SUBJECT: &str = "2026-09-28 17:31:00.000000-0700 0x1  Default     \
        0x1            3501   0    tccd: [com.apple.TCC:access] AUTHREQ_PROMPTING: \
        msgID=69745.300, service=kTCCServiceSystemPolicyAppData, \
        subject=Sub:{com.example.other}Resp:{TCCDProcess: identifier=com.example.other, \
        pid=1234, auid=501, euid=501, responsible_path=/Applications/Other.app/Contents/MacOS/Other, \
        binary_path=/Applications/Other.app/Contents/MacOS/Other},";

    fn prompting(msg_id: &str, subject: &str, resp_pid: i32) -> ObserverEvent {
        ObserverEvent::Prompt(PromptNeed {
            msg_id: msg_id.to_owned(),
            service: "kTCCServiceSystemPolicyAppData".to_owned(),
            subject: subject.to_owned(),
            resp_pid,
            accessing_pid: Some(resp_pid),
        })
    }

    #[test]
    fn the_recorded_prompting_lines_parse_into_prompting_events() {
        for (line, msg_id, pid) in [
            (REC_PROMPTING_WINDOW, "69745.137", 70374),
            (REC_PROMPTING_ADOPTED, "69745.190", 74756),
            (REC_PROMPTING_ADOPTED_2, "69745.222", 99705),
        ] {
            assert_eq!(
                parse_line(line),
                Some(TccEvent::Prompting {
                    msg_id: msg_id.to_owned(),
                    service: "kTCCServiceSystemPolicyAppData".to_owned(),
                    subject: "com.aterm.aterm".to_owned(),
                    resp_pid: pid,
                }),
                "{line}"
            );
        }
        assert_eq!(
            parse_line(REC_PROMPTING_OTHER_SUBJECT),
            Some(TccEvent::Prompting {
                msg_id: "69745.300".to_owned(),
                service: "kTCCServiceSystemPolicyAppData".to_owned(),
                subject: "com.example.other".to_owned(),
                resp_pid: 1234,
            })
        );
    }

    #[test]
    fn a_prompting_line_missing_its_fields_is_unparsed_never_guessed() {
        for broken in [
            "tccd: AUTHREQ_PROMPTING: msgID=1.2, service=kTCCServiceSystemPolicyAppData, \
             subject=Sub:{com.aterm.aterm}",
            "tccd: AUTHREQ_PROMPTING: msgID=1.2, service=kTCCServiceSystemPolicyAppData, \
             subject=Sub:{}Resp:{TCCDProcess: identifier=x, pid=5},",
            "tccd: AUTHREQ_PROMPTING: msgID=1.2, subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: \
             identifier=x, pid=5},",
            "tccd: AUTHREQ_PROMPTING: service=kTCCServiceSystemPolicyAppData, \
             subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=x, pid=5},",
            "tccd: AUTHREQ_PROMPTING: msgID=1.2, service=kTCCServiceSystemPolicyAppData, \
             subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=x, pid=abc},",
        ] {
            assert_eq!(parse_line(broken), Some(TccEvent::Unparsed), "{broken}");
        }
    }

    #[test]
    fn the_sentinel_predicate_is_narrow_and_names_tccd() {
        assert!(SENTINEL_PREDICATE.starts_with("process == \"tccd\""));
        assert!(SENTINEL_PREDICATE.contains("AUTHREQ_PROMPTING"));
        assert!(
            !SENTINEL_PREDICATE.contains("AUTHREQ_CTX")
                && !SENTINEL_PREDICATE.contains("AUTHREQ_ATTRIBUTION"),
            "the sentinel reads one line, not the correlation stream"
        );
        let sentinel = stream_args(ObserverMode::Sentinel);
        assert_eq!(sentinel[0], "stream");
        let at = sentinel.iter().position(|a| a == "--predicate").unwrap();
        assert_eq!(sentinel[at + 1], SENTINEL_PREDICATE);
        let full = stream_args(ObserverMode::Full);
        let at = full.iter().position(|a| a == "--predicate").unwrap();
        assert_eq!(full[at + 1], "process == \"tccd\"");
        assert!(
            !full.iter().any(|a| a == "--timeout"),
            "full mode is unbounded"
        );
    }

    #[test]
    fn the_sentinel_stream_is_bounded_by_a_timeout_and_rotated_on_eof() {
        let args = stream_args(ObserverMode::Sentinel);
        let at = args.iter().position(|a| a == "--timeout").unwrap();
        assert_eq!(args[at + 1], SENTINEL_TIMEOUT);
        assert_eq!(SENTINEL_TIMEOUT, "15m");
        assert!(ROTATION_MIN < Duration::from_secs(15 * 60));
        assert!(ROTATION_MIN >= Duration::from_secs(14 * 60));
        assert!(LOOKUP_TIMEOUT <= Duration::from_secs(5));
        assert!(lookup_args("69745.190").is_some());
        for hostile in ["", "1.2\" OR 1", "1.2,", "abc", "1.2 3"] {
            assert!(lookup_args(hostile).is_none(), "{hostile:?}");
        }
        let lookup = lookup_args("69745.190").unwrap();
        assert_eq!(lookup[0], "show");
        assert!(
            lookup
                .iter()
                .any(|a| a.contains("AUTHREQ_ATTRIBUTION: msgID=69745.190,"))
        );
    }

    #[test]
    fn an_end_at_the_timeout_is_a_rotation_not_an_unavailable_observer() {
        let mut policy = RotationPolicy::new();
        let t0 = Instant::now();
        for i in 0..20u64 {
            assert_eq!(
                policy.on_end(
                    ObserverMode::Sentinel,
                    Duration::from_secs(15 * 60),
                    t0 + Duration::from_secs(i * 900)
                ),
                StreamEnd::Rotate
            );
        }
        assert_eq!(
            policy.on_end(ObserverMode::Sentinel, ROTATION_MIN, t0),
            StreamEnd::Rotate,
            "14 min 59 s is the timeout"
        );
        assert_eq!(
            policy.on_end(ObserverMode::Full, Duration::from_secs(20 * 60), t0),
            StreamEnd::GiveUp
        );
        let mut observer = ObserverState::inert();
        let now = Instant::now();
        observer.feed_for_test(ObserverEvent::Started, now);
        assert_eq!(observer.availability(), ObserverAvailability::Ok);
    }

    #[test]
    fn an_early_end_restarts_at_most_three_times_an_hour() {
        let mut policy = RotationPolicy::new();
        let t0 = Instant::now();
        let early = Duration::from_secs(30);
        for i in 0..3u64 {
            assert_eq!(
                policy.on_end(
                    ObserverMode::Sentinel,
                    early,
                    t0 + Duration::from_secs(i * 60)
                ),
                StreamEnd::Restart,
                "early end {i}"
            );
        }
        assert_eq!(
            policy.on_end(ObserverMode::Sentinel, early, t0 + Duration::from_secs(200)),
            StreamEnd::GiveUp,
            "a fourth early end inside the hour gives up"
        );
        assert_eq!(
            policy.on_end(
                ObserverMode::Sentinel,
                Duration::from_secs(15 * 60),
                t0 + Duration::from_secs(300)
            ),
            StreamEnd::Rotate
        );
        assert_eq!(
            policy.on_end(
                ObserverMode::Sentinel,
                early,
                t0 + Duration::from_secs(60 * 60 + 61)
            ),
            StreamEnd::Restart,
            "an hour later the first early ends have aged out"
        );
    }

    #[test]
    fn a_stop_signals_the_child_only_while_it_is_unreaped() {
        let sent = std::cell::RefCell::new(Vec::new());
        let kill = |pid: i32, signal: Signal| sent.borrow_mut().push((pid, signal));

        let standing = ChildSlot::default();
        assert!(standing.publish(4101, kill), "nothing stopped it yet");
        assert!(
            sent.borrow().is_empty(),
            "a publication alone signals nothing"
        );
        standing.stop(kill);
        assert_eq!(
            *sent.borrow(),
            [(4101, Signal::Term)],
            "a standing child is asked to exit, once"
        );

        sent.borrow_mut().clear();
        let reaped = ChildSlot::default();
        assert!(reaped.publish(4102, kill));
        reaped.retire();
        reaped.stop(kill);
        assert!(sent.borrow().is_empty(), "a retired pid is never signalled");
        assert!(reaped.stopped(), "the stop still stands for the worker");

        ChildSlot::default().stop(kill);
        assert!(sent.borrow().is_empty(), "no pid, no signal");
    }

    #[test]
    fn a_stop_before_the_pid_is_published_signals_the_child_at_publication() {
        let sent = std::cell::RefCell::new(Vec::new());
        let kill = |pid: i32, signal: Signal| sent.borrow_mut().push((pid, signal));

        let slot = ChildSlot::default();
        slot.stop(kill);
        assert!(sent.borrow().is_empty());
        assert!(!slot.publish(4201, kill), "the worker reaps it unread");
        assert_eq!(
            *sent.borrow(),
            [(4201, Signal::Term)],
            "signalled exactly once, at its publication"
        );

        sent.borrow_mut().clear();
        assert!(ChildSlot::default().publish(4202, kill));
        assert!(sent.borrow().is_empty());
    }

    #[test]
    fn the_lookup_watchdog_kills_only_a_child_unreaped_at_its_limit() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let slot = Arc::new(ChildSlot::default());
        assert!(slot.publish(4301, |_, _| {}));
        let (done, returned) = std::sync::mpsc::channel();
        let (watched, log) = (Arc::clone(&slot), Arc::clone(&sent));
        std::thread::spawn(move || {
            watched.kill_unless_retired_within(Duration::from_secs(60 * 60), |pid, signal| {
                log.lock().unwrap().push((pid, signal));
            });
            let _ = done.send(());
        });
        slot.retire();
        returned
            .recv_timeout(Duration::from_secs(60))
            .expect("the watchdog returns when its child is retired");
        assert!(
            sent.lock().unwrap().is_empty(),
            "a retired child is never killed"
        );

        let stuck = ChildSlot::default();
        assert!(stuck.publish(4302, |_, _| {}));
        stuck.kill_unless_retired_within(Duration::ZERO, |pid, signal| {
            sent.lock().unwrap().push((pid, signal));
        });
        assert_eq!(*sent.lock().unwrap(), [(4302, Signal::Kill)]);
    }

    #[test]
    fn a_stop_ends_the_restart_pause() {
        let slot = Arc::new(ChildSlot::default());
        assert!(!slot.pause_unless_stopped(Duration::ZERO));
        let (done, returned) = std::sync::mpsc::channel();
        let paused = Arc::clone(&slot);
        std::thread::spawn(move || {
            let _ = done.send(paused.pause_unless_stopped(Duration::from_secs(60 * 60)));
        });
        slot.stop(|_, _| {});
        assert_eq!(
            returned.recv_timeout(Duration::from_secs(60)),
            Ok(true),
            "the stop ends the pause, and the worker reads it as a stop"
        );
    }

    #[test]
    fn one_msg_id_is_one_need_however_often_the_line_repeats() {
        let mut observer = ObserverState::inert();
        let now = Instant::now();
        for _ in 0..5 {
            observer.feed_for_test(prompting("69745.190", "com.aterm.aterm", 74756), now);
        }
        observer.feed_for_test(prompting("69745.222", "com.aterm.aterm", 99705), now);
        observer.feed_for_test(prompting("69745.300", "com.example.other", 1234), now);
        assert!(
            observer.take_prompting(None).is_empty(),
            "an unknown identity takes nothing"
        );
        let taken = observer.take_prompting(Some("com.aterm.aterm"));
        assert_eq!(
            taken.iter().map(|n| n.msg_id.as_str()).collect::<Vec<_>>(),
            ["69745.190", "69745.222"],
            "each dialog once, another app's not at all"
        );
        assert!(observer.take_prompting(Some("com.aterm.aterm")).is_empty());
        observer.feed_for_test(prompting("69745.190", "com.aterm.aterm", 74756), now);
        assert!(observer.take_prompting(Some("com.aterm.aterm")).is_empty());
        observer.feed_for_test(prompting("69745.400", "com.aterm.aterm.dev", 7), now);
        assert!(observer.take_prompting(Some("com.aterm.aterm")).is_empty());
    }

    #[test]
    fn a_dialog_is_understood_so_it_does_not_look_like_a_format_change() {
        let mut observer = ObserverState::inert();
        let now = Instant::now();
        observer.feed_for_test(ObserverEvent::Started, now);
        observer.feed_for_test(prompting("1.1", "com.aterm.aterm", 5), now);
        for _ in 0..(FORMAT_DRIFT_LIMIT + 1) {
            observer.feed_for_test(ObserverEvent::Line(TccEvent::Unparsed), now);
        }
        assert_eq!(
            observer.availability(),
            ObserverAvailability::Ok,
            "one understood dialog keeps drift from reading as a format change"
        );
    }

    #[test]
    fn the_lookup_reads_the_accessing_pid_from_the_attribution_line() {
        let out = "\
2026-09-28 17:27:59.877879-0700 0x20e0417  Default     0x6a638eb            69689  0    tccd: \
[com.apple.TCC:access] AUTHREQ_ATTRIBUTION: msgID=69745.190, attribution={responsible=\
{TCCDProcess: identifier=com.aterm.aterm, pid=70374, auid=501, euid=501, \
responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
binary_path=/Applications/aterm.app/Contents/MacOS/aterm}, accessing={TCCDProcess: \
identifier=com.example.tool, pid=44141, auid=501, euid=501, \
binary_path=/Users//someone/bin/tool}, requesting={TCCDProcess: identifier=\
com.apple.sandboxd, pid=69745, auid=0, euid=0, binary_path=/usr/libexec/sandboxd}, },
";
        assert_eq!(accessing_pid_in(out, "69745.190"), Some(44141));
        assert_eq!(
            accessing_pid_in(out, "69745.191"),
            None,
            "another dialog's line"
        );
        assert_eq!(accessing_pid_in("", "69745.190"), None);
        assert_eq!(accessing_pid_in("not a log line", "69745.190"), None);
    }

    #[test]
    fn a_program_is_placed_in_its_tab_by_ancestry() {
        let sessions = [(1_u64, 100), (2, 200)];
        let parent = |pid: i32| match pid {
            500 => Some(400),
            400 => Some(300),
            300 => Some(200),
            200 => Some(2),
            _ => None,
        };
        let no_group = |_pid: i32| None;
        assert_eq!(session_of_pid(500, &sessions, parent, no_group), Some(2));
        assert_eq!(session_of_pid(200, &sessions, parent, no_group), Some(2));
        assert_eq!(session_of_pid(999, &sessions, parent, no_group), None);
        assert_eq!(session_of_pid(0, &sessions, parent, no_group), None);
        let grouped = |pid: i32| (pid == 300).then_some(100);
        assert_eq!(session_of_pid(500, &sessions, parent, grouped), Some(1));
        let cyclic = |pid: i32| Some(if pid == 7 { 8 } else { 7 });
        assert_eq!(session_of_pid(7, &sessions, cyclic, no_group), None);
    }
}
