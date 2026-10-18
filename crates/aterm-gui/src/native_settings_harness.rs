// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS ▸ HARNESS (`docs/DESIGN-harness-autopilot-2026-10-15.md` §4): the
//! coding agents' harness page, headed "Coding agent harness" where that
//! fits (else "Harness", its rail entry), a SPECIAL route built like
//! Packages over the host's [`HarnessState`] (`crate::harness_screen`). Six
//! cards, in order:
//!
//! 1. **STATUS** — the policy in force in its own words
//!    (`HarnessPolicy::words`, as Supervision) and how many of the open agent
//!    sessions the harness supervises (the build's version is About's); under
//!    it the page's three actions, Refresh (read the ledgers again), Open
//!    aterm.toml (the canonical editor at its `[harness]` table, the Manual
//!    route's own act) and AI CLIs (its sub-page, `native_settings_ai.rs`:
//!    the AI CLIs' logins, whose "Use on this machine" switches are not the
//!    "Supervise" switches here).
//! 2. **SESSIONS** — each live session of a supervisable agent (Claude Code,
//!    Codex, agy, Muse Code, Gemini CLI): its tab title, where it is and how
//!    the harness holds it, and a three-way control —
//!    Cruise, Land, Off — the mode it runs under selected. A default that
//!    carries (Cruise or Land) has its button hand the session back to the
//!    default (its own word cleared); every other press writes its own
//!    Autopilot word, Off always, as the footer's chip does
//!    (`harness_screen::hands_back`, `App::execute_native_harness`). Inert
//!    where the harness runs no mode (the master switch off, no host), and
//!    an empty state where no agent session is open. Owner-only on the
//!    control surface (`harness_screen::SESSION_ACTION_PREFIX`). Under its
//!    detail, the row's STATE LINE (`lands at 18:00`, `landed 17:42`, …:
//!    `HarnessSessionRow::state_line`, clock times only), and a TIMER
//!    disclosure (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.4)
//!    holding `1 h`, `2 h`, `4 h`, `at 18:00` (only with `[harness] land_at`
//!    set; `at Sat 18:00` once today's 18:00 has passed, the day it would
//!    arm) and `Cancel` (only while a timer is armed), keyed
//!    `harness/session/<id>/land-in/60|120|240`, `…/land-at` and
//!    `…/land-cancel`: a timed Cruise, through the App's one writer
//!    (`HarnessTimerRequest`), Owner-only like the mode buttons.
//! 3. **SUPERVISION** — the registry rows the harness owns: Supervise coding
//!    agents (the master switch), one switch per supervisable agent (Supervise
//!    Claude Code sessions, Watch codex-alab sessions, Supervise Antigravity CLI
//!    sessions, Supervise Muse Code sessions, Supervise Gemini CLI sessions:
//!    `docs/DESIGN-harness-plugins-2026-10-15.md` §4), Default
//!    autopilot, Approve permission boxes, Answer questions and Question
//!    priority, each with its note — every row of the Harness section, in the
//!    registry's order ([`harness_page_rows`]). The ordinary
//!    `settings/set/<key>` controls, so OCC saves, Undo, Search and Modified
//!    behave as on every page; an enum row opens the ordinary choice picker,
//!    which takes the page while open.
//! 4. **WHAT THE HARNESS DID** — today and over 7 days, from its ledgers.
//! 5. **EFFECT OF EACH AUTOPILOT** — the A/B: the share of agents' time spent
//!    waiting for input under Cruise, Land and Off (and unsupervised), and
//!    Cruise's beside Off's, for this aterm run. Never a "time saved".
//! 6. **SETTINGS IN ATERM.TOML** — every `[harness]` key, its value, and
//!    whether it is the default or yours: the person's own keys and how many
//!    more are at their defaults, and under the card a disclosure that lists
//!    all of them (thirty keys are most of a page on their own).
//!
//! The cards are whole: a control card taller than one screen's room is split
//! into continued cards ([`chunk_rows`]), a fact card taller than the screen
//! scrolls by its rows, and the cards are one scrolling list, as on every
//! Settings page. A heading that would not be whole takes its short words
//! ([`card_heading`]); a compact page as wide as a Medium one keeps its
//! control rows on one line.

use super::*;
use crate::autopilot_timers::TimerChoice;
use crate::harness_screen::{
    HarnessSessionRow, autopilot_action, hands_back, parse_autopilot_action,
};
use crate::native_app::{HarnessRequest, HarnessTimerRequest};
use aterm_agent::supervise::Autopilot;

/// Every action the page mints starts with this (`harness/refresh`,
/// `harness/open-config`, `harness/session/<id>/autopilot/<word>`).
pub(super) const HARNESS_ACTION_PREFIX: &str = "harness/";

/// The Refresh button's key and action: read the ledgers again.
pub(super) const HARNESS_REFRESH: &str = "harness/refresh";

/// The Open aterm.toml button's key and action.
pub(super) const HARNESS_OPEN_CONFIG: &str = "harness/open-config";

/// The SETTINGS IN ATERM.TOML card's disclosure: every key, or only yours.
pub(super) const HARNESS_CONFIG_TOGGLE: &str = "harness/config/toggle";

/// A SESSIONS row's timer press: `harness/session/<id>/land-in/<minutes>`,
/// `…/land-at`, `…/land-cancel` (design §9.3.4) — under the session prefix,
/// so Owner-only on the control surface like its mode buttons.
pub(super) fn timer_action(local_id: u64, choice: TimerChoice) -> String {
    let prefix = crate::harness_screen::SESSION_ACTION_PREFIX;
    match choice {
        TimerChoice::In(minutes) => format!("{prefix}{local_id}/land-in/{minutes}"),
        TimerChoice::At => format!("{prefix}{local_id}/land-at"),
        TimerChoice::Cancel => format!("{prefix}{local_id}/land-cancel"),
    }
}

/// [`timer_action`] read back; `None` for any other action, and for a
/// duration the page does not offer.
pub(super) fn parse_timer_action(action: &str) -> Option<(u64, TimerChoice)> {
    let rest = action.strip_prefix(crate::harness_screen::SESSION_ACTION_PREFIX)?;
    let (id, what) = rest.split_once('/')?;
    let choice = match what {
        "land-at" => TimerChoice::At,
        "land-cancel" => TimerChoice::Cancel,
        _ => {
            let minutes: u32 = what.strip_prefix("land-in/")?.parse().ok()?;
            crate::autopilot_menu::LAND_IN_MINUTES
                .contains(&minutes)
                .then_some(TimerChoice::In(minutes))?
        }
    };
    Some((id.parse().ok()?, choice))
}

/// A SESSIONS row's Timer disclosure: `harness/session/<id>/timer`.
pub(super) fn timer_toggle(local_id: u64) -> String {
    format!(
        "{}{local_id}/timer",
        crate::harness_screen::SESSION_ACTION_PREFIX
    )
}

