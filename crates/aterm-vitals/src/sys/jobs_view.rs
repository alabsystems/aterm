// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scoped text and JSON formatting for `sys jobs` ([`crate::jobs::JobsView`]).

use super::json::Jv;
use super::view::Line;
use crate::jobs::wire::{exe_word, phi_text};
use crate::jobs::{Class, Findings, JobRow, JobsView, Verdict};
use crate::model::Snapshot;

/// The rows of `view` in the sessions `keys`: live rows before exited ones, then by id.
#[must_use]
pub fn rows_in<'a>(view: &'a JobsView, keys: &[u64]) -> Vec<&'a JobRow> {
    sorted(view.rows.iter().filter(|r| keys.contains(&r.tab)).collect())
}

/// Every row of `view` (`scope=global`, the owner's), ordered as [`rows_in`].
#[must_use]
pub fn rows_all(view: &JobsView) -> Vec<&JobRow> {
    sorted(view.rows.iter().collect())
}

fn sorted(mut rows: Vec<&JobRow>) -> Vec<&JobRow> {
    rows.sort_by(|a, b| {
        (a.verdict == Verdict::Exited)
            .cmp(&(b.verdict == Verdict::Exited))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// Why a snapshot carries no jobs view.
#[must_use]
pub fn missing_why(s: &Snapshot) -> &'static str {
    if s.procs_window_ns.is_none() {
        "warming"
    } else {
        "not_sampled"
    }
}

fn running(r: &JobRow) -> bool {
    r.class == Class::Running && r.verdict != Verdict::Exited
}

const SAID: [Verdict; 8] = [
    Verdict::WaitingYou,
    Verdict::Parked,
    Verdict::Quiet,
    Verdict::Busy,
    Verdict::Slow,
    Verdict::Stuck,
    Verdict::NeverReturns,
    Verdict::Exited,
];

fn worst<'a>(rows: &[&'a JobRow]) -> Option<&'a JobRow> {
    rows.iter()
        .copied()
        .filter(|r| {
            matches!(
                r.verdict,
                Verdict::Quiet | Verdict::Busy | Verdict::Slow | Verdict::Stuck
            )
        })
        .filter(|r| r.phi_milli.is_some_and(|p| p > 0))
        .max_by_key(|r| r.phi_milli.unwrap_or(0))
}

/// The summary clauses.
#[must_use]
pub fn summary(rows: &[&JobRow]) -> String {
    if rows.is_empty() {
        return "no jobs".to_owned();
    }
    let mut parts = vec![format!(
        "{} job{}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    )];
    let run = rows.iter().filter(|r| running(r)).count();
    if run > 0 {
        parts.push(format!("{run} running"));
    }
    for v in SAID {
        let n = rows.iter().filter(|r| r.verdict == v).count();
        if n > 0 {
            parts.push(format!("{n} {}", v.word()));
        }
    }
    if let Some(w) = worst(rows) {
        let phi = phi_text(w.phi_milli.unwrap_or(0));
        parts.push(match w.gap() {
            Some(gap) => format!("worst: {gap}, φ {phi}"),
            None => format!("worst: φ {phi}"),
        });
    }
    parts.join(" · ")
}

fn urgency(v: Verdict) -> Option<u8> {
    Some(match v {
        Verdict::Stuck => 0,
        Verdict::NeverReturns => 1,
        Verdict::Quiet => 2,
        Verdict::Slow => 3,
        Verdict::Busy => 4,
        Verdict::WaitingYou => 5,
        Verdict::Parked => 6,
        Verdict::Fine => 7,
        Verdict::Exited => return None,
    })
}

/// The live job of `rows` whose verdict asks most for a look (then the one quiet longest).
#[must_use]
pub fn worst_live<'a>(rows: &[&'a JobRow]) -> Option<&'a JobRow> {
    rows.iter()
        .copied()
        .filter_map(|r| urgency(r.verdict).map(|u| (u, r)))
        .min_by_key(|(u, r)| (*u, std::cmp::Reverse(r.quiet_ms.unwrap_or(0))))
        .map(|(_, r)| r)
}

