// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The operation script behind `tests/upstream_transcript.rs`, written so it
//! compiles unchanged against two crates named `smallvec`: this repository's
//! facade, and the crates.io 1.15.1 package it replaces.
//!
//! [`run`] drives a fixed section and a seeded operation fuzz, and writes one
//! line per step: the operation, what it returned (or `panic`), the contents,
//! the length, `spilled()`, whether `capacity()` is consistent with that
//! (`inline_size()` when inline, above it and at least `len()` when spilled),
//! whether `Hash` is the slice's, and the values of every element dropped
//! since the previous line, in drop order. Heap capacities and hash values are
//! deliberately not printed: the facade grows its heap buffer on a different
//! schedule, and `DefaultHasher` is not stable across toolchains.
//!
//! `transcript/smallvec-1.15.1.txt` is this script's output against the
//! registry crate. To re-record it, make a crate OUTSIDE this workspace (a
//! registry `smallvec` inside it would be redirected by `[patch.crates-io]`):
//!
//! ```text
//! [package]
//! name = "smallvec-recorder"
//! edition = "2024"
//! [dependencies]
//! smallvec = { version = "=1.15.1", features = ["union", "const_generics", "const_new"] }
//! [workspace]
//! ```
//!
//! with `src/main.rs` holding
//! `#[path = "<this file>"] mod script; fn main() { print!("{}", script::run()); }`,
//! and run `targo --unverified run --offline > smallvec-1.15.1.txt` there. The
//! features are the ones aterm's graph turns on (wgpu-hal and wayland-backend).

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::fmt::{Debug, Write as _};
use std::hash::{Hash, Hasher};
use std::panic::{self, AssertUnwindSafe};

use smallvec::{Array, SmallVec, smallvec};

