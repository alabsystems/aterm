// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native, read-only lab observations beside Claude's own account and recovery facts.

use super::harness_page::{metadata_cards, row_cards};
use super::*;
use crate::lab_screen::LabState;
use crate::native_app::HarnessRequest;
use crate::network_deck::Target;
use aterm_types::network_state::{ServiceAvailability, ServiceState};

pub(super) const LAB_ACTION_PREFIX: &str = "lab/";
pub(super) const LAB_ACCOUNTS_REFRESH: &str = "lab/accounts/refresh";

/// The action of account row `index`'s vault token offer (`lab/account/<index>/token`).
pub(super) fn token_action(index: usize) -> String {
    format!("lab/account/{index}/token")
}

/// The account row a [`token_action`] names.
fn token_index(action: &str) -> Option<usize> {
    action
        .strip_prefix("lab/account/")?
        .strip_suffix("/token")?
        .parse()
        .ok()
}

pub(super) fn lab_commands(lab: &LabState) -> Vec<Command> {
    vec![Command {
        id: ActionId::new(LAB_ACCOUNTS_REFRESH),
        title: "Lab: Refresh Claude accounts".into(),
        shortcut: None,
        enabled: !lab.collecting,
    }]
}

impl SettingsApp {
    pub(super) fn reduce_lab_action(
        &mut self,
        _view: &mut SettingsViewState,
        action: &str,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        if action == LAB_ACCOUNTS_REFRESH {
            if !self.harness.lab.collecting {
                cx.harness(HarnessRequest::LabAccountsRefresh);
            }
        } else if let Some(index) = token_index(action) {
            if self
                .harness
                .lab
                .accounts
                .rows
                .get(index)
                .is_some_and(|row| row.offer.is_some())
            {
                cx.harness(HarnessRequest::LabToken { index });
            }
        } else {
            return EventResult::Bubble;
        }
        cx.repaint(crate::native_app::DamageRegion::All);
        EventResult::Handled
    }
}

fn observed_time(at: Option<u64>) -> String {
    at.filter(|at| *at > 0 && at / 1000 <= 253_402_300_799)
        .map_or_else(
            || "Not observed".into(),
            |at| aterm_types::rfc3339::format_rfc3339(at / 1000),
        )
}

fn currency(service: &ServiceState) -> String {
    match service.availability {
        ServiceAvailability::Unknown => "Synchronization not established".into(),
        ServiceAvailability::Unavailable => {
            "Local access unavailable; synchronization not established".into()
        }
        ServiceAvailability::Cached => "Usable local cache; remote currency not established".into(),
        ServiceAvailability::Current => {
            "Current only within the service adapter's observed scope".into()
        }
    }
}

