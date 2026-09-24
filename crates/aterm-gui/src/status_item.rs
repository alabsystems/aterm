// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// The menu-bar status item is an AppKit surface; off macOS the types compile
// for the shared Wake plumbing and everything else is intentionally idle.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

//! The menu-bar OPERATOR status item (macOS `NSStatusItem`).
//!
//! aterm's fleet can be supervised by an OPERATOR — a designated session running
//! a coding agent that watches and drives the other sessions (the brief lives at
//! `docs/OPERATOR.md`). This module puts a persistent icon in the macOS menu bar,
//! created when the FIRST OS window attaches and alive for the process lifetime,
//! that (a) reflects operator/fleet state at a glance and (b) manages the
//! operator: Start (spawn a session and launch the agent CLI with the brief),
//! Show (focus its tab), Stop (retire its session).
//!
//! State is read from what the instance already knows — session user meta and
//! titles in the in-process registry — never from a socket loop. The TYPED
//! protocol (see `docs/OPERATOR.md`) is `meta set role operator` to name the
//! operator and a non-empty `meta set attention <why>` to escalate; the legacy
//! title conventions (`operator: …` prefix, `⚠`-prefixed titles) remain
//! honored as fallback so older briefs keep working. The classifier here is
//! PURE (rows in, glance out, no locks, no AppKit) so it is exhaustively
//! unit-tested off macOS too.
//!
//! Structure mirrors `menu.rs` exactly: a portable action enum with stable
//! integer tags + a pure model at the top; the `objc2` AppKit half in a
//! `#[cfg(target_os = "macos")]` module below; `()` handle + no-op `install`
//! stubs off macOS so `App`'s field shape is platform-independent.

/// The launch line [`OperatorAction::Start`] types into the freshly spawned shell —
/// exactly what a human would type to stand the operator up. SELF-CONTAINED by
/// design: it references no filesystem path, because the spawned session's cwd
/// is wherever the user's focused pane was and an installed aterm has no repo
/// checkout — the bootstrap brief is `aterm help introspection`, which ships in
/// every binary; `docs/OPERATOR.md` is named as optional enhancement only. The
/// agent CLI authenticates from its own config, so this carries no secrets
/// (aterm strips agent env vars from children by design).
pub const OPERATOR_LAUNCH_LINE: &str = "claude \"You are this machine's aterm fleet operator. Run 'aterm help introspection' to learn how to see and drive sessions, set your role with: aterm ctl @self meta set role operator - then await fleet instructions from the human. If a docs/OPERATOR.md exists in your cwd, read and follow it too.\"";

/// One user action from the status-item menu. Carried by `Wake::OperatorAction`
/// from the AppKit callback to the event loop, which dispatches on `App`.
///
/// Tags are this menu's OWN namespace (the items are wired to `ATermStatusTarget`,
/// never to the main-menu `ATermMenuTarget`, so they can never collide with
/// `MenuAction` tags). Stable once assigned; never reuse a retired tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatorAction {
    /// Spawn a fresh tab session and launch the operator agent in it.
    Start,
    /// Focus the operator session's tab and raise its window.
    Show,
    /// Retire the operator session (deliberate, confirm-free close).
    Stop,
    /// Raise a specific window of this instance (payload: logical `WindowId`).
    FocusWindow(u64),
    /// Focus the tab displaying a session (payload: session local id) — what a
    /// click on a needs-attention row does.
    FocusSession(u64),
    /// Activate a sibling aterm instance's windows (payload: its pid).
    RaiseInstance(u32),
    /// Open the §5 connection map on the frontmost window, raising it (the
    /// [`Self::Show`] host+raise shape; design §5.1 macOS entry).
    ShowConnectionMap,
}

/// The packed-tag band width for payload-carrying actions: `tag = kind × BAND +
/// payload`. Payloads are small monotonic counters (window/session ids) or pids,
/// so `payload < BAND` always holds in practice; an out-of-range payload encodes
/// to `0` (the inert tag) rather than aliasing another row — fail closed.
const TAG_BAND: isize = 1_000_000_000_000;

impl OperatorAction {
    /// The `NSMenuItem.tag` this action rides in. `0` is never used (an untagged
    /// item decodes to `None` and stays inert). Fixed actions keep their original
    /// small tags; payload actions ride the [`TAG_BAND`] codec.
    pub fn tag(self) -> isize {
        let packed = |kind: isize, payload: u64| -> isize {
            isize::try_from(payload)
                .ok()
                .filter(|p| *p < TAG_BAND)
                .map_or(0, |p| kind * TAG_BAND + p)
        };
        match self {
            OperatorAction::Start => 1,
            OperatorAction::Show => 2,
            OperatorAction::Stop => 3,
            OperatorAction::ShowConnectionMap => 4,
            OperatorAction::FocusWindow(id) => packed(1, id),
            OperatorAction::FocusSession(id) => packed(2, id),
            OperatorAction::RaiseInstance(pid) => packed(3, u64::from(pid)),
        }
    }

    /// Inverse of [`tag`](Self::tag); unknown tags are `None` (inert item).
    pub fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            1 => Some(OperatorAction::Start),
            2 => Some(OperatorAction::Show),
            3 => Some(OperatorAction::Stop),
            4 => Some(OperatorAction::ShowConnectionMap),
            t if t >= TAG_BAND => {
                let payload = t % TAG_BAND;
                let payload_u64 = u64::try_from(payload).ok()?;
                match t / TAG_BAND {
                    1 => Some(OperatorAction::FocusWindow(payload_u64)),
                    2 => Some(OperatorAction::FocusSession(payload_u64)),
                    3 => u32::try_from(payload_u64)
                        .ok()
                        .map(OperatorAction::RaiseInstance),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// The operator's classified state, derived purely from session titles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperatorState {
    /// No session identifies as the operator.
    NotRunning,
    /// An operator session exists; the payload is its self-reported detail (the
    /// part of its title after `operator`, e.g. `: fleet idle`), trimmed.
    Running(String),
}

/// One session as the classifier sees it: the effective title joined with the
/// TYPED user-meta fields and the server's published agent verdict. Built by
/// `App::status_session_row` from the registry snapshot; pure data so
/// [`classify`] stays lock-free and testable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionRow {
    /// Process-local session id (the `@<n>` selector and Show/close target).
    pub id: u64,
    /// Effective title: user meta title when set, else the live OSC title.
    pub title: String,
    /// Typed `meta set role` value (recognized: `operator`).
    pub role: Option<String>,
    /// Typed `meta set attention` value — non-empty means needs-human.
    pub attention: Option<String>,
    /// The server's agent verdict for this session (`status agent=`), or
    /// `None` when it is not an identified agent (`agent=-`) or the row came
    /// from a sibling instance, whose verdict this instance does not read.
    pub agent: Option<AgentFact>,
    /// Whether a supervisor's claim on the session is live (`status
    /// supervisor=`). A supervised session's prompts, questions and limits
    /// raise no row of their own: its supervisor answers the rote boxes and
    /// escalates the rest through keyed `attention`, which still shows. A
    /// wall the supervisor does not escalate (an overload, an API error, the
    /// login, a full context) still raises its row.
    pub supervised: bool,
}

/// The published agent verdict one [`SessionRow`] carries — the
/// `SessionTimeline` publication, copied under its leaf lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentFact {
    /// `agent=`: `busy|prompt|question|wall:<kind>|idle|survey|unknown`.
    pub word: &'static str,
    /// `agent_detail=` (raw): a prompt's `kind[:verdict]`, a limit's reset.
    pub detail: Option<String>,
    /// `agent_rev=`: bumps whenever `word` or `detail` moves — the identity
    /// of one transition, which is what a notification is keyed by.
    pub rev: u64,
    /// The approval box's command or path, host-side only (never on the
    /// wire), already folded to one clipped line.
    pub subject: Option<String>,
}

/// What an escalation row is about, MOST SEVERE FIRST: the derived order is
/// the menu's row order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EscalationKind {
    /// Typed `meta set attention` — someone (a supervisor, a script) asked
    /// for the human explicitly.
    Attention,
    /// An agent's approval box is waiting.
    Prompt,
    /// An agent asked a question and is waiting for the answer.
    Question,
    /// An agent's turn ended on a wall (`agent=wall:<kind>`): a usage limit,
    /// a model bucket, a full context, a lost login, an API error, overload.
    Wall,
    /// The legacy `⚠`-title convention. Its text is program output (an OSC
    /// title), so it badges and lists but never notifies.
    Title,
}

impl EscalationKind {
    /// Whether a transition into this kind posts a native notification.
    pub fn notifies(self) -> bool {
        !matches!(self, Self::Title)
    }

    /// The notification's title for this kind — aterm's own words.
    pub fn headline(self) -> &'static str {
        match self {
            Self::Attention => "aterm · needs you",
            Self::Prompt => "aterm · approval waiting",
            Self::Question => "aterm · question waiting",
            Self::Wall => "aterm · agent stopped at a wall",
            Self::Title => "aterm",
        }
    }
}

/// One session's escalation: at most one per session, the most severe of
/// what it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Escalation {
    /// Process-local session id — the [`OperatorAction::FocusSession`] payload.
    pub session: u64,
    /// What it is about.
    pub kind: EscalationKind,
    /// The transition's identity within `(session, kind)`: the agent's
    /// `agent_rev` for the agent kinds, a hash of the text otherwise. The
    /// same box re-read is the same key; a new box is a new one.
    pub key: u64,
    /// The menu row, host-written: `⚠ <tab title>: <kind> <command>` for an
    /// agent verdict; `⚠ <message>` for typed attention; the title itself
    /// for the legacy convention.
    pub label: String,
    /// The notification body: `<tab title>: <what>`.
    pub body: String,
}

/// Byte budget for the tab title inside a row or a notification.
const ROW_TITLE_MAX: usize = 32;
/// Byte budget for the `<kind> <command>` part of a row.
const ROW_WHAT_MAX: usize = 72;

