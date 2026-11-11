// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::hardware::{Channel, State};

/// A3 L2 (D2): a heuristic thermal level (Linux's, from thermal-zone trips and, where the
/// CPUs have them, throttle counters) says at every level, in the design's words, that it is
/// an estimate from thermal-zone trips and that this computer reports no thermal state; it is
/// never called an OS thermal state, in `sys` or the CPU lens, which both print these lines.
/// NEGATIVE CONTROL: an exact level, the OS's own (macOS), keeps its OS thermal state line.
#[test]
fn a_heuristic_thermal_level_is_never_called_an_os_state() {
    use crate::model::{Thermal, Throttle};
    let said = |thermal: Thermal| {
        lines(&Snapshot {
            thermal: Some(thermal),
            ..Snapshot::default()
        })
    };
    for level in [
        Throttle::None,
        Throttle::Light,
        Throttle::Heavy,
        Throttle::Severe,
    ] {
        let heuristic = Thermal {
            level,
            raw: u8::try_from(level.rank()).unwrap_or(0),
            low_power: None,
            exact: false,
        };
        assert_eq!(
            said(heuristic),
            [format!(
                "thermal estimate: {} (from thermal-zone trips; this computer reports no \
                 thermal state)",
                level.word()
            )]
        );
    }
    let os = Thermal {
        level: Throttle::None,
        raw: 0,
        low_power: Some(false),
        exact: true,
    };
    assert_eq!(
        said(os),
        [
            "OS thermal state 0 (nominal); low power off; nominal thermal state does not exclude \
          another limit"
        ]
    );
}
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
        thermal: Some(crate::model::Thermal {
            exact: true,
            ..crate::model::Thermal::default()
        }),
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
