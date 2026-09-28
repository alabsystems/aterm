// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Backspace and kill: the poof, the steam and their licences.

use super::*;

/// KILL-HINT TIMER PACING: a pending kill hint with NO moving light keeps
/// the engine active (the caret fallback needs ticked frames) but paces at
/// the COARSE ~40 ms poll — never the per-frame cadence, which would pin
/// ~21 full recomposes per kill press. Moving light restores full cadence,
/// and an expired hint disarms entirely.
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
    // A tick past KILL_HINT_FRESH expires the unconsumed hint (poof_scan)
    // and the timer disarms — no probe ever arrived, nothing poofed.
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

/// A PLAIN BACKSPACE WAKES THE HOST TOO — the scheduling half of "bring
/// back the sparkle poof of deleting characters".
///
/// `poof_scan` has always ORed the two poof licences into one gate, but the
/// two SCHEDULING seams named `kill_hint` alone. So from a QUIET screen a
/// lone Backspace armed a licence the host was never told to wake for: no
/// tick, no row probe, no shrink witnessed, hint expired — silence. A held
/// delete run still puffed (its own particles keep the frame train alive),
/// which is exactly the shape of the report: single characters lost their
/// poof while runs kept theirs.
///
/// Both hints must arm both seams, at the same coarse poll.
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
    // PARITY, stated directly: the two licences schedule identically.
    let mut kill = CursorGlow::default();
    kill.note_kill(t0, false);
    assert_eq!(
        bs.next_change_deadline(t0, frame),
        kill.next_change_deadline(t0, frame),
        "the two poof licences pace the host identically"
    );
    assert_eq!(bs.is_active(), kill.is_active());
    // And it self-disarms on the same freshness window.
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

/// RESPONSIVENESS: THE GRACE IS A CAP, NOT A FLOOR. Once the probe shows
/// the erase is already on glass, the caret fallback answers on THAT frame
/// instead of sitting out the rest of [`CursorGlow::POOF_FALLBACK_GRACE`] —
/// the span branch ran on the same probe and declined, so the remaining
/// wait bought nothing but latency the user is staring at.
///
/// Shape: a reflowing TUI (the case the fallback exists for) replaces the
/// caret row with a SHORTER one, so no stable prefix/suffix survives and
/// the precise branch cannot answer at any delay.
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
    // The kill key, and the reflowed (shorter, unrelated) row one frame later.
    let key = t0 + Duration::from_millis(16);
    glow.note_kill(key, false);
    let echo = key + Duration::from_millis(8);
    glow.observe_row(2, 0, &row("pulled up"), echo);
    glow.tick(Some((2, 0)), echo, &c, g, &mut out);
    // PRE-FIX PREDICATE, stated so this cannot pass vacuously: the timed
    // grace alone is still WIDE open at this instant.
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
    // …and it consumed its licence, so this is still ONE kill, one poof.
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

/// …and the release is a WITNESS, not a deletion of the grace. A kill whose
/// row has NOT shrunk since the key (an echo-less kill, a repaint that
/// changed nothing) still serves the full grace before the caret fallback
/// answers — the span branch keeps its first shot exactly where it always
/// had one.
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
    // Inside the grace, row UNCHANGED: no witness, so nothing may fire.
    let inside = key + Duration::from_millis(8);
    glow.observe_row(2, 7, &row("$ hello world"), inside);
    glow.tick(Some((2, 7)), inside, &c, g, &mut out);
    assert!(
        glow.vapor.is_empty() && glow.last_poof.is_none(),
        "no shrink since the key ⇒ the span branch keeps its whole window"
    );
    // Past the grace, still unchanged: the timer answers, as it always did.
    let past = key + Duration::from_millis(70);
    glow.observe_row(2, 7, &row("$ hello world"), past);
    glow.tick(Some((2, 7)), past, &c, g, &mut out);
    assert!(
        glow.last_poof.is_some(),
        "the timed grace remains the cap for every unwitnessed case"
    );
}

