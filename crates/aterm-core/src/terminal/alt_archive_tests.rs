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

// An independent expression of the retained-row accounting policy. Layout's
// padding calculation does not call the production charge/rounding helper, and
// the header/slot sizes do not use ALT_ARCHIVE_ROW_OVERHEAD.
fn expected_retained_row_charge(len: usize) -> usize {
    let padded_text = std::alloc::Layout::from_size_align(len, 16)
        .unwrap()
        .pad_to_align()
        .size();
    size_of::<ArchivedRow>() + size_of::<[usize; 2]>() + padded_text
}

#[test]
fn retained_row_charge_includes_header_and_padding() {
    for len in [0, 1, 4, 15, 16, 17, 31, 32, 33, 63, 64, 65, 80] {
        assert_eq!(
            alt_archive_row_charge(len),
            expected_retained_row_charge(len),
            "retained-row charge for {len} text bytes"
        );
    }
}

#[test]
fn independently_priced_archive_eviction_conforms_to_ring() {
    use aterm_spec::{derive::ring_model, interp};

    // Four characters are sufficient anchors. The historical len + 32 charge
    // would admit at least a fourth row into this independently priced budget.
    fn short_row(i: usize) -> String {
        format!("{i:04}")
    }
    const KEPT: usize = 3;
    let len = short_row(0).len();
    assert_eq!(len, 4);
    let charge = expected_retained_row_charge(len);
    let budget = KEPT * charge;
    assert!(4 * (len + 32) <= budget, "historical undercharge must fit");
    let mut a = AltArchive::with_limits(budget, ALT_ARCHIVE_MAX_ROWS);
    commit(&mut a, &frame_with(short_row, 0, 20, &CHROME));

    // Reuse Ring's existing Tier-0 obligation for this equal-cost projection:
    // seq is the newest archived index, lo is the oldest retained index.
    // This bind does not model mixed row costs, differ heuristics or RSS.
    let model = ring_model();
    let project = |archive: &AltArchive| -> interp::State {
        [
            ("seq", archive.last() as i64),
            ("lo", archive.oldest() as i64),
        ]
        .into_iter()
        .collect()
    };
    let mut previous = project(&a);
    assert_eq!(previous, model.init_state());
    for seq in 1usize..=6 {
        commit(&mut a, &frame_with(short_row, seq, 20, &CHROME));
        let observed = project(&a);
        assert_eq!(interp::admits(&model, &previous, &observed), Some("Push"));
        assert_eq!(a.bytes(), seq.min(KEPT) * charge);
        assert_eq!(a.lost(), seq.saturating_sub(KEPT) as u64);
        assert_eq!(a.texts(), range(short_row, seq.saturating_sub(KEPT)..seq));

        if seq == 4 {
            // Negative control: keeping the fourth row under the old charge
            // preserves lo instead of evicting. The same model must reject it.
            let mut undercharged = observed.clone();
            undercharged.insert("lo", previous["lo"]);
            assert_eq!(interp::admits(&model, &previous, &undercharged), None);
        }
        previous = observed;
    }
    let read = a.read(AltArchiveQuery::oldest(0, 100));
    assert_eq!((read.first, read.lost, read.rows.len()), (4, 3, KEPT));
}

