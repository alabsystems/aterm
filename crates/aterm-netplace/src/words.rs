// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE WORDING of the place. `status`, `sys`, `fleet peers`, `survey`, `secrets status`,
//! `members requests`, `doctor`, the band and the menu bar all print from here, so no two of them
//! can say different things about where this Mac is (the window's away entry, [`away_entry`],
//! included). The machine fields ([`status_fields`], [`json_fields`]) are an interface; the
//! sentences are for a person and may be reworded here, once, for every reporter.
//!
//! **Who is up is not the place's to say.** The place counts the members that answered in a stay
//! only as evidence for where this Mac is ([`Place::heard`]); no reporter words that count. The
//! lab's seats, up and in step, are presence's (one-mesh §4.12): a reporter that read the node
//! service's view hands them in as [`Seats`], and [`status_line`] and [`seats_short`] word them.

use crate::place::{
    Alab, Checking, InternetSource, InternetState, Kind, Lane, Place, Recognised, Where,
};
use crate::time::Clock;

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

/// The lab's seats as presence reads them (one-mesh §4.12): plain data that a reporter that read
/// the node service's view hands in, since this crate sits below the mesh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seats {
    /// The members, counted as seats, this Mac included.
    pub members: usize,
    /// Of them, those up: their presence row advanced within 90 seconds. `None` where no
    /// presence was read; the reader says why beside the line.
    pub up: Option<usize>,
    /// The members whose build is below what the lab needs now, `(label, build)`, in label order.
    pub needs_update: Vec<(String, String)>,
    /// How the up members' tables stand against this Mac's; `None` where no presence was read.
    pub in_step: Option<InStep>,
    /// The membership log's records this Mac holds, and their bound; `None` where the log was not
    /// read.
    pub log: Option<(u64, u64)>,
}

/// How the other up members' presence rows stand against this Mac's own (§4.12 "in step").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InStep {
    /// Those whose rows hold the membership log and the tables this Mac's row holds.
    pub with: usize,
    /// The up members, this Mac aside.
    pub of: usize,
    /// How long ago, at least, the oldest record the slowest of those behind lacks reached this
    /// Mac (ms); `None` where none is behind.
    pub slowest_ms: Option<u64>,
    /// Those whose rows hold what none of this Mac's recent rows held: records this Mac lacks, or
    /// a lag older than it remembers.
    pub apart: usize,
}

/// `ms` as an age for a person: `40 s`, `12 min`, `3 h`, `2 d`.
#[must_use]
pub fn age_words(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3_600 => format!("{} min", s / 60),
        3_600..86_400 => format!("{} h", s / 3_600),
        _ => format!("{} d", s / 86_400),
    }
}

/// `n` with its thousands grouped: `16,384`.
#[must_use]
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A build's release for a person: `0.147` of `0.147.0+g1a2b3c4d`; a build this cannot read as
/// itself.
fn release_words(build: &str) -> &str {
    let core = build.split_once('+').map_or(build, |(core, _)| core);
    let mut dots = core.match_indices('.');
    match (dots.next(), dots.next()) {
        (Some(_), Some((second, _))) => &core[..second],
        _ => build,
    }
}

/// `52 members`, `1 member`.
fn members_count(n: usize) -> String {
    format!("{n} member{}", if n == 1 { "" } else { "s" })
}

