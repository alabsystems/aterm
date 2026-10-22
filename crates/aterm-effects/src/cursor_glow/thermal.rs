// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing heat: the momentum-driven heat every style reads and the fire
//! style's forge temperature.

use super::*;

impl CursorGlow {
    /// The wake brightness multiplier for typing light at the current heat:
    /// cool typing is a whisper (~0.28×), sustained fast typing overdrives
    /// past the configured intensity (~1.2×). Jump comets and landing rings
    /// are navigation feedback, so they stay at 1×.
    #[inline]
    pub(super) const fn typing_boost(&self) -> f32 {
        0.28 + 0.92 * self.heat
    }

    /// The raw blaze level 0..1: the typing heat or the jump flare, whichever
    /// burns hotter, un-eased. Non-fire styles (the laser impact flash) read
    /// this; fire reads the eased [`Self::fire_t`]. The host hands the same
    /// surge to the signature cursors (e.g.
    /// [`crate::cursor_droplet::CursorDroplet`]); read it after
    /// [`Self::tick`], which applies the lazy heat/flare decay.
    #[inline]
    #[must_use]
    pub const fn blaze(&self) -> f32 {
        self.heat.max(self.flare).clamp(0.0, 1.0)
    }

    /// The cadence credit one keystroke at inter-key gap `gap` earns: full at
    /// ≤ [`Self::HEAT_GAP_FULL`], none at ≥ [`Self::HEAT_GAP_ZERO`]. Shared
    /// with the key-time click's timbre prediction so the click matches the
    /// light it is the twin of.
    #[inline]
    pub(super) const fn heat_cadence(gap: f32) -> f32 {
        1.0 - ((gap - Self::HEAT_GAP_FULL) / (Self::HEAT_GAP_ZERO - Self::HEAT_GAP_FULL))
            .clamp(0.0, 1.0)
    }

    /// The per-keystroke heat gain this style charges. Fire earns its blaze
    /// over ~30-40 keys; every other style keeps the shared [`Self::HEAT_GAIN`].
    #[inline]
    pub(super) const fn heat_gain(fire: bool) -> f32 {
        if fire {
            Self::FIRE_HEAT_GAIN
        } else {
            Self::HEAT_GAIN
        }
    }

    /// The heat cooling τ this style runs. Fire cools on its own slower τ
    /// (momentum survives a short thought); every other style keeps
    /// [`Self::HEAT_DECAY_TAU`].
    #[inline]
    pub(super) const fn heat_tau(cfg: &GlowConfig) -> f32 {
        if matches!(cfg.style, GlowStyle::Fire) {
            Self::FIRE_HEAT_TAU
        } else {
            Self::HEAT_DECAY_TAU
        }
    }

    /// Fire's eased display temperature 0..1, the one number every fire layer
    /// reads: coal-floored (relaxed typing keeps a small live flame),
    /// quench-damped (deleting douses it), attack/release-eased (the whole
    /// look ramps as one body). Evolved once per tick in the lazy-decay block,
    /// before any spawn, so every consumer in one frame agrees.
    #[inline]
    pub(super) const fn fire_t(&self) -> f32 {
        self.disp_t.clamp(0.0, 1.0)
    }

