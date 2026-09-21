// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PRESENCE — a window shows who is driving it (round 19, the first slice).
//!
//! Two surfaces, one model. The RIM is a colour-only inset border painted through
//! the same overlay pass the drag-drop target and the upgrade surge use
//! (`app_render::host_visual_state`): teal while a peer's hand is on the
//! keyboard, amber while a human should look (a prompt, a question, a typed
//! escalation), red at a wall (a usage limit) and doubled, with a faint wash,
//! under a hold. The BAND is one chrome row under the tab bar
//! (`status_bars::Lane::Presence`) with six fixed slots in a fixed order —
//! role · phase since · hand · mail · ctx · fabric — so a glance reads left to
//! right and a screen reader gets the same sentence
//! (`accesskit_tree::ChromeMessage::PresenceStatus`).
//!
//! THE LAWS, from the design (r11/presence-spec.md) and binding here:
//!
//! * The rim is CHANGE-DRIVEN. Nothing in this module reads a clock on the frame
//!   path: the per-window view is rebuilt only when a fact changes (a lease, a
//!   hold, mail, a status revision, `meta set`), and the repaint key's
//!   `presence_fp` is exactly `0` on a quiet window — an idle desktop pays
//!   nothing for this feature ([`WindowView::fp`]).
//! * The rim never carries a fact the band does not say in words: [`Rim`] is
//!   derived from [`Level`], and [`Level`] is derived from the same [`Slot`] the
//!   words are composed from. A colour with no sentence is unreachable.
//! * A stalled or lost bridge is an INSTANCE fact: it lives in the band's fabric
//!   slot (`~ 7s`, `✕ lost`) and never colours a session's rim. A session goes
//!   red only on its own `Hold` — which `bridge_lost` applies exactly to the
//!   sessions the bridge touched (`fabric.rs`), so "disconnected" is read from
//!   each session's hold, never inferred.
//! * No message body, command text, OSC title or Limited message text reaches
//!   any surface. The band is agent-readable (`chrome`, `image`), so its text is
//!   wire text: counts, kinds, a sender token, a phase word, a reset time.
//! * Reduced motion: the one animation (a 300 ms edge ripple on a turn submit,
//!   [`crate::motion::MotionEffect::PresenceRipple`]) has amplitude 0 and the
//!   frame is the same image as the steady rim.
//!
//! The agent PHASE (busy / prompt / question / limited / idle / survey) comes
//! from `aterm_phase::worker_phase` over the last [`CLASSIFY_ROWS`] rows of the
//! screen — run ONLY when the session's status revision moves
//! (`session_status.rs`'s own gate), never per frame; the test-only
//! [`classifier_calls`] counter is the gate's proof.

use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_session::SessionId;
use winit::event_loop::EventLoopProxy;

use crate::Wake;
use crate::status_bars::Tone;

// ---------------------------------------------------------------------------
// The wakes: how a change on the control thread reaches the main thread.
// ---------------------------------------------------------------------------

/// The event-loop proxy the control and bridge threads post presence wakes
/// through. Installed once at launch (`install_proxy`); a headless test App
/// installs nothing and drives the model directly, so a post before install is
/// a no-op rather than a lost invariant.
static PROXY: OnceLock<EventLoopProxy<Wake>> = OnceLock::new();

/// Install the proxy the wakes ride. First install wins; later calls are ignored.
pub(crate) fn install_proxy(proxy: EventLoopProxy<Wake>) {
    let _ = PROXY.set(proxy);
}

/// A session's drive lease changed hands — a `turn` began or settled, a
/// cooperative `lease` was acquired or released. `submitted` marks the moment a
/// turn's submit keypress verifiably landed: the one edge the rim ripples on.
pub(crate) fn post_lease_changed(session: &SessionId, submitted: bool) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::LeaseChanged {
            session: session.clone(),
            submitted,
        });
    }
}

/// A session's fabric endpoint changed — a delivery, a hold transition, a post
/// landing, `inbox seen`, a link report. Posted beside every
/// `changed.notify_all()` in `fabric.rs`, so the band moves exactly when a
/// parked `await inbox` would.
pub(crate) fn post_fabric_changed(session: &SessionId) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::FabricChanged {
            session: session.clone(),
        });
    }
}

// ---------------------------------------------------------------------------
// The classifier gate.
// ---------------------------------------------------------------------------

/// How many screen rows the agent-phase classifier reads: the composer frame
/// and the live zone above it fit in the last 40 rows of any screen tall enough
/// to show them, and the whole-screen fallback (no composer frame) reads the
/// same rows a supervisor would.
pub(crate) const CLASSIFY_ROWS: usize = 40;

#[cfg(test)]
thread_local! {
    static CLASSIFIER_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times the agent-phase classifier has run ON THIS THREAD — the
/// count the classifier-gate test compares against status-revision moves.
/// Thread-local because a headless test drives its App on its own thread
/// and the suite runs tests in parallel.
#[cfg(test)]
pub(crate) fn classifier_calls() -> u64 {
    CLASSIFIER_CALLS.with(|c| c.get())
}

/// What the classifier read off one screen: the worker's phase, the context
/// indicator, and whether the session survey is parked above the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentReading {
    pub(crate) phase: AgentPhase,
    pub(crate) context_pct: Option<u8>,
}

/// The worker's phase as the band spells it. `Limited` keeps only the RESET
/// time the notice named — never the notice text (the never-shown law).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentPhase {
    Busy,
    /// An approval box is showing; `detail` is the box's kind and the
    /// classifier's verdict on the command (`bash safe`), never the command.
    Prompt {
        detail: Option<String>,
    },
    Question,
    Limited {
        reset: Option<String>,
        /// When the reset falls, if the notice's time could be placed on this
        /// machine's clock ([`countdown_to_reset`]): the band counts DOWN to
        /// it (the mock: "the band counts down to the reset once a minute").
        /// `None` prints the reset time alone — never a figure that is not
        /// the countdown.
        until: Option<Instant>,
    },
    Idle,
    /// Idle with the session survey parked above the composer.
    Survey,
}

impl AgentPhase {
    /// The phase word the band prints.
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Prompt { .. } => "prompt",
            Self::Question => "question",
            Self::Limited { .. } => "limited",
            Self::Idle => "idle",
            Self::Survey => "survey",
        }
    }
}

/// Classify one screen. The ONLY caller of `aterm_phase` in this crate, so the
/// test counter is total; `rows` is the tail the App cut at [`CLASSIFY_ROWS`].
pub(crate) fn classify(rows: &[String], now: Instant) -> AgentReading {
    #[cfg(test)]
    CLASSIFIER_CALLS.with(|c| c.set(c.get() + 1));
    let phase = match aterm_phase::worker_phase(rows) {
        aterm_phase::Phase::Busy => AgentPhase::Busy,
        aterm_phase::Phase::Prompt => AgentPhase::Prompt {
            detail: aterm_phase::parse_prompt(rows).map(|p| prompt_detail(&p)),
        },
        aterm_phase::Phase::Question => AgentPhase::Question,
        aterm_phase::Phase::Limited { reset, .. } => {
            let until = reset
                .as_deref()
                .and_then(countdown_to_reset)
                .map(|d| now + d);
            AgentPhase::Limited {
                reset: reset.map(|r| sanitize_token(&r, 32)),
                until,
            }
        }
        aterm_phase::Phase::Idle => {
            if aterm_phase::survey_open(rows) {
                AgentPhase::Survey
            } else {
                AgentPhase::Idle
            }
        }
    };
    AgentReading {
        phase,
        context_pct: aterm_phase::context_left(rows),
    }
}

/// The prompt's kind word, and — for a tool approval — the classifier's verdict
/// on it. The command itself is never carried.
fn prompt_detail(p: &aterm_phase::Prompt) -> String {
    sanitize_token(p.kind.name(), 24)
}

// ---------------------------------------------------------------------------
// The reset clock: placing a limit notice's reset time on this machine's clock.
// ---------------------------------------------------------------------------

/// A limit notice's reset time as far as the band can read it: an optional
/// month and day, the hour and minute, and the zone the notice named
/// (`7:30pm (America/Los_Angeles)`, `3am`, `Sep 19 at 11am (…)`, `19:30`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResetSpec {
    pub(crate) date: Option<(u8, u8)>,
    pub(crate) hour: u8,
    pub(crate) minute: u8,
    pub(crate) zone: Option<String>,
}

/// Read a reset time out of the notice's own words. `None` for anything the
/// grammar above does not cover — the band then prints the reset text alone.
pub(crate) fn parse_reset(text: &str) -> Option<ResetSpec> {
    let (body, zone) = match text.find('(') {
        Some(i) => {
            let z = text[i + 1..].trim().trim_end_matches(')').trim();
            (&text[..i], (!z.is_empty()).then(|| z.to_string()))
        }
        None => (text, None),
    };
    let mut month: Option<u8> = None;
    let mut date: Option<(u8, u8)> = None;
    let mut time: Option<(u8, u8)> = None;
    for tok in body.split_whitespace() {
        let t = tok.trim_matches(|c: char| c == ',' || c == '.');
        if t.is_empty() || t.eq_ignore_ascii_case("at") || t.eq_ignore_ascii_case("on") {
            continue;
        }
        if let Some(m) = month_of(t) {
            month = Some(m);
            continue;
        }
        if let Some(m) = month
            && date.is_none()
            && let Ok(d) = t.parse::<u8>()
            && (1..=31).contains(&d)
        {
            date = Some((m, d));
            continue;
        }
        if let Some((h, mm)) = time
            && (t.eq_ignore_ascii_case("am") || t.eq_ignore_ascii_case("pm"))
        {
            time = Some((meridian(h, t.eq_ignore_ascii_case("pm"))?, mm));
            continue;
        }
        if let Some(clock) = parse_clock(t) {
            time = Some(clock);
        }
    }
    let (hour, minute) = time?;
    Some(ResetSpec {
        date,
        hour,
        minute,
        zone,
    })
}

fn month_of(t: &str) -> Option<u8> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    if t.len() < 3 || !t.is_char_boundary(3) {
        return None;
    }
    let head = t[..3].to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|m| *m == head)
        .map(|i| i as u8 + 1)
        .filter(|_| t[3..].chars().all(|c| c.is_ascii_alphabetic()))
}

/// A 12-hour figure onto the 24-hour clock.
fn meridian(h: u8, pm: bool) -> Option<u8> {
    if !(1..=12).contains(&h) {
        return None;
    }
    Some(match (h, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, false) => h,
        (h, true) => h + 12,
    })
}

