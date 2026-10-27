// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sparkle Words' state words (`crate::state_words`): the occurrences on glass and the ink the
//! scan took with them (their glyph colours and the pet's ink spans), the word episodes (by key,
//! so the map's order never reads), the last rescan's births, the done marks (oldest first), the
//! ignitions reserved, the live novas and supernovas, the typed-edit witness, the trick flash, the
//! cursor kitty's placement and the clocks the word pass keeps.
//!
//! Not hashed: the scan's scratch (the alignment's and the nova coupling's with it, each rebuilt
//! by the pass that reads it), the bakers' tiles, the paint caches and the custom kitty art, the
//! panes parked away, the cue lists (each pass clears its sightings, curse cues and peek cues
//! before it fills them, for the host to drain), a test's grace override, and what the host
//! sets each frame (the pane's pixel origin, whether it brackets frames, and whether its frame
//! has drawn the companion body yet). The bound pane, the scanned session and the pane each
//! record names are hashed only by presence: their ids count the process's terminals.

use super::{
    CatFootprint, DONE_MARK_NONE, Episode, IgnitionReservation, KittyCursorPlacementState,
    MAX_OCCURRENCES, NovaLive, Occurrence, PERSIST_CAP, PendingBirth, SuperLive, TypedEdit,
    WordDecorations,
};
use crate::kitty_cursor::CatFoldDirection;
use crate::state_words::{Fold, Words};
use aterm_time::Instant;

