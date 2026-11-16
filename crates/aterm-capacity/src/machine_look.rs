// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KEPT LOOK ([`MachineLook`]): what this machine says of its own resources when another
//! asks, in fixed integer units with capacity beside use, and whether each of its four
//! resources is at its limit ([`Limits`]).
//!
//! A look is taken only inside an answer ([`latest`]): nothing here arms a timer, starts a
//! thread or wakes the process. One look is kept [`LOOK_KEEP`], so many askers in a few seconds
//! share one reading of the machine.
//!
//! The levels (cores, load, memory, swap, the home volume, the GPU, the link's rate) are one
//! in-process [`Look`]. CPU busy and the network rates are deltas of cumulative counters, so the
//! [`Keeper`] holds a ring of the counters each look read, [`RATE_BACK_MAX`] deep, and takes a
//! rate against the look nearest [`RATE_BACK`] back: one window whoever asks and however often.
//! A rate against "the previous look" would be a ten-second mean under many askers and a
//! one-minute mean under one, and the same burst would then be at the limit for one fleet and
//! not for another. A machine nobody asked for [`RATE_BACK_MAX`] has no rate at its next look.
//!
//! Every figure is `None` where it was not read, and no figure is ever a zero that stands for
//! "unread". The machine decides its own verdicts, because the rules' constants live here and
//! in aterm-vitals and memory's rule differs by OS; it reports the inputs beside them. It scores
//! its own system load the same way ([`LoadScore`]): by the resource deck's own dial, so the
//! square another Mac draws for it is what its own dial reads.

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_sysprobe::linux::Psi;
use aterm_sysprobe::{CoreTicks, LinkReading, MemoryLevel};
use aterm_vitals::Pressure;
use aterm_vitals::net_counter::{ByteCounter, ByteSample, byte_delta};
use aterm_vitals::strain::{self, Now};

use crate::floor::Lines;
use crate::host_vitals::{Look, hundredths};
use crate::{CpuPoint, CpuWindow};

/// How long one look is kept: a look asked for within this of the last one is that one.
pub const LOOK_KEEP: Duration = Duration::from_secs(10);

/// How far back the look a rate is taken against should be.
pub const RATE_BACK: Duration = Duration::from_secs(60);

/// The nearest a look may be for a rate to be taken against it.
pub const RATE_BACK_MIN: Duration = Duration::from_secs(45);

/// The farthest a look may be for a rate to be taken against it, and how deep the ring is.
pub const RATE_BACK_MAX: Duration = Duration::from_secs(90);

/// The GPU is at its limit with its device at least this busy, percent. No rule for a GPU past
/// its capacity exists (it is a share of time, which cannot pass 100%); this is the share the
/// CPU's own rule asks of the cores (`aterm_vitals::model::CPU_OVER_BUSY`).
pub const GPU_LIMIT_PCT: u8 = 95;

const MIB: u64 = 1 << 20;

/// Whether each of the machine's four resources is at its limit: `Some(true)` at it,
/// `Some(false)` within it, `None` where an input of its rule was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The 1-minute load is over the cores and at least 95% of them were busy over the rate
    /// window: the deck's one over rule ([`aterm_vitals::strain::cpu_over_depth`]), so a load
    /// made of tasks blocked on I/O is not CPU at its limit.
    pub cpu: Option<bool>,
    /// macOS and Windows: the pressure word is `warn` or `critical`. Linux: some task stalled
    /// on memory for a tenth of the last minute and under a tenth of memory is available
    /// ([`aterm_sysprobe::linux::memory_at_limit`]). Swap in use alone is never a limit.
    pub mem: Option<bool>,
    /// The home volume's free space is below the line the disk watch starts reclaiming at
    /// ([`disk_limit_line`]).
    pub disk: Option<bool>,
    /// The GPU's device is at least [`GPU_LIMIT_PCT`] busy.
    pub gpu: Option<bool>,
}

impl Limits {
    /// No verdict read.
    pub const UNREAD: Limits = Limits {
        cpu: None,
        mem: None,
        disk: None,
        gpu: None,
    };

