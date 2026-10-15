// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The contracts the third-party consumers rely on (the liveness table in
//! `src/lib.rs` says who relies on which).
//!
//! The one that matters most: `wgpu-core`'s `ResourcePool` decides it won an
//! initialisation race by whether its closure ran, so
//! [`sync::OnceCell::get_or_try_init`] must call it EXACTLY ONCE.
//! [`get_or_try_init_runs_the_closure_exactly_once_under_contention`] asserts
//! that with wgpu-core's two-thread barrier shape, and
//! [`the_exactly_once_test_catches_a_racy_cell`] runs the same race against an
//! ungated control cell and requires it to be caught, so the first test cannot
//! go toothless.
//!
//! Each of these plants in `src/lib.rs` was compiled, run and caught:
//!
//! * `sync::OnceCell::get_or_try_init` without its gate -> the exactly-once
//!   test (`left: 2, right: 1`), plus
//!   [`lazy_forced_concurrently_runs_its_initialiser_once`] and
//!   [`once_box_runs_its_initialiser_once_under_contention`], which share the gate.
//! * `get_or_try_init` without the fallible path ->
//!   [`a_failed_initialiser_leaves_the_cell_empty_and_retryable`].
//! * `sync::Lazy::get` forcing instead of peeking ->
//!   [`lazy_does_not_run_its_initialiser_until_forced`].
//! * `race::OnceBox::get_or_init` as check-compute-set ->
//!   [`once_box_runs_its_initialiser_once_under_contention`].
//! * `unsync::OnceCell::get_or_try_init` ignoring a failed `set` (origin/main
//!   before 2026-10-10) -> [`unsync_get_or_try_init_refuses_a_reentrant_initialisation`].

use once_cell::{race, sync, unsync};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

// ---------------------------------------------------------------------------
// sync::OnceCell — the exactly-once contract
// ---------------------------------------------------------------------------

/// How long the initialiser sleeps to widen the race window; the barrier does
/// the synchronising. wgpu-core's own test uses 250 ms.
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
        "{} threads ran the initialiser; wgpu-core's ResourcePool would hand out two Arcs for \
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

/// A `sync::OnceCell` written the obvious wrong way: check, compute, set,
/// re-read, ungated. Every reader still sees one value; only exactly-once is
/// missing. It is what a plain `OnceLock` wrapper gives the fallible path.
struct RacyCell<T>(std::sync::OnceLock<T>);

impl<T> RacyCell<T> {
    fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }

    fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
    where
        F: FnOnce() -> Result<T, E>,
    {
        if let Some(v) = self.0.get() {
            return Ok(v);
        }
        let v = f()?;
        let _ = self.0.set(v);
        Ok(self.0.get().expect("just set"))
    }
}

#[test]
fn the_exactly_once_test_catches_a_racy_cell() {
    // Same race, run against the wrong implementation. A single pass is not
    // proof of a race, so this repeats until it observes one; if the harness
    // above could not detect double-initialisation at all, this loop runs out
    // and fails, which is the signal that the real test has gone toothless.
    let mut caught = false;
    for _ in 0..20 {
        let cell = Arc::new(RacyCell::<u32>::new());
        let runs = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let cell = Arc::clone(&cell);
            let runs = Arc::clone(&runs);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let _ = cell.get_or_try_init(|| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(RACE_WINDOW);
                    Ok::<u32, ()>(7)
                });
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        if runs.load(Ordering::SeqCst) > 1 {
            caught = true;
            break;
        }
    }
    assert!(
        caught,
        "control not caught: 20 races against the racy cell never double-initialised, so the \
         exactly-once test cannot detect a broken shim"
    );
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
    // …and now it is closed: x11-dl and xkbcommon-dl both rely on the cached
    // handle never being replaced once a load succeeds.
    assert_eq!(cell.get_or_try_init(|| Ok::<u32, &str>(9)), Ok(&3));
}

