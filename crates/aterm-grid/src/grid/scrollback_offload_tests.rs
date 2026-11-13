// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use aterm_scrollback::Scrollback;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn tiered_grid(rows: u16, cols: u16, ring: usize) -> Grid {
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    Grid::with_tiered_scrollback(rows, cols, ring, sb)
}

fn logical_line(grid: &mut Grid, text: &str) {
    use crate::{CellFlags, PackedColor};
    for c in text.chars() {
        // Deferred autowrap: long text soft-wraps.
        if aterm_grapheme::char_width(c) == 2 {
            grid.write_wide_char_wrap_styled(
                c,
                PackedColor::DEFAULT_FG,
                PackedColor::DEFAULT_BG,
                CellFlags::empty(),
            );
        } else {
            grid.write_char_wrap(c);
        }
    }
    grid.line_feed();
    grid.carriage_return();
}

fn feed_mixed_history(grid: &mut Grid, seed: u64, logical_lines: usize, cols: u16) {
    let mut rng = Rng(seed);
    for i in 0..logical_lines {
        match rng.below(5) {
            0 => logical_line(grid, ""),
            1 => logical_line(grid, &format!("S{i}")),
            2 => {
                let len = cols as usize * (2 + rng.below(4) as usize)
                    + rng.below(u64::from(cols)) as usize;
                let mut s = format!("R{i}-");
                while s.len() < len {
                    s.push((b'a' + rng.below(26) as u8) as char);
                }
                logical_line(grid, &s);
            }
            3 => {
                let n = 1 + rng.below(u64::from(cols)) as usize;
                let mut s = format!("W{i}-");
                for _ in 0..n {
                    s.push(if rng.below(2) == 0 { '世' } else { '界' });
                }
                logical_line(grid, &s);
            }
            _ => {
                let pad = "x".repeat(rng.below(u64::from(cols)) as usize);
                logical_line(grid, &format!("M{i}-{pad}"));
            }
        }
    }
}

fn store_fingerprint(store: &ScrollbackStorage) -> Vec<String> {
    (0..store.line_count())
        .map(|i| format!("{:?}", store.get_line(i).expect("decode").expect("present")))
        .collect()
}

fn reflowed_fingerprint(reflowed: &ReflowedScrollback) -> Vec<String> {
    let mut fp = store_fingerprint(&reflowed.store);
    fp.extend(reflowed.ring_out.iter().map(|line| format!("{line:?}")));
    fp
}

fn step_with_schedule(
    mut job: PendingScrollbackReflow,
    rng: &mut Rng,
    max_budget: u64,
) -> (ReflowedScrollback, usize) {
    let mut steps = 0usize;
    loop {
        steps += 1;
        assert!(steps < 1_000_000, "stepping must terminate");
        match job.reflow_step(1 + rng.below(max_budget) as usize) {
            ReflowStep::InProgress(next) => job = next,
            ReflowStep::Done(done) => return (done, steps),
        }
    }
}

#[test]
fn detached_settings_apply_on_normal_reattach() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    g.set_scrollback_line_limit(Some(200));
    g.set_scrollback_memory_budget(7_000_000)
        .expect("initial budget");
    for i in 0..400 {
        logical_line(&mut g, &format!("H{i}"));
    }
    assert!(g.storage.pending_scrollback_settings.is_none());

    let job = g
        .resize_offloading_scrollback(rows, 20)
        .expect("tiered store detaches");
    assert!(
        g.storage.pending_scrollback_settings.is_some(),
        "settings snapshot exists exactly while detach is unresolved"
    );
    assert_eq!(
        g.scrollback_line_limit(),
        Some(200),
        "detach keeps the effective total observable"
    );
    assert_eq!(g.scrollback_memory_budget(), Some(7_000_000));

    g.set_scrollback_line_limit(Some(40));
    g.set_scrollback_memory_budget(4_000_000)
        .expect("detached mutation is deferred");
    assert_eq!(g.scrollback_line_limit(), Some(40));
    assert_eq!(g.scrollback_memory_budget(), Some(4_000_000));
    assert!(g.scrollback().is_none(), "the worker still owns the store");

    g.reattach_reflowed_scrollback(job.reflow());
    assert!(
        g.storage.pending_scrollback_settings.is_none(),
        "reattach consumes the snapshot exactly once"
    );
    let store = g.scrollback().expect("store re-attached");
    assert_eq!(g.scrollback_line_limit(), Some(40));
    assert_eq!(
        store.line_limit(),
        Some(32),
        "raw store share is total 40 minus the fixed 8-line ring"
    );
    assert_eq!(store.memory_budget(), 4_000_000);
    assert!(
        g.scrollback_lines() <= 40,
        "deferred shrink enforces the unified total immediately"
    );
    g.assert_invariants();
}

#[test]
fn detached_unlimited_limit_round_trips_on_reattach() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    g.set_scrollback_line_limit(Some(40));
    for i in 0..100 {
        logical_line(&mut g, &format!("U{i}"));
    }

    let job = g
        .resize_offloading_scrollback(rows, 20)
        .expect("tiered store detaches");
    g.set_scrollback_line_limit(None);
    assert_eq!(
        g.scrollback_line_limit(),
        None,
        "nested pending state must distinguish unlimited from no mutation"
    );

    g.reattach_reflowed_scrollback(job.reflow());
    assert!(g.storage.pending_scrollback_settings.is_none());
    assert_eq!(g.scrollback_line_limit(), None);
    assert_eq!(
        g.scrollback().expect("store re-attached").line_limit(),
        None,
        "unlimited reaches the raw store"
    );
    g.assert_invariants();
}

#[test]
fn detached_settings_survive_clear_before_reattach() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    logical_line(&mut g, "PRE_CLEAR_ONLY");
    for i in 0..200 {
        logical_line(&mut g, &format!("C{i}"));
    }

    let job = g
        .resize_offloading_scrollback(rows, 20)
        .expect("tiered store detaches");
    g.set_scrollback_line_limit(Some(25));
    g.set_scrollback_memory_budget(3_000_000)
        .expect("detached mutation is deferred");
    g.erase_scrollback();
    for i in 0..30 {
        logical_line(&mut g, &format!("POST_CLEAR_{i}"));
    }

    g.reattach_reflowed_scrollback(job.reflow());
    let store = g.scrollback().expect("cleared store re-attached");
    assert_eq!(g.scrollback_line_limit(), Some(25));
    assert_eq!(store.line_limit(), Some(17));
    assert_eq!(store.memory_budget(), 3_000_000);
    let history = (0..g.scrollback_lines())
        .filter_map(|i| g.get_history_line(i))
        .map(|line| line.to_string())
        .collect::<String>();
    assert!(
        history.contains("POST_CLEAR_"),
        "output produced after clear survives the detach window"
    );
    assert!(
        !history.contains("PRE_CLEAR_ONLY"),
        "clear during reflow must not resurrect pre-clear history"
    );
    assert!(g.storage.pending_scrollback_settings.is_none());
    g.assert_invariants();
}

