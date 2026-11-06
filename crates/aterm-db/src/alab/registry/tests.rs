// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The address rules over hand-built registry histories (`typed::grid::tests::Book`), no store:
//! which row an address names ([`AddressIndex`]), the owner's address writes ([`guard`]) and a
//! new grid row's key ([`birth`]). Every address is synthetic (`example.invalid`).

use super::*;
use crate::typed::grid::tests::{Book, put};
use aterm_json::json;

fn at(local: &str) -> String {
    format!("{local}@example.invalid")
}

fn pk(local: &str) -> String {
    keys::address_pk(&at(local)).unwrap()
}

/// `local`'s row, born saying `says`, live.
fn row(key: &str, says: &str) -> Vec<Change> {
    vec![
        put(&ADDRESS, key, "addr", json!(says)),
        put(&ADDRESS, key, "domain", json!("example.invalid")),
        put(&ADDRESS, key, "row", json!("live")),
    ]
}

/// The rows: `ann` renamed to `ann2`; `bob`; `cy` renamed onto `both`, which `both` says too;
/// `dup` and the retired `old` both saying `dup`; the retired `gone`; a key-only `bare` row
/// with no `addr`; and `dan`, whose `addr` two writers set concurrently to `dan` and `dan2`.
fn fixture() -> Cells {
    let mut book = Book::new();
    let mut changes = Vec::new();
    for (key, says) in [
        (pk("ann"), at("ann2")),
        (pk("bob"), at("bob")),
        (pk("cy"), at("both")),
        (pk("both"), at("both")),
        (pk("dup"), at("dup")),
        (pk("old"), at("dup")),
        (pk("gone"), at("gone")),
    ] {
        changes.extend(row(&key, &says));
    }
    changes.push(put(&ADDRESS, &pk("old"), "row", json!("retired")));
    changes.push(put(&ADDRESS, &pk("gone"), "row", json!("retired")));
    changes.push(put(&ADDRESS, &pk("bare"), "row", json!("retired")));
    let first = book.write(1, &[], changes);
    book.write(
        1,
        &[first],
        vec![put(&ADDRESS, &pk("dan"), "addr", json!(at("dan")))],
    );
    book.write(
        2,
        &[first],
        vec![put(&ADDRESS, &pk("dan"), "addr", json!(at("dan2")))],
    );
    book.cells()
}

/// An address names the row whose `addr` says it now, never the key it derives: a renamed row by
/// its new address (in any spelling) and not its old one, whose key it still holds; the one live
/// row before a retired one, a lone retired row, and several rows that say it as `Several`; a
/// conflicted `addr` under each of its values; and an address no row says as `Free`, `taken`
/// where a row holds its key anyway. Only the import's match takes the derived key first.
/// NEGATIVE CONTROL: with `holder` answering by the derived key first (`Row` wherever `keys`
/// holds `address_pk(raw)`), the renamed row's old address still names it:
/// `` left: Row("ad-…") right: Free { derived: "ad-…", taken: true } ``.
#[test]
fn an_address_is_held_by_the_row_whose_addr_says_it_now() {
    let addresses = cell::project(&ADDRESS, &fixture());
    let index = AddressIndex::of(&addresses);
    let ann = pk("ann");
    assert_eq!(index.holder(&at("ann2")).unwrap(), AddressHolder::Row(&ann));
    assert_eq!(
        index.holder("  ANN2@Example.INVALID ").unwrap(),
        AddressHolder::Row(&ann)
    );
    assert_eq!(
        index.holder(&at("ann")).unwrap(),
        AddressHolder::Free {
            derived: ann.clone(),
            taken: true
        }
    );
    assert_eq!(index.row(&at("ann")), None);
    let (both, cy) = (pk("both"), pk("cy"));
    let mut two = vec![both.as_str(), cy.as_str()];
    two.sort_unstable();
    assert_eq!(
        index.holder(&at("both")).unwrap(),
        AddressHolder::Several(two.clone())
    );
    assert_eq!(index.row(&at("both")), None);
    assert_eq!(index.saying(&at("both")), two);
    assert_eq!(index.row(&at("dup")), Some(pk("dup").as_str()));
    assert_eq!(index.saying(&at("dup")), [pk("dup"), pk("old")]);
    assert_eq!(index.row(&at("gone")), Some(pk("gone").as_str()));
    assert_eq!(index.row(&at("dan")), Some(pk("dan").as_str()));
    assert_eq!(index.row(&at("dan2")), Some(pk("dan").as_str()));
    assert_eq!(
        index.holder(&at("bare")).unwrap(),
        AddressHolder::Free {
            derived: pk("bare"),
            taken: true
        }
    );
    assert_eq!(
        index.holder(&at("nobody")).unwrap(),
        AddressHolder::Free {
            derived: pk("nobody"),
            taken: false
        }
    );
    assert!(index.holder("not an address").is_err());
    // The import's match: the derived key's row first, whatever it says, then the address.
    assert_eq!(index.matched(&at("ann")).unwrap(), AddressHolder::Row(&ann));
    assert_eq!(
        index.matched(&at("ann2")).unwrap(),
        AddressHolder::Row(&ann)
    );
    assert_eq!(
        index.matched(&at("both")).unwrap(),
        AddressHolder::Row(&both)
    );
}

