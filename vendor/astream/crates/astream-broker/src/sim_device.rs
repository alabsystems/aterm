// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SIMULATED LOG DEVICE: a log file that can lose power after any operation, behind
//! the storage seam (`storage.rs`). The crash simulation (`crash_sim.rs`) runs the REAL
//! writer and the REAL open against it and states its law (P1-P8); an embedder's tests
//! open a real [`BrokerLog`](crate::BrokerLog) or [`Broker`](crate::Broker) on one
//! ([`SimDevice`]) to hold what astream recovers from each power loss to a model of
//! their own.
//!
//! # The device
//!
//! The log file is a byte array in fixed-size blocks. Each block is VOLATILE (written
//! since the last flush, in the page cache only), ISSUED in barrier epoch `k`, or
//! DURABLE. A write makes blocks volatile; an ordering BARRIER (`F_BARRIERFSYNC`, the
//! Ordered tier's per-batch flush) moves the volatile blocks — their contents then — and
//! the file's size into a new epoch; a full flush (`sync_data`/`sync_all`) makes every
//! block, every issued epoch, and the file's size durable. The identity sidecar is
//! replaced atomically and is durable when the replacement returns (tmp + fsync +
//! rename, an `F_FULLFSYNC`): it DRAINS every issued epoch but does NOT make the log's
//! volatile blocks durable.
//!
//! A POWER LOSS image is every durable byte, then every block of a PREFIX of the issued
//! epochs (each at its size), then a subset of the NEXT epoch's blocks — the volatile
//! blocks counting as one more epoch after the newest barrier — with or without that
//! epoch's size change; space nothing persisted reads zeros or `0xEE` garbage. That is
//! fcntl(2)'s barrier contract: nothing issued after a barrier reaches the media before
//! everything issued before it. A volatile block persists as it reads at the loss.
//!
//! # Where it is compiled
//!
//! Under `cfg(any(test, debug_assertions))`, aterm's seam rule: a dependent's tests build
//! with debug assertions on and see it without a feature a dev-dependency would flip. A
//! release build has none of it: the seam's enums hold the real file alone (the
//! release-build checks in `storage.rs`, on its `Backing`, `HandleBacking` and `Dir`,
//! and in `store.rs`, on its `Storage`, stop compiling otherwise), and every match on
//! them compiles to the call it wraps.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One barrier epoch: the blocks a barrier issued, with their contents then, and the
/// file's size then.
#[derive(Clone, Debug)]
pub(crate) struct Epoch {
    pub(crate) blocks: BTreeMap<usize, Vec<u8>>,
    pub(crate) size: usize,
}

/// One state of the simulated device.
#[derive(Clone)]
pub(crate) struct Device {
    pub(crate) block: usize,
    pub(crate) exists: bool,
    /// What reads see: the page cache.
    pub(crate) logical: Vec<u8>,
    /// What the media holds; its length is the durable size.
    pub(crate) durable: Vec<u8>,
    /// Blocks written since the last barrier or full flush (volatile).
    pub(crate) dirty: BTreeSet<usize>,
    /// Barrier epochs issued since the last full flush, oldest first.
    pub(crate) epochs: Vec<Epoch>,
    /// NC1: the plain-fsync device, on which a barrier orders nothing.
    pub(crate) ignore_barriers: bool,
    /// The identity sidecar (durable when replaced).
    pub(crate) id: Option<Vec<u8>>,
    /// The file's length at the last full flush the PRODUCT issued: bytes a flush
    /// covered (for P3), unlike `durable`, which an image makes everything.
    pub(crate) flushed: usize,
    /// Where the write that produced this state started, when it was a log append:
    /// a process killed during that write leaves any page-aligned prefix of it (a
    /// SHORT WRITE — Linux stops a write between pages for a fatal signal).
    #[cfg(test)]
    pub(crate) appended_from: Option<usize>,
    /// What the crash simulation had told clients when the device stood here.
    #[cfg(test)]
    pub(crate) meta: crate::crash_sim::Meta,
}