#[test]
fn detached_settings_apply_to_replacement_store_race() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    for i in 0..200 {
        logical_line(&mut g, &format!("R{i}"));
    }

    let job = g
        .resize_offloading_scrollback(rows, 20)
        .expect("tiered store detaches");
    g.set_scrollback_line_limit(Some(30));
    g.set_scrollback_memory_budget(2_000_000)
        .expect("detached mutation is deferred");
    g.erase_scrollback();
    let mut replacement: ScrollbackStorage = Scrollback::new(64, 512, 6_000_000).into();
    replacement
        .push_line(Line::from("REPLACEMENT_STORE_ONLY"))
        .expect("seed replacement");
    g.attach_scrollback(replacement);
    assert_eq!(
        g.scrollback_line_limit(),
        Some(30),
        "dirty settings apply as soon as the replacement attaches"
    );
    assert_eq!(g.scrollback_memory_budget(), Some(2_000_000));

    g.set_scrollback_line_limit(Some(35));
    g.set_scrollback_memory_budget(1_500_000)
        .expect("latest replacement budget");

    g.reattach_reflowed_scrollback(job.reflow());
    let store = g.scrollback().expect("replacement store remains attached");
    assert!(
        (0..store.line_count()).any(|i| {
            store
                .get_line(i)
                .ok()
                .flatten()
                .and_then(|line| line.as_str().map(|text| text == "REPLACEMENT_STORE_ONLY"))
                .unwrap_or(false)
        }),
        "the stale worker store must not clobber replacement content"
    );
    assert_eq!(g.scrollback_line_limit(), Some(35));
    assert_eq!(store.line_limit(), Some(27));
    assert_eq!(store.memory_budget(), 1_500_000);
    assert!(g.storage.pending_scrollback_settings.is_none());
    g.assert_invariants();
}

#[test]
fn clean_detach_snapshot_adopts_replacement_store_settings() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    for i in 0..100 {
        logical_line(&mut g, &format!("B{i}"));
    }
    let job = g
        .resize_offloading_scrollback(rows, 20)
        .expect("tiered store detaches");
    g.erase_scrollback();

    let mut replacement: ScrollbackStorage = Scrollback::new(64, 512, 2_500_000).into();
    replacement.set_line_limit(Some(12));
    replacement
        .push_line(Line::from("NEW_BASELINE"))
        .expect("seed replacement");
    g.attach_scrollback(replacement);
    assert_eq!(
        g.scrollback_line_limit(),
        Some(20),
        "clean snapshot follows replacement raw 12 + ring 8"
    );
    assert_eq!(g.scrollback_memory_budget(), Some(2_500_000));

    g.reattach_reflowed_scrollback(job.reflow());
    let store = g.scrollback().expect("replacement remains authoritative");
    assert_eq!(g.scrollback_line_limit(), Some(20));
    assert_eq!(store.line_limit(), Some(12));
    assert_eq!(store.memory_budget(), 2_500_000);
    assert!(g.storage.pending_scrollback_settings.is_none());
    g.assert_invariants();
}

#[test]
fn detached_line_limit_applies_to_ring_after_abort() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    for i in 0..200 {
        logical_line(&mut g, &format!("A{i}"));
    }

    let job = g
        .resize_offloading_scrollback(rows, 20)
        .expect("tiered store detaches");
    g.set_scrollback_line_limit(Some(5));
    g.set_scrollback_memory_budget(1_000_000)
        .expect("detached mutation is deferred");
    drop(job);
    g.abort_reflow_offload();

    assert!(g.storage.pending_scrollback_settings.is_none());
    assert!(g.scrollback().is_none(), "the failed worker lost its store");
    assert_eq!(
        g.scrollback_memory_budget(),
        None,
        "a byte budget has no backend after abort"
    );
    assert_eq!(
        g.scrollback_line_limit(),
        Some(5),
        "the requested total still caps the surviving ring"
    );
    for i in 0..100 {
        logical_line(&mut g, &format!("P{i}"));
    }
    assert_eq!(g.scrollback_lines(), 5);
    g.assert_invariants();
}

#[test]
fn abort_never_expands_fallback_ring_for_raise_or_unlimited() {
    for requested in [Some(100), None] {
        let (rows, cols) = (10u16, 40u16);
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..100 {
            logical_line(&mut g, &format!("E{i}"));
        }
        let job = g
            .resize_offloading_scrollback(rows, 20)
            .expect("tiered store detaches");
        g.set_scrollback_line_limit(requested);
        drop(job);
        g.abort_reflow_offload();

        assert_eq!(
            g.scrollback_line_limit(),
            Some(8),
            "emergency ring stays construction-bounded for request {requested:?}"
        );
        assert!(g.storage.pending_scrollback_settings.is_none());
        for i in 0..100 {
            logical_line(&mut g, &format!("F{i}"));
        }
        assert_eq!(g.scrollback_lines(), 8);
        g.assert_invariants();
    }
}

#[test]
fn abort_preserves_staged_output_when_replacement_store_exists() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    for i in 0..100 {
        logical_line(&mut g, &format!("OLD{i}"));
    }
    let job = g
        .resize_offloading_scrollback(rows, 20)
        .expect("tiered store detaches");
    g.erase_scrollback();
    g.set_scrollback_line_limit(Some(100));
    g.set_scrollback_memory_budget(2_000_000)
        .expect("defer replacement budget");

    let mut replacement: ScrollbackStorage = Scrollback::new(64, 512, 6_000_000).into();
    replacement
        .push_line(Line::from("ABORT_REPLACEMENT_BASE"))
        .expect("seed replacement");
    g.attach_scrollback(replacement);
    g.set_compress_offload_active(true);
    for i in 0..30 {
        logical_line(&mut g, &format!("WINDOW_AFTER_REPLACEMENT_{i}"));
    }
    assert!(
        g.lazy_backlog_len() > 0,
        "precondition: replacement-window rows are staged"
    );

    drop(job);
    g.abort_reflow_offload();
    assert!(!g.reflow_offload_in_flight());
    assert_eq!(g.lazy_backlog_len(), 0, "abort drains into replacement");
    assert!(g.storage.pending_scrollback_settings.is_none());
    assert_eq!(g.scrollback_line_limit(), Some(100));
    assert_eq!(g.scrollback_memory_budget(), Some(2_000_000));
    let history = (0..g.scrollback_lines())
        .filter_map(|i| g.get_history_line(i))
        .map(|line| line.to_string())
        .collect::<String>();
    assert!(history.contains("ABORT_REPLACEMENT_BASE"));
    assert!(
        history.contains("WINDOW_AFTER_REPLACEMENT_"),
        "abort must preserve rows staged after replacement attach"
    );
    g.assert_invariants();
}

#[test]
fn attached_memory_budget_settles_lazy_rows_under_new_budget() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    g.set_compress_offload_active(true);
    for i in 0..100 {
        logical_line(&mut g, &format!("M{i}"));
    }
    assert!(g.lazy_backlog_len() > 0, "precondition: rows are staged");
    let before = g.scrollback_lines();

    g.set_scrollback_memory_budget(4_000_000)
        .expect("new budget enforces after staged rows are drained");

    assert_eq!(g.lazy_backlog_len(), 0);
    assert_eq!(g.scrollback_memory_budget(), Some(4_000_000));
    assert_eq!(
        g.scrollback_lines(),
        before,
        "a generous budget moves staged rows without losing history"
    );
    g.assert_invariants();
}

