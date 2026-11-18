// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a subscriber's pass asks beside its source ([`Gate`]).
//!
//! A resting subscriber needs only its source. The vault's servers take each version and record
//! from one another by themselves, so a peer holds nothing for this Mac that its source lacks for
//! longer than their replication takes. Each pass therefore asks its source alone
//! ([`Gate::SourceFirst`]), and asks the peers too only where the source fell short: it failed, it
//! holds no such vault, it holds an older version than this Mac's, or its records could not be
//! taken whole. A pass after the loop's routes moved, after a pass that failed or whose source
//! said nothing, and the loop's first, asks every peer its plan cuts as every pass did before
//! ([`Gate::CatchUp`]): what the routes or a failure hid may sit anywhere. And at a sweep, once
//! every [`crate::server::sweep_after`] (12 to 18 minutes), a pass asks a few peers whatever the
//! source says ([`Gate::Sweep`], [`Sweeps`]), and asks the next few [`SWEEP_RETRY`] later until
//! one that serves this vault, and is not the source at another name, answers ([`Heard`]): a
//! source that answers but holds an old version for good is overtaken by the first such peer
//! that holds a newer one.

use super::*;
use std::collections::HashMap;
#[cfg(unix)]
use std::collections::HashSet;

/// How many peers a sweep asks ([`Sweeps::choose`]).
#[cfg(unix)]
pub(super) const SWEEP_PEERS: usize = 4;

/// How soon after a sweep no peer answered the next peers are asked ([`Sweeps::swept`]): no
/// sooner than a server's round pulls one group again.
#[cfg(unix)]
pub(super) const SWEEP_RETRY: Duration = crate::server::PULL_SPACING;

/// How many peers' last tries a subscriber keeps ([`Sweeps::tried`]), and so the most peers one
/// period tries: a fleet of the size [`crate::membership_log::untaken::UNTAKEN_URLS`] is chosen
/// for. Past it a try made before the running period goes, the one made longest ago first, and is
/// first in line again, where it stood anyway; a try made in the running period never goes, so a
/// peer the period asked is not asked again in it, and a period that has tried this many ends.
#[cfg(unix)]
const SWEEP_KEPT: usize = crate::membership_log::untaken::UNTAKEN_URLS;

/// What one subscriber pass asks beside its source.
pub(super) enum Gate {
    /// Every peer its plan cuts, beside the source, for records and for the envelope, as every
    /// pass asked before the gate: the loop's first pass, and one after its routes moved, a pass
    /// failed or a source said nothing.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the Unix subscriber loop's `gated_pass` catches up"
        )
    )]
    CatchUp,
    /// The peers a sweep chose ([`Sweeps::choose`]), beside the source, for records and for the
    /// envelope; what each answered is kept in its [`Heard`].
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the Unix subscriber loop's `gated_pass` sweeps"
        )
    )]
    Sweep(Heard),
    /// The source first, on the loop's thread, and the peers only where it fell short: for
    /// records, where it failed, stated no records bound where the log crossed, or sent a stream
    /// the store could not be offered whole ([`RecordsPass::source_short`]); for the envelope,
    /// where it said nothing, held no such vault, or held an older version than this Mac's
    /// ([`source_fell_short`]).
    SourceFirst,
}

impl Gate {
    /// What the sweep's peers answered, where this pass is a sweep.
    pub(super) const fn heard(&self) -> Option<&Heard> {
        match self {
            Self::Sweep(heard) => Some(heard),
            Self::CatchUp | Self::SourceFirst => None,
        }
    }
}

/// What each peer a sweep asked answered, by the endpoint its URL names
/// ([`transport::describe`]): whether it answered every request the pass made of it as a server
/// of this vault apart from the source. A records request is answered where the peer served the
/// vault's records (a `200` or `204`, which name its boot id; never a `404`) under another boot
/// id than the source's (one with the source's own is the source, reached at another name); the
/// envelope's where it answered a `204` or a head that verifies, never a `404`.
#[derive(Default)]
pub(super) struct Heard(std::sync::Mutex<HashMap<String, bool>>);

impl Heard {
    /// The peer at `url` answered a request (`answered`), or did not.
    pub(super) fn note(&self, url: &str, answered: bool) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(transport::describe(url).to_ascii_lowercase())
            .and_modify(|every| *every &= answered)
            .or_insert(answered);
    }

    /// How many peers answered every request the pass made of them.
    #[cfg(unix)]
    pub(super) fn answered(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|every| **every)
            .count()
    }
}

