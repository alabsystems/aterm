// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-registration (docs/DESIGN-alab-lab-model-2026-11-01.md §4.6, §4.7, §6): the facts a Mac
//! states about itself at its own Commons machine uuid, the one binding from an enrollment to a
//! machine row, chosen once by the adoption rule. [`plan`] is pure over the registry this Mac
//! holds; `Registry::register` runs it under the writer and [`Registrar`] runs that when the
//! Commons runtime completes repair rounds, so a Mac alone on its LAN writes nothing and no timer
//! wakes anything.
//!
//! A Mac's identity here is its Commons uuid, its fleet node and its membership label. The
//! IOPlatformUUID stays in this process: only a keyed digest of it ([`keys::hw_hint`]) is written.
//! No Mac edits an existing machine row, and a hint match without the §6 evidence is a finding,
//! never an adoption, because VM clones share a machine identifier.

use super::{
    checks::Finding,
    registry::{MACHINE, MACHINE_FACTS, MODEL},
};
use crate::{
    Error, Result,
    database::Change,
    typed::{
        LIFECYCLE,
        cell::{self, CellKey, CellValue, Cells, Projection, Row},
        guard, keys,
        owned::OwnedRead,
        pk_grammar,
    },
};
use aterm_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// A facts row is rewritten once its `seen` is this many days old, so a reinstall can be judged
/// after the journal's ring deleted the old weeks.
pub const REFRESH_DAYS: u64 = 30;
/// A hint match adopts only when the earlier row's `seen` predates this enrollment by more.
pub const REINSTALL_DAYS: u64 = 30;

const MS_PER_DAY: u64 = 86_400_000;

/// The `day` type: whole days since the Unix epoch.
pub const fn day_of(ms: u64) -> u64 {
    ms / MS_PER_DAY
}

/// What this Mac reads about itself. No `Debug`: it holds the IOPlatformUUID, which never leaves
/// the process (`keys::hw_hint`).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// `sysctl hw.model`, as read (`Mac16,8`).
    pub hw_model: String,
    pub chip: Option<String>,
    pub cores_p: Option<u64>,
    pub cores_e: Option<u64>,
    pub gpu_cores: Option<u64>,
    pub ram_gb: Option<u64>,
    pub disk_gb: Option<u64>,
    pub arch: Option<String>,
    pub os_major: Option<u64>,
    /// Computer and local host names (§4.7): the names the owner gave this Mac. `hostname` is not
    /// one, since a network can supply it.
    pub hosts: Vec<String>,
    /// The fleet node id, when this Mac has one.
    pub node: Option<String>,
    pub platform_uuid: String,
}

/// This Mac as the `aterm` executable reads it, for `check`'s preview of a pass.
pub struct ThisMac {
    pub probe: Probe,
    /// The file `[commons]` is read from.
    pub config: std::path::PathBuf,
}

/// Who this enrollment is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Me {
    /// The Commons machine uuid, 32 hex: the only key a facts row of this Mac can have.
    pub uuid: String,
    /// The membership log's label, clash suffix included.
    pub label: Option<String>,
    /// [`keys::hw_hint`] of this Mac's hardware under this network.
    pub hw_hint: String,
    /// When the vault's roster says this Mac was admitted, where it says.
    pub enrolled_ms: Option<u64>,
}

/// What a plan reads besides the registry.
#[derive(Clone, Copy)]
pub struct Input<'a> {
    pub me: &'a Me,
    pub probe: &'a Probe,
    /// The Commons uuids (32 hex) of the current members.
    pub live: &'a BTreeSet<String>,
    /// This process saw a completed repair round that left nothing to fetch from a peer holding
    /// the registry ([`crate::runtime::RepairRound::drained_table`]).
    pub heard: bool,
    pub now_ms: u64,
}

