// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence: this host's sensing half.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_agent::supervise::limit;
use aterm_session::SessionId;
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;

#[cfg(test)]
pub(crate) use aterm_messages::presence::Level;
pub(crate) use aterm_messages::presence::{
    ChipLevel, Hand, HoldFact, Link, NeedFact, NeedSeverity, StopCause, StoryVerb,
    TOLD_TEXT_MAX_BYTES, TurnFact, WallAnswer, fmt_dur, prompt_band_word, prompt_kind_words,
    short_sid, state_line,
};

/// The engine's presence slot, pinned to this host's seam.
pub(crate) type Slot = aterm_messages::presence::Slot<Native>;
/// The facts one refresh reads, pinned to this host's seam.
pub(crate) type Facts = aterm_messages::presence::Facts<Native>;
/// The worker's phase as the row spells it, pinned to this host's seam.
pub(crate) type AgentPhase = aterm_messages::presence::AgentPhase<Native>;
/// One screen's agent reading, pinned to this host's seam.
pub(crate) type AgentReading = aterm_messages::presence::AgentReading<Native>;
/// One window's presence view.
pub(crate) type WindowView = aterm_messages::presence::View;

/// The published need ([`crate::needs::Need`], the session's one verdict) in the engine's terms.
pub(crate) fn need_fact(need: &crate::needs::Need) -> NeedFact {
    use aterm_types::needs::{NeedWord, Severity};
    NeedFact {
        severity: match need.word.severity() {
            Severity::Stuck => NeedSeverity::Stuck,
            Severity::Decide => NeedSeverity::Decide,
        },
        cause: match need.word {
            NeedWord::Frozen => StopCause::Frozen,
            NeedWord::Stopped => StopCause::Suspended,
            _ => StopCause::Wall,
        },
        key: (need.word as u8, need.key),
        what: need.what.clone(),
    }
}

/// This host's presence seam (`aterm_messages::presence::Host`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Native {}

/// Ruling 313's judgement of a drive lease as aterm's own hand.
#[must_use]
pub(crate) fn attested_aterms_holder(holder: &str, pid: Option<u32>, own: &str, me: u32) -> bool {
    holder == own && pid == Some(me)
}

impl Native {
    /// Whether the harness retries this wall on its own clock (an API error, an overload, the API's own 429).
    pub(crate) const fn wall_retries(kind: aterm_phase::WallKind) -> bool {
        matches!(
            kind,
            aterm_phase::WallKind::ApiError { .. } | aterm_phase::WallKind::Overloaded
        )
    }
}

impl aterm_messages::presence::Host for Native {
    type Wall = aterm_phase::WallKind;
    type Stall = crate::input_stall::InputStallFact;

    fn wall_wire(kind: aterm_phase::WallKind) -> &'static str {
        kind.wire()
    }

    /// A wall that reads `limited` ([`aterm_phase::WallKind::reads_limited`]) keeps that word;
    /// an API error the network caused says what went wrong ([`api_cause_words`]); any other
    /// wall is the state it leaves the session in (`needs::wall_words`).
    fn wall_band(kind: aterm_phase::WallKind) -> &'static str {
        if kind.reads_limited() {
            return "limited";
        }
        api_cause_words(kind).unwrap_or_else(|| crate::needs::wall_words(kind.name()))
    }

    fn is_aterms_holder(holder: &str, pid: Option<u32>) -> bool {
        attested_aterms_holder(
            holder,
            pid,
            &crate::harness_host::holder(),
            std::process::id(),
        )
    }

    fn pct_encode(s: &str) -> String {
        aterm_control::wire::pct_encode(s)
    }

    fn pct_decode(s: &str) -> String {
        aterm_control::wire::pct_decode(s)
    }

    fn columns(s: &str) -> usize {
        aterm_grapheme::str_grid_columns(s)
    }
}

/// A free-text value as presence prints it.
pub(crate) fn sanitize_token(s: &str, cap: usize) -> String {
    aterm_messages::presence::sanitize_token::<Native>(s, cap)
}

