// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! VIDEO introspection capture core: record the swapchain-destination frames
//! submitted to `present()`, timestamped, so an AI driving aterm can inspect
//! temporal renderer output (cursor-trail smoothness, per-keystroke flashes,
//! and input→present-return latency) instead of relying on single-frame
//! captures. This tap does not observe compositor selection, scanout, or photons.
//!
//! WHY THE SWAPCHAIN and not the offscreen: the offscreen readbacks (`image`
//! verb, `present_input_readback`) are byte-exact for the GRID but structurally
//! miss the swapchain-only passes — the EDR aurora, the SDR crown, the bell
//! invert / drop-overlay / letterbox chrome baked into the blit. This tap copies
//! `frame.texture` itself, in the SAME encoder as the present pass, after the
//! last swapchain draw — the exact destination bytes queued alongside
//! `present()`. This proves submitted texture content, not eventual display.
//!
//! Discipline (the counted-drop recorder shape shared with the cast/temporal
//! recorders): a small ring of staging buffers absorbs GPU latency; when the
//! ring is saturated the frame is SKIPPED and counted, never blocked on; the
//! harvested store restores capture-sequence order across asynchronous map
//! callbacks, then applies byte-budgeted lowest-sequence eviction with a
//! separate count. The present path pays one `Option` branch when the tap is off.
//!
//! THE PER-PIXEL CONVERSION RUNS OFF THE PRESENT THREAD: a mapped slot's raw
//! padded bytes are memcpy'd out and shipped to the recording's dedicated
//! conversion worker ([`ConvertWorker`]), which runs [`mapped_to_rgba8`] — the
//! f16 decode + scRGB→SDR tonemap + sRGB encode + 2×2 downscale — and hands
//! finished RGBA8 frames back for the ordered, budgeted store. The present
//! thread's per-frame cost is one bounded memcpy plus channel traffic. (The
//! conversion used to run synchronously in `after_present`, and one 7.25 s EDR
//! recording episode starved input to p99 201 ms with 21 wake heals.) When no
//! thread can be spawned (wasm, exhaustion) the harvest falls back to the old
//! inline conversion; a worker more than [`CONVERT_BACKLOG`] frames behind is
//! a counted mid-stream drop, never a block.

use std::collections::VecDeque;
use std::sync::mpsc;

/// The colour space the platform compositor uses to interpret a window's
/// presented texture.
///
/// This is explicit per-window metadata rather than an inference from the
/// texture format: an 8-bit `Bgra8Unorm` surface can be tagged either sRGB or
/// Display-P3 on macOS. `Rgba16Float` is only valid with
/// [`Self::ExtendedLinearSrgb`] in aterm's present path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureColorSpace {
    /// The platform tag could not be established. Exact capture refuses this
    /// state rather than guessing how the compositor interpreted the bytes.
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
    /// Exactly `1.0` on macOS and all SDR surfaces.
    sdr_white_scale: f32,
}

impl CaptureEncoding {
    fn new(
        format: wgpu::TextureFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Result<Self, String> {
        if color_space == CaptureColorSpace::Unknown {
            return Err("presented-frame colour space is unknown".to_string());
        }
        let format_is_f16 = format == wgpu::TextureFormat::Rgba16Float;
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

    fn format_label(self, format: wgpu::TextureFormat) -> &'static str {
        match (format, self.color_space) {
            (wgpu::TextureFormat::Bgra8Unorm, CaptureColorSpace::Srgb) => "bgra8",
            (wgpu::TextureFormat::Rgba8Unorm, CaptureColorSpace::Srgb) => "rgba8",
            (wgpu::TextureFormat::Bgra8Unorm, CaptureColorSpace::DisplayP3) => {
                "bgra8-display-p3->srgb8"
            }
            (wgpu::TextureFormat::Rgba8Unorm, CaptureColorSpace::DisplayP3) => {
                "rgba8-display-p3->srgb8"
            }
            (wgpu::TextureFormat::Rgba16Float, CaptureColorSpace::ExtendedLinearSrgb) => {
                "rgba16f-scrgb->srgb8-tonemapped"
            }
            _ => "unknown",
        }
    }
}

/// One harvested frame: tightly-packed straight, non-linear sRGB RGBA8, plus
/// the same-clock timestamp the GUI stamped right after `present()` returned
/// (µs, `metrics::now_us` epoch).
///
/// The capture transform makes the bytes self-describing as ordinary sRGB:
/// unprofiled PNG/video writers must not reinterpret a tagged Display-P3
/// swapchain or clamp scRGB directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedFrame {
    pub seq: u64,
    pub t_us: u64,
    pub w: u32,
    pub h: u32,
    /// Tightly-packed RGBA8 (already swizzled/downscaled at harvest).
    pub rgba: Vec<u8>,
}

/// Insert one asynchronously harvested frame in capture-sequence order and
/// enforce the byte budget by evicting the lowest sequence numbers.
///
/// GPU map callbacks may arrive out of order. Append-on-completion would make
/// the recording timeline callback-ordered and make `pop_front` evict an
/// arbitrary completion rather than the oldest captured frame. The store is
/// bounded, so the linear insertion is bounded too.
///
/// Returns the number of frames evicted by this insertion; callers accumulate
/// that separately from mid-stream `dropped`.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "HarvestThree",
        project = "aterm_gpu::video_tap::ordered_capture_store_push"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "HarvestOne",
        project = "aterm_gpu::video_tap::ordered_capture_store_push"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "HarvestTwo",
        project = "aterm_gpu::video_tap::ordered_capture_store_push"
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
    /// Frames LOST mid-stream (staging ring saturated / map failure /
    /// conversion backlog or failure / device loss), counted so the artifact
    /// is honest about coverage. Budget
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

