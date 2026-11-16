// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rolling the registry to its next generation (docs/DESIGN-alab-lab-model-2026-11-01.md §3.8,
//! §3.9, §11 step 13): `aterm db alab roll [--apply]`, which `check` offers once a writer's quota
//! is spent or the table holds 75 % of the store's records or bytes.
//!
//! A roll is an owner verb, run on one Mac:
//!
//! 1. **Restate.** Every register of the old generation goes into the new one as one carried
//!    value (`typed::cell::carried`), listing each candidate it holds there with the operation
//!    that wrote it, so an open conflict stays open and the provisional value reads the same.
//!    A collection or column this build does not know is carried as it is. The schema rows are
//!    the compiled ones, published into the new generation; the old generation's cover rows are
//!    not carried. Past-quota automatic records stay inert: they never project, so nothing of
//!    them is restated, and the new generation gives every writer its whole quota again.
//!    No table holds a secret (docs/PLAN.md LT-04): an earlier build sealed a password or a phone
//!    number into its cell as a `sec1:` token under the generation's audience key, and the roll
//!    is what takes each one out. A candidate holding such a token, in any column, is opened
//!    through the old generation's sealer (`att::sealer`) at its coordinates: a `SecretRef`
//!    column's value is put into this Mac's secret store (`alab::vault::Vault`) under the entry
//!    the candidate's origin names (`typed::secret::moved_entry`, so every Mac that rolls the
//!    candidate names and puts the same one; equal values of one register share the first) and
//!    the candidate carries the reference; a `Phone` column's carries the number's digits, which
//!    are ordinary data. A token in any other column, or
//!    one that does not open, stops the roll, naming its coordinates and never a value. No token
//!    and no plaintext reaches the new generation. The new generation is a table scope of its
//!    own, so its audience key opens nothing of the old one; the old generation's records still
//!    hold the sealed tokens until its seal and the storage release (step 5) delete this Mac's
//!    storage of it.
//! 2. **Archive.** Each owned row a reader projects (machine facts, mailbox observations) is
//!    carried as the signed record that wrote it (`registry::ARCHIVED`), still sealed under the
//!    old table's scope, so its signer and operation digest are its own: a reader authenticates
//!    it and applies the owned rule exactly as before (`att::carried`). A Mac's own row in
//!    the new generation outranks its archived one.
//! 3. **History.** Each entry of the old generation's history this Mac holds and the new one
//!    does not carry yet goes in as a `history` row, a secret's value withheld, newest first
//!    within a bound (`alab::history`): without it every cell's history would begin at the
//!    roll once step 5 deletes the old storage.
//! 4. **Cover.** One `roll` row per (generation, Mac) says which records of each writer this
//!    restatement carried: for each writer, its last record before the first one this build
//!    cannot read. A record that only projects nothing (a merge, a past-quota record, a cut-off
//!    writer's) counts as carried.
//!    A cover counts only as the Mac its key names wrote it (`covers`).
//! 5. **Seal** the old generation (`catalog::Directory::seal`): readers move to the new one. The
//!    seal waits until every current Writer has declared the new generation in its own name
//!    (`endorse`, `catalog::Directory::undeclared`), which only a build that compiles it does,
//!    since an older build cannot read the registry once the old generation is sealed; the
//!    roll reports the Macs it waits for, and running it again once they have seals.
//! 6. **Release** this Mac's storage of the old generation where the covers carry everything
//!    it holds there and the history rows every entry of it (`catalog::release`).
//!
//! Every restated register change has a request id derived from its content and the target's
//! observed eligible business history. An unchanged re-run writes nothing; an exact retry
//! returns its first receipt. A record
//! that reached a Mac late (a Mac on the other side of a split edited the old generation before
//! it heard the seal) is carried by the roll run again on that Mac. New candidates (`late`)
//! merge with what the new generation holds; candidates the old history shows superseded are
//! removed even when the surviving candidate was already carried. Previously carried values
//! that the new generation replaced never return. An owner's edit made
//! in the new generation meanwhile stays, beside the late value, as a conflict for the owner,
//! since neither saw the other. Until that Mac's roll runs, its storage of the old generation
//! stays.

use super::{
    history,
    registry::{APPLICATION, ARCHIVED, HISTORY, ROLL},
    store::Registry,
};
use crate::{
    Error, Result,
    att::{self, Compiled, View, decode, pack},
    catalog::{
        Directory,
        release::{Released, Retired},
    },
    database::{Change, Command},
    documents::{application, log::Log},
    table::{Session, store::Store},
    typed::{
        self,
        cell::{self, CellKey, CellValue, Cells, Projection},
        keys, owned,
        secret::{self, Sealer, Secret},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_json::{Value, json};
use aterm_secrets::commons::bootstrap::Bootstrap;
use std::collections::{BTreeMap, BTreeSet};

/// The collections a roll does not restate as registers: the schema rows (the new generation
/// takes the compiled ones), the old generation's own cover rows, and its history rows, which a
/// roll writes again as history (`alab::history`).
const NOT_RESTATED: [&str; 3] = [typed::SCHEMA, ROLL.name, HISTORY.name];

/// The origins carried at each collection and key, including ones a later edit replaced.
pub(crate) type Carried = BTreeMap<(String, String), BTreeSet<Digest>>;

/// Every carried origin at its register. One operation can write several registers, so carrying
/// one cell does not mark the operation's other cells carried after an interrupted roll.
pub(crate) fn carried_origins<'a>(ops: impl IntoIterator<Item = &'a Command>) -> Carried {
    let mut carried = Carried::new();
    for change in ops.into_iter().filter_map(Command::changes).flatten() {
        if let Some(items) = change.value.as_ref().and_then(cell::carried_refs) {
            carried
                .entry((change.collection.clone(), change.key.clone()))
                .or_default()
                .extend(items.into_iter().map(|(operation, _)| operation));
        }
    }
    carried
}

/// Bind a batch's request identities to what its plan observed. Reapplying identical contents
/// after a later write is a new reconciliation, not a retry of an earlier receipt.
fn request_role(role: &str, log: &Log) -> String {
    let mut sha = Sha256::new();
    sha.update(b"aterm/alab/roll/request-basis\0");
    for (operation, _) in log.eligible_iter() {
        sha.update(operation.header.id);
    }
    format!(
        "{role}:roll:{}",
        aterm_codec::hex::encode_lower(&sha.finalize())
    )
}

/// What a restatement writes: the register changes, each secret-store entry the references
/// among them name with the value it must hold, put only by a roll that applies
/// (`Registry::roll`) before any change that names it is written, and each entry a candidate the
/// restatement drops named (`drops`), owed removal by that roll so a sweep collects it once no
/// cell names it.
#[derive(Default)]
pub(crate) struct Restated {
    pub(crate) rows: Vec<Vec<Change>>,
    pub(crate) puts: Vec<(String, Secret)>,
    pub(crate) drops: Vec<String>,
}

/// The new generation's sealer, for a roll into a generation builds up to 0.200.0 still read
/// ([`typed::Ty::Secret`]): a value is resealed for it, and a token it holds that opens there is
/// carried already.
pub(crate) type Reseal<'a> = &'a dyn Sealer;

/// Store entries a restatement chose, each with the value it holds.
type Moving = Vec<(String, Secret)>;

/// What a roll's restatement reaches secrets through: the old generation's sealer (to open), the
/// new one's [`Reseal`] where older builds still read it, and the secret store.
pub(crate) type Secrets<'a> = (&'a dyn Sealer, Option<Reseal<'a>>, &'a dyn secret::Store);

