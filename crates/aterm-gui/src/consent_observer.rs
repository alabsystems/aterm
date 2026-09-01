// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `tccd` consent observer, and the EPERM-driven attention path
//! (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.6).
//!
//! # Two halves, one module
//!
//! **The observer** streams `tccd`'s own log and looks for one signature: an
//! `AUTHREQ_CTX` for a subject, with no matching `AUTHREQ_RESULT` after a
//! threshold, whose *accessing pid* belongs to a session this instance owns.
//! That conjunction is a DIRECT OBSERVATION of a consent prompt naming a
//! process — not silence plus coincidence — and it is the only thing that ever
//! produces a verdict here. A quiet stream produces NOTHING.
//!
//! **The attention path** is the half that ships regardless of the observer:
//! when aterm's OWN file work takes `EPERM(1)` under a protected root, or when
//! the Full Disk Access probe flips granted → denied, one tab attention mark is
//! raised, ONE native notification is posted through `notify.rs`'s existing
//! bounded queue, and the menu-bar glance is re-rendered. Rate-limited to one
//! notification per posture transition, never one per `EPERM`.
//!
//! # `unavailable` is a third value, and it is not `false`
//!
//! `/var/db/diagnostics` is `root:admin drwxr-x---`. On a standard (non-admin)
//! account the stream yields nothing at all, and the honest report is
//! [`ObserverAvailability::Unavailable`] — distinct from `off` (nobody asked)
//! and from a negative verdict (we looked and there is no prompt). The §5.1
//! contract states this outright, and every degradation in this module lands on
//! `unavailable` rather than on a confident falsehood.
//!
//! # What the spike measured (S9, 2026-08-31, GREEN)
//!
//! * **Stream, do not poll.** Over one matched 25 s window `log stream`
//!   delivered 44 `tccd` events where `log show` returned 17: some info-level
//!   events never persist to the archive, so an archive poller silently misses
//!   events a live stream catches. This module streams.
//! * **`tccd` is bursty and idle.** A 12 s sample returned zero events and read
//!   as "streaming does not work"; a 25 s matched window refuted it. A quiet
//!   sample is NEVER a negative here — that is what `unavailable` is for.
//! * `<private>` redaction was 0 of 348 lines, so attribution is in the clear.
//! * Retention is roughly four days, which bounds only an archive query; a live
//!   stream is unaffected.
//! * `log config --status` is root-only and is not needed.
//!
//! # Caveats, stated once and rendered on the panel
//!
//! 1. **Admin-group dependency.** No membership, no events, and the answer is
//!    `unavailable`.
//! 2. **Log retention.** The observer sees only what the running stream
//!    delivers; nothing before it started, and nothing after it stopped.
//! 3. **`<private>` redaction.** Measured at zero on the spike machine, but it
//!    is a system setting, not a promise. A redacted attribution simply never
//!    completes a correlation, so the entry expires and no verdict is made.
//! 4. **Undocumented message format.** The `AUTHREQ_*` shapes are Apple's
//!    internal logging, not API. A format change degrades to
//!    [`UnavailableReason::FormatUnrecognized`], never to a false verdict; the
//!    parser is pure and table-tested over recorded lines so the change is
//!    caught by a failing test rather than by a wrong answer.
//! 5. **Absolute path, always.** `log` is a **zsh builtin** that shadows
//!    `/usr/bin/log`, exits 0 and prints nothing — that artifact alone produced
//!    a false "tccd is invisible" finding. This module names [`LOG_TOOL`].
//!
//! # Off by default, and unreachable from inside a session
//!
//! `[privacy] observer` defaults to FALSE. There is no environment variable
//! (`ATERM_*` is stripped from the shells aterm spawns) and no control verb: a
//! knob a program inside a session could flip would be a consent surface an
//! agent controls, which is the rule that also governs the warm-up (§3.5) and
//! `tccutil reset` (§3.7).
//!
//! # What this module will never do
//!
//! Scan the on-screen window list for a system alert and attach it to a session
//! because that session "went quiet before the alert appeared" (§3.6, *what we
//! will never do*). That manufactures a claim from silence plus coincidence and
//! puts a `WindowServer` call next to the 2026-08-17 fence. There is no window
//! query in this file, and `tests::the_module_makes_no_window_server_query`
//! is the fence.
//!
//! # Threading discipline
//!
//! Copied from `consent_warmup.rs` exactly: a named thread whose `JoinHandle`
//! is DROPPED on the spot, so joining it on the event loop is structurally
//! impossible; a bounded `sync_channel` the producer `try_send`s into and drops
//! on `Full`; and a payload-free poke of the event loop. The child process is
//! signalled by pid (never joined) and is reaped by the worker that spawned it,
//! so no `wait()` ever runs on the event loop.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use aterm_containment::consent::FdaState;

/// The observer's binary, named ABSOLUTELY.
///
/// A bare `log` is a zsh builtin that shadows this and exits 0 with empty
/// output — the single trap that produced a false "tccd is invisible" spike
/// result. Never spell it any other way.
pub(crate) const LOG_TOOL: &str = "/usr/bin/log";

/// The directory whose readability decides whether this account can see the
/// unified log at all. `root:admin drwxr-x---` on macOS: a standard account
/// gets `EACCES` here and the observer reports `unavailable`.
///
/// Ordinary Unix permissions, no TCC service, so probing it cannot raise a
/// dialog.
pub(crate) const DIAGNOSTICS_DIR: &str = "/var/db/diagnostics";

/// The stream predicate. `tccd` is the only process whose `AUTHREQ_*` lines
/// this module understands.
pub(crate) const STREAM_PREDICATE: &str = "process == \"tccd\"";

/// Bound on the observer's event queue.
///
/// The same structural reason `notify.rs` and `consent_warmup.rs` have one: a
/// producer that must never block needs somewhere bounded to put its output.
/// `tccd` is bursty, so unlike the warm-up this cap CAN be reached; the
/// overflow policy is DROP, never coalesce. A dropped line loses at most one
/// correlation, and a lost correlation expires into no verdict — which is the
/// honest failure direction.
const EVENT_QUEUE_CAP: usize = 256;

/// Bound on the in-flight correlation table.
///
/// The table is fed by a stream nothing in this process controls, so it is a
/// ring: the oldest entry is evicted rather than growing without bound. An
/// evicted entry produces no verdict.
const PENDING_CAP: usize = 512;

/// How long an uncorrelated entry may sit before it is discarded.
///
/// An `AUTHREQ_CTX` whose `AUTHREQ_RESULT` never arrived and whose accessing
/// pid never resolved is not evidence of anything after this long — the stream
/// may simply have dropped a line.
pub(crate) const PENDING_MAX_AGE: Duration = Duration::from_secs(600);

/// How long an `AUTHREQ_CTX` must sit un-answered before it counts as a pending
/// prompt (the design's "after N seconds").
///
/// Deliberately several seconds: a granted or already-remembered decision comes
/// back fast, and the dialog is what takes human time.
pub(crate) const PENDING_PROMPT_THRESHOLD: Duration = Duration::from_secs(3);

/// Bound on the announced-`msgID` ring. Large enough that a normal session's
/// dialogs are never re-announced, small enough to be a fixed cost.
const ANNOUNCED_CAP: usize = 64;

/// How many consecutive `AUTHREQ_*` lines may fail to parse, with nothing ever
/// parsed, before the observer declares the format unrecognized.
///
/// The whole point of the number is that a format change lands on `unavailable`
/// rather than on a confident negative.
const FORMAT_DRIFT_LIMIT: u32 = 64;

// ---------------------------------------------------------------------------
// The three-valued availability
// ---------------------------------------------------------------------------

/// Why the observer cannot look.
///
/// Every arm is a reason we could NOT measure — never a measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnavailableReason {
    /// This instance holds the inert arm: headless, a unit test, or a build
    /// with no live observer.
    Inert,
    /// Not macOS; there is no `tccd` and no unified log to stream. Constructed
    /// only by the non-macOS arm, hence unreachable — not dead — on macOS.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    UnsupportedPlatform,
    /// [`DIAGNOSTICS_DIR`] is not readable by this account — the admin-group
    /// constraint. THIS IS NOT A NEGATIVE VERDICT.
    NoLogAccess,
    /// [`LOG_TOOL`] is missing or would not start.
    LogToolUnavailable,
    /// The stream ended on its own. Nothing is being observed any more.
    StreamEnded,
    /// `AUTHREQ_*` lines are arriving and none of them parse: Apple changed an
    /// undocumented message format.
    FormatUnrecognized,
}

