// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scoped text and JSON formatting for `sys jobs` ([`crate::jobs::JobsView`]).

use super::json::Jv;
use super::view::Line;
use crate::jobs::wire::{exe_word, phi_text, verdict_summary};
use crate::jobs::{Class, Findings, JobRow, JobsView, Verdict};
use crate::model::Snapshot;

/// Machine measurements beside the jobs: spare CPU is not a reservation, and an old slot
/// reading is not a promise of admission. Keep the ages and unavailable readings visible.
#[must_use]
pub fn capacity_json(s: &Snapshot, as_of_ms: u64, age_ms: u64) -> Jv {
    let cpu = s.cpu.as_ref();
    let slots = match s.slots {
        Some(crate::model::SlotsView::Read {
            n,
            free,
            discounted,
            pending,
            unknown,
            ..
        }) => Jv::obj([
            ("total", Jv::u(u64::from(n))),
            ("free", Jv::u(u64::from(free.saturating_sub(discounted)))),
            ("pending", Jv::u(u64::from(pending))),
            ("unknown", Jv::u(u64::from(unknown))),
            (
                "age_ms",
                Jv::u(age_ms.saturating_add(s.slots_age_ms.unwrap_or(0))),
            ),
        ]),
        Some(crate::model::SlotsView::Unread(why)) => Jv::why(why.word()),
        None => Jv::why("not_sampled"),
    };
    Jv::obj([
        ("as_of_ms", Jv::u(as_of_ms)),
        ("age_ms", Jv::u(age_ms)),
        (
            "cpu_busy_mc",
            cpu.and_then(|c| c.current_busy()).map_or_else(
                || Jv::why("no_counter_delta"),
                |v| Jv::u(u64::from(crate::events::millicores(v))),
            ),
        ),
        (
            "cpu_total",
            cpu.map_or_else(|| Jv::why("not_sampled"), |c| Jv::u(c.total() as u64)),
        ),
        (
            "available_bytes",
            s.mem
                .as_ref()
                .map_or_else(|| Jv::why("not_sampled"), |m| Jv::u(m.sample.available)),
        ),
        (
            "pressure",
            s.mem.as_ref().map_or_else(
                || Jv::why("not_sampled"),
                |m| Jv::s(m.sample.pressure.word()),
            ),
        ),
        ("slots", slots),
    ])
}

/// Human-readable counterpart of [`capacity_json`], with the machine snapshot's own age.
#[must_use]
pub fn capacity_text(s: &Snapshot, as_of_ms: u64, age_ms: u64) -> String {
    let cpu = s.cpu.as_ref().map_or_else(
        || "unavailable(not_sampled)".to_owned(),
        |c| {
            format!(
                "{}/{} cores busy",
                c.current_busy().map_or_else(
                    || "unavailable(no_counter_delta)".to_owned(),
                    crate::words::cores
                ),
                c.total()
            )
        },
    );
    let available = s.mem.as_ref().map_or_else(
        || "unavailable(not_sampled)".to_owned(),
        |m| crate::words::mem(m.sample.available),
    );
    let pressure = s
        .mem
        .as_ref()
        .map_or("unavailable(not_sampled)", |m| m.sample.pressure.word());
    let slots = match s.slots {
        Some(crate::model::SlotsView::Read {
            n,
            free,
            discounted,
            pending,
            unknown,
            ..
        }) => format!(
            "{}/{n} confirmed free, {pending} awaiting confirmation, {unknown} unknown, age_ms={} (paired slot observations; admission queue not measured)",
            free.saturating_sub(discounted),
            age_ms.saturating_add(s.slots_age_ms.unwrap_or(0))
        ),
        Some(crate::model::SlotsView::Unread(why)) => format!("unavailable({})", why.word()),
        None => "unavailable(not_sampled)".to_owned(),
    };
    format!(
        "machine as_of_ms={as_of_ms} age_ms={age_ms} cpu={cpu} · available={available} · pressure={pressure} · build_slots={slots}"
    )
}

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
        if let Some(condition) = verdict_summary(rows.iter().copied(), v) {
            parts.push(condition);
        }
    }
    let described = worst(rows);
    if let Some(w) = described {
        let phi = phi_text(w.phi_milli.unwrap_or(0));
        let label = if w.working_descendant.is_some() {
            "worst local"
        } else {
            "worst"
        };
        parts.push(match w.gap() {
            Some(gap) => format!("{label}: {gap}, φ {phi}"),
            None => format!("{label}: φ {phi}"),
        });
    }
    if let Some(w) = described.or_else(|| worst_live(rows))
        && let Some((pid, start)) = w.working_descendant
    {
        let local = if described.is_some() {
            String::new()
        } else {
            format!("local {}: ", w.verdict.word())
        };
        parts.push(format!("{local}working descendant pid={pid} birth={start}"));
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
        Some(r) => {
            let descendant = r
                .working_descendant
                .map_or_else(String::new, |(pid, start)| {
                    format!(" job_working_descendant={pid} job_working_descendant_start={start}")
                });
            format!(
                "job={} job_quiet_ms={}{descendant}",
                r.verdict.word(),
                r.quiet_ms
                    .map_or_else(|| "-".to_owned(), |q| q.saturating_add(age_ms).to_string())
            )
        }
        None => "job=none job_quiet_ms=-".to_owned(),
    }
}

