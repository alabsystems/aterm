// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE WORDING of the place. `status`, `sys`, `fleet peers`, `survey`, `secrets status`,
//! `members requests`, `doctor`, the band and the menu bar all print from here, so no two of them
//! can say different things about where this Mac is (the window's away entry, [`away_entry`],
//! included). The machine fields ([`status_fields`], [`json_fields`]) are an interface; the
//! sentences are for a person and may be reworded here, once, for every reporter.

use crate::place::{
    Alab, Checking, InternetSource, InternetState, Kind, Lane, Place, Recognised, Where,
};

/// What the words need besides the place: the clock now and the local zone.
#[derive(Clone, Copy)]
pub struct Words<'a> {
    /// Now, unix ms.
    pub now_ms: u64,
    /// The local offset in seconds at a unix second (`None`: the zone is unknown, said as UTC).
    pub zone: &'a dyn Fn(i64) -> Option<i64>,
}

impl std::fmt::Debug for Words<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Words")
            .field("now_ms", &self.now_ms)
            .finish_non_exhaustive()
    }
}

/// The local zone ([`aterm_uds::zone::local_offset_at`]).
fn local_zone(unix: i64) -> Option<i64> {
    aterm_uds::zone::local_offset_at(unix)
}

impl Words<'static> {
    /// The words at this machine's clock and zone.
    #[must_use]
    pub fn now() -> Self {
        Self {
            now_ms: crate::unix_ms_now(),
            zone: &local_zone,
        }
    }
}

impl Words<'_> {
    /// `ms` as a local `HH:MM` (`UTC` added where the zone is unknown); a day or more ago adds
    /// how many days.
    #[must_use]
    pub fn clock(&self, ms: u64) -> String {
        let at = i64::try_from(ms / 1000).unwrap_or(i64::MAX);
        let offset = (self.zone)(at);
        let local = at.saturating_add(offset.unwrap_or(0));
        let minute = local.rem_euclid(86_400) / 60;
        let ago_days = self.now_ms.saturating_sub(ms) / 86_400_000;
        let zone = if offset.is_some() { "" } else { " UTC" };
        if ago_days > 0 {
            format!(
                "{:02}:{:02}{zone} ({ago_days}d ago)",
                minute / 60,
                minute % 60
            )
        } else {
            format!("{:02}:{:02}{zone}", minute / 60, minute % 60)
        }
    }
}

/// A kind of network for a person: `phone hotspot`, `another LAN`, `cellular`, `VPN`.
#[must_use]
pub fn kind_words(kind: Kind) -> &'static str {
    match kind {
        Kind::Hotspot { by_range: false } => "phone hotspot",
        Kind::Hotspot { by_range: true } => "phone hotspot, by its address range",
        Kind::Lan => "another LAN",
        Kind::Cellular => "cellular",
        Kind::Vpn => "VPN",
    }
}

fn where_words_cow(place: &Place) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    match place.at {
        Where::Network(Kind::Lan) => Cow::Borrowed("a LAN"),
        Where::Network(kind) => Cow::Borrowed(kind_words(kind)),
        Where::AlabLan => Cow::Borrowed("ALab LAN"),
        Where::Remote(_) if place.once_alab => {
            Cow::Borrowed("remote (a network members were once on)")
        }
        Where::Remote(kind) => Cow::Owned(format!("remote ({})", kind_words(kind))),
        Where::Offline => Cow::Borrowed("offline"),
        Where::Checking(Checking::Moved) => {
            Cow::Borrowed("checking (moved; reading the new network)")
        }
        Where::Checking(Checking::Learning) => Cow::Borrowed("checking (ALab LAN not learned yet)"),
        Where::Checking(Checking::Unidentified) => Cow::Borrowed(
            "checking (network not identified: its router's hardware address is unread)",
        ),
        Where::Checking(Checking::NoIpv4) => Cow::Borrowed("checking (no IPv4 route)"),
    }
}

/// Where this Mac is, for a person: `ALab LAN`, `remote (phone hotspot)`, `offline`,
/// `checking (…)`, or with no fleet to learn from `a LAN`, `VPN`.
#[must_use]
pub fn where_words(place: &Place) -> String {
    where_words_cow(place).into_owned()
}

/// ` since 11:14`, ` since before 11:14`, or nothing with no stay.
fn since_words(place: &Place, words: &Words<'_>) -> String {
    place.since_ms.map_or_else(String::new, |ms| {
        format!(
            " since {}{}",
            if place.since_before { "before " } else { "" },
            words.clock(ms)
        )
    })
}

