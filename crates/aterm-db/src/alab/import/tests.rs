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
    alab::registry::{ACCOUNT, ADDRESS, MACHINE, PREFERENCE},
    typed::{
        grid::tests::{Book, elem, put},
        secret::Fixed,
    },
};

/// A synthetic `[everyone]` key the harness files' sources are keyed under.
const KEY: [u8; 32] = [0x3c; 32];

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

    /// The fixtures' harness file: `realm,email,label,machine machine` per line.
    fn stated(
        &self,
        _kind: SheetKind,
        _name: &str,
        text: &str,
    ) -> std::result::Result<Vec<Stated>, String> {
        text.lines()
            .map(|line| {
                let [realm, email, label, machines] = line.split(',').collect::<Vec<_>>()[..]
                else {
                    return Err(line.to_owned());
                };
                Ok(Stated {
                    realm: realm.to_owned(),
                    email: email.to_owned(),
                    label: (!label.is_empty()).then(|| label.to_owned()),
                    machines: machines.split_whitespace().map(str::to_owned).collect(),
                })
            })
            .collect()
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

/// The registry's sealer in these plans: a fixed key, the registry's table name.
pub(crate) const SEALER: Fixed = Fixed("alab/registry");

fn input<'a>(sheets: &'a [Sheet], macs: &'a [KnownMac]) -> Input<'a> {
    Input {
        sheets,
        macs,
        vendors: &Stand,
        sealer: &SEALER,
        now_ms: 1_800_000_000_000,
        births: true,
        unread: &[],
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

/// Every changed value but an address row's `addr`, the one cell an address's text is in (a
/// mailbox's login is an address row like any other), as stored JSON text.
fn values_but_addr(plan: &Plan) -> Vec<String> {
    plan.rows
        .iter()
        .flatten()
        .filter(|c| !(c.collection == ADDRESS.name && c.key.ends_with(".addr")))
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

/// The live or planned key of the preference `label`'s Mac has for the Claude account of `address`.
fn pref(label: &str, address: &str) -> String {
    keys::preference_pk(
        &keys::machine_pk(label),
        &keys::account_pk("claude", address).unwrap(),
    )
    .unwrap()
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
/// pauses) but no other, and a value no column takes is free text, its coordinate kept. Every account is keyed by the canonical claims key from its realm and address,
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
    let plan = plan(&input(&[sheet], &[]), &Cells::new()).unwrap();
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
    // Cy's `pro` is free text: no Claude account, a finding, its coordinate on the address.
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

/// A note crosses behind the guard, and nothing else does (`import.rs`'s privacy invariant, on
/// its own `password PRIVATE-CELL` fixture): the sheet's plain notes and free-text cells reach
/// the row's `note`; a note naming a secret, a credential-shaped cell and one holding an address
/// stay coordinates with a `note-withheld` finding; the `m` column, the lead's reasoning and a
/// losing value never cross. A withheld cell costs only itself: the plain cell after it is kept.
/// NEGATIVE CONTROL: with `note_text` skipping the per-cell guard (`match Ok::<(), guard::Refusal>
/// (())`), the joined text is refused and the withheld cell's neighbours go with it: `` left: 5
/// right: 4 `` at the withheld count; with the joined check off too (`if true {`), the plan
/// itself refuses before any record could carry it: `` called `Result::unwrap()` on an `Err` value:
/// Error("address.note: free text naming a secret is never stored; keep it in the vault") ``.
#[test]
fn notes_cross_behind_the_guard_and_nothing_else_does() {
    // A token-shaped note, split here so no whole token sits in the source.
    let token = ["gh", "p_0123456789abcdefABCDEF0123456789abcd"].concat();
    let dee = format!(",dee@example.invalid,m6,,1,,,,,,{token},,");
    let sheet = vault(&[
        "7,owner+two@example.invalid,m3.2,,,,phone required,,,,password PRIVATE-CELL,,",
        "unknown,owner@example.invalid,m100,,,,email link,,,,alias mapping unconfirmed,,",
        &dee,
        ",gil@example.invalid,m8,,,,,,,,forwards to hal@example.invalid,,",
        ",ida@example.invalid,m9,,,,,,,,\"line one\nline two\",kept too,",
        ",jan@example.invalid,m9,,,,,,,,password PRIVATE-CELL,kept after,",
    ]);
    let other = logins(&[
        "m7,dee@example.invalid,claude,box@proton.example,the lead guessed from the domain,",
    ]);
    let plan = plan(&input(&[sheet, other], &[]), &Cells::new()).unwrap();
    let all = values_but_addr(&plan).join("\n");
    for secret in [
        "PRIVATE-CELL",
        "lead guessed",
        &token[..6],
        "unknown",
        "box@",
        "hal@",
    ] {
        assert!(!all.contains(secret), "{secret}");
    }
    let note = |address: &str| {
        planned(
            &plan,
            "address",
            &keys::address_pk(address).unwrap(),
            "note",
        )
    };
    assert_eq!(
        note("owner@example.invalid"),
        Some(&json!("google: email link; alias mapping unconfirmed"))
    );
    // The withheld note is gone from the text; the free-text cell beside it is not.
    assert_eq!(
        note("owner+two@example.invalid"),
        Some(&json!("google: phone required"))
    );
    assert_eq!(note("dee@example.invalid"), None);
    assert_eq!(note("gil@example.invalid"), None);
    assert_eq!(
        note("ida@example.invalid"),
        Some(&json!("line one line two; kept too"))
    );
    let withheld: Vec<&Finding> = plan
        .findings
        .iter()
        .filter(|f| f.kind == Kind::NoteWithheld)
        .collect();
    assert_eq!(withheld.len(), 4, "{withheld:?}");
    assert_eq!(note("jan@example.invalid"), Some(&json!("kept after")));
    let owner = keys::address_pk("owner+two@example.invalid").unwrap();
    let coordinates = planned(&plan, "address", &owner, "notes_src")
        .unwrap()
        .as_array()
        .unwrap();
    for col in [1, 7, 11] {
        assert!(
            coordinates
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

/// A sheet's note is three-way: an owner's note wins and is a finding, an import's follows the
/// sheet, and a note the sheet drops is emptied; so a re-run of the same sheet writes nothing.
/// NEGATIVE CONTROL: with `("address", "note")` out of `RETRACT_VALUES`, the dropped note stays:
/// `` left: Some(String("first note")) right: Some(Null) ``.
#[test]
fn a_notes_import_is_three_way() {
    let first = vault(&[
        ",ann@example.invalid,m3,,1,,,,,,first note,,",
        ",bob@example.invalid,m4,,1,,,,,,first note,,",
    ]);
    let mut book = Book::new();
    let one = plan(&input(std::slice::from_ref(&first), &[]), &Cells::new()).unwrap();
    let cells = apply(&mut book, &one);
    let again = plan(&input(&[first], &[]), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.rows);
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let bob = keys::address_pk("bob@example.invalid").unwrap();
    let seen = book.all();
    book.write(2, &seen, vec![put(&ADDRESS, &ann, "note", json!("mine"))]);
    let cells = book.cells();
    let edited = vault(&[
        ",ann@example.invalid,m3,,1,,,,,,second note,,",
        ",bob@example.invalid,m4,,1,,,,,,,,",
    ]);
    let next = plan(&input(&[edited], &[]), &cells).unwrap();
    let after = apply(&mut book, &next);
    let rows = cell::project(&ADDRESS, &after);
    assert_eq!(rows.rows[&ann].get("note"), Some(&json!("mine")));
    assert_eq!(rows.rows[&bob].get("note"), Some(&Value::Null));
    assert!(next.findings.iter().any(|f| f.kind == Kind::OwnerKept
        && f.pk.as_deref() == Some(ann.as_str())
        && f.column.as_deref() == Some("note")));
}

/// What the sheets stop saying is retracted three-way: a service dropped from a services cell
/// becomes `false` (an owner's element stays), a preference the sheet moved to another Mac ends
/// on the old one and starts on the new, and an account the sheet no longer lists is reported,
/// never removed. A run that reads the other sheet only retracts nothing.
/// NEGATIVE CONTROLS: with the preference pass in `finish` skipped (`if true { continue; }` at
/// its head), the old row stays live: `` left: Some("live") right: Some("ended") ``; with the element retraction off
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
    let one = plan(&input(std::slice::from_ref(&first), &[]), &Cells::new()).unwrap();
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
    let untouched = plan(&input(&[other], &[]), &cells).unwrap();
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
    let next = plan(&input(&[edited], &[]), &cells).unwrap();
    let after = apply(&mut book, &next);
    let ann_codex_pk = keys::account_pk("codex", "ann@example.invalid").unwrap();
    let old = keys::preference_pk(&keys::machine_pk("m5"), &ann_codex_pk).unwrap();
    let new = keys::preference_pk(&keys::machine_pk("m7"), &ann_codex_pk).unwrap();
    let preferences = cell::project(&PREFERENCE, &after);
    assert_eq!(preferences.rows[&old].lifecycle(), Some("ended"));
    assert!(preferences.rows[&new].live());
    // The account no sheet lists now keeps its row and loses its mapping.
    assert_eq!(
        preferences.rows[&pref("m5", "ann@example.invalid")].lifecycle(),
        Some("ended")
    );
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
    let one = plan(&input(std::slice::from_ref(&first), &[]), &Cells::new()).unwrap();
    let cells = apply(&mut book, &one);
    let again = plan(&input(&[first], &[]), &cells).unwrap();
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
    let next = plan(&input(&[edited], &[]), &cells).unwrap();
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
        ),
        &after,
    )
    .unwrap();
    assert!(settled.rows.is_empty(), "{:?}", settled.rows);
}

/// Every label resolves before a placeholder is minted: through a live row's label or alias
/// (a relabelled Mac keeps its row), then a Mac the roster knows by label or alias, whose row is
/// found by its fleet node; only then a planned placeholder keyed by the label. Spellings fold
/// (`M109` is m109, `m27-2` is number 2 on m27) and are reported.
/// NEGATIVE CONTROL: with `machine_for` skipping the live rows (`let found: Vec<String> =
/// Vec::new();`), the relabelled Mac's old label mints a second machine and its preference lands on
/// it: `` left: "mc-ee08c551e4729096" right: "mc-3dd96e6460b76735" `` (the placeholder m40's
/// key, then the relabelled row's).
#[test]
fn labels_resolve_through_live_rows_and_known_macs_before_a_placeholder() {
    // Born under its fleet node, so its key is not the placeholder its old label would mint.
    let relabelled = keys::machine_pk("n-fedcba9876543210fedcba9876543210");
    let adopted = keys::machine_pk("n-0123456789abcdef");
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
            put(&MACHINE, &adopted, "node", json!("0123456789abcdef")),
            put(&MACHINE, &adopted, "status", json!("planned")),
            put(&MACHINE, &adopted, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let macs = [KnownMac {
        label: Some(Label::plain(100)),
        aliases: vec![Label::plain(32)],
        node: Some("0123456789abcdef".to_owned()),
        hosts: Vec::new(),
    }];
    let sheet = vault(&[
        ",ann@example.invalid,m40.2,,1,,,,,,,,",
        ",bob@example.invalid,m32,,1,,,,,,,,",
        ",cy@example.invalid,M109,,1,,,,,,,,",
        ",dee@example.invalid,m27-2,,1,,,,,,,,",
    ]);
    let plan = plan(&input(&[sheet], &macs), &cells).unwrap();
    let machine_of = |address: &str| {
        let pk = keys::account_pk("claude", address).unwrap();
        plan.planned
            .iter()
            .find(|p| p.collection == "preference" && p.column == "account" && p.x == json!(pk))
            .and_then(|p| planned(&plan, "preference", &p.pk, "machine"))
            .and_then(Value::as_str)
            .unwrap()
            .to_owned()
    };
    assert_eq!(machine_of("ann@example.invalid"), relabelled);
    assert_eq!(machine_of("bob@example.invalid"), adopted);
    let m109 = keys::machine_pk("m109");
    assert_eq!(machine_of("cy@example.invalid"), m109);
    assert_eq!(
        planned(&plan, "machine", &m109, "label"),
        Some(&json!("m109"))
    );
    assert_eq!(
        planned(&plan, "machine", &m109, "status"),
        Some(&json!("planned"))
    );
    assert_eq!(machine_of("dee@example.invalid"), keys::machine_pk("m27"));
    assert_eq!(
        planned(
            &plan,
            "preference",
            &pref("m27", "dee@example.invalid"),
            "rank"
        ),
        Some(&json!(2))
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

/// A sheet naming a label whose birth key a retired row holds (m40 retired, a replacement Mac
/// planned under m40) births a row of its own under `<label>/2`, as `add machine` keys it: the
/// preference names the new row and nothing is written onto the retired one. A second import
/// over the result answers the label by the new live row and writes nothing.
/// NEGATIVE CONTROL: with `machine_for` keying the birth `keys::machine_pk(&stored)` again, the
/// preference names the retired row (`left: Some(String("mc-…")) right: Some(String("mc-…"))`).
#[test]
fn a_retired_machines_label_births_a_row_of_its_own() {
    let retired = keys::machine_pk("m40");
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &retired, "label", json!("m40")),
            put(&MACHINE, &retired, "status", json!("active")),
            put(&MACHINE, &retired, "row", json!("retired")),
        ],
    );
    let cells = book.cells();
    let sheets = [vault(&[",ann@example.test,m40,,1,,,,,,,,"])];
    let first = plan(&input(&sheets, &[]), &cells).unwrap();
    let fresh = keys::machine_pk("m40/2");
    let ann = keys::account_pk("claude", "ann@example.test").unwrap();
    let on = |plan: &Plan| {
        plan.planned
            .iter()
            .find(|p| p.collection == "preference" && p.column == "account" && p.x == json!(ann))
            .and_then(|p| planned(plan, "preference", &p.pk, "machine"))
            .cloned()
    };
    assert_eq!(on(&first), Some(json!(fresh)));
    assert_eq!(
        planned(&first, "machine", &fresh, "label"),
        Some(&json!("m40"))
    );
    assert!(
        first.planned.iter().all(|p| p.pk != retired),
        "nothing is written onto the retired row"
    );
    let cells = apply(&mut book, &first);
    let again = plan(&input(&sheets, &[]), &cells).unwrap();
    assert!(
        again.rows.is_empty(),
        "a re-import over the result writes nothing"
    );
}

/// A roster Mac whose node-born key a retired row holds (born `n-<node>`, then retired) is born
/// again under `n-<node>/2`: the import writes label, status and node onto the new row and
/// nothing onto the retired one, which keeps its history. A second import finds the new row by
/// its node and writes nothing.
/// NEGATIVE CONTROL: with `known_birth` keying a node birth `keys::machine_pk("n-<node>")` again,
/// the label lands on the retired row: `left: None right: Some(String("m77"))`.
#[test]
fn a_retired_node_born_row_leaves_the_mac_a_row_of_its_own() {
    let node = "0123456789abcdef";
    let retired = keys::machine_pk(&format!("n-{node}"));
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &retired, "label", json!("m77")),
            put(&MACHINE, &retired, "node", json!(node)),
            put(&MACHINE, &retired, "status", json!("active")),
            put(&MACHINE, &retired, "row", json!("retired")),
        ],
    );
    let cells = book.cells();
    let macs = [KnownMac {
        label: Some(Label::plain(77)),
        aliases: Vec::new(),
        node: Some(node.to_owned()),
        hosts: Vec::new(),
    }];
    let first = plan(&input(&[], &macs), &cells).unwrap();
    let fresh = keys::machine_pk(&format!("n-{node}/2"));
    assert_eq!(
        planned(&first, "machine", &fresh, "label"),
        Some(&json!("m77"))
    );
    assert_eq!(planned(&first, "machine", &fresh, "node"), None);
    assert!(
        first.planned.iter().all(|p| p.pk != retired),
        "nothing is written onto the retired row"
    );
    let cells = apply(&mut book, &first);
    let again = plan(&input(&[], &macs), &cells).unwrap();
    assert!(
        again.rows.is_empty(),
        "a re-import over the result writes nothing"
    );
}

/// The mailbox-login rule follows the owner's sheet (docs/DESIGN-alab-one-mesh-2026-11-05.md
/// §4.7): a hub that receives for another row and is listed as a login links to its own mailbox
/// like any other, the mailbox names it as its `login`, and no finding stands in the way.
/// NEGATIVE CONTROL: with `mailbox` wanting no `login`, the hub's mailbox names none: `` left:
/// None right: Some(String("ad-…")) ``.
#[test]
fn a_login_links_to_its_own_mailbox() {
    let sheet = logins(&[
        "m3,hub@proton.example,claude,hub@proton.example,lead's reasoning,",
        "m4,ann@example.invalid,claude,hub@proton.example,lead's reasoning,",
    ]);
    let plan = plan(&input(std::slice::from_ref(&sheet), &[]), &Cells::new()).unwrap();
    let hub = keys::address_pk("hub@proton.example").unwrap();
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let mailbox = keys::mailbox_pk(&hub).unwrap();
    assert_eq!(
        planned(&plan, "address", &hub, "mailbox_hint"),
        Some(&json!(mailbox))
    );
    assert_eq!(
        planned(&plan, "address", &ann, "mailbox_hint"),
        Some(&json!(mailbox))
    );
    assert_eq!(
        planned(&plan, "mailbox", &mailbox, "login"),
        Some(&json!(hub))
    );
    let found = kinds(&plan);
    assert!(!found.contains(&"self-mailbox"), "{found:?}");
    assert!(!found.contains(&"mailbox-is-login"), "{found:?}");
}

/// A receiving mailbox crosses as the key its login's address key derives, a plain label, a
/// service, a code path and its `login` reference: its address is in no key and no value, and
/// a fresh plan on any Mac names the same key, so no key file is kept. The lead's guess is a
/// hint and the owner's correction is the mailbox.
/// NEGATIVE CONTROL: with `mailbox` keying a new mailbox by `keys::random_pk`, the fresh plan's
/// hint differs: `` left: Some(String("mb-…")) right: Some(String("mb-…")) ``.
#[test]
fn mailbox_keys_derive_from_their_login() {
    let sheet = logins(&[
        "m3,ann@example.invalid,claude,box@proton.example,lead's reasoning,",
        "m4,bob@example.invalid,chatgpt/codex + google/gemini,box@proton.example,lead's reasoning,other@proton.example",
    ]);
    let first = plan(&input(std::slice::from_ref(&sheet), &[]), &Cells::new()).unwrap();
    let all = values_but_addr(&first).join("\n");
    for login in ["box@proton.example", "other@proton.example"] {
        assert!(!all.contains(login), "{login}");
    }
    let mailboxes: Vec<&Planned> = first
        .planned
        .iter()
        .filter(|p| p.collection == "mailbox" && p.column == "label")
        .collect();
    assert_eq!(mailboxes.len(), 2);
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let bob = keys::address_pk("bob@example.invalid").unwrap();
    let boxed = keys::address_pk("box@proton.example").unwrap();
    let hint = keys::mailbox_pk(&boxed).unwrap();
    assert_eq!(
        planned(&first, "address", &ann, "mailbox_hint"),
        Some(&json!(hint))
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
    assert_eq!(
        owner,
        keys::mailbox_pk(&keys::address_pk("other@proton.example").unwrap()).unwrap()
    );
    let columns: BTreeSet<&str> = first
        .planned
        .iter()
        .filter(|p| p.collection == "mailbox")
        .map(|p| p.column.as_str())
        .collect();
    assert_eq!(
        columns,
        BTreeSet::from(["code_path", "label", "login", "row", "service"])
    );
    assert_eq!(
        planned(&first, "mailbox", &hint, "login"),
        Some(&json!(boxed))
    );
    let fresh = plan(&input(std::slice::from_ref(&sheet), &[]), &Cells::new()).unwrap();
    assert_eq!(
        planned(&fresh, "address", &ann, "mailbox_hint"),
        Some(&json!(hint))
    );
    assert_eq!(
        planned(&fresh, "mailbox", &hint, "label"),
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

/// Every account of a row gets a preference per machine its cells name, and the number is a rank
/// and nothing else: `m3.2` is rank 2, a bare first cell rank 1, a second machine column one more
/// preference with no rank unless it writes one, a two-Mac label one unranked preference on each,
/// and two accounts at one rank on one Mac are no clash. No machine or rank is read from an
/// address: `m9.5@example.invalid` is on m3 alone. A row with no account names no mapping, and
/// says so. Each default label names no machine and no address.
/// NEGATIVE CONTROL: with `label_cell` ranking a second bare cell as the grammar does (`if false
/// && !first`), Ann's m8 ranks with the accounts there: `` left: Some(Number(1)) right: None ``.
#[test]
fn every_account_of_a_row_gets_its_mappings_and_a_number_is_only_a_rank() {
    let sheet = vault(&[
        ",ann@example.invalid,m3.2,m8,1,browser,,,,,,,",
        ",bob@example.invalid,m3.2,m8.4,1,,,,,,,,",
        ",cy@example.invalid,m106-m38,,1,,,,,,,,",
        ",dee@example.invalid,m4,,1,,,,,,,,",
        ",m9.5@example.invalid,m3,,1,,,,,,,,",
        ",eve@example.invalid,m5,,,,,,,,,,",
    ]);
    let plan = plan(&input(&[sheet], &[]), &Cells::new()).unwrap();
    let rank =
        |label: &str, address: &str| planned(&plan, "preference", &pref(label, address), "rank");
    let row = |label: &str, address: &str| {
        planned(&plan, "preference", &pref(label, address), "machine").is_some()
    };
    assert_eq!(rank("m3", "ann@example.invalid"), Some(&json!(2)));
    assert_eq!(rank("m3", "bob@example.invalid"), Some(&json!(2)));
    assert_eq!(rank("m8", "ann@example.invalid"), None);
    assert!(row("m8", "ann@example.invalid"));
    assert_eq!(rank("m8", "bob@example.invalid"), Some(&json!(4)));
    assert_eq!(rank("m4", "dee@example.invalid"), Some(&json!(1)));
    for label in ["m106", "m38"] {
        assert!(row(label, "cy@example.invalid"), "{label}");
        assert_eq!(rank(label, "cy@example.invalid"), None);
    }
    // Ann's two accounts each get both.
    let codex = keys::account_pk("codex", "ann@example.invalid").unwrap();
    for label in ["m3", "m8"] {
        let pk = keys::preference_pk(&keys::machine_pk(label), &codex).unwrap();
        assert!(
            planned(&plan, "preference", &pk, "account").is_some(),
            "{label}"
        );
    }
    // Nothing from the address.
    assert!(row("m3", "m9.5@example.invalid"));
    assert!(!row("m9", "m9.5@example.invalid"));
    assert!(
        planned(&plan, "machine", &keys::machine_pk("m9"), "label").is_none(),
        "an address is no label"
    );
    // A row with no account births its machine and maps nothing, and says so.
    assert!(planned(&plan, "machine", &keys::machine_pk("m5"), "label").is_some());
    assert!(kinds(&plan).contains(&"no-account"));
    for item in plan
        .planned
        .iter()
        .filter(|p| p.collection == "preference" && p.column == "why")
    {
        assert_eq!(item.x, json!("import"));
    }
    let ann = keys::account_pk("claude", "ann@example.invalid").unwrap();
    assert_eq!(
        planned(&plan, "account", &ann, "label"),
        Some(&json!(keys::default_account_label("claude", &ann).unwrap()))
    );
    assert!(
        !plan
            .findings
            .iter()
            .any(|f| f.kind == Kind::SourcesDisagree),
        "{:?}",
        plan.findings
    );
}

/// A preference the source drops ends three-way and comes back with it; one with an owner's cell
/// on it, or one the owner ended, is left; and a label cell the run could not read ends nothing,
/// since it says nothing against the mapping.
/// NEGATIVE CONTROLS: with `owner_cell` always false, the owner's row ends: `` left: Some("ended")
/// right: Some("live") ``; with `unsettled` never consulted, the unreadable cell ends Bob's: ``
/// left: Some("ended") right: Some("live") ``; with `revived` always `None`, the restated row
/// stays ended: `` assertion failed: rows[&cy].live() ``.
#[test]
fn ending_a_preference_is_three_way() {
    let first = vault(&[
        ",ann@example.invalid,m3,,1,,,,,,,,",
        ",bob@example.invalid,m4,,1,,,,,,,,",
        ",cy@example.invalid,m5,,1,,,,,,,,",
        ",dee@example.invalid,m6,,1,,,,,,,,",
    ]);
    let mut book = Book::new();
    let one = plan(&input(std::slice::from_ref(&first), &[]), &Cells::new()).unwrap();
    apply(&mut book, &one);
    let ann = pref("m3", "ann@example.invalid");
    let bob = pref("m4", "bob@example.invalid");
    let cy = pref("m5", "cy@example.invalid");
    let dee = pref("m6", "dee@example.invalid");
    let seen = book.all();
    // The owner ranks Ann's row and ends Dee's.
    book.write(
        2,
        &seen,
        vec![
            put(&PREFERENCE, &ann, "rank", json!(7)),
            put(&PREFERENCE, &dee, "row", json!("ended")),
        ],
    );
    let cells = book.cells();
    let dropped = vault(&[
        ",ann@example.invalid,,,1,,,,,,,,",
        ",bob@example.invalid,not a label,,1,,,,,,,,",
        ",cy@example.invalid,,,1,,,,,,,,",
        ",dee@example.invalid,m6,,1,,,,,,,,",
    ]);
    let next = plan(&input(&[dropped], &[]), &cells).unwrap();
    let after = apply(&mut book, &next);
    let rows = cell::project(&PREFERENCE, &after).rows;
    assert_eq!(rows[&ann].lifecycle(), Some("live"));
    assert_eq!(rows[&bob].lifecycle(), Some("live"));
    assert_eq!(rows[&cy].lifecycle(), Some("ended"));
    assert_eq!(rows[&dee].lifecycle(), Some("ended"));
    assert!(
        next.findings
            .iter()
            .any(|f| f.kind == Kind::OwnerKept && f.pk.as_deref() == Some(ann.as_str()))
    );
    assert!(
        next.findings
            .iter()
            .any(|f| f.kind == Kind::RetiredKept && f.pk.as_deref() == Some(dee.as_str()))
    );
    // The account row itself, and its status, are untouched by any of it.
    let account = keys::account_pk("claude", "cy@example.invalid").unwrap();
    assert_eq!(
        cell::project(&ACCOUNT, &after).rows[&account].get("status"),
        Some(&json!("pending"))
    );
    let restated = vault(&[",cy@example.invalid,m5,,1,,,,,,,,"]);
    let back = plan(&input(&[restated], &[]), &after).unwrap();
    let after = apply(&mut book, &back);
    let rows = cell::project(&PREFERENCE, &after).rows;
    assert!(rows[&cy].live());
    assert_eq!(rows[&dee].lifecycle(), Some("ended"));
}

/// An owner's correction naming another listed login links this address to it, and a `+tag` of a
/// listed login is its alias unless a correction says otherwise; a guess links nothing, and an
/// address listed nowhere is no target (a mailbox's login never enters a row).
/// NEGATIVE CONTROL: with `link_alias` called for a guess too (`column == "mailbox"` dropped), the
/// lead's guess makes a link: `` left: Some(String("ad-…")) right: None ``.
#[test]
fn an_alias_links_to_the_listed_login_it_is_received_at() {
    let hub = keys::address_pk("hub@example.invalid").unwrap();
    let sheet = logins(&[
        "m3,hub@example.invalid,claude,,x,",
        "m4,ann@example.invalid,claude,,x,hub@example.invalid",
        "m5,bob@example.invalid,claude,hub@example.invalid,x,",
        "m6,cy@example.invalid,claude,,x,box@proton.example",
        "m7,hub+two@example.invalid,claude,,x,",
        "m8,hub+three@example.invalid,claude,,x,ann@example.invalid",
        "m9,lone+tag@example.invalid,claude,,x,",
    ]);
    let plan = plan(&input(&[sheet], &[]), &Cells::new()).unwrap();
    let link = |address: &str| {
        planned(
            &plan,
            "address",
            &keys::address_pk(address).unwrap(),
            "alias_of",
        )
    };
    assert_eq!(link("ann@example.invalid"), Some(&json!(hub)));
    assert_eq!(link("bob@example.invalid"), None);
    assert_eq!(link("cy@example.invalid"), None);
    assert_eq!(link("hub+two@example.invalid"), Some(&json!(hub)));
    assert_eq!(
        link("hub+three@example.invalid"),
        Some(&json!(keys::address_pk("ann@example.invalid").unwrap()))
    );
    assert_eq!(link("lone+tag@example.invalid"), None);
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    assert_eq!(
        planned(&plan, "address", &ann, "kind"),
        Some(&json!("alias"))
    );
    assert!(!values_but_addr(&plan).join("\n").contains("box@"));
}

/// A harness file adds what the sheets did not state, below them: its machines are a set, so a
/// warm list adds preferences a sheet did not name, unranked, and keeps the sheet's rank where it
/// did; its label says the rank on the machine it names and becomes `legacy_label`, never `label`;
/// an account the vault sheet did not give is a disagreement, no row; a file dropped from a later
/// run ends what it alone stated only when its kind is read.
/// NEGATIVE CONTROL: with `harness` writing past the `held` check (`if true || !held`), the fleet
/// file's `m3-5` is a disagreement with the sheet's rank, when a harness file only adds: this
/// test's `` a preference a sheet stated is not compared `` message.
#[test]
fn a_harness_file_adds_below_the_sheets() {
    let stated = |kind, name: &str, text: &str| {
        let rows = Stand.stated(kind, name, text).unwrap();
        Sheet::stated(kind, name, text.as_bytes(), &rows, &KEY).unwrap()
    };
    let sheet = vault(&[",ann@example.invalid,m3.2,,1,,,,,,,,"]);
    let fleet = stated(
        SheetKind::FleetAccounts,
        "fleet-accounts.toml",
        "claude,ann@example.invalid,m3-5,m3\nclaude,bob@example.invalid,m8-2,m8\nkimi:global,bob@example.invalid,kimi-m16,m16",
    );
    let pool = stated(
        SheetKind::AiPool,
        "ai-pool.toml",
        "claude,ann@example.invalid,,m3 m9\ncodex,ann@example.invalid,,m9",
    );
    let sheets = [pool, sheet, fleet];
    let first = plan(&input(&sheets, &[]), &Cells::new()).unwrap();
    let rank =
        |label: &str, address: &str| planned(&first, "preference", &pref(label, address), "rank");
    assert_eq!(rank("m3", "ann@example.invalid"), Some(&json!(2)));
    assert!(
        planned(
            &first,
            "preference",
            &pref("m9", "ann@example.invalid"),
            "machine"
        )
        .is_some()
    );
    assert_eq!(rank("m9", "ann@example.invalid"), None);
    assert_eq!(rank("m8", "bob@example.invalid"), Some(&json!(2)));
    let kimi = keys::account_pk("kimi:global", "bob@example.invalid").unwrap();
    let on_m16 = keys::preference_pk(&keys::machine_pk("m16"), &kimi).unwrap();
    assert_eq!(
        planned(&first, "preference", &on_m16, "rank"),
        Some(&json!(1))
    );
    assert_eq!(
        planned(&first, "account", &kimi, "legacy_label"),
        Some(&json!("kimi-m16"))
    );
    assert_eq!(
        planned(&first, "account", &kimi, "label"),
        Some(&json!(
            keys::default_account_label("kimi:global", &kimi).unwrap()
        ))
    );
    // The vault sheet gave Ann no Codex account: compared, no row.
    let codex = keys::account_pk("codex", "ann@example.invalid").unwrap();
    assert!(planned(&first, "account", &codex, "realm").is_none());
    assert!(kinds(&first).contains(&"sources-disagree"));
    assert!(
        !first
            .findings
            .iter()
            .any(|f| f.kind == Kind::SourcesDisagree && f.collection == Some(PREFERENCE.name)),
        "a preference a sheet stated is not compared: {:?}",
        first.findings
    );
    let bob = keys::address_pk("bob@example.invalid").unwrap();
    assert_eq!(
        planned(&first, "address", &bob, "origin"),
        Some(&json!("harness"))
    );
    // Applied, the same sources write nothing; a run without the pool file ends nothing, and one
    // that reads a pool file no longer naming m9 ends that row alone.
    let mut book = Book::new();
    let cells = apply(&mut book, &first);
    let again = plan(&input(&sheets, &[]), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.rows);
    let [_, sheet, fleet] = sheets;
    let without = [sheet.clone(), fleet.clone()];
    let skipped = plan(&input(&without, &[]), &cells).unwrap();
    assert!(skipped.planned.iter().all(|p| p.x != json!("ended")));
    let shorter = stated(
        SheetKind::AiPool,
        "ai-pool.toml",
        "claude,ann@example.invalid,,m3",
    );
    let next = plan(&input(&[sheet, fleet, shorter], &[]), &cells).unwrap();
    let ended: Vec<&str> = next
        .planned
        .iter()
        .filter(|p| p.x == json!("ended"))
        .map(|p| p.pk.as_str())
        .collect();
    assert_eq!(ended, [pref("m9", "ann@example.invalid").as_str()]);
}

/// Two live rows naming one fleet node (any writer can set the cell) adopt neither: the known
/// Mac's label, status and aliases are written nowhere, the clash is a finding, and the generic
/// check reports `fleet_node` as a unique clash.
/// NEGATIVE CONTROL: with `known_found` taking the first row that names the node (`[pk, ..] =>`
/// in place of `[pk] =>`), the roster's label lands on that row: `` assertion failed:
/// plan.planned.iter().all(|p| p.pk != first && p.pk != second) ``.
#[test]
fn two_rows_naming_one_fleet_node_adopt_neither() {
    let node = "0011223344556677";
    let (first, second) = (keys::machine_pk("m12"), keys::machine_pk("m40"));
    let mut book = Book::new();
    for (pk, label) in [(&first, "m12"), (&second, "m40")] {
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, pk, "label", json!(label)),
                put(&MACHINE, pk, "node", json!(node)),
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
        sealer: &SEALER,
        now_ms: 0,
        births: false,
        unread: &[],
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
                if columns == &["node"])
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
    let node = "0011223344556677";
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
            node: Some("ffeeddccbbaa9988".to_owned()),
            hosts: Vec::new(),
        },
    ];
    let adopt = Input {
        sheets: &[],
        macs: &macs,
        vendors: &Stand,
        sealer: &SEALER,
        now_ms: 0,
        births: false,
        unread: &[],
    };
    let plan = plan(&adopt, &cells).unwrap();
    assert_eq!(planned(&plan, "machine", &row, "node"), None);
    assert_eq!(
        planned(&plan, "machine", &row, "status"),
        Some(&json!("active"))
    );
    assert_eq!(
        planned(&plan, "machine", &row, "aliases.m70"),
        Some(&json!(true))
    );
    let stranger = keys::machine_pk("n-ffeeddccbbaa9988");
    assert!(plan.planned.iter().all(|p| p.pk != stranger));
    assert!(kinds(&plan).contains(&"unadopted"));
    let src = planned(&plan, "machine", &row, "src").unwrap();
    assert_eq!((&src["row"], &src["col"]), (&json!(1), &json!(0)));
    // The import leaves node publication to the adopting Mac.
    let mut book2 = book;
    let after = apply(&mut book2, &plan);
    let sources = cell::project(&SOURCE, &after);
    let roster = &sources.rows[src["source"].as_str().unwrap()];
    assert_eq!(roster.get("kind"), Some(&json!("swarm-roster")));
    assert_eq!(roster.get("rows"), Some(&json!(2)));
    let seen = book2.all();
    book2.write(2, &seen, vec![put(&MACHINE, &row, "node", json!(node))]);
    let after = book2.cells();
    // Once the Mac publishes its own node, the next adopt finds it by that node.
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
    let plan = plan(&input(&[sheet, other], &[]), &Cells::new()).unwrap();
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

/// An address row is matched by its key, and the table wins: re-importing the sheet a row was
/// born from after the owner renamed it writes no new row and reports the row's `addr`
/// owner-kept, the cell still saying the new address; a sheet saying the new address names the
/// same row (nothing under the new address's key, the row not reported no-longer-listed); and an
/// address two rows were renamed onto, with no row under its key, is `address-ambiguous` and
/// nothing of its sheet row is written.
/// NEGATIVE CONTROL: with `Planner::address` keyed by `keys::address_pk` alone (as before), the
/// sheet saying the new address births a second row: `` left: 1 right: 0 ``.
#[test]
fn a_sheet_matches_a_renamed_row_by_key_and_the_table_keeps_its_addr() {
    let new_rows = |plan: &Plan| plan.counts.get(ADDRESS.name).map_or(0, |c| c.new_rows);
    let first = vault(&[
        ",ann@example.invalid,,,,,,,,,,,",
        ",bob@example.invalid,,,,,,,,,,,",
    ]);
    let mut book = Book::new();
    let one = plan(&input(std::slice::from_ref(&first), &[]), &Cells::new()).unwrap();
    apply(&mut book, &one);
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(
            &ADDRESS,
            &ann,
            "addr",
            json!("ann.new@example.invalid"),
        )],
    );
    let cells = book.cells();
    // The sheet the row was born from, again.
    let again = plan(&input(std::slice::from_ref(&first), &[]), &cells).unwrap();
    assert_eq!(new_rows(&again), 0);
    assert!(
        again.findings.iter().any(|f| f.kind == Kind::OwnerKept
            && f.pk.as_deref() == Some(ann.as_str())
            && f.column.as_deref() == Some("addr")),
        "{:?}",
        again.findings
    );
    let after = apply(&mut book, &again);
    assert_eq!(
        cell::project(&ADDRESS, &after).rows[&ann].get("addr"),
        Some(&json!("ann.new@example.invalid"))
    );
    // A sheet saying the new address.
    let renamed = vault(&[
        ",ann.new@example.invalid,,,,,,,,,,,",
        ",bob@example.invalid,,,,,,,,,,,",
    ]);
    let next = plan(&input(&[renamed], &[]), &after).unwrap();
    assert_eq!(new_rows(&next), 0);
    let new = keys::address_pk("ann.new@example.invalid").unwrap();
    assert!(next.planned.iter().all(|p| p.pk != new));
    assert!(
        !next
            .findings
            .iter()
            .any(|f| f.kind == Kind::NoLongerListed && f.pk.as_deref() == Some(ann.as_str())),
        "{:?}",
        next.findings
    );
    // Two rows renamed onto one address no row is keyed by.
    let (cy, dan) = (
        keys::address_pk("cy@example.invalid").unwrap(),
        keys::address_pk("dan@example.invalid").unwrap(),
    );
    let mut owned = Book::new();
    owned.write(
        2,
        &[],
        vec![
            put(&ADDRESS, &cy, "addr", json!("same@example.invalid")),
            put(&ADDRESS, &cy, "row", json!("live")),
            put(&ADDRESS, &dan, "addr", json!("same@example.invalid")),
            put(&ADDRESS, &dan, "row", json!("live")),
        ],
    );
    let both = vault(&[",same@example.invalid,m3,,1,,,,,,,,"]);
    let held = plan(&input(&[both], &[]), &owned.cells()).unwrap();
    assert!(
        kinds(&held).contains(&"address-ambiguous"),
        "{:?}",
        kinds(&held)
    );
    assert!(
        held.planned
            .iter()
            .all(|p| p.collection != ADDRESS.name && p.collection != ACCOUNT.name),
        "{:?}",
        held.planned
    );
}

/// A renamed row's account is keyed from the address the row says in the table, as the view and
/// the harness name it: re-importing the sheet the row was born from, which still names its birth
/// address and now gives it a Claude Code account, plans the account of the row's new address
/// with that row as its login, and no account keyed from the address the row no longer says.
/// NEGATIVE CONTROL: with `Planner::account` keyed from the sheet's address
/// (`keys::account_pk(realm, address)`, as before), the new address's account is never planned:
/// `` assertion `left == right` failed left: None right: Some(String("ad-…")) ``.
#[test]
fn a_renamed_row_s_account_is_keyed_from_the_address_the_table_says() {
    let first = vault(&[",ann@example.invalid,,,,,,,,,,,"]);
    let mut book = Book::new();
    let one = plan(&input(std::slice::from_ref(&first), &[]), &Cells::new()).unwrap();
    apply(&mut book, &one);
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(
            &ADDRESS,
            &ann,
            "addr",
            json!("ann.new@example.invalid"),
        )],
    );
    let joined = vault(&[",ann@example.invalid,,,1,,,,,,,,"]);
    let again = plan(&input(&[joined], &[]), &book.cells()).unwrap();
    let new = keys::account_pk("claude", "ann.new@example.invalid").unwrap();
    let old = keys::account_pk("claude", "ann@example.invalid").unwrap();
    assert_eq!(planned(&again, "account", &new, "login"), Some(&json!(ann)));
    assert!(
        again.planned.iter().all(|p| p.pk != old),
        "{:?}",
        again.planned
    );
    let after = apply(&mut book, &again);
    let view = crate::alab::view::LabView::read(
        &after,
        &crate::typed::owned::OwnedRead::default(),
        &crate::typed::owned::OwnedRead::default(),
        None,
    );
    let account = &view.accounts[&new];
    assert_eq!(account.login.as_deref(), Some("ann.new@example.invalid"));
}