/// Which step of the §6 rule bound this enrollment to its machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// 1: an earlier enrollment's row with this hint, with the reinstall evidence.
    Reinstall,
    /// 2: the machine row names this Mac's fleet node.
    Node,
    /// 2: the machine row names one of this Mac's host names.
    Host,
    /// 3: a planned machine under this Mac's membership label or an alias.
    Planned,
    /// 4: the machine's row was already this uuid's own birth.
    OwnBirth,
}

/// What one pass decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `[commons]` turns sharing off, or excludes the registry or the catalog.
    Excluded,
    /// No Commons here, or the registry is not declared here yet.
    NotHeld,
    /// This Mac's history of the registry waits on a record, so nothing can be compared.
    Incomplete,
    /// The row states what the row says, and `seen` is not old.
    Unchanged,
    /// The row was rewritten for a changed field or an old `seen`.
    Refreshed { mac: String },
    /// This enrollment's first row, bound to an existing machine.
    Adopted { rule: Rule, mac: String },
    /// This enrollment's first row, with a new machine row of its own.
    Born { mac: String },
    /// A first row waits for a completed repair round with a peer holding the registry.
    Deferred,
    /// Nothing can be decided without the owner; the findings say why.
    Held,
    /// The row would pass this Mac's quota of automatic records.
    QuotaSpent,
}

impl Outcome {
    /// The word `check` reports this outcome by, and the machine it names.
    pub fn report(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Excluded => ("excluded", None),
            Self::NotHeld => ("not-held", None),
            Self::Incomplete => ("incomplete", None),
            Self::Unchanged => ("unchanged", None),
            Self::Refreshed { mac } => ("refresh-due", Some(mac)),
            Self::Adopted { mac, .. } => ("adopts", Some(mac)),
            Self::Born { mac } => ("births", Some(mac)),
            Self::Deferred => ("deferred", None),
            Self::Held => ("held", None),
            Self::QuotaSpent => ("quota-spent", None),
        }
    }
}

/// What [`plan`] returns: at most one record, so the machine, its model and the facts row that
/// binds them land together.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub outcome: Outcome,
    pub changes: Vec<Change>,
    pub findings: Vec<Finding>,
}

impl Plan {
    fn quiet(outcome: Outcome, findings: Vec<Finding>) -> Self {
        Self {
            outcome,
            changes: Vec::new(),
            findings,
        }
    }
}

/// A `model` row's key from `hw.model` (`Mac16,8` becomes `Mac16-8`).
///
/// # Errors
/// What is left does not fit the key grammar.
pub fn model_key(hw_model: &str) -> Result<String> {
    let key = hw_model.trim().replace(',', "-");
    if pk_grammar(&key) {
        Ok(key)
    } else {
        Err(Error::new("hw.model does not make a model key"))
    }
}

/// `M4` from `Apple M4 Max`: the family word of an Apple chip, none for anything else.
fn chip_family(chip: &str) -> Option<&str> {
    let mut words = chip.split_whitespace();
    let family = (words.next() == Some("Apple"))
        .then(|| words.next())
        .flatten()?;
    let mut letters = family.chars();
    (letters.next() == Some('M') && letters.next().is_some_and(|c| c.is_ascii_digit()))
        .then_some(family)
}

fn text<'a>(row: &'a Value, column: &str) -> Option<&'a str> {
    row.get(column).and_then(Value::as_str)
}

/// One name, with or without the `.local` a stored `hostname` reading carries.
fn same_host(a: &str, b: &str) -> bool {
    fn bare(name: &str) -> &str {
        let name = name.trim();
        match name.len().checked_sub(".local".len()) {
            Some(cut)
                if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".local") =>
            {
                &name[..cut]
            }
            _ => name,
        }
    }
    bare(a).eq_ignore_ascii_case(bare(b))
}

/// This Mac's host names as a facts cell: spelled once each, in a stable order so a changed
/// reading order is no change, at most the column's four.
fn hosts_cell(hosts: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut names: Vec<&str> = hosts
        .iter()
        .map(|h| h.trim())
        .filter(|h| !h.is_empty() && guard::text(h, 64).is_ok())
        .collect();
    names.sort_unstable();
    for name in names {
        if !out.iter().any(|held| same_host(held, name)) && out.len() < 4 {
            out.push(name.to_owned());
        }
    }
    out
}

