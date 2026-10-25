// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cat that flies in front of the cursor on a rainbow (the [`crate::cursor_glow`] `rainbow kitty` ribbon is the wake).

use std::time::Duration;

use aterm_time::Instant;

use aterm_render::{GlowQuad, HaloMode, RainHalo, premul_rgb};

use crate::cat_baker::{CatColorKey, EyesFrame};
use crate::cat_glyphs_gen::CatGlyphId;
use crate::cursor_glow::{Geom, InkRole, push_halo_rows};
use crate::effect_util::{push_fx_rect, twinkle_rgb};
use crate::kitty_registry::KittyLook;
use crate::rainbow_kitty::companion::{FLYING_LEAD_MAX, Flight};
use crate::typing_momentum::TypingMomentum;

/// Stable atlas key for a user kitty sprite tile, which the host bakes into the shared free atlas via `CatBaker::host_tile`.
pub const HOST_ID: u64 = 0x6E79_616E;

const CAT_BAND: f32 = 0.65;
const CAT_DWELL: f32 = 1.2;
const SUSTAIN_CREDIT_MAX: f32 = 0.25;
const SUSTAIN_LEAK: f32 = 1.5;
/// Minimum forward keystrokes in the current high-band run before the cat can summon.
pub const MIN_RUN_KEYS: u32 = 16;
const LOW: f32 = 0.14;
const REVIVE_GATE: f32 = 0.35;
const FADE_IN: f32 = 0.35;
const FADE_OUT: f32 = 0.75;
const FADE_OUT_DUR: Duration = Duration::from_millis(750);

const EXIT_FACE_FADE: f32 = 0.25;
/// Total wall-clock time of the classic cursor kitty's off-glass line-fold seam.
pub const CURSOR_FOLD_DURATION: Duration = Duration::from_millis(280);
const EXIT_STAR_SCINT: f32 = 13.0;
const _: () = assert!(
    EXIT_STAR_SCINT > 0.0
        && EXIT_STAR_SCINT / FADE_OUT / std::f32::consts::TAU
            <= crate::effect_util::TWINKLE_FLASH_BOUND_HZ,
    "the exit star wink must glint within the photosensitivity flash bound"
);
const fn dur_from_secs_f32(secs: f32) -> Duration {
    Duration::from_nanos((secs as f64 * 1_000_000_000.0).round() as u64)
}

const DISCOVERY_HOLD: f32 = 2.8;
const DISCOVERY_HOLD_DUR: Duration = dur_from_secs_f32(DISCOVERY_HOLD);
const CELEBRATE_HOLD: f32 = 0.7;
const CELEBRATE_HOLD_DUR: Duration = dur_from_secs_f32(CELEBRATE_HOLD);

const OOPS_HOLD: f32 = 0.60;
const OOPS_HOLD_DUR: Duration = dur_from_secs_f32(OOPS_HOLD);
const OOPS_REARM: f32 = 0.30;
const OOPS_RECOIL: f32 = 0.12;
const OOPS_SQUASH: f32 = 0.07;
const OOPS_ATTACK: f32 = 0.07;
const OOPS_SETTLE: f32 = 0.18;

const WINCE_HOLD: f32 = 0.52;
const WINCE_HOLD_DUR: Duration = dur_from_secs_f32(WINCE_HOLD);
const WINCE_CHAIN_WINDOW: f32 = 0.85;
const WINCE_ATTACK: f32 = 0.045;
const WINCE_SETTLE: f32 = 0.16;
const WINCE_RECOIL: f32 = 0.16;
const WINCE_SQUASH: f32 = 0.11;
const MAX_WINCE_CHAIN: u8 = 4;
const WINCE_TUMBLE: f32 = 0.13;
const WINCE_TUMBLE_FREQ: f32 = 3.1;
const WINCE_TUMBLE_DECAY: f32 = 3.4;
const WINCE_TUMBLE_OMEGA: [f32; MAX_WINCE_CHAIN as usize] = {
    let mut out = [0.0f32; MAX_WINCE_CHAIN as usize];
    let mut i = 0;
    while i < out.len() {
        out[i] =
            std::f32::consts::TAU * WINCE_TUMBLE_FREQ * crate::color_math::sqrt_f32((i + 1) as f32);
        i += 1;
    }
    out
};

const DELIGHT_HOLD: f32 = 0.62;
const DELIGHT_HOLD_DUR: Duration = dur_from_secs_f32(DELIGHT_HOLD);
const DELIGHT_CHAIN_WINDOW: f32 = 0.9;
const DELIGHT_ATTACK: f32 = 0.05;
const DELIGHT_SETTLE: f32 = 0.2;
const DELIGHT_STRETCH: f32 = 0.14;
const MAX_DELIGHT_CHAIN: u8 = 4;

const DISP_TAU: f32 = 0.13;
const BANK_LO: f32 = 0.32;
const BANK_HI: f32 = 0.96;
const STRETCH_X: f32 = 0.17;
const THIN_Y: f32 = 0.10;
const LEAD_MAX: f32 = 0.22;
const HAPPY_GATE: f32 = 0.80;
const BLINK_PERIOD: f32 = 1.7;
const BLINK_DUR: f32 = 0.13;
/// The hover bob's amplitude, a fraction of cell height; the footprint keeps this much headroom on the top rows.
pub(crate) const BOB_AMP: f32 = 0.09;
const STRIDE_PER_KEY: f32 = 0.5;
const STRIDE_IDLE_HZ: f32 = 1.4;
const STRIDE_TAU: f32 = 0.05;
const STRIDE_RATE_MAX: f32 = 5.5;
const STRIDE_SQUASH: f32 = 0.05;
const LAND_DUR: f32 = 0.42;
const LAND_AMP: f32 = 0.26;
const LAND_FREQ: f32 = 1.35;
const LAND_DECAY: f32 = 3.0;

const SING_SQUASH: f32 = 0.14;
const SING_SWAY: f32 = 0.12;

/// One frame's sing-along sync from the host: the drive/beat pair the dance rides, shared by both render paths.
#[derive(Clone, Copy, Debug)]
pub struct SingSync {
    /// Celebration drive 0..=1 (1 armed, easing through the wind-down).
    pub drive: f32,
    /// Beat phase in beats since the arm (fractional).
    pub beat: f32,
}

impl SingSync {
    /// No active sing-along drive or beat phase.
    pub const NONE: Self = Self {
        drive: 0.0,
        beat: 0.0,
    };
}

impl Default for SingSync {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

/// Smoothstep ramp of `x` across `[lo, lo + 1 / inv_span]` → `0..=1` (eased at both ends).
#[inline]
const fn smoothstep(lo: f32, inv_span: f32, x: f32) -> f32 {
    let t = ((x - lo) * inv_span).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Smoothstep ramp of `x` across `[0, 1 / inv_hi]` → `0..=1` (eased at both ends).
#[inline]
const fn smoothstep_0(inv_hi: f32, x: f32) -> f32 {
    let t = (x * inv_hi).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The living-cartoon pose handed to the host each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CatPose {
    /// Dest-width multiplier about the sprite centre (`>1` stretch, `<1` squash).
    pub scale_x: f32,
    /// Dest-height multiplier about the sprite centre.
    pub scale_y: f32,
    /// Forward lean as a fraction of cell width (`+` = ahead of the rest anchor).
    pub lead: f32,
    /// Baked blink/squint frame for this present.
    pub eyes: EyesFrame,
}

/// Direction of a positively authenticated classic cursor-kitty line fold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatFoldDirection {
    Forward,
    Reverse,
}

/// The wall-clock fold sample handed to the pixel placement layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CatFoldFrame {
    pub started: Instant,
    pub direction: CatFoldDirection,
    /// Linear whole-seam progress, `0..1`.
    pub progress: f32,
}

/// CursorCat's complete placement-side verdict for one rendered frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorCatPlacementFrame {
    /// The same injected frame clock used to sample the lifecycle.
    pub sampled_at: Instant,
    pub fold: Option<CatFoldFrame>,
    /// Persistent facing outside a fold.
    pub facing_left: bool,
}

#[derive(Clone, Copy, Debug)]
struct CatFold {
    started: Instant,
    direction: CatFoldDirection,
}

impl CatPose {
    /// The neutral pose: natural size, no lean, eyes open.
    pub const STILL: CatPose = CatPose {
        scale_x: 1.0,
        scale_y: 1.0,
        lead: 0.0,
        eyes: EyesFrame::Open,
    };
}

impl Default for CatPose {
    #[inline]
    fn default() -> Self {
        Self::STILL
    }
}

/// The flourish the cat does on its way out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatExit {
    Plain,
    StarWink,
    HeartMeow,
}

/// One-shot expression layered over the collected look.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatReaction {
    Cruise,
    /// Enter/Return while the companion is already flying.
    Celebrate,
    /// A delete while flying (Backspace or any kill chord).
    Startled,
    /// A complete profanity cue: squeezed eyes and a burst-scaled recoil.
    Wince,
    /// A complete feline word was typed (`kitty`, `gato`, `neko`, …; see [`crate::kitty_summon`]).
    Delight,
    /// The collection hello's guaranteed hold ([`CursorCat::on_collect`]).
    Discovery,
}

#[derive(Clone, Copy)]
enum State {
    Hidden,
    FadeIn(Instant),
    Shown,
    FadeOut(Instant),
}

/// One frame's resolved cat state for the host.
#[derive(Clone, Copy, Debug)]
pub struct CatFrame {
    /// Opacity 0..=255 (0 ⇒ nothing to draw).
    pub alpha: u8,
    /// The exit flourish (meaningful only while fading out).
    pub exit: CatExit,
    /// Fade-out progress 0..1 (0 otherwise) — drives the exit fx animation.
    pub fade_out: f32,
    /// Collected visual identity to bake for this frame.
    pub look: KittyLook,
    /// Current bounded reaction.
    pub reaction: CatReaction,
    /// True during the guaranteed discovery hold.
    pub discovery: bool,
    /// True for the whole collection hello including its fade.
    pub collection_hello: bool,
    /// Signed hover-bob as a fraction of cell height (±`BOB_AMP`), so the flying cat reads as airborne.
    pub bob: f32,
    /// The living-cartoon pose for this present: squash/stretch, forward lean, and the baked eye frame.
    pub pose: CatPose,
    /// Sing-along drive 0..=1 (`crate::kitty_sing`), easing to 0 through the wind-down.
    pub sing: f32,
}

const MARKED_GLYPHS: [bool; crate::cat_glyphs_gen::GLYPHS.len()] = {
    let mut out = [false; crate::cat_glyphs_gen::GLYPHS.len()];
    let mut g = 0;
    while g < out.len() {
        let layers = crate::cat_glyphs_gen::GLYPHS[g].layers;
        let mut i = 0;
        while i < layers.len() {
            if matches!(
                layers[i].role,
                crate::cat_glyphs_gen::GlyphRole::Pattern
                    | crate::cat_glyphs_gen::GlyphRole::CoatShade
            ) {
                out[g] = true;
                break;
            }
            i += 1;
        }
        g += 1;
    }
    out
};

/// Whether `variant`'s art carries markings (pattern or coat-shade layers) a
/// face swap would drop.
#[inline]
const fn marked(variant: CatGlyphId) -> bool {
    MARKED_GLYPHS[variant as usize]
}

impl CatFrame {
    /// Resolve the authored art for this expression (typed glyph ids, so roster additions cannot shift it).
    #[must_use]
    #[inline]
    pub const fn render_look(&self) -> KittyLook {
        // The goodbye keeps the cat's markings: the flourish faces carry no
        // pattern layers, so a marked cat (a calico patch, an eye patch)
        // waves goodbye in its own head and the flourish rides beside it.
        let flourish_face_earned = self.fade_out >= EXIT_FACE_FADE && !marked(self.look.variant);
        let variant = match (self.exit, self.reaction) {
            (_, CatReaction::Startled) => CatGlyphId::S121,
            (CatExit::StarWink, _) if flourish_face_earned => CatGlyphId::S121,
            (CatExit::HeartMeow, _) if flourish_face_earned => CatGlyphId::S115,
            _ if crate::companion::sing_face_live(self.sing) => CatGlyphId::S115,
            (_, CatReaction::Celebrate) => CatGlyphId::SpecManeki,
            _ => return self.look,
        };
        KittyLook {
            variant,
            accessory: None,
            ..self.look
        }
        .normalized()
    }

    /// Quantized fingerprint folded into the frame's repaint key so the fade + bob + exit fx invalidate frames while active and settle (0) when hidden.
    #[must_use]
    #[inline]
    pub const fn fp(&self) -> u64 {
        if self.alpha == 0 {
            return 0;
        }
        let e = match self.exit {
            CatExit::Plain => 0u64,
            CatExit::StarWink => 1,
            CatExit::HeartMeow => 2,
        };
        let reaction = match self.reaction {
            CatReaction::Cruise => 0u64,
            CatReaction::Celebrate => 1,
            CatReaction::Startled => 2,
            CatReaction::Discovery => 3,
            CatReaction::Wince => 4,
            CatReaction::Delight => 5,
        };
        let bob_q = ((self.bob * 512.0) as i64) as u64;
        let eyes_q = match self.pose.eyes {
            EyesFrame::Open => 0u64,
            EyesFrame::Happy => 1,
            EyesFrame::Blink => 2,
        };
        let sx_q = ((self.pose.scale_x * 256.0) as i64) as u64;
        let sy_q = ((self.pose.scale_y * 256.0) as i64) as u64;
        let lead_q = ((self.pose.lead * 512.0) as i64) as u64;
        let sing_q = ((self.sing * 128.0) as i64) as u64;
        let acc = match self.look.accessory {
            Some(a) => a as u64 + 1,
            None => 0,
        };
        let values = [
            self.alpha as u64,
            (self.fade_out * 255.0) as u64,
            e,
            reaction,
            self.discovery as u64,
            self.collection_hello as u64,
            self.look.variant as u64,
            acc,
            self.look.coat as u64,
            self.look.iris as u64,
            self.look.age as u64,
            bob_q,
            eyes_q,
            sx_q,
            sy_q,
            lead_q,
            sing_q,
        ];
        let mut h = aterm_hash::FNV64_OFFSET;
        let mut i = 0;
        while i < values.len() {
            h = aterm_hash::fnv_mix_u64(h, values[i]);
            i += 1;
        }
        h
    }
}

/// The inert hidden/retired frame: alpha 0, plain exit, no reaction, STILL
/// pose — byte-identical across frames, so [`CatFrame::fp`] settles to 0.
#[inline]
const fn hidden_frame(look: KittyLook) -> CatFrame {
    CatFrame {
        alpha: 0,
        exit: CatExit::Plain,
        fade_out: 0.0,
        look,
        reaction: CatReaction::Cruise,
        discovery: false,
        collection_hello: false,
        bob: 0.0,
        sing: 0.0,
        pose: CatPose::STILL,
    }
}