/// THE STATUS LINE of the lab (§4.12): `52 members · 47 up · 3 need update (m11 0.147, m12 0.146,
/// m106 0.148) · tables in step with 46 of 47, slowest 40 s behind · log 4,169 of 16,384`.
#[must_use]
pub fn status_line(seats: &Seats) -> String {
    use std::fmt::Write as _;
    let mut out = members_count(seats.members);
    match seats.up {
        Some(up) => {
            let _ = write!(out, " · {up} up");
        }
        None => out.push_str(" · up not read"),
    }
    if !seats.needs_update.is_empty() {
        let n = seats.needs_update.len();
        let _ = write!(out, " · {n} need{} update (", if n == 1 { "s" } else { "" });
        for (i, (label, build)) in seats.needs_update.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "{label} {}", release_words(build));
        }
        out.push(')');
    }
    if let Some(step) = seats.in_step {
        if step.of == 0 {
            out.push_str(" · no other member up");
        } else {
            let _ = write!(out, " · tables in step with {} of {}", step.with, step.of);
            if let Some(ms) = step.slowest_ms {
                let _ = write!(out, ", slowest {} behind", age_words(ms));
            }
            if step.apart > 0 {
                let _ = write!(out, ", {} apart", step.apart);
            }
        }
    }
    if let Some((held, bound)) = seats.log {
        let _ = write!(out, " · log {} of {}", grouped(held), grouped(bound));
    }
    out
}

/// The seats in a few words: `47 up of 52 members`, or `52 members` where no presence was read.
#[must_use]
pub fn seats_short(seats: &Seats) -> String {
    let members = members_count(seats.members);
    match seats.up {
        Some(up) => format!("{up} up of {members}"),
        None => members,
    }
}