#[test]
fn a_width_change_leaves_the_staged_rows_raw_for_its_job() {
    let (rows, cols) = (10u16, 40u16);
    let detach = |staged: usize| {
        let mut g = tiered_grid(rows, cols, 8);
        for i in 0..50 {
            logical_line(&mut g, &format!("H{i}"));
        }
        g.set_compress_offload_active(true);
        for i in 0..staged {
            logical_line(&mut g, &format!("S{i}"));
        }
        assert!(
            g.lazy_backlog_len() + usize::from(rows) + 8 >= staged,
            "precondition: {staged} rows are staged, not in the store"
        );
        crate::test_counters::take_row_to_line_ops();
        let job = g
            .resize_offloading_scrollback(rows, 23)
            .expect("a width change with a tiered store detaches");
        (g, job, crate::test_counters::take_row_to_line_ops())
    };
    let (_, _, few) = detach(20);
    let (mut g, job, many) = detach(600);
    assert_eq!(many, few, "the detach materializes no staged row");

    g.reattach_reflowed_scrollback(job.reflow());
    let history: Vec<String> = (0..g.scrollback_lines())
        .filter_map(|i| g.get_history_line(i))
        .map(|line| line.to_string())
        .collect();
    let staged: Vec<&str> = history
        .iter()
        .map(|line| line.trim_end())
        .filter(|line| line.starts_with('S'))
        .collect();
    let want: Vec<String> = (0..staged.len()).map(|i| format!("S{i}")).collect();
    assert_eq!(staged, want, "every staged row reached history, in order");
    assert!(
        staged.len() + usize::from(rows) >= 600,
        "{} staged rows in history",
        staged.len()
    );
    g.assert_invariants();
}

#[test]
fn a_redetach_leaves_the_staged_rows_raw_for_its_job() {
    let rows = 10u16;
    let redetach = |staged: usize| {
        let mut g = tiered_grid(rows, 40, 8);
        for i in 0..50 {
            logical_line(&mut g, &format!("H{i}"));
        }
        let job = g
            .resize_offloading_scrollback(rows, 23)
            .expect("a width change with a tiered store detaches");
        for i in 0..staged {
            logical_line(&mut g, &format!("S{i}"));
        }
        g.resize(rows, 31);
        let reflowed = job.reflow();
        crate::test_counters::take_row_to_line_ops();
        let follow = g
            .reattach_reflowed_scrollback_or_redetach(reflowed)
            .expect("a width that moved on redetaches");
        (g, follow, crate::test_counters::take_row_to_line_ops())
    };
    let (_, _, few) = redetach(20);
    let (mut g, follow, many) = redetach(600);
    assert_eq!(many, few, "the redetach materializes no staged row");

    assert!(
        g.reattach_reflowed_scrollback_or_redetach(follow.reflow())
            .is_none()
    );
    let staged: Vec<String> = (0..g.scrollback_lines())
        .filter_map(|i| g.get_history_line(i))
        .map(|line| line.to_string().trim_end().to_owned())
        .filter(|line| line.starts_with('S'))
        .collect();
    let want: Vec<String> = (0..staged.len()).map(|i| format!("S{i}")).collect();
    assert_eq!(staged, want, "every staged row reached history, in order");
    assert!(
        staged.len() + usize::from(rows) >= 600,
        "{} staged rows in history",
        staged.len()
    );
    g.assert_invariants();
}

#[test]
fn reflow_step_any_schedule_matches_one_shot() {
    for seed in [1u64, 7, 42, 0x00C0_FFEE] {
        let (rows, cols, new_cols) = (10u16, 40u16, 23u16);
        let mut a = tiered_grid(rows, cols, 8);
        let mut b = tiered_grid(rows, cols, 8);
        for g in [&mut a, &mut b] {
            feed_mixed_history(g, seed, 400, cols);
            g.set_compress_offload_active(true);
            feed_mixed_history(g, seed ^ 0xABCD, 40, cols);
        }
        assert!(
            a.lazy_backlog_len() > 0,
            "seed {seed}: precondition — a staged lazy tail exists"
        );

        let one_shot = a
            .resize_offloading_scrollback(rows, new_cols)
            .expect("width change with a tiered store detaches")
            .reflow();
        let job = b
            .resize_offloading_scrollback(rows, new_cols)
            .expect("width change with a tiered store detaches");
        let mut rng = Rng(seed ^ 0x5EED);
        let (stepped, steps) = step_with_schedule(job, &mut rng, 37);

        assert!(
            steps > 4,
            "seed {seed}: the schedule must actually chunk (took {steps} steps)"
        );
        assert_eq!(stepped.new_cols(), one_shot.new_cols());
        assert_eq!(
            stepped.line_count(),
            one_shot.line_count(),
            "seed {seed}: line counts diverge"
        );
        assert_eq!(
            reflowed_fingerprint(&stepped),
            reflowed_fingerprint(&one_shot),
            "seed {seed}: stepped content != one-shot content"
        );
    }
}

#[test]
fn reflow_step_budget_one_never_splits_a_soft_wrapped_run() {
    let (rows, cols, new_cols) = (10u16, 20u16, 33u16);
    let giant = {
        let mut s = String::from("G-");
        while s.len() < cols as usize * 8 {
            s.push_str("abcdefghij");
        }
        s
    };
    let mut a = tiered_grid(rows, cols, 8);
    let mut b = tiered_grid(rows, cols, 8);
    for g in [&mut a, &mut b] {
        logical_line(g, "before");
        logical_line(g, &giant);
        logical_line(g, "after");
        for i in 0..300 {
            logical_line(g, &format!("pad{i}"));
        }
    }
    let one_shot = a
        .resize_offloading_scrollback(rows, new_cols)
        .expect("job")
        .reflow();
    let mut job = b.resize_offloading_scrollback(rows, new_cols).expect("job");
    let mut steps = 0usize;
    let stepped = loop {
        steps += 1;
        assert!(steps < 100_000, "budget-1 stepping must terminate");
        match job.reflow_step(1) {
            ReflowStep::InProgress(next) => job = next,
            ReflowStep::Done(done) => break done,
        }
    };
    assert_eq!(
        reflowed_fingerprint(&stepped),
        reflowed_fingerprint(&one_shot),
        "budget-1 stepping must match the one-shot"
    );

    b.reattach_reflowed_scrollback(stepped);
    let mut joined = String::new();
    for i in 0..b.scrollback_lines() {
        if let Some(line) = b.get_history_line(i) {
            joined.push_str(line.as_str().unwrap_or(""));
        }
    }
    assert!(
        joined.contains(&giant),
        "the soft-wrapped run's content must survive budget-1 stepping \
         (giant {} chars, joined {} chars)",
        giant.len(),
        joined.len()
    );
    b.assert_invariants();
}

#[test]
fn reflow_step_empty_history_terminates() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    logical_line(&mut g, "visible only");
    let job = g
        .resize_offloading_scrollback(rows, 20)
        .expect("a tiered store detaches even when empty");
    let mut rng = Rng(3);
    let (done, _steps) = step_with_schedule(job, &mut rng, 4);
    assert_eq!(done.line_count(), 0);
    g.reattach_reflowed_scrollback(done);
    g.assert_invariants();
}

#[test]
fn reflow_step_zero_budget_still_makes_progress() {
    let (rows, cols) = (10u16, 30u16);
    let mut g = tiered_grid(rows, cols, 8);
    for i in 0..50 {
        logical_line(&mut g, &format!("Z{i}"));
    }
    let mut job = g.resize_offloading_scrollback(rows, 15).expect("job");
    let mut steps = 0usize;
    loop {
        steps += 1;
        assert!(steps < 10_000, "budget 0 must clamp to 1 and terminate");
        match job.reflow_step(0) {
            ReflowStep::InProgress(next) => job = next,
            ReflowStep::Done(done) => {
                g.reattach_reflowed_scrollback(done);
                break;
            }
        }
    }
    assert!(g.scrollback_lines() > 30, "history preserved");
    g.assert_invariants();
}

