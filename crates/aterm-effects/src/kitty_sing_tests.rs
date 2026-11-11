// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const S: u64 = 7;

fn external_deadline_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        ExternalCelebrationDeadline {
            const Buggy = 0;
            const Bars = 4;
            var phase = 0;
            var consumed = 0;
            var deadline = 0;
            action Consume when (phase <= Bars + 1 && consumed == 0) {
                consumed = 1;
                deadline = if Buggy == 1 { phase } else { phase + 1 };
            }
            action Advance when (phase <= Bars && consumed == 1) {
                phase = phase + 1;
                consumed = 0;
            }
            invariant AConsumedEventCannotRearmNow: consumed == 0 || deadline > phase;
            invariant Bounded: phase <= Bars + 1 && deadline <= Bars + 2 && consumed <= 1;
        }
    }
}

#[test]
fn external_deadline_model_proves_and_catches_a_rearmed_consumed_event() {
    let model = external_deadline_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn external_deadline_conforms_through_bars_outro_and_settle() {
    let model = external_deadline_model();
    let mut expected = model.init_state();
    let mut sing = KittySing::default();
    let start = Instant::now();
    assert_eq!(sing.next_external_deadline(start), None);
    sing.arm_external(start, S, song_signature('C'), 4, CelebrateOn::Green)
        .unwrap();
    assert_eq!(
        sing.next_external_deadline(start),
        None,
        "a pending arm is inert"
    );
    sing.note_green_block(start, S);
    let end = start + Duration::from_secs_f32(4.0 * SING_BAR_SECONDS);
    let mut now = start;
    for phase in 0..=5 {
        assert_eq!(
            sing.next_external_deadline(now),
            Some(now),
            "the current edge is due"
        );
        if phase < 4 {
            assert_eq!(sing.take_bar_fan(now).unwrap().bar, phase);
        } else if phase == 4 {
            assert!(sing.take_outro(now).is_some());
            assert!(sing.take_outro(now).is_none(), "the outro is consumed once");
        } else {
            sing.settle(now);
        }
        assert!(model.fire("Consume", &mut expected));
        let next = sing.next_external_deadline(now);
        let mut actual = expected.clone();
        actual.insert(
            "deadline",
            match next {
                Some(at) if at > now => phase as i64 + 1,
                Some(_) => phase as i64,
                None => 6,
            },
        );
        assert_eq!(
            actual, expected,
            "the real consumer advances its offered deadline"
        );
        let mut stuck = actual.clone();
        stuck.insert("deadline", phase as i64);
        assert!(!model.check_invariant("AConsumedEventCannotRearmNow", &stuck));
        if phase < 5 {
            let at = next.expect("a finite run still owes an event");
            let exact = if phase < 4 {
                start + Duration::from_secs_f32((phase + 1) as f32 * SING_BAR_SECONDS)
            } else {
                end + Duration::from_secs_f32(SING_WIND_DOWN)
            };
            assert_eq!(at, exact);
            assert_eq!(
                sing.next_external_deadline(now + Duration::from_millis(1)),
                Some(at),
                "unrelated parks retain the same future offer"
            );
            now = at;
            assert!(model.fire("Advance", &mut expected));
        } else {
            assert_eq!(next, None, "settled runs owe no idle wake");
        }
    }
}

#[test]
fn external_deadline_does_not_schedule_held_keys_or_replay_missed_bars() {
    let start = Instant::now();
    let mut held = KittySing::default();
    let now = hold(&mut held, start, 'a', SING_ARM_REPEATS, 30);
    assert!(held.is_armed(now));
    assert_eq!(held.next_external_deadline(now), None);
    let mut external = KittySing::default();
    external
        .arm_external(start, S, song_signature('C'), 4, CelebrateOn::Enter)
        .unwrap();
    external.note_keyed_enter(start, S);
    let late = start + Duration::from_secs_f32(2.5 * SING_BAR_SECONDS);
    assert_eq!(external.next_external_deadline(late), Some(late));
    assert_eq!(external.take_bar_fan(late).unwrap().bar, 2);
    assert!(external.next_external_deadline(late).unwrap() > late);
    assert!(
        external.take_bar_fan(late).is_none(),
        "missed bars are not replayed"
    );
}

fn hold(d: &mut KittySing, t0: Instant, ch: char, n: u32, gap_ms: u64) -> Instant {
    let mut t = t0;
    for i in 0..n {
        t = t0 + Duration::from_millis(u64::from(i) * gap_ms);
        d.note_char(t, S, ch);
    }
    t
}

#[test]
fn threshold_at_cadence_arms_full_sing() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let before = hold(&mut d, t0, 'a', SING_ARM_REPEATS - 1, 30);
    assert!(!d.is_armed(before), "a sub-threshold hold must not arm");
    assert_eq!(d.drive(before), 0.0);
    let armed = before + Duration::from_millis(30);
    d.note_char(armed, S, 'a');
    assert!(d.is_armed(armed), "the threshold repeat arms");
    assert_eq!(d.drive(armed), 1.0);
    assert_eq!(
        d.beat(armed),
        Some(0.0),
        "the beat clock anchors at the arm"
    );
}

