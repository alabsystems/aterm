// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What `lab/tracker` maps from the lab's own words (docs/DESIGN-lab-tracker-2026-11-14.md §3,
//! §7 finding 7): a row's key from the word it is named by, and the state words and deciders
//! the lab tracker holds that the tracker's types do not.
//!
//! - **Keys.** A key is `[A-Za-z0-9_-]{1,26}` with no dot, so a cell key splits at its dots
//!   (`typed::pk_grammar`). A word in that grammar is its own key; any other (`TI-W0.1`, a
//!   decision id of 37 bytes, `agent harnesses`) is keyed by its mapped form ([`key`]). The
//!   row keeps the word in `id` either way, and a row whose key is not the one its `id` makes
//!   is a finding ([`super::checks`]).
//! - **States.** `waiting` is no package state and `active` no decision state; each maps to
//!   the tracker's word and the row keeps what was said in `state_said`, a finding until
//!   someone clears it ([`package_state`], [`decision_state`]).
//! - **Deciders.** `decided_by` is a principal; free text (`AI`, `owner`, an agent's name) is
//!   kept as what that principal said ([`decided_by`]), a finding likewise.

use super::tables::{CHANGE, DECISION_STATES, PACKAGE_STATES, RULING, STOP};
use crate::typed::{Collection, cell::Cells, guard, pk_grammar, values::Principal};
use crate::{Error, Result};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::Value;
use std::collections::BTreeMap;

/// The bytes of a mapped key's head: what is left of the 26 a key may have after `_` and the
/// digest's ten hex digits.
const HEAD: usize = 15;

/// The key of the row named `id`: `id` itself where it is a key a cell takes (`TK-10`,
/// `tables`, `A11`: the key grammar, and nothing the key guard reads as a credential), else its
/// mapped form, the first 15 bytes of `id` with each byte that is not an ASCII letter, a digit
/// or `-` as `-` (a `_` too, so a mapped key's one `_` is the one before its digest), then `_`
/// and ten hex digits of a SHA-256 of the whole `id` (`TI-W0.1` is `TI-W0-1_<10 hex>`). The
/// head keeps a mapped key readable and sorted beside its neighbours; the digest keeps two ids
/// that share a head apart. Where the key guard would refuse that form too (an id that starts
/// as a provider's token does), the key is `id_` and twenty hex digits of the same digest,
/// which no guard refuses. Deterministic, so every machine keys one id alike, and a reference
/// to the row (`depends`) is written with the same function.
///
/// A word in the key grammar that is also some other id's mapped form names one row with it:
/// the second of the two is refused at its birth, because the table already holds the row.
#[must_use]
pub fn key(id: &str) -> String {
    if pk_grammar(id) && guard::word(id).is_ok() {
        return id.to_owned();
    }
    let sum = digest(b"aterm/lab/tracker/key/1\0", &[id.as_bytes()]);
    let mut key: String = id
        .bytes()
        .take(HEAD)
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' {
                char::from(b)
            } else {
                '-'
            }
        })
        .collect();
    key.push('_');
    hex::push_lower(&mut key, &sum[..5]);
    if guard::word(&key).is_ok() {
        return key;
    }
    let mut key = String::from("id_");
    hex::push_lower(&mut key, &sum[..10]);
    key
}

/// A SHA-256 of `domain` and `parts`, each part apart from the next.
fn digest(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut sha = Sha256::new();
    sha.update(domain);
    for (n, part) in parts.iter().enumerate() {
        if n > 0 {
            sha.update([0]);
        }
        sha.update(part);
    }
    sha.finalize()
}

/// `<prefix>-<16 hex>` of a digest of `parts`: the key of a row with no word of its own.
fn digest_key(prefix: &str, domain: &[u8], parts: &[&[u8]]) -> String {
    let mut key = String::with_capacity(prefix.len() + 17);
    key.push_str(prefix);
    key.push('-');
    hex::push_lower(&mut key, &digest(domain, parts)[..8]);
    key
}

/// A change's key: a digest of its package, the column and the value it asks for.
#[must_use]
pub fn change_key(package: &str, column: &str, after: &Value) -> String {
    digest_key(
        CHANGE.prefix,
        b"aterm/lab/tracker/change/1\0",
        &[
            package.as_bytes(),
            column.as_bytes(),
            after.to_string().as_bytes(),
        ],
    )
}

/// A ruling's key: a digest of its day and its words.
#[must_use]
pub fn ruling_key(date: u64, quote: &str) -> String {
    digest_key(
        RULING.prefix,
        b"aterm/lab/tracker/ruling/1\0",
        &[date.to_string().as_bytes(), quote.as_bytes()],
    )
}