#[test]
fn reflow_progress_climbs_to_the_detached_total() {
    let (rows, cols) = (10u16, 30u16);
    let mut g = tiered_grid(rows, cols, 8);
    for i in 0..400 {
        logical_line(&mut g, &format!("P{i}"));
    }
    let mut job = g.resize_offloading_scrollback(rows, 15).expect("job");
    let total = job.lines_total();
    assert_eq!(total, job.line_count(), "the total is what was detached");
    assert!(total > 300, "{total}");
    assert_eq!(job.lines_done(), 0);
    let mut last = 0;
    let mut steps = 0;
    loop {
        steps += 1;
        match job.reflow_step(37) {
            ReflowStep::InProgress(next) => {
                assert!(next.lines_done() >= last, "monotone");
                assert!(next.lines_done() <= total);
                assert_eq!(next.lines_total(), total, "fixed at detach");
                last = next.lines_done();
                job = next;
            }
            ReflowStep::Done(done) => {
                assert_eq!(
                    last + 37.min(total - last),
                    total,
                    "the last step read the rest"
                );
                g.reattach_reflowed_scrollback(done);
                break;
            }
        }
    }
    assert!(steps > 5, "{steps}");
    g.assert_invariants();
}

#[test]
fn dropping_a_half_stepped_job_recovers_via_abort() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    for i in 0..500 {
        logical_line(&mut g, &format!("H{i}"));
    }
    let job = g.resize_offloading_scrollback(rows, 20).expect("job");
    let ReflowStep::InProgress(half) = job.reflow_step(5) else {
        panic!("a 500-line history cannot complete in one 5-line step");
    };
    drop(half);
    g.abort_reflow_offload();
    for i in 0..5000 {
        logical_line(&mut g, &format!("R{i}"));
    }
    assert!(
        g.scrollback_lines() < 1000,
        "post-abort the grid is ring-only bounded, not wedged (got {})",
        g.scrollback_lines()
    );
    g.assert_invariants();
}

#[test]
fn stepping_interleaved_with_window_output_keeps_history_and_order() {
    let (rows, cols) = (10u16, 40u16);
    let mut g = tiered_grid(rows, cols, 8);
    let mut plain = tiered_grid(rows, cols, 8);
    for i in 0..400 {
        let line = format!("H{i}");
        logical_line(&mut g, &line);
        logical_line(&mut plain, &line);
    }
    let mut job = g.resize_offloading_scrollback(rows, 20).expect("job");
    plain.resize(rows, 20);
    let mut wrote = 0usize;
    let done = loop {
        for _ in 0..3 {
            let line = format!("W{wrote}");
            logical_line(&mut g, &line);
            logical_line(&mut plain, &line);
            wrote += 1;
        }
        match job.reflow_step(64) {
            ReflowStep::InProgress(next) => job = next,
            ReflowStep::Done(done) => break done,
        }
    };
    g.reattach_reflowed_scrollback(done);
    assert_eq!(
        history_seq(&g),
        history_seq(&plain),
        "interleaved offload stepping must equal synchronous plain resize history"
    );
    assert_eq!(
        screen_rows(&g),
        screen_rows(&plain),
        "interleaved offload stepping must equal synchronous plain resize screen"
    );
    g.assert_invariants();
}

struct Drag {
    label: &'static str,
    policy: ResizePolicy,
    ring: usize,
    rows: u16,
    cols: u16,
    steps: &'static [(u16, u16)],
    output: usize,
}

const DRAGS: [Drag; 13] = [
    Drag {
        label: "one narrowing step, the ring holds its overflow",
        policy: ResizePolicy::Native,
        ring: 32,
        rows: 10,
        cols: 40,
        steps: &[(10, 23)],
        output: 0,
    },
    Drag {
        label: "a ring larger than the window holds back",
        policy: ResizePolicy::Native,
        ring: 64,
        rows: 4,
        cols: 40,
        steps: &[(4, 23), (6, 30), (5, 26)],
        output: 0,
    },
    Drag {
        label: "one widening step",
        policy: ResizePolicy::Native,
        ring: 8,
        rows: 10,
        cols: 23,
        steps: &[(10, 40)],
        output: 0,
    },
    Drag {
        label: "a drag whose later steps land inside the window",
        policy: ResizePolicy::Native,
        ring: 8,
        rows: 13,
        cols: 40,
        steps: &[(10, 35), (4, 30), (3, 28)],
        output: 0,
    },
    Drag {
        label: "a drag with output streaming between its steps",
        policy: ResizePolicy::Native,
        ring: 8,
        rows: 13,
        cols: 40,
        steps: &[(10, 35), (4, 30), (3, 28)],
        output: 12,
    },
    Drag {
        label: "a drag back to the job's width with output staged between",
        policy: ResizePolicy::Native,
        ring: 4,
        rows: 8,
        cols: 40,
        steps: &[(8, 31), (8, 26), (8, 31)],
        output: 12,
    },
    Drag {
        label: "a grow inside the window after output filled the screen",
        policy: ResizePolicy::Native,
        ring: 2,
        rows: 10,
        cols: 17,
        steps: &[(6, 15), (15, 16)],
        output: 8,
    },
    Drag {
        label: "a widening inside the window after output filled the screen",
        policy: ResizePolicy::Native,
        ring: 2,
        rows: 10,
        cols: 17,
        steps: &[(10, 15), (10, 30)],
        output: 8,
    },
    Drag {
        label: "a rows-only grow inside the window after output",
        policy: ResizePolicy::Native,
        ring: 2,
        rows: 10,
        cols: 17,
        steps: &[(6, 15), (15, 15)],
        output: 8,
    },
    Drag {
        label: "one step pushing more rows than the ring holds",
        policy: ResizePolicy::Native,
        ring: 8,
        rows: 13,
        cols: 40,
        steps: &[(3, 23)],
        output: 0,
    },
    Drag {
        label: "the same push under ConPTY",
        policy: ResizePolicy::ConPty,
        ring: 8,
        rows: 13,
        cols: 40,
        steps: &[(3, 23)],
        output: 0,
    },
    Drag {
        label: "one narrowing step under ConPTY",
        policy: ResizePolicy::ConPty,
        ring: 8,
        rows: 10,
        cols: 40,
        steps: &[(10, 23)],
        output: 0,
    },
    Drag {
        label: "a drag with output under ConPTY",
        policy: ResizePolicy::ConPty,
        ring: 8,
        rows: 13,
        cols: 40,
        steps: &[(10, 35), (4, 30), (3, 28)],
        output: 12,
    },
];

fn history_seq(g: &Grid) -> Vec<(String, bool)> {
    (0..g.scrollback_lines())
        .map(|i| {
            g.get_history_line(i).map_or_else(
                || (String::new(), false),
                |line| (line.as_str().unwrap_or("").to_string(), line.is_wrapped()),
            )
        })
        .collect()
}

fn screen_rows(g: &Grid) -> Vec<String> {
    (0..g.rows())
        .map(|r| g.row_text(r).unwrap_or_default())
        .collect()
}