/// [`timer_toggle`] read back.
fn parse_timer_toggle(action: &str) -> Option<u64> {
    action
        .strip_prefix(crate::harness_screen::SESSION_ACTION_PREFIX)?
        .strip_suffix("/timer")?
        .parse()
        .ok()
}

/// A SESSIONS row's `Show`: `harness/session/<id>/show` — raise the
/// session's tab and pane (§10.3.1). It writes nothing; it is under the
/// session prefix all the same, so the control surface keeps it Owner-only
/// like the row's Timer disclosure (fail-closed: one fence for the prefix).
pub(super) fn show_action(local_id: u64) -> String {
    format!(
        "{}{local_id}/show",
        crate::harness_screen::SESSION_ACTION_PREFIX
    )
}

/// [`show_action`] read back.
fn parse_show_action(action: &str) -> Option<u64> {
    action
        .strip_prefix(crate::harness_screen::SESSION_ACTION_PREFIX)?
        .strip_suffix("/show")?
        .parse()
        .ok()
}

pub(super) use crate::harness_screen::HARNESS_LAND;

/// The key the page lands on for `focus`: a session row's `Show` button (a
/// control of the row's own stop), or SUPERVISION's first switch.
fn landing_key(focus: crate::harness_screen::HarnessFocus) -> Option<UiKey> {
    use crate::harness_screen::HarnessFocus;
    match focus {
        HarnessFocus::Top => None,
        HarnessFocus::Session(id) => Some(UiKey::new(show_action(id))),
        HarnessFocus::Supervision => harness_page_rows()
            .next()
            .map(|key| UiKey::new(format!("settings/control/{key}"))),
    }
}

/// The AI CLIs button's key: its action is the sub-page's route.
pub(super) const HARNESS_AI_CLIS: &str = "harness/ai-clis";

/// The page's heading (its rail label is the route's, "Harness").
const HARNESS_PAGE_TITLE: &str = "Coding agent harness";

/// The SUPERVISION card's registry rows: every row of the Harness section, in
/// the registry's order, so an agent's switch added to the registry is on
/// the page with no list here — but the AI CLIs' (`ai.`), which its AI CLIs
/// sub-page draws ([`super::ai_page::ai_page_rows`]). The Bool rows are
/// [`registry_switch_row`]s; the choices are ordinary [`setting_row`]s whose
/// control opens the choice picker.
pub(super) fn harness_page_rows() -> impl Iterator<Item = &'static str> {
    prefs::NESTED_LEAVES
        .iter()
        .map(|leaf| leaf.key)
        .filter(|key| prefs::section_of(key) == prefs::Section::Harness)
        .filter(|key| !key.starts_with(super::ai_page::AI_KEY_PREFIX))
}

/// A card's padding (the metadata cards' own), the tight padding a row card
/// takes where its tallest row fits a page's room only that way, and the gap
/// between a card's rows.
pub(super) const CARD_PAD: f32 = ABOUT_METADATA_CARD_PADDING;
pub(super) const TIGHT_CARD_PAD: f32 = 8.0;
pub(super) const CARD_GAP: f32 = 6.0;

/// The gap between a session row's three buttons.
const MODE_GAP: f32 = 6.0;

/// The page's subtitle, which yields to nothing: it must fit the compact
/// phone page and the minimum Medium measure (the subtitle-fit test).
pub(super) fn harness_page_subtitle(width: SettingsWidth) -> &'static str {
    if width == SettingsWidth::Compact {
        "Autopilot, switches, and stats."
    } else {
        "Switches, each session's Autopilot, what it did, and its settings."
    }
}

/// The page's heading on a page `width` wide: "Coding agent harness" where it
/// is whole, else the route's own name, "Harness" (large type on a phone
/// page).
fn harness_page_title(width: f32) -> &'static str {
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Hero);
    if crate::tray_raster::ui_text_width_for(face, HARNESS_PAGE_TITLE, px) <= width {
        HARNESS_PAGE_TITLE
    } else {
        SettingsRoute::Harness.label()
    }
}

/// A card's heading at `width`: its full words, else its short ones; a
/// continued card's says so after them where that fits too.
pub(super) fn card_heading(full: &str, short: &str, continued: bool, width: f32) -> String {
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Quiet);
    let fits = |text: &str| crate::tray_raster::ui_text_width_for(face, text, px) <= width;
    let mut candidates = Vec::new();
    if continued {
        candidates.push(format!("{full} \u{b7} CONTINUED"));
        candidates.push(format!("{short} \u{b7} CONTINUED"));
    }
    candidates.push(full.to_string());
    candidates.push(short.to_string());
    if let Some(text) = candidates.iter().find(|text| fits(text)) {
        return text.clone();
    }
    // Not even the short words: they are elided (the card's group keeps its
    // whole name for a screen reader).
    let mut chars: Vec<char> = short.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let text = format!("{}\u{2026}", chars.iter().collect::<String>());
        if fits(&text) {
            return text;
        }
    }
    String::new()
}

/// The page's palette commands while it is the route.
pub(super) fn harness_commands(harness: &HarnessState) -> Vec<Command> {
    vec![
        Command {
            id: ActionId::new(HARNESS_REFRESH),
            title: "Harness: Refresh".to_string(),
            shortcut: None,
            enabled: !harness.stats.collecting,
        },
        Command {
            id: ActionId::new(HARNESS_OPEN_CONFIG),
            title: "Harness: Open aterm.toml".to_string(),
            shortcut: None,
            enabled: true,
        },
    ]
}

