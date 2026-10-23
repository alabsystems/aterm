// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Backspace and kill: the poof, the steam and their licences.

use super::*;

#[test]
fn kill_hint_paces_coarse() {
    let frame = Duration::from_millis(16);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    assert!(
        glow.next_change_deadline(t0, frame).is_none(),
        "idle: no arm"
    );
    glow.note_kill(t0, false);
    assert!(
        glow.is_active(),
        "a pending kill hint keeps the host arming"
    );
    let d = glow
        .next_change_deadline(t0, frame)
        .expect("pending hint arms a deadline");
    let poll = d.saturating_duration_since(t0);
    assert_eq!(
        poll,
        CursorGlow::KILL_HINT_POLL_INTERVAL,
        "hint-only pacing is the coarse poll, got {poll:?}"
    );
    assert!(
        poll > frame,
        "the coarse poll must be slower than frame cadence ({poll:?})"
    );
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let mut out = Vec::new();
    let t1 = t0 + Duration::from_millis(400);
    glow.tick(Some((2, 2)), t1, &c, g, &mut out);
    assert!(glow.kill_hint.is_none(), "unconsumed hint self-expires");
    assert!(
        glow.next_change_deadline(t1, frame).is_none(),
        "expired hint disarms the timer"
    );
}

#[test]
fn a_plain_backspace_arms_the_host_exactly_like_a_kill() {
    let frame = Duration::from_millis(16);
    let t0 = Instant::now();
    let mut bs = CursorGlow::default();
    assert!(!bs.is_active(), "idle: nothing armed");
    bs.note_backspace(t0);
    assert!(
        bs.is_active(),
        "a pending plain-Backspace poof licence keeps the host arming"
    );
    let d = bs
        .next_change_deadline(t0, frame)
        .expect("a plain Backspace arms a deadline");
    assert_eq!(
        d.saturating_duration_since(t0),
        CursorGlow::KILL_HINT_POLL_INTERVAL,
        "…at the same coarse poll a kill chord takes"
    );
    let mut kill = CursorGlow::default();
    kill.note_kill(t0, false);
    assert_eq!(
        bs.next_change_deadline(t0, frame),
        kill.next_change_deadline(t0, frame),
        "the two poof licences pace the host identically"
    );
    assert_eq!(bs.is_active(), kill.is_active());
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let mut out = Vec::new();
    let t1 = t0 + Duration::from_millis(400);
    bs.tick(Some((2, 2)), t1, &c, g, &mut out);
    assert!(bs.bs_poof_hint.is_none(), "unconsumed licence self-expires");
    assert!(
        bs.next_change_deadline(t1, frame).is_none(),
        "an expired licence disarms the timer"
    );
}

#[test]
fn cadence_predicate_separates_brisk_rainbow_from_coarse_ember() {
    let frame = Duration::from_millis(16);
    let t0 = Instant::now();
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.note_typed(t0);
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    let moved = t0 + frame;
    glow.note_synthetic_typed(moved, 1);
    glow.tick(Some((2, 3)), moved, &c, g, &mut out);
    assert!(glow.needs_frame_cadence(), "a visible Nyan spark is brisk");
    assert!(
        glow.next_change_deadline(moved, frame)
            .is_some_and(|due| due <= moved + frame),
        "brisk light requests frame cadence (or sooner: v2's own next change)"
    );

    glow.reset();
    glow.cursor_temp = CursorGlow::FORGE_MIN_TEMP + 0.1;
    assert!(!glow.needs_frame_cadence(), "forge ember stays coarse-only");
    assert_eq!(
        glow.next_change_deadline(moved, frame),
        Some(moved + CursorGlow::EMBER_POLL_INTERVAL),
        "coarse ember preserves its low-wakeup deadline"
    );
}

#[test]
fn a_witnessed_erase_releases_the_fallback_grace_early() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    glow.observe_row(2, 0, &row("the quick brown fox jumps over it"), t0);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    let key = t0 + Duration::from_millis(16);
    glow.note_kill(key, false);
    let echo = key + Duration::from_millis(8);
    glow.observe_row(2, 0, &row("pulled up"), echo);
    glow.tick(Some((2, 0)), echo, &c, g, &mut out);
    assert!(
        echo.saturating_duration_since(key).as_secs_f32() < CursorGlow::POOF_FALLBACK_GRACE,
        "precondition: the old timer would still be holding the poof back"
    );
    assert!(
        !glow.vapor.is_empty(),
        "the erase is on glass, so the poof lands on the frame it happens \
         — not {} ms later",
        (CursorGlow::POOF_FALLBACK_GRACE * 1000.0) as u32 - 8
    );
    assert!(glow.kill_hint.is_none(), "the fallback consumed the hint");
    let answered = glow.vapor.len();
    let later = echo + Duration::from_millis(120);
    glow.observe_row(2, 0, &row("pulled up"), later);
    glow.tick(Some((2, 0)), later, &c, g, &mut out);
    assert!(
        glow.vapor.len() <= answered,
        "the released grace must not license a SECOND poof"
    );
}

