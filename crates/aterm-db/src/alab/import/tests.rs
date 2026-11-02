// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab importer's plans over hand-built registry histories (`typed::grid::tests::Book`), no
//! store. The sheets are synthetic fixtures with the real layouts (the vault sheet's `m, email,
//! computer, <blank>, claude, Open AI, google, meta, kimi, x, notes, <blank>, <blank>` and the
//! logins sheet's six columns); every address is `example.invalid` or `proton.example`. The
//! vendor reading here is a stand-in with the harness's documented rules; the harness's own
//! (`accounts_sync::read_sheet_cell`) is tested against its pool parser in `aterm-agent`.

use super::*;
use crate::{
    alab::registry::{ACCOUNT, ADDRESS, MACHINE, SEAT},
    typed::grid::tests::{Book, elem, put},
};

/// A stand-in close to the harness's reading: `1` is a member, Kimi Code's bare `1` is held and
/// its region words name a region, Codex's and Grok Build's flow words name a flow, and a note
/// with `banned` pauses the vendor it names, or every vendor when it names none (the harness's
/// Claude Code column pauses on any such note; this one scopes every column alike).
pub(crate) struct Stand;

impl Vendors for Stand {
    fn mark(&self, provider: &str, cell: &str, notes: &str) -> Mark {
        let cell = cell.trim();
        let lower = notes.to_ascii_lowercase();
        let names = |p: &str| match p {
            keys::ANTHROPIC => lower.contains("claude"),
            keys::OPENAI => lower.contains("codex"),
            keys::KIMI => lower.contains("kimi"),
            keys::GOOGLE => lower.contains("google"),
            _ => false,
        };
        let any_named = keys::PROVIDERS.iter().any(|p| names(p));
        // As `read_sheet_cell`: any disqualifying note pauses Claude Code's column.
        let paused = lower.contains("banned")
            && (provider == keys::ANTHROPIC || !any_named || names(provider));
        let terms = match (provider, cell) {
            (_, "" | "0") => return Mark::Absent,
            (keys::KIMI, "1") => return Mark::Held,
            (keys::KIMI, "global") => Terms {
                region: Some("global"),
                flow: None,
            },
            (keys::KIMI, "cn") => Terms {
                region: Some("mainland-cn"),
                flow: None,
            },
            (keys::OPENAI | keys::XAI, "browser") => Terms {
                region: None,
                flow: Some("browser"),
            },
            (keys::OPENAI | keys::XAI, "device") => Terms {
                region: None,
                flow: Some("device"),
            },
            (keys::KIMI, _) => return Mark::Unread,
            (_, "1") => Terms::default(),
            _ => return Mark::Unread,
        };
        if paused {
            Mark::Paused(terms)
        } else {
            Mark::Member(terms)
        }
    }
}

pub(crate) const VAULT_HEADER: &str = "m,email,computer,,claude,Open AI,google,meta,kimi,x,notes,,";
pub(crate) const LOGINS_HEADER: &str = "computer,AI login email,services,codes arrive in (my guess),why I guessed,your correction (leave blank if my guess is right)";

pub(crate) fn vault(rows: &[&str]) -> Sheet {
    let text = format!("{VAULT_HEADER}\n{}\n", rows.join("\n"));
    Sheet::parse(SheetKind::Vault, "ai-accounts.csv", text.as_bytes()).unwrap()
}

pub(crate) fn logins(rows: &[&str]) -> Sheet {
    let text = format!("{LOGINS_HEADER}\n{}\n", rows.join("\n"));
    Sheet::parse(SheetKind::Logins, "ai-account-logins.csv", text.as_bytes()).unwrap()
}

fn input<'a>(sheets: &'a [Sheet], macs: &'a [KnownMac], keys: MailboxKeys) -> Input<'a> {
    Input {
        sheets,
        macs,
        vendors: &Stand,
        keys,
        now_ms: 1_800_000_000_000,
        births: true,
    }
}

/// Every changed value, as stored JSON text.
fn values(plan: &Plan) -> Vec<String> {
    plan.rows
        .iter()
        .flatten()
        .map(|c| aterm_json::to_string(c.value.as_ref().unwrap()).unwrap())
        .collect()
}

/// The planned value of `collection.pk.column`.
fn planned<'p>(plan: &'p Plan, collection: &str, pk: &str, column: &str) -> Option<&'p Value> {
    plan.planned
        .iter()
        .find(|p| p.collection == collection && p.pk == pk && p.column == column)
        .map(|p| &p.x)
}

fn kinds(plan: &Plan) -> Vec<&'static str> {
    plan.findings.iter().map(|f| f.kind.word()).collect()
}

/// The registry after `plan`'s rows, written in order by writer 1 on `book`.
fn apply(book: &mut Book, plan: &Plan) -> Cells {
    for changes in &plan.rows {
        let seen = book.all();
        book.write(1, &seen, changes.clone());
    }
    book.cells()
}

