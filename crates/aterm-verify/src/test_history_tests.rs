// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The test binaries' history: its bounds, its file, and what it does with a file it cannot read.

use std::time::Duration;

use super::{History, ROWS_KEPT, SAMPLES_KEPT, Sample, keep, load};

fn sample(at_ms: u64) -> Sample {
    Sample {
        at_ms,
        outcome: "ok".to_owned(),
        wall_ms: 1500,
        cpu_ms: Some(2400),
        starved_ms: Some(0),
        peak_bytes: Some(1 << 30),
        width: 4,
        tests: Some(133),
    }
}

/// A history reads back what it wrote, an unread figure as unread (`null`), and a text it cannot
/// read whole (another schema, a torn sample, not JSON) as no history at all.
#[test]
fn a_history_reads_back_what_it_wrote_and_nothing_from_what_it_cannot_read() {
    let mut h = History::default();
    h.push("-p a --lib", sample(1));
    h.push(
        "-p b --test t --features x",
        Sample {
            outcome: "NOT RUN".to_owned(),
            cpu_ms: None,
            starved_ms: None,
            peak_bytes: None,
            tests: None,
            ..sample(2)
        },
    );
    let text = h.encode();
    assert_eq!(History::decode(&text), h);
    for unread in [
        String::new(),
        "{".to_owned(),
        text.replace("\"schema\":1", "\"schema\":2"),
        text.replace("\"width\":4", "\"width\":\"4\""),
        text.replace("\"cpu_ms\":null", "\"cpu_ms\":-1"),
    ] {
        assert_eq!(History::decode(&unread), History::default(), "{unread}");
    }
}

/// A binary keeps its last eight samples, newest last. NEGATIVE CONTROL: with no cap, the ten
/// pushed are all kept.
#[test]
fn a_binary_keeps_its_last_eight_samples() {
    let mut h = History::default();
    for at in 1..=10 {
        h.push("-p a --lib", sample(at));
    }
    let row = h.row("-p a --lib").expect("kept");
    assert_eq!(row.samples.len(), SAMPLES_KEPT);
    let kept: Vec<u64> = row.samples.iter().map(|s| s.at_ms).collect();
    assert_eq!(kept, (3..=10).collect::<Vec<_>>());
}

/// Past [`ROWS_KEPT`] rows, the row whose newest sample is oldest goes, and never the row just
/// pushed, even when its own sample is older than every other (a clock set back): it is the row
/// a run just ran. A sample for a binary already kept adds no row and evicts none. NEGATIVE
/// CONTROL: an eviction that may take the pushed row takes it, and the binary just run is lost.
#[test]
fn past_its_row_bound_the_stalest_row_goes_and_never_the_one_just_pushed() {
    let mut h = History::default();
    for i in 0..ROWS_KEPT {
        let at = 1000 + u64::try_from(i).expect("small");
        h.push(&format!("-p c{i} --lib"), sample(at));
    }
    h.push("-p c5 --lib", sample(9000));
    assert_eq!(h.rows().len(), ROWS_KEPT);
    assert!(
        h.row("-p c0 --lib").is_some(),
        "no row evicted for a kept binary"
    );
    h.push("-p new --lib", sample(1));
    assert_eq!(h.rows().len(), ROWS_KEPT);
    assert!(
        h.row("-p new --lib").is_some(),
        "the row just pushed is kept"
    );
    assert!(h.row("-p c0 --lib").is_none(), "the stalest row went");
    assert!(h.row("-p c1 --lib").is_some());
    h.push("-p new --lib", sample(2));
    assert_eq!(h.row("-p new --lib").map(|r| r.samples.len()), Some(2));
}