/// `H`, `H:MM`, `Ham`, `H:MMpm`, `HH:MM` — 24-hour when no meridian.
fn parse_clock(t: &str) -> Option<(u8, u8)> {
    let lower = t.to_ascii_lowercase();
    let (num, pm) = if let Some(n) = lower.strip_suffix("am") {
        (n, Some(false))
    } else if let Some(n) = lower.strip_suffix("pm") {
        (n, Some(true))
    } else {
        (lower.as_str(), None)
    };
    let (h, m) = match num.split_once(':') {
        Some((h, m)) => (h.parse::<u8>().ok()?, m.parse::<u8>().ok()?),
        None => (num.parse::<u8>().ok()?, 0),
    };
    if m > 59 {
        return None;
    }
    let h = match pm {
        Some(pm) => meridian(h, pm)?,
        None if h <= 23 => h,
        None => return None,
    };
    Some((h, m))
}

/// How long until `reset` falls, on THIS machine's clock — the figure the
/// limited row counts down. `None` when the words cannot be read, when the
/// notice names a zone that is not this machine's, or when the reset is
/// already behind us: then the row prints the reset time and no figure.
pub(crate) fn countdown_to_reset(reset: &str) -> Option<Duration> {
    let spec = parse_reset(reset)?;
    if let Some(named) = &spec.zone
        && let Some(local) = local_zone()
        && &local != named
    {
        return None;
    }
    local_countdown(&spec)
}

/// This machine's IANA zone name, when it can be read (`TZ`, else the
/// `/etc/localtime` link); `None` leaves the notice's zone unchecked.
fn local_zone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if !tz.is_empty() {
            return Some(tz.to_string());
        }
    }
    let link = std::fs::read_link("/etc/localtime").ok()?;
    let s = link.to_str()?;
    let i = s.find("zoneinfo/")?;
    Some(s[i + "zoneinfo/".len()..].to_string())
}

/// The countdown on this machine's clock: the reset's civil time at the local
/// UTC offset, against `SystemTime::now()`. A bare clock time is the NEXT such
/// time; a dated reset already behind us is nothing to count down to. The
/// offset is read ONCE from `date +%z` (the one place every aterm crate reads
/// its zone — the `libc` shim declares no `localtime_r`), so a reset on the
/// far side of a daylight-saving switch counts at the offset of today.
fn local_countdown(spec: &ResetSpec) -> Option<Duration> {
    let offset = local_offset_s()?;
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
    )
    .ok()?;
    countdown_at(spec, now, offset)
}

/// [`local_countdown`] with the clock and the offset passed in — pure, so the
/// rollover law is testable at any hour.
fn countdown_at(spec: &ResetSpec, now: i64, offset: i64) -> Option<Duration> {
    let local = now + offset;
    let today = local.div_euclid(86_400);
    let (year, _, _) = aterm_types::rfc3339::civil_from_days(today);
    let tod = i64::from(spec.hour) * 3600 + i64::from(spec.minute) * 60;
    let mut at = match spec.date {
        // A dated reset is THIS year's; one already behind us (a stale
        // notice) is nothing to count down to.
        Some((m, d)) => {
            aterm_types::rfc3339::days_from_civil(year, i64::from(m), i64::from(d)) * 86_400 + tod
        }
        None => today * 86_400 + tod,
    };
    if at <= local {
        if spec.date.is_some() {
            return None;
        }
        // A bare clock time is the NEXT such time.
        at += 86_400;
    }
    Some(Duration::from_secs(u64::try_from(at - local).ok()?))
}

/// The local clock's offset from UTC in seconds, from `date +%z`, read once;
/// `None` where it cannot be run — then no figure is ever printed.
#[cfg(unix)]
fn local_offset_s() -> Option<i64> {
    static OFFSET: OnceLock<Option<i64>> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let out = std::process::Command::new("date")
            .arg("+%z")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_utc_offset(String::from_utf8_lossy(&out.stdout).trim())
    })
}

#[cfg(not(unix))]
fn local_offset_s() -> Option<i64> {
    None
}

/// `+0200` / `-0700` as seconds.
fn parse_utc_offset(z: &str) -> Option<i64> {
    let (sign, digits) = match z.as_bytes().first()? {
        b'+' => (1, &z[1..]),
        b'-' => (-1, &z[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hh: i64 = digits[..2].parse().ok()?;
    let mm: i64 = digits[2..].parse().ok()?;
    Some(sign * (hh * 3600 + mm * 60))
}

// ---------------------------------------------------------------------------
// The facts, gathered by the App from the session's own leaf locks.
// ---------------------------------------------------------------------------

/// Whose hand is on this session's keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hand {
    None,
    /// A peer's `turn` is open (`Lease::Turn`). `holder` names the driver the
    /// lease itself records (`Lease::Turn::driver`: the source of the edge the
    /// turn came over) — its `meta role` if it is a local session with one,
    /// else its short sid — and is `None` for a turn driven over the Owner
    /// token (the CLI, a human at the keyboard of another instance), whatever
    /// write edges happen to stand in the session's table.
    DrivenTurn {
        id: u64,
        holder: Option<String>,
    },
    /// A cooperative drive lease is held (`Lease::Drive`).
    DrivenLease {
        holder: String,
    },
    /// THIS session drives another — its own open `turn` on a peer whose lease
    /// names it: `sid` is the peer's short form.
    Driving {
        sid: String,
    },
}

/// The standing halt, as the band prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HoldFact {
    pub(crate) reason: String,
    /// `origin=fleet`: cannot be lifted from this window.
    pub(crate) fleet: bool,
}

/// The newest inbox row, trust FIRST (`aterm-link`'s rule, in its `render`
/// module since round 21: the receiver's verdict is the first thing a reader
/// sees).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MailLast {
    pub(crate) kind: String,
    pub(crate) from: String,
    /// One of the wire trusts (`agent`, `human`, `unknown`, `forged-self`, …).
    pub(crate) trust: String,
}

/// The mail counts the band prints.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct MailFacts {
    pub(crate) unread: u64,
    pub(crate) pending: u64,
    pub(crate) dropped: u64,
    pub(crate) queued: u64,
    pub(crate) last: Option<MailLast>,
    /// The highest row id ever delivered — how the story counts arrivals.
    pub(crate) head: u64,
}

/// The instance's bridge link, as `status`'s `fabric=` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    Absent,
    Connected {
        rtt_ms: Option<u64>,
    },
    /// A bridge is attached but its broker link is down — for `age_ms`, when
    /// the stall has a date (`crate::fabric::fabric_stalled_ms`: the moment
    /// the link was last reported down after being up, or the dial refused).
    /// `None` for a bridge still dialing since it attached: there is no stall
    /// to date, and the slot prints `~` with no figure rather than `~ 0s`.
    Stalled {
        age_ms: Option<u64>,
    },
    Disconnected,
}

/// The last completed turn on this session's ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TurnFact {
    pub(crate) id: u64,
    pub(crate) settled: bool,
    pub(crate) dur_ms: u64,
    /// The record came from an EARLIER aterm process, carried across a
    /// self-update handoff (`TurnRecord::carried`, `history`'s `carried=1`):
    /// a turn that settled before this process existed. It is the ledger's
    /// baseline, never news — the slot adopts it without a story point or
    /// the Success glow (design §4: the handoff carries the ledger, and the
    /// story is rebuilt from THIS process's own facts).
    pub(crate) carried: bool,
}

/// Everything one refresh reads. Built by the App (`app_presence.rs`) from the
/// session's own leaf locks; pure data so the model is testable without a
/// store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Facts {
    pub(crate) role: Option<String>,
    pub(crate) attention: Option<String>,
    /// The shell status word (`running`, `idle`, `quiet`, …) and when it was
    /// published — the phase the band shows when no agent reading exists.
    pub(crate) shell: Option<(&'static str, Instant)>,
    /// The session status REVISION: the agent classifier runs only when this
    /// moves, and `agent` is `Some` exactly then.
    pub(crate) revision: u64,
    pub(crate) agent: Option<AgentReading>,
    pub(crate) hand: Hand,
    /// When a cooperative drive lease LAPSES (`Lease::Drive`'s TTL): nothing
    /// posts a wake for a lapse, so the model arms this as a deadline and
    /// re-reads the hand when it passes. `None` without such a lease.
    pub(crate) lease_until: Option<Instant>,
    pub(crate) hold: Option<HoldFact>,
    pub(crate) mail: MailFacts,
    pub(crate) link: Link,
    pub(crate) turn: Option<TurnFact>,
}

impl Default for Facts {
    fn default() -> Self {
        Self {
            role: None,
            attention: None,
            shell: None,
            revision: 0,
            agent: None,
            hand: Hand::None,
            lease_until: None,
            hold: None,
            mail: MailFacts::default(),
            link: Link::Absent,
            turn: None,
        }
    }
}

// ---------------------------------------------------------------------------
// The story: what happened since the human last looked.
// ---------------------------------------------------------------------------

/// The story ring's capacity (design §4: `Ring<256, …>`).
const STORY_CAP: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoryVerb {
    Turn,
    /// A turn whose settle deadline passed (`status=timeout` on the ledger):
    /// counted with the turns, and said — a busy worker with a timed-out turn
    /// never reads `◇ quiet`.
    TurnTimedOut,
    Mail,
    Hold,
    Limited,
    Question,
    /// The watcher approved a read (`ctl story approved`).
    Approval,
    /// A stop (hold / limited) ended.
    Resumed,
    // The rest of the CLOSED SET `aterm ctl story <verb>` accepts (design §5):
    // what the watcher decided or saw, which reaches the GUI no other way.
    /// `ctl story dismissed` — the session survey was dismissed for the human.
    Dismissed,
    /// `ctl story reconnected` — the watcher rode out an outage and is back.
    Reconnected,
    /// `ctl story timeout` — the watcher's budget ran out.
    Timeout,
    /// `ctl story exit` — the watcher's loop ended.
    Exit,
    /// `ctl story compacted` — the worker compacted its context.
    Compacted,
    /// `ctl story warned` — the watcher warned (context running low).
    Warned,
}

impl StoryVerb {
    /// The closed set of words `aterm ctl story <verb>` accepts, in the order
    /// the usage line prints them. A word outside it is a usage error, never a
    /// free-text story point.
    pub(crate) const TOLD_WORDS: [&'static str; 7] = [
        "approved",
        "dismissed",
        "reconnected",
        "timeout",
        "exit",
        "compacted",
        "warned",
    ];

    /// The verb a `ctl story <word>` names, `None` outside the closed set.
    pub(crate) fn parse_told(word: &str) -> Option<Self> {
        Some(match word {
            "approved" => Self::Approval,
            "dismissed" => Self::Dismissed,
            "reconnected" => Self::Reconnected,
            "timeout" => Self::Timeout,
            "exit" => Self::Exit,
            "compacted" => Self::Compacted,
            "warned" => Self::Warned,
            _ => return None,
        })
    }

