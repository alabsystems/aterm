// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PROGRAM CATS WITH TENURE (owner spec 2026-08-07, "each major app gets its
//! own cursor kitty — the claude cat, the codex cat, and a cat for every
//! command"; owner rulings 2026-08-17: *"I like the different cats. I think
//! switching the cats all the time is too abrupt"* / *"what is a good
//! compromise? design something"*).
//!
//! THE COMPROMISE, in four parts:
//!
//! 1. **The base cat is the launch kitty** (`App::launch_kitty`, one per
//!    process, minted at launch by `aterm-gui`'s `launch_kitty.rs`). It is what the
//!    prompt wears. There is no "shell" program cat and no per-session cat
//!    (owner, 2026-08-20).
//! 2. **A program has to EARN the cursor: tenure.** The focused pane's
//!    `Executing` shell block names a program (`app_basename` →
//!    `canonical_app_id` → `KittyLook::for_app`), but that claim becomes the
//!    cat on glass only after holding unbroken for [`TENURE`], so quick
//!    commands never flip the cat while `claude`, `vim` or a long build do.
//!    After the program exits the cat lingers until the pane has been back at
//!    the prompt for [`RELEASE`]; moving straight to another program takes
//!    that program's [`TENURE`]. Focus changes and block changes pass the same
//!    gate: every candidate claim must be stable for its dwell before it lands.
//! 3. **The switch is soft.** The flying kitty swaps only between appearances
//!    (`kitty_cursor::set_look`'s per-appearance latch); the walking pet lets
//!    the old cat run off while the new one fades up in place
//!    (`kitty_pet::ARRIVE_IN`).
//! 4. **The rate rule.** The dwell decides when the costume moves; the
//!    per-window roster on [`KittyTenure`] decides how loudly. A program cat
//!    this window has worn within [`HOMECOMING`] (refreshed on every wear)
//!    comes home quietly; only a cat not worn recently gets the full arrival
//!    ceremony, and a stranger is only spaced by [`HELLO_FLOOR`], never
//!    denied. The roster authorises; the render seam performs and commits
//!    ([`KittyTenure::commit_hello`]), so a hello nobody saw is never spent.
//!
//! The pet is company, not a status indicator: the costume is always correct
//! (the roster rations only the ceremony), and the walking pet's visible
//! identity (a coat index and an iris; `PetCursorFrame` carries no `variant`)
//! is not a legible program identity.
//!
//! Identity source: the focused pane's shell block. While a command is
//! `Executing`, its OSC 633;E commandline names the app; every other state
//! (the prompt, a finished command, no block at all, a nested shell) claims
//! nothing and the pane falls to the base cat. Cached per window by
//! `(session, block id, state, commandline present)` in [`ProgramClaim`] so
//! a commandline is parsed only on shell-block TRANSITIONS, never per frame.
//!
//! THE TENURE GATE lives per WINDOW in [`KittyTenure`] (a window's cat follows
//! its own focused pane; two windows may honestly wear two programs' cats),
//! and THE PRECEDENCE LAW — favourite > program (with tenure) > launch kitty
//! — is [`companion_precedence`]. [`LookPolicy`] holds the three for one
//! window, beside — never inside — the pet's brain, on the injected clock
//! (host-boundary Phase 5; kitty-motion §9's rate law restated for this
//! layering, host-boundary §7 decision 1). The host keeps the durable ledger
//! (the pinned favourite), the launch seed's entropy and the terminal lock:
//! it hands the verdict the focused pane's block, the favourite and the
//! launch look.

use aterm_time::Instant;
use std::time::Duration;

use aterm_core::terminal::{BlockState, OutputBlock};

use crate::companion::CompanionRung;
use crate::kitty_pet::PetArrival;
use crate::kitty_registry::{KittyLook, SHELL_APP_ID, app_basename, canonical_app_id};

/// How long a program must hold the focused pane, unbroken, before its cat
/// takes the cursor. Long enough that every quick command (a listing, a
/// status, a short build) finishes first; short enough that a tool you
/// settle into is dressed within its first breath.
pub const TENURE: Duration = Duration::from_secs(5);

/// How long the pane must be back at the prompt (no program claim at all)
/// before the worn program cat is released to the base cat. Deliberately
/// longer than [`TENURE`]: leaving a program is the moment a bounce would
/// hurt most (exit-and-relaunch, a shell errand, a glance at another tab),
/// so the cat you had lingers until you have genuinely settled at the
/// prompt. (A move to ANOTHER program is that program's arrival and takes
/// [`TENURE`]; the old cat lingers until then.)
pub const RELEASE: Duration = Duration::from_secs(15);

/// THE HOMECOMING WINDOW: how long a worn cat stays "yours". A program cat
/// this window has worn within this window of time comes home QUIETLY; one
/// absent longer is owed a fresh arrival. Inherited, not invented, from the
/// tree's only shipped recency TTL — `kitty_log`'s `RING_TTL = 600 s`, one
/// file away, with the identical refresh-on-hit shape (a second recency
/// window with a different number would be two laws where the tree has one).
/// Because the stamp refreshes on EVERY wear, the operative meaning is "a
/// tool you touch at least once every ten minutes stays yours": a 20 s
/// alternation is 30× inside it; a lunch break is not. First number to
/// raise if the ceremony rate still reads high.
pub const HOMECOMING: Duration = Duration::from_secs(600);

/// THE HELLO FLOOR: the minimum spacing between two PERFORMED ceremonies.
/// Not taste: it is [`RELEASE`], quoted — the gate's own longest dwell is
/// the house's existing statement of "you have genuinely settled", so two
/// hellos are never closer than the gate's own settling time. It is 3×
/// [`TENURE`], so the pathological 5 s alternation can never chain hellos.
/// It SPACES strangers; it never denies one: a floored stranger keeps its
/// debt (`greeted` stays false) and collects on a later landing.
pub const HELLO_FLOOR: Duration = Duration::from_secs(15);

