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
//! 4. **THE HOMECOMING — the rate law** (owner ruling 2026-08-20: *"the
//!    problem before was that the cat swap animation was every time I
//!    switched tabs with differing programs"*). The dwell decides WHEN the
//!    costume moves; the per-window roster on [`KittyTenure`] decides HOW
//!    LOUDLY. A program cat this window has worn within [`HOMECOMING`]
//!    (refreshed on every wear) comes home QUIETLY; only a cat this window
//!    has never worn, or has not worn in ten minutes, is owed the full
//!    arrival ceremony — and a stranger is never denied one, only spaced,
//!    by [`HELLO_FLOOR`]. The roster authorises; the render seam performs
//!    and commits ([`KittyTenure::commit_hello`]), so a hello nobody saw is
//!    never spent.
//!
//! THE PET IS PLEASANT COMPANY, NOT A STATUS INDICATOR — said out loud
//! because the 2026-08-07 spec could be read either way. The costume is
//! ALWAYS correct (the roster never rations the verdict, only the theater);
//! the ANNOUNCEMENT is rationed to strangers. And the rate law deliberately
//! does NOT solve the 4-bit identity legibility problem: the walking pet's
//! whole visible identity is a coat index plus a functionally mute iris
//! (`PetCursorFrame` carries no `variant`, so even the three hand-designed
//! flagship heads are invisible on it), and making the ceremony rare makes
//! each surviving one carry MORE meaning through a vocabulary that cannot
//! carry it. Legible identity is a different design — arguably the one that
//! should follow this — and it is not attempted here.
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
pub(crate) const HOMECOMING: Duration = Duration::from_secs(600);

/// THE HELLO FLOOR: the minimum spacing between two PERFORMED ceremonies.
/// Not taste: it is [`RELEASE`], quoted — the gate's own longest dwell is
/// the house's existing statement of "you have genuinely settled", so two
/// hellos are never closer than the gate's own settling time. It is 3×
/// [`TENURE`], so the pathological 5 s alternation can never chain hellos.
/// It SPACES strangers; it never denies one: a floored stranger keeps its
/// debt (`greeted` stays false) and collects on a later landing.
pub(crate) const HELLO_FLOOR: Duration = Duration::from_secs(15);

/// The roster's depth: a working set, not a history. Above the realistic
/// simultaneous-tool count (the owner's case is 2; a busy split is 3–5)
/// with headroom, and small enough that the landing-time scan is eight
/// string compares a few times an hour. Deliberately NOT `kitty_log`'s
/// `RING_SLOTS = 256`: that ring is sized for a screenful of cats across
/// shared sessions; this is one human's open tools in one window.
pub(crate) const HOMECOMING_SLOTS: usize = 8;

/// HOW LOUDLY a landed claim arrives — the rate law's verdict, read by the
/// render seam via [`KittyTenure::arrival`]. The dwell decided WHEN the
/// costume moved; this decides whether the move is announced. A sticky
/// latch: it holds the LAST landing's ruling until the next landing, a
/// performed ceremony ([`KittyTenure::commit_hello`]) or a non-program
/// verdict ([`KittyTenure::note_non_program_verdict`]) rewrites it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Arrival {
    /// The full arrival ceremony — owed only to a stranger: a cat this
    /// window has never worn, or has not worn within [`HOMECOMING`].
    Ceremony,
    /// The quiet in-place re-dress. The costume still moves (the roster
    /// never rations the verdict, only the theater); nothing is announced.
    #[default]
    Quiet,
}