/// A stated file is named by its file name alone, and a name that names no file is refused.
#[test]
fn a_stated_file_is_named_by_its_file_name() {
    let win = Sheet::stated(
        SheetKind::FleetAccounts,
        r"C:\Users\owner\.secrets\fleet-accounts.toml",
        b"",
        &[],
        &KEY,
    )
    .unwrap();
    assert_eq!(win.name, "fleet-accounts.toml");
    assert!(Sheet::stated(SheetKind::FleetAccounts, "   ", b"", &[], &KEY).is_err());
    assert!(Sheet::stated(SheetKind::AiPool, "", b"x", &[], &KEY).is_err());
    assert!(Sheet::stated(SheetKind::AiPool, "dir/", b"x", &[], &KEY).is_err());
    let stated = Sheet::stated(SheetKind::AiPool, "ai-pool.toml", b"x", &[], &KEY).unwrap();
    assert_eq!(
        stated.text(&Coord {
            sheet: 0,
            source: stated.source.clone(),
            row: 0,
            col: 1,
        }),
        None
    );
}

/// When both sheets enrich one row (the vault sheet births the address and Google account, and the
/// logins sheet adds `mailbox_hint`, `mailbox` and `services.agy`), each cell keeps the source of
/// the sheet that stated it, so a later single-sheet import of the vault sheet alone leaves the
/// logins sheet's cells intact, and an empty-row sheet that ends a preference still records its
/// `source` row, so the cell it wrote names a source that exists.
/// NEGATIVE CONTROL: with `finish` stamping every cell on a row with `want.src` (`row_source` in
/// place of `wanted.at.source` and `at.source`), the vault-only re-import retracts the logins
/// sheet's `mailbox_hint`, `mailbox` and `services.agy`: `` left: Some(Null) right:
/// Some(String("mb-…")) ``.
#[test]
fn per_cell_provenance_preserves_cross_sheet_cells_and_empty_sheet_preference_endings() {
    let vault_one = vault(&[",ann@example.invalid,m3,,,,1,,,,,,"]);
    let logins_one =
        logins(&["m3,ann@example.invalid,google/agy,box@proton.example,lead,real@proton.example"]);
    let mut book = Book::new();
    let combined = plan(&input(&[vault_one.clone(), logins_one], &[]), &Cells::new()).unwrap();
    let cells = apply(&mut book, &combined);

    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let google = keys::account_pk("google", "ann@example.invalid").unwrap();
    let hint = cell::project(&ADDRESS, &cells).rows[&ann]
        .get("mailbox_hint")
        .unwrap()
        .clone();
    let mbox = cell::project(&ADDRESS, &cells).rows[&ann]
        .get("mailbox")
        .unwrap()
        .clone();
    assert!(hint.as_str().is_some_and(|pk| keys::is_digest_pk(pk, "mb")));
    assert!(mbox.as_str().is_some_and(|pk| keys::is_digest_pk(pk, "mb")));

    // Re-importing only an edited vault sheet must NOT wipe mailbox_hint, mailbox, or services.agy.
    let vault_two = vault(&[",ann@example.invalid,m4,,,,1,,,,,,"]);
    let vault_only = plan(&input(&[vault_two], &[]), &cells).unwrap();
    let after_vault = apply(&mut book, &vault_only);
    let addresses = cell::project(&ADDRESS, &after_vault);
    assert_eq!(addresses.rows[&ann].get("mailbox_hint"), Some(&hint));
    assert_eq!(addresses.rows[&ann].get("mailbox"), Some(&mbox));
    let accounts = cell::project(&ACCOUNT, &after_vault);
    assert_eq!(
        accounts.rows[&google].fields["services"].members(),
        BTreeSet::from(["agy", "gemini-web"])
    );

    // Next, re-importing only an edited logins sheet that drops `agy` and `mailbox_hint` retracts them.
    let logins_two = logins(&["m4,ann@example.invalid,google,,,real@proton.example"]);
    let logins_only = plan(&input(&[logins_two], &[]), &after_vault).unwrap();
    let after_logins = apply(&mut book, &logins_only);
    assert_eq!(
        cell::project(&ADDRESS, &after_logins).rows[&ann].get("mailbox_hint"),
        Some(&Value::Null)
    );
    assert_eq!(
        cell::project(&ADDRESS, &after_logins).rows[&ann].get("mailbox"),
        Some(&mbox)
    );
    assert_eq!(
        cell::project(&ACCOUNT, &after_logins).rows[&google].fields["services"].members(),
        BTreeSet::from(["gemini-web"])
    );

    // Empty-row vault sheet ending m4's preference still writes its `source` row.
    let empty_vault = Sheet::parse(
        SheetKind::Vault,
        r"C:\dir\ai-accounts.csv",
        format!("{VAULT_HEADER}\n").as_bytes(),
    )
    .unwrap();
    assert_eq!(empty_vault.name, "ai-accounts.csv");
    let ending = plan(
        &input(std::slice::from_ref(&empty_vault), &[]),
        &after_logins,
    )
    .unwrap();
    let after_ending = apply(&mut book, &ending);
    let m4_google = keys::preference_pk(&keys::machine_pk("m4"), &google).unwrap();
    assert_eq!(
        cell::project(&PREFERENCE, &after_ending).rows[&m4_google].get(crate::typed::LIFECYCLE),
        Some(&json!("ended"))
    );
    assert!(
        cell::project(&SOURCE, &after_ending)
            .rows
            .contains_key(&empty_vault.source),
        "ending a preference from a header-only sheet still records its source row"
    );

    // Path-only or empty sheet names and malformed KnownMac roster entries never crash `plan`.
    assert!(
        Sheet::parse(
            SheetKind::Vault,
            "dir/",
            format!("{VAULT_HEADER}\n").as_bytes()
        )
        .is_err()
    );
    let noisy_macs = [
        KnownMac {
            label: None,
            aliases: Vec::new(),
            node: Some("not-a-32-hex-node".to_owned()),
            hosts: vec!["bad\nhost".to_owned()],
        },
        KnownMac {
            label: Some(Label::plain(77)),
            aliases: Vec::new(),
            node: Some("0123456789ABCDEF".to_owned()),
            hosts: vec![
                "m77.local".to_owned(),
                "m77.local".to_owned(),
                "x".repeat(80),
                "ops@example.invalid".to_owned(),
            ],
        },
    ];
    let roster_plan = plan(&input(&[], &noisy_macs), &Cells::new()).unwrap();
    let m77 = keys::machine_pk("n-0123456789abcdef");
    assert_eq!(planned(&roster_plan, "machine", &m77, "node"), None);
    assert_eq!(
        planned(&roster_plan, "machine", &m77, "hosts"),
        Some(&json!(["m77.local"]))
    );
}

