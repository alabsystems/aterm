// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window introspection, screenshot capture, and diagnostic state serialization.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_render::Frame;

use crate::WindowId;
use crate::app_render::{
    apply_host_chrome_at, sync_cursor_effect_coordinate_space, tray_quad_below_y,
};
use crate::control::{DimsSnapshot, ImageReq};
use crate::platform::AppRt;
#[cfg(unix)]
use crate::snapshot_path;
use crate::{App, accessibility, control_auth};

#[cfg(test)]
const WORKER_PATIENCE: Duration = Duration::from_secs(60);

const DEFERRED_GPU_CAPTURE_LIMIT: usize = 8;
const GPU_CAPTURE_PENDING: &str = "capture pending: waiting for the requested GPU drawable";

/// The original request owns its deadline cancellation and artifact permit for the entire wait.
pub(crate) struct DeferredGpuCapture {
    window: WindowId,
    surface: u64,
    exact_tap: bool,
    budget: CapturePresentBudget,
    request: CaptureRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CapturePresentBudget {
    remaining: u8,
}

impl Default for CapturePresentBudget {
    fn default() -> Self {
        Self {
            remaining: crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT,
        }
    }
}

impl CapturePresentBudget {
    const fn record_attempt(&mut self, pending: bool, presented: bool, failed: bool) {
        if !pending && !presented && failed {
            self.remaining = self.remaining.saturating_sub(1);
        }
    }
}

/// One pixel capture.
pub(crate) enum CaptureRequest {
    Image(ImageReq),
    Snapshot {
        path: String,
    },
    #[cfg(any(target_os = "macos", windows))]
    Window(WindowReq),
}

/// A `window` photograph.
#[cfg(any(target_os = "macos", windows))]
pub(crate) struct WindowReq {
    target: Option<WindowId>,
    aux: AuxTarget,
    handoff: crate::control::ReplyRetentionPermit,
    path: control_auth::ConfinedImage,
    cancel: crate::control::CaptureCancellation,
    reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
}

impl CaptureRequest {
    fn target_window(&self, app: &App) -> Option<WindowId> {
        match self {
            Self::Image(request) => match request.session {
                Some(session) => app.windows_displaying(session).next(),
                None => app.frontmost_window,
            },
            Self::Snapshot { .. } => app.frontmost_window,
            #[cfg(any(target_os = "macos", windows))]
            Self::Window(request) => request.target.or(app.frontmost_window),
        }
    }

    fn charge(&self) -> CaptureCharge {
        match self {
            Self::Image(request) => {
                CaptureCharge::Frame(DestinationDemand::of_image(request.clean))
            }
            Self::Snapshot { .. } => CaptureCharge::Frame(DestinationDemand::Required),
            #[cfg(any(target_os = "macos", windows))]
            Self::Window(_) => CaptureCharge::Window,
        }
    }

    fn cancellation(&self) -> Option<&'static str> {
        match self {
            Self::Image(request) => request
                .cancel
                .is_cancelled()
                .then_some("image request cancelled before render"),
            Self::Snapshot { .. } => None,
            #[cfg(any(target_os = "macos", windows))]
            Self::Window(request) => request
                .cancel
                .is_cancelled()
                .then_some("window capture request cancelled before photograph"),
        }
    }

    const fn takes_client_tap(&self) -> bool {
        match self {
            Self::Image(request) => !request.clean,
            Self::Snapshot { .. } => true,
            #[cfg(any(target_os = "macos", windows))]
            Self::Window(_) => true,
        }
    }

    #[cfg(target_os = "macos")]
    fn bound_to(self, window: WindowId) -> Self {
        match self {
            Self::Window(request) if request.aux == AuxTarget::Front => Self::Window(WindowReq {
                target: Some(window),
                ..request
            }),
            request => request,
        }
    }

    fn reject(self, error: &str) {
        match self {
            Self::Image(request) => {
                let _ = request.reply.send(Err(error.to_string()));
            }
            Self::Snapshot { .. } => {
                crate::logging::stderr_line!("aterm-gui: snapshot refused: {error}");
            }
            #[cfg(any(target_os = "macos", windows))]
            Self::Window(request) => {
                let _ = request.reply.send(Err(error.to_string()));
            }
        }
    }

    fn reject_busy(self) {
        match self {
            Self::Image(request) => {
                crate::control_auth::cleanup_failed_automatic_image(&request.target);
                let _ = request
                    .reply
                    .send(Err(format!("image {ENCODE_QUEUE_FULL}")));
            }
            Self::Snapshot { path } => {
                match begin_snapshot_generation(std::path::Path::new(&path)) {
                    Ok(unpublished) => drop(unpublished),
                    Err(error) => {
                        crate::logging::stderr_line!(
                            "aterm-gui: snapshot refused for {path}: {error}"
                        );
                        return;
                    }
                }
                crate::logging::stderr_line!(
                    "aterm-gui: snapshot rejected for {path}: {ENCODE_QUEUE_FULL}"
                );
            }
            #[cfg(any(target_os = "macos", windows))]
            Self::Window(request) => {
                crate::control_auth::cleanup_failed_automatic_image(&request.path);
                let _ = request
                    .reply
                    .send(Err(format!("window capture {ENCODE_QUEUE_FULL}")));
            }
        }
    }

    fn reject_retargeted(self) {
        let error = match &self {
            Self::Image(_) => "image target changed while waiting for GPU acquisition",
            Self::Snapshot { .. } => "snapshot target changed while waiting for GPU acquisition",
            #[cfg(any(target_os = "macos", windows))]
            Self::Window(_) => "window capture target changed while waiting",
        };
        self.reject(error);
    }
}

fn deferred_capture_identity_matches(expected: u64, current: Option<u64>, cancelled: bool) -> bool {
    !cancelled && current == Some(expected)
}

fn deferred_capture_owns_current_tap(exact_tap: bool, expected: u64, current: Option<u64>) -> bool {
    exact_tap && current == Some(expected)
}

const fn capture_resume_owns_next_frame(
    before: u64,
    after: u64,
    pending: bool,
    retry_open: bool,
) -> bool {
    after != before || pending || !retry_open
}

fn enqueue_deferred_gpu_capture(
    queue: &mut std::collections::VecDeque<DeferredGpuCapture>,
    capture: DeferredGpuCapture,
    first: bool,
) -> Result<(), Box<DeferredGpuCapture>> {
    if queue.len() >= DEFERRED_GPU_CAPTURE_LIMIT {
        return Err(Box::new(capture));
    }
    if first {
        queue.push_front(capture);
    } else {
        queue.push_back(capture);
    }
    Ok(())
}

impl App {
    pub(crate) fn has_deferred_gpu_capture(&self, window: WindowId) -> bool {
        self.deferred_gpu_captures
            .iter()
            .any(|capture| capture.window == window)
    }

    fn queue_gpu_capture(&mut self, window: WindowId, request: CaptureRequest, first: bool) {
        self.prune_deferred_gpu_captures();
        let surface = self
            .windows
            .get(&window)
            .and_then(|state| state.current_gpu_acquire_id());
        let Some(surface) = surface else {
            request.reject("capture lost its GPU surface while waiting");
            return;
        };
        let exact_tap = first
            && request.takes_client_tap()
            && self
                .windows
                .get(&window)
                .is_some_and(|state| state.capture_acquire_armed);
        let capture = DeferredGpuCapture {
            window,
            surface,
            exact_tap,
            budget: if first {
                self.capture_present_budget
            } else {
                CapturePresentBudget::default()
            },
            request,
        };
        if let Err(capture) =
            enqueue_deferred_gpu_capture(&mut self.deferred_gpu_captures, capture, first)
        {
            if capture.exact_tap {
                self.discard_presented_client_capture(capture.window);
            }
            capture
                .request
                .reject("capture queue is full while waiting for GPU acquisition");
        }
    }

    pub(crate) fn prune_deferred_gpu_captures(&mut self) {
        let mut index = 0;
        while index < self.deferred_gpu_captures.len() {
            let capture = &self.deferred_gpu_captures[index];
            let window = self.windows.get(&capture.window);
            let window_live = window.is_some();
            let current = window.and_then(|window| window.current_gpu_acquire_id());
            let parked = window.is_some_and(|window| window.present_retry.parked);
            let cancelled = capture.request.cancellation().is_some();
            if !parked && deferred_capture_identity_matches(capture.surface, current, cancelled) {
                index += 1;
                continue;
            }
            let capture = self
                .deferred_gpu_captures
                .remove(index)
                .expect("indexed queued capture");
            let refusal = format!(
                "capture cancelled, window closed, or surface became unavailable while waiting: window={} window_live={window_live} expected_acquire={} current_acquire={} parked={parked} cancelled={cancelled}",
                capture.window.0,
                capture.surface,
                current.map_or_else(|| "none".to_string(), |id| id.to_string()),
            );
            if deferred_capture_owns_current_tap(capture.exact_tap, capture.surface, current) {
                self.discard_presented_client_capture(capture.window);
            }
            capture.request.reject(&refusal);
        }
    }

    /// THE door every pixel capture enters.
    pub(crate) fn run_capture(&mut self, request: CaptureRequest) {
        #[cfg(any(target_os = "macos", windows))]
        if let CaptureRequest::Window(window) = &request
            && let Some(refusal) = self.window_target_refusal(window.aux)
        {
            request.reject(&refusal);
            return;
        }
        let wid = request.target_window(self);
        if !self.admit_capture(wid, request.charge()) {
            request.reject_busy();
            return;
        }
        self.prune_deferred_gpu_captures();
        if let Some(cancelled) = request.cancellation() {
            request.reject(cancelled);
        } else if let Some(window) = wid
            && self.replaying_gpu_capture != wid
            && self.has_deferred_gpu_capture(window)
        {
            self.queue_gpu_capture(window, request, false);
        } else {
            if wid.is_none() || self.replaying_gpu_capture != wid {
                self.capture_present_budget = CapturePresentBudget::default();
            }
            match request {
                CaptureRequest::Image(request) => self.image_body(wid, request),
                CaptureRequest::Snapshot { path } => self.snapshot_body(wid, path),
                #[cfg(any(target_os = "macos", windows))]
                CaptureRequest::Window(request) => self.window_body(wid, request),
            }
        }
        self.capture_permit = None;
    }

    /// Consume waiting control demand before an ordinary ready redraw can take its drawable.
    pub(crate) fn resume_deferred_gpu_captures(&mut self, window: WindowId, surface: u64) -> bool {
        let budget = self.deferred_gpu_captures.len();
        let mut serviced = false;
        for _ in 0..budget {
            let Some(index) = self
                .deferred_gpu_captures
                .iter()
                .position(|capture| capture.window == window && capture.surface == surface)
            else {
                break;
            };
            let capture = self
                .deferred_gpu_captures
                .remove(index)
                .expect("selected queued capture");
            let current = self
                .windows
                .get(&window)
                .and_then(|state| state.current_gpu_acquire_id());
            if !deferred_capture_identity_matches(
                surface,
                current,
                capture.request.cancellation().is_some(),
            ) {
                if deferred_capture_owns_current_tap(capture.exact_tap, surface, current) {
                    self.discard_presented_client_capture(window);
                }
                capture
                    .request
                    .reject("capture cancelled or GPU surface changed while waiting");
                continue;
            }
            self.replaying_gpu_capture = Some(window);
            self.capture_present_budget = capture.budget;
            let before = self
                .windows
                .get(&window)
                .map_or(0, |state| state.capture_present_serial);
            let request = capture.request;
            #[cfg(target_os = "macos")]
            let request = request.bound_to(window);
            if request.target_window(self) == Some(window) {
                self.run_capture(request);
            } else {
                request.reject_retargeted();
            }
            self.replaying_gpu_capture = None;
            if capture.exact_tap
                && !self.deferred_gpu_captures.iter().any(|queued| {
                    queued.window == window && queued.surface == surface && queued.exact_tap
                })
                && self.windows.get(&window).is_some_and(|state| {
                    state.current_gpu_acquire_id() == Some(surface) && state.capture_acquire_armed
                })
            {
                self.discard_presented_client_capture(window);
            }
            serviced |= self.windows.get(&window).is_some_and(|state| {
                capture_resume_owns_next_frame(
                    before,
                    state.capture_present_serial,
                    state.gpu_acquire_pending(),
                    state.present_retry.present_attempt_allowed(),
                )
            });
            if self
                .windows
                .get(&window)
                .is_some_and(|state| state.gpu_acquire_pending())
            {
                break;
            }
        }
        serviced
    }
}

const CAPTURE_BIRTH_MAX_AGE: Duration = Duration::from_secs(10);

const CAPTURE_PREVIEW_AGE: Duration = Duration::from_millis(450);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentTake {
    Model,
    ClientTap,
    ClientTapOrModel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureTarget {
    Windowed,
    Headless,
}

#[derive(Debug)]
struct PresentedCapture {
    serial: u64,
    visuals: crate::app_render::HostVisualState,
    client: Option<CapturePixels>,
}

struct RecordingPresentedFrame {
    presented: PresentedCapture,
    input: aterm_render::RenderInput,
    terminal_grid: Option<crate::app_render::TerminalCaptureGrid>,
    facts: RecordedFacts,
}

struct RecordedFacts {
    cell_size: (usize, usize),
    theme_fingerprint: u64,
    overlay_fingerprint: u64,
    native_metadata: Option<crate::control::ImageFrameMetadata>,
}

struct RecordingPresentedDestination {
    pixels: CapturePixels,
    route: crate::VisibleContentRoute,
    serial: u64,
}

#[derive(Clone, Copy, Debug)]
struct SourcePolicy {
    destination: DestinationDemand,
    frame_only: bool,
    model: bool,
    metadata: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DestinationDemand {
    Never,
    Preferred,
    Required,
}

impl DestinationDemand {
    const fn of_image(clean: bool) -> Self {
        if clean { Self::Never } else { Self::Preferred }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureCharge {
    Frame(DestinationDemand),
    #[cfg(any(target_os = "macos", windows))]
    Window,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceKind {
    Destination,
    Rerender,
}

#[derive(Clone, Copy, Debug)]
struct SourceFacts {
    windowed: bool,
    translucent: bool,
    guards: Result<(), &'static str>,
    copyable: bool,
    recording: bool,
}

fn capture_source_kind(
    demand: DestinationDemand,
    facts: SourceFacts,
) -> Result<SourceKind, &'static str> {
    let destination = match demand {
        DestinationDemand::Never => false,
        _ if !facts.windowed => facts.recording,
        DestinationDemand::Required => {
            facts.guards?;
            true
        }
        DestinationDemand::Preferred => {
            !facts.translucent && facts.guards.is_ok() && facts.copyable
        }
    };
    Ok(if destination {
        SourceKind::Destination
    } else {
        SourceKind::Rerender
    })
}

enum CaptureSource {
    Destination {
        pixels: CapturePixels,
        serial: u64,
    },
    Rerender {
        serial: Option<u64>,
        visuals: crate::app_render::HostVisualState,
    },
}

impl CaptureSource {
    const fn serial(&self) -> Option<u64> {
        match *self {
            Self::Destination { serial, .. } => Some(serial),
            Self::Rerender { serial, .. } => serial,
        }
    }

    const fn phase(&self) -> &'static str {
        match self {
            Self::Destination { .. } => "destination",
            Self::Rerender {
                serial: Some(_), ..
            } => "presented",
            Self::Rerender { serial: None, .. } => "staged",
        }
    }
}

struct CaptureContext {
    plan: crate::tab_model::VisibleLeafPlan,
    route: crate::VisibleContentRoute,
    terminal_grid: Option<crate::app_render::TerminalCaptureGrid>,
    recorded: Option<RecordedFacts>,
}

struct ResolvedCapture {
    source: CaptureSource,
    model: Option<aterm_render::RenderInput>,
    context: CaptureContext,
}

#[derive(Debug)]
enum CaptureDeferral {
    Nothing,
    Held { retry_at: Instant },
    PendingGpu,
    Refused(String),
}

type VisibleFrameLayoutRef<'a> = (
    &'a crate::tab_model::VisibleLeafPlan,
    crate::VisibleContentRoute,
);

#[derive(Clone, Copy)]
struct ImageFrameFacts {
    phase: &'static str,
    capture_serial: Option<u64>,
    width: u32,
    height: u32,
}

/// An `image --meta` built on the main thread from the model.
pub(crate) struct PendingMetadata {
    metadata: crate::control::ImageFrameMetadata,
    raster_is_pixels: bool,
    cell: std::sync::Arc<std::sync::OnceLock<crate::control::ImageFrameMetadata>>,
}

impl PendingMetadata {
    fn publish(self, pixel_fingerprint: u64) -> Result<(), String> {
        let Self {
            mut metadata,
            raster_is_pixels,
            cell,
        } = self;
        metadata.pixel_fingerprint = pixel_fingerprint;
        if raster_is_pixels {
            metadata.raster_fingerprint = pixel_fingerprint;
            for leaf in &mut metadata.leaves {
                if leaf.raster_fingerprint.is_some() {
                    leaf.raster_fingerprint = Some(pixel_fingerprint);
                }
            }
        }
        cell.set(metadata)
            .map_err(|_| "image metadata was already initialized".to_string())
    }
}

/// A capture's pixels as their producer made them, moved to the encode worker as they are ([`EncodeJob`]).
pub(crate) struct CapturePixels {
    store: PixelStore,
    view: aterm_render::PixelRect,
}

impl std::fmt::Debug for CapturePixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let form = match &self.store {
            PixelStore::Rgba8 { .. } => "rgba8",
            PixelStore::Packed(_) => "packed",
            PixelStore::Raw(_) => "raw",
            PixelStore::Softbuffer { .. } => "softbuffer",
        };
        f.debug_struct("CapturePixels")
            .field("form", &form)
            .field("view", &self.view)
            .finish()
    }
}

enum PixelStore {
    #[cfg_attr(not(any(target_os = "macos", windows, test)), allow(dead_code))]
    Rgba8 {
        width: usize,
        bytes: Vec<u8>,
    },
    Packed(Frame),
    Raw(aterm_gpu::video_tap::RawCapture),
    Softbuffer {
        width: usize,
        pixels: Vec<u32>,
    },
}

fn exact_pixels(
    what: &str,
    len: usize,
    width: usize,
    height: usize,
    per_pixel: usize,
) -> Result<(), String> {
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(per_pixel))
        .ok_or_else(|| format!("{what} dimensions overflow"))?;
    if len != expected {
        return Err(format!(
            "{what} holds {len} units, expected {expected} for {width}x{height}"
        ));
    }
    Ok(())
}

impl CapturePixels {
    const fn whole(store: PixelStore, width: usize, height: usize) -> Self {
        Self {
            store,
            view: aterm_render::PixelRect {
                x: 0,
                y: 0,
                width,
                height,
            },
        }
    }

    #[cfg_attr(
        not(any(target_os = "macos", windows, test)),
        expect(dead_code, reason = "a window photograph is macOS's and Windows'")
    )]
    pub(crate) fn rgba8(width: u32, height: u32, bytes: Vec<u8>) -> Result<Self, String> {
        let (width, height) = (width as usize, height as usize);
        exact_pixels("capture RGBA", bytes.len(), width, height, 4)?;
        Ok(Self::whole(
            PixelStore::Rgba8 { width, bytes },
            width,
            height,
        ))
    }

    /// A rerender's frame. `Err` unless it holds `width · height` pixels.
    pub(crate) fn packed(frame: Frame) -> Result<Self, String> {
        let (width, height) = (frame.width, frame.height);
        exact_pixels("capture frame", frame.pixels.len(), width, height, 1)?;
        Ok(Self::whole(PixelStore::Packed(frame), width, height))
    }

    /// A GPU present's destination as mapped, whose geometry its tap checked.
    pub(crate) fn raw(raw: aterm_gpu::video_tap::RawCapture) -> Self {
        let (width, height) = (raw.width() as usize, raw.height() as usize);
        Self::whole(PixelStore::Raw(raw), width, height)
    }

    /// A CPU present's `width`×`height` softbuffer.
    pub(crate) fn softbuffer(
        pixels: Vec<u32>,
        width: usize,
        height: usize,
    ) -> Result<Self, String> {
        exact_pixels("capture softbuffer", pixels.len(), width, height, 1)?;
        Ok(Self::whole(
            PixelStore::Softbuffer { width, pixels },
            width,
            height,
        ))
    }

    fn cut_to(mut self, rect: aterm_render::PixelRect) -> Result<Self, String> {
        let inside = |start: usize, len: usize, bound: usize| {
            start.checked_add(len).is_some_and(|end| end <= bound)
        };
        if !inside(rect.x, rect.width, self.view.width)
            || !inside(rect.y, rect.height, self.view.height)
        {
            return Err(format!(
                "capture cut {rect:?} is outside its {}x{} pixels",
                self.view.width, self.view.height
            ));
        }
        self.view = aterm_render::PixelRect {
            x: self.view.x + rect.x,
            y: self.view.y + rect.y,
            ..rect
        };
        Ok(self)
    }

    const fn dims(&self) -> (usize, usize) {
        (self.view.width, self.view.height)
    }

    fn peak_bytes(&self) -> usize {
        let (width, height) = self.dims();
        self.allocation()
            .saturating_add(capture_encode_overhead(width, height))
    }

    const fn allocation(&self) -> usize {
        match &self.store {
            PixelStore::Rgba8 { bytes, .. } => bytes.capacity(),
            PixelStore::Packed(frame) => frame
                .pixels
                .capacity()
                .saturating_mul(std::mem::size_of::<u32>()),
            PixelStore::Raw(raw) => raw.allocation(),
            PixelStore::Softbuffer { pixels, .. } => {
                pixels.capacity().saturating_mul(std::mem::size_of::<u32>())
            }
        }
    }

    fn rgba_row(&self, y: usize, row: &mut Vec<u8>) {
        let aterm_render::PixelRect {
            x: x0,
            y: y0,
            width,
            height,
        } = self.view;
        if y >= height {
            return;
        }
        let y = y0 + y;
        match &self.store {
            PixelStore::Rgba8 {
                width: stride,
                bytes,
            } => {
                let start = (y * stride + x0) * 4;
                if let Some(scanline) = bytes.get(start..start + width * 4) {
                    row.extend_from_slice(scanline);
                }
            }
            PixelStore::Packed(frame) => {
                let start = y * frame.width + x0;
                let Some(scanline) = frame.pixels.get(start..start + width) else {
                    return;
                };
                for &pixel in scanline {
                    row.extend_from_slice(&[
                        (pixel >> 16) as u8,
                        (pixel >> 8) as u8,
                        pixel as u8,
                        255 - (pixel >> 24) as u8,
                    ]);
                }
            }
            PixelStore::Raw(raw) => raw.rgba_row(y, x0, width, row),
            PixelStore::Softbuffer {
                width: stride,
                pixels,
            } => {
                let start = y * stride + x0;
                let Some(scanline) = pixels.get(start..start + width) else {
                    return;
                };
                for &pixel in scanline {
                    row.extend_from_slice(&[
                        (pixel >> 16) as u8,
                        (pixel >> 8) as u8,
                        pixel as u8,
                        u8::MAX,
                    ]);
                }
            }
        }
    }

    fn pixel_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        note_pixel_work();
        let (width, height) = self.dims();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        width.hash(&mut hash);
        height.hash(&mut hash);
        width.saturating_mul(height).hash(&mut hash);
        if let PixelStore::Packed(frame) = &self.store {
            for y in self.view.y..self.view.y + height {
                let start = y * frame.width + self.view.x;
                u32::hash_slice(&frame.pixels[start..start + width], &mut hash);
            }
            return hash.finish() | 1;
        }
        let mut row = Vec::with_capacity(width.saturating_mul(4));
        let mut block = [0u32; 1024];
        for y in 0..height {
            row.clear();
            self.rgba_row(y, &mut row);
            for pixels in row.as_chunks::<4>().0.chunks(block.len()) {
                for (packed, &[r, g, b, a]) in block.iter_mut().zip(pixels) {
                    *packed = (u32::from(255 - a) << 24)
                        | (u32::from(r) << 16)
                        | (u32::from(g) << 8)
                        | u32::from(b);
                }
                u32::hash_slice(&block[..pixels.len()], &mut hash);
            }
        }
        hash.finish() | 1
    }
}

fn capture_encode_overhead(width: usize, height: usize) -> usize {
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return usize::MAX;
    };
    let png = aterm_png::max_encoded_len(width, height, aterm_png::ColorType::Rgba);
    let scratch = aterm_png::encoder_scratch_len(width, aterm_png::ColorType::Rgba);
    png.zip(scratch)
        .and_then(|(png, scratch)| png.checked_add(scratch))
        .unwrap_or(usize::MAX)
}

fn frame_capture_peak(width: usize, height: usize, destination: Option<usize>) -> usize {
    width
        .saturating_mul(height)
        .saturating_mul(4)
        .max(destination.unwrap_or(0))
        .saturating_add(capture_encode_overhead(width, height))
}

/// One encoded capture.
pub(crate) struct CapturePng {
    width: u32,
    height: u32,
    png: Vec<u8>,
}

impl CapturePng {
    /// THE capture encoder.
    pub(crate) fn encode(pixels: &CapturePixels) -> Result<Self, String> {
        note_pixel_work();
        let (width, height) = pixels.dims();
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return Err("capture is larger than a PNG can describe".to_string());
        };
        let png = encode_srgb_rgba8(width, height, |writer| {
            writer.write_rows(|y, row| pixels.rgba_row(y, row))
        })?;
        Ok(Self { width, height, png })
    }
}

/// A fixture frame encoded as a rerender's capture is.
#[cfg(test)]
pub(crate) fn encode_frame_png(frame: &Frame) -> CapturePng {
    CapturePixels::packed(frame.clone())
        .and_then(|pixels| CapturePng::encode(&pixels))
        .expect("a fixture frame encodes")
}

pub(crate) fn encode_srgb_rgba8(
    width: u32,
    height: u32,
    write: impl FnOnce(&mut aterm_png::Writer<&mut Vec<u8>>) -> Result<(), aterm_png::EncodingError>,
) -> Result<Vec<u8>, String> {
    let bound = aterm_png::max_encoded_len(width, height, aterm_png::ColorType::Rgba)
        .ok_or_else(|| "capture is larger than a PNG can describe".to_string())?;
    let mut out = Vec::with_capacity(bound);
    {
        let mut encoder = aterm_png::Encoder::new(&mut out, width, height);
        encoder.set_color(aterm_png::ColorType::Rgba);
        encoder.set_depth(aterm_png::BitDepth::Eight);
        encoder.set_source_srgb(aterm_png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        write(&mut writer).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    out.shrink_to_fit();
    Ok(out)
}

fn hash_raster(width: u32, height: u32, rgba: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hash = std::collections::hash_map::DefaultHasher::new();
    width.hash(&mut hash);
    height.hash(&mut hash);
    rgba.hash(&mut hash);
    hash.finish() | 1
}

impl crate::SettingsCard {
    /// This card's `raster-fingerprint`, hashed once per card.
    pub(crate) fn raster_fingerprint(&self) -> u64 {
        *self
            .raster_fingerprint_once
            .get_or_init(|| hash_raster(self.pw, self.ph, &self.rgba))
    }
}

impl crate::NativeLeafRaster {
    fn raster_fingerprint(&self) -> u64 {
        *self
            .raster_fingerprint_once
            .get_or_init(|| hash_raster(self.width, self.height, &self.rgba))
    }
}

fn bounded_capture_birth(now: Instant, pending: Option<Instant>) -> Option<Instant> {
    pending.map(|stamp| {
        let age = now
            .saturating_duration_since(stamp)
            .min(CAPTURE_BIRTH_MAX_AGE);
        now.checked_sub(age).unwrap_or(now)
    })
}

fn capture_rescan_birth(now: Instant, pending: Option<Instant>, windowless: bool) -> Instant {
    let birth = bounded_capture_birth(now, pending).unwrap_or(now);
    if windowless || now.saturating_duration_since(birth) > CAPTURE_PREVIEW_AGE {
        now.checked_sub(CAPTURE_PREVIEW_AGE).unwrap_or(now)
    } else {
        birth
    }
}

fn terminal_capture_text(
    input: &aterm_render::RenderInput,
    strip_rows: usize,
    cols: usize,
) -> String {
    let start = strip_rows.min(input.cells.len());
    let mut text = String::with_capacity(input.cells.len().saturating_sub(start) * (cols + 1));
    for cells in &input.cells[start..] {
        accessibility::push_visible_row(&mut text, cells, cols);
    }
    text
}

#[cfg(not(target_os = "macos"))]
fn non_macos_chrome_output(
    mut platform_lines: Vec<String>,
    toolbar_tabs: Option<String>,
    strip_version: Option<String>,
    strip_caption: Option<String>,
    ime: Option<String>,
    tab_menu_lines: Vec<String>,
    menu_lines: Vec<String>,
) -> Vec<String> {
    platform_lines.push("toolbar (none)".to_string());
    if let Some(toolbar_tabs) = toolbar_tabs {
        platform_lines.push(toolbar_tabs);
    }
    if let Some(strip_version) = strip_version {
        platform_lines.push(strip_version);
    }
    if let Some(strip_caption) = strip_caption {
        platform_lines.push(strip_caption);
    }
    if let Some(ime) = ime {
        platform_lines.push(ime);
    }
    platform_lines.extend(tab_menu_lines);
    platform_lines.extend(menu_lines);
    platform_lines
}

#[cfg(any(test, not(target_os = "macos")))]
fn ime_chrome_line(
    preedit: &str,
    caret: Option<usize>,
    spot: Option<(i32, i32, u32, u32)>,
) -> String {
    let caret = caret.map_or_else(|| "-".to_string(), |c| c.to_string());
    let spot = spot.map_or_else(
        || "-".to_string(),
        |(x, y, w, h)| format!("{x},{y},{w}x{h}"),
    );
    format!("ime preedit={preedit:?} caret={caret} spot={spot}")
}

#[cfg(any(test, not(target_os = "macos")))]
fn strip_caption_chrome_line(
    segments: &[crate::tab_bar::TabSegment],
    frame: Option<crate::client_frame::ClientFrame>,
    hovered: Option<crate::tab_bar::CaptionButton>,
) -> String {
    let Some(frame) = frame else {
        return "strip-caption frame=system".to_string();
    };
    let name = |button: crate::tab_bar::CaptionButton| match button {
        crate::tab_bar::CaptionButton::Minimize => "minimize",
        crate::tab_bar::CaptionButton::Maximize => "maximize",
        crate::tab_bar::CaptionButton::Restore => "restore",
        crate::tab_bar::CaptionButton::Close => "close",
    };
    let buttons: Vec<String> = segments
        .iter()
        .filter_map(|seg| match seg.kind {
            crate::tab_bar::TabHit::Caption(button) => Some(format!(
                "{}:{}..{}",
                name(button),
                seg.start_col,
                seg.end_col
            )),
            _ => None,
        })
        .collect();
    format!(
        "strip-caption frame=client buttons={} hovered={} maximized={} edges={} menu={}",
        if buttons.is_empty() {
            "none".to_string()
        } else {
            buttons.join(",")
        },
        hovered.map_or("none", name),
        frame.maximized,
        frame.edges,
        frame.window_menu,
    )
}

#[cfg(any(test, not(target_os = "macos")))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StripVersionAbsence {
    NoStrip,
    NotLaidOut,
    NoUiFace,
    StripFull,
}

#[cfg(any(test, not(target_os = "macos")))]
impl StripVersionAbsence {
    fn token(self) -> &'static str {
        match self {
            Self::NoStrip => "no-strip",
            Self::NotLaidOut => "not-laid-out",
            Self::NoUiFace => "no-ui-face",
            Self::StripFull => "strip-full",
        }
    }
}

