// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet brain's state words (`crate::state_words`): the body, its clocks and the surface it
//! stands on, the flight, the run and lead it keeps beside the caret (the held stands among
//! them), its moods and pending reactions (the offered and caught star, the trick and room cues),
//! the pointer's play, the gait and the still pose it holds, the wear, the palette and arrival,
//! the departures, the letter veil, the motes and the last frame it drew by, then the console's
//! resident (its words are `console_life/state_words.rs`'s) and the pointer's stroke detector
//! (`crate::pet_stroke`'s). Not hashed: the tests' switches (`hug_armed`, `letters_unseen`,
//! `letter_arrival_dwells`, `letter_step_rearms` and `old_paths`), which a test sets and the
//! brain only reads. A surface's session id is the
//! process's count of terminals, not the pet's state: a surface is hashed by its screen and by
//! whether it is the one the body stands on.

use super::{
    AdoptedLook, Carried, CheerAir, Departure, Flight, FlightKind, HeldStand, Mote, PendingResight,
    PendingTrick, PetAction, PetBrain, PlayLand, RoomCue, Stance, WrapAhead,
};
use crate::cat_baker::CatColorKey;
use crate::pet_glyphs_gen::PetGlyphId;
use crate::rainbow_kitty::companion::StarCatch;
use crate::state_words::{Fold, Words};
use aterm_lexicon::Trick;
use aterm_time::Instant;

impl PetBrain {
    /// The body and every scalar clock, latch and pending reaction named below.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.put("pet.species", self.species as u64);
        w.f32("pet.col", self.col);
        w.f32("pet.row", self.row);
        w.f32("pet.speed", self.speed);
        w.flag("pet.facing_left", self.facing_left);
        w.put("pet.action", self.action as u64);
        w.f32("pet.action_t", self.action_t);
        w.f32("pet.quiet", self.quiet);
        w.f32("pet.stride", self.stride);
        w.f64("pet.clock", self.clock);
        w.f64("pet.lane_clock", self.lane_clock);
        w.cell_opt("pet.last_caret", self.last_caret);
        w.flag("pet.line_returned", self.line_returned);
        w.cell_opt("pet.regrid_caret", self.regrid_caret);
        w.at_opt("pet.last_now", self.last_now);
        w.opt("pet.surface", self.surface, |f, (_session, alt)| {
            f.flag("alt", alt);
        });

        w.opt("pet.flight", self.flight, flight_words);
        w.opt(
            "pet.deferred_hidden_landing",
            self.deferred_hidden_landing,
            pair,
        );
        w.opt("pet.retired_flight_lift", self.retired_flight_lift, pair);
        w.f32("pet.sink", self.sink);
        w.f32("pet.sink_from", self.sink_from);
        w.f32("pet.sink_to", self.sink_to);
        w.f64("pet.sink_since", self.sink_since);
        w.f32("pet.fit", self.fit);
        w.f32("pet.fit_from", self.fit_from);
        w.f32("pet.fit_to", self.fit_to);
        w.opt("pet.sink_seat", self.sink_seat, |f, (at, over)| {
            pair(f, at);
            pair(f, over);
        });
        w.f32("pet.land_t", self.land_t);
        w.flag("pet.hop_crouch", self.hop_crouch);
        w.f32("pet.coil_d", self.coil_d);
        w.f32("pet.flinch_t", self.flinch_t);