#[test]
fn a_batched_ime_commit_of_repeats_does_not_arm() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    for _ in 0..16 {
        d.note_char(t0, S, 'w');
    }
    assert!(
        !d.is_armed(t0),
        "a single batched commit of repeats is one step, not a hold"
    );
    assert_eq!(d.drive(t0), 0.0);
    let mut held = KittySing::default();
    let armed_at = hold(&mut held, t0, 'w', SING_ARM_REPEATS, 30);
    assert!(
        held.is_armed(armed_at),
        "real repeats across time still arm at {SING_ARM_REPEATS}"
    );
}

#[test]
fn interleaved_chars_never_arm() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    for i in 0..64u64 {
        let t = t0 + Duration::from_millis(i * 30);
        d.note_char(t, S, if i % 2 == 0 { 'a' } else { 'b' });
        assert!(!d.is_armed(t), "alternating keys must never arm (i={i})");
    }
}

#[test]
fn backspace_never_arms_and_breaks_a_run() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    for i in 0..32u64 {
        d.note_backspace(t0 + Duration::from_millis(i * 30));
    }
    assert!(!d.is_armed(t0 + Duration::from_secs(1)));
    let t = hold(
        &mut d,
        t0 + Duration::from_secs(2),
        'x',
        SING_ARM_REPEATS - 1,
        30,
    );
    d.note_backspace(t + Duration::from_millis(30));
    let t2 = t + Duration::from_millis(60);
    d.note_char(t2, S, 'x');
    assert!(
        !d.is_armed(t2),
        "the broken run must re-earn all {SING_ARM_REPEATS} repeats"
    );
}

#[test]
fn slow_same_key_striking_never_arms() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    for i in 0..32u64 {
        let t = t0 + Duration::from_millis(i * 400); // 2.5 Hz — deliberate
        d.note_char(t, S, 'a');
        assert!(!d.is_armed(t), "slow striking must never arm (i={i})");
    }
}

#[test]
fn wind_down_crossfades_to_zero() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let t = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
    assert_eq!(d.drive(t), 1.0);
    let change = t + Duration::from_millis(30);
    d.note_char(change, S, 'b'); // key change: a provisional hand-over
    assert!(
        d.is_armed(change),
        "a key change hands the song over rather than cutting it"
    );
    let release = change + SING_REPEAT_GAP;
    let mut prev = d.drive(release);
    assert!(prev <= 1.0);
    for step in 1..=10u64 {
        let at = release + Duration::from_millis(step * 100);
        let v = d.drive(at);
        assert!(v <= prev, "the crossfade must be monotone ({prev} -> {v})");
        if step == 5 {
            assert!(
                (0.0..1.0).contains(&v) && v > 0.0,
                "mid-fade must be a real crossfade value, got {v}"
            );
        }
        prev = v;
    }
    let done = release + Duration::from_secs_f32(SING_WIND_DOWN) + Duration::from_millis(1);
    assert_eq!(d.drive(done), 0.0, "the crossfade completes at exactly 0");
    d.settle(done);
    assert_eq!(d.beat(done), None, "settled = byte-identical rest");
}

