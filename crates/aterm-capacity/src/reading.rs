// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ONE LOOK AT THE MACHINE: cores, load, memory, swap and one volume ([`Reading`]), and each
//! process's parent, footprint and bytes written ([`Procs`]), read through a [`Machine`]: the
//! live one ([`Live`], aterm-sysprobe's readers) or a fixed one a test sets ([`Fixed`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(any(test, debug_assertions))]
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use aterm_sysprobe::volumes::{self, Table, Unread};

use crate::floor::GIB;

/// One volume, as a [`Reading`] holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Where it is mounted.
    pub mount: PathBuf,
    /// Its size, bytes.
    pub total: u64,
    /// Bytes an unprivileged writer may still write (`f_bavail`).
    pub free: u64,
    /// The headroom admission retains ([`crate::floor::admission_floor`]): the emergency
    /// line, or a nonzero floor the owner wrote, separate from the watch's cleanup target.
    pub floor: u64,
    /// The room the floor is raised to: the second-largest daily two-hour fall of free space the watch measured over a week ([`crate::room`]), or `disk.room_gib` where written.
    pub room: u64,
}

impl Volume {
    /// Bytes free above the floor.
    #[must_use]
    pub const fn above_floor(&self) -> u64 {
        self.free.saturating_sub(self.floor)
    }

    /// The most a disk need can ever have here: the volume's size above its floor.
    #[must_use]
    pub const fn ceiling(&self) -> u64 {
        self.total.saturating_sub(self.floor)
    }
}

/// The least memory margin a job keeps ([`Reading::memory_margin`]): 256 MiB.
pub const MEMORY_MARGIN_FLOOR: u64 = 256 << 20;

/// The most ([`Reading::memory_margin`]): 8 GiB.
pub const MEMORY_MARGIN_CEILING: u64 = 8 * GIB;

/// THE MACHINE NOW. Each figure is `None` where this OS does not report it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// Logical cores.
    pub cores: u32,
    /// The 1-minute load average.
    pub load1: Option<f64>,
    /// The 5-minute load average.
    pub load5: Option<f64>,
    /// Memory a claim can take now, bytes: the machine's available memory
    /// (`aterm_sysprobe::MemoryDetail::available`: on macOS the free pages, the inactive ones
    /// file-backed pages could fill, purgeable pages, and what compressing the anonymous inactive
    /// rest frees at the compressor's ratio; Linux's `MemAvailable`; Windows' `ullAvailPhys`).
    pub available: Option<u64>,
    /// Physical memory, bytes.
    pub physical: Option<u64>,
    /// Memory wired down, bytes, which [`Self::available`] never holds: macOS's wired pages
    /// (the kernel's own, and what a driver holds wired for a process, Metal's buffers on Apple
    /// silicon), Linux's unreclaimable slab, kernel stacks and page tables. Read only where
    /// [`Self::available`] is, since only there is admission held to what is available.
    pub wired: Option<u64>,
    /// Swap in use, bytes.
    pub swap_used: Option<u64>,
    /// Swap allocated, bytes.
    pub swap_total: Option<u64>,
    /// The volume holding the path the reading was taken for.
    pub volume: Option<Volume>,
    /// The kernel's memory pressure level (macOS `kern.memorystatus_vm_pressure_level`,
    /// `aterm_sysprobe::memory_pressure`): at [`aterm_sysprobe::MemoryLevel::Critical`], about to
    /// end processes, no memory claim is admitted ([`crate::Short::Pressure`]). `None` off macOS.
    pub pressure: Option<aterm_sysprobe::MemoryLevel>,
    /// How busy the GPU is, percent: the accelerator's own device utilization
    /// (`aterm_sysprobe::gpu`), the work every process keeps on the GPU that timed work renders
    /// on. `None` where it is unread.
    pub gpu_busy: Option<u8>,
}

