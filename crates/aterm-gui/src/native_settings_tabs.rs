// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS ▸ HARNESS ▸ TAB CONTROL (`/tabs`): accessible computers, `aterm`
//! sessions, and open tabs with live telemetry and AI CLI status.

use super::harness_page::{metadata_cards, row_cards};
use super::*;
use crate::harness_screen::{TabComputer, TabControlProjection, TabSpreadsheetRow};
use crate::native_app::HarnessRequest;

/// Prefix for actions minted by the Tab Control page.
pub(crate) const TABS_ACTION_PREFIX: &str = "tabs/";

/// Key and action for the Refresh button.
pub(crate) const TABS_REFRESH: &str = "tabs/refresh";

/// Prefix for a local tab row's `Show tab` action (`tabs/show/<local_id>`).
pub(crate) const TABS_SHOW_PREFIX: &str = "tabs/show/";

/// The Harness navigation button's key on the Tab Control page.
pub(crate) const TABS_TO_HARNESS: &str = "tabs/to-harness";

/// The AI CLIs navigation button's key on the Tab Control page.
pub(crate) const TABS_TO_AI: &str = "tabs/to-ai";

/// Gap between a spreadsheet row's text columns and its `Show tab` button.
const ROW_GAP: f32 = 12.0;

/// Action ID for raising a local tab by its process-local session ID.
#[must_use]
pub(crate) fn show_tab_action(local_id: u64) -> String {
    format!("{TABS_SHOW_PREFIX}{local_id}")
}

/// The page's subtitle, fitted to both compact and wide layouts.
pub(super) fn tabs_page_subtitle(width: SettingsWidth) -> &'static str {
    if width == SettingsWidth::Compact {
        "Computers, sessions, live AI CLIs."
    } else {
        "Accessible computers, aterm sessions, tabs, and live AI CLI status."
    }
}

/// The page's Command Palette commands while it is the active route.
pub(super) fn tabs_commands(harness: &crate::harness_screen::HarnessState) -> Vec<Command> {
    vec![
        Command {
            id: ActionId::new(TABS_REFRESH),
            title: "Tab Control: Refresh".to_string(),
            shortcut: None,
            enabled: !harness.stats.collecting,
        },
        Command {
            id: route_action(SettingsRoute::Harness),
            title: "Tab Control: Open Coding Agent Harness".to_string(),
            shortcut: None,
            enabled: true,
        },
        Command {
            id: route_action(SettingsRoute::Ai),
            title: "Tab Control: Open AI CLIs".to_string(),
            shortcut: None,
            enabled: true,
        },
    ]
}

impl SettingsApp {
    /// Handle an action on Settings ▸ Harness ▸ Tab Control (`tabs/…`).
    pub(super) fn reduce_tabs_action(
        &mut self,
        view: &mut SettingsViewState,
        action: &str,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        if action == TABS_REFRESH {
            cx.harness(HarnessRequest::Refresh);
            view.feedback = Some("Refreshing computers, sessions, and tabs\u{2026}".to_string());
        } else if let Some(rest) = action.strip_prefix(TABS_SHOW_PREFIX)
            && let Ok(session) = rest.parse::<u64>()
        {
            cx.harness(HarnessRequest::Show { session });
        } else {
            return EventResult::Bubble;
        }
        cx.repaint(crate::native_app::DamageRegion::All);
        EventResult::Handled
    }
}

