// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Program cats with tenure: each major app gets its own cursor kitty, but a program must earn the cursor before the cat changes.

use aterm_time::Instant;
use std::time::Duration;

use aterm_core::terminal::{BlockState, OutputBlock};

use crate::companion::CompanionRung;
use crate::kitty_pet::PetArrival;
use crate::kitty_registry::{KittyLook, SHELL_APP_ID, app_basename_slice, is_shell_basename};

/// How long a program must hold the focused pane, unbroken, before its cat takes the cursor, so quick commands finish first.
pub const TENURE: Duration = Duration::from_secs(5);

/// How long the pane must be back at the prompt before the worn program cat is released to the base cat.
pub const RELEASE: Duration = Duration::from_secs(15);

/// A program cat this window wore within this span comes home quietly; one absent longer is owed a fresh arrival.
pub const HOMECOMING: Duration = Duration::from_secs(600);

/// The minimum spacing between two performed ceremonies, equal to [`RELEASE`] (3× [`TENURE`]), so a 5 s alternation can never chain hellos.
pub const HELLO_FLOOR: Duration = Duration::from_secs(15);

/// The roster's depth: one person's open tools in one window (typically 2–5) with headroom.
pub const HOMECOMING_SLOTS: usize = 8;

/// One program whose cat this window has worn.
struct Homecoming {
    /// The canonical app id ([`AppIdentity::id`]).
    id: String,
    /// Last worn — the recency clock. Refreshed by every landing of this id.
    at: Instant,
    /// Whether the owed hello was performed. An ungreeted slot is an unpaid debt
    /// and is never evicted.
    greeted: bool,
}

/// The resolved program identity of one pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppIdentity {
    /// Canonical app id (`"claude"`, `"vim"`, a basename).
    pub id: String,
    /// The first-token basename the id was derived from.
    pub basename: String,
    /// The resolved breed for `id`.
    pub look: KittyLook,
}

/// Per-window cache of the focused pane's raw program claim, keyed by `(session, block id, block state, commandline present)` so a commandline is parsed only on a transition.
pub struct ProgramClaim {
    key: Option<(u64, u64, BlockState, bool)>,
    identity: Option<AppIdentity>,
}

impl Default for ProgramClaim {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl ProgramClaim {
    /// Construct an empty program-claim cache.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            key: None,
            identity: None,
        }
    }

    /// `session`'s raw program claim from its current shell block, re-derived only when the key moves.
    #[inline]
    pub fn resolve(&mut self, session: u64, block: Option<&OutputBlock>) -> Option<&AppIdentity> {
        let key = block.map(|b| (session, b.id, b.state, b.commandline.is_some()));
        if key != self.key {
            self.key = key;
            self.identity = match block {
                Some(b) => derive_identity(b),
                None => None,
            };
        }
        self.identity.as_ref()
    }
}

/// The raw program claim of one shell block; `None` falls to the base cat.
#[inline]
fn derive_identity(block: &OutputBlock) -> Option<AppIdentity> {
    match block.state {
        BlockState::Executing => {
            let cmd = block.commandline.as_deref()?;
            let raw_base = app_basename_slice(cmd)?;
            if is_shell_basename(raw_base) || raw_base.eq_ignore_ascii_case(SHELL_APP_ID) {
                return None;
            }
            let basename = raw_base.to_ascii_lowercase();
            let look = KittyLook::for_app(&basename);
            let id = basename.clone();
            Some(AppIdentity { id, basename, look })
        }
        _ => None,
    }
}

/// The tenure gate: turns the focused pane's raw, flapping program claim into the slow one the cat wears.
pub struct KittyTenure {
    worn: Option<AppIdentity>,
    candidate: Option<(Option<AppIdentity>, Instant)>,
    /// The cats this window has worn, LRU by `at`. Expiry is lazy (at landings),
    /// so the roster arms no timer.
    roster: [Option<Homecoming>; HOMECOMING_SLOTS],
    /// Stamped only by [`Self::commit_hello`]. The roster's `at` refreshes on
    /// every wear, so alternation starves the ceremony on purpose.
    last_hello: Option<Instant>,
    /// How loudly the last landing arrives: the full ceremony only for a stranger.
    arrival: PetArrival,
}