/// A run's samples are kept in one write to the file, created where absent, beside each run's
/// before; no sample writes nothing; and a file that decodes as nothing is replaced. NEGATIVE
/// CONTROL: the second run's keep reads the first's samples back, so each binary has both.
#[test]
fn a_runs_samples_are_kept_in_one_write_beside_the_runs_before() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join("verify/test-binaries.json");
    keep(&path, Vec::new()).expect("nothing to keep");
    assert!(!path.exists(), "no sample writes nothing");
    let run = |at: u64| {
        vec![
            ("-p a --lib".to_owned(), sample(at)),
            ("-p b --lib".to_owned(), sample(at + 1)),
        ]
    };
    keep(&path, run(10)).expect("first run");
    keep(&path, run(20)).expect("second run");
    let h = load(&path);
    assert_eq!(h.rows().len(), 2);
    let ats: Vec<u64> = h.rows()[1].samples.iter().map(|s| s.at_ms).collect();
    assert_eq!(ats, [11, 21]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).expect("kept").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private");
    }
    std::fs::write(&path, "{\"schema\":9}").expect("an older schema");
    assert_eq!(load(&path), History::default());
    keep(&path, run(30)).expect("replaces what decodes as nothing");
    assert_eq!(load(&path).rows().len(), 2);
}

/// A file that cannot be read (past its bound, not UTF-8, a directory, a symlink) is not taken for
/// an empty history: keeping into it fails and leaves it exactly as it was, and reading it for
/// planning finds nothing. NEGATIVE CONTROL: a refused read taken as empty overwrites it.
#[test]
fn a_history_that_cannot_be_read_is_left_as_it_is() {
    for kind in ["oversized", "non-text", "directory", "symlink"] {
        if kind == "symlink" && !cfg!(unix) {
            continue;
        }
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("test-binaries.json");
        let target = dir.path().join("target.json");
        let before = match kind {
            "oversized" => Some(vec![b' '; (16 << 20) + 1]),
            "non-text" => Some(vec![0xff, 0xfe]),
            "directory" => {
                std::fs::create_dir(&path).expect("directory");
                None
            }
            "symlink" => {
                #[cfg(unix)]
                {
                    std::fs::write(&target, b"retained").expect("target");
                    std::os::unix::fs::symlink(&target, &path).expect("symlink");
                }
                None
            }
            _ => unreachable!(),
        };
        if let Some(bytes) = &before {
            std::fs::write(&path, bytes).expect("existing file");
        }
        assert_eq!(load(&path), History::default(), "{kind}");
        let kept = keep(&path, vec![("-p a --lib".to_owned(), sample(1))]);
        assert!(kept.is_err(), "{kind}: {kept:?}");
        match before {
            Some(bytes) => assert_eq!(std::fs::read(&path).expect("kept"), bytes, "{kind}"),
            None if kind == "directory" => assert!(path.is_dir()),
            None => {
                assert!(
                    std::fs::symlink_metadata(&path)
                        .expect("link")
                        .file_type()
                        .is_symlink()
                );
                assert_eq!(std::fs::read(&target).expect("target"), b"retained");
            }
        }
    }
}

/// A run of `wall_ms` that used `cpu_ms` at `width`, libtest having said it runs `tests`.
fn run(wall_ms: u64, cpu_ms: u64, width: u32, tests: Option<u64>, outcome: &str) -> Sample {
    Sample {
        outcome: outcome.to_owned(),
        wall_ms,
        cpu_ms: Some(cpu_ms),
        peak_bytes: None,
        width,
        tests,
        ..sample(1)
    }
}

