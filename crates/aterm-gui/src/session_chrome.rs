// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session UI chrome composer.

use aterm_session::SessionId;

use crate::menu::MenuAction;

/// How many timeline events the context menu shows (the tooltip shows none) — the "recent" TAIL, newest-first.
pub(crate) const TIMELINE_TAIL: usize = 5;

/// Display cap for authored description and status activity prose (GRAPHEME CLUSTERS, not bytes or `char`s — the cap must never split a user-perceived glyph, and a ZWJ emoji or combining sequence spans several `char`s).
pub(crate) const DESCRIPTION_DISPLAY_MAX: usize = 160;

/// Everything the composer knows about one session, gathered by the caller under its own (leaf) locks and handed over as plain data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionChromeInput {
    /// The resolved tab label — the chip text this chrome annotates.
    pub label: String,
    /// `meta set icon` token (emoji / short name), prefixed onto the title line.
    pub icon: Option<String>,
    /// `meta set description` free text (display-truncated, see the cap).
    pub description: Option<String>,
    /// The live activity line — Tab Subject & Status's words (`App::session_status_text`).
    pub activity: Option<String>,
    /// The active goal of the agent in front, one line
    /// ([`crate::session_timeline::SessionTimeline::goal`]).
    pub goal: Option<String>,
    /// The engine's reported cwd (OSC 7 / shell integration), RAW.
    pub cwd: Option<String>,
    /// The `$HOME` to abbreviate against (explicit, like [`crate::app_tabs::home_relative_suffix`], so tests need no env).
    pub home: Option<String>,
    /// The registry's lifecycle state (`spawning`/`alive`/`exited`/…), `None` when the session is not registered (a stub).
    pub state: Option<String>,
    /// Whether a registry identity exists — gates the `Copy Session ID` action (greyed out rather than copying an empty string).
    pub has_session: bool,
    /// The session's registry sid, when it has one: the AGENT section's Stop Worker row carries it as its argument.
    pub sid: Option<SessionId>,
    /// Whether this window can PRESENT a rename editor at all.
    pub can_rename: bool,
    /// The recent timeline tail, NEWEST-FIRST, already cut to the rows it shows and capped to [`TIMELINE_TAIL`] by the caller ([`timeline_tail`]; the composer re-applies both defensively).
    pub timeline: Vec<TimelineNote>,
    /// Sorted live session connections (design §2.3/§4), one fact per peer, gathered from the in-process edge tables.
    pub connections: Vec<ConnectionFact>,
    /// The LIVE AGENT UPGRADE of the agent this tab runs, as its menu offers the owner's words on it (gap #21), from the window's view of the harness host's rows.
    pub upgrade: Option<UpgradeMenu>,
    /// WHY the tab's attention mark stands.
    pub attention: Option<String>,
    /// What the menu's AGENT section offers for this session (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4).
    pub agent: AgentMenu,
    /// THE SESSION LOG'S CLAUSE (phase 3B, plan §5; addendum A12; on by default since D6).
    pub clause: Option<ClauseLine>,
    /// The session keeps screen history, so each timeline row opens Rewind at its instant.
    pub rewind: bool,
}

/// The facts the tab menu's AGENT section is composed from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentMenu {
    /// Hand Back has a hold to lift.
    pub person_hold: bool,
    /// The manager this session's task names, when this aterm hosts it: `(its sid, its label)` — the `Go to <label>` row.
    pub manager: Option<(SessionId, String)>,
    /// The session runs a task that is not over: Stop Worker… is offered.
    pub stoppable: bool,
    /// The workers whose tasks THIS session started: `(sid, its row's words)`, in session order — the `Workers (n)` rows.
    pub workers: Vec<(SessionId, String)>,
    /// The session's tab is a worker's ([`crate::tab_model::Tab::ephemeral`]): Keep as Tab is
    /// offered.
    pub worker: bool,
}

/// The session log's clause as the chrome shows it ([`SessionChromeInput::clause`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClauseLine {
    /// The clause's words without their coarse age (`aterm_slog::Clause::head`).
    pub head: String,
    /// When its state began, on [`crate::turn_ledger::now_ms`]'s clock.
    pub since_ms: u64,
    /// When a shown label leaves (its `until`, on the same clock), the clause being that label; `None` for every other row.
    pub until_ms: Option<u64>,
    /// A shown label's author (`aterm_slog::Clause::by` on a label row, D6): printed after the head, outside its 48 bytes.
    pub by: Option<ClauseAuthor>,
}