#[test]
fn eviction_counts_lost_and_advances_first() {
    let cost = super::alt_archive_row_charge(tx(0).len());
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

// ----------------------------------------------------------- shared budget

/// One session's archive with its own budget, shaped like the live one the GUI
/// spawns but counted in rows rather than megabytes.
fn session(budget: usize) -> AltArchive {
    AltArchive::with_limits(budget, ALT_ARCHIVE_MAX_ROWS)
}

/// Scroll `n` transcript rows off the top of a 20-row region: rows `tx(0)` to
/// `tx(n - 1)` are archived, newest last.
fn scroll(a: &mut AltArchive, n: usize) {
    for s in 0..=n {
        commit(a, &frame(s, 20));
    }
}

/// What a share of `share` bytes can actually retain: whole rows, never a
/// fraction of one.
fn whole_rows(share: usize, cost: usize) -> usize {
    share / cost * cost
}

/// The charge for one transcript row — the unit every pool size here is in.
fn row_cost() -> usize {
    alt_archive_row_charge(tx(0).len())
}

#[test]
fn a_pool_shares_one_total_where_per_session_budgets_charged_it_n_times() {
    const N: usize = 4;
    const KEPT: usize = 12;
    let cost = row_cost();
    let total = KEPT * cost;

    // Per session: each of N sessions retains the whole budget, so the process
    // pays N times for one 4 MiB policy — what eight tabs of a fullscreen app
    // cost before the pool (measured: 8 x 4 MiB retained).
    let mut alone: Vec<AltArchive> = (0..N).map(|_| session(total)).collect();
    for a in &mut alone {
        scroll(a, 40);
    }
    assert_eq!(
        alone.iter().map(AltArchive::bytes).sum::<usize>(),
        N * total
    );

    // Sharing: the same N sessions, the same frames, ONE total between them.
    let pool = AltArchiveBudget::new(total, cost);
    let mut tabs: Vec<AltArchive> = (0..N).map(|_| session(total)).collect();
    for a in &mut tabs {
        a.share_budget(Some(Arc::clone(&pool)));
    }
    // JOINING IS NOT USING. Four tabs are open and none is in a full-screen
    // app, so none is drawing on the pool and the first one that needs it gets
    // the whole total — the case the count-on-enabled rule got wrong.
    assert_eq!(pool.live(), 0, "four tabs open, no archive holding a row");
    assert_eq!(pool.share(), total);

    for a in &mut tabs {
        scroll(a, 40);
    }
    assert_eq!(pool.live(), N, "now all four hold rows");
    assert_eq!(pool.share(), total / N);
    // Each drew while fewer were live, so each comes within the equal share on
    // the next frame IT commits — never on another session's thread.
    for a in &mut tabs {
        commit(a, &frame(41, 20));
    }
    assert_eq!(tabs.iter().map(AltArchive::bytes).sum::<usize>(), total);
    for a in &tabs {
        // Equal shares, and each session still keeps its NEWEST rows: eviction
        // is oldest-first inside a session as before, only the line moved.
        assert_eq!(a.bytes(), total / N);
        assert_eq!(a.texts(), range(tx, 41 - KEPT / N..41));
    }

    // Closing every tab hands the whole total to the session that opens next.
    drop(tabs);
    assert_eq!(pool.live(), 0);
    assert_eq!(pool.share(), total);
}

#[test]
fn a_tab_opening_lowers_the_others_on_their_next_frame_and_closing_gives_it_back() {
    const KEPT: usize = 12;
    let cost = row_cost();
    let total = KEPT * cost;
    let pool = AltArchiveBudget::new(total, cost);
    let mut first = session(total);
    first.share_budget(Some(Arc::clone(&pool)));
    scroll(&mut first, 40);
    assert_eq!(first.len(), KEPT, "alone in the pool: the whole total");

    // A second tab opens. An EMPTY archive takes nothing: until that tab runs
    // something that fills it, the session using the pool keeps all of it.
    let mut second = session(total);
    second.share_budget(Some(Arc::clone(&pool)));
    assert_eq!(first.len(), KEPT);
    assert_eq!(
        first.effective_budget(),
        total,
        "an open tab holding nothing must not shrink the tab using the archive"
    );

    // The second session starts drawing. Joining the live count reaches into
    // no other session's archive — the share is lowered, not the rows...
    scroll(&mut second, 40);
    assert_eq!(first.len(), KEPT, "still holding what it held");
    assert_eq!(first.effective_budget(), total / 2);
    // ...so the first session comes within its half on the next frame it
    // commits, dropping the OLDEST rows it holds.
    commit(&mut first, &frame(41, 20));
    assert_eq!(first.len(), KEPT / 2);
    assert_eq!(first.texts(), range(tx, 41 - KEPT / 2..41));

    // And the second comes within its own half the same way: one total across
    // both, once each has drawn with the other live.
    commit(&mut second, &frame(41, 20));
    assert_eq!(second.len(), KEPT / 2);
    assert_eq!(first.bytes() + second.bytes(), total);

    // The tab closes — dropping the archive is the whole deregistration — and
    // the first session grows back into the returned share as it draws.
    drop(second);
    assert_eq!(pool.live(), 1);
    assert_eq!(first.effective_budget(), total);
    for s in 42..=60 {
        commit(&mut first, &frame(s, 20));
    }
    assert_eq!(first.len(), KEPT);
    assert_eq!(first.texts(), range(tx, 60 - KEPT..60));
}

#[test]
fn a_crowded_pool_floors_the_share_instead_of_starving_a_session() {
    const N: usize = 16;
    let cost = row_cost();
    let total = 4 * cost; // sixteen sessions, four rows of room between them
    let pool = AltArchiveBudget::new(total, cost);
    let mut tabs: Vec<AltArchive> = (0..N).map(|_| session(total)).collect();
    for a in &mut tabs {
        a.share_budget(Some(Arc::clone(&pool)));
        // Every one of the sixteen is really running a full-screen app: this
        // is the crowd, not sixteen idle tabs (those cost the pool nothing).
        scroll(a, 40);
    }
    assert_eq!(pool.live(), N);
    assert!(
        total / pool.live() < cost,
        "an unfloored share would not hold one row"
    );
    assert_eq!(pool.share(), cost, "the floor, not a quarter of a row");
    for a in &mut tabs {
        commit(a, &frame(41, 20));
    }
    for a in &tabs {
        assert!(a.enabled(), "a crowded pool never turns a session off");
        assert_eq!(a.texts(), range(tx, 40..41), "the newest row, not none");
    }
    // The floor is a deliberate overshoot: past `total / min_share` sessions
    // the process total grows again, because sixteen archives that answer
    // nothing would be worse than four times the bytes.
    assert_eq!(tabs.iter().map(AltArchive::bytes).sum::<usize>(), N * cost);
    assert_eq!(
        AltArchiveBudget::process().min_share(),
        ALT_ARCHIVE_TOTAL_BUDGET / 16,
        "the live pool's floor is the documented sixteenth"
    );
}

#[test]
fn a_share_is_a_ceiling_and_not_an_allowance() {
    let cost = row_cost();
    let total = 12 * cost;
    let pool = AltArchiveBudget::new(total, cost);
    let mut small = session(3 * cost);
    small.share_budget(Some(Arc::clone(&pool)));
    assert_eq!(
        small.effective_budget(),
        3 * cost,
        "its own budget is under the share and still binds"
    );
    scroll(&mut small, 40);
    assert_eq!(small.len(), 3);

    // And a session cannot buy itself out of the pool with a bigger number.
    small.set_budget(usize::MAX);
    assert_eq!(small.effective_budget(), total);
    assert_eq!(small.budget(), usize::MAX, "its own budget is what was set");
    scroll(&mut small, 40);
    assert_eq!(small.len(), 12);
}

#[test]
fn a_share_that_moves_mid_stream_never_leaves_more_than_it_allows() {
    let cost = row_cost();
    let total = 16 * cost;
    let pool = AltArchiveBudget::new(total, cost);
    let mut drawing = session(total);
    drawing.share_budget(Some(Arc::clone(&pool)));
    let mut crowd: Vec<AltArchive> = Vec::new();
    for s in 0..=60 {
        // Tabs open (the share falls) and close (it rises again) under a
        // session that is drawing the whole time.
        match s {
            10 | 20 | 30 => {
                let mut tab = session(total);
                tab.share_budget(Some(Arc::clone(&pool)));
                // A tab that DRAWS: an open one holding nothing takes no
                // share, so it would not move the line under `drawing` at all.
                scroll(&mut tab, 4);
                crowd.push(tab);
            }
            40 | 50 => {
                crowd.pop();
            }
            _ => {}
        }
        commit(&mut drawing, &frame(s, 20));
        let allowed = drawing.effective_budget();
        assert!(
            drawing.bytes() <= allowed,
            "step {s}: {} retained over {allowed}",
            drawing.bytes()
        );
        // However the share moved, what is left is the NEWEST unbroken run:
        // the rows a shrinking share drops are the oldest, and a growing one
        // resumes where the scroll is, never re-opening a hole.
        let kept = drawing.len();
        assert_eq!(drawing.texts(), range(tx, s - kept..s), "step {s}");
        assert_eq!(drawing.gaps().count(), 0, "step {s}: a scroll has no gap");
    }
    assert_eq!(pool.live(), 2, "three tabs opened, one closed twice");
}

#[test]
fn an_archive_that_is_off_takes_no_share() {
    let cost = row_cost();
    let total = 12 * cost;
    let pool = AltArchiveBudget::new(total, cost);
    let mut on = session(total);
    on.share_budget(Some(Arc::clone(&pool)));
    scroll(&mut on, 40);
    let mut off = session(total);
    off.share_budget(Some(Arc::clone(&pool)));
    scroll(&mut off, 40);
    assert_eq!(pool.live(), 2, "both hold rows");

    off.set_enabled(false);
    assert_eq!(pool.live(), 1, "an archive that is off retains nothing");
    assert_eq!(on.effective_budget(), total);
    // Turning it back ON is not the same as filling it: `set_enabled(true)`
    // starts from a fresh baseline, holding nothing, so it takes no share
    // until it draws again.
    off.set_enabled(true);
    assert_eq!(pool.live(), 1, "on, and holding nothing");
    assert_eq!(on.effective_budget(), total);
    scroll(&mut off, 40);
    assert_eq!(pool.live(), 2);
    assert_eq!(on.effective_budget(), total / 2);

    // `set_budget(0)` says the same thing by the other route.
    off.set_budget(0);
    assert_eq!(pool.live(), 1);
    off.set_budget(total);
    scroll(&mut off, 40);
    assert_eq!(pool.live(), 2);
    assert_eq!(on.effective_budget(), total / 2);

    // Leaving the pool is not the same as being off: it takes its own budget
    // back with it.
    off.share_budget(None);
    assert_eq!(pool.live(), 1);
    assert!(off.bytes() > 0, "leaving the pool keeps the rows");
    assert_eq!(off.effective_budget(), total);
    assert!(off.shared_budget().is_none());
}

/// THE CASE THE POOL EXISTS FOR, AND THE ONE IT MUST NOT BREAK. Eight tabs are
/// open and exactly one is running a full-screen app. Charging a share to the
/// seven holding nothing would hand the one that needs the archive an eighth of
/// the pool and make it evict rows there was room for — the feature taken away
/// from its only user by the accounting meant to protect it.
#[test]
fn seven_idle_tabs_do_not_shrink_the_one_running_a_full_screen_app() {
    const KEPT: usize = 16;
    let cost = row_cost();
    let total = KEPT * cost;
    let pool = AltArchiveBudget::new(total, cost);

    let mut idle: Vec<AltArchive> = (0..7).map(|_| session(total)).collect();
    for a in &mut idle {
        a.share_budget(Some(Arc::clone(&pool)));
    }
    let mut working = session(total);
    working.share_budget(Some(Arc::clone(&pool)));
    scroll(&mut working, 40);

    assert_eq!(pool.live(), 1, "seven tabs are open; one is using the pool");
    assert_eq!(working.effective_budget(), total);
    assert_eq!(working.len(), KEPT, "the whole total, not a KEPT / 8 of it");

    // And the moment one of them really needs the archive, the two split it.
    scroll(&mut idle[0], 40);
    assert_eq!(pool.live(), 2);
    commit(&mut working, &frame(41, 20));
    assert_eq!(working.len(), KEPT / 2);
}

/// **WHAT THE RULE COSTS, MEASURED AND PINNED.** Counting only archives that
/// HOLD rows means a session can fill up while it is alone and keep those rows
/// after others join: it comes within the smaller share on the next frame IT
/// commits, because reaching into another session's archive from this thread is
/// how deadlocks are written (see the module docs). So the pool is a bound that
/// CONVERGES, not one that holds at every instant, and the worst case is every
/// session filling up in turn and then going idle forever: session `k` keeps
/// `total / k`, and the sum is `total * H_n`.
///
/// This is the honest price of the fix above. Under the rule it replaced — count
/// every ENABLED archive — the sum held at `total` in this scenario, and the
/// seven idle tabs in the test above stole seven eighths of the pool from the
/// one tab using it. The exchange is a bounded, self-correcting overshoot for a
/// feature that works, and the arithmetic is pinned here so it can never quietly
/// get worse.
#[test]
fn a_session_that_fills_up_alone_and_goes_idle_is_the_pool_s_worst_case() {
    const N: usize = 8;
    let cost = row_cost();
    let total = 64 * cost;
    let pool = AltArchiveBudget::new(total, cost);

    let mut tabs: Vec<AltArchive> = Vec::new();
    for k in 1..=N {
        let mut a = session(total);
        a.share_budget(Some(Arc::clone(&pool)));
        // Fills up while `k - 1` others are already live, then never commits
        // again — the only way to hold more than an equal share.
        scroll(&mut a, 200);
        // Whole rows only: a share of 21.3 rows retains 21 of them.
        assert_eq!(
            a.bytes(),
            whole_rows(total / k, cost),
            "session {k} filled to the share it saw"
        );
        tabs.push(a);
    }

    let held: usize = tabs.iter().map(AltArchive::bytes).sum();
    let harmonic: usize = (1..=N).map(|k| whole_rows(total / k, cost)).sum();
    assert_eq!(held, harmonic, "the sum is total * H_n, exactly");
    assert!(
        held <= 3 * total,
        "H_8 is 2.72: {held} against a {total} pool"
    );

    // AND IT CONVERGES. One more frame each — the tabs are being used again —
    // and the pool is back inside its total.
    for a in &mut tabs {
        commit(a, &frame(201, 20));
    }
    assert_eq!(
        tabs.iter().map(AltArchive::bytes).sum::<usize>(),
        total,
        "one frame per session is all it takes"
    );
}

/// **TIER-1 BINDING FOR `AltArchivePool`** — the REAL archives, driven beside
/// the model, step for step.
///
/// The model (`aterm-spec`'s `alt_archive_pool_model`, discharged by
/// `derived_alt_archive_pool_proves_catches_and_multiplies` under the
/// prove/catch/MULTIPLY protocol and machine-checked by Trust `ty`) proves the
/// sentence. This test is what ties the sentence to the code: a theorem about a
/// state machine nobody runs is worth nothing if the shipped `AltArchive` does
/// something else.
///
/// The projection is exact and unit-free: the model counts ROWS with
/// `Total = 4` and `Half = 2`, so the pool is built at `4 * row_cost()` with
/// `min_share = row_cost()` (a floor under `Half`, so it never re-raises the
/// share and the projection stays 1:1). Each `CommitA` is one real committed
/// frame, and `a.len()` must equal the model's `a` after every single step —
/// including the two steps that are the whole point:
///
///   * B joins and starts drawing while A is already full. A is over its new
///     share and does NOT shed it, because an archive can only lower its OWN
///     retention. The model calls A unsettled there and `PoolBounded` holds
///     vacuously; the real archive holds exactly the same rows.
///   * A commits once more and comes within the share. The bound CONVERGES,
///     and that is the frame it converges on.
#[test]
fn alt_archive_pool_conformance_real_archives_project_onto_model() {
    let m = aterm_spec::derive::alt_archive_pool_model();
    let cost = row_cost();
    let pool = AltArchiveBudget::new(4 * cost, cost);
    let mut real_a = session(4 * cost);
    let mut real_b = session(4 * cost);
    real_a.share_budget(Some(Arc::clone(&pool)));
    real_b.share_budget(Some(Arc::clone(&pool)));

    let mut st = m.init_state();
    let (mut fa, mut fb) = (0usize, 0usize);
    let model = |st: &std::collections::BTreeMap<&'static str, i64>, v: &str| -> i64 {
        *st.get(v).expect("the model declares this variable")
    };

    // BASELINE FIRST, and it is not a model step. The first frame committed to
    // an archive establishes what the screen holds; nothing has scrolled off
    // it yet, so it archives no row. The model's `CommitA` is a row LEAVING
    // the viewport, which is every frame after this one.
    commit(&mut real_a, &frame(fa, 20));
    fa += 1;
    commit(&mut real_b, &frame(fb, 20));
    fb += 1;
    assert_eq!(real_a.len(), 0, "the baseline frame archives nothing");
    assert_eq!(real_b.len(), 0);

    // Five frames into A while it is ALONE in the pool: it may fill the whole
    // Total, and the fifth changes nothing because it is already there.
    for step in 1..=5 {
        assert!(
            m.fire("CommitA", &mut st),
            "step {step}: the model admits it"
        );
        commit(&mut real_a, &frame(fa, 20));
        fa += 1;
        assert_eq!(
            real_a.len() as i64,
            model(&st, "a"),
            "step {step}: the real archive and the model must retain the same rows"
        );
        assert!(m.check_invariant("PoolBounded", &st));
        assert!(m.check_invariant("IdleTakesNothing", &st));
    }
    assert_eq!(real_a.len(), 4, "alone in the pool: the whole total");
    assert_eq!(pool.live(), 1, "and B, holding nothing, is not counted");

    // B starts drawing. A is now over its halved share and keeps its rows
    // until its own next frame — the model's `unsettled`, the code's "an
    // archive can only lower its OWN retention".
    assert!(m.fire("CommitB", &mut st));
    // B's LAST frame — the rest of this test is A converging while B sits
    // settled — so `fb` is not advanced past it, exactly as `fa` is not
    // advanced past A's last frame below.
    commit(&mut real_b, &frame(fb, 20));
    assert_eq!(real_b.len() as i64, model(&st, "b"));
    assert_eq!(real_a.len() as i64, model(&st, "a"), "A did not shed a row");
    assert_eq!(pool.live(), 2, "both hold rows now");
    assert!(
        real_a.bytes() + real_b.bytes() > pool.total(),
        "fixture: this is the transient the model states over SETTLED archives \
         only — if it did not happen, PoolBounded would be proving something \
         easier than the code does"
    );
    assert!(m.check_invariant("PoolBounded", &st));

    // A commits again: the bound converges, on this frame.
    assert!(m.fire("CommitA", &mut st));
    commit(&mut real_a, &frame(fa, 20));
    assert_eq!(
        real_a.len() as i64,
        model(&st, "a"),
        "A came within its half"
    );
    assert_eq!(real_a.len(), 2);
    assert!(
        real_a.bytes() + real_b.bytes() <= pool.total(),
        "converged, exactly as `PoolBounded` says of two settled archives"
    );
    assert!(m.check_invariant("PoolBounded", &st));
    assert!(m.check_invariant("IdleTakesNothing", &st));
}