impl SettingsApp {
    /// A press on the page ([`HARNESS_ACTION_PREFIX`]): Refresh and a
    /// session's Autopilot become typed host requests
    /// ([`HarnessRequest`]); Open aterm.toml is the Manual route's own act,
    /// aimed at the `[harness]` table and staying on this page. A press for
    /// a session the projection no longer lists, or while the harness runs no
    /// mode here, writes nothing and says why.
    pub(super) fn reduce_harness_action(
        &mut self,
        view: &mut SettingsViewState,
        action: &str,
        value: Option<&SemanticInput>,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        if action == HARNESS_REFRESH {
            cx.harness(HarnessRequest::Refresh);
        } else if action == HARNESS_LAND {
            // The page opened for a row or a card: the list lands on it, as a
            // deep link does, before or after a render has measured the page.
            let Some(SemanticInput::Text(words)) = value else {
                return EventResult::Bubble;
            };
            let Some(focus) = crate::harness_screen::HarnessFocus::parse(words) else {
                return EventResult::Bubble;
            };
            view.harness_focus = focus;
            match landing_key(focus) {
                Some(key) => view.land_on_control(key),
                None => view.page_scroll = 0,
            }
        } else if let Some(session) = parse_show_action(action) {
            // Navigation writes nothing: no posture is weighed.
            if self.harness.sessions.iter().any(|s| s.local_id == session) {
                cx.harness(HarnessRequest::Show { session });
            } else {
                view.feedback = Some("That session is no longer open.".to_string());
            }
        } else if action == HARNESS_CONFIG_TOGGLE {
            // The page's cards change height: its bound is the next render's.
            view.harness_config_open = !view.harness_config_open;
            view.invalidate_result_page_limit();
        } else if action == HARNESS_OPEN_CONFIG {
            if view
                .pending
                .values()
                .any(|pending| matches!(pending, PendingAction::ConfigEditor))
            {
                view.feedback = Some("aterm.toml is already opening\u{2026}".to_string());
            } else {
                let operation = cx.open_config_editor_at(Some(ConfigEditorTarget::Key(
                    prefs::EDIT_HARNESS_ENABLED.to_string(),
                )));
                view.pending.insert(operation, PendingAction::ConfigEditor);
                view.feedback = Some("Opening aterm.toml\u{2026}".to_string());
            }
        } else if let Some(session) = parse_timer_toggle(action) {
            // One row's disclosure at a time; the page's cards change height.
            view.harness_timer_open = (view.harness_timer_open != Some(session)).then_some(session);
            view.invalidate_result_page_limit();
        } else if let Some((session, choice)) = parse_timer_action(action) {
            // By the session's own posture, as its mode buttons are.
            match self.harness.sessions.iter().find(|s| s.local_id == session) {
                None => view.feedback = Some("That session is no longer open.".to_string()),
                Some(open) if open.default.is_none() => {
                    view.feedback = Some(nothing_to_steer(self.harness.posture, open.program));
                }
                Some(_) => cx.harness_timer(HarnessTimerRequest { session, choice }),
            }
        } else if let Some((session, word)) = parse_autopilot_action(action) {
            // Each session by its own posture (the window's, joined with its
            // agent's switch), as its row's buttons are drawn.
            match self.harness.sessions.iter().find(|s| s.local_id == session) {
                None => view.feedback = Some("That session is no longer open.".to_string()),
                Some(open) if open.default.is_none() => {
                    view.feedback = Some(nothing_to_steer(self.harness.posture, open.program));
                }
                Some(_) => cx.harness(HarnessRequest::Autopilot { session, word }),
            }
        } else {
            return EventResult::Bubble;
        }
        cx.repaint(crate::native_app::DamageRegion::All);
        EventResult::Handled
    }
}

