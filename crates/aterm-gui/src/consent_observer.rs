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
//! when aterm's OWN file work takes `EPERM(1)` under a protected root, one tab
//! attention mark is raised, ONE native notification is posted through
//! `notify.rs`'s existing bounded queue, and the menu-bar glance is
//! re-rendered. Rate-limited to one notification, never one per `EPERM`.
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
//! # Three modes, and unreachable from inside a session
//!
//! `[privacy] observer` is a tri-state ([`ObserverMode`]). ABSENT is the
//! SENTINEL: one `/usr/bin/log stream` child whose predicate matches only the
//! line macOS writes when it shows a consent dialog (`AUTHREQ_PROMPTING`),
//! which is silent between dialogs and whose only job is to tell the access
//! card that a real need exists (a `find /` in a tab just raised the "access
//! data from other apps" dialog). `false` is off. `true` is FULL: the whole
//! `tccd` stream, the correlation table, the tab mark and the per-dialog OS
//! banner. The banner, the once-per-process `EPERM` banner and the tab mark
//! run ONLY in Full mode: a banner that repeats what the dialog already says
//! on screen is exactly the non-actionable alert the owner ruled out, and the
//! card's one question carries the one action.
//!
//! There is no environment variable (`ATERM_*` is stripped from the shells
//! aterm spawns) and no control verb: a knob a program inside a session could
//! flip would be a consent surface an agent controls, which is the rule that
//! also governs the warm-up (§3.5) and `tccutil reset` (§3.7).
//!
//! # The sentinel's lifetime: a quiet stream needs a timeout
//!
//! The full stream writes ~150 lines a second, so an orphaned `log stream`
//! (its reader gone after an in-place update: `exec` and `_exit` run no
//! destructor) dies of `EPIPE` within a second. A sentinel writes almost
//! never, so an orphan would live until the next dialog. Every generation of
//! it is therefore started with `--timeout 15m` and the worker respawns it on
//! EOF ([`RotationPolicy`]): an end at or after 14 min 59 s is a normal
//! rotation, an early end restarts at most three times an hour and then
//! reports `unavailable`. An orphan lives at most fifteen minutes and NO exit
//! path needs a hook. The blind gap between generations is one spawn; a missed
//! line means one dialog does not raise the entry and the next one does.
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

/// The observer's binary, named ABSOLUTELY, from the one place it is spelled
/// (`aterm_containment::consent`, which the Developer Tools row's log read
/// shares).
///
/// A bare `log` is a zsh builtin that shadows this and exits 0 with empty
/// output — the single trap that produced a false "tccd is invisible" spike
/// result. Never spell it any other way.
#[cfg(any(target_os = "macos", test))]
pub(crate) use aterm_containment::consent::LOG_TOOL;

/// Whether this account can read the unified log's backing store at all:
/// `aterm_containment::consent::DIAGNOSTICS_DIR` is `root:admin drwxr-x---` on
/// macOS, so a standard account gets `EACCES` there and the observer reports
/// `unavailable`. Ordinary Unix permissions, no TCC service, no dialog.
#[cfg(target_os = "macos")]
use aterm_containment::consent::log_store_readable;

/// The FULL-mode stream predicate. `tccd` is the only process whose
/// `AUTHREQ_*` lines this module understands.
#[cfg(any(target_os = "macos", test))]
pub(crate) const STREAM_PREDICATE: &str = "process == \"tccd\"";

/// The SENTINEL predicate: `tccd`, and only the one line that means a consent
/// dialog is on screen. Measured on 26.6.2: no lines in 40 s while the full
/// stream is ~150 lines/s.
#[cfg(any(target_os = "macos", test))]
pub(crate) const SENTINEL_PREDICATE: &str =
    "process == \"tccd\" AND eventMessage CONTAINS \"AUTHREQ_PROMPTING\"";

/// Every generation of the sentinel stream ends by itself after this long
/// (`log stream --timeout`), so an orphan is bounded.
#[cfg(any(target_os = "macos", test))]
pub(crate) const SENTINEL_TIMEOUT: &str = "15m";

/// A sentinel generation that ran at least this long ended because of its
/// timeout (one second of slack under fifteen minutes).
#[cfg(any(target_os = "macos", test))]
const ROTATION_MIN: Duration = Duration::from_secs(14 * 60 + 59);

/// How many EARLY ends the sentinel restarts from in one hour.
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