/// A hold's reason as every human-facing surface prints and speaks it.
pub(crate) fn hold_reason_words(reason: &str) -> String {
    aterm_messages::presence::hold_reason_words::<Native>(reason)
}

static PROXY: OnceLock<EventLoopProxy<Wake>> = OnceLock::new();

/// Install the proxy the wakes ride.
pub(crate) fn install_proxy(proxy: EventLoopProxy<Wake>) {
    let _ = PROXY.set(proxy);
}

/// A session's drive lease changed hands.
pub(crate) fn post_lease_changed(session: &SessionId) {
    #[cfg(test)]
    LEASE_WAKES.with(|w| w.borrow_mut().push(session.clone()));
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::LeaseChanged {
            session: session.clone(),
        });
    }
}

#[cfg(test)]
thread_local! {
    static LEASE_WAKES: std::cell::RefCell<Vec<SessionId>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Every lease-changed wake posted on this thread since the last take, in order.
#[cfg(test)]
pub(crate) fn take_lease_wakes() -> Vec<SessionId> {
    LEASE_WAKES.with(|w| std::mem::take(&mut *w.borrow_mut()))
}

/// A session's fabric endpoint changed.
pub(crate) fn post_fabric_changed(session: &SessionId) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::FabricChanged {
            session: session.clone(),
        });
    }
}

/// A control client typed into `session` and its hover's words moved ([`crate::typed_by::stamp`]).
pub(crate) fn post_typed(session: &SessionId) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::Typed {
            session: session.clone(),
        });
    }
}

/// Another Mac's node acted on session `session` over the fleet's drive op.
#[cfg(any(unix, test))]
pub(crate) fn post_driven(session: &SessionId, host: &str, act: &str) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::Driven {
            session: session.clone(),
            host: host.to_owned(),
            act: act.to_owned(),
        });
    }
}

/// Session `local`'s metadata moved off a control connection.
pub(crate) fn post_meta_changed(local: u64) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::MetaChanged { session: local });
    }
}

/// Session `local`'s TASK RECORD moved.
pub(crate) fn post_task_changed(local: u64) {
    #[cfg(test)]
    TASK_WAKES.with(|w| w.borrow_mut().push(local));
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::TaskChanged { session: local });
    }
}

#[cfg(test)]
thread_local! {
    static TASK_WAKES: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Every task-changed wake posted on this thread since the last take, in order.
#[cfg(test)]
pub(crate) fn take_task_wakes() -> Vec<u64> {
    TASK_WAKES.with(|w| std::mem::take(&mut *w.borrow_mut()))
}

/// How many screen rows the agent-phase classifier reads.
pub(crate) const CLASSIFY_ROWS: usize = 40;

/// The live zone the agent verdict reads and the status sweep's classifier gate hashes.
pub(crate) fn live_zone(rows: &[String]) -> &[String] {
    &rows[aterm_phase::live_zone_start(rows, CLASSIFY_ROWS)..]
}

/// The terminal's cursor as the verdict reads a screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// Its row among the rows the verdict is handed (`None`: on none of them).
    At(Option<usize>),
    /// Not given: a test's bare screen ([`aterm_phase::ScreenReader::read`]).
    #[cfg(test)]
    Unknown,
}

