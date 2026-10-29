// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure network-cell projection and drawing in the System Resources band.

use aterm_types::network_state::{
    HistoryState, MachineIdentity, NetworkStateSnapshot, PeerObservation, Reachability,
    ServiceAvailability, ServiceState, Visibility,
};
use aterm_vitals::ink::Rgb;

use crate::stats_deck::DeckGeom;
use crate::stats_paint::{Hit, HitKind, Painter, panel};
use crate::widget::{DrawPrim, rgba};

/// Fixed headroom, independent of the number or state of peers.
pub(crate) const HEIGHT_PT: f64 = 30.0;

pub(crate) fn inactive_snapshot() -> std::sync::Arc<NetworkStateSnapshot> {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Unavailable;
    snapshot.network.history = HistoryState::Unavailable;
    snapshot.network.detail = "Network observer is not active for this instance".into();
    std::sync::Arc::new(snapshot)
}

fn current_peer_reachability(peer: &PeerObservation, now_ms: Option<u64>) -> Reachability {
    if peer.reachability != Reachability::Reachable {
        return peer.reachability;
    }
    let ages = now_ms.and_then(|now| {
        let checked = peer.last_checked_ms?;
        if peer.last_seen_ms == 0 || checked == 0 {
            return None;
        }
        Some((
            now.checked_sub(peer.last_seen_ms)?,
            now.checked_sub(checked)?,
        ))
    });
    match ages {
        Some((seen, checked)) if seen <= 15_000 && checked <= 15_000 => Reachability::Reachable,
        Some(_) => Reachability::LastSeen,
        None => Reachability::Unknown,
    }
}

/// Derived peer expiry does not advance the source revision.
pub(crate) fn hash_peer_reachability(
    snapshot: &NetworkStateSnapshot,
    state: &mut impl std::hash::Hasher,
) {
    use std::hash::Hash;
    snapshot.network.peers.len().hash(state);
    for peer in &snapshot.network.peers {
        peer.slot.hash(state);
        std::mem::discriminant(&peer.reachability).hash(state);
    }
}

/// Expire current evidence outside painting, retaining the journal and its timestamps.
pub(crate) fn fresh_snapshot(
    snapshot: std::sync::Arc<NetworkStateSnapshot>,
    now_ms: Option<u64>,
) -> std::sync::Arc<NetworkStateSnapshot> {
    let recent = |observed, age| {
        now_ms.is_some_and(|now| observed != 0 && observed <= now && now - observed <= age)
    };
    let current_network_claim = match snapshot.network.visibility {
        Visibility::Active | Visibility::Offline | Visibility::Disabled | Visibility::Waiting => {
            true
        }
        Visibility::Checking => snapshot.network.observed_at_ms != 0,
        Visibility::Unavailable => false,
    };
    let network_stale = current_network_claim && !recent(snapshot.network.observed_at_ms, 15_000);
    let check_peers = !network_stale && snapshot.network.visibility == Visibility::Active;
    let peers_stale = check_peers
        && snapshot
            .network
            .peers
            .iter()
            .any(|peer| current_peer_reachability(peer, now_ms) != peer.reachability);
    // Service reads run independently. Past one minute, their availability
    // is retained evidence, not a current access claim.
    let service_stale = |service: &ServiceState| {
        service.availability != ServiceAvailability::Unknown
            && !recent(service.observed_at_ms, 60_000)
    };
    let accounts_stale = service_stale(&snapshot.services.accounts);
    let secrets_stale = service_stale(&snapshot.services.secrets);
    if !network_stale && !peers_stale && !accounts_stale && !secrets_stale {
        return snapshot;
    }
    let mut expired = snapshot.as_ref().clone();
    if network_stale {
        expired.network.visibility = Visibility::Unavailable;
        expired.network.detail = format!(
            "Current observation expired or its clock is unavailable; retained history remains. {}",
            expired.network.detail
        );
    }
    if check_peers {
        for peer in &mut expired.network.peers {
            peer.reachability = current_peer_reachability(peer, now_ms);
        }
    }
    for (service, stale) in [
        (&mut expired.services.accounts, accounts_stale),
        (&mut expired.services.secrets, secrets_stale),
    ] {
        if stale {
            service.detail = format!(
                "This service observation is no longer current. Last observed {:?}: {}",
                service.availability, service.detail
            );
            service.availability = ServiceAvailability::Unknown;
        }
    }
    std::sync::Arc::new(expired)
}

impl crate::App {
    pub(crate) fn network_state_deadline(&self) -> Option<std::time::Instant> {
        self.network_source.as_ref()?;
        let deck_shown = self.stats_bars_form() == crate::stats_deck::StatsBars::Deck;
        self.windows
            .values()
            .any(|window| {
                !window.occluded
                    && ((deck_shown && crate::stats_deck::stats_drawn_on(window))
                        || window.stats_lens.open.is_some_and(|lens| {
                            lens.focus == crate::stats_paint::Instrument::NetworkState
                        }))
            })
            .then(|| {
                self.network_next_poll
                    .unwrap_or_else(std::time::Instant::now)
            })
    }

    pub(crate) fn service_network_state(&mut self, now: std::time::Instant) {
        if self.network_next_poll.is_some_and(|due| now < due) {
            return;
        }
        self.network_next_poll = Some(now + std::time::Duration::from_secs(1));
        let Some(read) = self.network_source.as_ref() else {
            return;
        };
        let snapshot = fresh_snapshot(read(), aterm_time::unix_millis());
        if snapshot.as_ref() == self.network_snapshot.as_ref() {
            return;
        }
        self.network_snapshot = snapshot;
        let deck_shown = self.stats_bars_form() == crate::stats_deck::StatsBars::Deck;
        let windows: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(id, window)| {
                ((deck_shown && crate::stats_deck::stats_drawn_on(window))
                    || window.stats_lens.open.is_some_and(|lens| {
                        lens.focus == crate::stats_paint::Instrument::NetworkState
                    }))
                .then_some(*id)
            })
            .collect();
        for window in windows {
            self.request_window_redraw(window);
        }
    }
}

