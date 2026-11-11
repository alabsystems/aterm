// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const G: u64 = 0;

fn cell(map: &[((u16, u16), char)]) -> impl Fn(u16, u16) -> Option<char> + '_ {
    move |r, c| {
        map.iter()
            .find(|((rr, cc), _)| *rr == r && *cc == c)
            .map(|(_, ch)| *ch)
    }
}

fn t0() -> Instant {
    Instant::now()
}

fn at_the_end(
    p: &mut Predictor,
    ch: char,
    cursor: (u16, u16),
    grid: (u16, u16),
    now: Instant,
) -> bool {
    let row = vec![Seen::Blank; usize::from(grid.0)];
    let key = KeySample {
        cursor,
        coords: G,
        grid,
        row: Some(&row),
        ..KeySample::default()
    };
    p.predict_glyph(ch, &key, now)
}

#[test]
fn pending_bounds_reserves_wrapped_guesses_without_consuming_them() {
    let mut predictor = Predictor::new(PredictMode::Always);
    let now = t0();
    assert_eq!(predictor.pending_bounds(), None);
    for ch in ['a', 'b', 'c'] {
        assert!(predictor.predict_char_in_grid(ch, (2, 9), G, (10, 10), now));
    }
    let deadline = predictor.next_deadline();
    assert_eq!(predictor.pending_bounds(), Some((2, 0, 4, 10)));
    assert_eq!(predictor.pending_bounds(), Some((2, 0, 4, 10)));
    assert_eq!(predictor.preds.len(), 3);
    assert_eq!(predictor.next_deadline(), deadline);
    predictor.reset();
    assert_eq!(predictor.pending_bounds(), None);
}

#[test]
fn expire_due_consumes_the_deadline_without_a_redraw() {
    let mut p = Predictor::new(PredictMode::Always);
    let born = t0();
    assert!(!p.expire_due(born), "nothing pending, nothing due");
    assert!(p.predict_char('a', (0, 0), G, 80, born));
    let deadline = p.next_deadline().expect("a pending guess arms a deadline");

    let early = born + Duration::from_millis(1);
    assert!(!p.expire_due(early), "a guess inside its window is not due");
    assert_eq!(p.next_deadline(), Some(deadline), "and its deadline stands");

    let late = deadline + Duration::from_millis(1);
    assert!(
        p.next_deadline().is_some_and(|d| d <= late),
        "fixture: the peek's view — a past deadline"
    );
    assert!(p.expire_due(late), "the expired guess is dropped");
    assert!(
        p.next_deadline().is_none_or(|d| d > late),
        "no past deadline survives the sweep: {:?}",
        p.next_deadline()
    );
    assert!(!p.expire_due(late), "a second sweep finds nothing");
}

fn echo_turn(p: &mut Predictor, ch: char, col: u16, at: Instant, rtt: Duration) -> Instant {
    assert!(p.predict_char(ch, (0, col), G, 80, at));
    let done = at + rtt;
    let mut screen = vec![((0, col), ch)];
    screen.extend(p.proof_echo.map(|e| ((e.row, e.col), e.ch)));
    p.reconcile(Some((0, col + 1)), G, false, done, cell(&screen));
    done
}

#[test]
fn off_mode_predicts_nothing() {
    let mut p = Predictor::new(PredictMode::Off);
    assert!(!p.predict_char('a', (0, 0), G, 80, t0()));
    assert!(p.idle());
}

#[test]
fn sustained_slow_echo_retires_predictions_and_displays_next() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    assert!(p.predict_char('a', (0, 0), G, 80, now));
    assert!(!p.idle());
    let first = now + Duration::from_millis(60);
    p.reconcile(Some((0, 1)), G, false, first, cell(&[((0, 0), 'a')]));
    assert!(p.idle(), "confirmed prediction is retired");
    assert!(p.predict_char('b', (0, 1), G, 80, first));
    assert!(p.overlay(first).is_empty(), "one slow sample stays hidden");

    let second = first + Duration::from_millis(60);
    p.reconcile(Some((0, 2)), G, false, second, cell(&[((0, 1), 'b')]));
    assert!(p.predict_char('c', (0, 2), G, 80, second));
    let shown = p.overlay(second);
    assert_eq!(shown.len(), 1);
    assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (0, 2, 'c'));
}

#[test]
fn types_ahead_extends_from_last_guess_before_echo() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    p.predict_char('a', (0, 0), G, 80, now);
    p.predict_char('b', (0, 0), G, 80, now);
    let shown = p.overlay(now);
    assert_eq!(shown.len(), 2);
    assert_eq!((shown[0].col, shown[0].ch), (0, 'a'));
    assert_eq!((shown[1].col, shown[1].ch), (1, 'b'));
}

#[test]
fn adaptive_isolated_scheduler_tail_does_not_open_gate() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();

    assert!(p.predict_char('a', (0, 0), G, 80, now));
    let fast = now + Duration::from_millis(1);
    p.reconcile(Some((0, 1)), G, false, fast, cell(&[((0, 0), 'a')]));
    assert!(p.predict_char('b', (0, 1), G, 80, fast));
    assert!(p.predict_char('c', (0, 1), G, 80, fast));
    let tail = fast + Duration::from_millis(60);
    p.reconcile(
        Some((0, 3)),
        G,
        false,
        tail,
        cell(&[((0, 1), 'b'), ((0, 2), 'c')]),
    );

    assert!(p.predict_char('d', (0, 3), G, 80, tail));
    assert!(
        p.overlay(tail).is_empty(),
        "one delayed scheduler turn must not manufacture speculative pixels, even when it retires a burst"
    );

    let local_again = tail + Duration::from_millis(1);
    p.reconcile(Some((0, 4)), G, false, local_again, cell(&[((0, 3), 'd')]));
    assert!(p.predict_char('e', (0, 4), G, 80, local_again));
    assert!(p.overlay(local_again).is_empty());
}

#[test]
fn adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    let slow = Duration::from_millis(60);
    let fast = Duration::from_millis(1);

    let mut at = echo_turn(&mut p, 'a', 0, now, slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    assert_eq!(p.overlay(at).len(), 1, "sustained remote latency opens");
    p.reconcile(Some((0, 3)), G, false, at + fast, cell(&[((0, 2), 'c')]));
    at += fast;

    at = echo_turn(&mut p, 'd', 3, at, fast);
    at = echo_turn(&mut p, 'e', 4, at, fast);
    assert!(p.srtt.is_some_and(|s| s >= DISPLAY_CLOSE_MS));
    assert!(
        p.adaptive_slow,
        "a raw streak alone must not close the gate"
    );

    for col in 5..14 {
        at = echo_turn(&mut p, 'x', col, at, fast);
    }
    assert!(!p.adaptive_slow, "a settled local link stops painting");
    assert!(p.predict_char('z', (0, 14), G, 80, at));
    assert!(p.overlay(at).is_empty());
}

#[test]
fn adaptive_gate_does_not_flap_across_a_single_fast_sample() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    let slow = Duration::from_millis(30);

    let mut at = echo_turn(&mut p, 'a', 0, now, slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.adaptive_slow, "control: the slow link opened the gate");

    at = echo_turn(&mut p, 'c', 2, at, Duration::from_millis(5));
    assert!(p.adaptive_slow, "one fast dip must not blank the overlay");
    at = echo_turn(&mut p, 'd', 3, at, slow);
    assert!(p.predict_char('e', (0, 4), G, 80, at));
    assert_eq!(
        p.overlay(at).len(),
        1,
        "the slow link keeps painting across its own jitter"
    );
}

#[test]
fn adaptive_gate_never_closes_while_a_guess_is_in_flight() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    let slow = Duration::from_millis(60);
    let fast = Duration::from_millis(1);

    let mut at = echo_turn(&mut p, 'a', 0, now, slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.adaptive_slow);

    let mut col = 2u16;
    assert!(p.predict_char('x', (0, col), G, 80, at));
    for _ in 0..14 {
        assert!(
            p.predict_char('x', (0, col + 1), G, 80, at),
            "one glyph ahead"
        );
        at += fast;
        p.reconcile(Some((0, col + 1)), G, false, at, cell(&[((0, col), 'x')]));
        col += 1;
    }
    assert_eq!(p.preds.len(), 1, "fixture: one guess still in flight");
    assert!(
        p.srtt.is_some_and(|s| s < DISPLAY_CLOSE_MS),
        "control: the smoothed estimate is now unambiguously local"
    );
    assert!(
        p.adaptive_slow,
        "a gate flip with pixels in flight is exactly the visible blink"
    );

    at += fast;
    p.reconcile(Some((0, col + 1)), G, false, at, cell(&[((0, col), 'x')]));
    assert!(p.idle());
    assert!(!p.adaptive_slow);
}

#[test]
fn burst_is_classified_by_the_oldest_retired_guess() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    let burst = |p: &mut Predictor, start: Instant, base: u16| -> Instant {
        for i in 0..5u16 {
            assert!(p.predict_char(
                'a',
                (0, base + i),
                G,
                80,
                start + Duration::from_millis(u64::from(i) * 10)
            ));
        }
        let echoed: Vec<((u16, u16), char)> = (0..5u16).map(|i| ((0, base + i), 'a')).collect();
        let done = start + Duration::from_millis(45);
        p.reconcile(Some((0, base + 5)), G, false, done, cell(&echoed));
        done
    };

    let after = burst(&mut p, now, 0);
    let after = burst(&mut p, after, 5);
    assert!(
        p.adaptive_slow,
        "two 45 ms turns are a slow link no matter how fast the user types"
    );
    assert!(p.predict_char('z', (0, 10), G, 80, after));
    assert_eq!(p.overlay(after).len(), 1);
}

#[test]
fn adaptive_local_tail_expires_without_a_visible_erase() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    p.predict_char('a', (0, 0), G, 80, now);
    let fast = now + Duration::from_millis(1);
    p.reconcile(Some((0, 1)), G, false, fast, cell(&[((0, 0), 'a')]));
    assert!(p.predict_char('b', (0, 1), G, 80, fast));
    assert!(!p.is_displaying(fast));
    assert!(
        p.overlay(fast).is_empty(),
        "no speculative pixel was painted"
    );

    let after_deadline = fast + Duration::from_millis(GLITCH_FLOOR_MS + 1);
    assert!(p.overlay(after_deadline).is_empty());
    assert!(p.idle(), "the hidden stale guess still self-heals");
    assert!(
        p.next_deadline().is_none(),
        "invisible expiry disarms its timer"
    );
}

#[test]
fn divergence_flushes_all() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    p.predict_char('a', (0, 0), G, 80, now);
    p.reconcile(Some((0, 1)), G, false, now, cell(&[((0, 0), '*')]));
    assert!(p.idle(), "a wrong guess flushes the whole prediction set");
}

#[test]
fn alt_screen_flushes_predictions() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    p.predict_char('a', (0, 0), G, 80, now);
    assert!(!p.idle());
    p.reconcile(Some((10, 4)), G, true, now, cell(&[]));
    assert!(p.idle());
}

#[test]
fn backspace_cancels_pending_prediction() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    p.predict_char('a', (0, 0), G, 80, now);
    p.predict_char('b', (0, 0), G, 80, now);
    assert_eq!(p.overlay(now).len(), 2);
    assert!(p.predict_backspace(now));
    assert_eq!(p.overlay(now).len(), 1, "backspace removes the last guess");
    assert_eq!(p.overlay(now)[0].ch, 'a');
}

#[test]
fn height_blind_hosts_decline_at_the_margin_without_flushing() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char('a', (0, 78), G, 80, now));
    assert!(p.predict_char('b', (0, 78), G, 80, now));
    assert!(
        !p.predict_char('c', (0, 78), G, 80, now),
        "col 80 would wrap and no row height was supplied → declined"
    );
    assert_eq!(
        p.overlay(now).len(),
        2,
        "declining the wrap must not erase the two glyphs already on glass"
    );
}

#[test]
fn wraps_to_the_next_row_instead_of_flushing() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char_in_grid('a', (0, 79), G, (80, 24), now));
    assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now));
    let shown = p.overlay(now);
    assert_eq!(shown.len(), 2);
    assert_eq!((shown[0].row, shown[0].col), (0, 79));
    assert_eq!(
        (shown[1].row, shown[1].col, shown[1].ch),
        (1, 0, 'b'),
        "the wrapped glyph continues on the next row"
    );
}

#[test]
fn wrapped_guess_survives_the_deferred_wrap_cursor() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(at_the_end(&mut p, 'a', (0, 79), (80, 24), now));
    assert!(at_the_end(&mut p, 'b', (0, 79), (80, 24), now));
    p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 79), 'a')]));
    let shown = p.overlay(now);
    assert_eq!(shown.len(), 1, "'a' retires, the wrapped 'b' survives");
    assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (1, 0, 'b'));
}

#[test]
fn bottom_row_declines_without_flushing() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char_in_grid('a', (23, 79), G, (80, 24), now));
    assert!(
        !p.predict_char_in_grid('b', (23, 79), G, (80, 24), now),
        "there is no row 24 to wrap into"
    );
    assert_eq!(p.overlay(now).len(), 1, "the armed glyph keeps its pixels");
}

#[test]
fn wide_glyphs_are_declined_but_narrow_accents_are_predicted() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(
        p.predict_char('é', (0, 0), G, 80, now),
        "single-width accented Latin is ordinary self-echoing input"
    );
    assert!(p.predict_char('ü', (0, 0), G, 80, now));
    assert_eq!(p.overlay(now).len(), 2);
    assert!(
        !p.predict_char('日', (0, 0), G, 80, now),
        "wide/CJK glyphs are left to real echo"
    );
    assert!(
        !p.predict_char('\u{0301}', (0, 0), G, 80, now),
        "a combining mark modifies the cell to its LEFT — not a new cell"
    );
}

#[test]
fn ambiguous_glyphs_are_measured_as_the_terminal_writes_them() {
    let mut p = Predictor::new(PredictMode::Always);
    p.set_ambiguous_width_double(true);
    let now = t0();
    assert!(!p.predict_char('é', (0, 0), G, 80, now));
    assert!(!p.predict_char('°', (0, 0), G, 80, now));
    assert!(
        p.predict_char('a', (0, 0), G, 80, now),
        "ASCII stays narrow"
    );
}

#[test]
fn a_declined_glyph_does_not_erase_the_word_to_its_left() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char('a', (0, 0), G, 80, now));
    assert!(!p.predict_char('日', (0, 0), G, 80, now));
    assert_eq!(p.overlay(now).len(), 1, "'a' keeps its pixels");
    assert!(
        !p.predict_char('b', (0, 0), G, 80, now),
        "we cannot place a glyph past a wide echo we did not model"
    );
    assert_eq!(
        p.overlay(now).len(),
        1,
        "…and declining that changes nothing"
    );
    p.reconcile(Some((0, 3)), G, false, now, cell(&[((0, 0), 'a')]));
    assert!(p.predict_char('c', (0, 3), G, 80, now));
    assert_eq!(p.overlay(now)[0].col, 3);
}

#[test]
fn mode_parse_is_fail_safe() {
    assert_eq!(PredictMode::parse("adaptive"), PredictMode::Adaptive);
    assert_eq!(PredictMode::parse("ALWAYS"), PredictMode::Always);
    assert_eq!(PredictMode::parse("off"), PredictMode::Off);
    assert_eq!(PredictMode::parse("nonsense"), PredictMode::Off);
}

#[test]
fn fresh_line_resets_confirmation_no_leak() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    p.predict_char('a', (0, 0), G, 80, now);
    let t1 = now + Duration::from_millis(50);
    p.reconcile(Some((0, 1)), G, false, t1, cell(&[((0, 0), 'a')]));
    p.predict_char('b', (0, 1), G, 80, t1);
    let t2 = t1 + Duration::from_millis(50);
    p.reconcile(Some((0, 2)), G, false, t2, cell(&[((0, 1), 'b')]));
    assert!(
        p.adaptive_slow,
        "control: sustained slow echo opened the latch"
    );
    p.predict_char('s', (1, 9), G, 80, t2);
    assert!(
        p.overlay(t2).is_empty(),
        "an unechoed char on a new line must NOT display (no password leak)"
    );
}

#[test]
fn submit_does_not_blank_the_tail_of_the_command_line() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char('l', (0, 0), G, 80, now));
    assert!(p.predict_char('s', (0, 0), G, 80, now));
    assert_eq!(p.overlay(now).len(), 2);
    p.note_line_submit(now);
    assert_eq!(
        p.overlay(now).len(),
        2,
        "the unechoed tail of the submitted line stays on glass"
    );
    assert!(
        p.next_deadline().is_some(),
        "…and still self-heals on its own deadline"
    );
}

#[test]
fn a_stale_guess_confirming_after_submit_cannot_arm_the_new_line() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    let r = 23u16;
    let slow = Duration::from_millis(60);
    assert!(p.predict_char('a', (r, 0), G, 80, now));
    let t1 = now + slow;
    p.reconcile(Some((r, 1)), G, false, t1, cell(&[((r, 0), 'a')]));
    assert!(p.predict_char('b', (r, 1), G, 80, t1));
    let t2 = t1 + slow;
    p.reconcile(Some((r, 2)), G, false, t2, cell(&[((r, 1), 'b')]));
    assert!(p.adaptive_slow, "control: the link is classified slow");

    assert!(p.predict_char('c', (r, 2), G, 80, t2));
    p.note_line_submit(t2);
    let t3 = t2 + slow;
    p.reconcile(Some((r, 3)), G, false, t3, cell(&[((r, 2), 'c')]));
    assert!(p.idle(), "the stale guess retired normally");
    assert!(
        !p.confirmed_epoch,
        "a pre-submit confirmation must not arm the new line"
    );

    assert!(p.predict_char('s', (r, 9), G, 80, t3));
    assert!(
        p.overlay(t3).is_empty(),
        "an unechoed secret on the new line must never display"
    );
}

#[test]
fn slow_link_expiry_window_grows_until_the_echo_can_confirm() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    let rtt = Duration::from_millis(400);

    assert!(p.predict_char('a', (0, 0), G, 80, now));
    let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
    assert!(p.idle(), "the unconfirmed guess still self-heals");
    assert!(
        p.glitch_window() > Duration::from_millis(GLITCH_FLOOR_MS),
        "a timeout widens the window instead of repeating forever"
    );

    let armed = now + Duration::from_millis(500);
    assert!(p.predict_char('b', (0, 0), G, 80, armed));
    let _ = p.overlay(armed + Duration::from_millis(300));
    assert!(!p.idle(), "the guess outlives the old fixed window");
    p.reconcile(Some((0, 1)), G, false, armed + rtt, cell(&[((0, 0), 'b')]));
    assert!(p.confirmed_epoch, "the echo finally reaches a live guess");
    assert!(
        !p.adaptive_slow,
        "ONE confirmation is not a regime: silence must never substitute for the \
         second slow sample (that is what painted a ghost at a password prompt)"
    );
    assert_eq!(
        p.expiry_backoff, 0,
        "a real echo retires the cold-link probe"
    );

    let mut at = armed + rtt;
    assert!(p.predict_char('c', (0, 1), G, 80, at));
    at += rtt;
    p.reconcile(Some((0, 2)), G, false, at, cell(&[((0, 1), 'c')]));
    assert!(
        p.adaptive_slow,
        "two real slow echoes classify the link — the honest evidence path"
    );

    assert!(p.predict_char('d', (0, 2), G, 80, at));
    assert_eq!(p.overlay(at).len(), 1, "a 400 ms link finally predicts");
}

#[test]
fn silence_never_opens_the_gate_or_diverges_the_estimate() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let mut now = t0();
    for _ in 0..12 {
        assert!(p.predict_char('x', (0, 0), G, 80, now));
        now += Duration::from_millis(GLITCH_CEILING_MS + 10);
        assert!(
            p.overlay(now).is_empty(),
            "an unechoed guess is never displayed on a link that never echoed"
        );
    }
    assert!(!p.adaptive_slow, "silence is not evidence of a slow link");
    assert!(!p.confirmed_epoch, "nothing was ever confirmed");
    assert_eq!(p.srtt, None, "timeouts must not seed the RTT estimate");
    assert!(
        p.glitch_window() <= Duration::from_millis(GLITCH_CEILING_MS),
        "the probe is bounded, never divergent"
    );
}

#[test]
fn expiry_retires_only_the_oldest_guess_owes_it_and_voids_the_epoch() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char('a', (0, 0), G, 80, now));
    let later = now + Duration::from_millis(200);
    assert!(p.predict_char('b', (0, 0), G, 80, later));
    let expired = now + Duration::from_millis(GLITCH_FLOOR_MS + 10);
    let shown = p.overlay(expired);
    assert_eq!(shown.len(), 1, "only the expired head is retired");
    assert_eq!(shown[0].ch, 'b');
    assert_eq!(p.owed_at, Some(now), "the expired key's echo is owed");
    assert_eq!(
        p.preds.front().map(|g| g.owed),
        Some(Some(now)),
        "…to the guess typed after it"
    );

    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    assert_eq!(
        p.overlay(at).len(),
        1,
        "control: the proven line paints 'c'"
    );
    let expired = at + Duration::from_millis(GLITCH_FLOOR_MS + 10);
    assert!(p.overlay(expired).is_empty(), "the stale guess expired");
    assert!(
        !p.confirmed_epoch && p.proof_cell.is_none(),
        "an expiry voids the epoch's proof"
    );
    assert!(p.predict_char('d', (0, 2), G, 80, expired));
    assert!(
        p.overlay(expired).is_empty(),
        "'d' echoes after 'c''s late echo, at (0, 3): not painted at (0, 2)"
    );
    let late = expired + Duration::from_millis(60);
    p.reconcile(
        Some((0, 3)),
        G,
        false,
        late,
        cell(&after_ab(&[((0, 2), 'c')])),
    );
    assert!(p.idle(), "'c' at 'd''s cell: a different glyph, flushed");
    assert!(!p.confirmed_epoch);
}

#[test]
fn without_expiry_owes_an_expiry_keeps_the_proof_as_it_did() {
    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    p.disable_rule_for_test(Rule::ExpiryOwes);
    let owed = p.owed_at;
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    let expired = at + Duration::from_millis(GLITCH_FLOOR_MS + 10);
    assert!(p.overlay(expired).is_empty());
    assert_eq!(p.owed_at, owed, "nothing owed");
    assert!(p.confirmed_epoch, "the proof kept");
    assert!(p.predict_char('d', (0, 2), G, 80, expired));
    assert_eq!(p.overlay(expired).len(), 1, "'d' painted on 'c''s cell");
}

