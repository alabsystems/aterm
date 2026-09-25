// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::HashMap;

use super::*;

// ---------------------------------------------------------------------------
// Fixtures: real /proc texts (a 4-core Linux 6.8 box), trimmed.
// ---------------------------------------------------------------------------

const PROC_STAT: &str = "\
cpu  10132153 290696 3084719 46828483 16683 0 25195 0 175628 0
cpu0 1393280 32966 572056 13343292 6130 0 17875 0 23933 0
cpu1 1335085 25544 486386 11420474 3548 0 3431 0 57236 0
intr 1462898 0 0 0 0 0 0 0 0 1 0 0
ctxt 3082453
btime 1726000000
";

const PSI_CPU: &str = "\
some avg10=42.87 avg60=30.10 avg300=12.00 total=123456789
full avg10=0.00 avg60=0.00 avg300=0.00 total=0
";

const PSI_MEMORY: &str = "\
some avg10=10.05 avg60=4.00 avg300=1.00 total=4242
full avg10=5.00 avg60=2.00 avg300=0.50 total=2121
";

const PSI_IO: &str = "\
some avg10=33.30 avg60=20.00 avg300=5.00 total=99
full avg10=21.94 avg60=15.00 avg300=3.00 total=77
";

const MEMINFO: &str = "\
MemTotal:       16318756 kB
MemFree:          812344 kB
MemAvailable:    4079689 kB
Buffers:          204800 kB
Cached:          3310000 kB
SwapTotal:       2097148 kB
SwapFree:        1048576 kB
";

const VMSTAT: &str = "\
nr_free_pages 203086
nr_zone_inactive_anon 100
pgpgin 5123456
pgpgout 6123456
pswpin 1200
pswpout 3400
pgalloc_dma 0
";

const PID_STAT: &str = "4242 (my (odd) prog) R 17 4242 4242 34816 4242 4194304 1000 0 0 0 \
                        250 50 0 0 20 0 1 0 12345 1234567 512 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";

const PID_STATM: &str = "2048 512 128 16 0 400 0\n";

// ---------------------------------------------------------------------------
// Linux parsers.
// ---------------------------------------------------------------------------

#[test]
fn proc_stat_reads_the_aggregate_line_and_skips_guest() {
    // busy = user + nice + system + irq + softirq + steal; guest is inside user.
    let busy = 10_132_153 + 290_696 + 3_084_719 + 25_195;
    assert_eq!(
        linux::cpu_ticks(PROC_STAT),
        Some((busy, busy + 46_828_483 + 16_683))
    );
    assert_eq!(linux::cpu_ticks("cpu0 1 2 3 4\n"), None);
    assert_eq!(linux::cpu_ticks("cpu  1 x 3 4\n"), None);
}

#[test]
fn pressure_files_read_avg10_as_permille() {
    assert_eq!(linux::psi(PSI_CPU), (Some(429), Some(0)));
    assert_eq!(linux::psi(PSI_MEMORY), (Some(101), Some(50)));
    assert_eq!(linux::psi(PSI_IO), (Some(333), Some(219)));
    // An old kernel's cpu file has no `full` line.
    assert_eq!(
        linux::psi("some avg10=100.00 avg60=0 avg300=0 total=1\n"),
        (Some(1000), None)
    );
    let psi = linux::psi_of(Some(PSI_CPU), Some(PSI_MEMORY), Some(PSI_IO)).unwrap();
    assert_eq!(psi.cpu_some_pm, Some(429));
    assert_eq!(psi.memory_some_pm, Some(101));
    assert_eq!(psi.memory_full_pm, Some(50));
    assert_eq!(psi.io_full_pm, Some(219));
    assert_eq!(linux::psi_of(None, None, None), None);
}

#[test]
fn meminfo_gives_size_and_the_used_share() {
    let m = linux::meminfo(MEMINFO).unwrap();
    assert_eq!(m.total_kib, 16_318_756);
    assert_eq!(m.available_kib, 4_079_689);
    assert_eq!(m.mib(), 15_936);
    assert_eq!(m.used_pm(), Some(750));
    assert_eq!(linux::meminfo("MemTotal: 10 kB\n"), None);
}