/// THE PAGE: its heading pinned, then its cards as one scrolling list. While
/// a choice picker for one of its rows is open, the picker takes the page.
pub(super) fn harness_page(
    state: &SettingsViewState,
    harness: &HarnessProjection,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    let viewport = cx.viewport;
    if state
        .choice_picker
        .as_ref()
        .is_some_and(|picker| harness_page_rows().any(|key| key == picker.key))
    {
        return harness_choice_page(state, width, cx);
    }
    let gap = settings_page_gap(state, width, viewport);
    let maximum = page_maximum(SettingsRoute::Harness, width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (heading, heading_height) = pinned_heading(
        width,
        viewport,
        gap,
        harness_page_title(page_width),
        harness_page_subtitle(width),
    );
    let room = (settings_list_body_height(state, width, viewport) - heading_height)
        .max(scaled_control_height());
    let cards = harness_cards(state, harness, width, page_width, (room, gap));
    let mut out = heading;
    out.push(scroll::list_page_peeking(
        state,
        "settings/list",
        "Harness",
        cards
            .into_iter()
            .map(|(node, height)| ListItem::of(node, height))
            .collect(),
        room,
        gap,
        settings_list_overhang(state, width, viewport),
    ));
    out
}

/// The page while a choice picker for one of its rows is open: the picker
/// alone under the heading (beside nothing on a short landscape host, where
/// it takes the compact landscape choice page).
fn harness_choice_page(
    state: &SettingsViewState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    if width == SettingsWidth::Compact {
        let budget = compact_page_budget(state, cx.viewport);
        if budget.side_by_side_pager
            && let Some(page) =
                compact_landscape_choice_page(state, None, budget, cx.viewport.width)
        {
            return page;
        }
    }
    let gap = settings_page_gap(state, width, cx.viewport);
    let maximum = page_maximum(SettingsRoute::Harness, width);
    let (mut out, _) = pinned_heading(
        width,
        cx.viewport,
        gap,
        harness_page_title(settings_page_content_width(
            cx.viewport.width,
            width,
            maximum,
        )),
        "",
    );
    let picker = choice_picker_node(state, width, cx.viewport, &out);
    out.extend(picker);
    out
}

/// Every card of the page, in order, each no taller than `room` where its
/// rows allow (a single row taller than the room is a card of its own).
fn harness_cards(
    state: &SettingsViewState,
    harness: &HarnessProjection,
    width: SettingsWidth,
    card_width: f32,
    (room, gap): (f32, f32),
) -> Vec<(UiNode, f32)> {
    let inner = (card_width - 2.0 * CARD_PAD).max(0.0);
    let label_width = about_metadata_label_width();
    let value_width = (inner - label_width - ABOUT_METADATA_ROW_GAP).max(0.0);
    let facts = |key: &str, heading: (&str, &str), rows: &[(String, String)]| {
        metadata_cards(key, heading, rows, inner, (value_width, label_width))
    };
    // The page's own width class: a compact row keeps its label beside its
    // control wherever the label gets its room, else above it.
    let rows_width = width;
    let mut cards = Vec::new();
    let mut status = facts("harness/status", ("STATUS", "STATUS"), &harness.status_rows);
    // Today's numbers under STATUS's facts, inside its card (§10.3.1), in
    // what the first screen has left once SESSIONS' top is on it: the card,
    // the list's gap, and SESSIONS' pad and heading.
    if let (Some(lines), Some((card, height))) = (&harness.today, status.first_mut()) {
        let sessions_top = gap + CARD_PAD + 22.0_f32.max(16.0 * settings_text_scale());
        let budget = room - *height - sessions_top - 4.0;
        if let Some((block, block_height)) = today_block(lines, inner, budget) {
            card.children.push(block);
            *height += 4.0 + block_height;
            card.layout = card.layout.height(Length::Fixed(*height));
        }
    }
    cards.extend(status);
    // Each session's Autopilot is what a person comes here to change: it
    // goes beside the status, on the first page, before the switches — and
    // before the page's actions, which would push it off an 80 × 24
    // window's first screen under today's lines (§10.3.3, item 6).
    let selected = match state.harness_focus {
        crate::harness_screen::HarnessFocus::Session(id) => Some(id),
        _ => None,
    };
    cards.extend(row_cards(
        "harness/sessions",
        ("Sessions", ("SESSIONS", "SESSIONS")),
        session_rows(
            harness,
            rows_width,
            (inner, room),
            state.harness_timer_open,
            selected,
        ),
        room,
        card_width,
    ));
    cards.push(harness_actions(harness, card_width));
    cards.extend(row_cards(
        "harness/switches",
        ("Supervision", ("SUPERVISION", "SUPERVISION")),
        switch_rows(state, rows_width, inner, room),
        room,
        card_width,
    ));
    cards.extend(facts(
        "harness/did",
        ("WHAT THE HARNESS DID", "WHAT IT DID"),
        &harness.did_rows,
    ));
    cards.extend(facts(
        "harness/ab",
        ("EFFECT OF EACH AUTOPILOT", "EFFECT"),
        &harness.ab_rows,
    ));
    // The effective table: the person's own keys and how many are at their
    // defaults, or — its disclosure open — every key.
    let config = config_facts(harness, state.harness_config_open);
    // The keys are longer than About's labels: the column is as wide as the
    // longest, up to two fifths of the card.
    let px = 13.0 * crate::native_appearance::text_scale();
    let key_width = config
        .iter()
        .map(|(key, _)| metadata_value_text_width(key, px, false))
        .fold(0.0_f32, f32::max)
        + ABOUT_METADATA_TEXT_SAFETY;
    let config_label_width = key_width.min(inner * 0.4).max(label_width.min(inner * 0.4));
    cards.extend(metadata_cards(
        "harness/config",
        ("SETTINGS IN ATERM.TOML", "ATERM.TOML"),
        &config,
        inner,
        (
            (inner - config_label_width - ABOUT_METADATA_ROW_GAP).max(0.0),
            config_label_width,
        ),
    ));
    if harness.config_rows.len() > harness.config_own.len() {
        cards.push(config_disclosure(
            state.harness_config_open,
            harness.config_rows.len(),
            card_width,
        ));
    }
    cards
}

/// `text` whole where `fits` says so, else cut back at its last ` · ` joint
/// that leaves it fitting (§10.3.1: a narrow page cuts a day's words at a
/// joint, never inside one); where not even its first part fits, that part
/// elided by `elide`.
fn cut_at_joint(text: &str, fits: impl Fn(&str) -> bool, elide: impl Fn(&str) -> String) -> String {
    use crate::harness_screen::JOINT;
    if fits(text) {
        return text.to_string();
    }
    let parts: Vec<&str> = text.split(JOINT).collect();
    (1..parts.len())
        .rev()
        .map(|n| parts[..n].join(JOINT))
        .find(|cut| fits(cut))
        .unwrap_or_else(|| elide(parts[0]))
}

/// Whether `text` is whole at `width` in the type `role` and `style` paint.
fn fits_in(text: &str, width: f32, role: SemanticRole, style: StyleRef) -> bool {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    crate::tray_raster::ui_text_width_for(face, text, px) <= width
}

/// STATUS'S TODAY (§10.3.1, [`crate::harness_screen::HarnessState::today_lines`])
/// as one block of the card, keyed `harness/status/today`, its label both
/// lines whole, no taller than `budget`: the first line in Hero type (the
/// page heading's) where it is whole at the card's `inner` width, else in
/// the page's plain type; the second, where there is one, plain. A plain
/// line that does not fit is cut at a ` · ` joint ([`cut_at_joint`]).
///
/// THE BUDGET is what the page's first screen has left once SESSIONS' top
/// is on it (§4 item 2: SESSIONS shares the first screen with the status).
/// Where the block does not fit it gives way in steps — Hero to plain, then
/// the second line, then the block — and WHAT THE HARNESS DID still says
/// every count today. `None` where not even one plain line fits.
pub(super) fn today_block(
    lines: &(String, String),
    inner: f32,
    budget: f32,
) -> Option<(UiNode, f32)> {
    let (first, second) = lines;
    let scale = settings_text_scale();
    let plain_height = 28.0_f32.max(18.0 * scale);
    let hero_height = page_heading_height();
    let line = |suffix: &str, text: &str, hero: bool| {
        let (role, style, height) = if hero {
            (SemanticRole::Heading, StyleRef::Hero, hero_height)
        } else {
            (SemanticRole::Text, StyleRef::Primary, plain_height)
        };
        let shown = if hero {
            text.to_string()
        } else {
            cut_at_joint(
                text,
                |t| fits_in(t, inner, role, style),
                |t| crate::native_ui::fit_native_text_label(t, inner),
            )
        };
        UiNode::new(
            format!("harness/status/today/{suffix}"),
            UiContent::Text(TextSpec {
                text: shown,
                role,
                style,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
        .paint_only()
    };
    let hero = fits_in(first, inner, SemanticRole::Heading, StyleRef::Hero);
    let more = !second.is_empty();
    // Each shape, fullest first: (Hero for the first line, the second drawn).
    let shapes = [(hero, more), (false, more), (false, false)];
    let (hero, more) = shapes.into_iter().find(|&(hero, more)| {
        let height = if hero { hero_height } else { plain_height }
            + if more { 4.0 + plain_height } else { 0.0 };
        height <= budget
    })?;
    let mut children = vec![line("hero", first, hero)];
    let mut height = if hero { hero_height } else { plain_height };
    if more {
        children.push(line("more", second, false));
        height += 4.0 + plain_height;
    }
    let label = if second.is_empty() {
        first.clone()
    } else {
        format!("{first}{}{second}", crate::harness_screen::JOINT)
    };
    Some((
        UiNode::new(
            "harness/status/today",
            UiContent::Group(GroupSpec::new(label)),
        )
        .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
        .children(children),
        height,
    ))
}

/// The SETTINGS IN ATERM.TOML card's facts: every `[harness]` setting while
/// its disclosure is `open`; else the ones the person changed, then how many
/// more are at their defaults. Before the host publishes, that it is reading.
fn config_facts(harness: &HarnessProjection, open: bool) -> Vec<(String, String)> {
    if harness.config_rows.is_empty() {
        return vec![("Settings".to_string(), "reading\u{2026}".to_string())];
    }
    if open {
        return harness.config_rows.clone();
    }
    let mut rows = harness.config_own.clone();
    let mine = !harness.config_own.is_empty();
    // The row's label already says "Defaults": its words count the settings,
    // never "… at their defaults" after it again.
    let defaults = match (harness.config_rows.len() - harness.config_own.len(), mine) {
        (0, _) => None,
        (1, true) => Some("1 more setting".to_string()),
        (1, false) => Some("the 1 setting".to_string()),
        (more, true) => Some(format!("{more} more settings")),
        (more, false) => Some(format!("all {more} settings")),
    };
    rows.extend(defaults.map(|words| ("Defaults".to_string(), words)));
    rows
}

/// The disclosure under the SETTINGS IN ATERM.TOML card: every one of the
/// table's `keys`, or only the ones the person changed (`open` says which it
/// shows).
fn config_disclosure(open: bool, keys: usize, card_width: f32) -> (UiNode, f32) {
    let height = scaled_control_height();
    let (label, visual) = if open {
        (
            "Show only the settings you changed".to_string(),
            "Show only yours".to_string(),
        )
    } else {
        (
            format!("Show all {keys} settings"),
            format!("Show all {keys}"),
        )
    };
    let width = (crate::native_ui::native_button_label_width(&visual) + 56.0).min(card_width);
    (
        UiNode::new(
            HARNESS_CONFIG_TOGGLE,
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(label).visual_label(crate::native_ui::fit_native_button_label(
                        &visual,
                        (width - 48.0).max(0.0),
                    )),
                    ActionId::new(HARNESS_CONFIG_TOGGLE),
                )
                .state(ControlState {
                    expanded: Some(open),
                    ..ControlState::default()
                })
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fixed(height)),
        ),
        height,
    )
}

/// A fact card ([`metadata_card_at`]) keyed `key`, its heading fitted to the
/// card's `inner` width ([`card_heading`], `heading` its full and short
/// words). The page's list scrolls a card taller than the screen by its
/// rows. `columns` is the value column's width and the label column's.
pub(super) fn metadata_cards(
    key: &str,
    heading: (&str, &str),
    rows: &[(String, String)],
    inner: f32,
    columns: (f32, f32),
) -> Vec<(UiNode, f32)> {
    let (value_width, label_width) = columns;
    let scale = settings_text_scale();
    let heading_height = 22.0_f32.max(16.0 * scale);
    let line_height = 28.0_f32.max(18.0 * scale) + 4.0;
    let heading = card_heading(heading.0, heading.1, false, inner);
    let height = 2.0 * ABOUT_METADATA_CARD_PADDING
        + heading_height
        + metadata_line_count(rows, value_width) as f32 * line_height;
    vec![(
        metadata_card_at(key, &heading, rows, height, value_width, label_width),
        height,
    )]
}

/// A card of control rows, split into continued cards no taller than `room`
/// ([`chunk_rows`]): a Secondary group, its heading, then its rows. `names`
/// is its semantic label and its heading's full and short words. Its padding
/// is the page's, or a tight one where the room holds its tallest row only
/// that way (a short host at large type).
pub(super) fn row_cards(
    key: &str,
    names: (&str, (&str, &str)),
    rows: Vec<(UiNode, f32)>,
    room: f32,
    card_width: f32,
) -> Vec<(UiNode, f32)> {
    let (label, heading) = names;
    let heading_height = 22.0_f32.max(16.0 * settings_text_scale());
    let tallest = rows
        .iter()
        .map(|(_, height)| *height)
        .fold(0.0_f32, f32::max);
    let pad = if 2.0 * CARD_PAD + heading_height + tallest + CARD_GAP <= room {
        CARD_PAD
    } else {
        TIGHT_CARD_PAD
    };
    let fixed = 2.0 * pad + heading_height;
    let inner = (card_width - 2.0 * pad).max(0.0);
    chunk_rows(rows, room - fixed)
        .into_iter()
        .enumerate()
        .map(|(index, rows)| {
            let (key, label) = if index == 0 {
                (key.to_string(), label.to_string())
            } else {
                (
                    format!("{key}/continued-{index}"),
                    format!("{label}, continued"),
                )
            };
            let heading = card_heading(heading.0, heading.1, index > 0, inner);
            let height = fixed
                + rows
                    .iter()
                    .map(|(_, height)| height + CARD_GAP)
                    .sum::<f32>();
            let mut children = vec![
                UiNode::new(
                    format!("{key}/heading"),
                    UiContent::Text(TextSpec {
                        text: heading,
                        role: SemanticRole::Heading,
                        style: StyleRef::Quiet,
                    }),
                )
                .layout(Layout::default().height(Length::Fixed(heading_height))),
            ];
            children.extend(rows.into_iter().map(|(row, _)| row));
            let card = UiNode::new(
                key,
                UiContent::Group(GroupSpec::new(label).style(StyleRef::Secondary)),
            )
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fixed(height))
                    .padding(Insets::all(pad))
                    .gap(CARD_GAP)
                    .clipped(),
            )
            .children(children);
            (card, height)
        })
        .collect()
}