    /// The four verdicts of `look`'s own figures (its [`MachineLook::limits`] is not read), with
    /// the lines the disk watch holds the home volume to (`None`: unread) and, on Linux, the
    /// memory PSI read with the look (`None` on every other OS, and on a Linux that keeps no
    /// PSI, where the pressure word is unread too). Each verdict is `None` where an input of
    /// its rule is `None`.
    #[must_use]
    pub fn of(look: &MachineLook, disk: Option<&Lines>, psi: Option<&Psi>) -> Limits {
        Limits {
            cpu: cpu_at_limit(look),
            mem: mem_at_limit(look, psi),
            disk: disk_at_limit(look, disk),
            gpu: look.gpu_busy_pct.map(|pct| pct >= GPU_LIMIT_PCT),
        }
    }
}

fn cpu_at_limit(look: &MachineLook) -> Option<bool> {
    let cores = f64::from(look.cores.filter(|n| *n > 0)?);
    let load1 = f64::from(look.load_x100?[0]) / 100.0;
    let busy = look.cpu_busy_x100? as f64 / 100.0 / cores;
    Some(aterm_vitals::strain::cpu_over_depth(Some(busy), Some(cores), Some(load1)).is_some())
}

fn mem_at_limit(look: &MachineLook, psi: Option<&Psi>) -> Option<bool> {
    match psi {
        Some(psi) => {
            let room = (look.mem_total_mb?, look.mem_avail_mb?);
            Some(aterm_sysprobe::linux::memory_at_limit(psi, Some(room)))
        }
        None => look.mem_pressure.map(MemoryLevel::is_elevated),
    }
}

fn disk_at_limit(look: &MachineLook, lines: Option<&Lines>) -> Option<bool> {
    let free_mb = look.disk_free_mb?;
    Some(free_mb < disk_limit_line(lines?).div_ceil(MIB))
}

/// The machine's system-load score: the resource deck's own dial ([`aterm_vitals::strain`]), its
/// level and its depth past capacity, each in hundredths, so a machine's square on another Mac's
/// band reads what the dial on its own deck reads. One rule: [`Self::of`] calls the dial's
/// functions over the look's figures and copies none of them.
///
/// The dial's third reading, the soak (how much of the last hour was spent past capacity), is
/// not here: it reads an hour of 10 s buckets, and the [`Keeper`] holds at most
/// [`RATE_BACK_MAX`] of looks, taken only when another machine asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadScore {
    /// [`strain::level`] times 100, `0..=100`: the busier of the CPU and the GPU, lifted by
    /// memory pressure `warn` or `critical`.
    pub level_x100: u8,
    /// [`strain::over`] times 100: 0 within capacity, else
    /// [`strain::OVER_FLOOR`]` × 100..=100`; `None` where the dial could not judge it: the run
    /// queue (its CPU term) or the swap traffic (its swap term) unread, or, with neither over,
    /// the memory pressure unread (its `critical` term). The dial reads an unread input as no
    /// strain, and a 0 sent for it would say within capacity of a machine nobody measured.
    pub over_x100: Option<u8>,
}

impl LoadScore {
    /// The dial over `look`'s own figures (its [`MachineLook::load`] is not read) and the swap
    /// traffic `swap_bps` (bytes a second in plus out over the look's rate window; `None`
    /// unread): the CPU's busy share of its cores, the GPU's busy share, the cores, the
    /// 1-minute load as the run queue (the dial's own reading where it keeps no 10 s queue,
    /// [`aterm_vitals::runq`]), the swap traffic and the memory pressure, as
    /// [`aterm_vitals::strain::Now::of`] reads the deck's snapshot.
    ///
    /// `None` where the CPU's busy share is unread (no look [`RATE_BACK_MIN`] to
    /// [`RATE_BACK_MAX`] back, or no cores): the dial's level is the busier engine's share, and
    /// a level read from the GPU alone would draw a machine whose CPU nobody measured as calm.
    #[must_use]
    pub fn of(look: &MachineLook, swap_bps: Option<f64>) -> Option<LoadScore> {
        let cores = look.cores.filter(|n| *n > 0).map(f64::from);
        let cpu = cores
            .zip(look.cpu_busy_x100)
            .map(|(cores, busy)| busy as f64 / 100.0 / cores);
        cpu?;
        let now = Now {
            cpu,
            gpu: look.gpu_busy_pct.map(|pct| f64::from(pct) / 100.0),
            cores,
            queue: look.load_x100.map(|load| f64::from(load[0]) / 100.0),
            swap_bps,
            pressure: match look.mem_pressure {
                Some(MemoryLevel::Normal) => Pressure::Normal,
                Some(MemoryLevel::Warn) => Pressure::Warn,
                Some(MemoryLevel::Critical) => Pressure::Critical,
                None => Pressure::Unknown,
            },
        };
        let over = strain::over(&now);
        let judged = now.queue.is_some()
            && now.swap_bps.is_some()
            && (over > 0.0 || now.pressure != Pressure::Unknown);
        Some(LoadScore {
            level_x100: x100(strain::level(&now)?),
            over_x100: judged.then(|| x100(over)),
        })
    }
}

