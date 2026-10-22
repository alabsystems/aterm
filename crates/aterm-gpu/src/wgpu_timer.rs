// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wgpu arm's GPU clock for `crate::gpu_time`: per-pass timestamp queries.
//!
//! wgpu has no command-buffer clock, so a timed present asks each of its render
//! passes for a beginning and an end timestamp (`RenderPassTimestampWrites`,
//! which needs only `Features::TIMESTAMP_QUERY`; writes between passes would
//! need `TIMESTAMP_QUERY_INSIDE_ENCODERS`, which fewer adapters carry). Copies
//! between passes (the present copy, the band shift) carry no timestamp and are
//! not counted.
//!
//! # The readback waits for the frame
//!
//! When the present returns, its frame is sealed and the ring asks the queue to
//! report when the work submitted so far has completed
//! (`Queue::on_submitted_work_done`). Only then are the used queries resolved,
//! copied to a mappable buffer and mapped asynchronously; a later present (or
//! [`TimestampRing::harvest`]) finds the map complete and settles the frame.
//! Resolving right behind the frame's submit raced the device: on Metal the
//! resolve is a blit in a later command buffer, a counter sample buffer is not a
//! tracked resource, and the last pass's end-of-fragment sample read 0 in 5 of
//! 15 presents (a healthy frame dropped as a failed one).
//!
//! # One part per pass, the union of their intervals
//!
//! A readback reports its frame's passes as one held part that counts one part
//! per timed pass ([`crate::gpu_time::FrameToken::complete_parts`]), so a
//! frame's part count is the number of passes it timed. Its span is the UNION
//! of the pass intervals in ticks times `Queue::get_timestamp_period`: a tiled
//! GPU starts a pass's vertex work under the previous pass's fragment work
//! (measured on this desk's wgpu-on-Metal: 12.9-17.3 µs of overlap between
//! consecutive passes), so a sum of spans would count the overlap twice.
//!
//! # Fail closed
//!
//! Nothing here runs unless a `gpu_time` sink is installed and the device was
//! granted `TIMESTAMP_QUERY` (`terminal_device_features`): the renderer builds
//! the timer lazily, and a pass asks [`WgpuGpuTimer::pass_writes`] only while a
//! frame is open. A frame that runs out of query pairs, whose readback map
//! fails, whose timer is dropped before it settled, or whose readback holds a
//! zero (unsampled) or reversed pair is dropped (`gpu_time::dropped`), never
//! reported as the passes it could time.
//!
//! The ring's states are driven through [`Clock`], the device side, so each of
//! those failures is tested on a fake device as well as the healthy path on
//! the wgpu oracle (`tests/frame_instrumentation.rs`).

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::gpu_time::{FrameRing, FrameToken};

/// Timed passes per frame: the offscreen plan's passes, the effect passes
/// (bloom extract and composite, shimmer, card) and the blit, with headroom.
const PASS_PAIRS: u32 = 32;
/// Frames whose readback may be in flight at once; a present that finds every
/// slot busy goes untimed (`gpu_time::skipped`). A frame holds its slot for
/// about three presents (its work completes, its resolve maps, a harvest
/// reads it), so eight keep a device a frame or two behind timed.
const SLOTS: usize = 8;

/// The map callback's report.
const MAP_PENDING: u8 = 0;
const MAP_OK: u8 = 1;
const MAP_FAILED: u8 = 2;

/// The bytes a readback of `pairs` passes resolves: a begin and an end tick
/// (`u64`, little-endian) per pass.
#[inline]
const fn readback_bytes(pairs: u32) -> u64 {
    (pairs * 2 * wgpu::QUERY_SIZE) as u64
}

/// The device side of the ring: everything [`TimestampRing`] asks of wgpu,
/// so its states, and every way a frame fails, are driven without a GPU.
pub(crate) trait Clock {
    /// One slot's device resources: its query set and readback buffers.
    type Slot;
    /// Advance the device's callbacks without blocking.
    fn poll(&self);
    /// Raise `done` once every piece of work submitted so far has completed.
    fn on_done(&self, done: Arc<AtomicBool>);
    /// Resolve `slot`'s first `pairs` query pairs into its readback, submit
    /// that, and start the map; the map's callback stores [`MAP_OK`] or
    /// [`MAP_FAILED`] in `report`.
    fn resolve(&self, slot: &Self::Slot, pairs: u32, report: Arc<AtomicU8>);
    /// Hand the mapped `pairs` pairs to `read`, then unmap.
    fn read(&self, slot: &Self::Slot, pairs: u32, read: &mut dyn FnMut(&[u8]));
}

