// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-registration (docs/DESIGN-alab-lab-model-2026-11-01.md §4.6, §4.7, §6): the facts a Mac
//! states about itself at its own Commons machine uuid, the one binding from an enrollment to a
//! machine row, chosen once by the adoption rule. [`plan`] is pure over the registry this Mac
//! holds; `Registry::register` runs it under the writer and [`Registrar`] runs that when the
//! Commons runtime completes repair rounds, so a Mac alone on its LAN writes nothing and no timer
//! wakes anything.
//!
//! A Mac's identity here is its Commons uuid, its fleet node and its membership label. A writer
//! succession gives it a new uuid and a new seat: the earlier writers of its verified lineage
//! ([`Me::former`]) are this Mac too, so it keeps the machine row and the label they held. A row a
//! build before that birthed for a writer of its chain is folded back into it by the Mac's own
//! facts row ([`fold`]), and then closed into it ([`close`]); a writer of the chain still bound to
//! it follows the close ([`followed`]). The IOPlatformUUID stays in this process: only a keyed
//! digest of it ([`keys::hw_hint`]) is written. The machine rows a Mac writes are its own birth,
//! the row it is bound to (its node once, its number), and a birth of its own chain that its facts
//! folded. A hint match without the §6 evidence is a finding, never an adoption, because VM clones
//! share a machine identifier.

use super::{
    checks::Finding,
    registry::{MACHINE, MACHINE_FACTS, MODEL, PREFERENCE, bound_machine, held_machines},
};
use crate::{
    Error, Result,
    database::Change,
    typed::{
        Collection, LIFECYCLE, MAX_PK,
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

/// What this machine reads about itself. No `Debug`: it holds the IOPlatformUUID (a Linux
/// machine's `/etc/machine-id`), which never leaves the process (`keys::hw_hint`).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// The machine's model as read: a Mac's `sysctl hw.model` (`Mac16,8`), a Linux machine's
    /// DMI product name or device-tree model ([`Probe::linux`]); [`model_key`] makes its key.
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
    /// The key-derived node principal, 16 lower-case hex digits without `n-`.
    pub node: Option<String>,
    /// The identity the keyed hint is made of: a Mac's IOPlatformUUID, a Linux machine's
    /// `/etc/machine-id`.
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
    /// The membership log's label, clash suffix included: this writer's own seat's. A successor
    /// goes by an earlier seat's where [`Me::former`] gives one ([`resolved`]).
    pub label: Option<String>,
    /// The label derived from a holder-signed number choice, its fallback during a conflict
    /// included. Never an OS-name hint.
    pub chosen: Option<String>,
    /// [`keys::hw_hint`] of this Mac's hardware under this network.
    pub hw_hint: String,
    /// When the vault's roster says this Mac was admitted, where it says.
    pub enrolled_ms: Option<u64>,
    /// The earlier writers of this enrollment's verified writer lineage, oldest first. A writer
    /// succession gives a Mac a new uuid and seat, and the lineage proves one home held them all:
    /// the same Mac, unless the home was copied to other hardware ([`resolved`]).
    pub former: Vec<Former>,
}

/// An earlier writer in an enrollment's writer lineage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Former {
    /// Its Commons machine uuid, 32 hex.
    pub uuid: String,
    /// Its seat's membership label, where the vault still seats it.
    pub label: Option<String>,
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
    /// 0: the machine an earlier writer of this enrollment's own lineage is bound to.
    Succession,
    /// 1: an earlier enrollment's row with this hint, with the reinstall evidence.
    Reinstall,
    /// 2: the machine row names this Mac's fleet node.
    Node,
    /// 2: the machine row names one of this Mac's host names.
    Host,
    /// 3: a planned machine under this Mac's membership label or an alias, or an unbound sheet
    /// row answering its signed chosen number.
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
    /// A machine row a writer of this Mac's chain birthed before rule 0, which the chain's facts
    /// fold into `into`, the machine this Mac is, was closed into it ([`close`]); the record also
    /// carries the facts row where it was due.
    Closed { mac: String, into: String },
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
            Self::Closed { mac, .. } => ("closes", Some(mac)),
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

/// A `model` row's key from a machine's model as read ([`Probe::hw_model`]): each run of
/// characters outside the key grammar (`,`, a space, `.`, `(`, anything not ASCII) becomes one
/// `-`, and none leads or closes it. Case is kept, so a Mac's key is what it always was
/// (`Mac16,8` becomes `Mac16-8`) and a Linux machine's name reads as written (`Precision 5570`
/// becomes `Precision-5570`). A mapped name longer than the grammar ([`MAX_PK`]) keeps its first
/// characters and closes with `-` and six hex digits of its SHA-256, so two models that differ
/// only past the cut (`Raspberry Pi Compute Module 4 Rev 1.0` and `… 5 …`) are two rows, never
/// one model row described for both.
///
/// # Errors
/// Nothing in the grammar is left.
pub fn model_key(hw_model: &str) -> Result<String> {
    /// `-` and the hex digits that close a long name's key.
    const TAIL: usize = 1 + 6;
    let mut key = String::new();
    for c in hw_model.trim().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' {
            c
        } else {
            '-'
        };
        if c != '-' || !(key.is_empty() || key.ends_with('-')) {
            key.push(c);
        }
    }
    while key.ends_with('-') {
        key.pop();
    }
    if key.len() > MAX_PK {
        let digest = aterm_digest::Sha256::digest(key.as_bytes());
        key.truncate(MAX_PK - TAIL);
        while key.ends_with('-') {
            key.pop();
        }
        key.push('-');
        aterm_codec::hex::push_lower(&mut key, &digest[..3]);
    }
    if pk_grammar(&key) {
        Ok(key)
    } else {
        Err(Error::new("the machine's model makes no model key"))
    }
}

/// Bytes as decimal gigabytes, rounded, the way a disk is sold.
pub const fn gigabytes(bytes: u64) -> u64 {
    bytes.saturating_add(500_000_000) / 1_000_000_000
}

