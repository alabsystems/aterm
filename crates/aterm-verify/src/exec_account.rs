// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release children use the verifier's accounting without changing their process groups,
//! output transport, signal forwarding or wait ownership.

use super::{Clock, Host, LiveHost, Own, TreeUsage};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Sample a child's descendants from spawn until its owner finishes waiting. CPU is a lower
/// bound: short-lived processes between reads and descendants reparented before a read are
/// absent. Unknown telemetry stays unknown. No process is signalled or reaped here.
pub struct UsageMeter {
    stop: Option<mpsc::Sender<()>>,
    reader: Option<JoinHandle<Option<TreeUsage>>>,
}

impl UsageMeter {
    /// Start observing `root`, including descendants in other process groups. Existing group
    /// membership and cancellation custody are unchanged.
    #[must_use]
    pub fn start(root: u32) -> Self {
        let host = RootHost::new(root);
        let own = Own::default();
        if let Some(members) = host.read() {
            own.book(root, &members);
        }
        let (stop, receive) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("release-account".into())
            .spawn(move || {
                let began = Instant::now();
                let mut clock = Clock::new(None, &host, root, None).owning(&own);
                clock.tick();
                while let Err(mpsc::RecvTimeoutError::Timeout) =
                    receive.recv_timeout(Duration::from_millis(20))
                {
                    clock.tick();
                }
                clock.tick();
                own.usage(&clock, began.elapsed())
            })
            .ok();
        Self {
            stop: Some(stop),
            reader,
        }
    }

    /// Stop and join the observer. A failed observer supplies no reading.
    #[must_use]
    pub fn finish(mut self) -> Option<TreeUsage> {
        self.join()
    }

    fn join(&mut self) -> Option<TreeUsage> {
        self.stop.take();
        self.reader.take()?.join().ok().flatten()
    }
}

impl Drop for UsageMeter {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

/// An accounting host rooted in a process rather than a process group. Each read selects
/// descendants from one kernel parent table, reads their groups once each, and filters out
/// unrelated members (including concurrent slices in the same group). The root's birth and
/// each selected member's parent, group and birth bracket those non-atomic reads.
#[derive(Debug)]
struct RootHost {
    root: u32,
    birth: Option<(u64, u32)>,
}

impl RootHost {
    fn new(root: u32) -> Self {
        Self {
            root,
            birth: aterm_sysprobe::kproc::record(root).map(|r| (r.start_s, r.start_us)),
        }
    }

    fn read(&self) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        let records = aterm_sysprobe::kproc::records()?;
        self.read_with(
            &records,
            aterm_sysprobe::group_usage,
            aterm_sysprobe::kproc::record,
        )
    }

    fn read_with(
        &self,
        records: &[aterm_sysprobe::kproc::Record],
        mut read_group: impl FnMut(u32) -> Option<Vec<aterm_sysprobe::MemberUsage>>,
        mut record: impl FnMut(u32) -> Option<aterm_sysprobe::kproc::Record>,
    ) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        let birth = self.birth?;
        let mut by_pid = BTreeMap::new();
        let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for r in records {
            if by_pid.insert(r.pid, r).is_some() {
                return None;
            }
            children.entry(r.ppid).or_default().push(r.pid);
        }
        let root = by_pid.get(&self.root)?;
        if (root.start_s, root.start_us) != birth {
            return None;
        }
        let mut selected = BTreeMap::new();
        let mut groups = BTreeSet::new();
        let mut open = VecDeque::from([self.root]);
        while let Some(pid) = open.pop_front() {
            if selected.len() >= super::TEST_TREE_CAP || selected.contains_key(&pid) {
                return None;
            }
            let r = *by_pid.get(&pid)?;
            groups.insert(u32::try_from(r.pgid).ok()?);
            selected.insert(pid, r);
            if let Some(next) = children.get(&pid) {
                open.extend(next);
            }
        }
        let mut members = Vec::new();
        for group in groups {
            for m in read_group(group)? {
                let Some(before) = selected.get(&m.pid) else {
                    continue;
                };
                let Some(after) = record(m.pid) else {
                    continue;
                };
                if (before.start_s, before.start_us, before.ppid, before.pgid)
                    == (after.start_s, after.start_us, after.ppid, after.pgid)
                {
                    members.push(m);
                }
            }
        }
        let after = record(self.root)?;
        ((after.start_s, after.start_us) == birth).then_some(members)
    }
}

impl Host for RootHost {
    fn group_usage(&self, _: u32) -> Option<Vec<aterm_sysprobe::MemberUsage>> {
        self.read()
    }
    fn idle(&self) -> Option<Duration> {
        None
    }
    fn cores(&self) -> u32 {
        LiveHost.cores()
    }
}

#[cfg(test)]
mod tests {
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
        };
        let mut groups = Vec::new();
        let read = host
            .read_with(
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
                |pid| table.iter().find(|p| p.pid == pid).cloned(),
            )
            .unwrap();
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

    #[test]
    fn replacement_or_unread_root_cannot_supply_telemetry() {
        let table = [process(10, 1, 1)];
        let host = RootHost {
            root: 10,
            birth: Some((1, 10)),
        };
        assert!(
            host.read_with(&table, |_| None, |_| Some(table[0].clone()))
                .is_none()
        );
        let host = RootHost {
            root: 10,
            birth: Some((2, 10)),
        };
        assert!(
            host.read_with(&table, |_| panic!("replaced root"), |_| None)
                .is_none()
        );
        let host = RootHost {
            root: 10,
            birth: Some((1, 10)),
        };
        assert!(
            host.read_with(&table, |_| Some(Vec::new()), |_| None)
                .is_none()
        );
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
            cmd.args(["-c", "read start; /bin/sh -c 'i=0; while [ $i -lt 1000000 ]; do i=$((i+1)); done' & wait"])
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
}