#[cfg(any(test, not(target_os = "macos")))]
fn strip_version_chrome_line(
    segments: &[crate::tab_bar::TabSegment],
    hovered: bool,
    absent: StripVersionAbsence,
    attention: bool,
) -> String {
    let laid_out = segments.iter().find_map(|seg| match seg.kind {
        crate::tab_bar::TabHit::Version { rung, attention } => Some((seg, rung, attention)),
        _ => None,
    });
    match laid_out {
        Some((seg, rung, attention)) => {
            let text = crate::menu::strip_version_label(rung, attention).unwrap_or_default();
            let rung = match rung {
                crate::menu::VersionRung::Full => "full",
                crate::menu::VersionRung::Short => "short",
            };
            let action = crate::menu::version_slot_click(attention);
            format!(
                "strip-version shown=yes rung={rung} cols={}..{} text={text:?} \
                 attention={attention} hovered={hovered} action={action:?}",
                seg.start_col, seg.end_col
            )
        }
        None => {
            let text = crate::menu::strip_version_label(crate::menu::VersionRung::Full, attention)
                .unwrap_or_default();
            let action = crate::menu::version_slot_click(attention);
            format!(
                "strip-version shown=no reason={} text={text:?} attention={attention} \
                 action={action:?}",
                absent.token()
            )
        }
    }
}

pub(crate) enum EncodeJob {
    Image {
        pixels: CapturePixels,
        /// Boxed: an `image --meta`'s model is most of an image job, and unboxed, whether it
        /// clears clippy's `large_enum_variant` margin turns on the variants a platform compiles.
        metadata: Option<Box<PendingMetadata>>,
        target: control_auth::ConfinedImage,
        want_bytes: bool,
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::ImageReply>,
        handoff: crate::control::ReplyRetentionPermit,
    },
    Snapshot {
        pixels: CapturePixels,
        text: String,
        transaction: SnapshotTransaction,
    },
    #[cfg(any(target_os = "macos", windows))]
    Window {
        stitch: WindowStitch,
        target: control_auth::ConfinedImage,
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
        handoff: crate::control::ReplyRetentionPermit,
    },
    VideoDump {
        /// The finished recording, its last conversions settled on the export's lane.
        take: aterm_gpu::video_tap::UnsettledTake,
        mode: crate::VideoMode,
        keys_enabled: bool,
        inputs: Vec<(u64, crate::VideoInputSample)>,
        pace_ticks: Vec<u64>,
        unlogged_inputs: u64,
        unlogged_other_window: u64,
        unlogged_over_cap: u64,
        started_us: u64,
        dir: Box<crate::control_auth::ConfinedVideoDir>,
        reply: std::sync::mpsc::Sender<crate::control::Retained<String>>,
        cancel: crate::VideoCancellation,
        permit: crate::VideoExportPermit,
        handoff: crate::control::ReplyRetentionPermit,
    },
}

const ENCODE_QUEUE_CAPACITY: usize = 4;

const CAPTURE_BYTE_BUDGET: usize = 64 * 1024 * 1024;

/// One capture's bytes ([`CapturePixels::peak_bytes`]).
pub(crate) struct CapturePermit {
    held: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    bytes: usize,
}

impl CapturePermit {
    fn try_reserve(
        held: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
        bytes: usize,
    ) -> Option<Self> {
        use std::sync::atomic::Ordering;
        held.try_update(Ordering::AcqRel, Ordering::Acquire, |in_flight| {
            let next = in_flight.checked_add(bytes)?;
            (in_flight == 0 || next <= CAPTURE_BYTE_BUDGET).then_some(next)
        })
        .ok()?;
        Some(Self {
            held: std::sync::Arc::clone(held),
            bytes,
        })
    }

    fn recharge(&mut self, actual: usize) {
        use std::sync::atomic::Ordering;
        if actual >= self.bytes {
            self.held.fetch_add(actual - self.bytes, Ordering::AcqRel);
        } else {
            self.held.fetch_sub(self.bytes - actual, Ordering::AcqRel);
        }
        self.bytes = actual;
    }
}

impl Drop for CapturePermit {
    fn drop(&mut self) {
        self.held
            .fetch_sub(self.bytes, std::sync::atomic::Ordering::AcqRel);
    }
}

/// What travels to the encode worker.
pub(crate) struct EncodeWork {
    job: EncodeJob,
    permit: Option<CapturePermit>,
}

impl EncodeJob {
    fn peak_bytes(&self) -> usize {
        match self {
            Self::Image { pixels, .. } | Self::Snapshot { pixels, .. } => pixels.peak_bytes(),
            #[cfg(any(target_os = "macos", windows))]
            Self::Window { stitch, .. } => stitch.peak_bytes(),
            Self::VideoDump { .. } => 0,
        }
    }
}

#[cfg(test)]
thread_local! {
    static CAPTURE_BODIES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
static ENCODE_THREADS: std::sync::Mutex<Vec<std::thread::ThreadId>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
static PIXEL_WORK_THREADS: std::sync::Mutex<Vec<std::thread::ThreadId>> =
    std::sync::Mutex::new(Vec::new());

fn note_pixel_work() {
    #[cfg(test)]
    {
        let id = std::thread::current().id();
        let mut threads = PIXEL_WORK_THREADS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !threads.contains(&id) {
            threads.push(id);
        }
    }
}

/// Whether `thread` ran a capture's whole-frame pixel pass.
#[cfg(test)]
pub(crate) fn ran_pixel_work(thread: std::thread::ThreadId) -> bool {
    PIXEL_WORK_THREADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&thread)
}

fn note_capture_body() {
    #[cfg(test)]
    CAPTURE_BODIES.with(|bodies| bodies.set(bodies.get() + 1));
}

/// The capture bodies admitted on this thread so far.
#[cfg(test)]
pub(crate) fn capture_bodies() -> u64 {
    CAPTURE_BODIES.with(std::cell::Cell::get)
}

const ENCODE_QUEUE_FULL: &str = "encode queue full; retry";
const ENCODE_WORKER_UNAVAILABLE: &str = "encode worker unavailable; retry";

fn reject_encode_job(mut job: EncodeJob, reason: &str) {
    match &mut job {
        EncodeJob::Image { target, reply, .. } => {
            crate::control_auth::cleanup_failed_automatic_image(target);
            let _ = reply.send(Err(format!("image {reason}")));
        }
        EncodeJob::Snapshot { transaction, .. } => {
            crate::logging::stderr_line!(
                "aterm-gui: snapshot rejected for {}: {reason}",
                transaction.target.path.display()
            );
        }
        #[cfg(any(target_os = "macos", windows))]
        EncodeJob::Window { target, reply, .. } => {
            crate::control_auth::cleanup_failed_automatic_image(target);
            let _ = reply.send(Err(format!("window capture {reason}")));
        }
        EncodeJob::VideoDump { dir, reply, .. } => fail_video_dump_in_place(dir, reply, reason),
    }
}

#[cfg(test)]
fn snapshot_sidecar_path(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    sidecar.into()
}

#[derive(Clone, Debug)]
struct SnapshotTarget {
    path: std::path::PathBuf,
    dir: crate::pinned_dir::PinnedDir,
    png: std::ffi::OsString,
    text: std::ffi::OsString,
    done: std::ffi::OsString,
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotTransaction {
    generation: u64,
    target: SnapshotTarget,
}

#[cfg(test)]
impl SnapshotTransaction {
    #[must_use]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

fn sidecar_name(file_name: &std::ffi::OsStr, suffix: &str) -> std::ffi::OsString {
    let mut name = file_name.to_os_string();
    name.push(suffix);
    name
}

fn sha256_hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

fn clear_snapshot_completion(target: &SnapshotTarget) -> std::io::Result<()> {
    clear_snapshot_completion_with_sync(target, |dir| dir.sync())
}

fn clear_snapshot_completion_with_sync(
    target: &SnapshotTarget,
    sync: impl FnOnce(&crate::pinned_dir::PinnedDir) -> std::io::Result<()>,
) -> std::io::Result<()> {
    target.dir.remove_file_if_exists(&target.done)?;
    sync(&target.dir)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotCommitPlan {
    Publish,
    DiscardStale,
}

#[must_use]
const fn snapshot_commit_plan(job_generation: u64, latest_generation: u64) -> SnapshotCommitPlan {
    if job_generation == latest_generation {
        SnapshotCommitPlan::Publish
    } else {
        SnapshotCommitPlan::DiscardStale
    }
}

type SnapshotGenerations = std::collections::BTreeMap<std::path::PathBuf, u64>;

fn snapshot_generation_fence() -> &'static std::sync::Mutex<SnapshotGenerations> {
    static FENCE: std::sync::OnceLock<std::sync::Mutex<SnapshotGenerations>> =
        std::sync::OnceLock::new();
    FENCE.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SnapshotGenerationCommit",
        action = "BeginNew",
        project = "aterm_gui::artifact_transaction_conformance::project_snapshot"
    )
)]
pub(crate) fn begin_snapshot_generation(
    path: &std::path::Path,
) -> Result<SnapshotTransaction, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| "snapshot path has no filename".to_string())?;
    if !crate::pinned_dir::is_one_component(file_name) {
        return Err("snapshot filename is not one path component".to_string());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let dir = crate::pinned_dir::PinnedDir::open_resolved(parent)
        .map_err(|error| format!("could not pin snapshot directory: {error}"))?;
    let canonical_path = dir.path().join(file_name);
    let mut generations = snapshot_generation_fence()
        .lock()
        .map_err(|_| "snapshot generation fence is poisoned".to_string())?;
    let next = generations
        .get(&canonical_path)
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "snapshot generation counter exhausted".to_string())?;
    generations.insert(canonical_path.clone(), next);
    let target = SnapshotTarget {
        path: canonical_path,
        dir,
        png: file_name.to_os_string(),
        text: sidecar_name(file_name, ".txt"),
        done: sidecar_name(file_name, ".done"),
    };
    clear_snapshot_completion(&target)
        .map_err(|error| format!("could not clear stale completion marker: {error}"))?;
    target
        .dir
        .validate_path_identity()
        .map_err(|error| format!("snapshot directory changed during begin: {error}"))?;
    Ok(SnapshotTransaction {
        generation: next,
        target,
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SnapshotGenerationCommit",
        action = "CommitOld",
        project = "aterm_gui::artifact_transaction_conformance::project_snapshot"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SnapshotGenerationCommit",
        action = "CommitCurrent",
        project = "aterm_gui::artifact_transaction_conformance::project_snapshot"
    )
)]
pub(crate) fn write_snapshot_artifacts(
    encoded: &CapturePng,
    text: &str,
    transaction: &SnapshotTransaction,
) -> Result<(), String> {
    write_snapshot_artifacts_with_hooks(encoded, text, transaction, || {}, || {})
}

/// [`write_snapshot_artifacts`], with `before_done` run once the payload is
/// written and `on_done` the moment the `.done` marker is visible: its
/// appearance is the writer's last call on the directory, so a consumer may
/// read the payload or remove the directory as soon as it sees the marker.
fn write_snapshot_artifacts_with_hooks(
    encoded: &CapturePng,
    text: &str,
    transaction: &SnapshotTransaction,
    before_done: impl FnOnce(),
    on_done: impl FnOnce(),
) -> Result<(), String> {
    let generation = transaction.generation;
    let target = &transaction.target;
    let png = &encoded.png;

    let cleanup_names = || {
        let _ = target.dir.remove_file_if_exists(&target.png);
        let _ = target.dir.remove_file_if_exists(&target.text);
        let _ = target.dir.remove_file_if_exists(&target.done);
        let _ = target.dir.sync();
    };

    let generations = snapshot_generation_fence()
        .lock()
        .map_err(|_| "snapshot generation fence is poisoned".to_string())?;
    let latest = generations.get(&target.path).copied().unwrap_or(0);
    if snapshot_commit_plan(generation, latest) == SnapshotCommitPlan::DiscardStale {
        return Err(format!(
            "snapshot generation {generation} was superseded by {latest}"
        ));
    }
    let png_file = target
        .dir
        .write_private(&target.png, png)
        .map_err(|error| {
            cleanup_names();
            format!("PNG write failed: {error}")
        })?;
    let text_file = match target.dir.write_private(&target.text, text.as_bytes()) {
        Ok(file) => file,
        Err(error) => {
            let _ = png_file.remove_exact();
            let _ = target.dir.remove_file_if_exists(&target.text);
            let _ = target.dir.remove_file_if_exists(&target.done);
            let _ = target.dir.sync();
            return Err(format!("text write failed: {error}"));
        }
    };
    before_done();
    if let Err(error) = png_file
        .validate_path_identity()
        .and_then(|()| text_file.validate_path_identity())
    {
        let _ = png_file.remove_exact();
        let _ = text_file.remove_exact();
        let _ = target.dir.remove_file_if_exists(&target.done);
        let _ = target.dir.sync();
        return Err(format!("payload identity changed before commit: {error}"));
    }
    let done = format!(
        "{}x{}\ngeneration={generation}\npng_sha256={}\ntext_sha256={}\n",
        encoded.width,
        encoded.height,
        sha256_hex(png),
        sha256_hex(text.as_bytes())
    );
    let done_file =
        match target
            .dir
            .write_new_private_marker(&target.done, done.as_bytes(), on_done)
        {
            Ok(file) => file,
            Err(error) => {
                let _ = png_file.remove_exact();
                let _ = text_file.remove_exact();
                let _ = target.dir.remove_file_if_exists(&target.done);
                let _ = target.dir.sync();
                return Err(format!("completion marker write failed: {error}"));
            }
        };
    let _commit_guards = (png_file, text_file, done_file);
    Ok(())
}

/// The honesty tokens a `video ... keys` recording adds to its OK reply line.
pub(crate) fn video_keys_reply_tokens(keys_enabled: bool, logged: usize, unlogged: u64) -> String {
    if keys_enabled {
        format!(" inputs={logged} unlogged_inputs={unlogged}")
    } else {
        String::new()
    }
}

/// The same disclosure for the mid-flight `video status` line.
pub(crate) fn video_keys_status_tokens(keys_enabled: bool, logged: usize, unlogged: u64) -> String {
    if keys_enabled {
        format!(" keys=true inputs={logged} unlogged={unlogged}")
    } else {
        " keys=false".to_string()
    }
}

/// The `analysis.note` for a recording whose `inputs[]` came out empty.
pub(crate) fn video_empty_ledger_note(
    keys_enabled: bool,
    unlogged: u64,
    unlogged_other_window: u64,
) -> String {
    if !keys_enabled {
        return "no input ledger: this take did not pass the `keys` flag; record \
                `video <secs> keys` (owner scope) to correlate input with frames."
            .to_string();
    }
    if unlogged > 0 {
        let control_thread = unlogged.saturating_sub(unlogged_other_window);
        let mut causes = String::new();
        if control_thread > 0 {
            causes.push_str(&format!(
                " {control_thread} took the control-thread path, which bypasses the ledger: a \
                 verb aimed at any session but the front tab's focused pane (`@<sid>` naming a \
                 background tab or an unfocused split pane, or `@self` sent from one), a fenced \
                 `key`/`send` (`if=`, `if-gen=`, `if-fp=`, `if-human=`) wherever it is aimed, or \
                 aterm's own guarded presses anywhere in this process (a `turn` submit guard, a \
                 fact-menu slash command, a worker's launch line). Drive the front tab's focused \
                 pane instead: flagless `key`/`ctrl`/`send`/`feed`/`paste`, or `@<sid>` naming it."
            ));
        }
        if unlogged_other_window > 0 {
            causes.push_str(&format!(
                " {unlogged_other_window} reached a window this take was not recording (the \
                 front window changed mid-take), so no frame can answer them. Keep the target \
                 window front for the whole take."
            ));
        }
        return format!(
            "MEASUREMENT GAP: {unlogged} input attempt(s) during this take could not be \
             stamped on its frame clock, so inputs[] is empty despite them.{causes}"
        );
    }
    "measured zero: the ledger saw no input attempt during this take, and unlogged_inputs is \
     0. Flagless `key`/`ctrl`/`send`/`feed`/`paste` aimed at the front tab's focused pane, and \
     hardware keys on the recorded window, are all logged."
        .to_string()
}

#[cfg(test)]
#[path = "app_introspect_video_keys_honesty_tests.rs"]
mod video_keys_honesty_tests;

fn fail_video_dump(
    mut dir: Box<crate::control_auth::ConfinedVideoDir>,
    reply: &std::sync::mpsc::Sender<crate::control::Retained<String>>,
    error: impl std::fmt::Display,
) {
    fail_video_dump_in_place(&mut dir, reply, error);
}

fn fail_video_dump_in_place(
    dir: &mut crate::control_auth::ConfinedVideoDir,
    reply: &std::sync::mpsc::Sender<crate::control::Retained<String>>,
    error: impl std::fmt::Display,
) {
    let cleanup = match dir.abort_in_place() {
        Ok(()) => String::new(),
        Err(cleanup_error) => format!("; partial export cleanup failed: {cleanup_error}"),
    };
    let _ = reply.send(crate::control::Retained::plain(format!(
        "ERR video: export failed: {error}{cleanup}\n"
    )));
}

struct VideoPublication {
    frame_seals: Vec<crate::control_auth::VideoFrameSeal>,
    index_file: crate::pinned_dir::PinnedFile,
    published_marker: Option<crate::pinned_dir::PinnedFile>,
    dir: crate::control_auth::ConfinedVideoDir,
}

impl VideoPublication {
    fn prepare(&mut self) -> std::io::Result<std::path::PathBuf> {
        self.dir.publish(&self.frame_seals, &self.index_file)
    }

    fn validate_for_reply(&self) -> std::io::Result<()> {
        self.dir
            .validate_for_reply(&self.frame_seals, &self.index_file)?;
        if let Some(marker) = &self.published_marker {
            marker.validate_entry_identity_at_retained()?;
        }
        Ok(())
    }

    fn publish_marker(&mut self) -> std::io::Result<()> {
        let marker = self.dir.publish_marker()?;
        marker.validate_entry_identity_at_retained()?;
        self.published_marker = Some(marker);
        Ok(())
    }

    #[cfg(all(test, unix))]
    fn publish_marker_with_test_hook(
        &mut self,
        after_publish: impl FnOnce(),
    ) -> std::io::Result<()> {
        let marker = self.dir.publish_marker_with_test_hook(after_publish)?;
        marker.validate_entry_identity_at_retained()?;
        self.published_marker = Some(marker);
        Ok(())
    }

    #[cfg(all(test, unix))]
    fn is_published(&self) -> bool {
        self.dir.is_published()
    }

    fn abort(self) -> std::io::Result<()> {
        let Self {
            frame_seals,
            index_file,
            published_marker,
            dir,
        } = self;
        drop(frame_seals);
        drop(index_file);
        drop(published_marker);
        dir.abort()
    }
}

fn fail_video_publication(
    publication: VideoPublication,
    reply: &std::sync::mpsc::Sender<crate::control::Retained<String>>,
    error: impl std::fmt::Display,
) {
    let cleanup = match publication.abort() {
        Ok(()) => String::new(),
        Err(cleanup_error) => format!("; partial export cleanup failed: {cleanup_error}"),
    };
    let _ = reply.send(crate::control::Retained::plain(format!(
        "ERR video: export failed: {error}{cleanup}\n"
    )));
}

struct VideoReplyRetention {
    publication: VideoPublication,
}

impl VideoReplyRetention {
    fn prepare_write_with(
        &mut self,
        publish_marker: impl FnOnce(&mut VideoPublication) -> std::io::Result<()>,
    ) -> Result<(), String> {
        self.publication
            .validate_for_reply()
            .map_err(|error| format!("video identity changed before wire reply: {error}"))?;
        publish_marker(&mut self.publication)
            .map_err(|error| format!("video publish marker failed before wire reply: {error}"))?;
        self.publication
            .validate_for_reply()
            .map_err(|error| format!("video identity changed at wire reply: {error}"))?;
        Ok(())
    }
}

impl crate::control::WireRetention for VideoReplyRetention {
    fn prepare_write(&mut self) -> Result<(), String> {
        self.prepare_write_with(VideoPublication::publish_marker)
    }
}

fn send_video_reply_with_retention(
    publication: VideoPublication,
    permit: crate::control::ReplyRetentionPermit,
    reply: &std::sync::mpsc::Sender<crate::control::Retained<String>>,
    value: String,
) {
    let guard = VideoReplyRetention { publication };
    let retention = permit.retain(guard);
    let _ = reply.send(crate::control::Retained::guarded(value, retention));
}

struct CaptureReplyRetention {
    target: crate::control_auth::ConfinedImage,
    file: Option<crate::pinned_dir::PinnedFile>,
    _lease: Option<crate::control_auth::ArtifactPathLease>,
    committed: bool,
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReplyPublication",
        action = "AbortAuthorized",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn abort_authorized_artifact_anchor() {}

impl crate::control::WireRetention for CaptureReplyRetention {
    fn prepare_write(&mut self) -> Result<(), String> {
        self.target
            .validate_for_reply(self.file.as_ref().expect("live capture reply guard"))
            .map_err(|error| format!("capture identity changed before wire reply: {error}"))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for CaptureReplyRetention {
    fn drop(&mut self) {
        if self.committed {
            crate::control_auth::ConfinedImage::arm_automatic_prune(&self.target);
        } else if let Some(file) = self.file.take() {
            let _ = file.remove_exact();
        }
    }
}

struct PendingCaptureReply {
    target: crate::control_auth::ConfinedImage,
    file: crate::pinned_dir::PinnedFile,
    lease: Option<crate::control_auth::ArtifactPathLease>,
    permit: crate::control::ReplyRetentionPermit,
}

fn send_capture_reply_after_validation<T: Send>(
    pending: PendingCaptureReply,
    reply: &std::sync::mpsc::Sender<Result<crate::control::Retained<T>, String>>,
    value: T,
    context: &str,
) -> bool {
    if let Err(error) = pending.target.validate_for_reply(&pending.file) {
        return reject_pending_capture_reply(pending, reply, context, error);
    }
    let PendingCaptureReply {
        permit,
        lease,
        file,
        target,
    } = pending;
    let guard = CaptureReplyRetention {
        target,
        file: Some(file),
        _lease: lease,
        committed: false,
    };
    let retention = permit.retain(guard);
    let _ = reply.send(Ok(crate::control::Retained::guarded(value, retention)));
    true
}

fn reject_pending_capture_reply<T>(
    pending: PendingCaptureReply,
    reply: &std::sync::mpsc::Sender<Result<crate::control::Retained<T>, String>>,
    context: &str,
    error: std::io::Error,
) -> bool {
    abort_authorized_artifact_anchor();
    let _ = pending.file.remove_exact();
    let _ = reply.send(Err(format!(
        "{context} path identity changed at reply barrier: {error}"
    )));
    false
}

/// Queue one authorized capture exactly as the encode worker does.
#[cfg(test)]
pub(crate) fn queue_capture_reply_for_test(
    target: crate::control_auth::ConfinedImage,
    file: crate::pinned_dir::PinnedFile,
    lease: Option<crate::control_auth::ArtifactPathLease>,
) -> Result<crate::control::Retained<()>, String> {
    let (reply, result) = std::sync::mpsc::channel();
    send_capture_reply_after_validation(
        PendingCaptureReply {
            target,
            file,
            lease,
            permit: crate::control::ReplyRetentionPermit::from_own_budget(),
        },
        &reply,
        (),
        "image",
    );
    result.recv().expect("the capture worker always answers")
}

#[cfg(all(test, unix))]
fn send_capture_reply_after_validation_with_hook<T: Send>(
    pending: PendingCaptureReply,
    reply: &std::sync::mpsc::Sender<Result<crate::control::Retained<T>, String>>,
    value: T,
    context: &str,
    before_send: impl FnOnce(),
) -> bool {
    if let Err(error) = pending.target.validate_for_reply(&pending.file) {
        return reject_pending_capture_reply(pending, reply, context, error);
    }
    before_send();
    send_capture_reply_after_validation(pending, reply, value, context)
}

fn run_encode_job(job: EncodeJob, permit: Option<CapturePermit>) {
    #[cfg(test)]
    ENCODE_THREADS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(std::thread::current().id());
    match job {
        EncodeJob::Image {
            handoff,
            reply,
            cancel,
            target,
            want_bytes,
            pixels,
            metadata,
        } => {
            if cancel.is_cancelled() {
                drop((pixels, permit));
                let _ = reply.send(Err(if want_bytes {
                    "image request cancelled before byte reply".to_string()
                } else {
                    "image request cancelled before encoding".to_string()
                }));
                return;
            }
            if let Some(metadata) = metadata
                && let Err(error) = metadata.publish(pixels.pixel_fingerprint())
            {
                drop((pixels, permit));
                let _ = reply.send(Err(error));
                return;
            }
            let encoded = CapturePng::encode(&pixels);
            drop((pixels, permit));
            let CapturePng {
                width: w,
                height: h,
                png,
            } = match encoded {
                Ok(encoded) => encoded,
                Err(error) => {
                    let _ = reply.send(Err(format!("image PNG encode failed: {error}")));
                    return;
                }
            };
            if want_bytes {
                let retention = handoff.release_path_and_retain_in_memory(target);
                let _ = reply.send(Ok(crate::control::Retained::write_retained(
                    (w, h, Some(png)),
                    retention,
                )));
                return;
            }
            let lease = match crate::control_auth::acquire_capture_name_lease(&target, || {
                cancel.is_cancelled()
            }) {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let _ = reply.send(Err(
                        "image request cancelled while waiting for its output name".to_string(),
                    ));
                    return;
                }
                Err(error) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let _ = reply.send(Err(format!("image output-name lease failed: {error}")));
                    return;
                }
            };
            let write = target.write_private_authorized(&png, || cancel.authorize_commit());
            drop(png);
            match write {
                Ok(file) => {
                    send_capture_reply_after_validation(
                        PendingCaptureReply {
                            target,
                            file,
                            lease: Some(lease),
                            permit: handoff,
                        },
                        &reply,
                        (w, h, None),
                        "image",
                    );
                }
                Err(error) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let message = if error.kind() == std::io::ErrorKind::Interrupted
                        && cancel.is_cancelled()
                    {
                        "image request cancelled before publication".to_string()
                    } else {
                        format!("image write failed: {error}")
                    };
                    let _ = reply.send(Err(message));
                }
            }
        }
        EncodeJob::Snapshot {
            pixels,
            text,
            transaction,
        } => {
            let path = transaction.target.path.display();
            let encoded = CapturePng::encode(&pixels);
            drop((pixels, permit));
            match encoded
                .and_then(|encoded| write_snapshot_artifacts(&encoded, &text, &transaction))
            {
                Ok(()) => crate::logging::stderr_line!(
                    "aterm-gui: snapshot written to {path} (+ .txt, .done)"
                ),
                Err(error) => {
                    crate::logging::stderr_line!("aterm-gui: snapshot failed for {path}: {error}")
                }
            }
        }
        #[cfg(any(target_os = "macos", windows))]
        EncodeJob::Window {
            handoff,
            reply,
            cancel,
            target,
            stitch,
        } => {
            if cancel.is_cancelled() {
                drop((stitch, permit));
                let _ = reply.send(Err(
                    "window capture request cancelled before encoding".to_string()
                ));
                return;
            }
            let pixels = match stitch.finish() {
                Ok(pixels) => pixels,
                Err(error) => {
                    drop(permit);
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            let encoded = CapturePng::encode(&pixels);
            drop((pixels, permit));
            let CapturePng { width, height, png } = match encoded {
                Ok(encoded) => encoded,
                Err(error) => {
                    let _ = reply.send(Err(format!(
                        "window capture failed (PNG encode error: {error})"
                    )));
                    return;
                }
            };
            let lease = match crate::control_auth::acquire_capture_name_lease(&target, || {
                cancel.is_cancelled()
            }) {
                Ok(Some(lease)) => lease,
                Ok(None) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let _ = reply.send(Err(
                        "window capture request cancelled while waiting for its output name"
                            .to_string(),
                    ));
                    return;
                }
                Err(error) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let _ = reply.send(Err(format!(
                        "window capture output-name lease failed: {error}"
                    )));
                    return;
                }
            };
            let write = target.write_private_authorized(&png, || cancel.authorize_commit());
            drop(png);
            match write {
                Ok(file) => {
                    send_capture_reply_after_validation(
                        PendingCaptureReply {
                            target,
                            file,
                            lease: Some(lease),
                            permit: handoff,
                        },
                        &reply,
                        (width, height),
                        "window capture",
                    );
                }
                Err(error) => {
                    crate::control_auth::cleanup_failed_automatic_image(&target);
                    let message = if error.kind() == std::io::ErrorKind::Interrupted
                        && cancel.is_cancelled()
                    {
                        "window capture request cancelled before publication".to_string()
                    } else {
                        format!("window capture failed (write error: {error})")
                    };
                    let _ = reply.send(Err(message));
                }
            }
        }
        EncodeJob::VideoDump {
            permit: _permit,
            handoff,
            reply,
            cancel,
            dir,
            mode,
            keys_enabled,
            inputs,
            pace_ticks,
            unlogged_inputs,
            unlogged_other_window,
            unlogged_over_cap,
            started_us,
            take,
        } => {
            use std::fmt::Write as _;

            if cancel.is_cancelled() {
                fail_video_dump(dir, &reply, "request cancelled before export");
                return;
            }
            // The frames the conversion lane still owes land here, on the export's lane: the
            // UI thread that finished the recording never waits on a conversion.
            let take = take.settle();
            if cancel.is_cancelled() {
                fail_video_dump(dir, &reply, "request cancelled before export");
                return;
            }
            let inputs_logged = inputs.len();
            let inputs_cap = crate::VIDEO_KEY_LOG_CAP;
            for completion in ["index.json", "index.json.tmp"] {
                if let Err(error) = dir.remove_file_if_exists(std::ffi::OsStr::new(completion)) {
                    fail_video_dump(
                        dir,
                        &reply,
                        format!("could not clear stale {completion}: {error}"),
                    );
                    return;
                }
            }
            let mut fps = if inputs.is_empty() {
                Vec::new()
            } else {
                Vec::with_capacity(take.frames.len())
            };
            let mut frame_lines = String::with_capacity(take.frames.len().saturating_mul(112));
            let mut written = 0usize;
            let mut frame_seals = Vec::with_capacity(take.frames.len());
            let mut prev: Option<&aterm_gpu::video_tap::CapturedFrame> = None;
            for (i, f) in take.frames.iter().enumerate() {
                if cancel.is_cancelled() {
                    fail_video_dump(dir, &reply, "request cancelled during export");
                    return;
                }
                let fp = crate::video_key_analysis::frame_fingerprint(f);
                if !inputs.is_empty() {
                    fps.push((f.t_us, fp));
                }
                let delta = crate::video_key_analysis::changed_tiles(prev, f);
                prev = Some(f);
                let name = format!("frame_{:04}.png", i + 1);
                let png = match encode_rgba8_png(&f.rgba, f.w, f.h) {
                    Ok(png) => png,
                    Err(error) => {
                        fail_video_dump(
                            dir,
                            &reply,
                            format!("frame {} PNG encode failed: {error}", i + 1),
                        );
                        return;
                    }
                };
                let frame_seal = match dir.write_sealed_frame(std::ffi::OsStr::new(&name), &png) {
                    Ok(seal) => seal,
                    Err(error) => {
                        fail_video_dump(
                            dir,
                            &reply,
                            format!("frame {} write failed ({name}): {error}", i + 1),
                        );
                        return;
                    }
                };
                frame_seals.push(frame_seal);
                written += 1;
                if !frame_lines.is_empty() {
                    frame_lines.push_str(",\n");
                }
                write!(
                    &mut frame_lines,
                    "    {{\"n\":{},\"seq\":{},\"t_us\":{},\"fp\":{fp},\"delta\":{delta},\"file\":\"{name}\"}}",
                    i + 1,
                    f.seq,
                    f.t_us
                )
                .expect("writing to a String cannot fail");
            }
            if cancel.is_cancelled() {
                fail_video_dump(dir, &reply, "request cancelled before index publish");
                return;
            }
            let analysis = if inputs.is_empty() {
                format!(
                    "  \"analysis\": {{\"note\": \"{}\"}},\n",
                    video_empty_ledger_note(keys_enabled, unlogged_inputs, unlogged_other_window)
                )
            } else {
                let attempts: Vec<(u64, String)> = inputs
                    .iter()
                    .map(|(t, sample)| (*t, sample.json_field()))
                    .collect();
                crate::video_key_analysis::analysis_block(&attempts, &fps)
            };
            let mut input_lines = String::with_capacity(inputs.len().saturating_mul(48));
            for (t, sample) in &inputs {
                if !input_lines.is_empty() {
                    input_lines.push_str(",\n");
                }
                write!(
                    &mut input_lines,
                    "    {{\"t_us\":{t},{}}}",
                    sample.json_field()
                )
                .expect("writing to a String cannot fail");
            }
            let tick_list = pace_ticks
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let stop_us = crate::metrics::now_us();
            let dropped_total = take.dropped + take.evicted;
            let head_truncated = take.evicted > 0;
            let requested_ms = take.requested_ms;
            let covered_us = match (take.frames.front(), take.frames.back()) {
                (Some(first), Some(last)) => format!("[{}, {}]", first.t_us, last.t_us),
                _ => "null".to_string(),
            };
            let fps_cap = take
                .fps_cap
                .map_or_else(|| "null".to_string(), |n| n.to_string());
            let budget_mib = take.budget_bytes >> 20;
            let mode_str = mode.as_str();
            let stamp_semantics = mode.stamp_semantics();
            let index = format!(
                "{{\n  \"meta\": {{\n    \"w\": {}, \"h\": {}, \"device_px\": [{}, {}],\n    \
                 \"half_res\": {}, \"format\": \"{}\", \"mode\": \"{mode_str}\",\n    \
                 \"clock\": \"metrics now_us; same epoch as inputs[] — attempt->later-frame delta = frame.t_us - input.t_us\",\n    \
                 \"input_semantics\": \"pre-routing attempts (`ch` = character, `key` = named key with no character); not PTY-delivery or visible-glyph receipts\",\n    \
                 \"keys_requested\": {keys_enabled}, \"inputs_logged\": {}, \"unlogged_inputs\": {unlogged_inputs},\n    \
                 \"unlogged_other_window\": {unlogged_other_window}, \"unlogged_over_cap\": {unlogged_over_cap}, \"inputs_cap\": {inputs_cap},\n    \
                 \"unlogged_input_semantics\": \"input attempts during this take that are not in inputs[], from three causes. (1) The CONTROL-THREAD path: input written to the PTY off the App input seam this ledger hooks: a verb aimed at any session but the front tab's focused pane (`@<sid>` naming a background tab or an unfocused split pane; `@self` when the driving session is not that pane), any fenced `key`/`send` (`if=`, `if-gen=`, `if-fp=`, `if-human=`) wherever it is aimed, and aterm's own guarded presses (a `turn` submit guard, a fact-menu slash command, a worker's launch line). (2) `unlogged_other_window`: attempts that DID pass the seam but arrived on a window this take was not capturing — no frame here can answer them, so stamping them would fabricate a key->frame latency. (3) `unlogged_over_cap`: attempts on the recorded window after inputs[] reached inputs_cap rows — counted, not stamped. unlogged_inputs is the TOTAL and includes unlogged_other_window and unlogged_over_cap. Same unit as inputs_logged (one per would-be inputs[] row); cause (1) is process-wide, delta over this take. Non-zero means attempts happened that inputs[] does NOT contain.\",\n    \
                 \"stamp_semantics\": \"{stamp_semantics}\",\n    \
                 \"wall_start_us\": {started_us}, \"wall_stop_us\": {stop_us},\n    \
                 \"requested_ms\": {requested_ms}, \"covered_us\": {covered_us},\n    \
                 \"frames_written\": {written}, \"dropped_frames\": {dropped_total},\n    \
                 \"ring_skipped\": {}, \"evicted_frames\": {}, \"decimated_frames\": {},\n    \
                 \"head_truncated\": {head_truncated},\n    \
                 \"fps_cap\": {fps_cap}, \"budget_mib\": {budget_mib},\n    \
                 \"resized_early_stop\": {},\n    \
                 \"ticks_semantics\": \"frame-clock stamps of the recording loop's ticks (paced and headless present-real takes; empty for any other) whose redraw PROVED the screen unchanged, so it minted no frame: a sample of an unchanged screen, not a missed one. A tick that presented is in frames[]; a tick that did neither (a gated retry, a withheld or failed present) is in neither, so it reads as a hole\"\n  }},\n{analysis}  \"frames\": [\n{}\n  ],\n  \"ticks\": [{tick_list}],\n  \"inputs\": [\n{}\n  ]\n}}\n",
                take.w,
                take.h,
                take.device_px.0,
                take.device_px.1,
                take.half_res,
                take.format,
                inputs_logged,
                take.dropped,
                take.evicted,
                take.decimated,
                take.resized_early_stop,
                frame_lines,
                input_lines,
            );
            drop((
                take,
                inputs,
                pace_ticks,
                tick_list,
                fps,
                frame_lines,
                input_lines,
                analysis,
                covered_us,
                fps_cap,
            ));
            let index_file = match dir.write_batch_member_authorized(
                std::ffi::OsStr::new("index.json"),
                index.as_bytes(),
                || cancel.authorize_commit(),
            ) {
                Ok(file) => file,
                Err(error) => {
                    let message = if error.kind() == std::io::ErrorKind::Interrupted
                        && cancel.is_cancelled()
                    {
                        "request cancelled before index publish".to_string()
                    } else {
                        format!("index.json write failed: {error}")
                    };
                    fail_video_dump(dir, &reply, message);
                    return;
                }
            };
            drop(index);
            let mut publication = VideoPublication {
                frame_seals,
                index_file,
                published_marker: None,
                dir: *dir,
            };
            let published_dir = match publication.prepare() {
                Ok(path) => path,
                Err(error) => {
                    fail_video_publication(
                        publication,
                        &reply,
                        format!("recording publish failed: {error}"),
                    );
                    return;
                }
            };
            let value = format!(
                "OK frames={written} dropped={dropped_total} head_truncated={head_truncated}{} {}\n",
                video_keys_reply_tokens(keys_enabled, inputs_logged, unlogged_inputs),
                published_dir.join("index.json").display()
            );
            drop(published_dir);
            send_video_reply_with_retention(publication, handoff, &reply, value);
        }
    }
}

