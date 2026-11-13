// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_update::recovery_ledger::{Row, append_row};

struct Fake {
    alive: Option<(u32, u64)>,
    trial: u32,
}

impl Probe for Fake {
    fn birth(&self, pid: u32) -> Option<Birth> {
        self.alive
            .filter(|(p, _)| *p == pid)
            .map(|(_, seconds)| Birth {
                seconds,
                micros: None,
            })
    }
    fn trial(&self, _build: u64) -> u32 {
        self.trial
    }
}

const NOBODY: Fake = Fake {
    alive: None,
    trial: 0,
};

struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    fn new(tag: &str) -> Self {
        Self(
            aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-recovery-census-{tag}-"))
                .tempdir()
                .expect("scratch dir"),
        )
    }
    fn file(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.path().join(name);
        std::fs::write(&path, body).expect("write fixture");
        path
    }
    fn log(&self, lines: &[(u64, u32, &str)]) {
        let text: String = lines
            .iter()
            .map(|(secs, pid, body)| format!("{secs}.123 {pid} INFO aterm_gui: {body}\n"))
            .collect();
        self.file("aterm.log", &text);
    }
}

const BORN: u64 = 1_790_000_000;

fn me(pid: u32) -> Me {
    Me {
        pid,
        started: BORN + 5000,
        started_us: None,
        build: 1_790_400_000,
        version: "0.94.0".into(),
    }
}

fn with_previous(s: &Scratch) {
    let previous = Row {
        at: BORN,
        pid: 4242,
        started: BORN,
        started_us: None,
        build: 1_790_305_290,
        version: "0.93.0".into(),
        launch: LaunchKind::Cold,
        prev: PrevRun::none(),
        keeper: None,
        restores: None,
    };
    append_row(&ledger_path(s.0.path()), &previous).expect("seed the ledger");
}

fn marker(pid: u32, birth: u64) -> String {
    format!(
        "crash-marker-{pid}-{}-app.log",
        u128::from(birth) * 1_000_000_000 + 7
    )
}

#[test]
fn the_census_thread_amends_the_node_logs_start() {
    let s = Scratch::new("node-log");
    let clock = crate::session_log::Clock::reading(|| aterm_slog::Stamp::new(1_760_000_000_000, 5));
    let node: &'static crate::session_log_node::NodeLogHost = Box::leak(Box::new(
        crate::session_log_node::NodeLogHost::started(7, clock),
    ));
    let dir = s.0.path().to_path_buf();
    record_into(
        Some(node),
        move || Some(dir),
        LaunchKind::Cold,
        None,
        KeeperAtLaunch::On,
    );
    let amended = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the census's prev_end in the node log",
        || {
            node.lines()
                .into_iter()
                .find(|l| l.contains(" amend n start "))
        },
    );
    assert!(
        amended.starts_with("2 amend n start prev_end=none by=aterm src=aterm conf=exact "),
        "{amended}"
    );
    let rows = read_rows(&ledger_path(s.0.path()));
    assert_eq!(rows.len(), 1, "the row lands before the amend that says it");
    assert_eq!(rows[0].keeper, Some(KeeperAtLaunch::On));
}

fn swept_then_counted(
    owner: crate::crash_signal::MarkerOwner,
    scan: crate::crash_signal::MarkerOwner,
) -> (bool, Row) {
    use crate::crash_signal::{MarkerOwner, markers};
    let s = Scratch::new("lab-desk");
    with_previous(&s);
    s.log(&[
        (BORN + 1, 4242, "watchdog armed"),
        (BORN + 70, 4242, "idle"),
    ]);
    let marker = s.file(
        &markers::file_name(4242, u128::from(BORN) * 1_000_000_000 + 7, owner),
        "",
    );
    let swept = markers::sweep(s.0.path(), 5000, scan);
    let kept = marker.exists();
    assert_eq!(swept.is_empty(), kept, "the sweep answers what it removed");
    assert!(
        markers::dead_app_markers(s.0.path(), 5000).is_empty(),
        "no windowed launch's kill scan ever reports it"
    );
    if scan == MarkerOwner::Lab {
        let consumed =
            crate::logging::consume_dead_markers(s.0.path(), 5000, None, MarkerOwner::Lab);
        assert_eq!(consumed.is_some(), kept, "the lab desk's kill scan");
    }
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    (kept, row)
}

#[test]
fn a_lab_desks_census_reads_its_killed_run_and_no_other_start_fools_it() {
    use crate::crash_signal::MarkerOwner::{App, Lab, Other};
    for (owner, scan, kept, class) in [
        (Lab, Lab, true, EndClass::Killed),
        (Other, Lab, false, EndClass::Clean),
        (Lab, App, false, EndClass::Clean),
        (Lab, Other, false, EndClass::Clean),
    ] {
        let (left, row) = swept_then_counted(owner, scan);
        assert_eq!(left, kept, "{owner:?} under a {scan:?} sweep");
        assert_eq!(
            row.prev.class, class,
            "{owner:?} under a {scan:?} sweep: {row:?}"
        );
        assert_eq!(row.prev.pid, Some(4242));
    }
}