/// [`restate_with_claims`] with no claim provenance to carry.
#[cfg(test)]
pub(crate) fn restate(
    (from, table): (&typed::Table, &typed::Table),
    source: &Cells,
    held: &BTreeSet<Digest>,
    target: &Cells,
    carried: &Carried,
    secrets: Secrets<'_>,
) -> Result<Restated> {
    restate_with_claims(
        (from, table),
        source,
        held,
        target,
        carried,
        secrets,
        &typed::claim::CarriedClaims::new(),
    )
}

/// The registers to write into the new generation (`target`, whose rolls already carried
/// `carried`) from the old one's `source` cells, `held` being every operation this Mac holds of
/// the old one, `from` the old generation's declaration and `table` the new one's. A list
/// column the new generation holds as a text set is restated as its members ([`as_members`]),
/// each element a register of its own; every other register keeps its key. A register carries
/// candidates not carried before, retains ones the new generation still holds, and drops ones
/// this Mac's old history shows superseded. A previously carried candidate absent from the new
/// generation stays absent: a later edit replaced it. An unchanged result writes nothing; an
/// empty result clears the register. One change each. A collection `table` names as owned or log
/// rows is no register: its rows go as archived records.
///
/// A candidate holding an earlier build's sealed token (`typed::secret::is_sealed`), in any
/// column, is opened through `secrets.0` (the old generation's opener) at its coordinates
/// ([`moved`]). Into a generation builds up to 0.199.0 still read, whose secret columns are
/// [`typed::Ty::Secret`], it is resealed for that generation through `secrets.1`, and a token the
/// new generation holds that opens there is carried already. Into the third generation it is
/// never carried as a token: a `SecretRef` column's value goes to the store entry its origin
/// names in `secrets.2`, and the candidate carries the reference; a `Phone` column's carries the
/// number. A candidate the new generation already holds as a
/// reference or a plain value keeps what it holds there.
///
/// # Errors
/// A register would hold more candidates than a carried value lists, a key breaks a rule, or a
/// sealed token does not open, sits in a column that is neither `SecretRef` nor `Phone`, or opens
/// to a value its column cannot hold. Each names coordinates, never a value.
fn restate_with_claims(
    (from, table): (&typed::Table, &typed::Table),
    source: &Cells,
    held: &BTreeSet<Digest>,
    target: &Cells,
    carried: &Carried,
    secrets: Secrets<'_>,
    claims: &typed::claim::CarriedClaims,
) -> Result<Restated> {
    let target_claims = typed::claim::carried_claims(target)?;
    let reseal = secrets.1;
    let mut out = Restated::default();
    let mut registers: Vec<Register> = Vec::new();
    for (coord @ (collection, key), cell) in source {
        if NOT_RESTATED.contains(&collection.as_str())
            || cell.stopped
            || table
                .collection(collection)
                .is_some_and(|known| known.kind != typed::Kind::Cells)
        {
            continue;
        }
        let candidates = cell::origins_refs(cell);
        match list_to_set((from, table), collection, key) {
            Some(form) => registers.extend(as_members(
                form,
                coord,
                &candidates,
                (target, held, carried),
            )?),
            None => registers.push((
                coord.clone(),
                candidates
                    .into_iter()
                    .map(|(origin, value)| (origin, value.clone()))
                    .collect(),
            )),
        }
    }
    for (coord, candidates) in registers {
        let (collection, key) = &coord;
        let coord = &coord;
        let now: BTreeMap<Digest, &Value> = target
            .get(coord)
            .map(cell::origins_refs)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let carried_here = carried.get(coord);
        let mut old: BTreeMap<Digest, Value> = candidates
            .into_iter()
            .filter(|(origin, _)| {
                carried_here.is_none_or(|origins| !origins.contains(origin))
                    || now.contains_key(origin)
            })
            .collect();
        // Each moved value's entry is named by its origin ([`secret::moved_entry`]), so every Mac
        // that rolls this register names the same entries; equal values share the first one, in
        // origin order. `named` holds every entry chosen, `moving` those still to put.
        let mut named: Vec<(String, Secret)> = Vec::new();
        let mut moving: Vec<(String, Secret)> = Vec::new();
        for (origin, value) in &mut old {
            if let Some(&there) = now.get(origin).filter(|&&there| {
                token_of(there).is_none_or(|token| {
                    reseal.is_some_and(|opens| {
                        CellKey::parse(key).is_ok_and(|parsed| {
                            opens.open(collection, parsed.pk, parsed.col, token).is_ok()
                        })
                    })
                })
            }) {
                value.clone_from(there);
                continue;
            }
            if token_of(value).is_some() {
                moved(
                    table,
                    (collection.as_str(), key.as_str(), origin),
                    value,
                    secrets,
                    (&mut named, &mut moving),
                )?;
            } else {
                stray_secret(table, (collection.as_str(), key.as_str()), value)?;
            }
        }
        let chosen: BTreeSet<String> = named.into_iter().map(|(entry, _)| entry).collect();
        out.puts.extend(moving);
        let mut merged: BTreeMap<Digest, Value> = now
            .iter()
            .filter(|(origin, _)| !held.contains(*origin) || old.contains_key(*origin))
            .map(|(origin, value)| (*origin, (*value).clone()))
            .collect();
        merged.extend(old);
        let provenance_unchanged = merged.keys().all(|origin| {
            claims.get(coord).and_then(|origins| origins.get(origin))
                == target_claims
                    .get(coord)
                    .and_then(|origins| origins.get(origin))
        });
        if provenance_unchanged
            && merged.len() == now.len()
            && merged
                .iter()
                .all(|(origin, value)| now.get(origin) == Some(&value))
        {
            continue;
        }
        // What the register stops naming is owed removal; what it keeps naming from the new
        // generation must still be in the store: a Mac that wrote the register meanwhile may
        // have removed an entry this Mac's view of the new generation still names, and a late
        // write naming it again would leave a candidate naming nothing. Such a roll stops until
        // this Mac holds that write.
        let kept: BTreeSet<&str> = merged
            .values()
            .filter_map(|value| secret_entry(table, (collection, key), value))
            .collect();
        for value in now.values() {
            if let Some(entry) = secret_entry(table, (collection, key), value)
                && !kept.contains(entry)
            {
                out.drops.push(entry.to_owned());
            }
        }
        for entry in kept {
            if !chosen.contains(entry) && secrets.2.get(entry)?.is_none() {
                return Err(Error::new(&format!(
                    "{collection} {key}: the new generation names a store entry this Mac's vault \
                     does not hold; the roll stops until this Mac holds the write that removed it, \
                     or the vault that put it"
                )));
            }
        }
        let value = if merged.is_empty() {
            None
        } else {
            let mut encoded = cell::carry(&merged).ok_or_else(|| {
                Error::new(&format!(
                    "{collection}: a register holds more candidates than a roll carries; resolve it \
                     first"
                ))
            })?;
            for (item, origin) in encoded
                .as_object_mut()
                .unwrap()
                .get_mut("k")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .zip(merged.keys())
            {
                if let Some(provenance) = claims.get(coord).and_then(|origins| origins.get(origin))
                {
                    item.as_object_mut()
                        .unwrap()
                        .insert("c".into(), provenance.clone());
                }
            }
            Some(encoded)
        };
        out.rows.push(vec![Change {
            collection: collection.clone(),
            key: key.clone(),
            value,
        }]);
    }
    Ok(out)
}

/// One register a roll restates: its coordinates, and the old generation's candidates of it,
/// each by the operation that wrote it.
type Register = ((String, String), Vec<(Digest, Value)>);

