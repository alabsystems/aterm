// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Emission: turning the engine's live state into this frame's quads, halos
//! and patches, style by style.

use super::*;

impl CursorGlow {
    // ----- emitting -----

    /// Emit the live lightning bolts: each frozen jagged channel draws as a
    /// wide same-hue corona under a hot core (branches thinner, dimmer, no
    /// white mix), through the shared [`comet_beam`] so strikes stay smooth at
    /// any angle with CPU/GPU parity.
    ///
    /// One flash per bolt ([`bolt_flash`]: born at full strength, then a
    /// monotone burn-out while the channel retracts from its root), so every
    /// pixel a bolt lights rises once and falls once; how often bolts are born
    /// is the flash limiter's ([`Self::admit_bolt`]). A typing crackle crosses
    /// the newest letters, so on a dark ground it spends the streak's over-ink
    /// budget ([`Self::over_ink_ceiling`] of its own core-plus-corona stack);
    /// a jump strike keeps its full white-hot punch over air, and where its
    /// channel crosses a line of text the frame shows ([`TextView::covers_cell`]:
    /// an Enter at the bottom strikes up through the lines it just printed) it
    /// is held to that same ceiling in its own hue, so those letters keep
    /// 4.5:1.
    ///
    /// On a light ground additive light cannot show, so the same geometry is
    /// painted source-over ([`comet_beam_over`]) with no white-hot core, into
    /// the under-ink stream the caller hands the light arm: a strike across a
    /// line passes beneath the letters and leaves their strokes intact. So
    /// nothing it darkens is ink, and it takes the vivid ink of that role
    /// ([`InkRole::Leading`]) at most [`Self::LIGHT_BOLT_ALPHA_CAP`] opaque.
    pub(super) fn emit_bolts(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        verts: &mut Vec<BeamVertex>,
        channel: &mut Vec<(f32, f32, f32, bool)>,
    ) {
        if !matches!(cfg.style, GlowStyle::Laser) || self.bolts.is_empty() {
            return;
        }
        let ch = geom.ch as f32;
        let dark = cfg.dark_theme;
        // The vivid ink holds one weight across the hues, the weight the
        // opacity cap is measured at; a black trail colour carries no hue to
        // weigh and would come back black, so it strikes in the bar's neutral
        // grey, as any achromatic colour does.
        let light_ink = match InkRole::Leading.ink(cfg.color) {
            0 => InkRole::Leading.ink(0x00FF_FFFF),
            ink => ink,
        };
        let alpha_cap = Self::LIGHT_BOLT_ALPHA_CAP;
        // The crackle's over-ink ceiling, solved once per frame on first use:
        // its core and its corona (a third of the core's coverage) overlap on
        // the axis, in the bolt's own hue.
        let mut crackle_cap: Option<f32> = None;
        let lift_cap = || {
            let lift = [16u32, 8, 0]
                .map(|sh| ((cfg.color >> sh) & 0xff) as f32 * (1.0 + 1.0 / 3.0) / 255.0);
            Self::over_ink_ceiling(lift)
        };
        // A strike's hold over the text it crosses (dark only; a light
        // ground's bolt is ink under the letters).
        let text = self.text_view();
        let over_text = |x: f32, y: f32| {
            let row = ((y - f32::from(geom.origin_y)) / ch).floor() as i32;
            let col = ((x - f32::from(geom.origin_x)) / geom.cw as f32).floor() as i32;
            text.covers_cell(row, col)
        };
        let hold_over_text = dark && !self.streak_unbudgeted();
        // Each channel resampled at most half a cell apart, so its hold over
        // text eases in and out within half a cell of a letter's edge.
        let step = (geom.cw as f32 * 0.5).max(1.0);
        let clip = geom.beam_clip();
        // Newest strikes first, so a saturated budget sheds the oldest light.
        for b in self.bolts.iter().rev() {
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            let u = (now.saturating_duration_since(b.born).as_secs_f32() / b.life).clamp(0.0, 1.0);
            let env = bolt_flash(u);
            let mut cov_f = 255.0 * env * cfg.intensity * if b.branch { 0.72 } else { 1.0 };
            if b.crackle && dark && !self.streak_unbudgeted() {
                let cap = *crackle_cap.get_or_insert_with(lift_cap);
                cov_f = cov_f.min(cap);
            }
            if !dark {
                // On white the coverage is the stroke's opacity, capped so a
                // letter the strike crosses keeps its strokes.
                cov_f = cov_f.min(alpha_cap);
            }
            if cov_f < 1.0 {
                continue;
            }
            let cov = cov_f.min(255.0) as u8;
            // Burn-out: the channel retracts from its origin toward the tip
            // with a glowing edge (20% of length), withdrawing into where it
            // landed.
            let k_burn = if u > BOLT_BURN_START {
                (u - BOLT_BURN_START) / (1.0 - BOLT_BURN_START)
            } else {
                0.0
            };
            let n1 = (b.pts.len() - 1).max(1) as f32;
            let burn = |p: f32, c: u8| -> u8 {
                ((c as f32) * ((p - k_burn) / 0.20).clamp(0.0, 1.0)) as u8
            };
            // The channel's points `(x, y, p, over text)`, `p` its place
            // origin → tip as the burn reads it.
            channel.clear();
            let held = hold_over_text && !b.crackle;
            let resample = dark && !b.crackle;
            for (i, w) in b.pts.windows(2).enumerate() {
                let ((x0, y0), (x1, y1)) = (w[0], w[1]);
                let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
                let n = if resample {
                    (len / step).ceil().max(1.0) as usize
                } else {
                    1
                };
                for k in 0..n {
                    let f = k as f32 / n as f32;
                    let (x, y) = (x0 + (x1 - x0) * f, y0 + (y1 - y0) * f);
                    channel.push((x, y, (i as f32 + f) / n1, held && over_text(x, y)));
                }
            }
            if let Some(&(x, y)) = b.pts.last() {
                channel.push((x, y, 1.0, held && over_text(x, y)));
            }
            let text_cap = if channel.iter().any(|c| c.3) {
                *crackle_cap.get_or_insert_with(lift_cap)
            } else {
                255.0
            };
            let mut stroke = |out: &mut Vec<GlowQuad>,
                              color: &dyn Fn(bool) -> u32,
                              cov: u8,
                              share: f32,
                              thick: f32,
                              step| {
                verts.clear();
                for &(x, y, p, over) in channel.iter() {
                    let c = if over {
                        (text_cap * share).min(f32::from(cov)) as u8
                    } else {
                        cov
                    };
                    verts.push(BeamVertex {
                        x,
                        y,
                        color: color(over),
                        cov: burn(p, c),
                    });
                }
                if dark {
                    comet_beam(out, clip, verts, thick, step, 0.0);
                } else {
                    comet_beam_over(out, clip, verts, thick, step, 0.0);
                }
            };
            // Corona: the electric field around the channel, pure style hue
            // (its ink on a light ground).
            let corona_th = ch * if b.branch { 0.30 } else { 0.55 };
            let corona = if dark { cfg.color } else { light_ink };
            stroke(out, &|_| corona, (cov / 3).max(1), 1.0 / 3.0, corona_th, 2);
            if out.len() >= Self::MAX_QUADS {
                return;
            }
            // Core: white-hot on dark main strikes (0.62 white ceiling keeps
            // it in the beam's hue family), pure hue on branches; on a light
            // ground a white core would read as a pale smear, so the core is
            // the ink.
            // Over text the core is the hue alone, the light the ceiling is
            // solved for.
            let core_color = if !dark {
                light_ink
            } else if b.branch {
                cfg.color
            } else {
                lerp_rgb(cfg.color, 0x00FF_FFFF, 0.62 * env)
            };
            let core_th = (ch * if b.branch { 0.10 } else { 0.18 }).max(1.5);
            let core = |over: bool| if over { cfg.color } else { core_color };
            stroke(out, &core, cov, 1.0, core_th, 1);
        }
    }

    /// Emit the vapor (steam, smoke and erase poofs) as growing, thinning
    /// radial halos. Steam: pale blue-white, born small and bright, swelling
    /// ~3× as its coverage collapses; the live cloud shares one stacking
    /// budget ([`Self::STEAM_STACK`]) and a puff thins as it climbs out of its
    /// row ([`Self::STEAM_ABOVE_DIM`]). Smoke: warm grey, swelling on a gentle
    /// waft, lingering. Poof: neutral grey and brief, the erase puff any style
    /// may shed. Under fire on a dark ground every puff over a line of text
    /// spends the over-ink account ([`InkLedger`]).
    pub(super) fn emit_vapor(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        halos: &mut Vec<RainHalo>,
    ) {
        if self.vapor.is_empty() {
            return;
        }
        // Steam and smoke are fire's thermal language and render only under
        // it; the style-agnostic erase poof renders everywhere.
        let fire = matches!(cfg.style, GlowStyle::Fire);
        let cell = geom.ch as f32;
        let oy = f32::from(geom.origin_y);
        // Steam's stacking budget: a held backspace keeps a few puffs per key
        // alive at once, and their saturating sum whited out the line above.
        // The live cloud's light (each puff's own `(1 − t)²` fade) shares
        // `STEAM_STACK` fresh puffs' worth, however many keys fed it: a
        // single delete still hisses at full strength, a run grows the cloud
        // wider, not brighter.
        let budgeted = !self.streak_unbudgeted();
        let steam_share = if fire && budgeted {
            let live: f32 = self
                .vapor
                .iter()
                .filter(|v| v.kind == VaporKind::Steam)
                .map(|v| {
                    let t = (now.saturating_duration_since(v.born).as_secs_f32() / v.life)
                        .clamp(0.0, 1.0);
                    (1.0 - t) * (1.0 - t)
                })
                .sum();
            (Self::STEAM_STACK / live.max(f32::EPSILON)).min(1.0)
        } else {
            1.0
        };
        let text = TextView::new(
            if fire {
                self.probed_text_spans()
            } else {
                TextSpans::default()
            },
            Some(&self.frame_text),
        );
        let covers_text = |x: f32, y: f32| {
            let row = ((y - oy) / cell).floor() as i32;
            let col = ((x - f32::from(geom.origin_x)) / geom.cw as f32).floor() as i32;
            text.covers_cell(row, col)
        };
        for v in self.vapor.iter().rev() {
            if !fire && v.kind != VaporKind::Poof {
                continue;
            }
            let age = now.saturating_duration_since(v.born).as_secs_f32();
            let t = (age / v.life).clamp(0.0, 1.0);
            let x = v.x0
                + v.vx * age
                + (age * (1.3 + 1.9 * v.seed) + v.seed * std::f32::consts::TAU).sin()
                    * cell
                    * 0.22
                    * t;
            let y = v.y0 + v.vy * age + 0.5 * v.gy * age * age;
            // Smoke keeps off a line of text the frame shows (a prompt bar
            // or a status line above the burning row): its warm grey over
            // letters read as khaki blots on them.
            if v.kind == VaporKind::Smoke && budgeted && covers_text(x, y) {
                continue;
            }
            // Light themes: additive vapor cannot brighten white, so puffs are
            // source-over veils (straight RGB, falloff as alpha): steam a cool
            // pale mist, smoke a warm grey.
            let (r0, grow, color, peak) = match v.kind {
                VaporKind::Steam => {
                    // A puff thins as it rises out of the doused row into the
                    // line above (a fade over its first 0.6 cell of climb past
                    // the row's top), so the cloud hangs over the deleted text
                    // and the line above keeps its letters.
                    let row_top = oy + ((v.y0 - oy) / cell).floor() * cell;
                    let climb = ((row_top - y) / (0.6 * cell)).clamp(0.0, 1.0);
                    let rise_fade = if budgeted {
                        1.0 - Self::STEAM_ABOVE_DIM * climb * climb * (3.0 - 2.0 * climb)
                    } else {
                        1.0
                    };
                    (
                        cell * 0.26,
                        1.0 + 3.4 * t,
                        if cfg.dark_theme {
                            0x00C9_D6DE
                        } else {
                            0x00AB_BCC6
                        },
                        (150.0 * (1.0 - t) * (1.0 - t)) * cfg.intensity * steam_share * rise_fade,
                    )
                }
                VaporKind::Smoke => (
                    cell * 0.15,
                    1.0 + 2.4 * t,
                    if cfg.dark_theme {
                        // A grey wisp: a warm puff read as a khaki blot
                        // over the dark.
                        lerp_rgb(0x005E_5E60, 0x0042_4246, t)
                    } else {
                        lerp_rgb(0x0060_544A, 0x0084_7E78, t)
                    },
                    (if cfg.dark_theme { 34.0 } else { 52.0 }) * (1.0 - t) * cfg.intensity,
                ),
                // The erase poof: neutral grey (text going up in smoke, not
                // fire). On light themes a mid-grey source-over veil with
                // steam's stronger peak, which reads by contrast. On dark an
                // additive puff reads by area, so it is born wider (0.34 cell),
                // swells less, and its opacity falls quadratically from a
                // punchier peak: it arrives and dissipates rather than lingering.
                //
                // The peak is a stacking budget: puffs cluster, and three or
                // four overlap near the collapse point. Over a Nord ground the
                // blue channel clips at Σα ≈ 285, so 84 keeps a three-puff stack
                // (≈ 252) under the clip; the cloud's size says how much vanished
                // and the caret stays the brightest thing on the row.
                VaporKind::Poof if cfg.dark_theme => (
                    cell * 0.34,
                    1.0 + 2.2 * t,
                    lerp_rgb(0x009A_A0A6, 0x005F_6368, t),
                    (84.0 * (1.0 - t) * (1.0 - t)) * cfg.intensity,
                ),
                VaporKind::Poof => (
                    cell * 0.16,
                    1.0 + 2.6 * t,
                    lerp_rgb(0x005A_6065, 0x008B_9196, t),
                    (140.0 * (1.0 - t) * (1.0 - t)) * cfg.intensity,
                ),
            };
            if peak < 1.0 {
                continue;
            }
            let r = r0 * grow;
            if cfg.dark_theme {
                // Under fire, vapor over a line of text spends the over-ink
                // ledger like every other fire light (the erase poof of the
                // other styles keeps its own stacking budget).
                let share = if matches!(cfg.style, GlowStyle::Fire) {
                    self.ink_ledger.admit_halo(
                        (x, y),
                        (r * 1.35, r),
                        premul_rgb(color, peak as u8),
                        InkLedger::FLOOR,
                    )
                } else {
                    1.0
                };
                let peak = (peak * share) as u8;
                if peak == 0 {
                    continue;
                }
                push_halo(halos, geom, x, y, r * 1.35, r, premul_rgb(color, peak));
            } else {
                let peak = peak as u8;
                push_halo_over(halos, geom, x, y, r * 1.35, r, color, peak);
            }
        }
    }

