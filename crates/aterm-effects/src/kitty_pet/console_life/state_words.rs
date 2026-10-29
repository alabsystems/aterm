// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The console resident's state words (`crate::state_words`): the input episode and the typing it
//! saw, the attention it gave and why, the block it anchored to, its target, gaze and seat, the
//! contact, completion and result it reacts to, the pose and the holds it keeps (visibility, the
//! z-order, the motes it hid, the rest seat and the pause), the repair and the perch trip under
//! way, the progress it follows, the stamp of the last observation with the selection and
//! progress read from it, and each row's signature with when it last changed.
//!
//! Not hashed: the observed world's maps (rebuilt from the frame's cells by `PetWorld::observe`,
//! or kept while the reading holds the same frame, so equal frames read equal maps), the spare
//! world kept for its allocation, the reading's key (`PetReading`, which frame the world was read
//! from), what each pass rebuilds before it reads it (the host's chrome and blanked glass as the
//! rows carry them, and the row signatures read for them) and a test's switches
//! (`offer_hidden_motes`, `exact_arms`). A surface's session and terminal ids are the process's
//! count of terminals, not the pet's state: a surface is hashed by its screen, its size and its
//! epochs.

use super::{ConsoleLife, InputEpisode, MoteGate, PerchTrip, PetInputKind, VisibilityHold, ZHold};
use crate::pet_world::{PetAnchor, PetRect, PetSurface, PetWorldStamp, TaskbarProgress};
use crate::state_words::{Fold, Words};
use aterm_core::terminal::BlockState;
use aterm_time::Instant;

impl ConsoleLife {
    /// The episode, the attention, the anchor and seat, the reactions, the holds, the trips, the
    /// progress, the last observation's stamp and the row signatures.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.flag("pet.console.presentable", self.presentable);
        w.put("pet.console.seq", self.seq);
        w.opt("pet.console.input", self.input, |f, e: InputEpisode| {
            f.put("seq", e.seq);
            f.put("kind", e.kind as u64);
            f.at("at", e.at);
            f.opt("target", e.target, pair);
        });
        w.at_opt("pet.console.typed_at", self.typed_at);
        w.put(
            "pet.console.typed_kind",
            self.typed_kind.map_or(u64::MAX, |k: PetInputKind| k as u64),
        );
        w.put("pet.console.consumed", self.consumed);
        w.put("pet.console.attention", self.attention as u64);
        w.put(
            "pet.console.reason",
            aterm_hash::fnv1a_64(self.reason.as_bytes()),
        );
        w.opt("pet.console.anchor", self.anchor, anchor_words);
        w.opt("pet.console.target", self.target, rect_words);
        w.opt("pet.console.gaze", self.gaze, pair);
        w.flag("pet.console.resident", self.resident);
        w.flag("pet.console.resident_handoff", self.resident_handoff);
        w.flag("pet.console.moving", self.moving);
        w.flag("pet.console.clipped", self.clipped);
        w.at_opt("pet.console.contact_at", self.contact_at);
        w.flag("pet.console.contact_armed", self.contact_armed);
        w.flag("pet.console.contact_changed", self.contact_changed);
        w.opt("pet.console.completion", self.completion, stamped);
        w.opt("pet.console.result_at", self.result_at, stamped);
        w.put("pet.console.pose", self.pose.map_or(u64::MAX, |p| p as u64));
        w.flag("pet.console.still", self.still);
        w.f32("pet.console.hidden_hold", self.hidden_hold);
        w.flag("pet.console.caret_held", self.caret_held);
        w.opt("pet.console.tick_stamp", self.tick_stamp, stamp_words);
        w.at_opt("pet.console.repair_until", self.repair_until);
        w.opt("pet.console.repair_target", self.repair_target, pair);
        w.put(
            "pet.console.progress_key",
            self.progress_key.unwrap_or(u64::MAX),
        );
        w.flag("pet.console.progress_key.some", self.progress_key.is_some());
        w.opt("pet.console.progress", self.progress, progress_words);
        w.at_opt("pet.console.progress_at", self.progress_at);
        w.opt("pet.console.trip", self.trip, |f, t: PerchTrip| {
            pair(f, t.from);
            pair(f, t.to);
            f.f32("elapsed", t.elapsed);
            f.f32("duration", t.duration);
            f.flag("hop", t.hop);
        });
        w.f32("pet.console.lift", self.lift);
        w.put("pet.console.source_seq", self.source_seq);
        w.put("pet.console.completion_seq", self.completion_seq);
        w.at_opt("pet.console.failure_quiet_until", self.failure_quiet_until);
        w.opt("pet.console.last_body", self.last_body, rect_words);
        w.opt("pet.console.last_seat", self.last_seat, pair);
        w.flag("pet.console.on_glass", self.on_glass);
        w.group("pet.console.hold", |f| hold_words(f, self.hold));
        w.group("pet.console.z", |f| z_words(f, self.z));
        w.put(
            "pet.console.mote_gate",
            match self.mote_gate {
                MoteGate::Open => 0,
                MoteGate::Shut => 1,
                MoteGate::Clearance => 2,
            },
        );
        w.list(
            "pet.console.hidden_motes",
            self.hidden_motes,
            |f, hidden| {
                f.flag("hidden", hidden);
            },
        );
        w.cell_opt("pet.console.cursor_home", self.cursor_home);
        w.put("pet.console.observed_seq", self.observed_seq);
        w.put("pet.console.progress_seq", self.progress_seq);
        w.at_opt("pet.console.perched_since", self.perched_since);
        w.cell_opt("pet.console.faux_caret", self.faux_caret);
        w.opt("pet.console.rest_seat", self.rest_seat.get(), pair);
        w.opt("pet.console.rest_stand", self.rest_stand.get(), pair);
        w.flag("pet.console.rest_seated", self.rest_seated.get());
        w.at_opt("pet.console.pause_hold_until", self.pause_hold_until.get());
        w.flag("pet.console.line_escort", self.line_escort.get());
        w.opt("pet.console.world", self.world.as_deref(), |f, world| {
            f.opt("stamp", world.stamp(), stamp_words);
            f.opt("selection_target", world.selection_target(), pair);
            f.opt("progress", world.progress(), progress_words);
        });
        w.list("pet.console.row_sig", &self.row_sig, |f, &sig| {
            f.put("sig", sig);
        });
        w.list("pet.console.row_sig_prev", &self.row_sig_prev, |f, &sig| {
            f.put("sig", sig);
        });
        w.list("pet.console.row_changed", &self.row_changed, |f, &at| {
            f.at_opt("at", at);
        });
        w.put("pet.console.row_sig_first", self.row_sig_first as u64);
    }
}