#[test]
fn letting_go_winds_down_without_an_event() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let t = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
    let still_held = t + SING_REPEAT_GAP;
    assert_eq!(d.drive(still_held), 1.0, "within the gap the hold persists");
    let mid = t + SING_REPEAT_GAP + Duration::from_secs_f32(SING_WIND_DOWN * 0.5);
    let v = d.drive(mid);
    assert!(
        v > 0.0 && v < 1.0,
        "half a wind-down after the lazy release the fade is mid-flight: {v}"
    );
    let gone = t + SING_REPEAT_GAP + Duration::from_secs_f32(SING_WIND_DOWN * 1.01);
    assert_eq!(d.drive(gone), 0.0);
}

#[test]
fn session_switch_releases_the_hold() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let t = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
    assert!(d.is_armed(t));
    let t1 = t + Duration::from_millis(30);
    d.note_char(t1, 99, 'a'); // same key, other session
    assert!(!d.is_armed(t1), "the switch released the hold");
}

#[test]
fn re_earning_the_threshold_during_wind_down_re_arms() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
    assert!(d.is_armed(armed));
    let after_gap = armed + SING_REPEAT_GAP + Duration::from_millis(50);
    d.note_char(after_gap, S, 'a');
    assert!(
        d.drive(after_gap) < 1.0 && d.drive(after_gap) > 0.0,
        "the hiccup started a real wind-down"
    );
    let mut t = after_gap;
    for _ in 1..KEY_SWITCH_REPS {
        t += Duration::from_millis(30);
        d.note_char(t, S, 'a');
    }
    assert!(
        d.is_armed(t),
        "{KEY_SWITCH_REPS} reps inside the glow must re-arm, not keep fading"
    );
    assert_eq!(d.drive(t), 1.0, "the celebration snaps back to full");
    assert_eq!(
        d.beat(t),
        Some(0.0),
        "the beat clock re-anchors at the re-arm"
    );
    assert_eq!(
        d.bar(t),
        Some(0),
        "the re-anchored grid opens at bar 0 — a fresh verse under the \
         same finger"
    );

    let mut d = KittySing::default();
    let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
    let stray = armed + Duration::from_millis(30);
    d.note_char(stray, S, 'b'); // provisional hand-over to 'b'
    assert!(
        d.is_armed(stray),
        "the hand-over keeps the song alive while 'b' is still unproven"
    );
    let mut t = stray;
    for _ in 0..SING_ARM_REPEATS {
        t += Duration::from_millis(30);
        d.note_char(t, S, 'a');
    }
    assert!(d.is_armed(t), "resuming the hold after a stray key re-arms");
    assert_eq!(d.drive(t), 1.0);

    let mut d = KittySing::default();
    let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
    let resume = armed + SING_REPEAT_GAP + Duration::from_secs_f32(SING_WIND_DOWN * 0.4);
    assert!(
        d.drive(resume) > 0.0 && d.drive(resume) < 1.0,
        "the lazy wind-down is mid-flight before the resume"
    );
    let mut t = resume;
    for _ in 0..KEY_SWITCH_REPS {
        d.note_char(t, S, 'a');
        t += Duration::from_millis(30);
    }
    let last = t - Duration::from_millis(30);
    assert!(d.is_armed(last), "resuming after a brief pause re-arms");
    assert_eq!(d.drive(last), 1.0);
}

#[test]
fn a_continuous_hold_does_not_rewind_its_beat_clock() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let armed = hold(&mut d, t0, 'a', SING_ARM_REPEATS, 30);
    let anchor = d.beat(armed);
    let mut t = armed;
    for _ in 0..16 {
        t += Duration::from_millis(30);
        d.note_char(t, S, 'a');
    }
    let expected = t.saturating_duration_since(armed).as_secs_f32() / SING_BEAT_SECONDS;
    let got = d.beat(t).expect("still armed");
    assert!(
        (got - expected).abs() < 1e-3 && anchor == Some(0.0),
        "a continuous hold must never re-anchor: got {got}, expected {expected}"
    );
}

