// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The proc connector's wire, its proof and its routes against a scripted socket and `/proc`
//! (every unix), and the kernel's own listener (Linux).

use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const NONCE: u32 = 0x4c32_0100;
const PROBE: i32 = 4242;
const FAST: Waits = Waits {
    ack_ms: 60_000,
    probe_ms: 60_000,
};

/// One connector datagram from the proc connector: `what`, its stamp and its data words.
fn datagram(what: u32, ack: u32, at_ns: u64, data: &[u32]) -> Vec<u8> {
    let mut event = Vec::new();
    event.extend_from_slice(&what.to_ne_bytes());
    event.extend_from_slice(&0u32.to_ne_bytes());
    event.extend_from_slice(&at_ns.to_ne_bytes());
    for word in data {
        event.extend_from_slice(&word.to_ne_bytes());
    }
    let total = 16 + 20 + event.len();
    let mut m = Vec::new();
    m.extend_from_slice(&u32::try_from(total).unwrap().to_ne_bytes());
    m.extend_from_slice(&3u16.to_ne_bytes());
    m.extend_from_slice(&0u16.to_ne_bytes());
    m.extend_from_slice(&7u32.to_ne_bytes());
    m.extend_from_slice(&0u32.to_ne_bytes());
    m.extend_from_slice(&CN_IDX_PROC.to_ne_bytes());
    m.extend_from_slice(&CN_VAL_PROC.to_ne_bytes());
    m.extend_from_slice(&9u32.to_ne_bytes());
    m.extend_from_slice(&ack.to_ne_bytes());
    m.extend_from_slice(&u16::try_from(event.len()).unwrap().to_ne_bytes());
    m.extend_from_slice(&0u16.to_ne_bytes());
    m.extend_from_slice(&event);
    m
}

fn ack(ack: u32, err: u32) -> Vec<u8> {
    datagram(PROC_EVENT_NONE, ack, 0, &[err])
}

fn exec(pid: i32, at_ns: u64) -> Vec<u8> {
    let pid = u32::try_from(pid).unwrap();
    datagram(PROC_EVENT_EXEC, 0, at_ns, &[pid, pid])
}

fn exit(task: i32, pid: i32) -> Vec<u8> {
    let (task, pid) = (u32::try_from(task).unwrap(), u32::try_from(pid).unwrap());
    datagram(PROC_EVENT_EXIT, 0, 0, &[task, pid, 0, 17, 1, 1])
}

