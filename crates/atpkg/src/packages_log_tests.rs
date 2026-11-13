// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn scratch(label: &str) -> crate::testdir::Dir {
    crate::testdir::dir(&format!("atpkg-packages-log-{label}-"))
}

fn transition() -> Transition {
    Transition {
        program: "claude".into(),
        from: Some("2.1.278".into()),
        to: Some("2.1.280".into()),
        source: "Anthropic latest".into(),
        result: "updated",
        reason: String::new(),
    }
}

#[test]
fn a_line_is_one_tab_separated_record_that_reads_back() {
    let t = transition();
    let line = render(1_790_000_000, 4242, &Event::Program(&t));
    assert_eq!(
        line,
        "2026-09-21T14:13:20Z\tprogram\tpid=4242\tprogram=claude\tfrom=2.1.278\t\
         to=2.1.280\tsource=Anthropic latest\tresult=updated\n"
    );
    assert_eq!(line.matches('\n').count(), 1);
    let entry = parse_line(&line).unwrap();
    assert_eq!(entry.at, Some(1_790_000_000));
    assert_eq!(entry.kind, kind::PROGRAM);
    assert_eq!(entry.pid, 4242);
    assert_eq!(entry.get("to"), Some("2.1.280"));
    assert_eq!(entry.get("reason"), None, "an empty field is not written");

    let end = render(
        1_790_000_000,
        7,
        &Event::PassEnd {
            lane: "window",
            verb: "update",
            exit: 69,
            secs: 12,
            outcome: "offline",
        },
    );
    assert_eq!(
        end,
        "2026-09-21T14:13:20Z\tpass-end\tpid=7\tlane=window\tverb=update\texit=69\t\
         secs=12\toutcome=offline\n"
    );
    let seam = render(
        1_790_000_000,
        7,
        &Event::Seam {
            seam: "rustup:trust",
            from: "/h/trust/build/host/stage2",
            to: "/p/rustup/trust",
            why: "a toolchain from 2026-08-20, older than the store's 2026-09-17",
            undo: "to put it back: ln -sfn '/h/trust/build/host/stage2' '/h/.rustup/toolchains/trust'",
        },
    );
    let read = parse_line(&seam).expect("a seam line is one of the log's own");
    assert_eq!(read.kind, kind::SEAM);
    assert_eq!(read.get("from"), Some("/h/trust/build/host/stage2"));
    assert_eq!(
        read.get("undo"),
        Some("to put it back: ln -sfn '/h/trust/build/host/stage2' '/h/.rustup/toolchains/trust'"),
        "a path and its quotes pass the sanitiser whole"
    );
    let start = render(
        i64::MAX,
        7,
        &Event::PassStart {
            lane: "typed",
            verb: "seed",
        },
    );
    assert!(start.starts_with("-\tpass-start\t"), "{start}");
    assert_eq!(parse_line(&start).unwrap().at, None);
    let warm = render(
        1_790_000_000,
        9,
        &Event::Warm {
            program: "claude",
            build: crate::vendor_direct::Version::parse("2.1.280")
                .unwrap()
                .build_id(),
            outcome: "exit=0",
            ms: 10_412,
        },
    );
    assert_eq!(
        warm,
        "2026-09-21T14:13:20Z\twarm\tpid=9\tprogram=claude\tbuild=2.1.280\t\
         outcome=exit=0\tms=10412\n"
    );
    let entry = parse_line(&warm).unwrap();
    assert_eq!(entry.kind, kind::WARM);
    assert_eq!(entry.pid, 9);
    assert_eq!(
        entry.fields,
        [
            ("program", "claude"),
            ("build", "2.1.280"),
            ("outcome", "exit=0"),
            ("ms", "10412"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
    );
    for foreign in [
        "",
        "hello",
        "2026-09-21T14:13:20Z\tsomething\tpid=1",
        "2026-09-21T14:13:20Z\tprogram",
        "2026-09-21T14:13:20Z\tprogram\tpid=1\tno-equals",
        "2026-09-21T14:13:20Z\tprogram\tpid=1\tpid=2",
        "2026-09-21T14:13:20Z\tprogram\tpid=1\tpid=bad",
        "2026-09-21T14:13:20Z\tprogram\tpid=1\t=val",
        "2026-09-21 14:13:20\tprogram\tpid=1",
    ] {
        assert_eq!(parse_line(foreign), None, "{foreign:?}");
    }
}

#[test]
fn values_lose_controls_bidi_credentials_and_length() {
    let forged = sanitize("ok\tresult=installed\nfake line\u{1b}[31m");
    assert!(!forged.contains(['\t', '\n', '\u{1b}']), "{forged:?}");
    assert_eq!(sanitize("a\u{202E}b\u{2066}c\u{200F}d"), "abcd");
    assert_eq!(
        sanitize("fetch https://user:s3cret@github.com/o/r/releases?token=abc#x failed"),
        "fetch https://github.com/o/r/releases?… failed"
    );
    assert_eq!(
        sanitize("GET https://objects.githubusercontent.com/a?X-Amz-Signature=zz"),
        "GET https://objects.githubusercontent.com/a?…"
    );
    assert_eq!(
        sanitize("auth failed (ghp_abcdef123) with Bearer xyz and token ghs_q"),
        "auth failed [redacted] with Bearer [redacted] and token [redacted]"
    );
    for (raw, clean) in [
        (
            "fetch failed: Authorization: Bearer sk-ant-abc123secret",
            "fetch failed: Authorization: Bearer [redacted]",
        ),
        (
            "fetch failed: authorization: abc123secret now",
            "fetch failed: authorization: [redacted] now",
        ),
        ("error: token=abc123secret", "error: token=[redacted]"),
        ("error: token: abc123secret", "error: token: [redacted]"),
        ("password=hunter2 (retry)", "password=[redacted] (retry)"),
        (
            "{\"api_key\":\"abc\"} refused",
            "{\"api_key\":[redacted] refused",
        ),
        (
            "GITHUB_TOKEN=abc api_key: xyz",
            "GITHUB_TOKEN=[redacted] api_key: [redacted]",
        ),
        ("key=abc", "key=[redacted]"),
        ("api key sk-proj-XYZsecret", "api key [redacted]"),
        (
            concat!("AKIA", "IOSFODNN7EXAMPLE leaked"),
            "[redacted] leaked",
        ),
        (
            "session aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3z expired",
            "session [redacted] expired",
        ),
    ] {
        assert_eq!(sanitize(raw), clean, "{raw:?}");
    }
    let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    for kept in [
        format!("digest mismatch: expected {digest}"),
        "claude 2.1.281 refused: SHA256SUMS disagrees".to_string(),
        "names the refused key: channel".to_string(),
        "x86_64-apple-darwin-unknown-target-triple-long-name".to_string(),
    ] {
        assert_eq!(sanitize(&kept), kept);
    }
    assert_eq!(sanitize("a\u{2028}b\u{2029}c"), "a b c");
    assert_eq!(
        sanitize("gh\u{200B}p_x\u{200C}\u{200D}\u{2060}\u{FEFF}"),
        "[redacted]"
    );
    assert_eq!(sanitize("   spaced    out  "), "spaced out");
    let long = sanitize(&"x".repeat(10_000));
    assert_eq!(long.chars().count(), MAX_VALUE_CHARS + 1);
    assert!(long.ends_with('…'));
    let t = Transition {
        reason: "error: a\tb\nc https://u:p@h/x?y".into(),
        ..transition()
    };
    let line = render(1, 1, &Event::Program(&t));
    assert_eq!(line.matches('\n').count(), 1);
    let entry = parse_line(&line).unwrap();
    assert_eq!(entry.get("reason"), Some("error: a b c https://h/x?…"));
}

#[test]
fn the_log_rotates_at_its_bound_keeping_five() {
    let dir = scratch("rotate");
    let path = dir.join(LOG_NAME);
    let big = "y".repeat(usize::try_from(ROTATE_AT_BYTES).unwrap());
    for n in 1..=KEEP_ROTATED {
        std::fs::write(rotated_path(&path, n), format!("old {n}\n")).unwrap();
    }
    std::fs::write(&path, &big).unwrap();
    append_line(&dir, "no-lock\n", false).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > ROTATE_AT_BYTES);
    assert_eq!(
        std::fs::read_to_string(rotated_path(&path, 1)).unwrap(),
        "old 1\n"
    );
    append_line(&dir, "fresh\n", true).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
    assert!(
        std::fs::read_to_string(rotated_path(&path, 1))
            .unwrap()
            .ends_with("no-lock\n")
    );
    for n in 2..=KEEP_ROTATED {
        assert_eq!(
            std::fs::read_to_string(rotated_path(&path, n)).unwrap(),
            format!("old {}\n", n - 1)
        );
    }
    assert!(
        !rotated_path(&path, KEEP_ROTATED + 1).exists(),
        "only five kept"
    );
    assert!(!rotate_if_due(&path).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_grammar_is_shared_by_any_kind_with_its_own_cap() {
    let long = "e".repeat(1000);
    let line = render_record(
        1_790_000_000,
        "update",
        9,
        &[
            ("outcome", "warn"),
            ("title", "Update failed"),
            ("detail", &long),
        ],
        2000,
    );
    let entry = parse_record(&line).unwrap();
    assert_eq!(entry.kind, "update");
    assert_eq!(entry.pid, 9);
    assert_eq!(
        entry.get("detail"),
        Some(long.as_str()),
        "the whole value, no cut"
    );
    assert_eq!(parse_line(&line), None, "not a package-log kind");
    assert_eq!(sanitize_capped(&long, 10).chars().count(), 11);
    for foreign in [
        "2026-09-21T14:13:20Z	Upper	pid=1",
        "2026-09-21T14:13:20Z		pid=1",
        "2026-09-21T14:13:20Z	kind two	pid=1",
    ] {
        assert_eq!(parse_record(foreign), None, "{foreign:?}");
    }
    let dir = scratch("records");
    let path = dir.join("records.log");
    assert!(read_records(&path, 1024).unwrap().is_empty());
    assert!(read_records(&dir, 1024).is_err());
    let one = render_record(1_790_000_000, "app", 1, &[("title", "a")], 100);
    let two = render_record(1_790_000_001, "app", 1, &[("title", "b")], 100);
    std::fs::write(&path, format!("{one}{two}torn")).unwrap();
    let all = read_records(&path, 1024).unwrap();
    assert_eq!(
        all.iter()
            .filter_map(|e| e.get("title"))
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let cut = read_records(&path, (two.len() + 8) as u64).unwrap();
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].get("title"), Some("b"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_tail_is_newest_first_across_the_rotation() {
    let dir = scratch("tail");
    let path = dir.join(LOG_NAME);
    let line = |n: i64| {
        render(
            1_790_000_000 + n,
            1,
            &Event::PassStart {
                lane: "window",
                verb: "update",
            },
        )
    };
    std::fs::write(rotated_path(&path, 1), (0..3).map(line).collect::<String>()).unwrap();
    std::fs::write(&path, format!("{}{}torn-half", line(3), line(4))).unwrap();
    let tail = read_tail(&dir, 4);
    let ats: Vec<i64> = tail.iter().filter_map(|e| e.at).collect();
    assert_eq!(
        ats,
        vec![1_790_000_004, 1_790_000_003, 1_790_000_002, 1_790_000_001]
    );
    assert_eq!(read_tail(&dir, 50).len(), 5);
    assert!(read_tail(&dir.join("absent"), 50).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

fn row(build: Option<u64>, state: &str) -> crate::ProgramStatus {
    crate::ProgramStatus {
        installed_build: build,
        state: state.to_string(),
        tree_root: String::new(),
    }
}

fn status(rows: &[(&str, crate::ProgramStatus)]) -> Status {
    Status {
        schema: 1,
        last_index_build: 44,
        programs: rows
            .iter()
            .map(|(n, r)| ((*n).to_string(), r.clone()))
            .collect(),
        ..Default::default()
    }
}

#[test]
fn transitions_are_moves_and_changes_of_reason() {
    use crate::state;
    let v = |s: &str| crate::vendor_direct::Version::parse(s).unwrap().build_id();
    let before = status(&[
        ("ay", row(Some(1971), &state::managed(1971, 43))),
        (
            "claude",
            row(
                Some(v("2.1.278")),
                &state::vendor_managed("2.1.278", "Anthropic"),
            ),
        ),
        (
            "codex",
            row(
                Some(v("0.156.0")),
                &state::vendor_managed("0.156.0", "OpenAI"),
            ),
        ),
        ("gh", row(Some(12), &state::managed(12, 43))),
        ("ty", row(Some(5), "error: stage: disk full")),
        ("*toolset*", row(None, "unavailable: index unreachable")),
    ]);
    let after = status(&[
        ("ay", row(Some(1971), &state::managed(1971, 44))),
        (
            "claude",
            row(
                Some(v("2.1.280")),
                &state::vendor_managed("2.1.280", "Anthropic"),
            ),
        ),
        (
            "codex",
            row(
                Some(v("0.156.0")),
                &state::vendor_kept("0.156.0", "held by local pin"),
            ),
        ),
        ("nn", row(Some(108), &state::managed(108, 44))),
        ("ty", row(Some(5), &state::managed(5, 44))),
        ("*toolset*", row(None, "")),
    ]);
    let got = transitions(Some(&before), &after);
    type Summary = (String, &'static str, Option<String>, Option<String>, String);
    let summary: Vec<Summary> = got
        .iter()
        .map(|t| {
            (
                t.program.clone(),
                t.result,
                t.from.clone(),
                t.to.clone(),
                t.source.clone(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "claude".into(),
                "updated",
                Some("2.1.278".into()),
                Some("2.1.280".into()),
                "Anthropic latest".into()
            ),
            (
                "codex".into(),
                "held",
                Some("0.156.0".into()),
                None,
                "OpenAI latest".into()
            ),
            (
                "gh".into(),
                "removed",
                Some("build 12".into()),
                None,
                "ALab index 43".into()
            ),
            (
                "nn".into(),
                "installed",
                None,
                Some("build 108".into()),
                "ALab index 44".into()
            ),
            (
                "ty".into(),
                "current",
                Some("build 5".into()),
                None,
                "ALab index 44".into()
            ),
        ]
    );
    assert_eq!(got[1].reason, "held by local pin");
    let refused = "error: claude 2.1.281 refused: digest mismatch — keeping 2.1.280";
    let mut once = after.clone();
    once.programs
        .insert("claude".into(), row(Some(v("2.1.280")), refused));
    let t = transitions(Some(&after), &once);
    assert_eq!(t.len(), 1);
    assert_eq!((t[0].result, t[0].reason.as_str()), ("refused", refused));
    assert!(transitions(Some(&once), &once).is_empty());
    let fresh = transitions(None, &once);
    let result_of = |p: &str| fresh.iter().find(|t| t.program == p).map(|t| t.result);
    assert_eq!(result_of("claude"), Some("refused"));
    assert_eq!(result_of("ay"), Some("installed"));
}

#[test]
fn a_status_write_for_the_bound_store_logs_its_transitions() {
    let dir = scratch("hook");
    let store = scratch("hook-store");
    let other = scratch("hook-other");
    let _bound = test_bind::bind(dir.clone(), store.clone());
    let layout = Layout {
        prefix: store.clone(),
    };
    let st = status(&[("ay", row(Some(1971), &crate::state::managed(1971, 44)))]);
    crate::status::write(&layout, &st).unwrap();
    crate::status::write(
        &Layout {
            prefix: other.clone(),
        },
        &st,
    )
    .unwrap();
    let tail = read_tail(&dir, 10);
    assert_eq!(tail.len(), 1, "{tail:?}");
    assert_eq!(tail[0].get("program"), Some("ay"));
    assert_eq!(tail[0].get("result"), Some("installed"));
    crate::status::write(&layout, &st).unwrap();
    assert_eq!(read_tail(&dir, 10).len(), 1);
    for d in [&dir, &store, &other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn lines_inside_a_pass_carry_their_offset_into_it() {
    let dir = scratch("clock");
    let store = scratch("clock-store");
    let other = scratch("clock-other");
    let _bound = test_bind::bind(dir.clone(), store.clone());
    let layout = Layout {
        prefix: store.clone(),
    };
    let start = Event::PassStart {
        lane: "typed",
        verb: "update",
    };
    let index = Event::IndexResolved {
        heads: 3,
        rounds: 2,
        gets: 8,
        found: "atpkg-index-45",
    };
    append(&layout, &start);
    append_in_pass(&index);
    start_pass_clock(&Layout {
        prefix: other.clone(),
    });
    append_in_pass(&index);
    start_pass_clock(&layout);
    append(&layout, &start);
    append_in_pass(&index);
    stop_pass_clock();
    append_in_pass(&index);
    let lines = read_tail(&dir, 10);
    let kinds: Vec<(&str, Option<&str>)> = lines
        .iter()
        .rev()
        .map(|e| (e.kind.as_str(), e.get("t_ms")))
        .collect();
    assert_eq!(kinds.len(), 3, "{lines:?}");
    assert_eq!(kinds[0], (kind::PASS_START, None), "before the pass");
    assert_eq!(kinds[1].0, kind::PASS_START);
    assert_eq!(kinds[2].0, kind::INDEX_RESOLVED);
    let t: Vec<u64> = kinds[1..]
        .iter()
        .map(|(_, t)| t.expect("stamped inside the pass").parse().unwrap())
        .collect();
    assert!(t[0] <= t[1], "{t:?}");
    let resolved = &lines[0];
    assert_eq!(resolved.get("heads"), Some("3"));
    assert_eq!(resolved.get("rounds"), Some("2"));
    assert_eq!(resolved.get("gets"), Some("8"));
    assert_eq!(resolved.get("found"), Some("atpkg-index-45"));
    for d in [&dir, &store, &other] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// NEGATIVE CONTROL: `read_tail_of` walks all 5 rotated files (`packages.log.1`..`packages.log.5`)
/// when `max` is not yet satisfied, `redact_tokens` redacts credential keys inside `,`/`;`/`&`
/// compound tokens without swallowing neighboring fields, and `parse_record` rejects unescaped
/// control/bidi/zero-width bytes in keys or values while accepting sanitized records.
#[test]
fn tail_scans_all_five_rotations_compound_tokens_redact_and_dirty_records_refuse() {
    let dir = scratch("tail-five");
    let path = dir.join(LOG_NAME);
    let line = |n: i64| {
        render(
            1_790_000_000 + n,
            1,
            &Event::PassStart {
                lane: "window",
                verb: "update",
            },
        )
    };
    std::fs::write(&path, line(6)).unwrap();
    for n in 1..=KEEP_ROTATED {
        std::fs::write(rotated_path(&path, n), line(6 - i64::from(n))).unwrap();
    }
    let all = read_tail(&dir, 10);
    let ats: Vec<i64> = all.iter().filter_map(|e| e.at).collect();
    assert_eq!(
        ats,
        vec![
            1_790_000_006,
            1_790_000_005,
            1_790_000_004,
            1_790_000_003,
            1_790_000_002,
            1_790_000_001,
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        sanitize("user=alice,token=sec123,role=admin"),
        "user=alice,token=[redacted],role=admin"
    );
    assert_eq!(sanitize("a=1&api_key=xyz"), "a=1&api_key=[redacted]");

    assert_eq!(
        parse_record("2026-09-21T14:13:20Z\tprogram\tpid=1\treason=bad\u{1b}[31m\n"),
        None
    );
    assert_eq!(
        parse_record("2026-09-21T14:13:20Z\tprogram\tpid=1\treason=bad\u{202E}\n"),
        None
    );
    assert_eq!(
        parse_record("2026-09-21T14:13:20Z\tprogram\tpid=1\treason=clean\n")
            .and_then(|e| e.get("reason").map(str::to_string)),
        Some("clean".to_string())
    );
}
