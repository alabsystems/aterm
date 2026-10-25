// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence: this host's sensing half.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_agent::supervise::limit;
use aterm_session::SessionId;
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;

pub(crate) use aterm_messages::presence::{
    ChipLevel, Hand, HoldFact, Link, NeedFact, NeedSeverity, PillAct, PillHit, PillKind, PillWords,
    StopCause, StoryVerb, TOLD_TEXT_MAX_BYTES, TurnFact, WallAnswer, fmt_dur, pill_words,
    prompt_band_word, prompt_kind_words, short_sid, state_line,
};
#[cfg(test)]
pub(crate) use aterm_messages::presence::{Level, Rim};

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
        show_pill: need.harness.is_none(),
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
    /// wall is the state it leaves the session in (`status_item::wall_words`).
    fn wall_band(kind: aterm_phase::WallKind) -> &'static str {
        if kind.reads_limited() {
            return "limited";
        }
        api_cause_words(kind).unwrap_or_else(|| crate::status_item::wall_words(kind.name()))
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

/// What an API error the network caused is called on the tab, in the menu and in the notification.
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
            let local = (at_unix + offset).rem_euclid(86_400);
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
    let ahead = plan.at_unix - now_unix;
    let AgentPhase::Wall {
        kind,
        reset: None,
        until: until @ None,
    } = &mut agent.phase
    else {
        return Some(agent);
    };
    if ahead <= 0 || !Native::wall_retries(*kind) {
        return Some(agent);
    }
    *until = now.checked_add(Duration::from_secs(ahead.unsigned_abs()));
    if let AgentPhase::Wall { reset, .. } = &mut agent.phase {
        reset.clone_from(&plan.clock);
    }
    Some(agent)
}

/// The server's agent verdict on one screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentVerdict {
    /// The foreground program is not an identified agent: `agent=-`, no band phase, no rim.
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
}

