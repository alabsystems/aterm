// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session UI CHROME composer (session-metadata stage 2): the ONE pure
//! source of truth for what a terminal tab's HOVER TOOLTIP and RIGHT-CLICK
//! CONTEXT MENU say about a session.
//!
//! Stage 1 ([`crate::session_timeline`]) gave every session a user identity
//! (`meta set title|description|icon|role|attention`) and a bounded lifecycle timeline. This
//! module turns those — plus the session-status activity line, the engine's cwd,
//! and the registry's lifecycle state — into TWO renderings of the same identity
//! facts:
//!
//! * [`compose_tooltip`] — the multi-line text applied to a macOS strip
//!   [`crate::toolbar`] `TabView` via `setToolTip:` (and carried on
//!   [`crate::tab_model::TabPresentation::tooltip`], the field the settings/
//!   markdown/editor tabs already populate).
//! * [`compose_tab_menu`] — the context-menu MODEL a right-click / ctrl-click on
//!   a tab chip pops as a native `NSMenu`: disabled identity headers, the recent
//!   timeline tail, then the actions (`Copy Session ID` / `Copy CWD` /
//!   `Close Tab`, routed through the existing [`MenuAction`] tag dispatch).
//!
//! One composer feeding both surfaces is the point: the tooltip can never say
//! something the menu doesn't (the menu adds the recent timeline tail, a log
//! rather than a glance), and the `chrome` introspection verb's
//! [`tab_menu_chrome_line`] mirror (read off the live strip) is provably the
//! same items a human sees, because there is exactly one place items are made.
//!
//! PURE by construction — no locks, no AppKit, no clocks (a timeline row
//! carries its event's stamp and is aged against an EXPLICIT `now_ms` where a
//! surface shows it, [`TabMenuEntry::shown_at`]; the `~`-abbreviation takes
//! an EXPLICIT `home` like
//! [`crate::app_tabs::home_relative_suffix`], which it reuses) — so every
//! ordering/truncation/degradation rule is unit-proved headlessly, on every
//! platform. The impure work (leaf-lock reads, epoch caching) lives in
//! `App::composed_session_chrome` (`app_tabs.rs`).

use aterm_session::SessionId;

use crate::menu::MenuAction;

/// How many timeline events the context menu shows (the tooltip shows none) —
/// the "recent" TAIL, newest-first. Small on purpose: the `timeline` verb
/// serves the full ring.
pub(crate) const TIMELINE_TAIL: usize = 5;

/// Display cap for authored description and status activity prose (GRAPHEME
/// CLUSTERS, not bytes or `char`s — the cap must never split a user-perceived
/// glyph, and a ZWJ emoji or combining sequence spans several `char`s).
/// `meta set description` stores up to 1024 BYTES; a tooltip/menu header is a
/// one-liner, so anything longer is shown truncated with an ellipsis.
/// Truncation here is DISPLAY-only — the durable stored value is untouched (the
/// `meta` verb still returns it whole).
pub(crate) const DESCRIPTION_DISPLAY_MAX: usize = 160;

/// Everything the composer knows about one session, gathered by the caller
/// under its own (leaf) locks and handed over as plain data. `label` is the
/// already-resolved tab label (the `tab_titles` chain: user title ▸ OSC title ▸
/// cwd), so the tooltip's first line always matches the chip it hovers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionChromeInput {
    /// The resolved tab label — the chip text this chrome annotates.
    pub label: String,
    /// `meta set icon` token (emoji / short name), prefixed onto the title line.
    pub icon: Option<String>,
    /// `meta set description` free text (display-truncated, see the cap).
    /// This is durable, user-authored identity and is never replaced by the
    /// activity line.
    pub description: Option<String>,
    /// The live activity line — Tab Subject & Status's words
    /// (`App::session_status_text`). This is transient presentation data,
    /// kept distinct from the user-authored [`Self::description`]. When no
    /// authored description exists it supplies the descriptive header line;
    /// when both exist the composer renders both with honest labels.
    pub activity: Option<String>,
    /// The engine's reported cwd (OSC 7 / shell integration), RAW — the
    /// composer abbreviates it against `home` for display, while `Copy CWD`
    /// copies the raw form (a pasted path must be real, never `~`-relative).
    pub cwd: Option<String>,
    /// The `$HOME` to abbreviate against (explicit, like
    /// [`crate::app_tabs::home_relative_suffix`], so tests need no env).
    pub home: Option<String>,
    /// The registry's lifecycle state (`spawning`/`alive`/`exited`/…), `None`
    /// when the session is not registered (a stub) — the line is then omitted.
    pub state: Option<String>,
    /// Whether a registry identity exists — gates the `Copy Session ID` action
    /// (greyed out rather than copying an empty string).
    pub has_session: bool,
    /// The session's registry sid, when it has one: the AGENT section's Stop
    /// Worker row carries it as its argument.
    pub sid: Option<SessionId>,
    /// Whether this window can PRESENT a rename editor at all. Off macOS the
    /// editor is painted by the tab strip, so `tab_strip_rows = 0` leaves the
    /// command with nowhere to go and the row greys out instead of accepting a
    /// click that only reaches a log line.
    pub can_rename: bool,
    /// The recent timeline tail, NEWEST-FIRST, already cut to the rows it
    /// shows and capped to [`TIMELINE_TAIL`] by the caller ([`timeline_tail`];
    /// the composer re-applies both defensively).
    pub timeline: Vec<TimelineNote>,
    /// This session's live SESSION CONNECTIONS (design §2.3/§4), one fact per
    /// peer, gathered by the caller from the in-process edge tables (the
    /// authority record — never lineage, never the token store) and already
    /// sorted for a stable listing. Empty = no CONNECTIONS rows beyond the
    /// always-present spawn presets / picker entries.
    pub connections: Vec<ConnectionFact>,
    /// The LIVE AGENT UPGRADE of the agent this tab runs, as its menu offers
    /// the owner's words on it (gap #21), from the window's view of the
    /// harness host's rows; `None` when none is recorded for the tab or no
    /// word would do anything.
    pub upgrade: Option<UpgradeMenu>,
    /// WHY the tab's attention mark stands: the session's `meta set attention`
    /// text, as the caller read it (the composer sanitizes and caps it again).
    /// The mark alone says "needs attention"; the hover names the reason, so a
    /// mark that stays is never a riddle. `None` = no reason recorded.
    pub attention: Option<String>,
    /// What the menu's AGENT section offers for this session
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4): the person's hold, and
    /// its manager or its workers, gathered by the caller from the holds and
    /// the task records.
    pub agent: AgentMenu,
    /// THE SESSION LOG'S CLAUSE (phase 3B, plan §5; addendum A12; on by
    /// default since D6): `now: <its head words>` after `activity:`, and a
    /// shown label's author after them. `None` with `[session_log] clause =
    /// false`, and then every line and row is as it was before the clause,
    /// byte for byte.
    pub clause: Option<ClauseLine>,
}

