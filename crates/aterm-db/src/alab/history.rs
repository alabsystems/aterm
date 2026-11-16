// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a roll carries of a retired generation's history (docs/DESIGN-tables-app-2026-11-14.md
//! §4 item 6, tracker LT-36, decision LT-D12).
//!
//! A roll restates each register as one carried value and then releases this Mac's storage of
//! the old generation (`alab::roll`), so without this every cell's history would begin at the
//! roll and nothing could regenerate what was written before. Before the cover, a roll therefore
//! writes one `history` row ([`super::registry::HISTORY`]) for each entry of the old
//! generation's history this Mac holds and the new generation does not carry yet: the entries of
//! the old log itself, in its derived causal order, and the entries the old generation carried
//! from the one before it. An entry keeps what [`crate::att::Entry`] holds: the operation's
//! digest, its writer and the machine its roster bound then, its authored time, its derived
//! causal clock (which the new log cannot derive again), its request, what it reverted, and its
//! changes.
//!
//! **No secret value is carried.** A change to a `Secret` or `SecretRef` column, or one whose
//! value holds a sealed token anywhere (an earlier build sealed phone numbers too), is carried
//! with its key and no value ([`Written::withheld`]), and an entry carried on from an earlier
//! generation is judged again by the same rule: history shows that the write happened and which
//! Mac made it, and the seal and storage release that follow the roll delete the only copies of
//! the old tokens, as LT-04B requires. A signed record cannot be carried with a value taken out,
//! which is why an entry is the roller's restatement rather than the record itself.
//!
//! **Who may state history.** An entry counts only where its writer also wrote its own cover row
//! for some generation ([`super::roll::rollers`]): the same word a release already deletes data
//! on. A roll also counts its own Mac's rows, so a roll cut short after its history and before its
//! cover does not write that history again. Each row is keyed by the Mac that carried it and the
//! operation ([`keys::history_pk`]) and names that Mac, so two Macs carrying one operation write
//! two rows and no row ever holds two candidates. The first statement of an operation, in the
//! view's order, stands.
//!
//! **Bounded.** One roll writes at most [`MAX_HISTORY_BYTES`] of history rows, newest first, and
//! never more than its writer's room in the new generation leaves after the restatement, the
//! archived records, the cover and a reserve for the edits that follow ([`Budget::room`]). What
//! does not fit is named by one horizon row of that roller's: the entries ordered at or before it
//! were not carried, by design, and their cells keep only the carried value. A later roll that
//! leaves out more moves the horizon on. An entry whose values pass
//! [`super::registry::MAX_HISTORY_ENTRY`] is carried with its keys only ([`Entry::trimmed`]).
//!
//! **Release waits for it.** `alab::roll::covered` releases an old generation only once every
//! entry this Mac holds there is carried or behind a horizon ([`uncarried`]); where an older
//! build's roll carried no history, the storage stays until a roll on this Mac carries it, as a
//! late edit already does.

use super::registry::{ARCHIVED, HISTORY, MAX_HISTORY_ENTRY, ROLL};
use crate::{
    Error, Result,
    att::View,
    database::{Change, Command, MAX_WRITES},
    documents::log::{Hlc, Log},
    table::{
        Session,
        store::{WRITER_BYTES, WRITER_RECORDS},
    },
    typed::{
        self,
        cell::{self, CellKey, CellValue},
        keys, owned, secret,
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The most bytes of history rows one roll writes.
pub const MAX_HISTORY_BYTES: usize = 3 * 1024 * 1024;

/// What a roll leaves of its writer's room in the new generation for the edits that follow it.
const RESERVE_BYTES: usize = 2 * 1024 * 1024;
const RESERVE_RECORDS: usize = 512;

/// `Command::Write`'s fixed envelope around a record's changes (`att::pack`).
const WRITE_ENVELOPE: usize = 78;

/// The most a signed operation adds to its command's bytes.
const SIGNED: usize = crate::crypto::MAX_OPERATION - crate::crypto::MAX_BODY;

/// The form of an entry row's body.
const FORMAT: u64 = 1;

/// One carried entry of an earlier generation's history.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub operation: Digest,
    /// The generation whose log the operation is in.
    pub generation: u32,
    pub actor: Actor,
    /// The machine the record's roster bound its writer to, resolved when it was carried.
    pub machine: Option<[u8; 16]>,
    pub authored_ms: u64,
    /// The causal clock that generation's log derived for it.
    pub hlc: Hlc,
    pub request: Option<[u8; 16]>,
    pub reverts: Option<Digest>,
    pub changes: Vec<Written>,
    /// Its values passed what one entry carries: each change keeps its key and no value.
    pub trimmed: bool,
}

impl Entry {
    /// Its place in history order: causal clock, then digest.
    pub fn order(&self) -> (Hlc, Digest) {
        (self.hlc, self.operation)
    }
}

/// One change of a carried entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    pub collection: String,
    pub key: String,
    /// What it wrote; `None` where it cleared the cell, or where the value is not carried.
    pub value: Option<Value>,
    /// A secret's change: carried with no value.
    pub withheld: bool,
}