/// `rows` in order, greedily grouped so each group's rows (and the gap above
/// each) fit `room`; every group holds at least one row. Pure.
fn chunk_rows(rows: Vec<(UiNode, f32)>, room: f32) -> Vec<Vec<(UiNode, f32)>> {
    let mut chunks: Vec<Vec<(UiNode, f32)>> = Vec::new();
    let mut current = Vec::new();
    let mut used = 0.0_f32;
    for (row, height) in rows {
        let cost = height + CARD_GAP;
        if !current.is_empty() && used + cost > room {
            chunks.push(std::mem::take(&mut current));
            used = 0.0;
        }
        used += cost;
        current.push((row, height));
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// The page's three actions under the STATUS card: Refresh (busy while the
/// ledgers are read), Open aterm.toml, and AI CLIs (its sub-page, Settings ▸
/// Harness ▸ AI CLIs). Side by side where every label fits whole, else
/// stacked.
fn harness_actions(harness: &HarnessProjection, card_width: f32) -> (UiNode, f32) {
    let control_height = scaled_control_height();
    let need = |label: &str| crate::native_ui::native_button_label_width(label) + 40.0;
    let row = 3.0 * need("Open aterm.toml") + 2.0 * CARD_GAP <= card_width;
    let button = |key: &str, label: &str, busy: bool, style: StyleRef| {
        // Stacked, a button is the card's width: its label is fitted to it.
        let visual = crate::native_ui::fit_native_button_label(label, (card_width - 40.0).max(0.0));
        let action = if key == HARNESS_AI_CLIS {
            route_action(SettingsRoute::Ai)
        } else {
            ActionId::new(key)
        };
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(ButtonSpec::new(label).visual_label(visual), action)
                    .state(ControlState {
                        busy,
                        ..ControlState::default()
                    })
                    .style(style),
            ),
        )
        .layout(
            Layout::default()
                .width(if row {
                    Length::Fixed(need("Open aterm.toml"))
                } else {
                    Length::Fill
                })
                .height(Length::Fixed(control_height)),
        )
    };
    let buttons = vec![
        button(
            HARNESS_REFRESH,
            "Refresh",
            harness.collecting,
            StyleRef::Primary,
        ),
        button(
            HARNESS_OPEN_CONFIG,
            "Open aterm.toml",
            false,
            StyleRef::Secondary,
        ),
        button(HARNESS_AI_CLIS, "AI CLIs", false, StyleRef::Secondary),
    ];
    let height = if row {
        control_height
    } else {
        3.0 * control_height + 2.0 * CARD_GAP
    };
    let layout = if row { Layout::row() } else { Layout::column() };
    (
        UiNode::new(
            "harness/actions",
            UiContent::Group(GroupSpec::new("Harness actions")),
        )
        .layout(layout.height(Length::Fixed(height)).gap(CARD_GAP))
        .children(buttons),
        height,
    )
}

/// The SUPERVISION card's rows, each the way every page draws a setting
/// ([`page_setting_rows_noted`]): its note and its Changed tag and Reset with
/// it. A note two rows share (Answer questions and Question priority) is
/// drawn once, under the last of them ([`prefs::row_notes`]). A row and its
/// note taller than a card's `room` together (large type on a short host)
/// list the note as the card's next rows.
fn switch_rows(
    state: &SettingsViewState,
    width: SettingsWidth,
    inner: f32,
    room: f32,
) -> Vec<(UiNode, f32)> {
    let keys: Vec<&str> = harness_page_rows()
        .filter(|key| state.field_index_of(key).is_some())
        .collect();
    let notes = prefs::row_notes(&keys);
    // What one row of a continued card may take: the room under its heading,
    // at the tight padding.
    let heading_height = 22.0_f32.max(16.0 * settings_text_scale());
    let row_room = (room - 2.0 * TIGHT_CARD_PAD - heading_height - CARD_GAP).max(0.0);
    keys.iter()
        .zip(notes)
        .flat_map(|(key, note)| page_setting_rows_noted(state, key, width, inner, note, row_room))
        .collect()
}