    /// Emit the comet body as a smooth line of light rather than one opaque
    /// cell per swept step (which staircases on diagonals): the swept cells
    /// become a polyline that [`comet_beam`] rasterizes as an anti-aliased beam
    /// (a soft wide halo under a hot core). The head run extends to the live
    /// cursor cell so the beam stays attached.
    #[allow(
        clippy::too_many_arguments,
        reason = "clock + config + geometry + cursor + the three streams a beam can land in"
    )]
    pub(super) fn emit_comet(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
        under: &mut Vec<GlowQuad>,
    ) {
        // Styles whose streak is drawn elsewhere (Water = `emit_water`; rainbow kitty =
        // its own continuous ribbon) emit no shared additive beam. See `style_has_beam`.
        if !cfg.beam || self.sparks.is_empty() {
            return;
        }
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let oxf = geom.origin_x as f32;
        let center = |row: u16, col: u16| geom.cell_center(row, col);

        // Build the comet as runs of adjacent swept cells, so a hide/show gap
        // or stale older group never connects across empty space. A fully
        // faded tail cell breaks the colour, not the run. `runs_len` is the
        // live watermark; entries past it are emptied spares.
        for r in &mut self.comet_runs {
            r.clear();
        }
        for r in &mut self.comet_typing {
            r.clear();
        }
        let mut runs_len = 0usize;
        self.comet_run.clear();
        self.comet_run_typing.clear();
        let mut prev: Option<(u16, u16)> = None;
        // The head sample's coverage before Fire's caps: the bridge onto the
        // empty caret cell is the one place its streak runs at it.
        let mut head_full = 0u8;
        let stack = streak_layers(cfg.style);
        // Beam power-down: the tube's thickness follows the newest spark's
        // power envelope, holding full girth while live and thinning toward a
        // hairline as it dims.
        let beam_pw = if matches!(cfg.style, GlowStyle::Beam) {
            self.sparks
                .last()
                .map(|s| {
                    let age = now.saturating_duration_since(s.born).as_secs_f32();
                    beam_power((age / s.life).clamp(0.0, 1.0))
                })
                .unwrap_or(1.0)
        } else {
            1.0
        };
        // On a dark ground the beam family's streak lies under the letters
        // ([`Self::emit_dark_bed`]): its samples keep their whole coverage,
        // which lays the bed under text and the beam over air.
        let bed = if cfg.dark_theme && !self.streak_unbudgeted() {
            BedShape::of(cfg.style, beam_pw).map(|shape| {
                // Sparkle's tail is its rail, laid by distance from the head:
                // after a jump (a paste, a completion, a leap) `Length`
                // stretches it from the shipped four cells; while typing it
                // keeps them, as every style's typing tail does.
                let leapt = self.sparks.last().is_some_and(|s| !s.typing);
                if matches!(cfg.style, GlowStyle::Sparkle) && leapt {
                    let k = cfg.length as f32 / GLOW_LENGTH_DEFAULT as f32;
                    BedShape {
                        wedge: (shape.wedge * k).clamp(1.0, 4.0 * shape.wedge),
                        ..shape
                    }
                } else {
                    shape
                }
            })
        } else {
            None
        };
        // Fire's per-unit lift on each band of a cell, memoized by colour and
        // `pos` (consecutive samples of a typing run share both); its core
        // thickness is `core_thick`'s Fire arm below.
        let mut fire_lift_memo: Option<((u32, u32), BandLifts)> = None;
        let fire_core = (chf * (0.13 + 0.09 * self.fire_t())).max(2.0);
        // A dark fire's streak lays no light over a line of text: there it
        // read as a warm smear through the letters' x-height, and the flames
        // above the line are the fire. It lights only air (the caret's cell).
        let fire_text = if matches!(cfg.style, GlowStyle::Fire)
            && cfg.dark_theme
            && !self.streak_unbudgeted()
        {
            let spans = self.probed_text_spans();
            Some(TextView::new(spans, Some(&self.frame_text)))
        } else {
            None
        };
        for s in &self.sparks {
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            // Typing sparks hold full brightness for 55% of life then
            // cosine-fade, so the chained streak stays lit across inter-key
            // gaps (laser: a lightning-trail discharge). Jump sparks fade
            // linearly, except laser's, which hold for 45% of life and then burn
            // out tail→head behind a glowing 15% front (sparks of one jump share
            // born+life, so the front is coherent).
            let frac = (age / s.life).clamp(0.0, 1.0);
            let tf = if s.typing {
                if matches!(cfg.style, GlowStyle::Beam) {
                    // Beam: full power, then one cosine power-down; the rod dims
                    // uniformly while `core_thick` collapses it toward a
                    // hairline, like an emitter switching off.
                    beam_power(frac)
                } else if matches!(cfg.style, GlowStyle::Laser) {
                    // Lightning-trail discharge: full power while fresh, a
                    // cosine bleed to a residual charge that lingers (flickering
                    // under the ripple below) before its own collapse. Only the
                    // freshest cell is bright, so the wake never parks
                    // full-power light over the text.
                    if frac < 0.20 {
                        1.0
                    } else if frac < 0.55 {
                        let k = (frac - 0.20) / 0.35;
                        Self::LASER_RESIDUAL
                            + (1.0 - Self::LASER_RESIDUAL)
                                * 0.5
                                * (1.0 + (std::f32::consts::PI * k).cos())
                    } else {
                        Self::LASER_RESIDUAL
                            * 0.5
                            * (1.0 + (std::f32::consts::PI * (frac - 0.55) / 0.45).cos())
                    }
                } else if frac < 0.55 {
                    1.0
                } else {
                    0.5 * (1.0 + (std::f32::consts::PI * (frac - 0.55) / 0.45).cos())
                }
            } else if matches!(cfg.style, GlowStyle::Laser) {
                if frac < 0.45 {
                    1.0
                } else {
                    let k = (frac - 0.45) / 0.55; // burn front, tail 0 → head 1
                    ((s.pos - k) / 0.15).clamp(0.0, 1.0) * (1.0 - 0.35 * k)
                }
            } else if matches!(cfg.style, GlowStyle::Beam) {
                // A jumped beam is one rod spanning the leap and powers down
                // with the same uniform envelope as the typing tube.
                beam_power(frac)
            } else {
                1.0 - frac
            };
            // Phaser samples carry the cell's laid hue as an offset from the
            // live sweep phase (`comet_color` adds `self.hue` back), so each
            // cell keeps the colour it was laid in. Other styles use the
            // tail→head path position.
            let pos = if matches!(cfg.style, GlowStyle::Phaser) {
                (s.hue - self.hue).rem_euclid(1.0)
            } else {
                s.pos
            };
            // The sample's peak, before its time envelope `tf`. On a dark
            // ground the beam family keeps it whole: over the letters it
            // lays the bed (its share of the peak is the bed's opacity, so
            // the bed fades as the light would), and over air it is light.
            let peak_f = (s.born_cov as f32) * cfg.intensity;
            // On paper a laser's leap is its lightning's, out with the
            // strike: the fired beam laid under the lines the leap crosses
            // stood through their letters as a struck-through rule.
            let tf = if !cfg.dark_theme && matches!(cfg.style, GlowStyle::Laser) && !s.typing {
                0.0
            } else {
                tf
            };
            let mut cov_f = peak_f * tf;
            let mut full_f = cov_f;
            if matches!(cfg.style, GlowStyle::Fire) {
                // Fire keeps its own caps: the fire comet rides the typed line,
                // and its head stacked with halos and bloom would saturate
                // glyphs to white. The streak is held to FIRE_STREAK_COV_CAP;
                // the bridge onto the empty cursor cell keeps full punch below
                // (FIRE_HEAD_COV_CAP).
                cov_f = cov_f.min(Self::FIRE_STREAK_COV_CAP);
                full_f = cov_f;
                // On a dark ground the streak is the first light the over-ink
                // ledger charges: over a text cell a sample keeps the coverage
                // at which the layers of its stack reaching each band of the
                // cell, in its own colour, still leave the reference stroke
                // the steady lights' share of the contrast
                // (`InkLedger::STEADY_FLOOR`); the crown, forge, embers and
                // steam that follow spend what is left.
                if fire_text
                    .as_ref()
                    .is_some_and(|text| text.over_text(i32::from(s.row), i32::from(s.col)))
                {
                    cov_f = 0.0;
                } else if cfg.dark_theme {
                    let color = self.comet_color(cfg, pos);
                    let key = (color, pos.to_bits());
                    let lifts = match fire_lift_memo {
                        Some((k, l)) if k == key => l,
                        _ => {
                            let l = fire_band_lifts(stack, color, pos, fire_core, chf);
                            fire_lift_memo = Some((key, l));
                            l
                        }
                    };
                    cov_f = self.ink_ledger.admit_axis((s.row, s.col), &lifts, cov_f);
                }
            }
            // Laser scintillation: a ±14% energy ripple travelling head-ward,
            // deterministic in (pos, age). It modulates around full power, so it
            // reads as surging, never flicker toward dark. Applied to the
            // whole sample, so the bed under the letters surges with the light
            // over air (the factor never exceeds 1). One
            // swell per cycle, at [`Self::LASER_RIPPLE_OMEGA`]: under the
            // flash bound.
            if matches!(cfg.style, GlowStyle::Laser) {
                let ripple = 0.86 + 0.14 * (s.pos * 9.0 - age * Self::LASER_RIPPLE_OMEGA).sin();
                cov_f *= ripple;
                full_f *= ripple;
            }
            let cov = cov_f as u8;
            let here = (s.row, s.col);
            if cov == 0 {
                prev = Some(here); // keep continuity so the next cell isn't a false split
                continue;
            }
            let (x, y) = center(s.row, s.col);
            let sample = CometSample { x, y, cov, pos };
            let mut runs = CometRuns {
                runs: &mut self.comet_runs,
                run: &mut self.comet_run,
                typing: &mut self.comet_typing,
                run_typing: &mut self.comet_run_typing,
            };
            runs.chain(&mut runs_len, prev, here, sample, s.typing);
            prev = Some(here);
            head_full = full_f.min(255.0) as u8;
        }
        CometRuns {
            runs: &mut self.comet_runs,
            run: &mut self.comet_run,
            typing: &mut self.comet_typing,
            run_typing: &mut self.comet_run_typing,
        }
        .publish(&mut runs_len);
        if runs_len == 0 {
            return;
        }
        // The dark bridge, drawn as its own knee→caret segment after the head
        // run (see below).
        let mut bridge: Option<[CometSample; 2]> = None;
        // Whether the head run ends on the caret's own cell (the bed's
        // bridge, which is air).
        let mut caret_sample = false;
        // Connect the head run to the live cursor cell (no trailing gap), but
        // only while the cursor is adjacent to the run's head; after a line
        // change or leap a bridge would draw a stray diagonal from the old text.
        // The bridge attaches at `cfg.head_dx` of the cell (0.5 = centre), since
        // a bar cursor sits at the cell's left edge.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            // The live head run is at `runs_len - 1` (non-zero here), not
            // `last_mut()`, which could return a stale spare.
            && let Some(last) = self.comet_runs.get_mut(runs_len - 1)
            && let Some(head) = last.last().copied()
        {
            let (_, y) = center(cr, cc);
            // Window-absolute, like `center` with `head_dx` in place of 0.5;
            // without `oxf` the head lands `pad` px left of the cursor in a
            // real window and the adjacency test breaks.
            let x = oxf + (cc as f32 + cfg.head_dx) * cwf;
            if (x - head.x).abs() <= cwf * 1.5 && (y - head.y).abs() <= chf * 1.5 {
                // Phaser: the bridge is being laid now, so it leads at the live
                // sweep phase (offset 0.5, a fresh spark's hue). Other styles:
                // the head of the path ramp.
                let pos = if matches!(cfg.style, GlowStyle::Phaser) {
                    0.5
                } else {
                    1.0
                };
                let fire = matches!(cfg.style, GlowStyle::Fire);
                if bed.is_some() {
                    // The bed runs on under the caret, and the caret's cell
                    // is air the beam may light at full strength; the split
                    // below ends the beam's run over the head's letter.
                    last.push(CometSample {
                        x,
                        y,
                        cov: head_full.max(head.cov),
                        pos,
                    });
                    caret_sample = true;
                } else if cfg.dark_theme {
                    // Fire keeps full punch over the empty cursor cell: its
                    // swept samples are capped (FIRE_STREAK_COV_CAP) and spend
                    // the ledger over the glyphs, but the bridge has no glyph to
                    // bury, so the head returns to FIRE_HEAD_COV_CAP.
                    let full = if fire {
                        (head_full as f32 * Self::FIRE_HEAD_COV_CAP / Self::FIRE_STREAK_COV_CAP)
                            .min(Self::FIRE_HEAD_COV_CAP) as u8
                    } else {
                        head_full
                    };
                    // The bridge is the one sample off the glyphs, so it runs at
                    // the head's full coverage. `comet_beam` interpolates
                    // coverage along a segment (and RDP keeps only a straight
                    // run's ends), so a bridge inside the run would ramp from the
                    // budget back up to full across the glyphs and re-wash them.
                    // The run instead ends at a KNEE where the segment leaves
                    // the head's cell, at the budget, and the knee→bridge
                    // segment is drawn on its own below: the ramp to full
                    // happens over the caret cell alone. A bridge still inside
                    // the head's cell (a caret that did not leave it) is over
                    // that glyph and keeps the budget.
                    let (dx, dy) = (x - head.x, y - head.y);
                    let exit = |d: f32, half: f32| {
                        if d.abs() > 1e-3 {
                            half / d.abs()
                        } else {
                            f32::INFINITY
                        }
                    };
                    let t = exit(dx, cwf * 0.5).min(exit(dy, chf * 0.5));
                    if t < 1.0 {
                        let knee = CometSample {
                            x: head.x + dx * t,
                            y: head.y + dy * t,
                            cov: head.cov,
                            pos,
                        };
                        last.push(knee);
                        bridge = Some([
                            knee,
                            CometSample {
                                x,
                                y,
                                cov: full.max(head.cov),
                                pos,
                            },
                        ]);
                    } else {
                        last.push(CometSample {
                            x,
                            y,
                            cov: head.cov,
                            pos,
                        });
                    }
                } else {
                    // Light ground: no budget applied, the bridge is one more
                    // body sample, at the head's coverage.
                    last.push(CometSample {
                        x,
                        y,
                        cov: head.cov,
                        pos,
                    });
                }
            }
        }

        // The bridge and knee samples just pushed onto the head run carry
        // the head's own kind, sample for sample.
        if let (Some(samples), Some(kinds)) = (
            self.comet_runs.get(runs_len - 1),
            self.comet_typing.get_mut(runs_len - 1),
        ) {
            let head = kinds.last().copied().unwrap_or(true);
            kinds.resize(samples.len(), head);
        }

        // Light themes: additive light cannot brighten white, and saturating
        // add over fresh glyphs would lift their ink toward the ground. The
        // beam family draws as ink UNDER the letters instead
        // ([`Self::emit_comet_light`]), one shape per style. Only the newest
        // sample in a cell draws: an older one whose cell a newer sample
        // already claimed is zeroed, so a revisited cell (a backspace and
        // retype) never stacks two source-over bodies.
        if !cfg.dark_theme {
            if self.water_seen.len() == Self::WATER_OWNER_SLOTS {
                self.water_seen.fill(0);
            } else {
                self.water_seen.clear();
                self.water_seen.resize(Self::WATER_OWNER_SLOTS, 0);
            }
            let (oxi, oyi) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
            for run in self.comet_runs[..runs_len].iter_mut().rev() {
                for s in run.iter_mut().rev() {
                    let (col, row) = (
                        ((s.x - oxi as f32) / cwf).floor() as i32,
                        ((s.y - oyi as f32) / chf).floor() as i32,
                    );
                    if col < 0 || row < 0 || col >= geom.cols as i32 || row >= geom.rows as i32 {
                        continue;
                    }
                    if !Self::claim_water_cell(&mut self.water_seen, row as u16, col as u16) {
                        s.cov = 0;
                    }
                }
            }
            self.emit_comet_light(now, cfg, geom, runs_len, under);
            return;
        }

        // The dark bed: the swept runs lie under the letters as the style's
        // bed, and the beam is drawn only over air (and onto the caret).
        let runs_len = if let Some(shape) = bed {
            self.emit_dark_bed(
                cfg,
                geom,
                runs_len,
                shape,
                &|g: &CursorGlow, pos| g.comet_color(cfg, pos),
                under,
            );
            self.split_air_runs(geom, runs_len, caret_sample)
        } else {
            runs_len
        };
        // Phaser runs a fat core; with its tight PHASER_LAYERS halo it reads as
        // a solid bar of spectrum behind the cursor.
        let core_thick = match cfg.style {
            // Phaser spans most of the row but leaves the cap-height and
            // descender bands clear so letters on it stay legible.
            GlowStyle::Phaser => (chf * 0.66).max(8.0),
            // Fire's core swells with the eased temperature (slim vein at a
            // smolder, a rope at full blaze), thin enough not to bury glyphs.
            GlowStyle::Fire => fire_core,
            // A fat charged conductor (~2× the shared core) the strikes fork off.
            GlowStyle::Laser => (chf * 0.26).max(3.5),
            // The beam tube: a solid rod (~1/3 cell) at full power, collapsing
            // toward a hairline as it powers down (`beam_pw`).
            GlowStyle::Beam => (chf * (0.08 + 0.26 * beam_pw)).max(2.0),
            // Sparkle lays a wide ribbon (~0.45 cell, SPARKLE_LAYERS) for the
            // celestial shapes to fall out of; cap-height and descender bands
            // stay clear. While typing it is one rolling hue (every typing
            // spark sits at `pos` 1.0, and the hue steps each key); a
            // multi-cell move sweeps half the wheel tail to head.
            GlowStyle::Sparkle => (chf * 0.45).max(5.0),
            _ => (chf * 0.13).max(2.0), // crisp thin core
        };
        // Flatten the per-cell Bresenham path to the true straight move, so the
        // beam is a clean diagonal rather than a staircase of cell centres.
        let straighten = cwf.max(chf) * 0.8;

        // The layered rasterizer's working buffers, taken with `mem::take`
        // (the loop calls `self.comet_color`, a whole-`self` borrow) after
        // every early return and restored right after the loop.
        let mut beam_scratch = std::mem::take(&mut self.beam_scratch);
        let aura_from = halos.len();
        let color_at = |pos: f32| self.comet_color(cfg, pos);
        let runs = if bed.is_some() {
            &self.air_runs[..runs_len]
        } else {
            &self.comet_runs[..runs_len]
        };
        for (i, r) in runs.iter().enumerate() {
            // A one-row run (a typed run) is collinear, so straightening would
            // only drop its interior samples: the run would be drawn as one
            // chord whose coverage ramps from its fading tail to its head, and
            // each time that tail expired the next sample (at full light)
            // would become the chord's end, so every cell inside the run would
            // dim with each key and jump back as the tail went (a re-flare at
            // the typing rate, `the_typing_streak_flashes_no_pixel_more_than_three_times_a_second`).
            // Keeping every sample draws each cell at its own coverage and
            // laid hue, as the phaser (whose runs are always one row) does;
            // geometry is identical. A leap across rows still
            // straightens into a clean diagonal. Only the flash test's control
            // flag is forked, so that test reads the shipped expression.
            #[cfg(not(test))]
            let chords_rows = false;
            #[cfg(test)]
            let chords_rows = self.streak_chords_rows;
            let one_row = !chords_rows && r.iter().all(|s| s.y == r[0].y);
            let tol = if one_row || matches!(cfg.style, GlowStyle::Phaser) {
                0.0
            } else {
                straighten
            };
            // The head run's knee→caret bridge: drawn after the run on its own
            // (unsimplified), and the knee is geometry, not a sample, so it
            // lays no laser aura.
            let bridge = bridge.filter(|_| i + 1 == runs_len);
            // Over the bed, the head's bridge over the caret's own cell is
            // the caret's light: kept inside that cell, so the beam's halo
            // does not stand above and below the caret as a stub.
            let stub = (bed.is_some() && caret_sample && i + 1 == runs_len && Self::is_bridge(r))
                .then(|| cur.map(|(cr, cc)| Self::caret_box(geom, cr, cc)))
                .flatten();
            let from = out.len();
            if r.len() < 2 {
                // A lone swept cell (e.g. a single move with the cursor hidden): a
                // small soft dot, not a full blocky cell.
                let s0 = r[0];
                let s = core_thick.max(2.0) as i32;
                push_rect(
                    out,
                    geom,
                    s0.x as i32 - s / 2,
                    s0.y as i32 - s / 2,
                    s,
                    s,
                    premul_rgb(self.comet_color(cfg, s0.pos), s0.cov),
                );
                continue;
            }
            // The layered-bloom looks live once in `aterm_render`, shared with
            // the render demos: each style's stack is `streak_layers`, the same
            // one its over-ink budget was charged against.
            layered_beam_quads_with(
                &mut beam_scratch,
                out,
                geom.beam_clip(),
                r,
                core_thick,
                tol,
                &color_at,
                stack,
            );
            if let Some(cell) = stub {
                clip_quads_to(out, from, cell);
            }
            if let Some(pair) = bridge.as_ref() {
                layered_beam_quads_with(
                    &mut beam_scratch,
                    out,
                    geom.beam_clip(),
                    pair,
                    core_thick,
                    0.0,
                    &color_at,
                    stack,
                );
            }
            if matches!(cfg.style, GlowStyle::Laser) {
                // The laser's wide aura is radial halos, never per-row quad
                // slabs, which would posterize into bands and drop a detached
                // quad below the cursor. One halo per swept sample (the bridge
                // at its full punch, never the knee); neighbours merge into one
                // tube in the pure beam hue.
                let samples = &r[..r.len() - usize::from(bridge.is_some())];
                for s in samples.iter().chain(bridge.as_ref().map(|p| &p[1])) {
                    let peak = (s.cov as f32 * Self::LASER_AURA_COV).min(150.0) as u8;
                    if peak == 0 {
                        continue;
                    }
                    push_halo(
                        halos,
                        geom,
                        s.x,
                        s.y,
                        cwf * 1.35,
                        chf * 1.05,
                        premul_rgb(cfg.color, peak),
                    );
                }
            }
        }
        // Restore the scratch UNCONDITIONALLY (no exit path lies between the
        // take and here), so its capacity survives to the next frame.
        self.beam_scratch = beam_scratch;
        // Over the bed, whatever of the laser's aura reaches a letter spends
        // the over-ink account as the crown's head light does (an idle
        // account admits it whole).
        self.ink_ledger
            .admit_halos_masked(halos, aura_from, InkLedger::HEAD_FLOOR);
    }

    /// The beam family on a light ground: each style's body as ink under the
    /// letters (source-over into the under-ink stream through [`ribbon_beam`],
    /// so glyphs keep full contrast on top), held inside the swept row so the
    /// lines above and below stay clean. Each style keeps its own shape:
    ///
    /// * Comet: a wedge from a hairline tail to a full head at the caret,
    ///   held inside the caret's cell, with a dense round nucleus where the
    ///   caret meets the newest letter;
    /// * Beam: an even tube around the letters, one colour with no darker
    ///   core, fading as it powers down — never a line through them;
    /// * Laser: a hard thin line in its own hue over a faint sheath (the
    ///   bolts and sparks ink in their own emitters, and a crackle laid on
    ///   top keeps the stack under the body cap's legibility);
    /// * Phaser: the band of spectrum it sweeps through;
    /// * Sparkle: the light twin of its dark ribbon, at the beam's quiet
    ///   alpha, in the hue it rolls through;
    /// * Lumen: a quiet tracer behind the caret (its glow is its crown,
    ///   [`Self::emit_crown`]);
    /// * Fire: no body here (fire is carried by its ink flames, meteors and
    ///   embers).
    ///
    /// Every pass composites to at most [`LIGHT_BODY_ALPHA_CAP`] at any
    /// pixel, in the light body's ink ([`LightGround::body_ink`]), solved so
    /// that even opaque it keeps the theme's text at the bar over it.
    fn emit_comet_light(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        runs_len: usize,
        under: &mut Vec<GlowQuad>,
    ) {
        use aterm_render::{GlowBlend, RibbonVertex, ribbon_beam};
        if !matches!(
            cfg.style,
            GlowStyle::Comet
                | GlowStyle::Beam
                | GlowStyle::Laser
                | GlowStyle::Phaser
                | GlowStyle::Sparkle
                | GlowStyle::Lumen
        ) {
            return;
        }
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let body_cap = LIGHT_BODY_ALPHA_CAP;
        // Beam fades with the newest spark's power envelope.
        let beam_pw = if matches!(cfg.style, GlowStyle::Beam) {
            self.sparks.last().map_or(1.0, |s| {
                let age = now.saturating_duration_since(s.born).as_secs_f32();
                beam_power((age / s.life).clamp(0.0, 1.0))
            })
        } else {
            1.0
        };
        // Under the letters: the ink is solved against the theme's own text.
        let ground = self.light_ground(cfg);
        let ink_memo = std::cell::Cell::new(None::<(u32, u32)>);
        let ink = |rgb: u32| match ink_memo.get() {
            Some((prev, solved)) if prev == rgb => solved,
            _ => {
                let solved = ground.body_ink(rgb);
                ink_memo.set(Some((rgb, solved)));
                solved
            }
        };
        let clip = geom.beam_clip();
        // Eighth-cell slabs: a slab carries one coverage, so a wider one
        // would show the falloff as steps.
        let stride = (geom.cw / 8).max(1);
        let budget = under.len().saturating_add(Self::MAX_QUADS);
        let mut verts = std::mem::take(&mut self.light_verts);
        // One pass of one shape: `layer` maps a sample to its spine offset,
        // half-height (reach), alpha and colour.
        // `feather` starts a run of two or more samples from nothing half a
        // cell behind its oldest sample, so it does not begin square.
        let pass = |under: &mut Vec<GlowQuad>,
                    verts: &mut Vec<RibbonVertex>,
                    run: &[CometSample],
                    feather: bool,
                    layer: &dyn Fn(&CometSample) -> (f32, f32, f32, u32)| {
            verts.clear();
            let mut push = |x: f32, s: &CometSample| {
                let (dy, reach, alpha, color) = layer(s);
                // Inside the row: the reach never crosses the row's edges.
                let available = (chf * 0.5 - dy.abs()).max(0.0);
                let reach = reach.clamp(0.5_f32.min(available), available);
                verts.push(RibbonVertex {
                    x,
                    spine: s.y + dy,
                    up: reach,
                    dn: reach,
                    core_up: reach * 0.5,
                    core_dn: reach * 0.5,
                    color,
                    cov: alpha.clamp(0.0, body_cap),
                    lift: 0.0,
                    lift_span: 0.0,
                });
            };
            if let [lone] = run {
                // A lone swept cell: a short dash across its cell.
                push(lone.x - cwf * 0.45, lone);
                push(lone.x + cwf * 0.45, lone);
            } else {
                if feather && let [first, second, ..] = run {
                    let dir = if second.x < first.x { -1.0 } else { 1.0 };
                    push(first.x - dir * cwf * 0.5, &CometSample { cov: 0, ..*first });
                }
                for s in run {
                    push(s.x, s);
                }
            }
            ribbon_beam(under, clip, verts, 1.0, stride, budget, GlowBlend::Over);
        };
        let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        let row_of = |s: &CometSample| ((s.y - oyf) / chf).floor() as i32;
        let cell_of = |s: &CometSample| (row_of(s), ((s.x - oxf) / cwf).floor() as i32);
        // The host's newest probe of the caret's row (`'\0'` at a wide
        // glyph's continuation): which swept cells hold a wide glyph (CJK,
        // emoji). A band fills a wide glyph's whole box, gaps between its
        // strokes included, where a Latin letter keeps the paper above and
        // below its x-height, so over wide glyphs a band keeps its shape and
        // hue at a lighter opacity ([`LIGHT_WIDE_BED_KEEP`]).
        let probe = self
            .row_cur_meta
            .as_ref()
            .map(|m| (m.row, &self.row_cur))
            .or_else(|| self.row_prev_meta.as_ref().map(|m| (m.row, &self.row_prev)));
        let wide = |s: &CometSample| {
            let (row, col) = cell_of(s);
            probe.is_some_and(|(probed, chars)| {
                i32::from(probed) == row
                    && usize::try_from(col).is_ok_and(|col| {
                        chars.get(col) == Some(&'\0') || chars.get(col + 1) == Some(&'\0')
                    })
            })
        };
        let after_wide = |s: &CometSample| {
            let (row, col) = cell_of(s);
            probe.is_some_and(|(probed, chars)| {
                i32::from(probed) == row
                    && usize::try_from(col - 1).is_ok_and(|prev| chars.get(prev) == Some(&'\0'))
            })
        };
        // A band body's run in one pass (so a wide stretch joins the narrow
        // ones on either side with no seam), rising from nothing over its
        // oldest [`Self::LIGHT_BAND_TAIL_CELLS`] (a leap's tail fades out
        // instead of ending in a cut), the opacity over a wide glyph held to
        // what leaves the theme's text [`LIGHT_WIDE_BED_KEEP`] of its contrast
        // in that cell's ink.
        let (fg, bg) = (cfg.theme_fg, cfg.theme_bg);
        let banded = |under: &mut Vec<GlowQuad>,
                      verts: &mut Vec<RibbonVertex>,
                      run: &[CometSample],
                      band: &dyn Fn(&CometSample) -> (f32, f32, f32, u32)| {
            // The feather's zero knot, half a cell behind the oldest sample.
            let tail = match run {
                [first, second, ..] => {
                    let dir = if second.x < first.x { -1.0 } else { 1.0 };
                    Some((first.x - dir * cwf * 0.5, dir))
                }
                _ => None,
            };
            pass(under, verts, run, true, &|s| {
                let (dy, reach, alpha, color) = band(s);
                let alpha = tail.map_or(alpha, |(x0, dir)| {
                    let k =
                        ((s.x - x0) * dir / (cwf * Self::LIGHT_BAND_TAIL_CELLS)).clamp(0.0, 1.0);
                    alpha * k * k * (3.0 - 2.0 * k)
                });
                // The cell just past a wide glyph (the caret's, as one is
                // typed) is held too: the band's opacity ramps between its
                // samples, and the ramp up to a full caret cell crossed the
                // newest glyph's right half.
                if wide(s) || after_wide(s) {
                    let held = light_alpha_keeping(color, fg, bg, LIGHT_WIDE_BED_KEEP);
                    (dy, reach, alpha.min(held), color)
                } else {
                    (dy, reach, alpha, color)
                }
            });
        };
        for run in &self.comet_runs[..runs_len] {
            let Some(head) = run.last() else {
                continue;
            };
            // A band is ink under the letters of the row it lies in, so a run
            // that leaps rows (Enter, a jump) draws only its stretch on the
            // head's row: laid across the rows it crossed, it would stripe
            // the text there (the output an Enter just printed) for a frame.
            // The laser's strike is a hairline, so it keeps its whole path.
            let head_row = row_of(head);
            let last_off_row = run.iter().rposition(|s| row_of(s) != head_row);
            let run = match (cfg.style, last_off_row) {
                (GlowStyle::Laser, _) | (_, None) => &run[..],
                (_, Some(last_off)) => &run[last_off + 1..],
            };
            match cfg.style {
                GlowStyle::Comet => {
                    // A full head at the caret and a tail tapering to a
                    // hairline three cells behind it, its ink thinning with
                    // it. The live sparks span only the last cell or two at a
                    // typing cadence, so the tail is laid by distance from the
                    // head, in the direction the run came from, at the head's
                    // own fade.
                    let color = ink(cfg.color);
                    let head = run[run.len() - 1];
                    let from = run.len().checked_sub(2).map(|i| run[i]);
                    let back = match from {
                        Some(prev) if prev.x > head.x => 1.0,
                        _ => -1.0,
                    };
                    // The tail tapers over three and a half cells to the
                    // head under the caret, and the head is round: the same
                    // ink carries on past the caret's centre into a soft cap
                    // (the coma, in one pass with the tail, so the two never
                    // stack).
                    const KNOTS: usize = 14;
                    const CAP_KNOTS: usize = 5;
                    let tail = cwf * 3.5;
                    // The head's cell edge ahead of it: the cap ends there,
                    // under the caret, so no rim of it stands past the
                    // caret's edge as a nub.
                    let ox = f32::from(geom.origin_x);
                    let col = ((head.x - ox) / cwf).floor();
                    let (lead_edge, trail_edge) = if back < 0.0 {
                        (ox + (col + 1.0) * cwf, ox + col * cwf)
                    } else {
                        (ox + col * cwf, ox + (col + 1.0) * cwf)
                    };
                    let cap = (lead_edge - head.x).abs().min(cwf * 0.65);
                    let mut wedge = [head; KNOTS + CAP_KNOTS + 1];
                    for (k, knot) in wedge.iter_mut().enumerate() {
                        if k <= KNOTS {
                            // `k = 0` is the tail's tip, `KNOTS` the head.
                            let toward_head = k as f32 / KNOTS as f32;
                            knot.x = head.x + back * tail * (1.0 - toward_head);
                            knot.pos = toward_head;
                        } else {
                            // The cap ahead of the head, `pos` past 1.
                            let ahead = (k - KNOTS) as f32 / CAP_KNOTS as f32;
                            knot.x = head.x - back * cap * ahead;
                            knot.pos = 1.0 + ahead;
                        }
                    }
                    let q0 = under.len();
                    pass(under, &mut verts, &wedge, false, &|s| {
                        let (round, along) = if s.pos > 1.0 {
                            let a = s.pos - 1.0;
                            ((1.0 - a * a).max(0.0).sqrt(), 1.0)
                        } else {
                            (1.0, s.pos)
                        };
                        (
                            chf * 0.04,
                            chf * (0.06 + 0.40 * along) * round,
                            (f32::from(s.cov) * 4.0).min(body_cap) * along.sqrt() * round * round,
                            color,
                        )
                    });
                    // Held inside the caret's cell's leading edge and the
                    // row (the pass keeps its reach inside the row; the
                    // nucleus below is held here).
                    let lead = lead_edge.round() as i32;
                    let oy = i32::from(geom.origin_y);
                    let row_top = oy
                        + ((head.y - f32::from(geom.origin_y)) / chf).floor() as i32
                            * geom.ch as i32;
                    let hold = if back < 0.0 {
                        (i32::MIN, row_top, lead, row_top + geom.ch as i32)
                    } else {
                        (lead, row_top, i32::MAX, row_top + geom.ch as i32)
                    };
                    clip_quads_to(under, q0, hold);
                    // The nucleus: a dense round head where the caret's
                    // trailing edge meets the newest letter (the block caret
                    // covers the head's own cell), the comet's ink laid over
                    // the tail as deep as the body cap. The ink is solved so
                    // that even opaque it keeps the theme's text at the bar
                    // ([`LightGround::body_ink`]), so the nucleus is the
                    // source-over remainder that takes the tail's own alpha
                    // at the head, `t`, to the cap: `(cap − t) / (1 − t/255)`,
                    // nothing where the tail is already opaque.
                    let fresh = (f32::from(head.cov) / 255.0 * 4.0).min(1.0);
                    let t = (f32::from(head.cov) * 4.0).min(LIGHT_BODY_ALPHA_CAP);
                    let remainder = if t >= 255.0 {
                        0.0
                    } else {
                        ((LIGHT_BODY_ALPHA_CAP - t) / (1.0 - t / 255.0)).max(0.0)
                    };
                    let peak = remainder.min(LIGHT_BODY_ALPHA_CAP) * fresh;
                    if peak >= 1.0 {
                        let q1 = under.len();
                        let budget = under.len().saturating_add(Self::MAX_QUADS);
                        push_ink_ellipse(
                            under,
                            geom,
                            (trail_edge + back * cwf * 0.22, head.y + chf * 0.04),
                            (cwf * 0.62, cwf * 0.62),
                            chf * 0.36,
                            color,
                            peak,
                            budget,
                        );
                        clip_quads_to(under, q1, hold);
                    }
                    // The comet's head on paper, which neither Beam's tube nor
                    // Lumen's tracer has: a round coma around the caret,
                    // ringed in a deeper rim, as tall as the row, riding the
                    // head's freshness.
                    if fresh > 0.02 {
                        let budget = under.len().saturating_add(Self::MAX_QUADS);
                        push_ink_ring(
                            under,
                            geom,
                            (head.x, head.y),
                            chf * Self::COMET_LIGHT_COMA_R,
                            (chf * Self::COMET_LIGHT_COMA_RIM).max(1.5),
                            color,
                            (body_cap * 0.28 * fresh, body_cap * 0.92 * fresh),
                            budget,
                        );
                    }
                    // The comet's own cue on paper, which Lumen's quiet tracer
                    // has not: ice crystals glinting along its tail, small
                    // four-point stars of its ink in the row's leading, off
                    // every letter's ink: above the ascenders' tops, or below
                    // the baseline where the letter there has no descender,
                    // each twinkling on its own phase and riding the head's
                    // freshness. Flat stars (a long horizontal arm, a short
                    // vertical one), so they fit the leading.
                    let born = self.sparks.last().map_or(now, |s| s.born);
                    let age = now.saturating_duration_since(born).as_secs_f32();
                    let arm = chf * Self::COMET_LIGHT_GLINT_ARM;
                    let thin = (chf * 0.045).max(1.0);
                    let row_top = f32::from(geom.origin_y)
                        + ((head.y - f32::from(geom.origin_y)) / chf).floor() * chf;
                    let descends = |x: f32| {
                        let col = ((x - f32::from(geom.origin_x)) / cwf).floor() as i32;
                        probe.is_some_and(|(probed, chars)| {
                            i32::from(probed) == row_of(&head)
                                && usize::try_from(col).is_ok_and(|col| {
                                    chars.get(col).is_some_and(|c| {
                                        "gjpqyQ,;_|()[]{}@$".contains(*c) || !c.is_ascii()
                                    })
                                })
                        })
                    };
                    #[cfg(test)]
                    self.light_glints.clear();
                    for (k, &(behind, high)) in Self::COMET_LIGHT_GLINTS.iter().enumerate() {
                        let twinkle = twinkle_env(age, k as f32 * 2.1 + head.x * 0.013);
                        let peak = LIGHT_BODY_ALPHA_CAP * 0.85 * fresh * twinkle;
                        if peak < 1.0 {
                            continue;
                        }
                        let x = head.x + back * cwf * behind;
                        let low = !high && !descends(x);
                        let centre = (
                            x,
                            row_top
                                + chf
                                    * if low {
                                        Self::COMET_LIGHT_GLINT_LOW
                                    } else {
                                        Self::COMET_LIGHT_GLINT_HIGH
                                    },
                        );
                        let rise = chf
                            * if low {
                                Self::COMET_LIGHT_GLINT_RISE.1
                            } else {
                                Self::COMET_LIGHT_GLINT_RISE.0
                            };
                        // A wide glyph fills its whole box, its leading
                        // included, so a glint there would lie in its
                        // strokes: it gives way.
                        if wide(&CometSample {
                            x: centre.0,
                            y: head.y,
                            ..head
                        }) {
                            continue;
                        }
                        let budget = under.len().saturating_add(Self::MAX_QUADS);
                        let q2 = under.len();
                        push_ink_ellipse(
                            under,
                            geom,
                            centre,
                            (arm, arm),
                            thin,
                            color,
                            peak,
                            budget,
                        );
                        push_ink_ellipse(
                            under,
                            geom,
                            centre,
                            (thin, thin),
                            rise,
                            color,
                            peak,
                            budget,
                        );
                        clip_quads_to(under, q2, hold);
                        #[cfg(test)]
                        self.light_glints.push(centre);
                    }
                }
                GlowStyle::Lumen => {
                    // A quiet tracer behind the caret in the cursor's ink,
                    // through the x-height, fading with each cell's light.
                    let color = ink(cfg.color);
                    pass(under, &mut verts, run, true, &|s| {
                        (
                            chf * 0.04,
                            chf * 0.16,
                            (f32::from(s.cov) * 1.2).min(body_cap * 0.85),
                            color,
                        )
                    });
                }
                GlowStyle::Beam => {
                    // An even tube around the letters, like a highlighter:
                    // one colour with no darker core, fading (never thinning)
                    // as it powers down. A narrow dark band on the row's
                    // centre line would read as a strikethrough.
                    let body = ink(cfg.color);
                    banded(under, &mut verts, run, &|s| {
                        (0.0, chf * 0.40, f32::from(s.cov) * 0.55 * beam_pw, body)
                    });
                }
                GlowStyle::Laser => {
                    // A soft sheath, then the hard line. Composited they stay
                    // under about 113, so a crackle at
                    // `LIGHT_BOLT_ALPHA_CAP` laid on top keeps the stack
                    // within about 200 (4.59:1 at the worst hue on Default
                    // Light).
                    let color = ink(cfg.color);
                    pass(under, &mut verts, run, false, &|s| {
                        (
                            chf * 0.08,
                            chf * 0.20,
                            (f32::from(s.cov) * 0.12).min(28.0),
                            color,
                        )
                    });
                    pass(under, &mut verts, run, false, &|s| {
                        (
                            chf * 0.08,
                            chf * 0.07,
                            (f32::from(s.cov) * 0.45).min(96.0),
                            color,
                        )
                    });
                }
                GlowStyle::Phaser => {
                    // The spectrum band, each cell in the hue it was laid in.
                    banded(under, &mut verts, run, &|s| {
                        (
                            0.0,
                            chf * 0.40,
                            f32::from(s.cov) * 1.6,
                            ink(self.comet_color(cfg, s.pos)),
                        )
                    });
                }
                GlowStyle::Sparkle => {
                    // The light twin of its 0.45-cell dark ribbon, at the
                    // beam's quiet alpha, in the hue it rolls through.
                    banded(under, &mut verts, run, &|s| {
                        (
                            0.0,
                            chf * 0.40,
                            f32::from(s.cov) * 0.55,
                            ink(self.comet_color(cfg, s.pos)),
                        )
                    });
                }
                _ => {}
            }
        }
        self.light_verts = verts;
    }

    /// Per-pixel fire on a dark ground: each hot cell of the envelope emits
    /// [`FirePatch`]es that both backends evaluate per device pixel through
    /// the shared integer field, composited additively. The patches are cut
    /// in narrow columns ([`Self::FIRE_SUBCOLS`] a cell) whose height is the
    /// envelope smoothed across the cells ([`flame_heat`]), so a flame rises
    /// and falls off along the line instead of standing as a one-cell slab
    /// with square sides, and in short slices whose ceiling thins toward the
    /// tips, so the light dies upward instead of ending in a flat lid. Over
    /// a row the frame shows blank the ceiling is the air's, brighter than
    /// the translucent tint a line of text keeps. The per-cell engulfment,
    /// derived from the same envelope, feeds the contrast-halo strengths
    /// (`halo_cells`), a colour-free legibility stream (ink is never
    /// recoloured). On a light ground the fire is ink tongues beneath the
    /// letters instead ([`Self::emit_flames_light`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry and the five streams the fire can land in"
    )]
    pub(super) fn emit_flames(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        patches: &mut Vec<FirePatch>,
        // Unused; always left empty (see the contrast-halo note below).
        _charred: &mut Vec<CharFg>,
        halo_cells: &mut Vec<FireHaloCell>,
        engulf: &mut Vec<(u16, u16, f32)>,
        under: &mut Vec<GlowQuad>,
    ) {
        engulf.clear();
        if !matches!(cfg.style, GlowStyle::Fire) || self.sparks.is_empty() {
            return;
        }
        let ch = geom.ch as f32;
        let t = self.fire_t();
        // Legibility-first coverage ceiling. On a dark ground the additive
        // field crosses live text and lifts the ground behind it, so it stays a
        // translucent tint the glyph shows through, even at the white-hot head:
        // the drama comes from height, motion and the wall above the line, not
        // opacity over the letters.
        let cov_cap = (22.0 + 12.0 * t).min(255.0);
        // Per-cell heat envelope: the strongest spark per cell wins, newest
        // first. 12 cells at full strength plus a 4-cell fading tail, so the
        // oldest burning cell dissolves instead of vanishing in one frame.
        let mut cells: [(u16, u16, f32); 16] = [(0, 0, 0.0); 16];
        let mut n_cells = 0usize;
        for s in self.sparks.iter().rev() {
            if (s.row as usize) >= geom.rows || (s.col as usize) >= geom.cols {
                continue;
            }
            // Test cell membership before the transcendentals: a spark whose
            // cell is neither kept nor a free slot is dropped regardless of
            // strength.
            let slot = cells[..n_cells]
                .iter()
                .position(|c| c.0 == s.row && c.1 == s.col);
            if slot.is_none() && n_cells >= cells.len() {
                continue;
            }
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            let u = (age / s.life).clamp(0.0, 1.0);
            let env = if s.typing && !cfg.dark_theme {
                // On paper a flame is ink, not light, so one cell's lick is a
                // smudge: each typed cell keeps burning through the first
                // third of its life and burns down after, so the last few
                // letters carry a row of tongues falling away behind the
                // caret.
                if u < 0.30 {
                    1.0
                } else {
                    0.5 * (1.0 + (std::f32::consts::PI * (u - 0.30) / 0.70).cos())
                }
            } else if s.typing {
                const EMBER: f32 = 0.38;
                if u < 0.10 {
                    1.0
                } else if u < 0.40 {
                    let k = (u - 0.10) / 0.30;
                    EMBER + (1.0 - EMBER) * 0.5 * (1.0 + (std::f32::consts::PI * k).cos())
                } else {
                    EMBER * 0.5 * (1.0 + (std::f32::consts::PI * (u - 0.40) / 0.60).cos())
                }
            } else {
                1.0 - u
            };
            // Steep head→tail falloff: flames at the head, embers a few cells
            // back, darkness at the tail (on paper a gentler one, so the
            // tongues behind the head stand as a row).
            let strength = if cfg.dark_theme {
                env * (0.16 + 0.84 * s.pos.powf(1.4))
            } else {
                env * (0.40 + 0.60 * s.pos)
            };
            if strength < 0.02 {
                continue;
            }
            match slot {
                Some(i) => cells[i].2 = cells[i].2.max(strength),
                None => {
                    cells[n_cells] = (s.row, s.col, strength);
                    n_cells += 1;
                }
            }
        }
        if n_cells == 0 {
            return;
        }
        // Tail dissolve: the first 12 cells keep full strength; the tail
        // fades linearly so the oldest cell drops below the height cull as a
        // whisper.
        for (rank, c) in cells[..n_cells].iter_mut().enumerate().skip(12) {
            c.2 *= ((16 - rank.min(16)) as f32 / (16 - 11) as f32).min(1.0);
        }
        let cells = &cells[..n_cells];
        if !cfg.dark_theme {
            self.emit_flames_light(cfg, geom, cells, t, under);
            return;
        }
        // One quantized phase per frame: both backends see the identical time.
        let phase = (self.flame_phase * 1024.0) as u32;
        // Tongues drag behind rightward typing: lean/4 px at full rise.
        let lean_q = (-(ch * 0.30 * t) * 4.0).clamp(-127.0, 127.0) as i8;
        // Effects-box horizontal clamp: the grid's columns (`head` raises only
        // the top).
        let (fx_l, fx_r) = (geom.fx_left(), geom.fx_right());
        let chi = geom.ch as i32;
        let cwi = geom.cw as i32;
        // Root the flame in the cell's ascender band
        // ([`Self::FIRE_DARK_ROOT`]) so it rises into the inter-line space
        // and the row above, keeping the typed line's letters clear. Both clamps
        // are grid-relative, relaxed by `geom.head` (the chrome band), then
        // translated by origin, never window-clamped, so head == 0 matches
        // the pad-relative math. With head == 0 the root clamps at least
        // one cell down; with head ≥ ch row-0 flames climb into the chrome
        // band, and `fire_field::fire_top_fade` dissolves them at the edge.
        let head = i32::from(geom.head);
        let oy = geom.origin_y as i32;
        let ox = geom.origin_x as i32;
        let root_floor = (chi - head).max(2);
        // Shift the sampling window in the lean direction rather than
        // widening it both ways. The field shears internally
        // (`shear = lean·vn/4`) and is a function of absolute x, so
        // overlapping windows would add into a bright seam at every cell
        // boundary; a uniform shift makes adjacent patches tile exactly.
        let lean_px = (lean_q as i32).abs() / 4 + 1;
        let shift = if lean_q > 0 { lean_px } else { -lean_px };
        let sub = Self::FIRE_SUBCOLS;
        // Over a blank row the flame is light with no letter to bury: it
        // burns at the air's ceiling, easing down to the text's over a
        // shoulder on the air's side of a line of text, so the letters keep
        // the translucent tint and the flames above them their colour. A row
        // the frame does not show is text.
        let text = self.text_view();
        let air_cap = Self::FIRE_DARK_AIR_CAP_COLD + Self::FIRE_DARK_AIR_CAP_HEAT * t;
        let shoulder = (ch * 0.35).max(1.0);
        let smooth = |k: f32| {
            let k = k.clamp(0.0, 1.0);
            k * k * (3.0 - 2.0 * k)
        };
        let mut seen_rows = [0u16; 16];
        let mut n_rows = 0usize;
        for &(row, _, _) in cells {
            if seen_rows[..n_rows].contains(&row) {
                continue;
            }
            seen_rows[n_rows] = row;
            n_rows += 1;
            // The flame roots on the burning row's top edge and the newest
            // few typed letters each burn a tongue falling away behind the
            // head ([`Self::FIRE_DARK_AIR_RANKS`]), riding the head's own
            // strength so they die with it. Every row but the grid's first
            // burns this way, a line of text above or not: over that line's
            // letters the flame keeps the text's tint (the ceiling below), so
            // it burns bright in the line's gaps and past its end and never
            // crosses a letter at more than a tint; under open air the
            // tongues stand about a row tall with hot cores however cool
            // the fire still is.
            let air = row > 0;
            let mut burning = [(0u16, 0u16, 0.0f32); 16];
            let mut n_burning = 0usize;
            for c in cells.iter().filter(|c| c.0 == row) {
                burning[n_burning] = *c;
                n_burning += 1;
            }
            if air && n_burning > 0 {
                // The letters behind the head, back along the way the run
                // came, each raised to its rank's share of the head (a letter
                // whose spark already died still burns while the head does).
                let (head_col, head_strength) = (i32::from(burning[0].1), burning[0].2);
                let back = if n_burning > 1 && i32::from(burning[1].1) > head_col {
                    1
                } else {
                    -1
                };
                // Ahead of the head, over the caret, the flame tapers off
                // instead of standing cut at the newest letter's edge.
                let ahead = head_col - back;
                if ahead >= 0
                    && ahead < geom.cols as i32
                    && n_burning < burning.len()
                    && !burning[..n_burning].iter().any(|c| i32::from(c.1) == ahead)
                {
                    burning[n_burning] =
                        (row, ahead as u16, Self::FIRE_DARK_AIR_LEAD * head_strength);
                    n_burning += 1;
                }
                // Behind a typed head only: a jump's cells (a completion, a
                // paste) burn at their own strengths, laid along its
                // `Length`, as the knob law says a jump reads it.
                let typed_head = self
                    .sparks
                    .iter()
                    .rev()
                    .find(|s| s.row == row)
                    .is_some_and(|s| s.typing);
                let ranks = if typed_head {
                    &Self::FIRE_DARK_AIR_RANKS[..]
                } else {
                    &Self::FIRE_DARK_AIR_RANKS[..1]
                };
                for (rank, share) in ranks.iter().enumerate().skip(1) {
                    let col = head_col + back * rank as i32;
                    if col < 0 || col >= geom.cols as i32 || !text.covers_cell(i32::from(row), col)
                    {
                        break;
                    }
                    let floor = share * head_strength;
                    let col = col as u16;
                    match burning[..n_burning].iter().position(|c| c.1 == col) {
                        Some(i) => burning[i].2 = burning[i].2.max(floor),
                        None if n_burning < burning.len() => {
                            burning[n_burning] = (row, col, floor);
                            n_burning += 1;
                        }
                        None => {}
                    }
                }
            }
            let burning = &burning[..n_burning];
            let on_row = || burning.iter();
            let (cmin, cmax) = on_row().fold((i32::MAX, i32::MIN), |(lo, hi), c| {
                (lo.min(i32::from(c.1)), hi.max(i32::from(c.1)))
            });
            // Under open air (a blank row above) a cool fire still stands a
            // row tall with hot cores; under a line of text the tongues keep
            // the fire's own heat, so a sustained blaze still reads hotter
            // than a cold start there.
            let open_air = air && text.row_is_blank(i32::from(row) - 1);
            let (row_t, peak_floor) = if open_air {
                (t.max(Self::FIRE_DARK_AIR_TEMP), Self::FIRE_DARK_AIR_PEAK)
            } else {
                (t, 0.0)
            };
            let row_temp = (row_t * 255.0) as u8;
            let air_cap = if air {
                air_cap.max(Self::FIRE_DARK_AIR_TONGUE_CAP)
            } else {
                air_cap
            };
            // Over a blank row the flame roots on the burning row's top edge
            // and burns wholly in the air above it, its foot fading in over
            // [`Self::FIRE_DARK_AIR_FOOT`] of a row: a root inside the
            // letters' ascender band stood a dim slab over them, and the
            // ceiling easing down to it a dark seam above it.
            let base_y = if air {
                oy + (row as f32 * ch) as i32
            } else {
                oy + ((row as f32 * ch + (ch * Self::FIRE_DARK_ROOT).max(2.0)) as i32)
                    .max(root_floor)
            };
            let foot = (ch * Self::FIRE_DARK_AIR_FOOT).max(1.0);
            // Over a blank row the tongues are not a comb: each cell's own
            // height, from its hash, eased across the cells.
            let hash01 = |c: i32| {
                let h = aterm_render::fire_field::fire_hash(c as u32, u32::from(row), 0x7A3C_91E5);
                (h >> 8) as f32 / (1u32 << 24) as f32
            };
            let mut tongue_geom = [(0.0f32, 0.0f32, 0.0f32); 16];
            if air {
                for (slot, &(_, c, s)) in tongue_geom[..burning.len()].iter_mut().zip(burning) {
                    let half = Self::FIRE_DARK_TONGUE_HALF_W * (0.8 + 0.2 * s);
                    let size = 1.0 - Self::FIRE_DARK_AIR_SIZES * hash01(i32::from(c));
                    *slot = (f32::from(c) + 0.5, half, s * size);
                }
            }
            let tongue_geom = &tongue_geom[..burning.len()];
            // The tallest tongue over column position `u` (cells) and how
            // far inside it `u` stands (1 on its axis, 0 at its side).
            let teardrop = |u: f32| -> (f32, f32) {
                let mut best = (0.0f32, 0.0f32);
                for &(c_mid, half, s_size) in tongue_geom {
                    let d = ((u - c_mid) / half).abs();
                    if d >= 1.0 {
                        continue;
                    }
                    let h = s_size * (1.0 - d).powf(0.75);
                    if h > best.0 {
                        best = (h, 1.0 - d);
                    }
                }
                best
            };
            let row_rise_scale = ch * (0.16 + 5.80 * t * t).max(peak_floor);
            for col in (cmin - 1).max(0)..=(cmax + 1).min(geom.cols as i32 - 1) {
                for k in 0..sub {
                    // The envelope at this column: the burning cells'
                    // strengths smoothed along the line.
                    let u = col as f32 + (k as f32 + 0.5) / sub as f32;
                    // Height ramps quadratically in the eased temperature: a
                    // lone peck is a sixth-of-a-cell lick, sustained key-repeat
                    // cadence (inter-key ≤ HEAT_GAP_FULL) builds toward a
                    // ~6-cell wall. cov_cap keeps even the wall translucent
                    // over text.
                    // Over a blank row each burning letter raises its own
                    // tongue, a teardrop: tallest over the letter's middle,
                    // falling to a point at its tip and away to nothing at
                    // its sides, each a little shorter or taller by its
                    // cell's hash. Neighbours merge at their feet and part
                    // above them, so the run reads as flames, not as bars of
                    // a meter on one bright base.
                    let (tongue, side) = if air {
                        teardrop(u)
                    } else {
                        (flame_heat(on_row(), u), 1.0)
                    };
                    let rise_px = row_rise_scale * tongue;
                    if rise_px < 1.5 {
                        continue;
                    }
                    // A tongue's foot curls up from its sides, so the line
                    // of feet is scalloped rather than one flat base.
                    #[cfg(not(test))]
                    let curl = Self::FIRE_DARK_FOOT_CURL;
                    #[cfg(test)]
                    let curl = if self.fire_flat_feet {
                        0.0
                    } else {
                        Self::FIRE_DARK_FOOT_CURL
                    };
                    let base_y = base_y - (ch * curl * (1.0 - side)) as i32;
                    let scallop = if air { smooth(side / 0.5) } else { 1.0 };
                    // Rise ceiling: the grid top relaxed by the head band — at
                    // head 0 this is the grid top exactly (the historical clamp).
                    let top_y = (base_y - rise_px as i32 - 2).max(oy - head);
                    if top_y >= base_y {
                        continue;
                    }
                    let left = ox + col * cwi + shift;
                    let x0 = (left + k * cwi / sub).max(fx_l);
                    let x1 = (left + (k + 1) * cwi / sub).min(fx_r);
                    if x1 <= x0 {
                        continue;
                    }
                    let strength = if air { flame_heat(on_row(), u) } else { tongue };
                    // The flame ends at its root: the field's root lift fades
                    // it in over a few pixels inside the burning row's
                    // ascender band, so no unlit line stands between the
                    // letters and the flame (a mirrored skirt below the root
                    // left one at the root itself).
                    let flame_end = base_y.min(geom.fx_bot());
                    // The cells the patch's pixels lie over (the lean shifts
                    // it off its sampling column).
                    let (ca, cb) = ((x0 - ox).div_euclid(cwi), (x1 - 1 - ox).div_euclid(cwi));
                    let on_text =
                        |b: i32| b >= 0 && (text.over_text(b, ca) || text.over_text(b, cb));
                    // The slice ceiling at window y `mid` of band `band`: the
                    // text's tint over a line of text, the air's over a blank
                    // row, eased between them over a shoulder.
                    let ceiling_at = |band: i32, mid: f32| {
                        // Over a blank row the foot fades in from nothing at
                        // the root (the burning row's top edge).
                        let foot_in = if air {
                            smooth((base_y as f32 - mid) / foot)
                        } else {
                            1.0
                        };
                        if on_text(band) {
                            cov_cap * foot_in
                        } else {
                            let top = (oy + band * chi) as f32;
                            let mut k = 1.0f32;
                            // The burning row under a flame rooted on its
                            // top edge takes none of it: no shoulder there.
                            if band + 1 < geom.rows as i32
                                && !(air && band + 1 == i32::from(row))
                                && on_text(band + 1)
                            {
                                k = k.min((top + ch - mid) / shoulder);
                            }
                            if on_text(band - 1) {
                                k = k.min((mid - top) / shoulder);
                            }
                            (cov_cap + (air_cap - cov_cap) * smooth(k)) * foot_in
                        }
                    };
                    let x0_u = x0 as u16;
                    let w_u = (x1 - x0) as u16;
                    let base_y_u = base_y as u16;
                    let peak_h = (rise_px as u16).max(1);
                    let cell_h = geom.ch as u16;
                    let rise_denom = rise_px.max(1.0);
                    let col_u = col as u16;
                    let mut yy = top_y;
                    while yy < flame_end {
                        if patches.len() >= Self::MAX_QUADS {
                            return;
                        }
                        // Split per row band (the dirty gate stays exact) and
                        // per slice; every piece shares base_y/phase/params, so
                        // the field is seamless and only its ceiling steps,
                        // finely, toward the tips. Where the ceiling eases
                        // between the air's and the text's the slices are a
                        // pixel, so the ease never shows as stairs.
                        let band = (yy - oy).div_euclid(chi);
                        let band_end = (oy + (band + 1) * chi).min(flame_end);
                        let coarse = (yy + Self::FIRE_SLICE_PX).min(band_end);
                        let c_start = ceiling_at(band, yy as f32 + 0.5);
                        let steep = coarse > yy + 1
                            && (c_start - ceiling_at(band, coarse as f32 - 0.5)).abs() > 3.0;
                        let end = if steep { yy + 1 } else { coarse };
                        if band < geom.rows as i32 {
                            let mid = 0.5 * (yy + end) as f32;
                            let rise = ((base_y as f32 - mid) / rise_denom).clamp(0.0, 1.0);
                            let ceiling = if end == yy + 1 {
                                c_start
                            } else {
                                ceiling_at(band, mid)
                            };
                            patches.push(FirePatch {
                                row: band.max(0) as u16,
                                x: x0_u,
                                y: yy as u16,
                                w: w_u,
                                h: (end - yy) as u16,
                                base_y: base_y_u,
                                peak_h,
                                phase,
                                temp: row_temp,
                                strength: 255,
                                lean: lean_q,
                                cov_cap: (ceiling * flame_tip_fade(rise) * scallop) as u8,
                                cell_h,
                                mode: FireMode::Add,
                            });
                            // Engulfment: how much of this band the flame body
                            // covers (height share × envelope × density), a
                            // quarter column's share of its cell. Only for
                            // `band >= 0`: above-grid bands have no glyph, and
                            // a negative band must never reach `band as u16`.
                            if band >= 0 {
                                let frac = (end - yy) as f32 / ch;
                                let rise_frac = 1.0
                                    - ((base_y - end) as f32 / rise_denom).clamp(0.0, 1.0) * 0.55;
                                let wgt =
                                    strength * frac * rise_frac * (0.35 + 0.55 * t) / sub as f32;
                                let key = (band as u16, col_u);
                                match engulf.iter_mut().find(|e| (e.0, e.1) == key) {
                                    Some(e) => e.2 += wgt,
                                    None => engulf.push((key.0, key.1, wgt)),
                                }
                            }
                        }
                        yy = end;
                    }
                }
            }
        }
        // No char, no veil: ink never changes colour (a letter that dims and
        // pops back reads as lag). The engulfment instead feeds the contrast
        // halo, a colour-free strength per engulfed cell driving the dark
        // dilation ring the renderers stamp around glyph strokes (over the
        // flame, under the ink), so letters separate from a bright blaze. It
        // applies on every row the flame covers, around letters lighter than
        // their ground: around a dark letter on a bright ground (a prompt's
        // coloured segment) a dark ring would only smear the stroke.
        let luma_table = crate::color_math::relative_luminance_table();
        for &(er, ec, e) in engulf.iter() {
            // Smooth ramp, not a hard gate: a hard cutoff would snap the ring on
            // and flicker it as the envelope breathes around the threshold. A
            // smoothstep over a low toe fades it in from zero.
            const HALO_TOE: f32 = 0.05;
            let s = ((e - HALO_TOE) / (1.5 - HALO_TOE)).clamp(0.0, 1.0);
            let s = s * s * (3.0 - 2.0 * s);
            let strength = (s * 255.0) as u8;
            if strength == 0 {
                continue;
            }
            if let Some((Some(ink), ground)) = self.frame_text.pair(i32::from(er), i32::from(ec))
                && crate::color_math::relative_luminance_with(luma_table, ink)
                    <= crate::color_math::relative_luminance_with(luma_table, ground)
            {
                continue;
            }
            halo_cells.push(FireHaloCell {
                row: er,
                col: ec,
                strength,
            });
        }
        // The renderer's FireHaloCell invariant: per-row col-sorted, unique
        // cells. Accumulation merges duplicates; sort here.
        halo_cells.sort_unstable_by_key(|c| (c.row, c.col));
    }

    /// THE INK-FIRE ON A LIGHT GROUND: additive light cannot show on paper,
    /// so the fire is ink beneath the letters, source-over in the under-ink
    /// stream: a row of separate tongues rising from the burning cells' own
    /// row, each rooted at the foot of the letters' x-height in a warm
    /// orange and standing up out of the line, going amber and yellow as it
    /// climbs and melting to nothing at its tip, feathered at its sides,
    /// leaning back against the typing and flickering slowly in height. The
    /// row's heat is smoothed across its cells ([`flame_kernel`]), so the
    /// flames rise and fall off along the line with no square end. A
    /// temperature makes them taller, never more than about three rows.
    /// Over every letter they reach the paint is held to the
    /// opacity at which that letter keeps 4.5:1 (or its own contrast, if
    /// less) in the colours its cell is drawn in ([`Self::flame_light_cap`]),
    /// eased across cell edges so a held letter is a soft thinning, never a
    /// box.
    fn emit_flames_light(
        &self,
        cfg: &GlowConfig,
        geom: Geom,
        cells: &[(u16, u16, f32)],
        t: f32,
        under: &mut Vec<GlowQuad>,
    ) {
        const MAX_TONGUES: usize = 64;
        let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
        let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        let text = self.text_view();
        // Every ramp stop, interpolated tongue colour and weighted mixture
        // lies in this channel envelope. A cell may have a dark ground and
        // light ink even when the page is light.
        let paints = flame_light_paint_bounds();
        let tall = chf * (Self::FIRE_LIGHT_RISE_COLD + Self::FIRE_LIGHT_RISE_HOT * t);
        let spacing = Self::FIRE_LIGHT_SPACING;
        // The tongues lean back against rightward typing as they rise.
        let lean = -cwf * 0.35 * t;
        let phase = self.flame_phase;
        let hash01 = |i: i32, salt: u32| {
            let h = aterm_render::fire_field::fire_hash(i as u32, salt, 0x51F0_A3B7);
            (h >> 8) as f32 / (1u32 << 24) as f32
        };
        let mut seen_rows = [0u16; 16];
        let mut n_rows = 0usize;
        let mut row_cells = [(0u16, 0u16, 0.0f32); Self::MAX_SPARKS];
        for &(row, _, _) in cells {
            if seen_rows[..n_rows].contains(&row) {
                continue;
            }
            seen_rows[n_rows] = row;
            n_rows += 1;
            let mut n_row_cells = 0usize;
            let mut cmin = i32::MAX;
            let mut cmax = i32::MIN;
            for &c in cells {
                if c.0 == row && n_row_cells < row_cells.len() {
                    cmin = cmin.min(i32::from(c.1));
                    cmax = cmax.max(i32::from(c.1));
                    row_cells[n_row_cells] = c;
                    n_row_cells += 1;
                }
            }
            let row_slice = &row_cells[..n_row_cells];
            let envelope = |u: f32| flame_heat(row_slice.iter(), u);
            let root = oyf + (f32::from(row) + Self::FIRE_LIGHT_ROOT) * chf;
            // The tongues: anchored to the grid (index `i` sits at
            // `i · spacing` cells, jittered by its own hash), so a growing
            // run adds tongues and never slides the old ones.
            let (u_lo, u_hi) = (cmin as f32 - 0.8, cmax as f32 + 1.8);
            let mut tongues = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); MAX_TONGUES];
            let mut n = 0usize;
            let first = (u_lo / spacing).floor() as i32;
            let last = (u_hi / spacing).ceil() as i32;
            for i in first..=last {
                if n == MAX_TONGUES {
                    break;
                }
                let u = (i as f32 + 0.5 + (hash01(i, 1) - 0.5) * 0.5) * spacing;
                let e = envelope(u);
                if e < 0.03 {
                    continue;
                }
                // A slow flicker in height, its own rate and phase per
                // tongue, well under the flash bound.
                let rate = 0.45 + 0.35 * hash01(i, 2);
                let flick =
                    0.5 + 0.5 * (std::f32::consts::TAU * (phase * rate + hash01(i, 3))).sin();
                let h = tall * e * (0.70 + 0.30 * hash01(i, 4)) * (0.72 + 0.28 * flick);
                let half = cwf * Self::FIRE_LIGHT_HALF_W * (0.75 + 0.25 * e);
                // A tongue never climbs into another line's letters: under
                // a line of text above it, it stands only as tall as reaches
                // that line's foot (a shorter tongue, but always one).
                let h = h.min(Self::flame_light_headroom(
                    &text,
                    row,
                    (u, half / cwf),
                    root,
                    (chf, oyf),
                ));
                if h < 2.0 {
                    continue;
                }
                tongues[n] = (oxf + u * cwf, h, half, e);
                n += 1;
            }
            let tongues = &tongues[..n];
            if tongues.is_empty() {
                continue;
            }
            let peak_h = tongues.iter().map(|tg| tg.1).fold(0.0f32, f32::max);
            // Each cell's opacity ceiling for the rows the flames reach,
            // from the root's row up.
            let rows_up = ((peak_h - (root - (oyf + f32::from(row) * chf))) / chf)
                .ceil()
                .max(0.0) as i32;
            let (c_lo, c_hi) = (u_lo.floor() as i32 - 1, u_hi.ceil() as i32 + 1);
            let caps = |r: i32, c: i32| -> f32 {
                if r < 0 || c < 0 || r >= geom.rows as i32 || c >= geom.cols as i32 {
                    return Self::FIRE_LIGHT_ALPHA;
                }
                self.flame_light_cap(cfg, &text, r, c, paints)
            };
            let mut grid = [[0.0f32; 40]; 4];
            let span = ((c_hi - c_lo + 1) as usize).min(40);
            for (dr, line) in grid
                .iter_mut()
                .enumerate()
                .take((rows_up as usize + 1).min(4))
            {
                for (dc, slot) in line.iter_mut().enumerate().take(span) {
                    *slot = caps(i32::from(row) - dr as i32, c_lo + dc as i32);
                }
            }
            let cap_cell = |x: f32, y: f32| {
                let r = ((y - oyf) / chf).floor() as i32;
                let c = ((x - oxf) / cwf).floor() as i32;
                let dr = i32::from(row) - r;
                let dc = c - c_lo;
                if (0..4).contains(&dr) && dc >= 0 && (dc as usize) < span {
                    grid[dr as usize][dc as usize]
                } else {
                    caps(r, c)
                }
            };
            // Eased across cell edges: the least ceiling within a third of
            // a cell across and a few pixels up and down.
            let ease_x = cwf * 0.34;
            let cap_at = |x: f32, y: f32| {
                cap_cell(x - ease_x, y)
                    .min(cap_cell(x + ease_x, y))
                    .min(cap_cell(x, y - 3.0))
                    .min(cap_cell(x, y + 3.0))
            };
            let x_lo = (oxf + u_lo * cwf).max(geom.fx_left() as f32);
            let x_hi = (oxf + u_hi * cwf).min(geom.fx_right() as f32);
            let knot = Self::FIRE_LIGHT_KNOT_PX;
            let knots = (((x_hi - x_lo) / knot).ceil() as usize).max(1);
            let slab = Self::FIRE_LIGHT_SLAB_PX;
            let mut slab_tongues = [(0.0f32, 0.0f32, 0.0f32, 0.0f32, 0u32); MAX_TONGUES];
            let mut y = (root - peak_h).floor().max(geom.fx_top() as f32);
            while y < root {
                if under.len() >= Self::MAX_QUADS {
                    return;
                }
                let h = slab.min(root - y);
                let ym = y + 0.5 * h;
                let mut n_slab = 0usize;
                for &(xc, th, half, _) in tongues {
                    let rise = (root - ym) / th;
                    if !(0.0..1.0).contains(&rise) {
                        continue;
                    }
                    let xc = xc + lean * rise * rise;
                    // A teardrop: a little narrower at the root, widest a fifth
                    // of the way up, tapering to a point.
                    let base = {
                        let k = (rise / 0.2).clamp(0.0, 1.0);
                        0.62 + 0.38 * k * k * (3.0 - 2.0 * k)
                    };
                    let hw = half * base * (1.0 - rise).powf(0.7);
                    if hw < 0.3 {
                        continue;
                    }
                    let root_in = {
                        let k = (rise / 0.14).clamp(0.0, 1.0);
                        k * k * (3.0 - 2.0 * k)
                    };
                    let tip = (1.0 - rise).powf(0.55);
                    slab_tongues[n_slab] = (xc, hw, root_in, tip, flame_light_ramp(rise));
                    n_slab += 1;
                }
                let active_tongues = &slab_tongues[..n_slab];
                if active_tongues.is_empty() {
                    y += h;
                    continue;
                }
                // The union of the tongues at a knot: the most opaque, in
                // the colour of the paint there weighted by its opacity, under
                // the cell's ceiling.
                let at = |x: f32| -> (u8, u32) {
                    let (mut a, mut sum, mut rgb) = (0.0f32, 0.0f32, [0.0f32; 3]);
                    for &(xc, hw, root_in, tip, c) in active_tongues {
                        let d = ((x - xc) / hw).abs();
                        if d >= 1.0 {
                            continue;
                        }
                        // Feathered sides: the paint thins over the tongue's
                        // outer half to nothing at its edge.
                        let side = (1.0 - d * d).powf(1.6);
                        let ta = Self::FIRE_LIGHT_ALPHA * side * root_in * tip;
                        if ta <= 0.0 {
                            continue;
                        }
                        a = a.max(ta);
                        sum += ta;
                        for (k, v) in rgb.iter_mut().enumerate() {
                            *v += ta * ((c >> (16 - 8 * k)) & 0xff) as f32;
                        }
                    }
                    if a < 1.0 {
                        return (0, 0);
                    }
                    let a = a.min(cap_at(x, ym));
                    if a < 1.0 {
                        return (0, 0);
                    }
                    let ch = |k: usize| ((rgb[k] / sum).round() as u32).min(255);
                    ((a as u8).max(1), (ch(0) << 16) | (ch(1) << 8) | ch(2))
                };
                let mut prev = (x_lo, at(x_lo));
                for k in 1..=knots {
                    let x = (x_lo + k as f32 * knot).min(x_hi);
                    let here = at(x);
                    let (x0, (a0, c0)) = prev;
                    let (a1, c1) = here;
                    if a0 > 1 || a1 > 1 {
                        let (xi0, xi1) = (x0.round() as i32, x.round() as i32);
                        // A ramp to nothing ends in the colour it ramps from.
                        let (c0, c1) = (if a0 > 0 { c0 } else { c1 }, if a1 > 0 { c1 } else { c0 });
                        crate::effect_util::push_fx_ramp(
                            under,
                            geom,
                            xi0,
                            y as i32,
                            xi1 - xi0,
                            (y + h) as i32 - y as i32,
                            (premul_rgb(c0, a0.max(1)), a0.max(1)),
                            (premul_rgb(c1, a1.max(1)), a1.max(1)),
                        );
                    }
                    prev = (x, here);
                }
                y += h;
            }
        }
    }

    /// How tall a light tongue over column position `u` (cells) of half-width
    /// `half` (cells), rooted at window y `root` on `row`, may stand: up to
    /// the foot of the nearest line of text above its row over its span,
    /// less [`Self::FIRE_LIGHT_HEADROOM`] (its tip feathers out before the
    /// letters); unbounded under air.
    fn flame_light_headroom(
        text: &TextView<'_>,
        row: u16,
        (u, half): (f32, f32),
        root: f32,
        (chf, oyf): (f32, f32),
    ) -> f32 {
        let (c0, c1) = ((u - half).floor() as i32, (u + half).ceil() as i32 - 1);
        for r in (0..i32::from(row)).rev().take(4) {
            if (c0..=c1).any(|c| text.covers_cell(r, c)) {
                let foot = oyf + (r + 1) as f32 * chf;
                return (root - foot - Self::FIRE_LIGHT_HEADROOM * chf).max(0.0);
            }
        }
        f32::INFINITY
    }

    /// The most opacity any light ink-fire paint in `paints` may lay over
    /// cell `(row, col)`: over a letter (the probe's lines and the frame's
    /// cells, [`TextView`]), the opacity at which the letter keeps 4.5:1, or
    /// the contrast it has on its own ground if that is less, in the ink and
    /// on the ground its cell is drawn in (the theme's pair for a line the
    /// frame did not hand over); over a row nobody read, the least of that
    /// for the theme's ink and for a coloured letter held to 3:1
    /// ([`Self::FIRE_LIGHT_REF_RED`]), since earlier output may stand there;
    /// over air, [`Self::FIRE_LIGHT_ALPHA`].
    fn flame_light_cap(
        &self,
        cfg: &GlowConfig,
        text: &TextView<'_>,
        row: i32,
        col: i32,
        paints: (u32, u32),
    ) -> f32 {
        let (fg, bg) = (cfg.theme_fg & 0x00FF_FFFF, cfg.theme_bg & 0x00FF_FFFF);
        if (text.knows_row(row) && !text.covers_cell(row, col)) || self.light_body_unsolved() {
            // Air; or a test's control, every ceiling off.
            return Self::FIRE_LIGHT_ALPHA;
        }
        let table = crate::color_math::relative_luminance_table();
        let hold = |ink: u32, ground: u32, bar: f32| -> f32 {
            let ink_luma = crate::color_math::relative_luminance_with(table, ink);
            let ground_luma = crate::color_math::relative_luminance_with(table, ground);
            let orig_ratio =
                (ink_luma.max(ground_luma) + 0.05) / (ink_luma.min(ground_luma) + 0.05);
            let bar = bar.min(orig_ratio);
            let dark_ink = ink_luma <= ground_luma;
            let paint_end = if dark_ink { paints.0 } else { paints.1 };
            // At every opacity up to `a`, each channel stays between its
            // original ground and the envelope's end at `a`. The interval
            // only expands as `a` grows, unlike contrast across a colour
            // that can pass the ink and become contrasting again.
            let holds = |a: f32| {
                if a < 1.0 {
                    return true;
                }
                let end_rgb = aterm_render::over_rgb(ground, paint_end, a as u8);
                // Four byte levels cover premultiplication, a clipped ramp,
                // the pixel-centre ramp and the final integer blend. Keep
                // that rounding room on the unsafe side of each channel.
                let extreme = if dark_ink {
                    chan_min(ground, end_rgb)
                } else {
                    chan_max(ground, end_rgb)
                };
                let bound = |sh: u32| {
                    let c = (extreme >> sh) & 0xff;
                    if dark_ink {
                        c.saturating_sub(4)
                    } else {
                        (c + 4).min(255)
                    }
                };
                let rgb = (bound(16) << 16) | (bound(8) << 8) | bound(0);
                let luma = crate::color_math::relative_luminance_with(table, rgb);
                if dark_ink {
                    luma + 0.05 >= bar * (ink_luma + 0.05)
                } else {
                    ink_luma + 0.05 >= bar * (luma + 0.05)
                }
            };
            if holds(Self::FIRE_LIGHT_ALPHA) {
                return Self::FIRE_LIGHT_ALPHA;
            }
            let (mut lo, mut hi) = (0.0f32, Self::FIRE_LIGHT_ALPHA);
            for _ in 0..8 {
                let mid = 0.5 * (lo + hi);
                if holds(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        if !text.knows_row(row) {
            return hold(fg, bg, 4.5).min(hold(Self::FIRE_LIGHT_REF_RED, bg, 3.0));
        }
        match self.frame_text.pair(row, col) {
            Some((Some(ink), ground)) => hold(ink, ground, 4.5),
            Some((None, ground)) => hold(fg, ground, 4.5),
            None => hold(fg, bg, 4.5),
        }
    }

    /// Claim one water cell for this frame's newest-visible-owner pass. Work is
    /// bounded by [`Self::MAX_SPARKS`], not the grid; `0` is free because a
    /// coordinate is stored as `key + 1`.
    #[inline]
    pub(super) fn claim_water_cell(seen: &mut [u64], row: u16, col: u16) -> bool {
        debug_assert!(seen.len().is_power_of_two());
        let encoded = (u32::from(row) << 16) | u32::from(col);
        let key = u64::from(encoded) + 1;
        let mask = seen.len() - 1;
        let mut slot = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) as usize) & mask;
        loop {
            match seen[slot] {
                0 => {
                    seen[slot] = key;
                    return true;
                }
                owner if owner == key => return false,
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// Emit water's own fluid wake (not the shared straight comet): live path
    /// samples form a curved two-layer wave, a dim deep-blue undertow under a
    /// thin cyan crest. A repaint-heavy TUI may revisit one cell many times, so
    /// only its newest visible owner is emitted. Jump reflections turn into
    /// sparse falling beads as they fade; the mark is under glyph ink. On a
    /// light ground the same wave is ink beneath the letters
    /// ([`Self::emit_water_light`]).
    pub(super) fn emit_water(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
    ) {
        // Render scratch describes this frame even when the pool just reached
        // idle zero.
        self.water_samples.clear();
        if !matches!(cfg.style, GlowStyle::Water) || self.sparks.is_empty() {
            return;
        }
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let heat = self.heat.clamp(0.0, 1.0);
        let swell_rate = 6.0 + 6.0 * heat;
        let swell_amp = ch * (Self::WATER_SWELL.0 + Self::WATER_SWELL.1 * heat);

        if self.water_seen.len() == Self::WATER_OWNER_SLOTS {
            self.water_seen.fill(0);
        } else {
            self.water_seen.clear();
            self.water_seen.resize(Self::WATER_OWNER_SLOTS, 0);
        }

        // Newest first: the first visible sample to claim a coordinate owns it.
        // Claim only after coverage is known, so a sample rounded dark does not
        // hide a visible older owner.
        let mut head: Option<(u8, f32)> = None;
        for s in self.sparks.iter().rev() {
            if (s.row as usize) >= geom.rows || (s.col as usize) >= geom.cols {
                continue;
            }
            // The cursor bridge below owns the live caret cell; excluding stale
            // path owners prevents an A↔B repaint loop closing a bright chord.
            if cur == Some((s.row, s.col)) {
                continue;
            }
            let age = now.saturating_duration_since(s.born).as_secs_f32();
            let u = (age / s.life).clamp(0.0, 1.0);
            // Ease the old tail away faster than the foam at the head.
            let env = (1.0 - u) * (1.0 - 0.45 * u);
            let base_cov =
                (f32::from(s.born_cov) * env * cfg.intensity * 0.85).min(Self::WATER_WAKE_COV_CAP);

            // A stable per-cell deal turns about a third of a jump's old crest
            // into falling beads, derived from age (no stored particles, RNG or
            // idle work). The stagger keeps a jump from changing phase at once.
            let mut seed = (u32::from(s.row) << 16) | u32::from(s.col);
            seed ^= seed >> 16;
            seed = seed.wrapping_mul(0x7FEB_352D);
            seed ^= seed >> 15;
            seed = seed.wrapping_mul(0x846C_A68B);
            seed ^= seed >> 16;
            let jitter = f32::from((seed >> 8) as u8) / 255.0;
            let drip_start = 0.20 + 0.18 * jitter;
            let drip_t = ((u - drip_start) / (1.0 - drip_start)).clamp(0.0, 1.0);
            let transfer = if s.typing {
                0.0
            } else {
                smoothstep01((u - drip_start) / 0.28)
            };
            let surface_cov = (base_cov * (1.0 - 0.88 * transfer)) as u8;
            let drip_on = !s.typing && seed.is_multiple_of(3);
            let drip_cov = if drip_on && drip_t > 0.0 {
                let arrive = smoothstep01(drip_t / 0.16);
                (base_cov * 0.54 * arrive * (1.0 - 0.20 * drip_t)) as u8
            } else {
                0
            };
            if surface_cov == 0 && drip_cov == 0 {
                continue;
            }
            if !Self::claim_water_cell(&mut self.water_seen, s.row, s.col) {
                continue;
            }
            // Rolling swell: visibly undulating even at rest, churning into surf
            // with heat.
            let phase = s.col as f32 * 0.82 + s.row as f32 * 0.41 - age * swell_rate;
            let amp = swell_amp;
            let wave = phase.sin() * amp;
            let (sx, sy) = geom.cell_center(s.row, s.col);
            let line_y = sy + ch * Self::WATER_LINE_DY;
            let drift = (f32::from((seed >> 24) as u8) / 255.0 - 0.5) * cw * 0.36;
            let sample = WaterSample {
                surface: BeamVertex {
                    x: sx,
                    y: line_y + wave,
                    color: Self::water_crest_color(s.pos, heat),
                    cov: surface_cov,
                },
                phase,
                amp,
                line_y,
                row: s.row,
                col: s.col,
                drip_cov,
                drip_x: sx + drift,
                // Gravity is quadratic: the reflection first beads at the
                // surface, then visibly accelerates down through later rows.
                drip_y: sy + wave + ch * (0.08 + 2.15 * drip_t * drip_t),
                drip_h: ch * (0.08 + 0.30 * drip_t),
            };
            if head.is_none() && surface_cov > 0 {
                head = Some((surface_cov, age));
            }
            self.water_samples.push(sample);
        }
        // Restore path order for adjacent-segment construction. Ownership stays
        // newest-wins because claims were decided before this reversal.
        self.water_samples.reverse();

        // Attach the newest live surface to the cursor at the head's coverage,
        // so the last segment is continuous without a bright knot.
        if let (Some((row, col)), Some((cov, age))) = (cur, head)
            && (row as usize) < geom.rows
            && (col as usize) < geom.cols
        {
            let phase = col as f32 * 0.82 + row as f32 * 0.41 - age * swell_rate;
            let amp = swell_amp;
            let (hx, hy) = geom.cell_center(row, col);
            let line_y = hy + ch * Self::WATER_LINE_DY;
            self.water_samples.push(WaterSample {
                surface: BeamVertex {
                    x: hx,
                    y: line_y + phase.sin() * amp,
                    color: water_ramp(0.80 + 0.12 * heat),
                    cov,
                },
                phase,
                amp,
                line_y,
                row,
                col,
                drip_cov: 0,
                drip_x: hx,
                drip_y: hy,
                drip_h: 0.0,
            });
        }

        // A heavy liquid body: a massive undertow and a fuller crest, both
        // scaling with cell height; the leaner crest and under-ink composition
        // avoid a cyan-white slab over repainting prompts.
        let under_thickness = ch * (0.28 + 0.13 * heat);
        let crest_thickness = ch * (0.10 + 0.09 * heat);
        if !cfg.dark_theme {
            self.emit_water_light(cfg, geom, (under_thickness, crest_thickness), out);
            return;
        }
        // The wake lies under the letters: the over-ink account charges the
        // lights laid after it against the wake's own light on each cell, the
        // crest over the undertow on the page, as they add.
        let page = Self::bed_pair(cfg).1;
        let water_34 = water_ramp(0.34);
        for w in &self.water_samples {
            let v = w.surface;
            let undertow = premul_rgb(water_34, ((v.cov as f32) * 0.34) as u8);
            let lit = aterm_render::add_sat(
                aterm_render::add_sat(page, premul_rgb(v.color, v.cov)),
                undertow,
            );
            self.ink_ledger
                .lay_under((i32::from(w.row), i32::from(w.col)), lit);
        }

        if self.water_samples.len() == 1 && self.water_samples[0].surface.cov > 0 {
            let v = self.water_samples[0].surface;
            // Cell-scaled droplet (about 3×1 px at a ~16 px cell), so it does not
            // vanish at high DPI.
            let tw = (cw * 0.4).round().max(2.0) as i32;
            let th = (ch * 0.08).round().max(1.0) as i32;
            push_rect(
                out,
                geom,
                v.x.round() as i32 - tw / 2,
                v.y.round() as i32,
                tw,
                th,
                premul_rgb(v.color, v.cov),
            );
        }

        // Runs of adjacent samples, newest first, so a pathological geometry
        // sheds the old tail rather than the responsive head when it reaches
        // the upload budget; each run is one curve per layer.
        let mut curve = std::mem::take(&mut self.water_run);
        let clip = geom.beam_clip();
        let n = self.water_samples.len();
        let mut end = n;
        while end > 1 {
            let start = Self::water_run_start(&self.water_samples, end);
            let run = &self.water_samples[start..end];
            end = start;
            if run.len() < 2 || run.iter().all(|s| s.surface.cov == 0) {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            Self::water_curve(run, &mut curve);
            let surface_n = curve.len();
            curve.extend_from_within(..surface_n);
            for v in &mut curve[surface_n..] {
                v.cov = ((v.cov as f32) * 0.34) as u8;
                v.color = water_34;
            }
            comet_beam(out, clip, &curve[surface_n..], under_thickness, 1, 0.0);
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            comet_beam(out, clip, &curve[..surface_n], crest_thickness, 1, 0.0);
        }
        self.water_run = curve;
        if out.len() >= Self::MAX_QUADS {
            out.truncate(Self::MAX_QUADS);
            return;
        }

        // The old reflection leaves as individual gravity-driven beads, newest
        // first for load shedding: one soft round bead per dealt cell, drawn
        // out as it falls, never a hard-edged chip.
        let drip_r = (cw * 0.16).max(1.25);
        let water_88 = water_ramp(0.88);
        for sample in self.water_samples.iter().rev() {
            if sample.drip_cov == 0 {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            crate::effect_util::push_soft_disc(
                out,
                geom,
                (sample.drip_x, sample.drip_y),
                (
                    drip_r * Self::WATER_DROP_SOFT,
                    (sample.drip_h * 0.5).max(drip_r) * Self::WATER_DROP_SOFT,
                ),
                water_88,
                f32::from(sample.drip_cov),
            );
        }
        out.truncate(Self::MAX_QUADS);
    }

    /// The water's crest colour at path position `pos` (tail 0, head 1) and
    /// heat `heat`: a clear cyan, deeper toward the tail (a slate at the
    /// ramp's middle read as dull grey water).
    #[inline]
    pub(super) fn water_crest_color(pos: f32, heat: f32) -> u32 {
        water_ramp(0.66 + 0.24 * pos + 0.10 * heat)
    }

    /// The first sample of the run of adjacent samples that ends just
    /// before `end` in `samples`.
    #[inline]
    const fn water_run_start(samples: &[WaterSample], end: usize) -> usize {
        let mut start = end - 1;
        while start > 0 {
            let (a, b) = (&samples[start - 1], &samples[start]);
            if a.row.abs_diff(b.row) > 1 || a.col.abs_diff(b.col) > 1 {
                break;
            }
            start -= 1;
        }
        start
    }

    /// One run's surface as a curve into `curve` (cleared first): along one
    /// row, the swell itself between each two samples ([`Self::WATER_KNOTS`]
    /// knots a cell, phase and height eased between theirs), so its crests
    /// are rounded, never a chord's corner; across rows (a leap), the
    /// straight line from its first sample to its last, so the cells' stair
    /// draws no zigzag through the lines it crosses.
    pub(super) fn water_curve(run: &[WaterSample], curve: &mut Vec<BeamVertex>) {
        curve.clear();
        let (Some(first), Some(last)) = (run.first(), run.last()) else {
            return;
        };
        let one_row = run.iter().all(|s| s.row == first.row);
        let n = run.len();
        for (i, pair) in run.windows(2).enumerate() {
            let (a, b) = (&pair[0], &pair[1]);
            for k in 0..Self::WATER_KNOTS {
                let t = k as f32 / Self::WATER_KNOTS as f32;
                let x = a.surface.x + (b.surface.x - a.surface.x) * t;
                let y = if one_row {
                    let line = a.line_y + (b.line_y - a.line_y) * t;
                    let phase = a.phase + (b.phase - a.phase) * t;
                    line + phase.sin() * (a.amp + (b.amp - a.amp) * t)
                } else {
                    let u = (i as f32 + t) / (n - 1) as f32;
                    first.line_y + (last.line_y - first.line_y) * u
                };
                let cov = f32::from(a.surface.cov)
                    + (f32::from(b.surface.cov) - f32::from(a.surface.cov)) * t;
                curve.push(BeamVertex {
                    x,
                    y,
                    color: if t < 0.5 {
                        a.surface.color
                    } else {
                        b.surface.color
                    },
                    cov: cov.round() as u8,
                });
            }
        }
        curve.push(BeamVertex {
            y: if one_row { last.surface.y } else { last.line_y },
            ..last.surface
        });
    }

    /// Water on a light ground: the same newest-owner samples, painted
    /// source-over beneath the glyph ink (the caller's stream is
    /// `glow_under`), since additive light cannot brighten white.
    ///
    /// The two-layer wave keeps its shape: the undertow and the crest are
    /// drawn as one [`comet_beam_over`] polyline per run of adjacent samples
    /// (so a run's straight joins never paint a seam twice; a diagonal
    /// staircase's corners do, as the dark wake lights them twice), in the
    /// dark ramp's own stops. The undertow and the crest lie under the
    /// letters, so their ink is the light body's, solved against the theme's
    /// text ([`LightGround::body_ink`]); the beads and the lone droplet are
    /// small marks in the vivid ink of the role that never touches text
    /// ([`InkRole::Leading`]). A sample's coverage becomes opacity, scaled so
    /// a fresh crest reaches [`Self::LIGHT_WATER_ALPHA_CAP`] and the undertow
    /// keeps its 0.34 share; the beads and the lone droplet are source-over at
    /// the crest's opacity.
    fn emit_water_light(
        &mut self,
        cfg: &GlowConfig,
        geom: Geom,
        (under_th, crest_th): (f32, f32),
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let ink = InkRole::Leading;
        let ground = self.light_ground(cfg);
        let opacity = |cov: u8| -> u8 {
            (f32::from(cov) * Self::LIGHT_WATER_ALPHA_CAP / Self::WATER_WAKE_COV_CAP)
                .min(Self::LIGHT_WATER_ALPHA_CAP) as u8
        };
        let undertow_ink = ground.body_ink(water_ramp(0.34));
        if self.water_samples.len() == 1 && self.water_samples[0].surface.cov > 0 {
            let v = self.water_samples[0].surface;
            let a = opacity(v.cov);
            let tw = (cw * 0.4).round().max(2.0) as i32;
            let th = (ch * 0.08).round().max(1.0) as i32;
            let (x, y) = (v.x.round() as i32 - tw / 2, v.y.round() as i32);
            push_fx_quad(out, geom, x, y, tw, th, premul_rgb(ink.ink(v.color), a), a);
        }
        // Runs of adjacent samples, newest first so a saturated budget sheds
        // the old tail; each run is one polyline per layer.
        let mut curve = std::mem::take(&mut self.water_run);
        let clip = geom.beam_clip();
        let n = self.water_samples.len();
        let mut end = n;
        while end > 1 {
            let start = Self::water_run_start(&self.water_samples, end);
            let run = &self.water_samples[start..end];
            end = start;
            if run.len() < 2 || run.iter().all(|s| s.surface.cov == 0) {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            Self::water_curve(run, &mut curve);
            let surface_n = curve.len();
            curve.extend_from_within(..surface_n);
            for v in &mut curve[surface_n..] {
                v.color = undertow_ink;
                v.cov = (f32::from(opacity(v.cov)) * 0.34) as u8;
            }
            comet_beam_over(out, clip, &curve[surface_n..], under_th, 1, 0.0);
            for v in &mut curve[..surface_n] {
                v.color = ground.body_ink(v.color);
                v.cov = opacity(v.cov);
            }
            comet_beam_over(out, clip, &curve[..surface_n], crest_th, 1, 0.0);
        }
        self.water_run = curve;
        // The beads: a bead's coverage tops out at 0.54 of its crest's, so it
        // is priced back up to read as the water it was.
        // Each a round bead of clear water blue, a deeper rim round it.
        let drip_r = (cw * 0.16).max(1.25);
        for sample in self.water_samples.iter().rev() {
            if sample.drip_cov == 0 {
                continue;
            }
            if out.len() >= Self::MAX_QUADS {
                break;
            }
            let a = opacity((f32::from(sample.drip_cov) / 0.54).min(255.0) as u8);
            push_paper_bead(
                out,
                geom,
                (sample.drip_x, sample.drip_y),
                drip_r.max(sample.drip_h * 0.3),
                Self::LIGHT_WATER_DROP_INKS,
                a,
            );
        }
        out.truncate(Self::MAX_QUADS);
    }

    /// Emit the fire meteors: one straight anti-aliased streak per jump vector
    /// (white-hot head at the landing, orange core, deep-red tail fringe) whose
    /// tail retracts into the landing. Two `comet_beam` passes: a wide dim aura
    /// and a hot tapered core.
    ///
    /// On a light ground additive light cannot show, so the same geometry is
    /// painted source-over ([`comet_beam_over`]) into the under-ink stream the
    /// caller hands over: a deep-ember streak, red at the tail to burnt orange
    /// at the head (never the white-hot head, which a white page swallows),
    /// beneath the letters it crosses and at most
    /// [`Self::LIGHT_METEOR_ALPHA_CAP`] opaque.
    pub(super) fn emit_fire_meteors(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
    ) {
        if !matches!(cfg.style, GlowStyle::Fire) || self.fire_meteors.is_empty() {
            return;
        }
        let chf = geom.ch as f32;
        let dark = cfg.dark_theme;
        let beam = if dark { comet_beam } else { comet_beam_over };
        // A ramp position on the light ground: the dark ground's span folded
        // into the deep end, where every colour holds a dark letter at the cap.
        let ramp_at = |r: f32| {
            if dark {
                r
            } else {
                Self::LIGHT_METEOR_RAMP.0
                    + (Self::LIGHT_METEOR_RAMP.1 - Self::LIGHT_METEOR_RAMP.0) * r
            }
        };
        // Text-safety ceiling: bright enough to read as a meteor streak, still
        // under a wash; it crosses glyphs for ~0.3 s and the contrast rim keeps
        // them readable. On white the coverage is the paint's opacity.
        const METEOR_COV_CAP: f32 = 185.0;
        let cov_cap = if dark {
            METEOR_COV_CAP
        } else {
            Self::LIGHT_METEOR_ALPHA_CAP
        };
        const SEG: [(f32, f32, f32, f32, f32); 3] = [
            // (start_t, end_t, thickness ×, coverage ×, ramp position)
            (0.0, 0.45, 0.28, 0.16, 0.28),
            (0.45, 0.80, 0.55, 0.55, 0.55),
            (0.80, 1.0, 1.0, 1.0, 0.88),
        ];
        let clip = geom.beam_clip();
        let aura_c0 = fire_ramp(ramp_at(0.18));
        let aura_c1 = fire_ramp(ramp_at(0.35));
        let seg_colors = SEG
            .map(|(_, _, _, _, ramp)| (fire_ramp(ramp_at(ramp * 0.8)), fire_ramp(ramp_at(ramp))));
        for m in &self.fire_meteors {
            let age = now.saturating_duration_since(m.born).as_secs_f32();
            let u = (age / m.life).clamp(0.0, 1.0);
            let fade = 1.0 - u * u; // hold bright, then drop away
            if fade <= 0.02 {
                continue;
            }
            // The tail retracts into the landing point — the meteor's motion.
            let tx = m.x0 + (m.x1 - m.x0) * u;
            let ty = m.y0 + (m.y1 - m.y0) * u;
            let (dx, dy) = (m.x1 - tx, m.y1 - ty);
            // Steep dive: clamp the drawn horizontal reach to ≤1.6× the vertical
            // drop, so a one-row jump dives into its landing instead of skimming
            // the inter-line gap.
            let (tx, dx) = if dy.abs() > 0.5 && dx.abs() > 1.6 * dy.abs() {
                let ndx = dx.signum() * 1.6 * dy.abs();
                (m.x1 - ndx, ndx)
            } else {
                (tx, dx)
            };
            if (dx * dx + dy * dy).sqrt() < 1.0 {
                continue;
            }
            let head_cov = (cov_cap * fade * cfg.intensity).clamp(0.0, 255.0);
            if head_cov < 1.0 {
                continue;
            }
            // A needle, not a torch: thin core and slim taper, but a visible
            // body even on same-row jumps.
            let core_t = (chf * (0.16 + 0.14 * m.mom)).max(2.5);
            // Wide dim aura first (under the core): deep ember red.
            let aura = [
                BeamVertex {
                    x: tx,
                    y: ty,
                    color: aura_c0,
                    cov: (head_cov * 0.06) as u8,
                },
                BeamVertex {
                    x: m.x1,
                    y: m.y1,
                    color: aura_c1,
                    cov: (head_cov * 0.16) as u8,
                },
            ];
            beam(out, clip, &aura, core_t * 1.5, 2, 0.0);
            if out.len() >= Self::MAX_QUADS {
                out.truncate(Self::MAX_QUADS);
                return;
            }
            // Hot tapered core: three chained segments, slim red tail, orange
            // mid, thick near-white head wedge striking the landing.
            for (&(t0, t1, thick, covx, _), &(col0, col1)) in SEG.iter().zip(&seg_colors) {
                if out.len() >= Self::MAX_QUADS {
                    return;
                }
                let c0 = (head_cov * covx * 0.55) as u8;
                let c1 = (head_cov * covx) as u8;
                if c1 == 0 {
                    continue;
                }
                let verts = [
                    BeamVertex {
                        x: tx + dx * t0,
                        y: ty + dy * t0,
                        color: col0,
                        cov: c0,
                    },
                    BeamVertex {
                        x: tx + dx * t1,
                        y: ty + dy * t1,
                        color: col1,
                        cov: c1,
                    },
                ];
                beam(out, clip, &verts, (core_t * thick).max(1.5), 1, 0.0);
                if out.len() >= Self::MAX_QUADS {
                    out.truncate(Self::MAX_QUADS);
                    return;
                }
            }
        }
    }

    /// Polyline scratch lent to v2 ([`rk::Frame::beams`]): the meteor's 96
    /// stations and the ring's polyline, with room to spare.
    pub(super) const V2_BEAM_SCRATCH: usize = 512;
    /// The cue sink v2 is lent ([`rk::Frame::cues`]): far above the handful a
    /// landing frame mints (the meteor, five rain glints, the key's own).
    pub(super) const V2_CUE_SCRATCH: usize = 64;

    /// Size the three pixel streams to their caps once (see the emit arm of
    /// seam point 3): [`Self::MAX_QUADS`] for `under` and `out`,
    /// [`Self::MAX_HALOS`] for `halos`. A capacity compare per frame after
    /// the first; `reserve` is exact-or-more, never a shrink.
    #[inline]
    pub(super) fn reserve_v2_scratch(
        under: &mut Vec<GlowQuad>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        if under.capacity() < Self::MAX_QUADS {
            under.reserve(Self::MAX_QUADS - under.len());
        }
        if out.capacity() < Self::MAX_QUADS {
            out.reserve(Self::MAX_QUADS - out.len());
        }
        if halos.capacity() < Self::MAX_HALOS {
            halos.reserve(Self::MAX_HALOS - halos.len());
        }
    }

    /// Cap the halo stream to [`Self::MAX_HALOS`]; v2's quads are self-capped at
    /// the emitter.
    #[inline]
    pub(super) fn cap_and_clear_v2_streams(halos: &mut Vec<RainHalo>) {
        halos.truncate(Self::MAX_HALOS);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "clock, config, geometry, cursor, and the halo and under-ink streams; a param struct would obscure the single internal call site"
    )]
    pub(super) fn emit_crown(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        halos: &mut Vec<RainHalo>,
        under: &mut Vec<GlowQuad>,
    ) {
        // The knob law: a style that reads no `Glow size` draws no crown,
        // whatever radius a config carries.
        if cfg.radius <= 0.0
            || !cfg
                .style
                .reads_knob(TrailKnob::Radius, KnobContext::widest(false))
        {
            return;
        }
        // `crown_until` is the bounded emission arm (`last_move` may be stamped
        // by a move that declined visual admission); without this guard a
        // cleared arm would still paint until `last_move` aged out.
        if !self.crown_until.is_some_and(|until| now < until) {
            return;
        }
        let Some((cr, cc)) = cur else { return };
        if cr as usize >= geom.rows || cc as usize >= geom.cols {
            return;
        }
        let Some(t0) = self.last_move else { return };
        let age_ms = now.saturating_duration_since(t0).as_millis() as u64;
        // `CROWN_TYPING_MS` for a typing advance, `CROWN_MS` for a jump;
        // `.max(1)` guards the zeroed default.
        let window = self.crown_window_ms.max(1);
        if age_ms >= window {
            return;
        }
        let fade = 1.0 - age_ms as f32 / window as f32;
        // `Glow size` sizes every crown: the shipped default draws the
        // crown at its authored size, and the knob scales it from there.
        let size = cfg.radius / GLOW_RADIUS_DEFAULT;
        // A LIGHT GROUND: the crown is light added around the caret, which
        // white paper cannot show and which would lift fresh ink toward the
        // ground. Lumen, whose glow IS the style, wears it as soft ink under
        // the letters behind the caret; Fire's ink tongues
        // ([`Self::emit_flames_light`]) already carry the fire, so its mouth
        // is dropped there; every other style draws no crown on paper.
        // (Classic's own engine inks its crown, `ClassicWake::emit_light_crown`.)
        if !cfg.dark_theme {
            if matches!(cfg.style, GlowStyle::Lumen) {
                // A glow is the style itself, so it shows at a relaxed cadence
                // and deepens as typing warms. A cell and a half back over
                // the letters just typed and only to the caret's own edge
                // ahead, so it reads as the tracer's head rather than a lamp
                // in front of the caret; its ink solved against the theme's
                // text, so any strength keeps the letters on it legible.
                let warmth = 0.6 + 0.4 * self.typing_boost().min(1.0);
                let cap = LIGHT_BODY_ALPHA_CAP;
                let peak = (cap * (0.55 + 0.45 * fade) * cfg.intensity * warmth).min(cap);
                let (cwf, chf) = (geom.cw as f32, geom.ch as f32);
                let budget = under.len().saturating_add(Self::MAX_QUADS);
                push_ink_ellipse(
                    under,
                    geom,
                    geom.cell_center(cr, cc),
                    (cwf * 1.6 * size, cwf * 0.5 * size),
                    chf * 0.46 * size,
                    self.light_ground(cfg).body_ink(cfg.color),
                    peak,
                    budget,
                );
            }
            return;
        }
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = geom.origin_x as i32 + cc as i32 * cw;
        let cy = geom.origin_y as i32 + cr as i32 * ch;
        // Fire's crown swells with the blaze, from a candle-glow at rest to a
        // wide roaring halo at full burn.
        let r = if matches!(cfg.style, GlowStyle::Fire) {
            (cfg.radius * geom.ch as f32 * (1.1 + 0.7 * self.fire_t())) as i32
        } else {
            (cfg.radius * geom.ch as f32) as i32
        };
        // The crown's peak coverage and hue per style. Laser stays in its own
        // hue (no white flash).
        let (base_cov, color) = match cfg.style {
            // Laser's crown doubles as the beam's impact flash (boosted below),
            // so it carries the hottest base.
            GlowStyle::Laser => (105.0f32, cfg.color),
            // Fire's crown flickers on a fast phase and burns hotter as the
            // eased temperature climbs, capped at 0.85 on the black-body ramp
            // (orange-gold, never a cream-white box).
            GlowStyle::Fire => (
                (30.0 + 26.0 * self.fire_t()) * (0.74 + 0.26 * (age_ms as f32 * 0.045).sin()),
                fire_ramp(0.55 + 0.30 * self.fire_t()),
            ),
            GlowStyle::Water => (28.0, water_ramp(0.74)),
            GlowStyle::Phaser => (50.0, hsv2rgb(self.hue, 0.9, 1.0)),
            // Sparkle's head is its gold ([`Self::comet_color`]).
            GlowStyle::Sparkle => (50.0, Self::sparkle_gold(cfg)),
            // Exhaustiveness only: the classic wake draws its own crown in
            // `classic_wake::ClassicWake::emit_crown`.
            GlowStyle::Classic => (50.0, hsv2rgb(self.hue, 0.9, 1.0)),
            // Exhaustiveness only: v2 owns the rainbow kitty's head and this
            // emitter never runs while it does.
            GlowStyle::RainbowKitty => return,
            GlowStyle::Beam => (46.0, cfg.color),
            GlowStyle::Lumen => (50.0, cfg.color),
            // The comet's crown is the faint outer coma, slightly whitened; the
            // nucleus cursor draws the bright inner coma.
            GlowStyle::Comet => (44.0, lerp_rgb(cfg.color, 0x00FF_FFFF, 0.30)),
        };
        // The crown follows typing heat; Laser and Fire also ride the jump flare,
        // so the landing cell erupts the instant a jump connects.
        let boost = if matches!(cfg.style, GlowStyle::Laser) {
            self.typing_boost().max(1.3 * self.blaze())
        } else if matches!(cfg.style, GlowStyle::Fire) {
            (0.28 + 0.92 * self.fire_t()).max(1.2 * self.fire_t())
        } else {
            self.typing_boost()
        };
        // Fire: the mouth of the fire is two breathing radial halos (a wide
        // soft skirt under a tight hot heart) on the cursor.
        if matches!(cfg.style, GlowStyle::Fire) {
            // The light hovers above the glyph line (heat rises); broad halos
            // centred on the text row would clamp the typed line to white.
            let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 - ch as f32 * 0.10);
            let reach = r as f32 + ch as f32 * 0.55;
            let peak = (base_cov * fade * cfg.intensity * boost).min(255.0);
            // Two additive halos, each spending the over-ink ledger where it
            // reaches the letters beside the caret.
            if peak >= 1.0 {
                let heart = fire_ramp(0.62 + 0.23 * self.fire_t());
                for (radii, rgb, cov) in [
                    ((reach * 1.9, reach * 1.45), color, peak * 0.34),
                    ((reach * 1.05, reach * 0.85), heart, peak * 0.82),
                ] {
                    let share = self.ink_ledger.admit_halo(
                        (ccx, ccy),
                        radii,
                        premul_rgb(rgb, cov as u8),
                        InkLedger::STEADY_FLOOR,
                    );
                    push_halo(
                        halos,
                        geom,
                        ccx,
                        ccy,
                        radii.0,
                        radii.1,
                        premul_rgb(rgb, (cov * share) as u8),
                    );
                }
            }
            // The forge dressing rides `cursor_temp` in `emit_forge_cursor`,
            // outside this crown window.
            return;
        }
        // Every remaining style (phaser, sparkle, laser, water, lumen, comet):
        // a compact radial bloom on the cursor cell, full punch over the empty
        // cell and near zero at the neighbouring glyphs' centres at the
        // default size, so the newest letter keeps contrast. Not concentric
        // rectangles, which read as debug geometry and saturate the newest
        // glyph.
        let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 + ch as f32 * 0.5);
        let peak = (base_cov * 1.9 * fade * cfg.intensity * boost).min(210.0);
        if peak >= 1.0 {
            // Over the dark bed, whatever of the crown reaches the letters
            // beside the caret spends the over-ink account first of the
            // lights laid after the streak (an idle account admits it whole).
            let from = halos.len();
            let (core_rx, core_ry) = (cw as f32 * 1.25 * size, ch as f32 * 0.85 * size);
            push_halo(
                halos,
                geom,
                ccx,
                ccy,
                core_rx,
                core_ry,
                premul_rgb(color, peak as u8),
            );
            // Laser: a tight hot heart over the landing cell in the beam hue,
            // riding the blaze-boosted peak, as the impact flash.
            if matches!(cfg.style, GlowStyle::Laser) {
                push_halo(
                    halos,
                    geom,
                    ccx,
                    ccy,
                    cw as f32 * 0.7 * size,
                    ch as f32 * 0.55 * size,
                    premul_rgb(color, (peak * 0.9).min(230.0) as u8),
                );
            }
            self.ink_ledger
                .admit_halos_masked(halos, from, InkLedger::HEAD_FLOOR);
        }
    }

    /// The forge dressing on the cursor cell (fire only): molten core, hot
    /// top rim and edge-ember crawl, all riding `cursor_temp`. Not inside
    /// `emit_crown`: the metal cools on `TEMP_RELEASE_TAU` (1.4 s), far slower
    /// than the ~350 ms crown window. Emits every frame the metal is warm
    /// (`is_active` keeps the timer armed) and nothing below `FORGE_MIN_TEMP`.
    ///
    /// Dark ground only. All three are additive light, which cannot show on a
    /// light ground: there the block caret covers the cell, and the crawl
    /// would only lift the neighbouring glyph toward the page. The forge fill
    /// itself carries the heat on a light page.
    pub(super) fn emit_forge_cursor(
        &mut self,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        if !matches!(cfg.style, GlowStyle::Fire) || !cfg.dark_theme {
            return;
        }
        let temp = self.cursor_temp;
        if temp <= Self::FORGE_MIN_TEMP {
            return;
        }
        let Some((cr, cc)) = cur else { return };
        if cr as usize >= geom.rows || cc as usize >= geom.cols {
            return;
        }
        let cw = geom.cw as i32;
        let ch = geom.ch as i32;
        let cx = geom.origin_x as i32 + cc as i32 * cw;
        let cy = geom.origin_y as i32 + cr as i32 * ch;
        // The crown ambience's anchor (ccy lifted 0.10·ch — heat rises).
        let (ccx, ccy) = (cx as f32 + cw as f32 * 0.5, cy as f32 - ch as f32 * 0.10);
        let tn = ((temp - Self::FORGE_MIN_TEMP) / (1.0 - Self::FORGE_MIN_TEMP)).clamp(0.0, 1.0);
        let phase = self.flame_phase;
        // The core and the crawl spend the over-ink ledger where they reach
        // the letters beside the caret; the rim stays on the caret's own cell.
        //
        // Molten core: a small white-hot pool just above centre (heat rises),
        // swelling and pulsing with the metal.
        let pulse = 0.85 + 0.15 * (phase * 2.3).sin();
        let core_cov = (150.0 * tn * pulse * cfg.intensity).min(255.0);
        if core_cov >= 2.0 {
            let (centre, radii) = (
                (ccx, ccy - ch as f32 * 0.12),
                (
                    cw as f32 * (0.30 + 0.14 * tn),
                    ch as f32 * (0.20 + 0.10 * tn),
                ),
            );
            let rgb = fire_ramp((0.72 + 0.20 * tn).min(0.92));
            let share = self.ink_ledger.admit_halo(
                centre,
                radii,
                premul_rgb(rgb, core_cov as u8),
                InkLedger::STEADY_FLOOR,
            );
            push_halo(
                halos,
                geom,
                centre.0,
                centre.1,
                radii.0,
                radii.1,
                premul_rgb(rgb, (core_cov * share) as u8),
            );
        }
        // Hot top rim: the block's top edge glows hottest, a thin bright line.
        let rim_cov = (90.0 * tn * cfg.intensity).min(255.0);
        if rim_cov >= 2.0 {
            push_rect(
                out,
                geom,
                cx,
                cy,
                cw,
                2,
                premul_rgb(fire_ramp(0.88), rim_cov as u8),
            );
        }
        // Edge-ember crawl: above half temperature, two sparks crawl the block's
        // perimeter (a burning-paper edge), deterministic in the phase.
        if temp > 0.5 {
            let crawl_cov = (130.0 * (temp - 0.5) * 2.0 * cfg.intensity).min(200.0);
            if crawl_cov >= 2.0 {
                let per = 2.0 * (cw as f32 + ch as f32);
                let rgb = fire_ramp(0.85);
                let premul = premul_rgb(rgb, crawl_cov as u8);
                for k in 0..2u32 {
                    let u = fract(phase * (0.23 + 0.07 * k as f32) + k as f32 * 0.5);
                    let d = u * per;
                    let (ex, ey) = if d < cw as f32 {
                        (cx as f32 + d, cy as f32)
                    } else if d < cw as f32 + ch as f32 {
                        ((cx + cw) as f32, cy as f32 + (d - cw as f32))
                    } else if d < 2.0 * cw as f32 + ch as f32 {
                        (
                            (cx + cw) as f32 - (d - cw as f32 - ch as f32),
                            (cy + ch) as f32,
                        )
                    } else {
                        (
                            cx as f32,
                            (cy + ch) as f32 - (d - 2.0 * cw as f32 - ch as f32),
                        )
                    };
                    let share = self.ink_ledger.admit_halo(
                        (ex, ey),
                        (3.5, 3.5),
                        premul,
                        InkLedger::STEADY_FLOOR,
                    );
                    push_halo(
                        halos,
                        geom,
                        ex,
                        ey,
                        3.5,
                        3.5,
                        premul_rgb(rgb, (crawl_cov * share) as u8),
                    );
                }
            }
        }
    }

    pub(super) fn emit_ring(
        &self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        let Some(ring) = self.ring else { return };
        let age = now.saturating_duration_since(ring.born).as_secs_f32();
        let t = (age / ring.life).clamp(0.0, 1.0);
        let fade = 1.0 - t;
        let cov = (200.0 * fade * cfg.intensity) as u8;
        if cov == 0 {
            return;
        }
        // The jump's grade: every radius below is this much wider (1.0 for a hop).
        let rf = classic_ring_radius_factor(ring.scale);
        let color = match cfg.style {
            GlowStyle::Laser => cfg.color,
            // The landing shockwave glows white-hot at birth and cools as it
            // expands, matched to the jump flare erupting inside it.
            GlowStyle::Fire => fire_ramp(0.95 - 0.35 * t),
            GlowStyle::Water => water_ramp(0.6), // the expanding ripple
            _ => cfg.accent,
        };
        // A LIGHT GROUND: every ring is ink. Water's ripples keep their
        // flattened two-ring train, inked at the over-text ceiling; every
        // other style's shockwave is an expanding darkened veil ring.
        if !cfg.dark_theme {
            self.emit_ring_light(cfg, geom, ring, t, cov, color, out, halos);
            return;
        }
        // Fire: the impact is a round expanding flash (white-hot cooling to
        // ember) with a dimmer aftershock behind it.
        // Every round bloom below is fitted inside the effects box, so a
        // landing at the grid's edge fades out before it instead of standing
        // cut by it ([`fit_glow_in_box`]).
        let bloom = |halos: &mut Vec<RainHalo>, radii: (f32, f32), premul: u32| {
            let ((cx, cy), (rx, ry)) = fit_glow_in_box(geom, (ring.cx, ring.cy), radii);
            push_halo(halos, geom, cx, cy, rx, ry, premul);
        };
        if matches!(cfg.style, GlowStyle::Fire) {
            let grow = (0.55 + 1.25 * t) * geom.ch as f32 * rf;
            bloom(halos, (grow * 1.5, grow * 1.1), premul_rgb(color, cov));
            if t > 0.30 {
                let t2 = (t - 0.30) / 0.70;
                let cov2 = (150.0 * (1.0 - t2) * cfg.intensity) as u8;
                if cov2 > 0 {
                    let g2 = (0.55 + 1.25 * t2) * geom.ch as f32 * rf;
                    bloom(
                        halos,
                        (g2 * 1.5, g2 * 1.1),
                        premul_rgb(fire_ramp(0.45), cov2),
                    );
                }
            }
            return;
        }
        // Water lands as ripples: flattened ellipse rings spreading from the
        // splash point, centred slightly low (the waterline), fading outward.
        if matches!(cfg.style, GlowStyle::Water) {
            let cx = ring.cx;
            let cy = ring.cy + geom.ch as f32 * 0.28;
            let mut ripple = |tt: f32, cov: u8, color: u32| {
                if cov == 0 {
                    return;
                }
                let rx = (0.7 + 1.5 * tt) * geom.ch as f32 * rf;
                let ry = (rx * 0.38).max(2.0);
                push_ripple_ring(out, geom, (cx, cy), (rx, ry), 2.0, premul_rgb(color, cov));
            };
            ripple(t, cov, color);
            // The trailing second ripple: rings on water always come in trains.
            if t > 0.30 {
                let t2 = (t - 0.30) / 0.70;
                ripple(
                    t2,
                    (150.0 * (1.0 - t2) * cfg.intensity) as u8,
                    water_ramp(0.45),
                );
            }
            return;
        }
        // Every other dark style: the light arm's soft round shape as light,
        // an expanding radial bloom fading with the ring's life. A thin
        // square outline here read as a stray rectangle left on the page
        // (its side cut off by the pad when the landing sat near the edge);
        // the classic wake keeps its own square ring. A single ring: fire's
        // aftershock and water's ripple train returned above.
        let grow = (0.7 + 1.0 * t) * geom.ch as f32 * rf;
        bloom(
            halos,
            (grow, grow * 0.85),
            premul_rgb(color, (f32::from(cov) * Self::DARK_RING_SHARE) as u8),
        );
    }

    /// The dark ring's bloom peak as a share of the ring's coverage: a
    /// filled bloom carries far more light than the 2 px outline it replaced
    /// at the same coverage.
    const DARK_RING_SHARE: f32 = 0.45;

    /// The landing ring on a LIGHT GROUND, where an additive shockwave is
    /// invisible. Water lands as its ripple train, rasterized in white and
    /// re-inked as conservative over-text ink (the ripples cross letters) at
    /// the over-text ceiling. Every other style's ring inverts to an
    /// expanding source-over veil ring in the ring colour's conservative ink
    /// (Fire's white-hot flash included), its centre over-alpha fading with
    /// the ring life and
    /// capped by `aterm_render::halo_over_cap`, so it never buries text.
    #[allow(
        clippy::too_many_arguments,
        reason = "the ring's resolved facts (life, coverage, colour) handed over from emit_ring"
    )]
    fn emit_ring_light(
        &self,
        cfg: &GlowConfig,
        geom: Geom,
        ring: Ring,
        t: f32,
        cov: u8,
        color: u32,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
    ) {
        let rf = classic_ring_radius_factor(ring.scale);
        if matches!(cfg.style, GlowStyle::Water) {
            let cx = ring.cx;
            let cy = ring.cy + geom.ch as f32 * 0.28;
            let mut ripple = |tt: f32, cov: u8, color: u32| {
                if cov == 0 {
                    return;
                }
                let rx = (0.7 + 1.5 * tt) * geom.ch as f32 * rf;
                let ry = (rx * 0.38).max(2.0);
                let from = out.len();
                push_ripple_ring(
                    out,
                    geom,
                    (cx, cy),
                    (rx, ry),
                    2.0,
                    premul_rgb(0x00FF_FFFF, cov),
                );
                let ink = InkRole::OverText.ink(color);
                reink_quads(&mut out[from..], LIGHT_INK_ALPHA_CAP as u8, |_| ink);
            };
            ripple(t, cov, color);
            if t > 0.30 {
                let t2 = (t - 0.30) / 0.70;
                ripple(
                    t2,
                    (150.0 * (1.0 - t2) * cfg.intensity) as u8,
                    water_ramp(0.45),
                );
            }
            return;
        }
        let grow = (0.7 + 1.0 * t) * geom.ch as f32 * rf;
        // Fire's landing is the warmth of its flames, a soft orange glow,
        // not the grey a white-hot flash inks to on paper (it read as a
        // stain). Its cap keeps any letter under it legible.
        // Every other style's veil wears its own colour at the vivid ink's
        // weight ([`Self::light_veil_ink`]): the conservative over-text ink
        // is the colour darkened, which a veil thinned to a grey halo round
        // the caret.
        let veil = if matches!(cfg.style, GlowStyle::Fire) {
            Self::FIRE_LIGHT_LANDING
        } else {
            Self::light_veil_ink(color)
        };
        // The vivid ink is light enough that a deeper veil keeps a letter
        // under it at the light bar; the fire's warm veil keeps its cap.
        let cap = if matches!(cfg.style, GlowStyle::Fire) {
            ((cov as u32) / 2).clamp(1, 120) as u8
        } else {
            ((cov as u32) * 3 / 4).clamp(1, 160) as u8
        };
        // A veil falling off from its centre: one capped veil is a flat
        // disc that reads on paper as a stain.
        push_soft_veil(
            halos,
            geom,
            (ring.cx, ring.cy),
            (grow, grow * 0.85),
            veil,
            cap,
        );
    }

    /// A light landing veil's ink for `rgb`: its hue (a gold or yellow bent
    /// to amber) at its own saturation (raised to [`Self::LIGHT_VEIL_SAT`] at
    /// least, so a pale colour still tints), at full value, darkened only as
    /// far as the vivid ink's luma ([`LIGHT_INK_BOLD_MAX_LUMA`]): a colour,
    /// never the grey (or the olive) a darker ink thins to.
    pub(super) fn light_veil_ink(rgb: u32) -> u32 {
        let (h, s, _) = crate::color_math::rgb2hsv(rgb & 0x00FF_FFFF);
        // A gold or a yellow darkened to the vivid ink's weight is an olive:
        // the warm band bends to amber, and the yellow-greens past it cross
        // to leaf green in a few degrees.
        let h = if (30.0..80.0).contains(&h) {
            30.0 + (h - 30.0) * 0.2
        } else if (80.0..95.0).contains(&h) {
            40.0 + (h - 80.0) * (55.0 / 15.0)
        } else {
            h
        };
        let full = crate::color_math::hsv2rgb(h, s.max(Self::LIGHT_VEIL_SAT), 1.0);
        let y = luma709(full);
        if y <= LIGHT_INK_BOLD_MAX_LUMA {
            return full;
        }
        let k = LIGHT_INK_BOLD_MAX_LUMA / y;
        let ch = |sh: u32| ((((full >> sh) & 0xff) as f32 * k) + 0.5) as u32;
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    }

    /// Emit the flying particles, style by style.
    ///
    /// THE PARTICLE LAW (every arm but Fire, which keeps its own ember
    /// discipline): a grain's body is round, its one accent is the dealt plus,
    /// and it keeps off the text. "The text" is a line of text the frame
    /// shows, from its first glyph to its last, the gaps between words
    /// included ([`TextView`]: the probed rows, and the frame's own cells for
    /// the rows its light can reach, so the lines an Enter just printed above
    /// the caret count). On a dark ground a grain dims hard there
    /// ([`Self::GRAIN_OVER_GLYPH_DIM`], the embers' discipline), so the
    /// comet's debris, which hangs on the typed row, reads as neither a dotted
    /// leader through the letters nor a hyphen between them (water's droplets
    /// keep their own lower coverage). Sparkle's shower and Beam's stardust
    /// are not drawn over the text on either ground: a moon or a speck over a
    /// line reads as a smudge or as punctuation. On a light ground additive
    /// light cannot show, so every grain is source-over ink
    /// ([`push_light_mote`], the light star twin, the source-over dash and
    /// crescent; sparkle's in saturated golds, [`push_paper_bead`]), and a
    /// grain over the text is not drawn at all: text first.
    pub(super) fn emit_particles(
        &mut self,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
        halos: &mut Vec<RainHalo>,
        under: &mut Vec<GlowQuad>,
    ) {
        if self.particles.is_empty() {
            return;
        }
        let sz = (geom.ch as f32 * 0.18).max(2.0) as i32;
        let water = matches!(cfg.style, GlowStyle::Water);
        let dark = cfg.dark_theme;
        // Fire keeps its own ember discipline; every other arm follows the
        // particle law. Fire's light-ground embers keep off the text too (its
        // arm below), and Beam's stardust keeps off it whole.
        let beam = matches!(cfg.style, GlowStyle::Beam);
        let lawful = !matches!(cfg.style, GlowStyle::Fire | GlowStyle::Beam);
        let light_fire = !dark && matches!(cfg.style, GlowStyle::Fire);
        let (cwf, ch) = (geom.cw as f32, geom.ch as f32);
        let (oxf, oyf) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        // Read the frame's text once a frame, and only when a grain that
        // keeps off it flies.
        let text = TextView::new(self.probed_text_spans(), Some(&self.frame_text));
        let covers = |x: f32, y: f32| {
            let row = ((y - oyf) / ch).floor() as i32;
            let col = ((x - oxf) / cwf).floor() as i32;
            text.covers_cell(row, col)
        };
        // A mark's reach past its centre: a grain whose centre sits just off
        // a line still laid its rim over the letters' tops.
        let margin = ch * 0.14;
        let near_text = |x: f32, y: f32| {
            covers(x, y)
                || covers(x, y - margin)
                || covers(x, y + margin)
                || covers(x - margin, y)
                || covers(x + margin, y)
        };
        // The glyph band of a row holding text (from above its ascenders to
        // its descenders' feet): its top's window y when `y` lies in it.
        let glyph_band = |y: f32| -> Option<(i32, f32)> {
            let row = ((y - oyf) / ch).floor() as i32;
            let top = oyf + row as f32 * ch;
            let k = (y - top) / ch;
            ((Self::GLYPH_BAND.0..Self::GLYPH_BAND.1).contains(&k)
                && text.knows_row(row)
                && !text.row_is_blank(row))
            .then_some((row, top))
        };
        let caret_row = self.last.map(|(r, _)| i32::from(r));
        // The caret's cell grown by a grain's reach: the block caret covers
        // its cell, so a grain whose mark reaches into it is cut in half.
        let caret = self.last.map(|(r, c)| Self::caret_box(geom, r, c));
        let reach = ch * 0.30;
        let on_caret = |x: f32, y: f32| {
            caret.is_some_and(|(x0, y0, x1, y1)| {
                x > x0 as f32 - reach
                    && x < x1 as f32 + reach
                    && y > y0 as f32 - reach
                    && y < y1 as f32 + reach
            })
        };
        let comet_dark_leading =
            dark && matches!(cfg.style, GlowStyle::Comet) && !self.streak_unbudgeted();
        let sparkle = matches!(cfg.style, GlowStyle::Sparkle);
        let base_cov = if water { 112.0 } else { 230.0 };
        let laser_thick = (ch * 0.11).max(1.5);
        let mini_comet_girth = if dark {
            (ch * 0.09).max(1.2)
        } else {
            (ch * 0.13).max(2.0)
        };
        let fine_star_arm = star_arm_px(star_arm(ch, STAR_ARM_FINE));
        let sparkle_gold_rgb = (sparkle && dark).then(|| Self::sparkle_gold(cfg));
        'flying: for p in self.particles.iter().rev() {
            if out.len() >= Self::MAX_QUADS {
                break 'flying;
            }
            let age = now.saturating_duration_since(p.born).as_secs_f32();
            let t = (age / p.life).clamp(0.0, 1.0);
            let fade = 1.0 - t;
            // Analytic ballistic position.
            let x = p.x0 + p.vx * age;
            let y = p.y0 + p.vy * age + 0.5 * p.gy * age * age;
            // Comet dust on a dark ground hangs in a text row's leading, above
            // its ascenders or under its letters, never at their x-height,
            // where a grain between two words reads as a typed interpunct
            // (and a cluster after a line's last letter as a dash).
            let leading_band = comet_dark_leading.then(|| glyph_band(y)).flatten();
            let leading = leading_band.is_some();
            // A leading is a thin band: only some of the dust fits there, each
            // grain at its own height in it (a row of them on one line read as
            // a dotted rule).
            let seat = (p.hue * 977.0).fract();
            if leading && seat >= Self::COMET_DARK_DUST_SHARE {
                continue;
            }
            let y = match leading_band {
                Some((_, top)) => {
                    let k = seat / Self::COMET_DARK_DUST_SHARE;
                    let jitter = ((p.hue * 4513.0).fract() - 0.5) * Self::COMET_DARK_DUST_JITTER;
                    top + ch
                        * (jitter
                            + if k < 0.5 {
                                Self::COMET_DARK_DUST_HIGH
                            } else {
                                Self::COMET_DARK_DUST_LOW
                            })
                }
                None => y,
            };
            // Text first: on a light ground a grain over the text is not
            // drawn; on a dark one it dims to a faint glint (water's droplets
            // excepted, already at their lower coverage below).
            let over_text = (lawful || beam) && covers(x, y);
            if over_text && (!dark || sparkle || beam) {
                continue;
            }
            // Sparkle's shower and Beam's stardust keep their rims off the
            // letters too, and, on a printed line the caret is not on, off
            // its glyph band within two cells of its letters (an Enter's
            // leap otherwise leaves a grain standing after the line's last
            // letter, read as a stray glyph).
            if (sparkle || beam)
                && (near_text(x, y)
                    || glyph_band(y).is_some_and(|(row, _)| {
                        let col = ((x - oxf) / cwf).floor() as i32;
                        Some(row) != caret_row && (-2..=2).any(|d| text.covers_cell(row, col + d))
                    }))
            {
                continue;
            }
            // Nor on a light ground where the block caret would cut it: half
            // a grain standing off the caret's edge reads as a nub on it.
            // Sparkle's shower keeps the rule on a dark ground too, where a
            // grain the block half hides leaves a sliver of light on its edge.
            if (!dark || sparkle) && (lawful || light_fire) && on_caret(x, y) {
                continue;
            }
            let text_dim = if over_text && !water && !leading {
                Self::GRAIN_OVER_GLYPH_DIM
            } else {
                1.0
            };
            // Sparkles rotate hue over the flight; Fire/Water/Comet keep the
            // birth seed. `style_particle_color` (shared with the settings demo)
            // is evaluated lazily.
            let particle_color = || {
                let hue = match cfg.style {
                    // Fire/Water/Comet use `hue` as a per-particle seed
                    // (temperature, droplet depth, grain size + twinkle phase).
                    GlowStyle::Fire | GlowStyle::Water | GlowStyle::Comet => p.hue,
                    _ => (p.hue + age * 0.5).fract(),
                };
                style_particle_color(cfg.style, cfg.color, hue, fade)
            };
            // `cov_scale` divides a point-stacked burst's punch (laser:
            // 1/√burst; exactly 1.0 elsewhere).
            let cov = (base_cov * p.cov_scale * fade * cfg.intensity * text_dim) as u8;
            if cov == 0 {
                continue;
            }
            // A light grain's opacity for a dark coverage request: source-over
            // ink reads at less, so a fresh grain reaches the may-touch-text
            // ceiling (capped inside the light pushers).
            let ink_alpha =
                |c: u8| (f32::from(c) * Self::LIGHT_GRAIN_GAIN).min(LIGHT_INK_ALPHA_CAP) as u8;
            // Beam's stardust is additive light with no light twin: on a
            // light ground it could only lift the strokes it crosses, so it
            // is not drawn there (the tube under the letters is the look).
            if !dark && matches!(cfg.style, GlowStyle::Beam) {
                continue;
            }
            match cfg.style {
                // Laser: ablation sparks draw as velocity-aligned streaks (white
                // tip cooling to the beam hue) via the shared AA beam; on a
                // light ground, the same streak in the beam's ink.
                GlowStyle::Laser => {
                    let thick = laser_thick;
                    let vy = p.vy + p.gy * age; // live velocity, not birth velocity
                    if dark {
                        push_velocity_dash(
                            out,
                            geom,
                            x,
                            y,
                            p.vx,
                            vy,
                            0.05, // seconds of motion the dash trails
                            thick,
                            cfg.color,
                            particle_color(),
                            cov,
                        );
                    } else {
                        push_velocity_dash_over(
                            out,
                            geom,
                            x,
                            y,
                            p.vx,
                            vy,
                            0.05,
                            thick,
                            InkRole::OverText.ink(cfg.color),
                            ink_alpha(cov),
                        );
                    }
                    continue;
                }
                // Beam stardust: tiny twinkling 4-point stars, mostly white, some
                // ice-blue, the odd violet, hanging in the wake.
                GlowStyle::Beam => {
                    let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                    let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                    if scov > 0 {
                        let tint = if p.hue < 0.45 {
                            0x00FF_FFFF // starlight white
                        } else if p.hue < 0.85 {
                            0x00CF_ECFF // ice-blue
                        } else {
                            0x00A9_8BFF // bright nebula violet
                        };
                        // Stardust rule: round motes, with a dealt 1-in-16 keeping
                        // the 4-point twinkle; a mote is priced at the plus's
                        // composited centre ([`push_dust_halo`]).
                        if !star_accent((p.hue * 4096.0) as u32) {
                            let r = dust_r(ch, p.hue) * (0.6 + 0.4 * fade);
                            push_dust_halo(halos, geom, x, y, r, tint, scov);
                            continue;
                        }
                        let arm = fine_star_arm;
                        let (ix, iy) = (x as i32, y as i32);
                        if !push_twinkle_star(
                            out,
                            geom,
                            ix,
                            iy,
                            arm,
                            scov,
                            false,
                            tint,
                            Self::MAX_QUADS,
                        ) {
                            break 'flying;
                        }
                    }
                    continue;
                }
                // Comet debris: round grains of shed ice, each twinkling on its
                // own seeded phase, size riding the hue seed; a dealt 1-in-16
                // grain throws a tiny 4-point glint at its twinkle peak. The
                // grains hang on the typed row, so over the text they dim
                // (dark) or drop (light). On a light ground the debris is dust
                // the trail sheds, so it settles as round motes of the body's
                // ink UNDER the letters (`under`), where a row the host did
                // not probe cannot have a grain over its strokes.
                GlowStyle::Comet => {
                    // One twinkle rate for the family; only the size seed varies.
                    let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                    let gcov = ((cov as f32) * twinkle) as u8;
                    if gcov == 0 {
                        continue;
                    }
                    // Only a dealt grain throws the glint, at its twinkle peak;
                    // the rest shimmer as dust (owner, 2026-08-09).
                    let glint = star_accent((p.hue * 4096.0) as u32) && twinkle_peak(twinkle);
                    // The glint's coverage: a highlight on a grain of ice is
                    // dimmer than the grain, and is laid OVER the grain's
                    // body, never in place of it, so a grain keeps its round
                    // shape through every twinkle (the stardust rule: no grain
                    // changes shape mid-flight) and its peak is its brightest
                    // instant, not a dip.
                    let glint_cov = (f32::from(gcov) * COMET_GLINT_COV) as u8;
                    let r = dust_r(ch, p.hue) * (0.6 + 0.4 * fade);
                    let rgb = particle_color();
                    if !dark {
                        if under.len() >= Self::MAX_QUADS {
                            continue;
                        }
                        let mote = r * STAR_ARM_INK;
                        let budget = under.len().saturating_add(Self::MAX_QUADS);
                        push_ink_ellipse(
                            under,
                            geom,
                            (x, y),
                            (mote, mote),
                            mote,
                            self.light_ground(cfg).body_ink(rgb),
                            f32::from(ink_alpha(gcov)),
                            budget,
                        );
                        continue;
                    }
                    // The body is a round mote priced at the plus's composited
                    // centre ([`push_dust_halo`]), so every grain, dealt or
                    // not, reads equally bright off its peak.
                    push_dust_halo(halos, geom, x, y, r, rgb, gcov);
                    if !glint {
                        continue;
                    }
                    if !push_twinkle_star(
                        out,
                        geom,
                        x as i32,
                        y as i32,
                        fine_star_arm,
                        glint_cov,
                        false,
                        0x00FF_FFFF,
                        Self::MAX_QUADS,
                    ) {
                        break 'flying;
                    }
                    continue;
                }
                // Fire: a flickering point of light wafting on turbulence and
                // shrinking as it cools; the hue seed doubles as temperature and
                // size.
                //
                // On a light ground a point of light cannot show and an
                // additive ember only lifts the letters toward the page, so an
                // ember is a deep round mote of source-over ink (an ember's
                // dull red, never its white-hot core) in the air, and is not
                // drawn over a probed line of text: text first.
                GlowStyle::Fire => {
                    // One radial halo supplies core and skirt. Velocity stretch
                    // elongates the ellipse, so a fast riser streaks and a
                    // drifting smolder stays round.
                    let flicker = 0.68
                        + 0.32 * (age * (9.0 + 7.0 * p.hue) + p.hue * std::f32::consts::TAU).sin();
                    let waft = (age * (2.0 + 3.0 * p.hue) + p.hue * std::f32::consts::TAU).sin()
                        * geom.cw as f32
                        * 0.6
                        * t;
                    if light_fire {
                        if covers(x + waft, y) {
                            continue;
                        }
                        // A spark on paper: a small round bead of ember
                        // ink, a dull red rim round a burnt-orange core
                        // (a fleck of a pixel or two read as a square).
                        let alpha = ((cov as f32) * flicker * 0.95).min(235.0) as u8;
                        let r = (ch * (0.06 + 0.06 * p.hue)).max(1.8) * (0.7 + 0.3 * fade);
                        push_paper_bead(out, geom, (x + waft, y), r, Self::LIGHT_EMBER_INKS, alpha);
                        continue;
                    }
                    // A spark is a point of light, never a blob: a small hot
                    // core in the fire's own orange to amber, cooling toward
                    // red as it dies (a wide dim halo of the same light read
                    // as a tan smudge over the dark).
                    let ccov_f = (cov as f32) * flicker * Self::FIRE_EMBER_COV;
                    if ccov_f < 1.0 {
                        continue;
                    }
                    let cell = geom.ch as f32;
                    let base = (cell * (Self::FIRE_EMBER_R + Self::FIRE_EMBER_R_HOT * p.hue))
                        .max(1.2)
                        * (0.6 + 0.4 * fade);
                    let vx = p.vx;
                    let vy = p.vy + p.gy * age; // live velocity
                    let rx = base * (1.0 + (vx.abs() / cell).min(1.4) * 0.6);
                    let ry = base * (1.0 + (vy.abs() / cell).min(1.4) * 0.6);
                    let core_color = fire_ramp((0.42 + 0.30 * fade + 0.18 * p.hue).min(0.92));
                    let (centre, radii) = ((x + waft, y), (rx * 1.6, ry * 1.6));
                    // Text first: a spark never lands on a line of text the
                    // frame shows, nor on the caret's edge (the block would
                    // cut it to a sliver). Light over a letter is a smear on
                    // it, and an Enter carries the run's sparks onto the line
                    // it just printed.
                    let reach = radii.0.max(radii.1);
                    let near = |dx: f32, dy: f32| covers(centre.0 + dx, centre.1 + dy);
                    if !self.streak_unbudgeted()
                        && (near(0.0, 0.0)
                            || near(-reach, 0.0)
                            || near(reach, 0.0)
                            || near(0.0, -reach)
                            || near(0.0, reach)
                            || on_caret(centre.0, centre.1))
                    {
                        continue;
                    }
                    // Whatever of its glow still reaches a letter spends the
                    // over-ink ledger, newest embers first.
                    let share = self.ink_ledger.admit_halo(
                        centre,
                        radii,
                        premul_rgb(core_color, ccov_f as u8),
                        InkLedger::FLOOR,
                    );
                    let ccov = (ccov_f * share) as u8;
                    if ccov == 0 {
                        continue;
                    }
                    push_halo(
                        halos,
                        geom,
                        centre.0,
                        centre.1,
                        radii.0,
                        radii.1,
                        premul_rgb(core_color, ccov),
                    );
                    continue;
                }
                // Sparkle: every grain is a night-sky shape chosen by a stable
                // per-particle seed (the birth hue, quantized so the rolling tint
                // never flips it): round stardust that twinkles, plain glitter,
                // the dealt four-point star (the one cross), the odd pale
                // crescent moon, and the occasional rainbow mini-comet. A small
                // constant per grain, bounded by [`Self::MAX_QUADS`].
                GlowStyle::Sparkle => {
                    let seed = (p.hue * 4096.0) as u32;
                    let (ix, iy) = (x as i32, y as i32);
                    // On a dark ground the shower is the gold comet's: its
                    // round grains a third brighter and larger than the
                    // family's starfield grain, so they read as sparks
                    // rather than dim grey specks, and its plain glitter
                    // in the comet's gold. A grain over a letter still
                    // spends the over-ink account where it lands.
                    let (lift, grow) = if dark {
                        (Self::SPARKLE_DARK_LIFT, Self::SPARKLE_DARK_GROW)
                    } else {
                        (1.0, 1.0)
                    };
                    let cov = (f32::from(cov) * lift).min(255.0) as u8;
                    match seed % 8 {
                        // Bucket 0 is re-dealt through [`star_accent`]
                        // (`seed % 16 == 0` ⊂ `seed % 8 == 0`): 1-in-16 grains are
                        // plus stars, the rest join the dust arm.
                        0 if star_accent(seed) => {
                            // Four-point star, twinkling on its own seeded phase.
                            let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                            let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                            if scov > 0 {
                                // Size rides the seed between the starfield grain
                                // at rest and the family default.
                                let ratio = STAR_ARM_FINE + (STAR_ARM_STD - STAR_ARM_FINE) * p.hue;
                                if !dark {
                                    // The family's light star, the same mark on
                                    // white, taxed by the ink ratio, in the
                                    // may-touch-text ink of its hue.
                                    rk::stardust::push_twinkle_over_twin(
                                        halos,
                                        geom,
                                        Self::MAX_HALOS,
                                        rk::stardust::LightStar {
                                            centre: (ix, iy),
                                            // Its size rides the seed, not a clock.
                                            mass: star_arm(ch, ratio) * STAR_ARM_INK,
                                            step: 0,
                                            rgb: Self::SPARKLE_PAPER_STAR,
                                            gold: false,
                                            cov: Self::paper_gold_alpha(scov),
                                        },
                                    );
                                    continue;
                                }
                                let arm = star_arm_px(star_arm(ch, ratio));
                                // Arms via the shared star; Sparkle's glint stays
                                // local (white, only at the twinkle peak).
                                if !push_twinkle_star(
                                    out,
                                    geom,
                                    ix,
                                    iy,
                                    arm,
                                    scov,
                                    false,
                                    particle_color(),
                                    Self::MAX_QUADS,
                                ) {
                                    break 'flying;
                                }
                                if twinkle_peak(twinkle) {
                                    // Diffraction glints at the brightest instant,
                                    // at the family's glint offset and dimming.
                                    let d = ((arm as f32 * STAR_GLINT).round() as i32).max(1);
                                    let glint = premul_rgb(
                                        0x00FF_FFFF,
                                        (f32::from(scov) * STAR_GLINT_COV) as u8,
                                    );
                                    for (gx, gy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
                                        if out.len() >= Self::MAX_QUADS {
                                            break 'flying;
                                        }
                                        push_rect(out, geom, ix + gx, iy + gy, 1, 1, glint);
                                    }
                                }
                            }
                            continue;
                        }
                        0..=3 => {
                            // Buckets 1-3 and bucket 0's undealt half: round dust
                            // with the same twinkle, priced at the plus's
                            // composited light ([`push_dust_halo`]).
                            let twinkle = twinkle_env(age, p.hue * std::f32::consts::TAU);
                            let scov = ((cov as f32) * twinkle).clamp(0.0, 255.0) as u8;
                            if scov > 0 {
                                let r = dust_r(ch, p.hue) * (0.6 + 0.4 * fade) * grow;
                                if dark {
                                    push_dust_halo(halos, geom, x, y, r, particle_color(), scov);
                                } else {
                                    push_paper_bead(
                                        out,
                                        geom,
                                        (x, y),
                                        r * Self::SPARKLE_PAPER_BEAD,
                                        (Self::SPARKLE_PAPER_RIM, Self::SPARKLE_PAPER_CORE),
                                        Self::paper_gold_alpha(scov),
                                    );
                                }
                            }
                            continue;
                        }
                        4 | 5 => {
                            // Plain glitter: the texture between the set pieces,
                            // one soft round grain (no hot core, so it stays
                            // quieter than the dust it sits among).
                            let r = (sz as f32 * 0.5).max(1.3);
                            if dark {
                                push_halo(
                                    halos,
                                    geom,
                                    x,
                                    y,
                                    r * grow,
                                    r * grow,
                                    premul_rgb(sparkle_gold_rgb.unwrap_or(0), cov),
                                );
                            } else {
                                push_paper_bead(
                                    out,
                                    geom,
                                    (x, y),
                                    r * Self::SPARKLE_PAPER_BEAD,
                                    (Self::SPARKLE_PAPER_RIM, Self::SPARKLE_PAPER_CORE),
                                    Self::paper_gold_alpha(cov),
                                );
                            }
                            continue;
                        }
                        6 => {
                            // The moon: a pale silver crescent ([`push_crescent`]),
                            // a western limb tapering into horns opening east,
                            // well under a glyph's height. On white it keeps its
                            // size (a solid mark needs no ink tax) in the silver's
                            // ink. A moon is a shape, not a glint: dimmed over a
                            // line of text it is only a grey smudge between the
                            // words, so it keeps off the text on a dark ground too.
                            if over_text {
                                continue;
                            }
                            let r = (Self::MOON_DUST_RATIO * dust_r(ch, p.hue)).max(2.5);
                            // A moon fades slowly, so it reads gold, not ash,
                            // until it is nearly gone.
                            let moon = (230.0
                                * p.cov_scale
                                * fade.powf(Self::MOON_FADE_POW)
                                * cfg.intensity
                                * lift)
                                .min(255.0) as u8;
                            if dark {
                                push_crescent(out, geom, (x, y), r, Self::MOON_GOLD, moon, false);
                            } else {
                                push_crescent(
                                    out,
                                    geom,
                                    (x, y),
                                    r,
                                    Self::SPARKLE_PAPER_MOON,
                                    Self::paper_gold_alpha(moon),
                                    true,
                                );
                            }
                            continue;
                        }
                        _ => {
                            // Mini-comet: a short rainbow streak along the
                            // grain's motion; one hue at both ends.
                            let dash = particle_color();
                            // On paper the dash is a stroke of ink, which
                            // needs a little more girth than a line of
                            // light to register.
                            let (thick, vy) = (mini_comet_girth, p.vy + p.gy * age);
                            if dark {
                                push_velocity_dash(
                                    out, geom, x, y, p.vx, vy, 0.08, thick, dash, dash, cov,
                                );
                            } else {
                                push_velocity_dash_over(
                                    out,
                                    geom,
                                    x,
                                    y,
                                    p.vx,
                                    vy,
                                    0.08,
                                    thick,
                                    Self::SPARKLE_PAPER_RIM,
                                    Self::paper_gold_alpha(cov),
                                );
                                // Its head a gold bead, so a slow one still
                                // reads as a mark, not a smear.
                                push_paper_bead(
                                    out,
                                    geom,
                                    (x, y),
                                    thick * 1.2,
                                    (Self::SPARKLE_PAPER_RIM, Self::SPARKLE_PAPER_CORE),
                                    Self::paper_gold_alpha(cov),
                                );
                            }
                            continue;
                        }
                    }
                }
                // Water: per-droplet size from the hue seed and a vertical
                // stretch with fall speed (a fast drop streaks); rising bubbles
                // stay small round beads. On a light ground, the drop is a
                // round source-over bead in the water's ink.
                _ => {
                    let (w, h) = if water {
                        let base = (ch * (0.055 + 0.065 * p.hue)).max(1.0);
                        let v_down = p.vy + p.gy * age;
                        let stretch = (v_down / ch).clamp(0.0, 1.6);
                        (
                            base.round() as i32,
                            (base * (1.0 + stretch)).round().max(1.0) as i32,
                        )
                    } else {
                        (sz, sz)
                    };
                    if !dark && water {
                        // A drop of clear water blue on paper, round, its rim
                        // a deeper blue (a slate mote read as grit).
                        let r = (w.max(h) as f32 * 0.5).max(1.0) * STAR_ARM_INK;
                        push_paper_bead(
                            out,
                            geom,
                            (x, y),
                            r,
                            Self::LIGHT_WATER_DROP_INKS,
                            ink_alpha(cov),
                        );
                        continue;
                    }
                    if !dark && lawful {
                        let r = (w.max(h) as f32 * 0.5).max(1.0) * STAR_ARM_INK;
                        push_light_mote(halos, geom, (x, y), r, particle_color(), ink_alpha(cov));
                        continue;
                    }
                    // A water droplet on a dark ground is a soft round bead of
                    // light drawn out as it falls, never a hard-edged chip.
                    if water {
                        let r = (w as f32 * 0.5 + 0.75).max(1.25);
                        let ry = (h as f32 * 0.5 + 0.75).max(r);
                        push_halo(
                            halos,
                            geom,
                            x,
                            y,
                            r * Self::WATER_DROP_SOFT,
                            ry * Self::WATER_DROP_SOFT,
                            premul_rgb(particle_color(), cov),
                        );
                        continue;
                    }
                    push_rect(
                        out,
                        geom,
                        x as i32 - w / 2,
                        y as i32 - h / 2,
                        w,
                        h,
                        premul_rgb(particle_color(), cov),
                    );
                }
            }
        }
    }

    // ----- helpers -----

    /// The light ground a body under the letters solves its ink against
    /// ([`LightGround::of`]). Its ink is unsolved (the bare vivid ink, never
    /// lifted for the theme's text) only when a test asks: the negative
    /// control of the light-body legibility tests.
    #[inline]
    pub(super) const fn light_ground(&self, cfg: &GlowConfig) -> LightGround {
        let ground = LightGround::of(cfg);
        #[cfg(test)]
        let ground = LightGround {
            unsolved: self.light_body_unsolved,
            ..ground
        };
        ground
    }

    /// Whether a test asked for the light bodies' inks and ceilings unsolved
    /// ([`Self::light_ground`]); never in a build.
    #[cfg(test)]
    pub(super) const fn light_body_unsolved(&self) -> bool {
        self.light_body_unsolved
    }
    #[cfg(not(test))]
    #[inline(always)]
    pub(super) const fn light_body_unsolved(&self) -> bool {
        false
    }

    /// Whether the dark bed is laid at each stop's full value rather than
    /// solved to the bar: never in a build, and only when a test asks (the
    /// bed's bar test's negative control).
    #[cfg(test)]
    pub(super) const fn bed_unsolved(&self) -> bool {
        self.bed_unsolved
    }
    #[cfg(not(test))]
    #[inline(always)]
    pub(super) const fn bed_unsolved(&self) -> bool {
        false
    }

    /// Whether the frame keeps its dark bed but no over-ink account: never
    /// in a build, and only when a test asks (the account's negative
    /// control).
    #[cfg(test)]
    pub(super) const fn over_ink_idle(&self) -> bool {
        self.over_ink_idle
    }
    #[cfg(not(test))]
    #[inline(always)]
    pub(super) const fn over_ink_idle(&self) -> bool {
        false
    }

    /// Whether the dark streak skips its over-ink budget (and fire its
    /// over-ink ledger and its steam's stacking budget): never in a build,
    /// and only when a test asks (the contrast tests' negative control).
    #[cfg(test)]
    pub(super) const fn streak_unbudgeted(&self) -> bool {
        self.streak_unbudgeted
    }
    #[cfg(not(test))]
    #[inline(always)]
    pub(super) const fn streak_unbudgeted(&self) -> bool {
        false
    }

    /// The highest coverage at which a light whose axis lifts each channel by
    /// `lift` per unit coverage (a laser crackle across a letter) still
    /// leaves the reference stroke [`Self::STREAK_INK_FLOOR`] of contrast
    /// (`0..=255`; 255 when even full coverage holds it). Ink and ground are lifted together and clip at
    /// 255, as `add_sat` composites them; the floor is found by bisection,
    /// since the clip makes the ratio piecewise.
    fn over_ink_ceiling(lift: [f32; 3]) -> f32 {
        let table = crate::color_math::relative_luminance_table();
        let unpack = |rgb: u32| [16u32, 8, 0].map(|sh| ((rgb >> sh) & 0xff) as f32);
        let ref_ink = unpack(Self::STREAK_REF_INK);
        let ref_ground = unpack(Self::STREAK_REF_GROUND);
        let lifted = |base: [f32; 3], cov: f32| {
            let ch = |c: usize| (base[c] + cov * lift[c]).min(255.0) as u32;
            crate::color_math::relative_luminance_with(table, (ch(0) << 16) | (ch(1) << 8) | ch(2))
        };
        let holds = |cov: f32| {
            let (ink, ground) = (lifted(ref_ink, cov), lifted(ref_ground, cov));
            ink.max(ground) + 0.05 >= Self::STREAK_INK_FLOOR * (ink.min(ground) + 0.05)
        };
        if holds(255.0) {
            return 255.0;
        }
        // Ten halvings resolve the ceiling well inside one coverage level.
        let (mut lo, mut hi) = (0.0f32, 255.0f32);
        for _ in 0..10 {
            let mid = 0.5 * (lo + hi);
            if holds(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The comet colour at path position `pos` (0 tail .. 1 head) for the style.
    pub(super) fn comet_color(&self, cfg: &GlowConfig, pos: f32) -> u32 {
        // Sparkle is a gold comet on either ground: its head, the bridge onto
        // the caret, the rail, every typed cell and a leap's whole tail burn
        // in the gold family (on paper, the band under the newest letters in
        // its ink), its gold at the head a shade deeper toward the tail;
        // only its grains are dealt from the wheel.
        if matches!(cfg.style, GlowStyle::Sparkle) {
            return Self::sparkle_gold_at(cfg, pos);
        }
        let base = style_comet_color(cfg.style, cfg.color, cfg.accent, self.hue, pos);
        // Torpedo crest: sustained fast typing churns the head toward white
        // foam. Live animator only; the settings demo stays calm.
        if matches!(cfg.style, GlowStyle::Water) && self.heat > 0.0 {
            return lerp_rgb(base, water_ramp(1.0), 0.5 * self.heat * pos);
        }
        // Fire: the wake climbs from ember-orange toward the hot head with the
        // eased temperature, capped at 0.92 on the ramp so it never washes glyphs.
        if matches!(cfg.style, GlowStyle::Fire) {
            return fire_ramp((pos * (0.50 + 0.45 * self.fire_t())).min(0.92));
        }
        // The phaser's spectrum on paper steps over the hues that can only
        // read there as khaki or olive ([`crate::color_math::light_spectrum_hue`]).
        if matches!(cfg.style, GlowStyle::Phaser) && !cfg.dark_theme {
            return hsv2rgb(
                crate::color_math::light_spectrum_hue((self.hue + pos).fract()),
                0.95,
                1.0,
            );
        }
        base
    }

    /// Sparkle's gold on a dark ground: its configured colour (starlight
    /// gold by default) at a warm gold's chroma, so the comet reads gold, not
    /// cream.
    #[inline]
    pub(super) const fn sparkle_gold(cfg: &GlowConfig) -> u32 {
        crate::spectrum::spectrum_with_min_saturation(cfg.color & 0x00FF_FFFF, 0.72)
    }

    /// Sparkle's gold along its tail, `pos` 0 (tail) to 1 (head): its gold
    /// at the head, a shade deeper at the tail, never leaving the gold
    /// family (a warmer tail fading over the dark read as rust and brown).
    #[inline]
    pub(super) fn sparkle_gold_at(cfg: &GlowConfig, pos: f32) -> u32 {
        let gold = Self::sparkle_gold(cfg);
        let (h, s, v) = crate::color_math::rgb2hsv(gold);
        let k = 1.0 - pos.clamp(0.0, 1.0);
        let k = k * k * (3.0 - 2.0 * k);
        crate::color_math::hsv2rgb(h - 4.0 * k, (s + 0.05 * k).min(1.0), v * (1.0 - 0.08 * k))
    }

    /// A sparkle mark's opacity on paper for a dark coverage request: a
    /// fresh mark is all but opaque (it is never laid over the text, so it
    /// needs no may-touch-text ceiling), and a fading one thins with it.
    #[inline]
    pub(super) const fn paper_gold_alpha(cov: u8) -> u8 {
        (cov as f32 * 1.25).min(240.0) as u8
    }
}

/// The fire's heat smoothed along its line: a burning cell's share at `d`
/// cells from its centre, a raised cosine to nothing a cell out. The shares
/// of cells a cell apart sum to one, so the heat between two burning cells
/// runs smoothly from one's to the other's and a run's ends fall off over a
/// cell, never standing square on a cell edge ([`flame_heat`]).
#[inline]
fn flame_kernel(d: f32) -> f32 {
    let d = d.abs();
    if d >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * d).cos())
    }
}