#[test]
fn offload_round_trip_matches_plain_resize_history_sequence() {
    let (read, screen) = (history_seq, screen_rows);
    for drag in &DRAGS {
        for seed in [3u64, 11, 0xFEED] {
            let backs: &[i32] = if drag.output == 0 {
                &[0, 1 + (seed % 12) as i32]
            } else {
                &[0]
            };
            for &back in backs {
                let case = format!("{} / seed {seed} / back {back}", drag.label);
                let mut offloaded = tiered_grid(drag.rows, drag.cols, drag.ring);
                let mut plain = tiered_grid(drag.rows, drag.cols, drag.ring);
                for g in [&mut offloaded, &mut plain] {
                    feed_mixed_history(g, seed, 300, drag.cols);
                    g.scroll_display(back);
                }
                assert!(
                    offloaded.ring_buffer_scrollback() > 0,
                    "{case}: precondition — ring scrollback exists to ride the job"
                );

                let (first_rows, first_cols) = drag.steps[0];
                let job = offloaded
                    .resize_offloading_scrollback_with_policy(first_rows, first_cols, drag.policy)
                    .expect("width change with a tiered store detaches");
                let held = HOLD_BACK_SCREENS
                    * usize::from(drag.rows.max(first_rows))
                    * usize::from(drag.cols.max(first_cols))
                    / usize::from(drag.cols);
                assert!(
                    drag.ring <= held || !job.ring_lines.is_empty(),
                    "{case}: detach lifts the ring history past the hold-back into the job (RFL-1)"
                );
                plain.resize_with_policy(first_rows, first_cols, drag.policy);
                for (step, &(rows, cols)) in drag.steps[1..].iter().enumerate() {
                    for g in [&mut offloaded, &mut plain] {
                        feed_mixed_history(g, seed ^ step as u64, drag.output, cols);
                    }
                    assert!(
                        drag.output == 0 || !offloaded.storage.lazy_buffer.is_empty(),
                        "{case}: precondition — the window staged its output"
                    );
                    offloaded.resize_with_policy(rows, cols, drag.policy);
                    plain.resize_with_policy(rows, cols, drag.policy);
                    offloaded.storage.lazy_buffer.assert_running_sums();
                }
                let mut rng = Rng(seed ^ 0xD1FF);
                let mut next = Some(job);
                while let Some(job) = next {
                    let (done, _steps) = step_with_schedule(job, &mut rng, 29);
                    next = offloaded.reattach_reflowed_scrollback_or_redetach(done);
                }
                let grows = drag
                    .steps
                    .windows(2)
                    .any(|w| w[1].0 > w[0].0 || w[1].1 > w[0].1);
                assert!(
                    offloaded.ring_buffer_scrollback() > 0 || grows,
                    "{case}: re-attach returns ring history to the ring"
                );

                assert_eq!(
                    read(&offloaded),
                    read(&plain),
                    "{case}: offloaded history sequence != plain-resize sequence"
                );
                assert_eq!(
                    offloaded.display_offset(),
                    plain.display_offset(),
                    "{case}: the reader's place"
                );
                assert_eq!(screen(&offloaded), screen(&plain), "{case}: the screen");
                offloaded.assert_invariants();
                plain.assert_invariants();
            }
        }
    }
}

fn logical_lines(g: &Grid) -> Vec<String> {
    let rows = history_seq(g).into_iter().chain((0..g.rows()).map(|r| {
        let wrapped = g
            .storage
            .row_at_screen(r)
            .is_some_and(|row| row.is_wrapped());
        (g.row_text(r).unwrap_or_default(), wrapped)
    }));
    let mut lines: Vec<String> = Vec::new();
    for (text, wrapped) in rows {
        match lines.last_mut() {
            Some(line) if wrapped => line.push_str(&text),
            _ => lines.push(text),
        }
    }
    lines.retain(|line| !line.is_empty());
    lines
}

#[test]
fn random_offloaded_drags_end_where_the_plain_resize_does() {
    let mut exact = 0;
    for policy in [ResizePolicy::Native, ResizePolicy::ConPty] {
        for seed in 0..300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9) ^ 0x5EED);
            let rows = 2 + rng.below(13) as u16;
            let cols = 5 + rng.below(40) as u16;
            let ring = 1 + rng.below(20) as usize;
            let mut steps: Vec<(u16, u16)> = (0..1 + rng.below(4))
                .map(|_| (2 + rng.below(13) as u16, 5 + rng.below(40) as u16))
                .collect();
            if steps[0].1 == cols {
                steps[0].1 += 1;
            }
            let output = [0, 5, 10][rng.below(3) as usize];
            let redrawn = (rng.below(2) == 0).then(|| rng.below(u64::from(rows)) as usize);
            let prompt = rng.below(2) == 0;
            let case = format!(
                "{policy:?} seed {seed}: {rows}x{cols} ring {ring}, steps {steps:?}, \
                 output {output}, redrawn {redrawn:?}, prompt {prompt}"
            );
            let redraw = |g: &mut Grid| {
                if prompt {
                    g.carriage_return();
                    g.erase_to_end_of_screen();
                    "$ ".chars().for_each(|c| g.write_char_wrap(c));
                }
            };
            let mut offloaded = tiered_grid(rows, cols, ring);
            let mut plain = tiered_grid(rows, cols, ring);
            for g in [&mut offloaded, &mut plain] {
                feed_mixed_history(g, seed, 120, cols);
                if let Some(lines) = redrawn {
                    g.erase_screen();
                    g.set_cursor(0, 0);
                    feed_mixed_history(g, seed ^ 0xC1EA, lines, cols);
                }
            }
            let job = offloaded
                .resize_offloading_scrollback_with_policy(steps[0].0, steps[0].1, policy)
                .expect("a width change with a tiered store detaches");
            plain.resize_with_policy(steps[0].0, steps[0].1, policy);
            redraw(&mut offloaded);
            redraw(&mut plain);
            for (k, &(rows, cols)) in steps[1..].iter().enumerate() {
                for g in [&mut offloaded, &mut plain] {
                    feed_mixed_history(g, seed ^ (k as u64 + 7), output, cols);
                    g.resize_with_policy(rows, cols, policy);
                    redraw(g);
                }
            }
            let mut schedule = Rng(seed ^ 0xD1FF);
            let mut next = Some(job);
            while let Some(job) = next {
                let (done, _steps) = step_with_schedule(job, &mut schedule, 29);
                next = offloaded.reattach_reflowed_scrollback_or_redetach(done);
            }
            assert_eq!(
                logical_lines(&offloaded),
                logical_lines(&plain),
                "{case}: every line, once and in order"
            );
            if redrawn.is_none() || steps.len() == 1 {
                exact += 1;
                assert_eq!(
                    history_seq(&offloaded),
                    history_seq(&plain),
                    "{case}: history"
                );
                assert_eq!(
                    screen_rows(&offloaded),
                    screen_rows(&plain),
                    "{case}: screen"
                );
                assert_eq!(
                    offloaded.display_offset(),
                    plain.display_offset(),
                    "{case}: the reader's place"
                );
            }
            offloaded.assert_invariants();
            plain.assert_invariants();
        }
    }
    assert!(
        exact > 300,
        "only {exact} drags were held to the plain resize's rows"
    );
}

/// What a program does inside a detach window, the same on both grids of a
/// differential: output, a prompt redrawn on its row (`CR`, `ED 0`), one
/// redrawn from rows above it (`CUU`, `CR`, `ED 0`, as a multi-line prompt
/// does), a write at an addressed row (`CUP`), or a screen cleared and drawn
/// again (`ED 2`).
#[derive(Clone, Copy, Debug)]
enum WindowAction {
    Output,
    Prompt,
    PromptAbove,
    Addressed,
    Cleared,
}

