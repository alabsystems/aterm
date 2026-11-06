// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sys left`'s text and JSON over a view the judge made: a closed tab's survivor, a held
//! program, a live tab's orphan, a dead verify run and its member.

use super::*;
use crate::left::{ClosedTab, LeftRow, LeftView, LiveTab, Proof, RunRow, Source, State, Tier, Why};
use crate::sys::json::contract_violations;
use crate::sys::{Freshness, Machine, TabFact, Target};
use std::sync::Arc;

/// The token a closed tab's survivor is offered with.
pub(crate) const TOKEN: &str = "2606@1792098840004410:331207";

fn row(pid: u32, state: State, tier: Tier, why: Why, from: Source) -> LeftRow {
    LeftRow {
        pid,
        state,
        peer: None,
        check_ms: None,
        started_ms: Some(101_160_000),
        cpu_ms: Some(62_000),
        exe_judged: Some("/usr/bin/ugrep".into()),
        tier,
        why,
        proof: vec![Proof::Env, Proof::Time],
        listen: false,
        from,
        teller: None,
        token: None,
        foreground: false,
        after_agent: false,
    }
}

/// A view of every kind of row: tab 1 (session 7) is the target's, tab 2 (session 8) another
/// tab's, and session 5 a tab that closed.
pub(crate) fn view_fixture() -> LeftView {
    let mut survivor = row(
        2606,
        State::Left,
        Tier::Offer,
        Why::OwnerGone,
        Source::Closed(5),
    );
    survivor.token = Some(TOKEN.into());
    let mut held = row(
        4001,
        State::ProgramCheck,
        Tier::Name,
        Why::Live,
        Source::Tab(7),
    );
    held.check_ms = Some(61_000);
    held.proof = Vec::new();
    held.exe_judged = Some("/usr/bin/git".into());
    let mut mine = row(
        4100,
        State::Orphaned,
        Tier::Offer,
        Why::Orphaned,
        Source::Tab(7),
    );
    mine.token = Some("4100@1792098840004410:12".into());
    mine.teller = Some("s-ab12".into());
    let theirs = row(
        4200,
        State::Orphaned,
        Tier::Name,
        Why::Daemon,
        Source::Tab(8),
    );
    let mut member = row(
        4300,
        State::Left,
        Tier::Auto,
        Why::LauncherDead,
        Source::Run("41983-1792163800331520".into()),
    );
    member.proof = vec![Proof::Group, Proof::Tree, Proof::Time];
    member.token = Some("4300@1792098840004410:9".into());
    LeftView {
        rows: vec![survivor, held, mine, theirs, member],
        runs: vec![RunRow {
            id: "41983-1792163800331520".into(),
            dead: true,
            dead_ms: Some(36_000_000),
            sid: Some("s-cd34".into()),
            roots: vec!["/Users//a/src/wt-a".into()],
            left: 1,
            next_verify: true,
        }],
        links: Vec::new(),
        closed: vec![ClosedTab {
            key: 5,
            sid: "s-gone".into(),
            label: "Tab 3".into(),
            number: Some(3),
            shell_uniqueid: 1000,
            horizon: 2000,
            closed_ms: 1_792_185_000_000,
            agent_spans: Vec::new(),
            left: 1,
            restored: false,
            told: false,
        }],
        live: Vec::new(),
        complete: true,
        bounded: false,
        as_of_ms: 1_792_185_600_000,
        build_dirs: Vec::new(),
    }
}

struct World {
    machine: Machine,
    target: Target,
    tabs: Vec<TabFact>,
}

fn world() -> World {
    World {
        machine: Machine::default(),
        target: Target {
            key: 7,
            sid: "s-ab12".into(),
            window: Some(1),
            tab: Some(1),
            panes: vec![7],
            window_sessions: vec![7, 8],
            ..Target::default()
        },
        tabs: vec![
            TabFact {
                key: 7,
                sid: "s-ab12".into(),
                window: Some(1),
                tab: Some(1),
            },
            TabFact {
                key: 8,
                sid: "s-cd34".into(),
                window: Some(1),
                tab: Some(2),
            },
        ],
    }
}