/// A dial reading, `0..=1`, in whole hundredths.
fn x100(share: f32) -> u8 {
    (f64::from(share).clamp(0.0, 1.0) * 100.0).round() as u8
}

/// The free space, bytes, below which a volume held to `lines` is at its limit: the floor, the
/// line at which aterm itself starts reclaiming (it honours the owner's `[disk]` knobs and the
/// volume's measured room); with the floor turned off (0), the emergency line, below which
/// writes are about to fail. Never the warning line, which a volume crosses with room left.
#[must_use]
pub const fn disk_limit_line(lines: &Lines) -> u64 {
    if lines.floor > 0 {
        lines.floor
    } else {
        lines.emergency
    }
}

/// One look at this machine, as it says it to another: integers in fixed units, capacity
/// beside use. Every figure is `None` where it was not read; there is no default look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineLook {
    /// When the look was taken, Unix ms on this machine's clock; `None` where the clock reads
    /// before the epoch.
    pub at_unix_ms: Option<u64>,
    /// Logical cores.
    pub cores: Option<u32>,
    /// The 1-, 5- and 15-minute load averages, times 100.
    pub load_x100: Option<[u32; 3]>,
    /// Cores busy, times 100, the mean over [`Self::rate_win_ms`] (`350`: three and a half
    /// cores' worth).
    pub cpu_busy_x100: Option<u64>,
    /// The interval the CPU and network rates cover, ms: [`RATE_BACK_MIN`] to
    /// [`RATE_BACK_MAX`]; `None` where no rate was taken.
    pub rate_win_ms: Option<u64>,
    /// Physical memory, MiB.
    pub mem_total_mb: Option<u64>,
    /// Memory in use, MiB ([`aterm_sysprobe::MemoryDetail::used`]).
    pub mem_used_mb: Option<u64>,
    /// Memory the kernel can hand out now without paging, MiB
    /// ([`aterm_sysprobe::MemoryDetail::available`]).
    pub mem_avail_mb: Option<u64>,
    /// The kernel's memory pressure level.
    pub mem_pressure: Option<MemoryLevel>,
    /// Linux: PSI `some avg60`, times 100; `None` on every other OS.
    pub mem_psi60_x100: Option<u32>,
    /// Swap in use, MiB.
    pub swap_used_mb: Option<u64>,
    /// Swap allocated, MiB.
    pub swap_total_mb: Option<u64>,
    /// The home volume's size, MiB.
    pub disk_total_mb: Option<u64>,
    /// What this account may still write on the home volume, MiB.
    pub disk_free_mb: Option<u64>,
    /// GPU cores, where the host reports a count.
    pub gpu_cores: Option<u32>,
    /// How busy the GPU's device is, `0..=100`.
    pub gpu_busy_pct: Option<u8>,
    /// The driver's rate of the fastest link that counts toward the machine
    /// ([`aterm_sysprobe::counts_toward_machine`]), kbit/s; `None` where no counted link reports
    /// one.
    pub net_link_kbps: Option<u64>,
    /// Received on that one link, kbit/s, the mean over [`Self::rate_win_ms`]; where no counted
    /// link reports a rate, summed over the counted links.
    pub net_rx_kbps: Option<u64>,
    /// Sent, as [`Self::net_rx_kbps`].
    pub net_tx_kbps: Option<u64>,
    /// Received plus sent on the counted links that [`Self::net_rx_kbps`] and
    /// [`Self::net_tx_kbps`] leave out, kbit/s: traffic that is never added to one link's use
    /// against that link's rate. A read 0 where they leave none out.
    pub net_other_kbps: Option<u64>,
    /// Whether each resource is at its limit.
    pub limits: Limits,
    /// The system-load score its own deck's dial reads ([`LoadScore`]); `None` where the CPU's
    /// busy share is unread.
    pub load: Option<LoadScore>,
}