/// One roller's statement that the entries ordered at or before `through` were not carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Horizon {
    /// The generation that roller rolled from.
    pub generation: u32,
    /// The roller's machine, as a 32-digit key.
    pub by: String,
    /// The newest entry its rolls left out.
    pub through: (Hlc, Digest),
    /// How many entries its rolls left out.
    pub not_carried: u64,
}

/// The history a generation carries: each entry by operation, and each horizon by its row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Carried {
    pub entries: BTreeMap<Digest, Entry>,
    pub horizons: BTreeMap<String, Horizon>,
}

impl Carried {
    /// Whether an entry ordered at `order` is carried here or stated not carried.
    fn answers(&self, operation: &Digest, order: (Hlc, Digest)) -> bool {
        self.entries.contains_key(operation) || self.horizons.values().any(|h| order <= h.through)
    }

    /// One statement read from a row: an entry's first statement stands; of one roller's
    /// horizon, the one that reaches furthest.
    fn take(&mut self, pk: &str, stated: Stated) {
        match stated {
            Stated::Entry(entry) => {
                self.entries.entry(entry.operation).or_insert(entry);
            }
            Stated::Horizon(horizon) => match self.horizons.get(pk) {
                Some(held) if held.through >= horizon.through => {}
                _ => {
                    self.horizons.insert(pk.to_owned(), horizon);
                }
            },
        }
    }
}

/// Every entry of `log` (generation `generation` of `table`) a roll carries: each eligible edit
/// in derived causal order, except an automatic record past its writer's quota (`over`), which
/// never projected, and a roll's own records, which restate rather than edit.
///
/// # Errors
/// The log's causal clocks are incomplete or exhausted (`Log::into_causal_history`).
pub(crate) fn of_log(
    session: &Session<'_>,
    table: &typed::Table,
    generation: u32,
    log: Log,
    over: &BTreeSet<Digest>,
) -> Result<Vec<Entry>> {
    let resolve = owned::commons::historical(session);
    let mut out = Vec::new();
    for (operation, command, hlc) in log.into_causal_history()? {
        if over.contains(&operation.header.id) {
            continue;
        }
        let Some(changes) = command_changes(command) else {
            continue;
        };
        if restates(&changes) {
            continue;
        }
        out.push(Entry {
            operation: operation.header.id,
            generation,
            actor: operation.header.actor,
            machine: resolve(&operation.roster, &operation.header.actor),
            authored_ms: operation.authored_ms,
            hlc,
            request: operation.request,
            reverts: operation.reverts,
            changes: changes
                .into_iter()
                .map(|change| written(table, change))
                .collect(),
            trimmed: false,
        });
    }
    Ok(out)
}

fn command_changes(command: Command) -> Option<Vec<Change>> {
    command.into_changes().filter(|changes| !changes.is_empty())
}

/// A roll's own record: every change restates (a carried register), archives, covers, carries
/// history or publishes schema rows. It is no edit, so no history.
pub(crate) fn restates(changes: &[Change]) -> bool {
    changes.iter().all(|change| {
        [ARCHIVED.name, ROLL.name, HISTORY.name, typed::SCHEMA]
            .contains(&change.collection.as_str())
            || change
                .value
                .as_ref()
                .is_some_and(|value| cell::carried_refs(value).is_some())
    })
}