/// The facts the tab menu's AGENT section is composed from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentMenu {
    /// Hand Back has a hold to lift — the person's, or an Owner client's
    /// where no person's stands (`fabric::Holds::hand_back_origin`): the row
    /// is Hand Back, else Take Over.
    pub person_hold: bool,
    /// The manager this session's task names, when this aterm hosts it:
    /// `(its sid, its label)` — the `Go to <label>` row.
    pub manager: Option<(SessionId, String)>,
    /// The session runs a task that is not over: Stop Worker… is offered.
    pub stoppable: bool,
    /// The workers whose tasks THIS session started: `(sid, its row's words)`,
    /// in session order — the `Workers (n)` rows.
    pub workers: Vec<(SessionId, String)>,
}

/// The session log's clause as the chrome shows it ([`SessionChromeInput::clause`]):
/// the tooltip's `now: <head>` line, no age and no minutes left (a clock-free
/// cache cannot keep either true), and the menu's row aged as it pops
/// ([`TabMenuEntry::Event`]: `now: running shell · 4m ago`). A shown label
/// says whose it is after its words (`now: restarting · by the harness`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClauseLine {
    /// The clause's words without their coarse age (`aterm_slog::Clause::head`).
    pub head: String,
    /// When its state began, on [`crate::turn_ledger::now_ms`]'s clock.
    pub since_ms: u64,
    /// When a shown label leaves (its `until`, on the same clock), the clause
    /// being that label; `None` for every other row (ruling S10).
    pub until_ms: Option<u64>,
    /// A shown label's author (`aterm_slog::Clause::by` on a label row, D6):
    /// printed after the head, outside its 48 bytes. `None` for every other
    /// row: a need, a step, a turn or a run says no author (ruling U22).
    pub by: Option<ClauseAuthor>,
}

/// Who wrote a label the clause shows: only a proven author's label shows
/// (`aterm_slog::shown_proven_label`), so it is the harness or the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClauseAuthor {
    /// The window's harness (`by=harness`).
    Harness,
    /// The person at the window (`by=human`).
    You,
}

impl ClauseAuthor {
    /// The author of a label written by `by`; `None` for an author whose
    /// label never shows.
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

/// What the chrome cache keys the clause on (addendum A12): its row, its head
/// words, its tone, whose it is and when its state began — what moves only
/// when the state does, never with the clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClauseKey {
    pub row: aterm_slog::ClauseRow,
    pub head: String,
    pub tone: aterm_slog::Tone,
    pub by: aterm_slog::Actor,
    pub since_wall: u64,
}

/// One tab's live agent upgrade, as its context menu offers it (gap #21):
/// the same words, for the same build, as the upgrade's band row
/// (`message_reporters::agent_upgrade_words`), all of them — a row has room
/// for two capsules, a menu for every word.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpgradeMenu {
    /// The session the upgrade is on (`s-<hex>`): the pane the menu was
    /// composed for, which its words name ([`TabMenuEntry::Upgrade`]).
    pub sid: String,
    /// What it moves, in the band's words (`Claude Code 2.1.281 → 2.1.282`).
    pub moving: String,
    /// The target build the words are for.
    pub to: String,
    /// The words that do something for it, most useful first.
    pub words: Vec<aterm_messages::UpgradeWord>,
}

/// An upgrade word's menu item (title case, the menu's own voice — a menu
/// item has no capsule's 16-character bound, so "Skip This Version" whole).
#[must_use]
pub(crate) fn upgrade_menu_label(word: aterm_messages::UpgradeWord) -> &'static str {
    match word {
        aterm_messages::UpgradeWord::Now => "Upgrade Now",
        aterm_messages::UpgradeWord::NotToday => "Not Today",
        aterm_messages::UpgradeWord::Skip => "Skip This Version",
    }
}

/// Which way the AUTHORITY of a connection points, seen from the session whose
/// chrome is being composed ("S"). A row lives in its DESTINATION's table, so:
/// `Outbound` = S holds ops over the peer, `Inbound` = the peer holds ops over
/// S, `Peer` = both directions exist (the §2.3 `⇆` pair).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnDirection {
    Outbound,
    Inbound,
    Peer,
}