        w.group("pet.run", |f| {
            f.put("last_dir", self.last_dir as u8 as u64);
            f.put("reversals", u64::from(self.reversals));
            f.f32("reversal_t", self.reversal_t);
            f.f32("vhat", self.vhat);
            f.put("run_dir", self.run_dir as u8 as u64);
            f.put("run_count", u64::from(self.run_count));
            f.f32("run_t", self.run_t);
            f.f32("lead_room", self.lead_room);
            f.f32("held_lead", self.held_lead);
            f.cell_opt("held_for", self.held_for);
            f.list("held_stash", &self.held_stash, |f, slot| {
                f.opt("v", *slot, |f, s: HeldStand| {
                    self.surface_words(f, s.surface);
                    f.cell("caret", s.caret);
                    f.f32("lead", s.lead);
                });
            });
            f.f32("run_gap", self.run_gap);
            f.f32("vrun", self.vrun);
            f.opt("rhythm_at", self.rhythm_at, |f, v| f.f64("v", v));
            f.f32("run_pace", self.run_pace);
            f.f32("run_cells", self.run_cells);
            f.f32("run_secs", self.run_secs);
            f.f32("run_step", self.run_step);
            f.put("wide_steps", u64::from(self.wide_steps));
            f.f32("key_beat", self.key_beat);
            f.f32("key_pace", self.key_pace);
        });
        w.group("pet.wall", |f| {
            f.flag("wall_side", self.wall_side);
            f.flag("pending_wall_transit", self.pending_wall_transit);
            f.flag("chrome_hop", self.chrome_hop);
            f.flag("wrap_back", self.wrap_back);
            f.opt("wrap_ahead", self.wrap_ahead, wrap_ahead_words);
            f.opt("writing_wrap", self.writing_wrap, |f, v| f.f64("v", v));
            f.flag("launch_running", self.launch_running);
            f.flag("launch_brisk", self.launch_brisk);
            f.flag("land_running", self.land_running);
            f.put("retreat_run", u64::from(self.retreat_run));
            f.f32("retreat_gap", self.retreat_gap);
            f.flag("retreat_bottled", self.retreat_bottled);
        });
        w.group("pet.ink", |f| {
            f.put("ink_base", u64::from(self.ink_base));
            f.list("ink_spans", &self.ink_spans, |f, &span| {
                f.cell("span", span)
            });
            f.flag("ink_scrolled", self.ink_scrolled);
            f.u16_opt("ink_live", self.ink_live);
        });
        w.group("pet.mood", |f| {
            f.f32("content", self.content);
            f.at_opt("pending_bell", self.pending_bell);
            f.opt("pending_cheer", self.pending_cheer, |f, v| f.flag("v", v));
            f.flag("room_cheer", self.room_cheer);
            f.put("room_cheer_trips", u64::from(self.room_cheer_trips));
            f.opt("room_cheer_stage", self.room_cheer_stage, pair);
            f.opt("room_cheer_air", self.room_cheer_air, cheer_air_words);
            f.flag("room_cheer_leaving", self.room_cheer_leaving);
            f.flag("pending_sulk", self.pending_sulk);
            f.at_opt("pending_flow", self.pending_flow);
            f.at_opt("pending_perk", self.pending_perk);
            f.at_opt("perk_spent", self.perk_spent);
            f.opt("pending_catch", self.pending_catch, star_catch_words);
            f.opt("caught", self.caught, star_catch_words);
            f.f32("catch_cool", self.catch_cool);
            f.put(
                "mote_rgb",
                match self.mote_rgb {
                    Some(v) => u64::from(v),
                    None => u64::MAX,
                },
            );
            f.put("pending_pet", u64::from(self.pending_pet));
            f.at_opt("pet_at", self.pet_at);
            f.f32("pet_hold_t", self.pet_hold_t);
            f.put("song_left", u64::from(self.song_left));
            f.f32("song_next", self.song_next);
            f.f32("watch_heat", self.watch_heat);
            f.f32("watch_t", self.watch_t);
            f.flag("watch_spent", self.watch_spent);
            f.at_opt("exec_since", self.exec_since);
            f.flag("vigil_armed", self.vigil_armed);
            f.flag("full_screen", self.full_screen);
            f.f32("vigil_t", self.vigil_t);
            f.flag("vigil", self.vigil);
            f.flag("pending_vigil_pounce", self.pending_vigil_pounce);
            f.opt("vigil_cheer", self.vigil_cheer, |f, v| f.flag("v", v));
            f.opt("sulk_at", self.sulk_at, |f, v| f.f64("v", v));
        });
        w.group("pet.play", |f| {
            f.opt("last_pointer", self.last_pointer, pair);
            f.f32("pointer_vx", self.pointer_vx);
            f.f32("pointer_vy", self.pointer_vy);
            f.f32("pointer_speed", self.pointer_speed);
            f.f32("pointer_heat", self.pointer_heat);
            f.f32("dash_t", self.dash_t);
            f.flag("pointer_armed", self.pointer_armed);
            f.opt("pending_pointer_pounce", self.pending_pointer_pounce, pair);
            f.opt("play_to", self.play_to, pair);
            f.opt(
                "play_land",
                self.play_land,
                |f, land: PlayLand| match land {
                    PlayLand::PointerFrolic => f.put("kind", 0),
                    PlayLand::BatSwipe { col, row } => {
                        f.put("kind", 1);
                        pair(f, (col, row));
                    }
                },
            );
            f.flag("play_frolic", self.play_frolic);
            f.f32("play_hold", self.play_hold);
            f.flag("swipe", self.swipe);
            f.opt("pending_bat", self.pending_bat, pair);
            f.at_opt("bat_at", self.bat_at);
            f.opt("pursuit_t", self.pursuit_t, |f, v| f.f32("v", v));
            f.f32("pursuit_cool", self.pursuit_cool);
            f.flag("groom_owed", self.groom_owed);
            f.flag("stakeout", self.stakeout);
            f.f32("stakeout_t", self.stakeout_t);
            f.opt("pending_look", self.pending_look, |f, v| f.f32("v", v));
            f.at_opt("look_at", self.look_at);
            f.flag("bat_greet", self.bat_greet);
            f.flag("tennis", self.tennis);
            f.f64("tennis_last", self.tennis_last);
            f.f64("last_frolic", self.last_frolic);
        });
        w.group("pet.gait", |f| {
            f.f32("stumble_t", self.stumble_t);
            f.f32("leg_dist", self.leg_dist);
            f.flag("braking", self.braking);
            f.opt("brake_over", self.brake_over, |f, v| f.f32("v", v));
            f.f64("brake_at", self.brake_at);
            f.flag("skid_puff", self.skid_puff);
            f.flag("lateral", self.lateral);
            f.f32("gait_pace", self.gait_pace);
            f.f32("gait_dir", self.gait_dir);
            f.f32("gait_t", self.gait_t);
            f.f32("gait_v", self.gait_v);
            f.f32("gait_rest", self.gait_rest);
            f.f32("tick_dt", self.tick_dt);
            f.f64("gallop_at", self.gallop_at);
            f.flag("planted", self.planted);
            f.flag("departing", self.departing);
            f.opt("stand_from", self.stand_from, |f, v| f.f64("v", v));
            f.opt(
                "still_since",
                self.still_since,
                |f, (at, pose): (f64, PetGlyphId)| {
                    f.f64("at", at);
                    f.put("pose", pose as u64);
                },
            );
            f.f32("still_scale.x", self.still_scale.0);
            f.f32("still_scale.y", self.still_scale.1);
            f.flag("still_held", self.still_held);
            f.flag("asked_frames", self.asked_frames);
            f.flag("frame_owed", self.frame_owed);
            f.opt("aim", self.aim, pair);
            f.flag("set_off_owed", self.set_off_owed);
            f.opt("writing_wait", self.writing_wait, |f, v| f.f32("v", v));
            f.opt("set_off_asked", self.set_off_asked, |f, key: [i32; 4]| {
                f.put("body_px", key[0] as u32 as u64);
                f.put("body_row", key[1] as u32 as u64);
                f.put("aim_px", key[2] as u32 as u64);
                f.put("aim_row", key[3] as u32 as u64);
            });
            f.flag("action_began", self.action_began);
            f.flag("rest_kept.gait", self.rest_kept.0);
            f.flag("rest_kept.content", self.rest_kept.1);
            f.opt("moved_at", self.moved_at, |f, v| f.f64("v", v));
            f.flag("still_console", self.still_console);
            f.flag("stand_ends_leg", self.stand_ends_leg);
            f.flag("drew_leg", self.drew_leg);
            f.flag("console_posed", self.console_posed);
            f.opt(
                "deferred_arc",
                self.deferred_arc,
                |f, (to_col, to_row, dur, arc, kind): (f32, f32, f32, f32, FlightKind)| {
                    f.f32("to_col", to_col);
                    f.f32("to_row", to_row);
                    f.f32("dur", dur);
                    f.f32("arc", arc);
                    flight_kind_words(f, kind);
                },
            );
            f.opt("gallop_since", self.gallop_since, |f, v| f.f64("v", v));
            f.flag("running", self.running);
            f.flag("pending_drop", self.pending_drop);
            f.f32("drop_sit_t", self.drop_sit_t);
            f.flag("drop_stare", self.drop_stare);
            f.flag("scrabble", self.scrabble);
            f.f32("commit_t", self.commit_t);
            f.opt("resume", self.resume, |f, (action, t): (PetAction, f32)| {
                f.put("action", action as u64);
                f.f32("t", t);
            });
            f.f32("bored_cool", self.bored_cool);
            f.opt("hide_to", self.hide_to, |f, v| f.f32("v", v));
            f.flag("hiding", self.hiding);
            f.f32("hide_t", self.hide_t);
            f.f32("wriggle_t", self.wriggle_t);
            f.f32("twitch_t", self.twitch_t);
            f.flag("twitch_up", self.twitch_up);
            f.flag("last_burst", self.last_burst);
            f.flag("droop_seated", self.droop_seated);
            f.flag("droop_brief", self.droop_brief);
        });
        w.group("pet.room", |f| {
            f.flag("room_lease", self.room_lease);
            f.flag("room_quiet", self.room_quiet);
            f.flag("room_hold", self.room_hold);
            f.put("room_turn", self.room_turn);
            f.flag("room_sibling", self.room_sibling);
            f.put("room_unseen", self.room_unseen);
            f.flag("room_overdue", self.room_overdue);
            f.flag("room_turn_seeded", self.room_turn_seeded);
            f.flag("room_inbox_seeded", self.room_inbox_seeded);
            f.opt(
                "pending_room",
                self.pending_room,
                |f, cue: RoomCue| match cue {
                    RoomCue::Look { col } => {
                        f.put("kind", 0);
                        f.f32("col", col);
                    }
                    RoomCue::Nod => f.put("kind", 1),
                    RoomCue::EarFlat => f.put("kind", 2),
                    RoomCue::Mail => f.put("kind", 3),
                },
            );
            f.at_opt("room_at", self.room_at);
            f.flag("room_cue_act", self.room_cue_act);
        });
        w.group("pet.leap", |f| {
            f.opt("pending_trick", self.pending_trick, |f, p: PendingTrick| {
                f.put("trick", p.trick as u64);
                f.flag("confirmed", p.confirmed);
                f.at("noted", p.noted);
                f.at("stamped", p.stamped);
            });
            f.opt("trick_go", self.trick_go, |f, t: Trick| {
                f.put("v", t as u64)
            });
            f.opt(
                "trick_heard",
                self.trick_heard,
                |f, (t, at): (Trick, Instant)| {
                    f.put("trick", t as u64);
                    f.at("at", at);
                },
            );
            f.flag("trick_act", self.trick_act);
            f.opt("trick_settle", self.trick_settle, |f, t: Trick| {
                f.put("v", t as u64);
            });
            f.f32("hold_end_q", self.hold_end_q);
            f.at_opt("trick_submit_at", self.trick_submit_at);
            f.f64("trick_content_at", self.trick_content_at);
            f.flag("pending_pounce", self.pending_pounce);
            f.flag("pending_big_jump", self.pending_big_jump);
            f.flag("big_gather", self.big_gather);
            f.flag("quiet_leap", self.quiet_leap);
            f.flag("stream", self.stream);
            f.put("stream_run", u64::from(self.stream_run));
            f.f64("stream_last", self.stream_last);
            f.opt("jump_at", self.jump_at, |f, v| f.f64("v", v));
            f.opt("burst_from", self.burst_from, |f, v| f.f64("v", v));
            f.flag("lone_along", self.lone_along);
            f.opt("paste_hold", self.paste_hold, |f, v| f.f64("v", v));
            f.flag("coil_waited", self.coil_waited);
            f.opt("bound2", self.bound2, |f, (a, b, c): (f32, f32, bool)| {
                f.f32("a", a);
                f.f32("b", b);
                f.flag("c", c);
            });
            f.f32("land_span", self.land_span);
            f.f32("skid_from", self.skid_from);
            f.f32("skid_dir", self.skid_dir);
            f.f32("skid_len", self.skid_len);
            f.put("mote_serial", u64::from(self.mote_serial));
            f.put("departure_serial", u64::from(self.departure_serial));
            f.put("settle_serial", u64::from(self.settle_serial));
            f.flag("woke_seat", self.woke_seat);
            f.flag("touched_on_chrome", self.touched_on_chrome);
            f.put("settle_seed", u64::from(self.settle_seed));
            f.put("mote_mark", self.mote_mark as u64);
            f.flag("sit_front", self.sit_front);
            f.flag("peek", self.peek);
            f.put("look_row", self.look_row as u8 as u64);
            f.f32("gaze_dwell", self.gaze_dwell);
            f.f32("alpha", self.alpha);
        });
        w.list("pet.motes", &self.motes, |f, slot| {
            f.opt("v", *slot, |f, m: Mote| {
                f.put("kind", m.kind as u64);
                f.f64("born", m.born);
                f.f32("life", m.life);
                f.f32("col", m.col);
                f.f32("row", m.row);
                f.f32("dir", m.dir);
                f.put("seed", u64::from(m.seed));
                f.put(
                    "rgb",
                    match m.rgb {
                        Some(v) => u64::from(v),
                        None => u64::MAX,
                    },
                );
            });
        });
        w.group("pet.wear", |f| {
            f.opt("worn", self.worn, |f, (a, b): (u8, u8)| {
                f.put("coat", u64::from(a));
                f.put("iris", u64::from(b));
            });
            f.opt("pending_worn", self.pending_worn, |f, (a, b): (u8, u8)| {
                f.put("coat", u64::from(a));
                f.put("iris", u64::from(b));
            });
            f.opt("colors", self.colors, color_key_words);
            f.put("pending_arrival", self.pending_arrival as u64);
            f.opt("handoff_parked_clock", self.handoff_parked_clock, |f, v| {
                f.f64("v", v);
            });
            f.f32("arrive_t", self.arrive_t);
            f.f32("arrive_in", self.arrive_in);
            f.f32("arrive_from", self.arrive_from);
            f.f32("last_presence", self.last_presence);
        });
        w.group("pet.departures", |f| {
            f.list("departures", &self.departures, |f, slot| {
                f.opt("v", *slot, |f, d: Departure| self.departure_words(f, &d));
            });
            f.opt("resight", self.resight, |f, r: PendingResight| {
                self.departure_words(f, &r.ghost);
                f.at("at", r.at);
            });
            f.list("carried", &self.carried, |f, slot| {
                f.opt("v", *slot, |f, c: Carried| {
                    self.departure_words(f, &c.ghost);
                    f.f32("presence", c.presence);
                    f.flag("home", c.home);
                });
            });
        });
        w.group("pet.letters", |f| {
            let (a, at, b) = self.letter_veil;
            f.f32("veil.a", a);
            f.at_opt("veil.at", at);
            f.f32("veil.b", b);
            f.at_opt("cover_since", self.letter_cover_since);
            f.at_opt("rest_since", self.letter_rest_since);
            f.flag("rest_stepped", self.letter_rest_stepped);
            f.put("step_seq", self.letter_step_seq);
            f.at_opt("gone_since", self.letter_gone_since);
            f.flag("veil_gather", self.veil_gather);
            f.opt("front_pass", self.front_pass, |f, v| f.flag("v", v));
            f.put("front_pass_serial", u64::from(self.front_pass_serial));
            f.flag("repainting", self.repainting);
        });
        w.group("pet.drawn", |f| {
            f.list("last_drawn", self.last_drawn, |f, v| f.f32("v", v));
            f.cell("last_grid", self.last_grid);
            f.put("last_pose", self.last_pose as u64);
            f.flag("last_facing", self.last_facing);
            f.group("last_stance", |f| stance_words(f, self.last_stance));
            f.opt("adopted_look", self.adopted_look, |f, look: AdoptedLook| {
                f.put("pose", look.pose as u64);
                f.flag("facing_left", look.facing_left);
                f.group("stance", |f| stance_words(f, look.stance));
                f.f32("eased", look.eased);
                f.group("seat", |f| pair(f, look.seat));
                f.opt(
                    "beneath",
                    look.beneath,
                    |f, (pose, facing_left): (PetGlyphId, bool)| {
                        f.put("pose", pose as u64);
                        f.flag("facing_left", facing_left);
                    },
                );
            });
            f.put("last_cell_h", u64::from(self.last_cell_h));
            f.put("last_cell_w", u64::from(self.last_cell_w));
            f.flag("last_reduced", self.last_reduced);
            f.f32("col_at_tick", self.col_at_tick);
            f.f32("home", self.home);
            f.flag("evicting", self.evicting);
        });
        self.console.state_words(w);
        w.group("pet.stroke", |f| self.stroke.state_words(f));
    }

    /// A surface the brain keeps beside its own: whether it is the body's, then its screen.
    #[inline]
    fn surface_words(&self, f: &mut Fold, surface: Option<(u64, bool)>) {
        f.flag("own_surface", surface == self.surface);
        f.opt("surface", surface, |f, (_session, alt)| f.flag("alt", alt));
    }

    /// One departing body's every field, its surface as [`Self::surface_words`] writes it.
    #[inline]
    fn departure_words(&self, f: &mut Fold, d: &Departure) {
        f.f64("born", d.born);
        f.put("id", u64::from(d.id));
        f.put("coat", u64::from(d.coat));
        f.put("iris", u64::from(d.iris));
        f.f32("from_col", d.from_col);
        f.f32("row", d.row);
        f.f32("edge", d.edge);
        f.f32("speed", d.speed);
        f.flag("still", d.still);
        f.flag("facing_left", d.facing_left);
        f.f32("stride0", d.stride0);
        f.put("pose0", d.pose0 as u64);
        f.group("stance0", |f| stance_words(f, d.stance0));
        f.flag("resight", d.resight);
        f.put(
            "own_glass",
            match d.own_glass {
                Some(g) => u64::from(g),
                None => u64::MAX,
            },
        );
        f.flag("own_clear", d.own_clear);
        f.f32("peak", d.peak);
        f.opt("colors", d.colors, color_key_words);
        self.surface_words(f, d.surface);
    }
}

