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
//! (`message_band::BandTarget::Presence`) with six fixed slots in a fixed order —
//! role · phase since · hand · mail · ctx · fabric — so a glance reads left to
//! right and a screen reader gets the same sentence
//! (`accesskit_tree::ChromeMessage::PresenceStatus`).
//!
//! THE LAWS, from the design (r11/presence-spec.md) and binding here:
//!
//! * The rim is CHANGE-DRIVEN. Nothing in this module reads a clock on the frame
//!   path: the per-window view is rebuilt only when a fact changes (a lease, a
//!   hold, mail, a published agent verdict, `meta set`), and the repaint key's
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
//! The agent PHASE (busy / prompt / question / limited / idle / survey) is the
//! SERVER'S published verdict ([`agent_verdict`], run by the status sweep in
//! `session_status.rs`): `aterm_phase`'s readers over the [`live_zone`] (the
//! last [`CLASSIFY_ROWS`] rows of the screen's content, and the blank rows
//! under it) with the terminal's cursor ([`Cursor`]: Claude Code's `idle` is
//! the prompt box that holds it), applied only to a session identified as an
//! agent, and re-run only when the content or the cursor moved AND those rows
//! or the cursor's row changed — at most 4 Hz per session, never per frame;
//! the test-only [`classifier_calls`] counter is the gate's proof. This module
//! only folds that verdict in.

use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_agent::supervise::limit;
use aterm_session::SessionId;
use winit::event_loop::EventLoopProxy;

use crate::Wake;

/// The colour mood of a chrome row — information, a good end, or something
/// the user should look at. The presence row's phase slot reads it
/// ([`Words::tone`]); the message band paints it through
/// `message_band::paint_presence_row`. Moved here from the retired status
/// bars (2026-09-22), whose two lanes shared it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Tone {
    #[default]
    Info,
    Success,
    Warn,
}

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
/// and the live zone above it fit in the last 40 rows of any screen's CONTENT
/// ([`live_zone`]), and the whole-screen fallback (no composer frame) reads
/// the same rows a supervisor would.
pub(crate) const CLASSIFY_ROWS: usize = 40;

/// THE LIVE ZONE the agent verdict reads ([`agent_verdict`]) and the status
/// sweep's classifier gate hashes (`session_status`'s `agent_observe`): the
/// last [`CLASSIFY_ROWS`] rows of the screen's content — up to its last
/// non-blank row — and the blank rows under it
/// ([`aterm_phase::live_zone_start`], the one cut: a supervisor whose tail
/// read ends on a blank row cuts there too). Counted from the grid's last
/// row, the cut missed whatever was drawn wholly above a blank foot: the
/// live run of 2026-09-26 (Claude Code 2.1.283 in a fresh 149x62 pane whose
/// shell prompt sat at the top) drew the folder-trust dialog on rows 5-20,
/// the last 40 rows of the SCREEN were blank, and `status` published
/// `agent=idle` for 3+ minutes while the hosted loop, which waits on that
/// verdict at an idle point (`await agent`), slept; and Claude Code's inline
/// renderer draws its REPL at the top of a pane taller than the cut, blank
/// rows below it (the review of 2026-09-26, 150x50: `agent=unknown`, `await
/// agent idle` timed out). The zone never shrinks: on a screen whose last row
/// has content it is the last 40 rows, as before; it reaches up only by as
/// many rows as the screen's foot is blank, so a reader is handed the same
/// rows under the content it always was.
pub(crate) fn live_zone(rows: &[String]) -> &[String] {
    &rows[aterm_phase::live_zone_start(rows, CLASSIFY_ROWS)..]
}

/// The TERMINAL'S CURSOR as the verdict reads a screen: Claude Code's `idle`
/// is its prompt box's only where that box holds the cursor
/// ([`aterm_phase::ScreenReader::read_at`], `aterm_phase::phase::
/// prompt_box_holds`) — never a box an earlier run left on the screen. The
/// inline renderer relaunched in the SAME tab keeps the previous run's box
/// above the new launch line, and the server published `agent=idle` from it
/// before the new REPL was drawn (the review of 2026-09-26: a draft typed on
/// `await agent idle` lost 3 of 3 after the folder-trust dialog, 1 of 3
/// without it); the cursor sits under the new launch line there (measured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// Its row among the rows the verdict is handed (`None`: on none of
    /// them). The sweep reads it under the same terminal guard as the rows.
    At(Option<usize>),
    /// Not given: a test's bare screen, read by its frame alone
    /// ([`aterm_phase::ScreenReader::read`]). Nothing that ships reads one.
    #[cfg(test)]
    Unknown,
}

impl Cursor {
    /// The same cursor on the rows from `start` on (`rows[start..]`).
    fn on_rows_from(self, start: usize) -> Self {
        match self {
            Self::At(row) => Self::At(row.and_then(|r| r.checked_sub(start))),
            #[cfg(test)]
            Self::Unknown => Self::Unknown,
        }
    }
}

#[cfg(test)]
thread_local! {
    static CLASSIFIER_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times the agent-phase classifier has run ON THIS THREAD — the
/// count the classifier-gate test compares against screen changes.
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

/// The worker's phase as the band spells it. `Wall` keeps only the wall's
/// KIND and the RESET time the notice named — never the notice text (the
/// never-shown law).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentPhase {
    Busy,
    /// An approval box is showing; `detail` is the box's kind and the
    /// classifier's verdict on the command (`bash safe`), never the command.
    Prompt {
        detail: Option<String>,
    },
    Question,
    /// The last turn ended on a wall ([`aterm_phase::Wall`]): a usage window,
    /// a model bucket, spend, a full context, a lost login, an API error, an
    /// overload. The worker sits at an idle composer and will not move on its
    /// own (an overload or a retryable API error only after a retry).
    Wall {
        kind: aterm_phase::WallKind,
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
    /// An identified agent whose reader has no EVIDENCE for a phase
    /// ([`aterm_phase::Reading::phase_authoritative`] false — a Codex screen
    /// outside its choice box, a Claude Code screen with no prompt box that
    /// holds the terminal's cursor: its launch, before the REPL is up, an
    /// earlier run's box above a same-tab relaunch included): its default
    /// `idle` is not published as idle, since whatever acts on an idle worker
    /// would act on a guess — a first prompt typed there is lost.
    Unknown,
}

impl AgentPhase {
    /// The `agent=` wire word (`wall:<kind>` for a wall — [`wall_word`]).
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Prompt { .. } => "prompt",
            Self::Question => "question",
            Self::Wall { kind, .. } => wall_word(*kind),
            Self::Idle => "idle",
            Self::Survey => "survey",
            Self::Unknown => "unknown",
        }
    }

    /// The word the band prints: [`Self::word`], except that a wall the band
    /// has always called `limited` (a usage window, a model bucket, spend, an
    /// API rate limit — [`aterm_phase::WallKind::reads_limited`]) keeps that
    /// word (design §1: `limited → 19:30 · 1d 22h`), an API error the network
    /// caused says what went wrong ([`api_cause_words`]: `can't reach the
    /// API`, `reply cut off`, `TLS/proxy refused`), any other wall is the
    /// state it leaves the session in (the menu bar's
    /// `status_item::wall_words`), and a parked survey is `idle`: the worker
    /// waits at its composer either way.
    pub(crate) fn band_word(&self) -> &'static str {
        match self {
            Self::Wall { kind, .. } if kind.reads_limited() => "limited",
            Self::Wall { kind, .. } => api_cause_words(*kind)
                .unwrap_or_else(|| crate::status_item::wall_words(kind.name())),
            Self::Survey => "idle",
            other => other.word(),
        }
    }

    /// What the band's age counts from: its phase word, a box by the kind the
    /// band names ([`prompt_band_word`]), so a box that follows another
    /// starts its own age.
    fn age_key(&self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Prompt { detail } => prompt_band_word(detail.as_deref()).into(),
            other => other.band_word().into(),
        }
    }
}

/// A box's kind in a person's words: the kind before the colon of
/// `kind[:verdict]` (`bash:not-read-only` → `bash`), the hyphenated kinds
/// spelled out (`plan-exit` → `plan`). `None` for a box of no named kind.
/// The band and the menu bar (`status_item::agent_escalation`) both say it.
pub(crate) fn prompt_kind_words(detail: Option<&str>) -> Option<&str> {
    let kind = detail
        .and_then(|d| d.split(':').next())
        .filter(|k| !k.is_empty() && *k != "other")?;
    Some(match kind {
        "plan-enter" => "plan mode",
        "plan-exit" => "plan",
        "held-message" => "held message",
        "goal-proposal" => "goal",
        "computer-use" => "computer use",
        "read-outside-setting" => "outside read",
        "model-switch" => "model switch",
        "trust" => "folder trust",
        "powershell" => "PowerShell",
        other => other,
    })
}

/// A box in the band's words: `bash approval`, `plan approval`; the agent's
/// question tool is a `question`, never an approval
/// ([`aterm_phase::PromptKind::Question`]); a box of no named kind is
/// `approval`.
pub(crate) fn prompt_band_word(detail: Option<&str>) -> String {
    match prompt_kind_words(detail) {
        Some("question") => "question".to_string(),
        Some(kind) => format!("{kind} approval"),
        None => "approval".to_string(),
    }
}

/// The tab's and the menu's words for an API error that never reached the
/// API ([`aterm_phase::ApiCause::Unreachable`]). Not `offline`: most of that
/// family — `Connection refused — a firewall or proxy may be blocking it`,
/// `Request timed out`, `No response from API` — is no machine offline, and
/// the words promise nothing the harness may not do (the outage of
/// 2026-09-27).
pub(crate) const API_UNREACHABLE: &str = "can't reach the API";
/// … a reply the connection cut off ([`aterm_phase::ApiCause::CutOff`]).
pub(crate) const API_CUT_OFF: &str = "reply cut off";
/// … a certificate or proxy the connection refused
/// ([`aterm_phase::ApiCause::Config`]).
pub(crate) const API_REFUSED: &str = "TLS/proxy refused";

/// What an API error the NETWORK caused is called on the tab, in the menu and
/// in the notification — the cause the vendor's own line names
/// ([`aterm_phase::ApiCause`]), never a plan: nothing here knows what the
/// session's supervisor will do, or when (its `WAITING` is its journal's).
/// `None` for the server's own failure (a status, words the catalog does not
/// name) and every other wall: its kind's name stands.
pub(crate) fn api_cause_words(kind: aterm_phase::WallKind) -> Option<&'static str> {
    match kind {
        aterm_phase::WallKind::ApiError { cause, .. } => match cause {
            aterm_phase::ApiCause::Unreachable => Some(API_UNREACHABLE),
            aterm_phase::ApiCause::CutOff => Some(API_CUT_OFF),
            aterm_phase::ApiCause::Config => Some(API_REFUSED),
            aterm_phase::ApiCause::Server => None,
        },
        _ => None,
    }
}

/// `wall:<kind>` for a wall kind — `status agent=`'s spelling, `wall:` and
/// [`aterm_phase::WallKind::name`] (pinned by a test). Static because a
/// published verdict word is.
pub(crate) fn wall_word(kind: aterm_phase::WallKind) -> &'static str {
    use aterm_phase::WallKind as K;
    match kind {
        K::UsageSession => "wall:usage-session",
        K::UsageWeekly => "wall:usage-weekly",
        K::ModelBucket { .. } => "wall:model-bucket",
        K::Spend => "wall:spend",
        K::Context => "wall:context",
        K::Auth => "wall:auth",
        K::ApiError { .. } => "wall:api-error",
        K::Overloaded => "wall:overloaded",
        K::Memory => "wall:memory",
    }
}

/// The server's agent verdict on one screen: not an agent, or an agent's
/// reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentVerdict {
    /// The foreground program is not an identified agent: `agent=-`, no band
    /// phase, no rim. A shell whose last line ends in `?` lands here.
    NotAgent,
    /// An identified agent's reading. `by_frame` is true when only the
    /// agent's own screen identified it — Claude Code's composer frame or one
    /// of its boxes, Codex's composer or one of its boxes (the caller keeps
    /// that identity, `program`, for the rest of the foreground job: a box
    /// hides the composer, and Codex's composer holding a draft names
    /// nothing). `subject` is the approval
    /// box's command or path, folded to one clipped line — or, at an API
    /// error the network caused, its cause in words ([`api_cause_words`]) —
    /// for the host's own menu row and notification
    /// ([`crate::status_item::escalation`]), never for the wire or the band;
    /// `None` for every other phase.
    Agent {
        reading: AgentReading,
        by_frame: bool,
        subject: Option<String>,
        /// Which program's reader read it — by name or by its screen.
        program: aterm_phase::Program,
    },
}

