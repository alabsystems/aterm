// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Differential oracle: `aterm_alloc::ArrayVec` against the real crates.io
//! `arrayvec::ArrayVec`, operation for operation.
//!
//! `crates/aterm-arrayvec` republishes `aterm_alloc::ArrayVec` under the
//! package name `arrayvec` so that `[patch.crates-io]` can point `naga`,
//! `wgpu`, `wgpu-core`, `wgpu-hal`, `tiny-skia` and `vte` at it.
//! `crates/aterm-arrayvec/tests/consumer_forms.rs` proves the surface ACCEPTS
//! what those crates write. This file proves it COMPUTES what they expect: both
//! types are driven through the same scripts and a deterministic-LCG operation
//! fuzz, and their state is compared after every step — contents, length,
//! capacity accounting, `Debug` rendering, `Hash` output, drop counts and
//! whether an operation panicked.
//!
//! It lives here rather than in `crates/aterm-arrayvec/tests/` for a measured
//! reason: a dev-dependency on the registry `arrayvec` declared BY the package
//! that is itself named `arrayvec` makes `cargo test -p arrayvec` die with
//! "specification `arrayvec` is ambiguous". See the `[dev-dependencies]`
//! comment in `crates/aterm-alloc/Cargo.toml` for that probe and for why the
//! oracle is pinned to `=0.7.7` — BELOW the shim's own 0.7.8, which is the
//! half that makes the patch row work at all.
//!
//! Re-check the whole claim with `cargo test -p arrayvec` (the consumer forms)
//! and `cargo test -p aterm-alloc` (this file). Both at once needs
//! `cargo test -p arrayvec@0.7.8 -p aterm-alloc`: the oracle keeps a second
//! `arrayvec` in the graph, and cargo tolerates the bare name only when it is
//! the sole `-p`.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use arrayvec_upstream::ArrayVec as UpArrayVec;
use aterm_alloc::ArrayVec as MyArrayVec;

// ── The armed tripwire ──────────────────────────────────────────────────────

/// EVERY ASSERTION IN THIS FILE IS VACUOUS IF THIS FAILS, so it is checked
/// first and it is checked at runtime rather than trusted to a comment.
///
/// `[patch.crates-io] arrayvec = { path = "crates/aterm-arrayvec" }` — the row
/// a later agent adds to finish this swap — applies to dev-dependencies too,
/// including the shim's own. Measured in a probe workspace: with that row and a
/// dev-dep the shim could satisfy, `cargo metadata` resolved the dev edge to
/// `path+…/shim`. Were that to happen here, `UpArrayVec` would BE `MyArrayVec`,
/// every `assert_eq!` below would compare a value with itself, and the whole
/// file would pass while proving nothing.
///
/// The detector is `type_name`, and it is ARMED: the control half asserts it
/// can positively identify OUR type as well, so a `type_name` that returned
/// something useless (or a rename that broke the pattern) fails here instead of
/// silently making the upstream half unfalsifiable.
///
/// The arming was PROVED, not assumed: replacing the `arrayvec_upstream` import
/// at the top of this file with `use aterm_alloc::ArrayVec as UpArrayVec;` —
/// which is exactly the state the patch row would create — makes this test
/// fail. It was run that way once, deliberately, before being restored.
#[test]
fn oracle_is_genuinely_upstream() {
    let upstream = std::any::type_name::<UpArrayVec<u8, 4>>();
    let mine = std::any::type_name::<MyArrayVec<u8, 4>>();

    // CONTROL: the detector must positively recognise our own type. If this
    // fails, the assertion below proves nothing about the oracle.
    assert!(
        mine.starts_with("aterm_alloc::"),
        "control failed: our own ArrayVec reported as `{mine}`, so `type_name` \
         cannot be used to tell the two implementations apart"
    );
    // …and the ORDER matters: the informative assertion goes first, because in
    // the real failure the two names are IDENTICAL and a bare `assert_ne!`
    // would report "left == right" instead of what to do about it.
    assert!(
        upstream.starts_with("arrayvec::"),
        "\n\nDIFFERENTIAL ORACLE IS NOT UPSTREAM.\n\
         `arrayvec_upstream::ArrayVec` resolved to `{upstream}`, which is aterm's own \
         implementation, so every assertion in this file would compare the \
         replacement against itself and pass vacuously.\n\n\
         THE CAUSE is `[patch.crates-io] arrayvec = {{ path = \
         \"crates/aterm-arrayvec\" }}` reaching this crate's dev-dependency. The \
         `=0.7.7` pin in crates/aterm-alloc/Cargo.toml exists precisely to keep \
         that from happening — the shim declares 0.7.8, which a `=0.7.7` \
         requirement cannot resolve to — so if you are reading this, the pin has \
         been relaxed to something the shim satisfies (a `0.7`, a `^0.7.7`, or a \
         `cargo update` that moved the shim's version).\n\n\
         THE FIX is to restore a `=` requirement naming a published version the \
         shim's own version cannot satisfy, and — this is the trap that cost a \
         whole round — naming one BELOW the shim's version, never above it. A \
         pin above the shim does not fail here; it silently makes the pinned \
         registry copy satisfy the six real consumers as well, and the patch row \
         stops replacing anything. Re-read that manifest comment \
         before changing it again. Note that the graph then holds two `arrayvec` \
         versions; package specs stay unambiguous only because this dev-edge is \
         declared on `aterm-alloc` rather than on the package named `arrayvec`.\n"
    );
    assert_ne!(
        upstream, mine,
        "the two implementations are indistinguishable by name"
    );
}

