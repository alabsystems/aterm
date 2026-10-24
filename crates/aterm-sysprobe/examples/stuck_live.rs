// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A live check of the stuck-process engine on this host.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_sysprobe::jobs::{self as probe, AtStart, JobsReader, ProcFacts, ReadReq};
use aterm_sysprobe::{ProcessReading, processes};
use aterm_uds::spawnfd::{Child, Stdio, command};
use aterm_vitals::jobs::{
    Fd, FdKind, HeldProc, JobCounters, JobProc, JobTab, JobTick, Jobs, JobsView, Knote,
    KnoteFilter, LanesSeen, Machine, ProcState, StartWait, SweepProc, TcpState, ThreadState,
    Verdict,
};

const TICK: Duration = Duration::from_secs(2);
const BASE_MS: u64 = 10_000_000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs = args
        .iter()
        .position(|a| a == "--secs")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(24)
        .max(2);
    let hold = !args.iter().any(|a| a == "--no-hold");
    if let Err(e) = run(secs, hold) {
        eprintln!("stuck_live: {e}");
        std::process::exit(1);
    }
}

struct Kids(Vec<(&'static str, Child)>);

impl Drop for Kids {
    fn drop(&mut self) {
        for (_, c) in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(not(unix))]
fn run(_secs: u64, _hold: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "a unix host's check: its calls start in sessions of their own",
    ))
}

#[cfg(unix)]
fn run(secs: u64, hold: bool) -> std::io::Result<()> {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("stuck-live")
        .tempdir()?;
    let fifo = scratch.path().join("nobody-writes.fifo");
    let made = command("/usr/bin/mkfifo").arg(&fifo).status()?;
    if !made.success() {
        return Err(std::io::Error::other(format!("mkfifo failed: {made}")));
    }
    let out = std::fs::File::create(scratch.path().join("busy.out"))?;
    let (pipe_read, _pipe_write) = std::io::pipe()?;

    let mut kids = Kids(Vec::new());
    let mut start = |label: &'static str,
                     program: &str,
                     argv: &[&dyn AsRef<std::ffi::OsStr>],
                     stdin: Stdio,
                     stdout: Stdio|
     -> std::io::Result<u32> {
        let mut c = command(program);
        for a in argv {
            c.arg(a);
        }
        let child = c
            .setsid(true)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::null())
            .spawn()?;
        let pid = child.id();
        kids.0.push((label, child));
        Ok(pid)
    };
    let sleep = start(
        "sleep 60",
        "/bin/sleep",
        &[&"60"],
        Stdio::null(),
        Stdio::null(),
    )?;
    let busy = start(
        "busy loop, printing",
        "/bin/sh",
        &[
            &"-c",
            &"while :; do i=0; while [ $i -lt 20000 ]; do i=$((i+1)); done; echo tick $i; done",
        ],
        Stdio::null(),
        out.into(),
    )?;
    let spin = start(
        "silent spin",
        "/bin/sh",
        &[&"-c", &"while :; do :; done"],
        Stdio::null(),
        Stdio::null(),
    )?;
    let fifo_cat = start(
        "cat < fifo nobody writes",
        "/bin/sh",
        &[&"-c", &"exec cat 0<>\"$1\"", &"sh", &fifo],
        Stdio::null(),
        Stdio::null(),
    )?;
    let pipe_cat = start(
        "cat < pipe nobody writes",
        "/bin/cat",
        &[],
        std::os::fd::OwnedFd::from(pipe_read).into(),
        Stdio::null(),
    )?;
    let labels: BTreeMap<u32, &'static str> = kids.0.iter().map(|(l, c)| (c.id(), *l)).collect();
    println!("this process (the tab's shell): pid {}", std::process::id());
    for (pid, label) in &labels {
        println!("  a{pid:<7} {label}");
    }

    let mut host = Host::new();
    let began = Instant::now();
    let mut said: BTreeMap<String, Verdict> = BTreeMap::new();
    let mut next = began;
    let (tick, rel) = loop {
        next += TICK;
        while Instant::now() < next {
            std::thread::sleep(next - Instant::now());
        }
        let rel = began.elapsed().as_millis() as u64;
        let tick = host.read(BASE_MS + rel)?;
        let view = host.jobs.absorb(&tick);
        let mark = [2_000, 10_000, 20_000, 420_000, 1_200_000]
            .iter()
            .any(|m| rel.abs_diff(*m) < 1_000);
        let at = format!(
            "+{}s, stretch x{}.{:02}",
            (rel + 500) / 1000,
            tick.machine.stretch_milli / 1000,
            tick.machine.stretch_milli % 1000 / 10
        );
        print_changes(&at, &view, &mut said, mark);
        if rel >= secs * 1000 {
            break (tick, rel);
        }
    };

    if hold {
        let blocked = [fifo_cat, pipe_cat];
        let moving = [sleep, busy, spin];
        let mut held = tick;
        held.procs.retain(|p| !moving.contains(&p.pid));
        held.sweep.retain(|p| !moving.contains(&p.pid));
        let mut at = rel;
        for target in [7 * 60_000, 20 * 60_000] {
            while at + 2_000 <= target {
                at += 2_000;
                held.at_ms = BASE_MS + at;
                let view = host.jobs.absorb(&held);
                if at + 2_000 > target {
                    println!(
                        "\nheld to +{} min (the last reading of the two blocked readers, fed on):",
                        target / 60_000
                    );
                    for r in view.rows.iter().filter(|r| blocked.contains(&r.leader.0)) {
                        println!("  {}", r.wire());
                    }
                }
            }
        }
    }
    drop(kids);
    Ok(())
}