impl UnavailableReason {
    /// The report spelling, reported BESIDE `unavailable` so a reader can tell
    /// "no admin group" from "format changed".
    ///
    /// PENDING CONSUMER: `control_privacy`'s `observer log=` row, which renders
    /// a literal `unavailable` while the observer ships off.
    #[allow(dead_code)]
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

/// The `observer log=` value, on the §5.1 three-valued vocabulary.
///
/// `Unavailable` IS NOT `false`. `Off` is "nobody asked"; `Unavailable` is
/// "asked, and could not look"; `Ok` is "looking right now, and a quiet stream
/// means nothing has been observed".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ObserverAvailability {
    /// `[privacy] observer` is false — the shipping default.
    #[default]
    Off,
    /// A stream is live and its lines are understood.
    Ok,
    /// Asked for, and could not look.
    Unavailable(UnavailableReason),
}

impl ObserverAvailability {
    /// The `observer log=` token.
    ///
    /// PENDING CONSUMER: `control_privacy`'s `observer` row (§5.1).
    #[allow(dead_code)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ok => "ok",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// The reason, when there is one. Reported BESIDE `unavailable` so a reader
    /// can tell "no admin group" from "format changed"; never folded into the
    /// token itself, which stays on the closed three-value vocabulary.
    ///
    /// PENDING CONSUMER: `control_privacy`'s `observer` row.
    #[allow(dead_code)]
    pub(crate) const fn reason(self) -> Option<UnavailableReason> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            Self::Off | Self::Ok => None,
        }
    }
}

// ---------------------------------------------------------------------------
// THE PARSER — pure, total, and the only place a log line is understood
// ---------------------------------------------------------------------------

/// One recognized `tccd` message.
///
/// `msg_id` is carried as the RAW text (`475.2921`) rather than a parsed pair:
/// it is a correlation key, nothing computes with it, and an unparsed shape
/// would throw away a key that still correlates perfectly well as a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TccEvent {
    /// `AUTHREQ_CTX` — a request opened, naming the service.
    Ctx {
        msg_id: String,
        service: String,
        /// `preflight=yes` is a capability question, not a prompt. `None` when
        /// the field was absent.
        preflight: Option<bool>,
    },
    /// `AUTHREQ_ATTRIBUTION` — who is asking.
    ///
    /// The line carries up to THREE blocks, measured on this machine:
    /// `responsible={…}`, `accessing={…}` and `requesting={…}`. Only the first
    /// two of those are ever kept, and `responsible=` is deliberately DROPPED:
    /// it names the terminal application (measured: `com.googlecode.iterm2` for
    /// a probe several fork levels down), so it is the same pid for every
    /// session in the instance and correlating on it would attribute one
    /// session's prompt to all of them.
    ///
    /// Both pids are optional because a self-request carries only `requesting=`
    /// and a relayed one carries all three. An absent pid is `None`, never a
    /// guess.
    Attribution {
        msg_id: String,
        accessing_pid: Option<i32>,
        requesting_pid: Option<i32>,
    },
    /// `AUTHREQ_SUBJECT` — the code identity the request is about.
    Subject { msg_id: String, subject: String },
    /// `AUTHREQ_RESULT` — the request closed. This is what makes an open
    /// request stop counting as pending.
    Result { msg_id: String },
    /// An `AUTHREQ_*` line whose shape this parser does not recognize. Counted,
    /// never guessed at: enough of these with nothing understood is what turns
    /// the observer `unavailable`.
    Unparsed,
}

/// Read one field's value out of a log line.
///
/// Finds `key` and returns everything up to the first delimiter — `,`, `}`, or
/// whitespace. Total, allocation-free, and deliberately naive: the message
/// format is undocumented, so the parser claims to understand only the exact
/// shapes recorded in the tests and reports everything else as
/// [`TccEvent::Unparsed`].
fn field_after<'a>(haystack: &'a str, key: &str) -> Option<&'a str> {
    let start = haystack.find(key)? + key.len();
    let rest = haystack.get(start..)?;
    let end = rest.find([',', '}', ' ', '\t']).unwrap_or(rest.len());
    let value = rest.get(..end)?.trim();
    (!value.is_empty()).then_some(value)
}

/// THE PARSE. One log line to at most one event.
///
/// `None` for any line that is not a `tccd` `AUTHREQ_*` message at all — the
/// stream carries plenty of those and they are not evidence of a format change.
/// [`TccEvent::Unparsed`] is reserved for a line that IS an `AUTHREQ_*` message
/// and did not yield its fields, which is exactly the signal a format change
/// produces.
pub(crate) fn parse_line(line: &str) -> Option<TccEvent> {
    let marker = line.find("AUTHREQ_")?;
    let body = line.get(marker..)?;
    let msg_id = field_after(body, "msgID=").map(str::to_owned);
    if body.starts_with("AUTHREQ_ATTRIBUTION") {
        // ANCHOR ON EACH BLOCK, never on the line. `responsible={…}` comes
        // FIRST on a relayed request, so a naive "first pid= after
        // attribution=" would read the terminal app's pid every time.
        let Some(msg_id) = msg_id else {
            return Some(TccEvent::Unparsed);
        };
        if !body.contains("pid=") {
            // A recognized kind with no pid anywhere is the format-change
            // signature, not a self-request.
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
    // Some other `AUTHREQ_*` message. Recognized as one of ours in shape but
    // not in kind, which is not a format change — those are the ones we do not
    // need. Not counted against the drift limit.
    None
}

/// The first `pid=` INSIDE one named attribution block.
///
/// Anchored on the block name because the blocks are concatenated on one line
/// and the leading one is `responsible=` — the terminal application, which is
/// the same pid for every session and must never be correlated on.
fn pid_in_block(body: &str, block: &str) -> Option<i32> {
    let at = body.find(block)?;
    let rest = body.get(at..)?;
    field_after(rest, "pid=")?.parse::<i32>().ok()
}

/// `yes`/`true`/`1` are true; anything else is false. Case-insensitive.
fn parse_yes_no(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "yes" | "true" | "1"
    )
}

/// The `blocked_on=` token for one service (§5.1 / §3.6).
///
/// The `kTCCService` prefix is Apple's internal spelling and carries nothing a
/// reader wants, so it is stripped — the same convention
/// `consent::Folder::tcc_service` already uses. An empty or prefix-only service
/// reports `unknown` rather than an empty token.
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

// ---------------------------------------------------------------------------
// THE CORRELATION TABLE — pure
// ---------------------------------------------------------------------------

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

/// Which attribution block a prompt's pid came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PidSource {
    /// `accessing={…}` — the process on whose behalf the check runs. Preferred.
    Accessing,
    /// `requesting={…}` — the process that asked. Used only when there is no
    /// accessing block, which is what a self-request looks like.
    Requesting,
}

/// A request that has been open long enough, and is complete enough, to be a
/// DIRECT OBSERVATION of a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingPrompt {
    /// The correlation key, kept so a caller can tell two prompts apart.
    pub(crate) msg_id: String,
    /// The TCC service, verbatim from the log.
    pub(crate) service: String,
    /// The process that is parked in the syscall: the `accessing=` pid when the
    /// line carried one, else the `requesting=` pid (a self-request carries
    /// only that). NEVER the `responsible=` pid.
    pub(crate) accessing_pid: i32,
    /// Which block the pid came from, so a reader can tell a direct request
    /// from a relayed one.
    pub(crate) pid_source: PidSource,
    /// The code identity the request names, when the subject line arrived.
    pub(crate) subject: Option<String>,
    /// How long it has been open.
    pub(crate) age: Duration,
}

