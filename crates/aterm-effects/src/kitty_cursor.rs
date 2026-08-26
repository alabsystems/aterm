// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rare cat that flies in FRONT of the cursor on a rainbow (the
//! [`crate::cursor_glow`] `rainbow kitty` ribbon is the wake). The cat itself is baked by
//! the smooth anti-aliased [`crate::cat_baker::CatBaker`] (the same renderer as
//! the peeking word-cats) with a HAPPY face — or replaced by a user PNG
//! (`cursor_nyan_sprite`), resampled here via [`resample_nearest`].
//!
//! This module owns the DECISION layer, [`CursorCat`]: the cat is EARNED — it
//! fades in only after THE canonical typing-momentum metric
//! ([`crate::typing_momentum::TypingMomentum`], the same law the ribbon
//! spine/stars/fresh-ink pops read) has been HELD in its high band
//! ([`CAT_BAND`] for [`CAT_DWELL`] — several seconds of genuinely sustained
//! non-delete typing, not a word or two), DETERMINISTICALLY: a sustained run
//! always summons — no rarity roll — rides while you keep flying, surviving
//! sub-half-second typing breaths and even REVIVING out of a started fade —
//! and fades out COMPLETELY when you stall. On the way out it does,
//! occasionally, a little flourish: 20% a star wink, 20% a heart meow (else a
//! plain fade).

use std::time::Duration;

use aterm_time::Instant;

use aterm_render::{GlowQuad, HaloMode, RainHalo, premul_rgb};

use crate::cat_baker::{CatColorKey, EyesFrame};
use crate::cat_glyphs_gen::CatGlyphId;
use crate::cursor_glow::{Geom, InkRole};
use crate::effect_util::twinkle_rgb;
use crate::kitty_registry::KittyLook;
use crate::typing_momentum::TypingMomentum;

/// Stable atlas key for a USER kitty sprite tile — the host bakes it into the
/// shared free atlas via `CatBaker::host_tile`.
pub const HOST_ID: u64 = 0x6E79_616E;

// ── momentum + lifecycle tuning ─────────────────────────────────────────────
//
// The cat's earn law reads THE canonical typing-momentum metric
// ([`crate::typing_momentum`]) — the same leaky integrator the ribbon spine,
// star envelope, and fresh-ink pops key off. The metric is rate-normalized
// (momentum = time spent typing, not key count), so the band below is
// measured in SECONDS of sustained flow at any cadence.
/// The HIGH BAND: the metric value at/above which typing counts toward the
/// summon dwell. 0.65 is ~1.39 s of continuous non-delete typing from cold
/// (the metric's documented build curve `1.3·(1−e^(−t/2))`:
/// t = −2·ln(1 − 0.65/1.3)) and tolerates a real inter-word breath — a
/// clamped 1.0 metric stays in the band for 2·ln(1/0.65) = 0.86 s of silence,
/// which is exactly the 0.4–0.8 s prose rhythm `typing_momentum`'s τ was
/// chosen for. A steady 1 s/key peck still reads 0.35 before every key and
/// can NEVER enter the band (the cutoff is ~0.60 s/key) — the companion reads
/// as earned, not ambient.
const CAT_BAND: f32 = 0.65;
/// Seconds the metric must be HELD in the band (measured across the keys that
/// land while it is high — a pause key can never buy the dwell, per-key
/// credit is capped at [`SUSTAIN_CREDIT_MAX`]) before the cat is eligible.
/// Band entry (~1.39 s) + dwell (1.2 s) ≈ a 2.6 s run of real sustained typing
/// at 25–60 ms/key, 3.3 s at 120 ms/key; above ~135 ms/key the independent
/// [`MIN_RUN_KEYS`] travel floor takes over (16 keys × gap). The companion is
/// still EARNED, not ambient. A key landing with the metric back under the
/// band resets the dwell — "sustained" means held, not revisited.
const CAT_DWELL: f32 = 1.2;
/// Per-key ceiling on dwell credit (seconds): the dwell is bought by keys
/// landing in rhythm, so one key after a long thought cannot claim the whole
/// gap as "sustained high typing".
const SUSTAIN_CREDIT_MAX: f32 = 0.25;
/// How fast the dwell DRAINS while the metric sits under [`CAT_BAND`], as a
/// multiple of the elapsed gap. 1.5 means a one-second stall costs 1.5 s of
/// accrued dwell — a genuine pause still loses the run, but a single
/// inter-word breath no longer throws away several seconds of real flow.
///
/// A leak, not a hard reset: a reset cliff demands EVERY inter-key gap stay
/// under the band-decay time for the whole dwell, which makes the cat
/// unreachable from ordinary prose — a cliff has no good position.
const SUSTAIN_LEAK: f32 = 1.5;
/// Minimum forward keystrokes in the CURRENT high-band run before the cat can
/// summon. An INDEPENDENT travel floor: it must be satisfied alongside the
/// dwell even though the dwell normally dominates it.
pub const MIN_RUN_KEYS: u32 = 16;
/// Below this metric value the cat lets go and fades out. With the metric's
/// 2 s τ a full run grounds after ~3.9 s of true silence — the companion
/// glides out rather than blinking off at the first breath.
const LOW: f32 = 0.14;
/// Metric at/above which resumed typing REVIVES a cat that already began its
/// fade-out: the companion is loyal — a thinking pause never forces the full
/// re-earn. ~0.35 is about two quick keys' worth of rebuilt metric from the
/// fade floor (one full 0.23 credit + a rhythm credit on top of the residue),
/// deliberately far under [`CAT_BAND`]: reviving an already-earned flight is
/// cheap, EARNING one never is.
const REVIVE_GATE: f32 = 0.35;
/// Fade-in / fade-out durations (seconds).
const FADE_IN: f32 = 0.35;
const FADE_OUT: f32 = 0.75;
/// Total wall-clock time of the classic cursor kitty's off-glass line-fold
/// seam. Fourteen to eighteen 60 Hz samples are enough to read both the leave
/// and arrive beats, while a sparse frame past the bound lands immediately
/// instead of replaying stale travel after an occlusion.
pub const CURSOR_FOLD_DURATION: Duration = Duration::from_millis(280);
/// Scintillation rate of the exit StarWink, in radians across the whole fade-out.
///
/// INVARIANT: keep `EXIT_STAR_SCINT / FADE_OUT / TAU <= 3.2` Hz — the same bound
/// [`crate::cursor_rainbow`]'s `TWINKLE_SCINT` certifies for its own twinkle.
/// Pinned by `cat_exit_starwink_flash_rate_stays_under_the_photosensitivity_bound`.
const EXIT_STAR_SCINT: f32 = 13.0;
/// A newly unlocked collectible gets one guaranteed, bounded hello. This is
/// independent of cursor style so every user can see what they discovered.
const DISCOVERY_HOLD: f32 = 2.8;
/// Short input reactions never extend the companion's overall flight.
const CELEBRATE_HOLD: f32 = 0.7;

// ── the delete "oops" ───────────────────────────────────────────────────────
// A delete on a LIVE companion swaps in the authored tongue-out wink
// (`s1_21` — [`CatGlyphId::S121`] via [`CatFrame::render_look`]): the cat
// caught mid-mischief, tongue out. EMPHASIS contract: the expression must
// actually READ (a hold long enough for the eye to land on, plus a pose
// recoil legible at 16 px), it must fire for EVERY delete spelling
// (Backspace and the kill chords alike), and a key-repeat delete run must
// read as ONE sustained "ooops" — not a strobing re-trigger.
/// How long the tongue-out expression holds after the LAST delete of a burst:
/// long enough for the eye to land on (the EMPHASIS contract) yet still under
/// [`CELEBRATE_HOLD`], and — like every reaction — it never extends the
/// flight itself: the momentum/fade clocks are untouched (the
/// bounded-expression contract).
const OOPS_HOLD: f32 = 0.60;
/// Deletes closer together than this belong to the SAME burst: each one
/// EXTENDS the hold (the tongue sustains under autorepeat) but does NOT
/// re-fire the pose kick — one flinch per burst. Re-kicking on every repeat
/// key would pin the landing bounce at its impact frame (`u ≈ 0` forever):
/// maximum squash with zero motion. Sized above autorepeat cadences
/// (~30–90 ms) and fast manual tapping (~150–250 ms); a deliberate pause
/// re-arms the kick for the next burst.
const OOPS_REARM: f32 = 0.30;
/// Backward recoil while the oops is live, as a fraction of cell width: the
/// cat rocks AWAY from the deletion — the "caught out" body tilt that makes
/// the reaction legible at a glance (the glyph swap alone is easy to miss at
/// terminal size). Pose-path only; reduced motion keeps the pure swap.
const OOPS_RECOIL: f32 = 0.12;
/// Sustained brace crouch while the oops holds: the `land_at` bounce decays
/// in ~0.4 s, so a held burst needs this standing squash to keep reading —
/// shorter by this fraction, wider by 0.6× of it (the landing shape law).
const OOPS_SQUASH: f32 = 0.07;
/// Recoil envelope easing: ease IN over `OOPS_ATTACK` from the burst kick,
/// ease OUT over the final `OOPS_SETTLE` of the hold — the pose returns to
/// cruise smoothly instead of snapping the instant the expression expires.
const OOPS_ATTACK: f32 = 0.07;
const OOPS_SETTLE: f32 = 0.18;

// ── the curse wince ────────────────────────────────────────────────────────
// A complete profanity cue makes an already-visible companion squeeze its
// eyes shut and recoil. Repeated words inside one phrase build a short chain:
// each completion re-kicks the pose and raises the force/pulse count, so
// `fuck fuck` reads as two beats rather than one held sticker. Prefixes never
// reach this API; the word engine emits only complete-token cues.
const WINCE_HOLD: f32 = 0.52;
const WINCE_CHAIN_WINDOW: f32 = 0.85;
const WINCE_ATTACK: f32 = 0.045;
const WINCE_SETTLE: f32 = 0.16;
const WINCE_RECOIL: f32 = 0.16;
const WINCE_SQUASH: f32 = 0.11;
const MAX_WINCE_CHAIN: u8 = 4;
/// The wince TUMBLE: on top of the recoil the body rocks side to side, a
/// decaying oscillation whose frequency rides the chain: one curse is a flinch,
/// a string of them visibly knocks the cat about. The renderer's pose transform
/// is a dest-rect scale + lead shift with no rotation, so the "tumble" is
/// spelled as this lateral lead swing plus a counter-phase scale wobble.
const WINCE_TUMBLE: f32 = 0.13;
const WINCE_TUMBLE_FREQ: f32 = 3.1;
const WINCE_TUMBLE_DECAY: f32 = 3.4;

// A complete FELINE word (any language) makes an already-visible companion
// light up: happy eyes plus a springing LEAP — it rises off its rest anchor,
// stretches tall at the top of the hop, and settles back with the shared
// landing bounce. Chains exactly like the wince, so `kitty kitty` reads as two
// hops. Like every reaction this is expression on an EXISTING flight: it never
// summons a hidden companion and never changes its identity.
const DELIGHT_HOLD: f32 = 0.62;
const DELIGHT_CHAIN_WINDOW: f32 = 0.9;
const DELIGHT_ATTACK: f32 = 0.05;
const DELIGHT_SETTLE: f32 = 0.2;
/// Peak leap height as a fraction of cell height, added to the hover bob.
/// Vertical stretch at the top of the hop (and the squash on the way out).
const DELIGHT_STRETCH: f32 = 0.14;
const MAX_DELIGHT_CHAIN: u8 = 4;