thread_local! {
    static DROPS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// An element that records its value when it is dropped.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct E(pub u32);

impl Debug for E {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Drop for E {
    fn drop(&mut self) {
        DROPS.with(|d| d.borrow_mut().push(self.0));
    }
}

fn take_drops() -> Vec<u32> {
    DROPS.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

fn hash_of<H: Hash + ?Sized>(value: &H) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Runs `f` with the panic hook silenced; `true` if it unwound.
fn panics(f: impl FnOnce()) -> bool {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let unwound = panic::catch_unwind(AssertUnwindSafe(f)).is_err();
    panic::set_hook(hook);
    unwound
}

/// The deterministic generator (Knuth's MMIX LCG, top bits).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

struct Rec {
    out: String,
    step: usize,
}

impl Rec {
    fn line<A: Array>(&mut self, op: &str, result: &str, v: &SmallVec<A>)
    where
        A::Item: Debug + Hash,
    {
        let capacity_ok = if v.spilled() {
            v.capacity() > v.inline_size() && v.capacity() >= v.len()
        } else {
            v.capacity() == v.inline_size()
        };
        let hash_ok = hash_of(v) == hash_of(&v[..]);
        let drops = take_drops();
        writeln!(
            self.out,
            "{} {op} = {result} | {:?} n{} s{} c{} h{} d{drops:?}",
            self.step,
            &v[..],
            v.len(),
            u8::from(v.spilled()),
            u8::from(capacity_ok),
            u8::from(hash_ok),
        )
        .expect("writing to a String");
        self.step += 1;
    }
}

fn values(items: &[E]) -> Vec<u32> {
    items.iter().map(|e| e.0).collect()
}

/// The transcript: a fixed section, then the fuzz at three inline sizes.
pub fn run() -> String {
    let mut rec = Rec {
        out: String::new(),
        step: 0,
    };
    take_drops();
    fixed(&mut rec);
    fuzz::<[E; 3]>(&mut rec, 0x5eed_0003, 420);
    fuzz::<[E; 1]>(&mut rec, 0x5eed_0001, 160);
    fuzz::<[E; 0]>(&mut rec, 0x5eed_0000, 100);
    rec.out
}

/// Constructors, conversions and the comparison traits, once each.
fn fixed(rec: &mut Rec) {
    const EMPTY: SmallVec<[u32; 2]> = SmallVec::new_const();
    rec.line("new_const", "", &EMPTY);

    let v: SmallVec<[E; 3]> = smallvec![];
    rec.line("smallvec![]", "", &v);
    drop(v);
    let v: SmallVec<[E; 3]> = smallvec![E(1), E(2), E(3)];
    rec.line("smallvec![1,2,3]", "", &v);
    drop(v);
    let v: SmallVec<[E; 3]> = smallvec![E(1), E(2), E(3), E(4)];
    rec.line("smallvec![1,2,3,4]", "", &v);
    drop(v);
    let v: SmallVec<[E; 3]> = smallvec![E(7); 2];
    rec.line("smallvec![7;2]", "", &v);
    drop(v);
    let v: SmallVec<[E; 3]> = smallvec![E(8); 5];
    rec.line("smallvec![8;5]", "", &v);
    drop(v);
    let v: SmallVec<[E; 3]> = smallvec![E(9); 0];
    rec.line("smallvec![9;0]", "", &v);
    drop(v);

    for n in [0, 3, 4] {
        let v: SmallVec<[E; 3]> = SmallVec::with_capacity(n);
        rec.line(&format!("with_capacity({n})"), "", &v);
    }

    // from_vec keys on the Vec's capacity, not its length.
    for (len, cap) in [(2, 2), (2, 3), (2, 8), (5, 5), (0, 16)] {
        let mut vec = Vec::with_capacity(cap);
        vec.extend((0..len).map(E));
        let v: SmallVec<[E; 3]> = SmallVec::from_vec(vec);
        rec.line(&format!("from_vec(len {len} cap {cap})"), "", &v);
        drop(v);
        rec.line::<[E; 3]>("drop", "", &SmallVec::new());
    }
    let v: SmallVec<[E; 3]> = vec![E(4), E(5)].into();
    rec.line("From<Vec>", "", &v);
    drop(v);
    let src = [E(1), E(2), E(3), E(4)];
    let v: SmallVec<[E; 3]> = SmallVec::from(&src[..2]);
    rec.line("From<&[T]>(2)", "", &v);
    let w: SmallVec<[E; 3]> = SmallVec::from(&src[..]);
    rec.line("From<&[T]>(4)", "", &w);
    drop((v, w, src));
    let v: SmallVec<[E; 3]> = (10..15).map(E).collect();
    rec.line("collect(5)", "", &v);
    drop(v);
    let v: SmallVec<[E; 3]> = (10..15u32).filter(|n| n.is_multiple_of(2)).map(E).collect();
    rec.line("collect(filter)", "", &v);
    drop(v);

    // `Copy` items: from_slice and extend_from_slice.
    let mut c: SmallVec<[u32; 3]> = SmallVec::from_slice(&[1, 2]);
    rec.line("from_slice(2)", "", &c);
    c.extend_from_slice(&[3]);
    rec.line("extend_from_slice(1)", "", &c);
    c.extend_from_slice(&[4, 5]);
    rec.line("extend_from_slice(2)", "", &c);
    let c: SmallVec<[u32; 3]> = SmallVec::from_slice(&[1, 2, 3, 4, 5]);
    rec.line("from_slice(5)", "", &c);

    // Clone of a spilled vector whose items fit inline is inline.
    let mut v: SmallVec<[E; 3]> = (0..5).map(E).collect();
    v.truncate(2);
    rec.line("spilled truncate(2)", "", &v);
    let w = v.clone();
    rec.line("clone", "", &w);
    let into = w.into_vec();
    let w: SmallVec<[E; 3]> = SmallVec::from_vec(into);
    rec.line("into_vec+from_vec", "", &w);
    drop(w);
    let mut x: SmallVec<[E; 3]> = smallvec![E(20), E(21), E(22), E(23)];
    x.clone_from(&v);
    rec.line("clone_from(shorter)", "", &x);
    let long: SmallVec<[E; 3]> = (30..35).map(E).collect();
    x.clone_from(&long);
    rec.line("clone_from(longer)", "", &x);
    drop((v, x, long));

    // Comparison traits.
    let a: SmallVec<[E; 3]> = smallvec![E(1), E(2)];
    let b: SmallVec<[E; 2]> = smallvec![E(1), E(2)];
    let c: SmallVec<[E; 3]> = smallvec![E(1), E(3)];
    let result = format!(
        "{} {} {:?} {:?}",
        a == b,
        a == c,
        a.cmp(&c),
        a.partial_cmp(&a)
    );
    rec.line("eq/cmp", &result, &a);
    drop((a, b, c));

    // Panics.
    let mut v: SmallVec<[E; 3]> = smallvec![E(1), E(2)];
    let p = panics(|| {
        v.insert(3, E(9));
    });
    rec.line("insert(3)", if p { "panic" } else { "ok" }, &v);
    let p = panics(|| {
        v.remove(2);
    });
    rec.line("remove(2)", if p { "panic" } else { "ok" }, &v);
    let (start, end) = (2, 1);
    let p = panics(|| {
        v.drain(start..end);
    });
    rec.line("drain(2..1)", if p { "panic" } else { "ok" }, &v);
    let p = panics(|| {
        v.drain(0..3);
    });
    rec.line("drain(0..3)", if p { "panic" } else { "ok" }, &v);
    v.clear();
    let p = panics(|| {
        v.swap_remove(0);
    });
    rec.line(
        "swap_remove(0) on empty",
        if p { "panic" } else { "ok" },
        &v,
    );
    let p = panics(|| {
        v.reserve(usize::MAX);
    });
    rec.line("reserve(MAX)", if p { "panic" } else { "ok" }, &v);
}

/// A seeded operation fuzz over one vector with inline size `A::size()`.
fn fuzz<A: Array<Item = E>>(rec: &mut Rec, seed: u64, steps: usize) {
    let mut r = Lcg(seed);
    let mut v: SmallVec<A> = SmallVec::new();
    rec.line(&format!("fuzz {seed:#x} new"), "", &v);
    for _ in 0..steps {
        let x = r.below(1000) as u32;
        let len = v.len();
        match r.below(26) {
            0..=3 => {
                v.push(E(x));
                rec.line(&format!("push({x})"), "", &v);
            }
            4 => {
                let got = v.pop().map(|e| e.0);
                rec.line("pop", &format!("{got:?}"), &v);
            }
            5 | 6 => {
                let i = r.below(len + 1);
                v.insert(i, E(x));
                rec.line(&format!("insert({i},{x})"), "", &v);
            }
            7 => {
                // Out of range, but never on a full inline vector: upstream
                // reserves before it checks the index, so that case would
                // spill before panicking.
                let i = len + 1 + r.below(2);
                let p = if v.spilled() || len < v.inline_size() {
                    panics(|| v.insert(i, E(x)))
                } else {
                    false
                };
                rec.line(
                    &format!("insert({i}) oob"),
                    if p { "panic" } else { "skip" },
                    &v,
                );
            }
            8 => {
                let i = r.below(len + 1);
                let mut got = None;
                let p = panics(|| got = Some(v.remove(i).0));
                rec.line(&format!("remove({i})"), &format!("{got:?} {p}"), &v);
            }
            9 => {
                let i = r.below(len + 1);
                let mut got = None;
                let p = panics(|| got = Some(v.swap_remove(i).0));
                rec.line(&format!("swap_remove({i})"), &format!("{got:?} {p}"), &v);
            }
            10 => {
                let n = r.below(len + 2);
                v.truncate(n);
                rec.line(&format!("truncate({n})"), "", &v);
            }
            11 => {
                let k = x % 3;
                let mut seen = Vec::new();
                v.retain(|e| {
                    seen.push(e.0);
                    e.0 = e.0.wrapping_add(1);
                    e.0 % 3 != k
                });
                rec.line(&format!("retain(!= {k} mod 3)"), &format!("{seen:?}"), &v);
            }
            12 => {
                let n = r.below(len + 5);
                v.resize(n, E(x));
                rec.line(&format!("resize({n},{x})"), "", &v);
            }
            13 => {
                let n = r.below(len + 5);
                let mut next = x;
                v.resize_with(n, || {
                    next += 1;
                    E(next)
                });
                rec.line(&format!("resize_with({n})"), "", &v);
            }
            14 => {
                let n = r.below(7);
                if r.below(2) == 0 {
                    v.reserve(n);
                    rec.line(&format!("reserve({n})"), "", &v);
                } else {
                    v.reserve_exact(n);
                    rec.line(&format!("reserve_exact({n})"), "", &v);
                }
            }
            15 => {
                let m = r.below(6) as u32;
                if r.below(2) == 0 {
                    v.extend((x..x + m).map(E));
                    rec.line(&format!("extend({x}..+{m})"), "", &v);
                } else {
                    v.extend((x..x + m).filter(|_| true).map(E));
                    rec.line(&format!("extend(filter {x}..+{m})"), "", &v);
                }
            }
            16 | 17 => {
                let s = r.below(len + 1);
                let e = s + r.below(len - s + 1);
                let (label, got): (String, Vec<E>) = match r.below(5) {
                    0 => (format!("drain({s}..{e})"), v.drain(s..e).collect()),
                    1 => (format!("drain({s}..)"), v.drain(s..).collect()),
                    2 => (format!("drain(..{e})"), v.drain(..e).collect()),
                    3 => ("drain(..)".to_string(), v.drain(..).collect()),
                    _ if e > s => (
                        format!("drain({s}..={})", e - 1),
                        v.drain(s..=e - 1).collect(),
                    ),
                    _ => (format!("drain({s}..{e}) empty"), v.drain(s..e).collect()),
                };
                let yielded = values(&got);
                drop(got);
                rec.line(&label, &format!("{yielded:?}"), &v);
            }
            18 => {
                // A Drain consumed from both ends and dropped part-way.
                let s = r.below(len + 1);
                let e = s + r.below(len - s + 1);
                let front = r.below(3);
                let back = r.below(3);
                let mut got = Vec::new();
                {
                    let mut d = v.drain(s..e);
                    got.push(d.len());
                    for _ in 0..front {
                        got.extend(d.next().map(|e| e.0 as usize));
                    }
                    for _ in 0..back {
                        got.extend(d.next_back().map(|e| e.0 as usize));
                    }
                    got.push(d.len());
                }
                rec.line(
                    &format!("drain({s}..{e}) front {front} back {back}"),
                    &format!("{got:?}"),
                    &v,
                );
            }
            19 => {
                // A by-value iterator over a clone, consumed part-way.
                let take = r.below(len + 1);
                let back = r.below(2);
                let mut it = v.clone().into_iter();
                let mut got: Vec<u32> = it.by_ref().take(take).map(|e| e.0).collect();
                for _ in 0..back {
                    got.extend(it.next_back().map(|e| e.0));
                }
                let rest = it.len();
                drop(it);
                rec.line(
                    &format!("into_iter take {take} back {back}"),
                    &format!("{got:?} rest {rest}"),
                    &v,
                );
            }
            20 => {
                let c = v.clone();
                rec.line("clone", &format!("{}", c == v), &c);
                drop(c);
            }
            21 => {
                let vec = std::mem::take(&mut v).into_vec();
                let cap_ok = vec.capacity() >= vec.len();
                v = SmallVec::from_vec(vec);
                rec.line("into_vec+from_vec", &format!("{cap_ok}"), &v);
            }
            22 => {
                let other: SmallVec<A> = (0..r.below(5) as u32).map(|n| E(n * 100)).collect();
                let result = format!("{:?} {}", v.cmp(&other), v == other);
                drop(other);
                rec.line("cmp", &result, &v);
            }
            23 => {
                // `clear` keeps a spilled buffer; a fresh vector is inline
                // again, so the fuzz does not stay spilled for good.
                if x.is_multiple_of(2) {
                    v.clear();
                    rec.line("clear", "", &v);
                } else {
                    v = SmallVec::new();
                    rec.line("reset", "", &v);
                }
            }
            _ => {
                let mut items = v.iter_mut();
                if let Some(first) = items.next() {
                    first.0 += 1;
                }
                let sum: u32 = v.iter().map(|e| e.0).sum();
                let first = v.first().map(|e| e.0);
                rec.line("iter_mut/first", &format!("{first:?} {sum}"), &v);
            }
        }
    }
    drop(v);
    rec.line::<A>("fuzz drop", "", &SmallVec::new());
}