/// wgpu's side of the ring: the renderer's device and queue.
pub(crate) struct WgpuClock<'a> {
    pub(crate) device: &'a wgpu::Device,
    pub(crate) queue: &'a wgpu::Queue,
}

/// One wgpu slot's resources.
pub(crate) struct WgpuSlot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
}

impl Clock for WgpuClock<'_> {
    type Slot = WgpuSlot;

    #[inline]
    fn poll(&self) {
        let _ = self.device.poll(wgpu::PollType::Poll);
    }

    #[inline]
    fn on_done(&self, done: Arc<AtomicBool>) {
        self.queue
            .on_submitted_work_done(move || done.store(true, Ordering::Release));
    }

    fn resolve(&self, slot: &WgpuSlot, pairs: u32, report: Arc<AtomicU8>) {
        let bytes = readback_bytes(pairs);
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aterm-gpu gpu-time resolve"),
            });
        enc.resolve_query_set(&slot.queries, 0..pairs * 2, &slot.resolve, 0);
        enc.copy_buffer_to_buffer(&slot.resolve, 0, &slot.read, 0, bytes);
        crate::device_layer::submit_host_readable(
            self.device,
            self.queue,
            enc.finish(),
            std::slice::from_ref(&slot.read),
        );
        #[expect(
            clippy::disallowed_methods,
            reason = "the copy was submitted by device_layer::submit_host_readable"
        )]
        slot.read
            .map_async(wgpu::MapMode::Read, 0..bytes, move |r| {
                report.store(
                    if r.is_ok() { MAP_OK } else { MAP_FAILED },
                    Ordering::Release,
                );
            });
    }

    fn read(&self, slot: &WgpuSlot, pairs: u32, read: &mut dyn FnMut(&[u8])) {
        {
            let view = slot.read.get_mapped_range(0..readback_bytes(pairs));
            read(&view);
        }
        slot.read.unmap();
    }
}

/// Where one slot is in its cycle.
enum SlotState<'r> {
    /// Ready to time a frame.
    Free,
    /// Timing the open frame: passes take pairs from `next_pair`.
    Writing(FrameToken<'r>),
    /// Sealed with `pairs` passes timed; `done` rises when the device has
    /// finished the frame's work, before which a resolve could read a sample
    /// the device has not written.
    Running {
        token: FrameToken<'r>,
        pairs: u32,
        done: Arc<AtomicBool>,
    },
    /// The readback of `pairs` pass pairs is mapping; `map` is the callback's
    /// report.
    Mapping {
        token: FrameToken<'r>,
        pairs: u32,
        map: Arc<AtomicU8>,
    },
}

struct Slot<'r, S> {
    res: S,
    state: SlotState<'r>,
}

/// The per-renderer timestamp ring (see the module header), reporting into
/// `frames` through slots whose device resources are `S`.
pub(crate) struct TimestampRing<'r, S> {
    frames: &'r FrameRing,
    slots: Vec<Slot<'r, S>>,
    /// Nanoseconds per timestamp tick.
    period_ns: f64,
    /// The slot timing the open frame, if any.
    writing: Option<usize>,
    /// Query pairs handed out to the open frame's passes.
    next_pair: Cell<u32>,
    /// A pass of the open frame asked for a pair past [`PASS_PAIRS`].
    overflowed: Cell<bool>,
}

/// The renderer's clock: the process's ring over wgpu slots.
pub(crate) type WgpuGpuTimer = TimestampRing<'static, WgpuSlot>;