impl AgentVerdict {
    /// The `agent=` wire word: the phase word, or `-` for [`Self::NotAgent`].
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Self::NotAgent => "-",
            Self::Agent { reading, .. } => reading.phase.word(),
        }
    }

    /// The `agent_detail=` value (raw; the wire pct-encodes it): a prompt's
    /// `kind[:verdict]`, a wall's reset time, `None` otherwise.
    pub(crate) fn detail(&self) -> Option<String> {
        match self {
            Self::Agent { reading, .. } => match &reading.phase {
                AgentPhase::Prompt { detail } => detail.clone(),
                AgentPhase::Wall { reset, .. } => reset.clone(),
                _ => None,
            },
            Self::NotAgent => None,
        }
    }

    /// The agent the verdict is of (its reader's program), when it is one.
    pub(crate) fn program(&self) -> Option<aterm_phase::Program> {
        match self {
            Self::Agent { program, .. } => Some(*program),
            Self::NotAgent => None,
        }
    }

    /// The reading, when there is one.
    pub(crate) fn reading(&self) -> Option<&AgentReading> {
        match self {
            Self::Agent { reading, .. } => Some(reading),
            Self::NotAgent => None,
        }
    }

    /// The box's command or path, host-side only (see [`Self::Agent`]).
    pub(crate) fn subject(&self) -> Option<&str> {
        match self {
            Self::Agent { subject, .. } => subject.as_deref(),
            Self::NotAgent => None,
        }
    }
}

/// THE ONE ADAPTER between a session's screen and its published agent
/// verdict. Per-vendor screen knowledge stays in `aterm_phase` — its
/// per-program readers ([`aterm_phase::identify`]); this only decides WHICH
/// reader to ask, and whether to ask at all: a session whose foreground
/// program is an identified agent (`claude`, `codex`) gets that program's
/// reader; one `known_agent` already (identified by its screen earlier in
/// this foreground job) keeps that agent's reader; one whose program is a
/// runtime an agent runs under ([`aterm_phase::may_host_agent`]) or cannot
/// be known (no foreground group to name) is identified by its SCREEN
/// (`identify(None, rows)`: Claude Code's composer frame or one of its boxes;
/// Codex's composer — empty, or under its status row — or one of its boxes).
/// A program still being RESOLVED (`program_pending`: a
/// group, no name yet) identifies nothing until it is named — a shell's own
/// group is re-named after every job, and a frame left on screen by `cat`
/// must not flash an agent verdict in that gap. Anything else, and any screen
/// the generic reader gets, is [`AgentVerdict::NotAgent`] — a shell never
/// reads as a question, not even one that has just `cat`-ed a captured
/// Claude screen.
///
/// `cursor` is the terminal's cursor on `rows`, the WHOLE screen ([`Cursor`]).
pub(crate) fn agent_verdict(
    program: Option<&str>,
    program_pending: bool,
    known_agent: Option<aterm_phase::Program>,
    rows: &[String],
    cursor: Cursor,
    now: Instant,
) -> AgentVerdict {
    use aterm_phase::{Program, ScreenReader};
    // The live zone: the last CLASSIFY_ROWS of the screen's content
    // ([`live_zone`]); `whole` is read again for a box whose head the zone
    // cuts.
    let whole = rows;
    let rows = live_zone(whole);
    // The one name table is aterm-phase's (`program_of`, `may_host_agent`).
    let by_program = program.is_some_and(|p| aterm_phase::program_of(p).is_some());
    // A frame identification made while the program was unresolved does not
    // survive the program resolving to something that cannot host an agent.
    let frame_may_identify = program.map_or(!program_pending, aterm_phase::may_host_agent);
    let known_agent = known_agent.filter(|_| frame_may_identify);
    let (reader, by_frame): (&dyn ScreenReader, bool) = if by_program {
        (aterm_phase::identify(program, rows), false)
    } else if let Some(known) = known_agent {
        (known.reader(), false)
    } else if frame_may_identify {
        let reader = match aterm_phase::identify(None, rows) {
            // A box the zone cut names its program on the whole screen.
            r if r.program() == Program::Generic && whole.len() > rows.len() => {
                aterm_phase::identify(None, whole)
            }
            r => r,
        };
        match reader.program() {
            Program::Generic => return AgentVerdict::NotAgent,
            // The identity is carried across the job's boxes and drafts.
            _ => (reader, true),
        }
    } else {
        return AgentVerdict::NotAgent;
    };
    // A box the zone cut — its title above the zone of a taller screen — is
    // read on the whole screen, as the supervisor's loop reads it
    // (the E2E probe of 2026-09-25: a trust dialog at the top of a 45-row
    // pane was published `agent_detail=other`, and the menu bar said "other
    // approval").
    let (rows, cursor) =
        if whole.len() > rows.len() && reader.prompt(rows).is_some_and(|p| p.head_off_screen) {
            (whole, cursor)
        } else {
            (rows, cursor.on_rows_from(whole.len() - rows.len()))
        };
    let (reading, subject) = classify(reader, rows, cursor, now);
    AgentVerdict::Agent {
        reading,
        by_frame,
        subject,
        program: reader.program(),
    }
}

/// [`agent_verdict`] behind the screen reader's panic fence
/// ([`crate::reader_guard`]) — what the sweep (`session_status`) publishes.
/// A reader panic reads as [`unreadable_verdict`] (an identified agent's
/// `unknown`, never a prompt) and is warned once per session per panic
/// location, instead of taking the terminal and every session with it.
pub(crate) fn agent_verdict_guarded(
    session: u64,
    program: Option<&str>,
    program_pending: bool,
    known_agent: Option<aterm_phase::Program>,
    rows: &[String],
    cursor: Cursor,
    now: Instant,
) -> AgentVerdict {
    fenced_verdict(session, program, program_pending, known_agent, rows, || {
        agent_verdict(program, program_pending, known_agent, rows, cursor, now)
    })
}

/// The fence around one verdict `read` (the seam the tests inject a
/// panicking reader through).
fn fenced_verdict(
    session: u64,
    program: Option<&str>,
    program_pending: bool,
    known_agent: Option<aterm_phase::Program>,
    rows: &[String],
    read: impl FnOnce() -> AgentVerdict,
) -> AgentVerdict {
    match crate::reader_guard::guarded(read) {
        Ok(verdict) => verdict,
        Err(panic) => {
            crate::reader_guard::warn_once(&format!("{session}"), "agent verdict", &panic, rows);
            unreadable_verdict(program, program_pending, known_agent)
        }
    }
}

/// The verdict for a screen the reader could not read (it panicked): an
/// agent identified WITHOUT the screen — by its program's name, or by the
/// frame this foreground job already showed — is that agent at
/// [`AgentPhase::Unknown`], the reading a reader with no evidence gets
/// (nothing acts on it, nothing announces it); anything else is
/// [`AgentVerdict::NotAgent`], since identifying it would need the screen.
/// Never a prompt, a question or a wall.
fn unreadable_verdict(
    program: Option<&str>,
    program_pending: bool,
    known_agent: Option<aterm_phase::Program>,
) -> AgentVerdict {
    let frame_may_identify = program.map_or(!program_pending, aterm_phase::may_host_agent);
    let program = match program.and_then(aterm_phase::program_of) {
        Some(p) => p,
        None => match known_agent.filter(|_| frame_may_identify) {
            Some(known) => known,
            None => return AgentVerdict::NotAgent,
        },
    };
    AgentVerdict::Agent {
        reading: AgentReading {
            phase: AgentPhase::Unknown,
            context_pct: None,
        },
        by_frame: false,
        subject: None,
        program,
    }
}

/// Classify one screen with one program's reader: the reading, and the
/// box's command or path for the host's menu row ([`AgentVerdict::Agent`]).
/// The ONLY caller of `aterm_phase`'s readers in this crate, so the test
/// counter is total; `rows` is the [`live_zone`] (or the whole screen, for a
/// box whose head it cuts), `cursor` the terminal's cursor on them. Callers
/// go through [`agent_verdict`].
///
/// A reading that is not [`aterm_phase::Reading::phase_authoritative`] — read
/// with the cursor ([`aterm_phase::ScreenReader::read_at`]: Claude Code's
/// `idle` only at the prompt box that holds it) — is
/// [`AgentPhase::Unknown`], whatever its default phase. A wall
/// ([`aterm_phase::Reading::wall`], which the reader leaves `None` under a
/// box, and under a hard busy keeps only Claude Code's critical-memory
/// banner) outranks the phase it ended on — `idle`, `question`, a background
/// monitor's soft busy, and for that banner a running spinner: the worker
/// will not move past it.
pub(crate) fn classify(
    reader: &dyn aterm_phase::ScreenReader,
    rows: &[String],
    cursor: Cursor,
    now: Instant,
) -> (AgentReading, Option<String>) {
    #[cfg(test)]
    CLASSIFIER_CALLS.with(|c| c.set(c.get() + 1));
    let r = match cursor {
        Cursor::At(row) => reader.read_at(rows, row, None),
        #[cfg(test)]
        Cursor::Unknown => reader.read(rows, None),
    };
    let wall = |kind: aterm_phase::WallKind, reset: Option<String>| {
        let until = reset
            .as_deref()
            .and_then(countdown_to_reset)
            .map(|d| now + d);
        AgentPhase::Wall {
            kind,
            reset: reset.map(|r| sanitize_token(&r, 32)),
            until,
        }
    };
    let mut subject = None;
    let phase = if !r.phase_authoritative {
        AgentPhase::Unknown
    } else if let Some(w) = r.wall {
        // An API error the network caused names its cause for the menu row
        // and the notification: the wire word stays `wall:api-error`.
        subject = api_cause_words(w.kind).map(str::to_string);
        wall(w.kind, w.reset)
    } else {
        match r.phase {
            aterm_phase::Phase::Busy => AgentPhase::Busy,
            aterm_phase::Phase::Prompt => {
                subject = r
                    .prompt
                    .as_ref()
                    .map(|p| crate::status_item::fold_clip(&p.command, 96))
                    .filter(|s| !s.is_empty());
                AgentPhase::Prompt {
                    detail: r.prompt.as_ref().map(prompt_detail),
                }
            }
            aterm_phase::Phase::Question => AgentPhase::Question,
            // `worker_phase` names a limit only where `wall` places one, so
            // this arm is the belt to that brace: name the kind from the
            // notice's own words, a usage window when the table has none.
            aterm_phase::Phase::Limited { message, reset } => wall(
                aterm_phase::classify_wall(&message).unwrap_or(aterm_phase::WallKind::UsageSession),
                reset,
            ),
            aterm_phase::Phase::Idle if r.survey => AgentPhase::Survey,
            aterm_phase::Phase::Idle => AgentPhase::Idle,
        }
    };
    (
        AgentReading {
            phase,
            context_pct: r.context_left,
        },
        subject,
    )
}

/// The prompt's kind word, and — for a Bash approval — the read-only
/// classifier's verdict on it (`bash:read-only`, `bash:not-read-only`). The
/// command itself is never carried. `read-only` only when EVERY reading of
/// the box's command rows ([`aterm_phase::PromptV2::readings`]) is read-only,
/// and never for a box with no command rows. Advisory: the supervisor's own
/// decider decides.
fn prompt_detail(p: &aterm_phase::PromptV2) -> String {
    let kind = sanitize_token(p.kind.name(), 24);
    if p.kind != aterm_phase::PromptKind::Bash || p.command.trim().is_empty() {
        return kind;
    }
    let readings = p.readings();
    let read_only = !readings.is_empty()
        && readings
            .iter()
            .all(|r| aterm_agent::supervise::classify::classify_command(r).read_only);
    let verdict = if read_only {
        "read-only"
    } else {
        "not-read-only"
    };
    format!("{kind}:{verdict}")
}

// ---------------------------------------------------------------------------
// The reset clock: placing a limit notice's reset time on this machine's clock.
// ---------------------------------------------------------------------------

/// How long until `reset` falls, on THIS machine's clock — the figure the
/// limited row counts down. The notice's words are read by the supervisor's
/// own grammar ([`limit::parse_reset`], placed by [`limit::reset_at`]), so
/// the band and the engine never read one notice two ways. `None` when the
/// words cannot be read, when the notice names a zone that is not this
/// machine's, or when the reset is already behind us: then the row prints
/// the reset time and no figure.
pub(crate) fn countdown_to_reset(reset: &str) -> Option<Duration> {
    countdown_at(
        reset,
        limit::unix_now(),
        local_offset_s()?,
        local_zone().as_deref(),
    )
}

/// [`countdown_to_reset`] with the clock, the offset and the local zone
/// passed in — pure, so the rules are testable at any hour. A notice in
/// another zone is no figure rather than one placed by a `date` spawned per
/// read; with no zone named, or this machine's, it is placed at `offset`.
fn countdown_at(reset: &str, now: i64, offset: i64, local_zone: Option<&str>) -> Option<Duration> {
    let spec = limit::parse_reset(reset)?;
    if let limit::ResetSpec::At {
        zone: Some(named), ..
    } = &spec
        && local_zone.is_some_and(|local| local != named)
    {
        return None;
    }
    let at = limit::reset_at(&spec, now, offset, |_| None);
    u64::try_from(at - now)
        .ok()
        .filter(|&s| s > 0)
        .map(Duration::from_secs)
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

/// The local clock's offset from UTC in seconds, from `date +%z`, read once;
/// `None` where it cannot be run — then no figure is ever printed. (Settings ▸
/// Packages reads its clock per instant instead, [`local_offset_at`].)
#[cfg(unix)]
pub(crate) fn local_offset_s() -> Option<i64> {
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
        limit::parse_zone(String::from_utf8_lossy(&out.stdout).trim())
    })
}

