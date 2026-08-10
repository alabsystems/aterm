// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TYPED-KITTY CAMEO — the toy that appears when you type the word.
//!
//! Owner, 2026-08-09: *"When I type 'kitty' I do not want that to make the
//! CURSOR kitty appear. I want THE kitty to appear."*
//!
//! Before this module, typing a feline word routed through
//! [`crate::kitty_cursor::CursorCat`]'s bounded hello: the CURSOR ESCORT woke
//! up and peeked beside the caret. That answered the keystroke with the wrong
//! animal — the escort is the trail style's companion, a thing that already
//! lives on the cursor, so summoning it reads as "the cursor twitched", not as
//! "a kitty came".
//!
//! The cameo is a free-floating sprite of its OWN:
//!
//! * **Anchored at the caret cell where the word was typed** — not carried
//!   along by the caret afterwards. It is a toy that popped out at a place,
//!   which is why the anchor is latched at summon time and never re-read.
//! * **TEXT-SIZED, like every cat that lives in the terminal's text.** Owner
//!   regression report, 2026-08-10: *"AHH! the kitty that appears when I type
//!   'Kitty' is huge! go back to the old text kitty!"* — clarified: *"like how
//!   it appears in the regular text."* 0.19.0 drew this sprite at the full
//!   2-cell atlas slot with a further 2.0× dest scale — a ~4-cell creature
//!   standing ON the line, in front of the text. The cameo is now sized and
//!   placed by the SAME law as the ambient word-cat peek
//!   (`word_decorations::cameo_footprint_for`): a head at `cat_hart` × its own
//!   age band, chin tucked behind the anchor row, drawn under the text. What
//!   still distinguishes it from the escort is its ANCHOR and its lifecycle,
//!   not its bulk — a kitty typed is a kitty that fits the line.
//! * **Fires on EVERY typed feline completion.** The ledger's cooldown governs
//!   whether a Kitty Log ROW is written; it has never governed whether the user
//!   gets an answer to what they typed.
//! * **Never wakes the companion.** The companion's identity still follows the
//!   ledger through the host's ordinary per-frame `set_look` precedence sync —
//!   a discovery repoints it exactly as before — but nothing here puts it on
//!   glass.
//!
//! ## A cameo is a VIEW, not a discovery
//!
//! Nothing in this module writes a collectible row. The Kitty Log's episode
//! rules run entirely on the host side, before the summon; the sprite is the
//! presentation and only the presentation.
//!
//! ## Clockless, like every engine here
//!
//! The whole lifecycle is a pure function of `now - born`, so there is no
//! `advance`/`tick` to forget to call and nothing to desync: an occluded window
//! simply misses frames and the cat is exactly where the wall clock says it is
//! when the frames come back.

use std::time::Duration;

use web_time::Instant;

use crate::{cat_baker::CatColorKey, kitty_registry::KittyLook};

/// Slide-up entrance, in ms. Short: a toy appearing should feel like a pop, not
/// a reveal.
pub const CAMEO_RISE_MS: u64 = 220;
/// How long the cameo holds at rest before it starts leaving, in ms.
///
/// THE FLOOR IS NOT TASTE. The cameo's presence is what vetoes the ambient
/// word-cat at the same word, and that word-cat's own one-shot keeps advancing
/// underneath the veto (the episode prepass does not know about it). If the toy
/// left first, the suppressed cat's TAIL would pop into view behind it. The
/// ambient peek's worst case is `450 + 3750 + 60 + 320 = 4580 ms` and the
/// driven A2 gate bounds it at 4800 ms, so the cameo's TOTAL must clear 4800 —
/// this dwell puts it at 5040 ms.
pub const CAMEO_DWELL_MS: u64 = 4_400;
/// Fade-out, in ms.
pub const CAMEO_FADE_MS: u64 = 420;
/// The full animated lifetime.
pub const CAMEO_TOTAL_MS: u64 = CAMEO_RISE_MS + CAMEO_DWELL_MS + CAMEO_FADE_MS;

/// REDUCED MOTION shows ONE still pose for this long and then erases it, the
/// same shape as the companion's reduced-motion collection hello: one bounded
/// hold, one erase wake, no frame cadence. Matched to [`CAMEO_TOTAL_MS`] so the
/// veto it holds over the ambient word-cat lasts exactly as long either way.
pub const CAMEO_STATIC_HOLD_MS: u64 = CAMEO_TOTAL_MS;

/// How far the cameo slides up during its entrance, in cells.
pub const CAMEO_RISE_CELLS: f32 = 0.45;

/// The entrance's starting opacity, as a fraction. Non-zero so the summoning
/// frame itself already shows the cat.
pub const CAMEO_RISE_ALPHA_FLOOR: f32 = 0.25;