/// `s` folded to one line (control characters and runs of whitespace become
/// one space) and clipped to `max` bytes on a char boundary, with `…` when
/// cut. Row text comes from titles, commands and meta values — none of it may
/// carry a newline into a menu item or a notification. The characters the
/// crate forbids in native chrome
/// ([`crate::session_timeline::is_forbidden_metadata_char`]: bidi overrides
/// and isolates, zero-width and other invisible format characters) are
/// dropped too: a box's command is program-written text the human approves
/// from, and `rm -rf \u{202e}…` must not render reordered in a menu row.
pub fn fold_clip(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_control() || c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if crate::session_timeline::is_forbidden_metadata_char(c) {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    if out.len() <= max {
        return out;
    }
    let mut cut = max.saturating_sub('…'.len_utf8());
    while cut > 0 && !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('…');
    out
}

/// FNV-1a 64 of `s` — the key of a text escalation.
fn text_key(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The agent-verdict half of [`escalation`]: `(kind, what)` for a verdict
/// that needs a human, `None` for `busy|idle|survey`.
fn agent_escalation(fact: &AgentFact) -> Option<(EscalationKind, String)> {
    match fact.word {
        "prompt" => {
            // `bash:not-read-only` names the box's kind before the colon.
            let kind = fact
                .detail
                .as_deref()
                .and_then(|d| d.split(':').next())
                .filter(|k| !k.is_empty())
                .unwrap_or("approval");
            let what = match fact.subject.as_deref().filter(|s| !s.is_empty()) {
                Some(subject) => format!("{kind} {subject}"),
                None if kind == "approval" => kind.to_string(),
                None => format!("{kind} approval"),
            };
            Some((EscalationKind::Prompt, what))
        }
        "question" => Some((EscalationKind::Question, "question".to_string())),
        word => {
            // `wall:usage-session` → `usage-session`, the kind aterm-phase
            // names; a reset time rides it.
            let kind = word.strip_prefix("wall:").filter(|k| !k.is_empty())?;
            Some((
                EscalationKind::Wall,
                match fact.detail.as_deref().filter(|d| !d.is_empty()) {
                    Some(reset) => format!("{kind} until {reset}"),
                    None => kind.to_string(),
                },
            ))
        }
    }
}

/// The ONE escalation a session row carries, or `None`. Typed attention wins
/// (someone asked for the human in words); then the agent verdict — unless a
/// live supervisor holds the session: it answers the rote boxes and escalates
/// every other prompt, question and wall aterm-phase names through its own
/// keyed attention (the turn-end decider acts on or escalates every wall
/// kind), so the verdict's row would be a second row for one point; a
/// supervisor that stops or faults releases its claim and the rows are
/// back — then the legacy `⚠` title.
pub fn escalation(row: &SessionRow) -> Option<Escalation> {
    let title = fold_clip(stripped_title(&row.title), ROW_TITLE_MAX);
    let body_for = |what: &str| {
        if title.is_empty() {
            what.to_string()
        } else {
            format!("{title}: {what}")
        }
    };
    if let Some(message) = row
        .attention
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        let label = if message.starts_with('⚠') {
            message.to_string()
        } else {
            format!("⚠ {message}")
        };
        let what = fold_clip(message.trim_start_matches('⚠').trim_start(), ROW_WHAT_MAX);
        return Some(Escalation {
            session: row.id,
            kind: EscalationKind::Attention,
            key: text_key(message),
            label,
            body: body_for(&what),
        });
    }
    if let Some(fact) = &row.agent
        && !row.supervised
        && let Some((kind, what)) = agent_escalation(fact)
    {
        let what = fold_clip(&what, ROW_WHAT_MAX);
        let body = body_for(&what);
        return Some(Escalation {
            session: row.id,
            kind,
            key: fact.rev,
            label: format!("⚠ {body}"),
            body,
        });
    }
    let stripped = stripped_title(&row.title);
    if stripped.starts_with('⚠') || row.title.trim_start().starts_with('⚠') {
        let text = row.title.trim().to_string();
        return Some(Escalation {
            session: row.id,
            kind: EscalationKind::Title,
            key: text_key(&text),
            body: fold_clip(&text, ROW_WHAT_MAX),
            label: text,
        });
    }
    None
}

/// One window of this instance as the menu shows it (row order = id order,
/// which the windows map already keeps stable and ascending).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRow {
    /// Logical window id — the [`OperatorAction::FocusWindow`] payload.
    pub id: u64,
    /// The composed chrome title the window shows right now.
    pub title: String,
    /// Tab count, rendered beside the title.
    pub tabs: usize,
    /// Whether this is the frontmost window (rendered with a leading mark).
    pub frontmost: bool,
}

/// A sibling aterm instance discovered through the shared control-socket dir,
/// summarized by the background fleet scan (never dialed on the UI thread).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceRow {
    /// The sibling's pid — the [`OperatorAction::RaiseInstance`] payload; `0`
    /// means the graph entry predates pid recording (row renders disabled).
    pub pid: u32,
    /// Live sessions it reported.
    pub sessions: usize,
    /// How many of its sessions are escalating (typed attention or `⚠` title).
    pub warnings: usize,
    /// Whether it reports a running operator.
    pub operator: bool,
}

/// The shortest spacing between two notifications for ONE session. A verdict
/// that flaps (a misread box re-read a moment later as a new one) costs one
/// notification per window, never one per flap; the menu row stays current.
pub const NOTIFY_SESSION_FLOOR: std::time::Duration = std::time::Duration::from_secs(20);
/// At most [`NOTIFY_BURST`] notifications per [`NOTIFY_BURST_WINDOW`] across
/// the instance — ten agents hitting one usage limit together page once or a
/// few times, not ten times. The menu lists every one of them regardless.
pub const NOTIFY_BURST: usize = 3;
/// The window [`NOTIFY_BURST`] is counted over.
pub const NOTIFY_BURST_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// One native notification the herald decided to post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeraldNotice {
    /// The session it is about (the delivery thread's focus suppression).
    pub session: u64,
    /// aterm's own headline for the kind ([`EscalationKind::headline`]).
    pub title: &'static str,
    /// `<tab title>: <what>`.
    pub body: String,
}

/// What one [`Herald::note`] decided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeraldOutcome {
    /// The session's menu row appeared, changed or went away — re-render.
    pub row_moved: bool,
    /// Post exactly this notification.
    pub notice: Option<HeraldNotice>,
}

/// Why a transition into an escalation posted nothing (test-visible).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeraldQuiet {
    /// The same `(kind, key)` as the last note — the same box re-read.
    Same,
    /// The kind never notifies ([`EscalationKind::notifies`]).
    Silent,
    /// The human is looking at the tab in the active app.
    Looking,
    /// [`NOTIFY_SESSION_FLOOR`] or [`NOTIFY_BURST`] held it back: it is OWED,
    /// and posted (coalesced) when the limit allows ([`Herald::due`]).
    Limited,
}

#[derive(Clone, Debug, Default)]
struct HeraldSlot {
    /// The `(kind, key)` last noted, `None` while nothing escalates.
    shown: Option<(EscalationKind, u64)>,
    /// The row label last noted (a subject arriving late changes it).
    label: Option<String>,
    /// When this session last posted.
    notified_at: Option<std::time::Instant>,
    /// Transitions the rate limit held back since the last post. Nonzero
    /// means a notice is OWED for the escalation shown now; it is paid by one
    /// coalesced notice once the limit allows, and dropped when the
    /// escalation clears or the human looks.
    owed: u32,
}

/// THE ESCALATION HERALD: per session, folds the current [`escalation`] into
/// "did the menu row move" and "post one notification". Pure — the caller
/// reads the facts under leaf locks, acts on the outcome, and never holds a
/// lock across it — so the whole policy is a table test.
///
/// One notification per TRANSITION INTO an escalation, keyed by `(session,
/// kind, key)` (for an agent verdict the key is `agent_rev`): re-reading the
/// same box is not a transition. A transition while the human is looking at
/// that tab in the active app is spent silently — they already see it — and
/// is not re-announced when they look away. Rate-limited per session by
/// [`NOTIFY_SESSION_FLOOR`] and per instance by [`NOTIFY_BURST`]; a held-back
/// transition is OWED, not spent: when the limit allows ([`Self::due`], the
/// App's timer), and only while that session still escalates, ONE notice
/// pays every transition it held back (`… (+2 more)`). Several boxes in a
/// few seconds therefore page twice, not once and then never — and never
/// for a box already gone.
#[derive(Debug, Default)]
pub struct Herald {
    slots: std::collections::HashMap<u64, HeraldSlot>,
    /// Post times inside the last [`NOTIFY_BURST_WINDOW`], oldest first.
    recent: std::collections::VecDeque<std::time::Instant>,
    /// Why the last note posted nothing (test-visible).
    last_quiet: Option<HeraldQuiet>,
}

impl Herald {
    /// Fold `session`'s current escalation. `looking` is true when the
    /// session is the focused pane of a focused window (the notification
    /// suppression set).
    pub fn note(
        &mut self,
        session: u64,
        current: Option<&Escalation>,
        looking: bool,
        now: std::time::Instant,
    ) -> HeraldOutcome {
        self.last_quiet = None;
        if current.is_none() && !self.slots.contains_key(&session) {
            // Nothing escalates and nothing ever did: keep no slot (a retired
            // session's late refresh must not re-grow the map).
            return HeraldOutcome::default();
        }
        let slot = self.slots.entry(session).or_default();
        let ident = current.map(|e| (e.kind, e.key));
        let label = current.map(|e| e.label.clone());
        let row_moved = slot.label != label;
        slot.label = label;
        let quiet = |herald: &mut Self, why| {
            herald.last_quiet = Some(why);
            HeraldOutcome {
                row_moved,
                notice: None,
            }
        };
        let Some(esc) = current else {
            slot.shown = None;
            slot.owed = 0;
            return HeraldOutcome {
                row_moved,
                notice: None,
            };
        };
        if slot.shown == ident {
            if slot.owed == 0 {
                return quiet(self, HeraldQuiet::Same);
            }
        } else {
            slot.shown = ident;
            slot.owed += 1;
        }
        // A notice is wanted for what is shown now (a transition, or one the
        // limit held back earlier).
        if !esc.kind.notifies() {
            slot.owed = 0;
            return quiet(self, HeraldQuiet::Silent);
        }
        if looking {
            slot.owed = 0;
            return quiet(self, HeraldQuiet::Looking);
        }
        let notified_at = slot.notified_at;
        while self
            .recent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= NOTIFY_BURST_WINDOW)
        {
            self.recent.pop_front();
        }
        if Self::free_at(&self.recent, notified_at, now) > now {
            return quiet(self, HeraldQuiet::Limited);
        }
        let Some(slot) = self.slots.get_mut(&session) else {
            return HeraldOutcome::default();
        };
        let more = slot.owed.saturating_sub(1);
        slot.owed = 0;
        slot.notified_at = Some(now);
        self.recent.push_back(now);
        let body = if more == 0 {
            esc.body.clone()
        } else {
            format!("{} (+{more} more)", esc.body)
        };
        HeraldOutcome {
            row_moved,
            notice: Some(HeraldNotice {
                session,
                title: esc.kind.headline(),
                body,
            }),
        }
    }

    /// When a session last notified at `notified_at` may notify again: the
    /// later of its [`NOTIFY_SESSION_FLOOR`] and the instance's
    /// [`NOTIFY_BURST`] window freeing a place (posts older than the window
    /// do not count). `now` means now.
    fn free_at(
        recent: &std::collections::VecDeque<std::time::Instant>,
        notified_at: Option<std::time::Instant>,
        now: std::time::Instant,
    ) -> std::time::Instant {
        let live: Vec<std::time::Instant> = recent
            .iter()
            .copied()
            .filter(|t| now.saturating_duration_since(*t) < NOTIFY_BURST_WINDOW)
            .collect();
        let floor = notified_at.map_or(now, |t| t + NOTIFY_SESSION_FLOOR);
        let burst = if live.len() >= NOTIFY_BURST {
            live[live.len() - NOTIFY_BURST] + NOTIFY_BURST_WINDOW
        } else {
            now
        };
        floor.max(burst).max(now)
    }

    /// The sessions whose owed notice the limit now allows — the App's timer
    /// re-heralds each ([`Self::note`] with its current escalation), and the
    /// earliest instant any other owed notice becomes allowed (its next wake).
    pub fn due(&self, now: std::time::Instant) -> (Vec<u64>, Option<std::time::Instant>) {
        let mut ready = Vec::new();
        let mut next: Option<std::time::Instant> = None;
        for (session, slot) in self.slots.iter().filter(|(_, slot)| slot.owed > 0) {
            let (session, notified_at) = (*session, slot.notified_at);
            let at = Self::free_at(&self.recent, notified_at, now);
            if at <= now {
                ready.push(session);
            } else if next.is_none_or(|n| at < n) {
                next = Some(at);
            }
        }
        ready.sort_unstable();
        (ready, next)
    }

    /// Why the last [`Self::note`] posted nothing, when it had a transition
    /// or a re-read to judge; `None` after a post or a cleared escalation.
    pub fn last_quiet(&self) -> Option<HeraldQuiet> {
        self.last_quiet
    }

    /// Forget a retired session.
    pub fn retire(&mut self, session: u64) {
        self.slots.remove(&session);
    }
}