// ── living-cartoon animation tuning ─────────────────────────────────────────
// One eased "display momentum" spine drives every pose choice. It LAGS the raw
// momentum, so the body carries follow-through: it banks/stretches into speed,
// lands with a squash→stretch bounce, and blinks when it lingers. All of it is
// a pure function of the spine + the injected clock (no per-pixel work).
/// Spine follower time constant (seconds): how fast display-momentum chases the
/// live score. Short enough to feel responsive, long enough to trail = the
/// overlapping-action "the body catches up" tell.
const DISP_TAU: f32 = 0.13;
/// Spine range mapped to the banking curve: no lean below `BANK_LO`, full flying
/// lean by `BANK_HI`.
const BANK_LO: f32 = 0.32;
const BANK_HI: f32 = 0.96;
/// At full bank the cat STRETCHES along its motion axis (wider) and thins a
/// touch (shorter) — the classic "moving fast" stretch.
const STRETCH_X: f32 = 0.17;
const THIN_Y: f32 = 0.10;
/// At full bank the cat also lunges this fraction of a cell ahead of its rest
/// anchor — leaning into the motion.
const LEAD_MAX: f32 = 0.22;
/// Sustained-fast display momentum at/above which the cruising face squints
/// happily; a brief blink interrupts either face while visible. The
/// ceiling sits high enough that a lingering cat — which still carries real
/// residual momentum under the metric's 2 s τ — is blink-eligible.
const HAPPY_GATE: f32 = 0.80;
/// Deterministic idle-blink cadence: a full closure of `BLINK_DUR` every
/// `BLINK_PERIOD`, phased off the flight clock (never at the entrance).
const BLINK_PERIOD: f32 = 1.7;
const BLINK_DUR: f32 = 0.13;
/// Hover-bob amplitude (fraction of cell height). Every still path
/// ([`CursorCat::static_frame`], hidden, [`CatPose::STILL`]) pins `bob = 0`,
/// keeping reduced-motion presentation structurally static.
const BOB_AMP: f32 = 0.09;
/// THE STRIDE PUMP (owner: "the cursor cat can be more dynamic").
///
/// The cat's whole animation vocabulary — banking, stretch, thin, lead, the
/// happy/blink eyes — is driven by ONE input, an eased follower of the canonical
/// [`crate::typing_momentum::TypingMomentum`]. That metric is deliberately
/// RATE-NORMALIZED, so at every human cadence from 50 ms to 500 ms per key it
/// sits at its 1.0 clamp; and the cat's own visibility gate already requires
/// sustained momentum. So whenever the cat is on screen AND you are actually
/// typing, `bank == 1.0` exactly: every channel frozen, and the only live thing
/// a wall-clock sine the renderer then rounds to whole pixels.
///
/// The pump restores CADENCE as an animation driver, which is the one thing a
/// rate-normalized level structurally cannot carry: each committed forward
/// keystroke advances a stride phase, so the cat takes a visible step per key
/// and gallops when you type fast.
const STRIDE_PER_KEY: f32 = 0.5;
/// Idle drift (cycles/sec) between keystrokes, so a pause eases the cat through
/// its step instead of freezing it mid-stride. Deliberately the retired
/// wall-clock bob rate, so a RESTING cat reads exactly as it did before.
const STRIDE_IDLE_HZ: f32 = 1.4;
/// Body compression on each footfall. Small: the step should read as weight, not
/// as a bounce competing with the sing dance or the landing settle.
const STRIDE_SQUASH: f32 = 0.05;
/// Landing squash→stretch settle: a damped bounce of length `LAND_DUR`,
/// amplitude `LAND_AMP`, `LAND_FREQ` oscillations, decaying at `LAND_DECAY`.
const LAND_DUR: f32 = 0.42;
const LAND_AMP: f32 = 0.26;
const LAND_FREQ: f32 = 1.35;
const LAND_DECAY: f32 = 3.0;

/// SING-ALONG dance depths at full drive (`crate::kitty_sing`): the ON-BEAT
/// squash pulse (each beat lands as a bounce that relaxes across the beat)
/// and the two-beat side-to-side lean sway. Both scale with the drive, so
/// the wind-down crossfade eases the whole dance out — never a hard cut.
const SING_SQUASH: f32 = 0.14;
const SING_SWAY: f32 = 0.12;

/// One frame's sing-along sync from the host: the drive/beat pair the dance
/// rides. One struct so both render paths speak the same seam and a new
/// axis never forks the call sites again.
#[derive(Clone, Copy, Debug, Default)]
pub struct SingSync {
    /// Celebration drive 0..=1 (1 armed, easing through the wind-down).
    pub drive: f32,
    /// Beat phase in beats since the arm (fractional).
    pub beat: f32,
}

/// Smoothstep ramp of `x` across `[lo, hi]` → `0..=1` (eased at both ends).
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The living-cartoon pose the state machine hands the host each frame: a dest
/// rect scale (squash & stretch about the sprite centre), a forward lean/lunge,
/// and the baked eye frame. [`CatPose::STILL`] is the neutral identity — a hidden
/// or reduced-motion cat carries it, so nothing animates and the frame settles.
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
/// owns the behavioral clock; it deliberately does not know cell metrics,
/// custom sprite dimensions, pane offsets, or any renderer coordinate.
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
    fn default() -> Self {
        Self::STILL
    }
}

/// The little flourish the cat does on its way OUT.
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
    /// A delete while flying — Backspace or any kill chord: the authored
    /// tongue-out "ooops" ([`CatGlyphId::S121`]), held for the whole delete
    /// burst with a recoil pose kick, then decaying back to the base look.
    Startled,
    /// A complete profanity cue: squeezed eyes and a burst-scaled recoil.
    /// Repeated nearby cues increase the pose force and pulse count.
    Wince,
    /// A complete FELINE word was typed (`kitty`, `gato`, `neko`, … — see
    /// `aterm-gui`'s `kitty_summon`): happy eyes plus a springing leap.
    /// The companion's own name delights it; repeated words build the chain
    /// exactly like [`Self::Wince`] builds the recoil.
    Delight,
    /// A newly unlocked collectible's guaranteed hello.
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
    /// True for the complete collection hello, including its bounded fade.
    /// This is distinct from `discovery`: only the hold is forced, but the
    /// presentation must remain drawable until its exit reaches alpha zero.
    pub collection_hello: bool,
    /// Signed hover-bob as a fraction of cell height (≈±0.09): the cat gently
    /// bobs while flying so it reads as airborne, not a static sticker.
    pub bob: f32,
    /// The living-cartoon pose for this present: squash/stretch, forward lean,
    /// and the baked eye frame. [`CatPose::STILL`] when hidden or reduced-motion.
    pub pose: CatPose,
    /// SING-ALONG drive 0..=1 (`crate::kitty_sing`): 1.0 while the
    /// held-key celebration is armed, easing through its ~1 s wind-down
    /// crossfade, exactly 0.0 otherwise. Non-zero swaps the face to the
    /// singing head ([`CatFrame::render_look`]) and carries the beat-synced
    /// dance overlay already baked into `pose`/`bob` by the state machine.
    pub sing: f32,
}