#[test]
fn vmstat_sums_swap_in_and_out() {
    assert_eq!(linux::swap_pages(VMSTAT), Some(4600));
    assert_eq!(linux::swap_pages("pswpin 3\n"), None);
}

#[test]
fn thermal_trips_map_to_states() {
    assert_eq!(linux::millidegrees("45000\n"), Some(45_000));
    assert_eq!(linux::millidegrees("n/a"), None);
    let trips = [("passive", 85_000), ("critical", 105_000)];
    assert_eq!(linux::zone_thermal(45_000, &trips), Thermal::Nominal);
    assert_eq!(linux::zone_thermal(86_000, &trips), Thermal::Serious);
    assert_eq!(linux::zone_thermal(105_000, &trips), Thermal::Critical);
    // A zero trip is "unset", never crossed.
    assert_eq!(linux::zone_thermal(1, &[("passive", 0)]), Thermal::Nominal);
    assert_eq!(
        linux::zone_thermal(99_000, &[("active", 50_000)]),
        Thermal::Nominal
    );
}

#[test]
fn pid_stat_takes_the_comm_to_the_last_paren() {
    let s = linux::pid_stat(PID_STAT).unwrap();
    assert_eq!(s.pid, 4242);
    assert_eq!(s.comm, "my (odd) prog");
    assert_eq!(s.ppid, 17);
    assert_eq!(s.ticks, 300);
    assert_eq!(linux::statm_resident(PID_STATM), Some(512));
    let row = linux::proc_row(PID_STAT, Some(PID_STATM), 100, 4, true).unwrap();
    assert_eq!(row.cpu_ns, Some(3_000_000_000));
    assert_eq!(row.footprint_kib, Some(2048));
    assert!(row.uid_is_ours);
    assert_eq!(linux::pid_stat("4242 no parens R 1"), None);
    assert_eq!(linux::ticks_ns(1, 0), 0);
}

#[test]
fn a_linux_reading_assembles_from_the_texts() {
    let at = Instant::now();
    let texts = linux::Texts {
        stat: Some(PROC_STAT),
        meminfo: Some(MEMINFO),
        vmstat: Some(VMSTAT),
        psi_cpu: Some(PSI_CPU),
        psi_memory: Some(PSI_MEMORY),
        psi_io: None,
    };
    let r = linux::reading(at, 4, 4, texts, Some(Thermal::Serious));
    assert_eq!(r.cores, 4);
    assert_eq!(r.mem_mib, 15_936);
    assert_eq!(r.mem_used_pm, Some(750));
    assert_eq!(
        r.mem_used_mib,
        Some((16_318_756 - 4_079_689) / 1024),
        "total less available"
    );
    assert_eq!(r.swap_pages, Some(4600));
    assert_eq!(r.pressure, None);
    assert_eq!(r.low_power, None);
    assert_eq!(r.thermal, Some(Thermal::Serious));
    assert_eq!(r.psi.unwrap().io_full_pm, None);
    let none = linux::reading(at, 4, 4, linux::Texts::default(), None);
    assert_eq!(
        none,
        Reading {
            cores: 4,
            page_kib: 4,
            ..Reading::empty(at)
        }
    );
}

// ---------------------------------------------------------------------------
// The named tests of the spec (§11 "Probe").
// ---------------------------------------------------------------------------

#[test]
fn rusage_time_is_scaled_by_the_timebase() {
    // Measured on the m1: 12 000 193 ticks advanced per 500 ms of CPU at a
    // timebase of 125/3.
    assert_eq!(ticks_to_ns(12_000_193, 125, 3), 500_008_041);
    // Intel's timebase is 1/1: ticks are nanoseconds.
    assert_eq!(ticks_to_ns(12_000_193, 1, 1), 12_000_193);
    // No overflow on the way through: 128-bit, saturating.
    assert_eq!(ticks_to_ns(u64::MAX, 125, 3), u64::MAX);
    assert_eq!(ticks_to_ns(u64::MAX / 125, 125, 125), u64::MAX / 125);
    // A missing timebase reads nothing rather than dividing by zero.
    assert_eq!(ticks_to_ns(5, 1, 0), 0);
}

