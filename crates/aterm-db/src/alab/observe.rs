// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account discovery's registry mark (`docs/DESIGN-account-discovery-2026-11-09.md`, step 1):
//! what a Mac observed its tabs' CLIs really run on becomes this Mac's `account-obs` rows, and an
//! account no row names is born, so the registry learns from what every tab runs instead of
//! rotting until the owner imports again.
//!
//! An observation is matched to an account by address, never by a recomputed key: the `account`
//! row at the observed realm whose `login` or `identity` address row says the login email. Where
//! no live row does but a row that is not live holds the key a birth would take, the sighting is
//! that row's and nothing is born: a birth would revive what the owner retired. Where none does,
//! the account is born (and its address first, where no row says it), keyed as the importer keys
//! it (`keys::account_pk` of the realm and the claims email), so two Macs that observe one login
//! birth the same cells and converge; a concurrent owner's cell beats a birth cell. Where several
//! rows say it, nothing is written and the pass reports `account-ambiguous`.
//!
//! A record is written only for news the registry keeps for good: an account born, or this
//! Mac's first row of an account. A row that exists is rewritten only with such a record, where
//! what it says changed (another account or state, or its tab count crossing none, one, two or
//! more) or its last write is [`REWRITE_DAYS`] UTC days old, and never by a record of its own: a
//! tab opening or closing flips state and count many times a day, a Mac's quota of automatic
//! records is 24 for the table's life, and the journal's `seen` events, which its ring is built
//! to carry, say what runs now (a row's `running` counts for two hours after its write, no
//! longer). The CLI and the source a row was last read through are no change: two tabs may read
//! one account two ways, and they follow at the next rewrite.
//! Observation spends at most [`OBSERVE_RESERVE`] records of this Mac's quota, and none of
//! registration's reserve.
//!
//! What a Mac observes is three kinds of sighting: a CLI's login place (`idle`), a tab running
//! one (`running`), and an account signed in to one of aterm's own browser profiles (`idle` from
//! [`BROWSER`]); [`how`] says which in a person's words. Of one account's sightings a browser's
//! speaks last. A sighting never writes a `preference` row: which Mac an account is assigned to is
//! the owner's, and seeing it elsewhere changes nothing of that.
//!
//! [`plan`] is pure over the registry; `Registry::observe_accounts` runs it under the writer. A
//! pass that cannot run (a writer that is fenced, a registry not read) leaves its sightings in
//! this Mac's [`kept`] file, so they are registered once it authors again; so does a sighting
//! whose birth or first row the quota refused ([`Plan::waiting`]), for a pass with room.

use super::{
    checks::Finding,
    register::day_of,
    registry::{ACCOUNT, ACCOUNT_OBS, ADDRESS, AddressHolder, AddressIndex},
};
use crate::{
    Result,
    database::Change,
    typed::{
        LIFECYCLE,
        cell::{self, CellKey, CellValue, Cells},
        keys,
        owned::OwnedRead,
    },
};
use aterm_json::{Map, Value};
use std::collections::BTreeSet;

/// Days after which an unchanged `account-obs` row is rewritten beside the next record a pass
/// writes, as discovery's (`discover::REWRITE_DAYS`); it never writes a record of its own.
pub const REWRITE_DAYS: u64 = super::discover::REWRITE_DAYS;

/// The most records of this Mac's quota of automatic records that observation spends: its
/// passes write while this Mac has written fewer `account-obs` records than this and more than
/// registration's reserve remains (`discover::REGISTRATION_RESERVE`), so observation takes at
/// most half of what registration leaves and mailbox discovery's rule is the one it had.
pub const OBSERVE_RESERVE: usize = 6;

/// The CLI words an `account-obs` row takes.
pub const CLIS: &[&str] = &[
    "claude",
    "codex",
    "grok",
    "kimi",
    "agy",
    "muse",
    "codex-alab",
];
/// The states an `account-obs` row takes.
pub const STATES: &[&str] = &["running", "idle", "signed-out"];
/// The sources an `account-obs` row takes: the journal's `seen` words, and `browser`, an account
/// signed in to one of aterm's own browser profiles (`accounts/browsers`), which no CLI's file
/// names and the journal does not hear.
pub const SOURCES: &[&str] = &["profile", "auth-file", "cli-log", "vault-token", BROWSER];

