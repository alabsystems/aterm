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

/// Live accounts `claude ann`, `claude bob`, `grok ann` and `codex ann`, each born as `add
/// account` births one, over live address rows; `ann`'s Claude account labelled `desk` by the
/// owner where `desk`.
fn accounts(desk: bool) -> (Cells, [String; 4]) {
    let mut book = Book::new();
    let (ann, bob) = (pk("ann"), pk("bob"));
    let mut changes = row(&ann, &at("ann"));
    changes.extend(row(&bob, &at("bob")));
    let keys = [
        ("claude", "ann", &ann),
        ("claude", "bob", &bob),
        ("grok", "ann", &ann),
        ("codex", "ann", &ann),
    ]
    .map(|(realm, local, login)| {
        let key = keys::account_pk(realm, &at(local)).unwrap();
        changes.extend([
            put(&ACCOUNT, &key, "realm", json!(realm)),
            put(&ACCOUNT, &key, "login", json!(login)),
            put(&ACCOUNT, &key, "row", json!("live")),
        ]);
        key
    });
    if desk {
        changes.push(put(&ACCOUNT, &keys[0], "label", json!("desk")));
    }
    book.write(1, &[], changes);
    (book.cells(), keys)
}

/// An account's `realm` holds the realm's own word: a write of another spelling of it (`oai`,
/// `openai`, `Claude`, `xai`) is refused naming the word to write, though the key it derives is
/// the row's own, and so is a word no realm goes by, never quoting what was typed; a new grid row
/// is refused alike, before any key is made from it. The canonical word passes.
/// NEGATIVE CONTROL: with the `realm` arm of `guard`'s first pass removed, `oai` on the Codex
/// account passes, the key it derives being the row's: `` panicked … refused: oai ``.
#[test]
fn an_accounts_realm_holds_the_realms_own_word() {
    let (cells, [claude, _, grok, codex]) = accounts(false);
    let write = |realm: &str, key: &str| {
        guard(
            &mut [vec![put(&ACCOUNT, key, "realm", json!(realm))]],
            &cells,
        )
        .unwrap()
    };
    let refused = |realm: &str, key: &str| {
        let refusal = write(realm, key).unwrap_or_else(|| panic!("refused: {realm}"));
        assert_eq!(
            (refusal.pk.as_str(), refusal.column.as_str()),
            (key, "realm")
        );
        refusal.rule
    };
    for (word, key, canonical) in [
        ("oai", &codex, "codex"),
        ("openai", &codex, "codex"),
        ("Claude", &claude, "claude"),
        ("xai", &grok, "grok"),
    ] {
        let rule = refused(word, key);
        assert!(rule.contains(&format!("`{canonical}`")), "{word}: {rule}");
    }
    let unknown = refused("nowhere", &claude);
    assert!(
        unknown.contains("codex") && !unknown.contains("nowhere"),
        "{unknown}"
    );
    assert_eq!(write("codex", &codex), None);
    assert_eq!(write("claude", &claude), None);
    let born = |realm: &str| {
        birth(
            &ACCOUNT,
            &BTreeMap::from([
                ("realm".to_owned(), json!(realm)),
                ("login".to_owned(), json!(pk("bob"))),
            ]),
            &cells,
        )
    };
    let oai = born("oai").unwrap_err().to_string();
    assert!(oai.contains("`codex`"), "{oai}");
    assert_eq!(
        born("codex").unwrap(),
        keys::account_pk("codex", &at("bob")).unwrap()
    );
}