// ── Comparison helpers ──────────────────────────────────────────────────────

fn hash_of<T: Hash + ?Sized>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// Compare every observable of the two vectors.
///
/// `Hash` is compared as a raw `u64` on purpose: both implementations are meant
/// to delegate to the slice, so the two hashers must produce the SAME NUMBER,
/// not merely be internally consistent. An implementation that hashed the
/// backing `[MaybeUninit<T>; N]` would still satisfy Hash/Eq inside one process
/// — it would pass every self-consistency test ever written — and this is the
/// assertion that catches it.
fn same<T, const N: usize>(mine: &MyArrayVec<T, N>, up: &UpArrayVec<T, N>, what: &str)
where
    T: std::fmt::Debug + PartialEq + Hash,
{
    assert_eq!(
        mine.as_slice(),
        up.as_slice(),
        "contents differ after {what}"
    );
    assert_eq!(mine.len(), up.len(), "len differs after {what}");
    assert_eq!(
        mine.is_empty(),
        up.is_empty(),
        "is_empty differs after {what}"
    );
    assert_eq!(mine.is_full(), up.is_full(), "is_full differs after {what}");
    assert_eq!(
        mine.capacity(),
        up.capacity(),
        "capacity differs after {what}"
    );
    assert_eq!(
        mine.remaining_capacity(),
        up.remaining_capacity(),
        "remaining_capacity differs after {what}"
    );
    assert_eq!(
        format!("{mine:?}"),
        format!("{up:?}"),
        "Debug rendering differs after {what}"
    );
    assert_eq!(
        hash_of(mine),
        hash_of(up),
        "Hash output differs after {what}"
    );
    assert_eq!(
        hash_of(mine),
        hash_of(mine.as_slice()),
        "Hash does not delegate to the slice after {what}"
    );
}

/// Run a closure with panic output suppressed, reporting only whether it
/// unwound. Used to compare PANIC BEHAVIOUR, which for a fixed-capacity vector
/// is as much a part of the contract as the contents.
fn panicked(f: impl FnOnce()) -> bool {
    let prev = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    panic::set_hook(prev);
    result.is_err()
}

// ── Scripted operations ─────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum Op {
    Push(u32),
    TryPush(u32),
    Pop,
    Insert(usize, u32),
    Remove(usize),
    Truncate(usize),
    Clear,
    RetainOdd,
    RetainAndBump,
    ExtendFromSlice(u32, usize),
    TryExtendFromSlice(u32, usize),
    Extend(usize),
    Drain(usize, usize),
    DrainFull,
    DrainFrom(usize),
    Sort,
}

const CAP: usize = 8;

