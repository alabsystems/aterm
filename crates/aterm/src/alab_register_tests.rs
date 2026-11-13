// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// What a shipped build logs: only a pass the owner would want to hear of passes its filter.
/// NEGATIVE CONTROL: with `speaks` always false, the first assertion fails.
#[test]
fn a_held_pass_and_an_error_reach_a_shipped_builds_log() {
    let pass = |outcome, findings| {
        Ok(Registered {
            outcome,
            findings,
            records: 0,
        })
    };
    assert!(speaks(&pass(Outcome::Held, Vec::new())));
    assert!(speaks(&pass(Outcome::QuotaSpent, Vec::new())));
    let refused = aterm_db::alab::register::model_key("no such model!").map(|_| Registered {
        outcome: Outcome::Unchanged,
        findings: Vec::new(),
        records: 0,
    });
    assert!(speaks(&refused));
    assert!(speaks(&pass(
        Outcome::Born { mac: "mc-1".into() },
        vec![aterm_db::alab::checks::Finding {
            kind: "hw-hint-live",
            collection: "machine-facts",
            pks: Vec::new(),
        }],
    )));
    assert!(!speaks(&pass(Outcome::Unchanged, Vec::new())));
    assert!(!speaks(&pass(Outcome::Deferred, Vec::new())));
}

/// This Mac's real readings make a row the schema takes, and the IOPlatformUUID stays out of
/// it. It only reads; the plan is computed and thrown away.
/// NEGATIVE CONTROL: with `probe` reading `hw.model` as `no such model!`, `plan` refuses it.
#[cfg(target_os = "macos")]
#[test]
fn this_macs_real_readings_make_a_row() {
    use aterm_db::alab::register::{Input, Me, plan};
    let read = probe().expect("this Mac states its model and its hardware");
    let me = Me {
        uuid: "11111111111111111111111111111111".into(),
        label: Some("m7".into()),
        chosen: None,
        hw_hint: "aaaaaaaaaaaaaaaa".into(),
        enrolled_ms: None,
    };
    let live = std::collections::BTreeSet::new();
    let made = plan(
        &Input {
            me: &me,
            probe: &read,
            live: &live,
            heard: true,
            now_ms: 400 * 86_400_000,
        },
        &Default::default(),
        &Default::default(),
    )
    .expect("every reading fits its column or keeps out of the row");
    assert!(matches!(made.outcome, Outcome::Born { .. }));
    assert!(made.changes.iter().all(|change| {
        !aterm_json::to_string(&change.value)
            .unwrap()
            .contains(&read.platform_uuid)
    }));
}

/// The readers' arithmetic: a chip's clusters, a disk rounded (the OS's first number is
/// `aterm_sysprobe::MachineWords::os_major`'s, tested there).
/// NEGATIVE CONTROL: with `core_counts` counting every cluster as performance cores, the
/// first assertion reads `(Some(14), Some(0))`.
#[cfg(target_os = "macos")]
#[test]
fn the_readers_arithmetic() {
    use aterm_sysprobe::{Cluster, CpuLayout};
    let cluster = |name: &str, n: u16| Cluster {
        name: name.to_owned(),
        logical: (0..n).collect(),
    };
    assert_eq!(
        core_counts(&CpuLayout {
            clusters: vec![cluster("Performance", 10), cluster("Efficiency", 4)],
        }),
        (Some(10), Some(4))
    );
    assert_eq!(core_counts(&CpuLayout::single(8)), (None, None));
    assert_eq!(gigabytes(994_662_584_320), 995);
}
