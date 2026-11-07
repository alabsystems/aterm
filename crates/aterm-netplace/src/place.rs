// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The place itself, pure: the facts read off this Mac ([`Facts`]), the evidence its lanes noted
//! ([`Evidence`], [`Book`]), the keeper's memory ([`Record`]), and [`step`], which turns them into
//! this Mac's [`Place`] and the record's next state. No clock, no file, no platform: every input
//! is handed in, so a reader and the keeper that hold the same files reach the same place.
//!
//! The rules, each with its reason:
//! - **The ALab LAN is proved by members, never assumed.** A network becomes one this Mac
//!   remembers as the ALab LAN when [`LEARN_AT`] distinct authenticated members answered within
//!   one stay on it ([`Evidence`] holds only proven answers), the fleet book knows at least that
//!   many, the network has a strong id, and it is no phone hotspot or cellular link. A browse
//!   row, a beacon, a join request or this Mac's own vault server is never evidence.
//! - **A remembered network that falls silent is not home.** After [`SILENT_ROUNDS`] fleet rounds
//!   in the stay that dialled members and met only silence, it reads remote ("a network members
//!   were once on"): a hotel or travel router where three Macs once sat together does not stay
//!   "home". A round a sandbox or firewall here refused, or a host refused, is not silence
//!   ([`Round::blocked`]).
//! - **No fleet to learn from, no ALab axis.** With nothing remembered and a fleet book of fewer
//!   than [`LEARN_AT`] members (or none: a Mac in no fleet), nothing can ever be learned, so the
//!   place says the network from the facts alone ([`Where::Network`], the ALab axis
//!   [`Alab::NoFleet`]) rather than `checking` for ever.
//! - **Nothing learned yet is not away.** With no remembered network (the first run after the
//!   update) the place reads `checking`, the ALab axis `learning`, and nothing is raised; the
//!   fleet book's addresses seed the first stay where at least [`LEARN_AT`] members answered from
//!   this subnet in the last day while this Mac was on this very network (its gateway's hardware
//!   address too: [`BookMember::network`]), without remembering anything.
//! - **The internet is measured or says it is not.** Only a note from this stay counts
//!   ([`InternetNote`]), and only for [`INTERNET_FRESH_MS`]: an `ok` measured at home never
//!   stands on the hotspot. With none, the path monitor gives `path-up` or `down`, never `ok`.
//! - **One id rule.** A network is its default route's gateway hardware address plus its subnet
//!   ([`NetworkId::of`]). Before ARP resolves the gateway the stay reads `checking (moved)` for
//!   [`SETTLE_MS`]; then it takes a weak id (subnet plus gateway address) that is never
//!   remembered and never matches a remembered network.

use std::net::{IpAddr, Ipv4Addr};

use crate::time::Clock;

/// Distinct authenticated members that must answer within one stay before a network is
/// remembered as the ALab LAN, and that the fleet book must know: a fleet of one or two Macs, or
/// two Macs on a café LAN, never makes a network "home".
pub const LEARN_AT: usize = 3;

/// The most networks remembered as the ALab LAN; the one confirmed longest ago goes first.
pub const REMEMBERED_MAX: usize = 16;

/// The most stays the record keeps, the current one last.
pub const STAYS_MAX: usize = 32;

/// How long after a move the place reads `checking` while the gateway's hardware address is
/// unread (the keeper looks again at 2, 5 and 15 s); after it the stay takes a weak id.
pub const SETTLE_MS: u64 = 22_000;

/// Fleet rounds in one stay that dialled members and heard none before a remembered network,
/// or a seeded first stay, stops reading as the ALab LAN.
pub const SILENT_ROUNDS: usize = 2;

/// How long a measured internet answer in this stay stands, `ok` or `down`, before the place says
/// what the path says again (`path-up`, `down`): an `ok` from a turn long ago says nothing of an
/// internet that dropped since while the LAN stayed up, and a `down` measured as a router came
/// back says nothing of its WAN once that is up too.
pub const INTERNET_FRESH_MS: u64 = 15 * 60_000;

/// How far past the clock a noted time may be and still be read (a clock stepped back).
pub const AHEAD_MAX_MS: u64 = 60_000;

/// How recent a fleet book answer must be to seed the first stay ([`Book`]).
pub const SEED_WITHIN_MS: u64 = 24 * 60 * 60_000;

/// How long a remote stay must have lasted for a move that leaves it to count as this Mac
/// returning alone ([`Record::lone_return`]): well past a router or access point reboot (one to
/// five minutes), during which Macs fall back to another network they know (a guest or extender
/// network, a neighbour's open one, an auto-joined hotspot) and all come back at once. A person
/// who carried the Mac off and back stayed longer; one back sooner waits only the few seconds of
/// its herd spread.
pub const LONE_RETURN_AFTER_MS: u64 = 10 * 60_000;

/// How recently a current stay must have begun to be the one a move just arrived in
/// ([`Record::lone_return`]): the keeper reads a new network within its settle (22 s) and a
/// process's watch reads the record within seconds after.
pub const LONE_ARRIVAL_MS: u64 = 2 * 60_000;

/// How stale a remembered network's `confirmed_ms` must be before members answering on it
/// write it again: the keeper writes its record only when it changed, and a confirmation at
/// every look (every 10 s at home with no member heard yet) would rewrite it at every look.
/// The time orders which remembered network goes first past [`REMEMBERED_MAX`], which an hour
/// does not change.
pub const CONFIRM_EVERY_MS: u64 = 60 * 60_000;

/// How soon after the keeper starts a stay it opens counts as its first look ([`observe`]).
const FIRST_LOOK_MS: u64 = 5_000;

/// An iPhone Personal Hotspot's subnet, `172.20.10.0/28`: a hint only where no path monitor
/// says whether the path is expensive.
const HOTSPOT_SUBNET: (Ipv4Addr, u8) = (Ipv4Addr::new(172, 20, 10, 0), 28);

