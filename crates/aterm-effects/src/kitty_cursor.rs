// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cat that flies in front of the cursor on a rainbow (the
//! [`crate::cursor_glow`] `rainbow kitty` ribbon is the wake). The cat is baked
//! by [`crate::cat_baker::CatBaker`] (the same renderer as the peeking
//! word-cats) with a happy face, or replaced by a user PNG
//! (`cursor_nyan_sprite`) resampled via [`resample_nearest`].
//!
//! [`CursorCat`] is the decision layer. The cat is earned: it fades in only
//! after the canonical typing-momentum metric
//! ([`crate::typing_momentum::TypingMomentum`]) has been held in its high band
//! ([`CAT_BAND`] for [`CAT_DWELL`], a few seconds of sustained non-delete
//! typing); a sustained run always summons. It rides while you type, survives
//! short breaths, revives out of a started fade, and fades out when you stall,
//! sometimes with a flourish (20% star wink, 20% heart meow, else plain).

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

/// Stable atlas key for a user kitty sprite tile, which the host bakes into the
/// shared free atlas via `CatBaker::host_tile`.
pub const HOST_ID: u64 = 0x6E79_616E;

// ── momentum + lifecycle tuning ─────────────────────────────────────────────
// The earn rule reads the canonical typing-momentum metric
// (`crate::typing_momentum`). It is rate-normalized (time spent typing, not
// key count), so the band is measured in seconds of sustained flow.
/// The high band: the metric value at/above which typing counts toward the
/// summon dwell. 0.65 is ~1.39 s of continuous typing from cold (build curve
/// `1.3·(1−e^(−t/2))`), and a clamped 1.0 metric stays in the band for 0.86 s
/// of silence, covering a normal inter-word breath. A steady 1 s/key peck
/// never enters the band (the cutoff is ~0.60 s/key).
const CAT_BAND: f32 = 0.65;
/// Seconds the metric must be held in the band (per-key credit capped at
/// [`SUSTAIN_CREDIT_MAX`]) before the cat is eligible: ~2.6 s of typing at
/// 25–60 ms/key; above ~135 ms/key the [`MIN_RUN_KEYS`] floor dominates. A key
/// landing under the band drains the dwell.
const CAT_DWELL: f32 = 1.2;
/// Per-key ceiling on dwell credit (seconds): the dwell is bought by keys
/// landing in rhythm, so one key after a long thought cannot claim the whole
/// gap as "sustained high typing".
const SUSTAIN_CREDIT_MAX: f32 = 0.25;
/// How fast the dwell drains while the metric sits under [`CAT_BAND`], as a
/// multiple of the elapsed gap: a one-second stall costs 1.5 s of dwell. A leak
/// rather than a hard reset, so a single inter-word breath does not discard
/// several seconds of real flow.
const SUSTAIN_LEAK: f32 = 1.5;
/// Minimum forward keystrokes in the current high-band run before the cat can
/// summon: an independent travel floor alongside the dwell.
pub const MIN_RUN_KEYS: u32 = 16;
/// Below this metric value the cat lets go and fades out: with the metric's
/// 2 s τ, a full run grounds after ~3.9 s of silence.
const LOW: f32 = 0.14;
/// Metric at/above which resumed typing revives a cat that already began its
/// fade-out, so a thinking pause does not force a full re-earn. About two quick
/// keys from the fade floor, far under [`CAT_BAND`]: reviving is cheap,
/// earning is not.
const REVIVE_GATE: f32 = 0.35;
/// Fade-in / fade-out durations (seconds).
const FADE_IN: f32 = 0.35;
const FADE_OUT: f32 = 0.75;

/// How far the goodbye must have faded before the exit flourish's baked head
/// is swapped in (see [`CatFrame::render_look`]). A quarter of [`FADE_OUT`]
/// (~190 ms), so the swap reads as part of the departure.
const EXIT_FACE_FADE: f32 = 0.25;
/// Total wall-clock time of the classic cursor kitty's off-glass line-fold
/// seam: enough 60 Hz samples to read the leave and arrive beats, while a
/// sparse frame past the bound lands immediately.
pub const CURSOR_FOLD_DURATION: Duration = Duration::from_millis(280);
/// Scintillation rate of the exit star wink, in radians across the fade-out.
///
/// Invariant: `EXIT_STAR_SCINT / FADE_OUT / TAU` Hz stays at or under the
/// crate's [`crate::effect_util::TWINKLE_FLASH_BOUND_HZ`], enforced at build
/// time and pinned by
/// `cat_exit_starwink_flash_rate_stays_under_the_photosensitivity_bound`.
const EXIT_STAR_SCINT: f32 = 13.0;
const _: () = assert!(
    EXIT_STAR_SCINT > 0.0
        && EXIT_STAR_SCINT / FADE_OUT / std::f32::consts::TAU
            <= crate::effect_util::TWINKLE_FLASH_BOUND_HZ,
    "the exit star wink must glint within the photosensitivity flash bound"
);
/// The collection hello's guaranteed, bounded hold. It is independent of cursor
/// style, so the user always sees the cat they chose.
const DISCOVERY_HOLD: f32 = 2.8;
/// Short input reactions never extend the companion's overall flight.
const CELEBRATE_HOLD: f32 = 0.7;

// ── the delete "oops" ───────────────────────────────────────────────────────
// A delete on a live companion swaps in the tongue-out wink (`CatGlyphId::S121`
// via `CatFrame::render_look`) with a recoil legible at 16 px, for every delete
// spelling (Backspace and kill chords); a key-repeat run reads as one sustained
// reaction, not a strobe.
/// How long the tongue-out expression holds after the last delete of a burst:
/// noticeable yet under [`CELEBRATE_HOLD`]. Like every reaction it never
/// extends the flight (momentum and fade clocks are untouched).
const OOPS_HOLD: f32 = 0.60;
/// Deletes closer together than this belong to one burst: each extends the
/// hold but does not re-fire the pose kick. Re-kicking on every repeat would
/// pin the landing bounce at its impact frame. Sized above autorepeat
/// (~30–90 ms) and fast tapping (~150–250 ms).
const OOPS_REARM: f32 = 0.30;
/// Backward recoil while the oops is live, as a fraction of cell width: the
/// cat rocks away from the deletion, since the glyph swap alone is easy to
/// miss at terminal size. Pose path only; reduced motion keeps the pure swap.
const OOPS_RECOIL: f32 = 0.12;
/// Sustained brace crouch while the oops holds (the `land_at` bounce decays
/// in ~0.4 s): shorter by this fraction, wider by 0.6× of it.
const OOPS_SQUASH: f32 = 0.07;
/// Recoil envelope: ease in over `OOPS_ATTACK` from the burst kick, ease out
/// over the final `OOPS_SETTLE` of the hold.
const OOPS_ATTACK: f32 = 0.07;
const OOPS_SETTLE: f32 = 0.18;

// ── the curse wince ────────────────────────────────────────────────────────
// A complete profanity cue makes a visible companion squeeze its eyes shut and
// recoil. Repeats inside one phrase chain: each re-kicks the pose and raises the
// force/pulse count, so `fuck fuck` reads as two beats. Only complete-token cues
// reach this API.
const WINCE_HOLD: f32 = 0.52;
const WINCE_CHAIN_WINDOW: f32 = 0.85;
const WINCE_ATTACK: f32 = 0.045;
const WINCE_SETTLE: f32 = 0.16;
const WINCE_RECOIL: f32 = 0.16;
const WINCE_SQUASH: f32 = 0.11;
const MAX_WINCE_CHAIN: u8 = 4;
/// The wince tumble: on top of the recoil the body rocks side to side in a
/// decaying oscillation whose frequency rides the chain. The pose transform
/// has no rotation, so the tumble is a lateral lead swing plus a counter-phase
/// scale wobble.
const WINCE_TUMBLE: f32 = 0.13;
const WINCE_TUMBLE_FREQ: f32 = 3.1;
const WINCE_TUMBLE_DECAY: f32 = 3.4;

// A complete feline word (any language) makes a visible companion light up:
// happy eyes (the leap is on the word's own cat, not here). Chains like the
// wince, so `kitty kitty` reads as two beats. Like every reaction it never
// summons a hidden companion or changes its identity.
const DELIGHT_HOLD: f32 = 0.62;
const DELIGHT_CHAIN_WINDOW: f32 = 0.9;
const DELIGHT_ATTACK: f32 = 0.05;
const DELIGHT_SETTLE: f32 = 0.2;
/// Delight's peak vertical stretch (with a paired horizontal squash).
const DELIGHT_STRETCH: f32 = 0.14;
const MAX_DELIGHT_CHAIN: u8 = 4;