fn apply(op: Op, mine: &mut MyArrayVec<u32, CAP>, up: &mut UpArrayVec<u32, CAP>) {
    let label = format!("{op:?}");
    match op {
        Op::Push(v) => {
            let a = panicked(|| {
                let mut tmp = mine.clone();
                tmp.push(v);
                *mine = tmp;
            });
            let b = panicked(|| {
                let mut tmp = up.clone();
                tmp.push(v);
                *up = tmp;
            });
            assert_eq!(a, b, "push panic parity differs for {label}");
        }
        Op::TryPush(v) => {
            let a = mine.try_push(v);
            let b = up.try_push(v);
            assert_eq!(
                a.is_err(),
                b.is_err(),
                "try_push Ok/Err polarity differs for {label}"
            );
            if let (Err(ea), Err(eb)) = (a, b) {
                assert_eq!(
                    ea.element(),
                    eb.element(),
                    "try_push returned a different element"
                );
            }
        }
        Op::Pop => assert_eq!(mine.pop(), up.pop(), "pop differs"),
        Op::Insert(i, v) => {
            let i = i % (mine.len() + 1);
            let a = panicked(|| {
                let mut tmp = mine.clone();
                tmp.insert(i, v);
                *mine = tmp;
            });
            let b = panicked(|| {
                let mut tmp = up.clone();
                tmp.insert(i, v);
                *up = tmp;
            });
            assert_eq!(a, b, "insert panic parity differs for {label}");
        }
        Op::Remove(i) => {
            if mine.is_empty() {
                return;
            }
            let i = i % mine.len();
            assert_eq!(mine.remove(i), up.remove(i), "remove differs for {label}");
        }
        Op::Truncate(n) => {
            mine.truncate(n);
            up.truncate(n);
        }
        Op::Clear => {
            mine.clear();
            up.clear();
        }
        Op::RetainOdd => {
            mine.retain(|v| *v % 2 == 1);
            up.retain(|v| *v % 2 == 1);
        }
        Op::RetainAndBump => {
            // Upstream's `retain` hands the predicate `&mut T`; the writes it
            // makes must survive on both sides.
            mine.retain(|v| {
                *v = v.wrapping_add(1);
                *v % 3 != 0
            });
            up.retain(|v| {
                *v = v.wrapping_add(1);
                *v % 3 != 0
            });
        }
        Op::ExtendFromSlice(base, n) => {
            // NOT a like-for-like call: upstream keeps its `extend_from_slice`
            // `pub(crate)` (arrayvec-0.7.6/src/arrayvec.rs:1116), so the public
            // equivalent — "append these cloned elements, panicking on
            // overflow" — is `extend`. `aterm_alloc`'s public
            // `extend_from_slice` is a deliberate superset, and this is the
            // assertion that it behaves like the upstream operation it
            // generalises rather than like some third thing.
            let src: Vec<u32> = (0..n as u32).map(|i| base.wrapping_add(i)).collect();
            let a = panicked(|| {
                let mut tmp = mine.clone();
                tmp.extend_from_slice(&src);
                *mine = tmp;
            });
            let b = panicked(|| {
                let mut tmp = up.clone();
                tmp.extend(src.iter().copied());
                *up = tmp;
            });
            assert_eq!(a, b, "extend_from_slice panic parity differs for {label}");
        }
        Op::TryExtendFromSlice(base, n) => {
            let src: Vec<u32> = (0..n as u32).map(|i| base.wrapping_add(i)).collect();
            let a = mine.try_extend_from_slice(&src);
            let b = up.try_extend_from_slice(&src);
            assert_eq!(
                a.is_err(),
                b.is_err(),
                "try_extend_from_slice polarity differs for {label}"
            );
        }
        Op::Extend(n) => {
            let src: Vec<u32> = (0..n as u32).collect();
            let a = panicked(|| {
                let mut tmp = mine.clone();
                tmp.extend(src.iter().copied());
                *mine = tmp;
            });
            let b = panicked(|| {
                let mut tmp = up.clone();
                tmp.extend(src.iter().copied());
                *up = tmp;
            });
            assert_eq!(a, b, "extend panic parity differs for {label}");
        }
        Op::Drain(s, e) => {
            let len = mine.len();
            if len == 0 {
                return;
            }
            let s = s % len;
            let e = s + (e % (len - s + 1));
            let a: Vec<u32> = mine.drain(s..e).collect();
            let b: Vec<u32> = up.drain(s..e).collect();
            assert_eq!(a, b, "drain yielded different elements for {label}");
        }
        Op::DrainFull => {
            let a: Vec<u32> = mine.drain(..).collect();
            let b: Vec<u32> = up.drain(..).collect();
            assert_eq!(a, b, "drain(..) yielded different elements");
        }
        Op::DrainFrom(s) => {
            let len = mine.len();
            if len == 0 {
                return;
            }
            let s = s % len;
            let a: Vec<u32> = mine.drain(s..).collect();
            let b: Vec<u32> = up.drain(s..).collect();
            assert_eq!(a, b, "drain(s..) yielded different elements for {label}");
        }
        Op::Sort => {
            // Reached through `DerefMut`, so this exercises the slice view.
            mine.sort_unstable();
            up.sort_unstable();
        }
    }
    same(mine, up, &label);
}