/// The vault sheet reads the harness's way: a bare Kimi `1` is held and makes no account (its
/// region decides which account a sign-in makes), Kimi's region word keys `kimi:global`,
/// Codex's flow word is kept, the `x` column is xAI's Grok realm, a note naming one vendor
/// pauses that vendor's account and Claude Code's (whose column any disqualifying note
/// pauses) but no other, and a value no column takes is free text kept as
/// coordinates. Every account is keyed by the canonical claims key from its realm and address,
/// with one provider word per realm.
/// NEGATIVE CONTROL: with a paused reading imported as a member (`Mark::Paused(terms) =>
/// (terms, false)` in `vault`), the Kimi-banned row's Kimi account plans `pending`: `` left:
/// Some(String("pending")) right: Some(String("paused")) ``.
#[test]
fn the_vault_sheet_reads_the_harness_way_and_keys_accounts_by_the_claims_key() {
    let sheet = vault(&[
        ",ann@example.invalid,m3,,1,browser,1,,1,1,,,",
        ",bob@example.invalid,m4.2,,1,,,1,global,,kimi banned,,",
        ",cy@example.invalid,m5,,pro,device,,,cn,device,,,",
    ]);
    let plan = plan(&input(&[sheet], &[], MailboxKeys::default()), &Cells::new()).unwrap();
    let key = |realm, address| keys::account_pk(realm, address).unwrap();
    let ann_claude = key("claude", "ann@example.invalid");
    assert_eq!(
        planned(&plan, "account", &ann_claude, "provider"),
        Some(&json!("anthropic"))
    );
    assert_eq!(
        planned(&plan, "account", &ann_claude, "status"),
        Some(&json!("pending"))
    );
    let ann_codex = key("codex", "ann@example.invalid");
    assert_eq!(
        planned(&plan, "account", &ann_codex, "flow"),
        Some(&json!("browser"))
    );
    assert_eq!(
        planned(&plan, "account", &ann_codex, "provider"),
        Some(&json!("openai"))
    );
    let ann_grok = key("xai", "ann@example.invalid");
    assert_eq!(ann_grok, key("grok", "ann@example.invalid"));
    assert_eq!(
        planned(&plan, "account", &ann_grok, "provider"),
        Some(&json!("xai"))
    );
    // Ann's bare Kimi 1 is held: no account in either Kimi realm.
    for realm in ["kimi:global", "kimi:mainland-cn"] {
        assert!(
            planned(
                &plan,
                "account",
                &key(realm, "ann@example.invalid"),
                "realm"
            )
            .is_none()
        );
    }
    assert!(kinds(&plan).contains(&"held"));
    // Bob's note names Kimi: his Kimi account is paused, and his Claude account (whose column
    // any disqualifying note pauses), but not his Meta account.
    let bob_kimi = key("kimi:global", "bob@example.invalid");
    assert_eq!(
        planned(&plan, "account", &bob_kimi, "status"),
        Some(&json!("paused"))
    );
    assert_eq!(
        planned(&plan, "account", &bob_kimi, "region"),
        Some(&json!("global"))
    );
    assert_eq!(
        planned(
            &plan,
            "account",
            &key("claude", "bob@example.invalid"),
            "status"
        ),
        Some(&json!("paused"))
    );
    assert_eq!(
        planned(
            &plan,
            "account",
            &key("meta", "bob@example.invalid"),
            "status"
        ),
        Some(&json!("pending"))
    );
    assert!(kinds(&plan).contains(&"paused"));
    // Cy's `pro` is free text: no Claude account, a finding, its coordinates on the address.
    assert!(
        planned(
            &plan,
            "account",
            &key("claude", "cy@example.invalid"),
            "realm"
        )
        .is_none()
    );
    let cy = keys::address_pk("cy@example.invalid").unwrap();
    let notes = planned(&plan, "address", &cy, "notes_src").unwrap();
    assert!(
        notes
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["col"] == json!(5))
    );
    assert_eq!(
        planned(
            &plan,
            "account",
            &key("kimi:mainland-cn", "cy@example.invalid"),
            "region"
        ),
        Some(&json!("mainland-cn"))
    );
    // One provider word per realm, shared with the registry's provider rows.
    for item in plan
        .planned
        .iter()
        .filter(|p| p.collection == "account" && p.column == "provider")
    {
        assert!(
            keys::PROVIDERS.contains(&item.x.as_str().unwrap()),
            "{:?}",
            item.x
        );
    }
}