/// The roster's depth: a working set, not a history. Above the realistic
/// simultaneous-tool count (the owner's case is 2; a busy split is 3–5)
/// with headroom, and small enough that the landing-time scan is eight
/// string compares a few times an hour. Deliberately NOT `kitty_log`'s
/// `RING_SLOTS = 256`: that ring is sized for a screenful of cats across
/// shared sessions; this is one human's open tools in one window.
pub const HOMECOMING_SLOTS: usize = 8;

/// One remembered cat: a program whose costume this window has put on the
/// pet. `at` is the last time it was worn (refreshed on every wear, quiet ones
/// included, so tools in rotation never go stale); `greeted` records whether
/// its one full arrival was performed, not merely authorised.
struct Homecoming {
    /// The canonical app id ([`AppIdentity::id`]), compared as a plain
    /// `String` across at most [`HOMECOMING_SLOTS`] slots.
    id: String,
    /// Last worn — the recency clock. Refreshed by every landing of this id.
    at: Instant,
    /// Whether the owed hello was performed (committed by the render seam).
    /// An ungreeted slot is an unpaid debt: it pins its slot and is never
    /// evicted.
    greeted: bool,
}

/// The resolved program identity of one pane: canonical id, the raw basename
/// it came from (for diagnostics), and the breed. The look is a pure function
/// of `id`, carried here so the render rung never re-hashes per frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppIdentity {
    /// Canonical app id (`"claude"`, `"vim"`, a basename).
    pub id: String,
    /// The first-token basename the id was derived from.
    pub basename: String,
    /// The resolved breed for `id`.
    pub look: KittyLook,
}

/// Per-window cache for the focused pane's raw program claim, keyed by
/// `(session, block id, block state, commandline present)` so a commandline
/// is parsed only on shell-block TRANSITIONS (or a focus move to another
/// session) — never per frame. The fourth key component covers a late
/// OSC 633;E that lands after 133;C already flipped the block to
/// `Executing`.
#[derive(Default)]
pub struct ProgramClaim {
    key: Option<(u64, u64, BlockState, bool)>,
    identity: Option<AppIdentity>,
}

impl ProgramClaim {
    /// Resolve `session`'s RAW program claim from its current shell block,
    /// re-deriving only when the key moves. This is the ungated claim;
    /// [`KittyTenure`] decides whether it is worn.
    pub fn resolve(&mut self, session: u64, block: Option<&OutputBlock>) -> Option<&AppIdentity> {
        let key = block.map(|b| (session, b.id, b.state, b.commandline.is_some()));
        if key != self.key {
            self.key = key;
            self.identity = block.and_then(derive_identity);
        }
        self.identity.as_ref()
    }
}

/// Derive the raw program claim for one shell block. `None` means "no claim"
/// — the pane falls to the base cat.
fn derive_identity(block: &OutputBlock) -> Option<AppIdentity> {
    match block.state {
        // While a command runs the pane belongs to that program. No parseable
        // commandline (no OSC 633;E, or a bare Enter) means no claim; a nested
        // shell is not a program cat either: the prompt, at any depth, wears
        // the launch kitty.
        BlockState::Executing => {
            let basename = app_basename(block.commandline.as_deref().unwrap_or(""))?;
            let id = canonical_app_id(&basename);
            if id == SHELL_APP_ID {
                return None;
            }
            let id = id.to_owned();
            let look = KittyLook::for_app(&id);
            Some(AppIdentity { id, basename, look })
        }
        // At the prompt — before, while, and after typing a command — and in
        // any unknown future state: no program claim, the base cat.
        _ => None,
    }
}

/// THE TENURE GATE (one lives in each window's [`LookPolicy`], because a
/// window's cat follows its own focused pane): turns the focused pane's raw, instantly
/// flapping program claim into the slow, deliberate one the cat wears.
///
/// Advanced by [`Self::observe`] on every companion verdict (every present)
/// and by [`Self::poll`] at the wake deadline:
///   * `worn` is the claim on glass (`None` = the base cat);
///   * `candidate` is the most recent raw claim that differs from `worn`, with
///     the instant it was first seen. A raw claim equal to `worn` clears the
///     candidate; a different new claim restarts its clock;
///   * a candidate that has held for its dwell ([`TENURE`] to arrive at any
///     program, [`RELEASE`] to fall back to the base cat) becomes `worn`.
///
/// Both directions are debounced, so switches are at least `TENURE` apart,
/// quick flickers never land, and the cat lingers after a program exits.
#[derive(Default)]
pub struct KittyTenure {
    worn: Option<AppIdentity>,
    candidate: Option<(Option<AppIdentity>, Instant)>,
    /// The cats this window has worn, LRU by `at`. Written only by
    /// [`Self::land`] and [`Self::commit_hello`]; arms no timer (expiry is
    /// lazy, at landings), so `deadline()` and idle-to-zero are unaffected.
    roster: [Option<Homecoming>; HOMECOMING_SLOTS],
    /// Stamped only by a performed ceremony ([`Self::commit_hello`]), never by
    /// an authorisation. The roster's `at`, by contrast, refreshes on every
    /// wear: starving the ceremony under alternation is intended. (`kitty_summon`
    /// rate-limits a record, where starvation loses data; this rate-limits a
    /// performance.) The two clocks differ on purpose.
    last_hello: Option<Instant>,
    /// HOW LOUDLY the last landing arrives, read by the render seam through
    /// [`Self::arrival`]: the full ceremony only for a stranger (a cat this
    /// window has not worn within [`HOMECOMING`]), else the quiet re-dress.
    /// The dwell decides WHEN the costume moves; this decides whether the
    /// move is announced. Sticky until the next landing, a performed
    /// ceremony ([`Self::commit_hello`]) or a non-program verdict
    /// ([`Self::note_non_program_verdict`]).
    arrival: PetArrival,
}

