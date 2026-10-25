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
    /// What it moves, in the band's words (`Claude Code 2.1.281 → 2.1.282`).
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

/// One entry of the composed context menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TabMenuEntry {
    /// A disabled, informational row (identity / connection lines).
    Header(String),
    /// A disabled timeline row: its event in a person's words and when it happened ([`crate::turn_ledger::now_ms`]'s clock).
    Event { words: String, at_ms: u64 },
    /// A native separator.
    Separator,
    /// A command row: the `NSMenuItem` carries `action`'s tag, so the click dispatches through the menu bar's tag→[`MenuAction`] decode.
    Action {
        label: String,
        action: MenuAction,
        enabled: bool,
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

/// The closed verb vocabulary of a [`TabMenuEntry::ConnectionAction`] row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnVerb {
    /// Raise the window hosting the session and select its tab (the `raise` wire verb's UI twin): `Go to <manager>`, a worker row.
    Show,
    /// Stop Worker…: after the person confirms the sheet (or, with no sheet to ask on, the Overview's inline confirm), SIGTERM to the session's agent, in process (`App::stop_worker`).
    StopWorker,
}

/// The per-tab tooltip and context-menu model the app pushes to the native strip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabChromeExt {
    /// Tooltip text for the tab chip (`None` = no tooltip installed).
    pub tooltip: Option<String>,
    /// Context-menu model (`empty` = no context menu pops).
    pub menu: Vec<TabMenuEntry>,
}

