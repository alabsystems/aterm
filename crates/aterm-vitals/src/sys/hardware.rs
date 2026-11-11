// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CPU diagnostic report: raw residency, independent policy observations and
//! retained anomaly frames. No command lines, environment, or automatic action.

use super::json::Jv;
use crate::hardware::{Bundle, Reading, Sample};
use crate::model::Snapshot;

fn read<T>(r: &Reading<T>, value: impl FnOnce(&T) -> Jv) -> Jv {
    match r {
        Ok(v) => value(v),
        Err(why) => Jv::why(why),
    }
}
fn integer(r: &Reading<u64>) -> Jv {
    read(r, |v| Jv::u(*v))
}
fn milli(r: &Reading<f64>) -> Jv {
    read(r, |v| {
        if v.is_finite() && *v >= 0.0 {
            Jv::u((v * 1000.0).round() as u64)
        } else {
            Jv::why("unread_format:nonfinite")
        }
    })
}

fn topology_json(clusters: &[crate::model::ClusterView]) -> Jv {
    Jv::Arr(
        clusters
            .iter()
            .map(|c| {
                Jv::obj([
                    ("name", Jv::s(&c.name)),
                    ("kind", Jv::s(&c.short)),
                    (
                        "logical_cpus",
                        Jv::Arr(c.cores.iter().map(|i| Jv::u(*i as u64)).collect()),
                    ),
                ])
            })
            .collect(),
    )
}

/// Complete hardware observation, with its own independent interval.
#[must_use]
pub fn sample_json(s: &Sample) -> Jv {
    let controls = &s.controls;
    Jv::obj([
        ("source", Jv::s("native_ioreport_cpu_stats")),
        (
            "os_version",
            read(&controls.os_version, |s| Jv::s(s.clone())),
        ),
        (
            "kernel_release",
            read(&controls.kernel_release, |s| Jv::s(s.clone())),
        ),
        (
            "kernel_version",
            read(&controls.kernel_version, |s| Jv::s(s.clone())),
        ),
        (
            "captured_unix_ms",
            s.captured_unix_ms
                .map_or_else(|| Jv::why("not_sampled"), Jv::Int),
        ),
        ("collection_span_ns", Jv::u(s.collection_span_ns)),
        ("cause", Jv::s("not_established")),
        ("window_ns", Jv::u(s.window_ns)),
        ("observation_span_ns", Jv::u(s.observation_span_ns)),
        (
            "channels",
            read(&s.channels, |rows| {
                Jv::Arr(
                    rows.iter()
                        .map(|c| {
                            Jv::obj([
                                (
                                    "raw_elements_hex",
                                    Jv::s(
                                        c.raw_elements
                                            .iter()
                                            .map(|b| format!("{b:02x}"))
                                            .collect::<String>(),
                                    ),
                                ),
                                ("name", Jv::s(&c.name)),
                                ("subgroup", Jv::s(&c.subgroup)),
                                ("unit", Jv::s(&c.unit)),
                                ("active_pm", milli(&c.active)),
                                ("active_khz", milli(&c.active_mhz)),
                                (
                                    "frequency_quality",
                                    c.active_mhz
                                        .as_ref()
                                        .map_or_else(|why| Jv::why(why), |_| Jv::s("estimate")),
                                ),
                                ("frequency_source", Jv::s(&c.frequency_source)),
                                (
                                    "states",
                                    Jv::Arr(
                                        c.states
                                            .iter()
                                            .map(|s| {
                                                Jv::obj([
                                                    ("name", Jv::s(&s.name)),
                                                    ("ticks", Jv::Int(s.ticks)),
                                                ])
                                            })
                                            .collect(),
                                    ),
                                ),
                            ])
                        })
                        .collect(),
                )
            }),
        ),
        ("active_cpus", integer(&controls.active_cpus)),
        (
            "online",
            read(&controls.online, |ids| {
                Jv::Arr(ids.iter().map(|i| Jv::u(u64::from(*i))).collect())
            }),
        ),
        (
            "requested_recommended_mask",
            read(&controls.requested_recommended_mask, |v| {
                Jv::s(format!("0x{v:016x}"))
            }),
        ),
        (
            "effective_recommended_mask",
            Jv::why("not_sampled:effective_mask"),
        ),
        (
            "clpc_cluster_masks",
            read(&controls.cluster_masks, |masks| {
                Jv::Arr(
                    masks
                        .iter()
                        .map(|(i, v)| {
                            Jv::obj([
                                ("cluster", Jv::u(u64::from(*i))),
                                ("mask", Jv::s(format!("0x{v:016x}"))),
                            ])
                        })
                        .collect(),
                )
            }),
        ),
        (
            "power_limits",
            read(&controls.power_limits, |p| {
                Jv::obj([
                    ("source", Jv::s("IOPMCopyCPUPowerStatus")),
                    ("speed_percent", integer(&p.speed_percent)),
                    ("scheduler_percent", integer(&p.scheduler_percent)),
                    ("available_cpus", integer(&p.available_cpus)),
                ])
            }),
        ),
        (
            "last_sleep_ms",
            read(&controls.last_sleep_ms, |v| Jv::Int(*v)),
        ),
        (
            "last_wake_ms",
            read(&controls.last_wake_ms, |v| Jv::Int(*v)),
        ),
        (
            "historical_sleep_reason",
            read(&controls.last_sleep_reason, |v| Jv::s(v.clone())),
        ),
    ])
}

