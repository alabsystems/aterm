// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cells collections (design §3.1–§3.2): one causal register per column. A key is
//! `<pk>.<col>` for a scalar or list column and `<pk>.<col>.<elem>` for one element of a set; a
//! value is `{"r": schema version, "x": typed value}`, plus `"s": <source pk>` when the
//! importer wrote it and `"a": true` when an automatic path did. The Commons projection keeps
//! each key's concurrent candidates and drops exactly the ones a later edit observed
//! (`documents::apply`); this reads those cells back as typed rows: equal candidates
//! collapse, a real conflict keeps every candidate with a provisional value by the column's
//! rule (never a timestamp winner), a set element is present when any candidate adds it
//! (add-wins), and a column or word this reader does not know stays visible, raw and
//! read-only.

use super::{
    Collection, Column, Kind, MAX_KEY, OnConflict, Ty, column_name, guard, keys, pk_grammar,
};
use crate::{
    Error, Result,
    database::{Cell, Change},
};
use astream_wire::sync::Digest;
use aterm_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// A table's projected cells: `(collection, key)` to its candidates.
pub type Cells = BTreeMap<(String, String), Cell>;

/// The parts of a cell key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellKey<'a> {
    pub pk: &'a str,
    pub col: &'a str,
    /// One element of a set column.
    pub elem: Option<&'a str>,
}

impl<'a> CellKey<'a> {
    /// The stored key.
    ///
    /// # Errors
    /// A part breaks its grammar, or the key passes the 64-byte name limit. An address can
    /// never be a key: `@` is in no grammar.
    pub fn encode(&self) -> Result<String> {
        if !pk_grammar(self.pk)
            || !column_name(self.col)
            || self.elem.is_some_and(|elem| !pk_grammar(elem))
        {
            return Err(Error::new("a typed cell key breaks its grammar"));
        }
        let len = self.pk.len() + 1 + self.col.len() + self.elem.map_or(0, |e| 1 + e.len());
        if len > MAX_KEY {
            return Err(Error::new("a typed cell key passes 64 bytes"));
        }
        let mut key = String::with_capacity(len);
        key.push_str(self.pk);
        key.push('.');
        key.push_str(self.col);
        if let Some(elem) = self.elem {
            key.push('.');
            key.push_str(elem);
        }
        Ok(key)
    }

    /// Split a stored key.
    ///
    /// # Errors
    /// It is not `<pk>.<col>` or `<pk>.<col>.<elem>` in the grammars.
    pub fn parse(key: &'a str) -> Result<Self> {
        let malformed = || Error::new("not a typed cell key");
        if key.len() > MAX_KEY {
            return Err(malformed());
        }
        let mut parts = key.split('.');
        let (Some(pk), Some(col)) = (parts.next(), parts.next()) else {
            return Err(malformed());
        };
        let elem = parts.next();
        if parts.next().is_some()
            || !pk_grammar(pk)
            || !column_name(col)
            || elem.is_some_and(|elem| !pk_grammar(elem))
        {
            return Err(malformed());
        }
        Ok(Self { pk, col, elem })
    }
}

/// One stored cell value.
#[derive(Clone, Debug, PartialEq)]
pub struct CellValue {
    /// The writer's schema version of the collection.
    pub r: u32,
    /// The typed value; `null` clears the cell.
    pub x: Value,
    /// The import source that wrote it, so a later import can tell its own writes from the
    /// owner's edits (design §9, three-way import).
    pub s: Option<String>,
    /// Written by an automatic path, so it counts against the writer's quota (design §3.7).
    pub a: bool,
}

impl CellValue {
    /// An owner's value.
    pub const fn owner(r: u32, x: Value) -> Self {
        Self {
            r,
            x,
            s: None,
            a: false,
        }
    }

    /// A value the importer wrote from `source` (a source pk).
    pub fn imported(r: u32, x: Value, source: &str) -> Self {
        Self {
            r,
            x,
            s: Some(source.to_owned()),
            a: false,
        }
    }

    /// A value an automatic path wrote (a birth row, a seed).
    pub const fn automatic(r: u32, x: Value) -> Self {
        Self {
            r,
            x,
            s: None,
            a: true,
        }
    }

    /// The stored object.
    pub fn encode(&self) -> Value {
        let mut object = Map::new();
        object.insert("r".into(), Value::from(u64::from(self.r)));
        object.insert("x".into(), self.x.clone());
        if let Some(source) = &self.s {
            object.insert("s".into(), Value::String(source.clone()));
        }
        if self.a {
            object.insert("a".into(), Value::Bool(true));
        }
        Value::Object(object)
    }