#[test]
fn an_unwitnessed_kill_still_serves_the_full_grace() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    glow.observe_row(2, 7, &row("$ hello world"), t0);
    glow.tick(Some((2, 7)), t0, &c, g, &mut out);
    let key = t0 + Duration::from_millis(16);
    glow.note_kill(key, false);
    let inside = key + Duration::from_millis(8);
    glow.observe_row(2, 7, &row("$ hello world"), inside);
    glow.tick(Some((2, 7)), inside, &c, g, &mut out);
    assert!(
        glow.vapor.is_empty() && glow.last_poof.is_none(),
        "no shrink since the key ⇒ the span branch keeps its whole window"
    );
    let past = key + Duration::from_millis(70);
    glow.observe_row(2, 7, &row("$ hello world"), past);
    glow.tick(Some((2, 7)), past, &c, g, &mut out);
    assert!(
        glow.last_poof.is_some(),
        "the timed grace remains the cap for every unwitnessed case"
    );
}

#[test]
fn reflow_kill_poofs_at_the_caret() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    glow.observe_row(2, 0, &row("rainbow ribbon typing through the wrap"), t0);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    let t1 = t0 + Duration::from_millis(16);
    glow.observe_row(2, 0, &row("the hints all the way"), t1);
    glow.tick(Some((2, 0)), t1, &c, g, &mut out);
    assert!(
        glow.vapor.is_empty(),
        "a hint-less replacement (repaint/reflow storm) never poofs"
    );
    glow.note_kill(t1 + Duration::from_millis(10), false);
    let t2 = t1 + Duration::from_millis(32);
    glow.observe_row(2, 0, &row("shorter pulled-up"), t2);
    glow.tick(Some((2, 0)), t2, &c, g, &mut out);
    let t3 = t1 + Duration::from_millis(90);
    glow.observe_row(2, 0, &row("shorter pulled-up"), t3);
    glow.tick(Some((2, 0)), t3, &c, g, &mut out);
    assert!(
        !glow.vapor.is_empty(),
        "a reflow kill still gets its poof (caret-anchored fallback)"
    );
    for v in &glow.vapor {
        assert!(
            v.x0 <= (8 * 8) as f32,
            "reflow poof puff at x={} strays from the caret",
            v.x0
        );
    }
}

#[test]
fn ctrl_k_shrink_poofs_once() {
    let g = geom(); // cols:40, cw:8
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    glow.observe_row(2, 7, &row("$ hello world"), t0);
    glow.tick(Some((2, 7)), t0, &c, g, &mut out);
    glow.note_kill(t0 + Duration::from_millis(30), false);
    let t1 = t0 + Duration::from_millis(46);
    glow.observe_row(2, 7, &row("$ hello"), t1);
    let fp = glow.tick(Some((2, 7)), t1, &c, g, &mut out);
    let puffs = glow.vapor.len();
    assert!(puffs > 0, "the kill's shrink puffs smoke");
    assert!(
        glow.vapor.iter().all(|v| v.kind == VaporKind::Poof),
        "a non-fire kill sheds the neutral POOF"
    );
    assert_ne!(fp, 0, "the poof is visible frame state");
    for v in &glow.vapor {
        assert!(
            v.x0 >= (7 * 8 - 8) as f32 && v.x0 <= (13 * 8 + 8) as f32,
            "puff at x={} outside the vanished span",
            v.x0
        );
    }
    let t2 = t0 + Duration::from_millis(62);
    glow.observe_row(2, 7, &row("$ hello"), t2);
    glow.tick(Some((2, 7)), t2, &c, g, &mut out);
    assert_eq!(glow.vapor.len(), puffs, "hint consumed — no second poof");
    let gone = glow.tick(Some((2, 7)), t0 + Duration::from_secs(3), &c, g, &mut out);
    assert_eq!(gone, 0, "the poof decays to exactly empty");
    assert!(glow.vapor.is_empty());
    assert!(!glow.is_active());
}

