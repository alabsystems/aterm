// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_gap_is_read_only_past_where_the_watch_was_armed() {
    let lines: Vec<String> = [
        "sub 1 s-a",
        "GAP 1 events-dropped=3",
        "EVENT 1 meta field=title value=x",
        "EVENT * bye handoff",
        "sub 1 s-a",
        "GAP 1 events-dropped=65536",
        "GAP 1 log-lost=2 why=handoff resync_to=9 ep=3",
    ]
    .map(str::to_string)
    .to_vec();
    assert_eq!(
        events_gaps(&lines, 0),
        [("1".to_string(), 3), ("1".to_string(), 65536)]
    );
    assert_eq!(events_gaps(&lines, 3), [("1".to_string(), 65536)]);
    assert!(
        events_gaps(&lines, 6).is_empty(),
        "a log gap is not an events gap"
    );
}

#[test]
fn the_log_parsers_read_marks_by_their_kind_word() {
    let log = "OK 5 v=2 as_of=10 ep=3 oldest=1\n\
               7 mark - mail dir=in kind=task off=12 by=bridge src=fabric conf=exact\n\
               8 mark - human by=human src=input conf=strong\n\
               9 mark - human why=mail by=human src=input conf=strong\n\
               10 mark - note text=why%3Dmail by=ctl src=mark conf=claim\n\
               11 mark - mail dir=in kind=note off=15 by=bridge src=fabric conf=exact\n\
               12 mark - mail dir=out kind=task off=16 by=bridge src=fabric conf=exact\n";
    assert_eq!(
        mail_answers(log),
        1,
        "only a human mark that says why=mail answers mail"
    );
    assert_eq!(human_marks(log), 2);
    assert_eq!(mail_count(log), 2, "dir=out is not delivered mail");
    assert_eq!(
        mail_off(log),
        Some(15),
        "the newest delivered mail's offset"
    );
    assert_eq!(mail_off("8 mark - human by=human\n"), None);
    assert_eq!(delivery_marks(log, 12), [(7, None)]);
    assert!(
        delivery_marks(log, 16).is_empty(),
        "a mail sent is not delivered"
    );
    assert!(
        delivery_marks(log, 1).is_empty(),
        "off=1 is not off=12's prefix"
    );
    assert_eq!(
        delivery_marks(
            "21 mark - mail dir=in kind=task off=78 inbox=1 t=1700\n\
             65565 mark - mail dir=in kind=task off=78 inbox=4098 t=1911\n",
            78
        ),
        [(21, Some(1700)), (65565, Some(1911))],
        "each delivery's seq and t="
    );
    assert_eq!(
        human_marks("OK 1 v=2 human=3\n8 open t1 human\n"),
        0,
        "a mark is `<seq> mark <node> <kind>`"
    );
    assert_eq!(
        without_text("7 mark - mail dir=in text=QZP6SECRET1 off=12\n8 x"),
        "7 mark - mail dir=in off=12 | 8 x"
    );
}

#[test]
fn a_brief_links_a_turn_only_with_the_tilde() {
    let brief = "s-x idle\n  t2 closed by=agent mail=@90~\n  t1 closed mail=@12 cause=@5\n";
    assert_eq!(brief_links(brief), [90]);
    assert!(brief_links("t1 open mail=@x~").is_empty());
}

#[test]
fn the_broker_log_count_is_of_whole_needles() {
    assert_eq!(
        count_of(
            b"gap 1 events-dropped=3 .. gap 1 events-dropped=9",
            b"events-dropped="
        ),
        2
    );
    assert_eq!(count_of(b"events-dropped", b"events-dropped="), 0);
    assert_eq!(count_of(b"abc", b""), 0);
}

fn figures(n: u64, p: [f64; 3]) -> Figures {
    let mut figures = Figures::read(&format!(
        "OK n_present={n} present_p50_ms={:.2} present_p95_ms={:.2} present_p99_ms={:.2} n_input=2",
        p[0], p[1], p[2]
    ));
    figures
        .now_rows
        .push("tab-menu item text=\"now: approval\"".into());
    figures
}