/// Which observer the config asks for. See the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObserverMode {
    /// `observer = false`: nothing is spawned or looked at.
    Off,
    /// `observer` absent: the narrow `AUTHREQ_PROMPTING` stream that feeds the
    /// access card and posts no banner.
    Sentinel,
    /// `observer = true`: the whole `tccd` stream, the tab mark and the banner.
    Full,
}

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
#[cfg(any(target_os = "macos", test))]
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
    /// Not macOS; there is no `tccd` and no unified log to stream.
    #[cfg(any(not(target_os = "macos"), test))]
    UnsupportedPlatform,
    /// `aterm_containment::consent::DIAGNOSTICS_DIR` is not readable by this
    /// account — the admin-group constraint. THIS IS NOT A NEGATIVE VERDICT.
    #[cfg(any(target_os = "macos", test))]
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
    /// A stream is live and its lines are understood. Only the macOS log
    /// stream ever starts one.
    #[cfg(any(target_os = "macos", test))]
    Ok,
    /// Asked for, and could not look.
    Unavailable(UnavailableReason),
}

impl ObserverAvailability {
    /// Whether a stream is live right now — never, where none can start.
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

    /// The `observer log=` token.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            #[cfg(any(target_os = "macos", test))]
            Self::Ok => "ok",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// The reason, when there is one. Reported BESIDE `unavailable` so a reader
    /// can tell "no admin group" from "format changed"; never folded into the
    /// token itself, which stays on the closed three-value vocabulary.
    #[cfg(test)]
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
#[cfg(any(target_os = "macos", test))]
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
    /// `AUTHREQ_PROMPTING` — a consent dialog is ON SCREEN. This is the one
    /// line the sentinel exists for. `subject` is the code identity the dialog
    /// is for (`Sub:{…}`), `resp_pid` the RESPONSIBLE process (`Resp:{…}`): for
    /// a program run in a live tab that is aterm's own window, for a tab that
    /// survived an update it is the accessing program itself.
    Prompting {
        msg_id: String,
        service: String,
        subject: String,
        resp_pid: i32,
    },
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
#[cfg(any(target_os = "macos", test))]
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
#[cfg(any(target_os = "macos", test))]
pub(crate) fn parse_line(line: &str) -> Option<TccEvent> {
    // `log stream` opens EVERY generation with a header on stdout that echoes
    // the predicate — `Filtering the log data using "... CONTAINS
    // "AUTHREQ_PROMPTING""` — which contains the marker but is not a message.
    // Counted as `Unparsed`, one per rotation, it would trip the format-drift
    // limit after 64 quiet generations (16 h) and report Apple's format as
    // changed on a machine where no dialog ever appears.
    if line.trim_start().starts_with("Filtering the log data") {
        return None;
    }
    let marker = line.find("AUTHREQ_")?;
    let body = line.get(marker..)?;
    // A message is `AUTHREQ_<KIND>: ...`. The kind is followed by a colon; a
    // predicate echo (`AUTHREQ_PROMPTING""`) or any other mention is not one.
    let kind_end = body.find(|c: char| !(c.is_ascii_uppercase() || c == '_'))?;
    if body.as_bytes().get(kind_end) != Some(&b':') {
        return None;
    }
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
    if body.starts_with("AUTHREQ_PROMPTING") {
        // `subject=Sub:{com.aterm.aterm}Resp:{TCCDProcess: …, pid=74756, …},`:
        // the subject sits between `Sub:{` and the next `}`, the pid inside the
        // `Resp:{` block. Anything that does not yield ALL of it is Unparsed,
        // never guessed.
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
                // OR-IN, never overwrite with `None`: a later line that carries
                // fewer blocks must not erase a pid an earlier one supplied.
                slot.accessing_pid = accessing_pid.or(slot.accessing_pid);
                slot.requesting_pid = requesting_pid.or(slot.requesting_pid);
            }
            TccEvent::Subject { msg_id, subject } => {
                self.slot(&msg_id, now).subject = Some(subject);
            }
            // A dialog on screen is understood (it keeps the format-drift
            // count honest) but opens no correlation: the host takes it as a
            // NEED through `ObserverState::take_prompting`.
            TccEvent::Prompting { .. } => {}
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
    #[cfg(test)]
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

/// Which session a process belongs to, by ANCESTRY: the pid itself, its process
/// group, then each parent in turn (at most 32 levels, stopping at pid 1) —
/// the same [`owning_session`] rule at every level. `None` for a process aterm
/// cannot place in one of its own tabs, which is the only honest answer.
/// Both lookups are passed in so the walk stays pure and the OS call belongs to
/// the caller's fence.
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

// ---------------------------------------------------------------------------
// THE FENCE: the injected stream arm
// ---------------------------------------------------------------------------

/// One message from the streaming worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ObserverEvent {
    /// The child started and lines are on the way.
    #[cfg(any(target_os = "macos", test))]
    Started,
    /// The observer could not look. Terminal for this pass.
    Unavailable(UnavailableReason),
    /// One recognized (or explicitly unrecognized) `tccd` message.
    #[cfg(any(target_os = "macos", test))]
    Line(TccEvent),
    /// A consent dialog is on screen (`AUTHREQ_PROMPTING`), enriched with the
    /// accessing pid where it could be found. Posted by the stream worker or,
    /// after a bounded lookup, by a short-lived lookup thread.
    #[cfg(any(target_os = "macos", test))]
    Prompt(PromptNeed),
    /// The stream ended and the child was reaped.
    #[cfg(any(target_os = "macos", test))]
    Ended,
}

/// One observed consent dialog for OUR bundle or another's: everything the
/// host needs to decide whether it is a need of this aterm, and from which
/// tab. Data only.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) struct PromptNeed {
    /// The dialog's `msgID`, its identity for de-duplication.
    pub(crate) msg_id: String,
    /// The TCC service, verbatim (`kTCCServiceSystemPolicyAppData`).
    pub(crate) service: String,
    /// The code identity the dialog is for (`com.aterm.aterm`).
    pub(crate) subject: String,
    /// The RESPONSIBLE process.
    pub(crate) resp_pid: i32,
    /// The process that actually accessed, when it could be found: the
    /// responsible pid itself when that is not our window, else the result of
    /// the on-demand lookup. Best effort, never a gate.
    pub(crate) accessing_pid: Option<i32>,
}