/// The in-flight requests, keyed by `msgID`.
///
/// Pure and bounded. Nothing here touches the OS, so the whole correlation is
/// table-testable against recorded lines.
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
    /// A fresh, empty table.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Fold one event.
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
                // OR-IN, never overwrite with `None`: a later line that carries
                // fewer blocks must not erase a pid an earlier one supplied.
                slot.accessing_pid = accessing_pid.or(slot.accessing_pid);
                slot.requesting_pid = requesting_pid.or(slot.requesting_pid);
            }
            TccEvent::Subject { msg_id, subject } => {
                self.slot(&msg_id, now).subject = Some(subject);
            }
            // THE CLOSE. A result is what stops a request counting as pending —
            // the whole verdict rests on its absence, so it is applied first and
            // unconditionally.
            TccEvent::Result { msg_id } => self.entries.retain(|e| e.msg_id != msg_id),
            TccEvent::Unparsed => unreachable!("handled above"),
        }
    }

    /// The entry for `msg_id`, created if new. Evicts the OLDEST entry when the
    /// ring is full: an unbounded table fed by a stream nothing in this process
    /// controls is a memory hazard, and an evicted entry simply never produces
    /// a verdict.
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

    /// Drop entries older than `max_age`. An old uncorrelated request is not
    /// evidence; the stream may simply have dropped its result line.
    pub(crate) fn expire(&mut self, now: Instant, max_age: Duration) {
        self.entries
            .retain(|e| now.saturating_duration_since(e.first_seen) < max_age);
    }

    /// THE SIGNATURE. Requests open for at least `threshold`, with a known
    /// service and a known accessing pid, that are not preflights.
    ///
    /// A preflight is a capability question that raises no dialog, so folding
    /// one in here would manufacture a prompt out of a question nobody saw.
    pub(crate) fn pending_prompts(&self, now: Instant, threshold: Duration) -> Vec<PendingPrompt> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let age = now.saturating_duration_since(entry.first_seen);
                if age < threshold || entry.preflight == Some(true) {
                    return None;
                }
                let (accessing_pid, pid_source) = match (entry.accessing_pid, entry.requesting_pid)
                {
                    (Some(pid), _) => (pid, PidSource::Accessing),
                    (None, Some(pid)) => (pid, PidSource::Requesting),
                    (None, None) => return None,
                };
                Some(PendingPrompt {
                    msg_id: entry.msg_id.clone(),
                    service: entry.service.clone()?,
                    accessing_pid,
                    pid_source,
                    subject: entry.subject.clone(),
                    age,
                })
            })
            .collect()
    }

    /// Whether the stream is delivering `AUTHREQ_*` lines this parser does not
    /// understand, and has never understood one. The format-change signature.
    pub(crate) const fn format_looks_changed(&self) -> bool {
        self.parsed_total == 0 && self.unparsed_run >= FORMAT_DRIFT_LIMIT
    }

    /// In-flight request count, for the panel and the tests.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is in flight. The `len` twin, so a reader never has to
    /// compare a count against zero.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Which session owns `accessing_pid`, if any.
///
/// `sessions` is `(session id, that session's shell pid)`. A match is either
/// the pid itself or its process GROUP naming a session's shell — the design's
/// "in a known session's process group". `pgid` is passed IN rather than looked
/// up here so the whole correlation stays pure; the caller's injected arm is
/// what decides whether an OS call happens at all.
///
/// Returns `None` for an unknown pid, which is the only honest answer: a prompt
/// aterm cannot attribute to one of its own sessions is not that session's
/// prompt.
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

// ---------------------------------------------------------------------------
// THE FENCE: the injected stream arm
// ---------------------------------------------------------------------------

/// One message from the streaming worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObserverEvent {
    /// The child started and lines are on the way.
    Started,
    /// The observer could not look. Terminal for this pass.
    Unavailable(UnavailableReason),
    /// One recognized (or explicitly unrecognized) `tccd` message.
    Line(TccEvent),
    /// The stream ended and the child was reaped.
    Ended,
}

/// Everything the worker posts through, gathered so the streaming arm can be a
/// plain function pointer with an inert twin.
pub(crate) struct StreamSink {
    tx: SyncSender<ObserverEvent>,
    poke: Box<dyn Fn() + Send>,
    /// The child's pid while it lives, `0` otherwise. Written by the worker,
    /// read by [`ObserverState::stop`] so a stop can SIGNAL the child without
    /// ever holding its handle — the event loop must never own something it
    /// could be tempted to `wait()` on.
    child_pid: Arc<AtomicI32>,
    /// Set by [`ObserverState::stop`]; the worker checks it between lines.
    stop: Arc<AtomicBool>,
}

impl StreamSink {
    /// Queue one message and poke the event loop.
    ///
    /// `try_send` and DROP on `Full` — never block, never coalesce, exactly as
    /// `notify.rs` and `consent_warmup.rs` do. The poke is issued either way:
    /// the main thread's drain is what discovers both the message and, at the
    /// end, the disconnect.
    fn post(&self, event: ObserverEvent) {
        match self.tx.try_send(event) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(dropped)) => {
                aterm_log::debug!("consent observer dropped a full queue's message: {dropped:?}");
            }
        }
        (self.poke)();
    }
}

/// The one OS question this module asks, behind the injection every consent
/// surface uses.
///
/// * [`ObserverProbe::live`] — a WINDOWED macOS instance: a real
///   `/usr/bin/log stream` child.
/// * [`ObserverProbe::inert`] — a headless instance and every unit test:
///   answers [`UnavailableReason::Inert`] without spawning anything.
#[derive(Clone, Copy)]
pub(crate) struct ObserverProbe {
    run: fn(&StreamSink),
    live: bool,
}

impl ObserverProbe {
    /// The windowed macOS instance's arm.
    pub(crate) const fn live() -> Self {
        Self {
            run: live_run_stream,
            live: true,
        }
    }

    /// The headless / unit-test arm: nothing spawned, and `unavailable` said
    /// out loud.
    pub(crate) const fn inert() -> Self {
        Self {
            run: inert_run_stream,
            live: false,
        }
    }

    /// `live()` for a windowed instance, `inert()` for a headless one.
    pub(crate) const fn for_instance(headless: bool) -> Self {
        if headless {
            Self::inert()
        } else {
            Self::live()
        }
    }

    /// Whether this instance's arm can reach the OS at all.
    ///
    /// PENDING CONSUMER: the Security panel's observer row, which reports "this
    /// instance is not looking" rather than "nothing is pending".
    #[allow(dead_code)]
    pub(crate) const fn is_live(self) -> bool {
        self.live
    }
}

/// The inert arm. No child, no syscall, and the reason said out loud.
fn inert_run_stream(sink: &StreamSink) {
    sink.post(ObserverEvent::Unavailable(UnavailableReason::Inert));
}

/// Whether this account can read the unified log's backing store at all.
///
/// [`DIAGNOSTICS_DIR`] is `root:admin drwxr-x---`, so this is the admin-group
/// test — ordinary Unix permissions, no TCC service, no dialog.
#[cfg(target_os = "macos")]
fn log_store_readable() -> bool {
    std::fs::read_dir(DIAGNOSTICS_DIR).is_ok()
}

/// THE LIVE ARM: one `/usr/bin/log stream` child, read to EOF, then reaped.
///
/// The child is spawned, its pid published for a signal-based stop, and its
/// stdout read line by line. Every line is parsed on THIS thread, so the event
/// loop only ever receives already-classified events. On EOF the child is
/// signalled (it may still be alive if the read simply stopped) and reaped
/// HERE — never on the event loop.
#[cfg(target_os = "macos")]
fn live_run_stream(sink: &StreamSink) {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    if !log_store_readable() {
        // The admin-group constraint. NOT a negative verdict.
        sink.post(ObserverEvent::Unavailable(UnavailableReason::NoLogAccess));
        return;
    }
    let spawned = Command::new(LOG_TOOL)
        .arg("stream")
        .arg("--predicate")
        .arg(STREAM_PREDICATE)
        .arg("--info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        sink.post(ObserverEvent::Unavailable(
            UnavailableReason::LogToolUnavailable,
        ));
        return;
    };
    let pid = i32::try_from(child.id()).unwrap_or(0);
    sink.child_pid.store(pid, Ordering::Release);
    sink.post(ObserverEvent::Started);
    if let Some(stdout) = child.stdout.take() {
        for line in std::io::BufReader::new(stdout).lines() {
            if sink.stop.load(Ordering::Acquire) {
                break;
            }
            let Ok(line) = line else { break };
            if let Some(event) = parse_line(&line) {
                sink.post(ObserverEvent::Line(event));
            }
        }
    }
    // REAPED HERE, on the worker that spawned it. `kill` is idempotent enough
    // for our purposes (an already-exited child just fails) and `wait` returns
    // promptly once the child is gone, which is why this is safe on a thread
    // nothing joins and would never be safe on the event loop.
    let _ = child.kill();
    let _ = child.wait();
    sink.child_pid.store(0, Ordering::Release);
    sink.post(ObserverEvent::Ended);
}

