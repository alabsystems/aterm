// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn feed(t: &mut ToneTracker, now: Instant, session: u64, s: &str) {
    for c in s.chars() {
        t.note_char(now, session, c, true);
        t.drain_pending_inference(now);
    }
}

#[test]
fn the_key_path_marks_inference_due_and_only_the_drain_runs_it() {
    let mut t = ToneTracker::default();
    let now = Instant::now();
    for c in "why is this broken again ugh".chars() {
        t.note_char(now, 1, c, true);
    }
    assert_eq!(t.inferences, 0, "no frame ⇒ the classifier has not run");
    assert_eq!(t.current(), Tone::Technical);
    assert!(t.infer_pending, "…but the keystrokes marked one due");
    t.drain_pending_inference(now);
    assert_eq!(t.inferences, 1, "one drain runs exactly one inference");
    assert_eq!(t.current(), Tone::Frustrated);
    t.drain_pending_inference(now);
    assert_eq!(t.inferences, 1, "a drain with nothing pending is free");
}

fn feed_inactive(t: &mut ToneTracker, now: Instant, session: u64, s: &str) {
    for c in s.chars() {
        t.note_char(now, session, c, false);
    }
}

#[test]
fn inference_is_throttled_by_keystrokes() {
    let mut t = ToneTracker::default();
    let now = Instant::now();
    feed(&mut t, now, 1, "why is this broken again ugh");
    let n = t.inferences;
    assert!(n >= 1, "at least one inference must have run");
    assert!(
        n <= 28 / INFER_KEYS as u64 + 1,
        "throttle failed: {n} inferences for 28 same-instant keys"
    );
}

#[test]
fn interval_reopens_inference() {
    let mut t = ToneTracker::default();
    let start = Instant::now();
    feed(&mut t, start, 1, "abc");
    let before = t.inferences;
    t.note_char(start + INFER_INTERVAL, 1, 'd', true);
    t.drain_pending_inference(start + INFER_INTERVAL);
    assert_eq!(
        t.inferences,
        before + 1,
        "an elapsed interval must re-infer"
    );
}

#[test]
fn session_switch_clears_window_but_keeps_mood() {
    let mut t = ToneTracker::default();
    let now = Instant::now();
    feed(&mut t, now, 1, "ㅋㅋㅋㅋ 너무 웃겨 ㅎㅎ");
    assert_eq!(t.current(), Tone::Playful);
    t.note_char(now, 2, 'a', true); // new session: window restarts
    assert_eq!(t.buf, vec!['a'], "the old session's chars must be gone");
    assert_eq!(t.current(), Tone::Playful, "the mood itself persists");
}

#[test]
fn backspace_pops_and_window_stays_bounded() {
    let mut t = ToneTracker::default();
    let now = Instant::now();
    feed(&mut t, now, 1, "abcd");
    let before = t.inferences;
    t.note_backspace(1);
    assert_eq!(t.buf.len(), 3);
    assert_eq!(t.inferences, before, "deletion alone must not re-infer");
    for _ in 0..(BUF_CAP * 2) {
        t.note_char(now, 1, 'x', true);
    }
    assert!(t.buf.len() <= BUF_CAP);
}

#[test]
fn thin_window_after_a_classified_line_keeps_the_mood() {
    let mut t = ToneTracker::default();
    let start = Instant::now();
    feed(&mut t, start, 1, "why is this broken again ugh");
    assert_eq!(t.current(), Tone::Frustrated);
    t.note_break(); // Enter — window clears, mood persists…
    t.note_char(start + INFER_INTERVAL, 1, 'x', true);
    t.drain_pending_inference(start + INFER_INTERVAL);
    assert_eq!(t.buf, vec!['x'], "the window is the single new char");
    assert_eq!(
        t.current(),
        Tone::Frustrated,
        "abstention must leave the cached mood untouched"
    );
}

#[test]
fn effective_is_neutral_with_the_knob_off_even_with_a_cached_verdict() {
    let mut t = ToneTracker::default();
    let now = Instant::now();
    feed(&mut t, now, 1, "why is this broken again ugh");
    assert_eq!(t.current(), Tone::Frustrated);
    assert_eq!(t.effective(true), Tone::Frustrated);
    assert_eq!(t.effective(false), Tone::Technical);
    assert_eq!(t.current(), Tone::Frustrated, "the cache is not cleared");
}

#[test]
fn breaks_while_inactive_keep_the_window_coherent() {
    let mut t = ToneTracker::default();
    let now = Instant::now();
    feed(&mut t, now, 1, "why is this broken again ugh");
    assert_eq!(t.current(), Tone::Frustrated);
    feed_inactive(&mut t, now, 1, "still poking at it");
    assert!(!t.buf.is_empty(), "the window tracks edits while inactive");
    t.note_break(); // Enter recorded while inactive — must clear.
    assert!(
        t.buf.is_empty(),
        "a break must clear the window even when inference is inactive"
    );
    feed(&mut t, now, 1, "git rebase -i HEAD~3 && cargo test");
    assert_eq!(
        t.current(),
        Tone::Technical,
        "the re-enabled window carries only post-break text"
    );
}

#[test]
fn drain_serious_clears_the_window_and_pending_inference_while_keeping_cached_tone() {
    let mut t = ToneTracker::default();
    let now = Instant::now();
    feed(&mut t, now, 1, "why is this broken again ugh");
    assert_eq!(t.current(), Tone::Frustrated);
    let inferences_before = t.inferences;
    t.note_char(now + Duration::from_secs(1), 1, '!', true);
    assert!(t.infer_pending && t.window_chars() > 0);
    t.drain_serious();
    assert_eq!(t.window_chars(), 0);
    assert!(!t.infer_pending);
    t.drain_pending_inference(now + Duration::from_secs(1));
    assert_eq!(t.inferences, inferences_before);
    assert_eq!(t.current(), Tone::Frustrated);
}
