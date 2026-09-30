// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PRESENCE — a window shows who is driving it: this host's SENSING half.
//!
//! The row's pure core — the facts, the per-session slot and its story, the
//! levels, the rim, the chip, the words and their width law, the tones, the
//! painted cells and the per-window view — is the engine's
//! (`aterm_messages::presence`, design ruling 348: the owner's "design the
//! logic in aterm core and then keep the osx layer lightweight so that we can
//! make this cross platform"). It is re-exported here under the names every
//! call site has always used; the generic types are pinned to this host's
//! [`Native`] seam ([`Slot`], [`Facts`], [`AgentPhase`], [`AgentReading`]).
//!
//! What stays here is what only a host can do: the wakes that carry a change
//! from the control and bridge threads to the main thread ([`install_proxy`]),
//! the agent verdict — `aterm_phase`'s screen readers over the [`live_zone`]
//! with the terminal's cursor ([`agent_verdict`], run by the status sweep in
//! `session_status.rs`, at most 4 Hz per session and never per frame; the
//! test-only [`classifier_calls`] counter is the gate's proof) — the words
//! for an API error's cause and a wall's wire word, the reset clock that
//! places a limit notice's reset time on this machine's clock
//! ([`countdown_to_reset`]), and [`Native`], the seam the engine reads a
//! wall, an input stall, this process's harness and the wire's percent codec
//! through. The App's side (`app_presence.rs`) gathers each session's facts,
//! hosts the engine's presence driver (`aterm_messages::presence::drive`,
//! ruling 353) and paints the row the engine lays out. The row's pure
//! `Slot`/`words` tests are the engine's (`presence_tests.rs`, over a test
//! seam); the tests here prove this host's sensing and its seam.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_agent::supervise::limit;
use aterm_session::SessionId;
use winit::event_loop::EventLoopProxy;

use crate::Wake;

// The engine's presence core under the names every call site has always used
// (design ruling 348).
pub(crate) use aterm_messages::presence::{
    ChipLevel, Hand, HoldFact, LeaseMark, Link, MailFacts, MailLast, StoryVerb,
    TOLD_TEXT_MAX_BYTES, TurnFact, Words, fmt_dur, prompt_band_word, prompt_kind_words,
    sanitize_token, short_sid,
};
// …and the ones only the tests name.
#[cfg(test)]
pub(crate) use aterm_messages::presence::{
    Level, RIPPLE, Rim, StopCause, TOLD_FLASH, words, words_step,
};

/// The engine's presence slot, pinned to this host's seam.
pub(crate) type Slot = aterm_messages::presence::Slot<Native>;
/// The facts one refresh reads, pinned to this host's seam.
pub(crate) type Facts = aterm_messages::presence::Facts<Native>;
/// The worker's phase as the row spells it, pinned to this host's seam.
pub(crate) type AgentPhase = aterm_messages::presence::AgentPhase<Native>;
/// One screen's agent reading, pinned to this host's seam.
pub(crate) type AgentReading = aterm_messages::presence::AgentReading<Native>;
/// One window's presence view (the engine's; the painted-row cache beside it
/// is the window state's own, `WindowState::presence_row`).
pub(crate) type WindowView = aterm_messages::presence::View;

/// THIS host's presence seam (`aterm_messages::presence::Host`): an
/// uninhabited marker, because the orphan rule forbids implementing the
/// engine's trait on `aterm_phase`'s types. A wall is `aterm_phase`'s
/// [`aterm_phase::WallKind`], a stall the server's published
/// [`crate::input_stall::InputStallFact`], this process's harness is
/// [`crate::harness_host::holder`], and the percent codec is the wire's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Native {}

impl aterm_messages::presence::Host for Native {
    type Wall = aterm_phase::WallKind;
    type Stall = crate::input_stall::InputStallFact;

