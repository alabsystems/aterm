// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this Mac is on the home ALab LAN now, which the default ALab vault's automatic LAN
//! admission rests on (`docs/DESIGN-automatic-alab-lan-admission-2026-10-29.md`).
//!
//! An address does not make a network home: a phone hotspot, a café, a hotel or a travel router
//! numbers its hosts from RFC 1918 too, and a vault server listens and advertises on every
//! network it is on. A network is the home ALab LAN here only by evidence that cannot be made up
//! without a member: at least [`MEMBERS`] distinct fleet members proved their node keys to this
//! Mac, within [`FRESH`], from addresses on the very subnet in question, through this Mac's
//! current address on it and the same interface, with the default gateway's hardware address
//! unchanged since. Only the network the IPv4 default route takes counts, and only over Ethernet
//! or Wi-Fi (`en…`, not point to point): a VM or Internet Sharing bridge, a VPN or tunnel,
//! AirDrop's links and cellular never do, whatever answered there (member VMs on a lab bridge
//! are members too). A network in the Personal Hotspot range (`172.20.10.0/28`), or one
//! Network.framework calls expensive (cellular, or Wi-Fi to a phone's hotspot, Android's
//! included), or whose path could not be read, is never home. No evidence is no home: the request
//! waits for a person's Allow.
//!
//! The evidence is [`FILE`] in the secrets directory: one line per member, the last time its
//! node-proven fleet answer came ([`note`], from the fleet heartbeat), written by the process
//! that heard it and read by the vault server and the approver. [`Place`] is the seam both read
//! it through, so the per-Mac network place (`aterm-netplace`) can stand in for [`Live`] once
//! it lands without either changing shape. An approver honours a server's `home=` only where it
//! reached that server on its own home network, or the server is itself ([`Place::serving`]).

use crate::client::Paths;
use crate::network::InterfaceAddress;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

/// The evidence file's name in [`Paths::dir`].
pub const FILE: &str = "home-lan";
/// How long a member's proven answer counts: past three heartbeat rounds (each every three
/// minutes, to up to three members), so a Mac on the home LAN keeps its evidence while one
/// round fails, and one that left it loses the evidence within minutes even if the next network
/// numbers its hosts alike.
pub const FRESH: Duration = Duration::from_secs(10 * 60);
/// How many distinct members must have answered on a network for it to be home: two Macs on one
/// hotspot or café network never make it so.
pub const MEMBERS: usize = 3;
/// The most members the file keeps, the newest answers first.
pub const MAX_ANSWERS: usize = 256;
/// The largest evidence file read.
const MAX_FILE: usize = MAX_ANSWERS * 160;
/// How far past this Mac's clock an answer may be dated and still count (a clock step back).
const AHEAD_MS: u64 = 60_000;
/// How long an answer is kept in the file at all.
const KEPT_MS: u64 = 24 * 60 * 60 * 1000;
/// Apple's Personal Hotspot hands out addresses from this network.
const HOTSPOT: (Ipv4Addr, u32) = (Ipv4Addr::new(172, 20, 10, 0), 28);
/// What a [`HomeTag`] hashes after.
const TAG_DOMAIN: &[u8] = b"aterm/alab/home-lan/1\0";
/// The longest node id the file takes.
const MAX_NODE: usize = 64;
/// The longest interface name the file takes.
const MAX_IFACE: usize = 16;
/// `<net/if.h>`'s `IFF_POINTOPOINT` (the same on macOS and Linux).
const IFF_POINTOPOINT: u32 = 0x10;
/// How long a read of whether the path is expensive stands ([`path_expensive`]).
#[cfg(target_os = "macos")]
const PATH_KEPT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long the read waits for Network.framework's first path update, which a Mac with a network
/// hands over at once.
#[cfg(target_os = "macos")]
const PATH_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Which home network a request was observed on: 16 lowercase hex digits hashed from the
/// subnet, so a listing names no address. Two Macs on one LAN compute the same tag.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HomeTag(String);

impl HomeTag {
    /// The tag of the subnet `network`/`prefix`.
    #[must_use]
    pub fn of(network: Ipv4Addr, prefix: u32) -> Self {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(TAG_DOMAIN);
        hash.update(network.octets());
        hash.update([u8::try_from(prefix).unwrap_or(u8::MAX)]);
        let digest = hash.finalize();
        Self(aterm_codec::hex::encode_lower(&digest[..8]))
    }

