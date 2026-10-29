// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Behavioral tests for `once_cell` consumer contracts.

use once_cell::{race, sync, unsync};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

const RACE_WINDOW: std::time::Duration = std::time::Duration::from_millis(120);

#[test]
fn get_or_try_init_runs_the_closure_exactly_once_under_contention() {
    let cell: Arc<sync::OnceCell<u32>> = Arc::new(sync::OnceCell::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(2));

    let mut handles = Vec::new();
    for _ in 0..2 {
        let cell = Arc::clone(&cell);
        let runs = Arc::clone(&runs);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let mut i_ran = false;
            let v: &u32 = cell
                .get_or_try_init(|| {
                    i_ran = true;
                    runs.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(RACE_WINDOW);
                    Ok::<u32, ()>(7)
                })
                .expect("the initialiser cannot fail here");
            (i_ran, *v)
        }));
    }
    let results: Vec<(bool, u32)> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "{} threads ran the initialiser; a pool keyed on the cell would hand out two Arcs for \
         one key",
        runs.load(Ordering::SeqCst)
    );
    assert_eq!(
        results.iter().filter(|(ran, _)| *ran).count(),
        1,
        "exactly one thread must observe that it was the initialiser: {results:?}"
    );
    for (_, v) in &results {
        assert_eq!(*v, 7, "both threads must see the same value");
    }
}

#[test]
fn a_failed_initialiser_leaves_the_cell_empty_and_retryable() {
    let cell: sync::OnceCell<u32> = sync::OnceCell::new();
    let first: Result<&u32, &str> = cell.get_or_try_init(|| Err("no"));
    assert_eq!(first, Err("no"));
    assert_eq!(cell.get(), None, "a failed init must not fill the cell");
    let second: Result<&u32, &str> = cell.get_or_try_init(|| Ok(3));
    assert_eq!(second, Ok(&3), "a later call must be able to try again");
    assert_eq!(cell.get(), Some(&3));
    assert_eq!(cell.get_or_try_init(|| Ok::<u32, &str>(9)), Ok(&3));
}

#[test]
fn set_is_single_assignment_and_reports_the_loser() {
    let cell: sync::OnceCell<u32> = sync::OnceCell::new();
    assert_eq!(cell.set(1), Ok(()));
    assert_eq!(cell.set(2), Err(2), "the rejected value comes back");
    assert_eq!(cell.get(), Some(&1));
    assert_eq!(cell.try_insert(3), Err((&1, 3)));
    let fresh: sync::OnceCell<u32> = sync::OnceCell::new();
    assert_eq!(fresh.try_insert(5), Ok(&5));
}

#[test]
fn take_and_into_inner_empty_the_cell() {
    let mut cell = sync::OnceCell::from(4u32);
    assert_eq!(cell.get(), Some(&4));
    assert_eq!(cell.take(), Some(4));
    assert_eq!(cell.get(), None, "take leaves the cell reusable");
    assert_eq!(cell.set(6), Ok(()));
    assert_eq!(cell.into_inner(), Some(6));
    assert_eq!(sync::OnceCell::<u32>::new().into_inner(), None);
}

#[test]
fn debug_and_eq_match_the_upstream_shapes() {
    let empty: sync::OnceCell<u32> = sync::OnceCell::new();
    assert_eq!(format!("{empty:?}"), "OnceCell(Uninit)");
    let full = sync::OnceCell::from(9u32);
    assert_eq!(format!("{full:?}"), "OnceCell(9)");
    assert_eq!(full, sync::OnceCell::from(9u32));
    assert_ne!(full, empty);
    assert_eq!(full.clone().get(), Some(&9));

    let lazy: sync::Lazy<u32> = sync::Lazy::new(|| unreachable!("Debug must not force"));
    assert_eq!(
        format!("{lazy:?}"),
        "Lazy { cell: OnceCell(Uninit), init: \"..\" }"
    );

    let boxed: race::OnceBox<u32> = race::OnceBox::new();
    let null_once_box = format!("OnceBox({:?})", core::ptr::null::<u32>());
    assert_eq!(format!("{boxed:?}"), null_once_box);
    boxed.set(Box::new(1)).expect("empty");
    let full_once_box = format!("{boxed:?}");
    assert!(
        full_once_box.starts_with("OnceBox(")
            && full_once_box.ends_with(')')
            && full_once_box != null_once_box,
        "a full OnceBox prints its address, not null: {boxed:?}"
    );
}