    /// Read a stored object; any key outside `r`, `x`, `s`, `a` refuses it.
    ///
    /// # Errors
    /// It is not a cell value.
    pub fn decode(value: &Value) -> Result<Self> {
        let malformed = || Error::new("not a typed cell value");
        let object = value.as_object().ok_or_else(malformed)?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "r" | "x" | "s" | "a"))
        {
            return Err(malformed());
        }
        let r = object
            .get("r")
            .and_then(Value::as_u64)
            .and_then(|r| u32::try_from(r).ok())
            .ok_or_else(malformed)?;
        let x = object.get("x").cloned().ok_or_else(malformed)?;
        let s = match object.get("s") {
            None => None,
            Some(Value::String(source)) if keys::is_digest_pk(source, "sr") => Some(source.clone()),
            Some(_) => return Err(malformed()),
        };
        let a = match object.get("a") {
            None => false,
            Some(Value::Bool(true)) => true,
            Some(_) => return Err(malformed()),
        };
        Ok(Self { r, x, s, a })
    }
}

fn refuse(collection: &Collection, column: &str, rule: &str) -> Error {
    Error::new(&format!("{}.{column}: {rule}", collection.name))
}

/// Check one value against a type. The rule it breaks names no part of the value.
fn check(ty: &Ty, x: &Value, null: bool) -> std::result::Result<(), &'static str> {
    if x.is_null() {
        return if null {
            Ok(())
        } else {
            Err("this column needs a value")
        };
    }
    let text = || x.as_str().ok_or("this column holds text");
    match ty {
        Ty::Bool => x
            .as_bool()
            .map(drop)
            .ok_or("this column holds true or false"),
        Ty::Int { min, max } => x
            .as_i64()
            .filter(|n| (*min..=*max).contains(n))
            .map(drop)
            .ok_or("this column holds an integer in its range"),
        Ty::Uint | Ty::Time | Ty::Day => x
            .as_u64()
            .map(drop)
            .ok_or("this column holds a whole number"),
        Ty::Text(max) => guard::text(text()?, *max).map_err(guard::Refusal::rule),
        Ty::AccountLabel(max) => guard::account_label(text()?, *max).map_err(guard::Refusal::rule),
        Ty::Enum(words) => {
            let word = text()?;
            if words.contains(&word) {
                Ok(())
            } else {
                Err("this word is not one the column knows")
            }
        }
        Ty::Label => keys::is_label(text()?)
            .then_some(())
            .ok_or("a machine label is m<N> or m<N>-xxxx, lower-case"),
        Ty::Digits(count) => {
            let digits = text()?;
            (digits.len() == *count && digits.bytes().all(|b| b.is_ascii_digit()))
                .then_some(())
                .ok_or("this column holds exactly its count of digits")
        }
        Ty::Hex(digits) => {
            let hex = text()?;
            (hex.len() == *digits
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
            .then_some(())
            .ok_or("this column holds lower-case hex of its length")
        }
        Ty::Email => keys::is_stored_address(text()?)
            .then_some(())
            .ok_or("an address is stored lower-case and trimmed"),
        Ty::Url => {
            let url = text()?;
            let rest = url.strip_prefix("https://").ok_or("a URL is https")?;
            if rest.is_empty()
                || url.len() > 2048
                || rest.contains(['?', '#', '@'])
                || rest.chars().any(char::is_whitespace)
            {
                return Err("a URL has no query, fragment or user part");
            }
            guard::word(url).map_err(guard::Refusal::rule)
        }
        Ty::Secret => super::secret::is_sealed(text()?)
            .then_some(())
            .ok_or("a secret cell holds an authenticated opaque token"),
        Ty::SecretRef => {
            let object = x.as_object().ok_or("a vault reference names its entry")?;
            let names = |key: &str| {
                object
                    .get(key)
                    .and_then(Value::as_str)
                    .is_some_and(crate::crypto::name)
            };
            if !object
                .keys()
                .all(|key| matches!(key.as_str(), "vault" | "entry" | "item"))
                || !names("vault")
                || !names("entry")
                || (object.contains_key("item") && !names("item"))
            {
                return Err("a vault reference holds only vault, entry and item names");
            }
            object
                .values()
                .filter_map(Value::as_str)
                .try_for_each(|name| guard::word(name).map_err(guard::Refusal::rule))
        }
        Ty::Ref(_) => {
            let target = text()?;
            if !pk_grammar(target) {
                return Err("a reference is a key");
            }
            guard::word(target).map_err(guard::Refusal::rule)
        }
        Ty::Set(_) => Err("a set is written one element at a time"),
        Ty::List(inner, max) => {
            let items = x.as_array().ok_or("this column holds a list")?;
            if items.len() > *max {
                return Err("this list is longer than its column allows");
            }
            items.iter().try_for_each(|item| check(inner, item, false))
        }
        Ty::Struct(members) => {
            let object = x.as_object().ok_or("this column holds a record")?;
            if object
                .keys()
                .any(|key| !members.iter().any(|m| m.name == key))
            {
                return Err("this record has a field its type does not name");
            }
            members
                .iter()
                .try_for_each(|member| match object.get(member.name) {
                    None if member.null => Ok(()),
                    None => Err("this record lacks a required field"),
                    Some(value) => check(&member.ty, value, member.null),
                })
        }
        Ty::SrcRef => {
            let object = x.as_object().ok_or("a source coordinate is a record")?;
            let whole = |key: &str| object.get(key).and_then(Value::as_u64).is_some();
            if object.len() == 3
                && object
                    .get("source")
                    .and_then(Value::as_str)
                    .is_some_and(|source| keys::is_digest_pk(source, "sr"))
                && whole("row")
                && whole("col")
            {
                Ok(())
            } else {
                Err("a source coordinate is {source, row, col}, never a value")
            }
        }
        Ty::Sealed(max) => aterm_secrets::b64::decode_padded(text()?)
            .filter(|wire| !wire.is_empty() && wire.len() <= *max)
            .map(drop)
            .ok_or("this column holds one sealed record within its size"),
        Ty::Json(max) => {
            let size = aterm_json::to_vec(x).map_or(usize::MAX, |bytes| bytes.len());
            if size > *max {
                return Err("this value is larger than its column allows");
            }
            check_json_strings(x)
        }
    }
}