/// Additive job status fields for `status`.
#[must_use]
pub fn status_fields(view: Option<&JobsView>, keys: &[u64], age_ms: u64) -> String {
    let Some(view) = view else {
        return "job=- job_quiet_ms=-".to_owned();
    };
    match worst_live(&rows_in(view, keys)) {
        Some(r) => format!(
            "job={} job_quiet_ms={}",
            r.verdict.word(),
            r.quiet_ms
                .map_or_else(|| "-".to_owned(), |q| q.saturating_add(age_ms).to_string())
        ),
        None => "job=none job_quiet_ms=-".to_owned(),
    }
}

/// `status`'s `program_check=` field: how many programs of the sessions `keys` macOS's program
/// check holds in `view` (members of their jobs, orphans of them included) and the oldest held
/// to now, `age_ms` being the view's age; `0` with none, `-` with no view.
#[must_use]
pub fn program_check_field(view: Option<&JobsView>, keys: &[u64], age_ms: u64) -> String {
    let Some(view) = view else {
        return "program_check=-".to_owned();
    };
    let held = view
        .findings
        .program_check
        .iter()
        .filter(|n| n.tab.is_some_and(|t| keys.contains(&t)));
    let (n, oldest) = held.fold((0usize, 0u64), |(n, oldest), h| {
        (n + 1, oldest.max(h.for_ms))
    });
    if n == 0 {
        "program_check=0".to_owned()
    } else {
        format!("program_check={n}:{}", oldest.saturating_add(age_ms))
    }
}

/// Formats the `brief` jobs line.
#[must_use]
pub fn brief_line(view: &JobsView, keys: &[u64]) -> Option<String> {
    let rows = rows_in(view, keys);
    worst_live(&rows)?;
    Some(format!("jobs {}", summary(&rows)))
}

fn value_line(key: String, text: String) -> Line {
    Line {
        key,
        text,
        value: true,
    }
}

/// The findings a scope may print.
#[must_use]
pub fn findings_in(f: &Findings, keys: &[u64], all: bool) -> Findings {
    let mine = |tab: Option<u64>| all || tab.is_some_and(|t| keys.contains(&t));
    Findings {
        stopped: f.stopped.iter().filter(|n| mine(n.tab)).cloned().collect(),
        zombies: f.zombies.iter().filter(|z| mine(z.tab)).cloned().collect(),
        d_state: f.d_state.iter().filter(|n| mine(n.tab)).cloned().collect(),
        busy_orphans: f
            .busy_orphans
            .iter()
            .filter(|n| mine(n.tab))
            .cloned()
            .collect(),
        program_check: f
            .program_check
            .iter()
            .filter(|n| mine(n.tab))
            .cloned()
            .collect(),
        program_check_aterm: f.program_check_aterm,
        program_check_episode: f.program_check_episode,
        program_check_drained: f.program_check_drained,
        translating: f
            .translating
            .iter()
            .filter(|n| mine(n.tab))
            .cloned()
            .collect(),
        not_started: f
            .not_started
            .iter()
            .filter(|n| mine(n.tab))
            .cloned()
            .collect(),
        probably_program_check: f
            .probably_program_check
            .iter()
            .filter(|n| mine(n.tab))
            .cloned()
            .collect(),
        own_procs: f.own_procs,
        process_limit: f.process_limit,
    }
}

/// The body lines after the `jobs` sentence ([`overflow`]).
pub fn row_lines(
    view: &JobsView,
    rows: &[&JobRow],
    findings: &Findings,
    wide: bool,
    top: usize,
    tab_number: &dyn Fn(u64) -> Option<usize>,
) -> Vec<Line> {
    let mut out = Vec::new();
    for r in rows.iter().take(top) {
        let mut t = r.wire();
        if wide && let Some(n) = tab_number(r.tab) {
            t.push_str(&format!(" tab={n}"));
        }
        out.push(value_line(format!("job {}:{}", r.tab, r.id), t));
    }
    for (i, f) in findings.wire_lines().into_iter().enumerate() {
        out.push(value_line(format!("finding {i}"), f));
    }
    if !view.lanes_off.is_empty() {
        out.push(value_line(
            "lanes".into(),
            format!("lanes {}", view.lanes_off.join(" ")),
        ));
    }
    out
}

