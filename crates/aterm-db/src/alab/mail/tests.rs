// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The effective mailbox over hand-built rows; every address is synthetic (`example.test`).

use super::*;
use crate::{
    alab::registry::{ADDRESS, AddressIndex},
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
    let index = AddressIndex::of(&addresses);
    let read = |address: &str| effective(&addresses, &index, &Observed::new(), &none, address);
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
    let index = AddressIndex::of(&addresses);
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
        effective(&addresses, &index, seen, routes, pk).map(|e| (e.mailbox, e.how))
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
    let index = AddressIndex::of(&addresses);
    let seen = Observed::from([
        (base.clone(), BTreeSet::from(["mb-seen".to_owned()])),
        (gone.clone(), BTreeSet::from(["mb-seen".to_owned()])),
    ]);
    let read = |seen: &Observed, pk: &str| {
        effective(&addresses, &index, seen, &none, pk).map(|e| (e.mailbox, e.how, e.via))
    };
    assert_eq!(
        read(&seen, &tag),
        Some(("mb-seen".to_owned(), How::Observed, vec![base.clone()]))
    );
    assert_eq!(
        listed_base(&addresses, &index, &seen, &tag),
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
    assert_eq!(listed_base(&addresses, &index, &seen, &base), None);
    // A base two mailboxes list decides nothing: the tag keeps its own exact route, and never takes
    // up the base's route or guess.
    let two = Observed::from([(
        base.clone(),
        BTreeSet::from(["mb-seen".to_owned(), "mb-stale".to_owned()]),
    )]);
    let route_tag = tag.clone();
    let routes = move |pk: &str| (pk == route_tag).then(|| "mb-route".to_owned());
    assert_eq!(listed_base(&addresses, &index, &two, &tag), None);
    assert_eq!(
        effective(&addresses, &index, &two, &routes, &tag).map(|e| (e.mailbox, e.how, e.via)),
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
    let index = AddressIndex::of(&addresses);
    let seen = Observed::from([(base, BTreeSet::from(["mb-a".to_owned(), "mb-c".to_owned()]))]);
    let route_tag = tag.clone();
    let routes = move |pk: &str| (pk == route_tag).then(|| "mb-route".to_owned());
    assert_eq!(
        effective(&addresses, &index, &seen, &routes, &tag).map(|e| (e.mailbox, e.how, e.via)),
        Some(("mb-route".to_owned(), How::Routed, Vec::new()))
    );
}

/// A `+tag`'s base is the row whose `addr` says the base address now, not the row its key derives:
/// a row renamed onto the base address links the tag, and the row born with that address and
/// renamed away links nothing.
/// NEGATIVE CONTROL: with `listed_base` keying the base by `keys::address_pk` again, the tag
/// follows the row renamed away: `` left: Some(("ad-…", "mb-old")) right: Some(("ad-…",
/// "mb-seen")) ``.
#[test]
fn a_tag_reads_the_base_its_row_says_now() {
    let (moved, born_base, tag) = (pk("moved"), pk("base"), pk("base+ai"));
    let addresses = rows(vec![
        put(&ADDRESS, &moved, "addr", json!("base@example.test")),
        put(
            &ADDRESS,
            &born_base,
            "addr",
            json!("elsewhere@example.test"),
        ),
        put(&ADDRESS, &tag, "addr", json!("base+ai@example.test")),
        put(&ADDRESS, &tag, "mailbox_hint", json!("mb-guess")),
    ]);
    let index = AddressIndex::of(&addresses);
    let seen = Observed::from([
        (moved.clone(), BTreeSet::from(["mb-seen".to_owned()])),
        (born_base, BTreeSet::from(["mb-old".to_owned()])),
    ]);
    assert_eq!(
        listed_base(&addresses, &index, &seen, &tag),
        Some((moved.clone(), "mb-seen".to_owned()))
    );
    assert_eq!(
        effective(&addresses, &index, &seen, &none, &tag).map(|e| (e.mailbox, e.how, e.via)),
        Some(("mb-seen".to_owned(), How::Observed, vec![moved]))
    );
}

#[test]
fn a_protected_address_never_resolves_a_mailbox() {
    let (priv_addr, alias, tag) = (pk("private"), pk("alias"), pk("private+ai"));
    let addresses = rows(vec![
        put(&ADDRESS, &priv_addr, "addr", json!("private@example.test")),
        put(&ADDRESS, &priv_addr, "protected", json!(true)),
        put(&ADDRESS, &priv_addr, "mailbox", json!("mb-own")),
        put(&ADDRESS, &priv_addr, "mailbox_hint", json!("mb-hint")),
        put(&ADDRESS, &alias, "addr", json!("alias@example.test")),
        put(&ADDRESS, &alias, "alias_of", json!(priv_addr)),
        put(&ADDRESS, &tag, "addr", json!("private+ai@example.test")),
    ]);
    let index = AddressIndex::of(&addresses);
    let seen = Observed::from([(priv_addr.clone(), BTreeSet::from(["mb-obs".to_owned()]))]);
    let route_priv = priv_addr.clone();
    let routes = move |pk: &str| (pk == route_priv).then(|| "mb-route".to_owned());
    assert_eq!(
        effective(&addresses, &index, &seen, &routes, &priv_addr),
        None
    );
    assert_eq!(effective(&addresses, &index, &seen, &none, &alias), None);
    assert_eq!(effective(&addresses, &index, &seen, &none, &tag), None);
}

/// A sign-in's mailbox from registry rows alone (step A9): the MAILBOX row names its login by
/// the address row, and its patterns, webmail and held credentials, never a value.
mod sign_in {
    use super::*;
    use crate::{
        alab::{registry::MAILBOX, secret::Journal},
        typed::{
            grid::tests::elem,
            secret::{Fixed, Sealer, Secret},
        },
    };

    const TABLE: &str = "alab/registry";

    fn sealed(collection: &str, row: &str, column: &str, value: &str) -> Value {
        json!(
            Fixed(TABLE)
                .seal(collection, row, column, &Secret::new(value.to_owned()))
                .unwrap()
        )
    }

    struct Fixture {
        book: Book,
        login: String,
        mailbox: String,
        member: String,
    }

    /// Every value is synthetic: an `example.test` login and fixture credentials.
    fn fixture() -> Fixture {
        let login = pk("owner");
        let member = pk("ai1");
        let mailbox = keys::mailbox_pk(&login).unwrap();
        let mut book = Book::new();
        book.write(
            1,
            &[],
            vec![
                put(&ADDRESS, &login, "addr", json!("owner@example.test")),
                put(
                    &ADDRESS,
                    &login,
                    "password",
                    sealed("address", &login, "password", "fixture-login-pass"),
                ),
                put(&ADDRESS, &member, "addr", json!("ai1@example.test")),
                put(&MAILBOX, &mailbox, "label", json!("proton-1")),
                put(&MAILBOX, &mailbox, "service", json!("proton")),
                put(&MAILBOX, &mailbox, "code_path", json!("webmail")),
                put(
                    &MAILBOX,
                    &mailbox,
                    "webmail",
                    json!("https://mail.example.test/inbox"),
                ),
                put(&MAILBOX, &mailbox, "login", json!(login)),
                put(
                    &MAILBOX,
                    &mailbox,
                    "mailbox_password",
                    sealed(
                        "mailbox",
                        &mailbox,
                        "mailbox_password",
                        "fixture-second-pass",
                    ),
                ),
                put(
                    &MAILBOX,
                    &mailbox,
                    "totp",
                    sealed(
                        "mailbox",
                        &mailbox,
                        "totp",
                        "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
                    ),
                ),
                elem(&MAILBOX, &mailbox, "receives", &member),
                put(
                    &MAILBOX,
                    &mailbox,
                    "receives_domains",
                    json!(["pm.example.test", "*"]),
                ),
            ],
        );
        Fixture {
            book,
            login,
            mailbox,
            member,
        }
    }

    fn projections(book: &Book) -> (Projection, Projection) {
        let cells = book.cells();
        (
            cell::project(&ADDRESS, &cells),
            cell::project(&MAILBOX, &cells),
        )
    }

    fn reveal_all(
        addresses: &Projection,
        mailboxes: &Projection,
        journal: &Journal,
        mailbox: &str,
    ) -> Vec<std::result::Result<String, Withheld>> {
        Credential::ALL
            .into_iter()
            .map(|which| {
                let ask = Ask {
                    mailbox,
                    which,
                    when_ms: 1_000,
                };
                reveal_in(
                    addresses,
                    mailboxes,
                    &Fixed(TABLE),
                    journal,
                    ask,
                    str::to_owned,
                )
                .unwrap()
            })
            .collect()
    }

    /// The mailbox reads its login, webmail, patterns and held credentials from its rows, and
    /// each credential reveals through the sealer, journalled once, with no value in the journal.
    /// NEGATIVE CONTROL: with [`mailboxes`] reading the login from the MAILBOX row's own `label`
    /// instead of its `login` address row, the login reads `proton-1` and the assertion on
    /// `owner@example.test` fails.
    #[test]
    fn a_sign_in_reads_the_login_and_its_credentials_from_registry_rows() {
        let Fixture {
            book,
            login,
            mailbox,
            member,
        } = fixture();
        let (addresses, mailboxes_rows) = projections(&book);
        let found = mailboxes(&addresses, &mailboxes_rows);
        assert_eq!(found.len(), 1);
        let only = &found[0];
        assert_eq!(only.pk, mailbox);
        assert_eq!(only.login, "owner@example.test");
        assert_eq!(only.login_pk, login);
        assert_eq!(only.label, "proton-1");
        assert_eq!(
            only.webmail.as_deref(),
            Some("https://mail.example.test/inbox")
        );
        assert_eq!(
            only.receives,
            ["ai1@example.test", "*@pm.example.test", "*"],
            "exact, then domain, then any"
        );
        assert_eq!(only.held, Credential::ALL);
        let _ = member;

        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        assert_eq!(
            reveal_all(&addresses, &mailboxes_rows, &journal, &mailbox),
            [
                Ok("fixture-login-pass".to_owned()),
                Ok("fixture-second-pass".to_owned()),
                Ok("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".to_owned()),
            ]
        );
        let log = std::fs::read_to_string(dir.path().join("reveals.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 3, "one record per reveal");
        assert!(log.lines().all(|line| line.contains("SignIn")));
        for value in ["fixture-login-pass", "fixture-second-pass", "GEZDGNBVGY3TQ"] {
            assert!(
                !log.contains(value),
                "the journal holds coordinates, never a value"
            );
        }
        assert!(log.contains(&login) && log.contains(&mailbox));
    }

    /// Two writes of one password under fresh nonces are one value; two different values are
    /// withheld until the owner picks one, an empty cell says so, and a mailbox whose login row
    /// is retired is no sign-in's. Nothing withheld reaches the journal.
    /// NEGATIVE CONTROL: with the candidate comparison in [`reveal_in`] removed (the first token
    /// always revealed), the two-values assertion reads `Ok("fixture-other")`.
    #[test]
    fn two_values_are_withheld_and_a_retired_login_is_no_mailbox() {
        let Fixture {
            mut book,
            login,
            mailbox,
            ..
        } = fixture();
        // Two other Macs write, each unaware of the first's write: concurrent candidates. One
        // writes the same login password under a fresh nonce, the other a different second one.
        book.write(
            2,
            &[],
            vec![put(
                &ADDRESS,
                &login,
                "password",
                sealed("address", &login, "password", "fixture-login-pass"),
            )],
        );
        book.write(
            3,
            &[],
            vec![put(
                &MAILBOX,
                &mailbox,
                "mailbox_password",
                sealed("mailbox", &mailbox, "mailbox_password", "fixture-other"),
            )],
        );
        let (addresses, mailboxes_rows) = projections(&book);
        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        let got = reveal_all(&addresses, &mailboxes_rows, &journal, &mailbox);
        assert_eq!(
            got[0],
            Ok("fixture-login-pass".to_owned()),
            "equal values are one"
        );
        assert_eq!(got[1], Err(Withheld::TwoValues));
        assert_eq!(got[2], Ok("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".to_owned()));
        let log = std::fs::read_to_string(dir.path().join("reveals.jsonl")).unwrap();
        assert_eq!(
            log.lines().count(),
            2,
            "a withheld value is never journalled"
        );

        let mut empty = Book::new();
        empty.write(
            1,
            &[],
            vec![
                put(&ADDRESS, &login, "addr", json!("owner@example.test")),
                put(&MAILBOX, &mailbox, "label", json!("proton-1")),
                put(&MAILBOX, &mailbox, "service", json!("proton")),
                put(&MAILBOX, &mailbox, "code_path", json!("webmail")),
                put(&MAILBOX, &mailbox, "login", json!(login)),
            ],
        );
        let (addresses, mailboxes_rows) = projections(&empty);
        assert!(mailboxes(&addresses, &mailboxes_rows)[0].held.is_empty());
        assert_eq!(
            reveal_all(&addresses, &mailboxes_rows, &journal, &mailbox)[0],
            Err(Withheld::NotSet)
        );
        empty.write(1, &[], vec![put(&ADDRESS, &login, "row", json!("retired"))]);
        let (addresses, mailboxes_rows) = projections(&empty);
        assert!(mailboxes(&addresses, &mailboxes_rows).is_empty());
        assert_eq!(
            reveal_all(&addresses, &mailboxes_rows, &journal, &mailbox)[0],
            Err(Withheld::NoMailbox)
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("reveals.jsonl"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    /// A token another table's sealer wrote does not open here: withheld as `Unopened`.
    #[test]
    fn a_cell_that_does_not_open_here_is_withheld() {
        let Fixture { book, mailbox, .. } = fixture();
        let (addresses, mailboxes_rows) = projections(&book);
        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        let ask = Ask {
            mailbox: &mailbox,
            which: Credential::Totp,
            when_ms: 1,
        };
        assert_eq!(
            reveal_in(
                &addresses,
                &mailboxes_rows,
                &Fixed("alab/other"),
                &journal,
                ask,
                str::to_owned
            )
            .unwrap(),
            Err(Withheld::Unopened)
        );
    }
}

/// A login names the mailbox of the row that says it now: a row renamed onto the login keeps its
/// birth key, so the mailbox key derives from that key; a login no row says names the key it
/// derives; one two rows say, or one whose derived key a row renamed away holds, names none.
/// What discovery keys a mailbox by, and what names `ai-mailbox.toml`'s version-1 logins.
/// NEGATIVE CONTROL: with `login_mailbox`'s `Row` arm deriving from the login's own key, the
/// renamed row's login names the mailbox its birth row's key derives:
/// `` left: Some("mb-26170bc9caa8be53") right: Some("mb-f6a9e6e07e36b369") ``.
#[test]
fn a_login_names_the_mailbox_of_the_row_that_says_it_now() {
    let (moved, born, twin_a, twin_b) = (pk("moved"), pk("login"), pk("twin-a"), pk("twin-b"));
    let addresses = rows(vec![
        put(&ADDRESS, &moved, "addr", json!("login@example.test")),
        put(&ADDRESS, &born, "addr", json!("elsewhere@example.test")),
        put(&ADDRESS, &twin_a, "addr", json!("twin@example.test")),
        put(&ADDRESS, &twin_b, "addr", json!("twin@example.test")),
    ]);
    let index = AddressIndex::of(&addresses);
    assert_eq!(
        login_mailbox(&index, "Login@Example.test"),
        Some(keys::mailbox_pk(&moved).unwrap())
    );
    assert_eq!(
        login_mailbox(&index, "new@example.test"),
        Some(keys::mailbox_pk(&pk("new")).unwrap())
    );
    assert_eq!(login_mailbox(&index, "twin@example.test"), None);
    let gone = pk("gone");
    let renamed_away = AddressIndex::of(&rows(vec![put(
        &ADDRESS,
        &gone,
        "addr",
        json!("kept@example.test"),
    )]));
    assert_eq!(login_mailbox(&renamed_away, "gone@example.test"), None);
    assert_eq!(login_mailbox(&index, "not an address"), None);
}
