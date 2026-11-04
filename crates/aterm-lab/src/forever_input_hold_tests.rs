// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the capture of a starting program that leaves its input unread
//! ([`crate::forever::input_hold`]): when a look takes one, once per tab, where it goes, the tree
//! it writes, how its children run and are judged, and what the start's and the run's lines say.
//! Nothing here runs `sample` or `lsof`.

use std::path::{Path, PathBuf};

use super::*;
use crate::forever::{ComposerWait, Started, start_failed, started_line};

fn input(word: &str, bytes: &str, wait_ms: &str) -> Input {
    Input {
        word: word.into(),
        bytes: bytes.into(),
        wait_ms: wait_ms.into(),
    }
}

fn holds(threshold_ms: u64) -> Holds {
    Holds::new(PathBuf::from("/lab/runs/t").join(DIR), threshold_ms)
}

/// A LOOK CAPTURES A START ONCE ITS INPUT HAS STOOD UNREAD A SECOND, AND A TAB ONCE: a start that
/// holds nothing (its input read at the look, or unread the few ms Andrews-MBP-5's 109 starts
/// showed) is no capture, nor is one whose host does not measure its input; 50 B unread a second,
/// or typed ahead or behind a stopped program as long, is. The tab that has its capture takes no
/// second one, at any look of either of its waits, while another tab still takes its own; the tab
/// whose leader `who` did not name takes none either, and its line says why.
#[test]
fn a_start_is_captured_once_its_input_stands_unread_a_second_and_once_per_tab() {
    let mut h = holds(HOLD_CAPTURE_MS);
    for (i, ready) in [
        (input("clear", "0", "0"), false),
        (input("clear", "0", "0"), true),
        (input("pending", "50", "21"), false),
        (input("pending", "3", "27"), false),
        (input("pending", "50", "999"), false),
        (input("pending", "50", "-"), false),
        (input("-", "-", "-"), false),
        (input("-", "-", "5000"), false),
    ] {
        assert!(!h.due("s-1", &i, ready), "{i:?} ready={ready}");
    }
    for i in [
        input("pending", "50", "1000"),
        input("pending", "50", "7500"),
        input("typeahead", "565", "1200"),
        input("stopped", "6", "1500"),
        input("stalled", "16", "60159"),
    ] {
        assert!(h.due("s-1", &i, false), "{i:?}");
    }
    let held = input("pending", "50", "1012");
    let taken = h
        .take_of("s-1", "land-n", &held, "OK schema=1 input=pending", None)
        .finish();
    assert_eq!(
        taken.said(),
        "not captured at input=pending input_bytes=50 input_wait_ms=1012: `who` named no \
         foreground group for the tab"
    );
    for (i, ready) in [
        (held.clone(), false),
        (input("pending", "50", "9000"), true),
    ] {
        assert!(!h.due("s-1", &i, ready), "taken once: {i:?}");
    }
    assert!(h.due("s-2", &held, false), "another tab takes its own");
    // The threshold the run may set: the default where unset, a number, refused otherwise.
    assert_eq!(threshold_of(None), Ok(HOLD_CAPTURE_MS));
    assert_eq!(threshold_of(Some(" 250 ")), Ok(250));
    for bad in ["", "1s", "-5", "1.5"] {
        let why = threshold_of(Some(bad)).expect_err(bad);
        assert!(why.contains(THRESHOLD_ENV), "{why}");
    }
    assert_eq!(
        holds(HOLD_CAPTURE_MS).said(true),
        None,
        "the default says nothing"
    );
    let said = holds(250).said(true).expect("a moved threshold is said");
    assert!(
        said.contains("250 ms") && said.contains(THRESHOLD_ENV),
        "{said}"
    );
}

/// A THRESHOLD OF 0 ALSO CAPTURES A START THAT HOLDS NOTHING, AT THE LOOK THAT FINDS IT READY: a
/// normal start's capture to set beside a hold's. A look before it, its input read, takes none;
/// any unread look takes one at once.
#[test]
fn a_threshold_of_naught_captures_a_start_that_holds_nothing_once_it_is_ready() {
    let h = holds(0);
    assert!(!h.due("s-1", &input("clear", "0", "0"), false));
    assert!(h.due("s-1", &input("clear", "0", "0"), true));
    assert!(h.due("s-1", &input("pending", "50", "0"), false));
    assert!(
        !holds(HOLD_CAPTURE_MS).due("s-1", &input("clear", "0", "0"), true),
        "control: at the default, a ready start that held nothing is no capture"
    );
    let said = h.said(true).expect("0 is not the default");
    assert!(said.contains("ready"), "{said}");
}