/// The push/pull face of a connection's op set — the family-row rung (§6):
/// `write-input` present ⇒ Push (it can type), else Pull (read-only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnFlow {
    Push,
    Pull,
}

/// One connected peer as the chrome shows it — plain data (the caller resolves
/// titles/directions under its own locks; the composer renders). `live` is
/// BEST-EFFORT: today's lease/watcher state names control connections, not
/// peer sessions, so per-peer traffic cannot be attributed honestly — callers
/// pass `false` (absent = not live) until a per-connection activity seam
/// exists; the rendering already supports the flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionFact {
    /// The peer's registry sid — what the [`TabMenuEntry::ConnectionAction`]
    /// rows target.
    pub peer_sid: SessionId,
    /// The peer's display title (user meta title ▸ registry title), or
    /// `unknown` for a foreign src no local session owns.
    pub peer_title: String,
    pub direction: ConnDirection,
    /// The [`ConnFlow`] of the S-relative half: the S→peer half for
    /// `Outbound`/`Peer`, the peer→S half for `Inbound`. (`Peer` renders the
    /// bare `⇆` line, direction-symmetric, so its flow is not shown.)
    pub kind: ConnFlow,
    /// Whether the connection is actively driving right now. Best-effort —
    /// see the struct doc; `false` means "not known live", never "idle".
    pub live: bool,
}

/// One timeline event as the chrome shows it: the kind token (printed in a
/// person's words, [`event_words`]) plus when it happened. The payload is
/// deliberately NOT shown — hover chrome is a pulse ("what happened
/// lately"), the `timeline` verb is the detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TimelineNote {
    /// The event kind token (`spawned`, `cwd-change`, `meta-change`, …).
    pub kind: &'static str,
    /// When the event was recorded, on its own monotonic clock
    /// ([`crate::turn_ledger::now_ms`]): the menu row carries it and is aged
    /// where it is shown ([`TabMenuEntry::Event`]).
    pub at_ms: u64,
}

/// One entry of the composed context menu — the platform-neutral MODEL the
/// macOS strip renders as `NSMenuItem`s and the `chrome` verb serialises via
/// [`tab_menu_chrome_line`]. Mirrors [`crate::menu::MENU_MODEL`]'s
/// philosophy: the description IS the menu; native code only renders it.
/// Deliberately FLAT — no submenu variant (design R15): per-peer sub-actions
/// are flat rows under a per-peer [`TabMenuEntry::Header`], which keeps the
/// serialization's flat `items=[…]` contract and the NSMenu render loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TabMenuEntry {
    /// A disabled, informational row (identity / connection lines).
    Header(String),
    /// A disabled timeline row: its event in a person's words and when it
    /// happened ([`crate::turn_ledger::now_ms`]'s clock). Its age is the
    /// surface's to read, AS IT SHOWS THE ROW ([`Self::shown_at`]: `changed
    /// directory · 2m ago`) — the native menu as it pops, the in-grid card
    /// as it opens, the `chrome` verb's mirror as it is read — so a
    /// composition is the same at every instant and the clock never wakes the
    /// loop to recompose a closed menu (the Together run, 2026-10-16: a timer
    /// wake every 30 s per tab re-deriving ages nobody was reading).
    Event { words: String, at_ms: u64 },
    /// A native separator.
    Separator,
    /// A live command row: `action`'s tag is what the `NSMenuItem` carries, so
    /// the click dispatches through the SAME tag→[`MenuAction`] decode the menu
    /// bar uses (never a parallel path). `enabled: false` renders greyed
    /// (e.g. `Copy CWD` with no reported cwd). `label` is owned so live rows
    /// can name peers (design §2.2 [v5]); the fixed rows pay one small clone
    /// per compose, which is already epoch-gated.
    Action {
        label: String,
        action: MenuAction,
        enabled: bool,
    },
    /// A per-peer connection row (design §2.2 [v5]): Show / Configure… /
    /// Disconnect against `peer_sid`. NOT tag→[`MenuAction`] dispatched — the
    /// action needs a peer ARGUMENT, so the native item's tag is an index into
    /// the pop-time snapshot (the `menu_tab` precedent) and the click posts
    /// `Wake::TabMenuConnection { window, tab, peer_sid, verb }`.
    ConnectionAction {
        label: String,
        peer_sid: SessionId,
        verb: ConnVerb,
        enabled: bool,
    },
    /// The owner's word on this tab's live agent upgrade (gap #21): Upgrade
    /// Now / Not Today / Skip This Version, for the build `to` the menu
    /// names. Carries its arguments like a connection row, for the same
    /// reason: the native item's tag indexes the strip's pop-time snapshot
    /// and the click posts `Wake::TabMenuUpgrade`, so a click after the
    /// upgrade moved on names the build the owner SAW — which the harness
    /// then refuses (`upgrade_drive::ask_for`) rather than applying the word
    /// to a newer one. And the SESSION it saw: a split tab runs one per
    /// pane and the menu is composed for the focused one, which can change
    /// before the choice lands — the word is for `sid`, never for whichever
    /// pane holds the focus then. Offered only when it does something:
    /// always enabled.
    Upgrade {
        label: String,
        sid: String,
        to: String,
        word: aterm_messages::UpgradeWord,
    },
}

/// The closed verb vocabulary of a [`TabMenuEntry::ConnectionAction`] row:
/// an act that needs a session ARGUMENT (`peer_sid`), dispatched by
/// `App::dispatch_tab_menu_connection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnVerb {
    /// Raise the window hosting the session and select its tab (the `raise`
    /// wire verb's UI twin): `Go to <manager>`, a worker row.
    Show,
    /// Stop Worker…: after the person confirms the sheet (or, with no sheet
    /// to ask on, the Overview's inline confirm), SIGTERM to the session's
    /// agent, in process (`App::stop_worker`); its task reads `exited`.
    StopWorker,
}

