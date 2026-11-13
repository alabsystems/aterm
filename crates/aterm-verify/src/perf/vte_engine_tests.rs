// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn payload(name: &str, setup: &[u8], once: &[u8]) -> Payload {
    Payload {
        name: name.into(),
        setup: setup.to_vec(),
        once: once.to_vec(),
    }
}

fn stream_of(payloads: &[Payload]) -> Vec<u8> {
    let mut s = Vec::new();
    for p in payloads {
        s.extend_from_slice(format!("Loading {}...\n", p.name).as_bytes());
        s.extend_from_slice(RESET);
        s.extend_from_slice(&p.setup);
        for _ in 0..(MIN_BYTES - 1) / p.once.len() + 1 {
            s.extend_from_slice(&p.once);
        }
        s.extend_from_slice(RESET);
    }
    s
}

#[test]
fn the_capture_matches_the_stream_vtebench_writes_for_it() {
    let payloads = [
        payload("scrolling", b"y\ny\n", b"y\n"),
        payload("scrolling_bottom_region", b"\x1b[?1049h\x1b[1;23r", b"y\n"),
        payload("unicode", b"\x1b[?1049h", "\u{2603}\n".as_bytes()),
    ];
    let stream = stream_of(&payloads);
    assert_eq!(cross_check(&stream, &payloads), Ok(()));
    // A run whose length does not divide 1 MiB repeats past it, as vtebench's loader does.
    assert_eq!(repeat_to_min(b"abc").len(), MIN_BYTES + 2);
}

#[test]
fn a_planted_divergence_is_caught() {
    let payloads = [
        payload("scrolling", b"", b"y\n"),
        payload("scrolling_bottom_region", b"\x1b[?1049h\x1b[1;23r", b"y\n"),
    ];
    let stream = stream_of(&payloads);
    let planted = [
        payload("scrolling", b"", b"y\n"),
        payload("scrolling_bottom_region", b"\x1b[?1049h\x1b[1;39r", b"y\n"),
    ];
    let e = cross_check(&stream, &planted).expect_err("the region differs");
    assert!(e.contains("scrolling_bottom_region"), "{e}");
    let e = cross_check(&stream, &payloads[..1]).expect_err("a benchmark left uncaptured");
    assert!(e.contains("past the last"), "{e}");
    let e = cross_check(&stream_of(&payloads[..1]), &payloads).expect_err("one more");
    assert!(e.contains("scrolling_bottom_region"), "{e}");
}

#[test]
fn a_report_without_performance_core_cycles_is_not_judged() {
    let report = |counters: bool, skip: Option<&str>| {
        let mut text = format!("{{\"counters\":{counters}");
        for key in LANE
            .keys
            .iter()
            .filter(|k| skip.is_none_or(|s| !k.starts_with(s)))
        {
            text.push_str(&format!(",\"{key}\":5.0"));
        }
        text.push('}');
        aterm_json::from_str::<aterm_json::Value>(&text).expect("report")
    };
    assert_eq!(unjudgeable(&LANE, &report(true, None)), None);
    let why = unjudgeable(&LANE, &report(false, None)).expect("no counters");
    assert!(why.contains("no cycles per core type"), "{why}");
    let why = unjudgeable(&LANE, &report(true, Some("sync_medium_cells"))).expect("one missing");
    assert!(
        why.contains("no sample of sync_medium_cells counted"),
        "{why}"
    );
}

/// The loader's view of a script: its stdout alone (stderr is piped apart), no stdin, and
/// an empty benchmark refused rather than dropped.
#[test]
fn scripts_run_as_vtebench_runs_them() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = Scratch::new("atv-vte-engine").expect("scratch");
    let benchmarks = dir.path().join("benchmarks");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&out).expect("out");
    let script = |bench: &str, name: &str, body: &str| {
        let d = benchmarks.join(bench);
        std::fs::create_dir_all(&d).expect("bench dir");
        let p = d.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("script");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    };
    script("a", "setup", "printf 'S'; echo noise >&2");
    script("a", "benchmark", "read x; printf 'B%s' \"$x\"");
    script("b", "benchmark", "printf 'only'");
    capture_scripts_on(&benchmarks, &out, script_interpreter).expect("captures");
    assert_eq!(std::fs::read(out.join("a.setup")).expect("a.setup"), b"S");
    assert_eq!(std::fs::read(out.join("a.bench")).expect("a.bench"), b"B");
    assert!(!out.join("b.setup").exists());
    assert_eq!(
        std::fs::read(out.join("b.bench")).expect("b.bench"),
        b"only"
    );
    assert_eq!(payload_names(&out).expect("names"), ["a", "b"]);
    script("c", "benchmark", "true");
    let e = capture_scripts_on(&benchmarks, &out, script_interpreter).expect_err("empty");
    assert!(e.contains("c's benchmark printed nothing"), "{e}");
}

fn script_interpreter(script: &Path) -> Command {
    let mut shell = command(aterm_uds::deadline::stand_in(Path::new("/bin/sh")));
    shell.arg0("sh").arg(script);
    shell
}

/// A setup or benchmark can emit a nonempty prefix and then fail. Neither prefix is a
/// complete payload; successful ordinary captures must still preserve stdout exactly.
#[test]
fn failed_scripts_cannot_become_nonempty_payloads() {
    let dir = Scratch::new("atv-vte-script-status").expect("scratch");
    let mut accepted = Vec::new();
    for failed in ["setup", "benchmark"] {
        let root = dir.path().join(failed);
        let benchmarks = root.join("benchmarks");
        let scenario = benchmarks.join("a");
        let ordinary = root.join("ordinary");
        let refused = root.join("refused");
        for path in [&scenario, &ordinary, &refused] {
            std::fs::create_dir_all(path).expect("owned directory");
        }
        for (name, body) in [("setup", "printf S"), ("benchmark", "printf B")] {
            std::fs::write(scenario.join(name), body).expect("script data");
        }
        capture_scripts_on(&benchmarks, &ordinary, script_interpreter).expect("ordinary success");
        assert_eq!(std::fs::read(ordinary.join("a.setup")).unwrap(), b"S");
        assert_eq!(std::fs::read(ordinary.join("a.bench")).unwrap(), b"B");
        std::fs::write(
            scenario.join(failed),
            "printf partial; printf 'fixture refusal' >&2; exit 7",
        )
        .expect("failing script data");
        match capture_scripts_on(&benchmarks, &refused, script_interpreter) {
            Ok(()) => accepted.push(failed),
            Err(why) => {
                assert!(
                    why.contains(failed) && why.contains("fixture refusal"),
                    "{why}"
                );
                assert!(!refused.join("a.bench").exists(), "no completed payload");
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "failed scripts were accepted as complete payloads: {accepted:?}"
    );
}
