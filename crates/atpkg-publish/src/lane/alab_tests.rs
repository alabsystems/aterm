// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn ledger(one: &str, two: &str, live: &str, revoked: &str) -> aterm_json::Value {
    aterm_json::from_str::<aterm_json::Value>(&format!(
        "{{\"mappings\":{{\"ay\":{{\"{one}\":{{\"release_rev\":\"d766\"}},\
         \"{two}\":{{\"release_rev\":\"d766\"}},\"{live}\":{{\"release_rev\":\"d766\"}},\
         \"{}\":{{\"release_rev\":\"other\"}}}}}},\"revoked\":{{{revoked}}}}}",
        "9".repeat(40)
    ))
    .unwrap()
}

/// A REVOKED ROW NEVER NAMES THE DEV COMMIT A SIGNED RELEASE WAS CUT FROM, AND TWO LIVE ROWS
/// DECIDE NONE: ay's signed commit d766… is the release of two revoked rows and one live one,
/// as the ledger held it on 2026-11-07, and only the live one is its source. NEGATIVE
/// CONTROL: with the revocations dropped, the sorted first row (a revoked one) was taken.
#[test]
fn a_revoked_row_never_names_the_source_and_two_live_rows_decide_none() {
    let (one, two, live) = ("0".repeat(40), "5".repeat(40), "6".repeat(40));
    let revoked = ledger(
        &one,
        &two,
        &live,
        &format!("\"ay\":{{\"{one}\":{{}},\"{two}\":{{}}}}"),
    );
    assert_eq!(
        sources(&revoked, "ay", "d766"),
        (vec![live.as_str()], vec![one.as_str(), two.as_str()])
    );
    assert_eq!(sources(&revoked, "trust-ir", "d766"), (vec![], vec![]));
    let unrevoked = ledger(&one, &two, &live, "");
    assert_eq!(
        sources(&unrevoked, "ay", "d766").0,
        vec![one.as_str(), two.as_str(), live.as_str()],
        "NEGATIVE CONTROL: without the revocations every row is live, so none is decided"
    );
}

/// A RELEASE WHOSE EVERY ROW IS REVOKED IS SAID TO BE WITHDRAWN, NOT AWAITING ITS PROMOTE:
/// the engine refuses to map a revoked source again, so waiting for a promote would wait for
/// one that already ran. NEGATIVE CONTROL: the same rows unrevoked are no withdrawal, and a
/// release with no row at all is still awaiting its promote.
#[test]
fn a_release_whose_every_row_is_revoked_is_withdrawn_not_awaited() {
    let (one, two, three) = ("0".repeat(40), "5".repeat(40), "6".repeat(40));
    let every = ledger(
        &one,
        &two,
        &three,
        &format!("\"ay\":{{\"{one}\":{{}},\"{two}\":{{}},\"{three}\":{{}}}}"),
    );
    let parsed = |value| CachedLedger::Parsed {
        at: "engine main 0123".into(),
        value,
    };
    let clone = Path::new("/nonexistent");
    let why = resolve_dev(&parsed(every.clone()), "ay", "d766", clone).unwrap_err();
    assert!(why.contains("every row naming it is revoked"), "{why}");
    assert!(
        why.contains(&one[..12]) && why.contains(&three[..12]),
        "{why}"
    );
    assert!(!why.contains("recorded none"), "{why}");
    let none = resolve_dev(&parsed(every), "ay", "absent", clone).unwrap_err();
    assert!(
        none.contains("its promote has recorded none there yet"),
        "{none}"
    );
    let live =
        resolve_dev(&parsed(ledger(&one, &two, &three, "")), "ay", "d766", clone).unwrap_err();
    assert!(
        live.contains("3 live ay rows") && !live.contains("withdrew"),
        "NEGATIVE CONTROL: {live}"
    );
}
