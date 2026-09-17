// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE INK WRAP SEAM TRACE** — the owner: "the rainbow cursor streak
//! doesn't follow the new line down in claude code and codex".
//!
//! A headless replay of Claude Code's (Ink's) composer WRAP at the seam,
//! modelled on `a_line_still_being_typed_has_no_dark_cell_inside_its_live_span`:
//! keys at 70 ms, each echo observed 9 ms after its key, ticks at 60 Hz, the
//! caret typed from Ink's inset column 2 out to column 78 on the composer's
//! bottom row. The wrap key then makes Ink re-wrap its box: the last word
//! (`pers`) moves down to the next visual row and the box grows one row.
//! Three observed shapes of that one frame are replayed:
//!
//! * `ScrolledOneMove` — the measured Claude Code shape on the main screen
//!   with the box at the screen's bottom (`classify_move`'s "BOX-GROWTH
//!   wrap": the box grows a row UP, the screen scrolls one line in the same
//!   repaint, the caret's terminal row stays constant). The host sees a
//!   `note_scroll(1)` and then the caret at `(row, 6)` — after
//!   `translate_scroll_state` the move is `(row−1, 78) → (row, 6)`.
//! * `ScrolledTwoMove` — the same, observed as Ink's two-move rewrite
//!   (`RAINBOW-KITTY-V2.md`'s "end → col 2, then col 2 → end + 1"): the
//!   caret first at the inset column `(row, 2)`, one tick later at `(row, 6)`.
//! * `BoxGrowsUpNoScroll` — the box not at the screen's bottom (or the alt
//!   screen): no scroll, the caret's terminal row constant, the old text
//!   moved UP a row by the app's rewrite without the host being told —
//!   the move is the same-row `(row, 78) → (row, 6)`.
//!
//! Every frame prints the v2 cells per row (`v2_cols`) from just before the
//! wrap to 3 s after it, and the seam's ring rows (with their `licence=`)
//! around the wrap. The assertions state what SHOULD hold (the owner's
//! expectation, the task's brief): the band SPANS the wrap — a second on,
//! with the hand typing below, the row it wrapped off still carries the
//! light its glyphs were typed with (2026-09-15); that light is gone within
//! ~2 s although the hand keeps typing on the new row; nothing stays lit on
//! the old row past its last glyph — the cells the re-wrap MOVED leave with
//! their text; and the new row's cells are contiguous from Ink's inset
//! column while it is being typed.

use super::*;
use std::collections::BTreeSet;

/// The observed shape of the wrap frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WrapShape {
    ScrolledOneMove,
    /// The scrolled shape with a ONE-letter wrapped word: the landing at
    /// col 3 from col 78 of an 80-cell pane is inside the seam's
    /// `coalesced_fold_shape` (landing 1..=3, origin within 4 of the edge).
    ScrolledShortWord,
    ScrolledTwoMove,
    BoxGrowsUpNoScroll,
}

impl WrapShape {
    /// The wrapped word's length: the caret lands at `INSET + wrapped`.
    fn wrapped(self) -> u16 {
        match self {
            WrapShape::ScrolledShortWord => 1,
            _ => WRAPPED,
        }
    }
}

/// Ink's inset column (after the `› ` prompt marker).
const INSET: u16 = 2;
/// The composer's caret row (the box's bottom row) before the wrap.
const ROW: u16 = 20;
/// The line typed before the wrap key — 76 cells from the inset, so the
/// caret stands at col 78 when the wrap key comes; `it's` sits at cols
/// 70..=73 and ` per` at 74..=77 (the cells Ink's re-wrap will blank).
const LINE: &str = "r streak doesn't follow the new line down in claude code and codex. it's per";
/// The wrapped word's length: `pers` — the wrap key is its `s`, and after
/// the re-wrap the caret sits at `INSET + 4` on the new row.
const WRAPPED: u16 = 4;
/// Typed after the wrap, on the new row.
const AFTER: &str = "istent on the sc";

const KEY_MS: u64 = 70;
const ECHO_MS: u64 = 9;

fn ranges(set: &BTreeSet<u16>) -> String {
    let mut out = String::new();
    let mut it = set.iter().copied().peekable();
    while let Some(start) = it.next() {
        let mut end = start;
        while it.peek() == Some(&(end + 1)) {
            end = it.next().unwrap();
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            out.push_str(&start.to_string());
        } else {
            out.push_str(&format!("{start}-{end}"));
        }
    }
    if out.is_empty() {
        out.push('-');
    }
    out
}