impl WgpuGpuTimer {
    /// The timer for `device`, or `None` when the device was not granted
    /// timestamp queries.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let bytes = readback_bytes(PASS_PAIRS);
        let slots = (0..SLOTS)
            .map(|_| WgpuSlot {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("aterm-gpu gpu-time queries"),
                    ty: wgpu::QueryType::Timestamp,
                    count: PASS_PAIRS * 2,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aterm-gpu gpu-time resolve"),
                    size: bytes,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                read: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("aterm-gpu gpu-time readback"),
                    size: bytes,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
            })
            .collect();
        Some(Self::with_slots(
            crate::gpu_time::frames(),
            slots,
            f64::from(queue.get_timestamp_period()),
        ))
    }

    /// The timestamp writes for the next pass of the open frame, or `None`
    /// when no frame is open or the frame is out of pairs.
    #[inline]
    pub(crate) fn pass_writes(&self) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let (slot, pair) = self.next_pass()?;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &slot.queries,
            beginning_of_pass_write_index: Some(pair * 2),
            end_of_pass_write_index: Some(pair * 2 + 1),
        })
    }
}

impl<'r, S> TimestampRing<'r, S> {
    /// A ring over `slots` reporting into `frames`, at `period_ns` per tick.
    fn with_slots(frames: &'r FrameRing, slots: Vec<S>, period_ns: f64) -> Self {
        Self {
            frames,
            slots: slots
                .into_iter()
                .map(|res| Slot {
                    res,
                    state: SlotState::Free,
                })
                .collect(),
            period_ns,
            writing: None,
            next_pair: Cell::new(0),
            overflowed: Cell::new(false),
        }
    }

    /// Open a timed frame for the present about to encode: harvest finished
    /// readbacks, then take a free slot. Returns the `gpu_time` serial, or
    /// `None` when no sink listens or every slot is still in flight.
    pub(crate) fn begin<C: Clock<Slot = S>>(&mut self, clock: &C) -> Option<u64> {
        self.abandon_open();
        self.harvest(clock);
        let free = self
            .slots
            .iter()
            .position(|s| matches!(s.state, SlotState::Free));
        let Some(index) = free else {
            self.frames.note_skipped();
            return None;
        };
        let token = self.frames.open_frame()?;
        self.slots[index].state = SlotState::Writing(token);
        self.writing = Some(index);
        self.next_pair.set(0);
        self.overflowed.set(false);
        Some(token.serial())
    }

    /// The open frame's slot and the pair index for its next pass, or `None`
    /// when no frame is open. A pass past the per-frame budget gets none and
    /// marks the frame overflowed (it will be dropped, not under-reported).
    fn next_pass(&self) -> Option<(&S, u32)> {
        let slot = &self.slots[self.writing?];
        let pair = self.next_pair.get();
        if pair >= PASS_PAIRS {
            self.overflowed.set(true);
            return None;
        }
        self.next_pair.set(pair + 1);
        Some((&slot.res, pair))
    }

    /// The present returned (its work submitted): seal the open frame and ask
    /// to hear when the device has run it. The readback waits for that
    /// ([`Self::harvest`]).
    pub(crate) fn finish<C: Clock<Slot = S>>(&mut self, clock: &C) {
        let Some(index) = self.writing.take() else {
            return;
        };
        let slot = &mut self.slots[index];
        let SlotState::Writing(token) = slot.state else {
            unreachable!("the writing slot is in the Writing state");
        };
        let pairs = self.next_pair.get();
        slot.state = if self.overflowed.get() {
            token.hold();
            token.complete(None);
            token.seal();
            SlotState::Free
        } else if pairs == 0 {
            token.seal();
            SlotState::Free
        } else {
            token.hold();
            token.seal();
            let done = Arc::new(AtomicBool::new(false));
            clock.on_done(Arc::clone(&done));
            SlotState::Running { token, pairs, done }
        };
    }

    /// Move each frame along without blocking (a device poll first): one the
    /// device has finished starts its readback, and one whose readback has
    /// mapped settles. Called at each timed present's start; a reader that
    /// needs a result without presenting again calls it directly.
    pub(crate) fn harvest<C: Clock<Slot = S>>(&mut self, clock: &C) {
        clock.poll();
        let period_ns = self.period_ns;
        for slot in &mut self.slots {
            slot.state = match std::mem::replace(&mut slot.state, SlotState::Free) {
                SlotState::Running { token, pairs, done } if done.load(Ordering::Acquire) => {
                    let map = Arc::new(AtomicU8::new(MAP_PENDING));
                    clock.resolve(&slot.res, pairs, Arc::clone(&map));
                    SlotState::Mapping { token, pairs, map }
                }
                SlotState::Mapping { token, pairs, map } => match map.load(Ordering::Acquire) {
                    MAP_PENDING => SlotState::Mapping { token, pairs, map },
                    MAP_OK => {
                        let mut busy = None;
                        clock.read(&slot.res, pairs, &mut |bytes| {
                            busy = busy_ns(bytes, period_ns);
                        });
                        token.complete_parts(busy, pairs);
                        SlotState::Free
                    }
                    _ => {
                        token.complete_parts(None, pairs);
                        SlotState::Free
                    }
                },
                state => state,
            };
        }
    }

