// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Video capture core.

use std::collections::VecDeque;
use std::sync::LazyLock;
#[cfg(any(wgpu_arm, not(target_arch = "wasm32")))]
use std::sync::mpsc;

use aterm_render::{linear_to_srgb, srgb8_to_linear};

/// The colour space the platform compositor uses for a window's presented texture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureColorSpace {
    /// The platform tag could not be established.
    Unknown,
    /// Non-linear sRGB coordinates (the default window and every virtual target).
    #[default]
    Srgb,
    /// Non-linear Display-P3 coordinates (the legacy wide-gamut macOS tag).
    DisplayP3,
    /// Linear sRGB/scRGB coordinates with values above `1.0` for EDR highlights.
    ExtendedLinearSrgb,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CaptureEncoding {
    color_space: CaptureColorSpace,
    /// Windows scRGB reference-white multiplier (`SDR white nits / 80`).
    sdr_white_scale: f32,
}

impl CaptureEncoding {
    fn new(
        format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Result<Self, String> {
        if color_space == CaptureColorSpace::Unknown {
            return Err("presented-frame colour space is unknown".to_string());
        }
        let format_is_f16 = format == crate::device_layer::TexelFormat::Rgba16Float;
        let space_is_linear = color_space == CaptureColorSpace::ExtendedLinearSrgb;
        if format_is_f16 != space_is_linear {
            return Err(format!(
                "presented-frame format/colour-space mismatch: {format:?} with {color_space:?}"
            ));
        }
        let sdr_white_scale = if space_is_linear {
            if !sdr_white_scale.is_finite() || sdr_white_scale < 1.0 {
                return Err(format!(
                    "presented-frame scRGB SDR-white scale must be finite and >= 1, got {sdr_white_scale}"
                ));
            }
            sdr_white_scale
        } else {
            1.0
        };
        Ok(Self {
            color_space,
            sdr_white_scale,
        })
    }

    #[inline]
    const fn format_label(self, format: crate::device_layer::TexelFormat) -> &'static str {
        match (format, self.color_space) {
            (crate::device_layer::TexelFormat::Bgra8Unorm, CaptureColorSpace::Srgb) => "bgra8",
            (crate::device_layer::TexelFormat::Rgba8Unorm, CaptureColorSpace::Srgb) => "rgba8",
            (crate::device_layer::TexelFormat::Bgra8Unorm, CaptureColorSpace::DisplayP3) => {
                "bgra8-display-p3->srgb8"
            }
            (crate::device_layer::TexelFormat::Rgba8Unorm, CaptureColorSpace::DisplayP3) => {
                "rgba8-display-p3->srgb8"
            }
            (
                crate::device_layer::TexelFormat::Rgba16Float,
                CaptureColorSpace::ExtendedLinearSrgb,
            ) => "rgba16f-scrgb->srgb8-tonemapped",
            _ => "unknown",
        }
    }
}

/// One harvested frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedFrame {
    pub seq: u64,
    pub t_us: u64,
    pub w: u32,
    pub h: u32,
    /// Tightly-packed RGBA8 (already swizzled/downscaled at harvest).
    pub rgba: Vec<u8>,
}

/// Insert one harvested frame in capture-sequence order and enforce the byte budget by evicting the lowest sequences.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "HarvestThree",
        project = "aterm_gpu::video_tap::ordered_capture_store_push"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "HarvestOne",
        project = "aterm_gpu::video_tap::ordered_capture_store_push"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "HarvestTwo",
        project = "aterm_gpu::video_tap::ordered_capture_store_push"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VideoTapSlot",
        action = "BuggyHarvestCallbackOrder",
        reason = "Buggy=1 negative control only; the push inserts by capture sequence, never at \
                  the callback-order tail"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VideoTapSlot",
        action = "BuggyEvictNewest",
        reason = "Buggy=1 negative control only; budget eviction pops the lowest sequence from \
                  the front, keeping the newest tail"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VideoTapSlot",
        action = "BuggyEvictionUnreported",
        reason = "Buggy=1 negative control only; every eviction increments the returned count \
                  that becomes the take's head_truncated"
    )
)]
pub fn ordered_capture_store_push(
    store: &mut VecDeque<CapturedFrame>,
    store_bytes: &mut usize,
    budget_bytes: usize,
    frame: CapturedFrame,
) -> u64 {
    debug_assert!(
        store.iter().all(|existing| existing.seq != frame.seq),
        "capture sequence numbers are unique"
    );
    *store_bytes += frame.rgba.len();
    let insert_at = store
        .iter()
        .position(|existing| existing.seq > frame.seq)
        .unwrap_or(store.len());
    store.insert(insert_at, frame);

    let mut evicted = 0u64;
    while *store_bytes > budget_bytes {
        let Some(oldest) = store.pop_front() else {
            break;
        };
        *store_bytes -= oldest.rgba.len();
        evicted += 1;
    }
    evicted
}

/// The finalized recording handed to the dump path.
pub struct VideoTake {
    pub frames: VecDeque<CapturedFrame>,
    /// Frames lost mid-stream (ring saturated, map failure, conversion backlog or failure, device loss).
    pub dropped: u64,
    /// Frames evicted from the front when the byte budget overflowed.
    pub evicted: u64,
    /// Frames skipped by the `fps=` capture gate at the client's request.
    pub decimated: u64,
    /// The fps cap the gate ran with (`None` = capture every present).
    pub fps_cap: Option<u32>,
    /// The byte budget the store ran with (echoed as `budget_mib`).
    pub budget_bytes: usize,
    /// The client's requested duration (ms), echoed into index.json so the consumer can judge the covered window against what was asked for.
    pub requested_ms: u64,
    /// Capture geometry actually used (post-downscale).
    pub w: u32,
    pub h: u32,
    /// Device pixels of the source swapchain.
    pub device_px: (u32, u32),
    /// True when the recording downscaled 2×2.
    pub half_res: bool,
    /// The source-encoding label (`bgra8`, `rgba16f-scrgb->srgb8-tonemapped`, `mixed-presented->srgb8`, …).
    pub format: &'static str,
    /// True when a mid-capture resize forced an early finalize.
    pub resized_early_stop: bool,
}

/// Abstract lifecycle phase of one [`VideoTap`] staging slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoSlotPhase {
    /// Available for a new swapchain copy.
    Free,
    /// Copy encoded.
    Pending,
    /// Mapping requested.
    InFlight,
}

/// Event accepted by the per-slot lifecycle gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoSlotEvent {
    /// Reserve a free slot for a newly encoded copy.
    Enqueue,
    /// Stamp and start mapping a pending copy after present.
    StartMap,
    /// A mapped frame was harvested successfully.
    MapOk,
    /// The map callback failed.
    MapError,
    /// Finalization/device loss abandons an outstanding slot.
    Abort,
}

/// Pure decision returned by [`video_slot_transition`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSlotDecision {
    /// Phase committed after the event.
    pub phase: VideoSlotPhase,
    /// Whether this transition is one counted mid-stream loss.
    pub count_drop: bool,
}

/// Decide one [`VideoTap`] staging-slot transition.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "Enqueue",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "StartMap",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "MapOk",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "MapError",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "Abort",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VideoTapSlot",
        action = "BuggyMapErrorLeaksSlot",
        reason = "Buggy=1 negative control only; a map failure frees the slot as it counts the \
                  loss"
    )
)]
#[must_use]
#[inline]
pub const fn video_slot_transition(
    phase: VideoSlotPhase,
    event: VideoSlotEvent,
) -> Option<VideoSlotDecision> {
    let (phase, count_drop) = match (phase, event) {
        (VideoSlotPhase::Free, VideoSlotEvent::Enqueue) => (VideoSlotPhase::Pending, false),
        (VideoSlotPhase::Pending, VideoSlotEvent::StartMap) => (VideoSlotPhase::InFlight, false),
        (VideoSlotPhase::InFlight, VideoSlotEvent::MapOk) => (VideoSlotPhase::Free, false),
        (VideoSlotPhase::InFlight, VideoSlotEvent::MapError)
        | (VideoSlotPhase::Pending | VideoSlotPhase::InFlight, VideoSlotEvent::Abort) => {
            (VideoSlotPhase::Free, true)
        }
        _ => return None,
    };
    Some(VideoSlotDecision { phase, count_drop })
}

/// Per-present decision after the client's fps gate and live metadata check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoPresentDecision {
    /// Copy this requested, well-described present.
    Capture,
    /// Client-requested fps decimation.
    Decimate,
    /// A requested sample had impossible metadata.
    DropInvalidMetadata,
}

/// Classify a present, keeping decimation separate from invalid-metadata loss.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VideoTapSlot",
        action = "RejectInvalidMetadata",
        project = "aterm_gpu::video_tap::video_present_decision"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "VideoTapSlot",
        action = "BuggyRejectInvalidUncounted",
        reason = "Buggy=1 negative control only; an accepted sample with invalid metadata is \
                  classified as a counted loss, never as decimation"
    )
)]
#[must_use]
#[inline]
pub const fn video_present_decision(
    fps_accepted: bool,
    metadata_valid: bool,
) -> VideoPresentDecision {
    if !fps_accepted {
        VideoPresentDecision::Decimate
    } else if metadata_valid {
        VideoPresentDecision::Capture
    } else {
        VideoPresentDecision::DropInvalidMetadata
    }
}

enum SlotState {
    Free,
    /// Copy enqueued this present; awaiting the post-present stamp + map_async.
    Pending {
        seq: u64,
        capture_encoding: CaptureEncoding,
    },
    /// map_async issued; waiting for the completion callback.
    InFlight {
        seq: u64,
        t_us: u64,
        capture_encoding: CaptureEncoding,
    },
}

impl SlotState {
    #[inline]
    const fn phase(&self) -> VideoSlotPhase {
        match self {
            Self::Free => VideoSlotPhase::Free,
            Self::Pending { .. } => VideoSlotPhase::Pending,
            Self::InFlight { .. } => VideoSlotPhase::InFlight,
        }
    }
}

struct Slot {
    buffer: SlotBuf,
    state: SlotState,
}

/// Backend staging buffer for a slot; cross-arm enqueue guards finalize a recording on a mid-take switch.
enum SlotBuf {
    #[cfg(wgpu_arm)]
    Wgpu(wgpu::Buffer),
    /// Metal slot holding a shared-storage `MTLBuffer` and its post-Submit-B status probe.
    #[cfg(target_os = "macos")]
    Metal {
        buf: crate::metal::resources::SharedBuffer,
        probe: Option<crate::metal::encoder::CbProbe>,
    },
    /// GL slot holding the destination's padded rows (`gl_present.rs`).
    #[cfg(gl_arm)]
    Gl(Vec<u8>),
}

impl SlotBuf {
    /// Whether this slot stages through the wgpu arm (always false on the wgpu-free build).
    #[inline]
    #[cfg(target_os = "macos")]
    const fn is_wgpu(&self) -> bool {
        #[cfg(wgpu_arm)]
        {
            matches!(self, Self::Wgpu(_))
        }
        #[cfg(not(wgpu_arm))]
        {
            false
        }
    }

    /// The wgpu buffer, for the map/harvest paths only WGPU slots reach.
    #[inline]
    #[cfg(wgpu_arm)]
    fn wgpu(&self) -> &wgpu::Buffer {
        match self {
            Self::Wgpu(b) => b,
            #[cfg(target_os = "macos")]
            Self::Metal { .. } => {
                unreachable!("a Metal slot never reaches the wgpu map/harvest path")
            }
            #[cfg(gl_arm)]
            Self::Gl(_) => unreachable!("a GL slot never reaches the wgpu map/harvest path"),
        }
    }

    /// Whether this slot stages through the GL arm.
    #[inline]
    #[cfg(gl_arm)]
    const fn is_gl(&self) -> bool {
        matches!(self, Self::Gl(_))
    }
}

/// Per-window recording state.
pub struct VideoTap {
    slots: Vec<Slot>,
    /// Map-completion channel where `map_async` sends its slot index.
    #[cfg(wgpu_arm)]
    done_tx: mpsc::Sender<(usize, bool)>,
    #[cfg(wgpu_arm)]
    done_rx: mpsc::Receiver<(usize, bool)>,
    store: VecDeque<CapturedFrame>,
    store_bytes: usize,
    budget_bytes: usize,
    /// Mid-stream losses (ring saturation, map failure, conversion backlog/failure, or device loss).
    dropped: u64,
    /// Head truncations (budget drop-oldest evictions).
    evicted: u64,
    /// `fps=` gate skips (deliberate, client-requested — not losses).
    decimated: u64,
    /// `fps=` capture cap; `None` captures every present.
    fps_cap: Option<u32>,
    /// Client's requested duration (ms), echoed in the dump; the GUI owns the stop deadline.
    requested_ms: u64,
    /// Earliest gate time (µs on the `epoch` clock) the next frame is accepted.
    next_capture_us: u64,
    /// Monotonic origin for the fps gate (`aterm_time`, wasm-safe like the renderer's present clocks).
    epoch: aterm_time::Instant,
    seq: u64,
    half_res: bool,
    /// Source swapchain geometry/format the ring was built for; a mismatch finalizes the recording.
    src_w: u32,
    src_h: u32,
    format: crate::device_layer::TexelFormat,
    /// First source encoding copied; later metadata changes transform per-slot and mark the source mixed.
    first_capture_encoding: Option<CaptureEncoding>,
    mixed_capture_encoding: bool,
    bpp: u32,
    resized_early_stop: bool,
    /// The conversion worker, so `after_present` never does per-pixel work.
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<ConvertWorker>,
}

/// Staging-ring depth to absorb a GPU running a frame or two behind without blocking present.
const RING: usize = 4;
/// Default RAM budget for the harvested store (bytes).
pub const DEFAULT_BUDGET: usize = 512 * 1024 * 1024;

/// Upper bound on conversions dispatched and not yet adopted, matching [`RING`].
#[cfg(not(target_arch = "wasm32"))]
const CONVERT_BACKLOG: usize = RING;