/// "No Claude Code or Codex session is open": every agent the harness
/// supervises or watches (`Program::ALL`, `harness_table`) by its product
/// name, so a new agent's sessions are named here as its Settings row is,
/// with no edit.
fn no_session_open() -> String {
    let products: Vec<&str> = aterm_phase::Program::ALL
        .into_iter()
        .filter(|p| p.harness_table())
        .map(|p| p.product())
        .collect();
    let named = match products.split_last() {
        Some((last, [])) => (*last).to_string(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => "coding agent".to_string(),
    };
    format!("No {named} session is open")
}

/// The SESSIONS card's rows: one per live session of an agent the harness
/// supervises, or the one line that says none is open (or that the page is
/// still reading).
fn session_rows(
    harness: &HarnessProjection,
    width: SettingsWidth,
    (inner, room): (f32, f32),
    timer_open: Option<u64>,
    selected: Option<u64>,
) -> Vec<(UiNode, f32)> {
    if harness.sessions.is_empty() {
        let text = if harness.observed {
            no_session_open()
        } else {
            "Reading the sessions\u{2026}".to_string()
        };
        let height = 22.0_f32.max(18.0 * settings_text_scale());
        return vec![(
            UiNode::new(
                "harness/sessions/empty",
                UiContent::Text(TextSpec {
                    text: crate::native_ui::fit_native_text_label(&text, inner),
                    role: SemanticRole::Text,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(Layout::default().height(Length::Fixed(height))),
            height,
        )];
    }
    let offer = harness.land_at_offer;
    // What one row of a continued card may take, as `switch_rows` reads it.
    let heading_height = 22.0_f32.max(16.0 * settings_text_scale());
    let row_room = (room - 2.0 * TIGHT_CARD_PAD - heading_height - CARD_GAP).max(0.0);
    let measure = RowMeasure::of(&harness.sessions, width, (inner, row_room));
    harness
        .sessions
        .iter()
        .map(|row| {
            let open = timer_open == Some(row.local_id);
            let landed = selected == Some(row.local_id);
            let (node, height) = session_row(row, open, landed, measure, inner);
            session_timer(
                row,
                (node, height),
                row.default.is_some(),
                offer,
                open,
                inner,
            )
        })
        .collect()
}

/// A SESSIONS row under its STATE LINE and its open TIMER (§9.3.4): the
/// `row` node as built (its `Timer` disclosure beside the mode buttons,
/// [`session_row`]), then — where its glass says something — the state line
/// (`lands at 18:00`, `landed 17:42`, `last: ✓ approved 14:05`: clock times
/// only), then, the disclosure open, `1 h`, `2 h`, `4 h`, `at 18:00` (with
/// `[harness] land_at` set, `offer`) and `Cancel` (while a timer is armed),
/// in as many lines as the column needs. Closed and with nothing to say, the
/// row is as tall as v1's. The timer's buttons act only where the modes do
/// (`enabled`). One column.
fn session_timer(
    row: &HarnessSessionRow,
    (node, height): (UiNode, f32),
    enabled: bool,
    offer: Option<crate::claude_lights::LandAtGlass>,
    open: bool,
    inner: f32,
) -> (UiNode, f32) {
    let scale = settings_text_scale();
    let line_height = 18.0_f32.max(16.0 * scale);
    let control_height = scaled_control_height();
    let key = format!("harness/session/{}", row.local_id);
    let mut children = vec![node];
    let mut total = height;
    if let Some(line) = row.state_line() {
        children.push(
            UiNode::new(
                format!("{key}/state"),
                UiContent::Text(TextSpec {
                    text: crate::native_ui::fit_native_status_label(&line, inner),
                    role: SemanticRole::Status,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(line_height)),
            ),
        );
        total += line_height + 4.0;
    }
    // Its day (§10.3.1): `today: needed you once, 1m at 18:29 · 31 approved`,
    // cut at a joint where the column is narrow; its label whole.
    if let Some(day) = &row.day_line {
        let shown = cut_at_joint(
            day,
            |t| fits_in(t, inner, SemanticRole::Status, StyleRef::Quiet),
            |t| crate::native_ui::fit_native_status_label(t, inner),
        );
        children.push(
            UiNode::new(
                format!("{key}/today"),
                UiContent::Group(GroupSpec::new(day.clone())),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(line_height)),
            )
            .children(vec![
                UiNode::new(
                    format!("{key}/today/text"),
                    UiContent::Text(TextSpec {
                        text: shown,
                        role: SemanticRole::Status,
                        style: StyleRef::Quiet,
                    }),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fill)
                        .height(Length::Fixed(line_height)),
                )
                .paint_only(),
            ]),
        );
        total += line_height + 4.0;
    }
    // Each button as wide as its words, and the buttons laid in as many rows
    // as the column needs: a phone page holds two or three a row.
    let button =
        |action: String, label: String, visual: &str, enabled: bool, expanded: Option<bool>| {
            let width = (crate::native_ui::native_button_label_width(visual) + 40.0).min(inner);
            let node = UiNode::new(
                action.clone(),
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new(label).visual_label(
                            crate::native_ui::fit_native_button_label(
                                visual,
                                (width - 24.0).max(0.0),
                            ),
                        ),
                        ActionId::new(action),
                    )
                    .state(ControlState {
                        enabled,
                        expanded,
                        ..ControlState::default()
                    })
                    .style(StyleRef::Quiet),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(width))
                    .height(Length::Fill),
            );
            (node, width)
        };
    let mut buttons = Vec::new();
    if open {
        for minutes in crate::autopilot_menu::LAND_IN_MINUTES {
            let hours = minutes / 60;
            let words = if hours == 1 {
                "1 hour".to_string()
            } else {
                format!("{hours} hours")
            };
            buttons.push(button(
                timer_action(row.local_id, TimerChoice::In(minutes)),
                format!("Cruise for {words}, then land"),
                &format!("{hours} h"),
                enabled,
                None,
            ));
        }
        if let Some(at) = offer {
            buttons.push(button(
                timer_action(row.local_id, TimerChoice::At),
                format!(
                    "Cruise until {}, then land",
                    crate::autopilot_menu::spoken_when(at)
                ),
                &format!("at {}", crate::autopilot_menu::when(at)),
                enabled,
                None,
            ));
        }
        if row.armed_at_ms.is_some() {
            buttons.push(button(
                timer_action(row.local_id, TimerChoice::Cancel),
                "Cancel the landing time".to_string(),
                "Cancel",
                enabled,
                None,
            ));
        }
    }
    let mut lines: Vec<Vec<UiNode>> = Vec::new();
    let mut used = 0.0_f32;
    for (node, width) in buttons {
        match lines.last_mut() {
            Some(line) if used + MODE_GAP + width <= inner => {
                used += MODE_GAP + width;
                line.push(node);
            }
            _ => {
                used = width;
                lines.push(vec![node]);
            }
        }
    }
    for (i, line) in lines.into_iter().enumerate() {
        children.push(
            UiNode::new(
                format!("{key}/timer-row/{i}"),
                UiContent::Group(GroupSpec::new("Timer")),
            )
            .layout(
                Layout::row()
                    .height(Length::Fixed(control_height))
                    .gap(MODE_GAP),
            )
            .children(line),
        );
        total += control_height + 4.0;
    }
    if children.len() == 1 {
        return (children.remove(0), height);
    }
    (
        UiNode::new(
            format!("{key}/with-timer"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().height(Length::Fixed(total)).gap(4.0))
        .children(children),
        total,
    )
}

/// Why a session's Autopilot press writes nothing: the harness steers no
/// mode for it — no host here, the master switch Off, or (the window
/// supervising) its agent's own switch Off, named as its row is.
fn nothing_to_steer(posture: crate::autopilot::HarnessPosture, product: &str) -> String {
    match posture {
        crate::autopilot::HarnessPosture::Unhosted => {
            "No harness runs here (this aterm has no control socket), so Autopilot has nothing \
             to steer."
                .to_string()
        }
        crate::autopilot::HarnessPosture::MasterOff => {
            "The harness is off here, so Autopilot has nothing to steer.".to_string()
        }
        crate::autopilot::HarnessPosture::On(_)
        | crate::autopilot::HarnessPosture::Restricted(..)
        | crate::autopilot::HarnessPosture::AgentOff(_) => {
            format!(
                "{} is Off, so Autopilot has nothing to steer there.",
                crate::prefs::agent_switch_label(product)
            )
        }
    }
}

/// A mode's button label.
const fn mode_label(mode: Autopilot) -> &'static str {
    match mode {
        Autopilot::Cruise => "Cruise",
        Autopilot::Land => "Land",
        Autopilot::Off => "Off",
    }
}

/// What a mode does, for its button's description.
const fn mode_description(mode: Autopilot) -> &'static str {
    match mode {
        Autopilot::Cruise => "keep the work moving until the agent's clear final statement",
        Autopilot::Land => "wrap the work up cleanly, then stop",
        Autopilot::Off => "the harness keeps its hands off this session",
    }
}

