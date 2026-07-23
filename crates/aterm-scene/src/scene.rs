// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layer 3 — the **scene contract**. A [`Scene`] is a host-side animator shaped like
//! aterm's `cursor_glow` aurora: [`Scene::tick`] advances bounded state from an injected
//! `dt` and resolved [`Drives`], and [`Scene::emit`] fills a [`SceneFrame`] with
//! axis-aligned [`LocalSprite`]s (sampled from the scene's [`Atlas`]). The renderer is a
//! dumb, parity-safe consumer; ALL art and animation live behind this trait.
//!
//! Coordinates are **scene-local pixels** (`0..w` × `0..h`, the panel's box); the host
//! offsets/clamps/row-slices them into `aterm_core::render::SpriteQuad`s. Keeping scenes
//! in a local box (and resolution-independent — the atlas is baked once and scaled at
//! draw) makes them trivially testable and reusable across panel sizes.

use crate::Rect;
use crate::atlas::Sprite;
use crate::bind::Drives;

/// Theme-resolved colours the host hands a scene each frame (from the active aterm
/// colorscheme, via `hud_colors`-style derivation), so scenes track the user's theme.
/// All colours are packed `0x00RRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Foreground/ink (text, outlines, Zzz).
    pub ink: u32,
    /// Dim/secondary tone.
    pub dim: u32,
    /// Daytime sky — top and bottom of the gradient.
    pub sky_day_top: u32,
    pub sky_day_bot: u32,
    /// Night sky — top and bottom of the gradient.
    pub sky_night_top: u32,
    pub sky_night_bot: u32,
    /// Far hills / mid hills.
    pub hill: u32,
    /// Grass — lit and shadowed.
    pub grass: u32,
    pub grass_dark: u32,
    /// Celestial body (sun by day, moon by night) + accent.
    pub sun: u32,
    pub accent: u32,
    /// Health hues (reused for cosmos/pulse).
    pub good: u32,
    pub warn: u32,
    pub hot: u32,
}

impl Default for Palette {
    /// A pleasant "Tokyo Night"-ish default so a scene is legible before the host wires
    /// a real theme.
    fn default() -> Self {
        Palette {
            ink: 0x001A_1B26,
            dim: 0x0056_5F89 & 0x00FF_FFFF,
            sky_day_top: 0x007A_A2F7 & 0x00FF_FFFF,
            sky_day_bot: 0x00B4_C8FF & 0x00FF_FFFF,
            sky_night_top: 0x001A_2042 & 0x00FF_FFFF,
            sky_night_bot: 0x0028_2E5A & 0x00FF_FFFF,
            hill: 0x002C_3868 & 0x00FF_FFFF,
            grass: 0x0078_A860 & 0x00FF_FFFF,
            grass_dark: 0x002C_4A34 & 0x00FF_FFFF,
            sun: 0x00FF_EEB4 & 0x00FF_FFFF,
            accent: 0x009E_CE6A & 0x00FF_FFFF,
            good: 0x009E_CE6A & 0x00FF_FFFF,
            warn: 0x00E0_AF68 & 0x00FF_FFFF,
            hot: 0x00F7_768E & 0x00FF_FFFF,
        }
    }
}

/// Per-frame context handed to a scene: its local pixel box, accessibility/preference
/// flags, and the theme palette. Cheap and `Copy`.
#[derive(Clone, Copy, Debug)]
pub struct Env {
    /// Panel width in pixels (scene-local x runs `0..w`).
    pub w: f32,
    /// Panel height in pixels (scene-local y runs `0..h`).
    pub h: f32,
    /// Honor the OS "reduce motion" setting — dampen speeds, drop particles.
    pub reduced_motion: bool,
    /// Optional night override (`Some(true)` forces night; `None` lets the scene/day
    /// drive decide).
    pub night: Option<bool>,
    /// Theme-resolved colours.
    pub palette: Palette,
}

impl Env {
    /// A simple env for tests/headless: a `w×h` box, day, motion on, default palette.
    #[must_use]
    pub fn new(w: f32, h: f32) -> Self {
        Env {
            w,
            h,
            reduced_motion: false,
            night: None,
            palette: Palette::default(),
        }
    }
}

/// A console text-entry pulse — the hook that lets typing "drop a butterfly". The host
/// posts one per real printable keystroke (control keys / IME / paste are filtered out
/// upstream, at the `App::input` convergence seam).
#[derive(Clone, Copy, Debug, Default)]
pub struct TextPulse {
    /// `true` for an ordinary printable character (the only kind that should delight).
    pub printable: bool,
}