/// An owner's label names its account by the slug it makes, so a label is refused where it makes
/// none (an xAI handle of 20 bytes, an xAI key's shape; the label's type, a handle, admits no
/// other) and where its slug is one another account goes by now (its derived slug, the owner's
/// label of another account, its 16-hex slug) or another label of the same save makes. A label
/// whose slug is free, or is the account's own already, passes, as does an xAI handle of 19
/// bytes. The refusals name keys, never an address.
/// NEGATIVE CONTROLS: with `label_refusal` returning `None`, `ann-example` on bob's account
/// passes and moves ann's slug: `` held: derived ``; with `making` left out, two labels of one
/// slug in one save pass: `` held: one save ``.
#[test]
fn an_owners_label_never_takes_a_slug_another_account_goes_by() {
    let (cells, [ann, bob, grok, _]) = accounts(false);
    let label = |key: &str, text: &str| put(&ACCOUNT, key, "label", json!(text));
    let refused = |case: &str, cells: &Cells, rows: &mut [Vec<Change>]| {
        let refusal = guard(rows, cells).unwrap().expect(case);
        assert_eq!(refusal.column, "label", "{case}");
        let text = Error::from(refusal.clone()).to_string();
        assert!(!text.contains('@'), "{case}: {text}");
        refusal
    };
    let passes = |cells: &Cells, rows: &mut [Vec<Change>]| {
        assert_eq!(guard(rows, cells).unwrap(), None);
    };
    // Its derived slug: `claude-ann-example`.
    let derived = refused(
        "held: derived",
        &cells,
        &mut [vec![label(&bob, "ann-example")]],
    );
    assert_eq!(derived.pk, bob);
    assert!(
        derived.rule.contains(&ann) && derived.rule.contains("`claude-ann-example`"),
        "{derived:?}"
    );
    passes(&cells, &mut [vec![label(&bob, "bob")]]);
    passes(&cells, &mut [vec![label(&bob, "bob-example")]]);
    passes(&cells, &mut [vec![label(&grok, "ann-example")]]);
    let one_save = refused(
        "held: one save",
        &cells,
        &mut [vec![label(&ann, "desk")], vec![label(&bob, "desk")]],
    );
    assert!(one_save.rule.contains("`claude-desk`"), "{one_save:?}");
    // Another account's owner label, and its 16-hex slug.
    let (named, _) = accounts(true);
    let owner = refused("held: owner", &named, &mut [vec![label(&bob, "desk")]]);
    assert!(owner.rule.contains(&ann), "{owner:?}");
    passes(&named, &mut [vec![label(&ann, "desk")]]);
    let reserved = refused(
        "held: reserved",
        &cells,
        &mut [vec![label(&bob, &ann[3..])]],
    );
    assert!(reserved.rule.contains(&ann), "{reserved:?}");
    // No slug.
    let none = refused(
        "no slug",
        &cells,
        &mut [vec![label(&grok, "andrew-yates-research")]],
    );
    assert!(none.rule.contains("names no account"), "{none:?}");
    passes(&cells, &mut [vec![label(&grok, "yates-research-lab1")]]);
}

/// An owner's label never makes a slug spelled like a label a Mac's roster row of another account
/// carries, which the fleet fold resolves to that account: its legacy label (`kimi-m16`, the
/// `<cli>-<machine>` shape the importer kept) or the default an earlier build gave rows
/// (`claude-<6 hex>`). The label `m16` on another Kimi account is refused naming the account
/// whose row it is, as is the 6 hex of Ann's key on Bob's Claude account; either passes on the
/// account whose row it is, and on an account of another provider word.
/// NEGATIVE CONTROL: with `label_refusal` holding slugs alone (its legacy and earlier-label loop
/// removed), `m16` on the second Kimi account passes: `` panicked … held: legacy ``.
#[test]
fn an_owners_label_never_spells_another_accounts_row_label() {
    let mut book = Book::new();
    let (ann, bob) = (pk("ann"), pk("bob"));
    let mut changes = row(&ann, &at("ann"));
    changes.extend(row(&bob, &at("bob")));
    let keys = [
        ("kimi:global", "ann", &ann),
        ("kimi:global", "bob", &bob),
        ("claude", "ann", &ann),
        ("claude", "bob", &bob),
    ]
    .map(|(realm, local, login)| {
        let key = keys::account_pk(realm, &at(local)).unwrap();
        changes.extend([
            put(&ACCOUNT, &key, "realm", json!(realm)),
            put(&ACCOUNT, &key, "login", json!(login)),
            put(&ACCOUNT, &key, "row", json!("live")),
        ]);
        key
    });
    let [kimi_ann, kimi_bob, claude_ann, claude_bob] = &keys;
    changes.push(put(&ACCOUNT, kimi_ann, "legacy_label", json!("kimi-m16")));
    book.write(1, &[], changes);
    let cells = book.cells();
    let label = |key: &str, text: &str| put(&ACCOUNT, key, "label", json!(text));
    let refused = |case: &str, rows: &mut [Vec<Change>]| {
        let refusal = guard(rows, &cells).unwrap().expect(case);
        assert_eq!(refusal.column, "label", "{case}");
        let text = Error::from(refusal.clone()).to_string();
        assert!(!text.contains('@'), "{case}: {text}");
        refusal
    };
    let passes = |rows: &mut [Vec<Change>]| assert_eq!(guard(rows, &cells).unwrap(), None);
    let legacy = refused("held: legacy", &mut [vec![label(kimi_bob, "m16")]]);
    assert_eq!(legacy.pk, *kimi_bob);
    assert!(
        legacy.rule.contains(kimi_ann.as_str()) && legacy.rule.contains("`kimi-m16`"),
        "{legacy:?}"
    );
    passes(&mut [vec![label(kimi_ann, "m16")]]);
    passes(&mut [vec![label(claude_bob, "m16")]]);
    let earlier = keys::earlier_row_label("claude", claude_ann).unwrap();
    let hex = earlier.strip_prefix("claude-").unwrap();
    let held = refused("held: earlier", &mut [vec![label(claude_bob, hex)]]);
    assert!(held.rule.contains(claude_ann.as_str()), "{held:?}");
    passes(&mut [vec![label(claude_ann, hex)]]);
}