impl KittyTenure {
    /// Feed this frame's raw claim; returns the claim ON GLASS after the
    /// gate. Idempotent for a stable input at a stable time, monotone in
    /// `now`, so re-observing (a capture splice, a typed summon) is harmless.
    pub fn observe(&mut self, raw: Option<&AppIdentity>, now: Instant) -> Option<&AppIdentity> {
        if raw == self.worn.as_ref() {
            self.candidate = None;
            return self.worn.as_ref();
        }
        match &self.candidate {
            Some((cand, since)) if cand.as_ref() == raw => {
                if now.duration_since(*since) >= dwell_for(raw) {
                    let cand = raw.cloned();
                    self.candidate = None;
                    self.land(cand, now);
                }
            }
            _ => self.candidate = Some((raw.cloned(), now)),
        }
        self.worn.as_ref()
    }

    /// The claim currently on glass (`None` = the base cat), without
    /// advancing the gate.
    pub fn worn(&self) -> Option<&AppIdentity> {
        self.worn.as_ref()
    }

    /// The instant the pending candidate (if any) earns its tenure, folded
    /// into the host's wake deadline so a silent program still gets dressed
    /// and a lingering cat still releases on time. `None` while nothing is
    /// pending, so the gate never pins the event loop.
    pub fn deadline(&self) -> Option<Instant> {
        self.candidate
            .as_ref()
            .map(|(cand, since)| *since + dwell_for(cand.as_ref()))
    }

    /// THE WAKE-TIME LANDING: called by the host's wake handler when
    /// [`Self::deadline`] is due, INDEPENDENT of whether a present will run.
    /// Lands the pending candidate as `worn` (it has held for its dwell as
    /// far as this gate has seen) and disarms, returning whether the look
    /// on glass moved. This is what keeps an armed deadline from ever
    /// spinning the event loop: a window that has left the terminal route
    /// (a Settings tab, a native tab, a failed-present backoff) never
    /// re-observes, so it must be able to consume its own deadline. If the
    /// raw claim moved on meanwhile, the next [`Self::observe`] simply starts
    /// the fresh candidate's clock — the same correction it applies to any
    /// stale claim.
    pub fn poll(&mut self, now: Instant) -> bool {
        match self.candidate.take() {
            Some((cand, since)) if now.duration_since(since) >= dwell_for(cand.as_ref()) => {
                let moved = cand != self.worn;
                // land() runs on every due poll, `moved` or not: a re-land of
                // the worn cat must refresh its roster stamp and re-rule
                // `arrival` exactly as the observe path would.
                self.land(cand, now);
                moved
            }
            other => {
                self.candidate = other;
                false
            }
        }
    }

    /// THE LANDING — the one place a due candidate becomes `worn`, factored
    /// from BOTH landing sites ([`Self::observe`]'s dwell branch and
    /// [`Self::poll`]'s wake-time take) so a window on a non-terminal route
    /// (a Settings tab, a native tab, a failed-present backoff) cannot
    /// silently keep the old law. Besides moving the costume it rules HOW
    /// LOUDLY, writing the [`PetArrival`] latch from the roster:
    ///   * base cat (`None`) — always [`PetArrival::Quiet`]: THE BASE CAT IS
    ///     ALWAYS HOME. The pet is born wearing the launch kitty, so the
    ///     resident cat is owned from frame one and every fall to the
    ///     prompt is quiet, forever;
    ///   * roster hit — a homecoming: refresh `at` (every wear, including
    ///     quiet ones), quiet if greeted, else the still-owed debt goes to
    ///     [`Self::floor`];
    ///   * roster miss — a stranger: insert (LRU among GREETED slots only)
    ///     and ask [`Self::floor`].
    ///
    /// Lazy expiry runs only here, never on a wake. It evicts only greeted
    /// slots older than [`HOMECOMING`] and skips the landing id: a
    /// continuously worn cat never re-lands, so its stamp can be arbitrarily
    /// stale at its own next landing, and expiring it then would award a
    /// spurious ceremony.
    fn land(&mut self, cand: Option<AppIdentity>, now: Instant) {
        self.worn = cand;
        let Some(id) = self.worn.as_ref() else {
            self.arrival = PetArrival::Quiet;
            return;
        };
        for s in &mut self.roster {
            if s.as_ref().is_some_and(|h| {
                h.greeted && h.id != id.id && now.duration_since(h.at) >= HOMECOMING
            }) {
                *s = None;
            }
        }
        match self.roster.iter_mut().flatten().find(|h| h.id == id.id) {
            Some(h) => {
                h.at = now;
                self.arrival = if h.greeted {
                    PetArrival::Quiet
                } else {
                    self.floor(now)
                };
            }
            None => {
                self.arrival = if insert_lru(&mut self.roster, &id.id, now) {
                    self.floor(now)
                } else {
                    // Every slot holds an unpaid debt (eight strangers all
                    // denied by the floor, none yet performed): refuse the
                    // insert rather than drop a debt, and arrive as a
                    // floor-spaced Quiet. This stranger's own hello is the
                    // one the roster can lose, and only in this state.
                    PetArrival::Quiet
                };
            }
        }
    }

    /// THE FLOOR — a stranger is never denied its ceremony, only SPACED:
    /// Ceremony iff no hello was ever performed or the last performed one
    /// is at least [`HELLO_FLOOR`] ago; Quiet otherwise, with the debt kept
    /// (`greeted` stays `false`) to be collected on a later landing.
    fn floor(&self, now: Instant) -> PetArrival {
        if self
            .last_hello
            .is_none_or(|t| now.duration_since(t) >= HELLO_FLOOR)
        {
            PetArrival::Ceremony
        } else {
            PetArrival::Quiet
        }
    }

    /// HOW LOUDLY the current costume arrived — the rate law's ruling for
    /// the most recent landing, read (never advanced) by the render seam at
    /// the `sync_look` sites, where the pet's actual pair is in hand.
    pub fn arrival(&self) -> PetArrival {
        self.arrival
    }

