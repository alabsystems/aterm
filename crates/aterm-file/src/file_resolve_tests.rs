// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A read's rows named through the resolver (`crate::resolver`), over the resolver tests' lab.

use super::Read;
use crate::{
    ACCOUNT_HANDLES, Status, Word, many_accounts, no_account,
    resolver::tests::ROWS,
    snapshot::{Column, Row, Snapshot, Source},
};

fn read(handles: &[&str]) -> Read {
    let columns = handles
        .iter()
        .map(|name| Column {
            name: (*name).to_owned(),
            source: Source::Derived,
            read_only: true,
            refers: None,
            ty: None,
            null: None,
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
    let accounts = read(&ACCOUNT_HANDLES);
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
        "`ada.fb@alpha.example` names 2 accounts; name one by its slug, its key, or its \
         provider word and address:\n  \
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