/// The source of a sighting made in one of aterm's own browser profiles.
pub const BROWSER: &str = "browser";

/// How a Mac's row says it holds an account, in a person's words: used by a tab, signed in at a
/// CLI's login place, signed in only in one of aterm's own browser profiles, or signed out.
#[must_use]
pub fn how(state: &str, src: &str) -> &'static str {
    match (state, src) {
        ("running", _) => "used by a tab",
        ("signed-out", _) => "signed out",
        (_, BROWSER) => "browser only",
        _ => "signed in",
    }
}

/// Whether `s` was made in a browser profile: of the sightings of one account, every other kind
/// speaks first, since a CLI's login or a tab says more of it than a browser's session does.
pub(crate) fn browser_only(s: &Sighting) -> bool {
    s.src == BROWSER
}

/// One login this Mac observed. No `Debug`: it holds the login email.
#[derive(Clone, PartialEq, Eq)]
pub struct Sighting {
    /// The CLI word ([`CLIS`]).
    pub cli: String,
    /// The canonical claims realm (`claude`, `codex`, `kimi:global`, …).
    pub realm: String,
    /// The login email the CLI's file names.
    pub email: String,
    /// [`STATES`].
    pub state: String,
    /// How many of this Mac's tabs run it now: each tab sighted on it within the last two
    /// hourly refreshes and not since ended or moved to another login.
    pub tabs: u64,
    /// [`SOURCES`].
    pub src: String,
    /// When, Unix milliseconds.
    pub at_ms: u64,
}

/// What [`plan`] reads besides the registry.
#[derive(Clone, Copy)]
pub struct Input<'a> {
    /// This Mac's Commons uuid, 32 hex: the suffix of every row it writes.
    pub uuid: &'a str,
    pub sightings: &'a [Sighting],
    pub now_ms: u64,
    /// Whether this Mac's quota has room for one more record of observation
    /// ([`OBSERVE_RESERVE`]).
    pub room: bool,
}

/// What one pass decided.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub changes: Vec<Change>,
    /// By key, never by value.
    pub findings: Vec<Finding>,
    /// Accounts born.
    pub born: usize,
    /// `account-obs` rows written.
    pub marked: usize,
    /// Rows whose change waits for a record an account's birth or first row writes.
    pub rested: usize,
    /// The sightings, by their place in [`Input::sightings`], whose account's birth or this
    /// Mac's first row of it the quota refused: what the registry does not hold yet, which a
    /// pass after a roll gives its whole quota again is handed again ([`kept`]).
    pub waiting: Vec<usize>,
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

/// An observed address's birth: `addr`, `domain`, `origin` and `row`, every cell automatic, as
/// discovery's.
fn address_birth(pk: &str, addr: &str) -> Result<Vec<Change>> {
    let domain = addr.split_once('@').map_or("", |(_, domain)| domain);
    Ok(vec![
        automatic(&ADDRESS, pk, "addr", Value::from(addr))?,
        automatic(&ADDRESS, pk, "domain", Value::from(domain))?,
        automatic(&ADDRESS, pk, "origin", Value::from("discovered"))?,
        automatic(&ADDRESS, pk, LIFECYCLE, Value::from("live"))?,
    ])
}

/// The service a provider's observed CLI login runs, as the importer names it.
fn service(provider: &str) -> &'static str {
    match provider {
        keys::ANTHROPIC => "claude-code",
        keys::OPENAI => "codex",
        keys::GOOGLE => "agy",
        keys::META => "muse-code",
        keys::KIMI => "kimi-code",
        _ => "grok",
    }
}