pub(crate) use aterm_types::control_verbs::AuxTarget;

const fn capture_ticks_cursor_fx(has_os_window: bool, recording_this_window: bool) -> bool {
    !has_os_window && !recording_this_window
}

#[cfg(test)]
fn image_req_for_test(
    target: crate::control_auth::ConfinedImage,
    clean: bool,
    want_bytes: bool,
    frame_metadata: Option<
        &std::sync::Arc<std::sync::OnceLock<crate::control::ImageFrameMetadata>>,
    >,
    reply: std::sync::mpsc::Sender<crate::control::ImageReply>,
) -> crate::control::ImageReq {
    crate::control::ImageReq {
        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
        target,
        clean,
        session: None,
        want_bytes,
        want_metadata: frame_metadata.is_some(),
        frame_metadata: frame_metadata.map_or_else(Default::default, std::sync::Arc::clone),
        cancel: crate::control::CaptureCancellation::new(),
        reply,
    }
}

#[cfg(test)]
fn captured_resident_body_for_test(app: &App, wid: WindowId) -> aterm_render::sprites::FreeSprite {
    let ws = &app.windows[&wid];
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy, _, _, _) =
        app.effects_origin_win(wid, usize::from(ws.rows), usize::from(ws.cols), ch);
    let published = ws
        .fx
        .companion()
        .hit_rect()
        .expect("capture publishes the resident body");
    let sprites = aterm_render::sprites::published(&ws.input_scratch.fx_layers);
    let mut matching = sprites.into_iter().filter(|sprite| {
        let x = sprite.x + i32::from(ox);
        let y = sprite.y + i32::from(oy);
        (x, x + i32::from(sprite.w), y, y + i32::from(sprite.h)) == published
    });
    let body = matching
        .next()
        .expect("publication identifies an emitted body");
    assert!(
        matching.next().is_none(),
        "one resident owns the published rectangle"
    );
    assert!(body.alpha > 0 && body.w > 0 && body.h > 0 && !body.art.is_empty());
    assert!(
        usize::from(body.art.w()) >= 5 * cw && usize::from(body.art.h()) >= ch,
        "the emitted sprite addresses full-body art, not a head or attachment"
    );
    body
}

fn observe_capture_pet_world(
    ws: &mut crate::WindowState,
    exact_focus: &crate::app_render::TerminalCaptureFocus,
    composed: bool,
) {
    use aterm_effects::pet_world::PetPane;
    let input = if composed {
        &ws.composed_focus_scratch
    } else {
        &ws.input_scratch
    };
    let exclusions = crate::app_render::pet_console_exclusions(
        input,
        !ws.preedit.is_empty(),
        ws.predictor.pending_bounds(),
        ws.search.is_some(),
        ws.pet_chrome.held(),
        ws.pet_chrome.held_overlays(),
        ws.pet_chrome.held_blanked(),
    )
    .painted_over(ws.pet_chrome.held_painted_over());
    let observed = ws.fx.companion_mut().observe_console_reserved(
        input,
        &exact_focus.pet_world,
        PetPane::full(input),
        exclusions.reserve(),
    );
    crate::metrics::note_pet_world_observation(observed);
}

struct CaptureCompanionSongSync<'a> {
    style: crate::cursor_glow::GlowStyle,
    cursor_trail_enabled: bool,
    cursor_companions_allowed: bool,
    pet_mode: bool,
    pet_flies: bool,
    now: Instant,
    detector: &'a mut aterm_effects::kitty_sing::KittySing,
    cat: &'a mut crate::kitty_cursor::CursorCat,
    glow: &'a mut crate::cursor_glow::CursorGlow,
    riff_bar: &'a mut Option<u64>,
}

fn sync_capture_companion_song(sync: CaptureCompanionSongSync<'_>) -> f32 {
    let CaptureCompanionSongSync {
        style,
        cursor_trail_enabled,
        cursor_companions_allowed,
        pet_mode,
        pet_flies,
        now,
        detector,
        cat,
        glow,
        riff_bar,
    } = sync;
    if !cursor_companions_allowed {
        *detector = aterm_effects::kitty_sing::KittySing::default();
        *riff_bar = None;
    }
    let drive = if cursor_companions_allowed
        && matches!(style, crate::cursor_glow::GlowStyle::RainbowKitty)
    {
        detector.drive(now)
    } else {
        0.0
    };
    if drive > 0.0 {
        glow.celebrate(now, drive);
        if let Some(bar) = detector.bar(now) {
            *riff_bar = Some(bar);
        }
    } else {
        detector.settle(now);
        *riff_bar = None;
    }
    cat.set_singing(
        now,
        crate::kitty_cursor::SingSync {
            drive,
            beat: detector.beat(now).unwrap_or(0.0),
        },
    );
    crate::app_render::retire_kitty_cursor_without_owner(
        cursor_trail_enabled && cursor_companions_allowed,
        pet_mode,
        pet_flies,
        style,
        drive,
        cat,
    );
    drive
}

fn capture_flying_companion_enabled(
    cursor_companion_presentable: bool,
    animate_cat: bool,
    cursor_trail_enabled: bool,
    style: crate::cursor_glow::GlowStyle,
    collection_hello: bool,
    sing: f32,
) -> bool {
    cursor_companion_presentable
        && (crate::app_render::cursor_cat_presentation_enabled(
            animate_cat,
            cursor_trail_enabled,
            style,
            collection_hello,
        ) || sing > 0.0)
}

const fn capture_flying_alpha(pet_mode: bool, kitty_enabled: bool, cat_alpha: u8) -> u8 {
    if kitty_enabled && !pet_mode {
        cat_alpha
    } else {
        0
    }
}

fn capture_pet_facts(
    focus: &crate::app_render::TerminalCaptureFocus,
) -> aterm_effects::host::TerminalFacts {
    aterm_effects::host::TerminalFacts {
        session: focus.session,
        caret: focus.cursor,
        cursor_visible: focus.cursor_visible,
        frame_unfinished: focus.frame_unfinished,
        sync_end_seq: focus.sync_end_seq,
        sync_end_caret: focus.sync_end_caret,
        display_offset: i32::try_from(focus.display_offset).unwrap_or(i32::MAX),
        live_viewport: focus.live_viewport,
        content_seq: focus.content_seq,
        wrap_serial: 0,
        scrolled: false,
        shell_executing: false,
        cmd_done: None,
        alt_screen: focus.alternate_screen,
    }
}

const fn capture_mode(windowless: bool) -> aterm_effects::host::CaptureMode {
    if windowless {
        aterm_effects::host::CaptureMode::StaticCapture
    } else {
        aterm_effects::host::CaptureMode::LiveCapture
    }
}

fn dims_axis(axis: aterm_render::AxisPlacement) -> (i64, u32, u32, u32, u32) {
    let px = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    (
        axis.origin,
        px(axis.band_before),
        px(axis.band_after),
        px(axis.crop_before),
        px(axis.crop_after),
    )
}

const fn dims_px(cells: u32, cell_px: u32, extra_px: u32) -> u32 {
    cells.saturating_mul(cell_px).saturating_add(extra_px)
}

fn native_settings_closed_controls_lines() -> Vec<String> {
    vec![
        "state open=false".to_string(),
        "prefs fields=0".to_string(),
        "native surface=native-tab route=- controls=0 semantics=0".to_string(),
    ]
}

fn native_settings_state_line(
    state: &crate::native_settings::SettingsViewState,
    field_count: usize,
    status: &str,
) -> String {
    let searching = !state.search.trim().is_empty();
    let editing = state.editing_field.as_deref().unwrap_or("");
    format!(
        "state open=true landing={} pane=native-tab category={} selected={} scroll={} \
         editing={editing:?} status={status:?} searching={searching} query={:?} \
         shown={field_count} total={field_count} surface=native-tab route={} page={:?}",
        state.route == crate::native_settings::SettingsRoute::Home && !searching,
        state.route.label(),
        state.page_scroll,
        state.page_scroll,
        state.search,
        state.route.path(),
        state.route.label(),
    )
}

