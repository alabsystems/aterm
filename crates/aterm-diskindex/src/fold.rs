// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold rule and the node bound (docs/DESIGN-disk-map-2026-11-18.md §3.2).
//!
//! A plain folder is a node only when it is 64 MiB or more and at least 0.5% of its
//! parent; anything smaller is part of its parent's `rest`. Past [`crate::NODE_BOUND`]
//! the smallest sibling folds into `rest`. Folding moves bytes, tally and class figures
//! into the rest row, so the laws hold after it as before.

use std::collections::BTreeSet;

use crate::{Index, Measured, Node, NodeId, NodeKind};

/// The smallest plain folder that is a node of its own: 64 MiB.
pub const FOLDER_MIN_BYTES: u64 = 64 << 20;

/// Whether a plain folder of `bytes` under a parent of `parent_bytes` is a node: 64 MiB
/// or more, and at least 0.5% (one two-hundredth) of its parent.
#[must_use]
pub const fn qualifies(bytes: u64, parent_bytes: u64) -> bool {
    bytes >= FOLDER_MIN_BYTES && bytes.saturating_mul(200) >= parent_bytes
}

/// Whether a node may fold under the bound. Regions, repositories, checkouts and their
/// parts carry what law 4 protects and the rails draw, so they never fold; rest rows
/// are where folding lands.
#[must_use]
pub const fn foldable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Folder | NodeKind::Build | NodeKind::Cache | NodeKind::Temp
    )
}

/// Moves `id` and everything under it into its parent's `rest`, making that row when it
/// is missing. A subtree holding a fate that removes something stays a node, so no fate
/// is lost to a fold. The rest row takes the bytes, tally, class figures, growth, newest
/// write, oldest `as_of` and carried mark of what it took.
fn fold_into_rest(index: &mut Index, id: &NodeId) {
    let Some(node) = index.nodes.get(id) else {
        return;
    };
    let Some(parent) = node.parent.clone() else {
        return;
    };
    let mut gone: Vec<NodeId> = vec![id.clone()];
    let mut seen: BTreeSet<NodeId> = BTreeSet::from([id.clone()]);
    let mut i = 0;
    while i < gone.len() {
        let at = gone[i].clone();
        for n in index.nodes.values() {
            if n.parent.as_ref() == Some(&at) && seen.insert(n.id.clone()) {
                gone.push(n.id.clone());
            }
        }
        i += 1;
    }
    if gone
        .iter()
        .any(|g| index.nodes.get(g).is_some_and(|n| n.fate.removes()))
    {
        return;
    }
    let Some(moved) = index.nodes.remove(id) else {
        return;
    };
    for g in &gone[1..] {
        index.nodes.remove(g);
    }
    let rest_id = NodeId::Rest(Box::new(parent.clone()));
    let made = !index.nodes.contains_key(&rest_id);
    let rest = index.nodes.entry(rest_id.clone()).or_insert_with(|| Node {
        measured: Measured::Yes,
        as_of: moved.as_of,
        ..Node::leaf(
            rest_id,
            NodeKind::Rest,
            parent,
            "rest",
            crate::Class::Unread,
            crate::Tally::default(),
        )
    });
    if made {
        rest.classes.clear();
    }
    rest.bytes = rest.bytes.saturating_add(moved.bytes);
    rest.tally.add(&moved.tally);
    for (class, b) in &moved.classes {
        let figure = rest.classes.entry(*class).or_default();
        *figure = figure.saturating_add(*b);
    }
    for (mine, theirs) in rest.growth.iter_mut().zip(moved.growth) {
        *mine = mine.saturating_add(theirs);
    }
    rest.newest_write = rest.newest_write.max(moved.newest_write);
    rest.as_of = rest.as_of.min(moved.as_of);
    if moved.measured == Measured::Carried {
        rest.measured = Measured::Carried;
    }
}

/// Folds every plain folder the fold rule does not admit into its parent's `rest`.
pub fn fold_small(index: &mut Index) {
    let small: Vec<NodeId> = index
        .nodes
        .values()
        .filter(|n| n.kind == NodeKind::Folder && !n.fate.removes())
        .filter(|n| {
            let parent_bytes = n
                .parent
                .as_ref()
                .and_then(|p| index.nodes.get(p))
                .map_or(0, |p| p.bytes);
            !qualifies(n.bytes, parent_bytes)
        })
        .map(|n| n.id.clone())
        .collect();
    for id in small {
        fold_into_rest(index, &id);
    }
}

/// Holds the index to `max` rows: the smallest foldable node goes into its parent's
/// `rest` until the count fits or nothing foldable is left. Ties go by id, so the same
/// rows fold the same way every time.
pub fn bound(index: &mut Index, max: usize) {
    if index.nodes.len() <= max {
        return;
    }
    let mut order: Vec<(u64, NodeId)> = index
        .nodes
        .values()
        .filter(|n| foldable(n.kind) && !n.fate.removes())
        .map(|n| (n.bytes, n.id.clone()))
        .collect();
    order.sort();
    for (_, id) in order {
        if index.nodes.len() <= max {
            break;
        }
        fold_into_rest(index, &id);
    }
}

/// Whether every plain folder that is a node passes the fold rule.
///
/// # Errors
/// The first plain folder that should have folded into its parent's `rest`.
pub fn check_fold(index: &Index) -> Result<(), NodeId> {
    for n in index
        .nodes
        .values()
        .filter(|n| n.kind == NodeKind::Folder && !n.fate.removes())
    {
        let parent_bytes = n
            .parent
            .as_ref()
            .and_then(|p| index.nodes.get(p))
            .map_or(0, |p| p.bytes);
        if !qualifies(n.bytes, parent_bytes) {
            return Err(n.id.clone());
        }
    }
    Ok(())
}

/// Whether the index is within `max` rows, or could fold nothing more.
///
/// # Errors
/// The row count, when it is over `max` while a foldable node is left.
pub fn check_bound(index: &Index, max: usize) -> Result<(), usize> {
    if index.nodes.len() > max
        && index
            .nodes
            .values()
            .any(|n| foldable(n.kind) && !n.fate.removes())
    {
        return Err(index.nodes.len());
    }
    Ok(())
}