/// What a sent subscription asked: its form and its ack field.
fn asked(message: &[u8]) -> (Form, u32) {
    let ack = u32_at(message, 16 + 12).unwrap();
    let op = u32_at(message, 36).unwrap();
    assert_eq!(op, PROC_CN_MCAST_LISTEN, "only LISTEN is ever sent");
    let form = match message.len() {
        40 => Form::Op,
        44 => Form::Input(u32_at(message, 40).unwrap()),
        n => panic!("a subscription of {n} bytes"),
    };
    (form, ack)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reply {
    Data,
    Lost,
    Failed,
}

/// What a scripted socket will read next, shared with the probe that queues its `exec`.
type Queue = Rc<RefCell<VecDeque<(Reply, Vec<u8>)>>>;

/// A scripted socket: a kernel that answers each subscription as `answer` says, and what the
/// test queued.
struct Fake {
    queue: Queue,
    sent: Rc<RefCell<Vec<(Form, u32)>>>,
    answer: Box<dyn FnMut(Form) -> Option<u32>>,
}

impl Fake {
    /// A kernel answering each form with `answer` (`Some(err)`: an ack carrying `err`).
    fn new(answer: impl FnMut(Form) -> Option<u32> + 'static) -> Self {
        Self {
            queue: Rc::default(),
            sent: Rc::default(),
            answer: Box::new(answer),
        }
    }

    /// Linux 6.8 as the lane measured it: both forms asking for every event are acked; a
    /// narrowing `proc_input`'s ack the kernel's filter drops.
    fn linux_6_8() -> Self {
        Self::new(|form| match form {
            Form::Op | Form::Input(PROC_EVENT_ALL) => Some(0),
            Form::Input(_) => None,
        })
    }

    fn push(&self, reply: Reply, bytes: Vec<u8>) {
        self.queue.borrow_mut().push_back((reply, bytes));
    }

    fn sent(&self) -> Vec<(Form, u32)> {
        self.sent.borrow().clone()
    }
}

impl Netlink for Fake {
    fn send(&mut self, message: &[u8]) -> Result<(), i32> {
        let (form, sent_ack) = asked(message);
        self.sent.borrow_mut().push((form, sent_ack));
        if let Some(err) = (self.answer)(form) {
            self.queue
                .borrow_mut()
                .push_back((Reply::Data, ack(sent_ack.wrapping_add(1), err)));
        }
        Ok(())
    }

    fn recv(&mut self, buf: &mut [u8], _wait_ms: i32) -> Recv {
        match self.queue.borrow_mut().pop_front() {
            None => Recv::Idle,
            Some((Reply::Lost, _)) => Recv::Lost,
            Some((Reply::Failed, _)) => Recv::Failed(5),
            Some((Reply::Data, bytes)) => {
                buf[..bytes.len()].copy_from_slice(&bytes);
                Recv::Data(bytes.len())
            }
        }
    }
}

/// A probe whose `exec` reaches `queue` when it starts (`heard`), or never, after what
/// `before` queues then (other processes' `exec`s).
struct FakeProbe {
    queue: Queue,
    heard: bool,
    before: Vec<Vec<u8>>,
    started: bool,
    finished: bool,
}

impl FakeProbe {
    fn on(net: &Fake, heard: bool) -> Self {
        Self {
            queue: Rc::clone(&net.queue),
            heard,
            before: Vec::new(),
            started: false,
            finished: false,
        }
    }
}

impl Prober for FakeProbe {
    fn start(&mut self) -> Option<i32> {
        self.started = true;
        for bytes in self.before.drain(..) {
            self.queue.borrow_mut().push_back((Reply::Data, bytes));
        }
        if self.heard {
            self.queue
                .borrow_mut()
                .push_back((Reply::Data, exec(PROBE, 1_000)));
        }
        Some(PROBE)
    }

    fn finish(&mut self) {
        self.finished = true;
    }
}

/// The subscription is the kernel's `nlmsghdr`, `cn_msg` and op or `proc_input`, and a datagram
/// reads as its acks, `exec`s and process exits, a thread's exit and a fork as neither, a message
/// cut short ending the read and another connector's message read as nothing of ours.
/// NEGATIVE CONTROL (scratch, `L2_NC=parse`): an exit read for every thread fails "a thread's
/// exit".
#[test]
fn a_listen_message_is_the_kernels_layout_and_parse_reads_each_kind() {
    let op = listen_message(Form::Op, 77);
    assert_eq!(op.len(), 40, "nlmsghdr + cn_msg + the 4-byte op");
    assert_eq!(u32_at(&op, 0), Some(40), "nlmsg_len");
    assert_eq!(&op[4..6], &3u16.to_ne_bytes(), "NLMSG_DONE");
    assert_eq!(u32_at(&op, 16), Some(CN_IDX_PROC));
    assert_eq!(u32_at(&op, 20), Some(CN_VAL_PROC));
    assert_eq!(u32_at(&op, 28), Some(77), "the cn_msg ack field");
    assert_eq!(&op[32..34], &4u16.to_ne_bytes(), "cn_msg len");
    assert_eq!(asked(&op), (Form::Op, 77));
    let input = listen_message(Form::Input(MASK), 78);
    assert_eq!(input.len(), 44);
    assert_eq!(asked(&input), (Form::Input(MASK), 78));

    let mut both = exec(31, 0x1_0000_0002);
    both.extend_from_slice(&ack(79, LINUX_EINVAL));
    both.extend_from_slice(&exit(31, 31));
    both.extend_from_slice(&exit(32, 31));
    both.extend_from_slice(&datagram(PROC_EVENT_FORK, 0, 0, &[1, 1, 31, 31]));
    let mut heard = Vec::new();
    parse(&both, &mut heard);
    assert_eq!(
        heard,
        [
            Heard::Exec {
                pid: 31,
                at_ns: 0x1_0000_0002
            },
            Heard::Ack {
                ack: 79,
                err: LINUX_EINVAL
            },
            Heard::Exit { pid: 31 },
            Heard::Other,
            Heard::Other,
        ],
        "a thread's exit and a fork are no process's exit or exec"
    );
    let mut cut = Vec::new();
    parse(&both[..both.len() - 3], &mut cut);
    assert_eq!(cut.len(), 4, "a message cut short ends the read");
    let mut other = exec(31, 1);
    other[16..20].copy_from_slice(&2u32.to_ne_bytes());
    let mut foreign = Vec::new();
    parse(&other, &mut foreign);
    assert_eq!(foreign, [Heard::Other], "another connector's message");
}

/// Linux 6.8 as the lane measured it: the `proc_input` asking for every event is acked, the mask
/// is sent after it, and the probe's own `exec` makes the subscription live, narrowed.
/// NEGATIVE CONTROL (scratch, `L2_NC=mask`): no mask sent fails "narrowed".
#[test]
fn an_acked_subscription_whose_probe_is_heard_is_live_and_narrowed_to_its_mask() {
    let mut net = Fake::linux_6_8();
    let mut probe = FakeProbe::on(&net, true);
    let proven = prove(&mut net, &mut probe, NONCE, FAST);
    assert_eq!(proven, Ok(Proven { masked: true }));
    assert_eq!(
        net.sent(),
        [
            (Form::Input(PROC_EVENT_ALL), NONCE),
            (Form::Input(MASK), NONCE + 4)
        ],
        "every event first, whose ack the kernel's filter lets through, then the mask"
    );
    assert!(probe.started && probe.finished, "the probe is reaped");
}

/// Another user namespace, as the lane measured it under `unshare -U`: the kernel ignores both
/// forms, though `exec`s still arrive through other listeners' subscriptions; so the ack, not
/// the probe, closes it, and acks of another listener's fields answer nothing of this one's.
/// NEGATIVE CONTROL (scratch, `L2_NC=ack`): an unanswered subscription taken as acked claims
/// live and fails both cases.
#[test]
fn a_subscription_nothing_acks_is_closed_though_its_probe_is_heard() {
    // Another user namespace: the kernel ignores the request, and the probe's exec still comes
    // through another listener's subscription.
    let mut net = Fake::new(|_| None);
    let mut probe = FakeProbe::on(&net, true);
    assert_eq!(prove(&mut net, &mut probe, NONCE, FAST), Err(Closed::NoAck));
    assert_eq!(
        net.sent(),
        [(Form::Input(PROC_EVENT_ALL), NONCE), (Form::Op, NONCE + 2)],
        "both forms were asked"
    );
    assert!(
        !probe.started,
        "no probe runs for a subscription nothing acked"
    );
    let mut foreign = Fake::new(|_| None);
    foreign.push(Reply::Data, ack(NONCE + 7, 0));
    foreign.push(Reply::Data, ack(1, 0));
    let mut probe = FakeProbe::on(&foreign, true);
    assert_eq!(
        prove(&mut foreign, &mut probe, NONCE, FAST),
        Err(Closed::NoAck),
        "another listener's acks answer no request of this one"
    );
}

/// A `proc_input` refused as a form (`EINVAL`, `ENOPROTOOPT`) or unanswered (a kernel before
/// 6.6) falls back to the 4-byte op, every event and no mask; any other error refuses the
/// subscription; a mask answered with an error leaves every event. NEGATIVE CONTROL (scratch,
/// `L2_NC=fallback`): no fallback to the op fails "mask unsupported".
#[test]
fn a_form_the_kernel_refuses_or_ignores_falls_back_to_every_event() {
    for (name, input_answer) in [
        ("EINVAL", Some(LINUX_EINVAL)),
        ("ENOPROTOOPT", Some(LINUX_ENOPROTOOPT)),
        ("before 6.6: unanswered", None),
    ] {
        let mut net = Fake::new(move |form| match form {
            Form::Op => Some(0),
            Form::Input(_) => input_answer,
        });
        let mut probe = FakeProbe::on(&net, true);
        assert_eq!(
            prove(&mut net, &mut probe, NONCE, FAST),
            Ok(Proven { masked: false }),
            "{name}: mask unsupported, every event"
        );
        assert_eq!(
            net.sent(),
            [(Form::Input(PROC_EVENT_ALL), NONCE), (Form::Op, NONCE + 2)],
            "{name}: no mask follows the 4-byte form"
        );
    }
    let mut refusing = Fake::new(|_| Some(1));
    let mut probe = FakeProbe::on(&refusing, true);
    assert_eq!(
        prove(&mut refusing, &mut probe, NONCE, FAST),
        Err(Closed::Refused),
        "EPERM refuses the subscription itself"
    );
    let mut mask_refused = Fake::new(|form| match form {
        Form::Op | Form::Input(PROC_EVENT_ALL) => Some(0),
        Form::Input(_) => Some(LINUX_EINVAL),
    });
    let mut probe = FakeProbe::on(&mask_refused, true);
    assert_eq!(
        prove(&mut mask_refused, &mut probe, NONCE, FAST),
        Ok(Proven { masked: false }),
        "a mask answered with an error leaves every event"
    );
    assert_eq!(mask_refused.sent().last(), Some(&(Form::Op, NONCE + 6)));
}

/// An acked subscription whose probe's `exec` never comes (or only another process's does) is
/// closed, and the probe is reaped. NEGATIVE CONTROL (scratch, `L2_NC=probe`): a proof that
/// takes any `exec` as its probe's fails "another process's exec".
#[test]
fn an_acked_subscription_whose_probe_exec_never_comes_is_closed() {
    let mut net = Fake::linux_6_8();
    let mut probe = FakeProbe::on(&net, false);
    assert_eq!(
        prove(&mut net, &mut probe, NONCE, FAST),
        Err(Closed::ProbeUnheard)
    );
    assert!(probe.finished, "the probe is reaped though unheard");
    let mut other = Fake::linux_6_8();
    let mut probe = FakeProbe::on(&other, false);
    probe.before.push(exec(PROBE + 1, 5));
    assert_eq!(
        prove(&mut other, &mut probe, NONCE, FAST),
        Err(Closed::ProbeUnheard),
        "another process's exec is no probe's"
    );
}

/// A drop the kernel reports (`ENOBUFS`) while an ack is awaited may have dropped the ack: the
/// subscription is sent again. NEGATIVE CONTROL (scratch, `L2_NC=resend`): no resend closes it
/// with no ack and fails the proof.
#[test]
fn a_drop_while_an_ack_is_awaited_sends_the_subscription_again() {
    let mut net = Fake::new(|_| None);
    let answered = Rc::new(RefCell::new(0u32));
    let count = Rc::clone(&answered);
    net.answer = Box::new(move |form| {
        *count.borrow_mut() += 1;
        (form == Form::Input(PROC_EVENT_ALL) && *count.borrow() == 2).then_some(0)
    });
    net.push(Reply::Lost, Vec::new());
    let mut probe = FakeProbe::on(&net, true);
    assert_eq!(
        prove(&mut net, &mut probe, NONCE, FAST),
        Ok(Proven { masked: true })
    );
    assert_eq!(
        net.sent()[..2],
        [
            (Form::Input(PROC_EVENT_ALL), NONCE),
            (Form::Input(PROC_EVENT_ALL), NONCE)
        ]
    );
}

/// A probe whose wait loses messages (`ENOBUFS`) for its first `losses` starts, the `exec`
/// among them, and is heard after unless `never`: each start a new pid.
struct LossyProbe {
    queue: Queue,
    losses: u32,
    never: bool,
    starts: u32,
}

impl LossyProbe {
    fn on(net: &Fake, losses: u32, never: bool) -> Self {
        Self {
            queue: Rc::clone(&net.queue),
            losses,
            never,
            starts: 0,
        }
    }
}

impl Prober for LossyProbe {
    fn start(&mut self) -> Option<i32> {
        self.starts += 1;
        let pid = PROBE + i32::try_from(self.starts).unwrap();
        if self.starts <= self.losses {
            self.queue.borrow_mut().push_back((Reply::Lost, Vec::new()));
        } else if !self.never {
            self.queue
                .borrow_mut()
                .push_back((Reply::Data, exec(pid, 1_000)));
        }
        Some(pid)
    }

    fn finish(&mut self) {}
}

/// A drop the kernel reports while a probe's `exec` is awaited may have dropped that `exec`: a
/// probe is started again, at most [`LOST_RESENDS`] times, and a probe heard then proves it;
/// drops in every wait close it as `probe-lost`, not `probe-unheard`, and a wait with no drop
/// starts no other. NEGATIVE CONTROLS (scratch): no restart (`L2_NC=reprobe`) fails "started
/// again"; every drop closing as unheard (`L2_NC=probelost`) fails "probe-lost".
#[test]
fn a_drop_while_a_probe_is_awaited_starts_the_probe_again() {
    let mut net = Fake::linux_6_8();
    let mut probe = LossyProbe::on(&net, LOST_RESENDS, false);
    assert_eq!(
        prove(&mut net, &mut probe, NONCE, FAST),
        Ok(Proven { masked: true }),
        "started again, a later probe proves it"
    );
    assert_eq!(probe.starts, LOST_RESENDS + 1);
    assert_eq!(
        net.sent(),
        [
            (Form::Input(PROC_EVENT_ALL), NONCE),
            (Form::Input(MASK), NONCE + 4)
        ],
        "a probe started again asks nothing again"
    );
    let mut net = Fake::linux_6_8();
    let mut probe = LossyProbe::on(&net, LOST_RESENDS + 1, false);
    assert_eq!(
        prove(&mut net, &mut probe, NONCE, FAST),
        Err(Closed::ProbeLost),
        "probe-lost: drops in every wait"
    );
    assert_eq!(probe.starts, LOST_RESENDS + 1, "bounded");
    let mut net = Fake::linux_6_8();
    let mut probe = LossyProbe::on(&net, 0, true);
    assert_eq!(
        prove(&mut net, &mut probe, NONCE, FAST),
        Err(Closed::ProbeUnheard)
    );
    assert_eq!(probe.starts, 1, "a wait with no drop starts no other");
}

/// A scripted `/proc`: each pid's stat reads, in turn, and its image.
#[derive(Default)]
struct FakeProc {
    stats: HashMap<i32, VecDeque<Stat>>,
    images: HashMap<i32, Image>,
    /// Every read, in order: `exe`, `stat` or `file`, and its pid.
    order: Vec<(&'static str, i32)>,
}

impl FakeProc {
    fn process(&mut self, pid: i32, session: i32, pgrp: i32, start_ns: u64) {
        let stat = Stat {
            pgrp,
            session,
            tpgid: pgrp,
            start_ns,
            execd: true,
        };
        self.stats.insert(pid, VecDeque::from([stat, stat]));
        self.images.insert(
            pid,
            Image {
                path: PathBuf::from(format!("/usr/bin/p{pid}")),
                file: Some((1, u64::try_from(pid).unwrap())),
            },
        );
    }
}

impl Kernel for FakeProc {
    fn stat(&mut self, pid: i32) -> Option<Stat> {
        self.order.push(("stat", pid));
        let reads = self.stats.get_mut(&pid)?;
        if reads.len() > 1 {
            reads.pop_front()
        } else {
            reads.front().copied()
        }
    }

    fn exe(&mut self, pid: i32) -> Option<PathBuf> {
        self.order.push(("exe", pid));
        self.images.get(&pid).map(|i| i.path.clone())
    }

    fn exe_file(&mut self, pid: i32) -> Option<(u64, u64)> {
        self.order.push(("file", pid));
        self.images.get(&pid).and_then(|i| i.file)
    }

    fn env_path(&mut self, _pid: i32) -> Option<Box<str>> {
        Some("/usr/bin:/bin".into())
    }

    fn boot_offset_ns(&mut self) -> Option<u64> {
        Some(1_000)
    }
}

fn readable(fd: i32) -> bool {
    let mut fds = [libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: one pollfd, live for the call; a zero wait.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) > 0 }
}

fn execs(events: &[Event]) -> Vec<(i32, i32)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Exec(x) => Some((x.pid, x.pgid)),
            Event::Exit { .. } => None,
        })
        .collect()
}