#[test]
fn lazy_does_not_run_its_initialiser_until_forced() {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let lazy: sync::Lazy<u32> = sync::Lazy::new(|| {
        RUNS.fetch_add(1, Ordering::SeqCst);
        11
    });
    assert_eq!(
        RUNS.load(Ordering::SeqCst),
        0,
        "construction must not force"
    );
    assert_eq!(sync::Lazy::get(&lazy), None);
    assert_eq!(*lazy, 11, "Deref forces");
    assert_eq!(*lazy, 11);
    assert_eq!(sync::Lazy::force(&lazy), &11);
    assert_eq!(
        RUNS.load(Ordering::SeqCst),
        1,
        "three forcings, one initialiser run"
    );
    assert_eq!(sync::Lazy::get(&lazy), Some(&11));
}

#[test]
fn unsync_lazy_forces_once_and_derefs() {
    let runs = std::cell::Cell::new(0u32);
    let lazy = unsync::Lazy::new(|| {
        runs.set(runs.get() + 1);
        "x".to_string()
    });
    assert_eq!(runs.get(), 0);
    assert_eq!(&*lazy, "x");
    assert_eq!(unsync::Lazy::force(&lazy), "x");
    assert_eq!(runs.get(), 1);
}

struct WasmBindgenShapedCell<T, F = fn() -> T>(unsync::Lazy<T, F>);

impl<T, F> WasmBindgenShapedCell<T, F> {
    const fn new(init: F) -> Self {
        Self(unsync::Lazy::new(init))
    }
}

const _: () = {
    const fn rus<T: std::panic::RefUnwindSafe>() {}
    const fn us<T: std::panic::UnwindSafe>() {}
    const fn sync_bound<T: Sync>() {}

    let _ = WasmBindgenShapedCell::<u32>::new(|| 42);
    rus::<unsync::OnceCell<u32>>();
    rus::<unsync::Lazy<u32>>();
    us::<unsync::OnceCell<u32>>();
    us::<unsync::Lazy<u32>>();
    rus::<sync::OnceCell<u32>>();
    us::<sync::OnceCell<u32>>();
    rus::<sync::Lazy<u32>>();
    us::<sync::Lazy<u32>>();
    rus::<race::OnceBox<u32>>();
    us::<race::OnceBox<u32>>();
    sync_bound::<sync::Lazy<u32>>();
};

// `sync::Lazy::new` is const, so a `Lazy` can be a static.
static _SHARED: sync::Lazy<u32> = sync::Lazy::new(|| 5);