#[test]
fn the_census_gate_is_on_for_a_lab_desk_and_off_for_a_plain_headless_one() {
    use crate::crash_signal::{Arming, MarkerOwner, markers};
    let lab = Arming::for_launch(false, true, false, true);
    let plain = Arming::for_launch(false, true, false, false);
    assert_eq!(cold_census(lab, None), ColdCensus::Own, "a lab desk");
    assert_eq!(cold_census(plain, None), ColdCensus::Off, "plain headless");
    assert_eq!((lab, plain), (Arming::Lab, Arming::Other));
    assert_eq!(cold_census(Arming::App, None), ColdCensus::Own);
    assert_eq!(cold_census(Arming::AppAfterCommit, None), ColdCensus::Off);
    assert_eq!(
        cold_census(plain, Some(std::ffi::OsStr::new("/private/tmp/x"))),
        ColdCensus::Seam(PathBuf::from("/private/tmp/x"))
    );
    assert_eq!(
        cold_census(plain, Some(std::ffi::OsStr::new(""))),
        ColdCensus::Off
    );

    let s = Scratch::new("gate");
    with_previous(&s);
    s.log(&[(BORN + 1, 4242, "watchdog armed")]);
    s.file(
        &markers::file_name(4242, u128::from(BORN) * 1_000_000_000 + 7, MarkerOwner::Lab),
        "",
    );
    let started = |ep| -> &'static crate::session_log_node::NodeLogHost {
        let clock =
            crate::session_log::Clock::reading(|| aterm_slog::Stamp::new(1_760_000_000_000, 5));
        Box::leak(Box::new(crate::session_log_node::NodeLogHost::started(
            ep, clock,
        )))
    };
    let plain_node = started(8);
    let dir = s.0.path().to_path_buf();
    record_cold_into(
        plain_node,
        move || Some(dir),
        &cold_census(plain, None),
        None,
        KeeperAtLaunch::Off,
    );
    let lines = plain_node.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("1 open n start prev_end=- "),
        "{lines:?}"
    );
    assert_eq!(read_rows(&ledger_path(s.0.path())).len(), 1);

    assert!(
        crate::logging::consume_dead_markers(s.0.path(), 5000, None, MarkerOwner::Lab).is_some()
    );
    let lab_node = started(9);
    let dir = s.0.path().to_path_buf();
    record_cold_into(
        lab_node,
        move || Some(dir),
        &cold_census(lab, None),
        None,
        KeeperAtLaunch::Off,
    );
    let amended = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the lab census's prev_end in the node log",
        || {
            lab_node
                .lines()
                .into_iter()
                .find(|l| l.contains(" amend n start "))
        },
    );
    assert!(
        amended.starts_with("2 amend n start prev_end=killed by=aterm "),
        "{amended}"
    );
    let rows = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the lab census's row",
        || Some(read_rows(&ledger_path(s.0.path()))).filter(|rows| rows.len() == 2),
    );
    assert_eq!(rows[1].prev.class, EndClass::Killed);
}

#[test]
fn a_clean_quit_is_dated_by_its_final_exit_line() {
    let s = Scratch::new("clean");
    with_previous(&s);
    s.log(&[
        (BORN + 1, 4242, "watchdog armed"),
        (BORN + 3600, 4242, QUIT_LINE),
        (BORN + 3601, 9999, "another process"),
    ]);
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::Clean);
    assert_eq!(row.prev.pid, Some(4242));
    assert_eq!(row.prev.build, Some(1_790_305_290));
    assert_eq!(
        row.prev.uptime,
        Some(Uptime {
            secs: 3600,
            at_least: false
        })
    );
    assert_eq!(row.prev.stall, Tri::No);
    assert_eq!(row.prev.pressure, Pressure::None);
}

#[test]
fn a_run_with_its_own_lines_and_no_marker_ended_clean_bounded_by_its_last_line() {
    let s = Scratch::new("clean-bound");
    with_previous(&s);
    s.log(&[
        (BORN + 1, 4242, "watchdog armed"),
        (BORN + 70, 4242, "idle"),
    ]);
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::Clean);
    assert_eq!(
        row.prev.uptime,
        Some(Uptime {
            secs: 70,
            at_least: true
        })
    );
}