/// No note, free-text cell, lead's reasoning or losing value reaches a replicated cell
/// (`import.rs`'s privacy invariant, on its own `password PRIVATE-CELL` fixture): each stays a
/// source coordinate, and a credential-shaped cell is reported by coordinate and never read.
/// NEGATIVE CONTROL: with `vault` copying each note cell into the address's `note` column
/// (`self.want(&ADDRESS, &address_pk, "note", json!(sheet.text(at)), at)` in place of
/// `self.note(..)`), the plan refuses the credential-shaped note before a record could carry
/// it: `` called `Result::unwrap()` on an `Err` value: Error("address.note: a credential-shaped
/// value is never stored") ``.
#[test]
fn no_note_free_text_or_losing_value_crosses() {
    // A token-shaped note, split here so no whole token sits in the source.
    let token = ["gh", "p_0123456789abcdefABCDEF0123456789abcd"].concat();
    let dee = format!(",dee@example.invalid,m6,,1,,,,,,{token},,");
    let sheet = vault(&[
        "7,owner+two@example.invalid,m3.2,,,,phone required,,,,password PRIVATE-CELL,,",
        "unknown,owner@example.invalid,m100,,,,email link,,,,alias mapping unconfirmed,,",
        &dee,
    ]);
    let other = logins(&[
        "m7,dee@example.invalid,claude,box@proton.example,the lead guessed from the domain,",
    ]);
    let plan = plan(
        &input(&[sheet, other], &[], MailboxKeys::default()),
        &Cells::new(),
    )
    .unwrap();
    let all = values(&plan).join("\n");
    for secret in [
        "PRIVATE-CELL",
        "phone required",
        "email link",
        "alias mapping",
        "lead guessed",
        &token[..6],
        "unknown",
        "box@",
    ] {
        assert!(!all.contains(secret), "{secret}");
    }
    let owner = keys::address_pk("owner+two@example.invalid").unwrap();
    let notes = planned(&plan, "address", &owner, "notes_src")
        .unwrap()
        .as_array()
        .unwrap();
    for col in [1, 7, 11] {
        assert!(
            notes
                .iter()
                .any(|c| c["row"] == json!(2) && c["col"] == json!(col)),
            "{col}"
        );
    }
    assert!(kinds(&plan).contains(&"credential-shaped"));
    // The logins sheet's m7 loses to the vault sheet's m6: a finding, never a value.
    assert!(kinds(&plan).contains(&"sources-disagree"));
    assert!(!all.contains("\"m7\""));
}

/// What the sheets stop saying is retracted three-way: a service dropped from a services cell
/// becomes `false` (an owner's element stays), a seat the sheet moved to another Mac is vacated
/// on the old one so the address holds one home seat, and an account the sheet no longer lists
/// is reported, never removed. A run that reads the other sheet only retracts nothing.
/// NEGATIVE CONTROLS: with the seat pass in `finish` skipped (`if true { continue; }` at its
/// head), the old seat keeps the address beside the new one: `` left:
/// Some(String("ad-0fe911f39376ccda")) right: Some(Null) ``; with the element retraction off
/// (`if false && field.present()`), the dropped service stays: `` left: {"chatgpt", "codex"}
/// right: {"codex"} ``; with `retractable` ignoring the kinds read this run, the other sheet's
/// run reports this sheet's rows: `` nor reports the rows only the other sheet lists ``.
#[test]
fn a_re_import_retracts_what_the_sheet_no_longer_says() {
    let first = logins(&[
        "m5,ann@example.invalid,claude + chatgpt/codex,,lead's reasoning,",
        "m6,bob@example.invalid,chatgpt/codex,,lead's reasoning,",
    ]);
    let mut book = Book::new();
    let one = plan(
        &input(std::slice::from_ref(&first), &[], MailboxKeys::default()),
        &Cells::new(),
    )
    .unwrap();
    apply(&mut book, &one);
    // The owner states Bob's ChatGPT service themself.
    let bob_codex = keys::account_pk("codex", "bob@example.invalid").unwrap();
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![elem(&ACCOUNT, &bob_codex, "services", "chatgpt")],
    );
    let cells = book.cells();
    // Ann moves to m7 and keeps Codex only; Bob keeps Codex only.
    let edited = logins(&[
        "m7,ann@example.invalid,codex,,lead's reasoning,",
        "m6,bob@example.invalid,codex,,lead's reasoning,",
    ]);
    let other = vault(&[",cy@example.invalid,m9,,1,,,,,,,,"]);
    let untouched = plan(&input(&[other], &[], MailboxKeys::default()), &cells).unwrap();
    assert!(
        untouched
            .planned
            .iter()
            .all(|p| p.x != json!(false) && p.x != Value::Null),
        "a run of the other sheet retracts nothing"
    );
    assert!(
        untouched
            .findings
            .iter()
            .all(|f| f.kind != Kind::NoLongerListed),
        "nor reports the rows only the other sheet lists"
    );
    let next = plan(&input(&[edited], &[], MailboxKeys::default()), &cells).unwrap();
    let after = apply(&mut book, &next);
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let old_seat = keys::seat_pk(&keys::machine_pk("m5"), 1).unwrap();
    let new_seat = keys::seat_pk(&keys::machine_pk("m7"), 1).unwrap();
    let seats = cell::project(&SEAT, &after);
    assert_eq!(seats.rows[&old_seat].get("address"), Some(&Value::Null));
    assert_eq!(seats.rows[&new_seat].get("address"), Some(&json!(ann)));
    let accounts = cell::project(&ACCOUNT, &after);
    let ann_codex = keys::account_pk("codex", "ann@example.invalid").unwrap();
    assert_eq!(
        accounts.rows[&ann_codex].fields["services"].members(),
        BTreeSet::from(["codex"])
    );
    assert_eq!(
        accounts.rows[&bob_codex].fields["services"].members(),
        BTreeSet::from(["chatgpt", "codex"])
    );
    let ann_claude = keys::account_pk("claude", "ann@example.invalid").unwrap();
    assert!(next.findings.iter().any(|f| f.kind == Kind::NoLongerListed
        && f.pk.as_deref() == Some(ann_claude.as_str())));
    assert_eq!(
        accounts.rows[&ann_claude].get("status"),
        Some(&json!("pending"))
    );
}