/// How the list cell `key` of `collection` changes form from the generation `from` to `to`
/// (LT-15B): where `from` holds its column as a list of at most `.2` texts and `to` as a text
/// set whose members are of type `.1`, the cell's key, the member type and that bound; else
/// `None`.
fn list_to_set<'k>(
    (from, to): (&typed::Table, &typed::Table),
    collection: &str,
    key: &'k str,
) -> Option<(CellKey<'k>, &'static typed::Ty, usize)> {
    let parsed = CellKey::parse(key)
        .ok()
        .filter(|parsed| parsed.elem.is_none())?;
    let typed::Ty::List(_, longest) = from.collection(collection)?.column(parsed.col)?.ty else {
        return None;
    };
    let member = to
        .collection(collection)?
        .column(parsed.col)?
        .ty
        .text_set()?;
    Some((parsed, member, longest))
}

/// The element registers that restate a list register (`coord`, its old `candidates`) in a
/// generation that holds its column as a text set (`form`, from [`list_to_set`]): one register
/// for each member any candidate lists, and for each element `target` holds there that an
/// operation this Mac holds of the old generation (`held`), or one a roll carried, wrote. At each
/// element every list candidate is one candidate, its origin, schema version, source and
/// automatic mark kept: the member's text where the list holds it, `false` where it does not, a
/// revert's bare `null` as it is. A member every candidate lists so reads as one value; any
/// other is a conflict of an add and a removal, as concurrent lists were, so no union settles a
/// concurrent removal. Members compare in their normal form ([`keys::normal_text`]); an item that
/// is only space lists nothing.
///
/// # Errors
/// A candidate is no typed list the old generation reads (not a list, longer than its column
/// allows, an item no text, or one no set member can be); the error names the coordinates,
/// never a value.
fn as_members(
    (parsed, member, longest): (CellKey<'_>, &typed::Ty, usize),
    (collection, key): &(String, String),
    candidates: &[(Digest, &Value)],
    (target, held, carried): (&Cells, &BTreeSet<Digest>, &Carried),
) -> Result<Vec<Register>> {
    let refused = || {
        Error::new(&format!(
            "{collection} {key}: a list the old generation reads raw cannot become set members; \
             resolve it first"
        ))
    };
    // Each candidate's list, by origin: its typed value's fields and the members it holds, by
    // element key; `None` for a revert's bare `null`.
    let mut lists: Vec<(Digest, Option<(CellValue, BTreeMap<String, String>)>)> = Vec::new();
    let mut universe: BTreeSet<String> = BTreeSet::new();
    for (origin, value) in candidates {
        if value.is_null() {
            lists.push((*origin, None));
            continue;
        }
        let typed = CellValue::decode(value).map_err(|_| refused())?;
        let items: &[Value] = match &typed.x {
            Value::Null => &[],
            Value::Array(items) if items.len() <= longest => items,
            _ => return Err(refused()),
        };
        let mut members = BTreeMap::new();
        for item in items {
            let raw = item.as_str().ok_or_else(refused)?;
            if keys::normal_text(raw).is_empty() {
                continue;
            }
            let text = cell::text_member(member, raw).map_err(|_| refused())?;
            members.insert(keys::text_elem(&text), text);
        }
        universe.extend(members.keys().cloned());
        lists.push((*origin, Some((typed, members))));
    }
    // An element the new generation holds from this generation's history, which no candidate
    // lists any more (a later list dropped it), is restated too, so the drop reaches it.
    for ((there, element), cell) in target {
        let Ok(at) = CellKey::parse(element) else {
            continue;
        };
        if there == collection
            && at.pk == parsed.pk
            && at.col == parsed.col
            && let Some(elem) = at.elem
            && cell::origins_refs(cell).iter().any(|(origin, _)| {
                held.contains(origin)
                    || carried
                        .get(&(there.clone(), element.clone()))
                        .is_some_and(|origins| origins.contains(origin))
            })
        {
            universe.insert(elem.to_owned());
        }
    }
    let mut out = Vec::new();
    for elem in universe {
        let element = CellKey {
            pk: parsed.pk,
            col: parsed.col,
            elem: Some(&elem),
        }
        .encode()?;
        let candidates = lists
            .iter()
            .map(|(origin, list)| {
                let value = list.as_ref().map_or(Value::Null, |(typed, members)| {
                    CellValue {
                        x: members
                            .get(&elem)
                            .map_or(Value::Bool(false), |text| Value::String(text.clone())),
                        ..typed.clone()
                    }
                    .encode()
                });
                (*origin, value)
            })
            .collect();
        out.push(((collection.clone(), element), candidates));
    }
    Ok(out)
}

/// Refuse a candidate of a `SecretRef` column that is neither empty nor a reference to an entry
/// of its own cell ([`typed::cell::owns_entry`]): a value some writer left in plain text, or a
/// reference to another entry, is never carried into the new generation.
///
/// # Errors
/// That candidate, naming its coordinates only.
fn stray_secret(
    table: &typed::Table,
    (collection, key): (&str, &str),
    value: &Value,
) -> Result<()> {
    let Ok(parsed) = CellKey::parse(key) else {
        return Ok(());
    };
    let Some(column) = table
        .collection(collection)
        .and_then(|known| known.column(parsed.col))
        .filter(|column| matches!(column.ty, typed::Ty::SecretRef) && parsed.elem.is_none())
    else {
        return Ok(());
    };
    let x = value.get("x").unwrap_or(value);
    if x.is_null()
        || (typed::cell::admissible(&column.ty, x)
            && typed::cell::owns_entry(collection, column, parsed.pk, x))
    {
        return Ok(());
    }
    Err(Error::new(&format!(
        "{collection} {key}: a secret cell holds neither a sealed value nor a reference to its \
         own store entry; the roll stops"
    )))
}

/// The store entry a candidate of the `SecretRef` cell at `collection` and `key` names, where it
/// names one of its own cell's ([`secret::bound`]).
fn secret_entry<'v>(
    table: &typed::Table,
    (collection, key): (&str, &str),
    value: &'v Value,
) -> Option<&'v str> {
    let parsed = CellKey::parse(key).ok()?;
    let column = table.collection(collection)?.column(parsed.col)?;
    if column.ty != typed::Ty::SecretRef || parsed.elem.is_some() {
        return None;
    }
    secret::entry_of(value.get("x").unwrap_or(value))
        .filter(|entry| secret::bound(collection, parsed.col, parsed.pk, entry))
}

/// The sealed token a candidate holds: its typed value's `x`, or the candidate itself where it
/// is a bare string.
fn token_of(value: &Value) -> Option<&str> {
    value
        .get("x")
        .unwrap_or(value)
        .as_str()
        .filter(|text| secret::is_sealed(text))
}