fn check_json_str(text: &str) -> std::result::Result<(), &'static str> {
    if text.contains('@') {
        Err(guard::Refusal::Address.rule())
    } else if guard::phone_shaped(text) {
        Err(guard::Refusal::Phone.rule())
    } else {
        guard::word(text).map_err(guard::Refusal::rule)
    }
}

fn check_json_strings(value: &Value) -> std::result::Result<(), &'static str> {
    match value {
        Value::String(text) => check_json_str(text),
        Value::Array(items) => items.iter().try_for_each(check_json_strings),
        Value::Object(object) => {
            for (k, v) in object {
                check_json_str(k)?;
                check_json_strings(v)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Check that `elem` can be an element of the set column `column`.
fn check_elem(column: &Column, elem: &str) -> std::result::Result<(), &'static str> {
    let Ty::Set(inner) = column.ty else {
        return Err("only a set column has elements");
    };
    match inner {
        Ty::Enum(words) if !words.contains(&elem) => Err("this word is not one the column knows"),
        Ty::Label if !keys::is_label(elem) => Err("a machine label is m<N> or m<N>-xxxx"),
        _ if !pk_grammar(elem) => Err("a set element breaks the key grammar"),
        Ty::Enum(_) | Ty::Label | Ty::Ref(_) => guard::word(elem).map_err(guard::Refusal::rule),
        _ => check(inner, &Value::String(elem.to_owned()), false),
    }
}

/// Whether `x` is a value of `ty` that passes the guards (null passes): what a reader checks
/// before it shows a value another writer stored, since an older or modified build may have
/// written one this build would refuse. A set passes when each element passes its type.
pub fn admissible(ty: &Ty, x: &Value) -> bool {
    match (ty, x) {
        (Ty::Set(inner), Value::Array(items)) => {
            items.iter().all(|item| check(inner, item, false).is_ok())
        }
        _ => check(ty, x, true).is_ok(),
    }
}

/// Validate one typed value for `column` of `collection`.
///
/// # Errors
/// The rule it breaks, naming the collection and column, never the value.
pub fn validate(collection: &Collection, column: &Column, x: &Value) -> Result<()> {
    check(&column.ty, x, column.null).map_err(|rule| refuse(collection, column.name, rule))
}

/// One change writing `value` to `key` of a cells `collection`, checked against the compiled
/// schema: the column must exist at the collection's version, a set element holds `true` or
/// `false`, and every other value passes its type and the guards.
///
/// # Errors
/// The collection is not cells, the key or value breaks a rule, or the column is unknown or
/// retired.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "NewRegion",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "OldEdit",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "NewEdit",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
pub fn change(collection: &Collection, key: CellKey<'_>, value: &CellValue) -> Result<Change> {
    if collection.kind != Kind::Cells {
        return Err(refuse(collection, key.col, "this collection is not cells"));
    }
    guard::word(key.pk).map_err(|r| refuse(collection, key.col, r.rule()))?;
    let column = collection
        .column(key.col)
        .filter(|column| column.live_at(collection.version))
        .ok_or_else(|| refuse(collection, key.col, "no such live column"))?;
    match key.elem {
        Some(elem) => {
            check_elem(column, elem).map_err(|rule| refuse(collection, column.name, rule))?;
            if value.x.as_bool().is_none() {
                return Err(refuse(
                    collection,
                    column.name,
                    "a set element is true or false",
                ));
            }
        }
        None => validate(collection, column, &value.x)?,
    }
    Ok(Change {
        collection: collection.name.to_owned(),
        key: key.encode()?,
        value: Some(value.encode()),
    })
}

/// One concurrent value of a cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub operation: Digest,
    pub value: CellValue,
}

/// A projected column of one row.
#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    /// One value, after equal candidates collapsed.
    Value(Candidate),
    /// Different concurrent values, each kept, with the column rule's provisional value.
    Conflict {
        candidates: Vec<Candidate>,
        provisional: Option<Value>,
    },
    /// A set: each element's own register.
    Set(BTreeMap<String, Field>),
}