/// Off macOS there is no `tccd` and no unified log. The live arm refuses like
/// the inert one, and says which reason it is.
#[cfg(not(target_os = "macos"))]
fn live_run_stream(sink: &StreamSink) {
    sink.post(ObserverEvent::Unavailable(
        UnavailableReason::UnsupportedPlatform,
    ));
}

// ---------------------------------------------------------------------------
// The instance's observer state
// ---------------------------------------------------------------------------

/// What a start attempt did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    /// A worker was spawned.
    Started,
    /// `[privacy] observer` is false — the shipping default. Nothing spawned,
    /// nothing looked at.
    Disabled,
    /// A worker is already streaming; the call changed nothing.
    AlreadyLive,
    /// The OS refused the thread.
    SpawnFailed,
}

/// The instance's observer: its injected arm, its availability, its bounded
/// receiver and its correlation table.
///
/// Instance-owned, with NO process-global anywhere in this module. An in-place
/// apply builds a fresh `App` and therefore a fresh, empty observer — nothing
/// claims anything about a request the previous process was correlating.
pub(crate) struct ObserverState {
    probe: ObserverProbe,
    availability: ObserverAvailability,
    rx: Option<Receiver<ObserverEvent>>,
    stop: Arc<AtomicBool>,
    child_pid: Arc<AtomicI32>,
    table: PendingTable,
    /// `msgID`s already announced to the human, so one observed prompt pages
    /// once. Bounded like everything else fed by the stream; an evicted id can
    /// at worst be announced a second time, long after the first.
    announced: VecDeque<String>,
}

impl ObserverState {
    /// Wire the arm for this instance. Nothing runs until [`Self::start`].
    pub(crate) fn new(headless: bool) -> Self {
        Self {
            probe: ObserverProbe::for_instance(headless),
            availability: ObserverAvailability::Off,
            rx: None,
            stop: Arc::new(AtomicBool::new(false)),
            child_pid: Arc::new(AtomicI32::new(0)),
            table: PendingTable::new(),
            announced: VecDeque::new(),
        }
    }

    /// The headless / unit-test instance.
    pub(crate) fn inert() -> Self {
        Self::new(true)
    }

    /// Whether this instance's arm can reach the OS.
    ///
    /// PENDING CONSUMER: the Security panel, like `WarmupState::probe_is_live`.
    #[allow(dead_code)]
    pub(crate) const fn probe_is_live(&self) -> bool {
        self.probe.is_live()
    }

    /// The `observer log=` value right now.
    pub(crate) const fn availability(&self) -> ObserverAvailability {
        self.availability
    }

    /// Whether a stream is running.
    ///
    /// PENDING CONSUMER: the Security panel's observer row.
    #[allow(dead_code)]
    pub(crate) const fn is_live(&self) -> bool {
        self.rx.is_some()
    }

    /// In-flight correlations, for the panel and the tests.
    ///
    /// PENDING CONSUMER: the Security panel's observer row.
    #[allow(dead_code)]
    pub(crate) fn pending_len(&self) -> usize {
        self.table.len()
    }

    /// Start the stream. `enabled` is `[privacy] observer`, resolved by the
    /// caller — this module never reads config or environment itself.
    ///
    /// `poke` is the payload-free event-loop wake, abstracted exactly as the
    /// warm-up's is so the posting discipline is unit-testable without an event
    /// loop.
    pub(crate) fn start<P>(&mut self, enabled: bool, poke: P) -> StartOutcome
    where
        P: Fn() + Send + 'static,
    {
        if !enabled {
            self.availability = ObserverAvailability::Off;
            return StartOutcome::Disabled;
        }
        if self.rx.is_some() {
            return StartOutcome::AlreadyLive;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel::<ObserverEvent>(EVENT_QUEUE_CAP);
        self.stop = Arc::new(AtomicBool::new(false));
        self.child_pid = Arc::new(AtomicI32::new(0));
        let sink = StreamSink {
            tx,
            poke: Box::new(poke),
            child_pid: Arc::clone(&self.child_pid),
            stop: Arc::clone(&self.stop),
        };
        let run = self.probe.run;
        let spawned = std::thread::Builder::new()
            .name("consent-observer".into())
            .spawn(move || run(&sink));
        // DETACHED ON PURPOSE: the handle is dropped here and never stored, so
        // no `Drop`, no shutdown path and no future edit can join a thread that
        // is parked in a blocking read of a child's stdout.
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

    /// Stop the stream. NEVER JOINS ANYTHING.
    ///
    /// Sets the stop flag, signals the child by pid (a non-blocking `kill`), and
    /// drops the receiver. The worker notices the flag or the EOF, reaps its own
    /// child, and exits; dropping the receiver also makes every further `post`
    /// a no-op through `Disconnected`.
    pub(crate) fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        signal_child(self.child_pid.load(Ordering::Acquire));
        self.rx = None;
        if matches!(self.availability, ObserverAvailability::Ok) {
            self.availability = ObserverAvailability::Unavailable(UnavailableReason::StreamEnded);
        }
    }

    /// Fold every queued message into the table. Returns how many were applied.
    ///
    /// Cheap and total: `try_recv` until the queue is empty. `Disconnected` is
    /// the authoritative end-of-stream edge — the worker dropped its sender by
    /// exiting — and it is the one signal a full queue cannot drop, so the
    /// availability always lands somewhere honest even if every message was
    /// lost.
    pub(crate) fn drain(&mut self, now: Instant) -> usize {
        // The receiver is TAKEN for the pump and put back only if the stream is
        // still connected, so the disconnect edge retires it exactly once and
        // the fold below borrows `self` freely.
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
        if disconnected && matches!(self.availability, ObserverAvailability::Ok) {
            self.availability = ObserverAvailability::Unavailable(UnavailableReason::StreamEnded);
        }
        self.table.expire(now, PENDING_MAX_AGE);
        // A FORMAT CHANGE DEGRADES TO `unavailable`, NEVER TO A VERDICT.
        if self.table.format_looks_changed() {
            self.availability =
                ObserverAvailability::Unavailable(UnavailableReason::FormatUnrecognized);
        }
        applied
    }

    /// Fold ONE message.
    fn apply(&mut self, event: ObserverEvent, now: Instant) {
        match event {
            ObserverEvent::Started => self.availability = ObserverAvailability::Ok,
            ObserverEvent::Unavailable(reason) => {
                self.availability = ObserverAvailability::Unavailable(reason);
            }
            ObserverEvent::Line(line) => self.table.note(line, now),
            ObserverEvent::Ended => {
                if matches!(self.availability, ObserverAvailability::Ok) {
                    self.availability =
                        ObserverAvailability::Unavailable(UnavailableReason::StreamEnded);
                }
            }
        }
    }

    /// THE VERDICTS. Every observed pending prompt that resolves to one of this
    /// instance's sessions, as `(session, blocked_on token)`.
    ///
    /// `sessions` is `(session id, shell pid)` and `pgid_of` resolves a pid's
    /// process group — passed in so the join stays pure and the OS call, if any,
    /// belongs to the caller's fence.
    ///
    /// EMPTY IS THE COMMON ANSWER, and it means "nothing observed", never "not
    /// blocked". Read [`Self::availability`] alongside it: a quiet stream and an
    /// unavailable observer produce the same empty vector and mean very
    /// different things.
    pub(crate) fn verdicts(
        &self,
        now: Instant,
        sessions: &[(u64, i32)],
        pgid_of: impl Fn(i32) -> Option<i32>,
    ) -> Vec<(u64, String)> {
        if !matches!(self.availability, ObserverAvailability::Ok) {
            return Vec::new();
        }
        let mut out: Vec<(u64, String)> = Vec::new();
        for prompt in self.table.pending_prompts(now, PENDING_PROMPT_THRESHOLD) {
            let pgid = pgid_of(prompt.accessing_pid);
            if let Some(session) = owning_session(prompt.accessing_pid, pgid, sessions)
                && !out.iter().any(|(existing, _)| *existing == session)
            {
                out.push((session, blocked_on_token(&prompt.service)));
            }
        }
        out
    }

    /// THE 3AM PAGE. Observed pending prompts this instance has not announced
    /// yet, marking each announced as it goes.
    ///
    /// One `msgID` pages once — the rate limit here is the identity of the
    /// dialog itself, not a posture epoch, because two distinct dialogs are two
    /// distinct things a human has to answer. A quiet stream returns an empty
    /// vector and nobody is woken.
    pub(crate) fn take_new_verdicts(
        &mut self,
        now: Instant,
        sessions: &[(u64, i32)],
        pgid_of: impl Fn(i32) -> Option<i32>,
    ) -> Vec<ObservedPrompt> {
        if !matches!(self.availability, ObserverAvailability::Ok) {
            return Vec::new();
        }
        let mut fresh: Vec<ObservedPrompt> = Vec::new();
        for prompt in self.table.pending_prompts(now, PENDING_PROMPT_THRESHOLD) {
            if self.announced.contains(&prompt.msg_id) {
                continue;
            }
            let pgid = pgid_of(prompt.accessing_pid);
            let Some(session) = owning_session(prompt.accessing_pid, pgid, sessions) else {
                // Not one of ours. Deliberately NOT marked announced: a prompt
                // we cannot attribute is not a prompt we answered for.
                continue;
            };
            if self.announced.len() >= ANNOUNCED_CAP {
                self.announced.pop_front();
            }
            self.announced.push_back(prompt.msg_id.clone());
            fresh.push(ObservedPrompt {
                session,
                blocked_on: blocked_on_token(&prompt.service),
                service: prompt.service,
            });
        }
        fresh
    }
}

/// One observed pending prompt, attributed to a session, that the human has not
/// been told about yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedPrompt {
    /// The session whose process is parked.
    pub(crate) session: u64,
    /// The `blocked_on=` token (§5.1), already rendered.
    pub(crate) blocked_on: String,
    /// The TCC service verbatim, for the notification body.
    pub(crate) service: String,
}