/// Move one candidate's sealed token out of the history, in place (`restate`): open it at the
/// coordinates `collection` and `key` name; in a `SecretRef` column the candidate carries the
/// reference to the store entry its origin names ([`secret::moved_entry`]), and the value joins
/// `moving`, the register's puts, unless the store holds it there already (another Mac's roll of
/// the same candidate, or a rerun of this one); a value equal to one chosen already (`named`)
/// shares its entry. An entry holding another value stops the roll. In a `Phone` column the
/// candidate carries the number itself.
///
/// # Errors
/// The token does not open, the column is neither, the candidate is not a typed cell value, the
/// value is a has-password flag or no phone number, or no entry name fits; each names the
/// coordinates only.
fn moved(
    table: &typed::Table,
    (collection, key, origin): (&str, &str, &Digest),
    value: &mut Value,
    (opener, reseal, store): Secrets<'_>,
    (named, moving): (&mut Moving, &mut Moving),
) -> Result<()> {
    let at = |why: &str| Error::new(&format!("{collection} {key}: {why}; the roll stops"));
    let parsed = CellKey::parse(key)
        .map_err(|_| at("a sealed value sits under a key this build cannot read"))?;
    let ty = table
        .collection(collection)
        .and_then(|known| known.column(parsed.col))
        .map(|column| &column.ty);
    let Some(object) = value.as_object_mut() else {
        return Err(at("a sealed value is not a typed cell value"));
    };
    let token = object
        .get("x")
        .and_then(Value::as_str)
        .ok_or_else(|| at("a sealed value is not a typed cell value"))?;
    let opened = opener
        .open(collection, parsed.pk, parsed.col, token)
        .map_err(|_| at("an earlier build's sealed value does not open on this Mac"))?;
    let carried = match ty {
        Some(typed::Ty::SecretRef) if parsed.elem.is_none() => {
            let entry = match named.iter().find(|(_, held)| held.same(&opened)) {
                Some((entry, _)) => entry.clone(),
                None => {
                    if secret::flag_like(&opened) {
                        return Err(at("a sealed value is a has-password flag, not a secret"));
                    }
                    let entry = secret::moved_entry((collection, parsed.col, parsed.pk), origin)
                        .map_err(|_| at("the cell's coordinates make no store entry name"))?;
                    match store.get(&entry)? {
                        Some(held) if !held.same(&opened) => {
                            return Err(at("the store entry this value moves into holds another"));
                        }
                        Some(_) => {}
                        None => {
                            moving.push((entry.clone(), Secret::new(opened.plaintext().to_owned())))
                        }
                    }
                    named.push((entry.clone(), opened));
                    entry
                }
            };
            secret::reference(&store.vault()?, &entry)
        }
        Some(typed::Ty::Secret) if parsed.elem.is_none() => {
            if secret::flag_like(&opened) {
                return Err(at("a sealed value is a has-password flag, not a secret"));
            }
            let sealer = reseal.ok_or_else(|| at("the new generation's sealer is not at hand"))?;
            Value::String(sealer.seal(collection, parsed.pk, parsed.col, &opened)?)
        }
        Some(typed::Ty::Phone) => Value::String(
            typed::cell::phone_digits(opened.plaintext())
                .ok_or_else(|| at("a sealed value opens to no phone number"))?,
        ),
        _ => return Err(at("a sealed value sits in a column that holds no secret")),
    };
    object.insert("x".into(), carried);
    Ok(())
}

/// One operation of a table as the covers count it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) actor: [u8; 48],
    pub(crate) sequence: u64,
    /// Its payload is here and this build cannot read it, so no restatement of this build's
    /// carries it.
    pub(crate) unread: bool,
}

/// The `Held` of each operation this Mac retains of a table.
pub(crate) fn held_of(log: &Log) -> Vec<Held> {
    held_of_ops(&log.snapshot().operations)
}

fn held_of_ops(operations: &[crate::documents::log::Operation]) -> Vec<Held> {
    operations
        .iter()
        .map(|op| Held {
            actor: op.header.actor,
            sequence: op.header.sequence,
            unread: !op.available || (op.catalog_eligible && op.refusal.is_some()),
        })
        .collect()
}

/// For each writer, the last of its records this Mac holds.
pub(crate) fn holdings(ops: &[Held]) -> BTreeMap<String, u64> {
    let mut by_actor: BTreeMap<Actor, u64> = BTreeMap::new();
    for op in ops {
        let last = by_actor.entry(op.actor).or_default();
        *last = (*last).max(op.sequence);
    }
    by_actor
        .into_iter()
        .map(|(actor, last)| (keys::actor_key(&actor), last))
        .collect()
}

/// For each writer, the last of its records a restatement of `ops` carries: the one before its
/// first record this build cannot read, else its last. A writer's records here are a prefix of
/// its chain, since each names its predecessor, so every record up to that one is carried.
pub(crate) fn frontier(ops: &[Held]) -> BTreeMap<String, u64> {
    let mut by_actor: BTreeMap<Actor, (u64, u64)> = BTreeMap::new();
    for op in ops {
        let (last, first_unread) = by_actor.entry(op.actor).or_insert((0, u64::MAX));
        *last = (*last).max(op.sequence);
        if op.unread {
            *first_unread = (*first_unread).min(op.sequence);
        }
    }
    by_actor
        .into_iter()
        .map(|(actor, (last, first_unread))| {
            let covered = if first_unread == u64::MAX {
                last
            } else {
                last.min(first_unread.saturating_sub(1))
            };
            (keys::actor_key(&actor), covered)
        })
        .collect()
}

/// What the cover rows of `view` for generation `from` carry, merged: each writer's highest.
///
/// A cover row counts only as the Mac it is keyed by wrote it (`keys::roll_pk` of `from` and
/// the signer's own machine, `resolve` reading the binding in the roster each record names): a
/// cover is that Mac's statement of what its own restatement carried, and the storage release
/// deletes data on its word, so no other writer may speak for it. The rows are read from every
/// eligible operation rather than the cells' current values, so a later write over a cover
/// cannot hide one either; each Mac's covers only grow, so merging every one it wrote reads
/// the same as its latest. A write that is no cover of its signer's counts for nothing.
pub(crate) fn covers(
    view: &View,
    resolve: impl Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    from: u32,
) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for op in view
        .ops
        .iter()
        .filter(|op| !op.revert && !view.quota.over.contains(&op.id))
    {
        if !op.changes.iter().any(|c| c.collection == ROLL.name) {
            continue;
        }
        let Some(machine) = resolve(&op.roster, &op.actor) else {
            continue;
        };
        let own = keys::roll_pk(from, &keys::uuid32(&machine));
        let (mut said_from, mut covered) = (None, None);
        for change in op.changes.iter().filter(|c| c.collection == ROLL.name) {
            let Ok(key) = CellKey::parse(&change.key) else {
                continue;
            };
            let Some(value) = change
                .value
                .as_ref()
                .and_then(|value| CellValue::decode(value).ok())
            else {
                continue;
            };
            if key.pk != own || key.elem.is_some() {
                continue;
            }
            match key.col {
                "from" => said_from = value.x.as_u64(),
                "covered" => covered = Some(value.x),
                _ => {}
            }
        }
        if said_from != Some(u64::from(from)) {
            continue;
        }
        for item in covered
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(actor), Some(sequence)) = (
                item.get("a").and_then(Value::as_str),
                item.get("s").and_then(Value::as_u64),
            ) {
                if let Some(last) = out.get_mut(actor) {
                    *last = (*last).max(sequence);
                } else {
                    out.insert(actor.to_owned(), sequence);
                }
            }
        }
    }
    out
}

/// Each writer of `view` that wrote its own cover row, for any generation: the Macs whose word a
/// carried history stands on (`alab::history::carried`), as a release stands on their covers.
/// A cover counts only as the Mac its key names wrote it, as in [`covers`].
pub(crate) fn rollers(
    view: &View,
    resolve: impl Fn(&Digest, &Actor) -> Option<[u8; 16]>,
) -> BTreeSet<Actor> {
    let mut out = BTreeSet::new();
    for op in view
        .ops
        .iter()
        .filter(|op| !op.revert && !view.quota.over.contains(&op.id))
    {
        if out.contains(&op.actor) {
            continue;
        }
        let Some(machine) = resolve(&op.roster, &op.actor) else {
            continue;
        };
        let uuid = keys::uuid32(&machine);
        let own = op
            .changes
            .iter()
            .filter(|change| change.collection == ROLL.name)
            .any(|change| {
                let Ok(key) = CellKey::parse(&change.key) else {
                    return false;
                };
                key.col == "from"
                    && key.elem.is_none()
                    && change
                        .value
                        .as_ref()
                        .and_then(|value| CellValue::decode(value).ok())
                        .and_then(|value| value.x.as_u64())
                        .and_then(|from| u32::try_from(from).ok())
                        .is_some_and(|from| key.pk == keys::roll_pk(from, &uuid))
            });
        if own {
            out.insert(op.actor);
        }
    }
    out
}