// ── living-cartoon animation tuning ─────────────────────────────────────────
// One eased display-momentum spine, lagging the raw momentum for follow-through,
// drives every pose choice; the pose is a pure function of the spine and the clock.
/// Spine follower time constant (seconds).
const DISP_TAU: f32 = 0.13;
/// Spine range mapped to the banking curve: no lean below `BANK_LO`, full flying
/// lean by `BANK_HI`.
const BANK_LO: f32 = 0.32;
const BANK_HI: f32 = 0.96;
/// At full bank the cat stretches along its motion axis (wider) and thins a
/// touch (shorter).
const STRETCH_X: f32 = 0.17;
const THIN_Y: f32 = 0.10;
/// At full bank the cat also leans this fraction of a cell ahead of its rest
/// anchor.
const LEAD_MAX: f32 = 0.22;
/// Sustained-fast display momentum at/above which the cruising face squints
/// happily; a brief blink interrupts either face while visible.
const HAPPY_GATE: f32 = 0.80;
/// Deterministic idle-blink cadence: a full closure of `BLINK_DUR` every
/// `BLINK_PERIOD`, phased off the flight clock (never at the entrance).
const BLINK_PERIOD: f32 = 1.7;
const BLINK_DUR: f32 = 0.13;
/// Hover-bob amplitude (fraction of cell height). Every still path
/// ([`CursorCat::static_frame`], hidden, [`CatPose::STILL`]) pins `bob = 0`,
/// keeping reduced-motion presentation structurally static.
const BOB_AMP: f32 = 0.09;
/// Stride pump: phase advance per committed forward keystroke, in cycles. The
/// momentum metric sits at its 1.0 clamp at every human cadence while the cat
/// is visible, so cadence drives the step instead: one visible step per key, a
/// gallop when you type fast.
const STRIDE_PER_KEY: f32 = 0.5;
/// Idle drift (cycles/sec) between keystrokes, so a pause eases the cat
/// through its step instead of freezing it mid-stride.
const STRIDE_IDLE_HZ: f32 = 1.4;
/// Time constant (seconds) of the drawn stride phase's follow of the pump.
///
/// A key adds [`STRIDE_PER_KEY`] (half a cycle) at once, and half a cycle is
/// `sin(θ + π) = −sin θ`: drawn directly, every key flipped the hover bob from
/// one side to the other in a single present, up to 0.18 of a row (5 px at a
/// 28 px cell), the jitter of a typing head. The drawn phase follows the pump
/// at first order instead (never overshooting it), so a key's step lands over
/// a few frames: typing at 9 keys a second, at most a tenth of a cycle per 8 ms
/// present, under 0.06 of a row (1.7 px), while 89 % of each key's step still
/// lands before the next key.
const STRIDE_TAU: f32 = 0.05;
/// The fastest (cycles/s) the drawn stride phase follows the pump, whatever the
/// display's rate. The follow alone moves a share of the gap per present, and
/// a present is twice as long at 60 Hz: a key's step there still moved the bob
/// 0.1 of a row in one present. Capped at this rate, no present at 60 Hz or
/// faster moves the bob more than `2π · BOB_AMP · STRIDE_RATE_MAX / 60`, about
/// 0.05 of a row, and 9 keys a second (4.5 cycles a second of steps) are still
/// kept up with. Faster typing than the cap is drawn a whole cycle short where
/// the phase falls a cycle behind (the bob is the same there), so the head
/// never steps on after the hand has stopped.
const STRIDE_RATE_MAX: f32 = 5.5;
/// Body compression on each footfall. Small: the step should read as weight, not
/// as a bounce competing with the sing dance or the landing settle.
const STRIDE_SQUASH: f32 = 0.05;
/// Landing squash→stretch settle: a damped bounce of length `LAND_DUR`,
/// amplitude `LAND_AMP`, `LAND_FREQ` oscillations, decaying at `LAND_DECAY`.
const LAND_DUR: f32 = 0.42;
const LAND_AMP: f32 = 0.26;
const LAND_FREQ: f32 = 1.35;
const LAND_DECAY: f32 = 3.0;

/// Sing-along dance depths at full drive (`crate::kitty_sing`): the on-beat
/// squash pulse and the two-beat lean sway. Both scale with the drive, so the
/// wind-down crossfade eases the dance out.
const SING_SQUASH: f32 = 0.14;
const SING_SWAY: f32 = 0.12;

/// One frame's sing-along sync from the host: the drive/beat pair the dance
/// rides, shared by both render paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct SingSync {
    /// Celebration drive 0..=1 (1 armed, easing through the wind-down).
    pub drive: f32,
    /// Beat phase in beats since the arm (fractional).
    pub beat: f32,
}

/// Smoothstep ramp of `x` across `[lo, hi]` → `0..=1` (eased at both ends).
#[inline]
const fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The living-cartoon pose handed to the host each frame: a dest-rect scale
/// (squash and stretch about the sprite centre), a forward lean, and the baked
/// eye frame. [`CatPose::STILL`] is the identity a hidden or reduced-motion cat
/// carries.
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

/// The wall-clock fold sample handed to the pixel placement layer. CursorCat
/// owns the behavioural clock and knows no cell metrics, sprite dimensions,
/// pane offsets or renderer coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CatFoldFrame {
    pub started: Instant,
    pub direction: CatFoldDirection,
    /// Linear whole-seam progress, `0..1`. The placement layer splits it into
    /// the leaving and arriving half-beats and applies their easing.
    pub progress: f32,
}

/// CursorCat's complete placement-side verdict for one rendered frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorCatPlacementFrame {
    /// The same injected frame clock used to sample the lifecycle. Placement
    /// uses it to reject stale geometry after a sparse/occluded frame gap.
    pub sampled_at: Instant,
    pub fold: Option<CatFoldFrame>,
    /// Persistent facing outside a fold. An active fold's direction outranks
    /// this value so a new forward key cannot mirror a reverse body mid-seam.
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

/// One-shot expression layered over the collected look. These are decision
/// values only; the renderer maps them to typed authored glyph ids.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatReaction {
    Cruise,
    /// Enter/Return while the companion is already flying.
    Celebrate,
    /// A delete while flying (Backspace or any kill chord): the tongue-out
    /// "ooops" ([`CatGlyphId::S121`]) with a recoil kick, held for the burst.
    Startled,
    /// A complete profanity cue: squeezed eyes and a burst-scaled recoil.
    /// Repeated nearby cues increase the pose force and pulse count.
    Wince,
    /// A complete feline word was typed (`kitty`, `gato`, `neko`, …; see
    /// [`crate::kitty_summon`]): happy eyes, chaining like [`Self::Wince`].
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
    /// True during the guaranteed discovery hold. The host may show this even
    /// when the selected cursor trail is not `rainbow kitty`.
    pub discovery: bool,
    /// True for the whole collection hello including its fade: only the hold
    /// is forced, but the presentation stays drawable until alpha reaches zero.
    pub collection_hello: bool,
    /// Signed hover-bob as a fraction of cell height (±`BOB_AMP`), so the
    /// flying cat reads as airborne.
    pub bob: f32,
    /// The living-cartoon pose for this present: squash/stretch, forward lean,
    /// and the baked eye frame. [`CatPose::STILL`] when hidden or reduced-motion.
    pub pose: CatPose,
    /// Sing-along drive 0..=1 (`crate::kitty_sing`), easing to 0 through the
    /// wind-down. Past the face threshold it swaps in the singing head
    /// ([`CatFrame::render_look`]); the dance is already baked into `pose`/`bob`.
    pub sing: f32,
}

impl CatFrame {
    /// Resolve the authored art for this expression (typed glyph ids, so roster additions
    /// cannot shift it). Precedence: the delete oops (S121), the exit flourishes, the singing
    /// head (S115), Enter's maneki celebration.
    #[must_use]
    #[inline]
    pub const fn render_look(&self) -> KittyLook {
        // The goodbye face waits until the fade is visible: the flourish is rolled at the LOW
        // crossing while alpha is still ~255 and a revive resets `exit`, so an immediate swap
        // would flash one full-opacity frame. `Startled` shares the S121 art but is no exit.
        let flourish_face_earned = self.fade_out >= EXIT_FACE_FADE;
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
            // Expression/full-body swaps cannot use a head-specific attachment.
            accessory: None,
            ..self.look
        }
        .normalized()
    }

    /// Quantized fingerprint folded into the frame's repaint key so the fade +
    /// bob + exit fx invalidate frames while active and settle (0) when hidden.
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
        // Fine enough that the sing crossfade re-presents smoothly.
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

