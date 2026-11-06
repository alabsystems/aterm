// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The deck's units (stats deck amendment A1 §4, step R4): what each mechanism tile draws, as
//! values the paint (R6) takes whole.
//!
//! Every value is the 10 s mean and max of the 1 s readings ([`NOW_WINDOW_MS`]), the window of
//! the row's % and live box, so a tile never disagrees with its row:
//! - CPU: each logical core's busy fraction, pushed beside the CPU row's reading and only when it
//!   is, from the same per-core fractions the row sums, so the mean of the tiles is the CPU %;
//!   each cluster's mean of its cores with its busiest core's max; the queue tile's 1-minute
//!   load beyond the cores.
//! - Memory: compressor churn (bytes compressed and decompressed a second) against its own 7-day
//!   sustained-30 s peak, and swap traffic against [`SWAP_OVER_BPS`].
//! - Disk: each physical device's write and read rates against that device-direction's peak,
//!   home's device first.
//!
//! A rate's peak learns for [`IO_LEARN_S`] (3 h) before its tile draws against it; until then its
//! basis is [`CapBasis::Learning`] and the tile draws `hollow`.

use crate::model::{CPU_OVER_BUSY, CapBasis, ClusterView, SWAP_OVER_BPS, day_of, note_peak};
use crate::peaks::{DailyMax, Peaks};
use crate::ring::{BucketRing, NOW_WINDOW_MS};

/// Seconds of sustained readings before compressor churn or a disk direction has learned its
/// peak: 3 hours (A1 §4.3, §4.4).
pub const IO_LEARN_S: u32 = 10_800;

/// The most devices a reading draws (A1 §4.4): home's, and the front tab's when it is another.
pub const DISK_UNITS: usize = 2;

/// The devices whose rates and peaks the model keeps, the least recently read dropped (A1 §9).
pub const DISK_DEVICES_KEPT: usize = 4;

/// A unit ring's buckets: the open one and the three closed ones a sustained-30 s value reads
/// ([`BucketRing::sustained`]). Its readings keep the last minute whatever its buckets, and it
/// keeps no ten-minute ones: a unit draws its last 10 s, and the rows' rings carry the hours (A1
/// §9: no per-core 6 h rings).
const UNIT_RING_BUCKETS: usize = 4;

/// The 10 s mean and max of one quantity's readings.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Level {
    /// The mean of the readings.
    pub mean: f64,
    /// The largest of them: the tile's tick.
    pub max: f64,
}

impl Level {
    /// `ring`'s readings in the [`NOW_WINDOW_MS`] ending `now` (unix ms); `None` without one.
    #[must_use]
    pub fn of(ring: &BucketRing, now: i64) -> Option<Self> {
        Some(Self {
            mean: ring.now_mean(now, NOW_WINDOW_MS)?,
            max: ring.now_max(now, NOW_WINDOW_MS)?,
        })
    }

    /// Both as shares of `cap`; `None` unless `cap` is positive and finite.
    #[must_use]
    pub fn over(self, cap: f64) -> Option<Self> {
        (cap > 0.0 && cap.is_finite()).then_some(Self {
            mean: self.mean / cap,
            max: self.max / cap,
        })
    }
}

/// The CPU's queue tile (A1 §4.1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Queue {
    /// The 1-minute load beyond the cores as a share of them, `clamp((load1 − n)/n, 0, 1)`
    /// ([`queue_level`]); the tile is absent at 0.
    pub level: f64,
    /// The CPU is over ([`cpu_over`]): the tile takes `over`.
    pub over: bool,
}

/// `clamp((load1 − n)/n, 0, 1)`: the over rule's own arm (load1 > n), as a level. 0 for no cores
/// or a load that is not a number.
#[must_use]
pub fn queue_level(load1: f64, n: usize) -> f64 {
    if n == 0 || !load1.is_finite() {
        return 0.0;
    }
    let n = n as f64;
    ((load1 - n) / n).clamp(0.0, 1.0)
}

/// The CPU over rule: the 1-minute load past the cores while at least [`CPU_OVER_BUSY`] of
/// them are busy.
#[must_use]
pub fn cpu_over(busy: Option<f64>, n: Option<usize>, load1: Option<f64>) -> bool {
    match (busy, n, load1) {
        (Some(b), Some(n), Some(load)) => load > n as f64 && b >= CPU_OVER_BUSY * n as f64,
        _ => false,
    }
}

/// A rate drawn against its own peak (compressor churn, a disk direction).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rate {
    /// Bytes a second, 10 s mean and max; `None` until two readings give a rate.
    pub level: Option<Level>,
    /// The 7-day sustained-30 s peak, where any was read.
    pub peak: Option<f64>,
    /// [`CapBasis::Peak`] once [`IO_LEARN_S`] of it were read, else [`CapBasis::Learning`].
    pub basis: CapBasis,
}