impl Device {
    pub(crate) fn new(block: usize) -> Device {
        Device {
            block,
            exists: false,
            logical: Vec::new(),
            durable: Vec::new(),
            dirty: BTreeSet::new(),
            epochs: Vec::new(),
            ignore_barriers: false,
            id: None,
            flushed: 0,
            #[cfg(test)]
            appended_from: None,
            #[cfg(test)]
            meta: crate::crash_sim::Meta::default(),
        }
    }

    /// Note where the write that left this state began (`None`: it was no log append),
    /// the start of what a process killed during it may have left (`short_writes`).
    #[cfg(test)]
    fn appended(&mut self, from: Option<usize>) {
        self.appended_from = from;
    }

    #[cfg(not(test))]
    const fn appended(&mut self, _from: Option<usize>) {}

    /// The contents of block `b` as reads see it now (short at the file's end).
    fn block_now(&self, b: usize) -> Vec<u8> {
        let from = (b * self.block).min(self.logical.len());
        let to = ((b + 1) * self.block).min(self.logical.len());
        self.logical[from..to].to_vec()
    }

    /// Persist `epoch` onto `bytes`: the file takes the epoch's size (new space reading
    /// `fill`), and every block it issued its contents then.
    fn apply(bytes: &mut Vec<u8>, block: usize, epoch: &Epoch, fill: u8) {
        bytes.resize(epoch.size, fill);
        for (&b, data) in &epoch.blocks {
            let from = b * block;
            let to = (from + data.len()).min(epoch.size);
            if from < to {
                bytes[from..to].copy_from_slice(&data[..to - from]);
            }
        }
    }

    /// The volatile blocks as the epoch a barrier would issue now.
    fn volatile_epoch(&self) -> Epoch {
        Epoch {
            blocks: self.dirty.iter().map(|&b| (b, self.block_now(b))).collect(),
            size: self.logical.len(),
        }
    }

    fn touch(&mut self, from: usize, to: usize) {
        if to > from {
            for b in from / self.block..to.div_ceil(self.block) {
                self.dirty.insert(b);
            }
        }
    }

    /// The epoch after the first `full` issued ones: the next issued epoch, or — past
    /// them all — the volatile blocks, the one more epoch after the newest barrier.
    fn epoch_after(&self, full: usize) -> Epoch {
        self.epochs
            .get(full)
            .cloned()
            .unwrap_or_else(|| self.volatile_epoch())
    }

    /// The file's size once the first `full` issued epochs are persisted.
    fn size_after(&self, full: usize) -> usize {
        match full {
            0 => self.durable.len(),
            n => self.epochs[n - 1].size,
        }
    }

    /// A power-loss image: the durable bytes, every block of the first `full` issued
    /// epochs, then the blocks `keep` picks from the epoch after them, at that epoch's
    /// size or the one before it, unpersisted space reading `fill`.
    pub(crate) fn image(
        &self,
        full: usize,
        keep: &BTreeSet<usize>,
        new_size: bool,
        fill: u8,
    ) -> Device {
        let mut bytes = self.durable.clone();
        for e in &self.epochs[..full] {
            Device::apply(&mut bytes, self.block, e, fill);
        }
        let next = self.epoch_after(full);
        let size = if new_size { next.size } else { bytes.len() };
        bytes.resize(size, fill);
        for b in keep {
            if let Some(data) = next.blocks.get(b) {
                let from = b * self.block;
                let to = (from + data.len()).min(size);
                if from < to {
                    bytes[from..to].copy_from_slice(&data[..to - from]);
                }
            }
        }
        Device {
            block: self.block,
            exists: self.exists,
            logical: bytes.clone(),
            durable: bytes,
            dirty: BTreeSet::new(),
            epochs: Vec::new(),
            ignore_barriers: self.ignore_barriers,
            id: self.id.clone(),
            flushed: self.flushed.min(size),
            #[cfg(test)]
            appended_from: None,
            #[cfg(test)]
            meta: self.meta.clone(),
        }
    }