fn native_settings_field_kind(
    field: &crate::prefs::EditField,
    themes: &crate::app_config::ThemeCatalog,
) -> String {
    match field.kind {
        crate::prefs::EditKind::Float => "float".to_string(),
        crate::prefs::EditKind::Integer => "integer".to_string(),
        crate::prefs::EditKind::Bool => "bool".to_string(),
        crate::prefs::EditKind::Text => "text".to_string(),
        crate::prefs::EditKind::Enum { options } => {
            format!("enum options=[{}]", options.join(","))
        }
        crate::prefs::EditKind::Theme => format!(
            "theme options=[{}]",
            crate::native_settings::settings_theme_options(themes)
                .iter()
                .map(|name| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        crate::prefs::EditKind::Color => "color".to_string(),
    }
}

fn native_settings_effective_value(value: &crate::native_ui::SemanticValue) -> String {
    match value {
        crate::native_ui::SemanticValue::None => String::new(),
        crate::native_ui::SemanticValue::Text(value) => value.clone(),
        crate::native_ui::SemanticValue::Bool(value) => value.to_string(),
        crate::native_ui::SemanticValue::Number { value, .. } => value.to_string(),
    }
}

pub(crate) fn native_settings_controls_lines(
    state: &crate::native_settings::SettingsViewState,
    compiled: &crate::native_ui::CompiledUi,
) -> Vec<String> {
    let rank = |node: &crate::native_ui::SemanticNode| {
        if node.key.as_str().starts_with("settings/control/") {
            0
        } else if matches!(node.value, crate::native_ui::SemanticValue::Text(_)) {
            1
        } else {
            2
        }
    };
    let mut fields: Vec<(&crate::native_ui::SemanticNode, &crate::prefs::EditField)> = Vec::new();
    for node in &compiled.semantics {
        let Some(key) = crate::native_settings::edited_setting_key(&node.key) else {
            continue;
        };
        let Some(field) = state.legacy.fields.iter().find(|field| field.key == key) else {
            continue;
        };
        match fields.iter_mut().find(|(_, bound)| bound.key == key) {
            Some(bound) if rank(node) < rank(bound.0) => bound.0 = node,
            Some(_) => {}
            None => fields.push((node, field)),
        }
    }
    let status = state.feedback.as_deref().unwrap_or("");
    let mut out = Vec::with_capacity(3 + fields.len() + compiled.semantics.len());
    out.push(native_settings_state_line(state, fields.len(), status));
    out.push(format!("prefs fields={}", fields.len()));
    for (node, field) in fields {
        let value = field.seed.as_deref().unwrap_or("");
        let effective = if node.key.as_str().starts_with("settings/control/") {
            native_settings_effective_value(&node.value)
        } else {
            crate::native_settings::setting_effective_words(state, field.key)
                .unwrap_or_else(|| native_settings_effective_value(&node.value))
        };
        let kind = native_settings_field_kind(field, &state.config_assets().themes);
        let action = node
            .action
            .as_ref()
            .map_or("-", crate::native_ui::ActionId::as_str);
        out.push(format!(
            "field key={} label={:?} value={:?} effective={effective:?} kind={kind} \
             visible=true native-key={:?} action={action}",
            field.key,
            field.label,
            value,
            node.key.as_str(),
        ));
    }
    let controls = compiled
        .semantics
        .iter()
        .filter(|node| node.action.is_some())
        .count();
    out.push(format!(
        "native surface=native-tab route={} controls={controls} semantics={}",
        state.route.path(),
        compiled.semantics.len(),
    ));
    out.extend(compiled.controls_lines());
    out
}

fn native_settings_unavailable_controls_lines(
    state: &crate::native_settings::SettingsViewState,
    error: &str,
) -> Vec<String> {
    vec![
        native_settings_state_line(state, 0, error),
        "prefs fields=0".to_string(),
        format!(
            "native surface=native-tab route={} controls=0 semantics=0 error={error:?}",
            state.route.path()
        ),
    ]
}

fn native_settings_route_not_open_controls_lines(
    alias: &str,
    expected: crate::native_settings::SettingsRoute,
    current: Option<&crate::native_settings::SettingsViewState>,
) -> Vec<String> {
    let current_route = current.map_or("-", |state| state.route.path());
    let error = if let Some(state) = current {
        format!(
            "controls {alias} needs the Settings {} route, but the front Settings tab shows {}; use `open {alias}` first",
            expected.path(),
            state.route.path(),
        )
    } else {
        format!(
            "controls {alias} needs the Settings {} route, which the front window is not showing; use `open {alias}` first",
            expected.path(),
        )
    };
    let state = current.map_or_else(
        || "state open=false".to_string(),
        |state| native_settings_state_line(state, 0, &error),
    );
    vec![
        state,
        "prefs fields=0".to_string(),
        format!(
            "native surface=native-tab alias={alias} route={current_route} expected-route={} controls=0 semantics=0 error={error:?}",
            expected.path(),
        ),
    ]
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static OS_WINDOW_PHOTOGRAPHS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl App {
    /// Identity of the global-front target selected by the trail readers.
    pub(crate) fn trail_target_fields(&self) -> Result<String, String> {
        let window = self.frontmost_window.ok_or("no focused window")?;
        let state = self.windows.get(&window).ok_or("no focused window")?;
        let view = state.front_content.map(|front| front.view().get());
        let session = state
            .front_content
            .and_then(|front| front.terminal_session());
        Ok(format!(
            "selection=front window={} view={} session={}",
            window.0,
            view.map_or_else(|| "-".to_string(), |id| id.to_string()),
            session.map_or_else(|| "-".to_string(), |id| id.to_string()),
        ))
    }

    /// The `trail` control-socket verb ([`crate::Wake::TrailAdmissions`]).
    pub(crate) fn trail_admissions(&self, count: Option<usize>) -> Result<Vec<String>, String> {
        let Some(ws) = self.frontmost_window.and_then(|wid| self.windows.get(&wid)) else {
            return Err("no focused window".to_string());
        };
        let now = Instant::now();
        let all: Vec<String> = ws
            .fx
            .cursor()
            .glow
            .admission_log()
            .map(|record| record.line(now))
            .collect();
        let keep = count.unwrap_or(all.len()).min(all.len());
        let mut lines = Vec::with_capacity(keep + 1);
        lines.push(format!("trail-target {}", self.trail_target_fields()?));
        lines.extend_from_slice(&all[all.len() - keep..]);
        Ok(lines)
    }

    /// Sample the selected terminal grid and all main-thread-owned presentation geometry as one
    /// event-turn record, with the grid's content clock in the frame the selected window's last own
    /// present drew (`presented_seq`) while that grid ([`crate::app_render::GridKey`]: its terminal,
    /// screen and grid instance) is still the live one.
    pub(crate) fn try_dims_snapshot(
        &self,
        session: u64,
        term: &Arc<Mutex<Terminal>>,
    ) -> Result<DimsSnapshot, &'static str> {
        let read = |terminal: &Terminal| {
            (
                u32::from(terminal.rows()),
                u32::from(terminal.cols()),
                crate::app_render::GridKey::live(terminal),
            )
        };
        let (rows, cols, grid) = match term.try_lock() {
            Ok(terminal) => read(&terminal),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => read(&poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err("terminal busy; retry dims");
            }
        };
        let mut snapshot = self.dims_snapshot(session, rows, cols);
        snapshot.presented_seq = snapshot
            .window
            .and_then(|wid| self.windows.get(&crate::WindowId(wid)))
            .and_then(|ws| {
                ws.presented_grids
                    .iter()
                    .find(|g| g.session == session && g.grid == grid)
                    .map(|g| g.content_seq)
            });
        Ok(snapshot)
    }

    fn detached_scale(&self) -> f64 {
        if let Some(pinned) = crate::app_config::resolve_force_scale() {
            return pinned;
        }
        let tuned = if self.backend.is_pending() {
            None
        } else {
            self.windows.values().find(|ws| {
                (ws.metrics.font_px - self.font_px).abs() < 0.5
                    && ws.metrics.pad == self.backend.pad()
                    && ws.metrics.pad_top == self.backend.pad_top()
                    && ws.metrics.head == self.backend.head()
            })
        };
        tuned
            .or_else(|| {
                self.frontmost_window
                    .and_then(|wid| self.windows.get(&wid))
                    .or_else(|| self.windows.values().next())
            })
            .map_or(1.0, |ws| ws.scale)
    }

    /// Project one selected session through the live geometry of the front window when it contains the session.
    pub(crate) fn dims_snapshot(&self, session: u64, rows: u32, cols: u32) -> DimsSnapshot {
        let mut windows: Vec<crate::WindowId> = self
            .windows
            .keys()
            .copied()
            .filter(|wid| self.window_contains_session(*wid, session))
            .collect();
        windows.sort_unstable_by_key(|wid| wid.0);
        let visible_viewers = self.windows_displaying(session).count();
        let selected = self
            .frontmost_window
            .filter(|wid| windows.contains(wid))
            .or_else(|| windows.first().copied());
        self.dims_snapshot_in(
            Some(session),
            selected,
            (windows.len(), visible_viewers),
            rows,
            cols,
        )
    }

    /// `dims` with no terminal in front (an app tab such as Settings is): the front window's own
    /// grid through its live geometry, `session=none`, so a script reads back the size a resize
    /// gave the window whatever tab is in front. `None` with no window.
    pub(crate) fn window_dims_snapshot(&self) -> Option<DimsSnapshot> {
        let wid = self
            .frontmost_window
            .filter(|wid| self.windows.contains_key(wid))?;
        let ws = &self.windows[&wid];
        Some(self.dims_snapshot_in(
            None,
            Some(wid),
            (0, 0),
            u32::from(ws.rows),
            u32::from(ws.cols),
        ))
    }

    /// [`Self::dims_snapshot`] of `session` (none for a window's own grid) at `rows`x`cols`,
    /// through window `selected`'s geometry (detached when none), with `viewers` the windows that
    /// hold the session and that show it.
    fn dims_snapshot_in(
        &self,
        session: Option<u64>,
        selected: Option<crate::WindowId>,
        viewers: (usize, usize),
        rows: u32,
        cols: u32,
    ) -> DimsSnapshot {
        let (viewers, visible_viewers) = viewers;
        let scale = selected.map_or_else(|| self.detached_scale(), |wid| self.windows[&wid].scale);

        let (
            cell_w,
            cell_h,
            font_px,
            window,
            window_rows,
            window_cols,
            composed_rows,
            pad,
            pad_top,
            head,
            tab_rows,
            frame_w,
            frame_h,
            surface_w,
            surface_h,
            geometry,
        ) = if let Some(wid) = selected {
            let ws = &self.windows[&wid];
            let (cell_w, cell_h) = if self.backend.is_pending() {
                crate::seed_cell_px(ws.metrics.font_px)
            } else {
                let (cell_w, cell_h, _) = self.backend.cell_geometry(ws.metrics.font_px);
                (cell_w, cell_h)
            };
            let cell_w = u32::try_from(cell_w).unwrap_or(u32::MAX);
            let cell_h = u32::try_from(cell_h).unwrap_or(u32::MAX);
            let pad = u32::try_from(ws.metrics.pad).unwrap_or(u32::MAX);
            let pad_top = u32::try_from(ws.metrics.pad_top).unwrap_or(u32::MAX);
            let head = u32::try_from(ws.metrics.head).unwrap_or(u32::MAX);
            let tab_rows = u32::from(self.chrome_rows(wid));
            let window_rows = u32::from(ws.rows);
            let window_cols = u32::from(ws.cols);
            let composed_rows = window_rows.saturating_add(tab_rows);
            let frame_w = dims_px(window_cols, cell_w, pad.saturating_mul(2));
            let frame_h = dims_px(
                composed_rows,
                cell_h,
                pad_top.saturating_add(pad).saturating_add(head),
            );
            let (surface_w, surface_h, geometry) = match ws.win_px {
                Some(size) => (size.width.max(1), size.height.max(1), "window"),
                None if self.headless => (frame_w, frame_h, "headless"),
                None => (frame_w, frame_h, "pre_attach"),
            };
            (
                cell_w,
                cell_h,
                ws.metrics.font_px,
                Some(wid.0),
                window_rows,
                window_cols,
                composed_rows,
                pad,
                pad_top,
                head,
                tab_rows,
                frame_w,
                frame_h,
                surface_w,
                surface_h,
                geometry,
            )
        } else {
            let (cell_w, cell_h) = if self.backend.is_pending() {
                crate::seed_cell_px(self.font_px)
            } else {
                self.cell_size()
            };
            let cell_w = u32::try_from(cell_w).unwrap_or(u32::MAX);
            let cell_h = u32::try_from(cell_h).unwrap_or(u32::MAX);
            let (pad, pad_top, head) = if self.backend.is_pending() {
                (crate::pad_for_scale(1.0), crate::pad_top_for_scale(1.0), 0)
            } else {
                (
                    self.backend.pad(),
                    self.backend.pad_top(),
                    self.backend.head(),
                )
            };
            let pad = u32::try_from(pad).unwrap_or(u32::MAX);
            let pad_top = u32::try_from(pad_top).unwrap_or(u32::MAX);
            let head = u32::try_from(head).unwrap_or(u32::MAX);
            let frame_w = dims_px(cols, cell_w, pad.saturating_mul(2));
            let frame_h = dims_px(
                rows,
                cell_h,
                pad_top.saturating_add(pad).saturating_add(head),
            );
            (
                cell_w,
                cell_h,
                self.font_px,
                None,
                rows,
                cols,
                rows,
                pad,
                pad_top,
                head,
                0,
                frame_w,
                frame_h,
                frame_w,
                frame_h,
                "detached",
            )
        };

        let (deck_y, deck_h) =
            selected
                .and_then(|wid| self.deck_rows_px(wid))
                .map_or((0, 0), |rows| {
                    (
                        u32::try_from(rows.start).unwrap_or(u32::MAX),
                        u32::try_from(rows.len()).unwrap_or(u32::MAX),
                    )
                });
        let place = aterm_render::frame_placement(
            surface_w as usize,
            surface_h as usize,
            frame_w as usize,
            frame_h as usize,
        );
        let (offset_x, band_left, band_right, crop_left, crop_right) = dims_axis(place.x);
        let (offset_y, band_top, band_bottom, crop_top, crop_bottom) = dims_axis(place.y);
        let (
            present_retry_state,
            present_retry_count,
            present_retry_remaining,
            present_retry_in_ms,
        ) = selected.map_or(("detached", 0, 0, None), |wid| {
            let retry = &self.windows[&wid].present_retry;
            let state = if retry.parked {
                "parked"
            } else if retry.deadline.is_some() {
                "backoff"
            } else if retry.recovery_redraw_outstanding {
                "redraw"
            } else {
                "ready"
            };
            let now = Instant::now();
            let in_ms = retry.deadline.map(|deadline| {
                let duration = deadline.saturating_duration_since(now);
                u64::try_from(duration.as_millis())
                    .unwrap_or(u64::MAX)
                    .max(u64::from(!duration.is_zero()))
            });
            (
                state,
                u32::from(retry.autonomous_retries),
                u32::from(crate::PRESENT_RETRY_CAP.saturating_sub(retry.autonomous_retries)),
                in_ms,
            )
        });

        DimsSnapshot {
            session,
            rows,
            cols,
            pixel_w: dims_px(cols, cell_w, 0),
            pixel_h: dims_px(rows, cell_h, 0),
            cell_w,
            cell_h,
            font_px,
            scale,
            window,
            window_rows,
            window_cols,
            composed_rows,
            frame_w,
            frame_h,
            surface_w,
            surface_h,
            offset_x,
            offset_y,
            band_left,
            band_right,
            band_top,
            band_bottom,
            crop_left,
            crop_right,
            crop_top,
            crop_bottom,
            pad,
            pad_top,
            pad_bottom: pad,
            head,
            tab_rows,
            deck_y,
            deck_h,
            viewers: u32::try_from(viewers).unwrap_or(u32::MAX),
            visible_viewers: u32::try_from(visible_viewers).unwrap_or(u32::MAX),
            geometry,
            present_retry_state,
            present_retry_count,
            present_retry_remaining,
            present_retry_in_ms,
            layer_presentation: selected
                .and_then(|wid| self.windows[&wid].os_window.as_ref())
                .and_then(|w| self.apprt.window_surface_presentation(w)),
            presented_seq: None,
        }
    }

    /// The cursor glow's box for a capture of window `wid`, as the present's
    /// `cursor_frame` builds it: the window pixels the sky draws its stars in.
    fn capture_glow_geom(
        &self,
        wid: WindowId,
        (cw, ch): (usize, usize),
        (rows, cols): (usize, usize),
    ) -> crate::cursor_glow::Geom {
        let (origin_x, origin_y, win_w, win_h, head) = self.effects_origin_win(wid, rows, cols, ch);
        crate::cursor_glow::Geom {
            cw,
            ch,
            rows,
            cols,
            origin_x,
            origin_y,
            win_w,
            win_h,
            head,
        }
    }

    fn capture_composed_words(
        &mut self,
        wid: WindowId,
        now: Instant,
        exact_focus: crate::app_render::TerminalCaptureFocus,
    ) {
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return;
        };
        let Some(focus_index) = plan
            .leaves
            .iter()
            .position(|leaf| leaf.focused && leaf.view == exact_focus.view)
        else {
            return;
        };
        let focus = exact_focus.session;
        let panes: Vec<(
            crate::pane::PaneRect,
            std::sync::Arc<std::sync::Mutex<Terminal>>,
        )> = plan
            .leaves
            .iter()
            .map(|leaf| {
                let crate::tab_model::View::Terminal(terminal) =
                    self.view_store.get(leaf.view).copied()?
                else {
                    return None;
                };
                let rect = crate::pane::PaneRect {
                    session: terminal.session,
                    row_off: leaf.rect.origin.y.round().max(0.0) as u16,
                    col_off: leaf.rect.origin.x.round().max(0.0) as u16,
                    rows: (leaf.rect.size.height.round() as u16).max(1),
                    cols: (leaf.rect.size.width.round() as u16).max(1),
                };
                self.pool
                    .get(terminal.session)
                    .map(|session| (rect, session.term.clone()))
            })
            .collect::<Option<_>>()
            .unwrap_or_default();
        if panes.len() != plan.leaves.len() {
            return;
        }
        let raw_focused = self.windows.get(&wid).is_some_and(|ws| ws.focused);
        let capture_focused = self.cursor_fx_focus(wid, raw_focused, now);
        let word_focus = self.motion_focus(wid, raw_focused);
        let motion = self.motion_policy(capture_focused);
        let pet_motion = self.accessibility_motion_policy(capture_focused);
        let shed_envelope = self.effective_shed_envelope(self.config.motion_mode(), now);
        let animate_sparkles = motion.animate(crate::motion::MotionEffect::WordSparkles);
        let (cell_w, cell_h) = self.backend.cell_size();
        let trail_presentation = self.trail_presentation();
        let glow_cfg = self.glow_config_for(trail_presentation);
        let cursor_companions_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorCat);
        let load_shed = self.load_shed_active();
        let pet_mode = trail_presentation.pet_species.is_some();
        let pet_species = trail_presentation
            .pet_species
            .unwrap_or(aterm_effects::kitty_pet::PetSpecies::Cat);
        let companion_look = self.companion_verdict(wid, focus, now);
        let windowless = self
            .windows
            .get(&wid)
            .is_none_or(|window| window.os_window.is_none());
        if exact_focus.session != focus || !exact_focus.damage_consumed {
            return;
        }
        let focus_live_viewport = exact_focus.live_viewport;
        let focus_rewound = self.pool.is_rewound(focus);
        let focus_coordinate_space = Some((exact_focus.terminal_id, exact_focus.alternate_screen));
        let Some((rows, cols)) = self
            .windows
            .get(&wid)
            .map(|ws| (usize::from(ws.rows), usize::from(ws.cols)))
        else {
            return;
        };
        let glow_geom = self.capture_glow_geom(wid, (cell_w, cell_h), (rows, cols));
        let (origin_x, origin_y) = (glow_geom.origin_x, glow_geom.origin_y);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some((terminal_id, alternate_screen)) = focus_coordinate_space {
            sync_cursor_effect_coordinate_space(ws, terminal_id, alternate_screen);
        }
        if pet_mode && cursor_companions_allowed {
            observe_capture_pet_world(ws, &exact_focus, true);
        }
        let (win_rows, win_cols) = (ws.rows, ws.cols);
        ws.fx.companions_mut().cursor_cat.set_look(companion_look);
        let cursor_companion_presentable =
            cursor_companions_allowed && capture_focused && !load_shed && focus_live_viewport;
        ws.fx
            .companions_mut()
            .cursor_cat
            .set_collection_presentable(now, cursor_companion_presentable);
        if !cursor_companions_allowed {
            ws.fx.companions_mut().music_notes.clear();
        }
        let sing_drive = {
            let (c, cursor) = ws.fx.companions_with_cursor();
            let (detector, cat, glow, riff_bar) = (
                &mut c.kitty_sing,
                &mut c.cursor_cat,
                &mut cursor.glow,
                &mut c.sing_riff_bar,
            );
            sync_capture_companion_song(CaptureCompanionSongSync {
                style: glow_cfg.style,
                cursor_trail_enabled: glow_cfg.enabled,
                cursor_companions_allowed,
                pet_mode,
                pet_flies: trail_presentation.pet_flies,
                now,
                detector,
                cat,
                glow,
                riff_bar,
            })
        };
        let animate_cat = motion.animate(crate::motion::MotionEffect::CursorGlow);
        let cat_frame = if animate_cat {
            ws.fx.companions_mut().cursor_cat.frame(now)
        } else {
            ws.fx.companions_mut().cursor_cat.static_frame(now)
        };
        let kitty_enabled = capture_flying_companion_enabled(
            cursor_companion_presentable,
            animate_cat,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
            cat_frame.collection_hello,
            cat_frame.sing,
        );
        let kitty_alpha = capture_flying_alpha(pet_mode, kitty_enabled, cat_frame.alpha);
        let (pane_rows, pane_cols, pane_origin) =
            panes.get(focus_index).map_or((0, 0, (0, 0)), |(r, _)| {
                (
                    r.rows,
                    r.cols,
                    (
                        i32::from(r.col_off) * cell_w as i32,
                        i32::from(r.row_off) * cell_h as i32,
                    ),
                )
            });
        ws.fx.companion_mut().set_species(pet_species);
        ws.fx
            .companion_mut()
            .offer_glass(
                panes
                    .iter()
                    .map(|(r, _)| aterm_effects::companion::PetGlass {
                        session: r.session,
                        origin: (
                            i32::from(r.col_off) * cell_w as i32,
                            i32::from(r.row_off) * cell_h as i32,
                        ),
                        cols: r.cols,
                        rows: r.rows,
                    }),
            );
        let pet_tick = ws.fx.pet_prepare(
            aterm_effects::companion::PetFacts::new(
                &capture_pet_facts(&exact_focus),
                &aterm_effects::host::HostFrameInput::new(
                    now,
                    crate::app_render::pet_frame_geom(
                        (cell_w, cell_h),
                        (usize::from(pane_rows), usize::from(pane_cols)),
                        (
                            i32::from(origin_x) + pane_origin.0,
                            i32::from(origin_y) + pane_origin.1,
                        ),
                    ),
                )
                .with_visibility(crate::app_render::pet_visibility(capture_focused))
                .with_reduced_motion(!pet_motion.animate(crate::motion::MotionEffect::CursorGlow))
                .with_serious(!cursor_companions_allowed)
                .with_shed_envelope(shed_envelope)
                .with_shed_active(load_shed)
                .with_capture(capture_mode(windowless)),
                aterm_effects::companion::PetOwnership { on: pet_mode },
            )
            .with_sing(aterm_effects::host::SingFacts {
                drive: sing_drive,
                flying_alpha: if kitty_enabled { cat_frame.alpha } else { 0 },
            })
            .with_focused(capture_focused)
            .with_obscured(ws.overlay_open() || ws.tab_menu.is_some() || focus_rewound)
            .with_pane(Some((focus, pane_origin))),
        );
        let escort_caret = pet_tick.escort_caret();
        let pet = ws.fx.companion_mut().tick(pet_tick);
        // The split present's offer (`redraw_compose_after_focused_extract`):
        // the body the sky draws no star on, in the focused pane's cells.
        let focus_off = panes
            .get(focus_index)
            .map_or((0, 0), |(r, _)| (r.row_off, r.col_off));
        ws.fx.offer_pet_v2(&pet.pet, glow_geom, focus_off, now);
        let ctx = crate::app_render::ComposeDecoCtx {
            panes: &panes,
            focus,
            focus_index,
            win_rows,
            win_cols,
            cell_w: cell_w as u32,
            cell_h: cell_h as u32,
            escort_caret,
            win_focused: raw_focused,
            word_focus,
            animate_sparkles,
            animate_streak: motion.animate(crate::motion::MotionEffect::OutputStreak),
            animate_cat,
            kitty_alpha,
            cat_frame,
            pet,
            present: false,
            accent: glow_cfg.accent,
            cursor_color: ws.input_scratch.cursor_color,
            now,
        };
        self.compose_words(wid, &ctx);
        self.compose_output_streak(wid, &ctx);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx.publish_words(&mut ws.input_scratch);
        }
    }

    /// Build this window's sparkle-word decorations into `input_scratch`.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn capture_words(&mut self, wid: crate::WindowId, now: Instant) {
        let Some(focus) = self
            .prepare_terminal_capture_grid_with_cursor_fx(
                wid,
                crate::app_render::ComposedCursorFxClock::Advance(now),
            )
            .and_then(|grid| grid.focus)
        else {
            return;
        };
        self.capture_words_sampled(wid, now, focus);
    }

    fn capture_words_sampled(
        &mut self,
        wid: crate::WindowId,
        now: Instant,
        exact_focus: crate::app_render::TerminalCaptureFocus,
    ) {
        let recording_this_window = self.video_rec.as_ref().is_some_and(|r| r.window == wid);
        let has_os_window = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.os_window.is_some());
        let capture_owns_cursor_fx = capture_ticks_cursor_fx(has_os_window, recording_this_window);
        if !capture_owns_cursor_fx {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.fx.withdraw_rain(&mut ws.input_scratch);
                ws.fx.publish_words(&mut ws.input_scratch);
            }
            return;
        }
        if self.sparkle_dirty {
            self.recompute_sparkle();
        }
        if self.rain_dirty {
            self.recompute_matrix_rain();
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.fx.withdraw_rain(&mut ws.input_scratch);
            ws.input_scratch.word_decorations.clear();
            ws.input_scratch.ink.clear();
            ws.fx.withdraw_sprites(&mut ws.input_scratch);
            ws.input_scratch.nova_add.clear();
        }
        self.install_window_config_assets(wid);
        if self
            .active_tree(wid)
            .is_some_and(|t| t.len() > 1 && !t.is_zoomed())
        {
            self.capture_composed_words(wid, now, exact_focus);
            return;
        }
        let sparkle = self
            .sparkle
            .as_ref()
            .map(|r| (r.cfg.clone(), r.lexicon.clone()));
        let capture_focused = self.cursor_fx_focus(
            wid,
            self.windows.get(&wid).is_some_and(|ws| ws.focused),
            now,
        );
        let motion = self.motion_policy(capture_focused);
        let pet_motion = self.accessibility_motion_policy(capture_focused);
        let shed_envelope = self.effective_shed_envelope(self.config.motion_mode(), now);
        let sparkle = sparkle.map(|(mut cfg, lexicon)| {
            if !motion.animate(crate::motion::MotionEffect::WordSparkles) {
                cfg.reduced_motion = true;
            }
            (cfg, lexicon)
        });
        let (cell_w, cell_h) = self.backend.cell_size();
        let kitty_log_on = self.kitty_log_enabled();
        let capture_front_session = self.focused_session_id(wid).unwrap_or(0);
        let companion_look = self.companion_verdict(wid, capture_front_session, now);
        let trail_presentation = self.trail_presentation();
        let glow_cfg = self.glow_config_for(trail_presentation);
        let cursor_companions_allowed = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::CursorCat);
        let pet_mode = trail_presentation.pet_species.is_some();
        let pet_species = trail_presentation
            .pet_species
            .unwrap_or(aterm_effects::kitty_pet::PetSpecies::Cat);
        let suppress_alt = sparkle.is_some() && self.config.sparkle_suppress_alt_screen();
        let load_shed = self.load_shed_active();
        let streak_enabled = self.output_streak_shows();
        let streak_intensity = self.config.output_streak_intensity_or_default();
        let streak_tail = self.config.output_streak_tail_or_default();
        let streak_max = self.config.output_streak_max_streaks_or_default();
        let streak_idle = self.config.output_streak_idle_secs_or_default();
        let streak_animates = motion.animate(crate::motion::MotionEffect::OutputStreak);
        let windowless = self
            .windows
            .get(&wid)
            .is_none_or(|window| window.os_window.is_none());
        if exact_focus.session != capture_front_session || !exact_focus.damage_consumed {
            return;
        }
        let output_echo = self
            .pool
            .get(exact_focus.session)
            .map(|session| session.ctx.output_echo.sample(&session.ctx.sink, now))
            .unwrap_or_default();
        let Some((rows, cols)) = self
            .windows
            .get(&wid)
            .map(|ws| (usize::from(ws.rows), usize::from(ws.cols)))
        else {
            return;
        };
        let glow_geom = self.capture_glow_geom(wid, (cell_w, cell_h), (rows, cols));
        let (origin_x, origin_y) = (glow_geom.origin_x, glow_geom.origin_y);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let (rows, cols) = (ws.rows as usize, ws.cols as usize);
        let live_viewport = exact_focus.live_viewport;
        let rewound = self.pool.is_rewound(exact_focus.session);
        let grid_cursor = (
            u16::try_from(ws.input_scratch.cursor_row).unwrap_or(u16::MAX),
            u16::try_from(ws.input_scratch.cursor_col).unwrap_or(u16::MAX),
        );
        sync_cursor_effect_coordinate_space(
            ws,
            exact_focus.terminal_id,
            exact_focus.alternate_screen,
        );
        if pet_mode && cursor_companions_allowed {
            observe_capture_pet_world(ws, &exact_focus, false);
        }
        ws.fx.companions_mut().cursor_cat.set_look(companion_look);
        let cursor_companion_presentable =
            cursor_companions_allowed && capture_focused && !load_shed && live_viewport;
        ws.fx
            .companions_mut()
            .cursor_cat
            .set_collection_presentable(now, cursor_companion_presentable);
        if !cursor_companions_allowed {
            ws.fx.companions_mut().music_notes.clear();
        }
        ws.fx
            .decos_mut()
            .set_scan_session(Some(exact_focus.session));
        let sing_drive = {
            let (c, cursor) = ws.fx.companions_with_cursor();
            let (detector, cat, glow, riff_bar) = (
                &mut c.kitty_sing,
                &mut c.cursor_cat,
                &mut cursor.glow,
                &mut c.sing_riff_bar,
            );
            sync_capture_companion_song(CaptureCompanionSongSync {
                style: glow_cfg.style,
                cursor_trail_enabled: glow_cfg.enabled,
                cursor_companions_allowed,
                pet_mode,
                pet_flies: trail_presentation.pet_flies,
                now,
                detector,
                cat,
                glow,
                riff_bar,
            })
        };
        let animate_cat = motion.animate(crate::motion::MotionEffect::CursorGlow);
        let cat_frame = if animate_cat {
            ws.fx.companions_mut().cursor_cat.frame(now)
        } else {
            ws.fx.companions_mut().cursor_cat.static_frame(now)
        };
        let kitty_enabled = capture_flying_companion_enabled(
            cursor_companion_presentable,
            animate_cat,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
            cat_frame.collection_hello,
            cat_frame.sing,
        );
        let kitty_alpha = capture_flying_alpha(pet_mode, kitty_enabled, cat_frame.alpha);
        let effect_geom = crate::word_decorations::EffectGeom {
            cell_w: cell_w as u16,
            cell_h: cell_h as u16,
            rows: rows as u16,
            cols: cols as u16,
        };
        ws.fx.companion_mut().set_species(pet_species);
        let pet_tick = ws.fx.pet_prepare(
            aterm_effects::companion::PetFacts::new(
                &capture_pet_facts(&exact_focus),
                &aterm_effects::host::HostFrameInput::new(
                    now,
                    crate::app_render::pet_frame_geom(
                        (cell_w, cell_h),
                        (rows, cols),
                        (i32::from(origin_x), i32::from(origin_y)),
                    ),
                )
                .with_visibility(crate::app_render::pet_visibility(capture_focused))
                .with_reduced_motion(!pet_motion.animate(crate::motion::MotionEffect::CursorGlow))
                .with_serious(!cursor_companions_allowed)
                .with_shed_envelope(shed_envelope)
                .with_shed_active(load_shed)
                .with_capture(capture_mode(windowless)),
                aterm_effects::companion::PetOwnership { on: pet_mode },
            )
            .with_sing(aterm_effects::host::SingFacts {
                drive: sing_drive,
                flying_alpha: if kitty_enabled { cat_frame.alpha } else { 0 },
            })
            .with_focused(capture_focused)
            .with_obscured(ws.overlay_open() || ws.tab_menu.is_some() || rewound),
        );
        let cur = pet_tick.escort_caret();
        let pet = ws.fx.companion_mut().tick(pet_tick);
        // The present's offer (`redraw_window_with_layout`): without it a
        // capture's sky never learns where the pet flies and draws its stars
        // on the fur.
        ws.fx.offer_pet_v2(&pet.pet, glow_geom, (0, 0), now);
        let words_suspended = (exact_focus.alternate_screen && suppress_alt) || rewound;
        if load_shed || words_suspended || sparkle.is_none() {
            // The word field freezes under load, alternate-screen suppression or a Rewind view; an explicit feature disable resets it.
            if load_shed || words_suspended {
                ws.fx.decos_mut().freeze(now);
            } else {
                ws.fx.decos_mut().hard_reset_words();
                ws.pending_deco_birth = None;
            }
            ws.fx.open_words();

            let default_bg = ws.input_scratch.default_bg;
            let cursor_color = ws.input_scratch.cursor_color;
            let _ = crate::app_render::emit_single_cursor_companion(
                ws,
                effect_geom,
                cur,
                &pet,
                false,
                cat_frame,
                animate_cat,
                kitty_alpha,
                now,
                !animate_cat,
                default_bg,
                cursor_color,
                glow_cfg.accent,
            );
            ws.fx.publish_words(&mut ws.input_scratch);
            return;
        }
        let (cfg, lexicon) = sparkle
            .as_ref()
            .expect("enabled, unsuspended Sparkle Words was checked above");
        let epoch = ws.input_scratch.snapshot_seq;
        let rescan =
            ws.fx
                .decos()
                .needs_rescan(epoch)
                .then(|| aterm_effects::pipeline::WordRescan {
                    birth: capture_rescan_birth(now, ws.pending_deco_birth.take(), windowless),
                    cursor: live_viewport.then_some(grid_cursor),
                });
        // `trail status`'s `head_body=` is this frame's: a head gone since the last capture left
        // its box in the status of every frame after it (the suspended branch's `open_words` does
        // this), and the trail gallery read that box over the letters it no longer lay on.
        ws.fx.cursor_mut().glow.note_head_body(None);
        let routed = crate::app_render::route_single_cursor_companion(
            ws,
            effect_geom,
            cur,
            &pet,
            cat_frame,
            animate_cat,
            kitty_alpha,
            now,
        );
        let companion_at = routed.on_glass(cur, pet.body_px);
        let words = ws.fx.word_grid(
            &aterm_effects::pipeline::WordGrid::new(
                now,
                lexicon,
                cfg,
                effect_geom,
                &ws.input_scratch.cells,
                &ws.input_scratch.line_sizes,
                crate::word_decorations::SelView {
                    sel: &ws.input_scratch.selection,
                    display_offset: ws.input_scratch.display_offset,
                },
            )
            .with_base_y(ws.input_scratch.base_y)
            .with_default_bg(ws.input_scratch.default_bg)
            .with_epoch(epoch)
            .with_rescan(rescan)
            .with_companion_at(companion_at)
            .with_focused(ws.focused),
        );
        let suppressed = ws.input_scratch.engine_alt || ws.input_scratch.display_offset != 0;
        let streak_inert = crate::app_render::output_streak_inert(
            streak_enabled,
            streak_animates,
            streak_intensity,
            suppressed,
        );
        ws.fx.streak_grid(
            (!streak_inert)
                .then(|| aterm_effects::pipeline::StreakGrid {
                    now,
                    cfg: crate::output_streak::StreakConfig {
                        enabled: true,
                        intensity: streak_intensity,
                        tail: streak_tail,
                        max_streaks: streak_max,
                        idle_secs: streak_idle,
                        dark_theme: aterm_render::theme_is_dark(ws.input_scratch.default_bg),
                        theme_bg: ws.input_scratch.default_bg,
                        theme_fg: ws.input_scratch.default_fg,
                        theme_cursor: ws.input_scratch.cursor_color,
                        sound: false,
                    },
                    seed: aterm_effects::genome::mix(wid.0 ^ 0x5052_4953_4D5F_574B),
                    echo: output_echo.streak(),
                    cells: &ws.input_scratch.cells,
                    cursor: (ws.input_scratch.cursor_row, ws.input_scratch.cursor_col),
                    viewport: (rows, cols),
                    content_seq: exact_focus.content_seq,
                    drawn_at: ws.input_scratch.drawn_at,
                    geom: crate::cursor_glow::Geom {
                        cw: cell_w,
                        ch: cell_h,
                        rows,
                        cols,
                        origin_x: 0,
                        origin_y: 0,
                        win_w: u16::try_from(cols * cell_w).unwrap_or(u16::MAX),
                        win_h: u16::try_from(rows * cell_h).unwrap_or(u16::MAX),
                        head: 0,
                    },
                })
                .as_ref(),
        );
        self.kitty_log.observe_ambient(
            exact_focus.session,
            ws.fx.decos_mut().drain_kitty_sightings(),
            lexicon,
            now,
            kitty_log_on,
        );
        let curse_drain = crate::app_render::drain_curse_bonk_cues(
            ws.fx.decos_mut(),
            glow_cfg.style,
            self.config.trail_sound_voice(),
            effect_geom.cols,
            None,
            false,
            |_| {},
        );
        ws.fx.companions_mut().cursor_cat.on_curse(
            now,
            words
                .primed_wince_hits
                .saturating_add(curse_drain.wince_hits),
        );
        let default_bg = ws.input_scratch.default_bg;
        let cursor_color = ws.input_scratch.cursor_color;
        let _ = crate::app_render::emit_routed_cursor_companion(
            ws,
            effect_geom,
            &pet,
            false,
            routed,
            kitty_alpha,
            now,
            !animate_cat,
            default_bg,
            cursor_color,
            glow_cfg.accent,
        );
        ws.fx.publish_words(&mut ws.input_scratch);
    }

    fn composed_capture_cursor_fx_clock(
        &self,
        wid: crate::WindowId,
        now: Instant,
    ) -> crate::app_render::ComposedCursorFxClock {
        let recording_here = self.video_rec.as_ref().is_some_and(|r| r.window == wid);
        let has_os_window = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.os_window.is_some());
        if capture_ticks_cursor_fx(has_os_window, recording_here) {
            crate::app_render::ComposedCursorFxClock::Advance(now)
        } else {
            crate::app_render::ComposedCursorFxClock::Retain { observed_at: now }
        }
    }

    /// Reconstruct the leaf inventory of a just-presented pure-terminal frame without touching a terminal lock.
    #[cfg(test)]
    pub(crate) fn presented_terminal_capture_grid(
        &self,
        wid: crate::WindowId,
    ) -> Option<crate::app_render::TerminalCaptureGrid> {
        let (plan, route) = self.active_visible_frame_layout(wid)?;
        self.presented_terminal_capture_grid_from_layout(wid, &plan, route)
    }

    fn presented_terminal_capture_grid_from_layout(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: crate::VisibleContentRoute,
    ) -> Option<crate::app_render::TerminalCaptureGrid> {
        let mut grid = crate::app_render::TerminalCaptureGrid {
            composed: false,
            focus: None,
            leaves: Vec::new(),
        };
        let crate::VisibleContentRoute::Terminal { composed } = route else {
            return None;
        };
        self.refill_presented_terminal_capture_grid(wid, composed, plan, &mut grid)
            .then_some(grid)
    }

    fn refill_presented_terminal_capture_grid(
        &self,
        wid: crate::WindowId,
        composed: bool,
        plan: &crate::tab_model::VisibleLeafPlan,
        grid: &mut crate::app_render::TerminalCaptureGrid,
    ) -> bool {
        let Some(window) = self.windows.get(&wid) else {
            return false;
        };
        if plan.leaves.is_empty()
            || plan.leaves.len() != window.capture_leaf_snapshot_seqs.len()
            || plan.leaves.len() != window.capture_leaf_content_seqs.len()
        {
            return false;
        }
        grid.composed = composed;
        grid.focus = None;
        grid.leaves.clear();
        for ((leaf, snapshot_seq), content_seq) in plan
            .leaves
            .iter()
            .zip(&window.capture_leaf_snapshot_seqs)
            .zip(&window.capture_leaf_content_seqs)
        {
            let Some(crate::tab_model::View::Terminal(terminal)) =
                self.view_store.get(leaf.view).copied()
            else {
                grid.leaves.clear();
                return false;
            };
            grid.leaves.push(crate::app_render::TerminalCaptureLeaf {
                view: leaf.view,
                session: terminal.session,
                focused: leaf.focused,
                rows: (leaf.rect.size.height.round() as usize).max(1),
                cols: (leaf.rect.size.width.round() as usize).max(1),
                snapshot_seq: *snapshot_seq,
                content_seq: *content_seq,
            });
        }
        true
    }

    /// The identity of the tray `window` presents: its one card's, or its tray stack's (the
    /// stack's union, and its cards' identities hashed in order).
    fn recording_card_identity(
        window: &crate::WindowState,
    ) -> Option<crate::VideoPresentedCardIdentity> {
        let one = |card: &crate::SettingsCard| crate::VideoPresentedCardIdentity {
            width: card.pw,
            height: card.ph,
            x: card.dx,
            y: card.dy,
            model: card.fp,
            geometry: card.geom,
        };
        let Some(stack) = window.tray_stack() else {
            return window.present_card().map(one);
        };
        let ids: Vec<_> = stack.into_iter().flatten().map(one).collect();
        let x0 = ids.iter().map(|c| c.x).min()?;
        let y0 = ids.iter().map(|c| c.y).min()?;
        let x1 = ids.iter().map(|c| c.x.saturating_add(c.width)).max()?;
        let y1 = ids.iter().map(|c| c.y.saturating_add(c.height)).max()?;
        let mut model = aterm_hash::FNV64_OFFSET;
        for id in &ids {
            for word in [
                u64::from(id.width),
                u64::from(id.height),
                u64::from(id.x),
                u64::from(id.y),
                id.model,
                id.geometry,
            ] {
                model = aterm_hash::fnv1a_64_u64_le(model, word);
            }
        }
        Some(crate::VideoPresentedCardIdentity {
            width: x1 - x0,
            height: y1 - y0,
            x: x0,
            y: y0,
            model,
            geometry: model,
        })
    }

    fn refill_recording_native_leaf_identities(
        &self,
        wid: crate::WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        identities: &mut Vec<crate::VideoPresentedLeafIdentity>,
    ) -> Option<bool> {
        let window = self.windows.get(&wid)?;
        let (cell_w, cell_h) = self.win_cell_size(wid);
        let mut changed = identities.len() != plan.leaves.len();
        for (index, leaf) in plan.leaves.iter().enumerate() {
            let retained = window.leaf_render_cache.get(&leaf.view)?;
            let identity = match self.view_store.get(leaf.view).copied()? {
                crate::tab_model::View::Terminal(terminal) => crate::VideoPresentedLeafIdentity {
                    kind: "terminal",
                    view: leaf.view.get(),
                    session: Some(terminal.session),
                    focused: leaf.focused,
                    width: (leaf.rect.size.width * cell_w as f32).round().max(1.0) as u32,
                    height: (leaf.rect.size.height * cell_h as f32).round().max(1.0) as u32,
                    snapshot_seq: Some(retained.input.snapshot_seq),
                    native_stamp: None,
                },
                crate::tab_model::View::Native(_) => {
                    let raster = retained.native.as_ref()?;
                    crate::VideoPresentedLeafIdentity {
                        kind: "native",
                        view: leaf.view.get(),
                        session: None,
                        focused: leaf.focused,
                        width: raster.width,
                        height: raster.height,
                        snapshot_seq: None,
                        native_stamp: Some(raster.stamp),
                    }
                }
            };
            if let Some(slot) = identities.get_mut(index) {
                changed |= *slot != identity;
                *slot = identity;
            } else {
                identities.push(identity);
                changed = true;
            }
        }
        identities.truncate(plan.leaves.len());
        Some(changed)
    }

    /// Commit the lightweight semantic half of the just-successful video present.
    pub(crate) fn commit_recording_presented_meta(
        &mut self,
        wid: crate::WindowId,
        route: crate::VisibleContentRoute,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        if !self.video_rec.as_ref().is_some_and(|rec| rec.window == wid) {
            return;
        }
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        let input_epoch = match &window.present {
            Some(
                crate::PresentTarget::Gpu { window_gpu, .. }
                | crate::PresentTarget::Virtual { window_gpu },
            ) => window_gpu.resident_input_epoch(),
            _ => 0,
        };
        if input_epoch == 0 || input_epoch == u64::MAX {
            return;
        }
        let serial = window.capture_present_serial;
        let invert = window.capture_present_invert;
        let overlay = window.capture_present_overlay;
        let metrics = window.metrics;
        let cell_size = self.win_cell_size(wid);
        let destination_height = window.win_px.map(|size| size.height.max(1) as usize);
        let overlay_fingerprint = window
            .last_present
            .map_or_else(|| window.overlay_fp(), |key| key.settings_fp);
        let card = Self::recording_card_identity(window);
        let theme = crate::VideoPresentedThemeIdentity {
            colors: self.image_theme_fingerprint(),
        };
        let blink_phase = window.blink_phase;
        let cursor_override = (!window.focused && window.os_window.is_some())
            .then_some(aterm_core::terminal::CursorStyle::HollowBlock);
        let selection_inactive = self.render_knobs.selection_inactive && !window.focused;
        let previous = self
            .video_rec
            .as_mut()
            .filter(|recording| recording.window == wid)
            .and_then(|recording| recording.presented.take());
        let (mut terminal_grid, mut native_leaves) = previous.map_or_else(
            || {
                (
                    crate::app_render::TerminalCaptureGrid {
                        composed: false,
                        focus: None,
                        leaves: Vec::new(),
                    },
                    Vec::new(),
                )
            },
            |mut previous| {
                (
                    previous.terminal_grid.take().unwrap_or(
                        crate::app_render::TerminalCaptureGrid {
                            composed: false,
                            focus: None,
                            leaves: Vec::new(),
                        },
                    ),
                    previous.native_leaves.take().unwrap_or_default(),
                )
            },
        );

        let terminal_grid = if let crate::VisibleContentRoute::Terminal { composed } = route {
            if !self.refill_presented_terminal_capture_grid(wid, composed, plan, &mut terminal_grid)
            {
                return;
            }
            Some(terminal_grid)
        } else {
            None
        };
        let native_leaves = if route.has_visible_native() {
            let Some(_) =
                self.refill_recording_native_leaf_identities(wid, plan, &mut native_leaves)
            else {
                return;
            };
            Some(native_leaves)
        } else {
            None
        };

        let presented = crate::VideoPresentedMeta {
            input_epoch,
            route,
            serial,
            invert,
            overlay,
            terminal_grid,
            metrics,
            cell_size,
            destination_height,
            theme,
            overlay_fingerprint,
            blink_phase,
            cursor_override,
            selection_inactive,
            card,
            native_leaves,
        };
        if let Some(recording) = self
            .video_rec
            .as_mut()
            .filter(|recording| recording.window == wid)
        {
            recording.presented = Some(presented);
        }
    }

    fn recording_presented_frame_capture(
        &self,
        wid: crate::WindowId,
        include_terminal_grid: bool,
        include_native_metadata: bool,
    ) -> Result<RecordingPresentedFrame, String> {
        let recording = self
            .video_rec
            .as_ref()
            .filter(|recording| recording.window == wid)
            .ok_or_else(|| "image has no active recording for this window".to_string())?;
        let presented = recording
            .presented
            .as_ref()
            .ok_or_else(|| "image deferred: recording has not presented a frame yet".to_string())?;
        let window = self
            .windows
            .get(&wid)
            .ok_or_else(|| "image deferred: recorded window closed before capture".to_string())?;

        let (current_plan, current_route) = self
            .active_visible_frame_layout(wid)
            .ok_or_else(|| "image deferred: recording layout changed before capture".to_string())?;
        let current_destination_height = window.win_px.map(|size| size.height.max(1) as usize);
        let current_card = Self::recording_card_identity(window);
        let current_theme = crate::VideoPresentedThemeIdentity {
            colors: self.image_theme_fingerprint(),
        };
        let current_cursor_override = (!window.focused && window.os_window.is_some())
            .then_some(aterm_core::terminal::CursorStyle::HollowBlock);
        let current_selection_inactive = self.render_knobs.selection_inactive && !window.focused;
        if current_route != presented.route
            || window.capture_present_serial != presented.serial
            || window.metrics != presented.metrics
            || self.win_cell_size(wid) != presented.cell_size
            || current_destination_height != presented.destination_height
            || current_theme != presented.theme
            || window.overlay_fp() != presented.overlay_fingerprint
            || window.blink_phase != presented.blink_phase
            || current_cursor_override != presented.cursor_override
            || current_selection_inactive != presented.selection_inactive
            || current_card != presented.card
        {
            return Err(
                "image deferred: recording frame geometry or chrome changed before capture"
                    .to_string(),
            );
        }

        if let Some(grid) = presented.terminal_grid.as_ref() {
            if current_plan.leaves.len() != grid.leaves.len()
                || !current_plan
                    .leaves
                    .iter()
                    .zip(&grid.leaves)
                    .all(|(leaf, captured)| {
                        let session = match self.view_store.get(leaf.view).copied() {
                            Some(crate::tab_model::View::Terminal(terminal)) => terminal.session,
                            _ => return false,
                        };
                        leaf.view == captured.view
                            && session == captured.session
                            && leaf.focused == captured.focused
                            && (leaf.rect.size.height.round() as usize).max(1) == captured.rows
                            && (leaf.rect.size.width.round() as usize).max(1) == captured.cols
                    })
            {
                return Err(
                    "image deferred: recording terminal layout changed before capture".to_string(),
                );
            }
        } else if matches!(presented.route, crate::VisibleContentRoute::Terminal { .. }) {
            return Err("image deferred: recorded terminal inventory is unavailable".to_string());
        }

        if presented.route.has_visible_native() {
            let expected = presented.native_leaves.as_ref().ok_or_else(|| {
                "image deferred: recorded native inventory is unavailable".to_string()
            })?;
            let mut current = Vec::with_capacity(expected.len());
            self.refill_recording_native_leaf_identities(wid, &current_plan, &mut current)
                .ok_or_else(|| {
                    "image deferred: recording native layout changed before capture".to_string()
                })?;
            if &current != expected {
                return Err(
                    "image deferred: recording native content changed before capture".to_string(),
                );
            }
        }

        let native_metadata = if include_native_metadata && presented.route.has_visible_native() {
            Some(self.native_image_metadata_from_plan(
                wid,
                &current_plan,
                ImageFrameFacts {
                    phase: "presented",
                    capture_serial: Some(presented.serial),
                    width: 0,
                    height: 0,
                },
            )?)
        } else {
            None
        };

        let window_gpu = match window.present.as_ref() {
            Some(
                crate::PresentTarget::Gpu { window_gpu, .. }
                | crate::PresentTarget::Virtual { window_gpu },
            ) => window_gpu,
            _ => {
                return Err("image deferred: recording resident frame is unavailable".to_string());
            }
        };
        let (mut input, effects_transport_shift_y) = window_gpu
            .clone_resident_input_with_transport_at(presented.input_epoch)
            .ok_or_else(|| {
                "image deferred: recording advanced before capture could clone its frame"
                    .to_string()
            })?;
        if effects_transport_shift_y != 0 {
            let canonical_shift = effects_transport_shift_y.checked_neg().ok_or_else(|| {
                "image deferred: recording effect transport shift is invalid".to_string()
            })?;
            if !crate::try_shift_window_absolute_effects_y(&mut input, canonical_shift) {
                return Err(
                    "image deferred: recording effect transport could not be canonicalized"
                        .to_string(),
                );
            }
        }
        Ok(RecordingPresentedFrame {
            presented: PresentedCapture {
                serial: presented.serial,
                visuals: crate::app_render::HostVisualState {
                    invert: presented.invert,
                    overlay: presented.overlay,
                },
                client: None,
            },
            input,
            terminal_grid: include_terminal_grid
                .then(|| presented.terminal_grid.clone())
                .flatten(),
            facts: RecordedFacts {
                cell_size: presented.cell_size,
                theme_fingerprint: self.image_theme_fingerprint(),
                overlay_fingerprint: presented.overlay_fingerprint,
                native_metadata,
            },
        })
    }

    fn recording_presented_destination_capture(
        &mut self,
        wid: crate::WindowId,
    ) -> Result<RecordingPresentedDestination, String> {
        let (input_epoch, serial, route) = {
            let recording = self
                .video_rec
                .as_ref()
                .filter(|recording| recording.window == wid)
                .ok_or_else(|| "image has no active recording for this window".to_string())?;
            let presented = recording.presented.as_ref().ok_or_else(|| {
                "image deferred: recording has not presented a frame yet".to_string()
            })?;
            (presented.input_epoch, presented.serial, presented.route)
        };
        let window = self
            .windows
            .get(&wid)
            .ok_or_else(|| "image deferred: recorded window closed before capture".to_string())?;
        if window.capture_present_serial != serial {
            return Err(
                "image deferred: recording destination advanced before capture".to_string(),
            );
        }
        if window.os_window.is_some() {
            return Err(
                "image deferred: recording destination is not a headless virtual target"
                    .to_string(),
            );
        }

        let App {
            backend, windows, ..
        } = self;
        let gpu = backend.gpu_mut().ok_or_else(|| {
            "image deferred: recording virtual GPU backend is unavailable".to_string()
        })?;
        let window_gpu = match windows.get(&wid).and_then(|window| window.present.as_ref()) {
            Some(crate::PresentTarget::Virtual { window_gpu }) => window_gpu,
            _ => {
                return Err(
                    "image deferred: recording virtual destination is unavailable".to_string(),
                );
            }
        };
        let captured = gpu.virtual_presented_snapshot_current(
            window_gpu,
            input_epoch,
            crate::metrics::now_us(),
        )?;
        Ok(RecordingPresentedDestination {
            pixels: CapturePixels::raw(captured),
            route,
            serial,
        })
    }

    /// Present one capture frame from caller-supplied synchronous layout authority when available, otherwise resolve the ordinary redraw layout.
    pub(crate) fn redraw_capture_frame(
        &mut self,
        wid: crate::WindowId,
        frame_layout: Option<VisibleFrameLayoutRef<'_>>,
    ) {
        self.mark_capture_present(wid, true);
        if let Some((plan, route)) = frame_layout {
            self.redraw_window_from_layout(wid, plan, route);
        } else {
            self.redraw_window(wid);
        }
        self.mark_capture_present(wid, false);
    }

    fn mark_capture_present(&mut self, wid: crate::WindowId, on: bool) {
        if let Some(crate::PresentTarget::Gpu { window_gpu, .. }) = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.present.as_mut())
        {
            window_gpu.set_capture_present(on);
        }
    }

    fn capture_target(&self, wid: WindowId) -> CaptureTarget {
        let Some(window) = self.windows.get(&wid) else {
            return CaptureTarget::Headless;
        };
        #[cfg(test)]
        if !self.backend.is_gpu()
            && matches!(
                &window.present,
                Some(crate::PresentTarget::Cpu { surface }) if surface.scripted().is_some()
            )
        {
            return CaptureTarget::Windowed;
        }
        if window.os_window.is_some() {
            CaptureTarget::Windowed
        } else {
            CaptureTarget::Headless
        }
    }

    fn present_before_capture(
        &mut self,
        wid: WindowId,
        frame_layout: Option<VisibleFrameLayoutRef<'_>>,
        take: PresentTake,
    ) -> Result<Option<PresentedCapture>, String> {
        if self.capture_target(wid) == CaptureTarget::Headless {
            return Ok(None);
        }
        let tap = take != PresentTake::Model;
        let tap_required = take == PresentTake::ClientTap;
        let mut captured = None;
        let mut acquire_pending = false;
        let mut last_capture_error = None;
        self.capture_barrier = Some(wid);
        let presented =
            crate::run_capture_present_barrier(self.capture_present_budget.remaining, || {
                if acquire_pending {
                    return false;
                }
                let resumed_tap = tap
                    && self
                        .windows
                        .get(&wid)
                        .is_some_and(|window| window.capture_acquire_armed);
                if tap && !resumed_tap {
                    self.discard_presented_client_capture(wid);
                }
                let before = self
                    .windows
                    .get(&wid)
                    .map_or(0, |window| window.capture_present_serial);
                let armed = resumed_tap
                    || (tap
                        && match self.arm_presented_client_capture(wid) {
                            Ok(()) => true,
                            Err(error) if tap_required => {
                                last_capture_error = Some(error);
                                return false;
                            }
                            Err(_) => false,
                        });
                let Some(window) = self.windows.get_mut(&wid) else {
                    last_capture_error = Some("capture lost its window".to_string());
                    return false;
                };
                let _ = window.present_retry.on_external_stimulus();
                window.last_present = None;
                self.redraw_capture_frame(wid, frame_layout);
                let Some(window) = self.windows.get_mut(&wid) else {
                    last_capture_error = Some("capture lost its window".to_string());
                    return false;
                };
                acquire_pending = window.gpu_acquire_pending();
                let serial = window.capture_present_serial;
                self.capture_present_budget.record_attempt(
                    acquire_pending,
                    serial != before,
                    !window.present_retry.present_attempt_allowed(),
                );
                if acquire_pending {
                    if armed {
                        window.capture_acquire_armed = true;
                    }
                    return false;
                }
                if serial == before {
                    last_capture_error = Some("the capture's present did not complete".to_string());
                    return false;
                }
                let visuals = crate::app_render::HostVisualState {
                    invert: window.capture_present_invert,
                    overlay: window.capture_present_overlay,
                };
                let client = if armed {
                    match self.take_presented_client_capture(wid) {
                        Ok(client) => Some(client),
                        Err(error) if tap_required => {
                            last_capture_error = Some(error);
                            return false;
                        }
                        Err(_) => {
                            self.discard_presented_client_capture(wid);
                            None
                        }
                    }
                } else {
                    None
                };
                captured = Some(PresentedCapture {
                    serial,
                    visuals,
                    client,
                });
                true
            });
        self.capture_barrier = None;
        if acquire_pending {
            return Err(GPU_CAPTURE_PENDING.to_string());
        }
        if presented && captured.is_some() {
            return Ok(captured);
        }
        if tap {
            self.discard_presented_client_capture(wid);
        }
        let detail = last_capture_error
            .map(|error| format!(": {error}"))
            .unwrap_or_default();
        Err(format!(
            "capture could not synchronize a presented frame after {} present attempts{detail}",
            crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT,
        ))
    }

    fn take_rerender_input(&self, wid: WindowId) -> aterm_render::RenderInput {
        #[cfg(test)]
        crate::work_counts::capture_input_cloned();
        self.windows
            .get(&wid)
            .map(|window| window.input_scratch.clone())
            .unwrap_or_default()
    }

    fn resolve_capture_source(
        &mut self,
        front: WindowId,
        policy: SourcePolicy,
    ) -> Result<ResolvedCapture, CaptureDeferral> {
        self.resolve_capture_source_at(front, policy, None)
    }

    /// [`Self::resolve_capture_source`], a windowless capture's instant `clock` where the caller
    /// injects one (`lab trail-diff`'s dump, [`Self::windowless_frame_at`]), else now.
    fn resolve_capture_source_at(
        &mut self,
        front: WindowId,
        policy: SourcePolicy,
        clock: Option<Instant>,
    ) -> Result<ResolvedCapture, CaptureDeferral> {
        self.ensure_pixel_backend();
        let (plan, route) = self
            .active_visible_frame_layout(front)
            .ok_or(CaptureDeferral::Nothing)?;
        if self.plan_shows_private(&plan) {
            return Err(CaptureDeferral::Refused(
                crate::app_control::PRIVATE_VIEW_REFUSED.to_owned(),
            ));
        }
        let windowed = self.capture_target(front) == CaptureTarget::Windowed;
        let wants_destination = policy.destination != DestinationDemand::Never;
        let facts = SourceFacts {
            windowed,
            translucent: windowed && wants_destination && self.client_is_translucent(front),
            guards: if windowed && wants_destination {
                self.client_capture_guards(front)
            } else {
                Ok(())
            },
            copyable: windowed && wants_destination && self.client_tap_is_copyable(front),
            recording: self
                .video_rec
                .as_ref()
                .is_some_and(|recording| recording.window == front),
        };
        let kind = capture_source_kind(policy.destination, facts)
            .map_err(|error| CaptureDeferral::Refused(error.to_string()))?;
        if windowed {
            let take = match (kind, policy.destination) {
                (SourceKind::Rerender, _) => PresentTake::Model,
                (SourceKind::Destination, DestinationDemand::Required) => PresentTake::ClientTap,
                (SourceKind::Destination, _) => PresentTake::ClientTapOrModel,
            };
            return self.presented_capture_source(front, plan, route, policy, take);
        }
        if facts.recording {
            return self.recorded_capture_source(front, plan, route, policy, kind);
        }
        let (plan, terminal_grid) = self.stage_headless_capture(front, plan, route, clock)?;
        Ok(ResolvedCapture {
            source: CaptureSource::Rerender {
                serial: None,
                visuals: self.host_visual_state(front, clock.unwrap_or_else(Instant::now)),
            },
            model: None,
            context: CaptureContext {
                plan,
                route,
                terminal_grid,
                recorded: None,
            },
        })
    }

    fn presented_capture_source(
        &mut self,
        front: WindowId,
        plan: crate::tab_model::VisibleLeafPlan,
        route: crate::VisibleContentRoute,
        policy: SourcePolicy,
        take: PresentTake,
    ) -> Result<ResolvedCapture, CaptureDeferral> {
        let presented = self
            .present_before_capture(front, Some((&plan, route)), take)
            .map_err(|error| {
                if error == GPU_CAPTURE_PENDING {
                    CaptureDeferral::PendingGpu
                } else {
                    CaptureDeferral::Refused(error)
                }
            })?;
        let presented = presented.ok_or(CaptureDeferral::Nothing)?;
        let terminal_grid = match route {
            crate::VisibleContentRoute::Terminal { .. } if policy.metadata => Some(
                self.presented_terminal_capture_grid_from_layout(front, &plan, route)
                    .ok_or_else(|| {
                        CaptureDeferral::Refused(
                            "image: the presented frame has no terminal inventory".to_string(),
                        )
                    })?,
            ),
            _ => None,
        };
        let pixels = presented.client.map(|client| {
            if policy.frame_only {
                self.presented_frame_of(front, client)
            } else {
                Ok(client)
            }
        });
        let source = match pixels {
            Some(Ok(pixels)) => CaptureSource::Destination {
                pixels,
                serial: presented.serial,
            },
            Some(Err(error)) if take == PresentTake::ClientTap => {
                return Err(CaptureDeferral::Refused(error));
            }
            Some(Err(_)) | None => CaptureSource::Rerender {
                serial: Some(presented.serial),
                visuals: presented.visuals,
            },
        };
        Ok(ResolvedCapture {
            source,
            model: None,
            context: CaptureContext {
                plan,
                route,
                terminal_grid,
                recorded: None,
            },
        })
    }

    fn recorded_capture_source(
        &mut self,
        front: WindowId,
        plan: crate::tab_model::VisibleLeafPlan,
        route: crate::VisibleContentRoute,
        policy: SourcePolicy,
        kind: SourceKind,
    ) -> Result<ResolvedCapture, CaptureDeferral> {
        let destination = match kind {
            SourceKind::Rerender => None,
            SourceKind::Destination => Some(
                self.recording_presented_destination_capture(front)
                    .map_err(CaptureDeferral::Refused)?,
            ),
        };
        if !policy.model
            && let Some(destination) = destination
        {
            return Ok(ResolvedCapture {
                source: CaptureSource::Destination {
                    pixels: destination.pixels,
                    serial: destination.serial,
                },
                model: None,
                context: CaptureContext {
                    plan,
                    route: destination.route,
                    terminal_grid: None,
                    recorded: None,
                },
            });
        }
        let recorded = self
            .recording_presented_frame_capture(front, policy.metadata, policy.metadata)
            .map_err(CaptureDeferral::Refused)?;
        let source = match destination {
            Some(destination) => {
                debug_assert_eq!(destination.serial, recorded.presented.serial);
                CaptureSource::Destination {
                    pixels: destination.pixels,
                    serial: destination.serial,
                }
            }
            None => CaptureSource::Rerender {
                serial: Some(recorded.presented.serial),
                visuals: recorded.presented.visuals,
            },
        };
        Ok(ResolvedCapture {
            source,
            model: Some(recorded.input),
            context: CaptureContext {
                plan,
                route,
                terminal_grid: recorded.terminal_grid,
                recorded: Some(recorded.facts),
            },
        })
    }

    fn sample_native_preview_for_capture(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        let Some(window) = self.windows.get(&wid) else {
            return;
        };
        if !crate::native_preview_may_tick(true, window.focused, false, window.overlay.is_some()) {
            return;
        }
        let Some((_, view)) = self.active_native_view(wid) else {
            return;
        };
        let Ok(viewport) = self.native_ui_viewport_in_plan(wid, view, plan) else {
            return;
        };
        let phase_ms = self.native_preview_phase_ms();
        self.invalidate_active_native_settings_preview_in(wid, phase_ms, viewport);
    }

    fn stage_headless_capture(
        &mut self,
        wid: WindowId,
        plan: crate::tab_model::VisibleLeafPlan,
        route: crate::VisibleContentRoute,
        clock: Option<Instant>,
    ) -> Result<
        (
            crate::tab_model::VisibleLeafPlan,
            Option<crate::app_render::TerminalCaptureGrid>,
        ),
        CaptureDeferral,
    > {
        use crate::app_render::CapturePreparation;
        self.sample_native_preview_for_capture(wid, &plan);
        let capture_now = clock.unwrap_or_else(Instant::now);
        let (plan, terminal_grid) = match route {
            crate::VisibleContentRoute::Terminal { .. } => {
                let clock = self.composed_capture_cursor_fx_clock(wid, capture_now);
                let (grid, plan) = match self
                    .prepare_terminal_capture_grid_with_cursor_fx_for_plan_outcome(wid, plan, clock)
                {
                    CapturePreparation::Ready(prepared) => prepared,
                    CapturePreparation::Held { retry_at } => {
                        return Err(CaptureDeferral::Held { retry_at });
                    }
                    CapturePreparation::Unavailable => return Err(CaptureDeferral::Nothing),
                };
                let focus = grid.focus.ok_or(CaptureDeferral::Nothing)?;
                self.capture_words_sampled(wid, capture_now, focus);
                self.refresh_pane_headers(wid, &plan);
                self.paint_pane_headers(wid);
                self.splice_tab_strip(wid);
                self.refresh_pane_overlay_for_capture(wid, &plan, capture_now);
                (plan, Some(grid))
            }
            crate::VisibleContentRoute::Native { .. } => {
                if !self.prepare_native_input_scratch_from_capture_plan(wid, &plan) {
                    return Err(CaptureDeferral::Nothing);
                }
                (plan, None)
            }
            crate::VisibleContentRoute::Heterogeneous => {
                let clock = self.composed_capture_cursor_fx_clock(wid, capture_now);
                match self.prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan_outcome(
                    wid,
                    Some(clock),
                    &plan,
                ) {
                    CapturePreparation::Ready(_) => {}
                    CapturePreparation::Held { retry_at } => {
                        return Err(CaptureDeferral::Held { retry_at });
                    }
                    CapturePreparation::Unavailable => return Err(CaptureDeferral::Nothing),
                }
                (plan, None)
            }
        };
        if self.compose_route_chrome(wid, route, &plan).is_none() {
            return Err(CaptureDeferral::Nothing);
        }
        Ok((plan, terminal_grid))
    }

    /// Lay a windowless window's terminal front as its next capture would ([`Self::stage_headless_capture`]),
    /// presenting nothing and ticking no cursor effect: a windowless instance composes a frame only
    /// when a capture asks, so a `pointer` that hit-tests the glass (the composer rules' hit lists
    /// and the grid they were laid on) first lays what that capture shows. A native front is
    /// [`Self::commit_glassless_native_frame`]'s. Whether a frame's chrome was laid.
    pub(crate) fn lay_glassless_terminal_chrome(&mut self, wid: WindowId) -> bool {
        use crate::app_render::{CapturePreparation, ComposedCursorFxClock};
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.os_window.is_none() && ws.present.is_none())
        {
            return false;
        }
        let Some((plan, route)) = self.active_visible_frame_layout(wid) else {
            return false;
        };
        if !matches!(route, crate::VisibleContentRoute::Terminal { .. }) {
            return false;
        }
        let now = Instant::now();
        let CapturePreparation::Ready((grid, plan)) = self
            .prepare_terminal_capture_grid_with_cursor_fx_for_plan_outcome(
                wid,
                plan,
                ComposedCursorFxClock::Retain { observed_at: now },
            )
        else {
            return false;
        };
        if grid.focus.is_none() {
            return false;
        }
        self.refresh_pane_headers(wid, &plan);
        self.paint_pane_headers(wid);
        self.splice_tab_strip(wid);
        self.refresh_pane_overlay_for_capture(wid, &plan, now);
        self.compose_route_chrome(wid, route, &plan).is_some()
    }

    fn rerender_capture(
        &mut self,
        wid: WindowId,
        input: &mut aterm_render::RenderInput,
        visuals: crate::app_render::HostVisualState,
    ) -> Result<Frame, String> {
        let tray_floor_y = self.paste_banner_tray_floor_y(wid);
        self.bind_window_renderer_state(wid);
        let render_t0 = Instant::now();
        let App {
            backend,
            introspect_gpu,
            windows,
            ..
        } = self;
        let window = windows
            .get(&wid)
            .ok_or_else(|| "capture lost its window".to_string())?;
        // A tray stack is folded here, on the CPU, as the renderers that take one card fold it.
        let folded = window.tray_stack().and_then(|stack| {
            let cards: Vec<_> = stack.into_iter().flatten().collect();
            crate::stats_deck::fold_cards(&cards, 1)
        });
        let tray = folded
            .as_ref()
            .or_else(|| window.present_card())
            .and_then(|card| tray_quad_below_y(card, tray_floor_y));
        let destination_height = window.win_px.map(|size| size.height.max(1) as usize);
        let mut frame = backend.render_input_for_destination(
            introspect_gpu,
            input,
            tray,
            destination_height,
        )?;
        let cpu_tray = (!backend.is_gpu()).then_some(tray).flatten();
        let (width, height) = (frame.width, frame.height);
        apply_host_chrome_at(
            &mut frame.pixels,
            width,
            height,
            0,
            0,
            width,
            height,
            0..0,
            cpu_tray,
            visuals.invert,
            visuals.overlay,
        );
        crate::metrics::record_offscreen_raster(render_t0.elapsed().as_nanos() as u64);
        Ok(frame)
    }

    /// One windowless frame of `wid` at `now`, taken as a `--no-windows` instance's `image`
    /// takes it ([`Self::image_body`]'s source, then its rerender): `None` while a synchronized
    /// update holds the frame. The light-fork pin's capture entry (`light_fork_pin_tests`).
    #[cfg(all(unix, test))]
    pub(crate) fn windowless_frame_at(
        &mut self,
        wid: WindowId,
        now: Instant,
    ) -> Result<Option<Frame>, String> {
        let policy = SourcePolicy {
            destination: DestinationDemand::of_image(false),
            frame_only: true,
            model: false,
            metadata: false,
        };
        let capture = match self.resolve_capture_source_at(wid, policy, Some(now)) {
            Ok(capture) => capture,
            Err(CaptureDeferral::Held { .. }) => return Ok(None),
            Err(CaptureDeferral::Nothing) => return Err("no frame to capture".to_string()),
            Err(CaptureDeferral::PendingGpu) => {
                return Err("a windowless frame waits on no GPU".to_string());
            }
            Err(CaptureDeferral::Refused(error)) => return Err(error),
        };
        let CaptureSource::Rerender { visuals, .. } = capture.source else {
            return Err("a windowless frame is a rerender".to_string());
        };
        let mut input = self.take_rerender_input(wid);
        self.rerender_capture(wid, &mut input, visuals).map(Some)
    }

    pub(crate) fn snapshot_visible_text(
        &self,
        wid: WindowId,
        route: crate::VisibleContentRoute,
        plan: &crate::tab_model::VisibleLeafPlan,
        input: &aterm_render::RenderInput,
        strip_rows: usize,
        cols: usize,
    ) -> String {
        let terminal = terminal_capture_text(input, strip_rows, cols);
        let native = if route.has_visible_native() {
            self.windows.get(&wid).map(|window| {
                let mut native = String::new();
                for leaf in &plan.leaves {
                    if !matches!(
                        self.view_store.get(leaf.view),
                        Some(crate::tab_model::View::Native(_))
                    ) {
                        continue;
                    }
                    native.push_str(&format!(
                        "[native view={} focused={}]\n",
                        leaf.view.get(),
                        leaf.focused
                    ));
                    if self.view_is_private(leaf.view) {
                        native.push_str(crate::app_control::PRIVATE_VIEW_WITHHELD);
                        native.push('\n');
                        continue;
                    }
                    let Some(raster) = window
                        .leaf_render_cache
                        .get(&leaf.view)
                        .and_then(|cache| cache.native.as_ref())
                    else {
                        native.push_str("semantics unavailable\n");
                        continue;
                    };
                    for line in crate::app_control::semantic_text_lines(&raster.compiled) {
                        native.push_str(&line);
                        native.push('\n');
                    }
                }
                native
            })
        } else {
            None
        };
        let mut visible = match native {
            None => terminal,
            Some(native) => match route {
                crate::VisibleContentRoute::Native { .. } => native,
                crate::VisibleContentRoute::Heterogeneous if native.is_empty() => terminal,
                crate::VisibleContentRoute::Heterogeneous if terminal.is_empty() => native,
                crate::VisibleContentRoute::Heterogeneous => format!("{terminal}{native}"),
                crate::VisibleContentRoute::Terminal { .. } => terminal,
            },
        };
        if let Some(palette) = self.windows.get(&wid).and_then(|window| window.palette()) {
            if !visible.is_empty() && !visible.ends_with('\n') {
                visible.push('\n');
            }
            for line in palette.controls_lines() {
                visible.push_str(&line);
                visible.push('\n');
            }
        }
        visible
    }

    /// Introspect the current visible route and write PNG + semantic text.
    #[cfg(unix)]
    pub(crate) fn snapshot(&mut self) {
        let Some(path) = snapshot_path::resolve() else {
            return; // refusal already logged by resolve()
        };
        self.snapshot_at_path(path);
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn snapshot_at_path(&mut self, path: String) {
        self.run_capture(CaptureRequest::Snapshot { path });
    }

    fn snapshot_body(&mut self, front: Option<WindowId>, path: String) {
        let transaction = match begin_snapshot_generation(std::path::Path::new(&path)) {
            Ok(transaction) => transaction,
            Err(error) => {
                crate::logging::stderr_line!("aterm-gui: snapshot refused for {path}: {error}");
                return;
            }
        };
        let Some(front) = front else {
            return;
        };
        let strip_rows = usize::from(self.chrome_rows(front));
        let policy = SourcePolicy {
            destination: DestinationDemand::Required,
            frame_only: false,
            model: true,
            metadata: false,
        };
        let capture = match self.resolve_capture_source(front, policy) {
            Ok(capture) => capture,
            Err(CaptureDeferral::PendingGpu) => {
                self.queue_gpu_capture(front, CaptureRequest::Snapshot { path }, true);
                return;
            }
            Err(CaptureDeferral::Refused(error)) => {
                crate::logging::stderr_line!("aterm-gui: snapshot refused: {error}");
                return;
            }
            Err(CaptureDeferral::Nothing | CaptureDeferral::Held { .. }) => return,
        };
        self.snapshot_tail(front, capture, strip_rows, transaction);
    }

    fn snapshot_tail(
        &mut self,
        front: WindowId,
        capture: ResolvedCapture,
        strip_rows: usize,
        transaction: SnapshotTransaction,
    ) {
        let ResolvedCapture {
            source,
            model,
            context,
        } = capture;
        let Some(cols) = self
            .windows
            .get(&front)
            .map(|window| usize::from(window.cols))
        else {
            return;
        };
        let text = self.snapshot_visible_text(
            front,
            context.route,
            &context.plan,
            model
                .as_ref()
                .unwrap_or(&self.windows[&front].input_scratch),
            strip_rows,
            cols,
        );
        let pixels = match source {
            CaptureSource::Destination { pixels, .. } => pixels,
            CaptureSource::Rerender { visuals, .. } => {
                let mut input = model.unwrap_or_else(|| self.take_rerender_input(front));
                let frame = self.rerender_capture(front, &mut input, visuals);
                match frame.and_then(CapturePixels::packed) {
                    Ok(pixels) => pixels,
                    Err(error) => {
                        crate::logging::stderr_line!("aterm-gui: snapshot refused: {error}");
                        return;
                    }
                }
            }
        };
        self.submit_encode_job(EncodeJob::Snapshot {
            pixels,
            text,
            transaction,
        });
    }

    /// Hand a capture to the single PNG encode/write worker, spawning it lazily on first use (a run that never screenshots pays nothing).
    pub(crate) fn submit_encode_job(&mut self, mut job: EncodeJob) {
        // A video dump can hold a worker for seconds to minutes (hundreds of full-frame PNG encodes), so it runs on its own one-shot lane and no `image`/`window`/`snapshot` reply queues behind it.
        if matches!(&job, EncodeJob::VideoDump { .. }) {
            let (vtx, vrx) = std::sync::mpsc::channel::<EncodeJob>();
            let spawned = crate::qos::spawn_lane_thread(
                "aterm-video-encode".to_string(),
                crate::qos::Role::Background,
                move || {
                    // Background QoS: a recording encode is cosmetic work no keystroke waits on, so it never takes P-core time from the UI thread on a saturated machine.
                    if let Ok(j) = vrx.recv() {
                        run_encode_job(j, None);
                    }
                },
            )
            .is_ok();
            if spawned {
                match vtx.send(job) {
                    Ok(()) => return,
                    Err(std::sync::mpsc::SendError(returned)) => job = returned,
                }
            }
        }
        let permit = if matches!(&job, EncodeJob::VideoDump { .. }) {
            None
        } else {
            self.capture_permit.take().map(|mut permit| {
                permit.recharge(job.peak_bytes());
                permit
            })
        };
        let mut work = EncodeWork { job, permit };
        if let Some(tx) = &self.encode_tx {
            match tx.try_send(work) {
                Ok(()) => return,
                Err(std::sync::mpsc::TrySendError::Full(work)) => {
                    reject_encode_job(work.job, ENCODE_QUEUE_FULL);
                    return;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(returned)) => {
                    self.encode_tx = None;
                    work = returned;
                }
            }
        }
        let (tx, rx) = std::sync::mpsc::sync_channel::<EncodeWork>(ENCODE_QUEUE_CAPACITY);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-png-encode".to_string(),
            crate::qos::Role::Background,
            move || {
                while let Ok(EncodeWork { job, permit }) = rx.recv() {
                    run_encode_job(job, permit);
                }
            },
        )
        .is_ok();
        if spawned {
            match tx.try_send(work) {
                Ok(()) => self.encode_tx = Some(tx),
                Err(std::sync::mpsc::TrySendError::Full(work)) => {
                    self.encode_tx = Some(tx);
                    reject_encode_job(work.job, ENCODE_QUEUE_FULL);
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(work)) => {
                    reject_encode_job(work.job, ENCODE_WORKER_UNAVAILABLE);
                }
            }
        } else {
            reject_encode_job(work.job, ENCODE_WORKER_UNAVAILABLE);
        }
    }

    fn capture_bytes_estimate(&self, wid: WindowId, charge: CaptureCharge) -> usize {
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        let (width, height) = match ws.win_px {
            Some(px) => (px.width as usize, px.height as usize),
            None => {
                let (cw, ch) = self.win_cell_size(wid);
                let rows = usize::from(ws.rows).saturating_add(usize::from(self.chrome_rows(wid)));
                (
                    usize::from(ws.cols).saturating_mul(cw),
                    rows.saturating_mul(ch),
                )
            }
        };
        let destination = self.destination_capture_bytes(wid, width, height);
        match charge {
            CaptureCharge::Frame(DestinationDemand::Never) => {
                frame_capture_peak(width, height, None)
            }
            CaptureCharge::Frame(_) => frame_capture_peak(width, height, destination),
            #[cfg(any(target_os = "macos", windows))]
            CaptureCharge::Window => {
                self.window_capture_estimate(wid, (width, height), destination)
            }
        }
    }

    fn destination_capture_bytes(
        &self,
        wid: WindowId,
        width: usize,
        height: usize,
    ) -> Option<usize> {
        match self.windows.get(&wid)?.present.as_ref()? {
            crate::PresentTarget::Gpu { gpu_surface, .. } => gpu_surface.snapshot_bytes(),
            crate::PresentTarget::Cpu { .. } => {
                Some(width.saturating_mul(height).saturating_mul(4))
            }
            crate::PresentTarget::Virtual { window_gpu } => window_gpu.virtual_snapshot_bytes(),
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn window_capture_estimate(
        &self,
        wid: WindowId,
        (width, height): (usize, usize),
        destination: Option<usize>,
    ) -> usize {
        let ws = self.windows.get(&wid);
        let (photo_width, photo_height) =
            ws.and_then(|ws| ws.os_window.as_ref())
                .map_or((width, height), |window| {
                    let outer = window.outer_size();
                    (outer.width as usize, outer.height as usize)
                });
        #[cfg(target_os = "macos")]
        let chrome = ws.map_or(0, |ws| {
            let chrome_height = ws.metrics.head.min(photo_height);
            let bitmap = photo_width.saturating_mul(chrome_height).saturating_mul(4);
            chrome_peak_bytes(photo_width, chrome_height, bitmap)
        });
        #[cfg(not(target_os = "macos"))]
        let chrome = 0;
        StitchCharge {
            width: photo_width,
            height: photo_height,
            photograph: photo_width.saturating_mul(photo_height).saturating_mul(4),
            client: destination.unwrap_or_else(|| width.saturating_mul(height).saturating_mul(4)),
            client_width: width,
            chrome,
            outer_shape: ShotEncoding::NATIVE.carries_outer_shape(),
        }
        .peak_bytes()
    }

    fn admit_capture(&mut self, wid: Option<WindowId>, charge: CaptureCharge) -> bool {
        let bytes = wid.map_or(0, |wid| self.capture_bytes_estimate(wid, charge));
        self.admit_capture_bytes(bytes)
    }

    fn admit_capture_bytes(&mut self, bytes: usize) -> bool {
        match CapturePermit::try_reserve(&self.capture_bytes, bytes) {
            Some(permit) => {
                self.capture_permit = Some(permit);
                note_capture_body();
                true
            }
            None => false,
        }
    }

    fn image_metadata(
        &self,
        front: WindowId,
        context: &CaptureContext,
        model: &aterm_render::RenderInput,
        facts: ImageFrameFacts,
        cell: std::sync::Arc<std::sync::OnceLock<crate::control::ImageFrameMetadata>>,
    ) -> Result<PendingMetadata, String> {
        if matches!(context.route, crate::VisibleContentRoute::Terminal { .. }) {
            let grid = context
                .terminal_grid
                .as_ref()
                .ok_or_else(|| "image: the frame has no terminal inventory".to_string())?;
            return Ok(PendingMetadata {
                metadata: self.terminal_image_metadata(
                    front,
                    grid,
                    model,
                    facts,
                    context.recorded.as_ref(),
                )?,
                raster_is_pixels: true,
                cell,
            });
        }
        let metadata = match context
            .recorded
            .as_ref()
            .and_then(|recorded| recorded.native_metadata.clone())
        {
            Some(mut metadata) => {
                metadata.phase = facts.phase;
                metadata.width = facts.width;
                metadata.height = facts.height;
                metadata
            }
            None => self.native_image_metadata_from_plan(front, &context.plan, facts)?,
        };
        Ok(PendingMetadata {
            metadata,
            raster_is_pixels: false,
            cell,
        })
    }

    fn terminal_image_metadata(
        &self,
        front: WindowId,
        grid: &crate::app_render::TerminalCaptureGrid,
        model: &aterm_render::RenderInput,
        facts: ImageFrameFacts,
        recorded: Option<&RecordedFacts>,
    ) -> Result<crate::control::ImageFrameMetadata, String> {
        let ImageFrameFacts {
            phase,
            capture_serial,
            width,
            height,
        } = facts;
        let window = self
            .windows
            .get(&front)
            .ok_or_else(|| "image metadata lost its window".to_string())?;
        let (cell_w, cell_h) =
            recorded.map_or_else(|| self.win_cell_size(front), |recorded| recorded.cell_size);
        let snapshot_fingerprint = Self::terminal_capture_fingerprint(grid, model);
        let geometry = Self::terminal_geometry_fingerprint(width as usize, height as usize, model);
        let singular = (grid.leaves.len() == 1).then(|| grid.leaves[0]);
        let leaves = grid
            .leaves
            .iter()
            .map(|leaf| {
                let leaf_width = singular.map_or_else(
                    || u32::try_from(leaf.cols.saturating_mul(cell_w)).unwrap_or(u32::MAX),
                    |_| width,
                );
                let leaf_height = singular.map_or_else(
                    || u32::try_from(leaf.rows.saturating_mul(cell_h)).unwrap_or(u32::MAX),
                    |_| height,
                );
                crate::control::ImageLeafFrameMetadata {
                    kind: "terminal",
                    view: leaf.view.get(),
                    session: Some(leaf.session),
                    focused: leaf.focused,
                    width: leaf_width,
                    height: leaf_height,
                    snapshot_seq: Some(leaf.snapshot_seq),
                    instance: None,
                    generation: None,
                    geometry: singular.map(|_| geometry),
                    config_revision: None,
                    update_revision: None,
                    document_seq: None,
                    presentation_revision: None,
                    paint_revision: None,
                    compiled_fingerprint: None,
                    raster_fingerprint: singular.map(|_| 0),
                }
            })
            .collect();
        Ok(crate::control::ImageFrameMetadata {
            frame_kind: if singular.is_some() {
                "terminal"
            } else {
                "composite"
            },
            phase,
            window: front.0,
            view: singular.map(|leaf| leaf.view.get()),
            generation: None,
            config_revision: None,
            update_revision: None,
            document_seq: None,
            presentation_revision: None,
            paint_revision: None,
            capture_serial: capture_serial.unwrap_or(window.capture_present_serial),
            width,
            height,
            pixel_fingerprint: 0,
            compiled_fingerprint: None,
            raster_fingerprint: 0,
            raster_model_fingerprint: snapshot_fingerprint,
            raster_geometry: geometry,
            overlay_fingerprint: recorded.map_or_else(
                || window.overlay_fp(),
                |recorded| recorded.overlay_fingerprint,
            ),
            theme_fingerprint: recorded.map_or_else(
                || self.image_theme_fingerprint(),
                |recorded| recorded.theme_fingerprint,
            ),
            leaves,
        })
    }

    fn native_image_metadata_from_plan(
        &self,
        front: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        facts: ImageFrameFacts,
    ) -> Result<crate::control::ImageFrameMetadata, String> {
        let ImageFrameFacts {
            phase,
            capture_serial,
            width,
            height,
        } = facts;

        let window = self
            .windows
            .get(&front)
            .ok_or_else(|| "native image metadata lost its window".to_string())?;
        let card = window
            .route_card
            .as_ref()
            .or(window.settings_card.as_ref())
            .ok_or_else(|| "native image metadata has no retained composite raster".to_string())?;
        let (cw, ch) = self.win_cell_size(front);
        let mut leaves = Vec::with_capacity(plan.leaves.len());
        for leaf in &plan.leaves {
            let retained = window.leaf_render_cache.get(&leaf.view).ok_or_else(|| {
                format!(
                    "native image metadata is missing retained view {}",
                    leaf.view
                )
            })?;
            match self.view_store.get(leaf.view).copied() {
                Some(crate::tab_model::View::Terminal(terminal)) => {
                    leaves.push(crate::control::ImageLeafFrameMetadata {
                        kind: "terminal",
                        view: leaf.view.get(),
                        session: Some(terminal.session),
                        focused: leaf.focused,
                        width: (leaf.rect.size.width * cw as f32).round().max(1.0) as u32,
                        height: (leaf.rect.size.height * ch as f32).round().max(1.0) as u32,
                        snapshot_seq: Some(retained.input.snapshot_seq),
                        instance: None,
                        generation: None,
                        geometry: None,
                        config_revision: None,
                        update_revision: None,
                        document_seq: None,
                        presentation_revision: None,
                        paint_revision: None,
                        compiled_fingerprint: None,
                        raster_fingerprint: None,
                    });
                }
                Some(crate::tab_model::View::Native(_)) => {
                    let raster = retained.native.as_ref().ok_or_else(|| {
                        format!(
                            "native image metadata is missing retained native raster {}",
                            leaf.view
                        )
                    })?;
                    let stamp = raster.stamp;
                    leaves.push(crate::control::ImageLeafFrameMetadata {
                        kind: "native",
                        view: leaf.view.get(),
                        session: None,
                        focused: leaf.focused,
                        width: raster.width,
                        height: raster.height,
                        snapshot_seq: None,
                        instance: Some(stamp.instance.get()),
                        generation: Some(stamp.generation),
                        geometry: Some(stamp.geometry),
                        config_revision: Some(stamp.config_revision),
                        update_revision: Some(stamp.update_revision),
                        document_seq: stamp.document_seq,
                        presentation_revision: Some(stamp.presentation_revision),
                        paint_revision: Some(stamp.paint_revision),
                        compiled_fingerprint: Some(raster.compiled.fingerprint()),
                        raster_fingerprint: Some(raster.raster_fingerprint()),
                    });
                }
                None => {
                    return Err(format!(
                        "native image metadata found stale view {}",
                        leaf.view
                    ));
                }
            }
        }
        if !leaves.iter().any(|leaf| leaf.kind == "native") {
            return Err("native image metadata composite contains no native leaf".to_string());
        }

        let frame_kind = if leaves.len() == 1 && leaves[0].kind == "native" {
            "native"
        } else {
            "composite"
        };
        let primary = (frame_kind == "native").then(|| &leaves[0]);
        Ok(crate::control::ImageFrameMetadata {
            frame_kind,
            phase,
            window: front.0,
            view: primary.map(|leaf| leaf.view),
            generation: primary.and_then(|leaf| leaf.generation),
            config_revision: primary.and_then(|leaf| leaf.config_revision),
            update_revision: primary.and_then(|leaf| leaf.update_revision),
            document_seq: primary.and_then(|leaf| leaf.document_seq),
            presentation_revision: primary.and_then(|leaf| leaf.presentation_revision),
            paint_revision: primary.and_then(|leaf| leaf.paint_revision),
            capture_serial: capture_serial.unwrap_or(window.capture_present_serial),
            width,
            height,
            pixel_fingerprint: 0,
            compiled_fingerprint: primary.and_then(|leaf| leaf.compiled_fingerprint),
            raster_fingerprint: card.raster_fingerprint(),
            raster_model_fingerprint: card.fp,
            raster_geometry: card.geom,
            overlay_fingerprint: window.overlay_fp(),
            theme_fingerprint: self.image_theme_fingerprint(),
            leaves,
        })
    }

    /// `theme-fingerprint`.
    pub(crate) fn image_theme_fingerprint(&self) -> u64 {
        let mut hash = aterm_hash::Fnv1a64::new();
        hash.write(b"theme-fingerprint-v2");
        for chrome in [
            self.theme.fg,
            self.theme.bg,
            self.theme.cursor,
            self.theme.selection,
        ] {
            hash.write(&chrome.to_le_bytes());
        }
        let colors = &self.installed_colors;
        for rgb in &colors.scheme.ansi {
            hash.write(&[rgb.r, rgb.g, rgb.b]);
        }
        hash.write(&(colors.extended_palette.len() as u64).to_le_bytes());
        for (index, rgb) in &colors.extended_palette {
            hash.write(&[*index, rgb.r, rgb.g, rgb.b]);
        }
        match colors.selection_fg() {
            Some(rgb) => hash.write(&[1, rgb.r, rgb.g, rgb.b]),
            None => hash.write(&[0]),
        }
        hash.write(&[u8::from(colors.bold_is_bright)]);
        hash.finish() | 1
    }

    fn hash_terminal_render_model<H: std::hash::Hasher>(
        input: &aterm_render::RenderInput,
        hash: &mut H,
    ) {
        use std::hash::Hash;

        "terminal-render-model-v8".hash(hash);
        input.rows.hash(hash);
        input.cols.hash(hash);
        for row in &input.cells {
            row.len().hash(hash);
            for cell in row {
                cell.ch.hash(hash);
                cell.fg.hash(hash);
                cell.bg.hash(hash);
                cell.wide.hash(hash);
                cell.emoji_presentation.hash(hash);
                cell.text_presentation.hash(hash);
                cell.bold.hash(hash);
                cell.italic.hash(hash);
                std::mem::discriminant(&cell.underline).hash(hash);
                cell.strikethrough.hash(hash);
                cell.overline.hash(hash);
                cell.underline_color.hash(hash);
                cell.overline_color.hash(hash);
            }
        }
        input.clusters.hash(hash);
        input.combining.hash(hash);
        for (row, map) in input.visual_to_logical.iter().enumerate() {
            if !map.is_empty() {
                row.hash(hash);
                map.hash(hash);
            }
        }
        usize::MAX.hash(hash);

        let mut seen_images = std::collections::HashSet::new();
        for row in &input.images {
            row.len().hash(hash);
            for (col, image_ref) in row {
                col.hash(hash);
                image_ref.cell_row.hash(hash);
                image_ref.cell_col.hash(hash);
                let identity = std::sync::Arc::as_ptr(&image_ref.image) as usize;
                let first = seen_images.insert(identity);
                first.hash(hash);
                if first {
                    let image = image_ref.image.as_ref();
                    std::mem::discriminant(&image.format).hash(hash);
                    if let aterm_core::grid::extra::ImageFormat::RawRgba8 { width, height } =
                        image.format
                    {
                        width.hash(hash);
                        height.hash(hash);
                    }
                    image.cols.hash(hash);
                    image.rows.hash(hash);
                    image.z_index.hash(hash);
                    image.scaling.hash(hash);
                    image.source_rect.hash(hash);
                    image.bytes.hash(hash);
                }
            }
        }
        for line_size in &input.line_sizes {
            std::mem::discriminant(line_size).hash(hash);
        }
        for row in &input.line_size_spans {
            row.len().hash(hash);
            for span in row {
                span.start_col.hash(hash);
                span.end_col.hash(hash);
                std::mem::discriminant(&span.line_size).hash(hash);
            }
        }
        for row in &input.default_bg_spans {
            row.len().hash(hash);
            for span in row {
                span.start_col.hash(hash);
                span.end_col.hash(hash);
                span.default_bg.hash(hash);
            }
        }

        input.display_offset.hash(hash);
        input.base_y.hash(hash);
        input.absolute_row_revision.hash(hash);
        input.history_renumber_epoch.hash(hash);
        input.holder.hash(hash);
        input.cursor_row.hash(hash);
        input.cursor_col.hash(hash);
        input.cursor_visible.hash(hash);
        std::mem::discriminant(&input.cursor_style).hash(hash);
        input.default_bg.hash(hash);
        input.cursor_color.hash(hash);
        input.selection_clip.hash(hash);
        input.selection_bg.hash(hash);
        input.selection_fg.hash(hash);
        input.selection.has_selection().hash(hash);
        std::mem::discriminant(&input.selection.state()).hash(hash);
        std::mem::discriminant(&input.selection.selection_type()).hash(hash);
        let last_col = u16::try_from(input.cols.saturating_sub(1)).unwrap_or(u16::MAX);
        if let Some(selection) = input.selection.project_range(last_col) {
            true.hash(hash);
            selection.start_row.hash(hash);
            selection.start_col.hash(hash);
            selection.end_row.hash(hash);
            selection.end_col.hash(hash);
            selection.is_block.hash(hash);
        } else {
            false.hash(hash);
        }
        input.selections.len().hash(hash);
        for pane in &input.selections {
            pane.clip.hash(hash);
            pane.bg.hash(hash);
            pane.fg.hash(hash);
            pane.inactive.hash(hash);
            pane.selection.has_selection().hash(hash);
            std::mem::discriminant(&pane.selection.state()).hash(hash);
            std::mem::discriminant(&pane.selection.selection_type()).hash(hash);
            if let Some(projected) = pane.selection.project_range(last_col) {
                true.hash(hash);
                projected.start_row.hash(hash);
                projected.start_col.hash(hash);
                projected.end_row.hash(hash);
                projected.end_col.hash(hash);
                projected.is_block.hash(hash);
            } else {
                false.hash(hash);
            }
        }
    }

    fn terminal_snapshot_fingerprint(
        view: u64,
        session: u64,
        input: &aterm_render::RenderInput,
    ) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut snapshot = std::collections::hash_map::DefaultHasher::new();
        view.hash(&mut snapshot);
        session.hash(&mut snapshot);
        Self::hash_terminal_render_model(input, &mut snapshot);
        snapshot.finish() | 1
    }

    fn terminal_capture_fingerprint(
        capture: &crate::app_render::TerminalCaptureGrid,
        input: &aterm_render::RenderInput,
    ) -> u64 {
        if !capture.composed
            && let [leaf] = capture.leaves.as_slice()
        {
            return Self::terminal_snapshot_fingerprint(leaf.view.get(), leaf.session, input);
        }

        use std::hash::{Hash, Hasher};

        let mut snapshot = std::collections::hash_map::DefaultHasher::new();
        "terminal-composite-v1".hash(&mut snapshot);
        Self::hash_terminal_render_model(input, &mut snapshot);
        for leaf in &capture.leaves {
            leaf.view.get().hash(&mut snapshot);
            leaf.session.hash(&mut snapshot);
            leaf.focused.hash(&mut snapshot);
            leaf.rows.hash(&mut snapshot);
            leaf.cols.hash(&mut snapshot);
            leaf.snapshot_seq.hash(&mut snapshot);
        }
        snapshot.finish() | 1
    }

    fn terminal_geometry_fingerprint(
        width: usize,
        height: usize,
        input: &aterm_render::RenderInput,
    ) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut geometry = std::collections::hash_map::DefaultHasher::new();
        width.hash(&mut geometry);
        height.hash(&mut geometry);
        input.rows.hash(&mut geometry);
        input.cols.hash(&mut geometry);
        input.grid_top_row.hash(&mut geometry);
        input.grid_bot_row.hash(&mut geometry);
        geometry.finish() | 1
    }

    #[cfg(test)]
    fn image_pixel_fingerprint<T: std::hash::Hash>(
        width: usize,
        height: usize,
        pixels: &[T],
    ) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hash = std::collections::hash_map::DefaultHasher::new();
        width.hash(&mut hash);
        height.hash(&mut hash);
        pixels.hash(&mut hash);
        hash.finish() | 1
    }

    /// The `panes` control verb's payload.
    pub(crate) fn read_pane_layout(&self, session: Option<u64>) -> Vec<String> {
        let win = match session {
            Some(id) => self.windows_displaying(id).next(),
            None => self.frontmost_window,
        };
        let Some(wid) = win else {
            return Vec::new();
        };
        let Some(ws) = self.windows.get(&wid) else {
            return Vec::new();
        };
        let active = ws.tab_set.active_index().unwrap_or(0);
        let Some(tree) = self.active_tree(wid) else {
            return vec![format!(
                "layout tab={active} panes=0 zoomed=false terminal=false"
            )];
        };
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return Vec::new();
        };
        let Some(focus_index) = plan.leaves.iter().position(|leaf| leaf.focused) else {
            return Vec::new();
        };
        let mut panes = Vec::with_capacity(plan.leaves.len());
        for (index, leaf) in plan.leaves.iter().enumerate() {
            let Some(sid) = self
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                return Vec::new();
            };
            let (rows, cols) = self.pane_target_grid(sid, Self::leaf_grid_cells(&leaf.rect));
            panes.push((
                sid,
                leaf.rect.origin.y.round() as u16,
                leaf.rect.origin.x.round() as u16,
                rows,
                cols,
                index == focus_index,
            ));
        }
        let mut lines = Vec::with_capacity(panes.len() + 1);
        lines.push(format!(
            "layout tab={} panes={} zoomed={} terminal=true",
            active,
            panes.len(),
            tree.is_zoomed(),
        ));
        for (sid, row_off, col_off, rows, cols, focused) in panes {
            lines.push(format!(
                "pane session={sid} rect={row_off},{col_off},{rows}x{cols} focused={focused}"
            ));
        }
        lines
    }

    /// One planned leaf's grid in cells.
    pub(crate) fn leaf_grid_cells(rect: &crate::tab_model::LogicalRect) -> (u16, u16) {
        (
            (rect.size.height.round() as u16).max(1),
            (rect.size.width.round() as u16).max(1),
        )
    }

    /// The grid `resize_panes` gives `session`'s engine for a leaf planned at `planned` cells.
    pub(crate) fn pane_target_grid(&self, session: u64, planned: (u16, u16)) -> (u16, u16) {
        if self.pool.views(session).is_some_and(|views| views > 1) {
            self.shared_target_geometry(session)
        } else {
            planned
        }
    }

    /// Capture the client frame of the window showing `req.session` (else the front window) for the control socket's `image` verb: the presented destination when one is held, else a rerender through the window's renderer.
    pub(crate) fn render_image(&mut self, req: ImageReq) {
        self.run_capture(CaptureRequest::Image(req));
    }

    fn image_body(&mut self, win: Option<WindowId>, req: ImageReq) {
        let Some(front) = win else {
            let _ = req.reply.send(Err(if req.session.is_some() {
                "image: no window shows that session; raise its tab (or end the pane zoom in it) first"
                    .to_string()
            } else {
                "image: no window to capture".to_string()
            }));
            return;
        };
        let policy = SourcePolicy {
            destination: DestinationDemand::of_image(req.clean),
            frame_only: true,
            model: req.want_metadata,
            metadata: req.want_metadata,
        };
        let capture = match self.resolve_capture_source(front, policy) {
            Ok(capture) => capture,
            Err(CaptureDeferral::PendingGpu) => {
                self.queue_gpu_capture(front, CaptureRequest::Image(req), true);
                return;
            }
            Err(CaptureDeferral::Nothing) => {
                let _ = req
                    .reply
                    .send(Ok(crate::control::Retained::plain((0, 0, None))));
                return;
            }
            Err(CaptureDeferral::Held { retry_at }) => {
                let retry_ms = retry_at
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .max(1);
                let _ = req.reply.send(Err(format!(
                    "image deferred: synchronized terminal update in progress; retry after {retry_ms} ms"
                )));
                return;
            }
            Err(CaptureDeferral::Refused(error)) => {
                let _ = req.reply.send(Err(error));
                return;
            }
        };
        self.image_tail(front, capture, req);
    }

    fn image_tail(&mut self, front: WindowId, capture: ResolvedCapture, req: ImageReq) {
        let ImageReq {
            handoff,
            target,
            clean,
            session: _,
            want_bytes,
            want_metadata,
            frame_metadata,
            cancel,
            reply,
        } = req;
        let ResolvedCapture {
            source,
            model,
            context,
        } = capture;
        let capture_serial = source.serial();
        let phase = source.phase();
        let (pixels, model) = match source {
            CaptureSource::Destination { pixels, .. } => (pixels, model),
            CaptureSource::Rerender { visuals, .. } => {
                let mut input = model.unwrap_or_else(|| self.take_rerender_input(front));
                if clean {
                    input.clear_overlays();
                }
                let frame = self.rerender_capture(front, &mut input, visuals);
                match frame.and_then(CapturePixels::packed) {
                    Ok(pixels) => (pixels, Some(input)),
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return;
                    }
                }
            }
        };
        // The cursor scene card this frame painted, for `settings card`: at its native page's
        // place in the window, as the frame composites it (the page's raster at its leaf's
        // cells inside the window's settings card, as
        // `App::retained_native_leaf_artifact_from_plan` places it).
        let (cw, ch) = self.win_cell_size(front);
        let card_at = self
            .windows
            .get(&front)
            .and_then(|window| window.settings_card.as_ref())
            .map_or((0, 0), |card| {
                (
                    i32::try_from(card.dx).unwrap_or(0),
                    i32::try_from(card.dy).unwrap_or(0),
                )
            });
        let native_origin = context.plan.leaves.iter().find_map(|leaf| {
            matches!(
                self.view_store.get(leaf.view).copied(),
                Some(crate::tab_model::View::Native(_))
            )
            .then(|| {
                (
                    card_at.0 + (leaf.rect.origin.x * cw as f32).round().max(0.0) as i32,
                    card_at.1 + (leaf.rect.origin.y * ch as f32).round().max(0.0) as i32,
                )
            })
        });
        crate::settings_preview::stash_captured_scene_card(native_origin);
        let metadata = if want_metadata {
            let (width, height) = pixels.dims();
            let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
                let _ = reply.send(Err(
                    "image metadata dimensions exceed the wire format".to_string()
                ));
                return;
            };
            let facts = ImageFrameFacts {
                phase,
                capture_serial,
                width,
                height,
            };
            let model = model
                .as_ref()
                .unwrap_or(&self.windows[&front].input_scratch);
            match self.image_metadata(front, &context, model, facts, frame_metadata) {
                Ok(metadata) => Some(Box::new(metadata)),
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            }
        } else {
            None
        };
        self.submit_encode_job(EncodeJob::Image {
            pixels,
            metadata,
            handoff,
            target,
            want_bytes,
            cancel,
            reply,
        });
    }

    /// THE FRONT WINDOW'S TAB LINE WHERE NO NATIVE STRIP DRAWS IT (a macOS `--headless` instance builds no toolbar).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn stripless_tab_chrome(&self) -> Option<String> {
        let wid = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied())?;
        let ws = self.windows.get(&wid)?;
        let titles: Vec<String> = ws
            .tab_set
            .shown()
            .iter()
            .map(|tab| {
                ws.tab_chrome_titles_by_tab.get(&tab.id).map_or_else(
                    || {
                        if tab.presentation.title.is_empty() {
                            "aterm".to_string()
                        } else {
                            tab.presentation.title.clone()
                        }
                    },
                    |(_, title)| title.clone(),
                )
            })
            .collect();
        let line = crate::toolbar::format_tab_chrome(
            &titles,
            &self.tab_strip_metadata(wid),
            &self.tab_tooltips(wid),
            ws.tab_set.strip_active().unwrap_or(0),
        )?;
        #[cfg(any(test, debug_assertions))]
        let line = {
            let pushed: Vec<Option<&str>> =
                ws.tooltips_pushed.iter().map(Option::as_deref).collect();
            format!("{line} pushed_tooltips={pushed:?}")
        };
        Some(line)
    }

    /// Read the frontmost window's native macOS chrome.
    #[cfg(target_os = "macos")]
    pub(crate) fn read_native_chrome(&self) -> Vec<String> {
        use aterm_objc::{autoreleasepool, sel};

        use crate::appkit::consts::{
            NS_TOOLBAR_DISPLAY_MODE_DEFAULT, NS_TOOLBAR_DISPLAY_MODE_ICON_AND_LABEL,
            NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY, NS_TOOLBAR_DISPLAY_MODE_LABEL_ONLY,
            NS_WINDOW_TOOLBAR_STYLE_AUTOMATIC, NS_WINDOW_TOOLBAR_STYLE_EXPANDED,
            NS_WINDOW_TOOLBAR_STYLE_PREFERENCE, NS_WINDOW_TOOLBAR_STYLE_UNIFIED,
            NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT,
        };
        use crate::appkit::{self, MainThread};

        let mut out: Vec<String> = Vec::new();

        let Some(mtm) = MainThread::new() else {
            out.push("ERR not on main thread".to_string());
            return out;
        };

        autoreleasepool(|_| {
            let ns_window = self
                .front()
                .and_then(|ws| ws.os_window.as_ref())
                .map(|w| crate::platform::ns_view_of(w))
                .filter(|view| !view.is_null())
                .and_then(|view| {
                    // SAFETY: `view` is the front window's live `NSView` borrowed on the main thread.
                    let window = unsafe { appkit::send_id(view, sel!(window)) };
                    (!window.is_null()).then_some(window)
                });

            if let (Some(window), Some(handle)) = (
                ns_window,
                self.frontmost_window
                    .and_then(|wid| self._toolbars.get(&wid)),
            ) {
                out.extend(macos_tab_frame_lines(window, handle));
            }
            // SAFETY: side-effect-free AppKit toolbar accessors on live main-thread receivers; each answer is +0, borrowed for the pool above.
            unsafe {
                let toolbar = ns_window
                    .map(|w| appkit::send_id(w, sel!(toolbar)))
                    .filter(|t| !t.is_null());
                match toolbar {
                    Some(toolbar) => {
                        let style =
                            match ns_window.map(|w| appkit::send_isize(w, sel!(toolbarStyle))) {
                                Some(NS_WINDOW_TOOLBAR_STYLE_AUTOMATIC) => "automatic",
                                Some(NS_WINDOW_TOOLBAR_STYLE_EXPANDED) => "expanded",
                                Some(NS_WINDOW_TOOLBAR_STYLE_PREFERENCE) => "preference",
                                Some(NS_WINDOW_TOOLBAR_STYLE_UNIFIED) => "unified",
                                Some(NS_WINDOW_TOOLBAR_STYLE_UNIFIED_COMPACT) => "unified-compact",
                                _ => "?",
                            };
                        let display_mode = match appkit::send_usize(toolbar, sel!(displayMode)) {
                            NS_TOOLBAR_DISPLAY_MODE_ICON_ONLY => "icon-only",
                            NS_TOOLBAR_DISPLAY_MODE_LABEL_ONLY => "label-only",
                            NS_TOOLBAR_DISPLAY_MODE_ICON_AND_LABEL => "icon-and-label",
                            NS_TOOLBAR_DISPLAY_MODE_DEFAULT => "default",
                            _ => "default",
                        };
                        let items = appkit::send_id(toolbar, sel!(items));
                        let count = if items.is_null() {
                            0
                        } else {
                            appkit::send_usize(items, sel!(count))
                        };
                        out.push(format!(
                            "toolbar style={style} displayMode={display_mode} items={count}"
                        ));
                        for i in 0..count {
                            let item = appkit::send_id_usize(items, sel!(objectAtIndex:), i);
                            if item.is_null() {
                                continue;
                            }
                            let id = appkit::nsstring_to_rust(appkit::send_id(
                                item,
                                sel!(itemIdentifier),
                            ));
                            let label =
                                appkit::nsstring_to_rust(appkit::send_id(item, sel!(label)));
                            out.push(format!("toolbar-item id={id} label={label:?}"));
                        }
                    }
                    None => out.push("toolbar (none)".to_string()),
                }
            }

            if let Some(handle) = self.frontmost_window.and_then(|w| self._toolbars.get(&w)) {
                if let Some(line) = self.apprt.read_toolbar_chrome(handle) {
                    out.push(line);
                }
                out.extend(self.apprt.read_toolbar_tab_menus(handle));
            } else if let Some(line) = self.stripless_tab_chrome() {
                out.push(line);
            }
            out.extend(self.workers_chrome_line());

            out.extend((self.app_menu_bar)(mtm));
        });

        out
    }

    /// Off macOS there is no native window toolbar.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn read_native_chrome(&self) -> Vec<String> {
        #[cfg(windows)]
        let out: Vec<String> = crate::platform_win::read_chrome_lines(
            self.front().and_then(|ws| ws.os_window.as_deref()),
            self.window_theme,
            self.render_knobs.background_material,
        );
        #[cfg(not(windows))]
        let out: Vec<String> = Vec::new();
        let toolbar_tabs = self
            .frontmost_window
            .and_then(|wid| self._toolbars.get(&wid))
            .and_then(|handle| self.apprt.read_toolbar_chrome(handle));
        let tab_menus = self
            .frontmost_window
            .and_then(|w| self._toolbars.get(&w))
            .map_or_else(Vec::new, |handle| self.apprt.read_toolbar_tab_menus(handle));
        let live = self.palette_live();
        let apply_row = crate::palette::apply_row_label(&live);
        let strip_version = crate::tab_bar::STRIP_VERSION_SLOT.then(|| {
            let (segments, hovered) = self
                .front()
                .filter(|_| self.tab_strip_rows > 0)
                .map_or((&[][..], false), |ws| {
                    (ws.tab_segments.as_slice(), ws.strip_hover_version)
                });
            #[cfg(any(windows, target_os = "linux"))]
            let laid_slot = self
                .front()
                .and_then(|ws| ws.version_slot_memo)
                .map_or(crate::tab_bar::VersionSlotCols::NONE, |(_, slot)| slot);
            #[cfg(not(any(windows, target_os = "linux")))]
            let laid_slot = crate::tab_bar::VersionSlotCols::NONE;
            let absent = if self.tab_strip_rows == 0 {
                StripVersionAbsence::NoStrip
            } else if segments.is_empty() {
                StripVersionAbsence::NotLaidOut
            } else if laid_slot == crate::tab_bar::VersionSlotCols::NONE {
                StripVersionAbsence::NoUiFace
            } else {
                StripVersionAbsence::StripFull
            };
            strip_version_chrome_line(segments, hovered, absent, self.strip_version_attention())
        });
        let strip_caption = cfg!(target_os = "linux").then(|| {
            let (segments, frame, hovered) = self.front().map_or((&[][..], None, None), |ws| {
                let segments = if self.tab_strip_rows > 0 {
                    ws.tab_segments.as_slice()
                } else {
                    &[][..]
                };
                (segments, ws.client_frame, ws.strip_hover_caption)
            });
            strip_caption_chrome_line(segments, frame, hovered)
        });
        let ime = self
            .front()
            .map(|ws| ime_chrome_line(&ws.preedit, ws.preedit_caret, ws.last_ime_rect));
        non_macos_chrome_output(
            out,
            toolbar_tabs,
            strip_version,
            strip_caption,
            ime,
            tab_menus,
            crate::menu::menu_chrome_lines(apply_row.as_deref(), live.front_tab_split),
        )
    }

    /// The `window` verb.
    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn capture_window(
        &mut self,
        handoff: crate::control::ReplyRetentionPermit,
        path: control_auth::ConfinedImage,
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        self.capture_aux_window(AuxTarget::Front, handoff, path, cancel, reply);
    }

    #[cfg(any(target_os = "macos", windows))]
    fn window_body(&mut self, wid: Option<WindowId>, request: WindowReq) {
        let Some(wid) = wid else {
            let _ = request
                .reply
                .send(Err("no window to capture (headless)".to_string()));
            return;
        };
        if self.window_shows_private(wid) {
            let _ = request
                .reply
                .send(Err(crate::app_control::PRIVATE_VIEW_REFUSED.to_owned()));
            return;
        }
        if let Err(error) = self.client_capture_guards(wid) {
            let _ = request.reply.send(Err(error.to_string()));
            return;
        }
        let (serial, client) = match self.present_before_capture(wid, None, PresentTake::ClientTap)
        {
            Ok(Some(PresentedCapture {
                serial,
                client: Some(client),
                ..
            })) => (serial, client),
            Ok(_) => {
                let _ = request
                    .reply
                    .send(Err("no window to capture (headless)".to_string()));
                return;
            }
            Err(error) => {
                if error == GPU_CAPTURE_PENDING {
                    self.queue_gpu_capture(wid, CaptureRequest::Window(request), true);
                } else {
                    let _ = request.reply.send(Err(error));
                }
                return;
            }
        };
        let WindowReq {
            handoff,
            path,
            cancel,
            reply,
            ..
        } = request;
        let captured = self
            .photograph(wid)
            .and_then(|shot| self.window_stitch(wid, serial, client, shot));
        match captured {
            Ok(stitch) => self.submit_encode_job(EncodeJob::Window {
                stitch,
                handoff,
                target: path,
                cancel,
                reply,
            }),
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        }
    }

    #[cfg(any(target_os = "macos", windows))]
    fn window_stitch(
        &self,
        wid: WindowId,
        serial: u64,
        client: CapturePixels,
        shot: PlatformShot,
    ) -> Result<WindowStitch, String> {
        let state = self
            .windows
            .get(&wid)
            .ok_or_else(|| "window capture lost its logical window".to_string())?;
        if state.capture_present_serial != serial {
            return Err("window capture client serial is no longer current".to_string());
        }
        let rect = self.window_client_rect_of(wid, &client)?;
        Ok(WindowStitch { shot, client, rect })
    }

    fn client_capture_guards(&self, wid: WindowId) -> Result<(), &'static str> {
        crate::platform_capture_guards(
            crate::HOST_CAPTURE,
            &self.render_knobs,
            self.backend.is_gpu(),
            self.presents_to_gpu_surface(wid),
        )
    }

    fn client_is_translucent(&self, wid: WindowId) -> bool {
        crate::translucent_gpu_client(
            &self.render_knobs,
            self.backend.is_gpu(),
            self.presents_to_gpu_surface(wid),
        )
    }

    fn presents_to_gpu_surface(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .is_some_and(|window| matches!(window.present, Some(crate::PresentTarget::Gpu { .. })))
    }

    fn client_tap_is_copyable(&self, wid: WindowId) -> bool {
        match self
            .windows
            .get(&wid)
            .and_then(|window| window.present.as_ref())
        {
            Some(crate::PresentTarget::Cpu { .. }) => true,
            Some(crate::PresentTarget::Gpu { gpu_surface, .. }) => {
                gpu_surface.snapshot_is_copyable()
            }
            Some(crate::PresentTarget::Virtual { .. }) | None => false,
        }
    }

    fn presented_frame_of(
        &self,
        wid: WindowId,
        client: CapturePixels,
    ) -> Result<CapturePixels, String> {
        let (frame_w, frame_h) = self
            .windows
            .get(&wid)
            .and_then(|window| window.capture_present_frame)
            .ok_or_else(|| "capture: the present recorded no frame size".to_string())?;
        let (width, height) = client.dims();
        let visible =
            aterm_render::frame_placement(width, height, frame_w as usize, frame_h as usize)
                .visible_rect(width, height)
                .ok_or_else(|| {
                    "capture: the destination is not the surface of its present".to_string()
                })?;
        client.cut_to(visible)
    }

    fn arm_presented_client_capture(&mut self, wid: WindowId) -> Result<(), String> {
        let App {
            backend, windows, ..
        } = self;
        let window = windows
            .get_mut(&wid)
            .ok_or_else(|| "window capture lost its window".to_string())?;
        window.capture_present_client = None;
        match window.present.as_mut() {
            Some(crate::PresentTarget::Cpu { .. }) => {
                window.capture_client_requested = true;
                Ok(())
            }
            Some(crate::PresentTarget::Gpu {
                gpu_surface,
                window_gpu,
            }) => backend
                .gpu_mut()
                .ok_or_else(|| "window capture GPU target/backend mismatch".to_string())?
                .presented_snapshot_begin(window_gpu, gpu_surface),
            Some(crate::PresentTarget::Virtual { .. }) | None => {
                Err("window capture has no window application-present target".to_string())
            }
        }
    }

    fn take_presented_client_capture(&mut self, wid: WindowId) -> Result<CapturePixels, String> {
        let App {
            backend, windows, ..
        } = self;
        let window = windows
            .get_mut(&wid)
            .ok_or_else(|| "window capture lost its window".to_string())?;
        window.capture_acquire_armed = false;
        match window.present.as_mut() {
            Some(crate::PresentTarget::Cpu { .. }) => {
                window.capture_client_requested = false;
                window.capture_present_client.take().ok_or_else(|| {
                    "window capture CPU present produced no exact client frame".to_string()
                })
            }
            Some(crate::PresentTarget::Gpu { window_gpu, .. }) => {
                let gpu = backend
                    .gpu_mut()
                    .ok_or_else(|| "window capture GPU target/backend mismatch".to_string())?;
                gpu.presented_snapshot_after_present(window_gpu, crate::metrics::now_us())?;
                gpu.presented_snapshot_finish(window_gpu)?;
                gpu.presented_snapshot_take(window_gpu)
                    .map(CapturePixels::raw)
            }
            Some(crate::PresentTarget::Virtual { .. }) | None => {
                Err("window capture lost its window application-present target".to_string())
            }
        }
    }

    fn discard_presented_client_capture(&mut self, wid: WindowId) {
        let App {
            backend, windows, ..
        } = self;
        let Some(window) = windows.get_mut(&wid) else {
            return;
        };
        window.capture_acquire_armed = false;
        window.capture_client_requested = false;
        window.capture_present_client = None;
        if let Some(crate::PresentTarget::Gpu { window_gpu, .. }) = window.present.as_mut()
            && let Some(gpu) = backend.gpu_mut()
        {
            let _ = gpu.presented_snapshot_take(window_gpu);
        }
    }

    #[cfg(target_os = "macos")]
    fn window_rgba_of(&self, wid: WindowId) -> Result<(Vec<u8>, u32, u32), String> {
        use aterm_objc::sel;

        #[cfg(test)]
        OS_WINDOW_PHOTOGRAPHS.with(|n| n.set(n.get() + 1));

        use crate::appkit;

        let Some(os_window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) else {
            return Err("no window to capture (headless)".to_string());
        };
        let view = crate::platform::ns_view_of(os_window);
        if view.is_null() {
            return Err("no window to capture (headless)".to_string());
        }
        // SAFETY: `view` is this window's live `NSView` borrowed on the main thread.
        let ns_window = unsafe { appkit::send_id(view, sel!(window)) };
        if ns_window.is_null() {
            return Err("no window to capture (headless)".to_string());
        }
        // SAFETY: side-effect-free `windowNumber` accessor on the live `NSWindow` on the main thread.
        let window_number = unsafe { appkit::send_isize(ns_window, sel!(windowNumber)) };
        if window_number <= 0 {
            return Err(
                "window capture failed (front window has no on-screen window number)".to_string(),
            );
        }

        capture_window_pixels(window_number as u32)
    }

    #[cfg(any(target_os = "macos", windows))]
    fn window_client_rect_of(
        &self,
        wid: WindowId,
        client: &CapturePixels,
    ) -> Result<WindowClientRect, String> {
        let state = self
            .windows
            .get(&wid)
            .ok_or_else(|| "window capture lost its logical window".to_string())?;
        let window = state
            .os_window
            .as_ref()
            .ok_or_else(|| "no window to capture (headless)".to_string())?;
        let inner_size = window.inner_size();
        let (width, height) = client.dims();
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return Err("window capture client is larger than a window".to_string());
        };
        if (inner_size.width, inner_size.height) != (width, height) {
            return Err(format!(
                "window client geometry changed during capture: presented {width}x{height}px, \
                 current {}x{}px",
                inner_size.width, inner_size.height
            ));
        }
        let inner = window
            .inner_position()
            .map_err(|error| format!("window capture could not read client origin: {error}"))?;
        let outer = window
            .outer_position()
            .map_err(|error| format!("window capture could not read frame origin: {error}"))?;
        let x = i64::from(inner.x) - i64::from(outer.x);
        let y = i64::from(inner.y) - i64::from(outer.y);
        let x = u32::try_from(x)
            .map_err(|_| "window capture reported a negative client x origin".to_string())?;
        let y = u32::try_from(y)
            .map_err(|_| "window capture reported a negative client y origin".to_string())?;
        Ok(WindowClientRect {
            x,
            y,
            width,
            height,
        })
    }

    #[cfg(target_os = "macos")]
    fn native_chrome_overlay_of(
        &self,
        wid: WindowId,
        window_width: u32,
        window_height: u32,
    ) -> Result<Option<ChromeLayer>, String> {
        use aterm_objc::autoreleasepool;

        autoreleasepool(|_| self.native_chrome_overlay_pooled(wid, window_width, window_height))
    }

    #[cfg(target_os = "macos")]
    fn native_chrome_overlay_pooled(
        &self,
        wid: WindowId,
        window_width: u32,
        window_height: u32,
    ) -> Result<Option<ChromeLayer>, String> {
        use aterm_objc::{Id, class, sel};

        use crate::appkit;
        use crate::appkit::consts::{NS_COLOR_RENDERING_INTENT_PERCEPTUAL, NS_WINDOW_CLOSE_BUTTON};

        let state = self
            .windows
            .get(&wid)
            .ok_or_else(|| "window capture lost its logical window".to_string())?;
        if state.metrics.head == 0 {
            return Ok(None);
        }
        let os_window = state
            .os_window
            .as_ref()
            .ok_or_else(|| "no window to capture (headless)".to_string())?;
        let content = crate::platform::ns_view_of(os_window);
        if content.is_null() {
            return Err("window capture could not read the AppKit content view".to_string());
        }
        // SAFETY: live main-thread `NSView` and `NSWindow` button accessors returning borrowed `+0` references.
        let ns_window = unsafe { appkit::send_id(content, sel!(window)) };
        if ns_window.is_null() {
            return Err("window capture lost its AppKit window".to_string());
        }
        // SAFETY: as above.
        let close = unsafe {
            appkit::send_id_usize(
                ns_window,
                sel!(standardWindowButton:),
                NS_WINDOW_CLOSE_BUTTON,
            )
        };
        if close.is_null() {
            return Err("window capture could not locate AppKit titlebar controls".to_string());
        }
        let mut chrome_root = close;
        for _ in 0..32 {
            // SAFETY: side-effect-free `superview` read on a live main-thread `NSView`.
            let parent = unsafe { appkit::send_id(chrome_root, sel!(superview)) };
            if parent.is_null() {
                break;
            }
            if macos_view_is_ancestor_of(parent, content)? {
                break;
            }
            chrome_root = parent;
        }
        if macos_view_is_ancestor_of(chrome_root, content)? {
            return Err(
                "window capture refused an AppKit chrome root containing client pixels".to_string(),
            );
        }

        if let Some(toolbar) = self._toolbars.get(&wid) {
            let strip_owner = crate::toolbar::native_strip_container(toolbar);
            let strip = strip_owner.id();
            if !macos_view_is_ancestor_of(chrome_root, strip)? {
                return Err(
                    "window capture could not isolate the complete AppKit toolbar subtree"
                        .to_string(),
                );
            }
        }

        // SAFETY: `-[NSView bounds]` on a live main-thread `NSView` via `send_rect`.
        let bounds = unsafe { appkit::send_rect(chrome_root, sel!(bounds)) };
        if !macos_rect_is_finite(bounds) || bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            return Err("window capture found invalid AppKit chrome bounds".to_string());
        }
        // SAFETY: `-convertRect:toView:` is `-(NSRect)(NSRect, NSView *)` and `-convertRectToBacking:` `-(NSRect)(NSRect)`, side-effect-free on live main-thread receivers; `Id::NIL` is the documented "window base coordinates" argument.
        let window_rect = unsafe {
            appkit::send_rect_rect_id(chrome_root, sel!(convertRect:toView:), bounds, Id::NIL)
        };
        // SAFETY: as above, on the live NSWindow.
        let backing_rect =
            unsafe { appkit::send_rect_rect(ns_window, sel!(convertRectToBacking:), window_rect) };
        if !macos_rect_is_finite(backing_rect) {
            return Err("window capture found invalid AppKit backing geometry".to_string());
        }

        // SAFETY: documented AppKit view-caching pair on a live main-thread subtree.
        let bitmap = unsafe {
            appkit::send_id_rect(
                chrome_root,
                sel!(bitmapImageRepForCachingDisplayInRect:),
                bounds,
            )
        };
        if bitmap.is_null() {
            return Err("window capture could not allocate an AppKit chrome bitmap".to_string());
        }
        // SAFETY: as above.
        unsafe {
            appkit::send_v_rect_id(
                chrome_root,
                sel!(cacheDisplayInRect:toBitmapImageRep:),
                bounds,
                bitmap,
            );
        }
        // SAFETY: `+[NSColorSpace sRGBColorSpace]` is +0 and process-lifetime; `-bitmapImageRepByConvertingToColorSpace:renderingIntent:` takes the intent as an NSInteger, on a live bitmap rep.
        let srgb = unsafe { appkit::send_id(class(c"NSColorSpace").as_id(), sel!(sRGBColorSpace)) };
        if srgb.is_null() {
            return Err("window capture could not reach the sRGB color space".to_string());
        }
        // SAFETY: as above.
        let bitmap = unsafe {
            appkit::send_id_id_isize(
                bitmap,
                sel!(bitmapImageRepByConvertingToColorSpace:renderingIntent:),
                srgb,
                NS_COLOR_RENDERING_INTENT_PERCEPTUAL,
            )
        };
        if bitmap.is_null() {
            return Err("window capture could not convert AppKit chrome to sRGB".to_string());
        }
        let (pixels, width, height) = chrome_pixels_of(bitmap)?;

        let expected_width = backing_rect.size.width.round();
        let expected_height = backing_rect.size.height.round();
        if expected_width <= 0.0
            || expected_height <= 0.0
            || f64::from(width) != expected_width
            || f64::from(height) != expected_height
        {
            return Err(format!(
                "window capture AppKit chrome bitmap {}x{}px disagrees with backing geometry {:.0}x{:.0}px",
                width, height, expected_width, expected_height
            ));
        }
        let x = backing_rect.origin.x.round();
        let bottom = backing_rect.origin.y.round();
        if x < 0.0 || bottom < 0.0 || x > f64::from(u32::MAX) || bottom > f64::from(u32::MAX) {
            return Err("window capture AppKit chrome origin is outside the window".to_string());
        }
        let x = x as u32;
        let bottom = bottom as u32;
        let y = window_height
            .checked_sub(
                bottom
                    .checked_add(height)
                    .ok_or_else(|| "window capture AppKit chrome origin overflowed".to_string())?,
            )
            .ok_or_else(|| "window capture AppKit chrome lies outside the window".to_string())?;
        if x.checked_add(width)
            .is_none_or(|right| right > window_width)
        {
            return Err("window capture AppKit chrome lies outside the window".to_string());
        }
        Ok(Some(ChromeLayer {
            x,
            y,
            width,
            height,
            pixels,
        }))
    }

    #[cfg(target_os = "macos")]
    fn photograph(&self, wid: WindowId) -> Result<PlatformShot, String> {
        let (pixels, width, height) = self.window_rgba_of(wid)?;
        let chrome = self.native_chrome_overlay_of(wid, width, height)?;
        Ok(PlatformShot {
            pixels,
            width,
            height,
            encoding: ShotEncoding::PremultipliedRgba,
            chrome,
        })
    }

    #[cfg(windows)]
    fn photograph(&self, wid: WindowId) -> Result<PlatformShot, String> {
        let window = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_deref())
            .ok_or_else(|| "no window to capture (headless)".to_string())?;
        let (pixels, width, height) = crate::platform_win::capture_window_bgrx(window)
            .map_err(|error| format!("window capture failed: {error}"))?;
        Ok(PlatformShot {
            pixels,
            width,
            height,
            encoding: ShotEncoding::Bgrx,
        })
    }

    /// Off macOS/Windows aterm has no window photograph (no CoreGraphics or `PrintWindow` path), so the `window` verb says so.
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    pub(crate) fn capture_window(
        &mut self,
        _handoff: crate::control::ReplyRetentionPermit,
        _target: control_auth::ConfinedImage,
        _cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        let _ = reply.send(Err(
            "window capture is only available on macOS and Windows".to_string()
        ));
    }
}

