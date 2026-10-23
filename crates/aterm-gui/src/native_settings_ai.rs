// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Settings `Harness ▸ AI CLIs` sub-page layout and actions.

use super::harness_page::{CARD_GAP, metadata_cards, row_cards};
use super::*;
use crate::ai_screen::{AiAct, AiCliRow, AiProjection};
use crate::native_app::HarnessRequest;

/// Prefix for all action identifiers minted by the AI CLIs page.
pub(crate) const AI_ACTION_PREFIX: &str = "ai/";

/// Registry key prefix for `[ai]` settings rows.
pub(super) const AI_KEY_PREFIX: &str = "ai.";

/// Action identifier for the Refresh button.
pub(super) const AI_REFRESH: &str = "ai/refresh";

/// Action identifier for opening `aterm.toml` at `[ai] reviewers`.
pub(super) const AI_OPEN_REVIEWS: &str = "ai/open-config/reviewers";

/// Action identifier for opening `aterm.toml` at `[ai] fleet`.
pub(super) const AI_OPEN_FLEET: &str = "ai/open-config/fleet";

/// Action identifier for fetching the fleet document immediately.
pub(super) const AI_FLEET_FETCH: &str = "ai/fleet-fetch";

/// A CLI's Sign in (or Install): `ai/sign-in/<id>`.
const AI_SIGN_IN_PREFIX: &str = "ai/sign-in/";

/// The gap between a one-line row's words and its controls, a setting row's
/// own, so a status row's button stands in the switches' column.
const ROW_GAP: f32 = 12.0;

/// Iterates over all `[ai]` registry setting rows in declaration order.
pub(super) fn ai_page_rows() -> impl Iterator<Item = &'static str> {
    prefs::NESTED_LEAVES
        .iter()
        .map(|leaf| leaf.key)
        .filter(|key| key.starts_with(AI_KEY_PREFIX))
}

/// The THIS MACHINE card's rows.
const MACHINE_ROWS: [&str; 2] = [prefs::EDIT_AI_PREFERRED, prefs::EDIT_AI_ENABLED];

/// The REVIEWS card's rows.
const REVIEW_ROWS: [&str; 2] = [prefs::EDIT_AI_REVIEWS, prefs::EDIT_AI_REVIEW_PERMISSIONS];

/// The FLEET DOCUMENT card's rows.
const FLEET_ROWS: [&str; 3] = [
    prefs::EDIT_AI_MACHINE,
    prefs::EDIT_AI_FLEET,
    prefs::EDIT_AI_FLEET_KEY,
];

/// The words a row adds where its value came from the fleet document.
const FROM_FLEET: &str = "From the fleet document.";

/// Formats the Sign-in / Install action identifier for CLI `id`.
#[must_use]
pub(super) fn sign_in_action(id: &str) -> String {
    format!("{AI_SIGN_IN_PREFIX}{id}")
}

/// Returns the subtitle for the AI CLIs page at `width`.
pub(super) fn ai_page_subtitle(width: SettingsWidth) -> &'static str {
    if width == SettingsWidth::Compact {
        "Logins, the preferred CLI, reviews."
    } else {
        "Each AI CLI's login, the preferred one, and reviews."
    }
}

/// Builds command-palette entries for the AI CLIs page.
pub(super) fn ai_commands(ai: &crate::ai_screen::AiState) -> Vec<Command> {
    vec![
        Command {
            id: ActionId::new(AI_REFRESH),
            title: "AI CLIs: Refresh".to_string(),
            shortcut: None,
            enabled: ai.enabled && !ai.looking,
        },
        Command {
            id: ActionId::new(AI_OPEN_REVIEWS),
            title: "AI CLIs: Open aterm.toml".to_string(),
            shortcut: None,
            enabled: true,
        },
        Command {
            id: ActionId::new(AI_FLEET_FETCH),
            title: "AI CLIs: Fetch the fleet document".to_string(),
            shortcut: None,
            enabled: ai.enabled
                && !ai.looking
                && ai
                    .choices
                    .fleet_source
                    .as_deref()
                    .is_some_and(aterm_agent::ai::fleet::is_url),
        },
    ]
}