    /// A tag as a listing spells it, when it is one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        (text.len() == 16 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self(text.to_owned()))
    }

    /// Its listed spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The tag a test's home network carries (`192.0.2.0/24`, TEST-NET-1).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn test() -> Self {
        Self::of(Ipv4Addr::new(192, 0, 2, 0), 24)
    }
}

impl std::fmt::Display for HomeTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a request was not observed on the home ALab LAN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Away {
    /// It came or arrived over IPv6, or from this Mac's own loopback.
    NotIpv4,
    /// It came from no subnet of the address it arrived at (a routed or relayed peer), or it
    /// arrived on another network than the one the default route takes.
    NotOnLink,
    /// The default route takes no Ethernet or Wi-Fi interface (a VPN, a tunnel, a bridge,
    /// cellular), or there is none.
    Interface,
    /// It came from one of this Mac's own addresses.
    ThisMac,
    /// The subnet is in the Personal Hotspot range.
    Hotspot,
    /// The network is expensive (cellular, or Wi-Fi to a phone's hotspot) as Network.framework
    /// reads it, or the path could not be read.
    Expensive,
    /// The default gateway's hardware address cannot be read now (no route, or ARP not
    /// resolved yet), so no evidence can be tied to this network.
    NoGateway,
    /// Fewer than [`MEMBERS`] members proved themselves on this subnet within [`FRESH`].
    TooFew {
        /// How many did.
        heard: usize,
    },
}

impl std::fmt::Display for Away {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotIpv4 => f.write_str("it did not come over IPv4 from another Mac"),
            Self::NotOnLink => f.write_str("it came from off this Mac's network"),
            Self::Interface => f.write_str("this Mac's traffic leaves by no Ethernet or Wi-Fi"),
            Self::ThisMac => f.write_str("it came from this Mac"),
            Self::Hotspot => f.write_str("this Mac is on a phone hotspot"),
            Self::Expensive => {
                f.write_str("this Mac is on a cellular or hotspot network, or cannot tell")
            }
            Self::NoGateway => f.write_str("this network's router is not known yet"),
            Self::TooFew { heard } => write!(
                f,
                "{heard} of the {MEMBERS} members needed answered on this network in the last \
                 {} minutes",
                FRESH.as_secs() / 60
            ),
        }
    }
}

/// One member's last node-proven fleet answer ([`note`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// Its fleet node id.
    pub node: String,
    /// The address it answered from.
    pub addr: Ipv4Addr,
    /// This Mac's address on that subnet when it answered.
    pub via: Ipv4Addr,
    /// The interface that address was on (`en0`).
    pub iface: String,
    /// The default gateway's hardware address then, lowercase hex (`None`: not read).
    pub gateway: Option<String>,
    /// When it answered (unix milliseconds).
    pub ms: u64,
}

impl Answer {
    fn line(&self) -> String {
        format!(
            "answered node={} addr={} via={} if={} gw={} ms={}",
            self.node,
            self.addr,
            self.via,
            self.iface,
            self.gateway.as_deref().unwrap_or("-"),
            self.ms
        )
    }

    fn parse(line: &str) -> Option<Self> {
        let mut words = line.split(' ');
        if words.next()? != "answered" {
            return None;
        }
        let mut field = |key: &str| words.next()?.strip_prefix(key)?.strip_prefix('=');
        let node = field("node").filter(|node| valid_node(node))?.to_owned();
        let addr = field("addr")?.parse().ok()?;
        let via = field("via")?.parse().ok()?;
        let iface = field("if").filter(|iface| valid_iface(iface))?.to_owned();
        let gateway = match field("gw")? {
            "-" => None,
            hex if !hex.is_empty() && hex.len() <= 32 && hex.bytes().all(is_lower_hex) => {
                Some(hex.to_owned())
            }
            _ => return None,
        };
        let ms = field("ms")?;
        let ms = (!ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()))
            .then(|| ms.parse().ok())
            .flatten()?;
        words.next().is_none().then_some(Self {
            node,
            addr,
            via,
            iface,
            gateway,
            ms,
        })
    }
}

const fn is_lower_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