#[test]
fn wind_down_drains_the_note_field() {
    let mut notes = MusicNotes::default();
    let t0 = Instant::now();
    for hb in 0..8u64 {
        let now = t0 + Duration::from_millis(hb * 200);
        notes.update(now, true, Some(hb as f32 * 0.5));
    }
    assert!(notes.is_active());
    let later = t0 + Duration::from_millis(8 * 200);
    notes.update(later, false, Some(8.0 * 0.5));
    let live_at_release = notes.ring.iter().flatten().count();
    for step in 0..20u64 {
        let now = later + Duration::from_millis(step * 100);
        notes.update(now, false, Some((8 + step) as f32 * 0.5));
        assert!(
            notes.ring.iter().flatten().count() <= live_at_release,
            "wind-down must never spawn"
        );
    }
    assert!(
        !notes.is_active(),
        "the field drains to byte-identical empty"
    );
}

#[test]
fn wind_down_clears_spawned_half_beat_so_reanchored_song_spawns_on_downbeat() {
    let mut notes = MusicNotes::default();
    let t0 = Instant::now();
    notes.update(t0, true, Some(0.1));
    assert_eq!(notes.ring.iter().flatten().count(), 1);
    let t1 = t0 + Duration::from_millis(50);
    notes.update(t1, false, Some(0.2));
    assert_eq!(notes.ring.iter().flatten().count(), 1);
    let t2 = t1 + Duration::from_millis(50);
    notes.update(t2, true, Some(0.0));
    assert_eq!(
        notes.ring.iter().flatten().count(),
        2,
        "re-anchored song after wind-down frame must spawn half-beat 0 note"
    );
}

#[test]
fn reduced_motion_notes_hold_still() {
    let mut notes = MusicNotes::default();
    let t0 = Instant::now();
    notes.update(t0, true, Some(0.0));
    let (a, b) = (
        t0 + Duration::from_millis(600),
        t0 + Duration::from_millis(1400),
    );
    let sample = |at: Instant, reduced: bool| {
        let mut out = Vec::new();
        notes.frames(at, reduced, &mut out);
        out
    };
    let (ra, rb) = (sample(a, true), sample(b, true));
    assert_eq!(ra.len(), 1);
    assert_eq!(ra[0].dx, rb[0].dx, "reduced motion: no wobble");
    assert_eq!(ra[0].dy, rb[0].dy, "reduced motion: no rise");
    assert!(
        rb[0].alpha < ra[0].alpha,
        "only the fade envelope animates while static"
    );
    let (fa, fb) = (sample(a, false), sample(b, false));
    assert!(
        fb[0].dy < fa[0].dy,
        "full motion: the note rises (−y is up)"
    );
}

#[test]
fn note_host_ids_avoid_kitty_sprite_family() {
    for kind in [NoteKind::Eighth, NoteKind::Beamed] {
        for (w, h) in [(9u16, 15u16), (15, 22), (1, 1)] {
            let id = note_host_id(kind, w, h);
            for generation in 0..64u64 {
                assert_ne!(id, crate::kitty_cursor::HOST_ID ^ generation);
            }
        }
    }
}

#[test]
fn song_signature_mixer_is_bijective_over_all_chars() {
    const fn minv(m: u32) -> u32 {
        let mut x = m;
        let mut i = 0;
        while i < 5 {
            x = x.wrapping_mul(2u32.wrapping_sub(m.wrapping_mul(x)));
            i += 1;
        }
        x
    }
    const INV_A: u32 = minv(0x7feb_352d);
    const INV_B: u32 = minv(0x846c_a68b);
    let (inv_a, inv_b) = (INV_A, INV_B);
    assert_eq!(inv_a.wrapping_mul(0x7feb_352d), 1);
    assert_eq!(inv_b.wrapping_mul(0x846c_a68b), 1);
    let unmix = |y: u32| -> u32 {
        let mut x = y ^ (y >> 16);
        x = x.wrapping_mul(inv_b);
        x = x ^ (x >> 15) ^ (x >> 30);
        x = x.wrapping_mul(inv_a);
        x ^ (x >> 16)
    };
    for ch in (0u32..=0x10_FFFF).filter_map(char::from_u32) {
        assert_eq!(
            unmix(song_signature(ch)),
            ch as u32,
            "mixer round trip failed at U+{:04X}",
            ch as u32
        );
    }
}