/// A stable cell selection; a slot is never the peer's current vector index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    #[default]
    Overview,
    Local,
    Peer(u64),
    Unverified,
    Accounts,
    Secrets,
}

pub(crate) fn height_px(scale: f64) -> usize {
    (HEIGHT_PT * scale).round().max(0.0) as usize
}

pub(crate) fn machine_name(identity: &MachineIdentity) -> String {
    identity.label.clone().unwrap_or_else(|| {
        let id = &identity.machine_id;
        format!(
            "machine {:02x}{:02x}{:02x}{:02x}",
            id[0], id[1], id[2], id[3]
        )
    })
}

fn observation_time(at_ms: u64) -> String {
    if at_ms == 0 {
        "Unknown".into()
    } else if at_ms / 1000 > 253_402_300_799 {
        // RFC3339's last four-digit year is 9999. Retained input outside
        // that range must not overflow the calendar formatter.
        "Invalid timestamp".into()
    } else {
        aterm_types::rfc3339::format_rfc3339(at_ms / 1000)
    }
}

pub(crate) fn summary(snapshot: &NetworkStateSnapshot) -> String {
    let mut text = status_summary(snapshot);
    if snapshot.network.visibility != Visibility::Active && !snapshot.network.peers.is_empty() {
        text.push_str(&format!(" · {} known", snapshot.network.peers.len()));
    }
    if snapshot.network.retention_limited {
        text.push_str(" · partial history");
    }
    text
}

fn status_summary(snapshot: &NetworkStateSnapshot) -> String {
    let network = &snapshot.network;
    if network.visibility == Visibility::Unavailable {
        return "Network visibility unavailable".into();
    }
    if network.history == HistoryState::Unavailable {
        return "Network history unavailable".into();
    }
    if network.history == HistoryState::Restoring {
        return "Restoring network history".into();
    }
    let reachable = network
        .peers
        .iter()
        .filter(|p| p.reachability == Reachability::Reachable)
        .count();
    match network.visibility {
        Visibility::Active => format!(
            "{reachable} reachable · {} known{}",
            network.peers.len(),
            if network.via_local_process.is_some() {
                " · via local aterm"
            } else {
                ""
            },
        ),
        Visibility::Checking => "Checking network".into(),
        Visibility::Offline => "Network offline · history retained".into(),
        Visibility::Disabled => "Network visibility off".into(),
        Visibility::Waiting => "Waiting for network observer".into(),
        Visibility::Unavailable => "Network visibility unavailable".into(),
    }
}

pub(crate) fn targets(snapshot: &NetworkStateSnapshot) -> Vec<Target> {
    let mut targets = vec![Target::Overview, Target::Local];
    let mut peers: Vec<_> = snapshot.network.peers.iter().collect();
    peers.sort_by_key(|p| p.slot);
    targets.extend(peers.into_iter().map(|p| Target::Peer(p.slot)));
    if snapshot.network.unverified_endpoints > 0 {
        targets.push(Target::Unverified);
    }
    targets.extend([Target::Accounts, Target::Secrets]);
    targets
}

pub(crate) fn step(snapshot: &NetworkStateSnapshot, target: Target, forward: bool) -> Target {
    let all = targets(snapshot);
    let index = all.iter().position(|value| *value == target).unwrap_or(0);
    all[if forward {
        (index + 1) % all.len()
    } else {
        (index + all.len() - 1) % all.len()
    }]
}

fn observed_reachability(visibility: Visibility, reachability: Reachability) -> Reachability {
    if visibility != Visibility::Active && reachability == Reachability::Reachable {
        Reachability::Unknown
    } else {
        reachability
    }
}

pub(crate) fn details(snapshot: &NetworkStateSnapshot, target: Target) -> Vec<(String, String)> {
    let network = &snapshot.network;
    let observer_first = target == Target::Overview
        && (network.visibility != Visibility::Active || network.history != HistoryState::Ready)
        && !network.detail.is_empty();
    let mut rows = Vec::new();
    if observer_first {
        // The overview must explain a blocked observation before optional
        // service metadata consumes the available panel height.
        rows.push(("Observer".into(), network.detail.clone()));
    }
    rows.push(("Network".into(), summary(snapshot)));
    if target == Target::Overview {
        rows.insert(
            1,
            (
                "Observation time".into(),
                observation_time(network.observed_at_ms),
            ),
        );
    }
    if network.retention_limited {
        rows.push((
            "History coverage".into(),
            "Older samples or peers omitted".into(),
        ));
    }
    let context_rows = rows.len();
    match target {
        Target::Peer(slot) => {
            if let Some(peer) = network.peers.iter().find(|p| p.slot == slot) {
                rows.push(("Machine".into(), machine_name(&peer.identity)));
                rows.push((
                    "Observation".into(),
                    format!(
                        "{:?}; last seen {}",
                        observed_reachability(network.visibility, peer.reachability),
                        observation_time(peer.last_seen_ms)
                    ),
                ));
                rows.push((
                    "Instances".into(),
                    peer.instances.map_or_else(
                        || "Count not verified".into(),
                        |n| format!("{n} last observed; not a current process guarantee"),
                    ),
                ));
                rows.push((
                    "History".into(),
                    format!(
                        "{} retained observations; unknown intervals are not outages",
                        peer.samples.len()
                    ),
                ));
            } else {
                rows.push((
                    "Machine".into(),
                    "Selected history is no longer retained".into(),
                ));
            }
        }
        Target::Local => rows.push((
            "This machine".into(),
            network
                .observer
                .as_ref()
                .map_or_else(|| "Identity not established".into(), machine_name),
        )),
        Target::Unverified => rows.push((
            "Discovered".into(),
            format!(
                "{} unverified endpoints; no machine identity or data access proved",
                network.unverified_endpoints
            ),
        )),
        Target::Accounts => rows.extend(account_service_details(&snapshot.services.accounts)),
        Target::Secrets => rows.extend(service_details("Secrets", &snapshot.services.secrets)),
        Target::Overview => {
            rows.push((
                "History".into(),
                format!(
                    "{} known machines; {:?}",
                    network.peers.len(),
                    network.history
                ),
            ));
            rows.extend(account_service_details(&snapshot.services.accounts));
            rows.extend(service_details("Secrets", &snapshot.services.secrets));
        }
    }
    if matches!(target, Target::Accounts | Target::Secrets) {
        // The selected service is the first detail the lens must keep when
        // its height is constrained. General network context comes after it.
        rows.rotate_left(context_rows);
    }
    if !observer_first && !network.detail.is_empty() {
        rows.push(("Observer".into(), network.detail.clone()));
    }
    if let Some(pid) = network.via_local_process {
        rows.push((
            "Observation source".into(),
            format!("Read from local aterm process {pid}; original observation times retained"),
        ));
    }
    rows
}

