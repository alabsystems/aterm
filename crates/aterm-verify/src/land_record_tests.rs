// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::checks::Check;
use crate::event_log::PlannedCheck;

/// A judged tree.
pub(crate) const T: &str = "1111111111111111111111111111111111111111";
/// Another tree.
pub(crate) const T2: &str = "2222222222222222222222222222222222222222";
/// The main commit both merged.
pub(crate) const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// An older main commit.
pub(crate) const OLD_BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The commit judged.
pub(crate) const COMMIT: &str = "c0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ff";
/// A UTC day, and a time in it.
pub(crate) const DAY: u64 = 20_404;
pub(crate) const NOW: u64 = DAY * 86_400 + 43_200;
/// A finding of a crate outside the branch's.
pub(crate) const FINDING: &str = "-p aterm-uds --lib -- tests::one";

/// The fixture node `seed` names.
pub(crate) fn signer(seed: u8) -> aterm_net::fleet::NodeIdentity {
    aterm_net::fleet::NodeIdentity::from_seed("0123456789abcdef0123456789abcdef", &[seed; 32])
        .unwrap()
}

/// A run's plan of `tree` at `base`: lint, test, cells and fmt keyed over fixed parts and seals.
pub(crate) fn planned(tree: &str, base: &str) -> Planned {
    let required = [Check::Fmt, Check::Lint, Check::Test, Check::Cells]
        .into_iter()
        .map(|check| {
            let part = crate::land_seal::digest(["part", check.name(), base]);
            let seal = crate::land_seal::digest(["seal", check.name()]);
            PlannedCheck {
                check,
                key: crate::land_seal::key(check, tree, &part, &seal),
                part,
                seal,
                fields: [("triple".to_owned(), "sha256:triple".to_owned())].into(),
            }
        })
        .collect();
    Planned {
        commit: COMMIT.into(),
        tree: tree.into(),
        base: base.into(),
        clean: true,
        required,
        local: crate::land_seal::digest(["local"]),
        local_fields: [("os".to_owned(), "sha256:os".to_owned())].into(),
        plan_digest: crate::land_seal::digest(["plan"]),
        unsealed: Vec::new(),
    }
}

/// The row of `check` saying `word` under `plan`'s key: a FAIL names [`FINDING`], and the
/// crate-scoped checks judged `aterm-gui`.
pub(crate) fn row(check: Check, word: Word, plan: &Planned) -> CheckRow {
    let fail = word == Word::Fail;
    CheckRow {
        check: check.name().into(),
        word: word.spelled().into(),
        detail: format!("{} said {}", check.name(), word.spelled()),
        findings: if fail {
            vec![FINDING.into()]
        } else {
            Vec::new()
        },
        findings_total: usize::from(fail),
        key: plan.row(check).map(|row| row.key.clone()),
        scope: check
            .crate_scoped()
            .then(|| (vec!["aterm-gui".to_owned()], false)),
        took: None,
        stopped: false,
    }
}

/// A whole, sealed-through run that ended at `ended`.
pub(crate) fn run(ended: u64) -> Run {
    Run {
        id: "0123456789abcdef0123456789abcdef".into(),
        started: ended - 600,
        ended,
        secs: Some(600),
        whole: Some(true),
        sealed_through: Some(true),
    }
}

/// The record of `check` saying `word` on `tree` at `base`, judged by host `m45` in a run that
/// ended at `ended`.
pub(crate) fn record(check: Check, word: Word, tree: &str, base: &str, ended: u64) -> Record {
    let plan = planned(tree, base);
    let at = Judging {
        commit: COMMIT,
        tree,
        base,
        host: "m45",
        planned: Some(&plan),
    };
    Record::of_row(&row(check, word, &plan), &at, run(ended)).unwrap()
}

/// The path `signed` would have under `tree`.
fn moved(signed: &Signed, tree: &str) -> String {
    signed.path.replace(&signed.filed().unwrap().tree, tree)
}