impl Reading {
    /// Cores the 5-minute load leaves idle (`aterm ctl await sys`'s `cores_idle`): the cores less the load, never below 0.
    #[must_use]
    pub fn idle_cores(&self) -> Option<f64> {
        self.load5.map(|l| (f64::from(self.cores) - l).max(0.0))
    }

    /// THE MOST MEMORY THAT CAN EVER BE AVAILABLE HERE, bytes: physical memory less what is
    /// wired ([`Self::wired`]), or all of it where nothing wired is read. Available memory
    /// ([`Self::available`]) never holds a wired page, an active one, nor what the compressor
    /// occupies, so it always stays below this, and a need of this much or more never fits
    /// ([`crate::never`] refuses it, unless it asks to run alone). This Mac (48 GiB,
    /// 2026-10-25): 3.6 GiB wired, so 44.4 GiB, with 21 to 27 GiB of free, inactive and
    /// purgeable pages under its usual load. `None` where physical memory is unread.
    #[must_use]
    pub fn reachable(&self) -> Option<u64> {
        self.physical
            .map(|p| p.saturating_sub(self.wired.filter(|w| *w < p).unwrap_or(0)))
    }

    /// THE MEMORY A RESOURCE-HOST JOB KEEPS SPARE BESIDE ITS PEAK, bytes ([`crate::Need::margin`]):
    /// an eighth of the memory available now ([`Self::available`]), at least
    /// [`MEMORY_MARGIN_FLOOR`] and at most [`MEMORY_MARGIN_CEILING`]; the floor where nothing
    /// available is read.
    #[must_use]
    pub fn memory_margin(&self) -> u64 {
        self.available.map_or(MEMORY_MARGIN_FLOOR, |bytes| {
            (bytes / 8).clamp(MEMORY_MARGIN_FLOOR, MEMORY_MARGIN_CEILING)
        })
    }

    /// The live machine now, its volume the one holding `path` ([`Live`]).
    #[must_use]
    pub fn now(path: &Path) -> Reading {
        Live.read(Some(path))
    }

    /// The reading in one line, as a log carries it: `load 3.2 on 14 cores, 30.1 GiB of 48 GiB
    /// available, swap 0.3 GiB, GPU 4% busy`; a figure this OS does not report is left out.
    #[must_use]
    pub fn brief(&self) -> String {
        between(self, self)
    }
}

/// Two readings of one machine in one line, `from` then `to`, a figure shown once where it held:
/// `load 3.2→5.1 on 14 cores, 30.1→29.8 GiB of 48 GiB available, swap 0.3 GiB, GPU 0→100%
/// busy`. What a run's log says the machine did while a step ran, so a step refused as starved
/// explains itself.
#[must_use]
pub fn between(from: &Reading, to: &Reading) -> String {
    use std::fmt::Write as _;
    let write_pair = |out: &mut String, a: Option<f64>, b: Option<f64>| {
        let start = out.len();
        match a {
            Some(v) => {
                let _ = write!(out, "{v:.1}");
            }
            None => out.push('?'),
        }
        let a_end = out.len();
        out.push('→');
        let b_start = out.len();
        match b {
            Some(v) => {
                let _ = write!(out, "{v:.1}");
            }
            None => out.push('?'),
        }
        if out[start..a_end] == out[b_start..] {
            out.truncate(a_end);
        }
    };
    let mut out = String::with_capacity(96);
    out.push_str("load ");
    write_pair(&mut out, from.load1, to.load1);
    let _ = write!(out, " on {} cores", to.cores);
    if from.available.is_some() || to.available.is_some() {
        out.push_str(", ");
        write_pair(
            &mut out,
            from.available.map(tenths),
            to.available.map(tenths),
        );
        out.push_str(" GiB");
        if let Some(p) = to.physical {
            let _ = write!(out, " of {}", crate::gib(p));
        }
        out.push_str(" available");
    }
    if from.swap_used.is_some() || to.swap_used.is_some() {
        out.push_str(", swap ");
        write_pair(
            &mut out,
            from.swap_used.map(tenths),
            to.swap_used.map(tenths),
        );
        out.push_str(" GiB");
    }
    if from.gpu_busy.is_some() || to.gpu_busy.is_some() {
        let pct = |out: &mut String, p: Option<u8>| match p {
            Some(p) => {
                let _ = write!(out, "{p}");
            }
            None => out.push('?'),
        };
        out.push_str(", GPU ");
        pct(&mut out, from.gpu_busy);
        if from.gpu_busy != to.gpu_busy {
            out.push('→');
            pct(&mut out, to.gpu_busy);
        }
        out.push_str("% busy");
    }
    out
}

