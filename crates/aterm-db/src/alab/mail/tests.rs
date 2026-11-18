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
/// the address row, and its patterns, webmail and held credentials, never a value; each value is
/// read from the secret-store entry its `SecretRef` cell names. Each reveal journals first, and
/// the reveal journal is Unix's alone (`secret::Journal::record`; elsewhere every reveal is
/// refused), as is the lab view that asks for a sign-in.
#[cfg(unix)]
mod sign_in {
    use super::*;
    use crate::{
        alab::{registry::MAILBOX, secret::Journal},
        typed::{
            grid::tests::elem,
            secret::{Memory, Secret, reference},
        },
    };

    const TABLE: &str = "alab/registry.3";
    const LOGIN_PASS: &str = "fixture-login-pass";
    const SECOND_PASS: &str = "fixture-second-pass";
    const TOTP: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    /// Put `value` into `store` under the cell's own entry and return the reference its cell
    /// holds, as a landed write leaves it.
    fn stored(store: &Memory, collection: &str, row: &str, column: &str, value: &str) -> Value {
        use crate::typed::secret::Store as _;
        let entry = crate::typed::secret::entry_name(collection, column, row).unwrap();
        store.put(&entry, &Secret::new(value.to_owned())).unwrap();
        reference(Memory::VAULT, &entry)
    }

    struct Fixture {
        book: Book,
        store: Memory,
        login: String,
        mailbox: String,
    }