struct HostSim {
    d: KittySing,
    clock: Instant,
    latch: Option<u64>,
    pushed: Vec<(u64, u32)>,
    ever_armed: bool,
    min_drive: f32,
}

impl HostSim {
    const fn new(t0: Instant) -> Self {
        Self {
            d: KittySing::new(),
            clock: t0,
            latch: None,
            pushed: Vec::new(),
            ever_armed: false,
            min_drive: f32::INFINITY,
        }
    }

    fn run_to(&mut self, until: Instant) {
        while self.clock <= until {
            if let Some(bar) = self.d.bar(self.clock)
                && self.latch != Some(bar)
            {
                self.latch = Some(bar);
                self.pushed.push((bar, self.d.signature()));
            }
            if self.d.is_armed(self.clock) {
                self.ever_armed = true;
            }
            if self.ever_armed {
                self.min_drive = self.min_drive.min(self.d.drive(self.clock));
            }
            self.clock += Duration::from_millis(16);
        }
    }

    fn press(&mut self, at: Instant, ch: char) {
        self.run_to(at);
        self.d.note_char(at, S, ch);
    }

    fn hold(&mut self, from: Instant, ch: char, n: u32, gap_ms: u64) -> Instant {
        let mut t = from;
        for i in 0..n {
            t = from + Duration::from_millis(u64::from(i) * gap_ms);
            self.press(t, ch);
        }
        t
    }
}

#[test]
fn the_owner_pattern_w_a_r_never_drops_the_drive() {
    let t0 = Instant::now();
    let mut host = HostSim::new(t0);
    let mut t = host.hold(t0, 'w', 20, 30);
    for ch in ['a', 'r'] {
        t = host.hold(t + Duration::from_millis(30), ch, 20, 30);
    }
    host.run_to(t);
    assert!(host.ever_armed);
    assert_eq!(
        host.min_drive, 1.0,
        "three consecutive held keys are one unbroken celebration"
    );
    assert!(host.d.is_armed(t), "still on stage after w -> a -> r");
    assert_eq!(host.d.signature(), song_signature('r'));
}

#[test]
fn a_six_key_medley_holds_the_stage_end_to_end() {
    let t0 = Instant::now();
    let mut host = HostSim::new(t0);
    let medley = ['w', 'a', 'r', 't', 'z', 'q'];
    let mut t = t0;
    for (i, &ch) in medley.iter().enumerate() {
        let from = if i == 0 {
            t0
        } else {
            t + Duration::from_millis(30)
        };
        t = host.hold(from, ch, 65, 30); // 1.95 s > one 1.6 s bar each
    }
    host.run_to(t);
    assert_eq!(host.min_drive, 1.0, "the singer never leaves the stage");
    for window in host.pushed.windows(2) {
        assert!(window[1].0 > window[0].0, "bar indices strictly monotone");
    }
    let mut heard = Vec::new();
    for &(_, sig) in &host.pushed {
        if heard.last() != Some(&sig) {
            heard.push(sig);
        }
    }
    assert_eq!(
        heard,
        medley.map(song_signature).to_vec(),
        "every key in the medley was heard, in order"
    );
}

#[test]
fn genuine_typing_still_winds_the_song_down() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let armed = hold(&mut d, t0, 'w', SING_ARM_REPEATS, 30);
    assert!(d.is_armed(armed));
    let a = armed + Duration::from_millis(30);
    d.note_char(a, S, 'a'); // provisional — could still be a switch
    let r = a + Duration::from_millis(30);
    d.note_char(r, S, 'r'); // a second distinct char: typing
    assert!(
        d.drive(r) < 1.0,
        "typing proves itself and the wind-down is already running"
    );
    let gone = a + Duration::from_secs_f32(SING_WIND_DOWN * 1.05);
    assert_eq!(d.drive(gone), 0.0, "the crossfade completes");
    d.settle(gone);
    assert_eq!(d.beat(gone), None, "settled = byte-identical rest");
}

