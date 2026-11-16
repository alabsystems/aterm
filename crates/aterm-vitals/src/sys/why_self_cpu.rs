// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Published CPU counter growth, with read times kept separate from accounting epochs.

use super::{Jv, Line, line};
use crate::self_cpu::{OwnCpuCoverage, OwnCpuGrowth, OwnCpuView};

fn number(value: u64) -> Jv {
    i64::try_from(value)
        .map(Jv::Int)
        .unwrap_or_else(|_| Jv::obj([("decimal", Jv::s(value.to_string()))]))
}

fn unknown(reason: &str) -> Jv {
    let qualified = format!("unavailable:{reason}");
    Jv::why(if crate::sys::is_unavailable_word(reason) {
        reason
    } else if crate::sys::is_unavailable_word(&qualified) {
        &qualified
    } else {
        "unavailable:self_cpu_read"
    })
}

fn coverage(c: &OwnCpuCoverage) -> Jv {
    Jv::obj([
        ("listed", Jv::u(c.listed as u64)),
        ("inspected", Jv::u(c.inspected as u64)),
        ("unread", Jv::u(c.unread as u64)),
        ("capped", Jv::Bool(c.capped)),
    ])
}

fn growth(growth: &OwnCpuGrowth) -> Jv {
    Jv::obj([
        ("lower", number(growth.lower_ns)),
        ("upper", number(growth.upper_ns)),
    ])
}

pub(super) fn text(view: &OwnCpuView) -> Option<Line> {
    let words = match view {
        OwnCpuView::Unavailable("not_sampled") => return None,
        OwnCpuView::Unavailable(reason) => format!("self CPU threads: unavailable ({reason})"),
        OwnCpuView::Warming => "self CPU threads: warming; one valid reading".into(),
        OwnCpuView::Observed(changes) => {
            let observed = changes.groups.first().map_or_else(
                || "no paired named counters".into(),
                |group| {
                    format!(
                        "{:?} {}..{} ns",
                        group.name, group.growth.lower_ns, group.growth.upper_ns
                    )
                },
            );
            format!(
                "self CPU published counter growth (updates deferred): {observed}; process {}..{} ns; names observed at both reads, window shares unavailable",
                changes.process_growth.lower_ns, changes.process_growth.upper_ns
            )
        }
    };
    Some(line("self_cpu", words))
}

pub(super) fn json(view: &OwnCpuView) -> Option<Jv> {
    let changes = match view {
        OwnCpuView::Unavailable("not_sampled") => return None,
        OwnCpuView::Unavailable(reason) => return Some(unknown(reason)),
        OwnCpuView::Warming => return Some(Jv::why("warming")),
        OwnCpuView::Observed(changes) => changes,
    };
    Some(Jv::obj([
        ("scope", Jv::s("self_process")),
        ("accounting", Jv::s("detail_only")),
        ("counter_basis", Jv::s("published_cumulative_cpu")),
        ("cpu_window_attribution", unknown("deferred_accounting")),
        ("name_basis", Jv::s("observed_at_both_cuts")),
        ("pid", Jv::u(u64::from(changes.process.pid))),
        ("process_start_counter", number(changes.process.start)),
        ("version", Jv::Int(i64::from(changes.process.version))),
        (
            "read_times_ns",
            Jv::obj([
                ("before_begin", number(changes.before_begin_ns)),
                ("before_end", number(changes.before_end_ns)),
                ("after_begin", number(changes.after_begin_ns)),
                ("after_end", number(changes.after_end_ns)),
            ]),
        ),
        ("process_counter_growth_ns", growth(&changes.process_growth)),
        (
            "names",
            Jv::Arr(
                changes
                    .groups
                    .iter()
                    .take(3)
                    .map(|group| {
                        Jv::obj([
                            ("observed_name", Jv::s(&group.name)),
                            ("counter_growth_ns", growth(&group.growth)),
                            ("threads", Jv::u(group.threads as u64)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "names_omitted",
            Jv::u(changes.groups.len().saturating_sub(3) as u64),
        ),
        (
            "coverage",
            Jv::obj([
                ("before", coverage(&changes.coverage.before)),
                ("after", coverage(&changes.coverage.after)),
                ("unpaired", Jv::u(changes.coverage.unpaired as u64)),
                ("unread", Jv::u(changes.coverage.unread as u64)),
                ("unnamed", Jv::u(changes.coverage.unnamed as u64)),
                ("name_changes", Jv::u(changes.coverage.name_changes as u64)),
            ]),
        ),
    ]))
}

#[cfg(test)]
#[path = "why_self_cpu_tests.rs"]
mod tests;