fn valid_iface(iface: &str) -> bool {
    !iface.is_empty()
        && iface.len() <= MAX_IFACE
        && iface.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Whether an interface named `name` with `flags` can carry the home ALab LAN: Ethernet or Wi-Fi
/// (`en` and a number), not point to point. A VM or Internet Sharing bridge (`bridge…`,
/// `vmenet…`), a VPN or tunnel (`utun…`, `ipsec…`, `ppp…`, `gif…`, `stf…`), AirDrop's `awdl…` and
/// `llw…`, `anpi…` and cellular (`pdp_ip…`) never do.
fn home_kind(name: &str, flags: u32) -> bool {
    name.strip_prefix("en")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && flags & IFF_POINTOPOINT == 0
}

fn valid_node(node: &str) -> bool {
    !node.is_empty()
        && node.len() <= MAX_NODE
        && node
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What this Mac's network is now, read with no packet.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Its interface addresses.
    pub interfaces: Vec<InterfaceAddress>,
    /// The address the IPv4 default route leaves this Mac from (`None`: no default route): the
    /// one network a request can be observed at home on.
    pub primary: Option<Ipv4Addr>,
    /// The default gateway's hardware address, lowercase hex (`None`: not read). It is the
    /// gateway of the default route's interface, the only one that counts.
    pub gateway: Option<String>,
    /// The path is expensive (cellular, or Wi-Fi to a phone's hotspot) as Network.framework's
    /// path monitor reads it, or it could not be read ([`Facts::live`]): never home.
    pub expensive: bool,
}

/// This Mac's address on the home-capable network the default route takes, with its mask, prefix
/// and interface name ([`Facts::home_subnet`]).
type HomeSubnet<'a> = (Ipv4Addr, Ipv4Addr, u32, &'a str);

impl Facts {
    /// This Mac's facts now. The path is read through Network.framework (at most every
    /// [`PATH_KEPT`], waiting at most [`PATH_PATIENCE`] for it); a path that cannot be read counts
    /// as expensive, so it is no home.
    #[must_use]
    pub fn live() -> Self {
        Self {
            interfaces: crate::network::interfaces().unwrap_or_default(),
            primary: crate::server::primary_ipv4(),
            gateway: crate::network::default_gateway_hw()
                .map(|hw| aterm_codec::hex::encode_lower(&hw)),
            expensive: path_expensive().unwrap_or(true),
        }
    }

    /// The default route's address with its subnet and interface, where that interface is
    /// Ethernet or Wi-Fi ([`home_kind`]): the only network the home ALab LAN can be.
    fn home_subnet(&self) -> Option<HomeSubnet<'_>> {
        let primary = self.primary?;
        self.interfaces
            .iter()
            .find_map(|address| match (address.address, address.netmask) {
                (IpAddr::V4(ip), Some(IpAddr::V4(mask)))
                    if ip == primary
                        && !ip.is_loopback()
                        && !ip.is_unspecified()
                        && home_kind(&address.name, address.flags) =>
                {
                    Some((ip, mask, mask.to_bits().count_ones(), address.name.as_str()))
                }
                _ => None,
            })
    }

    /// Whether `ip` is one of this Mac's own addresses, on any interface.
    fn is_own(&self, ip: Ipv4Addr) -> bool {
        self.interfaces
            .iter()
            .any(|address| address.address == IpAddr::V4(ip))
    }

    /// This Mac's address and interface on the home-capable network, when that network holds
    /// `ip`.
    fn via(&self, ip: Ipv4Addr) -> Option<(Ipv4Addr, &str)> {
        self.home_subnet()
            .filter(|&(own, mask, _, _)| own != ip && same(own, ip, mask))
            .map(|(own, _, _, iface)| (own, iface))
    }
}