/// Everything the worker posts through, gathered so the streaming arm can be a
/// plain function pointer with an inert twin. Cloneable: a short-lived lookup
/// thread posts its answer through a clone.
#[derive(Clone)]
pub(crate) struct StreamSink {
    tx: SyncSender<ObserverEvent>,
    poke: Arc<std::sync::Mutex<Box<dyn Fn() + Send>>>,
    /// The child's pid while it lives, `0` otherwise. Written by the worker,
    /// read by [`ObserverState::stop`] so a stop can SIGNAL the child without
    /// ever holding its handle — the event loop must never own something it
    /// could be tempted to `wait()` on. Only the macOS stream has a child.
    #[cfg(target_os = "macos")]
    child_pid: Arc<AtomicI32>,
    /// Set by [`ObserverState::stop`]; the worker checks it between lines.
    #[cfg(target_os = "macos")]
    stop: Arc<AtomicBool>,
    /// Lookups in flight ([`MAX_LOOKUPS`]).
    #[cfg(target_os = "macos")]
    lookups: Arc<std::sync::atomic::AtomicUsize>,
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
        let poke = self.poke.lock().unwrap_or_else(|p| p.into_inner());
        (poke)();
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
    run: fn(&StreamSink, ObserverMode),
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
    #[cfg(test)]
    pub(crate) const fn is_live(self) -> bool {
        self.live
    }
}

/// The inert arm. No child, no syscall, and the reason said out loud.
fn inert_run_stream(sink: &StreamSink, _mode: ObserverMode) {
    sink.post(ObserverEvent::Unavailable(UnavailableReason::Inert));
}

/// The arguments of one stream generation for `mode`.
///
/// FULL: the whole `tccd` stream, ended only by a stop or an EOF. SENTINEL: the
/// narrow predicate and `--timeout` ([`SENTINEL_TIMEOUT`]), so the child ends
/// by itself. Pure, so the two are table-tested.
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

/// The arguments of the bounded on-demand attribution lookup for one dialog:
/// who accessed, from the log the stream itself is reading (E9: 1.3 s wall).
/// `None` unless `msg_id` is made only of digits and dots, so a line of the
/// log can never inject into the predicate.
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

/// The accessing pid the lookup's output names for `msg_id`, from the
/// `accessing=` block (else the `requesting=` one), or `None`.
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

/// What to do when one sentinel generation's child has exited.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamEnd {
    /// It ran to its timeout: a normal rotation, start the next generation.
    Rotate,
    /// It ended EARLY but the hourly budget allows one more start.
    Restart,
    /// It ended early too often (or this is not a rotating stream): stop and
    /// report `unavailable(StreamEnded)`.
    GiveUp,
}

/// The sentinel's restart policy: rotation is free, an early end costs one of
/// [`EARLY_RESTARTS_PER_HOUR`]. Pure over the times it is given.
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

    /// One generation of `mode` ended after `ran_for`, at `now`.
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

