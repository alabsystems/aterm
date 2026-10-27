// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The glow's state words (`crate::state_words`): its typed-input custody (the held park, the
//! type and gesture hints, the press ledger, the recent typed run, the hand's row and the print
//! anchor), the sound cues it owes, every stored [`GlowConfig`] (through
//! [`crate::state_words::glow_cfg_words`]), then the ink ledger's and the v2 engine's words. The
//! other styles' particle pools and the frame's output buffers are not hashed.

use super::{CursorGlow, GlowConfig, HeldPark, OutgoingFade, RecentTypedRun};
use crate::rainbow_kitty as rk;
use crate::state_words::{Fold, Words, geom_words, glow_cfg_words, sound_cue_words};
use aterm_time::Instant;

impl CursorGlow {
    /// The custody, the cues, the stored configs, the ink ledger and the v2 engine.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.list("glow.type_hint", self.type_hint.slots, |f, s| {
            f.at_opt("at", s);
        });
        w.group("glow.type_press_ring", |f| {
            let ring = &self.type_press_ring;
            f.list("slots", ring.slots, |f, slot| {
                f.opt(
                    "slot",
                    slot,
                    |f, (at, cells, glyph): (Instant, u8, Option<char>)| {
                        f.at("at", at);
                        f.put("cells", u64::from(cells));
                        f.put(
                            "glyph",
                            match glyph {
                                Some(c) => u64::from(u32::from(c)),
                                None => u64::MAX,
                            },
                        );
                    },
                );
            });
            f.put("occupied.lo", ring.occupied as u64);
            f.put("occupied.hi", (ring.occupied >> 64) as u64);
            f.put("head", ring.head as u64);
            f.at_opt("held_through", ring.held_through);
            f.flag("admit_held", ring.admit_held);
        });
        w.at_opt("glow.last_committed_type", self.last_committed_type);
        w.opt("glow.held_park", self.held_park.as_ref(), held_park_words);
        w.opt("glow.torn_lay", self.torn_lay, |f, t| {
            f.put("row", u64::from(t.row));
            f.put("col0", u64::from(t.col0));
            f.put("col1", u64::from(t.col1));
            f.cell_opt("seen", t.seen);
            f.at("moved_at", t.moved_at);
            f.opt("hand_before", t.hand_before, |f, h| {
                f.put("row", u64::from(h.row));
                f.u16_opt("caret_row", h.caret_row);
            });
            f.opt("unowned_before", t.unowned_before, |f, u| {
                f.put("row", u64::from(u.row));
                f.flag("parked", u.parked);
            });
            f.put("anchor_seq", t.anchor_seq.unwrap_or(u64::MAX));
            f.flag("anchor_seq.some", t.anchor_seq.is_some());
            f.flag("printed_off_row", t.printed_off_row);
        });
        w.opt(
            "glow.recent_typed_run",
            self.recent_typed_run.as_ref(),
            recent_typed_run_words,
        );
        w.at_opt("glow.nav_hint", self.nav_hint);
        w.at_opt("glow.return_hint", self.return_hint);
        w.at_opt("glow.user_gesture_hint", self.user_gesture_hint);
        w.at_opt("glow.newline_hint", self.newline_hint);
        w.at_opt("glow.reflow_hint", self.reflow_hint);
        w.at_opt("glow.kill_hint", self.kill_hint);
        w.flag("glow.kill_hint_word", self.kill_hint_word);
        w.at_opt("glow.kill_retreat_pending", self.kill_retreat_pending);
        w.at_opt("glow.kill_reported_to_v2", self.kill_reported_to_v2);
        w.at_opt("glow.bs_poof_hint", self.bs_poof_hint);
        w.cell_opt("glow.bs_baseline", self.bs_baseline);
        w.at_opt("glow.blink_hint", self.blink_hint);
        w.at_opt("glow.quench_hint", self.quench_hint);
        w.opt("glow.last_erase", self.last_erase, |f, e| {
            f.at("at", e.at);
            f.list("retired", e.retired(), |f, &c| {
                f.put("glyph", u64::from(u32::from(c)));
            });
            f.put("owed", u64::from(e.owed));
        });
        w.group("glow.insert", |f| {
            let seam = &self.insert;
            f.flag("armed", seam.armed.is_some());
            f.flag("span", seam.span.is_some());
            f.flag("pending_hop", seam.pending_hop.is_some());
            f.flag("orphans", seam.orphans.is_some());
            f.flag("orphan_exact", seam.orphan_exact.is_some());
            f.put("tally.delivered", seam.tally.delivered);
            f.put("tally.lit", seam.tally.lit);
            f.put("tally.retracted", seam.tally.retracted);
            f.put("tally.last_cells", u64::from(seam.tally.last_cells));
        });
        w.group("glow.in_flight_tally", |f| {
            let t = &self.in_flight_tally;
            f.put("licensed", t.licensed);
            f.put("forgotten", t.forgotten);
            f.put("credits", t.credits as u64);
            f.put("swallowed_no_echo", t.swallowed_no_echo);
            f.put("park_returns", t.park_returns);
            f.put("park_flushed", t.park_flushed);
        });
        w.cell_opt("glow.park_source_intact", self.park_source_intact);
        w.opt(
            "glow.carried_key_move",
            self.carried_key_move,
            |f, (from, to): ((u16, u16), (u16, u16))| {
                f.cell("from", from);
                f.cell("to", to);
            },
        );
        w.list(
            "glow.park_source_cells",
            &self.park_source_cells,
            |f, &c| {
                f.put("glyph", u64::from(u32::from(c)));
            },
        );
        w.flag("glow.park_source_confirmed", self.park_source_confirmed);
        w.opt(
            "glow.last_licensed_row",
            self.last_licensed_row,
            |f, (row, at): (u16, Instant)| {
                f.put("row", u64::from(row));
                f.at("at", at);
            },
        );
        w.opt("glow.hand_row", self.hand_row, |f, h| {
            f.put("row", u64::from(h.row));
            f.u16_opt("caret_row", h.caret_row);
        });
        w.opt("glow.unowned_caret", self.unowned_caret, |f, u| {
            f.put("row", u64::from(u.row));
            f.flag("parked", u.parked);
        });
        w.opt(
            "glow.print_anchor",
            self.print_anchor,
            |f, (row, col, seq): (u16, u16, u64)| {
                f.cell("at", (row, col));
                f.put("seq", seq);
            },
        );
        w.put(
            "glow.print_anchor_tick_seq",
            self.print_anchor_tick_seq.unwrap_or(u64::MAX),
        );
        w.put(
            "glow.print_anchor_glyph",
            match self.print_anchor_glyph {
                Some(c) => u64::from(u32::from(c)),
                None => u64::MAX,
            },
        );
        w.put("glow.print_anchor_seen", self.print_anchor_seen);
        w.flag("glow.caret_hidden", self.caret_hidden);
        w.list("glow.anchor_rows", self.anchor_rows, |f, row| {
            f.opt("row", row, |f, r| {
                f.put("row", u64::from(r.row));
                f.put("end", u64::from(r.end));
                f.flag("tainted", r.tainted);
                f.put("touched", r.touched);
            });
        });
        w.put("glow.anchor_rows_clock", self.anchor_rows_clock);
        w.opt(
            "glow.last_anchor_sweep",
            self.last_anchor_sweep,
            |f, (row, at): (u16, Instant)| {
                f.put("row", u64::from(row));
                f.at("at", at);
            },
        );
        w.u16_opt("glow.wrap_paid_row", self.wrap_paid_row);
        w.cell_opt("glow.last", self.last);
        w.opt(
            "glow.last_visible",
            self.last_visible,
            |f, (cell, at): ((u16, u16), Instant)| {
                f.cell("cell", cell);
                f.at("at", at);
            },
        );
        w.cell_opt("glow.hide_bridge_shown", self.hide_bridge_shown);
        w.at_opt("glow.last_move", self.last_move);
        w.at_opt("glow.last_type", self.last_type);
        w.put("glow.spawns", self.spawns);
        w.put("glow.coord_band_moves", self.coord_band_moves);
        w.put("glow.last_ch", u64::from(self.last_ch));
        w.put("glow.last_origin_y", u64::from(self.last_origin_y));
        w.flag("glow.ctx_alt", self.ctx_alt);
        w.cell_opt("glow.pane_columns", self.pane_columns);
        w.flag("glow.reduced_motion", self.reduced_motion);
        w.group("glow.momentum", |f| self.momentum.state_words(f));
        w.group("glow.erase_mom", |f| self.erase_mom.state_words(f));
        w.f32("glow.heat", self.heat);
        w.at_opt("glow.heat_at", self.heat_at);
        w.at_opt("glow.crown_until", self.crown_until);
        w.put("glow.crown_window_ms", self.crown_window_ms);
        w.put("glow.keyed_clicks", u64::from(self.keyed_clicks));
        w.at_opt("glow.keyed_click_at", self.keyed_click_at);
        w.at_opt("glow.key_cue_at", self.key_cue_at);
        w.list("glow.sound_cues", &self.sound_cues, |f, cue| {
            sound_cue_words(f, cue);
        });
        w.list("glow.v2_cues", &self.v2_cues, |f, cue| {
            sound_cue_words(f, cue)
        });
        w.group("glow.v2_caret", |f| rk::caret_seam_words(f, self.v2_caret));
        w.opt(
            "glow.v2_stop",
            self.v2_stop,
            |f, (t, cell): (f32, Option<(u16, u16)>)| {
                f.f32("t", t);
                f.cell_opt("cell", cell);
            },
        );
        w.flag("glow.flow_open", self.flow_open);
        w.flag("glow.dark_settled", self.dark_settled);
        w.at_opt("glow.ramp_in_at", self.ramp_in_at);
        w.put(
            "glow.last_style",
            match self.last_style {
                Some(s) => s as u64,
                None => u64::MAX,
            },
        );
        w.flag("glow.last_classic_mono", self.last_classic_mono);
        w.flag("glow.last_classic_dark", self.last_classic_dark);
        w.at_opt("glow.dark_rim_hidden", self.dark_rim_hidden);
        w.opt(
            "glow.last_cfg",
            self.last_cfg.as_ref(),
            |f, cfg: &GlowConfig| {
                glow_cfg_words(f, cfg);
            },
        );
        w.list("glow.fading", &self.fading, fade_words);

        self.ink_ledger.state_words(w);
        self.v2.state_words(w);
    }

    /// The bed's ink table the v2 ribbon draws by ([`rk::ribbon::Ribbon::bed_ink_lut`]).
    #[inline]
    #[must_use]
    pub const fn bed_ink_lut(&self) -> &rk::ribbon::BedInkLut {
        self.v2.bed_ink_lut()
    }
}