/// The ONE test in this binary that touches the process-wide pool, so the live
/// count it asserts on is this session's own arithmetic.
#[test]
fn a_live_session_joins_the_process_budget_and_leaves_when_it_closes() {
    let pool = AltArchiveBudget::process();
    assert_eq!(pool.total(), ALT_ARCHIVE_TOTAL_BUDGET);
    let base = pool.live();

    let mut t = Terminal::new(25, COLS);
    t.set_alt_archive_enabled(true); // regardless of ATERM_ALT_ARCHIVE
    t.set_alt_archive_shared(true);
    assert_eq!(
        pool.live(),
        base,
        "a tab that has not entered the alt screen holds nothing, and a share \
         it does not need is a share taken from the tab that does"
    );

    // It runs a full-screen app: NOW it is drawing on the pool.
    t.process(b"\x1b[?1049h");
    for s in 0..=40 {
        t.process(&sync_frame(&frame(s, 20)));
    }
    assert!(t.alt_archive().bytes() > 0, "the archive filled");
    assert_eq!(pool.live(), base + 1);
    assert!(
        t.alt_archive()
            .shared_budget()
            .is_some_and(|p| Arc::ptr_eq(p, &pool)),
        "the session draws on THE process pool"
    );
    assert_eq!(
        t.alt_archive().effective_budget(),
        ALT_ARCHIVE_TOTAL_BUDGET / (base + 1),
        "one total, divided by the sessions drawing on it"
    );

    t.set_alt_archive_shared(false);
    assert_eq!(pool.live(), base);
    assert_eq!(
        t.alt_archive().effective_budget(),
        ALT_ARCHIVE_DEFAULT_BUDGET
    );

    // Re-joining counts it again straight away — it is still holding the rows,
    // so there is nothing to wait for.
    t.set_alt_archive_shared(true);
    assert_eq!(pool.live(), base + 1);
    drop(t); // the tab closes: nothing else has to say so
    assert_eq!(pool.live(), base);
}

