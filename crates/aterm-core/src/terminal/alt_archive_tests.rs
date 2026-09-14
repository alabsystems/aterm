// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unit tests for the alt-screen archive: the pure differ over `&[&str]`-style
//! frames (every rule of the spec, and the design's section-D cases), the
//! streaming DECRST matcher, and the `Terminal` hook.

use super::*;
use crate::terminal::ClockReading;

const COLS: u16 = 100;

/// A distinctive transcript row.
fn tx(i: usize) -> String {
    format!("⏺ transcript row {i:04} with words")
}

/// The same transcript with a blank separator every 7 rows (3, 10, 17, …).
fn txb(i: usize) -> String {
    if i % 7 == 3 { String::new() } else { tx(i) }
}

/// A composer box and footer: fixed bottom chrome.
const CHROME: [&str; 5] = [
    "",
    "╭──────────────────────────╮",
    "│ > type here              │",
    "╰──────────────────────────╯",
    "  ? for shortcuts",
];

fn frame_with(row: fn(usize) -> String, start: usize, t: usize, chrome: &[&str]) -> Vec<String> {
    (start..start + t)
        .map(row)
        .chain(chrome.iter().map(|c| (*c).to_string()))
        .collect()
}

fn frame(start: usize, t: usize) -> Vec<String> {
    frame_with(tx, start, t, &CHROME)
}

fn commit(a: &mut AltArchive, rows: &[String]) {
    a.commit_rows(rows, COLS);
}

fn range(row: fn(usize) -> String, r: std::ops::Range<usize>) -> Vec<String> {
    r.map(row).collect()
}

fn gap_kinds(a: &AltArchive) -> Vec<(u64, AltArchiveGapKind)> {
    a.gaps().map(|g| (g.after, g.kind)).collect()
}

// ------------------------------------------------------------------ differ

#[test]
fn streaming_append_is_exact_for_k_1_2_3() {
    for k in 1..=3 {
        for row in [tx as fn(usize) -> String, txb] {
            let mut a = AltArchive::new();
            for step in 0..=12 {
                commit(&mut a, &frame_with(row, step * k, 20, &CHROME));
            }
            assert_eq!(a.texts(), range(row, 0..12 * k), "k={k}");
            assert_eq!(a.gaps().count(), 0, "k={k}: a plain scroll has no gap");
            assert_eq!(a.lost(), 0);
            assert_eq!(a.back(), 0);
            assert_eq!(a.last(), 12 * k as u64, "indices start at 1");
        }
    }
}

#[test]
fn streaming_append_is_exact_for_mixed_steps() {
    let mut a = AltArchive::new();
    let mut start = 0;
    commit(&mut a, &frame_with(txb, start, 24, &CHROME));
    for k in [1, 2, 3, 1, 3, 2, 1, 1, 3, 2, 2, 1] {
        start += k;
        commit(&mut a, &frame_with(txb, start, 24, &CHROME));
    }
    assert_eq!(a.texts(), range(txb, 0..start));
    assert_eq!(a.gaps().count(), 0);
}

#[test]
fn twenty_row_shift_with_overlap_archives_exactly_twenty() {
    let mut a = AltArchive::new();
    commit(&mut a, &frame(0, 30));
    commit(&mut a, &frame(20, 30));
    assert_eq!(a.texts(), range(tx, 0..20));
    assert_eq!(a.gaps().count(), 0);
}

#[test]
fn twenty_row_jump_without_overlap_flushes_and_records_a_gap() {
    let mut a = AltArchive::new();
    commit(&mut a, &frame(0, 20));
    commit(&mut a, &frame(40, 20));
    // The whole old screen was displayed: flushed, not lost. Rows 20..40 were
    // never on screen, and the gap says the next rows do not continue these.
    assert_eq!(a.texts(), range(tx, 0..20));
    assert_eq!(gap_kinds(&a), vec![(20, AltArchiveGapKind::Jump)]);
    commit(&mut a, &frame(41, 20));
    let mut want = range(tx, 0..20);
    want.push(tx(40));
    assert_eq!(a.texts(), want);
}

#[test]
fn back_five_then_forward_eight_archives_three_without_duplicates() {
    let mut a = AltArchive::new();
    for s in 0..=30 {
        commit(&mut a, &frame(s, 20));
    }
    assert_eq!(a.texts(), range(tx, 0..30));
    commit(&mut a, &frame(25, 20));
    assert_eq!(a.back(), 5, "the top five rows are archived ones re-shown");
    assert_eq!(a.back_at(), 26, "tx(25) is archived index 26");
    assert_eq!(
        a.texts(),
        range(tx, 0..30),
        "scrolling back archives nothing"
    );
    commit(&mut a, &frame(33, 20));
    assert_eq!(
        a.texts(),
        range(tx, 0..33),
        "exactly three new rows, no duplicate"
    );
    assert_eq!(a.back(), 0);
}

#[test]
fn pageup_one_screen_then_forward_line_by_line_has_no_duplicates() {
    let mut a = AltArchive::new();
    for s in 0..=60 {
        commit(&mut a, &frame(s, 20));
    }
    // PageUp by a whole screen: no overlap, so it reads as a jump. The screen
    // that was showing is flushed; the reanchor finds the new top inside it.
    commit(&mut a, &frame(40, 20));
    assert_eq!(a.texts(), range(tx, 0..80));
    assert_eq!(a.back(), 20);
    assert_eq!(a.back_at(), 41);
    for s in 41..=100 {
        commit(&mut a, &frame(s, 20));
    }
    assert_eq!(
        a.texts(),
        range(tx, 0..100),
        "no row archived twice, none lost"
    );
    assert_eq!(gap_kinds(&a), vec![(80, AltArchiveGapKind::Jump)]);
}

#[test]
fn two_page_ups_then_forward_has_no_duplicates() {
    let mut a = AltArchive::new();
    for s in 0..=60 {
        commit(&mut a, &frame(s, 20));
    }
    // Back by 10 twice (overlapping moves): the second exceeds what is on screen
    // relative to the archive's end, which a debt capped at the screen height
    // would forget.
    commit(&mut a, &frame(50, 20));
    commit(&mut a, &frame(40, 20));
    assert_eq!(a.back(), 20);
    for s in 41..=70 {
        commit(&mut a, &frame(s, 20));
    }
    assert_eq!(a.texts(), range(tx, 0..70));
    assert_eq!(a.gaps().count(), 0);
}

#[test]
fn fixed_bottom_chrome_is_never_archived() {
    let chrome6 = [
        "",
        "╭───────────────╮",
        "│ > draft text  │",
        "╰───────────────╯",
        "  ⏵⏵ accept edits on",
        "  ? for shortcuts",
    ];
    let mut a = AltArchive::new();
    for s in 0..=10 {
        commit(&mut a, &frame_with(tx, s, 18, &chrome6));
    }
    assert_eq!(a.texts(), range(tx, 0..10));
    // Leaving flushes the scrolling region of the last frame — never the chrome.
    a.leave();
    assert_eq!(a.texts(), range(tx, 0..28));
    assert_eq!(gap_kinds(&a), vec![(28, AltArchiveGapKind::Leave)]);
}