/// Abstract lifecycle phase of one [`VideoTap`] staging slot.
///
/// Payloads such as sequence/timestamp/colour encoding remain on the internal
/// slot state. This phase is the pure, device-free projection used by the
/// shipping transition gate and its derived-model conformance test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoSlotPhase {
    /// Available for a new swapchain copy.
    Free,
    /// Copy encoded; waiting for the successful present timestamp.
    Pending,
    /// Mapping requested; waiting for the callback.
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

/// Decide one genuine [`VideoTap`] staging-slot transition.
///
/// Invalid phase/event pairs return `None`: stale/duplicate map callbacks never
/// mutate a reused slot. Map failures and finalization aborts both release the
/// slot and count exactly one loss.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "Enqueue",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "StartMap",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "MapOk",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "MapError",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "Abort",
        project = "aterm_gpu::video_tap::video_slot_transition"
    )
)]
#[must_use]
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
    /// Client-requested fps decimation; not a loss.
    Decimate,
    /// A requested sample had impossible metadata; count one loss.
    DropInvalidMetadata,
}

/// Classify a present without conflating decimation with invalid-metadata loss.
///
/// Metadata is intentionally irrelevant when the fps gate rejects the present:
/// a frame the client did not request cannot inflate `dropped`.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "VideoTapSlot",
        action = "RejectInvalidMetadata",
        project = "aterm_gpu::video_tap::video_present_decision"
    )
)]
#[must_use]
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
    const fn phase(&self) -> VideoSlotPhase {
        match self {
            Self::Free => VideoSlotPhase::Free,
            Self::Pending { .. } => VideoSlotPhase::Pending,
            Self::InFlight { .. } => VideoSlotPhase::InFlight,
        }
    }
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
    /// Mid-stream losses (ring saturation / map failure / conversion backlog
    /// or failure / device loss).
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
    // aterm-time so the type/clock matches the renderer's wasm-safe present clocks;
    // native builds get std's Instant unchanged (video tap is a native feature).
    epoch: aterm_time::Instant,
    seq: u64,
    half_res: bool,
    /// Source swapchain geometry/format the ring was built for; a mismatch at
    /// enqueue time (resize / format flip) finalizes the recording honestly.
    src_w: u32,
    src_h: u32,
    format: wgpu::TextureFormat,
    /// First source encoding actually copied. Later metadata changes are
    /// transformed per-slot and make the aggregate source label explicit.
    first_capture_encoding: Option<CaptureEncoding>,
    mixed_capture_encoding: bool,
    bpp: u32,
    resized_early_stop: bool,
    /// Dedicated conversion lane: the per-pixel decode/tonemap/downscale runs
    /// on this worker so `after_present` never pays per-pixel work on the
    /// present thread. `None` — spawn failed, or the worker died mid-take —
    /// falls back to the old inline conversion.
    worker: Option<ConvertWorker>,
}

/// Staging-ring depth: enough to absorb a GPU running a frame or two behind
/// without ever blocking the present path.
const RING: usize = 4;
/// Default RAM budget for the harvested store (bytes).
pub const DEFAULT_BUDGET: usize = 512 * 1024 * 1024;

/// Upper bound on conversions dispatched to the worker and not yet adopted
/// back into the store. At ring depth on purpose: it bounds the transient
/// raw-copy RAM to what the staging ring already commits, and a worker that
/// far behind means conversion cannot keep pace with presents — the recorder
/// discipline then counts a drop rather than ever blocking (or slowing) the
/// present thread the way the old inline conversion did.
const CONVERT_BACKLOG: usize = RING;

/// One mapped slot's RAW padded bytes en route to the conversion worker, with
/// the per-frame facts the pure [`mapped_to_rgba8`] needs. Per-tap geometry
/// and format travel in the worker's closure instead — they are fixed for a
/// recording's lifetime (a mid-capture resize finalizes the tap).
struct ConvertJob {
    raw: Vec<u8>,
    seq: u64,
    t_us: u64,
    capture_encoding: CaptureEncoding,
}

/// The recording's dedicated conversion lane. The worker thread lives exactly
/// as long as the tap (its job sender drops with the tap / at `finish`), runs
/// [`mapped_to_rgba8`] per job, and answers with `Some(frame)` or `None` on a
/// conversion error (adopted as one counted mid-stream loss).
struct ConvertWorker {
    job_tx: mpsc::Sender<ConvertJob>,
    result_rx: mpsc::Receiver<Option<CapturedFrame>>,
    /// Jobs dispatched and not yet adopted; bounded by [`CONVERT_BACKLOG`].
    pending: usize,
    handle: std::thread::JoinHandle<()>,
}

