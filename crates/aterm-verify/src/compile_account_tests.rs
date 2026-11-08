// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compile account's reading of Cargo's messages, its output bytes, and its line and event.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Account, Units, human, output_bytes, read};

/// One `compiler-artifact` message naming `files`, `fresh` as given (`None`: no `fresh` field).
fn artifact(name: &str, fresh: Option<bool>, files: &[&Path]) -> String {
    let mut message = aterm_json::json!({
        "reason": "compiler-artifact",
        "package_id": format!("path+file:///ws#{name}@0.1.0"),
        "target": {"name": name, "kind": ["lib"]},
        "filenames": files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
    });
    if let (Some(fresh), Some(object)) = (fresh, message.as_object_mut()) {
        object.insert("fresh".to_owned(), aterm_json::Value::from(fresh));
    }
    aterm_json::to_string(&message).expect("encoded")
}

/// A call's units are what its artifact messages say: `fresh: true` fresh, `fresh: false`
/// compiled, and an artifact with no `fresh`, a `fresh` that is not a boolean, or repeated fields
/// unknown, never either. Other messages, a repeated dispatch and a person's lines are no unit.
/// NEGATIVE CONTROL: read as the shaped compile reads freshness (`fresh` missing is `false`), the
/// fresh-less artifact counts as compiled and the unknown count falls to 1.
#[test]
fn units_are_fresh_compiled_or_unknown_and_never_guessed() {
    let none: [&Path; 0] = [];
    let stdout = [
        artifact("a", Some(true), &none),
        artifact("b", Some(false), &none),
        artifact("c", Some(true), &none),
        artifact("d", None, &none),
        r#"{"reason":"compiler-artifact","target":{"name":"e","kind":["lib"]},"fresh":"yes","filenames":[]}"#.to_owned(),
        r#"{"reason":"compiler-artifact","fresh":true,"fresh":false,"filenames":[]}"#.to_owned(),
        r#"{"reason":"compilation-unit-dispatched","mode":"build","mode":"check"}"#.to_owned(),
        r#"{"reason":"build-script-executed","package_id":"x"}"#.to_owned(),
        r#"{"reason":"compiler-message","message":{"rendered":"warning"}}"#.to_owned(),
        r#"{"reason":"build-finished","success":true}"#.to_owned(),
        "   Compiling a v0.1.0".to_owned(),
        "{not json".to_owned(),
    ]
    .join("\n");
    assert_eq!(
        read(&stdout).units,
        Units {
            fresh: 2,
            compiled: 1,
            unknown: 3,
        }
    );
    assert_eq!(read("").units, Units::default());
}

/// JSON spelling and unrelated values cannot decide whether an ambiguous message names an
/// artifact. Every decoded top-level artifact reason counts once as unknown, in either order.
#[test]
fn ambiguous_units_use_decoded_top_level_reasons() {
    for wire in [
        r#"{"reason":"compiler-\u0061rtifact","fresh":false,"fresh":true}"#,
        r#"{"re\u0061son":"compiler-artifact","fresh":false,"fresh":true}"#,
        r#"{"reason":"compilation-unit-dispatched","reason":"compiler-artifact"}"#,
        r#"{"reason":"compiler-artifact","reason":"compilation-unit-dispatched"}"#,
        r#"{"reason":"build-finished","re\u0061son":"compiler-\u0061rtifact"}"#,
    ] {
        let artifacts = read(wire);
        assert_eq!(
            artifacts.units,
            Units {
                unknown: 1,
                ..Units::default()
            },
            "{wire}"
        );
        assert!(artifacts.written.is_empty());
    }
    for wire in [
        r#"{"reason":"compilation-unit-dispatched","note":"compiler-artifact","mode":"build","mode":"check"}"#,
        r#"{"reason":"compilation-unit-dispatched","note":{"reason":"compiler-artifact","x":1,"x":2}}"#,
        r#"{"reason":"compilation-unit-dispatched","reason":"build-finished","note":"compiler-artifact"}"#,
    ] {
        let artifacts = read(wire);
        assert_eq!(artifacts.units, Units::default(), "{wire}");
        assert!(artifacts.written.is_empty());
    }
}

