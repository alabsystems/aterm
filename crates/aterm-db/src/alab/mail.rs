// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where an address's codes arrive (docs/DESIGN-alab-lab-model-2026-11-01.md §0, §6): the owner's
//! mailbox if set; else, for an alias, wherever the address it is linked with receives; else the
//! one mailbox whose latest complete `mailbox-obs` lists it; else, for a `+tag` address, the one
//! mailbox whose latest complete observation lists its live base address (a mailbox delivers a
//! `+tag` to its base, and the list is the mailbox saying it receives the base: the link the list
//! supplies); else, on a Mac that holds the admins-audience `ai-mailbox.toml`, its route; else the
//! lead's guess. Pure, over the registry's
//! projection, so every Mac reads one answer from the same merged rows.
//!
//! What a sign-in reads of a mailbox (one-mesh design §4.7, §4.8, step A9): [`mailboxes`] names
//! each live mailbox's login (its `login` row's address), its webmail, its `receives` patterns and
//! which of its credentials are set, never a value; [`reveal`] hands one credential to the
//! harness's sign-in through [`super::secret::reveal_sealed`], journalled as `SignIn`. The login
//! password is the login address's own `password` cell, so the emails table and the mailbox
//! never hold one fact twice.

use super::{
    Registry,
    registry::{ADDRESS, MAILBOX},
    secret::{Journal, Surface},
};
use crate::{
    Result,
    typed::{
        cell::{Projection, Row},
        secret::{Cell, Sealer},
    },
};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// How an address's mailbox was decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// `address.mailbox`.
    Owner,
    /// One complete observation lists it.
    Observed,
    /// `ai-mailbox.toml`'s routes, on a Mac that holds them.
    Routed,
    /// `address.mailbox_hint`.
    Hint,
}

/// An address's effective mailbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    /// The mailbox's key.
    pub mailbox: String,
    pub how: How,
    /// The address rows followed through `alias_of` to reach it, nearest first.
    pub via: Vec<String>,
}

/// The mailboxes whose latest complete observation lists each address: `address pk → mailbox
/// pks`. More than one for an address decides nothing.
pub type Observed = BTreeMap<String, BTreeSet<String>>;

/// The observations in `mailbox-obs` rows (each `{mailbox, complete, addresses, at}` under key
/// `<mailbox>.<uuid32>`): per mailbox the complete row whose key prefix matches `mailbox` with the
/// latest claimed time, its smallest key breaking a tie, so every Mac picks one. An incomplete row
/// or one whose key names another mailbox says nothing.
pub fn observed<'a>(rows: impl IntoIterator<Item = (&'a str, &'a Value)>) -> Observed {
    let mut latest: BTreeMap<&str, (u64, &str, &Value)> = BTreeMap::new();
    for (key, row) in rows {
        let Some(mailbox) = row
            .get("mailbox")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if key
            .rsplit_once('.')
            .is_none_or(|(prefix, _)| prefix != mailbox)
        {
            continue;
        }
        if row.get("complete").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Some(at) = row.get("at").and_then(Value::as_u64) else {
            continue;
        };
        let newer = latest.get(mailbox).is_none_or(|(held_at, held_key, _)| {
            (at, std::cmp::Reverse(key)) > (*held_at, std::cmp::Reverse(*held_key))
        });
        if newer {
            latest.insert(mailbox, (at, key, row));
        }
    }
    let mut out = Observed::new();
    for (mailbox, (_, _, row)) in latest {
        for address in row
            .get("addresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            out.entry(address.to_owned())
                .or_default()
                .insert(mailbox.to_owned());
        }
    }
    out
}

fn row_column<'a>(row: &'a Row, name: &str) -> Option<&'a str> {
    row.get(name)?.as_str().filter(|s| !s.is_empty())
}

fn column<'a>(addresses: &'a Projection, pk: &str, name: &str) -> Option<&'a str> {
    row_column(addresses.rows.get(pk)?, name)
}

/// Whether `row` is live and not marked `protected`.
fn unshielded(row: &crate::typed::cell::Row) -> bool {
    row.live() && row.get("protected").and_then(Value::as_bool) != Some(true)
}

/// The next live, unshielded address `pk` is linked with.
fn linked<'a>(addresses: &'a Projection, pk: &str) -> Option<&'a str> {
    let target = column(addresses, pk, "alias_of")?;
    addresses.rows.get(target).filter(|row| unshielded(row))?;
    Some(target)
}

/// The address `address` is a `+tag` of, by the same rule `mailbox_routes.rs` reads a route.
pub(crate) fn untagged(address: &str) -> Option<String> {
    let (local, domain) = address.split_once('@')?;
    let (base, _) = local.split_once('+')?;
    (!base.is_empty()).then(|| format!("{base}@{domain}"))
}

