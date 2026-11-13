// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests for pure job-stall kernels, command classification, and naming.

use std::collections::HashMap;
use std::net::SocketAddr;

use super::units::runs_exec;
use super::wait::{JobFacts, Scene, name_job};
use super::wire::{age_words, phi_text};
use super::*;

fn memory_entry_context() -> step::Cx {
    step::Cx {
        consumer: Consumer::PersonFg,
        kind: JobKind::Shell,
        stretch_milli: 1000,
        thrashing: false,
        class_mu: Some(60_000),
        fault: None,
    }
}

fn memory_entry_evidence(at_ms: u64, dt_ms: u64, wait: Wait) -> step::Evidence {
    step::Evidence {
        at_ms,
        dt_ms,
        exited: false,
        progress: None,
        refused: false,
        cpu_unread: false,
        cpu_ns: Some(0),
        reaped_cpu_ns: 0,
        tree_work: false,
        calls: Some(0),
        churn: false,
        churn_work: false,
        pageins: Some(0),
        stopped: None,
        holds_fg: true,
        gate: InputGate::default(),
        zombies: None,
        zombie_parent_waits: false,
        wait,
        named: None,
        named_kind: None,
    }
}

fn assert_immediate_memory(out: &step::Out) {
    assert_eq!(out.class, Class::Waiting);
    assert_eq!(
        out.wait.as_ref().map(|wait| wait.name),
        Some(WaitName::Memory)
    );
    assert_eq!(out.standing, Some(Standing::Legit));
    assert_eq!(out.phi_milli, Some(0));
    assert_eq!(out.verdict, Verdict::Fine);
    assert!(!out.no_progress);
}

#[test]
fn memory_wait_clears_existing_rate_classification_on_its_first_tick() {
    for class in [Class::Polling, Class::BusySilent, Class::Thrashing] {
        for refused in [false, true] {
            let mut cx = memory_entry_context();
            cx.thrashing = class == Class::Thrashing;
            let mut track = step::Track::new(0);
            let mut ev = memory_entry_evidence(
                2000,
                2000,
                Wait::bare(WaitName::Unnamed, Standing::Capped, 1, "front"),
            );
            ev.calls = Some(if class == Class::Polling { 80 } else { 0 });
            ev.cpu_ns = Some(if class == Class::BusySilent {
                2_000_000_000
            } else {
                0
            });
            ev.pageins = Some(if class == Class::Thrashing { 200 } else { 0 });
            let _ = step::classify_tick(&mut track, &ev, &cx);
            ev.at_ms = 4000;
            let trained = step::classify_tick(&mut track, &ev, &cx);
            assert_eq!(
                trained.class, class,
                "the prior rate class must really be active"
            );

            let mut ordinary = track.clone();
            ev.at_ms = 6000;
            let unchanged = step::classify_tick(&mut ordinary, &ev, &cx);
            assert_eq!(
                unchanged.class, class,
                "no memory declaration resets ordinary evidence"
            );

            let mut memory = memory_entry_evidence(
                6000,
                2000,
                Wait::bare(WaitName::Memory, Standing::Legit, 1, "front"),
            );
            if refused {
                memory.refused = true;
                memory.cpu_unread = true;
                memory.cpu_ns = None;
                memory.calls = None;
                memory.pageins = None;
            }
            assert_immediate_memory(&step::classify_tick(&mut track, &memory, &cx));

            memory.at_ms = 8000;
            memory.wait = Wait::bare(WaitName::Unnamed, Standing::Capped, 1, "build");
            let admitted = step::classify_tick(&mut track, &memory, &cx);
            assert_eq!(
                admitted.class,
                Class::Waiting,
                "old {class:?} must not enter the build"
            );
            assert_eq!(
                admitted.wait.as_ref().map(|wait| wait.name),
                Some(WaitName::Unnamed)
            );
        }
    }
}