#[test]
fn scripted_operation_sequence() {
    let mut mine: MyArrayVec<u32, CAP> = MyArrayVec::new();
    let mut up: UpArrayVec<u32, CAP> = UpArrayVec::new();
    same(&mine, &up, "construction");

    let script = [
        Op::Push(1),
        Op::Push(2),
        Op::Push(3),
        Op::Insert(1, 99),
        Op::Remove(0),
        Op::ExtendFromSlice(10, 3),
        Op::TryPush(7),
        Op::TryPush(8),
        // Now full: these must fail identically on both.
        Op::TryPush(9),
        Op::Push(9),
        Op::Extend(3),
        Op::Pop,
        Op::Drain(1, 3),
        Op::RetainOdd,
        Op::RetainAndBump,
        Op::Sort,
        Op::TryExtendFromSlice(50, 4),
        Op::TryExtendFromSlice(50, 40),
        Op::DrainFrom(1),
        Op::Truncate(1),
        Op::DrainFull,
        Op::Clear,
        Op::Pop,
    ];
    for op in script {
        apply(op, &mut mine, &mut up);
    }
}

/// Deterministic-LCG operation fuzz — the in-repo pattern (see aterm-toml's
/// differential). Fixed seeds, so a failure is reproducible from the seed alone.
#[test]
fn lcg_operation_fuzz() {
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
        fn below(&mut self, n: u64) -> usize {
            (self.next() % n) as usize
        }
    }

    for seed in [1u64, 7, 42, 1234, 99_991, 0xDEAD_BEEF] {
        let mut rng = Lcg(seed);
        let mut mine: MyArrayVec<u32, CAP> = MyArrayVec::new();
        let mut up: UpArrayVec<u32, CAP> = UpArrayVec::new();

        for step in 0..400 {
            let v = rng.next() as u32;
            let op = match rng.below(15) {
                0 => Op::Push(v),
                1 => Op::TryPush(v),
                2 => Op::Pop,
                3 => Op::Insert(rng.below(CAP as u64 + 1), v),
                4 => Op::Remove(rng.below(CAP as u64 + 1)),
                5 => Op::Truncate(rng.below(CAP as u64 + 1)),
                6 => Op::Clear,
                7 => Op::RetainOdd,
                8 => Op::RetainAndBump,
                9 => Op::ExtendFromSlice(v, rng.below(CAP as u64 + 2)),
                10 => Op::TryExtendFromSlice(v, rng.below(CAP as u64 + 2)),
                11 => Op::Extend(rng.below(CAP as u64 + 2)),
                12 => Op::Drain(rng.below(CAP as u64 + 1), rng.below(CAP as u64 + 1)),
                13 => Op::Sort,
                _ => Op::DrainFrom(rng.below(CAP as u64 + 1)),
            };
            apply(op, &mut mine, &mut up);
            assert_eq!(
                mine.as_slice(),
                up.as_slice(),
                "diverged at seed {seed} step {step} on {op:?}"
            );
        }
    }
}

// ── The behaviours that are dangerous to get subtly wrong ───────────────────