fn print_changes(at: &str, view: &JobsView, said: &mut BTreeMap<String, Verdict>, mark: bool) {
    let changed: Vec<_> = view
        .rows
        .iter()
        .filter(|r| mark || said.get(&r.id) != Some(&r.verdict))
        .collect();
    if changed.is_empty() {
        return;
    }
    println!("\n{at}{}:", if mark { "" } else { " (verdict changed)" });
    for r in changed {
        println!("  {}", r.wire());
        said.insert(r.id.clone(), r.verdict);
    }
    for z in view.findings.wire_lines() {
        println!("  {z}");
    }
}

struct Host {
    jobs: Jobs,
    reader: JobsReader,
    tab: JobTab,
    process_limit: Option<u64>,
}

impl Host {
    fn new() -> Self {
        Self {
            jobs: Jobs::new(),
            reader: JobsReader::new(),
            tab: JobTab {
                key: 1,
                number: 1,
                shell_pid: std::process::id(),
                ..JobTab::default()
            },
            process_limit: probe::process_limit(),
        }
    }

    fn read(&mut self, at_ms: u64) -> std::io::Result<JobTick> {
        let sweep = processes().ok_or_else(|| std::io::Error::other("no process sweep"))?;
        let me = std::process::id();
        let self_uid = sweep.procs.iter().find(|p| p.pid == me).and_then(|p| p.uid);
        let sweep: Vec<SweepProc> = sweep.procs.iter().map(sweep_proc).collect();
        let tabs = vec![self.tab.clone()];
        let wants = self.jobs.plan(&sweep, &tabs);
        let reqs: Vec<ReadReq> = wants
            .iter()
            .map(|w| ReadReq {
                pid: w.pid,
                fds: w.fds,
            })
            .collect();
        let procs: Vec<JobProc> = self.reader.read_all(&reqs).iter().map(job_proc).collect();
        let held = procs
            .iter()
            .filter(|p| p.program_check)
            .map(|p| HeldProc {
                pid: p.pid,
                exec: p.start.unwrap_or(0),
                name: p.exe.clone().unwrap_or_else(|| p.name.clone()),
                aterm: true,
                wait: StartWait::ProgramCheck,
            })
            .collect();
        let lanes = self.reader.lanes();
        Ok(JobTick {
            at_ms,
            read_ms: at_ms,
            self_uid,
            sweep,
            procs,
            tabs,
            machine: Machine {
                stretch_milli: stretch_milli(),
                thrashing: false,
            },
            lanes: LanesSeen {
                kqueue_detail: lanes.kqueue_detail.is_on(),
                pipe_flags: lanes.pipe_flags.is_on(),
            },
            process_limit: self.process_limit,
            held,
        })
    }
}

fn stretch_milli() -> u32 {
    #[cfg(target_os = "macos")]
    let text = command("/usr/sbin/sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    #[cfg(not(target_os = "macos"))]
    let text = std::fs::read_to_string("/proc/loadavg").ok();
    let loads: Vec<f64> = text
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|w| w.parse().ok())
        .take(2)
        .collect();
    let worst = loads.iter().copied().fold(0.0_f64, f64::max);
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let per_cpu_milli = worst / cpus as f64 * 1000.0;
    if per_cpu_milli.is_finite() {
        (per_cpu_milli as u32).clamp(1000, 10_000)
    } else {
        1000
    }
}

