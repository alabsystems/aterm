// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{App, spawn};
use spawn::{OUTPUT_CADENCE_NS as CADENCE, WAKE_LATCH_DIRTY, WAKE_LATCH_STREAMING};
use std::time::Duration;

fn app() -> App {
    let mut app = App::headless_for_test();
    for id in 1..=200 {
        app.pool.insert(crate::stub_session(id));
    }
    app
}

fn hold(app: &mut App, id: u64, at: u64) {
    app.pool
        .get(id)
        .unwrap()
        .output_wake_pending
        .store(WAKE_LATCH_STREAMING | at, Ordering::Relaxed);
    app.pool.output_pass(id, at);
}

fn dirty(app: &App, id: u64, at: u64) {
    let latch = &app.pool.get(id).unwrap().output_wake_pending;
    assert_eq!(
        spawn::gated_output_wake(
            latch,
            at,
            0,
            true,
            || panic!("held tail must not post"),
            || at
        ),
        spawn::WakeGate::Held
    );
}

fn park(app: &mut App, at: u64) -> Option<u64> {
    app.release_output_holds(app.lat_epoch + Duration::from_nanos(at))
        .map(|deadline| deadline.duration_since(app.lat_epoch).as_nanos() as u64)
}

#[test]
fn quiet_pool_has_zero_cadence_latch_visits_at_repeated_parks() {
    let mut app = app();
    for turn in 0..100 {
        assert_eq!(park(&mut app, turn * CADENCE), None);
    }
    assert_eq!(app.pool.output_holds.visits, 0);
    assert_eq!(app.pool.output_holds.passes, 0);
}

#[test]
fn a_cadence_read_observes_its_selected_latch_without_servicing_or_scanning() {
    let mut app = app();
    let visits = crate::wake_census::pool_visits();
    let empty = app.pool.output_cadence_snapshot(None, CADENCE);
    assert_eq!(empty.sessions, 201);
    assert_eq!((empty.ordered, empty.indexed), (0, 0));
    assert!(empty.selected.is_none());

    hold(&mut app, 197, CADENCE);
    dirty(&app, 197, CADENCE + 1);
    let passes = app.pool.output_holds.passes;
    for at in [CADENCE + 1, 2 * CADENCE, 3 * CADENCE] {
        let read = app.pool.output_cadence_snapshot(Some(197), at);
        assert_eq!(read.now_ns, at);
        assert_eq!((read.ordered, read.indexed), (1, 1));
        let selected = read.selected.unwrap();
        assert_eq!(selected.session, 197);
        assert!(selected.pooled && selected.enrolled && selected.current);
        assert!(selected.held && selected.dirty);
        assert_eq!(selected.due_ns, 2 * CADENCE);
    }
    let quiet = app
        .pool
        .output_cadence_snapshot(Some(198), 3 * CADENCE + 1)
        .selected
        .unwrap();
    assert!(quiet.pooled);
    assert!(!quiet.enrolled && !quiet.current && !quiet.held && !quiet.dirty);
    assert_eq!(quiet.due_ns, 0);
    assert_eq!(app.pool.output_holds.passes, passes);
    assert_eq!(app.pool.output_holds.visits, 0);
    assert_eq!(crate::wake_census::pool_visits(), visits);

    // A real reader clear leaves an indexed candidate until UI service sees it. A
    // diagnostic must preserve, and distinguish, that state rather than repair it.
    spawn::PostCadence::default().foreground_edge(&app.pool.get(197).unwrap().output_wake_pending);
    let cleared = app
        .pool
        .output_cadence_snapshot(Some(197), 3 * CADENCE + 2)
        .selected
        .unwrap();
    assert!(cleared.enrolled && cleared.current);
    assert!(!cleared.held && !cleared.dirty);
    assert_eq!(app.pool.output_holds.ordered.len(), 1);
    assert_eq!(park(&mut app, 3 * CADENCE + 2), None);
    let retired = app.pool.output_cadence_snapshot(Some(197), 4 * CADENCE);
    assert_eq!((retired.ordered, retired.indexed), (0, 0));
    assert!(!retired.selected.unwrap().enrolled);
    let absent = app
        .pool
        .output_cadence_snapshot(Some(404), 4 * CADENCE)
        .selected
        .unwrap();
    assert!(!absent.pooled && !absent.enrolled && !absent.current && !absent.held);
}