/// The fire's heat at column position `u` (cells, from the grid's left
/// edge) along the burning `cells` of one row ([`flame_kernel`]).
#[inline]
fn flame_heat<'a>(cells: impl Iterator<Item = &'a (u16, u16, f32)>, u: f32) -> f32 {
    cells
        .map(|c| c.2 * flame_kernel(u - (f32::from(c.1) + 0.5)))
        .sum::<f32>()
        .min(1.0)
}

/// The dark flame field's ceiling as a share of its whole, at `rise` (0 at
/// the root, 1 at the tallest tongue's tip): whole over the lower third,
/// thinning to a quarter at the tips, so the light dies upward.
#[inline]
fn flame_tip_fade(rise: f32) -> f32 {
    const HOLD: f32 = 0.33;
    if rise <= HOLD {
        1.0
    } else {
        1.0 - 0.75 * ((rise - HOLD) / (1.0 - HOLD)).clamp(0.0, 1.0)
    }
}

/// The light ink-fire's warm root through its gold tip: one source for
/// the colour lookup and the admission envelope.
const FLAME_LIGHT_STOPS: [(f32, u32); 4] = [
    (0.0, 0x00FF_6F0A),
    (0.32, 0x00FF_8C14),
    (0.62, 0x00FF_A820),
    (1.0, 0x00FF_C848),
];

