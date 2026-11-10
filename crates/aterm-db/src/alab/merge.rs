// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab merge`: fold each duplicate machine row into the one row its Mac is bound to,
//! with the registry's own `merged_into`. Rows that share a name, as a label or an alias, are one
//! Mac when exactly one of them holds a node cell: that row stays, and the others end up
//! `row=merged` pointing at it. So are the row holding the node the roster states of a name and the
//! row with no node answering that name: an import gave the roster's name a row before its Mac
//! joined, and the Mac registered a row of its own under the name it went by.
//!
//! The row that stays takes the Mac's current name as its label: the roster's name among the
//! rows' names, since the roster is what a Mac's membership goes by; with none, its own label
//! stays, and every other name is kept as an alias. A cell only a duplicate holds moves with its
//! provenance, an owner's cell moves over an import's, and a cell both hold differently stays as
//! the node-bound row has it, its column listed. What the owner cleared or removed on the row that
//! stays stands, and so does a cell of a newer build's it cannot read. The row answers every host
//! its duplicates answered, so a Mac that would have adopted a duplicate by host adopts it, and it
//! stays `planned` where a duplicate waited for a Mac by name, the one way `register::pick` adopts by
//! name. A duplicate's live preference moves to the row that stays and ends where it was, and so
//! does one a Mac that had not yet heard of a merge wrote on a row since merged, unless a Mac's facts
//! name that row or one its `merged_into` passes, which only that Mac moves off (lab model §6): a
//! Mac whose facts show it rebound along that merge has.
//!
//! A row an old import called `active` that no Mac has joined goes back to `planned`, the one word
//! `register::pick` adopts a Mac's label by: fleet_db wrote that `active` for every row, and left
//! standing, the row's Mac would birth a row of its own under another label on first registration.
//! Only an import's word on a row with no node, no host and no Mac's facts moves; the owner's stays,
//! and a row the roster gives another Mac, or two Macs' names, is held. The `active` a Mac's joining
//! brings back comes from the roster, once the Mac published its principal.
//!
//! Anything else is held and listed: two nodes, or a node the roster states of the name against
//! the one the row holds, or of another name against it, are two Macs; no node gives no row to keep;
//! a duplicate a Mac's own facts row is bound to moves only by that Mac; two rows stating different
//! words of `status` are not plainly one Mac; a cell the merge decides by that this build cannot read
//! is the newer build's to decide; and a conflicted cell is settled first. [`plan`] is pure over what
//! the registry holds, and a plan over its own result is empty. A report holds keys, labels and
//! column names, never a cell's value.

use super::{
    register::same_host,
    registry::{MACHINE, PREFERENCE},
};
use crate::{
    Result,
    database::Change,
    typed::{
        LIFECYCLE,
        cell::{self, CellKey, CellValue, Field, Projection, Row},
        keys,
    },
};
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The machine columns a merge writes by its own rules, or that say which row this is.
const NOT_CARRIED: [&str; 7] = [
    "label",
    "aliases",
    "hosts",
    "node",
    "fleet_node",
    "merged_into",
    LIFECYCLE,
];
/// The hosts a row can hold (`hosts`' column).
const MAX_HOSTS: usize = 4;
/// What binds a row to one Mac: the principal it publishes itself, and the older transport id a
/// replica states. A row holding either is that Mac's; two rows holding them are not one to fold.
const NODE_CELLS: [&str; 2] = ["node", "fleet_node"];
/// The preference columns that say which row this is.
const PREFERENCE_IDENTITY: [&str; 3] = ["machine", "account", LIFECYCLE];
/// Provenance moves with a value: another row's coordinates are not a difference worth listing.
const PROVENANCE: [&str; 2] = ["src", "notes_src"];
/// The changes one record may hold (`att::pack`), for a row's carried cells.
const RECORD: usize = 64;
/// The machine columns that say whether a row waits for its Mac by name.
const WAIT_COLUMNS: [&str; 4] = ["label", "status", "node", "hosts"];

/// The roster's labels, each with the node cells it states of that Mac by column.
pub type Roster = BTreeMap<String, BTreeMap<&'static str, String>>;

/// What a run would do.
#[derive(Debug, Default)]
pub struct Plan {
    pub merges: Vec<Merge>,
    pub held: Vec<Held>,
    /// Live preferences found on a row already merged, moved along its `merged_into`.
    pub strays: Vec<Moved>,
    /// The rows set to wait for their Mac.
    pub waiting: Vec<Waiting>,
    /// Each entry stays whole in one record: a preference with its move, then the machine rows.
    pub rows: Vec<Vec<Change>>,
}

/// A row no Mac has joined, set back to `planned` so that its Mac adopts it by name.
#[derive(Debug)]
pub struct Waiting {
    pub row: String,
    /// The label it holds once this plan ran.
    pub label: String,
}

/// One Mac's rows folded into the one that stays.
#[derive(Debug, Default)]
pub struct Merge {
    pub keep: String,
    pub gone: Vec<String>,
    pub label_from: Option<String>,
    pub label_to: String,
    pub aliases_added: Vec<String>,
    pub aliases_removed: Vec<String>,
    /// `(duplicate, column)` a cell moved from, a set's element as `column.element`.
    pub carried: Vec<(String, String)>,
    /// `(duplicate, column)` both rows hold differently: the node-bound row's stays.
    pub kept: Vec<(String, String)>,
    pub preferences: Vec<Moved>,
}

