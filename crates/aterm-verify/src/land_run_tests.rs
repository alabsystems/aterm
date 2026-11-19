// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The run records' names, signatures and reading, and the items they key.

use super::*;
use crate::land_record::tests::{BASE, COMMIT, DAY, FINDING, NOW, T, signer};

/// A run id.
pub(crate) const RUN: &str = "0123456789abcdef0123456789abcdef";

/// The run a record of [`RUN`] names: started an hour before [`NOW`] on host `mac`.
pub(crate) fn run() -> Run {
    Run {
        id: RUN.to_owned(),
        started: NOW - 3_600,
        host: "mac".to_owned(),
        node: None,
        session: Some("tab-1".to_owned()),
        memory_mib: Some(131_072),
        cores: Some(16),
        ..Run::default()
    }
}

/// A record of `kind`, the run's `seq`th, made at [`NOW`] of [`run`].
pub(crate) fn record(kind: Kind, seq: u32) -> Record {
    Record {
        schema: SCHEMA,
        kind: kind.word().to_owned(),
        seq,
        started: NOW - 600,
        at: NOW,
        secs: 600,
        head: COMMIT.to_owned(),
        base: BASE.to_owned(),
        tree: T.to_owned(),
        result: "pass".to_owned(),
        run: run(),
        ..Record::default()
    }
}

/// A run record files under its day, run, ordinal and kind, and its path reads back as that;
/// each other spelling is refused by name: no `land/runs` prefix, a day or an ordinal that is
/// not canonical decimal, a run that is no 32 hex digits, a kind no reader knows, a missing
/// `.toml`. NEGATIVE CONTROL, run as a mutant that made this test fail: an ordinal not held to
/// canonical decimal (`07` parsed as 7).
#[test]
fn a_run_record_files_under_its_day_run_ordinal_and_kind() {
    let filed = record(Kind::Abort, 7).filed().expect("it files");
    assert_eq!(filed.path(), format!("land/runs/{DAY}/{RUN}.7.abort.toml"));
    assert_eq!(Filed::parse(&filed.path()), Ok(filed));
    for (path, why) in [
        (format!("land/{DAY}/{RUN}.7.abort.toml"), "is not land/runs"),
        (
            format!("land/runs/0{DAY}/{RUN}.7.abort.toml"),
            "is not land/runs",
        ),
        (
            format!("land/runs/{DAY}/{RUN}.07.abort.toml"),
            "names no ordinal",
        ),
        (
            format!("land/runs/{DAY}/{}.7.abort.toml", &RUN[1..]),
            "names no run",
        ),
        (
            format!("land/runs/{DAY}/{RUN}.7.verdict.toml"),
            "names no kind",
        ),
        (format!("land/runs/{DAY}/{RUN}.7.abort"), "is not land/runs"),
    ] {
        let refused = Filed::parse(&path).expect_err(&path);
        assert!(refused.contains(why), "{path}: {refused}");
    }
    let unnamed = Record {
        run: Run {
            id: String::new(),
            ..run()
        },
        ..record(Kind::End, 0)
    };
    assert!(unnamed.filed().is_err(), "a run with no id files nothing");
}

