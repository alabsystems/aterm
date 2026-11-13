// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::{row, stuck};
use super::*;
use crate::jobs::Named;

#[test]
fn status_says_the_worst_live_job_and_brief_spends_nothing_on_an_idle_tab() {
    assert_eq!(status_fields(None, &[7], 0), "job=- job_quiet_ms=-");
    let mut quiet = row("a1", 7, Class::Waiting, Verdict::Quiet);
    quiet.quiet_ms = Some(400_000);
    let view = JobsView {
        rows: vec![
            row("a0", 7, Class::Running, Verdict::Fine),
            quiet,
            stuck("a2", 7),
            row("a3", 7, Class::Exited, Verdict::Exited),
            stuck("a9", 8),
        ],
        ..JobsView::default()
    };
    assert_eq!(
        status_fields(Some(&view), &[7], 0),
        "job=stuck job_quiet_ms=1152000"
    );
    assert_eq!(
        status_fields(Some(&view), &[7], 3_000),
        "job=stuck job_quiet_ms=1155000",
        "the quiet age counts on past a view 3 s old"
    );
    let calm = JobsView {
        rows: vec![row("a3", 7, Class::Exited, Verdict::Exited)],
        ..JobsView::default()
    };
    assert_eq!(
        status_fields(Some(&calm), &[7], 0),
        "job=none job_quiet_ms=-"
    );
    assert_eq!(brief_line(&calm, &[7]), None);
    assert_eq!(
        status_fields(Some(&view), &[5], 0),
        "job=none job_quiet_ms=-"
    );
    let line = brief_line(&view, &[7]).expect("live jobs");
    assert!(
        line.starts_with("jobs 4 jobs · 1 running · 1 quiet · 1 stuck · 1 exited · worst: "),
        "{line}"
    );
    assert!(!line.contains("curl"), "no name a process chose: {line}");
}

#[test]
fn status_counts_this_tabs_held_programs_and_the_oldest_to_now() {
    let held = |pid, tab, for_ms| Named {
        pid,
        exec_version: None,
        name: "git-remote-https".into(),
        tab,
        job: None,
        for_ms,
    };
    let view = JobsView {
        findings: Findings {
            program_check: vec![
                held(1, Some(7), 30_000),
                held(2, Some(7), 1_140_000),
                held(3, Some(8), 5_000),
                held(4, None, 9_000_000),
            ],
            ..Findings::default()
        },
        ..JobsView::default()
    };
    assert_eq!(
        program_check_field(None, &[7], 0),
        "program_check=- program_check_stranded=-"
    );
    assert_eq!(
        program_check_field(Some(&view), &[7], 2_000),
        "program_check=2:1142000 program_check_stranded=-",
        "the oldest of this tab's two, counted on past a view 2 s old"
    );
    assert_eq!(
        program_check_field(Some(&view), &[8], 2_000),
        "program_check=1:7000 program_check_stranded=-"
    );
    assert_eq!(
        program_check_field(Some(&view), &[9], 2_000),
        "program_check=0 program_check_stranded=-",
        "another tab's held program, and one no tab holds, are not this tab's"
    );
}