/// One preference of a duplicate.
#[derive(Debug)]
pub struct Moved {
    pub from: String,
    pub to: String,
    pub how: How,
    /// The columns it gave the row it moved to, by name.
    pub columns: Vec<String>,
    /// The columns the row it moved to holds differently and keeps, by name.
    pub kept: Vec<String>,
}

/// What a duplicate's preference became.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// The row that stays had none: it has one now.
    Moved,
    /// The row that stays had one: it took what it lacked.
    Filled,
    /// An import had ended the row that stays' preference: it is live again, as the duplicate's was.
    Revived,
    /// The owner ended the row that stays' preference: it stays ended.
    OwnerEnded,
}

impl How {
    pub const fn word(self) -> &'static str {
        match self {
            Self::Moved => "moved",
            Self::Filled => "filled",
            Self::Revived => "revived",
            Self::OwnerEnded => "owner-ended",
        }
    }
}

/// Rows that share a name and are not merged, and why.
#[derive(Debug)]
pub struct Held {
    pub rows: Vec<String>,
    pub names: Vec<String>,
    pub why: &'static str,
}

/// Every name a row goes by, each candidate of a conflicted label among them. An alias is a key, so
/// never an address; a label is a value, so another writer may have stored one there, and a report
/// never prints what this build would not take for a label.
fn names_of(row: &Row) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = row
        .fields
        .get("aliases")
        .map(|field| field.members().iter().map(|m| (*m).to_owned()).collect())
        .unwrap_or_default();
    if let Some(field) = row.fields.get("label") {
        names.extend(
            field
                .candidates_x()
                .into_iter()
                .filter(|x| {
                    MACHINE
                        .column("label")
                        .is_some_and(|column| cell::admissible(&column.ty, x))
                })
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    names
}

fn label_of(row: &Row) -> Option<&str> {
    row.get("label").and_then(Value::as_str)
}

fn status_of(row: &Row) -> Option<&str> {
    value(row.fields.get("status")).and_then(Value::as_str)
}

/// What the Macs' own facts rows say of the machine rows: each one's `mac`, the row that Mac is, and
/// `merged_into`, the row it rebound to after that row was merged, either unset. Only a Mac moves
/// its own row (lab model §6).
#[derive(Clone, Debug, Default)]
pub struct Facts {
    rows: Vec<(Option<String>, Option<String>)>,
}

impl Facts {
    /// The facts rows, as `(mac, merged_into)`.
    #[must_use]
    pub fn new(rows: Vec<(Option<String>, Option<String>)>) -> Self {
        Self { rows }
    }

    /// Whether a facts row names `pk`, as the row its Mac is or the one it rebound to.
    fn binds(&self, pk: &str) -> bool {
        self.rows.iter().any(|(mac, into)| {
            [mac, into]
                .into_iter()
                .any(|row| row.as_deref() == Some(pk))
        })
    }

    /// Whether every facts row naming `from` rebound to one of `onward`, the rows a merge chain goes
    /// on to after `from`: its Mac has moved off `from` along that chain. A Mac that rebound onto
    /// `from` (never in `onward`, which a chain passes once), or that went elsewhere, has not.
    fn rebound(&self, from: &str, onward: &[&str]) -> bool {
        self.rows
            .iter()
            .filter(|(mac, into)| {
                [mac, into]
                    .into_iter()
                    .any(|row| row.as_deref() == Some(from))
            })
            .all(|(_, into)| into.as_deref().is_some_and(|to| onward.contains(&to)))
    }
}

/// Whether no Mac is bound to `keep`: it has published no principal and no facts row names it, so its
/// Mac has yet to adopt it.
fn unbound(keep: &str, keeper: &Row, facts: &Facts) -> bool {
    !facts.binds(keep) && value(keeper.fields.get("node")).is_none()
}

/// The live rows that share a name with each other, transitively: each group of two or more.
fn shared_names(machines: &Projection) -> Vec<BTreeSet<&str>> {
    let mut by_name: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut names_by_row: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for (pk, row) in machines.rows.iter().filter(|(_, row)| row.live()) {
        let names = names_of(row);
        for name in &names {
            by_name.entry(name.clone()).or_default().push(pk);
        }
        names_by_row.insert(pk, names);
    }
    let mut seen = BTreeSet::new();
    let mut groups = Vec::new();
    for &start in names_by_row.keys() {
        if !seen.insert(start) {
            continue;
        }
        let mut group = BTreeSet::from([start]);
        let mut todo = vec![start];
        while let Some(pk) = todo.pop() {
            for name in &names_by_row[pk] {
                for &other in &by_name[name] {
                    if seen.insert(other) {
                        group.insert(other);
                        todo.push(other);
                    }
                }
            }
        }
        if group.len() > 1 {
            groups.push(group);
        }
    }
    groups
}

/// The rows to judge as one Mac's: those sharing a name, joined by each pair the roster ties (see
/// [`roster_ties`]), in key order of their first row.
fn groups<'a>(machines: &'a Projection, roster: &Roster) -> Vec<BTreeSet<&'a str>> {
    let mut groups = shared_names(machines);
    for tie in roster_ties(machines, roster) {
        let (joined, mut apart): (Vec<_>, Vec<_>) = groups
            .into_iter()
            .partition(|group| !group.is_disjoint(&tie));
        apart.push(joined.into_iter().fold(tie, |mut all, group| {
            all.extend(group);
            all
        }));
        groups = apart;
    }
    groups.sort();
    groups
}