/// A new grid row is born under its address's key only while no row says that address and no
/// row holds that key: a renamed row's new address and its old one, an address two rows say, and
/// a retired row's address are refused at the key, each refusal free of the address.
/// NEGATIVE CONTROL: with `birth`'s address arm keyed by `keys::address_pk` alone (as before),
/// a row for the renamed row's new address is born beside it:
/// `` assertion failed: born(&at("ann2")).is_err() ``.
#[test]
fn a_grid_row_is_born_only_for_an_address_no_row_says() {
    let cells = fixture();
    let born = |addr: &str| {
        birth(
            &ADDRESS,
            &BTreeMap::from([("addr".to_owned(), json!(addr))]),
            &cells,
        )
    };
    assert!(born(&at("ann2")).is_err());
    assert!(born(&at("ann")).is_err());
    assert!(born(&at("both")).is_err());
    assert!(born(&at("gone")).is_err());
    for refused in [at("ann2"), at("ann"), at("both"), at("gone")] {
        let text = born(&refused).unwrap_err().to_string();
        assert!(!text.contains('@'), "{text}");
    }
    assert_eq!(born(&at("nobody")).unwrap(), pk("nobody"));
}

/// One record: the owner's write of row `key`'s `addr`.
fn rename(key: &str, to: &str) -> Vec<Change> {
    vec![put(&ADDRESS, key, "addr", json!(to))]
}

/// The owner's writes keep one row per address on every path that writes `addr`: a rename to an
/// address another row says (now, or once the same save lands) is refused, so is one to an
/// address whose birth key a row renamed away holds, and one of a row a live account signs in
/// with; a rename back to the row's own birth address passes, and so does a rename onto an
/// address another row moves off in the same save; a passing rename writes the row's `domain` in
/// its record (replacing a different one the save typed); a `domain` that disagrees with the
/// row's `addr` is refused. Every refusal names keys and columns, never an address.
/// NEGATIVE CONTROLS: with the account rule removed (`if false && renames`), renaming the
/// account's login passes: `` the account's login ``; with the save's own address writes left
/// out of the landed table (`landed`'s filter `&& c.collection != ADDRESS.name`), two rows
/// renamed onto one address pass: `` two rows onto one address ``; with `landed` keeping a written
/// cell's earlier candidates beside the write (`entry(…).or_default().candidates.push(…)`), a row
/// moving off an address still holds it: `` assertion `left == right` failed left:
/// Some(Refused { collection: "address", pk: "ad-…", column: "addr", rule: "email row ad-…
/// holds that address; name that row, or rename it first" }) right: None ``.
#[test]
fn the_owner_s_address_writes_keep_one_row_per_address() {
    let mut book = Book::new();
    let (ann, bob, cy, moved) = (pk("ann"), pk("bob"), pk("cy"), pk("moved"));
    let account = keys::account_pk("claude", &at("bob")).unwrap();
    let mut changes = Vec::new();
    for (key, says) in [
        (&ann, at("ann")),
        (&bob, at("bob")),
        (&cy, at("cy")),
        (&moved, at("moved2")),
    ] {
        changes.extend(row(key, &says));
    }
    changes.extend([
        put(&ACCOUNT, &account, "realm", json!("claude")),
        put(&ACCOUNT, &account, "login", json!(bob)),
        put(&ACCOUNT, &account, "row", json!("live")),
    ]);
    book.write(1, &[], changes);
    let cells = book.cells();
    let refused = |case: &str, rows: &mut [Vec<Change>]| {
        let text = Error::from(guard(rows, &cells).unwrap().expect(case)).to_string();
        assert!(!text.contains('@'), "{text}");
        text
    };
    let passes = |rows: &mut [Vec<Change>]| assert_eq!(guard(rows, &cells).unwrap(), None);
    // Another row says it, or will once this save lands.
    assert!(refused("onto a held address", &mut [rename(&ann, &at("bob"))]).contains(&bob));
    let onto_one = &mut [rename(&ann, &at("new")), rename(&cy, &at("new"))];
    assert!(refused("two rows onto one address", onto_one).contains("holds"));
    // Its birth key is a row that says another address now; that row may go back to it.
    assert!(
        refused(
            "onto a moved row's birth",
            &mut [rename(&ann, &at("moved"))]
        )
        .contains(&moved)
    );
    passes(&mut [rename(&moved, &at("moved"))]);
    // A row moving off an address in the same save frees it for another.
    passes(&mut [rename(&moved, &at("moved")), rename(&ann, &at("moved2"))]);
    // A swap through a save: each row moving off frees its address, the key rule still holds.
    let swap = &mut [rename(&bob, &at("cy")), rename(&cy, &at("bob"))];
    assert!(refused("a swap", swap).contains("birth row"));
    // A live account signs in with the row.
    assert!(refused("the account's login", &mut [rename(&bob, &at("bob2"))]).contains(&account));
    // A rename that passes carries the row's domain, in its own record.
    let mut renamed = [rename(&ann, "ann3@example.test")];
    passes(&mut renamed);
    let domain = renamed[0]
        .iter()
        .find(|c| c.key == format!("{ann}.domain"))
        .and_then(|c| c.value.as_ref())
        .map(|v| CellValue::decode(v).unwrap().x);
    assert_eq!(domain, Some(json!("example.test")));
    let mut typed = [vec![
        put(&ADDRESS, &ann, "addr", json!("ann3@example.test")),
        put(&ADDRESS, &ann, "domain", json!("elsewhere.test")),
    ]];
    passes(&mut typed);
    assert_eq!(typed[0].len(), 2);
    assert_eq!(
        CellValue::decode(typed[0][1].value.as_ref().unwrap())
            .unwrap()
            .x,
        json!("example.test")
    );
    // The same domain asks for no second cell.
    let mut same = [rename(&ann, &at("ann3"))];
    passes(&mut same);
    assert_eq!(same[0].len(), 1);
    // A domain on its own follows the row's addr.
    assert!(
        refused(
            "a domain apart from addr",
            &mut [vec![put(&ADDRESS, &ann, "domain", json!("other.test"))]]
        )
        .contains("domain")
    );
    passes(&mut [vec![put(
        &ADDRESS,
        &ann,
        "domain",
        json!("example.invalid"),
    )]]);
    // Other collections and columns pass untouched.
    let mut other = [vec![put(&ADDRESS, &bob, "status", json!("disabled"))]];
    passes(&mut other);
    assert_eq!(other[0].len(), 1);
}