/// The kind of interface the path uses, as the path monitor names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Wi-Fi.
    Wifi,
    /// Ethernet.
    Wired,
    /// A cellular modem.
    Cellular,
    /// Loopback.
    Loopback,
    /// Anything else: a tunnel, a bridge, a tethering link the framework does not name.
    Other,
}

impl Link {
    /// The word for this link.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Wired => "wired",
            Self::Cellular => "cellular",
            Self::Loopback => "loopback",
            Self::Other => "other",
        }
    }

    /// The link a [`Self::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [
            Self::Wifi,
            Self::Wired,
            Self::Cellular,
            Self::Loopback,
            Self::Other,
        ]
        .into_iter()
        .find(|link| link.word() == word)
    }
}

/// What the path monitor last said ([`Facts::path`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathFacts {
    /// Whether the path can carry traffic; `None` for an invalid path.
    pub usable: Option<bool>,
    /// Whether Apple counts the path's interface as expensive: a cellular link or a Personal
    /// Hotspot.
    pub expensive: bool,
    /// Whether the interface is in Low Data Mode.
    pub constrained: bool,
    /// The interface's kind.
    pub link: Link,
}

/// The network traffic leaves this Mac on: its default route's interface, this Mac's address
/// and prefix there, and the gateway with its hardware address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteFacts {
    /// The interface (`en0`).
    pub interface: String,
    /// This Mac's IPv4 address on it.
    pub address: Ipv4Addr,
    /// That address's prefix length.
    pub prefix: u8,
    /// The gateway; `None` for a route straight onto a link.
    pub gateway: Option<Ipv4Addr>,
    /// The gateway's hardware address; `None` before ARP resolves it.
    pub gateway_hw: Option<Vec<u8>>,
}

impl RouteFacts {
    /// The subnet's network address and prefix.
    #[must_use]
    pub fn subnet(&self) -> (Ipv4Addr, u8) {
        (network_of(self.address, self.prefix), self.prefix)
    }

    /// Calls `f` with the subnet as `a.b.c.d/n` without allocating on the heap.
    pub(crate) fn with_subnet_words<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        use std::io::Write as _;
        let (net, prefix) = self.subnet();
        let mut buf = [0u8; 24];
        let mut cursor = std::io::Cursor::new(&mut buf[..]);
        let _ = write!(cursor, "{net}/{prefix}");
        let len = usize::try_from(cursor.position()).unwrap_or(0);
        let text = std::str::from_utf8(&buf[..len]).unwrap_or("");
        f(text)
    }

    /// The subnet as `a.b.c.d/n`.
    #[must_use]
    pub fn subnet_words(&self) -> String {
        self.with_subnet_words(str::to_owned)
    }

    /// Whether `addr` lies on this route's subnet.
    #[must_use]
    pub fn on_link(&self, addr: IpAddr) -> bool {
        match addr {
            IpAddr::V4(v4) => on_link_v4(self.address, self.prefix, v4),
            IpAddr::V6(v6) => v6
                .to_ipv4_mapped()
                .is_some_and(|v4| on_link_v4(self.address, self.prefix, v4)),
        }
    }
}

pub(crate) fn on_link_v4(local: Ipv4Addr, prefix: u8, peer: Ipv4Addr) -> bool {
    network_of(peer, prefix) == network_of(local, prefix)
}

/// `addr` with its host bits past `prefix` cleared.
fn network_of(addr: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let bits = u32::from(addr);
    let mask = match prefix {
        0 => 0,
        p if p >= 32 => u32::MAX,
        p => u32::MAX << (32 - u32::from(p)),
    };
    Ipv4Addr::from(bits & mask)
}

/// Everything read off this Mac, with no packet, no child process and no question asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The network traffic leaves on: the primary default route's, or, where a tunnel holds
    /// the primary, the scoped physical default under it. `None` with no IPv4 default route.
    pub route: Option<RouteFacts>,
    /// The tunnel interface (`utun4`) holding the primary default route, where one does.
    pub tunnel: Option<String>,
    /// The path monitor's last reading; `None` where no monitor runs (Linux, or Network.framework
    /// could not be opened).
    pub path: Option<PathFacts>,
}

impl Facts {
    /// Whether the path is a Personal Hotspot or cellular link (`expensive`), or, with no path
    /// monitor, whether the address lies in an iPhone hotspot's range.
    fn hotspot_or_cellular(&self) -> Option<Kind> {
        match self.path {
            Some(path) if path.link == Link::Cellular => Some(Kind::Cellular),
            Some(path) if path.expensive => Some(Kind::Hotspot { by_range: false }),
            Some(_) => None,
            None => self
                .route
                .as_ref()
                .filter(|route| route.prefix >= HOTSPOT_SUBNET.1)
                .filter(|route| network_of(route.address, HOTSPOT_SUBNET.1) == HOTSPOT_SUBNET.0)
                .map(|_| Kind::Hotspot { by_range: true }),
        }
    }

    /// The kind of network this is, whatever its place.
    fn kind(&self) -> Kind {
        self.hotspot_or_cellular()
            .unwrap_or(if self.tunnel.is_some() {
                Kind::Vpn
            } else {
                Kind::Lan
            })
    }
}

const fn hex16_bytes(val: u64) -> [u8; 16] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = val.to_be_bytes();
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 8 {
        let b = bytes[i];
        out[2 * i] = DIGITS[(b >> 4) as usize];
        out[2 * i + 1] = DIGITS[(b & 0x0f) as usize];
        i += 1;
    }
    out
}

const fn prefixed_hex16(prefix: u8, val: u64) -> [u8; 17] {
    let hex = hex16_bytes(val);
    let mut out = [0u8; 17];
    out[0] = prefix;
    let mut i = 0;
    while i < 16 {
        out[i + 1] = hex[i];
        i += 1;
    }
    out
}

