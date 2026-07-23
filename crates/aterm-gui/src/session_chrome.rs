// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session UI CHROME composer (session-metadata stage 2): the ONE pure
//! source of truth for what a terminal tab's HOVER TOOLTIP and RIGHT-CLICK
//! CONTEXT MENU say about a session.
//!
//! Stage 1 ([`crate::session_timeline`]) gave every session a user identity
//! (`meta set title|description|icon`) and a bounded lifecycle timeline. This
//! module turns those — plus the generated live activity, the engine's cwd, and
//! the registry's lifecycle state — into TWO renderings of the SAME facts:
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
//! One composer feeding both surfaces is the point: the menu can never say
//! something the tooltip doesn't, and the `chrome` introspection verb's
//! [`tab_menu_chrome_line`] mirror (read off the live strip) is provably the
//! same items a human sees, because there is exactly one place items are made.
//!
//! PURE by construction — no locks, no AppKit, no clocks (ages arrive as
//! `age_ms` deltas; the `~`-abbreviation takes an EXPLICIT `home` like
//! [`crate::app_tabs::home_relative_suffix`], which it reuses) — so every
//! ordering/truncation/degradation rule is unit-proved headlessly, on every
//! platform. The impure work (leaf-lock reads, epoch caching) lives in
//! `App::composed_session_chrome` (`app_tabs.rs`).

use crate::menu::MenuAction;

/// How many timeline events the tooltip / menu shows — the "recent" TAIL,
/// newest-first. Small on purpose: hover chrome is a glance, not a log (the
/// `timeline` verb serves the full ring).
pub(crate) const TIMELINE_TAIL: usize = 5;

/// Display cap for authored description and generated activity prose (GRAPHEME
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
    /// This is durable, user-authored identity and is never replaced by a
    /// generated activity summary.
    pub description: Option<String>,
    /// Generated live terminal activity. This is transient presentation data,
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
    /// The recent timeline tail, NEWEST-FIRST, already capped to
    /// [`TIMELINE_TAIL`] by the caller (the composer re-caps defensively).
    pub timeline: Vec<TimelineNote>,
}

/// One timeline event as the chrome shows it: the kind token plus its age at
/// compose time. The payload is deliberately NOT shown — hover chrome is a
/// pulse ("what happened lately"), the `timeline` verb is the detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TimelineNote {
    /// The event kind token (`spawned`, `cwd-change`, `meta-change`, …).
    pub kind: &'static str,
    /// Milliseconds since the event, on the same monotonic clock it was
    /// recorded with ([`crate::turn_ledger::now_ms`]).
    pub age_ms: u64,
}

/// One entry of the composed context menu — the platform-neutral MODEL the
/// macOS strip renders as `NSMenuItem`s and the `chrome` verb serialises via
/// [`tab_menu_chrome_line`]. Mirrors [`crate::menu::MENU_MODEL`]'s
/// philosophy: the description IS the menu; native code only renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TabMenuEntry {
    /// A disabled, informational row (identity / timeline lines).
    Header(String),
    /// A native separator.
    Separator,
    /// A live command row: `action`'s tag is what the `NSMenuItem` carries, so
    /// the click dispatches through the SAME tag→[`MenuAction`] decode the menu
    /// bar uses (never a parallel path). `enabled: false` renders greyed
    /// (e.g. `Copy CWD` with no reported cwd).
    Action {
        label: &'static str,
        action: MenuAction,
        enabled: bool,
    },
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
    /// Monotonic revision of the generated activity at compose time. Activity
    /// can change without a session-timeline event and can be omitted from the
    /// visible label by title-format settings, so it must be an independent
    /// cache epoch rather than relying on `high_id` or `label`.
    pub activity_revision: u64,
    /// [`crate::turn_ledger::now_ms`] at compose time — bounds staleness of
    /// the coarse relative ages.
    pub composed_ms: u64,
    /// The composed extension handed to the native strip.
    pub ext: TabChromeExt,
}

/// How long a cached composition may serve before the coarse relative ages are
/// re-derived even with unchanged inputs. The event loop arms the earliest
/// cache expiry and drains due entries in bounded batches, so this bounds age
/// drift without per-frame work.
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

#[must_use]
fn cache_expiry_ms(cache: &CachedChrome) -> u64 {
    cache.composed_ms.saturating_add(CACHE_MAX_AGE_MS)
}