    /// THE PERFORMANCE COMMIT: called by the render seam ONLY when a
    /// ceremony was actually PERFORMED on glass — never at authorisation
    /// (`companion_verdict` runs for unfocused windows, and a hello nobody
    /// saw must not be spent; the `kitty_summon` precedent, applied
    /// exactly). Marks the worn id `greeted` (the debt is paid), stamps
    /// `last_hello` (the ONLY writer of that clock), and consumes the
    /// latch back to [`PetArrival::Quiet`] so a pair the flying-kitty latch
    /// re-delivers later cannot replay a ceremony already performed.
    pub fn commit_hello(&mut self, now: Instant) {
        if let Some(id) = self.worn.as_ref()
            && let Some(h) = self.roster.iter_mut().flatten().find(|h| h.id == id.id)
        {
            h.greeted = true;
        }
        self.last_hello = Some(now);
        self.arrival = PetArrival::Quiet;
    }

    /// THE STALE-LATCH RESET: called by the render seam when a NON-program
    /// rung wins the precedence (a pinned favourite, the launch kitty). A
    /// park can be created by a non-landing event — unpinning a favourite
    /// reveals an already-worn program cat — and without this reset the
    /// seam would perform a ceremony authorised by a landing minutes old.
    /// Only the announcement is reset; the roster, its debts and the
    /// `last_hello` clock are untouched (a debt is owed, not spent).
    pub fn note_non_program_verdict(&mut self) {
        self.arrival = PetArrival::Quiet;
    }
}

/// Insert a stranger into the roster: an empty slot first, else evict the LRU
/// (oldest `at`) among greeted slots only, so an unpaid debt pins its slot.
/// Returns `false` (nothing inserted) when every slot holds an ungreeted debt.
fn insert_lru(roster: &mut [Option<Homecoming>; HOMECOMING_SLOTS], id: &str, now: Instant) -> bool {
    let slot = roster.iter().position(|s| s.is_none()).or_else(|| {
        roster
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_ref().is_some_and(|h| h.greeted))
            .min_by_key(|(_, s)| s.as_ref().map(|h| h.at))
            .map(|(i, _)| i)
    });
    match slot {
        Some(i) => {
            roster[i] = Some(Homecoming {
                id: id.to_owned(),
                at: now,
                greeted: false,
            });
            true
        }
        None => false,
    }
}

/// The dwell a candidate claim must hold before it lands: a program arrives
/// after [`TENURE`]; falling back to the base cat waits [`RELEASE`].
fn dwell_for(candidate: Option<&AppIdentity>) -> Duration {
    if candidate.is_some() { TENURE } else { RELEASE }
}

