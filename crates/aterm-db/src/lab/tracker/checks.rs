// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab/tracker`'s own rules (docs/DESIGN-lab-tracker-2026-11-14.md §3, §4, §7 findings 7 and
//! 9), on top of the reference, unique and conflict rules every table keeps (`typed::check`).
//!
//! Every reader reports what it finds among the converged rows ([`tracker`]). The first two
//! rules below are also held where they are written, as every table's are: a write this machine
//! authors as the owner's word is refused where it would break one on the writer's own view
//! ([`guard`]). The reader still reports them, because a table is one causal domain and per-cell
//! merge can break a rule no single write broke: sets are add-wins, so two machines that each
//! add one edge to `depends` can close a circle neither saw. The last two report what an import
//! mapped, which is written on purpose and never refused. Nothing repairs a finding silently. A
//! finding names collections and keys, never a value.
//!
//! The rules:
//!
//! - **A row's key is the one its name makes** (`key-mismatch`): [`super::mapped::key_of`].
//! - **No package waits on itself** (`depends-cycle`): the live packages' `depends` hold no
//!   circle.
//! - **A mapped state is said so** (`state-mapped`): a package or decision that holds
//!   `state_said` states a word the lab tracker did not give it.
//! - **A decider in words is said so** (`decided-by-said`): a decision whose `decided_by`
//!   names no session and no program.
//!
//! A package's state is judged apart, from the records that wrote it and not from the cells
//! alone (`state-outranked`, `state-not-allowed`): [`super::state`].
//!
//! One live lane holds a prefix, and one live row an `id`, by each collection's `unique` sets:
//! the table layer refuses and reports those (`unique`).

use super::mapped::{key_of, made_from};
use super::tables::{DECISION, PACKAGE};
use crate::Result;
pub use crate::alab::checks::Finding;
use crate::database::Change;
use crate::typed::{
    Kind, LIFECYCLE, Refused, Table,
    cell::{self, CellKey, CellValue, Cells, Projection},
    check, keys,
};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// What a tracker finding of `kind` means, in a person's words; `None` for any other word
/// ([`check::said`] asks every layer). A sentence names no value, as a finding names none.
pub fn sentence(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "key-mismatch" => "A row's key is not the one its name makes, so its name finds no row",
        "depends-cycle" => {
            "Packages depend on one another in a circle, so none of them can be ready"
        }
        "state-mapped" => {
            "A row states the nearest state to a word the tracker does not have; give it one of \
             the tracker's states and clear the word"
        }
        "decided-by-said" => {
            "A decision names who decided in words, not as a session or program aterm stated"
        }
        _ => return super::state::sentence(kind),
    })
}

/// A tracker finding of `kind`'s heading, a few words in sentence case, for each word
/// [`sentence`] says ([`check::headed`] asks every layer).
pub fn heading(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "key-mismatch" => "Key differs from the name",
        "depends-cycle" => "Packages depend in a circle",
        "state-mapped" => "State mapped from another word",
        "decided-by-said" => "Decider named in words",
        _ => return super::state::heading(kind),
    })
}

/// Every finding over `table`'s cells collections as `cells` hold them: the tracker's own, and
/// the table layer's (`check::findings`: a conflict, a reference to no row, a unique set two
/// live rows share, a value this build cannot read). `table` is a generation of `lab/tracker`.
pub fn tracker(table: &Table, cells: &Cells) -> (Vec<Finding>, Vec<check::Finding>) {
    let projections: BTreeMap<&str, Projection> = table
        .collections
        .iter()
        .filter(|c| c.kind == Kind::Cells)
        .map(|c| (c.name, cell::project(c, cells)))
        .collect();
    let generic = check::findings(
        table.collections,
        &projections.iter().map(|(name, p)| (*name, p)).collect(),
    );
    let mut found = Vec::new();
    for collection in table.collections.iter().filter(|c| c.kind == Kind::Cells) {
        let rows = &projections[collection.name].rows;
        for (pk, row) in rows.iter().filter(|(_, row)| row.live()) {
            // A row that lacks a column its key is made from says no name to compare: its
            // missing cell is the reader's to see, and no key is wrong by it.
            if key_of(collection, |column| row.get(column).cloned()).is_some_and(|made| made != *pk)
            {
                found.push(Finding {
                    kind: "key-mismatch",
                    collection: collection.name,
                    pks: vec![pk.clone()],
                });
            }
            if collection.column("state_said").is_some()
                && row.get("state_said").is_some_and(|said| !said.is_null())
            {
                found.push(Finding {
                    kind: "state-mapped",
                    collection: collection.name,
                    pks: vec![pk.clone()],
                });
            }
        }
    }
    if let Some(decisions) = projections.get(DECISION.name) {
        for (pk, row) in decisions.rows.iter().filter(|(_, row)| row.live()) {
            if row.get("decided_by").is_some_and(said_only) {
                found.push(Finding {
                    kind: "decided-by-said",
                    collection: DECISION.name,
                    pks: vec![pk.clone()],
                });
            }
        }
    }
    if let Some(packages) = projections.get(PACKAGE.name) {
        found.extend(circles(&depends(packages)).into_iter().map(|pks| Finding {
            kind: "depends-cycle",
            collection: PACKAGE.name,
            pks,
        }));
    }
    found.sort_unstable();
    found.dedup();
    (found, generic)
}

