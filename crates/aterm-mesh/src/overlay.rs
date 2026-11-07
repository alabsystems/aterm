// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The overlay (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, "The overlay"): each Mac keeps
//! k = 4 sticky neighbours among the current seats it can reach. Every 10 minutes it swaps one for
//! a random other seat, and it replaces a dead one at once. With fewer than k up it dials seats from
//! its address cache and the `_alab._tcp` browse until it has k. With fewer than 6 members, every
//! member is a neighbour.
//!
//! [`Overlay`] is the rule alone, a pure state machine stepped on what this Mac sees ([`Sight`]):
//! it answers which seats to dial and which links it dialed to let go ([`Plan`]). [`Attached`] runs
//! it over one generation's neighbour table (`aterm_net::fleet::link::Neighbours`), and runs the
//! relays: one push relay per log on every live link to a current seat, whichever end dialed it,
//! so a link carries records both ways; a seat's link is one whose peer proved the seat's key, not
//! another key of its principal. Its thread parks on the link clock until its next timer
//! (a swap, a dial's back-off, a minute's recheck) or its bell, which a link joining
//! ([`crate::Mesh::poke`]), a relay ending, a dial ending, a membership change and a neighbour's
//! `Bus` stream ring.

use crate::logs::{Mesh, MeshLog};
use crate::relay::{self, Slot};
use aterm_net::fleet::link::{Bell, Link, Neighbours};
use aterm_types::MutexExt;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

/// The sticky neighbours each Mac keeps.
pub const K: usize = 4;

/// A lab with fewer members than this makes every member a neighbour.
pub const FEW: usize = 6;

/// How often one neighbour is swapped for a random other seat.
pub const SWAP_EVERY: Duration = Duration::from_secs(10 * 60);

/// The overlay's own recheck, beside its bell.
pub const RECHECK: Duration = Duration::from_secs(60);

/// The first wait before a seat whose dial failed is dialed again, doubled each failure up to
/// [`DIAL_MAX`].
pub const DIAL_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between dials of one seat.
pub const DIAL_MAX: Duration = Duration::from_secs(5 * 60);

/// The bound on one dial and its handshake.
pub const DIAL_WITHIN: Duration = Duration::from_secs(10);

/// What this Mac sees when the overlay steps: the other current seats, those it holds a live link
/// to and which of those it dialed, those it is dialing, and those it has an address for, each by
/// principal.
#[derive(Clone, Copy, Debug)]
pub struct Sight<'a> {
    /// The link clock now.
    pub now: Duration,
    /// Every other current seat.
    pub seats: &'a BTreeSet<String>,
    /// The seats this Mac holds a live link to.
    pub up: &'a BTreeSet<String>,
    /// The live links this Mac dialed itself, by the seat each reached.
    pub dialed: &'a BTreeSet<String>,
    /// The seats a dial is under way to.
    pub dialing: &'a BTreeSet<String>,
    /// The seats this Mac holds an address for: the only ones it can dial.
    pub reachable: &'a BTreeSet<String>,
}

/// What a step asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Seats to dial now.
    pub dial: Vec<String>,
    /// Seats this Mac dialed a live link to that are not neighbours now: let go.
    pub drop: Vec<String>,
}

/// The overlay's rule: which seats are this Mac's neighbours.
#[derive(Clone, Debug)]
pub struct Overlay {
    chosen: BTreeSet<String>,
    /// Chosen seats seen linked: one that is down after is dead.
    seen_up: BTreeSet<String>,
    next_swap: Option<Duration>,
    /// Each seat whose dial failed: when it may be dialed again, and the back-off that set it.
    retry: BTreeMap<String, (Duration, Duration)>,
    rng: u64,
}