    fn wall_wire(kind: aterm_phase::WallKind) -> &'static str {
        wall_word(kind)
    }

    /// A wall the row has always called `limited` (a usage window, a model
    /// bucket, spend, an API rate limit — [`aterm_phase::WallKind::reads_limited`])
    /// keeps that word (design §1: `limited → 19:30 · 1d 22h`), an API error
    /// the network caused says what went wrong ([`api_cause_words`]), and any
    /// other wall is the state it leaves the session in (the menu bar's
    /// `status_item::wall_words`).
    fn wall_band(kind: aterm_phase::WallKind) -> &'static str {
        if kind.reads_limited() {
            return "limited";
        }
        api_cause_words(kind).unwrap_or_else(|| crate::status_item::wall_words(kind.name()))
    }

    /// An API error or an overload the harness retries on its own clock
    /// (not a rate limit, which names its own reset).
    fn wall_retries(kind: aterm_phase::WallKind) -> bool {
        !kind.reads_limited()
            && matches!(
                kind,
                aterm_phase::WallKind::ApiError { .. } | aterm_phase::WallKind::Overloaded
            )
    }

    fn stall_since(stall: &crate::input_stall::InputStallFact) -> aterm_messages::Instant {
        stall.since
    }

    fn stall_stopped(stall: &crate::input_stall::InputStallFact) -> bool {
        stall.stopped
    }

    fn stall_survived(stall: &crate::input_stall::InputStallFact) -> bool {
        stall.restart.survived
    }

    fn is_aterms_holder(holder: &str) -> bool {
        holder == crate::harness_host::holder()
    }

    fn pct_encode(s: &str) -> String {
        aterm_control::wire::pct_encode(s)
    }

    fn pct_decode(s: &str) -> String {
        aterm_control::wire::pct_decode(s)
    }
}

/// A hold's reason as every human-facing surface prints and speaks it — the
/// engine's `hold_reason_words` over the wire's decode: the band's hand slot,
/// its spoken sentence and the greyed menu row (`crate::menu::hold_row_reason`)
/// all read from here, so they cannot disagree.
pub(crate) fn hold_reason_words(reason: &str) -> String {
    aterm_messages::presence::hold_reason_words::<Native>(reason)
}

/// When a control client was last handed a session's screen GENERATION by a
/// read an `if-gen=` fence names (`status gen=`, `text --json`):
/// [`crate::metrics::now_us`] plus one (so `0` stays "never"). Lives on
/// [`crate::SessionCtx`]; the App's `Desk::looked_at` reads it, and a pending
/// fold of the presence row waits its quiet past it (ruling 394, bounded by
/// `FOLD_LOOK_CAP`). Why (2026-09-28 review of ruling 394): with the fold
/// deferred, its re-grid could land between a driver's read and its fenced
/// act — `meta` unset, a read, the fold's timer, the act refused
/// `reason=changed` — where the immediate fold had landed before the read.
/// Posts no wake: the loop recomputes the presence deadline every pass, and
/// a tick that finds the fold not yet due re-arms at the later instant.
/// Lock-free, one relaxed `fetch_max` per read.
#[derive(Debug, Default)]
pub(crate) struct GenerationLook(std::sync::atomic::AtomicU64);

impl GenerationLook {
    /// A generation of the session's screen was handed out at `now_us`.
    pub(crate) fn note(&self, now_us: u64) {
        self.0.fetch_max(
            now_us.saturating_add(1),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// When one last was ([`crate::metrics::now_us`]); `None`: never.
    pub(crate) fn last_us(&self) -> Option<u64> {
        self.0
            .load(std::sync::atomic::Ordering::Relaxed)
            .checked_sub(1)
    }
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
    #[cfg(test)]
    LEASE_WAKES.with(|w| w.borrow_mut().push((session.clone(), submitted)));
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::LeaseChanged {
            session: session.clone(),
            submitted,
        });
    }
}

