// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The "LEVEL UP" CELEBRATION: a brief, self-fading flourish that fires the moment an
//! update becomes available (`Wake::UpdateStaged`) or the app relaunches into a newer
//! build (post-apply [`crate::JUST_UPDATED`]). Two layers, both accent-themed off the
//! live cursor colour:
//!   * a PULSING BORDER GLOW — an inset accent frame (+ faint wash) whose alpha
//!     "breathes" and then fades out, painted through the SAME drop-target overlay pass
//!     the drag-and-drop highlight uses (CPU `apply_overlay_at`, GPU `DropOverlay`, and
//!     the SACRED `image`/`snapshot` introspection) so it reads on glass AND to an AI.
//!   * a RISING UP-ARROW (↑) — a single bold accent glyph that rises through the window
//!     centre and fades, rasterized as a paint-only [`DrawPrim`] card (the `level_up_card`
//!     tray-quad slot) exactly like the transient notice pill.
//!
//! GLOBAL (App-level), like `notice`/`config_notice`, and it borrows the SAME timed
//! lifecycle shape (`is_expired` + `deadline` + a quantized `fingerprint`). Unlike the
//! notice — which only animates through its fade tail — the celebration animates for its
//! WHOLE life (the border breathes and the arrow rises), so `deadline` steps every
//! [`FRAME`]. It is TASTEFUL by construction: the border alpha peaks below the drop
//! overlay's, the wash stays low enough to keep the terminal readable, and the arrow
//! bursts once (≈1.1s) then clears so the "Update ready" pill shows through beneath it.

use std::time::{Duration, Instant};

use crate::settings::{SettingsGeom, text_w};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// Whole-celebration lifetime (border ramp-in + breathing + fade-out).
const TTL: Duration = Duration::from_millis(2100);
/// Animation cadence — the celebration animates the WHOLE time, so this is the
/// deadline granularity and the fingerprint quantum (~30 fps).
const FRAME: Duration = Duration::from_millis(33);
/// The border envelope ramps 0→1 over this opening stretch.
const RAMP_IN: Duration = Duration::from_millis(200);
/// The border envelope ramps 1→0 over this closing stretch of [`TTL`].
const FADE_OUT: Duration = Duration::from_millis(650);
/// One full breath of the border glow (its alpha oscillates on this period).
const BREATH_PERIOD: Duration = Duration::from_millis(850);
/// The up-arrow's rise + fade lifetime (starts with the celebration; ends well
/// before [`TTL`] so the border keeps glowing after the arrow has cleared).
const ARROW_DUR: Duration = Duration::from_millis(1150);
/// The arrow's alpha ramps 0→1 over this opening stretch of [`ARROW_DUR`].
const ARROW_RAMP: Duration = Duration::from_millis(130);
/// The arrow's alpha ramps 1→0 over this closing stretch of [`ARROW_DUR`].
const ARROW_FADE: Duration = Duration::from_millis(480);

/// Peak border alpha (0..255) — a touch below the drop overlay's crisp 235 so the
/// celebration reads as a warm pulse, not an alarm.
const PEAK_BORDER: f32 = 210.0;
/// Peak interior-wash alpha (0..255) — below the drop overlay's 28 so the terminal
/// text stays readable under the celebration.
const PEAK_WASH: f32 = 20.0;
/// Total vertical travel of the arrow, as a fraction of the window height.
const ARROW_TRAVEL_FRAC: f32 = 0.16;
/// The arrow glyph size is the [`TypeStep::Display`] step applied to the terminal
/// `font_px` scaled by this — a deliberately large decorative pictogram.
const ARROW_SCALE: f32 = 2.0;

/// A live level-up celebration: just the spawn instant + the build it marks (folded
/// into the fingerprint so a second staged build re-animates rather than aliasing).
pub(crate) struct LevelUp {
    build: u64,
    spawned: Instant,
}

impl LevelUp {
    /// Begin a celebration for `build` at `now`.
    pub(crate) fn new(build: u64, now: Instant) -> Self {
        Self {
            build,
            spawned: now,
        }
    }