impl Default for KittyTenure {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl KittyTenure {
    /// Construct a fresh tenure gate on the base cat with an empty homecoming roster.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            worn: None,
            candidate: None,
            roster: [const { None }; HOMECOMING_SLOTS],
            last_hello: None,
            arrival: PetArrival::Quiet,
        }
    }

    /// Feed this frame's raw claim; returns the claim on glass.
    #[inline]
    pub fn observe(&mut self, raw: Option<&AppIdentity>, now: Instant) -> Option<&AppIdentity> {
        if raw == self.worn.as_ref() {
            self.candidate = None;
            return self.worn.as_ref();
        }
        match &mut self.candidate {
            Some((cand, since)) if cand.as_ref() == raw => {
                if now.saturating_duration_since(*since) >= dwell_for(raw)
                    && let Some((cand, _)) = self.candidate.take()
                {
                    self.land(cand, now);
                }
            }
            Some((Some(cand), since)) => match raw {
                Some(r) => {
                    cand.id.clear();
                    cand.id.push_str(&r.id);
                    cand.basename.clear();
                    cand.basename.push_str(&r.basename);
                    cand.look = r.look;
                    *since = now;
                }
                None => self.candidate = Some((None, now)),
            },
            _ => self.candidate = Some((raw.cloned(), now)),
        }
        self.worn.as_ref()
    }

    /// The claim on glass (`None` = the base cat), without advancing the gate.
    #[inline]
    #[must_use]
    pub const fn worn(&self) -> Option<&AppIdentity> {
        self.worn.as_ref()
    }

    /// When the pending candidate earns its tenure, for the host's wake deadline; `None` while nothing is pending, so the gate never pins the event loop.
    #[inline]
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.candidate
            .as_ref()
            .map(|(cand, since)| *since + dwell_for(cand.as_ref()))
    }

    /// Called by the host's wake handler when [`Self::deadline`] is due, present or not: lands the pending candidate and disarms, returning whether the look moved.
    #[inline]
    pub fn poll(&mut self, now: Instant) -> bool {
        let is_due = matches!(
            &self.candidate,
            Some((cand, since)) if now.saturating_duration_since(*since) >= dwell_for(cand.as_ref())
        );
        if !is_due {
            return false;
        }
        let Some((cand, _)) = self.candidate.take() else {
            return false;
        };
        let moved = cand != self.worn;
        self.land(cand, now);
        moved
    }

    /// The one place a due candidate becomes `worn`; also rules the [`PetArrival`]
    /// latch from the roster:
    ///   * base cat (`None`): always quiet (the pet is born wearing it);
    ///   * roster hit: refresh `at`; quiet if greeted, else [`Self::floor`];
    ///   * stranger: insert (LRU among greeted slots) and ask [`Self::floor`].
    #[inline]
    fn land(&mut self, cand: Option<AppIdentity>, now: Instant) {
        self.worn = cand;
        let Some(id) = self.worn.as_ref() else {
            self.arrival = PetArrival::Quiet;
            return;
        };
        let mut hit = None;
        let mut i = 0;
        while i < HOMECOMING_SLOTS {
            if let Some(h) = self.roster[i].as_ref() {
                if h.id == id.id {
                    if hit.is_none() {
                        hit = Some(i);
                    }
                } else if h.greeted && now.saturating_duration_since(h.at) >= HOMECOMING {
                    self.roster[i] = None;
                }
            }
            i += 1;
        }
        match hit {
            Some(idx) => {
                let greeted = if let Some(h) = &mut self.roster[idx] {
                    h.at = now;
                    h.greeted
                } else {
                    false
                };
                self.arrival = if greeted {
                    PetArrival::Quiet
                } else {
                    self.floor(now)
                };
            }
            None => {
                self.arrival = if insert_lru(&mut self.roster, &id.id, now) {
                    self.floor(now)
                } else {
                    // Every slot holds a debt: refuse the insert rather than
                    // drop one. The only way a stranger's hello is lost.
                    PetArrival::Quiet
                };
            }
        }
    }

    /// `Ceremony` if no hello was performed yet or the last was at least
    /// [`HELLO_FLOOR`] ago; else `Quiet`, with the debt kept.
    #[inline]
    fn floor(&self, now: Instant) -> PetArrival {
        match self.last_hello {
            Some(t) if now.saturating_duration_since(t) < HELLO_FLOOR => PetArrival::Quiet,
            _ => PetArrival::Ceremony,
        }
    }

    /// How loudly the most recent landing arrived; read, never advanced.
    #[inline]
    #[must_use]
    pub const fn arrival(&self) -> PetArrival {
        self.arrival
    }

    /// Called by the render seam only when a ceremony was performed on glass (`companion_verdict` also runs for unfocused windows).
    #[inline]
    pub fn commit_hello(&mut self, now: Instant) {
        if let Some(id) = self.worn.as_ref() {
            let mut i = 0;
            while i < HOMECOMING_SLOTS {
                if let Some(h) = &mut self.roster[i]
                    && h.id == id.id
                {
                    h.greeted = true;
                    break;
                }
                i += 1;
            }
        }
        self.last_hello = Some(now);
        self.arrival = PetArrival::Quiet;
    }

    /// Reset the arrival latch when a non-program rung wins.
    #[inline]
    pub const fn note_non_program_verdict(&mut self) {
        self.arrival = PetArrival::Quiet;
    }
}