fn account_service_details(service: &ServiceState) -> Vec<(String, String)> {
    let mut rows = service_details("Commons account catalog", service);
    rows.insert(1, (
        "Scope".into(),
        "Shared account proposals in Commons. Harness rosters and provider sign-in are tracked separately.".into(),
    ));
    rows
}

fn service_details(name: &str, service: &ServiceState) -> Vec<(String, String)> {
    let mut text = format!("{:?}", service.availability);
    if service.refresh_failed {
        text.push_str(
            if matches!(
                service.availability,
                ServiceAvailability::Cached | ServiceAvailability::Current
            ) {
                "; refresh failed, local availability retained"
            } else {
                "; refresh failed"
            },
        );
    }
    if !service.detail.is_empty() {
        text.push_str("; ");
        text.push_str(&service.detail);
    }
    let mut rows = vec![(name.into(), text)];
    if service.observed_at_ms > 0 {
        rows.push((
            format!("{name} checked"),
            observation_time(service.observed_at_ms),
        ));
    }
    if let Some(last_success) = service.last_success_ms {
        rows.push((
            format!("{name} last success"),
            observation_time(last_success),
        ));
    }
    if let Some(revision) = &service.revision {
        rows.push((format!("{name} revision"), revision.clone()));
    }
    rows
}