#[test]
fn the_present_leg_judges_the_tested_median_against_the_baselines_runs() {
    let base = [
        figures(340, [1.0, 1.5, 6.0]),
        figures(344, [1.2, 1.7, 9.0]),
        figures(342, [1.1, 1.4, 5.0]),
    ];
    let same = [
        figures(341, [1.1, 1.6, 7.0]),
        figures(343, [1.3, 1.9, 2.0]),
        figures(342, [1.0, 1.2, 6.0]),
    ];
    assert!(
        matches!(judge_present(&base, &same), Verdict::Pass(_)),
        "{:?}",
        judge_present(&base, &same)
    );
    let faster = vec![figures(342, [0.2, 0.3, 1.0]); 3];
    assert!(
        matches!(judge_present(&base, &faster), Verdict::Pass(ref s) if s.contains("under")),
        "a latency under the baseline is no regression"
    );
    let slow_p50 = [
        figures(342, [1.3, 1.5, 6.0]),
        figures(342, [1.4, 1.5, 6.0]),
        figures(342, [0.9, 1.5, 6.0]),
    ];
    let v = judge_present(&base, &slow_p50);
    assert!(
        matches!(&v, Verdict::Fail(s) if s.contains("present_p50_ms tested median 1.3 ") && s.contains("OVER")),
        "a median over the baseline's highest run is OVER, p50 included: {v:?}"
    );
    assert_eq!(
        v.said().matches("OVER").count(),
        1,
        "only that figure: {v:?}"
    );
    let more = vec![figures(360, [1.0, 1.4, 6.0]); 3];
    assert!(
        matches!(judge_present(&base, &more), Verdict::Fail(ref s) if s.contains("n_present tested median 360 ") && s.contains("outside")),
        "n_present is held inside the baseline's runs"
    );
    let mut off = same.clone();
    off[1].now_rows.clear();
    assert!(
        matches!(judge_present(&base, &off), Verdict::Fail(ref s) if s.contains("tested run 2")),
        "a run with the clause off is not a clause-on figure"
    );
    let mut base_off = base.clone();
    base_off[0].now_rows.clear();
    assert!(
        matches!(judge_present(&base_off, &same), Verdict::Fail(ref s) if s.contains("baseline run 1")),
        "nor is a baseline run with it off"
    );
    let mut missing = same.clone();
    missing[0].present[2] = None;
    assert!(matches!(judge_present(&base, &missing), Verdict::Fail(_)));
    assert!(matches!(judge_present(&[], &same), Verdict::Fail(_)));
    assert_eq!(median_twice(&[3, 1, 2]), Some(4));
    assert_eq!(median_twice(&[4, 1, 2, 3]), Some(5));
    assert_eq!(median_twice(&[]), None);
}

#[test]
fn a_reported_even_median_at_the_baseline_boundary_is_not_over() {
    let read = |n: u64, p: [&str; 3]| {
        let mut figures = Figures::read(&format!(
            "OK n_present={n} present_p50_ms={} present_p95_ms={} present_p99_ms={} n_input=3",
            p[0], p[1], p[2]
        ));
        figures
            .now_rows
            .push("tab-menu item text=\"now: approval\"".into());
        figures
    };
    let baseline = [
        read(347, ["1.84", "4.72", "6.82"]),
        read(349, ["1.97", "4.72", "6.29"]),
        read(348, ["1.97", "5.77", "9.44"]),
        read(348, ["1.84", "4.72", "7.34"]),
    ];
    let tested = [
        read(349, ["1.97", "5.77", "10.49"]),
        read(347, ["1.97", "5.77", "11.53"]),
        read(347, ["1.97", "5.24", "6.29"]),
        read(348, ["1.84", "4.72", "8.39"]),
    ];
    // The inputs are reported to 0.01 ms, so an even median can be halfway
    // between those units. Neither a full unit nor a half-unit overshoot
    // may be rounded back inside the baseline.
    for upper_middle in ["10.51", "10.50"] {
        let mut over = tested.clone();
        over[0] = read(349, ["1.97", "5.77", upper_middle]);
        let verdict = judge_present(&baseline, &over);
        assert!(
            matches!(&verdict, Verdict::Fail(_)),
            "a genuine p99 median above 9.44 ms must fail ({upper_middle}): {verdict:?}"
        );
    }
    // The retained live run reported middle p99 values 8.39 and 10.49:
    // their exact reported median is 9.44, the baseline's upper bound.
    let verdict = judge_present(&baseline, &tested);
    assert!(
        matches!(&verdict, Verdict::Pass(_)),
        "an equal reported p99 boundary must pass without a binary-float overshoot: {verdict:?}"
    );
}