/// Channel bounds of the actual ramp stops: interpolation and the tongues'
/// opacity-weighted colour mixture stay in the same box. Derived from the
/// ramp itself, so a different or nonmonotone palette keeps the admission.
#[inline]
fn flame_light_paint_bounds() -> (u32, u32) {
    FLAME_LIGHT_STOPS
        .iter()
        .fold((0x00FF_FFFF, 0), |(lo, hi), &(_, rgb)| {
            (chan_min(lo, rgb), chan_max(hi, rgb))
        })
}

/// The light ink-fire's colour at `rise` (0 at the root, 1 at a tongue's
/// tip), interpolated within the same stops the cap's envelope reads: a
/// warm orange root going amber and gold toward the tip (a pale yellow tip
/// vanished on white, so the tongue read a row shorter than it stood).
#[inline]
fn flame_light_ramp(rise: f32) -> u32 {
    let rise = rise.clamp(0.0, 1.0);
    let i = FLAME_LIGHT_STOPS
        .iter()
        .rposition(|s| s.0 <= rise)
        .unwrap_or(0)
        .min(FLAME_LIGHT_STOPS.len() - 2);
    let (a, b) = (FLAME_LIGHT_STOPS[i], FLAME_LIGHT_STOPS[i + 1]);
    lerp_rgb(a.1, b.1, (rise - a.0) / (b.0 - a.0))
}