/// Who wrote a label the clause shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClauseAuthor {
    /// The window's harness (`by=harness`).
    Harness,
    /// The person at the window (`by=human`).
    You,
}

impl ClauseAuthor {
    /// The author of a label written by `by`; `None` for an author whose label never shows.
    pub(crate) const fn of(by: &aterm_slog::Actor) -> Option<Self> {
        match by {
            aterm_slog::Actor::Harness => Some(Self::Harness),
            aterm_slog::Actor::Human => Some(Self::You),
            _ => None,
        }
    }

    /// The words the line ends with.
    pub(crate) const fn words(self) -> &'static str {
        match self {
            Self::Harness => "by the harness",
            Self::You => "by you",
        }
    }
}

/// What the chrome cache keys the clause on (addendum A12): its row, its head words, its tone, whose it is and when its state began.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClauseKey {
    pub row: aterm_slog::ClauseRow,
    pub head: String,
    pub tone: aterm_slog::Tone,
    pub by: aterm_slog::Actor,
    pub since_wall: u64,
}

/// One tab's live agent upgrade, as its context menu offers it (gap #21).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpgradeMenu {
    /// The session the upgrade is on (`s-<hex>`): the pane the menu was composed for, which its words name ([`TabMenuEntry::Upgrade`]).
    pub sid: String,
    /// What it moves, in the words its live row says
    /// ([`aterm_agent::harness::upgrade_drive::Row::move_words`]: `Claude Code 2.1.281 → 2.1.282`).
    pub moving: String,
    /// The target build the words are for.
    pub to: String,
    /// The words that do something for it, most useful first.
    pub words: Vec<aterm_messages::UpgradeWord>,
}

/// An upgrade word's menu item (title case, the menu's own voice — a menu item has no capsule's 16-character bound, so "Skip This Version" whole).
#[must_use]
pub(crate) const fn upgrade_menu_label(word: aterm_messages::UpgradeWord) -> &'static str {
    match word {
        aterm_messages::UpgradeWord::Now => "Upgrade Now",
        aterm_messages::UpgradeWord::NotToday => "Not Today",
        aterm_messages::UpgradeWord::Skip => "Skip This Version",
    }
}

/// Which way the AUTHORITY of a connection points, seen from the session whose chrome is being composed ("S").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnDirection {
    Outbound,
    Inbound,
    Peer,
}

/// The push/pull face of a connection's op set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnFlow {
    Push,
    Pull,
}

/// One connected peer as the chrome shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionFact {
    /// The peer's registry sid, which its [`TabMenuEntry::ConnectionAction`] rows target.
    pub peer_sid: SessionId,
    /// The peer's display title, or `unknown` for a foreign src no local session owns.
    pub peer_title: String,
    pub direction: ConnDirection,
    /// The [`ConnFlow`] of the half seen from this session.
    pub kind: ConnFlow,
    /// Whether the connection is driving right now; `false` means not known live, never idle.
    pub live: bool,
}

/// One timeline event as the chrome shows it: the kind token (printed in a person's words, [`event_words`]) plus when it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TimelineNote {
    /// The event kind token (`spawned`, `cwd-change`, `meta-change`, …).
    pub kind: &'static str,
    /// When the event was recorded, on its own monotonic clock ([`crate::turn_ledger::now_ms`]).
    pub at_ms: u64,
}

/// The terminal leaf whose session a menu row describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TerminalMenuOrigin {
    pub view: crate::tab_model::ViewId,
    pub session: u64,
}

/// One entry of the composed context menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TabMenuEntry {
    /// A disabled, informational row (identity / connection lines).
    Header(String),
    /// A disabled timeline row: its event in a person's words and when it happened ([`crate::turn_ledger::now_ms`]'s clock).
    Event { words: String, at_ms: u64 },
    /// A timeline row of a session that keeps screen history: a click opens Rewind at `at_ms`.
    RewindTo {
        words: String,
        at_ms: u64,
        /// Bound on a per-tab clone; the session's cached template has no view owner.
        origin: Option<TerminalMenuOrigin>,
    },
    /// A native separator.
    Separator,
    /// A command against the whole captured tab, independent of its focused pane.
    Action {
        label: String,
        action: MenuAction,
        enabled: bool,
    },
    /// A command against the terminal session that supplied this row's words and state.
    SessionAction {
        label: String,
        action: MenuAction,
        enabled: bool,
        /// Bound on a per-tab clone, never in the shared session template.
        origin: Option<TerminalMenuOrigin>,
    },
    /// A per-peer row (Show / Configure… / Disconnect) against `peer_sid`.
    ConnectionAction {
        label: String,
        peer_sid: SessionId,
        verb: ConnVerb,
        enabled: bool,
    },
    /// The owner's word on this tab's live agent upgrade, for build `to` of session `sid`.
    Upgrade {
        label: String,
        sid: String,
        to: String,
        word: aterm_messages::UpgradeWord,
    },
}