/// The internet in two words: `internet ok`, `internet down`, `internet path up (not
/// measured)`, `internet unknown`.
#[must_use]
pub fn internet_short(place: &Place) -> &'static str {
    match place.internet.state {
        InternetState::Ok => "internet ok",
        InternetState::Down => "internet down",
        InternetState::PathUp => "internet path up (not measured)",
        InternetState::Unknown => "internet unknown",
    }
}

/// The internet with how it is known.
#[must_use]
pub fn internet_long(place: &Place, words: &Words<'_>) -> String {
    let endpoint = place.internet.endpoint.as_deref().unwrap_or("the API");
    let at = || {
        place
            .internet
            .at_ms
            .map(|ms| words.clock(ms))
            .unwrap_or_default()
    };
    match (place.internet.state, place.internet.source) {
        (InternetState::Ok, InternetSource::TurnReply) => {
            format!(
                "internet ok (a session's turn got a reply from {endpoint} at {})",
                at()
            )
        }
        (InternetState::Ok, _) => format!("internet ok ({endpoint} answered {})", at()),
        (InternetState::Down, InternetSource::Path) => "internet down (no network path)".to_owned(),
        (InternetState::Down, _) => {
            format!("internet down ({endpoint} did not answer at {})", at())
        }
        (InternetState::PathUp, _) => "internet path up, not measured on this network".to_owned(),
        (InternetState::Unknown, _) => "internet unknown (nothing measures it here)".to_owned(),
    }
}

/// The members reachable: `12 of 41 members reachable`, or with no book `12 members reachable`.
#[must_use]
pub fn members_words(place: &Place) -> String {
    match place.known {
        Some(known) => format!("{} of {known} members reachable", place.reachable),
        None => format!("{} members reachable (no fleet book here)", place.reachable),
    }
}

/// The ALab LAN in a few words.
#[must_use]
pub fn alab_short(place: &Place, words: &Words<'_>) -> String {
    match place.alab {
        Alab::Here if place.reachable == 0 => "members not heard yet".to_owned(),
        Alab::Here => members_words(place),
        Alab::Away { since_ms: Some(ms) } => format!("ALab LAN: away since {}", words.clock(ms)),
        Alab::Away { since_ms: None } => "ALab LAN: away".to_owned(),
        Alab::Learning => "ALab LAN not learned yet".to_owned(),
        Alab::Unknown => "ALab LAN: not known while checking".to_owned(),
        Alab::NoFleet => format!(
            "no ALab LAN to learn (the fleet book knows fewer than {} members)",
            crate::place::LEARN_AT
        ),
    }
}

/// The last member answer: `last member answer 11:13 (fleet)`.
fn last_answer_words(place: &Place, words: &Words<'_>) -> Option<String> {
    place.last_answer.map(|(ms, lane)| {
        format!(
            "last member answer {} ({})",
            words.clock(ms),
            match lane {
                Lane::Fleet => "fleet",
                Lane::Commons => "Commons",
            }
        )
    })
}

/// THE HEADER every command's report opens with: `network: remote (phone hotspot) since 11:14 ·
/// internet ok · ALab LAN: away since 11:14`, or at home `network: ALab LAN · internet ok · 12
/// of 41 members reachable`.
#[must_use]
pub fn header(place: &Place, words: &Words<'_>) -> String {
    format!("network: {}", summary(place, words))
}

/// [`header`] without its label: the menu bar's first Fleet row and the doctor's row.
#[must_use]
pub fn summary(place: &Place, words: &Words<'_>) -> String {
    use std::fmt::Write as _;
    let since = if place.at == Where::AlabLan {
        String::new()
    } else {
        since_words(place, words)
    };
    let mut out = format!(
        "{}{since} · {} · {}",
        where_words_cow(place),
        internet_short(place),
        alab_short(place, words)
    );
    if matches!(place.alab, Alab::Away { .. }) && place.reachable > 0 {
        match place.known {
            Some(known) => {
                let _ = write!(out, " · {} of {known} members here", place.reachable);
            }
            None => {
                let _ = write!(out, " · {} of ? members here", place.reachable);
            }
        }
    }
    out
}

