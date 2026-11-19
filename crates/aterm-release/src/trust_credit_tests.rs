// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) const HEADER: &str = concat!(
    r#"{"schema":"trust.obligations.v1","package":"alpha","lane":"advisory","policy":"strict","#,
    r#""selection":"--lib","seal":{"absent":"A8"},"targo_trust_commit":"5062eed55","#,
    r#""profile":"test","outcome":"measured","vector":"digest","coverage_complete":true,"concluded":true,"degraded_anchors":0}"#,
);
const FUNCTION: &str = r#"{"fn":"alpha::f","source":null}"#;

/// A record of `lines`, the n-th line's key made `alpha::f#n` (Trust writes one line per
/// key).
fn record(lines: &[&str]) -> String {
    let keyed: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            l.replacen(
                "\"alpha::f#0000000000000001\"",
                &format!("\"alpha::f#{:016x}\"", i + 1),
                1,
            )
        })
        .collect();
    record_raw(&keyed.iter().map(String::as_str).collect::<Vec<_>>())
}

/// A record of `lines` exactly as given.
fn record_raw(lines: &[&str]) -> String {
    let mut s = format!("{HEADER}\n{FUNCTION}\n");
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// An obligation line as targo 0.11.0 writes it: no grade fields.
fn old(status: &str) -> String {
    let stop = match status {
        "proved" | "failed" => "solved",
        "timeout" => "wall-backstop",
        _ => "unclassified",
    };
    format!(
        r#"{{"key":"alpha::f#0000000000000001","kind":"overflow","status":"{status}","stop":"{stop}","file":"src/lib.rs","line":1,"col":0}}"#
    )
}

/// An obligation line as a grading targo writes it.
fn graded(status: &str, encoding: &str, discharge: &str) -> String {
    graded_with(status, encoding, discharge, &[])
}

/// An obligation line with premises.
fn graded_with(status: &str, encoding: &str, discharge: &str, premises: &[&str]) -> String {
    let base = old(status);
    let premises = premises
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{},"encoding":"{encoding}","discharge":"{discharge}","premises":[{premises}]}}"#,
        base.strip_suffix('}').unwrap()
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
fn the_credit_header_accepts_historical_fields_and_rejects_damaged_headers() {
    // The original version used a string seal and predated policy, commit, coverage
    // conclusion and degraded-anchor counts.
    let original = concat!(
        r#"{"schema":"trust.obligations.v1","package":"alpha","lane":"advisory","#,
        r#""selection":"--lib","seal":"old-seal","profile":"test","outcome":"measured","#,
        r#""vector":"digest","coverage_complete":true}"#,
    );
    let row = old("proved");
    let text = format!("{original}\n{FUNCTION}\n{row}\n");
    assert_eq!(tally(&text).unwrap().proved, 1);
    assert_eq!(tally(HEADER).unwrap(), Tally::default());
    for invalid in [
        r#"["trust.obligations.v1","alpha","advisory","--lib","old-seal","test","measured","digest",true]"#.into(),
        HEADER.replace(r#","selection":"--lib""#, ""),
        HEADER.replace(r#","seal":{"absent":"A8"}"#, ""),
        HEADER.replace(r#""package":"alpha""#, r#""package":null"#),
        HEADER.replace(r#""concluded":true"#, r#""concluded":null"#),
        HEADER.replace(r#""degraded_anchors":0"#, r#""degraded_anchors":-1"#),
        HEADER.replace(
            r#""coverage_complete":true"#,
            r#""coverage_complete":"true""#,
        ),
        HEADER.replace(
            r#""lane":"advisory""#,
            r#""lane":"advisory","unknown":true"#,
        ),
    ] {
        let text = format!("{invalid}\n{FUNCTION}\n{row}\n");
        let why = tally(&text).expect_err(&invalid);
        assert!(why.contains("line 1"), "{why}");
    }
}

#[test]
fn only_complete_body_records_contribute_credit() {
    let proof = calculus("proved");
    let complete = format!("{HEADER}\n{FUNCTION}\n{proof}\n");
    assert_eq!(tally(&complete).unwrap().calculus, 1);
    let closure = complete.replace("alpha::f", "alpha::f::{closure#0}");
    assert_eq!(tally(&closure).unwrap().calculus, 1);
    let no_location = complete.replace(
        r#""file":"src/lib.rs","line":1,"col":0"#,
        r#""file":null,"line":null,"col":null"#,
    );
    assert_eq!(tally(&no_location).unwrap().calculus, 1);
    let reject = |invalid: &str| {
        // A damaged row invalidates the record even if a subsequent row carries good credit.
        let text = format!("{HEADER}\n{FUNCTION}\n{invalid}\n{proof}\n");
        let why = tally(&text).expect_err(invalid);
        assert!(why.contains("line 3"), "{invalid}: {why}");
        why
    };
    let mixed = proof.replace(r#""key":"#, r#""fn":"alpha::g","key":"#);
    let why = reject(&mixed);
    assert!(why.contains("function") && why.contains("key"), "{why}");
    let missing_kind = proof.replace(r#","kind":"overflow""#, "");
    let why = reject(&missing_kind);
    assert!(why.contains("obligation") && why.contains("kind"), "{why}");
    for invalid in [
        "{}".to_string(),
        "".into(),
        r#"["alpha::f",null]"#.into(),
        r#"{"status":"proved"}"#.into(),
        r#"{"fn":null}"#.into(),
        r#"{"fn":"alpha::g","source":false}"#.into(),
        r#"{"fn":"alpha::g","extra":0}"#.into(),
        proof.replace(r#""key":"#, r#""compile_error":"E","key":"#),
        proof.replace(r#","stop":"solved""#, ""),
        proof.replace(r#""stop":"solved""#, r#""stop":null"#),
        proof.replace(r#""file":"src/lib.rs""#, r#""file":42"#),
        proof.replace(r#""line":1"#, r#""line":-1"#),
        proof.replace(r#""col":0"#, r#""col":4294967296"#),
        proof.replace(r#""col":0"#, r#""col":0,"extra":true"#),
        proof.replace("alpha::f#", "alpha::g#"),
        proof.replace("#0000000000000001", "#1"),
        proof.replace("#0000000000000001", "#000000000000000A"),
        proof.replace(r#""encoding":"proven-calculus""#, r#""encoding":null"#),
        old("proved").replace(
            r#""col":0"#,
            r#""col":0,"encoding":null,"discharge":null,"premises":null"#,
        ),
    ] {
        reject(&invalid);
    }
    let missing_function = format!("{HEADER}\n{proof}\n");
    assert!(tally(&missing_function).unwrap_err().contains("line 2"));
}

#[test]
fn compile_errors_precede_functions_in_key_order() {
    let error = r#"{"compile_error":"src/lib.rs#E1","code":"E1","message":"bad type","file":"src/lib.rs","line":1,"col":2}"#;
    let second = error.replace("E1", "E2");
    let proof = calculus("proved");
    let valid = format!("{HEADER}\n{error}\n{second}\n{FUNCTION}\n{proof}\n");
    assert_eq!(tally(&valid).unwrap().calculus, 1);
    let only_errors = format!("{HEADER}\n{error}\n{second}\n");
    assert_eq!(tally(&only_errors).unwrap(), Tally::default());
    for (body, line) in [
        (format!("{FUNCTION}\n{error}"), 3),
        (format!("{second}\n{error}"), 3),
        (format!("{error}\n{error}"), 3),
        (error.replace(r#","message":"bad type""#, ""), 2),
        (error.replace(r#""code":"E1""#, r#""code":2"#), 2),
        (
            error.replace(
                r#""message":"bad type""#,
                r#""message":"bad type","extra":true"#,
            ),
            2,
        ),
    ] {
        let text = format!("{HEADER}\n{body}\n{FUNCTION}\n{proof}\n");
        let why = tally(&text).expect_err(&body);
        assert!(why.contains(&format!("line {line}")), "{why}");
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
    assert!(
        judge(Some(&epoch(0)), &c).is_empty(),
        "0 holds a floor of 0"
    );
    // Negative control: the same ungraded record fails a floor above 0, since an
    // ungraded proof is no calculus credit.
    let failed = judge(Some(&epoch(1)), &c);
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
    assert!(judge(Some(&epoch(1)), &c).is_empty());
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
    assert!(
        judge(Some(&epoch(2)), &c).is_empty(),
        "2 holds a floor of 2"
    );
    assert_eq!(
        judge(Some(&epoch(3)), &c).len(),
        1,
        "and fails a floor of 3"
    );

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
fn a_proved_obligation_must_have_a_solved_stop() {
    let historical = old("proved");
    let calculus = calculus("proved");
    assert_eq!(
        tally(&record(&[&historical, &calculus])).unwrap(),
        Tally {
            proved: 2,
            graded: 1,
            calculus: 1,
        }
    );
    for (row, stop) in [
        (&calculus, "wall-backstop"),
        (&historical, "not-dispatched"),
    ] {
        let damaged = row.replace(r#""stop":"solved""#, &format!(r#""stop":"{stop}""#));
        let why = tally(&record(&[&damaged])).expect_err("a proved obligation must be solved");
        assert!(why.contains("line 3") && why.contains(stop), "{why}");
    }

    // Trust's Lean writer fails the file closed when its theorem does not elaborate.
    let failed = concat!(
        r#"{"fn":"<file>","source":null}"#,
        "\n",
        r#"{"key":"<file>#ce7ef7696ca036b6","kind":"lean:file","status":"failed","stop":"solver-incomplete","#,
        r#""file":"Rejected.lean","line":1,"col":1,"encoding":"ungraded","discharge":"ungraded","#,
        r#""premises":[],"basis":"unproved_fail_closed"}"#,
    );
    assert_eq!(
        tally(&format!("{HEADER}\n{failed}\n")),
        Ok(Tally::default())
    );
}

#[test]
fn failure_basis_must_agree_with_the_credited_status() {
    fn with_basis(row: &str, value: &str) -> String {
        format!(r#"{},"basis":{value}}}"#, row.strip_suffix('}').unwrap())
    }
    let proof = calculus("proved");
    let failed = graded("failed", UNGRADED, UNGRADED);
    for (row, value) in [
        (&proof, r#""refuted""#),
        (&proof, r#""unproved_fail_closed""#),
        (&failed, r#""unknown""#),
        (&failed, "null"),
        (&failed, "false"),
    ] {
        let text = record(&[&with_basis(row, value)]);
        let why = tally(&text).expect_err("inconsistent proof record");
        assert!(why.contains("line 3"), "{why}");
    }
    for value in [r#""refuted""#, r#""unproved_fail_closed""#] {
        assert_eq!(
            tally(&record(&[&with_basis(&failed, value)])),
            Ok(Tally::default())
        );
    }
    // The pre-basis writer omitted the field even on graded and failed rows.
    assert_eq!(
        tally(&record(&[&failed, &old("failed")])),
        Ok(Tally::default())
    );
    assert_eq!(
        tally(&record(&[&with_basis(&proof, "null")]))
            .unwrap()
            .calculus,
        1
    );
}

#[test]
fn a_grade_nobody_can_read_is_refused_not_ungraded() {
    let half = r#"{"key":"alpha::f#0000000000000001","kind":"assert","status":"proved","stop":"solved","encoding":"proven-calculus"}"#;
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
    assert!(
        c.line(None)
            .contains("incomplete count from readable records")
    );
    c.add("beta", Some(tally(&record(&[&calculus("proved")]))));
    assert_eq!(c.total.calculus, 1);
    let failed = judge(Some(&epoch(2)), &c);
    assert_eq!(
        failed.len(),
        1,
        "an incomplete count cannot establish a regression"
    );
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
    assert!(judge(Some(&epoch(0)), &c).is_empty());
    assert_eq!(judge(Some(&epoch(2)), &c).len(), 1);
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

#[test]
fn the_standalone_floor_counts_each_physical_record_once() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    let ledger = root.join(EPOCHS);
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(ledger, "1\tproved\tunvalidated-encoder\t2\ttwo proofs\n").unwrap();

    let text = record(&[&calculus("proved")]);
    let one = root.join("one.ndjson");
    let link = root.join("hardlink.ndjson");
    std::fs::write(&one, &text).unwrap();
    std::fs::hard_link(&one, &link).unwrap();
    for alias in [one.clone(), root.join("./one.ndjson"), link] {
        assert_eq!(run_cmd(root, &[one.clone(), alias]), 1);
    }

    // Two independent files for the same package still meet the floor.
    let second = root.join("second.ndjson");
    std::fs::write(&second, &text).unwrap();
    assert_eq!(run_cmd(root, &[one.clone(), second.clone()]), 0);

    // A trailing slash requests a directory. Its open error must survive the good reads.
    let mut directory = one.clone().into_os_string();
    directory.push("/");
    assert_eq!(run_cmd(root, &[one, second, PathBuf::from(directory)]), 1);
}

#[test]
fn a_changed_record_behind_an_alias_refuses_the_count() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let first = dir.path().join("first.ndjson");
    let alias = dir.path().join("alias.ndjson");
    std::fs::write(&first, record(&[&calculus("proved")])).unwrap();
    std::fs::hard_link(&first, &alias).unwrap();
    let mut credit = Credit::default();
    let mut files = BTreeMap::new();
    count_file(&mut credit, &mut files, &first);
    std::fs::write(&alias, record(&[&old("proved")])).unwrap();
    count_file(&mut credit, &mut files, &alias);
    assert_eq!(credit.total.calculus, 1);
    assert_eq!(credit.refused.len(), 1);
    assert!(judge(Some(&epoch(1)), &credit)[0].contains("CREDIT-UNREADABLE"));
}

#[test]
fn the_live_grade_record_must_name_the_package_being_checked() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path();
    let path = record_path(target, "beta");
    assert!(read_record(target, "beta").is_none());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let alpha = record(&[&calculus("proved"), &old("proved")]);
    std::fs::write(&path, &alpha).unwrap();
    let why = read_record(target, "beta").unwrap().unwrap_err();
    assert!(
        why.contains(path.to_str().unwrap())
            && why.contains(r#""alpha""#)
            && why.contains(r#""beta""#),
        "{why}"
    );

    // The standalone command takes arbitrary paths, including renamed records.
    let ledger = target.join(EPOCHS);
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(
        ledger,
        "1\tproved\tunvalidated-encoder\t1\tone calculus proof\n",
    )
    .unwrap();
    assert_eq!(run_cmd(target, std::slice::from_ref(&path)), 0);

    std::fs::write(&path, alpha.replace(r#","package":"alpha""#, "")).unwrap();
    let why = read_record(target, "beta").unwrap().unwrap_err();
    assert!(
        why.contains(path.to_str().unwrap())
            && why.contains(r#""beta""#)
            && why.contains("missing"),
        "{why}"
    );

    std::fs::write(&path, alpha.replace("alpha", "beta")).unwrap();
    assert_eq!(
        read_record(target, "beta").unwrap().unwrap(),
        Tally {
            proved: 2,
            calculus: 1,
            graded: 1
        }
    );
}