/// One socket serves every session: each `exec` goes to the inbox of the shell that leads its
/// process's session, stamped on the boot clock, a leader's with its `PATH`; a build's thousand
/// `exec`s wake that session alone, its inbox bounded with the rest lost; a group leader's exit
/// goes where its `exec` went, a child's nowhere, and a dropped route routes nothing. NEGATIVE
/// CONTROLS (scratch): every `exec` handed to every inbox (`L2_NC=route`) fails "no session
/// leads 301"; every routed process's exit routed (`L2_NC=exits`) fails "a build child's exit".
#[test]
fn each_exec_goes_to_the_session_that_leads_it_and_wakes_no_other() {
    let hub = Arc::new(Hub::new());
    let (a, b) = (hub.register(100).unwrap(), hub.register(200).unwrap());
    let mut kernel = FakeProc::default();
    kernel.process(101, 100, 101, 10);
    kernel.process(201, 200, 201, 10);
    kernel.process(301, 300, 301, 10);
    let mut net = Fake::new(|_| None);
    net.push(Reply::Data, exec(101, 50));
    net.push(Reply::Data, exec(201, 50));
    net.push(Reply::Data, exec(301, 50));
    assert_eq!(listen(&mut net, &hub, &mut kernel, 0), Listened::Idle);
    let (mut got_a, mut got_b) = (Vec::new(), Vec::new());
    assert!(!a.drain(&mut got_a) && !b.drain(&mut got_b));
    assert_eq!(execs(&got_a), [(101, 101)]);
    assert_eq!(execs(&got_b), [(201, 201)], "no session leads 301");
    let Event::Exec(first) = &got_a[0] else {
        panic!("an exec")
    };
    assert_eq!(first.at_ns, 1_050, "stamped on the boot clock");
    assert_eq!(
        first.env_path.as_deref(),
        Some("/usr/bin:/bin"),
        "a leader's PATH"
    );

    // A build in A: a thousand execs of its job's children wake A alone.
    for pid in 1_000..2_000 {
        kernel.process(pid, 100, 101, 10);
        net.push(Reply::Data, exec(pid, 60));
    }
    listen(&mut net, &hub, &mut kernel, 0);
    assert!(readable(a.fd()) && !readable(b.fd()), "B's gather sleeps");
    got_a.clear();
    assert!(
        a.drain(&mut got_a),
        "A's inbox held INBOX_MAX and lost the rest"
    );
    assert_eq!(got_a.len(), INBOX_MAX);
    assert!(
        got_a
            .iter()
            .all(|e| matches!(e, Event::Exec(x) if x.pgid == 101 && x.env_path.is_none()))
    );
    assert!(!readable(a.fd()), "a drain takes the wake with it");

    // A leader's exit goes where its exec went; a child's, or one never routed, goes nowhere.
    net.push(Reply::Data, exit(201, 201));
    net.push(Reply::Data, exit(1_000, 1_000));
    net.push(Reply::Data, exit(101, 101));
    net.push(Reply::Data, exit(301, 301));
    listen(&mut net, &hub, &mut kernel, 0);
    let (mut exits_a, mut exits_b) = (Vec::new(), Vec::new());
    a.drain(&mut exits_a);
    b.drain(&mut exits_b);
    assert_eq!(
        exits_a,
        [Event::Exit { pid: 101 }],
        "a build child's exit is routed nowhere"
    );
    assert_eq!(exits_b, [Event::Exit { pid: 201 }]);
    drop(b);
    net.push(Reply::Data, exec(201, 70));
    listen(&mut net, &hub, &mut kernel, 0);
    assert!(
        !readable(a.fd()),
        "a dropped route routes nothing, and wakes no other"
    );
}