// ------------------------------------------------------------------ differ

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
    // The replaced screen was flushed, never its chrome…
    assert_eq!(t.alt_archive().texts(), range(tx, 0..23));
    // …and the restored one is the new baseline: the first frame after the
    // restore is diffed against it, so the rows it scrolled off are archived.
    // (This restore put back the very screen it flushed, so they repeat after
    // the gap — a duplicate beats a loss.) It keeps following from there.
    t.process(&sync_frame(&frame(10, 20)));
    t.process(&sync_frame(&frame(11, 20)));
    let mut want = range(tx, 0..23);
    want.extend(range(tx, 3..11));
    assert_eq!(t.alt_archive().texts(), want);
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

// ================================================ ROUND 10: CHROME AFTER ADOPTION
// A self-update handoff (2026-09-14, live): the adopted worker's first archived
// rows were its composer's rule and footer. The adopted screen was installed
// blind, a repaint of it measured no chrome (Rule 2 returns first), and the
// window's first resize flushed the whole screen. Now the restored screen is
// the baseline, and a resize before any chrome was measured leaves out the rows
// the new frame still shows at the bottom.
mod chrome_after_adoption {
    use super::*;
    use crate::terminal::TerminalCheckpoint;

    /// Claude Code's composer: a blank, a rule, the prompt, a rule, the footer,
    /// with the rules drawn at the screen's width.
    fn claude_chrome(cols: usize) -> Vec<String> {
        vec![
            String::new(),
            "─".repeat(cols),
            "❯ draft the next instruction".to_string(),
            "─".repeat(cols),
            "  ? for shortcuts".to_string(),
        ]
    }

    fn claude_frame(start: usize, t: usize, cols: usize) -> Vec<String> {
        (start..start + t)
            .map(tx)
            .chain(claude_chrome(cols))
            .collect()
    }

    /// A composer BOX (`╭─╮ │ > text │ ╰─╯`) and footer, `inner` wide.
    fn boxed_chrome(inner: usize) -> Vec<String> {
        vec![
            String::new(),
            format!("╭{}╮", "─".repeat(inner)),
            format!("│ > type here{}│", " ".repeat(inner - 11)),
            format!("╰{}╯", "─".repeat(inner)),
            "  ? for shortcuts".to_string(),
        ]
    }

    fn boxed_frame(start: usize, t: usize, inner: usize) -> Vec<String> {
        (start..start + t)
            .map(tx)
            .chain(boxed_chrome(inner))
            .collect()
    }

    /// Every archived row that is composer or footer, rules and boxes included.
    fn chrome_in(texts: &[String]) -> Vec<String> {
        texts
            .iter()
            .filter(|r| r.starts_with(['─', '╭', '│', '╰', '❯']) || r.contains("shortcuts"))
            .cloned()
            .collect()
    }

    /// A terminal that adopted `cp` the way the seamless-update path does: a
    /// fresh engine, then `restore_checkpoint`.
    fn adopted(cp: &TerminalCheckpoint) -> Terminal {
        let mut t = term();
        t.restore_checkpoint(cp);
        t
    }

    /// The old process: `frame(s, 20)` for `s` in `0..=last` on the alt screen.
    fn worker_at(last: usize) -> Terminal {
        let mut t = term();
        t.process(b"\x1b[?1049h");
        for s in 0..=last {
            t.process(&sync_frame(&frame(s, 20)));
        }
        t
    }

    /// Design test 1: after a restore on the alt screen, the app repainting the
    /// restored screen archives nothing and is no rebaseline.
    #[test]
    fn restore_then_an_identical_frame_archives_nothing() {
        let mut t = adopted(&worker_at(3).checkpoint());
        let epoch = t.alt_archive().epoch();
        t.process(&sync_frame(&frame(3, 20)));
        assert!(t.alt_archive().is_empty(), "{:#?}", t.alt_archive().texts());
        assert_eq!(t.alt_archive().gaps().count(), 0);
        assert_eq!(t.alt_archive().epoch(), epoch, "a repaint is no rebaseline");
        t.process(&sync_frame(&frame(6, 20)));
        assert_eq!(t.alt_archive().texts(), range(tx, 3..6));
    }