/// A run that reads no vault sheet leaves what the vault sheet said about a row: a paused account
/// stays paused under a harness file or a logins sheet, an address keeps its origin, and a label
/// the owner gave is no finding. Pausing is what keeps a sign-in from being tried, so losing it to
/// a lower source would be a real harm.
/// NEGATIVE CONTROL: with `birth_status` ignoring what is held (`if !wanted {`), the harness file
/// plans over the import's `paused`: `` left: Some(String("pending")) right: None ``.
#[test]
fn a_lower_source_leaves_what_the_vault_sheet_said() {
    let first = vault(&[",ann@example.invalid,m3,,1,,,,,,claude banned,,"]);
    let mut book = Book::new();
    let one = plan(&input(&[first], &[]), &Cells::new()).unwrap();
    apply(&mut book, &one);
    let account = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&ACCOUNT, &account, "label", json!("research"))],
    );
    let cells = book.cells();
    let text = "claude,ann@example.invalid,m3,m3";
    let rows = Stand
        .stated(SheetKind::FleetAccounts, "fleet-accounts.toml", text)
        .unwrap();
    let fleet = Sheet::stated(
        SheetKind::FleetAccounts,
        "fleet-accounts.toml",
        text.as_bytes(),
        &rows,
        &KEY,
    )
    .unwrap();
    let other = logins(&["m3,ann@example.invalid,claude,,x,"]);
    for sheet in [fleet, other] {
        let next = plan(&input(&[sheet], &[]), &cells).unwrap();
        assert_eq!(planned(&next, "account", &account, "status"), None);
        assert_eq!(planned(&next, "address", &ann, "origin"), None);
        assert_eq!(planned(&next, "account", &account, "label"), None);
        assert!(
            !next
                .findings
                .iter()
                .any(|f| f.column.as_deref() == Some("label")),
            "{:?}",
            next.findings
        );
    }
}