impl Overlay {
    /// An overlay with no neighbours, whose random choices follow `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            chosen: BTreeSet::new(),
            seen_up: BTreeSet::new(),
            next_swap: None,
            retry: BTreeMap::new(),
            rng: seed,
        }
    }

    /// The neighbours chosen now.
    #[must_use]
    pub const fn chosen(&self) -> &BTreeSet<String> {
        &self.chosen
    }

    /// A dial of `node` ended with no link to it at `now`: it waits its back-off before the next.
    pub fn failed(&mut self, node: &str, now: Duration) {
        if let Some(slot) = self.retry.get_mut(node) {
            let backoff = if slot.1.is_zero() {
                DIAL_FIRST
            } else {
                slot.1.saturating_mul(2).min(DIAL_MAX)
            };
            *slot = (now.saturating_add(backoff), backoff);
        } else {
            self.retry.insert(
                node.to_owned(),
                (now.saturating_add(DIAL_FIRST), DIAL_FIRST),
            );
        }
    }

    /// The earliest time past `now` the overlay would plan something new by itself: the next swap
    /// or the end of a dial's back-off.
    #[must_use]
    pub fn next_wake(&self, now: Duration) -> Option<Duration> {
        self.retry
            .values()
            .map(|(at, _)| *at)
            .filter(|at| *at > now)
            .chain(self.next_swap)
            .min()
    }

    /// A splitmix64 draw.
    fn draw(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// One of `among` at random.
    fn pick<'a>(&mut self, among: &[&'a String]) -> Option<&'a String> {
        let len = u64::try_from(among.len()).ok().filter(|len| *len > 0)?;
        let at = usize::try_from(self.draw() % len).ok()?;
        among.get(at).copied()
    }

    fn backing_off(&self, node: &str, now: Duration) -> bool {
        self.retry.get(node).is_some_and(|(at, _)| *at > now)
    }

    /// Step on `sight`: forget what is no longer a seat, replace each dead neighbour (one seen
    /// linked that is down, one whose dial failed, one with no link and no address), fill up to
    /// [`K`] from the seats linked or reachable (linked first), swap one every [`SWAP_EVERY`], and
    /// plan the dials and the let-goes: every link this Mac dialed to a seat that is not a
    /// neighbour now. With fewer than [`FEW`] members every seat is a neighbour, and none is
    /// swapped.
    pub fn step(&mut self, sight: &Sight<'_>) -> Plan {
        let now = sight.now;
        let usable = |node: &String| sight.up.contains(node) || sight.reachable.contains(node);
        for node in sight.up {
            self.retry.remove(node);
        }
        self.retry.retain(|node, _| sight.seats.contains(node));
        self.chosen.retain(|node| sight.seats.contains(node));
        for node in &self.chosen {
            if sight.up.contains(node) && !self.seen_up.contains(node) {
                self.seen_up.insert(node.clone());
            }
        }
        if sight.seats.len().saturating_add(1) < FEW {
            self.chosen.clone_from(sight.seats);
            self.next_swap = None;
        } else {
            let dead: Vec<String> = self
                .chosen
                .iter()
                .filter(|node| {
                    !sight.up.contains(*node)
                        && !sight.dialing.contains(*node)
                        && (self.seen_up.contains(*node)
                            || self.backing_off(node, now)
                            || !sight.reachable.contains(*node))
                })
                .cloned()
                .collect();
            for node in &dead {
                self.chosen.remove(node);
            }
            if self.chosen.len() < K {
                let mut candidates: Vec<&String> = sight
                    .seats
                    .iter()
                    .filter(|node| {
                        !self.chosen.contains(*node)
                            && !dead.contains(*node)
                            && !self.backing_off(node, now)
                            && usable(node)
                    })
                    .collect();
                let mut linked: Vec<&String> = candidates
                    .iter()
                    .copied()
                    .filter(|node| sight.up.contains(*node))
                    .collect();
                while self.chosen.len() < K {
                    let pick = if linked.is_empty() {
                        self.pick(&candidates)
                    } else {
                        self.pick(&linked)
                    };
                    match pick {
                        Some(node) => {
                            self.chosen.insert(node.clone());
                            candidates.retain(|&cand| cand != node);
                            linked.retain(|&cand| cand != node);
                        }
                        None => break,
                    }
                }
            }
            if self.chosen.len() < K {
                self.next_swap = None;
            } else {
                match self.next_swap {
                    None => self.next_swap = Some(now.saturating_add(SWAP_EVERY)),
                    Some(at) if now >= at => {
                        let len = u64::try_from(self.chosen.len()).unwrap_or(1);
                        let out = usize::try_from(self.draw() % len)
                            .ok()
                            .and_then(|idx| self.chosen.iter().nth(idx).cloned());
                        let candidates: Vec<&String> = sight
                            .seats
                            .iter()
                            .filter(|node| {
                                !self.chosen.contains(*node)
                                    && !dead.contains(*node)
                                    && !self.backing_off(node, now)
                                    && usable(node)
                            })
                            .collect();
                        if let (Some(out), Some(into)) = (out, self.pick(&candidates).cloned()) {
                            self.chosen.remove(&out);
                            self.chosen.insert(into);
                        }
                        self.next_swap = Some(now.saturating_add(SWAP_EVERY));
                    }
                    Some(_) => {}
                }
            }
        }
        let chosen = &self.chosen;
        self.seen_up.retain(|node| chosen.contains(node));
        Plan {
            dial: self
                .chosen
                .iter()
                .filter(|node| {
                    !sight.up.contains(*node)
                        && !sight.dialing.contains(*node)
                        && sight.reachable.contains(*node)
                        && !self.backing_off(node, now)
                })
                .cloned()
                .collect(),
            drop: sight
                .dialed
                .iter()
                .filter(|node| sight.seats.contains(*node) && !self.chosen.contains(*node))
                .cloned()
                .collect(),
        }
    }
}