#[test]
fn the_shipping_default_puffs_a_cloud_on_every_delete_shape() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    assert!(c.dark_theme, "the shape under test is the DARK default");
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    let shapes: [(&str, &str, u16, &str, u16); 3] = [
        (
            "ctrl-w (word)",
            "$ echo hello world",
            18,
            "$ echo hello ",
            13,
        ),
        ("ctrl-u (line)", "$ echo hello world", 18, "$ ", 2),
        (
            "ctrl-k (to end)",
            "$ echo hello world",
            12,
            "$ echo hello",
            12,
        ),
    ];
    for (label, before, c0, after, c1) in shapes {
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut glow = CursorGlow::default();
        glow.observe_row(2, c0, &row(before), t0);
        glow.tick(Some((2, c0)), t0, &c, g, &mut out);
        glow.note_kill(t0 + Duration::from_millis(8), c1 != c0);
        let t1 = t0 + Duration::from_millis(24);
        glow.observe_row(2, c1, &row(after), t1);
        glow.tick(Some((2, c1)), t1, &c, g, &mut out);
        assert!(
            !glow.vapor.is_empty(),
            "{label}: the default style must puff a CLOUD, not sparkles alone"
        );
        assert!(
            glow.vapor.iter().all(|v| v.kind == VaporKind::Poof),
            "{label}: the cloud is the neutral erase poof"
        );
    }
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.observe_row(2, 5, &row("hello"), t0);
    glow.tick(Some((2, 5)), t0, &c, g, &mut out);
    arm_exact_backspace(&mut glow, t0 + Duration::from_millis(30), (2, 5), (2, 4));
    let t1 = t0 + Duration::from_millis(46);
    glow.observe_row(2, 4, &row("hell"), t1);
    glow.tick(Some((2, 4)), t1, &c, g, &mut out);
    assert!(
        glow.last_poof.is_some(),
        "plain backspace: the exact one-cell row proof licensed the poof"
    );
    assert!(
        !glow.vapor.is_empty() && glow.vapor.iter().all(|v| v.kind == VaporKind::Poof),
        "plain backspace: the default style must puff its neutral cloud too"
    );
}

#[test]
fn a_bigger_erase_makes_a_bigger_cloud_not_a_thinner_one() {
    let t0 = Instant::now();
    let clouds: Vec<(u16, usize, f32)> = [1u16, 4, 12, 38]
        .into_iter()
        .map(|n| {
            let mut glow = CursorGlow::default();
            let span_w = f32::from(n) * 8.0;
            glow.spawn_poof_smoke(80.0, span_w, 32.0, 16.0, 8.0, n, t0);
            let width = glow.vapor.iter().fold(f32::MIN, |a, v| a.max(v.x0))
                - glow.vapor.iter().fold(f32::MAX, |a, v| a.min(v.x0));
            (n, glow.vapor.len(), width)
        })
        .collect();
    for w in clouds.windows(2) {
        let (na, ca, _) = w[0];
        let (nb, cb, _) = w[1];
        assert!(
            cb >= ca,
            "a {nb}-column erase must not puff FEWER than a {na}-column one ({cb} < {ca})"
        );
    }
    let (_, _, widest) = *clouds.last().unwrap();
    let cap = 8.0 * ERASE_POOF_SPREAD_CELLS + 8.0;
    assert!(
        widest <= cap,
        "a 38-column kill's cloud spans {widest} px — it must CLUSTER at the \
         collapse point (cap {cap} px), not trace the whole dead line"
    );
    assert!(
        widest < 38.0 * 8.0 * 0.6,
        "precondition: the even-spread implementation would have spanned ~304 px"
    );
}

