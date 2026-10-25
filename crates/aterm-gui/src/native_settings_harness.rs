// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Settings Harness page ([`HarnessState`]).

use super::*;
use crate::autopilot_timers::TimerChoice;
use crate::harness_ab::AbLead;
use crate::harness_screen::{
    EveryPress, EveryRow, HarnessSessionRow, autopilot_action, every_action, hands_back,
    parse_autopilot_action, parse_every_action,
};
use crate::native_app::{HarnessRequest, HarnessTimerRequest};
use aterm_agent::supervise::Autopilot;

/// Every action the page mints starts with this.
pub(super) const HARNESS_ACTION_PREFIX: &str = "harness/";

/// The Refresh button's key and action.
pub(super) const HARNESS_REFRESH: &str = "harness/refresh";

/// The Open aterm.toml button's key and action.
pub(super) const HARNESS_OPEN_CONFIG: &str = "harness/open-config";

/// The SETTINGS IN ATERM.TOML card's disclosure.
pub(super) const HARNESS_CONFIG_TOGGLE: &str = "harness/config/toggle";

/// DECIDED FOR YOU's disclosure (§10.4.1).
pub(super) const HARNESS_DECIDED_ALL: &str = "harness/decided/all";

/// How many of today's choices DECIDED FOR YOU shows, its disclosure closed.
const DECIDED_SHOWN: usize = 3;

/// The most lines one choice's sentence is drawn on (its label keeps it whole).
const DECIDED_LINES: usize = 4;

/// A SESSIONS row's timer press: `harness/session/<id>/land-in/<minutes>`, `…/land-at` or
/// `…/land-cancel`.
pub(super) fn timer_action(local_id: u64, choice: TimerChoice) -> String {
    let prefix = crate::harness_screen::SESSION_ACTION_PREFIX;
    match choice {
        TimerChoice::In(minutes) => format!("{prefix}{local_id}/land-in/{minutes}"),
        TimerChoice::At => format!("{prefix}{local_id}/land-at"),
        TimerChoice::Cancel => format!("{prefix}{local_id}/land-cancel"),
    }
}

/// [`timer_action`] read back.
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

/// A SESSIONS row's Timer disclosure.
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

/// A SESSIONS row's `Show`: `harness/session/<id>/show`.
pub(super) fn show_action(local_id: u64) -> String {
    format!(
        "{}{local_id}/show",
        crate::harness_screen::SESSION_ACTION_PREFIX
    )
}

/// The `Every session` row's Timer disclosure.
pub(super) fn every_timer_toggle() -> String {
    format!(
        "{}{}/timer",
        crate::harness_screen::SESSION_ACTION_PREFIX,
        crate::harness_screen::EVERY_ID
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

/// The key the page lands on for `focus`.
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

/// The AI CLIs button's key.
pub(super) const HARNESS_AI_CLIS: &str = "harness/ai-clis";

/// The Tab Control button's key.
pub(super) const HARNESS_TAB_CONTROL: &str = "harness/tab-control";

/// The page's heading (its rail label is the route's, "Harness").
pub(super) const HARNESS_PAGE_TITLE: &str = "Coding agent harness";

/// The SUPERVISION card's registry rows: the Harness section's but the AI CLIs'
/// ([`super::ai_page::ai_page_rows`]).
pub(super) fn harness_page_rows() -> impl Iterator<Item = &'static str> {
    prefs::NESTED_LEAVES
        .iter()
        .map(|leaf| leaf.key)
        .filter(|key| prefs::section_of(key) == prefs::Section::Harness)
        .filter(|key| !key.starts_with(super::ai_page::AI_KEY_PREFIX))
}

/// A card's padding.
pub(super) const CARD_PAD: f32 = ABOUT_METADATA_CARD_PADDING;
pub(super) const TIGHT_CARD_PAD: f32 = 8.0;
pub(super) const CARD_GAP: f32 = 6.0;

/// The gap between a session row's three buttons.
const MODE_GAP: f32 = 6.0;

/// The page's subtitle, which yields to nothing.
pub(super) fn harness_page_subtitle(width: SettingsWidth) -> &'static str {
    if width == SettingsWidth::Compact {
        "Autopilot, switches, and stats."
    } else {
        "Switches, each session's Autopilot, what it did, and its settings."
    }
}

/// The page's heading on a page `width` wide.
fn harness_page_title(width: f32) -> &'static str {
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Hero);
    if crate::tray_raster::ui_text_width_for(face, HARNESS_PAGE_TITLE, px) <= width {
        HARNESS_PAGE_TITLE
    } else {
        SettingsRoute::Harness.label()
    }
}