impl MachineLook {
    /// A look that read nothing.
    pub const UNREAD: MachineLook = MachineLook {
        at_unix_ms: None,
        cores: None,
        load_x100: None,
        cpu_busy_x100: None,
        rate_win_ms: None,
        mem_total_mb: None,
        mem_used_mb: None,
        mem_avail_mb: None,
        mem_pressure: None,
        mem_psi60_x100: None,
        swap_used_mb: None,
        swap_total_mb: None,
        disk_total_mb: None,
        disk_free_mb: None,
        gpu_cores: None,
        gpu_busy_pct: None,
        net_link_kbps: None,
        net_rx_kbps: None,
        net_tx_kbps: None,
        net_other_kbps: None,
        limits: Limits::UNREAD,
        load: None,
    };

    /// The levels one in-process look `read` gives, taken at `at_unix_ms`: no rate, no PSI, no
    /// verdict and no load score, which the [`Keeper`] adds. Each figure comes from the reading
    /// that owns it and is `None` where that reading is absent, as
    /// [`crate::host_vitals::HostVitals::of`] maps the same look.
    fn levels(read: &Look, at_unix_ms: Option<u64>) -> MachineLook {
        let memory = read.memory.as_ref();
        let gpu = read.gpu.as_ref();
        let volume = read.volume.as_ref();
        MachineLook {
            at_unix_ms,
            cores: read.cores.filter(|n| *n > 0),
            load_x100: read
                .load
                .filter(|load| load.iter().all(|l| l.is_finite() && *l >= 0.0))
                .map(|load| load.map(hundredths)),
            mem_total_mb: memory.map(|m| m.total).filter(|t| *t > 0).map(mib),
            mem_used_mb: memory.map(|m| mib(m.used())),
            mem_avail_mb: memory.map(|m| mib(m.available)),
            mem_pressure: memory.and_then(|m| m.pressure),
            swap_used_mb: memory.and_then(|m| m.swap_used).map(mib),
            swap_total_mb: memory.and_then(|m| m.swap_total).map(mib),
            disk_total_mb: volume.map(|v| mib(v.total)),
            disk_free_mb: volume.map(|v| mib(v.free)),
            gpu_cores: gpu.and_then(|g| g.cores).filter(|n| *n > 0).map(u32::from),
            gpu_busy_pct: gpu.and_then(|g| g.device_pct).map(|p| p.min(100)),
            ..MachineLook::UNREAD
        }
    }
}

/// Whole MiB in `bytes`.
const fn mib(bytes: u64) -> u64 {
    bytes / MIB
}

/// The two clocks of one look.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    /// The monotonic instant: what a look's age and a rate's interval are measured on.
    pub at: Instant,
    /// The wall clock, Unix ms; `None` where it reads before the epoch.
    pub unix_ms: Option<u64>,
}

/// What the [`Keeper`] reads the machine through: the live machine ([`Live`]), or a test's.
pub trait Probe {
    /// The clocks now.
    fn now(&mut self) -> Clock;
    /// The levels: cores, load, memory, the GPU and the volume holding `home`.
    fn look(&mut self, home: Option<&Path>) -> Look;
    /// Each logical core's cumulative ticks; `None` where they cannot be read.
    fn core_ticks(&mut self) -> Option<Vec<CoreTicks>>;
    /// Every network interface with its cumulative byte counters; `None` where unread.
    fn net_links(&mut self) -> Option<Vec<LinkReading>>;
    /// Linux: the memory PSI file. `None` on every other OS and where the kernel keeps none.
    fn psi(&mut self) -> Option<Psi>;
    /// KiB swapped in plus out since boot, the counter the deck's swap rate reads
    /// ([`aterm_sysprobe::swapped_kib`]); `None` where unread.
    fn swapped_kib(&mut self) -> Option<u64>;
    /// The lines the disk watch holds the volume keyed `key`
    /// (`aterm_sysprobe::volumes::VolumeKey::spelled`) of `total` bytes to; `None` where they
    /// cannot be read.
    fn disk_lines(&mut self, key: &str, total: u64) -> Option<Lines>;
}

/// The live machine, read in this process through aterm-sysprobe with no child process.
#[derive(Debug, Default)]
pub struct Live {
    cpu: aterm_sysprobe::Probe,
    links_scratch: Vec<u8>,
}

impl Probe for Live {
    fn now(&mut self) -> Clock {
        Clock {
            at: Instant::now(),
            unix_ms: aterm_time::unix_millis(),
        }
    }

