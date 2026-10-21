// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS ▸ HARNESS ▸ TAB CONTROL (`/tabs`): the native Tab Control Panel
//! showing all accessible computers (`this Mac` plus managed LAN/remote Macs),
//! `aterm` sessions/instances, and their open tabs in a spreadsheet list with
//! live telemetry and live AI CLI status (`working: thinking`, `waiting on
//! permission`, `idle at prompt`, Autopilot mode, and last human activity).
//!
//! Three cards, in order:
//! 1. **FLEET & TAB SUMMARY** (`tabs/summary`) and **Actions** (`tabs/actions`:
//!    `Refresh`, `Harness`, `AI CLIs`).
//! 2. **ACCESSIBLE COMPUTERS** (`tabs/computers`) — spreadsheet rows for each
//!    accessible Mac (computer name, reachability, address, macOS version,
//!    `aterm` instance count, open tab count, and active AI CLI count).
//! 3. **SESSIONS & TABS SPREADSHEET** (`tabs/spreadsheet`) — spreadsheet rows
//!    for every open tab across all computers and `aterm` instances, showing
//!    computer, instance, tab/window place, title, foreground program or AI
//!    CLI product name, live AI CLI status and duration, Autopilot mode, human
//!    activity, and a `Show tab` button for local tabs.

use super::harness_page::{CARD_GAP, metadata_cards, row_cards};
use super::*;
use crate::harness_screen::{TabComputer, TabControlProjection, TabSpreadsheetRow};
use crate::native_app::HarnessRequest;

/// Every action the Tab Control page mints starts with this (`tabs/refresh`,
/// `tabs/show/<local_id>`).
pub(crate) const TABS_ACTION_PREFIX: &str = "tabs/";

/// The Refresh button's key and action: scan computers, sibling sockets, and
/// tabs again off the event loop.
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
        spreadsheet_rows(tabs, width, inner),
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

fn tabs_actions(tabs: &TabControlProjection, card_width: f32) -> (UiNode, f32) {
    let control_height = scaled_control_height();
    let need = |label: &str| crate::native_ui::native_button_label_width(label) + 40.0;
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
    let total_need: f32 = specs
        .iter()
        .map(|(_, label, _, _, _)| need(label))
        .sum::<f32>()
        + (specs.len().saturating_sub(1) as f32) * CARD_GAP;
    let row = total_need <= card_width;
    let buttons = specs
        .into_iter()
        .map(|(key, label, busy, style, action)| {
            let btn_width = need(label);
            let visual = crate::native_ui::fit_native_button_label(
                label,
                ((if row { btn_width } else { card_width }) - 24.0).max(0.0),
            );
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
                        Length::Fixed(btn_width)
                    } else {
                        Length::Fill
                    })
                    .height(Length::Fixed(control_height)),
            )
        })
        .collect();
    let height = if row {
        control_height
    } else {
        3.0 * control_height + 2.0 * CARD_GAP
    };
    let layout = if row { Layout::row() } else { Layout::column() };
    (
        UiNode::new(
            "tabs/actions",
            UiContent::Group(GroupSpec::new("Tab Control actions")),
        )
        .layout(layout.height(Length::Fixed(height)).gap(CARD_GAP))
        .children(buttons),
        height,
    )
}

