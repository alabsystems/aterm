// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ROUTES SEAM: which of its members' addresses each automatic LAN lane dials now, read from
//! the place. The fleet heartbeat, the vault subscriber's naps and atpkg's LAN race ask
//! [`Routes::dials`] and [`Routes::paced`]; a command a person runs (`fleet peers`, `survey
//! --fleet`) dials every member whatever it says, so a member on the same hotspot is still
//! found.
//!
//! Today the only path to the members is the LAN ([`AlabPath::Lan`]). Away from the ALab LAN
//! ([`Routes::paced`]) the lanes stop dialling addresses that cannot answer: an address counts as
//! behind the ALab LAN when it is on no subnet of this network and its route leaves through this
//! network's own interface (`route_source_ipv4` names this Mac's address here), the way the
//! home LAN's private addresses leave a phone hotspot for the internet. A member on this network,
//! one a VPN routes (its route takes the tunnel's address), and one that answered in this stay
//! are always dialled, as is an address on this Mac itself (loopback). Nothing here waits
//! forever: [`Routes::full_round_due`] lets one round dial every member on each network it comes
//! to and once every [`AWAY_FULL_EVERY`] while away, so a home LAN split across routed segments,
//! or a router replaced, is found again from its members' answers. The rounds are keyed to the
//! network ([`Routes::network`]), not the stay, so a process that reads the place with no keeper
//! (whose stay starts at each read) paces all the same.
//!
//! The future internet path to the members plugs in here ([`Routes::wants_internet_path`]: away
//! from the ALab LAN with the internet answering); `relay` is the reserved [`AlabPath`] word.
//! Being away gates nothing: no secret, no admission, no vault serving.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::place::{Alab, Evidence, Kind, Lane, NetworkId, Place, Where, member_key_bytes};

/// How often a round dials every member while away (besides once right after each move): the
/// fleet glance's longest backoff, so a lane away never dials its whole book more often than a
/// person's menu would.
pub const AWAY_FULL_EVERY_MS: u64 = 30 * 60_000;

/// [`AWAY_FULL_EVERY_MS`] as a duration.
pub const AWAY_FULL_EVERY: std::time::Duration =
    std::time::Duration::from_millis(AWAY_FULL_EVERY_MS);

/// The path the lanes reach the members by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlabPath {
    /// The LAN: this Mac is on the ALab LAN, or has not learned one yet.
    Lan,
    /// None: away from the ALab LAN, with no other path built (`relay` is reserved for the
    /// internet path).
    None,
}

impl AlabPath {
    /// The status word: `lan` or `none` (`relay` reserved).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::None => "none",
        }
    }
}

/// Whether the automatic LAN lanes pace themselves at `place` ([`Routes::paced`]): away from the
/// ALab LAN (it was learned and this is not it), on a phone hotspot or cellular link (never the
/// ALab LAN, learned or not), or offline.
#[must_use]
pub const fn paced(place: &Place) -> bool {
    matches!(place.alab, Alab::Away { .. })
        || matches!(
            place.at,
            Where::Remote(Kind::Hotspot { .. } | Kind::Cellular) | Where::Offline
        )
}

/// Whether the automatic LAN lanes pace themselves now under the state root `state`.
/// Reuses only the pacing decision for [`crate::keeper::CACHE_FOR`], not a place snapshot;
/// fresh LAN admission proof remains independent of this scheduling hint.
#[cfg(unix)]
#[must_use]
pub fn paced_now(state: &Path) -> bool {
    {
        let cache = PACING.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(value) = cache.get(state, std::time::Instant::now()) {
            return value;
        }
    }
    let value = paced(&crate::keeper::read(state));
    // Observation time precedes any wait for the publishing lock, so contention
    // cannot extend the freshness window of a completed read.
    let at = std::time::Instant::now();
    PACING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remember(state, at, value);
    value
}