#[test]
fn a_plain_backspace_never_throws_the_kill_swoosh() {
    use crate::trail_sound::SoundKind;
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.observe_row(2, 5, &row("hello"), t0);
    glow.tick(Some((2, 5)), t0, &c, g, &mut out);
    let _ = glow.drain_sound_cues().count();
    arm_exact_backspace(&mut glow, t0 + Duration::from_millis(30), (2, 5), (2, 4));
    let t1 = t0 + Duration::from_millis(46);
    glow.observe_row(2, 4, &row("hell"), t1);
    glow.tick(Some((2, 4)), t1, &c, g, &mut out);
    assert!(
        glow.last_poof.is_some(),
        "fixture: the plain Backspace poofed"
    );
    let cues: Vec<SoundKind> = glow.drain_sound_cues().map(|c| c.kind).collect();
    assert!(
        !cues.contains(&SoundKind::Kill),
        "a plain Backspace's poof must not throw the kill swoosh over its \
         own erase poof (cued {cues:?})"
    );
    assert!(
        cues.contains(&SoundKind::Backspace),
        "…while the deletion itself still speaks (cued {cues:?})"
    );
    assert!(
        cues.contains(&SoundKind::Poof),
        "…and the cloud carries its own little Puff cue (cued {cues:?})"
    );

    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let c = cfg(GlowStyle::Lumen, true);
    glow.observe_row(2, 7, &row("$ hello world"), t0);
    glow.tick(Some((2, 7)), t0, &c, g, &mut out);
    let _ = glow.drain_sound_cues().count();
    glow.note_kill(t0 + Duration::from_millis(30), false);
    let t1 = t0 + Duration::from_millis(46);
    glow.observe_row(2, 7, &row("$ hello"), t1);
    glow.tick(Some((2, 7)), t1, &c, g, &mut out);
    let cues: Vec<SoundKind> = glow.drain_sound_cues().map(|c| c.kind).collect();
    assert!(
        cues.contains(&SoundKind::Kill),
        "a kill chord's erase puff still earns the swoosh (cued {cues:?})"
    );

    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.observe_row(2, 12, &row("$ echo hello"), t0);
    glow.tick(Some((2, 12)), t0, &c, g, &mut out);
    let _ = glow.drain_sound_cues().count();
    glow.note_word_kill(t0 + Duration::from_millis(30), true);
    let t1 = t0 + Duration::from_millis(46);
    glow.observe_row(2, 7, &row("$ echo "), t1);
    glow.tick(Some((2, 7)), t1, &c, g, &mut out);
    let cues: Vec<SoundKind> = glow.drain_sound_cues().map(|c| c.kind).collect();
    assert!(
        cues.contains(&SoundKind::KillWord),
        "a word kill's erase puff speaks the word poof (cued {cues:?})"
    );
    assert!(
        !cues.contains(&SoundKind::Kill),
        "…and never the clause swoosh (cued {cues:?})"
    );

    let _ = glow.drain_sound_cues().count();
    glow.note_kill(t1 + Duration::from_millis(400), false);
    let t2 = t1 + Duration::from_millis(416);
    glow.observe_row(2, 7, &row("$ e"), t2);
    glow.tick(Some((2, 7)), t2, &c, g, &mut out);
    let cues: Vec<SoundKind> = glow.drain_sound_cues().map(|c| c.kind).collect();
    assert!(
        cues.contains(&SoundKind::Kill) && !cues.contains(&SoundKind::KillWord),
        "a fresh ^U/^K licence swooshes again (cued {cues:?})"
    );
}

#[test]
fn no_op_backspace_poofs_nothing_but_a_real_one_does() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.observe_row(2, 0, &row(""), t0);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    glow.note_backspace(t0 + Duration::from_millis(10));
    let t1 = t0 + Duration::from_millis(90);
    glow.observe_row(2, 0, &row(""), t1);
    glow.tick(Some((2, 0)), t1, &c, g, &mut out);
    assert!(
        glow.last_poof.is_none(),
        "a no-op backspace (col 0 / empty row) erases nothing → no poof"
    );
    assert!(
        glow.particles.is_empty() && glow.vapor.is_empty(),
        "no sparkles or smoke for a backspace that vanished no cell"
    );

    let mut real = CursorGlow::default();
    real.observe_row(2, 3, &row("abc"), t0);
    real.tick(Some((2, 3)), t0, &c, g, &mut out);
    arm_exact_backspace(&mut real, t0 + Duration::from_millis(10), (2, 3), (2, 2));
    let t1 = t0 + Duration::from_millis(90);
    real.observe_row(2, 2, &row("ab"), t1);
    real.tick(Some((2, 2)), t1, &c, g, &mut out);
    assert!(
        real.last_poof.is_some(),
        "a real backspace over a char still poofs the vanished cell"
    );
}