/// Whether the network path this Mac uses now is expensive, as a Network.framework path monitor's
/// first update reads it (`nw_path_is_expensive`), kept for [`PATH_KEPT`]; `None` where no path
/// that carries traffic was read within [`PATH_PATIENCE`]. Never on a window's main thread: the
/// vault server's and approver's threads, and the fleet heartbeat's, call it.
#[cfg(target_os = "macos")]
fn path_expensive() -> Option<bool> {
    use aterm_types::MutexExt as _;
    static KEPT: std::sync::Mutex<Option<(std::time::Instant, Option<bool>)>> =
        std::sync::Mutex::new(None);
    if let Some((at, read)) = *KEPT.lock_or_recover()
        && at.elapsed() < PATH_KEPT
    {
        return read;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<aterm_objc::PathReading>(1);
    let read = aterm_objc::PathMonitor::start(c"aterm.secrets.home-lan", move |reading| {
        let _ = tx.try_send(reading);
    })
    .and_then(|monitor| {
        let first = rx.recv_timeout(PATH_PATIENCE).ok();
        drop(monitor);
        first
    })
    .filter(|reading| reading.status == aterm_objc::PathStatus::Satisfied)
    .map(|reading| reading.expensive);
    *KEPT.lock_or_recover() = Some((std::time::Instant::now(), read));
    read
}

/// No path reader off macOS: not read, so no home.
#[cfg(not(target_os = "macos"))]
const fn path_expensive() -> Option<bool> {
    None
}

fn same(a: Ipv4Addr, b: Ipv4Addr, mask: Ipv4Addr) -> bool {
    a.to_bits() & mask.to_bits() == b.to_bits() & mask.to_bits()
}

fn network(ip: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from_bits(ip.to_bits() & mask.to_bits())
}

/// Whether `network`/`prefix` overlaps Apple's Personal Hotspot range.
fn hotspot(network: Ipv4Addr, prefix: u32) -> bool {
    let (range, range_prefix) = HOTSPOT;
    let shorter = prefix.min(range_prefix);
    let mask = u32::MAX.checked_shl(32 - shorter).unwrap_or(0);
    network.to_bits() & mask == range.to_bits() & mask
}

fn ipv4(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip.to_canonical() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// Whether the home-capable network ([`Facts::home_subnet`]) is home at `now_ms`.
fn home_at(facts: &Facts, answers: &[Answer], now_ms: u64) -> Result<HomeTag, Away> {
    let Some((own, mask, prefix, iface)) = facts.home_subnet() else {
        return Err(Away::Interface);
    };
    let subnet = network(own, mask);
    if hotspot(subnet, prefix) {
        return Err(Away::Hotspot);
    }
    if facts.expensive {
        return Err(Away::Expensive);
    }
    let Some(gateway) = facts.gateway.as_deref() else {
        return Err(Away::NoGateway);
    };
    let mut nodes: Vec<&str> = answers
        .iter()
        .filter(|answer| {
            same(answer.addr, own, mask)
                && answer.via == own
                && answer.iface == iface
                && !facts.is_own(answer.addr)
                && answer.gateway.as_deref() == Some(gateway)
                && answer.ms <= now_ms.saturating_add(AHEAD_MS)
                && now_ms.saturating_sub(answer.ms) < FRESH.as_millis() as u64
        })
        .map(|answer| answer.node.as_str())
        .collect();
    nodes.sort_unstable();
    nodes.dedup();
    if nodes.len() >= MEMBERS {
        Ok(HomeTag::of(subnet, prefix))
    } else {
        Err(Away::TooFew { heard: nodes.len() })
    }
}

/// The home network on which a request that arrived at this Mac's address `arrival` from `peer`
/// was observed at `now_ms`, or why it was not one: `arrival` must be this Mac's address on the
/// network the default route takes, `peer` on its subnet, and that network home ([`home_at`]).
///
/// # Errors
/// [`Away`] names why the request was not observed on the home ALab LAN.
pub fn observed(
    facts: &Facts,
    answers: &[Answer],
    arrival: IpAddr,
    peer: IpAddr,
    now_ms: u64,
) -> Result<HomeTag, Away> {
    let (Some(arrival), Some(peer)) = (ipv4(arrival), ipv4(peer)) else {
        return Err(Away::NotIpv4);
    };
    if facts.is_own(peer) {
        return Err(Away::ThisMac);
    }
    let Some((own, mask, _, _)) = facts.home_subnet() else {
        return Err(Away::Interface);
    };
    if arrival != own || !same(peer, own, mask) {
        return Err(Away::NotOnLink);
    }
    home_at(facts, answers, now_ms)
}

/// This Mac's home ALab LAN, as its address and mask there ([`home_net_at`]): what a member's
/// desk judges an asking Mac's network by (`crate::members::desk`), since the vault's server
/// observes only the member that forwards the ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HomeNet {
    own: Ipv4Addr,
    mask: Ipv4Addr,
}

impl HomeNet {
    /// Whether `peer` is another host on this network: an IPv4 (or IPv4-mapped) address on its
    /// subnet that is not this Mac's address there.
    #[must_use]
    pub fn holds(&self, peer: IpAddr) -> bool {
        ipv4(peer).is_some_and(|peer| peer != self.own && same(peer, self.own, self.mask))
    }

    /// The test home network: this Mac at `192.168.1.20/24`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn test() -> Self {
        Self::at(
            Ipv4Addr::new(192, 168, 1, 20),
            Ipv4Addr::new(255, 255, 255, 0),
        )
    }

    /// A home network a test names: this Mac at `own` under `mask`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn at(own: Ipv4Addr, mask: Ipv4Addr) -> Self {
        Self { own, mask }
    }
}