/// `program_check_stranded=` counts this tab's stranded programs and the oldest to now beside
/// its held ones: a count whenever one is named, `0` only when the view judged every held row,
/// and `-` with none named otherwise, or with no syspolicyd judged against.
#[test]
fn status_counts_a_tabs_stranded_programs_beside_its_held_ones() {
    let held = |pid, tab, for_ms| Named {
        pid,
        exec_version: Some(u64::from(pid)),
        name: "band".into(),
        tab: Some(tab),
        job: None,
        for_ms,
    };
    let judged = |stranded, every| JobsView {
        findings: Findings {
            program_check: vec![held(1, 7, 1_860_000), held(2, 7, 30_000), held(3, 8, 5_000)],
            program_check_stranded: stranded,
            program_check_daemon: Some(11291),
            program_check_judged: every,
            ..Findings::default()
        },
        ..JobsView::default()
    };
    let view = judged(vec![held(1, 7, 1_860_000), held(3, 8, 5_000)], false);
    assert_eq!(
        program_check_field(Some(&view), &[7], 2_000),
        "program_check=2:1862000 program_check_stranded=1:1862000",
        "this tab's one stranded, its oldest to now, though not every row was judged; \
         another tab's is not this tab's"
    );
    assert_eq!(
        program_check_field(Some(&judged(Vec::new(), true)), &[7], 2_000),
        "program_check=2:1862000 program_check_stranded=0",
        "CONTROL: every row judged, none stranded"
    );
    assert_eq!(
        program_check_field(Some(&judged(Vec::new(), false)), &[7], 2_000),
        "program_check=2:1862000 program_check_stranded=-",
        "none stranded, but a row was not judged: no verdict"
    );
    let unjudged = JobsView {
        findings: Findings {
            program_check_daemon: None,
            program_check_stranded: Vec::new(),
            program_check_judged: true,
            ..view.findings.clone()
        },
        ..JobsView::default()
    };
    assert_eq!(
        program_check_field(Some(&unjudged), &[7], 2_000),
        "program_check=2:1862000 program_check_stranded=-",
        "CONTROL: no daemon judged against is no verdict"
    );
}
use crate::sys::json::contract_violations;

fn view(rows: Vec<JobRow>) -> JobsView {
    JobsView {
        rows,
        findings: Findings::default(),
        demand: crate::jobs::Demand::Fast,
        lanes_off: Vec::new(),
        parked_tabs: Vec::new(),
        at_ms: 0,
    }
}

#[test]
fn a_scope_reads_only_its_sessions_rows_live_first() {
    let mut done = row("a1", 7, Class::Exited, Verdict::Exited);
    done.exit = Some(0);
    let v = view(vec![
        done,
        row("a3", 9, Class::Running, Verdict::Fine),
        stuck("a2", 7),
        row("b4", 7, Class::Running, Verdict::Fine),
    ]);
    let rows = rows_in(&v, &[7]);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["a2", "b4", "a1"], "tab 9's row is not this scope's");
    assert_eq!(
        summary(&rows),
        "3 jobs · 1 running · 1 stuck · 1 exited · worst: silent 19m12s, about 19× the assumed 60 s gap, φ 8.34"
    );
    assert_eq!(summary(&rows_in(&v, &[42])), "no jobs");
}

#[test]
fn rows_print_the_engines_words_and_json_keeps_the_contract() {
    let v = view(vec![stuck("a40977", 7)]);
    let rows = rows_in(&v, &[7]);
    let lines = row_lines(&v, &rows, &Findings::default(), true, 8, &|k| {
        (k == 7).then_some(1)
    });
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].text, "finding kind=process-table own=0");
    assert!(
        lines[0].text.starts_with("job id=a40977 kind=agent-call"),
        "{}",
        lines[0].text
    );
    assert!(lines[0].text.contains(" remote=127.0.0.1:47811"));
    assert!(lines[0].text.ends_with(" verdict=stuck tab=1"));
    let j = row_json(rows[0], Some(1));
    assert!(contract_violations(&j).is_empty(), "{:?}", j.to_text());
    assert_eq!(j.get("phi_m"), Some(&Jv::Int(8_338)));
    assert_eq!(j.get("kind"), Some(&Jv::s("agent_call")));
    let s = summary_json(&rows);
    assert!(contract_violations(&s).is_empty());
    assert_eq!(s.get("stuck"), Some(&Jv::Int(1)));
    assert_eq!(s.get("worst_quiet_ms"), Some(&Jv::Int(1_152_000)));
}

#[test]
fn json_preserves_the_active_descendant_that_qualifies_a_quiet_wrapper() {
    let mut wrapper = stuck("a14175", 7);
    wrapper.class = Class::Polling;
    wrapper.working_descendant = Some((72956, 1_793_213_706_219_818));
    let wire = wrapper.wire();
    let json = row_json(&wrapper, Some(1));
    assert!(wire.contains(" working_descendant=72956 "));
    assert_eq!(json.get("working_descendant"), Some(&Jv::Int(72956)));
    assert_eq!(
        json.get("working_descendant_start"),
        Some(&Jv::Int(1_793_213_706_219_818))
    );
    assert_eq!(json.get("verdict"), Some(&Jv::s("stuck")));
    assert_eq!(json.get("class"), Some(&Jv::s("polling")));
    assert!(contract_violations(&json).is_empty());

    wrapper.working_descendant = None;
    let json = row_json(&wrapper, Some(1));
    assert!(json.get("working_descendant").is_none());
    assert!(json.get("working_descendant_start").is_none());
}

