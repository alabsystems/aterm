// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! VIDEO introspection capture core: record the ACTUALLY-PRESENTED swapchain
//! frames, timestamped, so an AI driving aterm can *see* the true temporal
//! rendering (cursor-trail smoothness, per-keystroke flashes, real key→photon
//! latency) instead of relying on single-frame captures.
//!
//! WHY THE SWAPCHAIN and not the offscreen: the offscreen readbacks (`image`
//! verb, `present_input_readback`) are byte-exact for the GRID but structurally
//! miss the swapchain-only passes — the EDR aurora, the SDR crown, the bell
//! invert / drop-overlay / letterbox chrome baked into the blit. This tap copies
//! `frame.texture` itself, in the SAME encoder as the present pass, after the
//! last swapchain draw — the exact bytes handed to `present()`. It cannot lie.
//!
//! Discipline (the counted-drop recorder shape shared with the cast/temporal
//! recorders): a small ring of staging buffers absorbs GPU latency; when the
//! ring is saturated the frame is SKIPPED and counted, never blocked on; the
//! harvested store is byte-budgeted drop-oldest with a counted drop. The
//! present path pays one `Option` branch when the tap is off.

use std::collections::VecDeque;
use std::sync::mpsc;

/// One harvested frame: tightly-packed RGBA8, plus the same-clock timestamp the
/// GUI stamped right after `present()` returned (µs, `metrics::now_us` epoch).
pub struct CapturedFrame {
    pub seq: u64,
    pub t_us: u64,
    pub w: u32,
    pub h: u32,
    /// Tightly-packed RGBA8 (already swizzled/downscaled at harvest).
    pub rgba: Vec<u8>,
}

/// The finalized recording handed to the dump path.
pub struct VideoTake {
    pub frames: VecDeque<CapturedFrame>,
    /// Frames LOST mid-stream (staging ring saturated / map failure / device
    /// loss), counted so the artifact is honest about coverage. Budget
    /// evictions are counted separately in `evicted` (they truncate the HEAD
    /// of the recording, a different honesty story than a mid-stream skip).
    pub dropped: u64,
    /// Frames evicted from the FRONT of the store when the byte budget
    /// overflowed: `evicted > 0` means the head of the recording is gone
    /// (`head_truncated` in the dump's index.json).
    pub evicted: u64,
    /// Frames deliberately skipped by the `fps=` capture gate. NOT a loss:
    /// decimation is the client's explicit request, so it is never folded
    /// into `dropped`.
    pub decimated: u64,
    /// The fps cap the gate ran with (`None` = capture every present).
    pub fps_cap: Option<u32>,
    /// The byte budget the store ran with (echoed as `budget_mib`).
    pub budget_bytes: usize,
    /// The client's REQUESTED duration (ms), echoed into index.json so the
    /// consumer can judge the covered window against what was asked for.
    pub requested_ms: u64,
    /// Capture geometry actually used (post-downscale).
    pub w: u32,
    pub h: u32,
    /// Device pixels of the source swapchain.
    pub device_px: (u32, u32),
    /// True when the recording downscaled 2×2.
    pub half_res: bool,
    /// The swapchain format captured (`Bgra8Unorm`, `Rgba8Unorm`, or
    /// `Rgba16Float` decoded to RGBA8 at harvest).
    pub format: &'static str,
    /// True when a mid-capture resize forced an early finalize.
    pub resized_early_stop: bool,
}

enum SlotState {
    Free,
    /// Copy enqueued this present; awaiting the post-present stamp + map_async.
    Pending {
        seq: u64,
    },
    /// map_async issued; waiting for the completion callback.
    InFlight {
        seq: u64,
        t_us: u64,
    },
}

struct Slot {
    buffer: wgpu::Buffer,
    state: SlotState,
}

