// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The watch connection's order: `Hello`, `Booting` while the Fake's jobs run, `Ready`
//! once it reports `running` or `degraded` and the user bus answers, then `Beat`s; and
//! the host's `Clock`.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use aterm_vz_agent::fake::{BUS_SILENT, Fake};
use aterm_vz_agent::system::{RawJob, System};
use aterm_vz_agent::wire::{Frame, Hello, Job, PROTO, Ready};

use super::{Conn, HANG, until};

const TICK: Duration = Duration::from_millis(10);

fn job(id: u32, unit: &str) -> RawJob {
    RawJob {
        id,
        unit: unit.into(),
        kind: "start".into(),
        state: "running".into(),
    }
}

fn hello() -> Hello {
    Hello {
        proto: PROTO,
        agent_sha256: [7; 32],
        boot_id: "b".into(),
        kernel: "k".into(),
        uptime_ms: 1,
    }
}

/// The watch connection over a socket pair, and its end: the host's side and a channel
/// that says when `run` returned.
fn watching(fake: &Arc<Fake>) -> (Conn, mpsc::Receiver<std::io::Result<()>>) {
    let (host, agent) = UnixStream::pair().unwrap();
    let system: Arc<dyn System> = fake.clone();
    let (done, ended) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(aterm_vz_agent::watch::run(agent, &system, &hello(), TICK));
    });
    (Conn::new(host), ended)
}

fn units(jobs: &[Job]) -> Vec<&str> {
    jobs.iter().map(|j| j.unit.as_str()).collect()
}

#[test]
fn hello_then_booting_while_jobs_run_then_ready_degraded_then_beats() {
    let script = vec![
        vec![job(1, "cloud-init.service")],
        vec![
            job(1, "cloud-init.service"),
            job(2, "systemd-networkd-wait-online.service"),
        ],
        vec![job(2, "systemd-networkd-wait-online.service")],
    ];
    let fake = Arc::new(
        Fake::new()
            .jobs(script.clone())
            .booted("degraded", &["fwupd.service"])
            .bus_after(1),
    );
    let (mut host, ended) = watching(&fake);
    assert_eq!(host.frame(), Some(Frame::Hello(hello())));
    let clock = 1_792_000_123_000_000_000;
    host.send(&Frame::Clock { unix_ns: clock });

    let mut booting = Vec::new();
    let ready = loop {
        match host.frame() {
            Some(Frame::Booting(jobs)) => booting.push(jobs),
            Some(Frame::Ready(r)) => break r,
            other => panic!("before Ready: {other:?}"),
        }
    };
    assert_eq!(
        ready,
        Ready {
            state: "degraded".into(),
            failed_units: vec!["fwupd.service".into()],
        }
    );
    // Every scripted answer went out in order, and the boot was not over until they had:
    // the bus's first refusal adds one more.
    assert!(booting.len() > script.len(), "{booting:#?}");
    for (sent, want) in booting.iter().zip(&script) {
        let want: Vec<&str> = want.iter().map(|j| j.unit.as_str()).collect();
        assert_eq!(units(sent), want);
    }
    // A job's elapsed time runs from when the agent first saw it: 0 in that frame, and at
    // least a tick in the next, which waits out the tick while the boot is not over
    // (the Fake's is over only once its whole script was asked for).
    let elapsed = |id: u32| -> Vec<u64> {
        booting
            .iter()
            .filter_map(|jobs| jobs.iter().find(|j| j.id == id).map(|j| j.elapsed_ms))
            .collect()
    };
    let tick_ms = u64::try_from(TICK.as_millis()).unwrap();
    let (first, second) = (elapsed(1), elapsed(2));
    assert_eq!(first.len(), 2, "{booting:#?}");
    assert_eq!(first[0], 0);
    assert!(first[1] >= tick_ms, "job 1: {first:?}");
    assert_eq!(second[0], 0, "job 2, first seen a frame later: {second:?}");
    assert!(second[1] >= tick_ms, "job 2: {second:?}");
    assert!(second.windows(2).all(|w| w[0] <= w[1]), "{second:?}");

    let mut last = 0;
    for _ in 0..3 {
        match host.frame() {
            Some(Frame::Beat { uptime_ms }) => {
                assert!(uptime_ms > last, "{uptime_ms} after {last}");
                last = uptime_ms;
            }
            other => panic!("after Ready only Beats: {other:?}"),
        }
    }
    until("the host's Clock applied", || {
        fake.clock_sets().contains(&clock).then_some(())
    });

    // It ends on the host's EOF, or on the first Beat it cannot send to a closed host.
    drop(host);
    let _ = ended
        .recv_timeout(HANG)
        .expect("hung: the watch outlived its host");
}

#[test]
fn a_boot_that_was_over_is_ready_running_with_no_failed_unit() {
    let fake = Arc::new(Fake::new());
    let (mut host, _ended) = watching(&fake);
    assert_eq!(host.frame(), Some(Frame::Hello(hello())));
    assert_eq!(host.frame(), Some(Frame::Booting(Vec::new())));
    assert_eq!(
        host.frame(),
        Some(Frame::Ready(Ready {
            state: "running".into(),
            failed_units: Vec::new(),
        }))
    );
    assert!(matches!(host.frame(), Some(Frame::Beat { .. })));
    assert_eq!(
        fake.said(),
        Vec::<String>::new(),
        "a bus that answers is never waited on"
    );
}

/// A boot systemd calls over whose lane user's bus does not answer says so on the
/// console once, with why, while the host sees only empty `Booting` frames.
#[test]
fn a_ready_that_waits_on_the_lane_users_bus_says_why_once() {
    let fake = Arc::new(Fake::new().bus_after(6));
    let (mut host, _ended) = watching(&fake);
    assert_eq!(host.frame(), Some(Frame::Hello(hello())));
    let mut waited = 0;
    let ready = loop {
        match host.frame() {
            Some(Frame::Booting(jobs)) => {
                assert!(jobs.is_empty(), "{jobs:?}");
                waited += 1;
            }
            Some(Frame::Ready(r)) => break r,
            other => panic!("before Ready: {other:?}"),
        }
    };
    assert_eq!(ready.state, "running");
    assert!(waited >= 6, "{waited} Booting frames");
    let said = fake.said();
    let waits: Vec<&String> = said.iter().filter(|l| l.contains(BUS_SILENT)).collect();
    assert_eq!(waits.len(), 1, "{said:#?}");
    assert!(
        waits[0].contains("system is running") && waits[0].contains("lane user's manager"),
        "{}",
        waits[0]
    );
}

#[test]
fn a_boot_that_never_comes_up_is_never_ready() {
    let fake = Arc::new(Fake::new().booted("maintenance", &[]));
    let (mut host, _ended) = watching(&fake);
    assert_eq!(host.frame(), Some(Frame::Hello(hello())));
    for _ in 0..20 {
        match host.frame() {
            Some(Frame::Booting(_)) => {}
            other => panic!("maintenance is not up: {other:?}"),
        }
    }
    assert!(fake.jobs_asked() >= 20);
}