#[cfg(unix)]
static PACING: Mutex<PacingCache> = Mutex::new(PacingCache { entry: None });

/// The last pacing decision, keyed by state root and observation time.
#[cfg(unix)]
#[derive(Default)]
struct PacingCache {
    entry: Option<(PathBuf, std::time::Instant, bool)>,
}

#[cfg(unix)]
impl PacingCache {
    fn get(&self, state: &Path, now: std::time::Instant) -> Option<bool> {
        self.entry.as_ref().and_then(|(root, at, value)| {
            (root == state && now.duration_since(*at) < crate::keeper::CACHE_FOR).then_some(*value)
        })
    }

    fn remember(&mut self, state: &Path, at: std::time::Instant, value: bool) {
        if let Some((root, held_at, held_value)) = &mut self.entry
            && root == state
        {
            *held_at = at;
            *held_value = value;
            return;
        }
        self.entry = Some((state.to_path_buf(), at, value));
    }
}

/// The local address the route to an address takes (`route_source_ipv4`), injected.
type Via = Box<dyn Fn(IpAddr) -> Option<Ipv4Addr> + Send + Sync>;

/// Which member addresses the automatic lanes dial now.
pub struct Routes {
    place: Place,
    /// The fleet members (by [`member_key`]) that answered in this stay.
    heard: BTreeSet<[u8; 16]>,
    via: Via,
}

impl std::fmt::Debug for Routes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Routes")
            .field("place", &self.place.at)
            .field("heard", &self.heard.len())
            .finish_non_exhaustive()
    }
}

impl Routes {
    /// The routes `place` gives, with `evidence`'s fleet answers in its stay counted as heard
    /// here, and `via` naming the local address the route to each address takes.
    #[must_use]
    pub fn of(
        place: Place,
        evidence: &Evidence,
        via: impl Fn(IpAddr) -> Option<Ipv4Addr> + Send + Sync + 'static,
    ) -> Self {
        let heard = place.since_ms.map_or_else(BTreeSet::new, |since| {
            evidence
                .answers
                .iter()
                .filter(|answer| answer.lane == Lane::Fleet && answer.began_ms >= since)
                .filter_map(|answer| <[u8; 16]>::try_from(answer.member.as_bytes()).ok())
                .collect()
        });
        Self {
            place,
            heard,
            via: Box::new(via),
        }
    }

    /// The routes now, for the state root `state`: the place ([`crate::keeper::read`]), its
    /// evidence, and the kernel's routes (each through the scripted-facts seam where it is set).
    #[cfg(unix)]
    #[must_use]
    pub fn now(state: &std::path::Path) -> Self {
        let place = crate::keeper::read(state);
        let evidence = crate::heard::read(&crate::record::dir(state));
        Self::of(place, &evidence, crate::facts::route_via)
    }

    /// The place these routes come from.
    #[must_use]
    pub const fn place(&self) -> &Place {
        &self.place
    }

    /// Whether the automatic lanes pace their dialling: away from the ALab LAN (it was learned
    /// and this is not it), on a phone hotspot or cellular link (never the ALab LAN, learned or
    /// not), or offline. On the ALab LAN, while checking, and on a first run on another LAN
    /// (nothing learned to be away from) they dial as they always did.
    #[must_use]
    pub const fn paced(&self) -> bool {
        paced(&self.place)
    }

    /// The path the members are reached by now.
    #[must_use]
    pub const fn alab_path(&self) -> AlabPath {
        if self.paced() {
            AlabPath::None
        } else {
            AlabPath::Lan
        }
    }

    /// Whether this Mac wants a path to its members other than the LAN: the seam the internet
    /// path engages at ([`Place::wants_internet_path`]).
    #[must_use]
    pub fn wants_internet_path(&self) -> bool {
        self.place.wants_internet_path()
    }

    /// The network these routes are for, which a lane keys its once-a-network round to; `None`
    /// offline or before the network is read.
    #[must_use]
    pub const fn network(&self) -> Option<&NetworkId> {
        self.place.network.as_ref()
    }