/// The stated rows of one harness file.
fn harness_file(kind: SheetKind, name: &str, text: &str) -> Sheet {
    let rows = Stand.stated(kind, name, text).unwrap();
    Sheet::stated(kind, name, text.as_bytes(), &rows, &KEY).unwrap()
}

/// A note with no secret word from the old list still never crosses, on the address or on a
/// paused account, and its coordinate stays.
/// NEGATIVE CONTROL: with both of `note_text`'s checks using `guard::text`, `Hunter2` crosses: this test's
/// `` the password crossed `` message.
#[test]
fn a_note_that_reads_as_a_password_never_crosses() {
    let sheet = vault(&[
        ",ann@example.invalid,m3,,1,,,,,,pass: Hunter2!,,",
        ",bob@example.invalid,m3,,1,,,,,,claude banned,pw Hunter2!,",
    ]);
    let plan = plan(&input(&[sheet], &[]), &Cells::new()).unwrap();
    assert!(
        !values(&plan).join("\n").contains("Hunter2"),
        "the password crossed"
    );
    let withheld = plan
        .findings
        .iter()
        .filter(|f| f.kind == Kind::NoteWithheld)
        .count();
    assert_eq!(withheld, 2);
    let bob = keys::address_pk("bob@example.invalid").unwrap();
    assert_eq!(
        planned(&plan, "address", &bob, "note"),
        Some(&json!("claude banned"))
    );
}