impl Cursor {
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

/// How many times the agent-phase classifier has run on this thread.
#[cfg(test)]
pub(crate) fn classifier_calls() -> u64 {
    CLASSIFIER_CALLS.with(|c| c.get())
}
/// The subject of a wall read off a sign-in screen the program draws instead of its composer
/// ([`aterm_phase::Placement::SignIn`]): the one login wall whose sign-in already runs on the
/// screen, which the machine's one Grok Build sign-in keys on (`app_presence`'s `grok_walled`).
/// Any other login wall (a 401's failure row, a login-ended text) carries none.
pub(crate) const SIGN_IN_SCREEN: &str = "sign-in screen";

/// Tab and menu words for an unreachable API error ([`aterm_phase::ApiCause::Unreachable`]).
pub(crate) const API_UNREACHABLE: &str = "can't reach the API";
/// … a reply the connection cut off ([`aterm_phase::ApiCause::CutOff`]).
pub(crate) const API_CUT_OFF: &str = "reply cut off";
/// … a certificate or proxy the connection refused ([`aterm_phase::ApiCause::Config`]).
pub(crate) const API_REFUSED: &str = "TLS/proxy refused";
/// … a request the API itself refused ([`aterm_phase::ApiCause::Request`]).
pub(crate) const API_REQUEST_REFUSED: &str = "request refused";
/// … a reply past its output token maximum ([`aterm_phase::ApiCause::Overlong`]).
pub(crate) const API_OVERLONG: &str = "reply too long";

/// What an API error the network caused is called on the tab and in the menu.
pub(crate) const fn api_cause_words(kind: aterm_phase::WallKind) -> Option<&'static str> {
    match kind {
        aterm_phase::WallKind::ApiError { cause, .. } => match cause {
            aterm_phase::ApiCause::Unreachable => Some(API_UNREACHABLE),
            aterm_phase::ApiCause::CutOff => Some(API_CUT_OFF),
            aterm_phase::ApiCause::Config => Some(API_REFUSED),
            aterm_phase::ApiCause::Request => Some(API_REQUEST_REFUSED),
            aterm_phase::ApiCause::Overlong => Some(API_OVERLONG),
            aterm_phase::ApiCause::Server => None,
        },
        _ => None,
    }
}

/// The loop's next try for an API wall's reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetryPlan {
    /// The try's unix second: the countdown is off it.
    pub(crate) at_unix: i64,
    /// The local `HH:MM` of the try; `None` where the zone cannot be read — then the countdown is printed alone.
    pub(crate) clock: Option<String>,
}

impl RetryPlan {
    /// The plan for a try at `at_unix`, its clock read at that instant.
    pub(crate) fn at(at_unix: i64) -> Self {
        Self::with_offset(at_unix, local_offset_at(at_unix))
    }

    /// The plan for a try at `at_unix` in a zone `offset` seconds from UTC.
    pub(crate) fn with_offset(at_unix: i64, offset: Option<i64>) -> Self {
        let clock = offset.map(|offset| {
            let local = (at_unix.rem_euclid(86_400) + offset.rem_euclid(86_400)).rem_euclid(86_400);
            format!("{:02}:{:02}", local / 3600, (local % 3600) / 60)
        });
        Self { at_unix, clock }
    }
}

/// Lay the loop's next try on an API wall's reading.
pub(crate) fn with_retry_plan(
    agent: Option<AgentReading>,
    plan: Option<&RetryPlan>,
    now_unix: i64,
    now: Instant,
) -> Option<AgentReading> {
    let Some(plan) = plan else {
        return agent;
    };
    let mut agent = agent?;
    let AgentPhase::Wall {
        kind,
        reset: None,
        until: until @ None,
    } = &mut agent.phase
    else {
        return Some(agent);
    };
    if plan.at_unix <= now_unix || !Native::wall_retries(*kind) {
        return Some(agent);
    }
    *until = now.checked_add(Duration::from_secs(plan.at_unix.abs_diff(now_unix)));
    if let AgentPhase::Wall { reset, .. } = &mut agent.phase {
        reset.clone_from(&plan.clock);
    }
    Some(agent)
}

/// The server's agent verdict on one screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentVerdict {
    /// The foreground program is not an identified agent: `agent=-`, no band phase.
    NotAgent,
    /// An identified agent's reading.
    Agent {
        reading: AgentReading,
        by_frame: bool,
        subject: Option<String>,
        /// Which program's reader read it — by name or by its screen.
        program: aterm_phase::Program,
        /// What the screen says of the turn, for the session log.
        turn: TurnFacts,
    },
}