/// A network's id: a short hash of its gateway's hardware address and its subnet (strong), or,
/// before ARP gives that address, of its subnet and gateway address (weak). The hash is for tidy
/// display, not secrecy.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetworkId(String);

impl NetworkId {
    fn strong_bytes(route: &RouteFacts) -> Option<[u8; 17]> {
        let hw = route.gateway_hw.as_deref()?;
        let mut h = aterm_hash::Fnv1a64::new();
        h.write(b"aterm-netplace/1 strong\0");
        h.write(hw);
        h.write_u8(0);
        route.with_subnet_words(|subnet| h.write(subnet.as_bytes()));
        Some(prefixed_hex16(b'n', h.finish()))
    }

    /// The strong id of `route`: `None` before its gateway's hardware address is read.
    #[must_use]
    pub fn of(route: &RouteFacts) -> Option<Self> {
        let buf = Self::strong_bytes(route)?;
        Some(Self(
            std::str::from_utf8(&buf).unwrap_or_default().to_owned(),
        ))
    }

    /// The weak id of `route`: its subnet and gateway address, never remembered.
    #[must_use]
    pub fn weak(route: &RouteFacts) -> Self {
        let mut h = aterm_hash::Fnv1a64::new();
        h.write(b"aterm-netplace/1 weak\0");
        route.with_subnet_words(|subnet| h.write(subnet.as_bytes()));
        h.write_u8(0);
        if let Some(gateway) = route.gateway {
            h.write(&gateway.octets());
        }
        let buf = prefixed_hex16(b'w', h.finish());
        Self(std::str::from_utf8(&buf).unwrap_or_default().to_owned())
    }

    /// Whether this id came from the gateway's hardware address.
    #[must_use]
    pub fn is_strong(&self) -> bool {
        self.0.starts_with('n')
    }

    /// Its first eight hex digits, as the words show it.
    #[must_use]
    pub fn short(&self) -> &str {
        self.0.get(1..9).unwrap_or(&self.0)
    }

    /// The id as written in the record.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// An id the record wrote ([`Self::as_str`]); `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (kind, hex) = text.split_at_checked(1)?;
        ((kind == "n" || kind == "w")
            && hex.len() == 16
            && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| Self(text.to_owned()))
    }
}

pub(crate) fn member_key_bytes(lane: Lane, id: &str) -> [u8; 16] {
    let mut h = aterm_hash::Fnv1a64::new();
    h.write(lane.word().as_bytes());
    h.write_u8(0);
    h.write(id.as_bytes());
    hex16_bytes(h.finish())
}

/// A member's key in the evidence: a short hash of its lane and id, so the file holds no node id.
#[must_use]
pub fn member_key(lane: Lane, id: &str) -> String {
    let buf = member_key_bytes(lane, id);
    std::str::from_utf8(&buf).unwrap_or_default().to_owned()
}

/// The kind of a network this Mac is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A Personal Hotspot; `by_range` where only its address range says so (no path monitor).
    Hotspot {
        /// Read from `172.20.10.0/28` alone.
        by_range: bool,
    },
    /// A LAN or Wi-Fi network.
    Lan,
    /// A cellular link.
    Cellular,
    /// A tunnel holds the default route.
    Vpn,
}

impl Kind {
    /// The status word: `hotspot`, `lan`, `cellular` or `vpn`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Hotspot { .. } => "hotspot",
            Self::Lan => "lan",
            Self::Cellular => "cellular",
            Self::Vpn => "vpn",
        }
    }
}

/// Why the place reads `checking`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checking {
    /// This Mac moved and the new network's gateway is not read yet.
    Moved,
    /// No network has been learned as the ALab LAN on this Mac yet.
    Learning,
    /// The gateway's hardware address stayed unread, so the network has only a weak id.
    Unidentified,
    /// The path carries traffic but there is no IPv4 default route.
    NoIpv4,
}

/// Where this Mac is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    /// On the ALab LAN.
    AlabLan,
    /// On another network of this kind.
    Remote(Kind),
    /// No network.
    Offline,
    /// Not known yet, for this reason.
    Checking(Checking),
    /// On a network of this kind, with no ALab LAN to compare it with: nothing was ever
    /// learned and the fleet book holds fewer than [`LEARN_AT`] members (or there is none), so
    /// nothing here can be learned.
    Network(Kind),
}

impl Where {
    /// The status word: `alab-lan`, `remote`, `offline`, `checking` or `network`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::AlabLan => "alab-lan",
            Self::Remote(_) => "remote",
            Self::Offline => "offline",
            Self::Checking(_) => "checking",
            Self::Network(_) => "network",
        }
    }
}

/// An evidence lane: where an authenticated member answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lane {
    /// The fleet: a proven node-keyed answer (heartbeat, a proven read).
    Fleet,
    /// Commons: a verified contact.
    Commons,
}

impl Lane {
    /// The evidence file's word for this lane.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fleet => "fleet",
            Self::Commons => "commons",
        }
    }

    /// The lane a [`Self::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "fleet" => Some(Self::Fleet),
            "commons" => Some(Self::Commons),
            _ => None,
        }
    }
}

/// One authenticated member answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// Its lane.
    pub lane: Lane,
    /// The member ([`member_key`]).
    pub member: String,
    /// The address it answered from.
    pub addr: Option<IpAddr>,
    /// The local address the route to it took when it was noted (`route_source_ipv4`), which
    /// says whether it came over this network's interface; `None` where unread.
    pub via: Option<Ipv4Addr>,
    /// The interface that held [`Self::via`] when it was noted (`en0`); `None` where unread.
    pub iface: Option<String>,
    /// The network (strong id) it was heard on: the one this Mac's traffic left on both when the
    /// round that reached it began and when it was noted ([`crate::heard::note_answer`]); `None`
    /// where either was unread (the gateway's hardware address not read yet) or they differ (this
    /// Mac moved under the round), and for a Commons contact. The stay alone cannot say it: a
    /// stay continues across a network whose gateway's hardware address goes unread.
    pub network: Option<NetworkId>,
    /// The round that heard it ran under this Mac's vault's membership log, and so dialled only
    /// the members that log and the vault's deny list let it: `false` for a round with neither
    /// (an isolated job's, a test's, a lab's scratch state's or CI's, which reads no vault and
    /// denies no one) and for a Commons contact. Only such an answer is evidence for the vault's
    /// automatic admission, whatever state root it was noted under.
    pub logged: bool,
    /// When the dial that reached it began (unix ms).
    pub began_ms: u64,
    /// When it answered (unix ms).
    pub at_ms: u64,
}