/// A retraction is written under the file that wrote the cell, whichever file of its kind the run
/// read first, so an edited fleet file's ending names that file.
/// NEGATIVE CONTROL: with `retract_source` matching the kind alone, the ending names the Codex file
/// read first: the `assert_eq!` on the ended cell's source fails.
#[test]
fn a_retraction_names_the_file_that_wrote_the_cell() {
    let claude = |text: &str| harness_file(SheetKind::FleetAccounts, "fleet-accounts.toml", text);
    let codex = harness_file(
        SheetKind::FleetAccounts,
        "ai-accounts-codex.toml",
        "codex,cod@example.invalid,m5-2,m5",
    );
    let mut book = Book::new();
    let first = [
        codex.clone(),
        claude("claude,ann@example.invalid,,m3\nclaude,bob@example.invalid,,m3"),
    ];
    let cells = apply(
        &mut book,
        &plan(&input(&first, &[]), &Cells::new()).unwrap(),
    );
    let edited = claude("claude,ann@example.invalid,,m3");
    let source = edited.source.clone();
    let second = [codex, edited];
    let after = apply(&mut book, &plan(&input(&second, &[]), &cells).unwrap());
    let preferences = cell::project(&PREFERENCE, &after);
    let bob = &preferences.rows[&pref("m3", "bob@example.invalid")];
    assert_eq!(bob.get(crate::typed::LIFECYCLE), Some(&json!("ended")));
    let Some(Field::Value(ended)) = bob.fields.get(crate::typed::LIFECYCLE) else {
        panic!("the row cell is one value");
    };
    assert_eq!(ended.value.s.as_deref(), Some(source.as_str()));
    assert!(cell::project(&SOURCE, &after).rows.contains_key(&source));
}

