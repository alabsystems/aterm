// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The four laws, checked on every reply (docs/DESIGN-disk-map-2026-11-18.md §3.2):
//!
//! 1. A parent equals its children plus its `rest`.
//! 2. The regions other than Free, plus `unreached`, equal used.
//! 3. A node's class figures equal its bytes.
//! 4. Protection wins: a person's text, documents and media, then a checkout's tracked
//!    and untracked files (a main checkout's always; a linked worktree's until they are
//!    pinned), then a git store, each outrank any proof that something is temporary.

use std::collections::BTreeSet;

use crate::{Class, Fate, Index, Node, NodeId, NodeKind, Part, Region};

/// A law a set of rows breaks, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Break {
    /// Which law, 1 to 4.
    pub law: u8,
    /// The node it breaks at, when one does.
    pub node: Option<NodeId>,
    /// What was found.
    pub detail: String,
}

impl std::fmt::Display for Break {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.node {
            Some(id) => write!(f, "law {} at {id}: {}", self.law, self.detail),
            None => write!(f, "law {}: {}", self.law, self.detail),
        }
    }
}

fn broken(law: u8, node: Option<&NodeId>, detail: String) -> Break {
    Break {
        law,
        node: node.cloned(),
        detail,
    }
}

/// Law 1: every parent's bytes are its children's sum (its `rest` is one of them), and
/// every parent a row names is a row. A node with no children is a leaf and is its own
/// figure.
///
/// # Errors
/// The first parent whose children do not sum to it, or the first row whose parent is
/// missing.
pub fn parent_equals_children_plus_rest(index: &Index) -> Result<(), Break> {
    for node in index.nodes.values() {
        shape(index, node)?;
    }
    for (parent, kids) in index.children() {
        let sum = kids
            .iter()
            .filter_map(|k| index.nodes.get(*k))
            .try_fold(0u64, |s, n| s.checked_add(n.bytes));
        let bytes = index.nodes.get(parent).map_or(0, |n| n.bytes);
        if sum != Some(bytes) {
            let sum = sum.map_or_else(|| "overflow u64".to_owned(), |s| s.to_string());
            return Err(broken(
                1,
                Some(parent),
                format!("{bytes} bytes, its children {sum}"),
            ));
        }
    }
    Ok(())
}

/// Whether a row's place in the tree is one its id allows: a region and only a region
/// has no parent; a row's kind is the one its id names; a part sits under its own
/// checkout and a `rest` under its own parent; every chain of parents ends at a region.
fn shape(index: &Index, node: &Node) -> Result<(), Break> {
    let bad = |detail: String| Err(broken(1, Some(&node.id), detail));
    let kind_fits = match (&node.id, node.kind) {
        (NodeId::Region(_), NodeKind::Region)
        | (NodeId::Repository(_), NodeKind::Repository)
        | (NodeId::Part(..), NodeKind::Part)
        | (NodeId::Rest(_), NodeKind::Rest) => true,
        (NodeId::Dir(_), kind) => !matches!(
            kind,
            NodeKind::Region | NodeKind::Repository | NodeKind::Part | NodeKind::Rest
        ),
        _ => false,
    };
    if !kind_fits {
        return bad(format!("its kind {:?} is not one its id names", node.kind));
    }
    let Some(parent) = &node.parent else {
        return if matches!(node.id, NodeId::Region(_)) {
            Ok(())
        } else {
            bad("only a region has no parent".into())
        };
    };
    if matches!(node.id, NodeId::Region(_)) {
        return bad(format!("a region has no parent, and it names {parent}"));
    }
    let Some(above) = index.nodes.get(parent) else {
        return bad(format!("its parent {parent} is not a row"));
    };
    match &node.id {
        NodeId::Part(key, _)
            if *parent != NodeId::Dir(*key) || !matches!(above.kind, NodeKind::Checkout { .. }) =>
        {
            return bad(format!("a part sits under its own checkout, not {parent}"));
        }
        NodeId::Rest(of) if **of != *parent => {
            return bad(format!("a rest row sits under {of}, not {parent}"));
        }
        _ => {}
    }
    let chain = index.ancestors(&node.id);
    if chain.last().copied().unwrap_or(node).parent.is_some() {
        return bad("its parents loop and reach no region".into());
    }
    Ok(())
}

/// Law 2: the regions other than Free, plus `unreached`, equal used.
///
/// # Errors
/// When they do not.
pub fn regions_plus_unreached_equal_used(index: &Index) -> Result<(), Break> {
    let sum = index
        .nodes
        .values()
        .filter(|n| matches!(n.id, NodeId::Region(r) if r != Region::Free))
        .try_fold(index.unreached, |s, n| s.checked_add(n.bytes));
    if sum == Some(index.used) {
        Ok(())
    } else {
        let sum = sum.map_or_else(|| "overflow u64".to_owned(), |s| s.to_string());
        Err(broken(
            2,
            None,
            format!(
                "regions + unreached {} = {sum} != used {}",
                index.unreached, index.used
            ),
        ))
    }
}

