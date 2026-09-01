// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `OutputStreakEpisodeDelivery`: drive the shipping visual
//! engine and synth, then project their observable episode edges onto the
//! derived model. The negative controls prove the projection would reject each
//! regression rather than merely replaying a happy script. `RetireVisuals`
//! binds `OutputStreak::next_change_deadline`; native/web event-loop wiring of
//! that public deadline remains covered by its separate host-level unit tests.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{Geom, GlowStyle};
use aterm_effects::output_streak::{OutputStreak, StreakConfig, StreakSound};
use aterm_effects::tone::Tone;
use aterm_effects::trail_sound::{SoundEvent, SoundGesture, SoundKind, SoundVoice, TrailSynth};
use aterm_spec::derive::{Model, output_streak_episode_delivery_model};

type State = BTreeMap<&'static str, i64>;

#[derive(Clone, Copy, Default)]
struct Observation {
    phase: i64,
    shimmer: bool,
    settle: bool,
    wake: bool,
    human_gap: bool,
    audio_checked: bool,
    next_human: bool,
}

fn project(model: &Model, observed: Observation) -> State {
    let mut state = model.init_state();
    for (name, value) in [
        ("phase", observed.phase),
        ("shimmer", i64::from(observed.shimmer)),
        ("settle", i64::from(observed.settle)),
        ("wake", i64::from(observed.wake)),
        ("human_gap", i64::from(observed.human_gap)),
        ("audio_checked", i64::from(observed.audio_checked)),
        ("next_human", i64::from(observed.next_human)),
    ] {
        state.insert(name, value);
    }
    state
}

fn assert_transition(model: &Model, action: &str, before: &State, after: &State) {
    assert!(
        model
            .successors(action, before)
            .iter()
            .any(|next| next == after),
        "shipping observation must refine {action}: {before:?} -> {after:?}"
    );
}

fn sound(kind: SoundGesture) -> SoundEvent {
    SoundEvent {
        style: GlowStyle::RainbowKitty,
        voice: SoundVoice::Style,
        kind,
        pan: 0.0,
        heat: 0.0,
        hue: 0.3,
        gain: 0.4,
        tone: Tone::Technical,
        bed: false,
        shifted: false,
    }
}

fn geometry() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 800,
        win_h: 480,
        head: 0,
    }
}

