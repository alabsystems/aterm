// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The light latch (`docs/design/PRISM-GLASS.md` §2.1, §15.2 1.2): whether a window's frame draws
//! by the light rules.
//!
//! A frame may be light only when the front session's CONFIGURED scheme is light
//! ([`scheme_light`]: the pair its config or tab theme names, which DECSCNM and OSC 10/11 never
//! move) and the live page is a light ground ([`raw_light`]). Leaving is immediate: a frame that
//! is not both is off. Entering is latched: a light frame is on at once when its key is fresh
//! (the window's first frame, a new front session or a moved configured pair, held pending across
//! the frames that are not light until the first one that is), when the page comes back within
//! [`LIGHT_ENTRY_HOLD`] of the last frame that was on, and otherwise once raw light has held that
//! long. [`LightLatch::admits`] runs on every frame, never behind a short-circuit.
//! `aterm_models_effects::light_latch_model` is its model, which
//! `tests/light_latch_conformance.rs` holds it to.

use std::time::Duration;

use aterm_time::Instant;

use crate::color_math::{relative_luminance_table, relative_luminance_with};

/// How long raw light must hold on a light scheme before it enters with no other licence.
pub const LIGHT_ENTRY_HOLD: Duration = Duration::from_millis(300);

/// What a window's frame is latched on: its front session, and that session's configured
/// `(fg, bg)` (`0x00RRGGBB`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LatchKey {
    pub session: u64,
    pub fg: u32,
    pub bg: u32,
}

/// One frame's verdict.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LatchOut {
    /// The frame draws by the light rules.
    pub on: bool,
    /// The key is not the one the latch saw on its last frame, or this is its first: a new front
    /// session, or a writer moved the configured pair.
    pub scheme_changed: bool,
    /// The last frame was on and this one is not.
    pub fell: bool,
}

/// Whether the live page `(fg, bg)` is a light ground: not dark by main's polarity (`dark_theme`,
/// the frame's [`aterm_render::theme_is_dark`] of `bg`), its ink darker than its page, and the
/// two at least 3:1 apart. A colour that is no resolved `0x00RRGGBB` (`COLOR_UNSET`) is never
/// light.
#[must_use]
#[inline]
pub const fn raw_light(fg: u32, bg: u32, dark_theme: bool) -> bool {
    if !resolved(fg) || !resolved(bg) || dark_theme {
        return false;
    }
    let table = relative_luminance_table();
    let (l_fg, l_bg) = (
        relative_luminance_with(table, fg),
        relative_luminance_with(table, bg),
    );
    l_fg < l_bg && (l_bg + 0.05) / (l_fg + 0.05) >= 3.0
}

/// Whether a configured background is a light scheme's: main's polarity,
/// [`aterm_render::theme_is_dark`]. A colour that is no resolved `0x00RRGGBB` is never light.
#[must_use]
#[inline]
pub fn scheme_light(bg: u32) -> bool {
    resolved(bg) && !aterm_render::theme_is_dark(bg)
}

/// `rgb` is a packed colour, not a sentinel in the high byte.
#[inline]
const fn resolved(rgb: u32) -> bool {
    rgb >> 24 == 0
}

/// One window's latch.
#[derive(Clone, Copy, Debug, Default)]
pub struct LightLatch {
    /// The last frame's key; `None` before the first frame.
    key: Option<LatchKey>,
    /// A key change no light frame has met yet.
    pending: bool,
    /// The first frame of the current run of light frames.
    raw_since: Option<Instant>,
    /// The last frame that was on.
    last_on: Option<Instant>,
    /// The last frame's verdict.
    out: LatchOut,
}

impl LightLatch {
    /// The verdict for the frame at `now` on `key`, whose live page reads `raw` ([`raw_light`]).
    #[inline]
    pub fn admits(&mut self, now: Instant, key: LatchKey, raw: bool) -> LatchOut {
        let scheme_changed = self.key.replace(key) != Some(key);
        let fresh = self.pending || scheme_changed;
        let on = if raw && scheme_light(key.bg) {
            self.pending = false;
            let since = *self.raw_since.get_or_insert(now);
            fresh
                || now.saturating_duration_since(since) >= LIGHT_ENTRY_HOLD
                || self
                    .last_on
                    .is_some_and(|t| since.saturating_duration_since(t) <= LIGHT_ENTRY_HOLD)
        } else {
            self.pending = fresh;
            self.raw_since = None;
            false
        };
        if on {
            self.last_on = Some(now);
        }
        self.out = LatchOut {
            on,
            scheme_changed,
            fell: self.out.on && !on,
        };
        self.out
    }