/// An account write that leaves it live passes only under the key its realm and the address it
/// signs in as derive, beside no other live account of the realm that signs in as that address:
/// bringing back an account retired for a rename of its login is refused, and so is a new
/// account beside a live one a rename went past; another realm's account of that row, an
/// address born with its account in one save, and a write of a column no key reads pass. The
/// refusals name keys and columns, never an address.
/// NEGATIVE CONTROLS: with the key check removed (`if false && derived != *pk`), the retired
/// account comes back live: `` back to live ``; with the second-account check removed (`false &&
/// *other != pk`), the account beside the stale one passes: `` beside ``.
#[test]
fn an_account_is_live_only_under_the_key_its_rows_derive() {
    let mut book = Book::new();
    let (bob, cy, fresh) = (pk("bob"), pk("cy"), pk("fresh"));
    let retired = keys::account_pk("claude", &at("bob")).unwrap();
    let stale = keys::account_pk("claude", &at("cy")).unwrap();
    let account = |key: &str, realm: &str, login: &str, lifecycle: &str| {
        vec![
            put(&ACCOUNT, key, "realm", json!(realm)),
            put(&ACCOUNT, key, "login", json!(login)),
            put(&ACCOUNT, key, "row", json!(lifecycle)),
        ]
    };
    // `bob` renamed once its account was retired; `cy` renamed past its live account.
    let mut changes = row(&bob, &at("bob2"));
    changes.extend(row(&cy, &at("cy2")));
    changes.extend(account(&retired, "claude", &bob, "retired"));
    changes.extend(account(&stale, "claude", &cy, "live"));
    book.write(1, &[], changes);
    let cells = book.cells();
    let refused = |case: &str, rows: &mut [Vec<Change>]| {
        let refusal = guard(rows, &cells).unwrap().expect(case);
        let text = Error::from(refusal.clone()).to_string();
        assert!(!text.contains('@'), "{text}");
        refusal
    };
    let passes = |rows: &mut [Vec<Change>]| assert_eq!(guard(rows, &cells).unwrap(), None);
    let back = refused(
        "back to live",
        &mut [vec![put(&ACCOUNT, &retired, "row", json!("live"))]],
    );
    assert_eq!(
        (back.pk.as_str(), back.column.as_str()),
        (retired.as_str(), "row")
    );
    let beside = keys::account_pk("claude", &at("cy2")).unwrap();
    let second = refused("beside", &mut [account(&beside, "claude", &cy, "live")]);
    assert_eq!(second.pk, beside);
    assert!(second.rule.contains(&stale), "{second:?}");
    let codex = keys::account_pk("codex", &at("cy2")).unwrap();
    passes(&mut [account(&codex, "codex", &cy, "live")]);
    let mut together = row(&fresh, &at("fresh"));
    together.extend(account(
        &keys::account_pk("claude", &at("fresh")).unwrap(),
        "claude",
        &fresh,
        "live",
    ));
    passes(&mut [together]);
    passes(&mut [vec![put(&ACCOUNT, &stale, "status", json!("paused"))]]);
}