/// The earned cat's decision layer: the canonical typing-momentum metric, a
/// high-band dwell clock and a fade/exit state machine. `on_key` is stamped on
/// the input thread; `frame` advances the machine once per rendered frame.
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
    /// flinch, or the fade-out touchdown). `None` between bounces.
    land_at: Option<Instant>,
    /// The meteor the head is flying ([`Self::on_meteor`]): its whip and squint
    /// ride the pose until [`Flight::settled_at`].
    meteor: Option<Flight>,
    /// The current oops burst's kick instant, the recoil envelope's attack
    /// clock. Set on the burst's first delete only ([`OOPS_REARM`]).
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
    /// host sync. Meaningful only while `sing > 0`.
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
        Self {
            momentum: TypingMomentum::default(),
            sustain: 0.0,
            run_keys: 0,
            last: None,
            state: State::Hidden,
            exit: CatExit::Plain,
            flight: None,
            stride: 0.0,
            stride_at: None,
            stride_shown: 0.0,
            // Toastbyte is designed for the cursor's 16–32 px band and pairs
            // with the rainbow kitty trail; the first collectible replaces it.
            look: KittyLook {
                variant: CatGlyphId::SpecStretch,
                ..KittyLook::default()
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
}

impl CursorCat {
    /// Retire an ordinary cursor-following flight when the caret it follows
    /// stops being one coordinate: the host calls this on a torn coordinate
    /// fence (`retire_torn_cursor_fx_host`), on a front-terminal switch, and
    /// when the head's owner goes away (`retire_kitty_cursor_without_owner`).
    /// A program's own cursor moves on a finished frame are no such fence (the
    /// head escorts the caret the pet escorts, `PetTick::escort_caret`).
    ///
    /// Collection hellos and the sing-along are explicit presentations and are
    /// left alone. An earned flight is positioned from the live caret, so
    /// letting it survive the fence would make the sprite jump to an unrelated
    /// cell.
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
    ///
    /// Forward pulses feed the momentum rule. Reverse folds do not call
    /// `on_key(false)`: the input seam already delivered that Backspace, and
    /// repeating it would double-drain momentum and double-kick the oops.
    #[inline]
    pub fn on_motion_pulse(&mut self, pulse: crate::cursor_glow::CursorCatMotionPulse) {
        self.on_motion_pulse_with_owner(pulse, true);
    }

    /// Feed the momentum and travel count the tenure pet mode's singing face
    /// needs without starting an ordinary flight: the held-key song still owes
    /// the real cursor-travel floor.
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

    /// Sample the cat's geometry-free placement state. A sample past
    /// [`CURSOR_FOLD_DURATION`] reports no fold, so a sparse frame never replays stale travel.
    #[must_use]
    #[inline]
    pub fn placement_frame(&self, now: Instant) -> CursorCatPlacementFrame {
        let fold_now = self.collection_sample_time(now);
        let fold = self.fold.and_then(|fold| {
            let progress = fold_now
                .saturating_duration_since(fold.started)
                .as_secs_f32()
                / CURSOR_FOLD_DURATION.as_secs_f32();
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

    /// Retire renderer-coordinate continuity only: drop an in-flight fold and the palette
    /// sampled under the old footprint. A collection hello or singing appearance continues
    /// at the live caret.
    #[inline]
    pub const fn rebase_placement(&mut self) {
        self.colors = None;
        self.fold = None;
        self.facing_left = false;
    }

    /// Stamp one committed text keystroke: `forward` for a normal char / space /
    /// newline (the cursor advances), `!forward` for a backspace. Only text keys.
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
            // The dwell reads the metric before this key's credit: the band
            // must have been held through the gap the key closes. Per-key credit
            // is capped so a pause key cannot buy the dwell.
            let held = self.momentum.value(now);
            if held >= CAT_BAND {
                self.sustain += dt.min(SUSTAIN_CREDIT_MAX);
                self.run_keys = self.run_keys.saturating_add(1);
            } else {
                // Leak, not reset (see [`SUSTAIN_LEAK`]). The run-key floor
                // leaks with the dwell, one key per drained
                // `SUSTAIN_CREDIT_MAX`, so the two gates stay in step.
                let drain = dt * SUSTAIN_LEAK;
                self.sustain = (self.sustain - drain).max(0.0);
                let lost = (drain / SUSTAIN_CREDIT_MAX) as u32;
                self.run_keys = self.run_keys.saturating_sub(lost);
            }
            // One stride step per key (cadence is the signal that still varies). `advance`
            // is rate-normalized, so key-repeat floods earn no faster than typing.
            self.stride_advance(now);
            self.stride = (self.stride + STRIDE_PER_KEY).rem_euclid(1024.0);
            self.momentum.advance(now);
            // A fading cat revives on resumed typing at speed, re-entering FadeIn at the
            // equivalent alpha. A collection hello's goodbye is exempt: its hold bookkeeping
            // must complete, or a revived flight could carry hello state that freezes the
            // machine on focus loss.
            if let State::FadeOut(t0) = self.state
                && !self.collection_hello
                && (ordinary_flight_owned || self.sing > 0.0)
                && self.momentum.value(now) >= REVIVE_GATE
            {
                let faded = (now.saturating_duration_since(t0).as_secs_f32() / FADE_OUT).min(1.0);
                let alpha = 1.0 - faded;
                let back = Duration::from_secs_f32(FADE_IN * alpha);
                self.state = State::FadeIn(now.checked_sub(back).unwrap_or(now));
                self.exit = CatExit::Plain;
                if self.land_at == Some(t0) {
                    self.land_at = None;
                }
            }
        } else {
            self.sustain = 0.0; // a backspace breaks the flow
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
    /// keeps its own gate); a latched look lands here. The FadeOut revive is the same
    /// appearance and keeps the latched look.
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
        self.reaction_until = Some(now + Duration::from_secs_f32(OOPS_HOLD));
        self.oops_last = Some(now);
        if !same_burst {
            self.oops_at = Some(now);
            // The flinch: the shared landing bounce, kicked once per burst so it
            // plays instead of re-freezing at impact.
            self.land_at = Some(now);
        }
    }

    /// Wince at complete profanity cues on a live companion; never summons, touches momentum
    /// or extends the flight. Cues within one phrase chain (to `MAX_WINCE_CHAIN`) to scale
    /// the re-kicked recoil. Returns whether a live companion accepted it.
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
        self.reaction_until = Some(now + Duration::from_secs_f32(WINCE_HOLD));
        self.land_at = Some(now);
        true
    }

    /// Fly the head's meteor (horizontal flights only): the spine is floored at
    /// [`Flight::disp_floor`] past the `DISP_TAU` follower, the whip springs from
    /// `FLYING_LEAD_0`, the eyes squint through the flight, and the landing squash is booked
    /// for [`Flight::land_at`]. `frame` predates the impulse, so its pose is re-resolved.
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

    /// Refresh an animated [`CatFrame`] that predates an in-frame reaction
    /// impulse (such as Rainbow Kitty v2's `Delight` in `route_flying_head`),
    /// so frame 0 of the reaction carries the new [`CatReaction`] and eyes.
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

    /// Light up at one or more complete feline words: the twin of
    /// [`Self::on_curse`] with happy eyes and a leap. Never summons, touches
    /// momentum or the flight, or changes `look`; returns whether a live
    /// companion accepted it.
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
        self.reaction_until = Some(now + Duration::from_secs_f32(DELIGHT_HOLD));
        true
    }

    /// Stamp one kill chord (Ctrl-K/U/W, Alt-D, forward Delete, word backspace): drains
    /// momentum at twice the single-delete rate and fires the oops on a live companion.
    /// Never summons.
    #[inline]
    pub fn on_kill(&mut self, now: Instant) {
        self.sustain = 0.0;
        self.run_keys = 0;
        self.momentum.kill(now);
        self.last = Some(now);
        self.note_oops(now);
    }

    /// Celebrate a committed command without summoning an otherwise-hidden
    /// companion. The reaction is a bounded pose swap on an already-live cat.
    #[inline]
    pub fn on_enter(&mut self, now: Instant) {
        if self.is_active() {
            self.reaction = CatReaction::Celebrate;
            self.reaction_until = Some(now + Duration::from_secs_f32(CELEBRATE_HOLD));
            // The squash→stretch stomp.
            self.land_at = Some(now);
        }
    }

    /// Host sync for the sing-along (`crate::kitty_sing`), once per frame before
    /// [`Self::frame`]/[`Self::static_frame`]. While armed (`drive == 1`) the metric is pinned
    /// to 1.0, since holding a key at repeat cadence is the celebration's premise; an armed
    /// celebration skips the dwell but still owes [`MIN_RUN_KEYS`].
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

    /// Install the companion's identity (the host's per-frame verdict: a pinned favourite,
    /// else the tenured program cat, else the launch kitty) without changing the lifecycle.
    /// While on screen it parks until the next appearance (a mid-fade revive is the same
    /// appearance); while hidden it applies at once. [`Self::on_collect`] replaces it at once.
    #[inline]
    pub const fn set_look(&mut self, look: KittyLook) {
        let look = look.normalized();
        if self.is_active() || self.collection_hello {
            // On screen (or a paused hello is outstanding): defer. A sync matching the
            // current look clears the slot.
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

    /// The hello for a cat the user chose (a favourite pin or `wear`): visible for
    /// [`DISCOVERY_HOLD`] even at zero typing momentum, then the ordinary fade-out unless live
    /// typing has re-earned past [`REVIVE_GATE`]. Ambient discoveries never hand a look here.
    pub fn on_collect(&mut self, now: Instant, look: KittyLook) {
        self.look = look.normalized();
        // The hello presents the newly pinned cat, superseding any parked look.
        self.pending_look = None;
        self.momentum.set_value(now, 1.0);
        self.last = Some(now);
        self.sustain = 0.0;
        self.run_keys = 0;
        self.exit = CatExit::Plain;
        let until = now + Duration::from_secs_f32(DISCOVERY_HOLD);
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

    /// Pause or resume a collection hello at the host's drawability boundary: only the
    /// discovery promise freezes while it cannot be presented.
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
            // The oops clocks retire with the expression; a delete after full decay is a
            // fresh burst anyway (`OOPS_HOLD` > `OOPS_REARM`).
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
            self.stride = (self.stride + dt * STRIDE_IDLE_HZ).rem_euclid(1024.0);
            // The pump only runs forward; the shortest way round its 1024-cycle
            // wrap is the gap still to draw.
            let mut gap = (self.stride - self.stride_shown + 512.0).rem_euclid(1024.0) - 512.0;
            if gap >= 1.0 {
                self.stride_shown = (self.stride_shown + gap.floor()).rem_euclid(1024.0);
                gap = gap.fract();
            }
            let k = 1.0 - (-dt / STRIDE_TAU).exp();
            let step = (gap * k).clamp(-STRIDE_RATE_MAX * dt, STRIDE_RATE_MAX * dt);
            self.stride_shown = (self.stride_shown + step).rem_euclid(1024.0);
        }
    }

    /// The airborne hover-bob (fraction of cell height), a pure read of the stride pump.
    /// No delight leap: the feline word's own cat (`word_decorations::CatIdlePose`)
    /// celebrates; the companion only gets happy eyes.
    #[inline]
    fn bob(&self) -> f32 {
        match self.flight {
            Some(_) => (std::f32::consts::TAU * self.stride_shown).sin() * BOB_AMP,
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
        (elapsed / DELIGHT_HOLD).clamp(0.0, 1.0)
    }

    /// Advance the eased display-momentum spine toward `target` (the live
    /// score) with time constant `DISP_TAU`. `disp_at == None` reseeds it to
    /// the target, so a fresh flight opens already banked.
    #[inline]
    fn advance_spine(&mut self, now: Instant, target: f32) {
        let target = target.clamp(0.0, 1.0);
        match self.disp_at {
            None => self.disp = target,
            Some(prev) => {
                let dt = now.saturating_duration_since(prev).as_secs_f32().min(0.1);
                if dt > 0.0 {
                    let alpha = 1.0 - (-dt / DISP_TAU).exp();
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
                let t = now.saturating_duration_since(t0).as_secs_f32();
                let phase = (t + BLINK_PERIOD - BLINK_DUR).rem_euclid(BLINK_PERIOD);
                phase < BLINK_DUR
            }
            None => false,
        }
    }

    /// Resolve the living-cartoon pose for this present: advance the spine,
    /// map it to a banking squash/stretch and forward lean, overlay any landing
    /// bounce, and select the baked eye frame. Pure in the spine, the injected
    /// clock and the frame's expression (`reaction`/`exit`).
    fn resolve_pose(
        &mut self,
        now: Instant,
        target: f32,
        reaction: CatReaction,
        exit: CatExit,
    ) -> CatPose {
        self.advance_spine(now, target);
        // Banking: lean + stretch along the motion axis, eased by the spine.
        let bank = smoothstep(BANK_LO, BANK_HI, self.disp);
        let mut scale_x = 1.0 + STRETCH_X * bank;
        let mut scale_y = 1.0 - THIN_Y * bank;
        let mut lead = LEAD_MAX * bank;
        // The whip: a meteor yanks the head back against travel and it springs
        // to its resting lean, added over the bank so it settles without a step.
        if let Some(m) = self.meteor {
            if now >= m.settled_at() {
                self.meteor = None;
            } else {
                lead += m.lead_at(now) - FLYING_LEAD_MAX;
            }
        }
        // Footfall compression, phase-locked to `bob` and gated by `bank`: the level
        // decides whether to step, the cadence where in the step it is.
        self.stride_advance(now);
        if self.flight.is_some() && bank > 0.0 {
            let foot = (std::f32::consts::TAU * self.stride_shown).cos().max(0.0) * bank;
            scale_y *= 1.0 - STRIDE_SQUASH * foot;
            scale_x *= 1.0 + STRIDE_SQUASH * 0.7 * foot;
        }
        // Landing: a damped bounce, squashed at impact (`q > 0`), overshooting to a stretch.
        // A meteor's squash is booked for its arrival edge: nothing lands before it.
        if let Some(t0) = self.land_at.filter(|&t0| now >= t0) {
            let u = now.saturating_duration_since(t0).as_secs_f32() / LAND_DUR;
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
            // Delete oops recoil: a backward lean plus a light crouch (no rotation), held at
            // `env == 1` while the burst-extended hold lasts.
            CatReaction::Startled => {
                let attack = self.oops_at.map_or(1.0, |t0| {
                    smoothstep(
                        0.0,
                        OOPS_ATTACK,
                        now.saturating_duration_since(t0).as_secs_f32(),
                    )
                });
                let settle = self.reaction_until.map_or(1.0, |until| {
                    smoothstep(
                        0.0,
                        OOPS_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    )
                });
                let env = attack * settle;
                // A startled cat may carry a full forward lunge, so the bank lead is
                // suppressed with `env` (it pulls up short, then rocks back).
                lead = lead * (1.0 - env) - OOPS_RECOIL * env;
                scale_y *= 1.0 - OOPS_SQUASH * env;
                scale_x *= 1.0 + OOPS_SQUASH * 0.6 * env;
            }
            // Curse wince: every word re-kicks the recoil; `wince_chain` raises force and
            // pulse count within bounds. Squeezed eyes plus motion carry it; the look stays.
            CatReaction::Wince => {
                let elapsed = self
                    .wince_at
                    .map_or(0.0, |t0| now.saturating_duration_since(t0).as_secs_f32());
                let attack = smoothstep(0.0, WINCE_ATTACK, elapsed);
                let settle = self.reaction_until.map_or(1.0, |until| {
                    smoothstep(
                        0.0,
                        WINCE_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    )
                });
                let chain = self.wince_chain.max(1);
                let phase = elapsed / WINCE_HOLD * f32::from(chain);
                let pulse = 0.78 + 0.22 * (std::f32::consts::TAU * phase).cos().abs();
                let strength = 1.0 + 0.18 * f32::from(chain.saturating_sub(1));
                let env = attack * settle * pulse * strength;
                lead = lead * (1.0 - env.min(1.0)) - WINCE_RECOIL * env;
                scale_y *= 1.0 - WINCE_SQUASH * env;
                scale_x *= 1.0 + WINCE_SQUASH * 0.7 * env;
                // Tumble: a decaying lateral rock whose frequency rides the chain, with a
                // counter-phase scale wobble standing in for rotation.
                let swing = (-WINCE_TUMBLE_DECAY * elapsed).exp()
                    * (std::f32::consts::TAU
                        * WINCE_TUMBLE_FREQ
                        * f32::from(chain).sqrt()
                        * elapsed)
                        .sin();
                lead += WINCE_TUMBLE * swing * attack * settle;
                let wobble = 0.05 * swing * attack * settle;
                scale_x *= 1.0 + wobble;
                scale_y *= 1.0 - wobble;
            }
            // Feline-word delight: a brief vertical swell across the hold; `delight_chain`
            // pulses multiple beats and lifts the swell within bounds (`kitty kitty` reads
            // as two beats) without moving `lead` or `bob`.
            CatReaction::Delight => {
                let elapsed = self
                    .delight_at
                    .map_or(0.0, |t0| now.saturating_duration_since(t0).as_secs_f32());
                let attack = smoothstep(0.0, DELIGHT_ATTACK, elapsed);
                let settle = self.reaction_until.map_or(1.0, |until| {
                    smoothstep(
                        0.0,
                        DELIGHT_SETTLE,
                        until.saturating_duration_since(now).as_secs_f32(),
                    )
                });
                let arc = (elapsed / DELIGHT_HOLD).clamp(0.0, 1.0);
                let env = attack * settle;
                let chain = self.delight_chain.max(1);
                let strength = 1.0 + 0.16 * f32::from(chain.saturating_sub(1));
                let pulse = (std::f32::consts::PI * arc * f32::from(chain)).sin().abs();
                // No forward lean: the cat perks up at its own name.
                let stretch = pulse * env * strength;
                scale_y *= 1.0 + DELIGHT_STRETCH * 0.33 * stretch;
                scale_x *= 1.0 - DELIGHT_STRETCH * 0.18 * stretch;
            }
            CatReaction::Cruise | CatReaction::Celebrate | CatReaction::Discovery => {}
        }
        // Sing-along dance: a per-beat squash relaxing across the beat plus a two-beat
        // sway, scaled by the drive, on the shared beat clock rather than the frame rate.
        if self.sing > 0.0 {
            let u = self.sing_beat.fract();
            let pulse = (1.0 - u) * (1.0 - u);
            scale_y *= 1.0 - SING_SQUASH * pulse * self.sing;
            scale_x *= 1.0 + SING_SQUASH * 0.7 * pulse * self.sing;
            lead += SING_SWAY * (std::f32::consts::PI * self.sing_beat).sin() * self.sing;
        }
        // Expression: blink/squint only over the plain cruising/discovery face —
        // the wink/celebrate reactions own their own eyes via a variant swap.
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
                    // Singing outranks the blink (a blinking open-mouth head reads as a glitch)
                    // and gets happy eyes over the open-mouth head.
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

    /// The reduced-motion frame, with no fade or bob: a collection hello is an
    /// opaque still for its bounded hold, then gone in one step; a summoned
    /// sing-along is an opaque singing still. An ordinary earned flight still
    /// runs its lifecycle by wall time (the host does not draw it).
    pub fn static_frame(&mut self, now: Instant) -> CatFrame {
        // A reduced-motion sample is a settled placement verdict, not a paused
        // off-glass seam that may resume halfway through when policy changes.
        self.fold = None;
        self.facing_left = false;
        let now = self.collection_sample_time(now);
        let look = self.look;
        if self.collection_hello && self.discovery_active(now) {
            // The oops and the wince show as a static face swap only; `resolve_pose`
            // never runs on this path.
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
                // The hello owns this still: no singing overlay.
                sing: 0.0,
                pose: CatPose::STILL,
            };
        }
        if self.collection_hello {
            self.settle_hidden();
        }
        let look = self.look;
        // Static celebration (reduced-motion sing-along): an opaque singing still while
        // `sing_face_live` holds, gone in one step below it. Only for a summoned cat: from
        // cold the detector arms before `run_keys` reaches `MIN_RUN_KEYS`.
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
        // Ordinary earned flight under reduced motion: `frame` never runs here, so a flight
        // summoned while motion is reduced would otherwise latch (`is_active` never clears,
        // the look latch wedges, resumed motion pops the cat in unearned). Drive the lifecycle
        // by wall time, alpha snapped to each state's endpoint. Only at `sing == 0`: a
        // winding-down celebration keeps the one-step disappearance above.
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
                    if now.saturating_duration_since(t0).as_secs_f32() >= FADE_OUT {
                        self.state = State::Hidden;
                    }
                }
                State::Hidden => {}
            }
            if matches!(self.state, State::Hidden) {
                self.settle_hidden();
                return hidden_frame(self.look);
            }
            // Still airborne: alpha snaps to the state's endpoint. Not drawn under
            // reduced motion; this keeps `is_active` accurate until retirement.
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

    /// The next future redraw deadline for a reduced-motion collection hello:
    /// the erase deadline, or the earlier oops-decay instant (the swap back has
    /// no key to ride). An expired deadline is never returned: rearming a past
    /// instant could spin if an occluded compositor withholds the redraw.
    #[inline]
    #[must_use]
    pub fn static_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.collection_hello || self.collection_paused_at.is_some() {
            return None;
        }
        let reaction_decay = matches!(self.reaction, CatReaction::Startled | CatReaction::Wince)
            .then_some(self.reaction_until)
            .flatten();
        [self.discovery_until, reaction_decay]
            .into_iter()
            .flatten()
            .filter(|deadline| *deadline > now)
            .min()
    }

    /// Freeze the first footprint palette sample of this visible episode, so later frames
    /// keep the same atlas tile when the colours beneath the cat change.
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

    /// The already-frozen palette, if any: the host checks it before its footprint walk,
    /// so every frame after the first is an O(1) hit.
    #[must_use]
    #[inline]
    pub const fn episode_colors(&self) -> Option<CatColorKey> {
        self.colors
    }

    /// Advance the fade/exit machine one frame and resolve the draw state.
    pub fn frame(&mut self, now: Instant) -> CatFrame {
        let now = self.collection_sample_time(now);
        // Keep the real hold deadline before retiring it: a late present
        // samples the lifecycle at wall-clock `now` rather than restarting a
        // full fade from the first delivered frame.
        let discovery_until = self.discovery_until;
        let discovery = self.discovery_active(now);
        if !discovery {
            self.discovery_until = None;
        }
        let reaction = self.reaction_at(now);
        let look = self.look;
        if matches!(self.state, State::Hidden) {
            // Hidden ⇒ zero animation work: the spine settles and stride/bob
            // math is skipped, so consecutive hidden frames change nothing
            // after the first (fp 0, timer disarmed).
            self.reset_spine();
            return hidden_frame(look);
        }
        let s = self.decayed(now);
        // The stride phase is advanced to `now` before the bob reads it. Read
        // first, the bob drew the last frame's phase on an ordinary frame and
        // this frame's on a key's (whose note advanced it), two frames' steps
        // in one present: a tenth of a row at 60 Hz.
        self.stride_advance(now);
        let bob = {
            let cruise = self.bob();
            if self.sing > 0.0 {
                // Dance bob (one per beat), drive-blended over the cruise hover.
                let dance = (std::f32::consts::TAU * self.sing_beat).sin() * BOB_AMP;
                cruise + (dance - cruise) * self.sing
            } else {
                cruise
            }
        };
        // Grounding: an ordinary flight lets go under [`LOW`]. A collection
        // hello whose hold expired must stay bounded by design: the full seed
        // still reads ~0.25 at hold expiry (above LOW), so the hello also
        // grounds under [`REVIVE_GATE`] unless live typing has re-earned it.
        let grounded = s < LOW || (self.collection_hello && s < REVIVE_GATE);
        match self.state {
            State::Hidden => hidden_frame(look),
            State::FadeIn(t0) => {
                if !discovery && grounded {
                    return self.grounded_exit(discovery_until, now);
                }
                let t = (now.saturating_duration_since(t0).as_secs_f32() / FADE_IN).clamp(0.0, 1.0);
                if t >= 1.0 {
                    self.state = State::Shown;
                }
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
                let t =
                    (now.saturating_duration_since(t0).as_secs_f32() / FADE_OUT).clamp(0.0, 1.0);
                if t >= 1.0 {
                    self.settle_hidden();
                    return hidden_frame(self.look);
                }
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
    /// the lifecycle is frame-rate independent. Only called after observing
    /// `decayed(now) < LOW`, so it is never in the future.
    #[inline]
    fn low_crossing_at(&self, now: Instant) -> Instant {
        self.momentum.low_crossing(LOW).unwrap_or(now)
    }

    #[inline]
    fn begin_fade_out_at(&mut self, started: Instant, now: Instant) -> CatFrame {
        // A discovery goodbye keeps showing the exact unlocked art. Ordinary
        // earned flights retain the occasional authored wink/meow flourish.
        self.exit = if self.collection_hello {
            CatExit::Plain
        } else {
            self.roll_exit()
        };
        self.state = State::FadeOut(started);
        // Touchdown: the cat lands as its momentum dies — a squash→stretch settle.
        self.land_at = Some(started);
        // Re-sample the FadeOut state at the caller's time: one bounded
        // recursion that settles directly to Hidden when the whole fade
        // elapsed in a frame gap.
        self.frame(now)
    }

    /// On screen (fading or shown) ⇒ keep the host's 60 fps re-arm going.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        !matches!(self.state, State::Hidden)
    }

    /// The canonical typing-momentum metric at `now` (lazily decayed), the
    /// same number the glow engine's spine chases.
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

/// The dark arm's heart pink: additive light over a near-black ground.
const EXIT_HEART_PINK: u32 = 0x00FF_5C8A;

/// The exit flourish's light-theme inks, through the shared recipe.
///
/// On light grounds additive pink and white are invisible, so the light arm
/// paints source-over veils that darken the ground. The flourish can land on
/// text, so both take the over-text role of [`crate::cursor_glow::InkRole`],
/// seeded from the dark arm's hue so both themes draw the same mark. The star
/// seeds from the family gold rather than white (white has no hue), so the
/// light sparkle stays warm.
#[inline]
fn exit_heart_light() -> u32 {
    static INK: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *INK.get_or_init(|| InkRole::OverText.ink(EXIT_HEART_PINK))
}
#[inline]
fn exit_star_light() -> u32 {
    static INK: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *INK.get_or_init(|| InkRole::OverText.ink(twinkle_rgb(true)))
}

/// Draw the fade-out flourish (a rising heart for HeartMeow, a sparkling star for StarWink)
/// near the cat anchor `(ax, ay)` in window-absolute pixels; `fade_out` is 0..1. On
/// `dark_theme` it emits additive [`GlowQuad`]s into `out`, on light source-over
/// [`RainHalo`] veils into `halos`; exactly one sink is written per call.
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
    // Rise + a peaked fade (0 at the ends, brightest mid-exit) so it pops then
    // dissolves with the cat.
    let pop = (fade_out * std::f32::consts::PI).sin().clamp(0.0, 1.0);
    match exit {
        CatExit::Plain => {}
        CatExit::HeartMeow => {
            let u = (ch as f32 / 14.0).max(1.0) as i32; // heart pixel scale
            let hx = ax;
            let hy = ay - (fade_out * ch as f32 * 1.4) as i32; // float up
            if dark_theme {
                let cov = (230.0 * pop) as u8;
                if cov == 0 {
                    return;
                }
                // 7×6 heart, drawn as scaled per-row spans of additive light.
                const ROWS: [(i32, i32); 6] = [(1, 2), (0, 6), (0, 6), (1, 5), (2, 4), (3, 3)];
                const BUMPS: [(i32, i32); 1] = [(4, 5)]; // second top bump on row 0
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
                // Light: one darkened-rose source-over veil on the heart, bounded
                // by the over-text role's ceiling.
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
            // Photosensitivity: `EXIT_STAR_SCINT` keeps this 40%-amplitude
            // swing on a white star under the flash bound (~2.76 Hz).
            let twinkle = 0.6 + 0.4 * (fade_out * EXIT_STAR_SCINT).sin();
            let sx = ax + geom.cw as i32; // by the cat's face
            let sy = ay - ch / 3;
            let a = (ch / 5).max(2);
            if dark_theme {
                let cov = (255.0 * pop * twinkle).clamp(0.0, 255.0) as u8;
                if cov == 0 {
                    return;
                }
                let star_rgb = twinkle_rgb(false);
                let star = premul_rgb(star_rgb, cov);
                push_fx_rect(out, geom, sx - a, sy, 2 * a + 1, 1, star); // h arm
                push_fx_rect(out, geom, sx, sy - a, 1, 2 * a + 1, star); // v arm
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
                // Light: a source-over amber cross of two veils, so the star shape
                // survives while darkening the ground; twinkle rides the alpha.
                let alpha = (230.0 * pop * twinkle).clamp(0.0, InkRole::OverText.alpha_cap()) as u8;
                if alpha >= 24 {
                    let ink = exit_star_light();
                    let (cx, cy) = (sx as f32, sy as f32);
                    let af = a as f32;
                    push_exit_veil(halos, geom, cx, cy, 1.4 * af, 0.5 * af, ink, alpha); // h arm
                    push_exit_veil(halos, geom, cx, cy, 0.5 * af, 1.4 * af, ink, alpha); // v arm
                }
            }
        }
    }
}

/// Push one source-over radial veil ([`HaloMode::Over`]) of the exit flourish: `rgb` is a
/// straight `0x00RRGGBB` ink and `alpha` the centre opacity ceiling, carried in the colour's
/// high byte (`aterm_render::halo_over_cap`). Skipped once the centre alpha is nil.
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

/// Nearest-neighbour resample a straight-alpha RGBA8 image `src` (`sw×sh`) to
/// `tw×th`. Crisp for pixel-art upscales (the user-sprite case). Returns
/// `tw*th*4` bytes.
pub fn resample_nearest(src: &[u8], sw: usize, sh: usize, tw: usize, th: usize) -> Vec<u8> {
    if tw == 0 || th == 0 {
        return Vec::new();
    }
    let row_bytes = tw * 4;
    let mut out = vec![0u8; row_bytes * th];
    if sw == 0 || sh == 0 || src.len() < sw * sh * 4 {
        return out;
    }
    if sw == tw && sh == th {
        out.copy_from_slice(&src[..row_bytes * th]);
        return out;
    }
    let mut prev_sy = usize::MAX;
    for ty in 0..th {
        let sy = ty * sh / th;
        let row_d = ty * row_bytes;
        if sy == prev_sy {
            let prev_row_d = row_d - row_bytes;
            out.copy_within(prev_row_d..prev_row_d + row_bytes, row_d);
            continue;
        }
        prev_sy = sy;
        let row_s = sy * sw * 4;
        if sw == tw {
            out[row_d..row_d + row_bytes].copy_from_slice(&src[row_s..row_s + row_bytes]);
            continue;
        }
        let src_row = src[row_s..row_s + sw * 4].as_chunks::<4>().0;
        let dst_row = out[row_d..row_d + row_bytes].as_chunks_mut::<4>().0;
        for (tx, dst_px) in dst_row.iter_mut().enumerate() {
            *dst_px = src_row[tx * sw / tw];
        }
    }
    out
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

    /// Photosensitivity: the exit star wink (a 255-coverage white star with a
    /// 40% swing) stays under the crate's flash bound.
    #[test]
    fn cat_exit_starwink_flash_rate_stays_under_the_photosensitivity_bound() {
        let hz = EXIT_STAR_SCINT / FADE_OUT / std::f32::consts::TAU;
        let bound = crate::effect_util::TWINKLE_FLASH_BOUND_HZ;
        assert!(
            hz <= bound,
            "the exit wink flashes at {hz} Hz (bound {bound})"
        );
        // Anti-vacuity: a zero `EXIT_STAR_SCINT` would pass by deleting the wink.
        const { assert!(EXIT_STAR_SCINT > 0.0, "the wink still glints") };
    }

    fn geom() -> Geom {
        // Identity layout: origin 0, window extents == grid extents.
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

    /// A casual burst (a word or two at speed) never summons the companion or
    /// enters the high band.
    #[test]
    fn casual_word_or_two_never_summons_the_cat() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        // ~15 fast keys (two short words at 60 ms/key ≈ 0.9 s of typing).
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

    /// The 16-key travel floor is load-bearing independently of the stricter
    /// canonical band and dwell: even a pre-warmed, fully-dwelled metric stays
    /// hidden through fifteen qualifying keys, then summons on the sixteenth.
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

    /// Touching the high band is not enough: the metric must be held there for
    /// the full [`CAT_DWELL`]. Band entry (~1.4 s of flat-out typing) plus the
    /// dwell puts the summon in the 2.2–3.6 s window pinned from both sides.
    #[test]
    fn cat_requires_the_high_band_held_not_merely_touched() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let mut band_entry_key = None;
        let mut summoned_at_key = None;
        // Ample headroom: the summon lands near key 65 (~1.4 s ramp + 1.2 s dwell).
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

    /// Two-sided summon latency: every summoning cadence owes a 2.2–3.6 s run (the 16-key
    /// floor stops sparse cadences arriving early), and a stroll slower than ~0.60 s/key
    /// never holds the band.
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
        // At/above ~0.60 s/key each gap decays the clamped metric below
        // [`CAT_BAND`] before the next key, so the dwell never accrues.
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

    /// Sustained fast typing always summons the cat, which fades in to full
    /// opacity and then out completely when momentum stops.
    #[test]
    fn sustained_typing_summons_then_fades_out_completely() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let mut on = false;
        // Sustained fast typing (25 ms gaps) — band + dwell earn the flight
        // around 2.6 s in (~key 105), so run comfortably past that.
        for i in 0..300 {
            c.on_key(t + Duration::from_millis(i * 25), true);
            if c.is_active() {
                on = true;
                break;
            }
        }
        assert!(on, "a sustained fast run must always summon the cat");
        // Fade in reaches full opacity while momentum stays up.
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
        // Go silent: a present long after the LOW crossing (~3.9 s from the 1.0 ceiling)
        // plus FADE_OUT samples Hidden directly instead of restarting a fade.
        let gone = hot + Duration::from_secs(6);
        let done = c.frame(gone);
        assert_eq!(done.alpha, 0, "a sparse late frame is already hidden");
        assert_eq!(done.fp(), 0, "hidden frames settle to the zero repaint key");
        assert!(!c.is_active());
    }

    /// The band tolerates rhythm but not pauses: short breaths keep the dwell
    /// accruing (the metric's 2 s τ holds a hot run above [`CAT_BAND`]), while
    /// casual prose with thinking pauses never builds the metric into the band.
    #[test]
    fn breaths_keep_the_dwell_but_word_pauses_stay_cat_free() {
        // Flowing typing with 150 ms breaths every 16 keys still summons: a
        // 0.15 s breath decays a hot metric by ×0.93, staying in the band.
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
        // Three casual words separated by real 650 ms thinking pauses: the
        // metric never holds the band, so the companion stays away.
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

    /// A collection hello's goodbye is exempt from the revive: typing during
    /// the hello fade lets it finish, and the cat re-earns normally after.
    #[test]
    fn hello_goodbye_is_not_revived_by_typing() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        // Ride past the hold so the goodbye fade begins.
        let fade_at = t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.05);
        let fading = cat.frame(fade_at);
        assert!(fading.collection_hello, "the goodbye is a hello frame");
        assert!(fading.alpha > 0);
        // Fast keys mid-goodbye must not hijack the hello into a flight.
        cat.on_key(fade_at + Duration::from_millis(40), true);
        cat.on_key(fade_at + Duration::from_millis(80), true);
        let done = cat.frame(fade_at + Duration::from_secs_f32(FADE_OUT + 0.2));
        assert_eq!(done.alpha, 0, "the goodbye completes");
        assert!(!done.collection_hello, "hello bookkeeping fully cleared");
        assert!(!cat.is_active(), "the machine returned to Hidden");
    }

    /// A cat that began fading out revives when typing resumes at speed, with
    /// continuous opacity.
    #[test]
    fn fading_cat_revives_on_resumed_typing() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        // 120 keys at 60 ms: well past band + dwell, metric ≈ 1.0.
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
        // Two quick keys mid-fade revive it.
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

    /// The goodbye face never appears at full opacity, so a revive a frame later cannot swap
    /// the head back. Only 2 exit rolls in 5 flourish, so the trials vary the key count and
    /// assert a flourish was seen.
    #[test]
    fn the_exit_flourish_head_never_shows_at_full_opacity() {
        let mut saw_flourish = 0u32;
        let mut saw_plain_head = 0u32;
        for keys in 120..160u64 {
            let mut c = CursorCat::default();
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
                    // Visibly dimmed, not merely below 255: at the gate (a
                    // quarter through a 0.75 s fade) alpha is ~191.
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
        // Anti-vacuity: the walk emitted ordinary frames and reached the flourish.
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

    /// Reduced motion draws only a cat the machine has summoned: from cold the
    /// drive can be up while the machine is `Hidden` (the travel floor is
    /// unmet), and `static_frame` must then draw nothing, as `frame` does.
    #[test]
    fn reduced_motion_never_draws_a_cat_the_machine_calls_hidden() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        // Drive the celebration without earning the travel floor: raise `sing`
        // directly, exactly as an armed detector would, and take no keys.
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

    /// A collection hello owns an absolute hold deadline: if the compositor
    /// skips every frame spanning hold + fade, the next present is Hidden and
    /// never an opaque restart frame.
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

    /// Launch kitties are a pure function of the seed (stable for the process,
    /// identical on every machine) and differ across seeds.
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

    /// Both typed-word reactions are expression on a live companion: they never
    /// summon a hidden one, and `on_delight` never touches the identity.
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

    /// The companion does not leap at its own name (the word's own cat celebrates); what
    /// remains is expression: happy eyes and a brief swell, while `bob` is identical to
    /// an undelighted twin at the same active stride phase.
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
        // …but the reaction is genuinely live, and reads on the face and swell.
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

        // The favourite pin's hello (`on_collect`) is immediate and independent
        // of the ordinary momentum/travel gate.
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

        // The oops holds `OOPS_HOLD` after the delete; sample past its decay.
        let settled = cat.frame(t + Duration::from_millis(1300));
        assert_eq!(settled.reaction, CatReaction::Cruise);
        assert_eq!(settled.render_look(), look.normalized());
    }

    /// A flight earned under reduced motion is presented only through `static_frame`, which
    /// drives FadeIn→Shown→FadeOut→Hidden on the clock: the flight retires, `is_active`
    /// clears, and a look parked mid-flight applies.
    #[test]
    fn reduced_motion_flight_retires_and_releases_the_look_latch() {
        let mut cat = CursorCat::default();
        let t = Instant::now();
        // Earn an ordinary flight (the summon lands near key 105 at 25 ms/key).
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
        // A companion reassignment while the flight is on screen parks.
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
        // Present only through the reduced-motion static path from here; typing
        // has stopped, so the lifecycle must run all the way down by time.
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

    /// A look synced while the companion is mid-flight never morphs the
    /// sprite: it holds for the appearance, and the parked look lands on the
    /// next wake with no re-sync.
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

        // Another window reassigns the global companion now.
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

        // Typing on through a partial fade keeps the same appearance: the
        // FadeOut→FadeIn revive must not consume the latch either.
        let reviving = last + Duration::from_secs(4);
        let _ = c.frame(reviving); // begins FadeOut at `reviving`
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

        // Let it ground completely, then re-earn without another sync: the
        // parked look releases as FadeOut completes and presents on the wake.
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

    /// `on_collect` presents the chosen cat immediately, even mid-flight, and
    /// supersedes any parked look so a stale deferred sync cannot overwrite the
    /// hello at the next wake.
    #[test]
    fn discovery_hello_presents_the_new_look_mid_flight() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        for i in 0..120 {
            c.on_key(t + Duration::from_millis(i * 60), true);
        }
        assert!(c.is_active());
        let last = t + Duration::from_millis(119 * 60);

        // Park a sync first (another window's reassignment)…
        let parked = KittyLook {
            variant: CatGlyphId::SpecSleeping,
            coat: 12,
            ..KittyLook::default()
        };
        c.set_look(parked);
        // …then this window pins a favourite mid-flight.
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

        // The hello completes; a fresh earn (no further sync) must keep the
        // collected look — the parked pre-collect sync was superseded.
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

    /// A placement rebase changes the cells under the cursor cat, so its contrast key is
    /// resampled; collection and song keep identity and lifecycle across the host's
    /// retirement/rebase sequence.
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

    /// On dark, exit fx emit additive quads only while fading (peaked), nothing
    /// for Plain, and never touch the veil sink.
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

    /// On light, the exit flourish emits no additive white/pink (invisible on a
    /// pale ground); the heart and star come through as source-over veils
    /// ([`HaloMode::Over`]) in darkened, saturated inks that darken the ground.
    #[test]
    fn exit_fx_light_theme_is_a_darkening_source_over_veil() {
        let g = geom();
        // Rec.601 luma (straight 8-bit): the veil ink must sit well below a
        // light ground (~250) so the composite visibly darkens it.
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
                // The straight ink is dark and saturated, and the high byte
                // carries a visible centre opacity.
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

    /// The light veil fades to nothing at the exit ends (`fade_out` near 0 or
    /// 1 ⇒ `pop → 0`), dissolving with the cat.
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

    // ───────────────────── living-cartoon pose animation ─────────────────────

    /// A key steps the hover bob over a few frames, never in one: typing at
    /// 9 and at 15 keys a second on a 120 Hz and on a 60 Hz display, no present
    /// moves the bob by more than 0.06 of a row, and it still moves with the
    /// keys. Negative control: the pump itself (what the bob drew until
    /// 2026-10-20) jumps half a cycle on a key, flipping the bob by up to 0.18
    /// of a row in one present. (With the follow alone, 60 Hz moved it 0.097
    /// to 0.113 of a row in a present.)
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

    /// Stride pump: the pose moves as you type, driven by the keys rather than
    /// the rate-normalized level (which pins at 1.0 while the cat is visible).
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
        // The tail of the run is where the metric is pinned.
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

        // Cadence, not the clock: the same elapsed time with fewer keys
        // advances the stride less, which a wall-clock sine cannot do.
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

        // The cat blinks while you type.
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

        // A resting cat is unchanged: the pump is gated by the same bank the
        // rest of the pose uses, so it cannot alter the idle silhouette.
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

    /// Banking is a pure function of the eased spine: sustained forward momentum
    /// leans + stretches the cat along its motion axis; a resting cat rides level.
    #[test]
    fn banking_leans_and_stretches_with_momentum() {
        let look = KittyLook::default();
        let t = Instant::now();
        // Fast: keep feeding forward keys so the display spine climbs to speed.
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

        // Slow: no typing after the hello; momentum decays, the cat rides level.
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

    /// A sudden stop (the fade-out touchdown) and an Enter stomp both fire the
    /// squash→stretch landing bounce, which then settles back toward neutral.
    #[test]
    fn landing_squash_fires_on_stop_and_on_enter() {
        let look = KittyLook::default();
        let t = Instant::now();

        // Enter stomp on a live cat squashes it, then it settles.
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

        // A fast stop: discovery expires with no momentum, so the fade-out
        // touchdown squashes the cat as it lands.
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

    /// The idle blink is deterministic and windowed: the eyes are open outside
    /// the blink window and closed inside it, identically for identical instants.
    #[test]
    fn idle_blink_is_deterministic_and_windowed() {
        let build = || {
            let mut cat = CursorCat::default();
            let t = Instant::now();
            cat.on_collect(t, KittyLook::default());
            (cat, t)
        };
        // Pump a 100 ms frame cadence so the display spine converges on the
        // decaying metric (sparse samples keep stale banking momentum).
        let pump = |cat: &mut CursorCat, t: Instant, upto_ms: u64| {
            let mut f = cat.frame(t + Duration::from_millis(1000));
            let mut ms = 1000;
            while ms < upto_ms {
                ms = (ms + 100).min(upto_ms);
                f = cat.frame(t + Duration::from_millis(ms));
            }
            f
        };
        // Outside the window while lingering: eyes stay open.
        let (mut a, t) = build();
        let open = pump(&mut a, t, 1000);
        assert!(open.alpha > 0, "lingering during the hold");
        assert_eq!(
            open.pose.eyes,
            EyesFrame::Open,
            "no blink outside the window"
        );
        // Inside the window: a full blink closes the eyes.
        let (mut b, t) = build();
        let blink = pump(&mut b, t, 1850);
        assert_eq!(
            blink.pose.eyes,
            EyesFrame::Blink,
            "the blink lands in its deterministic window"
        );
        // Determinism: the same clock ⇒ the same eyes.
        let (mut c, t) = build();
        let again = pump(&mut c, t, 1850);
        assert_eq!(
            blink.pose.eyes, again.pose.eyes,
            "blink is a pure clock function"
        );
    }

    /// Sustained fast momentum squints happily over the cruising face.
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

    // ───────────────────────── the delete "oops" ─────────────────────────

    /// A key-repeat delete run reads as one sustained oops: the expression
    /// holds across the burst, but the squash kick fires only on the first
    /// delete, so a later repeat finds the bounce decaying, not re-frozen.
    #[test]
    fn delete_burst_is_one_sustained_oops_with_one_kick() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let _ = cat.frame(t + Duration::from_millis(16));
        // Burst start: the kick lands — a visible squash on the impact frame.
        let t0 = t + Duration::from_millis(200);
        cat.on_key(t0, false);
        let hit = cat.frame(t0);
        assert_eq!(hit.reaction, CatReaction::Startled);
        assert!(
            hit.pose.scale_y < 0.85,
            "the burst's first delete kicks a visible squash, got {}",
            hit.pose.scale_y
        );
        // Hold the key: autorepeat deletes every 50 ms for 300 ms.
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
        // A fresh burst after a deliberate pause (> OOPS_REARM) re-kicks.
        let again = last + Duration::from_millis(500);
        cat.on_key(again, false);
        let rekick = cat.frame(again);
        assert!(
            rekick.pose.scale_y < 0.85,
            "a new burst re-arms the squash kick, got {}",
            rekick.pose.scale_y
        );
    }

    /// The oops hold: the tongue-out still reads 0.4 s after the delete, the
    /// pose recoils back off the rest anchor while it holds, and everything
    /// decays to the base look after.
    #[test]
    fn oops_holds_visibly_then_decays_to_base_look() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let _ = cat.frame(t + Duration::from_millis(16));
        let del = t + Duration::from_millis(300);
        cat.on_key(del, false);
        // Pump a frame cadence so the display spine settles as a real present
        // stream would (one sparse sample keeps stale banking lean).
        for ms in [100u64, 200, 300] {
            let _ = cat.frame(del + Duration::from_millis(ms));
        }
        // 0.4 s in, well inside OOPS_HOLD: the reaction still reads.
        let held = cat.frame(del + Duration::from_millis(400));
        assert_eq!(held.reaction, CatReaction::Startled);
        assert_eq!(held.render_look().variant, CatGlyphId::S121);
        assert!(
            held.pose.lead < -0.05,
            "mid-hold the body recoils backward, got lead {}",
            held.pose.lead
        );
        // Past the hold the expression and recoil release. Residual momentum
        // may leave a small forward lean; the backward recoil must be gone.
        let settled = cat.frame(del + Duration::from_millis(700));
        assert_eq!(settled.reaction, CatReaction::Cruise);
        assert_eq!(settled.render_look(), KittyLook::default().normalized());
        assert!(
            settled.pose.lead > -0.02,
            "the backward recoil releases with the expression, got lead {}",
            settled.pose.lead
        );
    }

    /// Kill chords fire the same bounded oops on a live companion and never
    /// summon a hidden one.
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

    /// Kills drain mildly: one kill dents the metric and breaks the dwell,
    /// while a kill flood walks it under [`LOW`] and grounds the flight, whose
    /// fade carries the tongue-out.
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
        // One kill: a mild dent, the flight rides on.
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
        // A held kill flood un-earns the run completely.
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

    /// Reduced motion: the oops is a static swap (full opacity, S121 face,
    /// pose `STILL`, zero bob) that decays back to the discovery face with its
    /// own armed wake.
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
        // Past the hold: the hello face returns, the erase deadline remains.
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

    /// Hidden ⇒ zero animation work: neutral pose, `fp() == 0`, timer disarmed —
    /// and a fully faded-out flight settles the spine back to exactly that.
    #[test]
    fn hidden_settles_to_zero_work() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        let idle = cat.frame(t);
        assert_eq!(idle.alpha, 0);
        assert_eq!(idle.fp(), 0, "hidden ⇒ fp settles to 0");
        assert_eq!(idle.pose, CatPose::STILL, "hidden ⇒ neutral pose");
        assert!(!cat.is_active(), "hidden ⇒ the host 60 fps timer disarms");

        // Summon, animate, then let it fully fade out and confirm it re-settles.
        cat.on_collect(t, KittyLook::default());
        let _ = cat.frame(t + Duration::from_millis(16));
        let _ = cat.frame(t + Duration::from_secs_f32(DISCOVERY_HOLD + 0.05)); // begins fade-out
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

    /// The [`SingSync`] for (drive, beat) — the pair the dance rides.
    fn sync(drive: f32, beat: f32) -> SingSync {
        SingSync { drive, beat }
    }

    fn arm_singing_after_travel(c: &mut CursorCat, t: Instant) -> Instant {
        // The detector arms after sixteen repeat events; pinning the metric
        // then must not substitute for the sixteen-event travel floor.
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

    /// An armed celebration pins the metric to 1.0, bypasses dwell only after
    /// sixteen qualifying events, wears the open-mouth meow head, and dances on
    /// the shared beat clock (the pose cycles within a beat).
    #[test]
    fn singing_pins_momentum_summons_and_dances_on_the_beat() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        assert_eq!(c.momentum(armed_at), 1.0, "the documented momentum bypass");
        // Two presents inside one beat, past the fade-in: fresh-beat pulse
        // vs relaxed mid-beat — the dance loop must move the pose.
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

    /// The singing head never blinks, including at the `companion::sing_face_live` gate
    /// value (the eyes are baked into the tile, so a blink is a distinct head). Two-sided:
    /// off the song the cat still blinks.
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
        // …and the control: a cat that is not singing still blinks, so the
        // assertion above is about the gate and not about blinking at all.
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

    /// The delete oops outranks the song: a backspace mid-celebration shows the
    /// tongue-out face over the singing head for its bounded hold.
    #[test]
    fn oops_outranks_the_singing_face() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        let t1 = armed_at + Duration::from_secs_f32(FADE_IN + 0.05);
        c.set_singing(t1, sync(1.0, 1.1));
        c.on_key(t1, false); // a delete mid-song
        let f = c.frame(t1 + Duration::from_millis(16));
        assert_eq!(f.render_look().variant, CatGlyphId::S121, "oops wins");
    }

    /// Wind-down is a crossfade: at half drive the frame still sings (face +
    /// scaled dance); at zero the overlay is gone and the companion glides out
    /// on natural momentum decay.
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
        // The metric was pinned at the arm and now decays naturally (~0.78 after 0.5 s);
        // 0.7 is a fixed marker, not `CAT_BAND`.
        assert!(
            c.momentum(t2) > 0.7,
            "the bypassed metric hands back to NATURAL decay (still high right after): {}",
            c.momentum(t2)
        );
    }

    /// Reduced motion: a static celebration (full opacity, singing face, pose
    /// `STILL`, `bob = 0`), disappearing in one step below the 0.33 face swap.
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
        // A late render deep in wind-down still draws the static singer until the face
        // swap, so the frame is not blank before the resident pet's fade-in.
        let late = armed_at + Duration::from_millis(600);
        c.set_singing(late, sync(0.49, 6.3));
        let retained = c.static_frame(late);
        assert_eq!(retained.alpha, 255, "late wind-down keeps opaque custody");
        assert_eq!(retained.render_look().variant, CatGlyphId::S115);
        // Below the face swap: one-step off.
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

        // Even 2s into the pause, `placement_frame` freezes fold progress at `t_pause`.
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

        // A new `on_collect` clears any pending landing/meteor/reaction/fold state.
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

        // When a reduced-motion collection hello retires in `static_frame`, a look
        // parked via `set_look` during the hello is consumed on the retirement edge.
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

        // Drive `from_fadeout` into a grounded `FadeOut` where `disp` has
        // decayed below `HAPPY_GATE` (`0.80`) and `disp_at` is still `Some(..)`.
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

        // Collecting a new cat during `FadeOut` must reseed `disp`/`disp_at` via
        // `reset_spine()` so frame 0 opens already banked with `EyesFrame::Happy`,
        // matching a fresh `on_collect` from `Hidden`.
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

        // Two consecutive hidden frames change nothing after the first: in
        // particular, `stride_at` stays `None` and `stride`/`stride_shown` stay 0.
        let f1 = cat.frame(t0);
        let f2 = cat.frame(t0 + Duration::from_millis(16));
        assert_eq!((f1.alpha, f1.bob, f1.pose), (0, 0.0, CatPose::STILL));
        assert_eq!((f2.alpha, f2.bob, f2.pose), (0, 0.0, CatPose::STILL));
        assert_eq!(cat.stride_at, None);
        assert_eq!(cat.stride, 0.0);
        assert_eq!(cat.stride_shown, 0.0);

        // A few sub-summon keystrokes while hidden advance `stride` ahead of
        // `stride_shown`; `frame` while still hidden does not mutate either,
        // and `on_collect` settles `stride_shown` to `stride` so the hello
        // hovers at rest without chasing stale pre-hello keystrokes.
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
}