/// The five faces of a SESSIONS row's buttons, in their order: the three
/// modes, `Timer`, `Show`.
const ROW_FACES: [&str; 5] = ["Cruise", "Land", "Off", "Timer", "Show"];

/// HOW THE SESSIONS ROWS ARE LAID OUT at the page's measure (§10.3.4, "A
/// narrow page"): one decision for every row, so the list reads as one.
/// The words go beside the buttons only where the column left to them holds
/// every row's place whole (`Tab 1, left pane`: what tells two agents of one
/// tab apart, [`HarnessSessionRow::place`]); else they go over the buttons,
/// which share the row's width, as many to a line as hold their faces whole
/// — all five; else the three modes, then `Timer` and `Show`; else two to a
/// line; else one — but never more lines than let the row, with its state
/// and day lines, fit one card of the page (`row_room`): there the faces are
/// cut, their labels whole. A fixed button is as wide as its widest face.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowMeasure {
    /// A fixed button's width, beside the words.
    button_width: f32,
    /// The words go over the buttons.
    stacked: bool,
    /// Stacked, how many buttons each line holds, in their order.
    lines: &'static [u8],
}

impl RowMeasure {
    fn of(rows: &[HarnessSessionRow], width: SettingsWidth, (inner, row_room): (f32, f32)) -> Self {
        let scale = settings_text_scale();
        let face = ROW_FACES
            .iter()
            .map(|face| crate::native_ui::native_button_label_width(face))
            .fold(0.0_f32, f32::max);
        let button_width = (76.0 * scale.min(1.25)).max(64.0).max(face + 20.0);
        // The place whole is the place with the cut's ellipsis after it: a
        // detail cut right after its place still says all of it.
        let place = rows
            .iter()
            .map(|row| {
                crate::native_ui::native_status_label_width(&format!("{}\u{2026}", row.place()))
            })
            .fold(0.0_f32, f32::max);
        let words = inner - 5.0 * (button_width + MODE_GAP);
        let stacked = width == SettingsWidth::Compact || words < place;
        const FLOWS: [&[u8]; 4] = [&[5], &[3, 2], &[2, 2, 1], &[1, 1, 1, 1, 1]];
        let holds = |flow: &[u8]| {
            flow.iter()
                .all(|&count| line_button(inner, count) - 20.0 >= face)
        };
        // The tallest row a flow makes: its words, its lines of buttons, and
        // the state and day lines under it ([`session_timer`]).
        let line_height = 18.0_f32.max(16.0 * scale);
        let under = rows
            .iter()
            .map(|row| {
                let lines = [row.state_line().is_some(), row.day_line.is_some()];
                lines.iter().filter(|line| **line).count()
            })
            .max()
            .unwrap_or(0);
        let under = f32::from(u8::try_from(under).unwrap_or(u8::MAX)) * (line_height + 4.0);
        let fits = |flow: &[u8]| {
            let lines = f32::from(u8::try_from(flow.len()).unwrap_or(u8::MAX));
            stacked_row_height(lines) + under <= row_room
        };
        let lines: &'static [u8] = if stacked {
            let want = FLOWS
                .iter()
                .position(|&flow| holds(flow))
                .unwrap_or(FLOWS.len() - 1);
            FLOWS[..=want]
                .iter()
                .rev()
                .find(|&&flow| fits(flow))
                .copied()
                .unwrap_or(FLOWS[0])
        } else {
            FLOWS[0]
        };
        Self {
            button_width,
            stacked,
            lines,
        }
    }

    /// The width of the `index`th button (in [`ROW_FACES`]' order).
    fn button(self, inner: f32, index: usize) -> f32 {
        if !self.stacked {
            return self.button_width;
        }
        let mut start = 0;
        for &count in self.lines {
            start += usize::from(count);
            if index < start {
                return line_button(inner, count);
            }
        }
        inner
    }
}

/// A stacked row's height with `lines` lines of buttons: its title and
/// detail over them ([`session_row`]).
fn stacked_row_height(lines: f32) -> f32 {
    let scale = settings_text_scale();
    let title_height = 22.0_f32.max(18.0 * scale);
    let detail_height = 18.0_f32.max(16.0 * scale);
    title_height + detail_height + lines * scaled_control_height() + (1.0 + lines) * 4.0
}

/// The width each of `count` buttons sharing a line of `inner` has.
fn line_button(inner: f32, count: u8) -> f32 {
    let count = f32::from(count.max(1));
    (inner - (count - 1.0) * MODE_GAP) / count
}