/// A re-import is three-way: the same sheets again write nothing; a cell an earlier import
/// wrote follows the sheet; a cell the owner set, a conflicted cell and a row the owner retired
/// are left as they are and reported.
/// NEGATIVE CONTROL: with a two-way step in `finish` (write whenever the value differs: `if
/// field.and_then(Field::value) == Some(&x) { ImportStep::Unchanged } else { ImportStep::Write
/// }`), the owner's `active` is overwritten: `` left: Some(String("paused")) right:
/// Some(String("active")) ``.
#[test]
fn a_re_import_never_overwrites_the_owner_and_writes_nothing_when_unchanged() {
    let first = vault(&[
        ",ann@example.invalid,m3,,1,browser,,,,,,,",
        ",bob@example.invalid,m4,,1,,,,,,,,",
        ",cy@example.invalid,m5,,1,,,,,,,,",
    ]);
    let mut book = Book::new();
    let one = plan(
        &input(std::slice::from_ref(&first), &[], MailboxKeys::default()),
        &Cells::new(),
    )
    .unwrap();
    let cells = apply(&mut book, &one);
    let again = plan(&input(&[first], &[], MailboxKeys::default()), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.rows);
    // The owner marks Ann's Claude account active, a peer writes Bob's concurrently with
    // another value, and the owner retires Cy's address.
    let ann = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let bob = keys::account_pk("claude", "bob@example.invalid").unwrap();
    let cy = keys::address_pk("cy@example.invalid").unwrap();
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&ACCOUNT, &ann, "status", json!("active"))],
    );
    book.write(
        3,
        &seen[..1],
        vec![put(&ACCOUNT, &bob, "status", json!("broken"))],
    );
    let seen = book.all();
    book.write(2, &seen, vec![put(&ADDRESS, &cy, "row", json!("retired"))]);
    let cells = book.cells();
    // The edited sheet pauses every Claude account and moves Ann to the device flow.
    let edited = vault(&[
        ",ann@example.invalid,m3,,1,device,,,,,claude banned,,",
        ",bob@example.invalid,m4,,1,,,,,,claude banned,,",
        ",cy@example.invalid,m5,,1,,,,,,claude banned,,",
    ]);
    let next = plan(&input(&[edited], &[], MailboxKeys::default()), &cells).unwrap();
    let after = apply(&mut book, &next);
    let accounts = cell::project(&ACCOUNT, &after);
    assert_eq!(accounts.rows[&ann].get("status"), Some(&json!("active")));
    let codex = keys::account_pk("codex", "ann@example.invalid").unwrap();
    assert_eq!(accounts.rows[&codex].get("flow"), Some(&json!("device")));
    assert!(accounts.rows[&bob].fields["status"].conflicted());
    let found = |kind: Kind, pk: &str| {
        next.findings
            .iter()
            .any(|f| f.kind == kind && f.pk.as_deref() == Some(pk))
    };
    assert!(found(Kind::OwnerKept, &ann));
    assert!(found(Kind::Conflicted, &bob));
    assert!(found(Kind::RetiredKept, &cy));
    assert_eq!(
        cell::project(&ADDRESS, &after).rows[&cy].lifecycle(),
        Some("retired")
    );
    // And the edited sheet again writes nothing more.
    let settled = plan(
        &input(
            &[vault(&[
                ",ann@example.invalid,m3,,1,device,,,,,claude banned,,",
                ",bob@example.invalid,m4,,1,,,,,,claude banned,,",
                ",cy@example.invalid,m5,,1,,,,,,claude banned,,",
            ])],
            &[],
            MailboxKeys::default(),
        ),
        &after,
    )
    .unwrap();
    assert!(settled.rows.is_empty(), "{:?}", settled.rows);
}