/// `status`'s `program_check=` field: how many programs of the sessions `keys` macOS's program
/// check holds in `view` (members of their jobs, orphans of them included) and the oldest held
/// to now, `age_ms` being the view's age; `0` with none, `-` with no view. Then
/// `program_check_stranded=`, the same of those stranded ([`Findings::program_check_stranded`]):
/// `0` only when the view judged every held row ([`Findings::program_check_judged`]), and `-`
/// otherwise with none, since one may be stranded and nothing read tells.
#[must_use]
pub fn program_check_field(view: Option<&JobsView>, keys: &[u64], age_ms: u64) -> String {
    let Some(view) = view else {
        return "program_check=- program_check_stranded=-".to_owned();
    };
    let count = |list: &[crate::jobs::Named]| {
        let (n, oldest) = list
            .iter()
            .filter(|n| n.tab.is_some_and(|t| keys.contains(&t)))
            .fold((0usize, 0u64), |(n, oldest), h| {
                (n + 1, oldest.max(h.for_ms))
            });
        if n == 0 {
            "0".to_owned()
        } else {
            format!("{n}:{}", oldest.saturating_add(age_ms))
        }
    };
    let f = &view.findings;
    let stranded = match (f.program_check_daemon, count(&f.program_check_stranded)) {
        (Some(_), n) if n != "0" || f.program_check_judged => n,
        _ => "-".to_owned(),
    };
    format!(
        "program_check={} program_check_stranded={stranded}",
        count(&f.program_check)
    )
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
        program_check_after_grant: f.program_check_after_grant,
        program_check_stranded: f
            .program_check_stranded
            .iter()
            .filter(|n| mine(n.tab))
            .cloned()
            .collect(),
        program_check_daemon: f.program_check_daemon,
        program_check_judged: f.program_check_judged,
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
        stopped_unstarted: f
            .stopped_unstarted
            .iter()
            .filter(|s| mine(s.named.tab))
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
    items.extend(f.program_check_stranded.iter().map(|n| {
        Jv::obj([
            ("kind", Jv::s("program_check_stranded")),
            ("pid", Jv::u(u64::from(n.pid))),
            ("exe", Jv::s(exe_word(&n.name))),
            ("for_ms", Jv::u(n.for_ms)),
            (
                "daemon",
                Jv::known_or(f.program_check_daemon.map(u64::from), "not_sampled"),
            ),
        ])
    }));
    items.extend(f.translating.iter().map(|n| named("translating", n)));
    items.extend(f.not_started.iter().map(|n| named("not_started", n)));
    items.extend(f.stopped_unstarted.iter().map(|s| {
        Jv::obj([
            ("kind", Jv::s("stopped_unstarted")),
            ("pid", Jv::u(u64::from(s.named.pid))),
            ("exe", Jv::s(exe_word(&s.named.name))),
            ("stopped", Jv::Bool(s.stopped)),
            ("traced", Jv::Bool(s.traced)),
            ("for_ms", Jv::u(s.named.for_ms)),
        ])
    }));
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
#[path = "jobs_view_fixtures_tests.rs"]
pub(crate) mod fixtures;

#[cfg(test)]
#[path = "jobs_view_tests.rs"]
mod tests;
