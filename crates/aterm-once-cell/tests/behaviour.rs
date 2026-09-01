// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The CONTRACTS this shim has to keep, asserted directly.
//!
//! `crates/aterm-core-maths` needs no behaviour tests: nothing calls it. This
//! crate is called on four of the five cells, so the bodies have to be right,
//! and "it compiles" is not the same claim. What is checked here is the set of
//! properties the third-party consumers actually depend on — see the liveness
//! table in `src/lib.rs` for who depends on which.
//!
//! # The one that matters most, and its CONTROL
//!
//! `wgpu-core 29.0.3`'s `ResourcePool` (src/pool.rs) decides whether it won an
//! initialisation race by whether its closure ran:
//!
//! ```text
//! let mut strong = None;
//! let weak = entry.get_or_try_init(|| { strong = Some(constructor(..)?); Ok(weak) })?;
//! if let Some(strong) = strong { return Ok(strong); }
//! ```
//!
//! If two threads both run the closure, both believe they won and each returns
//! a different `Arc` for one key — two bind group layouts where the pool exists
//! to guarantee one. So [`sync::OnceCell::get_or_try_init`] must call its
//! closure EXACTLY ONCE, and
//! [`get_or_try_init_runs_the_closure_exactly_once_under_contention`] asserts
//! it with the same two-thread barrier shape wgpu-core uses in its own test.
//!
//! A tripwire nobody has seen fire is a tripwire nobody knows is connected, and
//! this one is easy to write in a way that can never fail. So
//! [`the_exactly_once_test_catches_a_racy_cell`] runs the SAME race against
//! `RacyCell` — a check-compute-set cell with no gate, which is the obvious
//! wrong implementation and the one a "thin wrapper over `OnceLock`" would
//! produce for the fallible path — and requires it to be caught. If that
//! control ever passes, the exactly-once test above has stopped meaning
//! anything.
//!
//! # ARMED — each plant was compiled and run, not argued
//!
//! Four defects were planted in `src/lib.rs`, each VERIFIED TO COMPILE first (a
//! plant that does not build proves nothing about the test), run, and restored.
//! The evidence is what the test actually printed:
//!
//! * `sync::OnceCell::get_or_try_init` loses its `gate` ->
//!   [`get_or_try_init_runs_the_closure_exactly_once_under_contention`] exits
//!   101 with `left: 2, right: 1` and the wgpu-core explanation. This is the
//!   plant that matters: it is precisely the "thin wrapper over `OnceLock`"
//!   implementation. An earlier draft of this line said it "passes every other
//!   test in this file"; re-measured, it fails THREE of them — the other two
//!   being `lazy_forced_concurrently_runs_its_initialiser_once` and
//!   `once_box_runs_its_initialiser_once_under_contention`, which route through
//!   the same gate. The suite is stronger than that sentence claimed, but the
//!   sentence was still a number nobody had run.
//! * `sync::OnceCell::get_or_try_init` drops the fallible path (`f().unwrap…`)
//!   -> [`a_failed_initialiser_leaves_the_cell_empty_and_retryable`] exits 101.
//! * `sync::Lazy::get` forces instead of peeking ->
//!   [`lazy_does_not_run_its_initialiser_until_forced`] exits 101 with
//!   `left: Some(11), right: None`.
//! * `race::OnceBox::get_or_init` drops to check-compute-set ->
//!   [`once_box_runs_its_initialiser_once_under_contention`] exits 101 with
//!   `left: 3, right: 1`.
//!
//! [`lazy_new_is_unbounded_in_f_the_way_wasm_bindgen_needs`] needs no plant: it
//! is a COMPILE-TIME reproduction of wasm-bindgen's unbounded `impl<T, F>`, so
//! a regression in that signature stops the test crate building. The remaining
//! tests are direct value assertions over one thread, where the assertion IS
//! the demonstration.

use once_cell::{race, sync, unsync};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

// ---------------------------------------------------------------------------
// sync::OnceCell — the exactly-once contract
// ---------------------------------------------------------------------------

/// How long the initialiser sleeps so both threads are provably inside
/// `get_or_try_init` at the same time. wgpu-core's own test uses 250 ms; the
/// barrier does the real synchronising, and the sleep only widens the window.
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
        "{} threads ran the initialiser; wgpu-core's ResourcePool decides it won the race by \
         whether its closure ran, so a second run hands out a second Arc for one key",
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
/// re-read, with nothing serialising the middle. It is a correct
/// single-assignment cell — every reader still sees one value — and it is what
/// a plain wrapper over `OnceLock` gives you for the FALLIBLE path, since
/// `OnceLock::get_or_init` cannot fail out. Only the exactly-once property is
/// missing, which is exactly the property wgpu-core needs.
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
        "THE CONTROL FAILED. Twenty two-thread races against a deliberately racy cell never \
         observed a double initialisation, so \
         `get_or_try_init_runs_the_closure_exactly_once_under_contention` is not testing what it \
         claims and would pass against a broken shim. Fix the harness before trusting it."
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

