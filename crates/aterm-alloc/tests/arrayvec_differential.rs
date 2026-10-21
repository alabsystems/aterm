// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm_alloc::ArrayVec` against a `Vec` bounded to the same capacity,
//! operation for operation.
//!
//! `crates/aterm-arrayvec` republishes `aterm_alloc::ArrayVec` under the
//! package name `arrayvec` so that `[patch.crates-io]` can point `naga`,
//! `wgpu`, `wgpu-core` and `wgpu-hal` at it.
//! `crates/aterm-arrayvec/tests/consumer_forms.rs` proves the surface accepts
//! what those crates write. This file proves it computes what they expect: both
//! are driven through the same scripts and a deterministic-LCG operation fuzz,
//! and their state is compared after every step — contents, length, capacity
//! accounting, `Debug` rendering, `Hash` output, drop counts and whether an
//! operation panicked. The model encodes upstream `arrayvec`'s contract: a
//! full vector refuses (`try_*`) or panics (`push`, `insert`, `extend`,
//! `collect`), and an overflowing `extend` keeps the prefix it wrote.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use aterm_alloc::ArrayVec as MyArrayVec;
use model::Model as UpArrayVec;

/// A `Vec` held to capacity `N`, with upstream `arrayvec`'s overflow contract.
mod model {
    use std::ops::{Deref, DerefMut, RangeBounds};

    #[derive(Clone, PartialEq, Hash)]
    pub struct Model<T, const N: usize>(Vec<T>);

    impl<T: std::fmt::Debug, const N: usize> std::fmt::Debug for Model<T, N> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_list().entries(&self.0).finish()
        }
    }

    pub struct Full<T>(T);

    impl<T> Full<T> {
        pub fn element(self) -> T {
            self.0
        }
    }

    impl<T, const N: usize> Model<T, N> {
        pub fn new() -> Self {
            Self(Vec::with_capacity(N))
        }
        pub fn as_slice(&self) -> &[T] {
            &self.0
        }
        pub fn is_full(&self) -> bool {
            self.0.len() == N
        }
        pub fn capacity(&self) -> usize {
            N
        }
        pub fn remaining_capacity(&self) -> usize {
            N - self.0.len()
        }
        pub fn try_push(&mut self, v: T) -> Result<(), Full<T>> {
            if self.is_full() {
                return Err(Full(v));
            }
            self.0.push(v);
            Ok(())
        }
        pub fn push(&mut self, v: T) {
            assert!(self.try_push(v).is_ok(), "capacity exceeded");
        }
        pub fn insert(&mut self, i: usize, v: T) {
            assert!(!self.is_full(), "capacity exceeded");
            self.0.insert(i, v);
        }
        pub fn pop(&mut self) -> Option<T> {
            self.0.pop()
        }
        pub fn remove(&mut self, i: usize) -> T {
            self.0.remove(i)
        }
        pub fn truncate(&mut self, n: usize) {
            self.0.truncate(n);
        }
        pub fn clear(&mut self) {
            self.0.clear();
        }
        pub fn retain(&mut self, f: impl FnMut(&mut T) -> bool) {
            self.0.retain_mut(f);
        }
        pub fn drain(&mut self, r: impl RangeBounds<usize>) -> std::vec::Drain<'_, T> {
            self.0.drain(r)
        }
        pub fn into_inner(self) -> Result<[T; N], Self> {
            if self.is_full() {
                Ok(self.0.try_into().unwrap_or_else(|_| unreachable!()))
            } else {
                Err(self)
            }
        }
    }

    impl<T: Clone, const N: usize> Model<T, N> {
        pub fn try_extend_from_slice(&mut self, src: &[T]) -> Result<(), ()> {
            if src.len() > self.remaining_capacity() {
                return Err(());
            }
            self.0.extend_from_slice(src);
            Ok(())
        }
    }

    impl<T, const N: usize> Extend<T> for Model<T, N> {
        fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
            for v in iter {
                self.push(v);
            }
        }
    }

    impl<T, const N: usize> FromIterator<T> for Model<T, N> {
        fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
            let mut m = Self::new();
            m.extend(iter);
            m
        }
    }

    impl<T, const N: usize> Deref for Model<T, N> {
        type Target = [T];
        fn deref(&self) -> &[T] {
            &self.0
        }
    }

    impl<T, const N: usize> DerefMut for Model<T, N> {
        fn deref_mut(&mut self) -> &mut [T] {
            &mut self.0
        }
    }

    impl<T, const N: usize> IntoIterator for Model<T, N> {
        type Item = T;
        type IntoIter = std::vec::IntoIter<T>;
        fn into_iter(self) -> Self::IntoIter {
            self.0.into_iter()
        }
    }

    impl<'a, T, const N: usize> IntoIterator for &'a Model<T, N> {
        type Item = &'a T;
        type IntoIter = std::slice::Iter<'a, T>;
        fn into_iter(self) -> Self::IntoIter {
            self.0.iter()
        }
    }

    impl<'a, T, const N: usize> IntoIterator for &'a mut Model<T, N> {
        type Item = &'a mut T;
        type IntoIter = std::slice::IterMut<'a, T>;
        fn into_iter(self) -> Self::IntoIter {
            self.0.iter_mut()
        }
    }
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
/// to delegate to the slice, so the two hashers must produce the same number,
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
/// unwound. Used to compare panic behaviour, which for a fixed-capacity vector
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
            // `retain` hands the predicate `&mut T`; the writes it makes must
            // survive.
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
            // Upstream's public equivalent of `extend_from_slice` — "append
            // these cloned elements, panicking on overflow" — is `extend`.
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