fn rec(pid: u32, ppid: u32, comm: &str, start_s: u64, start_us: u32) -> Record {
    Record {
        pid,
        ppid,
        pgid: 100,
        tpgid: 100,
        tty: Some(5),
        uid: 501,
        comm: comm.into(),
        start_s,
        start_us,
        zombie: false,
    }
}

/// THE TREE NAMES THE PROGRAM AND EVERYTHING IT STARTED, EACH WITH ITS AGE: the leader first, then
/// its descendants by start, each `pid ppid pgid age_ms comm args` from one read of the table; the
/// tab's shell above it and a process outside it are left out, a zombie is `<defunct>`, and a
/// leader gone by the read is said.
#[test]
fn the_tree_names_the_program_and_what_it_started_each_with_its_age() {
    let now_us = 1_000_010_000_000;
    let mut dead = rec(103, 100, "sh", 1_000_009, 900_000);
    dead.zombie = true;
    let records = [
        rec(99, 1, "zsh", 999_000, 0),
        rec(102, 101, "ps", 1_000_009, 500_000),
        rec(100, 99, "claude", 1_000_000, 0),
        rec(101, 100, "sh", 1_000_009, 0),
        dead,
        rec(200, 1, "launchd-child", 1_000_008, 0),
    ];
    let args = |pid: u32| format!("args-of-{pid}");
    let text = tree_text("# head\n", &records, 100, now_us, args);
    assert_eq!(
        text,
        "# head\n\
         pid ppid pgid age_ms comm args\n\
         100 99 100 10000 claude args-of-100\n\
         101 100 100 1000 sh args-of-101\n\
         102 101 100 500 ps args-of-102\n\
         103 100 100 100 <defunct> args-of-103\n"
    );
    let gone = tree_text("# head\n", &records, 4242, now_us, args);
    assert_eq!(
        gone,
        "# head\npid ppid pgid age_ms comm args\n4242 - - - (gone) -\n"
    );
}

fn started(capture: Option<Capture>, held: Option<Input>) -> Started {
    let last = held.clone().unwrap_or_else(|| input("clear", "0", "0"));
    Started {
        wait: ComposerWait::Ready,
        ms: 300,
        held,
        last,
        capture,
    }
}

fn part(what: &'static str, file: &str) -> Part {
    Part {
        what,
        got: Ok(Path::new("/lab/runs/t/input-holds").join(file)),
    }
}

/// THE START'S LINE NAMES THE CAPTURE'S FILES, OR WHY A PART COULD NOT RUN, AND IS UNCHANGED WHERE
/// NONE WAS TAKEN: `started   land-n @…: 0.3 s, its input read at every look` as every run before
/// it printed, and a held start's line followed by the folder and each file, or the part that
/// could not run (sample refused) said in its place. A start that failed names its capture too.
#[test]
fn the_start_s_line_names_the_capture_s_files_and_is_unchanged_without_one() {
    assert_eq!(
        started_line(&started(None, None)),
        "0.3 s, its input read at every look"
    );
    let held = input("pending", "50", "1012");
    assert_eq!(
        started_line(&started(None, Some(held.clone()))),
        "0.3 s, its input unread meanwhile (last: input=pending input_bytes=50 input_wait_ms=1012)"
    );
    let capture = Capture {
        at: held.clone(),
        pid: Some(56_689),
        program: "claude".into(),
        dir: PathBuf::from("/lab/runs/t/input-holds"),
        parts: vec![
            part("tree", "land-n-tree.txt"),
            part("lsof", "land-n-lsof.txt"),
            part("sample", "land-n-sample.txt"),
        ],
        why: None,
    };
    assert_eq!(
        started_line(&started(Some(capture.clone()), Some(held.clone()))),
        "0.3 s, its input unread meanwhile (last: input=pending input_bytes=50 \
         input_wait_ms=1012); captured at input=pending input_bytes=50 input_wait_ms=1012, pid \
         56689 (claude), into /lab/runs/t/input-holds: tree land-n-tree.txt, lsof \
         land-n-lsof.txt, sample land-n-sample.txt"
    );
    let mut refused = capture.clone();
    refused.program = String::new();
    refused.parts[2] = Part {
        what: "sample",
        got: Err("sample exit status: 1: sample cannot examine process 56689".into()),
    };
    let said = refused.said();
    assert!(
        said.contains("pid 56689, into")
            && said.ends_with(
                "lsof land-n-lsof.txt, sample not taken: sample exit status: 1: sample cannot \
                 examine process 56689"
            ),
        "{said}"
    );
    let hung = Started {
        wait: ComposerWait::Hung,
        ms: 60_000,
        last: held.clone(),
        ..started(Some(capture), Some(held))
    };
    let why = start_failed(&hung, "claude").expect("a hang");
    assert!(
        why.contains("its input unread")
            && why.contains("; captured at")
            && why.contains("sample land-n-sample.txt"),
        "{why}"
    );
    assert_eq!(start_failed(&started(None, None), "claude"), None);
}