/// An observed account's birth: its provider, realm, login address, `status=active`, its
/// default label, the service and `row=live`, every cell automatic. The label is left empty
/// (which reads as the default) where `taken`, the labels live accounts and this pass's births
/// hold, already holds it: `label` is unique among live accounts, and the rules every write
/// passes would refuse the birth, and with it every later pass that plans it again
/// (`typed::check::unique_clash`). A label it writes joins `taken`.
fn account_birth(
    pk: &str,
    realm: &str,
    login: &str,
    taken: &mut BTreeSet<String>,
) -> Result<Vec<Change>> {
    let provider = keys::provider_of_realm(realm)
        .ok_or_else(|| crate::Error::new("account-obs: a realm no provider runs"))?;
    let mut out = vec![
        automatic(&ACCOUNT, pk, "provider", Value::from(provider))?,
        automatic(&ACCOUNT, pk, "realm", Value::from(realm))?,
        automatic(&ACCOUNT, pk, "login", Value::from(login))?,
        automatic(&ACCOUNT, pk, "status", Value::from("active"))?,
        cell::change(
            &ACCOUNT,
            CellKey {
                pk,
                col: "services",
                elem: Some(service(provider)),
            },
            &CellValue::automatic(ACCOUNT.version, Value::Bool(true)),
        )?,
        automatic(&ACCOUNT, pk, LIFECYCLE, Value::from("live"))?,
    ];
    if let Ok(label) = keys::default_account_label(realm, pk)
        && taken.insert(label.clone())
    {
        out.push(automatic(&ACCOUNT, pk, "label", Value::from(label))?);
    }
    Ok(out)
}

/// Every label any live account's `label` cell holds, each candidate of a conflicted one too.
fn live_labels(accounts: &cell::Projection) -> BTreeSet<String> {
    accounts
        .rows
        .values()
        .filter(|row| row.live())
        .filter_map(|row| row.fields.get("label"))
        .flat_map(cell::Field::candidates)
        .filter_map(|candidate| candidate.value.x.as_str().map(str::to_owned))
        .collect()
}

/// The tab count as the row's change rule reads it: none, one, or two or more.
const fn tabs_bucket(tabs: u64) -> u64 {
    if tabs > 2 { 2 } else { tabs }
}

/// The account a sighting is of, by address.
enum Matched {
    /// One live account row at the realm says the email, or none does and a row that is not live
    /// holds the key a birth would take.
    Row(String),
    /// None does: born under this key, its login address `login` (born first where `addr` is
    /// some, not yet a row).
    Born {
        pk: String,
        login: String,
        addr: Option<String>,
    },
    /// Several rows say it, or the address's key is held by another row.
    Held(Finding),
}

fn matched(
    sighting: &Sighting,
    index: &AddressIndex,
    accounts: &cell::Projection,
) -> Result<Matched> {
    let realm = keys::canonical_realm(&sighting.realm)
        .ok_or_else(|| crate::Error::new("account-obs: not a realm"))?;
    let email = keys::address(&sighting.email)?;
    let saying = index.saying(&email);
    let says = |row: &cell::Row, col: &str| {
        row.get(col)
            .and_then(Value::as_str)
            .is_some_and(|pk| saying.contains(&pk))
    };
    let rows: Vec<&String> = accounts
        .rows
        .iter()
        .filter(|(_, row)| row.live())
        .filter(|(_, row)| {
            row.get("realm")
                .and_then(Value::as_str)
                .and_then(keys::canonical_realm)
                == Some(realm)
        })
        .filter(|(_, row)| says(row, "login") || says(row, "identity"))
        .map(|(pk, _)| pk)
        .collect();
    match rows.as_slice() {
        [one] => return Ok(Matched::Row((*one).clone())),
        [_, _, ..] => {
            return Ok(Matched::Held(Finding {
                kind: "account-ambiguous",
                collection: ACCOUNT.name,
                pks: rows.into_iter().cloned().collect(),
            }));
        }
        [] => {}
    }
    let pk = keys::account_pk(realm, &keys::claims_email(None, Some(&email))?)?;
    // A row at the account's own key that is not live (the owner retired, ended, merged or
    // locked it, or its lifecycle is conflicted) is this account: the sighting marks it and
    // births nothing. This Mac reads the row as not live only once it holds the owner's write,
    // so a birth would be appended after it and its automatic cells would replace the owner's
    // (`documents::apply` keeps no candidate a write observed): `row=live`, `status=active` and
    // the default label, the account back in use. A machine's birth never revives an ended row
    // either (`register::plan`).
    if accounts.rows.get(&pk).is_some_and(|row| !row.live()) {
        return Ok(Matched::Row(pk));
    }
    match index.holder(&email)? {
        AddressHolder::Row(login) => Ok(Matched::Born {
            pk,
            login: login.to_owned(),
            addr: None,
        }),
        AddressHolder::Free {
            derived,
            taken: false,
        } => Ok(Matched::Born {
            pk,
            login: derived,
            addr: Some(email),
        }),
        AddressHolder::Several(pks) => Ok(Matched::Held(Finding {
            kind: "account-ambiguous",
            collection: ADDRESS.name,
            pks: pks.into_iter().map(str::to_owned).collect(),
        })),
        AddressHolder::Free { derived, .. } => Ok(Matched::Held(Finding {
            kind: "account-address-taken",
            collection: ADDRESS.name,
            pks: vec![derived],
        })),
    }
}