/// `Extend` and `FromIterator` must PANIC on overflow, never truncate — and the
/// elements written before the panic must survive on both sides. A truncating
/// `extend` would silently drop DXC compiler arguments in `wgpu-hal` and a
/// destination barrier in `wgpu-core`, with nothing to observe but wrong pixels.
#[test]
fn overflow_policy_parity() {
    // CONTROL: an extend that FITS must not panic on either side. Without this,
    // "both panicked" below could be satisfied by an implementation that
    // panics unconditionally.
    let mut mine: MyArrayVec<u32, 4> = MyArrayVec::new();
    let mut up: UpArrayVec<u32, 4> = UpArrayVec::new();
    assert!(!panicked(|| mine.extend([1, 2, 3, 4])));
    assert!(!panicked(|| up.extend([1, 2, 3, 4])));
    assert_eq!(mine.as_slice(), up.as_slice());

    // Overflowing extend: both panic, and both keep the prefix they wrote.
    let mut mine: MyArrayVec<u32, 4> = MyArrayVec::new();
    let mut up: UpArrayVec<u32, 4> = UpArrayVec::new();
    let a = panicked(|| mine.extend([1, 2, 3, 4, 5, 6]));
    let b = panicked(|| up.extend([1, 2, 3, 4, 5, 6]));
    assert!(
        a && b,
        "extend must panic on overflow (mine={a}, upstream={b})"
    );
    assert_eq!(
        mine.as_slice(),
        up.as_slice(),
        "the prefix retained after an overflowing extend differs"
    );

    // Overflowing collect (`from_iter`), the load-bearing over-limit GUARD at
    // wgpu-core/src/device/resource.rs:3740 and wgpu/src/backend/wgpu_core.rs:1308.
    let a = panicked(|| {
        let _: MyArrayVec<u32, 4> = (1..=5).collect();
    });
    let b = panicked(|| {
        let _: UpArrayVec<u32, 4> = (1..=5).collect();
    });
    assert!(
        a && b,
        "collect must panic on overflow (mine={a}, upstream={b})"
    );

    // …and the same collect one element smaller must not, on either side.
    let a = panicked(|| {
        let _: MyArrayVec<u32, 4> = (1..=4).collect();
    });
    let b = panicked(|| {
        let _: UpArrayVec<u32, 4> = (1..=4).collect();
    });
    assert!(!a && !b, "control: a fitting collect must not panic");
}

/// `into_inner` returns `Ok` ONLY when the vector is full. naga spells this
/// `.into_inner().unwrap()` in its const-expression evaluator; an inverted test
/// would build shader constants out of a partially-filled array and never
/// panic.
#[test]
fn into_inner_polarity_parity() {
    for fill in 0..=4usize {
        let mine: MyArrayVec<u32, 4> = (0..fill as u32).collect();
        let up: UpArrayVec<u32, 4> = (0..fill as u32).collect();
        let a = mine.into_inner();
        let b = up.into_inner();
        assert_eq!(
            a.is_ok(),
            b.is_ok(),
            "into_inner Ok/Err polarity differs at fill={fill}"
        );
        match (a, b) {
            (Ok(x), Ok(y)) => assert_eq!(x, y, "into_inner array differs at fill={fill}"),
            (Err(x), Err(y)) => assert_eq!(
                x.as_slice(),
                y.as_slice(),
                "into_inner Err payload differs at fill={fill}"
            ),
            _ => unreachable!("polarity already asserted equal"),
        }
    }
}