/// The pairs of live rows the roster says are one Mac though they share no name: the one row
/// holding a node cell the roster states of a label, and the one row answering that label, which
/// holds no node cell of its own. That is how a Mac the roster names otherwise than it named itself
/// keeps two rows: its own, bound to its node under the label it registered with, and the one an
/// import gave the roster's name before the Mac joined, which `register::pick`, holding no node or
/// host of it, could adopt only by that name.
fn roster_ties<'a>(machines: &'a Projection, roster: &Roster) -> Vec<BTreeSet<&'a str>> {
    let live: Vec<(&str, &Row)> = machines
        .rows
        .iter()
        .filter(|(_, row)| row.live())
        .map(|(pk, row)| (pk.as_str(), row))
        .collect();
    let holds_node = |row: &Row, column: &str, node: &str| {
        value(row.fields.get(column)).and_then(Value::as_str) == Some(node)
    };
    let mut ties = Vec::new();
    for (label, stated) in roster {
        let bound: Vec<&str> = live
            .iter()
            .filter(|(_, row)| {
                stated
                    .iter()
                    .any(|(column, node)| holds_node(row, column, node))
            })
            .map(|(pk, _)| *pk)
            .collect();
        let named: Vec<(&str, &Row)> = live
            .iter()
            .copied()
            .filter(|(_, row)| names_of(row).contains(label))
            .collect();
        if let ([keep], [(other, row)]) = (bound.as_slice(), named.as_slice())
            && keep != other
            && NODE_CELLS
                .iter()
                .all(|column| !row.fields.contains_key(*column))
        {
            ties.push(BTreeSet::from([*keep, *other]));
        }
    }
    ties
}

/// Whether a row holds different concurrent values anywhere, a set's elements included.
fn conflicted(row: &Row) -> bool {
    row.fields.values().any(|field| match field {
        Field::Set(elements) => elements
            .values()
            .any(|element| matches!(element, Field::Conflict { .. })),
        Field::Conflict { .. } => true,
        Field::Value(_) => false,
    })
}

/// A register's value, unless it is cleared.
fn value(field: Option<&Field>) -> Option<&Value> {
    match field? {
        Field::Value(candidate) if !candidate.value.x.is_null() => Some(&candidate.value.x),
        _ => None,
    }
}

/// Whether the row that stays lacks what `candidate` says of a cell it holds as `there`: it has
/// none, an import cleared it, or an owner's word meets an import's. What the owner cleared stands.
fn lacks(candidate: &CellValue, there: Option<&Field>) -> bool {
    match there {
        None => true,
        Some(Field::Value(held)) if held.value.x.is_null() => held.value.s.is_some(),
        Some(Field::Value(held)) => held.value.s.is_some() && candidate.s.is_none() && !candidate.a,
        Some(Field::Set(_) | Field::Conflict { .. }) => false,
    }
}

/// The same value and provenance under this schema version, written by the owner's verb.
fn carried(field: &CellValue, version: u32) -> CellValue {
    CellValue {
        r: version,
        x: field.x.clone(),
        s: field.s.clone(),
        a: false,
    }
}

/// Plan the merges of `machines`, moving `preferences`. `facts` holds what each Mac's own facts row
/// names, which only that Mac rebinds; `roster` what the roster says of each label.
///
/// # Errors
/// A value carried breaks its column's type, which is a bug: nothing is written then.
pub fn plan(
    machines: &Projection,
    preferences: &Projection,
    facts: &Facts,
    roster: &Roster,
) -> Result<Plan> {
    let mut plan = Plan::default();
    for group in groups(machines, roster) {
        let rows: Vec<(&str, &Row)> = group.iter().map(|&pk| (pk, &machines.rows[pk])).collect();
        let names: BTreeSet<String> = rows.iter().flat_map(|(_, row)| names_of(row)).collect();
        match choose(&rows, preferences, facts, &names, roster) {
            Err(why) => plan.held.push(Held {
                rows: rows.iter().map(|(pk, _)| (*pk).to_owned()).collect(),
                names: names.into_iter().collect(),
                why,
            }),
            Ok((keep, label)) => {
                let waits = unbound(keep, &machines.rows[keep], facts);
                merge_group(&mut plan, &rows, keep, label, &names, preferences, waits)?;
            }
        }
    }
    move_strays(&mut plan, machines, preferences, facts)?;
    wait_for_macs(&mut plan, machines, facts, roster)?;
    Ok(plan)
}