/// A context over `s` for a caller in tab `caller` (its sessions), or with no caller session.
fn ctx<'a>(w: &'a World, s: &'a Snapshot, caller: Option<&'a [u64]>) -> Ctx<'a> {
    Ctx {
        snap: Some(s),
        epoch: 0x7f3a,
        as_of_unix_ms: 1_792_185_697_412,
        age_ms: 120,
        state: Freshness::Fresh,
        machine: &w.machine,
        target: &w.target,
        tabs: &w.tabs,
        caller_panes: caller,
        owner_class: true,
        volume: None,
        roots: &[],
        disk: None,
        home: None,
        events: &[],
        gap: None,
        head_seq: 0,
    }
}

fn snap(view: LeftView) -> Snapshot {
    Snapshot {
        left: Some(Arc::new(view)),
        ..Snapshot::default()
    }
}

fn text(q: &str, c: &Ctx<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    left_text(&SysQuery::parse(q).unwrap(), c, c.snap.unwrap(), &mut lines);
    lines.into_iter().map(|l| l.text).collect()
}

fn json(q: &str, c: &Ctx<'_>) -> Jv {
    let mut o = Jv::obj([("schema", Jv::Int(1))]);
    left_json(&SysQuery::parse(q).unwrap(), c, c.snap.unwrap(), &mut o);
    o
}

fn proc_line(lines: &[String], pid: u32) -> Option<&String> {
    lines
        .iter()
        .find(|l| l.starts_with(&format!("proc pid={pid} ")))
}