/// Whether a principal's value names no session and no program: only what a record said.
fn said_only(principal: &Value) -> bool {
    principal
        .as_object()
        .is_some_and(|object| !object.contains_key("session") && !object.contains_key("program"))
}

/// Each live package with the live packages its `depends` holds (add-wins: a member any
/// candidate adds).
fn depends(packages: &Projection) -> BTreeMap<&str, BTreeSet<&str>> {
    let live = |pk: &str| packages.rows.get(pk).is_some_and(cell::Row::live);
    packages
        .rows
        .iter()
        .filter(|(_, row)| row.live())
        .map(|(pk, row)| {
            let on = row
                .fields
                .get("depends")
                .map(cell::Field::members)
                .unwrap_or_default()
                .into_iter()
                .filter(|dependency| live(dependency))
                .collect();
            (pk.as_str(), on)
        })
        .collect()
}

/// The circles of `graph`: each set of two or more nodes that all reach one another, and each
/// node that names itself, its keys sorted. A node on no circle is in none.
fn circles(graph: &BTreeMap<&str, BTreeSet<&str>>) -> Vec<Vec<String>> {
    // Tarjan's strongly connected components, with its own stack: a chain of dependencies can
    // be as long as the table.
    struct Node<'a> {
        index: usize,
        low: usize,
        on_stack: bool,
        next: std::collections::btree_set::Iter<'a, &'a str>,
    }
    let mut nodes: BTreeMap<&str, Node<'_>> = BTreeMap::new();
    let mut stack: Vec<&str> = Vec::new();
    let mut found = Vec::new();
    let none = BTreeSet::new();
    for &root in graph.keys() {
        if nodes.contains_key(root) {
            continue;
        }
        let mut walk: Vec<&str> = vec![root];
        nodes.insert(
            root,
            Node {
                index: 0,
                low: 0,
                on_stack: true,
                next: graph[root].iter(),
            },
        );
        let count = nodes.len() - 1;
        if let Some(node) = nodes.get_mut(root) {
            node.index = count;
            node.low = count;
        }
        stack.push(root);
        while let Some(&at) = walk.last() {
            let step = nodes.get_mut(at).and_then(|node| node.next.next().copied());
            match step {
                Some(to) => match nodes.get(to).map(|node| (node.index, node.on_stack)) {
                    None => {
                        let index = nodes.len();
                        nodes.insert(
                            to,
                            Node {
                                index,
                                low: index,
                                on_stack: true,
                                next: graph.get(to).unwrap_or(&none).iter(),
                            },
                        );
                        stack.push(to);
                        walk.push(to);
                    }
                    Some((index, true)) => {
                        if let Some(node) = nodes.get_mut(at) {
                            node.low = node.low.min(index);
                        }
                    }
                    Some((_, false)) => {}
                },
                None => {
                    walk.pop();
                    let (index, low) = nodes.get(at).map_or((0, 0), |node| (node.index, node.low));
                    if let Some(&parent) = walk.last()
                        && let Some(node) = nodes.get_mut(parent)
                    {
                        node.low = node.low.min(low);
                    }
                    if low == index {
                        let mut component = Vec::new();
                        while let Some(member) = stack.pop() {
                            if let Some(node) = nodes.get_mut(member) {
                                node.on_stack = false;
                            }
                            component.push(member.to_owned());
                            if member == at {
                                break;
                            }
                        }
                        let itself = graph.get(at).is_some_and(|on| on.contains(at));
                        if component.len() > 1 || itself {
                            component.sort_unstable();
                            found.push(component);
                        }
                    }
                }
            }
        }
    }
    found
}

/// Whether `from` reaches `to` along `graph`'s edges, `from` itself included.
fn reaches(graph: &BTreeMap<&str, BTreeSet<&str>>, from: &str, to: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut walk = vec![from];
    while let Some(at) = walk.pop() {
        if at == to {
            return true;
        }
        if seen.insert(at) {
            walk.extend(graph.get(at).into_iter().flatten().copied());
        }
    }
    false
}