/// A call's output bytes are the regular files each artifact it did not report fresh names, one
/// metadata read each: a path named twice, and a hard link of a file already counted (an uplifted
/// binary beside its `deps/` copy), count once; a directory (`.dSYM`), a symlink and a file that is
/// gone add nothing; a fresh artifact's files add nothing; and nothing under a directory is read.
/// NEGATIVE CONTROLS: counted by path alone, the hard link adds its 300 bytes again; read through
/// symlinks, the symlink adds the 50 bytes of the file it names.
#[test]
fn output_bytes_are_the_files_a_call_wrote_once_each_and_never_a_walk() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let at = |name: &str| dir.path().join(name);
    std::fs::write(at("liba.rlib"), [0_u8; 300]).expect("rlib");
    std::fs::write(at("a.rmeta"), [0_u8; 20]).expect("rmeta");
    std::fs::write(at("fresh.rlib"), [0_u8; 4000]).expect("fresh");
    std::fs::create_dir_all(at("bin.dSYM/Contents")).expect("dsym");
    std::fs::write(at("bin.dSYM/Contents/dwarf"), [0_u8; 9000]).expect("dwarf");
    #[cfg(unix)]
    {
        std::fs::hard_link(at("liba.rlib"), at("uplifted.rlib")).expect("hard link");
        std::fs::write(at("elsewhere.rlib"), [0_u8; 50]).expect("symlink target");
        std::os::unix::fs::symlink(at("elsewhere.rlib"), at("link.rlib")).expect("symlink");
    }
    let written: Vec<PathBuf> = [
        "liba.rlib",
        "a.rmeta",
        "liba.rlib",
        "uplifted.rlib",
        "link.rlib",
        "bin.dSYM",
        "gone.rlib",
    ]
    .iter()
    .map(|n| at(n))
    .collect();
    let refs: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
    let stdout = [
        artifact("a", Some(false), &refs[..3]),
        artifact("b", None, &refs[3..]),
        artifact("fresh", Some(true), &[at("fresh.rlib").as_path()]),
    ]
    .join("\n");
    let read = read(&stdout);
    assert_eq!(
        read.written, written,
        "every file a non-fresh artifact names"
    );
    assert_eq!(output_bytes(&read.written), 320);
    assert_eq!(output_bytes(&[]), 0);
}

/// A person's lines stay and Cargo's messages go: what a build script printed, a JSON line that is
/// no compiler message, and the line order are kept.
#[test]
fn human_keeps_every_line_but_cargos_messages() {
    let none: [&Path; 0] = [];
    let stdout = format!(
        "first\n{}\n{{\"reason\":\"build-finished\",\"success\":true}}\n{{\"note\":1}}\nlast\n",
        artifact("a", Some(true), &none)
    );
    assert_eq!(human(&stdout), "first\n{\"note\":1}\nlast\n");
    assert_eq!(human(""), "");
}

