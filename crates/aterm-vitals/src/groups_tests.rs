// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ring::GapKind;

fn frame(seq: u64) -> Frame {
    Frame {
        seq,
        taken: Taken::at(1_000_500 + i64::try_from(seq).unwrap() * 1_000, 1_000),
        mono_ms: seq * 1_000,
        at_ns: seq * 1_000_000_000,
        processes_observed: true,
        coalitions_observed: true,
        processes_complete: true,
        coalitions_complete: true,
    }
}

fn app(n: u32) -> GroupKey {
    GroupKey::Tab {
        pane: 7,
        root: ProcessKey {
            pid: 100 + n,
            start: 10 + u64::from(n),
        },
    }
}

fn rate(value: f64, end_ns: u64, window_ns: u64) -> Rate {
    Rate {
        value: Value::Read(value),
        window: ReadWindow::Reported { end_ns, window_ns },
    }
}

fn observation(key: GroupKey, cpu: f64) -> Observation<'static> {
    let scope = match key {
        GroupKey::Tab { pane, .. } => Scope::Tab(pane),
        GroupKey::Aterm(_) => Scope::Aterm,
        GroupKey::Shared(_) => Scope::Shared,
        GroupKey::Coalition(_) => Scope::Unknown,
    };
    Observation {
        cpu: rate(cpu, 20_000_000_000, 1_000_000_000),
        footprint: Value::Read(1_234),
        read_bps: rate(0.25, 20_000_000_000, 1_000_000_000),
        membership_complete: true,
        ..Observation::new(key, scope, "root", 1)
    }
}

fn keys(book: &GroupBook) -> Vec<GroupKey> {
    book.view().rows().iter().map(Row::key).collect()
}

#[test]
fn group_book_never_holds_more_than_its_bound() {
    let name = "a😀".repeat(1_000);
    let mut book = GroupBook::new();
    let input = (0..500)
        .map(|n| observation(app(n), f64::from(n)))
        .chain((0_u32..500).map(|n| observation(GroupKey::Coalition(u64::from(n)), f64::from(n))))
        .map(|mut o| {
            o.name = &name;
            o.energy = Energy {
                thread_mw: rate(1.0, 100_000, 90_000),
                cpu_mw: rate(2.0, 100_000, 90_000),
                gpu_mw: rate(3.0, 100_000, 90_000),
                ane_mw: rate(4.0, 100_000, 90_000),
            };
            o.write_bps = rate(5.0, 200_000, 100_000);
            o.starts_ps = rate(6.0, 300_000, 200_000);
            o
        });
    book.absorb(frame(1), input);
    let view = book.view();
    assert_eq!(view.rows().len(), GROUPS);
    assert_eq!(
        view.rows().iter().filter(|r| r.key().in_aterm()).count(),
        APP_GROUPS
    );
    let mut outside: Vec<_> = view
        .rows()
        .iter()
        .filter_map(|r| match r.key() {
            GroupKey::Coalition(id) => Some(id),
            _ => None,
        })
        .collect();
    outside.sort_unstable();
    assert_eq!(
        outside,
        (468..500).collect::<Vec<u64>>(),
        "all 500 outside observations reached selection"
    );
    assert_eq!(view.omitted().app_members, 372);
    assert_eq!(view.omitted().outside_groups, 468);
    assert!(
        view.rows()
            .iter()
            .all(|r| r.name().len() <= LABEL_BYTES && r.name_truncated())
    );
    assert!(view.rows().iter().all(|r| r.name().ends_with('…')));
    assert!(
        book.retained_bytes() <= BOOK_BYTES,
        "{} retained bytes",
        book.retained_bytes()
    );
    assert!(
        view.retained_bytes() <= BOOK_BYTES,
        "{} view bytes",
        view.retained_bytes()
    );
    assert!(
        GroupBook::frame_scratch_bytes() <= 96 * 1024,
        "facts-only staging has a separate honest bound"
    );
    assert_eq!(
        book.rows.capacity(),
        GROUPS,
        "the capacity, not only the length, is bounded"
    );

    // Churn cannot create a hidden history/member/label reservoir behind the visible quota.
    for seq in 2..20 {
        book.absorb(
            frame(seq),
            (0..1_000).map(|n| observation(GroupKey::Coalition(seq * 10_000 + n), n as f64)),
        );
        assert!(book.view().rows().len() <= GROUPS);
        assert!(book.retained_bytes() <= BOOK_BYTES);
    }
}