/// What one reading says of the agent's turn for the session log (phase 3B, addendum A22).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TurnFacts {
    /// The context left, in percent, when the screen shows it.
    pub(crate) ctx_pct: Option<u8>,
    /// The goal's state (`None`: not read).
    pub(crate) goal: Option<bool>,
    /// A person stopped the last turn (Esc).
    pub(crate) interrupted: bool,
    /// What each choosing key would choose on the box or question the reading publishes (fix E8,
    /// [`crate::session_log::FrameKeys::of_prompt`]); `None` when it publishes none.
    pub(crate) keys: Option<crate::session_log::FrameKeys>,
}

impl TurnFacts {
    /// What `r`, read by `program`'s reader, says of the turn.
    pub(crate) fn of(program: aterm_phase::Program, r: &aterm_phase::Reading) -> TurnFacts {
        match program {
            aterm_phase::Program::Claude => TurnFacts {
                ctx_pct: r.context_left,
                goal: Some(r.goal_active),
                interrupted: r.interrupted,
                keys: None,
            },
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Kimi
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Generic => TurnFacts::default(),
        }
    }
}

impl AgentVerdict {
    /// The `agent=` wire word: the phase word, or `-` for [`Self::NotAgent`].
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Self::NotAgent => "-",
            Self::Agent { reading, .. } => reading.phase.word(),
        }
    }

    /// The `agent_detail=` value (raw; the wire pct-encodes it).
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
    pub(crate) const fn program(&self) -> Option<aterm_phase::Program> {
        match self {
            Self::Agent { program, .. } => Some(*program),
            Self::NotAgent => None,
        }
    }

    /// The reading, when there is one.
    pub(crate) const fn reading(&self) -> Option<&AgentReading> {
        match self {
            Self::Agent { reading, .. } => Some(reading),
            Self::NotAgent => None,
        }
    }

    /// What the screen says of the turn ([`TurnFacts`]).
    pub(crate) fn turn(&self) -> TurnFacts {
        match self {
            Self::Agent { turn, .. } => *turn,
            Self::NotAgent => TurnFacts::default(),
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

/// The one adapter between a session's screen and its published agent verdict. `title` is the
/// program's window title, read under the lock the rows were read under (`""` where it set
/// none): a turn its screen does not show but its title runs reads busy
/// ([`aterm_phase::ScreenReader::read_seen`]).
pub(crate) fn agent_verdict(
    program: Option<&str>,
    program_pending: bool,
    known_agent: Option<aterm_phase::Program>,
    rows: &[String],
    cursor: Cursor,
    title: &str,
    now: Instant,
) -> AgentVerdict {
    use aterm_phase::ScreenReader;
    let whole = rows;
    let rows = live_zone(whole);
    let by_program = program.is_some_and(|p| aterm_phase::program_of(p).is_some());
    let frame_may_identify = program.map_or(!program_pending, aterm_phase::may_host_agent);
    let known_agent = known_agent.filter(|_| frame_may_identify);
    let (reader, by_frame): (&dyn ScreenReader, bool) = if by_program {
        (aterm_phase::identify(program, rows), false)
    } else if let Some(known) = known_agent {
        (known.reader(), false)
    } else if frame_may_identify {
        let reader = match aterm_phase::identify(None, rows) {
            r if !names_agent(r.program()) && whole.len() > rows.len() => {
                aterm_phase::identify(None, whole)
            }
            r => r,
        };
        if !names_agent(reader.program()) {
            return AgentVerdict::NotAgent;
        }
        (reader, true)
    } else {
        return AgentVerdict::NotAgent;
    };
    let (rows, cursor) = verdict_rows(reader, whole, cursor);
    let (reading, subject, turn) = classify(reader, rows, cursor, title, now);
    AgentVerdict::Agent {
        reading,
        by_frame,
        subject,
        program: reader.program(),
        turn,
    }
}

/// The rows a verdict classifies: the live zone ([`live_zone`]), or the whole screen when the
/// zone cuts the head off a box (a prompt read `head_off_screen`) that a taller screen may hold
/// whole.
fn prompt_rows<'a>(reader: &dyn aterm_phase::ScreenReader, whole: &'a [String]) -> &'a [String] {
    let rows = live_zone(whole);
    if whole.len() > rows.len() && reader.prompt(rows).is_some_and(|p| p.head_off_screen) {
        whole
    } else {
        rows
    }
}