#[inline]
fn wrap_ahead_words(f: &mut Fold, ahead: WrapAhead) {
    match ahead {
        WrapAhead::Approach => f.put("kind", 0),
        WrapAhead::Launched(col, row) => {
            f.put("kind", 1);
            pair(f, (col, row));
        }
        WrapAhead::Rest(col, row) => {
            f.put("kind", 2);
            pair(f, (col, row));
        }
    }
}

#[inline]
fn cheer_air_words(f: &mut Fold, air: CheerAir) {
    match air {
        CheerAir::Lanes(facing_left) => {
            f.put("kind", 0);
            f.flag("facing_left", facing_left);
        }
        CheerAir::Compact => f.put("kind", 1),
    }
}

#[inline]
fn star_catch_words(f: &mut Fold, c: StarCatch) {
    f.f32("x", c.x);
    f.f32("y", c.y);
    f.at("born", c.born);
    f.put("col", c.col as u32 as u64);
}

#[inline]
fn stance_words(f: &mut Fold, s: Stance) {
    f.f32("scale_x", s.scale_x);
    f.f32("scale_y", s.scale_y);
    f.f32("lift", s.lift);
}

#[inline]
fn color_key_words(f: &mut Fold, k: CatColorKey) {
    f.put("accent", u64::from(k.accent));
    f.put("background", u64::from(k.background));
}