#[test]
fn group_book_rows_reorder_only_when_a_new_leader_leads_by_a_margin() {
    let check = |incumbent, challenger, changes| {
        let mut book = GroupBook::new();
        book.absorb(
            frame(1),
            [
                observation(app(1), incumbent),
                observation(app(2), incumbent),
            ],
        );
        assert_eq!(
            keys(&book),
            vec![app(1), app(2)],
            "equal points keep input order"
        );
        book.absorb(
            frame(2),
            [
                observation(app(1), incumbent),
                observation(app(2), challenger),
            ],
        );
        assert_eq!(
            keys(&book),
            if changes {
                vec![app(2), app(1)]
            } else {
                vec![app(1), app(2)]
            }
        );
    };
    check(10.0, 10.11, true); // absolute arm alone
    check(10.0, 10.1, true); // exact reported absolute boundary, without subtractive rounding
    check(0.1, 0.111, true); // relative arm alone
    check(0.3, 0.33, true); // exact reported relative boundary, below the absolute margin
    check(1.0, 1.05, false); // neither arm
    check(1.0, 1.0, false); // a threshold never overrules strict lead
    check(0.0, 0.0, false);
    check(0.0, 0.000_001, true);

    let mut book = GroupBook::new();
    book.absorb(
        frame(1),
        [
            observation(app(0), 5.0),
            observation(app(1), 0.1),
            observation(app(2), 0.1),
        ],
    );
    book.absorb(
        frame(2),
        [
            observation(app(0), 5.0),
            observation(app(1), 0.1),
            observation(app(2), 0.111),
        ],
    );
    assert_eq!(
        keys(&book),
        vec![app(0), app(2), app(1)],
        "the same rule governs interior ranks"
    );
    let before = keys(&book);
    book.absorb(
        frame(3),
        before.iter().map(|&key| {
            let mut o = observation(key, 0.0);
            o.cpu.value = Value::not_sampled(1);
            o
        }),
    );
    assert_eq!(keys(&book), before, "unread is not a zero used for ranking");
}

#[test]
fn group_book_folds_proved_member_points_without_losing_unknowns_or_source_windows() {
    let mut book = GroupBook::new();
    let mut a = observation(app(1), 1.25);
    a.energy.thread_mw = rate(0.25, 12_000, 10_000);
    let mut unread = observation(app(1), 0.0);
    unread.name = "";
    unread.cpu.value = Value::Unread {
        why: Unread::Refused,
        unread: 1,
    };
    unread.footprint = Value::Unread {
        why: Unread::Refused,
        unread: 1,
    };
    unread.energy.thread_mw = Rate {
        value: Value::not_sampled(1),
        window: ReadWindow::Unread,
    };
    unread.membership_complete = false;
    let mut zero = observation(app(1), 0.0);
    zero.name = "";
    zero.footprint = Value::Read(0);
    zero.energy.thread_mw = rate(0.0, 12_000, 10_000);
    book.absorb(frame(1), [a, unread, zero]);
    let old = book.view();
    let row = &old.rows()[0];
    assert_eq!(row.count(), 3);
    assert_eq!(
        row.cpu().value,
        Value::Partial {
            value: 1.25,
            unread: 1
        }
    );
    assert_eq!(
        row.footprint(),
        Value::Partial {
            value: 1_234,
            unread: 1
        }
    );
    assert_eq!(
        row.energy().thread_mw.value,
        Value::Partial {
            value: 0.25,
            unread: 1
        }
    );
    assert_eq!(
        row.energy().thread_mw.window,
        ReadWindow::Reported {
            end_ns: 12_000,
            window_ns: 10_000
        }
    );
    assert_ne!(row.energy().thread_mw.window, row.cpu().window);
    assert!(!row.membership_complete());
    assert_eq!(row.gpu().why(), Some(Unread::Unsupported));
    let partial_history = old.history(row, &[]);
    assert!(
        partial_history
            .cells(Metric::Cpu)
            .iter()
            .all(|c| c.cell.mean.is_none())
    );
    assert!(
        partial_history
            .cells(Metric::Memory)
            .iter()
            .all(|c| c.cell.mean.is_none()),
        "partial charges cannot become unqualified historical boxes"
    );

    let mut next = frame(2);
    next.processes_observed = false;
    next.processes_complete = false;
    book.absorb(next, []);
    let missing = book.view();
    assert_eq!(missing.frame(), Some(next));
    assert!(missing.rows()[0].cpu().value.value().is_none());
    assert!(missing.rows()[0].footprint().value().is_none());
    assert!(
        !missing.rows()[0].ended(),
        "a missing census proves no ending"
    );
    assert!(missing.rows()[0].name_held());
    assert_eq!(
        row.cpu().value,
        Value::Partial {
            value: 1.25,
            unread: 1
        },
        "the old immutable view did not mutate"
    );
    assert_eq!(old.frame(), Some(frame(1)));

    let mut second = observation(app(1), 2.0);
    second.cpu.window = ReadWindow::Reported {
        end_ns: 9_000,
        window_ns: 8_000,
    };
    book.absorb(frame(3), [a, second]);
    assert_eq!(book.view().rows()[0].cpu().window, ReadWindow::Mixed);
    assert_eq!(book.view().rows()[0].cpu().value, Value::Read(3.25));
    assert!(
        book.view()
            .history(row, &[])
            .cells(Metric::Cpu)
            .iter()
            .all(|c| c.cell.mean.is_none()),
        "an older Row cannot be projected under a newer View's frame"
    );
}