impl WordDecorations {
    /// The occurrences and their ink, the episodes, the births, the done marks, the reservations,
    /// the live bursts, the witness, the flash, the cursor kitty and the clocks.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.put("words.occ.n", self.occ.len() as u64);
        w.list("words.occ", &self.occ, occurrence_words);
        w.list(
            "words.ink.base_fg",
            &self.ink_base_fg,
            |f, rgb: &[u8; 3]| {
                f.put(
                    "rgb",
                    u64::from(rgb[0]) << 16 | u64::from(rgb[1]) << 8 | u64::from(rgb[2]),
                );
            },
        );
        w.list("words.ink.cols", &self.ink_cols, |f, &col| {
            f.put("col", u64::from(col));
        });
        w.list("words.pet_ink_spans", &self.pet_ink_spans, |f, &(a, b)| {
            f.put("first", u64::from(a));
            f.put("end", u64::from(b));
        });
        let mut keys_buf = [0u64; PERSIST_CAP];
        let mut keys_len = 0usize;
        for &k in self.persist.keys() {
            if keys_len < keys_buf.len() {
                keys_buf[keys_len] = k;
                keys_len += 1;
            }
        }
        let keys = &mut keys_buf[..keys_len];
        keys.sort_unstable();
        w.put("words.episodes.n", keys.len() as u64);
        w.list("words.episodes", &*keys, |f, key| {
            f.put("key", *key);
            if let Some(ep) = self.persist.get(key) {
                episode_words(f, ep);
            }
        });
        let mut ord_buf = [(0u64, 0u64); MAX_OCCURRENCES];
        let mut ord_len = 0usize;
        for (&k, &v) in &self.seed_ordinals {
            if ord_len < ord_buf.len() {
                ord_buf[ord_len] = (k, v);
                ord_len += 1;
            }
        }
        let ordinals = &mut ord_buf[..ord_len];
        ordinals.sort_unstable();
        w.list("words.seed_ordinals", &*ordinals, |f, &(k, v)| {
            f.put("key", k);
            f.put("ordinal", v);
        });
        w.list("words.pending", &self.pending, |f, b: &PendingBirth| {
            f.put("occ_idx", u64::from(b.occ_idx));
            f.put("ctx_fp", b.ctx_fp);
            f.flag("live_caret_completion", b.live_caret_completion);
            f.flag("at_live_cursor", b.at_live_cursor);
        });
        w.put("words.done_marks.n", self.done_marks.len() as u64);
        w.group("words.done_marks", |fold| {
            let mut n = 0_u64;
            let mut at = self.done_marks.oldest;
            let cap = self.done_marks.nodes.len();
            while at != DONE_MARK_NONE && (n as usize) < cap {
                let node = self.done_marks.nodes[at as usize];
                fold.group("item", |f| {
                    f.put("key", node.key);
                });
                n += 1;
                at = node.newer;
            }
            fold.put("len", n);
        });
        w.list(
            "words.ignitions",
            &self.ignitions,
            |f, r: &IgnitionReservation| {
                f.flag("pane", r.pane.is_some());
                f.put("owner", r.owner);
                f.at("start", r.start);
                f.put("center.x", r.center.0 as u32 as u64);
                f.put("center.y", r.center.1 as u32 as u64);
            },
        );
        w.list("words.novas", &self.novas, |f, n: &NovaLive| {
            f.put("idx", u64::from(n.idx));
            f.at("start", n.start);
            f.put("center.x", n.center_px.0 as u32 as u64);
            f.put("center.y", n.center_px.1 as u32 as u64);
            f.f32("r_max", n.r_max);
        });
        w.list("words.supers", &self.supers, |f, s: &SuperLive| {
            f.put("idx", u64::from(s.idx));
            f.at("start", s.start);
            f.put("center.x", s.center_px.0 as u32 as u64);
            f.put("center.y", s.center_px.1 as u32 as u64);
            f.f32("r_max", s.r_max);
        });
        w.put("words.last_epoch", self.last_epoch);
        w.flag("words.have_scanned", self.have_scanned);
        w.put("words.cols", u64::from(self.cols));
        w.put("words.frame", self.frame);
        w.at_opt("words.active_until", self.active_until);
        w.at_opt("words.frozen_at", self.frozen_at);
        w.at_opt("words.settle_until", self.settle_until);
        w.flag("words.away", self.away);
        w.flag("words.spend_next_births", self.spend_next_births);
        w.put("words.rescan_seq", self.rescan_seq);
        w.put(
            "words.companion_claim",
            self.companion_claim.unwrap_or(u64::MAX),
        );
        w.opt(
            "words.caret_word",
            self.caret_word,
            |f, (key, cell): (u64, (u16, u16))| {
                f.put("key", key);
                f.cell("cell", cell);
            },
        );
        w.cell_opt("words.last_caret", self.last_caret);
        w.at_opt("words.super_until", self.super_until);
        w.at_opt("words.burst_hint", self.burst_hint);
        w.put("words.birth_seq", self.birth_seq);
        w.list("words.combo_births", &self.combo_births, |f, &at| {
            f.at("at", at)
        });
        w.opt("words.typed_edit", self.typed_edit, |f, e: TypedEdit| {
            f.at("at", e.at);
            f.cell_opt("cell", e.cell);
            f.flag("pane", e.pane.is_some());
            f.flag("spent", e.spent);
        });
        w.group("words.trick_flash", |f| self.trick_flash.state_words(f));
        w.group("words.kitty_cursor", |f| {
            kitty_cursor_words(f, &self.kitty_cursor_placement);
        });
        w.flag("words.bound", self.bound.is_some());
        w.flag("words.scan_session", self.scan_session.is_some());
        w.put("words.kitty_gen", self.kitty_gen);
        w.flag("words.kitty_disabled", self.kitty_disabled);
    }
}

/// The cursor kitty's placement: its key, the footprint and sprite it last drew at, and the fold
/// under way.
#[inline]
fn kitty_cursor_words(f: &mut Fold, k: &KittyCursorPlacementState) {
    f.opt("key", k.key, |f, key| {
        f.flag("scope", key.scope.is_some());
        f.put("cell_w", u64::from(key.cell_w));
        f.put("cell_h", u64::from(key.cell_h));
        f.put("rows", u64::from(key.rows));
        f.put("cols", u64::from(key.cols));
        f.put("source", key.source);
    });
    f.opt("last_target", k.last_target, footprint_words);
    f.opt("last_sprite", k.last_sprite, sprite_words);
    f.at_opt("last_emitted_at", k.last_emitted_at);
    f.opt("settle", k.settle, |f, (a, b): (f32, f32)| {
        f.f32("a", a);
        f.f32("b", b);
    });
    f.opt(
        "seen_fold",
        k.seen_fold,
        |f, (at, dir): (Instant, CatFoldDirection)| {
            f.at("at", at);
            f.put("direction", dir as u64);
        },
    );
    f.opt("active", k.active, |f, fold| {
        f.at("started", fold.started);
        f.put("direction", fold.direction as u64);
        footprint_words(f, fold.from);
        f.opt("from_sprite", fold.from_sprite, sprite_words);
    });
}