#[test]
fn a_legitimate_memory_wait_clears_a_stuck_episode_on_its_first_tick() {
    let cx = memory_entry_context();
    let mut track = step::Track::new(0);
    let mut ev = memory_entry_evidence(
        0,
        0,
        Wait::bare(WaitName::Tcp, Standing::Capable, 1, "front"),
    );
    let _ = step::classify_tick(&mut track, &ev, &cx);
    ev.at_ms = 28 * 60_000;
    ev.dt_ms = ev.at_ms;
    let stuck = step::classify_tick(&mut track, &ev, &cx);
    assert_eq!(stuck.class, Class::Waiting);
    assert_eq!(
        stuck.verdict,
        Verdict::Stuck,
        "the old suspect episode must really be stuck"
    );
    assert!(track.episode_open());

    let mut blocked = track.clone();
    ev.at_ms += 2000;
    ev.dt_ms = 2000;
    ev.wait = Wait::bare(WaitName::Memory, Standing::Capable, 1, "front");
    let blocked = step::classify_tick(&mut blocked, &ev, &cx);
    assert_eq!(
        blocked.verdict,
        Verdict::Stuck,
        "a blocked claim holder must retain escalation"
    );
    assert!(blocked.phi_milli.is_some_and(|phi| phi > 0));

    let mut unnamed = track.clone();
    ev.wait = Wait::bare(WaitName::Unnamed, Standing::Capped, 1, "front");
    let unnamed = step::classify_tick(&mut unnamed, &ev, &cx);
    assert_ne!(
        unnamed.verdict,
        Verdict::Fine,
        "unproven waits confer no new legitimate standing"
    );
    assert!(unnamed.phi_milli.is_some_and(|phi| phi > 0));

    ev.wait = Wait::bare(WaitName::Memory, Standing::Legit, 1, "front");
    assert_immediate_memory(&step::classify_tick(&mut track, &ev, &cx));
    assert!(!track.episode_open());
}

#[test]
fn an_episode_lock_naming_overrides_polling_without_resetting_its_evidence() {
    let mut cx = memory_entry_context();
    cx.consumer = Consumer::Agent;
    cx.kind = JobKind::AgentCall;
    let mut track = step::Track::new(0);
    let mut ev = memory_entry_evidence(
        0,
        2000,
        Wait::bare(WaitName::Unnamed, Standing::Capped, 1, "front"),
    );
    ev.calls = Some(80);
    for at in (2000..=28 * 60_000).step_by(2000) {
        ev.at_ms = at;
        let _ = step::classify_tick(&mut track, &ev, &cx);
    }
    ev.at_ms += 2000;
    let polling = step::classify_tick(&mut track, &ev, &cx);
    assert_eq!(polling.class, Class::Polling);
    assert_eq!(polling.verdict, Verdict::Stuck);

    for (kind, standing) in [
        (None, Some(Standing::Legit)),
        (Some(TierB::Timer), Some(Standing::Legit)),
        (Some(TierB::Lock), None),
    ] {
        let mut control = track.clone();
        let mut evidence = ev.clone();
        evidence.at_ms += 2000;
        evidence.named_kind = kind;
        evidence.named = standing;
        let out = step::classify_tick(&mut control, &evidence, &cx);
        assert_eq!(out.class, Class::Polling, "{kind:?}, {standing:?}");
        assert_eq!(out.verdict, Verdict::Stuck);
    }

    let mut unknown = track.clone();
    let mut evidence = ev.clone();
    evidence.at_ms += 2000;
    evidence.named_kind = Some(TierB::Lock);
    evidence.named = Some(Standing::Capable);
    let out = step::classify_tick(&mut unknown, &evidence, &cx);
    assert_eq!(out.class, Class::Waiting);
    assert_eq!(out.verdict, Verdict::Stuck);

    ev.named_kind = Some(TierB::Lock);
    ev.named = Some(Standing::Legit);
    ev.at_ms += 2000;
    let first = step::classify_tick(&mut track, &ev, &cx);
    assert_eq!(first.class, Class::Waiting);
    assert_eq!(
        first.verdict,
        Verdict::Stuck,
        "existing two-tick hysteresis"
    );
    ev.at_ms += 2000;
    let confirmed = step::classify_tick(&mut track, &ev, &cx);
    assert_eq!(confirmed.class, Class::Waiting);
    assert_eq!(confirmed.verdict, Verdict::Fine);
    assert_eq!(confirmed.phi_milli, Some(0));
    assert!(!confirmed.no_progress);

    ev.named_kind = None;
    ev.named = None;
    ev.at_ms += 2000;
    assert_eq!(
        step::classify_tick(&mut track, &ev, &cx).class,
        Class::Polling,
        "the rate band was retained while the lock naming counted"
    );
}

/// The first whole second at which `quiet` reaches φ × 1000 = `phi` for μ.
fn reach_s(phi: u64, mu_ms: u64) -> u64 {
    (0..10_000u64)
        .find(|s| phi_milli(s * 1000, mu_ms) >= phi)
        .expect("reached")
}