#[test]
fn group_book_only_whole_observations_end_groups_and_retention_obeys_the_hard_bound() {
    let mut book = GroupBook::new();
    book.absorb(
        frame(1),
        [
            observation(app(1), 1.0),
            observation(GroupKey::Coalition(3), 2.0),
        ],
    );
    let mut partial = frame(2);
    partial.processes_complete = false;
    partial.coalitions_complete = false;
    book.absorb(partial, []);
    assert!(book.view().rows().iter().all(|r| !r.ended()));
    book.absorb(frame(3), []);
    assert!(book.view().rows().iter().all(Row::ended));
    let mut before = frame(4);
    before.mono_ms = frame(3).mono_ms + ENDED_KEEP_MS - 1;
    book.absorb(before, []);
    assert_eq!(book.view().rows().len(), 2);
    let mut expiry = frame(5);
    expiry.mono_ms = frame(3).mono_ms + ENDED_KEEP_MS;
    book.absorb(expiry, []);
    assert!(book.view().rows().is_empty());

    let mut book = GroupBook::new();
    book.absorb(
        frame(1),
        (0..APP_GROUPS as u32).map(|n| observation(app(n), 1.0)),
    );
    book.absorb(
        frame(2),
        (1..APP_GROUPS as u32).map(|n| observation(app(n), 1.0)),
    );
    book.absorb(
        frame(3),
        (2..APP_GROUPS as u32).map(|n| observation(app(n), 1.0)),
    );
    let mut partial = frame(4);
    partial.processes_complete = false;
    book.absorb(partial, [observation(app(1_000), 1.0)]);
    assert!(
        !keys(&book).contains(&app(0)),
        "oldest confirmed ending goes first under quota pressure"
    );
    assert!(keys(&book).contains(&app(1)));
    assert!(keys(&book).contains(&app(1_000)));
    assert_eq!(book.view().rows().len(), APP_GROUPS);
}

#[test]
fn group_book_a_full_new_census_admits_current_groups_before_unseen_old_roots() {
    let mut book = GroupBook::new();
    book.absorb(
        frame(1),
        (0..APP_GROUPS as u32).map(|n| observation(app(n), 1.0)),
    );
    book.absorb(
        frame(2),
        (1_000..1_000 + APP_GROUPS as u32).map(|n| observation(app(n), 1.0)),
    );
    assert_eq!(book.view().rows().len(), APP_GROUPS);
    assert_eq!(book.view().omitted().app_members, 0);
    assert!(keys(&book).iter().all(|key| match key {
        GroupKey::Tab { root, .. } => root.pid >= 1_100,
        _ => false,
    }));
}