    /// The forge cursor fill for the fire style, always warm metal on the
    /// black-body ramp: a glowing ember at rest, climbing toward the hot end
    /// with sustained momentum, inside the page's [`ForgeBand`]. Never the
    /// theme's cursor colour (the style is its colour; OSC 12 does not reach
    /// it, by design) and never `None`, so the caret is warm the instant the
    /// style is active.
    ///
    /// The glyph under a block caret is drawn in the page colour, so the
    /// fill's contrast against the page is that glyph's contrast; the band
    /// holds every fill at [`ForgeBand::PAGE_CONTRAST`] against the page the
    /// last tick saw (the reference dark ground before the first tick). The
    /// host's caret floor (`cursor_fx::floor_block_fill`) still runs over this
    /// fill and asks for the same 4.5:1, so on its own page it leaves the
    /// band's colour alone.
    /// Quantized to u8 channels, so frames with an imperceptible temperature
    /// change fingerprint identically and early-out: a cool cursor is a static
    /// ember (the heating arc keeps the timer armed via `is_active`'s
    /// `cursor_temp` term).
    #[inline]
    pub fn forge_fill(&self) -> Option<u32> {
        let band = self
            .forge_band
            .unwrap_or_else(|| ForgeBand::solve(Self::STREAK_REF_GROUND, true));
        let t = ((self.cursor_temp - Self::FORGE_MIN_TEMP) / (1.0 - Self::FORGE_MIN_TEMP))
            .clamp(0.0, 1.0);
        // A slow molten shimmer rides the field phase (deterministic; quantized to u8 by the
        // ramp, so idle settles).
        let breathe = 0.045 * t * (self.flame_phase * 1.7).sin();
        Some(fire_ramp(band.at(t, breathe)))
    }

    /// Re-solve the [`ForgeBand`] when the page under the caret changed (a
    /// theme switch, DECSCNM): one bisection per new ground, nothing per frame.
    #[inline]
    pub(super) fn refresh_forge_band(&mut self, cfg: &GlowConfig) {
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
        if self
            .forge_band
            .is_none_or(|b| b.ground != ground || b.dark != cfg.dark_theme)
        {
            self.forge_band = Some(ForgeBand::solve(ground, cfg.dark_theme));
        }
    }

