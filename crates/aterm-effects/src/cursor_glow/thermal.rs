// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing heat: the momentum-driven heat every style reads and the fire style's forge temperature.

use super::*;

impl CursorGlow {
    /// The wake brightness multiplier for typing light at the current heat: cool typing is a whisper (~0.28×), sustained fast typing overdrives past the configured intensity (~1.2×).
    #[inline]
    pub(super) const fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// The raw blaze level 0..1: the typing heat or the jump flare, whichever burns hotter, un-eased.
    #[inline]
    #[must_use]
    pub const fn blaze(&self) -> f32 {
        self.heat.max(self.flare).clamp(0.0, 1.0)
    }

    /// The cadence credit one keystroke at inter-key gap `gap` earns: full at ≤ [`Self::HEAT_GAP_FULL`], none at ≥ [`Self::HEAT_GAP_ZERO`].
    #[inline]
    pub(super) const fn heat_cadence(gap: f32) -> f32 {
        1.0 - ((gap - Self::HEAT_GAP_FULL) / (Self::HEAT_GAP_ZERO - Self::HEAT_GAP_FULL))
            .clamp(0.0, 1.0)
    }

    /// The per-keystroke heat gain this style charges.
    #[inline]
    pub(super) const fn heat_gain(fire: bool) -> f32 {
        if fire {
            Self::FIRE_HEAT_GAIN
        } else {
            Self::HEAT_GAIN
        }
    }

    /// The heat cooling τ this style runs.
    #[inline]
    pub(super) const fn heat_tau(cfg: &GlowConfig) -> f32 {
        if matches!(cfg.style, GlowStyle::Fire) {
            Self::FIRE_HEAT_TAU
        } else {
            Self::HEAT_DECAY_TAU
        }
    }

    /// Fire's eased display temperature 0..1, the one number every fire layer reads: coal-floored (relaxed typing keeps a small live flame), quench-damped (deleting douses it), attack/release-eased (the whole look ramps as one body).
    #[inline]
    pub(super) const fn fire_t(&self) -> f32 {
        self.disp_t.clamp(0.0, 1.0)
    }

    /// The forge cursor fill for the fire style, always warm metal on the black-body ramp: a glowing ember at rest, climbing toward the hot end with sustained momentum, inside the page's [`ForgeBand`].
    #[inline]
    #[must_use]
    pub const fn forge_fill(&self) -> Option<u32> {
        const REF_BAND: ForgeBand = ForgeBand::solve(CursorGlow::STREAK_REF_GROUND, true);
        let band = match self.forge_band {
            Some(b) => b,
            None => REF_BAND,
        };
        let t = ((self.cursor_temp - Self::FORGE_MIN_TEMP) / (1.0 - Self::FORGE_MIN_TEMP))
            .clamp(0.0, 1.0);
        let (s, _) = crate::supernova::sin_cos_f32(self.flame_phase * 1.7);
        let breathe = 0.045 * t * s;
        Some(fire_ramp(band.at(t, breathe)))
    }

    /// Re-solve the [`ForgeBand`] when the page under the caret changed (a theme switch, DECSCNM): one bisection per new ground, nothing per frame.
    #[inline]
    pub(super) const fn refresh_forge_band(&mut self, cfg: &GlowConfig) {
        let ground = if cfg.theme_bg & 0xFF00_0000 != 0 {
            // No resolved page (`COLOR_UNSET`): the reference ground of the
            // ground's side.
            if cfg.dark_theme {
                Self::STREAK_REF_GROUND
            } else {
                0x00FF_FFFF
            }
        } else {
            cfg.theme_bg
        };
        let needs_solve = match self.forge_band {
            Some(b) => b.ground != ground || b.dark != cfg.dark_theme,
            None => true,
        };
        if needs_solve {
            self.forge_band = Some(ForgeBand::solve(ground, cfg.dark_theme));
        }
    }

    /// Zero the thermal integrators (heat / flare / coal / quench, the eased display temperatures, the typing and erase momentum metrics, the flame phase, the typing cadence clock): the full-dark half that master-off and [`Self::reset`] add on top of [`Self::clear_transient_state`].
    #[inline]
    pub(super) const fn clear_thermals(&mut self) {
        self.heat = 0.0;
        self.heat_at = None;
        self.flare = 0.0;
        self.coal = 0.0;
        self.quench = 0.0;
        self.disp_t = 0.0;
        self.cursor_temp = 0.0;
        self.flame_phase = 0.0;
        self.momentum.reset();
        self.erase_mom.reset();
        self.last_type = None;
    }