/// `bytes` in GiB to a tenth.
fn tenths(bytes: u64) -> f64 {
    (bytes as f64 / GIB as f64 * 10.0).round() / 10.0
}

/// This machine's logical cores, at least one.
#[must_use]
pub fn cores() -> u32 {
    u32::try_from(aterm_sysprobe::cpu_layout().logical_cores())
        .unwrap_or(u32::MAX)
        .max(1)
}

impl Reading {
    /// A reading of `physical` GiB with `available` GiB of it available, `cores` cores and no load: what a test sets.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn of_gib(physical: u64, available: u64, cores: u32) -> Reading {
        Reading {
            cores,
            load1: Some(0.0),
            load5: Some(0.0),
            available: Some(available.saturating_mul(GIB)),
            physical: Some(physical.saturating_mul(GIB)),
            wired: None,
            swap_used: Some(0),
            swap_total: Some(0),
            volume: None,
            pressure: None,
            gpu_busy: None,
        }
    }

    /// This reading at memory pressure `level` ([`Self::pressure`]): what a test sets.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn at_pressure(mut self, level: aterm_sysprobe::MemoryLevel) -> Reading {
        self.pressure = Some(level);
        self
    }

    /// This reading with `gib` GiB of its memory wired ([`Self::wired`]): what a test sets.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn wired_gib(mut self, gib: u64) -> Reading {
        self.wired = Some(gib.saturating_mul(GIB));
        self
    }
}

/// One process, as the ledger reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Proc {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// The memory a claim's reservation charges it, bytes ([`Proc::of_reading`]: the lesser of
    /// its footprint and its resident size, on macOS its physical footprint and resident size, on
    /// Linux `RssAnon`): 0 where unread.
    pub charged: u64,
    /// Its footprint, bytes, the pages the kernel compressed or swapped out included
    /// (`ProcessReading::footprint`, which `aterm_sysprobe::procs::footprint` reads for one pid):
    /// never less than [`Self::charged`] as [`Proc::of_reading`] reads it, the same where only
    /// one figure is read, 0 where unread. No reservation counts it; what a holder's tree holds
    /// past its claims is reckoned by it ([`crate::Overrun`]).
    pub footprint: u64,
    /// Bytes it has written to disk since it started: 0 where unread (Linux).
    pub written: u64,
    /// The CPU time it and the children it reaped have used, nanoseconds
    /// (`ProcessReading::cpu_ns` with `child_cpu_ns`): 0 where unread.
    pub cpu_ns: u64,
    /// The time its threads were runnable, on a core or waiting for one, with the CPU time of
    /// the children it reaped, nanoseconds (`ProcessReading::runnable_ns` with `child_cpu_ns`):
    /// 0 where unread (off macOS), its CPU time then standing for it.
    pub runnable_ns: u64,
}

/// A process tree's use: what a claimant already takes, so its claims' reservations count only the rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeUse {
    /// The tree's charges ([`Proc::charged`]), summed: what its claims' reservations count.
    pub charged: u64,
    /// The tree's footprints ([`Proc::footprint`]), summed: what it holds, compressed and
    /// swapped pages included.
    pub footprint: u64,
    /// The tree's bytes written, summed.
    pub written: u64,
}

/// Every process of one sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Procs {
    procs: Vec<Proc>,
}