#[test]
fn phi_crosses_the_design_table_at_its_times() {
    assert_eq!(reach_s(3000, 10_000), 70);
    assert_eq!(reach_s(8000, 10_000), 185);
    assert_eq!(reach_s(3000, 60_000), 415);
    assert_eq!(reach_s(8000, 60_000), 1106);
    assert_eq!(reach_s(3000, 120_000), 829);
    assert_eq!(reach_s(8000, 120_000), 2211);
    assert_eq!(phi_milli(9_000, 10_000), 390);
    assert_eq!(phi_text(phi_milli(1_142_000, 60_000)), "8.27");
    assert_eq!(phi_text(phi_milli(420_000, 12_000)), "15.2");
    assert_eq!(phi_text(phi_milli(1_130_000, 60_000)), "8.18");
    assert_eq!(phi_text(phi_milli(60_000, 60_000)), "0.43");
}

#[test]
fn row_numbers_print_as_the_examples_do() {
    assert_eq!(phi_text(0), "0");
    assert_eq!(phi_text(3000), "3");
    assert_eq!(phi_text(8200), "8.2");
    assert_eq!(phi_text(130_400), "130");
    assert_eq!(
        phi_text(u64::MAX),
        ((u128::from(u64::MAX) + 500) / 1000).to_string()
    );
    assert_eq!(age_words(41_000), "41s");
    assert_eq!(age_words(420_000), "7m");
    assert_eq!(age_words(7_500_000), "2h5m");
}

#[test]
fn the_command_class_is_the_first_words_basename_plus_the_kind() {
    assert_eq!(
        command_class("cargo build --release", JobKind::Shell),
        "cargo shell"
    );
    assert_eq!(
        command_class("RUST_LOG=debug CC=clang /usr/bin/make -j8", JobKind::Shell),
        "make shell"
    );
    assert_eq!(
        command_class("curl -s x", JobKind::AgentCall),
        "curl agent-call"
    );
    assert_eq!(command_class("  ", JobKind::ShellBg), " shell-bg");
    assert_eq!(command_class("2=x y", JobKind::Shell), "2=x shell");
    let long = format!("{}é", "a".repeat(31));
    assert_eq!(
        command_class(&long, JobKind::Shell),
        format!("{} shell", "a".repeat(31))
    );
}

#[test]
fn the_class_table_learns_medians_and_drops_the_least_recently_used() {
    let mut t = ClassTable::default();
    t.learn("make shell", 8_000);
    t.learn("make shell", 12_000);
    assert_eq!(t.mu("make shell"), None, "fewer than 3 completed jobs");
    t.learn("make shell", 9_000);
    assert_eq!(t.mu("make shell"), Some(9_000));
    for i in 0..CLASS_RING + 4 {
        t.learn("make shell", 20_000 + u64::try_from(i).expect("small"));
    }
    assert!(
        t.mu("make shell").is_some_and(|m| m >= 20_000),
        "only the last 16 count"
    );
    for i in 0..CLASS_TABLE {
        t.learn(&format!("c{i} shell"), 1_000);
    }
    assert_eq!(t.len(), CLASS_TABLE);
    assert_eq!(
        t.mu("make shell"),
        None,
        "the least recently used is dropped"
    );
}

fn proc_with(pid: u32, name: &str, fds: Vec<Fd>) -> JobProc {
    JobProc {
        pid,
        name: name.to_owned(),
        fds: Some(fds),
        ..JobProc::default()
    }
}

#[test]
fn admission_keeps_kernel_and_other_unproven_dependencies_visible() {
    let mut queued = proc_with(10, "python3", Vec::new());
    queued.build_admission = Some(vec![50, 51]);
    let other = proc_with(11, "worker", Vec::new());
    let by = HashMap::from([(10, &queued), (11, &other)]);
    let mut ages = HashMap::new();
    let mut elsewhere = HashMap::from([(50, Standing::Capable), (51, Standing::Legit)]);
    let classify = |ages: &HashMap<u32, u64>, elsewhere: &HashMap<u32, Standing>, both| {
        let scene = Scene {
            procs: &by,
            lanes: LanesSeen::default(),
            d_age: ages,
            elsewhere,
            program_check: true,
        };
        let leaves = if both {
            vec![(10, "python3".into()), (11, "worker".into())]
        } else {
            vec![(10, "python3".into())]
        };
        name_job(&scene, &JobFacts::default(), &leaves, (1, "sh"))
    };
    let blocked = classify(&ages, &elsewhere, false);
    assert_eq!(blocked.name, WaitName::BuildAdmission);
    assert_eq!(
        blocked.standing,
        Standing::Capable,
        "one busy holder cannot hide a stuck one"
    );
    elsewhere.clear();
    assert_eq!(classify(&ages, &elsewhere, false).standing, Standing::Legit);
    assert_eq!(classify(&ages, &elsewhere, true).standing, Standing::Capped);
    ages.insert(10, KERNEL_WAIT_MS);
    assert_eq!(classify(&ages, &elsewhere, false).name, WaitName::Kernel);
    ages.clear();
    ages.insert(11, KERNEL_WAIT_MS);
    assert_eq!(classify(&ages, &elsewhere, true).name, WaitName::Kernel);
}