/// The line and the event carry the same numbers, and what was not read is said as unread or
/// unreported (`null` in the event), never as 0. NEGATIVE CONTROL: an account with every reading
/// gives every field a number.
#[test]
fn the_compile_line_and_event_agree_and_never_print_an_unread_zero() {
    let usage = crate::exec::TreeUsage {
        cpu: Duration::from_millis(45_250),
        runnable: Duration::from_secs(60),
        starved: Duration::from_millis(1_500),
        waited: Duration::from_millis(250),
        wall: Duration::from_secs(12),
        peak_bytes: 3 << 30,
        processes: 9,
    };
    let full = Account {
        step: "test compile".to_owned(),
        call: "targo test -p a --no-run".to_owned(),
        units: Some(Units {
            fresh: 120,
            compiled: 3,
            unknown: 1,
        }),
        usage: Some(usage),
        wall: Duration::from_secs(12),
        output_bytes: Some(5 << 20),
        overlap: Some(super::Overlap {
            timed: 3,
            avg: 1.6,
            p95: 3,
            max: 3,
            waiting_avg: 0.4,
            waiting_max: 1,
            waited_s: 6.0,
        }),
    };
    let line = full.line();
    // The wall and the sizes read as the run's formatters write them everywhere.
    let wall = format!("{} wall", crate::report::duration(12.0));
    let peak = format!("{} peak", aterm_capacity::size(3 << 30));
    let out = format!("{} out", aterm_capacity::size(5 << 20));
    for said in [
        "compile test compile/targo test -p a --no-run: ",
        "120 fresh / 3 compiled / 1 unknown",
        "45.2 CPU-s",
        "1.5 starved-s",
        &wall,
        &peak,
        &out,
        "compilers at once 1.6 avg, 3 p95, 3 most (3 read)",
        "waiting for memory 0.4 avg, 1 most, 6.0 unit-s",
    ] {
        assert!(line.contains(said), "{said:?} in {line}");
    }
    let event = |a: &Account| -> aterm_json::Value {
        let e = a.event();
        aterm_json::from_str(e.strip_prefix("verify-event: ").expect("prefixed")).expect("json")
    };
    let e = event(&full);
    assert_eq!(e["schema"].as_u64(), Some(1));
    assert_eq!(e["event"].as_str(), Some("compile-finished"));
    assert_eq!(e["step"].as_str(), Some("test compile"));
    assert_eq!(e["units"]["fresh"].as_u64(), Some(120));
    assert_eq!(e["units"]["compiled"].as_u64(), Some(3));
    assert_eq!(e["units"]["unknown"].as_u64(), Some(1));
    assert_eq!(e["cpu_s"].as_f64(), Some(45.25));
    assert_eq!(e["starved_s"].as_f64(), Some(1.5));
    assert_eq!(e["waited_s"].as_f64(), Some(0.25));
    assert_eq!(e["wall_s"].as_f64(), Some(12.0));
    assert_eq!(e["peak_bytes"].as_u64(), Some(3 << 30));
    assert_eq!(e["output_bytes"].as_u64(), Some(5 << 20));
    assert_eq!(e["compilers"]["timed"].as_u64(), Some(3));
    assert_eq!(e["compilers"]["avg"].as_f64(), Some(1.6));
    assert_eq!(e["compilers"]["p95"].as_u64(), Some(3));
    assert_eq!(e["compilers"]["max"].as_u64(), Some(3));
    assert_eq!(e["compilers"]["waiting_max"].as_u64(), Some(1));
    assert_eq!(e["compilers"]["waited_s"].as_f64(), Some(6.0));

    let unread = Account::of("lint", "tippy -p a", None, None, Duration::from_secs(2));
    let line = unread.line();
    for said in [
        "units unreported",
        "CPU-s unread",
        "starved-s unread",
        "peak unread",
        "out unread",
    ] {
        assert!(line.contains(said), "{said:?} in {line}");
    }
    assert!(!line.contains(" 0 "), "{line}");
    let e = event(&unread);
    for field in [
        "units",
        "cpu_s",
        "starved_s",
        "waited_s",
        "peak_bytes",
        "output_bytes",
        "compilers",
    ] {
        assert!(e[field].is_null(), "{field}: {e}");
    }
    assert!(!line.contains("compilers at once"), "{line}");
    assert_eq!(e["wall_s"].as_f64(), Some(2.0));
}

/// A span of unit `kind`, queued and run at the given ms.
fn span(kind: &str, queued: Option<u64>, started: u64, ended: u64) -> crate::memory::UnitSpan {
    crate::memory::UnitSpan {
        unit: crate::unitgraph::UnitName {
            package: "p".to_owned(),
            kind: kind.to_owned(),
        },
        queued_at_unix_ms: queued,
        started_at_unix_ms: started,
        ended_at_unix_ms: ended,
        cpu_s: 1.5,
        peak_bytes: 7,
    }
}