impl Procs {
    /// These processes.
    #[must_use]
    pub fn of(procs: Vec<Proc>) -> Procs {
        Procs { procs }
    }

    /// The processes `pid` descends from, its parent first, as far as the sweep holds them.
    #[must_use]
    pub fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        if self.procs.len() <= 64 {
            let mut at = pid;
            while let Some(up) = self.procs.iter().find(|p| p.pid == at).map(|p| p.ppid) {
                if up == at || up == pid || out.contains(&up) {
                    break;
                }
                out.push(up);
                at = up;
            }
            return out;
        }
        let mut parent: HashMap<u32, u32> = HashMap::with_capacity(self.procs.len());
        parent.extend(self.procs.iter().map(|p| (p.pid, p.ppid)));
        let mut at = pid;
        while let Some(&up) = parent.get(&at) {
            if up == at || up == pid || out.contains(&up) {
                break;
            }
            out.push(up);
            at = up;
        }
        out
    }

    /// What `pid` and every process descended from it have done ([`aterm_toolchain::stall`]),
    /// a claimant's included, with the trees of `credited` (the processes a claim of `pid`'s is
    /// credited with, [`crate::Record::credit`]: its VM's, outside its tree), and whether one of
    /// them waits in its turn, by its mark among `waits` ([`Machine::waits`]); `None` where the
    /// sweep does not hold `pid`.
    #[must_use]
    pub fn work(
        &self,
        pid: u32,
        credited: &[u32],
        waits: &[aterm_toolchain::waits::Waiting],
    ) -> Option<aterm_toolchain::stall::Work> {
        if !self.procs.iter().any(|p| p.pid == pid) {
            return None;
        }
        let procs: Vec<aterm_toolchain::stall::Proc> = self
            .procs
            .iter()
            .map(|p| aterm_toolchain::stall::Proc {
                pid: p.pid,
                ppid: p.ppid,
                cpu_ns: p.cpu_ns,
                runnable_ns: p.runnable_ns,
                written: p.written,
            })
            .collect();
        aterm_toolchain::stall::work_with(pid, credited, &procs, waits)
    }

    /// The use of `pid` and every process descended from it.
    #[must_use]
    pub fn tree(&self, pid: u32) -> TreeUse {
        self.trees(&[pid]).remove(&pid).unwrap_or_default()
    }

    /// The use of each of `roots` and the processes descended from it, each process counted
    /// once, toward its nearest ancestor among `roots` (itself when it is one): a claimant's
    /// tree never holds another claimant's, so no process is credited to two claims.
    #[must_use]
    pub fn trees(&self, roots: &[u32]) -> HashMap<u32, TreeUse> {
        if roots.is_empty() {
            return HashMap::new();
        }
        let mut parent: HashMap<u32, u32> = HashMap::with_capacity(self.procs.len());
        parent.extend(self.procs.iter().map(|p| (p.pid, p.ppid)));
        let mut out: HashMap<u32, TreeUse> = HashMap::with_capacity(roots.len());
        out.extend(roots.iter().map(|r| (*r, TreeUse::default())));
        for p in &self.procs {
            let mut at = p.pid;
            let mut steps = 0usize;
            while steps <= self.procs.len() {
                if let Some(sum) = out.get_mut(&at) {
                    sum.charged = sum.charged.saturating_add(p.charged);
                    sum.footprint = sum.footprint.saturating_add(p.footprint);
                    sum.written = sum.written.saturating_add(p.written);
                    break;
                }
                match parent.get(&at) {
                    Some(&up) if up != at && steps < self.procs.len() => {
                        at = up;
                        steps += 1;
                    }
                    _ => break,
                }
            }
        }
        out
    }
}

/// What the ledger reads of the machine: the live one ([`Live`]) or a fixed one ([`Fixed`]), so no test reaches a platform reader.
pub trait Machine {
    /// Whether this admission belongs to a live release. Injected machines are inert,
    /// so tests of isolated ledgers never inherit the verifier's scheduling priority.
    fn release_run(&self) -> bool {
        false
    }
    /// The machine now, its volume the one holding `path` (`None`: no volume).
    fn read(&self, path: Option<&Path>) -> Reading;