#[test]
fn an_empty_consumed_marker_is_a_kill_bounded_by_the_last_line() {
    let s = Scratch::new("killed");
    with_previous(&s);
    s.file(&format!("{}.seen", marker(4242, BORN)), "");
    s.log(&[
        (BORN + 10, 4242, "up"),
        (BORN + 90, 4242, &format!("{PRESSURE_LINE} critical")),
        (
            BORN + 100,
            4242,
            "MAIN-THREAD STALL: no heartbeat for 5.1s while inside `UserEvent`",
        ),
    ]);
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::Killed);
    assert_eq!(
        row.prev.uptime,
        Some(Uptime {
            secs: 100,
            at_least: true
        })
    );
    assert_eq!(row.prev.stall, Tri::Yes);
    assert_eq!(row.prev.pressure, Pressure::Critical);
}

#[test]
fn a_stall_that_ended_and_pressure_that_eased_are_not_at_the_end() {
    let s = Scratch::new("ended");
    with_previous(&s);
    s.file(&format!("{}.seen", marker(4242, BORN)), "");
    s.log(&[
        (BORN + 10, 4242, "MAIN-THREAD STALL: no heartbeat for 5.1s"),
        (
            BORN + 11,
            4242,
            &format!("{STALL_ENDED_LINE}: the main thread beat again"),
        ),
        (BORN + 12, 4242, &format!("{PRESSURE_LINE} critical")),
        (BORN + 13, 4242, &format!("{PRESSURE_LINE} normal")),
    ]);
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::Killed);
    assert_eq!(row.prev.stall, Tri::No);
    assert_eq!(row.prev.pressure, Pressure::Normal);
}

#[test]
fn a_written_marker_is_the_signal_it_names_at_its_mtime() {
    let s = Scratch::new("signal");
    with_previous(&s);
    s.file(
        &format!("{}.seen", marker(4242, BORN)),
        "aterm: fatal signal 11\n",
    );
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::Signal(11));
    assert_eq!(row.prev.uptime.map(|u| u.at_least), Some(false));
    assert_eq!(row.prev.stall, Tri::Unknown, "no log lines: not known");
}

#[test]
fn a_marker_in_the_older_banner_form_still_names_its_signal() {
    let s = Scratch::new("signal-old");
    with_previous(&s);
    s.file(
        &format!("{}.seen", marker(4242, BORN)),
        "aterm: fatal signal 6 \u{2014} crash marker written\n",
    );
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::Signal(6));
}

#[test]
fn a_panic_report_wins_and_is_dated_by_its_first_line() {
    let s = Scratch::new("panic");
    with_previous(&s);
    s.file(
        "crash-4242.log.seen",
        &format!(
            "aterm-gui 0.93.0 crashed at unix {}.250\npanicked at x\n",
            BORN + 42
        ),
    );
    s.file(
        &format!("{}.seen", marker(4242, BORN)),
        "aterm: fatal signal 6\n",
    );
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::Panic);
    assert_eq!(
        row.prev.uptime,
        Some(Uptime {
            secs: 42,
            at_least: false
        })
    );
}

#[test]
fn another_runs_artifacts_and_lines_under_a_recycled_pid_are_not_this_runs() {
    let s = Scratch::new("recycled");
    with_previous(&s);
    s.file(&format!("{}.seen", marker(4242, BORN - 86_400)), "");
    s.log(&[(BORN - 86_000, 4242, "MAIN-THREAD STALL: an old one")]);
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(
        row.prev.class,
        EndClass::Unknown,
        "none of that is this run's, so there is no evidence of it at all"
    );
    assert_eq!(
        row.prev.stall,
        Tri::Unknown,
        "none of those lines are this run's"
    );
}

#[test]
fn a_previous_run_still_alive_is_running_and_its_dead_pid_twin_is_not() {
    let s = Scratch::new("running");
    with_previous(&s);
    let alive = Fake {
        alive: Some((4242, BORN)),
        trial: 0,
    };
    let row = census_row(s.0.path(), &me(5000), LaunchKind::Cold, BORN + 5000, &alive);
    assert_eq!(row.prev.class, EndClass::Running);
    let reused = Fake {
        alive: Some((4242, BORN + 4000)),
        trial: 0,
    };
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &reused,
    );
    assert_ne!(
        row.prev.class,
        EndClass::Running,
        "a recycled pid is not the run"
    );
}

