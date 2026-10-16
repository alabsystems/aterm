// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS ▸ HARNESS ▸ AI CLIS (docs/DESIGN-ai-connections-2026-10-16.md
//! §10): the AI CLIs aterm keeps one working login to — Claude Code, Codex
//! CLI, Antigravity CLI, Muse Code — a SPECIAL route, Harness's sub-page,
//! built like the Harness page over the host's [`crate::ai_screen::AiState`]
//! (which the Harness snapshot carries). Four cards, in order:
//!
//! 1. **THIS MACHINE** — the registry rows Preferred AI CLI (`ai.preferred`)
//!    and Look for AI CLI problems (`ai.enabled`), each with its note, and
//!    "From the fleet document." where that is where the preferred CLI came
//!    from; under it the page's Refresh (look again, off the event loop).
//! 2. **AI CLIS** — who needs you, then one row per CLI: its name and
//!    version, its state and what is known (the doctor's words, sanitized),
//!    a **Sign in** (or **Install**) where it needs a person, and its "Use
//!    <product> on this machine" switch (`ai.<id>.use`). These say USE, not
//!    supervise: the Harness page's "Supervise … sessions" switches are the
//!    harness's hold on an agent's sessions, a thing apart. Sign in opens a
//!    new tab that runs `aterm ai login <id>` (`App::ai_sign_in`).
//! 3. **REVIEWS** — the egress grant `[ai] reviewers` and where it came from,
//!    shown and never edited here (a Settings row is drivable by `settings
//!    set`), with Open aterm.toml at it.
//! 4. **FLEET DOCUMENT** — this machine's name, the document and what became
//!    of it, and what the readers said, with Open aterm.toml at `[ai]
//!    machine`.
//!
//! The ordinary `settings/set/<key>` controls carry the rows, so OCC saves,
//! Undo, Reset (which removes this machine's key, so it follows the fleet
//! again), Search and Modified behave as on every page; the preferred CLI's
//! row opens the ordinary choice picker, which takes the page while open.

use super::harness_page::{CARD_GAP, CARD_PAD, metadata_cards, row_cards};
use super::*;
use crate::ai_screen::{AiAct, AiCliRow, AiProjection};
use crate::native_app::HarnessRequest;

/// Every action the page mints starts with this (`ai/refresh`,
/// `ai/sign-in/<id>`, `ai/open-config/<what>`).
pub(crate) const AI_ACTION_PREFIX: &str = "ai/";

/// The page's registry rows' prefix: every `[ai]` key with a row.
pub(super) const AI_KEY_PREFIX: &str = "ai.";

/// The Refresh button's key and action: look at the AI CLIs again.
pub(super) const AI_REFRESH: &str = "ai/refresh";

/// The REVIEWS card's Open aterm.toml: the editor at `[ai] reviewers`.
pub(super) const AI_OPEN_REVIEWS: &str = "ai/open-config/reviewers";

/// The FLEET DOCUMENT card's Open aterm.toml: the editor at `[ai] machine`.
pub(super) const AI_OPEN_FLEET: &str = "ai/open-config/fleet";

/// A CLI's Sign in (or Install): `ai/sign-in/<id>`.
const AI_SIGN_IN_PREFIX: &str = "ai/sign-in/";

/// The gap between a one-line row's words and its controls, a setting row's
/// own, so a status row's button stands in the switches' column.
const ROW_GAP: f32 = 12.0;

/// The page's registry rows, in the registry's order: every row of an `[ai]`
/// key ([`prefs::NESTED_LEAVES`]), so a fifth CLI's switch is on the page with
/// no list here.
pub(super) fn ai_page_rows() -> impl Iterator<Item = &'static str> {
    prefs::NESTED_LEAVES
        .iter()
        .map(|leaf| leaf.key)
        .filter(|key| key.starts_with(AI_KEY_PREFIX))
}

/// The THIS MACHINE card's rows.
const MACHINE_ROWS: [&str; 2] = [prefs::EDIT_AI_PREFERRED, prefs::EDIT_AI_ENABLED];

/// The words a row adds where its value came from the fleet document.
const FROM_FLEET: &str = "From the fleet document.";

/// The action of `id`'s Sign in (or Install).
#[must_use]
pub(super) fn sign_in_action(id: &str) -> String {
    format!("{AI_SIGN_IN_PREFIX}{id}")
}

/// The page's subtitle, which yields to nothing: it must fit the compact
/// phone page and the minimum Medium measure.
pub(super) fn ai_page_subtitle(width: SettingsWidth) -> &'static str {
    if width == SettingsWidth::Compact {
        "Logins, the preferred CLI, reviews."
    } else {
        "Each AI CLI's login, the preferred one, and reviews."
    }
}

/// The page's palette commands while it is the route.
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
    ]
}