/// An account's provider is its realm's: a birth or a write whose `provider` is another
/// provider's key is refused on `provider`, naming the one to write, and the realm's own passes.
/// NEGATIVE CONTROL: with the provider check after `guard`'s keyed checks removed, `openai` on the
/// Claude account passes: `` panicked … refused: openai ``.
#[test]
fn an_accounts_provider_is_its_realms() {
    let (cells, [claude, _, grok, _]) = accounts(false);
    let write = |key: &str, provider: &str| {
        guard(
            &mut [vec![put(&ACCOUNT, key, "provider", json!(provider))]],
            &cells,
        )
        .unwrap()
    };
    for (key, provider, own) in [
        (&claude, "openai", "anthropic"),
        (&grok, "anthropic", "xai"),
    ] {
        let refusal = write(key, provider).unwrap_or_else(|| panic!("refused: {provider}"));
        assert_eq!(
            (refusal.pk.as_str(), refusal.column.as_str()),
            (key.as_str(), "provider")
        );
        assert!(refusal.rule.contains(&format!("`{own}`")), "{refusal:?}");
    }
    assert_eq!(write(&claude, "anthropic"), None);
    assert_eq!(write(&grok, "xai"), None);
}

/// An account's provider and realm are written once, at its birth (`typed::check::
/// fixed_after_birth`, which `att::Compiled::rules` runs before the table's own guard): a write
/// to either on a row already born is refused on that column, while a birth that writes them
/// with its lifecycle, a column that is not fixed, a write that restates the one value the cell
/// holds (what a revert of an inert write there restores) and a revert that restores a fixed
/// cell's absence pass; a row a revert returned to unborn may be born again.
/// NEGATIVE CONTROL: with `fixed_after_birth` reading no row as born (`born` false), the
/// provider written on the Claude account passes: `` panicked … refused: provider ``.
#[test]
fn an_accounts_provider_and_realm_are_written_once_at_its_birth() {
    let (cells, [claude, ..]) = accounts(false);
    let rules = |rows: &[Vec<Change>], cells: &Cells| {
        crate::typed::check::fixed_after_birth(&REGISTRY_3, rows, cells)
    };
    for (column, value) in [("provider", "anthropic"), ("realm", "codex")] {
        let refusal = rules(
            &[vec![put(&ACCOUNT, &claude, column, json!(value))]],
            &cells,
        )
        .unwrap_or_else(|| panic!("refused: {column}"));
        assert_eq!(
            (
                refusal.collection,
                refusal.pk.as_str(),
                refusal.column.as_str()
            ),
            (ACCOUNT.name, claude.as_str(), column)
        );
        assert!(refusal.rule.contains("written once"), "{refusal:?}");
    }
    assert_eq!(
        rules(
            &[vec![put(&ACCOUNT, &claude, "status", json!("paused"))]],
            &cells
        ),
        None
    );
    assert_eq!(
        rules(
            &[vec![put(&ACCOUNT, &claude, "realm", json!("claude"))]],
            &cells
        ),
        None,
        "a restatement"
    );
    let restored = Change {
        collection: ACCOUNT.name.to_owned(),
        key: format!("{claude}.provider"),
        value: None,
    };
    assert_eq!(rules(&[vec![restored]], &cells), None);
    let new = keys::account_pk("claude", &at("cy")).unwrap();
    let birth = vec![
        put(&ACCOUNT, &new, "provider", json!("anthropic")),
        put(&ACCOUNT, &new, "realm", json!("claude")),
        put(&ACCOUNT, &new, "login", json!(pk("cy"))),
        put(&ACCOUNT, &new, "row", json!("live")),
    ];
    assert_eq!(rules(std::slice::from_ref(&birth), &cells), None);
    // The row's lifecycle restored to its absence: unborn, and born again.
    let mut book = Book::new();
    let born = book.write(1, &[], birth.clone());
    book.write(
        1,
        &[born],
        vec![Change {
            collection: ACCOUNT.name.to_owned(),
            key: format!("{new}.row"),
            value: None,
        }],
    );
    assert_eq!(rules(&[birth], &book.cells()), None);
}