/// `change` as carried: its value withheld where it is a secret's.
fn written(table: &typed::Table, change: Change) -> Written {
    let withheld = secret_column(table, &change.collection, &change.key)
        || change.value.as_ref().is_some_and(holds_sealed);
    Written {
        collection: change.collection,
        key: change.key,
        value: if withheld { None } else { change.value },
        withheld,
    }
}

/// Whether `key` of `collection` is a `Secret` or `SecretRef` column of `table`.
fn secret_column(table: &typed::Table, collection: &str, key: &str) -> bool {
    CellKey::parse(key).ok().is_some_and(|key| {
        table
            .collection(collection)
            .and_then(|collection| collection.column(key.col))
            .is_some_and(|column| matches!(column.ty, typed::Ty::Secret | typed::Ty::SecretRef))
    })
}

/// `entry` judged again as a roll of `table` carries it: a change the earlier roll carried with
/// a value is withheld here where this table holds it secret or the value holds a sealed token.
fn withhold_again(table: &typed::Table, mut entry: Entry) -> Entry {
    for change in &mut entry.changes {
        if !change.withheld
            && (secret_column(table, &change.collection, &change.key)
                || change.value.as_ref().is_some_and(holds_sealed))
        {
            change.value = None;
            change.withheld = true;
        }
    }
    entry
}

/// Whether any string within `value` is a sealed token.
fn holds_sealed(value: &Value) -> bool {
    match value {
        Value::String(text) => secret::is_sealed(text),
        Value::Array(items) => items.iter().any(holds_sealed),
        Value::Object(fields) => fields.values().any(holds_sealed),
        _ => false,
    }
}

fn hex(bytes: &[u8]) -> Value {
    Value::String(aterm_codec::hex::encode_lower(bytes))
}

fn hex_or_null(bytes: Option<&[u8]>) -> Value {
    bytes.map_or(Value::Null, hex)
}

fn unhex<const N: usize>(value: Option<&Value>) -> Option<[u8; N]> {
    let text = value?.as_str()?;
    let mut out = [0u8; N];
    aterm_codec::hex::decode_into(text.as_bytes(), &mut out).then_some(out)
}

fn clock(hlc: Hlc) -> Value {
    json!([hlc.ms, hlc.counter])
}

fn unclock(value: Option<&Value>) -> Option<Hlc> {
    let items = value?.as_array()?;
    match items.as_slice() {
        [ms, counter] => Some(Hlc {
            ms: ms.as_u64()?,
            counter: counter.as_u64()?,
        }),
        _ => None,
    }
}

/// An entry's body as the Mac `by` carries it, with its values or, `trim`, only its keys.
fn body(entry: &Entry, by: &str, trim: bool) -> Value {
    let changes: Vec<Value> = entry
        .changes
        .iter()
        .map(|change| {
            let mut out = Map::new();
            out.insert("c".into(), Value::String(change.collection.clone()));
            out.insert("k".into(), Value::String(change.key.clone()));
            if change.withheld {
                out.insert("w".into(), Value::Bool(true));
            } else if !trim && let Some(value) = &change.value {
                out.insert("x".into(), value.clone());
            }
            Value::Object(out)
        })
        .collect();
    json!({
        "v": FORMAT,
        "by": by,
        "o": hex(&entry.operation),
        "g": entry.generation,
        "a": hex(&entry.actor),
        "m": hex_or_null(entry.machine.as_ref().map(|m| m.as_slice())),
        "t": entry.authored_ms,
        "h": clock(entry.hlc),
        "q": hex_or_null(entry.request.as_ref().map(|r| r.as_slice())),
        "r": hex_or_null(entry.reverts.as_ref().map(|r| r.as_slice())),
        "z": trim || entry.trimmed,
        "c": changes,
    })
}