/// The idle bob's peak displacement, in cells, and its period in ms. Small on
/// purpose — this is "the cat is alive", not a performance. It is also what
/// makes the cameo's frame fingerprint change every frame, which is how a host
/// early-out can never swallow the animation.
pub const CAMEO_BOB_CELLS: f32 = 0.055;
/// The idle bob's period, in ms.
pub const CAMEO_BOB_MS: u64 = 1_500;

/// How soon a reduced-motion cameo asks for another frame while its tile has
/// not baked yet. One ordinary frame interval: long enough not to be a wake
/// train, short enough that the toy is on glass essentially immediately.
pub const CAMEO_BAKE_RETRY_MS: u64 = 16;

// THERE IS DELIBERATELY NO DEST SCALE ANY MORE. 0.19.0 shipped a
// `CAMEO_DEST_SCALE` (1.5, then 2.0) over a tile already baked at the 2-cell
// atlas ceiling — a ~4-cell cat, which is the regression the owner reported on
// 2026-08-10: "AHH! the kitty that appears when I type 'Kitty' is huge! go
// back to the old text kitty!" The cameo now bakes at its exact dest size
// (NEAREST 1:1, like every ambient word-cat) under the ambient head law in
// `word_decorations::cameo_footprint_for`. A multiplier here is the lever the
// next "make it its own cat" pass would reach for first; its absence is the
// point.

/// One live cameo. There is at most one per window: a second typed word
/// REPLACES the first rather than stacking, because two toys for two keystrokes
/// is the pile-up the one-cat-per-caret rule exists to prevent.
#[derive(Clone, Copy, Debug)]
struct Cameo {
    born: Instant,
    /// The caret cell the completing keystroke left behind, LATCHED. The cameo
    /// stays where the word was typed even as the caret walks on.
    anchor: (u16, u16),
    look: KittyLook,
    /// The SESSION the keystroke landed in — the same key a composed host
    /// binds per pane. `None` only for a host that names no session at all;
    /// aterm's summon always tags one.
    ///
    /// `anchor` is a CELL, and cells only mean something inside one session's
    /// grid. Without this, a cameo summoned at (4, 9) in the focused pane would
    /// hand its veto to every pane's scan and silence the unrelated word at
    /// (4, 9) in the pane next door — the same pane-scoping rule
    /// `WordDecorations`'s `companion_claim` carries, for the same reason.
    ///
    /// IT IS A TAB SCOPE TOO, and that is not a bonus — it is load-bearing.
    /// `WordDecorations` is per WINDOW, so every tab in a window shares this
    /// one cameo slot and a tab switch retires nothing. The session tag is the
    /// only thing that keeps a toy typed in tab A off tab B's glass; a host
    /// that draws without naming its session gets the teleport
    /// (see [`KittyCameo::frame_in_pane`]).
    pane: Option<u64>,
    /// THE LOCAL PALETTE THIS TOY BAKED AGAINST, latched at its first drawn
    /// frame exactly like [`Self::anchor`] is latched at summon.
    ///
    /// SKEPTIC'S FINDING, 2026-08-09: the composed host samples the palette out
    /// of the ONE grid-extraction scratch it reuses across panes, which holds
    /// "whichever pane extracted last this frame" — not necessarily this
    /// cameo's owner. Latching gives the host a single frame in which it has to
    /// get the sample right (and [`KittyCameo::wants_colors`] tells it when
    /// that frame is), instead of a five-second stream of samples in which any
    /// one wrong answer both mis-tints the cat and forces an atlas rebake.
    ///
    /// It is also the honest model of the toy: a thing that popped out at a
    /// place, wearing the colours of the place it popped out in.
    colors: Option<CatColorKey>,
}

/// The typed-kitty cameo's state machine: at most one live sprite, plus the
/// motion mode the host last rendered under.
#[derive(Clone, Debug, Default)]
pub struct KittyCameo {
    live: Option<Cameo>,
    /// Whether the live cameo has ever actually landed a sprite.
    ///
    /// The emitter can fail for a frame — the cat atlas admits at most two
    /// bakes per frame and the word-cats take theirs first — and full motion
    /// simply retries on the next cadence frame. REDUCED MOTION has no cadence:
    /// it schedules one erase wake and nothing else, so a first-frame bake miss
    /// would mean the toy is never drawn at all. Until a sprite lands, the
    /// reduced-motion deadline is pulled forward to a short retry.
    drawn: bool,
    /// The motion policy of the LAST tick. Held here rather than passed to
    /// every query because the scheduler asks `is_active` / `static_deadline`
    /// from outside any frame, where no config is in scope — and the two
    /// answers are mutually exclusive: full motion owns a frame cadence,
    /// reduced motion owns exactly one erase deadline.
    reduced: bool,
}