/// A leaf parked in an aterm `await` is legitimate alone, but excuses only itself: a sibling leaf
/// whose wait is unproven caps the job, and an uninterruptible kernel wait of either names kernel.
/// NEGATIVE CONTROL: the same leaf with no parked await reads by its descriptors (unnamed, capped).
#[test]
fn a_parked_await_excuses_only_its_own_leaf_and_yields_to_the_kernel() {
    let mut parked = proc_with(10, "aterm", Vec::new());
    parked.aterm_wait = true;
    let bare = proc_with(10, "aterm", Vec::new());
    let other = proc_with(11, "worker", Vec::new());
    let classify = |leaf: &JobProc, ages: &HashMap<u32, u64>, both: bool| {
        let by = HashMap::from([(10, leaf), (11, &other)]);
        let elsewhere = HashMap::new();
        let scene = Scene {
            procs: &by,
            lanes: LanesSeen::default(),
            d_age: ages,
            elsewhere: &elsewhere,
            program_check: true,
        };
        let leaves = if both {
            vec![(10, "aterm".into()), (11, "worker".into())]
        } else {
            vec![(10, "aterm".into())]
        };
        let w = name_job(&scene, &JobFacts::default(), &leaves, (1, "zsh"));
        (w.name, w.standing)
    };
    let mut ages = HashMap::new();
    assert_eq!(
        classify(&parked, &ages, false),
        (WaitName::AtermWait, Standing::Legit)
    );
    assert_eq!(
        classify(&bare, &ages, false),
        (WaitName::Unnamed, Standing::Capped)
    );
    assert_eq!(
        classify(&parked, &ages, true),
        (WaitName::AtermWait, Standing::Capped),
        "a parked leaf cannot hide a sibling whose wait is unproven"
    );
    ages.insert(10, KERNEL_WAIT_MS);
    assert_eq!(classify(&parked, &ages, false).0, WaitName::Kernel);
    ages.clear();
    ages.insert(11, KERNEL_WAIT_MS);
    assert_eq!(classify(&parked, &ages, true).0, WaitName::Kernel);
}

#[test]
fn a_pipe_retains_its_peer_and_follows_native_admission_standing() {
    let end = |fd, handle, peer, write_end, status| Fd {
        fd,
        kind: FdKind::Pipe {
            handle,
            peer,
            write_end: Some(write_end),
            status: Some(status),
            queued: Some(0),
        },
    };
    let mut queued = proc_with(10, "python3", vec![end(1, 2, 1, true, 0)]);
    queued.build_admission = Some(Vec::new());
    let reader = proc_with(11, "reader", vec![end(0, 1, 2, false, wait::PIPE_WANTR)]);
    let w = name_of(
        &[queued.clone(), reader.clone()],
        LanesSeen::default(),
        &[],
        None,
        11,
    );
    assert_eq!(
        (w.name, w.peer, w.leaf, w.standing),
        (WaitName::Pipe, Some(10), 11, Standing::Legit)
    );
    queued.build_admission = None;
    let w = name_of(&[queued, reader], LanesSeen::default(), &[], None, 11);
    assert_eq!(
        (w.name, w.peer, w.leaf, w.standing),
        (WaitName::Pipe, Some(10), 11, Standing::Capped)
    );
}

fn name_of(
    procs: &[JobProc],
    lanes: LanesSeen,
    d_age: &[(u32, u64)],
    consumer: Option<u32>,
    leaf: u32,
) -> Wait {
    let by: HashMap<u32, &JobProc> = procs.iter().map(|p| (p.pid, p)).collect();
    let d: HashMap<u32, u64> = d_age.iter().copied().collect();
    let elsewhere = HashMap::new();
    let scene = Scene {
        procs: &by,
        lanes,
        d_age: &d,
        elsewhere: &elsewhere,
        program_check: true,
    };
    let job = JobFacts {
        consumer,
        ..JobFacts::default()
    };
    let leaves: Vec<(u32, String)> = vec![(leaf, by[&leaf].name.clone())];
    name_job(&scene, &job, &leaves, (1, "zsh"))
}

fn kq(sleeping: bool, knotes: Option<Vec<Knote>>) -> Fd {
    Fd {
        fd: 9,
        kind: FdKind::EventQueue {
            sleeping: Some(sleeping),
            knotes,
        },
    }
}

fn file(fd: i32) -> Fd {
    Fd {
        fd,
        kind: FdKind::File {
            regular: true,
            size: 10,
            dev: 1,
            ino: 2,
            lf_tail: None,
            fresh_dylib: false,
        },
    }
}