    /// WHETHER AN AUTOMATIC LANE DIALS a member (`node`, its fleet node id where it has one) at
    /// `addr`: always where the lanes are not [`Self::paced`]; while they are, a member that
    /// answered in this stay, an address on this Mac (loopback), one on this network's subnet,
    /// and one whose route takes
    /// another interface than this network's (a VPN's tunnel); never one whose route leaves
    /// through this network's own interface toward the ALab LAN's addresses, one whose route the
    /// kernel names no local address for, or a member with no address to judge.
    #[must_use]
    pub fn dials(&self, node: Option<&str>, addr: Option<IpAddr>) -> bool {
        if !self.paced() {
            return true;
        }
        if !self.heard.is_empty()
            && node.is_some_and(|node| self.heard.contains(&member_key_bytes(Lane::Fleet, node)))
        {
            return true;
        }
        if addr.is_some_and(|addr| addr.to_canonical().is_loopback()) {
            return true;
        }
        let (Some(addr), Some(route)) = (addr, self.place.route.as_ref()) else {
            return false;
        };
        let addr = addr.to_canonical();
        if route.on_link(addr) {
            return true;
        }
        (self.via)(addr).is_some_and(|via| via != route.address)
    }

    /// Whether a round that dials every member is due: always where the lanes are not
    /// [`Self::paced`]; while they are, the first round on a network (`last` is the network and
    /// the unix-ms time of the last full round), and one every [`AWAY_FULL_EVERY_MS`] after.
    #[must_use]
    pub fn full_round_due(&self, last: Option<(Option<&NetworkId>, u64)>, now_ms: u64) -> bool {
        !self.paced()
            || last.is_none_or(|(network, at)| {
                network != self.network() || now_ms.saturating_sub(at) >= AWAY_FULL_EVERY_MS
            })
    }
}

