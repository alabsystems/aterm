// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let cases: [Case; 38] = [
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
        ("the wall that holds no seat", |p, _| p.unseated = Some(13)),
        ("the wall's cut", |p, _| p.wall_cut = 0.5),
        ("the wall's cut falling", |p, _| p.wall_cut_falls = true),
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
            p.sink_seat = Some(((4.0, 5.0), (4.0, 7.5), 0));
        }),
        ("the walk's stand", |p, _| p.walk_to = Some((6.0, 5.0))),
        ("the head law's fit", |p, _| p.fit_to = 0.54),
        ("the run's wide steps", |p, _| p.wide_steps = 2),
        ("the ear twitch cooldown", |p, _| p.twitch_cool = 2.0),
        ("the spent idle ear twitch", |p, _| p.twitch_spent = true),
        ("the sibling burst stamp", |p, t0| {
            p.room_sibling_at = Some(t0 + Duration::from_millis(20))
        }),
        ("the spent sibling look", |p, _| p.room_sibling_spent = true),
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