#[test]
fn a_sleeping_kqueue_names_follow_listen_timer_or_an_event_loop() {
    let on = LanesSeen::default();
    let read = |ident| Knote {
        ident,
        filter: KnoteFilter::Read,
    };
    let p = proc_with(5, "tail", vec![file(3), kq(true, Some(vec![read(3)]))]);
    let w = name_of(&[p], on, &[], None, 5);
    assert_eq!(
        (w.name, w.fd, w.standing),
        (WaitName::Follow, Some(3), Standing::Legit)
    );

    let listener = Fd {
        fd: 4,
        kind: FdKind::Tcp {
            state: TcpState::Listen,
            remote: None,
            local_port: 5173,
            queued_read: Some(0),
        },
    };
    let p = proc_with(5, "vite", vec![listener, kq(true, Some(vec![read(4)]))]);
    let w = name_of(&[p], on, &[], None, 5);
    assert_eq!((w.name, w.port), (WaitName::Listen, Some(5173)));

    let timer = Knote {
        ident: 1,
        filter: KnoteFilter::Timer,
    };
    let p = proc_with(5, "timerd", vec![kq(true, Some(vec![timer]))]);
    assert_eq!(name_of(&[p], on, &[], None, 5).name, WaitName::Timer);

    let p = proc_with(5, "tail", vec![file(3), kq(true, None)]);
    let w = name_of(&[p], on, &[], None, 5);
    assert_eq!(
        (w.name, w.standing),
        (WaitName::EventLoop, Standing::Capped)
    );

    let p = proc_with(5, "x", vec![kq(false, Some(vec![read(3)]))]);
    assert_eq!(name_of(&[p], on, &[], None, 5).name, WaitName::Unnamed);
}

#[test]
fn a_connection_names_the_wait_only_when_nothing_else_does() {
    let on = LanesSeen::default();
    let tcp = |fd, state, port| Fd {
        fd,
        kind: FdKind::Tcp {
            state,
            remote: (state != TcpState::Listen)
                .then(|| "127.0.0.1:52011".parse::<SocketAddr>().expect("addr")),
            local_port: port,
            queued_read: Some(0),
        },
    };
    let knote = |ident, filter| Knote { ident, filter };
    let listen = tcp(20, TcpState::Listen, 5173);
    let idle = tcp(21, TcpState::Established, 5173);
    let named = |fds: Vec<Fd>| name_of(&[proc_with(5, "node", fds)], on, &[], None, 5);

    let w = named(vec![
        listen.clone(),
        idle.clone(),
        kq(
            true,
            Some(vec![
                knote(20, KnoteFilter::Read),
                knote(21, KnoteFilter::Read),
            ]),
        ),
    ]);
    assert_eq!(
        (w.name, w.port, w.standing),
        (WaitName::Listen, Some(5173), Standing::Legit)
    );
    let w = named(vec![listen, idle.clone()]);
    assert_eq!((w.name, w.port), (WaitName::Listen, Some(5173)));
    let w = named(vec![
        idle.clone(),
        kq(true, Some(vec![knote(21, KnoteFilter::Read)])),
    ]);
    assert_eq!((w.name, w.standing), (WaitName::Tcp, Standing::Capable));
    let w = named(vec![
        idle.clone(),
        kq(
            true,
            Some(vec![
                knote(21, KnoteFilter::Read),
                knote(1, KnoteFilter::Timer),
            ]),
        ),
    ]);
    assert_eq!(w.name, WaitName::EventLoop);
    let w = named(vec![idle]);
    assert_eq!(w.name, WaitName::Tcp);
}