/// The per-frame presentation of a live cameo — everything the emitter needs
/// that is NOT geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameoFrame {
    /// The latched anchor cell (row, col).
    pub anchor: (u16, u16),
    /// The identity to bake.
    pub look: KittyLook,
    /// Straight alpha, `0` meaning "nothing to draw this frame".
    pub alpha: u8,
    /// Vertical offset in CELLS. Positive moves the head towards its HIDDEN
    /// side — the geometry half (`word_decorations::cameo_footprint_for`)
    /// maps it behind the anchor line, whichever side the head peeks from —
    /// so the entrance always reads as the classic slide-out. Carries both
    /// the entrance slide and the idle bob.
    pub dy: f32,
}

impl KittyCameo {
    /// Summon (or RE-summon) the cameo at `anchor` in `pane`.
    ///
    /// Re-summoning restarts the lifecycle in place rather than queueing:
    /// typing the word twice means the second one is what you asked for.
    ///
    /// `pane` is the split pane the keystroke landed in (`None` for an unsplit
    /// window) — see [`Cameo::pane`].
    pub fn summon(
        &mut self,
        now: Instant,
        anchor: (u16, u16),
        look: KittyLook,
        pane: Option<u64>,
    ) {
        self.live = Some(Cameo {
            born: now,
            anchor,
            look: look.normalized(),
            pane,
            colors: None,
        });
        self.drawn = false;
    }

    /// Whether the emitter still needs a freshly SAMPLED palette for the live
    /// cameo, i.e. nothing has been latched yet ([`Cameo::colors`]).
    ///
    /// The composed host reads this to decide whether it is worth extracting
    /// the owning pane's grid: once a cameo has its colours, the remaining ~300
    /// frames of its life need no sample at all.
    #[must_use]
    pub fn wants_colors(&self) -> bool {
        self.live.is_some_and(|c| c.colors.is_none())
    }

    /// The palette the live cameo has latched, if any. The observable half of
    /// [`Self::latch_colors`] — see `WordDecorations::cameo_colors`.
    #[must_use]
    pub fn latched_colors(&self) -> Option<CatColorKey> {
        self.live.and_then(|c| c.colors)
    }

    /// Latch `sampled` as this cameo's palette if it has none yet, and return
    /// the palette it will actually bake with.
    ///
    /// First writer wins: a later frame's sample cannot repaint a toy that is
    /// already on glass.
    pub fn latch_colors(&mut self, sampled: CatColorKey) -> CatColorKey {
        match self.live.as_mut() {
            Some(c) => *c.colors.get_or_insert(sampled),
            None => sampled,
        }
    }

    /// Record that the emitter actually landed this cameo's sprite. Retires the
    /// reduced-motion bake-retry wake ([`Self::drawn`]).
    pub fn note_drawn(&mut self) {
        self.drawn = true;
    }

