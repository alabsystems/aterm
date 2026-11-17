// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Account names over hand-built rows; every address is synthetic (`example.invalid`).

use super::{Found, Names, named, of_cells, one};
use crate::{
    alab::registry::{ACCOUNT, ADDRESS},
    database::Change,
    typed::{
        cell::{self, CellKey, CellValue},
        grid::tests::{Book, put},
        keys,
    },
};
use aterm_json::json;
use std::collections::BTreeMap;

const ADA: &str = "ada.fb@alpha.example.invalid";
const BEA: &str = "bea.m2@beta.example.invalid";

/// The address row of `email`, live.
fn address(email: &str) -> (String, Vec<Change>) {
    let pk = keys::address_pk(email).expect("an address key");
    let changes = vec![
        put(&ADDRESS, &pk, "addr", json!(email)),
        put(&ADDRESS, &pk, "row", json!("live")),
    ];
    (pk, changes)
}

/// The live account of `realm` that signs in as `email`, born as `add account` births one.
fn account(realm: &str, email: &str) -> (String, Vec<Change>) {
    let (login, _) = address(email);
    let pk = keys::account_pk(realm, email).expect("an account key");
    let changes = vec![
        put(&ACCOUNT, &pk, "realm", json!(realm)),
        put(&ACCOUNT, &pk, "login", json!(login)),
        put(&ACCOUNT, &pk, "status", json!("pending")),
        put(&ACCOUNT, &pk, "row", json!("live")),
    ];
    (pk, changes)
}

fn label(pk: &str, value: &CellValue) -> Change {
    cell::change(
        &ACCOUNT,
        CellKey {
            pk,
            col: "label",
            elem: None,
        },
        value,
    )
    .expect("a label cell")
}

fn names(book: &Book) -> BTreeMap<String, Names> {
    of_cells(&book.cells())
}

/// The owner's label is the handle a slug is made of, a machine's word in it included; an
/// account with none takes the handle its address derives. Only the owner's verb writes a label
/// a read admits: an imported or automatic one is inert under the authority rules
/// (`typed::authority`, `alab::tests::one_address_is_three_accounts_slugged_claude_oai_and_google_and_an_automatic_owner_write_is_inert`).
/// The names beside the slug are the provider word and the claims address, and the login
/// address.
/// NEGATIVE CONTROL: with `owner_handle` answering `None`, the Google account takes its derived
/// handle: `` left: "google-fb-alpha" right: "google-m2-work" ``.
#[test]
fn an_owners_label_names_an_account_and_may_hold_a_machines_word() {
    let mut book = Book::new();
    let (_, mut changes) = address(ADA);
    let (claude, born) = account("claude", ADA);
    let (oai, born_oai) = account("codex", ADA);
    let (google, born_google) = account("google", ADA);
    changes.extend(born.into_iter().chain(born_oai).chain(born_google));
    changes.push(label(&google, &CellValue::owner(2, json!("m2-work"))));
    book.write(1, &[], changes);
    let names = names(&book);
    assert_eq!(names[&claude].slug, "claude-fb-alpha");
    assert_eq!(names[&oai].slug, "oai-fb-alpha");
    assert_eq!(names[&google].slug, "google-m2-work");
    assert_eq!(
        names[&claude],
        Names {
            slug: "claude-fb-alpha".to_owned(),
            name: Some(format!("claude {ADA}")),
            email: Some(ADA.to_owned()),
            login: Some(ADA.to_owned()),
            live: true,
        }
    );
    assert_eq!(names[&oai].name.as_deref(), Some(&*format!("oai {ADA}")));
    assert_eq!(
        names[&claude].shown(),
        format!("claude {ADA} (claude-fb-alpha)")
    );
}