/// ERASE POOF — REFLOW KILL (live-verified against Claude Code): killing a
/// line of a WRAPPED input box reflows the TUI — the row below moves UP,
/// so the caret row's content is REPLACED (no stable survivors), not
/// shrunk in place. The kill still poofs, MODESTLY, at the CARET; and the
/// same replacement WITHOUT a kill hint stays silent (the repaint-storm
/// gate).
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
    // Control first: the same replacement with NO hint is silent.
    glow.observe_row(2, 0, &row("rainbow ribbon typing through the wrap"), t0);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    let t1 = t0 + Duration::from_millis(16);
    glow.observe_row(2, 0, &row("the hints all the way"), t1);
    glow.tick(Some((2, 0)), t1, &c, g, &mut out);
    assert!(
        glow.vapor.is_empty(),
        "a hint-less replacement (repaint/reflow storm) never poofs"
    );
    // Now the kill: caret at col 0, Ctrl-K, and the TUI reflows — the row
    // is REPLACED by the (shorter) line that moved up from below.
    glow.note_kill(t1 + Duration::from_millis(10), false);
    let t2 = t1 + Duration::from_millis(32);
    glow.observe_row(2, 0, &row("shorter pulled-up"), t2);
    glow.tick(Some((2, 0)), t2, &c, g, &mut out);
    // The fallback waits out its grace (the span branch's first-shot
    // window), then answers on the next probed frame.
    let t3 = t1 + Duration::from_millis(90);
    glow.observe_row(2, 0, &row("shorter pulled-up"), t3);
    glow.tick(Some((2, 0)), t3, &c, g, &mut out);
    assert!(
        !glow.vapor.is_empty(),
        "a reflow kill still gets its poof (caret-anchored fallback)"
    );
    // Modest and AT the caret: every puff within a few cells of col 0.
    for v in &glow.vapor {
        assert!(
            v.x0 <= (8 * 8) as f32,
            "reflow poof puff at x={} strays from the caret",
            v.x0
        );
    }
}

/// ERASE POOF LAW: a kill key + a same-row NET SHRINK of the probed row
/// puffs smoke off the exact vanished span, exactly ONCE (the hint is
/// consumed by the poof) — and the vapor decays to exactly empty
/// (idle-zero law).
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
    // Frame 1: the pre-kill truth ("$ hello world", caret mid-line).
    glow.observe_row(2, 7, &row("$ hello world"), t0);
    glow.tick(Some((2, 7)), t0, &c, g, &mut out);
    // Ctrl-K…
    glow.note_kill(t0 + Duration::from_millis(30), false);
    // …and the echo frame rewrites the row SHORTER (cols 7..13 vanished).
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
    // The puffs sit ON the vanished span [7..13) — never over survivors.
    for v in &glow.vapor {
        assert!(
            v.x0 >= (7 * 8 - 8) as f32 && v.x0 <= (13 * 8 + 8) as f32,
            "puff at x={} outside the vanished span",
            v.x0
        );
    }
    // The identical shrunken row on the next frame is SILENT: the hint was
    // consumed by the poof (one kill = one poof).
    let t2 = t0 + Duration::from_millis(62);
    glow.observe_row(2, 7, &row("$ hello"), t2);
    glow.tick(Some((2, 7)), t2, &c, g, &mut out);
    assert_eq!(glow.vapor.len(), puffs, "hint consumed — no second poof");
    // Idle-zero: the vapor decays to exactly empty.
    let gone = glow.tick(Some((2, 7)), t0 + Duration::from_secs(3), &c, g, &mut out);
    assert_eq!(gone, 0, "the poof decays to exactly empty");
    assert!(glow.vapor.is_empty());
    assert!(!glow.is_active());
}

/// A causally confirmed plain Backspace poofs. The timestamp schedules and
/// classifies the gesture, while the exact one-cell row proof identifies the
/// vanished cell. CAT-INDEPENDENT by construction: `poof_scan` runs unconditionally
/// and never consults the cursor cat (there is no cat in a `CursorGlow` at
/// all), so the burst fires whether or not the cat is flying. On rainbow kitty/dark
/// the poof is the momentum-independent GLITTER burst — round sparks with
/// one hero '+' since the sparkle rebalance.
#[test]
fn exact_plain_backspace_poofs_cat_independent() {
    let g = geom(); // cols 40, cw 8
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    // "hello", caret at col 5; then a PLAIN Backspace shrinks it to "hell".
    glow.observe_row(2, 5, &row("hello"), t0);
    glow.tick(Some((2, 5)), t0, &c, g, &mut out);
    arm_exact_backspace(&mut glow, t0 + Duration::from_millis(30), (2, 5), (2, 4));
    let t1 = t0 + Duration::from_millis(46);
    glow.observe_row(2, 4, &row("hell"), t1);
    glow.tick(Some((2, 4)), t1, &c, g, &mut out);
    assert!(
        glow.last_poof.is_some(),
        "an exact plain Backspace proof licensed the erase poof"
    );
    assert!(
        !glow.vapor.is_empty() && glow.vapor.iter().all(|v| v.kind == VaporKind::Poof),
        "the plain delete puffs its neutral cloud at the vanished cell (v2's erase \
         stardust is the sky's own law)"
    );
}