/// THE OVERLAP OF A CALL'S COMPILERS IS TIME-WEIGHTED OVER ITS WALL, EACH SPAN CUT TO IT:
/// spans of 0–10 s, 5–10 s and 5–6 s over a 10 s window ran 1.6 at once on average, 3 at most,
/// and 3 at the 95th percentile of the wall (one second of it at 3); a unit queued 1–5 s waited
/// alone, 0.4 on average, and the waits sum unclamped (2 s before the window and 4 s inside). The
/// percentile is of time, not of edges: 95 s at one compiler and 5 s at four gives a p95 of 1
/// and a most of 4. NEGATIVE CONTROLS: a window of no length and a call with no span have no
/// overlap, never a zero one; three spans wholly before the window change nothing, where clamped
/// onto its edge they would open beside the first and make a most of 4.
#[test]
fn a_calls_overlap_is_time_weighted_over_its_wall() {
    let spans = [
        span("a lib", Some(0), 2_000, 12_000),
        span("b lib", Some(3_000), 7_000, 12_000),
        span("c lib", None, 7_000, 8_000),
    ];
    let o = super::Overlap::of(&spans, 2_000, 12_000).expect("an overlap");
    assert_eq!(o.timed, 3);
    assert!((o.avg - 1.6).abs() < 1e-9, "{o:?}");
    assert_eq!((o.p95, o.max), (3, 3), "{o:?}");
    assert!((o.waiting_avg - 0.4).abs() < 1e-9, "{o:?}");
    assert_eq!(o.waiting_max, 1, "{o:?}");
    assert!((o.waited_s - 6.0).abs() < 1e-9, "{o:?}");

    let skewed = [
        span("a lib", None, 0, 100_000),
        span("b lib", None, 0, 5_000),
        span("c lib", None, 0, 5_000),
        span("d lib", None, 0, 5_000),
    ];
    let o = super::Overlap::of(&skewed, 0, 100_000).expect("an overlap");
    assert_eq!((o.p95, o.max), (1, 4), "{o:?}");

    let mut early = spans.to_vec();
    early.extend((0..3).map(|_| span("d lib", None, 0, 1_000)));
    let o = super::Overlap::of(&early, 2_000, 12_000).expect("an overlap");
    assert!((o.avg - 1.6).abs() < 1e-9, "{o:?}");
    assert_eq!((o.p95, o.max), (3, 3), "{o:?}");

    assert_eq!(super::Overlap::of(&spans, 5_000, 5_000), None);
    assert_eq!(super::Overlap::of(&[], 0, 1_000), None);
}

/// EACH SPAN IS ONE `compile-unit-timed` EVENT naming its unit as the memory book keys it, its
/// dates, CPU and peak, and the reads' period. NEGATIVE CONTROL: a span with no wrapper read has
/// a null queue date, never 0.
#[test]
fn each_span_is_one_timed_unit_event() {
    let events = super::span_events(
        "targo test --no-run",
        &[
            span("a lib", Some(10), 20, 30),
            span("b test", None, 40, 50),
        ],
    );
    let read = |line: &str| -> aterm_json::Value {
        aterm_json::from_str(line.strip_prefix("verify-event: ").expect("prefixed")).expect("json")
    };
    let (a, b) = (read(&events[0]), read(&events[1]));
    assert_eq!(events.len(), 2);
    assert_eq!(a["event"].as_str(), Some("compile-unit-timed"));
    assert_eq!(a["call"].as_str(), Some("targo test --no-run"));
    assert_eq!(a["package"].as_str(), Some("p"));
    assert_eq!(a["unit"].as_str(), Some("a lib"));
    assert_eq!(a["queued_at_unix_ms"].as_u64(), Some(10));
    assert_eq!(a["started_at_unix_ms"].as_u64(), Some(20));
    assert_eq!(a["ended_at_unix_ms"].as_u64(), Some(30));
    assert_eq!(a["cpu_s"].as_f64(), Some(1.5));
    assert_eq!(a["peak_bytes"].as_u64(), Some(7));
    assert_eq!(a["read_every_ms"].as_u64(), Some(500));
    assert!(b["queued_at_unix_ms"].is_null(), "{b}");
    assert_eq!(b["unit"].as_str(), Some("b test"));
}