#[test]
fn session_switch_forgets_slow_link_rtt() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    assert!(p.predict_char('a', (0, 0), G, 80, now));
    let slow1 = now + Duration::from_millis(60);
    p.reconcile(Some((0, 1)), G, false, slow1, cell(&[((0, 0), 'a')]));
    assert!(p.predict_char('b', (0, 1), G, 80, slow1));
    let slow2 = slow1 + Duration::from_millis(60);
    p.reconcile(Some((0, 2)), G, false, slow2, cell(&[((0, 1), 'b')]));
    assert!(p.predict_char('c', (0, 2), G, 80, slow2));
    assert_eq!(p.overlay(slow2).len(), 1, "control: slow link displays");

    p.reset_session();
    let local = slow2 + Duration::from_millis(1);
    assert!(p.predict_char('x', (0, 0), G, 80, slow2));
    p.reconcile(Some((0, 1)), G, false, local, cell(&[((0, 0), 'x')]));
    assert!(p.predict_char('y', (0, 1), G, 80, local));
    assert!(
        p.overlay(local).is_empty(),
        "a new session learns its own fast RTT instead of inheriting the old slow link"
    );
}

#[test]
fn the_deferred_wrap_slot_is_not_a_free_cell() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(at_the_end(&mut p, 'a', (0, 79), (80, 24), now));
    p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 79), 'a')]));
    assert!(p.idle(), "control: the echo retired the guess");

    assert!(at_the_end(&mut p, 'b', (0, 79), (80, 24), now));
    let shown = p.overlay(now);
    assert_eq!(
        (shown[0].row, shown[0].col, shown[0].ch),
        (1, 0, 'b'),
        "a parked cursor wraps the next glyph to the following row"
    );

    let mut resized = Predictor::new(PredictMode::Always);
    assert!(at_the_end(&mut resized, 'a', (0, 79), (80, 24), now));
    resized.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 79), 'a')]));
    assert!(resized.deferred_wrap);
    assert!(resized.predict_char_in_grid('b', (0, 99), G, (100, 24), now));
    let shown = resized.overlay(now);
    assert_eq!(
        (shown[0].row, shown[0].col, shown[0].ch),
        (0, 99, 'b'),
        "changing cols clears a stale deferred_wrap latched at the previous width"
    );
}

#[test]
fn a_wide_glyph_filling_the_last_two_columns_parks_the_cursor() {
    for (glyph, wide_policy, parked) in [
        ('日', false, true),
        ('日', true, true),
        ('é', true, true),
        ('é', false, false),
    ] {
        let mut p = Predictor::new(PredictMode::Always);
        p.set_ambiguous_width_double(wide_policy);
        let now = t0();
        let accepted = p.predict_char_in_grid(glyph, (0, 78), G, (80, 24), now);
        assert_eq!(
            accepted, !parked,
            "{glyph} wide={wide_policy}: placed iff narrow"
        );
        p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 78), glyph)]));
        assert!(
            p.idle(),
            "{glyph} wide={wide_policy}: the echo drained the set"
        );

        assert!(p.predict_char_in_grid('i', (0, 79), G, (80, 24), now));
        let shown = p.overlay(now);
        let want = if parked { (1, 0) } else { (0, 79) };
        assert_eq!(
            (shown[0].row, shown[0].col),
            want,
            "{glyph} wide={wide_policy}: parked={parked}"
        );
    }
}

#[test]
fn changing_the_ambiguous_width_flushes_guesses_placed_under_the_old_one() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char('a', (0, 0), G, 80, now));
    p.set_ambiguous_width_double(false);
    assert_eq!(p.overlay(now).len(), 1, "the same width keeps the guess");
    p.set_ambiguous_width_double(true);
    assert!(p.idle(), "a new width flushes what the old one placed");
    assert!(
        !p.predict_char('é', (0, 1), G, 80, now),
        "and measures with the new one"
    );
}

#[test]
fn a_free_last_column_is_still_predicted_in_place() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char_in_grid('a', (0, 78), G, (80, 24), now));
    p.reconcile(Some((0, 79)), G, false, now, cell(&[((0, 78), 'a')]));
    assert!(p.idle());
    assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now));
    let shown = p.overlay(now);
    assert_eq!((shown[0].row, shown[0].col), (0, 79));
}

#[test]
fn a_deferred_wrap_on_the_last_row_declines_without_painting() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(at_the_end(&mut p, 'a', (23, 79), (80, 24), now));
    p.reconcile(Some((23, 79)), G, false, now, cell(&[((23, 79), 'a')]));
    assert!(
        !at_the_end(&mut p, 'b', (23, 79), (80, 24), now),
        "no row 24 to wrap into, and the slot holds the echoed 'a'"
    );
    assert!(p.idle(), "declining paints nothing and erases nothing");
}

#[test]
fn a_wrap_the_terminal_did_not_defer_keeps_the_wrapped_guess() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char_in_grid('a', (0, 79), G, (80, 24), now));
    assert!(p.predict_char_in_grid('b', (0, 79), G, (80, 24), now));
    p.reconcile(Some((1, 0)), G, false, now, cell(&[((0, 79), 'a')]));
    let shown = p.overlay(now);
    assert_eq!(
        shown.len(),
        1,
        "'a' retires; the wrapped 'b' is no divergence"
    );
    assert_eq!((shown[0].row, shown[0].col, shown[0].ch), (1, 0, 'b'));
}

#[test]
fn a_scrolled_cursor_does_not_pass_for_the_deferred_wrap_slot() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(at_the_end(&mut p, 'a', (5, 79), (80, 24), now));
    assert!(at_the_end(&mut p, 'b', (5, 79), (80, 24), now));
    p.reconcile(Some((5, 79)), G, false, now, cell(&[((5, 79), 'a')]));
    assert_eq!(p.overlay(now).len(), 1, "control: the park keeps 'b' alive");

    p.reconcile(Some((4, 79)), G, false, now, cell(&[((4, 79), 'a')]));
    assert!(p.idle(), "a stale absolute row is retired, not tolerated");
}

#[test]
fn a_scroll_retires_guesses_instead_of_confirming_them_against_moved_cells() {
    let now = t0();
    let mut unaware = Predictor::new(PredictMode::Adaptive);
    assert!(unaware.predict_char_in_grid('a', (5, 0), G, (80, 24), now));
    unaware.reconcile(Some((5, 1)), G, false, now, cell(&[((5, 0), 'a')]));
    assert!(
        unaware.confirmed_epoch,
        "control: a glyph-equal cell really does confirm"
    );

    let mut p = Predictor::new(PredictMode::Adaptive);
    assert!(p.predict_char_in_grid('a', (5, 0), G, (80, 24), now));
    p.reconcile(Some((5, 1)), G + 1, false, now, cell(&[((5, 0), 'a')]));
    assert!(p.idle(), "a scrolled guess is retired, not re-aimed");
    assert!(
        !p.confirmed_epoch,
        "…so a moved cell can never arm the display gate"
    );
}

#[test]
fn a_scroll_ends_the_epoch_on_the_same_physical_row() {
    let slow = Duration::from_millis(60);
    let r = 23u16;
    let leak = |coords_after: u64| {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = t0();
        for (col, ch) in "yes".chars().enumerate() {
            let col = col as u16;
            assert!(p.predict_char_in_grid(ch, (r, col), G, (80, 24), at));
            at += slow;
            p.reconcile(Some((r, col + 1)), G, false, at, cell(&[((r, col), ch)]));
        }
        assert!(
            p.adaptive_slow && p.confirmed_epoch,
            "control: a proven slow line"
        );
        let prompt = [((r, 0), 'P'), ((r, 1), 'a'), ((r, 2), 's')];
        p.reconcile(Some((r, 3)), coords_after, false, at, cell(&prompt));
        assert!(p.predict_char_in_grid('s', (r, 3), coords_after, (80, 24), at));
        !p.overlay(at).is_empty()
    };
    assert!(
        leak(G),
        "control: without a scroll the row's proof still applies"
    );
    assert!(
        !leak(G + 1),
        "an unechoed glyph on a prompt that scrolled onto the proven row was painted"
    );
}

#[test]
fn a_line_terminator_makes_what_follows_tentative() {
    let slow = Duration::from_millis(60);
    let mut p = Predictor::new(PredictMode::Adaptive);
    let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    assert_eq!(p.overlay(at).len(), 1, "control: the proven line paints");

    p.note_line_submit(at);
    p.note_line_submit(at);
    assert!(!p.predict_char('h', (0, 2), G, 80, at));
    let shown: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
    assert_eq!(
        shown,
        ['c'],
        "the proven line's 'c' keeps its pixels; nothing typed after the \
         boundary paints"
    );

    at += slow;
    p.reconcile(Some((0, 3)), G, false, at, cell(&[((0, 2), 'c')]));
    assert!(p.idle());
    at += p.glitch_window() + Duration::from_millis(1);
    at = echo_turn(&mut p, 'x', 3, at, slow);
    assert!(p.predict_char('y', (0, 4), G, 80, at));
    assert_eq!(p.overlay(at).len(), 1, "a re-proven line paints again");
}

#[test]
fn nothing_is_armed_behind_a_terminator_while_its_line_is_in_flight() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char('l', (0, 2), G, 80, now));
    assert!(p.predict_char('s', (0, 2), G, 80, now));
    p.note_line_submit(now);
    assert!(!p.predict_char('c', (0, 2), G, 80, now));
    assert!(!p.predict_char('d', (0, 2), G, 80, now));
    let shown: Vec<(u16, u16, char)> = p
        .overlay(now)
        .iter()
        .map(|g| (g.row, g.col, g.ch))
        .collect();
    assert_eq!(
        shown,
        [(0, 2, 'l'), (0, 3, 's')],
        "the submitted line's tail stays on glass, and nothing is painted over it"
    );
    let echoed = now + Duration::from_millis(60);
    p.reconcile(
        Some((1, 2)),
        G,
        false,
        echoed,
        cell(&[((0, 2), 'l'), ((0, 3), 's')]),
    );
    assert!(p.idle(), "the line retired");
    assert!(p.predict_char('x', (1, 2), G, 80, echoed));
}

#[test]
fn the_submitted_lines_echo_cannot_prove_the_next_line() {
    let slow = Duration::from_millis(60);
    let mut p = Predictor::new(PredictMode::Adaptive);
    let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.predict_char('l', (0, 2), G, 80, at));
    assert!(p.predict_char('s', (0, 2), G, 80, at));
    assert_eq!(p.overlay(at).len(), 2, "control: the proven line paints");
    p.note_line_submit(at);
    assert!(!p.predict_char('l', (0, 2), G, 80, at));
    assert!(!p.predict_char('s', (0, 2), G, 80, at));
    at += slow;
    p.reconcile(
        Some((0, 4)),
        G,
        false,
        at,
        cell(&[((0, 0), 'a'), ((0, 1), 'b'), ((0, 2), 'l'), ((0, 3), 's')]),
    );
    assert!(p.idle());
    assert!(
        !p.confirmed_epoch,
        "the old line's echo proved nothing about the new one"
    );
    assert!(p.predict_char('x', (0, 4), G, 80, at));
    assert!(
        p.overlay(at).is_empty(),
        "'x' echoes on the next prompt row, not after \"ls\" on this one"
    );
}

#[test]
fn an_echo_owed_to_a_flushed_guess_cannot_prove_a_later_one() {
    let slow = Duration::from_millis(60);
    let mut p = Predictor::new(PredictMode::Adaptive);
    let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(
        p.adaptive_slow && p.confirmed_epoch,
        "control: a slow, proven line"
    );
    let screen = [((0, 0), 'a'), ((0, 1), 'b')];
    assert!(p.predict_char('n', (0, 2), G, 80, at));
    at += Duration::from_millis(10);
    p.reconcile(Some((1, 2)), G, false, at, cell(&screen));
    assert!(
        p.idle(),
        "the cursor left the guess: flushed, its echo still owed"
    );
    assert!(p.predict_char('n', (1, 2), G, 80, at));
    at += slow;
    let first_echo = [((0, 0), 'a'), ((0, 1), 'b'), ((1, 2), 'n')];
    p.reconcile(Some((1, 3)), G, false, at, cell(&first_echo));
    assert!(p.idle(), "glyph and cursor match, so the pixels retire");
    assert!(
        !p.confirmed_epoch,
        "…but the echo may be the first 'n's: it proves nothing"
    );
    assert!(p.predict_char(' ', (1, 3), G, 80, at));
    assert!(
        p.overlay(at).is_empty(),
        "the space echoes at (1, 4), after the second 'n': not painted at (1, 3)"
    );
    at += slow;
    let second_echo = [((0, 0), 'a'), ((0, 1), 'b'), ((1, 2), 'n'), ((1, 3), 'n')];
    p.reconcile(Some((1, 4)), G, false, at, cell(&second_echo));
    assert!(p.idle());
    at += p.glitch_window() + Duration::from_millis(1);
    assert!(p.predict_char('q', (1, 5), G, 80, at));
    at += slow;
    let q_echo = [((1, 2), 'n'), ((1, 3), 'n'), ((1, 5), 'q')];
    p.reconcile(Some((1, 6)), G, false, at, cell(&q_echo));
    assert!(p.confirmed_epoch, "an attributable echo proves the epoch");
    assert!(p.predict_char('r', (1, 6), G, 80, at));
    assert_eq!(p.overlay(at).len(), 1);
}

fn proven_slow_line(mode: PredictMode) -> (Predictor, Instant) {
    let slow = Duration::from_millis(60);
    let mut p = Predictor::new(mode);
    let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(
        p.adaptive_slow && p.confirmed_epoch && p.proof_cell == Some((0, 2)),
        "fixture: a slow, proven line"
    );
    assert_eq!(
        p.glitch_window(),
        Duration::from_millis(GLITCH_FLOOR_MS),
        "fixture: the floor window"
    );
    (p, at)
}

const AB: [((u16, u16), char); 2] = [((0, 0), 'a'), ((0, 1), 'b')];

fn after_ab(more: &[((u16, u16), char)]) -> Vec<((u16, u16), char)> {
    AB.iter().chain(more).copied().collect()
}

#[test]
fn every_untracked_press_owes_its_echo() {
    let submit: fn(&mut Predictor, Instant) = |p, at| p.note_line_submit(at);
    let unmodelled: fn(&mut Predictor, Instant) = |p, at| p.note_unmodelled_input(at);
    for (label, press) in [("terminator", submit), ("unmodelled input", unmodelled)] {
        let (mut p, mut at) = proven_slow_line(PredictMode::Adaptive);
        press(&mut p, at);
        assert!(p.idle());
        at += Duration::from_millis(10);
        assert!(p.predict_char('n', (0, 2), G, 80, at), "{label}");
        at += Duration::from_millis(50);
        let landed = after_ab(&[((0, 2), 'n')]);
        p.reconcile(Some((0, 3)), G, false, at, cell(&landed));
        assert!(
            p.idle(),
            "{label}: glyph and cursor match, so the pixels retire"
        );
        assert!(
            !p.confirmed_epoch,
            "{label}: the 'n' may be the press's output: it proves nothing"
        );
        assert!(p.predict_char('x', (0, 3), G, 80, at));
        assert!(
            p.overlay(at).is_empty(),
            "{label}: 'x' echoes after the typed 'n', at (0, 4): not painted at (0, 3)"
        );
    }
}

#[test]
fn a_declined_glyph_owes_its_echo() {
    let (mut p, mut at) = proven_slow_line(PredictMode::Adaptive);
    assert!(p.predict_char('a', (0, 2), G, 80, at));
    assert!(!p.predict_char('日', (0, 2), G, 80, at), "wide: declined");
    assert!(
        !p.predict_char('n', (0, 2), G, 80, at),
        "behind a declined glyph: declined"
    );
    at += Duration::from_millis(60);
    let wide = after_ab(&[((0, 2), 'a'), ((0, 3), '日')]);
    p.reconcile(Some((0, 5)), G, false, at, cell(&wide));
    assert!(p.idle(), "'a' retired; the first 'n' is still on its way");
    at += Duration::from_millis(10);
    assert!(p.predict_char('n', (0, 5), G, 80, at));
    at += Duration::from_millis(10);
    let landed: Vec<_> = wide.iter().copied().chain([((0, 5), 'n')]).collect();
    p.reconcile(Some((0, 6)), G, false, at, cell(&landed));
    assert!(p.idle());
    assert!(!p.confirmed_epoch, "the first 'n's echo proves nothing");
    assert!(p.predict_char('c', (0, 6), G, 80, at));
    assert!(
        p.overlay(at).is_empty(),
        "'c' echoes at (0, 7), after the second 'n'"
    );
}

#[test]
fn an_erase_we_did_not_cancel_owes_its_echo() {
    let (mut p, mut at) = proven_slow_line(PredictMode::Adaptive);
    assert!(!p.predict_backspace(at), "nothing of ours: it erases 'b'");
    at += Duration::from_millis(10);
    assert!(p.predict_char('n', (0, 2), G, 80, at));
    assert!(p.predict_char('n', (0, 2), G, 80, at));
    at += Duration::from_millis(60);
    let landed = [((0, 0), 'a'), ((0, 1), 'n'), ((0, 2), 'n')];
    p.reconcile(Some((0, 3)), G, false, at, cell(&landed));
    assert_eq!(p.preds.len(), 1, "the first guess's pixels retire");
    assert!(!p.confirmed_epoch, "the match was the second key's echo");
    assert!(
        p.overlay(at).is_empty(),
        "no ghost at (0, 3) of an 'n' that echoed at (0, 2)"
    );
}

#[test]
fn a_guess_retired_without_credit_owes_its_own_echo() {
    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    assert!(p.predict_char('n', (0, 2), G, 80, at));
    assert_eq!(p.overlay(at).len(), 1, "control: the proven line paints K");
    let k = at;
    p.reconcile(
        Some((0, 6)),
        G,
        false,
        k + Duration::from_millis(10),
        cell(&AB),
    );
    assert!(p.idle());
    let typed = k + Duration::from_millis(190);
    for ch in ['n', 'n', 'c'] {
        assert!(p.predict_char(ch, (0, 2), G, 80, typed));
    }
    let first = after_ab(&[((0, 2), 'n')]);
    p.reconcile(
        Some((0, 3)),
        G,
        false,
        k + Duration::from_millis(230),
        cell(&first),
    );
    assert_eq!(p.preds.len(), 2);
    assert!(!p.confirmed_epoch);
    let at = k + Duration::from_millis(260);
    let second = after_ab(&[((0, 2), 'n'), ((0, 3), 'n')]);
    p.reconcile(Some((0, 4)), G, false, at, cell(&second));
    assert_eq!(p.preds.len(), 1, "the second guess's pixels retire");
    assert!(
        !p.confirmed_epoch,
        "…but the echo may be the first new guess's: it proves nothing"
    );
    assert!(
        p.overlay(at).is_empty(),
        "'c' echoes at (0, 5), after the second 'n': not painted at (0, 4)"
    );
    let third = after_ab(&[((0, 2), 'n'), ((0, 3), 'n'), ((0, 4), 'n')]);
    let at = at + Duration::from_millis(30);
    p.reconcile(Some((0, 5)), G, false, at, cell(&third));
    assert!(p.idle());
}

#[test]
fn an_owed_echo_neither_proves_nor_measures_a_carried_guess() {
    let rtt = Duration::from_millis(100);
    let mut p = Predictor::new(PredictMode::Adaptive);
    let mut at = echo_turn(&mut p, 'a', 0, t0(), rtt);
    at = echo_turn(&mut p, 'b', 1, at, rtt);
    assert!(p.predict_char('n', (0, 2), G, 80, at));
    at += Duration::from_millis(10);
    p.reconcile(Some((1, 0)), G, false, at, cell(&AB));
    assert!(p.idle(), "the cursor jumped off 'n': flushed, owed");
    assert!(p.predict_char('n', (1, 0), G, 80, at));
    p.note_line_submit(at);
    let before = (p.srtt, p.slow_samples, p.fast_samples);
    at += Duration::from_millis(5);
    let landed = after_ab(&[((1, 0), 'n')]);
    p.reconcile(Some((1, 1)), G, false, at, cell(&landed));
    assert!(p.idle(), "the carried guess's pixels retire");
    assert_eq!(
        (p.srtt, p.slow_samples, p.fast_samples),
        before,
        "the first 'n's echo is no sample of the link"
    );

    at += Duration::from_millis(GLITCH_CEILING_MS);
    assert!(p.predict_char('x', (1, 1), G, 80, at));
    p.note_line_submit(at);
    let echoed = at + rtt;
    let tail = [((1, 0), 'n'), ((1, 1), 'x')];
    p.reconcile(Some((1, 2)), G, false, echoed, cell(&tail));
    assert!(p.idle());
    assert_eq!(p.slow_samples, before.1, "a slow sample, counted (capped)");
    assert!(
        p.srtt.is_some_and(|s| (s - 100.0).abs() < 0.01),
        "the tail's 100 ms echo measured: {:?}",
        p.srtt
    );
}

#[test]
fn an_owe_is_dated_by_its_press() {
    let rtt = Duration::from_millis(100);
    let mut p = Predictor::new(PredictMode::Adaptive);
    let mut at = echo_turn(&mut p, 'a', 0, t0(), rtt);
    at = echo_turn(&mut p, 'b', 1, at, rtt);
    p.note_line_submit(at);
    at += Duration::from_secs(60);
    assert!(p.predict_char('x', (1, 2), G, 80, at));
    at += rtt;
    p.reconcile(Some((1, 3)), G, false, at, cell(&[((1, 2), 'x')]));
    assert!(
        p.confirmed_epoch,
        "x's own echo, a minute after the Enter's"
    );
    assert!(p.predict_char('y', (1, 3), G, 80, at));
    assert_eq!(
        p.overlay(at).len(),
        1,
        "the new line paints its second glyph"
    );
}

#[test]
fn a_backspace_cancels_the_glyph_we_declined() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(p.predict_char('l', (0, 2), G, 80, now));
    assert!(p.predict_char('s', (0, 2), G, 80, now));
    p.note_line_submit(now);
    assert!(!p.predict_char('c', (0, 2), G, 80, now));
    assert!(!p.predict_backspace(now), "no guess of ours was cancelled");
    let shown = |p: &mut Predictor| -> Vec<(u16, u16, char)> {
        p.overlay(now)
            .iter()
            .map(|g| (g.row, g.col, g.ch))
            .collect()
    };
    assert_eq!(
        shown(&mut p),
        [(0, 2, 'l'), (0, 3, 's')],
        "the submitted line's tail stays on glass"
    );
    assert!(!p.predict_backspace(now));
    assert!(
        shown(&mut p).is_empty(),
        "an erase with nothing declined to cancel may reach the carried tail"
    );

    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    assert!(p.predict_char('x', (0, 2), G, 80, at));
    assert!(!p.predict_char('日', (0, 2), G, 80, at));
    assert!(!p.predict_backspace(at));
    assert_eq!(
        p.overlay(at).len(),
        1,
        "'x' (carried by its proven epoch) survives the erase of the wide glyph"
    );
    assert!(!p.predict_char('日', (0, 2), G, 80, at));
    p.note_line_submit(at);
    assert!(!p.predict_backspace(at));
    assert!(p.idle(), "fail closed: the erase may reach the carried 'x'");
}