impl CatFrame {
    /// Resolve the authored art for this expression. Typed ids keep additions to
    /// the sorted head roster from shifting what any expression selects.
    /// Delete deliberately prefers the AUTHORED tongue-out wink (`s1_21`,
    /// [`CatGlyphId::S121`] — "one bold eyelid and a tiny tongue-out smile"):
    /// no new frame is minted for the oops, the loved one is simply held.
    /// SINGING deliberately reuses the authored open-mouth MEOW head
    /// ([`CatGlyphId::S115`] — the heart-meow exit's face): the roster
    /// already holds a mouth-open singing expression, so no new reaction
    /// frame is minted (the art-flow additions stay reserved for genuinely
    /// missing expressions). It yields to the delete "oops" (the wrong-note
    /// gag outranks the song, exactly like the bonk outranks the riff) and
    /// to the exit flourishes, which own their own faces.
    #[must_use]
    pub fn render_look(&self) -> KittyLook {
        let variant = match (self.exit, self.reaction) {
            (CatExit::StarWink, _) | (_, CatReaction::Startled) => CatGlyphId::S121,
            (CatExit::HeartMeow, _) => CatGlyphId::S115,
            _ if self.sing > 0.33 => CatGlyphId::S115,
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
    pub fn fp(&self) -> u64 {
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
        // Quantize the pose so a change of lean / squash / stretch / blink
        // invalidates the frame while animating and settles to a stable key
        // once the spine is still (STILL → the same three integers every frame).
        let eyes_q = match self.pose.eyes {
            EyesFrame::Open => 0u64,
            EyesFrame::Happy => 1,
            EyesFrame::Blink => 2,
        };
        let sx_q = ((self.pose.scale_x * 256.0) as i64) as u64;
        let sy_q = ((self.pose.scale_y * 256.0) as i64) as u64;
        let lead_q = ((self.pose.lead * 512.0) as i64) as u64;
        // The sing drive swaps the face at 0.33 and scales the note alpha —
        // quantized fine enough that the crossfade re-presents smoothly.
        let sing_q = ((self.sing * 128.0) as i64) as u64;
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for value in [
            u64::from(self.alpha),
            (self.fade_out * 255.0) as u64,
            e,
            reaction,
            self.discovery as u64,
            self.collection_hello as u64,
            self.look.variant as u64,
            self.look.accessory.map_or(0, |a| a as u64 + 1),
            u64::from(self.look.coat),
            u64::from(self.look.iris),
            self.look.age as u64,
            bob_q,
            eyes_q,
            sx_q,
            sy_q,
            lead_q,
            sing_q,
        ] {
            h ^= value;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }
}

/// The inert hidden/retired frame: alpha 0, plain exit, no reaction, STILL
/// pose — byte-identical across frames, so [`CatFrame::fp`] settles to 0.
fn hidden_frame(look: KittyLook) -> CatFrame {
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

/// The EARNED cat's decision layer: the canonical typing-momentum metric + a
/// high-band dwell clock + a fade/exit state machine. `on_key` is stamped on
/// the input thread; `frame` advances the machine once per rendered frame.
pub struct CursorCat {
    /// THE canonical typing-momentum metric ([`crate::typing_momentum`]):
    /// this instance runs the same law, on the same keystream, as the glow
    /// engine's — the unified-readers proof in `cursor_glow` pins their
    /// values equal under one key script.
    momentum: TypingMomentum,
    /// Seconds of high-band dwell accrued across in-rhythm keys (resets the
    /// moment a key lands below [`CAT_BAND`]).
    sustain: f32,
    /// Forward keys in the current uninterrupted high-band run. This is an
    /// independent travel floor: dwell alone can never summon before sixteen
    /// qualifying events have carried the cursor forward.
    run_keys: u32,
    last: Option<Instant>,
    state: State,
    exit: CatExit,
    /// When the current flight began (FadeIn entry) — the hover-bob clock.
    flight: Option<Instant>,
    /// THE STRIDE PUMP's phase, in CYCLES (see [`STRIDE_PER_KEY`]). Advanced by
    /// every committed forward keystroke and drifting at [`STRIDE_IDLE_HZ`]
    /// between them, so the cat's step is driven by your cadence rather than by
    /// a level that is pinned whenever the cat is visible at all.
    stride: f32,
    /// Clock stamp of the last stride advance — the idle-drift `dt` source.
    /// `None` re-seeds without integrating, so a resumed or foreign clock cannot
    /// fling the phase forward.
    stride_at: Option<Instant>,
    /// The collected identity currently accompanying the cursor.
    look: KittyLook,
    /// APPEARANCE LATCH: a look change arriving while the companion is on
    /// screen parks HERE and is applied at the start of the NEXT appearance,
    /// so one appearance always wears one cat. See [`Self::set_look`] for the
    /// two-path rule.
    pending_look: Option<KittyLook>,
    reaction: CatReaction,
    reaction_until: Option<Instant>,
    /// A discovery cannot be grounded by low momentum before this deadline.
    discovery_until: Option<Instant>,
    /// The full collection presentation stays drawable through fade-out.
    collection_hello: bool,
    /// Start of an unfocused/suppressed interval for a collection hello. The
    /// promised hold is measured in drawable time, so resume shifts every
    /// presentation clock forward by this hidden duration.
    collection_paused_at: Option<Instant>,
    /// Quantized terminal context frozen for this flight/hello. The host
    /// supplies one footprint sample on the first visible frame; subsequent
    /// frames reuse it so text churn cannot fragment the atlas cache.
    colors: Option<CatColorKey>,
    /// Eased "display momentum" spine (0..1): a lagged follower of the live
    /// score that drives every pose choice. It trails the raw momentum, so the
    /// body carries follow-through into and out of speed.
    disp: f32,
    /// Last spine advance instant (the follower's `dt` source). `None` reseeds
    /// the spine to its target — a fresh flight starts banked, not from zero.
    disp_at: Option<Instant>,
    /// Start of a landing squash→stretch bounce (Enter stomp, a delete
    /// flinch, or the fade-out touchdown). `None` between bounces.
    land_at: Option<Instant>,
    /// The current oops burst's kick instant — the recoil envelope's attack
    /// clock. Set on the burst's FIRST delete only ([`OOPS_REARM`]).
    oops_at: Option<Instant>,
    /// The LAST delete of the current burst — the [`OOPS_REARM`] gap
    /// discriminator between "extend this burst" and "a new burst re-kicks".
    oops_last: Option<Instant>,
    /// First sample of the current curse-wince kick.
    wince_at: Option<Instant>,
    /// Last complete curse in the current short phrase.
    wince_last: Option<Instant>,
    /// Burst-scaled phrase strength/pulse count, bounded at four.
    wince_chain: u8,
    /// First sample of the current feline-delight kick (the leap clock).
    delight_at: Option<Instant>,
    /// Last complete feline word in the current short phrase.
    delight_last: Option<Instant>,
    /// Burst-scaled hop strength, bounded at [`MAX_DELIGHT_CHAIN`].
    delight_chain: u8,
    /// SING-ALONG drive 0..=1, synced by the host each frame
    /// ([`Self::set_singing`]) from the `kitty_sing` detector: 1.0 armed,
    /// crossfading through wind-down, 0.0 idle.
    sing: f32,
    /// The shared dance-beat phase in beats (fractional), from the same
    /// host sync. Meaningful only while `sing > 0`.
    sing_beat: f32,
    /// Authenticated line-fold choreography. The classifier and clock live in
    /// CursorGlow; CursorCat owns only the bounded behavioral episode, while
    /// WordDecorations resolves it into pixels for the current sprite/geometry.
    fold: Option<CatFold>,
    /// Resting direction after a reverse fold. An active fold's own direction
    /// wins in [`Self::placement_frame`].
    facing_left: bool,
    rng: u32,
}

impl Default for CursorCat {
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
    /// Retire an ordinary cursor-following flight after the host observes a
    /// cursor relocation that no authored movement candidate owned.
    ///
    /// Collection hellos and the sing-along are independent, explicitly
    /// promised presentations, so this fence leaves them alone.  An earned
    /// typing flight, however, is positioned from the live caret every frame;
    /// allowing it to survive a program-owned CUP would make the already-live
    /// sprite jump to an unrelated cell even though the trail engine correctly
    /// denied that move.
    pub fn retire_unowned_cursor_motion(&mut self) {
        if self.collection_hello || self.sing > 0.0 || matches!(self.state, State::Hidden) {
            return;
        }
        self.momentum = TypingMomentum::default();
        self.sustain = 0.0;
        self.run_keys = 0;
        self.last = None;
        self.state = State::Hidden;
        self.exit = CatExit::Plain;
        self.flight = None;
        self.stride = 0.0;
        self.stride_at = None;
        self.reaction = CatReaction::Cruise;
        self.reaction_until = None;
        self.discovery_until = None;
        self.collection_paused_at = None;
        self.colors = None;
        self.fold = None;
        self.facing_left = false;
        if let Some(pending) = self.pending_look.take() {
            self.look = pending;
        }
        self.reset_spine();
    }

    /// Consume the lossless classifier verdict produced by CursorGlow.
    ///
    /// Forward pulses replace the host's historical `on_key(at, true)` call,
    /// preserving the one canonical momentum law. Reverse folds do NOT call
    /// `on_key(false)`: the input seam already delivered that physical
    /// Backspace, and repeating it here would double-drain momentum and
    /// double-kick the oops reaction.
    pub fn on_motion_pulse(&mut self, pulse: crate::cursor_glow::CursorCatMotionPulse) {
        self.on_motion_pulse_with_owner(pulse, true);
    }

    /// Build the authenticated tenure used by pet mode's singing face without
    /// starting an ordinary flying episode. The resident pet owns idle glass,
    /// but the held-key song still owes the same real cursor-travel floor as
    /// classic mode; starving this feed makes that authored handoff unreachable.
    pub fn on_pet_mode_motion_pulse(
        &mut self,
        pulse: crate::cursor_glow::CursorCatMotionPulse,
    ) {
        self.on_motion_pulse_with_owner(pulse, false);
    }

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

    /// Sample the cat's bounded, geometry-free placement state. A sparse frame
    /// after the duration returns settled immediately; it never replays stale
    /// travel after an occlusion. The stale private stamp may remain until the
    /// next pulse/reset because this accessor is read-only, but it can no
    /// longer produce a fold once its wall-time bound elapsed.
    #[must_use]
    pub fn placement_frame(&self, now: Instant) -> CursorCatPlacementFrame {
        let fold = self.fold.and_then(|fold| {
            let progress = now.saturating_duration_since(fold.started).as_secs_f32()
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

    /// Retire only renderer-coordinate continuity. A LOCK A/B cursor/style/
    /// scroll divergence may not carry an in-flight edge fold into the next
    /// coordinate space, but a collection hello or singing appearance remains
    /// a presentation promise and must continue settled at the live caret.
    pub fn rebase_placement(&mut self) {
        self.fold = None;
        self.facing_left = false;
    }

    /// Stamp one committed text keystroke: `forward` for a normal char / space /
    /// newline (the cursor advances), `!forward` for a backspace. Only text keys.
    pub fn on_key(&mut self, now: Instant, forward: bool) {
        self.on_key_with_owner(now, forward, true);
    }

    fn on_key_with_owner(&mut self, now: Instant, forward: bool, ordinary_flight_owned: bool) {
        let dt = self
            .last
            .map(|l| now.saturating_duration_since(l).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.5);
        if forward {
            // The dwell clock reads the metric BEFORE this key's credit: the
            // band must have been HELD through the gap the key closes, not
            // merely re-touched by it. The per-key credit is CAPPED well
            // under an inter-word pause so a pause key can never buy the
            // dwell: the dwell is bought by keys landing in rhythm.
            let held = self.momentum.value(now);
            if held >= CAT_BAND {
                self.sustain += dt.min(SUSTAIN_CREDIT_MAX);
                self.run_keys = self.run_keys.saturating_add(1);
            } else {
                // LEAK, not RESET — see [`SUSTAIN_LEAK`] for why a reset
                // cliff has no good position. The run-key floor leaks with
                // the dwell, one key per drained `SUSTAIN_CREDIT_MAX`, so the
                // two gates stay in step.
                let drain = dt * SUSTAIN_LEAK;
                self.sustain = (self.sustain - drain).max(0.0);
                let lost = (drain / SUSTAIN_CREDIT_MAX) as u32;
                self.run_keys = self.run_keys.saturating_sub(lost);
            }
            // Build the canonical metric: rate-normalized inside `advance`,
            // so key-repeat floods earn no faster than real typing.
            // THE STRIDE PUMP: one step per key. Deliberately fed by the
            // keystroke and NOT by the momentum level, because the level is
            // already pinned whenever the cat is on screen — the cadence is the
            // only signal left that still varies with how you are typing.
            self.stride_advance(now);
            self.stride = (self.stride + STRIDE_PER_KEY).rem_euclid(1024.0);
            self.momentum.advance(now);
            // A FADING cat is LOYAL: resumed typing at speed revives it
            // mid-fade rather than forcing a complete re-earn. Re-enter FadeIn
            // at the equivalent alpha so opacity is continuous — no pop. A
            // COLLECTION hello's goodbye is exempt: its promised-hold
            // bookkeeping (`collection_hello`/pause clocks) must run to
            // completion, or a revived flight would carry hello state that can
            // freeze the machine on focus loss — the ordinary earn path takes
            // over after.
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
            }
        } else {
            self.sustain = 0.0; // a backspace breaks the flow
            self.run_keys = 0;
            // Deletes NEVER build — a mild drain (the one-metric law).
            self.momentum.delete(now);
            // The tongue-out "ooops" — burst-aware, so autorepeat reads as one
            // sustained reaction (see [`Self::note_oops`]).
            self.note_oops(now);
        }
        self.last = Some(now);
        // Earn the flight once BOTH the sustain clock crosses the dwell AND
        // the canonical metric's current high-band run has at least
        // MIN_RUN_KEYS forward events. DETERMINISTIC: a fully earned run ALWAYS
        // summons; no rarity roll can discard it.
        if ordinary_flight_owned
            && matches!(self.state, State::Hidden)
            && self.sustain >= CAT_DWELL
            && self.run_keys >= MIN_RUN_KEYS
        {
            self.summon(now);
        }
    }

    /// The Hidden → FadeIn summon shared by the earned summon in
    /// [`Self::on_key`] and the sing-along summon in [`Self::set_singing`]
    /// (each caller keeps its own gate). Appearance start: a look change
    /// deferred by the mid-appearance latch ([`Self::set_look`] path 1) lands
    /// HERE, on the wake — the fresh flight is the first moment a swap cannot
    /// read as the cat morphing mid-air. (The FadeOut revive in `on_key` is
    /// the same appearance continuing and deliberately keeps the latched
    /// look.)
    fn summon(&mut self, now: Instant) {
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

    /// Fire (or sustain) the delete "oops" on a LIVE companion — the bounded
    /// tongue-out expression swap ([`CatGlyphId::S121`]) plus the pose kick.
    /// Burst-aware: every delete EXTENDS the hold so the expression decays
    /// [`OOPS_HOLD`] after the LAST delete (a held autorepeat run is ONE
    /// continuous reaction that lets go shortly after the finger does), while
    /// the squash flinch + recoil attack fire only on the burst's FIRST
    /// delete — deletes within [`OOPS_REARM`] of the previous one never
    /// re-kick. A hidden companion ignores this entirely: reactions are
    /// expression state on an existing flight, never a summon.
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
            // The flinch: the shared landing squash→stretch bounce, kicked
            // once per burst so it PLAYS instead of re-freezing at impact.
            self.land_at = Some(now);
        }
    }

    /// Wince at one or more complete profanity cues. This is a bounded visual
    /// reaction on an already-present companion: it never summons a hidden cat
    /// and never changes typing momentum or the flight lifetime.
    ///
    /// Cues close enough to belong to one phrase build a four-step chain. Each
    /// call re-kicks the squash/recoil; the chain controls both force and pulse
    /// count, making repeated complete words visibly more dynamic than one.
    /// Returns whether a live companion accepted the reaction so a host that
    /// resolves cues after its current cat frame can request one follow-up draw.
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

    /// Light up at one or more complete FELINE words: the exact twin of
    /// [`Self::on_curse`] with the opposite affect — happy eyes and a springing
    /// leap instead of squeezed eyes and a recoil.
    ///
    /// This is expression on an already-present companion. It never summons a
    /// hidden cat, never touches typing momentum or the flight lifetime, and
    /// never touches `look`: typing the companion's name delights it, it does
    /// not REPLACE it.
    ///
    /// Words close enough to belong to one phrase build a bounded chain, so
    /// `kitty kitty` reads as two hops rather than one held pose. Returns
    /// whether a live companion accepted the reaction, so a host that resolves
    /// words after its current cat frame can request one follow-up draw.
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

    /// Stamp one KILL chord (Ctrl-K/U/W, Alt-D, forward Delete, a word-
    /// backspace): erasing a SPAN un-earns momentum like a big backspace —
    /// twice the single-delete drain, the same ≈two-deletes escalation the
    /// fire quench uses ([`crate::cursor_glow::CursorGlow::note_kill`],
    /// "killing a line un-earns momentum") — and fires the same bounded
    /// "oops" on a live companion: the reaction must not depend on WHICH
    /// delete spelling the hand used. Under the one-metric law the drain is
    /// MILD: one kill dents an earned flight (and always breaks the dwell), a
    /// kill FLOOD grounds it — deletes never build, but one slip of the hand
    /// does not vaporize minutes of earned flow. Never summons.
    pub fn on_kill(&mut self, now: Instant) {
        self.sustain = 0.0;
        self.run_keys = 0;
        self.momentum.kill(now);
        self.last = Some(now);
        self.note_oops(now);
    }

    /// Celebrate a committed command without summoning an otherwise-hidden
    /// companion. The reaction is a bounded pose swap on an already-live cat.
    pub fn on_enter(&mut self, now: Instant) {
        if self.is_active() {
            self.reaction = CatReaction::Celebrate;
            self.reaction_until = Some(now + Duration::from_secs_f32(CELEBRATE_HOLD));
            // The command lands: a satisfying squash→stretch stomp on the beat.
            self.land_at = Some(now);
        }
    }

    /// Host sync for the SING-ALONG (`crate::kitty_sing`): once per
    /// frame, BEFORE [`Self::frame`]/[`Self::static_frame`], with the
    /// detector's current drive and the shared dance-beat phase
    /// ([`SingSync`]).
    ///
    /// THE MOMENTUM BYPASS (documented, deliberate): while ARMED
    /// (`drive == 1`) the canonical typing-momentum metric is pinned to 1.0.
    /// Holding a key at repeat cadence IS maximal flow BY DEFINITION here —
    /// the celebration's whole premise — whereas the metric's rate-normalized
    /// accrual was designed to stop exactly that flood from out-earning real
    /// typing. The bypass rides the same `set_value` seam the collection
    /// hello's "guaranteed visible at full momentum" arm uses, so every
    /// consumer (ribbon saturation, star shower, and this cat's spine) reads
    /// genuine full flow with no second code path. An armed celebration may
    /// bypass the 1.5 s dwell, but it still owes the independent sixteen-event
    /// travel floor: the detector's sixteen-repeat arm cannot substitute for
    /// correlated cursor travel. The wind-down (`drive < 1`) pins nothing —
    /// the metric resumes its natural decay, which is the momentum half of the
    /// crossfade.
    pub fn set_singing(&mut self, now: Instant, sync: SingSync) {
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

    /// Install the companion's current identity — the host's per-frame
    /// verdict (a pinned favourite, else the tenured program cat, else the
    /// launch kitty) — without changing the lifecycle. This is an O(1) scalar compare/copy and is
    /// safe to call from the host's frame setup.
    ///
    /// THE TWO-PATH RULE for look changes: a companion must not switch to a
    /// different look mid-flight. Since the 2026-08-17 rulings the verdict
    /// changes rarely and deliberately — a program cat that has earned the
    /// cursor through the host's tenure gate, the base cat returning after it
    /// exits, a favourite pin — and this latch is what keeps any of those
    /// from morphing the sprite mid-air: the flying kitty swaps only BETWEEN
    /// appearances, which is the soft switch the owner asked for.
    ///
    /// 1. **Sync (this method) — LATCHED per appearance.** A verdict change
    ///    (a program earning or releasing the cursor, a favourite pinned from
    ///    another window) that lands while THIS companion is on screen must
    ///    never morph the sprite mid-appearance — one appearance wears one
    ///    cat. The new look parks in `pending_look` and is applied at the
    ///    start of the NEXT appearance (`on_key`'s Hidden→FadeIn earn). The
    ///    mid-fade revive (FadeOut→FadeIn in `on_key`) is the SAME appearance
    ///    continuing, so it deliberately does not consume the latch. While
    ///    hidden the sync applies immediately — there is nothing on screen to
    ///    protect.
    /// 2. **`on_collect` — IMMEDIATE, and USER-ACT-ONLY.** The favourite
    ///    pin's hello legitimately presents the cat the user just chose;
    ///    swapping to it is the point of the presentation, so it replaces
    ///    the look (and clears any parked sync) even mid-flight. Its only
    ///    route is the user's own act (the favourite pin). A discovery —
    ///    typed or in scanned OUTPUT text — records to the ledger but never
    ///    re-dresses this companion (owner rulings, 2026-08-07 and
    ///    2026-08-17): the host's drains return no look to present.
    pub fn set_look(&mut self, look: KittyLook) {
        let look = look.normalized();
        if self.is_active() || self.collection_hello {
            // On screen (or a paused hello's promise is outstanding): defer.
            // A sync that AGREES with the latched look clears the parking
            // slot — the host re-syncs every frame, so the slot always holds
            // the latest global verdict, never a stale intermediate.
            self.pending_look = (look != self.look).then_some(look);
        } else {
            self.look = look;
            self.pending_look = None;
        }
    }

    /// A newly unlocked collectible says hello exactly once. It is guaranteed
    /// visible for [`DISCOVERY_HOLD`] even at zero typing momentum (the metric
    /// is seeded full), then rejoins the ordinary fade-out path — beginning
    /// its goodbye at the hold deadline unless live typing has re-earned past
    /// [`REVIVE_GATE`] — and settles back to zero-idle.
    ///
    /// USER-ACT-ONLY BY CONTRACT (owner rulings, 2026-08-07 and 2026-08-17):
    /// the host's discovery drains — ambient grid scans and the typed summon
    /// alike — record to the ledger and never hand a look here, so the only
    /// route is the user's own act: the favourite pin. Output text that
    /// happens to say `cat` must neither activate nor re-dress the companion,
    /// and a typed `kitty` presents the cat it already has ([`Self::on_summon`]).
    pub fn on_collect(&mut self, now: Instant, look: KittyLook) {
        self.look = look.normalized();
        // Two-path rule, path 2 ([`Self::set_look`]): the hello IS the
        // presentation of the newly pinned cat, so the explicit replace above
        // supersedes any look the mid-appearance latch had parked.
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
        self.reaction = CatReaction::Discovery;
        self.reaction_until = Some(until);
        if matches!(self.state, State::Hidden | State::FadeOut(_)) {
            self.state = State::FadeIn(now);
            self.flight = Some(now);
        }
    }

    /// Present the companion because a FELINE WORD WAS TYPED, wearing the
    /// identity it already has.
    ///
    /// This is [`Self::on_collect`]'s lifecycle WITHOUT its look replacement.
    /// Typing the companion's name is not a reason to change it; there is no
    /// newly chosen cat to present, so nothing about it justifies swapping
    /// the sprite — the launch kitty stays the launch kitty (owner ruling,
    /// 2026-08-17: the cat is generated at launch and does not keep
    /// changing). Even a first-ever typed discovery rides THIS path now: it
    /// is collected into the ledger, but only a favourite pin (`on_collect`)
    /// re-dresses the companion.
    ///
    /// A hidden companion FADES IN (typing `kitty` must make a kitty appear —
    /// that is the whole point of the gesture), and a visible one simply
    /// delights in place: `CursorCat` enters `FadeIn` only from `Hidden`/
    /// `FadeOut`, so `kittykittykitty` extends one appearance rather than
    /// strobing it.
    pub fn on_summon(&mut self, now: Instant, hits: u8) {
        self.momentum.set_value(now, 1.0);
        self.last = Some(now);
        self.sustain = 0.0;
        self.run_keys = 0;
        self.exit = CatExit::Plain;
        self.colors = None;
        let waking = matches!(self.state, State::Hidden | State::FadeOut(_));
        if waking {
            // A summoned appearance is guaranteed visible for the same hold a
            // discovery gets, so the cat cannot flicker straight back out at
            // zero typing momentum.
            self.discovery_until = Some(now + Duration::from_secs_f32(DISCOVERY_HOLD));
            self.collection_hello = true;
            self.collection_paused_at = None;
            // A look parked by the mid-appearance latch lands on the wake —
            // the fresh appearance is the first moment a swap cannot read as
            // the cat morphing mid-air.
            if let Some(pending) = self.pending_look.take() {
                self.look = pending;
            }
            self.state = State::FadeIn(now);
            self.flight = Some(now);
        }
        // The leap is the reaction either way; `on_delight` requires a live
        // companion, which the wake above has just guaranteed.
        self.on_delight(now, hits.max(1));
    }

    /// Pause or resume a collection hello at the host's drawability boundary.
    /// Ordinary earned flights keep their existing focus behavior; only the
    /// one-shot discovery promise is frozen while it cannot be presented.
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
        self.flight = self.flight.map(|at| at + hidden);
        self.state = match self.state {
            State::FadeIn(at) => State::FadeIn(at + hidden),
            State::FadeOut(at) => State::FadeOut(at + hidden),
            state => state,
        };
    }

    fn collection_sample_time(&self, now: Instant) -> Instant {
        if self.collection_hello {
            self.collection_paused_at.unwrap_or(now)
        } else {
            now
        }
    }

    fn discovery_active(&self, now: Instant) -> bool {
        self.discovery_until.is_some_and(|until| now < until)
    }

    fn reaction_at(&mut self, now: Instant) -> CatReaction {
        if self.reaction_until.is_some_and(|until| now >= until) {
            self.reaction = CatReaction::Cruise;
            self.reaction_until = None;
            // The oops clocks retire WITH the expression: a stale attack clock
            // must not seed the next burst's envelope, and the next delete
            // after a full decay is by definition a fresh burst (its gap
            // exceeded `OOPS_HOLD` > `OOPS_REARM`), so nothing is lost.
            self.oops_at = None;
            self.oops_last = None;
        }
        self.reaction
    }

    /// Drift the STRIDE phase to `now` at [`STRIDE_IDLE_HZ`]. Called before each
    /// per-key advance and once per pose resolve. `dt` is clamped so a
    /// backgrounded window resumes with one ordinary frame's worth instead of
    /// collapsing the whole blind interval into a single jump.
    fn stride_advance(&mut self, now: Instant) {
        let dt = self
            .stride_at
            .map(|l| now.saturating_duration_since(l).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.10);
        self.stride_at = Some(now);
        if dt > 0.0 {
            self.stride = (self.stride + dt * STRIDE_IDLE_HZ).rem_euclid(1024.0);
        }
    }

    /// The airborne hover-bob, as a fraction of cell height.
    ///
    /// Takes NO time argument, and that is the point: since the feline-delight
    /// leap left the companion (see the note below) the hover is a pure read of
    /// the STRIDE PUMP, and the pump is advanced by keystrokes in
    /// [`Self::stride_advance`], not by the wall clock. A `now` parameter
    /// survived the leap's removal and sat unread — prose that promised a
    /// time-varying value the body could not deliver. Reading `self.stride`
    /// alone makes the keystroke-driven dependency the signature's only claim.
    fn bob(&self) -> f32 {
        // The HOVER rides the STRIDE PUMP rather than a wall clock, so the cat
        // steps in time with your keys: it gallops under a fast run and eases
        // through its step during a pause. A resting cat's drift rate is the
        // retired 1.4 Hz, so the idle read is unchanged and only the TYPING read
        // gained a beat. The delight leap below composes on top of it.
        let hover = match self.flight {
            Some(_) => (std::f32::consts::TAU * self.stride).sin() * BOB_AMP,
            None => 0.0,
        };
        // THE FELINE-WORD LEAP LEFT THE COMPANION (owner, 2026-08-04: "the
        // cursor kitty changes too unpredictably. I want the kitty animations to
        // be in the keyword kitties versus on the cursor"). Hearing its own name
        // is a reaction to a WORD, and that word now spawns its own living cat
        // (`word_decorations::CatIdlePose`), so the celebration plays where the
        // thing being celebrated actually is. The companion keeps the happy eyes
        // — the feeling reads without the body throwing itself up the screen
        // mid-sentence, which was the single largest unpredictable displacement
        // in the whole pose stack (`DELIGHT_LEAP` = 0.42 cells, chain-scaled).
        hover
    }

    /// Normalized 0..1 progress through the delight hop, or 0 when none is
    /// live. Shared by the leap offset and the stretch shaping so the apex of
    /// the arc and the peak of the stretch are the same instant.
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

    /// Advance the eased "display momentum" spine toward `target` (the live
    /// score). An exponential follower with `DISP_TAU`: it LAGS the raw momentum,
    /// so the body carries follow-through. `disp_at == None` reseeds it to the
    /// target, so a fresh flight opens already banked instead of springing up.
    fn advance_spine(&mut self, now: Instant, target: f32) {
        let target = target.clamp(0.0, 1.0);
        match self.disp_at {
            None => self.disp = target,
            Some(prev) => {
                let dt = now.saturating_duration_since(prev).as_secs_f32().min(0.1);
                let alpha = 1.0 - (-dt / DISP_TAU).exp();
                self.disp += (target - self.disp) * alpha;
            }
        }
        self.disp_at = Some(now);
    }

    /// Deterministic idle blink: a `BLINK_DUR` closure once per `BLINK_PERIOD`,
    /// phased off the flight clock so it never fires the instant the cat appears.
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

    /// Resolve the living-cartoon pose for this present: advance the spine, then
    /// map it to a banking squash/stretch + forward lean, overlay any landing
    /// bounce, and select the baked eye frame. A pure function of the spine + the
    /// injected clock + the frame's expression (`reaction`/`exit`).
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
        // THE STRIDE PUMP's body compression: the cat gathers on each footfall.
        // Phase-locked to `bob`, so the squash lands with the bottom of the step
        // and the two read as ONE motion. Gated by the SAME `bank` the rest of
        // the pose uses, so a resting cat keeps its natural silhouette. That the
        // gate saturates is the point of the split: the LEVEL says whether to
        // show a step at all, the CADENCE says where in the step we are — and
        // the pinned metric could only ever answer the first.
        self.stride_advance(now);
        if self.flight.is_some() {
            let foot = (std::f32::consts::TAU * self.stride).cos().max(0.0) * bank;
            scale_y *= 1.0 - STRIDE_SQUASH * foot;
            scale_x *= 1.0 + STRIDE_SQUASH * 0.7 * foot;
        }
        // Landing: a damped squash→stretch bounce. `q > 0` at the impact frame
        // (shorter + wider), overshoots to a stretch, then settles to zero.
        if let Some(t0) = self.land_at {
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
        // The delete "oops" recoil: while the tongue-out expression is live
        // the cat rocks BACK off its rest anchor and holds a light brace
        // crouch — the at-a-glance half of the emphasis (a 16 px glyph swap
        // alone is easy to miss). The renderer's pose transform is a dest-rect
        // scale + lead shift (no rotation), so the "tilt" is spelled as this
        // backward lean + squash. Eased in over `OOPS_ATTACK` from the burst
        // kick and out over the final `OOPS_SETTLE` of the hold, so expiry
        // releases smoothly instead of snapping; the burst-extended hold
        // keeps `env` at 1 for as long as the deletes keep coming — one
        // sustained recoil, exactly like the sustained expression above it.
        if matches!(reaction, CatReaction::Startled) {
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
            // The recoil must READ at any momentum (the emphasis contract). A
            // startled cat can still be carrying a full forward banking lunge,
            // which subtracting OOPS_RECOIL alone would not overcome, so while
            // the oops env is live the bank lead is SUPPRESSED with it — the
            // cat pulls up short, THEN rocks back.
            lead = lead * (1.0 - env) - OOPS_RECOIL * env;
            scale_y *= 1.0 - OOPS_SQUASH * env;
            scale_x *= 1.0 + OOPS_SQUASH * 0.6 * env;
        }
        // Complete-curse wince: unlike the sustained delete brace, every word
        // re-kicks this recoil. A short phrase builds `wince_chain`, increasing
        // both force and the number of small recoil pulses while remaining
        // bounded. The base collected look stays intact; squeezed eyes plus
        // body motion carry the expression.
        if matches!(reaction, CatReaction::Wince) {
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
            // The TUMBLE: a decaying lateral rock on top of the recoil, its
            // frequency riding the chain so a string of curses visibly knocks
            // the cat about. Counter-phase scale wobble sells the roll that the
            // rotation-free pose transform cannot express directly.
            let swing = (-WINCE_TUMBLE_DECAY * elapsed).exp()
                * (std::f32::consts::TAU * WINCE_TUMBLE_FREQ * f32::from(chain).sqrt() * elapsed)
                    .sin();
            lead += WINCE_TUMBLE * swing * attack * settle;
            scale_x *= 1.0 + 0.05 * swing * attack * settle;
            scale_y *= 1.0 - 0.05 * swing * attack * settle;
        }
        // FELINE-word delight: the springing leap. One smooth hop — rise,
        // stretch tall at the apex, squash back on the way down — scaled by the
        // bounded phrase chain. The vertical offset itself rides `bob` (see
        // `delight_leap`); here only the body's stretch/squash is shaped, so the
        // hop reads as a spring rather than a translated sticker.
        if matches!(reaction, CatReaction::Delight) {
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
            let arc = self.delight_arc(now);
            let env = attack * settle;
            // The BODY shaping went with the leap (see `bob`): a spring with
            // nothing to spring off reads as a twitch, so what is left is a
            // small, brief swell — the cat perking up at its own name — at a
            // third of the hop's amplitude and no forward lean at all.
            let stretch = (std::f32::consts::PI * arc).sin() * env;
            scale_y *= 1.0 + DELIGHT_STRETCH * 0.33 * stretch;
            scale_x *= 1.0 - DELIGHT_STRETCH * 0.18 * stretch;
        }
        // SING-ALONG DANCE overlay (`crate::kitty_sing`): a beat-synced loop
        // over the banking spine — each beat lands as a squash bounce that
        // relaxes across the beat, and the body sways side to side once per
        // two beats. Everything scales with the drive, so the wind-down
        // crossfade eases the dance out through the same multiplies. Runs on
        // the shared beat clock, never the frame rate — a dropped frame
        // resumes mid-dance, not mid-glitch. The delete "oops" recoil above
        // still reads through it (the wrong-note gag outranks the song).
        if self.sing > 0.0 {
            let u = self.sing_beat.fract();
            let pulse = (1.0 - u) * (1.0 - u);
            scale_y *= 1.0 - SING_SQUASH * pulse * self.sing;
            scale_x *= 1.0 + SING_SQUASH * 0.7 * pulse * self.sing;
            lead += SING_SWAY * (std::f32::consts::PI * self.sing_beat).sin() * self.sing;
        }
        // Expression: blink/squint only over the plain cruising/discovery face —
        // the wink/celebrate reactions own their own eyes via a variant swap.
        let plain_face = matches!(exit, CatExit::Plain)
            && matches!(reaction, CatReaction::Cruise | CatReaction::Discovery);
        let eyes = if matches!(reaction, CatReaction::Wince) {
            EyesFrame::Blink
        } else if matches!(reaction, CatReaction::Delight) {
            // Hearing its own name is the happiest the companion gets.
            EyesFrame::Happy
        } else if !plain_face {
            EyesFrame::Open
        } else if self.blink_active(now) && self.sing <= 0.33 {
            // A BLINK OUTRANKS THE CRUISING FACE. This arm used to sit BELOW the
            // happy face and was additionally gated on `disp < BLINK_CEIL` — but
            // `disp` is pinned at 1.0 at every human cadence whenever the cat is
            // visible, so BOTH conditions failed together and the cat could not
            // blink at any point during actual typing. It stared, wide-eyed, for
            // the entire flight. A 130 ms blink every 1.7 s costs nothing and is
            // the cheapest life in the sprite. SINGING still outranks it: the
            // open-mouth meow head is a different baked head, and blinking
            // through a song reads as a glitch rather than as breathing.
            EyesFrame::Blink
        } else if self.sing > 0.33 || self.disp >= HAPPY_GATE {
            // Singing is sung with happy eyes (over the open-mouth meow head
            // `render_look` swaps in — the same gate value).
            EyesFrame::Happy
        } else {
            EyesFrame::Open
        };
        CatPose {
            scale_x,
            scale_y,
            lead,
            eyes,
        }
    }

    /// Settle the spine to fully idle (hidden cat ⇒ zero animation work): the
    /// display momentum resets, the follower disarms, and any in-flight landing
    /// bounce clears, so the next hidden frame is byte-identical off.
    fn reset_spine(&mut self) {
        self.disp = 0.0;
        self.disp_at = None;
        self.land_at = None;
        self.oops_at = None;
        self.oops_last = None;
        self.wince_at = None;
        self.wince_last = None;
        self.wince_chain = 0;
        self.delight_at = None;
        self.delight_last = None;
        self.delight_chain = 0;
    }

    /// Resolve a reduced-motion collection hello without advancing the
    /// fade/bob machine. The discovery pose is a fully opaque still for the
    /// bounded hold, then disappears in one step. Ordinary earned flights are
    /// not rendered by this path.
    pub fn static_frame(&mut self, now: Instant) -> CatFrame {
        // A reduced-motion sample is a settled placement verdict, not a paused
        // off-glass seam that may resume halfway through when policy changes.
        self.fold = None;
        self.facing_left = false;
        let now = self.collection_sample_time(now);
        let look = self.look;
        if self.collection_hello && self.discovery_active(now) {
            // Reduced motion honors the loved delete "oops" as a STATIC
            // expression swap ONLY: a live oops shows the authored tongue-out
            // face (`render_look` → S121) at the hello's full opacity, with
            // the pose pinned STILL and `bob = 0` — no recoil, no squash
            // kick. Stillness is structural here (this path never calls
            // `resolve_pose`), not a flag. Every other reaction state keeps
            // the plain discovery face, exactly as before.
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
                // The hello's bounded promise owns this presentation — no
                // singing overlay competes with the discovery still.
                sing: 0.0,
                pose: CatPose::STILL,
            };
        }
        if self.collection_hello {
            self.state = State::Hidden;
            self.flight = None;
            self.reaction = CatReaction::Cruise;
            self.reaction_until = None;
            self.discovery_until = None;
            self.collection_hello = false;
            self.collection_paused_at = None;
            self.colors = None;
        }
        // STATIC CELEBRATION (the reduced-motion arm of the sing-along): a
        // held-key celebration presents as a fully opaque STILL — the
        // singing face without the dance loop (pose STILL, bob 0 — the same
        // structural stillness as the hello above), appearing while the
        // drive holds the authored singing face and disappearing in one step
        // below its 0.33 face-swap threshold (the hello's one-step law; a
        // crossfade is motion this path refuses). Keeping the still through
        // 0.33 is also the late-frame safety net: a render occluded through
        // the first half of wind-down can sample 1.0 -> 0.49 directly while
        // the caret-fed resident finishes readying behind the opaque still.
        // The riff is host policy and plays independently if sound is on.
        if self.sing >= 0.33 {
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
        // ORDINARY EARNED FLIGHT under reduced motion (M3): the animated
        // `frame` lifecycle never runs on this path, so a flight summoned while
        // motion is reduced (the earned `on_key` summon, or a celebration wound
        // below the static-sing gate) would otherwise LATCH — `is_active` never
        // clears, the mid-appearance look latch ([`Self::set_look`]) wedges
        // permanently, and when motion resumes `frame` would find the machine
        // still Shown and pop the cat in at full alpha UNEARNED. Drive the SAME
        // FadeIn→Shown→FadeOut→Hidden lifecycle by WALL TIME instead — no
        // interpolation (alpha snaps to the state's static endpoint, and an
        // ordinary flight is not drawn under reduced motion anyway), so the
        // flight retires to Hidden, `is_active` clears, and the latch releases.
        //
        // Gated to `sing == 0`: a winding-down celebration (`0 < sing < 0.33`)
        // keeps the hello's ONE-STEP disappearance in the fall-through below
        // (it reads alpha 0 the instant the drive drops below the face swap), and it
        // retires through this very lifecycle once the drive settles to 0.
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
                // Retired: release the flight, its reactions, and the
                // mid-appearance look latch — nothing is on screen to protect,
                // so a look parked while the flight was live applies HERE, the
                // next static wake wears it.
                self.flight = None;
                self.reaction = CatReaction::Cruise;
                self.reaction_until = None;
                self.discovery_until = None;
                self.colors = None;
                if let Some(pending) = self.pending_look.take() {
                    self.look = pending;
                }
                self.reset_spine();
                return hidden_frame(self.look);
            }
            // Still airborne: snap alpha to the static endpoint of the
            // (possibly advanced) state. Not drawn under reduced motion — this
            // only keeps `is_active` honest until the machine retires above.
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
    /// the erase deadline, or — when a delete "oops" is live — the earlier
    /// expression-decay instant. The oops swap itself rides the delete key's
    /// own redraw, but the swap BACK has no key to ride; without this wake
    /// the tongue-out face would stick until the erase. An expired deadline
    /// is never returned: the deadline event already asks for its redraw, and
    /// rearming a past instant could spin if an occluded compositor withholds
    /// that redraw.
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

    /// Freeze the first prospective-footprint palette sample for the current
    /// visible episode. This is O(1) and keeps later animation frames on the
    /// same atlas tile even when terminal colors beneath the cat change.
    pub fn colors_for_episode(&mut self, sampled: CatColorKey) -> CatColorKey {
        *self.colors.get_or_insert(sampled)
    }

    /// Return the already-frozen palette without sampling the terminal again.
    /// The host checks this before its bounded footprint walk, making every
    /// frame after the first an O(1) cache hit with no WCAG color math.
    #[must_use]
    pub fn episode_colors(&self) -> Option<CatColorKey> {
        self.colors
    }

    /// Advance the fade/exit machine one frame and resolve the draw state.
    pub fn frame(&mut self, now: Instant) -> CatFrame {
        let now = self.collection_sample_time(now);
        let s = self.decayed(now);
        let bob = {
            let cruise = self.bob();
            if self.sing > 0.0 {
                // DANCE BOB, drive-blended over the cruise hover: rides the
                // shared beat clock (2.5 Hz at the sing-along's 150 BPM vs
                // the 1.4 Hz cruise bob).
                let dance = (std::f32::consts::TAU * self.sing_beat).sin() * BOB_AMP;
                cruise + (dance - cruise) * self.sing
            } else {
                cruise
            }
        };
        // Preserve the real hold deadline before retiring it.  A late present
        // must sample the lifecycle at wall-clock `now`; it must not restart a
        // full fade from the first frame the compositor happens to deliver.
        // This is the sparse-frame/gapped-present contract that prevents a cat
        // from snapping back into view after a terminal has been quiet.
        let discovery_until = self.discovery_until;
        let discovery = self.discovery_active(now);
        if !discovery {
            self.discovery_until = None;
        }
        let reaction = self.reaction_at(now);
        let look = self.look;
        // GROUNDING: an ordinary flight lets go under [`LOW`] (the metric's
        // ~3.9 s-of-silence glide-out). A COLLECTION hello whose hold just
        // expired must stay BOUNDED by design, not by decay luck: with the
        // canonical metric's 2 s τ the `on_collect` full seed still reads
        // ~0.25 at hold expiry — above LOW — so the hello also grounds
        // whenever the metric is under [`REVIVE_GATE`], i.e. unless live
        // typing has genuinely re-earned the flight (the same threshold that
        // revives a fading one: loyalty is symmetric, the promise is not
        // open-ended).
        let grounded = s < LOW || (self.collection_hello && s < REVIVE_GATE);
        match self.state {
            State::Hidden => {
                // Hidden ⇒ zero animation work: the spine settles, so the next
                // hidden frame is byte-identical off (fp 0, timer disarmed).
                self.reset_spine();
                hidden_frame(look)
            }
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
                    self.reset_spine();
                    return hidden_frame(look);
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
    fn grounded_exit(&mut self, discovery_until: Option<Instant>, now: Instant) -> CatFrame {
        let started = if self.collection_hello {
            discovery_until.unwrap_or(now)
        } else {
            self.low_crossing_at(now)
        };
        self.begin_fade_out_at(started, now)
    }

    /// Exact wall-clock instant at which an earned flight crossed [`LOW`].
    /// `frame` may be called arbitrarily late, so deriving this boundary
    /// analytically from the metric's decay law
    /// ([`TypingMomentum::low_crossing`]) is what makes the lifecycle
    /// frame-rate independent.  The caller only uses it after observing
    /// `decayed(now) < LOW`, hence the returned instant is never in the future.
    fn low_crossing_at(&self, now: Instant) -> Instant {
        self.momentum.low_crossing(LOW).unwrap_or(now)
    }

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
        // Re-sample the FadeOut state at the caller's actual time. This is one
        // bounded recursion (the state is no longer FadeIn/Shown), and it also
        // settles directly to Hidden when the entire fade elapsed in a frame
        // gap instead of leaking one newly-opaque frame.
        self.frame(now)
    }

    /// On screen (fading or shown) ⇒ keep the host's 60 fps re-arm going.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self.state, State::Hidden)
    }

    /// The canonical typing-momentum metric at `now` (lazily decayed) — the
    /// same number the glow engine's spine chases. Public so hosts/tests can
    /// observe the unified metric (the unified-readers proof reads it).
    #[must_use]
    pub fn momentum(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }

    fn decayed(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }
    fn xorshift(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }
    /// 20% star wink, 20% heart meow, else a plain fade.
    fn roll_exit(&mut self) -> CatExit {
        match self.xorshift() % 5 {
            0 => CatExit::StarWink,
            1 => CatExit::HeartMeow,
            _ => CatExit::Plain,
        }
    }
}

/// The DARK arm's heart pink — additive light over a near-black ground.
const EXIT_HEART_PINK: u32 = 0x00FF_5C8A;

/// THE EXIT FLOURISH'S LIGHT-THEME INKS, through the SHARED RECIPE.
///
/// On LIGHT the additive pink washes out to nothing and the additive white is
/// the single worst colour there is (you cannot brighten a pale ground toward
/// white and see a mark), so the light arm paints SOURCE-OVER veils that DARKEN
/// the ground instead. They used to be two hand-picked constants —
/// `0x00C2_1852` and `0x00B8_6A00` — i.e. two more independent answers to a
/// question this crate answers in exactly one place.
///
/// The flourish rises beside the cat and can land on a line of text at any
/// moment, so both take the OVER-TEXT role of
/// [`crate::cursor_glow::InkRole`]'s ONE LIGHT-INK RULE — the same policy every
/// star, sparkle and poof grain takes — seeded from the DARK arm's own hue so
/// the two themes draw the same mark.
///
/// The star seeds from the family GOLD rather than from the dark arm's white,
/// exactly as the rainbow ribbon's landing sparkles do: a white has no hue to
/// carry, and this way the light sparkle stays warm. It also sidesteps the
/// LEADING policy's achromatic collapse — `light_ink_bold` normalizes to a PURE
/// hue and used to return literal BLACK for a white input — which is fixed at
/// the source, but a white star would still have come out neutral grey.
/// Pinned by `exit_flourish_inks_come_from_the_shared_recipe`.
#[inline]
fn exit_heart_light() -> u32 {
    InkRole::OverText.ink(EXIT_HEART_PINK)
}
#[inline]
fn exit_star_light() -> u32 {
    InkRole::OverText.ink(twinkle_rgb(true))
}

/// Draw the fade-out FLOURISH (a heart rising for HeartMeow, a sparkling star
/// for StarWink) near the cat anchor `(ax, ay)` in WINDOW-ABSOLUTE pixels —
/// hosts must pass anchors that already include the grid origin
/// (`geom.origin_x/origin_y`); all internal offsets here are relative to that
/// anchor. `fade_out` is 0..1. No-op for `Plain`.
///
/// THEME-AWARE (the light-theme legibility law). On `dark_theme` the flourish is
/// additive [`GlowQuad`]s into `out` (added light over the
/// near-black ground). On LIGHT additive white/pink is invisible (you cannot
/// brighten a pale ground into a visible mark), so the heart/star are instead
/// emitted as SOURCE-OVER [`RainHalo`] veils ([`HaloMode::Over`]) into `halos`,
/// in the darkened saturated [`exit_heart_light`] / [`exit_star_light`] inks that
/// DARKEN the ground and read on any background — the same veil discipline the
/// ribbon rails and the fresh-ink light pop use. The two sinks are disjoint per
/// theme: exactly one is written per call, so a caller may pass the same frame's
/// additive + halo scratch buffers unconditionally.
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
                let rows: [(i32, i32); 6] = [(1, 2), (0, 6), (0, 6), (1, 5), (2, 4), (3, 3)];
                let bumps = [(4, 5)]; // second top bump on row 0
                let pink = premul_rgb(EXIT_HEART_PINK, cov);
                for (ry, &(x0, x1)) in rows.iter().enumerate() {
                    push_fx(
                        out,
                        geom,
                        hx + x0 * u,
                        hy + ry as i32 * u,
                        (x1 - x0 + 1) * u,
                        u,
                        pink,
                    );
                }
                for &(x0, x1) in &bumps {
                    push_fx(out, geom, hx + x0 * u, hy, (x1 - x0 + 1) * u, u, pink);
                }
            } else {
                // LIGHT: one darkened-rose source-over veil centred on the heart.
                // Bounded by the OVER-TEXT role's legibility ceiling — the ink
                // comes from that role, so the ceiling has to as well.
                let alpha = (210.0 * pop).min(InkRole::OverText.alpha_cap()) as u8;
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
        CatExit::StarWink => {
            // PHOTOSENSITIVITY. `fade_out` is `elapsed / FADE_OUT` with FADE_OUT =
            // 0.75, so the retired 18.0 advanced at 24 rad/s = 3.82 Hz — a 40%
            // amplitude swing on a 255-coverage pure-white star, the FASTEST
            // luminance oscillator in this family, and it EXCEEDED the 3.2 Hz
            // invariant its sibling module certifies (`cursor_rainbow::
            // TWINKLE_SCINT`) with nothing pinning it. Lowered to 13.0 = 2.76 Hz:
            // a pass that makes everything else brighter must not leave this one
            // hotter AND faster. Visually indistinguishable — the wink still glints
            // once over its exit, it simply no longer strobes.
            let twinkle = 0.6 + 0.4 * (fade_out * EXIT_STAR_SCINT).sin();
            let sx = ax + geom.cw as i32; // by the cat's face
            let sy = ay - ch / 3;
            let a = (ch / 5).max(2);
            if dark_theme {
                let cov = (255.0 * pop * twinkle).clamp(0.0, 255.0) as u8;
                if cov == 0 {
                    return;
                }
                let star = premul_rgb(twinkle_rgb(false), cov);
                push_fx(out, geom, sx - a, sy, 2 * a + 1, 1, star); // h arm
                push_fx(out, geom, sx, sy - a, 1, 2 * a + 1, star); // v arm
                let d = a / 2;
                push_fx(
                    out,
                    geom,
                    sx - d,
                    sy - d,
                    2 * d + 1,
                    1,
                    premul_rgb(twinkle_rgb(false), cov / 2),
                );
            } else {
                // LIGHT: a source-over amber sparkle — a cross of two veils so
                // the star SHAPE survives while darkening (never brightening)
                // the ground. Twinkle rides the alpha, matching the dark arm.
                let alpha = (230.0 * pop * twinkle).clamp(0.0, InkRole::OverText.alpha_cap()) as u8;
                let (cx, cy) = (sx as f32, sy as f32);
                let af = a as f32;
                push_exit_veil(
                    halos,
                    geom,
                    cx,
                    cy,
                    1.4 * af,
                    0.5 * af,
                    exit_star_light(),
                    alpha,
                ); // h arm
                push_exit_veil(
                    halos,
                    geom,
                    cx,
                    cy,
                    0.5 * af,
                    1.4 * af,
                    exit_star_light(),
                    alpha,
                ); // v arm
            }
        }
    }
}

/// Push one SOURCE-OVER radial veil ([`HaloMode::Over`]) of the exit flourish:
/// `rgb` is a STRAIGHT `0x00RRGGBB` ink and `alpha` (0..=255) is stamped into the
/// colour's HIGH BYTE as the centre opacity CEILING (`aterm_render::halo_over_cap`),
/// scaled per-pixel by the elliptical radial falloff toward 0 at the radii. The
/// band-splitting + EFFECTS-BOX clamp mirror the additive [`push_fx`], so a veil
/// spanning cell rows is emitted as one [`RainHalo`] per row (the dirty-gate /
/// scissor contract). Skipped honestly once the centre alpha is perceptually nil.
#[allow(
    clippy::too_many_arguments,
    reason = "centre + per-axis radii + ink + alpha; a param struct would obscure the geometry at the flourish call sites"
)]
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
    let rxi = (rx.round() as i32).max(1);
    let ryi = (ry.round() as i32).max(1);
    let (cxi, cyi) = (cx.round() as i32, cy.round() as i32);
    let (bl, br) = (geom.fx_left(), geom.fx_right());
    let (bt, bb) = (geom.fx_top(), geom.fx_bot());
    let x0 = (cxi - rxi).max(bl);
    let x1 = (cxi + rxi).min(br);
    let y0 = (cyi - ryi).max(bt);
    let y1 = (cyi + ryi).min(bb);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = geom.origin_y as i32;
    let mut yy = y0;
    while yy < y1 {
        // Grid-row DAMAGE HINT, anchored at origin_y: an above-grid band tags
        // row 0 (opening the top scissor band), never a wrapped u16.
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(RainHalo {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color,
            cx: cxi.clamp(0, br) as u16,
            cy: cyi.clamp(0, bb) as u16,
            rx: rxi as u16,
            ry: ryi as u16,
            mode: HaloMode::Over,
        });
        yy = band_end;
    }
}