/// One finished fleet round: how many members answered it, and how many of its dials met
/// something other than silence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Round {
    /// Its lane.
    pub lane: Lane,
    /// Members that answered.
    pub answered: usize,
    /// Dials that met something other than silence: a host at the address answered (a refusal,
    /// a handshake or proof that failed, a heartbeat it declined), or this Mac's own way onto this
    /// network refused the connect (no network, or not permitted, to an address on this network:
    /// a sandbox, a firewall). Such a round says nothing of whether members are here.
    pub blocked: usize,
    /// When it began (unix ms).
    pub began_ms: u64,
    /// When it ended (unix ms).
    pub at_ms: u64,
}

/// Where an internet answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternetSource {
    /// The harness's API reach probe.
    ApiProbe,
    /// A supervised session's turn ended on a real reply from its API.
    TurnReply,
    /// The path monitor alone (never `ok`).
    Path,
    /// Nothing says.
    Nothing,
}

impl InternetSource {
    /// The word for this source.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::ApiProbe => "api-probe",
            Self::TurnReply => "turn-reply",
            Self::Path => "path",
            Self::Nothing => "none",
        }
    }

    /// The source a [`Self::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::ApiProbe, Self::TurnReply, Self::Path, Self::Nothing]
            .into_iter()
            .find(|source| source.word() == word)
    }
}

/// One measured internet answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternetNote {
    /// Whether the endpoint answered.
    pub ok: bool,
    /// What measured it.
    pub source: InternetSource,
    /// When (unix ms).
    pub at_ms: u64,
    /// The endpoint (`api.anthropic.com`).
    pub endpoint: String,
}

/// What the lanes noted ([`crate::heard`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    /// Authenticated member answers.
    pub answers: Vec<Answer>,
    /// Finished fleet rounds.
    pub rounds: Vec<Round>,
    /// Internet answers.
    pub internet: Vec<InternetNote>,
}

/// One member of the fleet book as the fleet lane last wrote it ([`Book`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookMember {
    /// The member ([`member_key`], lane fleet).
    pub member: String,
    /// Its address in the book (rewritten on each proven answer).
    pub addr: Option<IpAddr>,
    /// When it last answered this Mac (unix ms).
    pub answered_ms: Option<u64>,
    /// The network (strong id) this Mac was on when that answer was booked; `None` where it was
    /// not read. A hotel or office numbered like home (`192.168.1.0/24`) is another network.
    pub network: Option<NetworkId>,
}

/// The fleet book's admitted members, not this Mac, as the fleet lane last wrote them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    /// The members.
    pub members: Vec<BookMember>,
}

/// What a stay was, as the keeper last decided it while it was current.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StayAt {
    /// The ALab LAN.
    AlabLan,
    /// A remote network.
    Remote,
    /// No network.
    Offline,
    /// Not known.
    Checking,
}

impl StayAt {
    fn of(at: Where) -> Self {
        match at {
            Where::AlabLan => Self::AlabLan,
            Where::Remote(_) | Where::Network(_) => Self::Remote,
            Where::Offline => Self::Offline,
            Where::Checking(_) => Self::Checking,
        }
    }

    /// The record's word for it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::AlabLan => "alab-lan",
            Self::Remote => "remote",
            Self::Offline => "offline",
            Self::Checking => "checking",
        }
    }

    /// The value a [`Self::word`] names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::AlabLan, Self::Remote, Self::Offline, Self::Checking]
            .into_iter()
            .find(|at| at.word() == word)
    }
}

/// One stay: a stretch on one network, or offline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stay {
    /// When it began (unix ms).
    pub start_ms: u64,
    /// When it ended; `None` for the current stay.
    pub end_ms: Option<u64>,
    /// What it was.
    pub at: StayAt,
    /// Its network; `None` while offline, or before its gateway was read.
    pub network: Option<NetworkId>,
    /// Its subnet (`a.b.c.d/n`); `None` offline.
    pub subnet: Option<String>,
    /// Its gateway address.
    pub gateway: Option<Ipv4Addr>,
    /// It began before the keeper that opened it started, so its start is that keeper's start
    /// and the words say "since before".
    pub from_start: bool,
}

impl Stay {
    /// Whether `facts` continue this stay: the same network (or both offline), or the same
    /// subnet and gateway before or after the gateway's hardware address is read.
    pub(crate) fn continues(&self, facts: &Facts) -> bool {
        let Some(route) = facts.route.as_ref() else {
            return self.subnet.is_none();
        };
        match (NetworkId::strong_bytes(route), &self.network) {
            (Some(id), Some(network)) if network.is_strong() => network.0.as_bytes() == id,
            _ => {
                self.gateway == route.gateway
                    && route.with_subnet_words(|subnet| self.subnet.as_deref() == Some(subnet))
            }
        }
    }
}

/// A network remembered as the ALab LAN.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    /// Its id (always strong).
    pub network: NetworkId,
    /// When it was learned (unix ms).
    pub learned_ms: u64,
    /// When members last answered on it (unix ms).
    pub confirmed_ms: u64,
}