    /// Zero the thermal integrators (heat / flare / coal / quench, the eased display
    /// temperatures, the typing and erase momentum metrics, the flame phase, the typing cadence
    /// clock): the full-dark half that master-off and [`Self::reset`] add on top of
    /// [`Self::clear_transient_state`]. The zero-amplitude path does not call this: a momentary
    /// unfocus only cools by the elapsed gap, so earned forge momentum survives a focus blip.
    #[inline]
    pub(super) fn clear_thermals(&mut self) {
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

    /// Typing cadence → the thermal integrators (heat / coal / the canonical
    /// typing-momentum metric), plus the quench-steam flash on a deletion
    /// echo. Returns the observed inter-key `gap` (`f32::MAX` off the typing
    /// path), which the life/chain math downstream keys on.
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
        // Each keystroke earns heat by how quickly it followed the previous one, so only
        // sustained fast typing ramps the wake up; the life math downstream also keys on `gap`.
        if typing {
            let gap = self
                .last_type
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(f32::MAX);
            // Forward-only momentum heats fire; the other styles keep direction-blind heat.
            // A typed echo run (a wide glyph's two cells, or keys the echo batched into one
            // hop) is a forward advance too, earning one key's credit: CJK typed into fire
            // heats it as Latin text does.
            if (forward || echo_run || !fire) && !deletion && !navigation {
                let cadence = Self::heat_cadence(gap);
                let gain = Self::heat_gain(fire);
                self.heat = (self.heat + cadence * gain).min(1.0);
                // Fire-only: the coal bed charges on a much wider cadence window, so human
                // rhythm writing (300-400 ms/key) builds a persistent ember floor over a
                // sentence or two.
                if fire {
                    let coal_cred = 1.0
                        - ((gap - Self::COAL_GAP_FULL)
                            / (Self::COAL_GAP_ZERO - Self::COAL_GAP_FULL))
                            .clamp(0.0, 1.0);
                    self.coal = (self.coal + coal_cred * Self::COAL_GAIN).min(1.0);
                }
            }
            // The canonical typing-momentum metric builds here and only here: a non-delete
            // printable typing advance (a forward glyph echo, a typing wrap/re-anchor, or a
            // coalesced multi-glyph echo). Deletes and kills drain at their key hints
            // ([`Self::note_backspace`]/[`Self::note_kill`]); rate normalization lives in
            // [`TypingMomentum::advance`], so key count never buys momentum. It is
            // style-independent, so a mid-run switch to rainbow kitty keeps its earned warmth.
            //
            // Every arm requires the fresh press hint ([`Self::note_typed`], armed by the host on
            // a real printable key): a forward same-row echo alone is program output. A printable
            // key whose echo is a forward move (vim `l`/`w`) is indistinguishable from typed
            // text. The spine advances on the press hint, not on candidate admission, so
            // streaming collisions that deny every candidate still let real typing earn its
            // starfield. A denied Backspace landing in a wrap shape (`bs_pair`) is excluded, and
            // one press credits at most one advance. The pulse mirrors the advance onto the
            // cursor cat ([`Self::take_momentum_pulse`]).
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
                // A held park's fold, flushed on this tick ahead of the key
                // that followed it ([`HeldPark`]), keeps the tick's pulse: the
                // cat turns the corner it really turned.
                let fold_held = kind == CursorCatMotionKind::Advance
                    && self
                        .momentum_pulse
                        .is_some_and(|p| p.kind == CursorCatMotionKind::FoldForward);
                if !fold_held {
                    self.momentum_pulse = Some(CursorCatMotionPulse { at: now, kind });
                }
            } else if deletion && re_anchor && cc > pc && !navigation {
                // The input seam already delivered `CursorCat::on_key(false)`
                // for this Backspace; this pulse carries only the place change,
                // so momentum is not drained twice.
                self.momentum_pulse = Some(CursorCatMotionPulse {
                    at: now,
                    kind: CursorCatMotionKind::FoldReverse,
                });
            }
            self.last_type = Some(now);
            // Quench steam: a deletion echo flashes vapor off the doused cell,
            // scaling with the quench meter, plus a hiss speck. A held
            // backspace keeps the cloud fed key after key, so a key adds at
            // most `STEAM_ECHO_PUFFS_MAX` puffs (the cloud's light is budgeted
            // as a whole in `emit_vapor`).
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

/// The forge caret's span of the black-body ramp ([`fire_ramp`]), solved for
/// one page: the resting ember sits at `lo`, full heat at `hi`, and every
/// point between reads against the page at [`Self::PAGE_CONTRAST`] or better.
///
/// On a dark page the ramp's dark end is lost in the ground, so the band is
/// on the ramp's bright side: `lo` is the lowest point that reaches the
/// contrast (an orange-red ember on the default dark theme) and the metal
/// heats from there toward gold. On a light page the ramp's hot end is lost
/// instead, so the band is on the deep side: the ember keeps its deep rest
/// point and `hi` is the hottest point that still reads, so the metal heats
/// from maroon to a deep orange, never to a yellow the page swallows. A page
/// takes the side its theme names when that side reads, else the other one;
/// where neither reaches the contrast (a mid-grey page) the band collapses
/// onto the ramp end that comes closest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ForgeBand {
    /// The page this band was solved against.
    pub(super) ground: u32,
    /// The theme's side (`GlowConfig::dark_theme`) the solve was asked for.
    pub(super) dark: bool,
    /// Whether the band lies on the ramp's bright side (the ember lighter than
    /// the page), where the breath may ride above `hi`.
    pub(super) bright: bool,
    /// The resting ember's ramp position.
    pub(super) lo: f32,
    /// The ramp position at full heat.
    pub(super) hi: f32,
}

impl ForgeBand {
    /// The least contrast every forge fill keeps against its page: the
    /// host's caret floor ([`crate::cursor_fx::CARET_PAGE_MIN_CONTRAST`],
    /// WCAG's text minimum, since a block caret draws the glyph beneath it in
    /// the page colour), so the floor finds the band's ember already clear.
    pub(super) const PAGE_CONTRAST: f32 = crate::cursor_fx::CARET_PAGE_MIN_CONTRAST;
    /// The deep rest point a cold forge took before its page was solved, kept
    /// wherever it already reads (a light page).
    pub(super) const COLD: f32 = 0.10;
    /// The shipped full-heat point: orange-gold, short of cream.
    pub(super) const HOT: f32 = 0.74;
    /// The ramp ceiling the fill never passes, breathing included.
    pub(super) const TOP: f32 = 0.86;
    /// The least heating arc a dark page keeps between rest and full heat, so
    /// a page that pushes the ember up still shows the metal heating.
    pub(super) const MIN_ARC: f32 = 0.22;

