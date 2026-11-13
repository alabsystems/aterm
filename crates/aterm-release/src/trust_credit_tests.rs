// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HEADER: &str = r#"{"schema":"trust.obligations.v1","package":"alpha","lane":"advisory"}"#;

/// A record of `lines`, the n-th line's key made `alpha::f#n` (Trust writes one line per
/// key).
fn record(lines: &[&str]) -> String {
    let keyed: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| l.replacen("\"alpha::f#1\"", &format!("\"alpha::f#{}\"", i + 1), 1))
        .collect();
    record_raw(&keyed.iter().map(String::as_str).collect::<Vec<_>>())
}

/// A record of `lines` exactly as given.
fn record_raw(lines: &[&str]) -> String {
    let mut s = format!("{HEADER}\n{{\"fn\":\"alpha::f\",\"source\":null}}\n");
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// An obligation line as targo 0.11.0 writes it: no grade fields.
fn old(status: &str) -> String {
    format!(
        r#"{{"key":"alpha::f#1","kind":"overflow","status":"{status}","stop":null,"file":"src/lib.rs","line":1,"col":0}}"#
    )
}

/// An obligation line as a grading targo writes it.
fn graded(status: &str, encoding: &str, discharge: &str) -> String {
    graded_with(status, encoding, discharge, &[])
}

/// An obligation line with premises.
fn graded_with(status: &str, encoding: &str, discharge: &str, premises: &[&str]) -> String {
    let premises = premises
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"col":0,"discharge":"{discharge}","encoding":"{encoding}","file":"src/lib.rs","key":"alpha::f#1","kind":"overflow","line":1,"premises":[{premises}],"status":"{status}","stop":null}}"#
    )
}

/// A row Trust's VF5 calculus authority (`CalculusRowProved`) graded, as targo-trust writes
/// it.
fn calculus(status: &str) -> String {
    graded_with(status, CALCULUS, CALCULUS_DISCHARGE, &CALCULUS_PREMISES)
}

fn epoch(floor: u64) -> Epoch {
    Epoch {
        id: 1,
        old: OLD_PROVED.into(),
        mapped: "unvalidated-encoder".into(),
        calculus_floor: floor,
        why: "test".into(),
    }
}

#[test]
fn the_committed_ledger_starts_the_counter_at_zero() {
    let text = include_str!("../trust/credit.tsv");
    let epochs = parse_epochs(text).expect("the committed ledger parses");
    let first = &epochs[0];
    assert_eq!(first.id, 1);
    assert_eq!(first.old, OLD_PROVED);
    assert_eq!(first.mapped, "unvalidated-encoder");
    assert_eq!(first.calculus_floor, 0, "proved (calculus) starts at 0");
}

#[test]
fn an_ungraded_record_counts_zero_and_says_ungraded() {
    let text = record(&[&old("proved"), &old("proved"), &old("unknown")]);
    let t = tally(&text).unwrap();
    assert_eq!(
        t,
        Tally {
            proved: 2,
            calculus: 0,
            graded: 0
        }
    );
    let mut c = Credit::default();
    c.add("alpha", Some(Ok(t)));
    let line = c.line(Some(&epoch(0)));
    assert_eq!(
        line,
        "proved (calculus): 0 of 2 proved [credit epoch 1, floor 0; ungraded: the prover stated no grade on any of 2 proved]"
    );
    assert!(judge(&epoch(0), &c).is_empty(), "0 holds a floor of 0");
    // Negative control: the same ungraded record fails a floor above 0, since an
    // ungraded proof is no calculus credit.
    let failed = judge(&epoch(1), &c);
    assert_eq!(failed.len(), 1);
    assert!(failed[0].contains("CREDIT-REGRESSED 0"), "{failed:?}");
}

#[test]
fn written_ungraded_rows_count_zero() {
    let text = record(&[&graded("proved", UNGRADED, UNGRADED)]);
    assert_eq!(
        tally(&text).unwrap(),
        Tally {
            proved: 1,
            calculus: 0,
            graded: 0
        }
    );
}

#[test]
fn calculus_counts_only_proven_calculus_proofs() {
    let text = record(&[
        &graded("proved", "unvalidated-encoder", "kernel-reconstructed"),
        &graded("proved", "e6-reflector", "kernel-reconstructed"),
        // Trust's KR0 switch: the Certified rows' discharge, read as graded, not calculus.
        &graded("proved", "unvalidated-encoder", "kernel-reference-checked"),
        &graded("proved", "e6-reflector", "kernel-reference-checked"),
        &graded("unknown", "not-encoded", "assumed"),
        &old("failed"),
    ]);
    let t = tally(&text).unwrap();
    assert_eq!(
        t,
        Tally {
            proved: 4,
            calculus: 0,
            graded: 4
        }
    );
    // Negative control: a discharge Trust does not state is still refused.
    let text = record(&[&graded(
        "proved",
        "e6-reflector",
        "kernel-reference-guessed",
    )]);
    assert!(tally(&text).unwrap_err().contains("unknown discharge"));
    // Negative control: one row graded as Trust's calculus authority grades it is
    // counted, and only when proved.
    let text = record(&[
        &calculus("proved"),
        &graded("failed", UNGRADED, UNGRADED),
        &graded("proved", "unvalidated-encoder", "solver-asserted"),
    ]);
    let t = tally(&text).unwrap();
    assert_eq!(
        t,
        Tally {
            proved: 2,
            calculus: 1,
            graded: 2
        }
    );
    let mut c = Credit::default();
    c.add("alpha", Some(Ok(t)));
    assert!(judge(&epoch(1), &c).is_empty());
    assert_eq!(
        c.line(Some(&epoch(1))),
        "proved (calculus): 1 of 2 proved [credit epoch 1, floor 1; graded]"
    );
}

