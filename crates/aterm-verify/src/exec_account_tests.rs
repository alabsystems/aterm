// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_sysprobe::{MemberUsage, kproc::Record};

fn process(pid: u32, ppid: u32, pgid: i32) -> Record {
    Record {
        pid,
        ppid,
        pgid,
        tpgid: 0,
        tty: None,
        uid: 0,
        comm: String::new(),
        start_s: 1,
        start_us: pid,
        zombie: false,
    }
}

#[test]
fn descendants_cross_groups_without_booking_a_sibling_slice() {
    let table = [
        process(10, 1, 1),
        process(11, 10, 1),
        process(12, 11, 12),
        process(20, 1, 1),
    ];
    let host = RootHost {
        root: 10,
        birth: Some((1, 10)),
        source: Kernel,
    };
    let mut groups = Vec::new();
    let read = host.read_with(
        &table,
        |group| {
            groups.push(group);
            Some(
                table
                    .iter()
                    .filter(|p| p.pgid == group as i32)
                    .map(|p| MemberUsage {
                        pid: p.pid,
                        start: u64::from(p.pid),
                        cpu_ns: 1_000_000_000,
                        ..MemberUsage::default()
                    })
                    .collect(),
            )
        },
        |pid| Ok(table.iter().find(|p| p.pid == pid).cloned()),
    );
    let Reading::Read(read) = read else {
        panic!("a read: {read:?}");
    };
    assert_eq!(groups, [1, 12]);
    assert_eq!(read.iter().map(|m| m.pid).collect::<Vec<_>>(), [10, 11, 12]);
    let meter = Own::default();
    meter.book(10, &read);
    meter.book(10, &read);
    let clock = Clock::new(None, &host, 10, None);
    assert_eq!(
        meter.usage(&clock, Duration::ZERO).unwrap().cpu,
        Duration::from_secs(3)
    );
}

/// An unread group or root record misses this read; a replaced, gone or never-read root
/// ends the reads. Neither supplies telemetry.
#[test]
fn replacement_or_unread_root_cannot_supply_telemetry() {
    let table = [process(10, 1, 1)];
    let host = |birth| RootHost {
        root: 10,
        birth,
        source: Kernel,
    };
    let born = Some((1, 10));
    assert_eq!(
        host(born).read_with(&table, |_| None, |_| Ok(Some(table[0].clone()))),
        Reading::Missed
    );
    assert_eq!(
        host(born).read_with(
            &table,
            |_| Some(Vec::new()),
            |_| Err(std::io::ErrorKind::Interrupted.into())
        ),
        Reading::Missed
    );
    assert_eq!(
        host(Some((2, 10))).read_with(&table, |_| panic!("replaced root"), |_| Ok(None)),
        Reading::Over
    );
    assert_eq!(
        host(born).read_with(&table, |_| Some(Vec::new()), |_| Ok(None)),
        Reading::Over
    );
    assert_eq!(
        host(born).read_with(&[], |_| panic!("gone root"), |_| Ok(None)),
        Reading::Over
    );
    assert_eq!(
        host(None).read_with(&table, |_| panic!("unborn root"), |_| Ok(None)),
        Reading::Over
    );
}

/// One read of a scripted machine: the root's tree (pid 10, group 1) having used this many
/// seconds of CPU, a process table the kernel did not hand over in time, or a table the
/// root has left.
#[derive(Clone, Copy, Debug)]
enum Step {
    Cpu(u64),
    TableTimeout,
    Gone,
}

/// A machine whose process-table reads answer `steps` in turn, then a table the root left.
#[derive(Debug)]
struct Script {
    steps: std::sync::Mutex<VecDeque<Step>>,
    now: std::sync::Mutex<Step>,
    reads: std::sync::atomic::AtomicUsize,
}

impl Script {
    fn of(steps: &[Step]) -> RootHost<Script> {
        RootHost {
            root: 10,
            birth: Some((1, 10)),
            source: Script {
                steps: std::sync::Mutex::new(steps.iter().copied().collect()),
                now: std::sync::Mutex::new(Step::Gone),
                reads: std::sync::atomic::AtomicUsize::new(0),
            },
        }
    }
}