/// The per-tab chrome EXTENSION the app pushes to the native strip alongside
/// titles/metadata: the composed tooltip + context-menu model. `Default` (no
/// tooltip, empty menu) is the non-terminal-tab / unknown-session shape —
/// exactly today's behavior, so native tabs and stubs degrade cleanly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabChromeExt {
    /// Tooltip text for the tab chip (`None` = no tooltip installed).
    pub tooltip: Option<String>,
    /// Context-menu model (`empty` = no context menu pops).
    pub menu: Vec<TabMenuEntry>,
}

/// One session's cached composed chrome + the input EPOCHS it was composed
/// from — the `App::session_chrome` map's value. Plain data here (next to the
/// [`TabChromeExt`] it caches); the reuse policy lives at the one write site,
/// [`crate::App::composed_session_chrome`].
pub(crate) struct CachedChrome {
    /// The session timeline's high-water event id at compose time. Every
    /// composed fact (meta / cwd / state) records a timeline event, so an
    /// unchanged id means unchanged facts.
    pub high_id: u64,
    /// The resolved tab label at compose time — covers OSC-title drift, which
    /// (for a BACKGROUND tab) may move without a timeline record.
    pub label: String,
    /// The session-status revision at compose time. The activity line can
    /// change without a session-timeline event and is never part of the label,
    /// so it must be an independent cache epoch rather than relying on
    /// `high_id` or `label`.
    pub activity_revision: u64,
    /// The connection store's revision at compose time (design §2.4 [v5]): a
    /// connection mint/revoke/sweep moves NONE of the other epochs, so without
    /// this the menu would list a revoked connection (and the tooltip its
    /// line) for up to [`CACHE_MAX_AGE_MS`] while the §4 marks already
    /// cleared. Bumped by every authority act — see
    /// `crate::connections::ConnectionTable::bump_revision`.
    pub connections_revision: u64,
    /// The window's upgrade view's revision at compose time
    /// (`upgrade_host::UpgradeView::revision`): the harness host's rows move
    /// none of the other epochs, and the menu's upgrade words are theirs.
    pub upgrade_revision: u64,
    /// When the clock may move this composition with no input epoch moving
    /// ([`clock_expiry_ms`], from [`crate::turn_ledger::now_ms`] at compose
    /// time): the instant the scheduler recomposes it, or `None` when nothing
    /// it says moves with the clock — no wake.
    pub expires_ms: Option<u64>,
    /// The session log's clause at compose time ([`ClauseKey`]), `None` with
    /// `[session_log] clause` off or no clause: a clause that moved, or a
    /// switch that flipped, misses the cache.
    pub clause: Option<ClauseKey>,
    /// The clause line composed from it, served again when the timeline is
    /// contended at the next compose (ruling S16).
    pub clause_line: Option<ClauseLine>,
    /// The composed extension handed to the native strip.
    pub ext: TabChromeExt,
}

/// How long a cached composition the clock moves (an upgrade's words) may
/// serve before it is re-derived with unchanged inputs. The event loop arms
/// the earliest cache expiry ([`CachedChrome::expires_ms`],
/// [`clock_expiry_ms`]) and drains due entries in bounded batches, so this
/// bounds their drift without per-frame work — and a composition the clock
/// cannot move arms nothing.
pub(crate) const CACHE_MAX_AGE_MS: u64 = 30_000;

/// Maximum real window inspections/toolbar refreshes performed by the expiry
/// fan-out in one event-loop turn. The cursor retains the unvisited remainder.
pub(crate) const EXPIRY_WINDOW_SCAN_BUDGET: usize = 1;

/// Observable projection of one real expiry scheduler turn. Tier-1 binds these
/// counts to the derived model's `work` variable; unlike a due-session count,
/// they measure the actual window traversal/rebuild boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ExpiryProgress {
    pub(crate) admitted_session: Option<u64>,
    pub(crate) window_scans: usize,
    pub(crate) window_refreshes: usize,
    pub(crate) completed_session: Option<u64>,
}

/// THE CLOCK'S PART IN A COMPOSITION: when the one composed at `composed_ms`
/// from `input` may read differently though no input epoch moves, or `None`
/// when it never does. The timeline rows' ages are no part of it: a row
/// carries its event's stamp and is aged where it is shown
/// ([`TabMenuEntry::Event`]). What is left is the upgrade's words
/// (`upgrade_host::UpgradeView::menu_for` reads them against the clock: a
/// stall, a deferral running out), re-derived every [`CACHE_MAX_AGE_MS`]
/// while a tab has one. A tab with none arms no wake: a quiet desktop's
/// tabs, on screen or not, cost the event loop nothing (the Together run,
/// 2026-10-16, measured one timer wake every 30 s per tab recomposing a
/// closed menu's ages).
///
/// With `[session_log] clause` on, a label the clause shows leaves at its
/// `until` (ruling S10): the expiry is the earlier of the upgrade's term and
/// that instant. Off (no clause), it is exactly the upgrade's term.
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

/// Earliest absolute process-clock expiry represented by the cache, or `None`
/// when no retained composition moves with the clock (the event loop can
/// return to pure Wait).
#[must_use]
pub(crate) fn next_cache_expiry_ms(
    cache: &std::collections::HashMap<u64, CachedChrome>,
) -> Option<u64> {
    cache.values().filter_map(|c| c.expires_ms).min()
}