/// The earned cat's decision layer: the canonical typing-momentum metric, a high-band dwell clock and a fade/exit state machine.
pub struct CursorCat {
    /// The canonical typing-momentum metric, run on the same keystream as the
    /// glow engine's (`momentum_unifies_glow_and_cat_metrics` pins them equal).
    momentum: TypingMomentum,
    /// Seconds of high-band dwell accrued across in-rhythm keys; leaks
    /// ([`SUSTAIN_LEAK`]) while keys land below [`CAT_BAND`].
    sustain: f32,
    /// Forward keys in the current uninterrupted high-band run: the
    /// [`MIN_RUN_KEYS`] travel floor.
    run_keys: u32,
    last: Option<Instant>,
    state: State,
    exit: CatExit,
    /// When the current flight began (FadeIn entry) — the hover-bob clock.
    flight: Option<Instant>,
    /// Stride pump phase, in cycles (see [`STRIDE_PER_KEY`]): advanced per
    /// committed forward keystroke, drifting at [`STRIDE_IDLE_HZ`] between.
    stride: f32,
    /// The last stride advance (the idle-drift `dt` source). `None` re-seeds without
    /// integrating, so a resumed or foreign clock cannot fling the phase forward.
    stride_at: Option<Instant>,
    /// The stride phase the pose draws (the bob, the footfall squash),
    /// following [`Self::stride`] at [`STRIDE_TAU`], never faster than
    /// [`STRIDE_RATE_MAX`].
    stride_shown: f32,
    /// The collected identity currently accompanying the cursor.
    look: KittyLook,
    /// A look synced while on screen, applied at the next appearance so one appearance
    /// wears one cat ([`Self::set_look`]).
    pending_look: Option<KittyLook>,
    reaction: CatReaction,
    reaction_until: Option<Instant>,
    /// A discovery cannot be grounded by low momentum before this deadline.
    discovery_until: Option<Instant>,
    /// The full collection presentation stays drawable through fade-out.
    collection_hello: bool,
    /// Start of a paused (unfocused/suppressed) collection hello. The hold counts drawable
    /// time, so resume shifts every presentation clock by the paused span.
    collection_paused_at: Option<Instant>,
    /// Terminal colour context frozen for this flight/hello: sampled on the first visible
    /// frame and reused, so text churn cannot fragment the atlas cache.
    colors: Option<CatColorKey>,
    /// Eased "display momentum" spine (0..1): a lagged follower of the live
    /// score that drives every pose choice, giving the body follow-through.
    disp: f32,
    /// Last spine advance instant. `None` reseeds the spine to its target, so a
    /// fresh flight starts banked, not from zero.
    disp_at: Option<Instant>,
    /// Start of a landing squash→stretch bounce (Enter stomp, a delete
    /// flinch, or the fade-out touchdown).
    land_at: Option<Instant>,
    /// The meteor the head is flying ([`Self::on_meteor`]): its whip and squint
    /// ride the pose until [`Flight::settled_at`].
    meteor: Option<Flight>,
    /// The current oops burst's kick instant, the recoil envelope's attack
    /// clock.
    oops_at: Option<Instant>,
    /// The last delete of the current burst: the [`OOPS_REARM`] gap decides
    /// between extending this burst and re-kicking a new one.
    oops_last: Option<Instant>,
    /// First sample of the current curse-wince kick.
    wince_at: Option<Instant>,
    /// Last complete curse in the current short phrase.
    wince_last: Option<Instant>,
    /// Burst-scaled phrase strength/pulse count, bounded at [`MAX_WINCE_CHAIN`].
    wince_chain: u8,
    /// First sample of the current feline-delight kick (the leap clock).
    delight_at: Option<Instant>,
    /// Last complete feline word in the current short phrase.
    delight_last: Option<Instant>,
    /// Burst-scaled hop strength, bounded at [`MAX_DELIGHT_CHAIN`].
    delight_chain: u8,
    /// Sing-along drive 0..=1, synced by the host each frame
    /// ([`Self::set_singing`]) from the `kitty_sing` detector.
    sing: f32,
    /// The shared dance-beat phase in beats (fractional), from the same
    /// host sync.
    sing_beat: f32,
    /// The authenticated line-fold episode: CursorGlow classifies and clocks it,
    /// WordDecorations resolves it into pixels.
    fold: Option<CatFold>,
    /// Resting direction after a reverse fold. An active fold's own direction
    /// wins in [`Self::placement_frame`].
    facing_left: bool,
    rng: u32,
}

