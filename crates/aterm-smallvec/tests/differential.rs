// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Differential LCG fuzz tests comparing `SmallVec` against a `Vec`-plus-spill-bit
//! model across inline sizes 0, 1, 2, 4, and 8.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::panic::{self, AssertUnwindSafe};

use smallvec::SmallVec;

/// Upstream's contract as a `Vec` plus the one bit of state it adds.
#[derive(Clone)]
struct Model {
    items: Vec<u32>,
    inline: usize,
    spilled: bool,
}

impl Model {
    fn new(inline: usize) -> Self {
        Model {
            items: Vec::new(),
            inline,
            spilled: false,
        }
    }
    /// Growth past the inline size spills; nothing un-spills in place.
    fn grew(&mut self) {
        self.spilled |= self.items.len() > self.inline;
    }
    /// `reserve` of `n` more spills an inline vector that cannot hold them.
    fn reserved(&mut self, n: usize) {
        self.spilled |= self.items.len() + n > self.inline;
    }
}

fn hash_of<H: Hash + ?Sized>(value: &H) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn panicked(f: impl FnOnce()) -> bool {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let unwound = panic::catch_unwind(AssertUnwindSafe(f)).is_err();
    panic::set_hook(hook);
    unwound
}

fn same<const N: usize>(mine: &SmallVec<[u32; N]>, model: &Model, what: &str) {
    assert_eq!(&mine[..], &model.items[..], "contents after {what}");
    assert_eq!(mine.len(), model.items.len(), "len after {what}");
    assert_eq!(
        mine.is_empty(),
        model.items.is_empty(),
        "is_empty after {what}"
    );
    assert_eq!(mine.inline_size(), N, "inline_size after {what}");
    assert_eq!(mine.spilled(), model.spilled, "spilled after {what}");
    if mine.spilled() {
        assert!(mine.capacity() > N, "spilled capacity after {what}");
        assert!(mine.capacity() >= mine.len(), "capacity < len after {what}");
    } else {
        assert_eq!(mine.capacity(), N, "inline capacity after {what}");
    }
    assert_eq!(
        format!("{mine:?}"),
        format!("{:?}", model.items),
        "Debug after {what}"
    );
    assert_eq!(hash_of(mine), hash_of(&model.items), "Hash after {what}");
}

struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as usize
    }
    fn range(&mut self, len: usize) -> Range<usize> {
        let s = self.below(len + 1);
        s..s + self.below(len - s + 1)
    }
}

fn fuzz<const N: usize>(seed: u64, steps: usize) {
    let mut r = Lcg(seed);
    let mut mine: SmallVec<[u32; N]> = SmallVec::new();
    let mut model = Model::new(N);
    for step in 0..steps {
        let x = r.below(10_000) as u32;
        let len = model.items.len();
        let what = match r.below(20) {
            0..=3 => {
                mine.push(x);
                model.items.push(x);
                model.grew();
                "push"
            }
            4 => {
                assert_eq!(mine.pop(), model.items.pop(), "pop");
                "pop"
            }
            5 => {
                let i = r.below(len + 2);
                let a = panicked(|| mine.insert(i, x));
                let b = panicked(|| model.items.insert(i, x));
                assert_eq!(a, b, "insert({i}) panic parity");
                model.grew();
                "insert"
            }
            6 => {
                let i = r.below(len + 1);
                let (mut a, mut b) = (None, None);
                let pa = panicked(|| a = Some(mine.remove(i)));
                let pb = panicked(|| b = Some(model.items.remove(i)));
                assert_eq!((a, pa), (b, pb), "remove({i})");
                "remove"
            }
            7 => {
                let i = r.below(len + 1);
                let (mut a, mut b) = (None, None);
                let pa = panicked(|| a = Some(mine.swap_remove(i)));
                let pb = panicked(|| b = Some(model.items.swap_remove(i)));
                assert_eq!((a, pa), (b, pb), "swap_remove({i})");
                "swap_remove"
            }
            8 => {
                let n = r.below(len + 2);
                mine.truncate(n);
                model.items.truncate(n);
                "truncate"
            }
            9 => {
                let k = x % 3;
                mine.retain(|v| {
                    *v = v.wrapping_add(1);
                    *v % 3 != k
                });
                model.items.retain_mut(|v| {
                    *v = v.wrapping_add(1);
                    *v % 3 != k
                });
                "retain"
            }
            10 => {
                let n = r.below(len + N + 3);
                mine.resize(n, x);
                model.items.resize(n, x);
                model.grew();
                "resize"
            }
            11 => {
                let n = r.below(N + 4);
                if r.below(2) == 0 {
                    mine.reserve(n);
                } else {
                    mine.reserve_exact(n);
                }
                model.reserved(n);
                "reserve"
            }
            12 => {
                let src: Vec<u32> = (x..x + r.below(N + 3) as u32).collect();
                match r.below(3) {
                    0 => mine.extend(src.iter().copied()),
                    1 => mine.extend(src.iter().copied().filter(|_| true)),
                    _ => mine.extend_from_slice(&src),
                }
                model.items.extend_from_slice(&src);
                model.grew();
                "extend"
            }
            13 | 14 => {
                let range = r.range(len);
                let a: Vec<u32> = mine.drain(range.clone()).collect();
                let b: Vec<u32> = model.items.drain(range.clone()).collect();
                assert_eq!(a, b, "drain({range:?})");
                "drain"
            }
            15 => {
                let range = r.range(len);
                let (front, back) = (r.below(3), r.below(3));
                let mut a = Vec::new();
                let mut b = Vec::new();
                {
                    let mut d = mine.drain(range.clone());
                    a.extend(d.by_ref().take(front));
                    a.extend(d.by_ref().rev().take(back));
                }
                {
                    let mut d = model.items.drain(range.clone());
                    b.extend(d.by_ref().take(front));
                    b.extend(d.by_ref().rev().take(back));
                }
                assert_eq!(a, b, "partial drain({range:?})");
                "partial drain"
            }
            16 => {
                let a = mine.clone();
                let mut b = model.clone();
                b.spilled = b.items.len() > N;
                same(&a, &b, "clone");
                let mut c: SmallVec<[u32; N]> = (0..r.below(N + 3) as u32).collect();
                c.clone_from(&mine);
                assert_eq!(c, mine, "clone_from");
                "clone"
            }
            17 => {
                let vec = std::mem::take(&mut mine).into_vec();
                assert_eq!(vec, model.items, "into_vec");
                let spilled = vec.capacity() > N;
                mine = SmallVec::from_vec(vec);
                model.spilled = spilled;
                "into_vec + from_vec"
            }
            18 => {
                let take = r.below(len + 1);
                let a: Vec<u32> = mine.clone().into_iter().rev().take(take).collect();
                let b: Vec<u32> = model.items.iter().rev().take(take).copied().collect();
                assert_eq!(a, b, "into_iter().rev()");
                "into_iter"
            }
            _ => {
                if x.is_multiple_of(2) {
                    mine.clear();
                    model.items.clear();
                } else {
                    mine = SmallVec::new();
                    model = Model::new(N);
                }
                "clear"
            }
        };
        same(&mine, &model, &format!("seed {seed:#x} step {step} {what}"));
    }
}

