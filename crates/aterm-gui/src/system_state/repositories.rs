// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recorded acquisition state, not current inventory readiness. Stable destination identity
//! survives acquisition attempts; completed observations remain historical after their owner exits.

use super::{SystemStateApp, SystemStateViewState};
use crate::native_ui::{
    ActionId, AuditedCustomNode, ButtonSpec, Control, ControlState, GroupSpec, Layout, Length,
    NodeMotion, SemanticRole, StyleRef, TextSpec, UiContent, UiNode,
};
use aterm_repository_progress::Progress;

pub(crate) const BAR_AUDIT: &str = "system-state.repository-progress";

/// Destination identity stays independent of attempts and fits the semantic action protocol.
fn identity(destination: &std::path::Path) -> String {
    format!(
        "{:032x}",
        aterm_hash::fnv1a_128(destination.as_os_str().as_encoded_bytes())
    )
}

pub(super) fn act(view: &mut SystemStateViewState, key: &str) -> bool {
    if key == "system-state/repositories/disclose" {
        view.repositories_open = !view.repositories_open;
    } else if let Some(sort) = key
        .strip_prefix("system-state/repositories/sort/")
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|s| *s < 3)
    {
        if view.repository_sort == sort {
            view.repository_reverse = !view.repository_reverse;
        } else {
            view.repository_sort = sort;
            view.repository_reverse = false;
        }
    } else if let Some(name) = key.strip_prefix("system-state/repositories/repo/") {
        if !view.repository_open.remove(name) {
            view.repository_open.insert(name.to_owned());
        }
    } else {
        return false;
    }
    true
}

fn group(key: &str, children: Vec<UiNode>, height: f32, row: bool) -> UiNode {
    UiNode::new(
        key.to_owned(),
        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
    )
    .layout(
        (if row { Layout::row() } else { Layout::column() })
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    )
    .children(children)
}

fn words(key: String, text: String, height: f32) -> UiNode {
    UiNode::new(
        key,
        UiContent::Text(TextSpec {
            text,
            role: SemanticRole::Text,
            style: StyleRef::Secondary,
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(height)),
    )
}

fn button(key: String, label: String, expanded: Option<bool>) -> UiNode {
    let spec = ButtonSpec::new(label);
    UiNode::new(
        key.clone(),
        UiContent::Button(Control::new(spec, ActionId::new(key)).state(ControlState {
            expanded,
            ..ControlState::default()
        })),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(28.0)),
    )
}

fn bar(
    key: String,
    label: String,
    value: Option<(u32, u32)>,
    motion: Option<NodeMotion>,
) -> UiNode {
    UiNode::new(
        key,
        UiContent::Custom(AuditedCustomNode {
            audit_id: BAR_AUDIT,
            role: SemanticRole::ProgressIndicator,
            label,
            paint: None,
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
            progress: value,
            motion: if value.is_none() { motion } else { None },
        }),
    )
    .layout(
        Layout::default()
            .width(Length::Fill)
            .height(Length::Fixed(8.0)),
    )
}

fn compare(a: &Progress, b: &Progress, sort: u8, reverse: bool) -> std::cmp::Ordering {
    let direction = |order: std::cmp::Ordering| if reverse { order.reverse() } else { order };
    match sort {
        1 => direction(a.name.cmp(&b.name)),
        2 => direction(a.phase.label().cmp(b.phase.label())).then_with(|| {
            match (a.percent, b.percent) {
                (Some(a), Some(b)) => direction(b.cmp(&a)),
                // An unknown counter is not zero or completion in either direction.
                _ => a.percent.is_none().cmp(&b.percent.is_none()),
            }
        }),
        _ => direction(
            a.phase
                .rank()
                .cmp(&b.phase.rank())
                .then_with(|| a.phase.label().cmp(b.phase.label()))
                .then_with(|| b.percent.cmp(&a.percent)),
        ),
    }
    .then_with(|| direction(a.name.cmp(&b.name)))
}

fn sort_label(view: &SystemStateViewState, column: u8, label: &str) -> String {
    if view.repository_sort != column {
        return label.into();
    }
    let descending = (column == 2) != view.repository_reverse;
    format!("{label} {}", if descending { "↓" } else { "↑" })
}

fn speed_words(repo: &Progress, now: u64) -> String {
    let age = now.saturating_sub(repo.updated_ms) / 1000;
    if repo.speed.is_some() && age >= 5 && repo.phase.rank() == 1 {
        format!("quiet {age}s")
    } else {
        repo.speed.clone().unwrap_or_else(|| "—".into())
    }
}