/// Whether every record `held` names is within `covered`.
pub(crate) fn within(held: &BTreeMap<String, u64>, covered: &BTreeMap<String, u64>) -> bool {
    held.iter()
        .all(|(actor, last)| covered.get(actor).is_some_and(|c| c >= last))
}

/// The cover row's cells for this Mac (`uuid`) restating generation `from` with `frontier`, or
/// none where the new generation's row already says it.
pub(super) fn cover_row(
    target: &Projection,
    from: u32,
    uuid: &str,
    frontier: &BTreeMap<String, u64>,
    ops: usize,
    now_ms: u64,
) -> Result<Vec<Change>> {
    if frontier.len() > super::registry::MAX_COVER_WRITERS {
        return Err(Error::new(
            "the table names more writers than a cover row can; this build cannot roll it",
        ));
    }
    let pk = keys::roll_pk(from, uuid);
    if target
        .rows
        .get(&pk)
        .and_then(|row| row.get("covered"))
        .and_then(Value::as_array)
        .is_some_and(|arr| {
            arr.len() == frontier.len()
                && arr.iter().zip(frontier).all(|(item, (actor, sequence))| {
                    item.as_object().is_some_and(|obj| {
                        obj.len() == 2
                            && obj.get("a").and_then(Value::as_str) == Some(actor.as_str())
                            && obj.get("s").and_then(Value::as_u64) == Some(*sequence)
                    })
                })
        })
    {
        return Ok(Vec::new());
    }
    let covered: Vec<Value> = frontier
        .iter()
        .map(|(actor, sequence)| json!({"a": actor, "s": sequence}))
        .collect();
    let cell = |col: &str, x: Value| {
        cell::change(
            &ROLL,
            CellKey {
                pk: &pk,
                col,
                elem: None,
            },
            &CellValue::owner(ROLL.version, x),
        )
    };
    Ok(vec![
        cell("from", Value::from(u64::from(from)))?,
        cell("covered", Value::Array(covered))?,
        cell("ops", Value::from(ops as u64))?,
        cell("at", Value::from(now_ms))?,
        cell("row", Value::from("live"))?,
    ])
}

/// The archived row carrying the record `wire` of generation `from`, keyed by its operation.
fn archive_row(operation: &Digest, from: u32, wire: &[u8]) -> Result<Vec<Change>> {
    let pk = keys::archived_pk(operation);
    let cell = |col: &str, x: Value| {
        cell::change(
            &ARCHIVED,
            CellKey {
                pk: &pk,
                col,
                elem: None,
            },
            &CellValue::owner(ARCHIVED.version, x),
        )
    };
    Ok(vec![
        cell("generation", Value::from(u64::from(from)))?,
        cell(
            "wire",
            Value::String(aterm_secrets::b64::encode_padded(wire)),
        )
        .map_err(|_| Error::new("an owned record is too large for a roll to carry"))?,
        cell("row", Value::from("live"))?,
    ])
}

/// What a roll found or did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rolled {
    /// The generation restated, and the one it went into.
    pub from: u32,
    pub to: u32,
    /// Registers restated, owned records archived, and whether this Mac's cover row changed.
    pub registers: usize,
    pub archived: usize,
    pub cover: bool,
    /// History entries carried (`alab::history`), and those left behind its bound.
    pub history: usize,
    pub history_left: usize,
    /// Records appended (none for a plan).
    pub records: usize,
    /// Whether this roll sealed the old generation (an earlier one, or another Mac, may have).
    pub sealed: bool,
    /// The current Writers, by Commons machine, that have not declared the next generation, so
    /// have shown no build that reads it: the seal waits for them (for a plan, would wait).
    pub waiting: Vec<[u8; 16]>,
    /// What the storage release did after it.
    pub released: Released,
    /// Records this Mac holds of the old generation that this build cannot read, so no cover of
    /// its own carries them: they keep the old storage here until a build that reads them rolls.
    pub unread: usize,
}

/// The storage release's `covered` answer for a restated registry generation: whether the next
/// generation's cover rows carry every record this Mac holds of `retired`, in its primary and
/// its recovery storage alike, since the release deletes both, and its history rows every entry
/// of that history (`alab::history::uncarried`), so a roll by a build that carried none keeps
/// the storage until a roll here carries it; a history this Mac cannot read answers not carried. Holding none, it is carried;
/// holding some with no next generation here, it is not. A region whose storage directory is
/// there but cannot be observed (an unfinished enrollment) answers not carried: what it holds
/// is unknown, so it is never released unread.
///
/// # Errors
/// A table's storage cannot be read, or the retired generation's view's resolution refuses (as
/// [`att::Resolved::read`]).
pub(crate) fn covered(
    session: &Session<'_>,
    directory: &Directory,
    retired: &Retired,
) -> Result<bool> {
    let trust = directory.trust();
    let chain = att::REGISTRY.chain();
    let Some(from) = chain
        .iter()
        .find(|c| c.descriptor(trust).is_ok_and(|d| d == retired.descriptor))
    else {
        return Ok(false);
    };
    let Some(to) = from.successor else {
        return Ok(false);
    };
    let scope = retired.descriptor.scope;
    let mut held: BTreeMap<String, u64> = BTreeMap::new();
    let mut owed = Vec::new();
    for region in [directory.primary(), directory.recovery()] {
        match region.observe_store_if_exists(session, scope)? {
            Some(old) => {
                let log = Log::read_with(session, &old, scope, decode)?;
                for (actor, last) in holdings(&held_of(&log)) {
                    let entry = held.entry(actor).or_default();
                    *entry = (*entry).max(last);
                }
                let view = View::read(from.table, session, &log)?;
                let Ok((entries, _)) =
                    history::owed(session, from.table, from.generation, log, &view)
                else {
                    return Ok(false);
                };
                owed.extend(entries);
            }
            None if region.path(scope)?.try_exists()? => return Ok(false),
            None => {}
        }
    }
    if held.is_empty() {
        return Ok(true);
    }
    let next = to.descriptor(trust)?.scope;
    let Some(new) = directory.primary().observe_store_if_exists(session, next)? else {
        return Ok(false);
    };
    let view = View::read(
        to.table,
        session,
        &Log::read_with(session, &new, next, decode)?,
    )?;
    let resolve = owned::commons::historical(session);
    Ok(within(&held, &covers(&view, &resolve, from.generation))
        && history::uncarried(&owed, &history::carried(&view, &resolve, None)).is_empty())
}

/// Each register of `cells` holding a candidate whose sealed token (an earlier build's `sec1:`)
/// does not open through `opener`, the generation's own, as (collection, pk, column): a token an
/// older build's roll copied as it was, sealed for the generation before it, or one under a key
/// this Mac does not hold. Any column is read, since no column holds a token by design; a key
/// this build cannot parse is named whole, with an empty column. Names coordinates only, never
/// a value.
pub(crate) fn unopened(cells: &Cells, opener: &dyn Sealer) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for ((collection, key), cell) in cells {
        let parsed = CellKey::parse(key).ok();
        let (pk, column) = parsed
            .as_ref()
            .map_or((key.as_str(), ""), |parsed| (parsed.pk, parsed.col));
        if cell::origins_refs(cell).iter().any(|(_, value)| {
            token_of(value).is_some_and(|token| opener.open(collection, pk, column, token).is_err())
        }) {
            out.push((collection.clone(), pk.to_owned(), column.to_owned()));
        }
    }
    out
}