/// A pid whose start is later than the `exec` (reused since), or that has not exec'd since its
/// fork, is never routed. The executable and its file's identity are read before the stat that
/// vouches for both, so a process that exited between them (its file gone, its stat a zombie's)
/// is still routed, without the identity, and one whose executable no longer reads costs no
/// stat. NEGATIVE CONTROLS (scratch): `L2_NC=reuse` (no start checks) routes the reused pids;
/// `L2_NC=order` (the identity read after the stat) fails "read before the stat that vouches
/// for them"; `L2_NC=gone` (the stat read first)
/// fails "no stat for a process already gone".
#[test]
fn a_pid_reused_or_not_yet_execd_is_never_routed() {
    let hub = Arc::new(Hub::new());
    let route = hub.register(100).unwrap();
    let mut kernel = FakeProc::default();
    // 101 started after the exec was stamped (1_000 + 50): a reused pid.
    kernel.process(101, 100, 101, 1_051);
    // 102 exited between the two reads: its executable read, its file no longer does.
    kernel.process(102, 100, 102, 10);
    if let Some(image) = kernel.images.get_mut(&102) {
        image.file = None;
    }
    // 103 forked and has not exec'd: a reused pid's fork.
    kernel.process(103, 100, 103, 10);
    for stat in kernel.stats.get_mut(&103).unwrap().iter_mut() {
        stat.execd = false;
    }
    // 104 started at the exec's own instant: it may be trusted.
    kernel.process(104, 100, 104, 1_050);
    // 105 exited before its executable was read.
    kernel.process(105, 100, 105, 10);
    kernel.images.remove(&105);
    let mut net = Fake::new(|_| None);
    for pid in [101, 102, 103, 104, 105] {
        net.push(Reply::Data, exec(pid, 50));
    }
    listen(&mut net, &hub, &mut kernel, 0);
    let mut got = Vec::new();
    route.drain(&mut got);
    assert_eq!(execs(&got), [(102, 102), (104, 104)]);
    let files: Vec<Option<(u64, u64)>> = got
        .iter()
        .filter_map(|e| match e {
            Event::Exec(x) => Some(x.image.file),
            Event::Exit { .. } => None,
        })
        .collect();
    assert_eq!(files, [None, Some((1, 104))], "102's file was gone");
    let reads = |pid: i32| -> Vec<&str> {
        kernel
            .order
            .iter()
            .filter(|r| r.1 == pid)
            .map(|r| r.0)
            .collect()
    };
    assert_eq!(
        reads(104),
        ["exe", "file", "stat"],
        "the executable and its file are read before the stat that vouches for them"
    );
    assert_eq!(reads(105), ["exe"], "no stat for a process already gone");
}