fn state(s: probe::RunState) -> ProcState {
    match s {
        probe::RunState::Running => ProcState::Running,
        probe::RunState::Sleeping => ProcState::Sleeping,
        probe::RunState::Stopped => ProcState::Stopped,
        probe::RunState::Zombie => ProcState::Zombie,
        probe::RunState::Uninterruptible => ProcState::Uninterruptible,
        probe::RunState::Idle => ProcState::Idle,
        probe::RunState::Unknown => ProcState::Unknown,
    }
}

fn sweep_proc(p: &ProcessReading) -> SweepProc {
    SweepProc {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        uid: p.uid,
        name: p.name.clone(),
        state: state(p.state),
        session_leader: p.session_leader,
        has_tty: p.has_tty,
    }
}

fn tcp(s: probe::TcpState) -> TcpState {
    match s {
        probe::TcpState::Closed => TcpState::Closed,
        probe::TcpState::Listen => TcpState::Listen,
        probe::TcpState::SynSent => TcpState::SynSent,
        probe::TcpState::SynReceived => TcpState::SynReceived,
        probe::TcpState::Established => TcpState::Established,
        probe::TcpState::CloseWait => TcpState::CloseWait,
        probe::TcpState::Closing => TcpState::Closing,
        _ => TcpState::Other,
    }
}

fn filter(f: probe::KnoteFilter) -> KnoteFilter {
    match f {
        probe::KnoteFilter::Read => KnoteFilter::Read,
        probe::KnoteFilter::Write => KnoteFilter::Write,
        probe::KnoteFilter::Timer => KnoteFilter::Timer,
        probe::KnoteFilter::Vnode => KnoteFilter::Vnode,
        probe::KnoteFilter::Proc => KnoteFilter::Proc,
        probe::KnoteFilter::Signal => KnoteFilter::Signal,
        probe::KnoteFilter::User => KnoteFilter::User,
        probe::KnoteFilter::Other(_) => KnoteFilter::Other,
    }
}

fn fd(f: &probe::FdReading) -> Fd {
    let kind = match &f.kind {
        probe::FdKind::File {
            regular,
            size,
            dev,
            ino,
            lf_tail,
            fresh_dylib,
        } => FdKind::File {
            regular: *regular,
            size: *size,
            dev: *dev,
            ino: *ino,
            lf_tail: *lf_tail,
            fresh_dylib: *fresh_dylib,
        },
        probe::FdKind::Device { tty } => FdKind::Device { tty: *tty },
        probe::FdKind::Pipe {
            handle,
            peer,
            status,
            queued,
            write_end,
        } => FdKind::Pipe {
            handle: *handle,
            peer: *peer,
            status: *status,
            queued: *queued,
            write_end: *write_end,
        },
        probe::FdKind::Tcp {
            state,
            remote,
            local_port,
            queued_read,
        } => FdKind::Tcp {
            state: tcp(*state),
            remote: *remote,
            local_port: *local_port,
            queued_read: *queued_read,
        },
        probe::FdKind::Socket => FdKind::Socket,
        probe::FdKind::EventQueue { sleeping, knotes } => FdKind::EventQueue {
            sleeping: *sleeping,
            knotes: knotes.as_ref().map(|ks| {
                ks.iter()
                    .map(|k| Knote {
                        ident: k.ident,
                        filter: filter(k.filter),
                    })
                    .collect()
            }),
        },
        probe::FdKind::Watch => FdKind::Watch,
        probe::FdKind::Other => FdKind::Other,
    };
    Fd { fd: f.fd, kind }
}

fn job_proc(p: &ProcFacts) -> JobProc {
    JobProc {
        pid: p.pid,
        ppid: p.ppid,
        pgid: p.pgid,
        sid: p.sid,
        start: p.start,
        uid: p.uid,
        name: p.name.clone(),
        exe: p.exe.clone(),
        state: state(p.state),
        tty: p.tty,
        tpgid: p.tpgid,
        counters: p.counters.map(|c| JobCounters {
            cpu_ns: c.cpu_ns,
            child_cpu_ns: c.child_cpu_ns,
            syscalls: c.syscalls,
            wakeups: c.wakeups,
            ctx_switches: c.ctx_switches,
            disk_read: c.disk_read,
            disk_written: c.disk_written,
            logical_writes: c.logical_writes,
            runnable_ns: c.runnable_ns,
            pageins: c.pageins,
        }),
        threads: p.threads.as_ref().map(|ts| {
            ts.iter()
                .map(|t| ThreadState {
                    state: state(t.state),
                    wchan: t.wchan.clone(),
                })
                .collect()
        }),
        wchan: p.wchan.clone(),
        fds: p.fds.as_ref().map(|fs| fs.iter().map(fd).collect()),
        program_check: p.program_check() == Some(AtStart::Held),
    }
}