/// THE `sys net` LINE: where and since, the interface, address, subnet and gateway, the
/// expensive and Low Data flags, the internet with its source, and the ALab LAN.
#[must_use]
pub fn line(place: &Place, words: &Words<'_>) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(192);
    out.push_str(&where_words_cow(place));
    if let Some(ms) = place.since_ms {
        let _ = write!(
            out,
            " since {}{}",
            if place.since_before { "before " } else { "" },
            words.clock(ms)
        );
    }
    if place.at == Where::AlabLan {
        out.push_str(" (recognised: ");
        match place.recognised {
            Some(Recognised::Members) => out.push_str("members answered here"),
            Some(Recognised::Remembered) => {
                out.push_str("network ");
                out.push_str(place.network.as_ref().map_or("?", |id| id.short()));
            }
            Some(Recognised::Seeded) => {
                out.push_str("the fleet book's members answered on this subnet");
            }
            None => {}
        }
        out.push(')');
    }
    if let Some(route) = &place.route {
        let link = place.path.map_or("", |path| path.link.word());
        let _ = write!(
            out,
            " · {}{}{} {}/{} ",
            route.interface,
            if link.is_empty() { "" } else { " " },
            link,
            route.address,
            route.prefix
        );
        match route.gateway {
            Some(gw) => {
                let _ = write!(out, "via {gw}");
            }
            None => out.push_str("no gateway"),
        }
    }
    if let Some(tunnel) = &place.tunnel {
        let _ = write!(out, " · {tunnel} holds the default route");
    }
    if place.path.is_some_and(|path| path.expensive) {
        out.push_str(" · expensive");
    }
    if place.path.is_some_and(|path| path.constrained) {
        out.push_str(" · Low Data Mode");
    }
    out.push_str(" · ");
    out.push_str(&internet_long(place, words));
    out.push_str(" · ");
    if matches!(place.alab, Alab::Here) {
        out.push_str("ALab LAN: ");
    }
    out.push_str(&alab_short(place, words));
    if !matches!(place.alab, Alab::Here) {
        if let Some(last) = last_answer_words(place, words) {
            let _ = write!(out, " ({last})");
        }
        out.push_str(" · ");
        out.push_str(&members_words(place));
    }
    out
}

/// THE `status` FIELDS: `net=<alab-lan|remote|offline|checking> net_kind=<hotspot|lan|cellular|
/// vpn|-> net_since_ms=<unix ms|-> internet=<ok|down|path-up|unknown> internet_ms=<unix ms|->
/// members=<reachable>/<known>|- away_since_ms=<unix ms|->`, in this order.
#[must_use]
pub fn status_fields(place: &Place) -> String {
    use std::fmt::Write as _;
    let push_opt = |out: &mut String, ms: Option<u64>| match ms {
        Some(ms) => {
            let _ = write!(out, "{ms}");
        }
        None => out.push('-'),
    };
    let mut out = String::with_capacity(96);
    let _ = write!(
        out,
        "net={} net_kind={} net_since_ms=",
        place.at.word(),
        place.kind.map_or("-", Kind::word),
    );
    push_opt(&mut out, place.since_ms);
    let _ = write!(
        out,
        " internet={} internet_ms=",
        place.internet.state.word()
    );
    push_opt(&mut out, place.internet.at_ms);
    out.push_str(" members=");
    match (place.at, place.known) {
        (Where::Offline, _) | (_, None) => out.push('-'),
        (_, Some(known)) => {
            let _ = write!(out, "{}/{known}", place.reachable);
        }
    }
    out.push_str(" away_since_ms=");
    match place.alab {
        Alab::Away { since_ms } => push_opt(&mut out, since_ms),
        _ => out.push('-'),
    }
    out
}

/// `status`'s fields when no place could be read: every value `-`.
pub const STATUS_UNKNOWN: &str =
    "net=- net_kind=- net_since_ms=- internet=- internet_ms=- members=- away_since_ms=-";