/// One remembered cat: a program whose costume this window has actually put
/// on the pet. `at` is the LAST time it was worn (refreshed on every wear,
/// INCLUDING quiet ones — two tools in rotation never grow stale, and
/// starving the ceremony under sustained alternation is the design);
/// `greeted` records whether its one full arrival has actually been
/// PERFORMED, not merely authorised.
struct Homecoming {
    /// The canonical app id ([`AppIdentity::id`]), compared as a plain
    /// `String` across at most [`HOMECOMING_SLOTS`] slots — an in-memory
    /// roster is not an identity lane, so it takes the exact compare over
    /// a salt-namespaced hash.
    id: String,
    /// Last worn — the recency clock. Refreshed by every landing of this id.
    at: Instant,
    /// Whether the one owed hello was PERFORMED (committed by the render
    /// seam), not merely authorised. An ungreeted slot is an unpaid debt:
    /// it pins its slot and is never evicted.
    greeted: bool,
}

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
    /// THE HOMECOMING roster: the cats this window has worn, LRU by `at`.
    /// Written only by [`Self::land`] (both landing sites) and
    /// [`Self::commit_hello`]; arms no timer (expiry is lazy, at landings),
    /// so `deadline()` and idle-to-zero are untouched by construction.
    roster: [Option<Homecoming>; HOMECOMING_SLOTS],
    /// TWO CLOCKS, TWO OPPOSITE RULES, AND THE DIFFERENCE IS DELIBERATE:
    /// the roster's `at` refreshes on every wear including quiet ones
    /// (starving the ceremony under alternation is the design); this one is
    /// stamped ONLY by a PERFORMED ceremony ([`Self::commit_hello`]), never
    /// by an authorisation. `kitty_summon` rate-limits a RECORD, where
    /// starvation loses data; this rate-limits a PERFORMANCE, where
    /// starvation is the goal. Do not "fix" either clock to match the other.
    last_hello: Option<Instant>,
    /// The sticky how-loudly latch — see [`Arrival`].
    arrival: Arrival,
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
                // land() runs on EVERY due poll, `moved` or not: a blind
                // re-land of the worn cat must refresh its roster stamp and
                // re-rule `arrival` exactly as the observe path would, or a
                // window off the terminal route silently keeps the old law.
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
    /// LOUDLY, writing the [`Arrival`] latch from the roster:
    ///   * base cat (`None`) — always [`Arrival::Quiet`]: THE BASE CAT IS
    ///     ALWAYS HOME. The pet is born wearing the launch kitty, so the
    ///     resident cat is owned from frame one and every fall to the
    ///     prompt is quiet, forever;
    ///   * roster hit — a homecoming: refresh `at` (every wear, including
    ///     quiet ones), quiet if greeted, else the still-owed debt goes to
    ///     [`Self::floor`];
    ///   * roster miss — a stranger: insert (LRU among GREETED slots only)
    ///     and ask [`Self::floor`].
    ///
    /// LAZY EXPIRY runs only here, never on a wake — the roster arms no
    /// timer, so idle-to-zero survives by construction. It evicts only
    /// GREETED slots older than [`HOMECOMING`], and it SKIPS the currently
    /// landing id: a continuously-worn cat never re-lands (observe's
    /// equal-claim arm clears the candidate without landing), so its stamp
    /// can be arbitrarily stale at its own next landing, and expiring it
    /// then would hand a 20-minute heads-down session a spurious ceremony.
    fn land(&mut self, cand: Option<AppIdentity>, now: Instant) {
        self.worn = cand;
        let Some(id) = self.worn.as_ref() else {
            self.arrival = Arrival::Quiet;
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
                    Arrival::Quiet
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
                    Arrival::Quiet
                };
            }
        }
    }

    /// THE FLOOR — a stranger is never denied its ceremony, only SPACED:
    /// Ceremony iff no hello was ever performed or the last performed one
    /// is at least [`HELLO_FLOOR`] ago; Quiet otherwise, with the debt kept
    /// (`greeted` stays `false`) to be collected on a later landing.
    fn floor(&self, now: Instant) -> Arrival {
        if self
            .last_hello
            .is_none_or(|t| now.duration_since(t) >= HELLO_FLOOR)
        {
            Arrival::Ceremony
        } else {
            Arrival::Quiet
        }
    }

    /// HOW LOUDLY the current costume arrived — the rate law's ruling for
    /// the most recent landing, read (never advanced) by the render seam at
    /// the `sync_look` sites, where the pet's actual pair is in hand.
    pub fn arrival(&self) -> Arrival {
        self.arrival
    }

    /// THE PERFORMANCE COMMIT: called by the render seam ONLY when a
    /// ceremony was actually PERFORMED on glass — never at authorisation
    /// (`companion_verdict` runs for unfocused windows, and a hello nobody
    /// saw must not be spent; the `kitty_summon` precedent, applied
    /// exactly). Marks the worn id `greeted` (the debt is paid), stamps
    /// `last_hello` (the ONLY writer of that clock), and consumes the
    /// latch back to [`Arrival::Quiet`] so a pair the flying-kitty latch
    /// re-delivers later cannot replay a ceremony already performed.
    pub fn commit_hello(&mut self, now: Instant) {
        if let Some(id) = self.worn.as_ref()
            && let Some(h) = self.roster.iter_mut().flatten().find(|h| h.id == id.id)
        {
            h.greeted = true;
        }
        self.last_hello = Some(now);
        self.arrival = Arrival::Quiet;
    }

    /// THE STALE-LATCH RESET: called by the render seam when a NON-program
    /// rung wins the precedence (a pinned favourite, the launch kitty). A
    /// park can be created by a non-landing event — unpinning a favourite
    /// reveals an already-worn program cat — and without this reset the
    /// seam would perform a ceremony authorised by a landing minutes old.
    /// Only the announcement is reset; the roster, its debts and the
    /// `last_hello` clock are untouched (a debt is owed, not spent).
    pub fn note_non_program_verdict(&mut self) {
        self.arrival = Arrival::Quiet;
    }
}