/// Preserved evidence suitable for JSON export through the existing sys interface.
#[must_use]
pub fn bundle_json(b: &Bundle) -> Jv {
    Jv::obj([
        ("version", Jv::u(u64::from(b.version))),
        ("cause", Jv::s("not_established")),
        (
            "observation",
            Jv::s("sustained_efficiency_busy_performance_underused"),
        ),
        (
            "thresholds",
            Jv::obj([
                (
                    "max_mean_performance_busy_pm",
                    Jv::u((crate::scheduling::PERFORMANCE_BUSY_MAX * 1000.0).round() as u64),
                ),
                (
                    "min_each_efficiency_busy_pm",
                    Jv::u((crate::hardware::EFFICIENCY_BUSY_MIN * 1000.0).round() as u64),
                ),
                ("min_runnable_wait_mc", Jv::u(1000)),
                ("continuous_ns", Jv::u(crate::hardware::SUSTAIN_NS)),
            ]),
        ),
        ("since_ns", Jv::u(b.since_ns)),
        ("last_ns", Jv::u(b.last_ns)),
        ("resolved_ns", Jv::known_or(b.resolved_ns, "not_sampled")),
        (
            "frames",
            Jv::Arr(
                b.frames
                    .iter()
                    .map(|f| {
                        Jv::obj([
                            ("kernel_topology", topology_json(&f.clusters)),
                            (
                                "kernel_window_ns",
                                Jv::known_or(f.kernel_window_ns, "not_sampled"),
                            ),
                            (
                                "kernel_busy_pm",
                                Jv::Arr(
                                    f.busy
                                        .iter()
                                        .map(|v| {
                                            Jv::known_or(
                                                v.map(|v| (v * 1000.0).round() as u64),
                                                "no_counter_delta",
                                            )
                                        })
                                        .collect(),
                                ),
                            ),
                            ("at_ns", Jv::u(f.at_ns)),
                            ("at_unix_ms", Jv::Int(f.at_unix_ms)),
                            (
                                "kernel_counters",
                                Jv::Arr(
                                    f.counters
                                        .iter()
                                        .map(|t| {
                                            Jv::obj([
                                                ("busy_ticks", Jv::s(t.busy.to_string())),
                                                ("total_ticks", Jv::s(t.total.to_string())),
                                            ])
                                        })
                                        .collect(),
                                ),
                            ),
                            (
                                "hardware",
                                f.hardware
                                    .as_ref()
                                    .map_or_else(|| Jv::why("not_sampled"), sample_json),
                            ),
                            (
                                "thermal",
                                f.thermal.map_or_else(
                                    || Jv::why("not_sampled"),
                                    super::view::thermal_json,
                                ),
                            ),
                            (
                                "workers",
                                Jv::Arr(
                                    f.workers
                                        .iter()
                                        .map(|worker| {
                                            Jv::obj([
                                                ("pid", Jv::u(u64::from(worker.pid))),
                                                (
                                                    "birth_ms",
                                                    worker.birth_ms.map_or_else(
                                                        || Jv::why("not_sampled"),
                                                        Jv::Int,
                                                    ),
                                                ),
                                                (
                                                    "scheduling",
                                                    super::view::scheduling_json(
                                                        worker.scheduling,
                                                        "macOS",
                                                    ),
                                                ),
                                                (
                                                    "cpu_accounting",
                                                    super::view::cpu_accounting_json(
                                                        &worker.cpu_accounting,
                                                        "macOS",
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
    ])
}

/// One bounded export, without process command lines or environment data.
#[must_use]
pub fn report_json(s: &Snapshot) -> Jv {
    let mut report = current_report_json(s);
    report.set(
        "prior_process_capture",
        s.historical_cpu_report
            .as_deref()
            .cloned()
            .unwrap_or_else(|| Jv::why("not_sampled")),
    );
    report
}

/// This process's observations only, suitable for saving without nesting older reports.
#[must_use]
pub fn current_report_json(s: &Snapshot) -> Jv {
    Jv::obj([
        ("version", Jv::u(1)),
        ("at_unix_ms", Jv::Int(s.at_unix_ms)),
        (
            "authorized_cpu_policy",
            s.authorized_cpu_policy
                .as_deref()
                .cloned()
                .unwrap_or_else(|| Jv::why("not_sampled")),
        ),
        ("cause", Jv::s("not_established")),
        (
            "chip",
            s.facts
                .cpu_brand
                .as_ref()
                .map_or_else(|| Jv::why("not_sampled"), |v| Jv::s(v.clone())),
        ),
        (
            "kernel_topology",
            s.cpu.as_ref().map_or_else(
                || Jv::why("not_sampled"),
                |cpu| topology_json(&cpu.clusters),
            ),
        ),
        (
            "workers",
            Jv::Arr(
                s.procs
                    .iter()
                    .take(64)
                    .map(|p| {
                        Jv::obj([
                            ("pid", Jv::u(u64::from(p.pid))),
                            (
                                "birth_ms",
                                p.birth_ms.map_or_else(|| Jv::why("not_sampled"), Jv::Int),
                            ),
                            (
                                "scheduling",
                                super::view::scheduling_json(p.scheduling, "macOS"),
                            ),
                            (
                                "cpu_accounting",
                                super::view::cpu_accounting_json(&p.cpu_accounting, "macOS"),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("workers_truncated", Jv::Bool(s.procs.len() > 64)),
        (
            "kernel_window_ns",
            Jv::known_or(s.cores_window_ns, "not_sampled"),
        ),
        (
            "process_window_ns",
            Jv::known_or(s.procs_window_ns, "not_sampled"),
        ),
        (
            "kernel_counters",
            s.cpu_counter_sample.as_ref().map_or_else(
                || Jv::why("not_sampled"),
                |rows| {
                    Jv::Arr(
                        rows.iter()
                            .map(|t| {
                                Jv::obj([
                                    ("busy_ticks", Jv::s(t.busy.to_string())),
                                    ("total_ticks", Jv::s(t.total.to_string())),
                                ])
                            })
                            .collect(),
                    )
                },
            ),
        ),
        (
            "hardware",
            s.hardware
                .as_ref()
                .map_or_else(|| Jv::why("not_sampled"), sample_json),
        ),
        (
            "anomaly",
            s.cpu_diagnostics
                .as_ref()
                .map_or_else(|| Jv::why("not_sampled"), bundle_json),
        ),
    ])
}

/// The reason every reading of a CPU Stats sample gives on a platform with no source for it
/// (`aterm_sysprobe::cpu_hardware`'s arm off macOS; macOS's own reasons name a cause after a
/// colon, `unsupported:ioreport_library`): the clauses those sources feed are absent there, not
/// unavailable.
pub const NO_SOURCE: &str = "unsupported";

/// Whether `r`'s source exists on this machine: every reading but [`NO_SOURCE`].
fn here<T>(r: &crate::hardware::Reading<T>) -> bool {
    !matches!(r, Err(why) if why == NO_SOURCE)
}

/// Report clauses shared by the control interface and the resource panel. The CPU Stats
/// clauses (the IOReport window, online CPUs and the CLPC mask, power limits, channels, and the
/// pointer to inspect and resample them) need a sample, which a snapshot holds only where a
/// lens, the recorder or a pull took one (A2 §6.4); the thermal state, a prior process's report,
/// saved underuse and the policy collection do not. Each CPU Stats clause prints only where its
/// source exists on this machine ([`NO_SOURCE`]), and the pointer only beside one of them. An OS
/// thermal level says it is the OS's; a heuristic one says it is an estimate, never an OS state.
#[must_use]
pub fn lines(s: &Snapshot) -> Vec<String> {
    let mut policy = Vec::new();
    if let Some(report) = s.authorized_cpu_policy.as_deref() {
        let state = report
            .get("state")
            .and_then(|v| {
                if let Jv::Str(v) = v {
                    Some(v.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("unread_format");
        policy.push(format!("Authorized CPU policy collection: {state}; see exported incident for per-selector observations/errors and exact window"));
        policy.push("A zero-hit census means no active flags observed across the checked processes during that window; it is not an atomic global all-clear.".into());
    }
    let h = s.hardware.as_ref();
    let mut out = Vec::new();
    let mut sourced = false;
    if let Some(h) = h.filter(|h| here(&h.channels)) {
        sourced = true;
        out.push(format!(
            "CPU hardware: native IOReport, {} ms window; cause not established",
            h.window_ns / 1_000_000
        ));
    }
    if s.historical_cpu_report.is_some() {
        out.push("A prior process's anomaly report is retained for export; it is historical, not a current diagnosis".into());
    }
    out.extend(policy);
    if let Some(controls) = h
        .map(|h| &h.controls)
        .filter(|c| here(&c.active_cpus) || here(&c.requested_recommended_mask))
    {
        sourced = true;
        out.push(format!(
            "Online CPUs: {}; requested CLPC mask: {}; effective mask unavailable",
            controls
                .active_cpus
                .as_ref()
                .map_or_else(|e| format!("unavailable ({e})"), ToString::to_string),
            controls
                .requested_recommended_mask
                .as_ref()
                .map_or_else(|e| format!("unavailable ({e})"), |v| format!("0x{v:x}"))
        ));
    }
    match s.thermal {
        Some(t) if t.exact => out.push(format!("OS thermal state {} ({}); low power {}; nominal thermal state does not exclude another limit",
            t.raw, t.word(), t.low_power.map_or("unavailable", |v| if v { "on" } else { "off" }))),
        Some(t) => out.push(format!(
            "thermal estimate: {} (from thermal-zone trips; this computer reports no thermal state)",
            t.word()
        )),
        None => {}
    }
    if let Some(h) = h {
        if here(&h.controls.power_limits) {
            sourced = true;
            out.push(match &h.controls.power_limits {
                Err(e) => {
                    format!("Public CPU power limits unavailable ({e}); no unrestricted claim")
                }
                Ok(p) => format!(
                    "Public CPU speed/scheduler limits: {:?} / {:?}; other limits may exist",
                    p.speed_percent, p.scheduler_percent
                ),
            });
        }
        match &h.channels {
            Err(why) if why == NO_SOURCE => {}
            Err(why) => out.push(format!("CPU frequency/residency unavailable ({why})")),
            Ok(channels) => {
                for c in channels {
                    let active = c.active.as_ref().map_or_else(
                        |e| format!("unavailable ({e})"),
                        |v| format!("{:.1}% active", v * 100.0),
                    );
                    let frequency = c.active_mhz.as_ref().map_or_else(
                        |e| format!("frequency unavailable ({e})"),
                        |v| format!("estimated {v:.0} MHz while active"),
                    );
                    out.push(format!("{}: {active}, {frequency}", c.name));
                }
            }
        }
    }
    if let Some(b) = &s.cpu_diagnostics {
        out.push(format!(
            "Saved CPU underuse: {} frames (P mean <=25%, every E >=90%, wait >=1 core, >=30s); cause not established",
            b.frames.len()
        ));
    }
    if sourced {
        out.push("Inspect/export: aterm ctl @self sys cores --json. Resample: aterm ctl @self sys cores fresh=0 --json. Administrator frequency/thermal trace may be needed; no reboot or policy change is automatic.".into());
    }
    out
}

#[cfg(test)]
#[path = "hardware_tests.rs"]
mod tests;