#[test]
fn a_missed_gap_recovers_with_three_reps_inside_the_glow() {
    let t0 = Instant::now();
    let mut host = HostSim::new(t0);
    let armed = host.hold(t0, 'w', 20, 30);
    let a1 = armed + Duration::from_millis(30);
    host.press(a1, 'a');
    let mut t = a1 + Duration::from_millis(400);
    for _ in 0..KEY_SWITCH_REPS {
        host.press(t, 'a'); // KEY_SWITCH_REPS genuine repeats in the glow
        t += Duration::from_millis(80);
    }
    let recovered = t - Duration::from_millis(80);
    host.run_to(recovered);
    assert!(
        host.d.is_armed(recovered),
        "three reps inside the glow re-arm — not sixteen"
    );
    assert_eq!(host.d.drive(recovered), 1.0, "back to full song");
    assert!(
        host.min_drive >= 0.33,
        "the dip never crosses the 0.33 face gate — the singer stayed \
         (worst frame: {})",
        host.min_drive
    );
    assert!(
        host.d.bar(recovered).is_some(),
        "the recovery schedules bars again"
    );
}

#[test]
fn cold_arms_still_cost_sixteen() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let t = hold(&mut d, t0, 'a', KEY_SWITCH_REPS, 30);
    assert!(!d.is_armed(t), "three reps from rest arm nothing");
    let t = hold(
        &mut d,
        t0 + Duration::from_secs(2),
        'a',
        SING_ARM_REPEATS,
        30,
    );
    assert!(d.is_armed(t), "sixteen still arm");
    d.note_break(t + Duration::from_millis(30));
    let drained = t + Duration::from_secs_f32(SING_WIND_DOWN + 0.1);
    assert_eq!(d.drive(drained), 0.0);
    d.settle(drained);
    let again = hold(
        &mut d,
        drained + Duration::from_millis(30),
        'a',
        KEY_SWITCH_REPS,
        30,
    );
    assert!(
        !d.is_armed(again),
        "after the glow is gone, three reps are just typing again"
    );
}

fn frames(
    d: &mut KittySing,
    from: Instant,
    until: Instant,
    mut each: impl FnMut(Instant, &mut KittySing),
) {
    let mut t = from;
    while t <= until {
        each(t, d);
        t += Duration::from_millis(16);
    }
}

#[test]
fn an_armed_celebration_fires_on_the_next_green_block_and_only_once() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let sig = song_signature('C');
    let arm = d
        .arm_external(t0, S, sig, 2, CelebrateOn::Green)
        .expect("the first arm is admitted");
    assert_eq!(arm.bars, 2);
    assert_eq!(d.drive(t0), 0.0, "an arm lights nothing by itself");
    assert!(!d.is_armed(t0));
    assert_eq!(d.armed_external(), Some(arm), "…it is latched");

    let other = t0 + Duration::from_millis(100);
    d.note_green_block(other, S + 1);
    assert_eq!(
        d.drive(other),
        0.0,
        "another session's green block is not this one's"
    );
    assert_eq!(d.armed_external(), Some(arm), "the arm still waits");

    let green = t0 + Duration::from_millis(500);
    d.note_green_block(green, S);
    assert_eq!(d.armed_external(), None, "the arm is spent by the edge");
    assert!(d.is_armed(green), "the green block fires the celebration");
    assert_eq!(d.drive(green), 1.0, "…at full drive");
    assert_eq!(d.signature(), sig, "…in the chosen key");
    assert_eq!(
        d.beat(green),
        Some(0.0),
        "the beat clock anchors at the fire"
    );

    let again = green + Duration::from_secs_f32(SING_BAR_SECONDS + 0.2);
    d.note_green_block(again, S);
    assert_eq!(d.bar(again), Some(1), "the grid did not move");

    let end = green + Duration::from_secs_f32(2.0 * SING_BAR_SECONDS);
    assert!(d.is_armed(end - Duration::from_millis(1)));
    assert!(!d.is_armed(end), "the run ends on its last bar line");
    let done = end + Duration::from_secs_f32(SING_WIND_DOWN) + Duration::from_millis(1);
    assert_eq!(d.drive(done), 0.0);
    d.settle(done);
    assert!(!d.external_live());
    d.note_green_block(done + Duration::from_secs(1), S);
    assert_eq!(
        d.drive(done + Duration::from_secs(1)),
        0.0,
        "spent arms never fire twice"
    );
}