/// One pending echo: the caret the host will observe at `at`, and whether
/// this echo is the WRAP frame (a scroll may precede the caret).
#[derive(Clone, Copy)]
struct Echo {
    at: Instant,
    to: (u16, u16),
    scroll: u16,
}

struct Replay {
    log: Vec<String>,
    violations: Vec<String>,
}

fn replay(shape: WrapShape) -> Replay {
    // An 80-column pane, the measured instance's width: Ink's box ends at
    // col 78, two cells short of the pane's edge.
    let g = Geom {
        cw: 8,
        ch: 16,
        rows: 30,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 480,
        head: 0,
    };
    let wrapped = shape.wrapped();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let tick = Duration::from_micros(16_667);
    let ms = |t: Instant| -> i64 { t.saturating_duration_since(t0).as_millis() as i64 };

    glow.tick(Some((ROW, INSET)), t0, &c, g, &mut out);
    assert!(glow.v2.engaged());

    let mut log = Vec::new();
    let mut violations = Vec::new();
    let mut law_hits = [0usize; 4];
    let mut now = t0;
    let mut key_at = t0;
    let mut caret: (u16, u16) = (ROW, INSET);
    let mut pending: Vec<Echo> = Vec::new();
    // The wrap key's echo clock, once it has been observed (the reference
    // every assertion is stated against).
    let mut wrap_echo: Option<Instant> = None;
    let mut last_echo: Instant = t0;
    let mut next_print: Option<Instant> = None;
    let mut ring_snap: Vec<String> = Vec::new();

    // The old row after the wrap: on the scrolled shapes the old text moved
    // up with the scroll; on the no-scroll shape the app moved it up and
    // the host was not told, so the old light stays on ROW.
    let old_row = match shape {
        WrapShape::BoxGrowsUpNoScroll => ROW,
        _ => ROW - 1,
    };
    let new_row = ROW;

    // key → (echo(s)).
    type ScheduledEcho = (u64, (u16, u16), u16);
    let keys: Vec<(char, Vec<ScheduledEcho>)> = {
        let mut v = Vec::new();
        let mut col = INSET;
        for ch in LINE.chars() {
            col += 1;
            v.push((ch, vec![(ECHO_MS, (ROW, col), 0)]));
        }
        assert_eq!(col, 78, "the caret stands at col 78 before the wrap key");
        // THE WRAP KEY: Ink moves `pers` down; the caret lands at INSET + 4.
        let landing = (ROW, INSET + wrapped);
        let wrap_echoes = match shape {
            WrapShape::ScrolledOneMove | WrapShape::ScrolledShortWord => {
                vec![(ECHO_MS, landing, 1)]
            }
            WrapShape::ScrolledTwoMove => {
                vec![(ECHO_MS, (ROW, INSET), 1), (ECHO_MS + 17, landing, 0)]
            }
            WrapShape::BoxGrowsUpNoScroll => vec![(ECHO_MS, landing, 0)],
        };
        v.push(('s', wrap_echoes));
        let mut col = INSET + wrapped;
        for ch in AFTER.chars() {
            col += 1;
            v.push((ch, vec![(ECHO_MS, (ROW, col), 0)]));
        }
        v
    };
    let n_keys = keys.len();

    let mut keys_iter = keys.into_iter().enumerate().peekable();
    let idle_until_after_wrap = Duration::from_millis(3000);

    loop {
        // Advance the clock one tick, applying every echo that is due.
        now += tick;
        let mut due: Vec<Echo> = pending.iter().copied().filter(|e| now >= e.at).collect();
        pending.retain(|e| now < e.at);
        due.sort_by_key(|e| e.at);
        let mut observed_this_tick = false;
        for e in due {
            if e.scroll > 0 {
                glow.note_scroll(e.scroll);
            }
            caret = e.to;
            last_echo = now;
            observed_this_tick = true;
            if e.to.0 == ROW
                && e.to.1 <= INSET + wrapped
                && wrap_echo.is_none()
                && e.at > t0 + Duration::from_millis(KEY_MS * 70)
            {
                wrap_echo = Some(now);
            }
        }
        glow.tick(Some(caret), now, &c, g, &mut out);
        let old = v2_cols(&glow, old_row);
        let new = v2_cols(&glow, new_row);

        // The ring around the wrap: snapshot after the wrap echo tick(s) and
        // after the first two keys on the new row.
        if let Some(w) = wrap_echo
            && observed_this_tick
            && now <= w + Duration::from_millis(200)
        {
            let rows = ring_rows(&glow);
            let tail: Vec<String> = rows
                .iter()
                .rev()
                .take(3)
                .rev()
                .map(|(reason, licence, o, t)| format!("{reason} licence={licence} {o:?}->{t:?}"))
                .collect();
            let fl = glow.in_flight_tally();
            ring_snap.push(format!(
                "+{:.3}s ring[{}] mirror={:?} credits={} park_flushed={} forgotten={} pending_v2={}",
                (ms(now) - ms(w)) as f32 / 1000.0,
                tail.join(" | "),
                glow.v2.caret_mirror(),
                glow.typed_credits_within(now),
                fl.park_flushed,
                fl.forgotten,
                glow.v2.pending_events().len(),
            ));
        }

        // Frame log: every tick from 60 ms before the wrap to 300 ms after
        // it, then every 100 ms.
        let print = match wrap_echo {
            None => {
                key_at
                    >= t0 + Duration::from_millis(KEY_MS * (n_keys as u64 - AFTER.len() as u64 - 1))
                        - Duration::from_millis(60)
            }
            Some(w) => {
                if now <= w + Duration::from_millis(300) {
                    true
                } else {
                    match next_print {
                        Some(np) if now < np => false,
                        _ => {
                            next_print = Some(now + Duration::from_millis(100));
                            true
                        }
                    }
                }
            }
        };
        if print {
            let rel = wrap_echo.map_or_else(
                || format!("wrap{:+}ms", 0),
                |w| format!("wrap{:+}ms", ms(now) - ms(w)),
            );
            log.push(format!(
                "+{:>6}ms {rel:>12} caret={:?} row{}={:<12} row{}={:<12} typing={}",
                ms(now),
                caret,
                old_row,
                ranges(&old),
                new_row,
                ranges(&new),
                now <= last_echo + Duration::from_millis(KEY_MS),
            ));
        }

        // THE ASSERTIONS (what should hold).
        if let Some(w) = wrap_echo {
            let since = now.saturating_duration_since(w);
            let hand_on_new_row = keys_iter.peek().is_some() || now <= last_echo;
            match shape {
                WrapShape::ScrolledOneMove
                | WrapShape::ScrolledShortWord
                | WrapShape::ScrolledTwoMove => {
                    // (2) nothing stays lit on the old row past its last
                    // glyph (`it's` ends at col 73; 74..=77 are blank after
                    // Ink's re-wrap) once a retract could have run.
                    // The moved word's cells and the space the wrap ate:
                    // `78 - wrapped ..= 77` on the old row are blank now.
                    let first_blank = 78 - wrapped;
                    if since >= Duration::from_millis(1000) {
                        let past: Vec<u16> =
                            old.iter().copied().filter(|c| *c >= first_blank).collect();
                        if !past.is_empty() && law_hits[1] < 4 {
                            law_hits[1] += 1;
                            violations.push(format!(
                                "wrap+{:.2}s: row {old_row} still lit PAST its last glyph (cols {first_blank}..=77 are blank since the re-wrap): {past:?}",
                                since.as_secs_f32()
                            ));
                        }
                    }
                    // (0) **THE BAND SPANS THE WRAP** (2026-09-15, the
                    // owner: *"awkward transitions when going to a new line
                    // still with the rainbow"* — his screenshot with the
                    // first row bare and the second coloured). A second
                    // after the wrap, with the hand typing on the new row,
                    // the row it wrapped off STILL CARRIES the light it was
                    // typed with: wrapping is one input continuing, and the
                    // glyphs on that row have not moved. Under the shipped
                    // abandon that row was empty inside 0.7 s.
                    if (Duration::from_millis(1000)..Duration::from_millis(1200)).contains(&since)
                        && old.is_empty()
                        && law_hits[3] < 4
                    {
                        law_hits[3] += 1;
                        violations.push(format!(
                            "wrap+{:.2}s: row {old_row} is DARK although its text is still on glass — the band does not span the wrap",
                            since.as_secs_f32()
                        ));
                    }
                    // (1) the old row's light is gone within ~2 s of the
                    // wrap — the hand left the row and no key on the new one
                    // renews it. Measured 2026-09-15: lit to +1.7 s, its own
                    // swoosh; under the shipped abandon, +0.7 s.
                    if since >= Duration::from_millis(2000) && !old.is_empty() && law_hits[0] < 4 {
                        law_hits[0] += 1;
                        violations.push(format!(
                            "wrap+{:.2}s: row {old_row} (the row the hand left) still carries light: {}",
                            since.as_secs_f32(),
                            ranges(&old)
                        ));
                    }
                }
                WrapShape::BoxGrowsUpNoScroll => {
                    // The light must be where the text is: on the caret's
                    // row nothing lit beyond the caret (the old line's cells
                    // now sit under blanks / the new text).
                    if since >= Duration::from_millis(1000) {
                        let beyond: BTreeSet<u16> =
                            new.iter().copied().filter(|c| *c >= caret.1).collect();
                        if !beyond.is_empty() && law_hits[0] < 4 {
                            law_hits[0] += 1;
                            violations.push(format!(
                                "wrap+{:.2}s: row {new_row} lit beyond the caret {:?} — the old line's cells left on the caret's row: {}",
                                since.as_secs_f32(),
                                caret,
                                ranges(&beyond)
                            ));
                        }
                    }
                }
            }
            // (3) the new row is contiguous from the inset column while it
            // is being typed (from the second key on the new row, so a
            // one-tick-late echo is not counted).
            if hand_on_new_row && caret.1 > INSET + wrapped && since >= Duration::from_millis(100) {
                let dark: Vec<u16> = (INSET..caret.1).filter(|c| !new.contains(c)).collect();
                if !dark.is_empty() && law_hits[2] < 4 {
                    law_hits[2] += 1;
                    violations.push(format!(
                        "wrap+{:.2}s: row {new_row} being typed (caret {:?}) has dark cells inside its span from the inset column: {dark:?}",
                        since.as_secs_f32(),
                        caret
                    ));
                }
            }
        }

        // Keys: press when the clock reaches the next key's time.
        if let Some((_, (_, _))) = keys_iter.peek() {
            let next_key_at = key_at + Duration::from_millis(KEY_MS);
            if now + tick > next_key_at {
                let (_, (ch, echoes)) = keys_iter.next().unwrap();
                key_at = next_key_at;
                let class = if ch == ' ' {
                    rk::TypedClass::Space
                } else {
                    rk::TypedClass::Glyph
                };
                glow.supersede_typed_press();
                glow.note_typed_glyph(key_at, 1, false, class);
                for (delay, to, scroll) in echoes {
                    pending.push(Echo {
                        at: key_at + Duration::from_millis(delay),
                        to,
                        scroll,
                    });
                }
            }
        } else if let Some(w) = wrap_echo
            && now >= w + idle_until_after_wrap
        {
            break;
        }
        if now > t0 + Duration::from_secs(30) {
            panic!("runaway replay");
        }
    }
    log.push(String::new());
    log.push("ring around the wrap:".to_string());
    log.extend(ring_snap);
    Replay { log, violations }
}

