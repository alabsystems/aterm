// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Address-list discovery (docs/DESIGN-alab-lab-model-2026-11-01.md §4.8, §4.10, §6, step 10):
//! what a Mac records about a Proton mailbox it already opened to read a sign-in code. The list
//! is the one the mailbox's own unlock read, so discovery makes no request of its own; it writes
//! this Mac's `mailbox-obs` row (address keys only) and, for the address being signed in when the
//! list receives it and no row holds it, that address's birth, in one record under discovery's
//! share of the per-Mac quota, only when what the list says changed: at once for a birth, else at
//! most monthly per mailbox.
//!
//! A mailbox's login is an address row like any other (docs/DESIGN-alab-one-mesh-2026-11-05.md
//! §4.7), so the observation lists every receiving address the registry already holds, the
//! login and the account's own addresses among them. Only addresses the registry already holds
//! are listed, and only the address being signed in is born, so the owner's other aliases stay
//! out of a lab-wide table that never compacts. Which mailbox row the open mailbox is comes from
//! its login (a mailbox's key derives from its login's address key, `keys::mailbox_pk`) or,
//! failing that, from the owner's `mailbox` cell on a listed address; an address text never
//! reaches a key, a finding or a log line.
//!
//! [`plan`] is pure over the registry; `Registry::observe_mailbox` runs it under the writer.

use super::{
    checks::Finding,
    register::day_of,
    registry::{ADDRESS, MAILBOX, MAILBOX_OBS},
};
use crate::{
    Result,
    database::Change,
    typed::{
        LIFECYCLE,
        cell::{self, CellKey, CellValue, Cells, Projection},
        keys,
        owned::OwnedRead,
    },
};
use aterm_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The most addresses one observation lists (`mailbox-obs.addresses`).
pub const MAX_LISTED: usize = 512;

/// Days between two rewrites of a Mac's observation that carry no birth. An observation spends a
/// record of this Mac's lifetime quota, and what it lists moves with every row anyone adds that
/// the mailbox receives, so a list that only changed rides a monthly rewrite, as registration's
/// host names do; a birth is written at once, since the address being signed in needs its row.
pub const REWRITE_DAYS: u64 = 30;

/// The records of this Mac's quota of automatic records (24 a generation) that discovery leaves
/// to registration, whose `seen` refresh needs about 12 a year: discovery writes only while more
/// than these remain, so it spends at most half a generation and never registration's share.
pub const REGISTRATION_RESERVE: usize = 12;

/// The `method` word of a list read through Proton's API.
pub const PROTON_API: &str = "proton-api";

/// One address of a mailbox's list, as the mailbox reported it.
#[derive(Clone, PartialEq, Eq)]
pub struct Listed {
    pub addr: String,
    /// Mail sent to it lands in this mailbox.
    pub receives: bool,
}

/// What one open mailbox's list said. No `Debug`: it holds addresses and the mailbox's login.
#[derive(Clone, PartialEq, Eq)]
pub struct Listing {
    /// The address this Mac signed in to the mailbox with: never observed, never written.
    pub login: String,
    /// The address whose code the mailbox was opened to read, when one was.
    pub target: Option<String>,
    pub listed: Vec<Listed>,
    /// The mailbox returned its whole list.
    pub complete: bool,
}

/// What one discovery pass decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `[commons]` turns sharing off, or excludes the registry or the catalog.
    Excluded,
    /// No Commons here, or the registry is not declared here yet.
    NotHeld,
    /// This Mac's history of the registry waits on a record.
    Incomplete,
    /// No live mailbox row is this mailbox: none is keyed by its login and no listed address
    /// carries the owner's mailbox link.
    Unknown,
    /// The owner's links on listed addresses name more than one mailbox; the findings say which.
    Held,
    /// The list lists nothing the registry holds, and this Mac has no row to change.
    Empty,
    /// This Mac's row already says what the list says.
    Unchanged,
    /// This Mac's row changed within [`REWRITE_DAYS`] and the list births nothing: the change
    /// waits for a later open.
    Rested { mailbox: String },
    /// This Mac's row was written: `listed` address keys, `born` of them new rows.
    Observed {
        mailbox: String,
        listed: usize,
        born: usize,
    },
    /// The row would pass the share of this Mac's quota of automatic records that discovery may
    /// spend ([`REGISTRATION_RESERVE`]).
    QuotaSpent,
}