    /// Every process now; `None` where the platform has no sweep.
    fn procs(&self) -> Option<Procs>;

    /// One actual CPU counter point; unsupported/injected machines report unknown by default.
    fn cpu_point(&self) -> Option<crate::CpuPoint> {
        None
    }

    /// `pid` and the processes it descends from, as far as they are read: what deciding whether
    /// an ancestor's claim covers `pid` needs ([`crate::covering`]), with no footprints. By
    /// default the whole sweep ([`Self::procs`]).
    fn lineage(&self, pid: u32) -> Option<Procs> {
        let _ = pid;
        self.procs()
    }

    /// How busy the GPU has been of late, percent: [`Reading::gpu_busy`] at its most over
    /// [`GPU_WINDOW`], what a quiet claim's look holds to [`crate::QUIET_GPU_BUSY`]. One reading
    /// can fall in a pause between another process's command buffers, which the window spans; the
    /// read ends early at a reading above the bound, which decides it, at a GPU its first read
    /// cannot read, or once `stopped` says so (an interrupt that ends the wait), with what it read
    /// by then. By default one reading's.
    fn gpu_busy(&self, stopped: &dyn Fn() -> bool) -> Option<u8> {
        let _ = stopped;
        self.read(None).gpu_busy
    }

    /// The waits marked in this user's waits now, but this process's own: what tells a holder
    /// that waits in its turn ([`aterm_toolchain::stall::Waits`]). By default none.
    fn waits(&self) -> Vec<aterm_toolchain::waits::Waiting> {
        Vec::new()
    }
}

/// How long [`Live`]'s [`Machine::gpu_busy`] reads the GPU for: longer than the pause between
/// the long command buffers of a load that starved paint takes at a fifth of the GPU's time
/// (270 ms on, 1 s off; `docs/measured/quiet-burst-2026-10-29.md`), so the window holds one.
pub const GPU_WINDOW: Duration = Duration::from_millis(1200);

/// Between two of [`Live`]'s GPU reads across [`GPU_WINDOW`].
const GPU_EVERY: Duration = Duration::from_millis(100);

/// The busiest of `read`'s readings across [`GPU_WINDOW`], one every [`GPU_EVERY`] (`nap`
/// between two): `None` where the first reads nothing, and a later one that reads nothing keeps
/// what was read. It ends early at a reading above [`crate::QUIET_GPU_BUSY`], which decides it,
/// or once `stopped` says so, with the most it read by then ([`Machine::gpu_busy`]).
pub(crate) fn busiest(
    read: &mut dyn FnMut() -> Option<u8>,
    nap: &mut dyn FnMut(Duration),
    stopped: &dyn Fn() -> bool,
) -> Option<u8> {
    let mut most = read()?;
    let reads = GPU_WINDOW.as_millis() / GPU_EVERY.as_millis();
    for _ in 0..reads {
        if most > crate::QUIET_GPU_BUSY || stopped() {
            break;
        }
        nap(GPU_EVERY);
        most = read().map_or(most, |b| most.max(b));
    }
    Some(most)
}

/// The most parents [`Live`]'s lineage walks up: deeper than any shell, agent and build nest.
#[cfg(target_os = "macos")]
const LINEAGE_MAX: usize = 64;

/// The volume holding `path`, read now through the mount table `table` ([`aterm_sysprobe::volumes`]), as the window's disk watch reads its volume: a local volume is placed by the path's nearest existing ancestor, with the platform's error boxes off.
///
/// # Errors
/// [`Unread::NoMount`] when no mount holds `path`, or the volume's own read error.
pub fn volume_of(path: &Path, table: &Table) -> Result<volumes::Reading, Unread> {
    let path = table.long_spelling(path);
    let spelled = volumes::mount_of(&path, table).ok_or(Unread::NoMount)?;
    if spelled.automounted || !spelled.local {
        return volumes::read(spelled);
    }
    let _quiet = aterm_sysprobe::quiet_errors();
    #[cfg(test)]
    VOLUME_ERRORS_QUIET.set(Some(aterm_sysprobe::errors_quiet()));
    let placed = path
        .ancestors()
        .find_map(|a| std::fs::canonicalize(a).ok())
        .ok_or(Unread::NoMount)?;
    volumes::read_path(&placed, table)
}