/// A drop the kernel reports tells every session, once; a socket that fails closes the hub.
/// NEGATIVE CONTROL (scratch, `L2_NC=lost`): a drop that tells nobody fails "each is told".
#[test]
fn a_drop_the_kernel_reports_tells_every_session_it_lost() {
    let hub = Arc::new(Hub::new());
    let (a, b) = (hub.register(100).unwrap(), hub.register(200).unwrap());
    let mut kernel = FakeProc::default();
    let mut net = Fake::new(|_| None);
    net.push(Reply::Lost, Vec::new());
    assert_eq!(listen(&mut net, &hub, &mut kernel, 0), Listened::Idle);
    assert!(readable(a.fd()) && readable(b.fd()), "each gather is woken");
    let mut got = Vec::new();
    assert!(a.drain(&mut got) && b.drain(&mut got), "each is told");
    assert!(!a.drain(&mut got), "once");
    assert!(got.is_empty());
    net.push(Reply::Failed, Vec::new());
    assert_eq!(listen(&mut net, &hub, &mut kernel, 0), Listened::Failed);
    assert_eq!(a.state(), State::Closed(Closed::Failed));
}

/// Every state the listener can stand at reads back through every route, and each has a word of
/// its own (`status exec_watch_why=`). NEGATIVE CONTROL (scratch, `L2_NC=state`): one word for
/// both live states fails "one word a state".
#[test]
fn a_state_reads_back_through_every_route_and_names_its_reason() {
    let hub = Arc::new(Hub::new());
    let route = hub.register(5).unwrap();
    assert_eq!(route.state(), State::Proving);
    let every: Vec<State> = [State::Proving, State::Live { masked: true }]
        .into_iter()
        .chain([State::Live { masked: false }])
        .chain(Closed::ALL.map(State::Closed))
        .collect();
    assert_eq!(every, State::ALL, "State::ALL is every state, in order");
    for state in State::ALL {
        hub.set_state(state);
        assert_eq!(route.state_reader().get(), state);
    }
    let words: std::collections::BTreeSet<&str> = State::ALL.iter().map(|s| s.as_str()).collect();
    assert_eq!(words.len(), State::ALL.len(), "one word a state: {words:?}");
}