/// Spawn the dedicated conversion worker for one recording. `None` when the
/// platform cannot spawn a thread (wasm, thread exhaustion) — the harvest then
/// falls back to converting inline on the present thread, the old behavior.
fn spawn_convert_worker(
    src_w: u32,
    src_h: u32,
    padded_row: usize,
    format: wgpu::TextureFormat,
    half_res: bool,
) -> Option<ConvertWorker> {
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
                    return; // tap gone: nothing is left to adopt results
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

/// Bytes per source pixel for every destination format the presented-frame taps
/// support. Kept independent of a device so the decode contract can be tested
/// exhaustively without an adapter.
fn capture_bpp(format: wgpu::TextureFormat) -> Result<u32, String> {
    match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm => Ok(4),
        wgpu::TextureFormat::Rgba16Float => Ok(8),
        other => Err(format!("unsupported presented-frame format {other:?}")),
    }
}

fn padded_row_bytes(width: u32, bpp: u32) -> u64 {
    (u64::from(width) * u64::from(bpp)).div_ceil(256) * 256
}

/// Strip GPU row padding, swizzle/decode the source format, and optionally box
/// downsample 2×2. This is deliberately PURE: both the streaming [`VideoTap`]
/// and the one-shot [`PresentedFrameTap`] use it, while unit tests feed it
/// synthetic mapped bytes with no GPU/device.
fn mapped_to_rgba8(
    data: &[u8],
    src_w: u32,
    src_h: u32,
    padded_row: usize,
    format: wgpu::TextureFormat,
    capture_encoding: CaptureEncoding,
    half_res: bool,
) -> Result<(Vec<u8>, u32, u32), String> {
    let bpp = usize::try_from(capture_bpp(format)?)
        .map_err(|_| "presented-frame bytes-per-pixel does not fit memory".to_string())?;
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
    if data.len() < required {
        return Err(format!(
            "presented-frame buffer has {} bytes, expected at least {required}",
            data.len()
        ));
    }

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
    let is_f16 = format == wgpu::TextureFormat::Rgba16Float;
    let bgra = format == wgpu::TextureFormat::Bgra8Unorm;

    // Fetch one source pixel as straight, non-linear sRGB RGBA8. Alpha is
    // linear coverage and must NEVER receive a transfer or gamut transform.
    let fetch = |x: usize, y: usize| -> [u32; 4] {
        let base = y * padded_row + x * bpp;
        if is_f16 {
            let half = |channel: usize| {
                u16::from_le_bytes([data[base + channel * 2], data[base + channel * 2 + 1]])
            };
            let linear = tone_map_scrgb_to_sdr(
                [
                    f16_to_f32(half(0)),
                    f16_to_f32(half(1)),
                    f16_to_f32(half(2)),
                ],
                capture_encoding.sdr_white_scale,
            );
            let alpha = (f16_to_f32(half(3)).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
            [
                srgb8_from_linear(linear[0]),
                srgb8_from_linear(linear[1]),
                srgb8_from_linear(linear[2]),
                alpha,
            ]
        } else {
            let raw = if bgra {
                [data[base + 2], data[base + 1], data[base], data[base + 3]]
            } else {
                [data[base], data[base + 1], data[base + 2], data[base + 3]]
            };
            if capture_encoding.color_space == CaptureColorSpace::DisplayP3 {
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
    };

    // Byte-indexed sRGB→linear decode table for the 2×2 box filter below. Every
    // `fetch` return path yields channels in 0..=255 (the 8-bit swizzle,
    // `display_p3_to_srgb8`, and `srgb8_from_linear` for f16 all quantize to a
    // byte), so `decode[n]` is EXACTLY what `srgb_to_linear(n as f32 / 255.0)`
    // returns for that same n — this is a bit-identical substitution, not an
    // approximation. It trades 12 `powf` per OUTPUT pixel (4 samples × 3
    // channels) for 256 `powf` per frame, which still matters off-thread (the
    // conversion worker must keep pace with presents or frames drop) and
    // matters doubly on the no-thread inline fallback. The encode side keeps its `powf`:
    // `srgb8_from_linear` produces the stored pixel byte and its rounding is a
    // pinned parity artifact. Only the downsampling path decodes, so the table
    // is skipped otherwise.
    let decode: [f32; 256] = if half_res {
        std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0))
    } else {
        [0.0; 256]
    };

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
                                let linear = decode[sample[channel] as usize];
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
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
        opts: CaptureOpts,
    ) -> Result<Self, String> {
        let bpp = capture_bpp(format)
            .map_err(|_| format!("video: unsupported swapchain format {format:?}"))?;
        let _ = CaptureEncoding::new(format, color_space, sdr_white_scale)
            .map_err(|error| format!("video: {error}"))?;
        // COPY_BYTES_PER_ROW_ALIGNMENT (256) padding, computed on BYTES so it is
        // correct for both the 4-byte SDR and 8-byte f16 formats.
        let padded = padded_row_bytes(src_w, bpp);
        let size = padded * src_h as u64;
        // A stride that does not fit `usize` cannot be converted on ANY thread;
        // the (unreachable in practice) fallback keeps the old inline path.
        let worker = usize::try_from(padded)
            .ok()
            .and_then(|row| spawn_convert_worker(src_w, src_h, row, format, opts.half_res));
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
            worker,
        })
    }

    fn padded_row(&self) -> u64 {
        padded_row_bytes(self.src_w, self.bpp)
    }

    fn note_capture_encoding(&mut self, capture_encoding: CaptureEncoding) {
        match self.first_capture_encoding {
            Some(first) if first != capture_encoding => self.mixed_capture_encoding = true,
            None => self.first_capture_encoding = Some(capture_encoding),
            Some(_) => {}
        }
    }

    /// Enqueue the swapchain copy into THIS present's encoder (called between
    /// the render-pass close and `queue.submit`). Never blocks: a saturated
    /// ring or a mismatched size counts a drop / finalizes instead.
    pub fn enqueue_copy(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        frame_tex: &wgpu::Texture,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) {
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

    /// Apply the client fps gate before validating this present's live colour
    /// metadata. An invalid source therefore counts one loss only when this was
    /// a requested sampling opportunity; deliberately decimated presents remain
    /// decimated rather than inflating `dropped`.
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
                // The arm validates its initial metadata. A later impossible
                // source state is a counted loss, never a guessed conversion.
                self.dropped += 1;
                None
            }
        }
    }

    /// Post-present hook (GUI-side, right after `frame.present()` returned):
    /// stamp the just-enqueued copy with the same-clock time, issue its
    /// map_async, then NON-BLOCKING poll + harvest any completed earlier maps
    /// (a raw memcpy handed to the conversion worker — never per-pixel work
    /// here) and adopt any finished conversions into the store. Bounded work;
    /// never waits on the GPU or the worker.
    pub fn after_present(&mut self, device: &wgpu::Device, t_us: u64) {
        // Stamp + map the newest Pending slot (at most one per present).
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let SlotState::Pending {
                seq,
                capture_encoding,
            } = slot.state
            {
                let tx = self.done_tx.clone();
                slot.buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| {
                        let _ = tx.send((i, r.is_ok()));
                    });
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
        // Non-blocking poll: advances map completions without waiting.
        let _ = device.poll(wgpu::PollType::Poll);
        self.harvest_ready();
    }

    /// Drain the completion channel: adopt finished off-thread conversions,
    /// then route every newly mapped slot toward RGBA8. A dispatched slot is
    /// copied out raw and unmapped HERE, so a slot's lifetime is still bounded
    /// by map latency alone — conversion lag can never saturate the staging
    /// ring, it can only (bounded, counted) drop via [`CONVERT_BACKLOG`].
    fn harvest_ready(&mut self) {
        self.adopt_converted();
        while let Ok((idx, ok)) = self.done_rx.try_recv() {
            let (seq, t_us, capture_encoding) = match self.slots[idx].state {
                SlotState::InFlight {
                    seq,
                    t_us,
                    capture_encoding,
                } => (seq, t_us, capture_encoding),
                _ => continue, // stale/duplicate completion
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
            self.slots[idx].buffer.unmap();
            match transition.phase {
                VideoSlotPhase::Free => self.slots[idx].state = SlotState::Free,
                _ => unreachable!("a completed video map always releases its slot"),
            }
        }
        self.adopt_converted();
    }

    /// Route one successfully mapped slot's bytes toward RGBA8. With a live
    /// worker the padded bytes are memcpy'd out (bounded, no per-pixel work)
    /// and the decode/tonemap/downscale runs OFF the present thread; the
    /// result is adopted by a later [`Self::adopt_converted`] drain, so this
    /// returns `None`. `Some(frame)` is the inline fallback (no worker, or the
    /// worker died) — the old synchronous behavior, pushed by the caller. A
    /// worker already [`CONVERT_BACKLOG`] frames behind means conversion is
    /// not keeping pace: count one mid-stream drop rather than queueing
    /// unbounded raw copies or blocking the present thread.
    fn convert_or_dispatch(
        &mut self,
        idx: usize,
        seq: u64,
        t_us: u64,
        capture_encoding: CaptureEncoding,
    ) -> Option<CapturedFrame> {
        if let Some(worker) = self.worker.as_mut() {
            if worker.pending >= CONVERT_BACKLOG {
                self.dropped += 1;
                return None;
            }
            let raw = self.slots[idx].buffer.slice(..).get_mapped_range().to_vec();
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
                // Worker died (its thread panicked): fall back to inline
                // conversion for the rest of the recording.
                Err(_) => self.worker = None,
            }
        }
        Some(self.copy_out(idx, seq, t_us, capture_encoding))
    }

    /// Non-blocking adoption of finished off-thread conversions into the
    /// ordered, budgeted store. A conversion failure is one counted mid-stream
    /// loss, exactly like a map failure.
    fn adopt_converted(&mut self) {
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

    /// INLINE FALLBACK (no conversion worker): strip row padding +
    /// swizzle/decode + (optionally) 2×2 box-downscale one mapped slot into a
    /// tightly-packed RGBA8 frame, synchronously on the calling (present)
    /// thread.
    fn copy_out(
        &self,
        idx: usize,
        seq: u64,
        t_us: u64,
        capture_encoding: CaptureEncoding,
    ) -> CapturedFrame {
        let padded = self.padded_row() as usize;
        let data = self.slots[idx].buffer.slice(..).get_mapped_range();
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

    /// Budget-bounded, sequence-ordered push (the cast/temporal recorder
    /// discipline). Async map callbacks may complete out of order, so insertion
    /// restores capture order before drop-oldest evicts the lowest sequence.
    /// An eviction counts in `evicted`, NOT `dropped`: it truncates the HEAD of
    /// the recording while `dropped` means a mid-stream skip.
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
    pub fn resized(&self) -> bool {
        self.resized_early_stop
    }

    /// Frames harvested so far (for `video status`). Includes frames dispatched
    /// to the conversion worker and not yet adopted: they were captured, and
    /// counting them keeps this progress read equivalent to the old
    /// synchronous harvest (a conversion failure later books a drop instead).
    #[must_use]
    pub fn frames_so_far(&self) -> usize {
        self.store.len() + self.worker.as_ref().map_or(0, |worker| worker.pending)
    }

    /// Finalize: BLOCKING drain of in-flight maps and dispatched conversions
    /// (off the hot path by definition — the recording is over), then hand the
    /// store out. Every frame the worker was still converting is adopted
    /// before the take is sealed, so deferring the tonemap loses nothing.
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
        // Conversion lane drain: dropping the job sender lets the worker run
        // its queue dry and exit; every outstanding result is adopted here. A
        // worker that died with jobs outstanding is counted honestly.
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
    /// Copy encoded; waiting for the matching successful-present hook.
    Pending,
    /// Mapping requested; waiting for completion.
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

/// Decide one genuine [`PresentedFrameTap`] lifecycle transition.
///
/// Invalid phase/event pairs return `None`, preventing a stale callback or
/// repeated present hook from completing the wrong capture generation.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "PresentedFrameTap",
        action = "EnqueueValid",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "PresentedFrameTap",
        action = "RejectEnqueue",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "PresentedFrameTap",
        action = "StartMap",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "PresentedFrameTap",
        action = "CompleteMap",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "PresentedFrameTap",
        action = "MapError",
        project = "aterm_gpu::video_tap::presented_frame_transition"
    )
)]
#[must_use]
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
    /// The copy is in a command encoder that has not yet reached the
    /// caller's post-present hook.
    Pending,
    /// `map_async` is outstanding.
    InFlight { t_us: u64 },
    /// `result` contains either the exact frame or a terminal capture error.
    Complete,
}