/// Run `/usr/bin/log` with `args`, feeding every stdout line to `on_line`, then
/// kill and reap the child HERE, on the worker that spawned it. The ONE place
/// this module spawns a process and the ONE `wait()`.
///
/// * `child_pid`: publishes the pid while the child lives, so
///   [`ObserverState::stop`] can signal it by pid.
/// * `stop`: checked between lines.
/// * `kill_after`: a bounded run — a watchdog thread SIGKILLs the child by pid
///   after this long. It cannot hit a reused pid: it kills only under a lock
///   the worker takes, and sets, BEFORE it reaps.
///
/// `Err` only when the tool would not start.
#[cfg(target_os = "macos")]
fn run_log_tool(
    args: &[String],
    child_pid: Option<&AtomicI32>,
    stop: Option<&AtomicBool>,
    kill_after: Option<Duration>,
    on_spawn: impl FnOnce(),
    mut on_line: impl FnMut(&str),
) -> Result<(), UnavailableReason> {
    use aterm_uds::spawnfd::{ChildStdio, LaneCommand};
    use std::io::BufRead;

    // THE CHILD HOLDS ITS STDOUT AND NOTHING ELSE (`spawnfd::spawn`:
    // `posix_spawn` with `POSIX_SPAWN_CLOEXEC_DEFAULT`). A plain `Command` handed
    // this child — the stream lives as long as the observer — every descriptor
    // this process held without close-on-exec at the spawn: Metal's shader-cache
    // files, or a fabric bridge lane caught between its `socketpair` and its
    // `F_SETFD`, whose fail-closed halt then waited on `log` (2026-09-30 review).
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
    let pid = i32::try_from(child.id()).unwrap_or(0);
    if let Some(slot) = child_pid {
        slot.store(pid, Ordering::Release);
    }
    let reaped = Arc::new(std::sync::Mutex::new(false));
    if let Some(limit) = kill_after {
        let reaped = Arc::clone(&reaped);
        let _ = std::thread::Builder::new()
            .name("consent-lookup-watchdog".into())
            .spawn(move || {
                let deadline = Instant::now() + limit;
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                    if *reaped.lock().unwrap_or_else(|p| p.into_inner()) {
                        return;
                    }
                }
                let reaped = reaped.lock().unwrap_or_else(|p| p.into_inner());
                // A pid that did not fit is 0, and `kill(0, …)` would signal
                // aterm's own process group ([`signal_child`] refuses it too).
                if !*reaped && pid > 0 {
                    // SAFETY: a plain signal delivery to a child that is not
                    // yet reaped (the worker sets the flag under this lock
                    // before it waits), so the pid is still ours.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            });
    }
    on_spawn();
    for line in std::io::BufReader::new(stdout).lines() {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            break;
        }
        let Ok(line) = line else { break };
        on_line(&line);
    }
    // REAPED HERE, on the worker that spawned it. `kill` is idempotent enough
    // for our purposes (an already-exited child just fails) and `wait` returns
    // promptly once the child is gone, which is why this is safe on a thread
    // nothing joins and would never be safe on the event loop.
    *reaped.lock().unwrap_or_else(|p| p.into_inner()) = true;
    let _ = child.kill();
    let _ = child.wait();
    if let Some(slot) = child_pid {
        slot.store(0, Ordering::Release);
    }
    Ok(())
}

/// One dialog on screen: post it, with the accessing pid where it can be found.
///
/// The responsible pid is what `AUTHREQ_PROMPTING` names. When it is not our
/// window (a tab that survived an update: the responsible instance is the
/// accessing program itself) that IS the accessing pid. When it is our window,
/// one bounded `log show` finds who accessed, on a short-lived thread, so the
/// stream keeps reading; a burst beyond [`MAX_LOOKUPS`] is posted without a
/// pid (a need without a tab).
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
                None,
                None,
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

/// THE LIVE ARM: `/usr/bin/log stream` children, read to EOF, then reaped —
/// one for [`ObserverMode::Full`], a rotating series for the sentinel
/// ([`RotationPolicy`]). Every line is parsed on THIS thread, so the event loop
/// only ever receives already-classified events.
#[cfg(target_os = "macos")]
fn live_run_stream(sink: &StreamSink, mode: ObserverMode) {
    if !log_store_readable() {
        // The admin-group constraint. NOT a negative verdict.
        sink.post(ObserverEvent::Unavailable(UnavailableReason::NoLogAccess));
        return;
    }
    let mut policy = RotationPolicy::new();
    let mut started_posted = false;
    loop {
        let generation = Instant::now();
        let generation_wall = std::time::SystemTime::now();
        let ran = run_log_tool(
            &stream_args(mode),
            Some(&sink.child_pid),
            Some(&sink.stop),
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
        if sink.stop.load(Ordering::Acquire) {
            break;
        }
        // How long the generation lived. The monotonic clock does not advance
        // while the Mac sleeps, but the child's `--timeout` may: a generation
        // that spanned a sleep would then end "early" by the monotonic count
        // and burn the hourly restart budget on what was a rotation. The
        // larger of the two is the honest length (a wall clock stepped back
        // only ever makes the monotonic one win).
        let ran_for = generation
            .elapsed()
            .max(generation_wall.elapsed().unwrap_or_default());
        match policy.on_end(mode, ran_for, Instant::now()) {
            StreamEnd::Rotate => {}
            StreamEnd::Restart => std::thread::sleep(Duration::from_secs(1)),
            StreamEnd::GiveUp => break,
        }
    }
    sink.post(ObserverEvent::Ended);
}

/// Off macOS there is no `tccd` and no unified log. The live arm refuses like
/// the inert one, and says which reason it is.
#[cfg(not(target_os = "macos"))]
fn live_run_stream(sink: &StreamSink, _mode: ObserverMode) {
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
    /// Observed dialogs waiting for the host ([`Self::take_prompting`]).
    prompts: VecDeque<PromptNeed>,
    /// `msgID`s of dialogs already queued, so one dialog is one need however
    /// often its line repeats. Separate from `announced` (Full mode's banner).
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    prompt_seen: VecDeque<String>,
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
            prompts: VecDeque::new(),
            prompt_seen: VecDeque::new(),
        }
    }

    /// The headless / unit-test instance.
    pub(crate) fn inert() -> Self {
        Self::new(true)
    }

    /// Whether this instance's arm can reach the OS.
    #[cfg(test)]
    pub(crate) const fn probe_is_live(&self) -> bool {
        self.probe.is_live()
    }

    /// The `observer log=` value right now.
    pub(crate) const fn availability(&self) -> ObserverAvailability {
        self.availability
    }

    /// Whether a stream is running.
    #[cfg(test)]
    pub(crate) const fn is_live(&self) -> bool {
        self.rx.is_some()
    }

    /// In-flight correlations.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.table.len()
    }

    /// Start the stream. `mode` is `[privacy] observer` resolved by the caller
    /// (and gated on the card's own switches for the sentinel) — this module
    /// never reads config or environment itself.
    ///
    /// `poke` is the payload-free event-loop wake, abstracted exactly as the
    /// warm-up's is so the posting discipline is unit-testable without an event
    /// loop.
    pub(crate) fn start<P>(&mut self, mode: ObserverMode, poke: P) -> StartOutcome
    where
        P: Fn() + Send + 'static,
    {
        if mode == ObserverMode::Off {
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
            poke: Arc::new(std::sync::Mutex::new(Box::new(poke))),
            #[cfg(target_os = "macos")]
            child_pid: Arc::clone(&self.child_pid),
            #[cfg(target_os = "macos")]
            stop: Arc::clone(&self.stop),
            #[cfg(target_os = "macos")]
            lookups: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let run = self.probe.run;
        let spawned = std::thread::Builder::new()
            .name("consent-observer".into())
            .spawn(move || run(&sink, mode));
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
        if self.availability.is_live() {
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
        self.settle(now);
        if disconnected && self.availability.is_live() {
            self.availability = ObserverAvailability::Unavailable(UnavailableReason::StreamEnded);
        }
        applied
    }

    /// After a fold: expire stale correlations and settle the availability
    /// the format-drift count implies.
    fn settle(&mut self, now: Instant) {
        self.table.expire(now, PENDING_MAX_AGE);
        // A FORMAT CHANGE DEGRADES TO `unavailable`, NEVER TO A VERDICT. It is
        // not sticky: the count is a run of unparsed lines with nothing ever
        // understood, so the first understood line lifts it again.
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

    /// Fold ONE message.
    fn apply(&mut self, event: ObserverEvent, now: Instant) {
        // Only a macOS stream line is timed; elsewhere nothing reads the clock.
        #[cfg(not(any(target_os = "macos", test)))]
        let _ = now;
        match event {
            #[cfg(any(target_os = "macos", test))]
            ObserverEvent::Started => self.availability = ObserverAvailability::Ok,
            ObserverEvent::Unavailable(reason) => {
                self.availability = ObserverAvailability::Unavailable(reason);
            }
            #[cfg(any(target_os = "macos", test))]
            ObserverEvent::Line(line) => self.table.note(line, now),
            #[cfg(any(target_os = "macos", test))]
            ObserverEvent::Prompt(need) => {
                // Understood, so the drift count stays honest; then queued once
                // per dialog.
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
    /// different things. The tests' non-consuming read of what
    /// [`Self::take_new_verdicts`] announces.
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
                out.push((session, blocked_on_token(&prompt.service)));
            }
        }
        out
    }

    /// THE NEEDS. Every observed consent dialog that is FOR THIS APP (its
    /// subject is `bundle_id`), each once, oldest first. A dialog for any other
    /// subject is dropped: it is not this aterm's need.
    ///
    /// `None` means the running bundle's identity is not warm yet: nothing is
    /// taken and nothing is dropped, so a dialog that arrives before the
    /// identity is known is judged when it is.
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
        if !self.availability.is_live() {
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
        use aterm_containment::consent::Folder;
        let short = self
            .service
            .trim()
            .strip_prefix("kTCCService")
            .unwrap_or(self.service.trim());
        // The folder's own name where the service is one of aterm's, and what
        // the dialog is about in words where aterm knows the service. The
        // service id itself ("AddressBook", "ListenEvent") is no name macOS
        // shows a person: an unknown one is "a permission" here, and stays
        // whole in `blocked_on=`.
        let what = match Folder::ALL.iter().find(|f| f.tcc_service() == short) {
            Some(Folder::AppData) => Some("other applications' data"),
            Some(folder) => Some(folder.label()),
            None => match short {
                "SystemPolicyNetworkVolumes" => Some("network drives"),
                "SystemPolicyRemovableVolumes" => Some("removable drives"),
                "SystemPolicyAllFiles" => Some("all your files"),
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
        };
        let asked = what.map_or_else(
            || "macOS is asking for a permission.".to_string(),
            |what| format!("macOS is asking for access to {what}."),
        );
        AttentionNotice {
            session: self.session,
            title: PROMPT_TITLE,
            body: format!("{asked} The program that asked waits until you answer."),
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
}

/// The one native notification an [`AttentionEvent`] may produce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AttentionNotice {
    /// The originating session, for `notify.rs`'s focus-aware suppression.
    /// `u64::MAX` when the fact is instance-level: no session has that id, so
    /// focus suppresses nothing (session `0` is the first tab).
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

impl AttentionOutcome {
    /// What the host may ACT on in this observer mode. The tab mark and the
    /// native banner repeat what the access card's one question already says,
    /// so they exist only in [`ObserverMode::Full`]; the menu-bar refresh is
    /// the same in every mode.
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

/// The title of a denial ([`AttentionGate`]): macOS refused aterm's own file
/// work under a protected folder.
const DENIED_TITLE: &str = "aterm · file access denied";

/// The title of an observed dialog ([`ObservedPrompt::notice`]): macOS is
/// asking, for any permission — a file, the camera, another app.
const PROMPT_TITLE: &str = "aterm · permission waiting";

/// THE RATE LIMIT (§3.6): one notification, never one per `EPERM`.
///
/// A denial storm is one notification. Instance-owned like everything else
/// here: a fresh `App` starts with the gate armed.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AttentionGate {
    /// Whether the one notification has been spent.
    spent: bool,
}

impl AttentionGate {
    /// A fresh, armed gate.
    pub(crate) const fn new() -> Self {
        Self { spent: false }
    }

    /// Whether the notification has been spent.
    #[cfg(all(test, unix))]
    pub(crate) const fn is_spent(&self) -> bool {
        self.spent
    }

    /// THE FOLD. One event to one outcome.
    pub(crate) fn note(&mut self, event: AttentionEvent) -> AttentionOutcome {
        match event {
            AttentionEvent::ProtectedEperm { session } => self.note_eperm(session),
        }
    }

    /// An observed `EPERM` under a protected root. The tab mark is raised every
    /// time (it is per-tab and idempotent); the notification is spent once.
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
                session: session.unwrap_or(u64::MAX),
                title: DENIED_TITLE,
                body: format!(
                    "Turn on aterm in {} (+ adds a missing app).",
                    crate::menu::privacy_settings_path_words(
                        crate::menu::PrivacyPane::FullDiskAccess
                    )
                ),
            }),
            refresh_status_item: true,
        }
    }
}

impl crate::App {
    /// The `privacy` verb's `observer log=` value: a read of the observer's
    /// state, never a start. It lives here so that no control module names the
    /// observer (`consent_retire`'s fence).
    pub(crate) fn privacy_observer_log(&self) -> &'static str {
        self.consent_observer.availability().as_str()
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
    pub(crate) fn feed_for_test(&mut self, event: ObserverEvent, now: Instant) {
        self.apply(event, now);
        self.settle(now);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// The module's SHIPPING source: everything before the first TOP-LEVEL
    /// `#[cfg(test)]` attribute, which is the test-seam block's (an indented one
    /// gates a single test-only method and ends nothing). Mirrors
    /// `tools/grep_guard.sh`'s `np_strip`, and it is what lets the scans below
    /// name the very patterns they are forbidding.
    fn shipping_source() -> &'static str {
        const SOURCE: &str = include_str!("consent_observer.rs");
        let marker = "\n#[cfg(test)]\n";
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

    /// A FRESH instance has asked nothing, spawned nothing and holds the arm
    /// that cannot reach the OS (the sentinel default is the config's; nothing
    /// starts until the host starts it).
    #[test]
    fn a_fresh_observer_is_off_and_inert() {
        let observer = ObserverState::default();
        assert_eq!(observer.availability(), ObserverAvailability::Off);
        assert!(!observer.probe_is_live());
        assert!(!observer.is_live());
        assert_eq!(observer.pending_len(), 0);
    }

    /// `[privacy] observer = false` (the `Off` mode) spawns NOTHING and stays
    /// `off`.
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

    /// The inert arm reports `unavailable(inert)` — it does not claim there is
    /// no prompt, it says it did not look.
    #[test]
    fn the_inert_arm_degrades_to_unavailable_and_never_to_a_verdict() {
        let mut observer = ObserverState::inert();
        assert_eq!(
            observer.start(ObserverMode::Full, || {}),
            StartOutcome::Started
        );
        // The worker is detached; spin briefly for its one message rather than
        // joining anything (this module owns no handle to join).
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

    /// A second start while a stream is live is a no-op — one child, never two.
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
        assert_eq!(notice.title, PROMPT_TITLE, "a question, not a denial");
        assert_eq!(
            notice.body,
            "macOS is asking for access to Documents. The program that asked waits until \
             you answer.",
            "the body names the folder in words"
        );
        for (service, what) in [
            ("kTCCServiceSystemPolicyNetworkVolumes", "network drives"),
            (
                "kTCCServiceSystemPolicyRemovableVolumes",
                "removable drives",
            ),
            ("kTCCServiceAddressBook", "your contacts"),
            ("kTCCServiceListenEvent", "your keyboard input"),
            ("kTCCServiceAppleEvents", "another app"),
        ] {
            let prompt = ObservedPrompt {
                session: 4,
                blocked_on: String::new(),
                service: service.to_owned(),
            };
            assert_eq!(
                prompt.notice().body,
                format!(
                    "macOS is asking for access to {what}. The program that asked waits until \
                     you answer."
                ),
                "{service} is named in words where aterm knows it"
            );
        }
        // A service aterm does not know is never printed as its id, which
        // stays whole in `blocked_on=`.
        let unknown = ObservedPrompt {
            session: 4,
            blocked_on: blocked_on_token("kTCCServiceSomethingNew"),
            service: "kTCCServiceSomethingNew".to_owned(),
        };
        assert_eq!(
            unknown.notice().body,
            "macOS is asking for a permission. The program that asked waits until you answer."
        );
        assert_eq!(unknown.blocked_on, "macos-permission:SomethingNew");

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

    /// `log stream` prints two header lines on STDOUT at the start of every
    /// generation; the first echoes the predicate and so contains
    /// `AUTHREQ_PROMPTING`. Neither is a message: a machine that never sees a
    /// dialog must not drift to `format-unrecognized` after a day of rotations.
    #[test]
    fn the_stream_header_lines_are_not_messages_and_never_count_as_drift() {
        let filtering = "Filtering the log data using \"process == \"tccd\" AND \
            composedMessage CONTAINS \"AUTHREQ_PROMPTING\"\"";
        let columns = "Timestamp                       Thread     Type        Activity             \
            PID    TTL  ";
        assert_eq!(parse_line(filtering), None);
        assert_eq!(parse_line(columns), None);
        // The same guard covers a bare mention that is not `AUTHREQ_<KIND>:`.
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

    /// The degradation is not sticky: once a line IS understood, the stream is
    /// speaking a format this parser reads, so `unavailable` lifts.
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

    /// ONE NOTIFICATION PER PROCESS, NOT ONE PER EPERM. An agent
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

    /// THE TAB MARK AND THE BANNER EXIST ONLY IN FULL MODE. The gate's outcome
    /// is passed through `for_mode` before the host acts on it: the sentinel
    /// and an off observer post no banner and mark no tab (the access card's
    /// one question says it), a full observer does, and the menu-bar refresh is
    /// the same in every mode.
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

    /// An instance-level denial (the warm-up) marks no tab — there is no session
    /// it belongs to — but it still notifies once.
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
        assert_eq!(notice.title, "aterm · file access denied");
        assert_eq!(
            notice.body,
            format!(
                "Turn on aterm in {} (+ adds a missing app).",
                crate::menu::privacy_settings_path_words(crate::menu::PrivacyPane::FullDiskAccess)
            )
        );
    }

    // -----------------------------------------------------------------------
    // THE SENTINEL (C6): the dialog itself
    // -----------------------------------------------------------------------

    /// `AUTHREQ_PROMPTING` lines RECONSTRUCTED from what `/usr/bin/log stream`
    /// showed on this machine on 2026-09-28 (26.6.2): the field grammar
    /// (`msgID=`, `service=`, `subject=Sub:{..}Resp:{TCCDProcess: .., pid=..}`) is
    /// the recorded one; the timestamps, thread and activity ids and the
    /// anonymized paths are NOT verbatim (the retained log no longer holds the
    /// original lines), so these pin the parser to the grammar, not to a
    /// capture. The first models a program of a LIVE tab (`Resp:` is aterm's
    /// window); the second and third programs of a tab that survived an update
    /// (`Resp:` is the accessing program itself, a different pid each). All
    /// three are the app-data dialog for `com.aterm.aterm`.
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

    /// A dialog for another app's subject, and for a service that is not
    /// app-data: both parse (they ARE dialogs) and neither is our need.
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

    /// A dialog missing any field is `Unparsed` (feeding the format-drift
    /// limit), never a guess: no subject, no responsible pid, no service, no id.
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

    /// The sentinel's predicate is narrow and names `tccd`; the full stream's
    /// is the whole daemon. Both go through `--info`; only the sentinel is
    /// bounded by `--timeout`.
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

    /// Every sentinel generation ends by itself (`--timeout 15m`), so an
    /// orphan of an in-place update lives at most fifteen minutes.
    #[test]
    fn the_sentinel_stream_is_bounded_by_a_timeout_and_rotated_on_eof() {
        let args = stream_args(ObserverMode::Sentinel);
        let at = args.iter().position(|a| a == "--timeout").unwrap();
        assert_eq!(args[at + 1], SENTINEL_TIMEOUT);
        assert_eq!(SENTINEL_TIMEOUT, "15m");
        assert!(ROTATION_MIN < Duration::from_secs(15 * 60));
        assert!(ROTATION_MIN >= Duration::from_secs(14 * 60));
        // The lookup is bounded too, and cannot be steered by a log line.
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
        // Any number of full-length generations rotate; none is charged.
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
        // The FULL stream has no timeout: any end is an end.
        assert_eq!(
            policy.on_end(ObserverMode::Full, Duration::from_secs(20 * 60), t0),
            StreamEnd::GiveUp
        );
        // And a state fed a rotation stays `ok`: only `Ended` retires it.
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
        // A rotation in between does not refund the budget, and the hour does.
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

    /// One dialog is one need, however often its line repeats; a dialog for
    /// another subject is dropped; and nothing is taken (or dropped) until the
    /// running bundle's identity is known.
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
        // A repeat of an already-taken dialog is still the same dialog.
        observer.feed_for_test(prompting("69745.190", "com.aterm.aterm", 74756), now);
        assert!(observer.take_prompting(Some("com.aterm.aterm")).is_empty());
        // A dev build's subject is its own bundle id.
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

    /// The lookup's output names the accessing process for the dialog's msgID;
    /// the responsible block (aterm's window) is never mistaken for it.
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

    /// A process is placed in a tab by ANCESTRY: its own pid or group, then
    /// each parent (at most 32 levels); anything else stays unresolved.
    #[test]
    fn a_program_is_placed_in_its_tab_by_ancestry() {
        let sessions = [(1_u64, 100), (2, 200)];
        // 500 -> 400 -> 300 -> 200 (tab 2's shell).
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
        // The group rule still applies at any level.
        let grouped = |pid: i32| (pid == 300).then_some(100);
        assert_eq!(session_of_pid(500, &sessions, parent, grouped), Some(1));
        // A cycle cannot loop forever.
        let cyclic = |pid: i32| Some(if pid == 7 { 8 } else { 7 });
        assert_eq!(session_of_pid(7, &sessions, cyclic, no_group), None);
    }

    // -----------------------------------------------------------------------
    // THE FENCES
    // -----------------------------------------------------------------------

    /// `log` is a ZSH BUILTIN that shadows `/usr/bin/log`, exits 0 and prints
    /// nothing — the artifact that produced a false "tccd is invisible"
    /// finding. The tool is named absolutely, here and nowhere else.
    ///
    /// Unix-only: `/usr/bin/log` is macOS's, and on Windows `Path::is_absolute`
    /// wants a drive letter, so the absoluteness half cannot be asked there.
    #[cfg(unix)]
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
        assert!(
            code.contains("LaneCommand::new(LOG_TOOL)") && !code.contains("std::process::Command"),
            "and it is spawned holding its stdout and nothing else (`spawnfd::spawn`)"
        );
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
}