impl Field {
    /// The value a view shows: the value, or a conflict's provisional value.
    pub fn value(&self) -> Option<&Value> {
        match self {
            Self::Value(candidate) => Some(&candidate.value.x),
            Self::Conflict { provisional, .. } => provisional.as_ref(),
            Self::Set(_) => None,
        }
    }

    /// Every candidate's value of a register: one, or each of a conflict's; none for a set.
    pub fn candidates_x(&self) -> Vec<&Value> {
        match self {
            Self::Value(candidate) => vec![&candidate.value.x],
            Self::Conflict { candidates, .. } => candidates.iter().map(|c| &c.value.x).collect(),
            Self::Set(_) => Vec::new(),
        }
    }

    /// Whether this register holds different concurrent values.
    pub const fn conflicted(&self) -> bool {
        matches!(self, Self::Conflict { .. })
    }

    /// For a set element: present when any candidate adds it (add-wins).
    pub fn present(&self) -> bool {
        match self {
            Self::Value(candidate) => candidate.value.x == Value::Bool(true),
            Self::Conflict { candidates, .. } => candidates
                .iter()
                .any(|candidate| candidate.value.x == Value::Bool(true)),
            Self::Set(_) => false,
        }
    }

    /// For a set: whether `elem` is present.
    pub fn has_member(&self, elem: &str) -> bool {
        match self {
            Self::Set(elements) => elements.get(elem).is_some_and(Self::present),
            _ => false,
        }
    }

    /// For a set: its present elements.
    pub fn members(&self) -> BTreeSet<&str> {
        match self {
            Self::Set(elements) => elements
                .iter()
                .filter(|(_, field)| field.present())
                .map(|(elem, _)| elem.as_str())
                .collect(),
            _ => BTreeSet::new(),
        }
    }
}

/// One projected row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub fields: BTreeMap<String, Field>,
    /// Columns this reader cannot interpret, a newer build's column or word: kept, shown raw
    /// and read-only, never dropped.
    pub raw: BTreeSet<String>,
}

impl Row {
    /// A column's shown value.
    pub fn get(&self, column: &str) -> Option<&Value> {
        self.fields.get(column).and_then(Field::value)
    }

    /// The lifecycle word (`live`, `retired`, `ended`, …).
    pub fn lifecycle(&self) -> Option<&str> {
        self.get(super::LIFECYCLE).and_then(Value::as_str)
    }

    /// Whether the row is live: its lifecycle says so, or no lifecycle was written yet. A
    /// conflicted lifecycle is not live, whatever its provisional word (a newer build's word
    /// the column's rule does not rank leaves `live` provisional).
    pub fn live(&self) -> bool {
        self.fields
            .get(super::LIFECYCLE)
            .is_none_or(|field| !field.conflicted())
            && self.lifecycle().is_none_or(|word| word == "live")
    }
}

