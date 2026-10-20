// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wgpu arm's GPU clock for `crate::gpu_time`: per-pass timestamp queries.
//!
//! wgpu has no command-buffer clock, so a timed present asks each of its render
//! passes for a beginning and an end timestamp (`RenderPassTimestampWrites`,
//! which needs only `Features::TIMESTAMP_QUERY`; writes between passes would
//! need `TIMESTAMP_QUERY_INSIDE_ENCODERS`, which fewer adapters carry). When the
//! present returns, the used queries are resolved, copied to a mappable buffer
//! and mapped asynchronously; a later present (or [`WgpuGpuTimer::harvest`])
//! finds the map complete and reports each pass span, in ticks times
//! `Queue::get_timestamp_period`, as one part of the frame, so a frame's part
//! count is the number of passes it timed. Copies between passes (the present
//! copy, the band shift) carry no timestamp and are not counted.
//!
//! Nothing here runs unless a `gpu_time` sink is installed and the device was
//! granted `TIMESTAMP_QUERY` (`terminal_device_features`): the renderer builds
//! the timer lazily, and a pass asks [`WgpuGpuTimer::pass_writes`] only while a
//! frame is open. A frame that runs out of query pairs, or whose readback map
//! fails, is completed as a failure so `gpu_time` drops it rather than report
//! the passes it could time.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::gpu_time::FrameToken;

/// Timed passes per frame: the offscreen plan's passes, the effect passes
/// (bloom extract and composite, shimmer, card) and the blit, with headroom.
const PASS_PAIRS: u32 = 32;
/// Frames whose readback may be in flight at once; a present that finds every
/// slot busy goes untimed (`gpu_time::skipped`).
const SLOTS: usize = 4;

/// The map callback's report.
const MAP_PENDING: u8 = 0;
const MAP_OK: u8 = 1;
const MAP_FAILED: u8 = 2;

/// Where one slot is in its cycle.
enum SlotState {
    /// Ready to time a frame.
    Free,
    /// Timing the open frame: passes take pairs from `next`.
    Writing(FrameToken<'static>),
    /// The readback of `pairs` pass spans is mapping; `map` is the callback's
    /// report.
    Mapping {
        token: FrameToken<'static>,
        pairs: u32,
        map: Arc<AtomicU8>,
    },
}

struct Slot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    state: SlotState,
}

/// The per-renderer timestamp ring (see the module header).
pub(crate) struct WgpuGpuTimer {
    slots: Vec<Slot>,
    /// Nanoseconds per timestamp tick.
    period_ns: f64,
    /// The slot timing the open frame, if any.
    writing: Option<usize>,
    /// Query pairs handed out to the open frame's passes.
    next_pair: Cell<u32>,
    /// A pass of the open frame asked for a pair past [`PASS_PAIRS`].
    overflowed: Cell<bool>,
}