/// Trust VF5: the counter reads the calculus authority's grade off the record, as
/// targo-trust writes `CalculusRowProved`'s rows, and nothing else as calculus. A
/// `proven-calculus` line with another discharge, or with a premise dropped, is refused:
/// Trust states no such grade, so it is a record nobody can read.
#[test]
fn the_counter_reads_the_vf5_calculus_authority_from_the_record() {
    let text = record(&[
        &calculus("proved"),
        &calculus("proved"),
        &graded("proved", "unvalidated-encoder", "kernel-reference-checked"),
        &graded("unknown", UNGRADED, UNGRADED),
    ]);
    let t = tally(&text).unwrap();
    assert_eq!(
        t,
        Tally {
            proved: 3,
            calculus: 2,
            graded: 3
        }
    );
    let mut c = Credit::default();
    c.add("alpha", Some(Ok(t)));
    assert_eq!(
        c.line(Some(&epoch(0))),
        "proved (calculus): 2 of 3 proved [credit epoch 1, floor 0; graded]"
    );
    assert!(judge(&epoch(2), &c).is_empty(), "2 holds a floor of 2");
    assert_eq!(judge(&epoch(3), &c).len(), 1, "and fails a floor of 3");

    for bad in [
        graded_with(
            "proved",
            CALCULUS,
            "kernel-reconstructed",
            &CALCULUS_PREMISES,
        ),
        graded_with(
            "proved",
            CALCULUS,
            CALCULUS_DISCHARGE,
            &["semr-translation", "target-kappa"],
        ),
        graded("proved", CALCULUS, CALCULUS_DISCHARGE),
    ] {
        let why = tally(&record(&[&bad])).unwrap_err();
        assert!(
            why.contains("calculus authority") || why.contains("no proof authority"),
            "{why}"
        );
    }
    assert!(is_calculus_row(
        CALCULUS,
        CALCULUS_DISCHARGE,
        &CALCULUS_PREMISES.map(String::from)
    ));
    assert!(!is_calculus_row(
        "unvalidated-encoder",
        CALCULUS_DISCHARGE,
        &CALCULUS_PREMISES.map(String::from)
    ));
    assert!(!is_calculus_row(
        CALCULUS,
        CALCULUS_DISCHARGE,
        &[
            "parameter-view".to_string(),
            "semr-translation".to_string(),
            "target-kappa".to_string(),
            "closed-world-entry".to_string(),
        ]
    ));
    assert!(!is_calculus_row(
        CALCULUS,
        CALCULUS_DISCHARGE,
        &[
            "closed-world-entry".to_string(),
            "semr-translation".to_string(),
            "target-kappa".to_string(),
        ]
    ));
    let extra_premise = graded_with(
        "proved",
        CALCULUS,
        CALCULUS_DISCHARGE,
        &[
            "parameter-view",
            "semr-translation",
            "target-kappa",
            "closed-world-entry",
        ],
    );
    assert!(
        tally(&record(&[&extra_premise]))
            .unwrap_err()
            .contains("calculus authority")
    );
}