#[test]
fn a_key_we_did_not_model_retires_the_guesses_in_flight() {
    let slow = Duration::from_millis(60);
    for mode in [PredictMode::Adaptive, PredictMode::Always] {
        let mut p = Predictor::new(mode);
        let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "{mode:?}: control: 'c' paints");

        p.note_unmodelled_input(at);
        assert!(
            p.overlay(at).is_empty(),
            "{mode:?}: 'c' stays on glass past a key that may have erased it"
        );
        if mode == PredictMode::Adaptive {
            assert!(p.predict_char('h', (0, 2), G, 80, at));
            assert!(
                p.overlay(at).is_empty(),
                "the glyph typed after the boundary stays dark"
            );
            at += slow;
            p.reconcile(Some((0, 3)), G, false, at, cell(&[((0, 2), 'c')]));
            assert!(p.idle());
            at += p.glitch_window() + Duration::from_millis(1);
            at = echo_turn(&mut p, 'x', 3, at, slow);
            assert!(p.predict_char('y', (0, 4), G, 80, at));
            assert_eq!(p.overlay(at).len(), 1, "a re-proven line paints again");
        }
    }
}

#[test]
fn an_erase_after_a_boundary_darkens_the_carried_guesses() {
    let slow = Duration::from_millis(60);
    let mut p = Predictor::new(PredictMode::Adaptive);
    let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    p.note_line_submit(at);
    assert_eq!(p.overlay(at).len(), 1, "control: 'c' is carried");
    assert!(
        !p.predict_backspace(at),
        "'c' is not this epoch's to cancel"
    );
    assert!(p.overlay(at).is_empty(), "the erase may be landing on it");
}

#[test]
fn an_erase_landing_on_typed_ahead_text_leaves_no_ghost() {
    type Erase = fn(&mut Predictor, Instant);
    type Cells = &'static [((u16, u16), char)];
    let killed: Cells = &[((0, 0), 'l'), ((0, 1), 's')];
    let one_erased: Cells = &[((0, 0), 'l'), ((0, 1), 's'), ((0, 3), 'f'), ((0, 4), 'o')];
    let erases: [(&str, Erase, Cells, u16); 3] = [
        (
            "ctrl+w kills the word",
            |p, at| p.note_unmodelled_input(at),
            killed,
            3,
        ),
        (
            "ctrl+h deletes its last glyph",
            |p, at| p.note_unmodelled_input(at),
            one_erased,
            5,
        ),
        (
            "a BS handed over as a glyph",
            |p, at| assert!(!p.predict_char('\u{8}', (0, 3), G, 80, at)),
            one_erased,
            5,
        ),
    ];
    let slow = Duration::from_millis(60);
    for mode in [PredictMode::Adaptive, PredictMode::Always] {
        for (label, erase, left, cursor) in erases {
            let mut p = Predictor::new(mode);
            let mut at = echo_turn(&mut p, 'l', 0, t0(), slow);
            at = echo_turn(&mut p, 's', 1, at, slow);
            at = echo_turn(&mut p, ' ', 2, at, slow);
            for ch in ['f', 'o', 'o'] {
                assert!(p.predict_char(ch, (0, 3), G, 80, at));
            }
            assert_eq!(
                p.overlay(at).len(),
                3,
                "{mode:?} {label}: control: the typed-ahead word paints"
            );
            erase(&mut p, at);
            at += slow;
            p.reconcile(Some((0, cursor)), G, false, at, cell(left));
            let ghost: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
            assert!(
                ghost.is_empty(),
                "{mode:?} {label}: the erased text is still on glass as {ghost:?}"
            );
        }
    }
}

#[test]
fn only_text_ending_in_a_terminator_is_a_line_submit() {
    for yes in [
        &b"\r"[..],
        b"\n",
        b"\r\n",
        b"ls -l\r",
        "caf\u{e9}\n".as_bytes(),
    ] {
        assert!(is_line_submit(yes), "{yes:?}");
    }
    for no in [
        &b""[..],
        b"ls",
        b"\r ",
        b"\x1b\r",
        b"\x15clear\r",
        b"\x08\r",
        b"\x7f\n",
        b"\t\r",
        b"\xff\r",
    ] {
        assert!(!is_line_submit(no), "{no:?}");
    }
}

#[test]
fn a_backgrounded_session_keeps_its_link_estimate() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    let slow = Duration::from_millis(60);
    let fast = Duration::from_millis(1);
    let mut at = echo_turn(&mut p, 'a', 0, now, slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    assert_eq!(p.overlay(at).len(), 1, "control: the slow link paints");

    let ssh = p.take_link_estimate();
    assert!(
        p.idle(),
        "the outgoing pane's guesses do not follow the window"
    );
    at = echo_turn(&mut p, 'x', 0, at, fast);
    assert!(p.predict_char('y', (0, 1), G, 80, at));
    assert!(p.overlay(at).is_empty(), "a fast local link stays dark");
    let local = p.take_link_estimate();

    p.restore_link_estimate(ssh);
    at += p.glitch_window() + Duration::from_millis(1);
    at = echo_turn(&mut p, 'a', 0, at, slow);
    assert!(p.predict_char('b', (0, 1), G, 80, at));
    assert_eq!(
        p.overlay(at).len(),
        1,
        "a restored slow-link estimate paints the second keystroke"
    );

    p.restore_link_estimate(local);
    at = echo_turn(&mut p, 'x', 0, at, fast);
    assert!(p.predict_char('y', (0, 1), G, 80, at));
    assert!(
        p.overlay(at).is_empty(),
        "restoring a fast session's estimate must not paint either"
    );
}

#[test]
fn a_returning_sessions_old_echo_cannot_prove_its_new_guess() {
    let slow = Duration::from_millis(60);
    let start = t0();

    let mut control = Predictor::new(PredictMode::Adaptive);
    let mut at = echo_turn(&mut control, 'a', 0, start, slow);
    at = echo_turn(&mut control, 'b', 1, at, slow);
    let estimate = control.take_link_estimate();
    assert!(control.idle());
    control.restore_link_estimate(estimate);
    at = echo_turn(&mut control, 'q', 0, at, slow);
    assert!(control.predict_char('r', (0, 1), G, 80, at));
    assert_eq!(control.overlay(at).len(), 1, "debt-free return paints");

    let mut verdicts = Vec::new();
    for pending_guess in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, start, slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(p.adaptive_slow && p.confirmed_epoch);
        if pending_guess {
            assert!(p.predict_char('n', (0, 2), G, 80, at));
            assert_eq!(p.overlay(at).len(), 1);
        } else {
            p.note_unmodelled_input(at);
        }
        let parked = p.take_link_estimate();
        assert!(p.idle());
        assert!(p.overlay(at).is_empty(), "outgoing pixels do not travel");

        at = echo_turn(&mut p, 'x', 0, at, Duration::from_millis(1));
        assert!(p.confirmed_epoch);
        let _other = p.take_link_estimate();
        p.restore_link_estimate(parked);
        assert!(p.adaptive_slow, "the returning link is still known slow");
        assert!(!p.confirmed_epoch, "a restored line must prove itself");

        assert!(p.predict_char('n', (0, 2), G, 80, at));
        assert!(p.overlay(at).is_empty());
        at += slow;
        p.reconcile(
            Some((0, 3)),
            G,
            false,
            at,
            cell(&[((0, 0), 'a'), ((0, 1), 'b'), ((0, 2), 'n')]),
        );
        assert!(p.idle(), "a matching glyph retires the guess's pixels");
        let borrowed_proof = p.confirmed_epoch;
        assert!(p.predict_char(' ', (0, 3), G, 80, at));
        let displayed: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
        verdicts.push((pending_guess, borrowed_proof, displayed));

        at += slow;
        p.reconcile(
            Some((0, 4)),
            G,
            false,
            at,
            cell(&[((0, 0), 'a'), ((0, 1), 'b'), ((0, 2), 'n'), ((0, 3), 'n')]),
        );
        assert!(p.idle());
        at += p.glitch_window() + Duration::from_millis(1);
        at = echo_turn(&mut p, 'q', 4, at, slow);
        assert!(p.predict_char('r', (0, 5), G, 80, at));
        assert_eq!(p.overlay(at).len(), 1, "fresh proof paints again");
    }
    assert_eq!(
        verdicts,
        [(true, false, Vec::new()), (false, false, Vec::new())],
        "a parked session's old output cannot prove a new guess or paint its space"
    );
}

#[test]
fn restoring_an_estimate_clears_the_outgoing_panes_pixels() {
    let mut a = Predictor::new(PredictMode::Always);
    let now = t0();
    assert!(a.predict_char('x', (3, 4), G, 80, now));
    assert_eq!(a.overlay(now).len(), 1, "pane A has a ghost on glass");
    assert!(a.is_displaying(now));

    a.restore_link_estimate(LinkEstimate::default());

    assert!(
        a.overlay(now).is_empty(),
        "the outgoing pane's speculative pixels must not survive the swap"
    );
    assert!(!a.is_displaying(now));
    assert!(
        !a.confirmed_epoch,
        "nor may its confirmation license the incoming line"
    );
}

#[test]
fn a_default_estimate_is_the_no_estimate_path() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    let slow = Duration::from_millis(60);
    let mut at = echo_turn(&mut p, 'a', 0, now, slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.adaptive_slow, "control: the latch is open");

    p.restore_link_estimate(LinkEstimate::default());
    at = echo_turn(&mut p, 'a', 0, at, slow);
    assert!(p.predict_char('b', (0, 1), G, 80, at));
    assert!(
        p.overlay(at).is_empty(),
        "an unmeasured session re-earns the latch from its own samples"
    );
    at += slow;
    p.reconcile(Some((0, 2)), G, false, at, cell(&[((0, 1), 'b')]));
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    assert_eq!(p.overlay(at).len(), 1, "…and opens it on the second sample");
}

#[test]
fn the_cold_link_probe_travels_with_the_session_estimate() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    assert!(p.predict_char('a', (0, 0), G, 80, now));
    let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
    assert_eq!(
        p.expiry_backoff, 1,
        "control: the timeout widened the probe"
    );

    let est = p.take_link_estimate();
    assert_eq!(p.expiry_backoff, 0, "an unknown link probes from the floor");
    p.restore_link_estimate(est);
    assert_eq!(
        p.expiry_backoff, 1,
        "…the probing session gets its window back"
    );
}

#[test]
fn session_reset_restarts_the_cold_link_probe() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = t0();
    assert!(p.predict_char('a', (0, 0), G, 80, now));
    let _ = p.overlay(now + Duration::from_millis(GLITCH_FLOOR_MS + 1));
    assert_eq!(
        p.expiry_backoff, 1,
        "control: the timeout widened the probe"
    );
    p.reset_session();
    assert_eq!(p.expiry_backoff, 0);
    assert_eq!(p.glitch_window(), Duration::from_millis(GLITCH_FLOOR_MS));
}

#[test]
fn an_erase_we_did_not_model_leaves_no_ghost_where_nothing_echoes() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let slow = Duration::from_millis(60);
    let mut at = echo_turn(&mut p, 'a', 0, t0(), slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(
        p.adaptive_slow && p.confirmed_epoch,
        "control: a slow, proven line"
    );
    assert!(
        !p.predict_backspace(at),
        "nothing of ours to pop: 'b' already echoed"
    );
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    let wrong_cell = |p: &mut Predictor, at| {
        p.overlay(at)
            .iter()
            .any(|g| (g.row, g.col, g.ch) == (0, 2, 'c'))
    };
    assert!(
        !wrong_cell(&mut p, at),
        "'c' will echo at column 1, not 2: it must not be painted at 2"
    );
    at += slow;
    p.reconcile(
        Some((0, 2)),
        G,
        false,
        at,
        cell(&[((0, 0), 'a'), ((0, 1), 'c')]),
    );
    assert!(
        !wrong_cell(&mut p, at),
        "after the echo a ghost 'c' beside the real one is a stale duplicate"
    );
}

#[test]
fn space_confirms_by_cursor_advance() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    p.predict_char('a', (0, 0), G, 80, now);
    p.predict_char(' ', (0, 0), G, 80, now);
    p.predict_char('b', (0, 0), G, 80, now);
    p.reconcile(Some((0, 2)), G, false, now, cell(&[((0, 0), 'a')]));
    let shown = p.overlay(now);
    assert_eq!(shown.len(), 1, "'a' and the space retire; only 'b' remains");
    assert_eq!(shown[0].ch, 'b');
}

#[test]
fn pending_queue_caps_at_max_predictions() {
    let mut p = Predictor::new(PredictMode::Always);
    let now = t0();
    for i in 0..MAX_PREDICTIONS {
        assert!(
            p.predict_char_in_grid('a', (0, 0), G, (80, 24), now),
            "prediction {i} below MAX_PREDICTIONS must succeed"
        );
    }
    assert_eq!(p.preds.len(), MAX_PREDICTIONS);
    assert!(
        !p.predict_char_in_grid('z', (0, 0), G, (80, 24), now),
        "prediction past MAX_PREDICTIONS must be declined"
    );
    assert_eq!(p.preds.len(), MAX_PREDICTIONS);
    for _ in 0..4 {
        p.note_line_submit(now);
        assert!(!p.predict_char_in_grid('b', (0, 0), G, (80, 24), now));
    }
    assert_eq!(p.preds.len(), MAX_PREDICTIONS);
}

fn seen_row(text: &str) -> Vec<Seen> {
    let mut row: Vec<Seen> = text.chars().map(|c| Seen::of_cell(c, false)).collect();
    row.resize(12, Seen::Blank);
    row
}

fn seen_cells(text: &str) -> impl FnMut(u16, u16) -> Seen + '_ {
    move |r, c| {
        let ch = if r == 0 {
            text.chars().nth(usize::from(c))
        } else {
            None
        };
        ch.map_or(Seen::Blank, |ch| Seen::of_cell(ch, false))
    }
}

fn slow_link_then(p: &mut Predictor, text: &str) -> Instant {
    let slow = Duration::from_millis(60);
    let mut at = echo_turn(p, 'a', 0, t0(), slow);
    at = echo_turn(p, 'b', 1, at, slow);
    assert!(p.adaptive_slow, "fixture: a slow link");
    p.note_unmodelled_input(at);
    at += Duration::from_millis(10);
    p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells(text));
    at + p.glitch_window() + Duration::from_millis(1)
}

#[test]
fn a_mid_line_echo_proves_nothing() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let at = slow_link_then(&mut p, "ab cd");
    let row = seen_row("ab cd");
    assert!(p.wants_key_row(), "the run starts on the live cursor");
    let key = KeySample {
        cursor: (0, 2),
        coords: G,
        grid: (12, 6),
        row: Some(&row),
        ..KeySample::default()
    };
    assert!(p.predict_glyph('e', &key, at));
    assert!(!p.wants_key_row(), "type-ahead reads no row");
    let echoed = at + Duration::from_millis(60);
    let before = p.events_for_test().tail_refusals;
    p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abe cd"));
    assert!(p.idle(), "glyph and cursor match: the pixels retire");
    assert_eq!(p.events_for_test().tail_refusals, before + 1);
    assert!(!p.confirmed_epoch, "a shifted tail proves nothing");
    let row = seen_row("abe cd");
    let key = KeySample {
        cursor: (0, 3),
        row: Some(&row),
        ..key
    };
    assert!(p.predict_glyph('i', &key, echoed));
    assert!(
        p.overlay(echoed).is_empty(),
        "'i' is not painted over the shifted space"
    );

    let mut p = Predictor::new(PredictMode::Adaptive);
    p.disable_rule_for_test(Rule::TailGuard);
    let at = slow_link_then(&mut p, "ab cd");
    let row = seen_row("ab cd");
    let key = KeySample {
        cursor: (0, 2),
        coords: G,
        grid: (12, 6),
        row: Some(&row),
        ..KeySample::default()
    };
    assert!(p.predict_glyph('e', &key, at));
    let echoed = at + Duration::from_millis(60);
    p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abe cd"));
    assert!(p.confirmed_epoch, "rule off: the mid-line echo proves");
    assert!(p.predict_char('i', (0, 3), G, 12, echoed));
    assert_eq!(p.overlay(echoed).len(), 1);
}

#[test]
fn a_right_prompt_the_echo_leaves_in_place_still_proves() {
    let screen = "ab      <r>";
    let echoed_screen = "abc     <r>";
    for with_row in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let at = slow_link_then(&mut p, screen);
        let row = seen_row(screen);
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: with_row.then_some(row.as_slice()),
            ..KeySample::default()
        };
        assert!(p.predict_glyph('c', &key, at));
        let echoed = at + Duration::from_millis(60);
        let decorated = p.events_for_test().decorated_proofs;
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells(echoed_screen));
        assert!(p.idle());
        assert_eq!(p.confirmed_epoch, with_row, "with the row: {with_row}");
        assert_eq!(
            p.events_for_test().decorated_proofs,
            decorated + u64::from(with_row)
        );
        assert!(p.predict_char('d', (0, 3), G, 12, echoed));
        assert_eq!(p.overlay(echoed).len(), usize::from(with_row));
    }
}

#[test]
fn a_run_typed_over_other_text_never_proves() {
    for (under, typed, proves) in [('x', 'e', false), ('e', 'e', true)] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let screen: String = ['a', 'b', under, 'd'].iter().collect();
        let at = slow_link_then(&mut p, &screen);
        let row = seen_row(&screen);
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        assert!(p.predict_glyph(typed, &key, at));
        let echoed = at + Duration::from_millis(60);
        let after: String = ['a', 'b', typed, 'd'].iter().collect();
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells(&after));
        assert!(p.idle());
        assert_eq!(p.confirmed_epoch, proves, "{typed:?} typed over {under:?}");
    }

    for (under, proves) in [('x', false), ('q', true)] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let screen: String = ['a', 'b', ' ', under, 'd'].iter().collect();
        let at = slow_link_then(&mut p, &screen);
        let row = seen_row(&screen);
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        assert!(p.predict_glyph('e', &key, at));
        assert!(p.predict_glyph('q', &key, at));
        let echoed = at + Duration::from_millis(60);
        p.reconcile_seen(Some((0, 4)), G, false, echoed, seen_cells("abeqd"));
        assert!(p.idle());
        assert_eq!(p.confirmed_epoch, proves, "'q' typed over {under:?}");
    }
}

#[test]
fn a_dirty_tail_voids_a_standing_proof() {
    for guard in [true, false] {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        if !guard {
            p.disable_rule_for_test(Rule::TailGuard);
        }
        assert!(p.predict_char('n', (0, 2), G, 12, at));
        assert!(p.predict_char('o', (0, 2), G, 12, at));
        assert_eq!(
            p.overlay(at).len(),
            2,
            "control: the proven line paints both"
        );
        let echoed = at + Duration::from_millis(60);
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abn  sug"));
        assert_eq!(p.preds.len(), 1, "'o' still waits on its blank cell");
        assert_eq!(p.confirmed_epoch, !guard, "guard {guard}: proof voided");
        assert_eq!(
            p.overlay(echoed).len(),
            usize::from(!guard),
            "guard {guard}"
        );
    }
}

#[test]
fn a_retired_burst_reads_its_tail_once() {
    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    for ch in ['c', 'd', 'e', 'f'] {
        assert!(p.predict_char(ch, (0, 2), G, 12, at));
    }
    let echoed = at + Duration::from_millis(60);
    let mut reads = 0usize;
    let mut observe = seen_cells("abcdef");
    p.reconcile_seen(Some((0, 6)), G, false, echoed, |r, c| {
        reads += 1;
        observe(r, c)
    });
    assert!(p.idle() && p.confirmed_epoch);
    assert_eq!(reads, 1 + 4 + 9, "observe calls for a four-guess burst");
}

type Keystroke<'a> = (char, u16, &'a str, &'a str, u16);

fn typed_at_the_prompt(
    p: &mut Predictor,
    key: Keystroke<'_>,
    with_row: bool,
    at: Instant,
) -> (bool, Instant) {
    let (ch, col, screen, echoed, cursor) = key;
    let row = seen_row(screen);
    let key = KeySample {
        cursor: (0, col),
        coords: G,
        grid: (12, 6),
        row: with_row.then_some(row.as_slice()),
        ..KeySample::default()
    };
    assert!(p.wants_key_row(), "one key per round trip: nothing pending");
    assert!(p.predict_glyph(ch, &key, at), "{ch:?} is guessed");
    let frame = at + MS(10);
    p.reconcile_seen(Some((0, col)), G, false, frame, seen_cells(screen));
    let shown = p.overlay(frame).iter().any(|g| (g.col, g.ch) == (col, ch));
    let landed = at + MS(100);
    p.reconcile_seen(Some((0, cursor)), G, false, landed, seen_cells(echoed));
    assert!(p.idle(), "{ch:?} retired by its echo");
    (shown, landed + MS(5))
}

#[test]
fn a_suggestion_beside_the_end_of_the_line_darkens_no_key() {
    let keys: [Keystroke<'_>; 6] = [
        ('e', 2, "$", "$ e", 3),
        ('c', 3, "$ e", "$ ec", 4),
        ('h', 4, "$ ec", "$ echo hello", 5),
        ('x', 5, "$ echo hello", "$ echx", 6),
        ('y', 6, "$ echx", "$ echxyzz", 7),
        ('z', 7, "$ echxyzz", "$ echxyzz", 8),
    ];
    for with_row in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = t0();
        let mut shown = Vec::new();
        for key in keys {
            let (on_glass, next) = typed_at_the_prompt(&mut p, key, with_row, at);
            shown.push(on_glass);
            at = next;
        }
        let events = p.events_for_test();
        if with_row {
            assert_eq!(
                shown,
                [false, false, true, true, true, true],
                "two echoes latch the slow link, then every key paints"
            );
            assert_eq!(events.tail_refusals, 0, "{events:?}");
            assert!(
                events.advance_waits >= 1,
                "z waited for its echo: {events:?}"
            );
            assert!(p.confirmed_epoch);
        } else {
            assert_eq!(shown, [false, false, true, false, false, false], "no row");
            assert_eq!(events.tail_refusals, 1, "{events:?}");
        }
    }
}