#[test]
fn repeated_actual_rearms_replace_the_owned_record_without_stale_heap_growth() {
    let mut app = app();
    for turn in 1..=1000 {
        let at = turn * CADENCE;
        hold(&mut app, 1, at);
        assert_eq!(app.pool.output_holds.next(), Some(at + CADENCE));
        assert_eq!(app.pool.output_holds.ordered.len(), 1);
        assert_eq!(app.pool.output_holds.deadlines.len(), 1);
    }
    assert_eq!(park(&mut app, 1001 * CADENCE), None);
    assert_eq!(app.pool.output_holds.visits, 1);
    assert_eq!(app.pool.output_holds.ordered.len(), 0);
    assert_eq!(app.pool.output_holds.deadlines.len(), 0);
}

#[test]
fn one_held_tail_among_two_hundred_quiet_sessions_runs_real_output_then_retires_cleanly() {
    let mut app = app();
    let at = CADENCE;
    hold(&mut app, 197, at);
    dirty(&app, 197, at + 1);
    for (index, now) in [at, at + 1, 2 * CADENCE - 1].into_iter().enumerate() {
        assert_eq!(park(&mut app, now), Some(2 * CADENCE));
        assert_eq!(
            app.pool.output_holds.visits,
            index + 1,
            "only the relevant pending head is validated"
        );
    }
    assert_eq!(park(&mut app, 2 * CADENCE), Some(3 * CADENCE));
    assert_eq!(
        app.pool.output_holds.visits, 5,
        "three pending looks, one due dispatch, one rearmed pending head"
    );
    assert_eq!(
        app.pool.output_holds.passes, 2,
        "real on_output_wake rearmed the hold"
    );
    assert_eq!(
        app.pool
            .get(197)
            .unwrap()
            .output_wake_pending
            .load(Ordering::Relaxed)
            & WAKE_LATCH_DIRTY,
        0
    );
    assert_eq!(park(&mut app, 3 * CADENCE), None);
    assert_eq!(app.pool.output_holds.visits, 6);
    assert_eq!(
        app.pool.output_holds.passes, 2,
        "clean release must not synthesize output"
    );
    for now in 4..50 {
        assert_eq!(park(&mut app, now * CADENCE), None);
    }
    assert_eq!(app.pool.output_holds.visits, 6);
}

#[test]
fn due_frontier_orders_sessions_and_keeps_real_earliest_rearm() {
    let mut app = app();
    for (id, at) in [(3, 3 * CADENCE), (2, 2 * CADENCE), (1, CADENCE)] {
        hold(&mut app, id, at);
        dirty(&app, id, at + 1);
    }
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    crate::vitals_host::BEFORE_LATCH_CLEAR.with_borrow_mut(|slot| {
        let seen = seen.clone();
        *slot = Some(Box::new(move |_, id| seen.borrow_mut().push(id)));
    });
    assert_eq!(park(&mut app, 4 * CADENCE), Some(5 * CADENCE));
    crate::vitals_host::BEFORE_LATCH_CLEAR.with_borrow_mut(|slot| *slot = None);
    assert_eq!(*seen.borrow(), vec![1, 2, 3]);
    assert_eq!(
        app.pool.output_holds.visits, 4,
        "three due heads and the current pending head"
    );
    assert_eq!(app.pool.output_holds.ordered.len(), 3);
    assert_eq!(park(&mut app, 5 * CADENCE), None);
}

#[test]
fn foreground_clear_and_clear_during_due_dispatch_never_lose_the_next_real_post() {
    let mut app = app();
    hold(&mut app, 1, CADENCE);
    dirty(&app, 1, CADENCE + 1);
    spawn::PostCadence::default().foreground_edge(&app.pool.get(1).unwrap().output_wake_pending);
    assert_eq!(park(&mut app, 2 * CADENCE), None);
    assert_eq!(app.pool.output_holds.passes, 1);
    hold(&mut app, 1, 3 * CADENCE);
    dirty(&app, 1, 3 * CADENCE + 1);
    crate::vitals_host::BEFORE_LATCH_CLEAR.with_borrow_mut(|slot| {
        *slot = Some(Box::new(|app, id| {
            spawn::PostCadence::default()
                .foreground_edge(&app.pool.get(id).unwrap().output_wake_pending);
        }));
    });
    assert_eq!(park(&mut app, 4 * CADENCE), None);
    crate::vitals_host::BEFORE_LATCH_CLEAR.with_borrow_mut(|slot| *slot = None);
    let posts = std::cell::Cell::new(0);
    assert_eq!(
        spawn::gated_output_wake(
            &app.pool.get(1).unwrap().output_wake_pending,
            5 * CADENCE,
            0,
            false,
            || {
                posts.set(posts.get() + 1);
                true
            },
            || 5 * CADENCE
        ),
        spawn::WakeGate::Posted
    );
    assert_eq!(posts.get(), 1);
}