    /// The last frame's verdict; all false before the first frame.
    #[inline]
    #[must_use]
    pub const fn last(&self) -> LatchOut {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::{ColorBaseline, Terminal};

    /// Default Light's pair.
    const LIGHT: LatchKey = LatchKey {
        session: 1,
        fg: 0x002b_2d33,
        bg: 0x00f6_f7f9,
    };

    /// Default's pair.
    const DARK: LatchKey = LatchKey {
        session: 1,
        fg: 0x00d0_d0d0,
        bg: 0x0011_1318,
    };

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    /// Frames every 16 ms over `from..=to` ms on `key` with the page reading `raw`: whether any
    /// was on, and the last verdict.
    fn frames(
        latch: &mut LightLatch,
        t0: Instant,
        (from, to): (u64, u64),
        key: LatchKey,
        raw: bool,
    ) -> (bool, LatchOut) {
        let mut any = false;
        let mut out = latch.last();
        for ms in (from..=to).step_by(16) {
            out = latch.admits(at(t0, ms), key, raw);
            any |= out.on;
        }
        (any, out)
    }

    /// A light scheme's window, lit from its first frame through 100 ms.
    fn lit(t0: Instant) -> LightLatch {
        let mut latch = LightLatch::default();
        let first = latch.admits(t0, LIGHT, true);
        assert!(first.on && first.scheme_changed, "the first frame is fresh");
        let (_, out) = frames(&mut latch, t0, (16, 84), LIGHT, true);
        assert!(out.on && !out.scheme_changed);
        assert!(latch.admits(at(t0, 100), LIGHT, true).on);
        latch
    }

    #[test]
    fn raw_light_needs_dark_ink_on_a_light_page_at_three_to_one() {
        assert!(raw_light(LIGHT.fg, LIGHT.bg, false));
        assert!(
            !raw_light(LIGHT.fg, LIGHT.bg, true),
            "main's polarity first"
        );
        assert!(
            !raw_light(LIGHT.bg, LIGHT.fg, false),
            "light ink is no page"
        );
        assert!(!raw_light(DARK.fg, DARK.bg, false));
        // The pairs §2.1 measured: main reads #a0a0a0 light (BT.601 luma 160 > 150), yet white
        // ink on it is no light ground, and none on #9a9a9a either.
        assert!(!aterm_render::theme_is_dark(0x00a0_a0a0));
        assert!(!raw_light(0x00ff_ffff, 0x00a0_a0a0, false));
        assert!(!raw_light(0x00ff_ffff, 0x009a_9a9a, false));
        // Too close to read at 3:1.
        assert!(!raw_light(0x00b0_b0b0, 0x00f6_f7f9, false));
        let unset = aterm_core::render::COLOR_UNSET;
        assert!(!raw_light(unset, LIGHT.bg, false) && !raw_light(LIGHT.fg, unset, false));
        assert!(scheme_light(LIGHT.bg) && !scheme_light(DARK.bg) && !scheme_light(unset));
    }

    /// §2.1: an app resetting its OSC 11 dark page on a light scheme waits the hold.
    #[test]
    fn light_returning_after_more_than_300_ms_away_waits_300_ms() {
        let t0 = Instant::now();
        let mut latch = lit(t0);
        let away = latch.admits(at(t0, 116), LIGHT, false);
        assert!(!away.on && away.fell, "leaving is this frame");
        let (any, _) = frames(&mut latch, t0, (132, 516), LIGHT, false);
        assert!(!any);
        assert!(!latch.admits(at(t0, 532), LIGHT, true).on, "432 ms away");
        let (any, _) = frames(&mut latch, t0, (548, 820), LIGHT, true);
        assert!(!any, "raw light holds for 300 ms first");
        assert!(!latch.admits(at(t0, 831), LIGHT, true).on, "299 ms held");
        let back = latch.admits(at(t0, 832), LIGHT, true);
        assert!(back.on && !back.scheme_changed && !back.fell, "300 ms held");
    }

    /// §2.1: a light scheme's DECSCNM flash leaves at once and comes back at once.
    #[test]
    fn light_returning_within_300_ms_enters_at_once() {
        let t0 = Instant::now();
        let mut latch = lit(t0);
        let (any, _) = frames(&mut latch, t0, (116, 216), LIGHT, false);
        assert!(!any, "the flash draws main's rules");
        assert!(latch.admits(at(t0, 232), LIGHT, true).on, "132 ms away");

        for (back_ms, on) in [(400, true), (401, false)] {
            let mut latch = lit(t0);
            assert!(!latch.admits(at(t0, 116), LIGHT, false).on);
            let back = latch.admits(at(t0, back_ms), LIGHT, true);
            assert_eq!(
                back.on,
                on,
                "back {} ms after the last lit frame",
                back_ms - 100
            );
        }
    }

    /// §2.1: an OS appearance flip, or a light-to-light change, enters on the frame it lands.
    #[test]
    fn a_configured_scheme_change_enters_on_its_first_light_frame() {
        let t0 = Instant::now();
        let mut latch = LightLatch::default();
        // A dark scheme under a program's light page never enters…
        let (any, _) = frames(&mut latch, t0, (0, 1_000), DARK, true);
        assert!(!any);
        // …and the flip to Default Light enters at once on it.
        let flip = latch.admits(at(t0, 1_016), LIGHT, true);
        assert!(flip.on && flip.scheme_changed);

        // Light away past the hold, back on another light pair: no wait. Its twin on the same
        // pair waits the hold.
        let solarized = LatchKey {
            fg: 0x0058_6e75,
            bg: 0x00fd_f6e3,
            ..LIGHT
        };
        let mut latch = lit(t0);
        let mut twin = lit(t0);
        frames(&mut latch, t0, (116, 1_000), LIGHT, false);
        frames(&mut twin, t0, (116, 1_000), LIGHT, false);
        let moved = latch.admits(at(t0, 1_016), solarized, true);
        assert!(moved.on && moved.scheme_changed);
        assert!(!twin.admits(at(t0, 1_016), LIGHT, true).on);
    }

    /// §15.2 1.2: a change seen on a frame that is not light stays pending until the first frame
    /// that is (§2.1 as written spent it on the dark frame, and the light frame then waited).
    #[test]
    fn a_scheme_change_seen_on_a_dark_page_still_enters_on_its_first_light_frame() {
        let t0 = Instant::now();
        let solarized = LatchKey {
            fg: 0x0058_6e75,
            bg: 0x00fd_f6e3,
            ..LIGHT
        };
        let mut latch = lit(t0);
        let mut twin = lit(t0);
        frames(&mut latch, t0, (116, 484), LIGHT, false);
        frames(&mut twin, t0, (116, 484), LIGHT, false);
        let seen = latch.admits(at(t0, 500), solarized, false);
        assert!(!seen.on && seen.scheme_changed, "seen on the dark page");
        let (any, out) = frames(&mut latch, t0, (516, 1_000), solarized, false);
        assert!(!any && !out.scheme_changed, "reported once");
        let first = latch.admits(at(t0, 1_016), solarized, true);
        assert!(
            first.on && !first.scheme_changed,
            "the pending change enters"
        );
        frames(&mut twin, t0, (500, 1_000), LIGHT, false);
        assert!(
            !twin.admits(at(t0, 1_016), LIGHT, true).on,
            "control: no change waits"
        );
    }

    /// §15.2 1.2: the key holds the front session, so a tab switched back from a session whose
    /// page was dark enters at once, though both wear the same light pair.
    #[test]
    fn a_tab_switch_back_to_a_light_session_enters_at_once() {
        let t0 = Instant::now();
        let other = LatchKey {
            session: 2,
            ..LIGHT
        };
        let mut latch = lit(t0);
        let to = latch.admits(at(t0, 116), other, false);
        assert!(!to.on && to.fell && to.scheme_changed);
        let (any, _) = frames(&mut latch, t0, (132, 1_000), other, false);
        assert!(!any);
        let back = latch.admits(at(t0, 1_016), LIGHT, true);
        assert!(back.on && back.scheme_changed, "the switch back is fresh");

        // Control: the same 916 ms of dark page on the front session waits the hold.
        let mut stay = lit(t0);
        frames(&mut stay, t0, (116, 1_000), LIGHT, false);
        assert!(!stay.admits(at(t0, 1_016), LIGHT, true).on);
    }

    /// A page a program lays over its configured scheme.
    const PAGES: [(&str, &[u8]); 5] = [
        ("plain", b""),
        ("DECSCNM", b"\x1b[?5h"),
        ("OSC 11 #ffffff", b"\x1b]11;#ffffff\x1b\\"),
        ("OSC 11 #f6f7f9", b"\x1b]11;#f6f7f9\x1b\\"),
        (
            "OSC 10 #2b2d33, OSC 11 #f6f7f9",
            b"\x1b]10;#2b2d33\x1b\\\x1b]11;#f6f7f9\x1b\\",
        ),
    ];

    fn pack(c: aterm_types::Rgb) -> u32 {
        aterm_render::rgb_to_u32([c.r, c.g, c.b])
    }

    /// A terminal configured to `scheme` with `page` laid over it: the latch's key (session 1 and
    /// the configured pair) and the live page's raw light, as the host reads them.
    fn ground(scheme: &aterm_types::scheme::ColorScheme, page: &[u8]) -> (LatchKey, bool) {
        let mut term = Terminal::new(4, 20);
        term.rebase_colors(&ColorBaseline {
            foreground: scheme.foreground,
            background: scheme.background,
            cursor: None,
            selection_background: None,
            selection_foreground: None,
            palette: None,
            color_scheme: None,
        });
        term.process(page);
        let configured = term.color_baseline();
        let key = LatchKey {
            session: 1,
            fg: pack(configured.foreground),
            bg: pack(configured.background),
        };
        let blank = term.implicit_blank_render_cell();
        let (fg, bg) = (
            aterm_render::rgb_to_u32(blank.fg),
            aterm_render::rgb_to_u32(blank.bg),
        );
        (key, raw_light(fg, bg, aterm_render::theme_is_dark(bg)))
    }

    /// Every built-in and pack scheme `aterm_render::theme_is_dark` reads dark, by name.
    fn dark_schemes() -> Vec<(String, aterm_types::scheme::ColorScheme)> {
        let builtins = aterm_types::scheme::builtin_names()
            .into_iter()
            .filter_map(|name| Some((name.to_owned(), aterm_types::scheme::builtin(name)?)));
        let packs = aterm_theme_pack::themes().iter().filter_map(|theme| {
            let scheme = aterm_types::scheme::parse_scheme_str(theme.text).ok()?;
            Some((theme.id.to_owned(), scheme))
        });
        builtins
            .chain(packs)
            .filter(|(_, scheme)| aterm_render::theme_is_dark(pack(scheme.background)))
            .collect()
    }

    /// §2.1: a dark configured scheme never enters, whatever DECSCNM or OSC 10/11 lays over it,
    /// on its first frame, after the hold, through a flash, or on a tab switch. Default Light's
    /// pair under the same pages is the control.
    #[test]
    fn no_dark_scheme_enters_under_decscnm_or_osc11() {
        let dark = dark_schemes();
        for theme in aterm_theme_pack::themes() {
            if let Err(err) = aterm_types::scheme::parse_scheme_str(theme.text) {
                panic!("pack theme {} parses: {err:?}", theme.id);
            }
        }
        let packs = aterm_theme_pack::themes()
            .iter()
            .filter(|theme| dark.iter().any(|(id, _)| id == theme.id))
            .count();
        assert!(
            packs > 0 && dark.len() > packs,
            "built-ins and pack themes both swept"
        );
        assert!(dark.len() >= 37, "{} dark schemes swept", dark.len());
        let t0 = Instant::now();
        let mut raw_pages = 0;
        for (name, scheme) in &dark {
            for (page, bytes) in PAGES {
                let (key, raw) = ground(scheme, bytes);
                let other = LatchKey { session: 2, ..key };
                assert!(
                    !scheme_light(key.bg),
                    "{name}: the configured pair stays dark"
                );
                raw_pages += usize::from(raw);
                let mut latch = LightLatch::default();
                let mut any = latch.admits(t0, key, raw).on;
                any |= frames(&mut latch, t0, (16, 500), key, raw).0;
                any |= frames(&mut latch, t0, (516, 600), key, false).0;
                any |= frames(&mut latch, t0, (616, 1_000), key, raw).0;
                any |= frames(&mut latch, t0, (1_016, 1_100), other, raw).0;
                any |= frames(&mut latch, t0, (1_116, 1_200), key, raw).0;
                assert!(!any, "{name} under {page} entered");
            }
        }
        // The sweep reaches raw light: every scheme under Default Light's own page, and others
        // under DECSCNM.
        assert!(raw_pages > dark.len(), "{raw_pages} raw light pages");

        let light = aterm_types::scheme::builtin("Default Light").expect("a built-in");
        let (key, raw) = ground(&light, b"");
        assert!(
            raw && LightLatch::default().admits(t0, key, raw).on,
            "control"
        );
        let (key, raw) = ground(&light, b"\x1b[?5h");
        assert!(!raw && !LightLatch::default().admits(t0, key, raw).on);
    }
}