/// Append two fixed rows with the existing resource painter and theme inks.
pub(crate) fn paint_with_inks(
    g: &DeckGeom,
    top: f32,
    snapshot: &NetworkStateSnapshot,
    inks: &aterm_vitals::ink::VitalsInks,
    dwc: bool,
) -> (Vec<DrawPrim>, Vec<Hit>) {
    let scale = g.scale.max(0.5);
    let mut p = Painter::new(inks, scale, g.font_px, dwc);
    let height = height_px(f64::from(scale)) as f32;
    p.prims.push(DrawPrim::ClipPush {
        x: 0.0,
        y: top,
        w: g.w,
        h: height,
    });
    p.prims
        .push(panel(0.0, top, g.w, height, 0.0, rgba(inks.ground, 255)));
    p.prims
        .push(panel(0.0, top, g.w, 1.0, 0.0, rgba(inks.grid, 255)));
    let left = 12.0 * scale;
    let cell_x = left + 88.0 * scale;
    let first_y = top + 7.0 * scale;
    let second_y = top + 21.0 * scale;
    p.ui_text(
        left,
        first_y + 3.0 * scale,
        "Network State".into(),
        inks.words2,
        true,
    );
    p.ui_text(
        left,
        second_y + 3.0 * scale,
        "Shared State".into(),
        inks.words2,
        true,
    );
    let mut hits = vec![Hit {
        rect: (0.0, top, g.w, height),
        kind: HitKind::Network(Target::Overview),
    }];
    let mut draw = |p: &mut Painter<'_>,
                    x: f32,
                    y: f32,
                    state: ServiceAvailability,
                    color: Rgb,
                    target: Target,
                    warning: bool| {
        let side = 8.0 * scale;
        if target == Target::Unverified {
            let points = [
                (x, y),
                (x + side / 2.0, y - side / 2.0),
                (x + side, y),
                (x + side / 2.0, y + side / 2.0),
                (x, y),
            ];
            for pair in points.windows(2) {
                p.prims.push(DrawPrim::Line {
                    x1: pair[0].0,
                    y1: pair[0].1,
                    x2: pair[1].0,
                    y2: pair[1].1,
                    width: scale.max(1.0),
                    color: rgba(color, 255),
                });
            }
        } else {
            p.prims.push(DrawPrim::Stroke {
                x,
                y: y - side / 2.0,
                w: side,
                h: side,
                radius: 0.0,
                width: scale.max(1.0),
                color: rgba(color, 255),
            });
        }
        if target == Target::Local {
            if state != ServiceAvailability::Unknown {
                p.prims.push(panel(
                    x + side / 3.0,
                    y - side / 6.0,
                    side / 3.0,
                    side / 3.0,
                    0.0,
                    rgba(color, 255),
                ));
            }
        } else {
            match state {
                ServiceAvailability::Current | ServiceAvailability::Cached => p.prims.push(panel(
                    x,
                    y - side / 2.0,
                    if state == ServiceAvailability::Cached {
                        side / 2.0
                    } else {
                        side
                    },
                    side,
                    0.0,
                    rgba(color, 255),
                )),
                ServiceAvailability::Unavailable => {
                    p.ui_text(x, y + 3.0 * scale, "×".into(), color, false)
                }
                ServiceAvailability::Unknown => {}
            }
        }
        if warning {
            p.prims.push(panel(
                x + side,
                y - side / 2.0,
                2.0 * scale,
                2.0 * scale,
                0.0,
                rgba(inks.over, 255),
            ));
        }
        hits.insert(
            0,
            Hit {
                rect: (x - 3.0 * scale, y - 6.0 * scale, 14.0 * scale, 12.0 * scale),
                kind: HitKind::Network(target),
            },
        );
    };
    draw(
        &mut p,
        cell_x,
        first_y,
        if snapshot.network.observer.is_some() {
            ServiceAvailability::Cached
        } else {
            ServiceAvailability::Unknown
        },
        inks.aterm,
        Target::Local,
        snapshot.network.retention_limited,
    );
    let available = (g.w - cell_x - 95.0 * scale).max(0.0);
    let slots = (available / (14.0 * scale)) as usize;
    let mut peers: Vec<_> = snapshot.network.peers.iter().collect();
    peers.sort_by_key(|peer| peer.slot);
    for (index, peer) in peers.iter().take(slots).enumerate() {
        let reachability = observed_reachability(snapshot.network.visibility, peer.reachability);
        let state = match reachability {
            Reachability::Reachable => ServiceAvailability::Current,
            Reachability::LastSeen | Reachability::Unknown => ServiceAvailability::Unknown,
        };
        draw(
            &mut p,
            cell_x + (24.0 + index as f32 * 14.0) * scale,
            first_y,
            state,
            inks.machine,
            Target::Peer(peer.slot),
            reachability == Reachability::Unknown,
        );
    }
    let next_x = cell_x + (24.0 + peers.len().min(slots) as f32 * 14.0) * scale;
    if peers.len() > slots {
        p.ui_text(
            next_x,
            first_y + 3.0 * scale,
            format!("+{}", peers.len() - slots),
            inks.words2,
            false,
        );
    } else if snapshot.network.unverified_endpoints > 0 {
        draw(
            &mut p,
            next_x,
            first_y,
            ServiceAvailability::Unknown,
            inks.hollow,
            Target::Unverified,
            true,
        );
    }
    if next_x + 90.0 * scale < g.w {
        let mut text = match snapshot.network.visibility {
            Visibility::Active => format!(
                "{} reachable",
                snapshot
                    .network
                    .peers
                    .iter()
                    .filter(|p| p.reachability == Reachability::Reachable)
                    .count()
            ),
            Visibility::Checking => "Checking".into(),
            Visibility::Offline => "Offline".into(),
            Visibility::Disabled => "Off".into(),
            Visibility::Waiting => "Waiting".into(),
            Visibility::Unavailable => "Unavailable".into(),
        };
        if snapshot.network.visibility == Visibility::Active
            && snapshot.network.via_local_process.is_some()
        {
            let full = format!("{text} · via local aterm");
            let room = g.w - next_x - 20.0 * scale;
            text = if p.ui_text_w(&full, false) <= room {
                full
            } else {
                "via aterm".into()
            };
        }
        if snapshot.network.visibility != Visibility::Active && !peers.is_empty() {
            let known = format!("{text} · {} known", peers.len());
            if p.ui_text_w(&known, false) <= g.w - next_x - 20.0 * scale {
                text = known;
            }
        }
        if snapshot.network.retention_limited {
            let partial = format!("{text} · partial history");
            if p.ui_text_w(&partial, false) <= g.w - next_x - 20.0 * scale {
                text = partial;
            }
        }
        p.ui_text(
            next_x + 20.0 * scale,
            first_y + 3.0 * scale,
            text,
            inks.words2,
            false,
        );
    }
    let gap = 7.0 * scale;
    let accounts_cell_x = cell_x + p.ui_text_w("Accounts", false) + gap;
    let secrets_x = accounts_cell_x + 8.0 * scale + 2.0 * gap;
    let secrets_cell_x = secrets_x + p.ui_text_w("Secrets", false) + gap;
    p.ui_text(
        cell_x,
        second_y + 3.0 * scale,
        "Accounts".into(),
        inks.words2,
        false,
    );
    draw(
        &mut p,
        accounts_cell_x,
        second_y,
        snapshot.services.accounts.availability,
        inks.aterm,
        Target::Accounts,
        snapshot.services.accounts.refresh_failed,
    );
    p.ui_text(
        secrets_x,
        second_y + 3.0 * scale,
        "Secrets".into(),
        inks.words2,
        false,
    );
    draw(
        &mut p,
        secrets_cell_x,
        second_y,
        snapshot.services.secrets.availability,
        inks.aterm,
        Target::Secrets,
        snapshot.services.secrets.refresh_failed,
    );
    p.prims.push(DrawPrim::ClipPop);
    (p.prims, hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_types::network_state::NetworkObservation;

    fn peer(slot: u64, reachability: Reachability) -> PeerObservation {
        PeerObservation {
            slot,
            identity: MachineIdentity {
                machine_id: [slot as u8; 16],
                incarnation: [1; 16],
                label: Some(format!("m{slot}")),
            },
            first_seen_ms: 1,
            last_seen_ms: 1000,
            last_checked_ms: Some(2000),
            reachability,
            instances: None,
            samples: Vec::new(),
        }
    }

    #[test]
    fn waiting_network_keeps_remembered_count_visible_without_claiming_reachability() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Waiting;
        snapshot.network.history = HistoryState::Ready;
        snapshot
            .network
            .peers
            .push(peer(2, Reachability::Reachable));
        let g = DeckGeom {
            w: 700.0,
            h: 200.0,
            pad: 0.0,
            scale: 1.0,
            font_px: 14.0,
        };
        let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
        let (prims, _) = paint_with_inks(&g, 170.0, &snapshot, &inks, false);
        assert!(prims.iter().any(|prim| matches!(prim,
            DrawPrim::Text { s, .. } if s == "Waiting · 1 known"
        )));
        let summary = summary(&snapshot);
        assert!(summary.contains("1 known"));
        assert!(!summary.contains("reachable"));
    }

    #[test]
    fn reachable_peer_expires_before_a_still_fresh_observer() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.history = HistoryState::Ready;
        snapshot.network.observed_at_ms = 10_000;
        let mut remote = peer(2, Reachability::Reachable);
        remote.last_seen_ms = 2_000;
        remote.last_checked_ms = Some(2_000);
        snapshot.network.peers.push(remote);
        let snapshot = std::sync::Arc::new(snapshot);
        let expired = fresh_snapshot(snapshot.clone(), Some(17_001));
        assert_eq!(expired.network.visibility, Visibility::Active);
        assert_eq!(
            expired.network.peers[0].reachability,
            Reachability::LastSeen
        );
        assert_eq!(
            expired.network.observed_at_ms,
            snapshot.network.observed_at_ms
        );
        assert_eq!(expired.network.peers[0].last_seen_ms, 2_000);
        assert_eq!(expired.network.peers[0].last_checked_ms, Some(2_000));
        assert_eq!(
            expired.network.peers[0].samples,
            snapshot.network.peers[0].samples
        );
        assert_eq!(expired.network.revision, snapshot.network.revision);
        assert_eq!(expired.revision, snapshot.revision);
    }

    #[test]
    fn peer_contact_and_check_clocks_each_bound_current_reachability() {
        for (seen, checked, expected) in [
            (2_001, Some(2_001), Reachability::Reachable),
            (2_000, Some(2_001), Reachability::LastSeen),
            (2_001, Some(2_000), Reachability::LastSeen),
            (0, Some(2_001), Reachability::Unknown),
            (2_001, None, Reachability::Unknown),
            (2_001, Some(0), Reachability::Unknown),
            (17_002, Some(2_001), Reachability::Unknown),
            (2_001, Some(17_002), Reachability::Unknown),
        ] {
            let mut snapshot = NetworkStateSnapshot::default();
            snapshot.network.visibility = Visibility::Active;
            snapshot.network.observed_at_ms = 10_000;
            let mut remote = peer(2, Reachability::Reachable);
            remote.last_seen_ms = seen;
            remote.last_checked_ms = checked;
            snapshot.network.peers.push(remote.clone());
            let projected = fresh_snapshot(std::sync::Arc::new(snapshot), Some(17_001));
            remote.reachability = expected;
            assert_eq!(projected.network.peers, vec![remote]);
        }
    }

    #[test]
    fn observer_refresh_does_not_refresh_retained_peer_evidence() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.observed_at_ms = 10_000;
        let mut remote = peer(2, Reachability::Reachable);
        remote.last_seen_ms = 2_000;
        remote.last_checked_ms = Some(2_000);
        snapshot.network.peers.push(remote);
        let first = fresh_snapshot(std::sync::Arc::new(snapshot.clone()), Some(17_001));
        assert_eq!(first.network.peers[0].reachability, Reachability::LastSeen);
        snapshot.network.observed_at_ms = 18_000;
        let refreshed = fresh_snapshot(std::sync::Arc::new(snapshot.clone()), Some(18_000));
        assert_eq!(
            refreshed.network.peers[0].reachability,
            Reachability::LastSeen
        );
        snapshot.network.peers[0].last_seen_ms = 18_000;
        snapshot.network.peers[0].last_checked_ms = Some(18_000);
        let contacted = fresh_snapshot(std::sync::Arc::new(snapshot), Some(18_000));
        assert_eq!(
            contacted.network.peers[0].reachability,
            Reachability::Reachable
        );
        let mut retained = first.as_ref().clone();
        retained.network.observed_at_ms = 18_000;
        retained.network.peers[0].last_seen_ms = 18_000;
        retained.network.peers[0].last_checked_ms = Some(18_000);
        assert_eq!(
            fresh_snapshot(std::sync::Arc::new(retained), Some(18_000))
                .network
                .peers[0]
                .reachability,
            Reachability::LastSeen
        );
    }

    #[test]
    fn peer_expiry_invalidates_an_open_network_lens_without_a_source_revision() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.observed_at_ms = 10_000;
        let mut remote = peer(2, Reachability::Reachable);
        remote.last_seen_ms = 2_000;
        remote.last_checked_ms = Some(2_000);
        snapshot.network.peers.push(remote);
        app.network_snapshot = std::sync::Arc::new(snapshot);
        assert!(app.open_network_lens());
        let before = app.stats_lens_fp(wid);
        assert_ne!(before, 0);
        app.network_snapshot = fresh_snapshot(app.network_snapshot.clone(), Some(17_001));
        assert_eq!(app.network_snapshot.revision, 0);
        assert_eq!(app.network_snapshot.network.revision, 0);
        assert_eq!(app.network_snapshot.network.visibility, Visibility::Active);
        assert_ne!(app.stats_lens_fp(wid), before);
    }

    #[test]
    fn observation_time_changes_the_lens_fingerprint() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Waiting;
        snapshot.network.observed_at_ms = 1_000;
        app.network_snapshot = std::sync::Arc::new(snapshot);
        assert!(app.open_network_lens());
        let before = app.stats_lens_fp(wid);
        let mut updated = app.network_snapshot.as_ref().clone();
        updated.network.observed_at_ms = 2_000;
        app.network_snapshot = std::sync::Arc::new(updated);
        assert_eq!(app.network_snapshot.revision, 0);
        assert_ne!(app.stats_lens_fp(wid), before);
    }

    #[test]
    fn retention_limit_changes_visible_coverage_without_moving_cells() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.history = HistoryState::Ready;
        snapshot.network.observer = Some(peer(1, Reachability::Reachable).identity);
        snapshot
            .network
            .peers
            .push(peer(2, Reachability::Reachable));
        let g = DeckGeom {
            w: 700.0,
            h: 200.0,
            pad: 0.0,
            scale: 1.0,
            font_px: 14.0,
        };
        let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
        let before = paint_with_inks(&g, 170.0, &snapshot, &inks, false);
        let before_summary = summary(&snapshot);
        let before_details = details(&snapshot, Target::Overview);
        snapshot.network.retention_limited = true;
        let limited = paint_with_inks(&g, 170.0, &snapshot, &inks, false);
        assert_ne!(summary(&snapshot), before_summary);
        assert_ne!(
            crate::stats_deck::prims_fp(&limited.0),
            crate::stats_deck::prims_fp(&before.0)
        );
        assert_eq!(limited.1, before.1);
        assert!(details(&snapshot, Target::Overview).len() > before_details.len());
        assert_eq!(snapshot.network.peers.len(), 1);
        assert_eq!(
            snapshot.network.peers[0].reachability,
            Reachability::Reachable
        );
    }

    #[test]
    fn retention_limit_invalidates_network_details_without_a_revision_change() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        assert!(app.open_network_lens());
        let before = app.stats_lens_fp(wid);
        let mut snapshot = app.network_snapshot.as_ref().clone();
        snapshot.network.retention_limited = true;
        app.network_snapshot = std::sync::Arc::new(snapshot);
        assert_ne!(app.stats_lens_fp(wid), before);
    }

    #[test]
    fn peer_slots_survive_status_and_input_order_changes() {
        let mut snapshot = NetworkStateSnapshot {
            network: NetworkObservation {
                visibility: Visibility::Active,
                history: HistoryState::Ready,
                peers: vec![
                    peer(7, Reachability::Reachable),
                    peer(2, Reachability::LastSeen),
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let g = DeckGeom {
            w: 700.0,
            h: 200.0,
            pad: 0.0,
            scale: 1.0,
            font_px: 14.0,
        };
        let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
        let first = paint_with_inks(&g, 170.0, &snapshot, &inks, false).1;
        snapshot.network.peers.reverse();
        snapshot.network.peers[0].reachability = Reachability::Unknown;
        let second = paint_with_inks(&g, 170.0, &snapshot, &inks, false).1;
        assert_eq!(first, second);
        snapshot.network.via_local_process = Some(42);
        let (relayed, relayed_hits) = paint_with_inks(&g, 170.0, &snapshot, &inks, false);
        assert_eq!(second, relayed_hits);
        assert!(relayed.iter().any(|prim| {
            matches!(prim, DrawPrim::Text { s, face, .. }
                if s.contains("via local aterm") && *face != crate::widget::TextFace::Mono)
        }));
        assert_eq!(step(&snapshot, Target::Local, true), Target::Peer(2));
    }

    #[test]
    fn captions_use_resource_faces_and_service_cells_clear_their_labels() {
        for scale in [1.0, 2.0] {
            let g = DeckGeom {
                w: 700.0 * scale,
                h: 200.0 * scale,
                pad: 0.0,
                scale,
                font_px: 14.0 * scale,
            };
            let inks =
                crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
            let (prims, hits) =
                paint_with_inks(&g, 170.0 * scale, &inactive_snapshot(), &inks, false);
            for prim in &prims {
                if let DrawPrim::Text { face, px, .. } = prim {
                    assert_ne!(*face, crate::widget::TextFace::Mono);
                    assert_eq!(*px, 9.5 * scale);
                }
            }
            for (label, target) in [("Accounts", Target::Accounts), ("Secrets", Target::Secrets)] {
                let end = prims
                    .iter()
                    .find_map(|prim| match prim {
                        DrawPrim::Text { x, s, px, face, .. } if s == label => {
                            Some(x + crate::tray_raster::ui_text_width_for(*face, s, *px).ceil())
                        }
                        _ => None,
                    })
                    .expect("service label");
                let hit = hits
                    .iter()
                    .find(|hit| hit.kind == HitKind::Network(target))
                    .expect("service cell");
                assert!(hit.rect.0 >= end + 4.0 * scale);
            }
        }
    }
    #[test]
    fn retained_contact_and_service_times_do_not_depend_on_the_observer_clock() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Unavailable;
        snapshot.network.peers.push(peer(2, Reachability::LastSeen));
        snapshot.services.secrets.observed_at_ms = 3000;
        snapshot.services.secrets.last_success_ms = Some(2000);
        let contact = details(&snapshot, Target::Peer(2));
        let service = details(&snapshot, Target::Secrets);
        assert!(
            contact
                .iter()
                .any(|(_, value)| value.contains("1970-01-01T00:00:01Z"))
        );
        assert!(
            service
                .iter()
                .any(|(_, value)| value == "1970-01-01T00:00:03Z")
        );
        assert!(
            service
                .iter()
                .any(|(_, value)| value == "1970-01-01T00:00:02Z")
        );
        for observer_time in [1000, 10_000, 1_000_000] {
            snapshot.network.observed_at_ms = observer_time;
            assert_eq!(details(&snapshot, Target::Peer(2)), contact);
            assert_eq!(details(&snapshot, Target::Secrets), service);
        }
        assert_eq!(observation_time(0), "Unknown");
        assert_eq!(observation_time(u64::MAX), "Invalid timestamp");
        assert_eq!(
            observation_time(253_402_300_799_000),
            "9999-12-31T23:59:59Z"
        );
    }

    #[test]
    fn failed_refresh_preserves_available_cache_and_does_not_claim_peers() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.history = HistoryState::Ready;
        snapshot.network.unverified_endpoints = 3;
        snapshot.services.secrets.availability = ServiceAvailability::Cached;
        snapshot.services.secrets.refresh_failed = true;
        let failed_details = details(&snapshot, Target::Secrets);
        snapshot.services.secrets.refresh_failed = false;
        assert_ne!(failed_details, details(&snapshot, Target::Secrets));
        assert_eq!(
            snapshot.services.secrets.availability,
            ServiceAvailability::Cached
        );
        assert!(
            !targets(&snapshot)
                .iter()
                .any(|t| matches!(t, Target::Peer(_)))
        );
    }
    #[test]
    fn unread_history_is_not_an_empty_network() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.history = HistoryState::Ready;
        let empty_network = summary(&snapshot);
        snapshot.network.history = HistoryState::Unavailable;
        assert_ne!(summary(&snapshot), empty_network);
    }

    #[test]
    fn stale_reachable_peer_requires_an_active_observer() {
        for visibility in [
            Visibility::Checking,
            Visibility::Offline,
            Visibility::Disabled,
            Visibility::Waiting,
            Visibility::Unavailable,
        ] {
            assert_eq!(
                observed_reachability(visibility, Reachability::Reachable),
                Reachability::Unknown
            );
        }
        assert_eq!(
            observed_reachability(Visibility::Active, Reachability::Reachable),
            Reachability::Reachable
        );
    }

    #[test]
    fn stalled_or_unusable_clock_expires_current_evidence_but_keeps_history() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.observed_at_ms = 10_000;
        let mut remote = peer(2, Reachability::Reachable);
        remote.last_seen_ms = 10_000;
        remote.last_checked_ms = Some(10_000);
        snapshot.network.peers.push(remote);
        let snapshot = std::sync::Arc::new(snapshot);
        assert!(std::sync::Arc::ptr_eq(
            &fresh_snapshot(snapshot.clone(), Some(25_000)),
            &snapshot
        ));
        for now in [Some(25_001), Some(9_999), None] {
            let expired = fresh_snapshot(snapshot.clone(), now);
            assert_eq!(expired.network.visibility, Visibility::Unavailable);
            assert_eq!(expired.network.peers, snapshot.network.peers);
            assert_eq!(expired.network.observed_at_ms, 10_000);
        }
    }

    #[test]
    fn direct_visibility_claims_expire_without_losing_history_or_repeated_expiry() {
        for visibility in [
            Visibility::Offline,
            Visibility::Disabled,
            Visibility::Waiting,
            Visibility::Checking,
        ] {
            let mut snapshot = NetworkStateSnapshot::default();
            snapshot.network.visibility = visibility;
            snapshot.network.observed_at_ms = 100_000;
            snapshot.network.history = HistoryState::Ready;
            snapshot.network.peers.push(peer(2, Reachability::LastSeen));
            let snapshot = std::sync::Arc::new(snapshot);
            assert!(snapshot.network.via_local_process.is_none());
            assert!(std::sync::Arc::ptr_eq(
                &fresh_snapshot(snapshot.clone(), Some(115_000)),
                &snapshot,
            ));
            for now in [Some(115_001), Some(99_999), None] {
                let expired = fresh_snapshot(snapshot.clone(), now);
                assert_eq!(expired.network.visibility, Visibility::Unavailable);
                assert_eq!(
                    expired.network.observed_at_ms,
                    snapshot.network.observed_at_ms
                );
                assert_eq!(expired.network.history, snapshot.network.history);
                assert_eq!(expired.network.peers, snapshot.network.peers);
                assert!(std::sync::Arc::ptr_eq(
                    &fresh_snapshot(expired.clone(), now),
                    &expired
                ));
            }
        }
    }

    #[test]
    fn initial_checking_and_unavailable_history_do_not_expire_again() {
        for (visibility, observed_at_ms, source) in [
            (Visibility::Checking, 0, None),
            (Visibility::Unavailable, 100_000, None),
            (Visibility::Unavailable, 100_000, Some(41)),
        ] {
            let mut snapshot = NetworkStateSnapshot::default();
            snapshot.network.visibility = visibility;
            snapshot.network.observed_at_ms = observed_at_ms;
            snapshot.network.via_local_process = source;
            let snapshot = std::sync::Arc::new(snapshot);
            for now in [Some(115_001), None] {
                assert!(std::sync::Arc::ptr_eq(
                    &fresh_snapshot(snapshot.clone(), now),
                    &snapshot
                ));
            }
        }
    }

    #[test]
    fn service_freshness_is_independent_of_network_and_other_services() {
        for visibility in [Visibility::Active, Visibility::Offline, Visibility::Waiting] {
            let mut snapshot = NetworkStateSnapshot::default();
            snapshot.network.visibility = visibility;
            snapshot.network.observed_at_ms = 100_000;
            snapshot.services.accounts = ServiceState {
                availability: ServiceAvailability::Cached,
                observed_at_ms: 40_000,
                last_success_ms: Some(35_000),
                revision: Some("retained revision".into()),
                refresh_failed: true,
                ..Default::default()
            };
            snapshot.services.secrets = ServiceState {
                availability: ServiceAvailability::Cached,
                observed_at_ms: 100_000,
                ..Default::default()
            };
            let snapshot = std::sync::Arc::new(snapshot);
            assert!(std::sync::Arc::ptr_eq(
                &fresh_snapshot(snapshot.clone(), Some(100_000)),
                &snapshot,
            ));
            let expired = fresh_snapshot(snapshot.clone(), Some(100_001));
            assert_eq!(expired.network, snapshot.network);
            assert_eq!(expired.services.secrets, snapshot.services.secrets);
            assert_eq!(
                expired.services.accounts.availability,
                ServiceAvailability::Unknown
            );
            assert_eq!(expired.services.accounts.observed_at_ms, 40_000);
            assert_eq!(expired.services.accounts.last_success_ms, Some(35_000));
            assert_eq!(
                expired.services.accounts.revision,
                snapshot.services.accounts.revision
            );
            assert!(expired.services.accounts.refresh_failed);
            assert_eq!(
                snapshot.services.accounts.availability,
                ServiceAvailability::Cached
            );
            assert!(std::sync::Arc::ptr_eq(
                &fresh_snapshot(expired.clone(), Some(100_001)),
                &expired,
            ));
        }
    }

    #[test]
    fn visible_network_lens_keeps_its_poll_deadline_with_resource_bars_hidden() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        let snapshot = inactive_snapshot();
        app.network_source = Some(std::sync::Arc::new(move || snapshot.clone()));
        let due = std::time::Instant::now() + std::time::Duration::from_secs(1);
        app.network_next_poll = Some(due);
        assert_eq!(app.network_state_deadline(), None);
        assert!(app.open_network_lens());
        assert_eq!(app.network_state_deadline(), Some(due));
        app.windows.get_mut(&wid).unwrap().occluded = true;
        assert_eq!(app.network_state_deadline(), None);
        app.windows.get_mut(&wid).unwrap().occluded = false;
        assert_eq!(app.network_state_deadline(), Some(due));
        app.close_stats_lens(wid);
        assert_eq!(app.network_state_deadline(), None);
        assert!(app.open_network_lens());
        app.network_source = None;
        assert_eq!(app.network_state_deadline(), None);
    }

    #[test]
    fn service_expiry_invalidates_an_open_network_lens() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Offline;
        snapshot.network.observed_at_ms = 61_001;
        snapshot.services.secrets.availability = ServiceAvailability::Cached;
        snapshot.services.secrets.observed_at_ms = 1000;
        app.network_snapshot = std::sync::Arc::new(snapshot);
        assert!(app.open_network_lens());
        let before = app.stats_lens_fp(wid);
        assert_ne!(before, 0);
        app.network_snapshot = fresh_snapshot(app.network_snapshot.clone(), Some(61_001));
        assert_eq!(app.network_snapshot.revision, 0);
        assert_eq!(app.network_snapshot.network.visibility, Visibility::Offline);
        assert_ne!(app.stats_lens_fp(wid), before);
    }

    #[test]
    fn invalid_service_clocks_never_establish_current_access() {
        for availability in [
            ServiceAvailability::Cached,
            ServiceAvailability::Current,
            ServiceAvailability::Unavailable,
        ] {
            for (at, now) in [
                (0, Some(100_000)),
                (100_001, Some(100_000)),
                (100_000, None),
            ] {
                let mut snapshot = NetworkStateSnapshot::default();
                snapshot.services.secrets.availability = availability;
                snapshot.services.secrets.observed_at_ms = at;
                let snapshot = std::sync::Arc::new(snapshot);
                let expired = fresh_snapshot(snapshot.clone(), now);
                assert_eq!(
                    expired.services.secrets.availability,
                    ServiceAvailability::Unknown
                );
                assert_eq!(expired.services.secrets.observed_at_ms, at);
                assert_eq!(snapshot.services.secrets.availability, availability);
            }
        }
    }

    #[test]
    fn relayed_observations_name_their_source_without_refreshing_expired_evidence() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.history = HistoryState::Ready;
        snapshot.network.observed_at_ms = 100_000;
        snapshot
            .network
            .peers
            .push(peer(2, Reachability::Reachable));
        let direct = summary(&snapshot);
        snapshot.network.via_local_process = Some(41);
        assert_ne!(summary(&snapshot), direct);
        assert!(
            details(&snapshot, Target::Overview)
                .iter()
                .any(|(key, value)| { key == "Observation source" && value.contains("41") })
        );
        let expired = fresh_snapshot(std::sync::Arc::new(snapshot), Some(115_001));
        assert_eq!(expired.network.visibility, Visibility::Unavailable);
        assert_eq!(expired.network.observed_at_ms, 100_000);
        assert_eq!(expired.network.via_local_process, Some(41));
        assert_eq!(
            observed_reachability(
                expired.network.visibility,
                expired.network.peers[0].reachability
            ),
            Reachability::Unknown
        );
        let mut offline = expired.as_ref().clone();
        offline.network.visibility = Visibility::Offline;
        let expired_offline = fresh_snapshot(std::sync::Arc::new(offline), Some(115_001));
        assert_eq!(expired_offline.network.visibility, Visibility::Unavailable);
        assert_eq!(expired_offline.network.observed_at_ms, 100_000);
    }

    #[test]
    fn absent_observer_does_not_promise_a_pending_read() {
        let snapshot = inactive_snapshot();
        assert_eq!(snapshot.network.visibility, Visibility::Unavailable);
        assert_eq!(snapshot.network.history, HistoryState::Unavailable);
        assert!(!snapshot.network.detail.is_empty());
    }

    #[test]
    fn changing_the_relay_source_invalidates_network_details() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        assert!(app.open_network_lens());
        let before = app.stats_lens_fp(wid);
        let mut snapshot = app.network_snapshot.as_ref().clone();
        snapshot.network.via_local_process = Some(42);
        app.network_snapshot = std::sync::Arc::new(snapshot);
        assert_ne!(app.stats_lens_fp(wid), before);
    }

    #[test]
    fn explicit_network_closeup_keeps_hidden_deck_and_terminal_geometry() {
        let mut app = crate::App::headless_for_test();
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("off".into());
        let wid = *app.windows.keys().next().expect("window");
        let before = (
            app.windows[&wid].rows,
            app.windows[&wid].cols,
            app.windows[&wid].metrics.head,
        );
        assert!(app.open_network_lens());
        assert!(app.lens_frame(wid).is_some());
        assert_eq!(app.stats_bars_form(), crate::stats_deck::StatsBars::Off);
        assert_eq!(
            (
                app.windows[&wid].rows,
                app.windows[&wid].cols,
                app.windows[&wid].metrics.head
            ),
            before
        );
    }
}
