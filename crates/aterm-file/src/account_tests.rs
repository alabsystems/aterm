// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account layer over hand-built names; every address is synthetic (`x.example`).

use super::{AccountNames, Found, MAX_SLUG, is_slug, named, one};
use crate::{Row, many_accounts, no_account};
use aterm_json::Value;
use std::collections::BTreeMap;

const ADA: &str = "ada@x.example";
const BEA: &str = "bea@y.example";
const CY: &str = "cy@z.example";

fn names(realm: &str, slug: &str, email: Option<&str>, live: bool) -> AccountNames {
    let word = match realm {
        "codex" => "oai",
        "kimi:global" => "kimi",
        "kimi:mainland-cn" => "kimicn",
        other => other,
    };
    AccountNames {
        realm: realm.to_owned(),
        slug: slug.to_owned(),
        name: email.map(|email| format!("{word} {email}")),
        email: email.map(str::to_owned),
        login: email.map(str::to_owned),
        live,
    }
}

/// One address signed in to Claude and OpenAI, a second address only Claude holds, and a
/// retired Google account that shares the first address.
fn lab() -> BTreeMap<String, AccountNames> {
    BTreeMap::from([
        (
            "ac-00000000000000a1".to_owned(),
            names("claude", "claude-ada-x", Some(ADA), true),
        ),
        (
            "ac-00000000000000a2".to_owned(),
            names("codex", "oai-ada-x", Some(ADA), true),
        ),
        (
            "ac-00000000000000b1".to_owned(),
            names("claude", "claude-bea-y", Some(BEA), true),
        ),
        (
            "ac-00000000000000a3".to_owned(),
            names("google", "google-ada-x", Some(ADA), false),
        ),
    ])
}

/// A row of `ai/accounts` gives its realm, the pass's slug, name and addresses, and its
/// lifecycle; an empty cell is no name, and a realm with no typed value reads its cell.
#[test]
fn a_row_of_the_accounts_file_gives_its_realm_and_every_name_the_pass_wrote() {
    let cells = [
        ("realm", "codex"),
        ("slug", "oai-ada-x"),
        ("name", "oai ada@x.example"),
        ("email", ADA),
        ("login_email", "ada.login@x.example"),
    ];
    let mut row = Row {
        pk: "ac-00000000000000a2".to_owned(),
        live: true,
        cells: cells
            .iter()
            .map(|(column, text)| ((*column).to_owned(), (*text).to_owned()))
            .collect(),
        values: [("realm".to_owned(), Value::from("codex"))]
            .into_iter()
            .collect(),
    };
    assert_eq!(
        AccountNames::of_row(&row),
        AccountNames {
            realm: "codex".to_owned(),
            slug: "oai-ada-x".to_owned(),
            name: Some("oai ada@x.example".to_owned()),
            email: Some(ADA.to_owned()),
            login: Some("ada.login@x.example".to_owned()),
            live: true,
        }
    );
    row.values.clear();
    row.cells.remove("name");
    row.cells.remove("login_email");
    row.live = false;
    let read = AccountNames::of_row(&row);
    assert_eq!(read.realm, "codex", "the realm's cell, with no typed value");
    assert_eq!((read.name, read.login, read.live), (None, None, false));
}

/// A `=slug` cell that is no slug (an address, a capital, a space) is no name, so an older or
/// edited file never puts an address where a surface prints the slug or a step types it.
/// NEGATIVE CONTROL: with `of_row` keeping the cell as it is (no `is_slug` filter), the address
/// is the slug: `` left: "ada@x.example" right: "" ``.
#[test]
fn a_slug_cell_that_is_no_slug_is_no_name() {
    let row = |slug: &str| Row {
        pk: "ac-00000000000000a1".to_owned(),
        live: true,
        cells: [("realm", "claude"), ("slug", slug), ("email", ADA)]
            .iter()
            .map(|(column, text)| ((*column).to_owned(), (*text).to_owned()))
            .collect(),
        values: aterm_json::Map::new(),
    };
    assert_eq!(
        AccountNames::of_row(&row("claude-ada-x")).slug,
        "claude-ada-x"
    );
    for bad in [
        ADA,
        "Claude-Ada-X",
        "claude ada",
        "claude--ada",
        "-claude",
        "claude-",
    ] {
        let read = AccountNames::of_row(&row(bad));
        assert_eq!(read.slug, "", "{bad}");
        assert_eq!(read.email.as_deref(), Some(ADA), "{bad}");
    }
    // A reserved slug is one; one byte past the bound is not.
    assert!(is_slug("claude-0123456789abcdef"));
    assert!(is_slug(&"a".repeat(MAX_SLUG)));
    assert!(!is_slug(&"a".repeat(MAX_SLUG + 1)));
    assert!(!is_slug(""));
}