/// The live row a merged row's `merged_into` ends at, following this plan's merges too, or why it
/// cannot be told: a cell it is decided by that this build cannot read, a dispute over one, a merged
/// row on the way that a Mac's facts name and that Mac has not rebound off along the chain (only
/// that Mac moves off it), or a chain that reaches no live row.
fn merge_target(
    plan: &Plan,
    machines: &Projection,
    facts: &Facts,
    merged: &str,
) -> std::result::Result<String, &'static str> {
    // The merged rows passed, in order, then the live row reached and the one a merge of this plan
    // keeps for it.
    let mut path: Vec<&str> = Vec::new();
    let mut at = merged;
    while !path.contains(&at) {
        let row = machines.rows.get(at).ok_or("no-merge-target")?;
        if [LIFECYCLE, "merged_into"]
            .iter()
            .any(|column| row.raw.contains(*column))
        {
            return Err("unreadable");
        }
        if row.live() {
            let keep = plan
                .merges
                .iter()
                .find(|merge| merge.gone.iter().any(|gone| gone == at))
                .map_or(at, |merge| merge.keep.as_str());
            path.extend([at, keep]);
            let passed = path.len() - 2;
            if (0..passed).any(|n| facts.binds(path[n]) && !facts.rebound(path[n], &path[n + 1..]))
            {
                return Err("bound-by-its-mac");
            }
            return Ok(keep.to_owned());
        }
        if [LIFECYCLE, "merged_into"]
            .iter()
            .any(|column| matches!(row.fields.get(*column), Some(Field::Conflict { .. })))
        {
            return Err("conflicted");
        }
        if row.lifecycle() != Some("merged") {
            return Err("no-merge-target");
        }
        path.push(at);
        at = value(row.fields.get("merged_into"))
            .and_then(Value::as_str)
            .ok_or("no-merge-target")?;
    }
    Err("no-merge-target")
}

/// Move each live preference found on a row already merged. A Mac that has not yet heard of a merge
/// writes one there, and preferences on such a row would count for nothing. What `choose` holds a
/// merge for holds the move: a row a Mac's facts name, the stray's or one its `merged_into` passes,
/// a preference or the one it would move onto that this build cannot read or that is in dispute. So
/// does a `merged_into` that cannot be followed.
fn move_strays(
    plan: &mut Plan,
    machines: &Projection,
    preferences: &Projection,
    facts: &Facts,
) -> Result<()> {
    for (pk, row) in &machines.rows {
        let merged = row.lifecycle() == Some("merged");
        let on_row = |row: &&Row| row.get("machine").and_then(Value::as_str) == Some(pk.as_str());
        let stray: Vec<&Row> = preferences.rows.values().filter(on_row).collect();
        if !merged || !stray.iter().any(|row| row.live()) {
            continue;
        }
        let why = match merge_target(plan, machines, facts, pk) {
            Err(why) => Some(why),
            Ok(keep)
                if stray.iter().any(|row| {
                    unreadable_preference(row)
                        || target_of(row, &keep, preferences).is_some_and(unreadable_preference)
                }) =>
            {
                Some("unreadable")
            }
            Ok(keep)
                if stray.iter().any(|row| {
                    unsettled_preference(row)
                        || target_of(row, &keep, preferences).is_some_and(conflicted)
                }) =>
            {
                Some("conflicted")
            }
            Ok(keep) => {
                let moved = match plan.merges.iter_mut().find(|merge| merge.keep == keep) {
                    Some(merge) => &mut merge.preferences,
                    None => &mut plan.strays,
                };
                move_preferences(&mut plan.rows, moved, pk, &keep, preferences)?;
                None
            }
        };
        if let Some(why) = why {
            plan.held.push(Held {
                rows: vec![pk.clone()],
                names: Vec::new(),
                why,
            });
        }
    }
    Ok(())
}

/// Set each row an import called `active` that no Mac has joined back to `planned`, with the
/// provenance it has, so a later import can still correct it. A row a merge just decided, or holds,
/// is left to that: the `status` or the host it took says what the row waits for already. A row the
/// roster gives another Mac, or two names of the roster, is held: either Mac would adopt it.
fn wait_for_macs(
    plan: &mut Plan,
    machines: &Projection,
    facts: &Facts,
    roster: &Roster,
) -> Result<()> {
    let carried = |merge: &&Merge| {
        merge
            .carried
            .iter()
            .any(|(_, column)| column == "status" || column == "hosts")
    };
    let decided: BTreeSet<&String> = plan
        .held
        .iter()
        .flat_map(|held| &held.rows)
        .chain(plan.merges.iter().flat_map(|merge| &merge.gone))
        .chain(plan.merges.iter().filter(carried).map(|merge| &merge.keep))
        .collect();
    let mut waiting = Vec::new();
    let mut held = Vec::new();
    for (pk, row) in machines.rows.iter().filter(|(_, row)| row.live()) {
        let Some(Field::Value(word)) = row.fields.get("status") else {
            continue;
        };
        if decided.contains(pk)
            || word.value.x != json!("active")
            || word.value.s.is_none()
            || !unbound(pk, row, facts)
            || conflicted(row)
            || WAIT_COLUMNS.iter().any(|column| row.raw.contains(*column))
            || row
                .get("hosts")
                .and_then(Value::as_array)
                .is_some_and(|hosts| !hosts.is_empty())
        {
            continue;
        }
        let merge = plan.merges.iter().find(|merge| merge.keep == *pk);
        let Some(label) = merge
            .map(|merge| merge.label_to.as_str())
            .or_else(|| label_of(row))
        else {
            continue;
        };
        let names = names_of(row);
        let mut on_roster = names.iter().filter(|name| roster.contains_key(*name));
        let why = match (on_roster.next(), on_roster.next()) {
            (Some(_), Some(_)) => Some("two-roster-names"),
            (Some(name), None) if other_mac(row, name, roster) => Some("two-macs"),
            _ => None,
        };
        if let Some(why) = why {
            held.push(Held {
                rows: vec![pk.clone()],
                names: names.into_iter().collect(),
                why,
            });
            continue;
        }
        waiting.push(Waiting {
            row: pk.clone(),
            label: label.to_owned(),
        });
        plan.rows.push(vec![cell::change(
            &MACHINE,
            CellKey {
                pk,
                col: "status",
                elem: None,
            },
            &CellValue {
                r: MACHINE.version,
                x: json!("planned"),
                s: word.value.s.clone(),
                a: false,
            },
        )?]);
    }
    plan.waiting = waiting;
    plan.held.extend(held);
    Ok(())
}