/// The cells a facts row states, the three a rewrite always moves (`n`, `seen`, `at`) apart. A
/// reading that failed or that its column refuses keeps what the row held, so a passing read
/// failure spends no record and blanks no cell.
fn stated(
    input: &Input<'_>,
    held: Option<&Value>,
    mac: &str,
    predecessor: Option<&str>,
) -> Result<BTreeMap<&'static str, Value>> {
    let probe = input.probe;
    let hosts = hosts_cell(&probe.hosts);
    let readings: [(&'static str, Option<Value>); 13] = [
        ("mac", Some(Value::from(mac))),
        ("predecessor", predecessor.map(Value::from)),
        ("label", input.me.label.as_deref().map(Value::from)),
        (
            "hosts",
            (!hosts.is_empty())
                .then(|| Value::Array(hosts.iter().map(|h| Value::from(h.as_str())).collect())),
        ),
        ("hw_model", Some(Value::from(model_key(&probe.hw_model)?))),
        ("chip", probe.chip.as_deref().map(Value::from)),
        ("cores_p", probe.cores_p.map(Value::from)),
        ("cores_e", probe.cores_e.map(Value::from)),
        ("gpu_cores", probe.gpu_cores.map(Value::from)),
        ("ram_gb", probe.ram_gb.map(Value::from)),
        ("disk_gb", probe.disk_gb.map(Value::from)),
        ("arch", probe.arch.as_deref().map(Value::from)),
        ("os_major", probe.os_major.map(Value::from)),
    ];
    let mut row = BTreeMap::new();
    row.insert("hw_hint", Value::from(input.me.hw_hint.as_str()));
    if let Some(node) = probe.node.as_deref() {
        row.insert("node", Value::from(node));
    }
    for (column, reading) in readings {
        if let Some(x) = reading {
            row.insert(column, x);
        }
    }
    // An owner merge's acknowledgment (`merged_into`) is this Mac's own word: only ever carried.
    for column in MACHINE_FACTS.columns {
        if matches!(column.name, "n" | "seen" | "at") {
            continue;
        }
        let refused = row
            .get(column.name)
            .is_some_and(|x| cell::validate(&MACHINE_FACTS, column, x).is_err());
        if refused {
            row.remove(column.name);
        }
        if !row.contains_key(column.name)
            && let Some(x) = held.and_then(|row| row.get(column.name))
            && !x.is_null()
        {
            row.insert(column.name, x.clone());
        }
    }
    for required in ["label", "hw_hint", "hw_model", "mac"] {
        if !row.contains_key(required) {
            return Err(Error::new(&format!(
                "machine-facts.{required}: nothing to state"
            )));
        }
    }
    row.entry("hosts")
        .or_insert_with(|| Value::Array(Vec::new()));
    Ok(row)
}

fn facts_change(
    uuid: &str,
    row: BTreeMap<&'static str, Value>,
    n: u64,
    seen: u64,
    at: u64,
) -> Change {
    let mut object = Map::new();
    object.insert("r".into(), Value::from(u64::from(MACHINE_FACTS.version)));
    for (column, x) in row {
        object.insert(column.into(), x);
    }
    object.insert("n".into(), Value::from(n));
    object.insert("seen".into(), Value::from(seen));
    object.insert("at".into(), Value::from(at));
    Change {
        collection: MACHINE_FACTS.name.into(),
        key: uuid.to_owned(),
        value: Some(Value::Object(object)),
    }
}

fn automatic(
    collection: &crate::typed::Collection,
    pk: &str,
    col: &str,
    x: Value,
) -> Result<Change> {
    cell::change(
        collection,
        CellKey {
            pk,
            col,
            elem: None,
        },
        &CellValue::automatic(collection.version, x),
    )
}