fn window_action(g: &mut Grid, action: WindowAction, seed: u64) {
    let mut rng = Rng(seed);
    match action {
        WindowAction::Output => feed_mixed_history(g, seed, 5, g.cols()),
        WindowAction::Prompt | WindowAction::PromptAbove => {
            if let WindowAction::PromptAbove = action {
                g.cursor_up(1 + rng.below(3) as u16);
            }
            g.carriage_return();
            g.erase_to_end_of_screen();
            "$ ".chars().for_each(|c| g.write_char_wrap(c));
        }
        WindowAction::Addressed => {
            g.set_cursor(rng.below(u64::from(g.rows())) as u16, 0);
            g.write_char_wrap('Z');
        }
        WindowAction::Cleared => {
            g.erase_screen();
            g.set_cursor(0, 0);
            feed_mixed_history(g, seed, 2, g.cols());
        }
    }
}

#[test]
fn random_offloaded_drags_over_a_full_ring_end_where_the_plain_resize_does() {
    use WindowAction::{Addressed, Cleared, Output, Prompt, PromptAbove};
    let (mut drags, mut owed_drags) = (0, 0);
    for policy in [ResizePolicy::Native, ResizePolicy::ConPty] {
        for action in [Output, Prompt, PromptAbove, Addressed, Cleared] {
            for seed in 0..100u64 {
                let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9) ^ 0xF011);
                let rows = 2 + rng.below(13) as u16;
                let cols = 5 + rng.below(40) as u16;
                let mut steps: Vec<(u16, u16)> = (0..1 + rng.below(4))
                    .map(|_| (2 + rng.below(13) as u16, 5 + rng.below(40) as u16))
                    .collect();
                if steps[0].1 == cols {
                    steps[0].1 += 1;
                }
                let redrawn = (rng.below(2) == 0).then(|| rng.below(u64::from(rows)) as usize);
                let act_seed = rng.next();
                let case = format!(
                    "{policy:?} {action:?} seed {seed}: {rows}x{cols}, steps {steps:?}, \
                     redrawn {redrawn:?}"
                );
                let mut offloaded = tiered_grid(rows, cols, 300);
                let mut plain = tiered_grid(rows, cols, 300);
                for g in [&mut offloaded, &mut plain] {
                    feed_mixed_history(g, seed, 120, cols);
                    if let Some(lines) = redrawn {
                        g.erase_screen();
                        g.set_cursor(0, 0);
                        feed_mixed_history(g, seed ^ 0xC1EA, lines, cols);
                    }
                }
                let job = offloaded
                    .resize_offloading_scrollback_with_policy(steps[0].0, steps[0].1, policy)
                    .expect("a width change with a tiered store detaches");
                plain.resize_with_policy(steps[0].0, steps[0].1, policy);
                let mut owed = offloaded.storage.fill_debt != FillDebt::default();
                for (k, &(rows, cols)) in steps.iter().enumerate() {
                    if k > 0 {
                        offloaded.resize_with_policy(rows, cols, policy);
                        plain.resize_with_policy(rows, cols, policy);
                        owed |= offloaded.storage.fill_debt != FillDebt::default();
                    }
                    window_action(&mut offloaded, action, act_seed ^ k as u64);
                    window_action(&mut plain, action, act_seed ^ k as u64);
                }
                let mut schedule = Rng(seed ^ 0xD1FF);
                let mut next = Some(job);
                while let Some(job) = next {
                    let (done, _steps) = step_with_schedule(job, &mut schedule, 29);
                    next = offloaded.reattach_reflowed_scrollback_or_redetach(done);
                }
                drags += 1;
                if owed {
                    owed_drags += 1;
                } else {
                    assert_eq!(
                        history_seq(&offloaded),
                        history_seq(&plain),
                        "{case}: history"
                    );
                    assert_eq!(
                        screen_rows(&offloaded),
                        screen_rows(&plain),
                        "{case}: screen"
                    );
                    assert_eq!(
                        offloaded.display_offset(),
                        plain.display_offset(),
                        "{case}: the reader's place"
                    );
                }
                offloaded.assert_invariants();
                plain.assert_invariants();
            }
        }
    }
    assert!(
        owed_drags * 50 < drags,
        "{owed_drags} of {drags} drags owed the re-attach rows: the window held too little back"
    );
}

#[test]
fn a_prompt_redrawn_from_rows_above_finds_the_rows_the_resize_pulled_down() {
    let mut offloaded = tiered_grid(5, 42, 11);
    let mut plain = tiered_grid(5, 42, 11);
    for g in [&mut offloaded, &mut plain] {
        feed_mixed_history(g, 5, 40, 42);
        logical_line(g, "first");
        let long: String = (0..178).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        logical_line(g, &long);
    }
    let job = offloaded
        .resize_offloading_scrollback(3, 12)
        .expect("a width change with a tiered store detaches");
    plain.resize(3, 12);
    assert_eq!(
        offloaded.storage.fill_debt,
        FillDebt::default(),
        "the window filled its screen from the lines it held back"
    );
    assert_eq!(screen_rows(&offloaded), screen_rows(&plain), "the screen");
    for g in [&mut offloaded, &mut plain] {
        g.cursor_up(2);
        g.carriage_return();
        g.erase_to_end_of_screen();
        "> ".chars().for_each(|c| g.write_char_wrap(c));
    }
    offloaded.reattach_reflowed_scrollback(job.reflow());
    assert_eq!(screen_rows(&offloaded), screen_rows(&plain), "the screen");
    assert_eq!(history_seq(&offloaded), history_seq(&plain), "the history");
    offloaded.assert_invariants();
}

#[test]
fn window_debt_follows_what_the_program_does() {
    type Step = fn(&mut Grid);
    fn print_two(g: &mut Grid) {
        logical_line(g, "one");
        logical_line(g, "two");
    }
    fn prompt(g: &mut Grid) {
        g.carriage_return();
        g.erase_to_end_of_screen();
        "$ ".chars().for_each(|c| g.write_char_wrap(c));
    }
    fn home_and_erase(g: &mut Grid) {
        g.set_cursor(0, 0);
        g.erase_to_end_of_screen();
        logical_line(g, "new screen");
    }
    fn shrink_and_grow(g: &mut Grid) {
        g.resize(4, 30);
        g.resize(12, 30);
    }
    let cases: [(&str, Option<usize>, Step); 4] = [
        ("print", Some(5), print_two),
        ("prompt", None, prompt),
        ("home and erase", None, home_and_erase),
        ("shrink and grow", Some(0), shrink_and_grow),
    ];
    for (name, redrawn, step) in cases {
        let mut offloaded = tiered_grid(12, 12, 1);
        let mut plain = tiered_grid(12, 12, 1);
        for g in [&mut offloaded, &mut plain] {
            feed_mixed_history(g, 7, 120, 12);
            if let Some(lines) = redrawn {
                g.erase_screen();
                g.set_cursor(0, 0);
                feed_mixed_history(g, 9, lines, 12);
            }
        }
        let job = offloaded
            .resize_offloading_scrollback(12, 30)
            .expect("a width change with a tiered store detaches");
        plain.resize(12, 30);
        assert!(
            redrawn == Some(0) || offloaded.storage.fill_debt.rows > 0,
            "{name}: precondition, the widening owes rows"
        );
        step(&mut offloaded);
        step(&mut plain);
        offloaded.reattach_reflowed_scrollback(job.reflow());
        assert_eq!(
            screen_rows(&offloaded),
            screen_rows(&plain),
            "{name}: the screen"
        );
        assert_eq!(
            (offloaded.cursor_row(), offloaded.cursor_col()),
            (plain.cursor_row(), plain.cursor_col()),
            "{name}: the cursor"
        );
        offloaded.assert_invariants();
    }
}