impl Default for CursorCat {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CursorCat {
    /// Construct a fresh cursor-kitty state machine in its resting hidden state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            momentum: TypingMomentum::new(),
            sustain: 0.0,
            run_keys: 0,
            last: None,
            state: State::Hidden,
            exit: CatExit::Plain,
            flight: None,
            stride: 0.0,
            stride_at: None,
            stride_shown: 0.0,
            look: KittyLook {
                variant: CatGlyphId::SpecStretch,
                ..KittyLook::DEFAULT
            },
            pending_look: None,
            reaction: CatReaction::Cruise,
            reaction_until: None,
            discovery_until: None,
            collection_hello: false,
            collection_paused_at: None,
            colors: None,
            disp: 0.0,
            disp_at: None,
            land_at: None,
            meteor: None,
            oops_at: None,
            oops_last: None,
            wince_at: None,
            wince_last: None,
            wince_chain: 0,
            delight_at: None,
            delight_last: None,
            delight_chain: 0,
            sing: 0.0,
            sing_beat: 0.0,
            fold: None,
            facing_left: false,
            rng: 0x2545_F491,
        }
    }

    /// Retire an ordinary cursor-following flight when the caret it follows stops being one coordinate.
    #[inline]
    pub const fn retire_unowned_cursor_motion(&mut self) {
        if self.collection_hello || self.sing > 0.0 || matches!(self.state, State::Hidden) {
            return;
        }
        self.momentum.reset();
        self.sustain = 0.0;
        self.run_keys = 0;
        self.last = None;
        self.exit = CatExit::Plain;
        self.stride = 0.0;
        self.settle_hidden();
    }

    /// Consume the classifier verdict produced by CursorGlow.
    #[inline]
    pub fn on_motion_pulse(&mut self, pulse: crate::cursor_glow::CursorCatMotionPulse) {
        self.on_motion_pulse_with_owner(pulse, true);
    }

    /// Feed the momentum and travel count the tenure pet mode's singing face needs without starting an ordinary flight.
    #[inline]
    pub fn on_pet_mode_motion_pulse(&mut self, pulse: crate::cursor_glow::CursorCatMotionPulse) {
        self.on_motion_pulse_with_owner(pulse, false);
    }

    #[inline]
    fn on_motion_pulse_with_owner(
        &mut self,
        pulse: crate::cursor_glow::CursorCatMotionPulse,
        ordinary_flight_owned: bool,
    ) {
        use crate::cursor_glow::CursorCatMotionKind;

        match pulse.kind {
            CursorCatMotionKind::Advance => {
                self.on_key_with_owner(pulse.at, true, ordinary_flight_owned);
                self.facing_left = false;
            }
            CursorCatMotionKind::FoldForward => {
                self.on_key_with_owner(pulse.at, true, ordinary_flight_owned);
                self.facing_left = false;
                if self.is_active() && (ordinary_flight_owned || self.sing > 0.0) {
                    self.fold = Some(CatFold {
                        started: pulse.at,
                        direction: CatFoldDirection::Forward,
                    });
                }
            }
            CursorCatMotionKind::FoldReverse => {
                self.facing_left = true;
                if self.is_active() && (ordinary_flight_owned || self.sing > 0.0) {
                    self.fold = Some(CatFold {
                        started: pulse.at,
                        direction: CatFoldDirection::Reverse,
                    });
                }
            }
        }
    }

    /// Sample the cat's geometry-free placement state.
    #[must_use]
    #[inline]
    pub fn placement_frame(&self, now: Instant) -> CursorCatPlacementFrame {
        let fold_now = self.collection_sample_time(now);
        let fold = self.fold.and_then(|fold| {
            const INV_CURSOR_FOLD_SECS: f32 = 1.0 / CURSOR_FOLD_DURATION.as_secs_f32();
            let progress = fold_now
                .saturating_duration_since(fold.started)
                .as_secs_f32()
                * INV_CURSOR_FOLD_SECS;
            (progress < 1.0).then_some(CatFoldFrame {
                started: fold.started,
                direction: fold.direction,
                progress: progress.clamp(0.0, 1.0),
            })
        });
        CursorCatPlacementFrame {
            sampled_at: now,
            fold,
            facing_left: fold.map_or(self.facing_left, |active| {
                matches!(active.direction, CatFoldDirection::Reverse)
            }),
        }
    }

    /// Retire renderer-coordinate continuity only: drop an in-flight fold and the palette sampled under the old footprint.
    #[inline]
    pub const fn rebase_placement(&mut self) {
        self.colors = None;
        self.fold = None;
        self.facing_left = false;
    }

    /// Stamp one committed text keystroke.
    #[inline]
    pub fn on_key(&mut self, now: Instant, forward: bool) {
        self.on_key_with_owner(now, forward, true);
    }

    #[inline]
    fn on_key_with_owner(&mut self, now: Instant, forward: bool, ordinary_flight_owned: bool) {
        let dt = self
            .last
            .map(|l| now.saturating_duration_since(l).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.5);
        if forward {
            let held = self.momentum.value(now);
            if held >= CAT_BAND {
                self.sustain += dt.min(SUSTAIN_CREDIT_MAX);
                self.run_keys = self.run_keys.saturating_add(1);
            } else {
                const INV_SUSTAIN_CREDIT_MAX: f32 = 1.0 / SUSTAIN_CREDIT_MAX;
                let drain = dt * SUSTAIN_LEAK;
                self.sustain = (self.sustain - drain).max(0.0);
                let lost = (drain * INV_SUSTAIN_CREDIT_MAX) as u32;
                self.run_keys = self.run_keys.saturating_sub(lost);
            }
            self.stride_advance(now);
            let next_stride = self.stride + STRIDE_PER_KEY;
            self.stride = if next_stride < 1024.0 {
                next_stride
            } else {
                next_stride.rem_euclid(1024.0)
            };
            self.momentum.advance(now);
            if let State::FadeOut(t0) = self.state
                && !self.collection_hello
                && (ordinary_flight_owned || self.sing > 0.0)
                && self.momentum.value(now) >= REVIVE_GATE
            {
                const INV_FADE_OUT: f32 = 1.0 / FADE_OUT;
                let faded =
                    (now.saturating_duration_since(t0).as_secs_f32() * INV_FADE_OUT).min(1.0);
                let alpha = 1.0 - faded;
                let back = Duration::from_secs_f32(FADE_IN * alpha);
                self.state = State::FadeIn(now.checked_sub(back).unwrap_or(now));
                self.exit = CatExit::Plain;
                if self.land_at == Some(t0) {
                    self.land_at = None;
                }
            }
        } else {
            self.sustain = 0.0;
            self.run_keys = 0;
            self.momentum.delete(now);
            self.note_oops(now);
        }
        self.last = Some(now);
        if ordinary_flight_owned
            && matches!(self.state, State::Hidden)
            && self.sustain >= CAT_DWELL
            && self.run_keys >= MIN_RUN_KEYS
        {
            self.summon(now);
        }
    }

    /// The Hidden → FadeIn summon shared by [`Self::on_key`] and [`Self::set_singing`] (each
    /// keeps its own gate); a latched look lands here.
    #[inline]
    const fn summon(&mut self, now: Instant) {
        self.sustain = 0.0;
        self.run_keys = 0;
        self.colors = None;
        self.fold = None;
        self.facing_left = false;
        if let Some(pending) = self.pending_look.take() {
            self.look = pending;
        }
        self.state = State::FadeIn(now);
        self.flight = Some(now);
    }

    /// Fire (or sustain) the delete oops on a live companion: every delete extends the hold
    /// to [`OOPS_HOLD`] past it; the flinch and recoil kick only on a burst's first delete
    /// ([`OOPS_REARM`]).
    #[inline]
    fn note_oops(&mut self, now: Instant) {
        if !self.is_active() {
            return;
        }
        let same_burst = self.reaction == CatReaction::Startled
            && self
                .oops_last
                .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() < OOPS_REARM);
        self.reaction = CatReaction::Startled;
        self.reaction_until = Some(now + OOPS_HOLD_DUR);
        self.oops_last = Some(now);
        if !same_burst {
            self.oops_at = Some(now);
            self.land_at = Some(now);
        }
    }

    /// Wince at complete profanity cues on a live companion; never summons, touches momentum or extends the flight.
    #[inline]
    pub fn on_curse(&mut self, now: Instant, hits: u8) -> bool {
        if hits == 0 || !self.is_active() {
            return false;
        }
        let chained = self.wince_last.is_some_and(|last| {
            now.saturating_duration_since(last).as_secs_f32() < WINCE_CHAIN_WINDOW
        });
        self.wince_chain = if chained {
            self.wince_chain.saturating_add(hits).min(MAX_WINCE_CHAIN)
        } else {
            hits.min(MAX_WINCE_CHAIN)
        };
        self.wince_at = Some(now);
        self.wince_last = Some(now);
        self.reaction = CatReaction::Wince;
        self.reaction_until = Some(now + WINCE_HOLD_DUR);
        self.land_at = Some(now);
        true
    }

    /// Fly the head's meteor (horizontal flights only).
    #[inline]
    pub fn on_meteor(&mut self, flight: Flight, now: Instant, frame: &mut CatFrame) {
        if !self.is_active() {
            return;
        }
        let now = self.collection_sample_time(now);
        self.meteor = Some(flight);
        self.disp = self.disp.max(flight.disp_floor());
        self.disp_at = Some(now);
        self.land_at = Some(flight.land_at);
        frame.pose = self.resolve_pose(now, self.decayed(now), frame.reaction, frame.exit);
    }

    /// Refresh an animated [`CatFrame`] that predates an in-frame reaction impulse (such as Rainbow Kitty v2's `Delight` in `route_flying_head`), so frame 0 of the reaction carries the new [`CatReaction`] and eyes.
    #[inline]
    pub(crate) fn refresh_reaction_frame(&mut self, now: Instant, frame: &mut CatFrame) {
        if !self.is_active() {
            return;
        }
        let now = self.collection_sample_time(now);
        let reaction = self.reaction_at(now);
        frame.reaction = reaction;
        frame.pose = self.resolve_pose(now, self.decayed(now), reaction, frame.exit);
    }

    /// The display spine (0..1) the pose banks on.
    #[cfg(test)]
    #[must_use]
    pub const fn spine(&self) -> f32 {
        self.disp
    }

    /// The instant the landing squash is booked for, if pending or playing.
    #[cfg(test)]
    #[must_use]
    pub const fn land_at(&self) -> Option<Instant> {
        self.land_at
    }

    /// Light up at one or more complete feline words: the twin of [`Self::on_curse`] with happy eyes and a leap.
    #[inline]
    pub fn on_delight(&mut self, now: Instant, hits: u8) -> bool {
        if hits == 0 || !self.is_active() {
            return false;
        }
        let chained = self.delight_last.is_some_and(|last| {
            now.saturating_duration_since(last).as_secs_f32() < DELIGHT_CHAIN_WINDOW
        });
        self.delight_chain = if chained {
            self.delight_chain
                .saturating_add(hits)
                .min(MAX_DELIGHT_CHAIN)
        } else {
            hits.min(MAX_DELIGHT_CHAIN)
        };
        self.delight_at = Some(now);
        self.delight_last = Some(now);
        self.reaction = CatReaction::Delight;
        self.reaction_until = Some(now + DELIGHT_HOLD_DUR);
        true
    }

    /// Stamp one kill chord (Ctrl-K/U/W, Alt-D, forward Delete, word backspace).
    #[inline]
    pub fn on_kill(&mut self, now: Instant) {
        self.sustain = 0.0;
        self.run_keys = 0;
        self.momentum.kill(now);
        self.last = Some(now);
        self.note_oops(now);
    }

    /// Celebrate a committed command without summoning an otherwise-hidden companion.
    #[inline]
    pub fn on_enter(&mut self, now: Instant) {
        if self.is_active() {
            self.reaction = CatReaction::Celebrate;
            self.reaction_until = Some(now + CELEBRATE_HOLD_DUR);
            self.land_at = Some(now);
        }
    }

    /// Host sync for the sing-along (`crate::kitty_sing`), once per frame before [`Self::frame`]/[`Self::static_frame`].
    #[inline]
    pub const fn set_singing(&mut self, now: Instant, sync: SingSync) {
        self.sing = if sync.drive.is_finite() {
            sync.drive.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.sing_beat = if sync.beat.is_finite() {
            sync.beat
        } else {
            0.0
        };
        if self.sing < 1.0 {
            return;
        }
        self.momentum.set_value(now, 1.0);
        self.last = Some(now);
        if matches!(self.state, State::Hidden) && self.run_keys >= MIN_RUN_KEYS {
            self.summon(now);
        }
    }

    /// Install the companion's identity (the host's per-frame verdict: a pinned favourite, else the tenured program cat, else the launch kitty) without changing the lifecycle.
    #[inline]
    pub const fn set_look(&mut self, look: KittyLook) {
        let look = look.normalized();
        if self.is_active() || self.collection_hello {
            self.pending_look = if !look.eq_const(&self.look) {
                Some(look)
            } else {
                None
            };
        } else {
            self.look = look;
            self.pending_look = None;
        }
    }

    /// The hello for a cat the user chose (a favourite pin or `wear`).
    pub fn on_collect(&mut self, now: Instant, look: KittyLook) {
        self.look = look.normalized();
        self.pending_look = None;
        self.momentum.set_value(now, 1.0);
        self.last = Some(now);
        self.sustain = 0.0;
        self.run_keys = 0;
        self.exit = CatExit::Plain;
        let until = now + DISCOVERY_HOLD_DUR;
        self.discovery_until = Some(until);
        self.collection_hello = true;
        self.collection_paused_at = None;
        self.colors = None;
        self.fold = None;
        self.facing_left = false;
        self.reaction = CatReaction::Discovery;
        self.reaction_until = Some(until);
        self.stride_at = None;
        self.stride_shown = self.stride;
        self.reset_spine();
        if matches!(self.state, State::Hidden | State::FadeOut(_)) {
            self.state = State::FadeIn(now);
            self.flight = Some(now);
        }
    }

    /// Pause or resume a collection hello at the host's drawability boundary.
    pub fn set_collection_presentable(&mut self, now: Instant, presentable: bool) {
        if !self.collection_hello {
            self.collection_paused_at = None;
            return;
        }
        if !presentable {
            self.collection_paused_at.get_or_insert(now);
            return;
        }
        let Some(paused_at) = self.collection_paused_at.take() else {
            return;
        };
        let hidden = now.saturating_duration_since(paused_at);
        self.discovery_until = self.discovery_until.map(|at| at + hidden);
        self.reaction_until = self.reaction_until.map(|at| at + hidden);
        self.wince_at = self.wince_at.map(|at| at + hidden);
        self.wince_last = self.wince_last.map(|at| at + hidden);
        self.delight_at = self.delight_at.map(|at| at + hidden);
        self.delight_last = self.delight_last.map(|at| at + hidden);
        self.oops_at = self.oops_at.map(|at| at + hidden);
        self.oops_last = self.oops_last.map(|at| at + hidden);
        self.land_at = self.land_at.map(|at| at + hidden);
        self.meteor = self.meteor.map(|m| Flight {
            t0: m.t0 + hidden,
            land_at: m.land_at + hidden,
            ..m
        });
        self.fold = self.fold.map(|f| CatFold {
            started: f.started + hidden,
            ..f
        });
        self.disp_at = self.disp_at.map(|at| at + hidden);
        self.stride_at = self.stride_at.map(|at| at + hidden);
        self.flight = self.flight.map(|at| at + hidden);
        self.state = match self.state {
            State::FadeIn(at) => State::FadeIn(at + hidden),
            State::FadeOut(at) => State::FadeOut(at + hidden),
            state => state,
        };
    }

    #[inline]
    const fn collection_sample_time(&self, now: Instant) -> Instant {
        if self.collection_hello {
            match self.collection_paused_at {
                Some(paused) => paused,
                None => now,
            }
        } else {
            now
        }
    }

    #[inline]
    fn discovery_active(&self, now: Instant) -> bool {
        self.discovery_until.is_some_and(|until| now < until)
    }

    #[inline]
    fn reaction_at(&mut self, now: Instant) -> CatReaction {
        if self.reaction_until.is_some_and(|until| now >= until) {
            self.reaction = CatReaction::Cruise;
            self.reaction_until = None;
            self.oops_at = None;
            self.oops_last = None;
            self.wince_at = None;
            self.delight_at = None;
        }
        self.reaction
    }

    /// Drift the stride phase to `now` at [`STRIDE_IDLE_HZ`]. `dt` is clamped
    /// so a backgrounded window resumes with one ordinary frame's worth.
    #[inline]
    fn stride_advance(&mut self, now: Instant) {
        if self.stride_at == Some(now) {
            return;
        }
        let dt = self
            .stride_at
            .map(|l| now.saturating_duration_since(l).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.10);
        self.stride_at = Some(now);
        if dt > 0.0 {
            let wrap = |x: f32| {
                if (0.0..1024.0).contains(&x) {
                    x
                } else {
                    x.rem_euclid(1024.0)
                }
            };
            self.stride = wrap(self.stride + dt * STRIDE_IDLE_HZ);
            let mut gap = wrap(self.stride - self.stride_shown + 512.0) - 512.0;
            if gap >= 1.0 {
                self.stride_shown = wrap(self.stride_shown + gap.floor());
                gap = gap.fract();
            }
            if gap != 0.0 {
                const INV_STRIDE_TAU: f32 = 1.0 / STRIDE_TAU;
                let k = 1.0 - (-dt * INV_STRIDE_TAU).exp();
                let step = (gap * k).clamp(-STRIDE_RATE_MAX * dt, STRIDE_RATE_MAX * dt);
                self.stride_shown = wrap(self.stride_shown + step);
            }
        }
    }

    /// The airborne hover-bob (fraction of cell height), a pure read of the stride pump.
    #[inline]
    const fn bob(&self) -> f32 {
        match self.flight {
            Some(_) => {
                crate::supernova::sin_cos_f32(std::f32::consts::TAU * self.stride_shown).0 * BOB_AMP
            }
            None => 0.0,
        }
    }

    /// Progress 0..1 through the delight swell, or 0 when none is live.
    #[cfg(test)]
    fn delight_arc(&self, now: Instant) -> f32 {
        if !matches!(self.reaction, CatReaction::Delight) {
            return 0.0;
        }
        let Some(t0) = self.delight_at else {
            return 0.0;
        };
        let elapsed = now.saturating_duration_since(t0).as_secs_f32();
        (elapsed * (1.0 / DELIGHT_HOLD)).clamp(0.0, 1.0)
    }

    /// Advance the eased display-momentum spine toward `target` (the live
    /// score) with time constant `DISP_TAU`.
    #[inline]
    fn advance_spine(&mut self, now: Instant, target: f32) {
        let target = target.clamp(0.0, 1.0);
        match self.disp_at {
            None => self.disp = target,
            Some(prev) => {
                let dt = now.saturating_duration_since(prev).as_secs_f32().min(0.1);
                if dt > 0.0 && self.disp != target {
                    const INV_DISP_TAU: f32 = 1.0 / DISP_TAU;
                    let alpha = 1.0 - (-dt * INV_DISP_TAU).exp();
                    self.disp += (target - self.disp) * alpha;
                }
            }
        }
        self.disp_at = Some(now);
    }

    /// Deterministic idle blink: a `BLINK_DUR` closure once per `BLINK_PERIOD`,
    /// phased off the flight clock so it never fires the instant the cat appears.
    #[inline]
    fn blink_active(&self, now: Instant) -> bool {
        match self.flight {
            Some(t0) => {
                const INV_BLINK_PERIOD: f32 = 1.0 / BLINK_PERIOD;
                const BLINK_DUTY: f32 = BLINK_DUR / BLINK_PERIOD;
                let t = now.saturating_duration_since(t0).as_secs_f32();
                let u = (t + (BLINK_PERIOD - BLINK_DUR)) * INV_BLINK_PERIOD;
                (u - u.floor()) < BLINK_DUTY
            }
            None => false,
        }
    }

    fn resolve_pose(
        &mut self,
        now: Instant,
        target: f32,
        reaction: CatReaction,
        exit: CatExit,
    ) -> CatPose {
        const INV_BANK_SPAN: f32 = 1.0 / (BANK_HI - BANK_LO);
        self.advance_spine(now, target);
        let bank = smoothstep(BANK_LO, INV_BANK_SPAN, self.disp);
        let mut scale_x = 1.0 + STRETCH_X * bank;
        let mut scale_y = 1.0 - THIN_Y * bank;
        let mut lead = LEAD_MAX * bank;
        if let Some(m) = self.meteor {
            if now >= m.settled_at() {
                self.meteor = None;
            } else {
                lead += m.lead_at(now) - FLYING_LEAD_MAX;
            }
        }
        self.stride_advance(now);
        if self.flight.is_some() && bank > 0.0 {
            let foot = (std::f32::consts::TAU * self.stride_shown).cos().max(0.0) * bank;
            scale_y *= 1.0 - STRIDE_SQUASH * foot;
            scale_x *= 1.0 + STRIDE_SQUASH * 0.7 * foot;
        }
        if let Some(t0) = self.land_at.filter(|&t0| now >= t0) {
            const INV_LAND_DUR: f32 = 1.0 / LAND_DUR;
            let u = now.saturating_duration_since(t0).as_secs_f32() * INV_LAND_DUR;
            if u >= 1.0 {
                self.land_at = None;
            } else {
                let q = LAND_AMP
                    * (-LAND_DECAY * u).exp()
                    * (std::f32::consts::TAU * LAND_FREQ * u).cos();
                scale_y *= 1.0 - q;
                scale_x *= 1.0 + q * 0.6;
            }
        }
        match reaction {
            CatReaction::Startled => {
                const INV_OOPS_ATTACK: f32 = 1.0 / OOPS_ATTACK;
                const INV_OOPS_SETTLE: f32 = 1.0 / OOPS_SETTLE;
                let attack = self.oops_at.map_or(1.0, |t0| {
                    smoothstep_0(
                        INV_OOPS_ATTACK,
                        now.saturating_duration_since(t0).as_secs_f32(),
                    )
                });
                let settle = self.reaction_until.map_or(1.0, |until| {
                    smoothstep_0(
                        INV_OOPS_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    )
                });
                let env = attack * settle;
                lead = lead * (1.0 - env) - OOPS_RECOIL * env;
                scale_y *= 1.0 - OOPS_SQUASH * env;
                scale_x *= 1.0 + OOPS_SQUASH * 0.6 * env;
            }
            CatReaction::Wince => {
                const INV_WINCE_HOLD: f32 = 1.0 / WINCE_HOLD;
                const INV_WINCE_ATTACK: f32 = 1.0 / WINCE_ATTACK;
                const INV_WINCE_SETTLE: f32 = 1.0 / WINCE_SETTLE;
                let elapsed = self
                    .wince_at
                    .map_or(0.0, |t0| now.saturating_duration_since(t0).as_secs_f32());
                let attack = smoothstep_0(INV_WINCE_ATTACK, elapsed);
                let settle = self.reaction_until.map_or(1.0, |until| {
                    smoothstep_0(
                        INV_WINCE_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    )
                });
                let chain = self.wince_chain.max(1);
                let phase = elapsed * INV_WINCE_HOLD * f32::from(chain);
                let pulse = 0.78 + 0.22 * (std::f32::consts::TAU * phase).cos().abs();
                let strength = 1.0 + 0.18 * f32::from(chain.saturating_sub(1));
                let env = attack * settle * pulse * strength;
                lead = lead * (1.0 - env.min(1.0)) - WINCE_RECOIL * env;
                scale_y *= 1.0 - WINCE_SQUASH * env;
                scale_x *= 1.0 + WINCE_SQUASH * 0.7 * env;
                let omega =
                    WINCE_TUMBLE_OMEGA[(chain as usize - 1).min(MAX_WINCE_CHAIN as usize - 1)];
                let swing = (-WINCE_TUMBLE_DECAY * elapsed).exp() * (omega * elapsed).sin();
                let swing_env = swing * attack * settle;
                lead += WINCE_TUMBLE * swing_env;
                let wobble = 0.05 * swing_env;
                scale_x *= 1.0 + wobble;
                scale_y *= 1.0 - wobble;
            }
            CatReaction::Delight => {
                const INV_DELIGHT_HOLD: f32 = 1.0 / DELIGHT_HOLD;
                const INV_DELIGHT_ATTACK: f32 = 1.0 / DELIGHT_ATTACK;
                const INV_DELIGHT_SETTLE: f32 = 1.0 / DELIGHT_SETTLE;
                let elapsed = self
                    .delight_at
                    .map_or(0.0, |t0| now.saturating_duration_since(t0).as_secs_f32());
                let attack = smoothstep_0(INV_DELIGHT_ATTACK, elapsed);
                let settle = self.reaction_until.map_or(1.0, |until| {
                    smoothstep_0(
                        INV_DELIGHT_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    )
                });
                let arc = (elapsed * INV_DELIGHT_HOLD).clamp(0.0, 1.0);
                let env = attack * settle;
                let chain = self.delight_chain.max(1);
                let strength = 1.0 + 0.16 * f32::from(chain.saturating_sub(1));
                let pulse = (std::f32::consts::PI * arc * f32::from(chain)).sin().abs();
                let stretch = pulse * env * strength;
                scale_y *= 1.0 + DELIGHT_STRETCH * 0.33 * stretch;
                scale_x *= 1.0 - DELIGHT_STRETCH * 0.18 * stretch;
            }
            CatReaction::Cruise | CatReaction::Celebrate | CatReaction::Discovery => {}
        }
        if self.sing > 0.0 {
            let u = self.sing_beat.fract();
            let pulse = (1.0 - u) * (1.0 - u);
            scale_y *= 1.0 - SING_SQUASH * pulse * self.sing;
            scale_x *= 1.0 + SING_SQUASH * 0.7 * pulse * self.sing;
            lead += SING_SWAY * (std::f32::consts::PI * self.sing_beat).sin() * self.sing;
        }
        let eyes = match reaction {
            CatReaction::Wince => EyesFrame::Blink,
            CatReaction::Delight => EyesFrame::Happy,
            CatReaction::Celebrate | CatReaction::Startled => EyesFrame::Open,
            CatReaction::Cruise | CatReaction::Discovery if !matches!(exit, CatExit::Plain) => {
                EyesFrame::Open
            }
            CatReaction::Cruise | CatReaction::Discovery => {
                if self.meteor.is_some_and(|m| now < m.squint_until()) {
                    // The flight's squint: through the flight and a beat past the
                    // landing, outranking the idle blink.
                    EyesFrame::Happy
                } else if crate::companion::sing_face_live(self.sing) {
                    // Singing outranks blink and uses happy eyes.
                    EyesFrame::Happy
                } else if self.blink_active(now) {
                    // A blink outranks the cruising face (`disp` is pinned at 1.0 while typing,
                    // so a blink gated below the happy face would never fire).
                    EyesFrame::Blink
                } else if self.disp >= HAPPY_GATE {
                    EyesFrame::Happy
                } else {
                    EyesFrame::Open
                }
            }
        };
        CatPose {
            scale_x,
            scale_y,
            lead,
            eyes,
        }
    }

    /// Settle the spine to fully idle: display momentum resets, the follower
    /// disarms and any landing bounce clears, so the next hidden frame is
    /// byte-identical.
    #[inline]
    const fn reset_spine(&mut self) {
        self.disp = 0.0;
        self.disp_at = None;
        self.land_at = None;
        self.meteor = None;
        self.oops_at = None;
        self.oops_last = None;
        self.wince_at = None;
        self.wince_last = None;
        self.wince_chain = 0;
        self.delight_at = None;
        self.delight_last = None;
        self.delight_chain = 0;
    }

    /// Settle a finished flight or collection hello back to `State::Hidden`,
    /// applying any parked look and clearing per-appearance state.
    #[inline]
    const fn settle_hidden(&mut self) {
        self.state = State::Hidden;
        self.flight = None;
        self.fold = None;
        self.facing_left = false;
        self.reaction = CatReaction::Cruise;
        self.reaction_until = None;
        self.discovery_until = None;
        self.collection_hello = false;
        self.collection_paused_at = None;
        self.colors = None;
        if let Some(pending) = self.pending_look.take() {
            self.look = pending;
        }
        self.stride_at = None;
        self.stride_shown = self.stride;
        self.reset_spine();
    }

    /// The reduced-motion frame, with no fade or bob: a collection hello is an opaque still for its bounded hold, then gone in one step.
    pub fn static_frame(&mut self, now: Instant) -> CatFrame {
        self.fold = None;
        self.facing_left = false;
        let now = self.collection_sample_time(now);
        let look = self.look;
        if self.collection_hello && self.discovery_active(now) {
            let live_reaction = self.reaction_at(now);
            let reaction = if matches!(live_reaction, CatReaction::Startled | CatReaction::Wince) {
                live_reaction
            } else {
                CatReaction::Discovery
            };
            return CatFrame {
                alpha: 255,
                exit: CatExit::Plain,
                fade_out: 0.0,
                look,
                reaction,
                discovery: true,
                collection_hello: true,
                bob: 0.0,
                sing: 0.0,
                pose: CatPose::STILL,
            };
        }
        if self.collection_hello {
            self.settle_hidden();
        }
        let look = self.look;
        if crate::companion::sing_face_live(self.sing) && !matches!(self.state, State::Hidden) {
            return CatFrame {
                alpha: 255,
                exit: CatExit::Plain,
                fade_out: 0.0,
                look,
                reaction: CatReaction::Cruise,
                discovery: false,
                collection_hello: false,
                bob: 0.0,
                sing: self.sing,
                pose: CatPose::STILL,
            };
        }
        if self.sing == 0.0 && !matches!(self.state, State::Hidden) {
            let grounded = self.decayed(now) < LOW;
            match self.state {
                State::FadeIn(t0) => {
                    if grounded {
                        self.exit = CatExit::Plain;
                        self.state = State::FadeOut(self.low_crossing_at(now));
                    } else if now.saturating_duration_since(t0).as_secs_f32() >= FADE_IN {
                        self.state = State::Shown;
                    }
                }
                State::Shown => {
                    if grounded {
                        self.exit = CatExit::Plain;
                        self.state = State::FadeOut(self.low_crossing_at(now));
                    }
                }
                State::FadeOut(t0) => {
                    if now.saturating_duration_since(t0) >= FADE_OUT_DUR {
                        self.state = State::Hidden;
                    }
                }
                State::Hidden => {}
            }
            if matches!(self.state, State::Hidden) {
                self.settle_hidden();
                return hidden_frame(self.look);
            }
            let alpha = if matches!(self.state, State::Shown) {
                255
            } else {
                0
            };
            return CatFrame {
                alpha,
                ..hidden_frame(look)
            };
        }
        self.reset_spine();
        hidden_frame(look)
    }

    /// The next future redraw deadline for a reduced-motion collection hello.
    #[inline]
    #[must_use]
    pub fn static_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.collection_hello || self.collection_paused_at.is_some() {
            return None;
        }
        let d0 = self.discovery_until.filter(|d| *d > now);
        let d1 = if matches!(self.reaction, CatReaction::Startled | CatReaction::Wince) {
            self.reaction_until.filter(|d| *d > now)
        } else {
            None
        };
        match (d0, d1) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Freeze the first footprint palette sample of this visible episode, so later frames keep the same atlas tile when the colours beneath the cat change.
    #[inline]
    pub const fn colors_for_episode(&mut self, sampled: CatColorKey) -> CatColorKey {
        match self.colors {
            Some(c) => c,
            None => {
                self.colors = Some(sampled);
                sampled
            }
        }
    }

    /// The already-frozen palette, if any: the host checks it before its footprint walk, so every frame after the first is an O(1) hit.
    #[must_use]
    #[inline]
    pub const fn episode_colors(&self) -> Option<CatColorKey> {
        self.colors
    }

    /// Advance the fade/exit machine one frame and resolve the draw state.
    pub fn frame(&mut self, now: Instant) -> CatFrame {
        let now = self.collection_sample_time(now);
        let discovery_until = self.discovery_until;
        let discovery = self.discovery_active(now);
        if !discovery {
            self.discovery_until = None;
        }
        let reaction = self.reaction_at(now);
        let look = self.look;
        if matches!(self.state, State::Hidden) {
            self.reset_spine();
            return hidden_frame(look);
        }
        let s = self.decayed(now);
        self.stride_advance(now);
        let grounded = s < LOW || (self.collection_hello && s < REVIVE_GATE);
        match self.state {
            State::Hidden => hidden_frame(look),
            State::FadeIn(t0) => {
                if !discovery && grounded {
                    return self.grounded_exit(discovery_until, now);
                }
                const INV_FADE_IN: f32 = 1.0 / FADE_IN;
                let t =
                    (now.saturating_duration_since(t0).as_secs_f32() * INV_FADE_IN).clamp(0.0, 1.0);
                if t >= 1.0 {
                    self.state = State::Shown;
                }
                let bob = self.frame_bob();
                let pose = self.resolve_pose(now, s, reaction, CatExit::Plain);
                CatFrame {
                    alpha: (t * 255.0) as u8,
                    exit: CatExit::Plain,
                    fade_out: 0.0,
                    look,
                    reaction,
                    discovery,
                    collection_hello: self.collection_hello,
                    bob,
                    sing: self.sing,
                    pose,
                }
            }
            State::Shown => {
                if !discovery && grounded {
                    return self.grounded_exit(discovery_until, now);
                }
                let bob = self.frame_bob();
                let pose = self.resolve_pose(now, s, reaction, CatExit::Plain);
                CatFrame {
                    alpha: 255,
                    exit: CatExit::Plain,
                    fade_out: 0.0,
                    look,
                    reaction,
                    discovery,
                    collection_hello: self.collection_hello,
                    bob,
                    sing: self.sing,
                    pose,
                }
            }
            State::FadeOut(t0) => {
                const INV_FADE_OUT: f32 = 1.0 / FADE_OUT;
                let t = (now.saturating_duration_since(t0).as_secs_f32() * INV_FADE_OUT)
                    .clamp(0.0, 1.0);
                if t >= 1.0 {
                    self.settle_hidden();
                    return hidden_frame(self.look);
                }
                let bob = self.frame_bob();
                let pose = self.resolve_pose(now, s, reaction, self.exit);
                CatFrame {
                    alpha: ((1.0 - t) * 255.0) as u8,
                    exit: self.exit,
                    fade_out: t,
                    look,
                    reaction,
                    discovery: false,
                    collection_hello: self.collection_hello,
                    bob,
                    sing: self.sing,
                    pose,
                }
            }
        }
    }

    #[inline]
    const fn frame_bob(&self) -> f32 {
        if self.sing <= 0.0 {
            self.bob()
        } else {
            let dance =
                crate::supernova::sin_cos_f32(std::f32::consts::TAU * self.sing_beat).0 * BOB_AMP;
            if self.sing >= 1.0 {
                dance
            } else {
                let cruise = self.bob();
                cruise + (dance - cruise) * self.sing
            }
        }
    }

    /// The shared grounded-exit edge of the FadeIn/Shown arms: a grounded
    /// non-discovery flight begins its fade-out — a collection hello's
    /// goodbye starts at the promised hold deadline, an earned flight at the
    /// analytic [`LOW`] crossing.
    #[inline]
    fn grounded_exit(&mut self, discovery_until: Option<Instant>, now: Instant) -> CatFrame {
        let started = if self.collection_hello {
            discovery_until.unwrap_or(now)
        } else {
            self.low_crossing_at(now)
        };
        self.begin_fade_out_at(started, now)
    }

    /// Exact wall-clock instant at which an earned flight crossed [`LOW`],
    /// derived from the metric's decay ([`TypingMomentum::low_crossing`]) so
    /// the lifecycle is frame-rate independent.
    #[inline]
    fn low_crossing_at(&self, now: Instant) -> Instant {
        self.momentum.low_crossing(LOW).unwrap_or(now)
    }

    #[inline]
    fn begin_fade_out_at(&mut self, started: Instant, now: Instant) -> CatFrame {
        self.exit = if self.collection_hello {
            CatExit::Plain
        } else {
            self.roll_exit()
        };
        self.state = State::FadeOut(started);
        self.land_at = Some(started);
        self.frame(now)
    }

    /// On screen (fading or shown) ⇒ keep the host's 60 fps re-arm going.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        !matches!(self.state, State::Hidden)
    }

    /// The canonical typing-momentum metric at `now` (lazily decayed), the same number the glow engine's spine chases.
    #[must_use]
    #[inline]
    pub fn momentum(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }

    #[inline]
    fn decayed(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }
    #[inline]
    const fn xorshift(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }
    /// 20% star wink, 20% heart meow, else a plain fade.
    #[inline]
    const fn roll_exit(&mut self) -> CatExit {
        match self.xorshift() % 5 {
            0 => CatExit::StarWink,
            1 => CatExit::HeartMeow,
            _ => CatExit::Plain,
        }
    }
}

