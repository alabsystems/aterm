// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An account's names (docs/DESIGN-shared-files-2026-11-11.md §3, §10), derived in one place
//! from the registry's account and address rows: its slug, unique across the lab
//! ([`keys::account_slugs`]); its name, its provider word and its address (`claude
//! ada.fb@alpha.example`), which is how every surface shows it; and the addresses it signs in
//! with. The shared files' computed columns (`file::materialize`), LabView and the `aterm db`
//! commands read them from here as [`AccountNames`], and every command that takes an account
//! resolves the word it was given among them through aterm-file's account layer
//! (`aterm_file::account::one`): the slug, the `ac-` key, the provider word and the address
//! (`oai ada@x.example` or `oai:ada@x.example`), or an address that names one account. A roster
//! label and a legacy label name none.

use super::registry::{ACCOUNT, ADDRESS};
use crate::typed::{
    Ty, View,
    cell::{self, Cells, Projection},
    grid, keys,
};
use astream_wire::sync::Digest;
use aterm_file::AccountNames;
use aterm_json::Value;
use std::collections::BTreeMap;

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

/// The canonical realm of an account whose realm cell says `raw` ([`keys::canonical_realm`], so
/// an older build's `openai` or `agy` reads as `codex` or `google`); empty where the cell is
/// missing, conflicted, empty or no realm's, since the claims ring's empty default (`claude`)
/// says nothing of an account row.
pub(crate) fn realm(raw: Option<&str>) -> String {
    raw.filter(|raw| !raw.trim_ascii().is_empty())
        .and_then(keys::canonical_realm)
        .unwrap_or_default()
        .to_owned()
}

/// Every account's names in `view`, by key.
#[must_use]
pub fn of(view: &View) -> BTreeMap<String, AccountNames> {
    of_cells(&view.cells)
}

/// Every account's names in `cells`, by key. Two owners' labels that make one slug go to the
/// write with the lowest operation digest ([`keys::account_slugs`]), which a table roll keeps: a
/// carried label projects at the operation that wrote it.
pub(crate) fn of_cells(cells: &Cells) -> BTreeMap<String, AccountNames> {
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
) -> BTreeMap<String, AccountNames> {
    let (mut email, mut login) = addresses_of(accounts, addresses);
    let mut slugs = keys::account_slugs(&slugged(accounts, &email));
    accounts
        .rows
        .iter()
        .map(|(pk, row)| {
            let email = email.remove(pk.as_str());
            let realm = realm(text(row, "realm"));
            let names = AccountNames {
                name: Some(realm.as_str())
                    .filter(|realm| !realm.is_empty())
                    .zip(email.as_deref())
                    .and_then(|(realm, email)| keys::account_name(realm, email)),
                realm,
                slug: slugs.remove(pk).unwrap_or_default(),
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

#[cfg(test)]
#[path = "account_names_tests.rs"]
mod tests;