/// [`prompt_rows`] and the cursor on them.
fn verdict_rows<'a>(
    reader: &dyn aterm_phase::ScreenReader,
    whole: &'a [String],
    cursor: Cursor,
) -> (&'a [String], Cursor) {
    let rows = prompt_rows(reader, whole);
    (rows, cursor.on_rows_from(whole.len() - rows.len()))
}

/// What the box `program`'s reader reads on `whole` draws, off the rows a verdict classifies
/// ([`prompt_rows`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Drawn {
    /// The read-only rule's readings of it: what [`prompt_detail`] judged.
    pub(crate) readings: Vec<String>,
    /// Its own rows, from its upper rule to its lower one
    /// ([`aterm_phase::prompt::PromptV2::span`]), hashed: a footer below it or a transcript above
    /// it repaints without moving this.
    pub(crate) key: u64,
}

/// [`Drawn`] of the box `program`'s reader reads on `whole`.
pub(crate) fn prompt_drawn(program: aterm_phase::Program, whole: &[String]) -> Option<Drawn> {
    let reader = program.reader();
    let rows = prompt_rows(reader, whole);
    let prompt = reader.prompt(rows)?;
    let mut key = aterm_hash::Fnv1a64::new();
    for row in rows.get(prompt.span.0..=prompt.span.1).unwrap_or(rows) {
        key.write(row.as_bytes());
        key.write(b"\n");
    }
    Some(Drawn {
        readings: prompt.readings(),
        key: key.finish(),
    })
}

/// The title's run bit the agent watch keeps: whether any agent's reader reads `title` as its
/// run going on ([`aterm_phase::ScreenReader::runs`]). Claude Code's two busy frames and Muse
/// Code's spinner frames flip under it, so a flip moves nothing; a turn starting or ending does.
pub(crate) fn title_runs(title: &str) -> bool {
    aterm_phase::Program::ALL
        .iter()
        .any(|p| p.reader().runs(Some(title)) == Some(true))
}

/// Whether the reader a screen sniff answered ([`aterm_phase::identify`]) names an agent.
const fn names_agent(program: aterm_phase::Program) -> bool {
    match program {
        aterm_phase::Program::Claude
        | aterm_phase::Program::Codex
        | aterm_phase::Program::Grok
        | aterm_phase::Program::Kimi
        | aterm_phase::Program::Agy
        | aterm_phase::Program::Muse => true,
        aterm_phase::Program::Generic => false,
    }
}

/// [`agent_verdict`] behind the screen reader's panic fence ([`crate::reader_guard`]).
#[allow(
    clippy::too_many_arguments,
    reason = "agent_verdict's seven inputs, passed through whole, and the session the fence's \
              warning names; a wrapper struct would relocate the list, not simplify it"
)]
pub(crate) fn agent_verdict_guarded(
    session: u64,
    program: Option<&str>,
    program_pending: bool,
    known_agent: Option<aterm_phase::Program>,
    rows: &[String],
    cursor: Cursor,
    title: &str,
    now: Instant,
) -> AgentVerdict {
    fenced_verdict(session, program, program_pending, known_agent, rows, || {
        agent_verdict(
            program,
            program_pending,
            known_agent,
            rows,
            cursor,
            title,
            now,
        )
    })
}

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

/// Verdict for an unreadable screen: an agent identified without the screen is that agent at
/// [`AgentPhase::Unknown`]; anything else is [`AgentVerdict::NotAgent`].
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
            composer: aterm_messages::presence::ComposerState::Unknown,
        },
        by_frame: false,
        subject: None,
        program,
        turn: TurnFacts::default(),
    }
}

