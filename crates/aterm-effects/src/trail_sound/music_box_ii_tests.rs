// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::rainbow_kitty_v2::{LANE_BASS, LANE_TUNE};
use super::super::{CHANNELS, SoundVoice, typed_glyph_class, typed_glyph_rank};
use super::*;
use crate::cursor_glow::GlowStyle;

const SR: f32 = 48_000.0;
const GAIN: f32 = 0.4;
const PROSE: &str = "the quick brown fox jumps over the lazy dog while seven wizards quietly \
                     mix a bright jade potion and the old sphinx of black quartz judges my vow";

fn synth() -> TrailSynth {
    TrailSynth::new(SR, 0x4B49_5454)
}

fn event(kind: SoundKind, shifted: bool) -> SoundEvent {
    SoundEvent {
        style: GlowStyle::RainbowKitty,
        voice: SoundVoice::MusicBoxII,
        kind: SoundGesture::Trail(kind),
        pan: 0.0,
        heat: 0.5,
        hue: 0.0,
        gain: GAIN,
        tone: Tone::Technical,
        bed: false,
        shifted,
    }
}

/// Push one key (`\n` is Enter, `\u{8}` Backspace) and return the voices it started.
fn key(s: &mut TrailSynth, ch: char, at: u32, session: u32) -> Vec<Voice> {
    let mark = s.born_seq;
    let kind = match ch {
        ' ' => SoundKind::Space,
        '\n' => SoundKind::Enter { cells: 10 },
        '\u{8}' => SoundKind::Backspace,
        _ => SoundKind::Typed,
    };
    let typed = matches!(kind, SoundKind::Typed | SoundKind::Space);
    s.push_meta(
        event(kind, ch.is_uppercase()),
        EventMeta {
            at_ms: at,
            glyph_class: if typed {
                typed_glyph_class(Some(ch))
            } else {
                0
            },
            rank: if typed { typed_glyph_rank(Some(ch)) } else { 0 },
            session,
            ..EventMeta::default()
        },
    );
    born_since(s, mark)
}

/// Render `ms` of audio in the app's 512-frame blocks, and return the voices born meanwhile.
fn rest_for(s: &mut TrailSynth, ms: u32) -> Vec<Voice> {
    let mut buf = [0.0f32; 512 * CHANNELS];
    let blocks = (f64::from(ms) * f64::from(SR) / 1_000.0 / 512.0).ceil() as usize;
    let mut born = Vec::new();
    for _ in 0..blocks {
        let mark = s.born_seq;
        s.render(&mut buf);
        born.extend(born_since(s, mark));
    }
    born
}

/// Enter at a shell prompt, on a line with something on it.
fn run_command(s: &mut TrailSynth, at: u32, session: u32) -> Vec<Voice> {
    let mark = s.born_seq;
    s.push_meta(
        event(SoundKind::Enter { cells: 10 }, false),
        EventMeta {
            at_ms: at,
            session,
            prompt: true,
            ..EventMeta::default()
        },
    );
    born_since(s, mark)
}

/// The result of the command the last Enter ran.
fn result(s: &mut TrailSynth, failed: bool, at: u32, session: u32) -> Vec<Voice> {
    let mark = s.born_seq;
    s.push_meta(
        SoundEvent {
            kind: SoundGesture::Output(OutputGesture::Verdict { failed, ice: false }),
            ..event(SoundKind::Typed, false)
        },
        EventMeta {
            at_ms: at,
            session,
            ..EventMeta::default()
        },
    );
    born_since(s, mark)
}

fn born_since(s: &TrailSynth, mark: u32) -> Vec<Voice> {
    let mut v: Vec<Voice> = s
        .voices
        .iter()
        .filter(|v| v.on && v.born > mark)
        .copied()
        .collect();
    v.sort_by_key(|v| v.born);
    v
}

/// The scale degree a voice sounds, searched over the tune and bass registers.
fn degree(v: &Voice) -> i32 {
    (BASS_LO..=TUNE_HI)
        .find(|d| (degree_hz(*d) - v.p[0].f0).abs() < 0.01)
        .unwrap_or_else(|| panic!("{} Hz is no scale degree", v.p[0].f0))
}

/// Type `text` from `t0`, one key every `period` ms: each key with the voices it started, and the time after.
fn type_text(
    s: &mut TrailSynth,
    text: &str,
    t0: u32,
    period: u32,
    session: u32,
) -> (Vec<(char, Vec<Voice>)>, u32) {
    let mut at = t0;
    let mut out = Vec::new();
    for ch in text.chars() {
        out.push((ch, key(s, ch, at, session)));
        at += period;
    }
    (out, at)
}

/// The tune's degrees for `text`, a word at a time.
fn words_of(keys: &[(char, Vec<Voice>)]) -> Vec<Vec<i32>> {
    let mut words = vec![Vec::new()];
    for (ch, voices) in keys {
        if *ch == ' ' || *ch == '\n' {
            words.push(Vec::new());
        } else {
            words.last_mut().unwrap().push(degree(&voices[0]));
        }
    }
    words.retain(|w| !w.is_empty());
    words
}