    /// Fully gone (past its whole lifetime) — the caller drops it and the border/arrow
    /// vanish on the next present.
    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.spawned) >= TTL
    }

    /// The next wake time: the celebration animates every frame for its whole life, so
    /// always step one [`FRAME`] ahead (mirrors the notice's fade-tail cadence).
    pub(crate) fn deadline(&self, now: Instant) -> Instant {
        now + FRAME
    }

    /// The overall border envelope: a quick ramp-in, a steady middle, then a fade-out
    /// over the closing stretch (`0` at/after [`TTL`]).
    fn envelope(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let ttl = TTL.as_secs_f32();
        if e >= ttl {
            return 0.0;
        }
        let ramp = (e / RAMP_IN.as_secs_f32()).min(1.0);
        let fade = ((ttl - e) / FADE_OUT.as_secs_f32()).min(1.0);
        ramp.min(fade).clamp(0.0, 1.0)
    }

    /// The "breathing" multiplier the glow pulses on — a sine in `[0.55, 1.0]`.
    fn breath(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let phase = std::f32::consts::TAU * e / BREATH_PERIOD.as_secs_f32();
        0.55 + 0.45 * (0.5 + 0.5 * phase.sin())
    }

    /// The inset-border alpha (0..255) at `now`: the peak scaled by the fade envelope
    /// and the breathing pulse.
    pub(crate) fn border_alpha(&self, now: Instant) -> u8 {
        (PEAK_BORDER * self.envelope(now) * self.breath(now)).round() as u8
    }

    /// The interior-wash alpha (0..255) at `now` — the same envelope × breath, capped
    /// low so content stays readable.
    pub(crate) fn wash_alpha(&self, now: Instant) -> u8 {
        (PEAK_WASH * self.envelope(now) * self.breath(now)).round() as u8
    }

    /// The rising arrow's alpha (0..1) at `now`: a fast ramp-in, a hold, then a fade to
    /// `0` at [`ARROW_DUR`] (and `0` thereafter, so the card clears and the pill shows).
    pub(crate) fn arrow_alpha(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let dur = ARROW_DUR.as_secs_f32();
        if e >= dur {
            return 0.0;
        }
        let rin = (e / ARROW_RAMP.as_secs_f32()).min(1.0);
        let rout = ((dur - e) / ARROW_FADE.as_secs_f32()).min(1.0);
        rin.min(rout).clamp(0.0, 1.0)
    }

    /// The arrow's rise fraction (0→1, ease-out) over [`ARROW_DUR`] — `0` at the
    /// bottom of its travel, `1` at the top.
    pub(crate) fn arrow_rise(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let t = (e / ARROW_DUR.as_secs_f32()).clamp(0.0, 1.0);
        1.0 - (1.0 - t) * (1.0 - t)
    }

    /// A repaint fingerprint folded into `RepaintKey::level_up_fp`, quantized to the
    /// [`FRAME`] step so the animation re-presents ~30×/s. `0` is the no-celebration
    /// sentinel (the caller's `map_or(0, …)`), so a live celebration is forced non-zero.
    pub(crate) fn fingerprint(&self, now: Instant) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.build.hash(&mut h);
        let step = (now.duration_since(self.spawned).as_millis() / FRAME.as_millis()) as u64;
        step.hash(&mut h);
        h.finish() | 1
    }
}