/// What this Mac can say of a later registry generation another Mac declared, before it
/// declares it too (`endorse`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Judged {
    /// Its store is complete here, holds a roller's cover for the generation before it, and
    /// every sealed token it holds opens under its key.
    Opens,
    /// It cannot be judged yet: this Mac holds no store of it, its history here is incomplete, no
    /// cover of the generation before it is there (a roller writes its cover last, so it says the
    /// restated records before it are there), or its key is unavailable. A later housekeeping
    /// pass judges it again.
    Unjudged,
    /// This many registers hold a sealed token that does not open under its key ([`unopened`]):
    /// a roll by a build that copied tokens as they were (v0.154.0). Endorsing it would let that
    /// roll seal the generation before it, whose storage release deletes the only copy that
    /// opens, so this Mac does not.
    Unopened(usize),
}

/// Judge the registry generation `compiled` (whose predecessor in the chain is `previous`) as
/// this Mac stores it. Fails closed: whatever cannot be read is [`Judged::Unjudged`].
fn judge(
    session: &Session<'_>,
    directory: &Directory,
    previous: &Compiled,
    compiled: &Compiled,
) -> Judged {
    let trust = directory.trust();
    let judged = || -> Result<Judged> {
        let scope = compiled.descriptor(trust)?.scope;
        let Some(store) = directory.observe_store_if_exists(session, scope)? else {
            return Ok(Judged::Unjudged);
        };
        let log = Log::read_with(session, &store, scope, decode)?;
        if !log.awaited(session.actor()).is_empty() {
            return Ok(Judged::Unjudged);
        }
        let view = View::read(compiled.table, session, &log)?;
        if covers(
            &view,
            owned::commons::historical(session),
            previous.generation,
        )
        .is_empty()
        {
            return Ok(Judged::Unjudged);
        }
        let name = compiled.name();
        let opener = att::sealer(&name, session, scope)?;
        Ok(match unopened(&view.cells, &opener).len() {
            0 => Judged::Opens,
            n => Judged::Unopened(n),
        })
    };
    judged().unwrap_or(Judged::Unjudged)
}

/// Each later registry generation this build compiles that `snapshot` names, with its predecessor
/// and descriptor, judged here ([`judge`]), and whether this Mac has declared it.
fn later_generations(
    session: &Session<'_>,
    directory: &Directory,
    snapshot: &crate::catalog::Snapshot,
) -> Vec<(&'static Compiled, crate::catalog::Descriptor, bool, Judged)> {
    let trust = directory.trust();
    let chain = att::REGISTRY.chain();
    let mut out = Vec::new();
    for pair in chain.windows(2) {
        let [previous, compiled] = pair else {
            continue;
        };
        let Ok(descriptor) = compiled.descriptor(trust) else {
            continue;
        };
        let Some(entry) = snapshot.entries.get(&descriptor.scope.digest()) else {
            continue;
        };
        let declared = entry
            .declarations
            .iter()
            .any(|d| d.actor == session.actor());
        out.push((
            *compiled,
            descriptor,
            declared,
            judge(session, directory, previous, compiled),
        ));
    }
    out
}

/// Declare, as this Mac, each later registry generation this build compiles that `snapshot`
/// shows another Mac declared and this one has not (`Directory::endorse`): the evidence a roll
/// waits for before it seals the generation before it. Housekeeping on a claims write and a
/// release pass, so it never fails its caller: a Mac that cannot declare now does at a later
/// write.
///
/// A generation is declared only once it is judged to open (`judge`, design one-mesh §4.8, §11
/// step A6): a roll by a build that copied a sealed cell's token as it was (v0.154.0, whose
/// restatement carried an unknown column as it is) leaves a token sealed to the old table's
/// name, which never opens in the new one, so this Mac refuses to declare it and the roll's seal
/// keeps waiting for it, reporting it among the Macs it waits for. Refusing changes nothing
/// here; a roll by this build moves every such token's value out of the history (`restate`),
/// and the next pass declares the generation. A generation this build's roll wrote holds no
/// token, so it opens. A generation this Mac cannot judge yet (no store, an incomplete history, no
/// roller's cover) waits for a later pass. The roller's own roll declares its target directly,
/// since its own restatement repairs the generation.
///
/// A generation whose secret cells name store entries is declared only while `store` admits a
/// value ([`secret::Store::admit`], the fleet floor): a declaration is this Mac's word that it
/// reads the generation, which lets a roll seal the one before, after which every secret is
/// written into the store; a Mac the floor holds back says nothing, and the seal waits for it as
/// for an older build.
pub(crate) fn endorse(
    session: &Session<'_>,
    directory: &Directory,
    snapshot: &crate::catalog::Snapshot,
    store: &dyn secret::Store,
) -> bool {
    let trust = directory.trust();
    let chain = att::REGISTRY.chain();
    let mut endorsed = false;
    for pair in chain.windows(2) {
        let [previous, compiled] = pair else {
            continue;
        };
        let Ok(descriptor) = compiled.descriptor(trust) else {
            continue;
        };
        let Some(entry) = snapshot.entries.get(&descriptor.scope.digest()) else {
            continue;
        };
        if entry
            .declarations
            .iter()
            .any(|d| d.actor == session.actor())
        {
            continue;
        }
        if compiled.table.names_store() && store.admit().is_err() {
            continue;
        }
        if judge(session, directory, previous, compiled) == Judged::Opens
            && directory.endorse(session, &descriptor).is_ok()
        {
            endorsed = true;
        }
    }
    endorsed
}

/// A later registry generation holding sealed tokens that do not open under its key, as `aterm db
/// alab check` names it: by generation and table digest, never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unopened {
    pub generation: u32,
    /// Its descriptor's scope digest.
    pub table: Digest,
    /// The registers holding a sealed token that does not open.
    pub cells: usize,
    /// Whether this Mac declared it anyway (before this build refused such a generation).
    pub declared: bool,
}

impl Registry {
    /// Roll the registry to its next generation (module docs), or with `apply` false, say what a
    /// roll would write and write nothing. Run again on a Mac that holds records of a sealed
    /// generation no cover carries, it restates those. `now_ms` stamps the cover row; the storage
    /// release reads `now_ms` as this Mac's clock.
    ///
    /// A sealed value the old generation holds goes into this Mac's `aterm secrets` vault
    /// (`alab::vault::Vault`), put only when `apply` is true.
    ///
    /// # Errors
    /// No Commons writer; this Mac holds no registry; its history of either generation is
    /// incomplete; this build compiles no later generation; a register or record is too large to
    /// carry; a sealed value cannot be moved (`restate`) or the vault refuses it; or a record
    /// cannot be written.
    pub fn roll(&self, apply: bool, now_ms: u64) -> Result<Rolled> {
        self.roll_with(apply, now_ms, &super::secret::store(self.paths()))
    }