    /// The word the band prints for a TOLD verb (the wire word), with the
    /// glyph the mock pairs it with: `✓ approved`.
    pub(crate) const fn told_words(self) -> Option<(char, &'static str)> {
        Some(match self {
            Self::Approval => ('\u{2713}', "approved"),
            Self::Dismissed => ('\u{2713}', "dismissed"),
            Self::Reconnected => ('\u{27df}', "reconnected"),
            Self::Timeout => ('\u{2715}', "timeout"),
            Self::Exit => ('\u{2715}', "exit"),
            Self::Compacted => ('\u{25c7}', "compacted"),
            Self::Warned => ('\u{26a0}', "warned"),
            Self::Turn
            | Self::TurnTimedOut
            | Self::Mail
            | Self::Hold
            | Self::Limited
            | Self::Question
            | Self::Resumed => return None,
        })
    }
}

/// How long a told story point stands in the PHASE slot (`✓ approved`) before
/// the slot returns to the phase (the mock: "three seconds after the watcher
/// approves it, the slot reads ✓ approved, then returns to phase").
pub(crate) const TOLD_FLASH: Duration = Duration::from_secs(3);

/// The longest text `ctl story` carries, in bytes — the wire cap, checked on
/// the control thread before any wake.
pub(crate) const TOLD_TEXT_MAX_BYTES: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StoryPoint {
    pub(crate) seq: u64,
    pub(crate) at: Instant,
    pub(crate) verb: StoryVerb,
}

// ---------------------------------------------------------------------------
// The per-session slot.
// ---------------------------------------------------------------------------

/// A session's presence, as the last refresh left it.
#[derive(Clone, Debug)]
pub(crate) struct Slot {
    /// The status revision the agent reading was classified at; `None` before
    /// the first classification. THE GATE: the classifier runs again only when
    /// the revision moves past this — never per refresh, never per frame.
    pub(crate) revision_seen: Option<u64>,
    pub(crate) role: Option<String>,
    pub(crate) attention: Option<String>,
    pub(crate) shell: Option<(&'static str, Instant)>,
    pub(crate) agent: Option<AgentReading>,
    /// When the AGENT phase word last changed — the band's `since` for it.
    pub(crate) agent_since: Instant,
    pub(crate) hand: Hand,
    /// When the cooperative lease behind `hand` lapses (see
    /// [`Facts::lease_until`]); the App's timer re-reads the hand then.
    pub(crate) lease_until: Option<Instant>,
    pub(crate) hold: Option<HoldFact>,
    pub(crate) mail: MailFacts,
    pub(crate) link: Link,
    pub(crate) turn: Option<TurnFact>,
    /// When the last turn SETTLED (the Success tone's 2 s window).
    pub(crate) settled_at: Option<Instant>,
    /// When a stop (hold or limit) began, for the story's stop duration.
    /// `None` under a hold the slot first saw at its own mint: nothing in a
    /// `Hold` says when it began, so the band prints no figure for it
    /// rather than dating it from the mint.
    stop_began: Option<(StoryVerb, Instant)>,
    /// Whether a refresh has been absorbed yet. The FIRST absorb is the
    /// baseline — what already stood when the slot was minted — and a fact
    /// already standing then (a hold) is adopted as state, not narrated as
    /// something that happened since the human last looked.
    baselined: bool,
    story: VecDeque<StoryPoint>,
    /// The seq of the newest story point; 0 = nothing ever happened.
    pub(crate) story_seq: u64,
    /// The last TOLD point (`ctl story`), with its text: the phase slot reads
    /// it for [`TOLD_FLASH`] after `at`, then returns to the phase.
    told: Option<(StoryVerb, String, Instant)>,
}

impl Slot {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            revision_seen: None,
            role: None,
            attention: None,
            shell: None,
            agent: None,
            agent_since: now,
            hand: Hand::None,
            lease_until: None,
            hold: None,
            mail: MailFacts::default(),
            link: Link::Absent,
            turn: None,
            settled_at: None,
            stop_began: None,
            baselined: false,
            story: VecDeque::new(),
            story_seq: 0,
            told: None,
        }
    }

    /// `aterm ctl story <verb> [<text>]` landed: one story point, and the
    /// phase slot reads the verb for [`TOLD_FLASH`]. `text` is the wire text
    /// (already bounded); it is sanitized for cells here like every other
    /// free-text value. Returns the point's seq.
    pub(crate) fn tell(&mut self, verb: StoryVerb, text: &str, now: Instant) -> u64 {
        self.note(verb, now);
        self.told = Some((verb, sanitize_token(text, 48), now));
        self.story_seq
    }

    /// The told point still standing in the phase slot at `now`.
    fn told_now(&self, now: Instant) -> Option<(StoryVerb, &str)> {
        self.told
            .as_ref()
            .filter(|(_, _, at)| now.saturating_duration_since(*at) < TOLD_FLASH)
            .map(|(v, t, _)| (*v, t.as_str()))
    }

    /// When the phase slot returns from a told point to the phase — the one
    /// deadline a story post adds (there is no other clock).
    pub(crate) fn told_deadline(&self, now: Instant) -> Option<Instant> {
        self.told
            .as_ref()
            .map(|(_, _, at)| *at + TOLD_FLASH)
            .filter(|end| *end > now)
    }

    fn note(&mut self, verb: StoryVerb, now: Instant) {
        self.story_seq += 1;
        if self.story.len() >= STORY_CAP {
            self.story.pop_front();
        }
        self.story.push_back(StoryPoint {
            seq: self.story_seq,
            at: now,
            verb,
        });
    }

    /// Fold one refresh's facts in. Returns what CHANGED: whether anything the
    /// view reads moved, and whether a turn was just SUBMITTED (the ripple's
    /// edge, taken from the wake rather than inferred).
    pub(crate) fn absorb(&mut self, facts: Facts, now: Instant) -> bool {
        let mut changed = false;
        if self.role != facts.role {
            self.role = facts.role;
            changed = true;
        }
        if self.attention != facts.attention {
            self.attention = facts.attention;
            changed = true;
        }
        if self.shell.map(|s| s.0) != facts.shell.map(|s| s.0) {
            changed = true;
        }
        self.shell = facts.shell;
        if let Some(agent) = facts.agent {
            self.revision_seen = Some(facts.revision);
            let word_moved =
                self.agent.as_ref().map(|a| a.phase.word()) != Some(agent.phase.word());
            if word_moved {
                self.agent_since = now;
                match agent.phase {
                    AgentPhase::Question => self.note(StoryVerb::Question, now),
                    AgentPhase::Limited { .. } => {
                        self.note(StoryVerb::Limited, now);
                        self.stop_began = Some((StoryVerb::Limited, now));
                    }
                    _ => {}
                }
                if matches!(
                    self.agent.as_ref().map(|a| &a.phase),
                    Some(AgentPhase::Limited { .. })
                ) && !matches!(agent.phase, AgentPhase::Limited { .. })
                {
                    self.note(StoryVerb::Resumed, now);
                }
            }
            if self.agent.as_ref() != Some(&agent) {
                changed = true;
            }
            self.agent = Some(agent);
        }
        if self.hand != facts.hand {
            self.hand = facts.hand;
            changed = true;
        }
        // A renewed lease moves its deadline without moving a word.
        self.lease_until = facts.lease_until;
        if self.hold != facts.hold {
            match (&self.hold, &facts.hold) {
                (None, Some(_)) if self.baselined => {
                    self.note(StoryVerb::Hold, now);
                    self.stop_began = Some((StoryVerb::Hold, now));
                }
                // Standing at the mint: the hold is state, its age unknown.
                (None, Some(_)) => self.stop_began = None,
                (Some(_), None) => self.note(StoryVerb::Resumed, now),
                _ => {}
            }
            self.hold = facts.hold;
            changed = true;
        }
        if self.mail != facts.mail {
            if facts.mail.head > self.mail.head {
                self.note(StoryVerb::Mail, now);
            }
            self.mail = facts.mail;
            changed = true;
        }
        if self.link != facts.link {
            self.link = facts.link;
            changed = true;
        }
        if self.turn != facts.turn {
            // A CARRIED record settled in a previous process: the baseline,
            // not news (see [`TurnFact::carried`]).
            if let Some(t) = facts.turn
                && !t.carried
                && self.turn.is_none_or(|old| old.id < t.id)
            {
                if t.settled {
                    self.note(StoryVerb::Turn, now);
                    self.settled_at = Some(now);
                } else {
                    self.note(StoryVerb::TurnTimedOut, now);
                }
            }
            self.turn = facts.turn;
            changed = true;
        }
        self.baselined = true;
        changed
    }

    /// The severity this slot stands at (the rim and the chip follow it).
    pub(crate) fn level(&self, watermark: u64) -> Level {
        if self.hold.is_some() {
            return Level::Hold;
        }
        if matches!(
            self.agent.as_ref().map(|a| &a.phase),
            Some(AgentPhase::Limited { .. })
        ) {
            return Level::Limited;
        }
        if self.attention.is_some()
            || matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Prompt { .. } | AgentPhase::Question)
            )
        {
            return Level::Attention;
        }
        match &self.hand {
            Hand::DrivenTurn { .. } | Hand::DrivenLease { .. } => return Level::Driven,
            Hand::Driving { .. } => return Level::Driving,
            Hand::None => {}
        }
        // An unread task or ask is a wait state only while no hand is on the
        // session: a driven worker with mail waiting stays teal (the mock's
        // first row), and the mail slot says the rest.
        if self.unread_wants_a_human() {
            return Level::Attention;
        }
        // A story outranks waiting mail: "something happened since you
        // looked" is the glance fact, and the mail slot prints the mail.
        if self.story_seq > watermark {
            return Level::Story;
        }
        if self.mail.unread > 0
            || self.mail.queued > 0
            || self.mail.dropped > 0
            || matches!(self.link, Link::Stalled { .. } | Link::Disconnected)
        {
            return Level::Note;
        }
        Level::Quiet
    }

    /// An unread `task` or `ask` is addressed to someone: a wait state.
    fn unread_wants_a_human(&self) -> bool {
        self.mail.unread > 0
            && self
                .mail
                .last
                .as_ref()
                .is_some_and(|l| l.kind == "task" || l.kind == "ask")
    }

    /// CALM: nothing is happening that the human's next keystroke should not
    /// fold away — the fold law's second conjunct.
    pub(crate) fn calm(&self) -> bool {
        self.hold.is_none()
            && matches!(self.hand, Hand::None)
            && self.attention.is_none()
            && !matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Prompt { .. } | AgentPhase::Question | AgentPhase::Limited { .. })
            )
            && !self.unread_wants_a_human()
    }

    /// The tone the band paints in: Warn while a human should look, Success for
    /// [`SETTLED_GLOW`] after a settled turn, else Info.
    pub(crate) fn tone(&self, now: Instant, watermark: u64) -> Tone {
        if self.level(watermark) >= Level::Attention {
            return Tone::Warn;
        }
        if self
            .settled_at
            .is_some_and(|t| now.saturating_duration_since(t) < SETTLED_GLOW)
        {
            return Tone::Success;
        }
        Tone::Info
    }

    /// The story points after `watermark`, oldest first.
    pub(crate) fn story_since(&self, watermark: u64) -> impl Iterator<Item = &StoryPoint> {
        self.story.iter().filter(move |p| p.seq > watermark)
    }

    /// The current stop (hold / limit) in progress, for the story's summary.
    fn stop_in_progress(&self) -> Option<(StoryVerb, Instant)> {
        self.stop_began.filter(|(verb, _)| match verb {
            StoryVerb::Hold => self.hold.is_some(),
            _ => matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Limited { .. })
            ),
        })
    }
}

