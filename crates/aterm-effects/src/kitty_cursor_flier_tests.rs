// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flying cat's body: the pet setting's species in its glide, one body an appearance, the
//! face its reaction and eyes ask for, and no face shown for less than [`FACE_DWELL`].

use super::*;
use crate::kitty_pet::{glide_faces, glide_species};
use std::time::Duration;

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A cat flying `body`, summoned by a 25 ms run from `t`, and the summoning key's time.
fn summoned(body: Option<PetSpecies>, t: Instant) -> (CursorCat, Instant) {
    let mut c = CursorCat::default();
    c.set_body(body);
    for i in 0..400 {
        let at = t + ms(i * 25);
        c.on_key(at, true);
        if c.is_active() {
            return (c, at);
        }
    }
    panic!("a sustained 25 ms run summons");
}

/// Keep typing at 25 ms from `from` for `span`, sampling a frame every 4 ms.
fn cruise(c: &mut CursorCat, from: Instant, span: Duration) -> Vec<CatFrame> {
    let mut frames = Vec::new();
    let mut at = from;
    let end = from + span;
    let mut next_key = from;
    while at < end {
        if at >= next_key {
            c.on_key(at, true);
            next_key = at + ms(25);
        }
        frames.push(c.frame(at));
        at += ms(4);
    }
    frames
}

/// The pet setting picks the body: no species flies the head; the cat and the dog fly their
/// own glide, every frame of the appearance, and only while on glass. A change made in flight
/// lands on the next appearance (one appearance, one body), and hidden is the head again.
#[test]
fn the_flier_body_follows_the_pet_setting() {
    let t = Instant::now();
    for body in [None, Some(PetSpecies::Cat), Some(PetSpecies::Dog)] {
        let (mut c, at) = summoned(body, t);
        let frames = cruise(&mut c, at, ms(1500));
        for f in &frames {
            match (body, f.body) {
                (None, CursorBody::Head) => {}
                (Some(species), CursorBody::Glide(face)) => assert_eq!(
                    glide_species(face).map(|s| s as u8),
                    Some(species as u8),
                    "{face:?} is not {species:?}'s glide"
                ),
                (want, got) => panic!("{want:?} flew {got:?}"),
            }
        }
    }

    let (mut c, at) = summoned(Some(PetSpecies::Cat), t);
    c.set_body(Some(PetSpecies::Dog));
    for f in cruise(&mut c, at, ms(800)) {
        let CursorBody::Glide(face) = f.body else {
            panic!("a flying cat drew the head");
        };
        assert_eq!(
            glide_species(face).map(|s| s as u8),
            Some(PetSpecies::Cat as u8),
            "a change in flight waits for the next appearance"
        );
    }
    let quiet = at + Duration::from_secs(12);
    let hidden = c.frame(quiet);
    assert_eq!(hidden.alpha, 0, "a quiet cat hides");
    assert_eq!(hidden.body, CursorBody::Head, "hidden, nothing flies");
    let mut back = None;
    for i in 0..400 {
        let k = quiet + ms(i * 25);
        c.on_key(k, true);
        if c.is_active() {
            back = Some(k);
            break;
        }
    }
    let back = back.expect("summoned again");
    let CursorBody::Glide(face) = c.frame(back + ms(100)).body else {
        panic!("the next appearance flies the glide");
    };
    assert_eq!(
        glide_species(face).map(|s| s as u8),
        Some(PetSpecies::Dog as u8),
        "the latched setting lands on the next appearance"
    );
}