/// Whether the envelope a source answered a gated pass with leaves the pass to ask the peers
/// ([`Gate::SourceFirst`]): it said nothing, held no such vault, named an older version than this
/// Mac holds (`before`, which the Mac puts back), or served a body that does not verify under the
/// pin at this Mac's version or later (anyone on the LAN can answer at the source's name). A `204`
/// at this Mac's version, a held request that heard nothing new, and a body that advanced
/// (`advanced`) or verified at this Mac's version (`verified`, its seq) ask nobody.
pub(super) fn source_fell_short(
    primary: &crate::Result<Fetched>,
    before: &State,
    advanced: bool,
    verified: Option<u64>,
) -> bool {
    match primary {
        Err(_) | Ok(Fetched::Missing) => true,
        Ok(Fetched::NoNewer(Some(seq))) => *seq < before.seq,
        Ok(Fetched::NoNewer(None)) => false,
        Ok(Fetched::Body(_)) => !advanced && verified.is_none_or(|seq| seq < before.seq),
    }
}

/// Which pass the loop runs next ([`Sweeps::turn`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Turn {
    /// [`Gate::CatchUp`].
    CatchUp,
    /// [`Gate::Sweep`].
    Sweep,
    /// [`Gate::SourceFirst`].
    SourceFirst,
}

/// When a subscriber's next sweep comes, and which peers it asks ([`Gate::Sweep`]).
///
/// A sweep comes due [`crate::server::sweep_after`] after the last pass that asked beyond the
/// source and finished: a catch-up, or a sweep in which a peer answered every request. Each sweep
/// asks at most [`SWEEP_PEERS`] of the peers its period has not tried yet, those never tried
/// first and then those tried longest ago, each name resolved once for the whole pass. Where none
/// of them answered, the next are asked [`SWEEP_RETRY`] later; a period that has tried every peer,
/// or [`SWEEP_KEPT`] of them, ends whatever they answered. So a peer that does not answer is asked
/// at most once a period, never at every pass, and holds no sweep due: on a LAN where most names
/// heard never answer, a period still ends once one answers or after at most
/// `SWEEP_KEPT / SWEEP_PEERS` sweeps, and the next starts with the peers it did not reach.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(super) struct Sweeps {
    /// When the next sweep comes due: none before the loop's first pass, which catches up.
    due: Option<Instant>,
    /// When the sweep that runs now came due: a peer tried since then waits for the next period.
    period: Option<Instant>,
    /// Each peer's last try, by the key [`PeerDiscovery::sweep`] knows it by (its endpoint, or
    /// the name the browse heard), at most [`SWEEP_KEPT`].
    tried: HashMap<String, Instant>,
    /// Whether the last sweep's period left peers it had not tried.
    left: bool,
}

#[cfg(unix)]
impl Sweeps {
    /// The pass the loop runs at `now`: a catch-up where the loop's routes moved or its last pass
    /// owes one (`moved`), and before any pass; a sweep once one is due; the source first
    /// otherwise.
    pub(super) fn turn(&mut self, now: Instant, moved: bool) -> Turn {
        match self.due {
            None => Turn::CatchUp,
            Some(_) if moved => Turn::CatchUp,
            Some(due) if now >= due => {
                self.period.get_or_insert(due);
                Turn::Sweep
            }
            Some(_) => Turn::SourceFirst,
        }
    }

    /// A catch-up ran at `now`: it asked every peer the plan cuts, so the next sweep comes
    /// [`crate::server::sweep_after`]`(unit)` later, and a period that was running ends.
    pub(super) fn caught_up(&mut self, now: Instant, unit: u64) {
        self.due = Some(now + crate::server::sweep_after(unit));
        self.period = None;
        self.left = false;
    }