/// How long the Success tone stands after a settled turn.
pub(crate) const SETTLED_GLOW: Duration = Duration::from_secs(2);

/// Severity, ascending. `hold > limited > attention > driven > story > quiet`
/// (design §1), with the rim-less states between: mail waiting (`Note`)
/// sits under a story, and driving another session under being driven.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Quiet,
    /// Mail waiting, a queued post, a stalled bridge: information, no rim.
    Note,
    /// Something happened since the human last looked; the session is calm.
    Story,
    /// This session drives another.
    Driving,
    /// A peer's hand is on this keyboard.
    Driven,
    /// A human should look: prompt, question, escalation, an unread task.
    Attention,
    Limited,
    Hold,
}

impl Level {
    /// The rim this level paints — colour only, and only for the levels whose
    /// band sentence says why.
    pub(crate) const fn rim(self) -> Rim {
        match self {
            Self::Quiet | Self::Story | Self::Note | Self::Driving => Rim::None,
            Self::Driven => Rim::Drive,
            Self::Attention => Rim::Wait,
            Self::Limited => Rim::Stop { hold: false },
            Self::Hold => Rim::Stop { hold: true },
        }
    }

    /// The tab chip's mark: a hollow diamond to wait, a filled one at a stop, a
    /// dot for a story.
    pub(crate) const fn chip(self) -> ChipLevel {
        match self {
            Self::Quiet | Self::Note | Self::Driving | Self::Driven => ChipLevel::Off,
            Self::Story => ChipLevel::Story,
            Self::Attention => ChipLevel::Wait,
            Self::Limited | Self::Hold => ChipLevel::Stop,
        }
    }

    /// Whether the band row EXISTS for this level (the fold law's first half:
    /// state ≠ quiet). `Story` counts — it is the row the summary lives in.
    pub(crate) const fn shows_row(self) -> bool {
        !matches!(self, Self::Quiet)
    }

    /// The wire word `status level=` and `chrome` print: the variant's name in
    /// lower case, a closed set a reader can match on.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Note => "note",
            Self::Story => "story",
            Self::Driving => "driving",
            Self::Driven => "driven",
            Self::Attention => "attention",
            Self::Limited => "limited",
            Self::Hold => "hold",
        }
    }
}

impl Rim {
    /// The wire word `chrome` prints for the rim.
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Drive => "drive",
            Self::Wait => "wait",
            Self::Stop { hold: false } => "stop",
            Self::Stop { hold: true } => "stop-hold",
        }
    }
}

impl Hand {
    /// The `status hand=` token: `-` | `turn:<id>[:<holder>]` | `lease:<holder>`
    /// | `driving:<sid>`, the free-text part percent-encoded so the record
    /// stays one line of `key=value` words whatever a holder is called.
    pub(crate) fn wire(&self) -> String {
        use aterm_control::wire::pct_encode;
        match self {
            Self::None => "-".to_string(),
            Self::DrivenTurn { id, holder: None } => format!("turn:{id}"),
            Self::DrivenTurn {
                id,
                holder: Some(h),
            } => format!("turn:{id}:{}", pct_encode(h)),
            Self::DrivenLease { holder } => format!("lease:{}", pct_encode(holder)),
            Self::Driving { sid } => format!("driving:{}", pct_encode(sid)),
        }
    }
}

/// The rim's colour state. `Stop { hold }` doubles the thickness and adds the
/// wash under a hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Rim {
    #[default]
    None,
    Drive,
    Wait,
    Stop {
        hold: bool,
    },
}

/// The tab chip's attention mark as a LEVEL (the chip's old `attention: bool`
/// is `Wait`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub(crate) enum ChipLevel {
    #[default]
    Off,
    Story,
    Wait,
    Stop,
}

impl ChipLevel {
    /// The chrome-state tokens the introspection line prints for this mark.
    pub(crate) const fn chrome_states(self) -> &'static [&'static str] {
        match self {
            Self::Off => &[],
            Self::Story => &["story"],
            Self::Wait => &["attention"],
            Self::Stop => &["attention", "stop"],
        }
    }

    /// The hover-help clause.
    pub(crate) const fn help(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Story => Some("Something happened while you were away"),
            Self::Wait => Some("Needs attention"),
            Self::Stop => Some("Stopped — held or at a limit"),
        }
    }
}

// ---------------------------------------------------------------------------
// The words.
// ---------------------------------------------------------------------------

/// The six slots, composed. Each is a whole string; [`Words::fit`] lays them
/// out at a width and sheds from the right in the design's order.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Words {
    pub(crate) role: String,
    pub(crate) phase: String,
    /// The `since` clauses, in the order they are printed; elided shortest
    /// first when the row is narrow.
    pub(crate) since: Vec<String>,
    pub(crate) hand: String,
    /// The hand without its detail (`◂ turn 41` for `◂ manager · turn 41`,
    /// `⊘ hold` for `⊘ hold <reason>`): what survives past the ctx slot on a
    /// narrow row, so the hand is never cut mid-word.
    pub(crate) hand_short: String,
    pub(crate) mail: String,
    /// The mail counts alone (`✉2 ↑1`), without `kind←✓from`.
    pub(crate) mail_short: String,
    pub(crate) ctx: String,
    pub(crate) fabric: String,
    /// The a11y sentence.
    pub(crate) sentence: String,
    pub(crate) tone: Tone,
}

/// Two spaces between slots, one between a glyph and its value.
const SLOT_GAP: &str = "  ";
/// The joint between since-clauses.
const CLAUSE_SEP: &str = " \u{00b7} ";
/// The empty-slot mark.
const DASH: &str = "\u{2014}";

/// Which slot a laid-out piece belongs to, so the painter can colour it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotKind {
    Role,
    Phase,
    Since,
    Hand,
    Mail,
    Ctx,
    Fabric,
}

impl Words {
    /// Lay the slots out at `cols` cells. Elision right→left: the fabric slot
    /// (`rtt`), then since-clauses shortest first (the longest stop survives),
    /// then the role, then ctx; then the mail's `kind←from` and the hand's
    /// detail fold to their short forms, so hand, phase and mail survive to
    /// 24 columns — past which the line is cut.
    pub(crate) fn fit(&self, cols: usize) -> String {
        let pieces = self.pieces(cols);
        join_pieces(&pieces)
    }

    /// The pieces [`Self::fit`] keeps at `cols`, in order, each tagged with
    /// its slot: a `Since` piece follows its `Phase` by one space, every other
    /// piece follows its predecessor by two.
    pub(crate) fn pieces(&self, cols: usize) -> Vec<(SlotKind, String)> {
        let mut since = self.since.clone();
        let mut fabric = true;
        let mut role = true;
        let mut ctx = true;
        let mut mail_full = true;
        let mut hand_full = true;
        loop {
            let pieces = self.compose(&since, role, ctx, fabric, hand_full, mail_full);
            if width(&join_pieces(&pieces)) <= cols {
                return pieces;
            }
            if fabric {
                fabric = false;
                continue;
            }
            // The since-clauses, shortest first — but the stop clause (the
            // longest stop) outlives everything below and goes last of all.
            let is_stop = |c: &String| {
                c.starts_with("limited") || c.starts_with("held") || c.starts_with('\u{2192}')
            };
            if let Some(i) = since
                .iter()
                .enumerate()
                .filter(|(_, c)| !is_stop(c))
                .min_by_key(|(_, c)| width(c))
                .map(|(i, _)| i)
            {
                since.remove(i);
                continue;
            }
            if role {
                role = false;
                continue;
            }
            if ctx {
                ctx = false;
                continue;
            }
            if mail_full && self.mail_short != self.mail {
                mail_full = false;
                continue;
            }
            if hand_full && self.hand_short != self.hand {
                hand_full = false;
                continue;
            }
            if !since.is_empty() {
                since.clear();
                continue;
            }
            // Nothing left to shed: cut the survivors at the width — the
            // MAIL keeps its cells (it is the rightmost survivor and the
            // shortest), the hand gives way first, then the phase, so all
            // three are on the row down to 13 columns.
            if pieces.len() == 3 {
                let (phase, hand, mail) = (&pieces[0].1, &pieces[1].1, &pieces[2].1);
                let (pw, hw, mw) = (width(phase), width(hand), width(mail));
                if pw + hw + mw + 4 > cols && cols >= mw + 4 + 3 + 4 {
                    let hand_room = (cols - mw - 4).saturating_sub(pw).max(3);
                    let hand_cut = truncate(hand, hand_room.min(hw));
                    let phase_room = cols - mw - 4 - width(&hand_cut);
                    let phase_cut = truncate(phase, phase_room.min(pw));
                    return vec![
                        (SlotKind::Phase, phase_cut),
                        (SlotKind::Hand, hand_cut),
                        (SlotKind::Mail, mail.clone()),
                    ];
                }
            }
            let mut out = Vec::new();
            let mut used = 0usize;
            for (i, (kind, text)) in pieces.into_iter().enumerate() {
                let gap = if i == 0 {
                    0
                } else if kind == SlotKind::Since {
                    1
                } else {
                    2
                };
                if used + gap >= cols {
                    break;
                }
                let room = cols - used - gap;
                let cut = truncate(&text, room);
                used += gap + width(&cut);
                out.push((kind, cut));
            }
            return out;
        }
    }

    fn compose(
        &self,
        since: &[String],
        role: bool,
        ctx: bool,
        fabric: bool,
        hand_full: bool,
        mail_full: bool,
    ) -> Vec<(SlotKind, String)> {
        let mut out = Vec::with_capacity(7);
        if role && !self.role.is_empty() {
            out.push((SlotKind::Role, self.role.clone()));
        }
        out.push((SlotKind::Phase, self.phase.clone()));
        if !since.is_empty() {
            out.push((SlotKind::Since, since.join(CLAUSE_SEP)));
        }
        out.push((
            SlotKind::Hand,
            if hand_full {
                &self.hand
            } else {
                &self.hand_short
            }
            .clone(),
        ));
        out.push((
            SlotKind::Mail,
            if mail_full {
                &self.mail
            } else {
                &self.mail_short
            }
            .clone(),
        ));
        if ctx && !self.ctx.is_empty() {
            out.push((SlotKind::Ctx, self.ctx.clone()));
        }
        if fabric && !self.fabric.is_empty() {
            out.push((SlotKind::Fabric, self.fabric.clone()));
        }
        out
    }
}