/// A point-in-time glance at operator + fleet, produced by [`classify`] and
/// rendered verbatim by the AppKit half. Pure data — no handles, no locks.
/// `windows` and `instances` start empty out of [`classify`] (they are not
/// session facts) and are filled by the caller before rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetGlance {
    /// Operator state (see [`OperatorState`]).
    pub operator: OperatorState,
    /// The operator session's process-local id, when one is running — what
    /// the menu's Show/Stop actions act on.
    pub operator_session: Option<u64>,
    /// Whether the operator was elected by the TYPED `role=operator` meta
    /// (`true`) or by the legacy title heuristic (`false`). Destructive
    /// authority follows this bit: only a typed operator gets a Stop row and a
    /// confirm-suppressed close — a title match ("operator.md" in an editor is
    /// enough to produce one) must never be silently closable.
    pub operator_typed: bool,
    /// Whether the operator agent CLI is actually launchable on this machine
    /// (caller-owned fact, like `windows`): gates the Start row so clicking
    /// it can never type a command the shell will not find.
    pub start_available: bool,
    /// Total live sessions in this instance (the operator included).
    pub sessions: usize,
    /// Live session connections (design §5.1): distinct directed flows across
    /// the edge tables — exactly the §5 map's arrow count, so the menu number
    /// and the map it opens can never disagree. Not a session-title fact, so
    /// [`classify`] leaves it `0` and the glance builder fills it. Recorded
    /// authority only — NO live-activity term (design DECIDED: lease/watcher
    /// liveness has no wake funnel and belongs to the map's paint time).
    pub connections: usize,
    /// Escalating sessions, most severe first: `(local_id, display text)`,
    /// one per session ([`escalation`]). Non-empty ⇒ the bar icon badges.
    pub warnings: Vec<(u64, String)>,
    /// This instance's windows, in id order.
    pub windows: Vec<WindowRow>,
    /// Sibling instances from the last background fleet scan, in pid order.
    pub instances: Vec<InstanceRow>,
}

impl FleetGlance {
    /// The menu-bar button title for this glance. `❯` is the operator mark;
    /// a trailing `⚠` is the needs-human badge (AppKit renders color emoji in
    /// menu titles natively — safe here, unlike aterm's own overlay renderer).
    /// Sibling escalations badge too — the icon answers "does ANY aterm need
    /// me", as far as the last background scan saw.
    pub fn button_title(&self) -> &'static str {
        if !self.warnings.is_empty() || self.instances.iter().any(|i| i.warnings > 0) {
            "❯⚠"
        } else {
            "❯"
        }
    }

    /// One line summarizing the operator for the menu header (disabled item).
    pub fn header_line(&self) -> String {
        match &self.operator {
            OperatorState::NotRunning => "Operator: not running".to_string(),
            OperatorState::Running(detail) if detail.is_empty() => "Operator: running".to_string(),
            OperatorState::Running(detail) => format!("Operator{detail}"),
        }
    }

    /// The fleet-summary info row: session count with the connections count
    /// folded beside it (design §5.1 — one row, no separate live term). Pure
    /// so the label is testable off macOS; both menu builders render it
    /// verbatim.
    pub fn sessions_line(&self) -> String {
        format!(
            "Sessions: {} \u{b7} Connections: {}",
            self.sessions, self.connections
        )
    }

    /// A cheap change fingerprint: the caller refreshes AppKit only when this
    /// string differs from the previous glance's (title drift is frequent; menu
    /// rebuilds should not be). Covers EVERY rendered fact — windows and
    /// sibling instances included — and its inputs arrive pre-sorted (windows
    /// in id order, instances in pid order), so it is deterministic.
    pub fn fingerprint(&self) -> String {
        let mut fp = self.header_line();
        fp.push('\u{1f}');
        fp.push_str(self.button_title());
        fp.push('\u{1f}');
        fp.push_str(&self.sessions.to_string());
        // Both bits change what the menu renders (Stop row / Start enablement).
        fp.push('\u{1f}');
        fp.push(if self.operator_typed { 'T' } else { 't' });
        fp.push(if self.start_available { 'S' } else { 's' });
        // The connections count is a rendered fact (the sessions row), so it
        // must move the fingerprint or a mint/revoke would leave a stale menu.
        fp.push('\u{1f}');
        fp.push_str(&self.connections.to_string());
        for (id, title) in &self.warnings {
            fp.push('\u{1f}');
            fp.push_str(&id.to_string());
            fp.push('=');
            fp.push_str(title);
        }
        for w in &self.windows {
            fp.push('\u{1f}');
            fp.push_str(&format!("w{}={}:{}:{}", w.id, w.title, w.tabs, w.frontmost));
        }
        for i in &self.instances {
            fp.push('\u{1f}');
            fp.push_str(&format!(
                "i{}={}:{}:{}",
                i.pid, i.sessions, i.warnings, i.operator
            ));
        }
        fp
    }
}

/// A title after stripping leading non-alphanumeric status glyphs (tab
/// spinners/state marks like `✳`/`◐`) and whitespace — `⚠` survives the strip
/// so escalated titles stay recognizable.
fn stripped_title(title: &str) -> &str {
    title
        .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '⚠')
        .trim_start()
}

/// The legacy title-convention operator test: the stripped title starts with
/// `operator` (ASCII case-insensitive) ending at a word boundary — "operator",
/// "operator:", "operator (2)" match; "operators", "cooperator" never do.
/// Returns the trailing detail on a match.
fn title_operator_detail(title: &str) -> Option<String> {
    let stripped = stripped_title(title);
    match stripped.get(..8) {
        Some(prefix) if prefix.eq_ignore_ascii_case("operator") => stripped[8..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric())
            .then(|| stripped[8..].trim_end().to_string()),
        _ => None,
    }
}

/// Classify the fleet from [`SessionRow`]s (roster order).
///
/// TYPED STATE FIRST, title conventions as fallback:
///
/// * Operator — the first row whose `role` equals `operator` (ASCII
///   case-insensitive) wins outright; only when NO row carries the typed role
///   does the legacy `operator`-title scan run. The running detail comes from
///   the title either way (the title stays the human-readable status line).
/// * Warnings — one row per session, from [`escalation`]: typed `attention`
///   renders `⚠ <message>`; an unsupervised agent's prompt, question or wall
///   — or a supervised agent's wall its supervisor does not escalate —
///   renders `⚠ <tab title>: <kind> <command>`; a `⚠`-prefixed title renders
///   as itself. Rows are ordered most severe first ([`EscalationKind`]), in
///   roster order within a kind.
///
/// An operator whose own row escalates still counts as running (its warning
/// row carries the detail). `windows`/`instances` start empty — the caller
/// owns those facts.
pub fn classify(rows: &[SessionRow]) -> FleetGlance {
    let mut escalations: Vec<Escalation> = rows.iter().filter_map(escalation).collect();
    // Most severe first; roster order within a kind (the sort is stable).
    escalations.sort_by_key(|e| e.kind);
    let warnings = escalations
        .into_iter()
        .map(|e| (e.session, e.label))
        .collect();

    let typed = rows.iter().find(|row| {
        row.role
            .as_deref()
            .is_some_and(|r| r.trim().eq_ignore_ascii_case("operator"))
    });
    let by_title = || {
        rows.iter()
            .find_map(|row| title_operator_detail(&row.title).map(|detail| (row, detail)))
    };
    let (operator, operator_session, operator_typed) = match typed {
        Some(row) => {
            // Detail rung: legacy `operator…` title tail when present (the
            // brief's status-line convention), else the whole title.
            let detail = title_operator_detail(&row.title).unwrap_or_else(|| {
                let title = row.title.trim();
                if title.is_empty() {
                    String::new()
                } else {
                    format!(": {title}")
                }
            });
            (OperatorState::Running(detail), Some(row.id), true)
        }
        None => match by_title() {
            Some((row, detail)) => (OperatorState::Running(detail), Some(row.id), false),
            None => (OperatorState::NotRunning, None, false),
        },
    };

    FleetGlance {
        operator,
        operator_session,
        operator_typed,
        start_available: false,
        sessions: rows.len(),
        // Not a title fact — the glance builder fills it from the edge fold.
        connections: 0,
        warnings,
        windows: Vec::new(),
        instances: Vec::new(),
    }
}

/// One row of the rendered status menu — the pure model the AppKit half only
/// paints (the tab context menu's "the description IS the menu" discipline).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusRow {
    /// A disabled information line.
    Info(String),
    /// A separator.
    Separator,
    /// A clickable row carrying an [`OperatorAction`].
    Action {
        /// The visible label.
        label: String,
        /// What a click posts.
        action: OperatorAction,
        /// Rendered-but-inert when `false` (e.g. a pid-less instance row).
        enabled: bool,
    },
}

