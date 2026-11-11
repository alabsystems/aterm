// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ground a window's trail draws on (`docs/design/PRISM-GLASS.md` §15.1 P0.2a, §15.2 1.3):
//! the frame's live page and main's polarity of it, and the light the window's
//! [`LightLatch`] admits on it at the live level.
//!
//! [`TrailGround::ground`] writes the glow config's page exactly as main's frame does, then runs
//! the latch on every frame it grounds, never behind a short-circuit, and sets the config's
//! light to [`Light::admitted`] its verdict at [`LIVE`], on the frame's page ([`PageFacts`],
//! [`PageFacts::glass`], [`Light::on_page`]). A dark scheme is never admitted, so on every dark
//! ground the config it hands on is main's. A composed frame's other panes take [`pane_light`].

use crate::cursor_glow::{GlowConfig, Light, Live};
use crate::host::light_latch::{LatchKey, LatchOut, LightLatch, raw_light, scheme_light};
use crate::rainbow_kitty::prism_glass::LIVE;
use crate::rainbow_kitty::prism_glass::colour::THIN_INK_SHARE;
use aterm_time::Instant;

/// What the host knows of the page under a window's text (§15.2 1.7, D1): whether the window is
/// opaque (its `background_opacity` is 1), whether a wallpaper or an image lies under the text,
/// and how near its face lays its thinnest glyph to the full ink. Glass draws only on a clear
/// page whose face keeps its letters under it ([`Self::glass`]); on any other the frame draws
/// main's paper arm ([`Light::on_page`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageFacts {
    /// The window is opaque: nothing behind it shows through the page.
    pub opaque: bool,
    /// A wallpaper or an image lies under the text.
    pub wallpaper: bool,
    /// How far toward the page, as a share of linear light, the darkest pixel of the window's
    /// thinnest printable glyph lies, the face at the window's size laying the page's ink
    /// (`aterm_render::GlyphInk::thin_share`): `0` for a host that does not know its face, which
    /// takes it as laying its full ink; `1` for one that knows no glyph of it yet.
    pub thin_ink: f32,
}

impl PageFacts {
    /// An opaque window with nothing under its text: the scheme's page alone, its face taken as
    /// laying its full ink.
    pub const OPAQUE: Self = Self {
        opaque: true,
        wallpaper: false,
        thin_ink: 0.0,
    };

    /// The page is the scheme's own colour alone: opaque, with no wallpaper or image under it.
    #[inline]
    #[must_use]
    pub const fn clear(self) -> bool {
        self.opaque && !self.wallpaper
    }

    /// The glass draws on this page (`docs/design/PRISM-GLASS.md` §15.8 amendment 53): it is
    /// [`Self::clear`], and the window's face lays its thinnest glyph's darkest pixel no further
    /// toward the page than [`THIN_INK_SHARE`], which the glass is solved for, so every letter's
    /// solid ink keeps the glass's bar. A face that lays a 1 px stroke between two pixel rows
    /// (most at small sizes, and SF Mono's `_` at 26 px) keeps main's paper arm, whatever its
    /// cells' height.
    #[inline]
    #[must_use]
    pub const fn glass(self) -> bool {
        self.clear() && self.thin_ink <= THIN_INK_SHARE as f32
    }
}

impl Default for PageFacts {
    #[inline]
    fn default() -> Self {
        Self::OPAQUE
    }
}

/// One window's ground: its light latch, and the last frame it grounded.
#[derive(Clone, Copy, Debug, Default)]
pub struct TrailGround {
    latch: LightLatch,
    /// The last grounded frame's page; [`PageFacts::OPAQUE`] before the first.
    page: PageFacts,
    /// The last grounded frame's key and its page's raw light; `None` before the first.
    seen: Option<(LatchKey, bool)>,
    /// The level a debug dump previews in place of [`LIVE`] (`live <level>`).
    #[cfg(any(test, debug_assertions))]
    forced: Option<Live>,
}

impl TrailGround {
    /// Ground `cfg` on the frame's live page `live` (`(fg, bg)`, DECSCNM and OSC 10/11 folded)
    /// at `now`: main's polarity (`dark_theme`) and page (`theme_fg`, `theme_bg`), then the
    /// latch's verdict on `scheme`, the front session's configured pair, on the window's `page`
    /// ([`PageFacts::glass`], [`Light::on_page`]) as the config's light.
    #[inline]
    pub fn ground(
        &mut self,
        cfg: &mut GlowConfig,
        live: (u32, u32),
        scheme: LatchKey,
        page: PageFacts,
        now: Instant,
    ) -> LatchOut {
        let (fg, bg) = live;
        cfg.dark_theme = aterm_render::theme_is_dark(bg);
        cfg.theme_fg = fg;
        cfg.theme_bg = bg;
        let raw = raw_light(fg, bg, cfg.dark_theme);
        let out = self.latch.admits(now, scheme, raw);
        self.seen = Some((scheme, raw));
        self.page = page;
        cfg.light = Light::admitted(out.on, self.live()).on_page(page.glass());
        out
    }

    /// The level a lit frame draws at: [`LIVE`], or the level a debug dump forced.
    #[inline]
    #[must_use]
    pub const fn live(&self) -> Live {
        #[cfg(any(test, debug_assertions))]
        if let Some(forced) = self.forced {
            return forced;
        }
        LIVE
    }

    /// Preview `live` in place of [`LIVE`] on every frame this ground lights from now on, or
    /// restore [`LIVE`] with `None` (the trail-diff dump's `live <level>`). Debug builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn force_live(&mut self, live: Option<Live>) {
        self.forced = live;
    }

    /// The last grounded frame's verdict; all false before the first.
    #[inline]
    #[must_use]
    pub const fn last(&self) -> LatchOut {
        self.latch.last()
    }

    /// The last grounded frame's light: what a reader outside the frame takes, before it checks
    /// the scheme still holds.
    #[inline]
    #[must_use]
    pub const fn light(&self) -> Light {
        Light::admitted(self.latch.last().on, self.live()).on_page(self.page.glass())
    }

    /// The last grounded frame's key and its page's raw light; `None` before the first.
    #[inline]
    #[must_use]
    pub const fn seen(&self) -> Option<(LatchKey, bool)> {
        self.seen
    }
}

/// A composed frame's pane light (§15.2 1.3): the frame's light `frame` where the pane's
/// configured pair `scheme` is a light scheme's and its live page `live` (`(fg, bg)`) is a light
/// ground, and [`Light::OFF`] everywhere else, a pane whose host named no scheme among them.
#[inline]
#[must_use]
pub fn pane_light(frame: Light, scheme: Option<(u32, u32)>, live: (u32, u32)) -> Light {
    let (fg, bg) = live;
    match scheme {
        Some((_, scheme_bg))
            if scheme_light(scheme_bg) && raw_light(fg, bg, aterm_render::theme_is_dark(bg)) =>
        {
            frame
        }
        _ => Light::OFF,
    }
}

#[cfg(test)]
#[path = "trail_ground_tests.rs"]
mod tests;