/// A message shows the name and the slug; with no address it shows the slug, and with no slug
/// (no lab named it) the name. The narrow form is the slug, else the name.
#[test]
fn shown_gives_the_name_and_slug_and_narrow_the_slug_else_the_name() {
    let full = names("claude", "claude-ada-x", Some(ADA), true);
    assert_eq!(full.shown(), format!("claude {ADA} (claude-ada-x)"));
    assert_eq!(full.narrow(), "claude-ada-x");
    let bare = names("claude", "claude-1a2b3c", None, true);
    assert_eq!(bare.shown(), "claude-1a2b3c");
    assert_eq!(bare.narrow(), "claude-1a2b3c");
    let unslugged = names("claude", "", Some(ADA), true);
    assert_eq!(unslugged.shown(), format!("claude {ADA}"));
    assert_eq!(unslugged.narrow(), format!("claude {ADA}"));
    assert_eq!(AccountNames::default().narrow(), "");
}

/// A word names an account by its slug, its key, its provider word and address in either
/// spelling, or an address that names one live account; an address two live accounts share
/// names both and lists each by slug, name and key; a prefix, a provider word alone and a
/// label name none, and that refusal lists no address.
/// NEGATIVE CONTROL: with `named` giving the resolver no address handle (`email` left out of
/// `handles`), an address that names one account names none: `` left: Err("no account is named
/// `bea@y.example`: …") right: Ok("ac-00000000000000b1") ``.
#[test]
fn one_takes_every_name_of_an_account_and_lists_the_several_an_address_names() {
    let lab = lab();
    for word in [
        "oai-ada-x".to_owned(),
        "ac-00000000000000a2".to_owned(),
        format!("oai {ADA}"),
        format!("oai:{ADA}"),
    ] {
        assert_eq!(one(&lab, &word), Ok("ac-00000000000000a2"), "{word}");
    }
    assert_eq!(one(&lab, BEA), Ok("ac-00000000000000b1"));
    // The retired Google account answers its own slug, and the live ones outrank it.
    assert_eq!(one(&lab, "google-ada-x"), Ok("ac-00000000000000a3"));
    assert_eq!(
        named(&lab, ADA),
        Found::Many(vec!["ac-00000000000000a1", "ac-00000000000000a2"])
    );
    let several = one(&lab, ADA).expect_err("two live accounts");
    assert_eq!(
        several,
        many_accounts(
            ADA,
            [
                (
                    "claude-ada-x",
                    "claude ada@x.example",
                    "ac-00000000000000a1"
                ),
                ("oai-ada-x", "oai ada@x.example", "ac-00000000000000a2"),
            ]
            .into_iter()
        )
    );
    for word in [
        "claude-ada",
        "ada-x",
        "claude",
        "m2-2",
        "claude-1a2b3c",
        "x.example",
    ] {
        assert_eq!(named(&lab, word), Found::None, "{word}");
        let none = one(&lab, word).expect_err("names none");
        assert_eq!(none, no_account(word));
        assert!(!none.contains('@'), "{none}");
    }
}

/// Accounts no lab has slugged (a Mac names an account itself until the lab does) that one
/// address names, as Kimi Code's two regions of one address, are listed by their name and key
/// with no empty slug column, and the refusal offers the key and the provider word and address,
/// which tell them apart; that word names one.
/// NEGATIVE CONTROL: with `many_accounts` writing each line as `  {slug}  {name}  {pk}` and asking
/// only for a slug, the slug column is blank: `` left: Err("`cy@z.example` names 2 accounts; name
/// one by its slug:\n    kimi cy@z.example  ac-00000000000000c1\n    kimicn …") ``.
#[test]
fn accounts_with_no_slug_are_listed_by_name_and_key() {
    let lab = BTreeMap::from([
        (
            "ac-00000000000000c1".to_owned(),
            names("kimi:global", "", Some(CY), true),
        ),
        (
            "ac-00000000000000c2".to_owned(),
            names("kimi:mainland-cn", "", Some(CY), true),
        ),
    ]);
    assert_eq!(
        one(&lab, CY),
        Err(format!(
            "`{CY}` names 2 accounts; name one by its slug, its key, or its provider word and \
             address:\n  kimi {CY}  ac-00000000000000c1\n  kimicn {CY}  ac-00000000000000c2"
        ))
    );
    assert_eq!(
        one(&lab, &format!("kimicn {CY}")),
        Ok("ac-00000000000000c2")
    );
}