/// Join laid-out pieces into the printed line: one space before a `Since`
/// piece, two before every other.
pub(crate) fn join_pieces(pieces: &[(SlotKind, String)]) -> String {
    let mut out = String::new();
    for (i, (kind, text)) in pieces.iter().enumerate() {
        if i > 0 {
            out.push_str(if *kind == SlotKind::Since {
                " "
            } else {
                SLOT_GAP
            });
        }
        out.push_str(text);
    }
    out
}

/// Display width in cells (every glyph the band uses is one cell wide; the
/// text-presentation rule in the painter keeps ✓ and ⚠ that way).
fn width(s: &str) -> usize {
    s.chars().count()
}

fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('\u{2026}');
    t
}

/// The band's OWN vocabulary — the glyphs that spell a hand, a hold, mail, the
/// link, a story, a trust verdict, a reset — and its two separators (two
/// spaces between slots, ` · ` between clauses). None of it may arrive inside
/// a free-text value: a `meta role` of `⊘ hold pause ·fleet🔒  ◂ manager` would
/// otherwise print as a hold and a hand the session does not have.
const BAND_GLYPHS: &[char] = &[
    '\u{25c2}',
    '\u{25b8}',
    '\u{2298}',
    '\u{2709}',
    '\u{21af}',
    '\u{2191}',
    '\u{27df}',
    '~',
    '\u{2715}',
    '\u{25c7}',
    '\u{2713}',
    '\u{2717}',
    '\u{26a0}',
    '\u{2190}',
    '\u{2192}',
    '\u{00b7}',
    '\u{1f512}',
];

/// A wire-safe token: printable, single-spaced, none of the band's own glyphs,
/// at most `cap` chars, cut with `…`. Every free-text value the band prints (a
/// role, a holder, a sender, a reason, a reset time, a told story's text)
/// passes here, so a control byte, a bidi override or the band's grammar in
/// an agent-chosen name can never reach the chrome or forge a slot.
pub(crate) fn sanitize_token(s: &str, cap: usize) -> String {
    let mut clean = String::with_capacity(s.len());
    let mut at_space = true;
    for c in s.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if c.is_control()
            || matches!(
                c,
                '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
            || BAND_GLYPHS.contains(&c)
        {
            continue;
        }
        if c == ' ' {
            if at_space {
                continue;
            }
            at_space = true;
        } else {
            at_space = false;
        }
        clean.push(c);
    }
    truncate(clean.trim_end(), cap)
}

/// A hold's reason as every human-facing surface prints and speaks it: the
/// wire token (`status hold=`'s pct-encoded form, `main%20broken`) DECODED to
/// its words and then held to [`sanitize_token`]'s rule — a bridge-supplied
/// reason can encode a control byte or a bidi override, and the decode must
/// not be the step that lets it through. The band's hand slot, its spoken
/// sentence and the greyed menu row (`crate::menu::hold_row_reason`) all
/// read from here, so they cannot disagree (`main broken` on one and
/// `main%20broken` on another was round 19's second review).
pub(crate) fn hold_reason_words(reason: &str) -> String {
    sanitize_token(&aterm_control::wire::pct_decode(reason), 32)
}

/// A duration as the band prints it: `12s`, `3m12s`, `2h05m`, `1d 22h`.
pub(crate) fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// The short form of a session id for the hand slot: `s-1e91`.
pub(crate) fn short_sid(sid: &str) -> String {
    let n = sid.chars().count().min(6);
    sid.chars().take(n).collect()
}

/// The trust glyph: ✓ for a verdict the receiver trusts, ✗ for one it refuses,
/// ? for anything unresolved. BEFORE the sender, always.
pub(crate) fn trust_glyph(trust: &str) -> char {
    match trust {
        "agent" | "human" | "owner" | "operator" | "verified" => '\u{2713}',
        "forged-self" | "unreadable" | "observer" | "refused" | "spoof" => '\u{2717}',
        _ => '?',
    }
}

/// Compose the six slots for `slot` as seen from a window whose story
/// watermark is `watermark`. Pure; allocates (it is called on CHANGE, never per
/// frame).
pub(crate) fn words(slot: &Slot, now: Instant, watermark: u64) -> Words {
    let level = slot.level(watermark);
    let tone = slot.tone(now, watermark);
    let role = slot
        .role
        .as_deref()
        .map(|r| sanitize_token(r, 64))
        .unwrap_or_else(|| DASH.to_string());

    // phase + since. A TOLD point (`ctl story`) takes the slot for three
    // seconds ahead of everything: it is the watcher's decision, and the
    // phase it interrupts is still one keystroke of patience away.
    let (phase, mut since, mut spoken_phase) = if let Some((verb, text)) = slot.told_now(now)
        && let Some((glyph, word)) = verb.told_words()
    {
        let since = if text.is_empty() {
            Vec::new()
        } else {
            vec![text.to_string()]
        };
        let spoken = if text.is_empty() {
            format!("{word} by watcher")
        } else {
            format!("{word} by watcher, {text}")
        };
        (format!("{glyph} {word}"), since, spoken)
    } else if let Some(text) = &slot.attention {
        let t = sanitize_token(text, 48);
        (t.clone(), Vec::new(), format!("attention, {t}"))
    } else if level == Level::Story {
        // The summary of what happened after the watermark (≤5 clauses, zero
        // counts omitted): turns · timed out · mails · approvals · questions ·
        // the longest stop that ended since.
        let count = |verb: StoryVerb| {
            slot.story_since(watermark)
                .filter(|p| p.verb == verb)
                .count()
        };
        let timeouts = count(StoryVerb::TurnTimedOut);
        let turns = count(StoryVerb::Turn) + timeouts;
        let mut story = Vec::new();
        for (n, word) in [
            (turns, "turn"),
            (timeouts, "timed out"),
            (count(StoryVerb::Mail), "mail"),
            (count(StoryVerb::Approval), "approval"),
            (count(StoryVerb::Question), "question"),
        ] {
            if n > 0 {
                let plural = if n == 1 || word == "timed out" {
                    ""
                } else {
                    "s"
                };
                story.push(format!("{n} {word}{plural}"));
            }
        }
        // The longest stop that ENDED since the watermark: its verb, how long
        // it lasted, and how long ago it lifted.
        let mut stops: Vec<(StoryVerb, Instant, Instant)> = Vec::new();
        let mut open: Option<(StoryVerb, Instant)> = None;
        // A stop that LIFTED but never began in this story — it was already
        // standing when the slot was minted, so its length is unknown — is
        // told as the resume alone.
        let mut unpaired_resume: Option<Instant> = None;
        for p in slot.story_since(watermark) {
            match p.verb {
                StoryVerb::Hold | StoryVerb::Limited => open = Some((p.verb, p.at)),
                StoryVerb::Resumed => match open.take() {
                    Some((v, began)) => stops.push((v, began, p.at)),
                    None => unpaired_resume = Some(p.at),
                },
                _ => {}
            }
        }
        if let Some((verb, began, ended)) = stops
            .into_iter()
            .max_by_key(|(_, b, e)| e.saturating_duration_since(*b))
        {
            let word = if verb == StoryVerb::Hold {
                "held"
            } else {
                "limited"
            };
            story.push(format!(
                "{word} {}, resumed {} ago",
                fmt_dur(ended.saturating_duration_since(began)),
                fmt_dur(now.saturating_duration_since(ended))
            ));
        } else if let Some(ended) = unpaired_resume {
            story.push(format!(
                "resumed {} ago",
                fmt_dur(now.saturating_duration_since(ended))
            ));
        }
        // A worker still RUNNING is not quiet, story or no story: the live
        // phase keeps the slot (`busy 31s`) and the story rides `since` after
        // it — so a turn that timed out on a busy worker never reads `◇ quiet`.
        let live = match &slot.agent {
            Some(a) => matches!(a.phase, AgentPhase::Busy).then_some(("busy", slot.agent_since)),
            None => slot.shell.filter(|(w, _)| *w == "running"),
        };
        if let Some((word, at)) = live {
            let mut since = vec![fmt_dur(now.saturating_duration_since(at))];
            since.extend(story.iter().cloned());
            let spoken = if story.is_empty() {
                word.to_string()
            } else {
                format!("{word}, {}", story.join(", "))
            };
            (word.to_string(), since, spoken)
        } else {
            let mut clauses = Vec::new();
            if let Some(at) = slot.story_since(watermark).next().map(|p| p.at) {
                clauses.push(format!(
                    "since {}",
                    fmt_dur(now.saturating_duration_since(at))
                ));
            }
            clauses.extend(story);
            let spoken = format!("quiet, {}", clauses.join(", "));
            ("\u{25c7} quiet".to_string(), clauses, spoken)
        }
    } else if let Some(agent) = &slot.agent {
        let word = agent.phase.word();
        let mut clauses = Vec::new();
        let mut spoken = word.to_string();
        match &agent.phase {
            AgentPhase::Prompt { detail: Some(d) } => {
                spoken = format!("prompt, {d}");
                (format!("prompt\u{00b7}{d}"), clauses, spoken)
            }
            AgentPhase::Limited { reset, until } => {
                // `limited → 19:30 · 1d 22h`: the reset the notice named, then
                // the time TO it (design §1; the mock counts down) — never the
                // time since the limit began, which is the story's to tell.
                if let Some(r) = reset {
                    clauses.push(format!("\u{2192} {r}"));
                    spoken = format!("limited, resets {r}");
                }
                if let Some(left) = until
                    .map(|u| u.saturating_duration_since(now))
                    .filter(|d| !d.is_zero())
                {
                    clauses.push(fmt_dur(left));
                }
                (word.to_string(), clauses, spoken)
            }
            _ => {
                clauses.push(fmt_dur(now.saturating_duration_since(slot.agent_since)));
                (word.to_string(), clauses, spoken)
            }
        }
    } else if let Some((word, at)) = slot.shell {
        (
            word.to_string(),
            vec![fmt_dur(now.saturating_duration_since(at))],
            word.to_string(),
        )
    } else {
        (DASH.to_string(), Vec::new(), String::new())
    };

    // hand (and its short form for a narrow row)
    let (hand, hand_short, spoken_hand) = match (&slot.hold, &slot.hand) {
        (Some(h), _) => {
            let reason = hold_reason_words(&h.reason);
            let fleet = if h.fleet {
                " \u{00b7}fleet\u{1f512}"
            } else {
                ""
            };
            (
                format!("\u{2298} hold {reason}{fleet}"),
                "\u{2298} hold".to_string(),
                format!(
                    "held, {reason}{}",
                    if h.fleet {
                        ", fleet, cannot be lifted here"
                    } else {
                        ""
                    }
                ),
            )
        }
        (
            None,
            Hand::DrivenTurn {
                id,
                holder: Some(h),
            },
        ) => {
            let h = sanitize_token(h, 32);
            (
                format!("\u{25c2} {h} \u{00b7} turn {id}"),
                format!("\u{25c2} turn {id}"),
                format!("driven by {h}, turn {id}"),
            )
        }
        (None, Hand::DrivenTurn { id, holder: None }) => (
            format!("\u{25c2} turn {id}"),
            format!("\u{25c2} turn {id}"),
            format!("driven, turn {id}"),
        ),
        (None, Hand::DrivenLease { holder }) => {
            let h = sanitize_token(holder, 32);
            (
                format!("\u{25c2} {h}"),
                format!("\u{25c2} {}", truncate(&h, 8)),
                format!("driven by {h}"),
            )
        }
        (None, Hand::Driving { sid }) => {
            let s = sanitize_token(sid, 12);
            (
                format!("\u{25b8} @{s}"),
                format!("\u{25b8} @{s}"),
                format!("driving session {s}"),
            )
        }
        (None, Hand::None) => (DASH.to_string(), DASH.to_string(), String::new()),
    };
    if slot.hold.is_some() {
        // Under a hold the stop clause rides `since` so the summary law and the
        // live row agree on where a duration is printed — but not behind a
        // told word (`✓ approved 0s ⊘ hold review` read as an approval's age).
        if let Some((_, began)) = slot.stop_in_progress()
            && since.is_empty()
            && slot.told_now(now).is_none()
        {
            since.push(fmt_dur(now.saturating_duration_since(began)));
        }
    }

    // mail
    let m = &slot.mail;
    let mut mail = format!("\u{2709}{}", m.unread);
    if m.pending > 0 {
        mail.push_str(&format!(" \u{00b7}{}", m.pending));
    }
    if m.dropped > 0 {
        mail.push_str(&format!(" \u{21af}{}", m.dropped));
    }
    if m.queued > 0 {
        mail.push_str(&format!(" \u{2191}{}", m.queued));
    }
    let mail_short = mail.clone();
    let mut spoken_mail = String::new();
    if m.unread > 0 {
        spoken_mail = format!("{} unread", m.unread);
        if let Some(last) = &m.last {
            let kind = sanitize_token(&last.kind, 12);
            let from = sanitize_token(&last.from, 24);
            mail.push_str(&format!(
                " {kind}\u{2190}{}{from}",
                trust_glyph(&last.trust)
            ));
            spoken_mail.push_str(&format!(", {kind} from {from}, {}", last.trust));
        }
    }

    // ctx
    let ctx = match slot.agent.as_ref().and_then(|a| a.context_pct) {
        Some(pct) if pct <= CTX_WARN_PCT => format!("ctx {pct}% \u{26a0}"),
        Some(pct) => format!("ctx {pct}%"),
        None => String::new(),
    };
    let spoken_ctx = slot
        .agent
        .as_ref()
        .and_then(|a| a.context_pct)
        .map(|p| format!("context {p} percent"));

    // fabric
    let (fabric, spoken_fabric) = match slot.link {
        Link::Absent => ("\u{00b7}".to_string(), None),
        Link::Connected { rtt_ms: Some(ms) } => (format!("\u{27df} {ms}ms"), None),
        Link::Connected { rtt_ms: None } => ("\u{27df}".to_string(), None),
        // A stall with a date prints its age; one without (a bridge still
        // dialing since it attached) prints the glyph alone — never a figure
        // the model made up.
        Link::Stalled { age_ms: Some(a) } => {
            let s = a / 1000;
            (
                format!("~ {s}s"),
                Some(format!("bridge stalled {s} seconds")),
            )
        }
        Link::Stalled { age_ms: None } => ("~".to_string(), Some("bridge stalled".to_string())),
        Link::Disconnected => ("\u{2715} lost".to_string(), Some("bridge lost".to_string())),
    };

    if spoken_phase.is_empty() {
        spoken_phase = "quiet".to_string();
    }
    let mut sentence = Vec::new();
    if !spoken_hand.is_empty() {
        sentence.push(spoken_hand);
    }
    sentence.push(spoken_phase);
    if !spoken_mail.is_empty() {
        sentence.push(spoken_mail);
    }
    if let Some(c) = spoken_ctx.filter(|_| !ctx.is_empty()) {
        sentence.push(c);
    }
    if let Some(f) = spoken_fabric {
        sentence.push(f);
    }

    Words {
        role,
        phase,
        since,
        hand,
        hand_short,
        mail,
        mail_short,
        ctx,
        fabric,
        sentence: sentence.join(", "),
        tone,
    }
}

