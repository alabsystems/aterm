// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one resolver every row and account name passes (docs/DESIGN-shared-files-2026-11-11.md
//! §3, §10): what a word names among rows' keys and handle cells ([`resolve`]), and the words an
//! account command answers a word that names no account, or several, with ([`no_account`],
//! [`many_accounts`]). Pure: it reads no file and holds no I/O, so the account layer over it
//! (`crate::account`) depends on nothing else here but a snapshot's row, and the two can move
//! out of this crate together.

/// The handle columns of a file of accounts (`ai/accounts`), in the order a name tries them:
/// the slug, the name (`claude ada.fb@alpha.example`) and the address.
pub const ACCOUNT_HANDLES: [&str; 3] = ["slug", "name", "email"];

/// One row as [`resolve`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named<'a> {
    /// Its key.
    pub pk: &'a str,
    /// Its lifecycle says live, or says nothing.
    pub live: bool,
    /// Its handle cells, in the order a name tries them; an empty cell names nothing.
    pub handles: Vec<&'a str>,
}

/// What a word names among the rows [`resolve`] is given, by their places there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    One(usize),
    None,
    /// Several, in the order given.
    Many(Vec<usize>),
}

impl Resolved {
    /// The places it names: none, one, or each of several.
    #[must_use]
    pub fn places(self) -> Vec<usize> {
        match self {
            Self::One(place) => vec![place],
            Self::None => Vec::new(),
            Self::Many(places) => places,
        }
    }
}

/// The rows `word` names (docs/DESIGN-shared-files-2026-11-11.md §3, §10): the one resolver
/// every read of a shared file and every account command passes. Exact only: no prefix, no
/// substring, no folded case and no nearest match. The first of these that names a row answers:
///
/// 1. the row whose key is `word`, live or not;
/// 2. among live rows, then among the others: each handle in turn, every row whose cell is
///    `word`; then the same with the first `:` of `word` read as a space, so
///    `oai:ada.fb@alpha.example` names what `oai ada.fb@alpha.example` does.
///
/// For accounts that is the slug, the `ac-` key, the provider word and the address in either
/// spelling, and the address alone, which names every account that signs in as it: one
/// address signed in to three providers is three accounts, and [`Resolved::Many`].
#[must_use]
pub fn resolve(word: &str, rows: &[Named<'_>]) -> Resolved {
    if word.is_empty() {
        return Resolved::None;
    }
    if let Some(place) = rows.iter().position(|row| row.pk == word) {
        return Resolved::One(place);
    }
    let spaced = word
        .split_once(':')
        .map(|(provider, address)| format!("{provider} {address}"));
    let width = rows.iter().map(|row| row.handles.len()).max().unwrap_or(0);
    for live in [true, false] {
        for wanted in std::iter::once(word).chain(spaced.as_deref()) {
            for handle in 0..width {
                let found: Vec<usize> = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        row.live == live
                            && row
                                .handles
                                .get(handle)
                                .is_some_and(|cell| !cell.is_empty() && *cell == wanted)
                    })
                    .map(|(place, _)| place)
                    .collect();
                match found.as_slice() {
                    [] => {}
                    [place] => return Resolved::One(*place),
                    _ => return Resolved::Many(found),
                }
            }
        }
    }
    Resolved::None
}

/// What a word that names no account answers. It lists none: a typo never prints the lab's
/// addresses.
#[must_use]
pub fn no_account(word: &str) -> String {
    format!(
        "no account is named `{word}`: name one by its slug, its key, or its provider word and \
         address"
    )
}

/// What a word that names several accounts answers: one line for each, its slug, its name (its
/// provider word and address) and its key, leaving out the slug or the name it lacks. An account
/// no lab has slugged yet (one a Mac names itself) has no slug, so the refusal offers the key and
/// the provider word and address too, which tell Kimi Code's two regions of one address apart.
#[must_use]
pub fn many_accounts<'a>(
    word: &str,
    accounts: impl ExactSizeIterator<Item = (&'a str, &'a str, &'a str)>,
) -> String {
    let mut why = format!(
        "`{word}` names {} accounts; name one by its slug, its key, or its provider word and \
         address:",
        accounts.len()
    );
    for (slug, name, pk) in accounts {
        let parts: Vec<&str> = [slug, name, pk]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect();
        why.push_str("\n  ");
        why.push_str(&parts.join("  "));
    }
    why
}

#[cfg(test)]
#[path = "resolver_tests.rs"]
pub(crate) mod tests;