/// THE SIGNATURE CONSTRAINT that a wrapper over `core::cell::LazyCell` would
/// fail, reproduced from `wasm-bindgen-0.2.108/src/rt/mod.rs`.
///
/// wasm-bindgen builds its own `LazyCell` inside an `impl<T, F>` with NO
/// `F: FnOnce() -> T` bound, so `unsync::Lazy::new` must be callable there.
/// `core::cell::LazyCell::new` IS bounded, which is why this shim hand-rolls
/// the type instead of forwarding to it. If that ever regresses, this fails to
/// COMPILE — which is the point of writing it as a type rather than a check.
struct WasmBindgenShapedCell<T, F = fn() -> T>(unsync::Lazy<T, F>);

impl<T, F> WasmBindgenShapedCell<T, F> {
    const fn new(init: F) -> Self {
        Self(unsync::Lazy::new(init))
    }
}

/// The same construction in a CONST CONTEXT, which is where wasm-bindgen puts
/// it (a `static` behind its own `unsafe impl Sync` wrapper — sound on a
/// single-threaded wasm target, and not reproduced here because this test
/// binary is threaded). A const block asks for const evaluation without asking
/// for `Sync`, and without declaring a named constant whose type has interior
/// mutability, which is a thing to be linted for rather than to write.
///
/// This item is the assertion: it fails to COMPILE if `unsync::Lazy::new` stops
/// being a `const fn`, or gains an `F: FnOnce() -> T` bound that
/// `WasmBindgenShapedCell::new` cannot satisfy.
const _: () = {
    let _ = WasmBindgenShapedCell::<u32>::new(|| 42);
};

#[test]
fn lazy_new_is_unbounded_in_f_the_way_wasm_bindgen_needs() {
    let cell = WasmBindgenShapedCell::<u32>::new(|| 42);
    assert_eq!(*cell.0, 42);
}

/// THE AUTO-TRAIT SURFACE, pinned because two of these were WRONG.
///
/// `RefUnwindSafe` is an auto trait with a NEGATIVE impl on `UnsafeCell`.
/// Upstream's `unsync` cells contain one and therefore spell the impl out by
/// hand (`once_cell-1.21.4/src/lib.rs:430` and `:729`); the first draft of this
/// shim wrapped `core::cell::OnceCell` and `Cell` and inherited neither, so
/// `unsync::OnceCell<T>` and `unsync::Lazy<T, F>` were `!RefUnwindSafe` where
/// upstream's are. Nothing in the graph asks for it today — `wasm-bindgen`
/// feeds `maybe_catch_unwind` an `AssertUnwindSafe` at
/// `wasm-bindgen-0.2.108/src/convert/closures.rs:74` — so it never failed a
/// build. It also never could have failed one HERE: the three `unsync`
/// consumers are `js-sys`, `wasm-bindgen` and `wasm-bindgen-futures`, all of
/// them wasm-only, and no wasm target is installed on the machine that runs
/// this suite. That is exactly why it is a test and not a comment.
///
/// Sixteen bounds were measured against real upstream 1.21.4 side by side; the
/// other fourteen — every `Send`, every `Sync`, and the whole of `sync` and
/// `race` — were already identical. `sync::Lazy` is deliberately WIDER than
/// upstream (its `Mutex<Option<F>>` is unconditionally `RefUnwindSafe`, so it
/// does not need `F: RefUnwindSafe`); wider accepts every program upstream
/// accepts, and narrowing it back would need a `PhantomData` for no consumer.
#[test]
fn the_unwind_safety_surface_matches_upstream() {
    fn rus<T: std::panic::RefUnwindSafe>() {}
    fn us<T: std::panic::UnwindSafe>() {}

    // The two that regressed. `unsync::OnceCell` is upstream's lib.rs:430,
    // `unsync::Lazy` its lib.rs:729.
    rus::<unsync::OnceCell<u32>>();
    rus::<unsync::Lazy<u32>>();
    // The rest of the surface, which was already right and must stay so.
    us::<unsync::OnceCell<u32>>();
    us::<unsync::Lazy<u32>>();
    rus::<sync::OnceCell<u32>>();
    us::<sync::OnceCell<u32>>();
    rus::<sync::Lazy<u32>>();
    us::<sync::Lazy<u32>>();
    rus::<race::OnceBox<u32>>();
    us::<race::OnceBox<u32>>();

    // NO NEGATIVE HALF, AND THE ABSENCE IS DELIBERATE. The impls above are
    // bounded exactly as upstream bounds them, but "`unsync::OnceCell<Cell<u32>>`
    // is NOT RefUnwindSafe" cannot be asserted on stable — a negative trait
    // bound is a compile error, and the obvious workaround (a helper returning
    // a hardcoded `false`) asserts nothing at all. What guards the widening
    // direction is the bound written on each impl, which a reader can diff
    // against once_cell-1.21.4/src/lib.rs:430 and :729 in one line each.
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
    // Upstream's `race` module permits more than one run; this shim gives the
    // stricter guarantee (divergence 2 in src/lib.rs). Asserting the stronger
    // property is what makes that documented claim checkable.
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

// ---------------------------------------------------------------------------
// Poisoning: the private mutexes must not turn one panic into a permanent one
// ---------------------------------------------------------------------------

#[test]
fn a_panicking_initialiser_leaves_the_cell_usable() {
    // Upstream has no locks and therefore no poisoning: after a panic in `f`
    // the cell is simply still empty. The private `Mutex` here must not turn
    // that into a poisoned lock that panics every later caller.
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