    /// Every value is synthetic: an `example.test` login and fixture credentials.
    fn fixture() -> Fixture {
        let login = pk("owner");
        let member = pk("ai1");
        let mailbox = keys::mailbox_pk(&login).unwrap();
        let store = Memory::default();
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
                    stored(&store, "address", &login, "password", LOGIN_PASS),
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
                    stored(&store, "mailbox", &mailbox, "mailbox_password", SECOND_PASS),
                ),
                put(
                    &MAILBOX,
                    &mailbox,
                    "totp",
                    stored(&store, "mailbox", &mailbox, "totp", TOTP),
                ),
                elem(&MAILBOX, &mailbox, "receives", &member),
                // The fourth generation's domains: one element each.
                elem(&MAILBOX, &mailbox, "receives_domains", "pm.example.test"),
                elem(&MAILBOX, &mailbox, "receives_domains", "*"),
            ],
        );
        Fixture {
            book,
            store,
            login,
            mailbox,
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
        store: &Memory,
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
                    TABLE,
                    store,
                    journal,
                    ask,
                    str::to_owned,
                )
                .unwrap()
            })
            .collect()
    }

    fn journal_lines(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("reveals.jsonl"))
            .map(|log| log.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// The mailbox reads its login, webmail, patterns and held credentials from its rows, and
    /// each credential reveals from the store entry its cell names, journalled once, with no
    /// value in the journal or the table.
    /// NEGATIVE CONTROL: with [`mailboxes`] reading the login from the MAILBOX row's own `label`
    /// instead of its `login` address row, the login reads `proton-1` and the assertion on
    /// `owner@example.test` fails; a value written into a cell instead of the store shows up in
    /// the table's cells and fails the last loop.
    #[test]
    fn a_sign_in_reads_the_login_and_its_credentials_from_registry_rows() {
        let Fixture {
            book,
            store,
            login,
            mailbox,
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

        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        assert_eq!(
            reveal_all(&addresses, &mailboxes_rows, &store, &journal, &mailbox),
            [
                Ok(LOGIN_PASS.to_owned()),
                Ok(SECOND_PASS.to_owned()),
                Ok(TOTP.to_owned()),
            ]
        );
        let log = journal_lines(dir.path());
        assert_eq!(log.len(), 3, "one record per reveal");
        assert!(log.iter().all(|line| line.contains("SignIn")));
        let log = log.concat();
        assert!(log.contains(&login) && log.contains(&mailbox) && log.contains(TABLE));
        let cells = format!("{:?}", book.cells());
        for value in store.every() {
            assert!(
                !log.contains(&value),
                "the journal holds coordinates, never a value"
            );
            assert!(
                !cells.contains(&value),
                "a cell names its entry, never holds the value"
            );
        }
    }

    /// Only a settled domain routes (LT-15B): a domain one writer added while another removed it
    /// is no route, and the mailbox's other domains still route.
    /// NEGATIVE CONTROL (run 2026-11-15): with [`mailboxes`] reading its domains through
    /// `cell::texts` (each element's first candidate), the disputed domain routes: `` left:
    /// ["ai1@example.test", "*@dispute.example.test", "*@pm.example.test", "*"] ``.
    #[test]
    fn a_disputed_domain_routes_nothing() {
        let Fixture {
            mut book, mailbox, ..
        } = fixture();
        let seen = book.all();
        book.write(
            2,
            &seen,
            vec![elem(
                &MAILBOX,
                &mailbox,
                "receives_domains",
                "dispute.example.test",
            )],
        );
        book.write(
            3,
            &seen,
            vec![
                cell::member_change(
                    &MAILBOX,
                    &mailbox,
                    "receives_domains",
                    "dispute.example.test",
                    false,
                    |x| cell::CellValue::owner(MAILBOX.version, x),
                )
                .unwrap(),
            ],
        );
        let (addresses, mailboxes_rows) = projections(&book);
        let found = mailboxes(&addresses, &mailboxes_rows);
        assert_eq!(
            found[0].receives,
            ["ai1@example.test", "*@pm.example.test", "*"]
        );
    }

    /// Two entries holding one value are one value: Macs that rolled one value from different
    /// candidates name different entries for it, so a sign-in reads every entry the candidates
    /// name before it withholds anything; one missing from the store withholds the cell.
    /// NEGATIVE CONTROL: with `open_credential` withholding any second reference unread (before
    /// the fix), `` left: Err(TwoValues) right: Ok("…") ``.
    #[test]
    fn two_entries_holding_one_value_are_one_value() {
        let Fixture {
            mut book,
            store,
            login,
            mailbox,
        } = fixture();
        let twin = crate::typed::secret::test_entry(("address", "password", &login), 3);
        store
            .put(&twin, &Secret::new(LOGIN_PASS.to_owned()))
            .unwrap();
        book.write(
            2,
            &[],
            vec![put(
                &ADDRESS,
                &login,
                "password",
                reference(Memory::VAULT, &twin),
            )],
        );
        let (addresses, mailboxes_rows) = projections(&book);
        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        let got = reveal_all(&addresses, &mailboxes_rows, &store, &journal, &mailbox);
        assert_eq!(got[0], Ok(LOGIN_PASS.to_owned()));
        store.remove(&twin).unwrap();
        let got = reveal_all(&addresses, &mailboxes_rows, &store, &journal, &mailbox);
        assert_eq!(got[0], Err(Withheld::NotInStore));
    }

    /// Two Macs writing the same reference are one value; references to two different values are
    /// withheld until the owner picks one, an empty cell says so, and a mailbox whose login row is retired
    /// is no sign-in's. Nothing withheld reaches the journal.
    /// NEGATIVE CONTROL: with the value comparison in `open_credential` removed (the first
    /// candidate always read), the two-values assertion reads `Ok(…)`.
    #[test]
    fn two_values_are_withheld_and_a_retired_login_is_no_mailbox() {
        let Fixture {
            mut book,
            store,
            login,
            mailbox,
        } = fixture();
        // Two other Macs write, each unaware of the first's write: concurrent candidates. One
        // writes the same login reference, the other a reference to a different entry.
        let same = reference(
            Memory::VAULT,
            &crate::typed::secret::entry_name("address", "password", &login).unwrap(),
        );
        book.write(2, &[], vec![put(&ADDRESS, &login, "password", same)]);
        let elsewhere =
            crate::typed::secret::test_entry(("mailbox", "mailbox_password", &mailbox), 2);
        store
            .put(&elsewhere, &Secret::new("fixture-other".to_owned()))
            .unwrap();
        book.write(
            3,
            &[],
            vec![put(
                &MAILBOX,
                &mailbox,
                "mailbox_password",
                reference(Memory::VAULT, &elsewhere),
            )],
        );
        let (addresses, mailboxes_rows) = projections(&book);
        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        let got = reveal_all(&addresses, &mailboxes_rows, &store, &journal, &mailbox);
        assert_eq!(
            got[0],
            Ok(LOGIN_PASS.to_owned()),
            "one reference is one value"
        );
        assert_eq!(got[1], Err(Withheld::TwoValues));
        assert_eq!(got[2], Ok(TOTP.to_owned()));
        assert_eq!(
            journal_lines(dir.path()).len(),
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
            reveal_all(&addresses, &mailboxes_rows, &store, &journal, &mailbox)[0],
            Err(Withheld::NotSet)
        );
        empty.write(1, &[], vec![put(&ADDRESS, &login, "row", json!("retired"))]);
        let (addresses, mailboxes_rows) = projections(&empty);
        assert!(mailboxes(&addresses, &mailboxes_rows).is_empty());
        assert_eq!(
            reveal_all(&addresses, &mailboxes_rows, &store, &journal, &mailbox)[0],
            Err(Withheld::NoMailbox)
        );
        assert_eq!(journal_lines(dir.path()).len(), 2);
    }

    /// A candidate naming an entry that is not its cell's own (another credential in the same
    /// vault, which a writer that skipped the write check stored) or its own entry in another
    /// vault is withheld as `NotOwnEntry`: the store is never asked for it, nothing is handed on
    /// and nothing is journalled.
    /// NEGATIVE CONTROL: the store holds the other credential, so with `open_credential` reading
    /// `entry_of` unchecked the sign-in receives it: `` the sign-in read another entry ``.
    #[test]
    fn a_reference_to_an_entry_the_cell_does_not_own_is_never_read() {
        let Fixture {
            mut book,
            store,
            login,
            mailbox,
        } = fixture();
        store
            .put(
                "gh_access_token",
                &Secret::new("fixture-api-key".to_owned()),
            )
            .unwrap();
        let own = crate::typed::secret::entry_name("address", "password", &login).unwrap();
        let stray = |x: Value| crate::database::Change {
            collection: ADDRESS.name.to_owned(),
            key: format!("{login}.password"),
            value: Some(cell::CellValue::owner(ADDRESS.version, x).encode()),
        };
        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        for (n, x) in [
            reference(Memory::VAULT, "gh_access_token"),
            reference("other-vault", &own),
        ]
        .into_iter()
        .enumerate()
        {
            book.write(1, &[], vec![stray(x)]);
            let (addresses, mailboxes_rows) = projections(&book);
            let got = reveal_all(&addresses, &mailboxes_rows, &store, &journal, &mailbox);
            assert_ne!(
                got[0],
                Ok("fixture-api-key".to_owned()),
                "the sign-in read another entry"
            );
            assert_eq!(got[0], Err(Withheld::NotOwnEntry), "case {n}");
        }
        assert_eq!(
            journal_lines(dir.path()).len(),
            4,
            "only the two cells that name their own entries were journalled, twice each"
        );
    }

    /// Whether a sign-in could be handed a credential is answered as a reveal would answer,
    /// with nothing handed on: a held value is usable, one of whitespace alone is blank, an
    /// empty one empty, and a reference the store lacks is withheld in the reveal's own words.
    /// `usable_in` is handed no journal, so it can record nothing.
    /// NEGATIVE CONTROL: with `usable_in` answering from the mailbox's `held` list alone, the
    /// store-less, the blank and the empty cell all read usable.
    #[test]
    fn whether_a_credential_is_usable_is_answered_with_no_reveal() {
        let Fixture {
            book,
            store,
            mailbox,
            ..
        } = fixture();
        let (addresses, mailboxes_rows) = projections(&book);
        let ask = |which| Ask {
            mailbox: &mailbox,
            which,
            when_ms: 1,
        };
        let usable =
            |which| usable_in(&addresses, &mailboxes_rows, TABLE, &store, ask(which)).unwrap();
        for which in Credential::ALL {
            assert_eq!(usable(which), Ok(Standing::Usable), "{which:?}");
        }
        assert_eq!(
            usable_in(
                &addresses,
                &mailboxes_rows,
                TABLE,
                &store,
                Ask {
                    mailbox: "mb-none",
                    which: Credential::Password,
                    when_ms: 1
                }
            )
            .unwrap(),
            Err(Withheld::NoMailbox)
        );
        let entry = crate::typed::secret::entry_name("mailbox", "totp", &mailbox).unwrap();
        store.put(&entry, &Secret::new(" \t\n".to_owned())).unwrap();
        assert_eq!(usable(Credential::Totp), Ok(Standing::Blank));
        store.put(&entry, &Secret::new(String::new())).unwrap();
        assert_eq!(usable(Credential::Totp), Ok(Standing::Empty));
        store.remove(&entry).unwrap();
        assert_eq!(usable(Credential::Totp), Err(Withheld::NotInStore));
        // Still listed as held: the list says a cell names an entry, not that it opens here.
        assert!(
            mailboxes(&addresses, &mailboxes_rows)[0]
                .held
                .contains(&Credential::Totp)
        );
    }

    /// The holder of a revealed bundle learns, with nothing revealed, whether the registry still
    /// holds the values it was handed, whichever entries hold them: yes at a second look, at
    /// another cell's write and where a cell comes to name a second entry of the same value; no
    /// when a value is replaced, under a new reference or in place under the same one, for
    /// values in another order and for another count of them; and withheld in a reveal's own
    /// words where a reveal would be (a binding not accepted, candidates that disagree, an entry
    /// the store lacks).
    /// NEGATIVE CONTROL: a compare of the cells' references (the recheck's first form) answers
    /// yes after the in-place replacement.
    #[test]
    fn a_held_bundle_is_the_registrys_only_while_its_values_are() {
        let Fixture {
            mut book,
            store,
            login,
            mailbox,
        } = fixture();
        let bundle = [Credential::Password, Credential::Totp];
        let same_under =
            |table: &str, book: &Book, parts: &[Credential], held: &[&str], accept: bool| {
                let (addresses, rows) = projections(book);
                let held: Vec<&[u8]> = held.iter().map(|value| value.as_bytes()).collect();
                bound_same_in(
                    &addresses,
                    &rows,
                    table,
                    &store,
                    &mailbox,
                    parts,
                    &held,
                    |_| accept,
                )
                .unwrap()
            };
        let same = |book: &Book, parts: &[Credential], held: &[&str], accept: bool| {
            same_under(TABLE, book, parts, held, accept)
        };
        let first = [LOGIN_PASS, TOTP];
        assert_eq!(same(&book, &bundle, &first, true), Ok(true));
        assert_eq!(
            same_under("alab/registry.next", &book, &bundle, &first, true),
            Ok(true),
            "another generation's name over the same values is the same bundle"
        );
        assert_eq!(
            same(&book, &bundle, &first, true),
            Ok(true),
            "a second look"
        );
        assert_eq!(same(&book, &bundle, &[TOTP, LOGIN_PASS], true), Ok(false));
        assert_eq!(same(&book, &bundle, &[LOGIN_PASS], true), Ok(false));
        assert_eq!(
            same(&book, &bundle, &[LOGIN_PASS, TOTP, TOTP], true),
            Ok(false)
        );
        assert_eq!(
            same(&book, &[Credential::Password], &[LOGIN_PASS], true),
            Ok(true)
        );
        assert_eq!(
            same(&book, &bundle, &first, false),
            Err(Withheld::NoMailbox)
        );

        // A cell outside the bundle names another candidate: the bundle is still held.
        let elsewhere =
            crate::typed::secret::test_entry(("mailbox", "mailbox_password", &mailbox), 2);
        store
            .put(&elsewhere, &Secret::new(SECOND_PASS.to_owned()))
            .unwrap();
        book.write(
            2,
            &[],
            vec![put(
                &MAILBOX,
                &mailbox,
                "mailbox_password",
                reference(Memory::VAULT, &elsewhere),
            )],
        );
        assert_eq!(same(&book, &bundle, &first, true), Ok(true));

        // The password's cell names a second entry holding the same value: the same values.
        let twin = crate::typed::secret::test_entry(("address", "password", &login), 3);
        store
            .put(&twin, &Secret::new(LOGIN_PASS.to_owned()))
            .unwrap();
        book.write(
            3,
            &[],
            vec![put(
                &ADDRESS,
                &login,
                "password",
                reference(Memory::VAULT, &twin),
            )],
        );
        assert_eq!(same(&book, &bundle, &first, true), Ok(true));

        // One entry's value is replaced in place, as a by-name vault write does: no cell and
        // no reference changed, the candidates disagree and nothing is answered.
        store
            .put(&twin, &Secret::new(SECOND_PASS.to_owned()))
            .unwrap();
        assert_eq!(same(&book, &bundle, &first, true), Err(Withheld::TwoValues));
        // Both hold the new value: still no cell or reference changed, and the held bundle is
        // no longer the registry's.
        let own = crate::typed::secret::entry_name("address", "password", &login).unwrap();
        store
            .put(&own, &Secret::new(SECOND_PASS.to_owned()))
            .unwrap();
        assert_eq!(same(&book, &bundle, &first, true), Ok(false));
        assert_eq!(same(&book, &bundle, &[SECOND_PASS, TOTP], true), Ok(true));

        store.remove(&twin).unwrap();
        assert_eq!(
            same(&book, &bundle, &[SECOND_PASS, TOTP], true),
            Err(Withheld::NotInStore)
        );
    }

    /// A reference whose entry this Mac's store lacks is withheld as `NotInStore`: no value is
    /// handed on and nothing is journalled.
    /// NEGATIVE CONTROL: with `open_credential` journalling or consuming before the store read
    /// answers, `consumed` is set or the journal holds a line.
    #[test]
    fn a_reference_the_store_lacks_reveals_nothing_and_journals_nothing() {
        let Fixture {
            book,
            store,
            mailbox,
            ..
        } = fixture();
        let (addresses, mailboxes_rows) = projections(&book);
        let entry = crate::typed::secret::entry_name("mailbox", "totp", &mailbox).unwrap();
        store.remove(&entry).unwrap();
        let dir = aterm_tempfile::tempdir().unwrap();
        let journal = Journal::at(dir.path());
        let mut consumed = false;
        let ask = Ask {
            mailbox: &mailbox,
            which: Credential::Totp,
            when_ms: 1,
        };
        assert_eq!(
            reveal_in(
                &addresses,
                &mailboxes_rows,
                TABLE,
                &store,
                &journal,
                ask,
                |_| consumed = true,
            )
            .unwrap(),
            Err(Withheld::NotInStore)
        );
        assert!(!consumed, "nothing reached the sign-in");
        assert!(
            journal_lines(dir.path()).is_empty(),
            "nothing was journalled"
        );
        // The cell still names its entry: the mailbox lists the credential as held.
        assert!(
            mailboxes(&addresses, &mailboxes_rows)[0]
                .held
                .contains(&Credential::Totp)
        );
        // The same store read with the entry present reveals it.
        store.put(&entry, &Secret::new(TOTP.to_owned())).unwrap();
        assert_eq!(
            reveal_in(
                &addresses,
                &mailboxes_rows,
                TABLE,
                &store,
                &journal,
                ask,
                str::to_owned
            )
            .unwrap(),
            Ok(TOTP.to_owned())
        );
        assert_eq!(journal_lines(dir.path()).len(), 1);
    }

    /// Below the third generation a registry's secret cells hold sealed tokens, as builds up to
    /// 0.199.0 write them, and a sign-in on this build opens each under its generation's key, so a
    /// mixed fleet keeps signing in until the roll.
    /// The listing a sign-in reads first (`mailboxes`) names each sealed credential as held.
    /// NEGATIVE CONTROLS: another table's key opens nothing (`Unopened`), the store path reads
    /// no token (`NotOwnEntry`), and `holds_tokens` sends only a generation before the third to
    /// the opener; with `holds_credential` counting only references (before the fix), the
    /// listing holds none: `` left: [] right: [Password, Totp] ``.
    #[test]
    fn a_sign_in_below_the_third_generation_opens_its_sealed_token() {
        use crate::{
            alab::registry::{REGISTRY_2, REGISTRY_3, SEALED_ADDRESS, SEALED_MAILBOX},
            typed::secret::{Fixed, Sealer as _},
        };
        assert!(holds_tokens(&REGISTRY_2));
        assert!(!holds_tokens(&REGISTRY_3));
        let login = pk("owner");
        let mailbox = keys::mailbox_pk(&login).unwrap();
        let key = Fixed("alab/registry.2");
        let seal = |collection: &str, row: &str, column: &str, value: &str| {
            json!(
                key.seal(collection, row, column, &Secret::new(value.to_owned()))
                    .unwrap()
            )
        };
        let mut book = Book::new();
        book.write(
            1,
            &[],
            vec![
                put(&SEALED_ADDRESS, &login, "addr", json!("owner@example.test")),
                put(
                    &SEALED_ADDRESS,
                    &login,
                    "password",
                    seal("address", &login, "password", LOGIN_PASS),
                ),
                put(&SEALED_MAILBOX, &mailbox, "label", json!("proton-1")),
                put(&SEALED_MAILBOX, &mailbox, "service", json!("proton")),
                put(&SEALED_MAILBOX, &mailbox, "code_path", json!("webmail")),
                put(&SEALED_MAILBOX, &mailbox, "login", json!(login)),
                put(
                    &SEALED_MAILBOX,
                    &mailbox,
                    "totp",
                    seal("mailbox", &mailbox, "totp", TOTP),
                ),
            ],
        );
        let cells = book.cells();
        let addresses = cell::project(&SEALED_ADDRESS, &cells);
        let mailboxes_rows = cell::project(&SEALED_MAILBOX, &cells);
        let read = |values: Values<'_>, which: Credential| {
            open_credential(
                &addresses,
                &mailboxes_rows,
                "alab/registry.2",
                values,
                Ask {
                    mailbox: &mailbox,
                    which,
                    when_ms: 1,
                },
            )
            .unwrap()
            .map(|(_, held)| held.plaintext().to_owned())
        };
        assert_eq!(
            read(Values::Sealed(&key), Credential::Password),
            Ok(LOGIN_PASS.to_owned())
        );
        assert_eq!(
            read(Values::Sealed(&key), Credential::Totp),
            Ok(TOTP.to_owned())
        );
        assert_eq!(
            read(Values::Sealed(&key), Credential::MailboxPassword),
            Err(Withheld::NotSet)
        );
        assert_eq!(
            read(
                Values::Sealed(&Fixed("alab/registry")),
                Credential::Password
            ),
            Err(Withheld::Unopened)
        );
        let store = Memory::default();
        assert_eq!(
            read(Values::Store(&store), Credential::Password),
            Err(Withheld::NotOwnEntry)
        );
        // A bundle revealed under a sealed generation is rechecked there by its values alone.
        let held = |password: &str| {
            same_of(
                &addresses,
                &mailboxes_rows,
                "alab/registry.2",
                Values::Sealed(&key),
                &mailbox,
                &[Credential::Password, Credential::Totp],
                &[password.as_bytes(), TOTP.as_bytes()],
                |_| true,
            )
            .unwrap()
        };
        assert_eq!(held(LOGIN_PASS), Ok(true));
        assert_eq!(held(SECOND_PASS), Ok(false));
        // The listing a sign-in reads first names the sealed credentials it can open, so the
        // harness asks for them.
        let listed = mailboxes(&addresses, &mailboxes_rows);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].held, [Credential::Password, Credential::Totp]);
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