    /// The SHORT WRITES of the append that produced this state: the file as a process
    /// killed during that write leaves it — the bytes before the write plus a prefix of
    /// it ending on a block boundary (Linux checks for a fatal signal between pages, so
    /// a write it stops ends on one), at every such boundary inside it. Process-crash
    /// images: the kernel runs on, so what reads see is what it holds.
    #[cfg(test)]
    pub(crate) fn short_writes(&self) -> Vec<Device> {
        let Some(from) = self.appended_from else {
            return Vec::new();
        };
        let to = self.logical.len();
        (from / self.block + 1..)
            .map(|b| b * self.block)
            .take_while(|&end| end < to)
            .map(|end| {
                let mut d = self.clone();
                d.logical.truncate(end);
                d.durable.truncate(end);
                d.dirty.retain(|&b| b * d.block < end);
                d.appended_from = None;
                d
            })
            .collect()
    }
}

struct State {
    dev: Device,
    trace: Option<Vec<Device>>,
}

impl State {
    fn record(&mut self) {
        if let Some(t) = &mut self.trace {
            t.push(self.dev.clone());
        }
    }
}

/// The simulated device behind the storage seam: cheap to clone (one shared state).
#[derive(Clone)]
pub(crate) struct Sim {
    inner: Arc<Mutex<State>>,
    path: Arc<PathBuf>,
}

impl Sim {
    /// A fresh, empty simulated device of 16-byte blocks: what a unit test elsewhere in
    /// the crate opens a log on when the behaviour it checks must not depend on the
    /// platform's real flush (the simulated barrier exists everywhere).
    #[cfg(test)]
    pub(crate) fn fresh() -> Sim {
        Sim::with(Device::new(16))
    }

