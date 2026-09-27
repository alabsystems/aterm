// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Linux arm: PARSERS — pure functions over `&str` — and the [`Sampler`]
//! that feeds them, all compiled and tested on every platform (rulings 210,
//! 227).
//!
//! The sampler reads under a [`Roots`] (`/proc` and `/sys` on a live
//! machine, a fixture tree in the tests: `tests/fixtures/linux`), so its
//! whole path — the directory walk, the pid rows, each process's uid, the
//! thermal zones — runs on this Mac against realistic files. Only a Linux
//! [`crate::Probe`] builds one over the live roots. Each file is read
//! through [`crate::read_bounded`] (one 4 KiB buffer) and handed to these
//! functions, so every parser tolerates a text cut at 4 KiB — a torn last
//! line is ignored, and a fact that was cut off reads `None`, which is never
//! heavy.
//!
//! | file | parser | into |
//! |---|---|---|
//! | `/proc/stat` | [`cpu_ticks`] | `Reading::busy_ticks` |
//! | `/proc/pressure/{cpu,memory,io}` | [`psi`] | `Reading::psi` |
//! | `/proc/meminfo` | [`meminfo`] | `mem_mib`, `mem_used_pm` |
//! | `/proc/vmstat` | [`swap_pages`] | `Reading::swap_pages` |
//! | `thermal_zone*/temp`, `trip_point_*` | [`millidegrees`], [`zone_thermal`] | `Reading::thermal` |
//! | `/proc/<pid>/stat`, `statm` | [`pid_stat`], [`statm_resident`] | [`ProcRow`] |
//! | `/proc/<pid>/status` | [`status_uid`] | `ProcRow::uid_is_ours` |
//! | `/proc/self/auxv` | [`auxv_value`] | the clock tick, the page size |

use crate::{Instant, ProcRow, Psi, Reading, Thermal};

/// Complete lines only: a text cut mid-line (at the 4 KiB cap) loses its
/// last, torn line rather than parsing half a number.
fn whole_lines(text: &str) -> impl Iterator<Item = &str> {
    let end = if text.ends_with('\n') {
        text.len()
    } else {
        text.rfind('\n').map_or(0, |i| i + 1)
    };
    text[..end].lines()
}

/// `(busy, total)` jiffies from the aggregate `cpu ` line of `/proc/stat`:
/// busy is user + nice + system + irq + softirq + steal, total adds idle and
/// iowait. `guest`/`guest_nice` are already inside user/nice and are not
/// counted twice.
#[must_use]
pub fn cpu_ticks(stat: &str) -> Option<(u64, u64)> {
    let line = whole_lines(stat).find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if v.len() < 4 {
        return None;
    }
    let at = |i: usize| v.get(i).copied().unwrap_or(0);
    let busy = at(0) + at(1) + at(2) + at(5) + at(6) + at(7);
    Some((busy, busy + at(3) + at(4)))
}

/// A percentage with up to two decimals (`12.34`) as permille, rounded,
/// capped at 1000.
fn percent_to_pm(s: &str) -> Option<u16> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, "0"));
    let whole: u32 = whole.parse().ok()?;
    let mut hundredths: u32 = 0;
    for (i, ch) in frac.chars().take(2).enumerate() {
        let d = ch.to_digit(10)?;
        hundredths += d * if i == 0 { 10 } else { 1 };
    }
    let pm = (whole * 100 + hundredths + 5) / 10;
    Some(u16::try_from(pm.min(1000)).unwrap_or(1000))
}

/// `(some, full)` `avg10` of one `/proc/pressure/*` file, permille.
#[must_use]
pub fn psi(text: &str) -> (Option<u16>, Option<u16>) {
    let avg10 = |kind: &str| {
        whole_lines(text)
            .find(|l| l.split_whitespace().next() == Some(kind))?
            .split_whitespace()
            .find_map(|f| f.strip_prefix("avg10="))
            .and_then(percent_to_pm)
    };
    (avg10("some"), avg10("full"))
}

/// The engine's [`Psi`] from the three pressure files (any may be absent).
#[must_use]
pub fn psi_of(cpu: Option<&str>, memory: Option<&str>, io: Option<&str>) -> Option<Psi> {
    if cpu.is_none() && memory.is_none() && io.is_none() {
        return None;
    }
    let (cpu_some_pm, _) = cpu.map_or((None, None), psi);
    let (memory_some_pm, memory_full_pm) = memory.map_or((None, None), psi);
    let (_, io_full_pm) = io.map_or((None, None), psi);
    Some(Psi {
        cpu_some_pm,
        memory_some_pm,
        memory_full_pm,
        io_full_pm,
    })
}