/// The keeper's memory ([`crate::record`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// The stays, oldest first, the current one last; at most [`STAYS_MAX`].
    pub stays: Vec<Stay>,
    /// Networks remembered as the ALab LAN; at most [`REMEMBERED_MAX`].
    pub remembered: Vec<Known>,
}

impl Record {
    /// The current stay.
    #[must_use]
    pub fn current(&self) -> Option<&Stay> {
        self.stays.last().filter(|stay| stay.end_ms.is_none())
    }

    /// Whether `network` is remembered as the ALab LAN.
    #[must_use]
    pub fn remembers(&self, network: &NetworkId) -> bool {
        network.is_strong()
            && self
                .remembered
                .iter()
                .any(|known| known.network == *network)
    }

    /// Whether a network move heard at `now` is this Mac returning alone, so it waits out no
    /// herd spread: the newest stay that was neither offline nor checking, passing over a current
    /// stay that began within [`LONE_ARRIVAL_MS`] of `now` (the one this move arrived in, which
    /// the keeper may have recorded before the move was told: [`crate::moves`]), was a remote stay
    /// that lasted longer than [`LONE_RETURN_AFTER_MS`]. A router or access point reboot moves
    /// every Mac from the ALab LAN through offline or checking back onto it, and the newest
    /// settled stay before the arrival is then the ALab LAN's, so the herd spread stays.
    #[must_use]
    pub fn lone_return(&self, now: u64) -> bool {
        self.stays
            .iter()
            .rev()
            .filter(|stay| {
                stay.end_ms.is_some() || now.saturating_sub(stay.start_ms) > LONE_ARRIVAL_MS
            })
            .find(|stay| matches!(stay.at, StayAt::AlabLan | StayAt::Remote))
            .is_some_and(|stay| {
                stay.at == StayAt::Remote
                    && stay.end_ms.unwrap_or(now).saturating_sub(stay.start_ms)
                        > LONE_RETURN_AFTER_MS
            })
    }
}

/// The internet axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternetState {
    /// An endpoint answered in this stay.
    Ok,
    /// The measured endpoint did not answer, or the path cannot carry traffic.
    Down,
    /// The path carries traffic; nothing measured an endpoint in this stay.
    PathUp,
    /// Nothing says.
    Unknown,
}

impl InternetState {
    /// The status word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Down => "down",
            Self::PathUp => "path-up",
            Self::Unknown => "unknown",
        }
    }
}

/// The internet axis with its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Internet {
    /// What it reads.
    pub state: InternetState,
    /// How it is known.
    pub source: InternetSource,
    /// When it was measured (unix ms), for a measured state.
    pub at_ms: Option<u64>,
    /// The endpoint measured.
    pub endpoint: Option<String>,
}

/// The ALab LAN axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alab {
    /// On it.
    Here,
    /// Away from it, since this time (unix ms; `None` where the record lost it).
    Away {
        /// When the last ALab LAN stay ended.
        since_ms: Option<u64>,
    },
    /// No ALab LAN learned on this Mac yet.
    Learning,
    /// Not known while the place is checking.
    Unknown,
    /// None to learn: no fleet book, or one of fewer than [`LEARN_AT`] members ([`Where::Network`]).
    NoFleet,
}

impl Alab {
    /// The status word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Here => "here",
            Self::Away { .. } => "away",
            Self::Learning => "learning",
            Self::Unknown => "unknown",
            Self::NoFleet => "no-fleet",
        }
    }
}

/// How the place was computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputedBy {
    /// By the live keeper.
    Keeper,
    /// By a reader, from the record and the facts now, writing nothing.
    Reader,
}

impl ComputedBy {
    /// The word for it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Keeper => "keeper",
            Self::Reader => "reader",
        }
    }
}

/// Why the place reads the ALab LAN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recognised {
    /// Members answered in this stay.
    Members,
    /// A remembered network.
    Remembered,
    /// The fleet book's answers from this subnet seeded the first stay.
    Seeded,
}

/// This Mac's network place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// Where it is.
    pub at: Where,
    /// The network's kind, whatever its place; `None` offline.
    pub kind: Option<Kind>,
    /// Why it reads the ALab LAN, when it does.
    pub recognised: Option<Recognised>,
    /// A remembered network that fell silent: "a network members were once on".
    pub once_alab: bool,
    /// The network's id.
    pub network: Option<NetworkId>,
    /// The route traffic takes.
    pub route: Option<RouteFacts>,
    /// The tunnel holding the default route.
    pub tunnel: Option<String>,
    /// The path monitor's reading.
    pub path: Option<PathFacts>,
    /// When this stay began (unix ms).
    pub since_ms: Option<u64>,
    /// The stay began before the keeper that opened it started.
    pub since_before: bool,
    /// The internet axis.
    pub internet: Internet,
    /// The ALab LAN axis.
    pub alab: Alab,
    /// Distinct members heard in this stay: the evidence [`LEARN_AT`] counts. It counts answers,
    /// not members up, and no reporter words it: presence says who is up (one-mesh §4.12).
    pub heard: usize,
    /// Members the fleet book knows, this Mac aside; `None` with no book. The learning rule
    /// reads it; no reporter words it.
    pub known: Option<usize>,
    /// The newest member answer on any network: when and on which lane.
    pub last_answer: Option<(u64, Lane)>,
    /// How it was computed.
    pub computed_by: ComputedBy,
}

/// Whether `answer` counts for `stay` on `route`: its dial began in the stay, its time is not
/// ahead of `now`, and it came over this network's interface (the local address its route took
/// is this Mac's address here), which a routed home LAN (a VLAN, double NAT, a VM network)
/// reached through this network's gateway passes too. An answer whose route was not read counts
/// where its address lies on this subnet. A tunnel's answer took the tunnel's address and counts
/// for no network.
fn counts<const TRACK: bool>(
    answer: &Answer,
    stay: &Stay,
    route: &RouteFacts,
    clock: &Clock<TRACK>,
) -> bool {
    answer.began_ms >= stay.start_ms
        && clock.admits(answer.at_ms, AHEAD_MAX_MS)
        && match answer.via {
            Some(via) => via == route.address,
            None => answer.addr.is_some_and(|addr| route.on_link(addr)),
        }
}