impl PresentedFrameState {
    const fn phase(self) -> PresentedFramePhase {
        match self {
            Self::Armed => PresentedFramePhase::Armed,
            Self::Pending => PresentedFramePhase::Pending,
            Self::InFlight { .. } => PresentedFramePhase::InFlight,
            Self::Complete => PresentedFramePhase::Complete,
        }
    }
}

/// Independent one-shot tap for the exact presented destination.
///
/// Unlike [`VideoTap`], this has one staging buffer, no fps/budget policy, and
/// captures only the first successful present after it is armed. It deliberately
/// lives beside (not inside) the video recorder so a still-image request and an
/// active recording can copy the same destination in one encoder without
/// consuming or perturbing each other's state.
pub(crate) struct PresentedFrameTap {
    buffer: wgpu::Buffer,
    done_tx: mpsc::Sender<bool>,
    done_rx: mpsc::Receiver<bool>,
    state: PresentedFrameState,
    result: Option<Result<CapturedFrame, String>>,
    src_w: u32,
    src_h: u32,
    format: wgpu::TextureFormat,
    capture_encoding: Option<CaptureEncoding>,
    bpp: u32,
}

impl PresentedFrameTap {
    pub(crate) fn new(
        device: &wgpu::Device,
        src_w: u32,
        src_h: u32,
        format: wgpu::TextureFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> Result<Self, String> {
        let bpp = capture_bpp(format)
            .map_err(|_| format!("presented snapshot: unsupported format {format:?}"))?;
        let _ = CaptureEncoding::new(format, color_space, sdr_white_scale)
            .map_err(|error| format!("presented snapshot: {error}"))?;
        let size = padded_row_bytes(src_w, bpp)
            .checked_mul(u64::from(src_h))
            .ok_or_else(|| "presented snapshot: staging size overflow".to_string())?;
        if size == 0 {
            return Err("presented snapshot: destination has zero area".to_string());
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm presented-frame snapshot staging"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let (done_tx, done_rx) = mpsc::channel();
        Ok(Self {
            buffer,
            done_tx,
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

    fn padded_row(&self) -> u64 {
        padded_row_bytes(self.src_w, self.bpp)
    }

    fn apply_transition(
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

    /// Append this one-shot copy to the SAME encoder as the completed destination
    /// render. A mismatch is terminal and explicit; it never silently returns a
    /// differently-sized or differently-encoded frame.
    pub(crate) fn enqueue_copy(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame_tex: &wgpu::Texture,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) {
        if self.state != PresentedFrameState::Armed {
            return;
        }
        if frame_tex.width() != self.src_w
            || frame_tex.height() != self.src_h
            || frame_tex.format() != self.format
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
            return;
        }
        let capture_encoding = match CaptureEncoding::new(self.format, color_space, sdr_white_scale)
        {
            Ok(encoding) => encoding,
            Err(error) => {
                self.result = Some(Err(format!("presented snapshot: {error}")));
                let outcome = self.apply_transition(PresentedFrameEvent::RejectEnqueue, None);
                debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
                return;
            }
        };
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: frame_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
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
    }

    /// Stamp and map the copy belonging to the successful present. Non-blocking:
    /// [`Self::finish`] owns the off-hot-path wait.
    pub(crate) fn after_present(&mut self, device: &wgpu::Device, t_us: u64) -> Result<(), String> {
        match self.state {
            PresentedFrameState::Armed => {
                return Err(
                    "presented snapshot: successful present did not enqueue a destination copy"
                        .to_string(),
                );
            }
            PresentedFrameState::Pending => {
                let tx = self.done_tx.clone();
                self.buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        let _ = tx.send(result.is_ok());
                    });
                let outcome = self.apply_transition(PresentedFrameEvent::StartMap, Some(t_us));
                debug_assert_eq!(outcome, PresentedFrameOutcome::None);
            }
            PresentedFrameState::InFlight { .. } | PresentedFrameState::Complete => {}
        }
        let _ = device.poll(wgpu::PollType::Poll);
        self.harvest_ready();
        self.result_status()
    }

    fn harvest_ready(&mut self) {
        let Ok(mapped) = self.done_rx.try_recv() else {
            return;
        };
        let PresentedFrameState::InFlight { t_us } = self.state else {
            return;
        };
        let result = if mapped {
            let data = self.buffer.slice(..).get_mapped_range();
            let converted = mapped_to_rgba8(
                &data,
                self.src_w,
                self.src_h,
                self.padded_row() as usize,
                self.format,
                self.capture_encoding
                    .expect("a pending presented copy freezes its capture encoding"),
                false,
            );
            drop(data);
            converted.map(|(rgba, w, h)| CapturedFrame {
                seq: 1,
                t_us,
                w,
                h,
                rgba,
            })
        } else {
            Err("presented snapshot: staging-buffer map failed".to_string())
        };
        let completed_ok = result.is_ok();
        self.buffer.unmap();
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

    /// Blocking completion, called only after the explicit capture leaves the
    /// present hot path. The frame remains owned by the tap until [`Self::take`].
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
                if let Err(error) = device.poll(wgpu::PollType::wait_indefinitely()) {
                    self.buffer.unmap();
                    self.result = Some(Err(format!(
                        "presented snapshot: GPU completion wait failed: {error}"
                    )));
                    let outcome = self.apply_transition(PresentedFrameEvent::MapError, None);
                    debug_assert_eq!(outcome, PresentedFrameOutcome::Error);
                } else {
                    self.harvest_ready();
                    if self.state != PresentedFrameState::Complete {
                        self.buffer.unmap();
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

    /// Consume the completed tap and return its exact RGBA8 destination.
    pub(crate) fn take(self) -> Result<CapturedFrame, String> {
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

fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb8_from_linear(linear: f32) -> u32 {
    (linear_to_srgb(linear.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32
}

/// Convert the legacy Display-P3 interpretation of 8-bit swapchain coordinates
/// into unprofiled sRGB output. Display-P3 and sRGB share a transfer curve, so
/// this decodes that curve, applies the D65 linear-primary matrix, clips the
/// out-of-sRGB gamut, then re-encodes. The matrix is the CSS Color 4
/// Display-P3→linear-sRGB transform.
fn display_p3_to_srgb8(p3: [u8; 3]) -> [u8; 3] {
    let [r, g, b] = p3.map(|channel| srgb_to_linear(f32::from(channel) / 255.0));
    let srgb_linear = [
        1.224_940_2_f32.mul_add(r, -0.224_940_18 * g),
        (-0.042_056_955_f32).mul_add(r, 1.042_057 * g),
        (-0.019_637_555_f32).mul_add(r, (-0.078_636_05_f32).mul_add(g, 1.098_273_6 * b)),
    ];
    srgb_linear.map(|channel| srgb8_from_linear(channel) as u8)
}

/// Portable scRGB/EDR → SDR-linear capture transform.
///
/// First remove Windows' *presentation-only* SDR-white multiplier so ordinary
/// grid colours regain their original linear-sRGB values. If a resulting pixel
/// exceeds SDR white, max-RGB compression divides all three channels by the
/// peak. This is a deliberately small, bounded local tone map: it leaves every
/// in-range SDR pixel byte-stable, maps the brightest highlight channel to
/// `1.0`, and preserves highlight RGB ratios instead of independently clipping
/// channels (which would turn coloured EDR crowns white/yellow). EDR luminance
/// magnitude above white cannot be represented in an 8-bit SDR artifact; this
/// transform preserves its chromatic structure without pretending otherwise.
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
            epoch: aterm_time::Instant::now(),
            seq: 0,
            half_res: false,
            src_w: 2,
            src_h: 2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            first_capture_encoding: None,
            mixed_capture_encoding: false,
            bpp: 4,
            resized_early_stop: false,
            worker: None,
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
        format: wgpu::TextureFormat,
        color_space: CaptureColorSpace,
        sdr_white_scale: f32,
    ) -> CaptureEncoding {
        CaptureEncoding::new(format, color_space, sdr_white_scale).unwrap()
    }

    #[test]
    fn capture_lifecycle_xrefs_cover_every_modeled_action() {
        let actions_for = |machine| {
            aterm_spec::xref::refinements()
                .filter(|anchor| anchor.machine == machine)
                .map(|anchor| anchor.action)
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            actions_for("PresentedFrameTap"),
            std::collections::BTreeSet::from([
                "CompleteMap",
                "EnqueueValid",
                "MapError",
                "RejectEnqueue",
                "StartMap",
            ])
        );
        assert_eq!(
            actions_for("VideoTapSlot"),
            std::collections::BTreeSet::from([
                "Abort",
                "Enqueue",
                "HarvestOne",
                "HarvestThree",
                "HarvestTwo",
                "MapError",
                "MapOk",
                "RejectInvalidMetadata",
                "StartMap",
            ])
        );
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

    /// Async map callbacks are not ordered. Completing capture sequences
    /// 3,1,2 must still publish 1,2,3; under a two-frame budget the same arrival
    /// order must evict sequence 1 and retain the chronological tail 2,3.
    #[test]
    fn out_of_order_harvest_is_sorted_and_budget_keeps_sequence_tail() {
        let mut all = test_tap(usize::MAX, None);
        for seq in [3, 1, 2] {
            all.push_frame(synth_frame(seq));
        }
        assert_eq!(
            all.store.iter().map(|frame| frame.seq).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "callback arrival order must not become recording order"
        );
        assert_eq!(all.evicted, 0);
        assert_eq!(all.store_bytes, 3 * 16);

        let mut tail = test_tap(2 * 16, None);
        for seq in [3, 1, 2] {
            tail.push_frame(synth_frame(seq));
        }
        assert_eq!(
            tail.store.iter().map(|frame| frame.seq).collect::<Vec<_>>(),
            vec![2, 3],
            "budget eviction must remove the lowest sequence, not first callback"
        );
        assert_eq!(tail.evicted, 1);
        assert_eq!(tail.dropped, 0);
        assert_eq!(tail.store_bytes, 2 * 16);
    }

    /// The OFF-THREAD conversion lane (the harvest no longer tonemaps on the
    /// present thread) is a pure relocation: for both the SDR swizzle and the
    /// EDR f16 scRGB→SDR tonemap path the worker's output is BYTE-IDENTICAL
    /// to the inline [`mapped_to_rgba8`] fallback, frame stamps ride through
    /// untouched, and dropping the job sender drains + exits the worker (the
    /// `finish`/tap-drop lifecycle).
    #[test]
    fn convert_worker_matches_inline_conversion_and_drains_on_drop() {
        let stride = 256; // COPY_BYTES_PER_ROW_ALIGNMENT padding
        // SDR: 2x2 RGBA8 sRGB, full res.
        let sdr_rows: [&[u8]; 2] = [
            &[1, 2, 3, 255, 9, 8, 7, 128],
            &[0, 0, 0, 0, 255, 255, 255, 255],
        ];
        let sdr_raw = padded_fixture(&sdr_rows, stride);
        let sdr_enc = encoding(
            wgpu::TextureFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        );
        // EDR: 2x2 RGBA16F scRGB with an above-1.0 highlight, half res (the
        // downsampling path exercises the decode table + premultiplied filter).
        let edr_pixels: Vec<u8> = [
            half_pixel([0x3C00, 0x0000, 0x0000, 0x3C00]), // 1.0 red
            half_pixel([0x4400, 0x4400, 0x4400, 0x3C00]), // 4.0 highlight
            half_pixel([0x0000, 0x3800, 0x0000, 0x3C00]), // 0.5 green
            half_pixel([0x0000, 0x0000, 0x3C00, 0x3800]), // blue, alpha 0.5
        ]
        .concat();
        let edr_rows: [&[u8]; 2] = [&edr_pixels[..16], &edr_pixels[16..]];
        let edr_raw = padded_fixture(&edr_rows, stride);
        let edr_enc = encoding(
            wgpu::TextureFormat::Rgba16Float,
            CaptureColorSpace::ExtendedLinearSrgb,
            1.0,
        );
        for (label, raw, format, enc, half_res) in [
            (
                "sdr",
                sdr_raw,
                wgpu::TextureFormat::Rgba8Unorm,
                sdr_enc,
                false,
            ),
            (
                "edr",
                edr_raw,
                wgpu::TextureFormat::Rgba16Float,
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

    /// Device-free format contract: both byte-order variants retain source
    /// alpha exactly while BGRA performs only the RGB swizzle.
    #[test]
    fn mapped_sdr_formats_preserve_alpha() {
        let rgba_source = [10, 20, 30, 0, 40, 50, 60, 173];
        let (rgba, w, h) = mapped_to_rgba8(
            &rgba_source,
            2,
            1,
            8,
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
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
            wgpu::TextureFormat::Bgra8Unorm,
            encoding(
                wgpu::TextureFormat::Bgra8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            false,
        )
        .unwrap();
        assert_eq!(rgba, [10, 20, 30, 7, 40, 50, 60, 241]);
    }

    /// f16 RGB is converted from extended-linear to sRGB8, but alpha is linear
    /// coverage: 0.5 must become 128, not the ~188 an sRGB transfer would yield.
    #[test]
    fn mapped_f16_preserves_linear_alpha() {
        let mut source = half_pixel([0x3c00, 0x3800, 0x0000, 0x3800]);
        source.extend(half_pixel([0x0000, 0x3c00, 0x3800, 0x3400]));
        let (rgba, w, h) = mapped_to_rgba8(
            &source,
            2,
            1,
            16,
            wgpu::TextureFormat::Rgba16Float,
            encoding(
                wgpu::TextureFormat::Rgba16Float,
                CaptureColorSpace::ExtendedLinearSrgb,
                1.0,
            ),
            false,
        )
        .unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba, [255, 188, 0, 128, 0, 255, 188, 64]);
    }

    /// The 2×2 video downscale averages all four channels, including alpha, for
    /// every supported source format. Padded rows make this the same layout the
    /// GPU staging buffers expose rather than a tightly-packed special case.
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
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
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
            wgpu::TextureFormat::Bgra8Unorm,
            encoding(
                wgpu::TextureFormat::Bgra8Unorm,
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
            wgpu::TextureFormat::Rgba16Float,
            encoding(
                wgpu::TextureFormat::Rgba16Float,
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
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
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
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
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

        // Round-half alpha: three of the four samples carry coverage=1.
        let low_alpha =
            padded_fixture(&[&[1, 2, 3, 1, 1, 2, 3, 1], &[1, 2, 3, 1, 1, 2, 3, 0]], 256);
        let (rgba, _, _) = mapped_to_rgba8(
            &low_alpha,
            2,
            2,
            256,
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
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
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
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
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
                CaptureColorSpace::Srgb,
                1.0,
            ),
            true,
        )
        .unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgba, one);
    }

    /// The Windows scRGB multiplier is presentation metadata, not scene
    /// exposure. Removing it recovers byte-identical SDR content; omitting that
    /// metadata is a visible regression (negative control).
    #[test]
    fn scrgb_capture_undoes_sdr_white_scale_before_encoding() {
        // Stored [1.0, 0.5, 0.0] is scene [0.5, 0.25, 0.0] at scale=2.
        let source = half_pixel([0x3c00, 0x3800, 0x0000, 0x3800]);
        let decode = |scale| {
            mapped_to_rgba8(
                &source,
                1,
                1,
                8,
                wgpu::TextureFormat::Rgba16Float,
                encoding(
                    wgpu::TextureFormat::Rgba16Float,
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

    /// Over-white channels are compressed together, retaining their ratios
    /// instead of independent clamping. Alpha remains linear coverage.
    #[test]
    fn scrgb_highlight_tone_map_is_bounded_and_chromatic() {
        // Stored [4,2,1] at scale=2 => scene [2,1,0.5] => [1,0.5,0.25].
        let source = half_pixel([0x4400, 0x4000, 0x3c00, 0x3400]);
        let (rgba, _, _) = mapped_to_rgba8(
            &source,
            1,
            1,
            8,
            wgpu::TextureFormat::Rgba16Float,
            encoding(
                wgpu::TextureFormat::Rgba16Float,
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

    /// P3-tagged coordinates must become actual sRGB before an unprofiled PNG
    /// consumes them. The same bytes on an sRGB-tagged surface remain exact.
    #[test]
    fn display_p3_capture_converts_primaries_and_srgb_is_byte_exact() {
        let source = [128, 255, 0, 73];
        let p3 = mapped_to_rgba8(
            &source,
            1,
            1,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
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
            wgpu::TextureFormat::Rgba8Unorm,
            encoding(
                wgpu::TextureFormat::Rgba8Unorm,
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

    #[test]
    fn capture_encoding_rejects_format_space_mismatch_and_bad_scale() {
        assert!(
            CaptureEncoding::new(
                wgpu::TextureFormat::Rgba16Float,
                CaptureColorSpace::Srgb,
                1.0,
            )
            .is_err()
        );
        assert!(
            CaptureEncoding::new(
                wgpu::TextureFormat::Bgra8Unorm,
                CaptureColorSpace::ExtendedLinearSrgb,
                1.0,
            )
            .is_err()
        );
        assert!(
            CaptureEncoding::new(
                wgpu::TextureFormat::Rgba16Float,
                CaptureColorSpace::ExtendedLinearSrgb,
                f32::NAN,
            )
            .is_err()
        );
        assert!(
            CaptureEncoding::new(
                wgpu::TextureFormat::Bgra8Unorm,
                CaptureColorSpace::Unknown,
                1.0,
            )
            .is_err()
        );
    }

    #[test]
    fn in_flight_slots_freeze_encoding_while_later_frames_track_changes() {
        let srgb = encoding(
            wgpu::TextureFormat::Rgba8Unorm,
            CaptureColorSpace::Srgb,
            1.0,
        );
        let p3 = encoding(
            wgpu::TextureFormat::Rgba8Unorm,
            CaptureColorSpace::DisplayP3,
            1.0,
        );
        let mut tap = test_tap(usize::MAX, None);
        tap.note_capture_encoding(srgb);
        let in_flight = SlotState::InFlight {
            seq: 1,
            t_us: 10,
            capture_encoding: srgb,
        };
        tap.note_capture_encoding(p3);
        assert_eq!(tap.first_capture_encoding, Some(srgb));
        assert!(tap.mixed_capture_encoding);
        assert!(matches!(
            in_flight,
            SlotState::InFlight {
                capture_encoding,
                ..
            } if capture_encoding == srgb
        ));

        let scale_one = encoding(
            wgpu::TextureFormat::Rgba16Float,
            CaptureColorSpace::ExtendedLinearSrgb,
            1.0,
        );
        let scale_two = encoding(
            wgpu::TextureFormat::Rgba16Float,
            CaptureColorSpace::ExtendedLinearSrgb,
            2.0,
        );
        assert_ne!(
            scale_one, scale_two,
            "a brightness/SDR-white change is source metadata, not the same encoding"
        );
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