impl SettingsApp {
    /// A press on the page ([`AI_ACTION_PREFIX`]): Refresh and a CLI's Sign in
    /// become typed host requests ([`HarnessRequest`]); Open aterm.toml is the
    /// Manual route's own act, aimed at the key its card shows. A Sign in for
    /// a CLI the newest look does not have needing a person writes nothing
    /// and says why.
    pub(super) fn reduce_ai_action(
        &mut self,
        view: &mut SettingsViewState,
        action: &str,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        if action == AI_REFRESH {
            cx.harness(HarnessRequest::AiLook);
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
                    "ai.machine"
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

/// THE PAGE: its heading pinned, then its cards as one scrolling list. While
/// a choice picker for one of its rows is open, the picker takes the page.
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
    let maximum = page_maximum(SettingsRoute::Ai, width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (heading, heading_height) = pinned_heading(
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
    let (mut out, _) = pinned_heading(width, cx.viewport, gap, SettingsRoute::Ai.label(), "");
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
    let inner = (card_width - 2.0 * CARD_PAD).max(0.0);
    let label_width = about_metadata_label_width();
    let value_width = (inner - label_width - ABOUT_METADATA_ROW_GAP).max(0.0);
    // What one row of a continued card may take: the room under its heading,
    // at the tight padding.
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
    cards.extend(metadata_cards(
        "ai/reviews",
        ("REVIEWS", "REVIEWS"),
        &ai.review_rows,
        inner,
        (value_width, label_width),
    ));
    cards.push(open_config_button(
        AI_OPEN_REVIEWS,
        "Open aterm.toml at [ai] reviewers",
        card_width,
    ));
    cards.extend(metadata_cards(
        "ai/fleet",
        ("FLEET DOCUMENT", "FLEET"),
        &ai.fleet_rows,
        inner,
        (value_width, label_width),
    ));
    cards.push(open_config_button(
        AI_OPEN_FLEET,
        "Open aterm.toml at [ai] machine",
        card_width,
    ));
    cards
}

/// The THIS MACHINE card's rows, each the way every page draws a setting
/// ([`page_setting_rows_noted`]): its note, and the fleet's word where the
/// preferred CLI came from the fleet document.
fn machine_rows(
    state: &SettingsViewState,
    ai: &AiProjection,
    width: SettingsWidth,
    inner: f32,
    row_room: f32,
) -> Vec<(UiNode, f32)> {
    MACHINE_ROWS
        .iter()
        .flat_map(|key| {
            let note = prefs::footnote_of(key).map(str::to_string);
            let note = if *key == prefs::EDIT_AI_PREFERRED && ai.preferred_from_fleet {
                Some(match note {
                    Some(note) => format!("{FROM_FLEET} {note}"),
                    None => FROM_FLEET.to_string(),
                })
            } else {
                note
            };
            page_setting_rows_noted(state, key, width, inner, note.as_deref(), row_room)
        })
        .collect()
}

/// The AI CLIS card's rows: who needs you, then for each CLI its status row
/// and its "Use on this machine" switch. The switches' shared note is drawn
/// once, under the last of them; a switch whose value came from the fleet
/// document says so.
fn cli_rows(
    state: &SettingsViewState,
    ai: &AiProjection,
    width: SettingsWidth,
    inner: f32,
    row_room: f32,
) -> Vec<(UiNode, f32)> {
    let mut rows = vec![quiet_line("ai/clis/health", &ai.health, inner)];
    let last = ai.clis.len().saturating_sub(1);
    for (index, row) in ai.clis.iter().enumerate() {
        rows.push(cli_status_row(row, width, inner));
        let key = format!("ai.{}.use", row.id);
        let shared = (index == last).then_some(prefs::AI_USE_NOTE);
        let note = match (row.use_from_fleet, shared) {
            (true, Some(shared)) => Some(format!("{FROM_FLEET} {shared}")),
            (true, None) => Some(FROM_FLEET.to_string()),
            (false, shared) => shared.map(str::to_string),
        };
        rows.extend(page_setting_rows_noted(
            state,
            &key,
            width,
            inner,
            note.as_deref(),
            row_room,
        ));
    }
    rows
}

/// One quiet line of text, fitted to `width` (its node carries it whole).
fn quiet_line(key: &str, text: &str, width: f32) -> (UiNode, f32) {
    let height = 22.0_f32.max(18.0 * settings_text_scale());
    (
        UiNode::new(
            key,
            UiContent::Text(TextSpec {
                text: crate::native_ui::fit_native_text_label(text, width),
                role: SemanticRole::Status,
                style: StyleRef::Quiet,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        ),
        height,
    )
}

/// One CLI's status row: its name and version over its state and fact
/// (fitted to the text column; the row's group carries both whole), and its
/// Sign in or Install where it needs a person. A row on a line where the
/// measure allows; stacked on a compact page.
fn cli_status_row(row: &AiCliRow, width: SettingsWidth, inner: f32) -> (UiNode, f32) {
    let scale = settings_text_scale();
    let title_height = 22.0_f32.max(18.0 * scale);
    let detail_height = 18.0_f32.max(16.0 * scale);
    let control_height = scaled_control_height();
    let compact = width == SettingsWidth::Compact;
    let key = format!("ai/cli/{}", row.id);
    let button = row.act.map(|act| {
        let label = act.label();
        // One width for every act, so the buttons stand as one column.
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
                    .height(if compact {
                        Length::Fixed(control_height)
                    } else {
                        Length::Fill
                    }),
            ),
            button_width,
        )
    });
    // Beside its words, the button keeps the setting rows' control column:
    // the same gap, and Reset's place after it (`reset_slot_width`).
    let text_width = match (&button, compact) {
        (Some((_, button_width)), false) => {
            (inner - button_width - reset_slot_width() - 2.0 * ROW_GAP).max(0.0)
        }
        _ => inner,
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
    // A setting row's band: its controls inset 4 above and below.
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

/// An Open aterm.toml under a card, its spoken label naming the key it opens
/// at, its visual label the short words.
fn open_config_button(key: &str, spoken: &str, card_width: f32) -> (UiNode, f32) {
    let height = scaled_control_height();
    let visual = "Open aterm.toml";
    let width = (crate::native_ui::native_button_label_width(visual) + 40.0).min(card_width);
    (
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(spoken).visual_label(
                        crate::native_ui::fit_native_button_label(visual, (width - 20.0).max(0.0)),
                    ),
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
