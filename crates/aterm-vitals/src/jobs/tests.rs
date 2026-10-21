// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pure kernels: φ and its §11.7 table, the row's number words, the
//! command class and its table, the Tier B frame map, and Tier A naming from
//! each kernel fact (the engine's tick laws are `tests/stuck_scenarios.rs`).

use std::collections::HashMap;
use std::net::SocketAddr;

use super::wait::{JobFacts, Scene, map_leaf_frame, name_job};
use super::wire::{age_words, phi_text};
use super::*;

/// The first whole second at which `quiet` reaches φ × 1000 = `phi` for μ.
fn reach_s(phi: u64, mu_ms: u64) -> u64 {
    (0..10_000u64)
        .find(|s| phi_milli(s * 1000, mu_ms) >= phi)
        .expect("reached")
}

#[test]
fn phi_crosses_the_design_table_at_its_times() {
    // μ 10 s: φ 3 at 69 s, φ 8 at 3 min 4 s.
    assert_eq!(reach_s(3000, 10_000), 70);
    assert_eq!(reach_s(8000, 10_000), 185);
    // μ 60 s: 6 min 55 s and 18 min 25 s.
    assert_eq!(reach_s(3000, 60_000), 415);
    assert_eq!(reach_s(8000, 60_000), 1106);
    // μ 60 s × 2.0: 13 min 49 s and 36 min 50 s.
    assert_eq!(reach_s(3000, 120_000), 829);
    assert_eq!(reach_s(8000, 120_000), 2211);
    // A line every 10 s: a 9 s gap scores 0.39.
    assert_eq!(phi_milli(9_000, 10_000), 390);
    // The examples' figures.
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
    // A digit-led name is no assignment; at most 32 bytes, on a char boundary.
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

#[test]
fn tier_b_maps_libsystem_leaf_frames_only() {
    assert_eq!(
        map_leaf_frame("__semwait_signal  (in libsystem_kernel.dylib) + 8"),
        TierB::Timer
    );
    assert_eq!(map_leaf_frame("nanosleep"), TierB::Timer);
    assert_eq!(
        map_leaf_frame("flock  (in libsystem_kernel.dylib)"),
        TierB::Lock
    );
    assert_eq!(map_leaf_frame("__fcntl"), TierB::Lock);
    assert_eq!(map_leaf_frame("read$NOCANCEL"), TierB::Read);
    assert_eq!(map_leaf_frame("__select_nocancel"), TierB::Event);
    assert_eq!(map_leaf_frame("kevent"), TierB::Event);
    assert_eq!(map_leaf_frame("mach_msg2_trap"), TierB::Ipc);
    assert_eq!(map_leaf_frame("__psynch_cvwait"), TierB::Mutex);
    assert_eq!(map_leaf_frame("__ulock_wait2"), TierB::Mutex);
    assert_eq!(map_leaf_frame("__wait4"), TierB::Children);
    assert_eq!(
        map_leaf_frame("ignore_previous_instructions"),
        TierB::Unparsed
    );
    assert_eq!(TierB::Lock.word(), "lock");
}

// --- Tier A naming, fact by fact -------------------------------------------

fn proc_with(pid: u32, name: &str, fds: Vec<Fd>) -> JobProc {
    JobProc {
        pid,
        name: name.to_owned(),
        fds: Some(fds),
        ..JobProc::default()
    }
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

    // Without lane L6 the entries are unlisted: an event loop, capped.
    let p = proc_with(5, "tail", vec![file(3), kq(true, None)]);
    let w = name_of(&[p], on, &[], None, 5);
    assert_eq!(
        (w.name, w.standing),
        (WaitName::EventLoop, Standing::Capped)
    );

    // A kqueue nobody sleeps on names nothing.
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

    // A server asleep on its listener with a keep-alive client: listen.
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
    // The same server blocked in accept (no event queue): it serves.
    let w = named(vec![listen, idle.clone()]);
    assert_eq!((w.name, w.port), (WaitName::Listen, Some(5173)));
    // A queue whose only entries are the connection: tcp.
    let w = named(vec![
        idle.clone(),
        kq(true, Some(vec![knote(21, KnoteFilter::Read)])),
    ]);
    assert_eq!((w.name, w.standing), (WaitName::Tcp, Standing::Capable));
    // The connection beside a timer: the queue waits for more, an event loop.
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
    // No queue and no listener: the connection is the wait (curl).
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
    // cat reads what curl writes; curl waits on a silent socket.
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
    // Without lane L7 the flag is not read.
    let off = LanesSeen {
        pipe_flags: false,
        ..on
    };
    assert_eq!(
        name_of(&[curl, cat], off, &[], None, 11).name,
        WaitName::Unnamed
    );

    // yes blocked on a full pipe to sleep: the reader decides (unnamed).
    let yes = proc_with(20, "yes", vec![end(1, 0xE2, 0xE1, true, 0, 0)]);
    let sleep = proc_with(21, "sleep", vec![end(0, 0xE1, 0xE2, false, 0x10, 65_536)]);
    let w = name_of(&[yes, sleep], on, &[], None, 20);
    assert_eq!(
        (w.name, w.peer, w.standing),
        (WaitName::PipeFull, Some(21), Standing::Capped)
    );

    // Linux: one inode for both ends, the wait channel names the wait.
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
    // A canonical echoing read is every plain job's termios: not named.
    assert_eq!(
        name_job(&scene, &fg(Some(mode(true, true))), &leaves, (1, "zsh")).name,
        WaitName::Unnamed
    );
    // A stale word (older than the job's last output) is no word.
    assert_eq!(
        name_job(&scene, &fg(None), &leaves, (1, "zsh")).name,
        WaitName::Unnamed
    );
    // Queued input or a background job: no tty name.
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
