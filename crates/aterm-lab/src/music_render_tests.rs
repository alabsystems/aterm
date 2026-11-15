// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const LINE: &str = "Hello, kitty! Is this (a) tune?\nls -la\n";

#[test]
fn the_same_text_seed_and_voice_render_the_same_bytes_twice() {
    let a = wav_bytes(
        &render(
            &type_text(LINE, 7, 70),
            SoundVoice::RainbowKittyV2,
            0.4,
            7,
            8,
        )
        .samples,
    );
    let b = wav_bytes(
        &render(
            &type_text(LINE, 7, 70),
            SoundVoice::RainbowKittyV2,
            0.4,
            7,
            8,
        )
        .samples,
    );
    assert!(a == b, "two renders of one take differ");
    let other = type_text(LINE, 8, 70);
    assert_ne!(
        type_text(LINE, 7, 70),
        other,
        "the seed does not move the timing"
    );
}

#[test]
fn a_render_sounds_and_then_ends_in_exact_zeros() {
    let take = type_text(LINE, 3, 70);
    let r = render(&take, SoundVoice::RainbowKittyV2, 0.4, 3, 8);
    assert!(peak(&r.samples) > 0.01, "the take made no sound");
    assert!(peak(&r.samples) <= 1.0, "the take clips");
    let silent = r.silent_frame.expect("the tail never reached silence");
    assert!(silent > r.last_key_frame);
    let tail = &r.samples[silent * CHANNELS..];
    assert!(
        tail.len() >= BLOCK_FRAMES * CHANNELS,
        "nothing was rendered after silence"
    );
    assert!(
        tail.iter().all(|s| *s == 0.0),
        "the tail is not exact zeros"
    );
    assert!(
        r.samples[..silent * CHANNELS]
            .iter()
            .rev()
            .take(BLOCK_FRAMES * CHANNELS)
            .any(|s| *s != 0.0),
        "silence began before the frame named"
    );
    assert!(!r.block_ns.is_empty(), "no block was timed");
}

#[test]
fn a_take_is_one_event_a_key_in_time_order_with_shift_before_a_capital() {
    let take = type_text(LINE, 5, 70);
    assert!(
        take.windows(2).all(|w| w[0].at_ms <= w[1].at_ms),
        "events out of order"
    );
    let keyed = take.iter().filter(|k| k.kind != SoundKind::Shift).count();
    assert_eq!(keyed, LINE.chars().count(), "a key made no event, or two");
    assert_eq!(
        take[0].kind,
        SoundKind::Shift,
        "the capital H is announced by its Shift"
    );
    assert_eq!(take[1].ch, Some('H'));
    assert!(take[0].at_ms < take[1].at_ms);
    let enters: Vec<&Key> = take
        .iter()
        .filter(|k| matches!(k.kind, SoundKind::Enter { .. }))
        .collect();
    assert_eq!(enters.len(), 2);
    assert_eq!(
        enters[1].kind,
        SoundKind::Enter { cells: 6 },
        "Enter flies the line it ends"
    );
    let spaces = take.iter().filter(|k| k.kind == SoundKind::Space).count();
    assert_eq!(spaces, LINE.matches(' ').count());
    assert!(
        take.iter()
            .all(|k| (0.0..=1.0).contains(&k.heat) && (0.0..=1.0).contains(&k.flow))
    );
}

#[test]
fn a_faster_typist_finishes_sooner_and_enter_is_followed_by_a_pause() {
    let slow = type_text(LINE, 5, 40);
    let fast = type_text(LINE, 5, 120);
    assert!(fast.last().unwrap().at_ms < slow.last().unwrap().at_ms);
    let take = type_text(LINE, 5, 70);
    let enter = take
        .iter()
        .position(|k| matches!(k.kind, SoundKind::Enter { .. }))
        .unwrap();
    assert!(
        take[enter + 1].at_ms - take[enter].at_ms >= 900,
        "no pause to read the result"
    );
}