impl Outcome {
    /// The word a log line reports this outcome by.
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Excluded => "excluded",
            Self::NotHeld => "not-held",
            Self::Incomplete => "incomplete",
            Self::Unknown => "mailbox-unknown",
            Self::Held => "held",
            Self::Empty => "nothing-listed",
            Self::Unchanged => "unchanged",
            Self::Rested { .. } => "rested",
            Self::Observed { .. } => "observed",
            Self::QuotaSpent => "quota-spent",
        }
    }
}

/// What [`plan`] returns: at most one record.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub outcome: Outcome,
    pub changes: Vec<Change>,
    pub findings: Vec<Finding>,
}

impl Plan {
    fn quiet(outcome: Outcome) -> Self {
        Self {
            outcome,
            changes: Vec::new(),
            findings: Vec::new(),
        }
    }
}

/// What [`plan`] reads besides the registry.
#[derive(Clone, Copy)]
pub struct Input<'a> {
    /// This Mac's Commons uuid, 32 hex: the suffix of every observation it writes.
    pub uuid: &'a str,
    pub listing: &'a Listing,
    /// The mailbox key the login derives (`keys::mailbox_pk`), where it is an address.
    pub keyed: Option<&'a str>,
    pub now_ms: u64,
}

/// The addresses the list receives, by key, each in the claims spelling.
fn receiving(listing: &Listing) -> BTreeMap<String, String> {
    listing
        .listed
        .iter()
        .filter(|listed| listed.receives)
        .filter_map(|listed| keys::address(&listed.addr).ok())
        .filter_map(|addr| Some((keys::address_pk(&addr).ok()?, addr)))
        .collect()
}

/// The mailbox row the open mailbox is: its login's derived key when that row is live, else the one
/// live mailbox the owner linked a listed address to. `Err` holds the candidates when the links
/// name several.
fn identify(
    keyed: Option<&str>,
    received: &BTreeMap<String, String>,
    addresses: &Projection,
    mailboxes: &Projection,
) -> std::result::Result<Option<String>, Vec<String>> {
    let live = |pk: &str| mailboxes.rows.get(pk).is_some_and(cell::Row::live);
    if let Some(pk) = keyed.filter(|pk| live(pk)) {
        return Ok(Some(pk.to_owned()));
    }
    let linked: BTreeSet<&str> = received
        .keys()
        .filter_map(|pk| addresses.rows.get(pk)?.fields.get("mailbox"))
        .filter(|field| !field.conflicted())
        .filter_map(|field| field.value()?.as_str())
        .filter(|pk| live(pk))
        .collect();
    match linked.len() {
        0 => Ok(None),
        1 => Ok(linked.first().map(|pk| (*pk).to_owned())),
        _ => Err(linked.into_iter().map(str::to_owned).collect()),
    }
}

fn automatic(pk: &str, col: &str, x: Value) -> Result<Change> {
    cell::change(
        &ADDRESS,
        CellKey {
            pk,
            col,
            elem: None,
        },
        &CellValue::automatic(ADDRESS.version, x),
    )
}

/// An address's birth (§4.8): `addr`, `domain`, `origin` and `row`, every cell automatic.
fn birth(pk: &str, addr: &str) -> Result<Vec<Change>> {
    let domain = addr.split_once('@').map_or("", |(_, domain)| domain);
    Ok(vec![
        automatic(pk, "addr", Value::from(addr))?,
        automatic(pk, "domain", Value::from(domain))?,
        automatic(pk, "origin", Value::from("discovered"))?,
        automatic(pk, LIFECYCLE, Value::from("live"))?,
    ])
}

fn observation(
    key: &str,
    n: u64,
    mailbox: &str,
    complete: bool,
    listed: &[String],
    at: u64,
) -> Change {
    let mut row = Map::new();
    row.insert("r".into(), Value::from(u64::from(MAILBOX_OBS.version)));
    row.insert("n".into(), Value::from(n));
    row.insert("mailbox".into(), Value::from(mailbox));
    row.insert("method".into(), Value::from(PROTON_API));
    row.insert("complete".into(), Value::from(complete));
    row.insert(
        "addresses".into(),
        Value::Array(listed.iter().map(|pk| Value::from(pk.as_str())).collect()),
    );
    row.insert("at".into(), Value::from(at));
    Change {
        collection: MAILBOX_OBS.name.into(),
        key: key.to_owned(),
        value: Some(Value::Object(row)),
    }
}