#[test]
fn spinner_only_edits_archive_nothing() {
    let spin = ["✻", "✢", "✳", "✶", "✻", "✽"];
    let with_spinner = |start: usize, n: usize| {
        let mut f = range(tx, start..start + 16);
        f.push(format!(
            "{} Crunching… ({n}s · esc to interrupt)",
            spin[n % spin.len()]
        ));
        f.extend(CHROME.iter().map(|c| (*c).to_string()));
        f
    };
    let mut a = AltArchive::new();
    for n in 0..10 {
        commit(&mut a, &with_spinner(0, n));
    }
    assert!(a.is_empty(), "a ticking spinner is an in-place edit");
    // A tool row finishing in place is an edit too.
    let mut f = with_spinner(0, 10);
    f[5] = "⏺ Bash(cargo test) done".to_string();
    commit(&mut a, &f);
    assert!(a.is_empty());
    // Shifts with the spinner still ticking archive the transcript only.
    for (n, s) in (1..=5).enumerate() {
        commit(&mut a, &with_spinner(s, 11 + n));
    }
    assert_eq!(a.texts(), range(tx, 0..5));
    assert!(a.texts().iter().all(|r| !r.contains("Crunching")));
}

#[test]
fn blank_start_archives_nothing_until_the_first_real_shift() {
    let t = 18;
    let grow = |n: usize| {
        let mut f = vec![String::new(); t - n.min(t)];
        f.extend((n.saturating_sub(t)..n).map(tx));
        f.extend(CHROME.iter().map(|c| (*c).to_string()));
        f
    };
    let mut a = AltArchive::new();
    for n in 0..=t {
        commit(&mut a, &grow(n));
    }
    assert!(a.is_empty(), "filling a blank screen scrolls nothing off");
    assert_eq!(a.gaps().count(), 0);
    commit(&mut a, &grow(t + 1));
    assert_eq!(a.texts(), vec![tx(0)]);
    commit(&mut a, &grow(t + 3));
    assert_eq!(a.texts(), range(tx, 0..3));
}

#[test]
fn blank_start_painted_all_at_once_archives_nothing() {
    let mut a = AltArchive::new();
    let mut blank = vec![String::new(); 18];
    blank.extend(CHROME.iter().map(|c| (*c).to_string()));
    commit(&mut a, &blank);
    commit(&mut a, &frame(0, 18));
    assert!(a.is_empty());
    assert_eq!(
        a.gaps().count(),
        0,
        "a gap before the first row says nothing"
    );
    commit(&mut a, &frame(2, 18));
    assert_eq!(a.texts(), range(tx, 0..2));
}

#[test]
fn blank_area_shrinking_mid_archive_archives_nothing() {
    let mut a = AltArchive::new();
    for s in 0..=4 {
        commit(&mut a, &frame(s, 18));
    }
    assert_eq!(a.texts(), range(tx, 0..4));
    // The app clears (a jump, flushed) and then grows from the bottom again.
    let grow = |n: usize| {
        let mut f = vec![String::new(); 18 - n];
        f.extend((100..100 + n).map(tx));
        f.extend(CHROME.iter().map(|c| (*c).to_string()));
        f
    };
    for n in [0, 2, 3, 5, 8] {
        commit(&mut a, &grow(n));
    }
    assert_eq!(a.texts(), range(tx, 0..22));
    assert_eq!(gap_kinds(&a), vec![(22, AltArchiveGapKind::Jump)]);
}

#[test]
fn wholesale_redraw_and_back_reanchors_without_duplicates() {
    let view = |i: usize| format!("  ⎿ expanded detail {i:03} of the tool output");
    let mut a = AltArchive::new();
    for s in 0..=30 {
        commit(&mut a, &frame(s, 20));
    }
    // Toggle to a different view: a jump; the old screen is flushed.
    commit(&mut a, &frame_with(view, 0, 20, &CHROME));
    assert_eq!(a.texts(), range(tx, 0..50));
    assert_eq!(gap_kinds(&a), vec![(50, AltArchiveGapKind::Jump)]);
    assert_eq!(a.back(), 0);
    // And back: the other view is flushed, and the reanchor finds the screen's
    // top in the archive.
    commit(&mut a, &frame(30, 20));
    assert_eq!(a.len(), 70);
    assert_eq!(
        gap_kinds(&a),
        vec![(50, AltArchiveGapKind::Jump), (70, AltArchiveGapKind::Jump)]
    );
    assert_eq!(a.back(), 20, "reanchor restored the debt");
    assert_eq!(a.back_at(), 31);
    for s in 31..=60 {
        commit(&mut a, &frame(s, 20));
    }
    let mut want = range(tx, 0..50);
    want.extend(range(view, 0..20));
    want.extend(range(tx, 50..60));
    assert_eq!(
        a.texts(),
        want,
        "rows 30..50 were not archived a second time"
    );
}

#[test]
fn all_rule_rows_make_no_false_shift() {
    let boxes = |phase: usize| -> Vec<String> {
        (0..24)
            .map(|r| match (r + phase) % 4 {
                0 => "│                  │".to_string(),
                1 => "├──────────────────┤".to_string(),
                2 => "│ ║ │ ║ │".to_string(),
                _ => "╰──────╯".to_string(),
            })
            .collect()
    };
    let mut a = AltArchive::new();
    for phase in 0..8 {
        commit(&mut a, &boxes(phase));
    }
    assert!(a.is_empty());
    assert_eq!(a.gaps().count(), 0);
}

#[test]
fn a_pinned_header_does_not_block_archiving() {
    let mut a = AltArchive::new();
    let with_header = |start: usize| {
        let mut f = vec!["== fake claude ==".to_string(), String::new()];
        f.extend(frame_with(txb, start, 18, &CHROME));
        f
    };
    for s in 0..=30 {
        commit(&mut a, &with_header(s));
    }
    assert_eq!(a.texts(), range(txb, 0..30));
    assert_eq!(a.read(AltArchiveQuery::oldest(0, 10)).pin, 2);
}

#[test]
fn identical_frames_do_no_work() {
    let mut a = AltArchive::new();
    commit(&mut a, &frame(0, 20));
    let epoch = a.epoch();
    for _ in 0..5 {
        commit(&mut a, &frame(0, 20));
    }
    assert!(a.is_empty());
    assert_eq!(a.epoch(), epoch);
}

