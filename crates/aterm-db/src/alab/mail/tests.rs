// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The effective mailbox over hand-built rows; every address is synthetic (`example.test`).

use super::*;
use crate::{
    alab::registry::ADDRESS,
    database::Change,
    typed::{
        cell,
        grid::tests::{Book, put},
        keys,
    },
};
use aterm_json::json;

fn pk(local: &str) -> String {
    keys::address_pk(&format!("{local}@example.test")).unwrap()
}

fn rows(changes: Vec<Change>) -> Projection {
    let mut book = Book::new();
    book.write(1, &[], changes);
    cell::project(&ADDRESS, &book.cells())
}

fn none(_: &str) -> Option<String> {
    None
}

/// The order of §0 and §6: the owner's mailbox first, then the alias's target, then one complete
/// observation, then this Mac's routes, then the guess. A link past a retired address or round a
/// cycle reads past it, and ends.
/// NEGATIVE CONTROL: with `follow` not following `alias_of` (the arm removed), the alias reads its
/// own hint: `` left: Effective { mailbox: "mb-guess", how: Hint, via: [] } right: Effective {
/// mailbox: "mb-hub", how: Owner, via: ["ad-…"] } ``.
#[test]
fn an_alias_reads_the_mailbox_of_what_it_is_linked_with() {
    let (ann, hub, top, bob, cy, gone, loop_a, loop_b) = (
        pk("ann"),
        pk("hub"),
        pk("top"),
        pk("bob"),
        pk("cy"),
        pk("gone"),
        pk("loop-a"),
        pk("loop-b"),
    );
    let addresses = rows(vec![
        put(&ADDRESS, &ann, "alias_of", json!(hub)),
        put(&ADDRESS, &ann, "mailbox_hint", json!("mb-guess")),
        put(&ADDRESS, &hub, "alias_of", json!(top)),
        put(&ADDRESS, &top, "mailbox", json!("mb-hub")),
        put(&ADDRESS, &bob, "mailbox", json!("mb-own")),
        put(&ADDRESS, &bob, "alias_of", json!(top)),
        put(&ADDRESS, &cy, "alias_of", json!(gone)),
        put(&ADDRESS, &cy, "mailbox_hint", json!("mb-cy")),
        put(&ADDRESS, &gone, "mailbox", json!("mb-gone")),
        put(&ADDRESS, &gone, "row", json!("retired")),
        put(&ADDRESS, &loop_a, "alias_of", json!(loop_b)),
        put(&ADDRESS, &loop_a, "mailbox_hint", json!("mb-loop")),
        put(&ADDRESS, &loop_b, "alias_of", json!(loop_a)),
    ]);
    let read = |address: &str| effective(&addresses, &Observed::new(), &none, address);
    assert_eq!(
        read(&ann),
        Some(Effective {
            mailbox: "mb-hub".to_owned(),
            how: How::Owner,
            via: vec![hub.clone(), top.clone()],
        })
    );
    // The owner's word on the alias itself comes first.
    assert_eq!(read(&bob).unwrap().mailbox, "mb-own");
    assert_eq!(read(&bob).unwrap().via, Vec::<String>::new());
    // A retired target is read past, and a retired or absent address itself resolves nothing.
    assert_eq!(read(&cy).unwrap().mailbox, "mb-cy");
    assert_eq!(read(&gone), None);
    assert_eq!(read(&pk("absent")), None);
    // A cycle ends, and each row's own guess still answers.
    assert_eq!(read(&loop_a).unwrap().mailbox, "mb-loop");
    assert_eq!(read(&loop_b).unwrap().mailbox, "mb-loop");
    assert!(alias_cycle(&addresses, &loop_a));
    assert!(!alias_cycle(&addresses, &ann));
}

/// Observation outranks a route and a route outranks the guess; two mailboxes listing one address
/// decide nothing, and an incomplete, older or mismatched-key observation says nothing.
/// NEGATIVE CONTROL: with `observed` keeping incomplete rows (the `complete` check removed), the
/// incomplete row's mailbox is listed beside the complete one's: `` left: {"mb-new", "mb-partial"}
/// ``; without the key-prefix check, `spoofed` wins `mb-new` at `at: 99` and moves `ann` back to
/// `bob`.
#[test]
fn observation_then_route_then_guess() {
    let (ann, bob) = (pk("ann"), pk("bob"));
    let addresses = rows(vec![
        put(&ADDRESS, &ann, "mailbox_hint", json!("mb-guess")),
        put(&ADDRESS, &bob, "mailbox_hint", json!("mb-guess")),
    ]);
    let route = |_: &str| Some("mb-route".to_owned());
    let older = json!({"mailbox": "mb-new", "complete": true, "at": 1, "addresses": [bob]});
    let newer = json!({"mailbox": "mb-new", "complete": true, "at": 2, "addresses": [ann, ""]});
    let partial = json!({"mailbox": "mb-partial", "complete": false, "at": 9, "addresses": [ann]});
    let spoofed = json!({"mailbox": "mb-new", "complete": true, "at": 99, "addresses": [bob]});
    let unstamped = json!({"mailbox": "mb-unstamped", "complete": true, "addresses": [ann]});
    let nameless = json!({"mailbox": "", "complete": true, "at": 9, "addresses": [ann]});
    let list = [
        ("mb-new.a", &older),
        ("mb-new.b", &newer),
        ("mb-partial.c", &partial),
        ("mb-other.d", &spoofed),
        ("mb-unstamped.e", &unstamped),
        (".f", &nameless),
    ];
    let seen = observed(list);
    assert_eq!(seen[&ann], BTreeSet::from(["mb-new".to_owned()]));
    assert!(!seen.contains_key(""));
    assert!(!seen.contains_key(&bob));
    let read = |seen: &Observed, routes: &dyn Fn(&str) -> Option<String>, pk: &str| {
        effective(&addresses, seen, routes, pk).map(|e| (e.mailbox, e.how))
    };
    assert_eq!(
        read(&seen, &route, &ann),
        Some(("mb-new".to_owned(), How::Observed))
    );
    assert_eq!(
        read(&seen, &route, &bob),
        Some(("mb-route".to_owned(), How::Routed))
    );
    assert_eq!(
        read(&seen, &none, &bob),
        Some(("mb-guess".to_owned(), How::Hint))
    );
    let two = Observed::from([(
        ann.clone(),
        BTreeSet::from(["mb-a".to_owned(), "mb-b".to_owned()]),
    )]);
    assert_eq!(
        read(&two, &none, &ann),
        Some(("mb-guess".to_owned(), How::Hint))
    );
}