impl WgpuGpuTimer {
    /// The timer for `device`, or `None` when the device was not granted
    /// timestamp queries.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let bytes = u64::from(PASS_PAIRS * 2 * wgpu::QUERY_SIZE);
        let slots = (0..SLOTS)
            .map(|_| Slot {
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
                state: SlotState::Free,
            })
            .collect();
        Some(Self {
            slots,
            period_ns: f64::from(queue.get_timestamp_period()),
            writing: None,
            next_pair: Cell::new(0),
            overflowed: Cell::new(false),
        })
    }

    /// Open a timed frame for the present about to encode: harvest finished
    /// readbacks, then take a free slot. Returns the `gpu_time` serial, or
    /// `None` when no sink listens or every slot is still mapping.
    pub(crate) fn begin(&mut self, device: &wgpu::Device) -> Option<u64> {
        self.abandon_open();
        self.harvest(device);
        let free = self
            .slots
            .iter()
            .position(|s| matches!(s.state, SlotState::Free));
        let Some(index) = free else {
            crate::gpu_time::note_skipped();
            return None;
        };
        let token = crate::gpu_time::open_frame()?;
        self.slots[index].state = SlotState::Writing(token);
        self.writing = Some(index);
        self.next_pair.set(0);
        self.overflowed.set(false);
        Some(token.serial())
    }

    /// The timestamp writes for the next pass of the open frame, or `None`
    /// when no frame is open. A pass past the per-frame budget gets none and
    /// marks the frame overflowed (it will be dropped, not under-reported).
    pub(crate) fn pass_writes(&self) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let slot = &self.slots[self.writing?];
        let pair = self.next_pair.get();
        if pair >= PASS_PAIRS {
            self.overflowed.set(true);
            return None;
        }
        self.next_pair.set(pair + 1);
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &slot.queries,
            beginning_of_pass_write_index: Some(pair * 2),
            end_of_pass_write_index: Some(pair * 2 + 1),
        })
    }

    /// The present returned: resolve the open frame's queries into its
    /// readback, start the map, and seal the frame. Each timed pass is held
    /// as one part until a later [`Self::harvest`] completes it.
    pub(crate) fn finish(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let Some(index) = self.writing else {
            return;
        };
        let slot = &mut self.slots[index];
        let SlotState::Writing(token) = slot.state else {
            unreachable!("the writing slot is in the Writing state");
        };
        let pairs = self.next_pair.get();
        if self.overflowed.get() {
            token.hold();
            token.complete(None);
            token.seal();
            slot.state = SlotState::Free;
            self.writing = None;
            return;
        }
        if pairs == 0 {
            token.seal();
            slot.state = SlotState::Free;
            self.writing = None;
            return;
        }
        let bytes = u64::from(pairs * 2 * wgpu::QUERY_SIZE);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aterm-gpu gpu-time resolve"),
        });
        enc.resolve_query_set(&slot.queries, 0..pairs * 2, &slot.resolve, 0);
        enc.copy_buffer_to_buffer(&slot.resolve, 0, &slot.read, 0, bytes);
        queue.submit([enc.finish()]);
        let map = Arc::new(AtomicU8::new(MAP_PENDING));
        let report = Arc::clone(&map);
        slot.read
            .map_async(wgpu::MapMode::Read, 0..bytes, move |r| {
                report.store(
                    if r.is_ok() { MAP_OK } else { MAP_FAILED },
                    Ordering::Release,
                );
            });
        for _ in 0..pairs {
            token.hold();
        }
        token.seal();
        slot.state = SlotState::Mapping { token, pairs, map };
        self.writing = None;
    }

    /// Complete every frame whose readback has mapped (a non-blocking device
    /// poll first). Called at each timed present's start; a reader that
    /// needs a result without presenting again calls it directly.
    pub(crate) fn harvest(&mut self, device: &wgpu::Device) {
        let _ = device.poll(wgpu::PollType::Poll);
        let period_ns = self.period_ns;
        for slot in &mut self.slots {
            let SlotState::Mapping { token, pairs, map } = &slot.state else {
                continue;
            };
            let (token, pairs) = (*token, *pairs);
            match map.load(Ordering::Acquire) {
                MAP_PENDING => continue,
                MAP_OK => {
                    let bytes = u64::from(pairs * 2 * wgpu::QUERY_SIZE);
                    {
                        let view = slot.read.get_mapped_range(0..bytes);
                        for span in pass_spans(&view, period_ns) {
                            token.complete(span);
                        }
                    }
                    slot.read.unmap();
                }
                _ => {
                    for _ in 0..pairs {
                        token.complete(None);
                    }
                }
            }
            slot.state = SlotState::Free;
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

impl Drop for WgpuGpuTimer {
    fn drop(&mut self) {
        for slot in &mut self.slots {
            match std::mem::replace(&mut slot.state, SlotState::Free) {
                SlotState::Writing(token) => token.seal(),
                SlotState::Mapping { token, pairs, .. } => {
                    for _ in 0..pairs {
                        token.complete(None);
                    }
                }
                SlotState::Free => {}
            }
        }
    }
}

/// Each pass's `end - begin` from the resolved pairs in `bytes` (little-endian
/// `u64` ticks, begin then end per pass) in nanoseconds; `None` for a pair
/// that runs backwards (a reset or garbage clock), which drops the frame.
pub(crate) fn pass_spans(bytes: &[u8], period_ns: f64) -> impl Iterator<Item = Option<u64>> + '_ {
    bytes.as_chunks::<16>().0.iter().map(move |pair| {
        let begin = u64::from_le_bytes(pair[..8].try_into().ok()?);
        let end = u64::from_le_bytes(pair[8..].try_into().ok()?);
        let ns = (end.checked_sub(begin)? as f64 * period_ns).round();
        // u64::MAX rounds up to 2^64 as f64; keep that boundary exclusive.
        (period_ns.is_finite()
            && period_ns > 0.0
            && ns.is_finite()
            && ns >= 0.0
            && ns < u64::MAX as f64)
            .then_some(ns as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::pass_spans;

    fn pairs(spans: &[(u64, u64)]) -> Vec<u8> {
        spans
            .iter()
            .flat_map(|&(b, e)| b.to_le_bytes().into_iter().chain(e.to_le_bytes()))
            .collect()
    }

    /// One span per pass, scaled by the tick period; a backwards pair is no
    /// span (the frame is then dropped), never a wrapped huge one.
    #[test]
    fn pass_spans_scale_ticks_and_a_backwards_pair_is_no_span() {
        let spans = |p: &[(u64, u64)], period| pass_spans(&pairs(p), period).collect::<Vec<_>>();
        assert_eq!(spans(&[(100, 160), (200, 240)], 1.0), [Some(60), Some(40)]);
        assert_eq!(spans(&[(10, 20)], 41.666), [Some(417)]);
        assert_eq!(spans(&[(100, 160), (240, 200)], 1.0), [Some(60), None]);
        assert!(spans(&[], 1.0).is_empty());
        for period in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert_eq!(spans(&[(0, 2)], period), [None]);
        }
        assert_eq!(spans(&[(0, u64::MAX)], 1.0), [None]);
        assert_eq!(spans(&[(0, 1)], (1_u64 << 63) as f64), [Some(1_u64 << 63)]);
    }
}