fn part(s: &TrailSynth, session: u32) -> Part {
    *s.ii.part(session).expect("the session has a part")
}

#[test]
fn every_key_is_one_note_at_once_in_the_scale_and_the_register() {
    let mut s = synth();
    let text = format!("{PROSE}. Is it so? Yes, it is!\n{PROSE}\n");
    let (keys, _) = type_text(&mut s, &text, 1_000, 150, 1);
    for (ch, voices) in &keys {
        assert_eq!(voices.len(), 1, "{ch:?} started {} voices", voices.len());
        let v = &voices[0];
        assert!(v.t == 0.0, "{ch:?} does not sound at once");
        let d = degree(v);
        if *ch == ' ' || *ch == '\n' {
            assert_eq!(v.lane, LANE_BASS, "{ch:?}");
            assert!((BASS_LO..=BASS_HI).contains(&d), "{ch:?} bass at {d}");
        } else {
            assert_eq!(v.lane, LANE_TUNE, "{ch:?}");
            assert!((TUNE_LO..=TUNE_HI).contains(&d), "{ch:?} at {d}");
        }
    }
}

#[test]
fn a_word_is_the_same_figure_wherever_it_is_typed() {
    let mut s = synth();
    let (keys, _) = type_text(&mut s, PROSE, 1_000, 150, 1);
    let words = words_of(&keys);
    let text: Vec<&str> = PROSE.split_whitespace().collect();
    assert_eq!(words.len(), text.len());
    let shape = |w: &[i32]| w.iter().map(|d| d - w[0]).collect::<Vec<_>>();
    let the: Vec<Vec<i32>> = text
        .iter()
        .zip(&words)
        .filter(|(t, _)| **t == "the")
        .map(|(_, w)| shape(w))
        .collect();
    assert_eq!(the.len(), 3);
    assert!(the.iter().all(|f| *f == the[0]), "`the` played {the:?}");
    let starts: std::collections::BTreeSet<i32> = text
        .iter()
        .zip(&words)
        .filter(|(t, _)| **t == "the")
        .map(|(_, w)| w[0])
        .collect();
    assert!(
        starts.len() > 1,
        "`the` always started on one pitch: the harmony never moved it"
    );
    // And the figure is its letters' alone: a word's opening is the opening of every word that starts with it.
    let fig = |w: &str| {
        let mut f = Figure::NEW;
        w.chars()
            .map(|c| {
                f = f.push(typed_glyph_rank(Some(c)));
                f.offset
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(fig("test"), fig("testing")[..4]);
    assert_ne!(fig("testing"), fig("tasting"));
    for w in &text {
        assert!(
            fig(w).iter().all(|o| o.abs() <= REACH),
            "{w} leaves its reach"
        );
    }
}

#[test]
fn enter_resets_nothing() {
    let mut s = synth();
    let (_, at) = type_text(
        &mut s,
        "git commit --amend and then push it all",
        1_000,
        150,
        1,
    );
    let before = part(&s, 1);
    assert!(before.keys > 30);
    let voices = key(&mut s, '\n', at, 1);
    assert_eq!(voices.len(), 1);
    assert_eq!(voices[0].lane, LANE_BASS);
    let after = part(&s, 1);
    assert_eq!(after.chord, before.chord, "Enter moved the harmony");
    assert_eq!(after.last, before.last, "Enter moved the tune");
    assert!(
        is_chord_tone(after.chord, after.bass),
        "the bass left the chord"
    );
    assert_eq!(after.keys, before.keys + 1);
    // The same line typed after two different histories starts in two different places.
    let mut other = synth();
    let (_, at2) = type_text(&mut other, "ls\n", 1_000, 150, 1);
    let (a, _) = type_text(&mut s, "make test", at + 2_000, 150, 1);
    let (b, _) = type_text(&mut other, "make test", at2 + 2_000, 150, 1);
    let state = |s: &TrailSynth| (part(s, 1).chord, part(s, 1).bass);
    assert!(
        words_of(&a) != words_of(&b) || state(&s) != state(&other),
        "a line sounds the same whatever came before it"
    );
}

#[test]
fn two_sessions_keep_separate_parts() {
    let mut alone = synth();
    let (a, _) = type_text(&mut alone, PROSE, 1_000, 200, 1);
    let mut shared = synth();
    let mut b = Vec::new();
    let other: Vec<char> = "cargo build && echo done; ls -la /tmp "
        .chars()
        .cycle()
        .take(PROSE.len())
        .collect();
    for (i, ch) in PROSE.chars().enumerate() {
        let at = 1_000 + 200 * i as u32;
        b.push((ch, key(&mut shared, ch, at, 1)));
        key(&mut shared, other[i], at + 90, 2);
    }
    let degrees =
        |keys: &[(char, Vec<Voice>)]| keys.iter().map(|(_, v)| degree(&v[0])).collect::<Vec<_>>();
    assert_eq!(
        degrees(&a),
        degrees(&b),
        "another session's typing moved this one's notes"
    );
    assert_ne!(part(&shared, 1).keys, 0);
    assert_ne!(part(&shared, 2).keys, 0);
}

#[test]
fn the_notes_do_not_depend_on_how_the_audio_was_rendered() {
    let text = format!("{PROSE}. And then? Again!\n{PROSE}");
    let run = |block: usize| {
        let mut s = synth();
        let mut buf = vec![0.0f32; block * CHANNELS];
        let (mut at, mut rendered_ms) = (1_000u32, 0.0f64);
        let mut notes = Vec::new();
        let mut note = |v: &Voice| {
            notes.push((v.p[0].f0.to_bits(), v.gl.to_bits(), v.gr.to_bits(), v.lane));
        };
        for (i, ch) in text.chars().enumerate() {
            // A long pause every 40 keys, rendered through: the rest falls inside it.
            at += if i % 40 == 39 { 3_130 } else { 130 };
            while rendered_ms < f64::from(at) {
                let mark = s.born_seq;
                s.render(&mut buf);
                rendered_ms += block as f64 * 1_000.0 / f64::from(SR);
                born_since(&s, mark).iter().for_each(&mut note);
            }
            key(&mut s, ch, at, 1).iter().for_each(&mut note);
        }
        notes
    };
    // Rendered at all, so no voice outlives its ring and none is stolen from the key that started it.
    let coarse = run(4_096);
    assert!(coarse.len() > text.len(), "the pauses were never answered");
    assert!(run(512) == coarse, "512-frame blocks changed the notes");
    assert!(run(64) == coarse, "64-frame blocks changed the notes");
}

#[test]
fn a_chord_holds_two_to_six_words() {
    let mut s = synth();
    let mut runs = Vec::new();
    let (mut chord, mut held, mut at) = (0, 0, 1_000);
    for (i, word) in PROSE.split_whitespace().cycle().take(120).enumerate() {
        // Fast and slow stretches, so the hold's speed term is exercised.
        let period = if (i / 20) % 2 == 0 { 110 } else { 260 };
        (_, at) = type_text(&mut s, word, at, period, 1);
        key(&mut s, ' ', at, 1);
        at += period;
        held += 1;
        let now = part(&s, 1).chord;
        if now != chord {
            runs.push(held);
            (chord, held) = (now, 0);
        }
        assert!((0..=5).contains(&now), "the chord on degree {now}");
        assert!(
            is_chord_tone(now, part(&s, 1).bass),
            "the bass is not in the chord"
        );
    }
    assert!(runs.len() >= 15, "the harmony barely moved: {runs:?}");
    assert!(
        runs.iter().all(|r| (2..=6).contains(r)),
        "a chord held {runs:?} words"
    );
    assert!(
        runs.iter().any(|r| *r >= 4) && runs.iter().any(|r| *r <= 3),
        "{runs:?}"
    );
}

#[test]
fn a_full_stop_comes_home_and_a_question_stays_open() {
    let mut s = synth();
    let (keys, at) = type_text(
        &mut s,
        "we went all the way up and came back down.",
        1_000,
        150,
        1,
    );
    let stop = degree(&keys.last().unwrap().1[0]);
    assert!(stop == 0 || stop == 7, "the full stop landed on {stop}");
    let bass = key(&mut s, ' ', at, 1);
    let p = part(&s, 1);
    assert_eq!((p.chord, p.bass, p.closed), (0, -7, true));
    assert_eq!(degree(&bass[0]), -7, "the close's bass is the tonic");
    assert!(bass[0].p[1].lvl > 0.0, "a full close sounds its fifth");

    let (keys, at) = type_text(&mut s, "is that the way home?", at + 150, 150, 1);
    let n = keys.len();
    let (before, mark) = (degree(&keys[n - 2].1[0]), degree(&keys[n - 1].1[0]));
    assert!(mark > before, "the question fell from {before} to {mark}");
    key(&mut s, ' ', at, 1);
    let p = part(&s, 1);
    assert_eq!((p.chord, p.bass, p.closed), (4, -3, false));

    // A stop inside a token ends nothing.
    let (_, at) = type_text(&mut s, "main.rs", at + 150, 150, 1);
    let chord = part(&s, 1).chord;
    key(&mut s, ' ', at, 1);
    assert!(!part(&s, 1).closed);
    assert!(part(&s, 1).chord == chord || part(&s, 1).chord != 0 || chord == 0);
}

#[test]
fn a_phrase_has_one_apex() {
    for sentence in [
        "all good things come to those who wait here.",
        "seven wizards quietly mix a bright jade potion now.",
        "every phrase should climb and turn and then come home.",
        "git push origin main and then open the pull request.",
        "zebras graze quietly under wide open skies at dusk.",
    ] {
        let mut s = synth();
        let (keys, _) = type_text(&mut s, sentence, 1_000, 150, 1);
        let words = words_of(&keys);
        let apex = part(&s, 1).apex_word as usize;
        assert!((2..=4).contains(&apex));
        let top = *words[apex].iter().max().unwrap();
        for (i, w) in words.iter().enumerate() {
            if i != apex {
                assert!(
                    w.iter().all(|d| *d < top),
                    "{sentence:?}: word {i} {w:?} reaches the apex {top} of word {apex}"
                );
            }
        }
        let first = words[0][0];
        assert!(top > first, "{sentence:?} never climbed");
        assert!(
            *words.last().unwrap().last().unwrap() < top,
            "{sentence:?} ended on its apex"
        );
    }
}

#[test]
fn a_key_at_a_secret_prompt_is_an_unpitched_tick_that_changes_nothing() {
    let mut s = synth();
    let (_, at) = type_text(&mut s, "sudo make", 1_000, 150, 1);
    let before = part(&s, 1);
    let mark = s.born_seq;
    for (i, kind) in [SoundKind::Typed, SoundKind::Space, SoundKind::Typed]
        .into_iter()
        .enumerate()
    {
        s.push_meta(
            event(kind, false),
            EventMeta {
                at_ms: at + 150 * i as u32,
                glyph_class: SECRET,
                rank: 0,
                session: 1,
                ..EventMeta::default()
            },
        );
    }
    let voices = born_since(&s, mark);
    assert_eq!(voices.len(), 3);
    for v in &voices {
        assert!(v.p.iter().all(|p| p.lvl == 0.0), "a secret key has a pitch");
        assert!(v.n_lvl > 0.0);
    }
    let after = part(&s, 1);
    assert_eq!(
        (
            after.keys,
            after.last,
            after.fig,
            after.chord,
            after.bass,
            after.phrase_words
        ),
        (
            before.keys,
            before.last,
            before.fig,
            before.chord,
            before.bass,
            before.phrase_words
        )
    );
    // And a session that has typed only secrets has no part at all.
    let mut fresh = synth();
    fresh.push_meta(
        event(SoundKind::Typed, false),
        EventMeta {
            at_ms: 1_000,
            glyph_class: SECRET,
            session: 9,
            ..EventMeta::default()
        },
    );
    assert!(fresh.ii.part(9).is_none());
}

#[test]
fn fast_typing_and_a_long_sitting_are_quieter_and_nothing_is_louder_than_a_first_key() {
    let level = |v: &Voice| v.gl + v.gr;
    let mean = |keys: &[(char, Vec<Voice>)]| {
        let tune: Vec<f32> = keys
            .iter()
            .filter(|(_, v)| v[0].lane == LANE_TUNE)
            .map(|(_, v)| level(&v[0]))
            .collect();
        tune.iter().sum::<f32>() / tune.len() as f32
    };
    let mut first = synth();
    let loudest = level(&key(&mut first, 'a', 1_000, 1)[0]) / (1.0 - TOUCH_SPREAD);
    let (mut slow, mut fast) = (synth(), synth());
    let (s, _) = type_text(&mut slow, PROSE, 1_000, 320, 1);
    let (f, _) = type_text(&mut fast, PROSE, 1_000, 60, 1);
    assert!(
        mean(&f) < 0.8 * mean(&s),
        "fast {} against slow {}",
        mean(&f),
        mean(&s)
    );
    for (ch, v) in s.iter().chain(&f) {
        if v[0].lane == LANE_TUNE {
            assert!(
                level(&v[0]) <= loudest * 1.0001,
                "{ch:?} is louder than a first key"
            );
        }
    }
    // Six thousand keys in, the same words are quieter than they were.
    let mut long = synth();
    let (fresh, mut at) = type_text(&mut long, PROSE, 1_000, 320, 1);
    for _ in 0..(6_000 / PROSE.len()) {
        (_, at) = type_text(&mut long, PROSE, at, 120, 1);
    }
    let (tired, _) = type_text(&mut long, PROSE, at + 5_000, 320, 1);
    assert!(
        mean(&tired) < 0.8 * mean(&fresh),
        "tired {} against fresh {}",
        mean(&tired),
        mean(&fresh)
    );
}

#[test]
fn backspace_takes_the_last_letter_back() {
    let mut s = synth();
    let (keys, at) = type_text(&mut s, "so far so good an", 1_000, 150, 1);
    let n = degree(&keys.last().unwrap().1[0]);
    key(&mut s, '\u{8}', at, 1);
    assert_eq!(
        degree(&key(&mut s, 'n', at + 150, 1)[0]),
        n,
        "the retyped letter moved"
    );
    // A word's first letter, taken back and typed again, starts where it did.
    let (keys, at) = type_text(&mut s, " t", at + 300, 150, 1);
    let t = degree(&keys.last().unwrap().1[0]);
    let before = part(&s, 1);
    key(&mut s, '\u{8}', at, 1);
    assert_eq!(
        degree(&key(&mut s, 't', at + 150, 1)[0]),
        t,
        "the retyped first letter moved"
    );
    let after = part(&s, 1);
    assert_eq!(
        (after.head, after.fig, after.phrase_words),
        (before.head, before.fig, before.phrase_words)
    );
}

/// A synth whose session 1 has typed a phrase that is left hanging, and the time of its last key.
fn hanging() -> (TrailSynth, u32) {
    for text in [
        PROSE,
        "one two three four",
        "a small phrase left open",
        "nothing is settled yet",
    ] {
        let mut s = synth();
        let (_, at) = type_text(&mut s, text, 1_000, 150, 1);
        let p = part(&s, 1);
        if !(p.chord == 0 && is_chord_tone(0, p.last)) {
            return (s, at - 150);
        }
    }
    panic!("every probe phrase resolved by itself");
}

#[test]
fn a_hanging_phrase_is_answered_once_when_the_rest_is_reached() {
    let (mut s, _) = hanging();
    assert!(
        rest_for(&mut s, REST_MS - 30).is_empty(),
        "answered before the pause was a rest"
    );
    let answer = rest_for(&mut s, 60);
    assert_eq!(answer.len(), 1);
    assert_eq!(answer[0].lane, LANE_TUNE);
    assert!(
        is_chord_tone(0, degree(&answer[0])),
        "the answer is not home"
    );
    let p = part(&s, 1);
    assert_eq!((p.chord, p.closed), (0, true));
    assert!(rest_for(&mut s, 5_000).is_empty(), "answered twice");

    // The answer starts on the sample the rest is reached, however the audio was cut into blocks.
    let onset = |block: usize| {
        let (mut s, _) = hanging();
        let mut buf = vec![0.0f32; block * CHANNELS];
        let mut frames = 0usize;
        loop {
            let mark = s.born_seq;
            s.render(&mut buf);
            if let Some(v) = born_since(&s, mark).first() {
                // `t` is the voice's age in seconds, negative until it sounds.
                return frames as f64 + f64::from(block as f32 - v.t * SR);
            }
            frames += block;
            assert!(frames < 3 * SR as usize, "the phrase was never answered");
        }
    };
    let at = onset(512);
    assert!(
        (at - f64::from(REST_MS) * 48.0).abs() < 2.0,
        "answered at frame {at}"
    );
    assert!((onset(4_096) - at).abs() < 2.0 && (onset(64) - at).abs() < 2.0);

    // A pause no audio was rendered through ends the phrase silently: its answer's time has passed.
    let (mut late, last) = hanging();
    let before = part(&late, 1);
    let back = key(&mut late, 'a', last + 30_000, 1);
    assert_eq!(
        back.len(),
        1,
        "the key after a long pause brought a second note"
    );
    assert_eq!(part(&late, 1).phrase_words, 0);
    assert!(before.phrase_words > 0);

    // A phrase that came home by itself is left alone.
    let mut home = synth();
    type_text(
        &mut home,
        "we went all the way up and came back down. ",
        1_000,
        150,
        1,
    );
    assert!(
        rest_for(&mut home, 3_000).is_empty(),
        "a closed phrase was answered"
    );
}

#[test]
fn cadence_enter_at_a_prompt_stops_on_the_dominant_and_a_pass_comes_home() {
    let mut s = synth();
    let (_, at) = type_text(&mut s, "git status", 1_000, 150, 1);
    let tune = part(&s, 1).last;
    let asked = run_command(&mut s, at, 1);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].lane, LANE_BASS);
    assert_eq!(
        degree(&asked[0]).rem_euclid(7),
        4,
        "Enter's bass is not the dominant"
    );
    let p = part(&s, 1);
    assert!(p.awaiting && p.chord == 4 && p.last == tune);
    // Nothing answers by itself while the result is out.
    assert!(rest_for(&mut s, 2_000).is_empty(), "the wait was filled");

    let home = result(&mut s, false, at + 2_100, 1);
    assert_eq!(home.len(), 2, "a pass is a bass and one note of the tune");
    assert_eq!(home[0].lane, LANE_BASS);
    assert_eq!(
        degree(&home[0]).rem_euclid(7),
        0,
        "the bass did not come home"
    );
    assert!(home[0].p[1].lvl > 0.0, "home has no fifth over it");
    assert_eq!(home[1].lane, LANE_TUNE);
    assert!(is_chord_tone(0, degree(&home[1])));
    assert!((degree(&home[1]) - tune).abs() <= 2, "the tune leapt home");
    let p = part(&s, 1);
    assert!(!p.awaiting && p.chord == 0 && p.closed);
    // One result, one answer; and the rest that follows adds nothing.
    assert!(result(&mut s, false, at + 2_200, 1).is_empty());
    assert!(rest_for(&mut s, 3_000).is_empty());
}

#[test]
fn cadence_a_failure_turns_aside_and_is_darker_and_no_louder_than_a_pass() {
    let run = |failed: bool| {
        let mut s = synth();
        let (_, at) = type_text(&mut s, "make test", 1_000, 150, 1);
        run_command(&mut s, at, 1);
        let voices = result(&mut s, failed, at + 900, 1);
        (voices, part(&s, 1))
    };
    let (pass, _) = run(false);
    let (fail, p) = run(true);
    assert_eq!(fail.len(), 2);
    assert_eq!(
        degree(&fail[0]).rem_euclid(7),
        5,
        "the bass is not the sixth"
    );
    assert_eq!(fail[0].p[1].lvl, 0.0, "a failure's bass is one note");
    assert_eq!(degree(&fail[1]).rem_euclid(7), 5);
    assert!(p.chord == 5 && !p.closed && !p.awaiting);
    assert!(fail[1].lp_cut < pass[1].lp_cut, "a failure is not darker");
    let level = |v: &Voice| v.gl + v.gr;
    assert!(level(&fail[0]) <= level(&pass[0]) && level(&fail[1]) <= level(&pass[1]));
    // Neither is louder than the key that asked.
    let mut s = synth();
    let first = level(&key(&mut s, 'a', 1_000, 1)[0]);
    assert!(level(&pass[1]) < first && level(&fail[1]) < first);
}

#[test]
fn cadence_a_result_answers_no_sooner_than_enters_bass_and_a_late_one_is_not_sounded() {
    let asked = || {
        let mut s = synth();
        let (_, at) = type_text(&mut s, "ls -la", 1_000, 150, 1);
        run_command(&mut s, at, 1);
        (s, at)
    };
    // With Enter: the answer waits out the rest of the least gap.
    let (mut s, at) = asked();
    let quick = result(&mut s, false, at + 50, 1);
    let wait = (CADENCE_MIN_MS - 50) as f32 * 0.001;
    assert!((quick[0].delay - wait).abs() < 1e-6, "{}", quick[0].delay);
    assert!(quick[1].delay > quick[0].delay);
    // After the gap: at once.
    let (mut s, at) = asked();
    assert_eq!(result(&mut s, false, at + 1_000, 1)[0].delay, 0.0);
    // At the limit it still answers; past it, nothing, and nothing later either.
    let (mut s, at) = asked();
    assert_eq!(result(&mut s, true, at + AWAIT_MS, 1).len(), 2);
    let (mut s, at) = asked();
    assert!(result(&mut s, true, at + AWAIT_MS + 1, 1).is_empty());
    assert!(!part(&s, 1).awaiting);
    assert!(result(&mut s, true, at + AWAIT_MS + 2, 1).is_empty());
    // Rendered through, the wait ends by itself at the limit, and in silence.
    let (mut s, at) = asked();
    assert!(rest_for(&mut s, AWAIT_MS + 200).is_empty());
    assert!(!part(&s, 1).awaiting);
    assert!(result(&mut s, false, at + 3_900, 1).is_empty());
}

#[test]
fn cadence_only_a_command_at_a_prompt_is_answered_and_only_in_its_own_session() {
    // Enter where no prompt reads it (a program, a multi-line editor) asks nothing.
    let mut s = synth();
    let (_, at) = type_text(&mut s, "a line in an editor", 1_000, 150, 1);
    key(&mut s, '\n', at, 1);
    assert!(!part(&s, 1).awaiting);
    assert!(result(&mut s, false, at + 500, 1).is_empty());
    assert!(result(&mut s, true, at + 600, 1).is_empty());
    // A result for a session that asked nothing, or has no part, changes nothing.
    let (_, at) = type_text(&mut s, "make", at + 2_000, 150, 1);
    type_text(&mut s, "other tab", at, 150, 2);
    // Rendered, so the notes typed so far have rung out and every voice has room.
    rest_for(&mut s, 1_000);
    run_command(&mut s, at + 2_000, 1);
    let before = (part(&s, 1), part(&s, 2));
    assert!(result(&mut s, true, at + 2_300, 2).is_empty());
    assert!(result(&mut s, true, at + 2_300, 9).is_empty());
    assert!(s.ii.part(9).is_none(), "a result made a part");
    assert_eq!(
        (part(&s, 1).chord, part(&s, 2).chord),
        (before.0.chord, before.1.chord)
    );
    assert_eq!(result(&mut s, true, at + 2_400, 1).len(), 2);
    // A result at zero gain (the window lost focus, or sound is off) is silent.
    let mut muted = synth();
    let (_, at) = type_text(&mut muted, "make", 1_000, 150, 1);
    run_command(&mut muted, at, 1);
    let mark = muted.born_seq;
    muted.push_meta(
        SoundEvent {
            kind: SoundGesture::Output(OutputGesture::Verdict {
                failed: false,
                ice: false,
            }),
            gain: 0.0,
            ..event(SoundKind::Typed, false)
        },
        EventMeta {
            at_ms: at + 500,
            session: 1,
            ..EventMeta::default()
        },
    );
    assert!(born_since(&muted, mark).is_empty());
}

#[test]
fn cadence_keys_typed_before_the_result_keep_the_tune_and_take_the_new_chord() {
    let mut s = synth();
    let (_, at) = type_text(&mut s, "make test", 1_000, 150, 1);
    run_command(&mut s, at, 1);
    let (keys, at) = type_text(&mut s, "git ", at + 300, 150, 1);
    assert!(
        keys.iter().all(|(_, v)| v.len() == 1),
        "a key lost its note"
    );
    let tune = part(&s, 1).last;
    let late = result(&mut s, false, at, 1);
    assert_eq!(late.len(), 1, "the result played over the typing's tune");
    assert_eq!(late[0].lane, LANE_BASS);
    let p = part(&s, 1);
    assert!(p.chord == 0 && p.last == tune && !p.closed && !p.awaiting);
}

#[test]
fn the_old_music_box_never_hears_the_new_ones_events() {
    let mut s = synth();
    type_text(&mut s, PROSE, 1_000, 150, 1);
    assert_eq!(s.v2.walk(), TrailSynth::new(SR, 0x4B49_5454).v2.walk());
    assert!(!s.v2_latched);
}

#[test]
fn the_arrow_keys_walk_the_scale_quietly_and_never_move_the_tune() {
    let mut s = synth();
    let (keys, at) = type_text(&mut s, "walk the line", 1_000, 150, 1);
    let last = degree(&keys.last().unwrap().1[0]);
    let typed = keys.last().unwrap().1[0].gl + keys.last().unwrap().1[0].gr;
    let before = part(&s, 1);
    let nav = |s: &mut TrailSynth, kind: SoundKind, at: u32| {
        let mark = s.born_seq;
        s.push_meta(
            event(kind, false),
            EventMeta {
                at_ms: at,
                session: 1,
                ..EventMeta::default()
            },
        );
        born_since(s, mark)
    };
    let mut walked = Vec::new();
    for (i, kind) in [
        SoundKind::Glide { dir: 1 },
        SoundKind::Glide { dir: 1 },
        SoundKind::Sweep { dir: -1 },
        SoundKind::Navigation,
        SoundKind::Glide { dir: -1 },
    ]
    .into_iter()
    .enumerate()
    {
        let v = nav(&mut s, kind, at + 100 * i as u32);
        assert_eq!(v.len(), 1, "{kind:?}");
        assert!(
            v[0].gl + v[0].gr < 0.8 * typed,
            "{kind:?} is as loud as a key"
        );
        walked.push(degree(&v[0]) - last);
    }
    assert_eq!(walked, [1, 2, 0, 0, -1]);
    let after = part(&s, 1);
    assert_eq!(
        (after.last, after.fig, after.chord, after.keys),
        (before.last, before.fig, before.chord, before.keys)
    );
    // A typed key ends the walk: the next arrow steps from the tune again.
    let (keys, at) = type_text(&mut s, "x", at + 1_000, 150, 1);
    let x = degree(&keys[0].1[0]);
    assert_eq!(
        degree(&nav(&mut s, SoundKind::Glide { dir: 1 }, at)[0]),
        (x + 1).min(TUNE_HI)
    );
    // A jump sounds one chord tone above the tune; the line feed of an Enter does not.
    let jump = nav(&mut s, SoundKind::Jump, at + 500);
    assert_eq!(jump.len(), 1);
    let d = degree(&jump[0]);
    assert!(is_chord_tone(part(&s, 1).chord, d) && (d > x || d == TUNE_HI));
    key(&mut s, '\n', at + 1_000, 1);
    assert!(
        nav(&mut s, SoundKind::Jump, at + 1_040).is_empty(),
        "Enter's line feed sounded"
    );
    assert_eq!(nav(&mut s, SoundKind::Jump, at + 1_400).len(), 1);
    assert!(nav(&mut s, SoundKind::Land, at + 1_500).is_empty());
}

/// Type `text`, one key every `period` ms, rendering the audio between keys in the app's
/// 512-frame blocks, then `tail_ms` more: the interleaved samples.
fn play(s: &mut TrailSynth, text: &str, period: u32, tail_ms: u32) -> Vec<f32> {
    fn render(s: &mut TrailSynth, ms: u32, out: &mut Vec<f32>) {
        let mut buf = [0.0f32; 512 * CHANNELS];
        let blocks = (f64::from(ms) * f64::from(SR) / 1_000.0 / 512.0).ceil() as usize;
        for _ in 0..blocks {
            s.render(&mut buf);
            out.extend_from_slice(&buf);
        }
    }
    let mut out = Vec::new();
    let mut at = 1_000;
    for ch in text.chars() {
        let _ = key(s, ch, at, 1);
        at += period;
        render(s, period, &mut out);
    }
    render(s, tail_ms, &mut out);
    out
}

fn peak_of(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

#[test]
fn body_every_voice_of_the_box_sounds_through_it_and_no_other_voice_does() {
    let mut s = synth();
    let (keys, at) = type_text(&mut s, "hello, world. Ok?\n", 1_000, 120, 1);
    assert!(keys.iter().flat_map(|(_, v)| v).all(|v| v.body));
    let cadence = run_command(&mut s, at, 1);
    assert!(!cadence.is_empty() && cadence.iter().all(|v| v.body));
    let answer = result(&mut s, false, at + 300, 1);
    assert!(!answer.is_empty() && answer.iter().all(|v| v.body));

    // The rainbow kitty's own music box is the instrument it was: nothing of it reaches the body.
    let mut old = synth();
    let mark = old.born_seq;
    let mut buf = [0.0f32; 512 * CHANNELS];
    for (n, ch) in "hello world".chars().enumerate() {
        let kind = if ch == ' ' {
            SoundKind::Space
        } else {
            SoundKind::Typed
        };
        old.push_meta(
            SoundEvent {
                voice: SoundVoice::RainbowKittyV2,
                ..event(kind, false)
            },
            EventMeta {
                at_ms: 1_000 + 120 * n as u32,
                glyph_class: typed_glyph_class(Some(ch)),
                rank: typed_glyph_rank(Some(ch)),
                ..EventMeta::default()
            },
        );
        assert!(born_since(&old, mark).iter().all(|v| !v.body));
        for _ in 0..12 {
            old.render(&mut buf);
            assert!(old.body.idle(), "the old box rang the new one's body");
        }
    }
    assert!(peak_of(&buf) > 0.0, "the old box was heard");
}

#[test]
fn body_changes_the_sound_and_never_raises_a_takes_peak() {
    for period in [70, 140, 260] {
        let mut with = synth();
        let mut without = synth();
        without.body_bypass = true;
        let a = play(&mut with, PROSE, period, 3_000);
        let b = play(&mut without, PROSE, period, 3_000);
        assert_eq!(a.len(), b.len());
        assert!(a != b, "the body changed nothing at {period} ms a key");
        let (pa, pb) = (peak_of(&a), peak_of(&b));
        assert!(
            pa <= pb,
            "the body raised the peak at {period} ms a key: {pa} over {pb}"
        );
        assert!(
            pa > 0.25 * pb,
            "the body all but silenced the take: {pa} of {pb}"
        );
    }
}

#[test]
fn body_the_box_is_exactly_silent_soon_after_its_last_note_ends() {
    // A phrase left hanging: its answer starts REST_MS after the last key and is the last note.
    let (mut s, _) = hanging();
    let mut buf = [0.0f32; 512 * CHANNELS];
    let block_ms = 512.0 * 1_000.0 / f64::from(SR);
    let mut blocks = 0u32;
    let mut answered_at = None;
    let mut ended_at = None;
    let silent_at = loop {
        let mark = s.born_seq;
        s.render(&mut buf);
        blocks += 1;
        if !born_since(&s, mark).is_empty() {
            answered_at = Some(blocks);
            ended_at = None;
        }
        if ended_at.is_none() && answered_at.is_some() && s.live_voices() == 0 {
            ended_at = Some(blocks);
        }
        // The typed notes can die away before the rest is reached: the silence that counts is
        // the one after the answer.
        if answered_at.is_some() && s.is_quiet() {
            break blocks;
        }
        assert!(blocks < 2_000, "the box never fell silent");
    };
    let answered_at = answered_at.expect("the hanging phrase was answered");
    let ended_at = ended_at.expect("the answer ended");
    let ring_ms = f64::from(silent_at - ended_at) * block_ms;
    let whole_ms = f64::from(silent_at - answered_at) * block_ms;
    // The body rings on after the note, and then stops: its ring, its longest delay and a block.
    // Measured: 416 ms after the answer ends, 2229 ms after it begins.
    assert!(
        (200.0..=600.0).contains(&ring_ms),
        "silence came {ring_ms:.0} ms after the last note ended"
    );
    assert!(
        whole_ms <= 2_500.0,
        "silence came {whole_ms:.0} ms after the last note began"
    );
    for _ in 0..4 {
        s.render(&mut buf);
        assert!(buf.iter().all(|x| *x == 0.0), "silence is exact zeros");
    }
    assert!(s.body.idle());
}

#[test]
fn body_the_ribbons_hue_is_where_the_key_strikes_and_never_adds_level() {
    let lo = 1.0 - STRIKE_DEPTH;
    let mut seen = (1.0f32, 1.0f32);
    for n in 0..=200 {
        let hue = n as f32 / 200.0;
        let (second, third) = strike_point(hue);
        assert!((lo..=1.0).contains(&second) && (lo..=1.0).contains(&third));
        // One overtone gives what the other takes.
        assert!((second + third - (2.0 - STRIKE_DEPTH)).abs() < 1e-4);
        seen = (seen.0.min(second), seen.1.min(third));
    }
    assert!(
        seen.0 < lo + 0.01 && seen.1 < lo + 0.01,
        "the travel is used"
    );
    let (a, b) = (strike_point(0.0), strike_point(1.0));
    assert!(
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4,
        "hue is a circle"
    );
    assert_eq!(strike_point(0.25), strike_point(1.25));

    // On a key: the same note at two hues differs only in its overtones, each no louder.
    let strike = |hue: f32| {
        let mut s = synth();
        let mark = s.born_seq;
        s.push_meta(
            SoundEvent {
                hue,
                ..event(SoundKind::Typed, false)
            },
            EventMeta {
                at_ms: 1_000,
                glyph_class: typed_glyph_class(Some('a')),
                rank: typed_glyph_rank(Some('a')),
                session: 1,
                ..EventMeta::default()
            },
        );
        born_since(&s, mark)[0]
    };
    let (red, cyan) = (strike(0.0), strike(0.5));
    assert_eq!(red.p[0].f0, cyan.p[0].f0);
    assert_eq!(red.p[0].lvl, cyan.p[0].lvl);
    assert!(red.p[1].lvl > cyan.p[1].lvl && red.p[2].lvl < cyan.p[2].lvl);
}