/// The distinct members answering in `stay` on `route`: the larger of the two lanes' counts
/// (their ids are separate spaces).
fn members_in<const TRACK: bool>(
    evidence: &Evidence,
    stay: &Stay,
    route: &RouteFacts,
    clock: &Clock<TRACK>,
) -> usize {
    let mut most = 0;
    let mut members: Vec<&str> = Vec::new();
    for lane in [Lane::Fleet, Lane::Commons] {
        members.clear();
        members.extend(
            evidence
                .answers
                .iter()
                .filter(|answer| answer.lane == lane && counts(answer, stay, route, clock))
                .map(|answer| answer.member.as_str()),
        );
        members.sort_unstable();
        members.dedup();
        most = most.max(members.len());
    }
    most
}

/// Fleet rounds in `stay` that heard no member and met only silence ([`Round::blocked`] none):
/// timeouts, a host down, no route to a member. A round whose dials a sandbox or firewall here
/// refused, or that a host answered and refused, is no evidence that the members are elsewhere:
/// at home behind such a refusal, or with members refusing this Mac's key, the remembered network
/// stays the ALab LAN.
fn silent_rounds<const TRACK: bool>(
    evidence: &Evidence,
    stay: &Stay,
    clock: &Clock<TRACK>,
) -> usize {
    evidence
        .rounds
        .iter()
        .filter(|round| {
            round.lane == Lane::Fleet
                && round.answered == 0
                && round.blocked == 0
                && round.began_ms >= stay.start_ms
                && clock.admits(round.at_ms, AHEAD_MAX_MS)
        })
        .count()
}

/// Whether the fleet book seeds `route` (the network `network`) as the ALab LAN: at least
/// [`LEARN_AT`] members answered this Mac within [`SEED_WITHIN_MS`] from addresses on this
/// subnet while this Mac was on this very network ([`BookMember::network`]), so a network
/// numbered like home behind another router seeds nothing.
fn seeded<const TRACK: bool>(
    book: Option<&Book>,
    (route, network): (&RouteFacts, Option<&NetworkId>),
    clock: &Clock<TRACK>,
) -> bool {
    let Some(network) = network.filter(|network| network.is_strong()) else {
        return false;
    };
    book.is_some_and(|book| {
        book.members
            .iter()
            .filter(|member| member.network.as_ref() == Some(network))
            .filter(|member| member.addr.is_some_and(|addr| route.on_link(addr)))
            .filter(|member| {
                member.answered_ms.is_some_and(|at| {
                    clock.admits(at, AHEAD_MAX_MS) && !clock.older_than(at, SEED_WITHIN_MS)
                })
            })
            .count()
            >= LEARN_AT
    })
}

/// The internet axis in `stay`: the newest measured note in it while [`INTERNET_FRESH_MS`] old,
/// else what the path says.
fn internet_in<const TRACK: bool>(
    evidence: &Evidence,
    stay: Option<&Stay>,
    path: Option<PathFacts>,
    clock: &Clock<TRACK>,
) -> Internet {
    let measured = stay.and_then(|stay| {
        evidence
            .internet
            .iter()
            .filter(|note| note.at_ms >= stay.start_ms && clock.admits(note.at_ms, AHEAD_MAX_MS))
            .max_by_key(|note| note.at_ms)
    });
    match measured {
        Some(note) if !clock.older_than(note.at_ms, INTERNET_FRESH_MS) => Internet {
            state: if note.ok {
                InternetState::Ok
            } else {
                InternetState::Down
            },
            source: note.source,
            at_ms: Some(note.at_ms),
            endpoint: Some(note.endpoint.clone()),
        },
        _ => match path.and_then(|path| path.usable) {
            Some(true) => Internet {
                state: InternetState::PathUp,
                source: InternetSource::Path,
                at_ms: None,
                endpoint: None,
            },
            Some(false) => Internet {
                state: InternetState::Down,
                source: InternetSource::Path,
                at_ms: None,
                endpoint: None,
            },
            None => Internet {
                state: InternetState::Unknown,
                source: InternetSource::Nothing,
                at_ms: None,
                endpoint: None,
            },
        },
    }
}

/// The record's stays brought up to `facts` at `now`: the current stay continued, a pending one
/// given its strong id (and folded back into the stay before it where that was the same network
/// and nothing came between, as a wake on the same network), a pending one past [`SETTLE_MS`]
/// given a weak id, or a new stay opened. `keeper_start` is when the keeper reading began: a new
/// stay with no earlier record starts there and says "since before".
fn observe<const TRACK: bool>(
    mut record: Record,
    facts: &Facts,
    clock: &Clock<TRACK>,
    keeper_start: Option<u64>,
) -> Record {
    let fresh = |start_ms: u64, from_start: bool| Stay {
        start_ms,
        end_ms: None,
        at: StayAt::Checking,
        network: facts.route.as_ref().and_then(NetworkId::of),
        subnet: facts.route.as_ref().map(RouteFacts::subnet_words),
        gateway: facts.route.as_ref().and_then(|route| route.gateway),
        from_start,
    };
    match record.stays.last_mut() {
        Some(stay) if stay.end_ms.is_none() && stay.continues(facts) => {
            if let Some(route) = facts.route.as_ref()
                && stay.network.as_ref().is_none_or(|n| !n.is_strong())
            {
                if let Some(id) = NetworkId::of(route) {
                    stay.network = Some(id);
                } else if stay.network.is_none() && clock.older_than(stay.start_ms, SETTLE_MS) {
                    stay.network = Some(NetworkId::weak(route));
                }
            }
        }
        Some(stay) => {
            if stay.end_ms.is_none() {
                stay.end_ms = Some(clock.now);
            }
            // The keeper's first look: when this Mac came onto this network is not known, only
            // that it was here by now. A reader on a record the keeper has not brought up to
            // these facts sees a move a running keeper is about to record as starting now, so
            // it opens the stay as that keeper will: from now, with the dwell still ahead.
            // Treating it as here-before stamped away-since with each read's own time and
            // skipped the dwell, so every read was a new settled away key.
            let from_start =
                keeper_start.is_some_and(|start| !clock.at_least(start, FIRST_LOOK_MS));
            record.stays.push(fresh(clock.now, from_start));
        }
        None => {
            let start = keeper_start.unwrap_or(clock.now).min(clock.now);
            record.stays.push(fresh(start, true));
        }
    }
    // A pending stay that resolved to the network of the stay right before it (a wake whose ARP
    // came back): one stay, so away-since and the stay's evidence carry on.
    let n = record.stays.len();
    if n >= 2 {
        let (before, last) = (&record.stays[n - 2], &record.stays[n - 1]);
        if last.network.as_ref().is_some_and(NetworkId::is_strong)
            && before.network == last.network
            && before.end_ms == Some(last.start_ms)
        {
            record.stays.pop();
            if let Some(before) = record.stays.last_mut() {
                before.end_ms = None;
            }
        }
    }
    let excess = record.stays.len().saturating_sub(STAYS_MAX);
    record.stays.drain(..excess);
    record
}