#[test]
fn a_glyph_typed_over_the_text_its_press_saw_waits_for_its_echo() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Always);
        if !rule_on {
            p.disable_rule_for_test(Rule::ScreenAsItWas);
        }
        let at = t0();
        let row = seen_row("$ du");
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        assert!(p.predict_glyph('s', &key, at));
        let unread = at + MS(5);
        p.reconcile_seen(Some((0, 2)), G, false, unread, seen_cells("$ du"));
        assert_eq!(
            p.pending().len(),
            usize::from(rule_on),
            "rule on {rule_on}: the screen as the press saw it"
        );
        let echoed = unread + MS(60);
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("$ sdu"));
        assert!(
            p.idle(),
            "rule on {rule_on}: retired by its echo or flushed"
        );
        assert_eq!(
            p.take_credited_for_test().len(),
            usize::from(rule_on),
            "rule on {rule_on}: credited with its own echo, or gone before it"
        );
    }
}

#[test]
fn a_glyph_waiting_over_the_line_s_text_is_not_painted() {
    for rule_on in [true, false] {
        let fresh = || {
            let mut p = Predictor::new(PredictMode::Always);
            if !rule_on {
                p.disable_rule_for_test(Rule::DarkOverText);
            }
            p
        };
        let row = seen_row("$ du -sh");
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        let mut p = fresh();
        let at = t0();
        assert!(p.predict_glyph('s', &key, at));
        assert_eq!(
            p.overlay(at).len(),
            usize::from(!rule_on),
            "rule on {rule_on}: s at its press, before any reconcile"
        );
        let unread = at + MS(5);
        p.reconcile_seen(Some((0, 2)), G, false, unread, seen_cells("$ du -sh"));
        assert_eq!(p.pending().len(), 1, "rule on {rule_on}: s waits");
        assert_eq!(
            p.overlay(unread).len(),
            usize::from(!rule_on),
            "rule on {rule_on}: s over the d of du"
        );
        assert_eq!(p.shown_through(), (!rule_on).then_some(0));
        assert_eq!(p.events_for_test().text_waits, u64::from(rule_on));
        let echoed = unread + MS(60);
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("$ sdu -sh"));
        assert!(p.idle(), "rule on {rule_on}: s retired by its echo");

        let mut p = fresh();
        p.note_unmodelled_input(at);
        let typed = at + MS(30);
        assert!(p.predict_glyph('l', &key, typed));
        assert_eq!(
            p.overlay(typed).len(),
            usize::from(!rule_on),
            "rule on {rule_on}: l at its press, before any reconcile"
        );
        let unread = typed + MS(5);
        p.reconcile_seen(Some((0, 2)), G, false, unread, seen_cells("$ du -sh"));
        assert_eq!(
            p.overlay(unread).len(),
            usize::from(!rule_on),
            "rule on {rule_on}: l over the d Ctrl-K is still to erase"
        );
        let erased = at + MS(60);
        p.reconcile_seen(Some((0, 2)), G, false, erased, seen_cells("$"));
        let shown: Vec<(u16, char)> = p.overlay(erased).iter().map(|g| (g.col, g.ch)).collect();
        assert_eq!(
            shown,
            [(2, 'l')],
            "rule on {rule_on}: l over the erased row"
        );
        let echoed = typed + MS(60);
        p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("$ l"));
        assert!(p.idle(), "rule on {rule_on}: l retired by its echo");

        let mut p = fresh();
        let keys: [Keystroke<'_>; 4] = [
            ('e', 2, "$", "$ e", 3),
            ('c', 3, "$ e", "$ ec", 4),
            ('h', 4, "$ ec", "$ echo hello", 5),
            ('x', 5, "$ echo hello", "$ echx", 6),
        ];
        let mut at = t0();
        let mut shown = Vec::new();
        for key in keys {
            let (on_glass, next) = typed_at_the_prompt(&mut p, key, true, at);
            shown.push(on_glass);
            at = next;
        }
        assert_eq!(shown, [true; 4], "rule on {rule_on}: x over the suggestion");
        assert_eq!(p.events_for_test().text_waits, 0);
    }
}

#[test]
fn a_guess_after_the_head_over_the_line_s_text_is_not_painted() {
    for rule_on in [true, false] {
        for (screen, cursor, typed) in [("$ ssh host", 2u16, "su"), ("$ git commit", 5, " -C x")] {
            let mut p = Predictor::new(PredictMode::Always);
            if !rule_on {
                p.disable_rule_for_test(Rule::DarkOverText);
            }
            let row = seen_row(screen);
            let key = KeySample {
                cursor: (0, cursor),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            let mut at = t0();
            for ch in typed.chars() {
                assert!(p.predict_glyph(ch, &key, at), "{ch:?} is guessed");
                at += MS(5);
            }
            let painted = if rule_on { 1 } else { typed.len() };
            let expect: Vec<(u16, char)> = (cursor..).zip(typed.chars()).take(painted).collect();
            let shown = |p: &mut Predictor| -> Vec<(u16, char)> {
                p.overlay(at).iter().map(|g| (g.col, g.ch)).collect()
            };
            assert_eq!(
                shown(&mut p),
                expect,
                "rule on {rule_on}, {screen:?}: at the presses"
            );
            p.reconcile_seen(Some((0, cursor)), G, false, at, seen_cells(screen));
            assert_eq!(
                p.pending().len(),
                typed.len(),
                "{screen:?}: every guess waits"
            );
            assert_eq!(
                shown(&mut p),
                expect,
                "rule on {rule_on}, {screen:?}: at the reconcile"
            );
            assert_eq!(p.events_for_test().text_waits, u64::from(rule_on));
        }
    }
}

#[test]
fn a_hold_over_the_line_s_text_outlives_its_guess() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Always);
        if !rule_on {
            p.disable_rule_for_test(Rule::DarkOverText);
        }
        let row = seen_row("$ du -sh");
        let key = KeySample {
            cursor: (0, 2),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        let at = t0();
        for (ch, after) in [('x', 0), ('y', 50), ('z', 100)] {
            assert!(
                p.predict_glyph(ch, &key, at + MS(after)),
                "{ch:?} is guessed"
            );
        }
        let shown = |p: &mut Predictor, now| -> Vec<(u16, char)> {
            p.overlay(now).iter().map(|g| (g.col, g.ch)).collect()
        };
        let read = at + MS(110);
        p.reconcile_seen(Some((0, 2)), G, false, read, seen_cells("$ du -sh"));
        let all = [(2, 'x'), (3, 'y'), (4, 'z')];
        let expect = |from: usize| {
            if rule_on {
                Vec::new()
            } else {
                all[from..].to_vec()
            }
        };
        assert_eq!(shown(&mut p, read), expect(0), "rule on {rule_on}: x waits");
        assert_eq!(p.events_for_test().text_waits, u64::from(rule_on));
        let x_gone = at + MS(251);
        assert_eq!(
            shown(&mut p, x_gone),
            expect(1),
            "rule on {rule_on}: x expired"
        );
        assert_eq!(p.pending().len(), 2, "x expired, y and z wait");
        let y_gone = at + MS(551);
        assert_eq!(
            shown(&mut p, y_gone),
            expect(2),
            "rule on {rule_on}: y expired"
        );
        assert_eq!(p.pending().len(), 1, "y expired, z waits");
    }
}

#[test]
fn a_glyph_echoed_before_its_tail_moves_proves_nothing_after() {
    for rule_on in [true, false] {
        for i_first in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::StandingTail);
            }
            let at = slow_link_then(&mut p, "ab cd");
            let row = seen_row("ab cd");
            let key = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            assert!(p.predict_glyph('e', &key, at));
            let glyph_only = at + MS(60);
            p.reconcile_seen(Some((0, 3)), G, false, glyph_only, seen_cells("abecd"));
            assert!(p.idle() && p.confirmed_epoch, "the glyph alone proves");
            let (press_row, redrawn) = if i_first {
                ("abecd", glyph_only + MS(5))
            } else {
                let redrawn = glyph_only + MS(5);
                p.reconcile_seen(Some((0, 3)), G, false, redrawn, seen_cells("abe cd"));
                ("abe cd", redrawn)
            };
            let row = seen_row(press_row);
            let key = KeySample {
                cursor: (0, 3),
                row: Some(&row),
                ..key
            };
            assert!(p.predict_glyph('i', &key, redrawn));
            let frame = redrawn + MS(5);
            p.reconcile_seen(Some((0, 3)), G, false, frame, seen_cells("abe cd"));
            assert_eq!(
                p.overlay(frame).len(),
                usize::from(!rule_on),
                "rule on {rule_on}, i typed before the redraw {i_first}"
            );
            if rule_on {
                assert_eq!(p.events_for_test().standing_voids, 1);
            }
        }
    }
}

#[test]
fn a_shifted_glyph_is_not_a_pending_guess_s_echo() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let at = slow_link_then(&mut p, "abo");
    let row = seen_row("abo");
    let key = KeySample {
        cursor: (0, 2),
        coords: G,
        grid: (12, 6),
        row: Some(&row),
        ..KeySample::default()
    };
    assert!(p.predict_glyph('o', &key, at));
    let glyph_only = at + MS(60);
    p.reconcile_seen(Some((0, 3)), G, false, glyph_only, seen_cells("abo"));
    assert!(p.idle() && p.confirmed_epoch, "the glyph alone proves");
    let key = KeySample {
        cursor: (0, 3),
        ..key
    };
    assert!(p.predict_glyph('o', &key, glyph_only));
    assert!(p.predict_glyph('z', &key, glyph_only));
    assert_eq!(
        p.overlay(glyph_only).len(),
        2,
        "the proven line paints both"
    );
    let redrawn = glyph_only + MS(5);
    p.reconcile_seen(Some((0, 3)), G, false, redrawn, seen_cells("aboo"));
    assert_eq!(p.preds.len(), 2, "the second 'o' waits for its own echo");
    assert!(!p.confirmed_epoch, "the displaced 'o' is not its echo");
    assert!(
        p.overlay(redrawn).is_empty(),
        "'z' is not painted a cell short"
    );
}

#[test]
fn a_glyph_already_on_screen_is_not_its_echo() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::CursorAdvance);
        }
        let slow = MS(300);
        let mut at = t0();
        for (col, ch) in (0u16..).zip(['a', 'b', 'o']) {
            let screen: String = "abo".chars().take(usize::from(col)).collect();
            let row = seen_row(&screen);
            let key = KeySample {
                cursor: (0, col),
                coords: G,
                grid: (12, 6),
                row: Some(&row),
                ..KeySample::default()
            };
            assert!(p.predict_glyph(ch, &key, at));
            at += slow;
            let echoed: String = "abo".chars().take(usize::from(col) + 1).collect();
            p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&echoed));
        }
        assert!(
            p.confirmed_epoch && p.adaptive_slow && p.proof_line_end,
            "fixture: a slow line, proven at its end"
        );
        let window = p.glitch_window();
        assert_eq!(window, MS(900), "fixture: three round trips");
        let screen = "abodice";
        let row = seen_row(screen);
        let key = KeySample {
            cursor: (0, 3),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        for ch in ['d', 'i', 'c', 'e'] {
            assert!(p.predict_glyph(ch, &key, at));
        }
        let frame = at + MS(5);
        p.reconcile_seen(Some((0, 3)), G, false, frame, seen_cells(screen));
        if rule_on {
            assert_eq!(p.preds.len(), 4, "nothing echoed yet");
            assert_eq!(p.glitch_window(), window, "and nothing measured");
            assert_eq!(p.overlay(frame).len(), 4, "on glass over the suggestion");
            let echoed = at + slow;
            p.reconcile_seen(Some((0, 7)), G, false, echoed, seen_cells(screen));
            assert!(p.idle() && p.confirmed_epoch, "retired by their echoes");
            assert_eq!(p.glitch_window(), window, "a 300 ms sample, as before");
        } else {
            assert!(p.idle(), "rule off: retired before any echo");
            assert!(
                p.glitch_window() < MS(300),
                "rule off: the window collapsed below the round trip: {:?}",
                p.glitch_window()
            );
        }
    }
}

#[test]
fn a_cancelled_glyph_s_echo_is_owed_to_the_same_glyph_typed_next() {
    for rule_on in [true, false] {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::CancelOwes);
        }
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert!(p.predict_backspace(at), "our own guess, cancelled");
        let again = at + MS(20);
        assert!(p.predict_char('c', (0, 2), G, 80, again));
        assert_eq!(
            p.preds.front().map(|g| g.col),
            Some(2),
            "on the cancelled cell"
        );
        let _ = p.take_credited_for_test();
        p.reconcile(
            Some((0, 3)),
            G,
            false,
            at + MS(60),
            cell(&after_ab(&[((0, 2), 'c')])),
        );
        assert!(
            p.idle(),
            "the cell shows the guess's glyph, the cursor past it"
        );
        assert_eq!(
            p.take_credited_for_test().len(),
            usize::from(!rule_on),
            "rule on {rule_on}: credited with the cancelled key's echo"
        );
        assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
    }
}

#[test]
fn a_right_to_left_row_is_never_guessed_on() {
    let opaque = |text: &str| -> Vec<Seen> {
        let mut row: Vec<Seen> = text.chars().map(|c| Seen::of_cell(c, true)).collect();
        row.resize(12, Seen::Blank);
        row
    };
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::RightToLeftRows);
        }
        let at = slow_link_then(&mut p, "");
        let screen = "$ אב    <r>";
        let row = opaque(screen);
        assert_eq!(
            row.get(5),
            Some(&Seen::Opaque(None)),
            "fixture: an opaque blank"
        );
        let key = KeySample {
            cursor: (0, 5),
            coords: G,
            grid: (12, 6),
            row: Some(&row),
            ..KeySample::default()
        };
        assert_eq!(
            p.predict_glyph('e', &key, at),
            !rule_on,
            "rule on {rule_on}"
        );
        if rule_on {
            assert!(p.idle() && p.owed_at == Some(at), "declined, owed");
            assert_eq!(p.events_for_test().bidi_refusals, 1);
            let plain = seen_row("$ ab");
            let key = KeySample {
                row: Some(&plain),
                cursor: (0, 4),
                ..key
            };
            assert!(!p.predict_glyph('\u{5d0}', &key, at));
            assert_eq!(p.events_for_test().bidi_refusals, 2);
            continue;
        }
        let echoed = opaque("$ אב e  <r>");
        let landed = at + MS(60);
        p.reconcile_seen(Some((0, 6)), G, false, landed, |r, c| {
            if r == 0 {
                echoed.get(usize::from(c)).copied().unwrap_or(Seen::Blank)
            } else {
                Seen::Blank
            }
        });
        assert!(p.idle());
        assert!(
            p.confirmed_epoch,
            "the right prompt stayed, the line proves"
        );
        assert_eq!(p.events_for_test().tail_refusals, 0);
    }
}

#[test]
fn a_predictor_still_off_wants_the_row_of_the_press_that_starts_it() {
    let mut p = Predictor::default();
    assert!(p.wants_key_row(), "Off, with nothing pending");
    p.set_mode(PredictMode::Adaptive);
    assert!(p.wants_key_row());
    let row = seen_row("ab      <r>");
    let key = KeySample {
        cursor: (0, 2),
        coords: G,
        grid: (12, 6),
        row: Some(&row),
        ..KeySample::default()
    };
    assert!(p.predict_glyph('c', &key, t0()));
    assert!(!p.wants_key_row(), "type-ahead reads no row");
    assert_eq!(p.tail, Some((0, 2)), "the first run took its snapshot");
}

const MS: fn(u64) -> Duration = Duration::from_millis;

fn typed_owe_run(second: char, echo_x: &str, rule_off: Option<Rule>) -> (Predictor, bool) {
    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    if let Some(rule) = rule_off {
        p.disable_rule_for_test(rule);
    }
    p.note_line_submit(at);
    assert!(p.predict_char('x', (1, 0), G, 12, at + MS(100)));
    let row1 = |text: &str| -> Vec<((u16, u16), char)> {
        (0u16..)
            .zip(text.chars())
            .filter(|&(_, ch)| ch != ' ')
            .map(|(col, ch)| ((1, col), ch))
            .collect()
    };
    p.reconcile(Some((1, 1)), G, false, at + MS(160), cell(&row1(echo_x)));
    assert!(p.idle() && !p.confirmed_epoch, "x retired without credit");
    assert!(p.predict_char(second, (1, 1), G, 12, at + MS(200)));
    let echoed: String = ['x', second].iter().collect();
    p.reconcile(Some((1, 2)), G, false, at + MS(270), cell(&row1(&echoed)));
    assert!(p.idle());
    assert!(p.predict_char('z', (1, 2), G, 12, at + MS(270)));
    let painted = !p.overlay(at + MS(270)).is_empty();
    (p, painted)
}

#[test]
fn an_uncredited_guess_owes_only_its_own_glyph() {
    let (p, proved) = typed_owe_run('y', "x", None);
    assert!(
        proved,
        "a different glyph earns credit past the Enter's window"
    );
    assert_eq!(p.events_for_test().typed_owes, 1, "x owed typed");
    let (_, proved) = typed_owe_run('x', "x", None);
    assert!(!proved, "the same glyph may be x's own echo: no credit");
    let (_, proved) = typed_owe_run('y', "x", Some(Rule::TypedOwes));
    assert!(
        !proved,
        "TypedOwes off: owed as any echo, the cascade refuses y"
    );
    let (_, proved) = typed_owe_run('x', "x", Some(Rule::TypedOwesSameGlyph));
    assert!(proved, "TypedOwesSameGlyph off: the second x is credited");
}

#[test]
fn a_guess_retired_over_a_moved_tail_owes_any_echo() {
    let moved = "x  sug";
    let (p, proved) = typed_owe_run('y', moved, None);
    assert!(!proved, "owed as any echo: y refused credit");
    assert_eq!(
        p.events_for_test().typed_owes,
        1,
        "only y's own retirement is typed"
    );
    let (_, proved) = typed_owe_run('y', moved, Some(Rule::TypedOwesCleanTail));
    assert!(proved, "TypedOwesCleanTail off: typed over a moved tail");
}

#[test]
fn typed_owes_fold_into_any_echo_past_the_cap_and_at_a_take() {
    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    p.note_line_submit(at);
    let born = |i: u16| at + MS(10 + u64::from(i));
    for i in 0..=64u16 {
        assert!(p.predict_char('x', (1, i), G, 80, born(i)));
    }
    assert!(p.predict_char('y', (1, 65), G, 80, born(65)));
    let echoed: Vec<((u16, u16), char)> = (0..=64u16).map(|i| ((1, i), 'x')).collect();
    p.reconcile(Some((1, 65)), G, false, at + MS(120), cell(&echoed));
    assert_eq!(p.preds.len(), 1, "y still waits");
    assert_eq!(p.glyph_owes.len(), MAX_GLYPH_OWES, "capped");
    assert_eq!(p.owed_at, Some(born(0)), "the oldest folded into any echo");
    assert_eq!(
        p.preds.front().and_then(|g| g.owed),
        Some(born(0)),
        "…raised for y"
    );
    let estimate = p.take_link_estimate();
    assert!(p.glyph_owes.is_empty());
    p.restore_link_estimate(estimate);
    assert_eq!(
        p.owed_at,
        Some(born(65)),
        "the session carries its typed owes and pending guesses as owes of any echo"
    );
}

#[test]
fn a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move() {
    fn proven(cols: u16, start: u16) -> (Predictor, Instant) {
        let mut predictor = Predictor::new(PredictMode::Adaptive);
        let mut at = t0();
        let mut cells = Vec::new();
        for (offset, ch) in [(0, 'a'), (1, 'b')] {
            let col = start + offset;
            assert!(at_the_end(&mut predictor, ch, (0, col), (cols, 6), at));
            cells.push(((0, col), ch));
            at += MS(60);
            predictor.reconcile(
                Some((0, (col + 1).min(cols - 1))),
                G,
                false,
                at,
                cell(&cells),
            );
            assert!(predictor.idle(), "ordinary echo retires its guess");
        }
        (predictor, at)
    }

    for ahead in [false, true] {
        let (mut predictor, mut at) = proven(80, 0);
        assert!(predictor.predict_char_in_grid('c', (0, 2), G, (80, 6), at));
        assert_eq!(
            predictor.overlay(at).len(),
            1,
            "the original line is proven"
        );
        assert!(predictor.predict_backspace(at));
        assert!(predictor.idle(), "only our trailing guess was cancelled");
        at += MS(30);
        let cursor = (0, if ahead { 3 } else { 2 });
        let cells = if ahead {
            after_ab(&[((0, 2), 'c')])
        } else {
            AB.to_vec()
        };
        predictor.reconcile(Some(cursor), G, false, at, cell(&cells));
        assert!(predictor.predict_char_in_grid('d', cursor, G, (80, 6), at));
        assert_eq!(predictor.pending_bounds(), Some((0, 2, 1, 3)));
        assert_eq!(
            predictor.overlay(at).len(),
            1,
            "legitimate pair transit retains proof"
        );
    }
    let (mut predictor, mut at) = proven(9, 6);
    assert!(predictor.predict_char_in_grid('c', (0, 8), G, (9, 6), at));
    assert_eq!(predictor.overlay(at).len(), 1);
    assert!(predictor.predict_backspace(at));
    at += MS(30);
    predictor.reconcile(
        Some((0, 8)),
        G,
        false,
        at,
        cell(&[((0, 6), 'a'), ((0, 7), 'b'), ((0, 8), 'c')]),
    );
    assert!(predictor.predict_char_in_grid('d', (0, 8), G, (9, 6), at));
    assert_eq!(
        predictor.pending_bounds(),
        Some((0, 8, 1, 9)),
        "last-column parking stays in its cell"
    );
    assert_eq!(predictor.overlay(at).len(), 1);

    let (mut predictor, at) = proven(9, 7);
    assert!(predictor.predict_char_in_grid('c', (0, 8), G, (9, 6), at));
    assert_eq!(predictor.pending_bounds(), Some((1, 0, 2, 1)));
    assert!(predictor.predict_backspace(at));
    predictor.reconcile(
        Some((0, 8)),
        G,
        false,
        at,
        cell(&[((0, 7), 'a'), ((0, 8), 'b')]),
    );
    assert!(predictor.predict_char_in_grid('d', (0, 8), G, (9, 6), at));
    assert_eq!(
        predictor.pending_bounds(),
        Some((1, 0, 2, 1)),
        "pending wrap remains a legitimate transit position"
    );

    let mut wrong = Vec::new();
    for cursor in [(1, 0), (0, 6)] {
        for reconciled in [false, true] {
            let (mut predictor, mut at) = proven(80, 0);
            assert!(predictor.predict_char_in_grid('c', (0, 2), G, (80, 6), at));
            assert_eq!(predictor.overlay(at).len(), 1);
            assert!(predictor.predict_backspace(at));
            at += MS(30);
            if reconciled {
                predictor.reconcile(Some(cursor), G, false, at, cell(&AB));
            }
            assert!(predictor.predict_char_in_grid('s', cursor, G, (80, 6), at));
            let bounds = predictor.pending_bounds();
            let visible = predictor.overlay(at).len();
            let expected = Some((
                usize::from(cursor.0),
                usize::from(cursor.1),
                usize::from(cursor.0) + 1,
                usize::from(cursor.1) + 1,
            ));
            if bounds != expected || visible != 0 {
                wrong.push((Some(cursor), reconciled, bounds, visible));
            }
        }
    }
    let (mut predictor, at) = proven(80, 0);
    assert!(predictor.predict_char_in_grid('c', (0, 2), G, (80, 6), at));
    assert!(predictor.predict_backspace(at));
    predictor.reconcile(None, G, false, at, cell(&AB));
    assert!(predictor.predict_char_in_grid('s', (0, 2), G, (80, 6), at));
    let visible = predictor.overlay(at).len();
    if visible != 0 {
        wrong.push((None, true, predictor.pending_bounds(), visible));
    }
    assert!(
        wrong.is_empty(),
        "PREDICT_CANCELLED_ANCHOR_SURVIVED_UNRELATED_CURSOR: {wrong:?}"
    );
}