/// The volume holding `path`, with the same mount/privacy policy and kept
/// floor and room as [`Live::read`]. A nonexistent path is placed by its nearest
/// existing ancestor. `None` means the volume could not be read.
///
/// This entry reads no CPU, GPU, memory, load or process observations.
#[must_use]
pub fn read_volume(path: &Path) -> Option<Volume> {
    let v = volume_of(path, &volumes::table()).ok()?;
    let now_s = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    let watch = crate::floor::Watch::kept();
    let key = v.key.spelled();
    let room = watch.room(&key, v.total, now_s);
    Some(Volume {
        floor: crate::floor::admission_floor(
            watch.knobs,
            &crate::floor::lines(watch.knobs, Some(v.total), Some(room)),
        ),
        room,
        mount: v.mount,
        total: v.total,
        free: v.free,
    })
}

#[cfg(test)]
thread_local! {
    /// Whether the platform's error boxes were off while [`volume_of`] placed a local volume.
    pub(crate) static VOLUME_ERRORS_QUIET: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// The live machine, through aterm-sysprobe.
#[derive(Clone, Copy, Debug, Default)]
pub struct Live;

impl Machine for Live {
    fn release_run(&self) -> bool {
        std::env::var_os(crate::RELEASE_RUN_ENV).is_some_and(|v| !v.is_empty())
    }
    fn cpu_point(&self) -> Option<crate::CpuPoint> {
        let ticks = aterm_sysprobe::Probe::new().core_ticks()?;
        Some(crate::CpuPoint {
            at: std::time::Instant::now(),
            unix_ms: aterm_time::unix_millis()?,
            ticks,
        })
    }
    fn read(&self, path: Option<&Path>) -> Reading {
        let cores = cores();
        let load = aterm_sysprobe::load_average();
        let detail = aterm_sysprobe::memory_detail();
        let physical = aterm_sysprobe::physical_memory_mib()
            .map(|m| m << 20)
            .or_else(|| detail.map(|d| d.total).filter(|t| *t > 0));
        let available = detail.map(|d| d.available);
        let volume = path.and_then(read_volume);
        Reading {
            cores,
            load1: load.map(|l| l[0]),
            load5: load.map(|l| l[1]),
            wired: available.and(detail.map(|d| d.wired)),
            available,
            physical,
            swap_used: detail.map(|d| d.swap_used),
            swap_total: detail.map(|d| d.swap_total),
            volume,
            pressure: detail.and_then(|d| d.pressure),
            gpu_busy: aterm_sysprobe::gpu().and_then(|g| g.device_pct),
        }
    }

    fn gpu_busy(&self, stopped: &dyn Fn() -> bool) -> Option<u8> {
        busiest(
            &mut || aterm_sysprobe::gpu().and_then(|g| g.device_pct),
            &mut std::thread::sleep,
            stopped,
        )
    }

    fn procs(&self) -> Option<Procs> {
        let sweep = aterm_sysprobe::processes()?;
        Some(Procs::of(
            sweep.procs.iter().map(Proc::of_reading).collect(),
        ))
    }

    fn waits(&self) -> Vec<aterm_toolchain::waits::Waiting> {
        aterm_toolchain::waits::user_waiting()
    }

    /// On macOS a walk up `pid`'s parents, one kernel record each (`kproc::record`), not a whole
    /// sweep: a build's admission reads it on every call.
    #[cfg(target_os = "macos")]
    fn lineage(&self, pid: u32) -> Option<Procs> {
        let mut line = Vec::with_capacity(16);
        let mut at = pid;
        while line.len() < LINEAGE_MAX {
            let Some(r) = aterm_sysprobe::kproc::record(at) else {
                break;
            };
            line.push(Proc {
                pid: r.pid,
                ppid: r.ppid,
                ..Proc::default()
            });
            if r.ppid <= 1 || r.ppid == at {
                break;
            }
            at = r.ppid;
        }
        (!line.is_empty()).then(|| Procs::of(line))
    }
}

impl Proc {
    /// Process `p` as the ledger reads it: the memory it holds is its footprint, or its resident
    /// size where that is less (macOS `ri_resident_size`, Linux `RssAnon`:
    /// `ProcessReading::resident`). A footprint counts the pages the kernel compressed or swapped
    /// out, which a claim's room never held, and a macOS resident size counts the file-backed
    /// pages a process maps, which a footprint does not; so the lesser leaves the compressed and
    /// swapped pages out where they outnumber the file-backed resident ones (an aterm-gui compile
    /// on this Mac, 2026-10-29: 14 GB of footprint, 8.7 GiB resident): a tree whose 10 GiB of
    /// footprint holds 2 GiB of resident pages, credited 10 GiB, has its 12 GiB claim reserve 2
    /// GiB of the room it would take back as it touches them; credited 2 GiB, it reserves 10.
    /// Where the file-backed resident pages outnumber the compressed and swapped ones, the
    /// footprint is credited whole, compressed pages included. The footprint itself is kept beside
    /// it ([`Proc::footprint`]), so a line can say what a tree holds past its claim in pages the
    /// credit leaves out.
    #[must_use]
    pub fn of_reading(p: &aterm_sysprobe::ProcessReading) -> Proc {
        let footprint = p.footprint.unwrap_or(0);
        let reaped = p.child_cpu_ns.unwrap_or(0);
        Proc {
            pid: p.pid,
            ppid: p.ppid,
            charged: p.resident.map_or(footprint, |r| r.min(footprint)),
            footprint,
            written: p.disk_written.unwrap_or(0),
            cpu_ns: p.cpu_ns.unwrap_or(0).saturating_add(reaped),
            runnable_ns: p.runnable_ns.map_or(0, |r| r.saturating_add(reaped)),
        }
    }
}

/// A machine a test sets: the reading and the processes it answers until set again. Its volume is the one the reading holds, whatever path is asked about.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default)]
pub struct Fixed {
    reading: Mutex<Reading>,
    procs: Mutex<Option<Procs>>,
    waits: Mutex<Vec<aterm_toolchain::waits::Waiting>>,
}

#[cfg(any(test, debug_assertions))]
impl Fixed {
    /// A machine that answers `reading`, and no processes.
    #[must_use]
    pub fn new(reading: Reading) -> Fixed {
        Fixed {
            reading: Mutex::new(reading),
            procs: Mutex::new(None),
            waits: Mutex::new(Vec::new()),
        }
    }

    /// Answer `reading` from now on.
    pub fn set(&self, reading: Reading) {
        *self.reading.lock().unwrap_or_else(PoisonError::into_inner) = reading;
    }

    /// Answer `procs` from now on.
    pub fn set_procs(&self, procs: Option<Procs>) {
        *self.procs.lock().unwrap_or_else(PoisonError::into_inner) = procs;
    }

    /// Answer `waits` as the user's marked waits from now on.
    pub fn set_waits(&self, waits: Vec<aterm_toolchain::waits::Waiting>) {
        *self.waits.lock().unwrap_or_else(PoisonError::into_inner) = waits;
    }
}

#[cfg(any(test, debug_assertions))]
impl Machine for Fixed {
    fn read(&self, _path: Option<&Path>) -> Reading {
        self.reading
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn procs(&self) -> Option<Procs> {
        self.procs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn waits(&self) -> Vec<aterm_toolchain::waits::Waiting> {
        self.waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}