/// Deterministic-LCG operation fuzz. Fixed seeds, so a failure is reproducible
/// from the seed alone.
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

/// `Extend` and `FromIterator` must panic on overflow, never truncate — and the
/// elements written before the panic must survive on both sides. A truncating
/// `extend` would silently drop DXC compiler arguments in `wgpu-hal` and a
/// destination barrier in `wgpu-core`, with nothing to observe but wrong pixels.
#[test]
fn overflow_policy_parity() {
    // Control: an extend that fits must not panic on either side. Without this,
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
        "extend must panic on overflow (mine={a}, model={b})"
    );
    assert_eq!(
        mine.as_slice(),
        up.as_slice(),
        "the prefix retained after an overflowing extend differs"
    );

    // Overflowing collect (`from_iter`), the load-bearing over-limit guard at
    // wgpu-core/src/device/resource.rs:3740 and wgpu/src/backend/wgpu_core.rs:1308.
    let a = panicked(|| {
        let _: MyArrayVec<u32, 4> = (1..=5).collect();
    });
    let b = panicked(|| {
        let _: UpArrayVec<u32, 4> = (1..=5).collect();
    });
    assert!(
        a && b,
        "collect must panic on overflow (mine={a}, model={b})"
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

/// `into_inner` returns `Ok` only when the vector is full. naga spells this
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
/// dropped exactly once. A missing drop leaks an `Arc<Texture>` per early
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
    assert_eq!(format!("{mi:?}"), format!("{:?}", ui.as_slice()));
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

/// `drain` must actually remove, on every range shape, and dropping the
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

/// `retain` whose predicate panics keeps what the predicate had kept and every
/// element it had not yet returned for, closes the hole between them, and
/// drops each element exactly once overall: std `Vec::retain`'s contract, which
/// upstream's `BackshiftOnDrop` mirrors. Swept over every panic position and
/// every keep/reject pattern before it, so a guard that drops the unprocessed
/// tail (or the element under examination) shows up as missing elements.
#[test]
fn retain_with_a_panicking_predicate_parity() {
    use std::cell::RefCell;

    struct Tracked(u32, Rc<RefCell<Vec<u32>>>);
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.1.borrow_mut().push(self.0);
        }
    }

    const LEN: u32 = 5;
    for panic_at in 0..LEN {
        for keep_mask in 0..(1u32 << LEN) {
            let what = format!("retain panicking at {panic_at}, keep mask {keep_mask:#07b}");
            let pred = move |t: &mut Tracked| {
                assert!(t.0 != panic_at, "predicate panics at {panic_at}");
                keep_mask & (1 << t.0) != 0
            };
            let mine_log = Rc::new(RefCell::new(Vec::new()));
            let up_log = Rc::new(RefCell::new(Vec::new()));
            let mut mine: MyArrayVec<Tracked, 8> =
                (0..LEN).map(|i| Tracked(i, Rc::clone(&mine_log))).collect();
            let mut up: UpArrayVec<Tracked, 8> =
                (0..LEN).map(|i| Tracked(i, Rc::clone(&up_log))).collect();

            assert!(panicked(|| mine.retain(pred)), "{what}: mine must unwind");
            assert!(panicked(|| up.retain(pred)), "{what}: model must unwind");

            let mine_ids: Vec<u32> = mine.iter().map(|t| t.0).collect();
            let up_ids: Vec<u32> = up.iter().map(|t| t.0).collect();
            let expected: Vec<u32> = (0..LEN)
                .filter(|&i| i >= panic_at || keep_mask & (1 << i) != 0)
                .collect();
            assert_eq!(up_ids, expected, "{what}: the model's contract");
            assert_eq!(mine_ids, up_ids, "{what}: contents after the panic differ");
            assert_eq!(
                *mine_log.borrow(),
                *up_log.borrow(),
                "{what}: elements dropped during the unwind differ"
            );

            drop(mine);
            drop(up);
            let mut all = mine_log.borrow().clone();
            all.sort_unstable();
            assert_eq!(
                all,
                (0..LEN).collect::<Vec<_>>(),
                "{what}: every element must be dropped exactly once overall"
            );
        }
    }
}
