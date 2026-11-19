// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{panel_cpu_sample, proc};
use crate::events::SessionScope;
use crate::{Sample, Vitals};
use std::sync::Arc;

const SEC: u64 = 1_000_000_000;

fn sample(second: u64) -> Sample {
    let mut s = panel_cpu_sample(second, 0, 0);
    let procs = s.procs.as_mut().unwrap();
    procs.push(proc(201, 200, 0, 8, "compiler"));
    for p in procs {
        let (mib, read, write) = match p.pid {
            200 => (3, 20, 7),
            201 => (8, 30, 13),
            300 => (23, 0, 41),
            _ => unreachable!(),
        };
        p.footprint = Some(mib << 20);
        p.disk_read = Some(read * second);
        p.disk_written = Some(write * second);
    }
    s.event_sources = vec![SessionScope {
        local: 7,
        sid: "build".into(),
        nonce: "first".into(),
    }];
    s
}

#[test]
fn local_panel_mem_disk_tok_footprint_preserves_readable_bytes_and_partial_coverage() {
    let mut vitals = Vitals::new();
    let start = vitals.absorb(&sample(60));
    let a = start.pane_resources[&7].latest.unwrap();
    assert_eq!(
        (a.footprint, a.members, a.footprint_unread),
        (Some(11 << 20), 2, 0)
    );
    assert!(a.membership_complete);
    assert_eq!(
        start.pane_resources[&8].latest.unwrap().footprint,
        Some(23 << 20)
    );
    assert_eq!(a.window_ns, None, "first sweep has a footprint but no rate");
    let mut partial = sample(65);
    // Lost historical CPU evidence does not make the current footprint inventory incomplete.
    partial.native_source_lost = true;
    partial
        .procs
        .as_mut()
        .unwrap()
        .last_mut()
        .unwrap()
        .footprint = None;
    let half = vitals.absorb(&partial);
    let a = half.pane_resources[&7].latest.unwrap();
    assert_eq!(
        (a.at_ns, a.footprint, a.footprint_unread),
        (65 * SEC, Some(3 << 20), 1)
    );
    assert!(
        a.membership_complete,
        "the member is known independently of its footprint"
    );
    let mut machine = sample(67);
    machine.machine_only = true;
    machine.procs = None;
    machine.tabs.clear();
    let cheap = vitals.absorb(&machine);
    assert!(Arc::ptr_eq(&cheap.pane_resources, &half.pane_resources));
    let mut uncertain = sample(70);
    uncertain.procs_truncated = true;
    uncertain.procs.as_mut().unwrap()[1].footprint = None;
    let incomplete = vitals.absorb(&uncertain);
    let a = incomplete.pane_resources[&7].latest.unwrap();
    assert_eq!(a.footprint, Some(11 << 20));
    assert!(!a.membership_complete);
    let b = incomplete.pane_resources[&8].latest.unwrap();
    assert_eq!((b.footprint, b.footprint_unread), (None, 1));
    let mut unread = sample(75);
    unread.procs = None;
    let snapshot = vitals.absorb(&unread);
    let h = &snapshot.pane_resources[&7];
    assert_eq!(h.latest, None);
    assert_eq!(h.buckets(75 * SEC)[11], None);
    assert_eq!(
        h.buckets(75 * SEC)[10],
        Some(a),
        "the prior observation survives the gap"
    );
}