#[test]
fn scroll_fence_retires_plain_backspace_poof_provenance() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };

    let mut glow = CursorGlow::default();
    glow.observe_row(2, 5, &row("hello"), t0);
    glow.tick(Some((2, 5)), t0, &c, g, &mut out);
    glow.note_backspace(t0 + Duration::from_millis(10));
    assert_eq!(glow.bs_baseline, Some((2, 5)));
    assert!(glow.bs_poof_hint.is_some());

    glow.note_scroll(1);
    assert!(
        glow.bs_baseline.is_none() && glow.bs_poof_hint.is_none() && glow.quench_hint.is_none(),
        "the scroll edge itself retires the old proof"
    );
    glow.drop_row_probe();
    let t1 = t0 + Duration::from_millis(90);
    glow.observe_row(2, 1, &row("x"), t1);
    glow.tick(Some((2, 1)), t1, &c, g, &mut out);
    assert!(glow.last_poof.is_none());
    assert!(
        glow.particles.is_empty() && glow.vapor.is_empty(),
        "unrelated post-scroll text cannot spend the old Backspace"
    );

    let mut switched = CursorGlow::default();
    switched.observe_row(2, 5, &row("hello"), t0);
    switched.tick(Some((2, 5)), t0, &c, g, &mut out);
    switched.note_backspace(t0 + Duration::from_millis(10));
    switched.drop_row_probe();
    assert!(switched.bs_baseline.is_none() && switched.bs_poof_hint.is_none());
}

#[test]
fn ink_repaint_same_text_is_silent() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    for i in 0..6u64 {
        let t = t0 + Duration::from_millis(16 * i);
        glow.observe_row(2, 9, &row("> typing away"), t);
        glow.tick(Some((2, 9)), t, &c, g, &mut out);
    }
    assert!(glow.vapor.is_empty(), "identical repaints shed nothing");
    let t = t0 + Duration::from_millis(120);
    glow.observe_row(2, 9, &row("> typing"), t);
    glow.tick(Some((2, 9)), t, &c, g, &mut out);
    assert!(glow.vapor.is_empty(), "no hint, no poof — ever");
}

#[test]
fn repaint_same_text_with_stale_hint_is_silent() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    glow.observe_row(2, 7, &row("$ hello world"), t0);
    glow.tick(Some((2, 7)), t0, &c, g, &mut out);
    glow.note_kill(t0 + Duration::from_millis(5), false);
    for i in 1..=6u64 {
        let t = t0 + Duration::from_millis(100 * i);
        glow.observe_row(2, 7, &row("$ hello world"), t);
        glow.tick(Some((2, 7)), t, &c, g, &mut out);
    }
    let answered = glow.vapor.len();
    assert!(answered > 0, "an echo-less kill still answers at the caret");
    assert!(
        glow.kill_hint.is_none(),
        "the fallback consumed the hint — one kill, one poof"
    );
    let t = t0 + Duration::from_millis(700);
    glow.observe_row(2, 7, &row("$ hello"), t);
    glow.tick(Some((2, 7)), t, &c, g, &mut out);
    assert!(
        glow.vapor.len() <= answered,
        "a stale kill licenses no further poof"
    );
}

#[test]
fn a_re_rowed_kill_answers_once_and_a_stale_hint_never_does() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    let mut moved = CursorGlow::default();
    moved.observe_row(2, 7, &row("$ hello world"), t0);
    moved.tick(Some((2, 7)), t0, &c, g, &mut out);
    moved.note_kill(t0 + Duration::from_millis(5), false);
    let t = t0 + Duration::from_millis(20);
    moved.observe_row(3, 2, &row("$ he"), t);
    moved.tick(Some((3, 2)), t, &c, g, &mut out);
    let t = t0 + Duration::from_millis(80);
    moved.observe_row(3, 2, &row("$ he"), t);
    moved.tick(Some((3, 2)), t, &c, g, &mut out);
    assert!(
        !moved.vapor.is_empty(),
        "a re-rowed kill still answers at the caret"
    );
    assert!(
        moved.kill_hint.is_none(),
        "the fallback consumed the hint — one kill, one poof"
    );
    let poofed_at = moved.last_poof;
    let t = t0 + Duration::from_millis(700);
    moved.observe_row(3, 2, &row("$ h"), t);
    moved.tick(Some((3, 2)), t, &c, g, &mut out);
    assert_eq!(
        moved.last_poof, poofed_at,
        "a stale kill licenses no further poof"
    );

    let mut replaced = CursorGlow::default();
    replaced.observe_row(5, 13, &row("$ hello world"), t0);
    replaced.tick(Some((5, 13)), t0, &c, g, &mut out);
    replaced.note_kill(t0 + Duration::from_millis(5), false);
    replaced.observe_row(5, 2, &row("% "), t);
    replaced.tick(Some((5, 2)), t, &c, g, &mut out);
    let t = t0 + Duration::from_millis(80);
    replaced.observe_row(5, 2, &row("% "), t);
    replaced.tick(Some((5, 2)), t, &c, g, &mut out);
    assert!(
        replaced.vapor.is_empty(),
        "replaced row + timestamp-only hint must stay silent"
    );
}