/// THE COMPANION PRECEDENCE LAW (owner rulings, 2026-08-07 and 2026-08-17),
/// the ONE place the order is stated — every dressing surface (the
/// single-pane present, the split/composed present, and both capture
/// splices) resolves through [`LookPolicy::verdict`], which calls here:
///
///   favourite > program (with tenure) > launch kitty
///
///   1. A PINNED FAVOURITE owns the companion look (standing owner law): the
///      user chose that cat, and only a reason outranks a choice. Pinning is
///      also how a launch kitty you like is kept across launches.
///   2. THE PROGRAM CAT — but only once it has EARNED the cursor: `app` is
///      the focused pane's claim AFTER [`KittyTenure`] (a program that has
///      held the pane for [`TENURE`], lingering [`RELEASE`] after it exits),
///      never the raw, flapping block state.
///   3. THE LAUNCH KITTY: the process's own base cat, minted once at launch —
///      the face of "no stronger claim". There is no session rung and no
///      discovery rung: neither is a choice, and each made the cat change.
///
/// Returns the winning look AND the [`CompanionRung`] that won, so the rate
/// law's sync sites can ration the theater without a second, drifting
/// re-derivation of this order.
#[must_use]
pub fn companion_precedence(
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

/// ONE WINDOW'S LOOK POLICY: the focused pane's claim cache, the tenure gate
/// and the rung that won the last verdict — held by the window's
/// [`crate::pipeline::EffectsPipeline`] beside the pet's owner, which the
/// verdict dresses.
pub struct LookPolicy {
    claim: ProgramClaim,
    tenure: KittyTenure,
    rung: CompanionRung,
}

impl Default for LookPolicy {
    /// A fresh window wears the launch kitty floor until a verdict resolves.
    fn default() -> Self {
        Self {
            claim: ProgramClaim::default(),
            tenure: KittyTenure::default(),
            rung: CompanionRung::Launch,
        }
    }
}

impl LookPolicy {
    /// The focused pane's RAW program claim, read off `block` (the pane's
    /// current block, else its last) under the host's terminal lock.
    pub fn claim(&mut self, session: u64, block: Option<&OutputBlock>) -> Option<AppIdentity> {
        self.claim.resolve(session, block).cloned()
    }

    /// THE ONE COMPANION VERDICT: `raw` (the focused pane's claim) through
    /// the tenure gate, then [`companion_precedence`] over the pinned
    /// `favourite`, the tenured program and the `launch` kitty. Records the
    /// rung that won; a NON-program win also resets the gate's sticky
    /// arrival latch ([`KittyTenure::note_non_program_verdict`]) — a park can
    /// be created by a non-landing event (unpinning a favourite reveals an
    /// already-worn program cat), and without the reset the render seam
    /// would perform a ceremony authorised by a landing minutes old.
    /// Idempotent for a stable claim at a stable time, so a capture splice
    /// or a typed summon re-resolving in the same frame lands nothing the
    /// present would not.
    pub fn verdict(
        &mut self,
        raw: Option<&AppIdentity>,
        favourite: Option<KittyLook>,
        launch: KittyLook,
        now: Instant,
    ) -> KittyLook {
        let app = self.tenure.observe(raw, now).map(|i| i.look);
        let (look, rung) = companion_precedence(favourite, app, launch);
        self.rung = rung;
        if rung != CompanionRung::Program {
            self.tenure.note_non_program_verdict();
        }
        look
    }

    /// Which rung won the most recent verdict (`Launch` before any).
    #[must_use]
    pub fn rung(&self) -> CompanionRung {
        self.rung
    }

    /// The tenure gate, for its readers ([`KittyTenure::arrival`],
    /// [`KittyTenure::worn`], [`KittyTenure::deadline`]).
    #[must_use]
    pub fn tenure(&self) -> &KittyTenure {
        &self.tenure
    }

    /// The tenure gate, for the host's wake-time landing
    /// ([`KittyTenure::poll`]) and the performance commit
    /// ([`KittyTenure::commit_hello`]).
    pub fn tenure_mut(&mut self) -> &mut KittyTenure {
        &mut self.tenure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(state: BlockState, commandline: Option<&str>) -> OutputBlock {
        let mut b = OutputBlock::new(7, 0, 0);
        b.state = state;
        b.commandline = commandline.map(Box::from);
        b
    }

    fn ident(id: &str) -> AppIdentity {
        AppIdentity {
            id: id.to_owned(),
            basename: id.to_owned(),
            look: KittyLook::for_app(id),
        }
    }

    /// Tier-1 binding for `CompanionTenureFlicker`: the real gate, driven
    /// through a flicker storm beside the model.
    ///
    /// The model (aterm-spec's `companion_tenure_flicker_model`, discharged by
    /// `derived_companion_tenure_flicker_proves_and_catches_a_flapping_cat`)
    /// proves the worn identity never changes across an alternating storm.
    /// Here the same alternation, at a cadence far below `TENURE`, reads
    /// `worn()` after every observation. The mechanism is `observe`'s `_` arm:
    /// any observation that does not match the standing candidate restarts it
    /// at `now`, so an alternating pane never accumulates dwell.
    #[test]
    fn companion_tenure_flicker_conformance_real_gate_holds_the_cat_still() {
        let m = aterm_spec::derive::companion_tenure_flicker_model();
        let mut st = m.init_state();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        let t0 = Instant::now();
        // A cadence well inside TENURE, so the storm is a storm and not six
        // separate tenures served back to back.
        let beat = TENURE / 8;

        for k in 0..3u32 {
            assert!(
                m.fire("SeeProgram", &mut st),
                "the model admits the block opening"
            );
            let worn = gate.observe(Some(&claude), t0 + beat * (2 * k)).cloned();
            assert!(
                worn.is_none(),
                "beat {k}: a program that has not dwelled cannot take the cursor"
            );
            assert_eq!(st.get("worn"), Some(&0), "and the model agrees");

            assert!(
                m.fire("SeeNone", &mut st),
                "the model admits the block closing"
            );
            let worn = gate.observe(None, t0 + beat * (2 * k + 1)).cloned();
            assert!(worn.is_none(), "beat {k}: still the base cat");
            assert_eq!(st.get("worn"), Some(&0));
            assert!(m.check_invariant("FlickerNeverLands", &st));
        }
        assert_eq!(
            st.get("changes"),
            Some(&0),
            "six flickers and the model counted no identity change"
        );
        assert!(
            gate.worn().is_none(),
            "and the real gate is still on the base cat"
        );

        // Negative control: the same program, observed steadily across
        // TENURE, does take the cursor, so the invariant above is not met by a
        // gate that never lands.
        let mut steady = KittyTenure::default();
        assert!(steady.observe(Some(&claude), t0).is_none());
        assert_eq!(
            steady
                .observe(Some(&claude), t0 + TENURE)
                .map(|i| i.id.clone()),
            Some("claude".to_owned()),
            "a program that really holds the pane lands at TENURE"
        );
    }

    /// The raw claim: prompt-side states claim nothing (the base cat, not a
    /// "shell" cat), a nested shell claims nothing, and an `Executing` block
    /// with a commandline hands the pane to the named program.
    #[test]
    fn prompt_and_shells_claim_nothing_and_executing_names_the_program() {
        let mut slot = ProgramClaim::default();
        assert!(
            slot.resolve(0, Some(&block(BlockState::PromptOnly, None)))
                .is_none()
        );
        for state in [BlockState::EnteringCommand, BlockState::Complete] {
            assert!(
                slot.resolve(0, Some(&block(state, Some("claude --resume"))))
                    .is_none(),
                "{state:?}: prompt-side, no claim"
            );
        }
        assert!(
            slot.resolve(0, Some(&block(BlockState::Executing, Some("bash"))))
                .is_none(),
            "a nested shell is not a program cat"
        );
        assert!(
            slot.resolve(0, Some(&block(BlockState::Executing, None)))
                .is_none(),
            "no commandline: no claim"
        );
        assert!(slot.resolve(0, None).is_none(), "no block: no claim");
        let ident = slot
            .resolve(
                0,
                Some(&block(
                    BlockState::Executing,
                    Some("/usr/local/bin/claude --resume"),
                )),
            )
            .expect("an executing commandline names the program");
        assert_eq!(ident.id, "claude");
        assert_eq!(ident.basename, "claude");
        assert_eq!(ident.look, KittyLook::for_app("claude"));
    }

    /// The slot re-derives only on key transitions: the same (block, state,
    /// commandline-present) shape keeps the cached identity, and a late
    /// OSC 633;E (commandline arriving mid-Executing) is its own transition.
    #[test]
    fn the_claim_caches_by_block_state_and_commandline_presence() {
        let mut slot = ProgramClaim::default();
        let executing = block(BlockState::Executing, None);
        assert!(slot.resolve(0, Some(&executing)).is_none());
        assert!(
            slot.resolve(0, Some(&executing)).is_none(),
            "stable across frames"
        );
        let late_e = block(BlockState::Executing, Some("codex exec"));
        assert_eq!(
            slot.resolve(0, Some(&late_e))
                .expect("late 633;E re-resolves")
                .id,
            "codex"
        );
        let done = block(BlockState::Complete, Some("codex exec"));
        assert!(slot.resolve(0, Some(&done)).is_none(), "complete → base");
    }

    /// The claim is cached per WINDOW (it was cached per session): a focus
    /// move to another session's pane re-derives even when the two blocks
    /// share an id, a state and a commandline's presence, and a stable key
    /// keeps the cached identity without parsing again.
    #[test]
    fn a_focus_move_to_another_session_re_derives_the_claim() {
        let mut claim = ProgramClaim::default();
        let vim = block(BlockState::Executing, Some("vim notes.md"));
        let claude = block(BlockState::Executing, Some("claude"));
        assert_eq!(
            claim.resolve(1, Some(&vim)).map(|i| i.id.clone()),
            Some("vim".to_owned())
        );
        assert_eq!(
            claim.resolve(2, Some(&claude)).map(|i| i.id.clone()),
            Some("claude".to_owned()),
            "another session's block is another claim"
        );
        assert_eq!(
            claim.resolve(2, Some(&vim)).map(|i| i.id.clone()),
            Some("claude".to_owned()),
            "the same key is served from the cache"
        );
    }

    /// Tenure, arrival: a program claim lands only after holding for
    /// `TENURE`; a claim that comes and goes inside that window never lands.
    #[test]
    fn a_program_earns_the_cursor_only_after_tenure() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        assert!(gate.observe(None, t0).is_none(), "fresh: the base cat");

        // A quick command: `ls` for 300 ms, then home.
        let ls = ident("ls");
        assert!(gate.observe(Some(&ls), t0).is_none());
        assert!(
            gate.observe(Some(&ls), t0 + Duration::from_millis(300))
                .is_none(),
            "300 ms of `ls` earns nothing"
        );
        assert!(
            gate.observe(None, t0 + Duration::from_millis(400))
                .is_none()
        );
        assert!(gate.deadline().is_none(), "home again: nothing pending");

        // claude: seen, then held.
        let t1 = t0 + Duration::from_secs(1);
        assert!(
            gate.observe(Some(&claude), t1).is_none(),
            "seen, not yet earned"
        );
        assert_eq!(
            gate.deadline(),
            Some(t1 + TENURE),
            "the wake is armed at tenure"
        );
        assert!(
            gate.observe(Some(&claude), t1 + TENURE - Duration::from_millis(1))
                .is_none(),
            "one ms short: still the base cat"
        );
        assert_eq!(
            gate.observe(Some(&claude), t1 + TENURE)
                .map(|i| i.id.as_str()),
            Some("claude"),
            "tenure served: the claude cat"
        );
        assert!(gate.deadline().is_none(), "landed: the gate disarms");
    }

    /// Tenure, release: after the program exits the cat lingers for `RELEASE`,
    /// so exit-and-relaunch inside that window bounces nothing, and only a
    /// settled return to the prompt hands the cursor back to the base cat.
    #[test]
    fn a_worn_program_cat_lingers_and_a_relaunch_never_bounces() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        gate.observe(Some(&claude), t0);
        assert!(gate.observe(Some(&claude), t0 + TENURE).is_some());

        // Exit to the prompt: still the claude cat, release pending.
        let t_exit = t0 + Duration::from_secs(60);
        assert_eq!(
            gate.observe(None, t_exit).map(|i| i.id.as_str()),
            Some("claude"),
            "the cat lingers at the prompt"
        );
        assert_eq!(gate.deadline(), Some(t_exit + RELEASE));
        // Relaunch inside the window: home again, nothing lands, no bounce.
        assert_eq!(
            gate.observe(Some(&claude), t_exit + Duration::from_secs(4))
                .map(|i| i.id.as_str()),
            Some("claude")
        );
        assert!(
            gate.deadline().is_none(),
            "the round trip came home: disarmed"
        );

        // A settled return to the prompt releases the cat.
        let t_exit2 = t_exit + Duration::from_secs(120);
        assert!(gate.observe(None, t_exit2).is_some(), "lingering…");
        assert!(
            gate.observe(None, t_exit2 + RELEASE - Duration::from_millis(1))
                .is_some(),
            "…right up to the release dwell"
        );
        assert!(
            gate.observe(None, t_exit2 + RELEASE).is_none(),
            "settled at the prompt: the base cat is back"
        );
    }

    /// The wake-time landing (`poll`): a due candidate lands and disarms
    /// without a present, so no armed deadline is left unconsumed on a
    /// non-terminal route. A not-yet-due candidate is left as it was; a stale
    /// landing is corrected by the next `observe`.
    #[test]
    fn poll_lands_a_due_candidate_and_disarms_without_a_present() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        gate.observe(Some(&claude), t0);
        assert!(
            !gate.poll(t0 + TENURE - Duration::from_millis(1)),
            "not due: untouched"
        );
        assert_eq!(gate.deadline(), Some(t0 + TENURE), "…and still armed");
        assert!(gate.poll(t0 + TENURE), "due: lands (the look moved)");
        assert_eq!(gate.worn().map(|i| i.id.as_str()), Some("claude"));
        assert!(
            gate.deadline().is_none(),
            "…and disarms: nothing left to spin on"
        );
        assert!(
            !gate.poll(t0 + TENURE + Duration::from_secs(1)),
            "idempotent once landed"
        );

        // The linger's release, likewise, lands at the wake without a present…
        let t1 = t0 + Duration::from_secs(60);
        gate.observe(None, t1);
        assert!(gate.poll(t1 + RELEASE), "release lands at its deadline");
        assert!(gate.worn().is_none() && gate.deadline().is_none());

        // …and a claim that moved on meanwhile is corrected by the next observe:
        // vim was seen once, poll landed it blind, but the pane is at the prompt.
        let vim = ident("vim");
        let t2 = t1 + Duration::from_secs(120);
        gate.observe(Some(&vim), t2);
        assert!(gate.poll(t2 + TENURE));
        assert_eq!(
            gate.worn().map(|i| i.id.as_str()),
            Some("vim"),
            "landed blind"
        );
        assert_eq!(
            gate.observe(None, t2 + TENURE + Duration::from_millis(16))
                .map(|i| i.id.as_str()),
            Some("vim"),
            "the next observe sees the prompt and starts the RELEASE clock"
        );
        assert_eq!(
            gate.deadline(),
            Some(t2 + TENURE + Duration::from_millis(16) + RELEASE)
        );
    }

    /// Program to program: leaving claude for vim, the vim cat arrives after
    /// its own tenure (the claude cat lingers meanwhile); a different candidate
    /// mid-dwell restarts the clock, so a flapping raw claim never lands.
    #[test]
    fn switching_programs_and_flapping_claims() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        let vim = ident("vim");
        let git = ident("git");
        gate.observe(Some(&claude), t0);
        gate.observe(Some(&claude), t0 + TENURE);

        let t1 = t0 + Duration::from_secs(30);
        assert_eq!(
            gate.observe(Some(&vim), t1).map(|i| i.id.as_str()),
            Some("claude")
        );
        // A stray `git` claim 2 s in restarts the candidate clock…
        let t2 = t1 + Duration::from_secs(2);
        gate.observe(Some(&git), t2);
        assert_eq!(
            gate.deadline(),
            Some(t2 + TENURE),
            "the clock restarted on git"
        );
        // …and vim again 1 s later restarts it once more.
        let t3 = t2 + Duration::from_secs(1);
        gate.observe(Some(&vim), t3);
        assert_eq!(gate.deadline(), Some(t3 + TENURE));
        assert_eq!(
            gate.observe(Some(&vim), t3 + TENURE - Duration::from_millis(1))
                .map(|i| i.id.as_str()),
            Some("claude"),
            "still claude until vim's own tenure is served"
        );
        assert_eq!(
            gate.observe(Some(&vim), t3 + TENURE).map(|i| i.id.as_str()),
            Some("vim")
        );
    }

    /// A↔B alternation: each program is a stranger exactly once and every
    /// later landing is a homecoming, so ceremonies scale with how many tools
    /// you use, not how often you switch. Ten landings over 200 s: exactly two
    /// `Ceremony`.
    #[test]
    fn alternating_two_programs_greets_each_once() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        let (vim, claude) = (ident("vim"), ident("claude"));
        let mut ceremonies = Vec::new();
        for n in 0..10u64 {
            let now = t0 + Duration::from_secs(5 + 20 * n);
            let cand = if n % 2 == 0 { &vim } else { &claude };
            gate.land(Some(cand.clone()), now);
            if gate.arrival() == PetArrival::Ceremony {
                ceremonies.push(n);
                gate.commit_hello(now);
            }
        }
        assert_eq!(
            ceremonies,
            [0, 1],
            "one hello per cat, both up front; every return is quiet"
        );
    }

    /// The same landing has identical roster effects through `observe` and
    /// `poll`, so a window off the terminal route (which lands only via
    /// `poll`) follows the same rule.
    #[test]
    fn poll_lands_the_roster_like_observe() {
        let t0 = Instant::now();
        let claude = ident("claude");
        let mut via_observe = KittyTenure::default();
        let mut via_poll = KittyTenure::default();

        // The stranger's arrival, landed by each path.
        via_observe.observe(Some(&claude), t0);
        via_observe.observe(Some(&claude), t0 + TENURE);
        via_poll.observe(Some(&claude), t0);
        assert!(via_poll.poll(t0 + TENURE));
        for (name, gate) in [("observe", &via_observe), ("poll", &via_poll)] {
            assert_eq!(gate.arrival(), PetArrival::Ceremony, "{name}: a stranger");
        }
        via_observe.commit_hello(t0 + TENURE);
        via_poll.commit_hello(t0 + TENURE);

        // The release to the base cat, landed by each path: quiet.
        let t1 = t0 + Duration::from_secs(60);
        via_observe.observe(None, t1);
        via_observe.observe(None, t1 + RELEASE);
        via_poll.observe(None, t1);
        assert!(via_poll.poll(t1 + RELEASE));
        for (name, gate) in [("observe", &via_observe), ("poll", &via_poll)] {
            assert!(gate.worn().is_none(), "{name}: the base cat is back");
            assert_eq!(gate.arrival(), PetArrival::Quiet, "{name}: home is quiet");
        }

        // The return within HOMECOMING, landed by each path: a homecoming.
        let t2 = t1 + RELEASE + Duration::from_secs(10);
        via_observe.observe(Some(&claude), t2);
        via_observe.observe(Some(&claude), t2 + TENURE);
        via_poll.observe(Some(&claude), t2);
        assert!(via_poll.poll(t2 + TENURE));
        for (name, gate) in [("observe", &via_observe), ("poll", &via_poll)] {
            assert_eq!(gate.arrival(), PetArrival::Quiet, "{name}: a homecoming");
            let h = gate
                .roster
                .iter()
                .flatten()
                .find(|h| h.id == "claude")
                .expect("the roster remembers claude on both paths");
            assert!(h.greeted, "{name}: the debt stays paid");
            assert_eq!(h.at, t2 + TENURE, "{name}: the wear refreshed the stamp");
        }
    }

    /// The floor spaces but never denies: a stranger landing inside
    /// `HELLO_FLOOR` of a performed hello arrives quietly but keeps its debt
    /// (`greeted` stays false), and gets the full ceremony on a later landing
    /// once the floor is clear.
    #[test]
    fn a_stranger_spaced_by_the_floor_keeps_its_debt() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(Some(ident("vim")), t0);
        assert_eq!(gate.arrival(), PetArrival::Ceremony);
        gate.commit_hello(t0);

        // A second stranger 5 s later: floored, debt held.
        let t1 = t0 + Duration::from_secs(5);
        gate.land(Some(ident("claude")), t1);
        assert_eq!(gate.arrival(), PetArrival::Quiet, "spaced by the floor");
        let debt = gate.roster.iter().flatten().find(|h| h.id == "claude");
        assert!(
            debt.is_some_and(|h| !h.greeted),
            "the debt is owed, not spent"
        );

        // Back once the floor is clear: the held debt is collected.
        let t2 = t0 + HELLO_FLOOR + Duration::from_secs(1);
        gate.land(Some(ident("claude")), t2);
        assert_eq!(
            gate.arrival(),
            PetArrival::Ceremony,
            "the debt is collected"
        );
    }

    /// An unpaid debt pins its slot: the LRU evicts only greeted slots, and
    /// when every slot holds a debt the insert is refused (floor-spaced quiet)
    /// rather than any debt dropped.
    #[test]
    fn an_ungreeted_slot_is_never_evicted() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        // One greeted cat…
        gate.land(Some(ident("vim")), t0);
        gate.commit_hello(t0);
        // …then seven strangers, authorised but never performed (debts).
        for n in 0..7u64 {
            gate.land(
                Some(ident(&format!("tool{n}"))),
                t0 + Duration::from_secs(20 + 20 * n),
            );
        }
        // The ninth id must evict the greeted vim, never a debt.
        let t_full = t0 + Duration::from_secs(200);
        gate.land(Some(ident("htop")), t_full);
        assert!(
            gate.roster.iter().flatten().all(|h| h.id != "vim"),
            "the one greeted slot was the LRU victim"
        );
        for n in 0..7u64 {
            let id = format!("tool{n}");
            assert!(
                gate.roster
                    .iter()
                    .flatten()
                    .any(|h| h.id == id && !h.greeted),
                "{id}: an unpaid debt pins its slot"
            );
        }
        // All eight slots now hold debts: a tenth id is refused, quietly.
        let t_refused = t_full + Duration::from_secs(60);
        gate.land(Some(ident("git")), t_refused);
        assert_eq!(
            gate.arrival(),
            PetArrival::Quiet,
            "a full-of-debts roster refuses the insert as floor-spaced quiet"
        );
        assert!(
            gate.roster.iter().flatten().all(|h| h.id != "git"),
            "…and records nothing"
        );
    }

    /// The base cat is always home: a landing on `None` is quiet on a fresh
    /// gate and after ceremonies, since the pet is born wearing the launch
    /// kitty.
    #[test]
    fn the_base_cat_is_always_a_homecoming() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(None, t0);
        assert_eq!(gate.arrival(), PetArrival::Quiet, "born home");
        gate.land(Some(ident("claude")), t0 + Duration::from_secs(5));
        assert_eq!(gate.arrival(), PetArrival::Ceremony);
        // No commit (the ceremony may still be pending on glass): the fall
        // to the prompt is quiet regardless.
        gate.land(None, t0 + Duration::from_secs(40));
        assert_eq!(
            gate.arrival(),
            PetArrival::Quiet,
            "the prompt is always home"
        );
        // …and an hour later, still quiet: home never expires.
        gate.land(None, t0 + Duration::from_secs(3600));
        assert_eq!(gate.arrival(), PetArrival::Quiet);
    }

    /// Expiry skip: a continuously worn cat never re-lands, so its roster
    /// stamp goes stale while worn. Its own next landing must skip it in the
    /// lazy-expiry scan, or a long session in one tool would end with a
    /// spurious ceremony.
    #[test]
    fn a_worn_cat_survives_a_long_heads_down_stretch() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(Some(ident("claude")), t0);
        gate.commit_hello(t0);

        // Heads-down in claude for 20 min: worn continuously, no landings,
        // the stamp untouched at t0 — then a settled prompt errand releases
        // the cat (a quiet landing that runs no expiry: base early-return).
        let t_errand = t0 + Duration::from_secs(1200);
        gate.land(None, t_errand);
        assert_eq!(gate.arrival(), PetArrival::Quiet);

        // Relaunch: claude's stamp is 20 min stale, but claude is the
        // landing id — skipped by the expiry scan, so this is a homecoming.
        let t_back = t_errand + Duration::from_secs(30);
        gate.land(Some(ident("claude")), t_back);
        assert_eq!(
            gate.arrival(),
            PetArrival::Quiet,
            "a cat worn heads-down is still yours when you relaunch it"
        );
        assert!(
            gate.roster
                .iter()
                .flatten()
                .any(|h| h.id == "claude" && h.greeted && h.at == t_back),
            "…and the landing refreshed its stamp"
        );
    }

    /// (d) The precedence law, rung by rung: a pinned favourite beats the
    /// tenured program cat, the program cat beats the launch kitty, and the
    /// launch kitty is the floor — and each verdict NAMES its winning rung,
    /// so the rate law's sync sites can tell a program win from the rest.
    #[test]
    fn precedence_favourite_beats_program_beats_the_launch_kitty_floor() {
        let favourite = KittyLook {
            coat: 2,
            ..KittyLook::default()
        }
        .normalized();
        let app = KittyLook::for_app("claude");
        let launch = KittyLook::for_launch(0x5EED);
        assert!(
            favourite != launch && app != launch && favourite != app,
            "fixture: the three rungs are distinguishable"
        );
        assert_eq!(
            companion_precedence(Some(favourite), Some(app), launch),
            (favourite, CompanionRung::Favourite),
            "a pinned favourite owns the companion look"
        );
        assert_eq!(
            companion_precedence(None, Some(app), launch),
            (app, CompanionRung::Program),
            "a program that earned the cursor outranks the base cat"
        );
        assert_eq!(
            companion_precedence(None, None, launch),
            (launch, CompanionRung::Launch),
            "the launch kitty is the floor"
        );
    }
}