/// Formats the `more=` overflow prefix for rows omitted by `top`, or `None` if all printed.
#[must_use]
pub fn overflow(rows: usize, top: usize) -> Option<String> {
    (rows > top).then(|| format!("{} jobs · top={}", rows - top, rows.min(64)))
}

fn enum_word(w: &str) -> String {
    w.replace('-', "_")
}

/// Formats one job row as JSON with integers in base units and `phi_m`.
#[must_use]
pub fn row_json(r: &JobRow, tab_number: Option<usize>) -> Jv {
    let mut o = Jv::obj([
        ("id", Jv::s(r.id.clone())),
        (
            "tab",
            tab_number.map_or_else(|| Jv::why("gone"), |n| Jv::u(n as u64)),
        ),
        ("kind", Jv::s(enum_word(r.kind.word()))),
        ("consumer", Jv::s(enum_word(r.consumer.word()))),
    ]);
    if let Some(t) = &r.task {
        o.set("task", Jv::s(exe_word(t)));
    }
    if let Some(p) = r.promoted_ms {
        o.set("promoted_ms", Jv::u(p));
    }
    o.set("pgid", Jv::u(u64::from(r.pgid)));
    o.set("exe", Jv::s(exe_word(&r.exe)));
    o.set("members", Jv::u(r.members as u64));
    o.set("class", Jv::s(r.class.word()));
    if let Some(w) = &r.wait {
        let mut wj = Jv::obj([("name", Jv::s(w.name.word()))]);
        if let Some(p) = w.peer {
            wj.set("peer", Jv::u(u64::from(p)));
        }
        if let Some((remote, state, queued)) = &w.tcp {
            if let Some(a) = remote {
                wj.set("remote", Jv::s(a.to_string()));
            }
            wj.set("state", Jv::s(state.word()));
            wj.set("queued_bytes", Jv::u(*queued));
        }
        if let Some(fd) = w.fd {
            wj.set("fd", Jv::u(fd));
        }
        if let Some(port) = w.port {
            wj.set("port", Jv::u(u64::from(port)));
        }
        o.set("wait", wj);
    }
    o.set(
        "named",
        r.named.as_ref().map_or_else(
            || Jv::why("not_sampled"),
            |n| {
                let mut nj = Jv::obj([("word", Jv::s(n.word.word()))]);
                if let Some(h) = &n.holder {
                    let mut hj = Jv::obj([
                        ("pid", Jv::u(u64::from(h.pid))),
                        ("exe", Jv::s(exe_word(&h.exe))),
                    ]);
                    if let Some(t) = h.tab {
                        hj.set("tab", Jv::u(u64::from(t)));
                    }
                    if let Some(c) = h.class {
                        hj.set("class", Jv::s(c.word()));
                    }
                    nj.set("holder", hj);
                }
                nj
            },
        ),
    );
    if let Some(s) = &r.stop {
        o.set(
            "stop",
            Jv::obj([
                ("parked", Jv::Bool(s.parked)),
                ("pid", Jv::u(u64::from(s.pid))),
                ("exe", Jv::s(exe_word(&s.name))),
            ]),
        );
    }
    if let Some(i) = &r.input {
        o.set(
            "input",
            Jv::obj([
                ("word", Jv::s(i.word.word())),
                ("bytes", Jv::u(i.bytes)),
                ("wait_ms", Jv::u(i.wait_ms)),
            ]),
        );
    }
    if let Some(c) = r.cpu_mc {
        o.set("cpu_mc", Jv::u(c));
    }
    if let Some(z) = &r.zombies {
        o.set(
            "zombies",
            Jv::obj([
                ("count", Jv::u(u64::from(z.count))),
                ("parent", Jv::u(u64::from(z.parent))),
                ("per_min_deci", Jv::u(z.per_min_deci)),
            ]),
        );
    }
    if let Some(q) = r.quiet_ms {
        o.set("quiet_ms", Jv::u(q));
    }
    if let Some(p) = r.phi_milli {
        o.set("phi_m", Jv::u(p));
    }
    if let Some(mu) = r.mu_ms {
        o.set("mu_ms", Jv::u(mu));
    }
    if let Some(from) = r.mu_from {
        o.set("mu_from", Jv::s(from.word()));
    }
    o.set("verdict", Jv::s(r.verdict.word()));
    o.set("no_progress", Jv::Bool(r.no_progress));
    if let Some((pid, start)) = r.working_descendant {
        o.set("working_descendant", Jv::u(u64::from(pid)));
        o.set("working_descendant_start", Jv::u(start));
    }
    o.set("episode", Jv::u(u64::from(r.episode)));
    if let Some(e) = r.exit {
        o.set("exit", Jv::Int(i64::from(e)));
    }
    o
}