/// This Mac's home ALab LAN at `now_ms` ([`here`]), as its address and mask there; `None` where
/// it is on none.
#[must_use]
pub fn home_net_at(facts: &Facts, answers: &[Answer], now_ms: u64) -> Option<HomeNet> {
    let (own, mask, _, _) = facts.home_subnet()?;
    home_at(facts, answers, now_ms).ok()?;
    Some(HomeNet { own, mask })
}

/// This Mac's home ALab LAN now, from its live facts and the evidence under `paths`. It reads the
/// network (Network.framework's path among it), so never on a window's main thread.
#[must_use]
pub fn home_net(paths: &Paths) -> Option<HomeNet> {
    aterm_time::unix_millis().and_then(|now| home_net_at(&Facts::live(), &read(paths), now))
}

/// The home networks this Mac is on at `now_ms` (the default route's, when home), each once.
#[must_use]
pub fn here(facts: &Facts, answers: &[Answer], now_ms: u64) -> Vec<HomeTag> {
    home_at(facts, answers, now_ms).ok().into_iter().collect()
}

/// The home networks this Mac is on at `now_ms` through which it reaches `server` ([`here`]):
/// all of them where `server` is this Mac itself (loopback, or one of its addresses), the home
/// network where `server` is on its subnet, else none. An approver honours a server's `home=`
/// only so: a server it reaches by a route (a remote URL, a VPN, a VM bridge) observed nothing
/// this approver can stand behind.
#[must_use]
pub fn serving(facts: &Facts, answers: &[Answer], server: IpAddr, now_ms: u64) -> Vec<HomeTag> {
    let server = server.to_canonical();
    let itself = server.is_loopback()
        || match server {
            IpAddr::V4(ip) => facts.is_own(ip),
            IpAddr::V6(_) => facts.interfaces.iter().any(|a| a.address == server),
        };
    let on_link = match server {
        IpAddr::V4(ip) => facts
            .home_subnet()
            .is_some_and(|(own, mask, _, _)| same(ip, own, mask)),
        IpAddr::V6(_) => false,
    };
    if itself || on_link {
        here(facts, answers, now_ms)
    } else {
        Vec::new()
    }
}

/// The answers [`FILE`] under `paths` holds; none where it is absent or unreadable.
#[must_use]
pub fn read(paths: &Paths) -> Vec<Answer> {
    let Ok(Some(bytes)) = crate::fsx::read_owned(&paths.dir.join(FILE), MAX_FILE) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(Answer::parse)
        .collect()
}

/// Note `answers` (each a fleet member's node id and the address its node-proven answer came
/// from) heard at `now_ms` on the network `facts` describe, in [`FILE`] under `paths`: the newest
/// answer stands for each node, answers older than a day go, and past [`MAX_ANSWERS`] the
/// oldest go. An answer from off the default route's Ethernet or Wi-Fi subnet (a routed or
/// relayed member, one on a VM bridge or a VPN) is no evidence of this network and is passed
/// over, as is a node id the file cannot hold.
///
/// # Errors
/// The secrets directory or the file could not be written.
pub fn note_with(
    paths: &Paths,
    answers: &[(String, IpAddr)],
    facts: &Facts,
    now_ms: u64,
) -> crate::Result<()> {
    let fresh: Vec<Answer> = answers
        .iter()
        .filter(|(node, _)| valid_node(node))
        .filter_map(|(node, addr)| {
            let addr = ipv4(*addr)?;
            let (via, iface) = facts.via(addr)?;
            Some(Answer {
                node: node.clone(),
                addr,
                via,
                iface: iface.to_owned(),
                gateway: facts.gateway.clone(),
                ms: now_ms,
            })
        })
        .collect();
    if fresh.is_empty() {
        return Ok(());
    }
    let mut kept = read(paths);
    kept.retain(|held| {
        now_ms.saturating_sub(held.ms) < KEPT_MS
            && held.ms <= now_ms.saturating_add(AHEAD_MS)
            && !fresh.iter().any(|new| new.node == held.node)
    });
    kept.extend(fresh);
    kept.sort_by(|a, b| b.ms.cmp(&a.ms).then_with(|| a.node.cmp(&b.node)));
    kept.truncate(MAX_ANSWERS);
    let mut text = String::from(
        "# When each fleet member last proved its node to this Mac, and from where: written by \
         aterm's fleet heartbeat, read by the vault server and approver to tell the home ALab \
         LAN, never by hand.\n",
    );
    for answer in &kept {
        text.push_str(&answer.line());
        text.push('\n');
    }
    crate::fsx::ensure_private_dir(&paths.dir)?;
    crate::fsx::write_atomic(&paths.dir.join(FILE), text.as_bytes(), 0o600)
}