/// The ALab LAN in a few words where the place has more to say of it than its own word: nothing
/// at home.
#[must_use]
pub fn alab_short(place: &Place, words: &Words<'_>) -> Option<String> {
    match place.alab {
        Alab::Here => None,
        Alab::Away { since_ms: Some(ms) } => {
            Some(format!("ALab LAN: away since {}", words.clock(ms)))
        }
        Alab::Away { since_ms: None } => Some("ALab LAN: away".to_owned()),
        Alab::Learning => Some("ALab LAN not learned yet".to_owned()),
        Alab::Unknown => Some("ALab LAN: not known while checking".to_owned()),
        Alab::NoFleet => Some(format!(
            "no ALab LAN to learn (the fleet book knows fewer than {} members)",
            crate::place::LEARN_AT
        )),
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
/// internet ok · ALab LAN: away since 11:14`, or at home `network: ALab LAN · internet ok`.
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
        "{}{since} · {}",
        where_words_cow(place),
        internet_short(place)
    );
    if let Some(alab) = alab_short(place, words) {
        let _ = write!(out, " · {alab}");
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
    if let Some(alab) = alab_short(place, words) {
        out.push_str(" · ");
        out.push_str(&alab);
        if let Some(last) = last_answer_words(place, words) {
            let _ = write!(out, " ({last})");
        }
    }
    out
}

/// THE `status` FIELDS: `net=<alab-lan|remote|offline|checking> net_kind=<hotspot|lan|cellular|
/// vpn|-> net_since_ms=<unix ms|-> internet=<ok|down|path-up|unknown> internet_ms=<unix ms|->
/// members=<up>/<members>|- away_since_ms=<unix ms|->`, in this order: `members=` is the seats
/// presence reads up of the lab's members, `-` where the reporter read no presence.
#[must_use]
pub fn status_fields(place: &Place, seats: Option<&Seats>) -> String {
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
    match seats.and_then(|seats| Some((seats.up?, seats.members))) {
        Some((up, members)) => {
            let _ = write!(out, "{up}/{members}");
        }
        None => out.push('-'),
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

/// The band's summary and its short form: `Away from ALab LAN · phone hotspot` and
/// `Away · hotspot`, or at home `ALab LAN` and `ALab LAN`.
#[must_use]
pub fn band(place: &Place) -> (String, String) {
    match place.at {
        Where::AlabLan => ("ALab LAN".to_owned(), "ALab LAN".to_owned()),
        Where::Remote(kind) if matches!(place.alab, Alab::Away { .. }) => (
            format!("Away from ALab LAN · {}", kind_words(kind)),
            format!("Away · {}", kind.word()),
        ),
        Where::Remote(kind) => (
            format!("Remote · {}", kind_words(kind)),
            format!("Remote · {}", kind.word()),
        ),
        Where::Offline => ("Offline".to_owned(), "Offline".to_owned()),
        Where::Network(kind) => (where_words(place), kind.word().to_owned()),
        Where::Checking(_) => ("Checking the network".to_owned(), "Checking".to_owned()),
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

/// The away-entry fact the follower compares, independent of formatted detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AwayKey {
    /// The existing log's kind word, or `-` when unknown. Both hotspot variants say `hotspot`.
    pub kind_word: &'static str,
    /// When the ALab axis says this Mac went away, which can precede its current stay.
    pub since_ms: Option<u64>,
}

/// The away-entry key and its next time-only dwell change with the place held fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AwayProjection {
    /// The key at the supplied time.
    pub key: Option<AwayKey>,
    /// The first future change of this key; `None` when no forward change is representable.
    /// External place changes and clock rollback invalidate the projection.
    pub next_change_ms: Option<u64>,
}

fn away_at<const TRACK: bool>(place: &Place, clock: &Clock<TRACK>) -> Option<(AwayKey, Kind)> {
    let (Alab::Away { since_ms }, Where::Remote(kind)) = (place.alab, place.at) else {
        return None;
    };
    if place.once_alab {
        return None;
    }
    let settled = place.since_before
        || place
            .since_ms
            .is_some_and(|ms| clock.at_least(ms, AWAY_ENTRY_AFTER_MS));
    if !settled {
        return None;
    }
    Some((
        AwayKey {
            kind_word: place.kind.map_or("-", Kind::word),
            since_ms,
        },
        kind,
    ))
}

/// The follower's away fact, using the same eligibility decision as [`away_entry`].
/// No detailed wording or local-zone callback is evaluated.
#[must_use]
pub fn away_key(place: &Place, now_ms: u64) -> Option<AwayKey> {
    away_at(place, &Clock::<false>::at(now_ms)).map(|(key, _)| key)
}

/// Project the follower key's dwell change, independently of [`crate::step_with_deadline`].
/// This reads no clock and leaves detailed wording's day/zone invalidation separate.
#[must_use]
pub fn away_key_with_deadline(place: &Place, now_ms: u64) -> AwayProjection {
    let clock = Clock::<true>::at(now_ms);
    let key = away_at(place, &clock).map(|(key, _)| key);
    AwayProjection {
        key,
        next_change_ms: clock.next(),
    }
}

/// The away entry for `place`: `Some` only while this Mac is on another network (remote) with
/// the ALab LAN reading away (learned, and this is not it), and has been there for
/// [`AWAY_ENTRY_AFTER_MS`], so a first run that has learned nothing, a place still checking, a
/// blip, home, offline (a router reboot longer than the dwell puts every Mac at home there, and
/// no person need move anything), and a remembered network that fell silent (it may be home
/// with every member asleep or off: "rejoin" would be wrong there) raise nothing.
#[must_use]
pub fn away_entry(place: &Place, words: &Words<'_>) -> Option<AwayEntry> {
    let (key, kind) = away_at(place, &Clock::<false>::at(words.now_ms))?;
    let kind = kind_words(kind);
    let since = key
        .since_ms
        .map_or_else(String::new, |ms| format!(" since {}", words.clock(ms)));
    Some(AwayEntry {
        what: "Away from the ALab LAN",
        why: "this Mac is on another network, and only a person moves it back",
        step: "Rejoin the ALab LAN to reach the members again",
        more: vec![
            format!("{kind}{since} · {}", internet_short(place)),
            "secrets, fleet and Commons work from what this Mac holds and catch up when it is back"
                .to_owned(),
        ],
        log: format!("network away kind={}", key.kind_word),
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
#[path = "words_away_tests.rs"]
mod away_deadline_tests;

#[cfg(test)]
#[path = "words_tests.rs"]
mod tests;