/// `/proc/meminfo`'s two facts, KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    /// `MemTotal`.
    pub total_kib: u64,
    /// `MemAvailable`.
    pub available_kib: u64,
}

impl MemInfo {
    /// Physical memory, MiB.
    #[must_use]
    pub fn mib(self) -> u32 {
        u32::try_from(self.total_kib / 1024).unwrap_or(u32::MAX)
    }

    /// Memory in use, MiB: total less available.
    #[must_use]
    pub fn used_mib(self) -> u32 {
        let used = self.total_kib.saturating_sub(self.available_kib);
        u32::try_from(used / 1024).unwrap_or(u32::MAX)
    }

    /// Memory in use, permille.
    #[must_use]
    pub fn used_pm(self) -> Option<u16> {
        if self.total_kib == 0 {
            return None;
        }
        let avail = self.available_kib.min(self.total_kib);
        u16::try_from(1000 - avail * 1000 / self.total_kib).ok()
    }
}

/// The value of a `Key:   123 kB` / `key 123` line.
fn keyed(text: &str, key: &str) -> Option<u64> {
    whole_lines(text).find_map(|l| {
        let mut it = l.split_whitespace();
        if it.next()?.trim_end_matches(':') != key {
            return None;
        }
        it.next()?.parse().ok()
    })
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`.
#[must_use]
pub fn meminfo(text: &str) -> Option<MemInfo> {
    Some(MemInfo {
        total_kib: keyed(text, "MemTotal")?,
        available_kib: keyed(text, "MemAvailable")?,
    })
}

/// `pswpin + pswpout` from `/proc/vmstat` (pages).
#[must_use]
pub fn swap_pages(vmstat: &str) -> Option<u64> {
    Some(keyed(vmstat, "pswpin")?.saturating_add(keyed(vmstat, "pswpout")?))
}

/// A thermal-zone `temp` or `trip_point_*_temp` file: millidegrees Celsius.
#[must_use]
pub fn millidegrees(text: &str) -> Option<i64> {
    text.trim().parse().ok()
}

/// One zone's state from its temperature and its trip points
/// `(type, millidegrees)`: past a `critical` or `hot` trip reads
/// [`Thermal::Critical`], past a `passive` trip [`Thermal::Serious`]
/// (the kernel is throttling), else [`Thermal::Nominal`].
#[must_use]
pub fn zone_thermal(temp_mc: i64, trips: &[(&str, i64)]) -> Thermal {
    let past = |kinds: &[&str]| {
        trips
            .iter()
            .any(|(t, at)| kinds.contains(&t.trim()) && *at > 0 && temp_mc >= *at)
    };
    if past(&["critical", "hot"]) {
        Thermal::Critical
    } else if past(&["passive"]) {
        Thermal::Serious
    } else {
        Thermal::Nominal
    }
}

/// `/proc/<pid>/stat`'s facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PidStat {
    /// Field 1.
    pub pid: u32,
    /// Field 2, without its parentheses (it may itself hold spaces and
    /// parentheses: everything to the LAST `)`).
    pub comm: String,
    /// Field 4.
    pub ppid: u32,
    /// Fields 14 + 15, `utime + stime`, in clock ticks.
    pub ticks: u64,
}

/// Parse `/proc/<pid>/stat`.
#[must_use]
pub fn pid_stat(text: &str) -> Option<PidStat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let pid = text[..open].trim().parse().ok()?;
    let comm = text[open + 1..close].to_string();
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // After the comm, field 3 (state) is index 0.
    let field = |n: usize| rest.get(n - 3).and_then(|s| s.parse::<u64>().ok());
    Some(PidStat {
        pid,
        comm,
        ppid: u32::try_from(field(4)?).ok()?,
        ticks: field(14)?.saturating_add(field(15)?),
    })
}

/// Resident pages from `/proc/<pid>/statm` (its second field).
#[must_use]
pub fn statm_resident(text: &str) -> Option<u64> {
    text.split_whitespace().nth(1)?.parse().ok()
}

/// Clock ticks (`_SC_CLK_TCK`, usually 100) to nanoseconds, saturating.
#[must_use]
pub fn ticks_ns(ticks: u64, clk_tck: u64) -> u64 {
    if clk_tck == 0 {
        return 0;
    }
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(clk_tck)).unwrap_or(u64::MAX)
}

/// One sweep row from a pid's `stat` and `statm`: cumulative CPU and the
/// resident set (`statm` × `page_kib`) as the footprint. `None` when `stat`
/// does not parse (the process went away mid-read).
#[must_use]
pub fn proc_row(
    stat: &str,
    statm: Option<&str>,
    clk_tck: u64,
    page_kib: u32,
    uid_is_ours: bool,
) -> Option<ProcRow> {
    let s = pid_stat(stat)?;
    Some(ProcRow {
        pid: s.pid,
        ppid: s.ppid,
        uid_is_ours,
        name: s.comm,
        bundle: None,
        cpu_ns: Some(ticks_ns(s.ticks, clk_tck)),
        footprint_kib: statm
            .and_then(statm_resident)
            .map(|p| p.saturating_mul(u64::from(page_kib))),
    })
}

/// The texts one Linux reading is made from; any may be missing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Texts<'a> {
    /// `/proc/stat`.
    pub stat: Option<&'a str>,
    /// `/proc/meminfo`.
    pub meminfo: Option<&'a str>,
    /// `/proc/vmstat`.
    pub vmstat: Option<&'a str>,
    /// `/proc/pressure/cpu`.
    pub psi_cpu: Option<&'a str>,
    /// `/proc/pressure/memory`.
    pub psi_memory: Option<&'a str>,
    /// `/proc/pressure/io`.
    pub psi_io: Option<&'a str>,
}

/// One [`Reading`] from the texts: the hottest zone's state (already folded
/// by the caller through [`zone_thermal`]) and the page size in KiB come
/// in beside them. Linux has no kernel pressure LEVEL (PSI stands in) and no
/// Low Power Mode.
#[must_use]
pub fn reading(
    at: Instant,
    cores: u16,
    page_kib: u32,
    texts: Texts<'_>,
    thermal: Option<Thermal>,
) -> Reading {
    let mem = texts.meminfo.and_then(meminfo);
    Reading {
        at,
        cores,
        mem_mib: mem.map_or(0, MemInfo::mib),
        busy_ticks: texts.stat.and_then(cpu_ticks),
        pressure: None,
        mem_used_pm: mem.and_then(MemInfo::used_pm),
        mem_used_mib: mem.map(MemInfo::used_mib),
        swap_pages: texts.vmstat.and_then(swap_pages),
        page_kib,
        thermal,
        low_power: None,
        psi: psi_of(texts.psi_cpu, texts.psi_memory, texts.psi_io),
    }
}

// ---------------------------------------------------------------------------
// The live sampler, over a configurable root (ruling 227).
// ---------------------------------------------------------------------------

/// `AT_PAGESZ` in the auxiliary vector.
const AT_PAGESZ: u64 = 6;
/// `AT_CLKTCK` in the auxiliary vector.
const AT_CLKTCK: u64 = 17;
/// `USER_HZ` on every architecture aterm ships for, the fallback when
/// `/proc/self/auxv` cannot be read.
pub const DEFAULT_CLK_TCK: u64 = 100;
/// The page size, KiB, when `/proc/self/auxv` cannot be read.
pub const DEFAULT_PAGE_KIB: u32 = 4;
/// Thermal zones read per reading, at most.
pub const MAX_ZONES: usize = 64;
/// Trip points read per zone, at most.
pub const MAX_TRIPS: usize = 16;

/// One value of a binary auxiliary vector (`/proc/self/auxv`): native-endian
/// `(key, value)` pairs of the platform's word, ended by `AT_NULL`. `None`
/// when the key is absent or the bytes are cut.
#[must_use]
pub fn auxv_value(bytes: &[u8], key: u64) -> Option<u64> {
    const W: usize = std::mem::size_of::<usize>();
    let word = |b: &[u8]| -> u64 {
        let mut a = [0u8; W];
        a.copy_from_slice(b);
        u64::try_from(usize::from_ne_bytes(a)).unwrap_or(u64::MAX)
    };
    let (pairs, _) = bytes.as_chunks::<{ 2 * W }>();
    pairs.iter().find_map(|pair| {
        let k = word(&pair[..W]);
        (k == key).then(|| word(&pair[W..]))
    })
}

/// The logical CPUs `/proc/stat` lists (`cpu0` … `cpuN` lines), counted
/// only when the block is whole — a later line began — so a many-core
/// machine whose list the 4 KiB cap cut reads `None`, and the caller asks
/// the scheduler instead. The line that ends the block may itself be TORN:
/// on real hardware the `intr` line right after the `cpuN` lines carries one
/// count per IRQ, often thousands of bytes, so the cap cuts it on most
/// machines. Its first bytes are enough — any text that is not `cpu…` and
/// cannot be the start of one (`c`, `cp`) ends the block (review round 9).
#[must_use]
pub fn stat_cores(stat: &str) -> Option<u16> {
    let mut n: u16 = 0;
    for l in stat.split('\n') {
        match l.strip_prefix("cpu") {
            Some(rest) => {
                if rest.starts_with(|c: char| c.is_ascii_digit()) {
                    n = n.saturating_add(1);
                }
            }
            None if l.is_empty() || "cpu".starts_with(l) => {}
            None => return (n > 0).then_some(n),
        }
    }
    None
}

/// The EFFECTIVE uid from a `/proc/<pid>/status` text (`Uid:` real,
/// effective, saved, filesystem).
#[must_use]
pub fn status_uid(status: &str) -> Option<u32> {
    let line = whole_lines(status).find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

/// Where the sampler reads: `/proc` and `/sys` on a live machine, a fixture
/// tree in the tests — so every path it opens is `root.join(…)`, and nothing
/// here is Linux-only but the default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    /// The procfs mount (`/proc`).
    pub proc: std::path::PathBuf,
    /// The sysfs mount (`/sys`).
    pub sys: std::path::PathBuf,
}

impl Roots {
    /// The live machine's `/proc` and `/sys`.
    #[must_use]
    pub fn live() -> Self {
        Self {
            proc: "/proc".into(),
            sys: "/sys".into(),
        }
    }
}

/// The Linux samplers: one reading of `/proc` and the thermal zones, one
/// sweep of `/proc/<pid>`. Every file goes through [`crate::read_bounded`]
/// (one 4 KiB buffer); a sweep lists at most [`crate::MAX_PIDS`] pids and
/// reads at most [`crate::MAX_PATH_READS`] new `exe` links, cached per
/// `(pid, comm)` like the macOS arm's paths. No thread, no timer.
#[derive(Debug)]
pub struct Sampler {
    roots: Roots,
    cores: u16,
    clk_tck: u64,
    page_kib: u32,
    our_uid: Option<u32>,
    /// The last sweep's cumulative CPU per pid (for [`crate::rows_to_name`]).
    prev: std::collections::HashMap<u32, u64>,
    /// `pid -> (comm, exe)`: a link is read once per process image.
    paths: std::collections::HashMap<u32, (String, Option<String>)>,
}

impl Sampler {
    /// Read the fixed facts once under `roots`: the clock tick and page size
    /// (`self/auxv`, else [`DEFAULT_CLK_TCK`] and [`DEFAULT_PAGE_KIB`]), this
    /// process's effective uid (`self/status`) and the cores (`stat`, else the
    /// scheduler's count).
    #[must_use]
    pub fn new(roots: Roots) -> Self {
        let auxv = crate::read_bounded_bytes(&roots.proc.join("self/auxv")).ok();
        let aux = |key| {
            auxv.as_deref()
                .and_then(|b| auxv_value(b, key))
                .filter(|v| *v > 0)
        };
        let clk_tck = aux(AT_CLKTCK).unwrap_or(DEFAULT_CLK_TCK);
        let page_kib = aux(AT_PAGESZ)
            .and_then(|b| u32::try_from(b / 1024).ok())
            .filter(|k| *k > 0)
            .unwrap_or(DEFAULT_PAGE_KIB);
        let our_uid = crate::read_bounded(&roots.proc.join("self/status"))
            .as_deref()
            .and_then(status_uid);
        let cores = crate::read_bounded(&roots.proc.join("stat"))
            .as_deref()
            .and_then(stat_cores)
            .unwrap_or_else(crate::logical_cores);
        Self {
            roots,
            cores,
            clk_tck,
            page_kib,
            our_uid,
            prev: std::collections::HashMap::new(),
            paths: std::collections::HashMap::new(),
        }
    }

    /// The clock tick, page size (KiB), cores and effective uid it read.
    #[must_use]
    pub fn facts(&self) -> (u64, u32, u16, Option<u32>) {
        (self.clk_tck, self.page_kib, self.cores, self.our_uid)
    }

    fn proc_text(&self, rel: &str) -> Option<String> {
        crate::read_bounded(&self.roots.proc.join(rel))
    }

    /// The hottest thermal zone's state ([`zone_thermal`]); `None` with no
    /// zone whose temperature reads.
    fn thermal(&self) -> Option<Thermal> {
        let dir = self.roots.sys.join("class/thermal");
        let zones = std::fs::read_dir(&dir).ok()?;
        let mut worst: Option<Thermal> = None;
        for zone in zones
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("thermal_zone"))
            .take(MAX_ZONES)
        {
            let path = zone.path();
            let Some(temp) = crate::read_bounded(&path.join("temp"))
                .as_deref()
                .and_then(millidegrees)
            else {
                continue;
            };
            let mut trips: Vec<(String, i64)> = Vec::new();
            for i in 0..MAX_TRIPS {
                let Some(kind) = crate::read_bounded(&path.join(format!("trip_point_{i}_type")))
                else {
                    break;
                };
                let at = crate::read_bounded(&path.join(format!("trip_point_{i}_temp")))
                    .as_deref()
                    .and_then(millidegrees)
                    .unwrap_or(0);
                trips.push((kind.trim().to_string(), at));
            }
            let trips: Vec<(&str, i64)> = trips.iter().map(|(k, t)| (k.as_str(), *t)).collect();
            let state = zone_thermal(temp, &trips);
            worst = Some(worst.map_or(state, |w| w.max(state)));
        }
        worst
    }

    /// One reading, stamped `at`.
    pub fn reading(&mut self, at: Instant) -> Reading {
        let stat = self.proc_text("stat");
        let meminfo = self.proc_text("meminfo");
        let vmstat = self.proc_text("vmstat");
        let psi_cpu = self.proc_text("pressure/cpu");
        let psi_memory = self.proc_text("pressure/memory");
        let psi_io = self.proc_text("pressure/io");
        let texts = Texts {
            stat: stat.as_deref(),
            meminfo: meminfo.as_deref(),
            vmstat: vmstat.as_deref(),
            psi_cpu: psi_cpu.as_deref(),
            psi_memory: psi_memory.as_deref(),
            psi_io: psi_io.as_deref(),
        };
        let thermal = self.thermal();
        reading(at, self.cores, self.page_kib, texts, thermal)
    }

    /// One row per pid directory: `stat` (gone ⇒ no row; refused ⇒ kept
    /// unmeasured, its time the services'), `statm` for the resident set and
    /// `status` for the effective uid. A row whose uid does not read is not
    /// ours.
    fn row(&self, pid: u32) -> Option<ProcRow> {
        let dir = self.roots.proc.join(pid.to_string());
        let stat = match crate::read_bounded_kind(&dir.join("stat")) {
            Ok(text) => text,
            Err(std::io::ErrorKind::PermissionDenied) => {
                // No identity to read: an empty name, no parent, and (uid 0
                // against no uid) not ours.
                return crate::row_of(pid, None, crate::Usage::Refused, u32::MAX);
            }
            Err(_) => return None,
        };
        let statm = crate::read_bounded(&dir.join("statm"));
        let uid = crate::read_bounded(&dir.join("status"))
            .as_deref()
            .and_then(status_uid);
        let ours = uid.is_some() && uid == self.our_uid;
        let row = proc_row(&stat, statm.as_deref(), self.clk_tck, self.page_kib, ours)?;
        // The directory names the process; a `stat` that names another pid
        // is not this row's.
        (row.pid == pid).then_some(row)
    }

    /// One sweep of the process table, cumulative CPU times (the engine
    /// takes the deltas): every numeric entry of the proc root, at most
    /// [`crate::MAX_PIDS`], then the `exe` links of the rows worth naming.
    pub fn scan(&mut self) -> Vec<ProcRow> {
        let Ok(dir) = std::fs::read_dir(&self.roots.proc) else {
            return Vec::new();
        };
        let mut pids: Vec<u32> = dir
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok()))
            .take(crate::MAX_PIDS)
            .collect();
        pids.sort_unstable();
        let mut rows: Vec<ProcRow> = pids.into_iter().filter_map(|pid| self.row(pid)).collect();
        crate::fill_cached_paths(&mut rows, &self.paths);
        let mut reads = 0;
        for i in crate::rows_to_name(&rows, &self.prev) {
            if reads == crate::MAX_PATH_READS {
                break;
            }
            let row = &mut rows[i];
            let cached = self
                .paths
                .get(&row.pid)
                .is_some_and(|(comm, _)| *comm == row.name);
            if cached {
                continue;
            }
            reads += 1;
            let exe = std::fs::read_link(self.roots.proc.join(format!("{}/exe", row.pid)))
                .ok()
                .map(|p| {
                    let mut s = p.to_string_lossy().into_owned();
                    let mut cap = s.len().min(crate::PATH_CAP);
                    while !s.is_char_boundary(cap) {
                        cap -= 1;
                    }
                    s.truncate(cap);
                    s
                });
            self.paths.insert(row.pid, (row.name.clone(), exe.clone()));
            row.bundle = exe;
        }
        self.prev = rows
            .iter()
            .filter_map(|r| r.cpu_ns.map(|ns| (r.pid, ns)))
            .collect();
        let live: std::collections::HashSet<u32> = rows.iter().map(|r| r.pid).collect();
        self.paths.retain(|pid, _| live.contains(pid));
        rows
    }
}