impl SettingsApp {
    /// Handles an `ai/*` action press on the AI CLIs page.
    pub(super) fn reduce_ai_action(
        &mut self,
        view: &mut SettingsViewState,
        action: &str,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        if action == AI_REFRESH {
            cx.harness(HarnessRequest::AiLook);
        } else if action == AI_FLEET_FETCH {
            cx.harness(HarnessRequest::AiFleetFetch);
            view.feedback = Some("Fetching the fleet document\u{2026}".to_string());
        } else if action == AI_OPEN_REVIEWS || action == AI_OPEN_FLEET {
            if view
                .pending
                .values()
                .any(|pending| matches!(pending, PendingAction::ConfigEditor))
            {
                view.feedback = Some("aterm.toml is already opening\u{2026}".to_string());
            } else {
                let key = if action == AI_OPEN_REVIEWS {
                    "ai.reviewers"
                } else {
                    "ai.fleet"
                };
                let operation =
                    cx.open_config_editor_at(Some(ConfigEditorTarget::Key(key.to_string())));
                view.pending.insert(operation, PendingAction::ConfigEditor);
                view.feedback = Some("Opening aterm.toml\u{2026}".to_string());
            }
        } else if let Some(id) = action.strip_prefix(AI_SIGN_IN_PREFIX) {
            let cli = self.harness.ai.actionable_cli(id);
            match cli.and_then(|cli| cli.act().map(|act| (cli, act))) {
                Some((cli, act)) => {
                    cx.harness(HarnessRequest::AiSignIn { id: cli.id });
                    view.feedback = Some(match act {
                        AiAct::SignIn => {
                            format!("Opening a tab to sign in to {}\u{2026}", cli.product)
                        }
                        AiAct::Install => {
                            format!("Opening a tab to install {}\u{2026}", cli.product)
                        }
                    });
                }
                None => {
                    view.feedback = Some("That AI CLI does not need you now.".to_string());
                }
            }
        } else {
            return EventResult::Bubble;
        }
        cx.repaint(crate::native_app::DamageRegion::All);
        EventResult::Handled
    }
}

