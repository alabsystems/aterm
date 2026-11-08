// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::libtest::{self, COULD_NOT_RUN_SENTINEL};
use crate::libtest_fixture::{FIXTURE, NONCE};

const HEADER: &str = "     Running unittests src/lib.rs (target/debug/deps/aterm_verify-0)\n";
const RERUN: &str = "error: test failed, to rerun pass `-p aterm-verify --lib`\n";

fn event(v: aterm_json::Value) -> String {
    format!("{v}\n")
}

/// Only a whole libtest event is one: no other `type`, no unknown key, no line cut short, and no
/// test event without a name. NEGATIVE CONTROL: libtest's own line is one.
#[test]
fn only_a_whole_libtest_line_is_an_event() {
    let ok = r#"{ "type": "test", "name": "a::b", "event": "ok" }"#;
    assert_eq!(parse_event(ok).map(|e| e.event), Some("ok".into()));
    for not in [
        r#"{ "type": "test", "name": "a::b", "event": "ok""#,
        r#"{ "type": "build", "name": "a::b", "event": "ok" }"#,
        r#"{ "type": "test", "name": "a::b", "event": "ok", "extra": 1 }"#,
        r#"{ "type": "test", "event": "ok" }"#,
        r#"x { "type": "test", "name": "a::b", "event": "ok" }"#,
        "test a::b ... ok",
    ] {
        assert_eq!(parse_event(not), None, "{not}");
    }
}

/// An event that output written past capture ran into still reads as one, the output before it
/// kept on its own line; a suite start that names its shuffle seed and a timed failure's reason
/// read too, the reason as the case's note. NEGATIVE CONTROL: a line with no event in it is none,
/// and the strict reader still refuses the glued line whole.
#[test]
fn an_event_after_uncaptured_output_on_its_line_still_reads() {
    let glued = r#"progress 40%{ "type": "test", "name": "a::red", "event": "failed", "stdout": "boom {\"type\": x}\n" }"#;
    let (before, ev) = line_event(glued).expect("the event at the line's end");
    assert_eq!(
        (before, ev.name.as_deref()),
        ("progress 40%", Some("a::red"))
    );
    assert_eq!(parse_event(glued), None);
    assert!(line_event("progress 40% { not an event").is_none());
    let raw = [
        HEADER,
        "{ \"type\": \"suite\", \"event\": \"started\", \"test_count\": 1, \"shuffle_seed\": 7 }\n",
        "{ \"type\": \"test\", \"event\": \"started\", \"name\": \"a::red\" }\n",
        glued,
        "\n",
        "{ \"type\": \"test\", \"name\": \"a::slow\", \"event\": \"failed\", \"reason\": \"time limit exceeded\" }\n",
        "{ \"type\": \"suite\", \"event\": \"failed\", \"passed\": 0, \"failed\": 2, \"ignored\": 0, \"measured\": 0, \"filtered_out\": 0, \"exec_time\": 0.1 }\n",
    ]
    .concat();
    let log = render(&raw, None);
    assert!(log.contains("\nrunning 1 test\n"), "{log}");
    assert!(
        log.contains("progress 40%\ntest a::red ... FAILED\n"),
        "{log}"
    );
    assert!(
        log.contains("---- a::slow stdout ----\nnote: time limit exceeded\n"),
        "{log}"
    );
    let failed = libtest::failed_tests(&format!("{log}{RERUN}")).expect("accounted for");
    assert_eq!(
        failed.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["a::red", "a::slow"]
    );
}

/// A STOPPED BINARY'S LOG: the finished cases' blocks and their list, a `test result: FAILED.`
/// line every reader takes, and, outside every block, the line that names what had not
/// finished. NEGATIVE CONTROL: without `stopped`, the same events (a crash) get no result line.
#[test]
fn a_stopped_binary_reads_as_a_failed_report_with_its_stop_said() {
    let raw = [
        HEADER.to_owned(),
        event(aterm_json::json!({"type": "suite", "event": "started", "test_count": 4})),
        event(aterm_json::json!({"type": "test", "event": "started", "name": "a::ok"})),
        event(aterm_json::json!({"type": "test", "event": "ok", "name": "a::ok"})),
        event(aterm_json::json!({"type": "test", "event": "started", "name": "a::red"})),
        event(aterm_json::json!({"type": "test", "event": "started", "name": "a::nap"})),
        "uncaptured line\n".to_owned(),
        event(aterm_json::json!({
            "type": "test", "event": "failed", "name": "a::red",
            "stdout": "\nthread 'a::red' panicked at src/a.rs:1:1:\nboom\n"
        })),
    ]
    .concat();
    let stop = Stopped {
        spec: "-p x --lib".into(),
        case: "a::red".into(),
    };
    let log = render(&raw, Some(&stop));
    assert!(log.starts_with(HEADER), "{log}");
    assert!(
        log.contains(
            "\nrunning 4 tests\ntest a::ok ... ok\nuncaptured line\ntest a::red ... FAILED\n"
        ),
        "{log}"
    );
    assert!(
        log.contains(
            "---- a::red stdout ----\n\nthread 'a::red' panicked at src/a.rs:1:1:\nboom\n\n"
        ),
        "{log}"
    );
    assert!(log.contains(
        "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; stopped by verify at its first red case `a::red`"
    ), "{log}");
    assert!(log.contains(
        "aterm-verify: stopped -p x --lib at its first red case `a::red`; 2 of 4 cases had not finished: a::nap; 1 not started"
    ), "{log}");
    let failed = libtest::failed_tests(&format!("{log}{RERUN}")).expect("accounted for");
    assert_eq!(failed.len(), 1);
    assert!(failed[0].block.contains("boom"));
    assert_eq!(libtest::tests_run(&log), Some(4));
    assert!(!libtest::has_ceiling_kill(&log));
    let crashed = render(&raw, None);
    assert!(!crashed.contains("test result:"), "{crashed}");
    assert!(crashed.contains("---- a::red stdout ----"), "{crashed}");
}

