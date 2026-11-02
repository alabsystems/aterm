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

mod repair;

/// Fixed headroom, independent of the number or state of peers.
pub(crate) const HEIGHT_PT: f64 = 30.0;

/// Rendered long and short place captions, captured together before a frame is keyed.
pub(crate) type PlaceCaption = (String, String);

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

/// A busy composition read supplies no new evidence. Keep known records and their
/// original times, but withhold current claims until the producer can be read again.
/// The producer's next publication applies its original monotonic expiry lease.
pub(crate) fn snapshot_after_read(
    publication: Option<std::sync::Arc<NetworkStateSnapshot>>,
    retained: &std::sync::Arc<NetworkStateSnapshot>,
    busy: &mut bool,
    now_ms: Option<u64>,
) -> std::sync::Arc<NetworkStateSnapshot> {
    if let Some(publication) = publication {
        *busy = false;
        return fresh_snapshot(publication, now_ms);
    }
    if *busy {
        return retained.clone();
    }
    *busy = true;
    let mut snapshot = retained.as_ref().clone();
    snapshot.network.visibility = Visibility::Unavailable;
    snapshot.network.detail = if snapshot.network.detail.is_empty() {
        "Network State reader is busy; current reachability is unknown".into()
    } else {
        format!(
            "Network State reader is busy; current reachability is unknown. Last observation: {}",
            snapshot.network.detail
        )
    };
    for peer in &mut snapshot.network.peers {
        peer.reachability = Reachability::Unknown;
    }
    for service in [
        &mut snapshot.services.accounts,
        &mut snapshot.services.secrets,
    ] {
        service.detail = if service.availability == ServiceAvailability::Unknown
            && service.detail.is_empty()
        {
            "Network State reader is busy; current access is unknown".into()
        } else {
            format!(
                "Network State reader is busy; current access is unknown. Last observed {:?}: {}",
                service.availability, service.detail
            )
        };
        service.availability = ServiceAvailability::Unknown;
    }
    std::sync::Arc::new(snapshot)
}