/// Earliest absolute process-clock expiry represented by the cache, or `None`
/// when no terminal chrome is retained (the event loop can return to pure Wait).
#[must_use]
pub(crate) fn next_cache_expiry_ms(
    cache: &std::collections::HashMap<u64, CachedChrome>,
) -> Option<u64> {
    cache.values().map(cache_expiry_ms).min()
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
    // thousand-tab expiry still allocates O(Budget), never O(due entries).
    let mut due: Vec<(u64, u64)> = Vec::with_capacity(budget.min(cache.len()));
    for (session, entry) in cache {
        let expiry = cache_expiry_ms(entry);
        if expiry > now_ms {
            continue;
        }
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

/// The shared identity/timeline HEADER LINES (title, durable description,
/// generated activity, cwd, state, then the newest-first timeline tail) — the
/// one list both surfaces render. Authored and generated prose are labeled so
/// transient model output can never masquerade as durable session metadata.
/// Returns `(identity_lines, timeline_lines)` so the two consumers can place
/// their own separators between the groups.
fn header_lines(input: &SessionChromeInput) -> (Vec<String>, Vec<String>) {
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
    if let Some(cwd) = input.cwd.as_deref().filter(|c| !c.is_empty()) {
        let cwd = display_cwd(cwd, input.home.as_deref());
        if !cwd.is_empty() {
            identity.push(format!("cwd: {cwd}"));
        }
    }
    if let Some(state) = input
        .state
        .as_deref()
        .filter(|s| !s.is_empty() && *s != "-")
    {
        identity.push(format!("state: {state}"));
    }
    let timeline: Vec<String> = input
        .timeline
        .iter()
        .take(TIMELINE_TAIL)
        .map(|n| format!("{} · {}", n.kind, relative_age(n.age_ms)))
        .collect();
    (identity, timeline)
}

/// Compose the hover TOOLTIP for one terminal tab, or `None` when the session
/// carries NOTHING beyond its bare label (no authored description, generated
/// activity, icon, cwd, state, or timeline) — the tab then keeps today's
/// no-tooltip behavior instead of a tooltip that merely repeats the chip text.
/// Shape: the identity lines, then a blank line, then the newest-first timeline
/// tail (each `<kind> · <age>`).
#[must_use]
pub(crate) fn compose_tooltip(input: &SessionChromeInput) -> Option<String> {
    let (identity, timeline) = header_lines(input);
    if identity.len() <= 1 && timeline.is_empty() {
        // Only the title line — nothing the chip doesn't already say.
        return None;
    }
    let mut lines = identity;
    if !timeline.is_empty() {
        lines.push(String::new());
        lines.extend(timeline);
    }
    Some(lines.join("\n"))
}

/// Compose the right-click CONTEXT-MENU model for one terminal tab: the same
/// identity lines as the tooltip as disabled headers, a separator, the
/// newest-first timeline tail (disabled), a separator, then the actions. The
/// menu ALWAYS exists (unlike the tooltip): a bare unnamed session still owns
/// `Copy Session ID` / `Copy CWD` / `Close Tab`, with the unavailable copies
/// greyed rather than hidden (a stable menu shape is learnable).
#[must_use]
pub(crate) fn compose_tab_menu(input: &SessionChromeInput) -> Vec<TabMenuEntry> {
    let (identity, timeline) = header_lines(input);
    let mut entries: Vec<TabMenuEntry> = identity.into_iter().map(TabMenuEntry::Header).collect();
    if !timeline.is_empty() {
        entries.push(TabMenuEntry::Separator);
        entries.extend(timeline.into_iter().map(TabMenuEntry::Header));
    }
    entries.push(TabMenuEntry::Separator);
    entries.push(TabMenuEntry::Action {
        label: "Copy Session ID",
        action: MenuAction::CopySessionId,
        enabled: input.has_session,
    });
    entries.push(TabMenuEntry::Action {
        label: "Copy CWD",
        action: MenuAction::CopyCwd,
        enabled: input.cwd.as_deref().is_some_and(|c| !c.is_empty()),
    });
    entries.push(TabMenuEntry::Action {
        label: "Close Tab",
        action: MenuAction::CloseTab,
        enabled: true,
    });
    entries
}

/// Serialise one tab's context-menu model to the `chrome` verb's line —
/// `tab-menu tab=<i> items=["…", "---", …]` — the introspection mirror of what
/// a right-click on that chip pops. Separators print as `---`; a DISABLED
/// action is suffixed ` (disabled)` so a driving AI sees the same
/// greyed-vs-live distinction a human does. Headers print verbatim (their
/// disabled-ness is structural: they carry no action).
#[must_use]
pub(crate) fn tab_menu_chrome_line(index: usize, entries: &[TabMenuEntry]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|e| match e {
            TabMenuEntry::Header(t) => t.clone(),
            TabMenuEntry::Separator => "---".to_string(),
            TabMenuEntry::Action { label, enabled, .. } => {
                if *enabled {
                    (*label).to_string()
                } else {
                    format!("{label} (disabled)")
                }
            }
        })
        .collect();
    format!("tab-menu tab={index} items={items:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached_at(composed_ms: u64) -> CachedChrome {
        CachedChrome {
            high_id: 0,
            label: "session".to_string(),
            activity_revision: 0,
            composed_ms,
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

    fn full_input() -> SessionChromeInput {
        SessionChromeInput {
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
                    age_ms: 3_000,
                },
                TimelineNote {
                    kind: "cwd-change",
                    age_ms: 125_000,
                },
                TimelineNote {
                    kind: "spawned",
                    age_ms: 7_200_000,
                },
            ],
        }
    }

    /// The tooltip renders identity lines in the pinned order (icon+title,
    /// durable description, generated activity, ~-abbreviated cwd, state), a
    /// blank line, then the newest-first timeline tail — one exact byte shape,
    /// so surfaces never drift apart silently.
    #[test]
    fn tooltip_orders_identity_then_timeline_with_home_abbreviation() {
        let tip = compose_tooltip(&full_input()).expect("full metadata composes");
        assert_eq!(
            tip,
            "🤖 build agent\n\
             description: Rebuilds the docs site\n\
             activity: Building documentation\n\
             cwd: ~/src/aterm\n\
             state: alive\n\
             \n\
             meta-change · just now\n\
             cwd-change · 2m ago\n\
             spawned · 2h ago"
        );
    }

    /// Generated activity is transient and therefore never overwrites durable
    /// `meta set description` identity. With both present they remain distinct;
    /// without authored metadata, activity alone supplies the descriptive line.
    #[test]
    fn authored_description_and_generated_activity_remain_distinct() {
        let both = full_input();
        let tip = compose_tooltip(&both).expect("full metadata composes");
        assert!(
            tip.lines()
                .any(|line| line == "description: Rebuilds the docs site")
        );
        assert!(
            tip.lines()
                .any(|line| line == "activity: Building documentation")
        );

        let activity_only = SessionChromeInput {
            label: "tests".to_string(),
            activity: Some("Running the focused test suite".to_string()),
            ..SessionChromeInput::default()
        };
        assert_eq!(
            compose_tooltip(&activity_only).as_deref(),
            Some("tests\nactivity: Running the focused test suite")
        );
        assert_eq!(
            compose_tab_menu(&activity_only)[1],
            TabMenuEntry::Header("activity: Running the focused test suite".to_string())
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
            TabMenuEntry::Header(label) =>
                !crate::session_timeline::metadata_has_forbidden_formatting(label),
            TabMenuEntry::Action { label, .. } =>
                !crate::session_timeline::metadata_has_forbidden_formatting(label),
            TabMenuEntry::Separator => true,
        }));
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

    /// One extra fact (here: a cwd) is enough to earn a tooltip — and a cwd
    /// OUTSIDE home (or a sibling like `/Users//foobar`) stays verbatim, the
    /// component-boundary rule `home_relative_suffix` proves.
    #[test]
    fn single_fact_composes_and_foreign_cwd_stays_verbatim() {
        let input = SessionChromeInput {
            label: "zsh".to_string(),
            cwd: Some("/Users//foobar/x".to_string()),
            home: Some("/Users//foo".to_string()),
            ..SessionChromeInput::default()
        };
        assert_eq!(
            compose_tooltip(&input).as_deref(),
            Some("zsh\ncwd: /Users//foobar/x")
        );
    }

    /// The description is display-truncated at the cap with an ellipsis —
    /// counting GRAPHEME CLUSTERS (single-char clusters here, so the cluster
    /// count and char count agree) — while short text is untouched.
    #[test]
    fn description_truncates_on_a_char_boundary() {
        let mut input = full_input();
        input.description = Some("é".repeat(DESCRIPTION_DISPLAY_MAX + 40));
        let tip = compose_tooltip(&input).unwrap();
        let line = tip
            .lines()
            .nth(1)
            .unwrap()
            .strip_prefix("description: ")
            .unwrap();
        assert_eq!(line.chars().count(), DESCRIPTION_DISPLAY_MAX + 1, "cap+…");
        assert!(line.ends_with('…'));
        assert!(line.chars().take(DESCRIPTION_DISPLAY_MAX).all(|c| c == 'é'));
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
    /// the caller hands more.
    #[test]
    fn timeline_tail_is_capped() {
        let mut input = full_input();
        input.timeline = (0..9)
            .map(|i| TimelineNote {
                kind: "state-change",
                age_ms: i * 1000,
            })
            .collect();
        let tip = compose_tooltip(&input).unwrap();
        let events = tip
            .lines()
            .filter(|l| l.starts_with("state-change"))
            .count();
        assert_eq!(events, TIMELINE_TAIL);
        let menu = compose_tab_menu(&input);
        let headers = menu
            .iter()
            .filter(|e| matches!(e, TabMenuEntry::Header(h) if h.starts_with("state-change")))
            .count();
        assert_eq!(headers, TIMELINE_TAIL);
    }

    /// The age buckets are coarse and monotone: sub-10s reads "just now", then
    /// seconds, minutes, hours, days.
    #[test]
    fn relative_age_buckets() {
        assert_eq!(relative_age(0), "just now");
        assert_eq!(relative_age(9_999), "just now");
        assert_eq!(relative_age(10_000), "10s ago");
        assert_eq!(relative_age(59_999), "59s ago");
        assert_eq!(relative_age(60_000), "1m ago");
        assert_eq!(relative_age(3_599_999), "59m ago");
        assert_eq!(relative_age(3_600_000), "1h ago");
        assert_eq!(relative_age(86_400_000), "1d ago");
    }

    /// The menu model carries the pinned structure: identity headers, sep,
    /// timeline headers, sep, then the three actions in order — with the
    /// SAME header text the tooltip shows (one composer, two surfaces).
    #[test]
    fn menu_matches_tooltip_headers_and_orders_actions() {
        let input = full_input();
        let menu = compose_tab_menu(&input);
        let tip = compose_tooltip(&input).unwrap();
        let tip_lines: Vec<&str> = tip.lines().filter(|l| !l.is_empty()).collect();
        let menu_headers: Vec<&str> = menu
            .iter()
            .filter_map(|e| match e {
                TabMenuEntry::Header(h) => Some(h.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(menu_headers, tip_lines, "menu headers ARE the tooltip");
        let actions: Vec<(&str, MenuAction, bool)> = menu
            .iter()
            .filter_map(|e| match e {
                TabMenuEntry::Action {
                    label,
                    action,
                    enabled,
                } => Some((*label, *action, *enabled)),
                _ => None,
            })
            .collect();
        assert_eq!(
            actions,
            vec![
                ("Copy Session ID", MenuAction::CopySessionId, true),
                ("Copy CWD", MenuAction::CopyCwd, true),
                ("Close Tab", MenuAction::CloseTab, true),
            ]
        );
        assert_eq!(
            menu.iter()
                .filter(|e| matches!(e, TabMenuEntry::Separator))
                .count(),
            2,
            "identity | timeline | actions"
        );
    }

    /// A bare session still gets a menu (label header + actions), with the
    /// unavailable copies GREYED, not hidden — and only ONE separator (no
    /// empty timeline group).
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
            1
        );
        assert!(menu.iter().any(|e| matches!(
            e,
            TabMenuEntry::Action {
                action: MenuAction::CopySessionId,
                enabled: false,
                ..
            }
        )));
        assert!(menu.iter().any(|e| matches!(
            e,
            TabMenuEntry::Action {
                action: MenuAction::CopyCwd,
                enabled: false,
                ..
            }
        )));
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
    /// headers verbatim, separators as `---`, disabled actions annotated — so
    /// the introspected listing IS the on-screen menu.
    #[test]
    fn chrome_line_mirrors_composed_model() {
        let input = SessionChromeInput {
            label: "zsh".to_string(),
            cwd: Some("/tmp".to_string()),
            ..SessionChromeInput::default()
        };
        let line = tab_menu_chrome_line(2, &compose_tab_menu(&input));
        assert_eq!(
            line,
            r#"tab-menu tab=2 items=["zsh", "cwd: /tmp", "---", "Copy Session ID (disabled)", "Copy CWD", "Close Tab"]"#
        );
    }
}