/// A `/proc/<pid>/stat` line reads as its group, session, terminal group, start and whether it
/// exec'd since its fork, a zombie's too; another pid's reads nothing; `PATH` is read from an
/// environment, bounded. NEGATIVE CONTROLS (scratch): `L2_NC=stat` (the exec flag ignored) fails
/// "PF_FORKNOEXEC"; `L2_NC=zombie` (a zombie reads nothing) fails "a zombie is still the program
/// that exec'd".
#[test]
fn a_stat_line_reads_its_group_session_terminal_start_and_exec() {
    let line = "4242 (sleep) S 100 4242 100 34816 4242 4194304 0 0 0 0 0 0 0 0 20 0 1 0 \
                12345 2048 100 0 0 0";
    let stat = stat_of(line, 4242, 100).unwrap();
    assert_eq!(
        stat,
        Stat {
            pgrp: 4242,
            session: 100,
            tpgid: 4242,
            start_ns: 123_450_000_000,
            execd: true,
        }
    );
    let forked = line.replace(" 4194304 ", " 4194368 ");
    assert!(!stat_of(&forked, 4242, 100).unwrap().execd, "PF_FORKNOEXEC");
    let zombie = line.replace(" S ", " Z ");
    assert_eq!(
        stat_of(&zombie, 4242, 100),
        Some(stat),
        "a zombie is still the program that exec'd"
    );
    assert_eq!(stat_of(line, 4243, 100), None, "another pid's line");
    assert_eq!(
        env_path_of("HOME=/h\0PATH=/x:/y\0Z=1\0").as_deref(),
        Some("/x:/y")
    );
    assert_eq!(env_path_of(&format!("PATH={}", "a".repeat(5_000))), None);
}