    /// No GPU on Windows: a process's first GPU read there opens a counter query for about a
    /// quarter of a second ([`aterm_sysprobe::gpu`]), inside an answer another machine waits
    /// for.
    fn look(&mut self, home: Option<&Path>) -> Look {
        if cfg!(windows) {
            Look::without_gpu(home)
        } else {
            Look::now(home)
        }
    }

    fn core_ticks(&mut self) -> Option<Vec<CoreTicks>> {
        self.cpu.core_ticks()
    }

    fn net_links(&mut self) -> Option<Vec<LinkReading>> {
        aterm_sysprobe::net_links_into(&mut self.links_scratch)
    }

    fn psi(&mut self) -> Option<Psi> {
        if cfg!(target_os = "linux") {
            aterm_sysprobe::linux::Root::system().psi().ok()
        } else {
            None
        }
    }

    fn swapped_kib(&mut self) -> Option<u64> {
        aterm_sysprobe::swapped_kib()
    }

    fn disk_lines(&mut self, key: &str, total: u64) -> Option<Lines> {
        let now_s = i64::try_from(aterm_time::unix_secs()?).ok()?;
        Some(crate::floor::Watch::kept().lines(key, total, now_s))
    }
}

/// One counted link's cumulative bytes at one look.
#[derive(Clone, Debug)]
struct LinkBytes {
    name: String,
    sample: ByteSample,
}

fn link_byte_sample(link: &LinkReading) -> ByteSample {
    ByteSample {
        index: link.index,
        counter: match link.byte_counter {
            aterm_sysprobe::LinkByteCounter::Unknown => ByteCounter::Unknown,
            aterm_sysprobe::LinkByteCounter::Cumulative64 => ByteCounter::Cumulative64,
            aterm_sysprobe::LinkByteCounter::DarwinModulo32 => ByteCounter::DarwinModulo32,
        },
        rx: link.rx.bytes,
        tx: link.tx.bytes,
        rx_packets: link.rx.packets,
        tx_packets: link.tx.packets,
        rate_bps: link.rate_bps,
    }
}

/// The counters one look read, kept for a later look's rates.
#[derive(Debug)]
struct Past {
    at: Instant,
    /// The wall clock then, Unix ms; `None` where it read before the epoch.
    unix_ms: Option<u64>,
    cpu: Option<CpuPoint>,
    /// The links that counted toward the machine then; `None` where the links were unread.
    links: Option<Vec<LinkBytes>>,
    /// KiB swapped in plus out since boot then; `None` where unread.
    swapped_kib: Option<u64>,
}

/// The look last taken, and the home it was taken for.
#[derive(Debug)]
struct Kept {
    at: Instant,
    home: Option<PathBuf>,
    look: MachineLook,
}

/// Keeps one look [`LOOK_KEEP`] and the counters of the looks of the last [`RATE_BACK_MAX`], at
/// most one a [`LOOK_KEEP`], for the rates.
#[derive(Debug, Default)]
pub struct Keeper {
    ring: VecDeque<Past>,
    kept: Option<Kept>,
}

impl Keeper {
    /// A keeper that has looked at nothing.
    #[must_use]
    pub const fn new() -> Keeper {
        Keeper {
            ring: VecDeque::new(),
            kept: None,
        }
    }