fn entry_of(body: &Value) -> Option<Entry> {
    let trimmed = body.get("z")?.as_bool()?;
    let changes = body
        .get("c")?
        .as_array()?
        .iter()
        .map(|change| {
            let withheld = change.get("w").and_then(Value::as_bool).unwrap_or(false);
            Some(Written {
                collection: change.get("c")?.as_str()?.to_owned(),
                key: change.get("k")?.as_str()?.to_owned(),
                value: if withheld {
                    None
                } else {
                    change.get("x").cloned()
                },
                withheld,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Entry {
        operation: unhex(body.get("o"))?,
        generation: u32::try_from(body.get("g")?.as_u64()?).ok()?,
        actor: unhex(body.get("a"))?,
        machine: unhex(body.get("m")),
        authored_ms: body.get("t")?.as_u64()?,
        hlc: unclock(body.get("h"))?,
        request: unhex(body.get("q")),
        reverts: unhex(body.get("r")),
        changes,
        trimmed,
    })
}

fn horizon_body(horizon: &Horizon) -> Value {
    json!({
        "v": FORMAT,
        "horizon": {
            "g": horizon.generation,
            "by": horizon.by,
            "h": clock(horizon.through.0),
            "o": hex(&horizon.through.1),
            "n": horizon.not_carried,
        },
    })
}

fn horizon_of(body: &Value) -> Option<Horizon> {
    let horizon = body.get("horizon")?;
    Some(Horizon {
        generation: u32::try_from(horizon.get("g")?.as_u64()?).ok()?,
        by: horizon.get("by")?.as_str()?.to_owned(),
        through: (unclock(horizon.get("h"))?, unhex(horizon.get("o"))?),
        not_carried: horizon.get("n")?.as_u64()?,
    })
}

/// What one history row states.
#[derive(Debug, PartialEq)]
enum Stated {
    Entry(Entry),
    Horizon(Horizon),
}

/// What the row `pk` states in `body`, where its key is the one its statement makes: an entry
/// under the key of the Mac it names and its operation, a horizon under the key of its roller
/// and generation. `writer` is the Mac that wrote the row as it stands, where that is known: an
/// entry it states must name that Mac. An earlier build's roll restating the row as a carried
/// register passes `None`, since the row then still names the roller that carried it.
fn stated(pk: &str, body: &Value, writer: Option<&str>) -> Option<Stated> {
    if body.get("v").and_then(Value::as_u64) != Some(FORMAT) {
        return None;
    }
    if let Some(horizon) = horizon_of(body) {
        return (keys::history_horizon_pk(horizon.generation, &horizon.by) == pk)
            .then_some(Stated::Horizon(horizon));
    }
    let by = body.get("by")?.as_str()?;
    if writer.is_some_and(|writer| writer != by) {
        return None;
    }
    entry_of(body)
        .filter(|entry| keys::history_pk(by, &entry.operation) == pk)
        .map(Stated::Entry)
}

/// One history row's cells: `body` as the sealed-typed `entry` cell, and the row's lifecycle.
fn row(pk: &str, body: &Value) -> Result<Vec<Change>> {
    let bytes =
        aterm_json::to_vec(body).map_err(|_| Error::new("a history entry does not encode"))?;
    if bytes.len() > MAX_HISTORY_ENTRY {
        return Err(Error::new("a history entry passes what one row carries"));
    }
    let cell = |col: &str, x: Value| {
        cell::change(
            &HISTORY,
            CellKey {
                pk,
                col,
                elem: None,
            },
            &CellValue::owner(HISTORY.version, x),
        )
    };
    Ok(vec![
        cell(
            "entry",
            Value::String(aterm_secrets::b64::encode_padded(&bytes)),
        )?,
        cell("row", Value::from("live"))?,
    ])
}

/// An entry's row as the Mac `by` carries it, its values dropped where they pass what one row
/// carries.
fn entry_row(entry: &Entry, by: &str) -> Result<Vec<Change>> {
    let pk = keys::history_pk(by, &entry.operation);
    row(&pk, &body(entry, by, false)).or_else(|_| row(&pk, &body(entry, by, true)))
}

/// How much of its writer's room in the new generation one roll's history may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Budget {
    pub(crate) bytes: usize,
    pub(crate) records: usize,
}

impl Budget {
    /// The most one roll writes.
    pub(crate) const MOST: Self = Self {
        bytes: MAX_HISTORY_BYTES,
        records: WRITER_RECORDS,
    };

    /// What is left for history once this Mac's writer holds `records` and `bytes` in the new
    /// generation, the roll's own restatement, archive and cover counted in: the room under
    /// the writer's share less a reserve for the edits that follow, at most [`Self::MOST`].
    /// `None` where the roll alone passes the writer's share.
    pub(crate) fn room(records: usize, bytes: usize) -> Option<Self> {
        let records = WRITER_RECORDS.checked_sub(records)?;
        let bytes = WRITER_BYTES.checked_sub(bytes)?;
        Some(Self {
            bytes: bytes.saturating_sub(RESERVE_BYTES).min(Self::MOST.bytes),
            records: records.saturating_sub(RESERVE_RECORDS),
        })
    }
}

/// Rows charged as `att::pack` packs them into records: each record's envelope and signature,
/// and each row's changes.
#[derive(Debug)]
struct Meter {
    budget: Budget,
    bytes: usize,
    records: usize,
    /// The last record's changes and body bytes.
    changes: usize,
    body: usize,
}

impl Meter {
    fn new(budget: Budget) -> Self {
        Self {
            budget,
            bytes: 0,
            records: 0,
            changes: 0,
            body: 0,
        }
    }

    /// Charge `row`, or nothing and `false` where it passes what is left.
    fn take(&mut self, row: &[Change]) -> Result<bool> {
        let json = row_bytes(row)?;
        let joins = self.records > 0
            && self.changes + row.len() <= MAX_WRITES
            && self.body + json <= crate::crypto::MAX_BODY;
        let next = if joins {
            (
                self.records,
                self.bytes + json,
                self.changes + row.len(),
                self.body + json,
            )
        } else {
            (
                self.records + 1,
                self.bytes + WRITE_ENVELOPE + SIGNED + json,
                row.len(),
                WRITE_ENVELOPE + json,
            )
        };
        if next.0 > self.budget.records || next.1 > self.budget.bytes {
            return Ok(false);
        }
        (self.records, self.bytes, self.changes, self.body) = next;
        Ok(true)
    }
}

/// The records and bytes `rows` take of a writer's share once appended.
///
/// # Errors
/// A row cannot be encoded.
pub(crate) fn appended(rows: &[Vec<Change>]) -> Result<(usize, usize)> {
    let mut meter = Meter::new(Budget {
        bytes: usize::MAX,
        records: usize::MAX,
    });
    for row in rows {
        meter.take(row)?;
    }
    Ok((meter.records, meter.bytes))
}

/// What one roll writes of history.
#[derive(Debug, Default)]
pub(crate) struct Planned {
    pub(crate) rows: Vec<Vec<Change>>,
    /// Entries carried by these rows.
    pub(crate) carried: usize,
    /// Entries left behind this roll's horizon.
    pub(crate) not_carried: usize,
}

/// The history rows a roll of `table` by the Mac `uuid` from generation `from` writes: each
/// entry of `owed` that `target` neither carries nor states not carried, judged again for
/// secrets, and each horizon of `stated` that reaches past what `target` holds of it, newest
/// first within `budget`. What does not fit is named by this roller's horizon row, moved on past
/// what an earlier roll of its left out; the horizon goes in whatever the budget, since nothing
/// else says those entries are not carried.
///
/// # Errors
/// A row cannot be encoded.
pub(crate) fn plan(
    table: &typed::Table,
    owed: impl IntoIterator<Item = Entry>,
    stated: &BTreeMap<String, Horizon>,
    target: &Carried,
    from: u32,
    uuid: &str,
    budget: Budget,
) -> Result<Planned> {
    let mut pending: BTreeMap<(Hlc, Digest), Entry> = BTreeMap::new();
    for entry in owed {
        if !target.answers(&entry.operation, entry.order()) {
            pending
                .entry(entry.order())
                .or_insert_with(|| withhold_again(table, entry));
        }
    }
    let mut out = Planned::default();
    let mut meter = Meter::new(budget);
    for (pk, horizon) in stated {
        if target
            .horizons
            .get(pk)
            .is_none_or(|held| held.through < horizon.through)
        {
            let row = row(pk, &horizon_body(horizon))?;
            meter.take(&row)?;
            out.rows.push(row);
        }
    }
    let mut entries = pending.into_values().rev();
    let mut through = None;
    for entry in entries.by_ref() {
        let row = entry_row(&entry, uuid)?;
        if !meter.take(&row)? {
            through = Some(entry.order());
            break;
        }
        out.rows.push(row);
        out.carried += 1;
    }
    if let Some(through) = through {
        // The first entry that did not fit and every older one are left out, past whatever this
        // roller's earlier roll left out.
        out.not_carried = 1 + entries.count();
        let pk = keys::history_horizon_pk(from, uuid);
        let before = target.horizons.get(&pk).map_or(0, |held| held.not_carried);
        let horizon = Horizon {
            generation: from,
            by: uuid.to_owned(),
            through,
            not_carried: before + out.not_carried as u64,
        };
        out.rows.push(row(&pk, &horizon_body(&horizon))?);
    }
    Ok(out)
}

fn row_bytes(row: &[Change]) -> Result<usize> {
    aterm_json::to_vec(row)
        .map(|bytes| bytes.len())
        .map_err(|_| Error::new("a history row does not encode"))
}

/// The history `view` carries: every `history` row a roller wrote ([`super::roll::rollers`],
/// `resolve` binding each record's writer to its machine), and those `also` wrote (a roll counts
/// its own Mac's rows), read from each eligible operation rather than the cells' current values,
/// so a later write over a row cannot hide it. A row is read only under the key its statement
/// makes ([`stated`]). A row an earlier build's roll restated as a carried register is read
/// through it.
pub fn carried(
    view: &View,
    resolve: impl Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    also: Option<Actor>,
) -> Carried {
    let mut trusted = super::roll::rollers(view, &resolve);
    trusted.extend(also);
    let mut out = Carried::default();
    for op in view
        .ops
        .iter()
        .filter(|op| !op.revert && !view.quota.over.contains(&op.id) && trusted.contains(&op.actor))
    {
        let writer = resolve(&op.roster, &op.actor).map(|machine| keys::uuid32(&machine));
        for change in op
            .changes
            .iter()
            .filter(|change| change.collection == HISTORY.name)
        {
            let Ok(key) = CellKey::parse(&change.key) else {
                continue;
            };
            if key.col != "entry" || key.elem.is_some() {
                continue;
            }
            let Some(value) = &change.value else {
                continue;
            };
            let (values, by): (Vec<Value>, Option<&str>) = match cell::carried(value) {
                Some(items) => (items.into_iter().map(|(_, value)| value).collect(), None),
                None => {
                    // A row as its roller wrote it names that roller; one whose writer this
                    // Mac cannot bind to a machine states nothing.
                    let Some(writer) = writer.as_deref() else {
                        continue;
                    };
                    (vec![value.clone()], Some(writer))
                }
            };
            for value in values {
                let Some(body) = CellValue::decode(&value)
                    .ok()
                    .and_then(|value| value.x.as_str().map(str::to_owned))
                    .and_then(|text| aterm_secrets::b64::decode_padded(&text))
                    .and_then(|bytes| aterm_json::from_slice::<Value>(&bytes).ok())
                else {
                    continue;
                };
                if let Some(statement) = stated(key.pk, &body, by) {
                    out.take(key.pk, statement);
                }
            }
        }
    }
    out
}

/// What a roll of an old generation owes history, as this Mac holds it: the entries of its own
/// log and those it carried from the generation before, and the horizons it carried.
///
/// # Errors
/// As [`of_log`].
pub(crate) fn owed(
    session: &Session<'_>,
    table: &typed::Table,
    generation: u32,
    log: Log,
    view: &View,
) -> Result<(Vec<Entry>, BTreeMap<String, Horizon>)> {
    let mut entries = of_log(session, table, generation, log, &view.quota.over)?;
    let earlier = carried(view, owned::commons::historical(session), None);
    entries.extend(earlier.entries.into_values());
    Ok((entries, earlier.horizons))
}

/// Each entry of `owed` that `carried` neither carries nor states not carried: what keeps a
/// Mac's storage of the old generation until a roll here carries it.
pub(crate) fn uncarried<'a>(owed: &'a [Entry], carried: &Carried) -> Vec<&'a Entry> {
    owed.iter()
        .filter(|entry| !carried.answers(&entry.operation, entry.order()))
        .collect()
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