#[test]
fn a_pid_reused_within_the_same_second_is_not_the_previous_run() {
    struct Exact(u32, Birth);
    impl Probe for Exact {
        fn birth(&self, pid: u32) -> Option<Birth> {
            (pid == self.0).then_some(self.1)
        }
        fn trial(&self, _build: u64) -> u32 {
            0
        }
    }
    let s = Scratch::new("reused-second");
    let first = Me {
        pid: 4242,
        started: BORN,
        started_us: Some(250_000),
        ..me(4242)
    };
    let row = census_row(s.0.path(), &first, LaunchKind::Cold, BORN + 1, &NOBODY);
    assert_eq!(row.started_us, Some(250_000), "the row carries the birth");
    append_row(&ledger_path(s.0.path()), &row).expect("append");
    let reused = Exact(
        4242,
        Birth {
            seconds: BORN,
            micros: Some(750_000),
        },
    );
    let next = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &reused,
    );
    assert_ne!(
        next.prev.class,
        EndClass::Running,
        "a pid reused within the second is not the run"
    );
    let same = Exact(
        4242,
        Birth {
            seconds: BORN,
            micros: Some(250_000),
        },
    );
    let next = census_row(s.0.path(), &me(5000), LaunchKind::Cold, BORN + 5000, &same);
    assert_eq!(next.prev.class, EndClass::Running);
}

#[test]
fn a_successor_records_the_handoff_and_the_boot_trial() {
    let s = Scratch::new("successor");
    with_previous(&s);
    let trial = Fake {
        alive: Some((4242, BORN)),
        trial: 1,
    };
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Successor,
        BORN + 700,
        &trial,
    );
    assert_eq!(row.launch, LaunchKind::Successor);
    assert_eq!(row.prev.class, EndClass::Handoff);
    assert_eq!(row.prev.pid, Some(4242));
    assert_eq!(row.prev.trial, 1);
    assert_eq!(row.prev.uptime.map(|u| u.secs), Some(700));
}

#[test]
fn the_first_row_never_takes_a_marker_owned_by_another_kind_of_launch() {
    let s = Scratch::new("first-other");
    s.file(
        &format!(
            "crash-marker-888-{}-other.log.seen",
            u128::from(BORN) * 1_000_000_000 + 7
        ),
        "aterm: fatal signal 11\n",
    );
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(
        row.prev.class,
        EndClass::None,
        "a dev or test launch's crash is not the run before"
    );
    assert_eq!(row.prev.pid, None);
    s.file(&format!("{}.seen", marker(777, BORN)), "");
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.pid, Some(777));
    assert_eq!(row.prev.class, EndClass::Killed);
}

#[test]
fn the_first_row_classifies_the_newest_artifact_or_says_none() {
    let s = Scratch::new("first");
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::None);
    s.file(&format!("{}.seen", marker(777, BORN)), "");
    let row = census_row(
        s.0.path(),
        &me(5000),
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(row.prev.class, EndClass::Killed);
    assert_eq!(row.prev.pid, Some(777));
    assert_eq!(row.prev.build, None);
}

#[test]
fn a_cold_lane_update_exec_is_a_handoff_from_the_image_it_replaced() {
    let s = Scratch::new("self");
    with_previous(&s);
    let old_image = Me {
        build: 1_790_300_000,
        version: "0.93.0".into(),
        ..me(5000)
    };
    let first = census_row(
        s.0.path(),
        &old_image,
        LaunchKind::Cold,
        BORN + 5000,
        &NOBODY,
    );
    assert_eq!(first.prev.pid, Some(4242));
    append_row(&ledger_path(s.0.path()), &first).expect("append");
    s.log(&[
        (BORN + 5001, 5000, "watchdog armed"),
        (BORN + 9000, 5000, "update apply: cold lane"),
    ]);
    let alive = Fake {
        alive: Some((5000, BORN + 5000)),
        trial: 0,
    };
    let again = census_row(s.0.path(), &me(5000), LaunchKind::Cold, BORN + 9002, &alive);
    assert_eq!(again.prev.pid, Some(5000), "the image it replaced");
    assert_eq!(again.prev.class, EndClass::Handoff);
    assert_eq!(again.prev.build, Some(1_790_300_000));
    assert_eq!(again.prev.version.as_deref(), Some("0.93.0"));
    assert_eq!(again.prev.uptime.map(|u| u.secs), Some(4002));
    assert_eq!(again.prev.stall, Tri::No);
    append_row(&ledger_path(s.0.path()), &again).expect("append");
    let rows = read_rows(&ledger_path(s.0.path()));
    assert_eq!(
        rows.iter().filter(|r| r.prev.pid == Some(4242)).count(),
        1,
        "the run before the process is classified once"
    );
}

#[test]
fn the_names_the_scans_leave_are_parsed_both_ways() {
    assert_eq!(panic_report_pid("crash-12.log"), Some(12));
    assert_eq!(panic_report_pid("crash-12.log.seen"), Some(12));
    assert_eq!(panic_report_pid("crash-marker-12-3-app.log"), None);
    assert_eq!(
        marker_pid_birth("crash-marker-12-1790000000123456789-app.log.seen"),
        Some((12, 1_790_000_000))
    );
    assert_eq!(
        marker_pid_birth("crash-signal-12-1790000000123456789.log"),
        Some((12, 1_790_000_000))
    );
    assert_eq!(marker_pid_birth("recovery-ledger.log"), None);
}