/// Classify one screen with one program's reader, its phase read beside the program's window
/// `title` ([`aterm_phase::ScreenReader::read_seen`]; `""` says nothing).
pub(crate) fn classify(
    reader: &dyn aterm_phase::ScreenReader,
    rows: &[String],
    cursor: Cursor,
    title: &str,
    now: Instant,
) -> (AgentReading, Option<String>, TurnFacts) {
    #[cfg(test)]
    CLASSIFIER_CALLS.with(|c| c.set(c.get() + 1));
    let runs = reader.runs(Some(title));
    let r = match cursor {
        Cursor::At(row) => reader.read_seen(rows, row, None, runs),
        #[cfg(test)]
        Cursor::Unknown => reader.read_titled(rows, None, runs),
    };
    let turn = TurnFacts::of(reader.program(), &r);
    let composer = match &r.composer {
        None => aterm_messages::presence::ComposerState::Unknown,
        Some((_, lines)) if lines.iter().all(|line| line.trim().is_empty()) => {
            aterm_messages::presence::ComposerState::Empty
        }
        Some(_) if reader.placeholder() => aterm_messages::presence::ComposerState::Unknown,
        Some(_) => aterm_messages::presence::ComposerState::Draft,
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
    // A turn that ended while the agent's own background work runs on is a turn end: published
    // as the phase it rests at, so the session log closes the turn there and `agent=` says the
    // composer takes the next message. The reader's phase stays busy for the supervisor's breaks.
    // The rest stands only as the reader's own reading of that phase would: authoritative, and
    // an idle one only where the cursor is in the composer (an agents panel holding it, or no
    // cursor read, keeps the busy the reader read). A title that runs a turn the screen does not
    // show rests nothing: the title runs for a turn and not for a shell or a monitor
    // ([`aterm_phase::ScreenReader::background_rest_seen`]).
    let read = match r.phase {
        aterm_phase::Phase::Busy => reader
            .background_rest_seen(rows, runs)
            .filter(|rest| {
                let rested = aterm_phase::Reading {
                    phase: rest.clone(),
                    ..r.clone()
                };
                reader.phase_authoritative(rows, rest)
                    && match cursor {
                        Cursor::At(row) => reader.cursor_vouches(rows, &rested, row),
                        #[cfg(test)]
                        Cursor::Unknown => true,
                    }
            })
            .unwrap_or(aterm_phase::Phase::Busy),
        p => p,
    };
    let phase = if !r.phase_authoritative {
        AgentPhase::Unknown
    } else if let Some(w) = r.wall {
        subject = api_cause_words(w.kind)
            .or((w.placement == aterm_phase::Placement::SignIn).then_some(SIGN_IN_SCREEN))
            .map(str::to_string);
        wall(w.kind, w.reset)
    } else {
        match read {
            aterm_phase::Phase::Busy => AgentPhase::Busy,
            aterm_phase::Phase::Prompt => {
                subject = r
                    .prompt
                    .as_ref()
                    .map(|p| crate::needs::fold_clip(&p.command, 96))
                    .filter(|s| !s.is_empty());
                AgentPhase::Prompt {
                    detail: r.prompt.as_ref().map(prompt_detail),
                }
            }
            aterm_phase::Phase::Question => AgentPhase::Question,
            aterm_phase::Phase::Limited { message, reset } => wall(
                aterm_phase::classify_wall(&message).unwrap_or(aterm_phase::WallKind::UsageSession),
                reset,
            ),
            aterm_phase::Phase::Idle if r.survey => AgentPhase::Survey,
            aterm_phase::Phase::Idle => AgentPhase::Idle,
        }
    };
    // The box or question the verdict publishes is the frame a key lands on (fix E8): what each
    // choosing key would choose on it, from the options the reader parsed.
    let keys = match &phase {
        AgentPhase::Prompt { .. } => r
            .prompt
            .as_ref()
            .map(crate::session_log::FrameKeys::of_prompt),
        _ => None,
    };
    (
        AgentReading {
            phase,
            context_pct: r.context_left,
            composer,
        },
        subject,
        TurnFacts { keys, ..turn },
    )
}

/// The prompt's kind word and, for a Bash approval, the read-only rule's verdict on its command
/// ([`aterm_agent::supervise::policy::approval::readings_read_only`]).
fn prompt_detail(p: &aterm_phase::PromptV2) -> String {
    let kind = sanitize_token(p.kind.name(), 24);
    if p.kind != aterm_phase::PromptKind::Bash || p.command.trim().is_empty() {
        return kind;
    }
    let python_allow: Vec<String> = aterm_agent::supervise::DEFAULT_PYTHON_ALLOW
        .iter()
        .map(|g| (*g).to_string())
        .collect();
    let read_only =
        aterm_agent::supervise::policy::approval::readings_read_only(&p.readings(), &python_allow)
            .is_ok();
    let verdict = if read_only {
        "read-only"
    } else {
        "not-read-only"
    };
    format!("{kind}:{verdict}")
}

/// How long until `reset` falls on this machine's clock.
pub(crate) fn countdown_to_reset(reset: &str) -> Option<Duration> {
    countdown_in(
        reset,
        limit::unix_now(),
        &local_offset_at,
        local_zone().as_deref(),
    )
}

/// [`countdown_in`] at one constant offset — pure, so the rules are testable
/// at any hour.
#[cfg(test)]
fn countdown_at(reset: &str, now: i64, offset: i64, local_zone: Option<&str>) -> Option<Duration> {
    countdown_in(reset, now, &|_| Some(offset), local_zone)
}

/// [`countdown_to_reset`] with the clock, the offset function and the local zone passed in.
fn countdown_in(
    reset: &str,
    now: i64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
    local_zone: Option<&str>,
) -> Option<Duration> {
    let at = place_in(reset, now, offset_at, local_zone)?;
    (at > now).then(|| Duration::from_secs(at.abs_diff(now)))
}

/// Where `reset` falls on this machine's clock (unix seconds), placed as of `printed`.
pub(crate) fn place_reset(reset: &str, printed: i64) -> Option<i64> {
    place_in(reset, printed, &local_offset_at, local_zone().as_deref())
}

/// [`place_reset`] with the offset and the local zone passed in.
fn place_in(
    reset: &str,
    now: i64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
    local_zone: Option<&str>,
) -> Option<i64> {
    let spec = limit::parse_reset(reset)?;
    if let limit::ResetSpec::At {
        zone: Some(named), ..
    } = &spec
        && local_zone.is_some_and(|local| local != named)
    {
        return None;
    }
    limit::reset_in_zone(&spec, now, limit::SESSION_AHEAD_MAX, offset_at)
}

fn local_zone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if !tz.is_empty() {
            return Some(tz.to_string());
        }
    }
    local_zone_at(std::path::Path::new("/etc/localtime"))
}

fn local_zone_at(path: &std::path::Path) -> Option<String> {
    let link = std::fs::read_link(path).ok()?;
    let s = link.to_str()?;
    let i = s.find("zoneinfo/")?;
    Some(s[i + "zoneinfo/".len()..].to_string())
}

/// The local clock's offset from UTC in seconds now ([`local_offset_at`]).
pub(crate) fn local_offset_s() -> Option<i64> {
    local_offset_at(limit::unix_now())
}

/// The local zone's offset from UTC at the instant `unix`: aterm's one local-clock reader
/// (`aterm_uds::zone::local_offset_at`, Unix and Windows), the one the usage tap says its reset
/// on, so the glass and the tap's row name the same local instant.
pub(crate) fn local_offset_at(unix: i64) -> Option<i64> {
    aterm_uds::zone::local_offset_at(unix)
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "presence_corpus_tests.rs"]
mod corpus_tests;