#[test]
fn rainbow_kill_poofs_fire_kill_steams() {
    let g = geom();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };

    let mut rainbow = CursorGlow::default();
    let cn = cfg(GlowStyle::RainbowKitty, true);
    rainbow.observe_row(2, 2, &row("> sparkly words"), t0);
    rainbow.tick(Some((2, 2)), t0, &cn, g, &mut out);
    rainbow.note_kill(t0 + Duration::from_millis(5), false);
    let t = t0 + Duration::from_millis(20);
    rainbow.observe_row(2, 2, &row("> "), t);
    rainbow.tick(Some((2, 2)), t, &cn, g, &mut out);
    assert!(
        rainbow.vapor.iter().all(|v| v.kind == VaporKind::Poof),
        "the rainbow kill sheds only neutral poof, never steam"
    );
    assert!(
        !rainbow.vapor.is_empty(),
        "the rainbow kill sheds its cloud on a dark theme"
    );

    let mut fire = CursorGlow::default();
    let cf = cfg(GlowStyle::Fire, true);
    fire.observe_row(2, 2, &row("> burning words"), t0);
    fire.tick(Some((2, 2)), t0, &cf, g, &mut out);
    fire.note_kill(t0 + Duration::from_millis(5), false);
    fire.observe_row(2, 2, &row("> "), t);
    fire.tick(Some((2, 2)), t, &cf, g, &mut out);
    assert!(
        fire.vapor.iter().any(|v| v.kind == VaporKind::Steam),
        "fire kill flashes quench steam"
    );
    assert!(fire.quench > 0.4, "a kill is a big douse ({})", fire.quench);
}

#[test]
fn kill_repeat_rate_limits_to_one_poof() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    glow.observe_row(2, 2, &row("$ aaaa bbbb cccc"), t0);
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    glow.note_kill(t0 + Duration::from_millis(5), false);
    let t1 = t0 + Duration::from_millis(21);
    glow.observe_row(2, 2, &row("$ aaaa bbbb"), t1);
    glow.tick(Some((2, 2)), t1, &c, g, &mut out);
    let after_first = glow.vapor.len();
    assert!(after_first > 0, "first kill poofs");
    glow.note_kill(t0 + Duration::from_millis(55), false);
    let t2 = t0 + Duration::from_millis(55);
    glow.observe_row(2, 2, &row("$ aaaa"), t2);
    glow.tick(Some((2, 2)), t2, &c, g, &mut out);
    assert_eq!(
        glow.vapor.len(),
        after_first,
        "a 50 ms kill repeat is rate-limited to one poof"
    );
}

#[test]
fn erase_poof_drive_grows_with_momentum_and_weight() {
    assert!(
        erase_poof_drive(1.0, 1) > erase_poof_drive(0.0, 1),
        "a hot delete run poofs harder than a cold one"
    );
    assert!(
        erase_poof_drive(0.5, 8) > erase_poof_drive(0.5, 1),
        "a word kill outweighs a character"
    );
    assert!(
        erase_poof_drive(0.5, 40) > erase_poof_drive(0.5, 8),
        "a line kill outweighs a word"
    );
    for cells in [1u16, 2, 5, 13, 40, 200] {
        let mut prev = -1.0;
        for i in 0..=20 {
            let v = erase_poof_drive(i as f32 / 20.0, cells);
            assert!(v >= prev - 1e-6, "drive rises with momentum");
            prev = v;
        }
    }
    let mut prev = -1.0;
    for cells in 1u16..=200 {
        let v = erase_poof_drive(0.4, cells);
        assert!(v >= prev - 1e-6, "drive rises with the erased span");
        prev = v;
    }
    assert!(
        erase_poof_drive(1.0, u16::MAX) <= ERASE_WEIGHT_CAP + 1e-6,
        "the energy law is bounded at the top"
    );
    assert!(
        (ERASE_POOF_STARS * erase_poof_drive(0.0, 1)) as usize >= 3,
        "a cold single-character erase still throws a visible few sparkles"
    );
}