/// Where the overlay finds addresses to dial: each seat's principal and an address it was heard
/// at (the node service's address cache and browse). Every dial authenticates again.
pub type Hints = Arc<dyn Fn() -> Vec<(String, SocketAddr)> + Send + Sync>;

/// The dials under way and the ones that ended since the overlay last looked.
#[derive(Default)]
struct Dials {
    dialing: BTreeSet<String>,
    /// Each seat whose dial ended, and whether it linked that seat.
    ended: Vec<(String, bool)>,
}

struct Runtime {
    mesh: Arc<Mesh>,
    table: Arc<Neighbours>,
    hints: Hints,
    bell: Arc<Bell>,
    stop: AtomicBool,
    dials: Mutex<Dials>,
    chosen: Mutex<BTreeSet<String>>,
}

/// The overlay and the relays running over one generation's neighbour table: dropped, every relay
/// stops and is joined, with the overlay's thread. A dial under way is not joined: its connect
/// waits out its bound ([`DIAL_WITHIN`]) and nothing cuts it short, and a link it makes then is
/// refused by the stopped table.
pub struct Attached {
    runtime: Arc<Runtime>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Attached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attached").finish_non_exhaustive()
    }
}

impl Mesh {
    /// Run the overlay and the relays over `table`, dialing seats at the addresses `hints` gives.
    /// # Errors
    /// Where the overlay's thread cannot start.
    pub fn attach(self: &Arc<Self>, table: &Arc<Neighbours>, hints: Hints) -> io::Result<Attached> {
        let runtime = Arc::new(Runtime {
            mesh: Arc::clone(self),
            table: Arc::clone(table),
            hints,
            bell: Arc::new(Bell::default()),
            stop: AtomicBool::new(false),
            dials: Mutex::new(Dials::default()),
            chosen: Mutex::new(BTreeSet::new()),
        });
        *self.wake.lock_or_recover() = Some(Arc::downgrade(&runtime.bell));
        let running = Arc::clone(&runtime);
        let thread = std::thread::Builder::new()
            .name("aterm-mesh-overlay".to_owned())
            .spawn(move || running.run())?;
        Ok(Attached {
            runtime,
            thread: Some(thread),
        })
    }
}

impl Attached {
    /// Wake the overlay now: a link joined the table.
    pub fn poke(&self) {
        self.runtime.bell.ring();
    }

    /// The neighbours the overlay chose at its last step.
    #[must_use]
    pub fn chosen(&self) -> BTreeSet<String> {
        self.runtime.chosen.lock_or_recover().clone()
    }
}

impl Attached {
    /// Ask the overlay and its relays to stop, without waiting: the node service then closes the
    /// links, which ends every relay still waiting on one, and the drop joins them.
    pub fn stop_soon(&self) {
        self.runtime.stop.store(true, Ordering::Release);
        self.runtime.bell.ring();
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.runtime.stop.store(true, Ordering::Release);
        self.runtime.bell.ring();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The principal of the node key a link's peer proved.
fn principal_of(link: &Link) -> String {
    astream_broker::identity_principal(link.peer().key.bytes())
}

/// Whether a link whose peer proved `key` stands for its principal's node: `seats` seats that
/// key under the principal, or seats none there. A broker files a key by its 64-bit principal, so
/// another key of a seated principal would pass for the seat; the seat's link is the one whose
/// key is the seat's, as `Mesh::admits` takes a stream by the key alone.
pub(crate) fn stands_for_its_node(key: &[u8; 32], seats: &BTreeMap<String, [u8; 32]>) -> bool {
    let buf = crate::logs::principal_buf(key);
    seats
        .get(crate::logs::principal_str(&buf))
        .is_none_or(|seat| seat == key)
}

/// Whether `link`'s peer proved a current seat's key ([`Mesh::seats`]), not merely its principal:
/// what a dial to a seat, for its relays or its mail, must reach.
pub(crate) fn proves_a_seat(mesh: &Mesh, link: &Link) -> bool {
    mesh.admits(link.peer().key.bytes()) == Some(true)
}

type Relays = BTreeMap<String, BTreeMap<MeshLog, (Arc<Slot>, JoinHandle<()>)>>;

impl Runtime {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire) || self.mesh.is_stopped()
    }

