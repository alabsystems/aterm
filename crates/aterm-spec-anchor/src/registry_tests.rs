// Copyright 2026 Andrew Yates
// Copyright (c) 2018-2025 David Tolnay
// SPDX-License-Identifier: Apache-2.0
//
// Derived from inventory 0.3.24 (src/lib.rs), offered under MIT OR Apache-2.0
// and taken here under Apache-2.0. Modified by the aterm project in 2026: the
// nodes are typed (no `dyn` erasure and no cast back on the way out, with a
// `T: Sync` bound on the types in place of `ErasedNode: Sync`), the link
// pointers are atomics, a node links at most once, and a target with no
// constructor section is a compile error rather than an empty registry.

use super::{Node, Registry};

struct Seen(u32);

crate::registry::collect!(Seen);
crate::registry::submit!(Seen => Seen(1));
crate::registry::submit!(Seen => Seen(2));

#[test]
fn the_load_time_constructors_collected_both_submits() {
    let mut seen: Vec<u32> = super::iter::<Seen>().map(|s| s.0).collect();
    seen.sort_unstable();
    assert_eq!(seen, [1, 2]);
}

#[test]
fn a_node_submitted_twice_is_linked_once() {
    static LIST: Registry<u8> = Registry::new();
    static ONE: Node<u8> = Node::new(1);
    static TWO: Node<u8> = Node::new(2);
    LIST.submit(&ONE);
    LIST.submit(&TWO);
    LIST.submit(&ONE);
    assert_eq!(LIST.iter().copied().collect::<Vec<_>>(), [2, 1]);
}
