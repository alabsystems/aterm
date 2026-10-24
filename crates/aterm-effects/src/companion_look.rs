// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Program cats with tenure: each major app gets its own cursor kitty, but a program must earn the cursor before the cat changes.

use aterm_time::Instant;
use std::time::Duration;

use aterm_core::terminal::{BlockState, OutputBlock};

use crate::companion::CompanionRung;
use crate::kitty_pet::PetArrival;
use crate::kitty_registry::{KittyLook, SHELL_APP_ID, app_basename, canonical_app_id};

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
#[derive(Default)]
pub struct ProgramClaim {
    key: Option<(u64, u64, BlockState, bool)>,
    identity: Option<AppIdentity>,
}

impl ProgramClaim {
    /// `session`'s raw program claim from its current shell block, re-derived only when the key moves.
    #[inline]
    pub fn resolve(&mut self, session: u64, block: Option<&OutputBlock>) -> Option<&AppIdentity> {
        let key = block.map(|b| (session, b.id, b.state, b.commandline.is_some()));
        if key != self.key {
            self.key = key;
            self.identity = block.and_then(derive_identity);
        }
        self.identity.as_ref()
    }
}

/// The raw program claim of one shell block; `None` falls to the base cat.
#[inline]
fn derive_identity(block: &OutputBlock) -> Option<AppIdentity> {
    match block.state {
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
        _ => None,
    }
}

/// The tenure gate: turns the focused pane's raw, flapping program claim into the slow one the cat wears.
#[derive(Default)]
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

impl KittyTenure {
    /// Feed this frame's raw claim; returns the claim on glass.
    #[inline]
    pub fn observe(&mut self, raw: Option<&AppIdentity>, now: Instant) -> Option<&AppIdentity> {
        if raw == self.worn.as_ref() {
            self.candidate = None;
            return self.worn.as_ref();
        }
        match &self.candidate {
            Some((cand, since)) if cand.as_ref() == raw => {
                if now.saturating_duration_since(*since) >= dwell_for(raw)
                    && let Some((cand, _)) = self.candidate.take()
                {
                    self.land(cand, now);
                }
            }
            _ => self.candidate = Some((raw.cloned(), now)),
        }
        self.worn.as_ref()
    }

    /// The claim on glass (`None` = the base cat), without advancing the gate.
    #[inline]
    pub const fn worn(&self) -> Option<&AppIdentity> {
        self.worn.as_ref()
    }

    /// When the pending candidate earns its tenure, for the host's wake deadline; `None` while nothing is pending, so the gate never pins the event loop.
    #[inline]
    pub fn deadline(&self) -> Option<Instant> {
        self.candidate
            .as_ref()
            .map(|(cand, since)| *since + dwell_for(cand.as_ref()))
    }

