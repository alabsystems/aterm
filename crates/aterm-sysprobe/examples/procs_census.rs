// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stats deck A4, step N1, the process measurements (`docs/DESIGN-stats-deck-instruments-2026-11-06.md`
//! §6.5 and §16):
//!
//! - **M-ZOMB**: whether the sweep every whole reading takes ([`aterm_sysprobe::processes_keeping`],
//!   with the options the reading passes) reads every process's status and parent, other users'
//!   included, checked against the kernel's table (`KERN_PROC_ALL`) read just before and just after
//!   it; what that sweep costs; and what the machine-wide zombie count adds on top of it (the
//!   grouping `note_zombies` does, over the sweep's rows).
//! - **M-DSTATE**: the own-thread uninterruptible-wait census (§6.5) on its own utility-QoS worker,
//!   stopping at 5 ms of thread time and resuming at the next process: its thread time a pass, how
//!   many passes cover every own process, and the probe thread's reading cost with and without it,
//!   in alternating windows.
//!
//! The process count is raised with this process's own idle children: `/bin/sleep`, started in the
//! background band, each ending by itself within [`KID_LIFE_S`] if this process dies, and a handful
//! of deliberate zombies (`/usr/bin/true`, waited on without a reap and reaped at the level's end).
//! A raise stops at whichever comes first: [`KIDS_MAX`] children, a third of
//! `kern.maxprocperuid` in children, or a third of it in this user's whole process count.
//! Each raised level is held at most [`HOLD_CAP`]; every child is killed and reaped by this process,
//! by pid, and each pid is read again afterwards to show none is left.
//!
//! `procs_census --out <dir> [--levels 0,1450,2000,0] [--zombies 6] [--no-cadence]
//! [--window-secs 10] [--spin-control] [--budget-rule fixed|margin] [--quick]`
//! writes `levels.jsonl`, `sys.jsonl`, `sweeps.jsonl`, `missing.jsonl`, `passes.jsonl`,
//! `readings.jsonl` and `summary.txt` in `<dir>`.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("procs_census: a macOS measurement (libproc thread states and the BSD sweep)");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    std::process::exit(mac::run());
}

#[cfg(target_os = "macos")]
mod mac {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::ffi::{c_int, c_void};
    use std::fmt::Write as _;
    use std::fs::File;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{RecvTimeoutError, sync_channel};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use aterm_sysprobe::coalitions::CoalitionIds;
    use aterm_sysprobe::jobs::RunState;
    use aterm_sysprobe::{ProcessReading, SweepOpts, kproc, processes_keeping, task_room};
    use aterm_uds::spawnfd::{Child, Stdio, command};

    /// The longest a raised level is held (the unit's rule: two minutes at most).
    const HOLD_CAP: Duration = Duration::from_secs(115);
    /// A sleeping child's own end, seconds, should this process die before it kills them.
    const KID_LIFE_S: u32 = 180;
    /// The census's thread-time budget a pass (§6.5).
    const BUDGET_NS: u64 = 5_000_000;
    /// The census's cadence (§6.5).
    const CADENCE: Duration = Duration::from_secs(30);
    /// The stand-in probe thread's reading gap.
    const TICK: Duration = Duration::from_millis(250);
    /// The most children this process starts, whatever the per-user limit allows.
    const KIDS_MAX: usize = 1_500;
    /// The threads that start the sleepers.
    const SPAWNERS: usize = 8;
    /// `PROC_UID_ONLY` (`libproc.h`): the processes whose effective uid is the one named, the
    /// credential `proc_pidinfo`'s same-user check compares.
    const PROC_UID_ONLY: u32 = 4;