fn reference_materialize_all(job: PendingScrollbackReflow) -> ScrollbackStorage {
    let mut store = job.store;
    let mut input: Vec<Line> = Vec::with_capacity(store.line_count() + job.lazy_lines.len());
    for i in 0..store.line_count() {
        input.push(match store.get_line(i) {
            Ok(Some(line)) => line.into_owned(),
            Ok(None) | Err(_) => Line::new(),
        });
    }
    input.extend(job.lazy_lines);
    let mut staged = job.lazy_staged;
    input.extend(staged.drain_all());
    if job.ring_lines.first().is_some_and(Line::is_wrapped) {
        let head = input.iter().rposition(|line| !line.is_wrapped());
        input.truncate(head.unwrap_or(0));
    }
    let out = super::super::scrollback_reflow::reflow_scrollback_lines(&input, job.new_cols);
    store.clear().expect("reference clear");
    for line in out {
        store.push_line(line).expect("reference push");
    }
    store
}

fn parity_grid(seed: u64, limit: Option<usize>, budget_bytes: usize) -> Grid {
    let (rows, cols) = (10u16, 40u16);
    let mut sb: ScrollbackStorage = Scrollback::new(64, 512, budget_bytes).into();
    sb.set_line_limit(limit);
    let mut g = Grid::with_tiered_scrollback(rows, cols, 8, sb);
    feed_mixed_history(&mut g, seed, 700, cols);
    g.set_compress_offload_active(true);
    feed_mixed_history(&mut g, seed ^ 0xABCD, 40, cols);
    g
}

#[test]
fn streaming_matches_materialize_all_reference() {
    for (limit, budget_bytes) in [(None, 8_000_000usize), (Some(120), 8_000_000)] {
        for seed in [5u64, 21] {
            let (rows, new_cols) = (10u16, 23u16);
            let mut reference_grid = parity_grid(seed, limit, budget_bytes);
            let mut streamed_grid = parity_grid(seed, limit, budget_bytes);
            let mut chunked_grid = parity_grid(seed, limit, budget_bytes);

            let reference_store = reference_materialize_all(
                reference_grid
                    .resize_offloading_scrollback(rows, new_cols)
                    .expect("reference job"),
            );
            let one_shot = streamed_grid
                .resize_offloading_scrollback(rows, new_cols)
                .expect("streaming job")
                .reflow();
            let chunked_job = chunked_grid
                .resize_offloading_scrollback(rows, new_cols)
                .expect("chunked job");
            let mut rng = Rng(seed ^ 0x57EA);
            let (chunked, steps) = step_with_schedule(chunked_job, &mut rng, 41);

            assert!(steps > 4, "reach: the schedule actually chunked");
            assert_eq!(
                store_fingerprint(&one_shot.store),
                store_fingerprint(&reference_store),
                "limit {limit:?} budget {budget_bytes} seed {seed}: streaming \
                 one-shot != materialize-all reference"
            );
            assert_eq!(
                store_fingerprint(&chunked.store),
                store_fingerprint(&reference_store),
                "limit {limit:?} budget {budget_bytes} seed {seed}: streaming \
                 chunked != materialize-all reference"
            );
            assert_eq!(one_shot.store.line_limit(), limit);
            assert_eq!(one_shot.store.memory_budget(), budget_bytes);
        }
    }
}

#[test]
fn streaming_under_byte_budget_pressure_keeps_a_longer_suffix() {
    const BUDGET_BYTES: usize = 10_000;
    let (rows, new_cols) = (10u16, 23u16);
    let mut deeper_anywhere = false;

    for seed in [5u64, 21] {
        let mut reference_grid = parity_grid(seed, None, BUDGET_BYTES);
        let mut streamed_grid = parity_grid(seed, None, BUDGET_BYTES);
        let mut chunked_grid = parity_grid(seed, None, BUDGET_BYTES);

        let reference_store = reference_materialize_all(
            reference_grid
                .resize_offloading_scrollback(rows, new_cols)
                .expect("reference job"),
        );
        let one_shot = streamed_grid
            .resize_offloading_scrollback(rows, new_cols)
            .expect("streaming job")
            .reflow();
        let chunked_job = chunked_grid
            .resize_offloading_scrollback(rows, new_cols)
            .expect("chunked job");
        let mut rng = Rng(seed ^ 0x57EA);
        let (chunked, steps) = step_with_schedule(chunked_job, &mut rng, 41);
        assert!(steps > 4, "reach: the schedule actually chunked");

        let reference = store_fingerprint(&reference_store);
        assert!(
            reference_store.total_memory_used() <= BUDGET_BYTES,
            "seed {seed}: the reference must end under the byte budget"
        );
        assert!(
            one_shot.store.total_memory_used() <= BUDGET_BYTES,
            "seed {seed}: the streamed store must end under the byte budget"
        );

        for (name, store) in [("one-shot", &one_shot.store), ("chunked", &chunked.store)] {
            let streamed = store_fingerprint(store);
            assert!(
                streamed.len() >= reference.len(),
                "seed {seed} {name}: end-of-flight enforcement must never retain \
                 FEWER lines than push-time enforcement ({} < {})",
                streamed.len(),
                reference.len()
            );
            assert_eq!(
                streamed[streamed.len() - reference.len()..],
                reference[..],
                "seed {seed} {name}: the historical shape's retained history must be \
                 the byte-identical NEWEST SUFFIX of the streamed history — only the \
                 eviction depth may differ"
            );
            deeper_anywhere |= streamed.len() > reference.len();
        }

        assert_eq!(one_shot.store.line_limit(), None);
        assert_eq!(one_shot.store.memory_budget(), BUDGET_BYTES);
    }

    assert!(
        deeper_anywhere,
        "reach: the byte-budget corpus must actually evict to DIFFERENT depths — \
         that divergence is the boundary this test exists to pin"
    );
}

#[test]
fn streaming_step_carry_stays_bounded() {
    let (rows, cols) = (10u16, 30u16);
    let mut g = tiered_grid(rows, cols, 8);
    let mut rng = Rng(9);
    for i in 0..1200 {
        let len = (rng.below(4) as usize) * cols as usize + rng.below(20) as usize;
        let mut s = format!("B{i}-");
        while s.len() < len {
            s.push('y');
        }
        logical_line(&mut g, &s);
    }
    let depth = g.scrollback().map_or(0, |s| s.line_count());
    assert!(depth > 800, "precondition: deep tiered input ({depth})");

    let budget = 64usize;
    let mut job = g.resize_offloading_scrollback(rows, 17).expect("job");
    let mut steps = 0usize;
    loop {
        steps += 1;
        assert!(steps < 100_000, "stepping must terminate");
        match job.reflow_step(budget) {
            ReflowStep::InProgress(next) => {
                job = next;
                let carry_len = match &job.phase {
                    ReflowPhase::Stream { carry, .. } | ReflowPhase::RingRewrap { carry, .. } => {
                        carry.len()
                    }
                };
                assert!(
                    carry_len <= budget + 8,
                    "carry must stay O(budget + one run), got {carry_len} at step {steps}"
                );
            }
            ReflowStep::Done(done) => {
                g.reattach_reflowed_scrollback(done);
                break;
            }
        }
    }
    assert!(steps > 10, "reach: the budget actually chunked the input");
    assert!(g.scrollback_lines() > 400, "history preserved");
    g.assert_invariants();
}