/// The link an address list supplies: a `+tag` address reads the mailbox of its base once a
/// complete observation lists the base, and through it; a base no observation lists, a retired
/// base, or the owner's own `alias_of` leave the derived link out.
/// NEGATIVE CONTROL: with `follow`'s `listed_base` arm removed, the tag reads its guess: `` left:
/// Some(("mb-guess", Hint, [])) right: Some(("mb-seen", Observed, ["ad-…"])) ``.
#[test]
fn a_tag_reads_its_listed_base() {
    let tagged = |local: &str| keys::address_pk(&format!("{local}@example.test")).unwrap();
    let (base, tag, gone, gone_tag, owned_tag, top) = (
        tagged("base"),
        tagged("base+ai"),
        tagged("gone"),
        tagged("gone+ai"),
        tagged("base+own"),
        tagged("top"),
    );
    let addr =
        |pk: &str, local: &str| put(&ADDRESS, pk, "addr", json!(format!("{local}@example.test")));
    let addresses = rows(vec![
        addr(&base, "base"),
        addr(&tag, "base+ai"),
        put(&ADDRESS, &tag, "mailbox_hint", json!("mb-guess")),
        addr(&gone, "gone"),
        put(&ADDRESS, &gone, "row", json!("retired")),
        addr(&gone_tag, "gone+ai"),
        put(&ADDRESS, &gone_tag, "mailbox_hint", json!("mb-guess")),
        addr(&owned_tag, "base+own"),
        put(&ADDRESS, &owned_tag, "alias_of", json!(top)),
        addr(&top, "top"),
        put(&ADDRESS, &top, "mailbox", json!("mb-top")),
    ]);
    let seen = Observed::from([
        (base.clone(), BTreeSet::from(["mb-seen".to_owned()])),
        (gone.clone(), BTreeSet::from(["mb-seen".to_owned()])),
    ]);
    let read = |seen: &Observed, pk: &str| {
        effective(&addresses, seen, &none, pk).map(|e| (e.mailbox, e.how, e.via))
    };
    assert_eq!(
        read(&seen, &tag),
        Some(("mb-seen".to_owned(), How::Observed, vec![base.clone()]))
    );
    assert_eq!(
        listed_base(&addresses, &seen, &tag),
        Some((base.clone(), "mb-seen".to_owned()))
    );
    // Nothing lists the base: the tag keeps its own guess.
    assert_eq!(
        read(&Observed::new(), &tag),
        Some(("mb-guess".to_owned(), How::Hint, Vec::new()))
    );
    // A retired base is no link.
    assert_eq!(read(&seen, &gone_tag).unwrap().0, "mb-guess");
    // The owner's link comes first.
    assert_eq!(read(&seen, &owned_tag).unwrap().0, "mb-top");
    assert_eq!(listed_base(&addresses, &seen, &base), None);
    // A base two mailboxes list decides nothing: the tag keeps its own exact route, and never takes
    // up the base's route or guess.
    let two = Observed::from([(
        base.clone(),
        BTreeSet::from(["mb-seen".to_owned(), "mb-stale".to_owned()]),
    )]);
    let route_tag = tag.clone();
    let routes = move |pk: &str| (pk == route_tag).then(|| "mb-route".to_owned());
    assert_eq!(listed_base(&addresses, &two, &tag), None);
    assert_eq!(
        effective(&addresses, &two, &routes, &tag).map(|e| (e.mailbox, e.how, e.via)),
        Some(("mb-route".to_owned(), How::Routed, Vec::new()))
    );
}

/// The derived link is decided by the observation that supplies it alone: a base that two
/// mailboxes list and that carries its own guess leaves its `+tag` on the tag's own route.
/// NEGATIVE CONTROL: with `listed_base` taking the first of several listing mailboxes, the tag
/// reads one of them: `` left: Some(("mb-a", Observed, ["ad-…"])) right: Some(("mb-route",
/// Routed, [])) ``.
#[test]
fn a_tag_never_takes_up_its_base_s_guess() {
    let (base, tag) = (pk("base"), pk("base+ai"));
    let addresses = rows(vec![
        put(&ADDRESS, &base, "addr", json!("base@example.test")),
        put(&ADDRESS, &base, "mailbox_hint", json!("mb-base-guess")),
        put(&ADDRESS, &tag, "addr", json!("base+ai@example.test")),
    ]);
    let seen = Observed::from([(base, BTreeSet::from(["mb-a".to_owned(), "mb-c".to_owned()]))]);
    let route_tag = tag.clone();
    let routes = move |pk: &str| (pk == route_tag).then(|| "mb-route".to_owned());
    assert_eq!(
        effective(&addresses, &seen, &routes, &tag).map(|e| (e.mailbox, e.how, e.via)),
        Some(("mb-route".to_owned(), How::Routed, Vec::new()))
    );
}