#[test]
fn group_book_admission_never_discards_a_retained_history_because_the_input_order_changed() {
    let mut book = GroupBook::new();
    book.absorb(
        frame(1),
        (0..APP_GROUPS as u32)
            .map(|n| observation(app(n), 1.0))
            .chain((0..OUTSIDE_GROUPS as u64).map(|n| observation(GroupKey::Coalition(n), 2.0))),
    );
    let incoming = [
        observation(app(9_000), 0.01),
        observation(GroupKey::Coalition(9_000), 0.01),
    ]
    .into_iter()
    .chain(
        (0..APP_GROUPS as u32)
            .rev()
            .map(|n| observation(app(n), 1.0)),
    )
    .chain(
        (0..OUTSIDE_GROUPS as u64)
            .rev()
            .map(|n| observation(GroupKey::Coalition(n), 2.0)),
    );
    book.absorb(frame(2), incoming);
    assert!(!keys(&book).contains(&app(9_000)));
    assert!(!keys(&book).contains(&GroupKey::Coalition(9_000)));
    assert_eq!(book.view().omitted().app_members, 1);
    assert_eq!(book.view().omitted().outside_groups, 1);
    for row in &book.rows {
        let history = row.0.history.as_deref().unwrap();
        let cell = history.cells[0][usize::from(book.axes[0].cursor)][0].cell();
        assert_eq!(
            cell.tau_ms,
            2_000.0,
            "both original readings survive for {:?}",
            row.key()
        );
    }
}

#[test]
fn group_book_labels_and_anchors_are_bounded_and_never_joined_by_pid_alone() {
    let mut book = GroupBook::new();
    assert_eq!(book.view().frame(), None);
    let key = app(0);
    let mut first = observation(key, 0.0);
    first.name = "original";
    book.absorb(frame(1), [first]);
    let mut empty = first;
    empty.name = "";
    book.absorb(frame(2), [empty]);
    assert_eq!(book.view().rows()[0].name(), "original");
    assert!(book.view().rows()[0].name_held());
    let mut reused = observation(
        GroupKey::Tab {
            pane: 7,
            root: ProcessKey {
                pid: 100,
                start: 999,
            },
        },
        0.0,
    );
    reused.name = "";
    book.absorb(frame(3), [reused]);
    let view = book.view();
    let row = view.rows().iter().find(|r| r.key() == reused.key).unwrap();
    assert_eq!(
        row.name(),
        "",
        "a new process generation inherits no old label"
    );
    assert!(!row.ended());
    let mut invalid = observation(app(1), 1.0);
    invalid.scope = Scope::Tab(8);
    book.absorb(frame(4), [invalid]);
    assert!(!keys(&book).contains(&app(1)));
    assert_eq!(book.view().omitted().invalid, 1);
}

#[test]
fn group_book_invalid_numbers_duplicate_accounts_and_known_zero_are_distinct() {
    for bad in [f64::NAN, f64::INFINITY, -1.0] {
        let mut book = GroupBook::new();
        book.absorb(frame(1), [observation(app(1), bad)]);
        assert_eq!(
            book.view().rows()[0].cpu().value.why(),
            Some(Unread::UnreadFormat)
        );
        assert!(
            book.view()
                .history(&book.view().rows()[0], &[])
                .cells(Metric::Cpu)[0]
                .cell
                .mean
                .is_none()
        );
    }
    let mut book = GroupBook::new();
    book.absorb(frame(1), [observation(app(1), 0.0)]);
    let view = book.view();
    assert_eq!(view.rows()[0].cpu().value, Value::Read(0.0));
    assert_eq!(
        view.history(&view.rows()[0], &[]).cells(Metric::Cpu)[0]
            .cell
            .mean,
        Some(0.0)
    );
    book.absorb(
        frame(2),
        [
            observation(GroupKey::Coalition(1), 2.0),
            observation(GroupKey::Coalition(1), 3.0),
        ],
    );
    let view = book.view();
    let row = view
        .rows()
        .iter()
        .find(|r| r.key() == GroupKey::Coalition(1))
        .unwrap();
    assert_eq!(
        row.cpu().value.why(),
        Some(Unread::UnreadFormat),
        "a whole account cannot be supplied twice and summed"
    );
    let mut a = observation(app(2), 1.0);
    a.footprint = Value::Read(u64::MAX);
    book.absorb(frame(3), [a, a]);
    let view = book.view();
    let row = view.rows().iter().find(|r| r.key() == app(2)).unwrap();
    assert_eq!(
        row.footprint().why(),
        Some(Unread::UnreadFormat),
        "an overflowing charge is not a saturated exact number"
    );
}