/// Every label resolves before a placeholder is minted: through a live row's label or alias
/// (a relabelled Mac keeps its row), then a Mac the roster knows by label or alias, whose row is
/// found by its fleet node; only then a planned placeholder keyed by the label. Spellings fold
/// (`M109` is m109, `m27-2` is slot 2 on m27) and are reported.
/// NEGATIVE CONTROL: with `machine_for` skipping the live rows (`let found: Vec<String> =
/// Vec::new();`), the relabelled Mac's old label mints a second machine and its seat lands on
/// it: `` left: "st-ee08c551e4729096-2" right: "st-3dd96e6460b76735-2" `` (the placeholder m40's
/// seat, then the relabelled row's).
#[test]
fn labels_resolve_through_live_rows_and_known_macs_before_a_placeholder() {
    // Born under its fleet node, so its key is not the placeholder its old label would mint.
    let relabelled = keys::machine_pk("n-fedcba9876543210fedcba9876543210");
    let adopted = keys::machine_pk("n-0123456789abcdef0123456789abcdef");
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &relabelled, "label", json!("m107")),
            elem(&MACHINE, &relabelled, "aliases", "m40"),
            put(&MACHINE, &relabelled, "status", json!("active")),
            put(&MACHINE, &relabelled, "row", json!("live")),
        ],
    );
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![
            put(&MACHINE, &adopted, "label", json!("m99")),
            put(
                &MACHINE,
                &adopted,
                "fleet_node",
                json!("0123456789abcdef0123456789abcdef"),
            ),
            put(&MACHINE, &adopted, "status", json!("planned")),
            put(&MACHINE, &adopted, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let macs = [KnownMac {
        label: Some(Label::plain(100)),
        aliases: vec![Label::plain(32)],
        node: Some("0123456789abcdef0123456789abcdef".to_owned()),
        hosts: Vec::new(),
    }];
    let sheet = vault(&[
        ",ann@example.invalid,m40.2,,1,,,,,,,,",
        ",bob@example.invalid,m32,,1,,,,,,,,",
        ",cy@example.invalid,M109,,1,,,,,,,,",
        ",dee@example.invalid,m27-2,,1,,,,,,,,",
    ]);
    let plan = plan(&input(&[sheet], &macs, MailboxKeys::default()), &cells).unwrap();
    let seat_of = |address: &str| {
        let pk = keys::address_pk(address).unwrap();
        plan.planned
            .iter()
            .find(|p| p.collection == "seat" && p.column == "address" && p.x == json!(pk))
            .map(|p| p.pk.clone())
            .unwrap()
    };
    assert_eq!(
        seat_of("ann@example.invalid"),
        keys::seat_pk(&relabelled, 2).unwrap()
    );
    assert_eq!(
        seat_of("bob@example.invalid"),
        keys::seat_pk(&adopted, 1).unwrap()
    );
    let m109 = keys::machine_pk("m109");
    assert_eq!(
        seat_of("cy@example.invalid"),
        keys::seat_pk(&m109, 1).unwrap()
    );
    assert_eq!(
        planned(&plan, "machine", &m109, "label"),
        Some(&json!("m109"))
    );
    assert_eq!(
        planned(&plan, "machine", &m109, "status"),
        Some(&json!("planned"))
    );
    assert_eq!(
        seat_of("dee@example.invalid"),
        keys::seat_pk(&keys::machine_pk("m27"), 2).unwrap()
    );
    // No placeholder for a label a live row or a known Mac already answers.
    for label in ["m40", "m32", "m107", "m100"] {
        assert!(
            planned(&plan, "machine", &keys::machine_pk(label), "label").is_none(),
            "{label}"
        );
    }
    let found = |kind: Kind| plan.findings.iter().filter(|f| f.kind == kind).count();
    assert_eq!(found(Kind::LabelRespelled), 2);
    assert_eq!(found(Kind::AdoptedByNode), 1);
    // The roster's current label differs from the one the owner gave the row: left, reported.
    assert!(plan.findings.iter().any(|f| f.kind == Kind::OwnerKept
        && f.pk.as_deref() == Some(adopted.as_str())
        && f.column.as_deref() == Some("label")));
}

/// An address never links to the mailbox it is itself: that link, beside the address row's own
/// `addr`, would name the mailbox's login. A hub that receives for another row and is listed as
/// a login too is a finding, and the other row's link (which names no login) is written.
/// NEGATIVE CONTROL: with the self arm removed from `logins` (every mailbox address linked),
/// the hub's own row links to its mailbox: `` left: Some(String("mb-…")) right: None ``.
#[test]
fn an_address_never_links_to_the_mailbox_it_is() {
    let sheet = logins(&[
        "m3,hub@proton.example,claude,hub@proton.example,lead's reasoning,",
        "m4,ann@example.invalid,claude,hub@proton.example,lead's reasoning,",
    ]);
    let plan = plan(
        &input(std::slice::from_ref(&sheet), &[], MailboxKeys::default()),
        &Cells::new(),
    )
    .unwrap();
    let hub = keys::address_pk("hub@proton.example").unwrap();
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    assert_eq!(planned(&plan, "address", &hub, "mailbox_hint"), None);
    assert!(
        planned(&plan, "address", &ann, "mailbox_hint")
            .and_then(Value::as_str)
            .is_some_and(|pk| keys::is_digest_pk(pk, "mb"))
    );
    let found = kinds(&plan);
    assert!(found.contains(&"self-mailbox"), "{found:?}");
    assert!(found.contains(&"mailbox-is-login"), "{found:?}");
}

/// A receiving mailbox crosses as a random key, a plain label, a service, a code path and a
/// vault entry name: its address is in no key and no value, its key is no digest of it, and
/// this Mac's mailbox keys give the same key and label on the next import. The lead's guess is
/// a hint and the owner's correction is the mailbox.
/// NEGATIVE CONTROL: with `mailbox` keying a new mailbox by its address's digest
/// (`format!("mb-{}", &keys::address_pk(address).unwrap_or_default()[3..])` in place of
/// `keys::random_pk(..)`), the key carries the login's digest and the assertion that no key
/// does fails with the synthetic login as its message: `` box@proton.example ``.
#[test]
fn mailbox_logins_never_cross_and_keys_are_random_and_kept_locally() {
    let sheet = logins(&[
        "m3,ann@example.invalid,claude,box@proton.example,lead's reasoning,",
        "m4,bob@example.invalid,chatgpt/codex + google/gemini,box@proton.example,lead's reasoning,other@proton.example",
    ]);
    let first = plan(
        &input(std::slice::from_ref(&sheet), &[], MailboxKeys::default()),
        &Cells::new(),
    )
    .unwrap();
    let all = values(&first).join("\n");
    let keys_text: String = first.rows.iter().flatten().map(|c| c.key.clone()).collect();
    for login in ["box@proton.example", "other@proton.example"] {
        assert!(!all.contains(login), "{login}");
        let digest = keys::address_pk(login).unwrap();
        assert!(!keys_text.contains(&digest[3..]), "{login}");
    }
    let mailboxes: Vec<&Planned> = first
        .planned
        .iter()
        .filter(|p| p.collection == "mailbox" && p.column == "label")
        .collect();
    assert_eq!(mailboxes.len(), 2);
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let bob = keys::address_pk("bob@example.invalid").unwrap();
    let hint = planned(&first, "address", &ann, "mailbox_hint")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    assert!(keys::is_digest_pk(&hint, "mb"));
    assert_ne!(
        &hint[3..],
        &keys::address_pk("box@proton.example").unwrap()[3..]
    );
    assert_eq!(
        planned(&first, "address", &bob, "mailbox_hint"),
        Some(&json!(hint))
    );
    let owner = planned(&first, "address", &bob, "mailbox")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(owner, hint);
    let columns: BTreeSet<&str> = first
        .planned
        .iter()
        .filter(|p| p.collection == "mailbox")
        .map(|p| p.column.as_str())
        .collect();
    assert_eq!(
        columns,
        BTreeSet::from(["code_path", "label", "row", "service", "vault_ref"])
    );
    assert_eq!(
        planned(&first, "mailbox", &hint, "vault_ref"),
        Some(
            &json!({"vault": "alab", "entry": "ai-mailbox.toml", "item": format!("mailbox-{hint}")})
        )
    );
    // A fresh plan with no keys mints another random key; the kept keys find the same one.
    let fresh = plan(
        &input(std::slice::from_ref(&sheet), &[], MailboxKeys::default()),
        &Cells::new(),
    )
    .unwrap();
    assert_ne!(
        planned(&fresh, "address", &ann, "mailbox_hint"),
        Some(&json!(hint))
    );
    let kept = MailboxKeys::from_json(&first.keys.to_json()).unwrap();
    let again = plan(&input(&[sheet], &[], kept), &Cells::new()).unwrap();
    assert_eq!(
        planned(&again, "address", &ann, "mailbox_hint"),
        Some(&json!(hint))
    );
    assert_eq!(
        planned(&again, "mailbox", &hint, "label"),
        planned(&first, "mailbox", &hint, "label")
    );
    // Bob's services: a ChatGPT/Codex account with both services, and a Google one.
    let codex = keys::account_pk("codex", "bob@example.invalid").unwrap();
    assert_eq!(
        planned(&first, "account", &codex, "services.chatgpt"),
        Some(&json!(true))
    );
    assert_eq!(
        planned(&first, "account", &codex, "services.codex"),
        Some(&json!(true))
    );
    assert!(
        planned(
            &first,
            "account",
            &keys::account_pk("google", "bob@example.invalid").unwrap(),
            "realm"
        )
        .is_some()
    );
}

/// Seats hold owner placements only, keyed by machine and slot: two addresses in one slot are a
/// clash (the first stays), the sheet's second machine column is a warm seat in that machine's
/// first free slot, an address the owner seated in another slot of that machine is left there,
/// and every seat the import writes says `import`, never an allocator word.
/// NEGATIVE CONTROL: with the clash arm removed from `seats` (a second address in a taken slot
/// written like the first), the slot's address becomes a disagreement instead of a clash: ``
/// assertion failed: kinds(&plan).contains(&"seat-clash") ``.
#[test]
fn seats_hold_owner_placements_and_clash_visibly() {
    let m3 = keys::machine_pk("m3");
    let m8 = keys::machine_pk("m8");
    let moved = keys::address_pk("eve@example.invalid").unwrap();
    let mut book = Book::new();
    let owner_seat = keys::seat_pk(&m8, 4).unwrap();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &m8, "label", json!("m8")),
            put(&MACHINE, &m8, "status", json!("active")),
            put(&MACHINE, &m8, "row", json!("live")),
            put(&SEAT, &owner_seat, "address", json!(moved)),
            put(&SEAT, &owner_seat, "role", json!("home")),
            put(&SEAT, &owner_seat, "why", json!("owner")),
            put(&SEAT, &owner_seat, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let sheet = vault(&[
        ",ann@example.invalid,m3.2,m8,1,,,,,,,,",
        ",bob@example.invalid,m3.2,,1,,,,,,,,",
        ",eve@example.invalid,m8.1,,1,,,,,,,,",
    ]);
    let plan = plan(&input(&[sheet], &[], MailboxKeys::default()), &cells).unwrap();
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let clash = keys::seat_pk(&m3, 2).unwrap();
    assert_eq!(planned(&plan, "seat", &clash, "address"), Some(&json!(ann)));
    assert!(kinds(&plan).contains(&"seat-clash"));
    // Ann's warm seat on m8 takes its first free slot; slot 4 is the owner's.
    let warm = keys::seat_pk(&m8, 1).unwrap();
    assert_eq!(planned(&plan, "seat", &warm, "address"), Some(&json!(ann)));
    assert_eq!(planned(&plan, "seat", &warm, "role"), Some(&json!("warm")));
    // Eve sits where the owner put her: no seat m8.1 for her, a finding instead.
    assert!(
        plan.findings
            .iter()
            .any(|f| f.kind == Kind::SeatMoved && f.pk.as_deref() == Some(owner_seat.as_str()))
    );
    assert!(
        !plan
            .planned
            .iter()
            .any(|p| p.collection == "seat" && p.column == "address" && p.x == json!(moved))
    );
    for item in plan
        .planned
        .iter()
        .filter(|p| p.collection == "seat" && p.column == "why")
    {
        assert_eq!(item.x, json!("import"));
    }
}

/// Two live rows naming one fleet node (any writer can set the cell) adopt neither: the known
/// Mac's label, status and aliases are written nowhere, the clash is a finding, and the generic
/// check reports `fleet_node` as a unique clash.
/// NEGATIVE CONTROL: with `known_found` taking the first row that names the node (`[pk, ..] =>`
/// in place of `[pk] =>`), the roster's label lands on that row: `` assertion failed:
/// plan.planned.iter().all(|p| p.pk != first && p.pk != second) ``.
#[test]
fn two_rows_naming_one_fleet_node_adopt_neither() {
    let node = "00112233445566778899aabbccddeeff";
    let (first, second) = (keys::machine_pk("m12"), keys::machine_pk("m40"));
    let mut book = Book::new();
    for (pk, label) in [(&first, "m12"), (&second, "m40")] {
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, pk, "label", json!(label)),
                put(&MACHINE, pk, "fleet_node", json!(node)),
                put(&MACHINE, pk, "row", json!("live")),
            ],
        );
    }
    let cells = book.cells();
    let macs = [KnownMac {
        label: Some(Label::plain(12)),
        aliases: vec![Label::plain(70)],
        node: Some(node.to_owned()),
        hosts: Vec::new(),
    }];
    let adopt = Input {
        sheets: &[],
        macs: &macs,
        vendors: &Stand,
        keys: MailboxKeys::default(),
        now_ms: 0,
        births: false,
    };
    let plan = plan(&adopt, &cells).unwrap();
    assert!(plan.planned.iter().all(|p| p.pk != first && p.pk != second));
    assert!(kinds(&plan).contains(&"node-ambiguous"));
    let projection = cell::project(&MACHINE, &cells);
    let tables = BTreeMap::from([(MACHINE.name, &projection)]);
    assert!(
        crate::typed::check::findings(&[&MACHINE], &tables)
            .iter()
            .any(
                |f| matches!(f, crate::typed::check::Finding::Unique { columns, .. }
                if columns == &["fleet_node"])
            )
    );
}