    /// [`Registry::roll`], putting each sealed value it moves into `secrets`.
    ///
    /// # Errors
    /// As [`Registry::roll`].
    pub(crate) fn roll_with(
        &self,
        apply: bool,
        now_ms: u64,
        secrets: &dyn secret::Store,
    ) -> Result<Rolled> {
        let deadline = crate::admission::deadline();
        let paths = crate::application_paths_until(self.paths(), deadline)?;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        session.writer()?;
        let trust = bootstrap.trust();
        let directory = Directory::new(&paths, trust);
        let snapshot = directory.snapshot(&session)?;
        let chain = att::REGISTRY.chain();
        let current = att::current_in(&snapshot, trust, &chain);
        // The oldest earlier generation this Mac still stores is what is left to carry; else the
        // current one rolls.
        let mut from = current;
        for compiled in &chain {
            if compiled.generation < current.generation
                && directory
                    .path(compiled.descriptor(trust)?.scope)?
                    .try_exists()?
            {
                from = compiled;
                break;
            }
        }
        let to: &'static Compiled = from.successor.ok_or_else(|| {
            Error::new("this build compiles no registry generation after this one")
        })?;
        let fresh = from.generation == current.generation;
        let old_scope = from.descriptor(trust)?.scope;
        let old = directory
            .observe_store_if_exists(&session, old_scope)?
            .ok_or_else(|| Error::new("this Mac holds no registry to roll"))?;
        let old_log = Log::read_with(&session, &old, old_scope, decode)?;
        if !old_log.awaited(session.actor()).is_empty() {
            return Err(Error::new(
                "this Mac's history of the registry is incomplete; a roll waits for a complete scan",
            ));
        }
        let old_view = View::read(from.table, &session, &old_log)?;
        old_view.require_roll_source(from.table)?;
        let old_claims = old_view.claim_provenance(from.table)?;
        let old_snapshot = old_log.snapshot();
        let held_ops = held_of_ops(&old_snapshot.operations);
        let held: BTreeSet<Digest> = old_snapshot
            .operations
            .iter()
            .map(|op| op.header.id)
            .collect();
        let frontier = frontier(&held_ops);
        let (owed, stated) = history::owed(
            &session,
            from.table,
            from.generation,
            Log::read_with(&session, &old, old_scope, decode)?,
            &old_view,
        )?;
        let to_descriptor = to.descriptor(trust)?;
        let observed = directory.observe_store_if_exists(&session, to_descriptor.scope)?;
        let uuid = session
            .members()?
            .iter()
            .find(|binding| binding.actor() == session.actor())
            .map(|binding| keys::uuid32(binding.machine()))
            .ok_or_else(|| Error::new("this Mac is not in its own Commons roster"))?;

        let plan =
            |new: Option<&Store>| -> Result<(Restated, usize, usize, bool, history::Planned)> {
                let (target, carried, done_archive, rolls, target_claims, target_history, used) =
                    match new {
                        Some(store) => {
                            let log = Log::read_with(&session, store, to_descriptor.scope, decode)?;
                            if !log.awaited(session.actor()).is_empty() {
                                return Err(Error::new(
                                    "this Mac's history of the new registry generation is incomplete; a \
                             roll waits for a complete scan",
                                ));
                            }
                            let resolved = att::Resolved::read(to.table, &session, &log)?;
                            let view = View::of(to.table, &log, &resolved);
                            let carried =
                                carried_origins(log.eligible_iter().map(|(_, command)| command));
                            // What is carried already is each record that authenticates, wherever
                            // it was written: a cell some other writer put at a record's key first
                            // must not keep that record from being carried.
                            let archived: BTreeSet<Digest> =
                                resolved.carried.keys().copied().collect();
                            let rolls = view.project(&ROLL);
                            let claims = view.claim_provenance(to.table)?;
                            // This Mac's own rows count too: a roll cut short after its history and
                            // before its cover does not write that history again.
                            let history = history::carried(
                                &view,
                                owned::commons::historical(&session),
                                Some(session.actor()),
                            );
                            let used = store.retained_use(&session, session.actor())?;
                            (view.cells, carried, archived, rolls, claims, history, used)
                        }
                        None => Default::default(),
                    };
                let from_name = from.name();
                let opener = att::sealer(&from_name, &session, old_scope)?;
                // A generation builds up to 0.199.0 still read takes a token resealed for it, never
                // a store reference ([`typed::Ty::Secret`]); only the third generation takes those.
                let to_name = to.name();
                let reseal = if to.table.collections.iter().any(|c| {
                    c.columns
                        .iter()
                        .any(|column| column.ty == typed::Ty::Secret)
                }) {
                    Some(att::sealer(&to_name, &session, to_descriptor.scope)?)
                } else {
                    None
                };
                let mut claims = target_claims;
                typed::claim::merge_claims(&mut claims, old_claims.clone())?;
                let mut restated = restate_with_claims(
                    (from.table, to.table),
                    &old_view.cells,
                    &held,
                    &target,
                    &carried,
                    (&opener, reseal.as_ref().map(|s| s as &dyn Sealer), secrets),
                    &claims,
                )?;
                let rows = &mut restated.rows;
                let registers = rows.len();
                let mut archived = 0;
                let mut operations: BTreeSet<Digest> = BTreeSet::new();
                for read in old_view.owned.values() {
                    operations.extend(read.rows.values().map(|row| row.operation));
                }
                for operation in operations {
                    if done_archive.contains(&operation) {
                        continue;
                    }
                    // A row the old generation itself read from an archived record is carried with
                    // that record's archived row, a register like any other.
                    let Some(wire) = old.wire_unchecked(&operation) else {
                        continue;
                    };
                    rows.push(archive_row(&operation, from.generation, wire)?);
                    archived += 1;
                }
                let cover = cover_row(
                    &rolls,
                    from.generation,
                    &uuid,
                    &frontier,
                    held.len(),
                    now_ms,
                )?;
                let changed = !cover.is_empty();
                if changed {
                    rows.push(cover);
                }
                // The history goes in with the restatement, before the cover that says it is there,
                // within what this Mac's writer has room for once the rest of the roll is in: a
                // roll whose restatement alone passes that is refused before anything is declared.
                let (records, bytes) = history::appended(&rows[..])?;
                let budget = history::Budget::room(
                    used.writer_records.saturating_add(records),
                    used.writer_bytes.saturating_add(bytes),
                )
                .ok_or_else(|| {
                    Error::new(
                        "this roll passes what this Mac may write into the new registry generation",
                    )
                })?;
                let mut planned = history::plan(
                    from.table,
                    owed.iter().cloned(),
                    &stated,
                    &target_history,
                    from.generation,
                    &uuid,
                    budget,
                )?;
                // The cover stays last, as the apply path appends it.
                let cover = if changed { rows.pop() } else { None };
                rows.append(&mut planned.rows);
                rows.extend(cover);
                Ok((restated, registers, archived, changed, planned))
            };

        // The plan runs on what this Mac holds before anything is declared, so a roll it
        // refuses (a register past the candidates a carried value holds, a cover past its
        // writers, a record too large to archive) leaves no declaration of the next generation.
        let (planned, registers, archived, cover, carried) = plan(observed.as_ref())?;
        let unread = held_ops.iter().filter(|op| op.unread).count();
        let mut rolled = Rolled {
            from: from.generation,
            to: to.generation,
            registers,
            archived,
            cover,
            history: carried.carried,
            history_left: carried.not_carried,
            unread,
            ..Rolled::default()
        };
        if !apply {
            if fresh {
                // This Mac declares the next generation when it applies.
                rolled.waiting = Directory::undeclared(&session, &snapshot, &to_descriptor)?;
                rolled
                    .waiting
                    .retain(|machine| keys::uuid32(machine) != uuid);
            }
            return Ok(rolled);
        }
        // No value goes into the store until every Mac reading it reads one in place (the fleet
        // floor, [`secret::Store::admit`]): asked before anything is declared, so a roll it holds
        // back writes nothing, to the store or the catalog. It is asked of every roll into a
        // generation whose cells name store entries, puts or none: one with nothing to put still
        // declares that generation and lets the one before it be sealed, after which every
        // secret written goes into the store.
        if !planned.puts.is_empty() || to.table.names_store() {
            secrets.admit()?;
        }
        drop(planned);
        drop(observed);
        directory.publish(&session, &to_descriptor)?;
        directory.endorse(&session, &to_descriptor)?;
        // A write the old generation took after it was read and before the next one was
        // declared saw no roll, and may have removed an entry the plan carries
        // (`alab::secret::settle_write`): the declared generation stops every later removal, and
        // a roll that read the old generation too early is run again.
        let reread = Log::read_with(&session, &old, old_scope, decode)?.snapshot();
        if reread
            .operations
            .iter()
            .any(|op| !held.contains(&op.header.id))
        {
            return Err(Error::new(
                "the registry changed while the roll read it; run the roll again",
            ));
        }
        let mut store = Store::open(
            &session,
            &directory.path(to_descriptor.scope)?,
            to_descriptor.scope,
        )?;
        let (
            Restated {
                mut rows,
                puts,
                drops,
            },
            registers,
            archived,
            cover,
            carried,
        ) = plan(Some(&store))?;
        (rolled.registers, rolled.archived, rolled.cover) = (registers, archived, cover);
        (rolled.history, rolled.history_left) = (carried.carried, carried.not_carried);
        // Every record as the new generation holds it ([`att::conform`]), before any value
        // goes into the store: a generation older builds read takes no store reference.
        for row in &mut rows {
            att::conform(to.table, row)?;
        }
        // Each moved value is in the store before any reference to it is written, owed first
        // by this run ([`typed::secret::Store::owe`]) and paid once the records naming it are
        // appended: a roll cut short leaves them owed, and no retire or sweep runs while the next
        // generation is declared ([`att::sole_generation`]).
        // The entries the candidates it drops named stay owed when it is done: a sweep collects
        // each once no cell names it ([`typed::secret::sweep`]).
        let moved: BTreeSet<String> = puts.iter().map(|(entry, _)| entry.clone()).collect();
        secrets.owe(&moved.iter().chain(&drops).cloned().collect::<Vec<_>>())?;
        for (entry, value) in &puts {
            secrets.put(entry, value)?;
        }
        // A put the store took is not yet a copy: a vault write signed on this Mac and not
        // posted, or a draft its vault pass may still drop, holds the value nowhere another Mac
        // reads, and the seal and storage release that follow the references delete the sealed
        // tokens. So every moved entry must read back equal before any record names it; one that
        // does not stops the roll here, the entries owed, the old generation untouched.
        for (entry, value) in &puts {
            if !secrets.get(entry)?.is_some_and(|held| held.same(value)) {
                return Err(Error::new(&format!(
                    "the secret store does not hold {entry} as it was put yet, so no reference \
                     to it is written; roll again once this Mac's vault has posted it"
                )));
            }
        }
        drop(puts);
        // The cover goes last, alone: it says the records before it are there.
        let (cover_rows, body): (Vec<_>, Vec<_>) = rows
            .into_iter()
            .partition(|row| row.first().is_some_and(|c| c.collection == ROLL.name));
        let mut records = pack(body)?;
        records.extend(pack(cover_rows)?);
        let mut log = Log::read_with(&session, &store, to_descriptor.scope, decode)?;
        let request_role = request_role(to.table.role, &log);
        let mut appended = BTreeMap::new();
        for changes in records {
            let request = typed::request_id(APPLICATION, &request_role, &changes)?;
            let command = Command::Write { request, changes };
            if att::retry_or_appended(&log, session.actor(), &command, &appended)?.is_some() {
                continue;
            }
            let receipt = application::append(&session, &mut store, &command.encode()?)?;
            appended.insert(request, (receipt.id, command));
            rolled.records += 1;
        }
        if old_view.ops.iter().any(|op| {
            op.changes
                .iter()
                .any(|change| change.collection == typed::SCHEMA)
        }) {
            if rolled.records > 0 {
                log = Log::read_with(&session, &store, to_descriptor.scope, decode)?;
                appended.clear();
            }
            for changes in pack(att::schema_rows(
                to.table,
                &View::read(to.table, &session, &log)?,
            ))? {
                let request = typed::request_id(APPLICATION, to.table.role, &changes)?;
                let command = Command::Write { request, changes };
                if att::retry_or_appended(&log, session.actor(), &command, &appended)?.is_some() {
                    continue;
                }
                let receipt = application::append(&session, &mut store, &command.encode()?)?;
                appended.insert(request, (receipt.id, command));
                rolled.records += 1;
            }
        }
        drop(log);
        drop(store);
        secrets.paid(&moved)?;
        // A build that does not compile the next generation cannot read the registry once the
        // old one is sealed (its declarations conflict there, and the next is unknown to it),
        // so the seal waits until every current Writer has declared the next generation itself
        // (`Directory::endorse`), which only a build that compiles it does.
        if fresh {
            rolled.waiting =
                Directory::undeclared(&session, &directory.snapshot(&session)?, &to_descriptor)?;
            if rolled.waiting.is_empty() {
                rolled.sealed = directory.seal(&session, &from.descriptor(trust)?)?;
            }
        }
        let snapshot = directory.snapshot(&session)?;
        rolled.released = directory.release_sealed(&session, &snapshot, now_ms / 1000, &covered)?;
        Ok(rolled)
    }

    /// One storage release pass on this Mac (`catalog::release`), as its claims write runs one
    /// when due, after declaring the registry generations this build compiles (`endorse`):
    /// it deletes the storage of each sealed table this build retires once that is safe, `now_ms`
    /// being this Mac's clock, and says why each other one stays.
    ///
    /// # Errors
    /// No Commons writer here, or the catalog or a table's storage cannot be read or removed.
    pub fn release(&self, now_ms: u64) -> Result<Released> {
        self.release_with(now_ms, &super::secret::store(self.paths()))
    }

    /// [`Self::release`], judging whether this Mac may declare a generation whose secret cells
    /// name store entries by `store`'s floor ([`endorse`]).
    ///
    /// # Errors
    /// As [`Self::release`].
    pub(crate) fn release_with(&self, now_ms: u64, store: &dyn secret::Store) -> Result<Released> {
        let deadline = crate::admission::deadline();
        let paths = crate::application_paths_until(self.paths(), deadline)?;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        let directory = Directory::new(&paths, bootstrap.trust());
        let mut snapshot = directory.snapshot(&session)?;
        if endorse(&session, &directory, &snapshot, store) {
            snapshot = directory.snapshot(&session)?;
        }
        directory.release_sealed(&session, &snapshot, now_ms / 1000, &covered)
    }

    /// Each later registry generation the catalog names holding sealed tokens that do not open
    /// under its key (`Judged::Unopened`), which `endorse` refuses to declare: what `aterm db alab
    /// check` names. A generation this Mac cannot judge yet is not listed. Reads only; declares
    /// nothing.
    ///
    /// # Errors
    /// No Commons here, or the catalog cannot be read.
    pub fn unopened(&self) -> Result<Vec<Unopened>> {
        let deadline = crate::admission::deadline();
        let paths = crate::application_paths_until(self.paths(), deadline)?;
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, deadline)
            .map_err(crate::table::failure)?;
        let session = Session::from_admitted_journal_until(&bootstrap, journal, Some(deadline))?;
        let directory = Directory::new(&paths, bootstrap.trust());
        let snapshot = directory.snapshot(&session)?;
        Ok(later_generations(&session, &directory, &snapshot)
            .into_iter()
            .filter_map(|(compiled, descriptor, declared, judged)| match judged {
                Judged::Unopened(cells) => Some(Unopened {
                    generation: compiled.generation,
                    table: descriptor.scope.digest(),
                    cells,
                    declared,
                }),
                Judged::Opens | Judged::Unjudged => None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;

/// A register's candidates as the restatement reads them, for tests of [`restate`].
#[cfg(test)]
pub(crate) fn register(candidates: &[(Digest, Value)]) -> crate::database::Cell {
    crate::database::Cell {
        stopped: false,
        candidates: candidates
            .iter()
            .map(|(operation, value)| crate::database::Candidate {
                operation: *operation,
                value: value.clone(),
            })
            .collect(),
    }
}