/// Whether the roster says `row` is not the Mac it calls `name`: it states another node of `name`,
/// or a node `row` holds of another label.
fn other_mac(row: &Row, name: &str, roster: &Roster) -> bool {
    let holds = |column: &str, node: &str| {
        value(row.fields.get(column)).map(|held| held.as_str() == Some(node))
    };
    roster.get(name).is_some_and(|stated| {
        stated
            .iter()
            .any(|(column, node)| holds(column, node) == Some(false))
    }) || roster.iter().any(|(label, stated)| {
        label != name
            && stated
                .iter()
                .any(|(column, node)| holds(column, node) == Some(true))
    })
}

/// Whether what says which preference this is may be a newer build's.
fn unreadable_preference(row: &Row) -> bool {
    PREFERENCE_IDENTITY
        .iter()
        .any(|column| row.raw.contains(*column))
}

/// Whether a preference's end is in dispute, so it may be live, or a live one holds a dispute: it is
/// settled before it is left behind.
fn unsettled_preference(row: &Row) -> bool {
    matches!(row.fields.get(LIFECYCLE), Some(Field::Conflict { .. }))
        || (row.live() && conflicted(row))
}

/// The preference this live one would move onto, on `keep`.
fn target_of<'a>(row: &Row, keep: &str, preferences: &'a Projection) -> Option<&'a Row> {
    if !row.live() {
        return None;
    }
    let account = row.get("account").and_then(Value::as_str)?;
    let target = keys::preference_pk(keep, account).ok()?;
    preferences.rows.get(&target)
}