    /// Typing cadence → the thermal integrators (heat / coal / the canonical typing-momentum metric), plus the quench-steam flash on a deletion echo.
    pub(super) fn update_typing_thermals(&mut self, mv: &MoveCtx) -> f32 {
        let &MoveCtx {
            pr,
            pc,
            cc,
            now,
            geom,
            wrap,
            re_anchor,
            rainbow_coalesce,
            typed_hinted,
            bs_pair,
            typing,
            fire,
            forward,
            echo_run,
            deletion,
            navigation,
            ..
        } = mv;
        if typing {
            let gap = self
                .last_type
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(f32::MAX);
            if (forward || echo_run || !fire) && !deletion && !navigation {
                let cadence = Self::heat_cadence(gap);
                let gain = Self::heat_gain(fire);
                self.heat = (self.heat + cadence * gain).min(1.0);
                if fire {
                    let coal_cred = 1.0
                        - ((gap - Self::COAL_GAP_FULL)
                            / (Self::COAL_GAP_ZERO - Self::COAL_GAP_FULL))
                            .clamp(0.0, 1.0);
                    self.coal = (self.coal + coal_cred * Self::COAL_GAIN).min(1.0);
                }
            }
            if typed_hinted
                && (forward || (wrap && !bs_pair) || rainbow_coalesce)
                && !deletion
                && !navigation
            {
                self.momentum.advance(now);
                let kind = if wrap && cc < pc {
                    CursorCatMotionKind::FoldForward
                } else {
                    CursorCatMotionKind::Advance
                };
                let fold_held = kind == CursorCatMotionKind::Advance
                    && self
                        .momentum_pulse
                        .is_some_and(|p| p.kind == CursorCatMotionKind::FoldForward);
                if !fold_held {
                    self.momentum_pulse = Some(CursorCatMotionPulse { at: now, kind });
                }
            } else if deletion && re_anchor && cc > pc && !navigation {
                self.momentum_pulse = Some(CursorCatMotionPulse {
                    at: now,
                    kind: CursorCatMotionKind::FoldReverse,
                });
            }
            self.last_type = Some(now);
            if deletion && fire {
                let cell = geom.ch as f32;
                let ox = geom.origin_x as f32 + (pc as f32 + 0.5) * geom.cw as f32;
                let oy = geom.origin_y as f32 + (pr as f32 + 0.2) * cell;
                let puffs = (2 + (self.quench * 1.5) as usize).min(Self::STEAM_ECHO_PUFFS_MAX);
                self.spawn_quench_steam(puffs, |_| ox, oy, cell, geom.cw as f32, now);
            }
            gap
        } else {
            f32::MAX
        }
    }
}

/// The forge caret's span of the black-body ramp ([`fire_ramp`]), solved for one page: the resting ember sits at `lo`, full heat at `hi`, and every point between reads against the page at [`Self::PAGE_CONTRAST`] or better.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ForgeBand {
    /// The page this band was solved against.
    pub(super) ground: u32,
    /// The theme's side (`GlowConfig::dark_theme`) the solve was asked for.
    pub(super) dark: bool,
    /// Whether the band lies on the ramp's bright side (the ember lighter than the page), where the breath may ride above `hi`.
    pub(super) bright: bool,
    /// The resting ember's ramp position.
    pub(super) lo: f32,
    /// The ramp position at full heat.
    pub(super) hi: f32,
}

impl ForgeBand {
    /// The least contrast every forge fill keeps against its page: the host's caret floor ([`crate::cursor_fx::CARET_PAGE_MIN_CONTRAST`], WCAG's text minimum, since a block caret draws the glyph beneath it in the page colour), so the floor finds the band's ember already clear.
    pub(super) const PAGE_CONTRAST: f32 = crate::cursor_fx::CARET_PAGE_MIN_CONTRAST;
    /// The deep rest point a cold forge took before its page was solved, kept wherever it already reads (a light page).
    pub(super) const COLD: f32 = 0.10;
    /// The shipped full-heat point: orange-gold, short of cream.
    pub(super) const HOT: f32 = 0.74;
    /// The ramp ceiling the fill never passes, breathing included.
    pub(super) const TOP: f32 = 0.86;
    /// The least heating arc a dark page keeps between rest and full heat, so a page that pushes the ember up still shows the metal heating.
    pub(super) const MIN_ARC: f32 = 0.22;