/// Observe owned history with current reader facts, then transfer its current
/// stay fields. Report classification changes neither field and is not admission.
#[cfg(unix)]
pub(crate) fn observed_stay(
    record: Record,
    facts: &Facts,
    now: u64,
) -> (Option<u64>, Option<NetworkId>) {
    let mut record = observe(record, facts, &Clock::<false>::at(now), None);
    match record.stays.last_mut().filter(|stay| stay.end_ms.is_none()) {
        Some(stay) => (Some(stay.start_ms), stay.network.take()),
        None => (None, None),
    }
}

/// Since when this Mac has been away from the ALab LAN: the end of the newest ALab LAN stay the
/// record keeps; `None` (unknown) where it keeps none, as after days away whose wakes and moves
/// pushed the last home stay out of the [`STAYS_MAX`] kept, rather than an invented time such as
/// the oldest stay's start.
fn away_since(record: &Record) -> Option<u64> {
    record
        .stays
        .iter()
        .rev()
        .find(|stay| stay.at == StayAt::AlabLan && stay.end_ms.is_some())
        .and_then(|stay| stay.end_ms)
}

/// `known` confirmed by members answering at `now`: its time moved on once it is
/// [`CONFIRM_EVERY_MS`] old, so the record does not change at every look.
fn confirm<const TRACK: bool>(known: &mut Known, clock: &Clock<TRACK>) {
    if clock.at_least(known.confirmed_ms, CONFIRM_EVERY_MS) {
        known.confirmed_ms = clock.now;
    }
}

/// THE PLACE. `record` brought up to `facts` at `now` ([`observe`]), the place those and the
/// `evidence` and `book` give, and the record's next state: the current stay's decision kept,
/// and a network learned as the ALab LAN (or a remembered one confirmed) where members proved
/// it. `keeper_start` is the keeper's start ([`observe`]); a reader passes `None` and keeps the
/// record it is handed.
#[must_use]
pub fn step(
    record: &Record,
    facts: &Facts,
    evidence: &Evidence,
    book: Option<&Book>,
    now: u64,
    keeper_start: Option<u64>,
) -> (Record, Place) {
    step_owned(record.clone(), facts, evidence, book, now, keeper_start)
}

/// Step a record the caller already owns without first cloning its history.
/// Readers consume parsed memory; the borrowed public entry preserves its input.
pub(crate) fn step_owned(
    record: Record,
    facts: &Facts,
    evidence: &Evidence,
    book: Option<&Book>,
    now: u64,
    keeper_start: Option<u64>,
) -> (Record, Place) {
    step_at(
        record,
        facts,
        evidence,
        book,
        &Clock::<false>::at(now),
        keeper_start,
    )
}