/// The row that stays and the label it takes, or why these rows are not merged.
fn choose<'a>(
    rows: &[(&'a str, &Row)],
    preferences: &Projection,
    facts: &Facts,
    names: &BTreeSet<String>,
    roster: &Roster,
) -> std::result::Result<(&'a str, String), &'static str> {
    let on_group = |row: &Row| {
        rows.iter()
            .any(|(pk, _)| row.get("machine").and_then(Value::as_str) == Some(*pk))
    };
    // What says which preference this is, or whether a row is this Mac's, may be a newer build's.
    if rows.iter().any(|(_, row)| {
        NOT_CARRIED
            .iter()
            .chain(&["status"])
            .any(|column| row.raw.contains(*column))
    }) || preferences
        .rows
        .values()
        .any(|row| on_group(row) && unreadable_preference(row))
    {
        return Err("unreadable");
    }
    if rows.iter().any(|(_, row)| conflicted(row))
        || preferences
            .rows
            .values()
            .any(|row| on_group(row) && unsettled_preference(row))
    {
        return Err("conflicted");
    }
    let nodes: Vec<(&'a str, Vec<&Value>)> = rows
        .iter()
        .filter_map(|(pk, row)| {
            let held: Vec<&Value> = NODE_CELLS
                .iter()
                .filter_map(|column| value(row.fields.get(*column)))
                .collect();
            (!held.is_empty()).then_some((*pk, held))
        })
        .collect();
    let keep = match nodes.as_slice() {
        [] => return Err("no-node"),
        [(keep, _)] => *keep,
        [(_, first), rest @ ..]
            if rest
                .iter()
                .all(|(_, held)| held.iter().any(|node| first.contains(node))) =>
        {
            return Err("one-node-twice");
        }
        _ => return Err("two-macs"),
    };
    if rows.iter().any(|(pk, _)| *pk != keep && facts.binds(pk)) {
        return Err("bound-by-its-mac");
    }
    let keeper = rows
        .iter()
        .find(|(pk, _)| *pk == keep)
        .map(|(_, row)| *row)
        .expect("the row that stays is one of the rows");
    let statuses: BTreeSet<&str> = rows
        .iter()
        .filter_map(|(_, row)| value(row.fields.get("status"))?.as_str())
        .filter(|status| *status != "planned")
        .collect();
    if statuses.len() > 1 {
        return Err("status-differs");
    }
    // A duplicate waiting for its Mac by name, beside a row no Mac is bound to: the row that stays
    // must wait too (`register::pick`), which only an import's `active` lets it, since that word says
    // nothing (`dump_computer`). Any other word is a statement this merge does not overrule.
    let waiting = rows
        .iter()
        .any(|(pk, row)| *pk != keep && status_of(row) == Some("planned"));
    let can_wait = match keeper.fields.get("status") {
        None => true,
        Some(Field::Value(held)) => {
            held.value.x == json!("planned")
                || (held.value.s.is_some()
                    && (held.value.x.is_null() || held.value.x == json!("active")))
        }
        Some(_) => false,
    };
    if waiting && unbound(keep, keeper, facts) && !can_wait {
        return Err("status-differs");
    }
    if preferences
        .rows
        .values()
        .any(|row| on_group(row) && target_of(row, keep, preferences).is_some_and(conflicted))
    {
        return Err("conflicted");
    }
    let mut current = names.iter().filter(|name| roster.contains_key(*name));
    let label = match (current.next(), current.next()) {
        (Some(_), Some(_)) => return Err("two-roster-names"),
        (Some(name), None) if other_mac(keeper, name, roster) => return Err("two-macs"),
        (Some(name), None) => name.clone(),
        (None, _) => {
            let held = label_of(keeper);
            match held.or_else(|| names.first().map(String::as_str)) {
                Some(label) => label.to_owned(),
                None => return Err("no-node"),
            }
        }
    };
    Ok((keep, label))
}

fn owner_cell(pk: &str, col: &str, elem: Option<&str>, x: Value) -> Result<Change> {
    cell::change(
        &MACHINE,
        CellKey { pk, col, elem },
        &CellValue::owner(MACHINE.version, x),
    )
}

fn merge_group(
    plan: &mut Plan,
    rows: &[(&str, &Row)],
    keep: &str,
    label: String,
    names: &BTreeSet<String>,
    preferences: &Projection,
    waits: bool,
) -> Result<()> {
    let keeper = rows
        .iter()
        .find(|(pk, _)| *pk == keep)
        .map(|(_, row)| *row)
        .expect("the row that stays is one of the rows");
    let gone: Vec<(&str, &Row)> = rows.iter().copied().filter(|(pk, _)| *pk != keep).collect();
    let mut merge = Merge {
        keep: keep.to_owned(),
        gone: gone.iter().map(|(pk, _)| (*pk).to_owned()).collect(),
        label_from: label_of(keeper).map(str::to_owned),
        label_to: label.clone(),
        ..Merge::default()
    };
    // Each preference moves in its own entry, with its end, so a run cut short leaves it on one
    // row and the next run finds nothing left to move of it.
    for (pk, _) in &gone {
        move_preferences(
            &mut plan.rows,
            &mut merge.preferences,
            pk,
            keep,
            preferences,
        )?;
    }
    let mut carries = Vec::new();
    for (pk, row) in &gone {
        carry_cells(&mut merge, &mut carries, pk, row, keep, keeper, waits)?;
    }
    carry_hosts(&mut merge, &mut carries, &gone, keep, keeper)?;
    plan.rows
        .extend(carries.chunks(RECORD).map(<[Change]>::to_vec));
    // The names and the duplicates' ends go in one record, so no reader sees a name on two rows or
    // on none.
    let mut last = Vec::new();
    if merge.label_from.as_deref() != Some(label.as_str()) {
        // The label keeps the provenance it had, so the source that wrote it can move it again.
        let source = rows
            .iter()
            .find_map(|(_, row)| match row.fields.get("label") {
                Some(Field::Value(candidate)) if candidate.value.x == json!(label) => {
                    candidate.value.s.clone()
                }
                _ => None,
            });
        last.push(cell::change(
            &MACHINE,
            CellKey {
                pk: keep,
                col: "label",
                elem: None,
            },
            &CellValue {
                r: MACHINE.version,
                x: json!(label),
                s: source,
                a: false,
            },
        )?);
    }
    let held = keeper.fields.get("aliases");
    let element = |name: &str| match held {
        Some(Field::Set(elements)) => elements.get(name).and_then(|field| match field {
            Field::Value(candidate) => Some(&candidate.value),
            _ => None,
        }),
        _ => None,
    };
    for name in names {
        let member = held.is_some_and(|field| field.has_member(name));
        if *name == label {
            if member {
                let source = element(name).and_then(|cell| cell.s.clone());
                last.push(cell::change(
                    &MACHINE,
                    CellKey {
                        pk: keep,
                        col: "aliases",
                        elem: Some(name),
                    },
                    &CellValue {
                        r: MACHINE.version,
                        x: json!(false),
                        s: source,
                        a: false,
                    },
                )?);
                merge.aliases_removed.push(name.clone());
            }
        } else if element(name).is_some_and(|cell| cell.x == json!(false) && cell.s.is_none()) {
            // The owner took this name off the row that stays: it stays off.
            let from = gone
                .iter()
                .find(|(_, row)| names_of(row).contains(name))
                .map_or(keep, |(pk, _)| *pk);
            merge
                .kept
                .push((from.to_owned(), format!("aliases.{name}")));
        } else if !member {
            last.push(owner_cell(keep, "aliases", Some(name), json!(true))?);
            merge.aliases_added.push(name.clone());
        }
    }
    for (pk, _) in &gone {
        last.push(owner_cell(pk, "merged_into", None, json!(keep))?);
        last.push(owner_cell(pk, LIFECYCLE, None, json!("merged"))?);
    }
    plan.rows.push(last);
    plan.merges.push(merge);
    Ok(())
}

/// The hosts of the row that stays and of its duplicates together, its own first, as many as a row
/// holds; what does not fit is listed.
fn carry_hosts(
    merge: &mut Merge,
    out: &mut Vec<Change>,
    gone: &[(&str, &Row)],
    keep: &str,
    keeper: &Row,
) -> Result<()> {
    let list = |row: &Row| -> Vec<String> {
        row.get("hosts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let mut hosts = list(keeper);
    let mut source = match keeper.fields.get("hosts") {
        Some(Field::Value(candidate)) if !candidate.value.x.is_null() => candidate.value.s.clone(),
        _ => None,
    };
    let mut grew = false;
    for (pk, row) in gone {
        for host in list(row) {
            if hosts.iter().any(|held| same_host(held, &host)) {
                continue;
            }
            if hosts.len() == MAX_HOSTS {
                merge.kept.push(((*pk).to_owned(), "hosts".to_owned()));
                continue;
            }
            if !grew
                && hosts.is_empty()
                && let Some(Field::Value(candidate)) = row.fields.get("hosts")
            {
                source.clone_from(&candidate.value.s);
            }
            hosts.push(host);
            grew = true;
            if !merge.carried.iter().any(|(_, column)| column == "hosts") {
                merge.carried.push(((*pk).to_owned(), "hosts".to_owned()));
            }
        }
    }
    if grew {
        out.push(cell::change(
            &MACHINE,
            CellKey {
                pk: keep,
                col: "hosts",
                elem: None,
            },
            &CellValue {
                r: MACHINE.version,
                x: json!(hosts),
                s: source,
                a: false,
            },
        )?);
    }
    Ok(())
}

/// Whether a duplicate's `status` word moves onto the row that stays. `planned` is the word a row
/// holds until a Mac joins it and fleet_db's `active` says nothing of that, so `active` never moves
/// off it while `parked` or `lost` do, and it moves onto a row that `waits` as the Mac's way in.
fn status_moves(word: &CellValue, there: Option<&Field>, waits: bool) -> bool {
    match value(there).and_then(Value::as_str) {
        None => lacks(word, there),
        Some("planned") => word.x != json!("active"),
        Some(_) => waits && word.x == json!("planned"),
    }
}

/// The cells of duplicate `pk` the row that stays lacks, with their provenance. A set takes the
/// elements it never held: one it holds false was removed there. A cell of the row that stays this
/// build cannot read stays. Its `status` is [`status_moves`]'s, and a `planned` it takes over an
/// import's word keeps that import's provenance, so a later import can still correct it.
fn carry_cells(
    merge: &mut Merge,
    out: &mut Vec<Change>,
    pk: &str,
    row: &Row,
    keep: &str,
    keeper: &Row,
    waits: bool,
) -> Result<()> {
    for (column, field) in &row.fields {
        if NOT_CARRIED.contains(&column.as_str()) || row.raw.contains(column) {
            continue;
        }
        if keeper.raw.contains(column) {
            merge.kept.push((pk.to_owned(), column.clone()));
            continue;
        }
        match field {
            Field::Set(elements) => {
                let held = keeper.fields.get(column);
                for (elem, element) in elements {
                    let Field::Value(candidate) = element else {
                        continue;
                    };
                    let shown = format!("{column}.{elem}");
                    let known = matches!(held, Some(Field::Set(have)) if have.contains_key(elem));
                    let taken = merge.carried.iter().any(|(_, column)| *column == shown);
                    if candidate.value.x == json!(true) && !known && !taken {
                        out.push(cell::change(
                            &MACHINE,
                            CellKey {
                                pk: keep,
                                col: column,
                                elem: Some(elem),
                            },
                            &carried(&candidate.value, MACHINE.version),
                        )?);
                        merge.carried.push((pk.to_owned(), shown));
                    }
                }
            }
            Field::Value(candidate) if !candidate.value.x.is_null() => {
                let there = keeper.fields.get(column);
                let held = value(there);
                let status = column == "status";
                let needed = if status {
                    status_moves(&candidate.value, there, waits)
                } else {
                    lacks(&candidate.value, there)
                };
                let taken = merge.carried.iter().any(|(_, col)| col == column);
                if held == Some(&candidate.value.x) {
                    continue;
                }
                if needed && !taken {
                    let mut moved = carried(&candidate.value, MACHINE.version);
                    // An import's `active` says nothing of a row that waits for its Mac by name.
                    if status && waits && moved.x == json!("active") && moved.s.is_some() {
                        moved.x = json!("planned");
                    }
                    if status
                        && held.is_some_and(|word| word != &json!("planned"))
                        && let Some(Field::Value(word)) = there
                    {
                        moved.s.clone_from(&word.value.s);
                    }
                    out.push(cell::change(
                        &MACHINE,
                        CellKey {
                            pk: keep,
                            col: column,
                            elem: None,
                        },
                        &moved,
                    )?);
                    merge.carried.push((pk.to_owned(), column.clone()));
                } else if !PROVENANCE.contains(&column.as_str()) {
                    merge.kept.push((pk.to_owned(), column.clone()));
                }
            }
            Field::Value(_) | Field::Conflict { .. } => {}
        }
    }
    // What a newer build wrote can't be read here, so it stays on the duplicate.
    merge
        .kept
        .extend(row.raw.iter().map(|column| (pk.to_owned(), column.clone())));
    Ok(())
}

/// Move every live preference of row `pk` to `keep`, each in an entry of its own, listing it in
/// `moved`, which says what each earlier move gave the row it reached.
fn move_preferences(
    rows: &mut Vec<Vec<Change>>,
    moved: &mut Vec<Moved>,
    pk: &str,
    keep: &str,
    preferences: &Projection,
) -> Result<()> {
    let change = |target: &str, col: &str, value: &CellValue| {
        cell::change(
            &PREFERENCE,
            CellKey {
                pk: target,
                col,
                elem: None,
            },
            value,
        )
    };
    let moving = preferences
        .rows
        .iter()
        .filter(|(_, row)| row.live() && row.get("machine").and_then(Value::as_str) == Some(pk));
    for (from, row) in moving {
        let Some(account) = row.get("account").and_then(Value::as_str) else {
            continue;
        };
        let target = keys::preference_pk(keep, account)?;
        let earlier = moved.iter().filter(|moved| moved.to == target);
        let given: BTreeSet<&str> = earlier
            .clone()
            .flat_map(|moved| moved.columns.iter().map(String::as_str))
            .collect();
        let already = earlier.clone().next().map(|moved| moved.how);
        let there = preferences.rows.get(&target);
        let how = match (already, there) {
            (Some(How::OwnerEnded), _) => How::OwnerEnded,
            (Some(_), _) => How::Filled,
            (None, None) => How::Moved,
            (None, Some(row)) if row.live() => How::Filled,
            (None, Some(row)) => match row.fields.get(LIFECYCLE) {
                Some(Field::Value(candidate)) if candidate.value.s.is_some() => How::Revived,
                _ => How::OwnerEnded,
            },
        };
        let mut entry = Vec::new();
        let mut columns = Vec::new();
        let mut kept: Vec<String> = row.raw.iter().cloned().collect();
        if how != How::OwnerEnded {
            for (column, field) in &row.fields {
                let Field::Value(candidate) = field else {
                    continue;
                };
                if PREFERENCE_IDENTITY.contains(&column.as_str())
                    || row.raw.contains(column)
                    || candidate.value.x.is_null()
                    || given.contains(column.as_str())
                {
                    continue;
                }
                if there.is_some_and(|row| row.raw.contains(column)) {
                    kept.push(column.clone());
                    continue;
                }
                let on_target = there.and_then(|row| row.fields.get(column));
                if value(on_target) == Some(&candidate.value.x) {
                    continue;
                }
                if lacks(&candidate.value, on_target) {
                    entry.push(change(
                        &target,
                        column,
                        &carried(&candidate.value, PREFERENCE.version),
                    )?);
                    columns.push(column.clone());
                } else if !PROVENANCE.contains(&column.as_str()) {
                    kept.push(column.clone());
                }
            }
        }
        if how == How::Moved {
            for (column, x) in [("machine", json!(keep)), ("account", json!(account))] {
                let source = match row.fields.get(column) {
                    Some(Field::Value(candidate)) => candidate.value.s.clone(),
                    _ => None,
                };
                entry.push(change(
                    &target,
                    column,
                    &CellValue {
                        r: PREFERENCE.version,
                        x,
                        s: source,
                        a: false,
                    },
                )?);
            }
        }
        if matches!(how, How::Moved | How::Revived) {
            let source = match row.fields.get(LIFECYCLE) {
                Some(Field::Value(candidate)) => candidate.value.s.clone(),
                _ => None,
            };
            entry.push(change(
                &target,
                LIFECYCLE,
                &CellValue {
                    r: PREFERENCE.version,
                    x: json!("live"),
                    s: source,
                    a: false,
                },
            )?);
        }
        entry.push(change(
            from,
            LIFECYCLE,
            &CellValue::owner(PREFERENCE.version, json!("ended")),
        )?);
        rows.push(entry);
        moved.push(Moved {
            from: from.clone(),
            to: target,
            how,
            columns,
            kept,
        });
    }
    Ok(())
}

impl Plan {
    /// The value-free report: keys, labels and column names.
    pub fn report(&self) -> Value {
        let pairs = |list: &[(String, String)]| -> Vec<Value> {
            list.iter()
                .map(|(from, column)| json!({"from": from, "column": column}))
                .collect()
        };
        let moved = |moved: &Moved| {
            json!({
                "from": moved.from,
                "to": moved.to,
                "how": moved.how.word(),
                "columns": moved.columns,
                "kept": moved.kept,
            })
        };
        json!({
            "merges": self.merges.iter().map(|merge| json!({
                "keep": merge.keep,
                "gone": merge.gone,
                "label": {"from": merge.label_from, "to": merge.label_to},
                "aliases_added": merge.aliases_added,
                "aliases_removed": merge.aliases_removed,
                "carried": pairs(&merge.carried),
                "kept": pairs(&merge.kept),
                "preferences": merge.preferences.iter().map(moved).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "strays": self.strays.iter().map(moved).collect::<Vec<_>>(),
            "waiting": self.waiting.iter().map(|waiting| json!({
                "row": waiting.row,
                "label": waiting.label,
            })).collect::<Vec<_>>(),
            "held": self.held.iter().map(|held| json!({
                "rows": held.rows,
                "names": held.names,
                "why": held.why,
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(all(test, unix))]
mod tests;