/// THE CLOUD, ON THE SHIPPED DEFAULT — the owner's whole report, as a gate:
/// *"there used to be a cloud poof on delete … can you bring something like
/// that back?"*
///
/// WHY THE GREEN SUITE MISSED IT. Every test that asserted a delete puffs
/// SMOKE (`ctrl_k_shrink_poofs_once`, `reflow_kill_poofs_at_the_caret`) built
/// `cfg(GlowStyle::Lumen, true)`, and the default-style test
/// (`exact_plain_backspace_poofs_cat_independent`) asserted only on
/// `particles`. So "the machinery is green" and "the owner sees a cloud" were
/// different claims and nothing tied them together: on RAINBOW KITTY over a
/// DARK theme — the shipping default, `prefs::DEFAULT_CURSOR_TRAIL_STYLE` —
/// the smoke was gated behind `!cfg.dark_theme` and `vapor` measured EMPTY
/// for every delete shape on a live window.
///
/// This asserts the pairing directly, for the four shapes a shell user
/// actually deletes with, in the configuration they actually run.
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
    // (label, pre-kill row, caret, post-kill row, caret) — Ctrl-W kills a
    // word, Ctrl-U the line back to the prompt, Ctrl-K to the end.
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
    // …and the PLAIN BACKSPACE, whose licence is the other one.
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
        !glow.vapor.is_empty(),
        "plain backspace: the default style must puff a cloud too"
    );
}

/// THE CLOUD IS A CLOUD BECAUSE ITS PUFFS OVERLAP — the weight law, which
/// [`CursorGlow::spawn_poof_smoke`] used to invert.
///
/// It computed `puffs` from the erased width and then spread them EVENLY
/// over that same width, so the two cancelled: a 44-column Ctrl-U dealt its
/// 8 puffs one every 5 columns — a dotted rule, measurably fainter on glass
/// than the 2 tightly overlapping puffs a one-character Backspace got. A
/// BIGGER erase produced a FAINTER cloud.
///
/// The law now: more erased ⇒ NOT FEWER puffs, and the cloud's own width is
/// capped at [`ERASE_POOF_SPREAD_CELLS`] however wide the span, so the
/// billow lands where the text COLLAPSED rather than tracing where it was.
/// Both halves are asserted, and the second is the one that refutes the
/// even-spread implementation.
#[test]
fn a_bigger_erase_makes_a_bigger_cloud_not_a_thinner_one() {
    let t0 = Instant::now();
    // Drive `spawn_poof_smoke` directly across a ladder of erase widths;
    // the cell metrics are `geom()`'s (cw 8, ch 16).
    // the span starts at x=80 (col 10) and grows rightward.
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
    // The spread cap, plus the ±half-cell jitter each puff draws.
    let cap = 8.0 * ERASE_POOF_SPREAD_CELLS + 8.0;
    assert!(
        widest <= cap,
        "a 38-column kill's cloud spans {widest} px — it must CLUSTER at the \
         collapse point (cap {cap} px), not trace the whole dead line"
    );
    // NON-VACUITY: the cap actually bites — the span it was given is far
    // wider than the cloud it produced.
    assert!(
        widest < 38.0 * 8.0 * 0.6,
        "precondition: the even-spread implementation would have spanned ~304 px"
    );
}

/// THE POOF CUE LAW, both halves against the same poof machinery:
///
/// * a PLAIN BACKSPACE never throws the TIER-3 `Kill` SWOOSH over the
///   erase POOF it already spoke on its cursor retreat (both `spawn_poof`
///   call sites passed a literal `cue_kill = true` until 2026-08-28, so
///   this law was documented and not enforced — 7 dB of falling noise
///   burying the one sound the owner asked to hear). Its cloud instead
///   cues [`SoundKind::Poof`], the puff's own little air.
/// * a kill CHORD still swooshes — that is the cue's whole purpose.
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
    // PLAIN BACKSPACE: the poof fires, cues its Puff, and no swoosh.
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

    // KILL CHORD: same poof machinery, and the swoosh IS its voice.
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

    // WORD KILL (^W): the same machinery armed by `note_word_kill` speaks
    // the WORD POOF — one word going, not a clause — and never the swoosh.
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

    // …and the NEXT line-scale chord is back to the swoosh: the word mark
    // is a property of the licence, not a latch.
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