    /// Seal a frame a present opened but never finished (it panicked or
    /// returned early), as untimed.
    fn abandon_open(&mut self) {
        if let Some(index) = self.writing.take() {
            if let SlotState::Writing(token) = self.slots[index].state {
                token.seal();
            }
            self.slots[index].state = SlotState::Free;
        }
    }
}

impl<S> Drop for TimestampRing<'_, S> {
    /// A frame still open is sealed untimed; one sealed but not yet read
    /// back fails, never left outstanding in the ring.
    fn drop(&mut self) {
        for slot in &mut self.slots {
            match std::mem::replace(&mut slot.state, SlotState::Free) {
                SlotState::Writing(token) => token.seal(),
                SlotState::Running { token, pairs, .. }
                | SlotState::Mapping { token, pairs, .. } => {
                    token.complete_parts(None, pairs);
                }
                SlotState::Free => {}
            }
        }
    }
}

/// The device busy time of the passes resolved in `bytes` (little-endian
/// `u64` ticks, begin then end per pass), in nanoseconds: the union of their
/// intervals, so overlapping passes count once. `None` (the frame is dropped)
/// for no pass, a pair with a zero tick (a sample the device never wrote), a
/// pair that runs backwards (a reset or garbage clock), or a tick period or
/// total outside the nanosecond domain.
pub(crate) fn busy_ns(bytes: &[u8], period_ns: f64) -> Option<u64> {
    let mut spans = [(0_u64, 0_u64); PASS_PAIRS as usize];
    let mut n = 0;
    for pair in bytes.as_chunks::<16>().0 {
        let begin = u64::from_le_bytes(pair[..8].try_into().ok()?);
        let end = u64::from_le_bytes(pair[8..].try_into().ok()?);
        if begin == 0 || end < begin {
            return None;
        }
        *spans.get_mut(n)? = (begin, end);
        n += 1;
    }
    let spans = &mut spans[..n];
    spans.sort_unstable();
    let (&first, rest) = spans.split_first()?;
    let (mut ticks, mut open) = (0_u128, first);
    for &(begin, end) in rest {
        if begin <= open.1 {
            open.1 = open.1.max(end);
        } else {
            ticks += u128::from(open.1 - open.0);
            open = (begin, end);
        }
    }
    ticks += u128::from(open.1 - open.0);
    let ns = (ticks as f64 * period_ns).round();
    // u64::MAX rounds up to 2^64 as f64; keep that boundary exclusive.
    (period_ns.is_finite() && period_ns > 0.0 && ns.is_finite() && ns < u64::MAX as f64)
        .then_some(ns as u64)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

    use super::{Clock, MAP_FAILED, MAP_OK, PASS_PAIRS, SLOTS, TimestampRing, busy_ns};
    use crate::gpu_time::{FrameRing, GpuFrameTime, Settled};

    fn pairs(spans: &[(u64, u64)]) -> Vec<u8> {
        spans
            .iter()
            .flat_map(|&(b, e)| b.to_le_bytes().into_iter().chain(e.to_le_bytes()))
            .collect()
    }

    /// A device the tests run by hand: its work completes when a test says
    /// (`finish_work`), each readback map settles when a test says, and a read
    /// returns the ticks a test planted.
    #[derive(Default)]
    struct FakeDevice {
        /// Every frame's completion flag, in the order the ring asked.
        done: RefCell<Vec<Arc<AtomicBool>>>,
        /// Every readback map started, in order.
        maps: RefCell<Vec<Arc<AtomicU8>>>,
        /// Resolves encoded while a frame's work was still running.
        early: RefCell<u32>,
        /// What a read hands back, one pair per pass.
        ticks: RefCell<Vec<(u64, u64)>>,
    }

    impl FakeDevice {
        /// The device has run everything submitted so far.
        fn finish_work(&self) {
            for done in self.done.borrow().iter() {
                done.store(true, Ordering::Release);
            }
        }

        /// Settle readback map `index` with `report`.
        fn settle_map(&self, index: usize, report: u8) {
            self.maps.borrow()[index].store(report, Ordering::Release);
        }
    }

    impl Clock for FakeDevice {
        type Slot = ();
        fn poll(&self) {}
        fn on_done(&self, done: Arc<AtomicBool>) {
            self.done.borrow_mut().push(done);
        }
        fn resolve(&self, _: &(), _: u32, report: Arc<AtomicU8>) {
            if self
                .done
                .borrow()
                .iter()
                .any(|d| !d.load(Ordering::Acquire))
            {
                *self.early.borrow_mut() += 1;
            }
            self.maps.borrow_mut().push(report);
        }
        fn read(&self, _: &(), n: u32, read: &mut dyn FnMut(&[u8])) {
            read(&pairs(
                &self.ticks.borrow()[..usize::try_from(n).unwrap_or(0)],
            ));
        }
    }

    fn ring(frames: &FrameRing) -> TimestampRing<'_, ()> {
        TimestampRing::with_slots(frames, vec![(); SLOTS], 1.0)
    }

    /// Open a frame, give `passes` passes their pairs, and finish it.
    fn time_frame(ring: &mut TimestampRing<'_, ()>, dev: &FakeDevice, passes: u32) -> u64 {
        let serial = ring.begin(dev).expect("a listening ring opens frames");
        for _ in 0..passes {
            let _ = ring.next_pass();
        }
        ring.finish(dev);
        serial
    }

    /// A healthy frame settles only after its work ran, its readback was
    /// resolved after that, and the map landed: one part per timed pass, its
    /// time the union of their intervals.
    #[test]
    fn a_healthy_frame_is_timed_once_its_work_has_run() {
        let frames = FrameRing::listening();
        let dev = FakeDevice::default();
        let mut ring = ring(&frames);
        let serial = time_frame(&mut ring, &dev, 3);
        ring.harvest(&dev);
        assert!(
            dev.maps.borrow().is_empty(),
            "nothing is resolved while the frame's work may still be running"
        );
        assert_eq!(frames.settled(serial), None);
        dev.finish_work();
        ring.harvest(&dev);
        assert_eq!(dev.maps.borrow().len(), 1, "the finished frame resolves");
        assert_eq!(*dev.early.borrow(), 0);
        assert_eq!(frames.settled(serial), None, "its map is still pending");
        *dev.ticks.borrow_mut() = vec![(100, 160), (140, 200), (300, 310)];
        dev.settle_map(0, MAP_OK);
        ring.harvest(&dev);
        assert_eq!(
            frames.settled(serial),
            Some(Settled::Timed(GpuFrameTime {
                serial,
                gpu_ns: 110,
                parts: 3,
            })),
            "three passes, 100-200 and 300-310 busy"
        );
    }

    /// A frame whose passes outnumber the query pairs is dropped, not reported
    /// as the passes it could time.
    #[test]
    fn a_frame_out_of_query_pairs_is_dropped() {
        let frames = FrameRing::listening();
        let dev = FakeDevice::default();
        let mut ring = ring(&frames);
        let serial = time_frame(&mut ring, &dev, PASS_PAIRS + 1);
        assert_eq!(frames.settled(serial), Some(Settled::Failed));
        assert!(dev.done.borrow().is_empty() && dev.maps.borrow().is_empty());
        let fits = time_frame(&mut ring, &dev, PASS_PAIRS);
        assert_eq!(frames.settled(fits), None, "a full budget is still timed");
    }

    /// A readback whose map fails drops its frame.
    #[test]
    fn a_failed_readback_map_drops_the_frame() {
        let frames = FrameRing::listening();
        let dev = FakeDevice::default();
        let mut ring = ring(&frames);
        let serial = time_frame(&mut ring, &dev, 2);
        dev.finish_work();
        ring.harvest(&dev);
        dev.settle_map(0, MAP_FAILED);
        ring.harvest(&dev);
        assert_eq!(frames.settled(serial), Some(Settled::Failed));
    }

    /// A readback holding an unsampled (zero) or reversed pair drops its
    /// frame.
    #[test]
    fn a_readback_with_an_unwritten_sample_drops_the_frame() {
        for planted in [vec![(100, 160), (170, 0)], vec![(100, 160), (240, 200)]] {
            let frames = FrameRing::listening();
            let dev = FakeDevice::default();
            let mut ring = ring(&frames);
            let serial = time_frame(&mut ring, &dev, 2);
            dev.finish_work();
            ring.harvest(&dev);
            *dev.ticks.borrow_mut() = planted;
            dev.settle_map(0, MAP_OK);
            ring.harvest(&dev);
            assert_eq!(frames.settled(serial), Some(Settled::Failed));
        }
    }

    /// A timer dropped with a frame still running on the device, or still
    /// mapping, fails it rather than leaving it outstanding.
    #[test]
    fn a_timer_dropped_with_a_frame_outstanding_drops_it() {
        let frames = FrameRing::listening();
        let dev = FakeDevice::default();
        let mut ring = ring(&frames);
        let running = time_frame(&mut ring, &dev, 2);
        drop(ring);
        assert_eq!(frames.settled(running), Some(Settled::Failed));

        let mut ring = self::ring(&frames);
        let mapping = time_frame(&mut ring, &dev, 2);
        dev.finish_work();
        ring.harvest(&dev);
        assert_eq!(dev.maps.borrow().len(), 1, "the frame is mapping");
        drop(ring);
        assert_eq!(frames.settled(mapping), Some(Settled::Failed));
    }

    /// A present with no timed pass is untimed, and one that finds every slot
    /// in flight goes untimed without disturbing them.
    #[test]
    fn an_untimed_present_and_a_full_ring_report_no_time() {
        let frames = FrameRing::listening();
        let dev = FakeDevice::default();
        let mut ring = ring(&frames);
        let none = time_frame(&mut ring, &dev, 0);
        assert_eq!(frames.settled(none), Some(Settled::Untimed));
        let held: Vec<u64> = (0..SLOTS).map(|_| time_frame(&mut ring, &dev, 1)).collect();
        assert_eq!(ring.begin(&dev), None, "every slot is in flight");
        dev.finish_work();
        ring.harvest(&dev);
        *dev.ticks.borrow_mut() = vec![(10, 15)];
        for i in 0..SLOTS {
            dev.settle_map(i, MAP_OK);
        }
        ring.harvest(&dev);
        for serial in held {
            assert!(matches!(
                frames.settled(serial),
                Some(Settled::Timed(t)) if t.gpu_ns == 5 && t.parts == 1
            ));
        }
    }

    /// Overlapping passes count once; disjoint ones add; a zero or reversed
    /// pair, no pair at all, or a period or total outside the nanosecond
    /// domain is no time.
    #[test]
    fn busy_time_is_the_union_of_the_pass_intervals() {
        let busy = |p: &[(u64, u64)], period| busy_ns(&pairs(p), period);
        assert_eq!(busy(&[(100, 160), (200, 240)], 1.0), Some(100));
        assert_eq!(busy(&[(100, 160), (140, 200)], 1.0), Some(100));
        assert_eq!(busy(&[(140, 200), (100, 160)], 1.0), Some(100), "any order");
        assert_eq!(busy(&[(100, 300), (150, 160)], 1.0), Some(200), "nested");
        assert_eq!(busy(&[(100, 160), (160, 170)], 1.0), Some(70), "abutting");
        assert_eq!(busy(&[(10, 20)], 41.666), Some(417));
        assert_eq!(busy(&[(100, 160), (240, 200)], 1.0), None);
        assert_eq!(busy(&[(100, 160), (170, 0)], 1.0), None);
        assert_eq!(busy(&[(0, 0)], 1.0), None);
        assert_eq!(busy(&[], 1.0), None);
        for period in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert_eq!(busy(&[(1, 3)], period), None);
        }
        assert_eq!(busy(&[(1, u64::MAX)], 1.0), None);
        assert_eq!(busy(&[(1, 2)], (1_u64 << 63) as f64), Some(1_u64 << 63));
        let too_many = vec![(1, 2); PASS_PAIRS as usize + 1];
        assert_eq!(busy(&too_many, 1.0), None);
    }
}