    /// The machine as `probe` reads it, its volume the one holding `home`: the kept look while
    /// it is younger than [`LOOK_KEEP`] on both clocks and was taken for the same home, else a
    /// new one. The monotonic clock stops while the machine sleeps and the wall clock does not,
    /// so a look from before a sleep is young on the one and old on the other: it is read
    /// again. Where either wall time is unread, the monotonic clock alone decides.
    ///
    /// A new look's CPU busy and network rates are taken against the kept counters nearest
    /// [`RATE_BACK`] back among those [`RATE_BACK_MIN`] to [`RATE_BACK_MAX`] old (of two equally
    /// near, the older). Counters whose wall age passes their monotonic age by more than
    /// [`LOOK_KEEP`] were read before a sleep and are dropped, so no rate spans one. Each rate is
    /// `None` where a counter it needs was unread at either end (the core ticks count as unread
    /// under a clock before the epoch), went back, or the cores changed; with no such counters
    /// every rate and [`MachineLook::rate_win_ms`] are `None`. The swap traffic over the same
    /// window feeds the load score ([`LoadScore::of`]) and is not a figure of the look.
    pub fn look(&mut self, probe: &mut dyn Probe, home: Option<&Path>) -> MachineLook {
        let now = probe.now();
        let age = |then: Instant| now.at.checked_duration_since(then);
        // How long ago `then` was on the wall clock: `None` where either reading is unread or
        // the clock went back past it.
        let wall_age =
            |then: Option<u64>| Some(Duration::from_millis(now.unix_ms?.checked_sub(then?)?));
        if let Some(kept) = &self.kept
            && kept.home.as_deref() == home
            && age(kept.at).is_some_and(|age| age < LOOK_KEEP)
            && (now.unix_ms.is_none()
                || kept.look.at_unix_ms.is_none()
                || wall_age(kept.look.at_unix_ms).is_some_and(|age| age < LOOK_KEEP))
        {
            return kept.look.clone();
        }
        let read = probe.look(home);
        let cpu = probe
            .core_ticks()
            .zip(now.unix_ms)
            .map(|(ticks, unix_ms)| CpuPoint {
                at: now.at,
                unix_ms,
                ticks,
            });
        let links = probe.net_links();
        let swapped_kib = probe.swapped_kib();
        let counted: Option<Vec<&LinkReading>> = links.as_ref().map(|links| {
            links
                .iter()
                .filter(|link| aterm_sysprobe::counts_toward_machine(link))
                .collect()
        });

        self.ring.retain(|past| {
            age(past.at).is_some_and(|age| {
                age <= RATE_BACK_MAX
                    && wall_age(past.unix_ms)
                        .is_none_or(|wall| wall.saturating_sub(age) <= LOOK_KEEP)
            })
        });
        let back = self
            .ring
            .iter()
            .filter_map(|past| Some((age(past.at)?, past)))
            .filter(|(age, _)| *age >= RATE_BACK_MIN)
            .min_by_key(|(age, _)| (age.abs_diff(RATE_BACK), Reverse(*age)));

        let mut look = MachineLook::levels(&read, now.unix_ms);
        look.cpu_busy_x100 = back
            .and_then(|(_, past)| CpuWindow::between(past.cpu.as_ref()?, cpu.as_ref()?))
            .and_then(cores_busy_x100);
        let net = Net::of(
            counted.as_deref(),
            back.and_then(|(age, past)| Some((past.links.as_deref()?, age))),
        );
        look.net_link_kbps = net.link_kbps;
        look.net_rx_kbps = net.rx_kbps;
        look.net_tx_kbps = net.tx_kbps;
        look.net_other_kbps = net.other_kbps;
        let rated = look.cpu_busy_x100.is_some()
            || net.rx_kbps.is_some()
            || net.tx_kbps.is_some()
            || net.other_kbps.is_some();
        look.rate_win_ms = back
            .filter(|_| rated)
            .and_then(|(age, _)| u64::try_from(age.as_millis()).ok());

        let psi = probe.psi();
        look.mem_psi60_x100 = psi.map(|psi| hundredths(f64::from(psi.some_avg60)));
        let lines = read
            .volume
            .as_ref()
            .and_then(|v| probe.disk_lines(&v.key.spelled(), v.total));
        look.limits = Limits::of(&look, lines.as_ref(), psi.as_ref());
        // Bytes a second, as the deck's swap rate: KiB moved over the window times 1024.
        let swap_bps = back.and_then(|(age, past)| {
            let moved = swapped_kib?.checked_sub(past.swapped_kib?)?;
            let secs = age.as_secs_f64();
            (secs > 0.0).then_some(moved as f64 * 1024.0 / secs)
        });
        look.load = LoadScore::of(&look, swap_bps);

        if self
            .ring
            .back()
            .is_none_or(|last| age(last.at).is_some_and(|age| age >= LOOK_KEEP))
        {
            self.ring.push_back(Past {
                at: now.at,
                unix_ms: now.unix_ms,
                cpu,
                links: counted.map(|counted| {
                    counted
                        .iter()
                        .map(|link| LinkBytes {
                            name: link.name.clone(),
                            sample: link_byte_sample(link),
                        })
                        .collect()
                }),
                swapped_kib,
            });
        }
        self.kept = Some(Kept {
            at: now.at,
            home: home.map(Path::to_path_buf),
            look: look.clone(),
        });
        look
    }
}