/// One mapped slot's raw padded bytes and per-frame inputs for [`mapped_to_rgba8`].
#[cfg(not(target_arch = "wasm32"))]
struct ConvertJob {
    raw: Vec<u8>,
    seq: u64,
    t_us: u64,
    capture_encoding: CaptureEncoding,
}

/// Off-thread conversion worker running [`mapped_to_rgba8`] per job until `finish` drops `job_tx`.
#[cfg(not(target_arch = "wasm32"))]
struct ConvertWorker {
    job_tx: mpsc::Sender<ConvertJob>,
    result_rx: mpsc::Receiver<Option<CapturedFrame>>,
    /// Jobs dispatched and not yet adopted; bounded by [`CONVERT_BACKLOG`].
    pending: usize,
    handle: std::thread::JoinHandle<()>,
}

/// Spawn the conversion worker for one recording, or `None` when inline conversion must be used.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_convert_worker(
    src_w: u32,
    src_h: u32,
    padded_row: usize,
    format: crate::device_layer::TexelFormat,
    half_res: bool,
) -> Option<ConvertWorker> {
    {
        let (job_tx, job_rx) = mpsc::channel::<ConvertJob>();
        let (result_tx, result_rx) = mpsc::channel::<Option<CapturedFrame>>();
        let handle = std::thread::Builder::new()
            .name("aterm-video-convert".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let frame = mapped_to_rgba8(
                        &job.raw,
                        src_w,
                        src_h,
                        padded_row,
                        format,
                        job.capture_encoding,
                        half_res,
                    )
                    .ok()
                    .map(|(rgba, w, h)| CapturedFrame {
                        seq: job.seq,
                        t_us: job.t_us,
                        w,
                        h,
                        rgba,
                    });
                    if result_tx.send(frame).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(ConvertWorker {
            job_tx,
            result_rx,
            pending: 0,
            handle,
        })
    }
}

/// Bytes per source pixel for every destination format the taps support.
#[inline]
fn capture_bpp(format: crate::device_layer::TexelFormat) -> Result<u32, String> {
    match format {
        crate::device_layer::TexelFormat::Bgra8Unorm
        | crate::device_layer::TexelFormat::Rgba8Unorm => Ok(4),
        crate::device_layer::TexelFormat::Rgba16Float => Ok(8),
        other => Err(format!("unsupported presented-frame format {other:?}")),
    }
}

#[inline]
const fn padded_row_bytes(width: u32, bpp: u32) -> u64 {
    (width as u64 * bpp as u64).div_ceil(256) * 256
}

/// Destination format layout and colour space for decoding mapped texels to sRGB RGBA8.
#[derive(Clone, Copy, Debug)]
struct TexelDecode {
    bpp: usize,
    f16: bool,
    bgra: bool,
    encoding: CaptureEncoding,
}

impl TexelDecode {
    fn new(
        format: crate::device_layer::TexelFormat,
        encoding: CaptureEncoding,
    ) -> Result<Self, String> {
        let bpp = usize::try_from(capture_bpp(format)?)
            .map_err(|_| "presented-frame bytes-per-pixel does not fit memory".to_string())?;
        Ok(Self {
            bpp,
            f16: format == crate::device_layer::TexelFormat::Rgba16Float,
            bgra: format == crate::device_layer::TexelFormat::Bgra8Unorm,
            encoding,
        })
    }

    /// The texel at byte `base` of `data` as straight, non-linear sRGB RGBA8 channels.
    #[inline]
    fn fetch(&self, data: &[u8], base: usize) -> [u32; 4] {
        if self.f16 {
            let half = |channel: usize| {
                u16::from_le_bytes([data[base + channel * 2], data[base + channel * 2 + 1]])
            };
            let linear = tone_map_scrgb_to_sdr(
                [
                    f16_to_f32(half(0)),
                    f16_to_f32(half(1)),
                    f16_to_f32(half(2)),
                ],
                self.encoding.sdr_white_scale,
            );
            let alpha = (f16_to_f32(half(3)).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
            [
                srgb8_from_linear(linear[0]),
                srgb8_from_linear(linear[1]),
                srgb8_from_linear(linear[2]),
                alpha,
            ]
        } else {
            let raw = if self.bgra {
                [data[base + 2], data[base + 1], data[base], data[base + 3]]
            } else {
                [data[base], data[base + 1], data[base + 2], data[base + 3]]
            };
            if self.encoding.color_space == CaptureColorSpace::DisplayP3 {
                let srgb = display_p3_to_srgb8([raw[0], raw[1], raw[2]]);
                [
                    u32::from(srgb[0]),
                    u32::from(srgb[1]),
                    u32::from(srgb[2]),
                    u32::from(raw[3]),
                ]
            } else {
                raw.map(u32::from)
            }
        }
    }
}

/// Validated `(width, height)` for a mapped `src_w`×`src_h` buffer with `padded_row` stride.
fn mapped_geometry(
    data_len: usize,
    src_w: u32,
    src_h: u32,
    padded_row: usize,
    bpp: usize,
) -> Result<(usize, usize), String> {
    let sw = usize::try_from(src_w)
        .map_err(|_| "presented-frame width does not fit memory".to_string())?;
    let sh = usize::try_from(src_h)
        .map_err(|_| "presented-frame height does not fit memory".to_string())?;
    let row_bytes = sw
        .checked_mul(bpp)
        .ok_or_else(|| "presented-frame row size overflow".to_string())?;
    if padded_row < row_bytes {
        return Err(format!(
            "presented-frame row stride {padded_row} is smaller than {row_bytes}"
        ));
    }
    let required = if sh == 0 {
        0
    } else {
        (sh - 1)
            .checked_mul(padded_row)
            .and_then(|n| n.checked_add(row_bytes))
            .ok_or_else(|| "presented-frame buffer size overflow".to_string())?
    };
    if data_len < required {
        return Err(format!(
            "presented-frame buffer has {data_len} bytes, expected at least {required}"
        ));
    }
    Ok((sw, sh))
}

/// Strip row padding, swizzle or decode the source format, and optionally box downsample 2×2.
fn mapped_to_rgba8(
    data: &[u8],
    src_w: u32,
    src_h: u32,
    padded_row: usize,
    format: crate::device_layer::TexelFormat,
    capture_encoding: CaptureEncoding,
    half_res: bool,
) -> Result<(Vec<u8>, u32, u32), String> {
    let texel = TexelDecode::new(format, capture_encoding)?;
    let bpp = texel.bpp;
    let (sw, sh) = mapped_geometry(data.len(), src_w, src_h, padded_row, bpp)?;

    let (dw, dh) = if half_res {
        (sw.div_ceil(2), sh.div_ceil(2))
    } else {
        (sw, sh)
    };
    let out_len = dw
        .checked_mul(dh)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "presented-frame output size overflow".to_string())?;
    let mut rgba = vec![0u8; out_len];
    let fetch = |x: usize, y: usize| texel.fetch(data, y * padded_row + x * bpp);

    for dy in 0..dh {
        for dx in 0..dw {
            let pixel = if half_res {
                let (x, y) = (dx * 2, dy * 2);
                let (a, b, c, d) = (
                    fetch(x, y),
                    fetch((x + 1).min(sw - 1), y),
                    fetch(x, (y + 1).min(sh - 1)),
                    fetch((x + 1).min(sw - 1), (y + 1).min(sh - 1)),
                );
                let samples = [a, b, c, d];
                let alpha_sum: u32 = samples.iter().map(|sample| sample[3]).sum();
                let rgb = if alpha_sum == 0 {
                    [0, 0, 0]
                } else {
                    std::array::from_fn(|channel| {
                        let premul_linear: f32 = samples
                            .iter()
                            .map(|sample| {
                                let linear = srgb8_to_linear(sample[channel] as u8);
                                linear * sample[3] as f32
                            })
                            .sum();
                        srgb8_from_linear(premul_linear / alpha_sum as f32)
                    })
                };
                [rgb[0], rgb[1], rgb[2], (alpha_sum + 2) / 4]
            } else {
                fetch(dx, dy)
            };
            let out = (dy * dw + dx) * 4;
            rgba[out] = pixel[0] as u8;
            rgba[out + 1] = pixel[1] as u8;
            rgba[out + 2] = pixel[2] as u8;
            rgba[out + 3] = pixel[3] as u8;
        }
    }
    Ok((rgba, dw as u32, dh as u32))
}

/// The client-set controls for one recording (the `video` verb's knobs), passed as one value from `Wake::Video` through `video_begin` to `VideoTap`.
#[derive(Clone, Copy, Debug)]
pub struct CaptureOpts {
    /// Downscale 2×2 at harvest (quarter the bytes — the default for multi-second runs).
    pub half_res: bool,
    /// Byte budget for the harvested store (floored at 16 MiB).
    pub budget_bytes: usize,
    /// `fps=` capture cap (clamped 1..=120).
    pub fps_cap: Option<u32>,
    /// The client's requested duration (ms), echoed as `requested_ms` in index.json.
    pub requested_ms: u64,
}

impl VideoTap {
    /// Build a tap for the current swapchain size/format with the client's [`CaptureOpts`].
    #[cfg(wgpu_arm)]
    pub(crate) fn new(
        device: &wgpu::Device,
        src_w: u32,
        src_h: u32,
        format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
        opts: CaptureOpts,
    ) -> Result<Self, String> {
        let bpp = capture_bpp(format)
            .map_err(|_| format!("video: unsupported swapchain format {format:?}"))?;
        let _ = CaptureEncoding::new(format, color_space, sdr_white_scale)
            .map_err(|error| format!("video: {error}"))?;
        let padded = padded_row_bytes(src_w, bpp);
        let size = padded * src_h as u64;
        #[cfg(not(target_arch = "wasm32"))]
        let worker = usize::try_from(padded)
            .ok()
            .and_then(|row| spawn_convert_worker(src_w, src_h, row, format, opts.half_res));
        #[cfg(wgpu_arm)]
        let (done_tx, done_rx) = mpsc::channel();
        let slots = (0..RING)
            .map(|i| Slot {
                buffer: SlotBuf::Wgpu(device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("aterm-video staging {i}")),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                })),
                state: SlotState::Free,
            })
            .collect();
        Ok(Self {
            slots,
            #[cfg(wgpu_arm)]
            done_tx,
            #[cfg(wgpu_arm)]
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
            epoch: aterm_time::Instant::now(),
            seq: 0,
            half_res: opts.half_res,
            src_w,
            src_h,
            format,
            first_capture_encoding: None,
            mixed_capture_encoding: false,
            bpp,
            resized_early_stop: false,
            #[cfg(not(target_arch = "wasm32"))]
            worker,
        })
    }

    #[inline]
    const fn padded_row(&self) -> u64 {
        padded_row_bytes(self.src_w, self.bpp)
    }

    #[inline]
    fn note_capture_encoding(&mut self, capture_encoding: CaptureEncoding) {
        match self.first_capture_encoding {
            Some(first) if first != capture_encoding => self.mixed_capture_encoding = true,
            None => self.first_capture_encoding = Some(capture_encoding),
            Some(_) => {}
        }
    }

    /// Enqueue the swapchain copy into this present's encoder (between the render pass and `queue.submit`).
    #[cfg(wgpu_arm)]
    #[must_use = "submit the encoder with device_layer::submit_host_readable naming this buffer"]
    pub(crate) fn enqueue_copy(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        frame_tex: &wgpu::Texture,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Option<wgpu::Buffer> {
        if self.resized_early_stop {
            return None;
        }
        if frame_tex.width() != self.src_w
            || frame_tex.height() != self.src_h
            || crate::device_layer::TexelFormat::from_wgpu(frame_tex.format()) != Some(self.format)
        {
            self.resized_early_stop = true;
            return None;
        }
        let now_us = self.epoch.elapsed().as_micros() as u64;
        let capture_encoding =
            self.capture_encoding_for_present(now_us, color_space, sdr_white_scale)?;
        #[cfg(target_os = "macos")]
        if self
            .slots
            .first()
            .is_some_and(|s| matches!(s.buffer, SlotBuf::Metal { .. }))
        {
            self.resized_early_stop = true;
            return None;
        }
        #[cfg(gl_arm)]
        if self.ring_is_gl() {
            self.resized_early_stop = true;
            return None;
        }
        let Some(idx) = self
            .slots
            .iter()
            .position(|s| matches!(s.state, SlotState::Free))
        else {
            self.dropped += 1;
            return None;
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
                buffer: self.slots[idx].buffer.wgpu(),
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
        self.note_capture_encoding(capture_encoding);
        let transition =
            video_slot_transition(self.slots[idx].state.phase(), VideoSlotEvent::Enqueue)
                .expect("the selected video staging slot is free");
        match transition {
            VideoSlotDecision {
                phase: VideoSlotPhase::Pending,
                count_drop: false,
            } => {
                self.slots[idx].state = SlotState::Pending {
                    seq: self.seq,
                    capture_encoding,
                };
            }
            _ => unreachable!("a free video slot enqueues to Pending without a drop"),
        }
        Some(self.slots[idx].buffer.wgpu().clone())
    }

    /// Build a Metal-arm tap.
    #[cfg(target_os = "macos")]
    pub(crate) fn new_metal(
        mint: &crate::metal::resources::MetalResourceDevice,
        src_w: u32,
        src_h: u32,
        format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
        opts: CaptureOpts,
    ) -> Result<Self, String> {
        let bpp = capture_bpp(format)
            .map_err(|_| format!("video: unsupported swapchain format {format:?}"))?;
        let _ = CaptureEncoding::new(format, color_space, sdr_white_scale)
            .map_err(|error| format!("video: {error}"))?;
        let padded = padded_row_bytes(src_w, bpp);
        let size = usize::try_from(padded * u64::from(src_h))
            .map_err(|_| "video: staging size overflows usize".to_owned())?;
        #[cfg(not(target_arch = "wasm32"))]
        let worker = usize::try_from(padded)
            .ok()
            .and_then(|row| spawn_convert_worker(src_w, src_h, row, format, opts.half_res));
        #[cfg(wgpu_arm)]
        let (done_tx, done_rx) = mpsc::channel();
        let mut slots = Vec::with_capacity(RING);
        for _ in 0..RING {
            slots.push(Slot {
                buffer: SlotBuf::Metal {
                    buf: crate::device_layer::DeviceHandle::Metal(mint)
                        .try_create_readback_buffer("aterm-gpu video tap slot", size as u64)?
                        .into_metal(),
                    probe: None,
                },
                state: SlotState::Free,
            });
        }
        Ok(Self {
            slots,
            #[cfg(wgpu_arm)]
            done_tx,
            #[cfg(wgpu_arm)]
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
            epoch: aterm_time::Instant::now(),
            seq: 0,
            half_res: opts.half_res,
            src_w,
            src_h,
            format,
            first_capture_encoding: None,
            mixed_capture_encoding: false,
            bpp,
            resized_early_stop: false,
            #[cfg(not(target_arch = "wasm32"))]
            worker,
        })
    }

    /// Metal enqueue.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_enqueue_copy(
        &mut self,
        cb: &mut crate::metal::encoder::CommandBuffer<'_>,
        target: &crate::metal::resources::SealedTexture,
        target_format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Result<(), String> {
        if self.resized_early_stop {
            return Ok(());
        }
        if self.slots.first().is_some_and(|s| s.buffer.is_wgpu()) {
            self.resized_early_stop = true;
            return Ok(());
        }
        if target.width() != self.src_w as usize
            || target.height() != self.src_h as usize
            || target_format != self.format
        {
            self.resized_early_stop = true;
            return Ok(());
        }
        let now_us = self.epoch.elapsed().as_micros() as u64;
        let Some(capture_encoding) =
            self.capture_encoding_for_present(now_us, color_space, sdr_white_scale)
        else {
            return Ok(());
        };
        let Some(idx) = self
            .slots
            .iter()
            .position(|s| matches!(s.state, SlotState::Free))
        else {
            self.dropped += 1;
            return Ok(());
        };
        let padded = self.padded_row() as usize;
        {
            let (buf, probe) = match &mut self.slots[idx].buffer {
                SlotBuf::Metal { buf, probe } => (buf, probe),
                #[cfg(multi_arm)]
                _ => unreachable!("guarded homogeneous above"),
            };
            *probe = None;
            cb.copy_texture_to_buffer(
                target,
                self.src_w as usize,
                self.src_h as usize,
                buf.obj(),
                padded,
            )?;
        }
        self.seq += 1;
        self.note_capture_encoding(capture_encoding);
        let transition =
            video_slot_transition(self.slots[idx].state.phase(), VideoSlotEvent::Enqueue)
                .expect("the selected video staging slot is free");
        match transition {
            VideoSlotDecision {
                phase: VideoSlotPhase::Pending,
                count_drop: false,
            } => {
                self.slots[idx].state = SlotState::Pending {
                    seq: self.seq,
                    capture_encoding,
                };
            }
            _ => unreachable!("a free video slot enqueues to Pending without a drop"),
        }
        Ok(())
    }

    /// Attach the committed Submit B's probe to the Pending slot filled above.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_note_submitted(&mut self, probe: crate::metal::encoder::CbProbe) {
        for slot in &mut self.slots {
            if matches!(slot.state, SlotState::Pending { .. })
                && let SlotBuf::Metal { probe: p, .. } = &mut slot.buffer
                && p.is_none()
            {
                *p = Some(probe);
                return;
            }
        }
    }

    /// Metal harvest (status-polled twin of [`Self::harvest_ready`]).
    #[cfg(target_os = "macos")]
    fn metal_harvest_ready(&mut self, block: bool) {
        self.adopt_converted();
        for idx in 0..self.slots.len() {
            let SlotState::InFlight {
                seq,
                t_us,
                capture_encoding,
            } = self.slots[idx].state
            else {
                continue;
            };
            let outcome = {
                let probe = match &self.slots[idx].buffer {
                    SlotBuf::Metal { probe, .. } => probe,
                    #[cfg(multi_arm)]
                    _ => continue,
                };
                match probe {
                    Some(p) if block => Some(p.wait_terminal()),
                    Some(p) => p.try_terminal(),
                    None => Some(crate::metal::loss::CbOutcome::Lost {
                        code: None,
                        name: "tap copy's command buffer was never committed",
                    }),
                }
            };
            let Some(outcome) = outcome else { continue };
            let ok = matches!(outcome, crate::metal::loss::CbOutcome::Completed);
            let event = if ok {
                VideoSlotEvent::MapOk
            } else {
                VideoSlotEvent::MapError
            };
            let transition = video_slot_transition(self.slots[idx].state.phase(), event)
                .expect("a terminal probe belongs to an in-flight slot");
            if ok
                && let Some(frame) =
                    self.metal_convert_or_dispatch(idx, seq, t_us, capture_encoding)
            {
                self.push_frame(frame);
            }
            if transition.count_drop {
                self.dropped += 1;
            }
            let probe = match &mut self.slots[idx].buffer {
                SlotBuf::Metal { probe, .. } => probe,
                #[cfg(multi_arm)]
                _ => unreachable!("matched Metal above"),
            };
            *probe = None;
            match transition.phase {
                VideoSlotPhase::Free => self.slots[idx].state = SlotState::Free,
                _ => unreachable!("a completed video probe always releases its slot"),
            }
        }
        self.adopt_converted();
    }

    /// Metal twin of [`Self::convert_or_dispatch`] reading raw padded bytes from the slot's shared buffer.
    #[cfg(target_os = "macos")]
    fn metal_convert_or_dispatch(
        &mut self,
        idx: usize,
        seq: u64,
        t_us: u64,
        capture_encoding: CaptureEncoding,
    ) -> Option<CapturedFrame> {
        let len = self.padded_row() as usize * self.src_h as usize;
        let raw = {
            let buf = match &self.slots[idx].buffer {
                SlotBuf::Metal { buf, .. } => buf,
                #[cfg(multi_arm)]
                _ => unreachable!("a Metal harvest reads a Metal slot"),
            };
            // SAFETY: shared storage of exactly `len` bytes; the probe reported the writing command buffer terminal.
            unsafe { crate::metal::ffi::buffer_bytes(buf.obj(), len) }
        };
        if let Some(worker) = self.worker.as_mut() {
            if worker.pending >= CONVERT_BACKLOG {
                self.dropped += 1;
                return None;
            }
            match worker.job_tx.send(ConvertJob {
                raw,
                seq,
                t_us,
                capture_encoding,
            }) {
                Ok(()) => {
                    worker.pending += 1;
                    return None;
                }
                Err(send_err) => {
                    self.worker = None;
                    let raw = send_err.0.raw;
                    return Some(Self::metal_inline_convert(
                        &raw,
                        self.src_w,
                        self.src_h,
                        self.padded_row() as usize,
                        self.format,
                        capture_encoding,
                        self.half_res,
                        seq,
                        t_us,
                    ));
                }
            }
        }
        Some(Self::metal_inline_convert(
            &raw,
            self.src_w,
            self.src_h,
            self.padded_row() as usize,
            self.format,
            capture_encoding,
            self.half_res,
            seq,
            t_us,
        ))
    }

    /// The inline (no-worker) conversion for a Metal slot's raw bytes.
    #[cfg(target_os = "macos")]
    #[allow(clippy::too_many_arguments, reason = "a private conversion tail")]
    fn metal_inline_convert(
        raw: &[u8],
        src_w: u32,
        src_h: u32,
        padded: usize,
        format: crate::device_layer::TexelFormat,
        capture_encoding: CaptureEncoding,
        half_res: bool,
        seq: u64,
        t_us: u64,
    ) -> CapturedFrame {
        let (rgba, width, height) = mapped_to_rgba8(
            raw,
            src_w,
            src_h,
            padded,
            format,
            capture_encoding,
            half_res,
        )
        .expect("VideoTap staging layout is validated at construction");
        CapturedFrame {
            seq,
            t_us,
            w: width,
            h: height,
            rgba,
        }
    }

    /// Build a GL-arm tap.
    #[cfg(gl_arm)]
    pub(crate) fn new_gl(
        src_w: u32,
        src_h: u32,
        format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
        opts: CaptureOpts,
    ) -> Result<Self, String> {
        let bpp = capture_bpp(format)
            .map_err(|_| format!("video: unsupported swapchain format {format:?}"))?;
        let _ = CaptureEncoding::new(format, color_space, sdr_white_scale)
            .map_err(|error| format!("video: {error}"))?;
        #[cfg(not(target_arch = "wasm32"))]
        let worker = usize::try_from(padded_row_bytes(src_w, bpp))
            .ok()
            .and_then(|row| spawn_convert_worker(src_w, src_h, row, format, opts.half_res));
        #[cfg(wgpu_arm)]
        let (done_tx, done_rx) = mpsc::channel();
        let slots = (0..RING)
            .map(|_| Slot {
                buffer: SlotBuf::Gl(Vec::new()),
                state: SlotState::Free,
            })
            .collect();
        Ok(Self {
            slots,
            #[cfg(wgpu_arm)]
            done_tx,
            #[cfg(wgpu_arm)]
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
            epoch: aterm_time::Instant::now(),
            seq: 0,
            half_res: opts.half_res,
            src_w,
            src_h,
            format,
            first_capture_encoding: None,
            mixed_capture_encoding: false,
            bpp,
            resized_early_stop: false,
            #[cfg(not(target_arch = "wasm32"))]
            worker,
        })
    }

    /// Whether this ring stages through the GL arm.
    #[inline]
    #[cfg(gl_arm)]
    pub(crate) fn ring_is_gl(&self) -> bool {
        self.slots.first().is_some_and(|s| s.buffer.is_gl())
    }

    /// Enqueue GL present destination bytes (`dims`, padded to [`padded_row_bytes`], `Rgba8Unorm`) under [`Self::enqueue_copy`] rules.
    #[cfg(gl_arm)]
    pub(crate) fn gl_enqueue_frame(
        &mut self,
        (w, h): (u32, u32),
        padded_rgba: &[u8],
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) {
        if self.resized_early_stop {
            return;
        }
        if !self.ring_is_gl()
            || (w, h) != (self.src_w, self.src_h)
            || self.format != crate::device_layer::TexelFormat::Rgba8Unorm
        {
            self.resized_early_stop = true;
            return;
        }
        let now_us = self.epoch.elapsed().as_micros() as u64;
        let Some(capture_encoding) =
            self.capture_encoding_for_present(now_us, color_space, sdr_white_scale)
        else {
            return;
        };
        let Some(idx) = self
            .slots
            .iter()
            .position(|s| matches!(s.state, SlotState::Free))
        else {
            self.dropped += 1;
            return;
        };
        let len = self.padded_row() as usize * self.src_h as usize;
        match &mut self.slots[idx].buffer {
            SlotBuf::Gl(bytes) => {
                bytes.clear();
                bytes.extend_from_slice(&padded_rgba[..len.min(padded_rgba.len())]);
            }
            #[cfg(multi_arm)]
            _ => unreachable!("guarded homogeneous above"),
        }
        self.seq += 1;
        self.note_capture_encoding(capture_encoding);
        let transition =
            video_slot_transition(self.slots[idx].state.phase(), VideoSlotEvent::Enqueue)
                .expect("the selected video staging slot is free");
        match transition {
            VideoSlotDecision {
                phase: VideoSlotPhase::Pending,
                count_drop: false,
            } => {
                self.slots[idx].state = SlotState::Pending {
                    seq: self.seq,
                    capture_encoding,
                };
            }
            _ => unreachable!("a free video slot enqueues to Pending without a drop"),
        }
    }

    /// [`Self::after_present`] for a GL ring.
    #[cfg(gl_arm)]
    pub(crate) fn gl_after_present(&mut self, t_us: u64) {
        self.adopt_converted();
        for slot in &mut self.slots {
            if let SlotState::Pending {
                seq,
                capture_encoding,
            } = slot.state
            {
                let start = video_slot_transition(slot.state.phase(), VideoSlotEvent::StartMap)
                    .expect("only pending slots start mapping");
                debug_assert_eq!(start.phase, VideoSlotPhase::InFlight);
                slot.state = SlotState::InFlight {
                    seq,
                    t_us,
                    capture_encoding,
                };
            }
        }
        for idx in 0..self.slots.len() {
            let SlotState::InFlight {
                seq,
                t_us,
                capture_encoding,
            } = self.slots[idx].state
            else {
                continue;
            };
            let done = video_slot_transition(VideoSlotPhase::InFlight, VideoSlotEvent::MapOk)
                .expect("an in-flight slot completes");
            let raw = match &mut self.slots[idx].buffer {
                SlotBuf::Gl(bytes) => std::mem::take(bytes),
                #[cfg(multi_arm)]
                _ => unreachable!("a GL ring holds GL slots"),
            };
            if let Some(frame) = self.convert_raw(raw, seq, t_us, capture_encoding) {
                self.push_frame(frame);
            }
            debug_assert!(!done.count_drop);
            self.slots[idx].state = SlotState::Free;
        }
        self.adopt_converted();
    }

    /// Finalize a GL ring.
    #[cfg(gl_arm)]
    pub(crate) fn gl_finish(mut self) -> VideoTake {
        for s in &mut self.slots {
            if !matches!(s.state, SlotState::Free) {
                let transition = video_slot_transition(s.state.phase(), VideoSlotEvent::Abort)
                    .expect("only an outstanding slot reaches finalization abort");
                self.dropped += u64::from(transition.count_drop);
                s.state = SlotState::Free;
            }
        }
        self.drain_convert_lane();
        self.into_take()
    }

    /// Hand one slot's raw padded bytes to the conversion lane, or convert inline (GL twin of `metal_convert_or_dispatch`).
    #[cfg(gl_arm)]
    fn convert_raw(
        &mut self,
        raw: Vec<u8>,
        seq: u64,
        t_us: u64,
        capture_encoding: CaptureEncoding,
    ) -> Option<CapturedFrame> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(worker) = self.worker.as_mut() {
            if worker.pending >= CONVERT_BACKLOG {
                self.dropped += 1;
                return None;
            }
            match worker.job_tx.send(ConvertJob {
                raw,
                seq,
                t_us,
                capture_encoding,
            }) {
                Ok(()) => {
                    worker.pending += 1;
                    return None;
                }
                Err(send_err) => {
                    self.worker = None;
                    return self.convert_inline(&send_err.0.raw, seq, t_us, capture_encoding);
                }
            }
        }
        self.convert_inline(&raw, seq, t_us, capture_encoding)
    }

    /// Inline conversion of raw padded bytes; a malformed slot is one drop.
    #[cfg(gl_arm)]
    fn convert_inline(
        &mut self,
        raw: &[u8],
        seq: u64,
        t_us: u64,
        capture_encoding: CaptureEncoding,
    ) -> Option<CapturedFrame> {
        match mapped_to_rgba8(
            raw,
            self.src_w,
            self.src_h,
            self.padded_row() as usize,
            self.format,
            capture_encoding,
            self.half_res,
        ) {
            Ok((rgba, w, h)) => Some(CapturedFrame {
                seq,
                t_us,
                w,
                h,
                rgba,
            }),
            Err(_) => {
                self.dropped += 1;
                None
            }
        }
    }

    /// The `fps=` gate on `(self, now_us)`: accepts every frame when uncapped, or once per `1_000_000 / n` µs.
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

    /// Apply the fps gate before validating colour metadata so invalid metadata counts a loss only on a requested frame.
    fn capture_encoding_for_present(
        &mut self,
        now_us: u64,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Option<CaptureEncoding> {
        let fps_accepted = self.should_capture(now_us);
        if !fps_accepted {
            debug_assert_eq!(
                video_present_decision(false, false),
                VideoPresentDecision::Decimate
            );
            return None;
        }
        let capture_encoding = CaptureEncoding::new(self.format, color_space, sdr_white_scale);
        let metadata_valid = capture_encoding.is_ok();
        match video_present_decision(fps_accepted, metadata_valid) {
            VideoPresentDecision::Capture => {
                Some(capture_encoding.expect("valid metadata decision has a validated encoding"))
            }
            VideoPresentDecision::Decimate => {
                unreachable!("the accepted fps branch cannot be decimated")
            }
            VideoPresentDecision::DropInvalidMetadata => {
                self.dropped += 1;
                None
            }
        }
    }

    /// Post-present hook (right after `frame.present()`).
    #[cfg(wgpu_arm)]
    pub(crate) fn after_present(&mut self, device: &wgpu::Device, t_us: u64) {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let SlotState::Pending {
                seq,
                capture_encoding,
            } = slot.state
            {
                match &slot.buffer {
                    SlotBuf::Wgpu(buffer) => {
                        let tx = self.done_tx.clone();
                        #[expect(
                            clippy::disallowed_methods,
                            reason = "the present that filled this slot was submitted by \
                                      device_layer::submit_host_readable (enqueue_copy's buffer)"
                        )]
                        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                            let _ = tx.send((i, r.is_ok()));
                        });
                    }
                    #[cfg(target_os = "macos")]
                    SlotBuf::Metal { .. } => {}
                    #[cfg(gl_arm)]
                    SlotBuf::Gl(_) => {}
                }
                let transition =
                    video_slot_transition(slot.state.phase(), VideoSlotEvent::StartMap)
                        .expect("only pending slots start mapping");
                match transition {
                    VideoSlotDecision {
                        phase: VideoSlotPhase::InFlight,
                        count_drop: false,
                    } => {
                        slot.state = SlotState::InFlight {
                            seq,
                            t_us,
                            capture_encoding,
                        };
                    }
                    _ => unreachable!("a pending video slot starts mapping without a drop"),
                }
            }
        }
        let _ = device.poll(wgpu::PollType::Poll);
        self.harvest_ready();
        #[cfg(target_os = "macos")]
        self.metal_harvest_ready(false);
    }

    /// Whether this ring stages through the armed (Metal) arm.
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) fn ring_is_metal(&self) -> bool {
        self.slots.first().is_some_and(|s| !s.buffer.is_wgpu())
    }

    /// [`Self::after_present`] for a Metal ring.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_after_present(&mut self, t_us: u64) {
        for slot in self.slots.iter_mut() {
            if let SlotState::Pending {
                seq,
                capture_encoding,
            } = slot.state
            {
                let transition =
                    video_slot_transition(slot.state.phase(), VideoSlotEvent::StartMap)
                        .expect("only pending slots start mapping");
                match transition {
                    VideoSlotDecision {
                        phase: VideoSlotPhase::InFlight,
                        count_drop: false,
                    } => {
                        slot.state = SlotState::InFlight {
                            seq,
                            t_us,
                            capture_encoding,
                        };
                    }
                    _ => unreachable!("a pending video slot starts mapping without a drop"),
                }
            }
        }
        self.metal_harvest_ready(false);
    }

    /// Drain the completion channel, adopting finished conversions and routing newly mapped slots toward RGBA8.
    #[cfg(wgpu_arm)]
    fn harvest_ready(&mut self) {
        self.adopt_converted();
        while let Ok((idx, ok)) = self.done_rx.try_recv() {
            let (seq, t_us, capture_encoding) = match self.slots[idx].state {
                SlotState::InFlight {
                    seq,
                    t_us,
                    capture_encoding,
                } => (seq, t_us, capture_encoding),
                _ => continue,
            };
            let event = if ok {
                VideoSlotEvent::MapOk
            } else {
                VideoSlotEvent::MapError
            };
            let transition = video_slot_transition(self.slots[idx].state.phase(), event)
                .expect("a current map callback belongs to an in-flight slot");
            if ok && let Some(frame) = self.convert_or_dispatch(idx, seq, t_us, capture_encoding) {
                self.push_frame(frame);
            }
            if transition.count_drop {
                self.dropped += 1;
            }
            self.slots[idx].buffer.wgpu().unmap();
            match transition.phase {
                VideoSlotPhase::Free => self.slots[idx].state = SlotState::Free,
                _ => unreachable!("a completed video map always releases its slot"),
            }
        }
        self.adopt_converted();
    }

    /// Route one mapped slot's bytes toward RGBA8 via the worker ([`Self::adopt_converted`]) or inline.
    #[cfg(wgpu_arm)]
    fn convert_or_dispatch(
        &mut self,
        idx: usize,
        seq: u64,
        t_us: u64,
        capture_encoding: CaptureEncoding,
    ) -> Option<CapturedFrame> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(worker) = self.worker.as_mut() {
            if worker.pending >= CONVERT_BACKLOG {
                self.dropped += 1;
                return None;
            }
            let raw = self.slots[idx]
                .buffer
                .wgpu()
                .slice(..)
                .get_mapped_range()
                .to_vec();
            match worker.job_tx.send(ConvertJob {
                raw,
                seq,
                t_us,
                capture_encoding,
            }) {
                Ok(()) => {
                    worker.pending += 1;
                    return None;
                }
                Err(_) => self.worker = None,
            }
        }
        Some(self.copy_out(idx, seq, t_us, capture_encoding))
    }

    /// Adopt finished conversions into the store without blocking; a failure counts one loss.
    fn adopt_converted(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        loop {
            let Some(worker) = self.worker.as_mut() else {
                return;
            };
            let Ok(result) = worker.result_rx.try_recv() else {
                return;
            };
            worker.pending = worker.pending.saturating_sub(1);
            match result {
                Some(frame) => self.push_frame(frame),
                None => self.dropped += 1,
            }
        }
    }

    /// Inline fallback (no worker): convert one mapped slot to a tightly packed RGBA8 frame.
    #[cfg(wgpu_arm)]
    fn copy_out(
        &self,
        idx: usize,
        seq: u64,
        t_us: u64,
        capture_encoding: CaptureEncoding,
    ) -> CapturedFrame {
        let padded = self.padded_row() as usize;
        let data = self.slots[idx].buffer.wgpu().slice(..).get_mapped_range();
        let (rgba, width, height) = mapped_to_rgba8(
            &data,
            self.src_w,
            self.src_h,
            padded,
            self.format,
            capture_encoding,
            self.half_res,
        )
        .expect("VideoTap staging layout is validated at construction");
        drop(data);
        CapturedFrame {
            seq,
            t_us,
            w: width,
            h: height,
            rgba,
        }
    }

    /// [`ordered_capture_store_push`] into this tap's store; evictions count in `evicted`, not `dropped`.
    #[inline]
    fn push_frame(&mut self, f: CapturedFrame) {
        self.evicted += ordered_capture_store_push(
            &mut self.store,
            &mut self.store_bytes,
            self.budget_bytes,
            f,
        );
    }

    /// Whether a mid-capture resize already finalized the recording.
    #[must_use]
    #[inline]
    pub const fn resized(&self) -> bool {
        self.resized_early_stop
    }

    /// Frames harvested so far (`video status`), including frames dispatched to the worker and not yet adopted.
    #[must_use]
    #[inline]
    pub fn frames_so_far(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        let dispatched = self.worker.as_ref().map_or(0, |worker| worker.pending);
        #[cfg(target_arch = "wasm32")]
        let dispatched = 0;
        self.store.len() + dispatched
    }

    /// Finalize a Metal ring.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_finish(mut self) -> VideoTake {
        self.metal_harvest_ready(true);
        for s in &mut self.slots {
            if !matches!(s.state, SlotState::Free) {
                let transition = video_slot_transition(s.state.phase(), VideoSlotEvent::Abort)
                    .expect("only an outstanding slot reaches finalization abort");
                self.dropped += u64::from(transition.count_drop);
                match transition.phase {
                    VideoSlotPhase::Free => s.state = SlotState::Free,
                    _ => unreachable!("finalization abort always releases the video slot"),
                }
            }
        }
        self.drain_convert_lane();
        self.into_take()
    }

    /// Finalize: blocking drain of in-flight maps and dispatched conversions (the recording is over), then hand out the store.
    #[cfg(wgpu_arm)]
    pub(crate) fn finish(mut self, device: &wgpu::Device) -> VideoTake {
        #[cfg(target_os = "macos")]
        self.metal_harvest_ready(true);
        let any_inflight = self
            .slots
            .iter()
            .any(|s| !matches!(s.state, SlotState::Free));
        if any_inflight {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            self.harvest_ready();
            for s in &mut self.slots {
                if !matches!(s.state, SlotState::Free) {
                    let transition = video_slot_transition(s.state.phase(), VideoSlotEvent::Abort)
                        .expect("only an outstanding slot reaches finalization abort");
                    self.dropped += u64::from(transition.count_drop);
                    match transition.phase {
                        VideoSlotPhase::Free => s.state = SlotState::Free,
                        _ => unreachable!("finalization abort always releases the video slot"),
                    }
                }
            }
        }
        self.drain_convert_lane();
        self.into_take()
    }

    /// Drain the conversion lane by dropping the job sender and adopting every result.
    fn drain_convert_lane(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(ConvertWorker {
            job_tx,
            result_rx,
            mut pending,
            handle,
        }) = self.worker.take()
        {
            drop(job_tx);
            while pending > 0 {
                match result_rx.recv() {
                    Ok(Some(frame)) => self.push_frame(frame),
                    Ok(None) => self.dropped += 1,
                    Err(_) => {
                        self.dropped += pending as u64;
                        pending = 0;
                        continue;
                    }
                }
                pending -= 1;
            }
            let _ = handle.join();
        }
    }

    /// The take assembly both finish arms share.
    fn into_take(self) -> VideoTake {
        let (dw, dh) = if self.half_res {
            (self.src_w.div_ceil(2), self.src_h.div_ceil(2))
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
            format: if self.mixed_capture_encoding {
                "mixed-presented->srgb8"
            } else {
                self.first_capture_encoding
                    .map_or("unknown", |encoding| encoding.format_label(self.format))
            },
            resized_early_stop: self.resized_early_stop,
        }
    }
}