/// A card's heading at `width`.
pub(super) fn card_heading(full: &str, short: &str, width: f32) -> String {
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Heading, StyleRef::Quiet);
    let fits = |text: &str| {
        crate::tray_raster::ui_text_width_for(face, text, px)
            <= width + crate::native_ui::TEXT_FIT_SLACK
    };
    [full, short]
        .into_iter()
        .find(|text| fits(text))
        .unwrap_or(full)
        .to_string()
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
    /// A press on the page ([`HARNESS_ACTION_PREFIX`], [`HarnessRequest`]).
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
            let Some(SemanticInput::Text(words)) = value else {
                return EventResult::Bubble;
            };
            let Some(focus) = crate::harness_screen::HarnessFocus::parse(words) else {
                return EventResult::Bubble;
            };
            view.harness_focus = focus;
            match landing_key(focus) {
                Some(key) if matches!(focus, crate::harness_screen::HarnessFocus::Session(_)) => {
                    view.land_on_stop(key);
                }
                Some(key) => view.land_on_control(key),
                None => view.land_at_top(),
            }
        } else if let Some(session) = parse_show_action(action) {
            // Navigation writes nothing: no posture is weighed. A SESSIONS
            // row's Show, or a DECIDED FOR YOU / TAB CONTROL row's Show tab
            // (whose session may be one SESSIONS does not list: its agent has
            // exited or it is a plain shell tab).
            if self.harness.sessions.iter().any(|s| s.local_id == session)
                || self
                    .harness
                    .decided_open
                    .iter()
                    .any(|s| s.local_id == session)
                || self
                    .harness
                    .tabs
                    .tabs
                    .iter()
                    .any(|r| r.show_local_id == Some(session))
            {
                cx.harness(HarnessRequest::Show { session });
            } else {
                view.feedback = Some("That session is no longer open.".to_string());
            }
        } else if action == HARNESS_CONFIG_TOGGLE {
            view.harness_config_open = !view.harness_config_open;
            view.invalidate_result_page_limit();
        } else if action == HARNESS_DECIDED_ALL {
            view.harness_decided_open = !view.harness_decided_open;
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
            view.harness_timer_open = (view.harness_timer_open != Some(session)).then_some(session);
            if view.harness_timer_open.is_some() {
                view.harness_every_timer_open = false;
                view.reveal_pressed(timer_opening_reveal(Some(session)));
            } else {
                view.drop_list_nudge();
            }
            view.invalidate_result_page_limit();
        } else if action == every_timer_toggle() {
            view.harness_every_timer_open = !view.harness_every_timer_open;
            if view.harness_every_timer_open {
                view.harness_timer_open = None;
                view.reveal_pressed(timer_opening_reveal(None));
            } else {
                view.drop_list_nudge();
            }
            view.invalidate_result_page_limit();
        } else if let Some(press) = parse_every_action(action) {
            match self.harness.every_row() {
                None => {
                    view.feedback = Some("There is no other session to set with it.".to_string())
                }
                Some(every) if every.steered == 0 => {
                    view.feedback = Some(match every.product {
                        Some(product) => nothing_to_steer(self.harness.posture, product),
                        None => match self.harness.posture {
                            posture @ (crate::autopilot::HarnessPosture::Unhosted
                            | crate::autopilot::HarnessPosture::MasterOff) => {
                                nothing_to_steer(posture, "")
                            }
                            _ => "Each agent's own switch is Off, so Autopilot has nothing to \
                                  steer there."
                                .to_string(),
                        },
                    });
                }
                Some(_) => cx.harness(HarnessRequest::Every(press)),
            }
        } else if let Some((session, choice)) = parse_timer_action(action) {
            match self.harness.sessions.iter().find(|s| s.local_id == session) {
                None => view.feedback = Some("That session is no longer open.".to_string()),
                Some(open) if open.default.is_none() => {
                    view.feedback = Some(nothing_to_steer(self.harness.posture, open.program));
                }
                Some(_) => cx.harness_timer(HarnessTimerRequest { session, choice }),
            }
        } else if let Some((session, word)) = parse_autopilot_action(action) {
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

/// THE PAGE: its heading pinned, then its cards as one scrolling list.
pub(super) fn harness_page(
    state: &SettingsViewState,
    harness: &HarnessProjection,
    update: &UpdateProjection,
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
    let maximum = PageLayout::Form.maximum(width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (heading, heading_height) = pinned_heading(
        state,
        width,
        viewport,
        gap,
        harness_page_title(page_width),
        harness_page_subtitle(width),
    );
    let room = (settings_list_body_height(state, width, viewport) - heading_height)
        .max(scaled_control_height());
    let cards = harness_cards(state, harness, update, width, page_width, (room, gap));
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

/// The page while a choice picker for one of its rows is open.
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
    let maximum = PageLayout::Form.maximum(width);
    let (mut out, _) = pinned_heading(
        state,
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

/// Every card of the page.
fn harness_cards(
    state: &SettingsViewState,
    harness: &HarnessProjection,
    update: &UpdateProjection,
    width: SettingsWidth,
    card_width: f32,
    (room, gap): (f32, f32),
) -> Vec<(UiNode, f32)> {
    let inner = card_text_width(card_width);
    let facts = |key: &str, heading: (&str, &str), rows: &[(String, String)]| {
        metadata_cards(
            key,
            heading,
            rows,
            card_width,
            fact_label_width(rows, inner),
        )
    };
    let rows_width = width;
    let mut cards = Vec::new();
    let mut status = facts("harness/status", ("STATUS", "STATUS"), &harness.status_rows);
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
    let selected = match state.harness_focus {
        crate::harness_screen::HarnessFocus::Session(id) => Some(id),
        _ => None,
    };
    cards.extend(row_cards_inset(
        "harness/sessions",
        ("Sessions", ("SESSIONS", "SESSIONS")),
        session_rows(
            harness,
            rows_width,
            inner,
            (state.harness_timer_open, state.harness_every_timer_open),
            selected,
        ),
        (room, card_width),
        true,
    ));
    if !harness.decided.is_empty() {
        cards.extend(row_cards(
            "harness/decided",
            ("Decided for you", ("DECIDED FOR YOU", "DECIDED")),
            decided_rows(harness, inner, state.harness_decided_open),
            room,
            card_width,
        ));
    }
    cards.push(harness_actions(harness, card_width));
    let mut effect = facts(
        "harness/ab",
        ("EFFECT OF EACH AUTOPILOT", "EFFECT"),
        &harness.ab_rows,
    );
    if let (Some(lead), Some((card, height))) = (&harness.ab_lead, effect.first_mut()) {
        let (block, block_height) = ab_lead_block(lead, inner);
        card.children.insert(1, block);
        *height += 4.0 + block_height;
        card.layout = card.layout.height(Length::Fixed(*height));
    }
    cards.extend(effect);
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
    let config = config_facts(harness, state.harness_config_open);
    let px = 13.0 * crate::native_appearance::text_scale();
    let key_width = config
        .iter()
        .map(|(key, _)| metadata_value_text_width(key, px, false))
        .fold(0.0_f32, f32::max)
        + ABOUT_METADATA_TEXT_SAFETY;
    let config_label_width = key_width
        .min(inner * 0.4)
        .max(about_metadata_label_width().min(inner * 0.4));
    cards.extend(metadata_cards(
        "harness/config",
        ("SETTINGS IN ATERM.TOML", "ATERM.TOML"),
        &config,
        card_width,
        config_label_width,
    ));
    if harness.config_rows.len() > harness.config_own.len() {
        cards.push(config_disclosure(
            state.harness_config_open,
            harness.config_rows.len(),
            card_width,
        ));
    }
    cards.extend(facts(
        "harness/updates",
        ("HARNESS UPDATES", "UPDATES"),
        &harness_update_rows(harness, update),
    ));
    cards.push(harness_update_actions(card_width));
    cards
}

/// Running harness provenance and installed CLI facts are distinct.
fn harness_update_rows(
    harness: &HarnessProjection,
    update: &UpdateProjection,
) -> Vec<(String, String)> {
    let cli = |id| {
        harness
            .ai
            .clis
            .iter()
            .find(|row| row.id == id)
            .map(|row| format!("{} · {}", row.title, row.detail))
            .unwrap_or_else(|| "Not read yet. Open AI CLI versions to refresh.".to_string())
    };
    vec![
        (
            "Claude harness".to_string(),
            // The Claude Code harness's own version, built into this aterm build.
            format!(
                "v{} (built-in) · in aterm {} ({})",
                aterm_phase::CliHarness::ClaudeCode.version_str(),
                crate::build_info::version(),
                crate::build_info::git_commit()
            ),
        ),
        ("aterm updates".to_string(), update.headline.clone()),
        ("CLI observations".to_string(), harness.ai.health.clone()),
        ("Claude CLI".to_string(), cli("claude")),
        ("Codex CLI".to_string(), cli("codex")),
        (
            "Delivery".to_string(),
            "Claude's harness updates with aterm. Published CLI builds arrive through Packages. Sessions resume on a replacement when it is safe.".to_string(),
        ),
        (
            "This session".to_string(),
            "In codex-alab, /alab update shows the running build and pending update. codex --version and claude --version show the installed CLI.".to_string(),
        ),
    ]
}

/// Existing update and version pages remain the places that check, install, and report failures.
fn harness_update_actions(card_width: f32) -> (UiNode, f32) {
    let buttons = [
        ("software", "Software Update", SettingsRoute::SoftwareUpdate),
        ("packages", "Packages", SettingsRoute::Packages),
        ("versions", "AI CLI versions", SettingsRoute::Ai),
    ]
    .map(|(key, label, route)| {
        let (visual, lines) = button_words(label, &[], card_width);
        let height =
            crate::native_ui::native_button_height_for_lines(lines, scaled_control_height());
        (
            UiNode::new(
                format!("harness/updates/{key}"),
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new(label).visual_label(visual),
                        route_action(route),
                    )
                    .style(StyleRef::Secondary),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            ),
            height,
        )
    });
    let total = buttons.iter().map(|(_, height)| height).sum::<f32>() + 2.0 * CARD_GAP;
    (
        UiNode::new(
            "harness/update-actions",
            UiContent::Group(GroupSpec::new("Harness updates")),
        )
        .layout(
            Layout::column()
                .width(Length::Fixed(card_width))
                .height(Length::Fixed(total))
                .gap(CARD_GAP),
        )
        .children(buttons.into_iter().map(|(node, _)| node).collect()),
        total,
    )
}

/// Whether `text` is whole at `width` in the type `role` and `style` paint.
fn fits_in(text: &str, width: f32, role: SemanticRole, style: StyleRef) -> bool {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    crate::tray_raster::ui_text_width_for(face, text, px) <= width
}

/// Today's status ([`crate::harness_screen::HarnessState::today_lines`],
/// [`crate::harness_screen::today_counts`]).
pub(super) fn today_block(
    lines: &(String, String),
    inner: f32,
    budget: f32,
) -> Option<(UiNode, f32)> {
    let (first, second) = lines;
    let scale = settings_text_scale();
    let plain_height = 28.0_f32.max(18.0 * scale);
    let hero_height = page_heading_height();
    let line = |suffix: &str, text: &str, hero: bool, slot: f32| -> (UiNode, f32) {
        let key = format!("harness/status/today/{suffix}");
        let (mut node, height) = if hero {
            (
                UiNode::new(
                    key,
                    UiContent::Text(TextSpec {
                        text: text.to_string(),
                        role: SemanticRole::Heading,
                        style: StyleRef::Hero,
                    }),
                ),
                slot,
            )
        } else {
            let (node, height) = wrapped_words_node(
                &key,
                text,
                (SemanticRole::Text, StyleRef::Primary),
                inner,
                plain_height,
            );
            (node, height.max(slot))
        };
        node.layout = node
            .layout
            .width(Length::Fill)
            .height(Length::Fixed(height));
        (node.paint_only(), height)
    };
    let tall = fits_in(first, inner, SemanticRole::Heading, StyleRef::Hero);
    let counts = crate::harness_screen::today_counts(first);
    let more = !second.is_empty();
    let shapes = [(tall, more), (false, more), (false, false)];
    let build = |(tall, more): (bool, bool)| {
        let slot = if tall { hero_height } else { plain_height };
        let (head, mut height) = line("hero", first, tall && counts, slot);
        let mut children = vec![head];
        if more {
            let (next, next_height) = line("more", second, false, plain_height);
            children.push(next);
            height += 4.0 + next_height;
        }
        (children, height)
    };
    let (children, height) = shapes
        .into_iter()
        .map(build)
        .find(|(_, height)| *height <= budget)?;
    let label = if second.is_empty() {
        first.clone()
    } else {
        format!("{first}{}{second}", crate::native_ui::FACT_SEPARATOR)
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

/// The room between a lead share's Hero figure and its plain words.
const LEAD_WORD_GAP: f32 = 6.0;

/// The room between the lead's two sides.
const LEAD_SIDE_GAP: f32 = 24.0;

/// `text` as `width` lines in plain type, each whole.
fn lead_lines(tokens: &[(&str, String)], width: f32) -> Vec<String> {
    let fits = |text: &str| fits_in(text, width, SemanticRole::Text, StyleRef::Primary);
    let mut pieces: Vec<(&str, &str)> = Vec::new();
    for (join, token) in tokens {
        if fits(token) {
            pieces.push((join, token));
            continue;
        }
        for (index, word) in token.split(' ').enumerate() {
            pieces.push((if index == 0 { join } else { " " }, word));
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for (join, piece) in pieces {
        let candidate = if current.is_empty() {
            piece.to_string()
        } else {
            format!("{current}{join}{piece}")
        };
        if current.is_empty() || fits(&candidate) {
            current = candidate;
        } else {
            lines.push(std::mem::take(&mut current));
            current = piece.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
        .into_iter()
        .map(|line| {
            if fits(&line) {
                line
            } else {
                crate::native_ui::fit_native_text_label(&line, width)
            }
        })
        .collect()
}

/// Autopilot lead block ([`crate::harness_ab::ab_lead`], [`AbLead::words`], [`lead_lines`]).
pub(super) fn ab_lead_block(lead: &AbLead, inner: f32) -> (UiNode, f32) {
    let plain_height = 28.0_f32.max(18.0 * settings_text_scale());
    let hero_height = page_heading_height();
    let width_of = |text: &str, role: SemanticRole, style: StyleRef| {
        let (px, face) = crate::native_ui::text_paint_metrics(role, style);
        crate::tray_raster::ui_text_width_for(face, text, px) + ABOUT_METADATA_TEXT_SAFETY
    };
    let text = |key: &str,
                text: &str,
                (role, style): (SemanticRole, StyleRef),
                width: Length,
                height: f32| {
        UiNode::new(
            format!("harness/ab/lead/{key}"),
            UiContent::Text(TextSpec {
                text: text.to_string(),
                role,
                style,
            }),
        )
        .layout(Layout::default().width(width).height(Length::Fixed(height)))
        .paint_only()
    };
    let hero = (SemanticRole::Heading, StyleRef::Hero);
    let plain = (SemanticRole::Text, StyleRef::Primary);
    let block = |children: Vec<UiNode>, height: f32| {
        (
            UiNode::new(
                "harness/ab/lead",
                UiContent::Group(GroupSpec::new(lead.words())),
            )
            .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
            .children(children),
            height,
        )
    };
    let lines = |tokens: &[(&str, String)]| {
        let lines = lead_lines(tokens, inner);
        let height = lines.len() as f32 * plain_height + lines.len().saturating_sub(1) as f32 * 4.0;
        let children = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                text(
                    &format!("line-{index}"),
                    line,
                    plain,
                    Length::Fill,
                    plain_height,
                )
            })
            .collect();
        block(children, height)
    };
    let AbLead::Both { cruise, off } = lead else {
        let note = crate::harness_ab::LEAD_NOTE;
        return lines(&[("", note.to_string())]);
    };
    let sides = [("cruise", cruise), ("off", off)];
    let widths = sides.map(|(_, side)| {
        (
            width_of(&side.share, hero.0, hero.1),
            width_of(&side.of, plain.0, plain.1),
        )
    });
    let cap_of = |(role, style): (SemanticRole, StyleRef)| {
        let (px, _) = crate::native_ui::text_paint_metrics(role, style);
        2.0 * crate::tray_raster::row_baseline(0.0, 0.0, px)
    };
    let share_height = hero_height - (cap_of(hero) - cap_of(plain)).max(0.0);
    let whole = widths
        .iter()
        .map(|(share, of)| share + LEAD_WORD_GAP + of)
        .sum::<f32>()
        + LEAD_SIDE_GAP;
    if whole > inner {
        let caption = format!("{}:", crate::harness_ab::LEAD_CAPTION);
        return lines(&[
            ("", caption),
            (" ", cruise.words()),
            (crate::native_ui::FACT_SEPARATOR, off.words()),
        ]);
    }
    let row = sides
        .iter()
        .zip(widths)
        .map(|((key, side), (share_width, of_width))| {
            UiNode::new(
                format!("harness/ab/lead/{key}"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::row()
                    .width(Length::Fixed(share_width + LEAD_WORD_GAP + of_width))
                    .height(Length::Fixed(hero_height))
                    .gap(LEAD_WORD_GAP),
            )
            .paint_only()
            .children(vec![
                text(
                    &format!("{key}/share"),
                    &side.share,
                    hero,
                    Length::Fixed(share_width),
                    share_height,
                ),
                text(
                    &format!("{key}/of"),
                    &side.of,
                    plain,
                    Length::Fixed(of_width),
                    hero_height,
                ),
            ])
        })
        .collect();
    let shares = UiNode::new(
        "harness/ab/lead/shares",
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::row()
            .height(Length::Fixed(hero_height))
            .gap(LEAD_SIDE_GAP),
    )
    .paint_only()
    .children(row);
    let caption = text(
        "caption",
        crate::harness_ab::LEAD_CAPTION,
        plain,
        Length::Fill,
        plain_height,
    );
    block(vec![caption, shares], plain_height + 4.0 + hero_height)
}

/// The `SETTINGS IN ATERM.TOML` card's facts.
fn config_facts(harness: &HarnessProjection, open: bool) -> Vec<(String, String)> {
    if harness.config_rows.is_empty() {
        return vec![("Settings".to_string(), "reading\u{2026}".to_string())];
    }
    if open {
        return harness.config_rows.clone();
    }
    let mut rows = harness.config_own.clone();
    let mine = !harness.config_own.is_empty();
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

/// The disclosure under the SETTINGS IN ATERM.TOML card.
fn config_disclosure(open: bool, keys: usize, card_width: f32) -> (UiNode, f32) {
    let height = scaled_control_height();
    let (label, forms) = if open {
        (
            "Show only yours: the settings you changed".to_string(),
            ["Show only yours".to_string(), "Only yours".to_string()],
        )
    } else {
        (
            format!("Show all {keys} settings"),
            [format!("Show all {keys}"), "Show all".to_string()],
        )
    };
    let need = |words: &str| crate::native_ui::native_button_label_width(words) + 56.0;
    let visual = forms
        .iter()
        .find(|words| need(words) <= card_width + crate::native_ui::TEXT_FIT_SLACK)
        .unwrap_or(&forms[1])
        .clone();
    let width = need(&visual).min(card_width);
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

/// Fact card ([`metadata_card`]) keyed `key` ([`card_heading`], [`FactColumns`]).
pub(super) fn metadata_cards(
    key: &str,
    heading: (&str, &str),
    rows: &[(String, String)],
    card_width: f32,
    label_width: f32,
) -> Vec<(UiNode, f32)> {
    let caption = card_heading(heading.0, heading.1, card_text_width(card_width));
    vec![metadata_card(key, &caption, rows, card_width, label_width)]
}

/// A card of control rows: a Secondary group, its heading, then its rows ([`scroll::SplitCard`]).
pub(super) fn row_cards(
    key: &str,
    names: (&str, (&str, &str)),
    rows: Vec<(UiNode, f32)>,
    room: f32,
    card_width: f32,
) -> Vec<(UiNode, f32)> {
    row_cards_inset(key, names, rows, (room, card_width), false)
}

/// How far a SESSIONS row's words stand inside the block its landing draws the accent card on.
pub(super) const ROW_INSET: f32 = 8.0;

/// [`row_cards`]; with `blocks`, each row's block keeps [`ROW_INSET`] beside its words.
fn row_cards_inset(
    key: &str,
    names: (&str, (&str, &str)),
    rows: Vec<(UiNode, f32)>,
    (room, card_width): (f32, f32),
    blocks: bool,
) -> Vec<(UiNode, f32)> {
    let (label, heading) = names;
    let side = card_side_padding(card_width);
    let inner = card_text_width(card_width);
    let (heading, heading_height) = caption_node(
        &format!("{key}/heading"),
        &card_heading(heading.0, heading.1, inner),
        inner,
    );
    let tallest = rows
        .iter()
        .map(|(_, height)| *height)
        .fold(0.0_f32, f32::max);
    let pad = if 2.0 * CARD_PAD + heading_height + tallest + CARD_GAP <= room {
        CARD_PAD
    } else {
        TIGHT_CARD_PAD
    };
    let (inset_x, inset_y) = if blocks {
        (ROW_INSET, CARD_GAP / 2.0)
    } else {
        (0.0, 0.0)
    };
    let height = 2.0 * pad
        + heading_height
        + rows
            .iter()
            .map(|(_, height)| height + CARD_GAP)
            .sum::<f32>();
    let heading = if blocks {
        UiNode::new(
            format!("{key}/heading-frame"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .height(Length::Fixed(heading_height + inset_y))
                .padding(Insets {
                    top: 0.0,
                    right: inset_x,
                    bottom: inset_y,
                    left: inset_x,
                }),
        )
        .paint_only()
        .children(vec![heading])
    } else {
        heading
    };
    let mut children = vec![heading];
    children.extend(rows.into_iter().map(|(mut row, height)| {
        if blocks {
            row.layout = row
                .layout
                .height(Length::Fixed(height + 2.0 * inset_y))
                .padding(Insets::symmetric(inset_x, inset_y));
        }
        row
    }));
    let card = UiNode::new(
        key,
        UiContent::Group(GroupSpec::new(label).style(StyleRef::Secondary)),
    )
    .layout(
        Layout::column()
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .padding(Insets {
                top: pad,
                right: side - inset_x,
                bottom: pad - inset_y,
                left: side - inset_x,
            })
            .gap(CARD_GAP - 2.0 * inset_y)
            .clipped(),
    )
    .children(children);
    vec![(card, height)]
}

/// The page's four actions under the STATUS card.
fn harness_actions(harness: &HarnessProjection, card_width: f32) -> (UiNode, f32) {
    let control_height = scaled_control_height();
    let need = |label: &str| crate::native_ui::native_button_label_width(label) + 40.0;
    let row = need("Refresh")
        + need("Open aterm.toml")
        + need("AI CLIs")
        + need("Tab Control")
        + 3.0 * CARD_GAP
        <= card_width;
    let subpages_side_by_side =
        !row && need("AI CLIs") + need("Tab Control") + CARD_GAP <= card_width;
    let button = |key: &str, label: &str, busy: bool, style: StyleRef, width: f32| {
        let (visual, lines) = button_words(label, &[], width);
        let action = if key == HARNESS_AI_CLIS {
            route_action(SettingsRoute::Ai)
        } else if key == HARNESS_TAB_CONTROL {
            route_action(SettingsRoute::Tabs)
        } else {
            ActionId::new(key)
        };
        let height = crate::native_ui::native_button_height_for_lines(lines, control_height);
        (
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
                        Length::Fixed(width)
                    } else {
                        Length::Fill
                    })
                    .height(Length::Fixed(height)),
            ),
            height,
        )
    };
    let w = |label: &str| if row { need(label) } else { card_width };
    let (buttons, height) = if subpages_side_by_side {
        let (refresh, refresh_height) = button(
            HARNESS_REFRESH,
            "Refresh",
            harness.collecting,
            StyleRef::Primary,
            card_width,
        );
        let (open, open_height) = button(
            HARNESS_OPEN_CONFIG,
            "Open aterm.toml",
            false,
            StyleRef::Secondary,
            card_width,
        );
        let (mut ai, _) = button(
            HARNESS_AI_CLIS,
            "AI CLIs",
            false,
            StyleRef::Secondary,
            need("AI CLIs"),
        );
        ai.layout.width = Length::Fixed(need("AI CLIs"));
        ai.layout.height = Length::Fixed(control_height);
        let (mut tabs, _) = button(
            HARNESS_TAB_CONTROL,
            "Tab Control",
            false,
            StyleRef::Secondary,
            card_width - need("AI CLIs") - CARD_GAP,
        );
        tabs.layout.height = Length::Fixed(control_height);
        (
            vec![
                refresh,
                open,
                UiNode::new(
                    "harness/actions/subpages",
                    UiContent::Group(GroupSpec::new("Harness sub-pages")),
                )
                .layout(
                    Layout::row()
                        .width(Length::Fill)
                        .height(Length::Fixed(control_height))
                        .gap(CARD_GAP),
                )
                .children(vec![ai, tabs]),
            ],
            refresh_height + open_height + control_height + 2.0 * CARD_GAP,
        )
    } else {
        let buttons = [
            (
                HARNESS_REFRESH,
                "Refresh",
                harness.collecting,
                StyleRef::Primary,
            ),
            (
                HARNESS_OPEN_CONFIG,
                "Open aterm.toml",
                false,
                StyleRef::Secondary,
            ),
            (HARNESS_AI_CLIS, "AI CLIs", false, StyleRef::Secondary),
            (
                HARNESS_TAB_CONTROL,
                "Tab Control",
                false,
                StyleRef::Secondary,
            ),
        ]
        .map(|(key, label, busy, style)| button(key, label, busy, style, w(label)));
        let height = if row {
            buttons
                .iter()
                .map(|(_, height)| *height)
                .fold(control_height, f32::max)
        } else {
            buttons.iter().map(|(_, height)| height).sum::<f32>() + 3.0 * CARD_GAP
        };
        (buttons.into_iter().map(|(node, _)| node).collect(), height)
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

/// The SUPERVISION card's rows ([`page_box_setting_rows`]).
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
    let heading_height = 22.0_f32.max(16.0 * settings_text_scale());
    let row_room = (room - 2.0 * TIGHT_CARD_PAD - heading_height - CARD_GAP).max(0.0);
    let rows: Vec<(&str, Option<prefs::RowNote<'_>>)> = keys.iter().copied().zip(notes).collect();
    page_box_setting_rows(state, &rows, width, inner, None, Some(row_room))
}

/// "No Claude Code, Codex, … session is open": every agent the harness supervises or watches
/// (`Program::ALL`, `harness_table`) by its product name.
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

/// The SESSIONS card's rows ([`every_row`]).
fn session_rows(
    harness: &HarnessProjection,
    width: SettingsWidth,
    inner: f32,
    (timer_open, every_open): (Option<u64>, bool),
    selected: Option<u64>,
) -> Vec<(UiNode, f32)> {
    if harness.sessions.is_empty() {
        let text = if harness.observed {
            no_session_open()
        } else {
            "Reading the sessions\u{2026}".to_string()
        };
        return vec![wrapped_words_node(
            "harness/sessions/empty",
            &text,
            (SemanticRole::Text, StyleRef::Quiet),
            inner,
            22.0_f32.max(18.0 * settings_text_scale()),
        )];
    }
    let offer = harness.land_at_offer;
    let measure = RowMeasure::of(&harness.sessions, width, inner);
    let mut rows: Vec<(UiNode, f32)> = harness
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
                (open, landed),
                inner,
            )
        })
        .collect();
    if let Some(every) = &harness.every {
        rows.push(every_row(every, measure, offer, every_open, inner));
    }
    rows
}

/// AN OPEN TIMER'S CHOICES, ONE PIECE OF ITS ROW ([`scroll::row_parts`]).
fn timer_choices(key: &str, rows: Vec<(UiNode, f32)>) -> (UiNode, f32) {
    let height = rows.iter().map(|(_, height)| height).sum::<f32>()
        + 4.0 * rows.len().saturating_sub(1) as f32;
    (
        UiNode::new(
            format!("{key}/timer-choices"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
        .children(rows.into_iter().map(|(row, _)| row).collect()),
        height,
    )
}

/// WHAT A TIMER'S OPENING REVEALS: its first choice ([`scroll::row_parts`]).
fn timer_opening_reveal(session: Option<u64>) -> UiKey {
    let first = TimerChoice::In(crate::autopilot_menu::LAND_IN_MINUTES[0]);
    UiKey::new(match session {
        Some(local_id) => timer_action(local_id, first),
        None => crate::harness_screen::every_action(EveryPress::Timer(first)),
    })
}

/// The `Every session` row's faces, in its order.
const EVERY_FACES: [&str; 4] = ["Cruise", "Land", "Timer", "Follow the default"];

/// SESSIONS' `EVERY SESSION` ROW ([`EveryRow::line`], [`EveryRow::description`]).
fn every_row(
    every: &EveryRow,
    measure: RowMeasure,
    offer: Option<crate::claude_lights::LandAtGlass>,
    open: bool,
    inner: f32,
) -> (UiNode, f32) {
    use crate::harness_screen::sessions_words;
    let enabled = every.steered > 0;
    let scale = settings_text_scale();
    let title_height = 22.0_f32.max(18.0 * scale);
    let detail_height = 18.0_f32.max(16.0 * scale);
    let control_height = scaled_control_height();
    let key = format!(
        "{}{}",
        crate::harness_screen::SESSION_ACTION_PREFIX,
        crate::harness_screen::EVERY_ID
    );
    let face_width = |face: &str| crate::native_ui::native_button_label_width(face);
    let widest = EVERY_FACES
        .iter()
        .map(|face| face_width(face))
        .fold(0.0_f32, f32::max);
    let wide_follow = 2.0 * measure.button_width + MODE_GAP;
    let stacked = measure.stacked || face_width(EVERY_FACES[3]) + 20.0 > wide_follow;
    const FLOWS: [&[u8]; 3] = [&[4], &[2, 2], &[1, 1, 1, 1]];
    let lines: &'static [u8] = if stacked {
        FLOWS
            .iter()
            .position(|flow| {
                flow.iter()
                    .all(|&count| line_button(inner, count) - 20.0 >= widest)
            })
            .map_or(FLOWS[FLOWS.len() - 1], |want| FLOWS[want])
    } else {
        FLOWS[0]
    };
    let width_of = |index: usize| -> f32 {
        if !stacked {
            return if index == 3 {
                wide_follow
            } else {
                measure.button_width
            };
        }
        let mut start = 0;
        for &count in lines {
            start += usize::from(count);
            if index < start {
                return line_button(inner, count);
            }
        }
        inner
    };
    let presses = [
        Some(EveryPress::Mode(Autopilot::Cruise)),
        Some(EveryPress::Mode(Autopilot::Land)),
        None,
        Some(EveryPress::Follow),
    ];
    let timer = EveryPress::Timer(TimerChoice::In(60));
    let buttons: Vec<UiNode> = EVERY_FACES
        .iter()
        .zip(presses)
        .enumerate()
        .map(|(index, (face, press))| {
            let room = (width_of(index) - 20.0).max(0.0);
            let (action, label, description, expanded) = match press {
                Some(press) => {
                    let moved = sessions_words(every.moved(press));
                    let label = match press {
                        EveryPress::Mode(mode) => {
                            format!("{} every session, {moved}", mode_label(mode))
                        }
                        EveryPress::Follow | EveryPress::Timer(_) => {
                            format!("Follow the default for every session, {moved}")
                        }
                    };
                    let label = format!("{label}{}", every.spoken_tail(press));
                    (every_action(press), label, every.description(press), None)
                }
                None => (
                    every_timer_toggle(),
                    format!(
                        "Timer for every session, {}",
                        sessions_words(every.moved(timer))
                    ),
                    every.description(timer),
                    Some(open),
                ),
            };
            let mut spec =
                ButtonSpec::new(label).visual_label(button_words(face, &[], room + 20.0).0);
            spec.description = Some(description);
            UiNode::new(
                action.clone(),
                UiContent::Button(
                    Control::new(spec, ActionId::new(action))
                        .look_first()
                        .state(ControlState {
                            enabled,
                            expanded,
                            ..ControlState::default()
                        })
                        .style(if press.is_some() {
                            StyleRef::Secondary
                        } else {
                            StyleRef::Quiet
                        }),
                ),
            )
            .layout(
                Layout::default()
                    .width(if stacked {
                        Length::Fill
                    } else {
                        Length::Fixed(width_of(index))
                    })
                    .height(Length::Fill),
            )
        })
        .collect();
    let text_width = if stacked {
        inner
    } else {
        (inner - 5.0 * (measure.button_width + MODE_GAP)).max(0.0)
    };
    let line = every.line();
    let label = format!("Every session, {line}");
    let (row, height) = if stacked {
        let wrapped = |suffix: &str, text: &str, kind: (SemanticRole, StyleRef), height: f32| {
            wrapped_words_node(&format!("{key}/{suffix}"), text, kind, inner, height)
                .0
                .paint_only()
        };
        let mut children = vec![
            wrapped(
                "title",
                "Every session",
                (SemanticRole::Text, StyleRef::Primary),
                title_height,
            ),
            wrapped(
                "detail",
                &line,
                (SemanticRole::Status, StyleRef::Quiet),
                detail_height,
            ),
        ];
        let mut rest = buttons.into_iter();
        let mut start = 0;
        for (n, &count) in lines.iter().enumerate() {
            let count = usize::from(count);
            let on_line: Vec<UiNode> = rest.by_ref().take(count).collect();
            let (suffix, name) = if n == 0 {
                (
                    "modes".to_string(),
                    "Autopilot for every session".to_string(),
                )
            } else {
                let faces = EVERY_FACES.get(start..start + count).unwrap_or_default();
                (format!("line/{n}"), faces.join(", "))
            };
            let line_height = on_line
                .iter()
                .filter_map(|button| match &button.content {
                    UiContent::Button(control) => control.spec.visual_label.as_deref(),
                    _ => None,
                })
                .map(|words| {
                    crate::native_ui::native_button_height_for_lines(
                        words.lines().count().max(1),
                        control_height,
                    )
                })
                .fold(control_height, f32::max);
            children.push(
                UiNode::new(
                    format!("{key}/{suffix}"),
                    UiContent::Group(GroupSpec::new(name)),
                )
                .layout(
                    Layout::row()
                        .height(Length::Fixed(line_height))
                        .gap(MODE_GAP),
                )
                .children(on_line),
            );
            start += count;
        }
        let height = children.iter().map(fixed_height).sum::<f32>()
            + 4.0 * children.len().saturating_sub(1) as f32;
        (
            UiNode::new(key.clone(), UiContent::Group(GroupSpec::new(label)))
                .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
                .children(children),
            height,
        )
    } else {
        let (title, title_lines) = wrapped_words_node(
            &format!("{key}/title"),
            "Every session",
            (SemanticRole::Text, StyleRef::Primary),
            text_width,
            title_height,
        );
        let (detail, detail_lines) = wrapped_words_node(
            &format!("{key}/detail"),
            &line,
            (SemanticRole::Status, StyleRef::Quiet),
            text_width,
            detail_height,
        );
        let height = control_height.max(title_lines + detail_lines);
        let words = UiNode::new(
            format!("{key}/words"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().width(Length::Fill).height(Length::Fill))
        .children(vec![title.paint_only(), detail.paint_only()]);
        let mut children = vec![words];
        children.extend(buttons.into_iter().map(|mut button| {
            button.layout.height = Length::Fixed(control_height);
            button
        }));
        (
            UiNode::new(key.clone(), UiContent::Group(GroupSpec::new(label)))
                .layout(Layout::row().height(Length::Fixed(height)).gap(MODE_GAP))
                .children(children),
            height,
        )
    };
    let mut children = vec![row];
    let mut total = height;
    if open {
        let moved = sessions_words(every.moved(timer));
        let mut choices: Vec<(EveryPress, String, String)> = crate::autopilot_menu::LAND_IN_MINUTES
            .iter()
            .map(|&minutes| {
                let hours = minutes / 60;
                let words = if hours == 1 {
                    "1 hour".to_string()
                } else {
                    format!("{hours} hours")
                };
                (
                    EveryPress::Timer(TimerChoice::In(minutes)),
                    format!("Cruise every session for {words}, then land, {moved}"),
                    words,
                )
            })
            .collect();
        if let Some(at) = offer {
            let when = crate::autopilot_menu::when(at);
            choices.push((
                EveryPress::Timer(TimerChoice::At),
                format!("Cruise every session, then land at {when}, {moved}"),
                format!("at {when}"),
            ));
        }
        let (indent, room) = if stacked {
            (0.0, inner)
        } else {
            let room = (3.0 * measure.button_width + 2.0 * MODE_GAP).min(inner);
            (inner - room, room)
        };
        let own = |face: &str| (face_width(face) + 40.0).min(room);
        let shared = wrapped_width(
            &choices
                .iter()
                .map(|(_, _, face)| own(face))
                .collect::<Vec<_>>(),
            room,
        );
        let one_line = shared.is_none();
        let slot = measure.button_width;
        let on_slots = !one_line
            && !stacked
            && choices
                .iter()
                .all(|(_, _, face)| face_width(face) + 24.0 <= slot);
        let per_line = on_slots.then(|| {
            let fit = (((room + MODE_GAP) / (slot + MODE_GAP)).floor() as usize).max(1);
            choices.len().div_ceil(choices.len().div_ceil(fit))
        });
        let mut lines: Vec<Vec<UiNode>> = Vec::new();
        let mut used = 0.0_f32;
        for (press, label, face) in choices {
            let width = match shared {
                None => own(&face),
                Some(_) if on_slots => slot,
                Some(widest) => widest,
            };
            let action = every_action(press);
            let mut spec = ButtonSpec::new(label).visual_label(
                crate::native_ui::fit_native_button_label(&face, (width - 24.0).max(0.0)),
            );
            spec.description = Some(every.description(press));
            let node = UiNode::new(
                action.clone(),
                UiContent::Button(
                    Control::new(spec, ActionId::new(action))
                        .look_first()
                        .state(ControlState {
                            enabled,
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
            match lines.last_mut() {
                Some(line)
                    if used + MODE_GAP + width <= room
                        && per_line.is_none_or(|per_line| line.len() < per_line) =>
                {
                    used += MODE_GAP + width;
                    line.push(node);
                }
                _ => {
                    used = width;
                    lines.push(vec![node]);
                }
            }
        }
        let rows = lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                (
                    UiNode::new(
                        format!("{key}/timer-row/{i}"),
                        UiContent::Group(GroupSpec::new("Timer for every session")),
                    )
                    .layout(
                        Layout::row()
                            .height(Length::Fixed(control_height))
                            .gap(MODE_GAP)
                            .padding(Insets {
                                left: indent,
                                ..Insets::default()
                            }),
                    )
                    .children(line),
                    control_height,
                )
            })
            .collect();
        let (choices, height) = timer_choices(&key, rows);
        children.push(choices);
        total += height + 4.0;
    }
    (
        UiNode::new(
            format!("{key}/block"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().height(Length::Fixed(total)).gap(4.0))
        .children(children),
        total,
    )
}

/// THE TIMER'S CHOICES WHERE THEY WRAP ([`MODE_GAP`]).
fn wrapped_width(own: &[f32], room: f32) -> Option<f32> {
    let line = own.iter().sum::<f32>() + own.len().saturating_sub(1) as f32 * MODE_GAP;
    (line > room).then(|| own.iter().copied().fold(0.0_f32, f32::max))
}

/// A SESSIONS row under its STATE LINE and its open TIMER ([`session_row`]).
fn session_timer(
    row: &HarnessSessionRow,
    (node, height): (UiNode, f32),
    enabled: bool,
    offer: Option<crate::claude_lights::LandAtGlass>,
    (open, landed): (bool, bool),
    inner: f32,
) -> (UiNode, f32) {
    let scale = settings_text_scale();
    let line_height = 18.0_f32.max(16.0 * scale);
    let control_height = scaled_control_height();
    let key = format!("harness/session/{}", row.local_id);
    let mut children = vec![node];
    let mut total = height;
    if let Some(line) = row.state_line() {
        let (state, height) = wrapped_words_node(
            &format!("{key}/state"),
            &line,
            (SemanticRole::Status, StyleRef::Quiet),
            inner,
            line_height,
        );
        children.push(state);
        total += height + 4.0;
    }
    if let Some(day) = &row.day_line {
        let (shown, height) = wrapped_words_node(
            &format!("{key}/today/text"),
            day,
            (SemanticRole::Status, StyleRef::Quiet),
            inner,
            line_height,
        );
        children.push(
            UiNode::new(
                format!("{key}/today"),
                UiContent::Group(GroupSpec::new(day.clone())),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            )
            .children(vec![shown.paint_only()]),
        );
        total += height + 4.0;
    }
    let own =
        |visual: &str| (crate::native_ui::native_button_label_width(visual) + 40.0).min(inner);
    let mut faces: Vec<String> = Vec::new();
    if open {
        for minutes in crate::autopilot_menu::LAND_IN_MINUTES {
            let hours = minutes / 60;
            faces.push(if hours == 1 {
                "1 hour".to_string()
            } else {
                format!("{hours} hours")
            });
        }
        if let Some(at) = offer {
            faces.push(format!("at {}", crate::autopilot_menu::when(at)));
        }
        if row.armed_at_ms.is_some() {
            faces.push("Cancel".to_string());
        }
    }
    let shared = wrapped_width(
        &faces.iter().map(|face| own(face)).collect::<Vec<_>>(),
        inner,
    );
    let button =
        |action: String, label: String, visual: &str, enabled: bool, expanded: Option<bool>| {
            let width = shared.unwrap_or_else(|| own(visual));
            let node = UiNode::new(
                action.clone(),
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new(label).visual_label(button_words(visual, &[], width).0),
                        ActionId::new(action),
                    )
                    .look_first()
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
                &words,
                enabled,
                None,
            ));
        }
        if let Some(at) = offer {
            let (mut node, width) = button(
                timer_action(row.local_id, TimerChoice::At),
                format!("Cruise, then land at {}", crate::autopilot_menu::when(at)),
                &format!("at {}", crate::autopilot_menu::when(at)),
                enabled,
                None,
            );
            if let UiContent::Button(control) = &mut node.content {
                control.spec.description = Some(format!(
                    "Cruise until {}, then land",
                    crate::autopilot_menu::spoken_when(at)
                ));
            }
            buttons.push((node, width));
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
    let mut rows = Vec::new();
    for (i, line) in lines.into_iter().enumerate() {
        let height = line
            .iter()
            .filter_map(|button| match &button.content {
                UiContent::Button(control) => control.spec.visual_label.as_deref(),
                _ => None,
            })
            .map(|words| {
                crate::native_ui::native_button_height_for_lines(
                    words.lines().count().max(1),
                    control_height,
                )
            })
            .fold(control_height, f32::max);
        rows.push((
            UiNode::new(
                format!("{key}/timer-row/{i}"),
                UiContent::Group(GroupSpec::new("Timer")),
            )
            .layout(Layout::row().height(Length::Fixed(height)).gap(MODE_GAP))
            .children(line),
            height,
        ));
    }
    if !rows.is_empty() {
        let (choices, height) = timer_choices(&key, rows);
        children.push(choices);
        total += height + 4.0;
    }
    let block = GroupSpec::unlabeled(SemanticRole::Group);
    (
        UiNode::new(
            format!("{key}/block"),
            UiContent::Group(if landed {
                block.style(StyleRef::Primary)
            } else {
                block
            }),
        )
        .layout(Layout::column().height(Length::Fixed(total)).gap(4.0))
        .children(children),
        total,
    )
}

/// DECIDED FOR YOU'S ROWS ([`DECIDED_LINES`]).
fn decided_rows(harness: &HarnessProjection, inner: f32, open: bool) -> Vec<(UiNode, f32)> {
    let shown = if open {
        harness.decided.len()
    } else {
        harness.decided.len().min(DECIDED_SHOWN)
    };
    let rows = &harness.decided[..shown];
    let (clock_px, clock_face) =
        crate::native_ui::text_paint_metrics(SemanticRole::Status, StyleRef::Quiet);
    let clock_width = rows
        .iter()
        .map(|row| crate::tray_raster::ui_text_width_for(clock_face, &row.clock, clock_px))
        .fold(0.0_f32, f32::max)
        + ABOUT_METADATA_TEXT_SAFETY;
    let button_width = (crate::native_ui::native_button_label_width("Show tab") + 40.0).min(inner);
    let any_show = rows.iter().any(|row| row.show.is_some());
    let words = inner - clock_width - MODE_GAP;
    let beside = words
        - if any_show {
            button_width + MODE_GAP
        } else {
            0.0
        };
    let stacked = beside < 240.0;
    let measure = DecidedMeasure {
        clock_width,
        words_width: if stacked { words } else { beside }.max(0.0),
        button_width,
        stacked,
    };
    let mut out: Vec<(UiNode, f32)> = rows.iter().map(|row| decided_row(row, measure)).collect();
    if harness.decided.len() > DECIDED_SHOWN {
        out.push(decided_disclosure(
            open,
            (harness.decided.len(), harness.decided_total),
            inner,
        ));
    }
    out
}

/// How DECIDED FOR YOU's rows are laid out at the page's measure ([`decided_rows`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct DecidedMeasure {
    /// The clock column's width.
    clock_width: f32,
    /// The sentence's column.
    words_width: f32,
    /// `Show tab`'s width.
    button_width: f32,
    /// `Show tab` goes under the words.
    stacked: bool,
}

/// One choice's row.
fn decided_row(row: &crate::harness_screen::DecidedRow, measure: DecidedMeasure) -> (UiNode, f32) {
    let scale = settings_text_scale();
    let line_height = 20.0_f32.max(18.0 * scale);
    let control_height = scaled_control_height();
    let key = format!("harness/decided/{}", row.id);
    let (px, _) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary);
    let room = (measure.words_width - ABOUT_METADATA_TEXT_SAFETY).max(1.0);
    let mut lines = wrap_native_lines(std::slice::from_ref(&row.sentence), room, px);
    if lines.len() > DECIDED_LINES {
        lines.truncate(DECIDED_LINES);
        if let Some(last) = lines.last_mut() {
            *last = crate::native_ui::fit_native_text_label(&format!("{last}\u{2026}"), room);
        }
    }
    let count = lines.len().max(1);
    let words_height = count as f32 * line_height + (count - 1) as f32 * 2.0;
    let text = |key: String, text: String, role: SemanticRole, style: StyleRef, width: Length| {
        UiNode::new(key, UiContent::Text(TextSpec { text, role, style }))
            .layout(
                Layout::default()
                    .width(width)
                    .height(Length::Fixed(line_height)),
            )
            .paint_only()
    };
    let clock = text(
        format!("{key}/clock"),
        row.clock.clone(),
        SemanticRole::Status,
        StyleRef::Quiet,
        Length::Fixed(measure.clock_width),
    );
    let words = UiNode::new(
        format!("{key}/words"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        Layout::column()
            .width(Length::Fixed(measure.words_width))
            .height(Length::Fixed(words_height))
            .gap(2.0),
    )
    .children(
        lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                text(
                    format!("{key}/line/{i}"),
                    line,
                    SemanticRole::Text,
                    StyleRef::Primary,
                    Length::Fill,
                )
            })
            .collect(),
    );
    let show = row.show.map(|local_id| {
        let action = show_action(local_id);
        let mut spec = ButtonSpec::new(format!("Show tab of {}", row.name)).visual_label(
            crate::native_ui::fit_native_button_label(
                "Show tab",
                (measure.button_width - 24.0).max(0.0),
            ),
        );
        spec.description = Some("raise its tab and pane".to_string());
        UiNode::new(
            format!("{key}/show"),
            UiContent::Button(Control::new(spec, ActionId::new(action)).style(StyleRef::Quiet)),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(measure.button_width))
                .height(Length::Fixed(control_height)),
        )
    });
    let label = format!("{} {}", row.clock, row.sentence);
    let when = format!("{key}/when");
    let line = |children: Vec<UiNode>, height: f32| {
        UiNode::new(
            when,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::row().height(Length::Fixed(height)).gap(MODE_GAP))
        .children(children)
    };
    let (node, height) = match show {
        Some(show) if measure.stacked => {
            let height = words_height + 4.0 + control_height;
            (
                UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
                    .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
                    .children(vec![line(vec![clock, words], words_height), show]),
                height,
            )
        }
        Some(show) => {
            let height = words_height.max(control_height);
            (
                UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
                    .layout(Layout::row().height(Length::Fixed(height)).gap(MODE_GAP))
                    .children(vec![clock, words, show]),
                height,
            )
        }
        None => (
            UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
                .layout(
                    Layout::row()
                        .height(Length::Fixed(words_height))
                        .gap(MODE_GAP),
                )
                .children(vec![clock, words]),
            words_height,
        ),
    };
    (node, height)
}

/// DECIDED FOR YOU's disclosure ([`aterm_agent::harness::stats::DECIDED_KEEP`]).
fn decided_disclosure(open: bool, (kept, total): (usize, u64), inner: f32) -> (UiNode, f32) {
    let height = scaled_control_height();
    let (label, visual) = if open {
        (
            format!("Show the newest {DECIDED_SHOWN} choices made for you, only"),
            format!("Show the newest {DECIDED_SHOWN}"),
        )
    } else if total > kept as u64 {
        (
            format!("Show the newest {kept} of {total} choices made for you today"),
            format!("Show the newest {kept} of {total}"),
        )
    } else {
        (
            format!("Show all {kept} choices made for you today"),
            format!("Show all {kept}"),
        )
    };
    let width = (crate::native_ui::native_button_label_width(&visual) + 56.0).min(inner);
    (
        UiNode::new(
            HARNESS_DECIDED_ALL,
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(label).visual_label(crate::native_ui::fit_native_button_label(
                        &visual,
                        (width - 48.0).max(0.0),
                    )),
                    ActionId::new(HARNESS_DECIDED_ALL),
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

/// Why a session's Autopilot press writes nothing: the harness steers no mode for it.
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
    crate::autopilot::mode_words(mode).button
}

/// The five faces of a SESSIONS row's buttons, in their order.
const ROW_FACES: [&str; 5] = ["Cruise", "Land", "Off", "Timer", "Show"];

/// HOW THE SESSIONS ROWS ARE LAID OUT at the page's measure (§10.3.4, "A narrow page").
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
    fn of(rows: &[HarnessSessionRow], width: SettingsWidth, inner: f32) -> Self {
        let scale = settings_text_scale();
        let face = ROW_FACES
            .iter()
            .map(|face| crate::native_ui::native_button_label_width(face))
            .fold(0.0_f32, f32::max);
        let button_width = (76.0 * scale.min(1.25)).max(64.0).max(face + 20.0);
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
        let lines: &'static [u8] = if stacked {
            FLOWS
                .iter()
                .position(|&flow| holds(flow))
                .map_or(FLOWS[FLOWS.len() - 1], |want| FLOWS[want])
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

/// The height of a SESSIONS row's setting line ([`setting_line`]).
fn setting_line_height() -> f32 {
    18.0_f32.max(16.0 * settings_text_scale())
}

/// A SESSIONS ROW'S SETTING LINE ([`HarnessSessionRow::setting`], [`wrapped_words_node`]).
fn setting_line(row: &HarnessSessionRow, inner: f32) -> (UiNode, f32) {
    let key = format!("harness/session/{}/setting", row.local_id);
    let (text, height) = wrapped_words_node(
        &format!("{key}/text"),
        &row.setting,
        (SemanticRole::Status, StyleRef::Quiet),
        inner,
        setting_line_height(),
    );
    (
        UiNode::new(key, UiContent::Group(GroupSpec::new(row.setting.clone())))
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            )
            .children(vec![text.paint_only()]),
        height,
    )
}

/// The width each of `count` buttons sharing a line of `inner` has.
fn line_button(inner: f32, count: u8) -> f32 {
    let count = f32::from(count.max(1));
    (inner - (count - 1.0) * MODE_GAP) / count
}

/// One session's row ([`session_timer`]).
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
    let face_words =
        |word: &str, index: usize| button_words(word, &[], measure.button(inner, index)).0;
    let key = format!("harness/session/{}", row.local_id);
    let mut buttons: Vec<UiNode> = Autopilot::ALL
        .iter()
        .enumerate()
        .map(|(index, mode)| {
            let action = autopilot_action(row.local_id, *mode);
            let word = mode_label(*mode);
            let mut spec = ButtonSpec::new(format!("Autopilot {} for {}", mode.word(), row.name))
                .visual_label(face_words(word, index));
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
                        .look_first()
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
                    ButtonSpec::new(format!("Timer for {}", row.name))
                        .visual_label(face_words("Timer", 3)),
                    ActionId::new(toggle),
                )
                .look_first()
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
    let show = show_action(row.local_id);
    let mut show_spec =
        ButtonSpec::new(format!("Show {}", row.name)).visual_label(face_words("Show", 4));
    show_spec.description = Some("raise its tab and pane".to_string());
    buttons.push(
        UiNode::new(
            show.clone(),
            UiContent::Button(
                Control::new(show_spec, ActionId::new(show))
                    .look_first()
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
    let label = format!("{}: {}", row.title, row.detail);
    if compact {
        let line = |suffix: &str, name: &str, buttons: Vec<UiNode>| {
            let height = buttons
                .iter()
                .filter_map(|button| match &button.content {
                    UiContent::Button(control) => control.spec.visual_label.as_deref(),
                    _ => None,
                })
                .map(|words| {
                    crate::native_ui::native_button_height_for_lines(
                        words.lines().count().max(1),
                        control_height,
                    )
                })
                .fold(control_height, f32::max);
            UiNode::new(
                format!("{key}/{suffix}"),
                UiContent::Group(GroupSpec::new(name)),
            )
            .layout(Layout::row().height(Length::Fixed(height)).gap(MODE_GAP))
            .children(buttons)
        };
        let wrapped = |suffix: &str, text: &str, kind: (SemanticRole, StyleRef), height: f32| {
            wrapped_words_node(&format!("{key}/{suffix}"), text, kind, inner, height)
                .0
                .paint_only()
        };
        let title = wrapped(
            "title",
            &row.title,
            (SemanticRole::Text, StyleRef::Primary),
            title_height,
        );
        let detail = wrapped(
            "detail",
            &row.detail,
            (SemanticRole::Status, StyleRef::Quiet),
            detail_height,
        );
        let (setting, _) = setting_line(row, inner);
        let mut children = vec![title, detail, setting];
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
        let height = children.iter().map(fixed_height).sum::<f32>()
            + 4.0 * children.len().saturating_sub(1) as f32;
        return (
            UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
                .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
                .children(children),
            height,
        );
    }
    let (title, words_title) = wrapped_words_node(
        &format!("{key}/title"),
        &row.title,
        (SemanticRole::Text, StyleRef::Primary),
        text_width,
        title_height,
    );
    let (detail, words_detail) = wrapped_words_node(
        &format!("{key}/detail"),
        &row.detail,
        (SemanticRole::Status, StyleRef::Quiet),
        text_width,
        detail_height,
    );
    let height = control_height.max(words_title + words_detail);
    let words = UiNode::new(
        format!("{key}/words"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::column().width(Length::Fill).height(Length::Fill))
    .children(vec![title.paint_only(), detail.paint_only()]);
    let mut children = vec![words];
    children.extend(buttons.into_iter().map(|mut button| {
        button.layout.height = Length::Fixed(control_height);
        button
    }));
    let line = UiNode::new(key.clone(), UiContent::Group(GroupSpec::new(label)))
        .layout(Layout::row().height(Length::Fixed(height)).gap(MODE_GAP))
        .children(children);
    let (setting, setting_height) = setting_line(row, inner);
    let total = height + setting_height;
    (
        UiNode::new(
            format!("{key}/head"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().height(Length::Fixed(total)))
        .children(vec![line, setting]),
        total,
    )
}