#[test]
fn eviction_counts_lost_and_advances_first() {
    let cost = tx(0).len() + ALT_ARCHIVE_ROW_OVERHEAD;
    let mut a = AltArchive::with_limits(10 * cost, ALT_ARCHIVE_MAX_ROWS);
    for s in 0..=25 {
        commit(&mut a, &frame(s, 20));
    }
    assert_eq!(a.len(), 10);
    assert_eq!(a.lost(), 15);
    assert_eq!(a.oldest(), 16);
    assert_eq!(a.last(), 25);
    assert_eq!(a.texts(), range(tx, 15..25));
    let r = a.read(AltArchiveQuery::oldest(0, 100));
    assert_eq!(r.lost, 15, "rows 1..=15 after since=0 are gone");
    assert_eq!(r.first, 16);
    let r = a.read(AltArchiveQuery::oldest(20, 100));
    assert_eq!(r.lost, 0);
    assert_eq!(r.rows.len(), 5);

    // The row cap applies regardless of bytes.
    let mut b = AltArchive::with_limits(usize::MAX, 7);
    for s in 0..=20 {
        commit(&mut b, &frame(s, 20));
    }
    assert_eq!(b.len(), 7);
    assert_eq!(b.lost(), 13);

    // Gaps below the retained window are dropped with their rows.
    let mut c = AltArchive::with_limits(5 * cost, ALT_ARCHIVE_MAX_ROWS);
    commit(&mut c, &frame(0, 20));
    commit(&mut c, &frame(3, 20));
    commit(&mut c, &frame(60, 20)); // jump: flush, gap
    assert!(c.gaps().count() == 1);
    commit(&mut c, &frame(61, 20));
    for s in 62..=70 {
        commit(&mut c, &frame(s, 20));
    }
    assert_eq!(c.gaps().count(), 0);
}

#[test]
fn resize_flushes_the_old_screen_and_records_a_gap() {
    let mut a = AltArchive::new();
    for s in 0..=10 {
        commit(&mut a, &frame(s, 20));
    }
    let epoch = a.epoch();
    a.commit_rows(&frame(30, 20), COLS - 10); // cols changed
    assert_eq!(
        a.texts(),
        range(tx, 0..30),
        "the old screen's region, no chrome"
    );
    assert_eq!(gap_kinds(&a), vec![(30, AltArchiveGapKind::Resize)]);
    assert_eq!(a.epoch(), epoch + 1);
    a.commit_rows(&frame(31, 20), COLS - 10);
    assert_eq!(a.texts(), range(tx, 0..31));
    // A rows-only change rebaselines too.
    a.commit_rows(&frame(31, 22), COLS - 10);
    assert_eq!(a.texts(), range(tx, 0..51));
    assert_eq!(a.gaps().count(), 2);
}

#[test]
fn leave_flushes_without_chrome_and_enter_starts_fresh() {
    let mut a = AltArchive::new();
    for s in 0..=3 {
        commit(&mut a, &frame(s, 20));
    }
    let epoch = a.epoch();
    a.leave();
    assert_eq!(a.texts(), range(tx, 0..23));
    assert_eq!(gap_kinds(&a), vec![(23, AltArchiveGapKind::Leave)]);
    assert_eq!(a.epoch(), epoch + 1);
    a.enter();
    assert_eq!(a.epoch(), epoch + 2);
    // A new app: its first frame is a baseline, its first shift archives.
    commit(&mut a, &frame(500, 20));
    commit(&mut a, &frame(501, 20));
    let mut want = range(tx, 0..23);
    want.push(tx(500));
    assert_eq!(a.texts(), want);
}

#[test]
fn wipe_counts_rows_lost_and_marks_a_reset() {
    let mut a = AltArchive::new();
    for s in 0..=10 {
        commit(&mut a, &frame(s, 20));
    }
    a.wipe();
    assert!(a.is_empty());
    assert_eq!(a.lost(), 10);
    assert_eq!(a.last(), 10, "indices keep increasing across a wipe");
    assert_eq!(a.oldest(), 11);
    assert_eq!(gap_kinds(&a), vec![(10, AltArchiveGapKind::Reset)]);
    let r = a.read(AltArchiveQuery::oldest(4, 100));
    assert_eq!(r.lost, 6);
    assert_eq!(r.gaps.len(), 1);
    commit(&mut a, &frame(0, 20));
    commit(&mut a, &frame(1, 20));
    assert_eq!(a.texts(), vec![tx(0)]);
    assert_eq!(a.oldest(), 11);
}

#[test]
fn consecutive_jumps_with_nothing_between_record_one_gap() {
    let mut a = AltArchive::new();
    commit(&mut a, &frame(0, 20));
    let mut blank = vec![String::new(); 20];
    blank.extend(CHROME.iter().map(|c| (*c).to_string()));
    commit(&mut a, &blank); // everything vanished: flush + gap
    commit(&mut a, &frame(300, 20)); // painted from blank: nothing to flush
    assert_eq!(a.texts(), range(tx, 0..20));
    assert_eq!(gap_kinds(&a), vec![(20, AltArchiveGapKind::Jump)]);
}

#[test]
fn read_pages_forward_and_tails() {
    let mut a = AltArchive::new();
    for s in 0..=50 {
        commit(&mut a, &frame(s, 20));
    }
    a.set_origin(0xabc);
    let r = a.read(AltArchiveQuery::oldest(0, 20));
    assert_eq!((r.first, r.last, r.rows.len(), r.more), (1, 50, 20, true));
    assert_eq!(
        r.page_last(),
        20,
        "the next page starts after the page, not at last"
    );
    assert_eq!(&*r.rows[0], tx(0));
    assert_eq!(r.origin, 0xabc);
    let r = a.read(AltArchiveQuery::oldest(40, 20));
    assert_eq!((r.first, r.rows.len(), r.more), (41, 10, false));
    assert_eq!(r.page_last(), 50);
    let r = a.read(AltArchiveQuery::newest(0, 5));
    assert_eq!((r.first, r.rows.len(), r.more), (46, 5, true));
    assert_eq!(&*r.rows[4], tx(49));
    let r = a.read(AltArchiveQuery::oldest(50, 20));
    assert!(r.rows.is_empty());
    assert_eq!((r.first, r.last, r.more), (51, 50, false));
    assert_eq!(r.page_last(), 50);
    let r = a.read(AltArchiveQuery::oldest(99, 20));
    assert!(r.rows.is_empty(), "since above last is clamped");
}

#[test]
fn control_chars_are_collapsed_like_the_screen_reply() {
    let mut s = String::from("a\u{1}b\u{85}c\t  ");
    normalize_row(&mut s);
    assert_eq!(s, "a b c");
    let mut s = String::from("middle · dot  ");
    normalize_row(&mut s);
    assert_eq!(s, "middle · dot");
}

#[test]
fn a_budget_of_zero_turns_it_off_and_wipes() {
    let mut a = AltArchive::new();
    for s in 0..=5 {
        commit(&mut a, &frame(s, 20));
    }
    a.set_budget(0);
    assert!(!a.enabled());
    assert!(a.is_empty());
    assert_eq!(a.lost(), 5);
    for s in 6..=9 {
        commit(&mut a, &frame(s, 20));
    }
    assert!(a.is_empty(), "off records nothing");
    a.set_enabled(true);
    commit(&mut a, &frame(9, 20));
    commit(&mut a, &frame(10, 20));
    assert_eq!(a.texts(), vec![tx(9)]);
}

// ----------------------------------------------------------------- matcher

fn hits_on(m: &mut DecModeMatcher, mut bytes: &[u8], on_alt: bool) -> Vec<(usize, HitKind)> {
    let mut out = Vec::new();
    let mut base = 0;
    while let Some(h) = m.scan(bytes, on_alt) {
        out.push((base + h.final_at, h.kind));
        base += h.final_at + 1;
        bytes = &bytes[h.final_at + 1..];
    }
    out
}