#[test]
fn local_panel_cpu_stream_same_live_shell_rebound_to_another_launch_starts_new_history() {
    let sample = |second: u64, nonce: &str| {
        let mut s = panel_cpu_sample(second, second * 200, second * 400);
        s.event_sources.push(SessionScope {
            local: 7,
            sid: "build".into(),
            nonce: nonce.into(),
        });
        s
    };
    let mut vitals = Vitals::new();
    vitals.absorb(&sample(60, "a"));
    let before = vitals.absorb(&sample(65, "a"));
    assert_eq!(before.pane_cpu[&7].latest.unwrap().cores, 0.2);
    let rebound = vitals.absorb(&sample(70, "b"));
    let cpu = &rebound.pane_cpu[&7];
    assert_eq!(cpu.root, before.pane_cpu[&7].root);
    assert_eq!(cpu.source, rebound.pane_resources[&7].source);
    assert_eq!(cpu.source.as_ref().unwrap().nonce, "b");
    assert_eq!(cpu.latest, None, "the interval crossed a launch boundary");
    assert!(cpu.buckets(70 * SEC).iter().all(Option::is_none));
    assert_eq!(rebound.pane_cpu[&8].latest.unwrap().cores, 0.4);
    let settled = vitals.absorb(&sample(75, "b"));
    let cpu = &settled.pane_cpu[&7];
    assert_eq!(cpu.latest.unwrap().cores, 0.2);
    assert!(cpu.buckets(75 * SEC)[..11].iter().all(Option::is_none));

    let mut missing = sample(80, "c");
    missing.procs = None;
    let rebound = vitals.absorb(&missing);
    assert!(
        rebound.pane_cpu[&7]
            .buckets(80 * SEC)
            .iter()
            .all(Option::is_none)
    );
    assert!(
        rebound.pane_resources[&7]
            .buckets(80 * SEC)
            .iter()
            .all(Option::is_none)
    );
}

#[test]
fn local_panel_mem_disk_tok_disk_intervals_require_each_owned_process_in_each_direction() {
    let mut vitals = Vitals::new();
    vitals.absorb(&sample(60));
    let full = vitals.absorb(&sample(65));
    let a = full.pane_resources[&7].latest.unwrap();
    assert_eq!(
        (a.read_bps, a.write_bps, a.window_ns),
        (Some(50.0), Some(20.0), Some(5 * SEC))
    );
    let b = full.pane_resources[&8].latest.unwrap();
    assert_eq!((b.read_bps, b.write_bps), (Some(0.0), Some(41.0)));
    let mut changed = sample(70);
    changed.tabs[0].tab = 17;
    changed.procs.as_mut().unwrap()[2].disk_read = Some(1);
    changed.procs.as_mut().unwrap()[1].disk_written = None;
    let split = vitals.absorb(&changed);
    let a = split.pane_resources[&7].latest.unwrap();
    let b = split.pane_resources[&8].latest.unwrap();
    assert_eq!(
        (a.read_bps, a.write_bps),
        (None, Some(20.0)),
        "a reset is not zero"
    );
    assert_eq!((b.read_bps, b.write_bps), (Some(0.0), None));
    let mut exit = sample(75);
    exit.procs.as_mut().unwrap().pop();
    let gone = vitals.absorb(&exit);
    assert_eq!(
        gone.pane_resources[&7].latest.unwrap().write_bps,
        None,
        "no final counter from the exited child"
    );
    let mut replacement = sample(80);
    replacement.event_sources[0].nonce = "restarted".into();
    let new_source = vitals.absorb(&replacement);
    let h = &new_source.pane_resources[&7];
    assert_eq!(h.source.as_ref().unwrap().nonce, "restarted");
    assert_eq!(h.latest.unwrap().write_bps, None);
    assert!(h.buckets(80 * SEC)[..11].iter().all(Option::is_none));
    replacement = sample(85);
    replacement.event_sources[0].nonce = "restarted".into();
    replacement.procs.as_mut().unwrap()[0].start = Some(2);
    let born = vitals.absorb(&replacement);
    let h = &born.pane_resources[&7];
    assert_eq!(h.root.unwrap().start, 2);
    assert_eq!(h.latest.unwrap().read_bps, None);
    assert!(h.buckets(85 * SEC)[..11].iter().all(Option::is_none));
    let mut ambiguous = sample(90);
    ambiguous
        .event_sources
        .push(ambiguous.event_sources[0].clone());
    let unbound = vitals.absorb(&ambiguous);
    let h = &unbound.pane_resources[&7];
    assert_eq!(h.source, None, "duplicate launches cannot authorize a card");
    assert_eq!(h.latest.unwrap().read_bps, None);
}