/// Per-window recording state. Owned by the window's GPU state; `None` = off.
pub struct VideoTap {
    slots: Vec<Slot>,
    /// Map-completion channel: the map_async callback sends its slot index.
    done_tx: mpsc::Sender<(usize, bool)>,
    done_rx: mpsc::Receiver<(usize, bool)>,
    store: VecDeque<CapturedFrame>,
    store_bytes: usize,
    budget_bytes: usize,
    /// Mid-stream losses (ring saturation / map failure / device loss).
    dropped: u64,
    /// Head truncations (budget drop-oldest evictions).
    evicted: u64,
    /// `fps=` gate skips (deliberate, client-requested — not losses).
    decimated: u64,
    /// `fps=` capture cap; `None` captures every present.
    fps_cap: Option<u32>,
    /// The client's requested duration (ms) — carried for the dump's honesty
    /// report only; the GUI owns the actual stop deadline.
    requested_ms: u64,
    /// Earliest gate time (µs on the `epoch` clock) the next frame is accepted.
    next_capture_us: u64,
    /// Local monotonic origin for the fps gate. The gate only ever compares
    /// spacing on this one clock, so it needs no shared epoch with the
    /// `metrics::now_us` frame stamps.
    // web-time so the type/clock matches the renderer's wasm-safe present clocks;
    // native builds get std's Instant unchanged (video tap is a native feature).
    epoch: web_time::Instant,
    seq: u64,
    half_res: bool,
    /// Source swapchain geometry/format the ring was built for; a mismatch at
    /// enqueue time (resize / format flip) finalizes the recording honestly.
    src_w: u32,
    src_h: u32,
    format: wgpu::TextureFormat,
    bpp: u32,
    resized_early_stop: bool,
}

/// Staging-ring depth: enough to absorb a GPU running a frame or two behind
/// without ever blocking the present path.
const RING: usize = 4;
/// Default RAM budget for the harvested store (bytes).
pub const DEFAULT_BUDGET: usize = 512 * 1024 * 1024;

/// The CLIENT-SET controls for one recording (the `video` verb's knobs),
/// bundled so they travel the `Wake::Video -> video_begin -> VideoTap` seam as
/// one value. Geometry/format stay separate `new()` parameters — they come
/// from the swapchain, not the client.
#[derive(Clone, Copy, Debug)]
pub struct CaptureOpts {
    /// Downscale 2×2 at harvest (quarter the bytes — the default for
    /// multi-second runs).
    pub half_res: bool,
    /// Byte budget for the harvested store (floored at 16 MiB).
    pub budget_bytes: usize,
    /// `fps=` capture cap (clamped 1..=120); `None` captures every present.
    pub fps_cap: Option<u32>,
    /// The client's requested duration (ms), carried verbatim into the take's
    /// honesty report (`requested_ms` in index.json).
    pub requested_ms: u64,
}