/// A signed run record reads back as it was made, naming its signer's key; an unsigned one
/// reads with no signature. Each record read elsewhere than its own path is refused: another
/// run, ordinal, kind or day. So is one whose signature is a verdict record's domain, one whose
/// body changed after it was signed, and one naming a commit that is no object id or a host
/// that is no token. NEGATIVE CONTROL: the same body at its own path reads. Mutant run: a body
/// read without its day check (the record read under the day after its own).
#[test]
fn a_run_record_reads_only_at_its_own_path_under_its_own_domain() {
    let me = signer(1);
    let made = Record {
        cause: Some(
            Cause::new("cut", BASE, "it shares Cargo.lock").sharing(&["Cargo.lock".into()]),
        ),
        items: vec![Item::of_finding("test", FINDING)],
        items_total: 1,
        ..record(Kind::Judgment, 2)
    };
    let signed = made.sign(Some(&me)).expect("it signs");
    let read = Entry::read(&signed.path, &signed.bytes, Origin::Journal);
    let body = read.body.expect("it reads");
    assert_eq!(body.record, made);
    assert_eq!(body.signature.map(|s| s.key), Some(me.key()));
    let unsigned = made.sign(None).expect("it encodes");
    let read = Entry::read(&unsigned.path, &unsigned.bytes, Origin::Spool);
    assert_eq!(read.body.expect("it reads").signature, None);

    let elsewhere = [
        (signed.path.replace(RUN, &"f".repeat(32)), "not the ffff"),
        (signed.path.replace(".2.", ".3."), "not the 3"),
        (signed.path.replace("judgment", "round"), "(round)"),
        (
            signed
                .path
                .replace(&DAY.to_string(), &(DAY + 1).to_string()),
            "made on day",
        ),
    ];
    for (path, why) in elsewhere {
        let refused = Entry::read(&path, &signed.bytes, Origin::Journal)
            .body
            .expect_err(&path);
        assert!(refused.contains(why), "{path}: {refused}");
    }
    let text = String::from_utf8(signed.bytes.clone()).unwrap();
    let (body, _) = text.rsplit_once("\n[signature]\n").unwrap();
    let as_verdict =
        crate::land_record::seal(format!("{body}\n"), Some(&me), crate::land_record::DOMAIN)
            .unwrap();
    assert!(
        Entry::read(&signed.path, &as_verdict, Origin::Journal)
            .body
            .is_err(),
        "a verdict record's signature never reads as a run record's"
    );
    let tampered = text.replace("secs = 600", "secs = 601");
    assert!(
        Entry::read(&signed.path, tampered.as_bytes(), Origin::Journal)
            .body
            .is_err()
    );
    for bad in [
        Record {
            head: "HEAD".to_owned(),
            ..made.clone()
        },
        Record {
            run: Run {
                host: "a host".to_owned(),
                ..run()
            },
            ..made.clone()
        },
        Record {
            landing: Some(Landing {
                commit: String::new(),
                onto: BASE.to_owned(),
                kind: "exact".to_owned(),
                resumed: false,
            }),
            ..made.clone()
        },
    ] {
        let signed = bad.sign(Some(&me)).expect("it signs");
        assert!(
            Entry::read(&signed.path, &signed.bytes, Origin::Journal)
                .body
                .is_err(),
            "{bad:?}"
        );
    }
}

/// A red item is keyed by its check, package, target and test path where its finding is a test
/// finding of one package and a target the reader knows; any other finding keeps its check
/// and id alone. A reason past [`WHY_BYTES`] is cut at a character boundary, on one line.
/// NEGATIVE CONTROL, run as a mutant that made this test fail: an id whose test path is empty
/// keyed as a test (`-p aterm-uds --lib -- ` named a package).
#[test]
fn an_item_keys_a_test_finding_by_package_target_and_test_path() {
    let item = Item::of_finding("test", FINDING);
    assert_eq!(
        (
            item.package.as_deref(),
            item.target.as_deref(),
            item.test.as_deref()
        ),
        (Some("aterm-uds"), Some("lib"), Some("tests::one"))
    );
    let item = Item::of_finding("test", "-p aterm-verify@0.1.0 --test it -- land::a");
    assert_eq!(
        (
            item.package.as_deref(),
            item.target.as_deref(),
            item.test.as_deref()
        ),
        (Some("aterm-verify"), Some("test:it"), Some("land::a"))
    );
    for id in [
        "clippy::needless_return at src/lib.rs:1",
        "-p a -p b --lib -- t",
        "-p aterm-uds --lib -- ",
        "-p aterm-uds --features x -- t",
    ] {
        let item = Item::of_finding("lint", id);
        assert_eq!(
            (item.package, item.target, item.test),
            (None, None, None),
            "{id}"
        );
    }
    let long = format!("{}é\nmore", "x".repeat(WHY_BYTES - 1));
    let clipped = clip(&long);
    assert!(
        clipped.len() <= WHY_BYTES && !clipped.contains('\n'),
        "{clipped}"
    );
}