#[test]
fn set_is_single_assignment_and_reports_the_loser() {
    let cell: sync::OnceCell<u32> = sync::OnceCell::new();
    assert_eq!(cell.set(1), Ok(()));
    assert_eq!(cell.set(2), Err(2), "the rejected value comes back");
    assert_eq!(cell.get(), Some(&1));
    assert_eq!(cell.try_insert(3), Err((&1, 3)));
    // `tempfile::env::override_temp_dir` distinguishes "I set it" from "someone
    // else had" exactly this way.
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

    // `wgpu-hal`'s DCompLib derives Debug over a `sync::Lazy` field, so Lazy
    // must be Debug with the upstream field names and must NOT force on debug.
    let lazy: sync::Lazy<u32> = sync::Lazy::new(|| unreachable!("Debug must not force"));
    assert_eq!(
        format!("{lazy:?}"),
        "Lazy { cell: OnceCell(Uninit), init: \"..\" }"
    );

    let boxed: race::OnceBox<u32> = race::OnceBox::new();
    assert_eq!(format!("{boxed:?}"), "OnceBox(0x0)");
    boxed.set(Box::new(1)).expect("empty");
    assert!(
        format!("{boxed:?}").starts_with("OnceBox(0x") && format!("{boxed:?}") != "OnceBox(0x0)",
        "a full OnceBox prints its address, not null: {boxed:?}"
    );
}

// ---------------------------------------------------------------------------
// Lazy — laziness, forcing, and the `F` bound that wasm-bindgen needs
// ---------------------------------------------------------------------------

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
    // `wasm-bindgen`'s `LazyCell::force` and `Deref` both land here.
    assert_eq!(unsync::Lazy::force(&lazy), "x");
    assert_eq!(runs.get(), 1);
}

/// wasm-bindgen's shape (`wasm-bindgen-0.2.108/src/rt/mod.rs`): `unsync::Lazy::new`
/// called inside an `impl<T, F>` with no `F: FnOnce() -> T` bound. Fails to
/// compile if `new` gains that bound.
struct WasmBindgenShapedCell<T, F = fn() -> T>(unsync::Lazy<T, F>);

impl<T, F> WasmBindgenShapedCell<T, F> {
    const fn new(init: F) -> Self {
        Self(unsync::Lazy::new(init))
    }
}

/// The same construction in a const context, where wasm-bindgen puts it (a
/// `static` behind its own `unsafe impl Sync`). A const block needs no `Sync`.
/// Fails to compile if `unsync::Lazy::new` stops being `const fn`.
const _: () = {
    let _ = WasmBindgenShapedCell::<u32>::new(|| 42);
};

#[test]
fn lazy_new_is_unbounded_in_f_the_way_wasm_bindgen_needs() {
    let cell = WasmBindgenShapedCell::<u32>::new(|| 42);
    assert_eq!(*cell.0, 42);
}

/// The auto-trait surface. Without hand-written impls, `unsync::OnceCell` and
/// `unsync::Lazy` are `!RefUnwindSafe` where upstream's are not (its
/// `lib.rs:430`, `:729`). No consumer asks for the bound (`wasm-bindgen` wraps
/// its argument in `AssertUnwindSafe`), so no consumer build can catch losing
/// it and this test is the only check. `sync::Lazy` is deliberately wider than
/// upstream (no `F: RefUnwindSafe` needed).
#[test]
fn the_unwind_safety_surface_matches_upstream() {
    fn rus<T: std::panic::RefUnwindSafe>() {}
    fn us<T: std::panic::UnwindSafe>() {}

    // The two that need hand-written impls.
    rus::<unsync::OnceCell<u32>>();
    rus::<unsync::Lazy<u32>>();
    // The rest of the surface.
    us::<unsync::OnceCell<u32>>();
    us::<unsync::Lazy<u32>>();
    rus::<sync::OnceCell<u32>>();
    us::<sync::OnceCell<u32>>();
    rus::<sync::Lazy<u32>>();
    us::<sync::Lazy<u32>>();
    rus::<race::OnceBox<u32>>();
    us::<race::OnceBox<u32>>();

    // No negative half: stable Rust cannot assert `!RefUnwindSafe`. The impl
    // bounds themselves guard against widening.
}