#[test]
fn lcg_operation_fuzz_at_five_inline_sizes() {
    for seed in 0..40u64 {
        fuzz::<0>(seed, 300);
        fuzz::<1>(seed, 300);
        fuzz::<2>(seed, 300);
        fuzz::<4>(seed, 300);
        fuzz::<8>(seed, 300);
    }
}

/// Upstream's constructors, each against the spill rule it documents.
#[test]
fn constructors_follow_upstreams_spill_rules() {
    let with: SmallVec<[u32; 4]> = SmallVec::with_capacity(4);
    assert!(!with.spilled());
    let with: SmallVec<[u32; 4]> = SmallVec::with_capacity(5);
    assert!(with.spilled() && with.capacity() >= 5);

    let mut long_buffer = Vec::with_capacity(32);
    long_buffer.push(1u32);
    let v: SmallVec<[u32; 4]> = SmallVec::from_vec(long_buffer);
    assert!(v.spilled() && v.capacity() >= 32);
    let v: SmallVec<[u32; 4]> = SmallVec::from_vec(vec![1, 2]);
    assert!(!v.spilled());

    let v: SmallVec<[u32; 2]> = SmallVec::from_elem(7, 2);
    assert!(!v.spilled() && v[..] == [7, 7]);
    let v: SmallVec<[u32; 2]> = SmallVec::from_elem(7, 3);
    assert!(v.spilled() && v[..] == [7, 7, 7]);

    let v: SmallVec<[u32; 2]> = SmallVec::from_slice(&[1, 2, 3]);
    assert!(v.spilled() && v.capacity() >= 3);

    let mut v: SmallVec<[u32; 2]> = SmallVec::from_slice(&[1, 2, 3]);
    v.pop();
    assert!(v.spilled());
    assert!(!v.clone().spilled());

    const EMPTY: SmallVec<[String; 3]> = SmallVec::new_const();
    assert!(EMPTY.is_empty() && !EMPTY.spilled());
}

/// A `SmallVec` of one inline size compares equal to another's, as upstream's
/// `PartialEq<SmallVec<B>> for SmallVec<A>` allows; order is the slice's.
#[test]
fn comparison_spans_inline_sizes() {
    let a: SmallVec<[u32; 2]> = SmallVec::from_slice(&[1, 2, 3]);
    let b: SmallVec<[u32; 8]> = SmallVec::from_slice(&[1, 2, 3]);
    assert!(a == b);
    let c: SmallVec<[u32; 2]> = SmallVec::from_slice(&[1, 2, 4]);
    assert!(a < c);
    assert_eq!(a.cmp(&c), [1, 2, 3].cmp(&[1, 2, 4]));
}
