// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_bare_verb_is_the_overview() {
    let q = SysQuery::parse("").unwrap();
    assert_eq!(q.view, View::Overview);
    assert_eq!(q.top, TOP_DEFAULT);
    assert!(!q.json && q.since.is_none() && q.scope.is_none());
    assert!(q.target_rows_only());
}

#[test]
fn every_view_word_parses_with_its_options() {
    for v in View::ALL {
        let q = SysQuery::parse(v.word()).unwrap();
        assert_eq!(q.view, v);
    }
    let q = SysQuery::parse("procs top=12 fresh=100 --json since=7f3a.4812 caller=s-ab12").unwrap();
    assert_eq!(q.view, View::Procs);
    assert_eq!(q.top, 12);
    assert_eq!(q.fresh_ms, Some(FRESH_MIN_MS), "below 250 reads as 250");
    assert!(q.json);
    assert_eq!(
        q.since,
        Some(Cursor {
            epoch: 0x7f3a,
            seq: 4812
        })
    );
    assert_eq!(q.caller.as_deref(), Some("s-ab12"));
    assert_eq!(SysQuery::parse("fresh=0").unwrap().fresh_ms, Some(0));
    assert_eq!(SysQuery::parse("disk depth=3").unwrap().depth, Some(3));
    assert_eq!(SysQuery::parse("cores json").map(|q| q.json), Ok(true));
}

#[test]
fn refusals_answer_the_usage_line() {
    for bad in [
        "bogus",
        "tab scope=global",
        "overview scope=tab",
        "global scope=aterm",
        "procs top=0",
        "procs top=65",
        "procs top=+5",
        "procs top=x",
        "procs depth=2",
        "disk depth=+2",
        "disk depth=9",
        "fresh=+100",
        "since=7f3a",
        "since=zz.1",
        "procs instances=all",
        "jobs instances=all",
        "disk instances=all",
        "instances=some",
        "cores cores",
        "scope=moon procs",
        "name",
        "name job=a;b",
        "name job=a1 task=b2",
        "name job=a1 job=a2",
        "procs caller=a%20b",
        "why scope=tab",
        "why scope=global",
        "why instances=all",
        "why depth=2",
        "why top=65",
        "procs why",
        "cpu why",
    ] {
        assert_eq!(SysQuery::parse(bad), Err(USAGE), "{bad}");
    }
}

/// `why` takes at most one subject, either spelling; with none the reply
/// chooses. Any other word after it, a second subject included, answers
/// `why`'s own usage line, never the built one.
#[test]
fn why_takes_one_subject_and_answers_its_own_usage() {
    let subject = |s: &str| SysQuery::parse(s).map(|q| (q.view, q.subject));
    assert_eq!(subject("why"), Ok((View::Why, None)));
    for (text, want) in [
        ("why cpu", WhySubject::Cpu),
        ("why global.cpu", WhySubject::Cpu),
        ("why mem", WhySubject::Mem),
        ("why global.mem --json top=3", WhySubject::Mem),
    ] {
        assert_eq!(subject(text), Ok((View::Why, Some(want))), "{text}");
    }
    let q = SysQuery::parse("why top=3 fresh=0 since=7f3a.2 caller=s-1").unwrap();
    assert!(q.top_set && q.top == 3 && q.fresh_ms == Some(0) && q.since.is_some());
    assert!(!SysQuery::parse("why").unwrap().top_set);
    assert_eq!(SysQuery::parse("mem").unwrap().subject, None);
    for bad in [
        "why pid=1",
        "why cpu mem",
        "why mem mem",
        "why gpu",
        "why disk",
        "why tab.cpu",
        "why name",
        "why why",
        "why job=a1",
    ] {
        assert_eq!(SysQuery::parse(bad), Err(WHY_USAGE), "{bad}");
    }
    assert_eq!(
        SysQuery::parse("why").unwrap().freshness_default_ms(),
        5_000
    );
    assert_eq!(SysQuery::parse("why").unwrap().effective_scope(), None);
}