/// Device-free projection of the one-shot presented-frame lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentedFramePhase {
    /// Waiting to encode the first destination copy.
    Armed,
    /// Copy encoded.
    Pending,
    /// Mapping requested.
    InFlight,
    /// Terminal success or error is stored.
    Complete,
}

/// Event accepted by the one-shot lifecycle gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentedFrameEvent {
    /// Destination geometry and live colour metadata validated.
    EnqueueValid,
    /// Destination geometry or live colour metadata was invalid.
    RejectEnqueue,
    /// The encoded copy reached its matching successful-present hook.
    StartMap,
    /// Mapping and pixel conversion completed.
    CompleteMap,
    /// Mapping, device wait, or callback completion failed.
    MapError,
}

/// Terminal result produced by a one-shot transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentedFrameOutcome {
    /// The lifecycle remains non-terminal.
    None,
    /// An exact frame is ready.
    Frame,
    /// A fail-closed capture error is ready.
    Error,
}

/// Pure decision returned by [`presented_frame_transition`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentedFrameDecision {
    /// Phase committed after the event.
    pub phase: PresentedFramePhase,
    /// Result introduced by this transition.
    pub outcome: PresentedFrameOutcome,
}

/// Decide one [`PresentedFrameTap`] transition.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PresentedFrameTap",
        action = "EnqueueValid",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PresentedFrameTap",
        action = "RejectEnqueue",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PresentedFrameTap",
        action = "StartMap",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PresentedFrameTap",
        action = "CompleteMap",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PresentedFrameTap",
        action = "MapError",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "PresentedFrameTap",
        action = "BuggyMapErrorPublishesFrame",
        reason = "Buggy=1 negative control only; the MapError arm completes with an Error, never \
                  a Frame"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "PresentedFrameTap",
        action = "BuggyRejectReservesSlot",
        reason = "Buggy=1 negative control only; the RejectEnqueue arm completes the one-shot \
                  instead of reserving the staging buffer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "PresentedFrameTap",
        action = "BuggyMapErrorWithoutOutcome",
        reason = "Buggy=1 negative control only; every terminal arm carries its Frame or Error"
    )
)]
#[must_use]
#[inline]
pub const fn presented_frame_transition(
    phase: PresentedFramePhase,
    event: PresentedFrameEvent,
) -> Option<PresentedFrameDecision> {
    let (phase, outcome) = match (phase, event) {
        (PresentedFramePhase::Armed, PresentedFrameEvent::EnqueueValid) => {
            (PresentedFramePhase::Pending, PresentedFrameOutcome::None)
        }
        (PresentedFramePhase::Armed, PresentedFrameEvent::RejectEnqueue) => {
            (PresentedFramePhase::Complete, PresentedFrameOutcome::Error)
        }
        (PresentedFramePhase::Pending, PresentedFrameEvent::StartMap) => {
            (PresentedFramePhase::InFlight, PresentedFrameOutcome::None)
        }
        (PresentedFramePhase::InFlight, PresentedFrameEvent::CompleteMap) => {
            (PresentedFramePhase::Complete, PresentedFrameOutcome::Frame)
        }
        (PresentedFramePhase::InFlight, PresentedFrameEvent::MapError) => {
            (PresentedFramePhase::Complete, PresentedFrameOutcome::Error)
        }
        _ => return None,
    };
    Some(PresentedFrameDecision { phase, outcome })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentedFrameState {
    /// Waiting for the next successful present to enqueue the one copy.
    Armed,
    /// The copy is in a command encoder that has not yet reached the caller's post-present hook.
    Pending,
    /// `map_async` is outstanding.
    InFlight { t_us: u64 },
    /// `result` contains either the exact frame or a terminal capture error.
    Complete,
}

impl PresentedFrameState {
    #[inline]
    const fn phase(self) -> PresentedFramePhase {
        match self {
            Self::Armed => PresentedFramePhase::Armed,
            Self::Pending => PresentedFramePhase::Pending,
            Self::InFlight { .. } => PresentedFramePhase::InFlight,
            Self::Complete => PresentedFramePhase::Complete,
        }
    }
}

/// A presented destination as its staging buffer mapped it.
#[derive(Debug)]
pub struct RawCapture {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    padded_row: usize,
    format: crate::device_layer::TexelFormat,
    texel: TexelDecode,
    t_us: u64,
}

impl RawCapture {
    fn new(
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        padded_row: usize,
        format: crate::device_layer::TexelFormat,
        encoding: CaptureEncoding,
        t_us: u64,
    ) -> Result<Self, String> {
        let texel = TexelDecode::new(format, encoding)?;
        mapped_geometry(bytes.len(), width, height, padded_row, texel.bpp)?;
        Ok(Self {
            bytes,
            width,
            height,
            padded_row,
            format,
            texel,
            t_us,
        })
    }

    /// A tightly packed 8-bit sRGB `Rgba8Unorm` destination, as the GL arm reads its resident virtual target back.
    #[cfg(gl_arm)]
    pub(crate) fn rgba8_srgb(
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        t_us: u64,
    ) -> Result<Self, String> {
        let format = crate::device_layer::TexelFormat::Rgba8Unorm;
        let encoding = CaptureEncoding::new(format, CaptureColorSpace::Srgb, 1.0)?;
        Self::new(
            bytes,
            width,
            height,
            width as usize * 4,
            format,
            encoding,
            t_us,
        )
    }

    /// A tightly or loosely padded 8-bit sRGB `Bgra8Unorm` destination, as a swapchain tap maps one, for tests in other crates.
    #[cfg(any(test, debug_assertions))]
    pub fn bgra8_srgb_for_test(
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        padded_row: usize,
    ) -> Result<Self, String> {
        let format = crate::device_layer::TexelFormat::Bgra8Unorm;
        let encoding = CaptureEncoding::new(format, CaptureColorSpace::Srgb, 1.0)?;
        Self::new(bytes, width, height, padded_row, format, encoding, 0)
    }

    /// An f16 extended-linear `Rgba16Float` destination, as an HDR swapchain tap maps one, for tests in other crates.
    #[cfg(any(test, debug_assertions))]
    pub fn scrgb_f16_for_test(
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        padded_row: usize,
    ) -> Result<Self, String> {
        let format = crate::device_layer::TexelFormat::Rgba16Float;
        let encoding = CaptureEncoding::new(format, CaptureColorSpace::ExtendedLinearSrgb, 1.0)?;
        Self::new(bytes, width, height, padded_row, format, encoding, 0)
    }

    #[must_use]
    #[inline]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    #[inline]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The bytes this capture holds.
    #[must_use]
    #[inline]
    pub const fn allocation(&self) -> usize {
        self.bytes.capacity()
    }

    /// Append `width` pixels of row `y`, from column `x`, as straight non-linear sRGB RGBA8.
    pub fn rgba_row(&self, y: usize, x: usize, width: usize, out: &mut Vec<u8>) {
        let inside = y < self.height as usize
            && x.checked_add(width)
                .is_some_and(|end| end <= self.width as usize);
        if !inside {
            return;
        }
        out.reserve(width.saturating_mul(4));
        let row = y * self.padded_row;
        for column in x..x + width {
            let pixel = self.texel.fetch(&self.bytes, row + column * self.texel.bpp);
            out.extend_from_slice(&pixel.map(|channel| channel as u8));
        }
    }

    /// The whole destination as tightly packed straight sRGB RGBA8, stamped with the present it was taken from.
    pub fn convert(self) -> Result<CapturedFrame, String> {
        let (rgba, w, h) = mapped_to_rgba8(
            &self.bytes,
            self.width,
            self.height,
            self.padded_row,
            self.format,
            self.texel.encoding,
            false,
        )?;
        Ok(CapturedFrame {
            seq: 1,
            t_us: self.t_us,
            w,
            h,
            rgba,
        })
    }
}

/// The bytes a one-shot tap of a `src_w`×`src_h` destination in `format` stages, and so the [`RawCapture`] its harvest copies holds.
pub(crate) fn presented_snapshot_len(
    src_w: u32,
    src_h: u32,
    format: crate::device_layer::TexelFormat,
) -> Result<u64, String> {
    let bpp = capture_bpp(format)
        .map_err(|_| format!("presented snapshot: unsupported format {format:?}"))?;
    padded_row_bytes(src_w, bpp)
        .checked_mul(u64::from(src_h))
        .ok_or_else(|| "presented snapshot: staging size overflow".to_string())
}

/// One-shot tap for the exact presented destination.
pub(crate) struct PresentedFrameTap {
    buffer: SlotBuf,
    #[cfg(wgpu_arm)]
    done_tx: mpsc::Sender<bool>,
    #[cfg(wgpu_arm)]
    done_rx: mpsc::Receiver<bool>,
    state: PresentedFrameState,
    result: Option<Result<RawCapture, String>>,
    src_w: u32,
    src_h: u32,
    format: crate::device_layer::TexelFormat,
    capture_encoding: Option<CaptureEncoding>,
    bpp: u32,
}

impl PresentedFrameTap {
    #[cfg(wgpu_arm)]
    pub(crate) fn new(
        device: &wgpu::Device,
        src_w: u32,
        src_h: u32,
        format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Result<Self, String> {
        let bpp = capture_bpp(format)
            .map_err(|_| format!("presented snapshot: unsupported format {format:?}"))?;
        let _ = CaptureEncoding::new(format, color_space, sdr_white_scale)
            .map_err(|error| format!("presented snapshot: {error}"))?;
        let size = presented_snapshot_len(src_w, src_h, format)?;
        if size == 0 {
            return Err("presented snapshot: destination has zero area".to_string());
        }
        let buffer = SlotBuf::Wgpu(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm presented-frame snapshot staging"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }));
        #[cfg(wgpu_arm)]
        let (done_tx, done_rx) = mpsc::channel();
        Ok(Self {
            buffer,
            #[cfg(wgpu_arm)]
            done_tx,
            #[cfg(wgpu_arm)]
            done_rx,
            state: PresentedFrameState::Armed,
            result: None,
            src_w,
            src_h,
            format,
            capture_encoding: None,
            bpp,
        })
    }

    #[inline]
    const fn padded_row(&self) -> u64 {
        padded_row_bytes(self.src_w, self.bpp)
    }

    /// The armed one-shot tap.
    #[cfg(target_os = "macos")]
    pub(crate) fn new_metal(
        mint: &crate::metal::resources::MetalResourceDevice,
        src_w: u32,
        src_h: u32,
        format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Result<Self, String> {
        let bpp = capture_bpp(format)
            .map_err(|_| format!("presented snapshot: unsupported format {format:?}"))?;
        let _ = CaptureEncoding::new(format, color_space, sdr_white_scale)
            .map_err(|error| format!("presented snapshot: {error}"))?;
        let size = presented_snapshot_len(src_w, src_h, format)?;
        if size == 0 {
            return Err("presented snapshot: destination has zero area".to_string());
        }
        let size = usize::try_from(size)
            .map_err(|_| "presented snapshot: staging size overflows usize".to_string())?;
        #[cfg(wgpu_arm)]
        let (done_tx, done_rx) = mpsc::channel();
        Ok(Self {
            buffer: SlotBuf::Metal {
                buf: crate::device_layer::DeviceHandle::Metal(mint)
                    .try_create_readback_buffer("aterm-gpu presented snapshot", size as u64)?
                    .into_metal(),
                probe: None,
            },
            #[cfg(wgpu_arm)]
            done_tx,
            #[cfg(wgpu_arm)]
            done_rx,
            state: PresentedFrameState::Armed,
            result: None,
            src_w,
            src_h,
            format,
            capture_encoding: None,
            bpp,
        })
    }

    /// Metal enqueue (see `VideoTap::metal_enqueue_copy`).
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_enqueue_copy(
        &mut self,
        cb: &mut crate::metal::encoder::CommandBuffer<'_>,
        target: &crate::metal::resources::SealedTexture,
        target_format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Result<(), String> {
        if self.state != PresentedFrameState::Armed {
            return Ok(());
        }
        if self.buffer.is_wgpu() {
            self.result = Some(Err(
                "presented snapshot: the renderer armed mid-capture (wgpu staging, \
                 Metal present)"
                    .to_string(),
            ));
            let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
            debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
            return Ok(());
        }
        if target.width() != self.src_w as usize
            || target.height() != self.src_h as usize
            || target_format != self.format
        {
            self.result = Some(Err(format!(
                "presented snapshot: destination changed from {}x{} {:?} to {}x{} {:?}",
                self.src_w,
                self.src_h,
                self.format,
                target.width(),
                target.height(),
                target_format,
            )));
            let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
            debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
            return Ok(());
        }
        let capture_encoding = match CaptureEncoding::new(self.format, color_space, sdr_white_scale)
        {
            Ok(encoding) => encoding,
            Err(error) => {
                self.result = Some(Err(format!("presented snapshot: {error}")));
                let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
                debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
                return Ok(());
            }
        };
        {
            let padded = self.padded_row() as usize;
            let (src_w, src_h) = (self.src_w as usize, self.src_h as usize);
            let (buf, probe) = match &mut self.buffer {
                SlotBuf::Metal { buf, probe } => (buf, probe),
                #[cfg(multi_arm)]
                _ => unreachable!("guarded above"),
            };
            *probe = None;
            cb.copy_texture_to_buffer(target, src_w, src_h, buf.obj(), padded)?;
        }
        self.capture_encoding = Some(capture_encoding);
        let outcome = self.apply_transition(PresentedFrameEvent::EnqueueValid, None);
        debug_assert_eq!(outcome, PresentedFrameOutcome::None);
        Ok(())
    }

    /// Attach the committed Submit B's probe (see `VideoTap::metal_note_submitted`).
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_note_submitted(&mut self, probe: crate::metal::encoder::CbProbe) {
        if self.state == PresentedFrameState::Pending
            && let SlotBuf::Metal { probe: p, .. } = &mut self.buffer
            && p.is_none()
        {
            *p = Some(probe);
        }
    }

    /// Metal harvest: a `Completed` probe copies the shared bytes out; any other terminal outcome is an error.
    #[cfg(target_os = "macos")]
    fn metal_harvest(&mut self, block: bool) {
        let PresentedFrameState::InFlight { t_us } = self.state else {
            return;
        };
        let outcome = {
            let probe = match &self.buffer {
                SlotBuf::Metal { probe, .. } => probe,
                #[cfg(multi_arm)]
                _ => return,
            };
            match probe {
                Some(p) if block => Some(p.wait_terminal()),
                Some(p) => p.try_terminal(),
                None => Some(crate::metal::loss::CbOutcome::Lost {
                    code: None,
                    name: "snapshot copy's command buffer was never committed",
                }),
            }
        };
        let Some(outcome) = outcome else { return };
        let result = if matches!(outcome, crate::metal::loss::CbOutcome::Completed) {
            let len = self.padded_row() as usize * self.src_h as usize;
            let raw = {
                let buf = match &self.buffer {
                    SlotBuf::Metal { buf, .. } => buf,
                    #[cfg(multi_arm)]
                    _ => unreachable!("matched above"),
                };
                // SAFETY: shared storage of exactly `len` bytes; the probe reported the writing command buffer terminal.
                unsafe { crate::metal::ffi::buffer_bytes(buf.obj(), len) }
            };
            RawCapture::new(
                raw,
                self.src_w,
                self.src_h,
                self.padded_row() as usize,
                self.format,
                self.capture_encoding
                    .expect("a pending presented copy freezes its capture encoding"),
                t_us,
            )
        } else {
            Err(format!(
                "presented snapshot: the present's command buffer ended {outcome:?}"
            ))
        };
        let completed_ok = result.is_ok();
        match &mut self.buffer {
            SlotBuf::Metal { probe, .. } => *probe = None,
            #[cfg(multi_arm)]
            _ => {}
        }
        self.result = Some(result);
        let event = if completed_ok {
            PresentedFrameEvent::CompleteMap
        } else {
            PresentedFrameEvent::MapError
        };
        let outcome = self.apply_transition(event, None);
        debug_assert_eq!(
            outcome,
            if completed_ok {
                PresentedFrameOutcome::Frame
            } else {
                PresentedFrameOutcome::Error
            }
        );
    }

    /// The GL one-shot.
    #[cfg(all(gl_arm, not(wgpu_arm)))]
    pub(crate) fn new_gl(
        src_w: u32,
        src_h: u32,
        format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Result<Self, String> {
        let bpp = capture_bpp(format)
            .map_err(|_| format!("presented snapshot: unsupported format {format:?}"))?;
        let _ = CaptureEncoding::new(format, color_space, sdr_white_scale)
            .map_err(|error| format!("presented snapshot: {error}"))?;
        if src_w == 0 || src_h == 0 {
            return Err("presented snapshot: destination has zero area".to_string());
        }
        #[cfg(wgpu_arm)]
        let (done_tx, done_rx) = mpsc::channel();
        Ok(Self {
            buffer: SlotBuf::Gl(Vec::new()),
            #[cfg(wgpu_arm)]
            done_tx,
            #[cfg(wgpu_arm)]
            done_rx,
            state: PresentedFrameState::Armed,
            result: None,
            src_w,
            src_h,
            format,
            capture_encoding: None,
            bpp,
        })
    }

    /// Whether this one-shot stages through the GL arm.
    #[inline]
    #[cfg(gl_arm)]
    pub(crate) const fn tap_is_gl(&self) -> bool {
        self.buffer.is_gl()
    }

    /// GL enqueue (see `VideoTap::gl_enqueue_frame`).
    #[cfg(gl_arm)]
    pub(crate) fn gl_enqueue_frame(
        &mut self,
        (w, h): (u32, u32),
        padded_rgba: &[u8],
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) {
        if self.state != PresentedFrameState::Armed {
            return;
        }
        let refusal = if !self.buffer.is_gl() {
            Some("presented snapshot: the renderer changed arms mid-capture".to_string())
        } else if (w, h) != (self.src_w, self.src_h)
            || self.format != crate::device_layer::TexelFormat::Rgba8Unorm
        {
            Some(format!(
                "presented snapshot: destination changed from {}x{} {:?} to {w}x{h} Rgba8Unorm",
                self.src_w, self.src_h, self.format,
            ))
        } else {
            None
        };
        let capture_encoding = match refusal {
            Some(e) => Err(e),
            None => CaptureEncoding::new(self.format, color_space, sdr_white_scale)
                .map_err(|error| format!("presented snapshot: {error}")),
        };
        let capture_encoding = match capture_encoding {
            Ok(encoding) => encoding,
            Err(e) => {
                self.result = Some(Err(e));
                let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
                debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
                return;
            }
        };
        let len = self.padded_row() as usize * self.src_h as usize;
        match &mut self.buffer {
            SlotBuf::Gl(bytes) => {
                bytes.clear();
                bytes.extend_from_slice(&padded_rgba[..len.min(padded_rgba.len())]);
            }
            #[cfg(multi_arm)]
            _ => unreachable!("guarded above"),
        }
        self.capture_encoding = Some(capture_encoding);
        let outcome = self.apply_transition(PresentedFrameEvent::EnqueueValid, None);
        debug_assert_eq!(outcome, PresentedFrameOutcome::None);
    }

    /// The GL one-shot's post-present hook.
    #[cfg(gl_arm)]
    pub(crate) fn gl_after_present(&mut self, t_us: u64) -> Result<(), String> {
        match self.state {
            PresentedFrameState::Armed => {
                return Err(
                    "presented snapshot: successful present did not enqueue a destination copy"
                        .to_string(),
                );
            }
            PresentedFrameState::Pending => {
                let outcome = self.apply_transition(PresentedFrameEvent::StartMap, Some(t_us));
                debug_assert_eq!(outcome, PresentedFrameOutcome::None);
                let raw = match &mut self.buffer {
                    SlotBuf::Gl(bytes) => std::mem::take(bytes),
                    #[cfg(multi_arm)]
                    _ => Vec::new(),
                };
                let result = RawCapture::new(
                    raw,
                    self.src_w,
                    self.src_h,
                    self.padded_row() as usize,
                    self.format,
                    self.capture_encoding
                        .expect("a pending presented copy freezes its capture encoding"),
                    t_us,
                );
                let ok = result.is_ok();
                self.result = Some(result);
                let event = if ok {
                    PresentedFrameEvent::CompleteMap
                } else {
                    PresentedFrameEvent::MapError
                };
                let _ = self.apply_transition(event, None);
            }
            PresentedFrameState::InFlight { .. } | PresentedFrameState::Complete => {}
        }
        self.result_status()
    }

    /// [`Self::finish`] for a GL one-shot.
    #[cfg(gl_arm)]
    pub(crate) fn gl_finish(&mut self) -> Result<(), String> {
        match self.state {
            PresentedFrameState::Armed => {
                Err("presented snapshot: no successful present was captured".to_string())
            }
            PresentedFrameState::Pending => Err(
                "presented snapshot: after-present hook was not called before finish".to_string(),
            ),
            PresentedFrameState::InFlight { .. } | PresentedFrameState::Complete => {
                self.result_status()
            }
        }
    }

    const fn apply_transition(
        &mut self,
        event: PresentedFrameEvent,
        t_us: Option<u64>,
    ) -> PresentedFrameOutcome {
        let decision = presented_frame_transition(self.state.phase(), event)
            .expect("presented-frame lifecycle event is enabled");
        self.state = match decision.phase {
            PresentedFramePhase::Armed => PresentedFrameState::Armed,
            PresentedFramePhase::Pending => PresentedFrameState::Pending,
            PresentedFramePhase::InFlight => PresentedFrameState::InFlight {
                t_us: t_us.expect("starting a presented-frame map freezes its timestamp"),
            },
            PresentedFramePhase::Complete => PresentedFrameState::Complete,
        };
        decision.outcome
    }

    /// Append this one-shot copy to the encoder that rendered the destination.
    #[cfg(wgpu_arm)]
    #[must_use = "submit the encoder with device_layer::submit_host_readable naming this buffer"]
    pub(crate) fn enqueue_copy(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame_tex: &wgpu::Texture,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Option<wgpu::Buffer> {
        if self.state != PresentedFrameState::Armed {
            return None;
        }
        if frame_tex.width() != self.src_w
            || frame_tex.height() != self.src_h
            || crate::device_layer::TexelFormat::from_wgpu(frame_tex.format()) != Some(self.format)
        {
            self.result = Some(Err(format!(
                "presented snapshot: destination changed from {}x{} {:?} to {}x{} {:?}",
                self.src_w,
                self.src_h,
                self.format,
                frame_tex.width(),
                frame_tex.height(),
                frame_tex.format(),
            )));
            let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
            debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
            return None;
        }
        let capture_encoding = match CaptureEncoding::new(self.format, color_space, sdr_white_scale)
        {
            Ok(encoding) => encoding,
            Err(error) => {
                self.result = Some(Err(format!("presented snapshot: {error}")));
                let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
                debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
                return None;
            }
        };
        #[cfg(gl_arm)]
        if self.buffer.is_gl() {
            self.result = Some(Err(
                "presented snapshot: the renderer changed arms mid-capture (GL staging, \
                 wgpu present)"
                    .to_string(),
            ));
            let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
            debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
            return None;
        }
        #[cfg(target_os = "macos")]
        if matches!(self.buffer, SlotBuf::Metal { .. }) {
            self.result = Some(Err(
                "presented snapshot: the renderer disarmed mid-capture (Metal staging, \
                 wgpu present)"
                    .to_string(),
            ));
            let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
            debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
            return None;
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: frame_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: self.buffer.wgpu(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row() as u32),
                    rows_per_image: Some(self.src_h),
                },
            },
            wgpu::Extent3d {
                width: self.src_w,
                height: self.src_h,
                depth_or_array_layers: 1,
            },
        );
        self.capture_encoding = Some(capture_encoding);
        let outcome = self.apply_transition(PresentedFrameEvent::EnqueueValid, None);
        debug_assert_eq!(outcome, PresentedFrameOutcome::None);
        Some(self.buffer.wgpu().clone())
    }

    /// Stamp and map the copy belonging to the successful present.
    #[cfg(wgpu_arm)]
    pub(crate) fn after_present(&mut self, device: &wgpu::Device, t_us: u64) -> Result<(), String> {
        match self.state {
            PresentedFrameState::Armed => {
                return Err(
                    "presented snapshot: successful present did not enqueue a destination copy"
                        .to_string(),
                );
            }
            PresentedFrameState::Pending => {
                match &self.buffer {
                    SlotBuf::Wgpu(buffer) => {
                        let tx = self.done_tx.clone();
                        #[expect(
                            clippy::disallowed_methods,
                            reason = "the present that filled this buffer was submitted by \
                                      device_layer::submit_host_readable (enqueue_copy's buffer)"
                        )]
                        buffer
                            .slice(..)
                            .map_async(wgpu::MapMode::Read, move |result| {
                                let _ = tx.send(result.is_ok());
                            });
                    }
                    #[cfg(target_os = "macos")]
                    SlotBuf::Metal { .. } => {}
                    #[cfg(gl_arm)]
                    SlotBuf::Gl(_) => {}
                }
                let outcome = self.apply_transition(PresentedFrameEvent::StartMap, Some(t_us));
                debug_assert_eq!(outcome, PresentedFrameOutcome::None);
            }
            PresentedFrameState::InFlight { .. } | PresentedFrameState::Complete => {}
        }
        let _ = device.poll(wgpu::PollType::Poll);
        self.harvest_ready();
        #[cfg(target_os = "macos")]
        self.metal_harvest(false);
        self.result_status()
    }

    /// Whether this one-shot stages through the armed (Metal) arm.
    #[cfg(target_os = "macos")]
    pub(crate) const fn tap_is_metal(&self) -> bool {
        !self.buffer.is_wgpu()
    }

    /// [`Self::after_present`] with no wgpu device to poll.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_after_present(&mut self, t_us: u64) -> Result<(), String> {
        match self.state {
            PresentedFrameState::Armed => {
                return Err(
                    "presented snapshot: successful present did not enqueue a destination copy"
                        .to_string(),
                );
            }
            PresentedFrameState::Pending => {
                let outcome = self.apply_transition(PresentedFrameEvent::StartMap, Some(t_us));
                debug_assert_eq!(outcome, PresentedFrameOutcome::None);
            }
            PresentedFrameState::InFlight { .. } | PresentedFrameState::Complete => {}
        }
        self.metal_harvest(false);
        self.result_status()
    }

    /// [`Self::finish`] for a Metal one-shot.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_finish(&mut self) -> Result<(), String> {
        match self.state {
            PresentedFrameState::Armed => {
                return Err("presented snapshot: no successful present was captured".to_string());
            }
            PresentedFrameState::Pending => {
                return Err(
                    "presented snapshot: after-present hook was not called before finish"
                        .to_string(),
                );
            }
            PresentedFrameState::InFlight { .. } => {
                self.metal_harvest(true);
            }
            PresentedFrameState::Complete => {}
        }
        self.result_status()
    }

    #[cfg(wgpu_arm)]
    fn harvest_ready(&mut self) {
        let Ok(mapped) = self.done_rx.try_recv() else {
            return;
        };
        let PresentedFrameState::InFlight { t_us } = self.state else {
            return;
        };
        let result = if mapped {
            let raw = self.buffer.wgpu().slice(..).get_mapped_range().to_vec();
            RawCapture::new(
                raw,
                self.src_w,
                self.src_h,
                self.padded_row() as usize,
                self.format,
                self.capture_encoding
                    .expect("a pending presented copy freezes its capture encoding"),
                t_us,
            )
        } else {
            Err("presented snapshot: staging-buffer map failed".to_string())
        };
        let completed_ok = result.is_ok();
        self.buffer.wgpu().unmap();
        self.result = Some(result);
        let event = if completed_ok {
            PresentedFrameEvent::CompleteMap
        } else {
            PresentedFrameEvent::MapError
        };
        let outcome = self.apply_transition(event, None);
        debug_assert_eq!(
            outcome,
            if completed_ok {
                PresentedFrameOutcome::Frame
            } else {
                PresentedFrameOutcome::Error
            }
        );
    }

    fn result_status(&self) -> Result<(), String> {
        match self.result.as_ref() {
            Some(Err(error)) => Err(error.clone()),
            Some(Ok(_)) | None => Ok(()),
        }
    }

    /// Blocking completion, called only after the explicit capture leaves the present hot path.
    #[cfg(wgpu_arm)]
    pub(crate) fn finish(&mut self, device: &wgpu::Device) -> Result<(), String> {
        match self.state {
            PresentedFrameState::Armed => {
                return Err("presented snapshot: no successful present was captured".to_string());
            }
            PresentedFrameState::Pending => {
                return Err(
                    "presented snapshot: after-present hook was not called before finish"
                        .to_string(),
                );
            }
            PresentedFrameState::InFlight { .. } => {
                #[cfg(target_os = "macos")]
                if matches!(self.buffer, SlotBuf::Metal { .. }) {
                    self.metal_harvest(true);
                    return self.result_status();
                }
                if let Err(error) = device.poll(wgpu::PollType::wait_indefinitely()) {
                    self.buffer.wgpu().unmap();
                    self.result = Some(Err(format!(
                        "presented snapshot: GPU completion wait failed: {error}"
                    )));
                    let outcome = self.apply_transition(PresentedFrameEvent::MapError, None);
                    debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
                } else {
                    self.harvest_ready();
                    if self.state != PresentedFrameState::Complete {
                        self.buffer.wgpu().unmap();
                        self.result = Some(Err(
                            "presented snapshot: map callback did not complete".to_string(),
                        ));
                        let outcome = self.apply_transition(PresentedFrameEvent::MapError, None);
                        debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
                    }
                }
            }
            PresentedFrameState::Complete => {}
        }
        self.result_status()
    }

    /// Consume the completed tap and return its exact destination, as mapped.
    pub(crate) fn take_raw(self) -> Result<RawCapture, String> {
        match self.result {
            Some(result) => result,
            None => Err(match self.state {
                PresentedFrameState::Armed => {
                    "presented snapshot: no successful present was captured".to_string()
                }
                PresentedFrameState::Pending => {
                    "presented snapshot: after-present hook was not called".to_string()
                }
                PresentedFrameState::InFlight { .. } => {
                    "presented snapshot: finish was not called".to_string()
                }
                PresentedFrameState::Complete => {
                    "presented snapshot: completed without a result".to_string()
                }
            }),
        }
    }
}

/// IEEE 754 half → f32, local so this module needs no renderer helper.
const fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x3ff) as u32;
    let f = match (exp, frac) {
        (0, 0) => sign << 31,
        (0, _) => {
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

/// The 8-bit sRGB encode of a linear channel: clamp to `[0, 1]`, apply the OETF, and round half up.
fn srgb8_from_linear_powf(linear: f32) -> u32 {
    (linear_to_srgb(linear.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32
}

/// Entry `k - 1` is the least `f32` whose [`srgb8_from_linear_powf`] is at least `k`, for `k` in `1..=255`.
static SRGB8_ENCODE_THRESHOLDS: LazyLock<[f32; 255]> = LazyLock::new(|| {
    std::array::from_fn(|index| {
        let byte = index as u32 + 1;
        let (mut lo, mut hi) = (0.0_f32.to_bits(), 1.0_f32.to_bits());
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if srgb8_from_linear_powf(f32::from_bits(mid)) >= byte {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        f32::from_bits(hi)
    })
});

/// [`srgb8_from_linear_powf`] read from [`SRGB8_ENCODE_THRESHOLDS`].
#[inline]
fn srgb8_from_linear(linear: f32) -> u32 {
    srgb8_by_thresholds(&SRGB8_ENCODE_THRESHOLDS, linear)
}

/// Number of `thresholds` at or below `linear`, by an eight-step binary search of the sorted table.
#[inline]
fn srgb8_by_thresholds(thresholds: &[f32; 255], linear: f32) -> u32 {
    let (mut below, mut step) = (0_usize, 128_usize);
    while step > 0 {
        below += step * usize::from(linear >= thresholds[below + step - 1]);
        step /= 2;
    }
    below as u32
}

/// The CSS Color 4 Display-P3-to-linear-sRGB matrix, on linear channels.
fn display_p3_linear_to_srgb_linear([r, g, b]: [f32; 3]) -> [f32; 3] {
    [
        1.224_940_2_f32.mul_add(r, -0.224_940_18 * g),
        (-0.042_056_955_f32).mul_add(r, 1.042_057 * g),
        (-0.019_637_555_f32).mul_add(r, (-0.078_636_05_f32).mul_add(g, 1.098_273_6 * b)),
    ]
}

/// Convert 8-bit Display-P3 coordinates to sRGB via the Display-P3-to-linear-sRGB matrix.
fn display_p3_to_srgb8(p3: [u8; 3]) -> [u8; 3] {
    display_p3_linear_to_srgb_linear(p3.map(srgb8_to_linear))
        .map(|channel| srgb8_from_linear(channel) as u8)
}

/// Tone-map scRGB/EDR to SDR-linear by removing `sdr_white_scale` and compressing over-white channels by peak.
fn tone_map_scrgb_to_sdr(scrgb: [f32; 3], sdr_white_scale: f32) -> [f32; 3] {
    let mut linear = scrgb.map(|channel| {
        let bounded = if channel.is_nan() || channel <= 0.0 {
            0.0
        } else if channel.is_infinite() {
            f32::MAX
        } else {
            channel
        };
        bounded / sdr_white_scale
    });
    let peak = linear[0].max(linear[1]).max(linear[2]);
    if peak > 1.0 {
        linear = linear.map(|channel| channel / peak);
    }
    linear
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_render::srgb_to_linear;

    /// Device-free tap for the store and gate laws; `budget_bytes` is taken without the 16 MiB floor.
    fn test_tap(budget_bytes: usize, fps_cap: Option<u32>) -> VideoTap {
        #[cfg(wgpu_arm)]
        let (done_tx, done_rx) = mpsc::channel();
        VideoTap {
            slots: Vec::new(),
            #[cfg(wgpu_arm)]
            done_tx,
            #[cfg(wgpu_arm)]
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
            epoch: aterm_time::Instant::now(),
            seq: 0,
            half_res: false,
            src_w: 2,
            src_h: 2,
            format: crate::device_layer::TexelFormat::Rgba8Unorm,
            first_capture_encoding: None,
            mixed_capture_encoding: false,
            bpp: 4,
            resized_early_stop: false,
            #[cfg(not(target_arch = "wasm32"))]
            worker: None,
        }
    }

    fn padded_fixture(rows: &[&[u8]], stride: usize) -> Vec<u8> {
        let mut out = vec![0u8; rows.len() * stride];
        for (row, bytes) in rows.iter().enumerate() {
            out[row * stride..row * stride + bytes.len()].copy_from_slice(bytes);
        }
        out
    }

    fn half_pixel(channels: [u16; 4]) -> Vec<u8> {
        channels.into_iter().flat_map(u16::to_le_bytes).collect()
    }

    fn encoding(
        format: crate::device_layer::TexelFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> CaptureEncoding {
        CaptureEncoding::new(format, color_space, sdr_white_scale).unwrap()
    }

    /// The worker's conversion matches inline [`mapped_to_rgba8`], passes stamps through, and exits on sender drop.
    #[test]
    fn convert_worker_matches_inline_conversion_and_drains_on_drop() {
        let stride = 256;
        let sdr_rows: [&[u8]; 2] = [
            &[1, 2, 3, 255, 9, 8, 7, 128],
            &[0, 0, 0, 0, 255, 255, 255, 255],
        ];
        let sdr_raw = padded_fixture(&sdr_rows, stride);
        let sdr_enc = encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        );
        let edr_pixels: Vec<u8> = [
            half_pixel([0x3C00, 0x0000, 0x0000, 0x3C00]),
            half_pixel([0x4400, 0x4400, 0x4400, 0x3C00]),
            half_pixel([0x0000, 0x3800, 0x0000, 0x3C00]),
            half_pixel([0x0000, 0x0000, 0x3C00, 0x3800]),
        ]
        .concat();
        let edr_rows: [&[u8]; 2] = [&edr_pixels[..16], &edr_pixels[16..]];
        let edr_raw = padded_fixture(&edr_rows, stride);
        let edr_enc = encoding(
            crate::device_layer::TexelFormat::Rgba16Float,
            CaptureColorSpace::ExtendedLinearSrgb,
            1.0,
        );
        for (label, raw, format, enc, half_res) in [
            (
                "sdr",
                sdr_raw,
                crate::device_layer::TexelFormat::Rgba8Unorm,
                sdr_enc,
                false,
            ),
            (
                "edr",
                edr_raw,
                crate::device_layer::TexelFormat::Rgba16Float,
                edr_enc,
                true,
            ),
        ] {
            let (inline_rgba, inline_w, inline_h) =
                mapped_to_rgba8(&raw, 2, 2, stride, format, enc, half_res)
                    .expect("inline conversion");
            let worker = spawn_convert_worker(2, 2, stride, format, half_res)
                .expect("native test hosts spawn threads");
            worker
                .job_tx
                .send(ConvertJob {
                    raw,
                    seq: 7,
                    t_us: 42,
                    capture_encoding: enc,
                })
                .expect("worker alive");
            let frame = worker
                .result_rx
                .recv()
                .expect("worker answers")
                .expect("conversion succeeds");
            assert_eq!(
                (frame.seq, frame.t_us, frame.w, frame.h),
                (7, 42, inline_w, inline_h),
                "{label}: stamps and geometry ride through the worker untouched"
            );
            assert_eq!(
                frame.rgba, inline_rgba,
                "{label}: the off-thread harvest must be byte-identical to inline"
            );
            drop(worker.job_tx);
            worker
                .handle
                .join()
                .expect("worker exits when the tap drops its sender");
        }
    }

    /// Every span [`RawCapture::rgba_row`] appends matches the corresponding slice of [`RawCapture::convert`].
    #[test]
    fn raw_capture_rows_are_its_conversion() {
        use crate::device_layer::TexelFormat::{Bgra8Unorm, Rgba8Unorm, Rgba16Float};
        let (width, height, stride) = (5u32, 3u32, 256usize);
        let bytes: Vec<u8> = (0..stride * height as usize)
            .map(|i| (i * 89 % 251) as u8)
            .collect();
        for (format, space, scale) in [
            (Bgra8Unorm, CaptureColorSpace::Srgb, 1.0),
            (Rgba8Unorm, CaptureColorSpace::Srgb, 1.0),
            (Rgba8Unorm, CaptureColorSpace::DisplayP3, 1.0),
            (Rgba16Float, CaptureColorSpace::ExtendedLinearSrgb, 2.5),
        ] {
            let label = format!("{format:?} {space:?}");
            let enc = encoding(format, space, scale);
            let raw = RawCapture::new(bytes.clone(), width, height, stride, format, enc, 9)
                .expect("a whole destination");
            let mut rows = Vec::new();
            for y in 0..height as usize {
                for (x, span) in [(0, 5), (1, 3), (4, 1), (2, 0)] {
                    rows.clear();
                    raw.rgba_row(y, x, span, &mut rows);
                    rows.push(0xEE);
                    let (whole, _, _) =
                        mapped_to_rgba8(&bytes, width, height, stride, format, enc, false)
                            .expect("converts");
                    let start = (y * width as usize + x) * 4;
                    let mut expected = whole[start..start + span * 4].to_vec();
                    expected.push(0xEE);
                    assert_eq!(rows, expected, "{label}: row {y} from {x} for {span}");
                }
                rows.clear();
                raw.rgba_row(y, 3, 3, &mut rows);
                assert!(rows.is_empty(), "{label}: a span past the right edge");
            }
            raw.rgba_row(height as usize, 0, 1, &mut rows);
            assert!(rows.is_empty(), "{label}: a row past the bottom");
            let converted = raw.convert().expect("converts");
            let (whole, w, h) = mapped_to_rgba8(&bytes, width, height, stride, format, enc, false)
                .expect("converts");
            assert_eq!(
                (converted.seq, converted.t_us, converted.w, converted.h),
                (1, 9, w, h),
                "{label}"
            );
            assert_eq!(converted.rgba, whole, "{label}");

            let bpp = capture_bpp(format).unwrap() as usize;
            let short = bytes[..(height as usize - 1) * stride + width as usize * bpp - 1].to_vec();
            assert!(
                RawCapture::new(short, width, height, stride, format, enc, 9).is_err(),
                "{label}: a short last row is refused"
            );
        }
    }

    /// The `should_capture` gate at fps=10 (100 ms spacing); rejections count only `decimated`.
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

    /// Device-free format contract: both byte-order variants retain source alpha while BGRA swizzles RGB.
    #[test]
    fn mapped_sdr_formats_preserve_alpha() {
        let rgba_source = [10, 20, 30, 0, 40, 50, 60, 173];
        let (rgba, w, h) = mapped_to_rgba8(
            &rgba_source,
            2,
            1,
            8,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            false,
        )
        .unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba, rgba_source);

        let bgra_source = [30, 20, 10, 7, 60, 50, 40, 241];
        let (rgba, _, _) = mapped_to_rgba8(
            &bgra_source,
            2,
            1,
            8,
            crate::device_layer::TexelFormat::Bgra8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Bgra8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            false,
        )
        .unwrap();
        assert_eq!(rgba, [10, 20, 30, 7, 40, 50, 60, 241]);
    }

    /// f16 RGB converts from extended-linear to sRGB8 while alpha remains linear coverage.
    #[test]
    fn mapped_f16_preserves_linear_alpha() {
        let mut source = half_pixel([0x3c00, 0x3800, 0x0000, 0x3800]);
        source.extend(half_pixel([0x0000, 0x3c00, 0x3800, 0x3400]));
        let (rgba, w, h) = mapped_to_rgba8(
            &source,
            2,
            1,
            16,
            crate::device_layer::TexelFormat::Rgba16Float,
            encoding(
                crate::device_layer::TexelFormat::Rgba16Float,
                CaptureColorSpace::ExtendedLinearSrgb,
                1.0,
            ),
            false,
        )
        .unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba, [255, 188, 0, 128, 0, 255, 188, 64]);
    }

    /// The 2×2 downscale averages alpha across every supported format on padded rows.
    #[test]
    fn half_res_preserves_alpha_for_every_format() {
        let rgba_rows: [&[u8]; 2] = [
            &[40, 50, 60, 0, 40, 50, 60, 64],
            &[40, 50, 60, 128, 40, 50, 60, 255],
        ];
        let rgba_source = padded_fixture(&rgba_rows, 256);
        let (rgba, w, h) = mapped_to_rgba8(
            &rgba_source,
            2,
            2,
            256,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgba, [40, 50, 60, 112]);

        let bgra_rows: [&[u8]; 2] = [
            &[60, 50, 40, 0, 60, 50, 40, 64],
            &[60, 50, 40, 128, 60, 50, 40, 255],
        ];
        let bgra_source = padded_fixture(&bgra_rows, 256);
        let (bgra, _, _) = mapped_to_rgba8(
            &bgra_source,
            2,
            2,
            256,
            crate::device_layer::TexelFormat::Bgra8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Bgra8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!(bgra, [40, 50, 60, 112]);

        let mut f16_row0 = half_pixel([0x0000, 0x0000, 0x0000, 0x0000]);
        f16_row0.extend(half_pixel([0x0000, 0x0000, 0x0000, 0x3800]));
        let mut f16_row1 = half_pixel([0x0000, 0x0000, 0x0000, 0x3c00]);
        f16_row1.extend(half_pixel([0x0000, 0x0000, 0x0000, 0x3400]));
        let f16_source = padded_fixture(&[&f16_row0, &f16_row1], 256);
        let (f16, _, _) = mapped_to_rgba8(
            &f16_source,
            2,
            2,
            256,
            crate::device_layer::TexelFormat::Rgba16Float,
            encoding(
                crate::device_layer::TexelFormat::Rgba16Float,
                CaptureColorSpace::ExtendedLinearSrgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!(f16, [0, 0, 0, 112]);
    }

    #[test]
    fn half_res_filters_linear_premultiplied_rgb() {
        let transparent_blue = [0, 0, 255, 0];
        let opaque_red = [255, 0, 0, 255];
        let rgba_source = padded_fixture(
            &[
                &[opaque_red, transparent_blue].concat(),
                &[opaque_red, transparent_blue].concat(),
            ],
            256,
        );
        let (rgba, _, _) = mapped_to_rgba8(
            &rgba_source,
            2,
            2,
            256,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!(rgba, [255, 0, 0, 128]);
        assert_ne!(
            rgba,
            [127, 0, 127, 128],
            "negative control: transparent blue must not create a purple fringe"
        );

        let black = [0, 0, 0, 255];
        let white = [255, 255, 255, 255];
        let grayscale = padded_fixture(&[&[black, white].concat(), &[black, white].concat()], 256);
        let (rgba, _, _) = mapped_to_rgba8(
            &grayscale,
            2,
            2,
            256,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!(rgba, [188, 188, 188, 255]);
        assert_ne!(
            rgba,
            [127, 127, 127, 255],
            "negative control: gamma-space averaging darkens black/white"
        );

        let low_alpha =
            padded_fixture(&[&[1, 2, 3, 1, 1, 2, 3, 1], &[1, 2, 3, 1, 1, 2, 3, 0]], 256);
        let (rgba, _, _) = mapped_to_rgba8(
            &low_alpha,
            2,
            2,
            256,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!(rgba[3], 1);
    }

    #[test]
    fn half_res_retains_odd_edges_and_one_pixel_axes() {
        let black = [0, 0, 0, 255];
        let green = [0, 255, 0, 255];
        let rows = [
            &[black, black, black].concat(),
            &[black, black, black].concat(),
            &[black, black, green].concat(),
        ];
        let source = padded_fixture(&[rows[0], rows[1], rows[2]], 256);
        let (rgba, w, h) = mapped_to_rgba8(
            &source,
            3,
            3,
            256,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&rgba[12..16], &green);

        let one = [9, 8, 7, 3];
        let (rgba, w, h) = mapped_to_rgba8(
            &one,
            1,
            1,
            4,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgba, one);
    }

    /// The Windows scRGB multiplier is presentation metadata, not scene exposure.
    #[test]
    fn scrgb_capture_undoes_sdr_white_scale_before_encoding() {
        let source = half_pixel([0x3c00, 0x3800, 0x0000, 0x3800]);
        let decode = |scale| {
            mapped_to_rgba8(
                &source,
                1,
                1,
                8,
                crate::device_layer::TexelFormat::Rgba16Float,
                encoding(
                    crate::device_layer::TexelFormat::Rgba16Float,
                    CaptureColorSpace::ExtendedLinearSrgb,
                    scale,
                ),
                false,
            )
            .unwrap()
            .0
        };
        assert_eq!(decode(2.0), [188, 137, 0, 128]);
        assert_eq!(
            decode(1.0),
            [255, 188, 0, 128],
            "negative control: treating scaled scRGB as unit-linear clips the red channel"
        );
    }

    /// Over-white channels are compressed together, retaining their ratios instead of independent clamping.
    #[test]
    fn scrgb_highlight_tone_map_is_bounded_and_chromatic() {
        let source = half_pixel([0x4400, 0x4000, 0x3c00, 0x3400]);
        let (rgba, _, _) = mapped_to_rgba8(
            &source,
            1,
            1,
            8,
            crate::device_layer::TexelFormat::Rgba16Float,
            encoding(
                crate::device_layer::TexelFormat::Rgba16Float,
                CaptureColorSpace::ExtendedLinearSrgb,
                2.0,
            ),
            false,
        )
        .unwrap();
        assert_eq!(rgba, [255, 188, 137, 64]);
        assert_ne!(
            rgba,
            [255, 255, 188, 64],
            "negative control: direct channel clamp destroys highlight chromaticity"
        );
    }

    /// P3-tagged coordinates must become actual sRGB before an unprofiled PNG consumes them.
    #[test]
    fn display_p3_capture_converts_primaries_and_srgb_is_byte_exact() {
        let source = [128, 255, 0, 73];
        let p3 = mapped_to_rgba8(
            &source,
            1,
            1,
            4,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::DisplayP3,
                1.0,
            ),
            false,
        )
        .unwrap()
        .0;
        assert_eq!(p3, [56, 255, 0, 73]);

        let srgb = mapped_to_rgba8(
            &source,
            1,
            1,
            4,
            crate::device_layer::TexelFormat::Rgba8Unorm,
            encoding(
                crate::device_layer::TexelFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            false,
        )
        .unwrap()
        .0;
        assert_eq!(srgb, source);
        assert_ne!(p3, srgb, "negative control: P3 cannot be passed through");

        assert_eq!(display_p3_to_srgb8([0, 0, 0]), [0, 0, 0]);
        assert_eq!(display_p3_to_srgb8([255, 255, 255]), [255, 255, 255]);
    }

    /// The ulps on each side of a threshold that are walked one by one.
    const THRESHOLD_WINDOW_ULPS: u32 = 4096;

    /// Where `thresholds` departs from the encode's definition near its own entries, if it does.
    fn threshold_table_error(thresholds: &[f32; 255]) -> Option<String> {
        for (index, &threshold) in thresholds.iter().enumerate() {
            let byte = index as u32 + 1;
            let bits = threshold.to_bits();
            let below = f32::from_bits(bits - 1);
            if srgb8_from_linear_powf(below) != byte - 1
                || srgb8_from_linear_powf(threshold) != byte
            {
                return Some(format!(
                    "threshold {byte} at {threshold:e} is not the first f32 of its byte"
                ));
            }
            let mut previous = 0;
            for pattern in bits - THRESHOLD_WINDOW_ULPS..=bits + THRESHOLD_WINDOW_ULPS {
                let linear = f32::from_bits(pattern);
                let exact = srgb8_from_linear_powf(linear);
                if exact < previous {
                    return Some(format!("the encode decreases at {linear:e}"));
                }
                let read = srgb8_by_thresholds(thresholds, linear);
                if read != exact {
                    return Some(format!(
                        "the table reads {read} at {linear:e}, the encode {exact}"
                    ));
                }
                previous = exact;
            }
        }
        None
    }

    /// `srgb8_from_linear_powf` before its floor, in both `f32` and `f64` over the same `f32` constants.
    fn srgb8_unrounded(linear: f32) -> (f64, f64) {
        let clamped = linear.clamp(0.0, 1.0);
        let in_f32 = f64::from(linear_to_srgb(clamped) * 255.0 + 0.5);
        let x = f64::from(clamped);
        let encoded = if clamped <= 0.003_130_8 {
            f64::from(12.92_f32) * x
        } else {
            f64::from(1.055_f32) * x.powf(f64::from(1.0_f32 / 2.4)) - f64::from(0.055_f32)
        };
        (in_f32, encoded * 255.0 + 0.5)
    }

    /// The table matches the definition for every `f32` across each threshold window.
    #[test]
    fn encode_thresholds_are_exact_and_locally_monotone() {
        let thresholds = &*SRGB8_ENCODE_THRESHOLDS;
        assert_eq!(threshold_table_error(thresholds), None);
        assert!(
            thresholds.windows(2).all(|pair| pair[0] < pair[1]),
            "the thresholds ascend"
        );
        for (index, &threshold) in thresholds.iter().enumerate() {
            let byte = f64::from(index as u32 + 1);
            let bits = threshold.to_bits();
            let edge = |pattern: u32| srgb8_unrounded(f32::from_bits(pattern)).1;
            let margin = (byte - edge(bits - THRESHOLD_WINDOW_ULPS))
                .min(edge(bits + THRESHOLD_WINDOW_ULPS) - byte);
            let stray = (bits - THRESHOLD_WINDOW_ULPS..=bits + THRESHOLD_WINDOW_ULPS)
                .map(|pattern| {
                    let (in_f32, exact) = srgb8_unrounded(f32::from_bits(pattern));
                    (in_f32 - exact).abs()
                })
                .fold(0.0, f64::max);
            assert!(
                margin > 16.0 * stray,
                "byte {byte}: margin {margin:e} against an f32 stray of {stray:e}"
            );
        }
        assert_eq!(
            srgb8_from_linear(f32::NAN),
            srgb8_from_linear_powf(f32::NAN)
        );
        for linear in [f32::NEG_INFINITY, -1.0, -0.0, 0.0, 1.0, 2.0, f32::INFINITY] {
            assert_eq!(
                srgb8_from_linear(linear),
                srgb8_from_linear_powf(linear),
                "{linear}"
            );
        }

        let mut late = *thresholds;
        late[127] = f32::from_bits(late[127].to_bits() + 1);
        assert!(
            threshold_table_error(&late).is_some(),
            "negative control: an entry one ulp late is caught"
        );
        let mut early = *thresholds;
        early[0] = f32::from_bits(early[0].to_bits() - 1);
        assert!(
            threshold_table_error(&early).is_some(),
            "negative control: an entry one ulp early is caught"
        );
    }

    /// The decode table holds `srgb_to_linear` bit for bit and every P3 triple encodes alike by table and `powf`.
    #[test]
    fn display_p3_lut_matches_powf_over_every_input() {
        let decoded: [f32; 256] = std::array::from_fn(|byte| {
            let by_powf = srgb_to_linear(byte as f32 / 255.0);
            assert_eq!(
                srgb8_to_linear(byte as u8).to_bits(),
                by_powf.to_bits(),
                "decode of {byte}"
            );
            by_powf
        });
        let decoded = &decoded;
        std::thread::scope(|scope| {
            for eighth in 0..8_u8 {
                scope.spawn(move || {
                    for r in eighth * 32..=eighth * 32 + 31 {
                        for g in 0..=u8::MAX {
                            for b in 0..=u8::MAX {
                                let p3 = [r, g, b];
                                let linear = display_p3_linear_to_srgb_linear(
                                    p3.map(|channel| decoded[usize::from(channel)]),
                                );
                                let by_powf =
                                    linear.map(|channel| srgb8_from_linear_powf(channel) as u8);
                                assert_eq!(display_p3_to_srgb8(p3), by_powf, "{p3:?}");
                            }
                        }
                    }
                });
            }
        });
    }

    #[test]
    fn f16_lut_matches_powf() {
        let agree = |linear: [f32; 3]| {
            for channel in linear {
                assert_eq!(
                    srgb8_from_linear(channel),
                    srgb8_from_linear_powf(channel),
                    "{channel:e}"
                );
            }
        };
        for bits in 0..=u16::MAX {
            agree(tone_map_scrgb_to_sdr([f16_to_f32(bits); 3], 1.0));
        }
        let mut state = 0x5eed_u64;
        for _ in 0..1_000_000 {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut word = state;
            word = (word ^ (word >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            word = (word ^ (word >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            word ^= word >> 31;
            let scrgb = [0, 16, 32].map(|shift| f16_to_f32((word >> shift) as u16));
            let sdr_white_scale = 1.0 + f32::from((word >> 48) as u16) / 8192.0;
            agree(tone_map_scrgb_to_sdr(scrgb, sdr_white_scale));
        }
    }

    #[test]
    fn capture_encoding_rejects_format_space_mismatch_and_bad_scale() {
        assert!(
            CaptureEncoding::new(
                crate::device_layer::TexelFormat::Rgba16Float,
                CaptureColorSpace::Srgb,
                1.0,
            )
            .is_err()
        );
        assert!(
            CaptureEncoding::new(
                crate::device_layer::TexelFormat::Bgra8Unorm,
                CaptureColorSpace::ExtendedLinearSrgb,
                1.0,
            )
            .is_err()
        );
        assert!(
            CaptureEncoding::new(
                crate::device_layer::TexelFormat::Rgba16Float,
                CaptureColorSpace::ExtendedLinearSrgb,
                f32::NAN,
            )
            .is_err()
        );
        assert!(
            CaptureEncoding::new(
                crate::device_layer::TexelFormat::Bgra8Unorm,
                CaptureColorSpace::Unknown,
                1.0,
            )
            .is_err()
        );
    }

    /// A recording whose surface encoding changes mid-capture keeps the first encoding and marks the recording mixed.
    #[test]
    fn a_mid_recording_encoding_change_marks_the_recording_mixed() {
        let srgb = encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        );
        let p3 = encoding(
            crate::device_layer::TexelFormat::Rgba8Unorm,
            CaptureColorSpace::DisplayP3,
            1.0,
        );
        let mut tap = test_tap(usize::MAX, None);
        tap.note_capture_encoding(srgb);
        assert!(!tap.mixed_capture_encoding);
        tap.note_capture_encoding(p3);
        assert_eq!(tap.first_capture_encoding, Some(srgb));
        assert!(tap.mixed_capture_encoding);
    }

    #[test]
    fn invalid_live_metadata_counts_only_accepted_fps_opportunities() {
        let mut tap = test_tap(usize::MAX, Some(10));
        for now_us in [0, 10_000, 50_000, 100_000] {
            assert!(
                tap.capture_encoding_for_present(now_us, CaptureColorSpace::Unknown, 1.0)
                    .is_none()
            );
        }
        assert_eq!(tap.dropped, 2, "t=0 and t=100ms were accepted then lost");
        assert_eq!(
            tap.decimated, 2,
            "t=10ms and t=50ms were client-requested decimation, not losses"
        );
    }
}