/// Builds the AI CLIs settings page UI node and layout metrics.
pub(super) fn ai_page(
    state: &SettingsViewState,
    ai: &AiProjection,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    let viewport = cx.viewport;
    if state
        .choice_picker
        .as_ref()
        .is_some_and(|picker| ai_page_rows().any(|key| key == picker.key))
    {
        return ai_choice_page(state, width, cx);
    }
    let gap = settings_page_gap(state, width, viewport);
    let maximum = PageLayout::Form.maximum(width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (heading, heading_height) = pinned_heading(
        state,
        width,
        viewport,
        gap,
        SettingsRoute::Ai.label(),
        ai_page_subtitle(width),
    );
    let room = (settings_list_body_height(state, width, viewport) - heading_height)
        .max(scaled_control_height());
    let cards = ai_cards(state, ai, width, page_width, room);
    let mut out = heading;
    out.push(scroll::list_page_peeking(
        state,
        "settings/list",
        "AI CLIs",
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
fn ai_choice_page(state: &SettingsViewState, width: SettingsWidth, cx: &ViewCx<'_>) -> Vec<UiNode> {
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
    let (mut out, _) = pinned_heading(
        state,
        width,
        cx.viewport,
        gap,
        SettingsRoute::Ai.label(),
        "",
    );
    let picker = choice_picker_node(state, width, cx.viewport, &out);
    out.extend(picker);
    out
}

/// Every card of the page, in order, each no taller than `room` where its
/// rows allow (a single row taller than the room is a card of its own).
fn ai_cards(
    state: &SettingsViewState,
    ai: &AiProjection,
    width: SettingsWidth,
    card_width: f32,
    room: f32,
) -> Vec<(UiNode, f32)> {
    let inner = card_text_width(card_width);
    let label_width = |rows: &[(String, String)]| fact_label_width(rows, inner);
    let heading_height = 22.0_f32.max(16.0 * settings_text_scale());
    let row_room =
        (room - 2.0 * super::harness_page::TIGHT_CARD_PAD - heading_height - CARD_GAP).max(0.0);
    let mut cards = Vec::new();
    cards.extend(row_cards(
        "ai/machine",
        ("This machine", ("THIS MACHINE", "MACHINE")),
        machine_rows(state, ai, width, inner, row_room),
        room,
        card_width,
    ));
    cards.push(ai_actions(ai, card_width));
    cards.extend(row_cards(
        "ai/clis",
        ("AI CLIs", ("AI CLIS", "CLIS")),
        cli_rows(state, ai, width, inner, row_room),
        room,
        card_width,
    ));
    cards.extend(row_cards(
        "ai/review-rows",
        ("Reviews", ("REVIEWS", "REVIEWS")),
        setting_rows(state, &REVIEW_ROWS, width, inner, row_room),
        room,
        card_width,
    ));
    cards.extend(metadata_cards(
        "ai/reviews",
        ("WHO REVIEWS", "WHO"),
        &ai.review_rows,
        card_width,
        label_width(&ai.review_rows),
    ));
    cards.push(open_config_button(
        AI_OPEN_REVIEWS,
        "Open aterm.toml at [ai] reviewers",
        card_width,
    ));
    cards.extend(row_cards(
        "ai/fleet-rows",
        ("Fleet document", ("FLEET DOCUMENT", "FLEET")),
        setting_rows(state, &FLEET_ROWS, width, inner, row_room),
        room,
        card_width,
    ));
    cards.extend(metadata_cards(
        "ai/fleet",
        ("WHAT IS HELD", "HELD"),
        &ai.fleet_rows,
        card_width,
        label_width(&ai.fleet_rows),
    ));
    cards.push(fleet_actions(ai, card_width));
    cards
}

/// The THIS MACHINE card's rows, each the way every page draws a setting
/// ([`page_box_setting_rows`]): its note, and the fleet's word where the
/// preferred CLI came from the fleet document.
fn machine_rows(
    state: &SettingsViewState,
    ai: &AiProjection,
    width: SettingsWidth,
    inner: f32,
    row_room: f32,
) -> Vec<(UiNode, f32)> {
    let notes: Vec<Option<String>> = MACHINE_ROWS
        .iter()
        .map(|key| {
            let note = prefs::footnote_of(key).map(str::to_string);
            if *key == prefs::EDIT_AI_PREFERRED && ai.preferred_from_fleet {
                Some(match note {
                    Some(note) => format!("{FROM_FLEET} {note}"),
                    None => FROM_FLEET.to_string(),
                })
            } else {
                note
            }
        })
        .collect();
    let rows: Vec<(&str, Option<prefs::RowNote<'_>>)> = MACHINE_ROWS
        .iter()
        .copied()
        .zip(
            notes
                .iter()
                .map(|note| note.as_deref().map(prefs::RowNote::Own)),
        )
        .collect();
    page_box_setting_rows(state, &rows, width, inner, None, Some(row_room))
}

/// A card's setting rows, each the way every page draws a setting, with its
/// note ([`prefs::row_notes`]), as one box ([`page_box_setting_rows`]).
fn setting_rows(
    state: &SettingsViewState,
    keys: &[&str],
    width: SettingsWidth,
    inner: f32,
    row_room: f32,
) -> Vec<(UiNode, f32)> {
    let rows: Vec<(&str, Option<prefs::RowNote<'_>>)> =
        keys.iter().copied().zip(prefs::row_notes(keys)).collect();
    page_box_setting_rows(state, &rows, width, inner, None, Some(row_room))
}

/// The AI CLIS card's rows: who needs you, then for each CLI its status row
/// and its "Use on this machine" switch.
fn cli_rows(
    state: &SettingsViewState,
    ai: &AiProjection,
    width: SettingsWidth,
    inner: f32,
    row_room: f32,
) -> Vec<(UiNode, f32)> {
    let mut rows = vec![quiet_line("ai/clis/health", &ai.health, inner)];
    let last = ai.clis.len().saturating_sub(1);
    let keys: Vec<String> = ai
        .clis
        .iter()
        .map(|row| format!("ai.{}.use", row.id))
        .collect();
    let own: Vec<Option<String>> = ai
        .clis
        .iter()
        .map(|row| {
            let words: Vec<&str> = [
                row.use_from_fleet.then_some(FROM_FLEET),
                (last == 0).then_some(prefs::AI_USE_NOTE),
            ]
            .into_iter()
            .flatten()
            .collect();
            (!words.is_empty()).then(|| words.join(" "))
        })
        .collect();
    let switches: Vec<(&str, Option<prefs::RowNote<'_>>)> = keys
        .iter()
        .map(String::as_str)
        .zip(
            own.iter()
                .map(|note| note.as_deref().map(prefs::RowNote::Own)),
        )
        .collect();
    let mut placed = page_finished_rows(state, &switches, width, inner, None);
    place_box_facts(&mut placed);
    if last > 0
        && let Some(closing) = placed.last_mut()
    {
        closing.shared_note = Some(prefs::AI_USE_NOTE.to_string());
    }
    let mut placed = placed.into_iter().peekable();
    for (row, key) in ai.clis.iter().zip(&keys) {
        rows.push(cli_status_row(row, width, inner, row_room));
        if placed
            .peek()
            .is_some_and(|switch| switch.key == key.as_str())
        {
            let switch = placed.next().expect("peeked");
            rows.extend(finished_row_stops(&switch, Some(row_room)));
        }
    }
    rows
}

/// A quiet line of text, wrapped to `width` where one line cannot hold it,
/// never cut ([`wrapped_words_node`]): "Claude Code, Codex CLI, Antigra…"
/// was all a narrow page said of who needs you.
fn quiet_line(key: &str, text: &str, width: f32) -> (UiNode, f32) {
    wrapped_words_node(
        key,
        text,
        (SemanticRole::Status, StyleRef::Quiet),
        width,
        22.0_f32.max(18.0 * settings_text_scale()),
    )
}

/// One CLI's status row: its name and version over its state and fact
/// (each wrapped whole to the text column; the row's group carries both), and its
/// Sign in or Install where it needs a person.
fn cli_status_row(
    row: &AiCliRow,
    width: SettingsWidth,
    inner: f32,
    row_room: f32,
) -> (UiNode, f32) {
    let stacked = width == SettingsWidth::Compact;
    let laid = cli_status_row_as(row, width, inner, stacked);
    if !stacked && laid.1 > row_room + 0.01 {
        return cli_status_row_as(row, width, inner, true);
    }
    laid
}

/// [`cli_status_row`] on one line beside its button, or stacked
/// (`compact`): its words over its button, each the column's width.
fn cli_status_row_as(
    row: &AiCliRow,
    width: SettingsWidth,
    inner: f32,
    compact: bool,
) -> (UiNode, f32) {
    let scale = settings_text_scale();
    let title_height = 22.0_f32.max(18.0 * scale);
    let detail_height = 18.0_f32.max(16.0 * scale);
    let control_height = scaled_control_height();
    let key = format!("ai/cli/{}", row.id);
    let button = row.act.map(|act| {
        let label = act.label();
        let button_width = [AiAct::SignIn, AiAct::Install]
            .iter()
            .map(|act| crate::native_ui::native_button_label_width(act.label()) + 40.0)
            .fold(0.0_f32, f32::max)
            .min(inner);
        let action = sign_in_action(row.id);
        let mut spec = ButtonSpec::new(format!("{label}: {}", row.product)).visual_label(
            crate::native_ui::fit_native_button_label(label, (button_width - 20.0).max(0.0)),
        );
        spec.description = Some(match act {
            AiAct::SignIn => format!(
                "opens a new tab that runs aterm ai login {}, its vendor's own sign-in",
                row.id
            ),
            AiAct::Install => {
                format!("opens a new tab that runs aterm pkg install {}", row.id)
            }
        });
        (
            UiNode::new(
                action.clone(),
                UiContent::Button(
                    Control::new(spec, ActionId::new(action)).style(StyleRef::Primary),
                ),
            )
            .layout(
                Layout::default()
                    .width(if compact {
                        Length::Fill
                    } else {
                        Length::Fixed(button_width)
                    })
                    .height(Length::Fixed(if compact {
                        control_height
                    } else {
                        width.row_height() - 8.0
                    })),
            ),
            button_width,
        )
    });
    let text_width = match (&button, compact) {
        (Some((_, button_width)), false) => {
            (inner - button_width - reset_slot_width() - 2.0 * ROW_GAP).max(0.0)
        }
        _ => inner,
    };
    let (title, title_height) = wrapped_words_node(
        &format!("{key}/title"),
        &row.title,
        (SemanticRole::Text, StyleRef::Primary),
        text_width,
        title_height,
    );
    let title = title.paint_only();
    let (detail, detail_height) = wrapped_words_node(
        &format!("{key}/detail"),
        &row.detail,
        (SemanticRole::Status, StyleRef::Quiet),
        text_width,
        detail_height,
    );
    let detail = detail.paint_only();
    let label = format!("{}: {}", row.title, row.detail);
    if compact {
        let mut children = vec![title, detail];
        let mut height = title_height + detail_height + 4.0;
        if let Some((button, _)) = button {
            children.push(button);
            height += control_height + 4.0;
        }
        return (
            UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
                .layout(Layout::column().height(Length::Fixed(height)).gap(4.0))
                .children(children),
            height,
        );
    }
    let height = width.row_height().max(title_height + detail_height + 8.0);
    let words = UiNode::new(
        format!("{key}/words"),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(Layout::column().width(Length::Fill).height(Length::Fill))
    .children(vec![title, detail]);
    let mut children = vec![words];
    if let Some((button, _)) = button {
        children.push(button);
        children.push(
            UiNode::new(
                format!("{key}/slot"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(reset_slot_width()))
                    .height(Length::Fill),
            )
            .paint_only(),
        );
    }
    (
        UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
            .layout(
                Layout::row()
                    .height(Length::Fixed(height))
                    .padding(Insets::symmetric(0.0, 4.0))
                    .gap(ROW_GAP),
            )
            .children(children),
        height,
    )
}

/// The page's action under the THIS MACHINE card: Refresh, busy while a look
/// runs and inert while the look is switched off.
fn ai_actions(ai: &AiProjection, card_width: f32) -> (UiNode, f32) {
    let control_height = scaled_control_height();
    let label = "Refresh";
    let width = (crate::native_ui::native_button_label_width(label) + 40.0).min(card_width);
    let button = UiNode::new(
        AI_REFRESH,
        UiContent::Button(
            Control::new(
                ButtonSpec::new("Refresh the AI CLIs").visual_label(label),
                ActionId::new(AI_REFRESH),
            )
            .state(ControlState {
                busy: ai.looking,
                enabled: ai.enabled,
                ..ControlState::default()
            })
            .style(StyleRef::Primary),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(width))
            .height(Length::Fixed(control_height)),
    );
    (
        UiNode::new(
            "ai/actions",
            UiContent::Group(GroupSpec::new("AI CLIs actions")),
        )
        .layout(
            Layout::row()
                .height(Length::Fixed(control_height))
                .gap(CARD_GAP),
        )
        .children(vec![button]),
        control_height,
    )
}

/// The FLEET DOCUMENT card's actions: Fetch now (busy while a look runs,
/// inert while the look is off or `[ai] fleet` names no URL) and Open
/// aterm.toml at `[ai] fleet` — side by side where the card holds both, else
/// one over the other (they ran past a phone-width card at 2× text).
fn fleet_actions(ai: &AiProjection, card_width: f32) -> (UiNode, f32) {
    let control_height = scaled_control_height();
    let label = "Fetch now";
    let need = crate::native_ui::native_button_label_width(label) + 40.0;
    let (open, open_height) =
        open_config_button(AI_OPEN_FLEET, "Open aterm.toml at [ai] fleet", card_width);
    let open_width = match open.layout.width {
        Length::Fixed(width) => width,
        _ => card_width,
    };
    let beside = need + CARD_GAP + open_width <= card_width && open_height <= control_height;
    let width = need.min(card_width);
    let (visual, lines) = button_words(label, &[], width);
    let fetch_height = crate::native_ui::native_button_height_for_lines(lines, control_height);
    let fetch = UiNode::new(
        AI_FLEET_FETCH,
        UiContent::Button(
            Control::new(
                ButtonSpec::new("Fetch now")
                    .visual_label(visual)
                    .description("Fetches the fleet document now"),
                ActionId::new(AI_FLEET_FETCH),
            )
            .state(ControlState {
                busy: ai.looking,
                enabled: ai.enabled && ai.fleet_from_url,
                ..ControlState::default()
            })
            .style(StyleRef::Secondary),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(if lines > 1 { card_width } else { width }))
            .height(Length::Fixed(fetch_height)),
    );
    let (layout, height) = if beside {
        (Layout::row(), control_height)
    } else {
        (Layout::column(), fetch_height + CARD_GAP + open_height)
    };
    (
        UiNode::new(
            "ai/fleet-actions",
            UiContent::Group(GroupSpec::new("Fleet document actions")),
        )
        .layout(layout.height(Length::Fixed(height)).gap(CARD_GAP))
        .children(vec![fetch, open]),
        height,
    )
}

/// An Open aterm.toml under a card, its spoken label naming the key it opens
/// at, its visual label the short words, kept whole by the one rule page
/// buttons keep ([`button_words`]): on as many lines as a narrow card needs
/// ("Open aterm.t…" was all a 2× phone-width card painted).
fn open_config_button(key: &str, spoken: &str, card_width: f32) -> (UiNode, f32) {
    let visual = "Open aterm.toml";
    let width = (crate::native_ui::native_button_label_width(visual) + 40.0).min(card_width);
    let (words, lines) = button_words(visual, &[], width);
    let width = if lines > 1 { card_width } else { width };
    let height = crate::native_ui::native_button_height_for_lines(lines, scaled_control_height());
    (
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(spoken).visual_label(words),
                    ActionId::new(key),
                )
                .style(StyleRef::Secondary),
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