const EXIT_HEART_PINK: u32 = 0x00FF_5C8A;

/// The exit flourish's light-theme inks, through the shared recipe.
#[inline]
const fn exit_heart_light() -> u32 {
    const INK: u32 = InkRole::OverText.ink(EXIT_HEART_PINK);
    INK
}
#[inline]
const fn exit_star_light() -> u32 {
    const INK: u32 = InkRole::OverText.ink(twinkle_rgb(true));
    INK
}

/// Draw the fade-out flourish (a rising heart for HeartMeow, a sparkling star for StarWink) near the cat anchor `(ax, ay)` in window-absolute pixels.
#[allow(
    clippy::too_many_arguments,
    reason = "geometry + theme + both output sinks; the exit flourish needs the full frame context"
)]
pub fn emit_exit_fx(
    exit: CatExit,
    fade_out: f32,
    ax: i32,
    ay: i32,
    dark_theme: bool,
    geom: Geom,
    out: &mut Vec<GlowQuad>,
    halos: &mut Vec<RainHalo>,
) {
    if matches!(exit, CatExit::Plain) {
        return;
    }
    let ch = geom.ch as i32;
    let pop = (fade_out * std::f32::consts::PI).sin().clamp(0.0, 1.0);
    match exit {
        CatExit::Plain => {}
        CatExit::HeartMeow => {
            const INV_14: f32 = 1.0 / 14.0;
            let u = (ch as f32 * INV_14).max(1.0) as i32;
            let hx = ax;
            let hy = ay - (fade_out * ch as f32 * 1.4) as i32;
            if dark_theme {
                let cov = (230.0 * pop) as u8;
                if cov == 0 {
                    return;
                }
                const ROWS: [(i32, i32); 6] = [(1, 2), (0, 6), (0, 6), (1, 5), (2, 4), (3, 3)];
                const BUMPS: [(i32, i32); 1] = [(4, 5)];
                let pink = premul_rgb(EXIT_HEART_PINK, cov);
                for (ry, &(x0, x1)) in ROWS.iter().enumerate() {
                    push_fx_rect(
                        out,
                        geom,
                        hx + x0 * u,
                        hy + ry as i32 * u,
                        (x1 - x0 + 1) * u,
                        u,
                        pink,
                    );
                }
                for &(x0, x1) in &BUMPS {
                    push_fx_rect(out, geom, hx + x0 * u, hy, (x1 - x0 + 1) * u, u, pink);
                }
            } else {
                let alpha = (210.0 * pop).min(InkRole::OverText.alpha_cap()) as u8;
                if alpha >= 24 {
                    let cx = (hx + 3 * u) as f32;
                    let cy = (hy + 3 * u) as f32;
                    push_exit_veil(
                        halos,
                        geom,
                        cx,
                        cy,
                        3.5 * u as f32,
                        3.2 * u as f32,
                        exit_heart_light(),
                        alpha,
                    );
                }
            }
        }
        CatExit::StarWink => {
            let twinkle = 0.6 + 0.4 * (fade_out * EXIT_STAR_SCINT).sin();
            let sx = ax + geom.cw as i32;
            let sy = ay - ch / 3;
            let a = (ch / 5).max(2);
            if dark_theme {
                let cov = (255.0 * pop * twinkle).clamp(0.0, 255.0) as u8;
                if cov == 0 {
                    return;
                }
                let star_rgb = twinkle_rgb(false);
                let star = premul_rgb(star_rgb, cov);
                push_fx_rect(out, geom, sx - a, sy, 2 * a + 1, 1, star);
                push_fx_rect(out, geom, sx, sy - a, 1, 2 * a + 1, star);
                let d = a / 2;
                push_fx_rect(
                    out,
                    geom,
                    sx - d,
                    sy - d,
                    2 * d + 1,
                    1,
                    premul_rgb(star_rgb, cov / 2),
                );
            } else {
                let alpha = (230.0 * pop * twinkle).clamp(0.0, InkRole::OverText.alpha_cap()) as u8;
                if alpha >= 24 {
                    let ink = exit_star_light();
                    let (cx, cy) = (sx as f32, sy as f32);
                    let af = a as f32;
                    push_exit_veil(halos, geom, cx, cy, 1.4 * af, 0.5 * af, ink, alpha);
                    push_exit_veil(halos, geom, cx, cy, 0.5 * af, 1.4 * af, ink, alpha);
                }
            }
        }
    }
}

/// Push one source-over radial veil ([`HaloMode::Over`]) of the exit flourish: `rgb` is a
/// straight `0x00RRGGBB` ink and `alpha` the centre opacity ceiling, carried in the colour's
/// high byte (`aterm_render::halo_over_cap`).
#[allow(
    clippy::too_many_arguments,
    reason = "centre + per-axis radii + ink + alpha; a param struct would obscure the geometry at the flourish call sites"
)]
#[inline]
fn push_exit_veil(
    out: &mut Vec<RainHalo>,
    geom: Geom,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    rgb: u32,
    alpha: u8,
) {
    if alpha < 24 {
        return;
    }
    let color = (u32::from(alpha) << 24) | (rgb & 0x00FF_FFFF);
    push_halo_rows(out, geom, (cx, cy), (rx, ry), color, HaloMode::Over);
}

/// Nearest-neighbour resample a straight-alpha RGBA8 image `src` (`sw×sh`) to `tw×th`.
pub fn resample_nearest(src: &[u8], sw: usize, sh: usize, tw: usize, th: usize) -> Vec<u8> {
    let mut out = Vec::new();
    resample_nearest_into(src, sw, sh, tw, th, &mut out);
    out
}

