// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_tty_lookup_is_trimmed_and_nothing_else_is_touched() {
    let script = format!("#!/bin/sh\n{STOCK_TTY}\ncols=$(tput cols < $tty)\n");
    let (out, changed) = trim_tty(&script).expect("trims");
    assert!(changed);
    assert_eq!(
        out,
        format!("#!/bin/sh\n{TRIMMED_TTY}\ncols=$(tput cols < $tty)\n")
    );
    assert_eq!(
        trim_tty("echo hi\n").expect("untouched"),
        ("echo hi\n".into(), false)
    );
    assert!(
        trim_tty("t=$(ps -o tty= -p 1)\n").is_err(),
        "another lookup is refused"
    );
}

#[test]
fn a_dat_parses_with_gaps_and_pools_per_terminal() {
    let dat = parse_dat("a b\n1 2\n3 _\n5 6\n");
    assert_eq!(dat["a"], vec![1.0, 3.0, 5.0]);
    assert_eq!(dat["b"], vec![2.0, 6.0]);
    let table = rows(&[
        (Term::Aterm, dat.clone()),
        (Term::Ghostty, parse_dat("a\n10\n")),
    ]);
    assert_eq!(table.len(), 2);
    assert!(
        table[0].starts_with("| vtebench a | aterm 3 ms (n=3) | ghostty-tip 10 ms (n=1)"),
        "{}",
        table[0]
    );
    assert!(table[1].contains("ghostty-tip no samples"), "{}", table[1]);
}

#[test]
fn a_trial_at_another_grid_or_short_a_benchmark_is_refused_and_kept() {
    let dir = super::super::Scratch::new("atv-vte").expect("scratch");
    let prefix = dir.path().join("aterm-1");
    std::fs::write(prefix.with_extension("dat"), "a b\n1 2\n").expect("write");
    assert!(judge_trial("vte 40 120", &prefix, 40, 120, 2).is_ok());
    let e = judge_trial("vte 40 120", &prefix, 40, 120, 3).expect_err("short");
    assert!(e.contains("2 of 3"), "{e}");
    assert!(prefix.with_extension("refused").is_file());
    std::fs::write(prefix.with_extension("dat"), "a b\n1 2\n").expect("write");
    assert!(judge_trial("vte 24 80", &prefix, 40, 120, 2).is_err());
    assert!(judge_trial("fail vtebench exited 1", &prefix, 40, 120, 2).is_err());
}

#[test]
fn a_clone_stage_publishes_atomically_and_never_removes_a_concurrent_destination() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let destination = root.path().join("vtebench");
    let stage = CloneStage::beside(&destination).expect("stage");
    std::fs::write(stage.path().join("clone"), b"complete").expect("staged clone");
    stage.publish().expect("publish");
    assert_eq!(
        std::fs::read(destination.join("clone")).expect("published clone"),
        b"complete"
    );

    let destination = root.path().join("raced-vtebench");
    let stage = CloneStage::beside(&destination).expect("stage");
    let stage_path = stage.path().to_path_buf();
    std::fs::write(stage.path().join("partial"), b"mine").expect("partial clone");
    std::fs::create_dir(&destination).expect("concurrent destination");
    std::fs::write(destination.join("theirs"), b"keep").expect("concurrent file");
    let error = stage.publish().expect_err("nonempty destination is final");
    assert!(error.contains("refusing to replace nonempty"), "{error}");
    assert_eq!(
        std::fs::read(destination.join("theirs")).expect("concurrent file preserved"),
        b"keep"
    );
    assert!(!stage_path.exists(), "only the private stage is cleaned");
}