/// By-value iteration: order, `size_hint`, `DoubleEndedIterator`, and — the
/// part that cannot be seen from the outside — that the un-yielded remainder is
/// dropped EXACTLY ONCE. A missing drop leaks an `Arc<Texture>` per early
/// return in `wgpu-core`; a double drop is a use-after-free.
#[test]
fn into_iter_parity_including_drop_counts() {
    // Order and both ends.
    let mine: MyArrayVec<u32, 8> = (0..6).collect();
    let up: UpArrayVec<u32, 8> = (0..6).collect();
    let mut mi = mine.into_iter();
    let mut ui = up.into_iter();
    assert_eq!(mi.size_hint(), ui.size_hint());
    assert_eq!(mi.next(), ui.next());
    assert_eq!(mi.next_back(), ui.next_back());
    assert_eq!(mi.size_hint(), ui.size_hint());
    assert_eq!(mi.as_slice(), ui.as_slice());
    assert_eq!(format!("{mi:?}"), format!("{ui:?}"));
    assert_eq!(mi.collect::<Vec<_>>(), ui.collect::<Vec<_>>());

    // Drop counts, for every prefix length, on both sides.
    struct Bomb(Rc<Cell<usize>>);
    impl Drop for Bomb {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    for taken in 0..=5usize {
        let mine_counter = Rc::new(Cell::new(0));
        let up_counter = Rc::new(Cell::new(0));

        let mut m: MyArrayVec<Bomb, 8> = MyArrayVec::new();
        let mut u: UpArrayVec<Bomb, 8> = UpArrayVec::new();
        for _ in 0..5 {
            m.push(Bomb(Rc::clone(&mine_counter)));
            u.push(Bomb(Rc::clone(&up_counter)));
        }
        {
            let mut mi = m.into_iter();
            let mut ui = u.into_iter();
            for _ in 0..taken {
                drop(mi.next().unwrap());
                drop(ui.next().unwrap());
            }
            assert_eq!(
                mine_counter.get(),
                up_counter.get(),
                "drop count after {taken} yielded elements differs"
            );
            // iterators dropped here, with 5 - taken elements un-yielded
        }
        assert_eq!(
            mine_counter.get(),
            up_counter.get(),
            "drop count after dropping the iterator differs (taken={taken})"
        );
        assert_eq!(
            mine_counter.get(),
            5,
            "every element must be dropped exactly once (taken={taken})"
        );
    }
}

/// `drain` must actually REMOVE, on every range shape, and dropping the
/// iterator early must leave the same vector an exhausted one would.
#[test]
fn drain_parity_over_every_range_shape() {
    for len in 0..=6usize {
        for start in 0..=len {
            for end in start..=len {
                let mut mine: MyArrayVec<u32, 8> = (0..len as u32).collect();
                let mut up: UpArrayVec<u32, 8> = (0..len as u32).collect();

                let a: Vec<u32> = mine.drain(start..end).collect();
                let b: Vec<u32> = up.drain(start..end).collect();
                assert_eq!(
                    a, b,
                    "drain({start}..{end}) on len {len} yielded differently"
                );
                same(&mine, &up, &format!("drain({start}..{end}) on len {len}"));

                // …and the same range, dropped without being consumed.
                let mut mine: MyArrayVec<u32, 8> = (0..len as u32).collect();
                let mut up: UpArrayVec<u32, 8> = (0..len as u32).collect();
                drop(mine.drain(start..end));
                drop(up.drain(start..end));
                same(
                    &mine,
                    &up,
                    &format!("dropped drain({start}..{end}) on len {len}"),
                );
            }
        }

        // The unbounded forms `wgpu-hal` actually writes.
        let mut mine: MyArrayVec<u32, 8> = (0..len as u32).collect();
        let mut up: UpArrayVec<u32, 8> = (0..len as u32).collect();
        let a: Vec<u32> = mine.drain(..).collect();
        let b: Vec<u32> = up.drain(..).collect();
        assert_eq!(a, b, "drain(..) on len {len} yielded differently");
        same(&mine, &up, &format!("drain(..) on len {len}"));
    }
}

/// `clone_from` must land on the same value whichever way the lengths compare —
/// our override reuses the initialized prefix exactly as upstream's does.
#[test]
fn clone_from_parity() {
    for dst_len in 0..=6usize {
        for src_len in 0..=6usize {
            let mut mine: MyArrayVec<u32, 8> = (0..dst_len as u32).collect();
            let mut up: UpArrayVec<u32, 8> = (0..dst_len as u32).collect();
            let msrc: MyArrayVec<u32, 8> = (100..100 + src_len as u32).collect();
            let usrc: UpArrayVec<u32, 8> = (100..100 + src_len as u32).collect();
            mine.clone_from(&msrc);
            up.clone_from(&usrc);
            same(&mine, &up, &format!("clone_from({dst_len} <- {src_len})"));
        }
    }
}

/// The by-reference iterators are bounded by `len`, not by `N`, on both sides.
/// `tiny-skia` writes through every element a `for x in &mut av` visits.
#[test]
fn by_reference_iteration_parity() {
    let mut mine: MyArrayVec<u32, 8> = (0..3).collect();
    let mut up: UpArrayVec<u32, 8> = (0..3).collect();

    let a: Vec<u32> = (&mine).into_iter().copied().collect();
    let b: Vec<u32> = (&up).into_iter().copied().collect();
    assert_eq!(a, b);
    assert_eq!(a.len(), 3, "by-ref iteration must walk 0..len, not 0..N");

    for v in &mut mine {
        *v += 1;
    }
    for v in &mut up {
        *v += 1;
    }
    same(&mine, &up, "for v in &mut av");
}

/// `Deref`-reached slice behaviour and equality, side by side.
#[test]
fn deref_and_equality_parity() {
    let mut mine: MyArrayVec<u32, 8> = [5, 1, 4, 2].into_iter().collect();
    let mut up: UpArrayVec<u32, 8> = [5, 1, 4, 2].into_iter().collect();
    mine.sort();
    up.sort();
    same(&mine, &up, "sort through DerefMut");

    assert_eq!(mine.first(), up.first());
    assert_eq!(mine.last(), up.last());
    assert_eq!(mine.get(2), up.get(2));
    assert_eq!(mine.contains(&4), up.contains(&4));
    assert_eq!(&mine[1..3], &up[1..3]);
    assert_eq!(
        mine[..] == *[1u32, 2, 4, 5].as_slice(),
        up[..] == *[1u32, 2, 4, 5].as_slice()
    );

    let mine2 = mine.clone();
    let up2 = up.clone();
    assert_eq!(mine == mine2, up == up2);
    let mut mine3 = mine.clone();
    let mut up3 = up.clone();
    mine3.pop();
    up3.pop();
    assert_eq!(mine == mine3, up == up3);
}

/// The one divergence that is a fact about LAYOUT rather than behaviour, pinned
/// so it cannot widen unnoticed: upstream stores the length as a `u32` and
/// `aterm_alloc` as a `usize`, so ours is never smaller and is EQUAL whenever
/// `T`'s alignment pads upstream's `u32` out anyway.
#[test]
fn layout_divergence_is_bounded() {
    use std::mem::{align_of, size_of};

    macro_rules! check {
        ($t:ty, $n:literal) => {{
            let mine = size_of::<MyArrayVec<$t, $n>>();
            let up = size_of::<UpArrayVec<$t, $n>>();
            assert!(
                mine >= up,
                "ours got SMALLER than upstream for <{}, {}> ({mine} < {up}) — the \
                 length field cannot have shrunk, so something else changed",
                stringify!($t),
                $n
            );
            assert!(
                mine - up <= size_of::<usize>(),
                "the gap for <{}, {}> is {} bytes, more than one length field",
                stringify!($t),
                $n,
                mine - up
            );
            if align_of::<$t>() >= size_of::<usize>() {
                assert_eq!(
                    mine, up,
                    "for an 8-aligned element the two layouts must coincide \
                     (upstream's u32 length is padded out anyway)"
                );
            }
        }};
    }

    check!(u8, 4);
    check!(u32, 8);
    check!(u64, 4);
    check!(std::sync::Arc<u32>, 3);
    check!(usize, 32);
}

/// Elements that own memory: the differential above uses `u32`, which cannot
/// catch a leak or a double free. This runs the same script over a `String`
/// payload with a drop counter attached, so it can.
#[test]
fn owning_elements_drop_parity() {
    struct Tracked(String, Rc<Cell<usize>>);
    impl Clone for Tracked {
        fn clone(&self) -> Self {
            Tracked(self.0.clone(), Rc::clone(&self.1))
        }
    }
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.1.set(self.1.get() + 1);
        }
    }

    let mine_drops = Rc::new(Cell::new(0));
    let up_drops = Rc::new(Cell::new(0));
    {
        let mut mine: MyArrayVec<Tracked, 8> = MyArrayVec::new();
        let mut up: UpArrayVec<Tracked, 8> = UpArrayVec::new();
        for i in 0..6u32 {
            mine.push(Tracked(format!("v{i}"), Rc::clone(&mine_drops)));
            up.push(Tracked(format!("v{i}"), Rc::clone(&up_drops)));
        }
        mine.truncate(5);
        up.truncate(5);
        assert_eq!(
            mine_drops.get(),
            up_drops.get(),
            "truncate drop count differs"
        );

        mine.remove(0);
        up.remove(0);
        assert_eq!(
            mine_drops.get(),
            up_drops.get(),
            "remove drop count differs"
        );

        drop(mine.drain(1..3));
        drop(up.drain(1..3));
        assert_eq!(mine_drops.get(), up_drops.get(), "drain drop count differs");

        mine.retain(|t| t.0.ends_with('4'));
        up.retain(|t| t.0.ends_with('4'));
        assert_eq!(
            mine_drops.get(),
            up_drops.get(),
            "retain drop count differs"
        );

        let mnames: Vec<String> = mine.iter().map(|t| t.0.clone()).collect();
        let unames: Vec<String> = up.iter().map(|t| t.0.clone()).collect();
        assert_eq!(mnames, unames, "surviving elements differ");
    }
    assert_eq!(
        mine_drops.get(),
        up_drops.get(),
        "total drop count differs after the vectors go out of scope"
    );
}