#[test]
fn a_celebration_s_fans_ride_the_bars_and_the_outro_ends_on_do() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let sig = song_signature('G');
    d.arm_external(t0, S, sig, 4, CelebrateOn::Enter)
        .expect("admitted");
    d.note_keyed_enter(t0, S);
    assert!(d.is_armed(t0), "the keyed Enter fires an `on=enter` arm");

    let mut fans = Vec::new();
    let mut outros = Vec::new();
    let end = t0 + Duration::from_secs_f32(4.0 * SING_BAR_SECONDS);
    let until = end + Duration::from_secs_f32(SING_WIND_DOWN) + Duration::from_millis(50);
    frames(&mut d, t0, until, |t, d| {
        if let Some(fan) = d.take_bar_fan(t) {
            fans.push((t, fan));
        }
        if let Some(o) = d.take_outro(t) {
            outros.push((t, o));
        }
    });
    let shape: Vec<(u64, u8, bool)> = fans.iter().map(|(_, f)| (f.bar, f.n, f.ring)).collect();
    assert_eq!(
        shape,
        vec![(0, 5, false), (1, 7, false), (2, 9, false), (3, 11, true)],
        "one fan per bar, 5/7/9/11 stars, the ring on the fourth bar"
    );
    for (t, f) in &fans {
        let line = t0 + Duration::from_secs_f32(f.bar as f32 * SING_BAR_SECONDS);
        assert!(
            line.saturating_duration_since(*t) <= Duration::from_millis(1)
                && t.saturating_duration_since(line) < Duration::from_millis(17),
            "bar {} fan born on the first frame after its bar line ({:?} late)",
            f.bar,
            t.saturating_duration_since(line)
        );
    }
    assert_eq!(outros.len(), 1, "exactly one outro");
    let (at, o) = outros[0];
    assert_eq!(o.sig, sig, "the outro resolves in the run's own key");
    assert_eq!(o.drop, DROP_FAN_N);
    assert!(
        end.saturating_duration_since(at) <= Duration::from_millis(1)
            && at.saturating_duration_since(end) < Duration::from_millis(17),
        "the outro lands on the last bar line, not mid-phrase"
    );
    assert_eq!(
        bar_fan(20).n,
        BAR_FAN_CAP,
        "the fan count caps at {BAR_FAN_CAP}"
    );
    assert!(
        bar_fan(7).ring && !bar_fan(4).ring && bar_fan(u64::MAX).ring,
        "the ring rides every fourth bar without overflowing at u64::MAX"
    );
}

#[test]
fn an_on_done_arm_fires_on_the_session_s_next_done_and_only_once() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let sig = song_signature('D');
    let arm = d
        .arm_external(t0, S, sig, 2, CelebrateOn::Done)
        .expect("admitted");
    assert_eq!(arm.on.as_str(), "done");
    d.note_green_block(t0 + Duration::from_millis(10), S);
    d.note_keyed_enter(t0 + Duration::from_millis(20), S);
    assert!(
        !d.note_program_done(t0 + Duration::from_millis(30), S + 1),
        "another session's done is not this one's"
    );
    let waiting = t0 + Duration::from_millis(40);
    assert_eq!(d.armed_external(), Some(arm), "the arm still waits");
    assert_eq!(d.drive(waiting), 0.0, "nothing lit");

    let done = t0 + Duration::from_millis(500);
    assert!(d.note_program_done(done, S), "this session's done fires it");
    assert_eq!(d.armed_external(), None, "the arm is spent by the edge");
    assert!(d.is_armed(done));
    assert_eq!(d.drive(done), 1.0);
    assert_eq!(d.signature(), sig);
    assert_eq!(
        d.beat(done),
        Some(0.0),
        "the beat clock anchors at the fire"
    );

    let again = done + Duration::from_secs_f32(SING_BAR_SECONDS + 0.2);
    assert!(
        !d.note_program_done(again, S),
        "a second done fires nothing"
    );
    assert_eq!(d.bar(again), Some(1), "the grid did not move");

    let mut g = KittySing::default();
    g.arm_external(t0, S, sig, 1, CelebrateOn::Green)
        .expect("admitted");
    assert!(!g.note_program_done(t0 + Duration::from_millis(5), S));
    assert!(g.armed_external().is_some(), "the green arm still waits");
    assert_eq!(g.drive(t0 + Duration::from_millis(5)), 0.0);
}