/// Render Settings ▸ Harness ▸ Tab Control (`/tabs`).
pub(super) fn tabs_page(
    state: &SettingsViewState,
    tabs: &TabControlProjection,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    let viewport = cx.viewport;
    let gap = settings_page_gap(state, width, viewport);
    let maximum = PageLayout::Form.maximum(width);
    let page_width = settings_page_content_width(viewport.width, width, maximum);
    let (heading, heading_height) = pinned_heading(
        state,
        width,
        viewport,
        gap,
        SettingsRoute::Tabs.label(),
        tabs_page_subtitle(width),
    );
    let room = (settings_list_body_height(state, width, viewport) - heading_height)
        .max(scaled_control_height());
    let cards = tabs_cards(tabs, width, page_width, room);
    let mut out = heading;
    out.push(scroll::list_page_peeking(
        state,
        "settings/list",
        "Tab Control",
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

fn tabs_cards(
    tabs: &TabControlProjection,
    width: SettingsWidth,
    card_width: f32,
    room: f32,
) -> Vec<(UiNode, f32)> {
    let inner = card_text_width(card_width);
    let label_width = fact_label_width(&tabs.summary_rows, inner);
    let mut cards = Vec::new();
    cards.extend(metadata_cards(
        "tabs/summary",
        ("FLEET & TAB SUMMARY", "SUMMARY"),
        &tabs.summary_rows,
        card_width,
        label_width,
    ));
    cards.push(tabs_actions(tabs, card_width));
    cards.extend(row_cards(
        "tabs/spreadsheet",
        (
            "Sessions and tabs spreadsheet",
            ("SESSIONS & TABS SPREADSHEET", "SESSIONS & TABS"),
        ),
        spreadsheet_rows(tabs, width, inner, room),
        room,
        card_width,
    ));
    cards.extend(row_cards(
        "tabs/computers",
        (
            "Accessible computers",
            ("ACCESSIBLE COMPUTERS", "COMPUTERS"),
        ),
        computer_rows(tabs, width, inner),
        room,
        card_width,
    ));
    cards
}

/// Refresh, Coding Agent Harness and AI CLIs: as many to a line as the card
/// holds, each at its words' width, a label wider than the line on as many
/// lines as it needs ([`page_button_lines`]) — never cut ("Coding Agent
/// Har…" at 22 columns).
fn tabs_actions(tabs: &TabControlProjection, card_width: f32) -> (UiNode, f32) {
    let specs = [
        (
            TABS_REFRESH,
            "Refresh",
            tabs.collecting,
            StyleRef::Primary,
            ActionId::new(TABS_REFRESH),
        ),
        (
            TABS_TO_HARNESS,
            "Coding Agent Harness",
            false,
            StyleRef::Secondary,
            route_action(SettingsRoute::Harness),
        ),
        (
            TABS_TO_AI,
            "AI CLIs",
            false,
            StyleRef::Secondary,
            route_action(SettingsRoute::Ai),
        ),
    ];
    let buttons = specs
        .into_iter()
        .map(|(key, label, busy, style, action)| {
            UiNode::new(
                key,
                UiContent::Button(
                    Control::new(ButtonSpec::new(label), action)
                        .state(ControlState {
                            busy,
                            ..ControlState::default()
                        })
                        .style(style),
                ),
            )
        })
        .collect();
    let (mut node, height) = page_button_lines("tabs/actions", buttons, card_width);
    node.content = UiContent::Group(GroupSpec::new("Tab Control actions"));
    (node, height)
}

/// A line of the page's words, wrapped to its card's column, never cut
/// ([`wrapped_words_node`]).
fn header_line(key: &str, text: &str, width: f32) -> (UiNode, f32) {
    wrapped_words_node(
        key,
        text,
        (SemanticRole::Status, StyleRef::Quiet),
        width,
        detail_line_height(),
    )
}

/// One line of a row's title.
fn title_line_height() -> f32 {
    22.0_f32.max(18.0 * settings_text_scale())
}

/// One line of a row's detail.
fn detail_line_height() -> f32 {
    18.0_f32.max(16.0 * settings_text_scale())
}

/// `text` in the row's title type, wrapped to `width`, paint-only.
fn title_words(key: String, text: &str, width: f32) -> (UiNode, f32) {
    let (node, height) = wrapped_words_node(
        &key,
        text,
        (SemanticRole::Text, StyleRef::Primary),
        width,
        title_line_height(),
    );
    (node.paint_only(), height)
}

/// `text` in the row's detail type, wrapped to `width`, paint-only.
fn detail_words(key: String, text: &str, width: f32) -> (UiNode, f32) {
    let (node, height) = wrapped_words_node(
        &key,
        text,
        (SemanticRole::Status, StyleRef::Quiet),
        width,
        detail_line_height(),
    );
    (node.paint_only(), height)
}

fn computer_rows(
    tabs: &TabControlProjection,
    _width: SettingsWidth,
    inner: f32,
) -> Vec<(UiNode, f32)> {
    if !tabs.observed {
        return vec![header_line(
            "tabs/computers/loading",
            "Reading accessible computers\u{2026}",
            inner,
        )];
    }
    let mut rows = vec![header_line(
        "tabs/computers/header",
        "COMPUTER \u{b7} STATUS \u{b7} IPV4 & HOSTNAME \u{2014} LIVE SYSTEM VITALS (CPU / GPU / RAM / DISK / UPTIME / RTT) \u{2014} HARDWARE \u{b7} OS & SSH \u{b7} SESSIONS",
        inner,
    )];
    if tabs.computers.is_empty() {
        rows.push(header_line(
            "tabs/computers/empty",
            "No accessible computers found.",
            inner,
        ));
        return rows;
    }
    for comp in &tabs.computers {
        rows.push(computer_row(comp, inner));
    }
    rows
}

/// One accessible computer: its name, state and address, its vitals where it
/// reports them, and its system and sessions, each wrapped to the card's
/// column, never cut ("m110-MacBook-P…" at 22 columns).
fn computer_row(comp: &TabComputer, inner: f32) -> (UiNode, f32) {
    let key = format!("tabs/computer/{}", comp.id);
    let line1 = format!(
        "{} \u{b7} {} \u{b7} {}",
        comp.name, comp.status, comp.address
    );
    let line2 = if comp.id.starts_with("lan/") {
        format!("{} \u{b7} sessions unobserved", comp.os_version)
    } else {
        format!(
            "{} \u{b7} {} aterm {} \u{b7} {} open {} \u{b7} {}",
            comp.os_version,
            comp.instances,
            if comp.instances == 1 {
                "instance"
            } else {
                "instances"
            },
            comp.tabs,
            if comp.tabs == 1 { "tab" } else { "tabs" },
            if comp.ai_clis == 0 {
                "no AI CLI".to_string()
            } else {
                format!(
                    "{} {} ({} active)",
                    comp.ai_clis,
                    if comp.ai_clis == 1 {
                        "AI CLI"
                    } else {
                        "AI CLIs"
                    },
                    comp.active_ai
                )
            }
        )
    };
    let has_vitals = !comp.vitals.is_empty();
    let full_label = if has_vitals {
        format!("{line1} \u{2014} {} \u{2014} {line2}", comp.vitals)
    } else {
        format!("{line1} \u{2014} {line2}")
    };
    let mut parts = vec![title_words(format!("{key}/title"), &line1, inner)];
    if has_vitals {
        parts.push(detail_words(format!("{key}/vitals"), &comp.vitals, inner));
    }
    parts.push(detail_words(format!("{key}/detail"), &line2, inner));
    let total_height = parts.iter().map(|(_, height)| height).sum::<f32>()
        + 4.0 * parts.len().saturating_sub(1) as f32;
    let node = UiNode::new(key, UiContent::Group(GroupSpec::new(full_label)))
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fixed(total_height))
                .gap(4.0),
        )
        .children(parts.into_iter().map(|(node, _)| node).collect());
    (node, total_height)
}

fn spreadsheet_rows(
    tabs: &TabControlProjection,
    width: SettingsWidth,
    inner: f32,
    room: f32,
) -> Vec<(UiNode, f32)> {
    if !tabs.observed {
        return vec![header_line(
            "tabs/spreadsheet/loading",
            "Reading aterm sessions and tabs\u{2026}",
            inner,
        )];
    }
    let mut rows = vec![header_line(
        "tabs/spreadsheet/header",
        "HOST & ATERM SESSION \u{2192} INDENTED TABS (GRID DIMS & CWD) \u{b7} PROGRAM & AI CLI STATUS",
        inner,
    )];
    if tabs.tabs.is_empty() {
        rows.push(header_line(
            "tabs/spreadsheet/empty",
            "No open aterm tabs across accessible computers.",
            inner,
        ));
        return rows;
    }
    let mut groups: Vec<(String, String, Vec<&TabSpreadsheetRow>)> = Vec::new();
    for row in &tabs.tabs {
        if let Some(group) = groups
            .iter_mut()
            .find(|(c, i, _)| c == &row.computer && i == &row.instance)
        {
            group.2.push(row);
        } else {
            groups.push((row.computer.clone(), row.instance.clone(), vec![row]));
        }
    }
    for (group_idx, (computer, instance, group_rows)) in groups.into_iter().enumerate() {
        rows.push(session_group_header_row(
            group_idx,
            &computer,
            &instance,
            &group_rows,
            inner,
        ));
        let count = group_rows.len();
        for (idx, row) in group_rows.into_iter().enumerate() {
            let is_last = idx + 1 == count;
            rows.push(spreadsheet_row(row, is_last, width, (inner, room)));
        }
    }
    rows
}

fn session_group_header_row(
    group_idx: usize,
    computer: &str,
    instance: &str,
    group_rows: &[&TabSpreadsheetRow],
    inner: f32,
) -> (UiNode, f32) {
    let key = format!("tabs/session/{group_idx}");
    let dims = group_rows
        .iter()
        .find_map(|r| {
            r.place
                .split(crate::native_ui::FACT_SEPARATOR)
                .find(|part| {
                    part.contains('\u{d7}')
                        && part.chars().next().is_some_and(|c| c.is_ascii_digit())
                })
        })
        .map(|d| format!("{}{d} grid", crate::native_ui::FACT_SEPARATOR))
        .unwrap_or_default();
    let tab_count = group_rows.len();
    let ai_count = group_rows.iter().filter(|r| r.is_ai_cli).count();
    let active_ai = group_rows
        .iter()
        .filter(|r| {
            r.is_ai_cli
                && (r.ai_status.starts_with("working") || r.ai_status.starts_with("waiting"))
        })
        .count();
    let mut progs: Vec<String> = Vec::new();
    for r in group_rows {
        if !progs.contains(&r.program) {
            progs.push(r.program.clone());
        }
    }
    let text = format!(
        "\u{25b8} {} \u{b7} {}{} \u{2014} {} open {} \u{b7} {} AI {} ({} active) \u{b7} [{}]",
        computer,
        instance,
        dims,
        tab_count,
        if tab_count == 1 { "tab" } else { "tabs" },
        ai_count,
        if ai_count == 1 { "CLI" } else { "CLIs" },
        active_ai,
        progs.join(", ")
    );
    wrapped_words_node(
        &key,
        &text,
        (SemanticRole::Text, StyleRef::Primary),
        inner,
        title_line_height(),
    )
}

// Words are laid out as a column of lines, each beside its branch stem, so rows
// taller than the viewport split cleanly across screens (`scroll::row_parts`).
/// One open tab: its place, title, program and state, then its AI CLI's
/// or its program's status, each wrapped to the words' column, never cut,
/// under the tree's branch (which runs down beside every line of a row that
/// another follows); and `Show tab` for a local tab, beside the words where
/// the card holds both and the list holds the row whole (`room`), else
/// under them.
fn spreadsheet_row(
    row: &TabSpreadsheetRow,
    is_last: bool,
    width: SettingsWidth,
    (inner, room): (f32, f32),
) -> (UiNode, f32) {
    let control_height = scaled_control_height();
    let compact = width == SettingsWidth::Compact;
    let key = format!("tabs/row/{}", row.key);
    let button_width = (crate::native_ui::native_button_label_width("Show tab") + 40.0).min(inner);

    let show_button = row.show_local_id.map(|local_id| {
        let action = show_tab_action(local_id);
        let spoken = format!("Show tab: {} ({})", row.title, row.place);
        let spec = ButtonSpec::new(spoken);
        let (visual, lines) = button_words_in(&spec, "Show tab", &[], button_width);
        UiNode::new(
            action.clone(),
            UiContent::Button(
                Control::new(spec.visual_label(visual), ActionId::new(action))
                    .style(StyleRef::Secondary),
            ),
        )
        .layout(Layout::default().width(Length::Fixed(button_width)).height(
            Length::Fixed(crate::native_ui::native_button_height_for_lines(
                lines,
                control_height,
            )),
        ))
    });
    let button_height = show_button.as_ref().map_or(0.0, fixed_height);

    let (branch, indent) = if is_last {
        ("   \u{2514}\u{2500}", "      ")
    } else {
        ("   \u{251c}\u{2500}", "   \u{2502}  ")
    };
    let title = format!(
        "{} \u{2014} \"{}\" \u{b7} {} \u{b7} {} ({})",
        row.place, row.title, row.program, row.state, row.sid
    );
    let vitals_suffix = if row.vitals.is_empty() {
        String::new()
    } else {
        format!(" \u{b7} vitals: {}", row.vitals)
    };
    let status = if row.is_ai_cli {
        format!(
            "AI CLI: {} \u{b7} autopilot: {} \u{b7} human: {}{vitals_suffix}",
            row.ai_status, row.autopilot, row.human
        )
    } else {
        format!(
            "status: {} \u{b7} program: {} \u{b7} human: {}{vitals_suffix}",
            row.state, row.program, row.human
        )
    };
    let full_label = format!("{branch} {title} \u{2014} {indent}{status}");
    let (px, face) = crate::native_ui::text_paint_metrics(SemanticRole::Text, StyleRef::Primary);
    let branch_width =
        crate::tray_raster::ui_text_width_for(face, &format!("{branch} "), px).ceil();
    let lines_in = |text_width: f32| {
        let words_width = (text_width - branch_width).max(0.0);
        let wrap = |text: &str, (role, style): (SemanticRole, StyleRef)| {
            let (px, face) = crate::native_ui::text_paint_metrics(role, style);
            wrap_to_box(text, words_width, &|line| {
                crate::tray_raster::ui_text_width_for(face, line, px)
            })
        };
        let title = wrap(&title, (SemanticRole::Text, StyleRef::Primary));
        let status = wrap(&status, (SemanticRole::Status, StyleRef::Quiet));
        let height = title.len().max(1) as f32 * title_line_height()
            + 4.0
            + status.len().max(1) as f32 * detail_line_height();
        (title, status, height, words_width)
    };
    let beside_width = (inner - button_width - ROW_GAP).max(0.0);
    let beside = show_button.is_some()
        && !compact
        && lines_in(beside_width).2.max(button_height) <= room + 0.01;
    let text_width = if beside { beside_width } else { inner };
    let (title_lines, status_lines, text_height, words_width) = lines_in(text_width);
    let stem = if is_last { "" } else { "   \u{2502}" };
    let line_row = |index: usize,
                    glyph: &str,
                    text: String,
                    (role, style): (SemanticRole, StyleRef),
                    height: f32| {
        UiNode::new(
            format!("{key}/line-{index}"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::row()
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
        .children(vec![
            UiNode::new(
                format!("{key}/branch/{index}"),
                UiContent::Text(TextSpec {
                    text: glyph.to_string(),
                    role: SemanticRole::Text,
                    style: StyleRef::Primary,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(branch_width))
                    .height(Length::Fill),
            )
            .paint_only(),
            UiNode::new(
                format!("{key}/words/{index}"),
                UiContent::Text(TextSpec { text, role, style }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(words_width))
                    .height(Length::Fill),
            )
            .paint_only(),
        ])
        .paint_only()
    };
    let title_count = title_lines.len().max(1);
    let mut lines: Vec<UiNode> = Vec::new();
    for (index, text) in title_lines.into_iter().enumerate() {
        let glyph = if index == 0 { branch } else { stem };
        lines.push(line_row(
            index,
            glyph,
            text,
            (SemanticRole::Text, StyleRef::Primary),
            title_line_height(),
        ));
    }
    lines.push(
        UiNode::new(
            format!("{key}/gap"),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(4.0)),
        )
        .paint_only(),
    );
    for (index, text) in status_lines.into_iter().enumerate() {
        lines.push(line_row(
            title_count + index,
            stem,
            text,
            (SemanticRole::Status, StyleRef::Quiet),
            detail_line_height(),
        ));
    }
    let text_col = UiNode::new(
        if show_button.is_some() {
            format!("{key}/words")
        } else {
            key.clone()
        },
        UiContent::Group(GroupSpec::new(full_label.clone())),
    )
    .layout(
        Layout::column()
            .width(if beside {
                Length::Fixed(text_width)
            } else {
                Length::Fill
            })
            .height(Length::Fixed(text_height)),
    )
    .children(lines);

    match (show_button, beside) {
        (None, _) => (text_col, text_height),
        (Some(btn), false) => {
            let total = text_height + 6.0 + button_height;
            (
                UiNode::new(key, UiContent::Group(GroupSpec::new(full_label)))
                    .layout(
                        Layout::column()
                            .width(Length::Fill)
                            .height(Length::Fixed(total))
                            .gap(6.0),
                    )
                    .children(vec![text_col.paint_only(), btn]),
                total,
            )
        }
        (Some(btn), true) => {
            let total = text_height.max(button_height);
            (
                UiNode::new(key, UiContent::Group(GroupSpec::new(full_label)))
                    .layout(
                        Layout::row()
                            .width(Length::Fill)
                            .height(Length::Fixed(total))
                            .gap(ROW_GAP),
                    )
                    .children(vec![text_col.paint_only(), btn]),
                total,
            )
        }
    }
}