/// Fire's streak lift per unit coverage on each band of a cell
/// ([`InkLedger::BANDS`]), its axis on the cell's middle row: each band takes
/// the layers of `stack` (at `core` px of core) whose half-thickness, plus a
/// pixel of anti-aliasing, reaches the band's nearest point, in `color` mixed
/// as the rasterizer mixes it at `pos` (`aterm_render::layer_stack_lift`).
fn fire_band_lifts(
    stack: &[(f32, f32, u32, f32, f32, usize)],
    color: u32,
    pos: f32,
    core: f32,
    ch: f32,
) -> BandLifts {
    let reaches: [f32; InkLedger::BANDS] =
        std::array::from_fn(|band| InkLedger::band_reach(ch, band));
    let mut lifts = [[0.0f32; 3]; InkLedger::BANDS];
    for layer in stack {
        let reach_max = 0.5 * layer.0 * core + 1.0;
        let one = layer_stack_lift(std::slice::from_ref(layer), color, pos);
        for (band, lift) in lifts.iter_mut().enumerate() {
            if reach_max >= reaches[band] {
                for (l, o) in lift.iter_mut().zip(one) {
                    *l += o;
                }
            }
        }
    }
    lifts
}

/// The bloom-layer stack `style` draws its streak with (and charges its
/// over-ink budget against): each beam style's own, the comet's for the rest.
#[inline]
const fn streak_layers(style: GlowStyle) -> &'static [(f32, f32, u32, f32, f32, usize)] {
    match style {
        GlowStyle::Phaser => &PHASER_LAYERS,
        GlowStyle::Beam => &BEAM_LAYERS,
        GlowStyle::Laser => &LASER_LAYERS,
        GlowStyle::Sparkle => &SPARKLE_LAYERS,
        _ => &COMET_LAYERS,
    }
}