/// One value of the `sys --json` place object; the host maps it onto `aterm_vitals::PlaceValue`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    /// A word or an address.
    Text(String),
    /// A count or a unix-ms time.
    Count(u64),
    /// A yes or no.
    Flag(bool),
    /// Not known (`sys`'s `not_sampled`).
    Absent,
    /// A nested object.
    Object(Vec<(&'static str, Field)>),
}

/// THE `sys --json` PLACE OBJECT's fields, in order.
#[must_use]
pub fn json_fields(place: &Place) -> Vec<(&'static str, Field)> {
    let text = |s: Option<String>| s.map_or(Field::Absent, Field::Text);
    let count = |n: Option<u64>| n.map_or(Field::Absent, Field::Count);
    let route = place.route.as_ref();
    let alab = match place.alab {
        Alab::Away { since_ms } => count(since_ms),
        _ => Field::Absent,
    };
    vec![
        ("state", Field::Text(place.at.word().to_owned())),
        ("kind", text(place.kind.map(|kind| kind.word().to_owned()))),
        ("words", Field::Text(where_words(place))),
        ("interface", text(route.map(|r| r.interface.clone()))),
        ("link", text(place.path.map(|p| p.link.word().to_owned()))),
        ("address", text(route.map(|r| r.address.to_string()))),
        (
            "subnet",
            text(route.map(crate::place::RouteFacts::subnet_words)),
        ),
        (
            "gateway",
            text(route.and_then(|r| r.gateway).map(|g| g.to_string())),
        ),
        (
            "network",
            text(place.network.as_ref().map(|id| id.short().to_owned())),
        ),
        (
            "network_strong",
            place
                .network
                .as_ref()
                .map_or(Field::Absent, |id| Field::Flag(id.is_strong())),
        ),
        ("tunnel", text(place.tunnel.clone())),
        (
            "expensive",
            place
                .path
                .map_or(Field::Absent, |p| Field::Flag(p.expensive)),
        ),
        (
            "constrained",
            place
                .path
                .map_or(Field::Absent, |p| Field::Flag(p.constrained)),
        ),
        ("since_ms", count(place.since_ms)),
        ("since_before", Field::Flag(place.since_before)),
        (
            "internet",
            Field::Object(vec![
                ("state", Field::Text(place.internet.state.word().to_owned())),
                (
                    "source",
                    Field::Text(place.internet.source.word().to_owned()),
                ),
                ("checked_ms", count(place.internet.at_ms)),
                ("endpoint", text(place.internet.endpoint.clone())),
            ]),
        ),
        (
            "alab",
            Field::Object(vec![
                ("state", Field::Text(place.alab.word().to_owned())),
                ("since_ms", alab),
                ("reachable", Field::Count(place.reachable as u64)),
                ("known", count(place.known.map(|k| k as u64))),
                ("last_answer_ms", count(place.last_answer.map(|(ms, _)| ms))),
                (
                    "last_answer_lane",
                    text(place.last_answer.map(|(_, lane)| lane.word().to_owned())),
                ),
                ("once_alab", Field::Flag(place.once_alab)),
            ]),
        ),
        (
            "computed_by",
            Field::Text(place.computed_by.word().to_owned()),
        ),
    ]
}

/// The band's summary and its short form: `Away from ALab LAN · phone hotspot · 41 known` and
/// `Away · hotspot`, or at home `12 reachable · 41 known` and `ALab LAN`.
#[must_use]
pub fn band(place: &Place) -> (String, String) {
    let known = place
        .known
        .map_or_else(String::new, |known| format!(" · {known} known"));
    match place.at {
        Where::AlabLan if place.reachable == 0 => (
            format!("ALab LAN · checking members{known}"),
            "ALab LAN".to_owned(),
        ),
        Where::AlabLan => (
            format!("{} reachable{known}", place.reachable),
            "ALab LAN".to_owned(),
        ),
        Where::Remote(kind) if matches!(place.alab, Alab::Away { .. }) => (
            format!("Away from ALab LAN · {}{known}", kind_words(kind)),
            format!("Away · {}", kind.word()),
        ),
        Where::Remote(kind) => (
            format!("Remote · {}{known}", kind_words(kind)),
            format!("Remote · {}", kind.word()),
        ),
        Where::Offline => (format!("Offline{known}"), "Offline".to_owned()),
        Where::Network(kind) => (
            format!("{}{known}", where_words_cow(place)),
            kind.word().to_owned(),
        ),
        Where::Checking(_) => (
            format!("Checking the network{known}"),
            "Checking".to_owned(),
        ),
    }
}

/// Whether this Mac is off the ALab LAN in a way its own words should lead with (the band's
/// summary, the doctor's note): on another network or offline. Never while checking (it may yet
/// be the ALab LAN, and the observer's words stand meanwhile) or with no fleet to learn from.
#[must_use]
pub const fn off_the_alab_lan(place: &Place) -> bool {
    matches!(place.at, Where::Remote(_) | Where::Offline)
}

/// Whether `addr` lies on the network this Mac is on now (its route's subnet, on the link): a
/// member there that this Mac's dial found no route to is judged by [`on_link`], wherever this
/// Mac is, so being away explains nothing of it.
#[must_use]
pub fn on_this_network(place: &Place, addr: std::net::IpAddr) -> bool {
    place
        .route
        .as_ref()
        .is_some_and(|route| route.on_link(addr.to_canonical()))
}

/// One dial of a fleet read, as the on-link rule reads it ([`on_link`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dial {
    /// The connect found no route to this address (`EHOSTUNREACH`); `None` where the read kept
    /// no address.
    NoRoute(Option<std::net::IpAddr>),
    /// Anything else: an answer, a refused connect or failed handshake, a timeout, a dial never
    /// made.
    Other,
}

