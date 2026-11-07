// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M-COAL, M-PCLK and M-ECLK (`docs/DESIGN-stats-deck-instruments-2026-11-06.md` §16): the
//! coalition heat words' self-check ([`aterm_sysprobe::coalition_heat`]) and its parse cost on top
//! of the coalition read aterm already makes, and the derived Super and Efficiency clocks against
//! IOReport's CPU Stats, one reading a second.
//!
//! - `coal_clocks run <dir> <phases> [sparse]`: readings into `<dir>/clocks.csv` (`sparse`: only
//!   those under the top Super decile or the high Efficiency band, those the self-check did not
//!   judge, and one a minute), and the machine's own `aterm ctl @self sys --json` (load, busy
//!   cores, thermal state) into `<dir>/sys.jsonl` at the start, every minute and the end, then
//!   every reading's verdict counted, written or not. `<phases>` is
//!   `name:qos:threads:duty:secs,...` with `qos` one of `none`, `ui` (`QOS_CLASS_USER_INTERACTIVE`,
//!   which lands on Super cores) or `bg` (`QOS_CLASS_BACKGROUND`, which lands on Efficiency
//!   cores); each spinner thread spins `duty` of every 10 ms. The spinners are this process's
//!   threads, stopped and joined at each phase's end; a plan whose spinning phases pass 30 minutes,
//!   or one stretch of them 10, is refused, and a spin phase whose kind of core the background
//!   already holds past `mac::GATE` does not spin (its readings are named `<phase>~`).
//! - `coal_clocks analyze <csv>...`: the per-decile and per-band tables of the results file.
//! - `coal_clocks analyze-bench <txt>...`: a bench series by load band.
//! - `coal_clocks bench [n [every_s count]]`: the parse's stages, each timed alone over `n`
//!   passes, and the parse folded into the start counter's merge three ways; `count` times
//!   `every_s` seconds apart, each after the machine's `sys --json` line.
//! - `coal_clocks fold <dir> <secs>`: the parse folded into the start counter, one pass of each
//!   way a reading, timed inside a loop of one reading a second (`<dir>/fold.csv`), with every
//!   reading's self-check verdict counted; `coal_clocks analyze-fold <csv>...` gives the pooled
//!   p50 and p95 by load band.
//! - `coal_clocks record`: one busy coalition's two records a second apart, as test words.
//!
//! The CPU Stats clock is read as the A4 recordings read it: each channel's active residency
//! weighted by its DVFS state's frequency, the state's ordinal into the pmgr `voltage-states`
//! table (`voltage-states1-sram` Efficiency, `voltage-states5-sram` Super), read once from `ioreg`
//! because `cpu_hardware` reads no table on this chip. The complex channels (`PCPU`, `ECPU`)
//! are the recordings' read; the per-core channels, weighted by each core's active residency, are
//! reported beside them.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("analyze") => analyze::main(&args[1..]),
        Some("analyze-bench") => analyze::bench(&args[1..]),
        Some("analyze-fold") => analyze::fold(&args[1..]),
        #[cfg(target_os = "macos")]
        Some("run") => mac::run(&args[1..]),
        #[cfg(target_os = "macos")]
        Some("record") => mac::record(),
        #[cfg(target_os = "macos")]
        Some("bench") => mac::bench(&args[1..]),
        #[cfg(target_os = "macos")]
        Some("fold") => mac::fold(&args[1..]),
        _ => {
            eprintln!(
                "usage: coal_clocks analyze <csv>... | analyze-bench <txt>... | analyze-fold <csv>... | run <dir> <phases> [sparse] \
                 | bench [n [every_s count]] | fold <dir> <secs> | record (the last four on macOS)"
            );
            std::process::exit(2);
        }
    }
}

/// The CSV's columns, in order.
const COLUMNS: &str = "t,ph,l1,pr,sb,eb,win,cpu,q,p,kc,pkc,mj,gmj,po,v,ipc,pm,em,er,eq,eqr,nvp,nve,cw,cp,ce,pa,ea,cpp,cpe,pc,ec,coal,coalc,warm,parse,parsec,n,cz,fz,mv";