#[test]
fn foreground_clear_before_due_returns_only_a_live_later_deadline_or_none() {
    for later in [false, true] {
        let mut app = app();
        hold(&mut app, 1, CADENCE);
        if later {
            hold(&mut app, 2, CADENCE + CADENCE / 2);
        }
        spawn::PostCadence::default()
            .foreground_edge(&app.pool.get(1).unwrap().output_wake_pending);
        let passes = app.pool.output_holds.passes;
        // Nondecreasing source times: C, optional 1.5C, then park 1.75C.
        assert_eq!(
            park(&mut app, CADENCE + 3 * CADENCE / 4),
            later.then_some(2 * CADENCE + CADENCE / 2)
        );
        assert_eq!(
            app.pool.output_holds.passes, passes,
            "a clear creates no output"
        );
        assert_eq!(app.pool.output_holds.visits, 1 + usize::from(later));
        assert_eq!(app.pool.output_holds.ordered.len(), usize::from(later));
    }
}

#[test]
fn pre_due_stale_identity_and_changed_word_are_validated_before_arming() {
    let mut app = app();
    hold(&mut app, 1, CADENCE);
    let old = app.pool.get(1).unwrap().output_wake_pending.clone();
    app.pool.insert(crate::stub_session(1));
    app.pool
        .output_holds
        .remember(1, &old, WAKE_LATCH_HELD | (2 * CADENCE));
    assert_eq!(park(&mut app, CADENCE + CADENCE / 2), None);
    assert_eq!(
        app.pool.output_holds.visits, 0,
        "stale owner is refused before atomic access"
    );
    hold(&mut app, 1, 2 * CADENCE);
    hold(&mut app, 2, 2 * CADENCE + CADENCE / 2);
    app.pool
        .get(1)
        .unwrap()
        .output_wake_pending
        .store(WAKE_LATCH_HELD | (6 * CADENCE), Ordering::Relaxed);
    // The changed first head moves beyond the second live head, before either is due.
    assert_eq!(
        park(&mut app, 2 * CADENCE + 3 * CADENCE / 4),
        Some(3 * CADENCE + CADENCE / 2)
    );
    assert_eq!(app.pool.output_holds.visits, 2);
    assert_eq!(
        app.pool.output_holds.passes, 3,
        "deadline validation creates no output"
    );
}

#[test]
fn output_rearmed_while_servicing_keeps_one_current_entry() {
    let mut app = app();
    hold(&mut app, 1, CADENCE);
    dirty(&app, 1, CADENCE + 1);
    crate::vitals_host::BEFORE_LATCH_CLEAR.with_borrow_mut(|slot| {
        *slot = Some(Box::new(|app, id| dirty(app, id, 2 * CADENCE)));
    });
    assert_eq!(park(&mut app, 2 * CADENCE), Some(3 * CADENCE));
    crate::vitals_host::BEFORE_LATCH_CLEAR.with_borrow_mut(|slot| *slot = None);
    dirty(&app, 1, 2 * CADENCE + 1);
    assert_eq!(park(&mut app, 3 * CADENCE), Some(4 * CADENCE));
    assert_eq!(app.pool.output_holds.ordered.len(), 1);
    assert_eq!(app.pool.output_holds.deadlines.len(), 1);
    assert_eq!(app.pool.output_holds.passes, 3);
}

#[test]
fn last_close_and_identity_replacement_retire_exact_entries() {
    let mut app = app();
    hold(&mut app, 1, CADENCE);
    dirty(&app, 1, CADENCE + 1);
    app.pool.attach(1);
    assert!(!app.pool.detach(1));
    assert_eq!(app.pool.output_holds.next(), Some(2 * CADENCE));
    assert!(app.pool.detach(1));
    assert_eq!(app.pool.output_holds.next(), None);
    hold(&mut app, 2, CADENCE);
    let old = app.pool.get(2).unwrap().output_wake_pending.clone();
    app.pool.insert(crate::stub_session(2));
    old.fetch_or(WAKE_LATCH_DIRTY, Ordering::Relaxed);
    assert_eq!(park(&mut app, 2 * CADENCE), None);
    assert_eq!(app.pool.output_holds.visits, 0);
    assert_eq!(app.pool.output_holds.ordered.len(), 0);
}