/// The words of the one way a worker's tab becomes a person's: the tab menu's row and the Workers
/// page's button.
pub(crate) const KEEP_AS_TAB: &str = "Keep as Tab";

/// The closed verb vocabulary of a [`TabMenuEntry::ConnectionAction`] row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnVerb {
    /// Raise the window hosting the session and select its tab (the `raise` wire verb's UI twin): `Go to <manager>`, a worker row.
    Show,
    /// Stop Worker…: after the person confirms the sheet (or, with no sheet to ask on, the Overview's inline confirm), SIGTERM to the session's agent, in process (`App::stop_worker`).
    StopWorker,
    /// Keep as Tab: a worker's tab becomes the person's, on the strip and never closed for its
    /// task's end (`App::keep_as_tab`).
    KeepAsTab,
}

/// The per-tab tooltip and context-menu model the app pushes to the native strip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabChromeExt {
    /// Tooltip text for the tab chip (`None` = no tooltip installed).
    pub tooltip: Option<String>,
    /// Context-menu model (`empty` = no context menu pops).
    pub menu: Vec<TabMenuEntry>,
    /// The tab the menu's rows act on where it is not the chip's own: the worker the Workers chip
    /// shows.
    pub menu_tab: Option<crate::tab_model::TabId>,
}

impl TabChromeExt {
    /// Bind this tab's cloned session rows without changing the shared session cache.
    pub(crate) fn bind_terminal_origin(&mut self, origin: TerminalMenuOrigin) {
        for entry in &mut self.menu {
            if let TabMenuEntry::RewindTo { origin: owner, .. }
            | TabMenuEntry::SessionAction { origin: owner, .. } = entry
            {
                *owner = Some(origin);
            }
        }
    }
}

/// One session's cached chrome and the input epochs it was composed from (the `App::session_chrome` map's value).
pub(crate) struct CachedChrome {
    /// The timeline's high-water event id.
    pub high_id: u64,
    /// The resolved tab label: an OSC title can move without a timeline record.
    pub label: String,
    /// Whether the window that composed these rows could present the session editor.
    pub can_rename: bool,
    /// The session-status revision at compose time.
    pub activity_revision: u64,
    /// The connection table's revision (`ConnectionTable::bump_revision`): a mint, revoke or sweep moves no other epoch.
    pub connections_revision: u64,
    /// The window's `upgrade_host::UpgradeView` revision: the harness host's rows move no other epoch.
    pub upgrade_revision: u64,
    /// The agent's active goal at compose time (`claude_footer::goal_term`): a goal read moves no other epoch.
    pub goal: u64,
    /// When the clock may move this composition with no input epoch moving ([`clock_expiry_ms`], from [`crate::turn_ledger::now_ms`] at compose time).
    pub expires_ms: Option<u64>,
    /// The session log's clause at compose time ([`ClauseKey`]), `None` with `[session_log] clause` off or no clause.
    pub clause: Option<ClauseKey>,
    /// The clause line composed from it, served again when the timeline is contended at the next compose.
    pub clause_line: Option<ClauseLine>,
    /// The composed extension handed to the native strip.
    pub ext: TabChromeExt,
}

/// How long a cached composition the clock moves (an upgrade's words) may serve before it is re-derived with unchanged inputs.
pub(crate) const CACHE_MAX_AGE_MS: u64 = 30_000;

/// Window inspections/toolbar refreshes the expiry fan-out performs per event-loop turn; the cursor keeps the remainder.
pub(crate) const EXPIRY_WINDOW_SCAN_BUDGET: usize = 1;

/// Observable projection of one expiry scheduler turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExpiryProgress {
    pub(crate) admitted_session: Option<u64>,
    pub(crate) window_scans: usize,
    pub(crate) window_refreshes: usize,
    pub(crate) completed_session: Option<u64>,
}

