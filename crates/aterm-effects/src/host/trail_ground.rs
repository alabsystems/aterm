// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ground a window's trail draws on (`docs/design/PRISM-GLASS.md` §15.1 P0.2a, §15.2 1.3):
//! the frame's live page and main's polarity of it, and the light the window's
//! [`LightLatch`] admits on it at the live level.
//!
//! [`TrailGround::ground`] writes the glow config's page exactly as main's frame does, then runs
//! the latch on every frame it grounds, never behind a short-circuit, and sets the config's
//! light to [`Light::admitted`] its verdict at [`LIVE`], on the frame's page ([`PageFacts`],
//! [`Light::on_page`]). A dark scheme is never admitted, so on every dark ground the config it
//! hands on is main's. A composed frame's other panes take [`pane_light`].

use crate::cursor_glow::{GlowConfig, Light, Live};
use crate::host::light_latch::{LatchKey, LatchOut, LightLatch, raw_light, scheme_light};
use crate::rainbow_kitty::prism_glass::LIVE;
use aterm_time::Instant;

/// What the host knows of the page under a window's text (§15.2 1.7, D1): whether the window is
/// opaque (its `background_opacity` is 1) and whether a wallpaper or an image lies under the text.
/// Glass draws only on a clear page ([`Self::clear`]); on any other the frame draws main's paper
/// arm ([`Light::on_page`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageFacts {
    /// The window is opaque: nothing behind it shows through the page.
    pub opaque: bool,
    /// A wallpaper or an image lies under the text.
    pub wallpaper: bool,
}

impl PageFacts {
    /// An opaque window with nothing under its text: the scheme's page alone.
    pub const OPAQUE: Self = Self {
        opaque: true,
        wallpaper: false,
    };