/// The first Mac to report an unseen model writes its key and what it knows; the owner fills in
/// the rest.
fn model_birth(cells: &Cells, probe: &Probe) -> Result<Vec<Change>> {
    let key = model_key(&probe.hw_model)?;
    if cell::project(&MODEL, cells).rows.contains_key(&key) {
        return Ok(Vec::new());
    }
    let mut changes = vec![automatic(&MODEL, &key, LIFECYCLE, Value::from("live"))?];
    if let Some(family) = probe.chip.as_deref().and_then(chip_family)
        && guard::text(family, 32).is_ok()
    {
        changes.push(automatic(&MODEL, &key, "chip_family", Value::from(family))?);
    }
    Ok(changes)
}

/// Rule 4's row: keyed by this uuid, naming what this Mac reads of itself, except a fleet node or a
/// host name a live row already names. A disk clone keeps its original's fleet files and names, and
/// a second row naming them would hold the original at `node-ambiguous` for good; this Mac's facts
/// row still states what it read.
fn machine_birth(
    input: &Input<'_>,
    machines: &Projection,
    pk: &str,
    label: &str,
) -> Result<Vec<Change>> {
    let probe = input.probe;
    let live_rows = || machines.rows.values().filter(|row| row.live());
    let node_named = |node: &str| {
        live_rows().any(|row| row.get("fleet_node").and_then(Value::as_str) == Some(node))
    };
    let host_named = |host: &str| {
        live_rows().any(|row| {
            row.get("hosts")
                .and_then(Value::as_array)
                .is_some_and(|hosts| {
                    hosts
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|held| same_host(held, host))
                })
        })
    };
    let mut changes = vec![
        automatic(&MACHINE, pk, "label", Value::from(label))?,
        automatic(&MACHINE, pk, "status", Value::from("active"))?,
        automatic(
            &MACHINE,
            pk,
            "model",
            Value::from(model_key(&probe.hw_model)?),
        )?,
        automatic(&MACHINE, pk, LIFECYCLE, Value::from("live"))?,
    ];
    if let Some(node) = probe.node.as_deref().filter(|node| !node_named(node)) {
        changes.push(automatic(&MACHINE, pk, "fleet_node", Value::from(node))?);
    }
    let mut hosts = hosts_cell(&probe.hosts);
    hosts.retain(|host| !host_named(host));
    if !hosts.is_empty() {
        changes.push(automatic(
            &MACHINE,
            pk,
            "hosts",
            Value::Array(hosts.iter().map(|h| Value::from(h.as_str())).collect()),
        )?);
    }
    Ok(changes)
}

/// Whether `row` goes by `label`, as its label or one of its aliases.
fn goes_by(row: &Row, label: &str) -> bool {
    row.get("label").and_then(Value::as_str) == Some(label)
        || row
            .fields
            .get("aliases")
            .is_some_and(|field| field.has_member(label))
}

/// The machines live enrollments other than this one are bound to.
fn bound_elsewhere<'a>(input: &Input<'_>, facts: &'a OwnedRead) -> BTreeSet<&'a str> {
    facts
        .rows
        .iter()
        .filter(|(uuid, _)| **uuid != input.me.uuid && input.live.contains(uuid.as_str()))
        .filter_map(|(_, row)| text(&row.value, "mac"))
        .collect()
}

fn finding(kind: &'static str, collection: &'static str, pks: Vec<String>) -> Finding {
    Finding {
        kind,
        collection,
        pks,
    }
}