#[inline]
fn pair(f: &mut Fold, (a, b): (f32, f32)) {
    f.f32("a", a);
    f.f32("b", b);
}

/// A reaction's stamp and its verdict.
#[inline]
fn stamped(f: &mut Fold, (at, ok): (Instant, bool)) {
    f.at("at", at);
    f.flag("ok", ok);
}

#[inline]
fn rect_words(f: &mut Fold, r: PetRect) {
    f.f32("row", r.row);
    f.f32("col", r.col);
    f.f32("rows", r.rows);
    f.f32("cols", r.cols);
}

/// A surface by its screen, its size and its epochs, never by its session's or terminal's id.
#[inline]
fn surface_words(f: &mut Fold, s: PetSurface) {
    f.flag("alt_screen", s.alt_screen);
    f.put("rows", s.rows as u64);
    f.put("cols", s.cols as u64);
    f.put("absolute_row_revision", s.absolute_row_revision);
    f.put("history_renumber_epoch", s.history_renumber_epoch);
    f.put("scroll_invalidation", s.scroll_invalidation);
}

#[inline]
fn stamp_words(f: &mut Fold, s: PetWorldStamp) {
    f.group("surface", |f| surface_words(f, s.surface));
    f.put("content_seq", s.content_seq);
    f.put("display_offset", s.display_offset as u32 as u64);
    f.put("top_absolute_row", s.top_absolute_row);
    f.put("uniform_up_rows", s.uniform_up_rows);
    f.put("scroll_band_batches", s.scroll_band_batches);
    f.put("scroll_band_seq", s.scroll_band_seq);
}

#[inline]
fn anchor_words(f: &mut Fold, a: PetAnchor) {
    f.group("surface", |f| surface_words(f, a.surface));
    f.put("block_id", a.block_id);
    f.put("absolute_row", a.absolute_row);
    f.f32("row", a.row);
    f.f32("col", a.col);
    f.put(
        "state",
        match a.state {
            BlockState::PromptOnly => 0,
            BlockState::EnteringCommand => 1,
            BlockState::Executing => 2,
            BlockState::Complete => 3,
            _ => u64::MAX,
        },
    );
    f.put(
        "exit_code",
        a.exit_code.map_or(u64::MAX, |c| u64::from(c as u32)),
    );
}

#[inline]
fn progress_words(f: &mut Fold, p: TaskbarProgress) {
    let (state, value) = match p {
        TaskbarProgress::Hidden => (0, 0),
        TaskbarProgress::Normal(v) => (1, v),
        TaskbarProgress::Error(v) => (2, v),
        TaskbarProgress::Indeterminate => (3, 0),
        TaskbarProgress::Paused(v) => (4, v),
        _ => (u64::MAX, 0),
    };
    f.put("state", state);
    f.put("value", u64::from(value));
}

#[inline]
fn hold_words(f: &mut Fold, h: VisibilityHold) {
    f.flag("shown", h.shown);
    f.at_opt("contrary_since", h.contrary_since);
    f.at_opt("blind_since", h.blind_since);
    f.f32("cover", h.cover);
    f.at_opt("at", h.at);
    f.flag("by_fiat", h.by_fiat);
}