/// Build the rising up-arrow as a single bold accent [`DrawPrim::Text`] glyph, with the
/// glyph's alpha and vertical position taken from the celebration's arrow curves. The
/// returned `card` rect tightly bounds the glyph so [`crate::App::splice_level_up`] crops
/// the raster to it (a small, cheap card that moves up frame by frame).
pub(crate) fn arrow_tray(
    l: &LevelUp,
    g: &SettingsGeom,
    accent: [u8; 3],
    now: Instant,
) -> TrayInput {
    let win_w = g.cols as f32 * g.cw;
    let win_h = g.panel_rows as f32 * g.ch;
    // A deliberately large decorative pictogram off the Display step (see `ARROW_SCALE`).
    let size = TypeStep::Display.px(g.font_px * ARROW_SCALE);
    let s = size.get();
    let glyph = "\u{2191}"; // ↑
    let gw = text_w(glyph, s);
    // The glyph's visual centre rises from `centre + travel/2` (below) to `centre -
    // travel/2` (above) as the rise fraction goes 0→1.
    let travel = win_h * ARROW_TRAVEL_FRAC;
    let cy = win_h * 0.5 + travel * (0.5 - l.arrow_rise(now));
    // Approximate mono cap-centre → baseline (cap height ≈ 0.7·size, centred).
    let baseline = cy + s * 0.34;
    let x = (win_w - gw) * 0.5;
    let alpha = (l.arrow_alpha(now) * 255.0).round().clamp(0.0, 255.0) as u8;

    let prims: Vec<DrawPrim> = vec![text_prim(
        x,
        baseline,
        glyph.to_string(),
        size,
        TextWeight::Bold,
        TextFace::Mono,
        rgba(accent, alpha),
    )];
    // Card bounds: the glyph sits within [baseline − ascent, baseline + descent].
    let card = (x, baseline - s, gw, s * 1.34);
    TrayInput { prims, card }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> SettingsGeom {
        SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 120,
            panel_rows: 40,
        }
    }

    #[test]
    fn border_ramps_in_breathes_then_fades_to_zero() {
        let now = Instant::now();
        let l = LevelUp::new(830, now);
        // Ramps in from ~0 at spawn.
        assert!(l.border_alpha(now) < l.border_alpha(now + RAMP_IN));
        // Mid-life the glow is present (breathing keeps it well above zero).
        let mid = l.border_alpha(now + Duration::from_millis(900));
        assert!(mid > 0, "glow visible mid-life");
        // Fully faded at/after the whole lifetime.
        assert_eq!(l.border_alpha(now + TTL), 0);
        assert_eq!(l.wash_alpha(now + TTL), 0);
        assert!(l.is_expired(now + TTL));
        assert!(!l.is_expired(now));
        // The breathing pulse actually moves the alpha across a breath.
        let a = l.border_alpha(now + Duration::from_millis(400));
        let b = l.border_alpha(now + Duration::from_millis(400) + BREATH_PERIOD / 2);
        assert_ne!(a, b, "border alpha breathes over a half-period");
    }

    #[test]
    fn arrow_rises_monotonically_and_alpha_fades_out() {
        let now = Instant::now();
        let l = LevelUp::new(830, now);
        // Rise is monotonic 0→1 across the arrow's life.
        let r0 = l.arrow_rise(now);
        let r1 = l.arrow_rise(now + ARROW_DUR / 2);
        let r2 = l.arrow_rise(now + ARROW_DUR);
        assert!(r0 < r1 && r1 < r2, "arrow rises: {r0} < {r1} < {r2}");
        assert!((r0 - 0.0).abs() < 1e-3 && (r2 - 1.0).abs() < 1e-3);
        // Alpha ramps in, holds, then reaches 0 by ARROW_DUR (and stays 0 after).
        assert!(l.arrow_alpha(now) < l.arrow_alpha(now + ARROW_RAMP));
        assert_eq!(l.arrow_alpha(now + ARROW_DUR), 0.0);
        assert_eq!(
            l.arrow_alpha(now + ARROW_DUR + Duration::from_millis(200)),
            0.0
        );
        // The arrow clears well before the border stops glowing.
        assert!(l.border_alpha(now + ARROW_DUR + Duration::from_millis(100)) > 0);
    }

    #[test]
    fn fingerprint_is_nonzero_stable_per_frame_and_steps() {
        let now = Instant::now();
        let l = LevelUp::new(830, now);
        assert_ne!(l.fingerprint(now), 0, "never the no-celebration sentinel");
        // Stable within one frame quantum, changes across a frame.
        assert_eq!(
            l.fingerprint(now),
            l.fingerprint(now + Duration::from_millis(10))
        );
        assert_ne!(
            l.fingerprint(now),
            l.fingerprint(now + FRAME + Duration::from_millis(1))
        );
    }

    #[test]
    fn arrow_tray_paints_the_glyph_centred_within_the_window() {
        let now = Instant::now();
        let l = LevelUp::new(830, now);
        let g = geom();
        let t = arrow_tray(&l, &g, [0, 255, 0], now);
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == "\u{2191}")),
            "an up-arrow glyph is emitted"
        );
        let (x, _, w, _) = t.card;
        let win_w = g.cols as f32 * g.cw;
        // Roughly horizontally centred.
        let mid = x + w * 0.5;
        assert!(
            (mid - win_w * 0.5).abs() < 1.0,
            "arrow centred: {mid} vs {}",
            win_w * 0.5
        );
    }
}