#[test]
fn group_book_history_uses_the_shared_weighted_bucket_and_backstep_rules() {
    let mut book = GroupBook::new();
    let mut oracle = crate::ring::BucketRing::without_long(BUCKETS);
    let inputs = [
        (500, 1_000, 1.0),
        (1_500, 1_000, 3.0),
        (7_500, 5_000, 5.0),
        (10_500, 1_000, 2.0),
        (20_500, 1_000, 7.0),
        (19_500, 1_000, 8.0),
        (-70_500, 1_000, 4.0),
        (-69_500, 1_000, 6.0),
        (90_500, 1_000, 9.0),
    ];
    for (seq, (at, period, cpu)) in inputs.into_iter().enumerate() {
        let mut f = frame(seq as u64 + 1);
        f.taken = Taken::at(at, period);
        book.absorb(f, [observation(app(1), cpu)]);
        // A wholly overlapped slot has no represented time. The main ring's minimum1ms
        // accumulator guard is not literal read coverage and is not invented in group history.
        oracle.push(f.taken, (at != 19_500).then_some(cpu), false);
        let entry = &book.rows[0].0;
        assert_eq!(book.axes[0].open, oracle.open_k());
        for (k, bucket) in oracle.iter() {
            let index = book.axes[0].index(k).unwrap();
            assert_eq!(
                entry.history.as_deref().unwrap().cells[0][index][0].cell(),
                bucket.cell(),
                "shared time weighting at {at}, bucket {k}"
            );
        }
    }
}

#[test]
fn group_book_history_has_sixteen_disjoint_boxes_and_no_future_or_gap_invention() {
    let mut book = GroupBook::new();
    for seq in 1..700 {
        book.absorb(frame(seq), [observation(app(1), 1.0)]);
    }
    let view = book.view();
    let row = &view.rows()[0];
    let history = view.history(row, &[]);
    let cells = history.cells(Metric::Cpu);
    assert_eq!(cells.len(), 16);
    assert_eq!(cells.iter().filter(|c| c.slot.is_live_now).count(), 1);
    for pair in cells.windows(2) {
        assert_eq!(pair[0].slot.age_hi_ms, pair[1].slot.age_lo_ms);
    }
    assert!(cells[1..6].iter().all(|c| c.cell.mean == Some(1.0)));
    assert!(cells[6..11].iter().all(|c| c.cell.mean == Some(1.0)));
    assert!(
        cells
            .iter()
            .any(|c| c.slot.band == 3 && c.cell.mean.is_none()),
        "unobserved old minutes stay hollow"
    );
    let now = view.frame().unwrap().taken.at_ms;
    let gap = TimeGap {
        start_ms: now - 11_000,
        end_ms: now - 1_000,
        kind: GapKind::Sleep,
    };
    let gapped = view.history(row, &[gap]);
    assert_eq!(gapped.cells(Metric::Cpu)[1].gap, Some(GapKind::Sleep));
    assert_eq!(gapped.cells(Metric::Cpu)[1].cell.mean, None);
    assert_eq!(
        gapped.cells(Metric::Cpu)[0].cell.mean,
        Some(1.0),
        "a current post-gap reading resumes only the live box"
    );

    let mut before = view.clone();
    before.frame.taken.at_ms -= 10_000_000;
    let projected = before.rows[0].0.history.as_deref().unwrap().project(
        0,
        before.axes,
        before.frame,
        None,
        &[],
    );
    assert!(
        projected.iter().all(|c| c.cell.mean.is_none()),
        "stored future buckets never become a forecast"
    );

    // One extra boundary bucket is retained at every resolution, rather than clipping the
    // oldest moving box as soon as now crosses an epoch edge.
    assert_eq!(BUCKETS, 7);
    let longest = HISTORY_SPEC.horizon_ms();
    assert!(longest < PERIODS[2] * (BUCKETS as i64 - 1));
}