/// The summary as JSON.
#[must_use]
pub fn summary_json(rows: &[&JobRow]) -> Jv {
    let mut o = Jv::obj([
        ("jobs", Jv::u(rows.len() as u64)),
        (
            "running",
            Jv::u(rows.iter().filter(|r| running(r)).count() as u64),
        ),
    ]);
    for v in SAID {
        let n = rows.iter().filter(|r| r.verdict == v).count() as u64;
        o.set(&enum_word(v.word()), Jv::u(n));
    }
    if let Some(w) = worst(rows) {
        if let Some(q) = w.quiet_ms {
            o.set("worst_quiet_ms", Jv::u(q));
        }
        o.set("worst_phi_m", Jv::u(w.phi_milli.unwrap_or(0)));
    }
    o
}

/// The findings as JSON objects, one per finding, and the process table.
#[must_use]
pub fn findings_json(f: &Findings) -> Jv {
    let named = |kind: &str, n: &crate::jobs::Named| {
        Jv::obj([
            ("kind", Jv::s(kind)),
            ("pid", Jv::u(u64::from(n.pid))),
            ("exe", Jv::s(exe_word(&n.name))),
            ("for_ms", Jv::u(n.for_ms)),
        ])
    };
    let mut items: Vec<Jv> = Vec::new();
    items.extend(f.stopped.iter().map(|n| named("stopped", n)));
    items.extend(f.zombies.iter().map(|z| {
        Jv::obj([
            ("kind", Jv::s("zombies")),
            ("parent", Jv::u(u64::from(z.ppid))),
            ("exe", Jv::s(exe_word(&z.name))),
            ("count", Jv::u(u64::from(z.count))),
            ("per_min_deci", Jv::u(z.per_min_deci)),
        ])
    }));
    items.extend(f.d_state.iter().map(|n| named("d_state", n)));
    items.extend(f.busy_orphans.iter().map(|n| named("busy_orphan", n)));
    items.extend(f.program_check.iter().map(|n| named("program_check", n)));
    items.extend(f.translating.iter().map(|n| named("translating", n)));
    items.extend(f.not_started.iter().map(|n| named("not_started", n)));
    items.extend(f.probably_program_check.iter().map(|n| {
        Jv::obj([
            ("kind", Jv::s("probably_program_check")),
            ("pid", Jv::u(u64::from(n.pid))),
            ("exe", Jv::s(exe_word(&n.name))),
            ("heuristic", Jv::s("fresh-dylib")),
        ])
    }));
    Jv::obj([
        ("items", Jv::Arr(items)),
        ("own_procs", Jv::u(f.own_procs)),
        (
            "process_limit",
            Jv::known_or(f.process_limit, "unsupported"),
        ),
    ])
}