/// `adopt` binds the Macs the roster knows to the rows they already have, by fleet node first,
/// and births none: a Mac no row answers is reported, never written. What the roster says
/// carries the roster's coordinate, and the roster is a source row of its own.
/// NEGATIVE CONTROL: with `known_row` birthing whatever `births` says (`None if true =>` in
/// place of `None if self.input.births =>`), the unknown Mac gets a row: `` assertion failed:
/// plan.planned.iter().all(|p| p.pk != stranger) ``. With the roster's values carrying no
/// source (`source: String::new()` in `known_row`, as before the roster was a source), the plan
/// refuses: `` called `Result::unwrap()` on an `Err` value: Error("machine.src: a source
/// coordinate is {source, row, col}, never a value") ``.
#[test]
fn adopt_binds_known_macs_by_node_and_births_none() {
    let row = keys::machine_pk("m12");
    let node = "00112233445566778899aabbccddeeff";
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &row, "label", json!("m12")),
            put(&MACHINE, &row, "status", json!("planned")),
            put(&MACHINE, &row, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let macs = [
        KnownMac {
            label: Some(Label::plain(12)),
            aliases: vec![Label::plain(70)],
            node: Some(node.to_owned()),
            hosts: Vec::new(),
        },
        KnownMac {
            label: Some(Label::plain(13)),
            aliases: Vec::new(),
            node: Some("ffeeddccbbaa99887766554433221100".to_owned()),
            hosts: Vec::new(),
        },
    ];
    let adopt = Input {
        sheets: &[],
        macs: &macs,
        vendors: &Stand,
        keys: MailboxKeys::default(),
        now_ms: 0,
        births: false,
    };
    let plan = plan(&adopt, &cells).unwrap();
    assert_eq!(
        planned(&plan, "machine", &row, "fleet_node"),
        Some(&json!(node))
    );
    assert_eq!(
        planned(&plan, "machine", &row, "status"),
        Some(&json!("active"))
    );
    assert_eq!(
        planned(&plan, "machine", &row, "aliases.m70"),
        Some(&json!(true))
    );
    let stranger = keys::machine_pk("n-ffeeddccbbaa99887766554433221100");
    assert!(plan.planned.iter().all(|p| p.pk != stranger));
    assert!(kinds(&plan).contains(&"unadopted"));
    let src = planned(&plan, "machine", &row, "src").unwrap();
    assert_eq!((&src["row"], &src["col"]), (&json!(1), &json!(0)));
    // Applied, the next adopt finds the row by its node and writes nothing.
    let mut book2 = book;
    let after = apply(&mut book2, &plan);
    let sources = cell::project(&SOURCE, &after);
    let roster = &sources.rows[src["source"].as_str().unwrap()];
    assert_eq!(roster.get("kind"), Some(&json!("swarm-roster")));
    assert_eq!(roster.get("rows"), Some(&json!(2)));
    let again = super::plan(&adopt, &after).unwrap();
    assert!(again.rows.is_empty());
    assert!(again.findings.iter().any(|f| f.kind == Kind::AdoptedByNode));
}