    fn run(self: &Arc<Self>) {
        let mesh = &self.mesh;
        let key = mesh.key();
        let mut seed = [0u8; 8];
        seed.copy_from_slice(&key[..8]);
        let mut overlay = Overlay::new(u64::from_le_bytes(seed));
        let mut relays: Relays = BTreeMap::new();
        let mut ending: Vec<JoinHandle<()>> = Vec::new();
        let mut hooked: BTreeMap<String, Weak<Link>> = BTreeMap::new();
        let mut generation = mesh.generation.load(Ordering::Acquire);
        while !self.stopped() {
            let now = mesh.clock.now();
            let ended = std::mem::take(&mut self.dials.lock_or_recover().ended);
            for (node, linked) in ended {
                if !linked {
                    overlay.failed(&node, now);
                }
            }
            let seats = mesh.seats_by_principal().unwrap_or_default();
            let links: BTreeMap<String, Arc<Link>> = self
                .table
                .links()
                .into_iter()
                .filter(|link| stands_for_its_node(link.peer().key.bytes(), &seats))
                .map(|link| (principal_of(&link), link))
                .collect();
            self.hook_ends(&mut hooked, &links);
            let changed = mesh.generation.load(Ordering::Acquire);
            let kick_all = changed != generation;
            generation = changed;
            let arrived = std::mem::take(&mut *mesh.arrived.lock_or_recover());
            self.keep_relays(&mut relays, &mut ending, &seats, &links, kick_all, &arrived);
            ending.retain(|handle| !handle.is_finished());
            let names: BTreeSet<String> = seats.keys().cloned().collect();
            let up: BTreeSet<String> = links
                .keys()
                .filter(|node| seats.contains_key(*node))
                .cloned()
                .collect();
            let dialed: BTreeSet<String> = links
                .iter()
                .filter(|(_, link)| link.dialed())
                .map(|(node, _)| node.clone())
                .collect();
            let hinted = (self.hints)();
            let mut reachable = BTreeSet::new();
            for (node, _) in &hinted {
                if !reachable.contains(node) {
                    reachable.insert(node.clone());
                }
            }
            let dialing = self.dials.lock_or_recover().dialing.clone();
            let plan = overlay.step(&Sight {
                now,
                seats: &names,
                up: &up,
                dialed: &dialed,
                dialing: &dialing,
                reachable: &reachable,
            });
            self.chosen.lock_or_recover().clone_from(overlay.chosen());
            for node in &plan.drop {
                if let Some(link) = links.get(node)
                    && link.dialed()
                {
                    link.close("the overlay swapped this neighbour for another");
                }
            }
            // A dial that could not start fails now: step again at once, so its seat is replaced
            // while it backs off, not when the back-off ends.
            let mut failed = false;
            for node in plan.dial {
                if !self.dial(&node, &hinted) {
                    overlay.failed(&node, now);
                    failed = true;
                }
            }
            if failed {
                continue;
            }
            let next = overlay
                .next_wake(now)
                .map_or(now.saturating_add(RECHECK), |at| {
                    at.min(now.saturating_add(RECHECK))
                });
            if self.stopped() {
                break;
            }
            mesh.clock.park(next, &self.bell);
        }
        for by_log in relays.values() {
            for (slot, _) in by_log.values() {
                slot.stop();
            }
        }
        for (_, by_log) in relays {
            for (_, (_, handle)) in by_log {
                let _ = handle.join();
            }
        }
        for handle in ending {
            let _ = handle.join();
        }
    }

    /// Ring this runtime's bell when a link it has not seen before ends, so a dead neighbour is
    /// replaced and its relays stopped at once rather than at the next recheck. The hook holds
    /// only the bell, weakly: it keeps neither the link nor the runtime.
    fn hook_ends(
        &self,
        hooked: &mut BTreeMap<String, Weak<Link>>,
        links: &BTreeMap<String, Arc<Link>>,
    ) {
        hooked.retain(|node, held| {
            links
                .get(node)
                .is_some_and(|link| std::ptr::eq(held.as_ptr(), Arc::as_ptr(link)))
        });
        for (node, link) in links {
            if hooked.contains_key(node) {
                continue;
            }
            let bell = Arc::downgrade(&self.bell);
            link.on_end(Box::new(move || {
                if let Some(bell) = bell.upgrade() {
                    bell.ring();
                }
            }));
            hooked.insert(node.clone(), Arc::downgrade(link));
        }
    }