impl Rate {
    fn of(ring: &BucketRing, now: i64, peak: &DailyMax) -> Self {
        let today = day_of(now);
        Self {
            level: Level::of(ring, now),
            peak: peak.peak(today),
            basis: if peak.learned(IO_LEARN_S) {
                CapBasis::Peak
            } else {
                CapBasis::Learning
            },
        }
    }

    /// The level as a share of the peak: what the tile's column and tick draw. `None` while the
    /// peak learns (the tile is `hollow`), and with no rate or no positive peak.
    #[must_use]
    pub fn share(&self) -> Option<Level> {
        if self.basis != CapBasis::Peak {
            return None;
        }
        self.level?.over(self.peak?)
    }
}

/// The swap column past the waffle's wall (A1 §4.3).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Swap {
    /// Swap in use, bytes: ⌈used / tile⌉ tiles, at most two.
    pub used: u64,
    /// Swap traffic, in plus out, bytes a second, 10 s mean and max.
    pub traffic: Option<Level>,
}

impl Swap {
    /// The traffic as a share of [`SWAP_OVER_BPS`]: each tile's heat column.
    #[must_use]
    pub fn share(&self) -> Option<Level> {
        self.traffic.and_then(|t| t.over(SWAP_OVER_BPS))
    }

    /// The column is at or past the memory over rule's rate: the tiles take `over`.
    #[must_use]
    pub fn over(&self) -> bool {
        self.share().is_some_and(|s| s.mean >= 1.0)
    }
}

/// Where a device is attached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiskLocation {
    /// Inside the machine.
    Internal,
    /// Plugged in.
    External,
    /// A disk image or loop device.
    Image,
    /// Not said.
    #[default]
    Unknown,
}

/// One physical device's I/O counters since boot, as the host's block reader read them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockSample {
    /// Its identity across boots, 16 hex digits ([`is_device_id`]).
    pub id: String,
    /// What it calls itself: data, never an instruction.
    pub name: String,
    /// Where it is attached.
    pub location: DiskLocation,
    /// Bytes read.
    pub read_bytes: u64,
    /// Bytes written.
    pub write_bytes: u64,
    /// Read operations.
    pub reads: u64,
    /// Write operations.
    pub writes: u64,
    /// Time serving reads, ns.
    pub read_ns: u64,
    /// Time serving writes, ns.
    pub write_ns: u64,
}

/// Whether `id` is a device identity the model keeps: 16 lowercase hex digits.
#[must_use]
pub fn is_device_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One device tile (A1 §4.4): the write half rises from the seam, the read half grows down.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Disk {
    /// Its identity ([`BlockSample::id`]).
    pub id: String,
    /// What it calls itself.
    pub name: String,
    /// Where it is attached.
    pub location: DiskLocation,
    /// Bytes written a second, against the device's write peak.
    pub write: Rate,
    /// Bytes read a second, against its read peak.
    pub read: Rate,
}

/// What the units draw at one reading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Units {
    /// Each logical core's busy fraction, by logical id ([`crate::CpuView::cores`]); `None` for
    /// one with no reading in the window.
    pub cores: Vec<Option<Level>>,
    /// Each cluster, in [`crate::CpuView::clusters`] order: its cores' mean, and its busiest
    /// core's max as the tick.
    pub clusters: Vec<Option<Level>>,
    /// The queue tile, where the load and the cores were read.
    pub queue: Option<Queue>,
    /// Compressor churn, bytes a second, where the VM reads its compressor's counters.
    pub churn: Option<Rate>,
    /// The swap column, where memory was read.
    pub swap: Option<Swap>,
    /// The device tiles, home's first, at most [`DISK_UNITS`].
    pub disks: Vec<Disk>,
}

/// What one reading gives the units.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct UnitReading<'a> {
    /// The sample's wall clock, unix ms.
    pub now: i64,
    /// Its monotonic clock, ns: the disk rates' time base.
    pub at_ns: u64,
    /// Each core's busy fraction this reading, exactly when the CPU row reads their sum.
    pub cores: Option<&'a [f64]>,
    /// The cores the sample read, read or not this reading.
    pub n_cores: Option<usize>,
    /// The clusters, fastest first.
    pub clusters: &'a [ClusterView],
    /// The 1-minute load.
    pub load1: Option<f64>,
    /// Compressor churn this reading, bytes a second.
    pub churn_bps: Option<f64>,
    /// Whether the memory reading carried the VM's counters.
    pub churn_read: bool,
    /// Swap traffic this reading, bytes a second.
    pub swap_bps: Option<f64>,
    /// Swap in use, where memory was read.
    pub swap_used: Option<u64>,
    /// The devices read, home's first.
    pub block: &'a [BlockSample],
}

/// One device's running state.
#[derive(Debug)]
struct DiskRings {
    id: String,
    /// When its counters were last read (monotonic ns), and its bytes written and read then.
    last: Option<(u64, u64, u64)>,
    write: BucketRing,
    read: BucketRing,
}