impl ObservedPrompt {
    /// The one native notification this prompt earns.
    pub(crate) fn notice(&self) -> AttentionNotice {
        let short = self
            .service
            .trim()
            .strip_prefix("kTCCService")
            .unwrap_or(self.service.trim());
        AttentionNotice {
            session: self.session,
            title: ATTENTION_TITLE,
            body: format!(
                "A macOS permission dialog is waiting for an answer ({short}). \
                 Only a human can answer it, and it does not time out."
            ),
        }
    }
}

impl Default for ObserverState {
    /// Default-safe: the arm that cannot reach the OS.
    fn default() -> Self {
        Self::inert()
    }
}

impl Drop for ObserverState {
    /// Signal and let go. NO JOIN, and no `wait()` — the worker reaps its own
    /// child. This is the `trail_audio.rs` incident's rule applied by
    /// construction: a `Drop` that joined a worker froze the whole UI.
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

/// Ask the streaming child to exit. NON-BLOCKING by construction: it signals
/// and returns, and the worker that spawned the child is what reaps it.
#[cfg(unix)]
fn signal_child(pid: i32) {
    if pid <= 0 {
        return;
    }
    // SAFETY: `kill` with a positive pid and SIGTERM is a plain signal delivery
    // with no memory effects. A stale pid fails with ESRCH, which is discarded.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// No child is ever spawned off unix, so there is never one to signal.
#[cfg(not(unix))]
fn signal_child(_pid: i32) {}

/// The process group of `pid`, for the live correlation arm.
///
/// `getpgid` is a getter with no memory effects and cannot prompt. `None` on
/// any failure, which the pure join reads as "cannot attribute" rather than as
/// a negative.
#[cfg(unix)]
pub(crate) fn live_pgid_of(pid: i32) -> Option<i32> {
    if pid <= 0 {
        return None;
    }
    // SAFETY: `getpgid` reads process-table state for a pid and writes nothing.
    let pgid = unsafe { libc::getpgid(pid) };
    (pgid > 0).then_some(pgid)
}

/// Off unix there are no process groups to consult.
#[cfg(not(unix))]
pub(crate) fn live_pgid_of(_pid: i32) -> Option<i32> {
    None
}

// ---------------------------------------------------------------------------
// THE ATTENTION PATH (§3.6 "What ships now", last bullet)
// ---------------------------------------------------------------------------

/// One observed fact that may deserve the away human's attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttentionEvent {
    /// aterm's OWN file work returned `EPERM(1)` on a path under a protected
    /// root. `session` names the tab to mark when there is one; the warm-up is
    /// instance-level work and carries `None`.
    ProtectedEperm { session: Option<u64> },
    /// The Full Disk Access probe reported this state. Only a granted → denied
    /// flip announces; every other move just re-arms and re-renders.
    FdaPosture(FdaState),
}

/// The one native notification an [`AttentionEvent`] may produce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttentionNotice {
    /// The originating session, for `notify.rs`'s focus-aware suppression.
    /// `0` when the fact is instance-level, which suppresses nothing.
    pub(crate) session: u64,
    /// Notification title.
    pub(crate) title: &'static str,
    /// Notification body.
    pub(crate) body: String,
}

/// What the caller should do about one [`AttentionEvent`].
///
/// Pure data: the gate decides, the caller acts. That split is what makes the
/// rate limit table-testable without a window, a menu bar or a notifier.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AttentionOutcome {
    /// Raise this session's tab attention mark.
    pub(crate) raise_tab: Option<u64>,
    /// Post EXACTLY this one notification, through the existing bounded queue.
    pub(crate) notice: Option<AttentionNotice>,
    /// Re-render the menu-bar glance.
    pub(crate) refresh_status_item: bool,
}

/// The notification title. One string, so a reader recognizes the class.
const ATTENTION_TITLE: &str = "aterm · file access";

/// THE RATE LIMIT (§3.6): one notification per POSTURE TRANSITION, never one
/// per `EPERM`.
///
/// A denial storm is one notification. A posture that moves — the probe
/// changing state — re-arms it, so the next real denial is announced again.
/// Instance-owned like everything else here: a fresh `App` starts with the gate
/// armed and no memory of the previous process's posture.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AttentionGate {
    /// The last posture seen, or `None` before the first probe.
    last_fda: Option<FdaState>,
    /// Whether this posture epoch has already spent its one notification.
    spent: bool,
}

impl AttentionGate {
    /// A fresh, armed gate.
    pub(crate) const fn new() -> Self {
        Self {
            last_fda: None,
            spent: false,
        }
    }

    /// The last posture this gate was told about. Reported, never inferred.
    ///
    /// PENDING CONSUMER: the Security panel, which states which posture the
    /// current rate-limit epoch belongs to.
    #[allow(dead_code)]
    pub(crate) const fn last_posture(&self) -> Option<FdaState> {
        self.last_fda
    }

    /// Whether the current posture epoch has spent its notification.
    ///
    /// PENDING CONSUMER: the Security panel, so a human can see that a denial
    /// storm was collapsed rather than missed.
    #[allow(dead_code)]
    pub(crate) const fn is_spent(&self) -> bool {
        self.spent
    }

    /// THE FOLD. One event to one outcome.
    pub(crate) fn note(&mut self, event: AttentionEvent) -> AttentionOutcome {
        match event {
            AttentionEvent::FdaPosture(state) => self.note_posture(state),
            AttentionEvent::ProtectedEperm { session } => self.note_eperm(session),
        }
    }

    /// A probe result. Unchanged posture is a no-op — a poll that keeps
    /// answering `denied` must not re-announce anything.
    fn note_posture(&mut self, state: FdaState) -> AttentionOutcome {
        let previous = self.last_fda.replace(state);
        if previous == Some(state) {
            return AttentionOutcome::default();
        }
        // THE TRANSITION RE-ARMS. Whatever moved, the next denial is news
        // again.
        self.spent = false;
        // The one posture move that is itself worth telling a human about:
        // access aterm HAD is gone. Every other move (unknown → anything,
        // denied → granted) is either good news or not a measurement.
        if previous == Some(FdaState::Granted) && state == FdaState::Denied {
            self.spent = true;
            return AttentionOutcome {
                raise_tab: None,
                notice: Some(AttentionNotice {
                    session: 0,
                    title: ATTENTION_TITLE,
                    body: "aterm no longer has full disk access. Programs here can be \
                           interrupted by macOS consent dialogs until it is granted again in \
                           System Settings ▸ Privacy & Security."
                        .to_owned(),
                }),
                refresh_status_item: true,
            };
        }
        AttentionOutcome {
            raise_tab: None,
            notice: None,
            refresh_status_item: true,
        }
    }