/// The last full round each lane made per state root: the network it was on (`None` offline) and
/// when (unix ms), and whether this network's passing over was said.
type FullRounds = Vec<(PathBuf, &'static str, Option<NetworkId>, u64, bool)>;

/// [`FullRounds`], process-wide: every round of a lane in this process paces from one memory.
static FULL_ROUNDS: Mutex<FullRounds> = Mutex::new(Vec::new());

/// ONE AUTOMATIC ROUND'S PACING: which members it dials ([`Self::dials`]) given the routes now and
/// when this lane last dialled every member. A full round ([`Routes::full_round_due`]) dials all
/// of them and is remembered as begun; a paced one passes over each member behind the ALab LAN,
/// and the first paced round after each full one that passes any over says so in the log (one
/// line per lane every [`AWAY_FULL_EVERY`] at most). Being away gates nothing a person runs: only
/// the automatic lanes take a `Round`.
#[derive(Debug)]
pub struct Round {
    routes: Routes,
    full: bool,
    passed: usize,
    told: bool,
    lane: &'static str,
}

impl Round {
    /// The round `lane` (a short name: `fleet`, `vault`, `atpkg`) begins now under the state root
    /// `state`, on the routes now ([`Routes::now`]).
    #[cfg(unix)]
    #[must_use]
    pub fn begin(state: &Path, lane: &'static str) -> Self {
        Self::begin_at(state, lane, Routes::now(state), crate::unix_ms_now())
    }

    /// The round `lane` begins at `now_ms` under `state` on `routes`: full where it is due
    /// against this process's memory of the lane's last full round there, which a full round
    /// replaces.
    #[must_use]
    pub fn begin_at(state: &Path, lane: &'static str, routes: Routes, now_ms: u64) -> Self {
        let mut rounds = FULL_ROUNDS.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = rounds
            .iter()
            .position(|(root, name, ..)| root == state && *name == lane);
        let last = slot.map(|at| (rounds[at].2.as_ref(), rounds[at].3));
        let full = routes.full_round_due(last, now_ms);
        let told = slot.is_some_and(|at| rounds[at].4);
        if full {
            let network = routes.network().cloned();
            match slot {
                Some(at) => {
                    rounds[at].2 = network;
                    rounds[at].3 = now_ms;
                    rounds[at].4 = false;
                }
                None => rounds.push((state.to_path_buf(), lane, network, now_ms, false)),
            }
        }
        drop(rounds);
        Self {
            routes,
            full,
            passed: 0,
            told: told || full,
            lane,
        }
    }

    /// Whether this round dials every member.
    #[must_use]
    pub const fn full(&self) -> bool {
        self.full
    }

    /// The routes this round paces by.
    #[must_use]
    pub const fn routes(&self) -> &Routes {
        &self.routes
    }

    /// How many members this round passed over.
    #[must_use]
    pub const fn passed(&self) -> usize {
        self.passed
    }

    /// Whether `addr` lies on the network this round began on ([`crate::words::on_this_network`]):
    /// a dial there that found no route found the member off the link.
    #[must_use]
    pub fn on_this_network(&self, addr: IpAddr) -> bool {
        crate::words::on_this_network(&self.routes.place, addr)
    }

    /// Whether this round dials the member `node` at `addr`: every one in a full round, else as
    /// [`Routes::dials`] says; each passed over is counted.
    pub fn dials(&mut self, node: Option<&str>, addr: Option<IpAddr>) -> bool {
        let dials = self.full || self.routes.dials(node, addr);
        if !dials {
            self.passed += 1;
        }
        dials
    }

    /// Ends the round under `state`: the first paced round after a full one that passed members
    /// over says, in the log, how many and that every member is dialled again within
    /// [`AWAY_FULL_EVERY`] or on the next network.
    pub fn end(self, state: &Path) {
        if self.passed == 0 || self.told {
            return;
        }
        let mut rounds = FULL_ROUNDS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = rounds
            .iter_mut()
            .find(|(root, name, ..)| root == state && *name == self.lane)
        {
            entry.4 = true;
        }
        drop(rounds);
        aterm_log::info!(
            "{} lane: off the ALab LAN, so its rounds pass over the {} member(s) behind it; \
             every member is dialled once every {} min and on each network this Mac comes to",
            self.lane,
            self.passed,
            AWAY_FULL_EVERY_MS / 60_000
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::place::{Answer, InternetState, member_key};

    #[cfg(unix)]
    #[test]
    fn pacing_cache_separates_roots_and_expires_at_the_existing_boundary() {
        let mut cache = PacingCache::default();
        let first = Path::new("fixture/first");
        let other = Path::new("fixture/other");
        let at = std::time::Instant::now();
        let due = at + crate::keeper::CACHE_FOR;
        assert_eq!(cache.get(first, at), None);
        cache.remember(first, at, true);
        assert_eq!(cache.get(first, at), Some(true));
        assert_eq!(cache.get(other, at), None, "another root never borrows it");
        assert_eq!(
            cache.get(first, due - std::time::Duration::from_nanos(1)),
            Some(true)
        );
        assert_eq!(cache.get(first, due), None, "the boundary is exclusive");
        assert_eq!(
            cache.get(first, due + std::time::Duration::from_secs(1)),
            None
        );
        cache.remember(first, due, false);
        assert_eq!(
            cache.get(first, due),
            Some(false),
            "refresh replaces the decision"
        );
        cache.remember(other, due, true);
        assert_eq!(cache.get(other, due), Some(true));
        assert_eq!(cache.get(first, due), None, "only the last root is cached");
    }

    #[cfg(unix)]
    #[test]
    fn pacing_cache_hits_and_same_root_refreshes_allocate_nothing() {
        let mut cache = PacingCache::default();
        let root = Path::new("fixture/pacing-state");
        let at = std::time::Instant::now();
        let (_, control) =
            aterm_heap_count::allocations_in(|| std::hint::black_box(root.to_path_buf()));
        assert!(control > 0, "the allocation counter sees an owned root");
        cache.remember(root, at, true);
        let (_, allocations) = aterm_heap_count::allocations_in(|| {
            for step in 0..64 {
                let now = at + std::time::Duration::from_nanos(step);
                cache.remember(root, now, step % 2 == 0);
                assert_eq!(
                    std::hint::black_box(cache.get(root, now)),
                    Some(step % 2 == 0)
                );
            }
        });
        assert_eq!(
            allocations, 0,
            "the cache retains only its root and a boolean"
        );
    }

    const HOTSPOT_ADDR: Ipv4Addr = Ipv4Addr::new(172, 20, 10, 2);

    fn hotspot() -> Place {
        crate::sample::hotspot(1_000_000, 900_000)
    }

    /// The kernel's routes on the hotspot: the home LAN's addresses leave through the hotspot's
    /// own interface, `10.8.0.0/16` through a VPN's tunnel.
    fn via(addr: IpAddr) -> Option<Ipv4Addr> {
        match addr {
            IpAddr::V4(v4) if v4.octets()[0] == 10 => Some(Ipv4Addr::new(10, 8, 0, 2)),
            IpAddr::V4(v4) if v4.octets()[..3] == [198, 51, 100] => None,
            _ => Some(HOTSPOT_ADDR),
        }
    }

    fn ip(text: &str) -> Option<IpAddr> {
        Some(text.parse().expect("an address"))
    }

    /// Away on a hotspot, an automatic lane dials a member on the hotspot itself, one a VPN
    /// routes and one that answered in this stay, and none of the home LAN's addresses (they
    /// leave through the hotspot toward the internet) or members with no address; at home it
    /// dials every one.
    /// NEGATIVE CONTROL: answering `true` for every address once paced (dropping the route test,
    /// `(self.via)(addr).is_some_and(..)` → `true`) dials the home LAN's address from the hotspot,
    /// and "behind the ALab LAN" fails; dropping the loopback rule (`is_loopback()` →
    /// `is_loopback() && false`) passes over a member on this Mac, and "this Mac itself" fails.
    #[test]
    fn away_a_lane_dials_only_members_this_network_can_reach() {
        let mut evidence = Evidence::default();
        evidence.answers.push(Answer {
            lane: Lane::Fleet,
            member: member_key(Lane::Fleet, "heard-here"),
            addr: ip("192.168.4.9"),
            via: Some(HOTSPOT_ADDR),
            iface: Some("en0".to_owned()),
            network: None,
            logged: true,
            began_ms: 1_000_500,
            at_ms: 1_001_000,
        });
        evidence.answers.push(Answer {
            lane: Lane::Fleet,
            member: member_key(Lane::Fleet, "heard-at-home"),
            addr: ip("192.168.4.10"),
            via: Some(Ipv4Addr::new(192, 168, 4, 31)),
            iface: Some("en0".to_owned()),
            network: None,
            logged: true,
            began_ms: 500_000,
            at_ms: 500_100,
        });
        let away = Routes::of(hotspot(), &evidence, via);
        assert!(away.paced());
        assert_eq!(away.alab_path(), AlabPath::None);
        assert!(away.dials(None, ip("172.20.10.5")), "on the hotspot itself");
        assert!(away.dials(None, ip("10.8.3.4")), "a VPN routes it");
        assert!(
            away.dials(Some("heard-here"), ip("192.168.4.9")),
            "it answered in this stay"
        );
        assert!(
            !away.dials(Some("heard-at-home"), ip("192.168.4.10")),
            "behind the ALab LAN"
        );
        assert!(!away.dials(None, ip("192.168.4.77")), "behind the ALab LAN");
        assert!(!away.dials(None, None), "no address to judge");
        assert!(!away.dials(None, ip("198.51.100.7")), "no route named");
        assert!(away.dials(None, ip("127.0.0.1")), "this Mac itself");
        let home = Routes::of(crate::sample::home(1_000_000, 3), &evidence, via);
        assert!(!home.paced());
        assert_eq!(home.alab_path(), AlabPath::Lan);
        assert!(home.dials(None, ip("192.168.4.77")));
        assert!(home.dials(None, None));
    }

    /// Away, a round that dials every member is due once on each network this Mac comes to and
    /// once every half hour after, never in between; at home every round is.
    /// NEGATIVE CONTROL: dropping the network test (`network != self.network()`) makes the round
    /// right after a move wait out the half hour, and "the first round after a move" fails.
    #[test]
    fn away_every_member_is_dialled_after_each_move_and_each_half_hour() {
        let away = Routes::of(hotspot(), &Evidence::default(), via);
        let here = away.network().cloned();
        let home = Routes::of(crate::sample::home(1_000_000, 3), &Evidence::default(), via);
        let before = home.network().cloned();
        assert!(away.full_round_due(None, 1_000_000), "the first round");
        assert!(
            away.full_round_due(Some((before.as_ref(), 999_000)), 1_000_000),
            "the first round after a move"
        );
        assert!(!away.full_round_due(Some((here.as_ref(), 1_000_000)), 1_000_000 + 60_000));
        assert!(away.full_round_due(
            Some((here.as_ref(), 1_000_000)),
            1_000_000 + AWAY_FULL_EVERY_MS
        ));
        assert!(home.full_round_due(Some((before.as_ref(), 1_000_000)), 1_000_001));
    }

    /// A lane's rounds away: the first on a network dials every member, the next passes over the
    /// ones behind the ALab LAN (and counts them), the first on another network dials every one
    /// again; a lane under another state root, or another lane, paces from its own memory.
    /// NEGATIVE CONTROL: never remembering a full round (dropping the `rounds[at] = entry` /
    /// `rounds.push(entry)` write) makes every round full, and "the next round on it is paced"
    /// fails.
    #[test]
    fn a_lane_dials_every_member_once_per_network_then_paces() {
        let root = Path::new("/nonexistent/aterm-round-test");
        let away = || Routes::of(hotspot(), &Evidence::default(), via);
        let behind = ip("192.168.4.77");
        let mut first = Round::begin_at(root, "fleet", away(), 1_000_000);
        assert!(first.full(), "the first round on a network");
        assert!(first.dials(None, behind));
        first.end(root);
        let mut next = Round::begin_at(root, "fleet", away(), 1_060_000);
        assert!(!next.full(), "the next round on it is paced");
        assert!(!next.dials(None, behind));
        assert!(next.dials(None, ip("172.20.10.5")));
        assert_eq!(next.passed(), 1);
        next.end(root);
        assert!(
            Round::begin_at(root, "vault", away(), 1_060_000).full(),
            "another lane's own memory"
        );
        assert!(
            Round::begin_at(Path::new("/nonexistent/other"), "fleet", away(), 1_060_000).full(),
            "another state root's own memory"
        );
        let home = Routes::of(crate::sample::home(1_100_000, 3), &Evidence::default(), via);
        assert!(
            Round::begin_at(root, "fleet", home, 1_100_000).full(),
            "at home"
        );
        assert!(
            Round::begin_at(root, "fleet", away(), 1_120_000).full(),
            "back on the hotspot after home: another network"
        );
    }

    /// The internet-path seam engages only away with the internet answering.
    #[test]
    fn the_internet_path_is_wanted_away_with_the_internet_answering() {
        let mut place = hotspot();
        assert!(!Routes::of(place.clone(), &Evidence::default(), via).wants_internet_path());
        place.internet.state = InternetState::Ok;
        assert!(Routes::of(place, &Evidence::default(), via).wants_internet_path());
    }
}