#[test]
fn the_latch_passes_only_on_the_outgoing_processs_own_mark() {
    let before = [(21, Some(1700))];
    let rows = "rows";
    assert!(matches!(
        latch_verdict(Ok(0), &[78], 78, &before, &before, rows),
        Verdict::Pass(_)
    ));
    let again = [(21, Some(1700)), (65565, Some(1911))];
    assert!(
        matches!(latch_verdict(Ok(0), &[], 78, &before, &again, rows), Verdict::Fail(ref s) if s.contains("delivered 2 times")),
        "a second delivery mark is named"
    );
    let own = [(65565, Some(1911))];
    assert!(
        matches!(latch_verdict(Ok(0), &[78], 78, &before, &own, rows), Verdict::Fail(ref s) if s.contains("not the carry's")),
        "a link to the successor's own mark (a carry that lost the waiting mail, or a log that started over) is not the latch's crossing"
    );
    assert!(
        matches!(
            latch_verdict(Ok(0), &[78], 78, &before, &again, rows),
            Verdict::Fail(_)
        ),
        "nor is a link beside a second mark"
    );
    assert!(matches!(
        latch_verdict(Ok(0), &[77], 78, &before, &before, rows),
        Verdict::Fail(_)
    ));
    assert!(matches!(
        latch_verdict(Ok(0), &[78], 78, &again, &again, rows),
        Verdict::Fail(ref s) if s.contains("not once")
    ));
    assert!(matches!(
        latch_verdict(Err("no turn".into()), &[78], 78, &before, &before, rows),
        Verdict::Fail(ref s) if s == "no turn"
    ));
}

#[test]
fn native_key_ack_needs_exact_active_posting() {
    let reply = |code, stdout: &str| Reply {
        code,
        stdout: stdout.into(),
        ..Reply::default()
    };
    assert!(active_key_posted(&reply(0, "OK posted=1 key=1\n")));
    for (code, stdout) in [
        (1, "OK posted=1 key=1"),
        (0, "OK posted=0 key=1"),
        (0, "OK posted=2 key=1"),
        (0, "OK posted=? key=1"),
        (0, "OK posted=1 key=0"),
        (0, "OK posted=1"),
    ] {
        assert!(!active_key_posted(&reply(code, stdout)), "{stdout}");
    }
}

fn no_person() -> KeyPerson {
    KeyPerson {
        seq: 0,
        gesture_clear: true,
        typed_clear: true,
        human: 0,
        why_mail: 0,
    }
}

#[test]
fn native_delivery_needs_actual_bytes_without_person_side_effects() {
    let before = no_person();
    assert!(matches!(
        keys_verdict(&before, &before, "abc", Some("abc")),
        Verdict::Pass(_)
    ));
    for output in [None, Some("ab"), Some("aabbcc")] {
        assert!(matches!(
            keys_verdict(&before, &before, "abc", output),
            Verdict::Fail(_)
        ));
    }
    let mut controls = [before; 5];
    controls[0].seq = 1;
    controls[1].gesture_clear = false;
    controls[2].typed_clear = false;
    controls[3].human = 1;
    controls[4].why_mail = 1;
    for after in controls {
        assert!(matches!(
            keys_verdict(&before, &after, "abc", Some("abc")),
            Verdict::Fail(_)
        ));
    }
}