    /// Called by the host's wake handler when [`Self::deadline`] is due, present or not: lands the pending candidate and disarms, returning whether the look moved.
    #[inline]
    pub fn poll(&mut self, now: Instant) -> bool {
        match self.candidate.take() {
            Some((cand, since))
                if now.saturating_duration_since(since) >= dwell_for(cand.as_ref()) =>
            {
                let moved = cand != self.worn;
                self.land(cand, now);
                moved
            }
            other => {
                self.candidate = other;
                false
            }
        }
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
        for s in &mut self.roster {
            if s.as_ref().is_some_and(|h| {
                h.greeted && h.id != id.id && now.saturating_duration_since(h.at) >= HOMECOMING
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
        if self
            .last_hello
            .is_none_or(|t| now.saturating_duration_since(t) >= HELLO_FLOOR)
        {
            PetArrival::Ceremony
        } else {
            PetArrival::Quiet
        }
    }

    /// How loudly the most recent landing arrived; read, never advanced.
    #[inline]
    pub const fn arrival(&self) -> PetArrival {
        self.arrival
    }

    /// Called by the render seam only when a ceremony was performed on glass (`companion_verdict` also runs for unfocused windows).
    #[inline]
    pub fn commit_hello(&mut self, now: Instant) {
        if let Some(id) = self.worn.as_ref()
            && let Some(h) = self.roster.iter_mut().flatten().find(|h| h.id == id.id)
        {
            h.greeted = true;
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
    for (i, entry) in roster.iter().enumerate() {
        let Some(h) = entry else {
            slot = Some(i);
            break;
        };
        if h.greeted && oldest_at.is_none_or(|oldest| h.at < oldest) {
            oldest_at = Some(h.at);
            slot = Some(i);
        }
    }
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
        Self {
            claim: ProgramClaim::default(),
            tenure: KittyTenure::default(),
            rung: CompanionRung::Launch,
        }
    }
}

impl LookPolicy {
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
        let (look, rung) = companion_precedence(favourite, app, launch);
        self.rung = rung;
        if rung != CompanionRung::Program {
            self.tenure.note_non_program_verdict();
        }
        look
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
        let (look, rung) = companion_precedence(favourite, app, launch);
        self.rung = rung;
        if rung != CompanionRung::Program {
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

    /// Conformance binding for aterm-spec's `companion_tenure_flicker_model`: the
    /// real gate, driven through the model's alternating storm at a cadence far
    /// below `TENURE`, never changes the worn cat.
    #[test]
    fn companion_tenure_flicker_conformance_real_gate_holds_the_cat_still() {
        let m = aterm_models_effects::companion_tenure_flicker_model();
        let mut st = m.init_state();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        let t0 = Instant::now();
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

    /// Prompt-side states and nested shells claim nothing; an `Executing` block
    /// with a commandline names the program.
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

    /// The claim re-derives only on key transitions; a late OSC 633;E is one.
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

    /// A focus move to another session re-derives the claim even when the blocks
    /// share a key shape.
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

    /// A program claim lands only after holding for `TENURE`.
    #[test]
    fn a_program_earns_the_cursor_only_after_tenure() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        assert!(gate.observe(None, t0).is_none(), "fresh: the base cat");

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

    /// After the program exits the cat lingers for `RELEASE`, so a relaunch
    /// inside it bounces nothing.
    #[test]
    fn a_worn_program_cat_lingers_and_a_relaunch_never_bounces() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        gate.observe(Some(&claude), t0);
        assert!(gate.observe(Some(&claude), t0 + TENURE).is_some());

        let t_exit = t0 + Duration::from_secs(60);
        assert_eq!(
            gate.observe(None, t_exit).map(|i| i.id.as_str()),
            Some("claude"),
            "the cat lingers at the prompt"
        );
        assert_eq!(gate.deadline(), Some(t_exit + RELEASE));
        assert_eq!(
            gate.observe(Some(&claude), t_exit + Duration::from_secs(4))
                .map(|i| i.id.as_str()),
            Some("claude")
        );
        assert!(
            gate.deadline().is_none(),
            "the round trip came home: disarmed"
        );

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

    /// `poll` lands a due candidate and disarms without a present, leaves a
    /// not-yet-due one alone, and a blind landing is corrected by the next `observe`.
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

        let t1 = t0 + Duration::from_secs(60);
        gate.observe(None, t1);
        assert!(gate.poll(t1 + RELEASE), "release lands at its deadline");
        assert!(gate.worn().is_none() && gate.deadline().is_none());

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

    /// A new program arrives after its own tenure while the old cat lingers; a
    /// different candidate mid-dwell restarts the clock.
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
        let t2 = t1 + Duration::from_secs(2);
        gate.observe(Some(&git), t2);
        assert_eq!(
            gate.deadline(),
            Some(t2 + TENURE),
            "the clock restarted on git"
        );
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

    /// A↔B alternation greets each program once: ten landings, two ceremonies.
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

    /// `observe` and `poll` landings have identical roster effects.
    #[test]
    fn poll_lands_the_roster_like_observe() {
        let t0 = Instant::now();
        let claude = ident("claude");
        let mut via_observe = KittyTenure::default();
        let mut via_poll = KittyTenure::default();

        via_observe.observe(Some(&claude), t0);
        via_observe.observe(Some(&claude), t0 + TENURE);
        via_poll.observe(Some(&claude), t0);
        assert!(via_poll.poll(t0 + TENURE));
        for (name, gate) in [("observe", &via_observe), ("poll", &via_poll)] {
            assert_eq!(gate.arrival(), PetArrival::Ceremony, "{name}: a stranger");
        }
        via_observe.commit_hello(t0 + TENURE);
        via_poll.commit_hello(t0 + TENURE);

        let t1 = t0 + Duration::from_secs(60);
        via_observe.observe(None, t1);
        via_observe.observe(None, t1 + RELEASE);
        via_poll.observe(None, t1);
        assert!(via_poll.poll(t1 + RELEASE));
        for (name, gate) in [("observe", &via_observe), ("poll", &via_poll)] {
            assert!(gate.worn().is_none(), "{name}: the base cat is back");
            assert_eq!(gate.arrival(), PetArrival::Quiet, "{name}: home is quiet");
        }

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

    /// A stranger landing inside `HELLO_FLOOR` of a hello arrives quietly, keeps
    /// its debt, and collects the ceremony on a later landing.
    #[test]
    fn a_stranger_spaced_by_the_floor_keeps_its_debt() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(Some(ident("vim")), t0);
        assert_eq!(gate.arrival(), PetArrival::Ceremony);
        gate.commit_hello(t0);

        let t1 = t0 + Duration::from_secs(5);
        gate.land(Some(ident("claude")), t1);
        assert_eq!(gate.arrival(), PetArrival::Quiet, "spaced by the floor");
        let debt = gate.roster.iter().flatten().find(|h| h.id == "claude");
        assert!(
            debt.is_some_and(|h| !h.greeted),
            "the debt is owed, not spent"
        );

        let t2 = t0 + HELLO_FLOOR + Duration::from_secs(1);
        gate.land(Some(ident("claude")), t2);
        assert_eq!(
            gate.arrival(),
            PetArrival::Ceremony,
            "the debt is collected"
        );
    }

    /// The LRU evicts only greeted slots; with every slot a debt, the insert is
    /// refused (quietly) rather than a debt dropped.
    #[test]
    fn an_ungreeted_slot_is_never_evicted() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(Some(ident("vim")), t0);
        gate.commit_hello(t0);
        for n in 0..7u64 {
            gate.land(
                Some(ident(&format!("tool{n}"))),
                t0 + Duration::from_secs(20 + 20 * n),
            );
        }
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

    /// A landing on the base cat (`None`) is always quiet.
    #[test]
    fn the_base_cat_is_always_a_homecoming() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(None, t0);
        assert_eq!(gate.arrival(), PetArrival::Quiet, "born home");
        gate.land(Some(ident("claude")), t0 + Duration::from_secs(5));
        assert_eq!(gate.arrival(), PetArrival::Ceremony);
        gate.land(None, t0 + Duration::from_secs(40));
        assert_eq!(
            gate.arrival(),
            PetArrival::Quiet,
            "the prompt is always home"
        );
        gate.land(None, t0 + Duration::from_secs(3600));
        assert_eq!(gate.arrival(), PetArrival::Quiet);
    }

    /// A continuously worn cat's stamp goes stale while worn; its own next landing
    /// skips it in the expiry scan, so a long session ends with no spurious ceremony.
    #[test]
    fn a_worn_cat_survives_a_long_heads_down_stretch() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(Some(ident("claude")), t0);
        gate.commit_hello(t0);

        let t_errand = t0 + Duration::from_secs(1200);
        gate.land(None, t_errand);
        assert_eq!(gate.arrival(), PetArrival::Quiet);

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

    /// The precedence law, rung by rung: favourite > tenured program > launch
    /// kitty, and each verdict names its winning rung.
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

    #[test]
    fn tenure_gate_saturates_rewound_clocks_without_panicking() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(10);
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        let vim = ident("vim");
        assert!(gate.observe(Some(&claude), t1).is_none());
        assert!(gate.observe(Some(&claude), t0).is_none());
        assert!(!gate.poll(t0));
        gate.land(Some(claude), t1);
        gate.commit_hello(t1);
        gate.land(Some(vim), t0);
        assert_eq!(gate.arrival(), PetArrival::Quiet);
    }
}