/// The installed memory, whole GiB, that a Linux kernel's `MemTotal` (KiB) stands for. The kernel
/// counts what is left after its page map (1/64 of memory with 4 KiB pages), the firmware's
/// reserve and a crash kernel's, so 64 GiB reads about 62.8 and a plain rounding up says 63. It
/// rounds up to a step of a quarter of the power of two at or below it, at least 1 GiB: 16 to 32
/// GiB in steps of 4, 32 to 64 in steps of 8, which holds every size modules sum to (24, 48, 96
/// and 192 included). Memory the firmware gives a GPU is no part of `MemTotal`, and nothing an
/// unprivileged process reads states it.
#[must_use]
pub const fn installed_gib(mem_total_kib: u64) -> u64 {
    const GIB_KIB: u64 = 1 << 20;
    if mem_total_kib == 0 {
        return 0;
    }
    let power = 1_u64 << mem_total_kib.ilog2();
    let step = if power / 4 > GIB_KIB {
        power / 4
    } else {
        GIB_KIB
    };
    mem_total_kib.div_ceil(step).saturating_mul(step) / GIB_KIB
}

impl Probe {
    /// A Linux machine's readings under `root` ([`aterm_sysprobe::linux::Root::system`] on the
    /// machine, a fixture tree in tests): its model ([`aterm_sysprobe::linux::Root::product_model`]),
    /// chip, physical cores, memory, root volume, the host names its owner set, and
    /// `/etc/machine-id` as the identity the keyed hint is made of ([`keys::hw_hint`], exactly as
    /// a Mac's IOPlatformUUID), which stays in this process. `None` where it states no model or no
    /// machine id, which its row needs, as a Mac with no IOPlatformUUID states nothing. A Linux
    /// machine has no GPU cores or OS major to state, and no fleet node until its caller adds one.
    ///
    /// A reinstall writes a new machine id, so a reinstalled Linux machine births a new machine
    /// row the owner merges (`merged_into`), where a reinstalled Mac is adopted by its hint.
    ///
    /// Memory is the installed amount `MemTotal` stands for ([`installed_gib`]).
    #[must_use]
    pub fn linux(root: &aterm_sysprobe::linux::Root) -> Option<Self> {
        let platform_uuid = root.machine_id()?;
        let hw_model = root.product_model()?;
        let (cores_p, cores_e) = root.core_counts();
        Some(Self {
            hw_model,
            chip: root.chip(),
            cores_p,
            cores_e,
            gpu_cores: None,
            ram_gb: root.mem_total_kib().map(installed_gib),
            disk_gb: root.volume_bytes().map(gigabytes),
            arch: Some(std::env::consts::ARCH.to_owned()),
            os_major: None,
            hosts: root.owner_host_names(),
            node: None,
            platform_uuid,
        })
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
pub(super) fn same_host(a: &str, b: &str) -> bool {
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
    label: Option<&str>,
) -> Result<BTreeMap<&'static str, Value>> {
    let probe = input.probe;
    let hosts = hosts_cell(&probe.hosts);
    let readings: [(&'static str, Option<Value>); 13] = [
        ("mac", Some(Value::from(mac))),
        ("predecessor", predecessor.map(Value::from)),
        (
            "label",
            label.or(input.me.label.as_deref()).map(Value::from),
        ),
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
    // An owner merge's acknowledgment (`merged_into`) is this Mac's own word: carried here, and
    // stated only by the own-row branch's [`fold`] or [`followed`] close.
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

/// Only the enrollment adopting this row publishes its node. An owner's value or conflict
/// stays authoritative; a later pass never overwrites it or spends another record on it.
fn own_node(
    input: &Input<'_>,
    (machine, machines): (&Collection, &Projection),
    mac: &str,
) -> Result<Vec<Change>> {
    let Some(node) = input.probe.node.as_deref().filter(|node| {
        node.len() == 16
            && node
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) else {
        return Ok(Vec::new());
    };
    if machines
        .rows
        .get(mac)
        .is_some_and(|row| row.fields.contains_key("node"))
        || machines.rows.iter().any(|(pk, row)| {
            pk != mac && row.live() && row.get("node").and_then(Value::as_str) == Some(node)
        })
    {
        return Ok(Vec::new());
    }
    Ok(vec![automatic(machine, mac, "node", Value::from(node))?])
}

/// Follow a signed choice, or replace an automatic label that lost its claim. A nodeless
/// sheet row may share a signed choice until `merge` folds it in.
fn number_change(
    input: &Input<'_>,
    (machine, machines): (&Collection, &Projection),
    facts: &OwnedRead,
    mac: &str,
) -> Result<Vec<Change>> {
    let Some(label) = input.me.chosen.as_deref() else {
        if machines.rows.get(mac).is_some_and(|row| {
            row.claims.get("label").is_some_and(|canonical| {
                canonical.as_ref() != row.fields.get("label").and_then(|field| field.value())
            }) && row.fields.get("label").is_some_and(|field| {
                !field.conflicted() && field.candidates().iter().all(|candidate| candidate.value.a)
            })
        }) {
            return Ok(vec![automatic(
                machine,
                mac,
                "label",
                Value::from(birth_label(input, machines)?),
            )?]);
        }
        return Ok(Vec::new());
    };
    if machines.rows.get(mac).is_some_and(|row| {
        row.claims
            .get("label")
            .and_then(Option::as_ref)
            .and_then(Value::as_str)
            == Some(label)
    }) {
        return Ok(Vec::new());
    }
    let bound = bound_elsewhere(input, facts);
    if machines.rows.iter().any(|(pk, row)| {
        pk != mac
            && row.live()
            && (goes_by(row, label)
                || row.fields.get("label").is_some_and(|field| {
                    field
                        .candidates_x()
                        .iter()
                        .any(|value| value.as_str() == Some(label))
                }))
            && (bound.contains(pk.as_str())
                || ["node", "fleet_node"]
                    .iter()
                    .any(|col| row.get(col).is_some()))
    }) {
        return Err(Error::new(
            "the chosen number belongs to another bound machine",
        ));
    }
    if machines
        .rows
        .get(mac)
        .is_some_and(|row| row.raw.contains("label"))
    {
        return Err(Error::new("the bound machine label needs a newer reader"));
    }
    if machines
        .rows
        .get(mac)
        .and_then(|row| row.get("label"))
        .and_then(Value::as_str)
        == Some(label)
    {
        return Ok(Vec::new());
    }
    Ok(vec![automatic(machine, mac, "label", Value::from(label))?])
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
/// row still states what it read. `machine` is the `machine` collection as the generation written
/// declares it, so its hosts go in the form that generation holds them ([`cell::texts_change`]).
/// Its `model` is left out where `models` holds that model's row and it is not live.
fn machine_birth(
    input: &Input<'_>,
    (machine, machines): (&Collection, &Projection),
    models: &Projection,
    pk: &str,
    label: &str,
) -> Result<Vec<Change>> {
    let probe = input.probe;
    let live_rows = || machines.rows.values().filter(|row| row.live());
    let node_named =
        |node: &str| live_rows().any(|row| row.get("node").and_then(Value::as_str) == Some(node));
    let host_named = |host: &str| {
        live_rows().any(|row| {
            cell::texts(row.fields.get("hosts"))
                .iter()
                .any(|held| same_host(held, host))
        })
    };
    let mut changes = vec![
        automatic(machine, pk, "label", Value::from(label))?,
        automatic(machine, pk, "status", Value::from("active"))?,
    ];
    // A model row the owner retired is left out: a live machine born naming it would be refused
    // (`typed::check::names_not_live`), and with it every later pass that plans the birth again.
    let model = model_key(&probe.hw_model)?;
    if !models.rows.get(&model).is_some_and(|row| !row.live()) {
        changes.push(automatic(machine, pk, "model", Value::from(model))?);
    }
    changes.push(automatic(machine, pk, LIFECYCLE, Value::from("live"))?);
    if let Some(node) = probe
        .node
        .as_deref()
        .filter(|node| aterm_codec::hex::is_lower(node, 16) && !node_named(node))
    {
        changes.push(automatic(machine, pk, "node", Value::from(node))?);
    }
    let mut hosts = hosts_cell(&probe.hosts);
    hosts.retain(|host| !host_named(host));
    changes.extend(cell::texts_change(
        machine,
        pk,
        "hosts",
        &[],
        &hosts,
        |x| CellValue::automatic(machine.version, x),
    )?);
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

/// Registry rows reserve labels, including planned and retired rows. Log names are hints,
/// never a second label allocator. Prefer this Mac's local hostname tag when it is free.
fn birth_label(input: &Input<'_>, machines: &Projection) -> Result<String> {
    let held = |label: &str| {
        machines.rows.values().any(|row| {
            goes_by(row, label)
                || row.fields.get("label").is_some_and(|field| {
                    field
                        .candidates()
                        .iter()
                        .any(|c| c.value.x.as_str() == Some(label))
                })
        })
    };
    if let Some(chosen) = input.me.chosen.as_deref() {
        if held(chosen) {
            return Err(Error::new("the chosen number already names a registry row"));
        }
        return Ok(chosen.to_owned());
    }
    if let Some(label) = input
        .probe
        .hosts
        .iter()
        .find_map(|host| aterm_secrets::swarm_roster::leading_m_label(host).map(|(_, label)| label))
        .filter(|label| !held(label))
    {
        return Ok(label);
    }
    use std::fmt::Write as _;
    let mut label = String::with_capacity(8);
    for number in 1..=aterm_types::machine_name::Number::MAX {
        label.clear();
        let _ = write!(label, "m{number}");
        if !held(&label) {
            return Ok(label);
        }
    }
    Err(Error::new("machine labels exhausted"))
}

/// The machines live enrollments other than this one hold: each one's binding, and the machine
/// its own `merged_into` moved it to ([`held_machines`]).
fn bound_elsewhere<'a>(input: &Input<'_>, facts: &'a OwnedRead) -> BTreeSet<&'a str> {
    facts
        .rows
        .iter()
        .filter(|(uuid, _)| **uuid != input.me.uuid && input.live.contains(uuid.as_str()))
        .flat_map(|(_, row)| held_machines(&row.value))
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
    // later enrollment of the same machine was seen more recently. A row counts for the machine
    // its Mac is, a merged or folded one's for the machine it moved to.
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
        let Some(mac) = bound_machine(&row.value) else {
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

/// This enrollment as the rule reads it, and what it found on the way. The lineage proves one
/// home held every writer in it, and a home can be copied to another Mac (a migration, a restored
/// backup, a cloned disk), where the copy's writer succeeds the original's by itself. So an
/// earlier writer whose facts row states other hardware than this Mac's is another Mac's: the
/// newest such is a finding. A writer whose row states this Mac's hardware is this Mac's wherever
/// it sits, a home that came back to its first Mac included. One that never registered states no
/// hardware: it is this Mac's where no writer states other hardware, and nobody's where one
/// does, since nothing then says which Mac the home was on when it ran. The Mac goes by the
/// first seat's label among its own writers, unless it signed a number: a successor is seated
/// under a new label while the seat it succeeded stands, and the Mac's machine row was planned
/// under the earlier one.
///
/// Only rule 0 and the label read this. A copy is a stranger to the rules after it as any Mac
/// is: while the original's uuid is seated its machine is bound elsewhere, and once that seat
/// is gone a copy carrying the original's fleet node adopts the row by rule 2, as a Mac that
/// replaced another does.
fn resolved(me: &Me, facts: &OwnedRead) -> (Me, Vec<Finding>) {
    let stated = |writer: &Former| {
        facts
            .rows
            .get(&writer.uuid)
            .and_then(|row| text(&row.value, "hw_hint"))
    };
    let cut = me
        .former
        .iter()
        .rposition(|writer| stated(writer).is_some_and(|hint| hint != me.hw_hint));
    let former: Vec<Former> = me
        .former
        .iter()
        .filter(|writer| match stated(writer) {
            Some(hint) => hint == me.hw_hint,
            None => cut.is_none(),
        })
        .cloned()
        .collect();
    let findings = cut
        .map(|at| {
            finding(
                "succession-other-hardware",
                MACHINE_FACTS.name,
                vec![me.uuid.clone(), me.former[at].uuid.clone()],
            )
        })
        .into_iter()
        .collect();
    let seat = me
        .chosen
        .is_none()
        .then(|| former.iter().find_map(|writer| writer.label.clone()))
        .flatten();
    (
        Me {
            label: seat.or_else(|| me.label.clone()),
            former,
            ..me.clone()
        },
        findings,
    )
}

/// Rule 0: the machine the newest earlier writer of this enrollment's lineage is ([`bound_machine`]:
/// the one its own `merged_into` names, else its binding), with that writer's uuid. The lineage,
/// less the writers [`resolved`] found on other hardware, is the proof a
/// reinstall's hint and dates stand in for, so nothing waits 30 days and the earlier uuid may
/// still be seated. `Err` names a machine row the owner ended: the binding is for good, exactly
/// as for this uuid's own row.
fn succession(
    input: &Input<'_>,
    machines: &Projection,
    facts: &OwnedRead,
    found: &mut Vec<Finding>,
) -> std::result::Result<Option<(String, String)>, ()> {
    newest_bound(&input.me.former, None, machines, facts, found)
}

/// Rule 0 over `writers` (oldest first): the machine the newest of them whose facts row binds a
/// machine the registry has is, with that writer's uuid, passing over a writer bound to `skip`.
/// A writer bound to a birth of its own [`chain`] that a close ended is the machine the close
/// names ([`followed`]), so a successor that makes its first row after the close is not held.
fn newest_bound(
    writers: &[Former],
    skip: Option<&str>,
    machines: &Projection,
    facts: &OwnedRead,
    found: &mut Vec<Finding>,
) -> std::result::Result<Option<(String, String)>, ()> {
    for uuid in writers.iter().rev().map(|writer| &writer.uuid) {
        let Some(row) = facts.rows.get(uuid) else {
            continue;
        };
        let Some(mac) = bound_machine(&row.value).filter(|mac| Some(*mac) != skip) else {
            continue;
        };
        return match machines.rows.get(mac) {
            Some(machine) if machine.live() => Ok(Some((mac.to_owned(), uuid.clone()))),
            Some(_) => {
                let writers = chain(uuid, text(&row.value, "predecessor"), facts);
                if let Some(into) = followed(machines, facts, &writers, mac) {
                    return Ok(Some((into.to_owned(), uuid.clone())));
                }
                found.push(finding(
                    "bound-machine-retired",
                    MACHINE.name,
                    vec![mac.to_owned()],
                ));
                Err(())
            }
            None => continue,
        };
    }
    Ok(None)
}

/// This Mac's chain of enrollments: `uuid`, then each uuid reached through `predecessor`, the
/// first being `predecessor` (the one the row this pass states names) and each after it the one
/// that uuid's facts row names. Every facts row counts, a removed seat's included, and the walk
/// stops at a uuid it passed. Each link is the Mac's own word, signed by the writer whose row
/// states it, so the chain needs no lineage read.
fn chain(uuid: &str, predecessor: Option<&str>, facts: &OwnedRead) -> Vec<String> {
    let mut chain = vec![uuid.to_owned()];
    let mut next = predecessor.map(str::to_owned);
    while let Some(at) = next.take() {
        if chain.contains(&at) {
            break;
        }
        next = facts
            .rows
            .get(&at)
            .and_then(|row| text(&row.value, "predecessor"))
            .map(str::to_owned);
        chain.push(at);
    }
    chain
}

/// The machine `mac` was closed into ([`close`]), for a facts row whose [`chain`] is `writers`
/// and that is `mac` ([`bound_machine`]): where `mac` is the birth of a writer `W` of `writers`
/// and is not live, its `row` says `merged` and its `merged_into` names another live machine,
/// each an automatic cell alone (a close's; an owner's or an import's word there is theirs), and
/// a facts row whose own chain holds `W` states that fold (`mac` the birth, `merged_into` that
/// machine): the signed word the close mirrored, which [`close`] requires before it writes.
///
/// Such a row follows the fold rather than prove it again. It is a writer of the chain that
/// still is the birth: one on a build before the fold, one that adopted the birth by rule 0 while
/// another writer closed it, a copy of the home that made its first row after the close, or a
/// later writer whose own pre-fix birth an earlier fold merged into this one before it closed.
/// The proof's evidence, the birth's record and the lineage, can be gone after a roll or out of
/// reach on this pass, and a hold would leave the Mac with no machine. Following decides nothing
/// the close did not, so it needs no heard pass: a word a partial history hides can only be the
/// owner's, and an owner's end of that machine holds the Mac there as it holds it on the birth.
fn followed<'a>(
    machines: &'a Projection,
    facts: &OwnedRead,
    writers: &[String],
    mac: &str,
) -> Option<&'a str> {
    let born = |writer: &String| keys::machine_pk(writer) == mac;
    if !writers.iter().any(born) {
        return None;
    }
    let row = machines.rows.get(mac).filter(|row| !row.live())?;
    let into = automatic_word(row, "merged_into")
        .filter(|into| *into != mac && machines.rows.get(*into).is_some_and(Row::live))?;
    let signed = facts.rows.iter().any(|(uuid, other)| {
        text(&other.value, "mac") == Some(mac)
            && text(&other.value, "merged_into") == Some(into)
            && chain(uuid, text(&other.value, "predecessor"), facts)
                .iter()
                .any(born)
    });
    (automatic_word(row, LIFECYCLE) == Some("merged") && signed).then_some(into)
}

/// `column`'s word in `row` where only automatic paths wrote it: one value, no import source.
fn automatic_word<'r>(row: &'r Row, column: &str) -> Option<&'r str> {
    row.fields
        .get(column)
        .filter(|field| {
            !field.conflicted()
                && field
                    .candidates()
                    .iter()
                    .all(|candidate| candidate.value.a && candidate.value.s.is_none())
        })
        .and_then(cell::Field::value)
        .and_then(Value::as_str)
}

/// A row a writer of this Mac's chain birthed where rule 0 would have bound that writer to the
/// machine an earlier writer is, had the build that wrote it had rule 0 (0.199.0 on m2,
/// 2026-11-13: a second machine row under a label another Mac's row also held, while the earlier
/// writer's row named m2): that machine and the earlier writer's uuid. The Mac folds the birth
/// into that machine by its own facts row (§6: only a Mac's own facts row rebinds it), and
/// [`close`] then ends the birth's machine row.
///
/// Only where the own row binds `mac`, the birth of a writer `W` of its [`chain`] (this uuid's
/// own, or an earlier writer's that this uuid adopted by rule 0 before any fold), and states no
/// `merged_into`. Rule 0 is taken over the verified lineage's writers older than `W`, passing over
/// one bound to `mac`; an earlier `W` the lineage does not name folds nothing. The record that
/// birthed `W`'s row must have had that writer's facts row in its causal past (`saw`, from
/// [`birth_saw`]), so the birth is one rule 0 would have prevented, never one made before the
/// earlier writer registered (a VM clone's successor, whose birth §6 keeps for good and `check`
/// names as a clone). And the pass is heard, since the fold binds for good and a partial history
/// can show a machine the owner ended as live, the reason a first row waits. A row the Mac
/// adopted that is no chain writer's birth and a merge it already acknowledged are never moved,
/// and a home with no earlier writer bound elsewhere folds nothing. A birth a close already ended
/// needs no proof here: the row follows the close ([`followed`]).
fn fold(
    input: &Input<'_>,
    machines: &Projection,
    facts: &OwnedRead,
    (mac, own): (&str, &Value),
    saw: &dyn Fn(&str, &str) -> bool,
) -> Option<(String, String)> {
    let me = input.me;
    if !input.heard || own.get("merged_into").is_some_and(|into| !into.is_null()) {
        return None;
    }
    let chain = chain(&me.uuid, text(own, "predecessor"), facts);
    let born = chain
        .iter()
        .find(|writer| keys::machine_pk(writer) == mac)?;
    let older = if *born == me.uuid {
        &me.former[..]
    } else {
        &me.former[..me.former.iter().position(|writer| writer.uuid == *born)?]
    };
    newest_bound(older, Some(mac), machines, facts, &mut Vec::new())
        .ok()
        .flatten()
        .filter(|(into, writer)| into != mac && saw(born, writer))
}

/// The record that first stated `uuid`'s facts row: its `n` 1, binding `uuid`'s own birth, signed
/// by the writer that signed the row `facts` holds. A record outside this generation's history (a
/// roll carried it) or past the quota is none.
fn birth_record(
    view: &crate::typed::View,
    facts: &OwnedRead,
    uuid: &str,
) -> Option<astream_wire::sync::Digest> {
    let own_pk = keys::machine_pk(uuid);
    let own = facts.rows.get(uuid)?;
    view.ops
        .iter()
        .filter(|op| op.actor == own.actor && !view.quota.over.contains(&op.id))
        .filter_map(|op| {
            op.changes
                .iter()
                .find(|c| c.collection == MACHINE_FACTS.name && c.key == uuid)
                .and_then(|c| c.value.as_ref())
                .map(|row| (op, row))
        })
        .min_by_key(|(op, _)| op.sequence)
        .filter(|(_, row)| {
            row.get("n").and_then(Value::as_u64) == Some(1)
                && text(row, "mac") == Some(own_pk.as_str())
        })
        .map(|(op, _)| op.id)
}

/// Whether the record that birthed `born`'s row ([`birth_record`]) had the facts row `facts`
/// holds for `writer` in its causal past: the evidence that rule 0 would have bound that birth to
/// `writer`'s machine instead ([`fold`]). Each birth's record is found once.
fn birth_saw<'a>(
    view: &'a crate::typed::View,
    facts: &'a OwnedRead,
) -> impl Fn(&str, &str) -> bool + 'a {
    let births =
        std::cell::RefCell::new(BTreeMap::<String, Option<astream_wire::sync::Digest>>::new());
    move |born: &str, writer: &str| {
        let birth = *births
            .borrow_mut()
            .entry(born.to_owned())
            .or_insert_with(|| birth_record(view, facts, born));
        birth.is_some_and(|birth| {
            facts
                .rows
                .get(writer)
                .is_some_and(|row| view.causal.observes(&birth, &row.operation))
        })
    }
}

/// Whether a pass over `view` could fold `uuid`'s own row ([`fold`]), so its lineage is worth
/// reading although the row is bound: a heard pass, the row binds the birth of a writer of its
/// [`chain`] and states no `merged_into`, and the record that birthed that row saw the facts row
/// of another enrollment that states `hw_hint` and is a live machine other than that birth. No
/// other row can be the writer rule 0 names, since [`resolved`] drops an earlier writer whose row
/// states other hardware. Its one reader is the store's plan, which builds only on unix.
#[cfg(unix)]
pub(super) fn may_fold(view: &crate::typed::View, uuid: &str, hw_hint: &str, heard: bool) -> bool {
    let Some(facts) = view.owned.get(MACHINE_FACTS.name) else {
        return false;
    };
    let Some(own) = facts.rows.get(uuid) else {
        return false;
    };
    let Some(mac) = text(&own.value, "mac") else {
        return false;
    };
    if !heard
        || own
            .value
            .get("merged_into")
            .is_some_and(|into| !into.is_null())
    {
        return false;
    }
    let chain = chain(uuid, text(&own.value, "predecessor"), facts);
    let Some(born) = chain.iter().find(|writer| keys::machine_pk(writer) == mac) else {
        return false;
    };
    let Some(birth) = birth_record(view, facts, born) else {
        return false;
    };
    let machines = view.project(&MACHINE);
    facts.rows.iter().any(|(writer, row)| {
        writer != uuid
            && writer != born
            && text(&row.value, "hw_hint") == Some(hw_hint)
            && bound_machine(&row.value).is_some_and(|bound| {
                bound != mac && machines.rows.get(bound).is_some_and(Row::live)
            })
            && view.causal.observes(&birth, &row.operation)
    })
}

/// What [`close`] writes: `mac`, a birth of this Mac's chain, closed into `into`.
struct Closing {
    mac: String,
    into: String,
    changes: Vec<Change>,
}

/// Whether only automatic paths wrote `row`: every candidate of every register and set element
/// is automatic and names no import source, and no column is one this build cannot read.
fn automatic_only(row: &Row) -> bool {
    let automatic = |field: &cell::Field| {
        field
            .candidates()
            .iter()
            .all(|candidate| candidate.value.a && candidate.value.s.is_none())
    };
    row.raw.is_empty()
        && row.fields.values().all(|field| match field {
            cell::Field::Set(elements) => elements.values().all(automatic),
            register => automatic(register),
        })
}

/// The machine row a writer of this Mac's [`chain`] birthed before rule 0, closed into the machine
/// this Mac is (`into`, [`bound_machine`] of `row`, the facts row this pass states) once a facts
/// row of the chain folded it there ([`fold`]): its `merged_into` names `into` and its `row` says
/// `merged`, the end state `aterm db alab merge` writes for a duplicate, as automatic cells. The
/// machine cells mirror what the chain's own signed facts state; every reader already skips a row
/// that is not live, so the birth no longer contends for its label, and the label stays reserved.
///
/// A candidate is `keys::machine_pk(W)` for a writer `W` of the chain, other than `into`, that
/// `row`, or the facts row of `W` or of a writer of the chain after it, binds while stating
/// `merged_into` `into`: the chain's own signed fold, whichever of its writers stated it (one
/// that folded and then moved on, its close held, leaves it to the next). That is the word
/// [`followed`] reads: a facts row that still is the birth ([`bound_machine`]) and whose own chain
/// reaches `W` (an earlier writer on a build before the fold, a writer that adopted the birth by
/// rule 0 while this one closed it, a copy of the home, a writer whose own birth an earlier fold
/// merged into it) follows the close into `into` instead of being held at
/// `bound-machine-retired`. It closes only on a heard pass, since a
/// partial history can hide an owner's word, a preference or a seat on it; only into a live
/// machine; and only where the row is live, no live enrollment that could not follow it holds it
/// ([`held_machines`]: one outside the chain whose own chain does not reach `W`, which the close
/// would strand), and only automatic paths wrote it ([`automatic_only`]). Such a seat skips the
/// birth until it ends, with no finding of its own (`checks::facts` names the two rows binding one
/// machine, `facts-bind-one-machine`). An owner's or an import's word on it holds the close for
/// good (`folded-birth-held`), and so does a `merged_into` naming another machine
/// (`folded-birth-disputed`, with it). A preference that may be live on it holds the close
/// (`folded-birth-held`, with its keys) and nothing is moved: the owner ends the preference and
/// the next pass closes. The first candidate along the chain that passes is closed.
///
/// # Errors
/// A cell this Mac would write breaks its column's rules.
fn close(
    input: &Input<'_>,
    cells: &Cells,
    (collection, machines): (&Collection, &Projection),
    facts: &OwnedRead,
    row: &BTreeMap<&'static str, Value>,
    into: &str,
    found: &mut Vec<Finding>,
) -> Result<Option<Closing>> {
    if !input.heard || !machines.rows.get(into).is_some_and(Row::live) {
        return Ok(None);
    }
    let stated = |column: &str| row.get(column).and_then(Value::as_str);
    let ours = chain(&input.me.uuid, stated("predecessor"), facts);
    let mut preferences = None;
    for (at, writer) in ours.iter().enumerate() {
        let mac = keys::machine_pk(writer);
        let folds = |binds: Option<&str>, merged: Option<&str>| {
            binds == Some(mac.as_str()) && merged == Some(into)
        };
        // `W` and each later writer of the chain, whose own chain reaches `W`.
        let named = folds(stated("mac"), stated("merged_into"))
            || ours[..=at].iter().any(|signer| {
                facts.rows.get(signer).is_some_and(|own| {
                    folds(text(&own.value, "mac"), text(&own.value, "merged_into"))
                })
            });
        if mac == into || !named {
            continue;
        }
        let Some(machine) = machines.rows.get(&mac).filter(|machine| machine.live()) else {
            continue;
        };
        if facts.rows.iter().any(|(uuid, other)| {
            input.live.contains(uuid)
                && !ours.contains(uuid)
                && held_machines(&other.value).contains(&mac.as_str())
                && !chain(uuid, text(&other.value, "predecessor"), facts).contains(writer)
        }) {
            continue;
        }
        let merged: Vec<&Value> = machine
            .fields
            .get("merged_into")
            .map(|field| field.candidates_x())
            .unwrap_or_default();
        let elsewhere: Vec<String> = merged
            .iter()
            .filter_map(|x| x.as_str())
            .filter(|x| *x != into)
            .map(str::to_owned)
            .collect();
        if !elsewhere.is_empty() {
            found.push(finding(
                "folded-birth-disputed",
                MACHINE.name,
                std::iter::once(mac).chain(elsewhere).collect(),
            ));
            continue;
        }
        // A `merged_into` that names no machine key is no word this Mac can mirror.
        if !automatic_only(machine) || merged.iter().any(|x| !x.is_null() && x.as_str().is_none()) {
            found.push(finding("folded-birth-held", MACHINE.name, vec![mac]));
            continue;
        }
        let preferences = preferences.get_or_insert_with(|| cell::project(&PREFERENCE, cells));
        let preferred: Vec<String> = preferences
            .rows
            .iter()
            .filter(|(_, preference)| {
                preference.fields.get("machine").is_some_and(|field| {
                    field
                        .candidates_x()
                        .iter()
                        .any(|x| x.as_str() == Some(mac.as_str()))
                }) && !preference.fields.get(LIFECYCLE).is_some_and(|field| {
                    !field.conflicted()
                        && field
                            .value()
                            .and_then(Value::as_str)
                            .is_some_and(|word| word != "live")
                })
            })
            .map(|(pk, _)| pk.clone())
            .collect();
        if !preferred.is_empty() {
            found.push(finding(
                "folded-birth-held",
                MACHINE.name,
                std::iter::once(mac).chain(preferred).collect(),
            ));
            continue;
        }
        let mut changes = Vec::new();
        if machine.get("merged_into").and_then(Value::as_str) != Some(into) {
            changes.push(automatic(
                collection,
                &mac,
                "merged_into",
                Value::from(into),
            )?);
        }
        changes.push(automatic(
            collection,
            &mac,
            LIFECYCLE,
            Value::from("merged"),
        )?);
        return Ok(Some(Closing {
            mac,
            into: into.to_owned(),
            changes,
        }));
    }
    Ok(None)
}

/// What adoption picks: rule 0, then rules 1 to 3 of §6, in order, then 4.
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
    match succession(input, machines, facts, found) {
        Err(()) => return Pick::Held,
        Ok(Some((mac, predecessor))) => {
            return Pick::Machine {
                rule: Rule::Succession,
                mac,
                predecessor: Some(predecessor),
            };
        }
        Ok(None) => {}
    }
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
        row.get("node").and_then(Value::as_str)
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
                && cell::texts(row.fields.get("hosts"))
                    .iter()
                    .any(|host| probe.hosts.iter().any(|mine| same_host(mine, host)))
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
                    && (row.get("status").and_then(Value::as_str) == Some("planned")
                        || me.chosen.as_deref() == Some(label))
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
/// The machine's model makes no model key, all machine numbers are reserved, or a cell this Mac would
/// write breaks its column's rules.
pub fn plan(input: &Input<'_>, view: &crate::typed::View) -> Result<Plan> {
    if !view.complete {
        return Ok(Plan::quiet(Outcome::Incomplete, Vec::new()));
    }
    let empty = OwnedRead::default();
    let facts = view.owned.get(MACHINE_FACTS.name).unwrap_or(&empty);
    let saw = birth_saw(view, facts);
    plan_projected(
        input,
        &view.cells,
        facts,
        (view.collection(&MACHINE), &view.project(&MACHINE)),
        &saw,
    )
}

/// [`plan`] over projected rows; `saw` is [`birth_saw`].
fn plan_projected(
    input: &Input<'_>,
    cells: &Cells,
    facts: &OwnedRead,
    (machine, machines): (&Collection, &Projection),
    saw: &dyn Fn(&str, &str) -> bool,
) -> Result<Plan> {
    let (me, mut findings) = resolved(input.me, facts);
    let input = &Input { me: &me, ..*input };
    let me = input.me;
    let today = day_of(input.now_ms);
    if let Some(own) = facts.rows.get(&me.uuid)
        && let Some(mac) = text(&own.value, "mac")
    {
        let folds = fold(input, machines, facts, (mac, &own.value), saw);
        // A birth of this Mac's chain that a close ended while this row still is it (its binding
        // unfolded, or the `merged_into` an earlier fold named): the Mac follows the chain's
        // signed fold where it cannot prove the fold now.
        let follows = if folds.is_none() {
            let writers = chain(&me.uuid, text(&own.value, "predecessor"), facts);
            followed(
                machines,
                facts,
                &writers,
                bound_machine(&own.value).unwrap_or(mac),
            )
        } else {
            None
        };
        let into = folds.as_ref().map(|(into, _)| into.as_str()).or(follows);
        // The machine this Mac is: the one its own facts row merged it into, else its binding.
        let bound = into
            .or_else(|| text(&own.value, "merged_into"))
            .unwrap_or(mac);
        // A binding is for good, so a machine row the owner ended holds this Mac rather than
        // have it restate facts under a row nobody uses any more; only the owner moves it.
        if machines.rows.get(bound).is_some_and(|row| !row.live()) {
            return Ok(Plan::quiet(
                Outcome::Held,
                vec![finding(
                    "bound-machine-retired",
                    MACHINE.name,
                    vec![bound.to_owned()],
                )],
            ));
        }
        let seen = own.value.get("seen").and_then(Value::as_u64).unwrap_or(0);
        let mut node = own_node(input, (machine, machines), bound)?;
        let renamed = number_change(input, (machine, machines), facts, bound)?;
        let moves = !renamed.is_empty();
        let moved_label = renamed
            .iter()
            .find(|change| change.key == format!("{bound}.label"))
            .and_then(|change| change.value.as_ref())
            .and_then(|value| CellValue::decode(value).ok())
            .and_then(|value| value.x.as_str().map(str::to_owned));
        node.extend(renamed);
        let label = moved_label
            .as_deref()
            .or(input.me.chosen.as_deref())
            .or_else(|| {
                machines
                    .rows
                    .get(bound)
                    .filter(|row| {
                        row.fields
                            .get("label")
                            .is_some_and(|field| !field.conflicted())
                    })
                    .and_then(|row| row.get("label"))
                    .and_then(Value::as_str)
            })
            // The machine's label is in conflict: the row keeps the one it states. This
            // enrollment's seat is no evidence here, since a successor's is a later label for
            // the same Mac and its lineage is read only until it is bound.
            .or_else(|| own.value.get("label").and_then(Value::as_str));
        let predecessor = folds
            .as_ref()
            .filter(|_| own.value.get("predecessor").is_none_or(Value::is_null))
            .map(|(_, predecessor)| predecessor.as_str());
        let mut row = stated(input, Some(&own.value), mac, predecessor, label)?;
        if let Some(into) = into {
            row.insert("merged_into", Value::from(into));
        }
        // A birth of this Mac's chain that its facts fold into `bound` is closed into it, in the
        // record that states the fold or alone in a later one.
        let mut held = Vec::new();
        let (outcome, closes) = match close(
            input,
            cells,
            (machine, machines),
            facts,
            &row,
            bound,
            &mut held,
        )? {
            Some(Closing { mac, into, changes }) => (Outcome::Closed { mac, into }, changes),
            None => (
                Outcome::Refreshed {
                    mac: bound.to_owned(),
                },
                Vec::new(),
            ),
        };
        // Host names move without the Mac changing (a Bonjour clash renames the local host), and a
        // change spends a record of a lifetime quota, so they ride the 30-day rewrite. The other
        // fields move at most once a day; a fold or a follow says which Mac this is and goes out
        // with the pass that decides it.
        let changed = MACHINE_FACTS
            .columns
            .iter()
            .filter(|c| !matches!(c.name, "n" | "seen" | "at" | "hosts"))
            .any(|c| row.get(c.name) != own.value.get(c.name));
        let chosen_changed = input
            .me
            .chosen
            .as_deref()
            .is_some_and(|label| own.value.get("label").and_then(Value::as_str) != Some(label));
        let due = into.is_some()
            || moves
            || chosen_changed
            || seen.saturating_add(REFRESH_DAYS) <= today
            || (changed && seen < today);
        if !due && node.is_empty() && closes.is_empty() {
            return Ok(Plan::quiet(Outcome::Unchanged, held));
        }
        if !due {
            node.extend(closes);
            return Ok(Plan {
                outcome,
                changes: node,
                findings: held,
            });
        }
        let n = own
            .value
            .get("n")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(1);
        let mut changes = model_birth(cells, input.probe)?;
        changes.extend(node);
        changes.extend(closes);
        changes.push(facts_change(
            &me.uuid,
            row,
            n,
            today.max(seen),
            input.now_ms,
        ));
        return Ok(Plan {
            outcome,
            changes,
            findings: held,
        });
    }
    // A first row binds this enrollment for good, and a partial history can show a machine row
    // without the facts row that binds it, so every first row waits for the round, not only a
    // birth (design §6, as built).
    if !input.heard {
        return Ok(Plan::quiet(Outcome::Deferred, findings));
    }
    let own_pk = keys::machine_pk(&me.uuid);
    let (outcome, mac, predecessor, births) = match pick(input, machines, facts, &mut findings) {
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
                let label = birth_label(input, machines)?;
                (
                    Outcome::Born {
                        mac: own_pk.clone(),
                    },
                    own_pk.clone(),
                    None,
                    machine_birth(
                        input,
                        (machine, machines),
                        &cell::project(&MODEL, cells),
                        &own_pk,
                        &label,
                    )?,
                )
            }
        },
    };
    let label = births
        .iter()
        .find(|change| change.collection == MACHINE.name && change.key.ends_with(".label"))
        .and_then(|change| change.value.as_ref())
        .and_then(|value| value.get("x"))
        .and_then(Value::as_str)
        .or_else(|| {
            machines
                .rows
                .get(&mac)
                .filter(|row| {
                    row.fields
                        .get("label")
                        .is_some_and(|field| !field.conflicted())
                })
                .and_then(|row| row.get("label"))
                .and_then(Value::as_str)
        });
    let label = match label {
        Some(label) => label.to_owned(),
        None => birth_label(input, machines)?,
    };
    let label = input.me.chosen.as_deref().unwrap_or(&label);
    let row = stated(input, None, &mac, predecessor.as_deref(), Some(label))?;
    let mut changes = model_birth(cells, input.probe)?;
    if births.is_empty() {
        changes.extend(own_node(input, (machine, machines), &mac)?);
        changes.extend(number_change(input, (machine, machines), facts, &mac)?);
        if input.me.chosen.is_none()
            && machines
                .rows
                .get(&mac)
                .is_some_and(|row| !row.fields.contains_key("label"))
        {
            changes.push(automatic(machine, &mac, "label", Value::from(label))?);
        }
    }
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

    /// How long after a pass that decided something another waits, unless a round brings the
    /// first evidence that a peer holds the registry. A pass reads the host, so it is bounded;
    /// rounds, not a timer, wake it.
    pub(super) const PASS_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

    /// How long after a pass that decided nothing the next waits, at first: one that failed, or
    /// that found this Mac's history of the registry incomplete. Either is more often a passing
    /// state than a standing one (a writer succession still pending, a table that moved under
    /// the read, a record still on its way), and resting [`PASS_EVERY`] on it leaves a Mac out
    /// of the registry for six hours behind one unlucky read. Each one after another doubles the
    /// rest ([`rest_after`]), so a standing one (a Mac that is no Writer) settles at
    /// [`PASS_EVERY`], where it was.
    pub(super) const RETRY_EVERY: Duration = Duration::from_secs(5 * 60);

    /// The rest after `undecided` passes in a row that decided nothing: [`PASS_EVERY`] after a
    /// pass that decided, else [`RETRY_EVERY`] doubled for each one after the first, never more
    /// than [`PASS_EVERY`].
    pub(super) fn rest_after(undecided: u32) -> Duration {
        match undecided {
            0 => PASS_EVERY,
            n => RETRY_EVERY
                .saturating_mul(1u32.checked_shl(n - 1).unwrap_or(u32::MAX))
                .min(PASS_EVERY),
        }
    }

    /// What one pass did: its outcome, the owner's findings, the records it appended.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Registered {
        pub outcome: Outcome,
        pub findings: Vec<Finding>,
        pub records: usize,
    }