/// One sprite to draw this frame: a region of the scene [`Atlas`] stamped into the
/// local-pixel dest rect, multiply-tinted, at `alpha` opacity, optionally mirrored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalSprite {
    /// Which atlas sprite to sample.
    pub sprite: Sprite,
    /// Destination rectangle in scene-local pixels.
    pub dst: Rect,
    /// Multiply tint `0x00RRGGBB` (`0x00FF_FFFF` = none) — how one grayscale cat sprite
    /// becomes any fur colour, and how lights take their hue.
    pub tint: u32,
    /// Opacity `0..=1` multiplied onto the sampled alpha.
    pub alpha: f32,
    /// Mirror horizontally (face left/right from one sprite).
    pub flip_x: bool,
}

impl LocalSprite {
    /// A convenience constructor (no flip, full opacity, no tint).
    #[must_use]
    pub fn new(sprite: Sprite, dst: Rect) -> Self {
        Self {
            sprite,
            dst,
            tint: 0x00FF_FFFF,
            alpha: 1.0,
            flip_x: false,
        }
    }
    /// Builder: set tint.
    #[must_use]
    pub fn tinted(mut self, tint: u32) -> Self {
        self.tint = tint & 0x00FF_FFFF;
        self
    }
    /// Builder: set opacity.
    #[must_use]
    pub fn opacity(mut self, a: f32) -> Self {
        self.alpha = crate::clampf(a, 0.0, 1.0);
        self
    }
    /// Builder: set horizontal flip.
    #[must_use]
    pub fn flip(mut self, flip_x: bool) -> Self {
        self.flip_x = flip_x;
        self
    }
}

/// The renderer-facing output of one scene frame: src-over sprites (the world) and
/// additive sprites (light — sun, fireflies, comets). The host owns and reuses this
/// buffer across frames (clear + refill) to avoid per-frame allocation, exactly like the
/// `cursor_glow` `out: &mut Vec<GlowQuad>` contract.
#[derive(Clone, Debug, Default)]
pub struct SceneFrame {
    /// Source-over sprites (back-to-front paint order).
    pub over: Vec<LocalSprite>,
    /// Premultiplied-additive light sprites (drawn over `over`, under terminal text).
    pub add: Vec<LocalSprite>,
}

impl SceneFrame {
    /// A fresh empty frame buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear both layers, keeping the allocations for reuse.
    pub fn clear(&mut self) {
        self.over.clear();
        self.add.clear();
    }

    /// Total sprite count (both layers) — the bounded per-frame draw budget.
    #[must_use]
    pub fn len(&self) -> usize {
        self.over.len() + self.add.len()
    }

    /// Whether the frame is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.over.is_empty() && self.add.is_empty()
    }

    /// Push a source-over sprite.
    pub fn push_over(&mut self, s: LocalSprite) {
        self.over.push(s);
    }

    /// Push an additive light sprite.
    pub fn push_add(&mut self, s: LocalSprite) {
        self.add.push(s);
    }
}

/// A host-side, data-driven animator that paints one panel. Implementations own bounded
/// entity pools and a baked [`Atlas`]; they are deterministic given a seed and the
/// `dt`/[`Drives`] stream (no wall clock, no global state).
///
/// `Send` is required so the host can run the (potentially expensive) simulation on a
/// dedicated worker thread, keeping it OFF the terminal's input→present path — every concrete
/// scene is plain owned data, so this costs nothing.
pub trait Scene: Send {
    /// Stable identifier (`"meadow"`, `"cosmos"`, `"pulse"`) — manifest + introspection.
    fn id(&self) -> &'static str;

    /// Advance one frame by `dt` seconds under the resolved `drives` and `env`.
    fn tick(&mut self, dt: f32, drives: &Drives, env: &Env);

    /// Emit the current frame's sprites into `out` (already cleared by the host).
    fn emit(&self, env: &Env, out: &mut SceneFrame);

    /// Whether anything is still moving — `false` lets the host return to 0% idle.
    fn is_active(&self) -> bool;

    /// React to a console text-entry pulse (default: ignore).
    fn on_text(&mut self, _pulse: TextPulse) {}

    /// A short human/inspection summary (the `controls scenes` dump): entity counts and
    /// salient state. Default: just the id.
    fn describe(&self) -> String {
        self.id().to_string()
    }
}