/// A held park's every field, its stored config through the one helper.
#[inline]
fn held_park_words(f: &mut Fold, p: &HeldPark) {
    f.put("row", u64::from(p.row));
    f.put("landing_row", u64::from(p.landing_row));
    f.flag("cross_row", p.cross_row);
    f.put("origin", u64::from(p.origin));
    f.put("landing", u64::from(p.landing));
    f.at("at", p.at);
    f.group("cfg", |f| glow_cfg_words(f, &p.cfg));
    f.group("geom", |f| geom_words(f, &p.geom));
    f.flag("landing_glyph", p.landing_glyph);
    f.put(
        "landing_cell",
        match p.landing_cell {
            Some(c) => u64::from(u32::from(c)),
            None => u64::MAX,
        },
    );
    f.flag("content_followed", p.content_followed);
    f.put("press_mask.lo", p.press_mask as u64);
    f.put("press_mask.hi", (p.press_mask >> 64) as u64);
    f.put("v2_typed_before", p.v2_typed_before as u64);
}

/// The recent typed run's words: its span, its glyphs, its stamp and its walk.
#[inline]
fn recent_typed_run_words(f: &mut Fold, r: &RecentTypedRun) {
    f.put("row", u64::from(r.row));
    f.put("col0", u64::from(r.col0));
    f.put("len", u64::from(r.len));
    let len = usize::from(r.len).min(r.glyphs.len());
    f.list("glyphs", &r.glyphs[..len], |f, &c| {
        f.put("glyph", u64::from(u32::from(c)));
    });
    f.at("at", r.at);
    f.opt("origin", r.origin, rk::ribbon::run_origin_words);
}

