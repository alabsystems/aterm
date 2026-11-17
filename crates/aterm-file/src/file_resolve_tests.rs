// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Named, Read, Resolved, many_accounts, no_account, resolve};
use crate::{
    Status, Word,
    snapshot::{Column, Row, Snapshot, Source},
};

/// The lab of these tests: one address signed in to Claude and OpenAI, a second Claude address
/// that Google also holds (retired there), and a retired Claude account.
const ROWS: [(&str, bool, [&str; 3]); 5] = [
    (
        "ac-0000000000000001",
        true,
        [
            "claude-fb-alpha",
            "claude ada.fb@alpha.example",
            "ada.fb@alpha.example",
        ],
    ),
    (
        "ac-0000000000000002",
        true,
        [
            "oai-fb-alpha",
            "oai ada.fb@alpha.example",
            "ada.fb@alpha.example",
        ],
    ),
    (
        "ac-0000000000000003",
        true,
        [
            "claude-m2-beta",
            "claude ada.m2@beta.example",
            "ada.m2@beta.example",
        ],
    ),
    (
        "ac-0000000000000004",
        false,
        [
            "google-m2-beta",
            "google ada.m2@beta.example",
            "ada.m2@beta.example",
        ],
    ),
    (
        "ac-0000000000000005",
        false,
        [
            "claude-old-gamma",
            "claude old@gamma.example",
            "old@gamma.example",
        ],
    ),
];

fn rows() -> Vec<Named<'static>> {
    ROWS.iter()
        .map(|(pk, live, handles)| Named {
            pk,
            live: *live,
            handles: handles.to_vec(),
        })
        .collect()
}

fn named(word: &str) -> Vec<&'static str> {
    resolve(word, &rows())
        .places()
        .into_iter()
        .map(|place| ROWS[place].0)
        .collect()
}

/// Every name the design gives an account names it, and nothing else does: its slug, its key,
/// its provider word and address in both spellings, and an address that names one account.
/// An address that names several names each, live rows are tried before retired ones, and a
/// prefix, a substring or another case names nothing.
/// NEGATIVE CONTROL: with `resolve` taking a cell that starts with the word (`cell.starts_with
/// (wanted)` for `*cell == wanted`), a slug's prefix names a row: `` left: ["ac-0000000000000001"]
/// right: [] `` at `claude-fb`. With the live pass dropped (`for live in [true, false]` reading
/// every row in one pass), the address Claude and a retired Google row share names both:
/// `` left: ["ac-0000000000000003", "ac-0000000000000004"] right: ["ac-0000000000000003"] ``.
#[test]
fn an_account_is_named_by_its_slug_key_provider_and_address_or_an_address_of_one() {
    for word in [
        "claude-fb-alpha",
        "ac-0000000000000001",
        "claude ada.fb@alpha.example",
        "claude:ada.fb@alpha.example",
    ] {
        assert_eq!(named(word), ["ac-0000000000000001"], "{word}");
    }
    assert_eq!(named("oai:ada.fb@alpha.example"), ["ac-0000000000000002"]);
    // One address signed in to two providers is two accounts.
    assert_eq!(
        named("ada.fb@alpha.example"),
        ["ac-0000000000000001", "ac-0000000000000002"]
    );
    assert!(matches!(
        resolve("ada.fb@alpha.example", &rows()),
        Resolved::Many(_)
    ));
    // An address a live account and a retired one share names the live one.
    assert_eq!(named("ada.m2@beta.example"), ["ac-0000000000000003"]);
    // A retired account is named by any of its names, and by its key.
    assert_eq!(named("old@gamma.example"), ["ac-0000000000000005"]);
    assert_eq!(named("google:ada.m2@beta.example"), ["ac-0000000000000004"]);
    assert_eq!(named("ac-0000000000000004"), ["ac-0000000000000004"]);
    // Exact only.
    for word in [
        "claude-fb",
        "fb-alpha",
        "claude-fb-alpha2",
        "Claude-FB-Alpha",
        "fb@alpha.example",
        "ada.fb@alpha",
        "ac-000000000000000",
        "claude",
        "oai:ada.fb@alpha",
        "claude  ada.fb@alpha.example",
        "",
    ] {
        assert_eq!(named(word), Vec::<&str>::new(), "{word:?}");
    }
}