/// Insert a stranger: an empty slot first, else evict the LRU among greeted slots
/// only.
#[inline]
fn insert_lru(roster: &mut [Option<Homecoming>; HOMECOMING_SLOTS], id: &str, now: Instant) -> bool {
    let mut slot = None;
    let mut oldest_at = None;
    let mut i = 0;
    while i < HOMECOMING_SLOTS {
        let Some(h) = &roster[i] else {
            slot = Some(i);
            break;
        };
        let is_older = match oldest_at {
            Some(oldest) => h.at < oldest,
            None => true,
        };
        if h.greeted && is_older {
            oldest_at = Some(h.at);
            slot = Some(i);
        }
        i += 1;
    }
    match slot {
        Some(i) => {
            if let Some(h) = &mut roster[i] {
                h.id.clear();
                h.id.push_str(id);
                h.at = now;
                h.greeted = false;
            } else {
                roster[i] = Some(Homecoming {
                    id: id.to_owned(),
                    at: now,
                    greeted: false,
                });
            }
            true
        }
        None => false,
    }
}

/// The dwell a candidate claim must hold before it lands: a program arrives
/// after [`TENURE`]; falling back to the base cat waits [`RELEASE`].
#[inline]
const fn dwell_for(candidate: Option<&AppIdentity>) -> Duration {
    if candidate.is_some() { TENURE } else { RELEASE }
}

/// The companion precedence law, stated only here (every dressing surface goes
/// through [`LookPolicy::verdict`]):
///
///   favourite > program (with tenure) > launch kitty
#[inline]
#[must_use]
pub const fn companion_precedence(
    favourite: Option<KittyLook>,
    app: Option<KittyLook>,
    launch: KittyLook,
) -> (KittyLook, CompanionRung) {
    if let Some(look) = favourite {
        (look, CompanionRung::Favourite)
    } else if let Some(look) = app {
        (look, CompanionRung::Program)
    } else {
        (launch, CompanionRung::Launch)
    }
}

/// One window's look policy: the focused pane's claim cache, the tenure gate and the rung that won the last verdict.
pub struct LookPolicy {
    claim: ProgramClaim,
    tenure: KittyTenure,
    rung: CompanionRung,
}

impl Default for LookPolicy {
    /// A fresh window wears the launch kitty floor until a verdict resolves.
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl LookPolicy {
    /// A fresh window wears the launch kitty floor until a verdict resolves.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            claim: ProgramClaim::new(),
            tenure: KittyTenure::new(),
            rung: CompanionRung::Launch,
        }
    }

    /// Refresh the focused pane's cached raw program claim from `block` (the pane's current block, else its last) without cloning it.
    #[inline]
    pub fn resolve_claim(
        &mut self,
        session: u64,
        block: Option<&OutputBlock>,
    ) -> Option<&AppIdentity> {
        self.claim.resolve(session, block)
    }

    /// The focused pane's raw program claim, read off `block` (the pane's current block, else its last) under the host's terminal lock.
    #[inline]
    pub fn claim(&mut self, session: u64, block: Option<&OutputBlock>) -> Option<AppIdentity> {
        self.resolve_claim(session, block).cloned()
    }

    /// [`Self::verdict`] reading the focused pane's already-resolved [`Self::resolve_claim`] in place, avoiding a per-frame [`AppIdentity`] clone across the terminal lock.
    #[inline]
    pub fn verdict_from_resolved_claim(
        &mut self,
        favourite: Option<KittyLook>,
        launch: KittyLook,
        now: Instant,
    ) -> KittyLook {
        let app = self
            .tenure
            .observe(self.claim.identity.as_ref(), now)
            .map(|i| i.look);
        self.apply_verdict(favourite, app, launch)
    }

    /// The companion verdict: `raw` (the focused pane's claim) through the tenure gate, then [`companion_precedence`].
    #[inline]
    pub fn verdict(
        &mut self,
        raw: Option<&AppIdentity>,
        favourite: Option<KittyLook>,
        launch: KittyLook,
        now: Instant,
    ) -> KittyLook {
        let app = self.tenure.observe(raw, now).map(|i| i.look);
        self.apply_verdict(favourite, app, launch)
    }

    #[inline]
    const fn apply_verdict(
        &mut self,
        favourite: Option<KittyLook>,
        app: Option<KittyLook>,
        launch: KittyLook,
    ) -> KittyLook {
        let (look, rung) = companion_precedence(favourite, app, launch);
        self.rung = rung;
        if !matches!(rung, CompanionRung::Program) {
            self.tenure.note_non_program_verdict();
        }
        look
    }

    /// Which rung won the most recent verdict (`Launch` before any).
    #[inline]
    #[must_use]
    pub const fn rung(&self) -> CompanionRung {
        self.rung
    }

    /// The tenure gate, for its readers.
    #[inline]
    #[must_use]
    pub const fn tenure(&self) -> &KittyTenure {
        &self.tenure
    }

    /// The tenure gate, for [`KittyTenure::poll`] and [`KittyTenure::commit_hello`].
    #[inline]
    pub const fn tenure_mut(&mut self) -> &mut KittyTenure {
        &mut self.tenure
    }
}

#[cfg(test)]
#[path = "companion_look_tests.rs"]
mod tests;