fn step_at<const TRACK: bool>(
    record: Record,
    facts: &Facts,
    evidence: &Evidence,
    book: Option<&Book>,
    clock: &Clock<TRACK>,
    keeper_start: Option<u64>,
) -> (Record, Place) {
    let now = clock.now;
    let mut record = observe(record, facts, clock, keeper_start);
    let stay = record.current();
    let path = facts.path;
    let internet = internet_in(evidence, stay, path, clock);
    let known = book.map(|book| book.members.len());
    let last_answer = evidence
        .answers
        .iter()
        .filter(|answer| clock.admits(answer.at_ms, AHEAD_MAX_MS))
        .max_by_key(|answer| answer.at_ms)
        .map(|answer| (answer.at_ms, answer.lane));
    let kind = facts.route.as_ref().map(|_| facts.kind());
    let (members_here, silent) = match (stay, facts.route.as_ref()) {
        (Some(stay), Some(route)) => (
            members_in(evidence, stay, route, clock),
            silent_rounds(evidence, stay, clock) >= SILENT_ROUNDS,
        ),
        _ => (0, false),
    };
    let (has_stay, since_ms, since_before, network) = match stay {
        Some(stay) => (
            true,
            Some(stay.start_ms),
            stay.from_start,
            stay.network.clone(),
        ),
        None => (false, None, false, None),
    };
    let mut recognised = None;
    let mut once_alab = false;
    let at = match (facts.route.as_ref(), has_stay) {
        (None, _) => match path.and_then(|path| path.usable) {
            Some(true) => Where::Checking(Checking::NoIpv4),
            _ => Where::Offline,
        },
        (Some(_), true) if network.is_none() => Where::Checking(Checking::Moved),
        (Some(route), _) => {
            let network = network.as_ref();
            let remembered = network.is_some_and(|network| record.remembers(network));
            if let Some(kind) = facts.hotspot_or_cellular() {
                // A phone hotspot or a cellular link is never the ALab LAN, whoever answers on it.
                Where::Remote(kind)
            } else if members_here >= LEARN_AT {
                recognised = Some(Recognised::Members);
                Where::AlabLan
            } else if remembered && !(silent && members_here == 0) {
                recognised = Some(Recognised::Remembered);
                Where::AlabLan
            } else if remembered {
                once_alab = true;
                Where::Remote(facts.kind())
            } else if record.remembered.is_empty()
                && !silent
                && seeded(book, (route, network), clock)
            {
                recognised = Some(Recognised::Seeded);
                Where::AlabLan
            } else if record.remembered.is_empty() && known.is_none_or(|known| known < LEARN_AT) {
                Where::Network(facts.kind())
            } else if record.remembered.is_empty() {
                Where::Checking(Checking::Learning)
            } else if network.is_some_and(|network| !network.is_strong()) {
                Where::Checking(Checking::Unidentified)
            } else {
                Where::Remote(facts.kind())
            }
        }
    };
    // A newly learned key is stamped at the clock now. Its ordering against an
    // older key can change with time if pruning initially discards the new key.
    let mut learned_now = None;
    // Learn: members proved this network in this stay.
    if recognised == Some(Recognised::Members)
        && known.is_some_and(|known| known >= LEARN_AT)
        && let Some(network) = network.as_ref().filter(|network| network.is_strong())
    {
        match record
            .remembered
            .iter_mut()
            .find(|known| known.network == *network)
        {
            Some(known) => confirm(known, clock),
            None => {
                record.remembered.push(Known {
                    network: network.clone(),
                    learned_ms: now,
                    confirmed_ms: now,
                });
                learned_now = Some(network);
            }
        }
    } else if recognised == Some(Recognised::Remembered)
        && members_here > 0
        && let Some(known) = record
            .remembered
            .iter_mut()
            .find(|known| Some(&known.network) == network.as_ref())
    {
        confirm(known, clock);
    }
    if record.remembered.len() > REMEMBERED_MAX {
        record.remembered.sort_by(|a, b| {
            match (
                learned_now == Some(&a.network),
                learned_now == Some(&b.network),
            ) {
                (true, false) => clock.cmp_now(b.confirmed_ms).reverse(),
                (false, true) => clock.cmp_now(a.confirmed_ms),
                _ => b.confirmed_ms.cmp(&a.confirmed_ms),
            }
        });
        record.remembered.truncate(REMEMBERED_MAX);
    }
    if let Some(current) = record.stays.last_mut().filter(|stay| stay.end_ms.is_none()) {
        current.at = StayAt::of(at);
    }
    let alab = match at {
        Where::AlabLan => Alab::Here,
        Where::Network(_) => Alab::NoFleet,
        Where::Checking(Checking::Moved | Checking::Unidentified | Checking::NoIpv4) => {
            Alab::Unknown
        }
        _ if record.remembered.is_empty() => Alab::Learning,
        _ => Alab::Away {
            since_ms: away_since(&record),
        },
    };
    let place = Place {
        at,
        kind,
        recognised,
        once_alab,
        network,
        route: facts.route.clone(),
        tunnel: facts.tunnel.clone(),
        path,
        since_ms,
        since_before,
        internet,
        alab,
        heard: members_here,
        known,
        last_answer,
        computed_by: if keeper_start.is_some() {
            ComputedBy::Keeper
        } else {
            ComputedBy::Reader
        },
    };
    (record, place)
}

/// The current step and its next time-only change with all other inputs fixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    /// The updated record, which subsequent comparisons must use.
    pub record: Record,
    /// The place at the supplied time.
    pub place: Place,
    /// The first different forward millisecond, or the supplied time if the updated
    /// record needs immediate normalization. `None` means no representable forward change.
    /// External facts, presentation clocks and wall-clock rollback still invalidate this query.
    pub next_change_ms: Option<u64>,
}

/// Step once, then find the earliest time-only change of either returned value.
/// Future comparisons use the returned record and the same facts, evidence, book and
/// keeper start. Candidates come from the production predicates; a predicate flip that
/// leaves both values equal is skipped. No clock, I/O, live wait or LAN proof is cached.
#[must_use]
pub fn step_with_deadline(
    record: &Record,
    facts: &Facts,
    evidence: &Evidence,
    book: Option<&Book>,
    now: u64,
    keeper_start: Option<u64>,
) -> Projection {
    step_with_deadline_owned(record.clone(), facts, evidence, book, now, keeper_start)
}

/// Project an owned intermediate record without copying it into the first step.
/// Future probes still copy the returned record they must preserve for comparison.
pub(crate) fn step_with_deadline_owned(
    record: Record,
    facts: &Facts,
    evidence: &Evidence,
    book: Option<&Book>,
    now: u64,
    keeper_start: Option<u64>,
) -> Projection {
    let (record, place) = step_owned(record, facts, evidence, book, now, keeper_start);
    let mut cursor = now;
    let next_change_ms = loop {
        let clock = Clock::<true>::at(cursor);
        let projected = step_at(record.clone(), facts, evidence, book, &clock, keeper_start);
        if projected.0 != record || projected.1 != place {
            break Some(cursor);
        }
        match clock.next() {
            Some(next) => cursor = next,
            None => break None,
        }
    };
    Projection {
        record,
        place,
        next_change_ms,
    }
}

impl Place {
    /// Whether this Mac wants a path to its members other than the LAN: it is away from the
    /// ALab LAN and the internet answers. The seam the future internet path engages at.
    #[must_use]
    pub fn wants_internet_path(&self) -> bool {
        matches!(self.alab, Alab::Away { .. }) && self.internet.state == InternetState::Ok
    }
}

#[cfg(test)]
#[path = "place_tests.rs"]
mod tests;
