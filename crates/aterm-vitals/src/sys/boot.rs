// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Saved boot observations: actual continuous-time cuts and independent accounting channels.

use super::json::Jv;
use super::view::{Line, line, note};
use crate::boot_cost::{self, Coverage, Observation, RawCounters};

/// Saved boot observations and the native owner's current persistence state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BootReport {
    /// Bounded observations, including prior boots.
    pub book: boot_cost::Book,
    /// This machine's current stable boot identity, where observed.
    pub boot_id: Option<String>,
    /// Native persistence state, independent of collection coverage.
    pub persistence: &'static str,
    /// A restore or save transaction is running.
    pub busy: bool,
    /// Captured observations still await durable persistence.
    pub dirty: bool,
    /// The native owner's bounded failure description, where present.
    pub failure: Option<String>,
}

fn decimal(n: u64) -> Jv {
    Jv::s(n.to_string())
}

fn optional_decimal(n: Option<u64>, why: &str) -> Jv {
    n.map_or_else(|| Jv::why(why), decimal)
}

fn coverage_json(c: Coverage) -> Jv {
    Jv::Arr(c.words().into_iter().map(Jv::s).collect())
}

fn counters_json(c: RawCounters) -> Jv {
    Jv::obj([
        (
            "own_ns_decimal",
            optional_decimal(c.own_ns, "not_sampled:own_counter"),
        ),
        (
            "inherited_ns_decimal",
            optional_decimal(c.inherited_ns, "not_sampled:inherited_counter"),
        ),
    ])
}