#[test]
fn a_pipe_counts_as_its_writer_and_a_full_one_as_its_reader() {
    let on = LanesSeen::default();
    let end = |fd, handle, peer, write, status, queued| Fd {
        fd,
        kind: FdKind::Pipe {
            handle,
            peer,
            status: Some(status),
            queued: Some(queued),
            write_end: Some(write),
        },
    };
    let tcp = Fd {
        fd: 5,
        kind: FdKind::Tcp {
            state: TcpState::Established,
            remote: Some("192.0.2.10:8080".parse::<SocketAddr>().expect("addr")),
            local_port: 1,
            queued_read: Some(0),
        },
    };
    let curl = proc_with(10, "curl", vec![end(1, 0xF2, 0xF1, true, 0, 0), tcp]);
    let cat = proc_with(11, "cat", vec![end(0, 0xF1, 0xF2, false, 0x8, 0)]);
    let w = name_of(&[curl.clone(), cat.clone()], on, &[], None, 11);
    assert_eq!(
        (w.name, w.peer, w.standing),
        (WaitName::Pipe, Some(10), Standing::Capable)
    );
    let off = LanesSeen {
        pipe_flags: false,
        ..on
    };
    assert_eq!(
        name_of(&[curl, cat], off, &[], None, 11).name,
        WaitName::Unnamed
    );

    let yes = proc_with(20, "yes", vec![end(1, 0xE2, 0xE1, true, 0, 0)]);
    let sleep = proc_with(21, "sleep", vec![end(0, 0xE1, 0xE2, false, 0x10, 65_536)]);
    let w = name_of(&[yes, sleep], on, &[], None, 20);
    assert_eq!(
        (w.name, w.peer, w.standing),
        (WaitName::PipeFull, Some(21), Standing::Capped)
    );

    let mut reader = proc_with(30, "cat", vec![end(0, 777, 777, false, 0, 0)]);
    reader.wchan = Some("pipe_read".to_owned());
    let mut writer = proc_with(31, "sleep", vec![end(1, 777, 777, true, 0, 0)]);
    writer.wchan = Some("hrtimer_nanosleep".to_owned());
    let w = name_of(&[reader, writer], LanesSeen::default(), &[], None, 30);
    assert_eq!(
        (w.name, w.peer, w.standing),
        (WaitName::Pipe, Some(31), Standing::Legit)
    );
}

/// A pipe with two writers counts as the one that progresses, whichever pid is lower, and names
/// it. NEGATIVE CONTROL: with neither progressing it counts as the first, capped.
#[test]
fn a_pipe_with_two_writers_counts_as_the_one_that_progresses() {
    let end = |fd, handle, peer, write, status| Fd {
        fd,
        kind: FdKind::Pipe {
            handle,
            peer,
            status: Some(status),
            queued: Some(0),
            write_end: Some(write),
        },
    };
    let reader = proc_with(40, "cat", vec![end(0, 0xC1, 0xC2, false, 0x8)]);
    let idle = proc_with(41, "ssh", vec![end(1, 0xC2, 0xC1, true, 0)]);
    let busy = proc_with(42, "trustc", vec![end(1, 0xC2, 0xC1, true, 0)]);
    let by = HashMap::from([(40, &reader), (41, &idle), (42, &busy)]);
    let classify = |elsewhere: &HashMap<u32, Standing>| {
        let d_age = HashMap::new();
        let scene = Scene {
            procs: &by,
            lanes: LanesSeen::default(),
            d_age: &d_age,
            elsewhere,
            program_check: true,
        };
        let w = name_job(
            &scene,
            &JobFacts::default(),
            &[(40, "cat".into())],
            (1, "zsh"),
        );
        (w.name, w.peer, w.standing)
    };
    let working = HashMap::from([(41, Standing::Capped), (42, Standing::Legit)]);
    assert_eq!(
        classify(&working),
        (WaitName::Pipe, Some(42), Standing::Legit)
    );
    let idle_both = HashMap::from([(41, Standing::Capped), (42, Standing::Capped)]);
    assert_eq!(
        classify(&idle_both),
        (WaitName::Pipe, Some(41), Standing::Capped)
    );
}

#[test]
fn the_consumers_pipe_is_serving_and_a_long_d_state_is_kernel() {
    let on = LanesSeen::default();
    let end = Fd {
        fd: 0,
        kind: FdKind::Pipe {
            handle: 0xA1,
            peer: 0xA2,
            status: Some(0x8),
            queued: Some(0),
            write_end: Some(false),
        },
    };
    let agent_end = Fd {
        fd: 7,
        kind: FdKind::Pipe {
            handle: 0xA2,
            peer: 0xA1,
            status: Some(0),
            queued: Some(0),
            write_end: Some(true),
        },
    };
    let server = proc_with(41, "server", vec![end]);
    let agent = proc_with(40, "claude", vec![agent_end]);
    let w = name_of(&[server, agent], on, &[], Some(40), 41);
    assert_eq!(
        (w.name, w.peer, w.standing),
        (WaitName::Serves, Some(40), Standing::Legit)
    );

    let p = proc_with(50, "cp", vec![]);
    assert_eq!(
        name_of(std::slice::from_ref(&p), on, &[(50, 9_000)], None, 50).name,
        WaitName::Unnamed
    );
    let w = name_of(&[p], on, &[(50, KERNEL_WAIT_MS)], None, 50);
    assert_eq!((w.name, w.standing), (WaitName::Kernel, Standing::Capable));
}