impl App {
    /// The `window [<target>]` verb: photograph the whole front window to the confined PNG once `target` is open in it (`front` always is), replying its `(width, height)`.
    #[cfg(any(target_os = "macos", windows))]
    pub(crate) fn capture_aux_window(
        &mut self,
        target: AuxTarget,
        handoff: crate::control::ReplyRetentionPermit,
        confined: control_auth::ConfinedImage,
        cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        self.run_capture(CaptureRequest::Window(WindowReq {
            target: None,
            aux: target,
            handoff,
            path: confined,
            cancel,
            reply,
        }));
    }

    /// Off macOS/Windows aterm has no window photograph path.
    #[cfg(not(any(target_os = "macos", windows)))]
    pub(crate) fn capture_aux_window(
        &mut self,
        _target: AuxTarget,
        _handoff: crate::control::ReplyRetentionPermit,
        _confined: control_auth::ConfinedImage,
        _cancel: crate::control::CaptureCancellation,
        reply: std::sync::mpsc::Sender<crate::control::WindowReply>,
    ) {
        let _ = reply.send(Err(
            "auxiliary window capture is only available on macOS and Windows".to_string(),
        ));
    }

    fn read_native_settings_controls(
        &self,
        route_alias: Option<(&'static str, crate::native_settings::SettingsRoute)>,
    ) -> Vec<String> {
        let target = route_alias.map_or_else(
            || self.native_settings_view_target(),
            |(_, route)| self.native_settings_route_view_target(route),
        );
        let Some((wid, instance, view, state)) = target else {
            return match route_alias {
                Some((alias, route)) => native_settings_route_not_open_controls_lines(
                    alias,
                    route,
                    self.native_settings_front_view_target()
                        .map(|(_, _, _, state)| state),
                ),
                None => native_settings_closed_controls_lines(),
            };
        };

        if let Some(frame) = self
            .cached_native_ui(wid)
            .filter(|frame| frame.stamp.instance == instance && frame.stamp.view == view)
        {
            return native_settings_controls_lines(state, &frame.compiled);
        }
        let compiled = self
            .native_ui_viewport_for(wid, view)
            .and_then(|viewport| self.compiled_native_ui_for(wid, instance, view, viewport));
        match compiled {
            Ok(compiled) => native_settings_controls_lines(state, &compiled),
            Err(error) => native_settings_unavailable_controls_lines(state, &error),
        }
    }

    fn read_about_controls(&self) -> Vec<String> {
        let Some((wid, instance, view)) = self.front_about_view() else {
            return vec!["state open=false pane=native-tab app=about".into()];
        };
        let compiled = self
            .cached_native_ui(wid)
            .filter(|frame| frame.stamp.instance == instance && frame.stamp.view == view)
            .map(|frame| frame.compiled.clone())
            .map_or_else(
                || {
                    self.native_ui_viewport_for(wid, view).and_then(|viewport| {
                        self.compiled_native_ui_for(wid, instance, view, viewport)
                    })
                },
                Ok,
            );
        let mut lines = vec!["state open=true pane=native-tab app=about".into()];
        match compiled {
            Ok(compiled) => {
                let controls = compiled
                    .semantics
                    .iter()
                    .filter(|node| node.action.is_some())
                    .count();
                lines.push(format!(
                    "native surface=native-tab app=about controls={controls} semantics={}",
                    compiled.semantics.len()
                ));
                lines.extend(compiled.controls_lines());
            }
            Err(error) => lines.push(format!(
                "native surface=native-tab app=about controls=0 semantics=0 error={error:?}"
            )),
        }
        lines
    }

    /// Read an auxiliary own-rendered surface's controls as human-readable text lines.
    pub(crate) fn read_aux_controls(&self, target: AuxTarget) -> Vec<String> {
        match target {
            AuxTarget::Prefs => self.read_native_settings_controls(None),
            AuxTarget::About => self.read_about_controls(),
            AuxTarget::Menu => match self.front().and_then(|ws| ws.palette()) {
                Some(p) => p.controls_lines(),
                None => self
                    .frontmost_window
                    .map_or_else(crate::palette::PaletteState::new, |wid| {
                        self.palette_snapshot(wid)
                    })
                    .controls_lines(),
            },
            AuxTarget::Update => self.read_native_settings_controls(Some((
                "update",
                crate::native_settings::SettingsRoute::SoftwareUpdate,
            ))),
            AuxTarget::TabMenu => match self.front().and_then(|ws| ws.tab_menu.as_ref()) {
                Some(menu) => menu.controls_lines(),
                None => vec!["tab-menu open=false".to_string()],
            },
            AuxTarget::FactMenu => self.fact_menu_controls(),
            AuxTarget::Overview => match self.front().and_then(|ws| ws.overview()) {
                Some(card) => card.controls_lines(),
                None => vec!["overview open=false".to_string()],
            },
            AuxTarget::Front => {
                let mut lines = match self.front().and_then(|ws| ws.overlay()) {
                    Some(o) => vec![o.status_line()],
                    None => vec!["overlay open=false".to_string()],
                };
                lines.extend(self.close_confirm_controls_line());
                lines
            }
        }
    }
}

#[cfg(test)]
#[path = "app_introspect_native_settings_compatibility_controls_tests.rs"]
mod native_settings_compatibility_controls_tests;

/// Each chip's `tab-frame` line: rects in the pixels of a `window` capture, whose origin is the window's top left.
#[cfg(target_os = "macos")]
fn macos_tab_frame_lines(
    window: aterm_objc::Id,
    handle: &crate::toolbar::ToolbarHandle,
) -> Vec<String> {
    use aterm_objc::sel;
    use aterm_objc::{CGPoint, CGRect, CGSize};

    use crate::appkit;
    // SAFETY: `-frame` and `-convertRectToBacking:` are side-effect-free on the live main-thread window.
    let height = unsafe {
        let frame = appkit::send_rect(window, sel!(frame));
        let local = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: frame.size,
        };
        appkit::send_rect_rect(window, sel!(convertRectToBacking:), local)
            .size
            .height
    };
    crate::toolbar::tab_frame_lines(handle, |view, [x, y, w, h]| {
        // SAFETY: `-convertRect:toView:` and `-convertRectToBacking:` are side-effect-free on the live chip and its window; nil is the documented "window base coordinates" argument.
        let b = unsafe {
            let in_window = appkit::send_rect_rect_id(
                view,
                sel!(convertRect:toView:),
                CGRect {
                    origin: CGPoint { x, y },
                    size: CGSize {
                        width: w,
                        height: h,
                    },
                },
                aterm_objc::Id::NIL,
            );
            appkit::send_rect_rect(window, sel!(convertRectToBacking:), in_window)
        };
        [
            b.origin.x,
            height - b.origin.y - b.size.height,
            b.size.width,
            b.size.height,
        ]
    })
}

