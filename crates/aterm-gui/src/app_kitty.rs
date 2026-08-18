// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PROGRAM CATS WITH TENURE (owner spec 2026-08-07, "each major app gets its
//! own cursor kitty — the claude cat, the codex cat, and a cat for every
//! command"; owner rulings 2026-08-17: *"I like the different cats. I think
//! switching the cats all the time is too abrupt"* / *"what is a good
//! compromise? design something"*).
//!
//! THE COMPROMISE, in three parts:
//!
//! 1. **The base cat is the launch kitty** (`App::launch_kitty`, one per
//!    process, generated at launch — see `launch_kitty.rs`). It is what the
//!    prompt wears. There is no "shell" program cat and no per-session cat.
//! 2. **A program has to EARN the cursor: tenure.** The focused pane's
//!    `Executing` shell block names a program (`app_basename` →
//!    `canonical_app_id` → `KittyLook::for_app`), but that claim becomes the
//!    cat on glass only after it has held, unbroken, for [`TENURE`] — so
//!    `ls`, `git status`, a quick `cargo check` never flip the cat, while
//!    `claude`, `vim`, a long build do. And when the program exits the cat
//!    LINGERS: the base cat returns only after the pane has been back at the
//!    prompt for [`RELEASE`], so exit-and-relaunch, a shell errand
//!    mid-session, or tabbing across panes does not bounce the cat (moving
//!    straight to ANOTHER program takes that program's own [`TENURE`]; the
//!    old cat lingers meanwhile). Focus changes and block changes go through
//!    the SAME gate: every candidate claim, whatever caused it, must be
//!    stable for its dwell before it lands. Net effect: switches happen at
//!    the boundaries that matter, never more often than every few seconds,
//!    and never for a flicker.
//! 3. **The switch itself is soft.** The flying kitty already swaps only
//!    between appearances (`kitty_cursor::set_look`'s per-appearance latch);
//!    the walking pet's breed handoff now lets the old cat run off while the
//!    new one fades UP in place (`kitty_pet::ARRIVE_IN`) instead of
//!    recolouring in one frame.
//!
//! IDENTITY SOURCE: the focused pane's shell block. While a command is
//! `Executing`, its OSC 633;E commandline names the app; every other state
//! (the prompt, a finished command, no block at all, a nested shell) claims
//! nothing and the pane falls to the base cat. Cached per session by
//! `(block id, state, commandline present)` in [`AppKittySlot`] so a
//! commandline is parsed only on shell-block TRANSITIONS, never per frame.
//!
//! THE TENURE GATE lives per WINDOW in [`KittyTenure`] (a window's cat follows
//! its own focused pane; two windows may honestly wear two programs' cats),
//! and THE PRECEDENCE LAW — favourite > program (with tenure) > launch kitty
//! — lives in `launch_kitty::companion_precedence`.

use std::time::{Duration, Instant};

use aterm_core::terminal::{BlockState, OutputBlock};
use aterm_effects::kitty_registry::{KittyLook, SHELL_APP_ID, app_basename, canonical_app_id};

/// How long a program must hold the focused pane, unbroken, before its cat
/// takes the cursor. Long enough that every quick command (a listing, a
/// status, a short build) finishes first; short enough that a tool you
/// settle into is dressed within its first breath.
pub(crate) const TENURE: Duration = Duration::from_secs(5);

/// How long the pane must be back at the prompt (no program claim at all)
/// before the worn program cat is released to the base cat. Deliberately
/// longer than [`TENURE`]: leaving a program is the moment a bounce would
/// hurt most (exit-and-relaunch, a shell errand, a glance at another tab),
/// so the cat you had lingers until you have genuinely settled at the
/// prompt. (A move to ANOTHER program is that program's arrival and takes
/// [`TENURE`]; the old cat lingers until then.)
pub(crate) const RELEASE: Duration = Duration::from_secs(15);

/// The resolved program identity of one pane: canonical id, the raw basename
/// it came from (diagnostics — `id` may canonicalize it), and the breed. The
/// look is a pure function of `id`, carried here so the render rung never
/// re-hashes per frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppIdentity {
    /// Canonical app id (`"claude"`, `"vim"`, a basename).
    pub id: String,
    /// The first-token basename the id was derived from.
    pub basename: String,
    /// The resolved breed for `id`.
    pub look: KittyLook,
}

