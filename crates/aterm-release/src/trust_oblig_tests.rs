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

#[test]
fn manifests_and_absolute_sources_are_folded_inside_the_checkout() {
    let d = checkout();
    let repo = d.path();
    for manifest in [
        "../Cargo.toml",
        "vendor/../../../Cargo.toml",
        "C:/outside/Cargo.toml",
    ] {
        assert!(Sources::new(repo, Some(manifest)).is_err(), "{manifest}");
    }
    let outside = tmpdir();
    let manifest = outside.path().join("Cargo.toml");
    assert!(Sources::new(repo, Some(manifest.to_str().unwrap())).is_err());
    let escaped = repo.join("../outside.rs");
    assert!(
        Sources::new(repo, None)
            .unwrap()
            .resolve(escaped.to_str().unwrap())
            .is_err()
    );
    let mut folded = Sources::new(repo, Some("vendor/w/crates/f/../f/Cargo.toml")).unwrap();
    assert_eq!(
        folded.resolve("src/lib.rs").unwrap(),
        ("vendor/w/crates/f/src/lib.rs".into(), true)
    );
    let absolute = repo.join("src/../src/lib.rs");
    let mut sources = Sources::new(repo, None).unwrap();
    assert_eq!(
        sources.resolve(absolute.to_str().unwrap()).unwrap(),
        ("src/lib.rs".into(), true)
    );
    assert_eq!(
        sources.lines("src/lib.rs").unwrap()[0],
        "pub fn twice(a: u16) -> u16 {"
    );
}