impl VideoTap {
    /// Build a tap for the CURRENT swapchain size/format with the client's
    /// [`CaptureOpts`].
    pub fn new(
        device: &wgpu::Device,
        src_w: u32,
        src_h: u32,
        format: wgpu::TextureFormat,
        opts: CaptureOpts,
    ) -> Result<Self, String> {
        let bpp: u32 = match format {
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm => 4,
            wgpu::TextureFormat::Rgba16Float => 8,
            other => return Err(format!("video: unsupported swapchain format {other:?}")),
        };
        // COPY_BYTES_PER_ROW_ALIGNMENT (256) padding, computed on BYTES so it is
        // correct for both the 4-byte SDR and 8-byte f16 formats.
        let row_bytes = (src_w as u64) * bpp as u64;
        let padded = row_bytes.div_ceil(256) * 256;
        let size = padded * src_h as u64;
        let (done_tx, done_rx) = mpsc::channel();
        let slots = (0..RING)
            .map(|i| Slot {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("aterm-video staging {i}")),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                state: SlotState::Free,
            })
            .collect();
        Ok(Self {
            slots,
            done_tx,
            done_rx,
            store: VecDeque::new(),
            store_bytes: 0,
            budget_bytes: opts.budget_bytes.max(16 * 1024 * 1024),
            dropped: 0,
            evicted: 0,
            decimated: 0,
            fps_cap: opts.fps_cap.map(|f| f.clamp(1, 120)),
            requested_ms: opts.requested_ms,
            next_capture_us: 0,
            epoch: web_time::Instant::now(),
            seq: 0,
            half_res: opts.half_res,
            src_w,
            src_h,
            format,
            bpp,
            resized_early_stop: false,
        })
    }

    fn padded_row(&self) -> u64 {
        ((self.src_w as u64) * self.bpp as u64).div_ceil(256) * 256
    }

    /// Enqueue the swapchain copy into THIS present's encoder (called between
    /// the render-pass close and `queue.submit`). Never blocks: a saturated
    /// ring or a mismatched size counts a drop / finalizes instead.
    pub fn enqueue_copy(&mut self, enc: &mut wgpu::CommandEncoder, frame_tex: &wgpu::Texture) {
        if self.resized_early_stop {
            return;
        }
        if frame_tex.width() != self.src_w
            || frame_tex.height() != self.src_h
            || frame_tex.format() != self.format
        {
            // Mid-capture resize/format flip: finalize honestly (the ring was
            // sized for the old geometry) rather than chase it.
            self.resized_early_stop = true;
            return;
        }
        // `fps=` decimation gate, BEFORE any copy work (a skipped frame costs
        // one compare). Counted in `decimated`, never in `dropped` — skipping
        // is the client's explicit request, not a loss.
        let now_us = self.epoch.elapsed().as_micros() as u64;
        if !self.should_capture(now_us) {
            return;
        }
        let Some(idx) = self
            .slots
            .iter()
            .position(|s| matches!(s.state, SlotState::Free))
        else {
            self.dropped += 1; // GPU behind: skip, never block.
            return;
        };
        let padded = self.padded_row();
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: frame_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.slots[idx].buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(self.src_h),
                },
            },
            wgpu::Extent3d {
                width: self.src_w,
                height: self.src_h,
                depth_or_array_layers: 1,
            },
        );
        self.seq += 1;
        self.slots[idx].state = SlotState::Pending { seq: self.seq };
    }

    /// The `fps=` capture gate, PURE on `(self, now_us)` — no clock reads, so
    /// the law is unit-testable with synthetic times. Semantics (pinned by
    /// tests): with no cap every frame is accepted; with cap `n` a frame at
    /// `now_us` is accepted iff at least `1_000_000 / n` µs have passed since
    /// the last ACCEPTED frame (spacing is measured accept-to-accept — a
    /// rejected frame never accrues catch-up debt, so a burst after a quiet
    /// stretch still captures at most one frame per interval). The first frame
    /// is always accepted. A rejection increments `decimated`.
    fn should_capture(&mut self, now_us: u64) -> bool {
        let Some(fps) = self.fps_cap else {
            return true;
        };
        if now_us < self.next_capture_us {
            self.decimated += 1;
            return false;
        }
        self.next_capture_us = now_us + 1_000_000 / u64::from(fps);
        true
    }

    /// Post-present hook (GUI-side, right after `frame.present()` returned):
    /// stamp the just-enqueued copy with the same-clock time, issue its
    /// map_async, then NON-BLOCKING poll + harvest any completed earlier maps.
    /// Bounded work; never waits on the GPU.
    pub fn after_present(&mut self, device: &wgpu::Device, t_us: u64) {
        // Stamp + map the newest Pending slot (at most one per present).
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let SlotState::Pending { seq } = slot.state {
                let tx = self.done_tx.clone();
                slot.buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| {
                        let _ = tx.send((i, r.is_ok()));
                    });
                slot.state = SlotState::InFlight { seq, t_us };
            }
        }
        // Non-blocking poll: advances map completions without waiting.
        let _ = device.poll(wgpu::PollType::Poll);
        self.harvest_ready();
    }

    /// Drain the completion channel and copy out every mapped slot.
    fn harvest_ready(&mut self) {
        while let Ok((idx, ok)) = self.done_rx.try_recv() {
            let (seq, t_us) = match self.slots[idx].state {
                SlotState::InFlight { seq, t_us } => (seq, t_us),
                _ => continue, // stale/duplicate completion
            };
            if ok {
                let frame = self.copy_out(idx, seq, t_us);
                self.push_frame(frame);
            } else {
                self.dropped += 1;
            }
            self.slots[idx].buffer.unmap();
            self.slots[idx].state = SlotState::Free;
        }
    }

    /// Strip row padding + swizzle/decode + (optionally) 2×2 box-downscale one
    /// mapped slot into a tightly-packed RGBA8 frame.
    fn copy_out(&self, idx: usize, seq: u64, t_us: u64) -> CapturedFrame {
        let padded = self.padded_row() as usize;
        let data = self.slots[idx].buffer.slice(..).get_mapped_range();
        let (sw, sh) = (self.src_w as usize, self.src_h as usize);
        let (dw, dh) = if self.half_res {
            (sw / 2, sh / 2)
        } else {
            (sw, sh)
        };
        let mut rgba = vec![0u8; dw * dh * 4];
        let is_f16 = self.format == wgpu::TextureFormat::Rgba16Float;
        let bgra = self.format == wgpu::TextureFormat::Bgra8Unorm;
        // Fetch one source pixel as RGBA8.
        let fetch = |x: usize, y: usize| -> [u32; 4] {
            let base = y * padded + x * self.bpp as usize;
            if is_f16 {
                let ch = |o: usize| {
                    let bits = u16::from_le_bytes([data[base + o * 2], data[base + o * 2 + 1]]);
                    let lin = f16_to_f32(bits).clamp(0.0, 1.0);
                    (linear_to_srgb(lin) * 255.0 + 0.5) as u32
                };
                [ch(0), ch(1), ch(2), 255]
            } else if bgra {
                [
                    data[base + 2] as u32,
                    data[base + 1] as u32,
                    data[base] as u32,
                    255,
                ]
            } else {
                [
                    data[base] as u32,
                    data[base + 1] as u32,
                    data[base + 2] as u32,
                    255,
                ]
            }
        };
        for dy in 0..dh {
            for dx in 0..dw {
                let px = if self.half_res {
                    let (x, y) = (dx * 2, dy * 2);
                    let (a, b, c, d) = (
                        fetch(x, y),
                        fetch((x + 1).min(sw - 1), y),
                        fetch(x, (y + 1).min(sh - 1)),
                        fetch((x + 1).min(sw - 1), (y + 1).min(sh - 1)),
                    );
                    [
                        (a[0] + b[0] + c[0] + d[0]) / 4,
                        (a[1] + b[1] + c[1] + d[1]) / 4,
                        (a[2] + b[2] + c[2] + d[2]) / 4,
                        255,
                    ]
                } else {
                    fetch(dx, dy)
                };
                let o = (dy * dw + dx) * 4;
                rgba[o] = px[0] as u8;
                rgba[o + 1] = px[1] as u8;
                rgba[o + 2] = px[2] as u8;
                rgba[o + 3] = 255;
            }
        }
        drop(data);
        CapturedFrame {
            seq,
            t_us,
            w: dw as u32,
            h: dh as u32,
            rgba,
        }
    }

    /// Budget-bounded drop-oldest push (the cast/temporal recorder discipline).
    /// An eviction counts in `evicted`, NOT `dropped`: it truncates the HEAD of
    /// the recording (the consumer's `head_truncated` signal), while `dropped`
    /// means a mid-stream skip.
    fn push_frame(&mut self, f: CapturedFrame) {
        self.store_bytes += f.rgba.len();
        self.store.push_back(f);
        while self.store_bytes > self.budget_bytes {
            if let Some(old) = self.store.pop_front() {
                self.store_bytes -= old.rgba.len();
                self.evicted += 1;
            } else {
                break;
            }
        }
    }

    /// Whether a mid-capture resize already finalized the recording.
    #[must_use]
    pub fn resized(&self) -> bool {
        self.resized_early_stop
    }

    /// Frames harvested so far (for `video status`).
    #[must_use]
    pub fn frames_so_far(&self) -> usize {
        self.store.len()
    }

    /// Finalize: BLOCKING drain of in-flight maps (off the hot path by
    /// definition — the recording is over), then hand the store out.
    pub fn finish(mut self, device: &wgpu::Device) -> VideoTake {
        let any_inflight = self
            .slots
            .iter()
            .any(|s| !matches!(s.state, SlotState::Free));
        if any_inflight {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            self.harvest_ready();
            // Anything still not Free after a full wait is lost to device loss.
            for s in &mut self.slots {
                if !matches!(s.state, SlotState::Free) {
                    self.dropped += 1;
                    s.state = SlotState::Free;
                }
            }
        }
        let (dw, dh) = if self.half_res {
            (self.src_w / 2, self.src_h / 2)
        } else {
            (self.src_w, self.src_h)
        };
        VideoTake {
            frames: self.store,
            dropped: self.dropped,
            evicted: self.evicted,
            decimated: self.decimated,
            fps_cap: self.fps_cap,
            budget_bytes: self.budget_bytes,
            requested_ms: self.requested_ms,
            w: dw,
            h: dh,
            device_px: (self.src_w, self.src_h),
            half_res: self.half_res,
            format: match self.format {
                wgpu::TextureFormat::Bgra8Unorm => "bgra8",
                wgpu::TextureFormat::Rgba8Unorm => "rgba8",
                wgpu::TextureFormat::Rgba16Float => "rgba16f->srgb8",
                _ => "unknown",
            },
            resized_early_stop: self.resized_early_stop,
        }
    }
}