    /// One pass's decisions, apart from the thread that waits for rounds.
    pub(super) struct Passes<G, T> {
        paths: Paths,
        config: PathBuf,
        gather: G,
        table: T,
        heard: bool,
        last: Option<Instant>,
        /// The passes in a row that decided nothing ([`rest_after`]).
        undecided: u32,
    }

    /// The registry's table here, as [`Passes`] reads it: [`super::super::Registry::table`].
    pub(super) fn table_at(paths: &Paths) -> impl FnMut() -> Option<Digest> + Send + 'static {
        let registry = super::super::Registry::at(paths.clone());
        move || registry.table().ok().flatten()
    }

    impl<G: FnMut() -> Option<Probe>, T: FnMut() -> Option<Digest>> Passes<G, T> {
        /// `gather` reads this Mac, and `table` the registry's table here ([`table_at`]).
        pub(super) fn new(paths: Paths, config: PathBuf, gather: G, table: T) -> Self {
            Self {
                paths,
                config,
                gather,
                table,
                heard: false,
                last: None,
                undecided: 0,
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
            let news = news(self.heard, drained, &mut self.table);
            let rest = rest_after(self.undecided);
            let rested = self
                .last
                .is_none_or(|last| now.saturating_duration_since(last) >= rest);
            if !news && !rested {
                return None;
            }
            let probe = (self.gather)()?;
            self.heard |= news;
            self.last = Some(now);
            let registry = super::super::Registry::at(self.paths.clone());
            let pass = registry.register(&self.config, &probe, self.heard, now_ms);
            let decided = pass
                .as_ref()
                .is_ok_and(|done| done.outcome != Outcome::Incomplete);
            self.undecided = if decided {
                0
            } else {
                self.undecided.saturating_add(1)
            };
            Some(pass)
        }
    }

    /// Whether `drained` brings the first evidence that a peer holds the registry: none was
    /// `heard` before, and the registry's table here (`table`) is one of `drained`. `table` reads
    /// custody and the catalog, so it is read only while no evidence was heard and a round
    /// drained a table.
    pub(super) fn news(
        heard: bool,
        drained: &BTreeSet<Digest>,
        table: impl FnOnce() -> Option<Digest>,
    ) -> bool {
        !heard && !drained.is_empty() && table().is_some_and(|table| drained.contains(&table))
    }

    /// The table `round` drained that may be the registry's: none where its report names that
    /// table's scope ([`crate::replica::Report::scope`]) and the scope is none of the registry's
    /// generations ([`super::super::Registry::is_generation`]), which then needs no read to say.
    pub(super) fn evidence(round: &RepairRound) -> Option<Digest> {
        let table = round.drained_table()?;
        let scope = round.application.as_ref().and_then(|report| report.scope);
        scope
            .is_none_or(|scope| {
                scope.digest() != table || super::super::Registry::is_generation(scope)
            })
            .then_some(table)
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
            let table = table_at(&paths);
            Self::start_with(paths, config, gather, table, report)
        }

        /// [`Self::start`], its passes reading the registry's table with `table`.
        pub(super) fn start_with(
            paths: Paths,
            config: PathBuf,
            gather: impl FnMut() -> Option<Probe> + Send + 'static,
            table: impl FnMut() -> Option<Digest> + Send + 'static,
            report: impl Fn(&Result<Registered>) + Send + 'static,
        ) -> Result<Self> {
            let shared = Arc::new((Mutex::new(Wake::default()), Condvar::new()));
            let state_handle = shared.clone();
            let worker = thread::Builder::new()
                .name("alab-register".into())
                .spawn(move || {
                    let mut passes = Passes::new(paths, config, gather, table);
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

        /// Hear one completed repair round, and hand on the table it drained that may be the
        /// registry's ([`evidence`]).
        pub fn round(&self, round: &RepairRound) {
            let drained = evidence(round);
            let (lock, wake) = &*self.shared;
            if let Ok(mut state) = lock.lock() {
                state.pending = true;
                state.drained.extend(drained);
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