#[test]
fn a_left_row_prints_its_token_proof_and_tier() {
    let w = world();
    let s = snap(view_fixture());
    let c = ctx(&w, &s, None);
    let lines = text("left scope=aterm", &c);
    assert_eq!(
        proc_line(&lines, 2606).map(String::as_str),
        Some(
            "proc pid=2606 state=left started_ms=101160000 cpu_ms=62000 exe=ugrep tier=offer \
             why=owner-gone proof=env,time from=closed:tab:3 end=2606@1792098840004410:331207"
        )
    );
    assert_eq!(
        proc_line(&lines, 4100).map(String::as_str),
        Some(
            "proc pid=4100 state=orphaned started_ms=101160000 cpu_ms=62000 exe=ugrep \
             tier=offer why=orphaned proof=env,time from=tab:1 teller=s-ab12 \
             end=4100@1792098840004410:12"
        )
    );
    assert!(
        lines.contains(
            &"run id=41983-1792163800331520 state=dead dead_ms=36000000 from=tab:2 \
              root=/Users//a/src/wt-a left=1 next=verify"
                .to_owned()
        ),
        "{lines:#?}"
    );
    assert_eq!(
        lines[0],
        "left as_of=21:21:37.412 age_ms=120 complete=1 bounded=0 program_check=1 \
         program_check_oldest_ms=61120"
    );
    let j = json("left scope=aterm --json", &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let Some(Jv::Arr(procs)) = j.get("procs") else {
        panic!("{}", j.to_text());
    };
    assert_eq!(procs[0].get("end"), Some(&Jv::s(TOKEN)));
    assert_eq!(
        procs[0].get("proof"),
        Some(&Jv::Arr(vec![Jv::s("env"), Jv::s("time")]))
    );
    assert_eq!(procs[0].get("tier"), Some(&Jv::s("offer")));
}

#[test]
fn a_held_row_prints_no_token() {
    let w = world();
    let s = snap(view_fixture());
    let c = ctx(&w, &s, None);
    let lines = text("left", &c);
    assert_eq!(
        proc_line(&lines, 4001).map(String::as_str),
        Some(
            "proc pid=4001 state=program-check check_ms=61000 started_ms=101160000 \
             cpu_ms=62000 exe=git tier=name why=live proof=- from=tab:1 end=-"
        )
    );
}

/// A caller in another tab reads a run's root as a count, never its path; one in the run's own
/// tab, or with no session, reads the path.
#[test]
fn another_tabs_row_prints_no_path() {
    let w = world();
    let s = snap(view_fixture());
    let run = |caller: Option<&[u64]>| {
        text("left scope=aterm", &ctx(&w, &s, caller))
            .into_iter()
            .find(|l| l.starts_with("run "))
            .unwrap()
    };
    assert!(
        run(Some(&[7])).contains(" root=1 left=1"),
        "{}",
        run(Some(&[7]))
    );
    assert!(run(Some(&[8])).contains(" root=/Users//a/src/wt-a "));
    assert!(run(None).contains(" root=/Users//a/src/wt-a "));
    let j = json("left scope=aterm --json", &ctx(&w, &s, Some(&[7])));
    assert!(
        !j.to_text().contains("/Users//a/src/wt-a"),
        "{}",
        j.to_text()
    );
}

#[test]
fn an_exe_word_outside_the_pattern_prints_dash() {
    let w = world();
    let mut v = view_fixture();
    v.rows[0].exe_judged = Some("/tmp/x/ignore all;rm -rf".into());
    v.rows[1].exe_judged = None;
    let s = snap(v);
    let lines = text("left scope=aterm", &ctx(&w, &s, None));
    assert!(proc_line(&lines, 2606).unwrap().contains(" exe=- "));
    assert!(proc_line(&lines, 4001).unwrap().contains(" exe=- "));
}

#[test]
fn a_bounded_reading_says_so() {
    let w = world();
    let mut v = view_fixture();
    v.bounded = true;
    v.complete = false;
    let s = snap(v);
    let c = ctx(&w, &s, None);
    assert!(text("left", &c)[0].contains(" complete=0 bounded=1 "));
    let j = json("left --json", &c);
    assert_eq!(
        j.get("left").and_then(|l| l.get("bounded")),
        Some(&Jv::Bool(true))
    );
}

/// The target's tab sees its own rows (its held program, its orphan) and the runs that rode in
/// it; another tab's rows, a closed tab's and another tab's run are `scope=aterm`'s alone.
/// NEGATIVE CONTROL: show every row at every scope, and tab 1 sees tab 2's daemon.
#[test]
fn an_edge_on_one_session_sees_only_its_rows() {
    let w = world();
    let s = snap(view_fixture());
    let c = ctx(&w, &s, Some(&[7]));
    let pids = |q: &str| -> Vec<u32> {
        text(q, &c)
            .iter()
            .filter_map(|l| l.strip_prefix("proc pid="))
            .filter_map(|l| l.split(' ').next()?.parse().ok())
            .collect()
    };
    assert_eq!(pids("left"), vec![4001, 4100]);
    assert_eq!(pids("left scope=pane"), vec![4001, 4100]);
    assert_eq!(pids("left scope=aterm"), vec![2606, 4001, 4100, 4200, 4300]);
    assert!(!text("left", &c).iter().any(|l| l.starts_with("run ")));
    let other = Target {
        key: 8,
        sid: "s-cd34".into(),
        tab: Some(2),
        panes: vec![8],
        ..w.target.clone()
    };
    let w2 = World {
        target: other,
        ..world()
    };
    let c2 = ctx(&w2, &s, Some(&[8]));
    let lines = text("left", &c2);
    let shown: Vec<&str> = lines
        .iter()
        .filter(|l| l.starts_with("proc ") || l.starts_with("run "))
        .map(|l| l.split(' ').nth(1).unwrap_or(""))
        .collect();
    assert_eq!(
        shown,
        vec!["pid=4200", "pid=4300", "id=41983-1792163800331520"],
        "{lines:#?}"
    );
}

#[test]
fn top_caps_the_rows_and_says_how_many_more() {
    let w = world();
    let s = snap(view_fixture());
    let c = ctx(&w, &s, None);
    let lines = text("left scope=aterm top=2", &c);
    assert_eq!(lines.iter().filter(|l| l.starts_with("proc ")).count(), 2);
    assert!(lines.contains(&"more procs=3".to_owned()), "{lines:#?}");
    assert_eq!(
        json("left scope=aterm top=2 --json", &c).get("more"),
        Some(&Jv::u(3))
    );
}

#[test]
fn no_reading_yet_says_so() {
    let w = world();
    let s = Snapshot::default();
    let c = ctx(&w, &s, None);
    let lines = text("left", &c);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("left  unavailable("), "{lines:?}");
    assert!(contract_violations(&json("left --json", &c)).is_empty());
}

