// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An account's names (docs/DESIGN-shared-files-2026-11-11.md §3, §10), derived in one place
//! from the registry's account and address rows: its slug, unique across the lab
//! ([`keys::account_slugs`]); its name, its provider word and its address (`claude
//! ada.fb@alpha.example`), which is how every surface shows it; and the addresses it signs in
//! with. The shared files' computed columns (`file::materialize`), LabView and the `aterm db`
//! commands read them from here, and every command that takes an account resolves the word it
//! was given among them through `aterm_file::resolve`: the slug, the `ac-` key, the provider
//! word and the address (`oai ada@x.example` or `oai:ada@x.example`), or an address that names
//! one account. A roster label and a legacy label name none.

use super::registry::{ACCOUNT, ADDRESS};
use crate::typed::{
    Ty, View,
    cell::{self, Cells, Projection},
    grid, keys,
};
use astream_wire::sync::Digest;
use aterm_json::Value;
use std::collections::BTreeMap;

/// One account's names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    /// Its slug (`claude-fb-alpha`), unique across the lab.
    pub slug: String,
    /// Its provider word and claims address (`claude ada.fb@alpha.example`), where its address
    /// rows resolve.
    pub name: Option<String>,
    /// Its claims address: its identity's when one is set, else its login's.
    pub email: Option<String>,
    /// Its login address, which a sign-in types.
    pub login: Option<String>,
    pub live: bool,
}

impl Names {
    /// The account as one message shows it: its name and its slug (`claude ada.fb@alpha.example
    /// (claude-fb-alpha)`), or the slug alone where its address does not resolve.
    #[must_use]
    pub fn shown(&self) -> String {
        match &self.name {
            Some(name) => format!("{name} ({})", self.slug),
            None => self.slug.clone(),
        }
    }
}

/// The address the `address` row `pk` says now, in the spelling the grid shows it; `None` where
/// the row is missing, conflicted to nothing, or says something this build refuses to show.
fn address_of(addresses: &Projection, pk: &str) -> Option<String> {
    let x = addresses.rows.get(pk)?.get("addr")?;
    let shown = grid::show(&Ty::Email, x);
    keys::address(&shown)
        .ok()
        .filter(|address| *address == shown)
}

fn text<'a>(row: &'a cell::Row, column: &str) -> Option<&'a str> {
    row.get(column).and_then(Value::as_str)
}

/// The handle an account's owner named it by: the `label` the row holds, with the operation that
/// wrote it; none where two labels are in conflict. Only an owner's verb writes a label a read
/// admits: the column's writer is `Verb`, and the authority rules hold an imported or automatic
/// label inert (`typed::authority`), so the default `<cli>-<6 hex>` label an earlier importer
/// or observer stored never reaches these cells.
fn owner_handle(row: &cell::Row) -> Option<(&str, Digest)> {
    let field = row.fields.get("label")?;
    let mut owners = field.candidates().iter();
    let owner = owners.next()?;
    if owners.next().is_some() {
        return None;
    }
    Some((owner.value.x.as_str()?, owner.operation))
}

/// Every account's names in `view`, by key.
#[must_use]
pub fn of(view: &View) -> BTreeMap<String, Names> {
    of_cells(&view.cells)
}

/// Every account's names in `cells`, by key. Two owners' labels that make one slug go to the
/// write with the lowest operation digest ([`keys::account_slugs`]), which a table roll keeps: a
/// carried label projects at the operation that wrote it.
pub(crate) fn of_cells(cells: &Cells) -> BTreeMap<String, Names> {
    of_projected(
        &cell::project(&ACCOUNT, cells),
        &cell::project(&ADDRESS, cells),
    )
}

/// Every account's names, as [`of_cells`] reads them, from the account and address rows already
/// projected (`accounts`, `addresses`): a reader that projects them for its own use as well (the
/// shared-file materializer's `typed::view::Projections`) projects each once.
pub(crate) fn of_projected(
    accounts: &Projection,
    addresses: &Projection,
) -> BTreeMap<String, Names> {
    let (mut email, mut login) = addresses_of(accounts, addresses);
    let mut slugs = keys::account_slugs(&slugged(accounts, &email));
    accounts
        .rows
        .iter()
        .map(|(pk, row)| {
            let email = email.remove(pk.as_str());
            let names = Names {
                slug: slugs.remove(pk).unwrap_or_default(),
                name: text(row, "realm")
                    .zip(email.as_deref())
                    .and_then(|(realm, email)| keys::account_name(realm, email)),
                email,
                login: login.remove(pk.as_str()),
                live: row.live(),
            };
            (pk.clone(), names)
        })
        .collect()
}