/// Replace each ` (<digits>)` after a thread's name and each `finished in <t>s`, which differ
/// between two runs of one binary.
fn normalised(log: &str) -> String {
    let mut out = String::with_capacity(log.len());
    for line in log.split_inclusive('\n') {
        let mut line = line.to_owned();
        if let Some(at) = line.find("' (")
            && let Some(close) = line[at + 3..].find(')')
            && line[at + 3..at + 3 + close]
                .bytes()
                .all(|b| b.is_ascii_digit())
        {
            line.replace_range(at + 1..at + 4 + close, "");
        }
        if let Some(at) = line.find("finished in ") {
            line.replace_range(at.., "finished in 0.00s\n");
        }
        out.push_str(&line);
    }
    out
}

/// What every reader of a test binary's log says of `log`, as one comparable value.
fn readings(log: &str) -> String {
    let log = format!("{log}{RERUN}");
    let run = crate::exec::Run {
        hung: None,
        ok: false,
        output: log.clone(),
        code: Some(101),
        ..crate::exec::Run::default()
    };
    let (findings, refused) =
        crate::findings::split_test_findings("test", &log, &crate::findings::Rerun::default());
    format!(
        "failed_tests: {:#?}\nenvironment_refusals: {:?}\nout_of_disk: {}\ntests_run: {:?}\n\
         passed_by_target: {:?}\nfindings: {:#?}\nrefused: {:?}\nenvironment_failure: {:?}",
        libtest::failed_tests(&log),
        libtest::environment_refusals(&log),
        libtest::out_of_disk(&log),
        libtest::tests_run(&log),
        libtest::passed_by_target(&log),
        findings
            .iter()
            .map(|f| (&f.id, &f.rerun, &f.message))
            .collect::<Vec<_>>(),
        refused,
        run.environment_failure(),
    )
}