    /// Solve the band for `ground`, preferring the side `dark` names.
    pub(super) const fn solve(ground: u32, dark: bool) -> Self {
        let table = crate::color_math::relative_luminance_table();
        #[inline]
        const fn ratio(table: &crate::color_math::RelativeLuminanceTable, lum: f32, p: f32) -> f32 {
            let l = crate::color_math::relative_luminance_with(table, fire_ramp(p));
            (l.max(lum) + 0.05) / (l.min(lum) + 0.05)
        }
        #[inline]
        const fn lit_rgb(
            table: &crate::color_math::RelativeLuminanceTable,
            lum: f32,
            rgb: u32,
        ) -> bool {
            let l = crate::color_math::relative_luminance_with(table, rgb);
            l > lum && (l + 0.05) / (lum + 0.05) >= ForgeBand::PAGE_CONTRAST
        }
        #[inline]
        const fn deep_ok_rgb(
            table: &crate::color_math::RelativeLuminanceTable,
            lum: f32,
            rgb: u32,
        ) -> bool {
            let l = crate::color_math::relative_luminance_with(table, rgb);
            l < lum && (lum + 0.05) / (l + 0.05) >= ForgeBand::PAGE_CONTRAST
        }
        let lum = crate::color_math::relative_luminance_with(table, ground);
        let bright = if lit_rgb(table, lum, fire_ramp(Self::TOP)) {
            let lo = if lit_rgb(table, lum, fire_ramp(Self::COLD)) {
                Self::COLD
            } else {
                bisect_edge(table, lum, Self::COLD, Self::TOP, true).1
            };
            Some((lo, (lo + Self::MIN_ARC).clamp(Self::HOT, Self::TOP)))
        } else {
            None
        };
        let deep = if deep_ok_rgb(table, lum, fire_ramp(0.0)) {
            let hi = if deep_ok_rgb(table, lum, fire_ramp(Self::HOT)) {
                Self::HOT
            } else {
                bisect_edge(table, lum, 0.0, Self::HOT, false).0
            };
            Some((Self::COLD.min(hi), hi))
        } else {
            None
        };
        let pick = if dark {
            match bright {
                Some(b) => Some((true, b)),
                None => match deep {
                    Some(d) => Some((false, d)),
                    None => None,
                },
            }
        } else {
            match deep {
                Some(d) => Some((false, d)),
                None => match bright {
                    Some(b) => Some((true, b)),
                    None => None,
                },
            }
        };
        let (bright, (lo, hi)) = match pick {
            Some(pair) => pair,
            None => {
                // Neither side reads: the ramp end that comes closest.
                if ratio(table, lum, Self::TOP) >= ratio(table, lum, 0.0) {
                    (true, (Self::TOP, Self::TOP))
                } else {
                    (false, (0.0, 0.0))
                }
            }
        };
        Self {
            ground,
            dark,
            bright,
            lo,
            hi,
        }
    }

    /// The ramp position at heat `t` (`0..=1`) with the metal's `breathe`: rest at `lo`, full heat at `hi`, never below the rest point and never past what the page allows (on the bright side the breath may ride up to [`Self::TOP`]).
    #[inline]
    pub(super) const fn at(self, t: f32, breathe: f32) -> f32 {
        let ceiling = if self.bright { Self::TOP } else { self.hi };
        (self.lo + (self.hi - self.lo) * t + breathe).clamp(self.lo, ceiling.max(self.lo))
    }
}

/// The edge of a predicate that holds on `[lo, edge]` and fails past it, to
/// well under one ramp step; the caller has checked it holds at `lo` and
/// fails at `hi`.
#[inline]
const fn bisect_edge(
    table: &crate::color_math::RelativeLuminanceTable,
    lum: f32,
    mut lo: f32,
    mut hi: f32,
    want_not_lit: bool,
) -> (f32, f32) {
    let (mut lo_rgb, mut hi_rgb) = (fire_ramp(lo), fire_ramp(hi));
    let mut i = 0;
    while i < 16 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = fire_ramp(mid);
        if mid_rgb == lo_rgb {
            lo = mid;
        } else if mid_rgb == hi_rgb {
            hi = mid;
        } else {
            let l = crate::color_math::relative_luminance_with(table, mid_rgb);
            let holds = if want_not_lit {
                !(l > lum && (l + 0.05) / (lum + 0.05) >= ForgeBand::PAGE_CONTRAST)
            } else {
                l < lum && (lum + 0.05) / (l + 0.05) >= ForgeBand::PAGE_CONTRAST
            };
            if holds {
                lo = mid;
                lo_rgb = mid_rgb;
            } else {
                hi = mid;
                hi_rgb = mid_rgb;
            }
        }
        i += 1;
    }
    (lo, hi)
}
