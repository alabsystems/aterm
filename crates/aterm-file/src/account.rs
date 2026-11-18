// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account layer over [`crate::resolve`] (docs/DESIGN-shared-files-2026-11-11.md §3, §10):
//! one account's names ([`AccountNames`]) and the account a word names among them ([`named`],
//! [`one`]). A word names an account by its slug, its `ac-` key, its provider word and address
//! (`oai ada@x.example` or `oai:ada@x.example`), or an address that names one account; a roster
//! label and a legacy label name none.
//!
//! aterm-db derives the names from the registry (`alab::account_names`) and the materializer
//! writes them into `ai/accounts`; a program with only that file reads them back with
//! [`AccountNames::of_row`]. Either way every command resolves a word here, so the harness, the
//! `aterm db` commands and a reader of the snapshot alone agree on what a word names. The module
//! is pure: it reads no file and holds no I/O, and it imports only the resolver
//! (`crate::resolver`, pure too) and a snapshot's row, so the two move out of this crate
//! together.

use crate::resolver::{Named, Resolved, many_accounts, no_account, resolve};
use crate::snapshot::Row;
use aterm_json::Value;
use std::collections::BTreeMap;

/// The most bytes of a slug (aterm-db's `typed::keys::MAX_SLUG`, which holds the reasons).
pub const MAX_SLUG: usize = 39;

/// Whether `text` is a slug: runs of `[a-z0-9]` joined by single `-`, at most [`MAX_SLUG`]
/// bytes. Every slug the lab derives is one (aterm-db's `typed::keys::account_slugs`); an
/// address, a space or a capital never is.
#[must_use]
pub fn is_slug(text: &str) -> bool {
    text.len() <= MAX_SLUG
        && text.split('-').all(|run| {
            !run.is_empty()
                && run
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// One account's names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountNames {
    /// Its provider realm (`claude`, `codex`, `google`, `kimi:global`, …): which pool it
    /// belongs to. The registry's names (aterm-db's `alab::account_names`) and LabView's give
    /// it canonical; empty where no one realm is known (a missing or conflicted cell).
    pub realm: String,
    /// Its slug (`claude-fb-alpha`), unique across the lab; empty where no lab named it.
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

impl AccountNames {
    /// The names a row of `ai/accounts` holds: its `realm` as the row holds it, and the pass's
    /// `=slug`, `=name`, `=email` and `=login_email`. An empty cell is no name, and so is a
    /// `=slug` cell that is no slug ([`is_slug`]: an address an older or edited file holds there
    /// never becomes the word a surface prints or a step types).
    #[must_use]
    pub fn of_row(row: &Row) -> Self {
        let shown = |column: &str| Some(row.cell(column).to_owned()).filter(|c| !c.is_empty());
        let realm = row
            .values
            .get("realm")
            .and_then(Value::as_str)
            .unwrap_or_else(|| row.cell("realm"));
        Self {
            realm: realm.to_owned(),
            slug: Some(row.cell("slug"))
                .filter(|slug| is_slug(slug))
                .unwrap_or_default()
                .to_owned(),
            name: shown("name"),
            email: shown("email"),
            login: shown("login_email"),
            live: row.live,
        }
    }

    /// The account as one message shows it: its name and its slug (`claude ada.fb@alpha.example
    /// (claude-fb-alpha)`), the slug alone where its address does not resolve, and the name
    /// alone where no lab gave it a slug.
    #[must_use]
    pub fn shown(&self) -> String {
        match (&self.name, self.slug.is_empty()) {
            (Some(name), false) => format!("{name} ({})", self.slug),
            (Some(name), true) => name.clone(),
            (None, _) => self.slug.clone(),
        }
    }

    /// The account in the fewest words: its slug, else its name; empty where it has neither.
    #[must_use]
    pub fn narrow(&self) -> &str {
        if self.slug.is_empty() {
            self.name.as_deref().unwrap_or_default()
        } else {
            &self.slug
        }
    }
}

/// What a word names among accounts' names ([`named`]), by key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found<'a> {
    One(&'a str),
    None,
    /// Several, by key.
    Many(Vec<&'a str>),
}

/// The accounts `word` names among `names` ([`resolve`]): its slug, its key, its provider word
/// and address in either spelling, or an address, live accounts tried first.
#[must_use]
pub fn named<'a>(names: &'a BTreeMap<String, AccountNames>, word: &str) -> Found<'a> {
    let keys: Vec<&String> = names.keys().collect();
    let rows: Vec<Named<'_>> = names
        .iter()
        .map(|(pk, names)| Named {
            pk,
            live: names.live,
            handles: vec![
                names.slug.as_str(),
                names.name.as_deref().unwrap_or_default(),
                names.email.as_deref().unwrap_or_default(),
            ],
        })
        .collect();
    match resolve(word, &rows) {
        Resolved::One(place) => Found::One(keys[place]),
        Resolved::None => Found::None,
        Resolved::Many(places) => Found::Many(
            places
                .into_iter()
                .map(|place| keys[place].as_str())
                .collect(),
        ),
    }
}

/// The key of the one account `word` names among `names`, or why it names none: a word that
/// names no account lists none ([`no_account`]), and one that names several lists each by its
/// slug, its name and its key ([`many_accounts`]).
///
/// # Errors
/// `word` names no account, or several.
pub fn one<'a>(names: &'a BTreeMap<String, AccountNames>, word: &str) -> Result<&'a str, String> {
    match named(names, word) {
        Found::One(pk) => Ok(pk),
        Found::None => Err(no_account(word)),
        Found::Many(pks) => Err(many_accounts(
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
#[path = "account_tests.rs"]
mod tests;