#[cfg(unix)]
#[test]
fn literal_unix_source_names_keep_colons_and_backslashes() {
    let repo = checkout();
    let original = fs::read(repo.path().join("src/lib.rs")).unwrap();
    for name in ["src/colon:name.rs", r"src/back\slash.rs", r"C:\literal.rs"] {
        fs::write(repo.path().join(name), &original).unwrap();
        let mut sources = Sources::new(repo.path(), None).unwrap();
        assert_eq!(sources.resolve(name).unwrap(), (name.into(), true));
        assert_eq!(
            sources.lines(name).unwrap()[0],
            "pub fn twice(a: u16) -> u16 {"
        );
        let json_name = aterm_json::to_string(name).unwrap();
        let report = probe_report().replace(r#""src/lib.rs""#, &json_name);
        let record = reduce_text(&report, repo.path()).unwrap();
        assert!(record.rows.iter().filter(|r| r.file == name).count() == 2);
        assert_eq!(Record::parse(&record.to_text()).unwrap(), record);
    }
}

#[cfg(windows)]
#[test]
fn native_windows_prefixes_cannot_be_relative_checkout_sources() {
    let repo = checkout();
    for name in [r"C:src\lib.rs", r"\src\lib.rs", r"\\server\share\lib.rs"] {
        assert!(source_name(Path::new(name)).is_err(), "{name}");
        assert!(
            Sources::new(repo.path(), None)
                .unwrap()
                .resolve(name)
                .is_err(),
            "{name}"
        );
    }
}

#[cfg(unix)]
#[test]
fn source_links_must_stay_inside_the_pinned_checkout() {
    use std::os::unix::fs::symlink;
    let d = checkout();
    let outside = checkout();
    let repo = d.path();
    symlink(outside.path(), repo.join("escaped")).unwrap();
    symlink(outside.path().join("src/lib.rs"), repo.join("escaped.rs")).unwrap();
    symlink("src", repo.join("internal")).unwrap();
    symlink("src/lib.rs", repo.join("internal.rs")).unwrap();
    let mut s = Sources::new(repo, None).unwrap();
    for raw in ["escaped/src/lib.rs", "escaped.rs"] {
        assert!(
            s.resolve(raw)
                .unwrap_err()
                .contains("outside the physical checkout")
        );
        assert!(
            s.lines(raw).is_err(),
            "the content reader enforces the boundary too"
        );
    }
    assert!(Sources::new(repo, Some("escaped/Cargo.toml")).is_err());
    symlink(outside.path().join("src/lib.rs"), repo.join("Cargo.toml")).unwrap();
    assert!(Sources::new(repo, Some("Cargo.toml")).is_err());
    for raw in ["internal/lib.rs", "internal.rs"] {
        assert_eq!(s.resolve(raw).unwrap(), (raw.into(), true));
        assert_eq!(s.lines(raw).unwrap()[0], "pub fn twice(a: u16) -> u16 {");
    }
    // A cached location must not let a later directory link redirect the first content read.
    assert!(s.resolve("vendor/w/crates/f/src/lib.rs").is_ok());
    fs::rename(repo.join("vendor"), repo.join("old-vendor")).unwrap();
    symlink(outside.path(), repo.join("vendor")).unwrap();
    assert!(s.lines("vendor/w/crates/f/src/lib.rs").is_err());
}

#[cfg(unix)]
#[test]
fn folding_parent_components_cannot_change_the_authenticated_native_target() {
    use std::os::unix::fs::symlink;
    let repo = checkout();
    let outside = checkout();
    fs::create_dir(outside.path().join("dir")).unwrap();
    symlink(outside.path().join("dir"), repo.path().join("escape")).unwrap();
    fs::create_dir_all(repo.path().join("other/dir")).unwrap();
    fs::create_dir(repo.path().join("other/src")).unwrap();
    fs::write(repo.path().join("other/src/lib.rs"), "fn unrelated() {}\n").unwrap();
    symlink("other/dir", repo.path().join("ambiguous")).unwrap();
    let mut sources = Sources::new(repo.path(), None).unwrap();
    for prefix in ["escape", "ambiguous"] {
        let raw = format!("{prefix}/../src/lib.rs");
        assert!(sources.resolve(&raw).is_err(), "{raw}");
        let absolute = repo.path().join(&raw);
        assert!(sources.resolve(absolute.to_str().unwrap()).is_err());
        let manifest = format!("{prefix}/../Cargo.toml");
        assert!(Sources::new(repo.path(), Some(&manifest)).is_err());
        let absolute_manifest = repo.path().join(&manifest);
        assert!(Sources::new(repo.path(), absolute_manifest.to_str()).is_err());
        assert!(Sources::new(&repo.path().join(prefix).join(".."), None).is_err());
    }
    let safe_repo = repo.path().join("src/..");
    let mut safe = Sources::new(&safe_repo, Some("src/../Cargo.toml")).unwrap();
    assert_eq!(
        safe.resolve("src/../src/lib.rs").unwrap(),
        ("src/lib.rs".into(), true)
    );
    assert_eq!(
        safe.lines("src/lib.rs").unwrap()[0],
        "pub fn twice(a: u16) -> u16 {"
    );
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
fn a_manifest_escape_cannot_publish_an_outside_source_anchor() {
    let repo = checkout();
    let outside = checkout();
    assert_eq!(repo.path().parent(), outside.path().parent());
    let d = tmpdir();
    let report = d.path().join("source.ndjson");
    let raw = probe_report();
    fs::write(&report, &raw).unwrap();
    let args = ReduceArgs {
        report: report.clone(),
        package: "d27probe".into(),
        repo: repo.path().to_path_buf(),
        manifest: Some(format!(
            "../{}/Cargo.toml",
            outside.path().file_name().unwrap().to_str().unwrap()
        )),
        out: d.path().join("escaped.oblig.tsv"),
    };
    assert!(write_new_record(&args).is_err());
    assert!(!args.out.exists());
    assert_eq!(fs::read_to_string(&report).unwrap(), raw);
}

#[test]
fn large_ignored_evidence_reduces_and_its_tail_is_still_validated() {
    let d = checkout();
    let statuses = ["proved", "failed", "unknown", "runtime_checked"];
    let obligations: Vec<Obl<'_>> = statuses
        .iter()
        .map(|status| {
            (
                "unbounded_allocation",
                *status,
                "src/lib.rs",
                2,
                24,
                2,
                29,
                "allocation observation",
            )
        })
        .collect();
    let good = report(&[function_line("d27probe::twice", &obligations)]).replacen(
        r#""outcome":{"status":"failed"}"#,
        r#""outcome":{"status":"failed","nested":{"messages":[{"note":"[stop:unclassified]"},{"reason":"[stop:rss-backstop]"},{"why":"[stop:wall-backstop]"}]}}"#,
        1,
    );
    let evidence = format!(
        r#""proof_evidence":{{"ignored":"{}"}},"time_ms""#,
        "x".repeat(20 * 1024 * 1024)
    );
    let text = good.replacen(
        r#""proof_evidence":{"artifacts":[{"deep":[[[1]]]}]},"time_ms""#,
        &evidence,
        1,
    );
    assert!(text.lines().any(|line| line.len() > 16 * 1024 * 1024));
    let expected = reduce_text(&good, d.path()).unwrap();
    let actual = reduce_text(&text, d.path()).unwrap();
    assert_eq!(actual, expected);
    assert_eq!((actual.total(), actual.proved()), (4, 1));
    assert_eq!(
        actual
            .rows
            .iter()
            .map(|row| row.status.as_str())
            .collect::<Vec<_>>(),
        statuses,
    );
    assert_eq!(actual.rows[1].stop, "rss-backstop");
    assert!(actual.rows[1].backstopped());
    let malformed = text.replacen("xxx\"},\"time_ms\"", "xx\\uD800\"},\"time_ms\"", 1);
    let why = reduce_text(&malformed, d.path()).unwrap_err();
    assert!(why.contains("unpaired high surrogate"), "{why}");
    let no_summary = text.replacen("\"summary\"", "\"ignored_summary\"", 1);
    assert!(
        reduce_text(&no_summary, d.path())
            .unwrap_err()
            .contains("summary is not its")
    );
    let no_footer = text.lines().take(2).collect::<Vec<_>>().join("\n");
    assert!(
        reduce_text(&no_footer, d.path())
            .unwrap_err()
            .contains("no footer")
    );
}

#[test]
fn physical_report_records_accept_crlf_and_no_final_lf_but_refuse_blank_or_split_strings() {
    let repo = checkout();
    let text = probe_report();
    let expected = reduce_text(&text, repo.path()).unwrap();
    let no_final_lf = text.trim_end_matches('\n');
    let crlf = no_final_lf.replace('\n', "\r\n");
    for valid in [no_final_lf, crlf.as_str()] {
        assert_eq!(reduce_text(valid, repo.path()).unwrap(), expected);
    }
    let blank = text.replacen('\n', "\r\n\r\n", 1);
    assert!(reduce_text(&blank, repo.path()).is_err());
    let split_string = text.replacen(
        r#""proof_evidence":{"#,
        "\"proof_evidence\":{\"padding\":\"first\nsecond\",",
        1,
    );
    assert!(reduce_text(&split_string, repo.path()).is_err());
}

#[test]
fn retained_report_data_still_has_a_memory_budget() {
    let d = checkout();
    let text = probe_report().replacen(
        "arithmetic overflow (Add)",
        &"x".repeat(17 * 1024 * 1024),
        1,
    );
    let why = reduce_text(&text, d.path()).unwrap_err();
    assert!(
        why.contains("retained JSON data exceeds its budget"),
        "{why}"
    );
}

#[test]
fn a_snapshot_never_borrows_the_external_files_pages() {
    use std::io::{Seek as _, Write as _};
    let d = tmpdir();
    let path = d.path().join("external");
    let before = "abcdef".repeat(1024 * 1024);
    fs::write(&path, &before).unwrap();
    let mut source = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let snapshot = snapshot::Snapshot::read(&mut source).unwrap();
    source.rewind().unwrap();
    source.write_all(b"changed").unwrap();
    source.set_len(0).unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(snapshot.bytes(), before.as_bytes());
    assert!(
        snapshot::Snapshot::read(&b""[..])
            .unwrap()
            .bytes()
            .is_empty()
    );
}

#[test]
fn standalone_reduction_uses_explicit_sources_and_never_replaces_outputs() {
    let repo = checkout();
    let d = tmpdir();
    let report = d.path().join("source.ndjson");
    let out = d.path().join("new.oblig.tsv");
    let original = probe_report().into_bytes();
    fs::write(&report, &original).unwrap();
    let native_grade = d.path().join("native-grade.json");
    let grade_bytes = b"retained native grade evidence\n";
    fs::write(&native_grade, grade_bytes).unwrap();
    let args = [
        "--report",
        report.to_str().unwrap(),
        "--package",
        "d27probe",
        "--repo",
        repo.path().to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ];
    let parsed = parse_reduce(&mut args.into_iter()).unwrap();
    // Current checkout identity does not select the source anchors or a prover.
    assert_eq!(
        crate::trust_facts::run_cmd(
            &d.path().join("no-current-checkout"),
            crate::trust_facts::TrustCmd::Reduce(parsed.clone()),
        ),
        0
    );
    let rec = Record::parse(&fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(reduce(&report, "d27probe", repo.path(), None), Ok(rec));
    assert_eq!(fs::read(&report).unwrap(), original);
    assert_eq!(fs::read(&native_grade).unwrap(), grade_bytes);
    fs::write(&out, "occupied").unwrap();
    assert!(write_new_record(&parsed).is_err());
    assert_eq!(fs::read_to_string(&out).unwrap(), "occupied");
    assert_eq!(fs::read(&report).unwrap(), original);
    let same_path = ReduceArgs {
        out: report.clone(),
        ..parsed.clone()
    };
    assert!(write_new_record(&same_path).is_err());
    assert_eq!(fs::read(&report).unwrap(), original);
    fs::remove_file(&out).unwrap();
    let missing_source = tmpdir();
    for invalid in [
        ReduceArgs {
            package: "another-package".into(),
            ..parsed.clone()
        },
        ReduceArgs {
            repo: missing_source.path().to_path_buf(),
            ..parsed.clone()
        },
    ] {
        assert!(write_new_record(&invalid).is_err());
        assert!(!out.exists());
        assert_eq!(fs::read(&report).unwrap(), original);
    }
    let no_footer = probe_report()
        .lines()
        .take(3)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&report, &no_footer).unwrap();
    assert!(write_new_record(&parsed).unwrap_err().contains("no footer"));
    assert!(!out.exists());
    assert_eq!(fs::read(&report).unwrap(), no_footer.as_bytes());
    fs::write(&report, "{broken").unwrap();
    assert!(write_new_record(&parsed).is_err());
    assert!(!out.exists());
    assert_eq!(fs::read(&report).unwrap(), b"{broken");
    assert_eq!(fs::read(native_grade).unwrap(), grade_bytes);
    for bad in [
        vec!["--update-ratchet"],
        vec!["--prover", "toolchain"],
        vec!["--admit", "now"],
        vec!["--whole-tree"],
        vec!["--try-refused"],
        vec!["--no-obligations"],
    ] {
        let mut invalid = args.to_vec();
        invalid.extend(bad);
        let why = parse_reduce(&mut invalid.into_iter()).unwrap_err();
        assert!(why.contains("unknown flag"), "{why}");
    }
    let mut repeated = args.to_vec();
    repeated.extend(["--repo", "again"]);
    assert_eq!(
        parse_reduce(&mut repeated.into_iter()).unwrap_err(),
        "trust reduce: --repo given twice",
    );
    let mut missing_value = args.to_vec();
    missing_value.push("--manifest-path");
    assert_eq!(
        parse_reduce(&mut missing_value.into_iter()).unwrap_err(),
        "trust reduce: --manifest-path needs a value",
    );
}

#[test]
fn reduction_writer_failures_return_two_and_preserve_evidence() {
    struct FailedWriter {
        fail_write: bool,
        writes: usize,
    }
    impl std::io::Write for FailedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            if self.fail_write {
                Err(std::io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }
    let repo = checkout();
    let d = tmpdir();
    let report = d.path().join("source.ndjson");
    let out = d.path().join("new.oblig.tsv");
    let original = probe_report().into_bytes();
    fs::write(&report, &original).unwrap();
    let grade = d.path().join("native-grade.json");
    let grade_bytes = b"retained native grade evidence\n";
    fs::write(&grade, grade_bytes).unwrap();
    let args = ReduceArgs {
        report: report.clone(),
        package: "d27probe".into(),
        repo: repo.path().to_path_buf(),
        out: out.clone(),
        ..ReduceArgs::default()
    };
    for fail_write in [true, false] {
        let mut stdout = Vec::new();
        let mut stderr = FailedWriter {
            fail_write,
            writes: 0,
        };
        assert_eq!(
            crate::trust_facts::reduce_with_io(&args, &mut stdout, &mut stderr),
            2
        );
        assert!(stderr.writes > 0);
        assert!(stdout.is_empty());
        assert!(!out.exists());
        assert_eq!(fs::read(&report).unwrap(), original);
        assert_eq!(fs::read(&grade).unwrap(), grade_bytes);
    }
    for fail_write in [true, false] {
        let mut stdout = FailedWriter {
            fail_write,
            writes: 0,
        };
        let mut stderr = Vec::new();
        assert_eq!(
            crate::trust_facts::reduce_with_io(&args, &mut stdout, &mut stderr),
            2
        );
        assert!(stdout.writes > 0);
        assert!(String::from_utf8(stderr).unwrap().contains("stdout:"));
        let record = Record::parse(&fs::read_to_string(&out).unwrap()).unwrap();
        assert_eq!(reduce(&report, "d27probe", repo.path(), None), Ok(record));
        assert_eq!(fs::read(&report).unwrap(), original);
        assert_eq!(fs::read(&grade).unwrap(), grade_bytes);
        fs::remove_file(&out).unwrap();
    }
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
            "@none",
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