/// One session's row: its title and detail (fitted to the text column; the
/// row's group carries both whole), its Cruise | Land | Off buttons keyed
/// `harness/session/<id>/autopilot/<word>`, the mode it runs under selected,
/// and beside them its `Timer` disclosure (`harness/session/<id>/timer`,
/// `timer_open` its `expanded` state; [`session_timer`] lays what it opens)
/// and its `Show`. They act only while the harness can steer this session
/// (its own posture's default, [`HarnessSessionRow::default`]), whose
/// button's description says it follows the default where a press of it
/// hands back ([`hands_back`]: a default that carries). On a line where
/// `measure` allows; else stacked, the buttons on one line or two
/// ([`RowMeasure`]).
fn session_row(
    row: &HarnessSessionRow,
    timer_open: bool,
    landed: bool,
    measure: RowMeasure,
    inner: f32,
) -> (UiNode, f32) {
    let default = row.default;
    let enabled = default.is_some();
    let scale = settings_text_scale();
    let title_height = 22.0_f32.max(18.0 * scale);
    let detail_height = 18.0_f32.max(16.0 * scale);
    let control_height = scaled_control_height();
    let compact = measure.stacked;
    let button_width = measure.button_width;
    let text_width = if compact {
        inner
    } else {
        (inner - 5.0 * (button_width + MODE_GAP)).max(0.0)
    };
    // Each face is fitted to its own button: stacked, a line's buttons share
    // its width ([`RowMeasure::button`]).
    let face_room = |index: usize| (measure.button(inner, index) - 20.0).max(0.0);
    let key = format!("harness/session/{}", row.local_id);
    let mut buttons: Vec<UiNode> = Autopilot::ALL
        .iter()
        .enumerate()
        .map(|(index, mode)| {
            let action = autopilot_action(row.local_id, *mode);
            let word = mode_label(*mode);
            let mut spec = ButtonSpec::new(format!("Autopilot {} for {}", mode.word(), row.name))
                .visual_label(crate::native_ui::fit_native_button_label(
                    word,
                    face_room(index),
                ));
            spec.description = Some(if default.is_some_and(|d| hands_back(*mode, d)) {
                format!(
                    "{}; the default, so this follows [harness] autopilot",
                    mode_description(*mode)
                )
            } else {
                mode_description(*mode).to_string()
            });
            UiNode::new(
                action.clone(),
                UiContent::Button(
                    Control::new(spec, ActionId::new(action))
                        .state(ControlState {
                            enabled,
                            selected: enabled && row.mode == Some(*mode),
                            ..ControlState::default()
                        })
                        .style(StyleRef::Secondary),
                ),
            )
            .layout(
                Layout::default()
                    .width(if compact {
                        Length::Fill
                    } else {
                        Length::Fixed(button_width)
                    })
                    .height(Length::Fill),
            )
        })
        .collect();
    let toggle = timer_toggle(row.local_id);
    buttons.push(
        UiNode::new(
            toggle.clone(),
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(format!("Timer for {}", row.name)).visual_label(
                        crate::native_ui::fit_native_button_label("Timer", face_room(3)),
                    ),
                    ActionId::new(toggle),
                )
                .state(ControlState {
                    enabled,
                    expanded: Some(timer_open),
                    ..ControlState::default()
                })
                .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(if compact {
                    Length::Fill
                } else {
                    Length::Fixed(button_width)
                })
                .height(Length::Fill),
        ),
    );
    // SHOW raises the session's tab and pane (§10.3.1); drawn selected while
    // the page is landed on this row.
    let show = show_action(row.local_id);
    let mut show_spec = ButtonSpec::new(format!("Show {}", row.name)).visual_label(
        crate::native_ui::fit_native_button_label("Show", face_room(4)),
    );
    show_spec.description = Some("raise its tab and pane".to_string());
    buttons.push(
        UiNode::new(
            show.clone(),
            UiContent::Button(
                Control::new(show_spec, ActionId::new(show))
                    .state(ControlState {
                        selected: landed,
                        ..ControlState::default()
                    })
                    .style(StyleRef::Quiet),
            ),
        )
        .layout(
            Layout::default()
                .width(if compact {
                    Length::Fill
                } else {
                    Length::Fixed(button_width)
                })
                .height(Length::Fill),
        ),
    );
    // The landed row is drawn on the accent-tinted card the page's selected
    // things wear, until the next press or scroll.
    let group = |label: String| {
        let spec = GroupSpec::new(label);
        if landed {
            spec.style(StyleRef::Primary)
        } else {
            spec
        }
    };
    let text = |suffix: &str, text: String, role: SemanticRole, style: StyleRef, height: f32| {
        UiNode::new(
            format!("{key}/{suffix}"),
            UiContent::Text(TextSpec { text, role, style }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
        .paint_only()
    };
    let title = text(
        "title",
        crate::native_ui::fit_native_text_label(&row.title, text_width),
        SemanticRole::Text,
        StyleRef::Primary,
        title_height,
    );
    let detail = text(
        "detail",
        crate::native_ui::fit_native_status_label(&row.detail, text_width),
        SemanticRole::Status,
        StyleRef::Quiet,
        detail_height,
    );
    let label = format!("{}: {}", row.title, row.detail);
    if compact {
        let line = |suffix: &str, name: &str, buttons: Vec<UiNode>| {
            UiNode::new(
                format!("{key}/{suffix}"),
                UiContent::Group(GroupSpec::new(name)),
            )
            .layout(
                Layout::row()
                    .height(Length::Fixed(control_height))
                    .gap(MODE_GAP),
            )
            .children(buttons)
        };
        let mut children = vec![title, detail];
        // The first line keeps v1's key and name (`…/modes`, Autopilot); the
        // next are `…/line/<n>`, named by their faces (`Timer, Show`).
        let mut rest = buttons.into_iter();
        let mut start = 0;
        for (n, &count) in measure.lines.iter().enumerate() {
            let count = usize::from(count);
            let on_line: Vec<UiNode> = rest.by_ref().take(count).collect();
            children.push(if n == 0 {
                line("modes", "Autopilot", on_line)
            } else {
                let faces = ROW_FACES.get(start..start + count).unwrap_or_default();
                line(&format!("line/{n}"), &faces.join(", "), on_line)
            });
            start += count;
        }
        let height = stacked_row_height(f32::from(
            u8::try_from(measure.lines.len()).unwrap_or(u8::MAX),
        ));
        return (
            UiNode::new(key, UiContent::Group(group(label)))
                .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
                .children(children),
            height,
        );
    }
    let height = control_height.max(title_height + detail_height);
    let words = UiNode::new(
        format!("{key}/words"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::column().width(Length::Fill).height(Length::Fill))
    .children(vec![title, detail]);
    let mut children = vec![words];
    children.extend(buttons);
    (
        UiNode::new(key, UiContent::Group(group(label)))
            .layout(Layout::row().height(Length::Fixed(height)).gap(MODE_GAP))
            .children(children),
        height,
    )
}