/// A NO-OP Backspace poofs NOTHING. A plain Backspace at column 0 /
/// on an empty row erases no cell, so the caret-anchored fallback must NOT
/// fire — an unconditional `bs_poof_hint` fallback lets autorepeat at the
/// left margin puff over text it never touched. The
/// bs-only fallback demands the SAME real-shrink proof (`cur.fill <
/// prev.fill`) the span branch does. A Backspace that genuinely erases a
/// glyph still poofs (the span branch), so feedback is unaffected.
#[test]
fn no_op_backspace_poofs_nothing_but_a_real_one_does() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let row = |s: &str| -> Vec<char> {
        let mut v: Vec<char> = s.chars().collect();
        v.resize(40, ' ');
        v
    };
    // NO-OP: caret at column 0 on an EMPTY row; Backspace erases nothing.
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.observe_row(2, 0, &row(""), t0);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    glow.note_backspace(t0 + Duration::from_millis(10));
    // Tick PAST the fallback grace (0.06 s) with the row UNCHANGED (no
    // shrink): an unguarded caret fallback would puff here.
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

    // REAL: a glyph genuinely vanishes (the row shrinks) → the poof fires.
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

/// A scroll fence invalidates the row identity captured at a plain
/// Backspace. Reusing the same numeric row for shorter unrelated content
/// must not satisfy that old `(row, fill)` witness and mint a phantom poof.
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

    // Tab/pane content replacement uses `drop_row_probe` without a scroll;
    // it owns the identical provenance fence.
    let mut switched = CursorGlow::default();
    switched.observe_row(2, 5, &row("hello"), t0);
    switched.tick(Some((2, 5)), t0, &c, g, &mut out);
    switched.note_backspace(t0 + Duration::from_millis(10));
    switched.drop_row_probe();
    assert!(switched.bs_baseline.is_none() && switched.bs_poof_hint.is_none());
}

/// FALSE-POSITIVE FAMILY 1: repaints. An Ink repaint that rewrites the
/// SAME text produces an identical probe (no net shrink) — silent with or
/// without a kill hint — and even a real shrink with NO hint is silent
/// (row diffs alone never poof; that's the engine-hook trap).
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
    // A shrink WITHOUT a kill hint (app truncated its own output): silent.
    let t = t0 + Duration::from_millis(120);
    glow.observe_row(2, 9, &row("> typing"), t);
    glow.tick(Some((2, 9)), t, &c, g, &mut out);
    assert!(glow.vapor.is_empty(), "no hint, no poof — ever");
}

/// FALSE-POSITIVE FAMILY 1b: a kill whose echo never shows still earns
/// exactly ONE (caret-anchored, post-grace) poof — and once the hint is
/// consumed, a LATE shrink (0.6 s on) is some other edit: it attributes
/// NOTHING to the long-gone kill.
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
    // Same-text frames tick past the hint's freshness window…
    for i in 1..=6u64 {
        let t = t0 + Duration::from_millis(100 * i);
        glow.observe_row(2, 7, &row("$ hello world"), t);
        glow.tick(Some((2, 7)), t, &c, g, &mut out);
    }
    // The echo-less kill answered ONCE at the caret (post-grace fallback)…
    let answered = glow.vapor.len();
    assert!(answered > 0, "an echo-less kill still answers at the caret");
    assert!(
        glow.kill_hint.is_none(),
        "the fallback consumed the hint — one kill, one poof"
    );
    // …so a shrink arriving now (0.6 s later) is NOT the kill's echo and
    // adds nothing (the old vapor may have decayed; none may be BORN).
    let t = t0 + Duration::from_millis(700);
    glow.observe_row(2, 7, &row("$ hello"), t);
    glow.tick(Some((2, 7)), t, &c, g, &mut out);
    assert!(
        glow.vapor.len() <= answered,
        "a stale kill licenses no further poof"
    );
}