/// The units' running state, beside the rows' ([`crate::Vitals`]).
#[derive(Debug)]
pub(crate) struct UnitRings {
    /// One per logical core, each pushed exactly when the CPU row's ring is.
    cores: Vec<BucketRing>,
    churn: BucketRing,
    swap: BucketRing,
    /// Most recently read first, at most [`DISK_DEVICES_KEPT`].
    disks: Vec<DiskRings>,
}

fn unit_ring() -> BucketRing {
    BucketRing::without_long(UNIT_RING_BUCKETS)
}

impl Default for UnitRings {
    fn default() -> Self {
        Self {
            cores: Vec::new(),
            churn: unit_ring(),
            swap: unit_ring(),
            disks: Vec::new(),
        }
    }
}

impl UnitRings {
    /// Takes one reading and says what the units draw; notes the closed buckets' sustained values
    /// in `peaks`.
    pub(crate) fn absorb(&mut self, r: &UnitReading<'_>, peaks: &mut Peaks) -> Units {
        let now = r.now;
        let cores = self.absorb_cores(r);
        let clusters = r
            .clusters
            .iter()
            .map(|c| {
                let levels: Option<Vec<Level>> = c
                    .cores
                    .iter()
                    .map(|&i| cores.get(i).copied().flatten())
                    .collect();
                let levels = levels.filter(|l| !l.is_empty())?;
                Some(Level {
                    mean: levels.iter().map(|l| l.mean).sum::<f64>() / levels.len() as f64,
                    max: levels
                        .iter()
                        .map(|l| l.max)
                        .fold(f64::NEG_INFINITY, f64::max),
                })
            })
            .collect();
        let busy = r.cores.map(|c| c.iter().sum::<f64>());
        let queue = r
            .n_cores
            .filter(|&n| n > 0)
            .zip(r.load1)
            .map(|(n, load1)| Queue {
                level: queue_level(load1, n),
                over: cpu_over(busy, Some(n), Some(load1)),
            });

        let closed = self.churn.push(now, r.churn_bps, false);
        note_peak(&self.churn, closed, &mut peaks.mem_churn);
        let churn = r
            .churn_read
            .then(|| Rate::of(&self.churn, now, &peaks.mem_churn));

        let _ = self.swap.push(now, r.swap_bps, false);
        let swap = r.swap_used.map(|used| Swap {
            used,
            traffic: Level::of(&self.swap, now),
        });

        let disks = self.absorb_disks(r, peaks);
        Units {
            cores,
            clusters,
            queue,
            churn,
            swap,
            disks,
        }
    }

    fn absorb_cores(&mut self, r: &UnitReading<'_>) -> Vec<Option<Level>> {
        let n = r.n_cores.unwrap_or(self.cores.len());
        if self.cores.len() != n {
            self.cores = (0..n).map(|_| unit_ring()).collect();
        }
        let fracs = r.cores.filter(|c| c.len() == n);
        for (i, ring) in self.cores.iter_mut().enumerate() {
            let _ = ring.push(r.now, fracs.map(|c| c[i]), false);
        }
        self.cores
            .iter()
            .map(|ring| Level::of(ring, r.now))
            .collect()
    }

    fn absorb_disks(&mut self, r: &UnitReading<'_>, peaks: &mut Peaks) -> Vec<Disk> {
        let mut out = Vec::new();
        for b in r.block.iter().filter(|b| is_device_id(&b.id)) {
            if out.len() == DISK_UNITS || out.iter().any(|d: &Disk| d.id == b.id) {
                continue;
            }
            let at = match self.disks.iter().position(|d| d.id == b.id) {
                Some(i) => i,
                None => {
                    self.disks.push(DiskRings {
                        id: b.id.clone(),
                        last: None,
                        write: unit_ring(),
                        read: unit_ring(),
                    });
                    self.disks.len() - 1
                }
            };
            let mut rings = self.disks.remove(at);
            let (write_bps, read_bps) = match rings.last {
                Some((then, w, rd))
                    if r.at_ns > then && b.write_bytes >= w && b.read_bytes >= rd =>
                {
                    let per_s = |d: u64| d as f64 * 1e9 / (r.at_ns - then) as f64;
                    (
                        Some(per_s(b.write_bytes - w)),
                        Some(per_s(b.read_bytes - rd)),
                    )
                }
                _ => (None, None),
            };
            rings.last = Some((r.at_ns, b.write_bytes, b.read_bytes));
            let peak = peaks.disk(&b.id);
            let closed = rings.write.push(r.now, write_bps, false);
            note_peak(&rings.write, closed, &mut peak.write);
            let closed = rings.read.push(r.now, read_bps, false);
            note_peak(&rings.read, closed, &mut peak.read);
            out.push(Disk {
                id: b.id.clone(),
                name: b.name.clone(),
                location: b.location,
                write: Rate::of(&rings.write, r.now, &peak.write),
                read: Rate::of(&rings.read, r.now, &peak.read),
            });
            self.disks.insert(0, rings);
        }
        self.disks.truncate(DISK_DEVICES_KEPT);
        out
    }
}

#[cfg(test)]
#[path = "units_tests.rs"]
mod tests;