/// Two owners' labels in conflict on one account are no handle: it takes its derived one until
/// the owner settles the cell. Where an identity is set, the name and the slug are the
/// identity's address and the login is the address a sign-in types.
/// NEGATIVE CONTROL: with `owner_handle` taking the first owner candidate of a conflict (its
/// `owners.next().is_some()` check removed), the account takes one of the two labels: `` left:
/// "claude-desk" right: "claude-m2-beta" ``.
#[test]
fn owners_labels_in_conflict_name_no_handle_and_an_identity_names_the_account() {
    let mut book = Book::new();
    let (_, mut changes) = address(BEA);
    let (claude, born) = account("claude", BEA);
    changes.extend(born);
    let birth = book.write(1, &[], changes);
    book.write(
        1,
        &[birth],
        vec![label(&claude, &CellValue::owner(1, json!("desk")))],
    );
    book.write(
        2,
        &[birth],
        vec![label(&claude, &CellValue::owner(1, json!("study")))],
    );
    assert_eq!(names(&book)[&claude].slug, "claude-m2-beta");

    // An alias login: the account is its identity's.
    let mut book = Book::new();
    let (_, mut changes) = address(ADA);
    let (identity, more) = address(BEA);
    changes.extend(more);
    let (claude, mut born) = account("claude", ADA);
    born.push(put(&ACCOUNT, &claude, "identity", json!(identity)));
    changes.extend(born);
    book.write(1, &[], changes);
    let names = names(&book);
    assert_eq!(names[&claude].slug, "claude-m2-beta");
    assert_eq!(
        names[&claude].name.as_deref(),
        Some(&*format!("claude {BEA}"))
    );
    assert_eq!(names[&claude].email.as_deref(), Some(BEA));
    assert_eq!(names[&claude].login.as_deref(), Some(ADA));
}

/// A word names an account by its slug, its key, its provider word and address in either
/// spelling, or an address that signs in to one account; an address three accounts share names
/// all three and lists them by slug, name and key; a prefix names none and lists none.
/// NEGATIVE CONTROL: with `named` giving the resolver no address handle (`email` left out of
/// `handles`), an address that names one account names none: `` left: Err("no account is named
/// `bea.m2@beta.example.invalid`: …") right: Ok("ac-…") ``.
#[test]
fn one_takes_every_name_of_an_account_and_lists_the_several_an_address_names() {
    let mut book = Book::new();
    let (_, mut changes) = address(ADA);
    let (_, more) = address(BEA);
    changes.extend(more);
    let mut keys_of = BTreeMap::new();
    for realm in ["claude", "codex", "google"] {
        let (pk, born) = account(realm, ADA);
        changes.extend(born);
        keys_of.insert(realm, pk);
    }
    let (bea, born) = account("claude", BEA);
    changes.extend(born);
    book.write(1, &[], changes);
    let names = names(&book);
    let oai = keys_of["codex"].as_str();
    for word in [
        "oai-fb-alpha".to_owned(),
        oai.to_owned(),
        format!("oai {ADA}"),
        format!("oai:{ADA}"),
    ] {
        assert_eq!(one(&names, &word), Ok(oai), "{word}");
    }
    assert_eq!(one(&names, BEA), Ok(bea.as_str()));
    let mut all: Vec<&str> = keys_of.values().map(String::as_str).collect();
    all.sort_unstable();
    assert_eq!(named(&names, ADA), Found::Many(all));
    let several = one(&names, ADA).expect_err("three accounts");
    assert!(
        several.starts_with(&format!("`{ADA}` names 3 accounts; name one by its slug:")),
        "{several}"
    );
    for realm in ["claude", "codex", "google"] {
        let names = &names[&keys_of[realm]];
        let line = format!(
            "  {}  {}  {}",
            names.slug,
            names.name.as_deref().unwrap_or_default(),
            keys_of[realm]
        );
        assert!(several.contains(&line), "{several}");
    }
    for word in [
        "claude-fb",
        "fb-alpha",
        "m2-beta",
        "claude",
        "fb@alpha.example.invalid",
    ] {
        let none = one(&names, word).expect_err("names none");
        assert_eq!(none, aterm_file::no_account(word));
        assert!(!none.contains(ADA) && !none.contains(BEA), "{none}");
    }
}