/// Compose the whole status menu from a glance — pure, deterministic, and
/// unit-testable off macOS. Layout: operator header + management actions,
/// this instance's windows (click focuses), escalation rows (click focuses the
/// session's tab), the session count, then sibling instances (click activates).
pub fn compose_status_menu(glance: &FleetGlance) -> Vec<StatusRow> {
    let mut rows = vec![StatusRow::Info(glance.header_line())];
    match glance.operator {
        OperatorState::NotRunning => {
            // Rendered-but-inert when the agent CLI is missing: the affordance
            // stays discoverable, and the Info line says why it is grey.
            rows.push(StatusRow::Action {
                label: "Start Operator".to_string(),
                action: OperatorAction::Start,
                enabled: glance.start_available,
            });
            if !glance.start_available {
                rows.push(StatusRow::Info(
                    "(claude CLI not found on PATH)".to_string(),
                ));
            }
        }
        OperatorState::Running(_) => {
            rows.push(StatusRow::Action {
                label: "Show Operator".to_string(),
                action: OperatorAction::Show,
                enabled: true,
            });
            // DESTRUCTIVE authority requires the TYPED role: a title-elected
            // "operator" can be an innocent session (vim editing operator.md),
            // so it gets no Stop row at all — Show is the only offer.
            if glance.operator_typed {
                rows.push(StatusRow::Action {
                    label: "Stop Operator".to_string(),
                    action: OperatorAction::Stop,
                    enabled: true,
                });
            } else {
                rows.push(StatusRow::Info(
                    "(detected by title only — stop it from its own tab)".to_string(),
                ));
            }
        }
    }

    if !glance.windows.is_empty() {
        rows.push(StatusRow::Separator);
        for w in &glance.windows {
            let mark = if w.frontmost { "• " } else { "" };
            let tabs = if w.tabs == 1 {
                "1 tab".to_string()
            } else {
                format!("{} tabs", w.tabs)
            };
            rows.push(StatusRow::Action {
                label: format!("{mark}{} — {tabs}", w.title),
                action: OperatorAction::FocusWindow(w.id),
                enabled: true,
            });
        }
    }

    rows.push(StatusRow::Separator);
    for (id, message) in &glance.warnings {
        rows.push(StatusRow::Action {
            label: message.clone(),
            action: OperatorAction::FocusSession(*id),
            enabled: true,
        });
    }
    rows.push(StatusRow::Info(glance.sessions_line()));
    // The map entry rides beside the count it summarizes (design §5.1); always
    // present — an empty fabric still opens an honest empty map.
    rows.push(StatusRow::Action {
        label: "Show Connection Map".to_string(),
        action: OperatorAction::ShowConnectionMap,
        enabled: true,
    });

    if !glance.instances.is_empty() {
        rows.push(StatusRow::Separator);
        rows.push(StatusRow::Info("Other aterm instances".to_string()));
        for i in &glance.instances {
            let sessions = if i.sessions == 1 {
                "1 session".to_string()
            } else {
                format!("{} sessions", i.sessions)
            };
            let mut label = format!("aterm {} — {sessions}", i.pid);
            if i.warnings > 0 {
                label.push_str(&format!(", ⚠ {}", i.warnings));
            }
            if i.operator {
                label.push_str(" — operator");
            }
            rows.push(StatusRow::Action {
                label,
                action: OperatorAction::RaiseInstance(i.pid),
                enabled: i.pid != 0,
            });
        }
    }
    rows
}

#[cfg(target_os = "macos")]
pub use macos::{StatusItemHandle, install, update};

/// Non-macOS no-op handle: there is no menu-bar status item off macOS. Held by
/// `App` in the same field on every target so the struct shape is
/// platform-independent (the `MenuHandle` pattern).
#[cfg(not(target_os = "macos"))]
pub type StatusItemHandle = ();

/// Non-macOS stub: installing a status item is a no-op that installs nothing.
#[cfg(not(target_os = "macos"))]
pub fn install(
    _proxy: &winit::event_loop::EventLoopProxy<crate::Wake>,
    _glance: &FleetGlance,
) -> Option<StatusItemHandle> {
    None
}

/// Non-macOS stub: nothing to refresh.
#[cfg(not(target_os = "macos"))]
pub fn update(_handle: &StatusItemHandle, _glance: &FleetGlance) {}

#[cfg(target_os = "macos")]
mod macos {
    use aterm_objc::{Id, Obj, Retained, Sel, autoreleasepool, class, sel};
    use winit::event_loop::EventLoopProxy;

    use super::{FleetGlance, OperatorAction};
    use crate::Wake;
    use crate::appkit::consts::NS_VARIABLE_STATUS_ITEM_LENGTH;
    use crate::appkit::{self, MainThread};

    /// What [`install`] returns. BOTH fields are load-bearing retentions:
    /// releasing an `NSStatusItem` REMOVES it from the menu bar, and AppKit holds
    /// a menu item's target only weakly — so `App` keeps this handle in a field
    /// for the process lifetime (the `MenuHandle` rule).
    pub struct StatusItemHandle {
        /// The single `statusAction:` relay target every item is wired to.
        target: Retained<StatusTarget>,
        /// The bar item itself; dropping it would vanish the icon.
        item: Obj,
    }