    /// Solve the band for `ground`, preferring the side `dark` names.
    pub(super) fn solve(ground: u32, dark: bool) -> Self {
        // The crate's WCAG luminance (`color_math`), the one any page floor
        // on the caret measures with, so such a floor leaves these fills be.
        let table = crate::color_math::relative_luminance_table();
        let lum_of = |c: u32| crate::color_math::relative_luminance_with(table, c);
        let lum = lum_of(ground);
        let ratio = |p: f32| {
            let l = lum_of(fire_ramp(p));
            (l.max(lum) + 0.05) / (l.min(lum) + 0.05)
        };
        // Bright side: lighter than the page and reading, false below and
        // true above, so the band starts at the first point that reads.
        let lit = |p: f32| {
            let l = lum_of(fire_ramp(p));
            l > lum && (l + 0.05) / (lum + 0.05) >= Self::PAGE_CONTRAST
        };
        let bright = lit(Self::TOP).then(|| {
            let lo = if lit(Self::COLD) {
                Self::COLD
            } else {
                bisect_edge(Self::COLD, Self::TOP, |p| !lit(p)).1
            };
            (lo, (lo + Self::MIN_ARC).clamp(Self::HOT, Self::TOP))
        });
        // Deep side: darker than the page and reading, true below and false
        // above, so the band ends at the last point that reads.
        let deep_ok = |p: f32| {
            let l = lum_of(fire_ramp(p));
            l < lum && (lum + 0.05) / (l + 0.05) >= Self::PAGE_CONTRAST
        };
        let deep = deep_ok(0.0).then(|| {
            let hi = if deep_ok(Self::HOT) {
                Self::HOT
            } else {
                bisect_edge(0.0, Self::HOT, deep_ok).0
            };
            (Self::COLD.min(hi), hi)
        });
        let pick = if dark {
            bright.map(|b| (true, b)).or(deep.map(|d| (false, d)))
        } else {
            deep.map(|d| (false, d)).or(bright.map(|b| (true, b)))
        };
        let (bright, (lo, hi)) = pick.unwrap_or_else(|| {
            // Neither side reads: the ramp end that comes closest.
            if ratio(Self::TOP) >= ratio(0.0) {
                (true, (Self::TOP, Self::TOP))
            } else {
                (false, (0.0, 0.0))
            }
        });
        Self {
            ground,
            dark,
            bright,
            lo,
            hi,
        }
    }

    /// The ramp position at heat `t` (`0..=1`) with the metal's `breathe`:
    /// rest at `lo`, full heat at `hi`, never below the rest point and never
    /// past what the page allows (on the bright side the breath may ride up
    /// to [`Self::TOP`]).
    #[inline]
    pub(super) const fn at(self, t: f32, breathe: f32) -> f32 {
        let ceiling = if self.bright { Self::TOP } else { self.hi };
        (self.lo + (self.hi - self.lo) * t + breathe).clamp(self.lo, ceiling.max(self.lo))
    }
}

/// The edge of a predicate that holds on `[lo, edge]` and fails past it, to
/// well under one ramp step; the caller has checked it holds at `lo` and
/// fails at `hi`. Returns the last point known to hold and the first known to
/// fail.
#[inline]
fn bisect_edge(mut lo: f32, mut hi: f32, holds: impl Fn(f32) -> bool) -> (f32, f32) {
    for _ in 0..16 {
        let mid = 0.5 * (lo + hi);
        if holds(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo, hi)
}