/// THE CLOCK'S PART IN A COMPOSITION.
#[must_use]
pub(crate) fn clock_expiry_ms(composed_ms: u64, input: &SessionChromeInput) -> Option<u64> {
    let upgrade = input
        .upgrade
        .is_some()
        .then(|| composed_ms.saturating_add(CACHE_MAX_AGE_MS));
    let label = input.clause.as_ref().and_then(|c| c.until_ms);
    match (upgrade, label) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Earliest absolute process-clock expiry represented by the cache, or `None` when no retained composition moves with the clock (the event loop can return to pure Wait).
#[must_use]
pub(crate) fn next_cache_expiry_ms(
    cache: &std::collections::HashMap<u64, CachedChrome>,
) -> Option<u64> {
    cache.values().filter_map(|c| c.expires_ms).min()
}

/// At most `budget` entries due at `now_ms`, ordered by deadline then session id.
#[must_use]
pub(crate) fn due_cache_batch(
    cache: &std::collections::HashMap<u64, CachedChrome>,
    now_ms: u64,
    budget: usize,
) -> Vec<u64> {
    if budget == 0 {
        return Vec::new();
    }
    let mut due: Vec<(u64, u64)> = Vec::new();
    for (session, entry) in cache {
        let Some(expiry) = entry.expires_ms.filter(|at| *at <= now_ms) else {
            continue;
        };
        let candidate = (expiry, *session);
        if due.len() < budget {
            due.push(candidate);
            due.sort_unstable();
        } else if candidate < due[due.len() - 1] {
            let last = due.len() - 1;
            due[last] = candidate;
            due.sort_unstable();
        }
    }
    due.into_iter().map(|(_, session)| session).collect()
}

/// An age delta as the coarse token the chrome shows.
#[must_use]
pub(crate) fn relative_age(age_ms: u64) -> String {
    const SEC: u64 = 1000;
    const MIN: u64 = 60 * SEC;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    if age_ms < 10 * SEC {
        "just now".to_string()
    } else if age_ms < MIN {
        format!("{}s ago", age_ms / SEC)
    } else if age_ms < HOUR {
        format!("{}m ago", age_ms / MIN)
    } else if age_ms < DAY {
        format!("{}h ago", age_ms / HOUR)
    } else {
        format!("{}d ago", age_ms / DAY)
    }
}

fn display_cwd(cwd: &str, home: Option<&str>) -> String {
    let display = match home.and_then(|h| crate::app_tabs::home_relative_suffix(cwd, h)) {
        Some(rest) => format!("~{rest}"),
        None => cwd.to_string(),
    };
    let display = crate::session_timeline::sanitize_presentation_line(
        &display,
        crate::session_timeline::META_DESCRIPTION_MAX,
    );
    display_description(&display)
}

fn title_line(input: &SessionChromeInput) -> String {
    let label = crate::session_timeline::sanitize_presentation_line(&input.label, usize::MAX);
    match input
        .icon
        .as_deref()
        .and_then(|icon| crate::session_timeline::sanitize_metadata_value("icon", icon))
    {
        Some(icon) => format!("{icon} {label}"),
        None => label,
    }
}

fn display_description(prose: &str) -> String {
    use aterm_grapheme::GraphemeClusters;
    let mut it = prose.graphemes();
    let head: String = it.by_ref().take(DESCRIPTION_DISPLAY_MAX).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn connection_line(fact: &ConnectionFact) -> String {
    let title = display_description(&crate::session_timeline::sanitize_presentation_line(
        &fact.peer_title,
        crate::session_timeline::META_DESCRIPTION_MAX,
    ));
    let head = match (fact.direction, fact.kind) {
        (ConnDirection::Outbound, ConnFlow::Push) => "\u{21e5} pushes into",
        (ConnDirection::Outbound, ConnFlow::Pull) => "\u{21e5} pulls from",
        (ConnDirection::Inbound, ConnFlow::Push) => "\u{21e4} pushed by",
        (ConnDirection::Inbound, ConnFlow::Pull) => "\u{21e4} pulled by",
        (ConnDirection::Peer, _) => "\u{21c6} peer",
    };
    let live = if fact.live { " \u{00b7} live" } else { "" };
    format!("{head} \"{title}\"{live}")
}

fn connection_lines(input: &SessionChromeInput) -> Vec<String> {
    input.connections.iter().map(connection_line).collect()
}

fn header_lines(input: &SessionChromeInput) -> (Vec<String>, usize) {
    let mut identity = vec![title_line(input)];
    if let Some(description) = input.description.as_deref().and_then(|description| {
        crate::session_timeline::sanitize_metadata_value("description", description)
    }) {
        identity.push(format!(
            "description: {}",
            display_description(&description)
        ));
    }
    if let Some(activity) = input.activity.as_deref().filter(|a| !a.is_empty()) {
        let activity = crate::session_timeline::sanitize_presentation_line(
            activity,
            crate::session_timeline::META_DESCRIPTION_MAX,
        );
        if !activity.is_empty() {
            identity.push(format!("activity: {}", display_description(&activity)));
        }
    }
    if let Some(goal) = input.goal.as_deref() {
        let goal = crate::session_timeline::sanitize_presentation_line(
            goal,
            crate::session_timeline::META_DESCRIPTION_MAX,
        );
        if !goal.is_empty() {
            identity.push(format!("{GOAL_PREFIX}{}", display_description(&goal)));
        }
    }
    let clause_at = identity.len();
    if let Some(cwd) = input.cwd.as_deref().filter(|c| !c.is_empty()) {
        let cwd = display_cwd(cwd, input.home.as_deref());
        if !cwd.is_empty() {
            identity.push(format!("cwd: {cwd}"));
        }
    }
    if let Some(state) = input.state.as_deref().filter(|s| {
        !s.is_empty()
            && !matches!(*s, "-" | "alive" | "spawning")
            && !input.activity.as_deref().is_some_and(|a| a.starts_with(s))
    }) {
        identity.push(format!("state: {state}"));
    }
    (identity, clause_at)
}

fn clause_words(clause: &ClauseLine) -> Option<String> {
    let head = crate::session_timeline::sanitize_presentation_line(
        &clause.head,
        crate::session_timeline::META_DESCRIPTION_MAX,
    );
    (!head.is_empty()).then(|| match clause.by {
        Some(by) => format!("now: {head} \u{b7} {}", by.words()),
        None => format!("now: {head}"),
    })
}

fn timeline_rows(input: &SessionChromeInput) -> Vec<TabMenuEntry> {
    input
        .timeline
        .iter()
        .filter(|n| timeline_row_shown(n.kind))
        .take(TIMELINE_TAIL)
        .map(|n| {
            let words = event_words(n.kind).to_string();
            if input.rewind {
                TabMenuEntry::RewindTo {
                    words,
                    at_ms: n.at_ms,
                    origin: None,
                }
            } else {
                TabMenuEntry::Event {
                    words,
                    at_ms: n.at_ms,
                }
            }
        })
        .collect()
}

impl TabMenuEntry {
    /// This entry as a surface shows it at `now_ms` ([`crate::turn_ledger::now_ms`]'s clock).
    #[must_use]
    pub(crate) fn shown_at(&self, now_ms: u64) -> Self {
        match self {
            Self::Event { words, at_ms } => Self::Header(format!(
                "{words} \u{b7} {}",
                relative_age(now_ms.saturating_sub(*at_ms))
            )),
            Self::RewindTo {
                words,
                at_ms,
                origin,
            } => Self::RewindTo {
                words: format!(
                    "{words} \u{b7} {}",
                    relative_age(now_ms.saturating_sub(*at_ms))
                ),
                at_ms: *at_ms,
                origin: *origin,
            },
            other => other.clone(),
        }
    }
}

/// A composed menu as a surface shows it at `now_ms`: every timeline row aged ([`TabMenuEntry::shown_at`]).
#[must_use]
pub(crate) fn menu_shown_at(entries: &[TabMenuEntry], now_ms: u64) -> Vec<TabMenuEntry> {
    entries.iter().map(|e| e.shown_at(now_ms)).collect()
}

/// Whether a timeline event earns a row in the tab's hover chrome.
pub(crate) fn timeline_row_shown(kind: &str) -> bool {
    !matches!(
        kind,
        "agent-change"
            | crate::session_timeline::PROGRAM_STATE
            | "human"
            | "fetch"
            | "topic"
            | "inbox-seen"
            | "post-landed"
            | "render"
            | "holder-edge"
            | "holder-image"
            | "holder-named"
            | "holder-claim"
            | "card-seen"
            | "card-decorated"
            | "card-row"
            | "card-armed"
            | "card-bare"
            | "holder-adopt"
            | "holder-retire"
            | crate::claude_footer::REGISTRY_READ
    )
}

/// The rows the hover chrome shows, from a NEWEST-FIRST walk of the session's `(kind, t_ms)` rows (the session log's view).
pub(crate) fn timeline_tail(
    newest_first: impl Iterator<Item = (&'static str, u64)>,
) -> Vec<TimelineNote> {
    newest_first
        .filter(|(kind, _)| timeline_row_shown(kind))
        .take(TIMELINE_TAIL)
        .map(|(kind, at_ms)| TimelineNote { kind, at_ms })
        .collect()
}

fn event_words(kind: &str) -> &str {
    match kind {
        "spawned" => "started",
        "handoff" => "carried across an update",
        "cwd-change" => "changed directory",
        "meta-change" => "details changed",
        "title-change" => "renamed",
        "state-change" => "state changed",
        "modes-restored" => "terminal modes restored",
        "hold" => "hold changed",
        "hold-advisory" => "fabric lost (not halted)",
        "inbox" => "mail arrived",
        "post" => "mail sent",
        "in-doubt" => "input unconfirmed",
        "task-change" => "task changed",
        "job-change" => "job verdict changed",
        other => other,
    }
}

/// Compose the hover TOOLTIP for one terminal tab, or `None` when the session carries NOTHING beyond its bare label (no authored description, status activity, icon, cwd, exited state or connection).
#[must_use]
pub(crate) fn compose_tooltip(input: &SessionChromeInput) -> Option<String> {
    let (mut identity, clause_at) = header_lines(input);
    if let Some(line) = input.clause.as_ref().and_then(clause_words) {
        identity.insert(clause_at, line);
    }
    let connections = connection_lines(input);
    let icon = input
        .icon
        .as_deref()
        .and_then(|icon| crate::session_timeline::sanitize_metadata_value("icon", icon))
        .is_some();
    let attention = attention_line(input);
    if identity.len() <= 1 && !icon && connections.is_empty() && attention.is_none() {
        return None;
    }
    let mut lines = identity;
    lines.extend(attention);
    lines.extend(connections);
    Some(lines.join("\n"))
}

/// Prefix of the hover's and the tab menu's line naming the agent's active goal.
pub(crate) const GOAL_PREFIX: &str = "goal: ";

/// Prefix of the tooltip's attention line; the native hover ([`crate::toolbar`]) reads it.
pub(crate) const NEEDS_ATTENTION_PREFIX: &str = "Needs attention: ";

/// The hover's line for a harness's need in the worker's own words (`App::presence_hover`, design §12.2.1) starts with this in place of [`NEEDS_ATTENTION_PREFIX`].
pub(crate) const NEEDS_YOU_PREFIX: &str = "Needs you: ";

fn attention_line(input: &SessionChromeInput) -> Option<String> {
    let reason = input
        .attention
        .as_deref()
        .and_then(|reason| crate::session_timeline::sanitize_metadata_value("attention", reason))?;
    Some(format!(
        "{NEEDS_ATTENTION_PREFIX}{}",
        display_description(&reason)
    ))
}

/// Compose the right-click CONTEXT-MENU model for one terminal tab.
#[must_use]
pub(crate) fn compose_tab_menu(input: &SessionChromeInput) -> Vec<TabMenuEntry> {
    let (headers, clause_at) = header_lines(input);
    let mut entries: Vec<TabMenuEntry> = headers.into_iter().map(TabMenuEntry::Header).collect();
    if let Some(clause) = input.clause.as_ref()
        && let Some(words) = clause_words(clause)
    {
        entries.insert(
            clause_at,
            TabMenuEntry::Event {
                words,
                at_ms: clause.since_ms,
            },
        );
    }
    let timeline = timeline_rows(input);
    if !timeline.is_empty() {
        entries.push(TabMenuEntry::Separator);
        entries.extend(timeline);
    }
    if let Some(upgrade) = input.upgrade.as_ref().filter(|u| !u.words.is_empty()) {
        entries.push(TabMenuEntry::Separator);
        entries.push(TabMenuEntry::Header("AGENT UPGRADE".to_string()));
        entries.push(TabMenuEntry::Header(upgrade.moving.clone()));
        for word in &upgrade.words {
            entries.push(TabMenuEntry::Upgrade {
                label: upgrade_menu_label(*word).to_string(),
                sid: upgrade.sid.clone(),
                to: upgrade.to.clone(),
                word: *word,
            });
        }
    }
    entries.push(TabMenuEntry::Separator);
    entries.push(TabMenuEntry::Header("AGENT".to_string()));
    let action = |label: &str, action: MenuAction, enabled: bool| TabMenuEntry::SessionAction {
        label: label.to_string(),
        action,
        enabled,
        origin: None,
    };
    let agent = &input.agent;
    entries.push(if agent.person_hold {
        action(
            "Hand Back This Tab",
            MenuAction::HandBack,
            input.has_session,
        )
    } else {
        action(
            "Take Over This Tab",
            MenuAction::TakeOver,
            input.has_session,
        )
    });
    entries.push(action(
        "Harness Log for This Tab",
        MenuAction::HarnessLog,
        input.has_session,
    ));
    entries.push(action(
        "Set Role…",
        MenuAction::SetRole,
        input.has_session && input.can_rename,
    ));
    if let Some((manager, label)) = &agent.manager {
        entries.push(TabMenuEntry::ConnectionAction {
            label: format!("Go to {label}"),
            peer_sid: manager.clone(),
            verb: ConnVerb::Show,
            enabled: true,
        });
    }
    if agent.stoppable
        && let Some(sid) = input.sid.clone()
    {
        entries.push(TabMenuEntry::ConnectionAction {
            label: "Stop Worker\u{2026}".to_string(),
            peer_sid: sid,
            verb: ConnVerb::StopWorker,
            enabled: true,
        });
    }
    if agent.worker
        && let Some(sid) = input.sid.clone()
    {
        entries.push(TabMenuEntry::ConnectionAction {
            label: KEEP_AS_TAB.to_string(),
            peer_sid: sid,
            verb: ConnVerb::KeepAsTab,
            enabled: true,
        });
    }
    if !agent.workers.is_empty() {
        entries.push(TabMenuEntry::Header(format!(
            "Workers ({})",
            agent.workers.len()
        )));
        for (sid, words) in &agent.workers {
            entries.push(TabMenuEntry::ConnectionAction {
                label: words.clone(),
                peer_sid: sid.clone(),
                verb: ConnVerb::Show,
                enabled: true,
            });
        }
    }
    entries.push(TabMenuEntry::Separator);
    entries.push(action(
        "Rename Session…",
        MenuAction::RenameSession,
        input.has_session && input.can_rename,
    ));
    entries.push(action(
        "Copy Session ID",
        MenuAction::CopySessionId,
        input.has_session,
    ));
    entries.push(action(
        "Copy CWD",
        MenuAction::CopyCwd,
        input.cwd.as_deref().is_some_and(|c| !c.is_empty()),
    ));
    entries.push(TabMenuEntry::Action {
        label: "Close Tab".into(),
        action: MenuAction::CloseTab,
        enabled: true,
    });
    entries
}

/// Serialise one tab's context-menu model to the `chrome` verb's line.
#[must_use]
pub(crate) fn tab_menu_chrome_line(index: usize, entries: &[TabMenuEntry], now_ms: u64) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|entry| menu_entry_mirror(&entry.shown_at(now_ms)))
        .collect();
    format!("tab-menu tab={index} items={items:?}")
}

/// One entry's mirror token.
#[must_use]
pub(crate) fn menu_entry_mirror(entry: &TabMenuEntry) -> String {
    let disabled_suffix = |enabled: bool| if enabled { "" } else { " (disabled)" };
    match entry {
        TabMenuEntry::Header(t) => t.clone(),
        TabMenuEntry::Event { words, .. } => words.clone(),
        TabMenuEntry::RewindTo { words, .. } => format!("{words} (rewind)"),
        TabMenuEntry::Separator => "---".to_string(),
        TabMenuEntry::Action { label, enabled, .. } => {
            format!("{label}{}", disabled_suffix(*enabled))
        }
        TabMenuEntry::SessionAction {
            label,
            enabled,
            origin,
            ..
        } => {
            format!("{label}{}", disabled_suffix(*enabled && origin.is_some()))
        }
        TabMenuEntry::ConnectionAction {
            label,
            peer_sid,
            enabled,
            ..
        } => format!(
            "{label} @{}{}",
            peer_sid.as_str(),
            disabled_suffix(*enabled)
        ),
        TabMenuEntry::Upgrade { label, to, .. } => format!("{label} ({to})"),
    }
}

#[cfg(test)]
#[path = "session_chrome_tests.rs"]
mod tests;