/// The context-left percentage at and below which the band warns.
pub(crate) const CTX_WARN_PCT: u8 = 15;

// ---------------------------------------------------------------------------
// The per-window view: what the frame path reads, allocation-free.
// ---------------------------------------------------------------------------

/// The ripple's life, and how many distinct frames it paints (nine, ~30 fps).
pub(crate) const RIPPLE: Duration = Duration::from_millis(300);
pub(crate) const RIPPLE_STEPS: u32 = 9;

/// One window's presence, as the last refresh left it. Every field the frame
/// path reads is plain data; the words are recomposed only on change.
#[derive(Clone, Debug)]
pub(crate) struct WindowView {
    /// The story watermarks, one per SESSION shown in this window: the newest
    /// story seq the human has seen of that session here (moved by the fold
    /// law, never by focus alone or a timer). Per session, not per window:
    /// reading one tab's story must not fold another's, and a story read once
    /// must not return after the human reads a second tab.
    pub(crate) watermarks: HashMap<u64, u64>,
    /// The band rows COMMITTED to this window's geometry (0 or 1) — the term
    /// `App::chrome_rows` adds; moved only by `App::sync_presence_rows`.
    pub(crate) rows: u16,
    pub(crate) level: Level,
    pub(crate) rim: Rim,
    /// The band's words, when the row exists.
    pub(crate) words: Option<Words>,
    /// Bumped on every change the painter can see; folded into `presence_fp`.
    /// Never 0 once anything has shown, but `fp()` maps a quiet window to 0.
    pub(crate) seed: u64,
    /// A turn submit's edge ripple: when it started, `None` when none runs
    /// (and always `None` under reduced motion — amplitude 0 means the ripple
    /// never STARTS, so the frame is the steady image).
    pub(crate) ripple_at: Option<Instant>,
    /// The painted band row for `(seed, cols, palette)`, reused until one moves.
    pub(crate) cached_row: Vec<aterm_core::terminal::RenderCell>,
    pub(crate) cached_key: Option<(u64, usize, u64)>,
}

impl Default for WindowView {
    fn default() -> Self {
        Self {
            watermarks: HashMap::new(),
            rows: 0,
            level: Level::Quiet,
            rim: Rim::None,
            words: None,
            seed: 0,
            ripple_at: None,
            cached_row: Vec::new(),
            cached_key: None,
        }
    }
}

impl WindowView {
    /// The story watermark this window holds for `session` (0: never read).
    pub(crate) fn watermark(&self, session: u64) -> u64 {
        self.watermarks.get(&session).copied().unwrap_or(0)
    }

    /// The ripple's step at `now`: `Some(0..RIPPLE_STEPS)` while it runs,
    /// `None` after.
    pub(crate) fn ripple_step(&self, now: Instant) -> Option<u32> {
        let t0 = self.ripple_at?;
        let elapsed = now.saturating_duration_since(t0);
        if elapsed >= RIPPLE {
            return None;
        }
        let step = elapsed.as_millis() * u128::from(RIPPLE_STEPS) / RIPPLE.as_millis();
        Some((step as u32).min(RIPPLE_STEPS - 1))
    }

    /// The repaint-key term: **exactly 0 on a quiet window** (no rim, no row,
    /// no ripple), else a nonzero fold of the seed (floored at 1, so the fold
    /// is never 0) and the ripple step (0 = none, else `step + 1`). No clock is
    /// read unless a ripple is live, and no allocation ever.
    pub(crate) fn fp(&self, now: Instant) -> u64 {
        if self.rows == 0 && matches!(self.rim, Rim::None) && self.ripple_at.is_none() {
            return 0;
        }
        let step = self
            .ripple_at
            .and_then(|_| self.ripple_step(now))
            .map_or(0, |s| u64::from(s) + 1);
        (self.seed.max(1) << 8) | step
    }

