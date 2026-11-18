// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Named, Resolved, resolve};

/// The lab of these tests: one address signed in to Claude and OpenAI, a second Claude address
/// that Google also holds (retired there), and a retired Claude account.
pub(crate) const ROWS: [(&str, bool, [&str; 3]); 5] = [
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