impl TurnFacts {
    /// What `r`, read by `program`'s reader, says of the turn.
    pub(crate) fn of(program: aterm_phase::Program, r: &aterm_phase::Reading) -> TurnFacts {
        match program {
            aterm_phase::Program::Claude => TurnFacts {
                ctx_pct: r.context_left,
                goal: Some(r.goal_active),
                interrupted: r.interrupted,
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

/// The one adapter between a session's screen and its published agent verdict.
pub(crate) fn agent_verdict(
    program: Option<&str>,
    program_pending: bool,
    known_agent: Option<aterm_phase::Program>,
    rows: &[String],
    cursor: Cursor,
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
    let (rows, cursor) =
        if whole.len() > rows.len() && reader.prompt(rows).is_some_and(|p| p.head_off_screen) {
            (whole, cursor)
        } else {
            (rows, cursor.on_rows_from(whole.len() - rows.len()))
        };
    let (reading, subject, turn) = classify(reader, rows, cursor, now);
    AgentVerdict::Agent {
        reading,
        by_frame,
        subject,
        program: reader.program(),
        turn,
    }
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
        },
        by_frame: false,
        subject: None,
        program,
        turn: TurnFacts::default(),
    }
}

/// Classify one screen with one program's reader.
pub(crate) fn classify(
    reader: &dyn aterm_phase::ScreenReader,
    rows: &[String],
    cursor: Cursor,
    now: Instant,
) -> (AgentReading, Option<String>, TurnFacts) {
    #[cfg(test)]
    CLASSIFIER_CALLS.with(|c| c.set(c.get() + 1));
    let r = match cursor {
        Cursor::At(row) => reader.read_at(rows, row, None),
        #[cfg(test)]
        Cursor::Unknown => reader.read(rows, None),
    };
    let turn = TurnFacts::of(reader.program(), &r);
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
        turn,
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
    u64::try_from(at - now)
        .ok()
        .filter(|&s| s > 0)
        .map(Duration::from_secs)
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

/// `struct tm` (`<time.h>`), laid out alike on Darwin and glibc: nine `int`s,
/// then the BSD/GNU `tm_gmtoff` and `tm_zone`.
#[cfg(unix)]
#[repr(C)]
struct Tm {
    _date_and_time: [libc::c_int; 9],
    tm_gmtoff: libc::c_long,
    _tm_zone: *const libc::c_char,
}

#[cfg(all(unix, target_pointer_width = "64"))]
const _: () = assert!(size_of::<Tm>() == 56);

#[cfg(unix)]
unsafe extern "C" {
    fn localtime_r(clock: *const libc::time_t, result: *mut Tm) -> *mut Tm;
    fn tzset();
}

/// The local zone's offset from UTC at the instant `unix`.
#[cfg(unix)]
pub(crate) fn local_offset_at(unix: i64) -> Option<i64> {
    refreshed_local_offset_at(
        unix,
        || {
            // SAFETY: `tzset` takes no pointers, serializes its C-library zone
            // state, and reads the environment without changing it, as does
            // `localtime_r`. No thread may mutate `TZ` beside these readers.
            unsafe { tzset() }
        },
        native_local_offset_at,
    )
}

/// [`local_offset_at`]'s order: `refresh`, then `read`, on every call.
#[cfg(unix)]
fn refreshed_local_offset_at(
    unix: i64,
    refresh: impl FnOnce(),
    read: impl FnOnce(i64) -> Option<i64>,
) -> Option<i64> {
    refresh();
    read(unix)
}

#[cfg(unix)]
fn native_local_offset_at(unix: i64) -> Option<i64> {
    let clock: libc::time_t = unix;
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: `clock` is a live `time_t` and `tm` a writable, correctly laid
    // out `struct tm` (its size pinned above); `localtime_r` writes only it.
    let filled = unsafe { localtime_r(&clock, tm.as_mut_ptr()) };
    if filled.is_null() {
        return None;
    }
    // SAFETY: a non-null answer filled every field; all are plain integers or
    // a pointer this never reads.
    Some(unsafe { tm.assume_init() }.tm_gmtoff)
}

/// `FILETIME` and `SYSTEMTIME` ([`local_offset_at`]).
#[cfg(windows)]
mod win_time {
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct FileTime {
        pub(super) low: u32,
        pub(super) high: u32,
    }

    /// Year, month, weekday, day, hour, minute, second, millisecond: only ever handed from one conversion to the next, never read here.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct SystemTime {
        _fields: [u16; 8],
    }

    unsafe extern "system" {
        pub(super) fn FileTimeToSystemTime(file: *const FileTime, system: *mut SystemTime) -> i32;
        pub(super) fn SystemTimeToTzSpecificLocalTime(
            zone: *const std::ffi::c_void,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
        pub(super) fn SystemTimeToFileTime(system: *const SystemTime, file: *mut FileTime) -> i32;
    }
}

/// Windows: the offset the system zone's rules give the instant `unix`, in seconds.
#[cfg(windows)]
pub(crate) fn local_offset_at(unix: i64) -> Option<i64> {
    use win_time::{
        FileTime, FileTimeToSystemTime, SystemTime, SystemTimeToFileTime,
        SystemTimeToTzSpecificLocalTime,
    };
    const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
    const TICKS_PER_SECOND: i64 = 10_000_000;
    let ticks = unix
        .checked_mul(TICKS_PER_SECOND)?
        .checked_add(UNIX_EPOCH_TICKS)?;
    let raw = u64::try_from(ticks).ok()?;
    let utc_file = FileTime {
        low: raw as u32,
        high: (raw >> 32) as u32,
    };
    let (mut utc, mut local) = (SystemTime::default(), SystemTime::default());
    let mut local_file = FileTime::default();
    // SAFETY: every pointer addresses a live, correctly laid out `repr(C)`
    // value of the type the call documents; a null zone names the active one.
    // Each call writes only its out parameter.
    let converted = unsafe {
        FileTimeToSystemTime(&utc_file, &mut utc) != 0
            && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
            && SystemTimeToFileTime(&local, &mut local_file) != 0
    };
    if !converted {
        return None;
    }
    let local_ticks =
        i64::try_from((u64::from(local_file.high) << 32) | u64::from(local_file.low)).ok()?;
    Some((local_ticks - ticks).div_euclid(TICKS_PER_SECOND))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn local_offset_at(_unix: i64) -> Option<i64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn aterms_own_hand_is_no_row_and_its_turn_is_no_story() {
        let now = t0();
        let turn = |id| {
            Some(TurnFact {
                id,
                settled: true,
                dur_ms: 10,
                carried: false,
                aterm: false,
                opened: None,
            })
        };
        let run = |holder: String, before: Option<TurnFact>, inside: bool| {
            let mut s = Slot::new(now);
            s.absorb(
                Facts {
                    turn: before,
                    ..Facts::default()
                },
                now,
            );
            let lease = Hand::DrivenLease {
                holder,
                pid: Some(std::process::id()),
            };
            let mut at = now;
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
            }
            s
        };
        let own = crate::harness_host::holder();
        for inside in [false, true] {
            let s = run(own.clone(), None, inside);
            assert_eq!(
                s.level(0),
                Level::Quiet,
                "aterm's turn read as news ({inside})"
            );
            assert_eq!(s.story_seq, 1, "the point is still kept (`story=`)");
            assert_eq!(s.chip(), ChipLevel::Off, "no mark on the tab");
        }
        let mut held = Slot::new(now);
        held.absorb(
            Facts {
                hand: Hand::DrivenLease {
                    holder: own.clone(),
                    pid: Some(std::process::id()),
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

        let s = run("manager".into(), None, true);
        assert_eq!(s.level(0), Level::Story, "a manager's turn is news");
        let s = run("aterm-harness@1".into(), None, false);
        assert_eq!(s.level(0), Level::Story);

        let s = run(own, turn(1), false);
        assert_eq!(s.level(0), Level::Story, "the earlier turn is still news");
        assert_eq!(
            (s.story_seq, s.level(1)),
            (2, Level::Quiet),
            "the earlier turn alone is news: read up to it, aterm's own turn is none"
        );
    }

    #[test]
    fn a_turn_the_harness_typed_is_no_story_without_a_refresh_inside_its_hand() {
        let now = t0();
        let level = |aterm: bool| {
            let mut s = Slot::new(now);
            s.absorb(Facts::default(), now);
            s.absorb(
                Facts {
                    turn: Some(TurnFact {
                        id: 3,
                        settled: false,
                        dur_ms: 600,
                        carried: false,
                        aterm,
                        opened: None,
                    }),
                    ..Facts::default()
                },
                now + Duration::from_secs(1),
            );
            (s.level(0), s.story_seq)
        };
        assert_eq!(
            level(true),
            (Level::Quiet, 1),
            "the harness's turn is kept, not news"
        );
        assert_eq!(
            level(false),
            (Level::Story, 1),
            "anyone else's turn is news"
        );
    }

    #[test]
    fn classify_is_aterm_phases_verdict() {
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
            let (ours, _, _) = classify(
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
        let (limited, _, _) = classify(
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

    #[test]
    fn the_memory_banner_publishes_wall_memory_under_a_busy_spinner() {
        use aterm_phase::prompt::fixtures::composer;
        let banner = format!(
            "{} (140.4GB) \u{2014} restart and resume with claude --continue",
            aterm_phase::anchor!("wall.memory"),
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
        let live = screen(&format!("{banner:>118}"), "");
        let v = agent_verdict(Some("claude"), false, None, &live, Cursor::Unknown, t0());
        assert_eq!(v.word(), "wall:memory");
        assert_eq!(v.detail(), None);
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

    #[test]
    fn a_reading_without_evidence_is_unknown_not_idle() {
        use aterm_phase::prompt::fixtures::{TRUST, screen};
        let idle = screen("\u{203a} ready\n\n  gpt-5 \u{00b7} 100% context left\n");
        let gate = screen(
            "  Do you trust the contents of this directory?\n\n\u{203a} 1. Yes, continue\n  2. No, \
             quit\n\n  Press enter to continue\n",
        );
        for rows in [idle, gate] {
            match agent_verdict(Some("codex"), false, None, &rows, Cursor::Unknown, t0()) {
                AgentVerdict::Agent { reading, .. } => {
                    assert_eq!(reading.phase, AgentPhase::Unknown);
                    assert_eq!(reading.phase.word(), "unknown");
                }
                AgentVerdict::NotAgent => panic!("codex is an agent by name"),
            }
        }
        let claude = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(TRUST),
            Cursor::Unknown,
            t0(),
        );
        assert_eq!(claude.word(), "prompt", "the control");
    }

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

    #[test]
    fn an_api_wall_says_its_cause() {
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
        for cause in [
            ApiCause::Unreachable,
            ApiCause::CutOff,
            ApiCause::Config,
            ApiCause::Request,
            ApiCause::Overlong,
            ApiCause::Server,
        ] {
            assert_eq!(wall(cause).word(), "wall:api-error", "{cause:?}");
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
        let cut = agent_verdict(
            Some("claude"),
            false,
            None,
            &screen(API_ERROR_SLEEP),
            Cursor::Unknown,
            now,
        );
        assert_eq!(cut.subject(), Some(API_CUT_OFF));
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
    }

    #[test]
    fn a_retry_plan_is_laid_on_an_api_wall_and_nothing_else() {
        use aterm_phase::{ApiCause, WallKind};
        const NOW_UNIX: i64 = 1_789_660_500;
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
            let plan = at.map(|at| RetryPlan::with_offset(at, zone));
            with_retry_plan(agent, plan.as_ref(), NOW_UNIX, now)
        };
        let got = lay(wall(unreachable, None), Some(NOW_UNIX + 180), Some(PDT)).unwrap();
        assert_eq!(got.context_pct, Some(40), "nothing else moves");
        let AgentPhase::Wall { kind, reset, until } = got.phase else {
            panic!("still a wall");
        };
        assert_eq!(kind, unreachable, "the wall's kind is the screen's");
        assert_eq!(reset.as_deref(), Some("08:58"));
        assert_eq!(until, now.checked_add(Duration::from_secs(180)));
        let got = lay(
            wall(unreachable, None),
            Some(NOW_UNIX + 180),
            Some(PDT + 3600),
        )
        .unwrap();
        let AgentPhase::Wall { reset, .. } = got.phase else {
            panic!("still a wall");
        };
        assert_eq!(reset.as_deref(), Some("09:58"));
        let got = lay(wall(WallKind::Overloaded, None), Some(NOW_UNIX + 60), None).unwrap();
        let AgentPhase::Wall { reset, until, .. } = got.phase else {
            panic!("still a wall");
        };
        assert_eq!((reset, until.is_some()), (None, true));
        let rate = WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        };
        assert!(rate.reads_limited(), "a 429 reads `limited`");
        let got = lay(wall(rate, None), Some(NOW_UNIX + 300), Some(PDT)).unwrap();
        let AgentPhase::Wall { reset, until, .. } = got.phase else {
            panic!("still a wall");
        };
        assert_eq!(reset.as_deref(), Some("09:00"));
        assert_eq!(until, now.checked_add(Duration::from_secs(300)));
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
                "a rate limit's own reset",
                wall(rate, Some("7:30pm")),
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
    }

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

    #[test]
    fn the_countdown_is_to_the_reset_on_this_clock() {
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
        if let Some(offset) = local_offset_s()
            && local_offset_at(limit::unix_now() + 2 * 3600) == Some(offset)
        {
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

    /// The in-process offset equals `date`'s (`limit::offset_at`) at instants
    /// across a year and on each side of the 2026 US and EU daylight-saving
    /// switches, so a switch this machine's zone has is crossed.
    #[cfg(unix)]
    #[test]
    fn the_in_process_offset_is_the_one_date_reads() {
        const JAN_1_2026: i64 = 1_767_225_600;
        let switches = [1_772_964_000, 1_774_746_000, 1_792_890_000, 1_793_523_600];
        let instants = (0..12)
            .map(|month| JAN_1_2026 + month * 30 * 86_400)
            .chain(switches.iter().flat_map(|&at| [at - 1, at]));
        let mut compared = 0;
        for at in instants {
            let Some(reference) = limit::offset_at(None, at) else {
                continue;
            };
            assert_eq!(local_offset_at(at), Some(reference), "at {at}");
            compared += 1;
        }
        assert!(compared > 0, "`date` answered for no instant");
    }

    /// Windows: the offset now is the one the zone record says is in force
    /// now (`GetTimeZoneInformation`: its bias, plus the daylight or standard
    /// bias it reports active), a second reading by another API.
    #[cfg(windows)]
    #[test]
    fn the_windows_offset_now_is_the_zone_records() {
        #[repr(C)]
        struct TimeZoneInformation {
            bias: i32,
            _standard_name: [u16; 32],
            _standard_date: [u16; 8],
            standard_bias: i32,
            _daylight_name: [u16; 32],
            _daylight_date: [u16; 8],
            daylight_bias: i32,
        }
        unsafe extern "system" {
            fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
        }
        const TIME_ZONE_ID_DAYLIGHT: u32 = 2;
        let mut info = std::mem::MaybeUninit::<TimeZoneInformation>::zeroed();
        // SAFETY: `info` is writable storage for one TIME_ZONE_INFORMATION.
        let id = unsafe { GetTimeZoneInformation(info.as_mut_ptr()) };
        assert_ne!(id, u32::MAX, "the zone record reads");
        // SAFETY: the call above filled the structure.
        let info = unsafe { info.assume_init() };
        let active = if id == TIME_ZONE_ID_DAYLIGHT {
            info.daylight_bias
        } else {
            info.standard_bias
        };
        let now = limit::unix_now();
        let offset = local_offset_at(now).expect("the zone reads");
        assert_eq!(offset, -i64::from(info.bias + active) * 60);
        assert_eq!(offset % (15 * 60), 0, "{offset}");
        assert!((-12 * 3600..=14 * 3600).contains(&offset), "{offset}");
        assert_eq!(local_offset_at(-11_644_473_601), None, "before 1601");
    }

    /// Replacing the system zone changes its name as well as its offsets: a
    /// notice naming the former zone is no figure under the new one, and one
    /// naming the new zone is placed with its offsets.
    #[cfg(unix)]
    #[test]
    fn the_countdown_tracks_replacement_of_the_local_zone_link() {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let path = dir.path().join("localtime");
        std::os::unix::fs::symlink("/usr/share/zoneinfo/America/Los_Angeles", &path).unwrap();
        let now = 1_789_812_000_i64;
        let source_offset = std::cell::Cell::new(-7 * 3600);
        let cached_offset = std::cell::Cell::new(-7 * 3600);
        let offset_at = |unix| {
            refreshed_local_offset_at(
                unix,
                || cached_offset.set(source_offset.get()),
                |_| Some(cached_offset.get()),
            )
        };
        let old = local_zone_at(&path);
        assert_eq!(old.as_deref(), Some("America/Los_Angeles"));
        assert_eq!(
            countdown_in("5pm (America/Los_Angeles)", now, &offset_at, old.as_deref()),
            Some(Duration::from_secs(14 * 3600))
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/usr/share/zoneinfo/Etc/UTC", &path).unwrap();
        source_offset.set(0);
        let current = local_zone_at(&path);
        assert_eq!(current.as_deref(), Some("Etc/UTC"));
        assert_eq!(
            countdown_in(
                "5pm (Etc/UTC)",
                now,
                &|_| Some(cached_offset.get()),
                current.as_deref(),
            ),
            Some(Duration::from_secs(14 * 3600))
        );
        assert_eq!(
            countdown_in(
                "5pm (America/Los_Angeles)",
                now,
                &offset_at,
                current.as_deref(),
            ),
            None,
            "the former zone's notice must not use the new zone's offset"
        );
        assert_eq!(
            countdown_in("5pm (Etc/UTC)", now, &offset_at, current.as_deref()),
            Some(Duration::from_secs(7 * 3600))
        );
    }

    #[test]
    fn the_hold_reason_is_decoded_and_sanitized_on_every_surface() {
        assert_eq!(hold_reason_words("main%20broken"), "main broken");
        assert_eq!(
            hold_reason_words("main%e2%80%aebroken%0a%1b[31m"),
            "mainbroken [31m"
        );
        assert_eq!(hold_reason_words("-"), "-");
        assert_eq!(
            hold_reason_words("hold=x%20%e2%8a%98%20review"),
            "hold=x review"
        );
    }

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

    #[test]
    fn a_bash_verdict_is_read_only_only_when_every_reading_is() {
        use aterm_agent::supervise::classify::classify_command;
        let all_read_only = |rows: &[String]| {
            let p = aterm_phase::parse_prompt_v2(rows).expect("a box");
            let readings = p.readings();
            !readings.is_empty() && readings.iter().all(|r| classify_command(r).read_only)
        };
        let one = bash_box(&["git log --oneline -5", "Show the five most recent commits"]);
        assert_eq!(detail_of(&one).as_deref(), Some("bash:read-only"));

        let bars = bash_box(&["\u{2502} git status", "\u{2502} node scripts/migrate.js"]);
        let p = aterm_phase::parse_prompt(&bars).expect("a box");
        assert_eq!(p.command, "git status node scripts/migrate.js");
        assert!(
            classify_command(&p.command).read_only,
            "NEGATIVE CONTROL: the flattened reading is read-only"
        );
        assert_eq!(detail_of(&bars).as_deref(), Some("bash:not-read-only"));

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
        let wrapped = bash_box(&["git status", "node scripts/migrate.js"]);
        let want = if all_read_only(&wrapped) {
            "bash:read-only"
        } else {
            "bash:not-read-only"
        };
        assert_eq!(detail_of(&wrapped).as_deref(), Some(want));
    }

    #[test]
    fn a_bash_verdict_is_the_read_only_rules_on_the_command_as_drawn() {
        use aterm_agent::supervise::classify::classify_command;
        use aterm_agent::supervise::policy::approval::readings_read_only;
        use aterm_phase::prompt::fixtures::{
            BOX_BASH_AWK_MULTI_LINE_2_1_308, BOX_BASH_FIND_CONTINUED_2_1_308, screen,
        };
        let continued = screen(BOX_BASH_FIND_CONTINUED_2_1_308);
        let p = aterm_phase::parse_prompt_v2(&continued).expect("the box");
        assert_eq!(p.readings().len(), 2, "{:?}", p.readings());
        for reading in p.readings() {
            assert!(classify_command(&reading).read_only, "{reading:?}");
        }
        let refused = readings_read_only(&p.readings(), &[]);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("a backslash ends a line")),
            "{refused:?}"
        );
        assert_eq!(detail_of(&continued).as_deref(), Some("bash:not-read-only"));
        let awk = screen(BOX_BASH_AWK_MULTI_LINE_2_1_308);
        let p = aterm_phase::parse_prompt_v2(&awk).expect("the box");
        assert_eq!(readings_read_only(&p.readings(), &[]), Ok(()));
        assert_eq!(detail_of(&awk).as_deref(), Some("bash:read-only"));
    }

    #[test]
    fn a_bash_verdict_reads_the_default_python_allowlist() {
        use aterm_agent::supervise::policy::approval::readings_read_only;
        let rows = bash_box(&["python3 report.py"]);
        let p = aterm_phase::parse_prompt_v2(&rows).expect("a box");
        let refused = readings_read_only(&p.readings(), &[]);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("not on the read-only script allowlist")),
            "{refused:?}"
        );
        assert_eq!(detail_of(&rows).as_deref(), Some("bash:not-read-only"));
        assert_eq!(
            readings_read_only(&p.readings(), &["report.py".to_string()]),
            Ok(())
        );
    }

    #[test]
    fn a_bash_verdict_refuses_a_secret_and_a_proof_gap() {
        use aterm_agent::supervise::classify::classify_command;
        for line in ["cat ~/.ssh/id_rsa", "FOO=1 cat README.md"] {
            assert!(classify_command(line).read_only, "{line}");
            assert_eq!(
                detail_of(&bash_box(&[line])).as_deref(),
                Some("bash:not-read-only"),
                "{line}"
            );
        }
        for line in ["cat README.md", "grep -m 1 credentials src/"] {
            assert_eq!(
                detail_of(&bash_box(&[line])).as_deref(),
                Some("bash:read-only"),
                "{line}"
            );
        }
    }

    #[test]
    fn a_node_run_codex_is_named_by_no_screen() {
        use aterm_phase::Program;
        use aterm_phase::prompt::fixtures::{composer, rows, screen};
        let idle = screen("\u{203a} Ask Codex to do anything\n\n  gpt-5 \u{00b7} ~/src\n");
        assert!(
            matches!(
                agent_verdict(Some("node"), false, None, &idle, Cursor::Unknown, t0()),
                AgentVerdict::NotAgent
            ),
            "a Codex composer names nothing"
        );
        match agent_verdict(
            Some("node"),
            false,
            Some(Program::Codex),
            &idle,
            Cursor::Unknown,
            t0(),
        ) {
            AgentVerdict::Agent {
                program, reading, ..
            } => {
                assert_eq!(program, Program::Codex);
                assert_eq!(reading.phase, AgentPhase::Unknown);
            }
            AgentVerdict::NotAgent => panic!("the job's identity is kept"),
        }
        let mut framed = rows(&["\u{23fa} Done.", ""]);
        framed.extend(composer("  ? for shortcuts"));
        match agent_verdict(Some("node"), false, None, &framed, Cursor::Unknown, t0()) {
            AgentVerdict::Agent {
                by_frame, program, ..
            } => assert!(by_frame && program == Program::Claude, "{program:?}"),
            AgentVerdict::NotAgent => panic!("the control: Claude Code's frame identifies it"),
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

    /// The band's countdown places a reset with the offset that holds at the
    /// reset, not one process-wide offset: a dated reset on the far side of a
    /// daylight-saving change counts down to the real instant.
    #[cfg(unix)]
    #[test]
    fn r44_hb5_the_reset_countdown_places_a_reset_with_the_offset_that_holds_then() {
        if !crate::test_alone::in_child() {
            crate::test_alone::run_alone_with(&[("TZ", std::ffi::OsStr::new("America/New_York"))]);
            return;
        }
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        fn month_day(days: i64) -> (usize, i64) {
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            (usize::try_from(m).unwrap(), d)
        }
        let Some(now_offset) = local_offset_s() else {
            aterm_spec::host::could_not_run("no local offset reads here");
        };
        let now = limit::unix_now();
        let Some((probe, then_offset)) = [90i64, 120, 150, 180, 210, 240]
            .into_iter()
            .map(|days| now + days * 86_400)
            .find_map(|t| {
                local_offset_at(t)
                    .filter(|o| *o != now_offset)
                    .map(|o| (t, o))
            })
        else {
            aterm_spec::host::could_not_run(
                "America/New_York keeps one offset here: no zone database to read it from",
            );
        };
        let (m, d) = month_day((probe + then_offset).div_euclid(86_400));
        let reset = format!("{} {d} at 12pm", MONTHS[m - 1]);
        let got = countdown_to_reset(&reset).expect("a figure");
        let spec = limit::parse_reset(&reset).expect("the grammar reads it");
        let now = limit::unix_now();
        let at = limit::reset_in_zone(&spec, now, limit::SESSION_AHEAD_MAX, &|t| {
            local_offset_at(t)
        })
        .expect("placed");
        let want = u64::try_from(at - now).expect("ahead");
        let off_by = got.as_secs().abs_diff(want);
        assert!(
            off_by <= 5,
            "`{reset}` counted down {got:?}, the reset is {want} s away: {off_by} s off \
             (offset now {now_offset}, then {then_offset})"
        );
    }
}

#[cfg(test)]
#[path = "presence_corpus_tests.rs"]
mod corpus_tests;