#[test]
fn a_cancelled_glyph_anchors_the_next_on_its_cell() {
    let transit = |rule_on: bool| {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::CancelTransit);
        }
        let owed = p.owed_at;
        assert!(p.predict_char('c', (0, 2), G, 80, at));
        assert!(p.predict_backspace(at), "our own guess, cancelled");
        assert_eq!(p.owed_at, owed, "the cancelled pair owes nothing");
        let ahead = at + MS(30);
        p.reconcile(
            Some((0, 3)),
            G,
            false,
            ahead,
            cell(&after_ab(&[((0, 2), 'c')])),
        );
        assert!(p.predict_char('x', (0, 3), G, 80, ahead));
        assert!(p.predict_char('x', (0, 3), G, 80, ahead));
        let cells: Vec<(u16, u16)> = p.preds.iter().map(|g| (g.row, g.col)).collect();
        (p, ahead, cells)
    };
    let echoed = after_ab(&[((0, 2), 'x'), ((0, 3), 'x')]);

    let (mut p, ahead, cells) = transit(true);
    assert_eq!(cells, [(0, 2), (0, 3)], "anchored on the cancelled cell");
    let landed = ahead + MS(60);
    p.reconcile(Some((0, 4)), G, false, landed, cell(&echoed));
    assert!(p.idle(), "both echoed where they were armed");
    assert!(p.confirmed_epoch, "and their own echoes proved the line");

    let (mut p, ahead, cells) = transit(false);
    assert_eq!(cells, [(0, 3), (0, 4)], "rule off: anchored a cell right");
    let landed = ahead + MS(60);
    p.reconcile(Some((0, 4)), G, false, landed, cell(&echoed));
    assert_eq!(
        p.preds.len(),
        1,
        "the first `x` retired by the second's echo"
    );
    assert!(
        p.confirmed_epoch,
        "rule off: credited with the second `x`'s echo"
    );
    assert_eq!(p.overlay(landed).len(), 1, "rule off: painted at (0, 4)");
}

#[test]
fn a_cancelled_glyph_at_the_margin_does_not_wrap_the_next() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::CancelTransit);
        }
        let slow = MS(60);
        let mut at = echo_turn(&mut p, 'a', 6, t0(), slow);
        at = echo_turn(&mut p, 'b', 7, at, slow);
        let grid = (9, 6);
        assert!(
            p.predict_char_in_grid('c', (0, 8), G, grid, at),
            "the last column"
        );
        assert_eq!(p.overlay(at).len(), 1, "control: the proven line paints c");
        assert!(p.predict_backspace(at));
        at += MS(30);
        p.reconcile(
            Some((0, 8)),
            G,
            false,
            at,
            cell(&[((0, 6), 'a'), ((0, 7), 'b'), ((0, 8), 'c')]),
        );
        assert!(p.deferred_wrap, "fixture: c's echo parked the terminal");
        assert!(p.predict_char_in_grid('d', (0, 8), G, grid, at));
        let cell = p.preds.front().map(|g| (g.row, g.col));
        assert_eq!(
            cell,
            Some(if rule_on { (0, 8) } else { (1, 0) }),
            "rule on {rule_on}"
        );
    }
}

#[test]
fn an_opaque_cell_is_its_glyph_to_the_glyph_lane() {
    assert_eq!(Seen::of_cell(' ', false), Seen::Blank);
    assert_eq!(Seen::of_cell(' ', true), Seen::Opaque(None));
    assert_eq!(Seen::of_cell('日', true).legacy(), Some('日'));
    let screens: [&[((u16, u16), char)]; 3] = [
        &[((0, 0), 'a')],
        &[((0, 0), 'a'), ((0, 1), 'x')],
        &[((0, 0), 'a'), ((0, 1), 'b'), ((0, 2), '日')],
    ];
    let mut head = Predictor::new(PredictMode::Always);
    for ch in ['a', 'b', 'c'] {
        assert!(head.predict_char(ch, (0, 0), G, 4, t0()));
    }
    let first = cell(screens[0]);
    head.reconcile_seen(Some((0, 1)), G, false, t0(), |r, c| {
        first(r, c).map_or(Seen::Opaque(None), |ch| Seen::Opaque(Some(ch)))
    });
    assert!(head.idle(), "`b` on an opaque blank: flushed");
    for screen in screens {
        let mut plain = Predictor::new(PredictMode::Always);
        let mut opaque = Predictor::new(PredictMode::Always);
        opaque.disable_rule_for_test(Rule::OpaqueHead);
        for p in [&mut plain, &mut opaque] {
            for ch in ['a', 'b', 'c'] {
                assert!(p.predict_char(ch, (0, 0), G, 4, t0()));
            }
        }
        let at = t0();
        plain.reconcile(Some((0, 1)), G, false, at, cell(screen));
        let marked = cell(screen);
        opaque.reconcile_seen(Some((0, 1)), G, false, at, |r, c| match marked(r, c) {
            Some(ch) => Seen::Opaque(Some(ch)),
            None => Seen::Opaque(None),
        });
        let pending = |p: &Predictor| {
            p.preds
                .iter()
                .map(|g| (g.row, g.col, g.ch))
                .collect::<Vec<_>>()
        };
        assert_eq!(pending(&plain), pending(&opaque), "{screen:?}");
        assert_eq!(plain.deferred_wrap, opaque.deferred_wrap, "{screen:?}");
    }
}

const RTT: Duration = Duration::from_millis(60);

fn sample(row: &[Seen], cursor: u16) -> KeySample<'_> {
    KeySample {
        cursor: (0, cursor),
        coords: G,
        grid: (12, 6),
        row: Some(row),
        ..KeySample::default()
    }
}

fn end_of(screen: &str) -> u16 {
    u16::try_from(screen.chars().count()).unwrap_or(u16::MAX)
}

fn type_after(p: &mut Predictor, screen: &str, text: &str, at: Instant) -> (String, Instant) {
    let mut screen = screen.to_owned();
    let mut at = at;
    for ch in text.chars() {
        let col = end_of(&screen);
        let row = seen_row(&screen);
        assert!(p.predict_glyph(ch, &sample(&row, col), at));
        at += RTT;
        screen.push(ch);
        p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&screen));
    }
    (screen, at)
}

fn edited_line(mode: PredictMode, text: &str) -> (Predictor, String, Instant) {
    let mut p = Predictor::new(mode);
    let (screen, at) = type_after(&mut p, "$ ", text, t0());
    assert!(
        p.adaptive_slow && p.confirmed_epoch,
        "fixture: a slow, proven line"
    );
    let hi = end_of(&screen);
    assert_eq!(
        p.edit_line_for_test(),
        Some((0, 2, hi, hi)),
        "fixture: the known line"
    );
    (p, screen, at)
}

fn press(p: &mut Predictor, key: EditKey, screen: &str, cursor: u16, at: Instant) -> EditOutcome {
    let row = seen_row(screen);
    p.predict_edit(key, &sample(&row, cursor), at)
}

type Shown = (Vec<(u16, char)>, Option<(u16, u16)>);

fn frame(p: &mut Predictor, screen: &str, cursor: u16, at: Instant) -> Shown {
    p.reconcile_seen(Some((0, cursor)), G, false, at, seen_cells(screen));
    let cells = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
    (cells, p.overlay_cursor())
}

#[test]
fn a_first_motion_stays_dark_until_its_echo_proves_the_class() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    assert_eq!(
        press(&mut p, EditKey::Left, &screen, 4, at),
        EditOutcome::Armed
    );
    assert_eq!(
        frame(&mut p, &screen, 4, at),
        (vec![], None),
        "the motion class is unproven: nothing shows"
    );
    let at = at + RTT;
    let _ = frame(&mut p, &screen, 3, at);
    assert!(p.idle(), "the state the screen shows retires the motion");
    assert_eq!(p.events_for_test().motions_proven, 1);
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 4, 3)));
    assert_eq!(p.proof_cell, Some((0, 3)), "the proof follows the cursor");
    assert_eq!(
        press(&mut p, EditKey::Left, &screen, 3, at),
        EditOutcome::Armed
    );
    assert_eq!(
        frame(&mut p, &screen, 3, at),
        (vec![], Some((0, 2))),
        "the second `←` shows at once: a cursor and no cell"
    );

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    p.disable_rule_for_test(Rule::MotionClassProof);
    let _ = press(&mut p, EditKey::Left, &screen, 4, at);
    assert_eq!(frame(&mut p, &screen, 4, at).1, Some((0, 3)), "rule off");
}

#[test]
fn a_burst_the_screen_cannot_tell_apart_retires_at_the_smallest_state() {
    for off in [
        None,
        Some(Rule::SmallestMatch),
        Some(Rule::NetNullUncredited),
    ] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if let Some(rule) = off {
            p.disable_rule_for_test(rule);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let _ = press(&mut p, EditKey::Right, &screen, 4, at);
        let net_null = p.events_for_test().net_null_retired;
        let _ = p.take_credited_for_test();
        let read = at + MS(5);
        p.reconcile_seen(Some((0, 4)), G, false, read, seen_cells(&screen));
        let credited = p.take_credited_for_test();
        assert!(p.idle(), "{off:?}");
        if off.is_none() {
            assert!(credited.is_empty(), "nothing was read: nothing credited");
            assert_eq!(p.events_for_test().net_null_retired, net_null + 1);
            assert_eq!(p.owed_at, Some(at), "both keys owed");
            assert!(!p.confirmed_epoch, "an owe beside a proof is no proof");
        } else {
            assert_eq!(credited.len(), 2, "{off:?}: credited before any echo");
        }
    }
}

#[test]
fn an_erase_at_the_end_paints_its_vacated_cell_blank() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
    assert_eq!(
        press(&mut p, EditKey::Backspace, &screen, 5, at),
        EditOutcome::Armed
    );
    assert_eq!(
        frame(&mut p, &screen, 5, at),
        (vec![(4, ' ')], Some((0, 4)))
    );
    let at = at + RTT;
    let _ = frame(&mut p, "$ ab", 4, at);
    assert!(p.idle());
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 4, 4)));
    let row = seen_row("$ ab");
    assert!(p.predict_glyph('x', &sample(&row, 4), at));
    assert_eq!(p.overlay(at).len(), 1, "the append after it shows");
}

#[test]
fn a_split_erase_echo_matches_no_state_early() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        if !rule_on {
            p.disable_rule_for_test(Rule::VacatedBlankInMatch);
        }
        for _ in 0..2 {
            assert_eq!(
                press(&mut p, EditKey::Backspace, &screen, 5, at),
                EditOutcome::Armed,
                "the second erase: `c` on screen is the chain's own text"
            );
        }
        let waits = p.events_for_test().transit_waits;
        p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ ab"));
        if rule_on {
            assert_eq!(p.preds.len(), 2, "no state matches: it waits");
            assert_eq!(p.events_for_test().transit_waits, waits + 1);
            p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ a"));
            assert!(p.idle(), "the second piece lands: `S_2`");
        } else {
            assert!(p.idle(), "rule off: matched with `b` still shown");
        }
    }
}

#[test]
fn an_erase_in_transit_refuses_new_text_past_every_state() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    assert!(
        p.proof_line_end,
        "fixture: the original append proved the end"
    );
    assert_eq!(
        press(&mut p, EditKey::Backspace, &screen, 4, at),
        EditOutcome::Armed
    );
    assert_eq!(
        frame(&mut p, &screen, 4, at),
        (vec![(3, ' ')], Some((0, 3))),
        "ordinary pending erase is visible on its glyph proof"
    );
    let waits = p.events_for_test().transit_waits;
    assert_eq!(
        frame(&mut p, "$ ab", 3, at + RTT),
        (vec![(3, ' ')], Some((0, 3))),
        "the cursor moved first; its own old b is allowed in transit"
    );
    assert!(!p.idle());
    assert_eq!(p.events_for_test().transit_waits, waits + 1);
    assert_eq!(
        frame(&mut p, "$ a", 3, at + RTT + RTT),
        (vec![], None),
        "the cell clears and the ordinary erase retires"
    );
    assert!(p.idle());
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 3, 3)));

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    assert!(p.proof_line_end, "fixture: standing-tail bypass is present");
    assert_eq!(
        press(&mut p, EditKey::Backspace, &screen, 4, at),
        EditOutcome::Armed
    );
    assert_eq!(
        frame(&mut p, &screen, 4, at),
        (vec![(3, ' ')], Some((0, 3)))
    );
    let shown = frame(&mut p, "$ abX", 3, at + RTT);
    assert!(
        p.idle() && !p.confirmed_epoch && shown == (vec![], None),
        "EDIT_ERASE_TRANSIT_ACCEPTED_NEW_TAIL: idle={} proof={} shown={shown:?}",
        p.idle(),
        p.confirmed_epoch
    );
}

#[test]
fn an_arrow_that_cannot_be_placed_keeps_the_guesses_in_flight() {
    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    assert_eq!(p.edit_line_for_test(), Some((0, 0, 2, 2)), "a known line");
    assert!(p.predict_char('n', (0, 2), G, 12, at));
    assert!(p.predict_char('o', (0, 2), G, 12, at));
    assert_eq!(p.overlay(at).len(), 2);
    let decline = p.events_for_test().decline_motions;
    assert_eq!(
        press(&mut p, EditKey::Left, "ab", 2, at),
        EditOutcome::Declined
    );
    assert_eq!(p.events_for_test().decline_motions, decline + 1);
    assert_eq!(p.overlay(at).len(), 2, "still on glass, carried");
    assert!(p.anchor_lost && p.owed_at == Some(at), "owed, anchor lost");
    assert!(!p.predict_char('x', (0, 2), G, 12, at), "nothing behind it");

    let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
    p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("ab"));
    assert_eq!(
        press(&mut p, EditKey::Left, "ab", 2, at),
        EditOutcome::Armed
    );
}

#[test]
fn an_arrow_at_an_edge_of_the_text_is_declined() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::TextEdges);
        }
        let right = press(&mut p, EditKey::Right, &screen, 4, at);
        assert_eq!(right == EditOutcome::Armed, !rule_on, "→ at the end");

        let (mut p, screen, mut at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::TextEdges);
        }
        for cursor in [4, 3] {
            assert_eq!(
                press(&mut p, EditKey::Left, &screen, cursor, at),
                EditOutcome::Armed
            );
            at += RTT;
            let _ = frame(&mut p, &screen, cursor - 1, at);
            assert!(p.idle());
        }
        let left = press(&mut p, EditKey::Left, &screen, 2, at);
        assert_eq!(left == EditOutcome::Armed, !rule_on, "← at lo");
    }
}

#[test]
fn editing_off_routes_edit_keys_exactly_as_before() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    let row = seen_row(&screen);
    let mut q = Predictor::new(PredictMode::Adaptive);
    let _ = type_after(&mut q, "$ ", "ab", t0());
    p.set_editing(false);
    q.flush();
    assert!(p.predict_glyph('c', &sample(&row, 4), at));
    assert!(q.predict_glyph('c', &sample(&row, 4), at));
    assert_eq!(
        press(&mut p, EditKey::Backspace, &screen, 4, at),
        EditOutcome::Cancelled
    );
    assert!(q.predict_backspace(at));
    assert_eq!(
        press(&mut p, EditKey::Left, &screen, 4, at),
        EditOutcome::Declined
    );
    q.note_unmodelled_input(at);
    let state = |p: &Predictor| {
        (
            p.preds.len(),
            p.owed_at,
            p.epoch,
            p.confirmed_epoch,
            p.glyph_owes.len(),
        )
    };
    assert_eq!(state(&p), state(&q));
    assert_eq!(
        press(&mut p, EditKey::Backspace, &screen, 4, at),
        EditOutcome::Declined
    );
    assert!(!q.predict_backspace(at));
    assert_eq!(state(&p), state(&q));
}

#[test]
fn a_glyph_typed_inside_the_text_is_not_an_append() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
    let mut at = at;
    for cursor in [5, 4] {
        let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
        at += RTT;
        let _ = frame(&mut p, &screen, cursor - 1, at);
    }
    assert!(p.idle() && p.confirmed_epoch);
    let row = seen_row(&screen);
    assert!(p.predict_glyph('x', &sample(&row, 3), at));
    assert!(p.engaged(), "a shift: the first op of a chain");
    assert_eq!(frame(&mut p, &screen, 3, at), (vec![], None), "dark");
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 5, 3)), "still known");
    let at = at + RTT;
    let _ = frame(&mut p, "$ axbc", 4, at);
    assert!(p.idle(), "its echo, the tail shifted, retires it");
    assert_eq!(p.events_for_test().shifts_proven, 1);
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 6, 4)));
    let row = seen_row("$ axbc");
    assert!(p.predict_glyph('y', &sample(&row, 4), at));
    let _ = frame(&mut p, "$ axbc", 4, at);
    let ghosts: Vec<(u16, char, GhostCell)> = p
        .overlay(at)
        .iter()
        .map(|g| (g.col, g.ch, g.ghost()))
        .collect();
    assert_eq!(
        ghosts,
        [
            (4, 'y', GhostCell::Typed),
            (5, 'b', GhostCell::Moved { from: 4 }),
            (6, 'c', GhostCell::Moved { from: 5 })
        ],
        "the next shows at once, `bc` moved"
    );
    assert_eq!(p.overlay_cursor(), Some((0, 5)));

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    let _ = press(&mut p, EditKey::Left, &screen, 4, at);
    let at = at + RTT;
    let _ = frame(&mut p, &screen, 3, at);
    let mut row = seen_row(&screen);
    row[4] = Seen::Opaque(None);
    let refusals = p.events_for_test().opaque_refusals;
    assert!(p.predict_glyph('x', &sample(&row, 3), at));
    assert!(!p.engaged());
    assert_eq!(p.events_for_test().opaque_refusals, refusals + 1);
    assert!(p.overlay(at).is_empty(), "not painted over `b`");
    assert_eq!(p.edit_line_for_test(), None);
    let _ = frame(&mut p, "$ axb", 4, at + RTT);
    assert!(p.idle(), "its echo retires it");
    assert!(
        !p.confirmed_epoch,
        "a run typed inside the text proves nothing"
    );
}

#[test]
fn an_opaque_cell_on_the_chain_s_line_flushes_it() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    let _ = press(&mut p, EditKey::Left, &screen, 4, at);
    let mut plain = seen_cells(&screen);
    p.reconcile_seen(Some((0, 3)), G, false, at + RTT, |r, c| {
        if (r, c) == (0, 4) {
            Seen::Opaque(Some('日'))
        } else {
            plain(r, c)
        }
    });
    assert!(p.idle() && !p.confirmed_epoch, "flushed");

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    let mut row = seen_row(&screen);
    row[4] = Seen::Opaque(None);
    assert_eq!(
        p.predict_edit(EditKey::Left, &sample(&row, 4), at),
        EditOutcome::Declined
    );
}

#[test]
fn an_erase_beside_text_past_the_end_is_not_armed() {
    for rule_on in [true, false] {
        let (mut p, _, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::VacateRefusal);
        }
        let refusals = p.events_for_test().vacate_refusals;
        let out = press(&mut p, EditKey::Backspace, "$ abcd", 4, at);
        assert_eq!(out == EditOutcome::Armed, !rule_on, "rule on {rule_on}");
        assert_eq!(
            p.events_for_test().vacate_refusals,
            refusals + u64::from(rule_on)
        );
    }
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    let _ = press(&mut p, EditKey::Left, &screen, 4, at);
    let _ = press(&mut p, EditKey::Right, &screen, 4, at);
    let _ = press(&mut p, EditKey::Backspace, &screen, 4, at);
    p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ abcd"));
    assert!(p.idle(), "a suggestion appeared beside a pending erase");
}

#[test]
fn an_expired_chain_head_takes_the_chain_with_it() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::StateExpiryFlushes);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let _ = press(&mut p, EditKey::Right, &screen, 4, at + MS(100));
        let late = at + p.glitch_window() + MS(1);
        let _ = p.overlay(late);
        assert_eq!(p.idle(), rule_on, "rule on {rule_on}");
        if rule_on {
            assert_eq!(p.owed_at, Some(at + MS(100)));
            assert!(!p.confirmed_epoch);
        }
    }
}

#[test]
fn a_chain_carried_across_enter_keeps_its_cursor() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::CarryCursor);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        let _ = press(&mut p, EditKey::Left, &screen, 3, at);
        assert_eq!(frame(&mut p, &screen, 3, at).1, Some((0, 2)));
        p.note_line_submit(at);
        let carried = frame(&mut p, &screen, 3, at).1;
        assert_eq!(carried, rule_on.then_some((0, 2)), "rule on {rule_on}");
    }
}

#[test]
fn a_motion_proof_does_not_outlive_its_line() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::ProofsEndWithTheEpoch);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        assert_eq!(p.events_for_test().motions_proven, 1);
        p.note_line_submit(at);
        let (screen, at) = type_after(&mut p, "", "cd", at + MS(300));
        assert!(p.confirmed_epoch, "the new line proves");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 2, at),
            EditOutcome::Armed
        );
        let shown = frame(&mut p, &screen, 2, at).1;
        assert_eq!(shown.is_some(), !rule_on, "rule on {rule_on}");
    }
}