#[test]
fn the_scope_and_instance_forms_are_classified_for_access() {
    let rows_only = |s: &str| SysQuery::parse(s).unwrap().target_rows_only();
    for ok in [
        "",
        "overview",
        "tab",
        "global",
        "cores",
        "gpu",
        "mem",
        "jobs",
        "disk",
        "procs",
        "procs scope=tab",
        "procs scope=pane",
        "jobs scope=pane",
        "disk scope=window",
        "mem top=20",
        "why",
        "why cpu",
        "why global.mem --json",
    ] {
        assert!(rows_only(ok), "{ok}");
    }
    for owner in [
        "aterm",
        "procs scope=window",
        "procs scope=aterm",
        "procs scope=global",
        "jobs scope=aterm",
        "disk scope=aterm",
        "disk scope=global",
        "overview instances=all",
        "global instances=all",
        "procs scope=aterm instances=all",
        "name job=a40977",
        "name task=b4u3h4kc3",
    ] {
        assert!(!rows_only(owner), "{owner}");
    }
}

#[test]
fn naming_keeps_advisory_caller_without_resource_read_options() {
    for text in [
        "name job=b1",
        "name task=call1 --json",
        "--json name job=b1",
        "name job=b1 caller=s-origin",
        "name task=call1 --json caller=s-origin",
    ] {
        assert!(SysQuery::parse(text).is_ok(), "{text}");
    }
    for option in [
        "scope=tab",
        "scope=aterm",
        "top=1",
        "depth=1",
        "fresh=0",
        "since=1.1",
        "instances=all",
    ] {
        assert_eq!(
            SysQuery::parse(&format!("name job=b1 {option}")),
            Err(USAGE),
            "{option}"
        );
    }
    let query = SysQuery::parse("name job=b1 caller=s-unrelated").unwrap();
    assert_eq!(query.name, Some(NameTarget::Job("b1".into())));
    assert_eq!(query.caller.as_deref(), Some("s-unrelated"));
    assert_eq!(query.scope, None);
    assert!(
        !query.target_rows_only(),
        "advisory metadata grants no scope"
    );
}

/// `left` reads the target's pane or tab, or with `scope=aterm` every session's rows; a
/// window or the machine is no scope of it.
#[test]
fn left_takes_pane_tab_and_aterm_and_refuses_window_and_global() {
    let scope = |s: &str| SysQuery::parse(s).map(|q| (q.view, q.effective_scope()));
    assert_eq!(scope("left"), Ok((View::Left, Some(ScopeArg::Tab))));
    for (text, want) in [
        ("left scope=pane", ScopeArg::Pane),
        ("left scope=tab --json", ScopeArg::Tab),
        ("left scope=aterm top=3", ScopeArg::Aterm),
    ] {
        assert_eq!(scope(text), Ok((View::Left, Some(want))), "{text}");
    }
    for bad in [
        "left scope=window",
        "left scope=global",
        "left depth=2",
        "left instances=all",
        "left left",
        "left scope=aterm instances=all",
    ] {
        assert_eq!(SysQuery::parse(bad), Err(USAGE), "{bad}");
    }
}

/// Another session's leftovers are Owner's to read: `scope=aterm` reads rows outside the
/// target's tab, so it is not target-rows-only and `argument_escalation` makes it Owner-only.
/// NEGATIVE CONTROL: read `left` as target-rows-only at every scope, and `scope=aterm` passes
/// as a target's read.
#[test]
fn left_scope_aterm_reads_other_sessions_rows() {
    let rows_only = |s: &str| SysQuery::parse(s).unwrap().target_rows_only();
    assert!(rows_only("left"));
    assert!(rows_only("left scope=pane"));
    assert!(rows_only("left scope=tab --json"));
    assert!(!rows_only("left scope=aterm"));
    assert!(!rows_only("left scope=aterm --json top=5"));
}

#[test]
fn a_cursor_round_trips() {
    let c = Cursor {
        epoch: 0x7f3a,
        seq: 4812,
    };
    assert_eq!(c.to_string(), "7f3a.4812");
    assert_eq!(Cursor::parse("7f3a.4812"), Some(c));
    assert_eq!(Cursor::parse(".1"), None);
    assert_eq!(Cursor::parse("1."), None);
    assert_eq!(Cursor::parse("+7f3a.4812"), None);
    assert_eq!(Cursor::parse("7f3a.+4812"), None);
    assert_eq!(Cursor::parse("12345678901234567.1"), None);
}

#[test]
fn scopes_default_by_view() {
    assert_eq!(
        SysQuery::parse("procs").unwrap().effective_scope(),
        Some(ScopeArg::Tab)
    );
    assert_eq!(
        SysQuery::parse("mem").unwrap().effective_scope(),
        Some(ScopeArg::Global)
    );
    assert_eq!(SysQuery::parse("tab").unwrap().effective_scope(), None);
}