/// THE CAPTURES GO WHERE THE JAILED PROGRAM CANNOT WRITE: `sample` writes its file by path,
/// following links, from outside the jail, and the jail lets its program write anywhere in the
/// scratch, so a link planted at a capture's predictable name, or in its folder's place, would
/// have had `sample` overwrite a file of the owner's. The captures go beside the scratch, in the
/// run's directory, which `jail.sb` denies the program (`deny file-write* (subpath RUN)`, then
/// `allow` only `RUN/scratch`).
#[test]
fn the_captures_go_where_the_jailed_program_cannot_write() {
    let run = Run {
        run: PathBuf::from("/lab/runs/t"),
        w: PathBuf::from("/lab/runs/t/scratch"),
        ..Run::default()
    };
    let dir = dir_of(&run);
    assert_eq!(dir, run.run.join(DIR));
    assert!(!dir.starts_with(&run.w), "{}", dir.display());
}

/// A RUN WITH NO READY GATE SAYS IT CAPTURES NOTHING: under `--no-ready-gate` no look watches a
/// start, so a hold there (`rv1-l2`'s, 12 to 102 s, the largest) would leave neither a file nor a
/// word. Whatever its threshold, that run says no start is captured; a gated run's line is as
/// before (none at the default).
#[test]
fn a_run_with_no_ready_gate_says_it_captures_nothing() {
    for ms in [HOLD_CAPTURE_MS, 0, 250] {
        let said = holds(ms).said(false).expect("an ungated run says it");
        assert!(
            said.starts_with("no start is captured") && said.contains("--no-ready-gate"),
            "{said}"
        );
    }
    assert_eq!(holds(HOLD_CAPTURE_MS).said(true), None);
}

fn scratch(prefix: &str) -> (aterm_tempfile::TempDir, PathBuf) {
    let top = aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap();
    let dir = std::fs::canonicalize(top.path()).unwrap();
    (top, dir)
}

/// A PROCESS TABLE THAT DID NOT READ IS SAID, NEVER WRITTEN AS A PROGRAM GONE: a capture's evidence
/// that called a stalled program exited would point its reader away from the stall. A table that
/// did not read writes no tree and says so in the part; one that read and lacks the program writes
/// it `(gone)`; one that holds it names it.
#[test]
fn a_process_table_that_did_not_read_is_said_not_written_as_a_program_gone() {
    let (_top, dir) = scratch("lab-hold-tree-");
    let args = |pid: u32| format!("args-of-{pid}");
    let tree = dir.join("t-tree.txt");
    let (program, part) = tree_part(&dir, "t", "# head\n", None, (100, 0), args);
    assert_eq!(program, "");
    assert_eq!(
        part,
        Part {
            what: "tree",
            got: Err("the kernel's process table did not read".into()),
        }
    );
    assert!(std::fs::symlink_metadata(&tree).is_err(), "no tree file");
    let (program, part) = tree_part(
        &dir,
        "t",
        "# head\n",
        Some(vec![rec(99, 1, "zsh", 0, 0)]),
        (100, 0),
        args,
    );
    assert_eq!((program.as_str(), part.got), ("", Ok(tree.clone())));
    assert!(
        std::fs::read_to_string(&tree)
            .unwrap()
            .ends_with("100 - - - (gone) -\n")
    );
    let (program, _) = tree_part(
        &dir,
        "t",
        "# head\n",
        Some(vec![rec(100, 99, "claude", 0, 0)]),
        (100, 0),
        args,
    );
    assert_eq!(program, "claude");
}