/// What the registry holds of one sighting, as a pass would match it.
pub(crate) enum Holds {
    /// The account row it is of.
    Row(String),
    /// No row: a pass would give birth to it under this key.
    Unborn(String),
    /// It matches no one row (several say it, its address's key is taken, or it does not read).
    Unmatched,
}

/// [`Holds`] of `s` in the registry `index` and `accounts` describe.
pub(crate) fn holds(s: &Sighting, index: &AddressIndex, accounts: &cell::Projection) -> Holds {
    match matched(s, index, accounts) {
        Ok(Matched::Row(pk)) => Holds::Row(pk),
        Ok(Matched::Born { pk, .. }) => Holds::Unborn(pk),
        Ok(Matched::Held(_)) | Err(_) => Holds::Unmatched,
    }
}

/// One `account-obs` row's value.
fn obs_row(n: u64, account: &str, s: &Sighting, first_at: u64, at: u64) -> Value {
    let mut row = Map::new();
    row.insert("r".into(), Value::from(u64::from(ACCOUNT_OBS.version)));
    row.insert("n".into(), Value::from(n));
    row.insert("account".into(), Value::from(account));
    row.insert("cli".into(), Value::from(s.cli.as_str()));
    row.insert("state".into(), Value::from(s.state.as_str()));
    row.insert("tabs".into(), Value::from(s.tabs));
    row.insert("first_at".into(), Value::from(first_at));
    row.insert("at".into(), Value::from(at));
    row.insert("src".into(), Value::from(s.src.as_str()));
    Value::Object(row)
}