/// The live base address of `pk`'s `+tag` and the one mailbox whose latest complete observation
/// lists that base: the alias link an address list supplies, `(base pk, mailbox pk)`. Only the
/// observation that supplies the link decides it, so a base two mailboxes list, or one only a
/// route or a guess places, links nothing and the tag keeps its own routes. Derived on every read
/// and never stored (§3.11), so the owner's and the importer's `alias_of` stay theirs alone.
pub fn listed_base(
    addresses: &Projection,
    observed: &Observed,
    pk: &str,
) -> Option<(String, String)> {
    let base = untagged(column(addresses, pk, "addr")?)?;
    let base = crate::typed::keys::address_pk(&base).ok()?;
    addresses.rows.get(&base).filter(|row| unshielded(row))?;
    let set = observed.get(&base)?;
    if set.len() != 1 {
        return None;
    }
    let mailbox = set.first()?.clone();
    Some((base, mailbox))
}

/// Whether following `pk`'s alias links returns to an address already followed.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Follow",
        project = "aterm_db::alab::view::tests::follow_transition"
    )
)]
pub fn alias_cycle(addresses: &Projection, pk: &str) -> bool {
    let mut seen = BTreeSet::from([pk]);
    let mut at = pk;
    while let Some(next) = linked(addresses, at) {
        if !seen.insert(next) {
            return true;
        }
        at = next;
    }
    false
}

/// The effective mailbox of address `pk`. `routes` answers for an address on a Mac that holds
/// `ai-mailbox.toml`, and nothing elsewhere. A link to a retired or absent address, or a cycle,
/// is left and read past: `check` reports it.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Follow",
        project = "aterm_db::alab::view::tests::follow_transition"
    )
)]
pub fn effective(
    addresses: &Projection,
    observed: &Observed,
    routes: &dyn Fn(&str) -> Option<String>,
    pk: &str,
) -> Option<Effective> {
    follow(addresses, observed, routes, pk, &mut BTreeSet::new())
}

fn follow<'a>(
    addresses: &'a Projection,
    observed: &Observed,
    routes: &dyn Fn(&str) -> Option<String>,
    pk: &str,
    seen: &mut BTreeSet<&'a str>,
) -> Option<Effective> {
    let (key, row) = addresses
        .rows
        .get_key_value(pk)
        .filter(|(_, row)| unshielded(row))?;
    if !seen.insert(key.as_str()) {
        return None;
    }
    let found = |mailbox: &str, how| {
        Some(Effective {
            mailbox: mailbox.to_owned(),
            how,
            via: Vec::new(),
        })
    };
    if let Some(mailbox) = row_column(row, "mailbox") {
        return found(mailbox, How::Owner);
    }
    if let Some(target) = row_column(row, "alias_of")
        && let Some(mut reached) = follow(addresses, observed, routes, target, seen)
    {
        reached.via.insert(0, target.to_owned());
        return Some(reached);
    }
    if let Some(mailboxes) = observed.get(pk)
        && mailboxes.len() == 1
        && let Some(mailbox) = mailboxes.first()
    {
        return found(mailbox, How::Observed);
    }
    if let Some((base, mailbox)) = listed_base(addresses, observed, pk) {
        return Some(Effective {
            mailbox,
            how: How::Observed,
            via: vec![base],
        });
    }
    if let Some(mailbox) = routes(pk).filter(|s| !s.is_empty()) {
        return found(&mailbox, How::Routed);
    }
    row_column(row, "mailbox_hint").and_then(|mailbox| found(mailbox, How::Hint))
}

/// A credential a sign-in types for a mailbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Credential {
    /// The login's password: the login address row's `password`.
    Password,
    /// A two-password webmail's second: the mailbox row's `mailbox_password`.
    MailboxPassword,
    /// The login's RFC 6238 seed or `otpauth://` URI: the mailbox row's `totp`.
    Totp,
}

impl Credential {
    /// Every credential, in the order a sign-in types them.
    pub const ALL: [Self; 3] = [Self::Password, Self::MailboxPassword, Self::Totp];

    /// The column that holds it: on the login's address row for [`Self::Password`], else on the
    /// mailbox row.
    pub const fn column(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::MailboxPassword => "mailbox_password",
            Self::Totp => "totp",
        }
    }
}