#[cfg(target_os = "macos")]
fn macos_view_is_ancestor_of(
    ancestor: aterm_objc::Id,
    descendant: aterm_objc::Id,
) -> Result<bool, String> {
    use aterm_objc::sel;

    if ancestor.is_null() || descendant.is_null() {
        return Err("window capture walked a nil AppKit view".to_string());
    }
    if ancestor == descendant {
        return Ok(true);
    }
    // SAFETY: side-effect-free `superview` reads on live main-thread `NSView`s; each +0 answer is only compared, never kept.
    let mut current = unsafe { crate::appkit::send_id(descendant, sel!(superview)) };
    for _ in 0..64 {
        if current.is_null() {
            return Ok(false);
        }
        if ancestor == current {
            return Ok(true);
        }
        // SAFETY: as above.
        current = unsafe { crate::appkit::send_id(current, sel!(superview)) };
    }
    Err("window capture found an invalid cyclic AppKit view hierarchy".to_string())
}

#[cfg(target_os = "macos")]
const fn macos_rect_is_finite(rect: aterm_objc::CGRect) -> bool {
    rect.origin.x.is_finite()
        && rect.origin.y.is_finite()
        && rect.size.width.is_finite()
        && rect.size.height.is_finite()
}

#[cfg(target_os = "macos")]
fn chrome_pixels_of(rep: aterm_objc::Id) -> Result<(ChromePixels, u32, u32), String> {
    use aterm_cg::*;
    use aterm_objc::cf::{CFDataGetBytePtr, CFDataGetLength, CFDataRef, CFRelease};
    use aterm_objc::sel;

    use crate::appkit;

    // SAFETY: `-[NSBitmapImageRep CGImage]` on the live converted rep kept alive by the autorelease pool.
    let image: CGImageRef = unsafe { appkit::send_id(rep, sel!(CGImage)) }
        .as_ptr()
        .cast();
    if image.is_null() {
        return Err("window capture could not read the AppKit chrome bitmap".to_string());
    }
    // SAFETY: side-effect-free accessors on the live `CGImage`; its colour space is +0, owned by the image.
    let desc = unsafe {
        let space = CGImageGetColorSpace(image);
        BitmapDesc {
            width: CGImageGetWidth(image),
            height: CGImageGetHeight(image),
            bytes_per_row: CGImageGetBytesPerRow(image),
            bits_per_component: CGImageGetBitsPerComponent(image),
            bits_per_pixel: CGImageGetBitsPerPixel(image),
            bitmap_info: CGImageGetBitmapInfo(image),
            rgb: !space.is_null() && CGColorSpaceGetModel(space) == kCGColorSpaceModelRGB,
        }
    };
    let (Ok(width), Ok(height)) = (u32::try_from(desc.width), u32::try_from(desc.height)) else {
        return Err("window capture AppKit chrome bitmap is too large".to_string());
    };
    if desc.direct_layout().is_ok() {
        // SAFETY: the data provider is +0, owned by the live image.
        let provider = unsafe { CGImageGetDataProvider(image) };
        if provider.is_null() {
            return Err("window capture could not reach the AppKit chrome bytes".to_string());
        }
        // SAFETY: `CGDataProviderCopyData` returns a `+1` `CFData` (or NULL) released once by the guard.
        let data = unsafe { CGDataProviderCopyData(provider) };
        if data.is_null() {
            return Err("window capture could not copy the AppKit chrome bytes".to_string());
        }
        struct DataGuard(CFDataRef);
        impl Drop for DataGuard {
            fn drop(&mut self) {
                // SAFETY: the CFData copied above, released exactly once.
                unsafe { CFRelease(self.0.cast()) };
            }
        }
        let _data_guard = DataGuard(data);
        // SAFETY: length and byte pointer of the live `CFData` held until the guard drops.
        let bytes = unsafe {
            let len = usize::try_from(CFDataGetLength(data)).unwrap_or(0);
            let ptr = CFDataGetBytePtr(data);
            if len == 0 || ptr.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(ptr, len).to_vec()
            }
        };
        return Ok((ChromePixels::Bitmap { bytes, desc }, width, height));
    }
    Ok((ChromePixels::Png(chrome_png_of(rep)?), width, height))
}