/// The columns of `fold`'s CSV, in order.
const FOLD_COLUMNS: &str =
    "t,l1,pr,sb,eb,n,first,v,alone,rows,totals,totals24,a_rows,a_totals,a_totals24,agree";

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_int};
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use aterm_sysprobe::coalition_heat::{self, ClockRange, Heat, HeatRead};
    use aterm_sysprobe::coalitions;
    use aterm_sysprobe::cpu_hardware::{self, State};

    unsafe extern "C" {
        fn notify_register_check(name: *const c_char, token: *mut c_int) -> u32;
        fn notify_get_state(token: c_int, state: *mut u64) -> u32;
    }

    /// The pmgr DVFS tables, MHz: Efficiency, then Super.
    pub(super) struct Tables {
        pub e: Vec<f64>,
        pub p: Vec<f64>,
    }

    fn ioreg_tables() -> Tables {
        let out = aterm_uds::spawnfd::command("/usr/sbin/ioreg")
            .args(["-rd1", "-p", "IODeviceTree", "-n", "pmgr"])
            .output()
            .expect("ioreg runs");
        let text = String::from_utf8_lossy(&out.stdout);
        let table = |key: &str| -> Vec<f64> {
            let line = text
                .lines()
                .find(|l| l.contains(&format!("\"{key}\" = <")))
                .unwrap_or_else(|| panic!("{key} in pmgr"));
            let hex = line
                .split('<')
                .nth(1)
                .and_then(|s| s.split('>').next())
                .expect("hex");
            let bytes: Vec<u8> = (0..hex.len() / 2)
                .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex digit"))
                .collect();
            let raw: Vec<f64> = bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| f64::from(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
                .collect();
            let top = raw.iter().copied().fold(0.0, f64::max);
            let scale = if top > 1e8 { 1e6 } else { 1e3 };
            raw.into_iter().map(|v| v / scale).collect()
        };
        Tables {
            e: table("voltage-states1-sram"),
            p: table("voltage-states5-sram"),
        }
    }

    /// A channel's (active ticks, all ticks, Σ active ticks × MHz) under `table`, by ordinal:
    /// idle states first, then `V{i}P{n-1-i}` (or `P{i}`) for each table row. `None` for another
    /// shape.
    fn weigh(states: &[State], table: &[f64]) -> Option<(f64, f64, f64)> {
        let idle = |n: &str| matches!(n, "IDLE" | "DOWN" | "OFF");
        let offset = states.iter().position(|s| !idle(&s.name))?;
        let active = &states[offset..];
        if active.len() != table.len() {
            return None;
        }
        let n = active.len();
        let named = active
            .iter()
            .enumerate()
            .all(|(i, s)| s.name == format!("V{i}P{}", n - 1 - i) || s.name == format!("P{i}"));
        if !named || states.iter().any(|s| s.ticks < 0) {
            return None;
        }
        let all: f64 = states.iter().map(|s| s.ticks as f64).sum();
        let act: f64 = active.iter().map(|s| s.ticks as f64).sum();
        let weighted: f64 = active
            .iter()
            .zip(table)
            .map(|(s, f)| s.ticks as f64 * f)
            .sum();
        Some((act, all, weighted))
    }

    /// One CPU Stats delta, read as the recordings did.
    #[derive(Default)]
    struct CpuStats {
        window_us: u64,
        p_mhz: Option<f64>,
        e_mhz: Option<f64>,
        p_active: Option<f64>,
        e_active: Option<f64>,
        p_core_mhz: Option<f64>,
        e_core_mhz: Option<f64>,
        p_cores: Option<f64>,
        e_cores: Option<f64>,
    }

    fn cpu_stats(sample: &cpu_hardware::Sample, tables: &Tables) -> CpuStats {
        let mut out = CpuStats {
            window_us: sample.window_ns / 1000,
            ..CpuStats::default()
        };
        let Ok(channels) = &sample.channels else {
            return out;
        };
        let mut cores = [(0.0, 0.0, 0.0); 2];
        for c in channels {
            let (kind, table) = if c.name.starts_with("PCPU") {
                (0, &tables.p)
            } else if c.name.starts_with("ECPU") {
                (1, &tables.e)
            } else {
                continue;
            };
            let Some((act, all, weighted)) = weigh(&c.states, table) else {
                continue;
            };
            let complex = c.subgroup == "CPU Complex Performance States"
                && (c.name == "PCPU" || c.name == "ECPU");
            let core = c.subgroup == "CPU Core Performance States"
                && c.name.len() > 4
                && c.name[4..].bytes().all(|b| b.is_ascii_digit());
            if complex {
                let mhz = (act > 0.0).then(|| weighted / act);
                let active = (all > 0.0).then(|| act / all);
                if kind == 0 {
                    (out.p_mhz, out.p_active) = (mhz, active);
                } else {
                    (out.e_mhz, out.e_active) = (mhz, active);
                }
            } else if core && all > 0.0 {
                let k = &mut cores[kind];
                k.0 += act;
                k.1 += weighted;
                k.2 += act / all;
            }
        }
        let core = |(act, weighted, n): (f64, f64, f64)| {
            ((act > 0.0).then(|| weighted / act), (n > 0.0).then_some(n))
        };
        (out.p_core_mhz, out.p_cores) = core(cores[0]);
        (out.e_core_mhz, out.e_cores) = core(cores[1]);
        out
    }

    fn thread_cpu_ns() -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a writable timespec.
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts) };
        u64::try_from(ts.tv_sec).unwrap_or(0) * 1_000_000_000
            + u64::try_from(ts.tv_nsec).unwrap_or(0)
    }

    fn pressure_token() -> Option<c_int> {
        let mut token = 0;
        // SAFETY: a NUL-terminated name and a writable token.
        let rc = unsafe {
            notify_register_check(
                c"com.apple.system.thermalpressurelevel".as_ptr(),
                &raw mut token,
            )
        };
        (rc == 0).then_some(token)
    }

    fn pressure(token: Option<c_int>) -> Option<u64> {
        let mut state = 0;
        // SAFETY: a token notify_register_check gave, and a writable state.
        (unsafe { notify_get_state(token?, &raw mut state) } == 0).then_some(state)
    }

    /// The machine's own reading: `aterm ctl @self sys --json`'s load, busy cores and thermal
    /// state, as one JSON object.
    fn sys_line(at_s: f64, phase: &str) -> String {
        let out = aterm_uds::spawnfd::command("aterm")
            .args(["ctl", "@self", "sys", "--json"])
            .output();
        let text = out
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let field = |key: &str, close: char| -> String {
            text.find(&format!("\"{key}\":"))
                .map(|i| {
                    let rest = &text[i + key.len() + 3..];
                    let end = rest.find(close).map_or(rest.len(), |e| e + 1);
                    rest[..end].trim_end_matches(',').to_owned()
                })
                .unwrap_or_else(|| "null".to_owned())
        };
        format!(
            "{{\"t\":{at_s:.1},\"ph\":\"{phase}\",\"load_m\":{},\"cpu_mc\":{},\"cpu_by_level_mc\":{},\"thermal\":{}}}",
            field("load_m", ']'),
            field("cpu_mc", ','),
            field("cpu_by_level_mc", ']'),
            field("thermal", ',')
        )
    }

    /// What one reading's parse gave: the change and its judgement, or the first refusal (with
    /// the change, where it was taken), each with the window.
    enum Verdict {
        Judged(coalition_heat::Moved, coalition_heat::Judged, u64),
        Refused(&'static str, Option<coalition_heat::Moved>, u64),
    }

    /// The background busy share at which a spin phase of that kind of core does not spin.
    pub(super) const GATE: f64 = 0.8;

    struct Phase {
        name: String,
        qos: Option<libc::qos_class_t>,
        threads: usize,
        duty: f64,
        secs: u64,
    }

    fn phases(spec: &str) -> Vec<Phase> {
        spec.split(',')
            .map(|p| {
                let f: Vec<&str> = p.split(':').collect();
                assert_eq!(f.len(), 5, "phase {p}: name:qos:threads:duty:secs");
                let qos = match f[1] {
                    "none" => None,
                    "ui" => Some(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE),
                    "bg" => Some(libc::qos_class_t::QOS_CLASS_BACKGROUND),
                    other => panic!("qos {other}"),
                };
                Phase {
                    name: f[0].to_owned(),
                    qos,
                    threads: f[2].parse().expect("threads"),
                    duty: f[3].parse::<f64>().expect("duty").clamp(0.0, 1.0),
                    secs: f[4].parse().expect("secs"),
                }
            })
            .collect()
    }

    /// The plan's bound: spinning phases at most 30 minutes in all, 10 at a stretch.
    fn bounded(plan: &[Phase]) -> bool {
        let spins = |p: &Phase| p.threads > 0 && p.duty > 0.0;
        let total: u64 = plan.iter().filter(|p| spins(p)).map(|p| p.secs).sum();
        let mut stretch = 0;
        let mut longest = 0;
        for p in plan {
            stretch = if spins(p) { stretch + p.secs } else { 0 };
            longest = longest.max(stretch);
        }
        total <= 1800 && longest <= 600
    }

    struct Spinners {
        stop: Arc<AtomicBool>,
        threads: Vec<std::thread::JoinHandle<u64>>,
    }

    fn spin(phase: &Phase) -> Spinners {
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..phase.threads)
            .filter(|_| phase.duty > 0.0)
            .map(|_| {
                let stop = Arc::clone(&stop);
                let (qos, duty) = (phase.qos, phase.duty);
                std::thread::spawn(move || {
                    if let Some(qos) = qos {
                        // SAFETY: sets this thread's own QoS class.
                        unsafe { libc::pthread_set_qos_class_self_np(qos, 0) };
                    }
                    let period = Duration::from_millis(10);
                    let busy = period.mul_f64(duty);
                    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
                    while !stop.load(Ordering::Relaxed) {
                        let start = Instant::now();
                        while start.elapsed() < busy {
                            for _ in 0..256 {
                                x ^= x << 13;
                                x ^= x >> 7;
                                x ^= x << 17;
                            }
                            x = std::hint::black_box(x);
                        }
                        if let Some(rest) = period.checked_sub(start.elapsed()) {
                            std::thread::sleep(rest);
                        }
                    }
                    x
                })
            })
            .collect();
        Spinners { stop, threads }
    }

    impl Spinners {
        fn end(self) {
            self.stop.store(true, Ordering::Relaxed);
            for t in self.threads {
                let _ = t.join();
            }
        }
    }

    /// Super and Efficiency busy shares between two core-tick reads, by the layout's clusters.
    fn busy(
        layout: &aterm_sysprobe::CpuLayout,
        then: &[aterm_sysprobe::CoreTicks],
        now: &[aterm_sysprobe::CoreTicks],
    ) -> [Option<f64>; 2] {
        let share = |i: usize| {
            let cluster = layout.clusters.get(i)?;
            let (mut b, mut t) = (0u64, 0u64);
            for &c in &cluster.logical {
                let (a, z) = (then.get(usize::from(c))?, now.get(usize::from(c))?);
                b += z.busy().saturating_sub(a.busy());
                t += z.total().saturating_sub(a.total());
            }
            (t > 0).then(|| b as f64 / t as f64)
        };
        [share(0), share(1)]
    }

    /// Every decodable record's (cpu ticks, P ticks, cycles, P cycles), summed: the recordings'
    /// naive machine sum, whose change spans coalitions that came and went.
    fn naive(read: &coalitions::RecordRead) -> [u64; 4] {
        let mut s = [0u64; 4];
        for (_, raw) in &read.records {
            if let Ok(h) = coalition_heat::decode(raw) {
                for (o, v) in s
                    .iter_mut()
                    .zip([h.cpu_ticks, h.p_ticks, h.cycles, h.p_cycles])
                {
                    *o = o.wrapping_add(v);
                }
            }
        }
        s
    }

    fn opt(v: Option<f64>, digits: usize) -> String {
        v.map_or_else(String::new, |v| format!("{v:.digits$}"))
    }

    pub(super) fn run(args: &[String]) {
        let (dir, spec, sparse) = match args {
            [dir, spec] => (dir, spec, false),
            [dir, spec, s] if s == "sparse" => (dir, spec, true),
            _ => {
                eprintln!("usage: coal_clocks run <dir> <phases> [sparse]");
                std::process::exit(2);
            }
        };
        let plan = phases(spec);
        assert!(
            bounded(&plan),
            "the plan's spinning passes 30 min, or 10 at a stretch"
        );
        std::fs::create_dir_all(dir).expect("dir");
        let tables = ioreg_tables();
        let range = ClockRange {
            p_mhz: (tables.p[0], tables.p.iter().copied().fold(0.0, f64::max)),
            e_mhz: (tables.e[0], tables.e.iter().copied().fold(0.0, f64::max)),
        };
        let timebase = aterm_sysprobe::procs::kernel_timebase().expect("timebase");
        let layout = aterm_sysprobe::cpu_layout();
        let cores = [0, 1].map(|i| {
            layout
                .clusters
                .get(i)
                .map_or(0.0, |c| c.logical.len() as f64)
        });
        let mut stats = cpu_hardware::Probe::default();
        let gap = Duration::from_millis(2_500);
        let mut csv = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}/clocks.csv"))
            .expect("csv");
        let mut sys = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}/sys.jsonl"))
            .expect("sys");
        let started = Instant::now();
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let mut head = String::new();
        let _ = writeln!(head, "# start_unix_ms={unix_ms} plan={spec}");
        let _ = writeln!(
            head,
            "# layout={:?} timebase={timebase:?}",
            layout
                .clusters
                .iter()
                .map(|c| (c.name.as_str(), c.logical.len()))
                .collect::<Vec<_>>()
        );
        let _ = writeln!(head, "# table_p_mhz={:?}", tables.p);
        let _ = writeln!(head, "# table_e_mhz={:?}", tables.e);
        let first = stats.sample(gap);
        let _ = writeln!(
            head,
            "# os={:?} kernel_release={:?} kernel_version={:?}",
            first.controls.os_version, first.controls.kernel_release, first.controls.kernel_version
        );
        let _ = writeln!(head, "{}", super::COLUMNS);
        csv.write_all(head.as_bytes()).expect("csv head");

        let token = pressure_token();
        let mut ticks = aterm_sysprobe::Probe::new();
        let mut prev_ticks = ticks.core_ticks();
        let (mut heat_then, mut heat_now, mut scratch) = (
            HeatRead::default(),
            HeatRead::default(),
            HeatRead::default(),
        );
        let mut then_at: Option<Instant> = None;
        let mut readings: u64 = 0;
        let mut verdicts: Vec<(String, u64)> = Vec::new();
        let mut prev_naive: Option<[u64; 4]> = None;
        let mut next_sys = Instant::now();
        let mut next = Instant::now() + Duration::from_secs(1);
        // The background: the last ten readings' Super and Efficiency busy shares taken while no
        // spinner of this run ran. A spin phase whose kind of core the background already holds
        // past [`GATE`] cannot place a reading below the top decile, so it does not spin.
        let mut spun_s: u64 = 0;
        let mut background: std::collections::VecDeque<[f64; 2]> =
            std::collections::VecDeque::new();
        for phase in &plan {
            let kind = usize::from(matches!(
                phase.qos,
                Some(libc::qos_class_t::QOS_CLASS_BACKGROUND)
            ));
            let held = {
                let mut v: Vec<f64> = background.iter().map(|b| b[kind]).collect();
                v.sort_by(f64::total_cmp);
                v.get(v.len() / 2).copied()
            };
            let gated = phase.threads > 0 && held.is_none_or(|b| b >= GATE);
            let name = if gated {
                format!("{}~", phase.name)
            } else {
                phase.name.clone()
            };
            let spinners = if gated {
                Spinners {
                    stop: Arc::new(AtomicBool::new(false)),
                    threads: Vec::new(),
                }
            } else {
                spin(phase)
            };
            let spinning = !spinners.threads.is_empty();
            if spinning {
                spun_s += phase.secs;
            }
            let phase_end = Instant::now() + Duration::from_secs(phase.secs);
            while Instant::now() < phase_end {
                if Instant::now() >= next_sys {
                    let line = sys_line(started.elapsed().as_secs_f64(), &name);
                    let _ = writeln!(sys, "{line}");
                    next_sys += Duration::from_secs(60);
                }
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                next += Duration::from_secs(1);

                let now_ticks = ticks.core_ticks();
                let records = coalitions::records();
                let read_at = Instant::now();
                let Some(records) = records else {
                    continue;
                };
                let [sb, eb] = match (&prev_ticks, &now_ticks) {
                    (Some(a), Some(b)) => busy(&layout, a, b),
                    _ => [None, None],
                };
                let busy_of = |window_ns: u64| {
                    let (sb, eb) = (sb?, eb?);
                    let w = window_ns as f64;
                    Some(coalition_heat::Busy {
                        all_ns: ((sb * cores[0] + eb * cores[1]) * w) as u64,
                        p_ns: (sb * cores[0] * w) as u64,
                    })
                };
                // The added parse as N2 would run it after the read: decode into kept rows, the
                // change since the last reading, and the judgement.
                let parse_cpu0 = thread_cpu_ns();
                let parse_t0 = Instant::now();
                heat_now.refill(&records);
                let judged = then_at.map(|then_at| {
                    let window_ns =
                        u64::try_from(read_at.duration_since(then_at).as_nanos()).unwrap_or(0);
                    match coalition_heat::moved(&heat_then, &heat_now) {
                        Err(why) => Verdict::Refused(why, None, window_ns),
                        Ok(m) => match coalition_heat::judge(
                            &m.sum,
                            window_ns,
                            timebase,
                            &range,
                            busy_of(window_ns),
                        ) {
                            Ok(j) => Verdict::Judged(m, j, window_ns),
                            Err(why) => Verdict::Refused(why, Some(m), window_ns),
                        },
                    }
                });
                let parse_us = parse_t0.elapsed().as_secs_f64() * 1e6;
                let parse_cpu_us = (thread_cpu_ns() - parse_cpu0) as f64 / 1e3;
                // The same parse again on records now in cache: the decode as it would run
                // inside the copy loop, each record just written.
                let warm_t0 = Instant::now();
                scratch.refill(&records);
                let again = coalition_heat::moved(&heat_then, &scratch).and_then(|m| {
                    coalition_heat::judge(
                        &m.sum,
                        1_000_000_000,
                        timebase,
                        &range,
                        busy_of(1_000_000_000),
                    )
                });
                let warm_us = warm_t0.elapsed().as_secs_f64() * 1e6;
                let _ = std::hint::black_box(again);
                // The decode as it would run inside the read's copy loop: each record copied
                // into a stack buffer (the kernel's copy-out) and decoded from it, against the
                // copies alone, in turn first so neither always finds the cache the other left;
                // and the change by id alone.
                let copies = || {
                    let t = Instant::now();
                    for (_, raw) in &records.records {
                        let buf: [u64; coalitions::USAGE_WORDS] = *std::hint::black_box(raw);
                        std::hint::black_box(&buf);
                    }
                    t.elapsed().as_secs_f64() * 1e6
                };
                let mut fused_rows = std::mem::take(&mut scratch.rows);
                let mut fused = || {
                    let t = Instant::now();
                    fused_rows.clear();
                    for (id, raw) in &records.records {
                        let buf: [u64; coalitions::USAGE_WORDS] = *std::hint::black_box(raw);
                        if let Ok(h) = coalition_heat::decode(std::hint::black_box(&buf)) {
                            fused_rows.push((*id, h));
                        }
                    }
                    t.elapsed().as_secs_f64() * 1e6
                };
                let (cz_us, fz_us) = if readings % 2 == 0 {
                    let c = copies();
                    (c, fused())
                } else {
                    let f = fused();
                    (copies(), f)
                };
                scratch.rows = fused_rows;
                readings += 1;
                let mv_t0 = Instant::now();
                let _ = std::hint::black_box(coalition_heat::moved(&heat_then, &heat_now));
                let mv_us = mv_t0.elapsed().as_secs_f64() * 1e6;

                let sample = stats.sample(gap);
                let coal_cpu0 = thread_cpu_ns();
                let coal_t0 = Instant::now();
                let existing = coalitions::coalitions();
                let coal_us = coal_t0.elapsed().as_secs_f64() * 1e6;
                let coal_cpu_us = (thread_cpu_ns() - coal_cpu0) as f64 / 1e3;

                let naive_now = naive(&records);
                let cs = cpu_stats(&sample, &tables);
                let ns = |t: u64| aterm_sysprobe::procs::ticks_to_ns(t, timebase.0, timebase.1);
                let mut row = String::new();
                let _ = write!(
                    row,
                    "{:.0},{},{},{},{},{},",
                    started.elapsed().as_secs_f64(),
                    name,
                    aterm_sysprobe::load_average()
                        .map_or_else(String::new, |l| format!("{:.1}", l[0])),
                    pressure(token).map_or_else(String::new, |p| p.to_string()),
                    opt(sb, 3),
                    opt(eb, 3),
                );
                let sums = |m: &coalition_heat::Moved, window_ns: u64| {
                    let s = m.sum;
                    format!(
                        "{},{},{},{},{},{},{:.1},{:.1},{},",
                        window_ns / 1000,
                        ns(s.cpu_ticks) / 1000,
                        ns(s.qos_ticks) / 1000,
                        ns(s.p_ticks) / 1000,
                        s.cycles / 1000,
                        s.p_cycles / 1000,
                        s.energy_nj as f64 / 1e6,
                        s.gpu_energy_nj as f64 / 1e6,
                        m.p_over,
                    )
                };
                match &judged {
                    Some(Verdict::Judged(m, j, window_ns)) => {
                        let _ = write!(
                            row,
                            "{}ok,{},{},{},{},{},{},",
                            sums(m, *window_ns),
                            opt(j.ipc, 2),
                            opt(j.p_mhz, 1),
                            opt(j.e_mhz, 1),
                            u8::from(j.e_refused),
                            opt(j.e_qos_mhz, 1),
                            u8::from(j.e_qos_refused),
                        );
                    }
                    Some(Verdict::Refused(why, m, window_ns)) => {
                        let s = m.map_or_else(|| ",,,,,,,,,".to_owned(), |m| sums(&m, *window_ns));
                        let why = match heat_now.refused.or(heat_then.refused) {
                            Some((r, n)) if *why == "refused_record" => format!("{why}:{r}x{n}"),
                            _ => (*why).to_owned(),
                        };
                        let _ = write!(row, "{s}{why},,,,,,,");
                    }
                    None => row.push_str(",,,,,,,,,first,,,,,,,"),
                }
                let naive_mhz = prev_naive.map(|a| {
                    let d: Vec<i128> = naive_now
                        .iter()
                        .zip(a)
                        .map(|(n, a)| i128::from(*n) - i128::from(a))
                        .collect();
                    let p_ns = ns(u64::try_from(d[1]).unwrap_or(0)) as f64;
                    let e_ns = ns(u64::try_from(d[0] - d[1]).unwrap_or(0)) as f64;
                    let p = (p_ns > 1e7).then(|| d[3] as f64 * 1e3 / p_ns);
                    let e = (e_ns > 1e7).then(|| (d[2] - d[3]) as f64 * 1e3 / e_ns);
                    (p, e)
                });
                let (nvp, nve) = naive_mhz.unwrap_or((None, None));
                let _ = writeln!(
                    row,
                    "{},{},{},{},{},{},{},{},{},{},{},{:.0},{:.0},{:.0},{:.1},{:.1},{},{:.1},{:.1},{:.1}",
                    opt(nvp, 1),
                    opt(nve, 1),
                    cs.window_us,
                    opt(cs.p_mhz, 1),
                    opt(cs.e_mhz, 1),
                    opt(cs.p_active, 3),
                    opt(cs.e_active, 3),
                    opt(cs.p_core_mhz, 1),
                    opt(cs.e_core_mhz, 1),
                    opt(cs.p_cores, 3),
                    opt(cs.e_cores, 3),
                    coal_us,
                    coal_cpu_us,
                    warm_us,
                    parse_us,
                    parse_cpu_us,
                    existing.map_or(0, |e| e.rows.len()),
                    cz_us,
                    fz_us,
                    mv_us,
                );
                // Every reading's verdict is counted, written or not, for the end line.
                let verdict = match &judged {
                    Some(Verdict::Judged(..)) => "ok",
                    Some(Verdict::Refused(why, ..)) => *why,
                    None => "first",
                };
                tally(&mut verdicts, verdict);
                // Sparse: only the readings under the top Super decile or the high Efficiency
                // band, every reading the self-check did not judge, and one a minute, are
                // written.
                let informative = sb.is_some_and(|s| s < 0.9)
                    || eb.is_some_and(|e| e < 2.0 / 3.0)
                    || !matches!(judged, Some(Verdict::Judged(..)));
                if !sparse || informative || readings % 60 == 0 {
                    csv.write_all(row.as_bytes()).expect("csv row");
                }
                std::mem::swap(&mut heat_then, &mut heat_now);
                then_at = Some(read_at);
                prev_naive = Some(naive_now);
                prev_ticks = now_ticks;
                if let (false, Some(sb), Some(eb)) = (spinning, sb, eb) {
                    background.push_back([sb, eb]);
                    if background.len() > 10 {
                        background.pop_front();
                    }
                }
            }
            spinners.end();
        }
        let line = sys_line(started.elapsed().as_secs_f64(), "end");
        let _ = writeln!(sys, "{line}");
        let _ = writeln!(
            sys,
            "{{\"spun_s\":{spun_s},\"readings\":{readings},\"verdicts\":{}}}",
            tallies(&verdicts)
        );
    }

    /// One more of `what` in `counts`.
    fn tally(counts: &mut Vec<(String, u64)>, what: &str) {
        match counts.iter_mut().find(|c| c.0 == what) {
            Some(c) => c.1 += 1,
            None => counts.push((what.to_owned(), 1)),
        }
    }

    /// `counts` as a JSON object.
    fn tallies(counts: &[(String, u64)]) -> String {
        let body: Vec<String> = counts.iter().map(|(k, n)| format!("\"{k}\":{n}")).collect();
        format!("{{{}}}", body.join(","))
    }

    /// `fold <dir> <secs>`: the parse folded into the start counter, timed inside a loop of one
    /// reading a second, as N2 would run it. Each reading copies every record
    /// ([`coalitions::records`]) and then, against the last reading's records, times one pass of
    /// the start counter alone and of each way [`n2_shape`] carries the heat, in an order that
    /// turns each reading, with each way's difference from the counter alone in the same
    /// reading. It then judges the change by id as the self-check would (untimed) and counts
    /// every reading's verdict. Every reading goes to `<dir>/fold.csv`; the machine's
    /// `sys --json` line to `<dir>/sys.jsonl` at the start, every minute and the end, then the
    /// verdict counts over every reading. It spins nothing.
    pub(super) fn fold(args: &[String]) {
        let (dir, secs) = match args {
            [dir, secs] => (dir, secs.parse::<u64>().unwrap_or(0)),
            _ => {
                eprintln!("usage: coal_clocks fold <dir> <secs>");
                std::process::exit(2);
            }
        };
        assert!((1..=1800).contains(&secs), "a fold run is 1 s to 30 min");
        std::fs::create_dir_all(dir).expect("dir");
        let tables = ioreg_tables();
        let range = ClockRange {
            p_mhz: (tables.p[0], tables.p.iter().copied().fold(0.0, f64::max)),
            e_mhz: (tables.e[0], tables.e.iter().copied().fold(0.0, f64::max)),
        };
        let timebase = aterm_sysprobe::procs::kernel_timebase().expect("timebase");
        let layout = aterm_sysprobe::cpu_layout();
        let cores = [0, 1].map(|i| {
            layout
                .clusters
                .get(i)
                .map_or(0.0, |c| c.logical.len() as f64)
        });
        let open = |name: &str| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(format!("{dir}/{name}"))
                .expect(name)
        };
        let (mut csv, mut sys) = (open("fold.csv"), open("sys.jsonl"));
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let controls = cpu_hardware::Probe::default()
            .sample(Duration::from_millis(100))
            .controls;
        let head = format!(
            "# start_unix_ms={unix_ms} secs={secs}\n# kernel_version={:?}\n{}\n",
            controls.kernel_version,
            super::FOLD_COLUMNS
        );
        csv.write_all(head.as_bytes()).expect("csv head");

        let token = pressure_token();
        let mut ticks = aterm_sysprobe::Probe::new();
        let mut prev_ticks = ticks.core_ticks();
        let (mut heat_then, mut heat_now) = (HeatRead::default(), HeatRead::default());
        let mut then: Option<(coalitions::RecordRead, Instant)> = None;
        let mut verdicts: Vec<(String, u64)> = Vec::new();
        let mut readings: u64 = 0;
        let started = Instant::now();
        let end = started + Duration::from_secs(secs);
        let mut next_sys = Instant::now();
        let mut next = Instant::now() + Duration::from_secs(1);
        while Instant::now() < end {
            if Instant::now() >= next_sys {
                let line = sys_line(started.elapsed().as_secs_f64(), "fold");
                let _ = writeln!(sys, "{line}");
                next_sys += Duration::from_secs(60);
            }
            if let Some(wait) = next.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            next += Duration::from_secs(1);
            let now_ticks = ticks.core_ticks();
            let records = coalitions::records();
            let read_at = Instant::now();
            readings += 1;
            let Some(records) = records else {
                tally(&mut verdicts, "unread");
                continue;
            };
            let first = usize::try_from(readings % 4).unwrap_or(0);
            let timed = then
                .as_ref()
                .map(|(then_read, _)| n2_shape(1, first, then_read, &records));
            let [sb, eb] = match (&prev_ticks, &now_ticks) {
                (Some(a), Some(b)) => busy(&layout, a, b),
                _ => [None, None],
            };
            heat_now.refill(&records);
            let verdict = match &then {
                None => "first",
                Some((_, then_at)) => {
                    let window_ns =
                        u64::try_from(read_at.duration_since(*then_at).as_nanos()).unwrap_or(0);
                    let busy = sb.zip(eb).map(|(sb, eb)| {
                        let w = window_ns as f64;
                        coalition_heat::Busy {
                            all_ns: ((sb * cores[0] + eb * cores[1]) * w) as u64,
                            p_ns: (sb * cores[0] * w) as u64,
                        }
                    });
                    match coalition_heat::moved(&heat_then, &heat_now) {
                        Err(why) => why,
                        Ok(m) => {
                            match coalition_heat::judge(&m.sum, window_ns, timebase, &range, busy) {
                                Ok(_) => "ok",
                                Err(why) => why,
                            }
                        }
                    }
                }
            };
            tally(&mut verdicts, verdict);
            let mut row = format!(
                "{:.0},{},{},{},{},{},{first},{verdict},",
                started.elapsed().as_secs_f64(),
                aterm_sysprobe::load_average().map_or_else(String::new, |l| format!("{:.1}", l[0])),
                pressure(token).map_or_else(String::new, |p| p.to_string()),
                opt(sb, 3),
                opt(eb, 3),
                records.records.len(),
            );
            match timed {
                Some((v, agree)) => {
                    let _ = writeln!(
                        row,
                        "{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{}{}{}",
                        v[0].0,
                        v[1].0,
                        v[3].0,
                        v[5].0,
                        v[2].0,
                        v[4].0,
                        v[6].0,
                        u8::from(agree[0]),
                        u8::from(agree[1]),
                        u8::from(agree[2]),
                    );
                }
                None => row.push_str(",,,,,,,\n"),
            }
            csv.write_all(row.as_bytes()).expect("csv row");
            std::mem::swap(&mut heat_then, &mut heat_now);
            then = Some((records, read_at));
            prev_ticks = now_ticks;
        }
        let line = sys_line(started.elapsed().as_secs_f64(), "end");
        let _ = writeln!(sys, "{line}");
        let _ = writeln!(
            sys,
            "{{\"readings\":{readings},\"verdicts\":{}}}",
            tallies(&verdicts)
        );
    }

    /// `bench [n] [every_s count]`: [`bench_once`] over `n` passes, `count` times `every_s`
    /// seconds apart, each after the machine's `aterm ctl @self sys --json` line.
    pub(super) fn bench(args: &[String]) {
        let arg = |i: usize| args.get(i).and_then(|a| a.parse::<u64>().ok());
        let n = arg(0).map_or(200, |n| usize::try_from(n).unwrap_or(200));
        let (every, count) = (arg(1).unwrap_or(0), arg(2).unwrap_or(1));
        let started = Instant::now();
        for i in 0..count {
            let at = started + Duration::from_secs(every * i);
            if let Some(wait) = at.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            let sys = sys_line(started.elapsed().as_secs_f64(), "bench");
            println!("# bench {i} sys={sys}");
            bench_once(n);
        }
    }

    /// The parse's stages, each timed alone over `n` passes on one pair of reads a second apart:
    /// the decode into kept rows, the change by id, the judgement; and the decode as it would run
    /// inside the copy loop, each record copied into a stack buffer (the kernel's copy-out) and
    /// decoded from there, less the copies alone. Then the kernel's order and what ordering by
    /// id costs, and the parse folded into the start counter three ways ([`n2_shape`]).
    fn bench_once(n: usize) {
        let tables = ioreg_tables();
        let range = ClockRange {
            p_mhz: (tables.p[0], tables.p.iter().copied().fold(0.0, f64::max)),
            e_mhz: (tables.e[0], tables.e.iter().copied().fold(0.0, f64::max)),
        };
        let timebase = aterm_sysprobe::procs::kernel_timebase().expect("timebase");
        let then_read = coalitions::records().expect("records");
        let then = HeatRead::of(&then_read);
        std::thread::sleep(Duration::from_secs(1));
        let records = coalitions::records().expect("records");
        let mut now = HeatRead::of(&records);
        let m = coalition_heat::moved(&then, &now).expect("moved");
        let time = |f: &mut dyn FnMut()| {
            let mut v: Vec<f64> = (0..n)
                .map(|_| {
                    let t = Instant::now();
                    f();
                    t.elapsed().as_secs_f64() * 1e6
                })
                .collect();
            v.sort_by(f64::total_cmp);
            (v[n / 2], v[(n * 95).div_ceil(100) - 1])
        };
        let refill = time(&mut || now.refill(std::hint::black_box(&records)));
        let moved = time(&mut || {
            let _ = std::hint::black_box(coalition_heat::moved(&then, &now));
        });
        let judge = time(&mut || {
            let _ = std::hint::black_box(coalition_heat::judge(
                &m.sum,
                1_000_000_000,
                timebase,
                &range,
                None,
            ));
        });
        let mut rows: Vec<(u64, coalition_heat::Heat)> = Vec::with_capacity(records.records.len());
        let copies = time(&mut || {
            for (_, raw) in &records.records {
                let buf: [u64; coalitions::USAGE_WORDS] = *std::hint::black_box(raw);
                std::hint::black_box(&buf);
            }
        });
        let fused = time(&mut || {
            rows.clear();
            for (id, raw) in &records.records {
                let buf: [u64; coalitions::USAGE_WORDS] = *std::hint::black_box(raw);
                if let Ok(h) = coalition_heat::decode(std::hint::black_box(&buf)) {
                    rows.push((*id, h));
                }
            }
        });
        // The kernel's own order, and what putting the rows in id order costs: the decoded rows
        // sorted from the kernel's order, against the listed ids (16 bytes each) sorted before
        // the per-id read, less the copy into the scratch vector each pass makes.
        let kernel_rows = rows.clone();
        let mut scratch_rows = Vec::with_capacity(kernel_rows.len());
        let rows_copy = time(&mut || {
            scratch_rows.clear();
            scratch_rows.extend_from_slice(std::hint::black_box(&kernel_rows));
        });
        let rows_sort = time(&mut || {
            scratch_rows.clear();
            scratch_rows.extend_from_slice(std::hint::black_box(&kernel_rows));
            scratch_rows.sort_unstable_by_key(|r| r.0);
        });
        let ids: Vec<(u64, u32)> = records.records.iter().map(|r| (r.0, 0)).collect();
        let mut scratch_ids = Vec::with_capacity(ids.len());
        let ids_copy = time(&mut || {
            scratch_ids.clear();
            scratch_ids.extend_from_slice(std::hint::black_box(&ids));
        });
        let ids_sort = time(&mut || {
            scratch_ids.clear();
            scratch_ids.extend_from_slice(std::hint::black_box(&ids));
            scratch_ids.sort_unstable_by_key(|r| r.0);
        });
        let ascending = records.records.is_sorted_by_key(|r| r.0);
        let descending = records.records.is_sorted_by_key(|r| std::cmp::Reverse(r.0));
        let runs = 1 + records
            .records
            .windows(2)
            .filter(|w| w[1].0 < w[0].0)
            .count();
        println!(
            "records={} passes={n} load1={:?} kernel order: ascending={ascending} \
             descending={descending} ascending runs={runs}; matched={} born={} gone={} unbased={}",
            records.records.len(),
            aterm_sysprobe::load_average().map(|l| l[0]),
            m.matched,
            m.born,
            then.rows
                .len()
                .saturating_sub(usize::try_from(m.matched).unwrap_or(0)),
            m.unbased
        );
        let (n2, agrees) = n2_shape(n, 0, &then_read, &records);
        println!(
            "n2 start counter alone p50={:.1} p95={:.1} us",
            n2[0].0, n2[0].1
        );
        for (k, name) in ["rows", "totals", "totals24"].iter().enumerate() {
            let (whole, added) = (n2[1 + 2 * k], n2[2 + 2 * k]);
            println!(
                "n2 {name:<8} p50={:.1} p95={:.1} us, added p50={:.1} p95={:.1} us, equals moved: {}",
                whole.0, whole.1, added.0, added.1, agrees[k]
            );
        }
        println!(
            "rows sort p50={:.1} p95={:.1} us (less the copy {:.1} / {:.1})",
            rows_sort.0 - rows_copy.0,
            rows_sort.1 - rows_copy.1,
            rows_copy.0,
            rows_copy.1
        );
        println!(
            "ids sort  p50={:.1} p95={:.1} us (less the copy {:.1} / {:.1})",
            ids_sort.0 - ids_copy.0,
            ids_sort.1 - ids_copy.1,
            ids_copy.0,
            ids_copy.1
        );
        println!("refill  p50={:.1} p95={:.1} us", refill.0, refill.1);
        println!("moved   p50={:.1} p95={:.1} us", moved.0, moved.1);
        println!("judge   p50={:.2} p95={:.2} us", judge.0, judge.1);
        println!("copies  p50={:.1} p95={:.1} us", copies.0, copies.1);
        println!(
            "copy+decode p50={:.1} p95={:.1} us (decode in the copy loop ≈ {:.1} / {:.1})",
            fused.0,
            fused.1,
            fused.0 - copies.0,
            fused.1 - copies.1
        );
    }

    /// One coalition's task counters as the start counter keeps them each reading
    /// (`aterm_vitals::starts::CoalTasks`).
    #[derive(Clone, Copy)]
    struct Tasks {
        id: u64,
        started: u64,
        exited: u64,
    }

    /// The same, with the index of its record's heat among those the copy loop decoded
    /// (`u32::MAX` where it refused the record).
    #[derive(Clone, Copy)]
    struct HeatTasks {
        id: u64,
        started: u64,
        exited: u64,
        at: u32,
    }

    /// The start counter's merge as `aterm_vitals::starts::coalition_moves` runs it over rows
    /// ascending by id: the starts and exits, and where `rows` holds both reads' decoded heat,
    /// each matched coalition's change through the rows' indices plus each born one whole
    /// ([`coalition_heat::moved`]'s sum). `gone` is told each coalition gone since `then` (its
    /// id and index), and refuses the merge by answering `None`.
    fn merge<R: Copy>(
        then: &[R],
        now: &[R],
        newest: Option<u64>,
        row: impl Fn(R) -> (u64, u64, u64, u32),
        rows: Option<(&[Heat], &[Heat])>,
        mut gone: impl FnMut(u64, u32) -> Option<()>,
    ) -> Option<(u64, u64, Heat)> {
        let (mut starts, mut exits, mut sum) = (0u64, 0u64, Heat::default());
        let at = |rows: &[Heat], i: u32| rows.get(usize::try_from(i).ok()?).copied();
        let (mut i, mut j) = (0, 0);
        loop {
            let (a, b) = (then.get(i).map(|r| row(*r)), now.get(j).map(|r| row(*r)));
            let left = match (a, b) {
                (None, None) => return Some((starts, exits, sum)),
                (Some(a), Some(b)) if a.0 == b.0 => {
                    starts = starts.checked_add(b.1.checked_sub(a.1)?)?;
                    exits = exits.checked_add(b.2.checked_sub(a.2)?)?;
                    if let Some((was, is)) = rows {
                        sum = sum.plus(at(is, b.3)?.since(at(was, a.3)?)?);
                    }
                    i += 1;
                    j += 1;
                    continue;
                }
                (Some(a), Some(b)) => (a.0 < b.0).then_some(a),
                (Some(a), None) => Some(a),
                (None, Some(_)) => None,
            };
            if let Some(a) = left {
                exits = exits.checked_add(a.1.checked_sub(a.2)?)?;
                gone(a.0, a.3)?;
                i += 1;
            } else if let Some(b) = b {
                if newest.is_some_and(|n| b.0 <= n) {
                    return None;
                }
                starts = starts.checked_add(b.1)?;
                exits = exits.checked_add(b.2)?;
                if let Some((_, is)) = rows {
                    sum = sum.plus(at(is, b.3)?);
                }
                j += 1;
            }
        }
    }

    /// The start counter's copy loop and order: each record copied out (the kernel's copy-out)
    /// into a 24-byte task row, the rows sorted by id as `CoalTaskRead::of` sorts them. Where
    /// `heat` is given, the loop also decodes each copied record's heat words and keeps them by
    /// id in the kernel's order, and gives their total where it refused no record.
    fn tasks(
        read: &coalitions::RecordRead,
        out: &mut Vec<Tasks>,
        mut heat: Option<&mut Vec<(u64, Heat)>>,
    ) -> Option<Heat> {
        out.clear();
        if let Some(h) = heat.as_deref_mut() {
            h.clear();
        }
        // Wrapping adds into a flat accumulator: the totals stay far under 2^64 (the largest
        // lifetime counter read here, one coalition's instructions, is under 2^53).
        let (mut total, mut whole) = ([0u64; 11], true);
        for (id, raw) in &read.records {
            let buf: [u64; coalitions::USAGE_WORDS] = *std::hint::black_box(raw);
            // The kernel's copy-out writes the whole record, whatever the loop then reads.
            std::hint::black_box(&buf);
            if let Some(h) = heat.as_deref_mut() {
                match coalition_heat::decode(std::hint::black_box(&buf)) {
                    Ok(one) => {
                        for (t, v) in total.iter_mut().zip(heat_words(&one)) {
                            *t = t.wrapping_add(v);
                        }
                        h.push((*id, one));
                    }
                    Err(_) => whole = false,
                }
            }
            out.push(Tasks {
                id: *id,
                started: buf[0],
                exited: buf[1],
            });
        }
        out.sort_by_key(|r| r.id);
        out.dedup_by_key(|r| r.id);
        (heat.is_some() && whole).then(|| heat_of_words(total))
    }

    /// A [`Heat`]'s counters in field order, and back.
    fn heat_words(h: &Heat) -> [u64; 11] {
        [
            h.cpu_ticks,
            h.qos_ticks,
            h.p_ticks,
            h.energy_nj,
            h.instructions,
            h.cycles,
            h.p_instructions,
            h.p_cycles,
            h.ane_ticks,
            h.ane_energy_nj,
            h.gpu_energy_nj,
        ]
    }

    fn heat_of_words(w: [u64; 11]) -> Heat {
        Heat {
            cpu_ticks: w[0],
            qos_ticks: w[1],
            p_ticks: w[2],
            energy_nj: w[3],
            instructions: w[4],
            cycles: w[5],
            p_instructions: w[6],
            p_cycles: w[7],
            ane_ticks: w[8],
            ane_energy_nj: w[9],
            gpu_energy_nj: w[10],
        }
    }

    /// The same loop decoding each copied record's heat into `heat` in the kernel's order, each
    /// task row taking its index (32 bytes); with `TOTAL`, the decoded heat summed as it goes,
    /// and that total where the loop refused no record.
    fn heat_tasks<const TOTAL: bool>(
        read: &coalitions::RecordRead,
        out: &mut Vec<HeatTasks>,
        heat: &mut Vec<Heat>,
    ) -> Option<Heat> {
        out.clear();
        heat.clear();
        let (mut total, mut whole) = (Heat::default(), true);
        for (id, raw) in &read.records {
            let buf: [u64; coalitions::USAGE_WORDS] = *std::hint::black_box(raw);
            std::hint::black_box(&buf);
            let at = match coalition_heat::decode(std::hint::black_box(&buf)) {
                Ok(h) => {
                    if TOTAL {
                        total = total.plus(h);
                    }
                    heat.push(h);
                    u32::try_from(heat.len() - 1).unwrap_or(u32::MAX)
                }
                Err(_) => {
                    whole = false;
                    u32::MAX
                }
            };
            out.push(HeatTasks {
                id: *id,
                started: buf[0],
                exited: buf[1],
                at,
            });
        }
        out.sort_by_key(|r| r.id);
        out.dedup_by_key(|r| r.id);
        whole.then_some(total)
    }

    /// The parse as N2 would fold it into what aterm already does each reading: the start
    /// counter copies every record out, keeps its task rows, orders them by id and merges them
    /// with the last reading's. Three ways to carry the heat there, each decoding the heat words
    /// in the copy loop:
    ///
    /// - `rows`: each task row takes its record's heat index (32 bytes), and the merge sums each
    ///   matched coalition's change and each born one whole;
    /// - `totals`: the same rows, the loop sums the decoded heat, and the merge sums only the
    ///   heat of the coalitions gone since the earlier read; the change is the later total less
    ///   the earlier total less those gone;
    /// - `totals24`: the task rows stay 24 bytes, the decoded heat is kept by id in the kernel's
    ///   order and summed by wrapping adds into a flat accumulator (`totals` sums by
    ///   [`Heat::plus`]), and each gone coalition's earlier heat is found there by its id.
    ///
    /// Timed in rotating order over `n` passes, p50 and p95 of: the start counter alone; then,
    /// for each way, the whole and its difference from the start counter alone in the same
    /// pass. And whether each way's summed change equals [`coalition_heat::moved`]'s.
    ///
    /// `offset` turns the rotation: the first pass starts with way `offset` (0 the counter
    /// alone, 1 `rows`, 2 `totals`, 3 `totals24`). The output vectors are sized before the
    /// timing, as the start counter keeps its own from one reading to the next, so a single
    /// pass ([`fold`]) times no growth.
    fn n2_shape(
        n: usize,
        offset: usize,
        then_read: &coalitions::RecordRead,
        now_read: &coalitions::RecordRead,
    ) -> ([(f64, f64); 7], [bool; 3]) {
        let task_row = |r: Tasks| (r.id, r.started, r.exited, 0);
        let heat_row = |r: HeatTasks| (r.id, r.started, r.exited, r.at);
        let newest = then_read.newest;
        let (mut then_tasks, mut then_ids) = (Vec::new(), Vec::new());
        let (mut then_rows, mut then_heat) = (Vec::new(), Vec::new());
        let then_total = heat_tasks::<true>(then_read, &mut then_rows, &mut then_heat);
        tasks(then_read, &mut then_tasks, Some(&mut then_ids));
        let cap = now_read.records.len();
        let (mut alone_now, mut t24_now, mut t24_heat) = (
            Vec::with_capacity(cap),
            Vec::with_capacity(cap),
            Vec::with_capacity(cap),
        );
        let (mut rows_now, mut rows_heat) = (Vec::with_capacity(cap), Vec::with_capacity(cap));
        let (mut totals_now, mut totals_heat) = (Vec::with_capacity(cap), Vec::with_capacity(cap));
        let mut alone = || {
            let t = Instant::now();
            tasks(now_read, &mut alone_now, None);
            let got = merge(&then_tasks, &alone_now, newest, task_row, None, |_, _| {
                Some(())
            });
            let _ = std::hint::black_box(got);
            t.elapsed().as_secs_f64() * 1e6
        };
        let mut rows = || {
            let t = Instant::now();
            heat_tasks::<false>(now_read, &mut rows_now, &mut rows_heat);
            let carry = Some((then_heat.as_slice(), rows_heat.as_slice()));
            let got = merge(&then_rows, &rows_now, newest, heat_row, carry, |_, _| {
                Some(())
            })
            .map(|m| m.2);
            let _ = std::hint::black_box(got);
            (t.elapsed().as_secs_f64() * 1e6, got)
        };
        let mut totals = || {
            let t = Instant::now();
            let now_total = heat_tasks::<true>(now_read, &mut totals_now, &mut totals_heat);
            let mut gone_heat = Heat::default();
            let got = merge(&then_rows, &totals_now, newest, heat_row, None, |_, at| {
                gone_heat = gone_heat.plus(*then_heat.get(usize::try_from(at).ok()?)?);
                Some(())
            })
            .and_then(|_| now_total?.since(then_total?.since(gone_heat)?));
            let _ = std::hint::black_box(got);
            (t.elapsed().as_secs_f64() * 1e6, got)
        };
        let mut totals24 = || {
            let t = Instant::now();
            let now_total = tasks(now_read, &mut t24_now, Some(&mut t24_heat));
            let mut gone_heat = Heat::default();
            let got = merge(&then_tasks, &t24_now, newest, task_row, None, |id, _| {
                gone_heat = gone_heat.plus(then_ids.iter().find(|h| h.0 == id)?.1);
                Some(())
            })
            .and_then(|_| now_total?.since(then_total?.since(gone_heat)?));
            let _ = std::hint::black_box(got);
            (t.elapsed().as_secs_f64() * 1e6, got)
        };
        let mut v: [Vec<f64>; 7] = Default::default();
        let mut got = [None; 3];
        for pass in 0..n {
            // Each pass runs the four in a rotated order, so none always follows the same one.
            let mut x = [0.0; 4];
            let mut by: [Option<Heat>; 3] = [None; 3];
            for k in 0..4 {
                match (pass + k + offset) % 4 {
                    0 => x[0] = alone(),
                    1 => (x[1], by[0]) = rows(),
                    2 => (x[2], by[1]) = totals(),
                    _ => (x[3], by[2]) = totals24(),
                }
            }
            let values = [
                x[0],
                x[1],
                x[1] - x[0],
                x[2],
                x[2] - x[0],
                x[3],
                x[3] - x[0],
            ];
            for (out, value) in v.iter_mut().zip(values) {
                out.push(value);
            }
            got = by;
        }
        let pct = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            (v[v.len() / 2], v[(v.len() * 95).div_ceil(100) - 1])
        };
        let want = coalition_heat::moved(&HeatRead::of(then_read), &HeatRead::of(now_read))
            .ok()
            .map(|m| m.sum);
        (
            v.each_mut().map(pct),
            got.map(|g| want.is_some() && g == want),
        )
    }

    /// One busy coalition's two records a second apart, both kinds of core in it.
    pub(super) fn record() {
        let a = coalitions::records().expect("records");
        let mut refused: Vec<(&str, u32, u64)> = Vec::new();
        for (id, raw) in &a.records {
            if let Err(why) = coalition_heat::decode(raw) {
                match refused.iter_mut().find(|r| r.0 == why) {
                    Some(r) => r.1 += 1,
                    None => {
                        refused.push((why, 1, *id));
                        println!("// refused {why}: coalition {id} {raw:?}");
                    }
                }
            }
        }
        println!("// {} records, refused {refused:?}", a.records.len());
        let at = Instant::now();
        std::thread::sleep(Duration::from_secs(1));
        let b = coalitions::records().expect("records");
        let window = at.elapsed();
        let best = b
            .records
            .iter()
            .filter_map(|(id, raw)| {
                let then = a.records.iter().find(|(i, _)| i == id)?.1;
                let (h0, h1) = (
                    coalition_heat::decode(&then).ok()?,
                    coalition_heat::decode(raw).ok()?,
                );
                let d = h1.since(h0)?;
                (d.p_ticks > 0 && d.cpu_ticks > d.p_ticks + 240_000).then_some((
                    d.cpu_ticks,
                    *id,
                    then,
                    *raw,
                ))
            })
            .max_by_key(|r| r.0);
        let Some((_, id, then, now)) = best else {
            eprintln!("no coalition ran on both kinds of core");
            return;
        };
        println!("// coalition {id}, window {} ns", window.as_nanos());
        println!("const THEN: [u64; 64] = {then:?};");
        println!("const NOW: [u64; 64] = {now:?};");
    }
}