/// A stop's key: a digest of its lane and its words.
#[must_use]
pub fn stop_key(lane: &str, item: &str) -> String {
    digest_key(
        STOP.prefix,
        b"aterm/lab/tracker/stop/1\0",
        &[lane.as_bytes(), item.as_bytes()],
    )
}

/// The key the row of `collection` with these `values` has, where they hold every column its
/// key is made from: [`key`] of its `id`, or the digest of a change, a ruling or a stop.
/// `None` where a column is missing or is not of its type. A conflicted cell is read as `value`
/// gives it: the checks pass a row's shown value, which for a column a key is made from is the
/// conflict's provisional candidate; the conflict itself is the table layer's finding.
#[must_use]
pub fn key_of(collection: &Collection, value: impl Fn(&str) -> Option<Value>) -> Option<String> {
    let text = |column: &str| value(column).and_then(|x| x.as_str().map(str::to_owned));
    if collection.name == CHANGE.name {
        // The value asked for is in the column `column` names.
        let column = text("column")?;
        let after = value(&column).filter(|after| !after.is_null())?;
        return Some(change_key(&text("package")?, &column, &after));
    }
    if collection.name == RULING.name {
        return Some(ruling_key(value("date")?.as_u64()?, &text("quote")?));
    }
    if collection.name == STOP.name {
        return Some(stop_key(&text("lane")?, &text("item")?));
    }
    collection.column("id")?;
    Some(key(&text("id")?))
}

/// The key a new row of `collection` is born under (`att::Birth`): the one its cells make
/// ([`key_of`]). A row is named by exactly what it says, so an `id` with white space around it
/// is refused rather than trimmed into another row's name.
///
/// # Errors
/// A column the key is made from is missing, or the `id` is empty or has white space around it.
pub fn birth(
    collection: &Collection,
    values: &BTreeMap<String, Value>,
    _: &Cells,
) -> Result<String> {
    if let Some(id) = values.get("id").and_then(Value::as_str)
        && (id.is_empty() || id.trim() != id)
    {
        return Err(Error::new(&format!(
            "{}.id: a row is named by a word with no white space around it",
            collection.name
        )));
    }
    key_of(collection, |column| values.get(column).cloned()).ok_or_else(|| {
        Error::new(&format!(
            "{}: a new row's key is made from {}; fill in what it needs",
            collection.name,
            made_from(collection).join(", "),
        ))
    })
}

/// The columns `collection`'s key is made from (a change's, of `exit`, `outcome` and `state`,
/// the one its `column` names).
#[must_use]
pub fn made_from(collection: &Collection) -> &'static [&'static str] {
    match collection.name {
        "change" => &["package", "column", "exit", "outcome", "state"],
        "ruling" => &["date", "quote"],
        "stop" => &["lane", "item"],
        _ => &["id"],
    }
}

/// A state word mapped to one of the tracker's: the word a row states, and what the lab
/// tracker said where that was another word, which the row keeps in `state_said`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State<'a> {
    pub state: &'static str,
    pub said: Option<&'a str>,
}

fn state<'a>(
    words: &'static [&'static str],
    mapped: &[(&str, &'static str)],
    said: &'a str,
) -> Option<State<'a>> {
    if let Some(state) = words.iter().copied().find(|word| *word == said) {
        return Some(State { state, said: None });
    }
    mapped
        .iter()
        .find(|(from, _)| *from == said)
        .map(|&(_, state)| State {
            state,
            said: Some(said),
        })
}

/// The package state `said` maps to (§3, §7 finding 7): one of the tracker's five as itself,
/// and `waiting` as `ready`. The lab tracker's `waiting` is a package nobody is working on that
/// is not ready to claim, by its unfinished dependencies or a holder's own word; the tracker
/// computes the first from `depends` and has no stored word for the second, so the row is
/// `ready` and says `waiting` in `state_said`, a finding until someone gives it one of the
/// five. `None` for any other word, which the import reports (TK-11).
#[must_use]
pub fn package_state(said: &str) -> Option<State<'_>> {
    state(PACKAGE_STATES, &[("waiting", "ready")], said)
}

/// The decision state `said` maps to: one of the tracker's four as itself, and `active` as
/// `decided`. An `active` decision is one in force, which is what `decided` says; `standing`
/// is kept for a rule a lane restates, and nothing tells the two apart from the word alone, so
/// the row says `active` in `state_said`, a finding. `None` for any other word.
#[must_use]
pub fn decision_state(said: &str) -> Option<State<'_>> {
    state(DECISION_STATES, &[("active", "decided")], said)
}

/// Who decided, from the lab tracker's free text (`AI`, `owner`, `aterm-perf-s8`): a principal
/// that says only that ([`Principal::said`]). No session, program or machine can be read out
/// of a word, so the decision is a finding until a principal aterm stated replaces it.
#[must_use]
pub fn decided_by(said: &str) -> Principal {
    Principal::said(said)
}