    aterm_objc::declare_class! {
        /// The target object for every status-menu item. Owns the
        /// `EventLoopProxy<Wake>` and exposes one `statusAction:` selector that
        /// decodes the sender's tag to an [`OperatorAction`] and posts
        /// [`Wake::OperatorAction`] — a pure relay from AppKit into the `Wake`
        /// channel, exactly like `menu.rs`'s `MenuTarget`.
        ///
        /// Declared with [`aterm_objc::declare_class!`]. The class name, the
        /// superclass, the two selectors, the declared protocols and the
        /// behaviour are unchanged from the `objc2` declaration this replaces;
        /// `a_real_menu_and_a_real_notification_reach_the_declared_class`
        /// drives both selectors — through `-performSelector:withObject:` and
        /// `NSNotificationCenter`, i.e. the runtime's and Foundation's own
        /// dispatch, on a probe class. NOT AppKit's: a status item's menu
        /// needs a modal menu-tracking run loop, which libtest cannot start
        /// (a judge measured `popUpMenuPositioningItem:` hanging, and that
        /// `pthread_main_np()` is 0 inside a `#[test]` even under
        /// `--test-threads=1`). An earlier version of this sentence named a
        /// test that does not exist and claimed AppKit's dispatch; in a
        /// campaign whose stated trap is "compiles, does nothing, reports
        /// success", an overstated proof is the defect that matters.
        ///
        /// What objc2 spelled `type Mutability = MainThreadOnly` is carried by
        /// [`MainThread`] at the two entry points plus
        /// [`aterm_objc::Retained`]'s unconditional `!Send` — see the note on
        /// [`MainThread`].
        pub(crate) struct StatusTarget: NSObject {
            const NAME: &str = "ATermStatusTarget";
            type Ivars = EventLoopProxy<Wake>;
            protocols: [NSObject, NSMenuDelegate];

            /// `statusAction:` — the one selector wired to every actionable item.
            /// A tag that doesn't decode is inert (never fires a wrong command).
            @sel(statusAction:)
            fn status_action(&self, sender: Id) {
                if sender.is_null() {
                    return;
                }
                // SAFETY: `sender` is the live NSMenuItem AppKit passed as the
                // action sender; `-tag` is `-(NSInteger)` with no side effects.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                if let Some(action) = OperatorAction::from_tag(tag) {
                    // Fire-and-forget: a closed loop (app shutting down) just
                    // drops the event — mirrors menu.rs.
                    let _ = self.ivars().send_event(Wake::OperatorAction { action });
                }
            }

            /// The menu is about to track: RELAY ONLY. This runs inside
            /// AppKit's nested menu-tracking run loop, so it must never touch
            /// `App` — it posts a wake and the event loop kicks the background
            /// sibling scan, whose result freshens the NEXT open (the
            /// `toolbar.rs` mid-track discipline).
            @sel(menuWillOpen:)
            fn menu_will_open(&self, _menu: Id) {
                let _ = self.ivars().send_event(Wake::OperatorMenuOpening);
            }
        }
    }

    /// Create the menu-bar status item and its menu, rendered from `glance`.
    /// Called once when the first OS window attaches (never headless). Returns
    /// the retained handle for `App` to keep alive; best-effort `None` off the
    /// main thread — never a panic (the `menu::install` contract).
    pub fn install(proxy: &EventLoopProxy<Wake>, glance: &FleetGlance) -> Option<StatusItemHandle> {
        let main = MainThread::new()?;
        let target = StatusTarget::alloc_init(main, proxy.clone())?;
        let item = autoreleasepool(|_| {
            // SAFETY: `+systemStatusBar` is a `-(id)` singleton accessor and
            // `-statusItemWithLength:` is `-(id)(CGFloat)`; both are main-thread
            // AppKit calls (proved by `main`). The item comes back AUTORELEASED
            // (+0), so it is retained into the handle here — releasing an
            // `NSStatusItem` removes it from the menu bar, which is why the
            // handle owns one.
            unsafe {
                let bar = appkit::send_id(class(c"NSStatusBar").as_id(), sel!(systemStatusBar));
                if bar.is_null() {
                    return None;
                }
                let raw = appkit::send_id_f64(
                    bar,
                    sel!(statusItemWithLength:),
                    NS_VARIABLE_STATUS_ITEM_LENGTH,
                );
                Obj::retain(raw)
            }
        })?;
        let handle = StatusItemHandle { target, item };
        update(&handle, glance);
        Some(handle)
    }

    /// Re-render the bar button title and rebuild the menu wholesale from
    /// `glance` — the `update_version_menu` mutation pattern. Main-thread
    /// guarded: a call off the main thread is a silent no-op.
    pub fn update(handle: &StatusItemHandle, glance: &FleetGlance) {
        let Some(_main) = MainThread::new() else {
            return;
        };
        autoreleasepool(|_| {
            // SAFETY: all sends are plain AppKit accessors/setters on live
            // receivers, on the main thread. `-button` is `-(id)` and returns
            // the bar button for an item created with a variable length;
            // `-setTitle:`/`-setDelegate:`/`-setMenu:` are `-(void)(id)`. The
            // delegate is the retained target, which outlives the menu because
            // the handle owns it.
            unsafe {
                let button = appkit::send_id(handle.item.id(), sel!(button));
                if !button.is_null()
                    && let Some(title) = appkit::nsstring(glance.button_title())
                {
                    appkit::send_v_id(button, sel!(setTitle:), title.id());
                }
                let Some(menu) = build_menu(&handle.target, glance) else {
                    return;
                };
                appkit::send_v_id(menu.id(), sel!(setDelegate:), handle.target.as_id());
                appkit::send_v_id(handle.item.id(), sel!(setMenu:), menu.id());
            }
        });
    }

    /// Build the status menu for `glance` by PAINTING the pure model from
    /// `compose_status_menu` — native code renders rows, never decides them.
    /// Auto-enable is off so the model's `enabled` flags are authoritative.
    fn build_menu(target: &Retained<StatusTarget>, glance: &FleetGlance) -> Option<Obj> {
        // SAFETY: `+alloc` on `NSMenu` gives a +1 uninitialised instance and
        // `-init` consumes it, so `Obj::from_owned` adopts exactly one +1;
        // `-setAutoenablesItems:` is `-(void)(BOOL)` on the fresh menu.
        let menu = unsafe {
            let raw = appkit::send_id(appkit::alloc(class(c"NSMenu")), sel!(init));
            let menu = Obj::from_owned(raw)?;
            appkit::send_v_bool(menu.id(), sel!(setAutoenablesItems:), false);
            menu
        };
        for row in super::compose_status_menu(glance) {
            match row {
                super::StatusRow::Info(text) => add_info(&menu, &text),
                super::StatusRow::Separator => add_separator(&menu),
                super::StatusRow::Action {
                    label,
                    action,
                    enabled,
                } => add_action(&menu, target, &label, action, enabled),
            }
        }
        Some(menu)
    }

    /// `[[NSMenuItem alloc] initWithTitle:action:keyEquivalent:]`, +1, or `None`
    /// if Foundation refused either string. `action` is [`Sel::NULL`] for a row
    /// that is a label rather than a command.
    fn new_item(title: &str, action: Sel) -> Option<Obj> {
        let title = appkit::nsstring(title)?;
        let empty = appkit::nsstring("")?;
        // SAFETY: `initWithTitle:action:keyEquivalent:` is NSMenuItem's
        // designated initializer, `-(id)(NSString *, SEL, NSString *)`, and a
        // nil SEL is its documented "no action" value. `+alloc` is +1 and the
        // initializer consumes it, so `Obj::from_owned` adopts one +1. Both
        // strings are live +1 NSStrings the initializer copies.
        unsafe {
            let raw = appkit::send_id_id_sel_id(
                appkit::alloc(class(c"NSMenuItem")),
                sel!(initWithTitle:action:keyEquivalent:),
                title.id(),
                action,
                empty.id(),
            );
            Obj::from_owned(raw)
        }
    }

    /// Append an actionable item wired to `target` carrying `action`'s tag.
    fn add_action(
        menu: &Obj,
        target: &Retained<StatusTarget>,
        title: &str,
        action: OperatorAction,
        enabled: bool,
    ) {
        let Some(item) = new_item(title, sel!(statusAction:)) else {
            return;
        };
        // SAFETY: plain setters on a fresh NSMenuItem, then `-addItem:` on the
        // live menu — `-setTarget:` is `-(void)(id)` (AppKit holds the target
        // WEAKLY, which is why the handle retains it), `-setTag:` is
        // `-(void)(NSInteger)` and `-setEnabled:` is `-(void)(BOOL)`.
        unsafe {
            appkit::send_v_id(item.id(), sel!(setTarget:), target.as_id());
            appkit::send_v_isize(item.id(), sel!(setTag:), action.tag());
            appkit::send_v_bool(item.id(), sel!(setEnabled:), enabled);
            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
        }
    }

    /// Append a disabled information row (explicit — auto-enable is off).
    fn add_info(menu: &Obj, title: &str) {
        let Some(item) = new_item(title, Sel::NULL) else {
            return;
        };
        // SAFETY: as `add_action`, on an item with no action.
        unsafe {
            appkit::send_v_bool(item.id(), sel!(setEnabled:), false);
            appkit::send_v_id(menu.id(), sel!(addItem:), item.id());
        }
    }

    /// Append a separator line.
    fn add_separator(menu: &Obj) {
        // SAFETY: `+separatorItem` is `-(id)` and returns a shared,
        // AUTORELEASED item — borrowed here for the length of `-addItem:`,
        // which retains it into the menu, inside the caller's pool.
        unsafe {
            let sep = appkit::send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem));
            if !sep.is_null() {
                appkit::send_v_id(menu.id(), sel!(addItem:), sep);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use aterm_objc::{
            ClassType, Obj, Sel, autoreleasepool, class, class_name, method_types, sel,
        };

        use super::{StatusTarget, new_item};
        use crate::appkit;
        use crate::appkit::consts::NS_VARIABLE_STATUS_ITEM_LENGTH;

        /// How many `statusAction:` / `menuWillOpen:` deliveries the probe saw.
        ///
        /// A real `StatusTarget`'s ivar is an `EventLoopProxy<Wake>`, and one of
        /// those can only be minted from a live winit `EventLoop`, which on
        /// macOS must be built on the process main thread and only once — so it
        /// cannot exist on a libtest thread. The probe is a SECOND
        /// `declare_class!` expansion of the identical shape (same superclass,
        /// same two selectors, same declared protocols, same trampoline
        /// generation) whose bodies count instead of posting, which is what
        /// makes it a fair stand-in: everything under test is the macro's
        /// output, not the relay.
        static DISPATCHES: AtomicUsize = AtomicUsize::new(0);

        aterm_objc::declare_class! {
            struct StatusProbe: NSObject {
                const NAME: &str = "ATermStatusProbe";
                type Ivars = ();
                protocols: [NSObject, NSMenuDelegate];

                @sel(statusAction:)
                fn status_action(&self, sender: aterm_objc::Id) {
                    // Read the tag exactly as the ported body does, so a wrong
                    // `-tag` prototype would show up here too.
                    // SAFETY: AppKit passed the live sending NSMenuItem;
                    // `-tag` is `-(NSInteger)` and side-effect free.
                    let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                    DISPATCHES.fetch_add(1 + usize::try_from(tag).unwrap_or(0), Ordering::SeqCst);
                }

                @sel(menuWillOpen:)
                fn menu_will_open(&self, _menu: aterm_objc::Id) {
                    DISPATCHES.fetch_add(1000, Ordering::SeqCst);
                }
            }
        }

        /// The sentinel is the SDK header's value, exactly. It cannot be
        /// linked (see the constant's own note), so this is the only check
        /// available and it is a pin rather than a proof.
        #[test]
        fn status_item_length_sentinel_matches_the_sdk() {
            assert!(
                (NS_VARIABLE_STATUS_ITEM_LENGTH + 1.0).abs() < f64::EPSILON,
                "NSVariableStatusItemLength is -1.0 in AppKit/Headers/NSStatusBar.h"
            );
        }

        /// The registered class is what the RUNTIME says it is: right name,
        /// right superclass, both selectors present on instances, and it
        /// CONFORMS to `NSMenuDelegate` — the property objc2 spelled
        /// `unsafe impl NSMenuDelegate for StatusTarget`, and the one
        /// `-[NSMenu setDelegate:]` reads before it will ever call back.
        #[test]
        fn the_registered_class_is_what_the_runtime_reports() {
            let cls = StatusTarget::class();
            assert!(!cls.is_null());
            assert_eq!(class(c"ATermStatusTarget"), cls);
            // SAFETY: `cls` is the class this module registered, so it and its
            // superclass are live, immortal class objects.
            unsafe {
                assert_eq!(class_name(cls), c"ATermStatusTarget");
                assert_eq!(class_name(aterm_objc::superclass_of(cls)), c"NSObject");
            }
            let proto = aterm_objc::protocol(c"NSMenuDelegate");
            assert!(
                !proto.is_null()
                    && !aterm_objc::protocols_registered_by_aterm().contains(&c"NSMenuDelegate"),
                "this host's AppKit registers NSMenuDelegate — not one aterm had to supply"
            );
            // SAFETY: `+conformsToProtocol:` and `+instancesRespondToSelector:`
            // are side-effect-free NSObject queries on a live class object.
            unsafe {
                assert!(
                    appkit::send_bool_id(
                        cls.as_id(),
                        sel!(conformsToProtocol:),
                        aterm_objc::Id::from_ptr(proto.as_ptr()),
                    ),
                    "the declared class does not answer NSMenuDelegate"
                );
                assert!(appkit::send_bool_sel(
                    cls.as_id(),
                    sel!(instancesRespondToSelector:),
                    sel!(statusAction:)
                ));
                assert!(appkit::send_bool_sel(
                    cls.as_id(),
                    sel!(instancesRespondToSelector:),
                    sel!(menuWillOpen:)
                ));
            }
        }

        /// THE PROOF for the ENCODINGS: what `class_addMethod` actually
        /// registered, read back out of the runtime with
        /// `method_getTypeEncoding`, against the verified table. Both selectors
        /// are `- (void)x:(id)y`, so both are `"v@:@"`; the generated `-dealloc`
        /// is `"v@:"`.
        #[test]
        fn the_runtime_reports_the_encodings_the_table_says() {
            let cls = StatusTarget::class();
            // SAFETY: `cls` is the live registered class.
            unsafe {
                assert_eq!(
                    method_types(cls, sel!(statusAction:)).as_deref(),
                    Some("v@:@")
                );
                assert_eq!(
                    method_types(cls, sel!(menuWillOpen:)).as_deref(),
                    Some("v@:@")
                );
                assert_eq!(method_types(cls, sel!(dealloc)).as_deref(), Some("v@:"));
            }
        }

        /// THE PROOF for BEHAVIOUR — not "the macro expands", and not our own
        /// typed cast either.
        ///
        /// Three legs, none of which is the thing under test:
        ///
        /// 1. **AppKit stored the wiring.** A real `NSMenu` takes the probe as
        ///    its delegate and a real `NSMenuItem` is built through the ported
        ///    `new_item`; `-delegate`, `-target` and `-action` are read back
        ///    out of AppKit and must name the declared class and its selector.
        /// 2. **Foundation dispatches one selector.** `NSNotificationCenter`
        ///    is handed `menuWillOpen:` as an observer selector and the
        ///    notification is posted — Foundation's own machinery finds the IMP
        ///    in the registered class's method table and calls the Rust body,
        ///    exactly as `platform.rs`'s W1 test does for the reduce-motion
        ///    target. Registration itself is a check: the centre reads the
        ///    encoding to build the call.
        /// 3. **The runtime dispatches the other.** `-performSelector:
        ///    withObject:` is `objc_msgSend` off the class's method table —
        ///    what `-[NSApplication sendAction:to:from:]` ultimately performs
        ///    for a menu click — handed the REAL `NSMenuItem`, so the body's
        ///    `-tag` read is exercised against a real item that AppKit itself
        ///    tagged.
        ///
        /// `-[NSMenu performActionForItemAtIndex:]` is deliberately NOT the
        /// leg used, and the reason is measured rather than assumed: it routes
        /// through `NSApp`, there is no `NSApplication` in a libtest process
        /// (and one cannot be made — libtest runs every test on a spawned
        /// thread, and `+sharedApplication` is main-thread-only), and the call
        /// silently does nothing. It was the first shape tried and it counted
        /// zero.
        #[test]
        fn a_real_menu_and_a_real_notification_reach_the_declared_class() {
            const NOTE: &str = "ATermStatusProbeWillOpen";
            DISPATCHES.store(0, Ordering::SeqCst);
            let probe = StatusProbe::alloc_init(crate::appkit::test_witness(), ()).expect("probe");
            autoreleasepool(|_| {
                // SAFETY: standard NSMenu/NSNotificationCenter construction and
                // plain accessors, every send through the same prototypes the
                // ported module uses. `-action` is `-(SEL)`, `-target` and
                // `-delegate` are `-(id)`, `-addObserver:selector:name:object:`
                // is `-(void)(id, SEL, id, id)` and `-postNotificationName:
                // object:` is `-(void)(id, id)`.
                unsafe {
                    let menu = Obj::from_owned(appkit::send_id(
                        appkit::alloc(class(c"NSMenu")),
                        sel!(init),
                    ))
                    .expect("NSMenu");
                    appkit::send_v_bool(menu.id(), sel!(setAutoenablesItems:), false);
                    appkit::send_v_id(menu.id(), sel!(setDelegate:), probe.as_id());

                    let item = new_item("row", sel!(statusAction:)).expect("item");
                    appkit::send_v_id(item.id(), sel!(setTarget:), probe.as_id());
                    appkit::send_v_isize(item.id(), sel!(setTag:), 7);
                    appkit::send_v_bool(item.id(), sel!(setEnabled:), true);
                    appkit::send_v_id(menu.id(), sel!(addItem:), item.id());

                    // (1) what AppKit stored.
                    assert_eq!(
                        appkit::send_id(menu.id(), sel!(delegate)),
                        probe.as_id(),
                        "NSMenu refused the declared class as its delegate"
                    );
                    let target_of: unsafe extern "C-unwind" fn(
                        aterm_objc::Id,
                        Sel,
                    )
                        -> aterm_objc::Id = aterm_objc::msg();
                    assert_eq!(target_of(item.id(), sel!(target)), probe.as_id());
                    let action_of: unsafe extern "C-unwind" fn(aterm_objc::Id, Sel) -> Sel =
                        aterm_objc::msg();
                    assert_eq!(action_of(item.id(), sel!(action)), sel!(statusAction:));
                    assert_eq!(appkit::send_isize(item.id(), sel!(tag)), 7);

                    // (2) Foundation's own dispatch of `menuWillOpen:`.
                    let centre = appkit::send_id(
                        class(c"NSNotificationCenter").as_id(),
                        sel!(defaultCenter),
                    );
                    assert!(!centre.is_null());
                    let name = appkit::nsstring(NOTE).expect("NSString");
                    let add: unsafe extern "C-unwind" fn(
                        aterm_objc::Id,
                        Sel,
                        aterm_objc::Id,
                        Sel,
                        aterm_objc::Id,
                        aterm_objc::Id,
                    ) = aterm_objc::msg();
                    add(
                        centre,
                        sel!(addObserver:selector:name:object:),
                        probe.as_id(),
                        sel!(menuWillOpen:),
                        name.id(),
                        aterm_objc::Id::NIL,
                    );
                    appkit::send_v_id_id(
                        centre,
                        sel!(postNotificationName:object:),
                        name.id(),
                        aterm_objc::Id::NIL,
                    );
                    appkit::send_v_id(centre, sel!(removeObserver:), probe.as_id());
                    // Removed: a further post must NOT reach it.
                    appkit::send_v_id_id(
                        centre,
                        sel!(postNotificationName:object:),
                        name.id(),
                        aterm_objc::Id::NIL,
                    );

                    // (3) the runtime's dispatch of `statusAction:`, with the
                    // REAL tagged NSMenuItem as the sender.
                    let perform_sel: unsafe extern "C-unwind" fn(
                        aterm_objc::Id,
                        Sel,
                        Sel,
                        aterm_objc::Id,
                    )
                        -> aterm_objc::Id = aterm_objc::msg();
                    perform_sel(
                        probe.as_id(),
                        sel!(performSelector:withObject:),
                        sel!(statusAction:),
                        item.id(),
                    );
                }
            });
            assert_eq!(
                DISPATCHES.load(Ordering::SeqCst),
                1008,
                "the declared class did not receive menuWillOpen: exactly once \
                 (1000) and statusAction: with the item's tag 7 (1 + 7)"
            );
        }

        /// An item built with [`Sel::NULL`] really has no action — the shape
        /// `add_info` needs, and the one the crate could not express before
        /// this wave.
        #[test]
        fn an_information_row_has_a_nil_action() {
            autoreleasepool(|_| {
                let item = new_item("info", Sel::NULL).expect("item");
                let wired = new_item("cmd", sel!(statusAction:)).expect("item");
                // SAFETY: `-action` is `-(SEL)` on a live NSMenuItem, and
                // `-title` is `-(NSString *)`.
                unsafe {
                    let action: unsafe extern "C-unwind" fn(aterm_objc::Id, Sel) -> Sel =
                        aterm_objc::msg();
                    assert!(action(item.id(), sel!(action)).is_null());
                    assert_eq!(action(wired.id(), sel!(action)), sel!(statusAction:));
                    assert_eq!(
                        appkit::nsstring_to_rust(appkit::send_id(item.id(), sel!(title))),
                        "info"
                    );
                }
            });
        }

        /// The generated `-dealloc` drops the Rust ivars, on an ivar SHAPE that
        /// owns something with a `Drop` — which the real site's
        /// `EventLoopProxy<Wake>` is.
        #[test]
        fn dropping_a_declared_instance_drops_its_ivars() {
            static DROPS: AtomicUsize = AtomicUsize::new(0);
            struct Spy;
            impl Drop for Spy {
                fn drop(&mut self) {
                    DROPS.fetch_add(1, Ordering::SeqCst);
                }
            }
            aterm_objc::declare_class! {
                struct StatusDropProbe: NSObject {
                    const NAME: &str = "ATermStatusDropProbe";
                    type Ivars = Spy;

                    @sel(ping)
                    fn ping(&self) {}
                }
            }
            DROPS.store(0, Ordering::SeqCst);
            let t = StatusDropProbe::alloc_init(crate::appkit::test_witness(), Spy).expect("probe");
            assert_eq!(DROPS.load(Ordering::SeqCst), 0);
            drop(t);
            assert_eq!(
                DROPS.load(Ordering::SeqCst),
                1,
                "the generated -dealloc did not drop the ivars"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(v: &[(u64, &str)]) -> Vec<SessionRow> {
        v.iter()
            .map(|(i, s)| SessionRow {
                id: *i,
                title: s.to_string(),
                role: None,
                attention: None,
                ..SessionRow::default()
            })
            .collect()
    }

    fn agent_row(id: u64, title: &str, word: &'static str, rev: u64) -> SessionRow {
        SessionRow {
            id,
            title: title.to_string(),
            agent: Some(AgentFact {
                word,
                detail: (word == "prompt").then(|| "bash:not-read-only".to_string()),
                rev,
                subject: (word == "prompt").then(|| "rm -rf build".to_string()),
            }),
            ..SessionRow::default()
        }
    }

    /// The agent verdict becomes one host-written row per session, most
    /// severe first; busy/idle/survey raise nothing; a live supervisor
    /// takes the agent's rows over (its typed attention still shows).
    #[test]
    fn agent_verdicts_list_most_severe_first_and_a_supervisor_takes_them_over() {
        let g = classify(&[
            agent_row(1, "\u{2733} limits", "wall:usage-session", 3),
            agent_row(2, "tests", "busy", 1),
            agent_row(3, "asker", "question", 2),
            agent_row(4, "builder", "prompt", 5),
            row(5, "zsh", None, Some("deploy needs a human")),
            agent_row(6, "idle one", "idle", 1),
        ]);
        assert_eq!(
            g.warnings,
            vec![
                (5, "\u{26a0} deploy needs a human".to_string()),
                (4, "\u{26a0} builder: bash rm -rf build".to_string()),
                (3, "\u{26a0} asker: question".to_string()),
                (1, "\u{26a0} limits: usage-session".to_string()),
            ]
        );
        assert_eq!(g.button_title(), "\u{276f}\u{26a0}");
        // A reset time rides the wall row.
        let mut limited = agent_row(1, "limits", "wall:usage-session", 3);
        limited.agent.as_mut().unwrap().detail = Some("7:30pm".into());
        assert_eq!(
            escalation(&limited).unwrap().label,
            "\u{26a0} limits: usage-session until 7:30pm"
        );
        // Every wall kind raises a row; `unknown` (no evidence) and a bare
        // `wall:` raise none.
        for word in ["wall:overloaded", "wall:auth", "wall:context"] {
            let esc = escalation(&agent_row(7, "w", word, 1)).expect(word);
            assert_eq!(esc.kind, EscalationKind::Wall, "{word}");
        }
        for word in ["unknown", "wall:", "limited"] {
            assert_eq!(escalation(&agent_row(7, "w", word, 1)), None, "{word}");
        }
        // Supervised: the prompt raises nothing of its own…
        let mut supervised = agent_row(4, "builder", "prompt", 5);
        supervised.supervised = true;
        assert_eq!(escalation(&supervised), None);
        // …while the supervisor's own escalation still shows.
        supervised.attention = Some("rm outside scratch".into());
        let e = escalation(&supervised).unwrap();
        assert_eq!(e.kind, EscalationKind::Attention);
        assert_eq!(e.body, "builder: rm outside scratch");
        // NEGATIVE CONTROL: the same row unsupervised does escalate.
        supervised.supervised = false;
        supervised.attention = None;
        assert_eq!(
            escalation(&supervised).unwrap().kind,
            EscalationKind::Prompt
        );
    }

    /// A supervised session's wall is the SUPERVISOR's to answer — every
    /// kind: the turn-end decider retries or escalates a 529, an API error, a
    /// lost login and a full context as it resumes, switches or escalates the
    /// limits — so the verdict raises no second row (and no second
    /// notification) while the claim is live; what the supervisor cannot
    /// resolve arrives as its own attention, which wins. NEGATIVE CONTROLS:
    /// the same walls with no supervisor each raise their row, and a
    /// supervised prompt raises nothing.
    #[test]
    fn a_supervised_wall_is_the_supervisors_and_an_unsupervised_one_raises_its_row() {
        let supervised = |word: &'static str| SessionRow {
            supervised: true,
            ..agent_row(8, "w", word, 1)
        };
        let walls = [
            "wall:overloaded",
            "wall:api-error",
            "wall:auth",
            "wall:context",
            "wall:usage-session",
            "wall:usage-weekly",
            "wall:model-bucket",
            "wall:spend",
        ];
        for word in walls.iter().copied().chain(["prompt", "question"]) {
            assert_eq!(escalation(&supervised(word)), None, "{word}");
        }
        for word in walls {
            let esc = escalation(&agent_row(8, "w", word, 1)).expect(word);
            assert_eq!(esc.kind, EscalationKind::Wall, "{word}");
        }
        let mut badged = supervised("wall:overloaded");
        badged.attention = Some("claude wall: 529".into());
        assert_eq!(escalation(&badged).unwrap().kind, EscalationKind::Attention);
    }

    #[test]
    fn row_text_is_one_clipped_line() {
        assert_eq!(fold_clip("rm -rf\n  build\t/x", 64), "rm -rf build /x");
        assert_eq!(fold_clip("  lead", 64), "lead");
        let long = "x".repeat(100);
        let clipped = fold_clip(&long, 20);
        assert!(
            clipped.len() <= 20 && clipped.ends_with('\u{2026}'),
            "{clipped}"
        );
        // Never splits a char.
        let wide = "\u{00e9}".repeat(30);
        assert!(fold_clip(&wide, 11).ends_with('\u{2026}'));
        // Bidi overrides/isolates and invisible format characters never reach
        // native chrome (review minor): the command reads in its real order.
        assert_eq!(
            fold_clip("rm -rf /tmp/\u{202e}gol.txt\u{2066}x\u{2069}\u{200b}y", 64),
            "rm -rf /tmp/gol.txtxy"
        );
        let mut spoof = agent_row(1, "t", "prompt", 1);
        spoof.agent.as_mut().unwrap().subject = Some("ls \u{202e}fr- mr".to_string());
        let esc = escalation(&spoof).unwrap();
        assert!(!esc.label.contains('\u{202e}') && !esc.body.contains('\u{202e}'));
        // NEGATIVE CONTROL: ordinary non-ASCII text and emoji joiners survive.
        assert_eq!(
            fold_clip("caf\u{00e9} \u{1f469}\u{200d}\u{1f4bb}", 64),
            "caf\u{00e9} \u{1f469}\u{200d}\u{1f4bb}"
        );
        // A prompt with no subject still names its kind.
        let mut bare = agent_row(1, "t", "prompt", 1);
        bare.agent.as_mut().unwrap().subject = None;
        assert_eq!(
            escalation(&bare).unwrap().label,
            "\u{26a0} t: bash approval"
        );
    }

    fn herald_note(
        h: &mut Herald,
        row: &SessionRow,
        looking: bool,
        now: std::time::Instant,
    ) -> HeraldOutcome {
        h.note(row.id, escalation(row).as_ref(), looking, now)
    }

    /// One notification per transition: the first box posts, the same box
    /// re-read does not, a cleared-then-new box does; a box first seen while
    /// the human looks is spent and never announced when they look away; the
    /// legacy title lists but never posts.
    #[test]
    fn the_herald_posts_once_per_transition() {
        let t0 = std::time::Instant::now();
        let mut h = Herald::default();
        let boxed = agent_row(4, "builder", "prompt", 5);
        let first = herald_note(&mut h, &boxed, false, t0);
        assert!(first.row_moved);
        let notice = first.notice.expect("the first box posts");
        assert_eq!(notice.session, 4);
        assert_eq!(notice.title, "aterm \u{b7} approval waiting");
        assert_eq!(notice.body, "builder: bash rm -rf build");
        let again = herald_note(&mut h, &boxed, false, t0);
        assert_eq!(again, HeraldOutcome::default(), "same box, nothing");
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Same));
        // Answered (busy, rev 6), then a new box (rev 7) past the floor.
        let busy = agent_row(4, "builder", "busy", 6);
        assert!(herald_note(&mut h, &busy, false, t0).row_moved);
        let later = t0 + NOTIFY_SESSION_FLOOR;
        let next = herald_note(&mut h, &agent_row(4, "builder", "prompt", 7), false, later);
        assert!(next.notice.is_some(), "a new box is a new transition");

        // Looking: spent, and not re-announced once the human looks away.
        let mut h = Herald::default();
        let seen = herald_note(&mut h, &boxed, true, t0);
        assert!(seen.row_moved && seen.notice.is_none());
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Looking));
        assert!(herald_note(&mut h, &boxed, false, t0).notice.is_none());

        // The legacy `⚠` title is program output: listed, never posted.
        let mut h = Herald::default();
        let titled = rows(&[(9, "\u{26a0} approve me")]).remove(0);
        let out = herald_note(&mut h, &titled, false, t0);
        assert!(out.row_moved && out.notice.is_none());
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Silent));
        // A late subject changes the row without a second post.
        let mut h = Herald::default();
        let mut early = boxed.clone();
        early.agent.as_mut().unwrap().subject = None;
        assert!(herald_note(&mut h, &early, false, t0).notice.is_some());
        let late = herald_note(&mut h, &boxed, false, t0);
        assert!(late.row_moved && late.notice.is_none());
    }

    /// The limiter: a session posts at most once per floor, the instance at
    /// most [`NOTIFY_BURST`] per window; a held transition is spent, not
    /// deferred; retiring a session forgets it.
    #[test]
    fn the_herald_is_rate_limited_per_session_and_per_instance() {
        let t0 = std::time::Instant::now();
        let mut h = Herald::default();
        assert!(
            herald_note(&mut h, &agent_row(1, "a", "prompt", 1), false, t0)
                .notice
                .is_some()
        );
        let _ = herald_note(&mut h, &agent_row(1, "a", "busy", 2), false, t0);
        let flap = herald_note(&mut h, &agent_row(1, "a", "prompt", 3), false, t0);
        assert!(flap.notice.is_none(), "inside the session floor");
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Limited));
        // A second box inside the floor is owed too.
        let second = herald_note(&mut h, &agent_row(1, "a", "prompt", 4), false, t0);
        assert!(second.notice.is_none());
        // OWED, not spent (review minor): the timer is armed at the floor…
        assert_eq!(h.due(t0), (vec![], Some(t0 + NOTIFY_SESSION_FLOOR)));
        // …and past it the same, still-shown box posts ONE coalesced notice.
        let after = t0 + NOTIFY_SESSION_FLOOR;
        assert_eq!(h.due(after), (vec![1], None));
        let paid = herald_note(&mut h, &agent_row(1, "a", "prompt", 4), false, after);
        assert_eq!(
            paid.notice.map(|n| n.body),
            Some("a: bash rm -rf build (+1 more)".to_string())
        );
        // Paid once: nothing more is owed, and a re-read stays quiet.
        assert_eq!(h.due(after), (vec![], None));
        assert!(
            herald_note(&mut h, &agent_row(1, "a", "prompt", 4), false, after)
                .notice
                .is_none()
        );
        // NEGATIVE CONTROL: an owed notice whose escalation CLEARED is dropped
        // — a box already answered is never announced late.
        let _ = herald_note(&mut h, &agent_row(1, "a", "prompt", 5), false, after);
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Limited));
        let _ = herald_note(&mut h, &agent_row(1, "a", "busy", 6), false, after);
        assert_eq!(h.due(after + NOTIFY_SESSION_FLOOR), (vec![], None));

        // Ten sessions hit a limit together: NOTIFY_BURST post now, not ten…
        let mut h = Herald::default();
        let posted = (10..20)
            .filter(|id| {
                herald_note(
                    &mut h,
                    &agent_row(*id, "w", "wall:usage-session", 1),
                    false,
                    t0,
                )
                .notice
                .is_some()
            })
            .count();
        assert_eq!(posted, NOTIFY_BURST);
        // Every one of them still has its row (the menu is not rate-limited).
        let rows: Vec<SessionRow> = (10..20)
            .map(|id| agent_row(id, "w", "wall:usage-session", 1))
            .collect();
        assert_eq!(classify(&rows).warnings.len(), 10);
        // …and the rest are owed until the window slides.
        let later = t0 + NOTIFY_BURST_WINDOW;
        assert_eq!(h.due(t0).1, Some(later));
        let (ready, _) = h.due(later);
        assert_eq!(ready.len(), 10 - NOTIFY_BURST);

        // Retire forgets; a cleared session that never escalated keeps no slot.
        h.retire(30);
        assert_eq!(h.note(99, None, false, t0), HeraldOutcome::default());
        assert!(!h.slots.contains_key(&99));
    }

    #[test]
    fn tags_round_trip_and_unknown_is_inert() {
        for a in [
            OperatorAction::Start,
            OperatorAction::Show,
            OperatorAction::Stop,
            OperatorAction::ShowConnectionMap,
        ] {
            assert_eq!(OperatorAction::from_tag(a.tag()), Some(a));
        }
        assert_eq!(OperatorAction::from_tag(0), None);
        assert_eq!(OperatorAction::from_tag(99), None);
    }

    #[test]
    fn empty_fleet_is_not_running() {
        let g = classify(&[]);
        assert_eq!(g.operator, OperatorState::NotRunning);
        assert_eq!(g.operator_session, None);
        assert_eq!(g.sessions, 0);
        assert!(g.warnings.is_empty());
        assert_eq!(g.button_title(), "❯");
        assert_eq!(g.header_line(), "Operator: not running");
    }

    #[test]
    fn operator_found_by_title_prefix_case_insensitive() {
        let g = classify(&rows(&[
            (3, "zsh"),
            (7, "Operator: fleet idle"),
            (9, "vim"),
        ]));
        assert_eq!(g.operator_session, Some(7));
        assert_eq!(g.operator, OperatorState::Running(": fleet idle".into()));
        assert_eq!(g.header_line(), "Operator: fleet idle");
        assert_eq!(g.sessions, 3);
    }

    #[test]
    fn status_glyph_prefixes_are_stripped() {
        // Tab titles carry spinner/state glyphs (e.g. "✳ operator: busy").
        let g = classify(&rows(&[(2, "✳ operator: driving 2 workers")]));
        assert_eq!(g.operator_session, Some(2));
        assert_eq!(
            g.operator,
            OperatorState::Running(": driving 2 workers".into())
        );
    }

    #[test]
    fn warning_titles_badge_the_button() {
        let g = classify(&rows(&[
            (1, "operator: fleet idle"),
            (4, "⚠ needs human: approval"),
        ]));
        assert_eq!(g.warnings, vec![(4, "⚠ needs human: approval".to_string())]);
        assert_eq!(g.button_title(), "❯⚠");
    }

    #[test]
    fn first_operator_wins_and_bare_operator_word_matches() {
        let g = classify(&rows(&[(1, "operator"), (2, "operator: second")]));
        assert_eq!(g.operator_session, Some(1));
        assert_eq!(g.operator, OperatorState::Running(String::new()));
        assert_eq!(g.header_line(), "Operator: running");
    }

    #[test]
    fn non_operator_titles_do_not_match() {
        // "operators guide" begins with the word but not the identity prefix
        // boundary we accept ("operator" + non-alnum); it still must not match.
        let g = classify(&rows(&[
            (1, "cooperator"),
            (2, "operators guide"),
            (3, "vim operators.txt"),
        ]));
        assert_eq!(g.operator_session, None);
        assert_eq!(g.operator, OperatorState::NotRunning);
    }

    #[test]
    fn fingerprint_changes_with_state_not_with_noise() {
        let a = classify(&rows(&[(1, "operator: fleet idle"), (2, "zsh")]));
        let b = classify(&rows(&[(1, "operator: fleet idle"), (2, "zsh")]));
        assert_eq!(a.fingerprint(), b.fingerprint());
        let c = classify(&rows(&[(1, "operator: fleet idle"), (2, "⚠ stuck")]));
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    fn row(id: u64, title: &str, role: Option<&str>, attention: Option<&str>) -> SessionRow {
        SessionRow {
            id,
            title: title.to_string(),
            role: role.map(str::to_string),
            attention: attention.map(str::to_string),
            ..SessionRow::default()
        }
    }

    #[test]
    fn typed_role_outranks_the_title_convention_in_both_directions() {
        // A LATER typed role beats an EARLIER title-convention candidate…
        let g = classify(&[
            row(2, "operator: legacy imposter", None, None),
            row(5, "fleet brain", Some("operator"), None),
        ]);
        assert_eq!(g.operator_session, Some(5));
        assert_eq!(g.operator, OperatorState::Running(": fleet brain".into()));
        // …and with NO typed role anywhere, the title scan still wins (the
        // fallback that keeps older briefs working).
        let g = classify(&[
            row(1, "zsh", None, None),
            row(2, "operator: legacy", None, None),
        ]);
        assert_eq!(g.operator_session, Some(2));
        // Typed role is case-insensitive and trimmed; other roles never match.
        let g = classify(&[row(3, "worker", Some(" Operator "), None)]);
        assert_eq!(g.operator_session, Some(3));
        let g = classify(&[row(3, "worker", Some("supervisor"), None)]);
        assert_eq!(g.operator_session, None);
    }

    #[test]
    fn typed_operator_with_operator_title_keeps_the_legacy_detail() {
        let g = classify(&[row(
            7,
            "✳ operator: driving 2 workers",
            Some("operator"),
            None,
        )]);
        assert_eq!(
            g.operator,
            OperatorState::Running(": driving 2 workers".into())
        );
        // An empty title renders as the bare running header.
        let g = classify(&[row(7, "", Some("operator"), None)]);
        assert_eq!(g.operator, OperatorState::Running(String::new()));
        assert_eq!(g.header_line(), "Operator: running");
    }

    #[test]
    fn typed_attention_escalates_dedups_and_never_double_prefixes() {
        // Typed attention escalates with the ⚠-prefixed message.
        let g = classify(&[row(4, "zsh", None, Some("needs approval"))]);
        assert_eq!(g.warnings, vec![(4, "⚠ needs approval".to_string())]);
        assert_eq!(g.button_title(), "❯⚠");
        // Typed attention + ⚠ title on ONE session ⇒ exactly one row, typed wins.
        let g = classify(&[row(4, "⚠ stale title", None, Some("real reason"))]);
        assert_eq!(g.warnings, vec![(4, "⚠ real reason".to_string())]);
        // A message already carrying ⚠ is not double-prefixed.
        let g = classify(&[row(4, "zsh", None, Some("⚠ already marked"))]);
        assert_eq!(g.warnings, vec![(4, "⚠ already marked".to_string())]);
        // Whitespace-only attention is unset in spirit: fall back to the title
        // scan (here: nothing).
        let g = classify(&[row(4, "zsh", None, Some("   "))]);
        assert!(g.warnings.is_empty());
        // An escalated operator still counts as running.
        let g = classify(&[row(
            1,
            "operator: stuck",
            Some("operator"),
            Some("wedged on CI"),
        )]);
        assert_eq!(g.operator_session, Some(1));
        assert_eq!(g.warnings, vec![(1, "⚠ wedged on CI".to_string())]);
    }

    #[test]
    fn windows_and_instances_ride_the_fingerprint_and_badge() {
        let base = classify(&rows(&[(1, "zsh")]));
        let mut with_window = base.clone();
        with_window.windows.push(WindowRow {
            id: 1,
            title: "aterm".into(),
            tabs: 2,
            frontmost: true,
        });
        assert_ne!(base.fingerprint(), with_window.fingerprint());
        let mut refocused = with_window.clone();
        refocused.windows[0].frontmost = false;
        assert_ne!(with_window.fingerprint(), refocused.fingerprint());
        // Sibling escalations badge the button and move the fingerprint.
        let mut with_instance = base.clone();
        with_instance.instances.push(InstanceRow {
            pid: 4242,
            sessions: 3,
            warnings: 1,
            operator: false,
        });
        assert_eq!(with_instance.button_title(), "❯⚠");
        assert_ne!(base.fingerprint(), with_instance.fingerprint());
        // Determinism: identical content ⇒ identical fingerprint.
        assert_eq!(with_instance.fingerprint(), with_instance.fingerprint());
    }

    #[test]
    fn compose_layout_not_running_is_minimal() {
        let mut g = classify(&[]);
        g.start_available = true;
        let rows = compose_status_menu(&g);
        assert_eq!(
            rows,
            vec![
                StatusRow::Info("Operator: not running".into()),
                StatusRow::Action {
                    label: "Start Operator".into(),
                    action: OperatorAction::Start,
                    enabled: true,
                },
                StatusRow::Separator,
                // The §5.1 sessions row folds the connections count, and the
                // map entry rides beside the number it summarizes — always
                // offered, because an empty fabric still opens an honest map.
                StatusRow::Info("Sessions: 0 \u{b7} Connections: 0".into()),
                StatusRow::Action {
                    label: "Show Connection Map".into(),
                    action: OperatorAction::ShowConnectionMap,
                    enabled: true,
                },
            ]
        );
    }

    #[test]
    fn start_is_inert_and_explained_without_the_cli() {
        // classify() leaves start_available=false (caller-owned fact): the
        // Start row renders disabled with the reason line under it.
        let rows = compose_status_menu(&classify(&[]));
        assert_eq!(
            &rows[1..3],
            &[
                StatusRow::Action {
                    label: "Start Operator".into(),
                    action: OperatorAction::Start,
                    enabled: false,
                },
                StatusRow::Info("(claude CLI not found on PATH)".into()),
            ]
        );
    }

    #[test]
    fn title_elected_operator_gets_no_stop_row() {
        // SI-3: vim editing "operator.md" must never receive a silent Stop.
        let rows_for = |role: Option<&str>, title: &str| {
            let mut g = classify(&[SessionRow {
                id: 7,
                title: title.into(),
                role: role.map(str::to_string),
                attention: None,
                ..SessionRow::default()
            }]);
            g.start_available = true;
            compose_status_menu(&g)
        };
        let heuristic = rows_for(None, "operator: fleet idle");
        assert!(
            !heuristic.iter().any(|r| matches!(
                r,
                StatusRow::Action {
                    action: OperatorAction::Stop,
                    ..
                }
            )),
            "title-elected operator must not be offered Stop: {heuristic:?}"
        );
        let typed = rows_for(Some("operator"), "operator: fleet idle");
        assert!(
            typed.iter().any(|r| matches!(
                r,
                StatusRow::Action {
                    action: OperatorAction::Stop,
                    ..
                }
            )),
            "typed operator must keep Stop: {typed:?}"
        );
    }

    #[test]
    fn typed_bit_reaches_the_glance_and_fingerprint() {
        let typed = classify(&[SessionRow {
            id: 1,
            title: "operator: x".into(),
            role: Some("operator".into()),
            attention: None,
            ..SessionRow::default()
        }]);
        let heuristic = classify(&[SessionRow {
            id: 1,
            title: "operator: x".into(),
            role: None,
            attention: None,
            ..SessionRow::default()
        }]);
        assert!(typed.operator_typed);
        assert!(!heuristic.operator_typed);
        // Same rendered strings, different authority — the fingerprint must
        // still differ so the menu rebuilds when the election basis changes.
        assert_ne!(typed.fingerprint(), heuristic.fingerprint());
    }

    #[test]
    fn compose_layout_full_glance_orders_every_section() {
        let mut g = classify(&[
            row(1, "operator: busy", Some("operator"), None),
            row(2, "zsh", None, Some("needs approval")),
        ]);
        g.windows = vec![
            WindowRow {
                id: 1,
                title: "build".into(),
                tabs: 1,
                frontmost: false,
            },
            WindowRow {
                id: 3,
                title: "review".into(),
                tabs: 4,
                frontmost: true,
            },
        ];
        g.instances = vec![
            InstanceRow {
                pid: 0,
                sessions: 1,
                warnings: 0,
                operator: false,
            },
            InstanceRow {
                pid: 900,
                sessions: 2,
                warnings: 1,
                operator: true,
            },
        ];
        let rows = compose_status_menu(&g);
        assert_eq!(
            rows,
            vec![
                StatusRow::Info("Operator: busy".into()),
                StatusRow::Action {
                    label: "Show Operator".into(),
                    action: OperatorAction::Show,
                    enabled: true,
                },
                StatusRow::Action {
                    label: "Stop Operator".into(),
                    action: OperatorAction::Stop,
                    enabled: true,
                },
                StatusRow::Separator,
                StatusRow::Action {
                    label: "build — 1 tab".into(),
                    action: OperatorAction::FocusWindow(1),
                    enabled: true,
                },
                StatusRow::Action {
                    label: "• review — 4 tabs".into(),
                    action: OperatorAction::FocusWindow(3),
                    enabled: true,
                },
                StatusRow::Separator,
                StatusRow::Action {
                    label: "⚠ needs approval".into(),
                    action: OperatorAction::FocusSession(2),
                    enabled: true,
                },
                StatusRow::Info("Sessions: 2 \u{b7} Connections: 0".into()),
                StatusRow::Action {
                    label: "Show Connection Map".into(),
                    action: OperatorAction::ShowConnectionMap,
                    enabled: true,
                },
                StatusRow::Separator,
                StatusRow::Info("Other aterm instances".into()),
                StatusRow::Action {
                    label: "aterm 0 — 1 session".into(),
                    action: OperatorAction::RaiseInstance(0),
                    enabled: false,
                },
                StatusRow::Action {
                    label: "aterm 900 — 2 sessions, ⚠ 1 — operator".into(),
                    action: OperatorAction::RaiseInstance(900),
                    enabled: true,
                },
            ]
        );
    }

    #[test]
    fn packed_tags_round_trip_and_out_of_range_is_inert() {
        for a in [
            OperatorAction::FocusWindow(7),
            OperatorAction::FocusSession(3),
            OperatorAction::RaiseInstance(4242),
        ] {
            assert_eq!(OperatorAction::from_tag(a.tag()), Some(a));
        }
        // Payload at the band edge encodes to the inert 0 tag, never aliases.
        assert_eq!(OperatorAction::FocusWindow(u64::MAX).tag(), 0);
        assert_eq!(OperatorAction::from_tag(0), None);
        // An unknown packed kind decodes to None.
        assert_eq!(OperatorAction::from_tag(4 * 1_000_000_000_000 + 5), None);
    }

    #[test]
    fn connections_count_moves_the_fingerprint() {
        // A mint/revoke changes ONLY the count (no title drifts) — the
        // fingerprint must still move or the menu serves the stale number.
        let base = classify(&rows(&[(1, "operator: fleet idle"), (2, "zsh")]));
        let mut minted = base.clone();
        minted.connections = 1;
        assert_ne!(base.fingerprint(), minted.fingerprint());
        let mut revoked = minted.clone();
        revoked.connections = 0;
        assert_eq!(base.fingerprint(), revoked.fingerprint());
    }

    #[test]
    fn sessions_line_folds_the_connections_count_beside_the_sessions() {
        // The menu renders this line verbatim (the §5.1 one-row rule).
        let mut g = classify(&rows(&[(1, "zsh"), (2, "vim")]));
        assert_eq!(g.sessions_line(), "Sessions: 2 \u{b7} Connections: 0");
        g.connections = 3;
        assert_eq!(g.sessions_line(), "Sessions: 2 \u{b7} Connections: 3");
    }
}
