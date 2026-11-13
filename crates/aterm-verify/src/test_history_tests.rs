// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The test binaries' history: its bounds, its file, and what it does with a file it cannot read.

use std::time::Duration;

use super::{History, ROWS_KEPT, SAMPLES_KEPT, Sample, Settled, keep, load};

fn sample(at_ms: u64) -> Sample {
    Sample {
        at_ms,
        outcome: "ok".to_owned(),
        wall_ms: 1500,
        cpu_ms: Some(2400),
        starved_ms: Some(0),
        peak_bytes: Some(1 << 30),
        width: 4,
        cores: None,
        tests: Some(133),
        partition: None,
        exe_sha256: None,
    }
}

fn first_sample_fields(value: &mut aterm_json::Value) -> &mut aterm_json::Map {
    value
        .as_object_mut()
        .unwrap()
        .get_mut("rows")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .get_mut("samples")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
}

#[test]
fn an_explicit_history_selects_its_records_and_leaves_account_history_unchanged() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let default = super::planning_path(None, Some(dir.path().to_path_buf())).unwrap();
    keep(&default, vec![("default".into(), sample(1))]).unwrap();
    let before = std::fs::read(&default).unwrap();
    let explicit = dir.path().join("experiment.json");
    let parsed = crate::cli::parse(["test", "--test-history", explicit.to_str().unwrap()]).unwrap();
    let selected =
        super::planning_path(parsed.test_history, Some(dir.path().to_path_buf())).unwrap();
    keep(
        &selected,
        vec![(
            "planted-long".into(),
            Sample {
                wall_ms: 600_000,
                ..sample(2)
            },
        )],
    )
    .unwrap();
    assert!(load(&selected).seen("planted-long").is_some());
    assert!(load(&selected).seen("default").is_none());
    assert_eq!(std::fs::read(&default).unwrap(), before);
    // Without the override, the same account still supplies its own records.
    assert!(
        load(&super::planning_path(None, Some(dir.path().to_path_buf())).unwrap())
            .seen("default")
            .is_some()
    );
    assert_eq!(super::planning_path(None, None), None);
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
    h.push(
        "-p c --lib",
        Sample {
            cores: Some(3),
            exe_sha256: Some("ab".repeat(32)),
            ..sample(3)
        },
    );
    let text = h.encode();
    assert_eq!(History::decode(&text), h);
    assert!(text.contains("\"cores\":3"), "{text}");
    assert!(text.contains(&"ab".repeat(32)), "{text}");
    // Not held at 3 cores (0.4 per thread), so its row settled there, as that executable.
    assert_eq!(
        h.row("-p c --lib").unwrap().settled,
        Some(Settled {
            cores: 3,
            exe_sha256: Some("ab".repeat(32)),
        })
    );
    // A row kept before settled grants has no `settled`: read as none, the rest whole.
    let mut fields: aterm_json::Value = aterm_json::from_str(&text).unwrap();
    for row in fields
        .as_object_mut()
        .unwrap()
        .get_mut("rows")
        .unwrap()
        .as_array_mut()
        .unwrap()
    {
        row.as_object_mut().unwrap().remove("settled");
    }
    let mut unsettled = h.clone();
    unsettled.rows[2].settled = None;
    assert_eq!(History::decode(&fields.to_string()), unsettled);
    let settled_row = |value: &mut aterm_json::Value| -> aterm_json::Map {
        let row = value.as_object_mut().unwrap().get_mut("rows").unwrap();
        let row = row.as_array_mut().unwrap()[2].as_object_mut().unwrap();
        row.get("settled").unwrap().as_object().unwrap().clone()
    };
    for (key, torn) in [
        (None, aterm_json::Value::from(3)),
        (Some("cores"), aterm_json::Value::from("3")),
        (Some("exe_sha256"), aterm_json::Value::from(3)),
    ] {
        let mut fields: aterm_json::Value = aterm_json::from_str(&text).unwrap();
        let mut settled = settled_row(&mut fields);
        let value = match key {
            None => torn,
            Some(key) => {
                settled.insert(key.into(), torn);
                aterm_json::Value::Object(settled)
            }
        };
        let rows = fields.as_object_mut().unwrap().get_mut("rows").unwrap();
        rows.as_array_mut().unwrap()[2]
            .as_object_mut()
            .unwrap()
            .insert("settled".into(), value);
        assert_eq!(
            History::decode(&fields.to_string()),
            History::default(),
            "a settled grant it cannot read is a torn row ({key:?})"
        );
    }
    // A sample kept before grants has no `cores`: read as ungranted, the rest whole.
    let mut fields: aterm_json::Value = aterm_json::from_str(&text).unwrap();
    first_sample_fields(&mut fields).remove("cores");
    assert_eq!(History::decode(&fields.to_string()), h);
    first_sample_fields(&mut fields).insert("cores".into(), aterm_json::Value::from("3"));
    assert_eq!(
        History::decode(&fields.to_string()),
        History::default(),
        "a grant it cannot read is a torn sample"
    );
    let mut fields: aterm_json::Value = aterm_json::from_str(&text).unwrap();
    first_sample_fields(&mut fields).insert("exe_sha256".into(), aterm_json::Value::from(1));
    assert_eq!(
        History::decode(&fields.to_string()),
        History::default(),
        "an executable it cannot read is a torn sample"
    );
    first_sample_fields(&mut fields).remove("exe_sha256");
    assert_eq!(
        History::decode(&fields.to_string()),
        h,
        "a sample kept before it"
    );
    let mut fields: aterm_json::Value = aterm_json::from_str(&text).unwrap();
    first_sample_fields(&mut fields).insert("partition".into(), aterm_json::Value::Bool(false));
    assert_eq!(History::decode(&fields.to_string()), History::default());
    first_sample_fields(&mut fields).remove("partition");
    assert_eq!(
        History::decode(&fields.to_string()).seen("-p a --lib"),
        h.seen("-p a --lib")
    );
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
        partition: None,
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
            cores: None,
            tests: Some(3),
            partition: None,
            exe_sha256: None,
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