fn exited(code: i32, stdout: &str, stderr: &str) -> Output {
    use std::os::unix::process::ExitStatusExt as _;
    Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

fn words(c: &Command) -> Vec<String> {
    std::iter::once(c.get_program())
        .chain(c.get_args())
        .map(|w| w.to_string_lossy().into_owned())
        .collect()
}

fn got(parts: &[Part]) -> Vec<(&'static str, Result<PathBuf, String>)> {
    parts.iter().map(|p| (p.what, p.got.clone())).collect()
}

/// `sample` AND `lsof` RUN AT ONCE, `sample` FIRST, UNDER ONE HANG DETECTOR: run one after the
/// other, each with a minute of its own, a stuck `lsof` held the gate two minutes and started the
/// sample after a hold as short as m8's 1.9 s had ended. The runner is asked once, `sample` first,
/// with the one deadline; a child that timed out or named nothing is said; `lsof` is not run where
/// the program's sockets may not be read now; and the file `sample` reports is taken only as a
/// regular file read through no link. Nothing here spawns a child: the runner is the test's.
#[test]
fn sample_and_lsof_run_at_once_sample_first_under_one_hang_detector() {
    let (_top, dir) = scratch("lab-hold-children-");
    let at = |n: &str| {
        (
            dir.join(format!("{n}-lsof.txt")),
            dir.join(format!("{n}-sample.txt")),
        )
    };
    let by = Instant::now() + CHILD_HANG;
    let (lsof_to, sample_to) = at("a");
    let parts = children(4242, (&lsof_to, &sample_to), true, by, |cmds, deadline| {
        assert_eq!(deadline, by, "one deadline for both");
        let seen: Vec<Vec<String>> = cmds.iter().map(|c| words(c)).collect();
        let sample_path = sample_to.to_string_lossy().into_owned();
        assert_eq!(
            seen,
            [
                vec![
                    "/usr/bin/sample",
                    "4242",
                    "2",
                    "-mayDie",
                    "-file",
                    &sample_path
                ],
                vec!["/usr/sbin/lsof", "-nP", "-p", "4242"],
            ]
        );
        std::fs::write(&sample_to, "Call graph:\n").unwrap();
        vec![
            Ok(exited(0, "", "")),
            Ok(exited(0, "COMMAND PID\nclaude 4242\n", "")),
        ]
    });
    assert_eq!(
        got(&parts),
        [("lsof", Ok(lsof_to.clone())), ("sample", Ok(sample_to))]
    );
    assert_eq!(
        std::fs::read_to_string(&lsof_to).unwrap(),
        "COMMAND PID\nclaude 4242\n"
    );
    let (lsof_to, sample_to) = at("b");
    let parts = children(4242, (&lsof_to, &sample_to), true, by, |_, _| {
        vec![
            Err(RunError::TimedOut),
            Ok(exited(1, "", "lsof: no such process\n")),
        ]
    });
    assert_eq!(
        got(&parts),
        [
            (
                "lsof",
                Err("lsof exit status: 1 named nothing: lsof: no such process".into())
            ),
            ("sample", Err("sample did not finish in time".into())),
        ]
    );
    let (lsof_to, sample_to) = at("c");
    let parts = children(4242, (&lsof_to, &sample_to), false, by, |cmds, _| {
        assert_eq!(cmds.len(), 1, "no lsof where the sockets may not be read");
        assert_eq!(cmds[0].get_program(), "/usr/bin/sample");
        vec![Ok(exited(0, "", ""))]
    });
    assert_eq!(
        got(&parts),
        [
            (
                "lsof",
                Err("the program was exiting, or a fork not yet past its exec".into())
            ),
            ("sample", Err("sample wrote no file".into())),
        ]
    );
    let (lsof_to, sample_to) = at("d");
    let elsewhere = dir.join("elsewhere.txt");
    std::fs::write(&elsewhere, "the owner's\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, &sample_to).unwrap();
    let parts = children(4242, (&lsof_to, &sample_to), true, by, |_, _| {
        vec![Ok(exited(0, "", ""))]
    });
    assert_eq!(
        got(&parts),
        [
            ("lsof", Err("lsof was not run".into())),
            ("sample", Err("sample wrote no file".into())),
        ],
        "a link at sample's file is no sample"
    );
}