/// The comet's run pools ([`CursorGlow`]'s `comet_runs` and `comet_run`)
/// and, sample for sample beside them, whether typing laid each sample
/// (`comet_typing`, `comet_run_typing`).
struct CometRuns<'a> {
    runs: &'a mut Vec<Vec<CometSample>>,
    run: &'a mut Vec<CometSample>,
    typing: &'a mut Vec<Vec<bool>>,
    run_typing: &'a mut Vec<bool>,
}

impl CometRuns<'_> {
    /// Add `sample` (laid by typing or not) to the open run, first publishing
    /// that run where `here` is not beside the last swept cell, so a hide/show
    /// gap or a stale older group never connects across empty space.
    fn chain(
        &mut self,
        runs_len: &mut usize,
        prev: Option<(u16, u16)>,
        here: (u16, u16),
        sample: CometSample,
        typing: bool,
    ) {
        if breaks_comet_run(prev, here) {
            self.publish(runs_len);
        }
        self.run.push(sample);
        self.run_typing.push(typing);
    }

    /// [`publish_comet_run`], publishing the kinds at the same watermark.
    fn publish(&mut self, runs_len: &mut usize) {
        if self.run.is_empty() {
            return;
        }
        let at = *runs_len;
        while self.typing.len() <= at {
            self.typing.push(Vec::new());
        }
        std::mem::swap(&mut self.typing[at], self.run_typing);
        self.run_typing.clear();
        publish_comet_run(self.runs, self.run, runs_len);
    }
}

/// Whether a swept cell `here` after `prev` starts a new run: it is not
/// beside the last.
fn breaks_comet_run(prev: Option<(u16, u16)>, here: (u16, u16)) -> bool {
    prev.is_some_and(|(pr, pc)| {
        (here.0 as i32 - pr as i32)
            .abs()
            .max((here.1 as i32 - pc as i32).abs())
            > 1
    })
}

/// Publish the open comet `run`, when it holds a sample, at the pool
/// watermark `runs_len`. Swapped rather than taken, so the next run reuses
/// the pooled buffer's capacity.
#[inline]
fn publish_comet_run(
    runs: &mut Vec<Vec<CometSample>>,
    run: &mut Vec<CometSample>,
    runs_len: &mut usize,
) {
    if run.is_empty() {
        return;
    }
    if *runs_len == runs.len() {
        runs.push(Vec::new());
    }
    std::mem::swap(&mut runs[*runs_len], run);
    *runs_len += 1;
}