#[inline]
fn flight_kind_words(f: &mut Fold, k: FlightKind) {
    f.flag("big", k.big);
    f.flag("show", k.show);
    f.flag("hop", k.hop);
    f.flag("drop", k.drop);
    f.flag("brisk", k.brisk);
    f.flag("spring", k.spring);
}

#[inline]
fn flight_words(f: &mut Fold, fl: Flight) {
    f.f32("from_col", fl.from_col);
    f.f32("from_row", fl.from_row);
    f.f32("to_col", fl.to_col);
    f.f32("to_row", fl.to_row);
    f.f32("dur", fl.dur);
    f.f32("arc", fl.arc);
    f.f32("t", fl.t);
    f.flag("reaimed", fl.reaimed);
    f.flag("big", fl.big);
    f.flag("show", fl.show);
    f.flag("hop", fl.hop);
    f.flag("coiled", fl.coiled);
    f.flag("drop", fl.drop);
    f.flag("late_move", fl.late_move);
    f.flag("running", fl.running);
    f.flag("brisk", fl.brisk);
    f.put("stage", u64::from(fl.stage));
    f.flag("spring", fl.spring);
}

#[inline]
fn pair(f: &mut Fold, (a, b): (f32, f32)) {
    f.f32("a", a);
    f.f32("b", b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::{Duration, Instant};

    fn words(p: &PetBrain, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        p.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_pet_field_moves_the_pet_words() {
        let t0 = Instant::now();
        let pet = PetBrain::new();
        let base = words(&pet, t0);
        assert_eq!(
            base,
            words(&PetBrain::new(), t0),
            "a fresh brain's words are fixed"
        );

        let mut walked = PetBrain::new();
        walked.col += 1.5;
        assert_ne!(words(&walked, t0), base, "the body's column is hashed");
        let mut wall = PetBrain::new();
        wall.wall_side = !wall.wall_side;
        assert_ne!(words(&wall, t0), base, "the wall side is hashed");
        let mut belled = PetBrain::new();
        belled.pending_bell = Some(t0 + Duration::from_millis(30));
        assert_ne!(
            words(&belled, t0),
            base,
            "a pending reaction's stamp is hashed"
        );
        let mut clocked = PetBrain::new();
        clocked.clock += 0.25;
        assert_ne!(words(&clocked, t0), base, "the brain's own clock is hashed");
    }

    /// A named change to a fresh brain, given the script's `t0`.
    type Case = (&'static str, fn(&mut PetBrain, Instant));

    fn ghost() -> Departure {
        Departure {
            born: 0.5,
            id: 3,
            coat: 2,
            iris: 1,
            from_col: 10.0,
            row: 4.0,
            edge: 90.0,
            speed: 12.0,
            still: false,
            facing_left: false,
            stride0: 0.0,
            pose0: PetGlyphId::PetSit,
            stance0: Stance::NEUTRAL,
            resight: false,
            own_glass: None,
            own_clear: false,
            peak: 1.0,
            colors: None,
            surface: None,
        }
    }

    fn star(t0: Instant) -> StarCatch {
        StarCatch {
            x: 120.0,
            y: 40.0,
            born: t0 + Duration::from_millis(9),
            col: 12,
        }
    }

    #[test]
    fn each_kept_reaction_stand_and_pose_moves_the_pet_words() {
        use super::super::PetMoteKind;
        let t0 = Instant::now();
        let base = words(&PetBrain::new(), t0);
        let cases: [Case; 30] = [
            ("the surface", |p, _| p.surface = Some((7, true))),
            ("a held stand", |p, _| {
                p.held_stash[1] = Some(HeldStand {
                    surface: None,
                    caret: (3, 9),
                    lead: 2.0,
                });
            }),
            ("the chrome hop", |p, _| p.chrome_hop = true),
            ("the wrap ahead", |p, _| {
                p.wrap_ahead = Some(WrapAhead::Rest(4.0, 2.0));
            }),
            ("the room cheer's air", |p, _| {
                p.room_cheer_air = Some(CheerAir::Compact);
            }),
            ("the offered star", |p, t0| p.pending_catch = Some(star(t0))),
            ("the caught star", |p, t0| p.caught = Some(star(t0))),
            ("the play landing", |p, _| {
                p.play_land = Some(PlayLand::BatSwipe { col: 3.0, row: 1.0 });
            }),
            ("the still pose", |p, _| {
                p.still_since = Some((0.5, PetGlyphId::PetSit));
            }),
            ("the still pose's scale", |p, _| p.still_scale = (1.1, 0.9)),
            ("the set-off ask", |p, _| {
                p.set_off_asked = Some([1, 2, 3, 4])
            }),
            ("the console's pose", |p, _| p.console_posed = true),
            ("the console resident", |p, t0| {
                p.note_console_completion(t0, true)
            }),
            ("the stroke detector", |p, t0| {
                p.stroke.sample(t0, Some((3.0, 1.0)), 2.0);
            }),
            ("a deferred arc", |p, _| {
                p.deferred_arc = Some((6.0, 2.0, 0.3, 1.0, FlightKind::PLAIN));
            }),
            ("the action to resume", |p, _| {
                p.resume = Some((PetAction::Sit, 0.4));
            }),
            ("a pending room cue", |p, _| {
                p.pending_room = Some(RoomCue::Mail)
            }),
            ("a pending trick", |p, t0| {
                p.pending_trick = Some(PendingTrick {
                    trick: Trick::Sit,
                    confirmed: true,
                    noted: t0,
                    stamped: t0 + Duration::from_millis(1),
                });
            }),
            ("the trick to go", |p, _| p.trick_go = Some(Trick::Jump)),
            ("a mote", |p, _| {
                p.motes[0] = Some(Mote {
                    kind: PetMoteKind::Note,
                    born: 0.2,
                    life: 1.0,
                    col: 5.0,
                    row: 2.0,
                    dir: 1.0,
                    seed: 4,
                    rgb: None,
                });
            }),
            ("the palette", |p, _| p.colors = Some(CatColorKey::DEFAULT)),
            ("a departure", |p, _| p.departures[0] = Some(ghost())),
            ("a pending re-sighting", |p, t0| {
                p.resight = Some(PendingResight {
                    ghost: ghost(),
                    at: t0,
                });
            }),
            ("a carried body", |p, _| {
                p.carried[0] = Some(Carried {
                    ghost: ghost(),
                    presence: 0.5,
                    home: false,
                });
            }),
            ("the front pass", |p, _| p.front_pass = Some(true)),
            ("the last stance", |p, _| {
                p.last_stance = Stance {
                    scale_x: 1.2,
                    ..Stance::NEUTRAL
                };
            }),
            ("the adopted look", |p, _| {
                p.adopted_look = Some(AdoptedLook {
                    pose: PetGlyphId::PetSit,
                    facing_left: true,
                    stance: Stance::NEUTRAL,
                    eased: 0.5,
                    seat: (2.0, 3.0),
                    beneath: None,
                });
            }),
            ("the head law's sink", |p, _| {
                p.sink_to = 0.78;
                p.sink_seat = Some(((4.0, 5.0), (4.0, 7.5)));
            }),
            ("the head law's fit", |p, _| p.fit_to = 0.54),
            ("the run's wide steps", |p, _| p.wide_steps = 2),
        ];
        for (what, set) in cases {
            let mut pet = PetBrain::new();
            set(&mut pet, t0);
            assert_ne!(words(&pet, t0), base, "{what} is hashed");
        }
    }

    #[test]
    fn a_surface_is_hashed_by_its_screen_and_whether_it_is_the_bodys() {
        let t0 = Instant::now();
        let on = |session| {
            let mut p = PetBrain::new();
            p.surface = Some((session, false));
            p.held_stash[0] = Some(HeldStand {
                surface: Some((session, false)),
                caret: (1, 1),
                lead: 0.0,
            });
            p
        };
        let base = words(&on(4), t0);
        assert_eq!(
            words(&on(9), t0),
            base,
            "a session's id is the process's count of terminals, not state"
        );
        let mut other = on(4);
        other.held_stash[0] = Some(HeldStand {
            surface: Some((5, false)),
            caret: (1, 1),
            lead: 0.0,
        });
        assert_ne!(
            words(&other, t0),
            base,
            "a stand held for another surface is hashed apart from one held for the body's"
        );
        let mut alt = on(4);
        alt.surface = Some((4, true));
        alt.held_stash[0] = Some(HeldStand {
            surface: Some((4, true)),
            caret: (1, 1),
            lead: 0.0,
        });
        assert_ne!(words(&alt, t0), base, "the surface's screen is hashed");
    }
}