#[test]
fn native_person_snapshot_does_not_treat_unread_or_lost_as_empty() {
    let status = "OK human_seq=0 human_ms=- human_typed_ms=-";
    let log = |header: &str, stdout: &str| Reply {
        stderr: format!("aterm-ctl: {header}\n"),
        stdout: stdout.into(),
        ..Reply::default()
    };
    let complete = log("OK 0 v=2 as_of=3 ep=7 oldest=1", "");
    assert_eq!(key_person(status, &complete), Ok(no_person()));
    for status in [
        "OK human_ms=- human_typed_ms=-",
        "OK human_seq=? human_ms=- human_typed_ms=-",
        "OK human_seq=0 human_typed_ms=-",
        "OK human_seq=0 human_ms=-",
    ] {
        assert!(key_person(status, &complete).is_err());
    }
    for unread in [
        log("", ""),
        log("OK 0 v=2 ep=7 oldest=1 lost=1", ""),
        log("OK 0 v=2 ep=7 oldest=1 more=1", ""),
        log("OK 1 v=2 ep=7 oldest=1", "gap why=evicted lost=1"),
        Reply {
            code: 1,
            ..complete.clone()
        },
    ] {
        assert!(key_person(status, &unread).is_err());
    }
}

#[test]
fn a_refused_hwkey_is_a_desk_wall_only_when_privacy_names_its_grant_missing() {
    let privacy = "OK 3\nschema=1\nfull_disk_access=granted probe=ok\naccessibility=denied\n";
    for refusal in [
        "ERR hwkey: no frontmost AppKit window to post into",
        "ERR hwkey: a sudo password sheet is up",
        "ERR hwkey: refused while a tab menu is open",
    ] {
        assert!(
            matches!(press_refused("a", refusal, privacy), Verdict::Fail(ref s) if s.contains(refusal)),
            "{refusal}: a FAIL that quotes it"
        );
    }
    let walled = "ERR hwkey: the Accessibility grant is missing";
    assert!(matches!(
        press_refused("a", walled, privacy),
        Verdict::CouldNotRun(ref s) if s.contains("accessibility=denied")
    ));
    assert!(
        matches!(
            press_refused("a", walled, "accessibility=granted"),
            Verdict::Fail(_)
        ),
        "a grant privacy holds is no wall"
    );
    assert!(matches!(
        press_refused("a", "ERR hwkey: Full Disk Access", privacy),
        Verdict::Fail(_)
    ));
}

#[test]
fn a_topic_verdict_says_the_row_without_its_words() {
    let row = "msg 7 off=40 t=1 from=s-x@n-y kind=note trust=agent topic=seamless.t len=11 text=QZP6SECRET1";
    let v = topic_verdict("how", "seamless.t", "s-a", Some(row), true, "OK 1");
    assert!(
        matches!(&v, Verdict::Pass(s) if s.contains("off=40") && !s.contains("QZP6SECRET1") && !s.contains("text=")),
        "{v:?}"
    );
    assert!(matches!(
        topic_verdict("how", "seamless.t", "s-a", None, false, "OK 1"),
        Verdict::Fail(_)
    ));
}

#[test]
fn a_watch_that_ran_out_of_its_own_timeout_is_not_called_a_redial_failure() {
    let lines = |l: &[&str]| l.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let timed = watch_ended("w", 0, &lines(&["OK timeout"]), &[], "(not 75)");
    assert!(
        matches!(&timed, Verdict::Fail(s) if s.contains("the lab's bound")),
        "{timed:?}"
    );
    let ended = watch_ended("w", 1, &lines(&["ERR exited"]), &lines(&["x"]), "(not 75)");
    assert!(
        matches!(&ended, Verdict::Fail(s) if s.contains("ended 1 (not 75)") && !s.contains("bound")),
        "{ended:?}"
    );
}

#[test]
fn a_successor_that_never_answers_is_still_named_for_its_end() {
    let mut probes = [Some(10), Some(10), Some(42)].into_iter();
    let (next, seen) =
        successor_of(Duration::ZERO, 10, || probes.next().flatten(), || false).expect("no stop");
    assert_eq!(next, None);
    assert!(seen.is_empty(), "the parent is not a successor: {seen:?}");
    let (next, seen) = successor_of(Duration::ZERO, 10, || Some(42), || false).expect("no stop");
    assert_eq!(
        (next, seen),
        (None, vec![42]),
        "seen though it never answered, so it is ended by its pid"
    );
    let (next, seen) = successor_of(Duration::ZERO, 10, || Some(42), || true).expect("no stop");
    assert_eq!((next, seen), (Some(42), vec![42]));
}