/// The fixture binary (this one, filtered to [`crate::libtest_fixture`]) run once, its cases
/// acting, at one test thread, with `extra` flags right after it: its whole output, under a
/// `Running` header.
fn fixture_run(extra: &[&str]) -> String {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    std::fs::write(dir.path().join("napper.release"), "").expect("the napper's release");
    let out = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test binary"))
        .args(extra)
        .args(["libtest_fixture::", "--test-threads=1"])
        .env(FIXTURE, dir.path())
        .env_remove("RUST_TEST_NOCAPTURE")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("the fixture ran");
    assert_eq!(out.status.code(), Some(101), "the fixture fails");
    format!(
        "{HEADER}{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// RENDERED JSON READS AS LIBTEST'S OWN REPORT, against the real toolchain: the fixture binary run
/// once in the human format and once in JSON ([`render`]) gives every reader the same answer
/// (failed cases with their refusal flags and blocks, refusals, full disk, test counts, passes by
/// target, findings), thread ids and times aside. NEGATIVE CONTROLS: a renderer that drops each
/// `note:` (a `should_panic` mismatch's), one that drops a block, and one that reorders the
/// blocks each read differently. (The closing list's order is no control: every reader sorts it.)
#[test]
fn rendered_json_reads_as_libtests_human_report() {
    let human = normalised(&fixture_run(&[]));
    let raw = fixture_run(&["-Z", "unstable-options", "--format", "json"]);
    let json = normalised(&render(&raw, None));
    let want = readings(&human);
    assert_eq!(readings(&json), want, "human:\n{human}\njson:\n{json}");
    assert!(
        want.contains(NONCE) && want.contains(COULD_NOT_RUN_SENTINEL),
        "{want}"
    );
    let drop_note: String = json
        .split_inclusive('\n')
        .filter(|l| !l.starts_with("note: panic did not contain"))
        .collect();
    let drop_block = {
        let at = json
            .find("---- libtest_fixture::c_plain stdout ----")
            .expect("a block");
        let end = at + json[at..].find("\n\n").expect("its end") + 2;
        format!("{}{}", &json[..at], &json[end..])
    };
    let reordered = {
        let a = json
            .find("---- libtest_fixture::c_plain stdout ----")
            .expect("first");
        let b = json
            .find("---- libtest_fixture::c_refused stdout ----")
            .expect("second");
        let c = json
            .find("---- libtest_fixture::d_enospc stdout ----")
            .expect("third");
        format!("{}{}{}{}", &json[..a], &json[b..c], &json[a..b], &json[c..])
    };
    for (mutant, log) in [
        ("drop note", drop_note),
        ("drop block", drop_block),
        ("reorder", reordered),
    ] {
        assert_ne!(readings(&log), want, "{mutant} went unseen:\n{log}");
    }
}

/// THE LIVE DECISION AND THE FINAL ONE AGREE: for every sequence of at most three failed blocks
/// drawn from plain, refused, out of disk and both, each prefix [`libtest::decisive`] holds of
/// makes the binary's whole rendered log, and its log stopped at that prefix, a FAIL (no
/// environmental reading). NEGATIVE CONTROL: a sequence no prefix of which is decisive (all
/// refused, or all out of disk) reads NOT RUN, so the guard is not vacuous.
#[test]
fn a_decisive_prefix_is_always_the_binarys_fail() {
    let blocks = [
        "\nthread 'x' panicked at src/a.rs:1:1:\nassertion failed\n".to_owned(),
        format!("\nthread 'x' panicked at src/a.rs:1:1:\n{COULD_NOT_RUN_SENTINEL} — no fish\n"),
        format!(
            "\nthread 'x' panicked at src/a.rs:1:1:\n{} (os error 28)\n",
            crate::exec::ENOSPC_SENTENCE
        ),
        format!(
            "\nthread 'x' panicked at src/a.rs:1:1:\n{COULD_NOT_RUN_SENTINEL} — {} (os error 28)\n",
            crate::exec::ENOSPC_SENTENCE
        ),
    ];
    let mut seqs: Vec<Vec<usize>> = Vec::new();
    for a in 0..4 {
        seqs.push(vec![a]);
        for b in 0..4 {
            seqs.push(vec![a, b]);
            for c in 0..4 {
                seqs.push(vec![a, b, c]);
            }
        }
    }
    let stream = |seq: &[usize], end: bool| {
        let mut raw = format!(
            "{HEADER}{}",
            event(
                aterm_json::json!({"type": "suite", "event": "started", "test_count": seq.len()})
            )
        );
        for (i, &k) in seq.iter().enumerate() {
            let name = format!("t::c{i}");
            raw.push_str(&event(
                aterm_json::json!({"type": "test", "event": "started", "name": name}),
            ));
            raw.push_str(&event(aterm_json::json!({
                "type": "test", "event": "failed", "name": name, "stdout": blocks[k]
            })));
        }
        if end {
            raw.push_str(&event(aterm_json::json!({
                "type": "suite", "event": "failed", "passed": 0, "failed": seq.len(),
                "ignored": 0, "measured": 0, "filtered_out": 0, "exec_time": 0.01
            })));
        }
        raw
    };
    let environmental = |log: String| {
        crate::exec::Run {
            hung: None,
            ok: false,
            output: format!("{log}{RERUN}"),
            code: Some(101),
            ..crate::exec::Run::default()
        }
        .environment_failure()
    };
    let mut decisive_seen = 0;
    for seq in &seqs {
        let marks: Vec<libtest::Marks> = seq.iter().map(|&k| libtest::marks(&blocks[k])).collect();
        for n in 1..=seq.len() {
            if !libtest::decisive(&marks[..n]) {
                continue;
            }
            decisive_seen += 1;
            let full = render(&stream(seq, true), None);
            assert_eq!(environmental(full.clone()), None, "{seq:?} at {n}:\n{full}");
            let stop = Stopped {
                spec: "-p aterm-verify --lib".into(),
                case: format!("t::c{}", n - 1),
            };
            let stopped = render(&stream(&seq[..n], false), Some(&stop));
            assert_eq!(
                environmental(stopped.clone()),
                None,
                "{seq:?} stopped at {n}:\n{stopped}"
            );
        }
    }
    assert!(decisive_seen > 0);
    for k in [1, 2] {
        let full = render(&stream(&[k, k, k], true), None);
        assert!(environmental(full.clone()).is_some(), "{full}");
    }
}