/// Each fleet file is its own source: naming one file, or reading one beside another that failed,
/// ends nothing the other stated, and a file read that dropped a row still ends its own.
/// NEGATIVE CONTROL: with `ReadNow::covers` testing the kind alone, the rank the unread Codex file
/// wrote is withdrawn: `` left: Some(Null) right: None ``.
#[test]
fn a_fleet_file_retracts_only_what_it_stated() {
    let claude = |text: &str| harness_file(SheetKind::FleetAccounts, "fleet-accounts.toml", text);
    let codex = harness_file(
        SheetKind::FleetAccounts,
        "ai-accounts-codex.toml",
        "codex,cod@example.invalid,m5-2,m5",
    );
    let both = [
        claude("claude,ann@example.invalid,,m3\nclaude,bob@example.invalid,,m3"),
        codex,
    ];
    let mut book = Book::new();
    let cells = apply(&mut book, &plan(&input(&both, &[]), &Cells::new()).unwrap());
    let ended = |plan: &Plan| {
        plan.planned
            .iter()
            .filter(|p| p.x == json!("ended"))
            .count()
    };
    // The Claude file now also lists Cod's mapping, with no rank: the rank the Codex file wrote,
    // and that file is not read, stands.
    let shorter = [claude(
        "claude,ann@example.invalid,,m3\ncodex,cod@example.invalid,,m5",
    )];
    let only_named = plan(&input(&shorter, &[]), &cells).unwrap();
    assert_eq!(
        only_named
            .planned
            .iter()
            .filter(|p| p.x == json!("ended"))
            .map(|p| p.pk.clone())
            .collect::<Vec<_>>(),
        [pref("m3", "bob@example.invalid")],
    );
    let cod = keys::preference_pk(
        &keys::machine_pk("m5"),
        &keys::account_pk("codex", "cod@example.invalid").unwrap(),
    )
    .unwrap();
    assert_eq!(planned(&only_named, "preference", &cod, "rank"), None);
    // The same run with a fleet file that failed to parse ends nothing.
    let failed = Input {
        unread: &[SheetKind::FleetAccounts],
        ..input(&shorter, &[])
    };
    assert_eq!(ended(&plan(&failed, &cells).unwrap()), 0);
}