#[test]
fn the_figures_are_read_from_their_own_keys() {
    let f = Figures::read(
        "OK n_present=338 present_p50_ms=1.18 present_p95_ms=1.44 present_p99_ms=5.77 \
         n_input=2 input_p50_ms=3.00",
    );
    assert_eq!(f.n_present, Some(338));
    assert_eq!(f.present, [Some(118), Some(144), Some(577)]);
    assert_eq!(f.n_input, Some(2));
    assert_eq!(Figures::read("OK n_input=2").n_present, None);
}

#[test]
fn every_present_run_has_present_and_input_samples_before_it_can_be_judged() {
    let read = |counts: &str| {
        let mut figures = Figures::read(&format!(
            "OK {counts} present_p50_ms=0.00 present_p95_ms=0.00 present_p99_ms=0.00"
        ));
        figures
            .now_rows
            .push("tab-menu item text=\"now: approval\"".into());
        figures
    };
    let populated = vec![read("n_present=1 n_input=1"); 3];
    assert!(
        matches!(judge_present(&populated, &populated), Verdict::Pass(_)),
        "one sample is valid, even when its reported latency rounds to zero"
    );

    let mut accepted = Vec::new();
    for cohort in ["baseline", "tested"] {
        for run in 0..populated.len() {
            for (missing, counts) in [
                ("missing n_present", "n_input=1"),
                ("zero n_present", "n_present=0 n_input=1"),
                ("missing n_input", "n_present=1"),
                ("zero n_input", "n_present=1 n_input=0"),
            ] {
                let mut baseline = populated.clone();
                let mut tested = populated.clone();
                let runs = if cohort == "baseline" {
                    &mut baseline
                } else {
                    &mut tested
                };
                runs[run] = read(counts);
                let verdict = judge_present(&baseline, &tested);
                if !matches!(verdict, Verdict::Fail(_)) {
                    accepted.push(format!("{cohort} run {} {missing}: {verdict:?}", run + 1));
                }
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "an invalid run cannot hide among populated runs: {}",
        accepted.join("; ")
    );
}

#[test]
fn the_present_judge_rejects_invalid_wire_units_and_keeps_large_counts_exact() {
    let read = |n: u64, p99: &str| {
        let mut figures = Figures::read(&format!(
            "OK n_present={n} present_p50_ms=1.00 present_p95_ms=1.00 present_p99_ms={p99} n_input=2"
        ));
        figures
            .now_rows
            .push("tab-menu item text=\"now: approval\"".into());
        figures
    };
    for value in ["1", "1.2", "1.20", "0.00", "184467440737095516.15"] {
        let f = read(342, value);
        assert!(
            matches!(
                judge_present(std::slice::from_ref(&f), std::slice::from_ref(&f)),
                Verdict::Pass(_)
            ),
            "valid reported value: {value}"
        );
    }
    for value in [
        "NaN",
        "inf",
        "-0.01",
        "+1",
        "1e0",
        "1.234",
        "1.",
        ".5",
        "",
        "184467440737095516.16",
        "184467440737095517",
    ] {
        let verdict = judge_present(&[read(342, "1.20")], &[read(342, value)]);
        assert!(
            matches!(&verdict, Verdict::Fail(s) if s.contains("present_p99_ms missing")),
            "invalid reported value {value:?}: {verdict:?}"
        );
    }
    let largest = u64::MAX;
    let pair = [read(largest - 1, "1.20"), read(largest, "1.20")];
    assert!(matches!(judge_present(&pair, &pair), Verdict::Pass(_)));
    assert!(matches!(
        judge_present(&[read(largest - 1, "1.20")], &pair),
        Verdict::Fail(_)
    ));
    assert!(matches!(
        judge_present(&[read(largest - 1, "1.20")], &[read(largest, "1.20")]),
        Verdict::Fail(_)
    ));
}

#[test]
fn a_failed_leg_outranks_one_that_could_not_run() {
    let pass = ("a", Verdict::Pass(String::new()));
    let cnr = ("b", Verdict::CouldNotRun(String::new()));
    let fail = ("c", Verdict::Fail(String::new()));
    assert_eq!(report(std::slice::from_ref(&pass)), 0);
    assert_eq!(report(&[pass.clone(), cnr.clone()]), 3);
    assert_eq!(report(&[cnr, fail, pass]), 1);
}