#[test]
fn lazy_forced_concurrently_runs_its_initialiser_once() {
    let runs = Arc::new(AtomicUsize::new(0));
    let runs2 = Arc::clone(&runs);
    let lazy: Arc<sync::Lazy<u32, Box<dyn Fn() -> u32 + Send>>> =
        Arc::new(sync::Lazy::new(Box::new(move || {
            runs2.fetch_add(1, Ordering::SeqCst);
            thread::sleep(RACE_WINDOW);
            13
        })));
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let lazy = Arc::clone(&lazy);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            **lazy
        }));
    }
    for h in handles {
        assert_eq!(h.join().unwrap(), 13);
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn once_box_is_a_single_assignment_cell_over_a_box() {
    let cell: race::OnceBox<Vec<u8>> = race::OnceBox::new();
    assert_eq!(cell.get(), None);
    assert_eq!(cell.set(Box::new(vec![1, 2])), Ok(()));
    assert_eq!(cell.set(Box::new(vec![9])), Err(Box::new(vec![9])));
    assert_eq!(cell.get(), Some(&vec![1, 2]));
    assert_eq!(cell.get_or_init(|| Box::new(vec![0])), &vec![1, 2]);

    let fresh: race::OnceBox<u32> = race::OnceBox::default();
    assert_eq!(fresh.get_or_init(|| Box::new(4)), &4);
    assert_eq!(fresh.get(), Some(&4));

    let failed: race::OnceBox<u32> = race::OnceBox::new();
    assert_eq!(failed.get_or_try_init(|| Err::<Box<u32>, u8>(1)), Err(1));
    assert_eq!(failed.get(), None, "a failed init leaves it empty");
}

#[test]
fn once_box_runs_its_initialiser_once_under_contention() {
    let cell: Arc<race::OnceBox<u32>> = Arc::new(race::OnceBox::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..3 {
        let cell = Arc::clone(&cell);
        let runs = Arc::clone(&runs);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            *cell.get_or_init(|| {
                runs.fetch_add(1, Ordering::SeqCst);
                thread::sleep(RACE_WINDOW);
                Box::new(21)
            })
        }));
    }
    for h in handles {
        assert_eq!(h.join().unwrap(), 21);
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[test]
fn unsync_once_cell_matches_its_sync_sibling() {
    let cell: unsync::OnceCell<u32> = unsync::OnceCell::new();
    assert_eq!(cell.get(), None);
    assert_eq!(cell.get_or_try_init(|| Err::<u32, u8>(2)), Err(2));
    assert_eq!(cell.get(), None);
    assert_eq!(cell.get_or_init(|| 8), &8);
    assert_eq!(cell.set(9), Err(9));
    assert_eq!(cell.try_insert(9), Err((&8, 9)));
    assert_eq!(format!("{cell:?}"), "OnceCell(8)");
    let mut cell = cell;
    assert_eq!(cell.take(), Some(8));
    assert_eq!(cell.into_inner(), None);
}

fn panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let payload = std::panic::catch_unwind(f).expect_err("accepted without a panic");
    match payload.downcast::<&'static str>() {
        Ok(s) => (*s).to_owned(),
        Err(payload) => *payload.downcast::<String>().expect("a string payload"),
    }
}

#[test]
fn unsync_get_or_try_init_refuses_a_reentrant_initialisation() {
    let by_set = panic_message(|| {
        let cell: unsync::OnceCell<u32> = unsync::OnceCell::new();
        let _ = cell.get_or_try_init(|| {
            assert_eq!(cell.set(1), Ok(()));
            Ok::<u32, ()>(2)
        });
    });
    assert!(by_set.contains("reentrant init"), "got {by_set:?}");
    assert!(
        std::panic::catch_unwind(|| {
            let cell: unsync::OnceCell<u32> = unsync::OnceCell::new();
            let _ = cell.get_or_try_init(|| Ok::<u32, ()>(*cell.get_or_init(|| 10) + 1));
        })
        .is_err(),
        "reentrant get_or_init inside get_or_try_init must panic"
    );
    assert!(
        std::panic::catch_unwind(|| {
            let cell: unsync::OnceCell<u32> = unsync::OnceCell::new();
            let _ = cell.get_or_init(|| {
                let _ = cell.set(1);
                2
            });
        })
        .is_err(),
        "reentrant set inside get_or_init must panic"
    );
}

#[test]
fn a_panicking_lazy_initialiser_poisons_the_lazy() {
    let sync_lazy: sync::Lazy<u32> = sync::Lazy::new(|| panic!("sync init boom"));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| *sync_lazy)).is_err());
    assert_eq!(sync::Lazy::get(&sync_lazy), None);
    let poisoned = panic_message(std::panic::AssertUnwindSafe(|| {
        let _ = *sync_lazy;
    }));
    assert_eq!(poisoned, "Lazy instance has previously been poisoned");

    let unsync_lazy: unsync::Lazy<u32> = unsync::Lazy::new(|| panic!("unsync init boom"));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| *unsync_lazy)).is_err());
    assert_eq!(unsync::Lazy::get(&unsync_lazy), None);
    let poisoned_unsync = panic_message(std::panic::AssertUnwindSafe(|| {
        let _ = *unsync_lazy;
    }));
    assert_eq!(
        poisoned_unsync,
        "Lazy instance has previously been poisoned"
    );
}

#[test]
fn a_panicking_initialiser_leaves_the_cell_usable() {
    let cell: Arc<sync::OnceCell<u32>> = Arc::new(sync::OnceCell::new());
    let c = Arc::clone(&cell);
    let panicked = thread::spawn(move || {
        let _: Result<&u32, ()> = c.get_or_try_init(|| panic!("boom"));
    })
    .join();
    assert!(panicked.is_err(), "the initialiser was supposed to panic");
    assert_eq!(
        cell.get(),
        None,
        "the cell stays empty, as upstream leaves it"
    );
    assert_eq!(
        cell.get_or_init(|| 5),
        &5,
        "a later caller must not inherit a poisoned lock"
    );
}