/// Any correction stops the `+tag` guess, and a run with no logins sheet neither guesses nor
/// withdraws a link, since it cannot see the correction.
/// NEGATIVE CONTROL: with `aliases` ignoring `corrected`, the self-mailbox row links to its base:
/// `` left: Some(String(...)) right: None ``.
#[test]
fn a_correction_stops_the_tag_guess() {
    let sheet = logins(&[
        "m3,ann@example.invalid,claude,,x,",
        "m4,ann+self@example.invalid,claude,,x,ann+self@example.invalid",
        "m5,ann+far@example.invalid,claude,,x,box@proton.example",
        "m6,ann+words@example.invalid,claude,,x,forwarded by hand",
        "m7,ann+plain@example.invalid,claude,,x,",
    ]);
    let first = plan(&input(std::slice::from_ref(&sheet), &[]), &Cells::new()).unwrap();
    let link = |plan: &Plan, address: &str, column: &str| {
        planned(plan, "address", &keys::address_pk(address).unwrap(), column).cloned()
    };
    let base = json!(keys::address_pk("ann@example.invalid").unwrap());
    for address in [
        "ann+self@example.invalid",
        "ann+far@example.invalid",
        "ann+words@example.invalid",
    ] {
        assert_eq!(link(&first, address, "alias_of"), None, "{address}");
        assert_eq!(link(&first, address, "kind"), None, "{address}");
    }
    assert_eq!(
        link(&first, "ann+plain@example.invalid", "alias_of"),
        Some(base)
    );

    // The vault sheet alone: no guess for a new `+tag`, no retraction of the one a logins run
    // made through this vault listing.
    let listing = vault(&[
        ",ann@example.invalid,m3,,1,,,,,,,,",
        ",ann+plain@example.invalid,m7,,1,,,,,,,,",
        ",ann+new@example.invalid,m8,,1,,,,,,,,",
    ]);
    let both = [listing.clone(), sheet];
    let mut book = Book::new();
    let cells = apply(&mut book, &plan(&input(&both, &[]), &Cells::new()).unwrap());
    let alone = plan(&input(&[listing], &[]), &cells).unwrap();
    assert_eq!(link(&alone, "ann+plain@example.invalid", "alias_of"), None);
    assert_eq!(link(&alone, "ann+plain@example.invalid", "kind"), None);
    assert_eq!(link(&alone, "ann+new@example.invalid", "alias_of"), None);
}

/// A file found and not read may state anything, so a sheet that drops a row ends nothing beside it.
/// NEGATIVE CONTROL: with `can_end` ignoring `input.unread`, the vault sheet's dropped row ends:
/// `` left: 1 right: 0 ``.
#[test]
fn an_unread_file_holds_every_ending() {
    let mut book = Book::new();
    let start = vault(&[
        ",ann@example.invalid,m3,,1,,,,,,,,",
        ",bob@example.invalid,m3,,1,,,,,,,,",
    ]);
    let cells = apply(
        &mut book,
        &plan(&input(&[start], &[]), &Cells::new()).unwrap(),
    );
    let dropped = [vault(&[",ann@example.invalid,m3,,1,,,,,,,,"])];
    let ended = |plan: &Plan| {
        plan.planned
            .iter()
            .filter(|p| p.x == json!("ended"))
            .count()
    };
    let held = Input {
        unread: &[SheetKind::FleetAccounts],
        ..input(&dropped, &[])
    };
    assert_eq!(ended(&plan(&held, &cells).unwrap()), 0);
    let whole = input(&dropped, &[]);
    assert_eq!(ended(&plan(&whole, &cells).unwrap()), 1);
}