/// Deterministic bounded set of entries due at `now_ms`, ordered by deadline
/// then session id. Selection does not mutate: the App removes each selected
/// entry immediately before recomposing every window that consumes it.
#[must_use]
pub(crate) fn due_cache_batch(
    cache: &std::collections::HashMap<u64, CachedChrome>,
    now_ms: u64,
    budget: usize,
) -> Vec<u64> {
    if budget == 0 {
        return Vec::new();
    }
    // Keep only the earliest `budget` candidates while scanning: a synchronized
    // thousand-tab expiry still allocates O(Budget), never O(due entries). The
    // buffer starts empty rather than pre-reserved because the push guard below
    // is what enforces that bound — reserving up front only moved the same
    // O(Budget) allocation earlier, and this runs on every event-loop park where
    // the overwhelmingly common answer (30 s TTL) is "nothing due", so the
    // lazy shape leaves the park allocation-free. Selection order and the
    // returned value are unchanged.
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

/// Render an age delta as the coarse human token the chrome shows. Coarse on
/// purpose: the tooltip is rebuilt on input-epoch changes (not per frame), so a
/// second-precise age would read as authoritative while being stale — a coarse
/// bucket stays honest for the whole gap between rebuilds.
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

/// The `~`-abbreviated display cwd, reusing the PROVEN component-boundary
/// matcher [`crate::app_tabs::home_relative_suffix`] (a sibling like
/// `/Users//foobar` under `home=/Users//foo` stays verbatim). `None` home or a
/// foreign path shows the raw path.
fn display_cwd(cwd: &str, home: Option<&str>) -> String {
    let display = match home.and_then(|h| crate::app_tabs::home_relative_suffix(cwd, h)) {
        Some(rest) => format!("~{rest}"),
        None => cwd.to_string(),
    };
    // OSC 7 / OSC 633 cwd is program-supplied and can legally name files with
    // controls on Unix. Keep that exact value for Copy CWD, but never let it
    // forge tooltip/menu rows or apply bidi controls in native chrome. The
    // display-only cap also keeps a valid PATH_MAX-sized directory from turning
    // a hover card into thousands of glyphs.
    let display = crate::session_timeline::sanitize_presentation_line(
        &display,
        crate::session_timeline::META_DESCRIPTION_MAX,
    );
    display_description(&display)
}

/// The identity title line: `<icon> <label>` when an icon token is set, else
/// the bare label. Shared verbatim by the tooltip's first line and the menu's
/// first header, so the two surfaces always open with the same identity.
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

/// One descriptive prose line, DISPLAY-truncated to
/// [`DESCRIPTION_DISPLAY_MAX`] user-perceived characters with a trailing
/// ellipsis. The unit is GRAPHEME CLUSTERS (via the workspace's own segmenter —
/// the same one the core grid cells use), not `char`s: a `char` cut is
/// UTF-8-safe but can land INSIDE a ZWJ emoji sequence or between a base letter
/// and its combining accent, rendering a mangled partial glyph in the tooltip /
/// menu header / `chrome` mirror. A cluster is indivisible on screen, so the
/// cut must be too.
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

/// ONE connection's display line, shared VERBATIM by the tooltip and the
/// menu's per-peer header (design §2.3/§4 — one composer, two surfaces):
/// `⇥ pushes into "build" · live` / `⇤ pulled by "operator"` /
/// `⇆ peer "claude (b)"`. The glyph encodes the AUTHORITY direction (⇥ this
/// session holds ops over the peer, ⇤ the peer holds ops over it, ⇆ both);
/// the verb encodes the [`ConnFlow`] rung (write-input ⇒ push). Titles are
/// program-influenced text, so they take the same sanitize+cap path as every
/// other prose line.
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

/// The per-connection lines in the input's (caller-sorted) order — appended to
/// the tooltip's identity block and rendered as the menu's per-peer headers.
fn connection_lines(input: &SessionChromeInput) -> Vec<String> {
    input.connections.iter().map(connection_line).collect()
}

/// The shared identity HEADER LINES (title, durable description, status
/// activity, cwd, the lifecycle state when it is news — `exited`, not the
/// `alive` every live tab is) — the one list both surfaces render. Authored
/// and observed prose are labeled so a transient status line can never
/// masquerade as durable session metadata. The newest-first timeline tail is
/// the menu's alone ([`timeline_rows`]).
///
/// Also where the clause goes (after `activity:`, before `cwd:`): the index
/// the tooltip's `now:` line and the menu's row are put at.
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
    let clause_at = identity.len();
    if let Some(cwd) = input.cwd.as_deref().filter(|c| !c.is_empty()) {
        let cwd = display_cwd(cwd, input.home.as_deref());
        if !cwd.is_empty() {
            identity.push(format!("cwd: {cwd}"));
        }
    }
    // The state is news unless it is the live default or the activity line
    // already says it (`activity: exited (exit 1)`).
    if let Some(state) = input.state.as_deref().filter(|s| {
        !s.is_empty()
            && !matches!(*s, "-" | "alive" | "spawning")
            && !input.activity.as_deref().is_some_and(|a| a.starts_with(s))
    }) {
        identity.push(format!("state: {state}"));
    }
    (identity, clause_at)
}

/// The clause's words as a line shows them (`now: <head>`, and a shown
/// label's ` · by the harness` or ` · by you` after them, outside the head's
/// 48 bytes), through the presentation sanitizer every header line takes (a
/// bidi or C1 character in a label is stripped); `None` when nothing is left.
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