#[inline]
fn footprint_words(f: &mut Fold, p: CatFootprint) {
    f.put("x", p.x as u32 as u64);
    f.put("y", p.y as u32 as u64);
    f.put("w", u64::from(p.w));
    f.put("h", u64::from(p.h));
}

#[inline]
fn sprite_words(f: &mut Fold, (x, y): (i32, i32)) {
    f.put("x", x as u32 as u64);
    f.put("y", y as u32 as u64);
}

#[inline]
fn occurrence_words(f: &mut Fold, o: &Occurrence) {
    f.put("row", u64::from(o.row));
    f.put("start_col", u64::from(o.start_col));
    f.put("end_col", u64::from(o.end_col));
    f.put("form_id", o.form_id.raw());
    f.put("seed", o.seed);
    f.put("ident", o.ident);
    f.at("appeared", o.appeared);
    f.put("genome.gkey", o.genome.gkey);
    f.put("genome.magic", o.genome.magic);
    f.put("ink_base", o.ink_base as u64);
    f.put("ink_cells", u64::from(o.ink_cells));
    f.put("cat_colors.accent", u64::from(o.cat_colors.accent));
    f.put("cat_colors.background", u64::from(o.cat_colors.background));
    f.flag("cat_text_clear", o.cat_text_clear);
    f.flag("cat_peek_down", o.cat_peek_down);
    f.flag("dec_line", o.dec_line);
    f.flag("inert", o.inert);
    f.flag("custom", o.custom);
}

