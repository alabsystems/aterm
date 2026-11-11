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
const FADE_IN_DUR: Duration = Duration::from_millis(350);
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
#[inline]
const fn dur_from_secs_f32(secs: f32) -> Duration {
    Duration::from_nanos((secs as f64 * 1_000_000_000.0).round() as u64)
}

const DISCOVERY_HOLD: f32 = 2.8;
const DISCOVERY_HOLD_DUR: Duration = dur_from_secs_f32(DISCOVERY_HOLD);
const CELEBRATE_HOLD: f32 = 0.7;
const CELEBRATE_HOLD_DUR: Duration = dur_from_secs_f32(CELEBRATE_HOLD);

const OOPS_HOLD: f32 = 0.60;
const OOPS_HOLD_DUR: Duration = dur_from_secs_f32(OOPS_HOLD);
const OOPS_REARM_DUR: Duration = Duration::from_millis(300);
const OOPS_RECOIL: f32 = 0.12;
const OOPS_SQUASH: f32 = 0.07;
const OOPS_ATTACK: f32 = 0.07;
const OOPS_SETTLE: f32 = 0.18;

const WINCE_HOLD: f32 = 0.52;
const WINCE_HOLD_DUR: Duration = dur_from_secs_f32(WINCE_HOLD);
const WINCE_CHAIN_WINDOW_DUR: Duration = Duration::from_millis(850);
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
const DELIGHT_CHAIN_WINDOW_DUR: Duration = Duration::from_millis(900);
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
    /// The last delete of the current burst: the [`OOPS_REARM_DUR`] gap decides
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
        let fold = match self.fold {
            Some(fold) => {
                const INV_CURSOR_FOLD_SECS: f32 = 1.0 / CURSOR_FOLD_DURATION.as_secs_f32();
                let progress = fold_now
                    .saturating_duration_since(fold.started)
                    .as_secs_f32()
                    * INV_CURSOR_FOLD_SECS;
                if progress < 1.0 {
                    Some(CatFoldFrame {
                        started: fold.started,
                        direction: fold.direction,
                        progress: progress.clamp(0.0, 1.0),
                    })
                } else {
                    None
                }
            }
            None => None,
        };
        let facing_left = match fold {
            Some(active) => matches!(active.direction, CatFoldDirection::Reverse),
            None => self.facing_left,
        };
        CursorCatPlacementFrame {
            sampled_at: now,
            fold,
            facing_left,
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
        let dt = match self.last {
            Some(l) => now.saturating_duration_since(l).as_secs_f32().min(0.5),
            None => 0.0,
        };
        if forward {
            let (held, next_momentum) = self.momentum.advance_reading(now);
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
                next_stride - (next_stride * (1.0 / 1024.0)).floor() * 1024.0
            };
            if let State::FadeOut(t0) = self.state
                && !self.collection_hello
                && (ordinary_flight_owned || self.sing > 0.0)
                && next_momentum >= REVIVE_GATE
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
    /// ([`OOPS_REARM_DUR`]).
    #[inline]
    fn note_oops(&mut self, now: Instant) {
        if !self.is_active() {
            return;
        }
        let same_burst = if self.reaction == CatReaction::Startled
            && let Some(t) = self.oops_last
        {
            now.saturating_duration_since(t) < OOPS_REARM_DUR
        } else {
            false
        };
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
        let chained = match self.wince_last {
            Some(last) => now.saturating_duration_since(last) < WINCE_CHAIN_WINDOW_DUR,
            None => false,
        };
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
        frame.pose = self.resolve_pose(now, self.decayed(now), frame.reaction, frame.exit, None);
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
        frame.pose = self.resolve_pose(now, self.decayed(now), reaction, frame.exit, None);
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
        let chained = match self.delight_last {
            Some(last) => now.saturating_duration_since(last) < DELIGHT_CHAIN_WINDOW_DUR,
            None => false,
        };
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
    #[inline]
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
    #[inline]
    pub fn set_collection_presentable(&mut self, now: Instant, presentable: bool) {
        if !self.collection_hello {
            self.collection_paused_at = None;
            return;
        }
        if !presentable {
            if self.collection_paused_at.is_none() {
                self.collection_paused_at = Some(now);
            }
            return;
        }
        let Some(paused_at) = self.collection_paused_at.take() else {
            return;
        };
        let hidden = now.saturating_duration_since(paused_at);
        if hidden.is_zero() {
            return;
        }
        if let Some(at) = &mut self.discovery_until {
            *at += hidden;
        }
        if let Some(at) = &mut self.reaction_until {
            *at += hidden;
        }
        if let Some(at) = &mut self.wince_at {
            *at += hidden;
        }
        if let Some(at) = &mut self.wince_last {
            *at += hidden;
        }
        if let Some(at) = &mut self.delight_at {
            *at += hidden;
        }
        if let Some(at) = &mut self.delight_last {
            *at += hidden;
        }
        if let Some(at) = &mut self.oops_at {
            *at += hidden;
        }
        if let Some(at) = &mut self.oops_last {
            *at += hidden;
        }
        if let Some(at) = &mut self.land_at {
            *at += hidden;
        }
        if let Some(m) = &mut self.meteor {
            m.t0 += hidden;
            m.land_at += hidden;
        }
        if let Some(f) = &mut self.fold {
            f.started += hidden;
        }
        if let Some(at) = &mut self.disp_at {
            *at += hidden;
        }
        if let Some(at) = &mut self.stride_at {
            *at += hidden;
        }
        if let Some(at) = &mut self.flight {
            *at += hidden;
        }
        match &mut self.state {
            State::FadeIn(at) | State::FadeOut(at) => *at += hidden,
            State::Hidden | State::Shown => {}
        }
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
        match self.discovery_until {
            Some(until) => now < until,
            None => false,
        }
    }

    #[inline]
    fn reaction_at(&mut self, now: Instant) -> CatReaction {
        if let Some(until) = self.reaction_until
            && now >= until
        {
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
        let dt = match self.stride_at {
            Some(l) => now.saturating_duration_since(l).as_secs_f32().min(0.10),
            None => 0.0,
        };
        self.stride_at = Some(now);
        if dt > 0.0 {
            #[inline]
            const fn wrap(x: f32) -> f32 {
                if x >= 0.0 && x < 1024.0 {
                    x
                } else {
                    x - (x * (1.0 / 1024.0)).floor() * 1024.0
                }
            }
            self.stride = wrap(self.stride + dt * STRIDE_IDLE_HZ);
            let mut gap = wrap(self.stride - self.stride_shown + 512.0) - 512.0;
            if gap >= 1.0 {
                self.stride_shown = wrap(self.stride_shown + gap.floor());
                gap = gap.fract();
            }
            if gap != 0.0 {
                const INV_STRIDE_TAU: f32 = 1.0 / STRIDE_TAU;
                let k = 1.0 - crate::color_math::exp_f32(-dt * INV_STRIDE_TAU);
                let step = (gap * k).clamp(-STRIDE_RATE_MAX * dt, STRIDE_RATE_MAX * dt);
                self.stride_shown = wrap(self.stride_shown + step);
            }
        }
    }

    /// The airborne hover-bob (fraction of cell height) and stride cosine, a pure read of the stride pump.
    #[inline]
    const fn bob_and_stride_cos(&self) -> (f32, Option<f32>) {
        match self.flight {
            Some(_) => {
                let (sin, cos) =
                    crate::supernova::sin_cos_f32(std::f32::consts::TAU * self.stride_shown);
                (sin * BOB_AMP, Some(cos))
            }
            None => (0.0, Some(0.0)),
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
                if self.disp != target {
                    let dt = now.saturating_duration_since(prev).as_secs_f32().min(0.1);
                    if dt > 0.0 {
                        const INV_DISP_TAU: f32 = 1.0 / DISP_TAU;
                        let alpha = 1.0 - crate::color_math::exp_f32(-dt * INV_DISP_TAU);
                        self.disp += (target - self.disp) * alpha;
                    }
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

    #[inline]
    fn resolve_pose(
        &mut self,
        now: Instant,
        target: f32,
        reaction: CatReaction,
        exit: CatExit,
        stride_cos: Option<f32>,
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
        if self.flight.is_some() && bank > 0.0 {
            let cos = match stride_cos {
                Some(c) => c,
                None => crate::supernova::sin_cos_f32(std::f32::consts::TAU * self.stride_shown).1,
            };
            if cos > 0.0 {
                let foot = cos * bank;
                scale_y *= 1.0 - STRIDE_SQUASH * foot;
                scale_x *= 1.0 + STRIDE_SQUASH * 0.7 * foot;
            }
        }
        if let Some(t0) = self.land_at
            && now >= t0
        {
            const INV_LAND_DUR: f32 = 1.0 / LAND_DUR;
            let u = now.saturating_duration_since(t0).as_secs_f32() * INV_LAND_DUR;
            if u >= 1.0 {
                self.land_at = None;
            } else {
                let q = LAND_AMP
                    * crate::color_math::exp_f32(-LAND_DECAY * u)
                    * crate::supernova::sin_cos_f32(std::f32::consts::TAU * LAND_FREQ * u).1;
                scale_y *= 1.0 - q;
                scale_x *= 1.0 + q * 0.6;
            }
        }
        match reaction {
            CatReaction::Startled => {
                const INV_OOPS_ATTACK: f32 = 1.0 / OOPS_ATTACK;
                const INV_OOPS_SETTLE: f32 = 1.0 / OOPS_SETTLE;
                let attack = match self.oops_at {
                    Some(t0) => smoothstep_0(
                        INV_OOPS_ATTACK,
                        now.saturating_duration_since(t0).as_secs_f32(),
                    ),
                    None => 1.0,
                };
                let settle = match self.reaction_until {
                    Some(until) => smoothstep_0(
                        INV_OOPS_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    ),
                    None => 1.0,
                };
                let env = attack * settle;
                lead = lead * (1.0 - env) - OOPS_RECOIL * env;
                scale_y *= 1.0 - OOPS_SQUASH * env;
                scale_x *= 1.0 + OOPS_SQUASH * 0.6 * env;
            }
            CatReaction::Wince => {
                const INV_WINCE_HOLD: f32 = 1.0 / WINCE_HOLD;
                const INV_WINCE_ATTACK: f32 = 1.0 / WINCE_ATTACK;
                const INV_WINCE_SETTLE: f32 = 1.0 / WINCE_SETTLE;
                let elapsed = match self.wince_at {
                    Some(t0) => now.saturating_duration_since(t0).as_secs_f32(),
                    None => 0.0,
                };
                let attack = smoothstep_0(INV_WINCE_ATTACK, elapsed);
                let settle = match self.reaction_until {
                    Some(until) => smoothstep_0(
                        INV_WINCE_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    ),
                    None => 1.0,
                };
                let env_base = attack * settle;
                if env_base > 0.0 {
                    let chain = self.wince_chain.max(1);
                    let phase = elapsed * INV_WINCE_HOLD * f32::from(chain);
                    let pulse = 0.78
                        + 0.22
                            * crate::supernova::sin_cos_f32(std::f32::consts::TAU * phase)
                                .1
                                .abs();
                    let strength = 1.0 + 0.18 * f32::from(chain.saturating_sub(1));
                    let env = env_base * pulse * strength;
                    lead = lead * (1.0 - env.min(1.0)) - WINCE_RECOIL * env;
                    scale_y *= 1.0 - WINCE_SQUASH * env;
                    scale_x *= 1.0 + WINCE_SQUASH * 0.7 * env;
                    let omega =
                        WINCE_TUMBLE_OMEGA[(chain as usize - 1).min(MAX_WINCE_CHAIN as usize - 1)];
                    let swing = crate::color_math::exp_f32(-WINCE_TUMBLE_DECAY * elapsed)
                        * crate::supernova::sin_cos_f32(omega * elapsed).0;
                    let swing_env = swing * env_base;
                    lead += WINCE_TUMBLE * swing_env;
                    let wobble = 0.05 * swing_env;
                    scale_x *= 1.0 + wobble;
                    scale_y *= 1.0 - wobble;
                }
            }
            CatReaction::Delight => {
                const INV_DELIGHT_HOLD: f32 = 1.0 / DELIGHT_HOLD;
                const INV_DELIGHT_ATTACK: f32 = 1.0 / DELIGHT_ATTACK;
                const INV_DELIGHT_SETTLE: f32 = 1.0 / DELIGHT_SETTLE;
                let elapsed = match self.delight_at {
                    Some(t0) => now.saturating_duration_since(t0).as_secs_f32(),
                    None => 0.0,
                };
                let attack = smoothstep_0(INV_DELIGHT_ATTACK, elapsed);
                let settle = match self.reaction_until {
                    Some(until) => smoothstep_0(
                        INV_DELIGHT_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    ),
                    None => 1.0,
                };
                let env = attack * settle;
                if env > 0.0 {
                    let arc = (elapsed * INV_DELIGHT_HOLD).clamp(0.0, 1.0);
                    let chain = self.delight_chain.max(1);
                    let strength = 1.0 + 0.16 * f32::from(chain.saturating_sub(1));
                    let pulse = crate::supernova::sin_cos_f32(
                        std::f32::consts::PI * arc * f32::from(chain),
                    )
                    .0
                    .abs();
                    let stretch = pulse * env * strength;
                    scale_y *= 1.0 + DELIGHT_STRETCH * 0.33 * stretch;
                    scale_x *= 1.0 - DELIGHT_STRETCH * 0.18 * stretch;
                }
            }
            CatReaction::Cruise | CatReaction::Celebrate | CatReaction::Discovery => {}
        }
        if self.sing > 0.0 {
            let one_minus_u = 1.0 - self.sing_beat.fract();
            let pulse_sing = one_minus_u * one_minus_u * self.sing;
            scale_y *= 1.0 - SING_SQUASH * pulse_sing;
            scale_x *= 1.0 + SING_SQUASH * 0.7 * pulse_sing;
            lead += SING_SWAY
                * crate::supernova::sin_cos_f32(std::f32::consts::PI * self.sing_beat).0
                * self.sing;
        }
        let eyes = match reaction {
            CatReaction::Wince => EyesFrame::Blink,
            CatReaction::Delight => EyesFrame::Happy,
            CatReaction::Celebrate | CatReaction::Startled => EyesFrame::Open,
            CatReaction::Cruise | CatReaction::Discovery if !matches!(exit, CatExit::Plain) => {
                EyesFrame::Open
            }
            CatReaction::Cruise | CatReaction::Discovery => {
                if matches!(self.meteor, Some(m) if now < m.squint_until()) {
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
    #[inline]
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
                    } else if now.saturating_duration_since(t0) >= FADE_IN_DUR {
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
        let d0 = match self.discovery_until {
            Some(d) if d > now => Some(d),
            _ => None,
        };
        let d1 = match self.reaction_until {
            Some(d)
                if matches!(self.reaction, CatReaction::Startled | CatReaction::Wince)
                    && d > now =>
            {
                Some(d)
            }
            _ => None,
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
    #[inline]
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
        if !discovery && grounded && matches!(self.state, State::FadeIn(_) | State::Shown) {
            self.grounded_exit(discovery_until, now);
        }
        match self.state {
            State::Hidden => hidden_frame(look),
            State::FadeIn(t0) => {
                const INV_FADE_IN: f32 = 1.0 / FADE_IN;
                let t =
                    (now.saturating_duration_since(t0).as_secs_f32() * INV_FADE_IN).clamp(0.0, 1.0);
                if t >= 1.0 {
                    self.state = State::Shown;
                }
                let (bob, stride_cos) = self.frame_bob();
                let pose = self.resolve_pose(now, s, reaction, CatExit::Plain, stride_cos);
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
                let (bob, stride_cos) = self.frame_bob();
                let pose = self.resolve_pose(now, s, reaction, CatExit::Plain, stride_cos);
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
                let (bob, stride_cos) = self.frame_bob();
                let pose = self.resolve_pose(now, s, reaction, self.exit, stride_cos);
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
    const fn frame_bob(&self) -> (f32, Option<f32>) {
        if self.sing <= 0.0 {
            self.bob_and_stride_cos()
        } else {
            let dance =
                crate::supernova::sin_cos_f32(std::f32::consts::TAU * self.sing_beat).0 * BOB_AMP;
            if self.sing >= 1.0 {
                (dance, None)
            } else {
                let (cruise, stride_cos) = self.bob_and_stride_cos();
                (cruise + (dance - cruise) * self.sing, stride_cos)
            }
        }
    }

    /// The shared grounded-exit edge of the FadeIn/Shown arms: a grounded
    /// non-discovery flight begins its fade-out — a collection hello's
    /// goodbye starts at the promised hold deadline, an earned flight at the
    /// analytic [`LOW`] crossing.
    #[inline]
    fn grounded_exit(&mut self, discovery_until: Option<Instant>, now: Instant) {
        let started = if self.collection_hello {
            discovery_until.unwrap_or(now)
        } else {
            self.low_crossing_at(now)
        };
        self.begin_fade_out_at(started);
    }

    /// Exact wall-clock instant at which an earned flight crossed [`LOW`],
    /// derived from the metric's decay ([`TypingMomentum::low_crossing`]) so
    /// the lifecycle is frame-rate independent.
    #[inline]
    fn low_crossing_at(&self, now: Instant) -> Instant {
        self.momentum.low_crossing(LOW).unwrap_or(now)
    }

    #[inline]
    const fn begin_fade_out_at(&mut self, started: Instant) {
        self.exit = if self.collection_hello {
            CatExit::Plain
        } else {
            self.roll_exit()
        };
        self.state = State::FadeOut(started);
        self.land_at = Some(started);
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
#[inline]
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
    if matches!(exit, CatExit::Plain) || fade_out <= 0.0 || fade_out >= 1.0 {
        return;
    }
    let ch = geom.ch as i32;
    let pop = crate::supernova::sin_cos_f32(fade_out * std::f32::consts::PI)
        .0
        .clamp(0.0, 1.0);
    if pop <= 0.0 {
        return;
    }
    match exit {
        CatExit::Plain => {}
        CatExit::HeartMeow => {
            const INV_14: f32 = 1.0 / 14.0;
            if dark_theme {
                let cov = (230.0 * pop) as u8;
                if cov == 0 {
                    return;
                }
                let u = (ch as f32 * INV_14).max(1.0) as i32;
                let hx = ax;
                let hy = ay - (fade_out * ch as f32 * 1.4) as i32;
                const ROWS: [(i32, i32); 6] = [(1, 2), (0, 6), (0, 6), (1, 5), (2, 4), (3, 3)];
                let pink = premul_rgb(EXIT_HEART_PINK, cov);
                let mut ry = 0;
                while ry < ROWS.len() {
                    let (x0, x1) = ROWS[ry];
                    push_fx_rect(
                        out,
                        geom,
                        hx + x0 * u,
                        hy + ry as i32 * u,
                        (x1 - x0 + 1) * u,
                        u,
                        pink,
                    );
                    ry += 1;
                }
                push_fx_rect(out, geom, hx + 4 * u, hy, 2 * u, u, pink);
            } else {
                let alpha = (210.0 * pop).min(InkRole::OverText.alpha_cap()) as u8;
                if alpha >= 24 {
                    let u = (ch as f32 * INV_14).max(1.0) as i32;
                    let hx = ax;
                    let hy = ay - (fade_out * ch as f32 * 1.4) as i32;
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
            let twinkle = 0.6 + 0.4 * crate::supernova::sin_cos_f32(fade_out * EXIT_STAR_SCINT).0;
            if dark_theme {
                let cov = (255.0 * pop * twinkle).clamp(0.0, 255.0) as u8;
                if cov == 0 {
                    return;
                }
                let sx = ax + geom.cw as i32;
                let sy = ay - ch / 3;
                let a = (ch / 5).max(2);
                const STAR_RGB: u32 = twinkle_rgb(false);
                let star = premul_rgb(STAR_RGB, cov);
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
                    premul_rgb(STAR_RGB, cov / 2),
                );
            } else {
                let alpha = (230.0 * pop * twinkle).clamp(0.0, InkRole::OverText.alpha_cap()) as u8;
                if alpha >= 24 {
                    let sx = ax + geom.cw as i32;
                    let sy = ay - ch / 3;
                    let a = (ch / 5).max(2);
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

/// Nearest-neighbour resample a straight-alpha RGBA8 image `src` (`sw×sh`) to `tw×th` into `out`, reusing its capacity.
#[inline]
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
    let mut ty = 0usize;
    while ty < th {
        let cur_sy = sy;
        sy += y_step;
        y_acc += y_rem;
        if y_acc >= th {
            y_acc -= th;
            sy += 1;
        }
        let row_d = ty * row_bytes;
        ty += 1;
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
        let mut tx = 0usize;
        while tx < dst_row.len() {
            dst_row[tx] = src_row[sx];
            tx += 1;
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
#[path = "kitty_cursor_tests.rs"]
mod tests;