#[test]
fn linux_wait_channels_name_sleep_and_event_waits() {
    let mut p = proc_with(60, "sleep", vec![]);
    p.wchan = Some("hrtimer_nanosleep".to_owned());
    assert_eq!(
        name_of(&[p.clone()], LanesSeen::default(), &[], None, 60).name,
        WaitName::Timer
    );
    p.wchan = Some("do_epoll_wait".to_owned());
    assert_eq!(
        name_of(&[p.clone()], LanesSeen::default(), &[], None, 60).name,
        WaitName::EventLoop
    );
    p.wchan = Some("wait_for_partner".to_owned());
    assert_eq!(
        name_of(&[p.clone()], LanesSeen::default(), &[], None, 60).name,
        WaitName::Pipe
    );
    p.wchan = Some("futex_wait_queue".to_owned());
    assert_eq!(
        name_of(&[p], LanesSeen::default(), &[], None, 60).name,
        WaitName::Unnamed
    );
}

#[test]
fn the_tty_names_read_the_fresh_mode_word_only_for_the_foreground_job() {
    let by: HashMap<u32, &JobProc> = HashMap::new();
    let d = HashMap::new();
    let e = HashMap::new();
    let scene = Scene {
        procs: &by,
        lanes: LanesSeen::default(),
        d_age: &d,
        elsewhere: &e,
        program_check: true,
    };
    let leaves = vec![(9, "ssh".to_owned())];
    let mode = |canonical, echo| TtyMode {
        canonical,
        echo,
        stamped_ms: 0,
    };
    let fg = |tty| JobFacts {
        holds_fg: true,
        queue_empty: true,
        tty,
        ..JobFacts::default()
    };
    assert_eq!(
        name_job(&scene, &fg(Some(mode(true, false))), &leaves, (1, "zsh")).name,
        WaitName::TtyPassword
    );
    assert_eq!(
        name_job(&scene, &fg(Some(mode(false, false))), &leaves, (1, "zsh")).name,
        WaitName::TtyRaw
    );
    assert_eq!(
        name_job(&scene, &fg(Some(mode(true, true))), &leaves, (1, "zsh")).name,
        WaitName::Unnamed
    );
    assert_eq!(
        name_job(&scene, &fg(None), &leaves, (1, "zsh")).name,
        WaitName::Unnamed
    );
    let mut j = fg(Some(mode(true, false)));
    j.queue_empty = false;
    assert_eq!(
        name_job(&scene, &j, &leaves, (1, "zsh")).name,
        WaitName::Unnamed
    );
    let mut j = fg(Some(mode(true, false)));
    j.holds_fg = false;
    assert_eq!(
        name_job(&scene, &j, &leaves, (1, "zsh")).name,
        WaitName::Unnamed
    );
    let j = JobFacts {
        builtin: true,
        ..JobFacts::default()
    };
    let w = name_job(&scene, &j, &leaves, (1, "zsh"));
    assert_eq!((w.name, w.leaf), (WaitName::TtyBuiltinRead, 1));
}

#[test]
fn a_command_line_runs_exec_only_as_a_command_word() {
    for (line, runs) in [
        ("exec zsh", true),
        ("cd ~ && exec \"$SHELL\"", true),
        ("TERM=xterm-256color exec tmux", true),
        ("if [ -x ./run ]; then exec ./run; fi", true),
        ("( exec zsh )", true),
        ("case $SHELL in */zsh) exec zsh -l ;; esac", true),
        ("cd ~/proj && \\\n  exec zsh", true),
        (
            "# don't forget: reload so PATH takes effect\nexec zsh",
            true,
        ),
        (
            "cat >> ~/.zshrc <<'EOF'\n# pyenv (don't remove)\neval \"$(pyenv init -)\"\nEOF\nexec zsh",
            true,
        ),
        (
            "ZDOTDIR=${XDG_CONFIG_HOME:-$HOME/.config}/zsh exec zsh",
            true,
        ),
        ("SHELL=$(command -v zsh) exec zsh", true),
        ("case $SHELL in (*/bash) exec zsh;; esac", true),
        ("case $x in a|b) echo hi ;; (c) exec zsh ;; esac", true),
        ("grep -c x <<< 'a b'\necho ${#x}; exec zsh", true),
        ("case $1 in -n) dry=1 ;; esac; exec zsh", true),
        (
            "cat <<-EOF > ~/.zshenv\n\texport EDITOR=vim\n\tEOF\nexec zsh",
            true,
        ),
        ("cat <<-EOF\n\texec zsh\n\tEOF\necho done", false),
        ("echo done # exec zsh", false),
        ("echo $(exec cat) `cd /; exec cat`", false),
        ("docker exec db psql", false),
        ("Invoke-Sqlcmd -Query \"use db; exec dbo.Nightly\"", false),
        ("echo $(date) exec", false),
        ("./execute-migrations.ps1", false),
    ] {
        assert_eq!(runs_exec(line), runs, "{line}");
    }
}