    pub(crate) fn with(dev: Device) -> Sim {
        Sim {
            inner: Arc::new(Mutex::new(State { dev, trace: None })),
            path: Arc::new(PathBuf::from("sim.log")),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Open the log file, creating it if absent; whether this call created it.
    pub(crate) fn open_file(&self) -> bool {
        let mut s = self.lock();
        let created = !s.dev.exists;
        s.dev.exists = true;
        created
    }

    pub(crate) fn read_log(&self, out: &mut Vec<u8>) -> usize {
        let s = self.lock();
        out.extend_from_slice(&s.dev.logical);
        s.dev.logical.len()
    }

    pub(crate) fn append(&self, bytes: &[u8]) {
        let mut s = self.lock();
        let from = s.dev.logical.len();
        s.dev.logical.extend_from_slice(bytes);
        let to = s.dev.logical.len();
        s.dev.touch(from, to);
        s.dev.appended(Some(from));
        s.record();
    }

    pub(crate) fn set_len(&self, len: u64) {
        let mut s = self.lock();
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        let old = s.dev.logical.len();
        s.dev.logical.resize(len, 0);
        // The block holding the new end changes (growth zero-fills it); a pure
        // truncation changes the size, which an image takes or leaves.
        s.dev.touch(len.min(old), len.max(old));
        if len < old && len % s.dev.block != 0 {
            let b = len / s.dev.block;
            s.dev.dirty.insert(b);
        }
        s.dev.appended(None);
        s.record();
    }

    pub(crate) fn full_flush(&self) {
        let mut s = self.lock();
        s.dev.durable = s.dev.logical.clone();
        s.dev.dirty.clear();
        s.dev.epochs.clear();
        s.dev.flushed = s.dev.logical.len();
        s.dev.appended(None);
        s.record();
    }

    /// An ordering barrier: the volatile blocks, as they read now, and the file's size
    /// become a new issued epoch — persisted after every earlier epoch and before any
    /// later write, but not yet durable. On the plain-fsync device (NC1) it orders
    /// nothing.
    pub(crate) fn barrier(&self) {
        let mut s = self.lock();
        if !s.dev.ignore_barriers {
            let epoch = s.dev.volatile_epoch();
            s.dev.epochs.push(epoch);
            s.dev.dirty.clear();
        }
        s.dev.appended(None);
        s.record();
    }

    pub(crate) fn read_id(&self) -> Option<Vec<u8>> {
        self.lock().dev.id.clone()
    }

    /// Replace the sidecar, durably: an `F_FULLFSYNC` of its own, which drains every
    /// issued epoch of the log onto the media (the device empties its queue) but leaves
    /// the log's volatile blocks in the page cache.
    pub(crate) fn replace_id(&self, bytes: &[u8]) {
        let mut s = self.lock();
        s.dev.id = Some(bytes.to_vec());
        let block = s.dev.block;
        for epoch in std::mem::take(&mut s.dev.epochs) {
            Device::apply(&mut s.dev.durable, block, &epoch, 0);
        }
        s.dev.appended(None);
        s.record();
    }

    #[cfg(test)]
    pub(crate) fn set_meta(&self, meta: crate::crash_sim::Meta) {
        self.lock().dev.meta = meta;
    }

    #[cfg(test)]
    pub(crate) fn meta(&self) -> crate::crash_sim::Meta {
        self.lock().dev.meta.clone()
    }

    /// Record the current state as a crash point with no device operation: the instant
    /// after an ack, whose device state is the operation's before it (and whose write,
    /// acked, finished: no short write of it is left to take).
    #[cfg(test)]
    pub(crate) fn mark(&self) {
        let mut s = self.lock();
        s.dev.appended(None);
        s.record();
    }

    #[cfg(test)]
    pub(crate) fn trace_on(&self) {
        let mut s = self.lock();
        let start = s.dev.clone();
        s.trace = Some(vec![start]);
    }

    #[cfg(test)]
    pub(crate) fn take_trace(&self) -> Vec<Device> {
        self.lock().trace.take().unwrap_or_default()
    }

    pub(crate) fn device(&self) -> Device {
        self.lock().dev.clone()
    }

    /// Put `bytes` in place of the log, durably, keeping the sidecar: a restored copy.
    #[cfg(test)]
    pub(crate) fn restore(&self, bytes: &[u8]) {
        let mut s = self.lock();
        s.dev.logical = bytes.to_vec();
        s.dev.durable = bytes.to_vec();
        s.dev.dirty.clear();
        s.dev.epochs.clear();
        s.dev.flushed = bytes.len();
    }
}

/// A SIMULATED LOG DEVICE an embedder's test opens a real log on
/// ([`BrokerLog::open_sim`](crate::BrokerLog::open_sim),
/// [`Broker::open_sim`](crate::Broker::open_sim)): the writer, the open and the clean
/// close run unchanged against it, and [`power_loss_images`](Self::power_loss_images)
/// names every state a power loss could leave it in, each a device of its own that the
/// next open recovers from. Cheap to clone: a clone is the same device. A test seam,
/// absent from a release build (module docs, "Where it is compiled").
///
/// Retention's rewrite, anti-rollback and the health sidecar live in files beside a
/// real log and are refused on a simulated one, and a log sealed at rest keeps no
/// `<log>.seal` binding there.
#[derive(Clone)]
pub struct SimDevice {
    sim: Sim,
}

impl SimDevice {
    /// An empty device of `block`-byte blocks, the unit a power loss keeps or loses (the
    /// crash simulation sweeps 16, 32 and 64: every 4 KiB outcome is one of theirs, and
    /// smaller blocks only add places a write can tear).
    ///
    /// # Panics
    ///
    /// If `block` is 0.
    #[must_use]
    pub fn new(block: usize) -> SimDevice {
        assert!(
            block > 0,
            "a simulated device's blocks hold a byte at least"
        );
        SimDevice {
            sim: Sim::with(Device::new(block)),
        }
    }

    /// THE POWER-LOSS IMAGES of the device as it stands now, each a device of its own
    /// holding one, all of it durable, its sidecar the one last replaced (module docs, "The
    /// device"): for each prefix of the barrier epochs issued since the last full flush,
    /// every subset of the next epoch's blocks with and without its size change while
    /// that fits `budget` images, and beyond it a sample seeded by `seed` (none, all,
    /// every prefix, each block alone, all but each one, random subsets) trimmed to
    /// `budget`, none and all never trimmed. With whether they are EVERY image the device
    /// allows (`false`: a sample was taken somewhere). The device itself is left as it was.
    #[must_use]
    pub fn power_loss_images(&self, budget: usize, seed: u64) -> (Vec<SimDevice>, bool) {
        let mut rng = Rng(seed | 1);
        let (images, every) = images(&self.sim.device(), budget, &mut rng);
        let images = images
            .into_iter()
            .map(|dev| SimDevice {
                sim: Sim::with(dev),
            })
            .collect();
        (images, every)
    }

    /// The crash simulation's device `sim`, as an embedder holds one.
    #[cfg(test)]
    pub(crate) const fn on(sim: Sim) -> SimDevice {
        SimDevice { sim }
    }

    /// The seam's own handle on the device.
    pub(crate) const fn sim(&self) -> &Sim {
        &self.sim
    }
}

/// xorshift64*: the simulation's only source of choice, seeded.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Up to `budget` power-loss images of `dev` for each prefix of its issued epochs, and
/// whether they are EVERY image: with the first `full` epochs persisted whole (`full`
/// from none to all of them), every subset of the NEXT epoch's blocks — the volatile
/// blocks, past the newest barrier — with and without its size change, while that fits
/// the budget (2^n subsets of n blocks, so at most 6 blocks at a budget of 64, or 5 with
/// a size change pending); beyond it, a SAMPLE — the structured subsets (none, all, each
/// prefix, each block alone, all but each one) and seeded random ones, trimmed to the
/// budget, none and all never trimmed ([`sample`]). Either way unpersisted space reads
/// zeros in one image and `0xEE` garbage in the next, alternating. Strict and Relaxed
/// issue no barrier, so for them this is the volatile blocks alone, as before the
/// Ordered tier.
pub(crate) fn images(dev: &Device, budget: usize, rng: &mut Rng) -> (Vec<Device>, bool) {
    let mut out = Vec::new();
    let mut every_prefix = true;
    let mut image_no = 0usize;
    for full in 0..=dev.epochs.len() {
        let next = dev.epoch_after(full);
        let before = dev.size_after(full);
        // An empty epoch (a barrier with nothing new, the open's probe) persists nothing
        // the next prefix does not: skip it, but never the volatile set at the end.
        if full < dev.epochs.len() && next.blocks.is_empty() && next.size == before {
            continue;
        }
        let blocks: Vec<usize> = next.blocks.keys().copied().collect();
        let sizes: &[bool] = if next.size == before {
            &[true]
        } else {
            &[false, true]
        };
        let n = blocks.len();
        let every = n < 63 && (1usize << n) * sizes.len() <= budget;
        every_prefix &= every;
        let subsets: Vec<BTreeSet<usize>> = if every {
            (0u64..(1u64 << n))
                .map(|mask| {
                    blocks
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| mask & (1 << i) != 0)
                        .map(|(_, &b)| b)
                        .collect()
                })
                .collect()
        } else {
            sample(&blocks, sizes.len(), budget, rng)
        };
        for keep in &subsets {
            for &new_size in sizes {
                // Zeros for unpersisted space, and every other image 0xEE garbage.
                let fill = if image_no % 2 == 1 { 0xEE } else { 0 };
                out.push(dev.image(full, keep, new_size, fill));
            }
            image_no += 1;
        }
    }
    (out, every_prefix)
}

/// One subset of an epoch's blocks the sample takes ([`sample`]), named until it is
/// kept, so a sample of many blocks builds only the subsets that survive its trim.
enum Pick {
    /// The first `k` blocks (none at `0`, every one at their count).
    Prefix(usize),
    /// The block at this index alone.
    Alone(usize),
    /// Every block but the one at this index.
    AllBut(usize),
    /// A seeded random subset.
    Drawn(BTreeSet<usize>),
}

/// THE SAMPLE of an epoch's `blocks`, too many for every subset to fit the budget: none,
/// all, each prefix, each block alone and all but each one, then seeded random subsets
/// while they make fewer than `budget` images (`per` to a subset). Over the room the
/// budget leaves (`budget / per` subsets, 2 at least), the head of that list is kept —
/// none and all always, then the shortest prefixes, the shapes a torn append leaves, up
/// to half the room — and a seeded pick of the rest fills it, so the longer prefixes of
/// an epoch of more blocks than half the room are a sample too.
fn sample(blocks: &[usize], per: usize, budget: usize, rng: &mut Rng) -> Vec<BTreeSet<usize>> {
    let n = blocks.len();
    let mut picks: Vec<Pick> = Vec::with_capacity(3 * n + 1);
    // None and all lead: the trim below keeps the head of the list, and these two are the
    // ones no other pick stands for (nothing of the epoch persisted, or every block of it).
    picks.push(Pick::Prefix(0));
    if n > 0 {
        picks.push(Pick::Prefix(n));
    }
    picks.extend((1..n).map(Pick::Prefix));
    for i in 0..n {
        picks.push(Pick::Alone(i));
        picks.push(Pick::AllBut(i));
    }
    while picks.len() * per < budget {
        picks.push(Pick::Drawn(
            blocks
                .iter()
                .copied()
                .filter(|_| rng.below(2) == 0)
                .collect(),
        ));
    }
    let room = (budget / per).max(2);
    // At most `room`, as `room` is 2 at least: none and all are never trimmed.
    let keep = (room / 2).max(2);
    while picks.len() > room {
        let at = keep + rng.below((picks.len() - keep) as u64) as usize;
        picks.swap_remove(at);
    }
    picks
        .into_iter()
        .map(|pick| match pick {
            Pick::Prefix(k) => blocks[..k].iter().copied().collect(),
            Pick::Alone(i) => [blocks[i]].into_iter().collect(),
            Pick::AllBut(i) => blocks
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, &b)| b)
                .collect(),
            Pick::Drawn(set) => set,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{sample, Rng};

    /// A SAMPLE KEEPS THE NONE AND ALL IMAGES: an epoch too big for every subset still
    /// reopens the image that persisted none of its blocks and the one that persisted all
    /// of them, at any block count and whether or not its size changed (`per` 2 or 1), so
    /// a trimmed sample never loses the one image with nothing lost.
    #[test]
    fn a_sample_keeps_the_none_and_all_images_at_any_block_count() {
        for n in [7usize, 15, 16, 17, 31, 32, 33, 40, 100] {
            let blocks: Vec<usize> = (0..n).map(|b| 3 * b + 1).collect();
            let all: BTreeSet<usize> = blocks.iter().copied().collect();
            for per in [1usize, 2] {
                let room = (64 / per).max(2);
                for seed in 0..64u64 {
                    let picks = sample(&blocks, per, 64, &mut Rng(seed | 1));
                    let at = format!("{n} blocks, {per} images a subset, seed {seed}");
                    assert!(picks.len() <= room, "{at}: {} picks", picks.len());
                    assert!(picks.contains(&BTreeSet::new()), "{at}: none trimmed");
                    assert!(picks.contains(&all), "{at}: all trimmed");
                }
            }
        }
    }
}