/// Rule 1: `Ok(Some((machine, predecessor)))` when an earlier enrollment with this hint is gone,
/// and its row's `seen` predates this enrollment by more than [`REINSTALL_DAYS`]. Every other hint
/// match is a finding: a live uuid (a clone running now), or a gone one without that evidence.
/// `Err` when the evidence names two machines, which only the owner can tell apart.
fn reinstall(
    input: &Input<'_>,
    machines: &Projection,
    facts: &OwnedRead,
    bound: &BTreeSet<&str>,
    found: &mut Vec<Finding>,
) -> std::result::Result<Option<(String, String)>, ()> {
    let me = input.me;
    let enrolled = day_of(input.now_ms.min(me.enrolled_ms.unwrap_or(u64::MAX)));
    // Per machine, the newest row that names this hint: an older row's age proves nothing when a
    // later enrollment of the same machine was seen more recently.
    let mut newest: BTreeMap<&str, (u64, &str)> = BTreeMap::new();
    for (uuid, row) in &facts.rows {
        if *uuid == me.uuid || text(&row.value, "hw_hint") != Some(me.hw_hint.as_str()) {
            continue;
        }
        if input.live.contains(uuid) {
            found.push(finding(
                "hw-hint-live",
                MACHINE_FACTS.name,
                vec![me.uuid.clone(), uuid.clone()],
            ));
            continue;
        }
        let Some(mac) = text(&row.value, "mac") else {
            continue;
        };
        let seen = row
            .value
            .get("seen")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let held = newest.entry(mac).or_insert((seen, uuid));
        if (seen, uuid.as_str()) > *held {
            *held = (seen, uuid);
        }
    }
    let mut proven = Vec::new();
    for (mac, (seen, uuid)) in newest {
        let usable = machines.rows.get(mac).is_some_and(Row::live) && !bound.contains(mac);
        if usable && seen.saturating_add(REINSTALL_DAYS) < enrolled {
            proven.push((mac.to_owned(), uuid.to_owned()));
        } else {
            found.push(finding(
                "hw-hint-unproven",
                MACHINE_FACTS.name,
                vec![me.uuid.clone(), uuid.to_owned()],
            ));
        }
    }
    match proven.len() {
        0 => Ok(None),
        1 => Ok(proven.pop()),
        _ => {
            found.push(finding(
                "hw-hint-ambiguous",
                MACHINE_FACTS.name,
                proven.into_iter().map(|(_, uuid)| uuid).collect(),
            ));
            Err(())
        }
    }
}

/// The live, unbound machine rule 2 or 3 picks, the rule's other candidates being a finding.
fn only(
    kind: &'static str,
    candidates: Vec<&String>,
    found: &mut Vec<Finding>,
) -> std::result::Result<Option<String>, ()> {
    match candidates.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some((*one).clone())),
        many => {
            found.push(finding(
                kind,
                MACHINE.name,
                many.iter().map(|pk| (*pk).clone()).collect(),
            ));
            Err(())
        }
    }
}

/// What adoption picks: rules 1 to 3 of §6, in order, then 4.
enum Pick {
    Machine {
        rule: Rule,
        mac: String,
        predecessor: Option<String>,
    },
    Birth,
    Held,
}