#[cfg(test)]
thread_local! {
    static LEASE_WAKES: std::cell::RefCell<Vec<(SessionId, bool)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Every lease-changed wake posted ON THIS THREAD since the last take, in
/// order: `(session, submitted)`. A headless test App installs no proxy, so
/// the wake itself goes nowhere; this is what a control verb run on the test's
/// own thread asked the event loop to deliver. Thread-local for the same
/// reason as [`classifier_calls`]: the suite runs tests in parallel.
#[cfg(test)]
pub(crate) fn take_lease_wakes() -> Vec<(SessionId, bool)> {
    LEASE_WAKES.with(|w| std::mem::take(&mut *w.borrow_mut()))
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

/// THE LOOP'S NEXT TRY laid on an API wall's reading (the tab retry plan,
/// 2026-09-29): a wall the harness retries ([`Native::wall_retries`]) that
/// names no time of its own gets the loop's deadline — `until` for the
/// countdown and, where the zone is known, the local `HH:MM` as its `reset`
/// — so the row reads `can't reach the API → 14:05 · 3m`. Only a plan still
/// AHEAD counts (a past one is a try already made), only a wall with neither
/// `reset` nor `until` (a usage limit keeps its own), and nothing else about
/// the reading moves: the wall's kind, its band word and `status agent=` are
/// the screen's. `zone` is asked only when a plan is laid on.
pub(crate) fn with_retry_plan(
    agent: Option<AgentReading>,
    at_unix: Option<i64>,
    now_unix: i64,
    now: Instant,
    zone: impl FnOnce() -> Option<i64>,
) -> Option<AgentReading> {
    let Some(at) = at_unix else {
        return agent;
    };
    let mut agent = agent?;
    let ahead = at - now_unix;
    let AgentPhase::Wall {
        kind,
        reset: None,
        until: until @ None,
    } = &mut agent.phase
    else {
        return Some(agent);
    };
    if ahead <= 0 || !<Native as aterm_messages::presence::Host>::wall_retries(*kind) {
        return Some(agent);
    }
    *until = now.checked_add(Duration::from_secs(ahead.unsigned_abs()));
    let clock = zone().map(|offset| {
        let local = (at + offset).rem_euclid(86_400);
        format!("{:02}:{:02}", local / 3600, (local % 3600) / 60)
    });
    if let AgentPhase::Wall { reset, .. } = &mut agent.phase {
        *reset = clock;
    }
    Some(agent)
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

/// The local zone's offset from UTC AT the instant `unix`, in seconds: the offset
/// daylight saving gave that instant, not today's — [`limit::offset_at`] with no zone
/// named, the workspace's one reader of an offset at an instant. A subprocess each
/// call, so a WORKER's only (Settings ▸ Packages' clock,
/// `packages_screen::LocalClock::read`); `None` where `date` cannot say.
pub(crate) fn local_offset_at(unix: i64) -> Option<i64> {
    limit::offset_at(None, unix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
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
        // The VENDOR's banner, word for word (its own remedy tail is Claude
        // Code's text on the screen, never a remedy aterm prints).
        let banner = format!(
            "{} (140.4GB) \u{2014} restart and resume with claude --continue",
            aterm_phase::anchor_text("wall.memory"),
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

    /// THE TAB RETRY PLAN (2026-09-29): the loop's next try is laid on an API
    /// wall's reading — `until` for the countdown and, the zone known, the
    /// local `HH:MM` as its `reset` — and the row PRINTS `→ 08:58 · 3m00s` and
    /// SPEAKS `next try 08:58`, never `resets`. CONTROLS, each leaving the
    /// reading exactly as it was: a plan in the past or now (a try already
    /// made), none, a wall that names its own time (a usage limit, a reset
    /// already read), a phase that is no wall, and no reading at all. An
    /// overload takes it too; an unknown zone gives the countdown alone; and
    /// the zone is asked only when a plan is laid on.
    #[test]
    fn a_retry_plan_is_laid_on_an_api_wall_and_nothing_else() {
        use aterm_phase::{ApiCause, WallKind};
        const NOW_UNIX: i64 = 1_789_660_500; // 2026-09-17T15:55:00Z, 08:55 in PDT
        const PDT: i64 = -7 * 3600;
        let now = t0();
        let wall = |kind: WallKind, reset: Option<&str>| {
            Some(AgentReading {
                phase: AgentPhase::Wall {
                    kind,
                    reset: reset.map(str::to_string),
                    until: None,
                },
                context_pct: Some(40),
            })
        };
        let unreachable = WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable,
        };
        let lay = |agent: Option<AgentReading>, at: Option<i64>, zone: Option<i64>| {
            with_retry_plan(agent, at, NOW_UNIX, now, || zone)
        };
        // Laid on: 3 minutes ahead, the local clock at the try.
        let got = lay(wall(unreachable, None), Some(NOW_UNIX + 180), Some(PDT)).unwrap();
        assert_eq!(got.context_pct, Some(40), "nothing else moves");
        let AgentPhase::Wall { kind, reset, until } = got.phase else {
            panic!("still a wall");
        };
        assert_eq!(kind, unreachable, "the wall's kind is the screen's");
        assert_eq!(reset.as_deref(), Some("08:58"));
        assert_eq!(until, now.checked_add(Duration::from_secs(180)));
        // An overload takes it; an unknown zone gives the countdown alone.
        let got = lay(wall(WallKind::Overloaded, None), Some(NOW_UNIX + 60), None).unwrap();
        let AgentPhase::Wall { reset, until, .. } = got.phase else {
            panic!("still a wall");
        };
        assert_eq!((reset, until.is_some()), (None, true));
        // CONTROLS: the reading is returned as it was.
        for (name, agent, at) in [
            ("past", wall(unreachable, None), Some(NOW_UNIX - 5)),
            ("now", wall(unreachable, None), Some(NOW_UNIX)),
            ("no plan", wall(unreachable, None), None),
            (
                "its own reset",
                wall(unreachable, Some("7:30pm")),
                Some(NOW_UNIX + 60),
            ),
            (
                "a usage limit",
                wall(WallKind::UsageSession, None),
                Some(NOW_UNIX + 60),
            ),
            (
                "not a wall",
                Some(AgentReading {
                    phase: AgentPhase::Idle,
                    context_pct: None,
                }),
                Some(NOW_UNIX + 60),
            ),
        ] {
            assert_eq!(lay(agent.clone(), at, Some(PDT)), agent, "{name}");
        }
        assert_eq!(lay(None, Some(NOW_UNIX + 60), Some(PDT)), None);
        // The zone is asked only when a plan is laid on.
        let asked = std::cell::Cell::new(0);
        let count = |zone| {
            asked.set(asked.get() + 1);
            zone
        };
        let _ = with_retry_plan(
            wall(unreachable, None),
            Some(NOW_UNIX - 1),
            NOW_UNIX,
            now,
            || count(Some(PDT)),
        );
        assert_eq!(asked.get(), 0, "a past plan asks nothing");
        let _ = with_retry_plan(
            wall(unreachable, None),
            Some(NOW_UNIX + 9),
            NOW_UNIX,
            now,
            || count(Some(PDT)),
        );
        assert_eq!(asked.get(), 1);

        // What the row prints and speaks (the shared words engine).
        let mut slot = Slot::new(now);
        slot.agent = lay(wall(unreachable, None), Some(NOW_UNIX + 180), Some(PDT));
        let said = words(&slot, now, 0);
        assert_eq!(said.phase, "can't reach the API");
        assert_eq!(
            said.since,
            vec!["\u{2192} 08:58".to_string(), "3m00s".to_string()],
            "{said:?}"
        );
        assert!(
            said.sentence.contains("next try 08:58"),
            "{}",
            said.sentence
        );
        assert!(!said.sentence.contains("resets"), "{}", said.sentence);
        // CONTROL: a usage limit still speaks `resets`.
        slot.agent = wall(WallKind::UsageSession, Some("7:30pm"));
        let said = words(&slot, now, 0);
        assert!(said.sentence.contains("resets 7:30pm"), "{}", said.sentence);
    }

    /// A TALL PANE, A SHORT TRANSCRIPT (a real render of 2026-09-28: Claude
    /// Code 2.1.284, 144x50, an API error on row 8 and the composer pinned on
    /// rows 45-47): the status sweep's verdict was `idle` with no wall, the
    /// error 41 rows above the last drawn row and cut by the zone. The
    /// verdict reads the wall and its cause, with the cursor on the caret as
    /// the sweep hands it.
    #[test]
    fn an_api_error_far_above_the_composer_is_a_wall_in_the_verdict() {
        use aterm_phase::prompt::fixtures::{API_ERROR_TALL_PANE_MEASURED, screen};
        let rows = screen(API_ERROR_TALL_PANE_MEASURED);
        let caret = Cursor::At(rows.iter().rposition(|r| r.trim() == "❯"));
        for program in [Some("claude"), None] {
            let v = agent_verdict(program, false, None, &rows, caret, t0());
            assert_eq!(v.word(), "wall:api-error", "{program:?}");
            assert_eq!(v.subject(), Some(API_UNREACHABLE), "{program:?}");
        }
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
        // 40 rows of that content and the blank foot under it. A screen
        // drawn to its last row: its last 40 rows, as it always was; an
        // all-blank one reads whole. A long blank GAP counts as one row
        // (`aterm_phase::live_zone_start`), so the dialog above a blank
        // middle stays in the zone of a screen with content on its last row.
        assert_eq!(live_zone(&live), &live[41 - CLASSIFY_ROWS..]);
        let drawn: Vec<String> = (0..62).map(|i| format!("row {i}")).collect();
        assert_eq!(live_zone(&drawn), &drawn[62 - CLASSIFY_ROWS..]);
        let mut full = trust.clone();
        full[61] = "x".to_string();
        assert!(live_zone(&full).len() > CLASSIFY_ROWS);
        assert!(
            live_zone(&full)
                .iter()
                .any(|r| r.contains("Accessing workspace"))
        );
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

    /// THIS host's stall seam: `Native`'s three accessors over the server's
    /// real [`crate::input_stall::InputStallFact`]. The engine's own stall
    /// tests run on its `TestStall`, so only this one proves that `since`
    /// reads the fact's instant, `stopped` its `stopped` field (never the
    /// word) and `survived` its restart record. Each case differs from the
    /// others in exactly the field its accessor reads, so a crossed or
    /// constant accessor turns one of them red.
    #[test]
    fn the_native_stall_seam_reads_since_stopped_and_survived_off_the_fact() {
        use crate::input_stall::{InputStallFact, Restart};
        use aterm_session::input_backlog::InputWord;
        let now = t0();
        let since = now - Duration::from_secs(123);
        let frozen = InputStallFact {
            word: InputWord::Stalled,
            since,
            bytes: 1,
            stopped: false,
            rss_mb: None,
            restart: Restart::default(),
        };
        let read = |fact: InputStallFact| {
            let mut s = Slot::new(now);
            assert!(s.absorb(
                Facts {
                    input_stall: Some(fact),
                    ..Facts::default()
                },
                now
            ));
            (s.level(0), s.chip(0), words(&s, now, 0))
        };
        // Frozen: the duration counts from the fact's `since`.
        let (level, chip, w) = read(frozen.clone());
        assert_eq!(level, Level::Limited);
        assert_eq!(chip, ChipLevel::Stop(StopCause::Frozen));
        assert_eq!(
            (w.phase.as_str(), w.since.as_slice()),
            (
                "frozen",
                &["2m03s".to_string(), "not reading input".to_string()][..]
            )
        );
        assert!(
            w.sentence
                .contains("not reading input for 2m03s; restart it"),
            "{}",
            w.sentence
        );
        // Stopped is the `stopped` FIELD: the word left at `Stalled`.
        let (level, chip, w) = read(InputStallFact {
            stopped: true,
            ..frozen.clone()
        });
        assert_eq!(level, Level::Limited);
        assert_eq!(chip, ChipLevel::Stop(StopCause::Suspended));
        assert_eq!(w.phase, "stopped");
        assert!(
            w.sentence
                .contains("stopped with input queued for 2m03s; resume it"),
            "{}",
            w.sentence
        );
        // Survived is the restart record's.
        let (_, chip, w) = read(InputStallFact {
            restart: Restart {
                survived: true,
                ..Restart::default()
            },
            ..frozen
        });
        assert_eq!(chip, ChipLevel::Stop(StopCause::Frozen));
        assert_eq!(
            w.since,
            vec!["2m03s".to_string(), "survived its restart".to_string()]
        );
        assert!(
            w.sentence.contains("end it with signal kill"),
            "{}",
            w.sentence
        );
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