/// [`note_with`] on this Mac's network now, at the clock's time.
///
/// # Errors
/// As [`note_with`], or the clock reads before 1970.
pub fn note(paths: &Paths, answers: &[(String, IpAddr)]) -> crate::Result<()> {
    let now = aterm_time::unix_millis().ok_or_else(|| crate::Error::new("no clock"))?;
    note_with(paths, answers, &Facts::live(), now)
}

/// Where this Mac is, as the vault server and the approver read it: the seam the network place
/// replaces [`Live`] at.
pub trait Place: Send + Sync {
    /// The home network on which a request that arrived at `arrival` from `peer` was observed
    /// now.
    ///
    /// # Errors
    /// [`Away`] names why it was not observed on the home ALab LAN.
    fn observed(&self, arrival: IpAddr, peer: IpAddr) -> Result<HomeTag, Away>;

    /// The home networks this Mac is on now.
    fn here(&self) -> Vec<HomeTag>;

    /// The home networks this Mac is on now through which it reaches the vault server at
    /// `server` ([`serving`]): the only ones an approver reading that server's listing honours.
    fn serving(&self, server: IpAddr) -> Vec<HomeTag>;
}

/// [`Place`] from this Mac's facts and the evidence under its secrets directory.
pub struct Live(Paths);

impl Live {
    /// The place of the Mac whose secrets directory `paths` names.
    #[must_use]
    pub const fn new(paths: Paths) -> Self {
        Self(paths)
    }
}

impl Place for Live {
    fn observed(&self, arrival: IpAddr, peer: IpAddr) -> Result<HomeTag, Away> {
        let now = aterm_time::unix_millis().ok_or(Away::NoGateway)?;
        observed(&Facts::live(), &read(&self.0), arrival, peer, now)
    }

    fn here(&self) -> Vec<HomeTag> {
        aterm_time::unix_millis()
            .map(|now| here(&Facts::live(), &read(&self.0), now))
            .unwrap_or_default()
    }

    fn serving(&self, server: IpAddr) -> Vec<HomeTag> {
        aterm_time::unix_millis()
            .map(|now| serving(&Facts::live(), &read(&self.0), server, now))
            .unwrap_or_default()
    }
}

/// [`Place`] with no evidence of a home anywhere: a server started with no secrets directory,
/// and every test's server unless the test says otherwise.
pub struct Nowhere;

impl Place for Nowhere {
    fn observed(&self, _: IpAddr, _: IpAddr) -> Result<HomeTag, Away> {
        Err(Away::TooFew { heard: 0 })
    }

    fn here(&self) -> Vec<HomeTag> {
        Vec::new()
    }

    fn serving(&self, _: IpAddr) -> Vec<HomeTag> {
        Vec::new()
    }
}

/// A test's [`Place`]: every IPv4 request (loopback too) observed on the home network
/// [`HomeTag::test`], which this Mac is on.
#[cfg(any(test, debug_assertions))]
pub struct AtHome;

#[cfg(any(test, debug_assertions))]
impl Place for AtHome {
    fn observed(&self, _: IpAddr, peer: IpAddr) -> Result<HomeTag, Away> {
        match peer.to_canonical() {
            IpAddr::V4(_) => Ok(HomeTag::test()),
            IpAddr::V6(_) => Err(Away::NotIpv4),
        }
    }

    fn here(&self) -> Vec<HomeTag> {
        vec![HomeTag::test()]
    }

    fn serving(&self, _: IpAddr) -> Vec<HomeTag> {
        vec![HomeTag::test()]
    }
}

#[cfg(test)]
mod tests;