/// An outgoing fade's words: its config through the one helper, its clock, and the fading
/// engine's own words.
#[inline]
fn fade_words(f: &mut Fold, fade: &OutgoingFade) {
    f.group("cfg", |f| glow_cfg_words(f, &fade.cfg));
    f.cell_opt("anchor", fade.anchor);
    f.at("started", fade.started);
    f.f32("level0", fade.level0);
    f.group("engine", |f| fade.engine.state_words(f));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::Duration;

    fn words(g: &CursorGlow, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        g.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_glow_custody_field_moves_the_glow_words() {
        let t0 = Instant::now();
        let mut glow = CursorGlow::default();
        glow.note_typed_cells(t0 + Duration::from_millis(2), 1);
        let base = words(&glow, t0);
        assert_eq!(
            base,
            words(&glow, t0),
            "the words are a function of the state"
        );

        glow.nav_hint = Some(t0 + Duration::from_millis(5));
        let hinted = words(&glow, t0);
        assert_ne!(hinted, base, "a type or gesture hint is hashed");
        glow.nav_hint = None;
        assert_eq!(words(&glow, t0), base);

        glow.recent_typed_run = Some(RecentTypedRun {
            row: 1,
            col0: 2,
            len: 1,
            glyphs: [' '; super::super::custody::RECENT_TYPED_RUN_CAP],
            at: t0,
            origin: None,
        });
        let run = words(&glow, t0);
        assert_ne!(run, base, "the recent typed run is hashed");
        if let Some(r) = glow.recent_typed_run.as_mut() {
            r.glyphs[0] = 'x';
        }
        assert_ne!(words(&glow, t0), run, "the run's glyphs are hashed");
        if let Some(r) = glow.recent_typed_run.as_mut() {
            r.glyphs[0] = ' ';
            r.glyphs[5] = 'x';
        }
        assert_eq!(
            words(&glow, t0),
            run,
            "glyphs past the run's length are not"
        );
        glow.recent_typed_run = None;

        glow.last_cfg = Some(GlowConfig::sample_for_test());
        let configured = words(&glow, t0);
        assert_ne!(configured, base, "the stored config is hashed");
        if let Some(cfg) = glow.last_cfg.as_mut() {
            cfg.theme_bg ^= 1;
        }
        assert_ne!(words(&glow, t0), configured, "through every field");
        glow.last_cfg = None;

        glow.under_out.push(aterm_render::GlowQuad::default());
        assert_eq!(words(&glow, t0), base, "the frame's output is not state");
    }
}