#[test]
fn every_proc_read_is_bounded_to_4_kib() {
    assert_eq!(READ_CAP, 4096);
    assert_eq!(PATH_CAP, 4096);
    assert_eq!(MAX_PIDS, 4096);
    // A device that never ends costs exactly one buffer (the 2026-07-04
    // incident read one to EOF and took the machine down).
    let zero = read_bounded(Path::new("/dev/zero")).expect("/dev/zero opens");
    assert_eq!(zero.len(), READ_CAP);
    // A long file is cut at the cap; a short one is read whole.
    let dir = std::env::temp_dir().join(format!("aterm-sysprobe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let long = dir.join("long");
    let body: String = (0..1000).map(|i| format!("line{i:05}\n")).collect();
    assert!(body.len() > 2 * READ_CAP);
    std::fs::write(&long, &body).unwrap();
    let got = read_bounded(&long).unwrap();
    assert_eq!(got.len(), READ_CAP);
    assert!(body.starts_with(&got));
    let short = dir.join("short");
    std::fs::write(&short, MEMINFO).unwrap();
    assert_eq!(read_bounded(&short).as_deref(), Some(MEMINFO));
    assert_eq!(read_bounded(&dir.join("absent")), None);
    std::fs::remove_dir_all(&dir).unwrap();
    // A text cut mid-line at the cap loses the torn line, never parses half
    // a number: `pswpout 34` of `pswpout 3400` is not read as 34.
    let torn = &VMSTAT[..VMSTAT.find("pswpout 34").unwrap() + "pswpout 34".len()];
    assert_eq!(linux::swap_pages(torn), None);
    let torn_stat = &PROC_STAT[..20];
    assert_eq!(linux::cpu_ticks(torn_stat), None);
}

#[test]
fn eperm_rows_are_kept_unmeasured() {
    let ident = Ident {
        ppid: 1,
        uid: 0,
        comm: "WindowServer".into(),
    };
    let row = row_of(391, Some(ident.clone()), Usage::Refused, 501).unwrap();
    assert_eq!(row.pid, 391);
    assert_eq!(row.ppid, 1);
    assert_eq!(row.name, "WindowServer");
    assert!(!row.uid_is_ours);
    assert_eq!(row.cpu_ns, None);
    assert_eq!(row.footprint_kib, None);
    // Refused without an identity still keeps the pid.
    let bare = row_of(392, None, Usage::Refused, 501).unwrap();
    assert_eq!((bare.pid, bare.ppid, bare.name.as_str()), (392, 0, ""));
    // Gone is the only answer that drops a row.
    assert_eq!(row_of(393, Some(ident), Usage::Gone, 501), None);
    let ours = row_of(
        394,
        Some(Ident {
            ppid: 2,
            uid: 501,
            comm: "cargo".into(),
        }),
        Usage::Measured {
            cpu_ns: 7,
            footprint_kib: 9,
        },
        501,
    )
    .unwrap();
    assert!(ours.uid_is_ours);
    assert_eq!((ours.cpu_ns, ours.footprint_kib), (Some(7), Some(9)));
}

// ---------------------------------------------------------------------------
// The shared pure pieces.
// ---------------------------------------------------------------------------

#[test]
fn cpu_ticks_accumulate_across_a_wrapped_counter() {
    let mut t = CpuTicks::default();
    assert_eq!(t.advance([10, 5, 100, 1]), (16, 116));
    assert_eq!(t.advance([20, 10, 150, 1]), (31, 181));
    // user wraps past u32::MAX: +11 ticks, not −4 billion.
    let mut w = CpuTicks::default();
    w.advance([u32::MAX - 5, 0, 0, 0]);
    let (busy, total) = w.advance([5, 0, 0, 0]);
    assert_eq!(busy, u64::from(u32::MAX - 5) + 11);
    assert_eq!(total, busy);
}

#[test]
fn kernel_levels_map_to_the_engine_words() {
    assert_eq!(pressure_of(1), Some(MemoryLevel::Normal));
    assert_eq!(pressure_of(2), Some(MemoryLevel::Warn));
    assert_eq!(pressure_of(4), Some(MemoryLevel::Critical));
    assert_eq!(pressure_of(3), None);
    assert_eq!(used_of_level(89), Some(110));
    assert_eq!(used_of_level(0), Some(1000));
    assert_eq!(used_of_level(101), None);
    assert_eq!(used_of_level(-1), None);
    assert_eq!(thermal_of(0), Some(Thermal::Nominal));
    assert_eq!(thermal_of(2), Some(Thermal::Serious));
    assert_eq!(thermal_of(4), None);
    assert_eq!(c_text(b"yes\0\0junk"), "yes");
    assert_eq!(c_text(b"sixteen-bytes-ab"), "sixteen-bytes-ab");
}

fn row(pid: u32, cpu_ns: Option<u64>, footprint_kib: Option<u64>) -> ProcRow {
    ProcRow {
        pid,
        ppid: 1,
        uid_is_ours: true,
        name: format!("p{pid}"),
        bundle: None,
        cpu_ns,
        footprint_kib,
    }
}

#[test]
fn only_the_leaders_by_cpu_and_by_footprint_get_a_path() {
    // Twenty processes: cumulative CPU i·1000 now, i·1000 − i² at the last
    // sweep (so the delta i² ranks them), footprint 100·(20 − i).
    let rows: Vec<ProcRow> = (1..=20u32)
        .map(|i| row(i, Some(u64::from(i) * 1000), Some(u64::from(20 - i) * 100)))
        .collect();
    let prev: HashMap<u32, u64> = (1..=20u32)
        .map(|i| (i, u64::from(i) * 1000 - u64::from(i * i)))
        .collect();
    let picked: Vec<u32> = rows_to_name(&rows, &prev)
        .into_iter()
        .map(|i| rows[i].pid)
        .collect();
    assert_eq!(&picked[..NAMED_ROWS], &[20, 19, 18, 17, 16, 15, 14, 13]);
    // …then the heaviest by footprint not already named (pid 20 has none).
    assert_eq!(&picked[NAMED_ROWS..], &[1, 2, 3, 4, 5, 6, 7, 8]);
    // Unmeasured rows and idle rows are never named.
    let quiet = vec![row(1, None, None), row(2, Some(5), Some(0))];
    let prev: HashMap<u32, u64> = [(2, 5)].into_iter().collect();
    assert!(rows_to_name(&quiet, &prev).is_empty());
}

/// A browser runs dozens of helpers: every one past a floor is named — its
/// path read, once per process image — so all of them sum into the app, not
/// only the eight that lead this sweep; the reads stay bounded.
#[test]
fn every_helper_past_a_floor_is_named_not_only_the_leaders() {
    let helper = |pid: u32, ns: u64, kib: u64| ProcRow {
        name: "Google Chrome He".into(),
        ..row(pid, Some(ns), Some(kib))
    };
    // Twenty renderer helpers at 0.1–0.3 cores over a 4 s sweep, and two
    // idle processes under both floors.
    let mut rows: Vec<ProcRow> = (1..=20u32)
        .map(|i| helper(i, 400_000_000 + u64::from(i) * 10_000_000, 90 * 1024))
        .collect();
    rows.push(row(90, Some(1_000), Some(1_024)));
    rows.push(row(91, Some(2_000), Some(2_048)));
    let prev = HashMap::new();
    let picked: Vec<u32> = rows_to_name(&rows, &prev)
        .into_iter()
        .map(|i| rows[i].pid)
        .collect();
    for pid in 1..=20 {
        assert!(picked.contains(&pid), "helper {pid}: {picked:?}");
    }
    // The two under both floors, outside both top-8s, are not.
    assert_eq!(picked.len(), 20, "{picked:?}");
    const { assert!(MAX_PATH_READS >= 2 * NAMED_ROWS) };

    // The cache reaches EVERY row each sweep, so a helper named once stays in
    // its app on later sweeps, whatever its rank; a reused pid (another comm)
    // takes nothing.
    let path =
        "/Applications/Google Chrome.app/Contents/Frameworks/Helper.app/Contents/MacOS/Helper";
    let cache: HashMap<u32, (String, Option<String>)> = [
        (1, ("Google Chrome He".to_string(), Some(path.to_string()))),
        (
            90,
            ("somebody-else".to_string(), Some("/bin/x".to_string())),
        ),
    ]
    .into_iter()
    .collect();
    fill_cached_paths(&mut rows, &cache);
    assert_eq!(rows[0].bundle.as_deref(), Some(path));
    assert_eq!(rows[1].bundle, None, "not cached yet");
    assert_eq!(rows[20].bundle, None, "pid 90 now runs another image");
    // …and the engine sums the named helpers into the app.
    let g = aterm_messages::strain::group(&rows[..1], &[], &[], 0);
    assert_eq!(
        g[0].culprit,
        aterm_messages::strain::Culprit::App("Google Chrome".into())
    );
}

/// Memory in use is Activity Monitor's `Memory Used` — app memory (internal
/// less purgeable) + wired + compressed — measured on this 24 GB m1 at
/// `kern.memorystatus_level` 89, where the old `1000 − 10·level` read 2.6 GB
/// "in use" beside Activity Monitor's ~8 GB.
#[test]
fn memory_in_use_is_counted_from_the_vm_pages() {
    let page = 16_384;
    // 358 721 anonymous (internal) pages, 4 509 purgeable, 118 941 wired,
    // 46 669 occupied by the compressor.
    let used = used_mib_of(358_721, 4_509, 118_941, 46_669, page).unwrap();
    assert_eq!(used, (358_721 - 4_509 + 118_941 + 46_669) * 16 / 1024);
    assert!((7_000..9_000).contains(&used), "{used} MiB");
    assert_eq!(used_mib_of(1, 0, 0, 0, 0), None);
    assert_eq!(
        used_mib_of(1, 5, 0, 0, page),
        Some(0),
        "purgeable past internal"
    );
}

/// The host carries no platform `cfg`: the services' name comes from here,
/// and it is one of the engine's two.
#[test]
fn the_services_noun_is_the_platforms() {
    let want = if cfg!(target_os = "macos") {
        "macOS services"
    } else {
        "system services"
    };
    assert_eq!(SERVICES_NOUN, want);
}

#[test]
fn the_probe_can_move_to_its_thread() {
    fn is_send<T: Send>() {}
    is_send::<Probe>();
}

// ---------------------------------------------------------------------------
// Live, on this host.
// ---------------------------------------------------------------------------

/// Burn this thread's CPU for `ms`.
#[cfg(target_os = "macos")]
fn spin(ms: u64) {
    let until = Instant::now() + std::time::Duration::from_millis(ms);
    let mut x: u64 = 1;
    while Instant::now() < until {
        x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_probe_reads_cpu_memory_and_thermal_on_this_host() {
    let mut probe = Probe::new();
    let a = probe.reading();
    spin(120);
    let b = probe.reading();
    assert!(b.cores > 0, "cores");
    assert!(b.mem_mib >= 1024, "hw.memsize: {} MiB", b.mem_mib);
    assert!(
        matches!(b.page_kib, 4 | 16),
        "vm.pagesize: {} KiB",
        b.page_kib
    );
    let (busy_a, total_a) = a.busy_ticks.expect("HOST_CPU_LOAD_INFO");
    let (busy_b, total_b) = b.busy_ticks.expect("HOST_CPU_LOAD_INFO");
    assert!(busy_b >= busy_a && total_b > total_a, "{a:?} -> {b:?}");
    assert!(busy_b - busy_a <= total_b - total_a);
    assert!(b.pressure.is_some(), "kern.memorystatus_vm_pressure_level");
    assert!(
        b.mem_used_pm.is_some_and(|pm| pm <= 1000),
        "kern.memorystatus_level"
    );
    // Memory IN USE is Activity Monitor's count, never the free share's
    // complement: some of the RAM, and at least this test's own footprint.
    assert!(
        b.mem_used_mib
            .is_some_and(|mib| (16..=b.mem_mib).contains(&mib)),
        "HOST_VM_INFO64 used: {:?} of {} MiB",
        b.mem_used_mib,
        b.mem_mib
    );
    assert!(b.swap_pages.is_some(), "HOST_VM_INFO64");
    assert!(b.swap_pages >= a.swap_pages);
    assert!(b.thermal.is_some(), "thermalState");
    assert!(b.low_power.is_some(), "isLowPowerModeEnabled");
    assert_eq!(b.psi, None);
    assert!(b.at > a.at);

    // The sends' prototypes are the runtime's: NSInteger and BOOL.
    let (thermal, low) = macos::sent_encodings();
    assert!(
        thermal.as_deref().is_some_and(|e| e.starts_with('q')),
        "{thermal:?}"
    );
    assert!(
        low.as_deref()
            .is_some_and(|e| e.starts_with('B') || e.starts_with('c')),
        "{low:?}"
    );

    // The sweep: this process measured and named; the short-info layout
    // reads this process's parent back.
    let me = std::process::id();
    let first = probe.scan();
    spin(120);
    let rows = probe.scan();
    let own = rows
        .iter()
        .find(|r| r.pid == me)
        .expect("own pid in the sweep");
    assert!(own.uid_is_ours);
    assert!(!own.name.is_empty());
    let before = first
        .iter()
        .find(|r| r.pid == me)
        .and_then(|r| r.cpu_ns)
        .unwrap();
    let after = own.cpu_ns.expect("own usage is readable");
    // ≥ 100 ms of the 120 ms spin, in nanoseconds: the timebase is applied.
    assert!(after - before >= 100_000_000, "{before} -> {after}");
    assert!(own.footprint_kib.is_some_and(|k| k > 0));
    let (pid, ident) = macos::own_ident().expect("PROC_PIDT_SHORTBSDINFO");
    assert_eq!(pid, me);
    // SAFETY: no arguments.
    assert_eq!(i64::from(ident.ppid), i64::from(unsafe { libc::getppid() }));
    // `proc_pidpath` answers the resolved path (a symlinked target dir reads
    // through), so compare canonical forms.
    let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let got = std::fs::canonicalize(macos::own_path().expect("proc_pidpath")).unwrap();
    assert_eq!(got, exe, "proc_pidpath");
    assert!(rows.len() > 20 && rows.len() <= MAX_PIDS);
    // Another user's process is kept, unmeasured (launchd is root's). Only
    // meaningful when the test does not itself run as root.
    // SAFETY: no arguments.
    if unsafe { libc::getuid() } != 0 {
        let launchd = rows.iter().find(|r| r.pid == 1).expect("launchd is listed");
        assert_eq!(launchd.cpu_ns, None);
        assert!(!launchd.uid_is_ours);
        assert_eq!(launchd.ppid, 0);
        assert!(rows.iter().filter(|r| r.cpu_ns.is_none()).count() > 10);
    }
    // At most MAX_PATH_READS new paths per sweep: the first sweep names no
    // more than that; the second keeps every one of them (the cache reaches
    // every row whose image is unchanged) and reads at most that many more.
    let named = |rows: &[ProcRow]| rows.iter().filter(|r| r.bundle.is_some()).count();
    assert!(named(&first) <= MAX_PATH_READS, "{}", named(&first));
    assert!(named(&rows) <= 2 * MAX_PATH_READS, "{}", named(&rows));
    for r in first.iter().filter(|r| r.bundle.is_some()) {
        if let Some(now) = rows.iter().find(|x| x.pid == r.pid && x.name == r.name) {
            assert_eq!(now.bundle, r.bundle, "pid {} lost its cached path", r.pid);
        }
    }
}

/// The cost of one reading and one sweep on this host, printed for the
/// record (`--nocapture`). Asserted only against a generous ceiling: this is
/// a measurement, not a benchmark gate.
#[cfg(target_os = "macos")]
#[test]
fn a_reading_and_a_sweep_cost_milliseconds_not_more() {
    let mut probe = Probe::new();
    let _ = probe.reading();
    let _ = probe.scan();
    let (mut read_us, mut scan_us) = (Vec::new(), Vec::new());
    let mut n = 0;
    for _ in 0..10 {
        let t = Instant::now();
        let _ = probe.reading();
        read_us.push(t.elapsed().as_micros());
        let t = Instant::now();
        n = probe.scan().len();
        scan_us.push(t.elapsed().as_micros());
    }
    read_us.sort_unstable();
    scan_us.sort_unstable();
    eprintln!(
        "sysprobe cost: read median {} us max {} us; scan ({n} pids) median {} us max {} us",
        read_us[5], read_us[9], scan_us[5], scan_us[9]
    );
    assert!(read_us[5] < 50_000 && scan_us[5] < 250_000);
}
