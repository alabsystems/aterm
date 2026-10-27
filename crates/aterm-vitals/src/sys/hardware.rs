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
                                    |t| {
                                        Jv::obj([
                                            ("raw", Jv::u(u64::from(t.raw))),
                                            ("level", Jv::s(t.level.word())),
                                            (
                                                "low_power",
                                                t.low_power.map_or_else(
                                                    || Jv::why("not_sampled"),
                                                    Jv::Bool,
                                                ),
                                            ),
                                        ])
                                    },
                                ),
                            ),
                            (
                                "workers",
                                Jv::Arr(
                                    f.workers
                                        .iter()
                                        .map(|(pid, birth, policy)| {
                                            Jv::obj([
                                                ("pid", Jv::u(u64::from(*pid))),
                                                (
                                                    "birth_ms",
                                                    birth.map_or_else(
                                                        || Jv::why("not_sampled"),
                                                        Jv::Int,
                                                    ),
                                                ),
                                                (
                                                    "scheduling",
                                                    super::view::scheduling_json(*policy, "macOS"),
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

/// Report clauses shared by the control interface and the resource panel.
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
    let Some(h) = &s.hardware else {
        return policy;
    };
    let mut out = vec![format!(
        "CPU hardware: native IOReport, {} ms window; cause not established",
        h.window_ns / 1_000_000
    )];
    if s.historical_cpu_report.is_some() {
        out.push("A prior process's anomaly report is retained for export; it is historical, not a current diagnosis".into());
    }
    out.extend(policy);
    let mut channels_text = Vec::new();
    match &h.channels {
        Err(why) => channels_text.push(format!("CPU frequency/residency unavailable ({why})")),
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
                channels_text.push(format!("{}: {active}, {frequency}", c.name));
            }
        }
    }
    let controls = &h.controls;
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
    if let Some(t) = s.thermal {
        out.push(format!("OS thermal state {} ({}); low power {}; nominal thermal state does not exclude another limit",
            t.raw, t.level.word(), t.low_power.map_or("unavailable", |v| if v { "on" } else { "off" })));
    }
    out.push(match &controls.power_limits {
        Err(e) => format!("Public CPU power limits unavailable ({e}); no unrestricted claim"),
        Ok(p) => format!(
            "Public CPU speed/scheduler limits: {:?} / {:?}; other limits may exist",
            p.speed_percent, p.scheduler_percent
        ),
    });
    out.extend(channels_text);
    if let Some(b) = &s.cpu_diagnostics {
        out.push(format!(
            "Saved CPU underuse: {} frames (P mean <=25%, every E >=90%, wait >=1 core, >=30s); cause not established",
            b.frames.len()
        ));
    }
    out.push("Inspect/export: aterm ctl @self sys cores --json. Resample: aterm ctl @self sys cores fresh=0 --json. Administrator frequency/thermal trace may be needed; no reboot or policy change is automatic.".into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{Channel, State};
    #[test]
    fn cpu_hardware_export_keeps_raw_bytes_and_unavailable_limits_distinct() {
        let mut h = Sample::unavailable("refused:ioreport_subscription");
        h.controls.os_version = Ok("macOS 27.0".into());
        h.controls.kernel_release = Ok("27.0.0".into());
        h.controls.kernel_version = Ok("Darwin test build".into());
        h.window_ns = 1_234_567_890;
        h.channels = Ok(vec![Channel {
            raw_elements: vec![0, 0xff, 0x80, 0x01],
            name: "DIE_0_ECPU_CPU0".into(),
            subgroup: "CPU Core Performance States".into(),
            unit: "ticks".into(),
            states: vec![
                State {
                    name: "IDLE".into(),
                    ticks: 0,
                },
                State {
                    name: "V0P0".into(),
                    ticks: 999,
                },
            ],
            active: Ok(1.0),
            active_mhz: Ok(1020.0),
            frequency_source: "pmgr_voltage_states:ordinal".into(),
        }]);
        let s = Snapshot {
            hardware: Some(h),
            facts: crate::model::RowFacts {
                cpu_brand: Some("Apple M3 Ultra".into()),
                ..Default::default()
            },
            cpu: Some(crate::model::CpuView {
                cores: vec![crate::model::CoreView::default(); 2],
                clusters: vec![
                    crate::model::ClusterView {
                        name: "Performance".into(),
                        short: "P".into(),
                        cores: vec![0],
                    },
                    crate::model::ClusterView {
                        name: "Efficiency".into(),
                        short: "E".into(),
                        cores: vec![1],
                    },
                ],
                ..crate::model::CpuView::default()
            }),
            thermal: Some(crate::model::Thermal::default()),
            cpu_counter_sample: Some(vec![
                crate::model::Ticks {
                    busy: u64::MAX,
                    total: u64::MAX,
                },
                crate::model::Ticks { busy: 0, total: 1 },
            ]),
            ..Snapshot::default()
        };
        let report = report_json(&s);
        assert_eq!(report.get("chip"), Some(&Jv::s("Apple M3 Ultra")));
        let Jv::Arr(topology) = report.get("kernel_topology").unwrap() else {
            panic!("topology");
        };
        assert_eq!(
            topology[0].get("logical_cpus"),
            Some(&Jv::Arr(vec![Jv::u(0)]))
        );
        assert_eq!(topology[1].get("kind"), Some(&Jv::s("E")));
        assert!(super::super::json::contract_violations(&report).is_empty());
        let text = report.to_text();
        assert!(text.contains("00ff8001"));
        assert!(
            text.contains("18446744073709551615"),
            "raw u64 counters are not saturated to i64"
        );
        let hardware = report.get("hardware").unwrap();
        assert_eq!(
            hardware.get("kernel_version"),
            Some(&Jv::s("Darwin test build"))
        );
        assert_eq!(hardware.get("window_ns"), Some(&Jv::u(1_234_567_890)));
        assert_eq!(
            hardware.get("power_limits"),
            Some(&Jv::why("refused:ioreport_subscription"))
        );
        assert!(
            lines(&s)
                .iter()
                .any(|s| s.contains("nominal thermal state does not exclude another limit"))
        );
    }
}