/// The face is the reaction's, else the eyes': a delete's oops is the wide-eyed face, an Enter
/// and a song are happy, a goodbye's flourish is happy, a blink is the blink, a fast run's
/// happy eyes are happy and the eased cruise is the open glide. Each of the four faces is reached. Control: the head flier (no species) shows
/// none of them, so the scan reads the body and not a constant.
#[test]
fn each_reaction_wears_its_glide_face() {
    FACE_LATCH_OFF.with(|c| c.set(true));
    let t = Instant::now();
    let g = glide_faces(PetSpecies::Cat);
    let scan = |body: Option<PetSpecies>| {
        let (mut c, at) = summoned(body, t);
        let mut seen: Vec<PetGlyphId> = Vec::new();
        let mut note = |f: &CatFrame| {
            if let CursorBody::Glide(face) = f.body {
                let want = match f.reaction {
                    CatReaction::Startled => g.oof,
                    CatReaction::Celebrate => g.happy,
                    _ if crate::companion::sing_face_live(f.sing) => g.happy,
                    _ => match f.pose.eyes {
                        EyesFrame::Blink => g.blink,
                        EyesFrame::Happy => g.happy,
                        EyesFrame::Open => g.fly,
                    },
                };
                assert_eq!(face, want, "{:?} with {:?} eyes", f.reaction, f.pose.eyes);
                if !seen.contains(&face) {
                    seen.push(face);
                }
            }
        };
        let frames = cruise(&mut c, at, Duration::from_secs(8));
        frames.iter().for_each(&mut note);
        let mut now = at + Duration::from_secs(8);
        c.on_key(now, false);
        now += ms(40);
        note(&c.frame(now));
        let mut later = now + ms(1500);
        for f in cruise(&mut c, now, ms(1500)) {
            note(&f);
        }
        c.on_enter(later);
        later += ms(40);
        note(&c.frame(later));
        let after = later + ms(2000);
        cruise(&mut c, later, ms(2000));
        c.set_singing(
            after,
            SingSync {
                drive: 1.0,
                beat: 0.0,
            },
        );
        note(&c.frame(after + ms(10)));
        c.set_singing(after + ms(20), SingSync::NONE);
        let mut quiet = after + ms(20);
        while quiet < after + Duration::from_secs(4) {
            note(&c.frame(quiet));
            quiet += ms(4);
        }
        seen
    };
    let seen = scan(Some(PetSpecies::Cat));
    for face in g.all() {
        assert!(seen.contains(&face), "{face:?} never shown: {seen:?}");
    }
    let none = scan(None);
    FACE_LATCH_OFF.with(|c| c.set(false));
    assert!(
        none.is_empty(),
        "control: the head flier wears no glide face"
    );

    // The goodbye's flourish: happy once the fade is a quarter through.
    let (mut c, at) = summoned(Some(PetSpecies::Cat), t);
    cruise(&mut c, at, ms(600));
    let out = at + ms(600);
    c.state = State::FadeOut(out);
    c.exit = CatExit::StarWink;
    let f = c.frame(out + Duration::from_secs_f32(FADE_OUT * 0.5));
    assert!(
        f.fade_out >= EXIT_FACE_FADE,
        "the fade is under way: {}",
        f.fade_out
    );
    assert_eq!(
        f.body,
        CursorBody::Glide(g.happy),
        "a star-wink goodbye is happy"
    );
}

/// Nothing blinks: a face, once on glass, stands at least [`FACE_DWELL`] (250 ms, so at most 4
/// face turns a second, under the 6-turn flash law with the breath's 2), however fast the
/// reactions alternate.
/// Control: with the latch off, Enter and Backspace alternating every 20 ms swap the face
/// faster than that.
#[test]
fn each_flier_face_stands_its_dwell() {
    let t = Instant::now();
    let shortest = |latch_off: bool| {
        FACE_LATCH_OFF.with(|c| c.set(latch_off));
        let (mut c, at) = summoned(Some(PetSpecies::Cat), t);
        cruise(&mut c, at, ms(500));
        let start = at + ms(500);
        let mut runs: Vec<(PetGlyphId, Duration)> = Vec::new();
        let mut shown: Option<(PetGlyphId, Instant)> = None;
        let mut step = 0u64;
        let mut now = start;
        while now < start + Duration::from_secs(3) {
            if step % 5 == 0 {
                c.momentum.set_value(now, 1.0);
                if (step / 5) % 2 == 0 {
                    c.on_enter(now);
                } else {
                    c.on_key(now, false);
                }
            }
            let f = c.frame(now);
            if let CursorBody::Glide(face) = f.body
                && f.alpha > 0
            {
                match shown {
                    Some((s, since)) if s != face => {
                        runs.push((s, now.saturating_duration_since(since)));
                        shown = Some((face, now));
                    }
                    None => shown = Some((face, now)),
                    Some(_) => {}
                }
            }
            step += 1;
            now += ms(4);
        }
        FACE_LATCH_OFF.with(|c| c.set(false));
        assert!(
            runs.len() >= 3,
            "the alternation must swap the face: {runs:?}"
        );
        // The first run began before the sampling did.
        runs[1..]
            .iter()
            .map(|&(_, d)| d)
            .min()
            .unwrap_or(Duration::MAX)
    };
    let held = shortest(false);
    assert!(
        held >= FACE_DWELL,
        "a face stood {held:?}, under the {FACE_DWELL:?} dwell"
    );
    let bare = shortest(true);
    assert!(
        bare < FACE_DWELL,
        "control: without the latch the 20 ms alternation swaps faster ({bare:?})"
    );
}