/// One session's cached chrome and the input epochs it was composed from (the `App::session_chrome` map's value).
pub(crate) struct CachedChrome {
    /// The timeline's high-water event id.
    pub high_id: u64,
    /// The resolved tab label: an OSC title can move without a timeline record.
    pub label: String,
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
        .map(|n| TabMenuEntry::Event {
            words: event_words(n.kind).to_string(),
            at_ms: n.at_ms,
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
    let action = |label: &str, action: MenuAction, enabled: bool| TabMenuEntry::Action {
        label: label.to_string(),
        action,
        enabled,
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
    entries.push(action("Close Tab", MenuAction::CloseTab, true));
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
        TabMenuEntry::Separator => "---".to_string(),
        TabMenuEntry::Action { label, enabled, .. } => {
            format!("{label}{}", disabled_suffix(*enabled))
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
mod tests {
    use super::*;

    fn cached_at(composed_ms: u64) -> CachedChrome {
        CachedChrome {
            high_id: 0,
            label: "session".to_string(),
            activity_revision: 0,
            connections_revision: 0,
            upgrade_revision: 0,
            goal: 0,
            expires_ms: Some(composed_ms + CACHE_MAX_AGE_MS),
            clause: None,
            clause_line: None,
            ext: TabChromeExt::default(),
        }
    }

    #[test]
    fn expiry_batch_is_deadline_ordered_bounded_and_self_disarming() {
        let mut cache = std::collections::HashMap::new();
        cache.insert(9, cached_at(20));
        cache.insert(7, cached_at(10));
        cache.insert(5, cached_at(10));
        assert_eq!(next_cache_expiry_ms(&cache), Some(10 + CACHE_MAX_AGE_MS));

        let due_at_boundary = due_cache_batch(&cache, 10 + CACHE_MAX_AGE_MS, 1);
        assert_eq!(
            due_at_boundary,
            vec![5],
            "equal deadlines use session id order"
        );
        let due = due_cache_batch(&cache, 20 + CACHE_MAX_AGE_MS, 2);
        assert_eq!(due, vec![5, 7], "one turn never exceeds its fixed budget");

        for session in due {
            cache.remove(&session);
        }
        assert_eq!(
            due_cache_batch(&cache, 20 + CACHE_MAX_AGE_MS, 2),
            vec![9],
            "the finite remainder stays immediately due for the next turn"
        );
        cache.clear();
        assert_eq!(
            next_cache_expiry_ms(&cache),
            None,
            "empty cache owns no wake"
        );
    }

    const NOW: u64 = 1_000_000_000;

    #[test]
    fn the_clock_moves_a_composition_only_by_an_upgrades_words() {
        let mut input = full_input();
        assert_eq!(clock_expiry_ms(NOW, &input), None, "its ages arm nothing");
        let menu = compose_tab_menu(&input);
        let aged = |at: u64| -> Vec<String> {
            menu_shown_at(&menu, at)
                .into_iter()
                .filter_map(|e| match e {
                    TabMenuEntry::Header(h) if h.contains(" \u{b7} ") => Some(h),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            aged(NOW),
            [
                "details changed \u{b7} just now",
                "changed directory \u{b7} 2m ago",
                "started \u{b7} 2h ago"
            ]
        );
        assert_eq!(
            aged(NOW + 60_000),
            [
                "details changed \u{b7} 1m ago",
                "changed directory \u{b7} 3m ago",
                "started \u{b7} 2h ago"
            ],
            "the same composition, aged a minute on"
        );
        let line = tab_menu_chrome_line(0, &menu, NOW + 60_000);
        assert!(
            line.contains("\"changed directory \u{b7} 3m ago\""),
            "{line}"
        );
        input.upgrade = Some(UpgradeMenu {
            sid: "s-1".to_string(),
            moving: "Claude Code 2.1.281 → 2.1.282".to_string(),
            to: "2.1.282".to_string(),
            words: Vec::new(),
        });
        assert_eq!(clock_expiry_ms(NOW, &input), Some(NOW + CACHE_MAX_AGE_MS));
        let mut cache = std::collections::HashMap::new();
        let mut quiet = cached_at(NOW);
        quiet.expires_ms = None;
        cache.insert(3, quiet);
        assert_eq!(next_cache_expiry_ms(&cache), None);
        assert!(due_cache_batch(&cache, u64::MAX, 4).is_empty());
    }

    fn full_input() -> SessionChromeInput {
        SessionChromeInput {
            can_rename: true,
            label: "build agent".to_string(),
            icon: Some("🤖".to_string()),
            description: Some("Rebuilds the docs site".to_string()),
            activity: Some("Building documentation".to_string()),
            goal: None,
            cwd: Some("/Users//foo/src/aterm".to_string()),
            home: Some("/Users//foo".to_string()),
            state: Some("alive".to_string()),
            has_session: true,
            timeline: vec![
                TimelineNote {
                    kind: "meta-change",
                    at_ms: NOW - 3_000,
                },
                TimelineNote {
                    kind: "cwd-change",
                    at_ms: NOW - 125_000,
                },
                TimelineNote {
                    kind: "spawned",
                    at_ms: NOW - 7_200_000,
                },
            ],
            connections: Vec::new(),
            upgrade: None,
            attention: None,
            sid: None,
            agent: AgentMenu::default(),
            clause: None,
        }
    }

    fn fact(
        sid: &str,
        title: &str,
        direction: ConnDirection,
        kind: ConnFlow,
        live: bool,
    ) -> ConnectionFact {
        ConnectionFact {
            peer_sid: SessionId::new(sid),
            peer_title: title.to_string(),
            direction,
            kind,
            live,
        }
    }

    #[test]
    fn tooltip_orders_identity_with_home_abbreviation() {
        let tip = compose_tooltip(&full_input()).expect("full metadata composes");
        assert_eq!(
            tip,
            "🤖 build agent\n\
             description: Rebuilds the docs site\n\
             activity: Building documentation\n\
             cwd: ~/src/aterm"
        );
        let mut exited = full_input();
        exited.state = Some("exited".to_string());
        assert!(
            compose_tooltip(&exited)
                .unwrap()
                .ends_with("\ncwd: ~/src/aterm\nstate: exited")
        );
        exited.activity = Some("exited (exit 1)".to_string());
        let tip = compose_tooltip(&exited).unwrap();
        assert!(tip.contains("activity: exited (exit 1)"), "{tip}");
        assert!(!tip.contains("state: exited"), "{tip}");
        let bare = SessionChromeInput {
            label: "zsh".to_string(),
            state: Some("alive".to_string()),
            has_session: true,
            timeline: vec![TimelineNote {
                kind: "spawned",
                at_ms: NOW - 7_200_000,
            }],
            ..SessionChromeInput::default()
        };
        assert_eq!(compose_tooltip(&bare), None);
    }

    #[test]
    fn the_agents_active_goal_is_a_line_of_the_hover_and_the_tab_menu() {
        let mut input = full_input();
        input.goal = Some("implement the aos aterm vision".to_string());
        let tip = compose_tooltip(&input).expect("composes");
        assert_eq!(
            tip,
            "🤖 build agent\n\
             description: Rebuilds the docs site\n\
             activity: Building documentation\n\
             goal: implement the aos aterm vision\n\
             cwd: ~/src/aterm"
        );
        let menu = compose_tab_menu(&input);
        assert!(
            menu.contains(&TabMenuEntry::Header(
                "goal: implement the aos aterm vision".to_string()
            )),
            "{menu:?}"
        );
        input.goal = Some(format!("spoof\u{202e}ed {}", "word ".repeat(80)));
        let tip = compose_tooltip(&input).expect("composes");
        let line = tip
            .lines()
            .find(|l| l.starts_with(GOAL_PREFIX))
            .expect("goal line");
        assert!(line.starts_with("goal: spoofed word"), "{line}");
        assert!(
            line.ends_with('…'),
            "a long goal is cut for display: {line}"
        );
        assert!(!crate::session_timeline::metadata_has_forbidden_formatting(
            line
        ));
        let bare = SessionChromeInput {
            label: "claude".to_string(),
            goal: Some("ship it".to_string()),
            ..SessionChromeInput::default()
        };
        assert_eq!(
            compose_tooltip(&bare).as_deref(),
            Some("claude\ngoal: ship it"),
            "a goal alone is worth a hover"
        );
    }

    #[test]
    fn chrome_defensively_strips_controls_and_bidi_from_legacy_input() {
        let input = SessionChromeInput {
            label: "raw\u{202e}title".to_string(),
            icon: Some("\u{2066}🚀\u{2069}".to_string()),
            description: Some("first\nsecond\u{2029}third".to_string()),
            activity: Some("running\u{0085}tests".to_string()),
            cwd: Some("/tmp/first\nsecond\u{202e}spoof".to_string()),
            ..SessionChromeInput::default()
        };
        let tooltip = compose_tooltip(&input).expect("sanitized fields still compose");
        assert_eq!(
            tooltip,
            "🚀 rawtitle\ndescription: firstsecondthird\nactivity: runningtests\ncwd: /tmp/firstsecondspoof"
        );
        assert!(
            tooltip
                .lines()
                .all(|line| { !crate::session_timeline::metadata_has_forbidden_formatting(line) })
        );
        assert!(compose_tab_menu(&input).iter().all(|entry| match entry {
            TabMenuEntry::Header(label) | TabMenuEntry::Event { words: label, .. } =>
                !crate::session_timeline::metadata_has_forbidden_formatting(label),
            TabMenuEntry::Action { label, .. }
            | TabMenuEntry::ConnectionAction { label, .. }
            | TabMenuEntry::Upgrade { label, .. } =>
                !crate::session_timeline::metadata_has_forbidden_formatting(label),
            TabMenuEntry::Separator => true,
        }));
    }

    #[test]
    fn the_tooltip_names_why_a_tab_needs_a_look() {
        let stalled = SessionChromeInput {
            attention: Some("Claude Code 2.1.283 -> 2.1.284 upgrade stalled".to_string()),
            ..full_input()
        };
        let tip = compose_tooltip(&stalled).expect("composes");
        assert!(
            tip.ends_with(
                "\ncwd: ~/src/aterm\nNeeds attention: Claude Code 2.1.283 -> 2.1.284 upgrade stalled"
            ),
            "{tip}"
        );
        let alone = SessionChromeInput {
            label: "zsh".to_string(),
            attention: Some("wedged on CI".to_string()),
            ..SessionChromeInput::default()
        };
        assert_eq!(
            compose_tooltip(&alone).as_deref(),
            Some("zsh\nNeeds attention: wedged on CI"),
            "a reason alone earns a tooltip"
        );
    }

    #[test]
    fn the_reason_is_capped_and_sanitized() {
        let input = SessionChromeInput {
            label: "zsh".to_string(),
            attention: Some(format!("wedged\u{202e} on\nCI {}", "x".repeat(600))),
            ..SessionChromeInput::default()
        };
        let tip = compose_tooltip(&input).expect("composes");
        let line = tip.lines().nth(1).expect("the reason line");
        assert!(
            line.starts_with("Needs attention: wedged onCI xxx"),
            "{line}"
        );
        assert!(!crate::session_timeline::metadata_has_forbidden_formatting(
            line
        ));
        assert_eq!(tip.lines().count(), 2, "one line, no injected newline");
        assert!(
            line.chars().count()
                <= NEEDS_ATTENTION_PREFIX.chars().count() + DESCRIPTION_DISPLAY_MAX + 1
        );
    }

    #[test]
    fn empty_metadata_degrades_to_no_tooltip() {
        let input = SessionChromeInput {
            label: "zsh".to_string(),
            ..SessionChromeInput::default()
        };
        assert_eq!(compose_tooltip(&input), None);
        let dashed = SessionChromeInput {
            label: "zsh".to_string(),
            state: Some("-".to_string()),
            icon: Some(String::new()),
            cwd: Some(String::new()),
            ..SessionChromeInput::default()
        };
        assert_eq!(compose_tooltip(&dashed), None);
    }

    #[test]
    fn description_truncation_never_splits_a_grapheme_cluster() {
        let family = "👨\u{200D}👩\u{200D}👧";
        let mut input = full_input();
        input.description = Some(format!("{}{family}", "x".repeat(159)));
        let tip = compose_tooltip(&input).unwrap();
        let line = tip
            .lines()
            .nth(1)
            .unwrap()
            .strip_prefix("description: ")
            .unwrap();
        assert!(
            line.ends_with(family),
            "cluster at the cap boundary stays whole: {line:?}"
        );
        assert!(!line.ends_with('…'), "exactly at the cap ⇒ no ellipsis");

        input.description = Some(format!("{}{family}", "x".repeat(160)));
        let tip = compose_tooltip(&input).unwrap();
        let line = tip
            .lines()
            .nth(1)
            .unwrap()
            .strip_prefix("description: ")
            .unwrap();
        assert_eq!(
            line,
            format!("{}…", "x".repeat(160)),
            "over-cap cluster is dropped whole, cut lands on a cluster boundary"
        );

        input.description = Some(format!("{}e\u{301}zz", "x".repeat(159)));
        let tip = compose_tooltip(&input).unwrap();
        let line = tip
            .lines()
            .nth(1)
            .unwrap()
            .strip_prefix("description: ")
            .unwrap();
        assert!(
            line.contains("e\u{301}"),
            "combining accent stays attached to its base: {line:?}"
        );
        assert!(line.ends_with('…'), "the trailing `zz` is truncated away");
    }

    #[test]
    fn timeline_tail_is_capped() {
        let mut input = full_input();
        input.timeline = (0..9)
            .map(|i| TimelineNote {
                kind: "state-change",
                at_ms: NOW - i * 1000,
            })
            .collect();
        let tip = compose_tooltip(&input).unwrap();
        let events = tip
            .lines()
            .filter(|l| l.starts_with("state changed"))
            .count();
        assert_eq!(events, 0, "the tooltip is no log: {tip:?}");
        let menu = menu_shown_at(&compose_tab_menu(&input), NOW);
        let headers = menu
            .iter()
            .filter(|e| matches!(e, TabMenuEntry::Header(h) if h.starts_with("state changed")))
            .count();
        assert_eq!(headers, TIMELINE_TAIL);
    }

    #[test]
    fn timeline_tail_leaves_out_agent_moves_and_typing() {
        let newest_first = [
            ("agent-change", 90),
            ("human", 90),
            ("fetch", 90),
            ("topic", 90),
            ("inbox-seen", 90),
            ("post-landed", 90),
            ("render", 90),
            ("program-state", 90),
            ("agent-change", 90),
            ("inbox", 90),
            ("cwd-change", 90),
        ];
        let tail = timeline_tail(newest_first.into_iter());
        let kinds: Vec<&str> = tail.iter().map(|n| n.kind).collect();
        assert_eq!(kinds, ["inbox", "cwd-change"]);
        assert!(tail.iter().all(|n| n.at_ms == 90), "each keeps its stamp");
        let mut input = full_input();
        input.timeline = [
            "agent-change",
            "task-change",
            "human",
            "agent-change",
            "cwd-change",
        ]
        .into_iter()
        .map(|kind| TimelineNote { kind, at_ms: NOW })
        .collect();
        let menu = menu_shown_at(&compose_tab_menu(&input), NOW);
        let rows: Vec<&str> = menu
            .iter()
            .filter_map(|e| match e {
                TabMenuEntry::Header(h) if h.ends_with("just now") => Some(h.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            [
                "task changed \u{b7} just now",
                "changed directory \u{b7} just now"
            ]
        );
    }

    #[test]
    fn menu_matches_tooltip_headers_and_orders_actions() {
        let mut input = full_input();
        input.connections = vec![fact(
            "s-peer",
            "build",
            ConnDirection::Outbound,
            ConnFlow::Push,
            false,
        )];
        let menu = menu_shown_at(&compose_tab_menu(&input), NOW);
        let tip = compose_tooltip(&input).unwrap();
        let tip_lines: Vec<&str> = tip.lines().filter(|l| !l.is_empty()).collect();
        let menu_headers: Vec<&str> = menu
            .iter()
            .filter_map(|e| match e {
                TabMenuEntry::Header(h) if h != "AGENT" => Some(h.as_str()),
                _ => None,
            })
            .collect();
        let tip_identity: Vec<&str> = tip_lines
            .iter()
            .copied()
            .filter(|l| !l.starts_with('\u{21e5}'))
            .collect();
        assert_eq!(&menu_headers[..tip_identity.len()], &tip_identity[..]);
        assert_eq!(
            menu_headers.len(),
            tip_identity.len() + 3,
            "+ the timeline tail"
        );
        let actions: Vec<MenuAction> = menu
            .iter()
            .filter_map(|e| match e {
                TabMenuEntry::Action { action, .. } => Some(*action),
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            vec![
                MenuAction::TakeOver,
                MenuAction::HarnessLog,
                MenuAction::SetRole,
                MenuAction::RenameSession,
                MenuAction::CopySessionId,
                MenuAction::CopyCwd,
                MenuAction::CloseTab,
            ]
        );
        assert_eq!(
            menu.iter()
                .filter(|e| matches!(e, TabMenuEntry::Separator))
                .count(),
            3,
            "identity | timeline | agent | actions"
        );
    }

    #[test]
    fn the_agent_section_offers_the_hold_and_the_team_rows() {
        let rows = |input: &SessionChromeInput| -> Vec<(String, ConnVerb)> {
            compose_tab_menu(input)
                .iter()
                .filter_map(|e| match e {
                    TabMenuEntry::ConnectionAction { peer_sid, verb, .. } => {
                        Some((peer_sid.as_str().to_string(), *verb))
                    }
                    _ => None,
                })
                .collect()
        };
        let first_agent_action = |input: &SessionChromeInput| {
            compose_tab_menu(input).iter().find_map(|e| match e {
                TabMenuEntry::Action { action, .. } => Some(*action),
                _ => None,
            })
        };
        let mut plain = full_input();
        plain.sid = Some(SessionId::new("s-me"));
        assert!(
            compose_tab_menu(&plain)
                .iter()
                .any(|e| matches!(e, TabMenuEntry::Header(h) if h == "AGENT"))
        );
        assert_eq!(first_agent_action(&plain), Some(MenuAction::TakeOver));
        assert!(rows(&plain).is_empty(), "no team, no team rows");

        let mut held = plain.clone();
        held.agent.person_hold = true;
        assert_eq!(first_agent_action(&held), Some(MenuAction::HandBack));

        let mut worker = plain.clone();
        worker.agent.manager = Some((SessionId::new("s-boss"), "lab-sweep".to_string()));
        worker.agent.stoppable = true;
        assert_eq!(
            rows(&worker),
            [
                ("s-boss".to_string(), ConnVerb::Show),
                ("s-me".to_string(), ConnVerb::StopWorker),
            ]
        );

        let mut manager = plain.clone();
        manager.agent.workers = vec![
            (SessionId::new("s-w1"), "bench".to_string()),
            (SessionId::new("s-w2"), "fmt".to_string()),
        ];
        let menu = compose_tab_menu(&manager);
        let header = menu
            .iter()
            .position(|e| matches!(e, TabMenuEntry::Header(h) if h.starts_with("Workers")))
            .expect("the workers header");
        assert!(matches!(
            &menu[header],
            TabMenuEntry::Header(h) if h.contains('2')
        ));
        assert_eq!(
            rows(&manager),
            [
                ("s-w1".to_string(), ConnVerb::Show),
                ("s-w2".to_string(), ConnVerb::Show),
            ]
        );
    }

    #[test]
    fn the_tooltip_carries_connection_lines_and_no_forged_one() {
        let mut peers = full_input();
        peers.connections = vec![
            fact(
                "s-b",
                "claude (b)",
                ConnDirection::Peer,
                ConnFlow::Push,
                false,
            ),
            fact(
                "s-c",
                "build",
                ConnDirection::Outbound,
                ConnFlow::Push,
                true,
            ),
        ];
        let tip = compose_tooltip(&peers).unwrap();
        assert!(tip.contains("\u{21c6} peer \"claude (b)\""));
        assert!(tip.contains("\u{21e5} pushes into \"build\" \u{00b7} live"));
        let mut hostile = full_input();
        hostile.connections = vec![fact(
            "s-evil",
            "a\u{202e}b\nAGENT",
            ConnDirection::Outbound,
            ConnFlow::Pull,
            false,
        )];
        let tip = compose_tooltip(&hostile).unwrap();
        assert!(
            tip.lines().any(|l| l == "\u{21e5} pulls from \"abAGENT\""),
            "controls/bidi stripped, no forged extra line: {tip:?}"
        );
    }

    #[test]
    fn bare_session_menu_greys_unavailable_copies() {
        let input = SessionChromeInput {
            label: "zsh".to_string(),
            ..SessionChromeInput::default()
        };
        let menu = compose_tab_menu(&input);
        assert_eq!(menu[0], TabMenuEntry::Header("zsh".to_string()));
        assert_eq!(menu[1], TabMenuEntry::Separator);
        assert_eq!(
            menu.iter()
                .filter(|e| matches!(e, TabMenuEntry::Separator))
                .count(),
            2
        );
        let greyed = |a: MenuAction| {
            menu.iter().any(|e| {
                matches!(e, TabMenuEntry::Action { action, enabled: false, .. } if *action == a)
            })
        };
        for a in [
            MenuAction::TakeOver,
            MenuAction::HarnessLog,
            MenuAction::SetRole,
            MenuAction::RenameSession,
            MenuAction::CopySessionId,
            MenuAction::CopyCwd,
        ] {
            assert!(greyed(a), "{a:?} greys without a session");
        }
        assert!(menu.iter().any(|e| matches!(
            e,
            TabMenuEntry::Action {
                action: MenuAction::CloseTab,
                enabled: true,
                ..
            }
        )));
    }

    #[test]
    fn chrome_line_mirrors_composed_model() {
        let mut input = SessionChromeInput {
            label: "zsh".to_string(),
            cwd: Some("/tmp".to_string()),
            has_session: true,
            can_rename: true,
            sid: Some(SessionId::new("s-me")),
            ..SessionChromeInput::default()
        };
        input.agent.manager = Some((SessionId::new("s-boss"), "boss".to_string()));
        let menu = compose_tab_menu(&input);
        let line = tab_menu_chrome_line(2, &menu, NOW);
        let items: Vec<String> = menu_shown_at(&menu, NOW)
            .iter()
            .map(menu_entry_mirror)
            .collect();
        assert_eq!(line, format!("tab-menu tab=2 items={items:?}"));
        assert_eq!(items.iter().filter(|t| *t == "---").count(), 2);
        assert!(items.iter().any(|t| t.ends_with(" @s-boss")), "{items:?}");
        input.has_session = false;
        let greyed: Vec<String> = compose_tab_menu(&input)
            .iter()
            .map(menu_entry_mirror)
            .collect();
        assert!(
            greyed.iter().any(|t| t.ends_with(" (disabled)")),
            "{greyed:?}"
        );
    }

    #[test]
    fn a_pending_upgrade_offers_the_owners_words_for_its_build() {
        use aterm_messages::UpgradeWord::{NotToday, Now, Skip};
        let mut input = SessionChromeInput {
            label: "claude".to_string(),
            has_session: true,
            ..SessionChromeInput::default()
        };
        let plain = compose_tab_menu(&input);
        input.upgrade = Some(UpgradeMenu {
            sid: "s-b5cf2faabac5ce5127bd".to_string(),
            moving: "Claude Code 2.1.281 → 2.1.282".to_string(),
            to: "2.1.282".to_string(),
            words: vec![Now, NotToday, Skip],
        });
        let menu = compose_tab_menu(&input);
        let start = menu
            .iter()
            .position(|e| *e == TabMenuEntry::Header("AGENT UPGRADE".to_string()))
            .expect("the section");
        assert_eq!(menu[start - 1], TabMenuEntry::Separator);
        assert_eq!(
            menu[start + 1],
            TabMenuEntry::Header("Claude Code 2.1.281 → 2.1.282".to_string())
        );
        let items: Vec<String> = menu[start + 2..start + 5]
            .iter()
            .map(menu_entry_mirror)
            .collect();
        assert_eq!(
            items,
            [
                "Upgrade Now (2.1.282)",
                "Not Today (2.1.282)",
                "Skip This Version (2.1.282)"
            ]
        );
        assert!(matches!(
            &menu[start + 4],
            TabMenuEntry::Upgrade { sid, to, word: Skip, .. }
                if to == "2.1.282" && sid == "s-b5cf2faabac5ce5127bd"
        ));
        assert_eq!(menu.len(), plain.len() + 6, "the section and nothing else");
        input.upgrade = Some(UpgradeMenu {
            words: Vec::new(),
            ..input.upgrade.clone().expect("set")
        });
        assert_eq!(compose_tab_menu(&input), plain, "no word: no section");
    }

    fn clause(head: &str, since_ms: u64, until_ms: Option<u64>) -> ClauseLine {
        ClauseLine {
            head: head.to_string(),
            since_ms,
            until_ms,
            by: None,
        }
    }

    #[test]
    fn a_shown_label_names_its_author_after_its_words() {
        let mut input = full_input();
        let mut label = clause("restarting", NOW - 240_000, Some(NOW + 60_000));
        label.by = Some(ClauseAuthor::Harness);
        input.clause = Some(label.clone());
        assert!(
            compose_tooltip(&input)
                .expect("composes")
                .contains("\nnow: restarting \u{b7} by the harness\ncwd:"),
            "{:?}",
            compose_tooltip(&input)
        );
        assert_eq!(
            menu_shown_at(&compose_tab_menu(&input), NOW)[3],
            TabMenuEntry::Header("now: restarting \u{b7} by the harness \u{b7} 4m ago".to_string())
        );
        label.by = Some(ClauseAuthor::You);
        input.clause = Some(label.clone());
        assert!(
            compose_tooltip(&input)
                .expect("composes")
                .contains("\nnow: restarting \u{b7} by you\n")
        );
        let head = "x".repeat(48);
        label.head.clone_from(&head);
        input.clause = Some(label.clone());
        assert!(
            compose_tooltip(&input)
                .expect("composes")
                .contains(&format!("\nnow: {head} \u{b7} by you\n"))
        );
        label.by = None;
        input.clause = Some(label);
        let bare = compose_tooltip(&input).expect("composes");
        assert!(bare.contains(&format!("\nnow: {head}\ncwd:")), "{bare:?}");
        assert!(!bare.contains("\u{b7} by "), "{bare:?}");
    }

    #[test]
    fn the_clause_is_a_now_line_after_activity_and_an_aged_menu_row() {
        let plain = full_input();
        let mut input = full_input();
        input.clause = Some(clause("running shell", NOW - 240_000, None));
        assert_eq!(
            compose_tooltip(&input).expect("composes"),
            "🤖 build agent\n\
             description: Rebuilds the docs site\n\
             activity: Building documentation\n\
             now: running shell\n\
             cwd: ~/src/aterm"
        );
        let menu = compose_tab_menu(&input);
        let without = compose_tab_menu(&plain);
        assert_eq!(menu.len(), without.len() + 1, "one row and nothing else");
        assert_eq!(
            menu[3],
            TabMenuEntry::Event {
                words: "now: running shell".to_string(),
                at_ms: NOW - 240_000,
            }
        );
        let mut rest = menu.clone();
        rest.remove(3);
        assert_eq!(rest, without, "every other row is today's");
        assert_eq!(
            menu_shown_at(&menu, NOW)[3],
            TabMenuEntry::Header("now: running shell \u{b7} 4m ago".to_string()),
            "the row is aged where it is shown"
        );
        input.activity = None;
        assert!(
            compose_tooltip(&input)
                .expect("composes")
                .contains("description: Rebuilds the docs site\nnow: running shell\ncwd:")
        );
        assert!(!compose_tooltip(&plain).expect("composes").contains("now:"));
        assert!(
            !without
                .iter()
                .any(|e| menu_entry_mirror(e).starts_with("now:"))
        );
    }

    #[test]
    fn the_clauses_words_are_sanitized_like_every_header() {
        let mut input = full_input();
        input.clause = Some(clause("ship\u{202e}ping\u{7}", NOW, None));
        let tip = compose_tooltip(&input).expect("composes");
        assert!(tip.contains("\nnow: shipping\n"), "{tip:?}");
        assert!(!tip.contains('\u{202e}') && !tip.contains('\u{7}'));
        input.clause = Some(clause("\u{202e}\u{7}", NOW, None));
        assert_eq!(compose_tooltip(&input), compose_tooltip(&full_input()));
        assert_eq!(compose_tab_menu(&input), compose_tab_menu(&full_input()));
    }

    #[test]
    fn a_shown_labels_until_bounds_the_compositions_expiry() {
        let mut input = full_input();
        assert_eq!(clock_expiry_ms(NOW, &input), None, "off and quiet: none");
        input.clause = Some(clause("running shell", NOW - 1_000, None));
        assert_eq!(clock_expiry_ms(NOW, &input), None, "a step arms nothing");
        input.clause = Some(clause("compiling", NOW - 1_000, Some(NOW + 9_000)));
        assert_eq!(clock_expiry_ms(NOW, &input), Some(NOW + 9_000));
        input.upgrade = Some(UpgradeMenu {
            sid: "s-1".to_string(),
            moving: "Claude Code 2.1.281 → 2.1.282".to_string(),
            to: "2.1.282".to_string(),
            words: Vec::new(),
        });
        assert_eq!(
            clock_expiry_ms(NOW, &input),
            Some(NOW + 9_000),
            "the earlier of the two"
        );
        input.clause = Some(clause("compiling", NOW, Some(NOW + 2 * CACHE_MAX_AGE_MS)));
        assert_eq!(clock_expiry_ms(NOW, &input), Some(NOW + CACHE_MAX_AGE_MS));
        input.clause = None;
        assert_eq!(
            clock_expiry_ms(NOW, &input),
            Some(NOW + CACHE_MAX_AGE_MS),
            "off: exactly the upgrade's term"
        );
    }
}