#[test]
fn short_views_qualify_local_verdicts_with_the_same_descendant_generation() {
    let mut wrapper = stuck("a48757", 7);
    wrapper.class = Class::Polling;
    wrapper.no_progress = false;
    for birth in [1_793_213_706_219_818, u64::MAX] {
        wrapper.working_descendant = Some((16998, birth));
        let raw = wrapper.wire();
        let v = view(vec![wrapper.clone()]);
        assert_eq!(
            status_fields(Some(&v), &[7], 3_000),
            format!(
                "job=stuck job_quiet_ms=1155000 job_working_descendant=16998 job_working_descendant_start={birth}"
            )
        );
        let text = summary(&rows_in(&v, &[7]));
        assert!(text.contains("worst local:"), "{text}");
        assert!(text.contains("working descendant pid=16998"), "{text}");
        assert!(text.contains(&format!("birth={birth}")), "{text}");
        assert_eq!(brief_line(&v, &[7]), Some(format!("jobs {text}")));
        assert_eq!(v.rows[0].wire(), raw);
        let said = v.rows[0].said();
        assert!(
            said.contains("local") && said.contains("activity"),
            "{said}"
        );
        assert!(said.contains("16998"), "{said}");
        assert_eq!(v.rows[0].class, Class::Polling);
        assert_eq!(v.rows[0].verdict, Verdict::Stuck);
        assert!(!v.rows[0].no_progress);
    }
}

#[test]
fn short_views_do_not_borrow_another_jobs_descendant_context() {
    let local = stuck("a1", 7);
    let mut other = stuck("a2", 7);
    other.phi_milli = Some(4_000);
    other.quiet_ms = Some(4_000);
    other.working_descendant = Some((16998, 99));
    let mut v = view(vec![local, other]);
    assert_eq!(
        status_fields(Some(&v), &[7], 0),
        "job=stuck job_quiet_ms=1152000"
    );
    let text = summary(&rows_in(&v, &[7]));
    assert!(!text.contains("working descendant"), "{text}");
    assert!(!text.contains("worst local"), "{text}");
    v.rows[1].tab = 8;
    assert_eq!(
        status_fields(Some(&v), &[7], 0),
        "job=stuck job_quiet_ms=1152000"
    );
    assert!(
        status_fields(Some(&v), &[8], 0)
            .contains("job_working_descendant=16998 job_working_descendant_start=99")
    );
    assert!(!brief_line(&v, &[7]).unwrap().contains("working descendant"));
}

#[test]
fn a_qualified_summary_without_phi_keeps_the_local_verdict_explicit() {
    let mut wrapper = row("a1", 7, Class::Polling, Verdict::Quiet);
    wrapper.working_descendant = Some((16998, 99));
    let mut v = view(vec![wrapper]);
    let text = summary(&rows_in(&v, &[7]));
    assert!(text.contains("local quiet:"), "{text}");
    assert!(
        text.contains("working descendant pid=16998 birth=99"),
        "{text}"
    );
    assert!(!text.contains('φ'), "unknown phi is not zero: {text}");
    v.rows[0].working_descendant = None;
    assert!(!v.rows[0].said().contains("activity"));
    assert_eq!(summary(&rows_in(&v, &[7])), "1 job · 1 quiet");
    assert_eq!(status_fields(Some(&v), &[7], 0), "job=quiet job_quiet_ms=-");
    v.rows[0].working_descendant = Some((16998, 99));
    v.rows[0].class = Class::Exited;
    v.rows[0].verdict = Verdict::Exited;
    assert_eq!(summary(&rows_in(&v, &[7])), "1 job · 1 exited");
    assert_eq!(status_fields(Some(&v), &[7], 0), "job=none job_quiet_ms=-");
    assert_eq!(brief_line(&v, &[7]), None);
}

