// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure network-cell projection and drawing in the System Resources band.

use aterm_types::network_state::{
    HistoryState, MachineIdentity, NetworkStateSnapshot, REACHABLE_MS, Reachability,
    ServiceAvailability, ServiceState, Visibility,
};
use aterm_vitals::ink::Rgb;

use crate::stats_deck::DeckGeom;
use crate::stats_paint::{Hit, HitKind, Painter, panel};
use crate::widget::{DrawPrim, rgba};

pub(crate) mod field;
mod repair;

pub(crate) use field::{SeatClass, SeatKey};

/// Fixed headroom, independent of the number or state of peers.
pub(crate) const HEIGHT_PT: f64 = 30.0;

/// Place words and their offline classification, captured together before a frame is keyed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PlaceCaption {
    pub(crate) long: String,
    pub(crate) short: String,
    offline: bool,
}

pub(crate) fn inactive_snapshot() -> std::sync::Arc<NetworkStateSnapshot> {
    let mut snapshot = NetworkStateSnapshot::default();
    snapshot.network.visibility = Visibility::Unavailable;
    snapshot.network.history = HistoryState::Unavailable;
    snapshot.network.detail = "Network observer is not active for this instance".into();
    std::sync::Arc::new(snapshot)
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
    let network_stale =
        current_network_claim && !recent(snapshot.network.observed_at_ms, REACHABLE_MS);
    let check_peers = !network_stale && snapshot.network.visibility == Visibility::Active;
    let peers_stale = check_peers
        && snapshot
            .network
            .peers
            .iter()
            .any(|peer| peer.reachability_at(now_ms) != peer.reachability);
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
        // Each peer is aged against the lease its observer measured for it
        // (`PeerObservation::reachable_for_ms`), the window the observer itself reads it by.
        for peer in &mut expired.network.peers {
            peer.reachability = peer.reachability_at(now_ms);
            if peer.reachability != Reachability::Reachable {
                peer.lease_ms = None;
            }
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
    /// The width (device px) and scale of the Network State band window `wid` draws, as its
    /// deck and lens frames lay it out: what its keys and meters lay the Fleet Field out on.
    pub(crate) fn network_band_px(&self, wid: crate::WindowId) -> (f32, f32) {
        let Some(ws) = self.windows.get(&wid) else {
            return (0.0, 1.0);
        };
        let (cw, _) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        ((ws.cols as usize * cw + 2 * pad) as f32, ws.scale as f32)
    }

    /// Whether a visible window draws the Network State band (the deck) or its lens: what the
    /// lab's account rows are for.
    pub(crate) fn network_band_drawn(&self) -> bool {
        let deck_shown = self.stats_bars_form() == crate::stats_deck::StatsBars::Deck;
        self.windows.values().any(|window| {
            !window.occluded
                && ((deck_shown && crate::stats_deck::stats_drawn_on(window))
                    || window.stats_lens.open.is_some_and(|lens| {
                        lens.focus == crate::stats_paint::Instrument::NetworkState
                    }))
        })
    }

    pub(crate) fn network_state_deadline(&self) -> Option<std::time::Instant> {
        let due = || {
            self.network_next_poll
                .unwrap_or_else(std::time::Instant::now)
        };
        if self.network_source.is_none() {
            // With no network source the band still carries the lab's account cells, which
            // the feed reprojects once a minute ([`crate::lab_deck::Feed::EVERY`]), and the
            // Fleet Field, which follows the fleet glance's slot; the Lab page draws neither.
            return ((self.lab_deck_feed.live || self.fleet_glance.is_some())
                && self.network_band_drawn())
            .then(due);
        }
        (self.lab_page_shown()
            || self.network_band_drawn()
            || self.network_inspected_at(std::time::Instant::now()))
        .then(due)
    }

    /// Project the Fleet Field again where what it stands on moved: the glance's generation,
    /// its error and fleet flags, the observer's words it leads with, or a freshness edge
    /// ([`crate::fleet_glance::FleetSnapshot::next_freshness_change`]) passed at `now_ms`.
    /// Whether the field's key moved. Only a drawn band calls it.
    pub(crate) fn reproject_fleet_field(&mut self, now_ms: u64) -> bool {
        use std::hash::{Hash, Hasher};
        let Some(glance) = self.fleet_glance.as_ref() else {
            self.fleet_field_gen = None;
            self.fleet_field_due = None;
            return self.fleet_field.take().is_some();
        };
        let generation = glance.generation();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let network = &self.network_snapshot.network;
        std::mem::discriminant(&network.visibility).hash(&mut h);
        network.via_local_process.hash(&mut h);
        let stamp = (generation, h.finish());
        let due = self.fleet_field_due.is_some_and(|due| now_ms >= due);
        if self.fleet_field_gen == Some(stamp) && !due {
            return false;
        }
        let slot = glance.copy();
        let field = slot
            .book
            .in_fleet
            .then(|| field::project(Some(&slot), &self.network_snapshot.network, now_ms));
        self.fleet_field_gen = Some(stamp);
        self.fleet_field_due = match field.as_ref().map(|f| f.source) {
            // A stale read's age moves each minute.
            Some(field::Source::Stale { .. }) => Some(now_ms.saturating_add(60_000)),
            _ => slot
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.next_freshness_change(now_ms)),
        };
        let moved = self.fleet_field.as_ref().map(|f| f.key) != field.as_ref().map(|f| f.key);
        self.fleet_field = field.map(std::sync::Arc::new);
        moved
    }

    pub(crate) fn service_network_state(&mut self, now: std::time::Instant) {
        if self.network_next_poll.is_some_and(|due| now < due) {
            return;
        }
        let band = self.network_band_drawn();
        // The lab's account rows, as the feed's own thread last projected them: never waited
        // on, and asked only while drawn, since the ask is what starts and keeps the feed.
        let drawn = self.lab_deck_feed.live && band;
        let lab = if drawn {
            self.lab_deck_feed.latest(&self.lab_deck)
        } else {
            None
        };
        let fleet = band && self.fleet_glance.is_some();
        // The services' cells are drawn only on the band, its lens and the Lab page, and their
        // states are read by an inspection of the projection; with none of these the services'
        // observer is not asked to sample, and their last sample ages.
        let inspected = self.network_inspected_at(now);
        let services = if band || self.lab_page_shown() || inspected {
            crate::NetworkRead::WithServices
        } else {
            crate::NetworkRead::Network
        };
        let snapshot = if let Some(read) = self.network_source.as_ref() {
            self.network_next_poll = Some(now + std::time::Duration::from_secs(1));
            snapshot_after_read(
                read(services),
                &self.network_snapshot,
                &mut self.network_source_busy,
                aterm_time::unix_millis(),
            )
        } else if drawn || fleet {
            // The lab feed's first look comes back soon, since its thread starts on it; then
            // once a projection. The fleet's field needs no tick of its own: a new read clears
            // the due (`on_fleet_read`), and its next freshness edge sets it below.
            let next = if drawn && !self.lab_deck_feed.held_any() {
                std::time::Duration::from_secs(1)
            } else {
                crate::lab_deck::Feed::EVERY
            };
            self.network_next_poll = Some(now + next);
            self.network_snapshot.clone()
        } else {
            // Nothing draws the rows: no poll is owed, and the next drawn turn looks at once.
            self.network_next_poll = None;
            return;
        };
        let changed = snapshot.as_ref() != self.network_snapshot.as_ref() || lab.is_some();
        if changed {
            self.network_snapshot = snapshot;
        }
        if let Some(lab) = lab {
            self.lab_deck = lab;
        }
        let now_ms = aterm_time::unix_millis();
        let field_moved = band && now_ms.is_some_and(|now_ms| self.reproject_fleet_field(now_ms));
        if fleet
            && self.network_source.is_none()
            && let (Some(due), Some(now_ms)) = (self.fleet_field_due, now_ms)
        {
            let edge = now + std::time::Duration::from_millis(due.saturating_sub(now_ms));
            self.network_next_poll = Some(self.network_next_poll.map_or(edge, |p| p.min(edge)));
        }
        if !changed && !field_moved {
            return;
        }
        if changed && self.lab_page_shown() {
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

/// A stable cell selection; a seat is never its current vector index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    #[default]
    Overview,
    Local,
    /// The Fleet Field's words: the fleet as a whole.
    Fleet,
    /// One seat of the Fleet Field: a fleet seat, or an observed peer by its slot.
    Seat(SeatKey),
    /// A Tier C tally group.
    Group(SeatClass),
    Accounts,
    Secrets,
    /// One lab account's row ([`crate::lab_deck::DeckAccount::id`]), on the Shared State line.
    LabAccount(u64),
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
        .map(|place| {
            let (long, short) = aterm_netplace::words::band(place);
            PlaceCaption {
                long,
                short,
                offline: place.at == aterm_netplace::Where::Offline,
            }
        })
}

/// Capture place words and status together; paint never reads the changing place cache.
pub(crate) fn capture_place_caption() -> Option<PlaceCaption> {
    place_caption(crate::net_place::cached().as_deref())
}

/// A test summary with the place as last read, without starting a read.
#[cfg(test)]
pub(crate) fn summary(snapshot: &NetworkStateSnapshot) -> String {
    summary_with_caption(snapshot, capture_place_caption().as_ref())
}

/// [`summary`] with this Mac's network place `place`, retaining independent observer failures.
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
    if let Some(caption) = caption {
        if place_only(network_status(snapshot), caption) {
            text.clone_from(&caption.long);
        } else {
            text.push_str(" · ");
            text.push_str(&caption.short);
        }
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

fn place_only(status: NetworkStatus, caption: &PlaceCaption) -> bool {
    status == NetworkStatus::Visibility(Visibility::Active)
        || status == NetworkStatus::Visibility(Visibility::Offline) && caption.offline
}

fn network_status(snapshot: &NetworkStateSnapshot) -> NetworkStatus {
    network_status_of(&snapshot.network)
}

fn network_status_of(network: &aterm_types::network_state::NetworkObservation) -> NetworkStatus {
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

/// Every selectable cell, the lab's account rows after Secrets in the band's order: the Fleet
/// Field's words and, as a band `band` = (width px, scale) lays it out, its attention seats or
/// its tally's groups where a fleet is read ([`field::targets`]), else every observed peer.
pub(crate) fn targets_with(
    snapshot: &NetworkStateSnapshot,
    lab: &crate::lab_deck::LabDeck,
    fleet: Option<&field::Field>,
    (w, scale): (f32, f32),
) -> Vec<Target> {
    let mut targets = vec![Target::Overview, Target::Local];
    let observed;
    let line1 = match fleet {
        Some(fleet) => fleet,
        None => {
            observed = field::observer(&snapshot.network);
            &observed
        }
    };
    let layout = field::layout(line1, &snapshot.network, w, scale.max(0.5));
    targets.extend(field::targets(line1, &layout));
    targets.extend([Target::Accounts, Target::Secrets]);
    targets.extend(lab.rows.iter().map(|row| Target::LabAccount(row.id)));
    targets
}

pub(crate) fn step(
    snapshot: &NetworkStateSnapshot,
    lab: &crate::lab_deck::LabDeck,
    fleet: Option<&field::Field>,
    band: (f32, f32),
    target: Target,
    forward: bool,
) -> Target {
    let all = targets_with(snapshot, lab, fleet, band);
    let index = all.iter().position(|value| *value == target).unwrap_or(0);
    all[if forward {
        (index + 1) % all.len()
    } else {
        (index + all.len() - 1) % all.len()
    }]
}

/// The band's accessibility meters on a band `band` = (width px, scale), `(name, words)`: one a
/// target ([`targets_with`]), and after
/// the Fleet's one for the strip, its seats by class ([`field::strip_words`]), so their count is
/// bounded by attention, never by the fleet's size.
pub(crate) fn meters(
    snapshot: &NetworkStateSnapshot,
    lab: &crate::lab_deck::LabDeck,
    fleet: Option<&field::Field>,
    band: (f32, f32),
) -> Vec<(String, String)> {
    let mut meters = Vec::new();
    for target in targets_with(snapshot, lab, fleet, band) {
        let words = details_lab(snapshot, lab, fleet, target);
        // A lab account's and a fleet target's first row names it; the others' second row says
        // what they are.
        let named = usize::from(!matches!(
            target,
            Target::LabAccount(_)
                | Target::Fleet
                | Target::Seat(SeatKey::Fleet(_))
                | Target::Group(_)
        ));
        let name = words.get(named).map_or_else(
            || "Network State".to_owned(),
            |(name, value)| format!("{name}: {value}"),
        );
        meters.push((
            name,
            words
                .into_iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join(". "),
        ));
        if target == Target::Fleet
            && let Some(fleet) = fleet
        {
            meters.push(("Fleet strip".to_owned(), field::strip_words(fleet)));
        }
    }
    meters
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

/// [`details`] with the lab's account rows (a lab account's own rows, and the overview's count)
/// and the Fleet Field `fleet` (its words, seats and groups, and the overview's first rows).
pub(crate) fn details_lab(
    snapshot: &NetworkStateSnapshot,
    lab: &crate::lab_deck::LabDeck,
    fleet: Option<&field::Field>,
    target: Target,
) -> Vec<(String, String)> {
    details_lab_with_caption(
        snapshot,
        lab,
        fleet,
        target,
        capture_place_caption().as_ref(),
    )
}

/// [`details_lab`] with the frame's captured place words.
pub(crate) fn details_lab_with_caption(
    snapshot: &NetworkStateSnapshot,
    lab: &crate::lab_deck::LabDeck,
    fleet: Option<&field::Field>,
    target: Target,
    caption: Option<&PlaceCaption>,
) -> Vec<(String, String)> {
    if let Target::LabAccount(id) = target {
        let mut rows = lab.details(id);
        rows.push((
            "Evidence".into(),
            "The lab registry and journal as this Mac last read them; no provider was asked".into(),
        ));
        return rows;
    }
    if let Some(fleet) = fleet
        && matches!(
            target,
            Target::Fleet | Target::Seat(SeatKey::Fleet(_)) | Target::Group(_)
        )
    {
        return field::details(fleet, target);
    }
    if fleet.is_none()
        && let Target::Group(_) = target
    {
        // An observer's tally: its groups are of the observed peers.
        return field::details(&field::observer(&snapshot.network), target);
    }
    let mut rows = details_with_caption(snapshot, target, caption);
    if target == Target::Overview
        && let Some(line) = lab.overview()
    {
        let at = rows
            .iter()
            .position(|(name, _)| name == "History")
            .map_or(rows.len(), |i| i + 1);
        rows.insert(at, line);
    }
    if target == Target::Overview
        && let Some(fleet) = fleet
        && fleet.snapshot.is_some()
    {
        // A read fleet comes first: its words, then its Now line.
        rows.insert(0, ("Now".into(), field::now_line(fleet)));
        rows.insert(
            0,
            (
                "Fleet".into(),
                fleet.words.first().cloned().unwrap_or_default(),
            ),
        );
    }
    rows
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
        Target::Seat(SeatKey::Observed(slot)) => {
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
        Target::Local => {
            rows.push((
                "This machine".into(),
                network
                    .observer
                    .as_ref()
                    .map_or_else(|| "Identity not established".into(), machine_name),
            ));
            rows.extend(field::observed_rows(network));
        }
        Target::Fleet | Target::Seat(SeatKey::Fleet(_)) | Target::Group(_) => rows.push((
            "Fleet".into(),
            "Not read: this Mac's fleet glance holds no read".into(),
        )),
        Target::Accounts => rows.extend(account_service_details(snapshot)),
        Target::LabAccount(_) => rows.push((
            "Account".into(),
            "Lab account rows are read with the lab journal".into(),
        )),
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
    if matches!(
        target,
        Target::Seat(SeatKey::Observed(_))
            | Target::Accounts
            | Target::Secrets
            | Target::LabAccount(_)
    ) {
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

/// Append two fixed rows with the existing resource painter and theme inks: line 1 the label,
/// the Local cell and the Fleet Field (`fleet`, else the observer's peers), line 2 Shared State.
#[expect(
    clippy::too_many_arguments,
    reason = "the band's geometry, its sources and its inks, each read by one line"
)]
pub(crate) fn paint_with_inks(
    g: &DeckGeom,
    top: f32,
    snapshot: &NetworkStateSnapshot,
    lab: &crate::lab_deck::LabDeck,
    fleet: Option<&field::Field>,
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
        p.prims.push(DrawPrim::Stroke {
            x,
            y: y - side / 2.0,
            w: side,
            h: side,
            radius: 0.0,
            width: scale.max(1.0),
            color: rgba(color, 255),
        });
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
    // Line 1's field after the cells: the cells' hits close over `hits` until here.
    let observed;
    let line1 = match fleet {
        Some(fleet) => fleet,
        None => {
            observed = field::observer(&snapshot.network);
            &observed
        }
    };
    field::paint(
        &mut p,
        line1,
        &snapshot.network,
        caption,
        top,
        g.w,
        &mut hits,
    );
    let lab_x = secrets_cell_x + 8.0 * scale + 2.0 * gap;
    paint_lab_accounts(&mut p, &mut hits, lab, (lab_x, second_y), g.w, scale, dwc);
    p.prims.push(DrawPrim::ClipPop);
    (p.prims, hits)
}

/// Where the lab's account cells go on the Shared State line, in the band's own pitch: the
/// word, then one 8 pt cell per account every 14 pt, then `+N` for the rows that do not fit.
const LAB_PITCH_PT: f32 = 14.0;
/// The band's right margin, pt: no cell starts closer to the edge than this.
const LAB_MARGIN_PT: f32 = 12.0;

/// The lab's account cells after Secrets (`docs/DESIGN-alab-lab-model-2026-11-01.md` §8, "As built
/// (Stats deck rows, task sd)"): each an 8 pt square in the band's cell form, its rim this Mac's
/// ink where this Mac prefers the account and the machine's otherwise, filled with the heat of
/// its binding window's use (the over-max cap's ink at a wall), hollow with no current reading.
/// It takes no row: the band's height is fixed.
fn paint_lab_accounts(
    p: &mut Painter<'_>,
    hits: &mut Vec<Hit>,
    lab: &crate::lab_deck::LabDeck,
    (x, y): (f32, f32),
    w: f32,
    scale: f32,
    dwc: bool,
) {
    use crate::lab_deck::Cell;
    if lab.is_empty() {
        return;
    }
    let inks = *p.inks;
    let word = "Lab accounts";
    let cells_x = x + p.ui_text_w(word, false) + 7.0 * scale;
    let side = 8.0 * scale;
    let pitch = LAB_PITCH_PT * scale;
    let total = lab.rows.len() + lab.more;
    let fits = |n: usize| {
        let more = if n < total {
            p.ui_text_w(&format!("+{}", total - n), false) + 4.0 * scale
        } else {
            0.0
        };
        cells_x + n as f32 * pitch + more <= w - LAB_MARGIN_PT * scale
    };
    let mut shown = lab.rows.len();
    while shown > 0 && !fits(shown) {
        shown -= 1;
    }
    if shown == 0 {
        return;
    }
    p.ui_text(x, y + 3.0 * scale, word.into(), inks.words2, false);
    for (index, row) in lab.rows.iter().take(shown).enumerate() {
        let cx = cells_x + index as f32 * pitch;
        let rim = if row.here { inks.aterm } else { inks.machine };
        p.prims.push(DrawPrim::Stroke {
            x: cx,
            y: y - side / 2.0,
            w: side,
            h: side,
            radius: 0.0,
            width: scale.max(1.0),
            color: rgba(rim, 255),
        });
        let (fill, step) = match row.cell {
            Cell::Unknown => (None, 0),
            Cell::Used(x10) => {
                let step = aterm_vitals::ink::heat_step(f32::from(x10) / 1000.0);
                (Some(inks.heat[usize::from(step)]), step)
            }
            Cell::AtLimit => (Some(inks.over), 6),
        };
        if let Some(fill) = fill {
            let h = if dwc {
                aterm_vitals::ink::dwc_height(side.round() as u32, step) as f32
            } else {
                side
            };
            p.prims
                .push(panel(cx, y + side / 2.0 - h, side, h, 0.0, rgba(fill, 255)));
        }
        hits.insert(
            0,
            Hit {
                rect: (
                    cx - 3.0 * scale,
                    y - 6.0 * scale,
                    14.0 * scale,
                    12.0 * scale,
                ),
                kind: HitKind::Network(Target::LabAccount(row.id)),
            },
        );
    }
    if shown < total {
        p.ui_text(
            cells_x + shown as f32 * pitch,
            y + 3.0 * scale,
            format!("+{}", total - shown),
            inks.words2,
            false,
        );
    }
}

#[cfg(test)]
#[path = "network_deck_tests.rs"]
mod tests;