/// A live mailbox as a sign-in reads it: names, patterns and which credentials are set, never a
/// credential's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mailbox {
    /// Its key (`mb-…`).
    pub pk: String,
    /// The owner's plain name for it (`proton-1`).
    pub label: String,
    /// Its login address, as the address row holds it (lower-case).
    pub login: String,
    /// The login address row's key.
    pub login_pk: String,
    pub service: Option<String>,
    pub code_path: Option<String>,
    /// Its webmail inbox URL.
    pub webmail: Option<String>,
    /// What it receives, as `ai-mailbox.toml`'s `receives` spelled it: each exact address
    /// (lower-case), then each `*@<domain>`, then `*`.
    pub receives: Vec<String>,
    /// The credentials whose cells hold a value, each in one candidate or several.
    pub held: Vec<Credential>,
}

fn token_candidates<'a>(row: &'a Row, column: &str) -> Vec<&'a str> {
    row.fields
        .get(column)
        .map(|field| field.candidates_x())
        .unwrap_or_default()
        .into_iter()
        .filter_map(Value::as_str)
        .filter(|token| !token.is_empty())
        .collect()
}

fn text_ref<'a>(row: &'a Row, column: &str) -> Option<&'a str> {
    row.fields
        .get(column)
        .filter(|field| !field.conflicted())
        .and_then(|field| field.value())
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn mailbox_of(addresses: &Projection, pk: &str, row: &Row) -> Option<Mailbox> {
    if !row.live() {
        return None;
    }
    let text = |row: &Row, column: &str| text_ref(row, column).map(str::to_owned);
    let login_pk = text_ref(row, "login")?;
    let login_row = addresses.rows.get(login_pk).filter(|r| r.live())?;
    let login = text(login_row, "addr")?;
    let mut receives: Vec<String> = row
        .fields
        .get("receives")
        .map(|field| field.members())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|address| addresses.rows.get(address))
        .filter(|address| address.live())
        .filter_map(|address| text_ref(address, "addr"))
        .map(str::to_ascii_lowercase)
        .collect();
    let mut any = false;
    for domain in row
        .fields
        .get("receives_domains")
        .filter(|field| !field.conflicted())
        .and_then(|field| field.value())
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if domain == "*" {
            any = true;
        } else {
            receives.push(format!("*@{}", domain.to_ascii_lowercase()));
        }
    }
    if any {
        receives.push("*".to_owned());
    }
    let held = Credential::ALL
        .into_iter()
        .filter(|which| {
            let holder = match which {
                Credential::Password => login_row,
                _ => row,
            };
            !token_candidates(holder, which.column()).is_empty()
        })
        .collect();
    Some(Mailbox {
        pk: pk.to_owned(),
        label: text(row, "label").unwrap_or_default(),
        login,
        login_pk: login_pk.to_owned(),
        service: text(row, "service"),
        code_path: text(row, "code_path"),
        webmail: text(row, "webmail"),
        receives,
        held,
    })
}

/// Every live mailbox whose `login` names a live address row, by key ([`Mailbox`]). A mailbox
/// with a conflicted or missing login, or one whose login row is retired, is no sign-in's.
pub fn mailboxes(addresses: &Projection, mailboxes: &Projection) -> Vec<Mailbox> {
    mailboxes
        .rows
        .iter()
        .filter_map(|(pk, row)| mailbox_of(addresses, pk, row))
        .collect()
}

/// Why a credential could not be handed to a sign-in: value-free words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Withheld {
    /// No live mailbox with a live login has this key.
    NoMailbox,
    /// The cell is empty.
    NotSet,
    /// The cell holds two different values; the owner picks one (§4.8, "Conflicts").
    TwoValues,
    /// A candidate does not open here.
    Unopened,
}

impl Withheld {
    /// The reason in words, naming no value.
    pub const fn words(self) -> &'static str {
        match self {
            Self::NoMailbox => "no live mailbox with a live login has that key",
            Self::NotSet => "not set in the registry",
            Self::TwoValues => "two values in the registry: the owner picks one",
            Self::Unopened => "a value in the registry does not open on this Mac",
        }
    }
}

/// One credential a sign-in asks for: mailbox `mailbox`'s `which`, revealed at `when_ms`.
#[derive(Clone, Copy, Debug)]
pub struct Ask<'a> {
    pub mailbox: &'a str,
    pub which: Credential,
    pub when_ms: u64,
}