/// Nearest-neighbour resample a straight-alpha RGBA8 image `src` (`sw×sh`) to `tw×th` into `out`, reusing its capacity.
pub fn resample_nearest_into(
    src: &[u8],
    sw: usize,
    sh: usize,
    tw: usize,
    th: usize,
    out: &mut Vec<u8>,
) {
    let Some(row_bytes) = tw.checked_mul(4) else {
        out.clear();
        return;
    };
    let Some(total_bytes) = row_bytes.checked_mul(th) else {
        out.clear();
        return;
    };
    if total_bytes == 0 {
        out.clear();
        return;
    }
    let Some(src_row_bytes) = sw.checked_mul(4) else {
        out.clear();
        out.resize(total_bytes, 0);
        return;
    };
    let Some(src_total_bytes) = src_row_bytes.checked_mul(sh) else {
        out.clear();
        out.resize(total_bytes, 0);
        return;
    };
    if sw == 0 || sh == 0 || src.len() < src_total_bytes {
        out.clear();
        out.resize(total_bytes, 0);
        return;
    }
    if sw == tw && sh == th {
        out.clear();
        out.extend_from_slice(&src[..total_bytes]);
        return;
    }
    out.resize(total_bytes, 0);
    let y_step = sh / th;
    let y_rem = sh % th;
    let x_step = sw / tw;
    let x_rem = sw % tw;
    let mut sy = 0usize;
    let mut y_acc = 0usize;
    let mut prev_sy = usize::MAX;
    for ty in 0..th {
        let cur_sy = sy;
        sy += y_step;
        y_acc += y_rem;
        if y_acc >= th {
            y_acc -= th;
            sy += 1;
        }
        let row_d = ty * row_bytes;
        if cur_sy == prev_sy {
            let prev_row_d = row_d - row_bytes;
            out.copy_within(prev_row_d..prev_row_d + row_bytes, row_d);
            continue;
        }
        prev_sy = cur_sy;
        let row_s = cur_sy * src_row_bytes;
        if sw == tw {
            out[row_d..row_d + row_bytes].copy_from_slice(&src[row_s..row_s + row_bytes]);
            continue;
        }
        let src_row = src[row_s..row_s + src_row_bytes].as_chunks::<4>().0;
        let dst_row = out[row_d..row_d + row_bytes].as_chunks_mut::<4>().0;
        let mut sx = 0usize;
        let mut x_acc = 0usize;
        for dst_px in dst_row {
            *dst_px = src_row[sx];
            sx += x_step;
            x_acc += x_rem;
            if x_acc >= tw {
                x_acc -= tw;
                sx += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const V056_MIN_RUN_KEYS: u32 = 10;
    const _: () = assert!(MIN_RUN_KEYS > V056_MIN_RUN_KEYS);

    #[test]
    fn authenticated_fold_is_bounded_by_wall_time_and_hidden_cats_ignore_it() {
        use crate::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};

        let t0 = Instant::now();
        let mut hidden = CursorCat::default();
        hidden.on_motion_pulse(CursorCatMotionPulse {
            at: t0,
            kind: CursorCatMotionKind::FoldForward,
        });
        assert!(
            hidden.placement_frame(t0).fold.is_none(),
            "one fold pulse cannot materialize an unearned cat"
        );

        let mut cat = CursorCat {
            state: State::Shown,
            ..CursorCat::default()
        };
        cat.on_motion_pulse(CursorCatMotionPulse {
            at: t0,
            kind: CursorCatMotionKind::FoldForward,
        });
        let launch = cat.placement_frame(t0).fold.expect("live cat folds");
        assert_eq!(launch.direction, CatFoldDirection::Forward);
        assert_eq!(launch.progress, 0.0);

        let middle = cat
            .placement_frame(t0 + CURSOR_FOLD_DURATION / 2)
            .fold
            .expect("the midpoint is still a bounded seam");
        assert!((middle.progress - 0.5).abs() < 1e-6);
        assert!(
            cat.placement_frame(t0 + CURSOR_FOLD_DURATION)
                .fold
                .is_none(),
            "a sparse frame at the duration lands immediately"
        );
    }

    #[test]
    fn reverse_fold_changes_place_without_double_delivering_backspace() {
        use crate::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};

        let t0 = Instant::now();
        let mut cat = CursorCat {
            state: State::Shown,
            ..CursorCat::default()
        };
        cat.momentum.set_value(t0, 0.8);
        let before = cat.momentum(t0);
        cat.on_motion_pulse(CursorCatMotionPulse {
            at: t0,
            kind: CursorCatMotionKind::FoldReverse,
        });
        assert_eq!(
            cat.momentum(t0),
            before,
            "the input seam already delivered the physical Backspace"
        );
        let placement = cat.placement_frame(t0);
        assert!(placement.facing_left);
        assert_eq!(
            placement.fold.expect("reverse seam").direction,
            CatFoldDirection::Reverse
        );

        let _ = cat.static_frame(t0 + Duration::from_millis(1));
        let settled = cat.placement_frame(t0 + Duration::from_millis(1));
        assert!(settled.fold.is_none() && !settled.facing_left);
    }

    #[test]
    fn cat_exit_starwink_flash_rate_stays_under_the_photosensitivity_bound() {
        let hz = EXIT_STAR_SCINT / FADE_OUT / std::f32::consts::TAU;
        let bound = crate::effect_util::TWINKLE_FLASH_BOUND_HZ;
        assert!(
            hz <= bound,
            "the exit wink flashes at {hz} Hz (bound {bound})"
        );
        const { assert!(EXIT_STAR_SCINT > 0.0, "the wink still glints") };
    }

    const fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 10,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: (40 * 8) as u16,
            win_h: (10 * 16) as u16,
            head: 0,
        }
    }

    #[test]
    fn unowned_cursor_relocation_retires_only_the_earned_flying_episode() {
        let now = Instant::now();
        let mut earned = CursorCat {
            state: State::Shown,
            flight: Some(now),
            ..CursorCat::default()
        };
        earned.momentum.set_value(now, 1.0);
        assert!(earned.is_active());
        earned.retire_unowned_cursor_motion();
        assert!(!earned.is_active());
        assert_eq!(earned.frame(now).alpha, 0);

        let mut promised = CursorCat::default();
        promised.on_collect(now, KittyLook::default());
        promised.retire_unowned_cursor_motion();
        assert!(
            promised.is_active(),
            "a collection hello is independently owned"
        );
        assert!(
            promised
                .frame(now + Duration::from_secs_f32(FADE_IN * 0.5))
                .alpha
                > 0,
            "the independently owned hello remains visible after its fade-in begins"
        );
    }

    #[test]
    fn casual_word_or_two_never_summons_the_cat() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        for i in 0..15u64 {
            c.on_key(t + Duration::from_millis(i * 60), true);
            assert!(
                !c.is_active(),
                "a casual burst must stay cat-free (key {i})"
            );
        }
        let end = t + Duration::from_millis(14 * 60);
        assert!(
            c.momentum(end) < CAT_BAND,
            "a word or two never reaches the {CAT_BAND} band: {}",
            c.momentum(end)
        );
        assert_eq!(c.frame(end + Duration::from_millis(100)).alpha, 0);
    }

    #[test]
    fn sixteen_high_band_keys_are_required_even_after_full_dwell() {
        let t = Instant::now();
        let mut c = CursorCat::default();
        c.momentum.set_value(t, 1.0);
        c.sustain = CAT_DWELL;
        c.last = Some(t);
        for i in 1..MIN_RUN_KEYS {
            c.on_key(t + Duration::from_millis(u64::from(i) * 40), true);
            assert!(
                !c.is_active(),
                "fewer than {MIN_RUN_KEYS} qualifying keys must stay hidden"
            );
        }
        assert_eq!(c.run_keys, MIN_RUN_KEYS - 1);
        assert!(
            c.run_keys >= V056_MIN_RUN_KEYS,
            "the negative control already clears v0.56's ten-key floor"
        );
        c.on_key(
            t + Duration::from_millis(u64::from(MIN_RUN_KEYS) * 40),
            true,
        );
        assert!(c.is_active(), "the sixteenth qualifying key summons");
    }

    #[test]
    fn cat_requires_the_high_band_held_not_merely_touched() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let mut band_entry_key = None;
        let mut summoned_at_key = None;
        for i in 0..240u64 {
            let ti = t + Duration::from_millis(i * 40);
            if band_entry_key.is_none() && c.momentum(ti) >= CAT_BAND {
                band_entry_key = Some(i);
            }
            c.on_key(ti, true);
            if c.is_active() {
                summoned_at_key = Some(i);
                break;
            }
        }
        let entered = band_entry_key.expect("flat-out typing must reach the band");
        let summoned = summoned_at_key.expect("a long sustained run must summon");
        let held = (summoned - entered) as f32 * 0.040;
        assert!(
            held >= CAT_DWELL - 0.10,
            "the band must be HELD ~{CAT_DWELL} s before the summon, held only {held} s"
        );
        let elapsed = summoned as f32 * 0.040;
        assert!(
            (2.2..=3.6).contains(&elapsed),
            "band entry + dwell must land in the 2.2-3.6 s target window \
             (owner 2026-07-24: 'a little bit less momentum'), got {elapsed} s"
        );
    }

    #[test]
    fn summon_latency_is_bounded_across_typing_cadences() {
        for (gap_ms, max_keys) in [(25u64, 320usize), (60, 160), (120, 90)] {
            let mut c = CursorCat::default();
            let t = Instant::now();
            let first = (0..max_keys).find_map(|i| {
                c.on_key(t + Duration::from_millis(i as u64 * gap_ms), true);
                c.is_active().then_some(i + 1)
            });
            let first =
                first.unwrap_or_else(|| panic!("{gap_ms}ms cadence must summon by key {max_keys}"));
            let elapsed = (first - 1) as f32 * gap_ms as f32 / 1000.0;
            assert!(
                (2.2..=3.6).contains(&elapsed),
                "{gap_ms}ms cadence summoned after {elapsed} s — outside the 2.2-3.6 s target window"
            );
        }
        for stroll_gap in [700u64, 1000] {
            let mut stroll = CursorCat::default();
            let t = Instant::now();
            for i in 0..80u64 {
                stroll.on_key(t + Duration::from_millis(i * stroll_gap), true);
            }
            assert!(
                !stroll.is_active(),
                "a {stroll_gap}ms cadence never holds the high band, so it never summons"
            );
        }
    }

    #[test]
    fn sustained_typing_summons_then_fades_out_completely() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let mut on = false;
        for i in 0..300 {
            c.on_key(t + Duration::from_millis(i * 25), true);
            if c.is_active() {
                on = true;
                break;
            }
        }
        assert!(on, "a sustained fast run must always summon the cat");
        let tf = t + Duration::from_secs(6);
        for i in 0..30 {
            c.on_key(tf + Duration::from_millis(i * 25), true);
        }
        let hot = tf + Duration::from_millis(30 * 25 + FADE_IN as u64 * 1000 + 400);
        let f = c.frame(hot);
        assert!(
            f.alpha > 200,
            "fades in to (near) full opacity, got {}",
            f.alpha
        );
        let gone = hot + Duration::from_secs(6);
        let done = c.frame(gone);
        assert_eq!(done.alpha, 0, "a sparse late frame is already hidden");
        assert_eq!(done.fp(), 0, "hidden frames settle to the zero repaint key");
        assert!(!c.is_active());
    }

    #[test]
    fn breaths_keep_the_dwell_but_word_pauses_stay_cat_free() {
        let mut flow = CursorCat::default();
        let mut t = Instant::now();
        for burst in 0..12 {
            for _ in 0..16 {
                t += Duration::from_millis(60);
                flow.on_key(t, true);
            }
            if flow.is_active() {
                break;
            }
            if burst < 11 {
                t += Duration::from_millis(150);
            }
        }
        assert!(
            flow.is_active(),
            "sustained flow with sub-200 ms breaths still earns the cat"
        );
        let mut prose = CursorCat::default();
        let mut t = Instant::now();
        for word in 0..3 {
            for _ in 0..7 {
                t += Duration::from_millis(120);
                prose.on_key(t, true);
            }
            if word < 2 {
                t += Duration::from_millis(650);
            }
        }
        assert!(
            !prose.is_active(),
            "three casual words with real pauses must NOT summon (the raised threshold)"
        );
    }

    #[test]
    fn hello_goodbye_is_not_revived_by_typing() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let fade_at = t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.05);
        let fading = cat.frame(fade_at);
        assert!(fading.collection_hello, "the goodbye is a hello frame");
        assert!(fading.alpha > 0);
        cat.on_key(fade_at + Duration::from_millis(40), true);
        cat.on_key(fade_at + Duration::from_millis(80), true);
        let done = cat.frame(fade_at + Duration::from_secs_f32(FADE_OUT + 0.2));
        assert_eq!(done.alpha, 0, "the goodbye completes");
        assert!(!done.collection_hello, "hello bookkeeping fully cleared");
        assert!(!cat.is_active(), "the machine returned to Hidden");
    }

    #[test]
    fn fading_cat_revives_on_resumed_typing() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        for i in 0..120 {
            c.on_key(t + Duration::from_millis(i * 60), true);
        }
        assert!(c.is_active(), "the run summons the cat");
        let last = t + Duration::from_millis(119 * 60);
        let low_crossing = last
            + Duration::from_secs_f32(
                crate::typing_momentum::TYPING_MOMENTUM_TAU * (c.momentum(last) / LOW).ln(),
            );
        let quiet = low_crossing + Duration::from_millis(200);
        let fading = c.frame(quiet);
        assert!(
            fading.alpha > 0 && fading.alpha < 255,
            "sampling shortly after the real LOW crossing lands mid-fade"
        );
        let observed = fading;
        c.on_key(quiet, true);
        c.on_key(quiet + Duration::from_millis(80), true);
        let back = c.frame(quiet + Duration::from_millis(400));
        assert!(
            back.alpha > observed.alpha,
            "resumed typing revives the fading cat ({} → {})",
            observed.alpha,
            back.alpha
        );
        assert!(c.is_active());
    }

    #[test]
    fn the_exit_flourish_head_never_shows_at_full_opacity() {
        let mut saw_flourish = 0u32;
        let mut saw_plain_head = 0u32;
        for keys in 120..160u64 {
            let mut c = CursorCat::default();
            // An unmarked head: a marked one keeps its own head through the
            // goodbye ([`a_marked_cat_s_goodbye_keeps_its_markings`]).
            c.set_look(KittyLook {
                variant: CatGlyphId::S100,
                ..KittyLook::default()
            });
            let t = Instant::now();
            for i in 0..keys {
                c.on_key(t + Duration::from_millis(i * 60), true);
            }
            assert!(c.is_active(), "the run summons the cat");
            let last = t + Duration::from_millis((keys - 1) * 60);
            let low_crossing = last
                + Duration::from_secs_f32(
                    crate::typing_momentum::TYPING_MOMENTUM_TAU * (c.momentum(last) / LOW).ln(),
                );
            for step in 0..120u32 {
                let now = low_crossing + Duration::from_millis(u64::from(step) * 16);
                let f = c.frame(now);
                let swapped = f.render_look().variant != f.look.variant;
                if matches!(f.exit, CatExit::HeartMeow | CatExit::StarWink) && swapped {
                    saw_flourish += 1;
                    assert!(
                        f.alpha <= 200,
                        "the flourish head appeared while the cat was still at \
                         full brightness (alpha {}, fade_out {}) — a revive in \
                         the next frame swaps it straight back, which is the \
                         one-frame flash",
                        f.alpha,
                        f.fade_out
                    );
                } else if f.alpha > 0 && !swapped {
                    saw_plain_head += 1;
                }
            }
        }
        assert!(
            saw_plain_head > 0,
            "the goodbye walks emitted no ordinary frames at all"
        );
        assert!(
            saw_flourish > 0,
            "no trial ever rolled a flourish, so the opacity assertion above \
             never ran — this test would be checking nothing"
        );
    }

    #[test]
    fn reduced_motion_never_draws_a_cat_the_machine_calls_hidden() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let mut unearned = 0u32;
        for step in 0..60u32 {
            let now = t + Duration::from_millis(u64::from(step) * 16);
            c.set_singing(
                now,
                SingSync {
                    drive: 1.0,
                    beat: 0.0,
                },
            );
            let f = c.static_frame(now);
            if f.alpha > 0 && !c.is_active() {
                unearned += 1;
            }
        }
        assert_eq!(
            unearned, 0,
            "reduced motion drew {unearned} frame(s) of a cat the machine \
             reports inactive"
        );
    }

    #[test]
    fn sparse_frame_after_collection_deadline_settles_directly_hidden() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        assert!(
            cat.frame(t + Duration::from_millis(16)).alpha > 0,
            "the hello was genuinely visible before the long present gap"
        );

        let late = t + Duration::from_secs_f32(DISCOVERY_HOLD + FADE_OUT + 2.0);
        let frame = cat.frame(late);
        assert_eq!(frame.alpha, 0);
        assert_eq!(frame.fp(), 0);
        assert!(!frame.collection_hello);
        assert!(!cat.is_active());
    }

    #[test]
    fn launch_kitties_are_unique_per_seed_and_stable() {
        let looks: Vec<KittyLook> = (0..64u64).map(KittyLook::for_launch).collect();
        for (i, look) in looks.iter().enumerate() {
            assert_eq!(
                *look,
                KittyLook::for_launch(i as u64),
                "the same seed always resolves to the same kitty"
            );
            assert_eq!(*look, look.normalized(), "launch kitties are normalized");
            assert!(
                look.accessory.is_none(),
                "accessories mark COLLECTED cats and are never minted for free"
            );
        }
        let distinct: std::collections::HashSet<_> = looks
            .iter()
            .map(|l| (l.variant, l.coat, l.iris, l.age as u8))
            .collect();
        assert!(
            distinct.len() >= 32,
            "seeds get visibly different kitties, got {} distinct of 64",
            distinct.len()
        );
        assert_ne!(
            KittyLook::for_launch(0),
            KittyLook::default(),
            "a launch kitty is not the one shared default cat"
        );
    }

    #[test]
    fn typed_word_reactions_are_bounded_expression() {
        let t = Instant::now();
        let mut hidden = CursorCat::default();
        assert!(
            !hidden.on_delight(t, 1),
            "delight never summons a hidden companion"
        );
        assert!(
            !hidden.on_curse(t, 1),
            "a wince never summons a hidden companion"
        );
        assert!(!hidden.is_active());

        let look = KittyLook::for_launch(7);
        let mut live = CursorCat::default();
        live.on_collect(t, look);
        assert!(live.on_delight(t + Duration::from_millis(5), 1));
        assert_eq!(
            live.frame(t + Duration::from_millis(5)).look,
            look,
            "delight never re-skins the companion"
        );
        assert!(live.on_curse(t + Duration::from_millis(10), 1));
        let frame = live.frame(t + Duration::from_millis(10));
        assert_eq!(frame.reaction, CatReaction::Wince);
        assert_eq!(frame.look, look, "a wince never re-skins the companion");
    }

    #[test]
    fn delight_is_expression_not_displacement() {
        let mut t = Instant::now();
        let look = KittyLook::for_launch(11);
        let mut cat = CursorCat::default();
        let mut plain = CursorCat::default();
        cat.on_collect(t, look);
        plain.on_collect(t, look);
        for _ in 0..8 {
            t += Duration::from_millis(75);
            cat.on_key(t, true);
            plain.on_key(t, true);
        }
        assert!(cat.on_delight(t, 1));
        let mid = t + Duration::from_secs_f32(DELIGHT_HOLD / 2.0);
        let apex = cat.frame(mid);
        let control = plain.frame(mid);
        assert!(
            control.bob.abs() > 0.0 && apex.bob.abs() <= BOB_AMP + 1e-6,
            "fixture must sample a non-zero stride hover: {}",
            control.bob
        );
        assert!(
            (apex.bob - control.bob).abs() < 1e-6,
            "the companion holds its stride hover through a delight: {} vs {}",
            apex.bob,
            control.bob
        );
        assert_eq!(
            apex.reaction,
            CatReaction::Delight,
            "the reaction is still armed"
        );
        assert_eq!(
            apex.pose.eyes,
            EyesFrame::Happy,
            "hearing its own name is still the happiest the companion gets"
        );
        assert!(
            apex.pose.scale_y > control.pose.scale_y,
            "delight swells the pose vertically ({} vs {})",
            apex.pose.scale_y,
            control.pose.scale_y
        );
    }

    #[test]
    fn collection_hello_uses_exact_look_and_bounded_input_reactions() {
        let t = Instant::now();
        let look = KittyLook {
            variant: CatGlyphId::S103,
            accessory: Some(CatGlyphId::AccBow),
            coat: 11,
            iris: 6,
            ..KittyLook::default()
        };
        let mut cat = CursorCat::default();
        cat.on_enter(t);
        assert!(!cat.is_active(), "Enter never summons a hidden companion");

        assert_eq!(cat.run_keys, 0);
        assert!(cat.sustain < CAT_DWELL);
        cat.on_collect(t, look);
        assert!(
            cat.is_active(),
            "an explicit collection/typed hello bypasses earning"
        );
        let hello = cat.frame(t + Duration::from_millis(400));
        assert_eq!(hello.look, look.normalized());
        assert_eq!(hello.render_look(), look.normalized());
        assert!(hello.discovery && hello.collection_hello);

        cat.on_enter(t + Duration::from_millis(500));
        let celebrate = cat.frame(t + Duration::from_millis(510));
        assert_eq!(celebrate.reaction, CatReaction::Celebrate);
        assert_eq!(celebrate.render_look().variant, CatGlyphId::SpecManeki);
        assert_eq!(celebrate.render_look().accessory, None);

        cat.on_key(t + Duration::from_millis(600), false);
        let startled = cat.frame(t + Duration::from_millis(610));
        assert_eq!(startled.reaction, CatReaction::Startled);
        assert_eq!(startled.render_look().variant, CatGlyphId::S121);
        assert_eq!(startled.render_look().accessory, None);

        let settled = cat.frame(t + Duration::from_millis(1300));
        assert_eq!(settled.reaction, CatReaction::Cruise);
        assert_eq!(settled.render_look(), look.normalized());
    }

    #[test]
    fn reduced_motion_flight_retires_and_releases_the_look_latch() {
        let mut cat = CursorCat::default();
        let t = Instant::now();
        let mut summoned_at = None;
        for i in 0..300u64 {
            let ti = t + Duration::from_millis(i * 25);
            cat.on_key(ti, true);
            if cat.is_active() {
                summoned_at = Some(ti);
                break;
            }
        }
        let summoned_at = summoned_at.expect("sustained typing summons the flight");
        let new_look = KittyLook {
            variant: CatGlyphId::S103,
            coat: 7,
            ..KittyLook::default()
        };
        cat.set_look(new_look);
        assert!(
            cat.pending_look.is_some(),
            "the sync parks while the flight is live"
        );
        let mut retired = None;
        for step in 0..400u64 {
            let ts = summoned_at + Duration::from_millis(step * 25);
            let f = cat.static_frame(ts);
            if !cat.is_active() {
                retired = Some(f);
                break;
            }
        }
        let frame = retired.expect("the reduced-motion flight retires to Hidden by time");
        assert_eq!(frame.alpha, 0, "a retired flight is fully gone");
        assert!(!cat.is_active(), "is_active clears once retired");
        assert_eq!(
            cat.look,
            new_look.normalized(),
            "the parked look applies as the flight retires — the latch released"
        );
        assert!(cat.pending_look.is_none(), "…and the parking slot is empty");
    }

    #[test]
    fn reduced_collection_hello_is_one_bounded_static_pose() {
        let t = Instant::now();
        let look = KittyLook {
            variant: CatGlyphId::S103,
            coat: 9,
            ..KittyLook::default()
        };
        let mut cat = CursorCat::default();
        cat.on_collect(t, look);

        assert_eq!(
            cat.static_deadline(t),
            Some(t + Duration::from_secs_f32(DISCOVERY_HOLD))
        );
        let first = cat.static_frame(t);
        let held = cat.static_frame(t + Duration::from_secs(2));
        for frame in [first, held] {
            assert_eq!(frame.alpha, 255);
            assert_eq!(frame.bob, 0.0);
            assert_eq!(frame.fade_out, 0.0);
            assert_eq!(frame.reaction, CatReaction::Discovery);
            assert_eq!(frame.render_look(), look.normalized());
            assert!(frame.collection_hello);
        }

        assert_eq!(
            cat.static_deadline(t + Duration::from_secs(3)),
            None,
            "an occluded erase redraw must not rearm an already-past wake"
        );
        assert!(
            cat.is_active(),
            "deadline filtering does not advance render-owned lifecycle state"
        );
        let gone = cat.static_frame(t + Duration::from_secs(3));
        assert_eq!(gone.alpha, 0);
        assert_eq!(cat.static_deadline(t + Duration::from_secs(3)), None);
        assert!(!cat.is_active());
    }

    #[test]
    fn hidden_time_does_not_consume_collection_hello() {
        let t = Instant::now();
        let look = KittyLook {
            variant: CatGlyphId::SpecSleeping,
            coat: 12,
            iris: 4,
            ..KittyLook::default()
        };
        let mut cat = CursorCat::default();
        cat.on_collect(t, look);
        cat.set_collection_presentable(t, false);

        let hidden = cat.frame(t + Duration::from_secs(30));
        assert_eq!(hidden.alpha, 0, "the hidden fade-in stays frozen");
        assert!(hidden.discovery && hidden.collection_hello);
        assert_eq!(
            cat.static_deadline(t + Duration::from_secs(30)),
            None,
            "hidden cats arm no wake"
        );

        let resumed_at = t + Duration::from_secs(30);
        cat.set_collection_presentable(resumed_at, true);
        assert_eq!(
            cat.static_deadline(resumed_at),
            Some(resumed_at + Duration::from_secs_f32(DISCOVERY_HOLD)),
            "resume restores the complete promised hold"
        );
        let visible = cat.frame(resumed_at + Duration::from_millis(400));
        assert_eq!(visible.alpha, 255);
        assert!(visible.discovery && visible.collection_hello);
        assert_eq!(visible.render_look(), look.normalized());

        let still_held = cat.frame(resumed_at + Duration::from_secs_f32(DISCOVERY_HOLD - 0.001));
        assert!(still_held.discovery, "only drawable time consumes the hold");
    }

    /// The goodbye face carries no pattern layers, so it would drop a marked
    /// cat's coat (nyan's `burst` at l4200). Every head whose art carries
    /// markings keeps its own head, coat and iris through both flourishes;
    /// the control: an unmarked head still swaps to the flourish face, so the
    /// frames below are the goodbye's.
    #[test]
    fn a_marked_cat_s_goodbye_keeps_its_markings() {
        use crate::cat_glyphs_gen::{GLYPH_IDS, GLYPHS, GlyphKind, GlyphRole};
        let mut marked_heads = 0;
        let mut swapped_plain = 0;
        for (i, def) in GLYPHS.iter().enumerate() {
            if def.kind != GlyphKind::Head {
                continue;
            }
            let variant = GLYPH_IDS[i];
            let look = KittyLook {
                variant,
                coat: 5,
                iris: 2,
                ..KittyLook::default()
            };
            let has_marks = def
                .layers
                .iter()
                .any(|l| matches!(l.role, GlyphRole::Pattern | GlyphRole::CoatShade));
            for exit in [CatExit::StarWink, CatExit::HeartMeow] {
                let mut f = CursorCat::default().frame(Instant::now());
                f.look = look;
                f.exit = exit;
                f.fade_out = 0.6;
                f.alpha = 90;
                let drawn = f.render_look();
                if has_marks {
                    assert_eq!(
                        drawn,
                        look.normalized(),
                        "{} waves goodbye ({exit:?}) without its markings",
                        def.id
                    );
                } else if drawn.variant != variant {
                    swapped_plain += 1;
                }
            }
            marked_heads += usize::from(has_marks);
        }
        assert!(marked_heads > 0, "the roster has marked heads");
        assert!(
            swapped_plain > 0,
            "an unmarked head still makes the flourish face"
        );
    }

    #[test]
    fn collection_goodbye_keeps_the_discovered_art() {
        let t = Instant::now();
        let look = KittyLook {
            variant: CatGlyphId::SpecSleeping,
            coat: 10,
            iris: 3,
            ..KittyLook::default()
        };
        let mut cat = CursorCat::default();
        cat.on_collect(t, look);

        let fade = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD));
        assert!(fade.collection_hello);
        assert_eq!(fade.exit, CatExit::Plain);
        assert_eq!(fade.render_look(), look.normalized());
    }

    #[test]
    fn global_companion_sync_cannot_morph_an_active_discovery() {
        let t = Instant::now();
        let discovered = KittyLook {
            variant: CatGlyphId::SpecSleeping,
            coat: 10,
            ..KittyLook::default()
        };
        let other_window = KittyLook {
            variant: CatGlyphId::SpecYarn,
            coat: 2,
            ..KittyLook::default()
        };
        let mut cat = CursorCat::default();
        cat.on_collect(t, discovered);
        cat.set_look(other_window);
        assert_eq!(
            cat.static_frame(t + Duration::from_millis(100))
                .render_look(),
            discovered.normalized(),
            "frame setup cannot replace the promised discovery art"
        );

        let _ = cat.static_frame(t + Duration::from_secs(3));
        cat.set_look(other_window);
        assert_eq!(
            cat.static_frame(t + Duration::from_secs(3)).render_look(),
            other_window.normalized(),
            "global companion sync resumes after the hello completes"
        );
    }

    #[test]
    fn mid_flight_look_sync_is_latched_until_the_next_wake() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        for i in 0..120 {
            c.on_key(t + Duration::from_millis(i * 60), true);
        }
        assert!(c.is_active(), "the sustained run summons the cat");
        let last = t + Duration::from_millis(119 * 60);
        let flying = c.frame(last + Duration::from_millis(100));
        assert!(flying.alpha > 0);

        let next_look = KittyLook {
            variant: CatGlyphId::SpecYarn,
            coat: 2,
            iris: 7,
            ..KittyLook::default()
        };
        c.set_look(next_look);
        let held = c.frame(last + Duration::from_millis(200));
        assert_eq!(
            held.look, flying.look,
            "a mid-flight sync must not swap the sprite"
        );
        assert_eq!(
            held.look.coat, flying.look.coat,
            "every component of the current look rides the same latch"
        );

        let reviving = last + Duration::from_secs(4);
        let _ = c.frame(reviving);
        let fading = c.frame(reviving + Duration::from_millis(200));
        assert!(
            fading.fade_out > 0.0 && fading.alpha > 0,
            "fixture must sample mid-FadeOut"
        );
        c.on_key(reviving + Duration::from_millis(200), true);
        c.on_key(reviving + Duration::from_millis(240), true);
        assert!(c.is_active(), "typing mid-FadeOut revives the appearance");
        let revived = c.frame(reviving + Duration::from_millis(280));
        assert_eq!(revived.fade_out, 0.0, "FadeOut revived back into FadeIn");
        assert_eq!(
            revived.look, flying.look,
            "a revived fade is the same appearance — still the latched look"
        );

        let fade_again = reviving + Duration::from_secs(4);
        let _ = c.frame(fade_again);
        let gone = fade_again + Duration::from_secs(2);
        let retired = c.frame(gone);
        assert_eq!(retired.alpha, 0);
        assert_eq!(
            retired.look,
            next_look.normalized(),
            "FadeOut completion releases the parked look onto the hidden frame"
        );
        assert!(!c.is_active());
        for i in 0..120 {
            c.on_key(gone + Duration::from_millis(i * 60), true);
        }
        assert!(c.is_active(), "the second run re-earns the flight");
        let rewoken = c.frame(gone + Duration::from_millis(119 * 60 + 100));
        assert_eq!(
            rewoken.look,
            next_look.normalized(),
            "the deferred look lands at the start of the next appearance"
        );
    }

    #[test]
    fn discovery_hello_presents_the_new_look_mid_flight() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        for i in 0..120 {
            c.on_key(t + Duration::from_millis(i * 60), true);
        }
        assert!(c.is_active());
        let last = t + Duration::from_millis(119 * 60);

        let parked = KittyLook {
            variant: CatGlyphId::SpecSleeping,
            coat: 12,
            ..KittyLook::default()
        };
        c.set_look(parked);
        let unlocked = KittyLook {
            variant: CatGlyphId::S103,
            accessory: Some(CatGlyphId::AccBow),
            coat: 5,
            iris: 6,
            ..KittyLook::default()
        };
        let collect_at = last + Duration::from_millis(200);
        c.on_collect(collect_at, unlocked);
        let hello = c.frame(collect_at + Duration::from_millis(100));
        assert_eq!(
            hello.look,
            unlocked.normalized(),
            "the hello presents the new collectible immediately"
        );
        assert!(hello.discovery && hello.collection_hello);

        let over = collect_at + Duration::from_secs_f32(DISCOVERY_HOLD + FADE_OUT + 1.0);
        assert_eq!(c.frame(over).alpha, 0);
        assert!(!c.is_active());
        for i in 0..120 {
            c.on_key(over + Duration::from_millis(i * 60), true);
        }
        assert!(c.is_active());
        assert_eq!(
            c.frame(over + Duration::from_millis(119 * 60 + 100)).look,
            unlocked.normalized(),
            "on_collect cleared the parked sync — the collectible stays"
        );
    }

    #[test]
    fn cursor_palette_is_frozen_for_one_visible_episode() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let dark = CatColorKey {
            accent: 2,
            background: 0,
        };
        let light = CatColorKey {
            accent: 8,
            background: 3,
        };
        assert_eq!(cat.episode_colors(), None);
        assert_eq!(cat.colors_for_episode(dark), dark);
        assert_eq!(cat.episode_colors(), Some(dark));
        assert_eq!(
            cat.colors_for_episode(light),
            dark,
            "terminal churn cannot change this episode's atlas key"
        );

        let _ = cat.static_frame(t + Duration::from_secs(3));
        cat.on_collect(t + Duration::from_secs(4), KittyLook::default());
        assert_eq!(cat.episode_colors(), None);
        assert_eq!(
            cat.colors_for_episode(light),
            light,
            "a new hello samples a new footprint palette"
        );
    }

    #[test]
    fn coordinate_rebase_invalidates_only_the_promised_cat_palette() {
        let t = Instant::now();
        let seeded = CatColorKey {
            accent: 2,
            background: 7,
        };

        let mut hello = CursorCat::default();
        hello.on_collect(t, KittyLook::default());
        let hello_look = hello.look;
        let hello_until = hello.discovery_until;
        assert_eq!(hello.colors_for_episode(seeded), seeded);
        hello.retire_unowned_cursor_motion();
        assert_eq!(
            hello.episode_colors(),
            Some(seeded),
            "the promised hello survives ordinary-flight retirement intact"
        );
        hello.rebase_placement();
        assert_eq!(
            hello.episode_colors(),
            None,
            "the old footprint key is stale"
        );
        assert!(hello.is_active() && hello.collection_hello);
        assert_eq!(
            hello.look, hello_look,
            "the collected identity is unchanged"
        );
        assert_eq!(
            hello.discovery_until, hello_until,
            "the hello's promised lifetime is unchanged"
        );

        let mut song = CursorCat::default();
        arm_singing_after_travel(&mut song, t + Duration::from_secs(1));
        let song_look = song.look;
        assert_eq!(song.colors_for_episode(seeded), seeded);
        song.retire_unowned_cursor_motion();
        assert_eq!(
            song.episode_colors(),
            Some(seeded),
            "the promised song survives ordinary-flight retirement intact"
        );
        song.rebase_placement();
        assert_eq!(
            song.episode_colors(),
            None,
            "the song also resamples its footprint"
        );
        assert!(song.is_active(), "the promised song lifecycle remains live");
        assert_eq!(song.sing, 1.0, "the rebase cannot cancel the song");
        assert_eq!(song.look, song_look, "the singing identity is unchanged");
        assert_eq!(
            song.colors_for_episode(CatColorKey {
                accent: 9,
                background: 1,
            }),
            CatColorKey {
                accent: 9,
                background: 1,
            },
            "the first frame in the new coordinate space seeds a fresh key"
        );
    }

    #[test]
    fn exit_fx_emits_for_wink_and_heart_only() {
        let g = geom();
        let mut out = Vec::new();
        let mut halos = Vec::new();
        emit_exit_fx(CatExit::Plain, 0.5, 40, 40, true, g, &mut out, &mut halos);
        assert!(out.is_empty(), "plain exit draws nothing");
        assert!(halos.is_empty(), "plain exit draws no veil either");
        emit_exit_fx(
            CatExit::HeartMeow,
            0.5,
            40,
            40,
            true,
            g,
            &mut out,
            &mut halos,
        );
        assert!(
            !out.is_empty(),
            "heart draws additive quads mid-exit on dark"
        );
        assert!(
            halos.is_empty(),
            "dark heart never uses the source-over veil"
        );
        out.clear();
        emit_exit_fx(
            CatExit::StarWink,
            0.5,
            40,
            40,
            true,
            g,
            &mut out,
            &mut halos,
        );
        assert!(
            !out.is_empty(),
            "star draws additive quads mid-exit on dark"
        );
        assert!(
            halos.is_empty(),
            "dark star never uses the source-over veil"
        );
    }

    #[test]
    fn exit_fx_light_theme_is_a_darkening_source_over_veil() {
        let g = geom();
        let luma = |rgb: u32| {
            let r = ((rgb >> 16) & 0xff) as f32;
            let g = ((rgb >> 8) & 0xff) as f32;
            let b = (rgb & 0xff) as f32;
            0.299 * r + 0.587 * g + 0.114 * b
        };
        for exit in [CatExit::HeartMeow, CatExit::StarWink] {
            let mut out = Vec::new();
            let mut halos = Vec::new();
            emit_exit_fx(exit, 0.5, 40, 40, false, g, &mut out, &mut halos);
            assert!(
                out.is_empty(),
                "{exit:?}: light theme emits NO additive quad (never white-on-white)"
            );
            assert!(
                !halos.is_empty(),
                "{exit:?}: light theme emits a visible source-over veil"
            );
            for h in &halos {
                assert!(
                    matches!(h.mode, HaloMode::Over),
                    "{exit:?}: the light veil composites SOURCE-OVER, not additive"
                );
                let ink = h.color & 0x00FF_FFFF;
                assert!(
                    luma(ink) < 160.0,
                    "{exit:?}: veil ink must be dark enough to contrast on light, luma {}",
                    luma(ink)
                );
                let centre_alpha = (h.color >> 24) & 0xff;
                assert!(
                    centre_alpha >= 96,
                    "{exit:?}: veil centre opacity must be legible, got {centre_alpha}"
                );
                assert!(
                    centre_alpha as f32 <= InkRole::OverText.alpha_cap(),
                    "{exit:?}: the veil can land on text, so it stays under the \
                     over-text legibility ceiling ({centre_alpha})"
                );
            }
        }
    }

    #[test]
    fn exit_fx_light_veil_vanishes_at_the_exit_ends() {
        let g = geom();
        for &fade in &[0.0_f32, 0.02, 0.99, 1.0] {
            let mut out = Vec::new();
            let mut halos = Vec::new();
            emit_exit_fx(
                CatExit::HeartMeow,
                fade,
                40,
                40,
                false,
                g,
                &mut out,
                &mut halos,
            );
            assert!(
                halos.is_empty(),
                "the light heart veil is nil at fade_out {fade}"
            );
        }
    }

    #[test]
    fn a_key_steps_the_hover_bob_over_frames_not_in_one() {
        for (frame_us, key_us) in [
            (8_333u64, 111_000u64),
            (8_333, 66_000),
            (16_667, 111_000),
            (16_667, 66_000),
        ] {
            let label = format!("a present every {frame_us} µs, a key every {key_us} µs");
            let t = Instant::now();
            let mut cat = CursorCat::default();
            cat.on_collect(t, KittyLook::default());
            let bob_of = |stride: f32| (std::f32::consts::TAU * stride).sin() * BOB_AMP;
            let (mut prev, mut prev_pump) = (None::<f32>, None::<f32>);
            let (mut step, mut pump_step) = (0.0f32, 0.0f32);
            let mut drawn = std::collections::BTreeSet::new();
            let mut next_key = 40_000;
            for i in 0..(3_000_000 / frame_us) {
                let at = 40_000 + i * frame_us;
                let ti = t + Duration::from_micros(at);
                if at >= next_key {
                    cat.on_key(ti, true);
                    next_key += key_us;
                }
                let f = cat.frame(ti);
                let pump = bob_of(cat.stride);
                if let Some(p) = prev {
                    step = step.max((f.bob - p).abs());
                }
                if let Some(p) = prev_pump {
                    pump_step = pump_step.max((pump - p).abs());
                }
                drawn.insert((f.bob * 4096.0) as i64);
                (prev, prev_pump) = (Some(f.bob), Some(pump));
            }
            assert!(
                step <= 0.06,
                "{label}: a present moved the bob {step} of a row"
            );
            assert!(
                drawn.len() > 20,
                "{label}: the bob still moves with the keys"
            );
            assert!(
                pump_step > 0.1,
                "{label}: control: the pump jumps a key's half cycle ({pump_step})"
            );
        }
    }

    #[test]
    fn stride_pump_animates_from_cadence_not_from_the_pinned_level() {
        let look = KittyLook::default();
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, look);

        let mut poses = Vec::new();
        let mut disps = Vec::new();
        for i in 0..30 {
            let ti = t + Duration::from_millis(40 + i * 16);
            cat.on_key(ti, true);
            let f = cat.frame(ti);
            poses.push((f.pose.scale_y, f.bob));
            disps.push(cat.disp);
        }
        let tail = &poses[15..];
        assert!(
            disps[15..].iter().all(|d| *d > 0.99),
            "precondition: the canonical metric IS pinned during a real run, so a \
             level-driven pose cannot animate here"
        );
        let squash: std::collections::BTreeSet<i64> =
            tail.iter().map(|(sy, _)| (sy * 4096.0) as i64).collect();
        assert!(
            squash.len() > 2,
            "the body must still pump while the level is pinned, got {} distinct \
             values from {} frames",
            squash.len(),
            tail.len()
        );
        let bobs: std::collections::BTreeSet<i64> =
            tail.iter().map(|(_, b)| (b * 4096.0) as i64).collect();
        assert!(bobs.len() > 2, "and the bob steps with it: {bobs:?}");

        let mut fast = CursorCat::default();
        fast.on_collect(t, look);
        let mut slow = CursorCat::default();
        slow.on_collect(t, look);
        for i in 0..20 {
            fast.on_key(t + Duration::from_millis(40 + i * 16), true);
        }
        for i in 0..5 {
            slow.on_key(t + Duration::from_millis(40 + i * 64), true);
        }
        let at = t + Duration::from_millis(400);
        let _ = fast.frame(at);
        let _ = slow.frame(at);
        assert!(
            fast.stride > slow.stride,
            "more keys in the same wall time = more strides ({} vs {})",
            fast.stride,
            slow.stride
        );

        let mut blinky = CursorCat::default();
        blinky.on_collect(t, look);
        let mut saw_blink = false;
        for i in 0..140 {
            let ti = t + Duration::from_millis(40 + i * 16);
            blinky.on_key(ti, true);
            if blinky.frame(ti).pose.eyes == EyesFrame::Blink {
                saw_blink = true;
            }
        }
        assert!(saw_blink, "a cat in sustained flight must still blink");

        let mut rest = CursorCat::default();
        rest.on_collect(t, look);
        let r = rest.frame(t + Duration::from_millis(1600));
        assert!(
            r.pose.scale_x < 1.03 && r.pose.scale_y > 0.97,
            "a resting cat keeps its natural silhouette ({} x {})",
            r.pose.scale_x,
            r.pose.scale_y
        );
    }

    #[test]
    fn banking_leans_and_stretches_with_momentum() {
        let look = KittyLook::default();
        let t = Instant::now();
        let mut fast = CursorCat::default();
        fast.on_collect(t, look);
        let mut ff = fast.frame(t + Duration::from_millis(16));
        for i in 0..26 {
            let ti = t + Duration::from_millis(40 + i * 16);
            fast.on_key(ti, true);
            ff = fast.frame(ti);
        }
        assert!(
            ff.pose.scale_x > 1.08,
            "fast flight stretches forward, got {}",
            ff.pose.scale_x
        );
        assert!(
            ff.pose.lead > 0.08,
            "fast flight leans ahead, got {}",
            ff.pose.lead
        );
        assert!(ff.pose.scale_y < 1.0, "fast flight thins vertically");

        let mut slow = CursorCat::default();
        slow.on_collect(t, look);
        let sf = slow.frame(t + Duration::from_millis(1600));
        assert!(sf.alpha > 0, "still visible during the discovery hold");
        assert!(
            sf.pose.scale_x < 1.03,
            "a resting cat is near natural width, got {}",
            sf.pose.scale_x
        );
        assert!(
            sf.pose.lead < 0.03,
            "a resting cat does not lean, got {}",
            sf.pose.lead
        );
        assert!(
            ff.pose.scale_x > sf.pose.scale_x + 0.05,
            "momentum, not chance, selects the lean"
        );
    }

    #[test]
    fn landing_squash_fires_on_stop_and_on_enter() {
        let look = KittyLook::default();
        let t = Instant::now();

        let mut stomp = CursorCat::default();
        stomp.on_collect(t, look);
        let _ = stomp.frame(t + Duration::from_millis(80));
        stomp.on_enter(t + Duration::from_millis(120));
        let hit = stomp.frame(t + Duration::from_millis(130));
        assert!(
            hit.pose.scale_y < 0.95,
            "the Enter stomp squashes vertically, got {}",
            hit.pose.scale_y
        );
        let settled =
            stomp.frame(t + Duration::from_millis(130) + Duration::from_secs_f32(LAND_DUR + 0.05));
        assert!(
            settled.pose.scale_y > hit.pose.scale_y + 0.05,
            "the bounce releases back toward neutral"
        );

        let mut stop = CursorCat::default();
        stop.on_collect(t, look);
        let _ = stop.frame(t + Duration::from_millis(80));
        let land = stop.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.06));
        assert!(land.alpha > 0, "the touchdown frame is still drawn");
        assert!(
            land.pose.scale_y < 0.95,
            "landing on a stop squashes, got {}",
            land.pose.scale_y
        );
    }

    #[test]
    fn idle_blink_is_deterministic_and_windowed() {
        let build = || {
            let mut cat = CursorCat::default();
            let t = Instant::now();
            cat.on_collect(t, KittyLook::default());
            (cat, t)
        };
        let pump = |cat: &mut CursorCat, t: Instant, upto_ms: u64| {
            let mut f = cat.frame(t + Duration::from_millis(1000));
            let mut ms = 1000;
            while ms < upto_ms {
                ms = (ms + 100).min(upto_ms);
                f = cat.frame(t + Duration::from_millis(ms));
            }
            f
        };
        let (mut a, t) = build();
        let open = pump(&mut a, t, 1000);
        assert!(open.alpha > 0, "lingering during the hold");
        assert_eq!(
            open.pose.eyes,
            EyesFrame::Open,
            "no blink outside the window"
        );
        let (mut b, t) = build();
        let blink = pump(&mut b, t, 1850);
        assert_eq!(
            blink.pose.eyes,
            EyesFrame::Blink,
            "the blink lands in its deterministic window"
        );
        let (mut c, t) = build();
        let again = pump(&mut c, t, 1850);
        assert_eq!(
            blink.pose.eyes, again.pose.eyes,
            "blink is a pure clock function"
        );
    }

    #[test]
    fn sustained_speed_squints_happily() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let mut f = cat.frame(t + Duration::from_millis(16));
        for i in 0..24 {
            let ti = t + Duration::from_millis(40 + i * 16);
            cat.on_key(ti, true);
            f = cat.frame(ti);
        }
        assert_eq!(
            f.pose.eyes,
            EyesFrame::Happy,
            "a fast sustained cruise squints, eyes were {:?}",
            f.pose.eyes
        );
    }

    #[test]
    fn delete_burst_is_one_sustained_oops_with_one_kick() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let _ = cat.frame(t + Duration::from_millis(16));
        let t0 = t + Duration::from_millis(200);
        cat.on_key(t0, false);
        let hit = cat.frame(t0);
        assert_eq!(hit.reaction, CatReaction::Startled);
        assert!(
            hit.pose.scale_y < 0.85,
            "the burst's first delete kicks a visible squash, got {}",
            hit.pose.scale_y
        );
        let mut last = t0;
        for i in 1..=6u64 {
            last = t0 + Duration::from_millis(i * 50);
            cat.on_key(last, false);
        }
        let mid = cat.frame(last);
        assert_eq!(
            mid.reaction,
            CatReaction::Startled,
            "the expression sustains across the burst"
        );
        assert_eq!(mid.render_look().variant, CatGlyphId::S121);
        assert!(
            mid.pose.scale_y > 0.85,
            "a repeat key must NOT re-freeze the bounce at impact, got {}",
            mid.pose.scale_y
        );
        let again = last + Duration::from_millis(500);
        cat.on_key(again, false);
        let rekick = cat.frame(again);
        assert!(
            rekick.pose.scale_y < 0.85,
            "a new burst re-arms the squash kick, got {}",
            rekick.pose.scale_y
        );
    }

    #[test]
    fn oops_holds_visibly_then_decays_to_base_look() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let _ = cat.frame(t + Duration::from_millis(16));
        let del = t + Duration::from_millis(300);
        cat.on_key(del, false);
        for ms in [100u64, 200, 300] {
            let _ = cat.frame(del + Duration::from_millis(ms));
        }
        let held = cat.frame(del + Duration::from_millis(400));
        assert_eq!(held.reaction, CatReaction::Startled);
        assert_eq!(held.render_look().variant, CatGlyphId::S121);
        assert!(
            held.pose.lead < -0.05,
            "mid-hold the body recoils backward, got lead {}",
            held.pose.lead
        );
        let settled = cat.frame(del + Duration::from_millis(700));
        assert_eq!(settled.reaction, CatReaction::Cruise);
        assert_eq!(settled.render_look(), KittyLook::default().normalized());
        assert!(
            settled.pose.lead > -0.02,
            "the backward recoil releases with the expression, got lead {}",
            settled.pose.lead
        );
    }

    #[test]
    fn kill_chords_fire_the_same_oops_and_never_summon() {
        let t = Instant::now();
        let mut hidden = CursorCat::default();
        hidden.on_kill(t);
        assert!(!hidden.is_active(), "a kill can never summon");
        assert_eq!(hidden.frame(t + Duration::from_millis(16)).alpha, 0);

        let mut live = CursorCat::default();
        live.on_collect(t, KittyLook::default());
        let _ = live.frame(t + Duration::from_millis(16));
        live.on_kill(t + Duration::from_millis(200));
        let f = live.frame(t + Duration::from_millis(210));
        assert_eq!(f.reaction, CatReaction::Startled);
        assert_eq!(f.render_look().variant, CatGlyphId::S121);
        assert_eq!(f.render_look().accessory, None);
    }

    #[test]
    fn a_kill_flood_grounds_the_flight_and_the_goodbye_keeps_the_oops() {
        let t = Instant::now();
        let mut c = CursorCat::default();
        for i in 0..120 {
            c.on_key(t + Duration::from_millis(i * 60), true);
        }
        assert!(c.is_active(), "the run summons the cat");
        let last = t + Duration::from_millis(119 * 60);
        let before = c.momentum(last);
        let kill1 = last + Duration::from_millis(100);
        c.on_kill(kill1);
        assert!(
            c.momentum(kill1) < before && c.momentum(kill1) > LOW,
            "one kill dents the metric without grounding: {} -> {}",
            before,
            c.momentum(kill1)
        );
        assert!(
            c.frame(kill1 + Duration::from_millis(50)).fade_out == 0.0,
            "a single kill does not ground an earned flight"
        );
        let mut kill = kill1;
        for _ in 0..8 {
            kill += Duration::from_millis(80);
            c.on_kill(kill);
        }
        assert!(c.momentum(kill) < LOW, "the flood walks the metric low");
        let f = c.frame(kill + Duration::from_millis(100));
        assert!(
            f.fade_out > 0.0,
            "the flood grounds the flight, so the fade begins"
        );
        assert_eq!(
            f.reaction,
            CatReaction::Startled,
            "the goodbye carries the oops"
        );
        assert_eq!(f.render_look().variant, CatGlyphId::S121);
    }

    #[test]
    fn reduced_motion_oops_is_a_static_swap_with_a_decay_wake() {
        let t = Instant::now();
        let look = KittyLook {
            variant: CatGlyphId::S103,
            coat: 7,
            ..KittyLook::default()
        };
        let mut cat = CursorCat::default();
        cat.on_collect(t, look);
        let del = t + Duration::from_millis(500);
        cat.on_key(del, false);
        let f = cat.static_frame(del + Duration::from_millis(100));
        assert_eq!(f.alpha, 255);
        assert_eq!(f.reaction, CatReaction::Startled);
        assert_eq!(f.render_look().variant, CatGlyphId::S121);
        assert_eq!(f.pose, CatPose::STILL, "no pose kick under reduced motion");
        assert_eq!(f.bob, 0.0);
        assert_eq!(
            cat.static_deadline(del + Duration::from_millis(100)),
            Some(del + Duration::from_secs_f32(OOPS_HOLD)),
            "the oops decay arms its own wake (it precedes the erase deadline)"
        );
        let back = cat.static_frame(del + Duration::from_millis(700));
        assert_eq!(back.reaction, CatReaction::Discovery);
        assert_eq!(back.render_look(), look.normalized());
        assert_eq!(back.pose, CatPose::STILL);
        assert_eq!(
            cat.static_deadline(del + Duration::from_millis(700)),
            Some(t + Duration::from_secs_f32(DISCOVERY_HOLD)),
            "after the decay only the single erase deadline is armed"
        );
    }

    #[test]
    fn hidden_settles_to_zero_work() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        let idle = cat.frame(t);
        assert_eq!(idle.alpha, 0);
        assert_eq!(idle.fp(), 0, "hidden ⇒ fp settles to 0");
        assert_eq!(idle.pose, CatPose::STILL, "hidden ⇒ neutral pose");
        assert!(!cat.is_active(), "hidden ⇒ the host 60 fps timer disarms");

        cat.on_collect(t, KittyLook::default());
        let _ = cat.frame(t + Duration::from_millis(16));
        let _ = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.05));
        let done = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + FADE_OUT + 0.6));
        assert_eq!(done.alpha, 0, "the flight fades out completely");
        assert_eq!(done.fp(), 0, "settled hidden ⇒ fp 0");
        assert_eq!(done.pose, CatPose::STILL, "settled hidden ⇒ neutral pose");
        assert!(!cat.is_active());
        let after = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + FADE_OUT + 2.0));
        assert_eq!(
            after.fp(),
            0,
            "a later hidden frame stays byte-identical off"
        );
        assert_eq!(after.pose, CatPose::STILL);
    }

    const fn sync(drive: f32, beat: f32) -> SingSync {
        SingSync { drive, beat }
    }

    fn arm_singing_after_travel(c: &mut CursorCat, t: Instant) -> Instant {
        c.set_singing(t, sync(1.0, 0.0));
        assert!(!c.is_active(), "an armed hold still owes cursor travel");
        let mut armed_at = t;
        for i in 1..=MIN_RUN_KEYS {
            armed_at = t + Duration::from_millis(u64::from(i) * 40);
            c.on_key(armed_at, true);
            c.set_singing(armed_at, sync(1.0, i as f32 * 0.1));
        }
        assert!(
            c.is_active(),
            "the singing bypass may summon after sixteen qualifying events"
        );
        armed_at
    }

    #[test]
    fn singing_pins_momentum_summons_and_dances_on_the_beat() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        assert_eq!(c.momentum(armed_at), 1.0, "the documented momentum bypass");
        let t1 = armed_at + Duration::from_secs_f32(FADE_IN + 0.02);
        c.set_singing(t1, sync(1.0, 2.02));
        let on_beat = c.frame(t1);
        assert_eq!(on_beat.alpha, 255);
        assert_eq!(on_beat.sing, 1.0);
        assert_eq!(
            on_beat.render_look().variant,
            CatGlyphId::S115,
            "the roster's open-mouth meow head IS the singing face"
        );
        let t2 = armed_at + Duration::from_secs_f32(FADE_IN + 0.2);
        c.set_singing(t2, sync(1.0, 2.47));
        let mid_beat = c.frame(t2);
        assert!(
            on_beat.pose.scale_y < mid_beat.pose.scale_y,
            "the on-beat squash pulse must relax across the beat ({} vs {})",
            on_beat.pose.scale_y,
            mid_beat.pose.scale_y
        );
        assert_ne!(
            on_beat.bob, mid_beat.bob,
            "the dance bob rides the beat clock"
        );
    }

    #[test]
    fn the_singing_head_never_blinks_at_the_gate_value() {
        for drive in [0.33_f32, 0.3300001, 0.34, 0.5, 1.0] {
            let mut c = CursorCat::default();
            let t = Instant::now();
            let armed_at = arm_singing_after_travel(&mut c, t);
            let mut singing_head = 0;
            let mut blinked_singing = 0;
            for i in 0..190 {
                let now = armed_at + Duration::from_secs_f32(FADE_IN + 0.016 * i as f32);
                c.set_singing(now, sync(drive, 0.0));
                let f = c.frame(now);
                if f.alpha != 0 && f.render_look().variant == CatGlyphId::S115 {
                    singing_head += 1;
                    blinked_singing += u32::from(f.pose.eyes == EyesFrame::Blink);
                }
            }
            assert!(
                singing_head > 0,
                "drive={drive}: the fixture never reached the singing head"
            );
            assert_eq!(
                blinked_singing, 0,
                "drive={drive}: the open-mouth singing head blinked"
            );
        }
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        let mut blinks = 0;
        for i in 0..600 {
            let now = armed_at + Duration::from_secs_f32(FADE_IN + 0.016 * i as f32);
            c.set_singing(now, sync(0.0, 0.0));
            blinks += u32::from(c.frame(now).pose.eyes == EyesFrame::Blink);
        }
        assert!(blinks > 0, "fixture: a cat off the song must still blink");
    }

    #[test]
    fn oops_outranks_the_singing_face() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        let t1 = armed_at + Duration::from_secs_f32(FADE_IN + 0.05);
        c.set_singing(t1, sync(1.0, 1.1));
        c.on_key(t1, false);
        let f = c.frame(t1 + Duration::from_millis(16));
        assert_eq!(f.render_look().variant, CatGlyphId::S121, "oops wins");
    }

    #[test]
    fn wind_down_crossfades_the_dance_out() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        let t1 = armed_at + Duration::from_secs_f32(FADE_IN + 0.05);
        c.set_singing(t1, sync(0.5, 3.05));
        let half = c.frame(t1);
        assert_eq!(half.sing, 0.5);
        assert_eq!(
            half.render_look().variant,
            CatGlyphId::S115,
            "half drive still sings"
        );
        let t2 = t1 + Duration::from_millis(100);
        c.set_singing(t2, sync(0.0, 0.0));
        let off = c.frame(t2);
        assert_eq!(off.sing, 0.0);
        assert_ne!(
            off.render_look().variant,
            CatGlyphId::S115,
            "at zero drive the singing face hands back to the collected look"
        );
        assert!(
            c.momentum(t2) > 0.7,
            "the bypassed metric hands back to NATURAL decay (still high right after): {}",
            c.momentum(t2)
        );
    }

    #[test]
    fn reduced_motion_celebration_is_a_static_singing_pose() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        c.set_singing(armed_at, sync(1.0, 5.3));
        let f = c.static_frame(armed_at);
        assert_eq!(f.alpha, 255, "static celebration presents fully opaque");
        assert_eq!(f.pose, CatPose::STILL, "no dance loop under reduced motion");
        assert_eq!(f.bob, 0.0, "no bob under reduced motion");
        assert_eq!(f.render_look().variant, CatGlyphId::S115);
        let late = armed_at + Duration::from_millis(600);
        c.set_singing(late, sync(0.49, 6.3));
        let retained = c.static_frame(late);
        assert_eq!(retained.alpha, 255, "late wind-down keeps opaque custody");
        assert_eq!(retained.render_look().variant, CatGlyphId::S115);
        let t1 = armed_at + Duration::from_millis(800);
        c.set_singing(t1, sync(0.3, 7.3));
        let off = c.static_frame(t1);
        assert_eq!(off.alpha, 0, "one-step disappearance, no fade animation");
    }

    #[test]
    fn pausing_a_collection_hello_shifts_delight_oops_and_landing_clocks_on_resume() {
        let mut c = CursorCat::default();
        let t0 = Instant::now();
        c.on_collect(t0, KittyLook::default());
        assert!(c.on_delight(t0, 2));
        let t_pause = t0 + Duration::from_millis(40);
        let before_pause = c.frame(t_pause);
        assert_eq!(before_pause.reaction, CatReaction::Delight);
        let u_before = c.delight_arc(t_pause);
        assert!(
            (0.0..1.0).contains(&u_before),
            "summon delight hop is mid-arc before pause: {u_before}"
        );

        c.set_collection_presentable(t_pause, false);
        let t_resume = t_pause + Duration::from_secs(3);
        c.set_collection_presentable(t_resume, true);

        let u_resume = c.delight_arc(t_resume);
        assert!(
            (u_resume - u_before).abs() < 1e-4,
            "resuming after 3s hidden preserves exact delight hop progress ({u_before} vs {u_resume})"
        );
        let after_resume = c.frame(t_resume + Duration::from_millis(16));
        assert_eq!(after_resume.reaction, CatReaction::Delight);
        assert!(
            c.delight_arc(t_resume + Duration::from_millis(16)) < 1.0,
            "delight hop envelope continues smoothly after resume instead of snapping expired"
        );
    }

    #[test]
    fn pausing_a_collection_hello_shifts_fold_and_meteor_and_collect_clears_stale_touchdown() {
        use crate::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
        use crate::rainbow_kitty::Dir;

        let mut c = CursorCat::default();
        let t0 = Instant::now();
        c.on_collect(t0, KittyLook::default());
        c.on_motion_pulse(CursorCatMotionPulse {
            at: t0,
            kind: CursorCatMotionKind::FoldForward,
        });
        let mut frame = c.frame(t0);
        c.on_meteor(
            Flight {
                dir: Dir::Right,
                t0,
                land_at: t0 + Duration::from_millis(120),
                land: (2, 10),
            },
            t0,
            &mut frame,
        );
        let t_pause = t0 + Duration::from_millis(40);
        let fold_before = c
            .placement_frame(t_pause)
            .fold
            .expect("fold active before pause")
            .progress;
        c.set_collection_presentable(t_pause, false);

        let t_mid_pause = t_pause + Duration::from_secs(2);
        let fold_paused = c
            .placement_frame(t_mid_pause)
            .fold
            .expect("fold remains frozen while collection hello is paused")
            .progress;
        assert!((fold_paused - fold_before).abs() < 1e-6);

        let t_resume = t_pause + Duration::from_secs(3);
        c.set_collection_presentable(t_resume, true);
        let fold_resumed = c
            .placement_frame(t_resume)
            .fold
            .expect("fold resumes at pre-pause progress")
            .progress;
        assert!((fold_resumed - fold_before).abs() < 1e-6);
        assert_eq!(
            c.land_at(),
            Some(t0 + Duration::from_millis(120) + Duration::from_secs(3)),
            "meteor landing clock shifts by hidden span"
        );
        assert!(
            c.meteor.is_some_and(|m| m.land_at == c.land_at().unwrap()),
            "meteor flight struct shifts in lockstep with land_at"
        );

        c.on_motion_pulse(CursorCatMotionPulse {
            at: t_resume,
            kind: CursorCatMotionKind::FoldReverse,
        });
        assert!(c.placement_frame(t_resume).facing_left);
        let t_recollect = t_resume + Duration::from_millis(20);
        c.on_collect(t_recollect, KittyLook::default());
        assert_eq!(c.land_at(), None);
        assert!(c.meteor.is_none());
        let recollected_pf = c.placement_frame(t_recollect);
        assert!(
            recollected_pf.fold.is_none() && !recollected_pf.facing_left,
            "on_collect must reset stale fold and reverse facing"
        );

        let mut reduced = CursorCat::default();
        let hello_look = KittyLook::for_app("claude");
        let parked_look = KittyLook::for_app("vim");
        assert_ne!(hello_look, parked_look);
        reduced.on_collect(t0, hello_look);
        reduced.set_look(parked_look);
        assert_eq!(reduced.static_frame(t0).look, hello_look);
        let retired = reduced.static_frame(t0 + Duration::from_secs(5));
        assert_eq!(retired.alpha, 0);
        assert_eq!(
            retired.look, parked_look,
            "static_frame collection-hello retirement must install parked pending_look"
        );
        assert_eq!(reduced.look, parked_look);
    }

    #[test]
    fn delight_chain_pulses_multiple_beats_with_scaled_swell() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(120);
        let mut base = CursorCat::default();
        let mut single = CursorCat::default();
        let mut chained = CursorCat::default();
        for cat in [&mut base, &mut single, &mut chained] {
            cat.on_collect(t1, KittyLook::default());
            cat.state = State::Shown;
        }

        single.on_delight(t1, 1);
        chained.on_delight(t0, 1);
        chained.on_delight(t1, 1);
        assert_eq!(single.delight_chain, 1);
        assert_eq!(chained.delight_chain, 2);

        let quarter = Duration::from_secs_f32(DELIGHT_HOLD * 0.25);
        let half = Duration::from_secs_f32(DELIGHT_HOLD * 0.5);
        let three_quarter = Duration::from_secs_f32(DELIGHT_HOLD * 0.75);

        let base_q = base.frame(t1 + quarter);
        let single_q = single.frame(t1 + quarter);
        let chained_q = chained.frame(t1 + quarter);
        assert!(
            chained_q.pose.scale_y > single_q.pose.scale_y
                && single_q.pose.scale_y > base_q.pose.scale_y,
            "chained delight first beat swells higher than a single delight at quarter hold"
        );

        let base_h = base.frame(t1 + half);
        let single_h = single.frame(t1 + half);
        let chained_h = chained.frame(t1 + half);
        assert!(
            (chained_h.pose.scale_y - base_h.pose.scale_y).abs() < 1e-5
                && single_h.pose.scale_y > base_h.pose.scale_y + 0.01,
            "chained delight dips to baseline between beats at mid-hold: chained={} base={}",
            chained_h.pose.scale_y,
            base_h.pose.scale_y
        );

        let base_tq = base.frame(t1 + three_quarter);
        let single_tq = single.frame(t1 + three_quarter);
        let chained_tq = chained.frame(t1 + three_quarter);
        assert!(
            chained_tq.pose.scale_y > single_tq.pose.scale_y
                && single_tq.pose.scale_y > base_tq.pose.scale_y,
            "chained delight rises for its second beat at three-quarter hold"
        );
    }

    #[test]
    fn on_collect_during_fadeout_reseeds_spine_to_open_banked() {
        let mut t = Instant::now();
        let mut from_hidden = CursorCat::default();
        let mut from_fadeout = CursorCat::default();
        let look_a = KittyLook::for_app("claude");
        let look_b = KittyLook::for_app("vim");

        from_fadeout.on_collect(t, look_a);
        let until = t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.10);
        while t < until {
            t += Duration::from_millis(16);
            let _ = from_fadeout.frame(t);
        }
        let fading = from_fadeout.frame(t);
        assert!(matches!(from_fadeout.state, State::FadeOut(_)));
        assert!(fading.alpha > 0);
        assert_eq!(fading.pose.eyes, EyesFrame::Open);

        from_hidden.on_collect(t, look_b);
        from_fadeout.on_collect(t, look_b);
        let f_hidden = from_hidden.frame(t);
        let f_fadeout = from_fadeout.frame(t);
        assert_eq!(f_fadeout.pose.eyes, EyesFrame::Happy);
        assert_eq!(f_fadeout.pose.lead, f_hidden.pose.lead);
        assert_eq!(from_fadeout.disp, 1.0);
    }

    #[test]
    fn hidden_frames_skip_stride_work_and_on_collect_syncs_stride() {
        let t0 = Instant::now();
        let mut cat = CursorCat::default();
        assert_eq!(cat.stride_at, None);

        let f1 = cat.frame(t0);
        let f2 = cat.frame(t0 + Duration::from_millis(16));
        assert_eq!((f1.alpha, f1.bob, f1.pose), (0, 0.0, CatPose::STILL));
        assert_eq!((f2.alpha, f2.bob, f2.pose), (0, 0.0, CatPose::STILL));
        assert_eq!(cat.stride_at, None);
        assert_eq!(cat.stride, 0.0);
        assert_eq!(cat.stride_shown, 0.0);

        cat.on_key(t0 + Duration::from_millis(32), true);
        cat.on_key(t0 + Duration::from_millis(64), true);
        let stride_before = cat.stride;
        let shown_before = cat.stride_shown;
        assert!(stride_before > shown_before);
        let _ = cat.frame(t0 + Duration::from_millis(80));
        assert_eq!(cat.stride, stride_before);
        assert_eq!(cat.stride_shown, shown_before);

        cat.on_collect(t0 + Duration::from_millis(96), KittyLook::for_app("claude"));
        assert_eq!(cat.stride_at, None);
        assert_eq!(cat.stride_shown, cat.stride);
    }

    #[test]
    fn resample_nearest_into_reuses_capacity_and_matches_resample_nearest() {
        let src: [u8; 16] = [
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
        ];
        let mut buf = Vec::with_capacity(256);
        let ptr_before = buf.as_ptr();
        let cap_before = buf.capacity();

        resample_nearest_into(&src, 2, 2, 4, 3, &mut buf);
        assert_eq!(buf, resample_nearest(&src, 2, 2, 4, 3));
        assert_eq!(buf.as_ptr(), ptr_before);
        assert_eq!(buf.capacity(), cap_before);

        resample_nearest_into(&src, 2, 2, 2, 2, &mut buf);
        assert_eq!(&buf[..], &src[..]);
        assert_eq!(buf.as_ptr(), ptr_before);

        resample_nearest_into(&src, 2, 2, 0, 3, &mut buf);
        assert!(buf.is_empty());
        assert_eq!(buf.capacity(), cap_before);
    }
}