fn pick(
    input: &Input<'_>,
    machines: &Projection,
    facts: &OwnedRead,
    found: &mut Vec<Finding>,
) -> Pick {
    let bound = bound_elsewhere(input, facts);
    let me = input.me;
    let probe = input.probe;
    match reinstall(input, machines, facts, &bound, found) {
        Err(()) => return Pick::Held,
        Ok(Some((mac, predecessor))) => {
            return Pick::Machine {
                rule: Rule::Reinstall,
                mac,
                predecessor: Some(predecessor),
            };
        }
        Ok(None) => {}
    }
    let live_rows = || machines.rows.iter().filter(|(_, row)| row.live());
    fn node_of(row: &Row) -> Option<&str> {
        row.get("fleet_node").and_then(Value::as_str)
    }
    // A row stating another fleet node is another Mac, whatever else it shares with this one.
    let no_other_node = |row: &Row| node_of(row).is_none_or(|n| probe.node.as_deref() == Some(n));
    // Two rows naming this node adopt neither, and a birth beside them would make it three.
    if let Some(node) = probe.node.as_deref() {
        let named: Vec<&String> = live_rows()
            .filter(|(_, row)| node_of(row) == Some(node))
            .map(|(pk, _)| pk)
            .collect();
        if named.len() > 1 {
            found.push(finding(
                "node-ambiguous",
                MACHINE.name,
                named.iter().map(|pk| (*pk).clone()).collect(),
            ));
            return Pick::Held;
        }
        for pk in named {
            if bound.contains(pk.as_str()) {
                // Another live enrollment holds this node's row: a disk clone keeps its fleet
                // files. This Mac goes on, and `check` shows the owner.
                found.push(finding(
                    "node-bound-elsewhere",
                    MACHINE.name,
                    vec![pk.clone()],
                ));
            } else {
                return Pick::Machine {
                    rule: Rule::Node,
                    mac: pk.clone(),
                    predecessor: None,
                };
            }
        }
    }
    let by_host: Vec<&String> = live_rows()
        .filter(|(pk, row)| {
            !bound.contains(pk.as_str())
                && no_other_node(row)
                && row
                    .get("hosts")
                    .and_then(Value::as_array)
                    .is_some_and(|hosts| {
                        hosts
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|host| probe.hosts.iter().any(|mine| same_host(mine, host)))
                    })
        })
        .map(|(pk, _)| pk)
        .collect();
    match only("host-ambiguous", by_host, found) {
        Err(()) => return Pick::Held,
        Ok(Some(mac)) => {
            return Pick::Machine {
                rule: Rule::Host,
                mac,
                predecessor: None,
            };
        }
        Ok(None) => {}
    }
    if let Some(label) = me.label.as_deref() {
        let planned: Vec<&String> = live_rows()
            .filter(|(pk, row)| {
                !bound.contains(pk.as_str())
                    && no_other_node(row)
                    && row.get("status").and_then(Value::as_str) == Some("planned")
                    && goes_by(row, label)
            })
            .map(|(pk, _)| pk)
            .collect();
        match only("planned-ambiguous", planned, found) {
            Err(()) => return Pick::Held,
            Ok(Some(mac)) => {
                return Pick::Machine {
                    rule: Rule::Planned,
                    mac,
                    predecessor: None,
                };
            }
            Ok(None) => {}
        }
    }
    Pick::Birth
}