#[test]
fn a_chain_matches_whole_states_not_its_cursor() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::WholeStateMatch);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ xy"));
        assert!(p.idle());
        let line = p.edit_line_for_test();
        assert_eq!(line.is_some(), !rule_on, "rule on {rule_on}: {line:?}");
    }
}

#[test]
fn a_motion_echo_never_proves_its_epoch() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut screen = String::from("$       <r>");
        let mut at = t0();
        for (i, ch) in ['a', 'b'].into_iter().enumerate() {
            let col = 2 + u16::try_from(i).unwrap_or(0);
            let row = seen_row(&screen);
            assert!(p.predict_glyph(ch, &sample(&row, col), at));
            at += RTT;
            screen.replace_range(usize::from(col)..=usize::from(col), &ch.to_string());
            p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&screen));
        }
        assert!(
            p.confirmed_epoch && !p.proof_line_end,
            "fixture: a decorated proof"
        );
        if !rule_on {
            p.disable_rule_for_test(Rule::MotionsNeverProve);
        }
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 4, at),
            EditOutcome::Armed
        );
        p.reconcile_seen(Some((0, 4)), G, false, at, seen_cells("$ ab"));
        assert!(
            p.confirmed_epoch && p.preds.len() == 1,
            "a right prompt cleared beside the line moves nothing"
        );
        p.void_proof();
        assert!(
            !p.confirmed_epoch && p.preds.len() == 1,
            "voided, ← in flight"
        );
        p.reconcile_seen(Some((0, 3)), G, false, at + RTT, seen_cells("$ ab"));
        assert!(p.idle());
        assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
    }
}

#[test]
fn an_edit_op_dropped_is_owed() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::EditOpsOwe);
        }
        let before = p.owed_at;
        let _ = press(&mut p, EditKey::Right, &screen, 4, at + MS(5));
        assert_eq!(p.owed_at == Some(at + MS(5)), rule_on, "the declined →");
        assert_eq!(p.owed_at == before, !rule_on);

        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::EditOpsOwe);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at + MS(5));
        p.reconcile_seen(Some((1, 0)), G, false, at + MS(10), seen_cells(&screen));
        assert!(p.idle(), "the cursor left the line: flushed");
        assert_eq!(p.owed_at == Some(at + MS(5)), rule_on, "the flushed ←");
    }
}

#[test]
fn pending_bounds_cover_the_chain_s_line() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
    let _ = press(&mut p, EditKey::Backspace, &screen, 5, at);
    assert_eq!(p.pending_bounds(), Some((0, 2, 1, 6)));
}

#[test]
fn a_chain_is_capped() {
    let (mut p, screen, at) = edited_line(PredictMode::Always, "ab");
    let mut armed = 0;
    for i in 0..EDIT_CHAIN_MAX + 2 {
        let key = if i % 2 == 0 {
            EditKey::Left
        } else {
            EditKey::Right
        };
        if press(&mut p, key, &screen, 4, at) == EditOutcome::Armed {
            armed += 1;
        }
    }
    assert_eq!(armed, EDIT_CHAIN_MAX);
    assert!(p.anchor_lost, "the key past the cap was declined");
}

#[test]
fn the_known_line_lives_with_the_proof_it_came_from() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let (screen, at) = type_after(&mut p, "$ ", "a", t0());
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 3, 3)), "started");
    let (screen, at) = type_after(&mut p, &screen, "bcdefgh", at);
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 10, 10)), "extended");
    let (screen, at) = type_after(&mut p, &screen, "i", at);
    assert_eq!(
        p.edit_line_for_test(),
        Some((0, 2, 11, 11)),
        "up to column 10"
    );
    let _ = type_after(&mut p, &screen, "j", at);
    assert_eq!(p.edit_line_for_test(), None, "a glyph in the last column");

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    let row = seen_row(&screen);
    assert!(
        p.predict_glyph('c', &sample(&row, 7), at),
        "the cursor moved"
    );
    assert_eq!(p.edit_line_for_test(), None);

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    let _ = press(&mut p, EditKey::Left, &screen, 4, at);
    p.note_unmodelled_input(at);
    assert_eq!(p.edit_line_for_test(), None, "a flush");

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    p.note_line_submit(at);
    assert_eq!(
        p.edit_line_for_test(),
        None,
        "a terminator, nothing carried"
    );
    let _ = screen;
}

#[test]
fn every_key_is_held_until_its_echo_is_credited() {
    let (mut p, screen, mut at) = edited_line(PredictMode::Adaptive, "ab");
    let mut presses: Vec<(u64, Instant)> = Vec::new();
    let check = |p: &mut Predictor, presses: &mut Vec<(u64, Instant)>, label: &str| {
        let credited = p.take_credited_for_test();
        presses.retain(|(seq, _)| !credited.contains(seq));
        for &(seq, t) in presses.iter() {
            let held = p.preds.iter().any(|g| g.seq == seq)
                || p.glyph_owes.iter().any(|&(s, _, _)| s == seq)
                || p.owed_at.is_some_and(|o| o >= t);
            assert!(held, "{label}: key {seq} typed at {t:?} is held by nothing");
        }
    };
    let _ = p.take_credited_for_test();
    let key = |p: &mut Predictor,
               k: EditKey,
               cursor: u16,
               at: Instant,
               presses: &mut Vec<(u64, Instant)>| {
        let seq = p.next_seq;
        let _ = press(p, k, &screen, cursor, at);
        presses.push((seq, at));
    };
    key(&mut p, EditKey::Left, 4, at, &mut presses);
    key(&mut p, EditKey::Right, 4, at + MS(1), &mut presses);
    check(&mut p, &mut presses, "a chain armed");
    at += MS(5);
    p.reconcile_seen(Some((0, 4)), G, false, at, seen_cells(&screen));
    check(&mut p, &mut presses, "a net-null burst");
    let row = seen_row(&screen);
    let seq = p.next_seq;
    assert!(p.predict_glyph('c', &sample(&row, 4), at));
    presses.push((seq, at));
    assert!(p.predict_backspace(at + MS(1)));
    check(&mut p, &mut presses, "a cancelled glyph");
    key(&mut p, EditKey::Right, 4, at + MS(2), &mut presses);
    check(&mut p, &mut presses, "a declined arrow");
    at += MS(5);
    p.reconcile_seen(Some((3, 0)), G, false, at, seen_cells(&screen));
    check(&mut p, &mut presses, "a divergence");
}

#[test]
fn a_stale_anchor_is_not_credited_by_the_next_key_s_echo() {
    for rule_on in [true, false] {
        let (mut p, at) = proven_slow_line(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::StaleAnchor);
        }
        assert!(!p.predict_backspace(at), "nothing of ours: owed");
        let typed = at + MS(140);
        assert!(p.predict_char('c', (0, 2), G, 12, typed));
        assert!(p.predict_char('c', (0, 2), G, 12, typed));
        assert!(p.preds.iter().all(|g| g.stale), "a stale anchor");
        let _ = p.take_credited_for_test();
        let landed = at + MS(300);
        let screen = [((0, 0), 'a'), ((0, 1), 'c'), ((0, 2), 'c')];
        p.reconcile(Some((0, 3)), G, false, landed, cell(&screen));
        let credited = p.take_credited_for_test();
        assert_eq!(
            credited.is_empty(),
            rule_on,
            "rule on {rule_on}: {credited:?}"
        );
        assert_eq!(p.confirmed_epoch, !rule_on);
    }
}

#[test]
fn an_edit_chain_does_not_saturate_its_last_column_guard() {
    fn append_after_a_motion_pair(cols: u16, first: u16) -> (bool, Option<(u16, u16)>) {
        let mut predictor = Predictor::new(PredictMode::Always);
        let born = t0();
        let mut row = vec![Seen::Blank; usize::from(cols)];
        fn key(cells: &[Seen], col: u16, cols: u16) -> KeySample<'_> {
            KeySample {
                cursor: (0, col),
                coords: G,
                grid: (cols, 2),
                row: Some(cells),
                ..KeySample::default()
            }
        }
        assert!(predictor.predict_glyph('a', &key(&row, first, cols), born));
        row[usize::from(first)] = Seen::Glyph('a');
        let now = born + RTT;
        let cursor = first + 1;
        predictor.reconcile_seen(Some((0, cursor)), G, false, now, |_, col| {
            row.get(usize::from(col)).copied().unwrap_or(Seen::Blank)
        });
        assert_eq!(
            predictor.edit_line_for_test(),
            Some((0, first, cursor, cursor))
        );
        let sampled = key(&row, cursor, cols);
        assert_eq!(
            predictor.predict_edit(EditKey::Left, &sampled, now),
            EditOutcome::Armed
        );
        assert_eq!(
            predictor.predict_edit(EditKey::Right, &sampled, now),
            EditOutcome::Armed
        );
        let armed = predictor.predict_glyph('b', &sampled, now);
        let _ = predictor.overlay(now);
        (armed, predictor.overlay_cursor())
    }

    assert_eq!(append_after_a_motion_pair(12, 2), (true, Some((0, 4))));
    for cols in [12, u16::MAX - 1] {
        let (armed, cursor) = append_after_a_motion_pair(cols, cols - 2);
        assert!(!armed, "ordinary last-column refusal at width {cols}");
        assert!(cursor.is_none_or(|(_, col)| col < cols));
    }
    let widest = append_after_a_motion_pair(u16::MAX, u16::MAX - 2);
    assert!(
        !widest.0 && widest.1.is_none_or(|(_, col)| col < u16::MAX),
        "EDIT_CHAIN_LAST_COLUMN_SATURATED: {widest:?}"
    );
}

#[test]
fn compute_glitch_window_ms_is_floored_backed_off_and_capped() {
    assert_eq!(compute_glitch_window_ms(None, 0), 250);
    assert_eq!(compute_glitch_window_ms(None, 2), 1000);
    assert_eq!(compute_glitch_window_ms(None, 9), 2000);
    assert_eq!(compute_glitch_window_ms(Some(120_000), 0), 360);
    assert_eq!(compute_glitch_window_ms(Some(900_000), 0), 2000);
}

#[test]
fn a_chain_composed_before_any_reconcile_paints_only_what_it_changes() {
    let (mut p, screen, at) = edited_line(PredictMode::Always, "abc");
    assert_eq!(
        press(&mut p, EditKey::Left, &screen, 5, at),
        EditOutcome::Armed
    );
    assert!(p.overlay(at).is_empty(), "a motion changes no cell");
    assert_eq!(p.overlay_cursor(), Some((0, 4)));
    let (mut p, screen, at) = edited_line(PredictMode::Always, "abc");
    assert_eq!(
        press(&mut p, EditKey::Backspace, &screen, 5, at),
        EditOutcome::Armed
    );
    let cells: Vec<(u16, char)> = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
    assert_eq!(cells, [(4, ' ')], "only the vacated cell");
}

#[test]
fn overlay_frame_is_the_overlay_and_its_cursor() {
    let (mut p, screen, at) = edited_line(PredictMode::Always, "ab");
    let row = seen_row(&screen);
    assert!(p.predict_glyph('c', &sample(&row, 4), at));
    let glyph_lane: Vec<(u16, char)> = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
    let (cells, cursor) = p.overlay_frame(at);
    let framed: Vec<(u16, char)> = cells.iter().map(|g| (g.col, g.ch)).collect();
    assert_eq!((framed, cursor), (glyph_lane, None));
    assert_eq!(
        press(&mut p, EditKey::Left, &screen, 4, at),
        EditOutcome::Armed
    );
    let chain: Vec<(u16, char)> = p.overlay(at).iter().map(|g| (g.col, g.ch)).collect();
    let chain_cursor = p.overlay_cursor();
    let (cells, cursor) = p.overlay_frame(at);
    let framed: Vec<(u16, char)> = cells.iter().map(|g| (g.col, g.ch)).collect();
    assert_eq!((framed, cursor), (chain, chain_cursor));
    assert_eq!(cursor, Some((0, 4)), "the chain's cursor on `c`");
}

#[test]
fn newest_op_and_shown_through_name_the_pressed_and_the_displayed_ops() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
    let row = seen_row(&screen);
    assert_eq!((p.newest_op(), p.shown_through()), (None, None));
    assert!(p.predict_glyph('c', &sample(&row, 4), at));
    let c = p.newest_op().expect("`c` armed an op");
    assert_eq!(p.armed_since(None), Some(c));
    let _ = p.overlay(at);
    assert_eq!(p.shown_through(), Some(c), "the proven line shows `c`");
    assert_eq!(
        press(&mut p, EditKey::Left, &screen, 5, at),
        EditOutcome::Armed
    );
    let left = p.newest_op().expect("`←` armed an op");
    assert!(left > c, "ops are numbered in press order");
    assert_eq!(p.armed_since(Some(c)), Some(left));
    let _ = p.overlay(at);
    assert_eq!(
        p.shown_through(),
        Some(c),
        "the line's first `←` is dark until a motion of it echoes (THE CLASS LADDER)"
    );
    let (mut q, screen, at) = edited_line(PredictMode::Always, "ab");
    let row = seen_row(&screen);
    assert!(q.predict_glyph('c', &sample(&row, 4), at));
    assert!(q.predict_glyph('d', &sample(&row, 5), at));
    let before = q.newest_op();
    assert_eq!(
        press(&mut q, EditKey::Backspace, &screen, 6, at),
        EditOutcome::Cancelled
    );
    assert!(
        q.newest_op() < before,
        "a cancel pops `d` and arms nothing newer"
    );
    assert_eq!(
        q.armed_since(before),
        None,
        "the cancel armed nothing (naming the newest op would hand `c`'s op to it)"
    );
}

#[test]
fn a_flush_takes_the_chain_s_cursor_with_it() {
    let (mut p, screen, at) = edited_line(PredictMode::Always, "abc");
    assert_eq!(
        press(&mut p, EditKey::Left, &screen, 5, at),
        EditOutcome::Armed
    );
    let _ = p.overlay(at);
    assert_eq!(
        p.overlay_cursor(),
        Some((0, 4)),
        "control: the chain's cursor"
    );
    p.reset();
    assert_eq!(p.overlay_cursor(), None);
    assert!(p.idle());
}

fn decorated_line(text: &str) -> (Predictor, String, Instant) {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let mut screen = String::from("$       <r>");
    let mut at = t0();
    for (i, ch) in text.chars().enumerate() {
        let col = 2 + u16::try_from(i).unwrap_or(0);
        let row = seen_row(&screen);
        assert!(p.predict_glyph(ch, &sample(&row, col), at));
        at += RTT;
        screen.replace_range(usize::from(col)..=usize::from(col), &ch.to_string());
        p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&screen));
    }
    assert!(
        p.adaptive_slow && p.confirmed_epoch && !p.proof_line_end,
        "fixture: a slow line proven beside its right prompt"
    );
    (p, screen, at)
}

#[test]
fn an_erase_inside_the_text_shifts_its_tail() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        if !rule_on {
            p.disable_rule_for_test(Rule::EraseShiftProof);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 5, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 4, at);
        assert!(p.idle() && p.confirmed_epoch, "`←` echoed: inside the text");
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 4, at),
            EditOutcome::Armed
        );
        let shown = frame(&mut p, &screen, 4, at);
        if !rule_on {
            assert_eq!(shown, (vec![(3, 'c'), (4, ' ')], Some((0, 3))), "rule off");
            continue;
        }
        assert_eq!(shown, (vec![], None), "dark: no mid-line erase has echoed");
        let at = at + RTT;
        let _ = frame(&mut p, "$ ac", 3, at);
        assert!(p.idle(), "its echo retires it");
        assert_eq!(p.events_for_test().shifts_proven, 1);
        assert_eq!(p.edit_line_for_test(), Some((0, 2, 4, 3)));
        assert_eq!(
            press(&mut p, EditKey::Backspace, "$ ac", 3, at),
            EditOutcome::Armed
        );
        let _ = frame(&mut p, "$ ac", 3, at);
        let ghosts: Vec<(u16, char, GhostCell)> = p
            .overlay(at)
            .iter()
            .map(|g| (g.col, g.ch, g.ghost()))
            .collect();
        assert_eq!(
            ghosts,
            [
                (2, 'c', GhostCell::Moved { from: 3 }),
                (3, ' ', GhostCell::Vacated)
            ],
            "the next shows at once"
        );
        assert_eq!(p.overlay_cursor(), Some((0, 2)));
    }
}

#[test]
fn a_first_shift_stays_dark_until_its_echo_proves_the_class() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::InsertShiftProof);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        let row = seen_row(&screen);
        assert!(p.predict_glyph('x', &sample(&row, 3), at));
        let shown = frame(&mut p, &screen, 3, at);
        let want = if rule_on {
            (vec![], None)
        } else {
            (vec![(3, 'x'), (4, 'b')], Some((0, 4)))
        };
        assert_eq!(shown, want, "rule on {rule_on}");
    }
}

#[test]
fn a_shift_of_spaces_proves_nothing() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "a  ");
        if !rule_on {
            p.disable_rule_for_test(Rule::ShiftProofDistinguishes);
        }
        let mut at = at;
        for cursor in [5, 4] {
            let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
            at += RTT;
            let _ = frame(&mut p, &screen, cursor - 1, at);
            assert!(p.idle());
        }
        let row = seen_row(&screen);
        assert!(p.predict_glyph(' ', &sample(&row, 3), at));
        at += RTT;
        let _ = p.take_credited_for_test();
        let _ = frame(&mut p, &screen, 4, at);
        assert!(p.idle(), "the screen shows its state");
        assert_eq!(p.take_credited_for_test().len(), 1, "credited");
        assert_eq!(
            p.events_for_test().shifts_proven,
            u64::from(!rule_on),
            "rule on {rule_on}"
        );
        assert_eq!(p.edit_line_for_test().is_some(), !rule_on);
        assert_eq!(p.confirmed_epoch, !rule_on);
        assert_eq!(p.events_for_test().ambiguous_shifts, u64::from(rule_on));
    }
}

#[test]
fn a_lone_glyph_moved_past_the_line_proves_nothing() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::ShiftProofDistinguishes);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        let row = seen_row(&screen);
        assert!(p.predict_glyph('x', &sample(&row, 3), at));
        let _ = frame(&mut p, "$ axb", 4, at + RTT);
        assert!(p.idle(), "rule on {rule_on}: retired");
        assert_eq!(p.events_for_test().shifts_proven, u64::from(!rule_on));
        assert_eq!(p.events_for_test().ambiguous_shifts, u64::from(rule_on));
        assert_eq!(
            p.edit_line_for_test(),
            (!rule_on).then_some((0, 2, 5, 4)),
            "rule on {rule_on}"
        );
    }

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
    for (key, cursor) in [(EditKey::Backspace, 5), (EditKey::Left, 4)] {
        assert_eq!(press(&mut p, key, &screen, cursor, at), EditOutcome::Armed);
    }
    let row = seen_row(&screen);
    assert!(p.predict_glyph('x', &sample(&row, 5), at));
    let _ = frame(&mut p, "$ axb", 4, at + RTT);
    assert!(p.idle(), "the chain's last state, matched whole");
    assert_eq!(p.events_for_test().shifts_proven, 1);
    assert_eq!(p.events_for_test().ambiguous_shifts, 0);
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 5, 4)));
}

#[test]
fn a_glyph_the_chain_cannot_place_inside_the_text_flushes_it() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Always, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::MidLineDeclineFlushes);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let typed = at + MS(5);
        assert!(!p.predict_char_in_grid('日', (0, 4), G, (12, 6), typed));
        assert_eq!(p.idle(), rule_on, "rule on {rule_on}");
        assert_eq!(p.owed_at, Some(typed));
        assert_eq!(p.events_for_test().mid_line_flushes, u64::from(rule_on));
    }
}

#[test]
fn a_shift_into_the_last_column_is_not_placed() {
    let (mut p, screen, at) = edited_line(PredictMode::Always, "abcdefghi");
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 11, 11)));
    let _ = press(&mut p, EditKey::Left, &screen, 11, at);
    let at = at + RTT;
    let _ = frame(&mut p, &screen, 10, at);
    let row = seen_row(&screen);
    assert!(p.predict_glyph('x', &sample(&row, 10), at));
    assert!(!p.engaged(), "the glyph lane's");
    assert_eq!(p.edit_line_for_test(), None);
}

#[test]
fn a_chain_beside_a_right_prompt_holds_the_row_past_it() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = decorated_line("ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::StandingTail);
        }
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 4, at),
            EditOutcome::Armed
        );
        p.reconcile_seen(Some((0, 4)), G, false, at, seen_cells("$ ab"));
        assert!(!p.idle(), "a right prompt cleared moves nothing");
        p.reconcile_seen(Some((0, 4)), G, false, at, seen_cells("$ ab cd"));
        assert_eq!(p.idle(), rule_on, "rule on {rule_on}");
    }
}

#[test]
fn a_credited_shift_re_bases_the_standing_proof() {
    let (mut p, screen, at) = decorated_line("abc");
    let mut at = at;
    for cursor in [5, 4] {
        let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
        at += RTT;
        let _ = frame(&mut p, &screen, cursor - 1, at);
    }
    assert!(p.idle() && p.confirmed_epoch);
    let row = seen_row(&screen);
    assert!(p.predict_glyph('x', &sample(&row, 3), at));
    let at = at + RTT;
    let _ = frame(&mut p, "$ axbc  <r>", 4, at);
    assert!(p.idle() && p.confirmed_epoch, "credited, proven");
    let _ = frame(&mut p, "$ axbc  <r>", 4, at);
    assert!(
        p.confirmed_epoch,
        "re-based: the shifted row is the snapshot"
    );
    assert_eq!(p.events_for_test().standing_voids, 0);
    let _ = frame(&mut p, "$ axbc z<r>", 4, at);
    assert!(!p.confirmed_epoch, "a glyph past the line voids it");
}

#[test]
fn an_edit_press_holds_the_standing_proof_to_its_row() {
    for rule_on in [true, false] {
        let (mut p, _, at) = decorated_line("ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::StandingTail);
        }
        let out = press(&mut p, EditKey::Left, "$ ab cd <r>", 4, at);
        assert_eq!(out == EditOutcome::Armed, !rule_on, "rule on {rule_on}");
    }
}