/// A grant caps what a run can show it needs, so a run its grant held reads as needing twice that
/// grant, and the binary's charge grows: a pool-heavy binary held at one core, then at two, is
/// read as needing four. A run not held at a grant at least the largest that held one is what the
/// binary needs: a single-threaded binary held at one core and then not held at two settles at
/// one. NEGATIVE CONTROL: a run kept before grants (`cores: None`) is never held, and its own
/// figure stands; and an unheld run at a smaller grant than the one that held settles nothing.
#[test]
fn a_run_its_grant_held_raises_what_the_binary_needs() {
    // One thread over 10 s: `cpu_ms / 10_000` cores.
    let at = |at_ms, cpu_ms, cores| {
        Sample {
            cores,
            ..run(10_000, cpu_ms, 1, Some(1), "ok")
        }
        .clone_at(at_ms)
    };
    let need = |samples: &[Sample]| {
        let mut h = History::default();
        for s in samples {
            h.push("b", s.clone());
        }
        h.seen("b").unwrap().per_thread
    };
    let pool = [at(1, 9_900, Some(1))];
    assert!((need(&pool) - 2.0).abs() < 1e-9, "held at one: needs two");
    let pool = [at(1, 9_900, Some(1)), at(2, 19_800, Some(2))];
    assert!((need(&pool) - 4.0).abs() < 1e-9, "held at two: needs four");
    let pool = [
        at(1, 9_900, Some(1)),
        at(2, 19_800, Some(2)),
        at(3, 31_000, Some(4)),
    ];
    assert!(
        (need(&pool) - 3.1).abs() < 1e-9,
        "not held at four: needs 3.1"
    );
    let single = [at(1, 9_950, Some(1)), at(2, 9_950, Some(2))];
    assert!((need(&single) - 0.995).abs() < 1e-9, "settled at one core");
    let single = [at(1, 9_950, Some(2)), at(2, 9_950, Some(1))];
    assert!(
        (need(&single) - 0.995).abs() < 1e-9,
        "settled whichever came first"
    );
    let legacy = [at(1, 164_000, None)];
    assert!((need(&legacy) - 16.4).abs() < 1e-9, "CONTROL: ungranted");
    let legacy = [at(1, 164_000, None), at(2, 9_950, Some(1))];
    assert!(
        (need(&legacy) - 16.4).abs() < 1e-9,
        "CONTROL: an ungranted run settles a later held one"
    );
    let smaller = [at(1, 5_000, Some(1)), at(2, 19_900, Some(2))];
    assert!(
        (need(&smaller) - 4.0).abs() < 1e-9,
        "CONTROL: unheld at one does not settle a hold at two"
    );
}