/// What this Mac should write now over the registry it holds.
///
/// # Errors
/// `hw.model` makes no model key, or a cell this Mac would write breaks its column's rules.
pub fn plan(input: &Input<'_>, cells: &Cells, facts: &OwnedRead) -> Result<Plan> {
    let me = input.me;
    let today = day_of(input.now_ms);
    if let Some(own) = facts.rows.get(&me.uuid)
        && let Some(mac) = text(&own.value, "mac")
    {
        // A binding is for good, so a machine row the owner ended holds this Mac rather than
        // have it restate facts under a row nobody uses any more; only the owner moves it.
        if cell::project(&MACHINE, cells)
            .rows
            .get(mac)
            .is_some_and(|row| !row.live())
        {
            return Ok(Plan::quiet(
                Outcome::Held,
                vec![finding(
                    "bound-machine-retired",
                    MACHINE.name,
                    vec![mac.to_owned()],
                )],
            ));
        }
        let seen = own.value.get("seen").and_then(Value::as_u64).unwrap_or(0);
        let row = stated(input, Some(&own.value), mac, None)?;
        // Host names move without the Mac changing (a Bonjour clash renames the local host), and a
        // change spends a record of a lifetime quota, so they ride the 30-day rewrite. The other
        // fields move at most once a day.
        let changed = MACHINE_FACTS
            .columns
            .iter()
            .filter(|c| !matches!(c.name, "n" | "seen" | "at" | "hosts"))
            .any(|c| row.get(c.name) != own.value.get(c.name));
        let due = seen.saturating_add(REFRESH_DAYS) <= today || (changed && seen < today);
        if !due {
            return Ok(Plan::quiet(Outcome::Unchanged, Vec::new()));
        }
        let n = own
            .value
            .get("n")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(1);
        let mut changes = model_birth(cells, input.probe)?;
        changes.push(facts_change(
            &me.uuid,
            row,
            n,
            today.max(seen),
            input.now_ms,
        ));
        return Ok(Plan {
            outcome: Outcome::Refreshed {
                mac: mac.to_owned(),
            },
            changes,
            findings: Vec::new(),
        });
    }
    let mut findings = Vec::new();
    // A first row binds this enrollment for good, and a partial history can show a machine row
    // without the facts row that binds it, so every first row waits for the round, not only a
    // birth (design §6, as built).
    if !input.heard {
        return Ok(Plan::quiet(Outcome::Deferred, findings));
    }
    let Some(label) = me.label.as_deref() else {
        findings.push(finding(
            "no-label",
            MACHINE_FACTS.name,
            vec![me.uuid.clone()],
        ));
        return Ok(Plan::quiet(Outcome::Held, findings));
    };
    let machines = cell::project(&MACHINE, cells);
    let own_pk = keys::machine_pk(&me.uuid);
    let (outcome, mac, predecessor, births) = match pick(input, &machines, facts, &mut findings) {
        Pick::Held => return Ok(Plan::quiet(Outcome::Held, findings)),
        Pick::Machine {
            rule,
            mac,
            predecessor,
        } => (
            Outcome::Adopted {
                rule,
                mac: mac.clone(),
            },
            mac,
            predecessor,
            Vec::new(),
        ),
        Pick::Birth => match machines.rows.get(&own_pk) {
            // This uuid's own row, whose facts row a spent quota or a lost record left out.
            Some(row) if row.live() => (
                Outcome::Adopted {
                    rule: Rule::OwnBirth,
                    mac: own_pk.clone(),
                },
                own_pk.clone(),
                None,
                Vec::new(),
            ),
            // The owner ended this row: a birth never revives it.
            Some(_) => {
                findings.push(finding("own-machine-retired", MACHINE.name, vec![own_pk]));
                return Ok(Plan::quiet(Outcome::Held, findings));
            }
            None => {
                // A label is unique among live rows (§4.6), and a binding is for good: a birth
                // beside the row holding this label would leave its preferences on that row.
                let holders: Vec<String> = machines
                    .rows
                    .iter()
                    .filter(|(_, row)| row.live() && goes_by(row, label))
                    .map(|(pk, _)| pk.clone())
                    .collect();
                if !holders.is_empty() {
                    findings.push(finding("label-held", MACHINE.name, holders));
                    return Ok(Plan::quiet(Outcome::Held, findings));
                }
                (
                    Outcome::Born {
                        mac: own_pk.clone(),
                    },
                    own_pk.clone(),
                    None,
                    machine_birth(input, &machines, &own_pk, label)?,
                )
            }
        },
    };
    let row = stated(input, None, &mac, predecessor.as_deref())?;
    let mut changes = model_birth(cells, input.probe)?;
    changes.extend(births);
    changes.push(facts_change(&me.uuid, row, 1, today, input.now_ms));
    Ok(Plan {
        outcome,
        changes,
        findings,
    })
}

#[cfg(unix)]
pub use run::{Registered, Registrar};

#[cfg(unix)]
mod run {
    use super::{Finding, Outcome, Probe};
    use crate::{Error, Result, runtime::RepairRound};
    use astream_wire::sync::Digest;
    use aterm_secrets::client::Paths;
    use std::{
        collections::BTreeSet,
        path::PathBuf,
        sync::{Arc, Condvar, Mutex},
        thread,
        time::{Duration, Instant},
    };

    /// How long after one pass another waits, unless a round brings the first evidence that a
    /// peer holds the registry. A pass reads the host, so it is bounded; rounds, not a timer,
    /// wake it.
    pub(super) const PASS_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