/// What the slugs of `cells`' accounts settled that `aterm db alab check` reports
/// ([`keys::slug_findings`]): a short handle live accounts share, an owner's label that names its
/// account nothing, and one that repeats its provider word.
pub(crate) fn findings(cells: &Cells) -> keys::SlugFindings {
    let accounts = cell::project(&ACCOUNT, cells);
    let addresses = cell::project(&ADDRESS, cells);
    let (email, _) = addresses_of(&accounts, &addresses);
    let rows = slugged(&accounts, &email);
    keys::slug_findings(&rows, &keys::account_slugs(&rows))
}

/// Each account's claims address and login address, by key, where its address rows resolve.
fn addresses_of<'a>(
    accounts: &'a Projection,
    addresses: &Projection,
) -> (BTreeMap<&'a str, String>, BTreeMap<&'a str, String>) {
    let mut email = BTreeMap::new();
    let mut login = BTreeMap::new();
    for (pk, row) in &accounts.rows {
        if let Some(address) = text(row, "login").and_then(|ad| address_of(addresses, ad)) {
            login.insert(pk.as_str(), address);
        }
        let claims = text(row, "identity").or_else(|| text(row, "login"));
        if let Some(address) = claims.and_then(|ad| address_of(addresses, ad)) {
            email.insert(pk.as_str(), address);
        }
    }
    (email, login)
}

/// Each account as [`keys::account_slugs`] reads it, with its claims address from `email`.
fn slugged<'a>(
    accounts: &'a Projection,
    email: &'a BTreeMap<&str, String>,
) -> Vec<keys::Slugged<'a>> {
    accounts
        .rows
        .iter()
        .map(|(pk, row)| keys::Slugged {
            pk,
            realm: text(row, "realm").unwrap_or_default(),
            handle: owner_handle(row),
            claims_email: email.get(pk.as_str()).map(String::as_str),
            live: row.live(),
        })
        .collect()
}

/// What a word names among `names` ([`named`]), by key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found<'a> {
    One(&'a str),
    None,
    /// Several, by key.
    Many(Vec<&'a str>),
}

/// The accounts `word` names among `names` (`aterm_file::resolve`): its slug, its key, its
/// provider word and address in either spelling, or an address, live accounts tried first.
#[must_use]
pub fn named<'a>(names: &'a BTreeMap<String, Names>, word: &str) -> Found<'a> {
    let keys: Vec<&String> = names.keys().collect();
    let rows: Vec<aterm_file::Named<'_>> = names
        .iter()
        .map(|(pk, names)| aterm_file::Named {
            pk,
            live: names.live,
            handles: vec![
                names.slug.as_str(),
                names.name.as_deref().unwrap_or_default(),
                names.email.as_deref().unwrap_or_default(),
            ],
        })
        .collect();
    match aterm_file::resolve(word, &rows) {
        aterm_file::Resolved::One(place) => Found::One(keys[place]),
        aterm_file::Resolved::None => Found::None,
        aterm_file::Resolved::Many(places) => Found::Many(
            places
                .into_iter()
                .map(|place| keys[place].as_str())
                .collect(),
        ),
    }
}

/// The key of the one account `word` names among `names`, or why it names none: a word that
/// names no account lists none (`aterm_file::no_account`), and one that names several lists
/// each by its slug, its name and its key (`aterm_file::many_accounts`).
///
/// # Errors
/// `word` names no account, or several.
pub fn one<'a>(names: &'a BTreeMap<String, Names>, word: &str) -> Result<&'a str, String> {
    match named(names, word) {
        Found::One(pk) => Ok(pk),
        Found::None => Err(aterm_file::no_account(word)),
        Found::Many(pks) => Err(aterm_file::many_accounts(
            word,
            pks.into_iter().map(|pk| {
                let names = &names[pk];
                (
                    names.slug.as_str(),
                    names.name.as_deref().unwrap_or_default(),
                    pk,
                )
            }),
        )),
    }
}

#[cfg(test)]
#[path = "account_names_tests.rs"]
mod tests;