/// Preference version 2 reads under the authority rules (`typed::authority`), over two writers:
/// an automatic cell of a preference never projects, every column being the owner's (rule 2);
/// the owner's write to its fixed `machine` after its birth never projects (rule 4); and two
/// concurrent owner ranks, 3 from one Mac and 1 from the other, read as the smaller, tried first
/// (`OnConflict::Min`), the rank the earlier record wrote being 3. The read counts the two
/// records that hold an inert write.
/// NEGATIVE CONTROLS: with `PREFERENCE.authority` `None`, the automatic note shows: `` left:
/// Some(String("seen on m7")) right: None ``; with `rank`'s `.on_conflict(OnConflict::Min)`
/// removed, the rank reads as the first candidate's: `` left: Some(Number(Number(Unsigned(3))))
/// right: Some(Number(Number(Unsigned(1)))) ``.
#[test]
fn a_preference_reads_under_the_authority_rules_and_its_rank_settles_to_the_smaller() {
    use crate::typed::authority::tests::History;
    let machine = keys::machine_pk("m7");
    let account = keys::account_pk("claude", &at("ann")).unwrap();
    let pf = keys::preference_pk(&machine, &account).unwrap();
    let write = |col: &str, value: CellValue| {
        cell::change(
            &PREFERENCE,
            CellKey {
                pk: &pf,
                col,
                elem: None,
            },
            &value,
        )
        .unwrap()
    };
    let owner = |col: &str, x: Value| write(col, CellValue::owner(PREFERENCE.version, x));
    let mut history = History::new();
    let birth = history.write(
        0,
        &[],
        vec![
            owner("machine", json!(machine)),
            owner("account", json!(account)),
            owner("why", json!("owner")),
            owner("row", json!("live")),
        ],
    );
    history.write(1, &[birth], vec![owner("rank", json!(3))]);
    history.write(0, &[birth], vec![owner("rank", json!(1))]);
    let noted = history.write(
        1,
        &history.all(),
        vec![write(
            "note",
            CellValue::automatic(PREFERENCE.version, json!("seen on m7")),
        )],
    );
    let moved = history.write(
        0,
        &history.all(),
        vec![owner("machine", json!(keys::machine_pk("m8")))],
    );
    let (cells, inert) = history.read(&REGISTRY);
    let rows = cell::project(&PREFERENCE, &cells).rows;
    let row = &rows[&pf];
    assert_eq!(row.get("note"), None);
    assert_eq!(row.get("machine"), Some(&json!(machine)));
    assert!(row.fields["rank"].conflicted());
    assert_eq!(row.get("rank"), Some(&json!(1)));
    let at = |operation, col: &str| (operation, PREFERENCE.name.to_owned(), format!("{pf}.{col}"));
    assert_eq!(
        inert.changes,
        std::collections::BTreeSet::from([at(noted, "note"), at(moved, "machine")])
    );
    assert_eq!(inert.records.get(PREFERENCE.name), Some(&2));
}

/// An account reads as its provider word and its login's address, one read of the view naming
/// it; with no address it reads as the provider and its label; with no provider, as its address.
/// NEGATIVE CONTROL: with `Names::of` naming an account by its address alone (its first arm
/// `(Some(_), Some(address)) => address`), `` left: "ann@example.invalid" right: "openai ·
/// ann@example.invalid" ``.
#[test]
fn an_account_is_named_by_its_provider_and_address() {
    let mut book = Book::new();
    let ann = pk("ann");
    let both = keys::account_pk("codex", &at("ann")).unwrap();
    let bare = keys::account_pk("claude", &at("bare")).unwrap();
    let lone = keys::account_pk("claude", &at("lone")).unwrap();
    let mut changes = row(&ann, &at("ann"));
    changes.extend([
        put(&ACCOUNT, &both, "provider", json!("openai")),
        put(&ACCOUNT, &both, "realm", json!("codex")),
        put(&ACCOUNT, &both, "login", json!(ann)),
        put(&ACCOUNT, &bare, "provider", json!("anthropic")),
        put(&ACCOUNT, &bare, "realm", json!("claude")),
        put(&ACCOUNT, &bare, "label", json!("spare")),
        put(&ACCOUNT, &lone, "login", json!(ann)),
    ]);
    book.write(1, &[], changes);
    let view = View {
        cells: book.cells(),
        ..View::empty_for_test()
    };
    let names = Names::of(&REGISTRY, &view);
    assert_eq!(
        names.words("account", &both),
        Some("openai \u{b7} ann@example.invalid")
    );
    assert_eq!(
        names.words("account", &bare),
        Some("anthropic \u{b7} spare")
    );
    assert_eq!(names.words("account", &lone), Some("ann@example.invalid"));
    assert_eq!(names.words("account", "ac-none"), None);
    assert_eq!(
        names.words("phone", &both),
        None,
        "only machines and accounts"
    );
}