#[test]
fn a_finding_prints_only_where_the_scope_reaches_unless_global() {
    let two = |pid: u32, name: &str| {
        vec![
            Named {
                pid,
                exec_version: None,
                name: name.into(),
                tab: Some(7),
                job: None,
                for_ms: 4_000,
            },
            Named {
                pid: pid + 1,
                exec_version: None,
                name: name.into(),
                tab: None,
                job: None,
                for_ms: 4_000,
            },
        ]
    };
    let f = Findings {
        stopped: vec![
            Named {
                pid: 10,
                exec_version: None,
                name: "vim".into(),
                tab: Some(7),
                job: None,
                for_ms: 0,
            },
            Named {
                pid: 11,
                exec_version: None,
                name: "someone-elses".into(),
                tab: None,
                job: None,
                for_ms: 0,
            },
        ],
        program_check: vec![
            Named {
                pid: 63454,
                exec_version: None,
                name: "git-remote-https".into(),
                tab: Some(7),
                job: None,
                for_ms: 1_139_000,
            },
            Named {
                pid: 25311,
                exec_version: None,
                name: "gh".into(),
                tab: None,
                job: None,
                for_ms: 35_940_000,
            },
        ],
        translating: two(7010, "x86tool"),
        not_started: two(7020, "nfsprog"),
        stopped_unstarted: two(7040, "debuggee")
            .into_iter()
            .map(|named| crate::jobs::StoppedUnstarted {
                named,
                stopped: true,
                traced: false,
            })
            .collect(),
        probably_program_check: two(7030, "trustc"),
        own_procs: 300,
        process_limit: Some(5_000),
        ..Findings::default()
    };
    let pids = |list: &[Named]| list.iter().map(|n| n.pid).collect::<Vec<_>>();
    let mine = findings_in(&f, &[7], false);
    assert_eq!(mine.stopped.len(), 1);
    assert_eq!(mine.stopped[0].pid, 10);
    assert_eq!(
        pids(&mine.program_check),
        [63454],
        "a held program no tab holds prints only at scope=global"
    );
    assert_eq!(
        [
            pids(&mine.translating),
            pids(&mine.not_started),
            pids(&mine.probably_program_check)
        ],
        [[7010], [7020], [7030]],
        "nor does a program that has not started, or one that probably waits"
    );
    assert_eq!(
        mine.stopped_unstarted
            .iter()
            .map(|s| s.named.pid)
            .collect::<Vec<_>>(),
        [7040],
        "nor one that reads stopped or traced"
    );
    assert_eq!(findings_in(&f, &[7], true).stopped_unstarted.len(), 2);
    let global = findings_in(&f, &[7], true);
    assert_eq!(global.stopped.len(), 2);
    assert_eq!(global.program_check.len(), 2);
    assert_eq!(
        [
            global.translating.len(),
            global.not_started.len(),
            global.probably_program_check.len()
        ],
        [2, 2, 2]
    );
    let j = findings_json(&mine);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    for item in [
        r#"{"kind":"program_check","pid":63454,"exe":"git-remote-https","for_ms":1139000}"#,
        r#"{"kind":"translating","pid":7010,"exe":"x86tool","for_ms":4000}"#,
        r#"{"kind":"not_started","pid":7020,"exe":"nfsprog","for_ms":4000}"#,
        r#"{"kind":"stopped_unstarted","pid":7040,"exe":"debuggee","stopped":true,"traced":false,"for_ms":4000}"#,
        r#"{"kind":"probably_program_check","pid":7030,"exe":"trustc","heuristic":"fresh-dylib"}"#,
    ] {
        assert!(j.to_text().contains(item), "{item}: {}", j.to_text());
    }
    let odd = Findings {
        stopped: vec![Named {
            pid: 12,
            exec_version: None,
            name: "evil\u{1b}[31m".into(),
            tab: Some(7),
            job: None,
            for_ms: 0,
        }],
        ..Findings::default()
    };
    let lines = row_lines(&view(Vec::new()), &[], &odd, false, 8, &|_| None);
    assert_eq!(lines[0].text, "finding kind=stopped pid=12 exe=-");
}