    /// The restored screen is the baseline: when the app's FIRST frame after the
    /// adoption scrolls, the rows that left the restored screen are archived
    /// (installed blind, that frame lost them).
    #[test]
    fn the_first_frame_after_a_restore_archives_what_left_the_restored_screen() {
        let mut t = adopted(&worker_at(3).checkpoint());
        t.process(&sync_frame(&frame(6, 20)));
        assert_eq!(t.alt_archive().texts(), range(tx, 3..6));
        assert_eq!(t.alt_archive().gaps().count(), 0);
    }

    /// A checkpoint captured inside an open 2026 window holds a half-painted
    /// screen; it is not the baseline. (As one, the app's finished frame was an
    /// in-place edit of three rows over blanks that measured the chrome as 0,
    /// and the next resize flushed the composer.)
    #[test]
    fn a_restore_inside_an_open_2026_window_is_not_the_baseline() {
        let mut old = worker_at(3);
        // A full redraw, cut after its first three rows.
        let redraw = frame(50, 20);
        let mut half = b"\x1b[?2026h\x1b[?25l\x1b[H\x1b[2J".to_vec();
        for (r, text) in redraw.iter().take(3).enumerate() {
            half.extend_from_slice(format!("\x1b[{};1H{text}\x1b[K", r + 1).as_bytes());
        }
        old.process(&half);
        assert!(old.modes().synchronized_output);
        let mut t = adopted(&old.checkpoint());
        assert!(t.modes().synchronized_output);
        // The app finishes the frame and closes the window.
        let mut rest = Vec::new();
        for (r, text) in redraw.iter().enumerate().skip(3) {
            rest.extend_from_slice(format!("\x1b[{};1H{text}\x1b[K", r + 1).as_bytes());
        }
        rest.extend_from_slice(b"\x1b[?25h\x1b[?2026l");
        t.process(&rest);
        assert!(t.alt_archive().is_empty());
        t.resize(26, COLS);
        t.process(&sync_frame(&frame(50, 21)));
        let texts = t.alt_archive().texts();
        assert_eq!(chrome_in(&texts), Vec::<String>::new(), "{texts:#?}");
        assert_eq!(texts, range(tx, 50..70));
    }

    /// Design test 2: one installed frame with the composer, then a resize of
    /// one row down or up — no rule, `❯` or footer row is archived, and exactly
    /// the rows the new screen no longer shows are.
    #[test]
    fn one_installed_frame_then_a_one_row_resize_archives_no_chrome() {
        // (installed frame, frame at the new size, rows expected in the archive)
        let cases: [(Vec<String>, Vec<String>, Vec<String>); 3] = [
            // One row shorter: the top row is gone, and only it is archived.
            (
                claude_frame(0, 20, 100),
                claude_frame(1, 19, 100),
                vec![tx(0)],
            ),
            // One row taller, an older row shown on top: every row is still shown.
            (
                claude_frame(1, 20, 100),
                claude_frame(0, 21, 100),
                Vec::new(),
            ),
            // One row taller, a blank opened above the composer: the transcript
            // moved up one row, so all of it is flushed (a duplicate, later —
            // never a loss) and none of the composer is.
            (
                claude_frame(0, 20, 100),
                (0..20)
                    .map(tx)
                    .chain(std::iter::once(String::new()))
                    .chain(claude_chrome(100))
                    .collect(),
                range(tx, 0..20),
            ),
        ];
        for (n, (installed, next, want)) in cases.into_iter().enumerate() {
            let mut a = AltArchive::new();
            commit(&mut a, &installed);
            commit(&mut a, &next);
            let texts = a.texts();
            assert_eq!(chrome_in(&texts), Vec::<String>::new(), "case {n}");
            assert_eq!(texts, want, "case {n}");
            let gaps = if want.is_empty() {
                Vec::new()
            } else {
                vec![(want.len() as u64, AltArchiveGapKind::Resize)]
            };
            assert_eq!(gap_kinds(&a), gaps, "case {n}");
            assert_eq!(a.epoch(), 1, "case {n}: a resize rebaselines");
        }
    }

    /// Design test 3: a WIDTH resize redraws the rules at the new width — a
    /// different row text — and they are still chrome.
    #[test]
    fn a_width_resize_still_treats_rule_rows_as_chrome() {
        let mut a = AltArchive::new();
        a.commit_rows(&claude_frame(0, 20, 100), 100);
        // Narrower: the transcript re-wrapped and moved up two.
        a.commit_rows(&claude_frame(2, 20, 90), 90);
        assert_eq!(a.texts(), range(tx, 0..20));
        assert_eq!(gap_kinds(&a), vec![(20, AltArchiveGapKind::Resize)]);
        // Wider and one row taller, from an installed frame again.
        let mut b = AltArchive::new();
        b.commit_rows(&claude_frame(0, 20, 90), 90);
        b.commit_rows(&claude_frame(5, 21, 120), 120);
        assert_eq!(b.texts(), range(tx, 0..20));
    }

    /// The composer BOX: `│ > type here │` and its borders redrawn at a new width
    /// (padding and rules change, the words do not) are chrome too.
    #[test]
    fn a_composer_box_redrawn_at_a_new_width_is_chrome() {
        let mut a = AltArchive::new();
        a.commit_rows(&boxed_frame(0, 20, 60), 100);
        a.commit_rows(&boxed_frame(2, 20, 50), 90);
        assert_eq!(a.texts(), range(tx, 0..20));
        let mut b = AltArchive::new();
        b.commit_rows(&boxed_frame(0, 20, 60), 100);
        b.commit_rows(&boxed_frame(1, 19, 60), 100);
        assert_eq!(b.texts(), vec![tx(0)]);
    }

    /// Counting too many rows as still shown is safe: a content row left out of
    /// the resize flush is archived when it scrolls off the new screen.
    #[test]
    fn rows_left_out_of_a_resize_flush_are_archived_when_they_scroll_off() {
        let mut a = AltArchive::new();
        commit(&mut a, &claude_frame(0, 20, 100));
        commit(&mut a, &claude_frame(1, 19, 100)); // shorter: tx(0) flushed
        for s in 2..=30 {
            commit(&mut a, &claude_frame(s, 19, 100));
        }
        assert_eq!(a.texts(), range(tx, 0..30));
        assert_eq!(gap_kinds(&a), vec![(1, AltArchiveGapKind::Resize)]);
    }
}