#[test]
fn converging_reattach_rewraps_to_the_settled_width() {
    let (rows, w0, w1, w2) = (10u16, 80u16, 40u16, 60u16);
    let mut g = tiered_grid(rows, w0, 8);
    for i in 0..300 {
        let mut s = format!("C{i}-");
        while s.len() + 1 < w0 as usize {
            s.push('z');
        }
        logical_line(&mut g, &s);
    }
    let job = g.resize_offloading_scrollback(rows, w1).expect("job");
    assert!(
        g.resize_offloading_scrollback(rows, w2).is_none(),
        "an in-flight reflow self-throttles a superseding resize"
    );

    let follow = g
        .reattach_reflowed_scrollback_or_redetach(job.reflow())
        .expect("width mismatch must re-detach for convergence");
    assert_eq!(
        follow.new_cols(),
        w2,
        "the follow-up job targets the settled width"
    );
    assert!(g.reflow_offload_in_flight(), "convergence window open");
    assert!(
        g.reattach_reflowed_scrollback_or_redetach(follow.reflow())
            .is_none(),
        "widths agree after exactly one extra pass"
    );
    assert!(!g.reflow_offload_in_flight());

    let store = g.scrollback().expect("store re-attached");
    let max_len = (0..store.line_count())
        .filter_map(|i| {
            store
                .get_line(i)
                .ok()
                .flatten()
                .map(|line| line.to_string().chars().count())
        })
        .max()
        .unwrap_or(0);
    assert!(
        max_len <= usize::from(w2),
        "settled store must be wrapped at {w2} cols (max stored width {max_len})"
    );
    g.assert_invariants();
}

#[test]
fn converging_redetach_stages_ring_and_window_output_without_ui_store_push() {
    let (rows, w0, w1, w2) = (10u16, 80u16, 40u16, 60u16);
    let mut g = tiered_grid(rows, w0, 64);
    for i in 0..300 {
        logical_line(&mut g, &format!("H{i}-content"));
    }
    let job = g.resize_offloading_scrollback(rows, w1).expect("job");
    let ring_carried = job.ring_lines.len();
    assert!(ring_carried > 0, "precondition: ring history rides job");

    for i in 0..40 {
        logical_line(&mut g, &format!("W{i}-window"));
    }
    let staged_before = g.storage.lazy_buffer.len();
    assert!(
        staged_before > 0,
        "precondition: lazy_buffer has window output"
    );
    g.resize(rows, w2);
    let staged_at_redetach = g.storage.lazy_buffer.len();

    let reflowed = job.reflow();
    let store_lines_before_redetach = reflowed.store.line_count();
    let ring_out_len = reflowed.ring_out.len();
    let follow = g
        .reattach_reflowed_scrollback_or_redetach(reflowed)
        .expect("width mismatch must redetach");

    assert_eq!(follow.store.line_count(), store_lines_before_redetach);
    assert_eq!(
        follow.lazy_lines.len() + follow.lazy_staged.len(),
        ring_out_len + staged_at_redetach
    );
    assert!(
        g.reattach_reflowed_scrollback_or_redetach(follow.reflow())
            .is_none()
    );
    g.assert_invariants();
}

#[test]
fn a_job_rewraps_a_line_straddling_the_store_ring_seam_whole() {
    for policy in [ResizePolicy::ConPty, ResizePolicy::Native] {
        let mut g = tiered_grid(3, 10, 4);
        let mut plain = tiered_grid(3, 10, 4);
        for g in [&mut g, &mut plain] {
            logical_line(g, "0123456789ABCDE");
            for i in 0..5 {
                logical_line(g, &format!("s{i}"));
            }
        }
        assert_eq!(g.storage.ring_buffer_scrollback(), 4, "precondition");
        assert_eq!(g.scrollback_lines(), 5, "precondition: the head left");

        let pending = g
            .resize_offloading_scrollback_with_policy(3, 20, policy)
            .expect("a width change with a store offloads");
        g.reattach_reflowed_scrollback(pending.reflow());
        plain.resize_with_policy(3, 20, policy);
        let history = |g: &Grid| -> Vec<String> {
            (0..g.scrollback_lines())
                .filter_map(|i| g.get_history_line(i).map(|l| l.to_string()))
                .map(|s| s.trim_end().to_string())
                .collect()
        };
        assert_eq!(
            history(&g).first().map(String::as_str),
            Some("0123456789ABCDE"),
            "{policy:?}: the straddling line, whole"
        );
        assert_eq!(history(&g), history(&plain), "{policy:?}");
        g.assert_invariants();
    }
}

/// A line limit or byte budget changed while a resize rewrap has the store out (Settings or a
/// config reload; the wasm hosts' budget share moving as a pane opens or closes) applies at
/// re-attach without reordering history. The job's ring history and the
/// window's staged output return as one run in write order, then the settings replay. Replayed
/// first, each setter settled the staged output into the store ahead of the job's older ring
/// history, so history read [store | window output | old ring history | live ring]. Both entry
/// points, a converging re-detach, a store keeping lines and one whose share is zero.
#[test]
fn a_setting_changed_inside_the_offload_window_keeps_history_in_order() {
    let (rows, cols, ring) = (3u16, 40u16, 64usize);
    let number = |line: &str| -> usize {
        line.trim_end()
            .strip_prefix('L')
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("an unexpected line {line:?}"))
    };
    for start_limit in [Some(500), Some(ring)] {
        for (limit, budget) in [
            (Some(Some(1000)), false),
            (None, true),
            (Some(Some(1000)), true),
            (Some(Some(100)), false),
            (Some(None), false),
        ] {
            for converge in [false, true] {
                let ctx = format!(
                    "start {start_limit:?}, limit {limit:?}, budget {budget}, converge {converge}"
                );
                let mut g = tiered_grid(rows, cols, ring);
                g.set_scrollback_line_limit(start_limit);
                let mut written = 0usize;
                let mut write = |g: &mut Grid, n: usize| {
                    for _ in 0..n {
                        logical_line(g, &format!("L{written}"));
                        written += 1;
                    }
                };
                write(&mut g, 300);
                let job = g
                    .resize_offloading_scrollback(rows, cols - 20)
                    .expect("a tiered width change offloads its history");
                assert!(
                    !job.ring_lines.is_empty(),
                    "{ctx}: precondition: ring history rides the job"
                );
                write(&mut g, 3 * ring);
                assert!(
                    !g.storage.lazy_buffer.is_empty() || start_limit == Some(ring),
                    "{ctx}: precondition: the window staged its output"
                );
                if let Some(limit) = limit {
                    g.set_scrollback_line_limit(limit);
                }
                if budget {
                    g.set_scrollback_memory_budget(7_000_000)
                        .expect("a deferred budget reports success");
                }
                if converge {
                    g.resize(rows, cols - 15);
                }
                let mut next = Some(job.reflow());
                let mut rounds = 0;
                while let Some(done) = next.take() {
                    rounds += 1;
                    assert!(rounds < 8, "{ctx}: the re-attach converges");
                    next = g
                        .reattach_reflowed_scrollback_or_redetach(done)
                        .map(PendingScrollbackReflow::reflow);
                }
                g.drain_lazy_buffer();

                let numbers: Vec<usize> = logical_lines(&g).iter().map(|l| number(l)).collect();
                let first = *numbers.first().expect("history and screen hold lines");
                let expected: Vec<usize> = (first..written).collect();
                assert_eq!(numbers, expected, "{ctx}: whole and in write order");
                let keeps_all = start_limit != Some(ring) && limit != Some(Some(100));
                if keeps_all {
                    assert_eq!(first, 0, "{ctx}: nothing within the limit is lost");
                }
                g.assert_invariants();
            }
        }
    }
}