#[test]
fn group_book_history_never_spreads_a_first_read_back_before_its_source_support() {
    let mut book = GroupBook::new();
    let mut first = frame(1);
    first.taken = Taken::at(9_500, 1_000);
    book.absorb(first, [observation(app(1), 2.0)]);
    let view = book.view();
    let history = view.history(&view.rows()[0], &[]);
    for metric in [Metric::Cpu, Metric::Memory] {
        assert!(
            history.cells(metric)[1..]
                .iter()
                .all(|c| c.cell.mean.is_none()),
            "[9000,10000) cannot populate the historical box [-1500,8500): {metric:?}"
        );
        assert_eq!(
            history.cells(metric)[0].cell.tau_ms,
            0.0,
            "a live point asserts no elapsed coverage"
        );
    }
    assert_eq!(history.cells(Metric::Cpu)[0].cell.mean, Some(2.0));

    let mut next = frame(2);
    next.taken = Taken::at(19_500, 1_000);
    let mut o = observation(app(1), 7.0);
    o.footprint = Value::Read(9_999);
    book.absorb(next, [o]);
    let view = book.view();
    let known = view.history(&view.rows()[0], &[]);
    assert_eq!(
        known.cells(Metric::Cpu)[1].cell.mean,
        Some(2.0),
        "the earlier read really is inside [8500,18500)"
    );
    assert_eq!(known.cells(Metric::Cpu)[1].cell.tau_ms, 1_000.0);
    assert_eq!(
        known.cells(Metric::Cpu)[1].quality,
        HistoryQuality::Reported
    );
    assert_eq!(known.cells(Metric::Memory)[1].cell.mean, Some(1_234.0));
}

#[test]
fn group_book_sparse_internal_gaps_and_changing_boundary_bins_never_claim_exact_window_values() {
    let mut sparse = SupportedAgg::default();
    sparse.note(1.0, 1_000, 0, 1_000, 0, 10_000, i64::MIN);
    sparse.note(9.0, 1_000, 9_000, 10_000, 0, 10_000, 1_000);
    assert!(
        sparse.part(0, 10_000, 2_000, 8_000).is_none(),
        "an internal hole is not represented time"
    );
    assert!(
        sparse.part(0, 10_000, -1_500, 8_500).is_none(),
        "the two samples cannot be separated at this boundary"
    );
    let (whole, quality) = sparse.part(0, 10_000, 0, 10_000).unwrap();
    assert_eq!(whole.mean, Some(5.0));
    assert_eq!(
        whole.tau_ms, 2_000.0,
        "only the represented source slots weigh, not the intervening gap"
    );
    assert_eq!(quality, HistoryQuality::Reported);
    let before = sparse;
    sparse.note(99.0, 0, 10_000, 10_000, 0, 10_000, 10_000);
    assert_eq!(sparse, before, "zero overlap adds no artificial1ms");

    let mut changing = SupportedAgg::default();
    let mut constant = SupportedAgg::default();
    for second in 0..10 {
        let from = second * 1_000;
        let before = if second == 0 { i64::MIN } else { from };
        changing.note(
            (second + 1) as f64,
            1_000,
            from,
            from + 1_000,
            0,
            10_000,
            before,
        );
        constant.note(2.0, 1_000, from, from + 1_000, 0, 10_000, before);
    }
    let (partial, quality) = changing.part(0, 10_000, 2_000, 5_000).unwrap();
    assert_eq!(partial.mean, Some(5.5));
    assert_ne!(
        partial.mean,
        Some(4.0),
        "the actual three inner values are3,4,5; this is only the source-bin mean"
    );
    assert_eq!(quality, HistoryQuality::Binned);
    assert_eq!(quality.word(), "binned_estimate");
    assert_eq!(partial.tau_ms, 3_000.0);
    let (partial, quality) = constant.part(0, 10_000, 2_000, 5_000).unwrap();
    assert_eq!(partial.mean, Some(2.0));
    assert_eq!(
        quality,
        HistoryQuality::Reported,
        "identical contiguous source values permit a supported clip"
    );
    assert_eq!(partial.tau_ms, 3_000.0);

    let mut coarse = SupportedAgg::default();
    coarse.note(2.0, 1_000, 12_345, 13_345, 0, 600_000, i64::MIN);
    assert!(
        coarse.part(0, 600_000, 0, 12_340).is_none(),
        "quantized outer bounds cannot create early support"
    );
    assert!(
        coarse.part(0, 600_000, 13_350, 20_000).is_none(),
        "quantized outer bounds cannot extend late support"
    );
    assert!(
        std::mem::size_of::<SupportedAgg>() <= 24,
        "support metadata replaces unused over-time, not a new reservoir"
    );
}