/// `lab/tracker`'s rules on what the owner writes (`att::Guard`), over each row's changes (one
/// record each) and the table's cells as they are under the writer: the first write that would
/// break one of this module's rules on the writer's own view, with the write landed.
///
/// - A row the write gives its name or makes live is keyed by what its name makes
///   ([`key_of`]), and a row it makes live holds every column its key is made from. A row the
///   grid or a shared file adds is born under that key already ([`super::mapped::birth`]); this
///   holds a write that names its own key (an owner verb's, a revert's) to the same.
/// - A member the write adds to a package's `depends` does not close a circle: the package it
///   adds is not one that already depends on this one, directly or through others. A package
///   the write makes live (a birth, a revival, a reverted retirement) adds every dependency it
///   then holds, as the table layer reads a revived row's references.
///
/// What concurrent writes left broken in rows the write leaves alone never holds it back;
/// [`tracker`] reports it. It adds no cell to a record.
///
/// # Errors
/// A change of a cells collection is not a typed cell.
pub fn guard(rows: &mut [Vec<Change>], cells: &Cells) -> Result<Option<Refused>> {
    let table = &super::TRACKER;
    // Each row the write names or makes live, and each member it adds to a package's `depends`.
    let mut named: BTreeSet<(&'static str, String, bool)> = BTreeSet::new();
    let mut added: Vec<(String, String)> = Vec::new();
    for change in rows.iter().flatten() {
        let Some(collection) = table
            .collection(&change.collection)
            .filter(|c| c.kind == Kind::Cells)
        else {
            continue;
        };
        let key = CellKey::parse(&change.key)?;
        // A restored absence (a revert of a birth) asks for no rule.
        let Some(value) = change.value.as_ref().map(CellValue::decode).transpose()? else {
            continue;
        };
        if value.x.is_null() {
            continue;
        }
        let born = key.col == LIFECYCLE && value.x.as_str() == Some("live");
        if born || (key.elem.is_none() && made_from(collection).contains(&key.col)) {
            named.insert((collection.name, key.pk.to_owned(), born));
        }
        if collection.name == PACKAGE.name
            && key.col == "depends"
            && value.x == Value::Bool(true)
            && let Some(dependency) = key.elem
        {
            added.push((key.pk.to_owned(), dependency.to_owned()));
        }
    }
    let mut landed: BTreeMap<&str, Projection> = BTreeMap::new();
    for (name, pk, born) in &named {
        let Some(collection) = table.collection(name) else {
            continue;
        };
        let after = landed
            .entry(collection.name)
            .or_insert_with(|| check::landed(collection, cells, rows));
        let Some(row) = after.rows.get(pk).filter(|row| row.live()) else {
            continue;
        };
        let from = made_from(collection);
        let refuse = |rule: String| Refused {
            collection: collection.name,
            pk: pk.clone(),
            column: from[0].to_owned(),
            rule,
        };
        match key_of(collection, |column| row.get(column).cloned()) {
            Some(made) if made != *pk => {
                return Ok(Some(refuse(format!(
                    "this row's key is not the one its {} makes ({}); add the row under that key",
                    from.join(", "),
                    keys::safe_key(&made)
                ))));
            }
            None if *born => {
                return Ok(Some(refuse(format!(
                    "a row is keyed by its {}; a new row states what its key needs",
                    from.join(", ")
                ))));
            }
            _ => {}
        }
    }
    // A package the write makes live adds every dependency it holds once the write lands; one
    // live before only restates its lifecycle and adds none.
    let revived: Vec<&str> = named
        .iter()
        .filter(|(name, _, born)| *born && *name == PACKAGE.name)
        .map(|(_, pk, _)| pk.as_str())
        .collect();
    if !added.is_empty() || !revived.is_empty() {
        let before = cell::project(&PACKAGE, cells);
        let packages = landed
            .entry(PACKAGE.name)
            .or_insert_with(|| check::landed(&PACKAGE, cells, rows));
        let graph = depends(packages);
        for pk in revived {
            if before.rows.get(pk).is_some_and(cell::Row::live) {
                continue;
            }
            for dependency in graph.get(pk).into_iter().flatten() {
                added.push((pk.to_owned(), (*dependency).to_owned()));
            }
        }
        for (package, dependency) in &added {
            // An edge to a package the table does not hold, or one that is not live, is the
            // table layer's to refuse.
            if graph.contains_key(package.as_str())
                && graph.contains_key(dependency.as_str())
                && reaches(&graph, dependency, package)
            {
                return Ok(Some(Refused {
                    collection: PACKAGE.name,
                    pk: package.clone(),
                    column: "depends".to_owned(),
                    rule: if package == dependency {
                        "a package does not depend on itself".to_owned()
                    } else {
                        format!(
                            "package {} already depends on this one, directly or through \
                             others; the two cannot each wait for the other",
                            keys::safe_key(dependency)
                        )
                    },
                }));
            }
        }
    }
    Ok(None)
}