fn elapsed_seconds(repo: &Progress, now: u64) -> u64 {
    (if repo.phase.is_finished() {
        repo.updated_ms
    } else {
        now
    })
    .saturating_sub(repo.started_ms)
        / 1000
}

fn report_age_seconds(updated_ms: u64, now: u64) -> Option<u64> {
    if updated_ms == 0 {
        None
    } else {
        now.checked_sub(updated_ms).map(|age| age / 1000)
    }
}

pub(super) fn table(
    app: &SystemStateApp,
    view: &SystemStateViewState,
    width: f32,
    motion: Option<NodeMotion>,
) -> (UiNode, f32) {
    let prefix = "system-state/repositories";
    let usable = app.repositories.iter().filter(|r| r.usable).count();
    let total = app.repositories.len();
    let active = app
        .repositories
        .iter()
        .filter(|r| r.phase.rank() == 1)
        .count();
    let mut children = vec![
        button(
            format!("{prefix}/disclose"),
            format!("Repo records · {usable}/{total} usable · {active} active"),
            Some(view.repositories_open),
        ),
        bar(
            format!("{prefix}/overall"),
            format!("Last recorded usable: {usable} of {total} destination records"),
            Some((
                u32::try_from(usable).unwrap_or(u32::MAX),
                u32::try_from(total).unwrap_or(u32::MAX),
            )),
            None,
        ),
    ];
    let mut height = 36.0;
    if view.repositories_open {
        let mut header = vec![
            button(
                format!("{prefix}/sort/1"),
                sort_label(view, 1, "Repository"),
                None,
            ),
            button(
                format!("{prefix}/sort/0"),
                sort_label(view, 0, "Recorded state"),
                None,
            ),
            button(
                format!("{prefix}/sort/2"),
                sort_label(view, 2, if width >= 640.0 { "Stage %" } else { "%" }),
                None,
            ),
        ];
        if width >= 640.0 {
            header.push(words(
                format!("{prefix}/header/received"),
                "Received".into(),
                28.0,
            ));
            header.push(words(
                format!("{prefix}/header/speed"),
                "Speed".into(),
                28.0,
            ));
        }
        children.push(columns(&format!("{prefix}/header"), header, width, 28.0));
        height += 28.0;
        let mut rows = app.repositories.iter().collect::<Vec<_>>();
        rows.sort_by(|a, b| compare(a, b, view.repository_sort, view.repository_reverse));
        for repo in rows {
            let identity = identity(&repo.destination);
            let key = format!("{prefix}/repo/{identity}");
            let open = view.repository_open.contains(&identity);
            let usability = if repo.usable {
                "Usable"
            } else if repo.placed {
                "Placed"
            } else {
                "Unconfirmed"
            };
            let progress = repo.percent.map_or_else(|| "—".into(), |p| format!("{p}%"));
            let (status, status_height) = super::page::words_node(
                &format!("{key}/status"),
                &format!("{usability} · {}", repo.phase.label()),
                SemanticRole::Text,
                StyleRef::Secondary,
                width * fractions(width)[1],
                24.0,
            );
            let mut cells = vec![
                button(key.clone(), repo.name.clone(), Some(open)),
                status,
                words(format!("{key}/percent"), progress, 28.0),
            ];
            if width >= 640.0 {
                cells.push(words(
                    format!("{key}/received"),
                    repo.received.clone().unwrap_or_else(|| "—".into()),
                    28.0,
                ));
                cells.push(words(
                    format!("{key}/speed"),
                    speed_words(repo, aterm_repository_progress::now_ms()),
                    28.0,
                ));
            }
            let row_height = status_height.max(28.0);
            children.push(columns(&format!("{key}/row"), cells, width, row_height));
            height += row_height;
            if repo.phase.rank() == 1 {
                children.push(bar(
                    format!("{key}/bar"),
                    repo.phase.label().into(),
                    repo.percent.map(|p| (u32::from(p), 100)),
                    motion,
                ));
                height += 8.0;
            }
            if open {
                let now = aterm_repository_progress::now_ms();
                let age = elapsed_seconds(repo, now);
                let mut detail = vec![format!(
                    "Destination: {} · elapsed {}m {}s",
                    repo.destination.display(),
                    age / 60,
                    age % 60
                )];
                if let Some(staging) = &repo.staging {
                    detail.push(format!("Temporary checkout: {}", staging.display()));
                }
                detail.push(format!("Attempt {}", repo.attempt));
                if let Some(outcome) = &repo.outcome {
                    detail.push(format!("Last stage: {}", outcome.stage.label()));
                    detail.push(outcome.reason.label().to_owned());
                    detail.push(outcome.retry.label().to_owned());
                } else if matches!(
                    repo.phase,
                    aterm_repository_progress::Phase::Failed
                        | aterm_repository_progress::Phase::Deferred
                        | aterm_repository_progress::Phase::Interrupted
                ) {
                    detail.push("Outcome detail: not recorded. Retry timing: not recorded.".into());
                }
                detail.extend(
                    repo.previous
                        .iter()
                        .map(|event| format!("Previous attempt: {}", event.phase.label())),
                );
                let recorded = if repo.updated_ms == 0 {
                    "unavailable".to_owned()
                } else {
                    aterm_types::rfc3339::format_rfc3339(repo.updated_ms / 1000)
                };
                detail.push(format!("Recorded at: {recorded}"));
                if !repo.phase.is_finished() {
                    let age = report_age_seconds(repo.updated_ms, now)
                        .map_or_else(|| "age unavailable".to_owned(), |age| format!("{age}s ago"));
                    detail.push(format!("Last report: {age}"));
                }
                detail.extend(repo.history.iter().map(|event| {
                    format!(
                        "+{}s  {}",
                        event.at_ms.saturating_sub(repo.started_ms) / 1000,
                        event.phase.label()
                    )
                }));
                for (i, text) in detail.into_iter().enumerate() {
                    let (node, h) = super::page::words_node(
                        &format!("{key}/detail/{i}"),
                        &text,
                        SemanticRole::Text,
                        StyleRef::Secondary,
                        width,
                        24.0,
                    );
                    children.push(node);
                    height += h;
                }
            }
        }
    }
    (group(prefix, children, height, false), height)
}