/// Rows for the tests of this module and of `await sys`'s job terms.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::jobs::{
        Class, Consumer, JobKind, JobRow, MuFrom, Standing, Tick, Verdict, Wait, WaitName,
    };

    pub(crate) fn row(id: &str, tab: u64, class: Class, verdict: Verdict) -> JobRow {
        JobRow {
            id: id.into(),
            tab,
            kind: JobKind::AgentCall,
            consumer: Consumer::AgentBg,
            promoted_ms: None,
            task: Some("bq7f2k1za".into()),
            pgid: 40977,
            leader: (40977, None),
            exe: "curl".into(),
            members: 2,
            class,
            wait: None,
            named: None,
            stop: None,
            input: None,
            cpu_mc: None,
            zombies: None,
            standing: None,
            quiet_ms: None,
            phi_milli: None,
            mu_ms: None,
            mu_from: None,
            verdict,
            no_progress: false,
            working_descendant: None,
            episode: 1,
            exit: None,
            tick: Tick::default(),
        }
    }

    pub(crate) fn stuck(id: &str, tab: u64) -> JobRow {
        let mut r = row(id, tab, Class::Waiting, Verdict::Stuck);
        let mut w = Wait::bare(WaitName::Tcp, Standing::Capable, 40978, "curl");
        w.tcp = Some((
            Some("127.0.0.1:47811".parse().unwrap()),
            crate::jobs::TcpState::Established,
            0,
        ));
        r.wait = Some(w);
        r.quiet_ms = Some(1_152_000);
        r.phi_milli = Some(8_338);
        r.mu_ms = Some(60_000);
        r.mu_from = Some(MuFrom::Prior);
        r
    }
}

#[cfg(test)]
mod tests {
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
            name: "git-remote-https".into(),
            tab,
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
        assert_eq!(program_check_field(None, &[7], 0), "program_check=-");
        assert_eq!(
            program_check_field(Some(&view), &[7], 2_000),
            "program_check=2:1142000",
            "the oldest of this tab's two, counted on past a view 2 s old"
        );
        assert_eq!(
            program_check_field(Some(&view), &[8], 2_000),
            "program_check=1:7000"
        );
        assert_eq!(
            program_check_field(Some(&view), &[9], 2_000),
            "program_check=0",
            "another tab's held program, and one no tab holds, are not this tab's"
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
            "3 jobs · 1 running · 1 stuck · 1 exited · worst: silent 19m12s, about 19× its usual 60 s gap, φ 8.34"
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
    fn a_finding_prints_only_where_the_scope_reaches_unless_global() {
        let two = |pid: u32, name: &str| {
            vec![
                Named {
                    pid,
                    name: name.into(),
                    tab: Some(7),
                    for_ms: 4_000,
                },
                Named {
                    pid: pid + 1,
                    name: name.into(),
                    tab: None,
                    for_ms: 4_000,
                },
            ]
        };
        let f = Findings {
            stopped: vec![
                Named {
                    pid: 10,
                    name: "vim".into(),
                    tab: Some(7),
                    for_ms: 0,
                },
                Named {
                    pid: 11,
                    name: "someone-elses".into(),
                    tab: None,
                    for_ms: 0,
                },
            ],
            program_check: vec![
                Named {
                    pid: 63454,
                    name: "git-remote-https".into(),
                    tab: Some(7),
                    for_ms: 1_139_000,
                },
                Named {
                    pid: 25311,
                    name: "gh".into(),
                    tab: None,
                    for_ms: 35_940_000,
                },
            ],
            translating: two(7010, "x86tool"),
            not_started: two(7020, "nfsprog"),
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
            r#"{"kind":"probably_program_check","pid":7030,"exe":"trustc","heuristic":"fresh-dylib"}"#,
        ] {
            assert!(j.to_text().contains(item), "{item}: {}", j.to_text());
        }
        let odd = Findings {
            stopped: vec![Named {
                pid: 12,
                name: "evil\u{1b}[31m".into(),
                tab: Some(7),
                for_ms: 0,
            }],
            ..Findings::default()
        };
        let lines = row_lines(&view(Vec::new()), &[], &odd, false, 8, &|_| None);
        assert_eq!(lines[0].text, "finding kind=stopped pid=12 exe=-");
    }
}