/// A settled grant outlives the samples that showed it: a single-threaded binary held at one core,
/// then not held at two, needs what it used run after run, long after the run at two left the
/// kept samples, through the file too, so its grant (and with it its content key) stays one core.
/// A run held at the settled grant or above unsettles it, and the binary is read as needing twice
/// that grant again. NEGATIVE CONTROL: with no settled grant (a row kept before them) the same
/// samples re-probe at two cores; a held run below the settled grant, and an unheld one below a
/// larger held grant, change nothing.
#[test]
fn a_settled_grant_outlives_the_samples_that_showed_it() {
    let at = |at_ms, cpu_ms, cores| {
        Sample {
            cores,
            ..run(10_000, cpu_ms, 1, Some(1), "ok")
        }
        .clone_at(at_ms)
    };
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("history.json");
    keep(&path, vec![("b".into(), at(1, 9_950, Some(1)))]).unwrap();
    assert!((load(&path).seen("b").unwrap().per_thread - 2.0).abs() < 1e-9);
    keep(&path, vec![("b".into(), at(2, 9_950, Some(2)))]).unwrap();
    for n in 3..3 + 3 * SAMPLES_KEPT as u64 {
        let h = load(&path);
        let settled = h.row("b").unwrap().settled.as_ref().map(|s| s.cores);
        assert_eq!(settled, Some(2), "run {n}");
        let need = h.seen("b").unwrap().per_thread;
        assert!((need - 0.995).abs() < 1e-9, "run {n}: needs {need}");
        keep(&path, vec![("b".into(), at(n, 9_950, Some(1)))]).unwrap();
    }
    let h = load(&path);
    let row = h.row("b").unwrap();
    assert!(row.samples.iter().all(|s| s.cores == Some(1)));
    let mut unsettled = h.clone();
    unsettled.rows[0].settled = None;
    assert!(
        (unsettled.seen("b").unwrap().per_thread - 2.0).abs() < 1e-9,
        "CONTROL: the kept samples alone re-probe at two"
    );
    let mut h = load(&path);
    h.push("b", at(100, 19_800, Some(2)));
    assert_eq!(
        h.row("b").unwrap().settled,
        None,
        "held at two unsettles it"
    );
    assert!((h.seen("b").unwrap().per_thread - 4.0).abs() < 1e-9);
    let mut h = History::default();
    h.push("b", at(1, 39_000, Some(4)));
    h.push("b", at(2, 9_000, Some(2)));
    assert_eq!(
        h.row("b").unwrap().settled,
        None,
        "CONTROL: unheld at 2 under a hold at 4"
    );
}

/// A settled grant is the executable's that showed it: a binary held at one core long after it
/// settled at two is re-probed at two once a run of another executable is kept, because a
/// changed binary that now needs more looks, at one core, as it did. NEGATIVE CONTROL: a held run
/// of the same executable, and one whose executable was not read (a libtest JSON probe's run),
/// keep it settled; an unheld run of the new executable settles it again, as that executable.
#[test]
fn a_changed_executable_unsettles_its_binary() {
    let exe = |b: &str| Some(b.repeat(32));
    let at = |at_ms, cpu_ms, cores, exe_sha256| {
        Sample {
            cores,
            exe_sha256,
            ..run(10_000, cpu_ms, 1, Some(1), "ok")
        }
        .clone_at(at_ms)
    };
    let mut h = History::default();
    h.push("b", at(1, 9_950, Some(1), exe("aa")));
    h.push("b", at(2, 9_950, Some(2), exe("aa")));
    for n in 3..3 + SAMPLES_KEPT as u64 {
        h.push("b", at(n, 9_950, Some(1), exe("aa")));
    }
    let settled = |h: &History| h.row("b").unwrap().settled.clone();
    let as_aa = Some(Settled {
        cores: 2,
        exe_sha256: exe("aa"),
    });
    assert_eq!(settled(&h), as_aa);
    h.push("b", at(20, 9_950, Some(1), None));
    assert_eq!(settled(&h), as_aa, "CONTROL: an unread executable");
    assert!((h.seen("b").unwrap().per_thread - 0.995).abs() < 1e-9);
    h.push("b", at(21, 9_950, Some(1), exe("bb")));
    assert_eq!(settled(&h), None, "another executable unsettles it");
    assert!(
        (h.seen("b").unwrap().per_thread - 2.0).abs() < 1e-9,
        "the changed binary is probed at two"
    );
    h.push("b", at(22, 9_950, Some(2), exe("bb")));
    assert_eq!(
        settled(&h),
        Some(Settled {
            cores: 2,
            exe_sha256: exe("bb"),
        }),
        "CONTROL: unheld at two, it settles again as the new executable"
    );
}

impl Sample {
    fn clone_at(self, at_ms: u64) -> Sample {
        Sample { at_ms, ..self }
    }
}