#[test]
fn an_unarmed_session_never_celebrates_on_a_green_block() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    d.note_green_block(t0, S);
    d.note_keyed_enter(t0 + Duration::from_millis(10), S);
    assert!(!d.note_program_done(t0 + Duration::from_millis(15), S));
    let t = t0 + Duration::from_millis(20);
    assert_eq!(d.drive(t), 0.0);
    assert!(!d.is_armed(t));
    assert_eq!(d.bar(t), None);
    assert_eq!(d.beat(t), None);
    assert_eq!(d.signature(), NEUTRAL_SIGNATURE);
    assert_eq!(d.take_bar_fan(t), None);
    assert_eq!(d.take_outro(t), None);
    assert!(!d.external_live());
    let armed = hold(&mut d, t, 'a', SING_ARM_REPEATS, 30);
    assert!(d.is_armed(armed));
    assert_eq!(d.take_bar_fan(armed).map(|f| f.n), Some(BAR_FAN_BASE));
    assert_eq!(
        d.take_bar_fan(armed + Duration::from_millis(16)),
        None,
        "one fan per bar"
    );
    let gone = armed + SING_REPEAT_GAP + Duration::from_millis(100);
    assert!(d.drive(gone) < 1.0, "letting go winds down");
    assert_eq!(
        d.take_outro(gone),
        None,
        "a hold's release keeps its plain crossfade"
    );
}

#[test]
fn the_cooldown_refuses_a_second_arm_for_thirty_seconds() {
    let mut d = KittySing::default();
    let t0 = Instant::now();
    let sig = song_signature('E');
    assert_eq!(
        d.arm_external(t0, S, sig, 0, CelebrateOn::Green),
        Err(ArmRefusal::Bars(0))
    );
    assert_eq!(
        d.arm_external(t0, S, sig, CELEBRATE_MAX_BARS + 1, CelebrateOn::Green),
        Err(ArmRefusal::Bars(CELEBRATE_MAX_BARS + 1))
    );
    assert_eq!(
        d.cooldown_remaining(t0),
        Duration::ZERO,
        "a refused arm costs nothing"
    );
    d.arm_external(t0, S, sig, 1, CelebrateOn::Green)
        .expect("admitted");
    assert_eq!(
        d.arm_external(t0 + Duration::from_millis(5), S, sig, 1, CelebrateOn::Green),
        Err(ArmRefusal::AlreadyArmed)
    );
    let fire = t0 + Duration::from_secs(1);
    d.note_green_block(fire, S);
    let done = fire
        + Duration::from_secs_f32(SING_BAR_SECONDS + SING_WIND_DOWN)
        + Duration::from_millis(1);
    d.settle(done);
    assert_eq!(d.drive(done), 0.0);
    let early = t0 + Duration::from_secs(10);
    assert_eq!(
        d.arm_external(early, S, sig, 1, CelebrateOn::Green),
        Err(ArmRefusal::Cooldown {
            remaining_ms: 20_000
        }),
        "the cooldown is measured from the ARM"
    );
    let ok = t0 + CELEBRATE_COOLDOWN;
    assert!(
        d.arm_external(ok, S, sig, 1, CelebrateOn::Green).is_ok(),
        "admitted at thirty seconds"
    );
}