/// An empty handle cell names nothing, even for an empty word, and a row with fewer handles than
/// another is read up to its own.
#[test]
fn an_empty_cell_names_nothing() {
    let rows = [
        Named {
            pk: "mc-1",
            live: true,
            handles: vec!["", "desk"],
        },
        Named {
            pk: "mc-2",
            live: true,
            handles: vec!["m2"],
        },
    ];
    assert_eq!(resolve("", &rows), Resolved::None);
    assert_eq!(resolve("desk", &rows), Resolved::One(0));
    assert_eq!(resolve("m2", &rows), Resolved::One(1));
}

fn read(handles: &[&str]) -> Read {
    let columns = handles
        .iter()
        .map(|name| Column {
            name: (*name).to_owned(),
            source: Source::Derived,
            read_only: true,
            refers: None,
        })
        .collect();
    let rows = ROWS
        .iter()
        .map(|(pk, live, cells)| Row {
            pk: (*pk).to_owned(),
            live: *live,
            cells: handles
                .iter()
                .zip(cells)
                .map(|(name, cell)| ((*name).to_owned(), (*cell).to_owned()))
                .collect(),
            values: aterm_json::Map::new(),
        })
        .collect();
    Read {
        status: Status {
            word: Word::Live,
            why: None,
            state: None,
            problem: None,
            queued: 0,
            held: 0,
        },
        snapshot: Some(Snapshot {
            name: crate::Name::parse("ai/accounts").expect("a name"),
            kind: "table".to_owned(),
            ext: "tsv".to_owned(),
            rev: 1,
            complete: true,
            at_ms: 1,
            columns,
            handles: handles.iter().map(|name| (*name).to_owned()).collect(),
            rows,
        }),
    }
}

/// A word that names no account says how to name one and lists none, so a typo prints no
/// address; one that names several lists each by its slug, its name and its key.
/// NEGATIVE CONTROL: with `Read::one` answering a word that names nothing with the generic
/// listing of rows (the `[] if accounts` arm removed), the refusal names the handle columns and
/// not the account's words: `` left: "no row is named `fb-alpha`: name one by its key or its
/// slug or name or email" ``.
#[test]
fn a_word_that_names_no_account_lists_none_and_one_that_names_several_lists_each() {
    let accounts = read(&super::ACCOUNT_HANDLES);
    assert_eq!(
        accounts.one("claude-fb-alpha").map(|row| row.pk.as_str()),
        Ok("ac-0000000000000001")
    );
    let none = accounts.one("fb-alpha").expect_err("names none");
    assert_eq!(none, no_account("fb-alpha"));
    assert_eq!(
        none,
        "no account is named `fb-alpha`: name one by its slug, its key, or its provider word \
         and address"
    );
    assert!(!none.contains('@'), "{none}");
    let several = accounts.one("ada.fb@alpha.example").expect_err("names two");
    assert_eq!(
        several,
        "`ada.fb@alpha.example` names 2 accounts; name one by its slug:\n  \
         claude-fb-alpha  claude ada.fb@alpha.example  ac-0000000000000001\n  \
         oai-fb-alpha  oai ada.fb@alpha.example  ac-0000000000000002"
    );
    assert_eq!(
        several,
        many_accounts(
            "ada.fb@alpha.example",
            [
                (
                    "claude-fb-alpha",
                    "claude ada.fb@alpha.example",
                    "ac-0000000000000001"
                ),
                (
                    "oai-fb-alpha",
                    "oai ada.fb@alpha.example",
                    "ac-0000000000000002"
                ),
            ]
            .into_iter()
        )
    );
    // Another file's rows are named in a row's words.
    let machines = read(&["label", "nickname"]);
    assert_eq!(
        machines.one("nope").expect_err("names none"),
        "no row is named `nope`: name one by its key or its label or nickname"
    );
    let empty = Read {
        snapshot: None,
        ..read(&[])
    };
    assert_eq!(
        empty.one("m2").expect_err("no snapshot"),
        "no row is named `m2`: name one by its key"
    );
}