    /// The next instant this view's PIXELS change on their own — only while a
    /// ripple runs. Steady rims and rows have no deadline (the change-driven
    /// law); the band's `since` text ticks through `App::presence_deadline`.
    pub(crate) fn ripple_deadline(&self, now: Instant) -> Option<Instant> {
        let t0 = self.ripple_at?;
        let end = t0 + RIPPLE;
        if now >= end {
            return Some(now);
        }
        let step = self.ripple_step(now).unwrap_or(0);
        Some((t0 + RIPPLE * (step + 1) / RIPPLE_STEPS).min(end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn driven(now: Instant) -> Slot {
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                revision: 1,
                agent: Some(AgentReading {
                    phase: AgentPhase::Busy,
                    context_pct: Some(41),
                }),
                hand: Hand::DrivenTurn {
                    id: 41,
                    holder: Some("manager".into()),
                },
                mail: MailFacts {
                    unread: 2,
                    head: 2,
                    last: Some(MailLast {
                        kind: "task".into(),
                        from: "manager".into(),
                        trust: "agent".into(),
                    }),
                    ..MailFacts::default()
                },
                link: Link::Connected { rtt_ms: Some(12) },
                ..Facts::default()
            },
            now,
        );
        s
    }

    /// THE MOCK'S FIRST BAND, verbatim: role, phase since, hand, mail with the
    /// trust glyph BEFORE the sender, ctx, fabric.
    #[test]
    fn the_six_slots_read_like_the_approved_mock() {
        let now = t0();
        let s = driven(now);
        let w = words(&s, now + Duration::from_secs(192), 0);
        assert_eq!(
            w.fit(120),
            "worker:claude-satcomp  busy 3m12s  \u{25c2} manager \u{00b7} turn 41  \u{2709}2 task\u{2190}\u{2713}manager  ctx 41%  \u{27df} 12ms"
        );
        assert_eq!(
            w.sentence,
            "driven by manager, turn 41, busy, 2 unread, task from manager, agent, context 41 percent"
        );
        assert_eq!(w.tone, Tone::Info);
        assert_eq!(s.level(0), Level::Driven);
        assert_eq!(s.level(0).rim(), Rim::Drive);
    }

    /// Elision right→left: rtt, then since, then role, then ctx; hand, phase
    /// and mail survive to 24 columns.
    #[test]
    fn elision_sheds_from_the_right_and_keeps_hand_phase_mail_to_24_columns() {
        let now = t0();
        let s = driven(now);
        let w = words(&s, now + Duration::from_secs(192), 0);
        let at = |cols| w.fit(cols);
        assert_eq!(width(&at(120)), 89, "the whole line: {}", at(120));
        assert!(!at(85).contains("\u{27df}"), "rtt goes first: {}", at(85));
        assert!(at(85).contains("3m12s"));
        assert!(!at(78).contains("3m12s"), "then since: {}", at(78));
        assert!(at(78).contains("worker:claude-satcomp"));
        assert!(!at(70).contains("worker:claude"), "then role: {}", at(70));
        assert!(at(70).contains("ctx 41%"));
        assert!(!at(50).contains("ctx"), "then ctx: {}", at(50));
        assert!(at(50).contains("task\u{2190}\u{2713}manager"));
        assert!(!at(40).contains("task"), "then the mail detail: {}", at(40));
        assert!(at(40).contains("\u{25c2} manager \u{00b7} turn 41"));
        let narrow = at(28);
        assert!(
            !narrow.contains("manager"),
            "then the hand's holder: {narrow}"
        );
        let n24 = at(24);
        assert!(n24.contains("busy"), "{n24}");
        assert!(
            n24.contains("\u{25c2} turn 41"),
            "the hand's short form: {n24}"
        );
        assert!(n24.contains("\u{2709}2"), "the mail's short form: {n24}");
        assert!(!n24.contains("manager"), "{n24}");
        assert!(width(&n24) <= 24, "{n24}");
        for cols in [24usize, 60, 120] {
            assert!(width(&at(cols)) <= cols, "{cols}: {}", at(cols));
        }
    }

    /// Severity: hold > limited > attention > driven > story > quiet, and the
    /// rim follows the level exactly — a colour with no words is unreachable.
    #[test]
    fn severity_and_rim_follow_the_design_table() {
        let now = t0();
        let mut s = Slot::new(now);
        assert_eq!(s.level(0), Level::Quiet);
        assert_eq!(s.level(0).rim(), Rim::None);
        s.absorb(
            Facts {
                hand: Hand::DrivenTurn {
                    id: 1,
                    holder: None,
                },
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.level(0), Level::Driven);
        s.absorb(
            Facts {
                hand: Hand::DrivenTurn {
                    id: 1,
                    holder: None,
                },
                attention: Some("needs a decision".into()),
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.level(0), Level::Attention);
        assert_eq!(s.level(0).rim(), Rim::Wait);
        s.absorb(
            Facts {
                hand: Hand::DrivenTurn {
                    id: 1,
                    holder: None,
                },
                attention: Some("needs a decision".into()),
                revision: 2,
                agent: Some(AgentReading {
                    phase: AgentPhase::Limited {
                        reset: Some("19:30".into()),
                        until: None,
                    },
                    context_pct: None,
                }),
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.level(0), Level::Limited);
        assert_eq!(s.level(0).rim(), Rim::Stop { hold: false });
        s.absorb(
            Facts {
                hold: Some(HoldFact {
                    reason: "fabric-lost".into(),
                    fleet: true,
                }),
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.level(0), Level::Hold);
        assert_eq!(s.level(0).rim(), Rim::Stop { hold: true });
        let w = words(&s, now + Duration::from_secs(40), 0);
        assert!(
            w.hand
                .starts_with("\u{2298} hold fabric-lost \u{00b7}fleet\u{1f512}"),
            "{}",
            w.hand
        );
        assert!(
            w.sentence
                .starts_with("held, fabric-lost, fleet, cannot be lifted here"),
            "{}",
            w.sentence
        );
        assert_eq!(w.tone, Tone::Warn);
        // A stalled bridge is an instance fact: fabric slot, never a rim.
        let mut q = Slot::new(now);
        q.absorb(
            Facts {
                link: Link::Stalled {
                    age_ms: Some(7_400),
                },
                ..Facts::default()
            },
            now,
        );
        assert_eq!(q.level(0), Level::Note);
        assert_eq!(q.level(0).rim(), Rim::None);
        assert_eq!(words(&q, now, 0).fabric, "~ 7s");
        let mut lost = Slot::new(now);
        lost.absorb(
            Facts {
                link: Link::Disconnected,
                ..Facts::default()
            },
            now,
        );
        assert_eq!(lost.level(0).rim(), Rim::None);
        assert_eq!(words(&lost, now, 0).fabric, "\u{2715} lost");
    }

    /// The story row: quiet, with a since-summary of what happened after the
    /// watermark — and it folds to Quiet once the watermark catches up.
    #[test]
    fn a_story_summarises_since_the_watermark_and_the_watermark_folds_it() {
        let now = t0();
        let mut s = Slot::new(now);
        for id in 1..=3 {
            s.absorb(
                Facts {
                    turn: Some(TurnFact {
                        id,
                        settled: true,
                        dur_ms: 10,
                        carried: false,
                    }),
                    ..Facts::default()
                },
                now + Duration::from_secs(id),
            );
        }
        s.absorb(
            Facts {
                turn: Some(TurnFact {
                    id: 3,
                    settled: true,
                    dur_ms: 10,
                    carried: false,
                }),
                mail: MailFacts {
                    unread: 0,
                    head: 2,
                    ..MailFacts::default()
                },
                ..Facts::default()
            },
            now + Duration::from_secs(5),
        );
        s.absorb(
            Facts {
                turn: Some(TurnFact {
                    id: 3,
                    settled: true,
                    dur_ms: 10,
                    carried: false,
                }),
                mail: MailFacts {
                    unread: 0,
                    head: 2,
                    ..MailFacts::default()
                },
                hold: Some(HoldFact {
                    reason: "pause".into(),
                    fleet: false,
                }),
                ..Facts::default()
            },
            now + Duration::from_secs(10),
        );
        s.absorb(
            Facts {
                turn: Some(TurnFact {
                    id: 3,
                    settled: true,
                    dur_ms: 10,
                    carried: false,
                }),
                mail: MailFacts {
                    unread: 0,
                    head: 2,
                    ..MailFacts::default()
                },
                ..Facts::default()
            },
            now + Duration::from_secs(70),
        );
        assert_eq!(s.level(0), Level::Story);
        assert_eq!(s.level(0).chip(), ChipLevel::Story);
        let w = words(&s, now + Duration::from_secs(130), 0);
        assert_eq!(w.phase, "\u{25c7} quiet");
        assert_eq!(
            w.since,
            vec![
                "since 2m09s".to_string(),
                "3 turns".to_string(),
                "1 mail".to_string(),
                "held 1m00s, resumed 1m00s ago".to_string(),
            ]
        );
        // The longest stop survives elision: at 40 columns the counts go first.
        let narrow = w.fit(44);
        assert!(narrow.contains("held 1m00s"), "{narrow}");
        assert!(s.calm());
        assert_eq!(s.level(s.story_seq), Level::Quiet);
        assert!(!s.level(s.story_seq).shows_row());
    }

    /// The tone: Warn while a human should look, Success for 2 s after a
    /// settled turn, Info otherwise.
    #[test]
    fn tone_is_warn_then_success_for_two_seconds_then_info() {
        let now = t0();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                turn: Some(TurnFact {
                    id: 1,
                    settled: true,
                    dur_ms: 1,
                    carried: false,
                }),
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.tone(now + Duration::from_millis(500), 0), Tone::Success);
        assert_eq!(s.tone(now + Duration::from_millis(2500), 0), Tone::Info);
        s.absorb(
            Facts {
                turn: Some(TurnFact {
                    id: 1,
                    settled: true,
                    dur_ms: 1,
                    carried: false,
                }),
                revision: 1,
                agent: Some(AgentReading {
                    phase: AgentPhase::Prompt {
                        detail: Some("bash".into()),
                    },
                    context_pct: Some(12),
                }),
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.tone(now, 0), Tone::Warn);
        let w = words(&s, now, 0);
        assert_eq!(w.phase, "prompt\u{00b7}bash");
        assert_eq!(w.ctx, "ctx 12% \u{26a0}");
    }

    /// No command text, body, title or limit message reaches the words: a
    /// hostile role/holder/reason is sanitized to a printable token.
    #[test]
    fn free_text_is_sanitized_and_the_limit_message_never_appears() {
        let now = t0();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                role: Some("evil\u{202e}role\nwith\tcontrol".into()),
                hand: Hand::DrivenLease {
                    holder: "h\u{7f}older".into(),
                },
                revision: 1,
                agent: Some(AgentReading {
                    phase: AgentPhase::Limited {
                        reset: Some("7:30pm".into()),
                        until: None,
                    },
                    context_pct: None,
                }),
                ..Facts::default()
            },
            now,
        );
        let w = words(&s, now, 0);
        assert_eq!(w.role, "evilrole with control");
        assert_eq!(w.hand, "\u{25c2} holder");
        assert_eq!(w.phase, "limited");
        assert_eq!(w.since, vec!["\u{2192} 7:30pm".to_string()], "{w:?}");
        assert_eq!(
            sanitize_token("You've reached your limit", 8),
            "You've \u{2026}"
        );
    }

    #[test]
    fn durations_read_like_the_mock() {
        assert_eq!(fmt_dur(Duration::from_secs(12)), "12s");
        assert_eq!(fmt_dur(Duration::from_secs(192)), "3m12s");
        assert_eq!(fmt_dur(Duration::from_secs(7_500)), "2h05m");
        assert_eq!(fmt_dur(Duration::from_secs(165_600)), "1d 22h");
        assert_eq!(short_sid("s-1e918c4662a1b7b8bd43"), "s-1e91");
    }

    /// The repaint term is 0 on a quiet view and nonzero the moment a rim, a
    /// row or a ripple exists; a ripple steps nine times and then stops.
    #[test]
    fn window_view_fp_is_zero_when_quiet_and_the_ripple_steps_nine_times() {
        let now = t0();
        let mut v = WindowView::default();
        for i in 0..1000u64 {
            assert_eq!(v.fp(now + Duration::from_millis(i)), 0);
        }
        v.rim = Rim::Drive;
        v.seed = 3;
        let steady = v.fp(now);
        assert_ne!(steady, 0);
        assert_eq!(
            v.fp(now + Duration::from_secs(60)),
            steady,
            "no clock in a steady rim"
        );
        v.ripple_at = Some(now);
        let mut seen = std::collections::BTreeSet::new();
        let mut t = now;
        while t < now + RIPPLE {
            seen.insert(v.fp(t));
            t += Duration::from_millis(1);
        }
        assert_eq!(seen.len(), 9, "nine ripple frames");
        assert_eq!(v.ripple_step(now + RIPPLE), None);
        assert!(v.ripple_deadline(now).is_some());
    }

    /// The classifier is `aterm-phase`'s verdict, word for word, and its count
    /// moves once per call (the gate test in `app_presence` compares it with
    /// revision moves). The screens are judged by `aterm_phase` itself, so the
    /// mapping — not the composer geometry — is what this pins.
    #[test]
    fn classify_is_aterm_phases_verdict_and_counts_once_per_call() {
        let before = classifier_calls();
        let rows = |body: &[&str]| body.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let screens = [
            rows(&["\u{23fa} Which do you prefer?", ""]),
            rows(&["\u{23fa} Done.", ""]),
            rows(&[
                "\u{23fa} Running the tests.",
                "",
                "\u{00b7} Cogitating\u{2026} (12s)",
            ]),
            rows(&[]),
        ];
        for screen in &screens {
            let verdict = aterm_phase::worker_phase(screen);
            let ours = classify(screen, Instant::now());
            assert_eq!(ours.phase.word(), verdict.name(), "{screen:?}");
            assert_eq!(ours.context_pct, aterm_phase::context_left(screen));
        }
        assert_eq!(classifier_calls() - before, screens.len() as u64);
        // The never-shown law at the classifier's own edge: a limit notice
        // keeps only its reset, never its message.
        let limited = classify(
            &rows(&[
                "\u{23fa} You've hit your weekly limit \u{00b7} resets Sep 19 at 11am (America/Los_Angeles)",
                "",
            ]),
            Instant::now(),
        );
        if let AgentPhase::Limited { reset, .. } = &limited.phase {
            assert!(
                reset.as_deref().is_none_or(|r| !r.contains("weekly")),
                "{reset:?}"
            );
        }
    }

    /// The reset clock reads the notice's own words: a bare time, a 12-hour
    /// time with its meridian attached or apart, a month-day, a zone in
    /// parens — and refuses what it cannot place.
    #[test]
    fn the_reset_clock_reads_the_notices_words() {
        let spec = |date, hour, minute, zone: Option<&str>| ResetSpec {
            date,
            hour,
            minute,
            zone: zone.map(str::to_string),
        };
        assert_eq!(
            parse_reset("7:30pm (America/Los_Angeles)"),
            Some(spec(None, 19, 30, Some("America/Los_Angeles")))
        );
        assert_eq!(parse_reset("3am"), Some(spec(None, 3, 0, None)));
        assert_eq!(parse_reset("12am"), Some(spec(None, 0, 0, None)));
        assert_eq!(parse_reset("12:15 pm"), Some(spec(None, 12, 15, None)));
        assert_eq!(parse_reset("19:30"), Some(spec(None, 19, 30, None)));
        assert_eq!(
            parse_reset("Sep 19 at 11am (America/Los_Angeles)"),
            Some(spec(Some((9, 19)), 11, 0, Some("America/Los_Angeles")))
        );
        assert_eq!(
            parse_reset("Sep 19, 11:00"),
            Some(spec(Some((9, 19)), 11, 0, None))
        );
        assert_eq!(parse_reset("soon"), None);
        assert_eq!(parse_reset("25:00"), None);
        assert_eq!(parse_reset("13pm"), None);
        assert_eq!(parse_reset(""), None);
    }

    /// The countdown is measured on THIS machine's clock: a bare clock time
    /// two hours from now counts down two hours; one an hour ago is tomorrow's;
    /// a dated reset behind us is no figure at all. Pure through
    /// [`countdown_at`] at a fixed clock, then the live path once.
    #[test]
    fn the_countdown_is_to_the_next_such_time_on_this_clock() {
        // 2026-09-19 10:00:00 UTC, at -0700: 03:00 local on the 19th.
        let now = 1_789_812_000_i64;
        let off = -7 * 3600;
        let spec = |date, hour, minute| ResetSpec {
            date,
            hour,
            minute,
            zone: None,
        };
        assert_eq!(
            countdown_at(&spec(None, 5, 0), now, off),
            Some(Duration::from_secs(2 * 3600))
        );
        assert_eq!(
            countdown_at(&spec(None, 2, 0), now, off),
            Some(Duration::from_secs(23 * 3600)),
            "an hour ago is tomorrow's"
        );
        assert_eq!(
            countdown_at(&spec(None, 3, 0), now, off),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(
            countdown_at(&spec(Some((9, 21)), 11, 0), now, off),
            Some(Duration::from_secs(2 * 86_400 + 8 * 3600))
        );
        assert_eq!(
            countdown_at(&spec(Some((9, 18)), 11, 0), now, off),
            None,
            "behind us"
        );
        assert_eq!(
            countdown_at(&spec(Some((9, 19)), 2, 0), now, off),
            None,
            "behind us today"
        );
        // The offset reader: `+0200` / `-0700` / nothing else.
        assert_eq!(parse_utc_offset("+0200"), Some(7200));
        assert_eq!(parse_utc_offset("-0700"), Some(-25_200));
        assert_eq!(parse_utc_offset("0700"), None);
        assert_eq!(parse_utc_offset("+07:00"), None);
        // The live path, once: a bare time two hours from now on this clock.
        if let Some(offset) = local_offset_s() {
            let local = i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            )
            .unwrap()
                + offset
                + 2 * 3600;
            let tod = local.rem_euclid(86_400);
            let text = format!("{}:{:02}", tod / 3600, (tod % 3600) / 60);
            let ahead = countdown_to_reset(&text).expect("placeable");
            assert!(
                (Duration::from_secs(2 * 3600 - 61)..=Duration::from_secs(2 * 3600))
                    .contains(&ahead),
                "{text}: {ahead:?}"
            );
            // A zone that is not this machine's: no figure, never a wrong one.
            if let Some(local_zone) = local_zone() {
                let other = if local_zone == "Etc/UTC" {
                    "America/Los_Angeles"
                } else {
                    "Etc/UTC"
                };
                assert_eq!(countdown_to_reset(&format!("{text} ({other})")), None);
            }
        }
        assert_eq!(countdown_to_reset("soon"), None);
    }

    // ───── the second adversarial reviews (honesty-cost, menu-a11y), 2026-09-19 ─────

    /// ADV-6 (model): a stalled link with no date behind it (a bridge still
    /// dialing since it attached) has no age — the composer used to print
    /// `~ 0s` and speak "bridge stalled 0 seconds". The glyph alone now, and
    /// the sentence names no figure; a dated stall still prints its seconds.
    #[test]
    fn adv6_a_stall_of_unknown_age_prints_no_figure() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                link: Link::Stalled { age_ms: None },
                ..Facts::default()
            },
            now,
        );
        let w = words(&s, now, 0);
        assert_ne!(w.fabric, "~ 0s", "sentence=`{}`", w.sentence);
        assert_eq!(w.fabric, "~");
        assert!(!w.sentence.contains("0 seconds"), "{}", w.sentence);
        assert!(w.sentence.ends_with("bridge stalled"), "{}", w.sentence);
        s.absorb(
            Facts {
                link: Link::Stalled {
                    age_ms: Some(7_400),
                },
                ..Facts::default()
            },
            now,
        );
        let w = words(&s, now, 0);
        assert_eq!(w.fabric, "~ 7s");
        assert!(
            w.sentence.ends_with("bridge stalled 7 seconds"),
            "{}",
            w.sentence
        );
    }