/// The kernel's own listener (Linux 6.8 on the lane): in the initial user and pid namespaces it
/// is proven, narrowed to its mask, and routes a child's `exec` to this process's session, its
/// image read; in another user namespace (`unshare -U`, the lane's run) it is closed, with no
/// ack, and routes nothing. The child lives until its `exec` is heard: a 1 ms `true` may exit
/// before its executable is read (the lane's hit rate is in `docs/DESIGN-session-log-2026-10-15.md`
/// §10, unit L2), which a test cannot hold. NEGATIVE CONTROL (scratch, `L2_NC=ack`, on the lane
/// under `unshare -U`): without the ack check the probe's `exec`, which other listeners'
/// subscriptions still bring, claims live.
#[cfg(target_os = "linux")]
#[test]
fn the_kernels_listener_routes_an_exec_and_closes_in_another_user_namespace() {
    const MUST_HAPPEN: std::time::Duration = std::time::Duration::from_secs(60);
    let ns = |what: &str| std::fs::read_link(format!("/proc/self/ns/{what}")).ok();
    // The initial namespaces' inode numbers (PROC_USER_INIT_INO, PROC_PID_INIT_INO).
    let initial = ns("user").is_some_and(|l| l.to_string_lossy() == "user:[4026531837]")
        && ns("pid").is_some_and(|l| l.to_string_lossy() == "pid:[4026531836]");
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut nums = release.split(|c: char| !c.is_ascii_digit());
    let version = (
        nums.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0),
        nums.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(0),
    );
    // SAFETY: getsid(0) reads this process's session.
    let session = unsafe { libc::getsid(0) };
    let route = route(session).expect("a route");
    let start = Instant::now();
    while route.state() == State::Proving {
        assert!(start.elapsed() < MUST_HAPPEN, "hang: the proof never ended");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let state = route.state();
    println!("L2-REAL kernel={release:?} initial={initial} state={state:?}");
    if version < (6, 6) {
        // Before 6.6 joining the group needs CAP_NET_ADMIN, in any namespace: `bind` without
        // it, and what a privileged run proves, are the host's.
        return;
    }
    if !initial {
        assert_eq!(state, State::Closed(Closed::NoAck), "another namespace");
        return;
    }
    assert_eq!(state, State::Live { masked: true });
    let sleep = ["/usr/bin/sleep", "/bin/sleep"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("a sleep");
    let mut child = crate::spawnfd::command(sleep)
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = i32::try_from(child.id()).unwrap();
    let mut events = Vec::new();
    let heard = loop {
        let mut fds = [libc::pollfd {
            fd: route.fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one pollfd, live for the call.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, 100) };
        route.drain(&mut events);
        if let Some(Event::Exec(x)) = events
            .iter()
            .find(|e| matches!(e, Event::Exec(x) if x.pid == pid))
        {
            break x.clone();
        }
        if start.elapsed() >= MUST_HAPPEN {
            let _ = child.kill();
            let _ = child.wait();
            panic!("hang: the exec of {pid} never came");
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    println!(
        "L2-REAL heard pid={pid} image={:?} file={}",
        heard.image.path.file_name(),
        heard.image.file.is_some()
    );
    assert_eq!(
        heard.image.path.file_name().and_then(|n| n.to_str()),
        Some("sleep")
    );
    assert!(heard.image.file.is_some(), "a live child's file reads");
    assert!(heard.at_ns >= heard.start_ns, "its exec follows its start");
}
