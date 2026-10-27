// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Identity and generation controls for detached-call activity.

use super::*;

#[test]
fn adopted_member_requires_full_generation_group_parent_and_user_evidence() {
    let (_, mut sweep, mut facts) = fixture();
    for p in &mut sweep {
        p.pgid = 400;
    }
    for p in &mut facts {
        p.pgid = 400;
    }
    sweep[1].ppid = 1;
    facts[1].ppid = 1;
    let valid = |s: &[SweepProc], f: &[JobProc]| {
        let s = s.iter().map(|p| (p.pid, p)).collect();
        let f = f.iter().map(|p| (p.pid, p)).collect();
        adopted_member(402, 400, 400, &s, &f)
    };
    assert!(valid(&sweep, &facts));
    let mut reversed_births = facts.clone();
    reversed_births[1].start = Some(100);
    reversed_births[2].start = Some(1000);
    reversed_births[3].start = Some(500);
    reversed_births[1].age_ms = Some(2000);
    reversed_births[2].age_ms = Some(1000);
    reversed_births[3].age_ms = Some(1500);
    assert!(
        !valid(&sweep, &reversed_births),
        "a child cannot precede its immediate parent's birth"
    );
    let mut opaque_tokens = facts.clone();
    opaque_tokens[1].start = Some(900);
    opaque_tokens[2].start = Some(700);
    opaque_tokens[3].start = Some(100);
    assert!(
        valid(&sweep, &opaque_tokens),
        "start tokens are compared for identity, not chronology"
    );
    for defect in [
        "group",
        "parent",
        "user",
        "missing",
        "older",
        "missing-age",
        "zero-age",
    ] {
        let mut broken = facts.clone();
        match defect {
            "group" => broken[3].pgid = 999,
            "parent" => broken[3].ppid = 300,
            "user" => broken[3].uid = Some(502),
            "missing" => broken[2].start = None,
            "older" => broken[3].age_ms = Some(5000),
            "missing-age" => broken[2].age_ms = None,
            "zero-age" => broken[2].age_ms = Some(0),
            _ => unreachable!(),
        }
        assert!(!valid(&sweep, &broken), "{defect}");
    }
}

fn fixture() -> (Vec<JobRow>, Vec<SweepProc>, Vec<JobProc>) {
    let mut rows = Vec::new();
    let mut sweep = Vec::new();
    let mut facts = Vec::new();
    for (pid, parent, age) in [
        (300, 1, 4000),
        (400, 300, 3000),
        (401, 400, 2000),
        (402, 401, 1000),
    ] {
        let state = if pid == 402 {
            ProcState::Running
        } else {
            ProcState::Sleeping
        };
        sweep.push(SweepProc {
            pid,
            ppid: parent,
            uid: Some(501),
            state,
            has_tty: Some(pid == 300),
            ..SweepProc::default()
        });
        facts.push(JobProc {
            pid,
            ppid: parent,
            uid: Some(501),
            state,
            start: Some(u64::from(pid) * 10),
            age_ms: Some(age),
            ..JobProc::default()
        });
        if pid != 300 {
            let class = if pid == 402 {
                Class::BusySilent
            } else {
                Class::Waiting
            };
            let mut row =
                crate::sys::jobs_view::fixtures::row(&format!("a{pid}"), 1, class, Verdict::Quiet);
            row.leader = (pid, Some(u64::from(pid) * 10));
            row.cpu_mc = (pid == 402).then_some(1000);
            row.no_progress = pid != 402;
            rows.push(row);
        }
    }
    (rows, sweep, facts)
}

fn qualify(rows: &mut [JobRow], sweep: &[SweepProc], facts: &[JobProc], joined: Option<u32>) {
    let sweep = sweep.iter().map(|p| (p.pid, p)).collect();
    let facts = facts.iter().map(|p| (p.pid, p)).collect();
    let calls = joined
        .into_iter()
        .map(|leader| super::super::CallHint {
            leader,
            ..super::super::CallHint::default()
        })
        .collect();
    let tabs = [JobTab {
        key: 1,
        calls,
        ..JobTab::default()
    }];
    note_working_descendants(rows, &sweep, &facts, &tabs, 1000);
}

#[test]
fn working_descendant_requires_consistent_process_generations() {
    let (mut rows, sweep, facts) = fixture();
    qualify(&mut rows, &sweep, &facts, None);
    assert_eq!(rows[0].working_descendant, Some((402, 4020)));
    assert_eq!(rows[1].working_descendant, Some((402, 4020)));
    assert_eq!(rows[2].working_descendant, None);
    assert!(
        rows[0].no_progress && rows[1].no_progress,
        "local evidence remains"
    );
    for defect in [
        "leader-reuse",
        "ancestor-reuse",
        "missing-start",
        "missing-age",
        "zero-age",
        "younger-parent",
        "younger-outer",
        "cycle",
        "missing-parent",
        "parent-disagrees",
    ] {
        let (mut rows, mut sweep, mut facts) = fixture();
        match defect {
            "leader-reuse" => facts[3].start = Some(999),
            "ancestor-reuse" => rows[1].leader.1 = Some(999),
            "missing-start" => facts[2].start = None,
            "missing-age" => facts[2].age_ms = None,
            "zero-age" => facts[2].age_ms = Some(0),
            "younger-parent" => facts[2].age_ms = Some(500),
            "younger-outer" => facts[1].age_ms = Some(1000),
            "cycle" => {
                facts[1].ppid = 402;
                sweep[1].ppid = 402;
                for p in &mut facts {
                    p.age_ms = Some(1000);
                }
            }
            "missing-parent" => {
                facts.remove(2);
            }
            "parent-disagrees" => sweep[2].ppid = 999,
            _ => unreachable!(),
        }
        qualify(&mut rows, &sweep, &facts, None);
        assert!(
            rows.iter().all(|r| r.working_descendant.is_none()),
            "{defect}"
        );
    }
}

#[test]
fn working_descendant_stops_at_a_joined_context_and_expires_without_activity() {
    let (mut rows, sweep, facts) = fixture();
    qualify(&mut rows, &sweep, &facts, Some(401));
    assert_eq!(rows[0].working_descendant, None);
    assert_eq!(rows[1].working_descendant, Some((402, 4020)));
    rows[2].cpu_mc = Some(0);
    qualify(&mut rows, &sweep, &facts, Some(401));
    assert!(rows.iter().all(|r| r.working_descendant.is_none()));
}