/// The planner reads a binary's last timed run (a FAIL that ran its tests is one; a NOT RUN, and
/// a FAIL in which libtest said it runs no test, are not), the most CPU per thread and the most
/// memory any kept run showed, and whether its last run was a FAIL. NEGATIVE CONTROL: the
/// untimed runs' walls (99 s, 0.2 s) are never read, and a NOT RUN last run is no FAIL.
#[test]
fn the_planner_reads_the_last_timed_run_and_the_most_any_run_used() {
    let mut h = History::default();
    h.push(
        "-p a --lib",
        Sample {
            peak_bytes: Some(900),
            ..run(10_000, 30_000, 1, Some(4), "ok")
        },
    );
    h.push(
        "-p a --lib",
        Sample {
            peak_bytes: Some(100),
            ..run(4000, 8000, 4, Some(8), "FAIL")
        },
    );
    h.push("-p a --lib", run(99_000, 1000, 4, Some(8), "NOT RUN"));
    let seen = h.seen("-p a --lib").expect("seen");
    assert_eq!(seen.wall, Duration::from_secs(4));
    assert_eq!(seen.width, 4);
    assert_eq!(seen.tests, 8);
    // 30 s of CPU over 10 s on one thread: three cores per thread, the most any run showed.
    assert!((seen.per_thread - 3.0).abs() < 1e-9, "{seen:?}");
    assert_eq!(seen.peak, 900);
    assert!(!seen.failed, "the last run was NOT RUN, not a FAIL");
    assert_eq!(h.seen("-p b --lib"), None);
    h.push("-p c --lib", run(200, 200, 4, None, "FAIL"));
    assert_eq!(h.seen("-p c --lib"), None, "it crashed before any test");
    h.push("-p d --lib", run(200, 200, 4, Some(1), "FAIL"));
    assert!(h.seen("-p d --lib").is_some_and(|s| s.failed));
}

/// Measured waiting survives the history read. Missing CPU has the conservative one-core
/// fallback; it is not substituted for an actual low or zero reading.
#[test]
fn measured_idle_threads_are_not_replaced_by_the_missing_cpu_fallback() {
    let mut h = History::default();
    h.push("waits", run(1000, 100, 1, Some(100), "ok"));
    assert_eq!(h.seen("waits").unwrap().per_thread, 0.1);
    h.push(
        "missing",
        Sample {
            cpu_ms: None,
            ..sample(1)
        },
    );
    assert_eq!(h.seen("missing").unwrap().per_thread, 1.0);
    h.push("zero", run(1000, 0, 1, Some(100), "ok"));
    assert_eq!(h.seen("zero").unwrap().per_thread, 0.0);
    h.push("waits", run(1000, 250, 1, Some(100), "ok"));
    assert_eq!(h.seen("waits").unwrap().per_thread, 0.25);
}

/// A decisive red keeps only its outcome after a timed pass: the planner retains the pass's
/// timing and width, but sees the newest FAIL and starts that binary before prior passes.
/// NEGATIVE CONTROL: dropping the outcome-only sample, or taking its zero wall as the latest
/// timing, makes one of these assertions fail.
#[test]
fn a_stopped_red_keeps_the_prior_timing_and_marks_the_binary_failed() {
    let mut h = History::default();
    h.push("-p a --lib", run(4000, 8000, 4, Some(8), "ok"));
    h.push(
        "-p a --lib",
        Sample {
            at_ms: 2,
            outcome: "FAIL".to_owned(),
            wall_ms: 0,
            cpu_ms: None,
            starved_ms: None,
            peak_bytes: None,
            width: 2,
            tests: Some(3),
        },
    );

    let seen = h.seen("-p a --lib").expect("the timed pass remains");
    assert_eq!(seen.wall, Duration::from_secs(4));
    assert_eq!((seen.width, seen.tests), (4, 8));
    assert!(seen.failed, "the latest decisive outcome is failed");
}

/// The heaviest run is read over the selection's crates' binaries, every binary's for the whole
/// workspace. NEGATIVE CONTROL: a crate the selection does not name, and one whose name only
/// begins with a selected one's, are not read.
#[test]
fn the_heaviest_run_is_read_for_the_selections_crates() {
    let mut h = History::default();
    for (binary, peak) in [
        ("-p a --lib", 100),
        ("-p a --test t", 300),
        ("-p ab --lib", 900),
        ("-p b --lib", 500),
    ] {
        h.push(
            binary,
            Sample {
                peak_bytes: Some(peak),
                ..sample(1)
            },
        );
    }
    assert_eq!(h.heaviest(&["a".to_owned()]), 300);
    assert_eq!(h.heaviest(&["a".to_owned(), "b".to_owned()]), 500);
    assert_eq!(h.heaviest(&[]), 900);
    assert_eq!(h.heaviest(&["c".to_owned()]), 0);
}