    /// At most `limit` of `targets` (each by its key) for a sweep at `now`: those its period has
    /// not tried yet, never tried first, then tried longest ago, then by key, and no more than
    /// keep the period's tries within [`SWEEP_KEPT`]. Each is marked tried at `now`; a target no
    /// longer offered is forgotten, and past [`SWEEP_KEPT`] tries those made before the period
    /// go, the oldest first. The period leaves peers for a next sweep only while it has tried
    /// fewer than [`SWEEP_KEPT`].
    pub(super) fn choose<T>(
        &mut self,
        targets: Vec<(String, T)>,
        now: Instant,
        limit: usize,
    ) -> Vec<T> {
        let period = self.period.unwrap_or(now);
        let offered: HashSet<&str> = targets.iter().map(|(key, _)| key.as_str()).collect();
        self.tried.retain(|key, _| offered.contains(key.as_str()));
        let mut due: Vec<(Option<Instant>, String, T)> = targets
            .into_iter()
            .filter_map(|(key, target)| {
                let at = self.tried.get(&key).copied();
                at.is_none_or(|at| at < period).then_some((at, key, target))
            })
            .collect();
        due.sort_by(|(a, a_key, _), (b, b_key, _)| (a, a_key).cmp(&(b, b_key)));
        let tried_in_period = self.tried.values().filter(|at| **at >= period).count();
        let chosen = limit
            .min(due.len())
            .min(SWEEP_KEPT.saturating_sub(tried_in_period));
        self.left = due.len() > chosen && tried_in_period + chosen < SWEEP_KEPT;
        due.truncate(chosen);
        for (_, key, _) in &due {
            self.tried.insert(key.clone(), now);
        }
        // Only tries made before the period go: one made in it keeps its peer from being asked
        // again in it, and the period's tries are no more than SWEEP_KEPT.
        while self.tried.len() > SWEEP_KEPT {
            let Some(oldest) = self
                .tried
                .iter()
                .filter(|(_, at)| **at < period)
                .min_by_key(|(_, at)| **at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.tried.remove(&oldest);
        }
        due.into_iter().map(|(_, _, target)| target).collect()
    }

    /// A sweep ran at `now`, and of the `asked` peers it chose, `answered` answered every request
    /// it made of them: where one did, or the period tried every peer, the next sweep comes
    /// [`crate::server::sweep_after`]`(unit)` later; otherwise the next peers are asked
    /// [`SWEEP_RETRY`] later. A peer that did not answer keeps no sweep due.
    pub(super) fn swept(&mut self, now: Instant, asked: usize, answered: usize, unit: u64) {
        debug_assert!(answered <= asked, "{answered} of {asked} peers answered");
        if answered > 0 || !self.left {
            self.caught_up(now, unit);
        } else {
            self.due = Some(now + SWEEP_RETRY);
        }
    }
}

/// What the subscriber's loop keeps between passes of what makes the next a catch-up
/// ([`Gate::CatchUp`]): the host's routes as the last pass began, and whether that pass owes one.
#[cfg(unix)]
#[derive(Debug, Default)]
pub(super) struct Moves {
    /// The last pass failed or its source said nothing.
    owed: bool,
    /// [`crate::server::route_sources`] as the last pass began; none before the first.
    routes: Option<[Option<std::net::IpAddr>; 3]>,
}

#[cfg(unix)]
impl Moves {
    /// Whether the pass that begins at `routes` catches up: the last pass owes one, or the routes
    /// moved since it began.
    pub(super) fn before_pass(&mut self, routes: [Option<std::net::IpAddr>; 3]) -> bool {
        let moved = self.owed || self.routes.is_some_and(|last| last != routes);
        self.routes = Some(routes);
        moved
    }

    /// A pass ended: where it failed (`failed`) or its source said nothing (`said`: what it read
    /// came from the servers found, or a local repair stood in for the source), the next catches
    /// up, since what hid the source may hide news anywhere.
    pub(super) fn after_pass(&mut self, failed: bool, said: SourceSaid) {
        self.owed = failed || said == SourceSaid::Nothing;
    }
}

/// What the subscriber's loop hands its pass of the gate ([`subscriber_pass_saying`]).
#[cfg(unix)]
pub(super) struct Gating<'a> {
    /// The loop's sweeps, which the pass picks its turn from and tells what it asked.
    pub(super) sweeps: &'a mut Sweeps,
    /// Whether the pass catches up ([`Gate::CatchUp`]): the loop's routes moved since its last
    /// pass, or that pass failed or its source said nothing.
    pub(super) moved: bool,
}

/// One gated pass of the source at `primary` ([`sync_found`]), its turn picked by `gating`: a
/// sweep asks the peers [`PeerDiscovery::sweep`] chose, the same ones for records and for the
/// envelope.
#[cfg(unix)]
pub(super) fn gated_pass(
    paths: &Paths,
    config: &Config,
    primary: &str,
    wait: Option<Duration>,
    found: Found<'_>,
    said: &std::cell::Cell<SourceSaid>,
    gating: Gating<'_>,
) -> crate::Result<SyncOutcome> {
    let now = Instant::now();
    match gating.sweeps.turn(now, gating.moved) {
        Turn::CatchUp => {
            pass_counts::add(Count::CatchUps);
            let synced = sync_found(paths, wait, found, said, Some(&Gate::CatchUp));
            gating.sweeps.caught_up(now, crate::server::draw());
            synced
        }
        Turn::SourceFirst => {
            pass_counts::add(Count::GatedPasses);
            sync_found(paths, wait, found, said, Some(&Gate::SourceFirst))
        }
        Turn::Sweep => {
            pass_counts::add(Count::Sweeps);
            let (asked, urls) = found.sweep(paths, config, primary, gating.sweeps, now);
            let selected = PeerDiscovery::selected(&urls);
            let gate = Gate::Sweep(Heard::default());
            let synced = sync_found(paths, wait, &selected, said, Some(&gate));
            // A name that resolved to no address answered nothing.
            let answered = gate.heard().map_or(0, Heard::answered).min(asked);
            pass_counts::add_n(Count::SweepUnanswered, asked - answered);
            gating
                .sweeps
                .swept(now, asked, answered, crate::server::draw());
            synced
        }
    }
}

// The gated passes run against vault servers on loopback, and the sweeps are the Unix loop's.
#[cfg(all(test, unix))]
#[path = "subscriber_gate_tests.rs"]
mod tests;