/// Push one additive rect as per-cell-row [`GlowQuad`]s, clamped to the WINDOW.
fn push_fx(out: &mut Vec<GlowQuad>, geom: Geom, x: i32, y: i32, w: i32, h: i32, premul: u32) {
    if w <= 0 || h <= 0 || premul == 0 {
        return;
    }
    // EFFECTS BOX (grid + head band): identity-exact at head 0; a below-grid
    // band would only be skipped by the renderers' row gates.
    let (x0, x1) = (x.max(geom.fx_left()), (x + w).min(geom.fx_right()));
    let (y0, y1) = (y.max(geom.fx_top()), (y + h).min(geom.fx_bot()));
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = geom.origin_y as i32;
    let mut yy = y0;
    while yy < y1 {
        // Grid-row DAMAGE HINT, anchored at origin_y (above-grid bands tag row 0).
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
        });
        yy = band_end;
    }
}

/// Nearest-neighbour resample a straight-alpha RGBA8 image `src` (`sw×sh`) to
/// `tw×th`. Crisp for pixel-art upscales (the user-sprite case). Returns
/// `tw*th*4` bytes.
pub fn resample_nearest(src: &[u8], sw: usize, sh: usize, tw: usize, th: usize) -> Vec<u8> {
    let mut out = vec![0u8; tw * th * 4];
    if sw == 0 || sh == 0 || tw == 0 || th == 0 || src.len() < sw * sh * 4 {
        return out;
    }
    for ty in 0..th {
        let sy = ty * sh / th;
        for tx in 0..tw {
            let sx = tx * sw / tw;
            let s = (sy * sw + sx) * 4;
            let d = (ty * tw + tx) * 4;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
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

    /// PHOTOSENSITIVITY, pinned. The cat's exit StarWink is a 255-coverage
    /// pure-white star carrying a 40% amplitude swing, and it was the FASTEST
    /// luminance oscillator anywhere in this family at 3.82 Hz — over the 3.2 Hz
    /// bound [`crate::cursor_rainbow`] certifies for its own twinkle, with nothing
    /// pinning it. Same invariant, same shape of test as that sibling.
    #[test]
    fn cat_exit_starwink_flash_rate_stays_under_the_photosensitivity_bound() {
        let hz = EXIT_STAR_SCINT / FADE_OUT / std::f32::consts::TAU;
        assert!(hz <= 3.2, "the exit wink flashes at {hz} Hz");
        // Anti-vacuity: the bound above is also satisfied by EXIT_STAR_SCINT = 0,
        // which passes by DELETING the wink rather than pacing it. Compile-time
        // (the file's own idiom, see `V056_MIN_RUN_KEYS`) because both operands
        // are constants — a runtime assert over constants is what tippy rejects.
        const { assert!(EXIT_STAR_SCINT > 0.0, "the wink still glints") };
    }

    /// THE EXIT FLOURISH'S LIGHT INKS COME FROM THE SHARED RECIPE, not from two
    /// more hand-picked constants. Both marks rise beside the cat and can land
    /// on a line of text, so both take the OVER-TEXT role, seeded from the DARK
    /// arm's own hue (the star from the family gold — a white has no hue to
    /// carry, exactly as the ribbon's landing sparkles decided).
    ///
    /// AND NEITHER IS BLACK. The LEADING policy normalizes to a PURE hue and
    /// used to collapse an achromatic input to literal black, which is what a
    /// naive routing of the white star would have produced.
    #[test]
    fn exit_flourish_inks_come_from_the_shared_recipe() {
        assert_eq!(exit_heart_light(), InkRole::OverText.ink(EXIT_HEART_PINK));
        assert_eq!(exit_star_light(), InkRole::OverText.ink(twinkle_rgb(true)));
        for ink in [exit_heart_light(), exit_star_light()] {
            assert_ne!(ink & 0x00FF_FFFF, 0, "a light-theme ink is never black");
        }
        // The white star seed would be neutral; the gold seed stays WARM.
        let (r, _, b) = (
            (exit_star_light() >> 16) & 0xff,
            (exit_star_light() >> 8) & 0xff,
            exit_star_light() & 0xff,
        );
        assert!(r > b, "the light exit sparkle keeps its warmth");
        // And the LEADING policy — the one this mark must NOT take — no longer
        // answers black for an achromatic hue either.
        assert_ne!(InkRole::Leading.ink(twinkle_rgb(false)) & 0x00FF_FFFF, 0);
        // Both veils stay under the over-text legibility ceiling.
        let g = geom();
        for exit in [CatExit::HeartMeow, CatExit::StarWink] {
            let (mut out, mut halos) = (Vec::new(), Vec::new());
            emit_exit_fx(exit, 0.5, 40, 40, false, g, &mut out, &mut halos);
            assert!(out.is_empty(), "the light arm draws no additive light");
            assert!(!halos.is_empty(), "{exit:?} draws a veil on light");
            assert!(
                halos
                    .iter()
                    .all(|h| ((h.color >> 24) & 0xff) as f32 <= InkRole::OverText.alpha_cap()),
                "{exit:?} veil stays under the over-text ceiling"
            );
        }
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

    /// A stray forward burst that isn't SUSTAINED long enough never summons the cat.
    #[test]
    fn short_burst_never_summons() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        // 5 fast keys (~0.4s) — nowhere near the band, let alone the dwell.
        for i in 0..5 {
            c.on_key(t + Duration::from_millis(i * 40), true);
        }
        assert!(!c.is_active(), "a short burst does not earn the cat");
        assert_eq!(c.frame(t + Duration::from_millis(220)).alpha, 0);
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
        assert!(promised.is_active(), "a collection hello is independently owned");
        assert!(
            promised
                .frame(now + Duration::from_secs_f32(FADE_IN * 0.5))
                .alpha
                > 0,
            "the independently owned hello remains visible after its fade-in begins"
        );
    }

    /// A CASUAL burst — a word or two at speed — never summons the companion,
    /// and never even enters the high band the summon dwell counts in. Pins the
    /// "earned, not ambient" contract against any loosening of [`CAT_BAND`].
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

    /// THE EARN LAW: touching the high band is not enough — the metric must be
    /// HELD there for the full [`CAT_DWELL`]. Band entry (~1.4 s of flat-out
    /// typing) plus the dwell puts the summon in the 2.2–3.6 s window this
    /// pins from both sides.
    #[test]
    fn cat_requires_the_high_band_held_not_merely_touched() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let mut band_entry_key = None;
        let mut summoned_at_key = None;
        // ~9.6 s of headroom at 40 ms/key — band entry + dwell summons well
        // inside it (~1.4 s ramp + 1.2 s dwell ≈ 2.6 s ≈ key 65).
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

    /// UX LATENCY CONTRACT: the metric builds by TIME spent typing while the
    /// independent 16-event floor prevents sparse cadences from arriving too
    /// early. The window is TWO-SIDED on purpose — a one-sided bar would let
    /// the latency drift the moment either constant moved. Every summoning
    /// cadence owes a 2.2-3.6 s sustained run at the [`CAT_BAND`] 0.65 /
    /// [`CAT_DWELL`] 1.2 tuning, and a stroll slower than ~0.60 s/key can never
    /// HOLD the band (each gap decays the clamped metric back out of it before
    /// the next key), so the dwell never accrues and no cat arrives.
    #[test]
    fn summon_latency_is_bounded_across_typing_cadences() {
        // Fast cadences (well under the ~0.60 s/key band-hold limit) summon,
        // but only after the full ramp+dwell. `max_keys` is generous headroom.
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
        // Cadences at/above ~0.60 s/key can never HOLD the band: each gap
        // decays the (clamped-at-1.0) metric below [`CAT_BAND`] before the next
        // key, so the dwell never accrues. Both strolls below stay cat-free.
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

    /// Sustained fast typing ALWAYS summons the cat (deterministic — no rarity
    /// roll), it fades in to full opacity, then fades out COMPLETELY when
    /// momentum stops.
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
        // Now go silent. A present can arrive long after both the real LOW
        // crossing and the fade deadline; it must sample Hidden directly, not
        // restart a full fade from this first late frame. (The warmup leaves the
        // metric at the 1.0 ceiling, so the LOW crossing is a full ~3.9 s τ-drain
        // plus FADE_OUT out; sample well past that.)
        let gone = hot + Duration::from_secs(6);
        let done = c.frame(gone);
        assert_eq!(done.alpha, 0, "a sparse late frame is already hidden");
        assert_eq!(done.fp(), 0, "hidden frames settle to the zero repaint key");
        assert!(!c.is_active());
    }

    /// THE BAND IS RHYTHM-TOLERANT BUT PAUSE-STRICT: short typing breaths (finger
    /// repositioning, a fast burst's micro-gaps) keep the dwell accruing, because
    /// the metric's 2 s τ holds a hot run above [`CAT_BAND`] across them. Casual
    /// prose — short words at a relaxed cadence separated by real thinking pauses
    /// — never builds the metric INTO the band at all, so no dwell ever accrues
    /// and the companion stays away (the "more special" contract).
    #[test]
    fn breaths_keep_the_dwell_but_word_pauses_stay_cat_free() {
        // Flowing typing with 150 ms breaths every 16 keys still summons: from a
        // hot (clamped ~1.0) metric a 0.15 s breath decays by ×0.93, staying in
        // the 0.65 band so the dwell keeps accruing across breaths.
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
        // metric never HOLDS the band, so the companion stays away.
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

    /// A COLLECTION hello's goodbye is exempt from the revive: its promised-
    /// hold bookkeeping must run to completion (a revived flight carrying
    /// hello state could freeze the machine on focus loss), so typing during
    /// the hello fade lets it finish and the cat re-earns normally after.
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
        // Fast keys mid-goodbye must NOT hijack the hello into a flight.
        cat.on_key(fade_at + Duration::from_millis(40), true);
        cat.on_key(fade_at + Duration::from_millis(80), true);
        let done = cat.frame(fade_at + Duration::from_secs_f32(FADE_OUT + 0.2));
        assert_eq!(done.alpha, 0, "the goodbye completes");
        assert!(!done.collection_hello, "hello bookkeeping fully cleared");
        assert!(!cat.is_active(), "the machine returned to Hidden");
    }

    /// A cat that already began fading out REVIVES when typing resumes at
    /// speed — opacity recovers continuously instead of the keys being
    /// ignored until a full (and then complete re-earn) fade.
    #[test]
    fn fading_cat_revives_on_resumed_typing() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        // Earn the cat with a sustained run: 120 keys at 60 ms is ~7.2 s of
        // flow, well past band + dwell, and leaves the metric ≈ 1.0.
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

    /// REGRESSION: a collection hello owns an absolute hold deadline. If the
    /// compositor skips every frame spanning hold + fade, the next present is
    /// Hidden immediately; it may never leak an opaque restart frame.
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

    /// Typing a feline word must PRESENT the companion without changing WHICH
    /// companion it is — the launch kitty survives every summon, including
    /// repeats inside the chain window.
    #[test]
    fn on_summon_presents_without_ever_changing_the_look() {
        let t = Instant::now();
        let launch_look = KittyLook::for_launch(4242);
        let mut cat = CursorCat::default();
        cat.set_look(launch_look);
        assert!(!cat.is_active(), "starts hidden");

        cat.on_summon(t, 1);
        assert!(
            cat.is_active(),
            "typing the word makes a kitty appear (it was hidden)"
        );
        assert_eq!(
            cat.frame(t).look,
            launch_look,
            "the launch kitty is the one that shows up"
        );
        assert_eq!(
            cat.frame(t).reaction,
            CatReaction::Delight,
            "and it is delighted to hear its name"
        );

        // A second word inside the chain window must not re-skin it either.
        cat.on_summon(t + Duration::from_millis(200), 1);
        assert_eq!(
            cat.frame(t + Duration::from_millis(200)).look,
            launch_look,
            "repeat summons never swap the identity"
        );
    }

    /// `on_collect` — the FAVOURITE PIN's hello — still swaps: the user
    /// choosing a cat is the one reason a mid-process identity change is
    /// allowed (owner ruling, 2026-08-17: the launch kitty otherwise never
    /// changes).
    #[test]
    fn a_favourite_pin_still_swaps_the_look() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.set_look(KittyLook::for_launch(1));
        let pinned = KittyLook::for_launch(999);
        cat.on_collect(t, pinned);
        assert_eq!(
            cat.frame(t).look,
            pinned.normalized(),
            "the pin's hello presents the cat the user just chose"
        );
    }

    /// Launch kitties are a PURE function of the seed (STABLE for the life of
    /// the process, byte-identical on every machine) and genuinely different
    /// across seeds — the property that makes each computer's cat its own.
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

    /// Both typed-word reactions are EXPRESSION on a live companion: they never
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
        live.set_look(look);
        live.on_summon(t, 1);
        assert!(live.on_curse(t + Duration::from_millis(10), 1));
        let frame = live.frame(t + Duration::from_millis(10));
        assert_eq!(frame.reaction, CatReaction::Wince);
        assert_eq!(frame.look, look, "a wince never re-skins the companion");
    }

    /// THE COMPANION NO LONGER LEAPS AT ITS OWN NAME (owner, 2026-08-04: "the
    /// cursor kitty changes too unpredictably. I want the kitty animations to be
    /// in the keyword kitties versus on the cursor").
    ///
    /// This test used to assert the opposite — that the feline-word reaction was
    /// "pose as well as expression, not a face swap alone". It is flipped rather
    /// than deleted, because the leap's ABSENCE is now the property worth
    /// pinning: at `DELIGHT_LEAP` = 0.42 cells, chain-scaled, it was the single
    /// largest unpredictable displacement in the pose stack, and it fired
    /// mid-sentence on a word the reader had just typed. The celebration moved to
    /// the cat that word actually spawns (`word_decorations::CatIdlePose`).
    ///
    /// What REMAINS is the expression: happy eyes and a small brief swell. The
    /// cat still hears its name; it just no longer throws itself up the screen.
    #[test]
    fn delight_is_expression_not_displacement() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.set_look(KittyLook::for_launch(11));
        cat.on_summon(t, 1);
        let rest = cat.frame(t).bob;
        // What was the apex of the hop: the anchor must not move there.
        let apex = cat.frame(t + Duration::from_secs_f32(DELIGHT_HOLD / 2.0));
        assert!(
            (apex.bob - rest).abs() < 0.05,
            "the companion holds its anchor through a delight: rest {rest}, \
             mid-reaction {}",
            apex.bob
        );
        // …but the reaction is genuinely live, and reads on the face.
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
        // The body shaping is deliberately NOT asserted in absolute terms here:
        // `scale_y` composes with the banking spine (which thins a moving cat),
        // so a bare `> 1.0` reads the momentum, not the reaction. The swell is
        // shaped at a third of the retired hop's amplitude at its use site; what
        // this test owns is that the ANCHOR no longer moves.
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

        // Typed-"kitty" uses this same explicit collection-hello seam. It must
        // remain immediate and independent of the ordinary momentum/travel gate.
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

    /// REDUCED-MOTION LIFECYCLE RETIREMENT (M3): a flight EARNED while motion
    /// is reduced is presented only through `static_frame`, so that path must
    /// drive FadeIn→Shown→FadeOut→Hidden on the CLOCK. If it did not, the
    /// flight would never retire — `is_active` latches, the mid-appearance
    /// look latch wedges permanently, and resuming motion pops the cat in at
    /// full alpha unearned. Pins: the flight retires, `is_active` clears, and
    /// a look parked mid-flight applies at the next static wake.
    #[test]
    fn reduced_motion_flight_retires_and_releases_the_look_latch() {
        let mut cat = CursorCat::default();
        let t = Instant::now();
        // Earn an ordinary flight through sustained typing (band+dwell summon
        // around key 105 on a 25 ms cadence; allow generous headroom).
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
        // A global companion reassignment lands while the flight is on screen:
        // the two-path rule PARKS it (the appearance must not morph mid-air).
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
        // Present ONLY through the reduced-motion static path from here; typing
        // has stopped, so the lifecycle must run all the way down by TIME.
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

    /// A kitty-log reassignment synced while the companion is MID-FLIGHT never
    /// morphs the sprite. The look holds for the whole appearance; the parked
    /// look lands on the NEXT wake, with no re-sync needed in between.
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

        // A discovery in another window advances the global companion NOW.
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

        // Typing on through a partial fade keeps the SAME appearance: the
        // FadeOut→FadeIn revive must not consume the latch either.
        let reviving = last + Duration::from_secs(4);
        let _ = c.frame(reviving); // deep into (or past) the fade
        c.on_key(reviving, true);
        c.on_key(reviving + Duration::from_millis(40), true);
        if c.is_active() {
            assert_eq!(
                c.frame(reviving + Duration::from_millis(80)).look,
                flying.look,
                "a revived fade is the same appearance — still the latched look"
            );
        }

        // Let it ground completely, then re-earn WITHOUT another sync: the
        // parked look presents on the wake.
        let gone = reviving + Duration::from_secs(10);
        assert_eq!(c.frame(gone).alpha, 0);
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

    /// The two-path rule's second path: `on_collect` (the discovery hello)
    /// legitimately presents the NEW collectible immediately, even mid-flight
    /// — and it supersedes any look the latch had parked, so the hello's art
    /// cannot be overwritten by a stale deferred sync at the next wake.
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
        // …then THIS window unlocks a collectible mid-flight.
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

    /// Exit rolls land ~20% StarWink, ~20% HeartMeow, ~60% Plain.
    #[test]
    fn exit_rolls_are_roughly_20_20_60() {
        let mut c = CursorCat::default();
        let (mut star, mut heart, mut plain) = (0, 0, 0);
        for _ in 0..1000 {
            match c.roll_exit() {
                CatExit::StarWink => star += 1,
                CatExit::HeartMeow => heart += 1,
                CatExit::Plain => plain += 1,
            }
        }
        assert!((120..=280).contains(&star), "star ~20%, got {star}");
        assert!((120..=280).contains(&heart), "heart ~20%, got {heart}");
        assert!(plain > 450, "plain majority, got {plain}");
    }

    /// The DARK-theme exit fx emit additive quads only while fading (peaked),
    /// and nothing for Plain. On dark the heart/star ride the additive `out`
    /// stream (added light over the near-black ground) and never the veil sink.
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

    /// LIGHT-THEME LEGIBILITY PROOF: on a light background the exit flourish
    /// must NOT emit additive white/pink — a pure-white star on a pale ground is
    /// fully invisible. Instead the heart and star
    /// come through as SOURCE-OVER veils ([`HaloMode::Over`]) in DARKENED,
    /// saturated inks that read against any light ground — contrast-correct by
    /// construction because the veil DARKENS the ground rather than brightening
    /// it toward white.
    #[test]
    fn exit_fx_light_theme_is_a_darkening_source_over_veil() {
        let g = geom();
        // Relative luminance (Rec.601, straight 8-bit) — a light theme's ground
        // sits high (a #FAFAFA-ish ground ≈ 250); the veil ink must sit well
        // BELOW it so a source-over composite visibly darkens the ground.
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
                // The straight ink (low 24 bits) is dark/saturated so it reads on
                // a light ground; the high byte carries a non-trivial centre
                // opacity so the veil is actually visible, not a whisper.
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
            }
        }
    }

    /// The light veil, like the additive arm, fades to nothing at the exit ends
    /// (`fade_out` near 0 or 1 ⇒ `pop → 0`), so the flourish dissolves with the
    /// cat instead of snapping off at full strength.
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

    /// THE STRIDE PUMP (owner: "the cursor cat can be more dynamic"). The core
    /// defect was that every pose channel is driven by a RATE-NORMALIZED metric
    /// which pins at its 1.0 clamp at every human cadence, so once the cat was
    /// visible the whole banking/stretch system was frozen and the only live
    /// channel was a wall-clock sine. Pins the fix: the pose MOVES as you type,
    /// and it moves because of the KEYS rather than the level.
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
        // The tail of the run is where the metric is pinned — exactly the window
        // in which the cat used to be frozen.
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

        // CADENCE, not the clock: the same elapsed time with FEWER keys advances
        // the stride LESS — a property a wall-clock sine cannot have.
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

        // THE CAT BLINKS WHILE YOU TYPE. Before, this arm sat below the happy
        // face AND was gated on a level pinned during any real run, so both
        // conditions failed together and the cat stared for the whole flight.
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

        // And a RESTING cat is unchanged: the pump is gated by the same bank the
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
        // FAST: keep feeding forward keys so the display spine climbs to speed.
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

        // SLOW: no typing after the hello — momentum decays, the cat rides level.
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
        // decaying metric (the follower's dt clamp makes sparse samples keep
        // stale banking momentum, which would hold the eyes above BLINK_CEIL).
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

    /// A key-repeat delete run reads as ONE sustained oops: the tongue-out
    /// expression holds across the whole burst (every key extends it), but
    /// the squash kick fires only on the burst's FIRST delete — a later
    /// repeat key finds the bounce well into its decay instead of re-frozen
    /// at maximum squash.
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

    /// The emphasized hold: the tongue-out is still reading 0.4 s after the
    /// delete, the pose recoils BACK off the rest anchor while it holds (the
    /// at-a-glance kick), and everything decays to the base look after.
    #[test]
    fn oops_holds_visibly_then_decays_to_base_look() {
        let t = Instant::now();
        let mut cat = CursorCat::default();
        cat.on_collect(t, KittyLook::default());
        let _ = cat.frame(t + Duration::from_millis(16));
        let del = t + Duration::from_millis(300);
        cat.on_key(del, false);
        // Pump a frame cadence so the display spine settles like a real 60 fps
        // present stream would (the follower's dt clamp makes ONE sparse
        // sample keep stale banking lean, which would mask the recoil).
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
        // Past the hold: the expression and the recoil both release. (The
        // canonical metric's mild delete drain leaves real residual momentum,
        // so the released pose may carry a small FORWARD banking lean — the
        // pin is that the BACKWARD recoil is gone, not that the body is inert.)
        let settled = cat.frame(del + Duration::from_millis(700));
        assert_eq!(settled.reaction, CatReaction::Cruise);
        assert_eq!(settled.render_look(), KittyLook::default().normalized());
        assert!(
            settled.pose.lead > -0.02,
            "the backward recoil releases with the expression, got lead {}",
            settled.pose.lead
        );
    }

    /// Kill chords are deletes too: `on_kill` fires the same bounded oops on
    /// a live companion and NEVER summons a hidden one — the reaction is
    /// expression state on an existing flight, not a lifecycle event.
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

    /// Kills un-earn but MILDLY (the one-metric law: deletes/kills never
    /// build, and one slip no longer vaporizes an earned flight): a single
    /// kill dents the metric and breaks the dwell, while a kill FLOOD walks
    /// the metric under [`LOW`] and grounds the flight — whose goodbye fade
    /// carries the tongue-out with it.
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

    /// Reduced motion: the oops is a STATIC expression swap only — full
    /// opacity, S121 face, pose pinned STILL, zero bob — and it decays back
    /// to the discovery face with its own armed wake (the swap back has no
    /// keypress redraw to ride).
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
        // The detector arms after sixteen repeat events; pinning the metric at
        // that point must NOT silently substitute for the independent sixteen-
        // event cursor-travel floor.
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

    /// SING-ALONG: an armed celebration pins the canonical metric
    /// to 1.0 (the documented momentum bypass — an armed hold IS maximal flow
    /// by definition), bypasses dwell only AFTER sixteen qualifying events,
    /// wears the authored open-mouth meow head as its singing face, and DANCES
    /// on the shared beat clock — the pose cycles within a beat.
    #[test]
    fn singing_pins_momentum_summons_and_dances_on_the_beat() {
        let mut c = CursorCat::default();
        let t = Instant::now();
        let armed_at = arm_singing_after_travel(&mut c, t);
        assert_eq!(c.momentum(armed_at), 1.0, "the documented momentum bypass");
        // Two presents inside ONE beat, past the fade-in: fresh-beat pulse
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

    /// The delete "oops" OUTRANKS the song (the wrong-note gag priority the
    /// riff/bonk share): a backspace mid-celebration swaps the tongue-out
    /// face over the singing head for its bounded hold.
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

    /// WIND-DOWN is a crossfade, never a hard cut: at half drive the frame
    /// still sings (face + scaled dance), and at zero the overlay is gone
    /// while the companion glides out on its natural momentum decay.
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
        // The metric was pinned to 1.0 at the arm and only decays naturally
        // from there (drive < 1.0 does not re-pin): ~0.5 s later it sits at
        // 1.0·e^(-0.5/2) ≈ 0.78 — still HIGH, proving the bypass handed back to
        // natural decay rather than zeroing the metric. 0.7 is a fixed marker,
        // deliberately NOT [`CAT_BAND`]: the pin is "still high", not "still
        // summon-eligible".
        assert!(
            c.momentum(t2) > 0.7,
            "the bypassed metric hands back to NATURAL decay (still high right after): {}",
            c.momentum(t2)
        );
    }

    /// THE REDUCED-MOTION ARM: a static celebration — full opacity, the
    /// singing face, pose pinned STILL and `bob = 0` (no dance loop), with
    /// the hello's one-step disappearance below the authored 0.33 face swap.
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
        // A late/occluded render can jump directly past the old 0.50 cutoff.
        // The static singer must retain custody until the authored face swap,
        // so that first sampled wind-down frame cannot be a blank while the
        // resident pet has not yet received an intermediate fade-in tick.
        let late = armed_at + Duration::from_millis(600);
        c.set_singing(late, sync(0.49, 6.3));
        let retained = c.static_frame(late);
        assert_eq!(retained.alpha, 255, "late wind-down keeps opaque custody");
        assert_eq!(retained.render_look().variant, CatGlyphId::S115);
        // Below the face swap: one-step off (the hello's disappearance law).
        let t1 = armed_at + Duration::from_millis(800);
        c.set_singing(t1, sync(0.3, 7.3));
        let off = c.static_frame(t1);
        assert_eq!(off.alpha, 0, "one-step disappearance, no fade animation");
    }
}