/// IEEE 754 half → f32 (the swapchain-capture twin of the renderer's helper;
/// small and local so this module has no renderer dependency).
fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x3ff) as u32;
    let f = match (exp, frac) {
        (0, 0) => sign << 31,
        (0, _) => {
            // subnormal: normalize
            let mut e = 127 - 15 + 1;
            let mut m = frac;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            (sign << 31) | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
        (0x1f, 0) => (sign << 31) | 0x7f80_0000,
        (0x1f, _) => (sign << 31) | 0x7fc0_0000,
        _ => (sign << 31) | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(f)
}

/// Linear → sRGB encode (standard piecewise), for the f16 scRGB capture path.
fn linear_to_srgb(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Device-free tap for the PURE store/gate laws (`push_frame`,
    /// `should_capture` touch no slot/GPU state). `budget_bytes` is taken
    /// verbatim — no 16 MiB floor — so a tiny test budget actually evicts.
    fn test_tap(budget_bytes: usize, fps_cap: Option<u32>) -> VideoTap {
        let (done_tx, done_rx) = mpsc::channel();
        VideoTap {
            slots: Vec::new(),
            done_tx,
            done_rx,
            store: VecDeque::new(),
            store_bytes: 0,
            budget_bytes,
            dropped: 0,
            evicted: 0,
            decimated: 0,
            fps_cap,
            requested_ms: 0,
            next_capture_us: 0,
            epoch: web_time::Instant::now(),
            seq: 0,
            half_res: false,
            src_w: 2,
            src_h: 2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            bpp: 4,
            resized_early_stop: false,
        }
    }

    fn synth_frame(seq: u64) -> CapturedFrame {
        CapturedFrame {
            seq,
            t_us: seq * 16_667,
            w: 2,
            h: 2,
            rgba: vec![seq as u8; 16], // 2x2 RGBA8 = 16 bytes
        }
    }

    /// PHASE-1 LAW: overflowing the byte budget counts EVICTED (head
    /// truncation), never DROPPED (mid-stream loss) — the split the honest
    /// `head_truncated` report is built on. Drop-oldest keeps the TAIL.
    #[test]
    fn budget_eviction_counts_evicted_not_dropped() {
        // Budget holds exactly 3 of the 16-byte frames; push 5.
        let mut tap = test_tap(3 * 16, None);
        for seq in 1..=5 {
            tap.push_frame(synth_frame(seq));
        }
        assert_eq!(tap.evicted, 2, "two head frames evicted");
        assert_eq!(tap.dropped, 0, "eviction is not a mid-stream drop");
        assert_eq!(tap.store.len(), 3, "store bounded by the budget");
        let kept: Vec<u64> = tap.store.iter().map(|f| f.seq).collect();
        assert_eq!(kept, vec![3, 4, 5], "drop-oldest keeps the tail");
        // head_truncated (index.json) is defined as evicted > 0.
        assert!(tap.evicted > 0, "this recording IS head-truncated");
    }

    /// PHASE-2 LAW: `should_capture` with fps=10 (interval 100ms) accepts a
    /// frame iff >= 100ms passed since the last ACCEPTED frame (accept-to-
    /// accept spacing; the first frame always accepted; a rejected frame
    /// accrues no catch-up debt). Rejections count `decimated` only.
    #[test]
    fn should_capture_fps10_spacing() {
        let mut tap = test_tap(usize::MAX, Some(10));
        assert!(tap.should_capture(0), "first frame always captured");
        assert!(!tap.should_capture(50_000), "50ms after accept: rejected");
        assert!(tap.should_capture(100_000), "100ms after accept: captured");
        assert!(!tap.should_capture(101_000), "1ms after accept: rejected");
        assert!(!tap.should_capture(199_999), "just under the interval");
        assert!(tap.should_capture(200_000), "100ms after accept: captured");
        assert_eq!(tap.decimated, 3, "each rejection counted as decimated");
        assert_eq!(tap.dropped, 0, "decimation is never a drop");
        assert_eq!(tap.evicted, 0, "decimation is never an eviction");
    }

    /// No cap = every present captured, nothing decimated.
    #[test]
    fn should_capture_uncapped_accepts_all() {
        let mut tap = test_tap(usize::MAX, None);
        for t in [0, 1, 2, 1_000] {
            assert!(tap.should_capture(t));
        }
        assert_eq!(tap.decimated, 0);
    }

    #[test]
    fn f16_roundtrip_anchors() {
        assert_eq!(f16_to_f32(0x3c00), 1.0, "1.0");
        assert_eq!(f16_to_f32(0x0000), 0.0, "0.0");
        assert!((f16_to_f32(0x3800) - 0.5).abs() < 1e-6, "0.5");
        assert!(
            (f16_to_f32(0x4000) - 2.0).abs() < 1e-6,
            "2.0 (EDR overrange)"
        );
    }

    #[test]
    fn srgb_encode_anchors() {
        assert_eq!(linear_to_srgb(0.0), 0.0);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-6);
        // mid-gray: linear 0.2159 ≈ sRGB 0.5 (well-known anchor)
        assert!((linear_to_srgb(0.2159) - 0.5).abs() < 0.01);
    }
}