/// The menu's newest-first timeline tail: each shown event
/// ([`timeline_row_shown`], at most [`TIMELINE_TAIL`]) as a row its surface
/// ages ([`TabMenuEntry::Event`]).
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
    /// This entry as a surface shows it at `now_ms`
    /// ([`crate::turn_ledger::now_ms`]'s clock): an [`Self::Event`] aged into
    /// its row (`changed directory · 2m ago`, [`relative_age`]), every other
    /// entry as it is.
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

/// A composed menu as a surface shows it at `now_ms`: every timeline row
/// aged ([`TabMenuEntry::shown_at`]).
#[must_use]
pub(crate) fn menu_shown_at(entries: &[TabMenuEntry], now_ms: u64) -> Vec<TabMenuEntry> {
    entries.iter().map(|e| e.shown_at(now_ms)).collect()
}

/// Whether a timeline event earns a row in the tab's hover chrome. An agent's
/// verdict moves, a program's own state reports (`program-state`, a codex's
/// turn ledger), a person's own typing, the bus's own traffic (a fetch, a
/// topic, a read receipt, a delivery receipt) and the resize ledger's
/// `render` verdict (a desync risk and its heal are one kind, told apart only
/// by the payload) say nothing without their payload, which this surface
/// never shows (the `timeline` verb does), and on an agent's or a fabric
/// session's tab they would fill the tail.
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
            // The card engine's dev rows (`crate::holder_image::DEV_ROWS`).
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
            // The footer resolver's dev row.
            | crate::claude_footer::REGISTRY_READ
    )
}

/// The rows the hover chrome shows, from a NEWEST-FIRST walk of the
/// session's `(kind, t_ms)` events: the newest [`TIMELINE_TAIL`] that earn a
/// row ([`timeline_row_shown`]), each with its stamp (aged where it is
/// shown, [`TabMenuEntry::shown_at`]). The cut runs before the cap, so the
/// rows left out never crowd out older ones that say something.
pub(crate) fn timeline_tail(
    newest_first: impl Iterator<Item = (&'static str, u64)>,
) -> Vec<TimelineNote> {
    newest_first
        .filter(|(kind, _)| timeline_row_shown(kind))
        .take(TIMELINE_TAIL)
        .map(|(kind, at_ms)| TimelineNote { kind, at_ms })
        .collect()
}

/// A timeline event kind in a person's words (`cwd-change` → `changed
/// directory`). A kind with no entry here is printed as recorded.
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

/// Compose the hover TOOLTIP for one terminal tab, or `None` when the session
/// carries NOTHING beyond its bare label (no authored description, status
/// activity, icon, cwd, exited state or connection) — the tab then keeps its
/// no-tooltip behavior instead of a tooltip that merely repeats the chip text.
/// Shape: the identity lines, then the per-connection lines (design §4: the
/// mark is never visual-only). The timeline tail is the context menu's alone:
/// raw event kinds (`spawned`, `meta-change`) are a log, not a glance, and the
/// strip joins the tooltip's lines into one help string.
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
        // Only the bare title line — nothing the chip doesn't already say.
        return None;
    }
    let mut lines = identity;
    lines.extend(attention);
    lines.extend(connections);
    Some(lines.join("\n"))
}

/// The tooltip line that names why a tab's attention mark stands starts with
/// this; the native hover ([`crate::toolbar`]) reads it to say the reason once.
pub(crate) const NEEDS_ATTENTION_PREFIX: &str = "Needs attention: ";

/// The hover's line for a harness's need in the worker's own words
/// (`App::presence_hover`, design §12.2.1) starts with this in place of
/// [`NEEDS_ATTENTION_PREFIX`]: the reason told, as that line is.
pub(crate) const NEEDS_YOU_PREFIX: &str = "Needs you: ";

/// The tooltip's `Needs attention: <reason>` line: the reason the tab's mark
/// stands, sanitized and capped at the `attention` field's own rules and then
/// display-truncated like the description. `None` when no reason is recorded
/// (the mark's own hover clause, `Needs attention`, still says THAT).
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

/// Compose the right-click CONTEXT-MENU model for one terminal tab: the same
/// identity lines as the tooltip as disabled headers, a separator, the
/// newest-first timeline tail (disabled), a separator, the AGENT section
/// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4, replacing CONNECTIONS), a
/// separator, then the actions. The menu ALWAYS exists (unlike the tooltip):
/// a bare unnamed session still owns `Copy Session ID` / `Copy CWD` / `Close
/// Tab`, with the unavailable copies greyed rather than hidden (a stable menu
/// shape is learnable). The AGENT section is always present — Take Over (or
/// Hand Back), Harness Log, Set Role… are its floor — and adds, on a worker,
/// `Go to <manager>` and Stop Worker…, and on a manager a `Workers (n)` header
/// with one row per worker, FLAT per design R15.
#[must_use]
pub(crate) fn compose_tab_menu(input: &SessionChromeInput) -> Vec<TabMenuEntry> {
    let (headers, clause_at) = header_lines(input);
    let mut entries: Vec<TabMenuEntry> = headers.into_iter().map(TabMenuEntry::Header).collect();
    // The clause, aged as the menu pops ([`TabMenuEntry::shown_at`]).
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
    // The live agent upgrade's words (gap #21), right under the identity the
    // headers above describe: the owner steers THIS tab's upgrade here.
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
    // The person's hold pair: the one that applies (`Take Over` and `Hand
    // Back` are the person's own; the clicked tab is their subject).
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
    // On a worker: its manager, and the stop.
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
    // On a manager: its workers, one row each, going there. Flat (design
    // R15): the header names the count, the rows follow.
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
    // FIRST because it is the identity action the headers above are about; the
    // clipboard pair and the close follow. Gated on `has_session` like the id
    // copy: the pin is SESSION metadata, so a tab with no session has nothing
    // to rename (and the write path would no-op silently).
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

/// Serialise one tab's context-menu model to the `chrome` verb's line —
/// `tab-menu tab=<i> items=["…", "---", …]` — the introspection mirror of what
/// a right-click on that chip pops. Separators print as `---`; a DISABLED
/// action is suffixed ` (disabled)` so a driving AI sees the same
/// greyed-vs-live distinction a human does. Headers print verbatim (their
/// disabled-ness is structural: they carry no action). A connection row stays
/// FLAT and carries its peer argument as `<label> @<sid>` — the control
/// layer's `@<sid>` addressing spelling, so a driving AI can read the target
/// straight off the mirror. Timeline rows are aged at `now_ms`, the instant
/// the mirror is read, as a right-click then would show them
/// ([`TabMenuEntry::shown_at`]).
#[must_use]
pub(crate) fn tab_menu_chrome_line(index: usize, entries: &[TabMenuEntry], now_ms: u64) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|entry| menu_entry_mirror(&entry.shown_at(now_ms)))
        .collect();
    format!("tab-menu tab={index} items={items:?}")
}