    /// What one pass did: its outcome, the owner's findings, the records it appended.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Registered {
        pub outcome: Outcome,
        pub findings: Vec<Finding>,
        pub records: usize,
    }

    /// One pass's decisions, apart from the thread that waits for rounds.
    pub(super) struct Passes<G> {
        paths: Paths,
        config: PathBuf,
        gather: G,
        heard: bool,
        last: Option<Instant>,
    }

    impl<G: FnMut() -> Option<Probe>> Passes<G> {
        pub(super) fn new(paths: Paths, config: PathBuf, gather: G) -> Self {
            Self {
                paths,
                config,
                gather,
                heard: false,
                last: None,
            }
        }

        /// Run a pass when one is due at `now`, after rounds that left nothing to fetch of the
        /// tables in `drained`. `None` when none is, or when this Mac can state nothing.
        pub(super) fn turn(
            &mut self,
            drained: &BTreeSet<Digest>,
            now: Instant,
            now_ms: u64,
        ) -> Option<Result<Registered>> {
            let registry = super::super::Registry::at(self.paths.clone());
            let heard = !drained.is_empty()
                && registry
                    .table()
                    .ok()
                    .flatten()
                    .is_some_and(|table| drained.contains(&table));
            let news = heard && !self.heard;
            let rested = self
                .last
                .is_none_or(|last| now.saturating_duration_since(last) >= PASS_EVERY);
            if !news && !rested {
                return None;
            }
            let probe = (self.gather)()?;
            self.heard |= heard;
            self.last = Some(now);
            Some(registry.register(&self.config, &probe, self.heard, now_ms))
        }
    }

    #[derive(Default)]
    struct Wake {
        pending: bool,
        drained: BTreeSet<Digest>,
        closed: bool,
    }

    /// The Commons runtime's hook: [`Self::round`] takes each completed round from the owner
    /// thread and returns at once; a thread of its own reads the host and the registry. Dropped
    /// with the runtime, it ends that thread.
    pub struct Registrar {
        shared: Arc<(Mutex<Wake>, Condvar)>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl Registrar {
        /// Start the thread. `gather` reads this Mac (`None` where it cannot state itself), and
        /// `report` hears each pass.
        ///
        /// # Errors
        /// The thread could not start.
        pub fn start(
            paths: Paths,
            config: PathBuf,
            gather: impl FnMut() -> Option<Probe> + Send + 'static,
            report: impl Fn(&Result<Registered>) + Send + 'static,
        ) -> Result<Self> {
            let shared = Arc::new((Mutex::new(Wake::default()), Condvar::new()));
            let state_handle = shared.clone();
            let worker = thread::Builder::new()
                .name("alab-register".into())
                .spawn(move || {
                    let mut passes = Passes::new(paths, config, gather);
                    let mut drained = BTreeSet::new();
                    loop {
                        let (lock, wake) = &*state_handle;
                        let Ok(mut state) = lock.lock() else { return };
                        while !state.pending && !state.closed {
                            let Ok(next) = wake.wait(state) else { return };
                            state = next;
                        }
                        if state.closed {
                            return;
                        }
                        state.pending = false;
                        drained.append(&mut state.drained);
                        drop(state);
                        let now_ms = aterm_time::unix_millis().unwrap_or(0);
                        if now_ms == 0 {
                            continue;
                        }
                        if let Some(result) = passes.turn(&drained, Instant::now(), now_ms) {
                            report(&result);
                        }
                    }
                })
                .map_err(|_| Error::new("the registration thread could not start"))?;
            Ok(Self {
                shared,
                worker: Some(worker),
            })
        }

        /// Hear one completed repair round.
        pub fn round(&self, round: &RepairRound) {
            let (lock, wake) = &*self.shared;
            if let Ok(mut state) = lock.lock() {
                state.pending = true;
                state.drained.extend(round.drained_table());
                wake.notify_one();
            }
        }
    }

    impl Drop for Registrar {
        fn drop(&mut self) {
            let (lock, wake) = &*self.shared;
            if let Ok(mut state) = lock.lock() {
                state.closed = true;
                wake.notify_all();
            }
            if let Some(worker) = self.worker.take()
                && worker.thread().id() != thread::current().id()
            {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