/// KILL FEEDBACK CONTRACT: an observed stable-row shrink earns a precise
/// poof, and a stationary echo-less kill may use the bounded caret
/// fallback — INCLUDING when the survivor row is replaced and re-rowed at
/// once, which is the exact shape the fallback was written for (Claude
/// Code's bottom-anchored Ink box reflows on a kill). The universal move
/// gate used to silence that case by revoking the kill hint on the
/// unproven relocation; the license does not, because a kill KEY was
/// pressed and "did a human touch the keyboard just now" is the whole
/// question (`docs/design/EFFECTS-LICENSE-REDESIGN.md`). What still bounds
/// it: one kill, one poof — and a stale hint answers nothing.
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
    // Row identity broken (Claude Code's bottom-anchored reflow, or a
    // scroll racing the hint): no row diff can name the vanished span, so
    // the bounded caret fallback answers — once.
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
    // …and the consumed hint is the bound: later frames of the same
    // re-rowed survivor buy no second puff, and a shrink arriving after
    // the freshness window is not this kill's echo at all.
    let poofed_at = moved.last_poof;
    let t = t0 + Duration::from_millis(700);
    moved.observe_row(3, 2, &row("$ h"), t);
    moved.tick(Some((3, 2)), t, &c, g, &mut out);
    assert_eq!(
        moved.last_poof, poofed_at,
        "a stale kill licenses no further poof"
    );

    // Same row + fresh hint, content REPLACED (reflow-or-scroll): the
    // unproven cursor relocation is the same fail-closed boundary.
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

/// STYLE DISPATCH: a rainbow kitty kill sheds star-power SPARKLES over its
/// grey CLOUD ([`VaporKind::Poof`] — never fire's [`VaporKind::Steam`]); a
/// Fire kill flashes QUENCH STEAM instead and visibly escalates the quench
/// meter (the standing blaze dies down at the same moment the steam rises).
///
/// The rainbow half asserted `vapor.is_empty()` until 2026-08-28, which
/// pinned the ABSENCE the owner reported as a loss ("there used to be a
/// cloud poof on delete") rather than the style-dispatch discrimination this
/// test exists for. It now asserts the sharper claim — the rainbow kill's
/// vapor is ALL Poof and no Steam — which still refutes any confusion of the
/// two thermal languages and additionally refutes an empty cloud.
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
    // RETUNED 2026-08-29: the rainbow kill now sheds a few NEUTRAL grey
    // puffs on dark themes too (owner: "I'm still not seeing the cloud
    // poof for backspace") — the same `VaporKind::Poof` light themes
    // always shed, drawn through the renderer's existing dark-ground arm.
    // What this pins is the KIND: removal reads as grey smoke, never as
    // the fire style's warm steam below.
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

/// RATE LIMIT: a held kill-key repeat billows at a readable cadence — two
/// kills 50 ms apart yield exactly ONE poof (POOF_MIN_GAP).
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
    // The second kill's shrink lands 34 ms later — inside POOF_MIN_GAP.
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

/// THE ERASE POOF's ENERGY LAW (owner: bring the delete poof back "with a
/// similar momentum as forward typing and also include some weight for the
/// size of what is deleted"). Both terms must bite, INDEPENDENTLY: a faster
/// run poofs harder at the same span, and a bigger span poofs harder at the
/// same speed — and a cold single character still clears the floor the
/// shipping poof always threw.
#[test]
fn erase_poof_drive_grows_with_momentum_and_weight() {
    // MOMENTUM bites at a fixed span.
    assert!(
        erase_poof_drive(1.0, 1) > erase_poof_drive(0.0, 1),
        "a hot delete run poofs harder than a cold one"
    );
    // WEIGHT bites at a fixed momentum, and keeps biting up the range.
    assert!(
        erase_poof_drive(0.5, 8) > erase_poof_drive(0.5, 1),
        "a word kill outweighs a character"
    );
    assert!(
        erase_poof_drive(0.5, 40) > erase_poof_drive(0.5, 8),
        "a line kill outweighs a word"
    );
    // Monotone non-decreasing in BOTH arguments across the range.
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
    // BOUNDED: the weight term saturates, so a 4000-column kill cannot ask
    // for an unbounded cloud.
    assert!(
        erase_poof_drive(1.0, u16::MAX) <= ERASE_WEIGHT_CAP + 1e-6,
        "the energy law is bounded at the top"
    );
    // And the FLOOR: a cold single character still throws a few grains.
    // The fewer-sparkles ruling (owner via 0405571c, 2026-08-08) cut the
    // poof back to ERASE_POOF_STARS = 7 — the erase's WEIGHT lives in the
    // hero grain's size and the debris speed now, not the count — so the
    // cold floor is a visible three, not the old six-grain cloud.
    assert!(
        (ERASE_POOF_STARS * erase_poof_drive(0.0, 1)) as usize >= 3,
        "a cold single-character erase still throws a visible few sparkles"
    );
}