#[test]
fn real_output_episode_and_synth_refine_delivery_model() {
    let model = output_streak_episode_delivery_model();
    let healthy_actions = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !name.starts_with("Buggy"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        healthy_actions,
        std::collections::BTreeSet::from([
            "CheckOutputDoesNotClaimHuman",
            "HumanGapElapses",
            "HumanVoice",
            "NextHuman",
            "OpenOutput",
            "RetireVisuals",
            "SettleAtWake",
        ]),
        "Tier-1 must name every healthy action added to the model"
    );
    let idle = project(&model, Observation::default());

    // The shared human gap is occupied by Enter/Jump. The shipping synth must
    // still admit the episode's sole opening cue immediately afterward.
    let mut synth = TrailSynth::new(48_000.0, 0x0A17_0A17);
    synth.push(sound(SoundGesture::Trail(SoundKind::Jump)));
    let human_voices = synth.live_voices();
    assert!(human_voices > 0, "the human control voice must be audible");
    synth.push(sound(SoundGesture::Trail(SoundKind::Typed)));
    assert_eq!(
        synth.live_voices(),
        human_voices,
        "an immediate ordinary key proves the shared human gap is occupied"
    );
    let after_human = project(
        &model,
        Observation {
            human_gap: true,
            ..Observation::default()
        },
    );
    assert_transition(&model, "HumanVoice", &idle, &after_human);

    let t0 = Instant::now();
    let mut streak = OutputStreak::new(17);
    let cfg = StreakConfig {
        enabled: true,
        sound: true,
        idle_secs: 10.0,
        ..StreakConfig::default()
    };
    let geom = geometry();
    assert!(!streak.note_output(1, &[(4, 0, 40)], t0, false));
    let spawned = t0 + Duration::from_millis(350);
    assert!(streak.note_output(2, &[(4, 0, 40)], spawned, false));
    let mut quads = Vec::new();
    let opened = streak.tick(spawned, geom, &cfg, &mut quads);
    let opening = opened.cue.expect("the visible opening owns one cue");
    assert_eq!(opening.sound, StreakSound::Shimmer);
    synth.push(sound(SoundGesture::Output(opening.sound.output_gesture())));
    assert!(
        synth.live_voices() > human_voices,
        "the mapped one-shot Shimmer must bypass the occupied shared gap"
    );
    assert!(streak.is_active());
    let visible = project(
        &model,
        Observation {
            phase: 1,
            shimmer: true,
            human_gap: true,
            ..Observation::default()
        },
    );
    assert_transition(&model, "OpenOutput", &after_human, &visible);

    // Four bounded 250 ms integration steps retire the longest comet. The
    // episode remains open, so the public analytic deadline must own one wake.
    for step in 1_u64..=4 {
        quads.clear();
        streak.tick(
            spawned + Duration::from_millis(250 * step),
            geom,
            &cfg,
            &mut quads,
        );
    }
    let parked_at = spawned + Duration::from_secs(1);
    assert!(!streak.is_active());
    let settle_at = streak
        .next_change_deadline(parked_at, cfg.idle_secs)
        .expect("an open parked episode owes exactly one settle wake");
    assert!(settle_at > parked_at);
    let parked = project(
        &model,
        Observation {
            phase: 2,
            shimmer: true,
            wake: true,
            human_gap: true,
            ..Observation::default()
        },
    );
    assert_transition(&model, "RetireVisuals", &visible, &parked);

    quads.clear();
    let closed = streak.tick(settle_at + Duration::from_millis(1), geom, &cfg, &mut quads);
    let closing = closed.cue.expect("the settle crossing owns one cue");
    assert_eq!(closing.sound, StreakSound::Settle);
    let before_settle = synth.live_voices();
    synth.push(sound(SoundGesture::Output(closing.sound.output_gesture())));
    assert!(
        synth.live_voices() > before_settle,
        "the shipping mapper must deliver the emitted settle cue"
    );
    assert_eq!(streak.next_change_deadline(settle_at, cfg.idle_secs), None);
    let settled = project(
        &model,
        Observation {
            phase: 3,
            shimmer: true,
            settle: true,
            human_gap: true,
            ..Observation::default()
        },
    );
    assert_transition(&model, "SettleAtWake", &parked, &settled);

    // A fresh synth starts with the human gap open. An output cue followed
    // immediately by a typed key proves that output did not claim that slot.
    let ready = project(
        &model,
        Observation {
            phase: 3,
            shimmer: true,
            settle: true,
            ..Observation::default()
        },
    );
    assert_transition(&model, "HumanGapElapses", &settled, &ready);
    let mut open_gap = TrailSynth::new(48_000.0, 7);
    open_gap.push(sound(SoundGesture::Output(
        StreakSound::Shimmer.output_gesture(),
    )));
    let output_voices = open_gap.live_voices();
    assert!(output_voices > 0);
    let checked = project(
        &model,
        Observation {
            phase: 3,
            shimmer: true,
            settle: true,
            audio_checked: true,
            ..Observation::default()
        },
    );
    assert_transition(&model, "CheckOutputDoesNotClaimHuman", &ready, &checked);
    open_gap.push(sound(SoundGesture::Trail(SoundKind::Typed)));
    assert!(
        open_gap.live_voices() > output_voices,
        "machine output cannot steal the following human key's slot"
    );
    let next_human = project(
        &model,
        Observation {
            phase: 3,
            shimmer: true,
            settle: true,
            human_gap: true,
            audio_checked: true,
            next_human: true,
            ..Observation::default()
        },
    );
    assert_transition(&model, "NextHuman", &checked, &next_human);

    // Projection-level negative controls: each former implementation shape
    // violates the precise invariant the real observations above established.
    let mut thinned = visible.clone();
    thinned.insert("shimmer", 0);
    assert!(!model.check_invariant("OpeningCueSurvivesSharedGap", &thinned));
    let mut missed_wake = parked.clone();
    missed_wake.insert("wake", 0);
    assert!(!model.check_invariant("ParkedEpisodeOwnsSettleWake", &missed_wake));
    let mut claimed_gap = checked;
    claimed_gap.insert("human_gap", 1);
    assert!(!model.check_invariant("OutputNeverClaimsHumanGap", &claimed_gap));
}