/// One pass over the registry this Mac holds: each sighting's account found by address or born,
/// and this Mac's first `account-obs` row of it written. A row that exists is rewritten, where
/// what it says changed or its rest period passed, only beside a birth or a first row: alone it
/// writes nothing and is counted in `rested`. A sighting older than this Mac's row of its
/// account says nothing newer of it, and leaves the account to its other sightings. A sighting
/// of a word this build does not know, or whose realm or address does not read, is skipped: one
/// such sighting kept on disk would otherwise refuse every later pass. A sighting whose birth or
/// first row the quota refuses is named in `waiting`.
///
/// # Errors
/// A change does not fit its column.
pub fn plan(input: &Input<'_>, cells: &Cells, obs: &OwnedRead) -> Result<Plan> {
    let addresses = cell::project(&ADDRESS, cells);
    let index = AddressIndex::of(&addresses);
    let accounts = cell::project(&ACCOUNT, cells);
    let mut labels = live_labels(&accounts);
    let today = day_of(input.now_ms);
    let mut out = Plan::default();
    let mut births: Vec<Change> = Vec::new();
    // First rows, which a record is written for, and rewrites, which only ride one.
    let mut marks: Vec<Change> = Vec::new();
    let mut riders: Vec<Change> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut quota_spent = false;
    // The account each sighting is of, and the accounts whose birth or first row a pass with
    // no room would refuse.
    let mut of: Vec<(usize, String)> = Vec::new();
    let mut unwritten: BTreeSet<String> = BTreeSet::new();
    // Each account's strongest sighting speaks for it: a browser's after every other.
    let mut ordered: Vec<(usize, &Sighting)> = input.sightings.iter().enumerate().collect();
    ordered.sort_by_key(|(_, s)| browser_only(s));
    for (at_input, s) in ordered {
        if !CLIS.contains(&s.cli.as_str())
            || !STATES.contains(&s.state.as_str())
            || !SOURCES.contains(&s.src.as_str())
        {
            continue;
        }
        let Ok(found) = matched(s, &index, &accounts) else {
            continue;
        };
        let account = match found {
            Matched::Held(finding) => {
                if !out.findings.contains(&finding) {
                    out.findings.push(finding);
                }
                continue;
            }
            Matched::Row(pk) => pk,
            Matched::Born { pk, login, addr } => {
                if seen.contains(&pk) {
                    pk
                } else if input.room {
                    let Ok(account) = account_birth(&pk, &s.realm, &login, &mut labels) else {
                        continue;
                    };
                    if let Some(addr) = &addr {
                        births.extend(address_birth(&login, addr)?);
                    }
                    births.extend(account);
                    out.born += 1;
                    pk
                } else {
                    quota_spent = true;
                    unwritten.insert(pk.clone());
                    pk
                }
            }
        };
        of.push((at_input, account.clone()));
        let key = format!("{account}.{}", input.uuid);
        let mine = obs.rows.get(&key).map(|row| &row.value);
        // A sighting older than the row (a kept one passed late) says nothing newer of it; a
        // newer sighting of the account through another login still may.
        if mine.is_some_and(|row| row.get("at").and_then(Value::as_u64).unwrap_or(0) > s.at_ms) {
            continue;
        }
        if !seen.insert(account.clone()) {
            continue;
        }
        let said = |row: &Value, col: &str| row.get(col).and_then(Value::as_str).map(str::to_owned);
        let Some(row) = mine else {
            unwritten.insert(account.clone());
            marks.push(Change {
                collection: ACCOUNT_OBS.name.into(),
                key,
                value: Some(obs_row(1, &account, s, s.at_ms, s.at_ms)),
            });
            continue;
        };
        let tabs = row.get("tabs").and_then(Value::as_u64).unwrap_or(0);
        // The CLI and the source are what the last write happened to read, not what the row says
        // of the account: two tabs on one account, one through its profile and one through a
        // vault token, would otherwise make the row a rider on every pass. They follow at the
        // next rewrite.
        let changed = said(row, "account").as_deref() != Some(account.as_str())
            || said(row, "state").as_deref() != Some(s.state.as_str())
            || tabs_bucket(tabs) != tabs_bucket(s.tabs);
        let at = row.get("at").and_then(Value::as_u64).unwrap_or(0);
        let rested = day_of(at).saturating_add(REWRITE_DAYS) <= today;
        if !changed && !rested {
            continue;
        }
        riders.push(Change {
            collection: ACCOUNT_OBS.name.into(),
            key,
            value: Some(obs_row(
                row.get("n").and_then(Value::as_u64).unwrap_or(0) + 1,
                &account,
                s,
                row.get("first_at").and_then(Value::as_u64).unwrap_or(at),
                s.at_ms,
            )),
        });
    }
    if quota_spent || (!input.room && !marks.is_empty()) {
        out.findings.push(Finding {
            kind: "quota-spent",
            collection: if quota_spent {
                ACCOUNT.name
            } else {
                ACCOUNT_OBS.name
            },
            pks: Vec::new(),
        });
    }
    if !input.room {
        out.waiting = of
            .iter()
            .filter(|(_, account)| unwritten.contains(account))
            .map(|(at_input, _)| *at_input)
            .collect();
        out.waiting.sort_unstable();
        out.waiting.dedup();
    }
    if !input.room || (births.is_empty() && marks.is_empty()) {
        out.born = 0;
        out.rested = riders.len();
        return Ok(out);
    }
    out.marked = marks.len() + riders.len();
    out.changes = births;
    out.changes.extend(marks);
    out.changes.extend(riders);
    Ok(out)
}

#[cfg(unix)]
pub mod kept;

#[cfg(all(test, unix))]
mod tests;