mod analyze {
    use std::fmt::Write as _;

    /// The nearest-rank `q` quantile of sorted `v`.
    fn q(v: &[f64], q: f64) -> f64 {
        if v.is_empty() {
            return f64::NAN;
        }
        let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
        v[i]
    }

    fn sorted(mut v: Vec<f64>) -> Vec<f64> {
        v.sort_by(f64::total_cmp);
        v
    }

    struct Rows {
        names: Vec<String>,
        rows: Vec<Vec<String>>,
        /// The tables' (Efficiency bottom, Efficiency top, Super top), MHz, from the header.
        tops: (f64, f64, f64),
    }

    impl Rows {
        fn col(&self, name: &str) -> usize {
            self.names.iter().position(|n| n == name).expect(name)
        }
        fn f(&self, r: &[String], name: &str) -> Option<f64> {
            r.get(self.col(name))?.parse().ok()
        }
        fn s<'a>(&self, r: &'a [String], name: &str) -> &'a str {
            r.get(self.col(name)).map_or("", String::as_str)
        }
    }

    fn load(paths: &[String]) -> Rows {
        let mut out = Rows {
            names: super::COLUMNS.split(',').map(str::to_owned).collect(),
            rows: Vec::new(),
            tops: (f64::NAN, f64::NAN, f64::NAN),
        };
        for p in paths {
            let text = std::fs::read_to_string(p).expect("csv");
            for line in text.lines() {
                let table = |key: &str| -> Option<Vec<f64>> {
                    let list = line.strip_prefix(key)?.trim_matches(['[', ']']);
                    Some(list.split(", ").filter_map(|v| v.parse().ok()).collect())
                };
                if let Some(e) = table("# table_e_mhz=") {
                    out.tops.0 = e.first().copied().unwrap_or(f64::NAN);
                    out.tops.1 = e.iter().copied().fold(0.0, f64::max);
                }
                if let Some(p) = table("# table_p_mhz=") {
                    out.tops.2 = p.iter().copied().fold(0.0, f64::max);
                }
                if line.starts_with('#') || line.starts_with("t,") || line.is_empty() {
                    continue;
                }
                out.rows.push(line.split(',').map(str::to_owned).collect());
            }
        }
        out
    }

    /// n, p50, p95 and, as the M-PCLK spread, p95 of |bias − p50|, of a set of biases (%).
    fn spread(v: Vec<f64>) -> (usize, f64, f64, f64, f64, f64) {
        let v = sorted(v);
        let mid = q(&v, 0.5);
        let dev = sorted(v.iter().map(|b| (b - mid).abs()).collect());
        (
            v.len(),
            mid,
            q(&v, 0.05),
            q(&v, 0.95),
            q(&dev, 0.95),
            q(&dev, 1.0),
        )
    }

    /// Each `p50=` and `p95=` pair on a line, in order, and the `≈ a / b` pair where one is.
    fn pairs(line: &str) -> Vec<(f64, f64)> {
        let num = |s: &str| -> Option<f64> {
            let end = s
                .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                .unwrap_or(s.len());
            s[..end].parse().ok()
        };
        let mut out = Vec::new();
        let mut rest = line;
        while let Some(i) = rest.find("p50=") {
            rest = &rest[i + 4..];
            let Some(j) = rest.find("p95=") else { break };
            if let (Some(a), Some(b)) = (num(rest), num(&rest[j + 4..])) {
                out.push((a, b));
            }
            rest = &rest[j + 4..];
        }
        if let Some(i) = line.find("≈ ") {
            let s = &line[i + "≈ ".len()..];
            if let (Some(a), Some(j)) = (num(s), s.find("/ "))
                && let Some(b) = num(&s[j + 2..])
            {
                out.push((a, b));
            }
        }
        out
    }

    /// `analyze-bench <file>...`: a bench series' runs ([`super::mac`]'s `bench n every count`)
    /// by load1 band: for each stage, the median over the runs of each run's p50, and of each
    /// run's p95, with the largest p95.
    pub(super) fn bench(paths: &[String]) {
        const STAGES: [(&str, &str, usize); 9] = [
            ("start counter alone", "n2 start counter alone", 0),
            ("rows: added", "n2 rows ", 1),
            ("totals: added", "n2 totals ", 1),
            ("totals24: added", "n2 totals24", 1),
            ("decode in the copy loop", "copy+decode", 1),
            ("change by id, rows in id order", "moved ", 0),
            ("sort the decoded rows", "rows sort", 0),
            ("sort the listed ids", "ids sort", 0),
            ("separate pass: decode into rows", "refill", 0),
        ];
        /// One bench run: its load1, each stage's (p50, p95), and whether every folded sum
        /// equalled the change by id.
        type Run = (f64, [Option<(f64, f64)>; 9], bool);
        let mut runs: Vec<Run> = Vec::new();
        for path in paths {
            let text = std::fs::read_to_string(path).expect("bench series");
            for line in text.lines() {
                if let Some(i) = line.find("load1=Some(") {
                    let s = &line[i + "load1=Some(".len()..];
                    let load = s[..s.find(')').unwrap_or(0)].parse().unwrap_or(f64::NAN);
                    runs.push((load, [None; 9], true));
                    continue;
                }
                let Some(run) = runs.last_mut() else { continue };
                if line.contains("equals moved: false") {
                    run.2 = false;
                }
                for (k, (_, prefix, at)) in STAGES.iter().enumerate() {
                    if line.starts_with(prefix) {
                        run.1[k] = pairs(line).get(*at).copied();
                    }
                }
            }
        }
        let mut out = String::new();
        let agree = runs.iter().filter(|r| r.2).count();
        let _ = writeln!(
            out,
            "bench runs {}, each folded sum equal to the change by id in {agree}\n",
            runs.len()
        );
        let bands = [
            (0.0, 20.0, "< 20"),
            (20.0, 40.0, "20–40"),
            (40.0, 80.0, "40–80"),
            (80.0, 1e9, "≥ 80"),
            (0.0, 1e9, "all"),
        ];
        let _ = write!(out, "| stage (µs) |");
        let mut counts = Vec::new();
        for (lo, hi, name) in bands {
            let n = runs.iter().filter(|r| r.0 >= lo && r.0 < hi).count();
            counts.push(n);
            let _ = write!(out, " load1 {name} (runs {n}) |");
        }
        let _ = writeln!(out, "\n|---|{}", "---|".repeat(bands.len()));
        for (k, (label, _, _)) in STAGES.iter().enumerate() {
            let _ = write!(out, "| {label} |");
            for (lo, hi, _) in bands {
                let got: Vec<(f64, f64)> = runs
                    .iter()
                    .filter(|r| r.0 >= lo && r.0 < hi)
                    .filter_map(|r| r.1[k])
                    .collect();
                if got.is_empty() {
                    let _ = write!(out, " — |");
                    continue;
                }
                let p50 = sorted(got.iter().map(|g| g.0).collect());
                let p95 = sorted(got.iter().map(|g| g.1).collect());
                let _ = write!(
                    out,
                    " {:.1} / {:.1} (max {:.1}) |",
                    q(&p50, 0.5),
                    q(&p95, 0.5),
                    p95.last().copied().unwrap_or(f64::NAN)
                );
            }
            let _ = writeln!(out);
        }
        print!("{out}");
    }

    /// `analyze-fold <csv>...`: `fold`'s readings pooled by load1 band: for the start counter
    /// alone, and for each way's difference from it in the same reading, n, p50, p95, max and how
    /// many readings passed 20 µs; the same for `totals24` by which way ran first; the verdicts
    /// the CSV holds; and how many readings' folded sums equalled the change by id.
    pub(super) fn fold(paths: &[String]) {
        let col = |name: &str| {
            super::FOLD_COLUMNS
                .split(',')
                .position(|c| c == name)
                .expect(name)
        };
        let mut rows: Vec<Vec<String>> = Vec::new();
        for p in paths {
            let text = std::fs::read_to_string(p).expect("fold csv");
            for line in text.lines() {
                if line.starts_with('#') || line.starts_with("t,") || line.is_empty() {
                    continue;
                }
                rows.push(line.split(',').map(str::to_owned).collect());
            }
        }
        let f = |r: &[String], name: &str| r.get(col(name)).and_then(|v| v.parse::<f64>().ok());
        let timed: Vec<&Vec<String>> = rows.iter().filter(|r| f(r, "alone").is_some()).collect();
        let mut out = String::new();
        let mut verdicts: Vec<(String, usize)> = Vec::new();
        for r in &rows {
            let v = r.get(col("v")).cloned().unwrap_or_default();
            match verdicts.iter_mut().find(|c| c.0 == v) {
                Some(c) => c.1 += 1,
                None => verdicts.push((v, 1)),
            }
        }
        let agree = timed
            .iter()
            .filter(|r| r.get(col("agree")).is_some_and(|a| a == "111"))
            .count();
        let _ = writeln!(
            out,
            "readings {} (timed {}); verdicts {verdicts:?}; every folded sum equal to the change by id in {agree} of {}\n",
            rows.len(),
            timed.len(),
            timed.len()
        );
        let stat = |v: Vec<f64>| -> String {
            let v = sorted(v);
            if v.is_empty() {
                return "— | — | — | —".to_owned();
            }
            let over = v.iter().filter(|x| **x > 20.0).count();
            format!(
                "{} | {:.1} / {:.1} | {:.1} | {over}",
                v.len(),
                q(&v, 0.5),
                q(&v, 0.95),
                q(&v, 1.0)
            )
        };
        let bands = [
            (0.0, 20.0, "< 20"),
            (20.0, 40.0, "20–40"),
            (40.0, 80.0, "40–80"),
            (80.0, 1e9, "≥ 80"),
            (0.0, 1e9, "all"),
        ];
        let _ = writeln!(
            out,
            "| µs | load1 | n | p50 / p95 | max | readings over 20 µs |\n|---|---|---|---|---|---|"
        );
        for (label, name) in [
            ("start counter alone", "alone"),
            ("rows: added", "a_rows"),
            ("totals: added", "a_totals"),
            ("totals24: added", "a_totals24"),
            ("totals: whole", "totals"),
            ("totals24: whole", "totals24"),
        ] {
            for (lo, hi, band) in bands {
                let v: Vec<f64> = timed
                    .iter()
                    .filter(|r| f(r, "l1").is_some_and(|l| l >= lo && l < hi))
                    .filter_map(|r| f(r, name))
                    .collect();
                let _ = writeln!(out, "| {label} | {band} | {} |", stat(v));
            }
        }
        // Each way's whole pass against the counter alone's, quantile by quantile: the order
        // turns each reading, so each way ran first, second, third and fourth equally often, and
        // the difference of the two distributions' quantiles is what folding the parse in adds
        // to the reading's own p50 and p95, free of the noise of two separate passes' pairing.
        let _ = writeln!(
            out,
            "\n| whole pass less the counter alone's, by quantile (µs) | load1 | n | p50 | p95 |\n|---|---|---|---|---|"
        );
        for (label, name) in [
            ("rows", "rows"),
            ("totals", "totals"),
            ("totals24", "totals24"),
        ] {
            for (lo, hi, band) in bands {
                let in_band: Vec<&&Vec<String>> = timed
                    .iter()
                    .filter(|r| f(r, "l1").is_some_and(|l| l >= lo && l < hi))
                    .collect();
                let way = sorted(in_band.iter().filter_map(|r| f(r, name)).collect());
                let alone = sorted(in_band.iter().filter_map(|r| f(r, "alone")).collect());
                if way.is_empty() || alone.is_empty() {
                    let _ = writeln!(out, "| {label} | {band} | 0 | — | — |");
                    continue;
                }
                let _ = writeln!(
                    out,
                    "| {label} | {band} | {} | {:.1} | {:.1} |",
                    way.len(),
                    q(&way, 0.5) - q(&alone, 0.5),
                    q(&way, 0.95) - q(&alone, 0.95)
                );
            }
        }
        let _ = writeln!(
            out,
            "\n| totals24 added, by the way that ran first | n | p50 / p95 | max | readings over 20 µs |\n|---|---|---|---|---|"
        );
        for (k, name) in ["alone", "rows", "totals", "totals24"].iter().enumerate() {
            let v: Vec<f64> = timed
                .iter()
                .filter(|r| f(r, "first").is_some_and(|x| x as usize == k))
                .filter_map(|r| f(r, "a_totals24"))
                .collect();
            let _ = writeln!(out, "| {name} first | {} |", stat(v));
        }
        print!("{out}");
    }

    pub(super) fn main(paths: &[String]) {
        let rows = load(paths);
        let mut out = String::new();
        let n_all = rows.rows.len();
        let ok: Vec<&Vec<String>> = rows
            .rows
            .iter()
            .filter(|r| rows.s(r, "v") == "ok")
            .collect();
        let _ = writeln!(out, "readings {n_all}, judged ok {}", ok.len());
        let mut verdicts: Vec<(String, usize)> = Vec::new();
        for r in &rows.rows {
            let v = rows.s(r, "v").to_owned();
            match verdicts.iter_mut().find(|(k, _)| *k == v) {
                Some(e) => e.1 += 1,
                None => verdicts.push((v, 1)),
            }
        }
        let _ = writeln!(out, "verdicts {verdicts:?}");

        let _ = writeln!(out, "\n## M-COAL cost (µs, per reading), by load1 band\n");
        let _ = writeln!(
            out,
            "| load1 | n | existing read p50 / p95 | its thread CPU p50 / p95 | added parse, after the read, p50 / p95 | its thread CPU p50 / p95 | the parse warm p50 / p95 | coalitions p50 |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
        for (lo, hi) in [
            (0.0, 10.0),
            (10.0, 20.0),
            (20.0, 40.0),
            (40.0, 1e9),
            (0.0, 1e9),
        ] {
            let band: Vec<&Vec<String>> = rows
                .rows
                .iter()
                .filter(|r| rows.f(r, "l1").is_some_and(|l| l >= lo && l < hi))
                .filter(|r| rows.f(r, "parse").is_some())
                .collect();
            if band.is_empty() {
                continue;
            }
            let col = |c: &str| sorted(band.iter().filter_map(|r| rows.f(r, c)).collect());
            let (a, b, c, d, e) = (
                col("coal"),
                col("coalc"),
                col("parse"),
                col("parsec"),
                col("n"),
            );
            let w = col("warm");
            let label = if hi > 1e8 && lo == 0.0 {
                "all".to_owned()
            } else if hi > 1e8 {
                format!("≥ {lo:.0}")
            } else {
                format!("{lo:.0}–{hi:.0}")
            };
            let _ = writeln!(
                out,
                "| {label} | {} | {:.0} / {:.0} | {:.0} / {:.0} | {:.1} / {:.1} | {:.1} / {:.1} | {:.1} / {:.1} | {:.0} |",
                band.len(),
                q(&a, 0.5),
                q(&a, 0.95),
                q(&b, 0.5),
                q(&b, 0.95),
                q(&c, 0.5),
                q(&c, 0.95),
                q(&d, 0.5),
                q(&d, 0.95),
                q(&w, 0.5),
                q(&w, 0.95),
                q(&e, 0.5)
            );
        }

        let _ = writeln!(
            out,
            "\n## M-COAL cost with the decode inside the copy loop (µs, per reading), by load1 band\n"
        );
        let _ = writeln!(
            out,
            "| load1 | n | copies alone p50 / p95 | copies + decode p50 / p95 | the decode (difference) p50 / p95 | change by id p50 / p95 | added (decode + change) p50 / p95 |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|");
        for (lo, hi) in [
            (0.0, 10.0),
            (10.0, 20.0),
            (20.0, 40.0),
            (40.0, 1e9),
            (0.0, 1e9),
        ] {
            let band: Vec<&Vec<String>> = rows
                .rows
                .iter()
                .filter(|r| rows.f(r, "l1").is_some_and(|l| l >= lo && l < hi))
                .filter(|r| rows.f(r, "fz").is_some() && rows.f(r, "mv").is_some())
                .collect();
            if band.is_empty() {
                continue;
            }
            let col = |c: &str| sorted(band.iter().filter_map(|r| rows.f(r, c)).collect());
            let decode = sorted(
                band.iter()
                    .filter_map(|r| Some(rows.f(r, "fz")? - rows.f(r, "cz")?))
                    .collect(),
            );
            let added = sorted(
                band.iter()
                    .filter_map(|r| Some(rows.f(r, "fz")? - rows.f(r, "cz")? + rows.f(r, "mv")?))
                    .collect(),
            );
            let (cz, fz, mv) = (col("cz"), col("fz"), col("mv"));
            let label = if hi > 1e8 && lo == 0.0 {
                "all".to_owned()
            } else if hi > 1e8 {
                format!("≥ {lo:.0}")
            } else {
                format!("{lo:.0}–{hi:.0}")
            };
            let _ = writeln!(
                out,
                "| {label} | {} | {:.1} / {:.1} | {:.1} / {:.1} | {:.1} / {:.1} | {:.1} / {:.1} | {:.1} / {:.1} |",
                band.len(),
                q(&cz, 0.5),
                q(&cz, 0.95),
                q(&fz, 0.5),
                q(&fz, 0.95),
                q(&decode, 0.5),
                q(&decode, 0.95),
                q(&mv, 0.5),
                q(&mv, 0.95),
                q(&added, 0.5),
                q(&added, 0.95)
            );
        }

        let _ = writeln!(
            out,
            "\n## Whole time by account, against the busy cores (judged readings; p50 [p5, p95])\n"
        );
        let ratio = |num: &dyn Fn(&[String]) -> Option<f64>,
                     den: &dyn Fn(&[String]) -> Option<f64>| {
            let v = sorted(ok.iter().filter_map(|r| Some(num(r)? / den(r)?)).collect());
            let outside = v.iter().filter(|x| !(0.5..=1.5).contains(*x)).count();
            format!(
                "{:.3} [{:.3}, {:.3}], min {:.3}, max {:.3}, outside 0.5–1.5: {outside}, n={}",
                q(&v, 0.5),
                q(&v, 0.05),
                q(&v, 0.95),
                q(&v, 0.0),
                q(&v, 1.0),
                v.len()
            )
        };
        let rr = &rows;
        let per_win = |c: &'static str| move |r: &[String]| Some(rr.f(r, c)? / rr.f(r, "win")?);
        let stats_cores = |r: &[String]| Some(rows.f(r, "pc")? + rows.f(r, "ec")?);
        let tick_cores = |r: &[String]| Some(rows.f(r, "sb")? * 4.0 + rows.f(r, "eb")? * 6.0);
        let _ = writeln!(
            out,
            "- cpu_time ÷ CPU Stats active cores: {}",
            ratio(&per_win("cpu"), &stats_cores)
        );
        let _ = writeln!(
            out,
            "- QoS time ÷ CPU Stats active cores: {}",
            ratio(&per_win("q"), &stats_cores)
        );
        let _ = writeln!(
            out,
            "- cpu_time ÷ core-tick busy cores: {}",
            ratio(&per_win("cpu"), &tick_cores)
        );
        let _ = writeln!(
            out,
            "- QoS time ÷ core-tick busy cores: {}",
            ratio(&per_win("q"), &tick_cores)
        );
        let _ = writeln!(
            out,
            "- Super time ÷ CPU Stats Super active cores: {}",
            ratio(&per_win("p"), &|r: &[String]| rows.f(r, "pc"))
        );
        let _ = writeln!(
            out,
            "- Super time ÷ core-tick Super busy cores: {}",
            ratio(&per_win("p"), &|r: &[String]| Some(rows.f(r, "sb")? * 4.0))
        );
        let over: Vec<f64> = ok.iter().filter_map(|r| rows.f(r, "po")).collect();
        let _ = writeln!(
            out,
            "- coalitions whose Super time moved past their cpu_time in the reading: in {} of {} \
             readings, at most {}",
            over.iter().filter(|x| **x > 0.0).count(),
            over.len(),
            over.iter().copied().fold(0.0, f64::max)
        );

        for (title, bin, scale) in [
            ("Super busy (core ticks)", "sb", 1.0),
            ("Super cores' worth from coalition P time ÷ 4", "p", 0.0),
        ] {
            for (cmp, label) in [("cp", "complex PCPU"), ("cpp", "per-core PCPU0-3")] {
                let _ = writeln!(
                    out,
                    "\n## M-PCLK: derived P clock against CPU Stats {label}, by {title} decile\n"
                );
                let _ = writeln!(
                    out,
                    "| decile | n | bias p50 | bias p5 / p95 | spread p95 (abs dev from p50) | max dev | load1 p50 | verdict |"
                );
                let _ = writeln!(out, "|---|---|---|---|---|---|---|---|");
                for d in 0..10 {
                    let sel: Vec<&&Vec<String>> = ok
                        .iter()
                        .filter(|r| {
                            let share = if scale > 0.0 {
                                rows.f(r, bin)
                            } else {
                                rows.f(r, "p")
                                    .zip(rows.f(r, "win"))
                                    .map(|(p, w)| p / w / 4.0)
                            };
                            share.is_some_and(|s| (((s * 10.0).floor() as i64).clamp(0, 9)) == d)
                        })
                        .collect();
                    let biases: Vec<f64> = sel
                        .iter()
                        .filter_map(|r| {
                            let (pm, c) = (rows.f(r, "pm")?, rows.f(r, cmp)?);
                            Some((pm / c - 1.0) * 100.0)
                        })
                        .collect();
                    let loads = sorted(sel.iter().filter_map(|r| rows.f(r, "l1")).collect());
                    let (n, p50, p5, p95, sp, max) = spread(biases);
                    let verdict = if n < 20 {
                        "too few: no cut".to_owned()
                    } else if sp <= 2.0 {
                        "pass".to_owned()
                    } else {
                        "fail: no cut".to_owned()
                    };
                    let _ = writeln!(
                        out,
                        "| {d} | {n} | {p50:+.2} % | {p5:+.2} / {p95:+.2} % | {sp:.2} % | {max:.2} % | {:.0} | {verdict} |",
                        q(&loads, 0.5)
                    );
                }
            }
        }

        for ((cmp, label), (ecol, rcol, basis)) in
            [("ce", "complex ECPU"), ("cpe", "per-core ECPU0-5")]
                .into_iter()
                .flat_map(|c| {
                    [
                        (c, ("em", "er", "cpu_time")),
                        (c, ("eq", "eqr", "QoS time")),
                    ]
                })
        {
            let _ = writeln!(
                out,
                "\n## M-ECLK: derived E clock over {basis} against CPU Stats {label}, by Efficiency busy band\n"
            );
            let _ = writeln!(
                out,
                "| E busy | n judged | refused by the bound | within 5 % | bias p50 | bias p5 / p95 | max abs | naive-sum E clock past the top | load1 p50 | verdict |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|---|");
            for (lo, hi, name) in [
                (0.0, 1.0 / 3.0, "low (< 1/3)"),
                (1.0 / 3.0, 2.0 / 3.0, "middling (1/3–2/3)"),
                (2.0 / 3.0, 1.01, "high (≥ 2/3)"),
            ] {
                let band: Vec<&&Vec<String>> = ok
                    .iter()
                    .filter(|r| rows.f(r, "eb").is_some_and(|e| e >= lo && e < hi))
                    .collect();
                let refused = band.iter().filter(|r| rows.s(r, rcol) == "1").count();
                let biases: Vec<f64> = band
                    .iter()
                    .filter_map(|r| {
                        let (em, c) = (rows.f(r, ecol)?, rows.f(r, cmp)?);
                        Some((em / c - 1.0) * 100.0)
                    })
                    .collect();
                let within = biases.iter().filter(|b| b.abs() <= 5.0).count();
                let naive_over = band
                    .iter()
                    .filter(|r| rows.f(r, "nve").is_some_and(|v| v > rows.tops.1 * 1.02))
                    .count();
                let abs = sorted(biases.iter().map(|b| b.abs()).collect());
                let loads = sorted(band.iter().filter_map(|r| rows.f(r, "l1")).collect());
                let n = biases.len();
                let (_, p50, p5, p95, _, _) = spread(biases);
                let share = if n > 0 {
                    within as f64 / n as f64
                } else {
                    f64::NAN
                };
                let verdict = if n < 20 {
                    "too few"
                } else if share >= 0.95 {
                    "pass"
                } else {
                    "fail"
                };
                let _ = writeln!(
                    out,
                    "| {name} | {n} | {refused} | {within} ({:.1} %) | {p50:+.2} % | {p5:+.2} / {p95:+.2} % | {:.2} % | {naive_over} | {:.0} | {verdict} |",
                    share * 100.0,
                    q(&abs, 1.0),
                    q(&loads, 0.5)
                );
            }
        }
        let naive_p = rows
            .rows
            .iter()
            .filter(|r| rows.f(r, "nvp").is_some_and(|v| v > rows.tops.2 * 1.02))
            .count();
        let naive_e = rows
            .rows
            .iter()
            .filter(|r| {
                rows.f(r, "nve")
                    .is_some_and(|v| v > rows.tops.1 * 1.02 || v < rows.tops.0 * 0.5)
            })
            .count();
        let _ = writeln!(
            out,
            "\nnaive Δ-of-sums clocks out of range: P {naive_p}, E {naive_e} of {n_all} readings"
        );
        print!("{out}");
    }
}