/// An address both sheets list, where they agree, is no disagreement: the vault sheet, read
/// first, names its origin, and the logins sheet's listing adds nothing to compare.
/// NEGATIVE CONTROL: with `address` wanting each sheet's origin (the `named` guard dropped:
/// `if true {`), every shared address is a disagreement: `` assertion `left == right` failed
/// left: ["sources-disagree"] right: [] ``.
#[test]
fn an_address_both_sheets_list_alike_is_no_disagreement() {
    let sheet = vault(&[",ann@example.invalid,m3,,1,,,,,,,,"]);
    let other = logins(&["m3,ann@example.invalid,claude,box@proton.example,x,"]);
    let plan = plan(
        &input(&[sheet, other], &[], MailboxKeys::default()),
        &Cells::new(),
    )
    .unwrap();
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    assert_eq!(
        planned(&plan, "address", &ann, "origin"),
        Some(&json!("sheet"))
    );
    let disagreements: Vec<&str> = kinds(&plan)
        .into_iter()
        .filter(|kind| *kind == "sources-disagree")
        .collect();
    assert_eq!(disagreements, Vec::<&str>::new());
}

/// A stored mailbox key file reads back as written, refuses anything else, and never holds an
/// address: each mailbox is filed under its address's digest.
/// NEGATIVE CONTROL: with `MailboxKeys::digest` returning the address itself, the stored form
/// holds it: `` assertion failed: !text.contains('@') ``.
#[test]
fn mailbox_keys_round_trip() {
    let sheet = logins(&["m3,ann@example.invalid,claude,box@proton.example,x,"]);
    let plan = plan(&input(&[sheet], &[], MailboxKeys::default()), &Cells::new()).unwrap();
    assert_eq!(plan.keys.len(), 1);
    assert_eq!(
        MailboxKeys::from_json(&plan.keys.to_json()).unwrap(),
        plan.keys
    );
    assert!(MailboxKeys::from_json(&json!({"format": "other"})).is_err());
    let text = aterm_json::to_string(&plan.keys.to_json()).unwrap();
    assert!(!text.contains('@'));
}