/// THE ON-LINK RULE's verdict on one fleet read's no-route dials, which `fleet peers`,
/// `ctl survey --fleet` and the menu bar (through the survey's note and rows) say alike. A
/// no-route dial is judged by its address against the network this Mac is on now, never by a
/// network members were once on:
///
/// - to an address on this network: that member's own, off the link now ([`Self::gone`],
///   [`GONE_FROM_THE_LINK`]), not being away;
/// - to an address off this network: the route's ([`Self::elsewhere`]);
/// - with no address to judge ([`Self::unplaced`]): the caller's rule from before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnLink {
    /// No-route dials on this network.
    pub gone: usize,
    /// No-route dials to an address off this network.
    pub elsewhere: usize,
    /// No-route dials with no address.
    pub unplaced: usize,
}

impl OnLink {
    /// Whether a no-route dial to `addr` is one of [`Self::gone`]: on this network.
    #[must_use]
    pub fn gone_at(&self, place: &Place, addr: std::net::IpAddr) -> bool {
        self.gone > 0 && on_this_network(place, addr)
    }
}

/// [`OnLink`] for the `dials` of one fleet read, this Mac at `place`.
#[must_use]
pub fn on_link(place: &Place, dials: impl IntoIterator<Item = Dial>) -> OnLink {
    let mut out = OnLink::default();
    for dial in dials {
        match dial {
            Dial::NoRoute(Some(addr)) if on_this_network(place, addr) => out.gone += 1,
            Dial::NoRoute(Some(_)) => out.elsewhere += 1,
            Dial::NoRoute(None) => out.unplaced += 1,
            Dial::Other => {}
        }
    }
    out
}

/// A row's words for a member [`OnLink::gone_at`] names.
pub const GONE_FROM_THE_LINK: &str = "no route to it on this Mac's network: it is off the link \
     now (asleep, off or moved)";

/// THE WHY a fleet read gives when this Mac dialled members and none answered while it is away
/// from the ALab LAN (timeouts, the hotspot case): which network it is on and since when. `None`
/// on the ALab LAN, offline (the offline note says that), while checking, and on a remembered
/// network that fell silent ([`Place::once_alab`]: it may be home with every member asleep or
/// off).
#[must_use]
pub fn why_away(place: &Place, words: &Words<'_>) -> Option<String> {
    let Where::Remote(kind) = place.at else {
        return None;
    };
    if place.once_alab {
        return None;
    }
    let subnet = place
        .route
        .as_ref()
        .map_or_else(String::new, |route| format!(", {}", route.subnet_words()));
    Some(format!(
        "Why: this Mac is on another network ({}{subnet}){}, not the ALab LAN; the members it \
         dialled do not answer from here. They answer again once it is back.",
        kind_words(kind),
        since_words(place, words)
    ))
}

/// The note a fleet survey ends with when every peer it dialled timed out while this Mac is
/// away from the ALab LAN; `None` where [`why_away`] gives none.
#[must_use]
pub fn survey_note(place: &Place, words: &Words<'_>) -> Option<String> {
    let Where::Remote(kind) = place.at else {
        return None;
    };
    if place.once_alab {
        return None;
    }
    let last = place.last_answer.map_or_else(
        || "no member answer is recorded".to_owned(),
        |(ms, _)| format!("the members were last heard at {}", words.clock(ms)),
    );
    Some(format!(
        "This Mac is on another network ({}{}): no fleet peer answered; {last}.",
        kind_words(kind),
        since_words(place, words)
    ))
}

/// THE AWAY ENTRY's words (the exceptions digest's one fact for being away, which the window
/// files as an Info entry: never an alert, never NEEDS YOU): a title, why a person is the one to
/// change it, the one step that does, and the detail lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwayEntry {
    /// The glass title.
    pub what: &'static str,
    /// Why only a person changes it.
    pub why: &'static str,
    /// The one step that changes it.
    pub step: &'static str,
    /// Detail: where and since when, and what still works.
    pub more: Vec<String>,
    /// The aterm.log line kept when it is first raised.
    pub log: String,
}