/// One tick of a tab whose shell (100) has the agent call `leader` (400, a session leader with no
/// terminal) under it, read as `facts`, at `at_ms` with the probe's clock 4 ms on; `exits` are the
/// host's watched leaders' ends.
fn call_tick(at_ms: u64, leader: Option<JobProc>, exits: Vec<LeaderExit>) -> JobTick {
    let shell = SweepProc {
        pid: 100,
        ppid: 1,
        pgid: 100,
        uid: Some(501),
        name: "zsh".to_owned(),
        state: ProcState::Sleeping,
        session_leader: Some(true),
        has_tty: Some(true),
        exec: Some(true),
    };
    let mut sweep = vec![shell];
    let mut procs = vec![JobProc {
        pid: 100,
        ppid: 1,
        pgid: 100,
        sid: Some(100),
        start: Some(1),
        uid: Some(501),
        name: "zsh".to_owned(),
        state: ProcState::Sleeping,
        ..JobProc::default()
    }];
    if let Some(p) = leader {
        sweep.push(SweepProc {
            pid: p.pid,
            ppid: 100,
            pgid: p.pid,
            uid: Some(501),
            name: p.name.clone(),
            state: ProcState::Sleeping,
            session_leader: Some(true),
            has_tty: Some(false),
            exec: Some(true),
        });
        procs.push(p);
    }
    JobTick {
        at_ms,
        read_ms: at_ms + 4,
        probe_ms: at_ms + 4,
        self_uid: Some(501),
        sweep,
        procs,
        tabs: vec![JobTab {
            key: 1,
            shell_pid: 100,
            ..JobTab::default()
        }],
        exits,
        ..JobTick::default()
    }
}

/// The call's leader as a pass reads it: started `start`, `age_ms` old as the probe began.
fn leader(start: u64, age_ms: Option<u64>) -> JobProc {
    JobProc {
        pid: 400,
        ppid: 100,
        pgid: 400,
        sid: Some(400),
        start: Some(start),
        age_ms,
        uid: Some(501),
        name: "zsh".to_owned(),
        state: ProcState::Sleeping,
        ..JobProc::default()
    }
}

/// AN AGENT CALL CARRIES ITS LEADER'S OWN START (TM-1 §5): the probe's clock of the first pass that
/// read the leader under the call's start less the age it read, here 600 ms before the pass that
/// first tracked the call, whatever later passes read. NEGATIVE CONTROLS: a call whose leader's age
/// no pass read carries none, and an age read of another process under the leader's pid (a start
/// other than the call's, read by the pass that finds the call gone) is not the call's.
#[test]
fn an_agent_call_carries_its_leaders_start_from_its_age_at_a_pass() {
    // The call the first pass tracked (start 77), once it ended: its leader's start and exit.
    let ends = |first_age: Option<u64>, second: JobProc| {
        let mut jobs = Jobs::new();
        let exit = LeaderExit {
            pid: 400,
            start: Some(77),
            end: ExitOf::Code(0),
            at_ms: 13_000,
        };
        let mut ended = Vec::new();
        for tick in [
            call_tick(10_000, Some(leader(77, first_age)), Vec::new()),
            call_tick(12_000, Some(second), Vec::new()),
            call_tick(14_000, None, vec![exit]),
        ] {
            let _ = jobs.absorb(&tick);
            ended.extend(jobs.take_ended());
        }
        let call = ended
            .iter()
            .find(|e| e.kind == JobKind::AgentCall && e.opened_ms == 10_000)
            .unwrap_or_else(|| panic!("the call ended: {ended:?}"));
        (call.leader_born_ms, call.leader_exited_ms)
    };
    assert_eq!(
        ends(Some(604), leader(77, Some(2_604))),
        (Some(9_400), Some(13_000)),
        "the first pass's reading: 10 004 less 604"
    );
    assert_eq!(
        ends(None, leader(77, Some(2_604))),
        (Some(9_400), Some(13_000)),
        "the first pass that read its age: 12 004 less 2 604"
    );
    assert_eq!(
        ends(None, leader(77, None)),
        (None, Some(13_000)),
        "no age read"
    );
    assert_eq!(
        ends(None, leader(78, Some(5))).0,
        None,
        "another process's age"
    );
    // TM-1 review: a leader older than the pass's clock (started before the reader whose passes
    // read it) has no start on that clock, never the clock's zero.
    assert_eq!(
        ends(Some(600_000), leader(77, Some(602_000))),
        (None, Some(13_000)),
        "older than the pass's clock"
    );
}