    /// An observed `EPERM` under a protected root. The tab mark is raised every
    /// time (it is per-tab and idempotent); the notification is spent once per
    /// posture epoch.
    fn note_eperm(&mut self, session: Option<u64>) -> AttentionOutcome {
        if self.spent {
            return AttentionOutcome {
                raise_tab: session,
                notice: None,
                refresh_status_item: false,
            };
        }
        self.spent = true;
        AttentionOutcome {
            raise_tab: session,
            notice: Some(AttentionNotice {
                session: session.unwrap_or(0),
                title: ATTENTION_TITLE,
                body: "macOS refused aterm access to a protected folder. Only a human can \
                       clear it: grant access in System Settings ▸ Privacy & Security."
                    .to_owned(),
            }),
            refresh_status_item: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Test seams
// ---------------------------------------------------------------------------

#[cfg(test)]
impl ObserverState {
    /// Fold one worker message without a worker, so the availability ladder and
    /// the correlation table can be driven deterministically. Test seam only:
    /// production folds through [`Self::drain`], and only from a real stream.
    fn feed_for_test(&mut self, event: ObserverEvent, now: Instant) {
        self.apply(event, now);
        self.table.expire(now, PENDING_MAX_AGE);
        if self.table.format_looks_changed() {
            self.availability =
                ObserverAvailability::Unavailable(UnavailableReason::FormatUnrecognized);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// The module's SHIPPING source: everything before the first `#[cfg(test)]`
    /// attribute, which is the test-seam block's. Mirrors `tools/grep_guard.sh`'s
    /// `np_strip`, and it is what lets the scans below name the very patterns
    /// they are forbidding.
    fn shipping_source() -> &'static str {
        const SOURCE: &str = include_str!("consent_observer.rs");
        let marker = "#[cfg(test)]";
        let shipping = SOURCE
            .split(marker)
            .next()
            .expect("split always yields a first part");
        assert!(
            shipping.len() < SOURCE.len(),
            "the test module must be excluded from the scan"
        );
        shipping
    }

    /// Shipping source minus comment lines, matching `np_strip`'s first rule:
    /// a comment naming a hazard is prose, not a syscall.
    fn shipping_code() -> String {
        shipping_source()
            .lines()
            .filter(|line| {
                let t = line.trim_start();
                !(t.starts_with("//") || t.starts_with('*'))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // -----------------------------------------------------------------------
    // THE PARSER, table-tested over recorded lines
    // -----------------------------------------------------------------------

    /// A REAL `AUTHREQ_ATTRIBUTION` line, captured from `/usr/bin/log stream`
    /// on this machine on 2026-08-31, verbatim but for the line wrapping.
    ///
    /// It carries all THREE blocks. `responsible=` is the terminal application
    /// (pid 4059) — the same pid for every session it hosts — `accessing=` is
    /// the process that actually ran the check (pid 18584, an `mdfind` started
    /// from a shell), and `requesting=` is the daemon that relayed it (pid 378).
    /// Correlating on `responsible=` would attribute one session's prompt to
    /// every session in the instance, which is why the parser anchors on blocks.
    const REC_ATTRIBUTION: &str = "2026-08-31 15:58:45.494008-0700 0x8de257   Default     \
        0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: \
        msgID=378.385, attribution={responsible={TCCDProcess: identifier=com.example.term, \
        pid=4059, auid=501, euid=501, responsible_path=/Applications/Example.app/Contents/\
        MacOS/Example, binary_path=/Applications/Example.app/Contents/MacOS/Example}, \
        accessing={TCCDProcess: identifier=com.apple.mdfind, pid=18584, auid=501, euid=501, \
        binary_path=/usr/bin/mdfind}, requesting={TCCDProcess: identifier=com.apple.mds, \
        pid=378, auid=0, euid=0, binary_path=/System/Library/Frameworks/CoreServices.\
        framework/Versions/A/Frameworks/Metadata.framework/Versions/A/Support/mds}, },";

    /// A REAL SELF-REQUEST attribution line, same capture: only `requesting=`,
    /// no `accessing=` block at all. Common, and NOT a format change.
    const REC_ATTRIBUTION_SELF: &str = "2026-08-31 15:30:50.039 Df tccd[66444:8d06f0] \
        [com.apple.TCC:access] AUTHREQ_ATTRIBUTION: msgID=3956.1, \
        attribution={requesting={TCCDProcess: identifier=com.apple.PerfPowerServices, \
        pid=3956, auid=0, euid=0, binary_path=/usr/libexec/PerfPowerServices}, },";

    /// A REAL `AUTHREQ_SUBJECT` line. The subject is a PATH here, not a bundle
    /// id — both shapes occur and both are carried verbatim.
    const REC_SUBJECT: &str = "2026-08-31 15:30:50.049 Df tccd[66444:8d0f0d] \
        [com.apple.TCC:access] AUTHREQ_SUBJECT: msgID=378.385, \
        subject=/usr/bin/mdfind,";

    /// A REAL `AUTHREQ_CTX` line, with `preflight=no` substituted for the
    /// captured `yes` so the fixture exercises the prompt path (a preflight
    /// raises no dialog and is filtered out; `REC_CTX_PREFLIGHT` covers that).
    const REC_CTX: &str = "2026-08-31 15:58:45.493995-0700 0x8de257   Default     \
        0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_CTX: \
        msgID=378.385, function=<private>, service=kTCCServiceSystemPolicyDocumentsFolder, \
        preflight=no, query=1, client_dict=(null), daemon_dict=<private>";

    /// The captured line unmodified: `preflight=yes`, a capability question.
    const REC_CTX_PREFLIGHT: &str = "2026-08-31 15:58:45.493995-0700 0x8de257   Default     \
        0x1579c5c            66389  0    tccd: [com.apple.TCC:access] AUTHREQ_CTX: \
        msgID=378.385, function=<private>, service=kTCCServiceAddressBook, preflight=yes, \
        query=1, client_dict=(null), daemon_dict=<private>";

    /// A REAL `AUTHREQ_RESULT` line — the close.
    const REC_RESULT: &str = "2026-08-31 15:41:28.194 Df tccd[66389:8d871a] \
        [com.apple.TCC:access] AUTHREQ_RESULT: msgID=378.385, authValue=2, authReason=11, \
        authVersion=1, desired_auth=0, error=(null),";

    /// EVERY recorded shape maps to exactly one event, and the fields land where
    /// the correlation expects them. This is the whole reason the parser is pure:
    /// the message format is undocumented, so a change must break a test rather
    /// than produce a verdict.
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

    /// The ATTRIBUTION line carries two pids — `accessing` and `requesting` —
    /// and only the accessing one names the parked process. Anchoring on the
    /// wrong block would attribute a prompt to somebody else's session, which is
    /// worse than reporting nothing.
    #[test]
    fn attribution_never_correlates_on_the_responsible_pid() {
        let Some(TccEvent::Attribution {
            accessing_pid,
            requesting_pid,
            ..
        }) = parse_line(REC_ATTRIBUTION)
        else {
            panic!("the recorded attribution line parses");
        };
        assert_eq!(accessing_pid, Some(18584), "the accessing block");
        assert_eq!(requesting_pid, Some(378), "the requesting block");
        // 4059 is the RESPONSIBLE pid — the terminal application, the same pid
        // for every session it hosts. Reading it would attribute one session's
        // prompt to all of them, and it is the FIRST block on the line, so a
        // parser that did not anchor on block names would read exactly this.
        assert_ne!(accessing_pid, Some(4059));
        assert_ne!(requesting_pid, Some(4059));
    }

    /// A self-request carries ONLY `requesting=`. It is ordinary traffic, not a
    /// format change, and the requesting pid stands in for the accessor.
    #[test]
    fn a_self_request_with_no_accessing_block_is_still_understood() {
        let event = parse_line(REC_ATTRIBUTION_SELF).expect("parses");
        assert_eq!(
            event,
            TccEvent::Attribution {
                msg_id: "3956.1".to_owned(),
                accessing_pid: None,
                requesting_pid: Some(3956),
            }
        );
        assert_ne!(event, TccEvent::Unparsed, "not a format change");
    }

    /// A line that is not a `tccd` `AUTHREQ_*` message is not evidence of
    /// anything — it is the ordinary traffic of a busy log — so it yields `None`
    /// and never counts against the format-drift limit.
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

    /// A RECOGNIZED kind whose fields do not parse is the format-change signal,
    /// and it is reported as [`TccEvent::Unparsed`] rather than guessed at.
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

    /// `preflight=yes` is a capability question, not a prompt.
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

    // -----------------------------------------------------------------------
    // THE CORRELATION
    // -----------------------------------------------------------------------

    fn feed(table: &mut PendingTable, line: &str, now: Instant) {
        let event = parse_line(line).expect("recorded line parses");
        table.note(event, now);
    }

    /// THE SIGNATURE, end to end over the recorded lines: a ctx plus an
    /// attribution, aged past the threshold and never answered, is one pending
    /// prompt naming the accessing pid and the service.
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
        assert_eq!(prompts[0].subject.as_deref(), Some("/usr/bin/mdfind"));
    }

    /// The RESULT closes it. This is the half the whole verdict rests on: with
    /// a result, there was no wall.
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

    /// An open request with NO attribution never becomes a prompt: without the
    /// accessing pid there is nothing to attribute, and "some process somewhere
    /// is asking" is exactly the silence-plus-coincidence claim this design
    /// refuses.
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

    /// A preflight raises no dialog, so it is never reported as one.
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

    /// The table is a RING. A stream nothing in this process controls may not
    /// grow it without bound; an evicted entry produces no verdict.
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

    /// An old uncorrelated request expires. The stream may simply have dropped
    /// its result line, and an ancient open ctx is not evidence of a live wall.
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

    // -----------------------------------------------------------------------
    // Session attribution
    // -----------------------------------------------------------------------

    /// The pid → session join, table-tested. `None` is the answer for anything
    /// aterm cannot attribute — a prompt that is not one of our sessions' is not
    /// our session's prompt.
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

    /// The `blocked_on=` token strips Apple's internal prefix and never renders
    /// empty.
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

    // -----------------------------------------------------------------------
    // The three-valued availability
    // -----------------------------------------------------------------------

    /// `unavailable` IS NOT `false`, and it is not `off` — the §5.1 contract, as
    /// a test over every reason.
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

    /// The shipping DEFAULT is off: a fresh instance has asked nothing, spawned
    /// nothing and holds the arm that cannot reach the OS.
    #[test]
    fn a_fresh_observer_is_off_and_inert() {
        let observer = ObserverState::default();
        assert_eq!(observer.availability(), ObserverAvailability::Off);
        assert!(!observer.probe_is_live());
        assert!(!observer.is_live());
        assert_eq!(observer.pending_len(), 0);
    }

    /// `[privacy] observer = false` spawns NOTHING and stays `off`.
    #[test]
    fn a_disabled_observer_starts_nothing() {
        let pokes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&pokes);
        let mut observer = ObserverState::inert();
        assert_eq!(
            observer.start(false, move || {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
            StartOutcome::Disabled
        );
        assert_eq!(observer.availability(), ObserverAvailability::Off);
        assert!(!observer.is_live());
        assert_eq!(pokes.load(Ordering::Relaxed), 0, "nothing was posted");
    }

    /// The inert arm reports `unavailable(inert)` — it does not claim there is
    /// no prompt, it says it did not look.
    #[test]
    fn the_inert_arm_degrades_to_unavailable_and_never_to_a_verdict() {
        let mut observer = ObserverState::inert();
        assert_eq!(observer.start(true, || {}), StartOutcome::Started);
        // The worker is detached; spin briefly for its one message rather than
        // joining anything (this module owns no handle to join).
        let deadline = Instant::now() + Duration::from_secs(5);
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

    /// A second start while a stream is live is a no-op — one child, never two.
    #[test]
    fn a_second_start_while_live_is_a_no_op() {
        let mut observer = ObserverState::inert();
        assert_eq!(observer.start(true, || {}), StartOutcome::Started);
        assert_eq!(observer.start(true, || {}), StartOutcome::AlreadyLive);
        observer.stop();
        assert!(!observer.is_live());
    }

    /// A QUIET STREAM IS NOT A NEGATIVE. `tccd` is bursty and idle — a 12 s
    /// sample returned zero events on the spike machine and read as "streaming
    /// does not work" until a 25 s matched window refuted it. A live stream with
    /// nothing in it publishes nothing and stays `ok`.
    #[test]
    fn a_quiet_stream_publishes_nothing_and_stays_ok() {
        let now = Instant::now();
        let mut observer = ObserverState::inert();
        observer.feed_for_test(ObserverEvent::Started, now);
        assert_eq!(observer.availability(), ObserverAvailability::Ok);
        assert!(observer.verdicts(now, &[(1, 4711)], |_| None).is_empty());
        assert_eq!(observer.availability(), ObserverAvailability::Ok);
    }

    /// The observed pending prompt becomes ONE verdict for the owning session.
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
        // 18584 is not a shell pid, but its process group is session 4's shell.
        let verdicts = observer.verdicts(later, &[(4, 4711)], |pid| (pid == 18584).then_some(4711));
        assert_eq!(
            verdicts,
            vec![(
                4u64,
                "macos-permission:SystemPolicyDocumentsFolder".to_owned()
            )]
        );
        // And an unrelated session set attributes nothing.
        assert!(
            observer.verdicts(later, &[(4, 9999)], |_| None).is_empty(),
            "a prompt we cannot attribute is nobody's prompt"
        );
    }

    /// THE 3AM PAGE, and its rate limit: one observed dialog pages ONCE, no
    /// matter how many times the correlation is re-read, because the `msgID` is
    /// the identity of the dialog a human still has to answer.
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
        let sessions = &[(4u64, 4711i32)];
        let pgid = |pid: i32| (pid == 18584).then_some(4711);

        let first = observer.take_new_verdicts(later, sessions, pgid);
        assert_eq!(first.len(), 1, "the observed dialog pages");
        assert_eq!(first[0].session, 4);
        assert_eq!(
            first[0].blocked_on,
            "macos-permission:SystemPolicyDocumentsFolder"
        );
        let notice = first[0].notice();
        assert_eq!(notice.session, 4);
        assert!(
            notice.body.contains("SystemPolicyDocumentsFolder"),
            "the body names the service: {}",
            notice.body
        );

        assert!(
            observer.take_new_verdicts(later, sessions, pgid).is_empty(),
            "the same dialog never pages twice"
        );
        // …but the non-consuming read still reports it, because the session IS
        // still blocked.
        assert_eq!(observer.verdicts(later, sessions, pgid).len(), 1);

        // A SECOND, DISTINCT dialog pages again.
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
        let second = observer.take_new_verdicts(later2, sessions, pgid);
        assert_eq!(second.len(), 1, "a distinct dialog is distinct news");
        assert_eq!(
            second[0].blocked_on,
            "macos-permission:SystemPolicyDesktopFolder"
        );
    }

    /// A prompt this instance cannot attribute is NOT marked announced: it was
    /// never ours to answer for, and if it later resolves to a session that
    /// session must still be paged.
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
                .take_new_verdicts(later, &[(4, 9999)], |_| None)
                .is_empty(),
            "not ours"
        );
        let ours =
            observer.take_new_verdicts(later, &[(4, 4711)], |pid| (pid == 18584).then_some(4711));
        assert_eq!(ours.len(), 1, "and once it IS ours, it pages");
    }

    /// An UNAVAILABLE observer pages nobody. `unavailable` is not `false` and it
    /// is certainly not "a dialog is up".
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
                .take_new_verdicts(later, &[(4, 4711)], |pid| (pid == 18584).then_some(4711))
                .is_empty()
        );
    }

    /// A FORMAT CHANGE DEGRADES TO `unavailable`, NEVER TO A VERDICT. The
    /// message shapes are undocumented; the honest failure is to stop claiming.
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

    /// One understood event clears the drift run: a single odd line among good
    /// ones is not a format change.
    #[test]
    fn one_understood_event_clears_the_drift_run() {
        let now = Instant::now();
        let mut table = PendingTable::new();
        for _ in 0..FORMAT_DRIFT_LIMIT {
            table.note(TccEvent::Unparsed, now);
        }
        assert!(table.format_looks_changed());
        table.note(
            TccEvent::Result {
                msg_id: "1.1".to_owned(),
            },
            now,
        );
        assert!(!table.format_looks_changed());
    }

    /// The disconnect edge is authoritative: a worker that exits leaves the
    /// observer `unavailable(stream-ended)`, never silently "ok".
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

    // -----------------------------------------------------------------------
    // THE ATTENTION PATH
    // -----------------------------------------------------------------------

    /// ONE NOTIFICATION PER POSTURE TRANSITION, NOT ONE PER EPERM. An agent
    /// hammering a protected path must not become a notification storm.
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

    /// A granted → denied flip is itself worth telling a human about, and it is
    /// the only posture move that announces.
    #[test]
    fn only_a_granted_to_denied_flip_announces_a_posture_move() {
        let cases: &[(FdaState, FdaState, bool)] = &[
            (FdaState::Granted, FdaState::Denied, true),
            (FdaState::Denied, FdaState::Granted, false),
            (FdaState::Unknown, FdaState::Denied, false),
            (FdaState::Granted, FdaState::Unknown, false),
            (FdaState::Unknown, FdaState::Granted, false),
        ];
        for (from, to, announces) in cases {
            let mut gate = AttentionGate::new();
            let _ = gate.note(AttentionEvent::FdaPosture(*from));
            let outcome = gate.note(AttentionEvent::FdaPosture(*to));
            assert_eq!(
                outcome.notice.is_some(),
                *announces,
                "{from:?} -> {to:?} announces={announces}"
            );
            assert!(
                outcome.refresh_status_item,
                "a posture move always re-renders"
            );
            assert_eq!(outcome.raise_tab, None, "a posture is not a tab fact");
        }
    }

    /// An UNCHANGED posture is a no-op. The probe polls; a poll that keeps
    /// answering `denied` must not re-announce anything.
    #[test]
    fn an_unchanged_posture_changes_nothing() {
        let mut gate = AttentionGate::new();
        let _ = gate.note(AttentionEvent::FdaPosture(FdaState::Denied));
        let repeat = gate.note(AttentionEvent::FdaPosture(FdaState::Denied));
        assert_eq!(repeat, AttentionOutcome::default());
        assert_eq!(gate.last_posture(), Some(FdaState::Denied));
    }

    /// A posture TRANSITION re-arms the gate: the storm's notification is spent,
    /// but the next real change is news again.
    #[test]
    fn a_posture_transition_rearms_the_spent_gate() {
        let mut gate = AttentionGate::new();
        assert!(
            gate.note(AttentionEvent::ProtectedEperm { session: None })
                .notice
                .is_some()
        );
        assert!(gate.is_spent());
        assert!(
            gate.note(AttentionEvent::ProtectedEperm { session: None })
                .notice
                .is_none()
        );
        let _ = gate.note(AttentionEvent::FdaPosture(FdaState::Granted));
        assert!(!gate.is_spent(), "the move re-armed it");
        assert!(
            gate.note(AttentionEvent::ProtectedEperm { session: None })
                .notice
                .is_some(),
            "and the next denial is news again"
        );
    }

    /// An instance-level denial (the warm-up) marks no tab — there is no session
    /// it belongs to — but it still notifies once.
    #[test]
    fn an_instance_level_eperm_marks_no_tab_but_still_notifies() {
        let mut gate = AttentionGate::new();
        let outcome = gate.note(AttentionEvent::ProtectedEperm { session: None });
        assert_eq!(outcome.raise_tab, None);
        let notice = outcome.notice.expect("instance denials still notify");
        assert_eq!(notice.session, 0, "0 suppresses nothing in notify.rs");
        assert!(!notice.body.is_empty());
    }

    /// The notification text names the human action and nothing aterm could do
    /// itself, and it never uses the retired vocabulary the repo guard forbids.
    #[test]
    fn the_notification_text_asks_a_human_and_avoids_the_retired_vocabulary() {
        let mut gate = AttentionGate::new();
        let eperm = gate
            .note(AttentionEvent::ProtectedEperm { session: Some(1) })
            .notice
            .expect("a first denial notifies");
        let mut flip = AttentionGate::new();
        let _ = flip.note(AttentionEvent::FdaPosture(FdaState::Granted));
        let posture = flip
            .note(AttentionEvent::FdaPosture(FdaState::Denied))
            .notice
            .expect("a granted -> denied flip notifies");
        for notice in [eperm, posture] {
            let lower = notice.body.to_ascii_lowercase();
            for banned in ["restart", "relaunch", "reopen", "reboot", "next launch"] {
                assert!(
                    !lower.contains(banned),
                    "notification text must not say {banned}: {}",
                    notice.body
                );
            }
            assert!(
                lower.contains("system settings"),
                "it must name where a human can act: {}",
                notice.body
            );
            assert_eq!(notice.title, ATTENTION_TITLE);
        }
    }

    // -----------------------------------------------------------------------
    // THE FENCES
    // -----------------------------------------------------------------------

    /// `log` is a ZSH BUILTIN that shadows `/usr/bin/log`, exits 0 and prints
    /// nothing — the artifact that produced a false "tccd is invisible"
    /// finding. The tool is named absolutely, here and nowhere else.
    #[test]
    fn the_log_tool_is_named_absolutely() {
        assert_eq!(LOG_TOOL, "/usr/bin/log");
        assert!(std::path::Path::new(LOG_TOOL).is_absolute());
        let code = shipping_code();
        assert!(
            !code.contains("Command::new(\"log\")"),
            "a bare `log` is the zsh builtin, not the tool"
        );
        assert_eq!(
            code.matches("Command::new(").count(),
            1,
            "exactly one child is ever spawned, and it is LOG_TOOL"
        );
        assert!(code.contains("Command::new(LOG_TOOL)"));
    }

    /// B13: no protected-folder path literal lives here. Paths arrive as
    /// already-resolved data or not at all; this module holds none of its own,
    /// and the one absolute path it does name is the log store, which is
    /// ordinary Unix permissions and cannot raise a dialog.
    #[test]
    fn the_module_contains_no_protected_path_literal() {
        let code = shipping_code();
        for needle in [
            "~/Documents",
            "~/Desktop",
            "~/Downloads",
            "/Volumes",
            "CloudStorage",
            "Containers",
        ] {
            assert!(
                !code.contains(needle),
                "shipping source must not name a protected path: {needle}"
            );
        }
        assert_eq!(DIAGNOSTICS_DIR, "/var/db/diagnostics");
    }

    /// §3.6 "what we will never do": no window-list scan, no `WindowServer`
    /// contact, no alert-on-screen inference. The 2026-08-17 fence is right
    /// next to this code.
    #[test]
    fn the_module_makes_no_window_server_query() {
        let code = shipping_code();
        for needle in [
            "WindowServer",
            "UserNotificationCenter",
            "CGWindowList",
            "NSWorkspace",
            "objc2",
        ] {
            assert!(
                !code.contains(needle),
                "the observer must never ask the window server anything: {needle}"
            );
        }
    }

    /// OFF BY DEFAULT AND UNREACHABLE FROM INSIDE A SESSION: no environment
    /// knob, and no process-global. The enable decision arrives as a resolved
    /// `bool` argument from the config layer, so a program inside a session has
    /// nothing to flip.
    #[test]
    fn the_module_owns_no_process_global_and_reads_no_environment_knob() {
        let code = shipping_code();
        for needle in [
            "std::env::var",
            "env::var",
            "ATERM_",
            "static mut",
            "OnceLock",
        ] {
            assert!(
                !code.contains(needle),
                "the observer must own no global and read no env knob: {needle}"
            );
        }
    }

    /// The threading discipline, asserted structurally: the worker's
    /// `JoinHandle` is dropped on the spot, and NOTHING here joins a thread or
    /// waits on a child from the event loop.
    #[test]
    fn nothing_in_this_module_joins_a_worker() {
        let code = shipping_code();
        assert!(
            !code.contains(".join()"),
            "a join on the event loop is the trail_audio.rs freeze"
        );
        assert!(
            code.contains("Ok(_handle) => {}"),
            "the handle must be dropped where it is produced"
        );
        // `wait()` exists exactly once, inside the worker that spawned the
        // child — never on any path the event loop can take.
        assert_eq!(
            code.matches(".wait()").count(),
            1,
            "the only reap is the worker's own"
        );
    }

    /// NOT REACHABLE FROM A CONTROL VERB. A consent surface an agent could
    /// enable from inside a session is the rule that governs the warm-up and
    /// `tccutil reset`; the observer takes the same fence.
    #[test]
    fn no_control_dispatch_arm_can_reach_the_observer() {
        for source in [
            include_str!("control.rs"),
            include_str!("control_privacy.rs"),
            include_str!("control_session.rs"),
            include_str!("control_query.rs"),
        ] {
            assert!(
                !source.contains("consent_observer"),
                "no control-verb file may reference the observer"
            );
        }
    }
}