    /// The page is the scheme's own colour alone: opaque, with no wallpaper or image under it.
    #[inline]
    #[must_use]
    pub const fn clear(self) -> bool {
        self.opaque && !self.wallpaper
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
    /// ([`Light::on_page`]) as the config's light.
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
        cfg.light = Light::admitted(out.on, self.live()).on_page(page.clear());
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
        Light::admitted(self.latch.last().on, self.live()).on_page(self.page.clear())
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
mod tests {
    use super::*;
    use std::time::Duration;

    /// Default Light's pair.
    const LIGHT: (u32, u32) = (0x002b_2d33, 0x00f6_f7f9);
    /// Default's pair.
    const DARK: (u32, u32) = (0x00d0_d0d0, 0x0011_1318);

    const fn key(session: u64, (fg, bg): (u32, u32)) -> LatchKey {
        LatchKey { session, fg, bg }
    }

    /// The page main's frame wrote before the ground took it over: its three assignments.
    fn mains(cfg: &GlowConfig, (fg, bg): (u32, u32)) -> GlowConfig {
        let mut main = *cfg;
        main.dark_theme = aterm_render::theme_is_dark(bg);
        main.theme_fg = fg;
        main.theme_bg = bg;
        main
    }

    /// Every field but the light is main's on every page, and every dark scheme's frame is main's
    /// whole: the ground's light is OFF there whatever its page reads.
    #[test]
    fn the_ground_writes_mains_page_and_lights_only_a_light_scheme() {
        let t0 = Instant::now();
        let unset = aterm_core::render::COLOR_UNSET;
        let pages = [
            LIGHT,
            DARK,
            (DARK.1, DARK.0),
            (LIGHT.1, LIGHT.0),
            (0x00ff_ffff, 0x00a0_a0a0),
            (unset, LIGHT.1),
            (LIGHT.0, unset),
        ];
        for scheme in [LIGHT, DARK] {
            for page in pages {
                let mut ground = TrailGround::default();
                let start = GlowConfig::default();
                let mut cfg = start;
                let out = ground.ground(&mut cfg, page, key(1, scheme), PageFacts::OPAQUE, t0);
                let main = mains(&start, page);
                assert_eq!(
                    GlowConfig {
                        light: Light::OFF,
                        ..cfg
                    },
                    main,
                    "{page:x?} on {scheme:x?}"
                );
                let raw = raw_light(page.0, page.1, aterm_render::theme_is_dark(page.1));
                assert_eq!(
                    raw,
                    matches!(page, LIGHT | (0x0011_1318, 0x00d0_d0d0)),
                    "the light grounds among the pages: Default Light's and Default's reversed"
                );
                let lit = scheme == LIGHT && raw;
                assert_eq!(out.on, lit, "{page:x?} on {scheme:x?}");
                assert_eq!(cfg.light, Light::admitted(lit, LIVE));
                assert_eq!(ground.light(), cfg.light);
                assert_eq!(
                    ground.seen(),
                    Some((key(1, scheme), raw)),
                    "the key and raw light it ran on"
                );
                if scheme == DARK {
                    assert_eq!(cfg, main, "a dark scheme's frame is main's");
                }
            }
        }
    }

    /// The latch runs on every grounded frame: a light scheme's flash leaves on its first frame
    /// and comes back on the first after it, and the falling edge is reported once.
    #[test]
    fn the_latch_runs_on_every_grounded_frame() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut ground = TrailGround::default();
        let mut cfg = GlowConfig::default();
        let first = ground.ground(&mut cfg, LIGHT, key(1, LIGHT), PageFacts::OPAQUE, at(0));
        assert!(first.on && first.scheme_changed);
        let flash = ground.ground(
            &mut cfg,
            (LIGHT.1, LIGHT.0),
            key(1, LIGHT),
            PageFacts::OPAQUE,
            at(16),
        );
        assert!(!flash.on && flash.fell && !flash.scheme_changed);
        assert_eq!(cfg.light, Light::OFF);
        assert!(cfg.dark_theme, "main reads the swapped page as dark");
        let back = ground.ground(&mut cfg, LIGHT, key(1, LIGHT), PageFacts::OPAQUE, at(116));
        assert!(back.on && !back.fell);
        assert_eq!(ground.last(), back);
    }

    /// `force_live` previews a level on the frames it lights, never on one the latch refused,
    /// and `None` restores [`LIVE`].
    #[test]
    fn a_forced_level_lights_only_what_the_latch_admits() {
        let t0 = Instant::now();
        let mut ground = TrailGround::default();
        assert_eq!(ground.live(), LIVE);
        ground.force_live(Some(Live::Glass));
        assert_eq!(ground.live(), Live::Glass);
        let mut cfg = GlowConfig::default();
        ground.ground(&mut cfg, LIGHT, key(1, LIGHT), PageFacts::OPAQUE, t0);
        assert_eq!(cfg.light, Light::at(Live::Glass));
        assert!(cfg.light.glass());
        let mut dark = GlowConfig::default();
        ground.ground(&mut dark, DARK, key(1, DARK), PageFacts::OPAQUE, t0);
        assert_eq!(
            dark.light,
            Light::OFF,
            "a forced level lights no dark frame"
        );
        ground.force_live(None);
        assert_eq!(ground.live(), LIVE);
    }

    /// A see-through page (a translucent window, a wallpaper or an image under the text) keeps
    /// the latch's verdict and its level but draws no glass, on the frame and read outside it;
    /// a clear page draws it; a dark frame is [`Light::OFF`] on every page. Negative control: the
    /// same frames on the opaque page light the glass.
    #[test]
    fn a_see_through_page_draws_no_glass() {
        let t0 = Instant::now();
        let pages = [
            (PageFacts::OPAQUE, true),
            (
                PageFacts {
                    opaque: false,
                    wallpaper: false,
                },
                false,
            ),
            (
                PageFacts {
                    opaque: true,
                    wallpaper: true,
                },
                false,
            ),
            (
                PageFacts {
                    opaque: false,
                    wallpaper: true,
                },
                false,
            ),
        ];
        assert_eq!(PageFacts::default(), PageFacts::OPAQUE);
        for (page, clear) in pages {
            assert_eq!(page.clear(), clear, "{page:?}");
            let mut ground = TrailGround::default();
            ground.force_live(Some(Live::Glass));
            let mut cfg = GlowConfig::default();
            let out = ground.ground(&mut cfg, LIGHT, key(1, LIGHT), page, t0);
            assert!(out.on, "{page:?}: the latch admits the light scheme");
            assert!(cfg.light.on(), "{page:?}");
            assert_eq!(cfg.light.glass(), clear, "{page:?}");
            assert_eq!(cfg.light.sheer(), !clear, "{page:?}");
            assert_eq!(
                ground.light(),
                cfg.light,
                "{page:?}: read outside the frame"
            );
            let mut dark = GlowConfig::default();
            ground.ground(&mut dark, DARK, key(1, DARK), page, t0);
            assert_eq!(dark.light, Light::OFF, "{page:?}: a dark frame");
        }
        assert_eq!(Light::OFF.on_page(false), Light::OFF);
    }

    /// A pane takes the frame's light only on a named light scheme over a light page.
    #[test]
    fn a_pane_is_lit_only_on_a_named_light_scheme_over_a_light_page() {
        let lit = Light::at(Live::Glass);
        assert_eq!(pane_light(lit, Some(LIGHT), LIGHT), lit);
        assert_eq!(pane_light(lit, None, LIGHT), Light::OFF, "no scheme named");
        assert_eq!(
            pane_light(lit, Some(DARK), LIGHT),
            Light::OFF,
            "a dark scheme"
        );
        assert_eq!(
            pane_light(lit, Some(LIGHT), (LIGHT.1, LIGHT.0)),
            Light::OFF,
            "a light scheme's flash"
        );
        assert_eq!(pane_light(Light::OFF, Some(LIGHT), LIGHT), Light::OFF);
    }
}