#[cfg(not(unix))]
pub(crate) fn local_offset_s() -> Option<i64> {
    None
}

/// The local zone's offset from UTC AT the instant `unix`, in seconds — `date -r <unix>
/// +%z` (BSD) / `date -d @<unix> +%z` (GNU): the offset daylight saving gave that instant,
/// not today's. Uncached and a subprocess each call, so a WORKER's only (Settings ▸
/// Packages' clock, `packages_screen::LocalClock::read`); `None` where `date` cannot say.
#[cfg(unix)]
pub(crate) fn local_offset_at(unix: i64) -> Option<i64> {
    let mut date = std::process::Command::new("date");
    if cfg!(target_os = "macos") {
        date.arg("-r").arg(unix.to_string());
    } else {
        date.arg("-d").arg(format!("@{unix}"));
    }
    let out = date
        .arg("+%z")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    limit::parse_zone(String::from_utf8_lossy(&out.stdout).trim())
}

#[cfg(not(unix))]
pub(crate) fn local_offset_at(_unix: i64) -> Option<i64> {
    None
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

/// Whether `hand` is THIS process's own harness (ruling 313): a drive lease
/// under the holder name its supervisor loops claim sessions by
/// ([`crate::harness_host::holder`]). Another instance's harness, a manager
/// session or an `aterm drive` is somebody else's hand, and stays a fact the
/// window shows.
pub(crate) fn is_aterms_hand(hand: &Hand) -> bool {
    matches!(hand, Hand::DrivenLease { holder } if *holder == crate::harness_host::holder())
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
    /// `attention` is told ELSEWHERE, in full (ruling 270, "one place tells
    /// it"): its only owner is the agent upgrade, whose stall is the message
    /// band's row and the log's record. The level, `status why=`, the tab
    /// chip and the rim still read `attention`; only the presence line's
    /// phase slot does not repeat its words.
    pub(crate) attention_told_elsewhere: bool,
    /// The shell status word (`running`, `idle`, `quiet`, …) and when it was
    /// published — the phase the band shows when no agent reading exists.
    pub(crate) shell: Option<(&'static str, Instant)>,
    /// The sequence number of the server's agent reading
    /// (`StatusObserver::agent_reading`): `agent` is folded in only when this
    /// is NEWER than the one the slot last absorbed; `0` = never read.
    pub(crate) agent_seq: u64,
    /// The server's agent reading at `agent_seq` — `None` for a session that
    /// is not an identified agent.
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
    /// The server's published input stall (`input_stall::InputStallFact`,
    /// `status input=stalled|stopped`): the program has stopped reading its
    /// input. Ranks [`Level::Limited`] and takes the phase slot as `frozen`
    /// (or `stopped`) — ahead of typed attention, which during a stall is the
    /// server's own entry saying the same thing at length.
    pub(crate) input_stall: Option<crate::input_stall::InputStallFact>,
}

impl Default for Facts {
    fn default() -> Self {
        Self {
            role: None,
            attention: None,
            attention_told_elsewhere: false,
            shell: None,
            agent_seq: 0,
            agent: None,
            hand: Hand::None,
            lease_until: None,
            hold: None,
            mail: MailFacts::default(),
            link: Link::Absent,
            turn: None,
            input_stall: None,
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
    /// `ctl story chose <policy>` — the SUPERVISOR answered Claude Code's
    /// question dialog by policy (`[harness] answer_questions`, or the
    /// session's `meta set questions recommended`), the text naming the policy
    /// word. The one told
    /// verb whose teller is the harness, not a watcher, and the one the window
    /// answers with a chime and a pulse (`App::tell_story`), because the point
    /// of it is that the human notices a question was answered without them.
    Chose,
}

impl StoryVerb {
    /// The closed set of words `aterm ctl story <verb>` accepts, in the order
    /// the usage line prints them. A word outside it is a usage error, never a
    /// free-text story point.
    pub(crate) const TOLD_WORDS: [&'static str; 8] = [
        "approved",
        "dismissed",
        "reconnected",
        "timeout",
        "exit",
        "compacted",
        "warned",
        "chose",
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
            "chose" => Self::Chose,
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
            // A filled diamond: distinct from an approval's check, and the
            // solid twin of the quiet summary's `◇`.
            Self::Chose => ('\u{25c6}', "chose"),
            Self::Turn
            | Self::TurnTimedOut
            | Self::Mail
            | Self::Hold
            | Self::Limited
            | Self::Question
            | Self::Resumed => return None,
        })
    }

    /// WHO acted on a told point, for the spoken sentence (`approved by
    /// watcher`, `chose by harness`, `compacted by worker`): the harness for
    /// [`Self::Chose`], the worker for [`Self::Compacted`] (the watcher only
    /// saw it happen), a watcher for every other told word. Saying "watcher"
    /// for what the harness or the worker did would misname who acted.
    pub(crate) const fn teller(self) -> &'static str {
        match self {
            Self::Chose => "harness",
            Self::Compacted => "worker",
            Self::Approval
            | Self::Dismissed
            | Self::Reconnected
            | Self::Timeout
            | Self::Exit
            | Self::Warned
            | Self::Turn
            | Self::TurnTimedOut
            | Self::Mail
            | Self::Hold
            | Self::Limited
            | Self::Question
            | Self::Resumed => "watcher",
        }
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
    /// The point happened under aterm's OWN harness's hand
    /// ([`Slot::aterm_hand`], ruling 313): kept in the story and counted by
    /// `story=`, never news — the band and the log already tell what aterm
    /// did there.
    pub(crate) aterm: bool,
}

// ---------------------------------------------------------------------------
// The per-session slot.
// ---------------------------------------------------------------------------

/// A session's presence, as the last refresh left it.
#[derive(Clone, Debug)]
pub(crate) struct Slot {
    /// The agent-reading sequence last absorbed ([`Facts::agent_seq`]); `None`
    /// before the first. The classifier itself runs in the status sweep, never
    /// per refresh or per frame; this only keeps a refresh from re-folding a
    /// reading it already has.
    pub(crate) agent_seq_seen: Option<u64>,
    pub(crate) role: Option<String>,
    pub(crate) attention: Option<String>,
    /// [`Facts::attention_told_elsewhere`].
    pub(crate) attention_told_elsewhere: bool,
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
    /// [`Facts::input_stall`].
    pub(crate) input_stall: Option<crate::input_stall::InputStallFact>,
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
    /// The seq of the newest point that is NEWS — one that did not happen
    /// under aterm's own hand ([`StoryPoint::aterm`]); what the level reads.
    news_seq: u64,
    /// aterm's OWN harness has its hand on the session (ruling 313): a drive
    /// lease this process's harness holds ([`is_aterms_hand`]), or a `turn`
    /// typed inside one (an Owner-token turn whose lease hands back to it).
    /// Not a presence fact: the window's own chrome does not show it, and the
    /// story points it makes are not news.
    aterm_hand: bool,
    /// Set for the length of one [`Self::absorb`] in which aterm's hand was on
    /// the session at either end: every point it notes is aterm's.
    noting_aterm: bool,
    /// The story up to this seq is CLOSED: the agent it was about left the
    /// session, so it is no longer news to hold the row for (2026-09-24,
    /// D10: `level=story` stood on a bare shell long after the agent exited,
    /// and in a headless instance nobody ever acts to read it). Every
    /// window's watermark is read as at least this.
    story_closed: u64,
    /// The last TOLD point (`ctl story`), with its text: the phase slot reads
    /// it for [`TOLD_FLASH`] after `at`, then returns to the phase.
    told: Option<(StoryVerb, String, Instant)>,
}

impl Slot {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            agent_seq_seen: None,
            role: None,
            attention: None,
            attention_told_elsewhere: false,
            shell: None,
            agent: None,
            agent_since: now,
            hand: Hand::None,
            lease_until: None,
            hold: None,
            mail: MailFacts::default(),
            link: Link::Absent,
            turn: None,
            input_stall: None,
            settled_at: None,
            stop_began: None,
            baselined: false,
            story: VecDeque::new(),
            story_seq: 0,
            news_seq: 0,
            aterm_hand: false,
            noting_aterm: false,
            story_closed: 0,
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
        let aterm = self.noting_aterm || self.aterm_hand;
        if !aterm {
            self.news_seq = self.story_seq;
        }
        if self.story.len() >= STORY_CAP {
            self.story.pop_front();
        }
        self.story.push_back(StoryPoint {
            seq: self.story_seq,
            at: now,
            verb,
            aterm,
        });
    }

    /// Fold one refresh's facts in. Returns what CHANGED: whether anything the
    /// view reads moved, and whether a turn was just SUBMITTED (the ripple's
    /// edge, taken from the wake rather than inferred).
    pub(crate) fn absorb(&mut self, facts: Facts, now: Instant) -> bool {
        let mut changed = false;
        // aterm's own hand at either end of this refresh makes every point it
        // notes aterm's (ruling 313): a relaunch's turn settles and its lease
        // is handed back between two refreshes, in either order.
        let was_aterm = self.aterm_hand;
        let now_aterm = is_aterms_hand(&facts.hand)
            || (was_aterm && matches!(facts.hand, Hand::DrivenTurn { holder: None, .. }));
        self.noting_aterm = was_aterm || now_aterm;
        if self.aterm_hand != now_aterm {
            self.aterm_hand = now_aterm;
            changed = true;
        }
        if self.role != facts.role {
            self.role = facts.role;
            changed = true;
        }
        if self.attention != facts.attention {
            self.attention = facts.attention;
            changed = true;
        }
        if self.attention_told_elsewhere != facts.attention_told_elsewhere {
            self.attention_told_elsewhere = facts.attention_told_elsewhere;
            changed = true;
        }
        if self.shell.map(|s| s.0) != facts.shell.map(|s| s.0) {
            changed = true;
        }
        self.shell = facts.shell;
        if facts.agent_seq > self.agent_seq_seen.unwrap_or(0) {
            self.agent_seq_seen = Some(facts.agent_seq);
            let agent = facts.agent;
            let word_moved = self.agent.as_ref().map(|a| a.phase.word())
                != agent.as_ref().map(|a| a.phase.word());
            if self.agent.as_ref().map(|a| a.phase.age_key())
                != agent.as_ref().map(|a| a.phase.age_key())
            {
                self.agent_since = now;
            }
            if word_moved {
                match agent.as_ref().map(|a| &a.phase) {
                    Some(AgentPhase::Question) => self.note(StoryVerb::Question, now),
                    Some(AgentPhase::Wall { .. }) => {
                        self.note(StoryVerb::Limited, now);
                        self.stop_began = Some((StoryVerb::Limited, now));
                    }
                    _ => {}
                }
                if matches!(
                    self.agent.as_ref().map(|a| &a.phase),
                    Some(AgentPhase::Wall { .. })
                ) && !matches!(
                    agent.as_ref().map(|a| &a.phase),
                    Some(AgentPhase::Wall { .. })
                ) {
                    self.note(StoryVerb::Resumed, now);
                }
            }
            if self.agent != agent {
                changed = true;
            }
            // The agent LEFT (a verdict, then none): what it did while nobody
            // looked is closed, not left standing on the shell it returned to.
            if self.agent.is_some() && agent.is_none() && self.story_closed < self.story_seq {
                self.story_closed = self.story_seq;
                changed = true;
            }
            self.agent = agent;
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
        if self.input_stall != facts.input_stall {
            self.input_stall = facts.input_stall;
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
        self.noting_aterm = false;
        changed
    }

    /// The tab chip's mark: a hollow diamond to wait, a filled one at a stop
    /// (with why it stopped, which the hover names), a dot for a story.
    pub(crate) fn chip(&self, watermark: u64) -> ChipLevel {
        match self.level(watermark) {
            Level::Quiet | Level::Note | Level::Driving | Level::Driven => ChipLevel::Off,
            Level::Story => ChipLevel::Story,
            Level::Attention => ChipLevel::Wait,
            Level::Hold => ChipLevel::Stop(StopCause::Hold),
            // `level` ranks a stall ahead of a wall, and so does this.
            Level::Limited => ChipLevel::Stop(match &self.input_stall {
                Some(stall) if stall.stopped => StopCause::Suspended,
                Some(_) => StopCause::Frozen,
                None => StopCause::Wall,
            }),
        }
    }

    /// The severity this slot stands at (`status level=`, `why=` and the
    /// tab chip follow it).
    pub(crate) fn level(&self, watermark: u64) -> Level {
        self.level_counting(watermark, true, true)
    }

    /// The level the window's OWN chrome shows — the rim and the band row's
    /// existence: [`Self::level`], except that an attention another place
    /// already tells ([`Facts::attention_told_elsewhere`]: a stalled agent
    /// upgrade, whose row and record are the message band's) is not counted.
    /// Round 18, day four (D1): a Claude stall raised an EMPTY presence line
    /// (`— quiet 8s — ✉0 ·`) and an orange rim beside its band row, because
    /// the level stood at attention while the line had no words for it — a
    /// colour with no words, which [`Level::rim`]'s own rule forbids. The
    /// fact stays where it is read on purpose: `status level=attention
    /// why=escalation` and the tab's wait mark.
    pub(crate) fn shown_level(&self, watermark: u64) -> Level {
        self.level_counting(watermark, !self.attention_told_elsewhere, !self.aterm_hand)
    }

    /// `attention`: an escalation counts; `hand`: a hand counts even when it
    /// is aterm's own (ruling 313 — `status level=` keeps the fact, the
    /// window's chrome does not show it).
    fn level_counting(&self, watermark: u64, attention: bool, hand: bool) -> Level {
        if self.hold.is_some() {
            return Level::Hold;
        }
        // A program that reads nothing is stopped as surely as one at a
        // wall, whatever its screen still shows (2026-09-24).
        if self.input_stall.is_some() {
            return Level::Limited;
        }
        if matches!(
            self.agent.as_ref().map(|a| &a.phase),
            Some(AgentPhase::Wall { .. })
        ) {
            return Level::Limited;
        }
        if (attention && self.attention.is_some())
            || matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Prompt { .. } | AgentPhase::Question)
            )
        {
            return Level::Attention;
        }
        match &self.hand {
            Hand::DrivenTurn { .. } | Hand::DrivenLease { .. } if hand => return Level::Driven,
            Hand::Driving { .. } => return Level::Driving,
            _ => {}
        }
        // An unread task or ask is a wait state only while no hand is on the
        // session: a driven worker with mail waiting stays teal (the mock's
        // first row), and the mail slot says the rest.
        if self.unread_wants_a_human() {
            return Level::Attention;
        }
        // A story outranks waiting mail: "something happened since you
        // looked" is the glance fact, and the mail slot prints the mail.
        if self.news_seq > watermark.max(self.story_closed) {
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

    /// WHY the slot stands at [`Level::Attention`] (`status why=`): the causes
    /// the level merges, comma-joined in a fixed order — `prompt` (an approval
    /// box), `question` (the agent asked), `escalation` (a typed `meta
    /// attention`), `mail` (an unread task/ask with no hand on the session).
    /// `-` at any other level.
    pub(crate) fn why(&self, watermark: u64) -> String {
        if self.level(watermark) != Level::Attention {
            return "-".to_string();
        }
        let phase = self.agent.as_ref().map(|a| &a.phase);
        let mut causes = Vec::new();
        if matches!(phase, Some(AgentPhase::Prompt { .. })) {
            causes.push("prompt");
        }
        if matches!(phase, Some(AgentPhase::Question)) {
            causes.push("question");
        }
        if self.attention.is_some() {
            causes.push("escalation");
        }
        if matches!(self.hand, Hand::None) && self.unread_wants_a_human() {
            causes.push("mail");
        }
        if causes.is_empty() {
            "-".to_string()
        } else {
            causes.join(",")
        }
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
            && self.input_stall.is_none()
            && matches!(self.hand, Hand::None)
            && self.attention.is_none()
            && !matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Prompt { .. } | AgentPhase::Question | AgentPhase::Wall { .. })
            )
            && !self.unread_wants_a_human()
    }

    /// The tone the band paints in: Warn while a human should look, Success for
    /// [`SETTLED_GLOW`] after a settled turn, else Info. It follows the level
    /// the row is SHOWN at ([`Self::shown_level`], ruling 280): an attention
    /// the message band already tells painted the row Warn with no words
    /// for it.
    pub(crate) fn tone(&self, now: Instant, watermark: u64) -> Tone {
        if self.shown_level(watermark) >= Level::Attention {
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

    /// The story points after `watermark` (and after a closed story), oldest
    /// first.
    pub(crate) fn story_since(&self, watermark: u64) -> impl Iterator<Item = &StoryPoint> {
        let seen = watermark.max(self.story_closed);
        self.story.iter().filter(move |p| p.seq > seen && !p.aterm)
    }

    /// The current stop (hold / limit) in progress, for the story's summary.
    fn stop_in_progress(&self) -> Option<(StoryVerb, Instant)> {
        self.stop_began.filter(|(verb, _)| match verb {
            StoryVerb::Hold => self.hold.is_some(),
            _ => matches!(
                self.agent.as_ref().map(|a| &a.phase),
                Some(AgentPhase::Wall { .. })
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
    Stop(StopCause),
}

/// Why a chip stands at a stop: what decides the remedy (lift a hold, restart
/// or resume the program, see the agent's wall). Ascending as
/// [`Slot::level`] ranks them, so a tab's `max` over its panes keeps a hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum StopCause {
    /// An agent at a wall ([`AgentPhase::Wall`]).
    Wall,
    /// The program has stopped reading its input.
    Frozen,
    /// The foreground job is stopped with input queued.
    Suspended,
    /// A hold.
    Hold,
}

impl ChipLevel {
    /// The chrome-state tokens the introspection line prints for this mark.
    pub(crate) const fn chrome_states(self) -> &'static [&'static str] {
        match self {
            Self::Off => &[],
            Self::Story => &["story"],
            Self::Wait => &["attention"],
            Self::Stop(_) => &["attention", "stop"],
        }
    }

    /// The hover-help clause.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const fn help(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Story => Some("Something happened while you were away"),
            Self::Wait => Some("Needs attention"),
            Self::Stop(StopCause::Hold) => Some("Held"),
            Self::Stop(StopCause::Frozen) => Some("Frozen, not reading input"),
            Self::Stop(StopCause::Suspended) => Some("Stopped with input queued"),
            Self::Stop(StopCause::Wall) => Some("Agent can't continue"),
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
    // The row's words follow the level it is shown at (ruling 280): an
    // attention another place tells must not hide the story under it.
    let level = slot.shown_level(watermark);
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
        let teller = verb.teller();
        let spoken = if text.is_empty() {
            format!("{word} by {teller}")
        } else {
            format!("{word} by {teller}, {text}")
        };
        (format!("{glyph} {word}"), since, spoken)
    } else if let Some(fact) = &slot.input_stall {
        crate::input_stall::band_phase(fact, now)
    } else if let Some(text) = slot
        .attention
        .as_ref()
        .filter(|_| !slot.attention_told_elsewhere)
    {
        let t = sanitize_token(text, 48);
        (t.clone(), Vec::new(), format!("attention, {t}"))
    } else if level == Level::Story {
        // The summary of what happened after the watermark (≤6 clauses, zero
        // counts omitted): turns · timed out · mails · approvals · choices ·
        // questions · the longest stop that ended since. A `choice` is the
        // harness answering a question box by policy — counted apart from the
        // watcher's approvals, and apart from `question` (a box that WAITED).
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
            (count(StoryVerb::Chose), "choice"),
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
        let word = agent.phase.band_word();
        let mut clauses = Vec::new();
        let mut spoken = word.to_string();
        match &agent.phase {
            AgentPhase::Prompt { detail } => {
                let word = prompt_band_word(detail.as_deref());
                clauses.push(fmt_dur(now.saturating_duration_since(slot.agent_since)));
                spoken.clone_from(&word);
                (word, clauses, spoken)
            }
            AgentPhase::Wall { reset, until, .. } => {
                // `limited → 19:30 · 1d 22h`: the reset the notice named, then
                // the time TO it (design §1; the mock counts down) — never the
                // time since the limit began, which is the story's to tell.
                if let Some(r) = reset {
                    clauses.push(format!("\u{2192} {r}"));
                    spoken = format!("{word}, resets {r}");
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
        // aterm's own harness is named as aterm, never by its holder's pid
        // (ruling 313), on the rare row something else raised.
        (None, Hand::DrivenTurn { .. } | Hand::DrivenLease { .. }) if slot.aterm_hand => (
            "\u{25c2} aterm".to_string(),
            "\u{25c2} aterm".to_string(),
            "driven by aterm".to_string(),
        ),
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
    // The context LEFT (`<n>% until auto-compact`, `<n>% context left`).
    let ctx = match slot.agent.as_ref().and_then(|a| a.context_pct) {
        Some(pct) if pct <= CTX_WARN_PCT => format!("ctx {pct}% left \u{26a0}"),
        Some(pct) => format!("ctx {pct}% left"),
        None => String::new(),
    };
    let spoken_ctx = slot
        .agent
        .as_ref()
        .and_then(|a| a.context_pct)
        .map(|p| format!("context {p} percent left"));

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
    /// The running ripple is a CHOICE PULSE (`App::start_presence_pulse`): the
    /// harness answered a question box in the session this window's FRONT tab
    /// shows. It paints in the story tone and paints on a window with NO rim —
    /// the one ripple a quiet window shows. Reset with `ripple_at`.
    pub(crate) ripple_chose: bool,
    /// When the words' `since` figures next move — the band's own text clock
    /// (1 s while they print seconds, 60 s after), set by `App::presence_tick`
    /// when it recomposes them; `None` until the first tick of a row. The tick
    /// recomposes a window only when this is due, so another owner's wakes
    /// (the band's 30 fps motion, blink) never re-read presence facts at their
    /// own rate (audit 2026-09-24).
    pub(crate) words_due: Option<Instant>,
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
            ripple_chose: false,
            words_due: None,
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
                agent_seq: 1,
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
            "worker:claude-satcomp  busy 3m12s  \u{25c2} manager \u{00b7} turn 41  \u{2709}2 task\u{2190}\u{2713}manager  ctx 41% left  \u{27df} 12ms"
        );
        assert_eq!(
            w.sentence,
            "driven by manager, turn 41, busy, 2 unread, task from manager, agent, context 41 percent left"
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
        assert_eq!(width(&at(120)), 94, "the whole line: {}", at(120));
        assert!(!at(90).contains("\u{27df}"), "rtt goes first: {}", at(90));
        assert!(at(90).contains("3m12s"));
        assert!(!at(83).contains("3m12s"), "then since: {}", at(83));
        assert!(at(83).contains("worker:claude-satcomp"));
        assert!(!at(75).contains("worker:claude"), "then role: {}", at(75));
        assert!(at(75).contains("ctx 41% left"));
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

    /// RULING 270, "ONE PLACE TELLS IT": an attention whose only owner is
    /// the agent upgrade is still a FACT — `level=attention`, `why=escalation`,
    /// the tab chip's wait mark and the rim all keep it, so the stall shows at
    /// the top after its band row folds — but the presence line's phase slot
    /// does not repeat words the message band already says. NEGATIVE CONTROL:
    /// the same text from any other owner reads on the line.
    #[test]
    fn an_upgrade_only_attention_keeps_its_level_and_chip_but_not_its_words() {
        let now = t0();
        let text = "Codex 0.157.0 \u{2192} 0.157.1 stalled";
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                attention: Some(text.into()),
                attention_told_elsewhere: true,
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.level(0), Level::Attention);
        assert_eq!(s.why(0), "escalation");
        assert_eq!(s.chip(0), ChipLevel::Wait);
        // Round 18 (D1): the window's own chrome shows nothing for it — no
        // rim and no presence row, since the line has no words to say.
        assert_eq!(s.shown_level(0), Level::Quiet);
        assert_eq!(s.shown_level(0).rim(), Rim::None);
        assert!(!s.shown_level(0).shows_row());
        assert!(!s.calm(), "the stall is not folded by a keystroke");
        let w = words(&s, now, 0);
        assert!(
            !w.phase.contains("Codex") && !w.sentence.contains("Codex"),
            "the line repeats the band's words: {:?} / {:?}",
            w.phase,
            w.sentence
        );
        // NEGATIVE CONTROL: another owner's attention is the line's phase.
        let mut other = Slot::new(now);
        other.absorb(
            Facts {
                attention: Some(text.into()),
                ..Facts::default()
            },
            now,
        );
        assert_eq!(other.level(0), Level::Attention);
        assert_eq!(other.shown_level(0), Level::Attention);
        assert_eq!(other.shown_level(0).rim(), Rim::Wait);
        assert!(words(&other, now, 0).phase.contains("Codex"));
        // The flag moving alone is a change the view must re-read.
        assert!(other.absorb(
            Facts {
                attention: Some(text.into()),
                attention_told_elsewhere: true,
                ..Facts::default()
            },
            now,
        ));
        assert!(!words(&other, now, 0).phase.contains("Codex"));
    }

    /// AN ATTENTION TOLD ELSEWHERE NEVER TAKES THE ROW'S WORDS OR COLOUR
    /// (ruling 280): with a Claude upgrade stall standing (its row and record
    /// are the band's) and a story since the watermark, the row is shown at
    /// the story's level — and its words are the story's summary, its tone
    /// not Warn. Before 280 the row was shown but `words` and `tone` read the
    /// full level: no summary, painted Warn — a colour with no words.
    /// NEGATIVE CONTROL: the same story beside another owner's attention is
    /// that attention's words, in Warn.
    #[test]
    fn a_story_beside_an_attention_told_elsewhere_keeps_its_words_and_tone() {
        let now = t0();
        let text = "Claude 2.1.281 \u{2192} 2.1.282 stalled";
        let slot = |elsewhere: bool| {
            let mut s = Slot::new(now);
            for (id, at) in [(1, 5), (2, 70)] {
                s.absorb(
                    Facts {
                        turn: Some(TurnFact {
                            id,
                            settled: true,
                            dur_ms: 10,
                            carried: false,
                        }),
                        attention: Some(text.into()),
                        attention_told_elsewhere: elsewhere,
                        ..Facts::default()
                    },
                    now + Duration::from_secs(at),
                );
            }
            s
        };
        let later = now + Duration::from_secs(130);
        let s = slot(true);
        assert_eq!(s.level(0), Level::Attention, "the fact stands");
        assert_eq!(s.shown_level(0), Level::Story);
        let w = words(&s, later, 0);
        assert!(
            w.since.iter().any(|c| c.ends_with("turns")),
            "the story's summary: {:?}",
            w.since
        );
        assert_ne!(w.tone, Tone::Warn);
        assert_ne!(s.tone(later, 0), Tone::Warn);

        let other = slot(false);
        assert_eq!(other.shown_level(0), Level::Attention);
        let w = words(&other, later, 0);
        assert!(w.phase.contains("stalled"), "{:?}", w.phase);
        assert_eq!(w.tone, Tone::Warn);
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
                agent_seq: 2,
                agent: Some(AgentReading {
                    phase: AgentPhase::Wall {
                        kind: aterm_phase::WallKind::UsageSession,
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
        assert_eq!(s.chip(0), ChipLevel::Stop(StopCause::Wall));
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

    /// THE INCIDENT'S BAND (2026-09-24): an approval box on the screen, the
    /// supervisor's "answer this box" as typed attention, a supervisor's turn
    /// on the hand — and a published input stall. The slot stands at
    /// `Limited` (the stop rim), is not calm (a keystroke does not fold it),
    /// and its phase reads `frozen` with the stall's age and why, ahead of the
    /// attention text. A stopped job reads `stopped` and says to resume it.
    /// NEGATIVE CONTROL: the same facts with the stall cleared are the box's
    /// attention again, at `Attention`.
    #[test]
    fn a_stall_is_limited_not_calm_and_reads_frozen_over_typed_attention() {
        let now = t0();
        let fact = crate::input_stall::InputStallFact {
            word: aterm_session::input_backlog::InputWord::Stalled,
            since: now,
            bytes: 1,
            stopped: false,
            rss_mb: Some(39_731),
            restart: crate::input_stall::Restart::default(),
        };
        let facts = |stall: Option<crate::input_stall::InputStallFact>| Facts {
            attention: Some("answer this box: 4. Chat about this".into()),
            agent_seq: 1,
            agent: Some(AgentReading {
                phase: AgentPhase::Prompt {
                    detail: Some("question".into()),
                },
                context_pct: None,
            }),
            hand: Hand::DrivenTurn {
                id: 3,
                holder: Some("supervisor".into()),
            },
            input_stall: stall,
            ..Facts::default()
        };
        let mut s = Slot::new(now);
        assert!(s.absorb(facts(Some(fact.clone())), now));
        assert_eq!(s.level(0), Level::Limited);
        assert_eq!(s.level(0).rim(), Rim::Stop { hold: false });
        assert_eq!(s.chip(0), ChipLevel::Stop(StopCause::Frozen));
        assert!(!s.calm());
        assert_eq!(s.why(0), "-");
        let later = now + Duration::from_secs(123);
        let w = words(&s, later, 0);
        assert_eq!(w.phase, "frozen");
        assert_eq!(
            w.since,
            vec!["2m03s".to_string(), "not reading input".to_string()]
        );
        assert!(
            w.fit(120)
                .contains("frozen 2m03s \u{00b7} not reading input"),
            "{}",
            w.fit(120)
        );
        assert!(!w.fit(120).contains("answer this box"), "{}", w.fit(120));
        assert!(
            w.sentence
                .contains("frozen, not reading input for 2m03s; restart it"),
            "{}",
            w.sentence
        );
        assert_eq!(w.tone, Tone::Warn);
        // A stopped job: resume it.
        let stopped = crate::input_stall::InputStallFact {
            word: aterm_session::input_backlog::InputWord::Stopped,
            stopped: true,
            ..fact
        };
        assert!(s.absorb(facts(Some(stopped)), later));
        let w = words(&s, now + Duration::from_secs(41), 0);
        assert_eq!(w.phase, "stopped");
        assert_eq!(w.since, vec!["41s".to_string(), "input queued".to_string()]);
        assert!(
            w.sentence
                .contains("stopped with input queued for 41s; resume it"),
            "{}",
            w.sentence
        );
        assert_eq!(s.level(0), Level::Limited);
        assert_eq!(s.chip(0), ChipLevel::Stop(StopCause::Suspended));
        // NEGATIVE CONTROL: the stall clears — the box's attention is back.
        assert!(s.absorb(facts(None), later));
        assert_eq!(s.level(0), Level::Attention);
        assert_eq!(s.why(0), "prompt,escalation");
        let w = words(&s, later, 0);
        assert!(w.phase.starts_with("answer this box"), "{}", w.phase);
        // A hold still outranks it.
        let mut held = Slot::new(now);
        held.absorb(
            Facts {
                hold: Some(HoldFact {
                    reason: "review".into(),
                    fleet: false,
                }),
                ..facts(Some(fact_stalled(now)))
            },
            now,
        );
        assert_eq!(held.level(0), Level::Hold);
    }

    fn fact_stalled(since: Instant) -> crate::input_stall::InputStallFact {
        crate::input_stall::InputStallFact {
            word: aterm_session::input_backlog::InputWord::Stalled,
            since,
            bytes: 1,
            stopped: false,
            rss_mb: None,
            restart: crate::input_stall::Restart::default(),
        }
    }

    /// Ruling 313 (day eight, E1): the turn aterm's OWN harness typed to
    /// relaunch an agent after a kill raised a teal rim and `◂
    /// aterm-harness@<pid>` while it typed, then `◇ quiet since … · 1 turn`
    /// standing until a key — FYI the restart record already told. aterm's
    /// hand is not shown by the window's chrome and what happens under it is
    /// not news; `status level=` keeps the hand. Controls: the same exchange
    /// under a manager's hand is Driven and then a story, and a story from
    /// before aterm's hand stays news, counted without aterm's turn.
    #[test]
    fn aterms_own_hand_is_no_row_and_its_turn_is_no_story() {
        let now = t0();
        let turn = |id| {
            Some(TurnFact {
                id,
                settled: true,
                dur_ms: 10,
                carried: false,
            })
        };
        // `inside`: the settled turn is first read while the turn's own lease
        // still stands (a refresh between the ledger push and the hand-back).
        let run = |holder: String, before: Option<TurnFact>, inside: bool| {
            let mut s = Slot::new(now);
            s.absorb(
                Facts {
                    turn: before,
                    ..Facts::default()
                },
                now,
            );
            let lease = Hand::DrivenLease { holder };
            let mut shown = Vec::new();
            let mut at = now;
            // The lease, the turn typed inside it, the settle as it is handed
            // back, and the lease given back — each a refresh of its own.
            for (hand, t) in [
                (lease.clone(), before),
                (
                    Hand::DrivenTurn {
                        id: 2,
                        holder: None,
                    },
                    if inside { turn(2) } else { before },
                ),
                (lease.clone(), turn(2)),
                (Hand::None, turn(2)),
            ] {
                at += Duration::from_secs(1);
                s.absorb(
                    Facts {
                        hand,
                        turn: t,
                        ..Facts::default()
                    },
                    at,
                );
                shown.push(s.shown_level(0));
            }
            (s, shown, at)
        };
        let own = crate::harness_host::holder();
        for inside in [false, true] {
            let (s, shown, _) = run(own.clone(), None, inside);
            assert_eq!(
                shown,
                [Level::Quiet; 4],
                "aterm's hand raised a row ({inside})"
            );
            assert_eq!(
                s.level(0),
                Level::Quiet,
                "aterm's turn read as news ({inside})"
            );
            assert_eq!(s.story_seq, 1, "the point is still kept (`story=`)");
            assert_eq!(s.chip(0), ChipLevel::Off, "no story dot on the tab");
        }
        let mut held = Slot::new(now);
        held.absorb(
            Facts {
                hand: Hand::DrivenLease {
                    holder: own.clone(),
                },
                ..Facts::default()
            },
            now,
        );
        assert_eq!(
            held.level(0),
            Level::Driven,
            "`status level=` keeps the hand"
        );
        assert_eq!(held.shown_level(0), Level::Quiet);

        // Control: somebody else's hand is shown, and its turn is a story.
        let (_, shown, _) = run("manager".into(), None, true);
        assert_eq!(shown[..3], [Level::Driven; 3], "a manager's hand is shown");
        assert_eq!(shown[3], Level::Story, "a manager's turn is news");
        // Control: another instance's harness is somebody else's hand.
        let (_, shown, _) = run("aterm-harness@1".into(), None, false);
        assert_eq!(shown[0], Level::Driven);

        // A story from before aterm's hand stays news, without aterm's turn.
        let (s, shown, at) = run(own, turn(1), false);
        assert_eq!(shown[3], Level::Story, "the earlier turn is still news");
        let w = words(&s, at, 0);
        assert!(w.since.iter().any(|c| c == "1 turn"), "{w:?}");
        assert!(!w.since.iter().any(|c| c == "2 turns"), "{w:?}");
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
        assert_eq!(s.chip(0), ChipLevel::Story);
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

    /// D10 (2026-09-24): the story an agent told while nobody looked closes
    /// when the agent LEAVES — a shell does not stand at `level=story` for a
    /// run that is over, least of all in a headless instance where no person
    /// ever acts to read it. NEGATIVE CONTROL: the same story with the agent
    /// still there stands, and a new point after the exit is news again.
    #[test]
    fn an_agents_story_closes_when_the_agent_leaves() {
        let now = Instant::now();
        let reading = |phase| AgentReading {
            phase,
            context_pct: None,
        };
        let mut s = Slot::new(now);
        s.absorb(Facts::default(), now);
        for (seq, phase) in [(1, AgentPhase::Question), (2, AgentPhase::Idle)] {
            s.absorb(
                Facts {
                    agent_seq: seq,
                    agent: Some(reading(phase)),
                    ..Facts::default()
                },
                now,
            );
        }
        assert_eq!(
            s.level(0),
            Level::Story,
            "the question is news while it runs"
        );
        assert!(s.story_since(0).next().is_some());
        s.absorb(
            Facts {
                agent_seq: 3,
                agent: None,
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.level(0), Level::Quiet, "the agent left: its story closed");
        assert_eq!(s.story_since(0).count(), 0);
        s.absorb(
            Facts {
                turn: Some(TurnFact {
                    id: 1,
                    settled: true,
                    dur_ms: 5,
                    carried: false,
                }),
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.level(0), Level::Story, "a point after the exit is news");
        assert_eq!(s.story_since(0).count(), 1);
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
                agent_seq: 1,
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
        assert_eq!(w.phase, "bash approval");
        assert_eq!(w.since, ["0s"], "the wait's age, as a question's");
        assert_eq!(w.ctx, "ctx 12% left \u{26a0}");
        // The classifier's verdict and an unnamed kind stay off the band.
        assert_eq!(
            prompt_band_word(Some("bash:not-read-only")),
            "bash approval"
        );
        assert_eq!(prompt_band_word(Some("other")), "approval");
        assert_eq!(prompt_band_word(None), "approval");
        // The question tool asks; it approves nothing.
        assert_eq!(prompt_band_word(Some("question")), "question");
        assert_eq!(prompt_band_word(Some("plan-exit")), "plan approval");
        assert_eq!(
            prompt_band_word(Some("read-outside-setting")),
            "outside read approval"
        );
        // A box that follows another, with no busy reading between, starts
        // its own age; the same box's next reading keeps it.
        let box_of = |seq, detail: &str| Facts {
            agent_seq: seq,
            agent: Some(AgentReading {
                phase: AgentPhase::Prompt {
                    detail: Some(detail.into()),
                },
                context_pct: Some(12),
            }),
            ..Facts::default()
        };
        let later = now + Duration::from_secs(65);
        s.absorb(box_of(2, "bash:not-read-only"), later);
        assert_eq!(words(&s, later, 0).since, ["1m05s"], "the same box");
        s.absorb(box_of(3, "edit"), later);
        let w = words(&s, later, 0);
        assert_eq!(w.phase, "edit approval");
        assert_eq!(w.since, ["0s"], "a new box's own age");
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
                agent_seq: 1,
                agent: Some(AgentReading {
                    phase: AgentPhase::Wall {
                        kind: aterm_phase::WallKind::UsageSession,
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

    /// The classifier is `aterm-phase`'s Claude reader, word for word — a
    /// reading that is not evidence ([`aterm_phase::Reading::
    /// phase_authoritative`]: Claude Code's idle with no composer drawn) is
    /// `unknown` — and its count moves once per call (the gate test in
    /// `session_status` compares it with screen changes). The screens are
    /// judged by `aterm_phase` itself, so the mapping — not the composer
    /// geometry — is what this pins.
    #[test]
    fn classify_is_aterm_phases_verdict_and_counts_once_per_call() {
        let before = classifier_calls();
        let rows = |body: &[&str]| body.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut done_at_composer = rows(&["\u{23fa} Done.", ""]);
        done_at_composer.extend(aterm_phase::prompt::fixtures::composer("  ? for shortcuts"));
        let screens = [
            rows(&["\u{23fa} Which do you prefer?", ""]),
            rows(&["\u{23fa} Done.", ""]),
            done_at_composer,
            rows(&[
                "\u{23fa} Running the tests.",
                "",
                "\u{00b7} Cogitating\u{2026} (12s)",
            ]),
            rows(&[]),
        ];
        let mut words = Vec::new();
        for screen in &screens {
            let verdict = aterm_phase::read(Some("claude"), screen, None);
            let (ours, _) = classify(
                &aterm_phase::ClaudeReader,
                screen,
                Cursor::Unknown,
                Instant::now(),
            );
            let want = if verdict.phase_authoritative {
                verdict.phase.name()
            } else {
                "unknown"
            };
            assert_eq!(ours.phase.word(), want, "{screen:?}");
            assert_eq!(ours.context_pct, verdict.context_left);
            words.push(ours.phase.word());
        }
        assert_eq!(
            words,
            ["question", "unknown", "idle", "busy", "unknown"],
            "both halves of the mapping are exercised"
        );
        assert_eq!(classifier_calls() - before, screens.len() as u64);
        // The never-shown law at the classifier's own edge: a limit notice
        // keeps only its kind and its reset, never its message.
        let (limited, _) = classify(
            &aterm_phase::ClaudeReader,
            &rows(&[
                "\u{23fa} You've hit your weekly limit \u{00b7} resets Sep 19 at 11am (America/Los_Angeles)",
                "",
            ]),
            Cursor::Unknown,
            Instant::now(),
        );
        if let AgentPhase::Wall { reset, .. } = &limited.phase {
            assert!(
                reset.as_deref().is_none_or(|r| !r.contains("weekly")),
                "{reset:?}"
            );
        }
    }

    /// The published verdict behind the reader's panic fence: a panicking
    /// stand-in for the reader does not unwind out of the sweep's call (the
    /// window's thread lives), and degrades to the no-evidence reading — an
    /// agent identified by its program name (or by the frame this job already
    /// showed) is that agent at `unknown`, never a prompt; one that only its
    /// screen could identify is no agent. Controls: the guarded verdict of a
    /// real screen is the unguarded one, box and subject included.
    #[test]
    fn a_reader_panic_degrades_the_verdict_to_unknown_and_the_thread_lives() {
        let rows = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::BOX_RM);
        let boom = || -> AgentVerdict { panic!("stand-in reader panic") };
        let unknown = |v: &AgentVerdict| {
            matches!(
                v,
                AgentVerdict::Agent {
                    reading: AgentReading {
                        phase: AgentPhase::Unknown,
                        context_pct: None
                    },
                    by_frame: false,
                    subject: None,
                    ..
                }
            )
        };
        let claude = fenced_verdict(9_001, Some("claude"), false, None, &rows, boom);
        assert!(unknown(&claude), "{claude:?}");
        assert_eq!(claude.program(), Some(aterm_phase::Program::Claude));
        assert_eq!(claude.word(), "unknown");
        let codex = fenced_verdict(9_001, Some("codex"), false, None, &rows, boom);
        assert_eq!(codex.program(), Some(aterm_phase::Program::Codex));
        assert!(unknown(&codex), "{codex:?}");
        // By the frame this job showed, under a runtime or no name at all.
        let node = fenced_verdict(
            9_001,
            Some("node"),
            false,
            Some(aterm_phase::Program::Claude),
            &rows,
            boom,
        );
        assert!(unknown(&node), "{node:?}");
        let codex_job = fenced_verdict(
            9_001,
            None,
            false,
            Some(aterm_phase::Program::Codex),
            &rows,
            boom,
        );
        assert!(unknown(&codex_job));
        assert_eq!(codex_job.program(), Some(aterm_phase::Program::Codex));
        // Only the screen could have identified these.
        let claude = Some(aterm_phase::Program::Claude);
        for (program, pending, known) in [
            (Some("node"), false, None),
            (None, false, None),
            (None, true, claude),
            (Some("zsh"), false, claude),
        ] {
            let v = fenced_verdict(9_001, program, pending, known, &rows, boom);
            assert_eq!(v, AgentVerdict::NotAgent, "{program:?} {pending} {known:?}");
        }
        let guarded = agent_verdict_guarded(
            9_001,
            Some("claude"),
            false,
            None,
            &rows,
            Cursor::Unknown,
            t0(),
        );
        let plain = agent_verdict(Some("claude"), false, None, &rows, Cursor::Unknown, t0());
        assert_eq!(guarded, plain);
        assert_eq!(guarded.word(), "prompt");
        assert!(guarded.subject().is_some());
    }

    /// `agent=wall:<kind>` is `wall:` and aterm-phase's own kind name, for
    /// every kind; the band keeps `limited` for exactly the kinds
    /// `worker_phase` has always read so.
    #[test]
    fn a_wall_word_is_wall_and_aterm_phases_kind_name() {
        use aterm_phase::WallKind as K;
        let kinds = [
            K::UsageSession,
            K::UsageWeekly,
            K::ModelBucket { consent: false },
            K::ModelBucket { consent: true },
            K::Spend,
            K::Context,
            K::Auth,
            K::ApiError {
                code: Some(500),
                retryable: true,
                cause: aterm_phase::ApiCause::Server,
            },
            K::ApiError {
                code: Some(429),
                retryable: true,
                cause: aterm_phase::ApiCause::Server,
            },
            K::Overloaded,
            K::Memory,
        ];
        for kind in kinds {
            assert_eq!(wall_word(kind), format!("wall:{}", kind.name()));
            let phase = AgentPhase::Wall {
                kind,
                reset: None,
                until: None,
            };
            // The state the wall leaves the session in, the menu bar's words.
            let band = match kind {
                _ if kind.reads_limited() => "limited",
                K::Context => "context full",
                K::Auth => "logged out",
                K::Memory => "memory critical",
                K::ApiError { .. } => "API error",
                K::Overloaded => "service overloaded",
                other => panic!("{other:?} reads limited"),
            };
            assert_eq!(phase.band_word(), band, "{kind:?}");
        }
        assert_eq!(AgentPhase::Unknown.word(), "unknown");
    }

    /// Lane A's fixtures, through the ONE adapter: each wall reads
    /// `wall:<kind>` under `program=claude` — a 529 included (it read `idle`
    /// before, and a supervisor trusting idle waited forever), and the real
    /// Fable screen. NEGATIVE CONTROLS: a tool's output QUOTING `API Error:
    /// 529` under a `⏺ Bash(…)` call stays `idle`; every wall screen under a
    /// shell is not an agent.
    #[test]
    fn a_wall_is_published_by_kind() {
        use aterm_phase::prompt::fixtures::{
            END_529, END_SESSION_LIMIT, IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, composer, screen,
        };
        let framed = |body: &[&str]| {
            let mut r: Vec<String> = body.iter().map(|s| s.to_string()).collect();
            r.extend(composer("  ? for shortcuts"));
            r
        };
        // The real Fable screen cut before its `/model`, as aterm-phase's own
        // wall test cuts it.
        let full: Vec<String> = IDLE_AFTER_LIMIT_AND_MODEL_SWITCH
            .lines()
            .map(str::to_string)
            .collect();
        let mut fable = full[..56].to_vec();
        fable.extend_from_slice(&full[58..]);
        let context = framed(&[
            "\u{23fa} Reading the remaining modules.",
            "  \u{23bf}  Context limit reached \u{00b7} /compact or /clear to continue",
            "",
        ]);
        let quoted = framed(&[
            "\u{23fa} Bash(tail -1 build.log)",
            "  \u{23bf}  API Error: 529 Overloaded. This is a server-side issue, usually temporary.",
            "",
        ]);
        let cases: [(&str, Vec<String>, &str); 5] = [
            ("529", screen(END_529), "wall:overloaded"),
            ("session", screen(END_SESSION_LIMIT), "wall:usage-session"),
            ("fable", fable, "wall:model-bucket"),
            ("context", context, "wall:context"),
            ("quoted 529", quoted, "idle"),
        ];
        for (name, rows, want) in &cases {
            let v = agent_verdict(Some("claude"), false, None, rows, Cursor::Unknown, t0());
            assert_eq!(v.word(), *want, "{name}");
            let shell = agent_verdict(Some("zsh"), false, None, rows, Cursor::Unknown, t0());
            assert_eq!(shell.word(), "-", "{name}: a shell is never an agent");
        }
        // The detail is the reset the notice named, nothing of its text.
        let session = agent_verdict(
            Some("claude"),
            false,
            None,
            &cases[1].1,
            Cursor::Unknown,
            t0(),
        );
        assert_eq!(
            session.detail().as_deref(),
            Some("3pm (America/Los_Angeles)")
        );
    }

    /// THE SERVER PUBLISHES CLAUDE CODE IDLE ONLY AT ITS COMPOSER (the live
    /// e2e of 2026-09-26, NEW-1: `agent=idle` went out 72-248 ms after the
    /// harness pressed the folder-trust dialog, `await agent idle` returned,
    /// and the first prompt sent then was lost or left unsent). The screens
    /// Claude Code 2.1.283 showed between its launch and its REPL (measured
    /// frame by frame: the shell's rows with the dialog erased, then the
    /// alternate screen half drawn) publish `unknown` — an agent whose
    /// screen is no evidence, never acted on as idle — and the REPL drawn
    /// whole publishes `idle` (the control), in shell mode too (`!` typed
    /// into the empty prompt box: its caret `!`, the REPL up and taking keys
    /// — `idle`, as every build before this rule published it). The dialog
    /// itself is still a prompt, and the same rows under a shell are no
    /// agent.
    #[test]
    fn claude_code_is_published_idle_only_at_its_composer() {
        use aterm_phase::prompt::fixtures::{
            LAUNCH_BEFORE_REPL, LAUNCH_REPL_HALF_DRAWN, LAUNCH_REPL_READY, SHELL_MODE,
            SHELL_MODE_DRAFT, TRUST, screen,
        };
        for (name, text) in [
            ("before the REPL", LAUNCH_BEFORE_REPL),
            ("the REPL half drawn", LAUNCH_REPL_HALF_DRAWN),
        ] {
            let v = agent_verdict(
                Some("claude"),
                false,
                None,
                &screen(text),
                Cursor::Unknown,
                t0(),
            );
            assert_eq!(v.word(), "unknown", "{name}: {v:?}");
            assert_eq!(v.program(), Some(aterm_phase::Program::Claude), "{name}");
            let shell = agent_verdict(
                Some("zsh"),
                false,
                None,
                &screen(text),
                Cursor::Unknown,
                t0(),
            );
            assert_eq!(shell.word(), "-", "{name}: a shell is never an agent");
        }
        let ready = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(LAUNCH_REPL_READY),
            Cursor::Unknown,
            t0(),
        );
        assert_eq!(ready.word(), "idle", "the control: {ready:?}");
        for (name, text) in [
            ("shell mode", SHELL_MODE),
            ("shell mode, a command typed", SHELL_MODE_DRAFT),
        ] {
            let v = agent_verdict(
                Some("claude"),
                false,
                None,
                &screen(text),
                Cursor::Unknown,
                t0(),
            );
            assert_eq!(v.word(), "idle", "{name}: {v:?}");
        }
        let dialog = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(TRUST),
            Cursor::Unknown,
            t0(),
        );
        assert_eq!(
            (dialog.word(), dialog.detail().as_deref()),
            ("prompt", Some("trust"))
        );
    }

    /// CLAUDE CODE'S INLINE REPL IS PUBLISHED IDLE WHERE IT IS DRAWN (the
    /// review of 2026-09-26): its inline renderer drew the REPL at the top of
    /// a 150x50 pane, blank rows below it, and the server read the grid's
    /// last 40 rows — the prompt box's bottom rule and footer, never its
    /// caret — so `agent=` stayed `unknown` and `await agent idle` timed out
    /// (measured on 2.1.283, 8 of 8 launches). The verdict reads the last 40
    /// DRAWN rows ([`live_zone`]): the REPL is `idle` at 50 rows and in any
    /// taller pane, and under `node` (identified by its frame, which the old
    /// cut never showed it). The inline launch's other frames keep their
    /// words: the dialog is `prompt trust`, the half-drawn REPL and the
    /// shell's rows `unknown`; the same rows under a shell are no agent. The
    /// fullscreen REPL, drawn to its last row, is `idle` as it was.
    #[test]
    fn an_inline_repl_at_the_top_of_a_tall_pane_is_published_idle() {
        use aterm_phase::prompt::fixtures::{
            INLINE_REPL_HALF_DRAWN, INLINE_REPL_READY, INLINE_TRUST, LAUNCH_BEFORE_REPL,
            LAUNCH_REPL_READY, screen,
        };
        let pad = |text: &str, rows: usize| {
            let mut r = screen(text);
            r.resize(rows.max(r.len()), String::new());
            r
        };
        for rows in [50, 51, 80, 200] {
            let ready = pad(INLINE_REPL_READY, rows);
            for program in ["claude", "node"] {
                let v = agent_verdict(Some(program), false, None, &ready, Cursor::Unknown, t0());
                assert_eq!(v.word(), "idle", "{program} at {rows} rows: {v:?}");
                assert_eq!(v.program(), Some(aterm_phase::Program::Claude));
            }
            let shell = agent_verdict(Some("zsh"), false, None, &ready, Cursor::Unknown, t0());
            assert_eq!(shell.word(), "-", "a shell is never an agent");
            let dialog = agent_verdict(
                Some("claude"),
                false,
                None,
                &pad(INLINE_TRUST, rows),
                Cursor::Unknown,
                t0(),
            );
            assert_eq!(
                (dialog.word(), dialog.detail().as_deref()),
                ("prompt", Some("trust")),
                "{rows} rows"
            );
            for (name, text) in [
                ("the REPL half drawn", INLINE_REPL_HALF_DRAWN),
                ("the shell's rows", LAUNCH_BEFORE_REPL),
            ] {
                let v = agent_verdict(
                    Some("claude"),
                    false,
                    None,
                    &pad(text, rows),
                    Cursor::Unknown,
                    t0(),
                );
                assert_eq!(v.word(), "unknown", "{name} at {rows} rows: {v:?}");
            }
        }
        let full = screen(LAUNCH_REPL_READY);
        assert_eq!(full.len(), 50);
        let v = agent_verdict(Some("claude"), false, None, &full, Cursor::Unknown, t0());
        assert_eq!(v.word(), "idle", "the fullscreen REPL: {v:?}");
    }

    /// CLAUDE CODE IS PUBLISHED IDLE AT THE PROMPT BOX THAT HOLDS THE CURSOR
    /// (the review of 2026-09-26): its inline renderer relaunched in the SAME
    /// tab leaves the previous run's prompt box on the main grid above the
    /// new launch line, and the server — reading the box by its frame alone
    /// — published `agent=idle` from it 187-352 ms before the new REPL was
    /// drawn; a draft typed on `await agent idle` was lost 3 of 3 (RED: the
    /// first assertion, the cursor not given). The sweep reads the verdict
    /// with the terminal's cursor, measured under the new launch line there:
    /// `unknown`, in the measured 50-row pane and padded to 80 rows. The
    /// relaunch's other frames keep their words with their measured cursors:
    /// the dialog `prompt trust`, the new REPL half drawn `unknown`, whole
    /// `idle`. NEGATIVE CONTROLS: the whole new REPL with the cursor moved
    /// one row under its box, or into the OLD box's rows, is not idle the
    /// first way and is idle the second (a box holds its rows: the cursor is
    /// what Claude Code keeps in the box that is running); the cursor on no
    /// row read is not idle.
    #[test]
    fn a_relaunched_inline_repl_is_published_idle_only_at_the_box_that_holds_the_cursor() {
        use aterm_phase::prompt::fixtures::{
            INLINE_RELAUNCH_BEFORE_REPL, INLINE_RELAUNCH_REPL_HALF_DRAWN,
            INLINE_RELAUNCH_REPL_READY, INLINE_RELAUNCH_TRUST, cursor, screen,
        };
        let at = |text: &str| Cursor::At(cursor(text).map(|(row, _)| row));
        let stale = screen(INLINE_RELAUNCH_BEFORE_REPL);
        let v = agent_verdict(Some("claude"), false, None, &stale, Cursor::Unknown, t0());
        assert_eq!(v.word(), "idle", "RED: the old box by its frame alone");
        for rows in [50, 80] {
            let pad = |text: &str| {
                let mut r = screen(text);
                r.resize(rows, String::new());
                r
            };
            for (name, text, word, detail) in [
                (
                    "before the new REPL",
                    INLINE_RELAUNCH_BEFORE_REPL,
                    "unknown",
                    None,
                ),
                ("the dialog", INLINE_RELAUNCH_TRUST, "prompt", Some("trust")),
                (
                    "the new REPL half drawn",
                    INLINE_RELAUNCH_REPL_HALF_DRAWN,
                    "unknown",
                    None,
                ),
                ("the new REPL", INLINE_RELAUNCH_REPL_READY, "idle", None),
            ] {
                let v = agent_verdict(Some("claude"), false, None, &pad(text), at(text), t0());
                assert_eq!(
                    (v.word(), v.detail().as_deref()),
                    (word, detail),
                    "{name} at {rows} rows: {v:?}"
                );
                assert_eq!(v.program(), Some(aterm_phase::Program::Claude));
            }
        }
        // NEGATIVE CONTROLS on the whole new REPL.
        let ready = screen(INLINE_RELAUNCH_REPL_READY);
        let (caret, _) = cursor(INLINE_RELAUNCH_REPL_READY).expect("measured");
        let bottom = (caret..ready.len())
            .find(|&i| ready[i].starts_with('─'))
            .expect("the new bottom rule");
        let old_caret = ready
            .iter()
            .position(|r| r.starts_with('❯'))
            .expect("the old caret");
        assert!(old_caret < caret, "the old box is above the new one");
        for (name, cursor, word) in [
            ("on the new caret", Cursor::At(Some(caret)), "idle"),
            ("on the new bottom rule", Cursor::At(Some(bottom)), "idle"),
            ("under the new box", Cursor::At(Some(bottom + 1)), "unknown"),
            ("on no row read", Cursor::At(None), "unknown"),
            ("in the old box", Cursor::At(Some(old_caret)), "idle"),
        ] {
            let v = agent_verdict(Some("claude"), false, None, &ready, cursor, t0());
            assert_eq!(v.word(), word, "the cursor {name}: {v:?}");
        }
    }

    /// Claude Code's critical-memory banner under a RUNNING spinner (the
    /// 2026-09-24 incident: 36 minutes into a turn, 38.8 GiB resident, no
    /// input read for 2h41m) publishes `wall:memory`, not `busy` — the one
    /// wall aterm-phase keeps under a hard busy — with no detail and the
    /// band word `memory`, never `limited`. NEGATIVE CONTROLS: the same
    /// words in the composer draft alone read `busy`, and under a shell the
    /// screen is no agent. The row is built from aterm-phase's anchor.
    #[test]
    fn the_memory_banner_publishes_wall_memory_under_a_busy_spinner() {
        use aterm_phase::prompt::fixtures::composer;
        let banner = format!(
            "{} (140.4GB) \u{2014} restart and resume with {}",
            aterm_phase::anchor_text("wall.memory"),
            aterm_phase::resume_hint(aterm_phase::Program::Claude).unwrap()
        );
        let screen = |banner_row: &str, draft: &str| {
            let mut r = vec![
                "\u{23fa} Running the reflow suite.".to_string(),
                String::new(),
                "\u{00b7} Gesticulating\u{2026} (36m 1s)".to_string(),
                banner_row.to_string(),
            ];
            let mut frame =
                composer("  \u{23f5}\u{23f5} bypass permissions on \u{00b7} esc to interrupt");
            frame[1] = format!("\u{276f} {draft}");
            r.extend(frame);
            r
        };
        // Right-aligned, ending two columns short of the 120-column rule.
        let live = screen(&format!("{banner:>118}"), "");
        let v = agent_verdict(Some("claude"), false, None, &live, Cursor::Unknown, t0());
        assert_eq!(v.word(), "wall:memory");
        assert_eq!(v.detail(), None);
        match &v {
            AgentVerdict::Agent { reading, .. } => {
                assert_eq!(reading.phase.band_word(), "memory critical");
            }
            AgentVerdict::NotAgent => panic!("claude is an agent by name"),
        }
        assert!(!aterm_phase::WallKind::Memory.reads_limited());
        let quoted = screen("", &banner);
        assert_eq!(
            agent_verdict(Some("claude"), false, None, &quoted, Cursor::Unknown, t0()).word(),
            "busy",
            "the draft's quote"
        );
        assert_eq!(
            agent_verdict(Some("zsh"), false, None, &live, Cursor::Unknown, t0()).word(),
            "-"
        );
    }

    /// A reader with no evidence publishes `unknown`, never its default
    /// `idle`: a Codex screen outside its choice box. NEGATIVE CONTROL: the
    /// Codex trust gate itself is read, as a prompt with its subject.
    #[test]
    fn a_reading_without_evidence_is_unknown_not_idle() {
        use aterm_phase::prompt::fixtures::{CODEX_TRUST, screen};
        let idle = screen("\u{203a} ready\n\n  gpt-5 \u{00b7} 100% context left\n");
        match agent_verdict(Some("codex"), false, None, &idle, Cursor::Unknown, t0()) {
            AgentVerdict::Agent { reading, .. } => {
                assert_eq!(reading.phase, AgentPhase::Unknown);
                assert_eq!(reading.phase.word(), "unknown");
            }
            AgentVerdict::NotAgent => panic!("codex is an agent by name"),
        }
        let gate = agent_verdict(
            Some("codex"),
            false,
            None,
            &screen(CODEX_TRUST),
            Cursor::Unknown,
            t0(),
        );
        assert_eq!(gate.word(), "prompt");
    }

    /// The box's subject rides the verdict, from the same reading: the
    /// measured rm box names its command; a non-prompt names nothing.
    #[test]
    fn the_prompt_subject_comes_from_the_reading() {
        use aterm_phase::prompt::fixtures::{BOX_RM, END_529, screen};
        let rm = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(BOX_RM),
            Cursor::Unknown,
            t0(),
        );
        assert_eq!(rm.word(), "prompt");
        let subject = rm.subject().expect("the rm box names its command");
        assert!(subject.contains("rm"), "{subject}");
        assert!(!subject.contains('\n'));
        let wall = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(END_529),
            Cursor::Unknown,
            t0(),
        );
        assert_eq!(wall.subject(), None);
    }

    /// AN API ERROR SAYS WHAT WENT WRONG, NEVER A PLAN (the outage of
    /// 2026-09-27): Claude Code 2.1.283's `⏺ API Error: Can't reach the API
    /// server … (ENOTFOUND)` is `can't reach the API` on the tab and in the
    /// menu row — not `offline` (a refused connection or a timeout is no
    /// machine offline), and not `continuing when …`: nothing here knows
    /// the supervisor's plan — a reply cut off is `reply cut off`, a
    /// certificate or proxy refused `TLS/proxy refused`. The wire word stays
    /// `wall:api-error` for every cause. NEGATIVE CONTROLS: the server's own
    /// failure (a 529, a 503) keeps its kind's word and names no subject.
    #[test]
    fn an_api_wall_says_its_cause_and_claims_no_plan() {
        use aterm_phase::prompt::fixtures::{
            API_ERROR_529, API_ERROR_ENOTFOUND, API_ERROR_SLEEP, screen,
        };
        use aterm_phase::{ApiCause, WallKind};
        let wall = |cause| AgentPhase::Wall {
            kind: WallKind::ApiError {
                code: None,
                retryable: cause != ApiCause::Config,
                cause,
            },
            reset: None,
            until: None,
        };
        for (cause, band) in [
            (ApiCause::Unreachable, "can't reach the API"),
            (ApiCause::CutOff, "reply cut off"),
            (ApiCause::Config, "TLS/proxy refused"),
            (ApiCause::Server, "API error"),
        ] {
            let phase = wall(cause);
            assert_eq!(phase.band_word(), band, "{cause:?}");
            assert_eq!(phase.word(), "wall:api-error", "{cause:?}");
            for claim in ["offline", "continuing", "reachable"] {
                assert!(!band.contains(claim), "{band}");
            }
        }
        let now = t0();
        let out = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(API_ERROR_ENOTFOUND),
            Cursor::Unknown,
            now,
        );
        assert_eq!(out.word(), "wall:api-error");
        assert_eq!(out.subject(), Some(API_UNREACHABLE));
        let mut slot = Slot::new(now);
        slot.agent = out.reading().cloned();
        let said = words(&slot, now, 0);
        assert_eq!(said.phase, "can't reach the API", "{said:?}");
        assert!(!said.sentence.contains("offline"), "{}", said.sentence);
        let cut = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(API_ERROR_SLEEP),
            Cursor::Unknown,
            now,
        );
        assert_eq!(cut.subject(), Some(API_CUT_OFF));
        // The controls: the server's own failure.
        let overloaded = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(API_ERROR_529),
            Cursor::Unknown,
            now,
        );
        assert_eq!(overloaded.word(), "wall:overloaded");
        assert_eq!(overloaded.subject(), None);
        assert_eq!(
            AgentPhase::Wall {
                kind: WallKind::ApiError {
                    code: Some(503),
                    retryable: true,
                    cause: ApiCause::Server,
                },
                reset: None,
                until: None,
            }
            .band_word(),
            "API error"
        );
    }

    /// THE FRESH PANE (the live run of 2026-09-26: Claude Code 2.1.283 in a
    /// private headless aterm, a fresh 149x62 pane whose shell prompt sat at
    /// the top). The folder-trust dialog was drawn on rows 5-20, wholly above
    /// the last 40 rows of the screen, every one of them blank, and `status`
    /// published `agent=idle` for 3+ minutes: the zone was the last 40 rows
    /// of the SCREEN. It is the last 40 rows of the screen's CONTENT
    /// ([`live_zone`]): the dialog is published a trust prompt — by the
    /// program's name, and by its frame while the program is unnamed. The
    /// same session's subagent Bash box with the rm breaker's note, drawn at
    /// the top of the pane, likewise (its command the subject, clipped). NEGATIVE
    /// CONTROLS: an idle Claude composer at the top of the same pane, blank
    /// rows under it, stays idle; and a screen whose last row has content
    /// keeps its last 40 rows as its zone, unchanged. Each screen is read with
    /// the cursor where Claude Code parks it ([`Cursor`]): on the focused
    /// option of a box (the measured row 17 of the dialog), on the caret row
    /// of its prompt box.
    #[test]
    fn a_box_above_the_last_rows_of_a_mostly_blank_pane_is_classified() {
        use aterm_phase::prompt::fixtures::{self as f, composer, screen};
        let on = |rows: &[String], row: fn(&str) -> bool| {
            Cursor::At(Some(
                rows.iter().position(|r| row(r)).expect("the cursor's row"),
            ))
        };
        let blank_foot = |rows: &[String]| {
            rows[rows.len() - CLASSIFY_ROWS..]
                .iter()
                .all(String::is_empty)
        };
        let trust = screen(f::TRUST_FRESH_PANE);
        assert_eq!(trust.len(), 62);
        assert!(
            blank_foot(&trust),
            "the dialog is wholly above the last 40 rows"
        );
        assert_eq!(
            live_zone(&trust),
            &trust[..],
            "21 rows of content: all of it"
        );
        let focus = on(&trust, |r| r == " ❯ No, exit");
        assert_eq!(focus, Cursor::At(Some(17)), "the measured cursor");
        for program in [Some("claude"), None] {
            let v = agent_verdict(program, false, None, &trust, focus, t0());
            assert_eq!(v.word(), "prompt", "{program:?}");
            assert_eq!(v.detail().as_deref(), Some("trust"), "{program:?}");
            assert_eq!(v.program(), Some(aterm_phase::Program::Claude));
        }

        let live = screen(f::BOX_RM_SUBAGENT_FRESH_PANE);
        let mut boxed = vec![String::new()];
        boxed.extend_from_slice(&live[22..=40]);
        boxed.resize(62, String::new());
        assert!(blank_foot(&boxed));
        let focus = on(&boxed, |r| r == " ❯ 1. Yes");
        for program in [Some("claude"), None] {
            let v = agent_verdict(program, false, None, &boxed, focus, t0());
            assert_eq!(v.word(), "prompt", "{program:?}");
            assert_eq!(
                v.detail().as_deref(),
                Some("bash:not-read-only"),
                "{program:?}"
            );
            assert!(
                v.subject()
                    .is_some_and(|s| s.starts_with("cd /var/folders/")),
                "{:?}",
                v.subject()
            );
        }

        let mut idle = aterm_phase::prompt::fixtures::rows(&[
            "\u{23fa} Done.",
            "",
            "\u{273b} Cogitated for 4s \u{00b7} done 2:41 PM",
            "",
        ]);
        idle.extend(composer("  ? for shortcuts"));
        idle.resize(62, String::new());
        assert!(blank_foot(&idle));
        let caret = on(&idle, |r| r.starts_with('❯'));
        for program in [Some("claude"), None] {
            let v = agent_verdict(program, false, None, &idle, caret, t0());
            assert_eq!(v.word(), "idle", "{program:?}");
        }

        // The live capture's content ends on row 40: its zone is the last
        // 40 rows of that content and the blank foot under it. A screen with
        // content on its last row: its last 40 rows, as it always was; an
        // all-blank one reads whole.
        assert_eq!(live_zone(&live), &live[41 - CLASSIFY_ROWS..]);
        let mut full = trust.clone();
        full[61] = "x".to_string();
        assert_eq!(live_zone(&full), &full[62 - CLASSIFY_ROWS..]);
        let blank = vec![String::new(); 62];
        assert_eq!(live_zone(&blank).len(), 62);
    }

    /// The countdown is measured on THIS machine's clock, through the
    /// supervisor's own reset grammar (`supervise::limit`, whose tests own
    /// the words): a bare clock time two hours ahead counts down two hours;
    /// one an hour ago has fallen (a session limit resets within five hours,
    /// so it is today's, passed) and is no figure; a dated reset behind us is
    /// no figure; the span and the auto-continue forms the band's old copy of
    /// the grammar could not read count down too; a notice in another zone is
    /// no figure, never a wrong one. Pure through [`countdown_at`] at a fixed
    /// clock, then the live path once.
    #[test]
    fn the_countdown_is_to_the_reset_on_this_clock() {
        // 2026-09-19 10:00:00 UTC, at -0700: 03:00 local on the 19th.
        let now = 1_789_812_000_i64;
        let off = -7 * 3600;
        let la = Some("America/Los_Angeles");
        let at = |text: &str| countdown_at(text, now, off, la);
        assert_eq!(at("5am"), Some(Duration::from_secs(2 * 3600)));
        assert_eq!(at("2am"), None, "an hour ago has fallen");
        assert_eq!(at("3am"), None, "now is no countdown");
        assert_eq!(
            at("Sep 21 at 11am (America/Los_Angeles)"),
            Some(Duration::from_secs(2 * 86_400 + 8 * 3600))
        );
        assert_eq!(at("Sep 18 at 11am"), None, "behind us");
        assert_eq!(at("Sep 19 at 2am"), None, "behind us today");
        assert_eq!(at("in 45m"), Some(Duration::from_secs(45 * 60)));
        assert_eq!(
            at("continuing automatically at 5am"),
            Some(Duration::from_secs(2 * 3600))
        );
        assert_eq!(at("5am (Etc/UTC)"), None, "another zone");
        assert_eq!(at("soon"), None);
        // The live path, once: a bare time two hours from now on this clock.
        if let Some(offset) = local_offset_s() {
            let local = limit::unix_now() + offset + 2 * 3600;
            let tod = local.rem_euclid(86_400);
            let text = format!("{}:{:02}", tod / 3600, (tod % 3600) / 60);
            let ahead = countdown_to_reset(&text).expect("placeable");
            assert!(
                (Duration::from_secs(2 * 3600 - 61)..=Duration::from_secs(2 * 3600))
                    .contains(&ahead),
                "{text}: {ahead:?}"
            );
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

    /// A Bash box in Claude Code's layout: header, the block of command (and
    /// description) rows, the question and options, the composer under it.
    fn bash_box(block: &[&str]) -> Vec<String> {
        let mut rows = vec![" Bash command".to_string(), String::new()];
        rows.extend(block.iter().map(|r| format!("   {r}")));
        rows.extend(
            [
                "",
                " Do you want to proceed?",
                " \u{276f} 1. Yes",
                "   2. No",
                "",
                " Esc to cancel \u{00b7} Tab to amend",
            ]
            .iter()
            .map(|r| (*r).to_string()),
        );
        rows.extend(aterm_phase::prompt::fixtures::composer("  ? for shortcuts"));
        rows
    }

    fn detail_of(rows: &[String]) -> Option<String> {
        match classify(&aterm_phase::ClaudeReader, rows, Cursor::Unknown, t0())
            .0
            .phase
        {
            AgentPhase::Prompt { detail } => detail,
            other => panic!("not a prompt: {other:?}"),
        }
    }

    /// `agent_detail=bash:read-only` holds for EVERY reading of the command
    /// rows ([`aterm_phase::PromptV2::readings`], the readings the
    /// supervisor's decider classifies — review major; SUP-1/APR-5).
    /// NEGATIVE CONTROL for the `│` case: the space-joined string the old
    /// parser returned classifies read-only on its own — the verdict this used
    /// to publish.
    #[test]
    fn a_bash_verdict_is_read_only_only_when_every_reading_is() {
        use aterm_agent::supervise::classify::classify_command;
        let all_read_only = |rows: &[String]| {
            let p = aterm_phase::parse_prompt_v2(rows).expect("a box");
            let readings = p.readings();
            !readings.is_empty() && readings.iter().all(|r| classify_command(r).read_only)
        };
        // One command row and its description: read-only, as ever.
        let one = bash_box(&["git log --oneline -5", "Show the five most recent commits"]);
        assert_eq!(detail_of(&one).as_deref(), Some("bash:read-only"));

        // `│` rows: two statements (Claude Code draws bars for a command with
        // a newline in it), joined with a space by the old parser.
        let bars = bash_box(&["\u{2502} git status", "\u{2502} node scripts/migrate.js"]);
        let p = aterm_phase::parse_prompt(&bars).expect("a box");
        assert_eq!(p.command, "git status node scripts/migrate.js");
        assert!(
            classify_command(&p.command).read_only,
            "NEGATIVE CONTROL: the flattened reading is read-only"
        );
        assert_eq!(detail_of(&bars).as_deref(), Some("bash:not-read-only"));

        // No `│`: one shell line that may wrap, its rows every command row. A
        // write anywhere in them — the row shown as the command or the one
        // the parser guesses is the description — is not read-only.
        for block in [
            &["git status &&", "rm -rf build"][..],
            &["echo hi >", "out.txt"][..],
            &["rm -rf build", "List the files"][..],
        ] {
            let rows = bash_box(block);
            assert!(!all_read_only(&rows), "{block:?}");
            assert_eq!(
                detail_of(&rows).as_deref(),
                Some("bash:not-read-only"),
                "{block:?}"
            );
        }
        // …and a wrapped read stays a read: the detail is the decider's
        // verdict, whatever it is.
        let wrapped = bash_box(&["git status", "node scripts/migrate.js"]);
        let want = if all_read_only(&wrapped) {
            "bash:read-only"
        } else {
            "bash:not-read-only"
        };
        assert_eq!(detail_of(&wrapped).as_deref(), Some(want));
    }

    /// FRAME identification is for a program that can be an agent (review
    /// major): a shell or `cat` showing a captured Claude screen — frame,
    /// question and all — is not an agent, a frame identity does not survive
    /// the program resolving to a shell, and a group still being named waits
    /// for its name. NEGATIVE CONTROL: the same screen under `node`, or with
    /// no foreground group to name, IS read as the question it shows.
    /// A Codex started under `node` (an npm install) names no agent: its
    /// screen does — its composer at idle ([`aterm_phase::identify`]) — and,
    /// as Claude Code's frame does across its boxes, that identity is kept
    /// for the job (`known_agent`): a draft standing in the composer, which
    /// alone identifies nothing, is still read by Codex's reader. NEGATIVE
    /// CONTROL: the draft screen with nothing known is no agent.
    #[test]
    fn a_node_run_codex_keeps_its_reader_across_its_job() {
        use aterm_phase::Program;
        use aterm_phase::codex::fixtures as cx;
        use aterm_phase::prompt::fixtures::screen;
        match agent_verdict(
            Some("node"),
            false,
            None,
            &screen(cx::IDLE),
            Cursor::Unknown,
            t0(),
        ) {
            AgentVerdict::Agent {
                by_frame, program, ..
            } => assert!(by_frame && program == Program::Codex, "{program:?}"),
            AgentVerdict::NotAgent => panic!("Codex's composer identifies it"),
        }
        let draft = screen(cx::DRAFT);
        assert!(
            matches!(
                agent_verdict(Some("node"), false, None, &draft, Cursor::Unknown, t0()),
                AgentVerdict::NotAgent
            ),
            "NEGATIVE CONTROL: a draft alone names nothing"
        );
        match agent_verdict(
            Some("node"),
            false,
            Some(Program::Codex),
            &draft,
            Cursor::Unknown,
            t0(),
        ) {
            AgentVerdict::Agent { program, .. } => assert_eq!(program, Program::Codex),
            AgentVerdict::NotAgent => panic!("the job's identity is kept"),
        }
    }

    #[test]
    fn a_shell_showing_a_captured_claude_frame_is_not_an_agent() {
        let mut rows = vec![
            "\u{23fa} Should I also delete the old logs?".to_string(),
            String::new(),
        ];
        rows.extend(aterm_phase::prompt::fixtures::composer("  ? for shortcuts"));
        assert!(
            aterm_phase::phase::has_composer_frame(&rows),
            "PRECONDITION"
        );
        for program in ["sh", "zsh", "cat", "less"] {
            for known in [None, Some(aterm_phase::Program::Claude)] {
                assert!(
                    matches!(
                        agent_verdict(Some(program), false, known, &rows, Cursor::Unknown, t0()),
                        AgentVerdict::NotAgent
                    ),
                    "{program} (known={known:?}) is not an agent"
                );
            }
        }
        // A group whose name is still being resolved identifies nothing yet
        // (the shell's own group is re-named after every job).
        assert!(matches!(
            agent_verdict(None, true, None, &rows, Cursor::Unknown, t0()),
            AgentVerdict::NotAgent
        ));
        for program in [None, Some("node")] {
            match agent_verdict(program, false, None, &rows, Cursor::Unknown, t0()) {
                AgentVerdict::Agent {
                    reading, by_frame, ..
                } => {
                    assert!(by_frame, "{program:?}");
                    assert_eq!(reading.phase, AgentPhase::Question, "{program:?}");
                }
                AgentVerdict::NotAgent => panic!("{program:?} is identified by its frame"),
            }
        }
    }
}