/// One collection's rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projection {
    pub rows: BTreeMap<String, Row>,
    /// Keys or values in this collection that are not typed cells at all.
    pub malformed: usize,
}

/// The provisional value of a conflict under `rule`. Candidates arrive sorted by operation
/// digest, so `show` takes the smallest; nothing here reads a clock.
fn provisional(rule: OnConflict, candidates: &[Candidate]) -> Option<Value> {
    let first = || candidates.first().map(|c| c.value.x.clone());
    let number = |c: &Candidate| c.value.x.as_i64();
    match rule {
        OnConflict::Show => first(),
        OnConflict::Prefer(order) => order
            .iter()
            .find_map(|word| {
                candidates
                    .iter()
                    .find(|c| c.value.x.as_str() == Some(word))
                    .map(|c| c.value.x.clone())
            })
            .or_else(first),
        OnConflict::Max => candidates
            .iter()
            .filter_map(|c| number(c).map(|n| (n, c)))
            .max_by_key(|(n, _)| *n)
            .map(|(_, c)| c.value.x.clone()),
        OnConflict::Min => candidates
            .iter()
            .filter_map(|c| number(c).map(|n| (n, c)))
            .min_by_key(|(n, _)| *n)
            .map(|(_, c)| c.value.x.clone()),
        OnConflict::Hold => None,
    }
}

/// A register's field: equal values collapse to one candidate, the owner's first (no import
/// source, not automatic), then the smallest operation digest, so an owner's value never reads
/// as an import's because an import wrote the same value too (an import writes only over its
/// own, [`import_step`]); different ones stay a conflict.
fn field(rule: OnConflict, mut candidates: Vec<Candidate>) -> Option<Field> {
    candidates.sort_by_key(|c| (c.value.s.is_some(), c.value.a, c.operation));
    let mut distinct: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        if !distinct
            .iter()
            .any(|kept| kept.value.x == candidate.value.x)
        {
            distinct.push(candidate);
        }
    }
    match distinct.len() {
        0 => None,
        1 => distinct.pop().map(Field::Value),
        _ => Some(Field::Conflict {
            provisional: provisional(rule, &distinct),
            candidates: distinct,
        }),
    }
}

/// The most candidates one carried register holds.
pub const MAX_CARRIED: usize = 16;

/// The candidates a table roll carried into one register (design §3.8): a roll restates a
/// register of the generation before as one value, `{"k": [{"o": <hex operation>, "v": <its
/// value>}, …]}`, each item a candidate the register held there, in operation order, with the
/// operation that wrote it. One writer's records are causally ordered, so a roll cannot write
/// concurrent candidates; the list is how an open conflict stays open across generations, and
/// `o` is how the next roll tells a candidate it carried from one it has not seen. `None` for
/// any value that is not exactly that shape.
pub fn carried(value: &Value) -> Option<Vec<(Digest, Value)>> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let items = object.get("k")?.as_array()?;
    if items.is_empty() || items.len() > MAX_CARRIED {
        return None;
    }
    let mut out: Vec<(Digest, Value)> = Vec::with_capacity(items.len());
    for item in items {
        let item = item.as_object()?;
        if item.len() != 2 {
            return None;
        }
        let mut operation = [0; 32];
        let hex = item.get("o")?.as_str()?;
        if !aterm_codec::hex::is_lower(hex, 64)
            || !aterm_codec::hex::decode_into(hex.as_bytes(), &mut operation)
        {
            return None;
        }
        let inner = item.get("v")?;
        if carried(inner).is_some() || out.last().is_some_and(|(last, _)| *last >= operation) {
            return None;
        }
        out.push((operation, inner.clone()));
    }
    Some(out)
}

/// The carried value of `candidates` ([`carried`]), sorted by operation with each operation
/// once. `None` when there are none or more than [`MAX_CARRIED`].
pub fn carry(candidates: &BTreeMap<Digest, Value>) -> Option<Value> {
    if candidates.is_empty() || candidates.len() > MAX_CARRIED {
        return None;
    }
    let items: Vec<Value> = candidates
        .iter()
        .map(|(operation, value)| {
            let mut item = Map::new();
            item.insert(
                "o".into(),
                Value::String(aterm_codec::hex::encode_lower(operation)),
            );
            item.insert("v".into(), value.clone());
            Value::Object(item)
        })
        .collect();
    let mut object = Map::new();
    object.insert("k".into(), Value::Array(items));
    Some(Value::Object(object))
}