#[test]
fn a_burst_overwrite_mode_also_shows_proves_no_shift() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
        if !rule_on {
            p.disable_rule_for_test(Rule::ShiftProofDistinguishes);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 5, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 4, at);
        assert!(p.idle() && p.events_for_test().motions_proven == 1);
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 4, at),
            EditOutcome::Armed
        );
        let row = seen_row(&screen);
        assert!(p.predict_glyph('o', &sample(&row, 4), at));
        let at = at + RTT;
        assert_eq!(
            frame(&mut p, "$ a c", 3, at),
            (vec![], None),
            "the rubout's echo: no state, the chain waits, dark"
        );
        assert!(!p.idle());
        let at = at + MS(5);
        let _ = frame(&mut p, "$ aoc", 4, at);
        assert!(p.idle(), "`S_2`, the screen of both");
        assert_eq!(
            p.events_for_test().shifts_proven,
            if rule_on { 0 } else { 2 },
            "rule on {rule_on}"
        );
        assert_eq!(
            p.edit_line_for_test(),
            Some((0, 2, 5, 4)),
            "the line stands"
        );
        let row = seen_row("$ aoc");
        assert!(p.predict_glyph('n', &sample(&row, 4), at));
        let shown = frame(&mut p, "$ aoc", 4, at);
        assert_eq!(shown.0.is_empty(), rule_on, "rule on {rule_on}: {shown:?}");
    }
}

#[test]
fn a_screen_overwrite_mode_shows_with_fewer_keys_credits_none() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::InPlaceReadsFewer);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        assert!(p.idle());
        let row = seen_row(&screen);
        assert!(p.predict_glyph('x', &sample(&row, 3), at));
        assert_eq!(
            press(&mut p, EditKey::Right, &screen, 3, at),
            EditOutcome::Armed
        );
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 3, at),
            EditOutcome::Armed
        );
        let _ = p.take_credited_for_test();
        let _ = frame(&mut p, "$ ax", 4, at + RTT);
        let credited = p.take_credited_for_test();
        assert!(p.idle());
        assert_eq!(
            credited.len(),
            if rule_on { 0 } else { 3 },
            "rule on {rule_on}: {credited:?}"
        );
    }
}

#[test]
fn a_backspace_cancels_no_glyph_of_an_unproven_shift() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::CancelsOnlyKnownShifts);
            p.disable_rule_for_test(Rule::TransitBlanks);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 4, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 3, at);
        let row = seen_row(&screen);
        assert!(p.predict_glyph('n', &sample(&row, 3), at));
        let out = press(&mut p, EditKey::Backspace, &screen, 3, at);
        let want = if rule_on {
            EditOutcome::Armed
        } else {
            EditOutcome::Cancelled
        };
        assert_eq!(out, want, "rule on {rule_on}");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 3, at),
            EditOutcome::Armed
        );
        let shown = frame(&mut p, "$ a", 3, at + RTT);
        assert_eq!(
            shown == (vec![], None),
            rule_on,
            "rule on {rule_on}: {shown:?}"
        );
    }
}

#[test]
fn an_arrow_with_a_run_in_flight_holds_the_standing_tail() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let (screen, at) = type_after(&mut p, "$ ", "ab cd", t0());
        assert!(p.adaptive_slow && p.confirmed_epoch, "fixture");
        if !rule_on {
            p.disable_rule_for_test(Rule::StandingTail);
        }
        p.note_unmodelled_input(at);
        let at = at + MS(600);
        let _ = frame(&mut p, &screen, 4, at);
        let row = seen_row(&screen);
        assert!(p.predict_glyph('e', &sample(&row, 4), at));
        assert!(p.predict_glyph('c', &sample(&row, 4), at));
        let at = at + RTT;
        let _ = frame(&mut p, "$ abecd", 5, at);
        assert!(
            p.confirmed_epoch,
            "fixture: `e` proved on the frame between"
        );
        let voids = p.events_for_test().standing_voids;
        let out = press(&mut p, EditKey::Left, "$ abe cd", 5, at);
        let shown = frame(&mut p, "$ abe cd", 5, at);
        if rule_on {
            assert_eq!(out, EditOutcome::Declined);
            assert_eq!(p.events_for_test().standing_voids, voids + 1);
            assert_eq!(shown, (vec![], None));
        } else {
            assert!(shown.0.contains(&(5, 'c')), "rule off: {shown:?}");
        }
    }
}

#[test]
fn a_blank_row_behind_a_declined_arrow_is_no_line_end() {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let t = t0();
    let empty = seen_row("");
    assert!(p.predict_glyph('b', &sample(&empty, 0), t));
    assert_eq!(
        press(&mut p, EditKey::Left, "", 0, t + MS(10)),
        EditOutcome::Declined
    );
    assert!(!p.predict_glyph('d', &sample(&empty, 0), t + MS(20)));
    let at = t + MS(60);
    p.reconcile_seen(Some((0, 1)), G, false, at, seen_cells("b"));
    assert!(p.idle(), "`b` echoed");
    let row = seen_row("b");
    for ch in ['b', 'n', 's'] {
        assert!(p.predict_glyph(ch, &sample(&row, 1), at));
    }
    p.reconcile_seen(Some((0, 4)), G, false, at + MS(400), seen_cells("dbnsb"));
    assert!(p.idle(), "the run's echoes retire it");
    assert!(!p.confirmed_epoch, "a tail its press did not see: no proof");
    assert!(p.events_for_test().tail_refusals > 0);
}

#[test]
fn an_echo_drawn_over_the_glyph_it_shifts_hides_its_tail() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::CleanTailInFlight);
        }
        let (screen, at) = type_after(&mut p, "", "xi", t0());
        assert!(
            p.adaptive_slow && p.confirmed_epoch,
            "fixture: a slow, proven line"
        );
        let step_back = at;
        p.note_unmodelled_input(step_back);
        let at = step_back + RTT;
        let _ = frame(&mut p, &screen, 1, at);
        assert!(p.predict_glyph('i', &sample(&seen_row(&screen), 1), at));
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 2, at);
        assert!(p.idle(), "the glyph's echo retires it");
        assert!(!p.confirmed_epoch, "uncredited: Ctrl-B's echo is owed");
        assert_eq!(
            p.hidden_tail_at.is_some(),
            rule_on,
            "rule on {rule_on}: typed over its own glyph"
        );
        let at = step_back + p.glitch_window_for_test() + MS(1);
        assert!(p.predict_glyph('a', &sample(&seen_row(&screen), 2), at));
        assert_eq!(
            p.tail_line_end, !rule_on,
            "rule on {rule_on}: the blank row is the line's end"
        );
        let at = at + RTT;
        let _ = frame(&mut p, "xiai", 3, at);
        assert!(p.idle(), "`a`'s echo retires it");
        assert_eq!(
            p.confirmed_epoch, !rule_on,
            "rule on {rule_on}: `a` proves beside the shifted `i`"
        );
        assert!(p.predict_glyph('d', &sample(&seen_row("xiai"), 3), at));
        let (shown, _) = frame(&mut p, "xiai", 3, at);
        assert_eq!(
            shown.is_empty(),
            rule_on,
            "rule on {rule_on}: `d` over the `i`: {shown:?}"
        );
    }
}

#[test]
fn a_run_started_inside_the_text_proves_nothing() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abcdefg i");
        if !rule_on {
            p.disable_rule_for_test(Rule::InsideRunsNeverProve);
            p.disable_rule_for_test(Rule::DarkOverText);
        }
        let mut at = at;
        for cursor in [11, 10] {
            let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
            at += RTT;
            let _ = frame(&mut p, &screen, cursor - 1, at);
            assert!(p.idle());
        }
        let row = seen_row(&screen);
        assert!(p.predict_glyph('o', &sample(&row, 9), at));
        assert!(!p.engaged(), "R-ROW: the glyph lane's");
        let at = at + RTT;
        let _ = frame(&mut p, "$ abcdefgoi", 10, at);
        assert!(p.idle());
        assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
        let row = seen_row("$ abcdefgoi");
        assert!(p.predict_glyph(' ', &sample(&row, 10), at));
        assert_eq!(p.overlay(at).is_empty(), rule_on, "rule on {rule_on}");
    }
}

#[test]
fn an_erase_before_a_tail_of_spaces_proves_nothing() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "cab ");
        if !rule_on {
            p.disable_rule_for_test(Rule::ShiftProofDistinguishes);
        }
        let _ = press(&mut p, EditKey::Left, &screen, 6, at);
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 5, at);
        assert!(p.idle());
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 5, at),
            EditOutcome::Armed
        );
        let _ = frame(&mut p, "$ ca  ", 4, at + RTT);
        assert!(p.idle(), "rule on {rule_on}: retired");
        let events = p.events_for_test();
        assert_eq!(events.shifts_proven, u64::from(!rule_on));
        assert_eq!(events.ambiguous_shifts, u64::from(rule_on));
        assert_eq!(
            p.edit_line_for_test(),
            (!rule_on).then_some((0, 2, 5, 4)),
            "rule on {rule_on}"
        );
    }
}

#[test]
fn a_glyph_typed_inside_the_text_holds_the_standing_proof_to_its_row() {
    let (mut p, screen, at) = decorated_line("abc");
    let mut at = at;
    for cursor in [5, 4] {
        let _ = press(&mut p, EditKey::Left, &screen, cursor, at);
        at += RTT;
        let _ = frame(&mut p, &screen, cursor - 1, at);
        assert!(p.idle());
    }
    assert!(p.confirmed_epoch, "fixture");
    let voids = p.events_for_test().standing_voids;
    let row = seen_row("$ abc z <r>");
    assert!(p.predict_glyph('x', &sample(&row, 3), at));
    assert!(!p.engaged(), "no chain on a tail that moved");
    assert_eq!(p.events_for_test().standing_voids, voids + 1);
    assert!(!p.confirmed_epoch);
}

#[test]
fn a_right_to_left_row_takes_no_edit_key() {
    let (mut p, _, at) = edited_line(PredictMode::Adaptive, "ab");
    let refusals = p.events_for_test().bidi_refusals;
    assert_eq!(
        press(&mut p, EditKey::Left, "$ ab     \u{5d0}", 4, at),
        EditOutcome::Declined
    );
    assert_eq!(p.events_for_test().bidi_refusals, refusals + 1);

    let (mut p, screen, at) = edited_line(PredictMode::Always, "ab");
    assert_eq!(
        press(&mut p, EditKey::Left, &screen, 4, at),
        EditOutcome::Armed
    );
    let refusals = p.events_for_test().bidi_refusals;
    assert!(!p.predict_char_in_grid('\u{5d0}', (0, 4), G, (12, 6), at));
    assert_eq!(p.events_for_test().bidi_refusals, refusals + 1);
}

#[test]
fn a_proof_whose_echo_was_redrawn_stands_no_more() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::StandingGlyph);
        }
        let t = t0();
        let empty = seen_row("");
        assert!(p.predict_glyph('i', &sample(&empty, 0), t));
        assert!(p.predict_glyph(' ', &sample(&empty, 0), t));
        let at = t + RTT;
        p.reconcile_seen(Some((0, 1)), G, false, at, seen_cells("i"));
        assert!(p.confirmed_epoch, "fixture: `i` proved");
        let at = at + RTT;
        p.reconcile_seen(Some((0, 1)), G, false, at, seen_cells("$"));
        assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
        let row = seen_row("$");
        assert!(p.predict_glyph('8', &sample(&row, 1), at));
        let at = at + MS(5);
        p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("$ "));
        assert_eq!(p.overlay(at).is_empty(), rule_on, "rule on {rule_on}");
    }
}

#[test]
fn a_cancelled_glyph_s_erase_is_no_space_s_echo() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::CancelOwesItsBlank);
        }
        let t = t0();
        let empty = seen_row("");
        assert!(p.predict_glyph('n', &sample(&empty, 0), t));
        assert!(p.predict_backspace(t + MS(5)));
        assert!(p.predict_glyph(' ', &sample(&empty, 0), t + MS(10)));
        let _ = p.take_credited_for_test();
        p.reconcile_seen(Some((0, 1)), G, false, t + MS(40), seen_cells(" "));
        assert!(p.idle(), "rule on {rule_on}: retired");
        let credited = p.take_credited_for_test();
        assert_eq!(
            credited.is_empty(),
            rule_on,
            "rule on {rule_on}: {credited:?}"
        );
    }
}

#[test]
fn an_erase_caught_part_way_is_no_later_state() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::PartWayEchoes);
        }
        let (screen, at) = type_after(&mut p, "", "n", t0());
        assert_eq!(p.edit_line_for_test(), Some((0, 0, 1, 1)), "fixture");
        assert_eq!(
            press(&mut p, EditKey::Backspace, &screen, 1, at),
            EditOutcome::Armed
        );
        assert!(p.predict_glyph(' ', &sample(&seen_row(&screen), 1), at));
        let _ = p.take_credited_for_test();
        p.reconcile_seen(Some((0, 1)), G, false, at + RTT, seen_cells(" "));
        let credited = p.take_credited_for_test();
        assert_eq!(
            credited.len(),
            if rule_on { 0 } else { 2 },
            "rule on {rule_on}"
        );
        if rule_on {
            p.reconcile_seen(Some((0, 0)), G, false, at + RTT, seen_cells(" "));
            assert_eq!(p.take_credited_for_test().len(), 1, "the erase alone");
            assert_eq!(p.preds.len(), 1, "the space still pending");
        }
    }
}

fn two_rows<'t>(top: &'t str, under: &'t str) -> impl FnMut(u16, u16) -> Seen + 't {
    move |r, c| {
        let text = match r {
            0 => top,
            1 => under,
            _ => "",
        };
        text.chars()
            .nth(usize::from(c))
            .map_or(Seen::Blank, |ch| Seen::of_cell(ch, false))
    }
}

fn typed_over(p: &mut Predictor, text: &str, below: &str, at: Instant) -> Instant {
    let under = seen_row(below);
    let mut at = at;
    for (col, ch) in (0u16..).zip(text.chars()) {
        let before: String = text.chars().take(usize::from(col)).collect();
        let row = seen_row(&before);
        let key = KeySample {
            below: Some(&under),
            ..sample(&row, col)
        };
        assert!(p.predict_glyph(ch, &key, at));
        at += RTT;
        let after: String = text.chars().take(usize::from(col) + 1).collect();
        p.reconcile_seen(Some((0, col + 1)), G, false, at, two_rows(&after, below));
    }
    at
}

#[test]
fn text_drawn_under_the_line_voids_its_proof() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::FloorBelow);
        }
        let at = typed_over(&mut p, "ab", "", t0());
        assert!(p.confirmed_epoch, "fixture: a proven line");
        p.reconcile_seen(Some((0, 2)), G, false, at, two_rows("ab", "Password: "));
        assert_eq!(p.confirmed_epoch, !rule_on, "rule on {rule_on}");
        assert_eq!(p.events_for_test().floor_voids, u64::from(rule_on));
    }
    let mut p = Predictor::new(PredictMode::Adaptive);
    let at = typed_over(&mut p, "ab", "ls ion", t0());
    p.reconcile_seen(Some((0, 2)), G, false, at, two_rows("ab", "ls io"));
    assert!(p.confirmed_epoch, "control: text the press saw, redrawn");
}

#[test]
fn text_over_a_wrapped_guess_s_cell_voids_its_proof() {
    for (under, rule_on, voids) in [("m", true, false), ("P", true, true), ("P", false, false)] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::FloorBelow);
        }
        let line = "abcdefghij";
        let at = typed_over(&mut p, line, "", t0());
        assert!(p.confirmed_epoch, "fixture: a proven line");
        let row = seen_row(line);
        let blank = seen_row("");
        for ch in ['k', 'l', 'm'] {
            let key = KeySample {
                below: Some(&blank),
                ..sample(&row, end_of(line))
            };
            assert!(p.predict_glyph(ch, &key, at));
        }
        let wrapped = p.preds.iter().any(|g| (g.row, g.col, g.ch) == (1, 0, 'm'));
        assert!(wrapped, "fixture: `m` wraps onto the floor");
        p.reconcile_seen(Some((0, 10)), G, false, at + MS(5), two_rows(line, under));
        assert_eq!(
            p.confirmed_epoch, !voids,
            "{under:?} under, rule on {rule_on}"
        );
        assert_eq!(p.events_for_test().floor_voids, u64::from(voids));
    }
}

#[test]
fn nothing_shows_behind_a_guess_held_on_its_own_glyph() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::DarkBehindAWait);
        }
        let at = typed_over(&mut p, "ab", "", t0());
        let row = seen_row("ab");
        assert!(p.predict_glyph('c', &sample(&row, 2), at));
        assert!(p.predict_glyph('d', &sample(&row, 2), at));
        let at = at + MS(5);
        p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("abc"));
        let shown: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
        let want: &[char] = if rule_on { &['c'] } else { &['c', 'd'] };
        assert_eq!(shown, want, "rule on {rule_on}");
        let at = at + RTT;
        p.reconcile_seen(Some((0, 3)), G, false, at, seen_cells("abc"));
        let shown: Vec<char> = p.overlay(at).iter().map(|g| g.ch).collect();
        assert_eq!(shown, ['d'], "rule on {rule_on}: `c` retired, `d` shows");
    }

    for arrows in [false, true] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        let at = typed_over(&mut p, "ab", "", t0());
        assert!(p.confirmed_epoch, "fixture: an echo-proven line");
        let _ = p.take_credited_for_test();
        let row = seen_row("ab");
        assert!(p.predict_glyph('c', &sample(&row, 2), at));
        let held = at + MS(5);
        p.reconcile_seen(Some((0, 2)), G, false, held, seen_cells("abc"));
        assert!(p.held_wait.is_some(), "fixture: c is held, not credited");
        assert!(p.take_credited_for_test().is_empty());
        if arrows {
            let row = seen_row("abc");
            assert_eq!(
                p.predict_edit(EditKey::Left, &sample(&row, 2), held + MS(1)),
                EditOutcome::Declined
            );
            assert!(p.preds.is_empty(), "uncertain input is retired and owed");
            assert_eq!(
                p.predict_edit(EditKey::Right, &sample(&row, 2), held + MS(2)),
                EditOutcome::Declined
            );
            p.reconcile_seen(Some((0, 2)), G, false, held + MS(3), seen_cells("abc"));
            assert!(p.take_credited_for_test().is_empty());
        }
        let row = seen_row("abc");
        assert!(p.predict_glyph('x', &sample(&row, 2), held + MS(4)));
        assert!(
            p.overlay(held + MS(4)).iter().all(|g| g.ch != 'x'),
            "arrows {arrows}: nothing new shows on the unresolved head"
        );
    }
    let mut p = Predictor::new(PredictMode::Adaptive);
    let at = typed_over(&mut p, "ab", "", t0());
    let _ = p.take_credited_for_test();
    let row = seen_row("ab");
    assert!(p.predict_glyph('c', &sample(&row, 2), at));
    p.reconcile_seen(Some((0, 2)), G, false, at + MS(5), seen_cells("abc"));
    assert!(p.held_wait.is_some());
    let echoed = at + RTT;
    p.reconcile_seen(Some((0, 3)), G, false, echoed, seen_cells("abc"));
    assert_eq!(p.take_credited_for_test().len(), 1, "c really echoed");
    assert!(p.held_wait.is_none());
    let row = seen_row("abc");
    assert_eq!(
        p.predict_edit(EditKey::Left, &sample(&row, 3), echoed + MS(1)),
        EditOutcome::Armed,
        "a resolved head does not refuse navigation"
    );
}

#[test]
fn a_cursor_back_on_a_credited_cell_takes_the_credit_back() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::CreditTakenBack);
        }
        let t = t0();
        let empty = seen_row("");
        assert!(p.predict_glyph('a', &sample(&empty, 0), t));
        p.reconcile_seen(Some((0, 1)), G, false, t + MS(5), seen_cells("a"));
        assert_eq!(p.take_credited_for_test().len(), 1, "the part-way frame");
        p.reconcile_seen(Some((0, 0)), G, false, t + MS(6), seen_cells("ab"));
        assert_eq!(p.owed_at == Some(t), rule_on, "rule on {rule_on}");
        let row = seen_row("ab");
        assert!(p.predict_glyph('a', &sample(&row, 0), t + MS(7)));
        p.reconcile_seen(Some((0, 1)), G, false, t + MS(60), seen_cells("ab"));
        assert_eq!(
            p.take_credited_for_test().is_empty(),
            rule_on,
            "rule on {rule_on}: the first `a`'s echo credits the second"
        );
    }
}

#[test]
fn a_cursor_on_text_in_the_last_column_is_not_parked() {
    for (rule_on, flag, want) in [
        (true, false, (0, 11)),
        (true, true, (1, 0)),
        (false, false, (1, 0)),
    ] {
        let mut p = Predictor::new(PredictMode::Always);
        if !rule_on {
            p.disable_rule_for_test(Rule::EngineWrapFlag);
        }
        let line = "$ abcdefghij";
        let row = seen_row(line);
        let _ = p.predict_edit(EditKey::Left, &sample(&row, 11), t0());
        p.reconcile_seen(Some((0, 11)), G, false, t0(), seen_cells(line));
        assert!(
            p.deferred_wrap,
            "fixture: a filled last column reads parked"
        );
        let row = seen_row(line);
        let key = KeySample {
            wrap_pending: Some(flag),
            ..sample(&row, 11)
        };
        assert!(p.predict_glyph('x', &key, t0()));
        let placed = p.preds.back().map(|g| (g.row, g.col));
        assert_eq!(placed, Some(want), "rule on {rule_on}, flag {flag}");
    }
}

fn context() -> EditorContext {
    EditorContext::new(1, Some(&[7; 32]), false, false)
}

const PROMPT: u64 = 7;

const B: InputStart = InputStart {
    cell: (0, 2),
    prompt: PROMPT,
};

fn signed(row: &[Seen], cursor: u16) -> KeySample<'_> {
    KeySample {
        input_start: Some(B),
        context: context(),
        ..sample(row, cursor)
    }
}

fn submitted(p: &mut Predictor, at: Instant) -> Instant {
    p.note_line_submit_at(Some(PROMPT - 1), at);
    at + p.glitch_window_for_test() + MS(1)
}

fn type_signed(p: &mut Predictor, screen: &str, text: &str, at: Instant) -> (String, Instant) {
    let mut screen = screen.to_owned();
    let mut at = at;
    for ch in text.chars() {
        let col = end_of(&screen);
        let row = seen_row(&screen);
        assert!(p.predict_glyph(ch, &signed(&row, col), at));
        at += RTT;
        screen.push(ch);
        p.reconcile_seen(Some((0, col + 1)), G, false, at, seen_cells(&screen));
    }
    (screen, at)
}