/// One pass over the registry this Mac holds: this Mac's observation of the open mailbox, and the
/// birth of the address being signed in where the list receives it and no row, live or ended,
/// holds it. A row is rewritten only when what it says changes: at once for a birth, otherwise
/// once its last rewrite is [`REWRITE_DAYS`] UTC days old.
///
/// # Errors
/// A change does not fit its column.
pub fn plan(input: &Input<'_>, cells: &Cells, obs: &OwnedRead) -> Result<Plan> {
    let received = receiving(input.listing);
    let addresses = cell::project(&ADDRESS, cells);
    let mailboxes = cell::project(&MAILBOX, cells);
    let mailbox = match identify(input.keyed, &received, &addresses, &mailboxes) {
        Ok(Some(mailbox)) => mailbox,
        Ok(None) => return Ok(Plan::quiet(Outcome::Unknown)),
        Err(candidates) => {
            return Ok(Plan {
                outcome: Outcome::Held,
                changes: Vec::new(),
                findings: vec![Finding {
                    kind: "mailbox-ambiguous",
                    collection: MAILBOX.name,
                    pks: candidates,
                }],
            });
        }
    };
    let mut listed: BTreeSet<String> = received
        .keys()
        .filter(|pk| addresses.rows.contains_key(*pk))
        .cloned()
        .collect();
    let mut changes = Vec::new();
    let mut born_pk: Option<String> = None;
    if let Some(addr) = input
        .listing
        .target
        .as_deref()
        .and_then(|target| keys::address(target).ok())
        && let Ok(pk) = keys::address_pk(&addr)
        && received.contains_key(&pk)
        && !addresses.rows.contains_key(&pk)
    {
        changes.extend(birth(&pk, &addr)?);
        listed.insert(pk.clone());
        born_pk = Some(pk);
    }
    let born = usize::from(born_pk.is_some());
    let complete = input.listing.complete && listed.len() <= MAX_LISTED;
    let listed: Vec<String> = if listed.len() <= MAX_LISTED {
        listed.into_iter().collect()
    } else if let Some(keep) = born_pk {
        let mut kept: Vec<String> = listed
            .into_iter()
            .filter(|pk| pk != &keep)
            .take(MAX_LISTED - 1)
            .collect();
        kept.push(keep);
        kept.sort();
        kept
    } else {
        listed.into_iter().take(MAX_LISTED).collect()
    };
    let key = format!("{mailbox}.{}", input.uuid);
    let mine = obs.rows.get(&key).map(|row| &row.value);
    let today = day_of(input.now_ms);
    match mine {
        None if listed.is_empty() => return Ok(Plan::quiet(Outcome::Empty)),
        None => {}
        Some(row) => {
            let says_str = |column: &str| row.get(column).and_then(Value::as_str);
            let same = says_str("mailbox") == Some(mailbox.as_str())
                && says_str("method") == Some(PROTON_API)
                && row.get("complete").and_then(Value::as_bool) == Some(complete)
                && row
                    .get("addresses")
                    .and_then(Value::as_array)
                    .is_some_and(|arr| {
                        arr.len() == listed.len()
                            && arr
                                .iter()
                                .zip(&listed)
                                .all(|(v, pk)| v.as_str() == Some(pk.as_str()))
                    });
            if same {
                return Ok(Plan::quiet(Outcome::Unchanged));
            }
            let at = row.get("at").and_then(Value::as_u64).unwrap_or(0);
            if born == 0 && day_of(at).saturating_add(REWRITE_DAYS) > today {
                return Ok(Plan::quiet(Outcome::Rested { mailbox }));
            }
        }
    }
    let n = mine
        .and_then(|row| row.get("n"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    changes.push(observation(
        &key,
        n,
        &mailbox,
        complete,
        &listed,
        input.now_ms,
    ));
    Ok(Plan {
        outcome: Outcome::Observed {
            mailbox,
            listed: listed.len(),
            born,
        },
        changes,
        findings: Vec::new(),
    })
}

#[cfg(all(test, unix))]
mod tests;
