// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    MAX_HANDLE, MAX_SLUG, MAX_WORD, SlugFindings, Slugged, account_name, account_pk, account_slugs,
    address_handles, canonical_realm, owner_slug, provider_word, slug_findings,
};

/// An owner's handle written by the operation `[n; 32]`.
type Owned<'a> = Option<(&'a str, u8)>;

/// The slugs of `accounts` (realm, address, owner's handle, live), in order.
fn slugs(accounts: &[(&str, &str, Owned<'_>, bool)]) -> Vec<String> {
    let keys: Vec<String> = accounts
        .iter()
        .map(|(realm, email, _, _)| account_pk(realm, email).expect("an account key"))
        .collect();
    let rows: Vec<Slugged<'_>> = accounts
        .iter()
        .zip(&keys)
        .map(|((realm, email, handle, live), pk)| Slugged {
            pk,
            realm,
            handle: handle.map(|(label, op)| (label, [op; 32])),
            claims_email: Some(email),
            live: *live,
        })
        .collect();
    let named = account_slugs(&rows);
    keys.iter().map(|pk| named[pk].clone()).collect()
}

fn key6(realm: &str, email: &str) -> String {
    let pk = account_pk(realm, email).expect("a key");
    pk[3..9].to_owned()
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

/// Every word a surface prints before an account's address names that account's realm, so
/// `add account oai` takes the word `oai ada@x.example` shows.
/// NEGATIVE CONTROL: without `oai` in `canonical_realm`'s Codex spellings, the word names no
/// realm: `` left: None right: Some("codex") `` at `oai`.
#[test]
fn every_provider_word_names_its_realm() {
    for realm in [
        "claude",
        "codex",
        "google",
        "meta",
        "grok",
        "kimi:global",
        "kimi:mainland-cn",
    ] {
        let word = provider_word(realm).expect("a word");
        assert_eq!(canonical_realm(word), Some(realm), "{word}");
        assert_eq!(canonical_realm(&word.to_uppercase()), Some(realm), "{word}");
        assert!(word.len() <= MAX_WORD, "{word}");
    }
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
    // A retired row neither forces the long handle nor takes a live row's slug: it goes long.
    let named = slugs(&[
        ("claude", "ada.fb@alpha.example", None, true),
        ("claude", "bob.fb@alpha.example", None, false),
    ]);
    assert_eq!(named, ["claude-fb-alpha", "claude-bob-fb-alpha"]);
}

/// The owner's handle names the account before any derived handle is read: a derived handle
/// whose slug it took goes long, and one whose long slug is taken too goes to the key. A live
/// account's derived handle comes before a retired account's owner handle.
/// NEGATIVE CONTROL: with the owner's handles read after the derived ones (step 1's claims made
/// once the rounds of step 2 are done), the derived handle keeps its short slug and the owner's
/// account its own derived one: `` left: ["claude-m2-beta", "claude-fb-alpha",
/// "claude-main-desk"] right: ["claude-fb-alpha", "claude-bob-fb-alpha", "claude-main-desk"] ``.
#[test]
fn the_owners_handle_comes_first_and_a_derived_one_it_meets_goes_long_then_to_the_key() {
    let named = slugs(&[
        ("claude", "ada.m2@beta.example", Some(("fb-alpha", 1)), true),
        ("claude", "bob.fb@alpha.example", None, true),
        ("claude", "cy.x@gamma.example", Some(("Main Desk", 2)), true),
    ]);
    assert_eq!(
        named,
        ["claude-fb-alpha", "claude-bob-fb-alpha", "claude-main-desk"]
    );
    // An owner's handle on the long slug too: the derived handle goes to the key.
    let named = slugs(&[
        ("claude", "ada.m2@beta.example", Some(("fb-alpha", 1)), true),
        (
            "claude",
            "dee.y@delta.example",
            Some(("bob-fb-alpha", 2)),
            true,
        ),
        ("claude", "bob.fb@alpha.example", None, true),
    ]);
    assert_eq!(
        named,
        [
            "claude-fb-alpha".to_owned(),
            "claude-bob-fb-alpha".to_owned(),
            format!("claude-{}", key6("claude", "bob.fb@alpha.example")),
        ]
    );
    // The live derived handle keeps its slug; the retired owner's handle is named as unnamed.
    let named = slugs(&[
        ("claude", "ada.fb@alpha.example", None, true),
        (
            "claude",
            "old.z@omega.example",
            Some(("fb-alpha", 1)),
            false,
        ),
    ]);
    assert_eq!(named, ["claude-fb-alpha", "claude-z-omega"]);
    // A handle past MAX_HANDLE bytes is no handle.
    let long = "h".repeat(MAX_HANDLE + 1);
    let fits = "h".repeat(MAX_HANDLE);
    let named = slugs(&[
        ("claude", "ada.fb@alpha.example", Some((&long, 1)), true),
        ("claude", "ada.m2@beta.example", Some((&fits, 2)), true),
    ]);
    assert_eq!(
        named,
        ["claude-fb-alpha".to_owned(), format!("claude-{fits}")]
    );
}

/// Two owners' handles that make one slug go to the write with the lowest operation digest,
/// whichever was written first, so a table roll, which carries each candidate at the operation
/// that wrote it, changes nothing (`alab::roll` tests hold that on a real roll). The other
/// takes its derived handle.
/// NEGATIVE CONTROL: with `account_slugs` taking the highest digest (`max_by_key`), the write
/// `[9; 32]` takes the slug: `` left: ["claude-desk", "claude-m2-gamma"] right:
/// ["claude-m2-beta", "claude-desk"] ``.
#[test]
fn two_owners_handles_for_one_slug_go_to_the_lowest_operation_digest() {
    assert_eq!(
        slugs(&[
            ("claude", "ada.m2@beta.example", Some(("desk", 9)), true),
            ("claude", "bob.m2@gamma.example", Some(("Desk", 1)), true),
        ]),
        ["claude-m2-beta", "claude-desk"]
    );
    // Two realms' owners may both say `desk`: two slugs.
    assert_eq!(
        slugs(&[
            ("claude", "ada.m2@beta.example", Some(("desk", 9)), true),
            ("codex", "bob.m2@gamma.example", Some(("desk", 1)), true),
        ]),
        ["claude-desk", "oai-desk"]
    );
}

/// Once two live accounts of a realm derive one short handle, its slug names no account: each
/// sharer takes its long handle, or its key where the long one does not fit, and neither a third
/// live account nor a retired row takes the short slug the first account held, by its derived
/// handle or by an owner's.
/// NEGATIVE CONTROLS: with `sharing` counting only the sharers whose long handle fits and
/// `shared` asking for one (as before), the pair of one such sharer and one whose long handle is
/// 39 bytes shares nothing, and the retired row takes the slug: `` left: ["claude-ann-fb-alpha",
/// "claude-740ae5", "claude-fb-alpha"] right: [… "claude-old-fb-alpha"] ``; with a shared short
/// slug left free (no `taken.insert` for it), the retired row takes it at the same pair, the
/// same failure.
#[test]
fn a_short_slug_two_live_accounts_share_names_no_account_after_them() {
    const ANN: &str = "ann.fb@alpha.example.invalid";
    const BOB: &str = "bob.fb@alpha.example.invalid";
    const OLD: &str = "old.fb@alpha.example.invalid";
    let long = "christopheralexandermontgomery.fb@alpha.example.invalid";
    assert_eq!(
        address_handles(long).map(|(_, long)| long.len()),
        Some(39),
        "its long handle does not fit"
    );
    assert_eq!(slugs(&[("claude", ANN, None, true)]), ["claude-fb-alpha"]);
    // Two sharers and one whose long handle does not fit: it goes to its key.
    assert_eq!(
        slugs(&[
            ("claude", ANN, None, true),
            ("claude", BOB, None, true),
            ("claude", long, None, true),
        ]),
        [
            "claude-ann-fb-alpha".to_owned(),
            "claude-bob-fb-alpha".to_owned(),
            format!("claude-{}", key6("claude", long)),
        ]
    );
    // One sharer whose long handle does not fit still shares: neither it nor a retired row takes
    // the short slug.
    assert_eq!(
        slugs(&[
            ("claude", ANN, None, true),
            ("claude", long, None, true),
            ("claude", OLD, None, false),
        ]),
        [
            "claude-ann-fb-alpha".to_owned(),
            format!("claude-{}", key6("claude", long)),
            "claude-old-fb-alpha".to_owned(),
        ]
    );
    // A retired row took its long handle beside the one live account; a second live account
    // leaves it there.
    assert_eq!(
        slugs(&[("claude", ANN, None, true), ("claude", OLD, None, false)]),
        ["claude-fb-alpha", "claude-old-fb-alpha"]
    );
    assert_eq!(
        slugs(&[
            ("claude", ANN, None, true),
            ("claude", OLD, None, false),
            ("claude", BOB, None, true),
        ]),
        [
            "claude-ann-fb-alpha",
            "claude-old-fb-alpha",
            "claude-bob-fb-alpha"
        ]
    );
    // Nor does a retired row the owner named by the shared handle take it.
    assert_eq!(
        slugs(&[
            ("claude", ANN, None, true),
            (
                "claude",
                "old.z@omega.example",
                Some(("fb-alpha", 1)),
                false
            ),
            ("claude", BOB, None, true),
        ]),
        [
            "claude-ann-fb-alpha",
            "claude-z-omega",
            "claude-bob-fb-alpha"
        ]
    );
}

/// An `xai-` slug whose handle holds 20 bytes or more reads as an xAI key, which the value
/// guards withhold, so an owner's label of 20 bytes names no xAI account (the registry refuses
/// it) and a derived handle that long goes to the key; under another word the same handle is a
/// slug, and so is a 19-byte xAI handle.
/// NEGATIVE CONTROL: with `handle_slug` skipping the guard (no `withheld` check), the 21-byte xAI
/// label makes a slug: `` left: Some("xai-andrew-yates-research") right: None ``.
#[test]
fn an_xai_handle_of_twenty_bytes_names_no_account() {
    let twenty_one = "andrew-yates-research";
    let nineteen = "yates-research-lab1";
    assert_eq!((twenty_one.len(), nineteen.len()), (21, 19));
    assert_eq!(owner_slug("grok", twenty_one), None);
    assert_eq!(owner_slug("grok", &"h".repeat(20)), None);
    assert_eq!(
        owner_slug("grok", nineteen).as_deref(),
        Some("xai-yates-research-lab1")
    );
    assert_eq!(
        owner_slug("claude", twenty_one).as_deref(),
        Some("claude-andrew-yates-research")
    );
    let derived = "ada.research-team-lab@alpha.example";
    assert_eq!(
        slugs(&[
            ("grok", "ada.m2@beta.example", Some((twenty_one, 1)), true),
            ("grok", "bob.m2@gamma.example", Some((nineteen, 2)), true),
            ("grok", derived, None, true),
            ("claude", "ada.m2@beta.example", Some((twenty_one, 3)), true),
        ]),
        [
            "xai-m2-beta".to_owned(),
            "xai-yates-research-lab1".to_owned(),
            format!("xai-{}", key6("grok", derived)),
            "claude-andrew-yates-research".to_owned(),
        ]
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
        assert_eq!(*slug, format!("claude-{}", key6(realm, email)), "{email}");
    }
    let unread = account_slugs(&[Slugged {
        pk: "ac-0123456789abcdef",
        realm: "claude",
        handle: None,
        claims_email: None,
        live: true,
    }]);
    assert_eq!(unread["ac-0123456789abcdef"], "claude-012345");
    // No owner's handle takes another account's last resort, its whole key.
    let other = account_pk("claude", "a.b@x.example").expect("a key");
    let whole = other[3..].to_owned();
    let named = slugs(&[
        ("claude", "c.d@y.example", Some((&whole, 1)), true),
        ("claude", "a.b@x.example", Some((&whole[..6], 2)), true),
    ]);
    assert_eq!(named[0], "claude-d-y");
    assert_eq!(named[1], format!("claude-{}", &whole[..6]));
    // Every slug is a vault name inside `claude-code-<slug>-oauth`, and a roster label.
    for slug in named.iter().chain(unread.values()) {
        assert!(slug.len() <= MAX_SLUG, "{slug}");
        assert!(
            slug.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{slug}"
        );
    }
}

/// The longest slug, the longest word and the longest handle, is shown (the value guards
/// withhold no slug for its length alone) and is a vault name inside `claude-code-<slug>-oauth`,
/// the name a roster row's snapshot is kept under, and so is an unknown realm's: its word is cut
/// to fit.
/// NEGATIVE CONTROL: with `MAX_SLUG` at 45, the vault's bound alone, a handle of the longest
/// length makes a slug the guards withhold, and the account takes its derived handle:
/// `` google-fb-alpha ``.
#[test]
fn every_slug_fits_the_vault_name_its_roster_row_keeps_a_snapshot_under() {
    let handle = "h".repeat(MAX_HANDLE);
    for realm in ["google", "kimi:mainland-cn", "some-new-realm", "+++"] {
        let named = account_slugs(&[Slugged {
            pk: "ac-0123456789abcdef",
            realm,
            handle: Some((&handle, [1; 32])),
            claims_email: Some("ada.fb@alpha.example"),
            live: true,
        }]);
        let slug = &named["ac-0123456789abcdef"];
        let vault = format!("claude-code-{slug}-oauth");
        assert!(vault.len() <= 63, "{vault} is {} bytes", vault.len());
        assert!(slug.ends_with(&handle), "{slug}");
    }
}

/// What the slugs settled is reported by key, over live accounts alone: two live Claude
/// accounts no owner named that derive one short handle are `shared`; an owner's label another
/// account's earlier label already makes is `lost`, naming that account; a label that starts
/// with its own provider word is `repeats`, and still names its account. A retired row whose
/// label lost to a live one is none of them.
/// NEGATIVE CONTROL: with `slug_findings`' provider-word test made `false`, `repeats` is empty:
/// `` left: SlugFindings { shared: [[…]], lost: [(…)], repeats: [] } right: SlugFindings {
/// shared: [[…]], lost: [(…)], repeats: ["ac-eaff6c84bfd100c3"] } ``.
#[test]
fn the_slugs_findings_name_a_shared_handle_a_lost_label_and_a_repeated_provider_word() {
    let accounts: [(&str, &str, Owned<'_>, bool); 6] = [
        ("claude", "ada.fb@alpha.example.invalid", None, true),
        ("claude", "bea.fb@alpha.example.invalid", None, true),
        (
            "claude",
            "cy@gamma.example.invalid",
            Some(("work", 1)),
            true,
        ),
        (
            "claude",
            "dee@delta.example.invalid",
            Some(("work", 2)),
            true,
        ),
        (
            "codex",
            "eve@eps.example.invalid",
            Some(("oai-team", 3)),
            true,
        ),
        (
            "claude",
            "flo@phi.example.invalid",
            Some(("work", 0)),
            false,
        ),
    ];
    let keys: Vec<String> = accounts
        .iter()
        .map(|(realm, email, _, _)| account_pk(realm, email).expect("an account key"))
        .collect();
    let rows: Vec<Slugged<'_>> = accounts
        .iter()
        .zip(&keys)
        .map(|((realm, email, handle, live), pk)| Slugged {
            pk,
            realm,
            handle: handle.map(|(label, op)| (label, [op; 32])),
            claims_email: Some(email),
            live: *live,
        })
        .collect();
    let slugs = account_slugs(&rows);
    assert_eq!(slugs[&keys[2]], "claude-work");
    assert_eq!(slugs[&keys[4]], "oai-oai-team");
    assert_eq!(
        slug_findings(&rows, &slugs),
        SlugFindings {
            shared: vec![vec![keys[0].clone(), keys[1].clone()]],
            lost: vec![(keys[3].clone(), Some(keys[2].clone()))],
            repeats: vec![keys[4].clone()],
        }
    );
}