/// A lower source's different rank is a finding and writes nothing, on a run that never read the
/// vault sheet, while a rank that source itself wrote it may still move.
/// NEGATIVE CONTROL: with `prefer` always wanting the rank (`held_above` returning false), the
/// fleet file rewrites it: `` left: Some(Number(2)) right: None ``.
#[test]
fn a_lower_source_never_moves_the_vault_sheets_rank() {
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &plan(
            &input(&[vault(&[",ann@example.invalid,m8.3,,1,,,,,,,,"])], &[]),
            &Cells::new(),
        )
        .unwrap(),
    );
    let on_m8 = pref("m8", "ann@example.invalid");
    for sheet in [
        harness_file(
            SheetKind::FleetAccounts,
            "fleet-accounts.toml",
            "claude,ann@example.invalid,m8-2,m8",
        ),
        logins(&["m8.2,ann@example.invalid,claude,,x,"]),
    ] {
        let next = plan(&input(&[sheet], &[]), &cells).unwrap();
        assert_eq!(planned(&next, "preference", &on_m8, "rank"), None);
        assert!(
            next.findings
                .iter()
                .any(|f| f.kind == Kind::SourcesDisagree && f.column.as_deref() == Some("rank")),
            "{:?}",
            next.findings
        );
    }
    let fleet = |label: &str| {
        harness_file(
            SheetKind::FleetAccounts,
            "fleet-accounts.toml",
            &format!("claude,bob@example.invalid,{label},m9"),
        )
    };
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &plan(&input(&[fleet("m9-2")], &[]), &Cells::new()).unwrap(),
    );
    let moved = plan(&input(&[fleet("m9-3")], &[]), &cells).unwrap();
    assert_eq!(
        planned(
            &moved,
            "preference",
            &pref("m9", "bob@example.invalid"),
            "rank"
        ),
        Some(&json!(3))
    );
}

/// Every machine a harness label names is a preference, whatever the row's list says.
/// NEGATIVE CONTROL: with the `named` loop in `harness` removed, m38 prefers nothing: this test's
/// `` m38 `` message.
#[test]
fn a_two_mac_label_in_a_harness_file_maps_both_macs() {
    let sheet = harness_file(
        SheetKind::AiPool,
        "ai-pool.toml",
        "claude,ann@example.invalid,m106-m38,m106\nclaude,bob@example.invalid,m7-m9,",
    );
    let plan = plan(&input(&[sheet], &[]), &Cells::new()).unwrap();
    for (machine, address) in [
        ("m106", "ann@example.invalid"),
        ("m38", "ann@example.invalid"),
        ("m7", "bob@example.invalid"),
        ("m9", "bob@example.invalid"),
    ] {
        assert!(
            planned(&plan, "preference", &pref(machine, address), "machine").is_some(),
            "{machine}"
        );
    }
}

/// One cell this run could not read says nothing against a row's mappings: a bad provider cell
/// keeps that login's, a held Kimi cell keeps its Kimi account's, and a bad address cell, which
/// could be any row's, ends nothing in the run, while a cell read as absent still ends its own.
/// NEGATIVE CONTROL: with `can_end` true whatever the run missed, the typo ends ann's: this test's
/// `` a typo ended `` message.
#[test]
fn a_cell_this_run_could_not_read_ends_nothing() {
    let start = vault(&[
        ",ann@example.invalid,m3,,1,,,,,,,,",
        ",bob@example.invalid,m3,,1,,,,,,,,",
        ",cy@example.invalid,m3,,1,,,,,,,,",
        ",kay@example.invalid,m3,,,,,,global,,,,",
    ]);
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &plan(&input(&[start], &[]), &Cells::new()).unwrap(),
    );
    let ended = |rows: &[&str]| {
        plan(&input(&[vault(rows)], &[]), &cells)
            .unwrap()
            .planned
            .iter()
            .filter(|p| p.x == json!("ended"))
            .map(|p| p.pk.clone())
            .collect::<Vec<_>>()
    };
    let (ann, bob, cy) = (
        ",ann@example.invalid,m3,,1,,,,,,,,",
        ",bob@example.invalid,m3,,1,,,,,,,,",
        ",cy@example.invalid,m3,,1,,,,,,,,",
    );
    let kay = ",kay@example.invalid,m3,,,,,,global,,,,";
    assert_eq!(ended(&[ann, bob, cy, kay]), Vec::<String>::new());
    assert_eq!(
        ended(&["x,not-an-address,m3,,1,,,,,,,,", bob, cy, kay]),
        Vec::<String>::new(),
        "a typo ended"
    );
    assert_eq!(
        ended(&[ann, ",bob@example.invalid,m3,,1?,,,,,,,,", cy, kay]),
        Vec::<String>::new(),
        "an unread provider cell ended"
    );
    assert_eq!(
        ended(&[ann, bob, cy, ",kay@example.invalid,m3,,,,,,1,,,,"]),
        Vec::<String>::new(),
        "a held cell ended"
    );
    // The control: a row whose cell plainly says no account ends its mapping.
    assert_eq!(
        ended(&[ann, bob, ",cy@example.invalid,m3,,,,,,,,,,", kay]),
        [pref("m3", "cy@example.invalid")]
    );
}

#[test]
fn computers_emails_and_phones_sheets_import_and_protect_private_addresses() {
    let phones = Sheet::parse(
        SheetKind::Phones,
        "alab-phones.csv",
        b"Phone Number,Carrier / Provider,Line Type,Associated Device / Person,Notes\n\
          415-555-0199,Tello Mobile,eSIM,Desk Phone,Primary test line\n",
    )
    .unwrap();
    let computers = Sheet::parse(
        SheetKind::Computers,
        "alab-computers.csv",
        b"ID,Make,Arch,OS,Processor,RAM,HD,Aliases,Nickname,Nickname Aliases,Location,Notes\n\
          m2,Apple,arm64,macOS,M3 Max,64,2000,\"m2.2, m2.3\",fb,redballoon,SF,Desk Mac\n",
    )
    .unwrap();
    let emails = Sheet::parse(
        SheetKind::Emails,
        "alab-emails.csv",
        b"Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n\
          private@example.invalid,,secret-ignored,Active,gmail,415-555-0199,Protected (Real Email),Personal real email\n\
          root@proton.example,,secret-ignored,Active,proton,,,Root proton mailbox\n\
          alias1@proton.example,root@proton.example,,Active,proton,415-555-0199,,First alias\n",
    )
    .unwrap();
    let mut book = Book::new();
    let plan1 = plan(&input(&[phones, computers, emails], &[]), &Cells::new()).unwrap();
    assert!(!kinds(&plan1).contains(&"credential-shaped"));
    let cells = apply(&mut book, &plan1);
    let (lab_findings, generic_findings) = crate::alab::checks::lab(&cells);
    assert!(lab_findings.is_empty(), "{lab_findings:?}");
    assert!(generic_findings.is_empty(), "{generic_findings:?}");

    let m2_pk = keys::machine_pk("m2");
    assert_eq!(
        planned(&plan1, "machine", &m2_pk, "nickname"),
        Some(&json!("fb"))
    );
    assert_eq!(
        planned(&plan1, "machine", &m2_pk, "ram_gb"),
        Some(&json!(64))
    );
    let priv_pk = keys::address_pk("private@example.invalid").unwrap();
    assert_eq!(
        planned(&plan1, "address", &priv_pk, "protected"),
        Some(&json!(true))
    );
    assert_eq!(planned(&plan1, "address", &priv_pk, "mailbox"), None);

    let alias_pk = keys::address_pk("alias1@proton.example").unwrap();
    let root_pk = keys::address_pk("root@proton.example").unwrap();
    assert_eq!(
        planned(&plan1, "address", &alias_pk, "alias_of"),
        Some(&json!(root_pk))
    );
    assert!(planned(&plan1, "address", &alias_pk, "mailbox").is_some());
    assert!(planned(&plan1, "address", &alias_pk, "phone").is_some());
}

/// A `~/.secrets` harness file's source is keyed under the `[everyone]` key, tokens or not: no
/// plain digest of its bytes (nor its first 8 bytes, the source key) reaches the shared `source`
/// row, while a one-byte change is still a new source and another epoch's key names it anew.
/// NEGATIVE CONTROL: with `keyed_source` a plain `Sha256::digest(bytes)` (the key ignored), the
/// plain-digest assertion fails: `` assertion `left != right` failed ``.
#[test]
fn a_secrets_file_source_is_a_keyed_digest() {
    let text = "claude,ann@example.invalid,,m3 m9\n";
    let read = |text: &str, key: &[u8; 32]| {
        let rows = Stand
            .stated(SheetKind::AiPool, "ai-pool.toml", text)
            .unwrap();
        Sheet::stated(
            SheetKind::AiPool,
            "ai-pool.toml",
            text.as_bytes(),
            &rows,
            key,
        )
        .unwrap()
    };
    let one = read(text, &KEY);
    assert_ne!(
        one.sha256,
        hex::encode_lower(&Sha256::digest(text.as_bytes()))
    );
    assert_ne!(one.source, keys::source_pk(text.as_bytes()));
    assert_eq!(one.sha256.len(), 64);
    assert!(one.source.starts_with("sr-") && one.sha256.starts_with(&one.source[3..]));
    let changed = read(&text.replace("m9", "m8"), &KEY);
    assert_ne!(changed.source, one.source);
    assert_ne!(read(text, &[0xc3; 32]).sha256, one.sha256);
    assert_eq!(read(text, &KEY).source, one.source);
}