/// Insert a stranger into the roster: an empty slot first, else evict the
/// LRU (oldest `at`) among GREETED slots only — an unpaid debt pins its
/// slot, so a hello can never be lost to churn. Returns `false` (nothing
/// inserted) when every slot holds an ungreeted debt.
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
        assert!(
            slot.resolve(Some(&block(BlockState::PromptOnly, None)))
                .is_none()
        );
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
        assert!(
            slot.resolve(Some(&executing)).is_none(),
            "stable across frames"
        );
        let late_e = block(BlockState::Executing, Some("codex exec"));
        assert_eq!(
            slot.resolve(Some(&late_e))
                .expect("late 633;E re-resolves")
                .id,
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

    /// THE RATE LAW's headline case (the owner's own day): A↔B alternation.
    /// Each program is a stranger exactly once; every later landing is a
    /// homecoming, forever — the ceremony count scales with how many tools
    /// you own, not with how often you tab. Ten landings over 200 s: exactly
    /// two `Ceremony` (the tree never had an alternation test, which is why
    /// the every-tab-switch defect shipped).
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
            if gate.arrival() == Arrival::Ceremony {
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

    /// THE OBSERVE/POLL EQUIVALENCE: the same landing produces identical
    /// roster effects through both landing sites — or a window off the
    /// terminal route (Settings tab, native tab, failed-present backoff),
    /// which lands only via `poll`, would silently keep the old law with no
    /// failing test anywhere.
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
            assert_eq!(gate.arrival(), Arrival::Ceremony, "{name}: a stranger");
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
            assert_eq!(gate.arrival(), Arrival::Quiet, "{name}: home is quiet");
        }

        // The return within HOMECOMING, landed by each path: a homecoming.
        let t2 = t1 + RELEASE + Duration::from_secs(10);
        via_observe.observe(Some(&claude), t2);
        via_observe.observe(Some(&claude), t2 + TENURE);
        via_poll.observe(Some(&claude), t2);
        assert!(via_poll.poll(t2 + TENURE));
        for (name, gate) in [("observe", &via_observe), ("poll", &via_poll)] {
            assert_eq!(gate.arrival(), Arrival::Quiet, "{name}: a homecoming");
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

    /// THE FLOOR SPACES, IT NEVER DENIES: a stranger landing inside
    /// `HELLO_FLOOR` of a performed hello arrives quietly but KEEPS its
    /// debt (`greeted` stays false), and collects the full ceremony on a
    /// later landing once the floor is clear.
    #[test]
    fn a_stranger_spaced_by_the_floor_keeps_its_debt() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(Some(ident("vim")), t0);
        assert_eq!(gate.arrival(), Arrival::Ceremony);
        gate.commit_hello(t0);

        // A second stranger 5 s later: floored, debt held.
        let t1 = t0 + Duration::from_secs(5);
        gate.land(Some(ident("claude")), t1);
        assert_eq!(gate.arrival(), Arrival::Quiet, "spaced by the floor");
        let debt = gate.roster.iter().flatten().find(|h| h.id == "claude");
        assert!(
            debt.is_some_and(|h| !h.greeted),
            "the debt is owed, not spent"
        );

        // Back once the floor is clear: the held debt is collected.
        let t2 = t0 + HELLO_FLOOR + Duration::from_secs(1);
        gate.land(Some(ident("claude")), t2);
        assert_eq!(gate.arrival(), Arrival::Ceremony, "the debt is collected");
    }

    /// AN UNPAID DEBT PINS ITS SLOT: the LRU evicts only greeted slots, so
    /// a hello can never be lost to churn — and when every slot holds a
    /// debt, the insert is refused (floor-spaced quiet) rather than any
    /// debt dropped.
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
            Arrival::Quiet,
            "a full-of-debts roster refuses the insert as floor-spaced quiet"
        );
        assert!(
            gate.roster.iter().flatten().all(|h| h.id != "git"),
            "…and records nothing"
        );
    }

    /// THE BASE CAT IS ALWAYS HOME: a landing on `None` is quiet on a fresh
    /// gate, quiet after ceremonies, quiet forever — the pet is born wearing
    /// the launch kitty, so every fall to the prompt is a homecoming.
    #[test]
    fn the_base_cat_is_always_a_homecoming() {
        let t0 = Instant::now();
        let mut gate = KittyTenure::default();
        gate.land(None, t0);
        assert_eq!(gate.arrival(), Arrival::Quiet, "born home");
        gate.land(Some(ident("claude")), t0 + Duration::from_secs(5));
        assert_eq!(gate.arrival(), Arrival::Ceremony);
        // No commit (the ceremony may still be pending on glass): the fall
        // to the prompt is quiet regardless.
        gate.land(None, t0 + Duration::from_secs(40));
        assert_eq!(gate.arrival(), Arrival::Quiet, "the prompt is always home");
        // …and an hour later, still quiet: home never expires.
        gate.land(None, t0 + Duration::from_secs(3600));
        assert_eq!(gate.arrival(), Arrival::Quiet);
    }

    /// THE EXPIRY SKIP: a continuously-worn cat never re-lands (observe's
    /// equal-claim arm clears the candidate without landing), so its roster
    /// stamp goes stale while it is worn. Its own next landing must SKIP it
    /// in the lazy-expiry scan, or a 20-minute heads-down session would be
    /// paid back with a spurious ceremony.
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
        assert_eq!(gate.arrival(), Arrival::Quiet);

        // Relaunch: claude's stamp is 20 min stale, but claude is the
        // landing id — skipped by the expiry scan, so this is a homecoming.
        let t_back = t_errand + Duration::from_secs(30);
        gate.land(Some(ident("claude")), t_back);
        assert_eq!(
            gate.arrival(),
            Arrival::Quiet,
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
}