/// Review finding 4: a line Trust neither writes nor reads back is refused, not counted.
/// One line per key; premises in Trust's declaration order, none repeated, each a premise
/// Trust knows; an (encoding, discharge) pair some authority states; and a grade only on a
/// status that admits it. Negative controls beside each: the same record as Trust writes it
/// counts.
#[test]
fn a_line_trust_would_not_write_is_refused() {
    // The same calculus line five times under one key: refused, not 5.
    let line = calculus("proved");
    let five = record_raw(&[&line, &line, &line, &line, &line]);
    assert!(tally(&five).unwrap_err().contains("one line per key"));
    let keyed = record(&[&line, &line, &line, &line, &line]);
    assert_eq!(
        tally(&keyed).unwrap().calculus,
        5,
        "control: five keys count five"
    );

    // Premises reordered, repeated, or unknown.
    for (premises, want) in [
        (
            &["closed-world-entry", "semr-translation", "target-kappa"][..],
            "declaration order",
        ),
        (
            &[
                "target-kappa",
                "semr-translation",
                "closed-world-entry",
                "closed-world-entry",
            ][..],
            "declaration order",
        ),
        (
            &[
                "semr-translation",
                "target-kappa",
                "closed-world-entry",
                "closed-world-entry",
            ][..],
            "declaration order",
        ),
        (
            &[
                "semr-translation",
                "target-kappa",
                "closed-world-entry",
                "made-up",
            ][..],
            "unknown grade premise",
        ),
    ] {
        let bad = graded_with("proved", CALCULUS, CALCULUS_DISCHARGE, premises);
        let why = tally(&record(&[&bad])).unwrap_err();
        assert!(why.contains(want), "{premises:?}: {why}");
    }
    assert_eq!(
        tally(&record(&[&calculus("proved")])).unwrap().calculus,
        1,
        "control: the authority's premises in order"
    );

    // A grade on a status that does not admit it.
    for status in ["failed", "unknown", "timeout"] {
        let why = tally(&record(&[&calculus(status)])).unwrap_err();
        assert!(why.contains("cannot carry"), "{status}: {why}");
        let kr0 = graded(status, "e6-reflector", "kernel-reference-checked");
        assert!(
            tally(&record(&[&kr0]))
                .unwrap_err()
                .contains("cannot carry")
        );
    }
    let assumed = graded("unknown", "not-encoded", "assumed");
    assert_eq!(
        tally(&record(&[&assumed])).unwrap().proved,
        0,
        "control: an unknown row may carry an assumed grade"
    );

    // A pair no authority states.
    let pair = graded("proved", "e6-reflector", "solver-asserted");
    assert!(
        tally(&record(&[&pair]))
            .unwrap_err()
            .contains("no proof authority")
    );
    let ok = graded("proved", "unvalidated-encoder", "solver-asserted");
    assert_eq!(
        tally(&record(&[&ok])).unwrap().graded,
        1,
        "control: an admitted pair"
    );
}

#[test]
fn a_grade_nobody_can_read_is_refused_not_ungraded() {
    let half = r#"{"key":"alpha::f#1","status":"proved","encoding":"proven-calculus"}"#;
    assert!(tally(&record(&[half])).unwrap_err().contains("together"));
    let unknown = graded("proved", "proven-by-vibes", "assumed");
    assert!(
        tally(&record(&[&unknown]))
            .unwrap_err()
            .contains("unknown encoding")
    );
    let lopsided = graded("proved", UNGRADED, "assumed");
    assert!(
        tally(&record(&[&lopsided]))
            .unwrap_err()
            .contains("ungraded row")
    );
    let other = record(&[]).replace("trust.obligations.v1", "trust.obligations.v0");
    assert!(tally(&other).unwrap_err().contains("schema"));
    let mut c = Credit::default();
    c.add("alpha", Some(tally(&record(&[half]))));
    let failed = judge(&epoch(0), &c);
    assert!(
        failed[0].starts_with("alpha CREDIT-UNREADABLE"),
        "{failed:?}"
    );
}

#[test]
fn a_check_that_left_no_record_is_named_and_counts_zero() {
    let mut c = Credit::default();
    c.add("alpha", None);
    assert_eq!(
        c.line(Some(&epoch(0))),
        "proved (calculus): 0 of 0 proved [credit epoch 1, floor 0; ungraded: no trust.obligations.v1 record; no record: alpha]"
    );
    assert!(judge(&epoch(0), &c).is_empty());
    assert_eq!(judge(&epoch(2), &c).len(), 1);
}

#[test]
fn the_ledger_moves_forward_only() {
    let row = |id: u32, floor: u64| {
        let old = if id == 1 {
            OLD_PROVED
        } else {
            "unvalidated-encoder"
        };
        format!("{id}\t{old}\tunvalidated-encoder\t{floor}\twhy\n")
    };
    // A later epoch maps an encoding, never the pre-grade `proved`.
    assert!(
        parse_epochs(&(row(1, 0) + "2\tproved\tunvalidated-encoder\t0\twhy\n"))
            .unwrap_err()
            .contains("later one")
    );
    assert_eq!(parse_epochs(&(row(1, 0) + &row(2, 3))).unwrap().len(), 2);
    assert!(
        parse_epochs(&(row(1, 3) + &row(2, 2)))
            .unwrap_err()
            .contains("lowers")
    );
    assert!(
        parse_epochs(&(row(1, 0) + &row(3, 0)))
            .unwrap_err()
            .contains("follows")
    );
    assert!(
        parse_epochs("1\tproved\tsolver-said\t0\twhy\n")
            .unwrap_err()
            .contains("no encoding")
    );
    assert!(parse_epochs("1\tproved\tnot-encoded\t0\t \n").is_err());
    assert!(
        parse_epochs("1\tunvalidated-encoder\tunvalidated-encoder\t0\twhy\n")
            .unwrap_err()
            .contains("first epoch")
    );
    assert!(parse_epochs("# only a comment\n").is_err());
    assert!(parse_epochs("+1\tproved\tunvalidated-encoder\t0\twhy\n").is_err());
    assert!(parse_epochs("1\tproved\tunvalidated-encoder\t+0\twhy\n").is_err());
}

#[test]
fn the_record_path_is_targo_trusts() {
    assert_eq!(
        record_path(Path::new("/t"), "aterm-json"),
        Path::new("/t/trust/obligations/aterm-json.ndjson")
    );
    assert_eq!(
        record_path(Path::new("/t"), "a.b"),
        Path::new("/t/trust/obligations/a_b.ndjson")
    );
}