/// Per-session cache for the pane's raw program claim, keyed by
/// `(block id, block state, commandline present)` so a commandline is parsed
/// only on shell-block TRANSITIONS — never per frame. The third key component
/// covers a late OSC 633;E that lands after 133;C already flipped the block
/// to `Executing`.
#[derive(Default)]
pub struct AppKittySlot {
    key: Option<(u64, BlockState, bool)>,
    identity: Option<AppIdentity>,
}

impl AppKittySlot {
    /// Resolve the pane's RAW program claim from its current shell block,
    /// re-deriving only when the `(block, state, commandline)` key moves.
    /// This is the ungated claim; [`KittyTenure`] decides whether it is worn.
    pub fn resolve(&mut self, block: Option<&OutputBlock>) -> Option<&AppIdentity> {
        let key = block.map(|b| (b.id, b.state, b.commandline.is_some()));
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
        // While a command RUNS the pane belongs to that program. No parseable
        // commandline (shell integration without OSC 633;E, or a bare Enter)
        // means no claim; a nested SHELL is not a program cat either — the
        // prompt, at any depth, is home and home wears the launch kitty.
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

/// THE TENURE GATE (one lives on each `WindowState`, because a window's cat
/// follows its own focused pane): turns the focused pane's raw, instantly
/// flapping program claim into the slow, deliberate one the cat wears.
///
/// State machine (advanced by [`Self::observe`] on every companion verdict,
/// i.e. every present, and by [`Self::poll`] at the wake deadline):
///   * `worn` is the claim on glass (`None` = the base cat);
///   * `candidate` is the raw claim most recently seen that differs from
///     `worn`, with the instant it was first seen. A raw claim equal to
///     `worn` clears the candidate (a round-trip flip that came home lands
///     nothing); a DIFFERENT new claim restarts the candidate's clock;
///   * a candidate that has held for its dwell — [`TENURE`] to arrive at
///     any program (from the base cat or from another program), [`RELEASE`]
///     to fall back to the base cat — becomes `worn`.
///
/// Both the switch INTO a program and OUT of one are debounced, so the
/// minimum time between two switches is `TENURE`, quick flickers never
/// land, and the cat lingers after a program exits.
#[derive(Default)]
pub struct KittyTenure {
    worn: Option<AppIdentity>,
    candidate: Option<(Option<AppIdentity>, Instant)>,
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
                    self.worn = raw.cloned();
                    self.candidate = None;
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

    /// The instant the pending candidate (if any) earns its tenure — the
    /// host folds this into its wake deadline so a program that prints
    /// nothing still gets dressed on time, and a lingering cat still
    /// releases on time. `None` while nothing is pending: the gate is
    /// self-disarming, so it never pins the event loop.
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
                self.worn = cand;
                moved
            }
            other => {
                self.candidate = other;
                false
            }
        }
    }
}