/// Cores busy, times 100, over a checked counter window: its busy share of its cores.
fn cores_busy_x100(window: CpuWindow) -> Option<u64> {
    let busy = u128::from(window.busy_ticks) * u128::from(window.cores) * 100;
    u64::try_from(busy.checked_div(u128::from(window.total_ticks))?).ok()
}

/// The network figures of one look.
struct Net {
    link_kbps: Option<u64>,
    rx_kbps: Option<u64>,
    tx_kbps: Option<u64>,
    other_kbps: Option<u64>,
}

impl Net {
    /// The figures of the links that count now (`None`: unread), their rates against what the
    /// links counted `back.1` ago read (`None`: no look to take a rate against).
    ///
    /// Capacity is the fastest counted link's driver rate (of two equally fast, the lower
    /// interface index), and received and sent are that one link's, so use never passes
    /// capacity by adding another link's traffic; the other counted links' bytes are the
    /// "other" figure. Where no counted link reports a rate there is no capacity, received and
    /// sent are sums over the counted links and nothing is left for "other". A link has a rate
    /// only where it counted at both looks and the counter source establishes an unambiguous
    /// delta. Darwin modulo bytes can be ambiguous over a long look even when they increase;
    /// no lifetime total is reconstructed. A sum is
    /// `None` where any link in it has none, and every rate is `None` with no counted link.
    fn of(counted: Option<&[&LinkReading]>, back: Option<(&[LinkBytes], Duration)>) -> Net {
        let mut net = Net {
            link_kbps: None,
            rx_kbps: None,
            tx_kbps: None,
            other_kbps: None,
        };
        let Some(counted) = counted else {
            return net;
        };
        let main = counted
            .iter()
            .filter_map(|link| Some((link.rate_bps.filter(|bps| *bps > 0)?, *link)))
            .max_by_key(|(bps, link)| (*bps, Reverse(link.index)));
        net.link_kbps = main.map(|(bps, _)| bps / 1000);
        let Some((then, interval)) = back else {
            return net;
        };
        let interval_ns = interval.as_nanos();
        if interval_ns == 0 || counted.is_empty() {
            return net;
        }
        // Bytes over the interval as kbit/s: bytes × 8 bits ÷ 1000 ÷ seconds.
        let kbps = |bytes: u64| u64::try_from(u128::from(bytes) * 8_000_000 / interval_ns).ok();
        let moved = |link: &LinkReading| {
            let was = then.iter().find(|was| was.name == link.name)?;
            let window_ns = u64::try_from(interval_ns).ok()?;
            let bytes = byte_delta(was.sample, link_byte_sample(link), window_ns).ok()?;
            Some((bytes.rx, bytes.tx))
        };
        let sum = |main: Option<&LinkReading>| {
            counted
                .iter()
                .filter(|link| main.is_none_or(|main| !std::ptr::eq(**link, main)))
                .try_fold((0u64, 0u64), |(rx, tx), link| {
                    let (link_rx, link_tx) = moved(link)?;
                    Some((rx.checked_add(link_rx)?, tx.checked_add(link_tx)?))
                })
        };
        match main {
            Some((_, main)) => {
                let own = moved(main);
                net.rx_kbps = own.and_then(|(rx, _)| kbps(rx));
                net.tx_kbps = own.and_then(|(_, tx)| kbps(tx));
                net.other_kbps = sum(Some(main)).and_then(|(rx, tx)| kbps(rx.checked_add(tx)?));
            }
            None => {
                let all = sum(None);
                net.rx_kbps = all.and_then(|(rx, _)| kbps(rx));
                net.tx_kbps = all.and_then(|(_, tx)| kbps(tx));
                net.other_kbps = all.map(|_| 0);
            }
        }
        net
    }
}

/// This machine's look now, its volume the one holding `home` (`None`: no volume is read): the
/// process's one [`Keeper`] over the live machine ([`Live`]), so every answer this process
/// gives within [`LOOK_KEEP`] carries the same look and its rates come from one ring. It reads
/// the machine only when called: nothing looks between answers.
#[must_use]
pub fn latest(home: Option<&Path>) -> MachineLook {
    static KEPT: Mutex<Option<(Keeper, Live)>> = Mutex::new(None);
    let mut kept = KEPT.lock().unwrap_or_else(PoisonError::into_inner);
    let (keeper, live) = kept.get_or_insert_with(|| (Keeper::new(), Live::default()));
    keeper.look(live, home)
}

#[cfg(test)]
#[path = "machine_look_tests.rs"]
mod tests;