impl Source for Script {
    fn records(&self) -> Option<Vec<Record>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let step = self.steps.lock().unwrap().pop_front().unwrap_or(Step::Gone);
        *self.now.lock().unwrap() = step;
        match step {
            Step::Cpu(_) => Some(vec![process(10, 1, 1)]),
            Step::TableTimeout => None,
            Step::Gone => Some(Vec::new()),
        }
    }
    fn group(&self, pgid: u32) -> Option<Vec<MemberUsage>> {
        assert_eq!(pgid, 1);
        let Step::Cpu(cpu) = *self.now.lock().unwrap() else {
            panic!("a group is read only from a table that holds it");
        };
        Some(vec![MemberUsage {
            pid: 10,
            start: 1,
            cpu_ns: cpu * 1_000_000_000,
            ..MemberUsage::default()
        }])
    }
    fn record(&self, pid: u32) -> std::io::Result<Option<Record>> {
        Ok(matches!(*self.now.lock().unwrap(), Step::Cpu(_)).then(|| process(pid, 1, 1)))
    }
}

/// What a clock over `steps` read of the tree, each step one read of it, and how many
/// process-table reads it made.
fn sampled(steps: &[Step]) -> (Option<TreeUsage>, usize) {
    let host = Script::of(steps);
    let own = Own::default();
    let mut clock = Clock::new(None, &host, 10, None).owning(&own);
    for _ in steps {
        clock.read_usage();
    }
    let usage = own.usage(&clock, Duration::from_secs(1));
    (
        usage,
        host.source.reads.load(std::sync::atomic::Ordering::SeqCst),
    )
}

/// A process table the kernel holds past its bound is one missed read, not the end of the
/// tree's: the next read books what the tree used since. A reading whose last read missed
/// is unknown, never the figure frozen at the last read that worked. NEGATIVE CONTROLS:
/// the old clock took the first missed read for a host with no reading, stopped reading
/// for good and reported the 1 s it had booked, in both cases; a reading that ends with its
/// root keeps what it read, and one that never read anything stays unknown.
#[test]
fn a_missed_table_read_neither_ends_sampling_nor_freezes_the_figure() {
    let (usage, reads) = sampled(&[Step::Cpu(1), Step::TableTimeout, Step::Cpu(5)]);
    assert_eq!(reads, 3, "sampling went on past the missed read");
    assert_eq!(usage.map(|u| u.cpu), Some(Duration::from_secs(5)));

    let (usage, _) = sampled(&[Step::Cpu(1), Step::TableTimeout]);
    assert_eq!(
        usage.map(|u| u.cpu),
        None,
        "the tree's CPU after its last read is unknown"
    );

    let (usage, _) = sampled(&[Step::Cpu(1), Step::Gone]);
    assert_eq!(usage.map(|u| u.cpu), Some(Duration::from_secs(1)));
    let (usage, _) = sampled(&[Step::TableTimeout, Step::TableTimeout]);
    assert_eq!(usage.map(|u| u.cpu), None);
}

/// A host whose clock reads answer `script` in turn, then [`Reading::Over`], at a time the
/// test moves.
#[derive(Debug)]
struct Turns {
    now: std::sync::Mutex<Instant>,
    script: std::sync::Mutex<VecDeque<Reading>>,
    reads: std::sync::atomic::AtomicUsize,
}

impl Host for Turns {
    fn monotonic(&self) -> Instant {
        *self.now.lock().unwrap()
    }
    fn group_usage(&self, _: u32) -> Option<Vec<MemberUsage>> {
        unreachable!("the clock reads its tree through group_reading")
    }
    fn group_reading(&self, _: u32) -> Reading {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Reading::Over)
    }
    fn idle(&self) -> Option<Duration> {
        None
    }
    fn cores(&self) -> u32 {
        1
    }
}