    /// Run one relay per log on every live link to a current seat, stop the rest, and kick the
    /// waiting ones a membership change or the neighbour's own stream concerns.
    fn keep_relays(
        &self,
        relays: &mut Relays,
        ending: &mut Vec<JoinHandle<()>>,
        seats: &BTreeMap<String, [u8; 32]>,
        links: &BTreeMap<String, Arc<Link>>,
        kick_all: bool,
        arrived: &BTreeSet<[u8; 32]>,
    ) {
        let wall = relay::wall_ms(&self.mesh.wall);
        let kept = std::mem::take(relays);
        for (peer, by_log) in kept {
            let seat_key = seats.get(&peer);
            let live_link = links.get(&peer);
            let kick_peer = kick_all || seat_key.is_some_and(|key| arrived.contains(key));
            let mut kept_logs = BTreeMap::new();
            for (log, (slot, handle)) in by_log {
                let keep = !handle.is_finished()
                    && seat_key.is_some()
                    && live_link.is_some_and(|link| Arc::ptr_eq(link, &slot.link));
                if !keep {
                    slot.stop();
                    ending.push(handle);
                    continue;
                }
                if kick_peer {
                    slot.kick();
                }
                if slot.following.load(Ordering::Acquire) {
                    self.mesh
                        .hosted(log)
                        .relayed
                        .lock_or_recover()
                        .touch(&peer, wall);
                }
                kept_logs.insert(log, (slot, handle));
            }
            if !kept_logs.is_empty() {
                relays.insert(peer, kept_logs);
            }
        }
        for (peer, link) in links {
            if !seats.contains_key(peer) {
                continue;
            }
            if relays
                .get(peer)
                .is_some_and(|by_log| by_log.len() == MeshLog::ALL.len())
            {
                continue;
            }
            for log in MeshLog::ALL {
                if relays
                    .get(peer)
                    .is_some_and(|by_log| by_log.contains_key(&log))
                {
                    continue;
                }
                let slot = Slot::new(peer.clone(), log, Arc::clone(link));
                let mesh = Arc::clone(&self.mesh);
                let running = Arc::clone(&slot);
                let started = std::thread::Builder::new()
                    .name(format!("aterm-mesh-relay-{}", log.word()))
                    .spawn(move || relay::run(&mesh, &running));
                match started {
                    Ok(handle) => {
                        relays
                            .entry(peer.clone())
                            .or_default()
                            .insert(log, (slot, handle));
                    }
                    Err(e) => aterm_log::warn!(
                        "mesh: the {} relay to {peer} could not start: {e}",
                        log.word()
                    ),
                }
            }
        }
    }

    /// Dial `node` at the addresses `hinted` gives it, on a thread of its own. `false` where no
    /// address is known or the thread could not start.
    fn dial(self: &Arc<Self>, node: &str, hinted: &[(String, SocketAddr)]) -> bool {
        if !hinted.iter().any(|(named, _)| named == node) {
            return false;
        }
        let mut dials = self.dials.lock_or_recover();
        if dials.dialing.contains(node) {
            return true;
        }
        let named = node.to_owned();
        dials.dialing.insert(named.clone());
        let addrs: Vec<SocketAddr> = hinted
            .iter()
            .filter(|(named, _)| named == node)
            .map(|(_, addr)| *addr)
            .collect();
        let runtime = Arc::clone(self);
        let started = std::thread::Builder::new()
            .name("aterm-mesh-dial".to_owned())
            .spawn(move || {
                let mut linked = false;
                for addr in addrs {
                    if runtime.stopped() {
                        break;
                    }
                    match runtime.table.dial(addr, DIAL_WITHIN) {
                        Ok(link)
                            if crate::logs::principal_str(&crate::logs::principal_buf(
                                link.peer().key.bytes(),
                            )) == named
                                && proves_a_seat(&runtime.mesh, &link) =>
                        {
                            linked = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => aterm_log::info!(
                            "mesh: a dial of {named} at {addr} failed: {}",
                            e.error
                        ),
                    }
                }
                let mut dials = runtime.dials.lock_or_recover();
                dials.dialing.remove(&named);
                dials.ended.push((named, linked));
                drop(dials);
                runtime.bell.ring();
            });
        match started {
            // Not joined (see [`Attached`]): the thread ends by itself within its bound.
            Ok(_) => true,
            Err(e) => {
                dials.dialing.remove(node);
                aterm_log::warn!("mesh: a dial of {node} could not start: {e}");
                false
            }
        }
    }
}