fn witnessed_line(mode: PredictMode, text: &str) -> (Predictor, String, Instant) {
    let mut p = Predictor::new(mode);
    let at = submitted(&mut p, t0());
    let (screen, at) = type_signed(&mut p, "$ ", text, at);
    assert!(
        p.line
            .as_ref()
            .is_some_and(|l| l.fresh == Some(PROMPT) && l.lo == 2),
        "fixture: a witnessed line"
    );
    (p, screen, at)
}

fn press_signed(
    p: &mut Predictor,
    key: EditKey,
    screen: &str,
    cursor: u16,
    at: Instant,
) -> EditOutcome {
    let row = seen_row(screen);
    p.predict_edit(key, &signed(&row, cursor), at)
}

fn echoed_edge(
    p: &mut Predictor,
    key: EditKey,
    (before, from): (&str, u16),
    (after, to): (&str, u16),
    at: Instant,
) -> Instant {
    assert_eq!(press_signed(p, key, before, from, at), EditOutcome::Armed);
    let at = at + RTT;
    let _ = frame(p, after, to, at);
    assert!(p.idle(), "{key:?}'s echo retires it");
    at
}

fn learn_home_and_end(p: &mut Predictor, screen: &str, end: u16, at: Instant) -> Instant {
    let mut at = at;
    for _ in 0..2 {
        at = echoed_edge(p, EditKey::Home, (screen, end), (screen, 2), at);
        at = echoed_edge(p, EditKey::End, (screen, 2), (screen, end), at);
    }
    at
}

#[test]
fn an_unlearned_key_is_a_dark_probe_until_two_echoes_teach_it() {
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Armed
    );
    assert!(p.preds.iter().all(|g| g.probe), "armed as a probe");
    assert_eq!(frame(&mut p, &screen, 5, at), (vec![], None), "dark");
    let at = at + RTT;
    let _ = frame(&mut p, &screen, 2, at);
    assert!(p.idle(), "its echo retires it");
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 5, 2)));
    assert_eq!(
        p.events_for_test().bindings_learned,
        0,
        "one echo is not two"
    );
    let at = echoed_edge(&mut p, EditKey::End, (&screen, 2), (&screen, 5), at);
    let at = echoed_edge(&mut p, EditKey::Home, (&screen, 5), (&screen, 2), at);
    let at = echoed_edge(&mut p, EditKey::End, (&screen, 2), (&screen, 5), at);
    assert_eq!(p.events_for_test().bindings_learned, 2, "Home and End");
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Armed
    );
    assert_eq!(
        frame(&mut p, &screen, 5, at),
        (vec![], Some((0, 2))),
        "learned: Home shows at once"
    );

    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    p.disable_rule_for_test(Rule::LearnedBindings);
    let _ = press_signed(&mut p, EditKey::Home, &screen, 5, at);
    assert_eq!(frame(&mut p, &screen, 5, at).1, Some((0, 2)), "rule off");
}

#[test]
fn nothing_after_a_probe_is_shown_even_in_always() {
    let (mut p, screen, at) = witnessed_line(PredictMode::Always, "ab");
    assert!(p.predict_glyph('c', &signed(&seen_row(&screen), 4), at));
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 4, at),
        EditOutcome::Armed
    );
    assert!(p.predict_glyph('x', &signed(&seen_row(&screen), 4), at));
    assert_eq!(p.visible_ops_for_test(), 1, "`c` alone, before the probe");
    let shown = frame(&mut p, &screen, 4, at);
    assert_eq!(shown, (vec![(4, 'c')], Some((0, 5))), "the state after `c`");
}

#[test]
fn a_learned_key_is_placed_only_on_a_witnessed_line() {
    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Declined,
        "a line not started at the B"
    );
    assert!(p.idle() && !p.confirmed_epoch && p.owed_at == Some(at));

    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    assert_eq!(
        press(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Declined,
        "a press that hands no start"
    );
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    let row = seen_row(&screen);
    let elsewhere = KeySample {
        input_start: Some(InputStart { cell: (0, 3), ..B }),
        ..signed(&row, 5)
    };
    assert_eq!(
        p.predict_edit(EditKey::Home, &elsewhere, at),
        EditOutcome::Declined,
        "a B the line was not started at"
    );
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    let row = seen_row(&screen);
    let next_prompt = KeySample {
        input_start: Some(InputStart {
            prompt: PROMPT + 1,
            ..B
        }),
        ..signed(&row, 5)
    };
    assert_eq!(
        p.predict_edit(EditKey::Home, &next_prompt, at),
        EditOutcome::Declined,
        "another prompt's B at the same cell"
    );

    let (mut p, screen, at) = edited_line(PredictMode::Adaptive, "abc");
    p.disable_rule_for_test(Rule::WitnessFreshPrompt);
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Armed,
        "rule off: any B on the row"
    );
}

fn witness(p: &Predictor) -> Option<u64> {
    p.line.as_ref().and_then(|l| l.fresh)
}

#[test]
fn only_the_first_key_of_a_prompt_starts_a_witnessed_line() {
    type Shut = fn(&mut Predictor, Instant);
    let cases: [(&str, Shut); 6] = [
        ("a key not modelled", |p, at| {
            p.note_line_submit_at(Some(PROMPT - 1), at);
            p.note_unmodelled_input(at);
        }),
        ("an Enter at the same prompt", |p, at| {
            p.note_line_submit_at(Some(PROMPT), at);
        }),
        ("an Enter at no prompt", |p, at| {
            p.note_line_submit_at(None, at);
        }),
        ("an Enter at a prompt not named", |p, at| {
            p.note_line_submit(at);
        }),
        ("a reset", |p, at| {
            p.note_line_submit_at(Some(PROMPT - 1), at);
            p.reset();
        }),
        ("a mode switched", |p, at| {
            p.note_line_submit_at(Some(PROMPT - 1), at);
            p.set_mode(PredictMode::Always);
        }),
    ];
    for (label, shut) in cases {
        for rule_on in [true, false] {
            let mut p = Predictor::new(PredictMode::Adaptive);
            if !rule_on {
                p.disable_rule_for_test(Rule::WitnessFirstKey);
            }
            let at = t0();
            shut(&mut p, at);
            let at = at + p.glitch_window_for_test() + MS(1);
            let (_, _) = type_signed(&mut p, "$ ", "cd", at);
            assert_eq!(
                witness(&p),
                (!rule_on).then_some(PROMPT),
                "{label}, rule on {rule_on}"
            );
        }
    }
    let mut p = Predictor::new(PredictMode::Adaptive);
    let at = submitted(&mut p, t0());
    let _ = type_signed(&mut p, "$ ", "cd", at);
    assert_eq!(witness(&p), Some(PROMPT), "control: the first key");
}

#[test]
fn a_scroll_after_a_line_terminator_keeps_the_first_key_s_gate() {
    for (host_reset, rule_on) in [(false, true), (true, true), (false, false)] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::ScrollKeepsFirstKey);
        }
        let at = t0();
        p.reconcile_seen(Some((0, 2)), G + 1, false, at, seen_cells("$ "));
        let at = submitted(&mut p, at);
        if host_reset {
            p.reset();
        }
        p.reconcile_seen(Some((0, 2)), G, false, at, seen_cells("$ "));
        let _ = type_signed(&mut p, "$ ", "cd", at);
        assert_eq!(
            witness(&p),
            (!host_reset && rule_on).then_some(PROMPT),
            "host reset {host_reset}, rule on {rule_on}"
        );
    }
}

#[test]
fn a_b_left_behind_by_a_narrower_redraw_witnesses_nothing() {
    for rule_on in [true, false] {
        fn at_stale(row: &[Seen], cursor: u16) -> KeySample<'_> {
            KeySample {
                input_start: Some(InputStart {
                    cell: (0, 4),
                    prompt: PROMPT,
                }),
                context: context(),
                ..sample(row, cursor)
            }
        }
        let mut p = Predictor::new(PredictMode::Always);
        if !rule_on {
            p.disable_rule_for_test(Rule::WitnessFirstKey);
        }
        let mut at = submitted(&mut p, t0());
        let mut screen = String::from("$$$ ");
        for ch in ['a', 'b'] {
            let col = end_of(&screen);
            assert!(p.predict_glyph(ch, &at_stale(&seen_row(&screen), col), at));
            at += RTT;
            screen.push(ch);
            let _ = frame(&mut p, &screen, col + 1, at);
        }
        assert_eq!(witness(&p), Some(PROMPT), "rule on {rule_on}: control");
        for _ in 0..2 {
            for (key, from, to) in [(EditKey::Home, 6, 4), (EditKey::End, 4, 6)] {
                let row = seen_row(&screen);
                assert_eq!(
                    p.predict_edit(key, &at_stale(&row, from), at),
                    EditOutcome::Armed
                );
                at += RTT;
                let _ = frame(&mut p, &screen, to, at);
            }
        }
        assert_eq!(p.events_for_test().bindings_learned, 2);
        let screen = String::from("$ ab");
        at += RTT;
        let _ = frame(&mut p, &screen, 4, at);
        assert!(p.predict_glyph('h', &at_stale(&seen_row(&screen), 4), at));
        at += RTT;
        let screen = String::from("$ abh");
        let _ = frame(&mut p, &screen, 5, at);
        let outcome = p.predict_edit(EditKey::Home, &at_stale(&seen_row(&screen), 5), at);
        if rule_on {
            assert_eq!(witness(&p), None, "the stale B witnesses nothing");
            assert_eq!(outcome, EditOutcome::Declined);
        } else {
            assert_eq!(outcome, EditOutcome::Armed, "rule off");
            assert_eq!(
                frame(&mut p, &screen, 5, at).1,
                Some((0, 4)),
                "rule off: Home on the stale B, zle at column 2"
            );
        }
    }
}

#[test]
fn a_contradicted_binding_is_unlearned_and_a_second_bans_it() {
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    let mut at = learn_home_and_end(&mut p, &screen, 5, at);
    for round in 0..2 {
        assert_eq!(
            press_signed(&mut p, EditKey::Home, &screen, 5, at),
            EditOutcome::Armed
        );
        let shown = frame(&mut p, &screen, 5, at).1;
        assert_eq!(shown.is_some(), round == 0, "learned, then a probe");
        at += p.glitch_window_for_test() + MS(1);
        let _ = frame(&mut p, &screen, 5, at);
        assert!(p.idle(), "no echo: it expires");
        assert_eq!(p.events_for_test().bindings_unlearned, round + 1);
        at += p.glitch_window_for_test() + MS(1);
        at = submitted(&mut p, at);
        (_, at) = type_signed(&mut p, "$ ", "abc", at);
    }
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Declined,
        "banned"
    );
    assert!(p.idle() && !p.confirmed_epoch);
}

#[test]
fn bindings_hold_for_their_editor_context_and_travel_with_the_session() {
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    let at = learn_home_and_end(&mut p, &screen, 5, at);
    let estimate = p.take_link_estimate();
    p.restore_link_estimate(estimate);
    let at = submitted(&mut p, at);
    let (screen, at) = type_signed(&mut p, "$ ", "abc", at);
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Armed
    );
    assert!(frame(&mut p, &screen, 5, at).1.is_some(), "carried: shown");

    for (label, other) in [
        (
            "another nonce",
            EditorContext::new(1, Some(&[8; 32]), false, false),
        ),
        ("DECCKM", EditorContext::new(1, Some(&[7; 32]), true, false)),
        (
            "escaped chords",
            EditorContext::new(1, Some(&[7; 32]), false, true),
        ),
    ] {
        assert_ne!(other, context(), "{label}: another context");
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let at = learn_home_and_end(&mut p, &screen, 5, at);
        let row = seen_row(&screen);
        let key = KeySample {
            context: other,
            ..signed(&row, 5)
        };
        assert_eq!(p.predict_edit(EditKey::Home, &key, at), EditOutcome::Armed);
        assert_eq!(
            frame(&mut p, &screen, 5, at),
            (vec![], None),
            "{label}: a probe"
        );
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let at = learn_home_and_end(&mut p, &screen, 5, at);
        let _ = press_signed(&mut p, EditKey::Home, &screen, 5, at);
        assert!(
            frame(&mut p, &screen, 5, at).1.is_some(),
            "{label}: control"
        );
    }

    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    let at = learn_home_and_end(&mut p, &screen, 5, at);
    p.reset_session();
    let at = submitted(&mut p, at);
    let (screen, at) = type_signed(&mut p, "$ ", "abc", at);
    assert!(
        p.line.as_ref().is_some_and(|l| l.fresh == Some(PROMPT)),
        "control: a witnessed line"
    );
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Armed
    );
    assert_eq!(
        frame(&mut p, &screen, 5, at),
        (vec![], None),
        "an unknown session: a probe"
    );
}

#[test]
fn a_new_editor_context_flushes_the_old_one_s_ops() {
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Armed
    );
    let row = seen_row(&screen);
    let other = KeySample {
        context: EditorContext::new(1, Some(&[8; 32]), false, false),
        ..signed(&row, 5)
    };
    let later = at + MS(10);
    let _ = p.predict_edit(EditKey::End, &other, later);
    assert!(
        p.preds
            .iter()
            .all(|g| g.op.learned() != Some(Learned::Home)),
        "the old context's Home is flushed"
    );
    assert!(p.owed_at >= Some(at), "and owed");
}

#[test]
fn a_divergence_a_learned_key_heads_unlearns_it() {
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    let at = learn_home_and_end(&mut p, &screen, 5, at);
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Armed
    );
    assert!(frame(&mut p, &screen, 5, at).1.is_some(), "learned: shown");
    let diverged = format!("{screen}~");
    let at = at + RTT;
    let _ = frame(&mut p, &diverged, 6, at);
    assert!(p.idle(), "no state of the chain: flushed");
    assert_eq!(p.events_for_test().bindings_unlearned, 1, "contradicted");
    let at = at + p.glitch_window_for_test() + MS(1);
    let at = submitted(&mut p, at);
    let (screen, at) = type_signed(&mut p, "$ ", "abc", at);
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 5, at),
        EditOutcome::Armed
    );
    assert_eq!(
        frame(&mut p, &screen, 5, at),
        (vec![], None),
        "unlearned: a probe again"
    );
}

#[test]
fn ctrl_a_under_a_kitty_mode_is_not_modelled() {
    for kitty in [true, false] {
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
        let row = seen_row(&screen);
        let key = KeySample {
            escaped_chords: kitty,
            ..signed(&row, 5)
        };
        let outcome = p.predict_edit(EditKey::CtrlA, &key, at);
        assert_eq!(outcome == EditOutcome::Armed, !kitty, "kitty {kitty}");
        assert_eq!(p.events_for_test().kitty_refusals, u64::from(kitty));
        if kitty {
            assert!(!p.confirmed_epoch, "not modelled: the epoch ended");
            let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
            let row = seen_row(&screen);
            let home = KeySample {
                escaped_chords: true,
                ..signed(&row, 5)
            };
            assert_eq!(p.predict_edit(EditKey::Home, &home, at), EditOutcome::Armed);
        }
    }
}

#[test]
fn end_at_the_end_of_the_text_is_declined_with_its_pixels_kept() {
    for rule_on in [true, false] {
        let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "ab");
        if !rule_on {
            p.disable_rule_for_test(Rule::EndRefusal);
        }
        let at = learn_home_and_end(&mut p, &screen, 4, at);
        assert!(p.predict_glyph('c', &signed(&seen_row(&screen), 4), at));
        assert_eq!(frame(&mut p, &screen, 4, at).0, vec![(4, 'c')]);
        let declines = p.events_for_test().decline_motions;
        let outcome = press_signed(&mut p, EditKey::End, &screen, 4, at);
        if rule_on {
            assert_eq!(outcome, EditOutcome::Declined);
            assert_eq!(p.events_for_test().decline_motions, declines + 1);
            assert!(p.anchor_lost && !p.confirmed_epoch);
            assert_eq!(frame(&mut p, &screen, 4, at).0, vec![(4, 'c')], "kept");
        } else {
            assert_eq!(outcome, EditOutcome::Armed, "rule off");
        }
    }
}

fn delete_learned_on_ade() -> (Predictor, Instant) {
    let (mut p, screen, mut at) = witnessed_line(PredictMode::Adaptive, "abcde");
    for cursor in [7, 6, 5, 4] {
        assert_eq!(
            press_signed(&mut p, EditKey::Left, &screen, cursor, at),
            EditOutcome::Armed
        );
        at += RTT;
        let _ = frame(&mut p, &screen, cursor - 1, at);
    }
    at = echoed_edge(&mut p, EditKey::Delete, (&screen, 3), ("$ acde", 3), at);
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 6, 3)), "`b` deleted");
    at = echoed_edge(&mut p, EditKey::Delete, ("$ acde", 3), ("$ ade", 3), at);
    assert_eq!(p.events_for_test().bindings_learned, 1, "Delete learned");
    (p, at)
}

#[test]
fn delete_erases_the_glyph_under_the_cursor_and_shifts_the_tail() {
    let (mut p, at) = delete_learned_on_ade();
    assert_eq!(
        press_signed(&mut p, EditKey::Delete, "$ ade", 3, at),
        EditOutcome::Armed
    );
    let _ = frame(&mut p, "$ ade", 3, at);
    let ghosts: Vec<(u16, char, GhostCell)> = p
        .overlay(at)
        .iter()
        .map(|g| (g.col, g.ch, g.ghost()))
        .collect();
    assert_eq!(
        ghosts,
        [
            (3, 'e', GhostCell::Moved { from: 4 }),
            (4, ' ', GhostCell::Vacated)
        ],
        "`d` deleted: `e` moved onto its cell, the last cell vacated"
    );
    assert_eq!(p.overlay_cursor(), Some((0, 3)));
    let at = at + RTT;
    let _ = frame(&mut p, "$ ae", 3, at);
    assert!(p.idle(), "its echo retires it");
    assert_eq!(p.edit_line_for_test(), Some((0, 2, 4, 3)));
}

#[test]
fn delete_vacates_only_beside_blanks() {
    let (mut p, at) = delete_learned_on_ade();
    assert_eq!(
        press_signed(&mut p, EditKey::Delete, "$ adez", 3, at),
        EditOutcome::Declined,
        "text where the line's end vacates"
    );
    let (mut p, at) = delete_learned_on_ade();
    assert_eq!(
        press_signed(&mut p, EditKey::Delete, "$ ade", 3, at),
        EditOutcome::Armed,
        "control: a blank row"
    );
    let _ = frame(&mut p, "$ ade", 3, at);
    assert!(!p.idle(), "control: a blank frame keeps it");
    let _ = frame(&mut p, "$ adez", 3, at + MS(5));
    assert!(p.idle(), "text drawn one past every state flushes it");
}

#[test]
fn home_at_the_start_and_delete_at_the_end_are_not_modelled() {
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    assert_eq!(
        press_signed(&mut p, EditKey::Delete, &screen, 5, at),
        EditOutcome::Declined
    );
    let (mut p, screen, at) = witnessed_line(PredictMode::Adaptive, "abc");
    let at = echoed_edge(&mut p, EditKey::Home, (&screen, 5), (&screen, 2), at);
    assert_eq!(
        press_signed(&mut p, EditKey::Home, &screen, 2, at),
        EditOutcome::Declined
    );
}

#[test]
fn a_cancelled_stale_guess_anchors_a_stale_run() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::StaleCancel);
        }
        let (screen, at) = type_after(&mut p, "", "xd", t0());
        assert!(p.adaptive_slow, "fixture: a slow link");
        p.note_unmodelled_input(at);
        assert!(p.predict_glyph('o', &sample(&seen_row(&screen), 2), at));
        assert!(p.preds.iter().all(|g| g.stale), "a stale anchor");
        let at = at + RTT;
        let _ = frame(&mut p, "xo", 2, at);
        assert_eq!(p.preds.len(), 1, "`o` waits on its blank cell");
        let at = at + p.glitch_window_for_test();
        assert!(p.predict_backspace(at), "the Backspace cancels `o`");
        let row = seen_row("xo");
        assert!(p.predict_glyph('a', &sample(&row, 2), at));
        assert!(p.predict_glyph('a', &sample(&row, 2), at));
        assert_eq!(
            p.preds.iter().all(|g| g.stale),
            rule_on,
            "rule on {rule_on}: the run is stale"
        );
        let _ = p.take_credited_for_test();
        let (shown, _) = frame(&mut p, "xaa", 3, at + RTT);
        let credited = p.take_credited_for_test();
        assert_eq!(
            (credited.len(), shown.is_empty()),
            if rule_on { (0, true) } else { (1, false) },
            "rule on {rule_on}: {shown:?}"
        );
    }
}

#[test]
fn a_delete_s_step_back_is_no_later_state() {
    for rule_on in [true, false] {
        let (mut p, at) = delete_learned_on_ade();
        if !rule_on {
            p.disable_rule_for_test(Rule::PartWayStepBack);
        }
        assert_eq!(
            press_signed(&mut p, EditKey::Delete, "$ ade", 3, at),
            EditOutcome::Armed
        );
        assert_eq!(
            press_signed(&mut p, EditKey::End, "$ ade", 3, at),
            EditOutcome::Armed
        );
        let _ = p.take_credited_for_test();
        let at = at + RTT;
        let _ = frame(&mut p, "$ ae", 4, at);
        let credited = p.take_credited_for_test();
        assert_eq!(
            credited.len(),
            if rule_on { 0 } else { 2 },
            "rule on {rule_on}: {credited:?}"
        );
        if rule_on {
            assert_eq!(p.preds.len(), 2, "both wait for the step back");
            let _ = frame(&mut p, "$ ae", 3, at + MS(2));
            assert_eq!(p.take_credited_for_test().len(), 1, "Delete alone");
            assert_eq!(p.preds.len(), 1, "End still pending");
        }
    }
}

#[test]
fn a_narrower_redraw_is_no_echo_in_transit() {
    for rule_on in [true, false] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::TransitBlanks);
        }
        let (screen, at) = type_after(&mut p, "$$$ ", "ab", t0());
        assert_eq!(p.edit_line_for_test(), Some((0, 4, 6, 6)), "fixture");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 6, at),
            EditOutcome::Armed
        );
        let at = at + RTT;
        let _ = frame(&mut p, &screen, 5, at);
        assert!(p.idle(), "fixture: the motion class proven");
        assert_eq!(
            press(&mut p, EditKey::Left, &screen, 5, at),
            EditOutcome::Armed
        );
        assert_eq!(frame(&mut p, &screen, 5, at), (vec![], Some((0, 4))));
        let shown = frame(&mut p, "$ ab", 4, at + MS(5));
        if rule_on {
            assert!(p.idle(), "flushed");
            assert_eq!(shown, (vec![], None));
        } else {
            assert_eq!(shown, (vec![(4, 'a'), (5, 'b')], Some((0, 4))), "rule off");
        }
    }
}