#[cfg(target_os = "macos")]
fn chrome_png_of(rep: aterm_objc::Id) -> Result<Vec<u8>, String> {
    use aterm_objc::{Obj, class, sel};

    use crate::appkit;
    use crate::appkit::consts::NS_BITMAP_IMAGE_FILE_TYPE_PNG;

    // SAFETY: `+[NSDictionary new]` is `+(instancetype)` and +1.
    let Some(properties) =
        (unsafe { Obj::from_owned(appkit::send_id(class(c"NSDictionary").as_id(), sel!(new))) })
    else {
        return Err("window capture could not allocate AppKit chrome properties".to_string());
    };
    // SAFETY: `-representationUsingType:properties:` on the live converted rep and dictionary.
    let png = unsafe {
        appkit::send_id_usize_id(
            rep,
            sel!(representationUsingType:properties:),
            NS_BITMAP_IMAGE_FILE_TYPE_PNG,
            properties.id(),
        )
    };
    if png.is_null() {
        return Err("window capture could not encode the AppKit chrome bitmap".to_string());
    }
    // SAFETY: `-[NSData length]` is `-(NSUInteger)` on the live PNG data.
    let png_len = unsafe { appkit::send_usize(png, sel!(length)) };
    let mut png_bytes = vec![0_u8; png_len];
    if png_len != 0 {
        // SAFETY: `-getBytes:length:` writes `png_len` bytes, the receiver's own length, into the `Vec` sized to it.
        unsafe {
            appkit::send_v_ptr_usize(
                png,
                sel!(getBytes:length:),
                png_bytes.as_mut_ptr().cast(),
                png_len,
            );
        }
    }
    Ok(png_bytes)
}