/// A record is read at its own path only: a body that judges another tree than its path names,
/// or whose key is not its name's or does not recompute from its check, tree, part and seal, is
/// unreadable, though its name still says a FAIL of the tree its path names. CONTROL: at its own
/// path it reads, signed by its key. NEGATIVE CONTROLS, each run as a mutant that made this test
/// fail: drop `read_body`'s tree check (the moved FAIL reads), and drop the key recompute (the
/// re-keyed body reads).
#[test]
fn a_record_filed_under_another_tree_or_key_is_unreadable() {
    let me = signer(7);
    let fail = record(Check::Lint, Word::Fail, T2, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    let at_home = Entry::read(&fail.path, &fail.bytes, Origin::Journal);
    let read = at_home
        .body
        .as_ref()
        .expect("CONTROL: at its own path it reads");
    assert_eq!(read.signature.as_ref().map(|s| s.key), Some(me.key()));

    let elsewhere = Entry::read(&moved(&fail, T), &fail.bytes, Origin::Journal);
    let why = elsewhere.body.as_ref().unwrap_err();
    assert!(why.contains("judges tree"), "{why}");
    let named = elsewhere.filed.as_ref().expect("its name parses");
    assert_eq!(
        (named.tree.as_str(), &named.name.word),
        (T, &Ok(Word::Fail)),
        "its name says a FAIL of the tree its path names"
    );
    assert_eq!(at_home.tree(), Some(T2));

    let mut rekeyed = record(Check::Lint, Word::Ok, T, BASE, NOW);
    rekeyed.key = "f".repeat(64);
    let signed = rekeyed.sign(Some(&me)).unwrap();
    let why = Entry::read(&signed.path, &signed.bytes, Origin::Journal)
        .body
        .unwrap_err();
    assert!(why.contains("does not recompute"), "{why}");

    let ok = record(Check::Lint, Word::Ok, T, BASE, NOW)
        .sign(Some(&me))
        .unwrap();
    let renamed = ok
        .path
        .replace(&ok.filed().unwrap().name.key, &"e".repeat(64));
    let why = Entry::read(&renamed, &ok.bytes, Origin::Journal)
        .body
        .unwrap_err();
    assert!(why.contains("not its name's"), "{why}");
    assert!(
        Entry::read(&ok.path, &ok.bytes, Origin::Journal)
            .body
            .is_ok(),
        "CONTROL"
    );
}

/// A record reads only where its body holds what a line and a git argument take, filed on its own
/// day: a body whose run ended on another day than its path names (a record copied into a later
/// day directory would outlive its retention), whose base is an option spelled where an object id
/// goes or whose commit is no object id, or whose host or run id is no token (an escape a line
/// would print) is unreadable, its FAIL name still parsing. No writer writes one:
/// `Record::of_row` spells the host and the run id as tokens, and `Record::sign` refuses a record
/// longer than a reader reads. CONTROL: the record at its own day's path reads. NEGATIVE CONTROL,
/// run as a mutant that made this test fail: drop `read_body`'s day check (the copied record
/// reads).
#[test]
fn a_record_reads_only_on_its_own_day_with_ids_and_tokens() {
    let me = signer(7);
    let fail = record(Check::Lint, Word::Fail, T, BASE, NOW);
    let signed = fail.sign(Some(&me)).unwrap();
    assert!(
        Entry::read(&signed.path, &signed.bytes, Origin::Journal)
            .body
            .is_ok(),
        "CONTROL"
    );
    let later = signed
        .path
        .replace(&format!("land/{DAY}/"), &format!("land/{}/", DAY + 2));
    let copied = Entry::read(&later, &signed.bytes, Origin::Journal);
    let why = copied.body.as_ref().unwrap_err();
    assert!(why.contains("ended on day"), "{why}");
    let mut entries = vec![copied];
    for case in [
        "an option as its base",
        "a short commit",
        "an escape in its host",
        "a space in its run id",
    ] {
        let mut spoiled = fail.clone();
        match case {
            "an option as its base" => spoiled.base = "--output=/tmp/x".into(),
            "a short commit" => spoiled.commit = "c0ffee".into(),
            "an escape in its host" => spoiled.local.host = "m45\u{1b}[2J".into(),
            _ => spoiled.run.id = "run one".into(),
        }
        let signed = spoiled.sign(Some(&me)).unwrap();
        let entry = Entry::read(&signed.path, &signed.bytes, Origin::Journal);
        assert!(entry.body.is_err(), "{case}: {:?}", entry.body);
        entries.push(entry);
    }
    for entry in &entries {
        let filed = entry.filed.as_ref().expect("its name parses");
        assert_eq!(filed.name.word, Ok(Word::Fail), "{}", entry.path);
    }

    let at = Judging {
        commit: COMMIT,
        tree: T,
        base: BASE,
        host: "m 45\n",
        planned: None,
    };
    let unspelled = Run {
        id: "run\u{7}".into(),
        ..run(NOW)
    };
    let written = Record::of_row(
        &row(Check::Lint, Word::Fail, &planned(T, BASE)),
        &at,
        unspelled,
    )
    .unwrap();
    assert_eq!(
        (written.local.host.as_str(), written.run.id.as_str()),
        ("m-45-", "run-")
    );
    let signed = written.sign(Some(&me)).unwrap();
    assert!(
        Entry::read(&signed.path, &signed.bytes, Origin::Journal)
            .body
            .is_ok(),
        "a writer's own record reads"
    );
    let mut long = fail;
    long.findings = vec!["x".repeat(1024); 80];
    let why = long.sign(Some(&me)).unwrap_err();
    assert!(why.contains("longer than"), "{why}");
}

/// What a reader makes of a record it may not trust: an unsigned record reads, naming no
/// signature; a body that does not parse, one of a newer schema, and a word this reader does not
/// know are unreadable, each name still parsing (the unknown word kept as it is spelled, so a
/// listing names it); a name that does not parse is named as such (its `filed` says why).
/// CONTROL: the same record signed reads under its signer's key. NEGATIVE CONTROL, run as a
/// mutant that made this test fail: let `read_body` skip the schema check (the newer body
/// reads).
#[test]
fn an_unsigned_record_reads_and_a_newer_or_unknown_one_is_named_unreadable() {
    let me = signer(7);
    let fail = record(Check::Lint, Word::Fail, T, BASE, NOW);
    let unsigned = fail.sign(None).unwrap();
    let key = fail.key.clone();
    let path = |word: &str| format!("land/{DAY}/{T}/lint.{key}.{word}.toml");
    let newer = String::from_utf8(unsigned.bytes.clone())
        .unwrap()
        .replace("schema = 1", "schema = 2");
    let entries = [
        Entry::read(&unsigned.path, &unsigned.bytes, Origin::Journal),
        Entry::read(&path("FAIL"), b"this is { not toml", Origin::Spool),
        Entry::read(&path("FAIL"), newer.as_bytes(), Origin::Journal),
        Entry::read(&path("ERROR"), &unsigned.bytes, Origin::Journal),
    ];
    let read = entries[0].body.as_ref().expect("an unsigned FAIL reads");
    assert_eq!(read.signature, None);
    assert!(
        entries[1]
            .body
            .as_ref()
            .unwrap_err()
            .contains("does not parse"),
        "{:?}",
        entries[1].body
    );
    assert!(
        entries[2]
            .body
            .as_ref()
            .unwrap_err()
            .contains("schema is 2"),
        "{:?}",
        entries[2].body
    );
    let unknown = entries[3].filed.as_ref().expect("its name parses");
    assert_eq!(unknown.name.word, Err("ERROR".to_owned()));
    assert!(
        entries[3]
            .body
            .as_ref()
            .unwrap_err()
            .contains("its name ERROR"),
        "{:?}",
        entries[3].body
    );
    for entry in &entries {
        assert_eq!(entry.tree(), Some(T), "{}", entry.path);
    }
    for malformed in [
        format!("land/{DAY}/{T}/lint.FAIL.toml"),
        format!("land/{DAY}/{T}/lint.{}.FAIL.toml", "X".repeat(64)),
    ] {
        let entry = Entry::read(&malformed, &unsigned.bytes, Origin::Journal);
        assert!(entry.filed.is_err() && entry.body.is_err(), "{entry:?}");
    }
    let signed = fail.sign(Some(&me)).unwrap();
    let read = Entry::read(&signed.path, &signed.bytes, Origin::Journal)
        .body
        .expect("CONTROL: signed, it reads");
    assert_eq!(read.signature.map(|s| s.key), Some(me.key()), "CONTROL");
}

/// A record is keyed only from a plan that judged its own tree from a clean checkout and keyed
/// its check under the row's own key: then its base, part, seal and local attributes are the
/// plan's, and its key the plan's. A dirty checkout's plan, another tree's plan, a row whose key
/// is not the plan's, and no plan at all each give an unkeyed record under the judged base, which
/// names no inputs. NEGATIVE CONTROL, run as a mutant that made this test fail: key a record from
/// a dirty checkout's plan (the dirty row comes out keyed).
#[test]
fn a_record_is_keyed_only_from_a_clean_plan_of_its_own_tree() {
    let clean = planned(T, OLD_BASE);
    let lint = row(Check::Lint, Word::Ok, &clean);
    fn at(planned: Option<&Planned>) -> Judging<'_> {
        Judging {
            commit: COMMIT,
            tree: T,
            base: BASE,
            host: "m45",
            planned,
        }
    }
    let keyed = Record::of_row(&lint, &at(Some(&clean)), run(NOW)).unwrap();
    assert!(keyed.keyed(), "CONTROL");
    let plan_row = clean.row(Check::Lint).unwrap();
    assert_eq!(keyed.key, plan_row.key);
    assert_eq!(keyed.key, keyed.expected_key());
    assert_eq!(
        (keyed.base.as_str(), keyed.local.digest.as_str()),
        (OLD_BASE, clean.local.as_str()),
        "the plan's base and local digest"
    );
    let dirty = Planned {
        clean: false,
        ..planned(T, OLD_BASE)
    };
    let other_tree = planned(T2, OLD_BASE);
    let rekeyed = CheckRow {
        key: Some("e".repeat(64)),
        ..lint.clone()
    };
    for (why, row, planned) in [
        ("a dirty checkout's plan", &lint, Some(&dirty)),
        ("another tree's plan", &lint, Some(&other_tree)),
        ("a row under another key", &rekeyed, Some(&clean)),
        ("no plan", &lint, None),
    ] {
        let record = Record::of_row(row, &at(planned), run(NOW)).unwrap();
        assert!(!record.keyed(), "{why}");
        assert_eq!(record.base, BASE, "{why}");
        assert!(record.local.digest.is_empty(), "{why}");
        assert_eq!(record.key, record.expected_key(), "{why}");
        assert_ne!(record.key, plan_row.key, "{why}");
    }
}