fn observation_json(index: usize, o: &Observation) -> Jv {
    let process = o.process.map_or_else(
        || Jv::why("not_sampled:process_identity"),
        |p| {
            Jv::obj([
                ("pid", Jv::Int(i64::from(p.pid))),
                ("start_decimal", decimal(p.start)),
                (
                    "version",
                    p.version.map_or_else(
                        || Jv::why("not_sampled:process_version"),
                        |v| Jv::Int(i64::from(v)),
                    ),
                ),
            ])
        },
    );
    let wakes = o.wakes.as_ref().map_or_else(
        || Jv::why("not_sampled:delivered_ui_wakes"),
        |w| {
            Jv::obj([
                ("process", process.clone()),
                ("epoch_decimal", decimal(w.epoch)),
                (
                    "variants",
                    Jv::Arr(
                        w.variants
                            .iter()
                            .map(|v| {
                                Jv::obj([
                                    ("kind", Jv::s(&v.kind)),
                                    ("count_decimal", decimal(v.count)),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        },
    );
    Jv::obj([
        ("index", Jv::u(index as u64)),
        ("at_ns_decimal", decimal(o.at_ns)),
        (
            "before_ns_decimal",
            optional_decimal(o.before_ns, "not_sampled:read_bracket"),
        ),
        (
            "after_ns_decimal",
            optional_decimal(o.after_ns, "not_sampled:read_bracket"),
        ),
        ("process", process),
        ("counter_epoch_decimal", decimal(o.counter_epoch)),
        ("process_cpu", counters_json(o.process_cpu)),
        (
            "coalition_id_decimal",
            optional_decimal(o.coalition_id, "not_sampled:coalition_identity"),
        ),
        ("coalition_cpu", counters_json(o.coalition_cpu)),
        (
            "historical_reported",
            Jv::obj([
                ("own_ns_decimal", decimal(o.historical_reported.own_ns)),
                ("tabs_ns_decimal", decimal(o.historical_reported.tabs_ns)),
                ("incomplete", Jv::Bool(o.historical_reported.incomplete)),
            ]),
        ),
        ("delivered_ui_wakes", wakes),
        ("coverage", coverage_json(o.coverage)),
    ])
}

/// Complete bounded report. Unsigned raw values use decimal strings without losing precision.
/// Wake epochs are local to the process identity carried by their observation.
#[must_use]
pub fn json(report: Option<&BootReport>) -> Jv {
    let Some(report) = report else {
        return Jv::why("not_sampled:boot_cost");
    };
    let mut out = Jv::obj([
        ("accounting_basis", Jv::s("published_counters")),
        (
            "execution_window",
            Jv::why("unavailable:execution_window_unknown"),
        ),
        ("clock", Jv::s("continuous_since_boot_including_sleep")),
        ("cpu_channels", Jv::s("independent_do_not_sum")),
        ("wake_scope", Jv::s("delivered_ui_proxy_events")),
        (
            "current_boot_id",
            report
                .boot_id
                .as_deref()
                .map_or_else(|| Jv::why("not_sampled:boot_identity"), Jv::s),
        ),
        ("persistence", Jv::s(report.persistence)),
        ("busy", Jv::Bool(report.busy)),
        ("dirty", Jv::Bool(report.dirty)),
        (
            "boots",
            Jv::Arr(
                report
                    .book
                    .boots
                    .iter()
                    .map(|b| {
                        Jv::obj([
                            ("boot_id", Jv::s(&b.boot_id)),
                            (
                                "current",
                                Jv::Bool(report.boot_id.as_deref() == Some(b.boot_id.as_str())),
                            ),
                            ("first_seen_ns_decimal", decimal(b.first_seen_ns)),
                            ("coverage", coverage_json(b.coverage)),
                            (
                                "observations",
                                Jv::Arr(
                                    b.observations
                                        .iter()
                                        .enumerate()
                                        .map(|(index, o)| observation_json(index, o))
                                        .collect(),
                                ),
                            ),
                            (
                                "targets",
                                Jv::Arr(
                                    boot_cost::TARGETS_NS
                                        .iter()
                                        .enumerate()
                                        .map(|(index, target)| {
                                            Jv::obj([
                                                ("target_ns_decimal", decimal(*target)),
                                                (
                                                    "observation_index",
                                                    b.targets[index].map_or_else(
                                                        || Jv::why("not_sampled:target"),
                                                        |slot| Jv::Int(i64::from(slot)),
                                                    ),
                                                ),
                                            ])
                                        })
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]);
    if let Some(failure) = &report.failure {
        out.set("failure", Jv::s(failure));
    }
    out
}

fn number(n: Option<u64>) -> String {
    n.map_or_else(|| "unavailable(not_sampled)".to_owned(), |n| n.to_string())
}

fn coverage_text(c: Coverage) -> String {
    let words = c.words();
    if words.is_empty() {
        "none".to_owned()
    } else {
        words.join(",")
    }
}

/// Human report: each observation appears once; targets refer to its index. Only wake rows
/// are shortened by `top`; the JSON report retains every bounded variant.
#[must_use]
pub fn text(report: Option<&BootReport>, top: usize) -> Vec<Line> {
    let Some(report) = report else {
        return vec![note("boot", "boot unavailable(not_sampled:boot_cost)")];
    };
    let mut status = format!(
        "boot current={} persistence={} busy={} dirty={}",
        report.boot_id.as_deref().map_or_else(
            || "unavailable(not_sampled:boot_identity)".to_owned(),
            super::pct
        ),
        super::pct(report.persistence),
        u8::from(report.busy),
        u8::from(report.dirty)
    );
    if let Some(failure) = &report.failure {
        status.push_str(&format!(" failure={}", super::pct(failure)));
    }
    let mut out = vec![
        line("boot", status),
        note(
            "boot-accounting",
            "boot_accounting basis=published_counters cpu_channels=independent_do_not_sum execution_window=unavailable:execution_window_unknown clock=continuous_since_boot_including_sleep wakes=delivered_ui_proxy_events",
        ),
    ];
    for (boot, b) in report.book.boots.iter().enumerate() {
        out.push(line(
            format!("boot {boot}"),
            format!(
                "boot_record boot={boot} boot_id={} current={} first_seen_ns={} coverage={}",
                super::pct(&b.boot_id),
                u8::from(report.boot_id.as_deref() == Some(b.boot_id.as_str())),
                b.first_seen_ns,
                coverage_text(b.coverage)
            ),
        ));
        for (index, o) in b.observations.iter().enumerate() {
            let process = o.process.map_or_else(
                || "unavailable(not_sampled:process_identity)".to_owned(),
                |p| {
                    format!(
                        "{}:{}:{}",
                        p.pid,
                        p.start,
                        p.version
                            .map_or_else(|| "unavailable".to_owned(), |v| v.to_string())
                    )
                },
            );
            out.push(line(format!("boot {boot} observation {index}"), format!(
                "boot_observation boot={boot} index={index} at_ns={} before_ns={} after_ns={} process={} counter_epoch={} process_own_ns={} process_inherited_ns={} coalition_id={} coalition_own_ns={} coalition_inherited_ns={} historical_own_ns={} historical_tabs_ns={} historical_incomplete={} coverage={}",
                o.at_ns, number(o.before_ns), number(o.after_ns), process, o.counter_epoch,
                number(o.process_cpu.own_ns), number(o.process_cpu.inherited_ns), number(o.coalition_id),
                number(o.coalition_cpu.own_ns), number(o.coalition_cpu.inherited_ns),
                o.historical_reported.own_ns, o.historical_reported.tabs_ns, u8::from(o.historical_reported.incomplete), coverage_text(o.coverage)
            )));
            if let Some(wakes) = &o.wakes {
                let mut variants: Vec<_> = wakes.variants.iter().collect();
                variants.sort_by(|a, b| b.count.cmp(&a.count).then(a.kind.cmp(&b.kind)));
                for variant in variants.iter().take(top) {
                    out.push(line(format!("boot {boot} observation {index} wake {}", variant.kind), format!(
                        "boot_ui_wake boot={boot} observation={index} process={process} wake_epoch={} kind={} count={}",
                        wakes.epoch, super::pct(&variant.kind), variant.count
                    )));
                }
                if variants.len() > top {
                    out.push(note(
                        &format!("boot {boot} observation {index} wakes more"),
                        format!(
                            "boot_ui_wakes_more boot={boot} observation={index} more={}",
                            variants.len() - top
                        ),
                    ));
                }
            } else {
                out.push(note(
                    &format!("boot {boot} observation {index} wakes unavailable"),
                    format!(
                        "boot_ui_wakes boot={boot} observation={index} unavailable(not_sampled)"
                    ),
                ));
            }
        }
        for (target, ns) in boot_cost::TARGETS_NS.iter().enumerate() {
            out.push(line(
                format!("boot {boot} target {target}"),
                format!(
                    "boot_target boot={boot} target_ns={ns} observation={}",
                    b.targets[target].map_or_else(
                        || "unavailable(not_sampled:target)".to_owned(),
                        |index| index.to_string()
                    )
                ),
            ));
        }
    }
    out
}

#[cfg(test)]
#[path = "boot_tests.rs"]
mod tests;