#[cfg(target_os = "macos")]
fn decode_native_chrome_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    const MAX_CHROME_BYTES: usize = 256 * 1024 * 1024;
    let mut decoder = aterm_png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(aterm_png::Limits {
        bytes: MAX_CHROME_BYTES,
    });
    decoder.set_transformations(
        aterm_png::Transformations::EXPAND | aterm_png::Transformations::STRIP_16,
    );
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("window capture could not decode AppKit chrome: {error}"))?;
    let (width, height) = (reader.info().width, reader.info().height);
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|&bytes| bytes <= MAX_CHROME_BYTES)
        .ok_or_else(|| "window capture AppKit chrome dimensions are invalid".to_string())?;
    if width == 0 || height == 0 {
        return Err("window capture AppKit chrome bitmap is empty".to_string());
    }
    let mut decoded = vec![0_u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut decoded)
        .map_err(|error| format!("window capture could not decode AppKit chrome: {error}"))?;
    if info.width != width
        || info.height != height
        || info.bit_depth != aterm_png::BitDepth::Eight
        || info.color_type != aterm_png::ColorType::Rgba
        || info.buffer_size() != expected
    {
        return Err(format!(
            "window capture AppKit chrome is not straight RGBA8 ({}x{}, {:?} {:?})",
            info.width, info.height, info.color_type, info.bit_depth
        ));
    }
    decoded.truncate(expected);
    Ok((decoded, width, height))
}

/// Capture the on-screen window `window_id` via CoreGraphics into an sRGB RGBA8 buffer.
#[cfg(target_os = "macos")]
pub(crate) fn capture_window_pixels(window_id: u32) -> Result<(Vec<u8>, u32, u32), String> {
    use crate::cg_capture::*;

    // SAFETY: `CGWindowListCreateImage` returns an owned `CGImage` (released below) or NULL.
    let image: CGImageRef = unsafe {
        CGWindowListCreateImage(
            CG_RECT_NULL,
            K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
            window_id,
            K_CG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING | K_CG_WINDOW_IMAGE_BEST_RESOLUTION,
        )
    };
    if image.is_null() {
        return Err(
            "window capture failed (the window is not on screen: minimized, on another \
             Space, not yet painted, or zero-sized; bring it on screen and retry — no \
             Screen Recording grant is needed)"
                .to_string(),
        );
    }

    // SAFETY: `image` is the live `CGImage` created above; width/height accessors are side-effect-free.
    struct ImageGuard(CGImageRef);
    impl Drop for ImageGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the CGImage created above, released exactly once.
            unsafe { CGImageRelease(self.0) };
        }
    }
    let _image_guard = ImageGuard(image);

    let width = unsafe { CGImageGetWidth(image) };
    let height = unsafe { CGImageGetHeight(image) };
    if width == 0 || height == 0 {
        return Err("window capture failed (captured image has zero size)".to_string());
    }

    let bytes_per_row = width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or_else(|| "window capture failed (image too large)".to_string())?;

    let Some(srgb_name) = crate::appkit::nsstring("kCGColorSpaceSRGB") else {
        return Err("window capture failed (could not name the sRGB color space)".to_string());
    };
    // SAFETY: `NSString` is toll-free bridged to `CFStringRef` and outlives `CGColorSpaceCreateWithName`.
    let color_space: CGColorSpaceRef =
        unsafe { CGColorSpaceCreateWithName(srgb_name.id().as_ptr().cast_const()) };
    if color_space.is_null() {
        return Err("window capture failed (could not create sRGB color space)".to_string());
    }
    struct CsGuard(CGColorSpaceRef);
    impl Drop for CsGuard {
        fn drop(&mut self) {
            // SAFETY: the colour space created above, released exactly once.
            unsafe { CGColorSpaceRelease(self.0) };
        }
    }
    let _cs_guard = CsGuard(color_space);

    let context: CGContextRef = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width,
            height,
            BITS_PER_COMPONENT,
            bytes_per_row,
            color_space,
            K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST,
        )
    };
    if context.is_null() {
        return Err("window capture failed (could not create bitmap context)".to_string());
    }
    struct CtxGuard(CGContextRef);
    impl Drop for CtxGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the `CGContext` created above, released once after copying its bytes.
            unsafe { CGContextRelease(self.0) };
        }
    }
    let _ctx_guard = CtxGuard(context);

    // SAFETY: `context` and `image` are live objects and `full` spans the context bounds.
    let full = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: width as f64,
            height: height as f64,
        },
    };
    unsafe { CGContextDrawImage(context, full, image) };

    // SAFETY: `CGBitmapContextGetData` returns a pointer valid until `_ctx_guard` drops.
    let data_ptr = unsafe { CGBitmapContextGetData(context) } as *const u8;
    if data_ptr.is_null() {
        return Err("window capture failed (bitmap context has no data)".to_string());
    }
    let total = bytes_per_row
        .checked_mul(height)
        .ok_or_else(|| "window capture failed (image too large)".to_string())?;
    // SAFETY: `data_ptr` points to `total` initialized RGBA8 bytes in the bitmap context.
    let rgba = unsafe { std::slice::from_raw_parts(data_ptr, total) }.to_vec();
    Ok((rgba, width as u32, height as u32))
}

#[cfg(any(target_os = "macos", test))]
fn unpremultiply_rgba8(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        match alpha {
            0 => pixel[..3].fill(0),
            255 => {}
            _ => {
                for channel in &mut pixel[..3] {
                    *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
    }
}

#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowClientRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct RgbaOverlay {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[cfg(any(target_os = "macos", windows))]
struct PlatformShot {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    encoding: ShotEncoding,
    #[cfg(target_os = "macos")]
    chrome: Option<ChromeLayer>,
}

#[cfg(any(target_os = "macos", windows))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShotEncoding {
    #[cfg(target_os = "macos")]
    PremultipliedRgba,
    #[cfg(windows)]
    Bgrx,
}

#[cfg(any(target_os = "macos", windows))]
impl ShotEncoding {
    #[cfg(target_os = "macos")]
    const NATIVE: Self = Self::PremultipliedRgba;
    #[cfg(windows)]
    const NATIVE: Self = Self::Bgrx;

    fn to_straight_rgba8(self, pixels: &mut [u8]) {
        match self {
            #[cfg(target_os = "macos")]
            Self::PremultipliedRgba => unpremultiply_rgba8(pixels),
            #[cfg(windows)]
            Self::Bgrx => bgrx_to_opaque_rgba8(pixels),
        }
    }

    const fn carries_outer_shape(self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Self::PremultipliedRgba => true,
            #[cfg(windows)]
            Self::Bgrx => false,
        }
    }
}

/// A `window` capture as its main-thread turn left it.
#[cfg(any(target_os = "macos", windows))]
pub(crate) struct WindowStitch {
    shot: PlatformShot,
    client: CapturePixels,
    rect: WindowClientRect,
}

#[cfg(any(target_os = "macos", windows))]
impl WindowStitch {
    fn finish(self) -> Result<CapturePixels, String> {
        note_pixel_work();
        let Self { shot, client, rect } = self;
        let (width, height, encoding) = (shot.width, shot.height, shot.encoding);
        let mut pixels = shot.pixels;
        let outer_alpha: Option<Vec<u8>> = encoding.carries_outer_shape().then(|| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[3])
                .collect()
        });
        encoding.to_straight_rgba8(&mut pixels);
        stitch_presented_client_into_window_rgba(&mut pixels, width, height, rect, &client)?;
        drop(client);
        #[cfg(target_os = "macos")]
        if let Some(chrome) = shot.chrome {
            composite_straight_rgba_overlay(&mut pixels, width, height, &chrome.overlay()?, rect)?;
        }
        if let Some(outer_alpha) = &outer_alpha {
            multiply_platform_outer_shape_alpha(&mut pixels, width, height, outer_alpha)?;
        }
        CapturePixels::rgba8(width, height, pixels)
    }

    fn peak_bytes(&self) -> usize {
        #[cfg(target_os = "macos")]
        let chrome = self.shot.chrome.as_ref().map_or(0, ChromeLayer::peak_bytes);
        #[cfg(not(target_os = "macos"))]
        let chrome = 0;
        StitchCharge {
            width: self.shot.width as usize,
            height: self.shot.height as usize,
            photograph: self.shot.pixels.capacity(),
            client: self.client.allocation(),
            client_width: self.client.dims().0,
            chrome,
            outer_shape: self.shot.encoding.carries_outer_shape(),
        }
        .peak_bytes()
    }
}

#[cfg(any(target_os = "macos", windows))]
struct StitchCharge {
    width: usize,
    height: usize,
    photograph: usize,
    client: usize,
    client_width: usize,
    chrome: usize,
    outer_shape: bool,
}

#[cfg(any(target_os = "macos", windows))]
impl StitchCharge {
    fn peak_bytes(&self) -> usize {
        let shape = if self.outer_shape {
            self.width.saturating_mul(self.height).saturating_mul(3)
        } else {
            0
        };
        let stitch = self
            .client
            .saturating_add(self.chrome)
            .saturating_add(shape)
            .saturating_add(self.client_width.saturating_mul(4));
        self.photograph
            .saturating_add(stitch.max(capture_encode_overhead(self.width, self.height)))
    }
}

#[cfg(target_os = "macos")]
struct ChromeLayer {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    pixels: ChromePixels,
}

#[cfg(target_os = "macos")]
enum ChromePixels {
    Bitmap { bytes: Vec<u8>, desc: BitmapDesc },
    Png(Vec<u8>),
}

#[cfg(target_os = "macos")]
impl ChromeLayer {
    fn overlay(self) -> Result<RgbaOverlay, String> {
        note_pixel_work();
        let rgba = match self.pixels {
            ChromePixels::Bitmap { bytes, desc } => normalize_bitmap(&bytes, desc)
                .map_err(|error| format!("window capture AppKit chrome: {error}"))?,
            ChromePixels::Png(png) => {
                let (rgba, width, height) = decode_native_chrome_png(&png)?;
                if (width, height) != (self.width, self.height) {
                    return Err(format!(
                        "window capture AppKit chrome PNG {width}x{height}px disagrees with its \
                         bitmap {}x{}px",
                        self.width, self.height
                    ));
                }
                rgba
            }
        };
        Ok(RgbaOverlay {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            rgba,
        })
    }

    fn peak_bytes(&self) -> usize {
        let handed = match &self.pixels {
            ChromePixels::Bitmap { bytes, .. } | ChromePixels::Png(bytes) => bytes.capacity(),
        };
        chrome_peak_bytes(self.width as usize, self.height as usize, handed)
    }
}

#[cfg(target_os = "macos")]
const fn chrome_peak_bytes(width: usize, height: usize, handed: usize) -> usize {
    handed.saturating_add(width.saturating_mul(height).saturating_mul(4))
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BitmapDesc {
    width: usize,
    height: usize,
    bytes_per_row: usize,
    bits_per_component: usize,
    bits_per_pixel: usize,
    bitmap_info: u32,
    rgb: bool,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectLayout {
    offsets: [usize; 4],
    premultiplied: bool,
}

#[cfg(any(target_os = "macos", test))]
impl BitmapDesc {
    fn direct_layout(&self) -> Result<DirectLayout, String> {
        const ALPHA_INFO: u32 = 0x1F;
        const PREMULTIPLIED_LAST: u32 = 1;
        const PREMULTIPLIED_FIRST: u32 = 2;
        const LAST: u32 = 3;
        const FIRST: u32 = 4;
        const FLOAT: u32 = 0xF00;
        const BYTE_ORDER: u32 = 0x7000;
        const ORDER_DEFAULT: u32 = 0;
        const ORDER_32_LITTLE: u32 = 2 << 12;
        const ORDER_32_BIG: u32 = 4 << 12;
        const PIXEL_FORMAT: u32 = 0xF_0000;
        let refuse = || format!("AppKit chrome bitmap layout is not read directly: {self:?}");
        if !self.rgb
            || self.bits_per_component != 8
            || self.bits_per_pixel != 32
            || self.bitmap_info & (FLOAT | PIXEL_FORMAT) != 0
            || self
                .width
                .checked_mul(4)
                .is_none_or(|row| row > self.bytes_per_row)
        {
            return Err(refuse());
        }
        let (alpha_first, premultiplied) = match self.bitmap_info & ALPHA_INFO {
            PREMULTIPLIED_LAST => (false, true),
            PREMULTIPLIED_FIRST => (true, true),
            LAST => (false, false),
            FIRST => (true, false),
            _ => return Err(refuse()),
        };
        let big_endian = match self.bitmap_info & BYTE_ORDER {
            ORDER_DEFAULT | ORDER_32_BIG => true,
            ORDER_32_LITTLE => false,
            _ => return Err(refuse()),
        };
        let offsets = match (alpha_first, big_endian) {
            (false, true) => [0, 1, 2, 3],  // R G B A
            (true, true) => [1, 2, 3, 0],   // A R G B
            (false, false) => [3, 2, 1, 0], // A B G R
            (true, false) => [2, 1, 0, 3],  // B G R A
        };
        Ok(DirectLayout {
            offsets,
            premultiplied,
        })
    }
}

#[cfg(any(target_os = "macos", test))]
fn normalize_bitmap(bytes: &[u8], desc: BitmapDesc) -> Result<Vec<u8>, String> {
    let layout = desc.direct_layout()?;
    let required = match desc.height {
        0 => 0,
        height => (height - 1)
            .checked_mul(desc.bytes_per_row)
            .and_then(|rows| rows.checked_add(desc.width * 4))
            .ok_or_else(|| "AppKit chrome bitmap size overflows".to_string())?,
    };
    if bytes.len() < required {
        return Err(format!(
            "AppKit chrome bitmap has {} bytes, expected at least {required}",
            bytes.len()
        ));
    }
    let mut rgba = Vec::with_capacity(desc.width * desc.height * 4);
    for y in 0..desc.height {
        let row = &bytes[y * desc.bytes_per_row..][..desc.width * 4];
        for pixel in row.as_chunks::<4>().0 {
            rgba.extend(layout.offsets.map(|offset| pixel[offset]));
        }
    }
    if layout.premultiplied {
        unpremultiply_rgba8(&mut rgba);
    }
    Ok(rgba)
}

#[cfg(any(windows, test))]
fn bgrx_to_opaque_rgba8(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        pixel[3] = u8::MAX;
    }
}

#[cfg(any(target_os = "macos", windows, test))]
fn stitch_presented_client_into_window_rgba(
    window_rgba: &mut [u8],
    window_width: u32,
    window_height: u32,
    rect: WindowClientRect,
    client: &CapturePixels,
) -> Result<(), String> {
    let width = usize::try_from(window_width)
        .map_err(|_| "window capture width does not fit memory".to_string())?;
    let height = usize::try_from(window_height)
        .map_err(|_| "window capture height does not fit memory".to_string())?;
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "window capture dimensions overflow".to_string())?;
    if window_rgba.len() != expected {
        return Err(format!(
            "window capture buffer has {} bytes, expected {expected}",
            window_rgba.len()
        ));
    }
    let (client_width, client_height) = client.dims();
    if (client_width, client_height) != (rect.width as usize, rect.height as usize) {
        return Err(format!(
            "presented client {client_width}x{client_height}px does not match platform client \
             {}x{}px",
            rect.width, rect.height
        ));
    }
    let x =
        usize::try_from(rect.x).map_err(|_| "platform client x does not fit memory".to_string())?;
    let y =
        usize::try_from(rect.y).map_err(|_| "platform client y does not fit memory".to_string())?;
    if x.checked_add(client_width)
        .is_none_or(|right| right > width)
        || y.checked_add(client_height)
            .is_none_or(|bottom| bottom > height)
    {
        return Err(format!(
            "platform client at ({},{}) size {}x{}px exceeds window {}x{}px",
            rect.x, rect.y, rect.width, rect.height, window_width, window_height
        ));
    }
    let mut row = Vec::with_capacity(client_width * 4);
    for source_y in 0..client_height {
        row.clear();
        client.rgba_row(source_y, &mut row);
        if row.len() != client_width * 4 {
            return Err(format!("presented client row {source_y} is short"));
        }
        let destination = ((y + source_y) * width + x) * 4;
        window_rgba[destination..destination + client_width * 4].copy_from_slice(&row);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows, test))]
fn multiply_platform_outer_shape_alpha(
    current_rgba: &mut [u8],
    width: u32,
    height: u32,
    platform_alpha: &[u8],
) -> Result<(), String> {
    let width = usize::try_from(width)
        .map_err(|_| "window capture width does not fit memory".to_string())?;
    let height = usize::try_from(height)
        .map_err(|_| "window capture height does not fit memory".to_string())?;
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| "window capture dimensions overflow".to_string())?;
    let bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| "window capture dimensions overflow".to_string())?;
    if current_rgba.len() != bytes {
        return Err(format!(
            "window capture buffer has {} bytes, expected {bytes}",
            current_rgba.len()
        ));
    }
    if platform_alpha.len() != pixels {
        return Err(format!(
            "window capture shape mask has {} pixels, expected {pixels}",
            platform_alpha.len()
        ));
    }
    if width == 0 || height == 0 {
        return Ok(());
    }
    let shape = photographed_shape_alpha(platform_alpha, width, height);
    let platform_alpha = shape.as_ref();

    let mut queued = vec![false; pixels];
    let mut boundary = std::collections::VecDeque::new();
    let mut enqueue = |index: usize| {
        if platform_alpha[index] != 255 && !queued[index] {
            queued[index] = true;
            boundary.push_back(index);
        }
    };
    for x in 0..width {
        enqueue(x);
        enqueue((height - 1) * width + x);
    }
    for y in 0..height {
        enqueue(y * width);
        enqueue(y * width + width - 1);
    }

    while let Some(index) = boundary.pop_front() {
        let alpha = u16::from(platform_alpha[index]);
        let current = &mut current_rgba[index * 4..index * 4 + 4];
        current[3] = ((u16::from(current[3]) * alpha + 127) / 255) as u8;

        let x = index % width;
        let y = index / width;
        for dy in -1_isize..=1 {
            for dx in -1_isize..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let Some(nx) = x.checked_add_signed(dx) else {
                    continue;
                };
                let Some(ny) = y.checked_add_signed(dy) else {
                    continue;
                };
                if nx >= width || ny >= height {
                    continue;
                }
                let neighbor = ny * width + nx;
                if platform_alpha[neighbor] != 255 && !queued[neighbor] {
                    queued[neighbor] = true;
                    boundary.push_back(neighbor);
                }
            }
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows, test))]
fn photographed_shape_alpha(
    alpha: &[u8],
    width: usize,
    height: usize,
) -> std::borrow::Cow<'_, [u8]> {
    let mut col_ink = vec![false; width];
    let mut row_ink = vec![false; height];
    for (index, value) in alpha.iter().enumerate() {
        if *value != 0 {
            col_ink[index % width] = true;
            row_ink[index / width] = true;
        }
    }
    if col_ink.iter().all(|ink| *ink) && row_ink.iter().all(|ink| *ink) {
        return std::borrow::Cow::Borrowed(alpha);
    }
    let mut shape = alpha.to_vec();
    for x in (0..width).filter(|x| !col_ink[*x]) {
        let mirror = width - 1 - x;
        for y in 0..height {
            shape[y * width + x] = if col_ink[mirror] {
                alpha[y * width + mirror]
            } else {
                255
            };
        }
    }
    for y in (0..height).filter(|y| !row_ink[*y]) {
        let mirror = height - 1 - y;
        for x in 0..width {
            shape[y * width + x] = if row_ink[mirror] {
                shape[mirror * width + x]
            } else {
                255
            };
        }
    }
    std::borrow::Cow::Owned(shape)
}

#[cfg(any(target_os = "macos", test))]
fn composite_straight_rgba_overlay(
    destination: &mut [u8],
    destination_width: u32,
    destination_height: u32,
    overlay: &RgbaOverlay,
    clip: WindowClientRect,
) -> Result<(), String> {
    let dst_width = usize::try_from(destination_width)
        .map_err(|_| "window capture width does not fit memory".to_string())?;
    let dst_height = usize::try_from(destination_height)
        .map_err(|_| "window capture height does not fit memory".to_string())?;
    let dst_expected = dst_width
        .checked_mul(dst_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "window capture dimensions overflow".to_string())?;
    if destination.len() != dst_expected {
        return Err(format!(
            "window capture buffer has {} bytes, expected {dst_expected}",
            destination.len()
        ));
    }

    let overlay_width = usize::try_from(overlay.width)
        .map_err(|_| "native chrome width does not fit memory".to_string())?;
    let overlay_height = usize::try_from(overlay.height)
        .map_err(|_| "native chrome height does not fit memory".to_string())?;
    let overlay_expected = overlay_width
        .checked_mul(overlay_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "native chrome dimensions overflow".to_string())?;
    if overlay.rgba.len() != overlay_expected {
        return Err(format!(
            "native chrome has {} bytes, expected {overlay_expected}",
            overlay.rgba.len()
        ));
    }

    let overlay_x = usize::try_from(overlay.x)
        .map_err(|_| "native chrome x does not fit memory".to_string())?;
    let overlay_y = usize::try_from(overlay.y)
        .map_err(|_| "native chrome y does not fit memory".to_string())?;
    if overlay_x
        .checked_add(overlay_width)
        .is_none_or(|right| right > dst_width)
        || overlay_y
            .checked_add(overlay_height)
            .is_none_or(|bottom| bottom > dst_height)
    {
        return Err(format!(
            "native chrome at ({},{}) size {}x{}px exceeds window {}x{}px",
            overlay.x,
            overlay.y,
            overlay.width,
            overlay.height,
            destination_width,
            destination_height
        ));
    }

    let clip_x0 =
        usize::try_from(clip.x).map_err(|_| "platform client x does not fit memory".to_string())?;
    let clip_y0 =
        usize::try_from(clip.y).map_err(|_| "platform client y does not fit memory".to_string())?;
    let clip_x1 = clip_x0
        .checked_add(
            usize::try_from(clip.width)
                .map_err(|_| "platform client width does not fit memory".to_string())?,
        )
        .ok_or_else(|| "platform client dimensions overflow".to_string())?;
    let clip_y1 = clip_y0
        .checked_add(
            usize::try_from(clip.height)
                .map_err(|_| "platform client height does not fit memory".to_string())?,
        )
        .ok_or_else(|| "platform client dimensions overflow".to_string())?;
    if clip_x1 > dst_width || clip_y1 > dst_height {
        return Err("platform client clip exceeds the window capture".to_string());
    }

    let x0 = overlay_x.max(clip_x0);
    let y0 = overlay_y.max(clip_y0);
    let x1 = overlay_x.saturating_add(overlay_width).min(clip_x1);
    let y1 = overlay_y.saturating_add(overlay_height).min(clip_y1);
    if x0 >= x1 || y0 >= y1 {
        return Ok(());
    }

    for dst_y in y0..y1 {
        let src_y = dst_y - overlay_y;
        for dst_x in x0..x1 {
            let src_x = dst_x - overlay_x;
            let source = (src_y * overlay_width + src_x) * 4;
            let target = (dst_y * dst_width + dst_x) * 4;
            let src = &overlay.rgba[source..source + 4];
            let dst = &mut destination[target..target + 4];
            let source_alpha = u32::from(src[3]);
            if source_alpha == 0 {
                continue;
            }
            if source_alpha == 255 {
                dst.copy_from_slice(src);
                continue;
            }
            let destination_alpha = u32::from(dst[3]);
            let inverse_source = 255 - source_alpha;
            let alpha_numerator = source_alpha * 255 + destination_alpha * inverse_source;
            if alpha_numerator == 0 {
                dst.fill(0);
                continue;
            }
            for channel in 0..3 {
                let numerator = u32::from(src[channel]) * source_alpha * 255
                    + u32::from(dst[channel]) * destination_alpha * inverse_source;
                dst[channel] = ((numerator + alpha_numerator / 2) / alpha_numerator).min(255) as u8;
            }
            dst[3] = ((alpha_numerator + 127) / 255).min(255) as u8;
        }
    }
    Ok(())
}

/// Encode a tightly packed RGBA8 buffer (`width * height * 4` bytes) as a PNG byte vector.
pub(crate) fn encode_rgba8_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    encode_srgb_rgba8(width, height, |writer| writer.write_image_data(rgba))
}

/// One session's window membership as the main thread sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionWindowRow {
    pub(crate) local: u64,
    pub(crate) window: u64,
    pub(crate) active_tab: bool,
    pub(crate) window_focused: bool,
}

impl App {
    /// Every session some window hosts, with the window it is reported under.
    pub(crate) fn session_window_rows(&self) -> Vec<SessionWindowRow> {
        let mut rows: Vec<SessionWindowRow> = Vec::new();
        for (wid, ws) in &self.windows {
            let focused = self.frontmost_window == Some(*wid);
            let mut seen: Vec<u64> = Vec::new();
            for tab in ws.tab_set.tabs() {
                tab.root.visit(&mut |&view| {
                    let Some(session) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                    else {
                        return;
                    };
                    if seen.contains(&session) {
                        return;
                    }
                    seen.push(session);
                    let row = SessionWindowRow {
                        local: session,
                        window: wid.0,
                        active_tab: crate::active_tab_displays_session(
                            ws,
                            &self.view_store,
                            session,
                        ),
                        window_focused: focused,
                    };
                    match rows.iter_mut().find(|r| r.local == session) {
                        Some(existing) => {
                            if focused {
                                *existing = row;
                            }
                        }
                        None => rows.push(row),
                    }
                });
            }
        }
        rows.sort_by_key(|r| r.local);
        rows
    }
}

#[cfg(test)]
#[path = "app_introspect_session_window_rows_tests.rs"]
mod session_window_rows_tests;

#[cfg(test)]
#[path = "app_introspect_dims_snapshot_tests.rs"]
mod dims_snapshot_tests;

#[cfg(test)]
#[path = "app_introspect_split_capture_tests.rs"]
mod split_capture_tests;

#[cfg(test)]
#[path = "app_introspect_chrome_output_tests.rs"]
mod chrome_output_tests;

#[cfg(test)]
#[path = "capture_barrier_tests.rs"]
mod capture_barrier_tests;

#[cfg(test)]
#[path = "app_introspect_terminal_split_capture_tests.rs"]
mod terminal_split_capture_tests;

#[cfg(test)]
#[path = "app_introspect_capture_overlay_reset_tests.rs"]
mod capture_overlay_reset_tests;

#[cfg(test)]
#[path = "app_introspect_encode_worker_tests.rs"]
mod encode_worker_tests;

#[cfg(test)]
#[path = "app_introspect_window_render_context_capture_tests.rs"]
mod window_render_context_capture_tests;

#[cfg(test)]
#[path = "app_introspect_headless_cursor_fx_tests.rs"]
mod headless_cursor_fx_tests;

#[cfg(test)]
#[path = "app_introspect_deferred_gpu_capture_tests.rs"]
mod deferred_gpu_capture_tests;

#[cfg(test)]
#[path = "app_introspect_headless_settings_preview_capture_tests.rs"]
mod headless_settings_preview_capture_tests;