/// Through its ticks, a clock's missed read, early or at its cadence, marks the reading
/// unknown until a read works, and waits its turn: the next read comes a cadence after the
/// miss, and books what the tree used since. NEGATIVE CONTROL: the old clock ended its reads
/// at the cadence miss and reported the 1 s it had booked.
#[test]
fn a_missed_read_waits_its_turn_and_the_next_one_books_the_tree() {
    let cpu = |s: u64| {
        Reading::Read(vec![MemberUsage {
            pid: 10,
            start: 1,
            cpu_ns: s * 1_000_000_000,
            ..MemberUsage::default()
        }])
    };
    let began = Instant::now();
    let host = Turns {
        now: std::sync::Mutex::new(began),
        script: std::sync::Mutex::new([Reading::Missed, cpu(1), Reading::Missed, cpu(4)].into()),
        reads: std::sync::atomic::AtomicUsize::new(0),
    };
    let own = Own::default();
    let mut clock = Clock::new(None, &host, 10, None).owning(&own);
    let mut at = |ms: u64, reads: usize, cpu: Option<u64>| {
        *host.now.lock().unwrap() = began + Duration::from_millis(ms);
        clock.tick();
        assert_eq!(
            host.reads.load(std::sync::atomic::Ordering::SeqCst),
            reads,
            "{ms} ms"
        );
        assert_eq!(
            own.usage(&clock, Duration::from_millis(ms)).map(|u| u.cpu),
            cpu.map(Duration::from_secs),
            "{ms} ms"
        );
    };
    at(0, 1, None);
    at(25, 2, Some(1));
    at(600, 3, None);
    at(700, 3, None);
    at(1100, 4, Some(4));
}

/// An opt-in measurement, not a timing assertion. The same finite shell workload is run
/// ABBA; wait4 is only the independent measurement oracle, never production accounting.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "A1's interleaved local accounting experiment"]
fn a1_accounting_abba() {
    use std::io::Write as _;
    let own_cpu = || {
        let me = std::process::id();
        let group = u32::try_from(aterm_sysprobe::kproc::record(me)?.pgid).ok()?;
        aterm_sysprobe::group_usage(group)?
            .into_iter()
            .find(|m| m.pid == me)
            .map(|m| m.cpu_ns)
    };
    let began_wait = Instant::now();
    let need = aterm_capacity::Need {
        cores: 1,
        ..aterm_capacity::Need::new("A1 accounting experiment")
    };
    let claim = aterm_capacity::admit(need, aterm_capacity::Admit::Wait).unwrap();
    let waiting = began_wait.elapsed().as_secs_f64();
    for (take, observe) in [false, true, true, false].into_iter().enumerate() {
        let cpu_before = own_cpu();
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.args([
            "-c",
            "read start; /bin/sh -c 'i=0; while [ $i -lt 1000000 ]; do i=$((i+1)); done' & wait",
        ])
        .stdin(aterm_uds::spawnfd::Stdio::piped());
        let start = Instant::now();
        let mut child = cmd.spawn().unwrap();
        let meter = observe.then(|| UsageMeter::start(child.id()));
        child.stdin.take().unwrap().write_all(b"start\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let usage = child.wait_with_rusage().unwrap();
            let wall = start.elapsed().as_secs_f64();
            let _ = tx.send((usage, wall));
        });
        let mut max_load = 0.0_f64;
        let (oracle, wall) = loop {
            if let Some(load) = aterm_sysprobe::load_average() {
                max_load = max_load.max(load[0]);
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(e) => panic!("waiter: {e}"),
            }
        };
        waiter.join().unwrap();
        let measured = meter.and_then(UsageMeter::finish);
        let observer_cpu = own_cpu()
            .zip(cpu_before)
            .map(|(after, before)| after.saturating_sub(before) as f64 / 1e9);
        println!(
            "A1-ABBA {}",
            aterm_json::json!({
                "take":take+1, "arm":if observe {"B"} else {"A"}, "S":claim.cores(),
                "waiting_s":waiting, "wall_s":wall, "oracle_cpu_s":oracle.cpu.as_secs_f64(),
                "observer_cpu_s":observer_cpu,
                "total_cpu_s":observer_cpu.map(|cpu|cpu + oracle.cpu.as_secs_f64()),
                "sampled_cpu_s":measured.map(|u|u.cpu.as_secs_f64()),
                "starved_s":measured.map(|u|u.starved.as_secs_f64()),
                "processes":measured.map(|u|u.processes), "max_load1":max_load,
            })
        );
        assert!(oracle.status.success());
        if observe {
            assert!(measured.is_some_and(|u| u.cpu > Duration::ZERO && u.processes >= 2));
        }
    }
}