#[test]
fn lazy_is_sync_when_upstream_says_it_is() {
    fn assert_sync<T: Sync>(_: &T) {}
    // Upstream: `unsafe impl<T, F: Send> Sync for Lazy<T, F> where OnceCell<T>: Sync`.
    // Here the same bound falls out of `Mutex<Option<F>>` + `OnceCell<T>`.
    static SHARED: sync::Lazy<u32> = sync::Lazy::new(|| 5);
    assert_sync(&SHARED);
    assert_eq!(*SHARED, 5);
}

#[test]
fn lazy_forced_concurrently_runs_its_initialiser_once() {
    // `wayland-sys` has eight of these, each `dlopen`ing a library; running the
    // initialiser twice would open it twice.
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

// ---------------------------------------------------------------------------
// race::OnceBox
// ---------------------------------------------------------------------------

#[test]
fn once_box_is_a_single_assignment_cell_over_a_box() {
    let cell: race::OnceBox<Vec<u8>> = race::OnceBox::new();
    assert_eq!(cell.get(), None);
    // `ahash::set_random_source` hands back the rejected box, and uses its
    // identity to tell "already set by a user" from "already defaulted".
    assert_eq!(cell.set(Box::new(vec![1, 2])), Ok(()));
    assert_eq!(cell.set(Box::new(vec![9])), Err(Box::new(vec![9])));
    assert_eq!(cell.get(), Some(&vec![1, 2]));
    // `ahash::get_src` and `read-fonts::Once::get_or_init` both take this path.
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
    // Divergence 2 in src/lib.rs: stricter than upstream's `race`.
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

// ---------------------------------------------------------------------------
// unsync::OnceCell
// ---------------------------------------------------------------------------

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

/// The panic message of `f`, which must panic.
fn panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let payload = std::panic::catch_unwind(f).expect_err("accepted without a panic");
    match payload.downcast::<&'static str>() {
        Ok(s) => (*s).to_owned(),
        Err(payload) => *payload.downcast::<String>().expect("a string payload"),
    }
}

#[test]
fn unsync_get_or_try_init_refuses_a_reentrant_initialisation() {
    // Upstream asserts `self.set(val).is_ok()` with "reentrant init"
    // (once_cell-1.21.4/src/lib.rs:644) and `get_or_init` panics the same
    // way through core's cell: a closure that fills the cell it is
    // initialising is a caller bug, and its value must not be dropped quietly
    // while the caller is handed one its closure never produced.
    let by_set = panic_message(|| {
        let cell: unsync::OnceCell<u32> = unsync::OnceCell::new();
        let _ = cell.get_or_try_init(|| {
            assert_eq!(cell.set(1), Ok(()));
            Ok::<u32, ()>(2)
        });
    });
    assert!(by_set.contains("reentrant init"), "got {by_set:?}");
    let by_get_or_init = panic_message(|| {
        let cell: unsync::OnceCell<u32> = unsync::OnceCell::new();
        let _ = cell.get_or_try_init(|| Ok::<u32, ()>(*cell.get_or_init(|| 10) + 1));
    });
    assert!(
        by_get_or_init.contains("reentrant init"),
        "got {by_get_or_init:?}"
    );
    // The control: `get_or_init` has always refused it.
    let control = panic_message(|| {
        let cell: unsync::OnceCell<u32> = unsync::OnceCell::new();
        let _ = cell.get_or_init(|| {
            let _ = cell.set(1);
            2
        });
    });
    assert!(control.contains("reentrant init"), "got {control:?}");
}

// ---------------------------------------------------------------------------
// Poisoning: the private mutexes must not turn one panic into a permanent one
// ---------------------------------------------------------------------------

#[test]
fn a_panicking_initialiser_leaves_the_cell_usable() {
    // Upstream has no locks: after a panic in `f` the cell is still empty.
    // The private `Mutex` must not turn that into a poisoned lock.
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