/// ONE entry's mirror token — the per-item serialization [`tab_menu_chrome_line`]
/// lists and the in-grid overlay's `controls tab-menu` rows reuse VERBATIM, so
/// the two introspection readings of one composed model can never spell an item
/// differently. Headers verbatim, separators `---`, disabled rows suffixed
/// ` (disabled)`, connection rows flat with their `@<sid>` peer argument.
#[must_use]
pub(crate) fn menu_entry_mirror(entry: &TabMenuEntry) -> String {
    let disabled_suffix = |enabled: bool| if enabled { "" } else { " (disabled)" };
    match entry {
        TabMenuEntry::Header(t) => t.clone(),
        // Aged by the surface before it is mirrored ([`menu_shown_at`]);
        // unaged, its words alone.
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
        // The build it is for rides the mirror, as a connection row's peer
        // does: a driving AI reads which target the word would name.
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

    /// The instant the tests' timelines are aged against
    /// ([`crate::turn_ledger::now_ms`]'s clock).
    const NOW: u64 = 1_000_000_000;

    /// THE CLOCK MOVES A COMPOSITION ONLY BY AN UPGRADE'S WORDS (D5, the
    /// Together run, and the review of ba16cabbe5): a timeline row carries
    /// its event's stamp and is aged where the menu is shown, so one
    /// composition reads `2m ago` now and `3m ago` a minute on and arms no
    /// wake; the upgrade's words read the clock, so a tab with one is
    /// re-derived every period; a cache entry with no expiry is never due.
    ///
    /// NEGATIVE CONTROL: the same input with an upgrade arms the period.
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
        // The `chrome` verb's mirror ages them as it is read.
        let line = tab_menu_chrome_line(0, &menu, NOW + 60_000);
        assert!(
            line.contains("\"changed directory \u{b7} 3m ago\""),
            "{line}"
        );
        // Negative control: the upgrade's words read the clock.
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

    /// The tooltip renders identity lines in the pinned order (icon+title,
    /// durable description, status activity, ~-abbreviated cwd) — one exact
    /// byte shape, so surfaces never drift apart silently. `alive` is every live
    /// tab's state, so it is no line; the timeline tail is the menu's.
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
        // A state that IS news keeps its line.
        let mut exited = full_input();
        exited.state = Some("exited".to_string());
        assert!(
            compose_tooltip(&exited)
                .unwrap()
                .ends_with("\ncwd: ~/src/aterm\nstate: exited")
        );
        // An activity line that already says it exited is the one exit line.
        exited.activity = Some("exited (exit 1)".to_string());
        let tip = compose_tooltip(&exited).unwrap();
        assert!(tip.contains("activity: exited (exit 1)"), "{tip}");
        assert!(!tip.contains("state: exited"), "{tip}");
        // A bare live session with a spawn event is only its label: no tooltip.
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

    /// The tab's hover names why its mark stands: a recorded reason is one
    /// `Needs attention: <reason>` line under the identity lines (before the
    /// connections), and is enough on its own to earn a tooltip.
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

    /// The reason is capped and stripped of controls like every other line:
    /// the stored `attention` is at most 256 bytes and the hover shows at most
    /// the description cap of grapheme clusters, ellipsised.
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

    /// A session with NOTHING beyond its label composes NO tooltip — exactly
    /// today's terminal-tab behavior (no tooltip that just repeats the chip).
    #[test]
    fn empty_metadata_degrades_to_no_tooltip() {
        let input = SessionChromeInput {
            label: "zsh".to_string(),
            ..SessionChromeInput::default()
        };
        assert_eq!(compose_tooltip(&input), None);
        // `-` (the registry's unregistered marker) and empty strings count as
        // absent, not as content worth a tooltip.
        let dashed = SessionChromeInput {
            label: "zsh".to_string(),
            state: Some("-".to_string()),
            icon: Some(String::new()),
            cwd: Some(String::new()),
            ..SessionChromeInput::default()
        };
        assert_eq!(compose_tooltip(&dashed), None);
    }

    /// REGRESSION (grapheme-cluster truncation): a multi-codepoint cluster
    /// straddling the cap must survive WHOLE or be dropped WHOLE — never split
    /// mid-sequence. The old `chars().take(cap)` cut a ZWJ family emoji at
    /// char 160, leaving a lone partial glyph (`👨…`) in the tooltip and menu
    /// header; same for a combining accent, which lost its diacritic.
    #[test]
    fn description_truncation_never_splits_a_grapheme_cluster() {
        // 159 ASCII chars, then a ZWJ family emoji (5 chars, ONE cluster):
        // 164 chars total but exactly 160 user-perceived characters — at the
        // cap, so it must render WHOLE with no ellipsis.
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

        // One cluster PAST the cap: the whole trailing emoji is dropped (never
        // a partial `👨` fragment) and the ellipsis follows a clean boundary.
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

        // A combining sequence (e + U+0301) straddling the cap: base and
        // accent travel together — the old cut kept the bare `e` and dropped
        // only the accent.
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

    /// The timeline tail is capped to [`TIMELINE_TAIL`] defensively even when
    /// the caller hands more — and it is the menu's: the tooltip carries none.
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

    /// An agent's verdict moves, a person's own typing, the bus's own traffic
    /// and a render verdict are no rows: without their payload they say
    /// nothing. The cut
    /// runs BEFORE the cap ([`timeline_tail`]), so a burst of them never
    /// hides the older rows that say something.
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
        // A worker's task moving is a row, in words: it is rare and it is news.
        assert_eq!(
            rows,
            [
                "task changed \u{b7} just now",
                "changed directory \u{b7} just now"
            ]
        );
    }

    /// The menu opens with the SAME header text the tooltip shows (one
    /// composer, two surfaces), plus the timeline tail; the `AGENT` marker is
    /// the one menu-only header, being structure rather than a fact. Then the
    /// AGENT rows and the actions, in order; four groups, three separators.
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
        // The tooltip carries the connection line; the menu does not (its
        // CONNECTIONS section went), and adds the timeline tail.
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

    /// THE AGENT SECTION per state (design §4): a plain session offers Take
    /// Over, the harness log and the role; under the person's hold the first
    /// row is Hand Back; a worker adds `Go to <manager>` (Show, the manager's
    /// sid) and Stop Worker… (on its own sid); a manager adds a `Workers (n)`
    /// header and one Show row per worker. Flat throughout (design R15).
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

    /// The tooltip still carries each connection line (§4: never
    /// visual-only), and a hostile peer title cannot forge a line.
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

    /// A bare session still gets a menu (label header + sections + actions),
    /// with the acts that need a session GREYED, not hidden — and only TWO
    /// separators (no empty timeline group; the AGENT section is always
    /// present because its floor is).
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

    /// The `chrome` verb's mirror line serialises EXACTLY the composed model:
    /// one token per entry, separators as `---`, disabled actions annotated,
    /// a session-argument row flat with its `@<sid>` — so the introspected
    /// listing IS the on-screen menu.
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

    /// A TAB WHOSE AGENT HAS AN UPGRADE PENDING OFFERS THE OWNER'S WORDS ON IT
    /// (gap #21): an `AGENT UPGRADE` section under the identity — what it
    /// moves, then every word that does something, each carrying the build it
    /// is for, which the `chrome` mirror prints. NEGATIVE CONTROLS: no
    /// upgrade, or one no word would move, composes no section at all.
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

    /// A clause as the composer is handed it (`SessionLog::chrome_clause`).
    fn clause(head: &str, since_ms: u64, until_ms: Option<u64>) -> ClauseLine {
        ClauseLine {
            head: head.to_string(),
            since_ms,
            until_ms,
            by: None,
        }
    }

    /// D6 (session log Phase 6 step 6B-2, ruling U22): a shown label's line
    /// names its author after its words, outside the head's 48 bytes, in the
    /// tooltip and in the menu's aged row: `by the harness` or `by you`. A
    /// row with no author (a need, a step, a turn, a run) names none.
    ///
    /// NEGATIVE CONTROL: the same label without its author is the bare line.
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
        // A head at the clause's full 48 bytes keeps every one of them; the
        // author follows.
        let head = "x".repeat(48);
        label.head.clone_from(&head);
        input.clause = Some(label.clone());
        assert!(
            compose_tooltip(&input)
                .expect("composes")
                .contains(&format!("\nnow: {head} \u{b7} by you\n"))
        );
        // Negative control.
        label.by = None;
        input.clause = Some(label);
        let bare = compose_tooltip(&input).expect("composes");
        assert!(bare.contains(&format!("\nnow: {head}\ncwd:")), "{bare:?}");
        assert!(!bare.contains("\u{b7} by "), "{bare:?}");
    }

    /// THE CLAUSE'S LINE AND ROW (phase 3B; addendum A12 item 3): the
    /// tooltip's `now: <head>` sits right after `activity:` with no age, and
    /// the menu's row at the same place, aged as the menu pops. Every other
    /// line and row is the input's without a clause, byte for byte.
    ///
    /// NEGATIVE CONTROL: no clause, no line and no row.
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
        // With no activity line the clause follows the description.
        input.activity = None;
        assert!(
            compose_tooltip(&input)
                .expect("composes")
                .contains("description: Rebuilds the docs site\nnow: running shell\ncwd:")
        );
        // Negative control.
        assert!(!compose_tooltip(&plain).expect("composes").contains("now:"));
        assert!(
            !without
                .iter()
                .any(|e| menu_entry_mirror(e).starts_with("now:"))
        );
    }

    /// A label's words reach the chrome through the presentation sanitizer
    /// every header line takes: a bidi override or a control character is
    /// stripped, and a clause with nothing left is no line at all.
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

    /// RULING S10: a shown label leaves at its `until`, so the composition's
    /// expiry is the earlier of the upgrade's term and that instant, and
    /// with the switch off (no clause) it is exactly the upgrade's term —
    /// `None` for a quiet tab, never `composed + 30 s`. A step or turn
    /// clause (no `until`) arms nothing.
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