impl crate::App {
    pub(crate) fn network_state_deadline(&self) -> Option<std::time::Instant> {
        self.network_source.as_ref()?;
        if self.lab_page_shown() {
            return Some(
                self.network_next_poll
                    .unwrap_or_else(std::time::Instant::now),
            );
        }
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
        let snapshot = snapshot_after_read(
            read(),
            &self.network_snapshot,
            &mut self.network_source_busy,
            aterm_time::unix_millis(),
        );
        if snapshot.as_ref() == self.network_snapshot.as_ref() {
            return;
        }
        self.network_snapshot = snapshot;
        if self.lab_page_shown() {
            self.publish_native_lab_state();
        }
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

/// The band's words for this Mac's network place where it is off the ALab LAN (another network,
/// offline: [`aterm_netplace::words::off_the_alab_lan`]), long and short
/// ([`aterm_netplace::words::band`]); `None` on the ALab LAN, while checking, with no fleet to
/// learn a home from, or before a place was read, where the observer's words (Commons' reachable
/// count among them) stand.
pub(crate) fn place_caption(place: Option<&aterm_netplace::Place>) -> Option<PlaceCaption> {
    place
        .filter(|place| aterm_netplace::words::off_the_alab_lan(place))
        .map(aterm_netplace::words::band)
}

/// Capture only the visible place words; paint never reads the changing place cache.
pub(crate) fn capture_place_caption() -> Option<PlaceCaption> {
    place_caption(crate::net_place::cached().as_ref())
}

/// A test summary with the place as last read, without starting a read.
#[cfg(test)]
pub(crate) fn summary(snapshot: &NetworkStateSnapshot) -> String {
    summary_with_caption(snapshot, capture_place_caption().as_ref())
}

/// [`summary`] with this Mac's network place `place`: away from the ALab LAN, the place's words
/// lead ([`place_caption`]).
#[cfg(test)]
pub(crate) fn summary_at(
    snapshot: &NetworkStateSnapshot,
    place: Option<&aterm_netplace::Place>,
) -> String {
    summary_with_caption(snapshot, place_caption(place).as_ref())
}

pub(crate) fn summary_with_caption(
    snapshot: &NetworkStateSnapshot,
    caption: Option<&PlaceCaption>,
) -> String {
    let mut text = status_summary(snapshot);
    if network_status(snapshot) != NetworkStatus::Visibility(Visibility::Active)
        && !snapshot.network.peers.is_empty()
    {
        text.push_str(&format!(" · {} known", snapshot.network.peers.len()));
    }
    if let Some((long, _)) = caption {
        text.clone_from(long);
    }
    if snapshot.network.retention_limited {
        text.push_str(" · partial history");
    }
    text
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NetworkStatus {
    Visibility(Visibility),
    HistoryUnavailable,
    HistoryRestoring,
}

fn network_status(snapshot: &NetworkStateSnapshot) -> NetworkStatus {
    let network = &snapshot.network;
    if network.visibility == Visibility::Unavailable {
        NetworkStatus::Visibility(Visibility::Unavailable)
    } else {
        match network.history {
            HistoryState::Unavailable => NetworkStatus::HistoryUnavailable,
            HistoryState::Restoring => NetworkStatus::HistoryRestoring,
            HistoryState::Ready => NetworkStatus::Visibility(network.visibility),
        }
    }
}

fn status_summary(snapshot: &NetworkStateSnapshot) -> String {
    let network = &snapshot.network;
    let reachable = network
        .peers
        .iter()
        .filter(|p| p.reachability == Reachability::Reachable)
        .count();
    match network_status(snapshot) {
        NetworkStatus::HistoryUnavailable => "Network history unavailable".into(),
        NetworkStatus::HistoryRestoring => "Restoring network history".into(),
        NetworkStatus::Visibility(Visibility::Active) => format!(
            "{reachable} reachable · {} known{}",
            network.peers.len(),
            if network.via_local_process.is_some() {
                " · via local aterm"
            } else {
                ""
            },
        ),
        NetworkStatus::Visibility(Visibility::Checking) => "Checking network".into(),
        NetworkStatus::Visibility(Visibility::Offline) => {
            "Network offline · history retained".into()
        }
        NetworkStatus::Visibility(Visibility::Disabled) => "Network visibility off".into(),
        NetworkStatus::Visibility(Visibility::Waiting) => "Waiting for network observer".into(),
        NetworkStatus::Visibility(Visibility::Unavailable) => {
            "Network visibility unavailable".into()
        }
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
    details_with_caption(snapshot, target, capture_place_caption().as_ref())
}

pub(crate) fn details_with_caption(
    snapshot: &NetworkStateSnapshot,
    target: Target,
    caption: Option<&PlaceCaption>,
) -> Vec<(String, String)> {
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
    rows.push(("Network".into(), summary_with_caption(snapshot, caption)));
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
                rows.extend(repair::details(peer.last_repair.as_ref()));
                rows.push((
                    "Last outbound".into(),
                    peer.last_authenticated_endpoint.as_ref().map_or_else(
                        || "Not observed; older or inbound-only history".into(),
                        |endpoint| {
                            format!(
                                "{} · authenticated {} (historical)",
                                endpoint.address,
                                observation_time(endpoint.observed_at_ms)
                            )
                        },
                    ),
                ));
                rows.push((
                    "Machine UUID".into(),
                    aterm_codec::hex::encode_lower(&peer.identity.machine_id),
                ));
                rows.push((
                    "Incarnation".into(),
                    aterm_codec::hex::encode_lower(&peer.identity.incarnation),
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
        Target::Accounts => rows.extend(account_service_details(snapshot)),
        Target::Secrets => rows.extend(secret_service_details(&snapshot.services.secrets)),
        Target::Overview => {
            rows.push((
                "History".into(),
                format!(
                    "{} known machines; {:?}",
                    network.peers.len(),
                    network.history
                ),
            ));
            rows.extend(account_service_details(snapshot));
            rows.extend(secret_service_details(&snapshot.services.secrets));
        }
    }
    if matches!(target, Target::Peer(_) | Target::Accounts | Target::Secrets) {
        // Keep the selected peer's evidence or service status before general
        // network context when the lens has limited height.
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

fn account_service_details(snapshot: &NetworkStateSnapshot) -> Vec<(String, String)> {
    let mut rows = service_details("Commons account catalog", &snapshot.services.accounts);
    rows.insert(1, (
        "Scope".into(),
        "Shared account proposals in Commons. Harness rosters and provider sign-in are tracked separately.".into(),
    ));
    match &snapshot.network.accounts_exchange {
        Some(exchange) => {
            rows.push((
                "Peer inventory".into(),
                format!(
                    "{} retained operation IDs reported matching by {} during an authenticated exchange",
                    exchange.inventory_count,
                    machine_name(&exchange.peer),
                ),
            ));
            rows.push((
                "Peer inventory sampled".into(),
                observation_time(exchange.sampled_at_ms),
            ));
            rows.push((
                "Inventory scope".into(),
                "Historical account-proposal IDs only. Current contents, conflict resolution, credentials, provider sign-in and quotas are not established by this observation.".into(),
            ));
        }
        None => rows.push((
            "Peer inventory".into(),
            "No retained observation of an authenticated matching account-proposal inventory"
                .into(),
        )),
    }
    rows
}

fn secret_service_details(service: &ServiceState) -> Vec<(String, String)> {
    let mut rows = service_details("Secrets", service);
    if let Some(refresh) = service.network_refresh {
        let outcome = match refresh.outcome {
            aterm_types::network_state::VaultNetworkRefreshOutcome::Succeeded => "Completed",
            aterm_types::network_state::VaultNetworkRefreshOutcome::Failed => "Failed",
        };
        rows.insert(1, ("Recorded network refresh".into(), outcome.into()));
        rows.insert(
            2,
            (
                "Refresh completed".into(),
                observation_time(refresh.completed_at_ms),
            ),
        );
        rows.insert(3, (
            "Refresh scope".into(),
            "Last retained outcome for this source. Cache availability and provider sign-in are separate.".into(),
        ));
    }
    match service.network_materialization {
        Some(receipt) => {
            let route = match receipt.source {
                aterm_types::network_state::VaultNetworkSource::ConfiguredUrl => "configured route",
                aterm_types::network_state::VaultNetworkSource::PeerUrl => "peer route",
            };
            rows.push((
                "Network sync".into(),
                format!(
                    "Signed vault version {} received via {route}; local vault checks passed",
                    receipt.sequence,
                ),
            ));
            rows.push((
                "Network fetch started".into(),
                observation_time(receipt.sampled_at_ms),
            ));
            rows.push((
                "Network sync scope".into(),
                "Historical receipt for the observed local version. Newer data elsewhere, other machines' state and provider sign-in are not established.".into(),
            ));
        }
        None => rows.push((
            "Network sync".into(),
            "No verified network receipt is available in this observation".into(),
        )),
    }
    match service.http204_response {
        Some(response) => {
            let route = match response.source {
                aterm_types::network_state::VaultNetworkSource::ConfiguredUrl => "Configured route",
                aterm_types::network_state::VaultNetworkSource::PeerUrl => "Peer route",
            };
            rows.push((
                "Retained HTTP 204".into(),
                format!("{route} answered HTTP 204"),
            ));
            rows.push((
                "Response received".into(),
                observation_time(response.responded_at_ms),
            ));
            rows.push((
                "Response scope".into(),
                "Historical unsigned HTTP status only. Vault contents, local access, newer data elsewhere and provider sign-in are not established.".into(),
            ));
        }
        None => rows.push((
            "Retained HTTP 204".into(),
            "No retained HTTP 204 response is available in this observation".into(),
        )),
    }
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
    caption: Option<&PlaceCaption>,
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
        snapshot.network.retention_limited || snapshot.network.history == HistoryState::Unavailable,
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
        let status = network_status(snapshot);
        let room = g.w - next_x - 20.0 * scale;
        let mut text = match status {
            NetworkStatus::HistoryUnavailable => "History unavailable".into(),
            NetworkStatus::HistoryRestoring => "Restoring history".into(),
            NetworkStatus::Visibility(Visibility::Active) => format!(
                "{} reachable",
                snapshot
                    .network
                    .peers
                    .iter()
                    .filter(|p| p.reachability == Reachability::Reachable)
                    .count()
            ),
            NetworkStatus::Visibility(Visibility::Checking) => "Checking".into(),
            NetworkStatus::Visibility(Visibility::Offline) => "Offline".into(),
            NetworkStatus::Visibility(Visibility::Disabled) => "Off".into(),
            NetworkStatus::Visibility(Visibility::Waiting) => "Waiting".into(),
            NetworkStatus::Visibility(Visibility::Unavailable) => "Unavailable".into(),
        };
        if p.ui_text_w(&text, false) > room {
            text = match status {
                NetworkStatus::HistoryUnavailable => "History error".into(),
                NetworkStatus::HistoryRestoring => "Restoring".into(),
                _ => text,
            };
        }
        if status == NetworkStatus::Visibility(Visibility::Active)
            && snapshot.network.via_local_process.is_some()
        {
            let full = format!("{text} · via local aterm");
            text = if p.ui_text_w(&full, false) <= room {
                full
            } else {
                "via aterm".into()
            };
        }
        if status != NetworkStatus::Visibility(Visibility::Active) && !peers.is_empty() {
            let known = format!("{text} · {} known", peers.len());
            if p.ui_text_w(&known, false) <= room {
                text = known;
            }
        }
        // Away from the ALab LAN, the frame's captured words explain its place.
        if let Some((long, short)) = caption {
            text = if p.ui_text_w(long, false) <= room {
                long.clone()
            } else {
                short.clone()
            };
        }
        if snapshot.network.retention_limited {
            let partial = format!("{text} · partial history");
            if p.ui_text_w(&partial, false) <= room {
                text = partial;
            }
        }
        if p.ui_text_w(&text, false) <= room {
            p.ui_text(
                next_x + 20.0 * scale,
                first_y + 3.0 * scale,
                text,
                inks.words2,
                false,
            );
        }
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
            last_repair: None,
            last_authenticated_endpoint: None,
            reachability,
            instances: None,
            samples: Vec::new(),
        }
    }

    #[test]
    fn an_initial_busy_read_invents_no_prior_observation() {
        let unread = std::sync::Arc::new(NetworkStateSnapshot::default());
        let mut busy = false;
        let snapshot = snapshot_after_read(None, &unread, &mut busy, Some(100_000));
        assert!(busy);
        assert_eq!(snapshot.revision, 0);
        assert_eq!(snapshot.network.observed_at_ms, 0);
        assert_eq!(snapshot.network.history, HistoryState::Restoring);
        assert_eq!(snapshot.network.visibility, Visibility::Unavailable);
        assert!(snapshot.network.peers.is_empty());
        assert!(snapshot.network.detail.contains("busy"));
        assert!(!snapshot.network.detail.contains("Last observation"));
        assert_eq!(snapshot.services.accounts.observed_at_ms, 0);
        assert_eq!(snapshot.services.secrets.observed_at_ms, 0);
        assert!(!snapshot.services.secrets.detail.contains("Last observed"));
    }

    #[test]
    fn busy_reads_keep_known_history_and_warnings_without_current_claims() {
        use aterm_types::network_state::{
            PresenceSample, VaultNetworkMaterialization, VaultNetworkRefresh,
            VaultNetworkRefreshOutcome, VaultNetworkSource,
        };

        let mut known = NetworkStateSnapshot {
            revision: 9,
            ..Default::default()
        };
        known.network.revision = 4;
        known.network.observed_at_ms = 100_000;
        known.network.visibility = Visibility::Active;
        known.network.history = HistoryState::Ready;
        known.network.detail = "authenticated observation".into();
        let mut machine = peer(7, Reachability::Reachable);
        machine.last_seen_ms = 100_000;
        machine.last_checked_ms = Some(100_000);
        machine.samples.push(PresenceSample {
            at_ms: 100_000,
            state: Reachability::Reachable,
        });
        known.network.peers.push(machine);
        let service = &mut known.services.secrets;
        service.availability = ServiceAvailability::Cached;
        service.observed_at_ms = 100_000;
        service.last_success_ms = Some(99_000);
        service.revision = Some("signed sequence 7".into());
        service.network_materialization = Some(VaultNetworkMaterialization {
            source: VaultNetworkSource::PeerUrl,
            sequence: 7,
            sampled_at_ms: 99_000,
        });
        service.network_refresh = Some(VaultNetworkRefresh {
            completed_at_ms: 99_500,
            outcome: VaultNetworkRefreshOutcome::Failed,
        });
        service.refresh_failed = true;
        service.detail = "Cache usable; latest refresh failed".into();
        let known = std::sync::Arc::new(known);
        let mut busy = false;
        let retained = snapshot_after_read(None, &known, &mut busy, Some(100_001));
        assert!(busy);
        assert_eq!((retained.revision, retained.network.revision), (9, 4));
        assert_eq!(retained.network.visibility, Visibility::Unavailable);
        assert_eq!(retained.network.history, HistoryState::Ready);
        assert_eq!(retained.network.observed_at_ms, 100_000);
        let machine = &retained.network.peers[0];
        assert_eq!(machine.reachability, Reachability::Unknown);
        assert_eq!(machine.identity, known.network.peers[0].identity);
        assert_eq!(machine.samples, known.network.peers[0].samples);
        assert_eq!(
            (
                machine.first_seen_ms,
                machine.last_seen_ms,
                machine.last_checked_ms
            ),
            (1, 100_000, Some(100_000))
        );
        let service = &retained.services.secrets;
        let prior = &known.services.secrets;
        assert_eq!(service.availability, ServiceAvailability::Unknown);
        assert_eq!(service.observed_at_ms, prior.observed_at_ms);
        assert_eq!(service.last_success_ms, prior.last_success_ms);
        assert_eq!(service.revision, prior.revision);
        assert_eq!(
            service.network_materialization,
            prior.network_materialization
        );
        assert_eq!(service.network_refresh, prior.network_refresh);
        assert!(service.refresh_failed);
        assert!(service.detail.contains(&prior.detail));
        assert!(service.detail.contains("busy"));
        assert!(retained.network.detail.contains("busy"));
        for wall in [Some(100_002), Some(99_999), None] {
            let again = snapshot_after_read(None, &retained, &mut busy, wall);
            assert!(std::sync::Arc::ptr_eq(&retained, &again));
        }
        let recovered =
            snapshot_after_read(Some(known.clone()), &retained, &mut busy, Some(100_002));
        assert!(!busy);
        assert!(std::sync::Arc::ptr_eq(&known, &recovered));
        assert_eq!(
            recovered.network.peers[0].reachability,
            Reachability::Reachable
        );
        assert_eq!(
            recovered.services.secrets.availability,
            ServiceAvailability::Cached
        );
    }

    #[test]
    fn the_app_keeps_a_known_peer_when_its_network_reader_is_busy() {
        let mut app = crate::App::headless_for_test();
        let mut known = NetworkStateSnapshot {
            revision: 9,
            ..Default::default()
        };
        known.network.history = HistoryState::Ready;
        known.network.peers.push(peer(7, Reachability::Reachable));
        known.services.accounts.refresh_failed = true;
        app.network_snapshot = std::sync::Arc::new(known);
        app.network_source = Some(std::sync::Arc::new(|| None));
        let now = std::time::Instant::now();
        app.service_network_state(now);
        assert!(app.network_source_busy);
        assert_eq!(app.network_snapshot.revision, 9);
        assert_eq!(app.network_snapshot.network.history, HistoryState::Ready);
        assert_eq!(app.network_snapshot.network.peers[0].slot, 7);
        assert_eq!(
            app.network_snapshot.network.peers[0].reachability,
            Reachability::Unknown
        );
        assert!(app.network_snapshot.services.accounts.refresh_failed);
        let retained = app.network_snapshot.clone();
        app.service_network_state(now + std::time::Duration::from_secs(1));
        assert!(std::sync::Arc::ptr_eq(&retained, &app.network_snapshot));
        assert_eq!(
            app.network_next_poll,
            Some(now + std::time::Duration::from_secs(2))
        );
    }

    fn cell_prims(prims: &[DrawPrim], hits: &[Hit], target: Target) -> Vec<DrawPrim> {
        let rect = hits
            .iter()
            .find(|hit| hit.kind == HitKind::Network(target))
            .expect("cell hit")
            .rect;
        prims
            .iter()
            .filter(|prim| match prim {
                DrawPrim::Panel { x, y, w, h, .. } | DrawPrim::Stroke { x, y, w, h, .. } => {
                    *x >= rect.0
                        && *y >= rect.1
                        && x + w <= rect.0 + rect.2
                        && y + h <= rect.1 + rect.3
                }
                _ => false,
            })
            .cloned()
            .collect()
    }

    fn local_warning(prims: &[DrawPrim], hits: &[Hit], scale: f32, color: Rgb) -> bool {
        cell_prims(prims, hits, Target::Local).iter().any(|prim| {
            matches!(prim, DrawPrim::Panel { w, h, fill, .. }
                if *w == 2.0 * scale && *h == 2.0 * scale && *fill == rgba(color, 255))
        })
    }

    fn compact_caption(prims: &[DrawPrim], top: f32, scale: f32) -> Option<&str> {
        prims.iter().find_map(|prim| match prim {
            DrawPrim::Text { x, baseline, s, .. }
                if *x > 100.0 * scale && *baseline == (top + 10.0 * scale).round() =>
            {
                Some(s.as_str())
            }
            _ => None,
        })
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
        let (prims, _) = paint_with_inks(&g, 170.0, &snapshot, None, &inks, false);
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
        let before = paint_with_inks(&g, 170.0, &snapshot, None, &inks, false);
        let before_summary = summary(&snapshot);
        let before_details = details(&snapshot, Target::Overview);
        snapshot.network.retention_limited = true;
        let limited = paint_with_inks(&g, 170.0, &snapshot, None, &inks, false);
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
        let first = paint_with_inks(&g, 170.0, &snapshot, None, &inks, false).1;
        snapshot.network.peers.reverse();
        snapshot.network.peers[0].reachability = Reachability::Unknown;
        let second = paint_with_inks(&g, 170.0, &snapshot, None, &inks, false).1;
        assert_eq!(first, second);
        snapshot.network.via_local_process = Some(42);
        let (relayed, relayed_hits) = paint_with_inks(&g, 170.0, &snapshot, None, &inks, false);
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
                paint_with_inks(&g, 170.0 * scale, &inactive_snapshot(), None, &inks, false);
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
    fn selected_peer_keeps_verified_identity_and_original_outbound_evidence() {
        use aterm_types::network_state::AuthenticatedEndpoint;

        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.history = HistoryState::Ready;
        let mut remote = peer(2, Reachability::LastSeen);
        remote.identity.label = None;
        remote.identity.machine_id = [0x23; 16];
        remote.identity.incarnation = [0x42; 16];
        remote.last_authenticated_endpoint = Some(AuthenticatedEndpoint {
            address: "[fd00::12]:7414".parse().unwrap(),
            observed_at_ms: 2000,
        });
        snapshot.network.peers.push(remote);

        for (visibility, observer_time) in [
            (Visibility::Active, 3000),
            (Visibility::Offline, 1_000_000),
            (Visibility::Unavailable, 0),
        ] {
            snapshot.network.visibility = visibility;
            snapshot.network.observed_at_ms = observer_time;
            let rows = details(&snapshot, Target::Peer(2));
            assert_eq!(rows[0].0, "Machine");
            let at = |key| rows.iter().position(|(name, _)| name == key).unwrap();
            assert!(at("Observation") < at("Last repair"));
            assert!(at("Last repair") < at("Last outbound"));
            assert!(at("Last outbound") < at("Machine UUID"));
            let value = |key| {
                rows.iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value.as_str())
                    .unwrap()
            };
            assert_eq!(value("Machine UUID"), "23232323232323232323232323232323");
            assert_eq!(value("Incarnation"), "42424242424242424242424242424242");
            assert!(value("Last outbound").contains("[fd00::12]:7414"));
            assert!(value("Last outbound").contains("1970-01-01T00:00:02Z"));
            assert!(value("Last outbound").contains("historical"));
            assert!(value("Observation").contains("1970-01-01T00:00:01Z"));
        }
    }

    #[test]
    fn selected_peer_does_not_infer_an_endpoint_for_a_replaced_incarnation() {
        use aterm_types::network_state::AuthenticatedEndpoint;

        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot
            .network
            .peers
            .push(peer(2, Reachability::Reachable));
        let absent = details(&snapshot, Target::Peer(2))
            .into_iter()
            .find(|(name, _)| name == "Last outbound")
            .unwrap();
        snapshot.network.peers[0].last_authenticated_endpoint = Some(AuthenticatedEndpoint {
            address: "192.0.2.12:7414".parse().unwrap(),
            observed_at_ms: 2000,
        });
        assert!(
            details(&snapshot, Target::Peer(2))
                .iter()
                .any(|(_, value)| value.contains("192.0.2.12:7414"))
        );

        // The observer clears endpoint evidence when a new verified incarnation replaces it.
        snapshot.network.peers[0].identity.incarnation = [0x3a; 16];
        snapshot.network.peers[0].last_authenticated_endpoint = None;
        let replaced = details(&snapshot, Target::Peer(2));
        assert!(replaced.contains(&absent));
        assert!(
            replaced
                .iter()
                .any(|(name, value)| { name == "Incarnation" && value == &"3a".repeat(16) })
        );
        assert!(
            !replaced
                .iter()
                .any(|(_, value)| value.contains("192.0.2.12:7414"))
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
    fn history_failure_is_visible_on_active_and_relayed_compact_strips() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.history = HistoryState::Ready;
        snapshot.network.observer = Some(peer(1, Reachability::Reachable).identity);
        // A failed journal retains known peers but the observer cannot accept contacts.
        snapshot.network.peers.push(peer(2, Reachability::Unknown));
        snapshot.network.unverified_endpoints = 3;
        snapshot.services.accounts.availability = ServiceAvailability::Cached;
        snapshot.services.secrets.availability = ServiceAvailability::Cached;
        let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
        for scale in [1.0, 2.0] {
            let painter = Painter::new(&inks, scale, 14.0 * scale, false);
            let full_width = painter.ui_text_w("History unavailable", false);
            let short_width = painter.ui_text_w("History error", false);
            let caption_x = 158.0 * scale;
            // Caption admission leaves 70 points after the retained peer and offset.
            let constrained_room = short_width.max(70.0 * scale + 1.0);
            assert!(constrained_room < full_width);
            let constrained_width = caption_x + constrained_room;
            let no_caption_width = (caption_x + 70.0 * scale - 1.0).floor();
            assert!(no_caption_width < constrained_width);
            for (width, expect_caption, expect_short) in [
                (no_caption_width, false, false),
                (constrained_width, true, true),
                (360.0 * scale, true, false),
                (700.0 * scale, true, false),
            ] {
                let g = DeckGeom {
                    w: width,
                    h: 200.0 * scale,
                    pad: 0.0,
                    scale,
                    font_px: 14.0 * scale,
                };
                let top = 170.0 * scale;
                for relay in [None, Some(42)] {
                    snapshot.network.via_local_process = relay;
                    snapshot.network.history = HistoryState::Ready;
                    let ready = paint_with_inks(&g, top, &snapshot, None, &inks, false);
                    let ready_summary = summary(&snapshot);
                    assert!(!local_warning(&ready.0, &ready.1, scale, inks.over));
                    snapshot.network.history = HistoryState::Unavailable;
                    let failed = paint_with_inks(&g, top, &snapshot, None, &inks, false);
                    assert!(local_warning(&failed.0, &failed.1, scale, inks.over));
                    assert_eq!(failed.1, ready.1);
                    assert_ne!(summary(&snapshot), ready_summary);
                    for target in [Target::Peer(2), Target::Accounts, Target::Secrets] {
                        assert_eq!(
                            crate::stats_deck::prims_fp(&cell_prims(&failed.0, &failed.1, target)),
                            crate::stats_deck::prims_fp(&cell_prims(&ready.0, &ready.1, target)),
                        );
                    }
                    if expect_caption {
                        let caption =
                            compact_caption(&failed.0, top, scale).expect("history caption");
                        assert!(caption.contains("History"));
                        assert!(!caption.contains("reachable") && !caption.contains("via"));
                        if expect_short {
                            assert!(painter.ui_text_w(caption, false) < full_width);
                            assert!(painter.ui_text_w(caption, false) <= constrained_room);
                        }
                    } else {
                        assert!(compact_caption(&failed.0, top, scale).is_none());
                    }
                    for prim in &failed.0 {
                        if let DrawPrim::Text {
                            x,
                            baseline,
                            s,
                            px,
                            face,
                            ..
                        } = prim
                            && *x > 100.0 * scale
                            && *baseline == (top + 10.0 * scale).round()
                        {
                            assert!(
                                x + crate::tray_raster::ui_text_width_for(*face, s, *px).ceil()
                                    <= g.w
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn history_health_and_partial_coverage_compose_without_changing_cells() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.observer = Some(peer(1, Reachability::Reachable).identity);
        snapshot.network.peers.push(peer(2, Reachability::Unknown));
        snapshot.network.via_local_process = Some(42);
        snapshot.services.accounts.availability = ServiceAvailability::Cached;
        snapshot.services.secrets.availability = ServiceAvailability::Cached;
        let g = DeckGeom {
            w: 700.0,
            h: 200.0,
            pad: 0.0,
            scale: 1.0,
            font_px: 14.0,
        };
        let inks = crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200]);
        for visibility in [
            Visibility::Active,
            Visibility::Checking,
            Visibility::Offline,
            Visibility::Disabled,
            Visibility::Waiting,
            Visibility::Unavailable,
        ] {
            snapshot.network.visibility = visibility;
            snapshot.network.history = HistoryState::Ready;
            snapshot.network.retention_limited = false;
            let ready = paint_with_inks(&g, 170.0, &snapshot, None, &inks, false);
            for history in [HistoryState::Restoring, HistoryState::Unavailable] {
                snapshot.network.history = history;
                for limited in [false, true] {
                    snapshot.network.retention_limited = limited;
                    let painted = paint_with_inks(&g, 170.0, &snapshot, None, &inks, false);
                    assert_eq!(painted.1, ready.1);
                    assert_eq!(
                        local_warning(&painted.0, &painted.1, 1.0, inks.over),
                        limited || history == HistoryState::Unavailable,
                    );
                    let caption = compact_caption(&painted.0, 170.0, 1.0).expect("status caption");
                    assert!(caption.contains("1 known"));
                    assert_eq!(caption.contains("partial history"), limited);
                    assert!(!caption.contains("reachable") && !caption.contains("via"));
                    if visibility == Visibility::Unavailable {
                        assert_eq!(
                            compact_caption(&ready.0, 170.0, 1.0),
                            Some(caption.split(" · partial history").next().unwrap()),
                        );
                    } else {
                        assert_ne!(Some(caption), compact_caption(&ready.0, 170.0, 1.0));
                        assert_eq!(
                            caption.contains("Restoring"),
                            history == HistoryState::Restoring
                        );
                    }
                    for target in [Target::Peer(2), Target::Accounts, Target::Secrets] {
                        assert_eq!(
                            crate::stats_deck::prims_fp(&cell_prims(
                                &painted.0, &painted.1, target
                            )),
                            crate::stats_deck::prims_fp(&cell_prims(&ready.0, &ready.1, target)),
                        );
                    }
                }
            }
        }
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
        app.network_source = Some(std::sync::Arc::new(move || Some(snapshot.clone())));
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
    fn account_inventory_history_survives_expiry_without_promoting_service_access() {
        let mut snapshot = NetworkStateSnapshot::default();
        snapshot.network.visibility = Visibility::Active;
        snapshot.network.observed_at_ms = 100_000;
        snapshot.services.accounts.availability = ServiceAvailability::Cached;
        snapshot.services.accounts.observed_at_ms = 100_000;
        let absent = details(&snapshot, Target::Accounts);
        assert!(absent.iter().any(|(key, value)| {
            key == "Peer inventory" && value.contains("No retained observation")
        }));
        snapshot.network.accounts_exchange =
            Some(aterm_types::network_state::AccountsExchangeObservation {
                scope_digest: [3; 32],
                local_actor: [4; 48],
                control_observation: [5; 64],
                inventory_count: 2,
                inventory_fingerprint: [6; 32],
                peer: MachineIdentity {
                    machine_id: [7; 16],
                    incarnation: [8; 16],
                    label: Some("fixture-peer".into()),
                },
                sampled_at_ms: 90_000,
            });
        let original = snapshot.network.accounts_exchange.clone();
        let expired = fresh_snapshot(std::sync::Arc::new(snapshot), Some(160_001));
        assert_eq!(expired.network.accounts_exchange, original);
        assert_eq!(
            expired.services.accounts.availability,
            ServiceAvailability::Unknown
        );
        assert_eq!(
            expired.services.secrets.availability,
            ServiceAvailability::Unknown
        );
        let rows = details(&expired, Target::Accounts);
        assert!(rows.iter().any(|(key, value)| {
            key == "Peer inventory"
                && value.contains("2 retained operation IDs")
                && value.contains("fixture-peer")
        }));
        assert!(rows.iter().any(|(key, value)| {
            key == "Peer inventory sampled" && value == &observation_time(90_000)
        }));
        assert!(
            rows.iter()
                .any(|(key, value)| { key == "Inventory scope" && value.contains("Historical") })
        );
        assert!(
            !details(&expired, Target::Secrets)
                .iter()
                .any(|(key, _)| key == "Peer inventory")
        );
    }

    #[test]
    fn vault_network_history_keeps_original_time_and_scope_after_local_observation_expires() {
        use aterm_types::network_state::{VaultNetworkMaterialization, VaultNetworkSource};
        for source in [
            VaultNetworkSource::ConfiguredUrl,
            VaultNetworkSource::PeerUrl,
        ] {
            let mut snapshot = NetworkStateSnapshot::default();
            snapshot.services.secrets.availability = ServiceAvailability::Cached;
            snapshot.services.secrets.observed_at_ms = 100_000;
            let absent = details(&snapshot, Target::Secrets);
            assert!(absent.iter().any(|(key, value)| key == "Network sync"
                && value.contains("No verified network receipt")));
            snapshot.services.secrets.network_materialization = Some(VaultNetworkMaterialization {
                source,
                sequence: 17,
                sampled_at_ms: 80_000,
            });
            let expired = fresh_snapshot(std::sync::Arc::new(snapshot), Some(160_001));
            assert_eq!(
                expired.services.secrets.availability,
                ServiceAvailability::Unknown
            );
            let receipt = expired.services.secrets.network_materialization.unwrap();
            assert_eq!(receipt.sampled_at_ms, 80_000);
            let rows = details(&expired, Target::Secrets);
            let route = match source {
                VaultNetworkSource::ConfiguredUrl => "configured route",
                VaultNetworkSource::PeerUrl => "peer route",
            };
            assert!(rows.iter().any(|(key, value)| key == "Network sync"
                && value.contains("Signed vault version 17")
                && value.contains(route)));
            assert!(
                rows.iter()
                    .any(|(key, value)| key == "Network fetch started"
                        && value == &observation_time(80_000))
            );
            assert!(
                rows.iter().any(|(key, value)| key == "Network sync scope"
                    && value.contains("Historical receipt"))
            );
            assert!(
                !details(&expired, Target::Accounts)
                    .iter()
                    .any(|(key, _)| key == "Network sync")
            );
        }
    }

    #[test]
    fn retained_204_details_do_not_turn_transport_contact_into_local_access() {
        use aterm_types::network_state::{VaultHttp204Response, VaultNetworkSource};
        for availability in [
            ServiceAvailability::Unknown,
            ServiceAvailability::Unavailable,
            ServiceAvailability::Cached,
        ] {
            let mut snapshot = NetworkStateSnapshot::default();
            snapshot.services.secrets.availability = availability;
            snapshot.services.secrets.observed_at_ms = 100_000;
            let absent = details(&snapshot, Target::Secrets);
            assert!(
                absent
                    .iter()
                    .any(|(key, value)| key == "Retained HTTP 204" && !value.is_empty())
            );
            let response = VaultHttp204Response {
                source: VaultNetworkSource::ConfiguredUrl,
                responded_at_ms: 80_000,
            };
            snapshot.services.secrets.http204_response = Some(response);
            let rows = details(&snapshot, Target::Secrets);
            assert_eq!(snapshot.services.secrets.availability, availability);
            assert_eq!(snapshot.services.secrets.network_materialization, None);
            assert_eq!(snapshot.services.secrets.last_success_ms, None);
            assert!(rows.iter().any(|(key, value)| key == "Retained HTTP 204"
                && value.contains("Configured route answered HTTP 204")));
            assert!(rows.iter().any(|(key, value)| key == "Response received"
                && value == &observation_time(response.responded_at_ms)));
            assert!(
                !details(&snapshot, Target::Accounts)
                    .iter()
                    .any(|(key, _)| key == "Retained HTTP 204")
            );
            let expired = fresh_snapshot(std::sync::Arc::new(snapshot), Some(160_001));
            assert_eq!(expired.services.secrets.http204_response, Some(response));
            assert_eq!(
                expired.services.secrets.availability,
                ServiceAvailability::Unknown
            );
        }
    }

    #[test]
    fn network_refresh_failure_keeps_cache_and_success_receipt_separate_in_the_lens() {
        use aterm_types::network_state::{
            VaultNetworkMaterialization, VaultNetworkRefresh, VaultNetworkRefreshOutcome,
            VaultNetworkSource,
        };
        let mut snapshot = NetworkStateSnapshot::default();
        let service = &mut snapshot.services.secrets;
        service.availability = ServiceAvailability::Cached;
        service.observed_at_ms = 100_000;
        service.refresh_failed = true;
        let receipt = VaultNetworkMaterialization {
            source: VaultNetworkSource::ConfiguredUrl,
            sequence: 17,
            sampled_at_ms: 80_000,
        };
        service.network_materialization = Some(receipt);
        for outcome in [
            VaultNetworkRefreshOutcome::Failed,
            VaultNetworkRefreshOutcome::Succeeded,
        ] {
            let refresh = VaultNetworkRefresh {
                completed_at_ms: 90_000,
                outcome,
            };
            snapshot.services.secrets.network_refresh = Some(refresh);
            snapshot.services.secrets.refresh_failed =
                outcome == VaultNetworkRefreshOutcome::Failed;
            let rows = details(&snapshot, Target::Secrets);
            let expected = if outcome == VaultNetworkRefreshOutcome::Failed {
                "Failed"
            } else {
                "Completed"
            };
            assert!(
                rows.iter()
                    .any(|(key, value)| key == "Recorded network refresh" && value == expected)
            );
            assert!(rows.iter().any(
                |(key, value)| key == "Refresh completed" && value == &observation_time(90_000)
            ));
            assert!(
                rows.iter()
                    .any(|(key, value)| key == "Network fetch started"
                        && value == &observation_time(80_000))
            );
            assert_eq!(
                snapshot.services.secrets.availability,
                ServiceAvailability::Cached
            );
            assert_eq!(
                snapshot.services.secrets.network_materialization,
                Some(receipt)
            );
            assert_eq!(snapshot.services.secrets.last_success_ms, None);
            assert!(
                !details(&snapshot, Target::Accounts)
                    .iter()
                    .any(|(key, _)| key == "Recorded network refresh")
            );
            let expired = fresh_snapshot(std::sync::Arc::new(snapshot.clone()), Some(160_001));
            assert_eq!(
                expired.services.secrets.availability,
                ServiceAvailability::Unknown
            );
            assert_eq!(expired.services.secrets.network_refresh, Some(refresh));
            assert_eq!(
                expired.services.secrets.network_materialization,
                Some(receipt)
            );
        }
    }

    #[test]
    fn an_inventory_observation_invalidates_an_open_network_lens_without_revision_change() {
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        assert!(app.open_network_lens());
        let before = app.stats_lens_fp(wid);
        let mut snapshot = app.network_snapshot.as_ref().clone();
        snapshot.network.accounts_exchange =
            Some(aterm_types::network_state::AccountsExchangeObservation {
                scope_digest: [1; 32],
                local_actor: [2; 48],
                control_observation: [3; 64],
                inventory_count: 0,
                inventory_fingerprint: [4; 32],
                peer: MachineIdentity {
                    machine_id: [5; 16],
                    incarnation: [6; 16],
                    label: None,
                },
                sampled_at_ms: 1,
            });
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