fn fractions(width: f32) -> &'static [f32] {
    if width >= 640.0 {
        &[0.23, 0.35, 0.12, 0.15, 0.15]
    } else {
        &[0.3, 0.5, 0.2]
    }
}

fn columns(key: &str, mut cells: Vec<UiNode>, width: f32, height: f32) -> UiNode {
    for (cell, &fraction) in cells.iter_mut().zip(fractions(width)) {
        cell.layout.width = Length::Fraction(fraction);
    }
    group(key, cells, height, true)
}

/// Collect table controls in content coordinates for keyboard scroll reveal.
pub(super) fn controls(node: &UiNode, top: f32, out: &mut Vec<(String, f32, f32)>) {
    if matches!(node.content, UiContent::Button(_)) {
        out.push((node.key.as_str().into(), top, 28.0));
    }
    let mut y = top;
    for child in &node.children {
        controls(child, y, out);
        if node.layout.flow == crate::native_ui::Flow::Column
            && let Length::Fixed(height) = child.layout.height
        {
            y += height;
        }
    }
}

/// Paint a restrained horizontal progress track; absent percentages have an open segment.
pub(crate) fn paint(
    prims: &mut Vec<crate::widget::DrawPrim>,
    rect: crate::native_ui::LogicalRect,
    progress: Option<(u32, u32)>,
    frame: Option<u64>,
    roles: &crate::settings::Roles,
) {
    if matches!(progress, Some((0, _))) {
        prims.push(crate::widget::DrawPrim::Stroke {
            x: rect.x + 0.5,
            y: rect.y + 2.5,
            w: (rect.width - 1.0).max(0.0),
            h: 3.0,
            radius: 1.5,
            width: 1.0,
            color: crate::widget::rgba(roles.text_secondary, 255),
        });
        return;
    }
    let frac = progress
        .filter(|(_, total)| *total > 0)
        .map_or(0.0, |(done, total)| {
            (f64::from(done) / f64::from(total)).clamp(0.0, 1.0) as f32
        });
    prims.push(crate::widget::DrawPrim::Capsule {
        x: rect.x,
        y: rect.y + 2.0,
        w: rect.width,
        h: 4.0,
        frac,
        fill: crate::widget::rgba(roles.text_secondary, 255),
        track: crate::widget::rgba(roles.text_secondary, 45),
    });
    if progress.is_none() {
        let place = frame.map_or(0.5, |f| ((f % 24) as f32 / 12.0 - 1.0).abs());
        prims.push(crate::widget::DrawPrim::Panel {
            x: rect.x + rect.width * 0.75 * place,
            y: rect.y + 2.0,
            w: rect.width * 0.25,
            h: 4.0,
            radius: 2.0,
            fill: crate::widget::rgba(roles.text_secondary, 255),
        });
    }
}

#[cfg(test)]
#[path = "repositories_tests.rs"]
mod tests;