// ------------------------------------------------------- handoff carry
//
// Round 10: a self-update handoff carries the archive to the process that
// adopts the session. The carry is exact when it is whole (the adopted
// archive then diffs every later frame exactly as the old one would have), a
// tail keeps the indices going, and nothing a carry says — however corrupt —
// may panic the adopting process, where it would kill a reader thread after
// the old process has already exited.
mod handoff_carry {
    use super::*;

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
            self.next() % n.max(1)
        }
        fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
            &xs[self.below(xs.len() as u64) as usize]
        }
    }

    /// Every retained row, plus the counters and the differ's state.
    fn whole(a: &AltArchive) -> AltArchiveCarry {
        let (fence, mut c) = a.carry_head(true);
        assert!(a.carry_rows(&mut c, fence, 0, usize::MAX), "nothing moved");
        c
    }

    fn adopted(c: AltArchiveCarry) -> AltArchive {
        let mut b = AltArchive::new();
        b.set_origin(0xdead_beef); // the adopting process's own, replaced
        assert_eq!(b.import(c), AltArchiveImport::Exact);
        b
    }

    /// The two archives answer every read alike.
    fn same(a: &AltArchive, b: &AltArchive, what: &str) {
        let last = a.last();
        for q in [
            AltArchiveQuery::oldest(0, usize::MAX),
            AltArchiveQuery::newest(0, 7),
            AltArchiveQuery::oldest(last.saturating_sub(5), 3),
            AltArchiveQuery::oldest(last, 10),
        ] {
            assert_eq!(a.read(q), b.read(q), "{what}: {q:?}");
        }
        assert_eq!(a.fence(), b.fence(), "{what}");
    }

    /// back_at names a retained row whenever back > 0.
    fn back_in_range(a: &AltArchive, what: &str) {
        let r = a.read(AltArchiveQuery::oldest(0, 0));
        if r.back > 0 {
            assert!(
                r.back_at >= r.oldest && r.back_at <= r.last,
                "{what}: back_at {} outside {}..={}",
                r.back_at,
                r.oldest,
                r.last
            );
        }
    }

    /// One random step of a viewport over a growing transcript (the walk of
    /// `property_walk`), with the odd resize, redraw, leave/enter and blank
    /// screen thrown in so every differ state gets carried at some point.
    struct Walk {
        len: usize,
        v: usize,
        live: bool,
        t: usize,
        world: usize,
    }

    enum Step {
        Frame(Vec<String>),
        Leave,
        Enter,
    }

    impl Walk {
        fn new() -> Self {
            Self {
                len: 20,
                v: 0,
                live: true,
                t: 20,
                world: 0,
            }
        }

        fn frame(&self) -> Vec<String> {
            let row = |i: usize| {
                if self.world == 0 {
                    tx(i)
                } else {
                    format!("⏺ world {} row {i:04} with words", self.world)
                }
            };
            let mut f: Vec<String> = (self.v..self.v + self.t).map(row).collect();
            f.extend(CHROME.iter().map(|c| (*c).to_string()));
            f
        }

        fn step(&mut self, r: &mut Rng) -> Step {
            match r.below(20) {
                0..=8 => {
                    let most = if r.below(8) == 0 { 30 } else { 4 };
                    self.len += 1 + r.below(most) as usize;
                    if self.live {
                        self.v = self.len - self.t;
                    }
                }
                9 | 10 => {
                    self.v = self.v.saturating_sub(1 + r.below(30) as usize);
                    self.live = false;
                }
                11 | 12 => {
                    self.v = (self.v + 1 + r.below(30) as usize).min(self.len - self.t);
                    self.live = self.v == self.len - self.t;
                }
                13 => {
                    self.v = self.len - self.t;
                    self.live = true;
                }
                14 => {
                    // A resize: one row more or fewer.
                    self.t = if self.t > 18 && r.below(2) == 0 {
                        self.t - 1
                    } else {
                        self.t + 1
                    };
                    self.v = self.len.saturating_sub(self.t);
                    self.len = self.len.max(self.t);
                    self.live = true;
                }
                15 => {
                    // A wholesale redraw into other content, and later back.
                    self.world = if self.world == 0 {
                        1 + r.below(3) as usize
                    } else {
                        0
                    };
                }
                16 => return Step::Leave,
                17 => return Step::Enter,
                18 => return Step::Frame(vec![String::new(); self.t + 5]),
                _ => {}
            }
            Step::Frame(self.frame())
        }
    }

    fn apply(a: &mut AltArchive, s: &Step) {
        match s {
            Step::Frame(f) => commit(a, f),
            Step::Leave => a.leave(),
            Step::Enter => a.enter(),
        }
    }

    /// Design test 5 (the property): a random history, a WHOLE carry into a
    /// fresh archive, then the same random frames into both — every read
    /// identical after every commit, and back_at always in range.
    #[test]
    fn a_whole_carry_continues_exactly_where_the_old_archive_would() {
        let mut exact_steps = 0usize;
        // How often each part of the differ's state was live at the handoff:
        // a property that never carried a re-shown run or a `below` row would
        // say nothing about carrying them.
        let (mut with_prev, mut with_debt, mut with_below, mut with_chrome) = (0, 0, 0, 0);
        for seed in 1..=120u64 {
            let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut w = Walk::new();
            let mut a = AltArchive::with_limits(64 * 1024, 4096);
            a.set_origin(seed);
            let before = 1 + r.below(250) as usize;
            for _ in 0..before {
                let s = w.step(&mut r);
                apply(&mut a, &s);
            }
            let carry = whole(&a);
            let d = carry.differ.as_ref().expect("asked for");
            with_prev += usize::from(d.prev.is_some());
            with_debt += usize::from(d.debt > 0);
            with_below += usize::from(!d.below.is_empty());
            with_chrome += usize::from(d.chrome.is_some());
            let mut b = adopted(carry);
            b.set_budget(64 * 1024);
            same(&a, &b, &format!("seed {seed} at the handoff"));
            for step in 0..200 {
                let s = w.step(&mut r);
                apply(&mut a, &s);
                apply(&mut b, &s);
                let what = format!("seed {seed} step {step} after a handoff at {before}");
                same(&a, &b, &what);
                back_in_range(&b, &what);
                exact_steps += 1;
            }
        }
        eprintln!(
            "{exact_steps} steps compared after a handoff; carried prev={with_prev} \
             debt={with_debt} below={with_below} chrome={with_chrome} of 120"
        );
        assert!(
            with_prev > 60 && with_debt > 5 && with_below > 5 && with_chrome > 30,
            "the walk must carry every part of the differ's state: prev={with_prev} \
             debt={with_debt} below={with_below} chrome={with_chrome}"
        );
    }

    /// The carry keeps the origin and the indices: a mark minted before the
    /// handoff reads the same rows after it.
    #[test]
    fn the_origin_and_indices_survive_the_carry() {
        let mut a = AltArchive::new();
        a.set_origin(77);
        for s in 0..=40 {
            commit(&mut a, &frame(s, 20));
        }
        let mark = a.last() - 10;
        let before = a.read(AltArchiveQuery::oldest(mark, 100));
        let b = adopted(whole(&a));
        let after = b.read(AltArchiveQuery::oldest(mark, 100));
        assert_eq!(after.origin, 77);
        assert_eq!(after.rows, before.rows);
        assert_eq!(after.first, before.first);
    }

    /// A TAIL: rows before `from` are left out and counted lost; the ones
    /// carried keep their indices; the byte cap takes the NEWEST rows.
    #[test]
    fn a_tail_carry_counts_what_it_leaves_out() {
        let mut a = AltArchive::new();
        // Longer than the differ's reach (8 screens of 25 rows), so `from`
        // is what bounds the tail.
        for s in 0..=260 {
            commit(&mut a, &frame(s, 20));
        }
        let (last, first) = (a.last(), a.oldest());
        assert!(last > 240);
        let (fence, mut c) = a.carry_head(true);
        assert!(a.carry_rows(&mut c, fence, 31, usize::MAX));
        assert_eq!((c.first, c.last()), (31, last));
        assert_eq!(c.lost, a.lost() + (31 - first));
        let b = adopted(c);
        assert_eq!(b.last(), last, "the indices go on");
        let r = b.read(AltArchiveQuery::oldest(10, 1000));
        assert_eq!(
            r.lost,
            31 - 11,
            "rows 11..31 were not carried: lost to a reader"
        );
        assert_eq!(r.rows[0].as_ref(), a.row(31).unwrap());

        // The byte cap: only the newest rows that fit.
        let (fence, mut c) = a.carry_head(true);
        let per_row = super::alt_archive_row_charge(a.row(last).unwrap().len());
        assert!(a.carry_rows(&mut c, fence, 0, per_row * 5 + per_row / 2));
        assert_eq!(c.rows.len(), 5);
        assert_eq!(c.last(), last);
        assert_eq!(c.first, last - 4);
    }

    /// A tail never stops short of what the adopting differ may point back
    /// at: the last [`REANCHOR_SCREENS`] screens of rows (never before this
    /// app run's floor) and the run the screen shows again. The host's `from`
    /// only reaches further back. So a scroll-back past the host's tail, at
    /// the same size, is recognized after the handoff exactly as before it.
    #[test]
    fn a_tail_carry_reaches_back_as_far_as_the_differ_can_point() {
        let screen = 25; // 20 transcript rows and 5 of chrome
        let mut a = AltArchive::new();
        for s in 0..=260 {
            commit(&mut a, &frame(s, 20));
        }
        let last = a.last();
        let (fence, mut c) = a.carry_head(true);
        assert!(a.carry_rows(&mut c, fence, last - 5, usize::MAX));
        assert_eq!(c.first, last + 1 - (REANCHOR_SCREENS * screen) as u64);
        let mut b = adopted(c);
        // Back 60 rows, three a frame, forward again, then three new rows.
        let mut views: Vec<usize> = (1..=20).map(|k| 260 - 3 * k).collect();
        views.extend((0..20).rev().map(|k| 260 - 3 * k));
        views.push(263);
        for v in views {
            commit(&mut a, &frame(v, 20));
            commit(&mut b, &frame(v, 20));
        }
        let after = |x: &AltArchive| x.read(AltArchiveQuery::oldest(last, 1000));
        let (ra, rb) = (after(&a), after(&b));
        assert_eq!(
            (&rb.rows, rb.last, &rb.gaps, rb.back),
            (&ra.rows, ra.last, &ra.gaps, ra.back)
        );
        let rows: Vec<String> = rb.rows.iter().map(|r| r.to_string()).collect();
        assert_eq!(rows, range(tx, 260..263), "only the new rows");
        assert_eq!(b.gaps().count(), 0);

        // The re-shown run reaches further back than the screens: it is
        // carried whole. Back 210 rows: the screen shows archived rows from
        // index 51 (tx(50)) on.
        for k in 1..=70 {
            commit(&mut a, &frame(263 - 3 * k, 20));
        }
        assert!(a.back() > 0);
        let (fence, mut c) = a.carry_head(true);
        assert!(a.carry_rows(&mut c, fence, a.last(), usize::MAX));
        assert_eq!(c.first, a.back_at());
        assert!(a.back_at() < a.last() + 1 - (REANCHOR_SCREENS * screen) as u64);

        // Never into an earlier app run: the reach stops at the floor.
        let mut a = AltArchive::new();
        for s in 0..=30 {
            commit(&mut a, &frame(s, 20));
        }
        a.leave();
        a.enter();
        let floor = a.last() + 1;
        for s in 100..=104 {
            commit(&mut a, &frame(s, 20));
        }
        assert!(a.last() >= floor);
        let (fence, mut c) = a.carry_head(true);
        assert!(a.carry_rows(&mut c, fence, a.last() + 1, usize::MAX));
        assert_eq!(c.first, floor);
    }

    /// A scroll-back past the rows a carry brought (the byte cap left the
    /// older ones out) cannot be matched against the archive: the rows it
    /// re-shows are archived again when they scroll off — after a `jump` gap
    /// at the newest row, so a reader is told, never silently.
    #[test]
    fn a_scroll_back_past_the_carried_rows_is_a_gap_not_a_silent_repeat() {
        let mut a = AltArchive::new();
        for s in 0..=100 {
            commit(&mut a, &frame(s, 20));
        }
        let last = a.last();
        let per_row = super::alt_archive_row_charge(tx(0).len());
        let (fence, mut c) = a.carry_head(true);
        assert!(a.carry_rows(&mut c, fence, 0, per_row * 30));
        assert_eq!(c.first, last - 29, "the cap kept the newest 30");
        let mut b = adopted(c);
        let mut views: Vec<usize> = (1..=20).map(|k| 100 - 3 * k).collect();
        views.extend((0..20).rev().map(|k| 100 - 3 * k));
        for v in views {
            commit(&mut a, &frame(v, 20));
            commit(&mut b, &frame(v, 20));
        }
        assert_eq!(a.last(), last, "the old archive recognized every row");
        assert_eq!(gap_kinds(&a), vec![]);
        assert!(b.last() > last, "the adopted one archived them again…");
        assert_eq!(
            gap_kinds(&b),
            vec![(last, AltArchiveGapKind::Jump)],
            "…after a gap"
        );
    }

    /// The differ's state the freeze had no time for is taken afterwards —
    /// exactly the state the freeze would have taken — only while nothing
    /// was committed since. An edit in place moves the state but archives
    /// nothing: the rows still attach, the state does not.
    #[test]
    fn the_differ_state_is_taken_off_the_freeze_only_while_nothing_committed() {
        let mut a = AltArchive::new();
        for s in 0..=30 {
            commit(&mut a, &frame(s, 20));
        }
        let (fence, head) = a.carry_head(false);
        assert!(head.differ.is_none());
        let mut c = head.clone();
        assert!(a.carry_differ(&mut c, fence));
        assert_eq!(
            c.differ,
            a.carry_head(true).1.differ,
            "what the freeze would have taken"
        );
        assert!(!a.carry_differ(&mut c, fence), "it has one already");
        assert!(a.carry_rows(&mut c, fence, 0, usize::MAX));
        let b = adopted(c);
        assert_eq!(b.last(), a.last());

        let mut edited = frame(30, 20);
        edited[19] = "⏺ transcript row 0049 with words, and then some".to_string();
        commit(&mut a, &edited);
        assert_eq!(a.last(), fence.last, "nothing archived");
        let mut c = head.clone();
        assert!(!a.carry_differ(&mut c, fence), "the state moved");
        assert!(c.differ.is_none());
        assert!(
            a.carry_rows(&mut c, fence, 0, usize::MAX),
            "the rows did not"
        );
        assert_eq!(AltArchive::new().import(c), AltArchiveImport::NoBaseline);
    }

    /// The fence: rows are attached only while the archive stands where the
    /// freeze saw it; otherwise the head carries counters only, and a reader
    /// is told the rows were lost rather than handed rows of another screen.
    #[test]
    fn rows_attach_only_while_the_fence_holds() {
        let mut a = AltArchive::new();
        for s in 0..=30 {
            commit(&mut a, &frame(s, 20));
        }
        let (fence, head) = a.carry_head(true);
        let last = fence.last;
        commit(&mut a, &frame(33, 20)); // the archive moved on
        let mut c = head.clone();
        assert!(!a.carry_rows(&mut c, fence, 0, usize::MAX), "moved");
        assert_eq!(c, head, "untouched");
        assert!(c.rows.is_empty());
        assert_eq!(c.first, last + 1);
        let b = adopted(c);
        assert_eq!(b.last(), last);
        let r = b.read(AltArchiveQuery::oldest(5, 100));
        assert_eq!((r.rows.len(), r.lost), (0, last - 5));
    }

    /// An archive that is off refuses a carry: `ATERM_ALT_ARCHIVE=0` on the
    /// adopting side drops the rows.
    #[test]
    fn an_archive_that_is_off_refuses_the_carry() {
        let mut a = AltArchive::new();
        for s in 0..=30 {
            commit(&mut a, &frame(s, 20));
        }
        let mut b = AltArchive::new();
        b.set_enabled(false);
        assert_eq!(b.import(whole(&a)), AltArchiveImport::Refused);
        assert!(b.is_empty());
        assert!(!b.enabled());
    }

    /// A differ state that does not describe its own frame is dropped WHOLE
    /// (never repaired field by field); the rows and counters still land.
    #[test]
    fn a_differ_state_that_does_not_fit_is_dropped_whole() {
        let mut a = AltArchive::new();
        for s in 0..=30 {
            commit(&mut a, &frame(s, 20));
        }
        let good = whole(&a);
        let rows = good.differ.as_ref().unwrap().prev.as_ref().unwrap().1.len();
        let bad: Vec<Box<dyn Fn(&mut AltArchiveDiffer)>> = vec![
            Box::new(|d| d.pin = rows + 1),
            Box::new(|d| d.chrome = Some(rows + 1)),
            Box::new(|d| {
                d.debt = 3;
                d.debt_at = u64::MAX - 1;
            }),
            Box::new(|d| {
                d.pin = rows - 1;
                d.debt = 2;
                d.debt_at = 1;
            }),
            Box::new(|d| d.prev.as_mut().unwrap().1[0].push_str("   ")),
            Box::new(|d| d.prev.as_mut().unwrap().1[0].push('\n')),
            Box::new(|d| d.prev.as_mut().unwrap().0 = 0),
            Box::new(|d| d.prev.as_mut().unwrap().1.clear()),
            Box::new(|d| {
                for r in &mut d.prev.as_mut().unwrap().1 {
                    r.clear();
                }
            }),
            Box::new(|d| d.below = vec!["x".to_string(); 8 * rows + 1]),
            Box::new(|d| {
                d.prev = None;
                d.pin = 1;
            }),
        ];
        for (i, spoil) in bad.iter().enumerate() {
            let mut c = good.clone();
            spoil(c.differ.as_mut().unwrap());
            let mut b = AltArchive::new();
            assert_eq!(b.import(c), AltArchiveImport::NoBaseline, "case {i}");
            assert_eq!(b.last(), a.last(), "case {i}: the rows still land");
            assert_eq!(b.back(), 0, "case {i}");
            // The next frame is a new baseline, and nothing panics after.
            for s in 31..40 {
                commit(&mut b, &frame(s, 20));
                back_in_range(&b, &format!("case {i}"));
            }
        }
    }

    fn random_text(r: &mut Rng) -> String {
        let pieces = [
            "⏺ row",
            "",
            " ",
            "  ",
            "\n",
            "\u{0}",
            "\u{9b}",
            "─────",
            "│ > x │",
            "abc",
            "é",
            "\t",
            "{",
            "0042",
            "tx",
            "   trailing   ",
        ];
        (0..r.below(6)).map(|_| *r.pick(&pieces)).collect()
    }

    fn random_u64(r: &mut Rng, near: u64) -> u64 {
        match r.below(6) {
            0 => 0,
            1 => u64::MAX - r.below(3),
            2 => MAX_CARRIED_INDEX + r.below(3) - 1,
            3 => near.wrapping_add(r.below(5)).wrapping_sub(2),
            _ => r.below(near.saturating_mul(2).max(4)),
        }
    }

    fn random_carry(r: &mut Rng, base: &AltArchiveCarry) -> AltArchiveCarry {
        let mut c = base.clone();
        let near = base.last().max(4);
        for _ in 0..1 + r.below(4) {
            match r.below(12) {
                0 => c.first = random_u64(r, near),
                1 => c.lost = random_u64(r, near),
                2 => c.floor = random_u64(r, near),
                3 => c.epoch = r.next() as u32,
                4 => {
                    c.rows = (0..r.below(40))
                        .map(|_| Arc::from(random_text(r)))
                        .collect();
                }
                5 => {
                    c.gaps = (0..r.below(6))
                        .map(|_| AltArchiveGap {
                            after: random_u64(r, near),
                            kind: *r.pick(&[
                                AltArchiveGapKind::Jump,
                                AltArchiveGapKind::Resize,
                                AltArchiveGapKind::Reset,
                            ]),
                        })
                        .collect();
                }
                6 => c.enabled = r.below(4) != 0,
                7 => c.differ = None,
                _ => {
                    let d = c.differ.get_or_insert_with(AltArchiveDiffer::default);
                    match r.below(8) {
                        0 => {
                            d.prev = Some((
                                r.below(3) as u16 * 50,
                                (0..r.below(30)).map(|_| random_text(r)).collect(),
                            ));
                        }
                        1 => d.chrome = (r.below(2) == 0).then(|| r.below(40) as usize),
                        2 => d.pin = r.below(40) as usize,
                        3 => d.debt = r.below(40) as usize,
                        4 => d.debt_at = random_u64(r, near),
                        5 => d.below = (0..r.below(30)).map(|_| random_text(r)).collect(),
                        6 => d.esu_seen = !d.esu_seen,
                        _ => d.prev = None,
                    }
                }
            }
        }
        c
    }

    /// Design test 6 / M2: whatever a carry says, the import and every frame
    /// after it are panic-free, and the reads stay in range. Corrupt carries
    /// are made from real ones, a field or four at a time, so they sit right
    /// on the validation's edges.
    #[test]
    fn no_carry_panics_the_import_or_any_frame_after_it() {
        let mut outcomes = [0usize; 3];
        for seed in 1..=300u64 {
            let mut r = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
            let mut w = Walk::new();
            let mut a = AltArchive::with_limits(32 * 1024, 2048);
            for _ in 0..1 + r.below(120) {
                let s = w.step(&mut r);
                apply(&mut a, &s);
            }
            let base = whole(&a);
            let c = random_carry(&mut r, &base);
            let mut b = AltArchive::with_limits(32 * 1024, 2048);
            let outcome = b.import(c);
            outcomes[outcome as usize] += 1;
            back_in_range(&b, &format!("seed {seed} import"));
            for step in 0..80 {
                let s = w.step(&mut r);
                apply(&mut b, &s);
                let what = format!("seed {seed} step {step}");
                back_in_range(&b, &what);
                let read = b.read(AltArchiveQuery::newest(0, 5));
                assert!(read.last >= read.oldest.saturating_sub(1), "{what}");
                assert!(b.bytes() <= b.budget(), "{what}: over budget");
            }
        }
        eprintln!("exact/no-baseline/refused = {outcomes:?}");
        assert!(
            outcomes.iter().all(|&n| n > 0),
            "every outcome exercised: {outcomes:?}"
        );
    }
}