/// On the alt screen: `(final byte offset, is an exit)`.
fn hits(m: &mut DecModeMatcher, bytes: &[u8]) -> Vec<(usize, bool)> {
    hits_on(m, bytes, true)
        .into_iter()
        .map(|(i, k)| (i, k == HitKind::Exit))
        .collect()
}

#[test]
fn matcher_finds_esu_and_alt_exit() {
    let cases: &[(&[u8], Vec<(usize, bool)>)] = &[
        (b"\x1b[?2026l", vec![(7, false)]),
        (b"ab\x1b[?2026lcd\x1b[?2026l", vec![(9, false), (19, false)]),
        (b"\x1b[?25;2026l", vec![(10, false)]),
        (b"\x1b[?1049l", vec![(7, true)]),
        (b"\x1b[?47l\x1b[?1047l", vec![(5, true), (13, true)]),
        (b"\x1b[?2026;1049l", vec![(12, true)]),
        (b"\x1b[?2026h", vec![]),
        (b"\x1b[?2026$p", vec![]),
        (b"\x1b[2026l", vec![]),
        (b"\x1b[?10490l", vec![]),
        (b"\x1b[?202\x186l", vec![]),               // CAN aborts
        (b"\x1b[?1\x1b[?2026l", vec![(11, false)]), // ESC restarts
        (b"\x1b[?20\n26l", vec![(8, false)]),       // C0 executes inside a CSI
        (b"\x1b]0;\x1b[?2026l", vec![(11, false)]), // ESC ends the OSC
    ];
    for (bytes, want) in cases {
        let mut m = DecModeMatcher::default();
        assert_eq!(
            &hits(&mut m, bytes),
            want,
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
    }
}

/// On the MAIN screen only an alt-screen enter splits (main-screen frames
/// commit nothing, so their ESUs cost nothing); XTRESTORE of an alt mode is an
/// exit candidate on the alt screen and an enter candidate on the main one.
#[test]
fn matcher_reports_by_screen_enters_and_xtrestore() {
    let main: &[(&[u8], Vec<(usize, HitKind)>)] = &[
        (b"\x1b[?2026l", vec![]),
        (b"\x1b[?1049l", vec![]),
        (b"\x1b[?1049h", vec![(7, HitKind::Enter)]),
        (b"ab\x1b[?25;47h", vec![(10, HitKind::Enter)]),
        (b"\x1b[?1049r", vec![(7, HitKind::Enter)]),
        (b"\x1b[?2026h\x1b[?25h", vec![]),
    ];
    for (bytes, want) in main {
        let mut m = DecModeMatcher::default();
        assert_eq!(
            &hits_on(&mut m, bytes, false),
            want,
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
    }
    let alt: &[(&[u8], Vec<(usize, HitKind)>)] = &[
        (b"\x1b[?1049h", vec![]),
        (b"\x1b[?1049r", vec![(7, HitKind::Restore(ALT_1049))]),
        (
            b"\x1b[?47;1047r",
            vec![(10, HitKind::Restore(ALT_47 | ALT_1047))],
        ),
        (b"\x1b[?2026r", vec![]),
    ];
    for (bytes, want) in alt {
        let mut m = DecModeMatcher::default();
        assert_eq!(
            &hits_on(&mut m, bytes, true),
            want,
            "{:?}",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn matcher_survives_a_split_at_every_byte() {
    let seq = b"xx\x1b[?2026lyy";
    for cut in 0..=seq.len() {
        let mut m = DecModeMatcher::default();
        let mut found = hits(&mut m, &seq[..cut]);
        found.extend(
            hits(&mut m, &seq[cut..])
                .into_iter()
                .map(|(i, e)| (i + cut, e)),
        );
        assert_eq!(found, vec![(9, false)], "cut at {cut}");
    }
    // The main screen's `?`-to-`?` scan: `ESC [` carried across the cut.
    let enter = b"x?\x1b[?1049hyy";
    for cut in 0..=enter.len() {
        let mut m = DecModeMatcher::default();
        let mut found = hits_on(&mut m, &enter[..cut], false);
        found.extend(
            hits_on(&mut m, &enter[cut..], false)
                .into_iter()
                .map(|(i, k)| (i + cut, k)),
        );
        assert_eq!(found, vec![(9, HitKind::Enter)], "cut at {cut}");
    }
}

// --------------------------------------------------------- terminal hook

/// A Claude-shaped frame: sync open, cursor hidden, every row painted with CUP +
/// text + EL, cursor shown, sync closed.
fn sync_frame(rows: &[String]) -> Vec<u8> {
    let mut v = b"\x1b[?2026h\x1b[?25l".to_vec();
    for (r, text) in rows.iter().enumerate() {
        v.extend_from_slice(format!("\x1b[{};1H{text}\x1b[K", r + 1).as_bytes());
    }
    v.extend_from_slice(b"\x1b[?25h\x1b[?2026l");
    v
}

/// The same paint with no 2026 bracket (vim/less style).
fn plain_frame(rows: &[String]) -> Vec<u8> {
    let mut v = Vec::new();
    for (r, text) in rows.iter().enumerate() {
        v.extend_from_slice(format!("\x1b[{};1H{text}\x1b[K", r + 1).as_bytes());
    }
    v
}

fn term() -> Terminal {
    let mut t = Terminal::new(25, COLS);
    t.set_alt_archive_enabled(true);
    t
}

#[test]
fn many_frames_in_one_read_are_each_committed() {
    let mut one = term();
    let mut each = term();
    let mut all = b"\x1b[?1049h".to_vec();
    each.process(b"\x1b[?1049h");
    for s in 0..=15 {
        let f = sync_frame(&frame(s, 20));
        all.extend_from_slice(&f);
        each.process(&f);
    }
    one.process(&all);
    assert_eq!(one.alt_archive().texts(), range(tx, 0..15));
    assert_eq!(one.alt_archive().texts(), each.alt_archive().texts());
    assert_eq!(one.visible_content(), each.visible_content());
}

/// The main screen's 2026 frames are not split (they commit nothing), yet an
/// alt-screen enter behind them in the SAME read still splits there: the alt
/// frames after it are each committed, and the main screen's closes neither
/// commit on the alt screen nor mark its run as 2026-paced.
#[test]
fn an_enter_behind_main_screen_frames_in_one_read_still_splits() {
    let mut t = term();
    let mut v = Vec::new();
    for i in 0..5 {
        v.extend_from_slice(format!("\x1b[?2026hmain {i}\r\n\x1b[?2026l").as_bytes());
    }
    v.extend_from_slice(b"\x1b[?1049h");
    for s in 0..=6 {
        v.extend_from_slice(&sync_frame(&frame(s, 20)));
    }
    t.process(&v);
    assert_eq!(t.alt_archive().texts(), range(tx, 0..6));
    assert_eq!(t.alt_archive().gaps().count(), 0);
}

/// XTRESTORE of 1049 that restores "on" while on the alt screen does not leave
/// it, so nothing is committed early — not even inside an open 2026 window,
/// where the grid is half painted.
#[test]
fn an_xtrestore_that_stays_on_the_alt_screen_commits_nothing_early() {
    let mut t = term();
    t.process(b"\x1b[?1049h\x1b[?1049s"); // saved: on
    for s in 0..=3 {
        t.process(&sync_frame(&frame(s, 20)));
    }
    let mut torn = sync_frame(&frame(9, 20));
    let half = torn.len() / 2;
    torn.splice(half..half, b"\x1b[?1049r".iter().copied());
    t.process(&torn);
    assert!(t.modes().alternate_screen);
    // Frame 9 shifts frame 3 up by 6, judged at its close only.
    assert_eq!(t.alt_archive().texts(), range(tx, 0..9));
    assert_eq!(t.alt_archive().gaps().count(), 0);
}

#[test]
fn alt_exit_commits_what_was_drawn_after_the_last_esu() {
    let mut t = term();
    t.process(b"\x1b[?1049h");
    for s in 0..=5 {
        t.process(&sync_frame(&frame(s, 20)));
    }
    // Two more rows scroll by with no closing ESU, then the app exits in the
    // same read.
    let mut tail = plain_frame(&frame(7, 20));
    tail.extend_from_slice(b"\x1b[?1049l");
    t.process(&tail);
    assert!(!t.modes().alternate_screen);
    assert_eq!(
        t.alt_archive().texts(),
        range(tx, 0..27),
        "5..7 scrolled, 7..27 flushed"
    );
    let gaps: Vec<_> = t.alt_archive().gaps().collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind, AltArchiveGapKind::Leave);
    assert!(
        t.alt_archive()
            .texts()
            .iter()
            .all(|r| !r.contains("shortcuts"))
    );
}

#[test]
fn ris_and_host_reset_wipe_the_archive() {
    let mut t = term();
    t.process(b"\x1b[?1049h");
    for s in 0..=5 {
        t.process(&sync_frame(&frame(s, 20)));
    }
    assert_eq!(t.alt_archive().len(), 5);
    t.process(b"\x1bc");
    assert!(t.alt_archive().is_empty());
    assert_eq!(t.alt_archive().lost(), 5);
    let gaps: Vec<_> = t.alt_archive().gaps().collect();
    assert_eq!(gaps.last().map(|g| g.kind), Some(AltArchiveGapKind::Reset));

    let mut h = term();
    h.process(b"\x1b[?1049h");
    for s in 0..=4 {
        h.process(&sync_frame(&frame(s, 20)));
    }
    h.reset();
    assert!(h.alt_archive().is_empty());
    assert_eq!(h.alt_archive().lost(), 4);
}

#[test]
fn ris_between_frames_in_one_read_wipes_only_what_came_before() {
    let mut t = term();
    let mut v = b"\x1b[?1049h".to_vec();
    for s in 0..=3 {
        v.extend_from_slice(&sync_frame(&frame(s, 20)));
    }
    v.extend_from_slice(b"\x1bc\x1b[?1049h");
    for s in 100..=102 {
        v.extend_from_slice(&sync_frame(&frame(s, 20)));
    }
    t.process(&v);
    assert_eq!(t.alt_archive().texts(), range(tx, 100..102));
    assert_eq!(t.alt_archive().lost(), 3);
}

#[test]
fn decstr_commits_without_rebaselining() {
    let mut t = term();
    t.process(b"\x1b[?1049h");
    t.process(&sync_frame(&frame(0, 20)));
    let epoch = t.alt_archive().epoch();
    // An open window closed by DECSTR (a soft reset) instead of an ESU.
    let mut f = sync_frame(&frame(3, 20));
    f.truncate(f.len() - b"\x1b[?25h\x1b[?2026l".len());
    f.extend_from_slice(b"\x1b[!p");
    t.process(&f);
    assert_eq!(t.alt_archive().texts(), range(tx, 0..3));
    assert_eq!(
        t.alt_archive().epoch(),
        epoch,
        "DECSTR is a commit, not a rebaseline"
    );
    assert_eq!(t.alt_archive().gaps().count(), 0);
}

#[test]
fn apps_without_2026_are_committed_at_the_epilogue_rate_limited() {
    let mut t = term();
    let base = ClockReading::now();
    let at = |ms: u64| ClockReading {
        monotonic: base
            .monotonic
            .checked_add(std::time::Duration::from_millis(ms))
            .expect("clock"),
        wall_ms: None,
    };
    t.process_at(b"\x1b[?1049h", at(0));
    t.process_at(&plain_frame(&frame(0, 20)), at(20));
    t.process_at(&plain_frame(&frame(1, 20)), at(40));
    assert_eq!(t.alt_archive().texts(), vec![tx(0)]);
    // Within 16 ms of the last commit: not committed yet…
    t.process_at(&plain_frame(&frame(2, 20)), at(45));
    assert_eq!(t.alt_archive().texts(), vec![tx(0)]);
    // …and the next commit sees the whole move.
    t.process_at(&plain_frame(&frame(3, 20)), at(80));
    assert_eq!(t.alt_archive().texts(), range(tx, 0..3));
}

#[test]
fn a_torn_frame_is_never_committed() {
    let mut t = term();
    t.process(b"\x1b[?1049h");
    t.process(&sync_frame(&frame(0, 20)));
    // A torn frame that never closes (the app is mid-paint when the read ends):
    // the fallback must not commit it.
    let mut torn = b"\x1b[?2026h".to_vec();
    torn.extend_from_slice(&plain_frame(&range(tx, 50..60)));
    t.process(&torn);
    assert!(t.alt_archive().is_empty());
    assert_eq!(t.alt_archive().gaps().count(), 0);
}

#[test]
fn main_screen_output_is_never_archived() {
    let mut t = term();
    for s in 0..=10 {
        t.process(&sync_frame(&frame(s, 20)));
        t.process(b"line\r\n");
    }
    assert!(t.alt_archive().is_empty());
    assert_eq!(t.alt_archive().last(), 0);
}

#[test]
fn disabled_archive_records_nothing() {
    let mut t = Terminal::new(25, COLS);
    t.set_alt_archive_enabled(false);
    t.process(b"\x1b[?1049h");
    for s in 0..=5 {
        t.process(&sync_frame(&frame(s, 20)));
    }
    assert!(t.alt_archive().is_empty());
    t.set_alt_archive_enabled(true);
    for s in 6..=8 {
        t.process(&sync_frame(&frame(s, 20)));
    }
    assert_eq!(
        t.alt_archive().texts(),
        range(tx, 6..8),
        "re-enabled mid-session"
    );
}

#[test]
fn restore_checkpoint_rebaselines() {
    let mut t = term();
    t.process(b"\x1b[?1049h");
    for s in 0..=3 {
        t.process(&sync_frame(&frame(s, 20)));
    }
    let cp = t.checkpoint();
    let epoch = t.alt_archive().epoch();
    t.restore_checkpoint(&cp);
    assert_eq!(t.alt_archive().epoch(), epoch + 1);
    assert_eq!(
        t.alt_archive().gaps().last().map(|g| g.kind),
        Some(AltArchiveGapKind::Restore)
    );
    // It keeps following the restored alt screen.
    let n = t.alt_archive().len();
    t.process(&sync_frame(&frame(10, 20)));
    t.process(&sync_frame(&frame(11, 20)));
    assert_eq!(t.alt_archive().len(), n + 1);
}

// ======================================================= ROUND-7 REVIEW
// The adversarial review's cases: each one lost rows (or archived chrome, or
// depended on read boundaries) before the fixes that came with it.
mod adversarial {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    /// A1: the chrome height leave/resize/restore used was the one measured at the
    /// LAST SHIFT, so rows that later changed in place inside that stale chrome band
    /// (a status block replaced by the answer) were never flushed. Every diffed
    /// frame now narrows it.
    #[test]
    fn a1_rows_edited_in_place_in_the_old_chrome_band_are_flushed_on_leave_and_resize() {
        let status = s(&[
            "✻ Working… (esc to interrupt)",
            "  ⎿ ☐ Build the crate",
            "    ☐ Run the tests",
        ]);
        let comp = s(&["╭────────────╮", "│ > draft    │", "╰────────────╯"]);
        let answer = s(&[
            "⏺ The build failed: crate foo's tests do not compile",
            "  on machine m7 after commit abc123 landed there",
            "  — see target/debug for the errors",
        ]);
        let mk = |start: usize, mid: &[String]| -> Vec<String> {
            (start..start + 6)
                .map(tx)
                .chain(mid.iter().cloned())
                .chain(comp.iter().cloned())
                .collect()
        };
        for how in ["leave", "resize"] {
            let mut a = AltArchive::new();
            commit(&mut a, &mk(0, &status));
            commit(&mut a, &mk(1, &status)); // shift by 1: chrome measured = 6
            assert_eq!(a.texts(), range(tx, 0..1));
            // The status block is replaced IN PLACE (the transcript does not move).
            commit(&mut a, &mk(1, &answer));
            if how == "leave" {
                a.leave();
            } else {
                a.commit_rows(&mk(1, &answer), COLS - 10);
            }
            let texts = a.texts();
            for row in &answer {
                assert!(
                    texts.iter().any(|t| t == row),
                    "{how}: displayed row never archived: {row:?}; archive = {texts:#?}"
                );
            }
        }
    }

    /// A2: before the first shift the chrome was 0, so leaving flushed the composer
    /// and footer ("never the composer/footer rows below T"); the frames diffed
    /// before the leave now measure it.
    #[test]
    fn a2_leave_before_any_shift_never_archives_the_composer() {
        let mut a = AltArchive::new();
        let mut rows: Vec<String> = vec![String::new(); 20];
        rows.extend(CHROME.iter().map(|c| (*c).to_string()));
        commit(&mut a, &rows);
        for i in 0..5 {
            rows[i] = tx(i); // a screen filling top-down: rows appear in place
            commit(&mut a, &rows);
        }
        a.leave();
        let texts = a.texts();
        assert!(
            texts
                .iter()
                .all(|r| !r.contains("shortcuts") && !r.contains("type here")),
            "chrome archived on leave: {texts:#?}"
        );
    }

    /// A3: an app that clears the alt screen and then leaves (the common exit path)
    /// made the pre-swap commit a blank frame -> a JUMP whose flush had B == 0, so the
    /// composer and footer were archived. A blank frame is no frame now.
    #[test]
    fn a3_clear_then_exit_never_archives_the_composer() {
        let mut t = term();
        t.process(b"\x1b[?1049h");
        for s in 0..=5 {
            t.process(&sync_frame(&frame(s, 20)));
        }
        t.process(b"\x1b[H\x1b[2J\x1b[?1049l");
        let texts = t.alt_archive().texts();
        assert!(
            texts
                .iter()
                .all(|r| !r.contains("shortcuts") && !r.contains("type here")),
            "chrome archived on a clear+exit: {texts:#?}"
        );
        assert_eq!(texts, range(tx, 0..25));
    }

    /// A4: stable anchors INSIDE [0,T) (a todo list above a composer whose footer
    /// ticks, so B == 0) outvoted the few overlap anchors of a big scroll. Rule 5 read
    /// d == 0 as "still" and archived nothing: 17 displayed rows lost with no gap.
    /// Fixed rows under a block that moved are that frame's chrome now.
    #[test]
    fn a4_fixed_rows_under_a_big_scroll_never_outvote_it() {
        let todo = s(&[
            "  ⎿ ☒ Read the spec",
            "    ☐ Build the crate",
            "    ☐ Run the tests",
            "    ☐ Write the report",
            "    ☐ Commit the work",
            "    ☐ Push the branch",
        ]);
        let comp = s(&["╭────────╮", "│ > typing │", "╰────────╯"]);
        let mk = |start: usize, tick: usize| -> Vec<String> {
            (start..start + 20)
                .map(tx)
                .chain(todo.iter().cloned())
                .chain(comp.iter().cloned())
                .chain(std::iter::once(format!("  ↓ {tick} tokens")))
                .collect()
        };
        let mut a = AltArchive::new();
        commit(&mut a, &mk(0, 100));
        commit(&mut a, &mk(17, 101)); // a 17-row block lands in one frame
        assert_eq!(a.texts(), range(tx, 0..17), "gaps = {:?}", gap_kinds(&a));
    }

    /// A5: a full screen of short rows (<4 visible chars: line numbers, a word list,
    /// `seq` in less) has no anchors, so every change was "sparse" and nothing was ever
    /// archived. Such rows vote (weakly) when anchors cannot, and a screen full of
    /// them is never sparse.
    #[test]
    fn a5_a_full_screen_of_short_rows_is_archived() {
        let mk = |st: usize| -> Vec<String> { (st..st + 20).map(|i| format!("{i}")).collect() };
        let mut a = AltArchive::new();
        commit(&mut a, &mk(100));
        commit(&mut a, &mk(110)); // half a page forward
        commit(&mut a, &mk(130)); // a whole page forward
        assert!(
            !a.is_empty(),
            "rows 100..130 left the screen and are nowhere: {:?} gaps={:?}",
            a.texts(),
            gap_kinds(&a)
        );
    }

    /// A6: rows that leave at the BOTTOM when the app scrolls back (rule 7) were never
    /// recorded; a later jump flushed only what was on screen, so they were lost.
    /// They wait in `below` now, and a flush archives them.
    #[test]
    fn a6_rows_that_left_at_the_bottom_survive_a_later_jump() {
        let mut a = AltArchive::new();
        for st in 0..=10 {
            commit(&mut a, &frame(st, 20)); // archive tx(0..10), screen tx(10..30)
        }
        commit(&mut a, &frame(0, 20)); // scroll back 10: tx(20..30) leave at the bottom
        commit(&mut a, &frame(60, 20)); // jump to live, no overlap
        let texts = a.texts();
        for i in 20..30 {
            assert!(
                texts.contains(&tx(i)),
                "displayed row tx({i}) lost; archive = {} rows, gaps={:?}",
                texts.len(),
                gap_kinds(&a)
            );
        }
    }

    /// A6, the other way: rows that left at the bottom and came back when the
    /// app scrolled forward again are not archived twice — they scroll off the
    /// top in order, once — and a leave with some still below flushes them.
    #[test]
    fn a6_rows_below_that_come_back_are_archived_once_in_order() {
        let mut a = AltArchive::new();
        for st in 0..=10 {
            commit(&mut a, &frame(st, 20));
        }
        commit(&mut a, &frame(0, 20)); // back 10: tx(20..30) below
        commit(&mut a, &frame(4, 20)); // forward 4: tx(20..24) back on screen
        commit(&mut a, &frame(10, 20)); // forward 6 more: all back
        for st in 11..=40 {
            commit(&mut a, &frame(st, 20));
        }
        assert_eq!(a.texts(), range(tx, 0..40), "gaps={:?}", gap_kinds(&a));
        assert_eq!(a.gaps().count(), 0);
        // Back 5, then leave: the five rows below are flushed after the screen.
        commit(&mut a, &frame(35, 20));
        a.leave();
        assert_eq!(a.texts(), range(tx, 0..60));
    }

    /// A5, exactly: short rows that move vote (weakly), so a half page forward
    /// archives exactly the half page that left, with no gap.
    #[test]
    fn a5_short_rows_that_move_are_archived_exactly() {
        let mk = |st: usize| -> Vec<String> { (st..st + 20).map(|i| format!("{i}")).collect() };
        let mut a = AltArchive::new();
        commit(&mut a, &mk(100));
        commit(&mut a, &mk(110));
        commit(&mut a, &mk(113));
        let want: Vec<String> = (100..113).map(|i| format!("{i}")).collect();
        assert_eq!(a.texts(), want);
        assert_eq!(a.gaps().count(), 0);
    }

    /// A7: XTRESTORE (`CSI ? 1049 r`) left the alt screen without the matcher's
    /// one-byte-early split, so the alt grid was swapped away before it was committed.
    #[test]
    fn a7_xtrestore_alt_exit_commits_before_the_swap() {
        let mut t = term();
        t.process(b"\x1b[?1049s\x1b[?1049h"); // XTSAVE (off), then enter
        for st in 0..=5 {
            t.process(&sync_frame(&frame(st, 20)));
        }
        let mut tail = plain_frame(&frame(7, 20));
        tail.extend_from_slice(b"\x1b[?1049r"); // XTRESTORE: back to main
        t.process(&tail);
        assert!(!t.modes().alternate_screen);
        assert_eq!(t.alt_archive().texts(), range(tx, 0..27));
    }

    /// A8: parser equivalence and archive equivalence under slicing for a 2026
    /// stream that also exits (1049l / 1047l / 47l), re-enters, RIS-es
    /// mid-read, soft-resets, and carries OSC / DCS strings (a read ending
    /// right after an enter used to commit a blank baseline and reanchor into
    /// the previous run).
    #[test]
    fn a8_chunked_equivalence_with_exits_ris_decstr_strings() {
        let mut data = b"\x1b[?1049h".to_vec();
        for st in 0..=6 {
            data.extend_from_slice(&sync_frame(&frame(st, 20)));
        }
        data.extend_from_slice(b"\x1b]0;title \x1b[?2026l-ish\x07");
        data.extend_from_slice(b"\x1b[?1049lmain text\r\n\x1b[?1047h");
        for st in 30..=34 {
            data.extend_from_slice(&sync_frame(&frame(st, 20)));
        }
        data.extend_from_slice(b"\x1bP1$qm\x1b\\");
        data.extend_from_slice(b"\x1b[?1047l\x1b[?47h");
        for st in 50..=53 {
            data.extend_from_slice(&sync_frame(&frame(st, 20)));
        }
        data.extend_from_slice(b"\x1b[?47l\x1bc\x1b[?1049h");
        for st in 70..=73 {
            data.extend_from_slice(&sync_frame(&frame(st, 20)));
        }
        let mut f = sync_frame(&frame(76, 20));
        f.truncate(f.len() - b"\x1b[?25h\x1b[?2026l".len());
        f.extend_from_slice(b"\x1b[!p");
        data.extend_from_slice(&f);
        for st in 77..=80 {
            data.extend_from_slice(&sync_frame(&frame(st, 20)));
        }
        data.extend_from_slice(b"\x1b[?2026;1049l after\r\n");
        let clock = ClockReading::now();
        let run = |sizes: &[usize], on: bool| {
            let mut t = Terminal::new(25, COLS);
            t.set_alt_archive_enabled(on);
            let (mut off, mut i) = (0, 0);
            while off < data.len() {
                let end = (off + sizes[i % sizes.len()]).min(data.len());
                t.process_at(&data[off..end], clock);
                off = end;
                i += 1;
            }
            (
                t.alt_archive().texts(),
                t.alt_archive().gaps().collect::<Vec<_>>(),
                t.alt_archive().lost(),
                t.visible_content(),
                (t.grid().cursor().row, t.grid().cursor().col),
                t.modes().alternate_screen,
            )
        };
        let whole = run(&[data.len()], true);
        let raw = run(&[data.len()], false);
        assert_eq!(whole.3, raw.3, "grid with archive on vs off");
        assert_eq!(whole.4, raw.4, "cursor with archive on vs off");
        for size in 1..=17 {
            let got = run(&[size], true);
            assert_eq!(got.3, whole.3, "grid, chunk {size}");
            assert_eq!(got.4, whole.4, "cursor, chunk {size}");
            assert_eq!(got, whole, "archive, chunk {size}");
        }
    }
}
mod property_walk {
    use super::*;
    use std::collections::HashSet;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Random walk of a viewport over a growing transcript: appends (auto-follow),
    /// scroll-backs, scroll-forwards, jumps to live. Invariant after every commit:
    /// every row that left the screen at the TOP is archived.
    fn walk(seed: u64, header: bool, steps: usize) -> Result<(usize, usize), String> {
        const T: usize = 20;
        let mut r = Rng(seed | 1);
        let mut a = AltArchive::new();
        let mut len = T; // doc length
        let mut v = 0usize; // view top
        let mut live = true;
        let mk = |v: usize| -> Vec<String> {
            let mut f = Vec::new();
            if header {
                f.push("══ fake claude · session header ══".to_string());
            }
            f.extend((v..v + T).map(tx));
            f.extend(CHROME.iter().map(|c| (*c).to_string()));
            f
        };
        commit(&mut a, &mk(v));
        let mut prev_v = v;
        for step in 0..steps {
            match r.below(10) {
                0..=4 => {
                    let big = r.below(8) == 0;
                    len += 1 + r.below(if big { 30 } else { 4 }) as usize;
                    if live {
                        v = len - T;
                    }
                }
                5 | 6 => {
                    let j = 1 + r.below(30) as usize;
                    v = v.saturating_sub(j);
                    live = false;
                }
                7 | 8 => {
                    let j = 1 + r.below(30) as usize;
                    v = (v + j).min(len - T);
                    live = v == len - T;
                }
                _ => {
                    v = len - T;
                    live = true;
                }
            }
            commit(&mut a, &mk(v));
            let have: HashSet<String> = a.texts().into_iter().collect();
            for i in prev_v..(prev_v + T).min(v) {
                if !have.contains(&tx(i)) {
                    return Err(format!(
                        "seed {seed} header {header} step {step}: tx({i}) left the top \
                         (view {prev_v}->{v}) but is not archived; gaps={:?} back={}",
                        gap_kinds(&a),
                        a.back()
                    ));
                }
            }
            prev_v = v;
        }
        let texts = a.texts();
        let uniq: HashSet<&String> = texts.iter().collect();
        Ok((texts.len(), texts.len() - uniq.len()))
    }

    #[test]
    fn p1_viewport_random_walk_never_loses_a_row_that_left_the_top() {
        let mut fails = Vec::new();
        let (mut rows, mut dups) = (0, 0);
        for seed in 1..=100u64 {
            for header in [false, true] {
                match walk(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), header, 300) {
                    Ok((n, d)) => {
                        rows += n;
                        dups += d;
                    }
                    Err(e) => fails.push(e),
                }
            }
        }
        eprintln!("rows={rows} duplicates={dups} failures={}", fails.len());
        assert!(
            fails.is_empty(),
            "{} failures, first 5:\n{}",
            fails.len(),
            fails[..fails.len().min(5)].join("\n")
        );
    }
}
mod property_claude_like {
    use super::*;
    use std::collections::HashSet;
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Transcript with blanks, rules and a repeated short row, like real output.
    fn row(i: usize) -> String {
        match i % 9 {
            3 => String::new(),
            6 => "  ────────────────────────────".to_string(),
            8 => "  }".to_string(),
            _ => tx(i),
        }
    }
    fn unique(i: usize) -> bool {
        !matches!(i % 9, 3 | 6 | 8)
    }

    #[test]
    fn p2_claude_like_walk_with_spinner_and_repeats() {
        const T: usize = 30;
        let mut fails = Vec::new();
        let (mut rows, mut dups, mut spinner) = (0usize, 0usize, 0usize);
        for seed in 1..=100u64 {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut a = AltArchive::new();
            let (mut len, mut v, mut live) = (T, 0usize, true);
            let mut tick = 0usize;
            let mk = |v: usize, tick: usize, edit: Option<usize>| -> Vec<String> {
                let mut f: Vec<String> = (v..v + T).map(row).collect();
                if let Some(e) = edit {
                    f[e] = format!("{} (done)", f[e]);
                }
                f.push(String::new());
                f.push(format!("✻ Working… ({}s · esc to interrupt)", tick / 3));
                f.extend(CHROME.iter().map(|c| (*c).to_string()));
                f
            };
            commit(&mut a, &mk(v, 0, None));
            let mut prev_v = v;
            for step in 0..300 {
                tick += 1;
                let mut edit = None;
                match r.below(12) {
                    0..=5 => {
                        let big = r.below(8) == 0;
                        len += 1 + r.below(if big { 40 } else { 4 }) as usize;
                        if live {
                            v = len - T;
                        }
                    }
                    6 => {
                        let j = 1 + r.below(40) as usize;
                        v = v.saturating_sub(j);
                        live = false;
                    }
                    7 => {
                        let j = 1 + r.below(40) as usize;
                        v = (v + j).min(len - T);
                        live = v == len - T;
                    }
                    8 => {
                        v = len - T;
                        live = true;
                    }
                    9 => {
                        edit = Some(r.below(T as u64) as usize);
                    }
                    _ => {}
                }
                commit(&mut a, &mk(v, tick, edit));
                let have: HashSet<String> = a
                    .texts()
                    .into_iter()
                    .map(|t| t.trim_end_matches(" (done)").to_string())
                    .collect();
                for i in prev_v..(prev_v + T).min(v) {
                    if unique(i) && !have.contains(&tx(i)) {
                        fails.push(format!("seed {seed} step {step}: tx({i}) left the top (view {prev_v}->{v}) not archived; gaps={:?}", gap_kinds(&a)));
                        break;
                    }
                }
                prev_v = v;
            }
            let texts = a.texts();
            let uniq: HashSet<&String> = texts.iter().filter(|t| t.starts_with('⏺')).collect();
            rows += texts.len();
            dups += texts.iter().filter(|t| t.starts_with('⏺')).count() - uniq.len();
            let chrome_rows = texts
                .iter()
                .filter(|t| t.contains("shortcuts") || t.contains("Working"))
                .count();
            spinner += chrome_rows;
        }
        eprintln!(
            "rows={rows} transcript-dups={dups} spinner-rows-archived={spinner} failures={}",
            fails.len()
        );
        assert!(
            fails.is_empty(),
            "{} failures, first 8:\n{}",
            fails.len(),
            fails[..fails.len().min(8)].join("\n")
        );
    }
}
mod adversarial_boundaries {
    use super::*;

    /// A8b: one PTY read boundary right after `?1049h` let the epilogue fallback
    /// commit the blank alt grid as a baseline; the first real frame then JUMPed and
    /// REANCHORed into the PREVIOUS app run's rows (across the Leave gap), so the new
    /// run's archive differed from the unsplit feed.
    #[test]
    fn a8b_a_read_boundary_after_alt_enter_never_changes_the_archive() {
        let mut s1 = b"\x1b[?1049h".to_vec();
        for st in 0..=6 {
            s1.extend_from_slice(&sync_frame(&frame(st, 20)));
        }
        s1.extend_from_slice(b"\x1b[?1049l");
        // The app relaunched (e.g. `claude --continue`): its first screen re-shows
        // rows the previous run archived.
        let mut s2 = Vec::new();
        for st in 10..=13 {
            s2.extend_from_slice(&sync_frame(&frame(st, 20)));
        }
        s2.extend_from_slice(b"\x1b[?1049l");
        let run = |split: bool| {
            let mut t = term();
            t.process(&s1);
            if split {
                t.process(b"\x1b[?1049h");
                t.process(&s2);
            } else {
                let mut v = b"\x1b[?1049h".to_vec();
                v.extend_from_slice(&s2);
                t.process(&v);
            }
            (
                t.alt_archive().texts(),
                t.alt_archive().gaps().collect::<Vec<_>>(),
            )
        };
        let (split, whole) = (run(true), run(false));
        assert_eq!(
            split.0.len(),
            whole.0.len(),
            "split gaps {:?} / whole gaps {:?}",
            split.1,
            whole.1
        );
        assert_eq!(split, whole);
    }

    /// A9 (cross-section): under a pinned header the rows the screen re-shows
    /// sit at `[pin, pin + back)`, not `[0, back)`, and they are archived rows
    /// `back_at..` — the read says both, so a reader skips exactly the rows it
    /// already has (the wire carries `pin=` and `back_at=` for this).
    #[test]
    fn pin_and_back_at_locate_the_reshown_rows_under_a_pinned_header() {
        let hdr = "══ fake claude · session header ══".to_string();
        let mk = |v: usize| -> Vec<String> {
            std::iter::once(hdr.clone())
                .chain((v..v + 20).map(tx))
                .chain(CHROME.iter().map(|c| (*c).to_string()))
                .collect()
        };
        let mut a = AltArchive::new();
        for v in 0..=10 {
            commit(&mut a, &mk(v));
        }
        let screen = mk(7);
        commit(&mut a, &screen); // the app scrolls back 3
        let rd = a.read(AltArchiveQuery::oldest(0, 0));
        assert_eq!((rd.back, rd.pin), (3, 1));
        assert_eq!(rd.back_at, rd.last - 3 + 1);
        for m in 0..rd.back {
            assert_eq!(
                a.row(rd.back_at + m as u64),
                Some(screen[rd.pin + m].as_str()),
                "screen row pin+{m} is archived row back_at+{m}"
            );
        }
    }
}