fn header_line(key: &str, text: &str, width: f32) -> (UiNode, f32) {
    let height = 20.0_f32.max(16.0 * settings_text_scale());
    (
        UiNode::new(
            key,
            UiContent::Text(TextSpec {
                text: crate::native_ui::fit_native_status_label(text, width),
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

fn computer_row(comp: &TabComputer, inner: f32) -> (UiNode, f32) {
    let scale = settings_text_scale();
    let title_height = 22.0_f32.max(18.0 * scale);
    let detail_height = 18.0_f32.max(16.0 * scale);
    let has_vitals = !comp.vitals.is_empty();
    let total_height = if has_vitals {
        title_height + 4.0 + detail_height + 4.0 + detail_height
    } else {
        title_height + 4.0 + detail_height
    };
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
    let full_label = if has_vitals {
        format!("{line1} \u{2014} {} \u{2014} {line2}", comp.vitals)
    } else {
        format!("{line1} \u{2014} {line2}")
    };
    let mut children = vec![
        UiNode::new(
            format!("{key}/title"),
            UiContent::Text(TextSpec {
                text: crate::native_ui::fit_native_text_label(&line1, inner),
                role: SemanticRole::Text,
                style: StyleRef::Primary,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(title_height)),
        )
        .paint_only(),
    ];
    if has_vitals {
        children.push(
            UiNode::new(
                format!("{key}/vitals"),
                UiContent::Text(TextSpec {
                    text: crate::native_ui::fit_native_status_label(&comp.vitals, inner),
                    role: SemanticRole::Status,
                    style: StyleRef::Quiet,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(detail_height)),
            )
            .paint_only(),
        );
    }
    children.push(
        UiNode::new(
            format!("{key}/detail"),
            UiContent::Text(TextSpec {
                text: crate::native_ui::fit_native_status_label(&line2, inner),
                role: SemanticRole::Status,
                style: StyleRef::Quiet,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(detail_height)),
        )
        .paint_only(),
    );
    let node = UiNode::new(key, UiContent::Group(GroupSpec::new(full_label)))
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fixed(total_height))
                .gap(4.0),
        )
        .children(children);
    (node, total_height)
}

fn spreadsheet_rows(
    tabs: &TabControlProjection,
    width: SettingsWidth,
    inner: f32,
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
            rows.push(spreadsheet_row(row, is_last, width, inner));
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
    let scale = settings_text_scale();
    let height = 22.0_f32.max(18.0 * scale);
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
    (
        UiNode::new(
            key,
            UiContent::Text(TextSpec {
                text: crate::native_ui::fit_native_text_label(&text, inner),
                role: SemanticRole::Text,
                style: StyleRef::Primary,
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

fn spreadsheet_row(
    row: &TabSpreadsheetRow,
    is_last: bool,
    width: SettingsWidth,
    inner: f32,
) -> (UiNode, f32) {
    let scale = settings_text_scale();
    let title_height = 22.0_f32.max(18.0 * scale);
    let detail_height = 18.0_f32.max(16.0 * scale);
    let control_height = scaled_control_height();
    let compact = width == SettingsWidth::Compact;
    let key = format!("tabs/row/{}", row.key);
    let button_width = (crate::native_ui::native_button_label_width("Show tab") + 40.0).min(inner);

    let show_button = row.show_local_id.map(|local_id| {
        let action = show_tab_action(local_id);
        let spoken = format!("Show tab: {} ({})", row.title, row.place);
        let visual =
            crate::native_ui::fit_native_button_label("Show tab", (button_width - 20.0).max(0.0));
        UiNode::new(
            action.clone(),
            UiContent::Button(
                Control::new(
                    ButtonSpec::new(spoken).visual_label(visual),
                    ActionId::new(action),
                )
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
                .height(Length::Fixed(control_height)),
        )
    });

    let text_width = if show_button.is_some() && !compact {
        (inner - button_width - ROW_GAP).max(0.0)
    } else {
        inner
    };

    let branch = if is_last {
        "   \u{2514}\u{2500}"
    } else {
        "   \u{251c}\u{2500}"
    };
    let indent = if is_last { "      " } else { "   \u{2502}  " };
    let line1 = format!(
        "{branch} {} \u{2014} \"{}\" \u{b7} {} \u{b7} {} ({})",
        row.place, row.title, row.program, row.state, row.sid
    );
    let vitals_suffix = if row.vitals.is_empty() {
        String::new()
    } else {
        format!(" \u{b7} vitals: {}", row.vitals)
    };
    let line2 = if row.is_ai_cli {
        format!(
            "{indent}AI CLI: {} \u{b7} autopilot: {} \u{b7} human: {}{vitals_suffix}",
            row.ai_status, row.autopilot, row.human
        )
    } else {
        format!(
            "{indent}status: {} \u{b7} program: {} \u{b7} human: {}{vitals_suffix}",
            row.state, row.program, row.human
        )
    };
    let full_label = format!("{line1} \u{2014} {line2}");
    let text_height = title_height + 4.0 + detail_height;
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
            .width(if show_button.is_some() && !compact {
                Length::Fixed(text_width)
            } else {
                Length::Fill
            })
            .height(Length::Fixed(text_height))
            .gap(4.0),
    )
    .children(vec![
        UiNode::new(
            format!("{key}/title"),
            UiContent::Text(TextSpec {
                text: crate::native_ui::fit_native_text_label(&line1, text_width),
                role: SemanticRole::Text,
                style: StyleRef::Primary,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(title_height)),
        )
        .paint_only(),
        UiNode::new(
            format!("{key}/status"),
            UiContent::Text(TextSpec {
                text: crate::native_ui::fit_native_status_label(&line2, text_width),
                role: SemanticRole::Status,
                style: StyleRef::Quiet,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(detail_height)),
        )
        .paint_only(),
    ]);

    match (show_button, compact) {
        (None, _) => (text_col, text_height),
        (Some(btn), true) => {
            let total = text_height + 6.0 + control_height;
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
        (Some(btn), false) => {
            let total = text_height.max(control_height);
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