/// A register's candidates with the operation each came from: a carried value ([`carried`])
/// stands for the candidates it lists, each at its own operation; any other value is its own
/// record's.
pub fn origins(cell: &Cell) -> Vec<(Digest, Value)> {
    let mut out = Vec::new();
    for candidate in &cell.candidates {
        match carried(&candidate.value) {
            Some(items) => out.extend(items),
            None => out.push((candidate.operation, candidate.value.clone())),
        }
    }
    out
}

/// Read one cells collection out of a table's projected cells.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "WriteA1",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "WriteA2",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "WriteB1",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "WriteB2",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "NewRegion",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "OldEdit",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "NewEdit",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
pub fn project(collection: &Collection, cells: &Cells) -> Projection {
    let mut projection = Projection::default();
    for ((_, key), cell) in cells
        .range((collection.name.to_owned(), String::new())..)
        .take_while(|((name, _), _)| name == collection.name)
    {
        let Ok(parsed) = CellKey::parse(key) else {
            projection.malformed += 1;
            continue;
        };
        if cell.stopped {
            projection.malformed += 1;
            continue;
        }
        let mut candidates = Vec::new();
        let mut unreadable = false;
        let mut decode_candidate = |operation: Digest, value: &Value| {
            // A revert that restores absence leaves a bare null: the cell reads as empty.
            let value = if value.is_null() {
                CellValue::owner(0, Value::Null)
            } else if let Ok(value) = CellValue::decode(value) {
                value
            } else {
                unreadable = true;
                return;
            };
            candidates.push(Candidate { operation, value });
        };
        for candidate in &cell.candidates {
            match carried(&candidate.value) {
                Some(items) => {
                    for (operation, value) in items {
                        decode_candidate(operation, &value);
                    }
                }
                None => decode_candidate(candidate.operation, &candidate.value),
            }
        }
        let row = projection.rows.entry(parsed.pk.to_owned()).or_default();
        let column = collection.column(parsed.col);
        let rule = column.map_or(OnConflict::Show, |c| c.on_conflict);
        let readable = !unreadable
            && column.is_some_and(|column| match parsed.elem {
                Some(elem) => {
                    check_elem(column, elem).is_ok()
                        && candidates.iter().all(|c| c.value.x.as_bool().is_some())
                }
                None => candidates
                    .iter()
                    .all(|c| check(&column.ty, &c.value.x, true).is_ok()),
            });
        if !readable {
            row.raw.insert(parsed.col.to_owned());
        }
        let Some(field) = field(rule, candidates) else {
            continue;
        };
        match parsed.elem {
            Some(elem) => {
                let set = row
                    .fields
                    .entry(parsed.col.to_owned())
                    .or_insert_with(|| Field::Set(BTreeMap::new()));
                if let Field::Set(elements) = set {
                    elements.insert(elem.to_owned(), field);
                } else {
                    row.raw.insert(parsed.col.to_owned());
                }
            }
            None => {
                if row.fields.insert(parsed.col.to_owned(), field).is_some() {
                    row.raw.insert(parsed.col.to_owned());
                }
            }
        }
    }
    projection
}

/// What an import does with one cell (design §9: three-way, never over the owner).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportStep {
    /// Write the import's value.
    Write,
    /// The cell already holds it: no record is spent.
    Unchanged,
    /// Leave the cell and report why.
    Finding(&'static str),
}

/// Decide one cell of a re-import against the current projection: write only a cell that is
/// absent or that an earlier import wrote (its value names a source); a cell the owner set,
/// cleared or resolved since, or a conflicted one, is left alone and reported. For a set,
/// pass the element's own field.
pub fn import_step(current: Option<&Field>, new: &Value) -> ImportStep {
    match current {
        None => ImportStep::Write,
        Some(Field::Value(candidate)) if candidate.value.x == *new => ImportStep::Unchanged,
        Some(Field::Value(candidate)) if candidate.value.s.is_some() => ImportStep::Write,
        Some(Field::Value(_)) => {
            ImportStep::Finding("the owner set this cell since the import; it is left as it is")
        }
        Some(Field::Conflict { .. }) => {
            ImportStep::Finding("this cell is conflicted; the owner resolves it first")
        }
        Some(Field::Set(_)) => ImportStep::Finding("a set is imported one element at a time"),
    }
}
