// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    Slugged, account_name, account_pk, account_slugs, address_handles, default_account_label,
    provider_word,
};

fn slugs(accounts: &[(&str, &str, Option<&str>, bool)]) -> Vec<String> {
    let keys: Vec<String> = accounts
        .iter()
        .map(|(realm, email, _, _)| account_pk(realm, email).expect("an account key"))
        .collect();
    let rows: Vec<Slugged<'_>> = accounts
        .iter()
        .zip(&keys)
        .map(|((realm, email, label, live), pk)| Slugged {
            pk,
            realm,
            label: *label,
            claims_email: Some(email),
            live: *live,
        })
        .collect();
    let named = account_slugs(&rows);
    keys.iter().map(|pk| named[pk].clone()).collect()
}

#[test]
fn each_realm_has_its_provider_word() {
    for (realm, word) in [
        ("claude", "claude"),
        ("codex", "oai"),
        ("google", "google"),
        ("meta", "meta"),
        ("grok", "xai"),
        ("kimi:global", "kimi"),
        ("kimi:mainland-cn", "kimicn"),
    ] {
        assert_eq!(provider_word(realm), Some(word), "{realm}");
    }
    assert_eq!(provider_word("nope"), None);
    assert_eq!(
        account_name("codex", "ada.fb@alpha.example").as_deref(),
        Some("oai ada.fb@alpha.example")
    );
}

#[test]
fn one_address_is_three_slugs_and_the_design_table_holds() {
    assert_eq!(
        slugs(&[
            ("claude", "ada.fb@alpha.example", None, true),
            ("codex", "ada.fb@alpha.example", None, true),
            ("google", "ada.fb@alpha.example", None, true),
            ("claude", "ada.m2@beta.example", None, true),
            ("claude", "ada.m2@b.example", None, true),
        ]),
        [
            "claude-fb-alpha",
            "oai-fb-alpha",
            "google-fb-alpha",
            "claude-m2-beta",
            "claude-m2-b"
        ]
    );
    // The tail is what follows the first `+`, else the first `.`, else the whole local part.
    assert_eq!(
        address_handles("ada.x+Work.Lab@alpha.example"),
        Some((
            "work-lab-alpha".to_owned(),
            "ada-x-work-lab-alpha".to_owned()
        ))
    );
    assert_eq!(
        address_handles("ada.fb.two@alpha.example")
            .map(|(short, _)| short)
            .as_deref(),
        Some("fb-two-alpha")
    );
    assert_eq!(
        address_handles("ada@alpha.example")
            .map(|(short, _)| short)
            .as_deref(),
        Some("ada-alpha")
    );
    assert_eq!(address_handles("no address"), None);
}

#[test]
fn two_live_accounts_of_one_realm_with_one_handle_take_the_long_handle() {
    assert_eq!(
        slugs(&[
            ("claude", "ada.fb@alpha.example", None, true),
            ("claude", "bob.fb@alpha.example", None, true),
            // Another realm with that handle is another slug, and keeps the short one.
            ("codex", "ada.fb@alpha.example", None, true),
        ]),
        ["claude-ada-fb-alpha", "claude-bob-fb-alpha", "oai-fb-alpha"]
    );
    // A retired row neither forces the long handle nor takes a live row's slug.
    let named = slugs(&[
        ("claude", "ada.fb@alpha.example", None, true),
        ("claude", "bob.fb@alpha.example", None, false),
    ]);
    assert_eq!(named[0], "claude-fb-alpha");
    let retired = account_pk("claude", "bob.fb@alpha.example").expect("a key");
    assert_eq!(named[1], format!("claude-{}", &retired[3..9]));
}

#[test]
fn the_owners_label_is_the_handle_and_a_default_label_is_not() {
    let pk = account_pk("claude", "ada.fb@alpha.example").expect("a key");
    let default = default_account_label("claude", &pk).expect("a default label");
    assert_eq!(
        slugs(&[
            ("claude", "ada.fb@alpha.example", Some(&default), true),
            ("claude", "ada.m2@beta.example", Some("Main Desk"), true),
        ]),
        ["claude-fb-alpha", "claude-main-desk"]
    );
}

#[test]
fn slugs_are_distinct_whatever_the_rows_say() {
    // Long handles that still collide, a handle too long, and no address at all: the key.
    let long = format!("{}@alpha.example", "a".repeat(60));
    let rows = [
        ("claude", "a.b@x.example", None, true),
        ("claude", "a+b@x.example", None, true),
        ("claude", long.as_str(), None, true),
    ];
    let named = slugs(&rows);
    for ((realm, email, _, _), slug) in rows.iter().zip(&named) {
        let pk = account_pk(realm, email).expect("a key");
        assert_eq!(*slug, format!("claude-{}", &pk[3..9]), "{email}");
    }
    let unread = account_slugs(&[Slugged {
        pk: "ac-0123456789abcdef",
        realm: "claude",
        label: None,
        claims_email: None,
        live: true,
    }]);
    assert_eq!(unread["ac-0123456789abcdef"], "claude-012345");
    // Every slug fits the roster label grammar `[a-z0-9][a-z0-9._-]{0,63}`.
    for slug in named.iter().chain(unread.values()) {
        assert!(
            slug.len() <= 64
                && slug
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
    }
}