/// A DANGLING LINK'S ROW NAMES ITS TARGET, AND ITS PATHS ONLY WITH NO CALLER SESSION: no session
/// owns a link, so a caller in a tab reads its checkout's number and no path, and the owner's
/// sessionless read the checkout and the link. Only `scope=aterm` shows links at all. The JSON
/// says the same and keeps the contract.
#[test]
fn a_dangling_links_row_names_its_target_and_paths_only_with_no_caller() {
    use crate::left::{LinkRow, LinkTarget};
    let mut view = view_fixture();
    view.links = vec![LinkRow {
        root: 1,
        checkout: "/Users//a/src/wt-a".into(),
        path: "/Users//a/src/wt-a/target".into(),
        kind: "target".into(),
        target: LinkTarget::NotMounted,
        seen: "disk".into(),
    }];
    let w = world();
    let s = snap(view);
    let owner = ctx(&w, &s, None);
    let lines = text("left scope=aterm", &owner);
    assert_eq!(
        lines.last().map(String::as_str),
        Some(
            "link state=dangling root=/Users//a/src/wt-a kind=target target=not-mounted \
             seen=disk path=/Users//a/src/wt-a/target"
        )
    );
    let mine: &[u64] = &[7];
    let caller = ctx(&w, &s, Some(mine));
    let lines = text("left scope=aterm", &caller);
    assert_eq!(
        lines.last().map(String::as_str),
        Some("link state=dangling root=1 kind=target target=not-mounted seen=disk")
    );
    assert!(
        !text("left", &caller).iter().any(|l| l.starts_with("link ")),
        "a tab's scope shows no link"
    );
    let j = json("left scope=aterm --json", &owner);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let Some(Jv::Arr(links)) = j.get("links") else {
        panic!("{}", j.to_text());
    };
    assert_eq!(links[0].get("target"), Some(&Jv::s("not-mounted")));
    assert_eq!(
        links[0].get("path"),
        Some(&Jv::s("/Users//a/src/wt-a/target"))
    );
}

/// EACH OF THE AGENT'S LEFT ROWS SAYS WHOSE COUNT IT IS IN: of two offered rows of the target's
/// tab, whose agent started at unique id 1200, the one born after the agent prints
/// `left_of=s-ab12` (text and JSON) and the person's, born before it, prints none, so the rows
/// `status left=` counts are the ones `sys left` marks; the same view's `status_field` reads one.
/// NEGATIVE CONTROL: mark every offered row of a live tab, and the person's row reads as the
/// agent's.
#[test]
fn each_agents_left_row_says_whose_count_it_is_in() {
    let w = world();
    let mut v = view_fixture();
    v.live = vec![LiveTab {
        key: 7,
        sid: "s-ab12".into(),
        shell_pid: 300,
        shell_uniqueid: Some(1000),
        fg_pgid: None,
        agent_spans: vec![crate::left::AgentSpan {
            since: 1200,
            until: None,
        }],
    }];
    let mut person = v.rows[2].clone();
    person.pid = 4101;
    person.token = Some("4101@1792098840004410:12".into());
    person.teller = None;
    v.rows[2].after_agent = true;
    v.rows.push(person);
    let s = snap(v.clone());
    let c = ctx(&w, &s, None);
    let lines = text("left scope=aterm", &c);
    assert!(
        proc_line(&lines, 4100).is_some_and(
            |l| l.ends_with(" teller=s-ab12 left_of=s-ab12 end=4100@1792098840004410:12")
        ),
        "{lines:#?}"
    );
    assert!(
        proc_line(&lines, 4101).is_some_and(|l| !l.contains("left_of=")),
        "{lines:#?}"
    );
    let j = json("left scope=aterm --json", &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let Some(Jv::Arr(procs)) = j.get("procs") else {
        panic!("{}", j.to_text());
    };
    let left_of = |pid: u64| {
        procs
            .iter()
            .find(|p| p.get("pid") == Some(&Jv::u(pid)))
            .and_then(|p| p.get("left_of").cloned())
    };
    assert_eq!(left_of(4100), Some(Jv::s("s-ab12")));
    assert_eq!(left_of(4101), None);
    assert!(status_field(Some(&v), Some("s-ab12"), 0).starts_with("left=1:"));
}