#[inline]
fn z_words(f: &mut Fold, z: ZHold) {
    f.flag("under", z.under);
    f.at_opt("clear_since", z.clear_since);
}

#[cfg(test)]
mod tests {
    use super::super::PetAttention;
    use super::*;
    use crate::pet_glyphs_gen::PetGlyphId;
    use crate::state_words::{StateWords, Word};
    use aterm_time::Duration;

    fn words(c: &ConsoleLife, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        c.state_words(&mut w);
        w.into_words()
    }

    fn surface(session: u64, terminal_id: u64) -> PetSurface {
        PetSurface {
            session,
            terminal_id,
            alt_screen: false,
            rows: 24,
            cols: 80,
            absolute_row_revision: 1,
            history_renumber_epoch: 0,
            scroll_invalidation: 0,
        }
    }

    fn stamp(session: u64, terminal_id: u64) -> PetWorldStamp {
        PetWorldStamp {
            surface: surface(session, terminal_id),
            content_seq: 7,
            display_offset: 0,
            top_absolute_row: 3,
            uniform_up_rows: 0,
            scroll_band_batches: 0,
            scroll_band_seq: 0,
        }
    }

    /// A named change to a fresh console resident, given the script's `t0`.
    type Case = (&'static str, fn(&mut ConsoleLife, Instant));

    #[test]
    fn each_console_episode_hold_seat_and_row_moves_its_words() {
        let t0 = Instant::now();
        let base = words(&ConsoleLife::new(), t0);
        assert_eq!(base, words(&ConsoleLife::new(), t0));
        let cases: [Case; 16] = [
            ("the input episode", |c, t0| {
                c.input = Some(InputEpisode {
                    seq: 2,
                    kind: PetInputKind::Text,
                    at: t0 + Duration::from_millis(4),
                    target: Some((3.0, 9.0)),
                });
            }),
            ("the attention", |c, _| c.attention = PetAttention::Typing),
            ("its reason", |c, _| c.reason = "typed"),
            ("the anchor", |c, _| {
                c.anchor = Some(PetAnchor {
                    surface: surface(1, 1),
                    block_id: 4,
                    absolute_row: 12,
                    row: 2.0,
                    col: 0.0,
                    state: BlockState::Executing,
                    exit_code: None,
                });
            }),
            ("the target", |c, _| {
                c.target = Some(PetRect::new(2.0, 5.0, 1.0, 3.0));
            }),
            ("a completion", |c, t0| c.completion = Some((t0, true))),
            ("the pose", |c, _| c.pose = Some(PetGlyphId::PetSit)),
            ("the last observation's stamp", |c, _| {
                c.tick_stamp = Some(stamp(1, 1));
            }),
            ("the progress followed", |c, _| {
                c.progress = Some(TaskbarProgress::Normal(40));
            }),
            ("the perch trip", |c, _| {
                c.trip = Some(PerchTrip {
                    from: (1.0, 2.0),
                    to: (1.0, 9.0),
                    elapsed: 0.1,
                    duration: 0.4,
                    hop: true,
                });
            }),
            ("the visibility hold", |c, t0| {
                c.hold.contrary_since = Some(t0 + Duration::from_millis(8));
            }),
            ("the z-order hold", |c, _| c.z.under = true),
            ("the motes it hid", |c, _| {
                c.mote_gate = MoteGate::Clearance;
                c.hidden_motes[1] = true;
            }),
            ("the rest seat", |c, _| c.rest_seat.set(Some((4.0, 2.0)))),
            ("the pause hold", |c, t0| {
                c.pause_hold_until
                    .set(Some(t0 + Duration::from_millis(600)));
            }),
            ("a row's signature and change", |c, t0| {
                c.row_sig.push(0xABCD);
                c.row_changed.push(Some(t0));
            }),
        ];
        for (what, set) in cases {
            let mut c = ConsoleLife::new();
            set(&mut c, t0);
            assert_ne!(words(&c, t0), base, "{what} is hashed");
        }
    }

    #[test]
    fn a_stamp_is_hashed_by_its_screen_and_epochs_not_its_session() {
        let t0 = Instant::now();
        let stamped = |s: PetWorldStamp| {
            let mut c = ConsoleLife::new();
            c.tick_stamp = Some(s);
            words(&c, t0)
        };
        let base = stamped(stamp(4, 4));
        assert_eq!(
            stamped(stamp(9, 12)),
            base,
            "a session's and a terminal's ids are the process's count of terminals, not state"
        );
        let mut scrolled = stamp(4, 4);
        scrolled.surface.scroll_invalidation = 1;
        assert_ne!(stamped(scrolled), base, "the surface's epochs are hashed");
        let mut moved = stamp(4, 4);
        moved.display_offset = 2;
        assert_ne!(
            stamped(moved),
            base,
            "the stamp's scroll position is hashed"
        );
    }
}