fn button(key: &str, label: &str, action: ActionId, width: f32, enabled: bool) -> (UiNode, f32) {
    let (words, lines) = button_words(label, &[], width);
    let height = crate::native_ui::native_button_height_for_lines(lines, scaled_control_height());
    (
        UiNode::new(
            key,
            UiContent::Button(
                Control::new(ButtonSpec::new(label).visual_label(words), action)
                    .state(ControlState {
                        enabled,
                        ..ControlState::default()
                    })
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
}

pub(super) fn lab_page(
    state: &SettingsViewState,
    lab: &LabState,
    width: SettingsWidth,
    cx: &ViewCx<'_>,
) -> Vec<UiNode> {
    let gap = settings_page_gap(state, width, cx.viewport);
    let page_width =
        settings_page_content_width(cx.viewport.width, width, PageLayout::Form.maximum(width));
    let (mut heading, heading_height) = pinned_heading(
        state,
        width,
        cx.viewport,
        gap,
        "Lab shared state",
        "Observed network, shared data and Claude accounts.",
    );
    let room = page_list_room(state, width, cx.viewport, heading_height);
    let inner = card_text_width(page_width);
    let facts = |key: &str, caption: (&str, &str), rows: &[(String, String)]| {
        metadata_cards(
            key,
            caption,
            rows,
            page_width,
            fact_label_width(rows, inner),
        )
    };
    let mut cards = Vec::new();
    let mut network = crate::network_deck::details(&lab.network, Target::Local);
    network.push((
        "Observation time".into(),
        observed_time(Some(lab.network.network.observed_at_ms)),
    ));
    network.push((
        "History".into(),
        format!(
            "{} known machines; {:?}",
            lab.network.network.peers.len(),
            lab.network.network.history
        ),
    ));
    cards.extend(facts("lab/network", ("THIS ATERM", "THIS ATERM"), &network));

    let actions = [
        button(
            "lab/harness",
            "Harness settings",
            route_action(SettingsRoute::Harness),
            inner,
            true,
        ),
        button(
            "lab/ai",
            "AI CLI sign-in status",
            route_action(SettingsRoute::Ai),
            inner,
            true,
        ),
    ]
    .into_iter()
    .collect();
    cards.extend(row_cards(
        "lab/actions",
        ("Lab navigation", ("LAB NAVIGATION", "NAVIGATION")),
        actions,
        room,
        page_width,
    ));

    for (key, caption, target, service) in [
        (
            "accounts",
            "SHARED ACCOUNT CATALOG",
            Target::Accounts,
            &lab.network.services.accounts,
        ),
        (
            "secrets",
            "SECRETS MANAGER",
            Target::Secrets,
            &lab.network.services.secrets,
        ),
    ] {
        let mut rows = crate::network_deck::details(&lab.network, target);
        rows.retain(|(label, _)| label != "Network");
        rows.push(("Currency".into(), currency(service)));
        cards.extend(facts(
            &format!("lab/service/{key}"),
            (caption, caption),
            &rows,
        ));
    }

    let inventory = vec![
        ("Account records".into(), if lab.accounts.status.is_empty() { "Account records have not been observed".into() } else { lab.accounts.status.clone() }),
        ("Observed".into(), observed_time(lab.accounts.observed_at_ms)),
        ("Scope".into(), "Claude's local roster and retained observations; catalog access and secret availability do not prove a provider login or remaining quota".into()),
    ];
    let mut inventory = facts(
        "lab/inventory",
        ("CLAUDE ACCOUNTS", "CLAUDE ACCOUNTS"),
        &inventory,
    );
    if let Some((card, height)) = inventory.last_mut() {
        let (refresh, refresh_height) = button(
            LAB_ACCOUNTS_REFRESH,
            if lab.collecting {
                "Reading Claude accounts…"
            } else {
                "Refresh Claude accounts"
            },
            ActionId::new(LAB_ACCOUNTS_REFRESH),
            inner,
            !lab.collecting,
        );
        card.children.push(refresh);
        *height += refresh_height + 4.0;
        card.layout.height = Length::Fixed(*height);
    }
    cards.extend(inventory);
    for (index, account) in lab.accounts.rows.iter().enumerate() {
        let mut rows = vec![("Account".into(), account.label.clone())];
        rows.extend(account.facts.iter().cloned());
        let mut group = facts(
            &format!("lab/account/{index}"),
            ("CLAUDE ACCOUNT", "CLAUDE ACCOUNT"),
            &rows,
        );
        // The quiet offer's one step: a tab beside the person's that runs the verb (§5.9).
        if let (Some(offer), Some((card, height))) = (&account.offer, group.last_mut()) {
            let action = token_action(index);
            let (step, step_height) = button(
                &action,
                if offer.replace {
                    "Replace vault token"
                } else {
                    "Make vault token"
                },
                ActionId::new(action.as_str()),
                inner,
                true,
            );
            card.children.push(step);
            *height += step_height + 4.0;
            card.layout.height = Length::Fixed(*height);
        }
        cards.extend(group);
    }

    for session in &lab.sessions {
        let key = format!("lab/session/{}", session.local_id);
        let mut rows = vec![("Session".into(), session.title.clone())];
        rows.extend(session.facts.iter().cloned());
        let mut group = facts(&key, ("SESSION RECOVERY", "SESSION RECOVERY"), &rows);
        if let Some((card, height)) = group.last_mut() {
            let (show, show_height) = button(
                &format!("{key}/show"),
                "Show session",
                ActionId::new(super::harness_page::show_action(session.local_id)),
                inner,
                true,
            );
            card.children.push(show);
            *height += show_height + 4.0;
            card.layout.height = Length::Fixed(*height);
        }
        cards.extend(group);
    }
    if lab.sessions.is_empty() {
        cards.extend(facts("lab/recovery", ("SESSION RECOVERY", "SESSION RECOVERY"), &[
            ("Sessions".into(), "No local Claude session recovery observation is available".into()),
            ("Scope".into(), "Sign-in activity observed here is process-local; work in another aterm or machine is not established".into()),
        ]));
    }
    for peer in &lab.network.network.peers {
        let mut rows = crate::network_deck::details(
            &lab.network,
            Target::Seat(crate::network_deck::SeatKey::Observed(peer.slot)),
        );
        rows.retain(|(label, _)| label != "Network");
        cards.extend(facts(
            &format!("lab/peer/{}", peer.slot),
            ("KNOWN MACHINE", "KNOWN MACHINE"),
            &rows,
        ));
    }
    heading.push(scroll::list_page_peeking(
        state,
        "settings/list",
        "Lab shared state",
        cards
            .into_iter()
            .map(|(node, height)| scroll::ListItem::of(node, height))
            .collect(),
        room,
        gap,
        settings_list_overhang(state, width, cx.viewport),
    ));
    heading
}

#[cfg(test)]
#[path = "native_settings_lab_tests.rs"]
mod tests;