/// How long this Mac must have been on its current network before being away is an entry: a
/// Wi-Fi blip at home (offline for seconds, then the same network) raises nothing.
pub const AWAY_ENTRY_AFTER_MS: u64 = 2 * 60_000;

/// The away entry for `place`: `Some` only while this Mac is on another network (remote) with
/// the ALab LAN reading away (learned, and this is not it), and has been there for
/// [`AWAY_ENTRY_AFTER_MS`], so a first run that has learned nothing, a place still checking, a
/// blip, home, offline (a router reboot longer than the dwell puts every Mac at home there, and
/// no person need move anything), and a remembered network that fell silent (it may be home
/// with every member asleep or off: "rejoin" would be wrong there) raise nothing.
#[must_use]
pub fn away_entry(place: &Place, words: &Words<'_>) -> Option<AwayEntry> {
    let (Alab::Away { since_ms }, Where::Remote(kind)) = (place.alab, place.at) else {
        return None;
    };
    if place.once_alab {
        return None;
    }
    let settled = place.since_before
        || place
            .since_ms
            .is_some_and(|ms| words.now_ms.saturating_sub(ms) >= AWAY_ENTRY_AFTER_MS);
    if !settled {
        return None;
    }
    let kind = kind_words(kind);
    let since = since_ms.map_or_else(String::new, |ms| format!(" since {}", words.clock(ms)));
    Some(AwayEntry {
        what: "Away from the ALab LAN",
        why: "this Mac is on another network, and only a person moves it back",
        step: "Rejoin the ALab LAN to reach the members again",
        more: vec![
            format!("{kind}{since} · {}", internet_short(place)),
            "secrets, fleet and Commons work from what this Mac holds and catch up when it is back"
                .to_owned(),
        ],
        log: format!("network away kind={}", place.kind.map_or("-", Kind::word)),
    })
}