/// Hand `ask`'s credential to `consume`, journalled once as a sign-in's reveal. Candidates that
/// open to one value are one value; two different values are withheld until the owner picks
/// one, so a sign-in never guesses between them.
///
/// # Errors
/// Journalling fails ([`super::secret::reveal_sealed`]).
pub fn reveal_in<T>(
    addresses: &Projection,
    mailboxes: &Projection,
    sealer: &dyn Sealer,
    journal: &Journal,
    ask: Ask<'_>,
    consume: impl FnOnce(&str) -> T,
) -> Result<std::result::Result<T, Withheld>> {
    let Some(mb_row) = mailboxes.rows.get(ask.mailbox).filter(|r| r.live()) else {
        return Ok(Err(Withheld::NoMailbox));
    };
    let Some(login_pk) = text_ref(mb_row, "login") else {
        return Ok(Err(Withheld::NoMailbox));
    };
    let Some(login_row) = addresses.rows.get(login_pk).filter(|r| r.live()) else {
        return Ok(Err(Withheld::NoMailbox));
    };
    if text_ref(login_row, "addr").is_none() {
        return Ok(Err(Withheld::NoMailbox));
    }
    let column = ask.which.column();
    let (collection, row_pk, row) = match ask.which {
        Credential::Password => (ADDRESS.name, login_pk, login_row),
        Credential::MailboxPassword | Credential::Totp => (MAILBOX.name, ask.mailbox, mb_row),
    };
    let tokens = token_candidates(row, column);
    let Some((first, rest)) = tokens.split_first() else {
        return Ok(Err(Withheld::NotSet));
    };
    let Ok(held) = sealer.open(collection, row_pk, column, first) else {
        return Ok(Err(Withheld::Unopened));
    };
    for token in rest {
        match sealer.open(collection, row_pk, column, token) {
            Ok(other) if other.same(&held) => {}
            Ok(_) => return Ok(Err(Withheld::TwoValues)),
            Err(_) => return Ok(Err(Withheld::Unopened)),
        }
    }
    let cell = Cell {
        table: sealer.table(),
        collection,
        row: row_pk,
        column,
    };
    super::secret::record_reveal(journal, cell, Surface::SignIn, ask.when_ms)?;
    Ok(Ok(consume(held.plaintext())))
}

/// The registry's live mailboxes on this Mac ([`mailboxes`]), read as an observer.
///
/// # Errors
/// The registry is not declared here or its log does not read.
pub fn sign_in_mailboxes(registry: &Registry) -> Result<Vec<Mailbox>> {
    let view = crate::att::Handle::at(registry.paths().clone(), &crate::att::REGISTRY).view()?;
    Ok(mailboxes(&view.project(&ADDRESS), &view.project(&MAILBOX)))
}

/// [`reveal_in`] over this Mac's registry, its Secret keys resolved inside the table session.
///
/// # Errors
/// The registry is not declared here, its log does not read, a key is unavailable, or the
/// journal fails.
pub fn reveal<T>(
    registry: &Registry,
    journal: &Journal,
    ask: Ask<'_>,
    consume: impl FnOnce(&str) -> T,
) -> Result<std::result::Result<T, Withheld>> {
    crate::att::Handle::at(registry.paths().clone(), &crate::att::REGISTRY).view_sealed(
        |view, sealer| {
            reveal_in(
                &view.project(&ADDRESS),
                &view.project(&MAILBOX),
                sealer,
                journal,
                ask,
                consume,
            )
        },
    )
}

/// Reveal a complete credential bundle from one sealed registry observation.
/// The admitted metadata is checked before any Secret cell opens; all requested parts
/// must succeed before the consumer receives any plaintext. Epoch keys stay in this module.
///
/// # Errors
/// The registry, key or journal is unavailable. Withholds incomplete or changed bindings.
pub fn reveal_bound<T>(
    registry: &Registry,
    journal: &Journal,
    mailbox: &str,
    parts: &[Credential],
    when_ms: u64,
    accept: impl FnOnce(&Mailbox) -> bool,
    consume: impl FnOnce(&[aterm_secrets::SecretBytes]) -> T,
) -> Result<std::result::Result<T, Withheld>> {
    crate::att::Handle::at(registry.paths().clone(), &crate::att::REGISTRY).view_sealed(
        |view, sealer| {
            let addresses = view.project(&ADDRESS);
            let mailboxes = view.project(&MAILBOX);
            let Some(found) = mailboxes
                .rows
                .get(mailbox)
                .and_then(|row| mailbox_of(&addresses, mailbox, row))
            else {
                return Ok(Err(Withheld::NoMailbox));
            };
            if !accept(&found) {
                return Ok(Err(Withheld::NoMailbox));
            }
            let mut values = Vec::with_capacity(parts.len());
            for part in parts {
                match reveal_in(
                    &addresses,
                    &mailboxes,
                    sealer,
                    journal,
                    Ask {
                        mailbox,
                        which: *part,
                        when_ms,
                    },
                    |value| aterm_secrets::SecretBytes::from(value.as_bytes()),
                )? {
                    Ok(value) => values.push(value),
                    Err(why) => return Ok(Err(why)),
                }
            }
            Ok(Ok(consume(&values)))
        },
    )
}

#[cfg(test)]
mod tests;