/// The dwell a candidate claim must hold before it lands: a program ARRIVES
/// after [`TENURE`]; falling back to the base cat waits [`RELEASE`].
fn dwell_for(candidate: Option<&AppIdentity>) -> Duration {
    if candidate.is_some() { TENURE } else { RELEASE }
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

    /// The RAW claim: prompt-side states claim nothing (the base cat, not a
    /// "shell" cat), a nested shell claims nothing, and an `Executing` block
    /// with a commandline hands the pane to the named program.
    #[test]
    fn prompt_and_shells_claim_nothing_and_executing_names_the_program() {
        let mut slot = AppKittySlot::default();
        assert!(slot.resolve(Some(&block(BlockState::PromptOnly, None))).is_none());
        for state in [BlockState::EnteringCommand, BlockState::Complete] {
            assert!(
                slot.resolve(Some(&block(state, Some("claude --resume"))))
                    .is_none(),
                "{state:?}: prompt-side, no claim"
            );
        }
        assert!(
            slot.resolve(Some(&block(BlockState::Executing, Some("bash"))))
                .is_none(),
            "a nested shell is not a program cat"
        );
        assert!(
            slot.resolve(Some(&block(BlockState::Executing, None)))
                .is_none(),
            "no commandline: no claim"
        );
        assert!(slot.resolve(None).is_none(), "no block: no claim");
        let ident = slot
            .resolve(Some(&block(
                BlockState::Executing,
                Some("/usr/local/bin/claude --resume"),
            )))
            .expect("an executing commandline names the program");
        assert_eq!(ident.id, "claude");
        assert_eq!(ident.basename, "claude");
        assert_eq!(ident.look, KittyLook::for_app("claude"));
    }

    /// The slot re-derives only on key transitions: the same (block, state,
    /// commandline-present) shape keeps the cached identity, and a late
    /// OSC 633;E (commandline arriving mid-Executing) is its own transition.
    #[test]
    fn slot_caches_by_block_state_and_commandline_presence() {
        let mut slot = AppKittySlot::default();
        let executing = block(BlockState::Executing, None);
        assert!(slot.resolve(Some(&executing)).is_none());
        assert!(slot.resolve(Some(&executing)).is_none(), "stable across frames");
        let late_e = block(BlockState::Executing, Some("codex exec"));
        assert_eq!(
            slot.resolve(Some(&late_e)).expect("late 633;E re-resolves").id,
            "codex"
        );
        let done = block(BlockState::Complete, Some("codex exec"));
        assert!(slot.resolve(Some(&done)).is_none(), "complete → base");
    }

    /// TENURE, the arrival half: a program claim lands only after it has held
    /// for `TENURE`; a claim that comes and goes inside that window (a quick
    /// command) never lands.
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
            gate.observe(Some(&ls), t0 + Duration::from_millis(300)).is_none(),
            "300 ms of `ls` earns nothing"
        );
        assert!(gate.observe(None, t0 + Duration::from_millis(400)).is_none());
        assert!(gate.deadline().is_none(), "home again: nothing pending");

        // claude: seen, then held.
        let t1 = t0 + Duration::from_secs(1);
        assert!(gate.observe(Some(&claude), t1).is_none(), "seen, not yet earned");
        assert_eq!(gate.deadline(), Some(t1 + TENURE), "the wake is armed at tenure");
        assert!(
            gate.observe(Some(&claude), t1 + TENURE - Duration::from_millis(1))
                .is_none(),
            "one ms short: still the base cat"
        );
        assert_eq!(
            gate.observe(Some(&claude), t1 + TENURE).map(|i| i.id.as_str()),
            Some("claude"),
            "tenure served: the claude cat"
        );
        assert!(gate.deadline().is_none(), "landed: the gate disarms");
    }

    /// TENURE, the release half: after the program exits the cat LINGERS
    /// for `RELEASE` — exit-and-relaunch inside that window bounces nothing,
    /// and only a settled return to the prompt hands the cursor back to the
    /// base cat.
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
        assert!(gate.deadline().is_none(), "the round trip came home: disarmed");

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

    /// THE WAKE-TIME LANDING (`poll`): a due candidate lands and disarms
    /// WITHOUT a present — the guard against an armed deadline that nothing
    /// on a non-terminal route would ever consume (an event-loop spin). A
    /// not-yet-due candidate is left exactly as it was; a stale landing is
    /// corrected by the next `observe`.
    #[test]
    fn poll_lands_a_due_candidate_and_disarms_without_a_present() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        let claude = ident("claude");
        gate.observe(Some(&claude), t0);
        assert!(!gate.poll(t0 + TENURE - Duration::from_millis(1)), "not due: untouched");
        assert_eq!(gate.deadline(), Some(t0 + TENURE), "…and still armed");
        assert!(gate.poll(t0 + TENURE), "due: lands (the look moved)");
        assert_eq!(gate.worn().map(|i| i.id.as_str()), Some("claude"));
        assert!(gate.deadline().is_none(), "…and disarms: nothing left to spin on");
        assert!(!gate.poll(t0 + TENURE + Duration::from_secs(1)), "idempotent once landed");

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
        assert_eq!(gate.worn().map(|i| i.id.as_str()), Some("vim"), "landed blind");
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

    /// Program-to-program: leaving claude for vim, the vim cat arrives after
    /// ITS tenure (the claude cat lingers meanwhile); a different candidate
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
        assert_eq!(gate.observe(Some(&vim), t1).map(|i| i.id.as_str()), Some("claude"));
        // A stray `git` claim 2 s in restarts the candidate clock…
        let t2 = t1 + Duration::from_secs(2);
        gate.observe(Some(&git), t2);
        assert_eq!(gate.deadline(), Some(t2 + TENURE), "the clock restarted on git");
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
}