    /// Record the motion policy the host is rendering under. Called from the
    /// engine tick BEFORE any early-out, so a cameo over an empty grid (a
    /// no-echo password prompt scans zero words) still resolves its mode.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced = reduced;
    }

    /// Drop any live cameo. Used by the engine's hard reset, where "fresh
    /// start" is explicit user intent.
    pub fn clear(&mut self) {
        self.live = None;
    }

    /// The cell the live cameo is anchored to, while it is still on glass.
    ///
    /// THE ONE-CAT-PER-CARET VETO reads this: the ambient word-scanner must not
    /// peek its own cat at the very word the cameo is already answering, or one
    /// keystroke draws two cats a couple of cells apart. Returning the anchor
    /// (rather than the live caret) is deliberate — the veto has to name the
    /// word the CAMEO came for, which is the word that was under the caret when
    /// it was summoned.
    ///
    /// `scanning_pane` is the SESSION whose scan is asking — the engine's
    /// `scan_scope`: the bound pane on a composed host, or the session an
    /// unsplit host declared. A cameo only vetoes inside the session its anchor
    /// cell actually belongs to; `None` on either side is the last-resort
    /// wildcard for a caller that genuinely names no session.
    ///
    /// **THAT WILDCARD IS A LOADED GUN AND IT HAS GONE OFF TWICE.** A veto is a
    /// SUPPRESSION, so an over-wide scope is silent by construction: the missing
    /// thing is a cat that never appeared. `WordDecorations` is per WINDOW and a
    /// tab switch retires nothing, so while the unsplit renderer passed `None`
    /// here, a cameo typed in tab A suppressed the identically-placed ambient
    /// cat in tab B for its whole five-second life (skeptic's third round,
    /// 2026-08-09; closed by `WordDecorations::set_scan_session`). Pass the
    /// session you are scanning unless you have a reason not to — and "the host
    /// binds no pane" is not one, because an unsplit window still knows which
    /// session it is showing.
    #[must_use]
    pub fn veto_cell(&self, now: Instant, scanning_pane: Option<u64>) -> Option<(u16, u16)> {
        let c = self.live?;
        if let (Some(mine), Some(asking)) = (c.pane, scanning_pane)
            && mine != asking
        {
            return None;
        }
        self.frame(now).map(|f| f.anchor)
    }

    /// Whether the cameo needs the 60 fps frame cadence.
    ///
    /// Reduced motion answers `false` unconditionally: that mode shows one
    /// still pose and arms exactly one erase deadline
    /// ([`Self::static_deadline`]) instead of a wake train. Without this the
    /// cameo would pin a reduced-motion user at full frame rate for its whole
    /// life, which is the opposite of what the setting asks for.
    ///
    /// PANE-BLIND: "is this toy animating", not "can anyone see it". Hosts that
    /// schedule frames must ask [`Self::is_active_in`] instead.
    #[must_use]
    pub fn is_active(&self, now: Instant) -> bool {
        if self.reduced {
            return false;
        }
        self.live.is_some_and(|c| {
            (now.saturating_duration_since(c.born).as_millis() as u64) < CAMEO_TOTAL_MS
        })
    }

    /// [`Self::is_active`], narrowed to a cameo whose owning session `visible`
    /// says is actually on glass — THE PREDICATE A SCHEDULER MUST USE.
    ///
    /// SKEPTIC'S FINDING, 2026-08-09: `WordDecorations` is per WINDOW and a tab
    /// switch retires nothing, so a cameo summoned in tab A stays live after
    /// the user moves to tab B. `frame_in_pane` correctly refuses to DRAW it
    /// there — and the wake train stayed armed anyway, holding a window at
    /// 60 fps for the toy's full [`CAMEO_TOTAL_MS`] to present a cat that no
    /// path emits. An invisible animation is not a reason to render.
    ///
    /// A cameo with no session (`Cameo::pane == None`, a host that binds no
    /// pane) is always visible: there is only the one grid.
    #[must_use]
    pub fn is_active_in(&self, now: Instant, visible: impl Fn(u64) -> bool) -> bool {
        self.is_active(now) && self.owner_visible(&visible)
    }

    /// Whether the live cameo's owning session passes `visible`. Vacuously true
    /// when nothing is live — every caller pairs it with a liveness test.
    fn owner_visible(&self, visible: &impl Fn(u64) -> bool) -> bool {
        self.live
            .is_none_or(|c| c.pane.is_none_or(visible))
    }

    /// The REDUCED-MOTION one-shot erase wake: the instant the single held pose
    /// must be taken off glass.
    ///
    /// `None` under full motion (the frame cadence owns that case) and `None`
    /// once the hold has elapsed — an already-erased cameo owns no clock, which
    /// is what returns the scheduler to 0% idle.
    #[must_use]
    pub fn static_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.reduced {
            return None;
        }
        let c = self.live?;
        let until = c.born + Duration::from_millis(CAMEO_STATIC_HOLD_MS);
        if now >= until {
            return None;
        }
        if self.drawn {
            return Some(until);
        }
        // Not on glass yet: keep asking for a frame soon (bounded by the hold,
        // so this can never outlive the cameo) until a bake lands.
        Some((now + Duration::from_millis(CAMEO_BAKE_RETRY_MS)).min(until))
    }

    /// [`Self::static_deadline`], narrowed to a visible owner — the reduced-motion
    /// twin of [`Self::is_active_in`], and it closes the SAME defect in the other
    /// lane.
    ///
    /// The bake-retry branch above is the sharp edge: a cameo hidden behind a tab
    /// switch never lands a sprite, so `drawn` stays `false` and the retry
    /// re-arms every [`CAMEO_BAKE_RETRY_MS`] — a ~62 fps wake train for the toy's
    /// whole hold, in the very mode whose contract is "one still pose, one erase,
    /// no cadence". A hidden toy needs neither: nothing drew it, so nothing has
    /// to erase it, and switching back schedules a frame that recomputes this.
    #[must_use]
    pub fn static_deadline_in(
        &self,
        now: Instant,
        visible: impl Fn(u64) -> bool,
    ) -> Option<Instant> {
        if !self.owner_visible(&visible) {
            return None;
        }
        self.static_deadline(now)
    }

    /// WHICH PANE OWNS THE TOY, while it is on glass.
    ///
    /// The outer `Option` is "is a cameo live at all"; the inner one is the
    /// summoning pane (`None` from a host that binds no pane). Composed hosts
    /// ask this to pick the GEOMETRY they emit at, which is what makes the
    /// cameo's scope identical on both sides of the seam: it is drawn in the
    /// pane it was typed in, and it vetoes the ambient cat in the pane it was
    /// typed in. Before this the emitter drew at whatever pane had FOCUS, so
    /// moving focus in a split left the veto standing over a pane where nothing
    /// was being drawn.
    #[must_use]
    pub fn live_pane(&self, now: Instant) -> Option<Option<u64>> {
        let c = self.live?;
        self.frame(now).map(|_| c.pane)
    }

    /// [`Self::frame`], scoped to the pane/session doing the drawing.
    ///
    /// A cameo belongs to the pane the word was typed in. Focus can move to
    /// another pane while the toy is still on glass, and the composed renderer
    /// emits at whatever pane is focused NOW — so without this the toy would
    /// teleport across the divider to the same cell in a pane it was never
    /// summoned in. The same check is what keeps it off ANOTHER TAB's grid:
    /// one window, one cameo slot, and nothing retires it on a tab switch.
    ///
    /// **`pane: None` IS A WILDCARD — "draw me anywhere".** It exists for
    /// callers that genuinely name no session, and a caller that passes it out
    /// of convenience re-opens the teleport this scope closes: the single-pane
    /// renderer used to, and typing `kitty` in tab A then selecting tab B put
    /// the toy on tab B (skeptic's finding, 2026-08-09; fixed in
    /// `aterm_gui::app_render::present_typed_cameo`, which names the front
    /// session). Pass the session you are drawing unless you have a reason not
    /// to.
    #[must_use]
    pub fn frame_in_pane(&self, now: Instant, pane: Option<u64>) -> Option<CameoFrame> {
        let c = self.live?;
        if let (Some(mine), Some(drawing)) = (c.pane, pane)
            && mine != drawing
        {
            return None;
        }
        self.frame(now)
    }

    /// This frame's presentation, or `None` when nothing should be drawn.
    ///
    /// Pure in `(now, state)`: no interior mutation, so an introspection
    /// capture can sample the same instant twice and get the same answer.
    #[must_use]
    pub fn frame(&self, now: Instant) -> Option<CameoFrame> {
        let c = self.live?;
        let t = now.saturating_duration_since(c.born).as_millis() as u64;
        if self.reduced {
            // ONE STILL POSE, at rest — no entrance, no bob, no fade. The
            // engine-wide reduced-motion idiom: the user sees the thing, and it
            // leaves on a deadline instead of an animation.
            return (t < CAMEO_STATIC_HOLD_MS).then_some(CameoFrame {
                anchor: c.anchor,
                look: c.look,
                alpha: 255,
                dy: 0.0,
            });
        }
        if t >= CAMEO_TOTAL_MS {
            return None;
        }
        // ENTRANCE: slide up into rest while fading in. Eased with the
        // smoothstep the rest of the crate uses for one-shot entrances so the
        // arrival decelerates instead of stopping dead.
        let (alpha, slide) = if t < CAMEO_RISE_MS {
            let p = t as f32 / CAMEO_RISE_MS as f32;
            let e = p * p * (3.0 - 2.0 * p);
            // The fade-in starts at a FLOOR, not at zero: a toy that pops out
            // must be on glass on the very frame it was summoned, or the first
            // (and on a no-echo prompt, possibly only) frame the host presents
            // draws nothing at all.
            let opacity = CAMEO_RISE_ALPHA_FLOOR + (1.0 - CAMEO_RISE_ALPHA_FLOOR) * e;
            ((opacity * 255.0) as u8, (1.0 - e) * CAMEO_RISE_CELLS)
        } else if t < CAMEO_RISE_MS + CAMEO_DWELL_MS {
            (255, 0.0)
        } else {
            let p = (t - CAMEO_RISE_MS - CAMEO_DWELL_MS) as f32 / CAMEO_FADE_MS as f32;
            (((1.0 - p).clamp(0.0, 1.0) * 255.0) as u8, 0.0)
        };
        if alpha == 0 {
            return None;
        }
        // IDLE BOB: a slow sine on the total clock, so the entrance and the
        // bob compose rather than fighting over `dy`.
        let phase = (t % CAMEO_BOB_MS) as f32 / CAMEO_BOB_MS as f32;
        let bob = (phase * std::f32::consts::TAU).sin() * CAMEO_BOB_CELLS;
        Some(CameoFrame {
            anchor: c.anchor,
            look: c.look,
            alpha,
            // Positive is TOWARDS HIDDEN: the entrance starts displaced
            // behind the anchor line and slides out into place.
            dy: bob + slide,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kitty_registry::KittyLook;

    fn look() -> KittyLook {
        KittyLook::for_session(7)
    }

    /// The lifecycle: nothing before a summon, a rising entrance, a full-alpha
    /// dwell, a fade, then permanently gone. Each phase asserts the phase
    /// BEFORE it so the sequence cannot rot into "always None".
    #[test]
    fn cameo_rises_dwells_fades_and_stays_gone() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        assert!(cam.frame(t0).is_none(), "no cameo before a summon");
        assert!(!cam.is_active(t0));

        cam.summon(t0, (4, 9), look(), None);
        let birth = cam.frame(t0).expect("a summoned cameo draws at once");
        assert_eq!(birth.anchor, (4, 9));
        assert!(
            birth.dy > 0.0,
            "the entrance starts displaced towards the hidden side"
        );
        assert!(cam.is_active(t0), "full motion owns the frame cadence");

        let dwell = cam
            .frame(t0 + Duration::from_millis(CAMEO_RISE_MS + 100))
            .expect("live through the dwell");
        assert_eq!(dwell.alpha, 255, "the dwell is fully opaque");
        assert!(
            dwell.dy.abs() <= CAMEO_BOB_CELLS + f32::EPSILON,
            "the slide is spent; only the bob remains"
        );

        let fading = cam
            .frame(t0 + Duration::from_millis(CAMEO_RISE_MS + CAMEO_DWELL_MS + CAMEO_FADE_MS / 2))
            .expect("live through the fade");
        assert!(
            fading.alpha > 0 && fading.alpha < 255,
            "the fade is partial, not a cut: {}",
            fading.alpha
        );

        let after = t0 + Duration::from_millis(CAMEO_TOTAL_MS + 1);
        assert!(cam.frame(after).is_none(), "spent");
        assert!(!cam.is_active(after), "and it owns no wake train");
    }

    /// The bob really moves. Without this the "visible to the frame-cadence
    /// scheduler" property would be untestable — a cameo whose frames are all
    /// identical is one the host's fingerprint early-out can legally swallow.
    #[test]
    fn the_dwell_bob_changes_between_frames() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        cam.summon(t0, (0, 0), look(), None);
        let at = |ms: u64| {
            cam.frame(t0 + Duration::from_millis(CAMEO_RISE_MS + ms))
                .expect("in dwell")
                .dy
        };
        let a = at(0);
        let b = at(CAMEO_BOB_MS / 4);
        assert!(
            (a - b).abs() > 0.001,
            "a quarter bob period must move the cat: {a} vs {b}"
        );
    }

    /// Reduced motion: ONE still pose, no cadence, one erase deadline — and the
    /// deadline disarms once spent (the 0%-idle property).
    #[test]
    fn reduced_motion_holds_one_still_pose_then_erases() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        cam.set_reduced_motion(true);
        cam.summon(t0, (2, 3), look(), None);

        let still = cam.frame(t0).expect("the still pose shows immediately");
        assert_eq!((still.alpha, still.dy), (255, 0.0), "no entrance, no bob");
        assert_eq!(
            cam.frame(t0 + Duration::from_millis(CAMEO_STATIC_HOLD_MS / 2)),
            Some(still),
            "the pose is STILL — byte-identical across the hold"
        );
        assert!(
            !cam.is_active(t0),
            "reduced motion must never arm the frame cadence"
        );
        // BEFORE the toy has landed a sprite, the wake is a short BAKE RETRY:
        // reduced motion has no frame cadence, so a tile that lost the shared
        // two-bake budget would otherwise never get a second chance.
        assert_eq!(
            cam.static_deadline(t0),
            Some(t0 + Duration::from_millis(CAMEO_BAKE_RETRY_MS)),
            "an undrawn cameo asks for a retry frame, not just its erase"
        );
        cam.note_drawn();
        let deadline = cam
            .static_deadline(t0)
            .expect("reduced motion arms exactly one erase wake");
        assert_eq!(
            deadline,
            t0 + Duration::from_millis(CAMEO_STATIC_HOLD_MS),
            "once on glass the retry retires and only the erase remains"
        );

        let after = deadline + Duration::from_millis(1);
        assert!(cam.frame(after).is_none(), "erased on the deadline");
        assert!(
            cam.static_deadline(after).is_none(),
            "and the clock disarms — no residual wake train"
        );
    }

    /// Full motion must NOT expose a static deadline: the two scheduler lanes
    /// are mutually exclusive, and a full-motion cameo that also handed out an
    /// erase deadline would let the host park the cadence mid-animation.
    #[test]
    fn full_motion_exposes_no_static_deadline() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        cam.summon(t0, (1, 1), look(), None);
        assert!(cam.is_active(t0), "precondition: it IS animating");
        assert!(cam.static_deadline(t0).is_none());
    }

    /// A second typed word replaces the first in place — one toy, at the newest
    /// place, with a restarted clock.
    #[test]
    fn a_second_summon_replaces_rather_than_stacks() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        cam.summon(t0, (4, 9), look(), None);
        let late = t0 + Duration::from_millis(CAMEO_RISE_MS + CAMEO_DWELL_MS);
        assert_eq!(
            cam.frame(late).map(|f| f.anchor),
            Some((4, 9)),
            "precondition: the first cameo is still live and fading"
        );
        cam.summon(late, (7, 2), look(), None);
        let f = cam.frame(late).expect("the replacement draws at once");
        assert_eq!(f.anchor, (7, 2), "the newest word owns the toy");
        assert!(f.dy > 0.0, "and its entrance restarts from the hidden side");
        // The replacement outlives the ORIGINAL's expiry: the clock restarted.
        assert!(
            cam.frame(t0 + Duration::from_millis(CAMEO_TOTAL_MS + 1))
                .is_some()
        );
    }

    /// ONE SCOPE, ASKED TWICE. A cameo summoned in pane 1 must answer
    /// identically to "may I draw you here?" ([`KittyCameo::frame_in_pane`],
    /// via [`KittyCameo::live_pane`] on the host side) and "are you vetoing
    /// here?" ([`KittyCameo::veto_cell`]) — otherwise a split can emit the toy
    /// in one pane while suppressing the ambient cat in another.
    ///
    /// SKEPTIC'S FINDING, 2026-08-09: the host asked the first question about
    /// the FOCUSED pane and the second about the SUMMONING pane, so the two
    /// disagreed the moment focus moved. The scope itself was always one
    /// question; this pins that it stays one, and that it really discriminates
    /// (pane 2 is refused on BOTH).
    #[test]
    fn emission_and_veto_scope_to_the_same_pane() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        cam.summon(t0, (5, 11), look(), Some(1));

        assert_eq!(
            cam.live_pane(t0),
            Some(Some(1)),
            "the toy names the pane its word was typed in"
        );
        assert!(
            cam.frame_in_pane(t0, Some(1)).is_some() && cam.veto_cell(t0, Some(1)).is_some(),
            "PRECONDITION: in its OWN pane both answers are yes"
        );
        assert!(
            cam.frame_in_pane(t0, Some(2)).is_none() && cam.veto_cell(t0, Some(2)).is_none(),
            "and in the pane next door both are no — one scope, not two"
        );
        // `None` IS A WILDCARD, pinned here as the deliberate asymmetry it is
        // rather than left as folklore: an unbound host names no grid, so it
        // matches whatever the toy carries. It is what lets the engine's own
        // ambient scan keep vetoing in an unsplit window (`self.bound` is
        // `None` there) — and it is also the loaded gun the single-pane
        // RENDERER fired by passing `None` for convenience, which teleported a
        // tab A toy onto tab B (skeptic's finding, 2026-08-09). That renderer
        // now names its front session; this assertion documents the wildcard
        // that is still there for callers who genuinely have no session.
        assert!(
            cam.frame_in_pane(t0, None).is_some() && cam.veto_cell(t0, None).is_some(),
            "a host that binds no pane sees the toy either way"
        );
        // And the scope retires with the toy: `live_pane` is not a standing
        // claim on a pane, it is a property of a sprite that is on glass.
        assert_eq!(
            cam.live_pane(t0 + Duration::from_millis(CAMEO_TOTAL_MS + 1)),
            None
        );
    }

    /// A CAMEO NOBODY CAN SEE OWNS NO CLOCK (skeptic's second-round finding,
    /// 2026-08-09). `WordDecorations` is per WINDOW and a tab switch retires
    /// nothing, so a toy typed in tab A stays live while the user works in tab
    /// B — where `frame_in_pane` correctly draws nothing. Both scheduler lanes
    /// used to stay armed anyway: full motion held 60 fps for the whole
    /// [`CAMEO_TOTAL_MS`], and reduced motion was WORSE, because an undrawn
    /// cameo re-arms the bake retry every [`CAMEO_BAKE_RETRY_MS`] — a ~62 fps
    /// wake train in the mode whose entire contract is "no cadence".
    #[test]
    fn a_hidden_cameo_arms_neither_scheduler_lane() {
        let t0 = Instant::now();
        let visible_a = |session: u64| session == 1;

        let mut cam = KittyCameo::default();
        cam.summon(t0, (4, 9), look(), Some(1));
        assert!(
            cam.is_active(t0) && cam.is_active_in(t0, visible_a),
            "PRECONDITION: in its own tab the toy owns the frame cadence"
        );
        // The user switches to another tab. Nothing retires the cameo; the
        // only thing that changes is which session is on glass.
        assert!(
            cam.frame_in_pane(t0, Some(2)).is_none(),
            "PRECONDITION: it is un-drawable there — that is the whole point"
        );
        assert!(
            cam.is_active(t0),
            "PRECONDITION: it is still ANIMATING; visibility is the host's question"
        );
        assert!(
            !cam.is_active_in(t0, |session| session == 2),
            "…so the window must not hold 60 fps for it"
        );

        // The reduced-motion lane, same toy, same tab switch.
        let mut cam = KittyCameo::default();
        cam.set_reduced_motion(true);
        cam.summon(t0, (4, 9), look(), Some(1));
        assert_eq!(
            cam.static_deadline_in(t0, visible_a),
            Some(t0 + Duration::from_millis(CAMEO_BAKE_RETRY_MS)),
            "PRECONDITION: visible and not yet drawn ⇒ the bake retry, which is \
             the arm that repeats"
        );
        assert_eq!(
            cam.static_deadline_in(t0, |session| session == 2),
            None,
            "hidden ⇒ no retry train and no erase wake: nothing drew it, so \
             nothing has to erase it"
        );
        // Coming back inside its life restores both — this is a scope, not a
        // deletion.
        let mid = t0 + Duration::from_millis(CAMEO_STATIC_HOLD_MS / 2);
        assert!(cam.static_deadline_in(mid, visible_a).is_some());
    }

    /// A cameo with no session is a host that binds no pane: one grid, always
    /// visible, whatever the predicate says about other sessions.
    #[test]
    fn a_paneless_cameo_is_always_visible_to_the_scheduler() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        cam.summon(t0, (0, 0), look(), None);
        assert!(
            cam.is_active_in(t0, |_| false),
            "an unbound host's toy cannot be hidden by a session filter"
        );
    }

    /// ONE PALETTE PER TOY, LATCHED AT THE FIRST DRAWN FRAME.
    ///
    /// SKEPTIC'S FINDING, 2026-08-09: the composed host samples the local text
    /// colours out of one grid scratch shared by every pane, so only the frame
    /// on which it deliberately extracts the OWNING pane is trustworthy. The
    /// latch is what turns "get it right every frame for five seconds" into
    /// "get it right once" — and it also keeps the palette out of the bake key
    /// churn, since the colours are part of that key.
    #[test]
    fn the_palette_latches_at_the_first_drawn_frame() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        assert!(
            !cam.wants_colors(),
            "no cameo, nothing to sample"
        );
        cam.summon(t0, (4, 9), look(), Some(1));
        assert!(cam.wants_colors(), "a fresh toy has no palette yet");

        let mine = CatColorKey {
            accent: 3,
            background: 1,
        };
        let someone_elses = CatColorKey {
            accent: 9,
            background: 3,
        };
        assert_eq!(cam.latch_colors(mine), mine, "the first sample is taken");
        assert!(
            !cam.wants_colors(),
            "and the host is told to stop sampling"
        );
        assert_eq!(
            cam.latch_colors(someone_elses),
            mine,
            "a later frame's sample — the neighbouring pane's grid — must not \
             re-tint a toy that is already on glass"
        );

        // A NEW toy is a new sample: the latch is per cameo, not per engine.
        cam.summon(t0 + Duration::from_millis(10), (7, 2), look(), Some(1));
        assert!(cam.wants_colors(), "the replacement wears its own place's colours");
        assert_eq!(cam.latch_colors(someone_elses), someone_elses);
    }

    /// The veto cell tracks the cameo's life exactly: it exists while the cat
    /// is on glass and vanishes with it, so the ambient scanner reclaims the
    /// word the moment the toy is gone.
    #[test]
    fn the_veto_cell_lives_exactly_as_long_as_the_cameo() {
        let t0 = Instant::now();
        let mut cam = KittyCameo::default();
        assert_eq!(cam.veto_cell(t0, None), None);
        cam.summon(t0, (5, 11), look(), None);
        assert_eq!(cam.veto_cell(t0, None), Some((5, 11)));
        assert_eq!(
            cam.veto_cell(t0 + Duration::from_millis(CAMEO_TOTAL_MS + 1), None),
            None
        );
    }
}