#[test]
fn stale_identity_candidate_cannot_touch_replacement_and_changed_future_word_reindexes() {
    let mut app = app();
    hold(&mut app, 1, CADENCE);
    let old = app.pool.get(1).unwrap().output_wake_pending.clone();
    app.pool.insert(crate::stub_session(1));
    // Independent stale-owner negative fixture: bypass normal insert eviction.
    app.pool
        .output_holds
        .remember(1, &old, WAKE_LATCH_HELD | (2 * CADENCE));
    old.fetch_or(WAKE_LATCH_DIRTY, Ordering::Relaxed);
    assert_eq!(park(&mut app, 2 * CADENCE), None);
    assert_eq!(
        app.pool
            .get(1)
            .unwrap()
            .output_wake_pending
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(app.pool.output_holds.visits, 0);
    hold(&mut app, 1, 3 * CADENCE);
    let passes_before_future_poll = app.pool.output_holds.passes;
    let latch = &app.pool.get(1).unwrap().output_wake_pending;
    // A later observed held word must remain scheduled, never use the old due stamp.
    latch.store(
        WAKE_LATCH_HELD | WAKE_LATCH_DIRTY | (6 * CADENCE),
        Ordering::Relaxed,
    );
    assert_eq!(park(&mut app, 4 * CADENCE), Some(6 * CADENCE));
    assert_eq!(app.pool.output_holds.passes, passes_before_future_poll);
    assert_eq!(park(&mut app, 6 * CADENCE), Some(7 * CADENCE));
}

fn project(
    model: &aterm_spec::derive::Model,
    app: &App,
    original: &Arc<AtomicU64>,
    reader_fed: bool,
    passes_at_retirement: Option<usize>,
) -> aterm_spec::interp::State {
    let mut state = model.init_state();
    let live = app
        .pool
        .get(1)
        .is_some_and(|s| Arc::ptr_eq(&s.output_wake_pending, original));
    let word = if live {
        original.load(Ordering::Relaxed)
    } else {
        0
    };
    let passes = app.pool.output_holds.passes;
    let delivered = passes > 1;
    state.insert("held", i64::from(word & WAKE_LATCH_HELD != 0));
    state.insert(
        "indexed",
        i64::from(app.pool.output_holds.deadlines.contains_key(&1)),
    );
    state.insert("dirty", i64::from(word & WAKE_LATCH_DIRTY != 0));
    state.insert("used", i64::from(passes > 0));
    state.insert("retired", i64::from(!live));
    state.insert("delivered", i64::from(delivered));
    state.insert("phantom", i64::from(delivered && !reader_fed));
    state.insert(
        "stale",
        i64::from(passes_at_retirement.is_some_and(|before| passes > before)),
    );
    state
}

#[test]
fn actual_hold_reader_service_and_retirement_conform_with_missing_enrollment_negative() {
    let model = aterm_models_session::output_cadence_frontier_model();
    for scenario in 0..4 {
        let mut app = app();
        let original = app.pool.get(1).unwrap().output_wake_pending.clone();
        let mut reader_fed = false;
        let mut retired_at = None;
        let mut before = project(&model, &app, &original, reader_fed, retired_at);
        let step = |before: &mut aterm_spec::interp::State, app: &App, action, fed, retired| {
            let after = project(&model, app, &original, fed, retired);
            assert!(
                model.successors(action, before).contains(&after),
                "{action}: {before:?} -> {after:?}"
            );
            *before = after;
        };
        hold(&mut app, 1, CADENCE);
        step(&mut before, &app, "Hold", reader_fed, retired_at);
        if scenario != 0 {
            dirty(&app, 1, CADENCE + 1);
            reader_fed = true;
            step(&mut before, &app, "Dirty", reader_fed, retired_at);
        }
        if scenario == 2 {
            spawn::PostCadence::default().foreground_edge(&original);
            step(&mut before, &app, "Clear", reader_fed, retired_at);
        }
        if scenario == 3 {
            retired_at = Some(app.pool.output_holds.passes);
            app.pool.insert(crate::stub_session(1));
            step(&mut before, &app, "Retire", reader_fed, retired_at);
        }
        park(
            &mut app,
            if scenario >= 2 {
                CADENCE + CADENCE / 2
            } else {
                2 * CADENCE
            },
        );
        step(&mut before, &app, "Service", reader_fed, retired_at);
        park(&mut app, 3 * CADENCE);
        step(&mut before, &app, "Service", reader_fed, retired_at);
    }
    // Independent invalid implementation trace: the real latch was held, but its
    // enrollment was lost. The projection must reject this, not normalize it away.
    let mut app = app();
    let original = app.pool.get(1).unwrap().output_wake_pending.clone();
    let initial = project(&model, &app, &original, false, None);
    hold(&mut app, 1, CADENCE);
    app.pool.output_holds.forget(1);
    let omitted = project(&model, &app, &original, false, None);
    assert!(!model.successors("Hold", &initial).contains(&omitted));
    let mutant = aterm_spec::interp::with_buggy(&model, 1);
    assert!(mutant.successors("Hold", &initial).contains(&omitted));
}