fn gate_state(
    [cell, gate, ok_runs, ran_ok1, ran_ok2, p1, p2, obs1, obs2]: [i64; 9],
) -> aterm_spec::interp::State {
    std::collections::BTreeMap::from([
        ("cell", cell),
        ("gate", gate),
        ("ok_runs", ok_runs),
        ("ran_ok1", ran_ok1),
        ("ran_ok2", ran_ok2),
        ("p1", p1),
        ("p2", p2),
        ("obs1", obs1),
        ("obs2", obs2),
    ])
}

#[test]
fn sync_once_cell_gate_conformance_and_ungated_negative_control() {
    use std::sync::{Condvar, Mutex, OnceLock};

    let m = aterm_models_core::once_cell_gate_protocol_model();
    assert!(aterm_spec::interp::prove_and_catch(&m) > 1);
    let check_step = |prev: &aterm_spec::interp::State,
                      next: &aterm_spec::interp::State,
                      expected: &'static str| {
        assert_eq!(
            aterm_spec::interp::admits(&m, prev, next),
            Some(expected),
            "step {expected} from {prev:?} to {next:?} must refine OnceCellGateProtocol"
        );
        for inv in &m.invariants {
            assert!(
                m.check_invariant(inv.name, next),
                "invariant {} violated at {next:?}",
                inv.name
            );
        }
    };

    // 1. Conformance binding on real `sync::OnceCell`: failed init leaves cell empty;
    //    deterministic latch holds caller 1 inside `f` while caller 2 queues at `gate`,
    //    never invoking caller 2's closure and returning caller 1's value to both.
    let cell: Arc<sync::OnceCell<i64>> = Arc::new(sync::OnceCell::new());
    let mut s = m.init_state();
    assert_eq!(s, gate_state([0, 0, 0, 0, 0, 0, 0, 0, 0]));

    let err_res: Result<&i64, ()> = cell.get_or_try_init(|| {
        let s1 = gate_state([0, 0, 0, 0, 0, 1, 0, 0, 0]);
        check_step(&s, &s1, "Call1");
        let s2 = gate_state([0, 1, 0, 0, 0, 2, 0, 0, 0]);
        check_step(&s1, &s2, "Enter1");
        s = s2;
        Err(())
    });
    assert_eq!(err_res, Err(()));
    assert_eq!(cell.get(), None);
    let s_after_err = gate_state([0, 0, 0, 0, 0, 0, 0, 0, 0]);
    check_step(&s, &s_after_err, "InitErr1");
    s = s_after_err;

    let latch = Arc::new((Mutex::new((false, false)), Condvar::new()));
    let ok_runs = Arc::new(AtomicUsize::new(0));
    let t2_entered_call = Arc::new(Barrier::new(2));

    let cell1 = Arc::clone(&cell);
    let latch1 = Arc::clone(&latch);
    let runs1 = Arc::clone(&ok_runs);
    let h1 = thread::spawn(move || {
        let mut ran = 0i64;
        let v = *cell1
            .get_or_try_init(|| {
                let (mu, cv) = &*latch1;
                let mut g = mu.lock().unwrap();
                g.0 = true;
                cv.notify_all();
                while !g.1 {
                    g = cv.wait(g).unwrap();
                }
                ran = 1;
                runs1.fetch_add(1, Ordering::SeqCst);
                Ok::<i64, ()>(1)
            })
            .unwrap();
        (ran, v)
    });

    {
        let (mu, cv) = &*latch;
        let mut g = mu.lock().unwrap();
        while !g.0 {
            g = cv.wait(g).unwrap();
        }
    }
    let s_t1_call = gate_state([0, 0, 0, 0, 0, 1, 0, 0, 0]);
    check_step(&s, &s_t1_call, "Call1");
    let s_t1_enter = gate_state([0, 1, 0, 0, 0, 2, 0, 0, 0]);
    check_step(&s_t1_call, &s_t1_enter, "Enter1");
    s = s_t1_enter;

    let cell2 = Arc::clone(&cell);
    let runs2 = Arc::clone(&ok_runs);
    let bar2 = Arc::clone(&t2_entered_call);
    let h2 = thread::spawn(move || {
        bar2.wait();
        let mut ran = 0i64;
        let v = *cell2
            .get_or_try_init(|| {
                ran = 1;
                runs2.fetch_add(1, Ordering::SeqCst);
                Ok::<i64, ()>(2)
            })
            .unwrap();
        (ran, v)
    });
    t2_entered_call.wait();
    assert_eq!(
        ok_runs.load(Ordering::SeqCst),
        0,
        "gate must hold caller 2 out of its closure while caller 1 is running"
    );
    let s_t2_call = gate_state([0, 1, 0, 0, 0, 2, 1, 0, 0]);
    check_step(&s, &s_t2_call, "Call2");
    s = s_t2_call;

    {
        let (mu, cv) = &*latch;
        let mut g = mu.lock().unwrap();
        g.1 = true;
        cv.notify_all();
    }
    let (ran1, obs1) = h1.join().unwrap();
    let (ran2, obs2) = h2.join().unwrap();
    let total_ok = ok_runs.load(Ordering::SeqCst) as i64;
    let stored = *cell.get().unwrap();
    assert_eq!(
        (ran1, ran2, total_ok, stored, obs1, obs2),
        (1, 0, 1, 1, 1, 1)
    );

    let s_t1_ok = gate_state([stored, 0, total_ok, ran1, 0, 3, 1, obs1, 0]);
    check_step(&s, &s_t1_ok, "InitOk1");
    let s_t2_recheck = gate_state([stored, 0, total_ok, ran1, ran2, 3, 3, obs1, obs2]);
    check_step(&s_t1_ok, &s_t2_recheck, "Enter2");

    // 2. Deterministic negative control: an ungated cell (check -> compute -> OnceLock::set -> get)
    //    lets both closures run concurrently and both return Ok, violating all three invariants.
    let ungated: Arc<OnceLock<i64>> = Arc::new(OnceLock::new());
    let both_in_closure = Arc::new(Barrier::new(2));
    let ungated_runs = Arc::new(AtomicUsize::new(0));
    let spawn_ungated = |id: i64| {
        let ungated = Arc::clone(&ungated);
        let bar = Arc::clone(&both_in_closure);
        let runs = Arc::clone(&ungated_runs);
        thread::spawn(move || {
            if let Some(&v) = ungated.get() {
                return (0i64, v);
            }
            bar.wait();
            runs.fetch_add(1, Ordering::SeqCst);
            let _ = ungated.set(id);
            (1i64, *ungated.get().unwrap())
        })
    };
    let u1 = spawn_ungated(1);
    let u2 = spawn_ungated(2);
    let (u_ran1, u_obs1) = u1.join().unwrap();
    let (u_ran2, u_obs2) = u2.join().unwrap();
    let u_ok = ungated_runs.load(Ordering::SeqCst) as i64;
    let u_cell = *ungated.get().unwrap();
    assert_eq!((u_ran1, u_ran2, u_ok), (1, 1, 2));

    let pre_race = gate_state([0, 0, 0, 0, 0, 2, 1, 0, 0]);
    let race_both_running = gate_state([0, 0, 0, 0, 0, 2, 2, 0, 0]);
    assert_eq!(
        aterm_spec::interp::admits(&m, &pre_race, &race_both_running),
        None,
        "Buggy=0 must reject concurrent entry into user closures"
    );
    let buggy = aterm_spec::interp::with_buggy(&m, 1);
    assert_eq!(
        aterm_spec::interp::admits(&buggy, &pre_race, &race_both_running),
        Some("Enter2"),
        "Buggy=1 (gate removed) admits concurrent entry"
    );
    assert!(!m.check_invariant("AtMostOneRunningInitializer", &race_both_running));

    let race_done = gate_state([u_cell, 0, u_ok, u_ran1, u_ran2, 3, 3, u_obs1, u_obs2]);
    assert!(!m.check_invariant("AtMostOneSucceededInitializer", &race_done));
    assert!(!m.check_invariant("SucceededInitializerSetCell", &race_done));

    let split_brain = gate_state([1, 0, 2, 1, 1, 3, 3, 1, 2]);
    assert!(!m.check_invariant("ReturnedValueMatchesCell", &split_brain));
}