fn run(shape: WrapShape) {
    let r = replay(shape);
    eprintln!(
        "=== ink wrap seam trace: {shape:?} (old row = {}, new row = {ROW}) ===",
        match shape {
            WrapShape::BoxGrowsUpNoScroll => ROW,
            _ => ROW - 1,
        }
    );
    for l in &r.log {
        eprintln!("{l}");
    }
    eprintln!();
    eprintln!("violations ({}):", r.violations.len());
    for v in &r.violations {
        eprintln!("  {v}");
    }
    assert!(
        r.violations.is_empty(),
        "{shape:?}: the streak does not follow the caret onto the new row:\n{}",
        r.violations.join("\n")
    );
}

/// The measured Claude Code shape: the bottom-anchored box grows a row up,
/// the screen scrolls one line in the same repaint, one observed move.
#[test]
fn an_ink_wrap_observed_as_one_scrolled_move_lets_the_streak_follow_the_caret_down() {
    run(WrapShape::ScrolledOneMove);
}

/// The scrolled shape with a one-letter wrapped word (landing col 3).
#[test]
fn an_ink_wrap_of_a_one_letter_word_observed_as_one_scrolled_move_lets_the_streak_follow_the_caret_down()
 {
    run(WrapShape::ScrolledShortWord);
}

/// Ink's two-move rewrite across the wrap (end → inset column, then inset
/// column → the wrapped word's end).
#[test]
fn an_ink_wrap_observed_as_two_scrolled_moves_lets_the_streak_follow_the_caret_down() {
    run(WrapShape::ScrolledTwoMove);
}

/// The box grows up without a scroll (the box above the screen's bottom, or
/// the alt screen): the caret's terminal row is constant and the old text
/// moved up without the host being told.
#[test]
fn an_ink_wrap_observed_as_a_same_row_re_anchor_keeps_the_light_under_the_text() {
    run(WrapShape::BoxGrowsUpNoScroll);
}