    unsafe extern "C" {
        /// libproc's `proc_listpids` (`libproc.h`); the first-party libc does not declare it.
        fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut c_void, buffersize: c_int)
        -> c_int;
    }

    struct Plan {
        out: PathBuf,
        levels: Vec<u64>,
        zombies: usize,
        cadence: bool,
        sweeps: usize,
        zomb_block: Duration,
        window: Duration,
        windows_max: usize,
        cadence_hold: Duration,
        /// A third window kind: the worker spins 5 ms of thread time instead of a census pass,
        /// the CPU share of a pass with none of its kernel reads.
        spin: bool,
        /// The census stops before a process whose read could take the pass past the budget
        /// (the most one process's read has cost so far), not after the one that does.
        margin: bool,
    }

    fn plan() -> Result<Plan, String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let value = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let quick = args.iter().any(|a| a == "--quick");
        let out = PathBuf::from(value("--out").ok_or("--out <dir> is required")?);
        let levels = match value("--levels") {
            Some(l) => l
                .split(',')
                .map(|n| {
                    n.trim()
                        .parse::<u64>()
                        .map_err(|e| format!("--levels {n}: {e}"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            None if quick => vec![0, 0],
            None => vec![0, 1_450, 2_000, 0],
        };
        let zombies = match value("--zombies") {
            Some(n) => n.parse::<usize>().map_err(|e| format!("--zombies: {e}"))?,
            None if quick => 2,
            None => 6,
        }
        .min(16);
        Ok(Plan {
            out,
            levels,
            zombies,
            cadence: !args.iter().any(|a| a == "--no-cadence"),
            sweeps: if quick { 10 } else { 100 },
            zomb_block: Duration::from_secs(if quick { 3 } else { 20 }),
            window: Duration::from_secs(match value("--window-secs") {
                Some(n) => n
                    .parse::<u64>()
                    .map_err(|e| format!("--window-secs: {e}"))?
                    .clamp(1, 30),
                None if quick => 2,
                None => 10,
            }),
            windows_max: if quick { 3 } else { 18 },
            cadence_hold: Duration::from_secs(if quick { 8 } else { 95 }),
            spin: args.iter().any(|a| a == "--spin-control"),
            margin: match value("--budget-rule").as_deref() {
                None | Some("fixed") => false,
                Some("margin") => true,
                Some(other) => return Err(format!("--budget-rule {other}: fixed or margin")),
            },
        })
    }

    pub(crate) fn run() -> i32 {
        let plan = match plan() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("procs_census: {e}");
                return 2;
            }
        };
        match measure(&plan) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("procs_census: {e}");
                1
            }
        }
    }

    // ---------------------------------------------------------------- clocks and statistics

    /// This thread's CPU time, nanoseconds (`CLOCK_THREAD_CPUTIME_ID`).
    fn thread_ns() -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a writable timespec; the clock id is a constant macOS defines.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts) };
        if rc != 0 {
            return 0;
        }
        u64::try_from(ts.tv_sec)
            .unwrap_or(0)
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::try_from(ts.tv_nsec).unwrap_or(0))
    }

    /// This thread at utility QoS, the class aterm's vitals lane runs its readings at
    /// (`crate::qos::Role::Background` in aterm-gui) and the census's worker is to run at (§6.5).
    fn set_utility() {
        // SAFETY: changes only the calling thread's QoS class.
        let _ =
            unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0) };
    }

    fn ms_since(epoch: Instant, at: Instant) -> f64 {
        at.saturating_duration_since(epoch).as_secs_f64() * 1e3
    }

    fn us(d: Duration) -> f64 {
        d.as_secs_f64() * 1e6
    }

    #[derive(Clone, Copy, Default)]
    struct Stats {
        n: usize,
        p50: f64,
        p95: f64,
        max: f64,
    }

    /// Nearest-rank percentiles.
    fn stats(values: &[f64]) -> Stats {
        let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
        if v.is_empty() {
            return Stats::default();
        }
        v.sort_by(f64::total_cmp);
        let rank = |q: f64| {
            let n = v.len() as f64;
            let i = ((q * n).ceil() as usize).clamp(1, v.len()) - 1;
            v[i]
        };
        Stats {
            n: v.len(),
            p50: rank(0.50),
            p95: rank(0.95),
            max: v[v.len() - 1],
        }
    }

    fn fmt_stats(s: Stats, unit: &str, scale: f64) -> String {
        format!(
            "n={} p50={:.3}{unit} p95={:.3}{unit} max={:.3}{unit}",
            s.n,
            s.p50 * scale,
            s.p95 * scale,
            s.max * scale
        )
    }

    // ---------------------------------------------------------------- the machine's state

    #[derive(Clone, Default)]
    struct Load {
        label: String,
        load1: f64,
        busy: f64,
        thermal: String,
        procs: i64,
    }

    /// `aterm ctl @self sys --json`'s overview: load1, busy cores and the thermal state, kept
    /// whole in `sys.jsonl` beside the label.
    fn sys_snapshot(label: &str, epoch: Instant, out: &mut File) -> Load {
        let at = ms_since(epoch, Instant::now());
        let read = command("aterm")
            .args(["ctl", "@self", "sys", "--json"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let text = read
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let body = text.find('{').map(|i| text[i..].trim()).unwrap_or("");
        let value = aterm_json::from_str::<aterm_json::Value>(body).ok();
        let num = |p: &str| {
            value
                .as_ref()
                .and_then(|v| v.pointer(p))
                .and_then(aterm_json::Value::as_i64)
        };
        let load = Load {
            label: label.to_owned(),
            load1: num("/global/load_m/0").map_or(f64::NAN, |m| m as f64 / 1e3),
            busy: num("/global/cpu_mc").map_or(f64::NAN, |m| m as f64 / 1e3),
            thermal: value
                .as_ref()
                .and_then(|v| v.pointer("/global/thermal"))
                .and_then(aterm_json::Value::as_str)
                .unwrap_or("unread")
                .to_owned(),
            procs: num("/global/procs").unwrap_or(-1),
        };
        let _ = writeln!(
            out,
            "{{\"label\":\"{label}\",\"t_ms\":{at:.1},\"load1\":{:.2},\"busy_cores\":{:.2},\"thermal\":\"{}\",\"procs\":{},\"sys\":{}}}",
            load.load1,
            load.busy,
            load.thermal,
            load.procs,
            if value.is_some() { body } else { "null" }
        );
        load
    }

    fn fmt_load(l: &Load) -> String {
        format!(
            "{} load1={:.2} busy={:.2} thermal={} procs={}",
            l.label, l.load1, l.busy, l.thermal, l.procs
        )
    }

    // ---------------------------------------------------------------- this process's children

    #[derive(Default)]
    struct Kids {
        sleepers: Vec<Child>,
        zombies: Vec<Child>,
        pids: Vec<u32>,
        ended: bool,
    }

    struct Ended {
        killed: usize,
        reaped: usize,
        left: Vec<u32>,
    }

    impl Kids {
        /// `n` sleepers, started from [`SPAWNERS`] threads at once: on a loaded desk one spawn
        /// took 144 ms (59 in 8.5 s at load1 69, the smoke run), so one thread could not reach
        /// the level inside its hold.
        fn sleep(&mut self, n: usize) -> std::io::Result<()> {
            let life = KID_LIFE_S.to_string();
            let share = n.div_ceil(SPAWNERS);
            let batches: Vec<(Vec<Child>, Option<std::io::Error>)> = std::thread::scope(|s| {
                let handles: Vec<_> = (0..SPAWNERS)
                    .map(|t| {
                        let life = &life;
                        let count = share.min(n.saturating_sub(t * share));
                        s.spawn(move || {
                            let mut made = Vec::with_capacity(count);
                            for _ in 0..count {
                                match command("/bin/sleep")
                                    .arg(life)
                                    .stdin(Stdio::null())
                                    .stdout(Stdio::null())
                                    .stderr(Stdio::null())
                                    .spawn()
                                {
                                    Ok(child) => made.push(child),
                                    Err(e) => return (made, Some(e)),
                                }
                            }
                            (made, None)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap_or_else(|_| (Vec::new(), None)))
                    .collect()
            });
            let mut failed = None;
            for (made, error) in batches {
                for child in made {
                    self.pids.push(child.id());
                    self.sleepers.push(child);
                }
                failed = failed.or(error);
            }
            failed.map_or(Ok(()), Err)
        }

        /// Waits until every sleeper has run its exec and its one thread waits in the kernel, or
        /// `limit` passes: a sleeper started in the background band on a saturated desk stays
        /// runnable until it is given a core, and so adds to the run queue (run 1 saw load1 rise
        /// from 26 to 73-146 while 650-1,300 of them started). Answers how many settled and when.
        fn settle(&self, limit: Duration) -> (usize, Duration) {
            let start = Instant::now();
            let mut handles = Vec::new();
            loop {
                let settled = self
                    .sleepers
                    .iter()
                    .filter(|c| {
                        let pid = c.id();
                        kproc::record(pid).is_some_and(|r| r.comm == "sleep")
                            && c_int::try_from(pid)
                                .ok()
                                .is_some_and(|p| all_waiting(p, &mut handles))
                    })
                    .count();
                if settled == self.sleepers.len() || start.elapsed() >= limit {
                    return (settled, start.elapsed());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }

        /// `n` zombies: each started, then each waited on until it exits, without a reap.
        fn zombies(&mut self, n: usize) -> std::io::Result<Vec<u32>> {
            let mut made = Vec::new();
            let start = self.zombies.len();
            for _ in 0..n {
                let child = command("/usr/bin/true")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;
                self.pids.push(child.id());
                made.push(child.id());
                self.zombies.push(child);
            }
            // Exited, not reaped: a zombie under this process until `end`.
            for child in &mut self.zombies[start..] {
                child.wait_exited()?;
            }
            Ok(made)
        }

        /// Kills every sleeper, reaps it and every zombie, then reads each pid again: one whose
        /// kernel record still names this process as its parent is left.
        fn end(&mut self) -> Ended {
            self.ended = true;
            let mut killed = 0;
            let mut reaped = 0;
            for c in &mut self.sleepers {
                if c.kill().is_ok() {
                    killed += 1;
                }
            }
            for c in self.sleepers.iter_mut().chain(self.zombies.iter_mut()) {
                if c.wait().is_ok() {
                    reaped += 1;
                }
            }
            let me = std::process::id();
            let left = self
                .pids
                .iter()
                .copied()
                .filter(|pid| kproc::record(*pid).is_some_and(|r| r.ppid == me))
                .collect();
            Ended {
                killed,
                reaped,
                left,
            }
        }
    }

    impl Drop for Kids {
        fn drop(&mut self) {
            if !self.ended {
                let _ = self.end();
            }
        }
    }

    // ---------------------------------------------------------------- M-ZOMB

    /// `note_zombies`' bookkeeping (`crates/aterm-vitals/src/jobs/units.rs`), over the sweep's
    /// rows: every zombie by pid with its first sighting and parent, every parent holding one.
    #[derive(Default)]
    struct ZombieBook {
        zombies: HashMap<u32, (u64, u32)>,
        parents: HashMap<u32, u64>,
    }

    impl ZombieBook {
        /// One reading's grouping: the book updated, and the zombies by parent.
        fn note(&mut self, at: u64, sweep: &[ProcessReading]) -> BTreeMap<u32, u32> {
            let mut live: HashMap<u32, u32> = HashMap::new();
            for p in sweep.iter().filter(|p| p.state == RunState::Zombie) {
                live.insert(p.pid, p.ppid);
            }
            self.zombies
                .retain(|pid, (_, ppid)| live.get(pid).is_some_and(|pp| pp == ppid));
            for (pid, ppid) in &live {
                self.zombies.entry(*pid).or_insert((at, *ppid));
                self.parents.entry(*ppid).or_insert(at);
            }
            let mut by_parent: BTreeMap<u32, u32> = BTreeMap::new();
            for ppid in live.values() {
                *by_parent.entry(*ppid).or_default() += 1;
            }
            self.parents.retain(|p, _| by_parent.contains_key(p));
            by_parent
        }
    }

    /// The sweep held to the kernel's table read just before and just after it.
    #[derive(Default)]
    struct Coverage {
        /// Processes in both tables, same start, parent and zombie flag: each must be a row.
        stable: usize,
        missing: Vec<(u32, u32, String)>,
        ppid_mismatch: usize,
        state_mismatch: usize,
        uid_mismatch: usize,
        rows: usize,
        unknown_state: usize,
        uid_none: usize,
        root: usize,
        windowserver: usize,
        own: usize,
        other: usize,
        other_uids: usize,
        zombies: usize,
        zombie_other_users: usize,
        my_zombies: usize,
        states: BTreeMap<&'static str, usize>,
        /// Whether the tables hold pid 0 (`kernel_task`), which the sweep skips by design.
        pid0: bool,
    }

    fn safe_name(name: &str) -> String {
        name.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._-+".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn coverage(
        before: &[kproc::Record],
        after: &[kproc::Record],
        sweep: &[ProcessReading],
        uid: u32,
        my_zombies: &BTreeSet<u32>,
    ) -> Coverage {
        let after_by: HashMap<u32, &kproc::Record> = after.iter().map(|r| (r.pid, r)).collect();
        let rows: HashMap<u32, &ProcessReading> = sweep.iter().map(|p| (p.pid, p)).collect();
        let mut c = Coverage {
            rows: sweep.len(),
            ..Coverage::default()
        };
        for b in before {
            if b.pid == 0 {
                c.pid0 = true;
                continue;
            }
            let Some(a) = after_by.get(&b.pid) else {
                continue;
            };
            let same = a.start_s == b.start_s
                && a.start_us == b.start_us
                && a.ppid == b.ppid
                && a.zombie == b.zombie
                && a.uid == b.uid;
            if !same {
                continue;
            }
            c.stable += 1;
            let Some(row) = rows.get(&b.pid) else {
                c.missing.push((b.pid, b.uid, safe_name(&b.comm)));
                continue;
            };
            if row.ppid != b.ppid {
                c.ppid_mismatch += 1;
            }
            if (row.state == RunState::Zombie) != b.zombie {
                c.state_mismatch += 1;
            }
            if row.uid != Some(b.uid) {
                c.uid_mismatch += 1;
            }
        }
        let mut others = BTreeSet::new();
        for p in sweep {
            *c.states.entry(p.state.word()).or_default() += 1;
            if p.state == RunState::Unknown {
                c.unknown_state += 1;
            }
            match p.uid {
                None => c.uid_none += 1,
                Some(0) => c.root += 1,
                Some(88) => c.windowserver += 1,
                Some(u) if u == uid => c.own += 1,
                Some(u) => {
                    c.other += 1;
                    others.insert(u);
                }
            }
            if p.state == RunState::Zombie {
                c.zombies += 1;
                if p.uid != Some(uid) {
                    c.zombie_other_users += 1;
                }
                if my_zombies.contains(&p.pid) && p.ppid == std::process::id() {
                    c.my_zombies += 1;
                }
            }
        }
        c.other_uids = others.len();
        c
    }

    #[derive(Default)]
    struct ZombRow {
        sweep_wall_us: f64,
        sweep_thread_us: f64,
        group_us: f64,
        group_thread_us: f64,
        table_us: f64,
        rows: usize,
        cov: Coverage,
    }

    fn zomb_block(
        level: usize,
        plan: &Plan,
        epoch: Instant,
        uid: u32,
        my_zombies: &BTreeSet<u32>,
        sweeps: &mut File,
        missing: &mut File,
    ) -> Vec<ZombRow> {
        std::thread::scope(|s| {
            s.spawn(|| {
                set_utility();
                let mut out = Vec::new();
                let mut ids = CoalitionIds::default();
                let mut book = ZombieBook::default();
                let deadline = Instant::now() + plan.zomb_block;
                for i in 0..plan.sweeps {
                    if Instant::now() >= deadline {
                        break;
                    }
                    let t0 = Instant::now();
                    let before = kproc::records().unwrap_or_default();
                    let table_us = us(t0.elapsed());
                    let th0 = thread_ns();
                    let w0 = Instant::now();
                    // The options a whole reading passes (`vitals_host.rs`'s `read`) off a split
                    // tick: every process's pid version where its start is unread.
                    let sweep = processes_keeping(
                        SweepOpts {
                            coalitions: false,
                            versions: true,
                        },
                        &mut ids,
                    );
                    let sweep_wall_us = us(w0.elapsed());
                    let sweep_thread_us = (thread_ns().saturating_sub(th0)) as f64 / 1e3;
                    let after = kproc::records().unwrap_or_default();
                    let procs = sweep.map(|s| s.procs).unwrap_or_default();
                    let at = ms_since(epoch, Instant::now()) as u64;
                    let g0 = Instant::now();
                    let gt0 = thread_ns();
                    let by_parent = book.note(at, &procs);
                    let group_thread_us = thread_ns().saturating_sub(gt0) as f64 / 1e3;
                    let group_us = us(g0.elapsed());
                    let cov = coverage(&before, &after, &procs, uid, my_zombies);
                    let biggest = by_parent.iter().max_by_key(|(_, n)| **n);
                    let states: Vec<String> =
                        cov.states.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
                    let _ = writeln!(
                        sweeps,
                        "{{\"level\":{level},\"i\":{i},\"t_ms\":{:.1},\"rows\":{},\"sweep_wall_us\":{sweep_wall_us:.1},\"sweep_thread_us\":{sweep_thread_us:.1},\"group_us\":{group_us:.2},\"group_thread_us\":{group_thread_us:.2},\"table_us\":{table_us:.1},\"table_rows\":{},\"stable\":{},\"missing\":{},\"ppid_mismatch\":{},\"state_mismatch\":{},\"uid_mismatch\":{},\"unknown_state\":{},\"uid_none\":{},\"root\":{},\"windowserver\":{},\"own\":{},\"other\":{},\"other_uids\":{},\"zombies\":{},\"zombie_parents\":{},\"zombie_other_users\":{},\"my_zombies\":{},\"biggest_pile\":[{},{}],\"pid0_in_table\":{},\"states\":{{{}}}}}",
                        ms_since(epoch, t0),
                        procs.len(),
                        before.len(),
                        cov.stable,
                        cov.missing.len(),
                        cov.ppid_mismatch,
                        cov.state_mismatch,
                        cov.uid_mismatch,
                        cov.unknown_state,
                        cov.uid_none,
                        cov.root,
                        cov.windowserver,
                        cov.own,
                        cov.other,
                        cov.other_uids,
                        cov.zombies,
                        by_parent.len(),
                        cov.zombie_other_users,
                        cov.my_zombies,
                        biggest.map_or(0, |(p, _)| *p),
                        biggest.map_or(0, |(_, n)| *n),
                        cov.pid0,
                        states.join(","),
                    );
                    for (pid, puid, name) in &cov.missing {
                        let gone = kproc::record(*pid).is_none();
                        let _ = writeln!(
                            missing,
                            "{{\"level\":{level},\"i\":{i},\"pid\":{pid},\"uid\":{puid},\"name\":\"{name}\",\"gone_after\":{gone}}}"
                        );
                    }
                    out.push(ZombRow {
                        sweep_wall_us,
                        sweep_thread_us,
                        group_us,
                        group_thread_us,
                        table_us,
                        rows: procs.len(),
                        cov,
                    });
                    std::thread::sleep(Duration::from_millis(50));
                }
                out
            })
            .join()
            .unwrap_or_default()
        })
    }

    // ---------------------------------------------------------------- M-DSTATE

    /// This user's processes (`PROC_UID_ONLY`), ascending.
    fn own_pids(uid: u32, pids: &mut Vec<c_int>) {
        // SAFETY: a null buffer asks for the bytes the list needs.
        let need = unsafe { proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
        let slots = usize::try_from(need).unwrap_or(0) / 4 + 256;
        pids.clear();
        pids.resize(slots, 0);
        let bytes = c_int::try_from(slots * 4).unwrap_or(c_int::MAX);
        // SAFETY: `pids` is `bytes` writable bytes; the call answers how many it wrote.
        let n = unsafe { proc_listpids(PROC_UID_ONLY, uid, pids.as_mut_ptr().cast(), bytes) };
        pids.truncate(usize::try_from(n).unwrap_or(0) / 4);
        pids.retain(|p| *p > 0);
        pids.sort_unstable();
    }

    /// `pid`'s threads (`PROC_PIDLISTTHREADS`, then `PROC_PIDTHREADINFO` each, as the jobs
    /// probe reads them) and how many are uninterruptible; `None` when the list is refused or
    /// the process is gone.
    fn thread_states(pid: c_int, handles: &mut Vec<u64>) -> Option<(u32, u32)> {
        if handles.len() < 64 {
            handles.resize(64, 0);
        }
        let count = loop {
            let bytes = c_int::try_from(handles.len() * 8).ok()?;
            // SAFETY: `handles` is `bytes` writable bytes; the call answers how many it wrote.
            let n = unsafe {
                libc::proc_pidinfo(
                    pid,
                    aterm_sysprobe::jobs::darwin::PROC_PIDLISTTHREADS,
                    0,
                    handles.as_mut_ptr().cast::<c_void>(),
                    bytes,
                )
            };
            let n = usize::try_from(n).ok().filter(|n| *n > 0)? / 8;
            if n < handles.len() || handles.len() >= 1 << 16 {
                break n.min(handles.len());
            }
            let grown = handles.len() * 2;
            handles.resize(grown, 0);
        };
        let size = std::mem::size_of::<libc::proc_threadinfo>();
        let (mut threads, mut unint) = (0u32, 0u32);
        for &h in &handles[..count] {
            // SAFETY: plain integers and bytes; all-zero is valid.
            let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is `size` writable bytes of the flavor's layout; the argument is
            // the thread's handle from the list.
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    h,
                    (&raw mut info).cast::<c_void>(),
                    c_int::try_from(size).unwrap_or(0),
                )
            };
            if usize::try_from(got).ok() == Some(size) {
                threads += 1;
                if RunState::from_mach_thread(info.pth_run_state) == RunState::Uninterruptible {
                    unint += 1;
                }
            }
        }
        Some((threads, unint))
    }

    /// Whether every one of `pid`'s threads waits interruptibly (Mach `TH_STATE_WAITING`).
    fn all_waiting(pid: c_int, handles: &mut Vec<u64>) -> bool {
        if handles.len() < 8 {
            handles.resize(8, 0);
        }
        let Ok(bytes) = c_int::try_from(handles.len() * 8) else {
            return false;
        };
        // SAFETY: `handles` is `bytes` writable bytes; the call answers how many it wrote.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                aterm_sysprobe::jobs::darwin::PROC_PIDLISTTHREADS,
                0,
                handles.as_mut_ptr().cast::<c_void>(),
                bytes,
            )
        };
        let count = usize::try_from(n).unwrap_or(0) / 8;
        if count == 0 || count >= handles.len() {
            return false;
        }
        let size = std::mem::size_of::<libc::proc_threadinfo>();
        handles[..count].iter().all(|&h| {
            // SAFETY: plain integers and bytes; all-zero is valid.
            let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is `size` writable bytes of the flavor's layout; the argument is
            // the thread's handle from the list.
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    h,
                    (&raw mut info).cast::<c_void>(),
                    c_int::try_from(size).unwrap_or(0),
                )
            };
            usize::try_from(got).ok() == Some(size)
                && RunState::from_mach_thread(info.pth_run_state) == RunState::Sleeping
        })
    }

    #[derive(Clone, Default)]
    struct Pass {
        kind: &'static str,
        start_ms: f64,
        end_ms: f64,
        thread_us: f64,
        wall_us: f64,
        listed: usize,
        read: usize,
        refused: usize,
        threads: u64,
        unint: u64,
        d_procs: usize,
        cover_done: bool,
        passes_in_cover: u32,
        /// The most one process's read cost in this pass, thread time.
        max_proc_us: f64,
        /// Each refused or uninterruptible process, `pid:name:why` (`z` a zombie, `x` gone, `r` refused live; `d` uninterruptible), named from its kernel record after the timed part.
        named: Vec<String>,
    }

    /// The own-thread census (§6.5): this user's processes in pid order from where the last pass
    /// stopped, until the budget of thread time is spent or every one is read.
    struct Census {
        uid: u32,
        cursor: c_int,
        passes_in_cover: u32,
        pids: Vec<c_int>,
        handles: Vec<u64>,
        /// [`Plan::margin`]'s rule.
        margin: bool,
        /// The most one process's read has cost in any pass yet, thread time.
        max_proc_ns: u64,
    }

    impl Census {
        fn new(uid: u32, margin: bool) -> Self {
            Self {
                uid,
                cursor: 0,
                passes_in_cover: 0,
                pids: Vec::new(),
                handles: Vec::new(),
                margin,
                max_proc_ns: 0,
            }
        }

        fn pass(&mut self, kind: &'static str, budget: Option<u64>, epoch: Instant) -> Pass {
            let start = Instant::now();
            let th0 = thread_ns();
            let mut pids = std::mem::take(&mut self.pids);
            own_pids(self.uid, &mut pids);
            let mut i = pids.partition_point(|p| *p < self.cursor);
            let mut marked: Vec<(c_int, char)> = Vec::new();
            let mut p = Pass {
                kind,
                listed: pids.len(),
                ..Pass::default()
            };
            self.passes_in_cover += 1;
            let mut prev = thread_ns();
            let mut max_proc = 0u64;
            loop {
                if i >= pids.len() {
                    p.cover_done = true;
                    p.passes_in_cover = self.passes_in_cover;
                    self.passes_in_cover = 0;
                    self.cursor = 0;
                    break;
                }
                match thread_states(pids[i], &mut self.handles) {
                    Some((threads, unint)) => {
                        p.read += 1;
                        p.threads += u64::from(threads);
                        p.unint += u64::from(unint);
                        if unint > 0 {
                            p.d_procs += 1;
                            marked.push((pids[i], 'd'));
                        }
                    }
                    None => {
                        p.refused += 1;
                        marked.push((pids[i], 'r'));
                    }
                }
                i += 1;
                let now = thread_ns();
                let cost = now.saturating_sub(prev);
                prev = now;
                max_proc = max_proc.max(cost);
                self.max_proc_ns = self.max_proc_ns.max(cost);
                let spent = now.saturating_sub(th0);
                let over = budget.is_some_and(|b| {
                    if self.margin {
                        spent.saturating_add(self.max_proc_ns) > b
                    } else {
                        spent >= b
                    }
                });
                if over && i < pids.len() {
                    self.cursor = pids[i];
                    break;
                }
            }
            self.pids = pids;
            p.thread_us = thread_ns().saturating_sub(th0) as f64 / 1e3;
            p.wall_us = us(start.elapsed());
            p.max_proc_us = max_proc as f64 / 1e3;
            p.start_ms = ms_since(epoch, start);
            p.end_ms = ms_since(epoch, Instant::now());
            p.named = marked
                .into_iter()
                .map(|(pid, why)| {
                    let record = u32::try_from(pid).ok().and_then(kproc::record);
                    let why = match (&record, why) {
                        (_, 'd') => 'd',
                        (None, _) => 'x',
                        (Some(r), _) if r.zombie => 'z',
                        _ => 'r',
                    };
                    let name = record.map_or_else(|| "-".to_owned(), |r| safe_name(&r.comm));
                    format!("{pid}:{name}:{why}")
                })
                .collect();
            p
        }
    }

    fn write_pass(out: &mut File, level: usize, block: &str, p: &Pass) {
        let _ = writeln!(
            out,
            "{{\"level\":{level},\"block\":\"{block}\",\"kind\":\"{}\",\"start_ms\":{:.2},\"end_ms\":{:.2},\"thread_us\":{:.1},\"wall_us\":{:.1},\"listed\":{},\"read\":{},\"refused\":{},\"threads\":{},\"uninterruptible\":{},\"d_procs\":{},\"cover_done\":{},\"passes_in_cover\":{},\"max_proc_us\":{:.1},\"named\":[{}]}}",
            p.kind,
            p.start_ms,
            p.end_ms,
            p.thread_us,
            p.wall_us,
            p.listed,
            p.read,
            p.refused,
            p.threads,
            p.unint,
            p.d_procs,
            p.cover_done,
            p.passes_in_cover,
            p.max_proc_us,
            p.named
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect::<Vec<_>>()
                .join(",")
        );
    }

    /// The stand-in for the probe thread's reading: the readers of the process table a whole
    /// reading takes in aterm-sysprobe (the sweep with the reading's options, every coalition's
    /// counters, and the task room), the part a census could contend with.
    struct Reader {
        uid: u32,
        ids: CoalitionIds,
    }

    #[derive(Clone, Default)]
    struct Reading {
        start_ms: f64,
        end_ms: f64,
        wall_us: f64,
        thread_us: f64,
        window: usize,
        /// The window's kind: `off`, `census` (a budgeted pass handed over at each reading) or
        /// `spin` (5 ms of thread time spun at each reading, [`Plan::spin`]).
        mode: &'static str,
        triggered: bool,
        procs: usize,
    }

    impl Reader {
        fn read(&mut self, epoch: Instant) -> Reading {
            let start = Instant::now();
            let th0 = thread_ns();
            let sweep = processes_keeping(
                SweepOpts {
                    coalitions: false,
                    versions: true,
                },
                &mut self.ids,
            );
            let coalitions = aterm_sysprobe::coalitions::coalitions();
            let room = task_room(self.uid);
            std::hint::black_box((&coalitions, &room));
            Reading {
                start_ms: ms_since(epoch, start),
                end_ms: ms_since(epoch, Instant::now()),
                wall_us: us(start.elapsed()),
                thread_us: thread_ns().saturating_sub(th0) as f64 / 1e3,
                procs: sweep.map_or(0, |s| s.procs.len()),
                ..Reading::default()
            }
        }
    }

    /// The window kinds, in their order.
    const MODES: [&str; 3] = ["off", "census", "spin"];

    enum Cmd {
        Pass,
        Spin,
        Full,
        Quit,
    }

    /// [`BUDGET_NS`] of this thread's time spent on arithmetic alone: a pass's CPU share with
    /// none of its kernel reads.
    fn spin(kind: &'static str, epoch: Instant) -> Pass {
        let start = Instant::now();
        let th0 = thread_ns();
        let mut x = 0u64;
        while thread_ns().saturating_sub(th0) < BUDGET_NS {
            for i in 0..256u64 {
                x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(i));
            }
        }
        std::hint::black_box(x);
        Pass {
            kind,
            start_ms: ms_since(epoch, start),
            end_ms: ms_since(epoch, Instant::now()),
            thread_us: thread_ns().saturating_sub(th0) as f64 / 1e3,
            wall_us: us(start.elapsed()),
            ..Pass::default()
        }
    }

    fn overlapped(r: &Reading, passes: &[Pass]) -> bool {
        passes
            .iter()
            .any(|p| p.start_ms < r.end_ms && p.end_ms > r.start_ms)
    }

    fn write_readings(out: &mut File, level: usize, block: &str, rs: &[Reading], ps: &[Pass]) {
        for r in rs {
            let _ = writeln!(
                out,
                "{{\"level\":{level},\"block\":\"{block}\",\"start_ms\":{:.2},\"wall_us\":{:.1},\"thread_us\":{:.1},\"window\":{},\"mode\":\"{}\",\"triggered\":{},\"overlapped\":{},\"procs\":{}}}",
                r.start_ms,
                r.wall_us,
                r.thread_us,
                r.window,
                r.mode,
                r.triggered,
                overlapped(r, ps),
                r.procs
            );
        }
    }

    /// Alternating windows: off, census, off, census … (with [`Plan::spin`]: off, census, spin,
    /// …). In a census window every reading first hands the census worker a budgeted pass, so the
    /// pass runs beside the reading (the worst case of the 30 s cadence, at 4 a second); in a spin
    /// window the worker spins [`BUDGET_NS`] instead. Then three unbudgeted passes, each a whole
    /// cover.
    fn alternating(
        uid: u32,
        epoch: Instant,
        windows: usize,
        plan: &Plan,
    ) -> (Vec<Reading>, Vec<Pass>) {
        let (window, margin) = (plan.window, plan.margin);
        let cycle = if plan.spin { 3 } else { 2 };
        let passes: Arc<Mutex<Vec<Pass>>> = Arc::new(Mutex::new(Vec::new()));
        let (go, orders) = sync_channel::<Cmd>(1);
        let readings = std::thread::scope(|s| {
            let worker_passes = Arc::clone(&passes);
            s.spawn(move || {
                set_utility();
                let mut census = Census::new(uid, margin);
                while let Ok(cmd) = orders.recv() {
                    let pass = match cmd {
                        Cmd::Pass => census.pass("budgeted", Some(BUDGET_NS), epoch),
                        Cmd::Spin => spin("spin", epoch),
                        Cmd::Full => {
                            census.cursor = 0;
                            census.passes_in_cover = 0;
                            census.pass("full", None, epoch)
                        }
                        Cmd::Quit => break,
                    };
                    if let Ok(mut all) = worker_passes.lock() {
                        all.push(pass);
                    }
                }
            });
            s.spawn(move || {
                set_utility();
                let mut reader = Reader {
                    uid,
                    ids: CoalitionIds::default(),
                };
                let mut out = Vec::new();
                let begin = Instant::now();
                let mut next = begin;
                for w in 0..windows {
                    let mode = MODES[w % cycle];
                    let end = begin + window * u32::try_from(w + 1).unwrap_or(u32::MAX);
                    while next < end {
                        let now = Instant::now();
                        if next > now {
                            std::thread::sleep(next - now);
                        }
                        let triggered = match mode {
                            "census" => go.try_send(Cmd::Pass).is_ok(),
                            "spin" => go.try_send(Cmd::Spin).is_ok(),
                            _ => false,
                        };
                        let mut r = reader.read(epoch);
                        r.window = w;
                        r.mode = mode;
                        r.triggered = triggered;
                        out.push(r);
                        next += TICK;
                    }
                }
                for _ in 0..3 {
                    let _ = go.send(Cmd::Full);
                }
                let _ = go.send(Cmd::Quit);
                out
            })
            .join()
            .unwrap_or_default()
        });
        let passes = passes.lock().map(|p| p.clone()).unwrap_or_default();
        (readings, passes)
    }

    /// The design cadence: the census worker passes every 30 s on its own clock while the
    /// stand-in probe thread reads every 250 ms.
    fn cadence(
        uid: u32,
        epoch: Instant,
        hold: Duration,
        margin: bool,
    ) -> (Vec<Reading>, Vec<Pass>) {
        let passes: Arc<Mutex<Vec<Pass>>> = Arc::new(Mutex::new(Vec::new()));
        let (quit, stop) = sync_channel::<()>(1);
        let readings = std::thread::scope(|s| {
            let worker_passes = Arc::clone(&passes);
            s.spawn(move || {
                set_utility();
                let mut census = Census::new(uid, margin);
                loop {
                    let pass = census.pass("cadence", Some(BUDGET_NS), epoch);
                    if let Ok(mut all) = worker_passes.lock() {
                        all.push(pass);
                    }
                    match stop.recv_timeout(CADENCE) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            });
            s.spawn(move || {
                set_utility();
                let mut reader = Reader {
                    uid,
                    ids: CoalitionIds::default(),
                };
                let mut out = Vec::new();
                let begin = Instant::now();
                let mut next = begin;
                while next < begin + hold {
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    }
                    let mut r = reader.read(epoch);
                    r.mode = "census";
                    out.push(r);
                    next += TICK;
                }
                let _ = quit.send(());
                out
            })
            .join()
            .unwrap_or_default()
        });
        let passes = passes.lock().map(|p| p.clone()).unwrap_or_default();
        (readings, passes)
    }

    // ---------------------------------------------------------------- the run

    struct Files {
        levels: File,
        sys: File,
        sweeps: File,
        missing: File,
        passes: File,
        readings: File,
        summary: File,
    }

    fn create(dir: &Path, name: &str) -> std::io::Result<File> {
        File::create(dir.join(name))
    }

    fn reading_lines(label: &str, rs: &[Reading], ps: &[Pass], s: &mut String) {
        let wall = |f: &dyn Fn(&Reading) -> bool| -> Vec<f64> {
            rs.iter().filter(|r| f(r)).map(|r| r.wall_us).collect()
        };
        let thread = |f: &dyn Fn(&Reading) -> bool| -> Vec<f64> {
            rs.iter().filter(|r| f(r)).map(|r| r.thread_us).collect()
        };
        for mode in MODES {
            let of = |r: &Reading| r.mode == mode;
            if !rs.iter().any(of) {
                continue;
            }
            let _ = writeln!(
                s,
                "  {label} readings, {mode}: wall {}",
                fmt_stats(stats(&wall(&of)), "ms", 1e-3)
            );
            let _ = writeln!(
                s,
                "  {label} readings, {mode}: thread {}",
                fmt_stats(stats(&thread(&of)), "ms", 1e-3)
            );
        }
        let over = |r: &Reading| r.mode != "off" && overlapped(r, ps);
        let _ = writeln!(
            s,
            "  {label} readings beside a pass or a spin: wall {}",
            fmt_stats(stats(&wall(&over)), "ms", 1e-3)
        );
        let mut per: BTreeMap<usize, (&str, Vec<f64>)> = BTreeMap::new();
        for r in rs {
            per.entry(r.window)
                .or_insert((r.mode, Vec::new()))
                .1
                .push(r.wall_us);
        }
        let windows: Vec<String> = per
            .iter()
            .map(|(w, (mode, v))| {
                let st = stats(v);
                format!("w{w}{mode}:p50={:.2}/p95={:.2}", st.p50 / 1e3, st.p95 / 1e3)
            })
            .collect();
        if per.len() > 1 {
            let _ = writeln!(s, "  {label} per window (ms): {}", windows.join(" "));
        }
    }

    fn pass_lines(label: &str, ps: &[Pass], s: &mut String) {
        for kind in ["budgeted", "full", "cadence", "spin"] {
            let of: Vec<&Pass> = ps.iter().filter(|p| p.kind == kind).collect();
            if of.is_empty() {
                continue;
            }
            let thread: Vec<f64> = of.iter().map(|p| p.thread_us).collect();
            let wall: Vec<f64> = of.iter().map(|p| p.wall_us).collect();
            let read: Vec<f64> = of.iter().map(|p| p.read as f64).collect();
            let threads: Vec<f64> = of.iter().map(|p| p.threads as f64).collect();
            let covers: Vec<f64> = of
                .iter()
                .filter(|p| p.cover_done)
                .map(|p| f64::from(p.passes_in_cover))
                .collect();
            let listed: Vec<f64> = of.iter().map(|p| p.listed as f64).collect();
            let refused: usize = of.iter().map(|p| p.refused).sum();
            let unint: u64 = of.iter().map(|p| p.unint).sum();
            let d: usize = of.iter().map(|p| p.d_procs).sum();
            let over = of
                .iter()
                .filter(|p| p.thread_us > BUDGET_NS as f64 / 1e3)
                .count();
            let biggest: Vec<f64> = of.iter().map(|p| p.max_proc_us).collect();
            let _ = writeln!(
                s,
                "  {label} {kind} passes: the costliest process a pass {}",
                fmt_stats(stats(&biggest), "us", 1.0)
            );
            let mut whys: BTreeMap<char, usize> = BTreeMap::new();
            let mut live_refused: BTreeMap<String, usize> = BTreeMap::new();
            let mut dstate: BTreeMap<String, usize> = BTreeMap::new();
            for n in of.iter().flat_map(|p| p.named.iter()) {
                let mut parts = n.rsplitn(2, ':');
                let why = parts.next().and_then(|w| w.chars().next()).unwrap_or('?');
                let name = parts
                    .next()
                    .and_then(|pn| pn.split_once(':'))
                    .map_or("-", |(_, nm)| nm)
                    .to_owned();
                *whys.entry(why).or_default() += 1;
                match why {
                    'r' => *live_refused.entry(name).or_default() += 1,
                    'd' => *dstate.entry(name).or_default() += 1,
                    _ => {}
                }
            }
            let _ = writeln!(
                s,
                "  {label} {kind} passes: marked {whys:?} (z zombie, x gone, r refused live, d uninterruptible); refused live by name {live_refused:?}; uninterruptible by name {dstate:?}"
            );
            let _ = writeln!(
                s,
                "  {label} {kind} passes: thread {}",
                fmt_stats(stats(&thread), "ms", 1e-3)
            );
            let _ = writeln!(
                s,
                "  {label} {kind} passes: wall {}",
                fmt_stats(stats(&wall), "ms", 1e-3)
            );
            let _ = writeln!(
                s,
                "  {label} {kind} passes: own listed p50={:.0} read/pass p50={:.0} threads/pass p50={:.0} refused={refused} uninterruptible_threads={unint} d_procs={d} over_5ms={over} passes_per_cover {}",
                stats(&listed).p50,
                stats(&read).p50,
                stats(&threads).p50,
                fmt_stats(stats(&covers), "", 1.0)
            );
        }
    }

    fn zomb_lines(rows: &[ZombRow], s: &mut String) {
        let col = |f: &dyn Fn(&ZombRow) -> f64| -> Vec<f64> { rows.iter().map(f).collect() };
        let sum = |f: &dyn Fn(&ZombRow) -> usize| -> usize { rows.iter().map(f).sum() };
        let _ = writeln!(
            s,
            "  m-zomb rows {}",
            fmt_stats(stats(&col(&|r| r.rows as f64)), "", 1.0)
        );
        let _ = writeln!(
            s,
            "  m-zomb sweep wall {}",
            fmt_stats(stats(&col(&|r| r.sweep_wall_us)), "ms", 1e-3)
        );
        let _ = writeln!(
            s,
            "  m-zomb sweep thread {}",
            fmt_stats(stats(&col(&|r| r.sweep_thread_us)), "ms", 1e-3)
        );
        let _ = writeln!(
            s,
            "  m-zomb zombie grouping (note_zombies' work) {}",
            fmt_stats(stats(&col(&|r| r.group_us)), "us", 1.0)
        );
        let _ = writeln!(
            s,
            "  m-zomb zombie grouping, thread time {}",
            fmt_stats(stats(&col(&|r| r.group_thread_us)), "us", 1.0)
        );
        let _ = writeln!(
            s,
            "  m-zomb KERN_PROC_ALL table read (the rejected separate census) {}",
            fmt_stats(stats(&col(&|r| r.table_us)), "us", 1.0)
        );
        let _ = writeln!(
            s,
            "  m-zomb coverage over {} sweeps: stable={} missing={} ppid_mismatch={} state_mismatch={} uid_mismatch={} unknown_state={} uid_none={}",
            rows.len(),
            sum(&|r| r.cov.stable),
            sum(&|r| r.cov.missing.len()),
            sum(&|r| r.cov.ppid_mismatch),
            sum(&|r| r.cov.state_mismatch),
            sum(&|r| r.cov.uid_mismatch),
            sum(&|r| r.cov.unknown_state),
            sum(&|r| r.cov.uid_none),
        );
        if let Some(first) = rows.first() {
            let c = &first.cov;
            let states: Vec<String> = c.states.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let _ = writeln!(
                s,
                "  m-zomb first sweep: rows={} root={} windowserver={} own={} other={} (in {} other uids) zombies={} of_other_users={} mine_seen={} pid0_in_table={} states: {}",
                c.rows,
                c.root,
                c.windowserver,
                c.own,
                c.other,
                c.other_uids,
                c.zombies,
                c.zombie_other_users,
                c.my_zombies,
                c.pid0,
                states.join(" ")
            );
        }
        let mine: Vec<f64> = rows.iter().map(|r| r.cov.my_zombies as f64).collect();
        let st = stats(&mine);
        let least = mine.iter().copied().fold(f64::INFINITY, f64::min);
        let _ = writeln!(
            s,
            "  m-zomb deliberate zombies seen per sweep: min={least:.0} max={:.0}",
            st.max
        );
    }

    fn measure(plan: &Plan) -> Result<(), String> {
        std::fs::create_dir_all(&plan.out).map_err(|e| format!("{}: {e}", plan.out.display()))?;
        let mk = |n: &str| create(&plan.out, n).map_err(|e| format!("{n}: {e}"));
        let mut f = Files {
            levels: mk("levels.jsonl")?,
            sys: mk("sys.jsonl")?,
            sweeps: mk("sweeps.jsonl")?,
            missing: mk("missing.jsonl")?,
            passes: mk("passes.jsonl")?,
            readings: mk("readings.jsonl")?,
            summary: mk("summary.txt")?,
        };
        aterm_uds::spawnfd::raise_soft_nofile();
        aterm_uds::spawnfd::start_children_in_background_band();
        // SAFETY: plain reads of this process's ids.
        let uid = unsafe { libc::getuid() };
        let me = std::process::id();
        let epoch = Instant::now();
        let per_user = aterm_sysprobe::jobs::process_limit().unwrap_or(0);
        let room0 = task_room(uid);
        let cap = usize::try_from(per_user / 3).unwrap_or(0).min(KIDS_MAX);
        let mut summary = String::new();
        let _ = writeln!(
            summary,
            "procs_census pid={me} uid={uid} kern.maxprocperuid={per_user} user_procs_max={:?} cap(kids)={cap} procs_now={:?} own_now={:?} maxproc={:?}",
            room0.user_procs_max, room0.procs, room0.user_procs, room0.procs_max
        );
        let _ = writeln!(
            summary,
            "budget={} ms rule={} tick={} ms cadence={} s hold_cap={} s window={} s spin_control={} levels={:?} zombies={}",
            BUDGET_NS / 1_000_000,
            if plan.margin { "margin" } else { "fixed" },
            TICK.as_millis(),
            CADENCE.as_secs(),
            HOLD_CAP.as_secs(),
            plan.window.as_secs(),
            plan.spin,
            plan.levels,
            plan.zombies
        );
        let top = plan.levels.iter().copied().max().unwrap_or(0);
        let mut runs: Vec<(usize, u64, bool)> = plan
            .levels
            .iter()
            .enumerate()
            .map(|(i, t)| (i, *t, false))
            .collect();
        if plan.cadence {
            runs.push((plan.levels.len(), top, true));
        }
        for (level, target, is_cadence) in runs {
            let start = Instant::now();
            let mut kids = Kids::default();
            if target > 0 {
                for _ in 0..2 {
                    let room_now = task_room(uid);
                    let now = room_now.procs.unwrap_or(0);
                    // This user's whole count, every other process of the uid and this level's
                    // zombies included, stays under a third of `kern.maxprocperuid`; an unread
                    // count raises nothing.
                    let user_room = room_now.user_procs.map_or(0, |own| {
                        (per_user / 3)
                            .saturating_sub(own)
                            .saturating_sub(u64::try_from(plan.zombies).unwrap_or(u64::MAX))
                    });
                    let room = cap
                        .saturating_sub(kids.sleepers.len())
                        .min(usize::try_from(user_room).unwrap_or(0));
                    let need = usize::try_from(target.saturating_sub(now))
                        .unwrap_or(0)
                        .min(room);
                    if need == 0 {
                        break;
                    }
                    kids.sleep(need)
                        .map_err(|e| format!("spawning sleepers: {e}"))?;
                }
            }
            let my_zombies: BTreeSet<u32> = kids
                .zombies(plan.zombies)
                .map_err(|e| format!("making zombies: {e}"))?
                .into_iter()
                .collect();
            let spawned_ms = start.elapsed().as_secs_f64() * 1e3;
            let (settled, settle) = kids.settle(Duration::from_secs(40));
            let settle_ms = settle.as_secs_f64() * 1e3;
            let room = task_room(uid);
            let label = if is_cadence {
                format!("L{level}-cadence")
            } else {
                format!("L{level}")
            };
            let _ = writeln!(
                f.levels,
                "{{\"level\":{level},\"label\":\"{label}\",\"target\":{target},\"sleepers\":{},\"zombies\":{},\"spawn_ms\":{spawned_ms:.0},\"settled\":{settled},\"settle_ms\":{settle_ms:.0},\"procs\":{},\"own\":{},\"user_procs_max\":{},\"t_ms\":{:.1}}}",
                kids.sleepers.len(),
                my_zombies.len(),
                room.procs.unwrap_or(0),
                room.user_procs.unwrap_or(0),
                room.user_procs_max.unwrap_or(0),
                ms_since(epoch, start)
            );
            let _ = writeln!(
                summary,
                "\n{label} target={target} sleepers={} zombies={} spawn={spawned_ms:.0}ms settled={settled} in {settle_ms:.0}ms procs={:?} own={:?}",
                kids.sleepers.len(),
                my_zombies.len(),
                room.procs,
                room.user_procs
            );
            let mut loads = vec![sys_snapshot(&format!("{label}-start"), epoch, &mut f.sys)];
            if is_cadence {
                let hold = plan
                    .cadence_hold
                    .min(HOLD_CAP.saturating_sub(start.elapsed() + Duration::from_secs(8)));
                let (rs, ps) = cadence(uid, epoch, hold, plan.margin);
                for p in &ps {
                    write_pass(&mut f.passes, level, "cadence", p);
                }
                write_readings(&mut f.readings, level, "cadence", &rs, &ps);
                loads.push(sys_snapshot(&format!("{label}-end"), epoch, &mut f.sys));
                reading_lines("cadence", &rs, &ps, &mut summary);
                pass_lines("cadence", &ps, &mut summary);
                let over: Vec<f64> = rs
                    .iter()
                    .filter(|r| overlapped(r, &ps))
                    .map(|r| r.wall_us)
                    .collect();
                let clear: Vec<f64> = rs
                    .iter()
                    .filter(|r| !overlapped(r, &ps))
                    .map(|r| r.wall_us)
                    .collect();
                let _ = writeln!(
                    summary,
                    "  cadence readings overlapping a pass: wall {}",
                    fmt_stats(stats(&over), "ms", 1e-3)
                );
                let _ = writeln!(
                    summary,
                    "  cadence readings clear of a pass: wall {}",
                    fmt_stats(stats(&clear), "ms", 1e-3)
                );
            } else {
                let rows = zomb_block(
                    level,
                    plan,
                    epoch,
                    uid,
                    &my_zombies,
                    &mut f.sweeps,
                    &mut f.missing,
                );
                loads.push(sys_snapshot(&format!("{label}-mid"), epoch, &mut f.sys));
                zomb_lines(&rows, &mut summary);
                // As many whole off/on pairs as the hold has room for, leaving room to end.
                let left = HOLD_CAP.saturating_sub(start.elapsed() + Duration::from_secs(10));
                let fit =
                    usize::try_from(left.as_secs() / plan.window.as_secs().max(1)).unwrap_or(0);
                let cycle = if plan.spin { 3 } else { 2 };
                let windows = (fit.min(plan.windows_max) / cycle) * cycle;
                if windows >= cycle {
                    let (rs, ps) = alternating(uid, epoch, windows, plan);
                    for p in &ps {
                        write_pass(&mut f.passes, level, "alternating", p);
                    }
                    write_readings(&mut f.readings, level, "alternating", &rs, &ps);
                    reading_lines("alternating", &rs, &ps, &mut summary);
                    let triggered = rs.iter().filter(|r| r.triggered).count();
                    let on = rs.iter().filter(|r| r.mode == "census").count();
                    let over = rs
                        .iter()
                        .filter(|r| r.mode == "census" && overlapped(r, &ps))
                        .count();
                    let spun = rs.iter().filter(|r| r.mode == "spin").count();
                    let _ = writeln!(
                        summary,
                        "  alternating: {windows} windows of {} s, census readings={on} spin readings={spun} triggered={triggered} census readings overlapping a pass={over}",
                        plan.window.as_secs()
                    );
                    pass_lines("alternating", &ps, &mut summary);
                }
                loads.push(sys_snapshot(&format!("{label}-end"), epoch, &mut f.sys));
            }
            let ended = kids.end();
            let hold = start.elapsed();
            let _ = writeln!(
                f.levels,
                "{{\"level\":{level},\"label\":\"{label}-end\",\"killed\":{},\"reaped\":{},\"left\":{:?},\"hold_s\":{:.1}}}",
                ended.killed,
                ended.reaped,
                ended.left,
                hold.as_secs_f64()
            );
            for l in &loads {
                let _ = writeln!(summary, "  load {}", fmt_load(l));
            }
            let _ = writeln!(
                summary,
                "  ended: killed={} reaped={} left={:?} hold={:.1}s",
                ended.killed,
                ended.reaped,
                ended.left,
                hold.as_secs_f64()
            );
            eprint!("{summary}");
            let _ = f.summary.write_all(summary.as_bytes());
            let _ = f.summary.flush();
            summary.clear();
            // Pause between levels so the next one starts from a settled table.
            std::thread::sleep(Duration::from_secs(5));
        }
        Ok(())
    }
}