    /// ADV-7 (model): a hold the slot first sees at its mint used to be dated
    /// from the mint. Nothing in `Hold` says when it began, so the model
    /// cannot know: the first absorb is the baseline, the hold is state with
    /// no figure, and when it lifts the story says `resumed N ago` without a
    /// length it never measured. A hold that BEGINS after the baseline is
    /// dated and told in full.
    #[test]
    fn adv7_a_hold_seen_at_first_sight_has_no_made_up_duration() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        let held = |reason: &str| {
            Some(HoldFact {
                reason: reason.into(),
                fleet: false,
            })
        };
        let later = now + Duration::from_secs(3600);
        s.absorb(
            Facts {
                hold: held("pause"),
                ..Facts::default()
            },
            later,
        );
        let w = words(&s, later, 0);
        assert!(
            !w.since.iter().any(|c| c == "0s"),
            "the hold's age is unknown, the band says: {} / since={:?}",
            w.fit(120),
            w.since
        );
        assert!(w.since.is_empty(), "{:?}", w.since);
        assert_eq!(w.hand, "\u{2298} hold pause");
        assert_eq!(s.level(0), Level::Hold);
        assert_eq!(s.story_seq, 0, "standing at the mint: state, not news");
        // Lifted 5 s later: the story is the resume alone.
        let lifted = later + Duration::from_secs(5);
        s.absorb(Facts::default(), lifted);
        let at = lifted + Duration::from_secs(3);
        let w = words(&s, at, 0);
        assert_eq!(s.level(0), Level::Story);
        assert_eq!(
            w.since,
            vec!["since 3s".to_string(), "resumed 3s ago".to_string()]
        );
        assert!(!w.sentence.contains("held"), "{}", w.sentence);
        // A hold that begins AFTER the baseline is dated and told in full.
        let began = at + Duration::from_secs(10);
        s.absorb(
            Facts {
                hold: held("review"),
                ..Facts::default()
            },
            began,
        );
        let w = words(&s, began + Duration::from_secs(4), 0);
        assert_eq!(w.since, vec!["4s".to_string()]);
        let end = began + Duration::from_secs(20);
        s.absorb(Facts::default(), end);
        let w = words(&s, end + Duration::from_secs(1), 0);
        assert!(
            w.since.iter().any(|c| c == "held 20s, resumed 1s ago"),
            "{:?}",
            w.since
        );
    }

    /// menu-a11y D2: the SPOKEN hold sentence carried the pct-encoded wire
    /// token (`held, main%20broken, …` — VoiceOver read "main percent twenty
    /// broken") while the greyed menu row beside it said `main broken`. The
    /// band, its sentence and the menu row all read the reason through
    /// [`hold_reason_words`] now: decoded, then sanitized like every other
    /// free-text value — a `%e2%80%ae` in a bridge-supplied reason cannot
    /// reverse the row.
    #[test]
    fn the_hold_reason_is_decoded_and_sanitized_on_every_surface() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                hold: Some(HoldFact {
                    reason: "main%20broken".into(),
                    fleet: true,
                }),
                ..Facts::default()
            },
            now,
        );
        let w = words(&s, now, 0);
        assert!(
            w.sentence
                .starts_with("held, main broken, fleet, cannot be lifted here"),
            "spoken: {:?}",
            w.sentence
        );
        assert_eq!(w.hand, "\u{2298} hold main broken \u{00b7}fleet\u{1f512}");
        assert_eq!(
            hold_reason_words("main%e2%80%aebroken%0a%1b[31m"),
            "mainbroken [31m"
        );
        assert_eq!(hold_reason_words("-"), "-");
        assert_eq!(
            hold_reason_words("hold=x%20%e2%8a%98%20review"),
            "hold=x review"
        );
        // The menu row's own pin (`menu::hold_row_reason`, the same words) lives
        // beside that row in `menu.rs`.
    }
}