#[test]
fn the_wav_header_names_what_follows_it() {
    let samples = [0.25f32, -0.25, 0.5, -0.5];
    let b = wav_bytes(&samples);
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    assert_eq!(&b[..4], b"RIFF");
    assert_eq!(u32_at(4) as usize, b.len() - 8);
    assert_eq!(&b[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([b[20], b[21]]), 3, "IEEE float");
    assert_eq!(u16::from_le_bytes([b[22], b[23]]), 2, "stereo");
    assert_eq!(u32_at(24), SAMPLE_RATE);
    assert_eq!(&b[38..42], b"fact");
    assert_eq!(u32_at(46), 2, "two frames");
    assert_eq!(&b[50..54], b"data");
    assert_eq!(u32_at(54) as usize, samples.len() * 4);
    assert_eq!(b.len(), 58 + samples.len() * 4);
    assert_eq!(f32::from_le_bytes(b[58..62].try_into().unwrap()), 0.25);
}

#[test]
fn voices_on_one_take_are_matched_on_energy_and_never_raised() {
    let take = type_text(LINE, 9, 70);
    let mut takes: Vec<Vec<f32>> = [
        SoundVoice::RainbowKittyV2,
        SoundVoice::Marimba,
        SoundVoice::Felt,
    ]
    .iter()
    .map(|v| render(&take, *v, 0.4, 9, 8).samples)
    .collect();
    let before: Vec<f64> = takes.iter().map(|t| rms(t)).collect();
    assert!(
        before.iter().all(|l| *l > 0.0),
        "a voice was silent: {before:?}"
    );
    let gains = match_energy(&mut takes);
    let after: Vec<f64> = takes.iter().map(|t| rms(t)).collect();
    let quietest = before.iter().copied().fold(f64::INFINITY, f64::min);
    for (level, gain) in after.iter().zip(&gains) {
        assert!(
            (level / quietest - 1.0).abs() < 1e-3,
            "{after:?} is not level with {quietest}"
        );
        assert!(*gain <= 1.0, "a voice was raised: {gains:?}");
    }
    assert_eq!(
        match_energy(&mut [vec![0.0; 8]]),
        vec![1.0],
        "a silent take is left alone"
    );
}

#[test]
fn the_command_line_is_held_to_its_flags() {
    let args = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
    let a = parse_args(&args(&[
        "--voice",
        "music box,felt",
        "--voice",
        "felt",
        "--wpm",
        "90",
        "out",
    ]))
    .unwrap();
    assert_eq!(a.voices, [SoundVoice::RainbowKittyV2, SoundVoice::Felt]);
    assert_eq!((a.wpm, a.seed, a.tail_secs, a.matched), (90, 1, 8, true));
    assert_eq!(a.text, DEFAULT_TEXT);
    assert_eq!(a.results, None);
    let shell = parse_args(&args(&["--results", "pf-"])).unwrap();
    assert_eq!(shell.results.as_deref(), Some("pf-"));
    for bad in [
        &["--voice", "kazoo"][..],
        &["--wpm", "fast"],
        &["--gain", "2"],
        &["--text", "a", "--text-file", "b"],
        &["--text", "  "],
        &["--loud"],
        &["a", "b"],
        &["--stem", "a/b"],
        &["--results", "pass"],
        &["--results", ""],
    ] {
        let Err(fail) = parse_args(&args(bad)) else {
            panic!("{bad:?} was accepted");
        };
        assert_eq!(fail.code, 2, "{bad:?}");
    }
}

#[test]
fn at_a_prompt_each_command_gets_its_result_and_an_empty_line_asks_nothing() {
    let take = type_text("ls\n\ntrue\nfalse\nvim\n", 5, 70);
    let shell = at_a_prompt(&take, "pf-");
    assert!(
        shell.windows(2).all(|w| w[0].at_ms <= w[1].at_ms),
        "events out of order"
    );
    let enters: Vec<bool> = shell
        .iter()
        .filter(|k| k.result.is_none() && matches!(k.kind, SoundKind::Enter { .. }))
        .map(|k| k.prompt)
        .collect();
    assert_eq!(enters, [true, false, true, true, true]);
    // `ls` passes, `true` fails, `false` gives no result, and `vim` is the first letter again.
    let results: Vec<bool> = shell.iter().filter_map(|k| k.result).collect();
    assert_eq!(results, [false, true, false]);
    for (i, k) in shell.iter().enumerate().filter(|(_, k)| k.result.is_some()) {
        assert!(shell[i - 1].prompt, "a result follows no command");
        assert_eq!(k.at_ms, shell[i - 1].at_ms + RESULT_AFTER_MS);
    }
    let keys: Vec<Key> = shell
        .iter()
        .filter(|k| k.result.is_none())
        .map(|k| Key {
            prompt: false,
            ..*k
        })
        .collect();
    assert_eq!(keys, take, "the keys of the take changed");
    assert!(
        at_a_prompt(&take, "").iter().all(|k| k.result.is_none()),
        "no letters, no results"
    );
}

#[test]
fn a_quick_pass_is_heard_by_the_new_music_box_alone() {
    let take = type_text("git status\n", 3, 70);
    let passed = at_a_prompt(&take, "p");
    let failed = at_a_prompt(&take, "f");
    let heard = |t: &[Key], voice| render(t, voice, 0.4, 3, 8).samples;
    // The longer of two takes may only add silence where the shorter one stopped.
    let same = |a: &[f32], b: &[f32]| {
        let n = a.len().min(b.len());
        a[..n] == b[..n] && a[n..].iter().chain(&b[n..]).all(|s| *s == 0.0)
    };
    let old = heard(&take, SoundVoice::RainbowKittyV2);
    assert!(
        same(&old, &heard(&passed, SoundVoice::RainbowKittyV2)),
        "the music box heard a quick pass"
    );
    assert!(
        !same(&old, &heard(&failed, SoundVoice::RainbowKittyV2)),
        "the music box did not hear a failure"
    );
    let new = heard(&take, SoundVoice::MusicBoxII);
    let home = heard(&passed, SoundVoice::MusicBoxII);
    assert!(!same(&new, &home), "the new music box did not hear a pass");
    assert!(
        !same(&home, &heard(&failed, SoundVoice::MusicBoxII)),
        "a pass and a failure sound the same"
    );
}
