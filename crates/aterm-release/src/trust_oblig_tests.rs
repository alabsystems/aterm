// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn tmpdir() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("trust-oblig")
        .tempdir()
        .unwrap()
}

#[test]
fn the_anchor_is_the_spans_text_by_characters() {
    let src: Vec<String> = [
        "pub fn unicode(a: u8) -> u8 {",
        "    let _s = \"éé—\"; if a < 10 { a + 1 } else { 0 }",
        "}",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let one = Span {
        l0: 2,
        c0: 32,
        l1: 2,
        c1: 37,
    };
    assert_eq!(anchor_of(&src, one).unwrap(), "a + 1");
    let whole = Span {
        l0: 1,
        c0: 0,
        l1: 3,
        c1: 1,
    };
    assert_eq!(
        anchor_of(&src, whole).unwrap(),
        "pub fn unicode(a: u8) -> u8 { … }"
    );
    let long = vec!["x".repeat(150)];
    let a = anchor_of(
        &long,
        Span {
            l0: 1,
            c0: 0,
            l1: 1,
            c1: 150,
        },
    )
    .unwrap();
    assert!(a.starts_with(&"x".repeat(80)) && a.contains("…#"), "{a}");
    assert_eq!(a.chars().count(), 80 + 2 + 12);
    for bad in [
        Span {
            l0: 4,
            c0: 0,
            l1: 4,
            c1: 1,
        },
        Span {
            l0: 3,
            c0: 0,
            l1: 3,
            c1: 2,
        },
        Span {
            l0: 0,
            c0: 0,
            l1: 1,
            c1: 1,
        },
    ] {
        assert!(anchor_of(&src, bad).is_err(), "{bad}");
    }
}

/// A checkout with `src/lib.rs` (and a fork whose workspace is `vendor/w`).
fn checkout() -> aterm_tempfile::TempDir {
    let d = tmpdir();
    fs::create_dir_all(d.path().join("src")).unwrap();
    fs::write(
        d.path().join("src/lib.rs"),
        "pub fn twice(a: u16) -> u16 {\n    let x = if a < 10 { a + 1 } else { 0 };\n    x + x\n}\n",
    )
    .unwrap();
    fs::create_dir_all(d.path().join("vendor/w/crates/f/src")).unwrap();
    fs::write(d.path().join("vendor/w/crates/f/src/lib.rs"), "fn f() {}\n").unwrap();
    d
}

#[test]
fn files_are_named_as_the_record_keeps_them() {
    let d = checkout();
    let repo = d.path();
    let mut s = Sources::new(repo, None).unwrap();
    assert_eq!(
        s.resolve("src/lib.rs").unwrap(),
        ("src/lib.rs".into(), true)
    );
    assert_eq!(
        s.resolve("alloc/src/macros.rs").unwrap(),
        ("@sysroot/alloc/src/macros.rs".into(), false)
    );
    // Trust 0.12 (29ebb403d) names them from its source tree's root.
    assert_eq!(
        s.resolve("library/core/src/macros/mod.rs").unwrap(),
        ("@sysroot/core/src/macros/mod.rs".into(), false)
    );
    assert!(
        s.resolve("library/serde/src/lib.rs").is_err(),
        "a library/ that is no toolchain crate's names no file"
    );
    assert_eq!(
        s.resolve("/Users//user000/trust/library/alloc/src/macros.rs")
            .unwrap(),
        ("@sysroot/alloc/src/macros.rs".into(), false)
    );
    assert_eq!(
        s.resolve("/rustc/0123abcd/library/core/src/num/mod.rs")
            .unwrap(),
        ("@sysroot/core/src/num/mod.rs".into(), false)
    );
    assert_eq!(
        s.resolve("/h/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/log-0.4.32/src/lib.rs")
            .unwrap(),
        ("@registry/log-0.4.32/src/lib.rs".into(), false)
    );
    let abs = format!("{}/src/lib.rs", repo.display());
    assert_eq!(s.resolve(&abs).unwrap(), ("src/lib.rs".into(), true));
    let mut fork = Sources::new(repo, Some("vendor/w/crates/f/Cargo.toml")).unwrap();
    assert_eq!(
        fork.resolve("crates/f/src/lib.rs").unwrap(),
        ("vendor/w/crates/f/src/lib.rs".into(), true)
    );
    assert_eq!(
        fork.resolve("src/lib.rs").unwrap(),
        ("vendor/w/crates/f/src/lib.rs".into(), true)
    );
    assert_eq!(
        s.resolve("vendor/w/crates/f/src/../../f/src/lib.rs")
            .unwrap(),
        ("vendor/w/crates/f/src/lib.rs".into(), true)
    );
    assert_eq!(
        fork.resolve("crates/f/src/../../f/src/lib.rs").unwrap(),
        ("vendor/w/crates/f/src/lib.rs".into(), true)
    );
    for bad in [
        "/opt/elsewhere/x.rs",
        "src/missing.rs",
        "../src/lib.rs",
        "src/../../src/lib.rs",
        "src/./lib.rs",
        "",
        "src//lib.rs",
    ] {
        assert!(s.resolve(bad).is_err(), "{bad}");
    }
}

/// A report.ndjson line for a function with `obligations` (kind, status,
/// file, l0, c0, l1, c1, description).
type Obl<'a> = (&'a str, &'a str, &'a str, u32, u32, u32, u32, &'a str);

const UNIT: &str = r#"cargo-target(package_id=\"path+file:///x#d27probe@0.1.0\",package=\"d27probe\",kind=[\"lib\"],target=\"d27probe\")"#;

fn function_line(path: &str, obls: &[Obl<'_>]) -> String {
    let proved = obls.iter().filter(|o| o.1 == "proved").count();
    let os: Vec<String> = obls
        .iter()
        .map(|(k, st, f, l0, c0, l1, c1, d)| {
            format!(
                r#"{{"obligation_id":"vc:x:y:0","description":"{d}","kind":"{k}","location":{{"file":"{f}","line_start":{l0},"col_start":{c0},"line_end":{l1},"col_end":{c1}}},"outcome":{{"status":"{st}"}},"proof_evidence":{{"artifacts":[{{"deep":[[[1]]]}}]}},"time_ms":0}}"#
            )
        })
        .collect();
    format!(
        r#"{{"record_type":"function","crate_name":"{UNIT}","function":"{UNIT}::{path}","summary":{{"total_obligations":{},"proved":{proved},"verdict":"x"}},"obligations":[{}]}}"#,
        obls.len(),
        os.join(",")
    )
}

fn report(functions: &[String]) -> String {
    let total: usize = functions
        .iter()
        .map(|f| f.matches("\"obligation_id\"").count())
        .sum();
    let proved: usize = functions
        .iter()
        .map(|f| f.matches(r#""status":"proved""#).count())
        .sum();
    let mut out = format!(
        r#"{{"record_type":"header","schema":"{REPORT_SCHEMA}","crate_name":"{UNIT}","expected_functions":{}}}"#,
        functions.len()
    );
    out.push('\n');
    for f in functions {
        out.push_str(f);
        out.push('\n');
    }
    out.push_str(&format!(
        r#"{{"record_type":"footer","schema":"{REPORT_SCHEMA}","summary":{{"total_obligations":{total},"total_proved":{proved},"verdict":"x"}},"functions_emitted":{}}}"#,
        functions.len()
    ));
    out.push('\n');
    out
}

fn probe_report() -> String {
    report(&[
        function_line(
            "d27probe::twice",
            &[
                (
                    "arithmetic_overflow_add",
                    "proved",
                    "src/lib.rs",
                    2,
                    24,
                    2,
                    29,
                    "arithmetic overflow (Add)",
                ),
                (
                    "arithmetic_overflow_add",
                    "unknown",
                    "src/lib.rs",
                    3,
                    4,
                    3,
                    9,
                    "arithmetic overflow (Add)",
                ),
            ],
        ),
        function_line(
            "d27probe::zeros_a",
            &[(
                "unbounded_allocation",
                "proved",
                "alloc/src/macros.rs",
                47,
                8,
                47,
                41,
                "unbounded allocation in `vec::from_elem` (count `_7`)",
            )],
        ),
    ])
}

fn reduce_text(text: &str, repo: &Path) -> std::result::Result<Record, String> {
    let d = tmpdir();
    let p = d.path().join("report.ndjson");
    fs::write(&p, text).unwrap();
    reduce(&p, "d27probe", repo, None)
}

#[test]
fn a_report_reduces_to_its_record() {
    let d = checkout();
    let rec = reduce_text(&probe_report(), d.path()).unwrap();
    assert_eq!((rec.functions, rec.total(), rec.proved()), (2, 3, 2));
    let o = &rec.rows[0];
    assert_eq!(o.function, "d27probe::twice");
    assert_eq!(o.anchor, "a + 1");
    assert_eq!(
        (o.file.as_str(), o.span.to_string()),
        ("src/lib.rs", "2:24-2:29".to_string())
    );
    assert_eq!(rec.rows[1].anchor, "x + x");
    let z = &rec.rows[2];
    assert_eq!(z.file, "@sysroot/alloc/src/macros.rs");
    assert_eq!(z.anchor, "-");
    assert!(z.desc.ends_with("(count `_7`)"), "{}", z.desc);
    let text = rec.to_text();
    let unit = format!("unit\t{}", rec.unit);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..6],
        [
            RECORD_MAGIC,
            "package\td27probe",
            unit.as_str(),
            "functions\t2",
            "total\t3",
            "proved\t2",
        ]
    );
    let row: Vec<&str> = lines[6].split('\t').collect();
    assert_eq!(
        row,
        [
            "o",
            "d27probe::twice",
            "arithmetic_overflow_add",
            "src/lib.rs",
            "2",
            "24",
            "2",
            "29",
            "proved",
            "-",
            "a + 1",
            "arithmetic overflow (Add)",
        ]
    );
    assert_eq!(lines.last().copied(), Some("end\t3"));
    assert_eq!(Record::parse(&text), Ok(rec.clone()));
    let none = probe_report().replacen(
        r#""location":{"file":"src/lib.rs","line_start":3,"col_start":4,"line_end":3,"col_end":9},"#,
        "",
        1,
    );
    let rec = reduce_text(&none, d.path()).unwrap();
    let o = &rec.rows[1];
    assert_eq!((o.file.as_str(), o.anchor.as_str()), ("@none", "-"));
    assert_eq!(o.span.to_string(), "0:0-0:0");
}

#[test]
fn a_rows_stop_is_the_reports_own_word() {
    let d = checkout();
    let five: Vec<Obl<'_>> = (0..5)
        .map(|_| {
            (
                "arithmetic_overflow_add",
                "unknown",
                "src/lib.rs",
                3,
                4,
                3,
                9,
                "arithmetic overflow (Add)",
            )
        })
        .collect();
    let plain = r#""outcome":{"status":"unknown"}"#;
    let text = report(&[function_line("d27probe::twice", &five)])
        .replacen(
            plain,
            r#""outcome":{"status":"runtime_checked","note":"solver timed out [stop:wall-backstop]"}"#,
            1,
        )
        .replacen(plain, r#""outcome":{"status":"timeout","timeout_ms":5000}"#, 1)
        .replacen(
            plain,
            r#""outcome":{"status":"unknown","reason":"the solver returned unknown [stop:unclassified]; RSS high [stop:rss-backstop]"}"#,
            1,
        )
        .replacen(
            plain,
            r#""outcome":{"status":"unknown","note":"[stop:unclassified]"}"#,
            1,
        );
    let rec = reduce_text(&text, d.path()).unwrap();
    let stops: Vec<(&str, bool)> = rec
        .rows
        .iter()
        .map(|o| (o.stop.as_str(), o.backstopped()))
        .collect();
    assert_eq!(
        stops,
        [
            ("wall-backstop", true),
            ("wall-backstop", true),
            ("rss-backstop", true),
            ("unclassified", false),
            ("-", false),
        ]
    );
    assert_eq!(Record::parse(&rec.to_text()), Ok(rec.clone()));
    let key = rec.rows[0].key();
    assert_eq!(key.len(), 16);
    let mut moved = rec.rows[0].clone();
    moved.span.l0 += 40;
    moved.span.l1 += 40;
    moved.status = "proved".into();
    assert_eq!(
        moved.key(),
        key,
        "a line or a verdict is no part of the key"
    );
    let mut local = rec.rows[0].clone();
    local.desc = "arithmetic   overflow (Add)".into();
    assert_eq!(local.key(), key);
    local.anchor = "x + y".into();
    assert_ne!(local.key(), key, "another anchor is another obligation");
    assert_eq!(
        norm_desc("unbounded allocation (count `_17`) at @f3f39371aeb0 `_x`"),
        "unbounded allocation (count `_`) at @# `_x`"
    );
    for (bad, said) in [
        (
            rec.to_text().replacen("\twall-backstop\t", "\tWall!\t", 1),
            "the stop `Wall!`",
        ),
        (
            rec.to_text().replacen("\twall-backstop\t", "\t", 1),
            "is not a row of a record",
        ),
        (
            rec.to_text()
                .replace(RECORD_MAGIC, "# aterm trust obligations 1"),
            "its first line",
        ),
        (
            rec.to_text().replace("total\t5", "total\t6"),
            "it says 6 obligations",
        ),
    ] {
        let why = Record::parse(&bad).expect_err(said);
        assert!(why.contains(said), "{said}: {why}");
    }
}

#[test]
fn every_report_it_cannot_vouch_for_is_refused_with_its_reason() {
    let d = checkout();
    let good = probe_report();
    let lines: Vec<&str> = good.lines().collect();
    let join = |v: &[&str]| v.join("\n") + "\n";
    let cases: Vec<(String, &str)> = vec![
        (String::new(), "the report is empty"),
        (join(&lines[..3]), "no footer: it was cut short"),
        (
            join(&[lines[0], "not json", lines[3]]),
            "is not a report record",
        ),
        (join(&[lines[1], lines[0]]), "before the header"),
        (
            good.replace(REPORT_SCHEMA, "trust.report.ndjson.v3"),
            "schema trust.report.ndjson.v3",
        ),
        (
            good.replace("package=\\\"d27probe\\\"", "package=\\\"other\\\""),
            "the report is of package other, not d27probe",
        ),
        (
            join(&[
                lines[0],
                lines[1],
                r#"{"record_type":"transport","schema":"x"}"#,
                lines[2],
                lines[3],
            ]),
            "a `transport` record",
        ),
        (
            good.replace("::d27probe::twice", "d27probe::twice"),
            "is not of the form <unit>::<path>",
        ),
        (
            good.replace("\"arithmetic_overflow_add\"", "\"Arith Overflow\""),
            "the kind `Arith Overflow`",
        ),
        (
            good.replace("\"arithmetic_overflow_add\"", "\"transport:missing-json\""),
            "a `transport:missing-json` row",
        ),
        (
            good.replace("\"line_end\":2,", "\"line_end\":1,"),
            "ends before it starts",
        ),
        (
            good.replace("src/lib.rs", "/opt/x/lib.rs"),
            "an absolute path of no form",
        ),
        (
            good.replace("\"line_start\":3", "\"line_start\":30")
                .replace("\"line_end\":3", "\"line_end\":30"),
            "past the file",
        ),
        (
            good.replace("\"total_proved\":2", "\"total_proved\":3"),
            "the footer says",
        ),
        (
            good.replace("\"functions_emitted\":2", "\"functions_emitted\":3"),
            "the footer says",
        ),
        (
            good.replace("\"proved\":1,", "\"proved\":2,"),
            "summary is not its",
        ),
        (
            good.replace("\"status\":\"unknown\"", "\"status\":\"Unknown!\""),
            "the status `Unknown!`",
        ),
        (
            good.replacen(r#""file":"src/lib.rs""#, r#""file":"@none""#, 1),
            "@none names no file in the checkout",
        ),
        (format!("{good}{}\n", lines[1]), "follows the footer"),
        (
            join(&[lines[0], lines[1], lines[1], lines[3]])
                .replace("\"functions_emitted\":2", "\"functions_emitted\":3"),
            "d27probe::twice is reported a second time",
        ),
    ];
    for (text, said) in cases {
        let why = reduce_text(&text, d.path()).expect_err(said);
        assert!(why.contains(said), "{said}: {why}");
    }
}