/// One word episode: its clocks, its genome and every done and started latch.
#[inline]
fn episode_words(f: &mut Fold, e: &Episode) {
    f.at("appeared", e.appeared);
    f.at("last_seen", e.last_seen);
    f.put("genome.gkey", e.genome.gkey);
    f.put("genome.magic", e.genome.magic);
    f.put("cat_colors.accent", u64::from(e.cat_colors.accent));
    f.put("cat_colors.background", u64::from(e.cat_colors.background));
    f.flag("nova_done", e.nova_done);
    f.at_opt("nova_start", e.nova_start);
    f.flag("logged", e.logged);
    f.put("seed", e.seed);
    f.put("form_id", e.form_id.raw());
    f.put("ctx_fp", e.ctx_fp);
    f.put("last_row", u64::from(e.last_row));
    f.put("last_col", u64::from(e.last_col));
    f.flag("continuity_tainted", e.continuity_tainted);
    f.put("seen_seq", e.seen_seq);
    f.flag("peek_done", e.peek_done);
    f.flag("peek_started", e.peek_started);
    f.flag("burst_done", e.burst_done);
    f.flag("burst_started", e.burst_started);
    f.flag("sweep_done", e.sweep_done);
    f.flag("sweep_started", e.sweep_started);
    f.flag("born_done", e.born_done);
    f.flag("born_settled", e.born_settled);
    f.put(
        "shown_as",
        match e.shown_as {
            Some(s) => s as u64,
            None => u64::MAX,
        },
    );
    f.at_opt("phase_start", e.phase_start);
    f.at_opt("peek_pause", e.peek_pause);
    f.put("peek_total", e.peek_total);
    f.flag("burst_roll", e.burst_roll);
    f.put("burst_tier", e.burst_tier as u64);
    f.put(
        "burst_kind",
        match e.burst_kind {
            Some(k) => k as u64,
            None => u64::MAX,
        },
    );
    f.flag("bonk_pending", e.bonk_pending);
    f.flag("ledger_stamped", e.ledger_stamped);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::{Duration, Instant};

    fn words(d: &WordDecorations, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        d.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_word_field_moves_the_word_words() {
        let t0 = Instant::now();
        let genome = crate::genome::Genome { gkey: 3, magic: 4 };
        let fresh = || {
            let mut d = WordDecorations::default();
            d.persist.insert(
                11,
                Episode::fresh(t0 + Duration::from_millis(7), genome, 5, 2, 1),
            );
            d.done_marks.insert(21);
            d.done_marks.insert(22);
            d
        };
        let base = words(&fresh(), t0);
        assert_eq!(base, words(&fresh(), t0));

        let mut done = fresh();
        if let Some(ep) = done.persist.get_mut(&11) {
            ep.nova_done = true;
        }
        assert_ne!(words(&done, t0), base, "an episode's done latch is hashed");
        let mut touched = fresh();
        touched.done_marks.touch(21);
        assert_ne!(words(&touched, t0), base, "the done marks' order is hashed");
        let mut marked = fresh();
        marked.done_marks.insert(23);
        assert_ne!(words(&marked, t0), base, "a new done mark is hashed");
        let mut away = fresh();
        away.away = true;
        assert_ne!(
            words(&away, t0),
            base,
            "the word pass's clocks and latches are hashed"
        );
        let mut scanned = fresh();
        scanned.scan_text.push_str("scratch");
        assert_eq!(words(&scanned, t0), base, "the scan's scratch is not state");
    }

    /// A named change to a fresh word pass, given the script's `t0`.
    type Case = (&'static str, fn(&mut WordDecorations, Instant));

    #[test]
    fn a_kept_reservation_witness_flash_and_birth_moves_the_word_words() {
        let t0 = Instant::now();
        let base = words(&WordDecorations::default(), t0);
        let cases: [Case; 7] = [
            ("an ignition reservation", |d, t0| {
                d.ignitions.push(IgnitionReservation {
                    pane: Some(9),
                    owner: 11,
                    start: t0 + Duration::from_millis(3),
                    center: (40, 18),
                });
            }),
            ("the typed-edit witness", |d, t0| {
                d.note_typed_edit(t0 + Duration::from_millis(2), None);
            }),
            ("a noted trick word", |d, t0| {
                d.trick_flash.note(t0, None, None, Some((2, 5)), "sit", 0);
            }),
            ("a pending birth", |d, _| {
                d.pending.push(PendingBirth {
                    occ_idx: 0,
                    ctx_fp: 5,
                    live_caret_completion: false,
                    at_live_cursor: true,
                });
            }),
            ("the cursor kitty's last sprite", |d, _| {
                d.kitty_cursor_placement.last_sprite = Some((30, 12));
            }),
            ("a word's taken ink", |d, _| {
                d.ink_base_fg.push([200, 40, 10]);
                d.ink_cols.push(4);
            }),
            ("the pet's ink span", |d, _| d.pet_ink_spans.push((2, 9))),
        ];
        for (what, set) in cases {
            let mut d = WordDecorations::default();
            set(&mut d, t0);
            assert_ne!(words(&d, t0), base, "{what} is hashed");
        }

        let reserved_in = |pane| {
            let mut d = WordDecorations::default();
            d.ignitions.push(IgnitionReservation {
                pane: Some(pane),
                owner: 11,
                start: t0,
                center: (40, 18),
            });
            words(&d, t0)
        };
        assert_eq!(
            reserved_in(9),
            reserved_in(14),
            "a pane's id is the process's count of terminals, not state"
        );
        let hosted = WordDecorations {
            pane_px: (120, 40),
            grace_override: Some(Duration::ZERO),
            ..WordDecorations::default()
        };
        assert_eq!(
            words(&hosted, t0),
            base,
            "the host's pane origin and a test's grace override are settings, not state"
        );
    }
}