/// Law 3: a node's class figures equal its bytes, and so do its bytes by kind and age.
/// Free space is no one's bytes and has no class.
///
/// # Errors
/// The first node whose class figures or tally differ from its bytes.
pub fn class_figures_equal_bytes(index: &Index) -> Result<(), Break> {
    for node in index
        .nodes
        .values()
        .filter(|n| n.id != NodeId::Region(Region::Free))
    {
        let classes = node
            .classes
            .values()
            .try_fold(0u64, |s, b| s.checked_add(*b));
        if classes != Some(node.bytes) {
            return Err(broken(
                3,
                Some(&node.id),
                format!("class figures {classes:?}, bytes {}", node.bytes),
            ));
        }
        let by_kind = node.tally.checked_total();
        if by_kind != Some(node.bytes) {
            return Err(broken(
                3,
                Some(&node.id),
                format!("bytes by kind {by_kind:?}, bytes {}", node.bytes),
            ));
        }
    }
    Ok(())
}

/// What protects a node, highest rank first (law 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Protection {
    /// Bytes nobody could class: never candidates.
    Unproven,
    /// A git store.
    GitStore,
    /// A checkout's tracked and untracked files.
    CheckoutFiles,
    /// A person's text, documents and media outside a temp root.
    Personal,
}

/// Whether `node` sits in a temp root: the tmp region or a temp row at or above it.
/// The row's own flag is believed only where its place agrees.
fn in_temp_root(index: &Index, node: &Node) -> bool {
    node.under_temp_root
        && std::iter::once(node)
            .chain(index.ancestors(&node.id))
            .any(|n| n.id == NodeId::Region(Region::Tmp) || n.kind == NodeKind::Temp)
}

/// What protects `node` from going by the hand of `by` (the node whose fate removes
/// it), if anything. Rule 3's temp exception is the one case decided the other way: a
/// person's files under a temp root are temporary because of where they were put.
/// Everything at or under a checkout's files part, or a git store, is protected as the
/// part or store is; a repository's store is what sits under it outside its checkouts.
#[must_use]
pub fn protection(index: &Index, node: &Node, by: &Fate) -> Option<Protection> {
    let personal =
        node.tally.personal() > 0 || node.classes.get(&Class::Personal).is_some_and(|b| *b > 0);
    if personal && !in_temp_root(index, node) {
        return Some(Protection::Personal);
    }
    let chain: Vec<&Node> = std::iter::once(node)
        .chain(index.ancestors(&node.id))
        .collect();
    if let NodeKind::Checkout { linked: false, .. } = node.kind {
        return Some(Protection::CheckoutFiles);
    }
    for n in &chain {
        let NodeId::Part(key, part) = &n.id else {
            continue;
        };
        if !matches!(
            part,
            Part::Pushed | Part::Unpushed | Part::Dirty | Part::Untracked
        ) {
            continue;
        }
        if let Some(NodeKind::Checkout { linked, pinned }) =
            index.nodes.get(&NodeId::Dir(*key)).map(|c| c.kind)
        {
            if !linked || (!pinned && *by != Fate::Pin) {
                return Some(Protection::CheckoutFiles);
            }
        }
    }
    let store = chain
        .iter()
        .take_while(|n| !matches!(n.kind, NodeKind::Checkout { .. }))
        .any(|n| {
            matches!(n.id, NodeId::Repository(_) | NodeId::Part(_, Part::Store))
                || n.kind == NodeKind::Repository
        });
    if store {
        return Some(Protection::GitStore);
    }
    if node.classes.get(&Class::Unread).is_some_and(|b| *b > 0)
        || node
            .classes
            .get(&Class::Unexplained)
            .is_some_and(|b| *b > 0)
    {
        return Some(Protection::Unproven);
    }
    None
}

/// Every node some fate removes, with the fate that removes it: `trim`, `remove` and
/// `pin` take the node and everything under it; `strip` takes only the build output
/// under it.
#[must_use]
pub fn going(index: &Index) -> Vec<(&Node, &Fate)> {
    let kids = index.children();
    let mut out: Vec<(&Node, &Fate)> = Vec::new();
    for node in index.nodes.values().filter(|n| n.fate.removes()) {
        let mut stack: Vec<(&NodeId, bool)> = if node.fate == Fate::Strip {
            kids.get(&node.id)
                .map(|k| k.iter().map(|id| (*id, false)).collect())
                .unwrap_or_default()
        } else {
            vec![(&node.id, true)]
        };
        let mut seen: BTreeSet<&NodeId> = BTreeSet::new();
        while let Some((id, taken)) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(n) = index.nodes.get(id) else {
                continue;
            };
            let taken = taken || n.class() == Class::Build && n.classes.len() == 1;
            if taken {
                out.push((n, &node.fate));
            }
            if let Some(k) = kids.get(id) {
                stack.extend(k.iter().map(|c| (*c, taken)));
            }
        }
    }
    out
}

/// Law 4: nothing a fate removes is protected, whatever proof it carries.
///
/// # Errors
/// The first going node something protects, naming the highest protection.
pub fn protection_wins(index: &Index) -> Result<(), Break> {
    for (node, by) in going(index) {
        if let Some(p) = protection(index, node, by) {
            return Err(broken(
                4,
                Some(&node.id),
                format!(
                    "{p:?} outranks fate {} and proofs {:?}",
                    by.word(),
                    node.evidence.proofs
                ),
            ));
        }
    }
    Ok(())
}

/// All four laws, in order.
///
/// # Errors
/// The first law broken.
pub fn check(index: &Index) -> Result<(), Break> {
    parent_equals_children_plus_rest(index)?;
    regions_plus_unreached_equal_used(index)?;
    class_figures_equal_bytes(index)?;
    protection_wins(index)
}