/// The aterm.log line when this Mac is back on the ALab LAN after being away since
/// `away_since_ms` on a network of `kind`: `network: back on the ALab LAN after 2h26m away
/// (phone hotspot)`.
#[must_use]
pub fn back_line(away_since_ms: Option<u64>, kind: Option<Kind>, now_ms: u64) -> String {
    let after = away_since_ms.map_or_else(String::new, |since| {
        let minutes = now_ms.saturating_sub(since) / 60_000;
        if minutes >= 60 {
            format!(" after {}h{:02}m away", minutes / 60, minutes % 60)
        } else {
            format!(" after {minutes}m away")
        }
    });
    let kind = kind.map_or_else(String::new, |kind| format!(" ({})", kind_words(kind)));
    format!("network: back on the ALab LAN{after}{kind}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place::{ComputedBy, Internet, Link, NetworkId, PathFacts, RouteFacts};
    use std::net::Ipv4Addr;

    fn place(at: Where, alab: Alab) -> Place {
        let route = RouteFacts {
            interface: "en0".to_owned(),
            address: Ipv4Addr::new(172, 20, 10, 2),
            prefix: 28,
            gateway: Some(Ipv4Addr::new(172, 20, 10, 1)),
            gateway_hw: Some(vec![2, 0, 0, 0, 0, 1]),
        };
        Place {
            at,
            kind: Some(Kind::Hotspot { by_range: false }),
            recognised: None,
            once_alab: false,
            network: NetworkId::of(&route),
            route: Some(route),
            tunnel: None,
            path: Some(PathFacts {
                usable: Some(true),
                expensive: true,
                constrained: false,
                link: Link::Wifi,
            }),
            since_ms: Some(1_800_000_000_000),
            since_before: false,
            internet: Internet {
                state: InternetState::PathUp,
                source: InternetSource::Path,
                at_ms: None,
                endpoint: None,
            },
            alab,
            reachable: 0,
            known: Some(41),
            last_answer: None,
            computed_by: ComputedBy::Keeper,
        }
    }

    /// The `status` fields are `key=value` words in one order, each value from the place:
    /// a script reads the place, its kind, its stay, the internet, the members and away-since.
    /// NEGATIVE CONTROL: printing `members=` as the reachable count alone (no `/<known>`) fails
    /// the members field.
    #[test]
    fn the_status_fields_carry_the_place_in_order() {
        let away = place(
            Where::Remote(Kind::Hotspot { by_range: false }),
            Alab::Away {
                since_ms: Some(1_799_999_000_000),
            },
        );
        let fields: Vec<(String, String)> = status_fields(&away)
            .split(' ')
            .map(|kv| {
                let (k, v) = kv.split_once('=').expect("key=value");
                (k.to_owned(), v.to_owned())
            })
            .collect();
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "net",
                "net_kind",
                "net_since_ms",
                "internet",
                "internet_ms",
                "members",
                "away_since_ms"
            ]
        );
        let value = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(value("net"), Some("remote"));
        assert_eq!(value("net_kind"), Some("hotspot"));
        assert_eq!(value("net_since_ms"), Some("1800000000000"));
        assert_eq!(value("internet"), Some("path-up"));
        assert_eq!(value("internet_ms"), Some("-"));
        assert_eq!(value("members"), Some("0/41"));
        assert_eq!(value("away_since_ms"), Some("1799999000000"));
        let unknown_keys: Vec<&str> = STATUS_UNKNOWN
            .split(' ')
            .map(|kv| kv.split_once('=').expect("key=value").0)
            .collect();
        assert_eq!(unknown_keys, keys, "the unknown line has the same keys");
        let home = place(Where::AlabLan, Alab::Here);
        assert!(status_fields(&home).contains("away_since_ms=-"));
    }

    /// The JSON object carries the axes apart: where, kind, the internet with its source, and
    /// the ALab LAN with its counts.
    #[test]
    fn the_json_fields_keep_the_two_axes_apart() {
        let away = place(
            Where::Remote(Kind::Hotspot { by_range: false }),
            Alab::Away { since_ms: Some(7) },
        );
        let fields = json_fields(&away);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("state"), Some(Field::Text("remote".to_owned())));
        assert_eq!(get("kind"), Some(Field::Text("hotspot".to_owned())));
        assert_eq!(get("expensive"), Some(Field::Flag(true)));
        let Some(Field::Object(internet)) = get("internet") else {
            panic!("internet object");
        };
        assert!(internet.contains(&("state", Field::Text("path-up".to_owned()))));
        assert!(internet.contains(&("source", Field::Text("path".to_owned()))));
        let Some(Field::Object(alab)) = get("alab") else {
            panic!("alab object");
        };
        assert!(alab.contains(&("state", Field::Text("away".to_owned()))));
        assert!(alab.contains(&("since_ms", Field::Count(7))));
        assert!(alab.contains(&("known", Field::Count(41))));
    }

    /// The away entry stands only while this Mac is on a remote network with the ALab LAN
    /// reading away, and has been there for a while: never at home, on a first run that learned
    /// nothing, while checking, offline (a router reboot past the dwell, at home), or in the
    /// first two minutes of a stay (a Wi-Fi blip); and its title and step pass the exceptions
    /// vocabulary's title rule (six words, no clause, no full stop).
    /// NEGATIVE CONTROLS: dropping the dwell (`if !settled { return None; }`) raises it for a stay
    /// a second old, and "a blip raises nothing" fails; taking any place whose ALab axis reads
    /// away (the first rule, which let `Where::Offline` through) raises it offline, and the
    /// `Offline` row fails.
    #[test]
    fn the_away_entry_stands_only_away_and_past_a_blip() {
        let since = 1_800_000_000_000;
        let at = |now_ms| Words {
            now_ms,
            zone: &|_| Some(0),
        };
        let away = place(
            Where::Remote(Kind::Hotspot { by_range: false }),
            Alab::Away {
                since_ms: Some(since - 1_000),
            },
        );
        let entry = away_entry(&away, &at(since + AWAY_ENTRY_AFTER_MS)).expect("away");
        assert!(entry.what.split_whitespace().count() <= 6 && !entry.what.ends_with('.'));
        assert!(!entry.more.is_empty());
        assert!(
            away_entry(&away, &at(since + 1_000)).is_none(),
            "a blip raises nothing"
        );
        let words = at(since + 60 * AWAY_ENTRY_AFTER_MS);
        for (where_, alab) in [
            (Where::AlabLan, Alab::Here),
            (
                Where::Remote(Kind::Hotspot { by_range: false }),
                Alab::Learning,
            ),
            (Where::Checking(Checking::Moved), Alab::Unknown),
            (
                Where::Offline,
                Alab::Away {
                    since_ms: Some(since - 1_000),
                },
            ),
        ] {
            assert!(
                away_entry(&place(where_, alab), &words).is_none(),
                "{where_:?} {alab:?}"
            );
        }
    }

    /// A remembered network that fell silent ("a network members were once on") may be home
    /// with every member asleep or off: no why, no survey note and no away entry say this Mac is
    /// elsewhere; a member on the network this Mac is on is one being away never explains.
    /// NEGATIVE CONTROL: `why_away` without its `once_alab` return fails "no why on a network
    /// members were once on".
    #[test]
    fn a_network_members_were_once_on_blames_nothing_away() {
        let words = Words {
            now_ms: 1_800_000_000_000 + 60 * AWAY_ENTRY_AFTER_MS,
            zone: &|_| Some(0),
        };
        let once = Place {
            once_alab: true,
            kind: Some(Kind::Lan),
            ..place(Where::Remote(Kind::Lan), Alab::Away { since_ms: Some(7) })
        };
        assert!(
            why_away(&once, &words).is_none(),
            "no why on a network members were once on"
        );
        assert!(survey_note(&once, &words).is_none());
        assert!(away_entry(&once, &words).is_none());
        let hotspot = place(
            Where::Remote(Kind::Hotspot { by_range: false }),
            Alab::Away { since_ms: Some(7) },
        );
        assert!(on_this_network(
            &hotspot,
            std::net::IpAddr::V4(Ipv4Addr::new(172, 20, 10, 5))
        ));
        assert!(!on_this_network(
            &hotspot,
            std::net::IpAddr::V4(Ipv4Addr::new(192, 168, 4, 9))
        ));
    }

    /// THE ON-LINK RULE judges a no-route dial by its address against the network this Mac is on
    /// now: on this network, that member's own (off the link now), a network members were once
    /// on alike; off this network, or with no route read, elsewhere; with no address, unplaced.
    /// NEGATIVE CONTROL: `on_link` counting every addressed no-route as gone fails "off this
    /// network".
    #[test]
    fn a_no_route_dial_is_judged_by_its_address_on_the_network_this_mac_is_on() {
        use std::net::IpAddr;
        let ip = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        let (here, home) = (ip(172, 20, 10, 5), ip(192, 168, 4, 9));
        let hotspot = place(
            Where::Remote(Kind::Hotspot { by_range: false }),
            Alab::Away { since_ms: Some(7) },
        );
        let gone = on_link(&hotspot, [Dial::NoRoute(Some(here)), Dial::Other]);
        assert_eq!(
            gone,
            OnLink {
                gone: 1,
                ..OnLink::default()
            },
            "a member on this hotspot with no route: its own"
        );
        assert!(gone.gone_at(&hotspot, here));
        assert!(
            !gone.gone_at(&hotspot, home),
            "off this network is not gone"
        );
        assert_eq!(
            on_link(&hotspot, [Dial::NoRoute(Some(home))]),
            OnLink {
                elsewhere: 1,
                ..OnLink::default()
            },
            "off this network"
        );
        assert_eq!(
            on_link(&hotspot, [Dial::NoRoute(None)]),
            OnLink {
                unplaced: 1,
                ..OnLink::default()
            }
        );
        let once = Place {
            once_alab: true,
            kind: Some(Kind::Lan),
            ..place(Where::Remote(Kind::Lan), Alab::Away { since_ms: Some(7) })
        };
        assert_eq!(
            on_link(&once, [Dial::NoRoute(Some(here))]).gone,
            1,
            "a network members were once on changes nothing"
        );
        let unrouted = Place {
            route: None,
            ..hotspot.clone()
        };
        assert_eq!(
            on_link(&unrouted, [Dial::NoRoute(Some(here))]).elsewhere,
            1,
            "with no route read nothing is on this network"
        );
    }

    /// A why and a survey note come only while this Mac is on a remote network: never on the
    /// ALab LAN, offline or while checking, where they would blame the wrong thing.
    #[test]
    fn the_why_and_the_survey_note_come_only_on_a_remote_network() {
        let words = Words {
            now_ms: 1_800_000_000_000,
            zone: &|_| Some(0),
        };
        let away = place(
            Where::Remote(Kind::Hotspot { by_range: false }),
            Alab::Away { since_ms: None },
        );
        assert!(why_away(&away, &words).is_some());
        assert!(survey_note(&away, &words).is_some());
        for at in [
            Where::AlabLan,
            Where::Offline,
            Where::Checking(Checking::Learning),
        ] {
            let p = place(at, Alab::Here);
            assert!(why_away(&p, &words).is_none(), "{at:?}");
            assert!(survey_note(&p, &words).is_none(), "{at:?}");
        }
    }
}
