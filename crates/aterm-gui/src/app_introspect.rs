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

fn encode_srgb_rgba8(
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
    write_snapshot_artifacts_with_hook(encoded, text, transaction, || {})
}

fn write_snapshot_artifacts_with_hook(
    encoded: &CapturePng,
    text: &str,
    transaction: &SnapshotTransaction,
    before_done: impl FnOnce(),
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
    let done_file = match target.dir.write_new_private(&target.done, done.as_bytes()) {
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
mod video_keys_honesty_tests {
    use super::{video_empty_ledger_note, video_keys_reply_tokens, video_keys_status_tokens};

    #[test]
    fn status_and_reply_tokens_state_both_counts_only_for_a_keys_take() {
        assert_eq!(
            video_keys_status_tokens(true, 0, 5),
            " keys=true inputs=0 unlogged=5"
        );
        assert_eq!(video_keys_status_tokens(false, 0, 0), " keys=false");
        assert_eq!(
            video_keys_reply_tokens(true, 0, 3),
            " inputs=0 unlogged_inputs=3"
        );
        assert_eq!(video_keys_reply_tokens(false, 0, 0), "");
    }

    #[test]
    fn empty_ledger_note_separates_measured_zero_from_unmeasurable() {
        let notes = [
            video_empty_ledger_note(false, 0, 0),
            video_empty_ledger_note(true, 4, 0),
            video_empty_ledger_note(true, 3, 3),
            video_empty_ledger_note(true, 5, 2),
            video_empty_ledger_note(true, 0, 0),
        ];
        for (i, note) in notes.iter().enumerate() {
            for other in &notes[i + 1..] {
                assert_ne!(note, other, "two situations share one note");
            }
            assert!(
                !note.contains('"') && !note.contains('\\'),
                "a note must stay JSON-safe unescaped: {note}"
            );
        }
    }
}

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
    );
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
        Ok(all[all.len() - keep..].to_vec())
    }

    /// Sample the selected terminal grid and all main-thread-owned presentation geometry as one event-turn record.
    pub(crate) fn try_dims_snapshot(
        &self,
        session: u64,
        term: &Arc<Mutex<Terminal>>,
    ) -> Result<DimsSnapshot, &'static str> {
        let (rows, cols) = match term.try_lock() {
            Ok(terminal) => (u32::from(terminal.rows()), u32::from(terminal.cols())),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                let terminal = poisoned.into_inner();
                (u32::from(terminal.rows()), u32::from(terminal.cols()))
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err("terminal busy; retry dims");
            }
        };
        Ok(self.dims_snapshot(session, rows, cols))
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
            viewers: u32::try_from(windows.len()).unwrap_or(u32::MAX),
            visible_viewers: u32::try_from(visible_viewers).unwrap_or(u32::MAX),
            geometry,
            present_retry_state,
            present_retry_count,
            present_retry_remaining,
            present_retry_in_ms,
            layer_presentation: selected
                .and_then(|wid| self.windows[&wid].os_window.as_ref())
                .and_then(|w| self.apprt.window_surface_presentation(w)),
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
        if aterm_effects::companion::resident_pet_owner_present(
            pet_mode,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
        ) {
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
        let pet_tick = ws.fx.pet_prepare(aterm_effects::companion::PetFacts {
            facts: &capture_pet_facts(&exact_focus),
            host: &aterm_effects::host::HostFrameInput {
                now,
                visibility: crate::app_render::pet_visibility(capture_focused),
                reduced_motion: !pet_motion.animate(crate::motion::MotionEffect::CursorGlow),
                serious: !cursor_companions_allowed,
                shed_envelope,
                shed_active: load_shed,
                pointer_px: None,
                capture: capture_mode(windowless),
                geometry: crate::app_render::pet_frame_geom(
                    (cell_w, cell_h),
                    (usize::from(pane_rows), usize::from(pane_cols)),
                    (
                        i32::from(origin_x) + pane_origin.0,
                        i32::from(origin_y) + pane_origin.1,
                    ),
                ),
            },
            glow: aterm_effects::companion::GlowOwnership {
                enabled: glow_cfg.enabled,
                style: glow_cfg.style,
                style_raw_names_pet: pet_mode,
            },
            sing: aterm_effects::host::SingFacts {
                drive: sing_drive,
                flying_alpha: if kitty_enabled { cat_frame.alpha } else { 0 },
            },
            focused: capture_focused,
            obscured: ws.overlay_open() || ws.tab_menu.is_some() || focus_rewound,
            pane: Some((focus, pane_origin)),
            room: None,
        });
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
        if aterm_effects::companion::resident_pet_owner_present(
            pet_mode,
            glow_cfg.enabled && cursor_companions_allowed,
            glow_cfg.style,
        ) {
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
        let pet_tick = ws.fx.pet_prepare(aterm_effects::companion::PetFacts {
            facts: &capture_pet_facts(&exact_focus),
            host: &aterm_effects::host::HostFrameInput {
                now,
                visibility: crate::app_render::pet_visibility(capture_focused),
                reduced_motion: !pet_motion.animate(crate::motion::MotionEffect::CursorGlow),
                serious: !cursor_companions_allowed,
                shed_envelope,
                shed_active: load_shed,
                pointer_px: None,
                capture: capture_mode(windowless),
                geometry: crate::app_render::pet_frame_geom(
                    (cell_w, cell_h),
                    (rows, cols),
                    (i32::from(origin_x), i32::from(origin_y)),
                ),
            },
            glow: aterm_effects::companion::GlowOwnership {
                enabled: glow_cfg.enabled,
                style: glow_cfg.style,
                style_raw_names_pet: pet_mode,
            },
            sing: aterm_effects::host::SingFacts {
                drive: sing_drive,
                flying_alpha: if kitty_enabled { cat_frame.alpha } else { 0 },
            },
            focused: capture_focused,
            obscured: ws.overlay_open() || ws.tab_menu.is_some() || rewound,
            pane: None,
            room: None,
        });
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
        let words = ws.fx.word_grid(&aterm_effects::pipeline::WordGrid {
            now,
            lexicon,
            cfg,
            geom: effect_geom,
            cells: &ws.input_scratch.cells,
            line_sizes: &ws.input_scratch.line_sizes,
            base_y: ws.input_scratch.base_y,
            default_bg: ws.input_scratch.default_bg,
            epoch,
            rescan,
            companion_at,
            sel: crate::word_decorations::SelView {
                sel: &ws.input_scratch.selection,
                display_offset: ws.input_scratch.display_offset,
            },
            focused: ws.focused,
        });
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

    fn recording_card_identity(
        card: Option<&crate::SettingsCard>,
    ) -> Option<crate::VideoPresentedCardIdentity> {
        card.map(|card| crate::VideoPresentedCardIdentity {
            width: card.pw,
            height: card.ph,
            x: card.dx,
            y: card.dy,
            model: card.fp,
            geometry: card.geom,
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
        let card = Self::recording_card_identity(window.present_card());
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
        let current_card = Self::recording_card_identity(window.present_card());
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
        self.ensure_pixel_backend();
        let (plan, route) = self
            .active_visible_frame_layout(front)
            .ok_or(CaptureDeferral::Nothing)?;
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
        let (plan, terminal_grid) = self.stage_headless_capture(front, plan, route)?;
        Ok(ResolvedCapture {
            source: CaptureSource::Rerender {
                serial: None,
                visuals: self.host_visual_state(front, Instant::now()),
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
    ) -> Result<
        (
            crate::tab_model::VisibleLeafPlan,
            Option<crate::app_render::TerminalCaptureGrid>,
        ),
        CaptureDeferral,
    > {
        use crate::app_render::CapturePreparation;
        self.sample_native_preview_for_capture(wid, &plan);
        // Every route splices the message band, so each captures it at the capture instant.
        let capture_now = Instant::now();
        self.prepare_band_motion(wid, capture_now);
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
        let tray = window
            .present_card()
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

    fn snapshot_visible_text(
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
            .tabs()
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
            ws.tab_set.active_index().unwrap_or(0),
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

    /// Read an auxiliary own-rendered surface's controls as human-readable text lines.
    pub(crate) fn read_aux_controls(&self, target: AuxTarget) -> Vec<String> {
        match target {
            AuxTarget::Prefs => self.read_native_settings_controls(None),
            AuxTarget::About => self.read_native_settings_controls(Some((
                "about",
                crate::native_settings::SettingsRoute::About,
            ))),
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
            AuxTarget::AutopilotMenu => self.autopilot_menu_controls(),
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
mod native_settings_compatibility_controls_tests {
    use super::{App, AuxTarget};
    use crate::WindowId;
    use crate::native_settings::SettingsRoute;
    use crate::native_ui::UiKey;

    #[test]
    fn about_and_update_aliases_emit_the_exact_retained_native_frame() {
        for (target, route, retired_prefix) in [
            (AuxTarget::About, SettingsRoute::About, "about "),
            (AuxTarget::Update, SettingsRoute::SoftwareUpdate, "update "),
        ] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            assert!(app.open_settings_tab(route));
            assert!(app.prepare_native_input_scratch(wid));

            let (expected_ui, actionable, semantics) = {
                let frame = app.windows[&wid]
                    .native_ui_compiled
                    .as_ref()
                    .expect("native Settings frame retained beside its raster");
                assert_eq!(frame.stamp.view, app.active_native_view(wid).unwrap().1);
                assert!(
                    frame
                        .compiled
                        .semantic(&UiKey::new(format!("settings/page{}", route.path())))
                        .is_some(),
                    "the retained frame owns the requested {route:?} page root"
                );
                (
                    frame.compiled.controls_lines(),
                    frame
                        .compiled
                        .semantics
                        .iter()
                        .filter(|node| node.action.is_some())
                        .count(),
                    frame.compiled.semantics.len(),
                )
            };

            let actual = app.read_aux_controls(target);
            assert!(
                actual[0].contains("state open=true")
                    && actual[0].contains("pane=native-tab")
                    && actual[0].contains(&format!("route={}", route.path()))
                    && actual[0].contains(&format!("page={:?}", route.label())),
                "alias identity must name the live native route: {:?}",
                actual[0]
            );
            assert!(actual.iter().any(|line| {
                line == &format!(
                    "native surface=native-tab route={} controls={actionable} semantics={semantics}",
                    route.path()
                )
            }));
            let actual_ui = actual
                .iter()
                .filter(|line| line.starts_with("ui "))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                actual_ui, expected_ui,
                "compatibility alias must serialize the exact semantic artifact retained for paint"
            );
            assert!(
                actual.iter().all(|line| !line.starts_with(retired_prefix)),
                "retired standalone projection leaked through {target:?}: {actual:?}"
            );
        }
    }

    fn split_record(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut token = String::new();
        let (mut quoted, mut escaped) = (false, false);
        for ch in line.chars() {
            if escaped {
                escaped = false;
            } else if quoted && ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = !quoted;
            } else if ch.is_whitespace() && !quoted {
                if !token.is_empty() {
                    out.push(std::mem::take(&mut token));
                }
                continue;
            }
            token.push(ch);
        }
        if !token.is_empty() {
            out.push(token);
        }
        out
    }

    #[test]
    fn controls_settings_lists_showcase_rows() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.cols = 160;
            window.rows = 50;
        }
        assert!(app.open_settings_tab(SettingsRoute::Themes));
        let lines = app.read_aux_controls(AuxTarget::Prefs);
        let listed = |lines: &[String], key: &str| {
            lines
                .iter()
                .any(|line| line.starts_with(&format!("ui key=\"{key}\" ")))
        };
        let row = lines
            .iter()
            .find(|line| {
                line.starts_with("ui key=\"settings/showcase/card/")
                    && line.contains(" role=ListBoxOption ")
            })
            .unwrap_or_else(|| panic!("a row record: {lines:#?}"))
            .clone();
        let tokens = split_record(&row);
        assert!(
            tokens.iter().any(|token| token.starts_with("value=")
                && token.contains("contrast ")
                && token.contains(":1")),
            "{row}"
        );
        for key in [
            "settings/showcase/list",
            "settings/showcase/filter",
            "settings/showcase/status",
            "settings/showcase/customize",
        ] {
            assert!(listed(&lines, key), "{key}: {lines:#?}");
        }
        for key in ["settings/showcase/keep", "settings/showcase/revert"] {
            assert!(!listed(&lines, key), "idle: {key}");
        }
        let (_, instance, view, state) = app.native_settings_view_target().unwrap();
        let in_use = state.highlighted_slug();
        let other = lines
            .iter()
            .filter_map(|line| line.strip_prefix("ui key=\"settings/showcase/card/"))
            .filter_map(|rest| rest.split('"').next())
            .find(|slug| !slug.contains('/') && Some(slug.to_string()) != in_use)
            .expect("a row not in use")
            .to_string();
        let press = |app: &mut App, key: &str| {
            let _ = app.native_runtime.dispatch(
                instance,
                view,
                crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new(key),
                    value: None,
                }),
            );
        };
        press(&mut app, &format!("settings/showcase/card/{other}"));
        let lines = app.read_aux_controls(AuxTarget::Prefs);
        for key in ["settings/showcase/keep", "settings/showcase/revert"] {
            assert!(listed(&lines, key), "trying: {key}");
        }
        press(&mut app, "settings/showcase/revert");
        press(&mut app, "settings/showcase/customize");
        let lines = app.read_aux_controls(AuxTarget::Prefs);
        assert!(
            lines
                .iter()
                .any(|line| line
                    .starts_with(&format!("field key={} ", crate::prefs::EDIT_FOREGROUND))),
            "{lines:#?}"
        );
        assert!(listed(&lines, "settings/showcase/palette/0"), "{lines:#?}");
        assert!(
            !lines
                .iter()
                .any(|line| line.contains(" role=ListBoxOption ")),
            "the list leaves while the sheet stands in its place"
        );
    }

    #[test]
    fn controls_theme_options_are_quoted_and_split_whole() {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "theme = \"Nord\"\n".to_string(),
        )
        .unwrap();
        let wid = WindowId(0);
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.cols = 160;
            window.rows = 50;
        }
        assert!(app.open_settings_tab(SettingsRoute::Home));
        let lines = app.read_aux_controls(AuxTarget::Prefs);
        let theme = lines
            .iter()
            .find(|line| line.starts_with(&format!("field key={} ", crate::prefs::EDIT_THEME)))
            .expect("Top Settings exposes the theme picker");
        let tokens = split_record(theme);
        let options = tokens
            .iter()
            .find_map(|token| token.strip_prefix("options="))
            .expect("theme options");
        assert!(
            options.contains("\"Solarized Dark\""),
            "a spaced name is one quoted option: {options}"
        );
        assert!(
            tokens.iter().all(|token| !token.starts_with("Dark")),
            "no option leaks a stray token: {tokens:?}"
        );
        assert!(
            tokens.iter().skip(1).all(|token| token.contains('=')),
            "{tokens:?}"
        );
        assert!(
            theme
                .split_whitespace()
                .any(|token| token.starts_with("Dark\""))
        );
    }

    #[test]
    fn route_aliases_never_describe_a_matching_background_window() {
        for (target, background_route, alias) in [
            (AuxTarget::About, SettingsRoute::About, "about"),
            (AuxTarget::Update, SettingsRoute::SoftwareUpdate, "update"),
        ] {
            let mut app = App::headless_for_test();
            let background = WindowId(0);
            assert!(app.open_settings_tab(background_route));
            let (_, background_view) = app
                .active_native_view(background)
                .expect("background Settings route");

            let session = app.next_session_id;
            let front = app.insert_logical_window(crate::stub_session(session), 24, 80);
            assert_eq!(app.frontmost_window, Some(front));
            assert!(app.open_settings_tab(SettingsRoute::Home));
            assert!(matches!(
                app.native_runtime.view_state(background_view),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state.route == background_route
            ));
            assert!(matches!(
                app.active_native_view(front)
                    .and_then(|(_, view)| app.native_runtime.view_state(view)),
                Some(crate::native_app::AppViewState::Settings(state))
                    if state.route == SettingsRoute::Home
            ));

            let lines = app.read_aux_controls(target);
            assert!(
                lines[0].contains("route=/home"),
                "front-route mismatch must be explicit: {:?}",
                lines[0]
            );
            assert!(lines[2].contains(&format!(
                "alias={alias} route=/home expected-route={}",
                background_route.path()
            )));
            assert!(
                lines.iter().all(|line| !line.starts_with("ui ")),
                "background semantics must not describe the front capture: {lines:?}"
            );
        }
    }
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
                for view in tab.root.leaves() {
                    let Some(session) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                    else {
                        continue;
                    };
                    if seen.contains(&session) {
                        continue;
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
                }
            }
        }
        rows.sort_by_key(|r| r.local);
        rows
    }
}

#[cfg(test)]
mod session_window_rows_tests {
    use super::SessionWindowRow;
    use crate::{App, WindowId};

    #[test]
    fn rows_follow_the_tab_tree_and_the_front_window() {
        let mut app = App::headless_for_test();
        assert_eq!(
            app.session_window_rows(),
            vec![SessionWindowRow {
                local: 0,
                window: 0,
                active_tab: true,
                window_focused: true,
            }]
        );

        let sid1 = app.next_session_id;
        app.push_stub_tab(WindowId(0), crate::stub_session(sid1));
        let rows = app.session_window_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].local, rows[0].window, rows[0].active_tab),
            (0, 0, false)
        );
        assert_eq!(
            (rows[1].local, rows[1].window, rows[1].active_tab),
            (sid1, 0, true)
        );
        assert!(rows.iter().all(|r| r.window_focused));

        let sid2 = app.next_session_id;
        let wid2 = app.insert_logical_window(crate::stub_session(sid2), 24, 80);
        let rows = app.session_window_rows();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter()
                .filter(|r| r.window == 0)
                .all(|r| !r.window_focused)
        );
        assert_eq!(
            rows[2],
            SessionWindowRow {
                local: sid2,
                window: wid2.0,
                active_tab: true,
                window_focused: true,
            }
        );
        assert!(rows.windows(2).all(|w| w[0].local < w[1].local));
        assert!(
            rows.iter()
                .all(|r| app.windows.contains_key(&WindowId(r.window)))
        );
    }
}

#[cfg(test)]
mod dims_snapshot_tests {
    use std::time::Instant;

    use super::App;
    use crate::WindowId;
    use aterm_window::dpi::PhysicalSize;

    #[test]
    fn coherent_dims_snapshot_is_nonblocking_when_the_grid_is_busy() {
        let app = App::headless_for_test();
        let term = app.front_terminal(WindowId(0)).unwrap().term.clone();
        let snapshot = app.try_dims_snapshot(0, &term).unwrap();
        assert_eq!((snapshot.rows, snapshot.cols), (24, 80));

        let _held = term.lock().unwrap();
        assert_eq!(
            app.try_dims_snapshot(0, &term).unwrap_err(),
            "terminal busy; retry dims"
        );
    }

    #[test]
    fn detached_dims_reports_the_scale_its_own_geometry_came_from() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let metrics = crate::MetricsView::for_scale(2.0);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.scale = 2.0;
            ws.metrics = metrics;
        }
        app.font_px = metrics.font_px;
        app.backend.activate_px(metrics.font_px);
        app.backend.set_pad(metrics.pad);
        app.backend.set_pad_top(metrics.pad_top);
        app.backend.set_head(metrics.head);

        let attached = app.dims_snapshot(0, 24, 80);
        let detached = app.dims_snapshot(u64::MAX, 24, 80);
        assert_eq!(
            (detached.geometry, detached.window),
            ("detached", None),
            "no window contains this session"
        );
        assert!(
            (detached.scale - 2.0).abs() < f64::EPSILON,
            "the detached record reports the 2x it was derived from, not 1.0 \
             (scale={})",
            detached.scale
        );
        assert_eq!(
            (detached.scale, detached.font_px, detached.pad),
            (attached.scale, attached.font_px, attached.pad),
            "one backend, one scale: detaching a session cannot change the DPI \
             the same pixels were rasterized at"
        );
    }

    fn panes_rows_matching_the_engines(app: &App, panes: usize) -> Vec<(u64, u16, u16)> {
        let lines = app.read_pane_layout(None);
        assert_eq!(lines.len(), panes + 1, "a header and the panes: {lines:?}");
        assert!(
            lines[0].starts_with(&format!("layout tab=0 panes={panes} ")),
            "{}",
            lines[0]
        );
        let mut focused = 0;
        let mut rows_out = Vec::new();
        for line in &lines[1..] {
            let field = |key: &str| {
                line.split_whitespace()
                    .find_map(|tok| tok.strip_prefix(key))
                    .unwrap_or_else(|| panic!("{key} in {line}"))
                    .to_string()
            };
            let sid: u64 = field("session=").parse().expect("session=<sid>");
            let rect = field("rect=");
            let (_, size) = rect
                .rsplit_once(',')
                .expect("rect=<row_off>,<col_off>,<rows>x<cols>");
            let (rows, cols) = size.split_once('x').expect("<rows>x<cols>");
            let (rows, cols): (u16, u16) = (rows.parse().unwrap(), cols.parse().unwrap());
            focused += usize::from(field("focused=") == "true");
            let term = app.pool.get(sid).expect("pane session").term.clone();
            let t = crate::term_lock(&term);
            assert_eq!(
                (t.rows(), t.cols()),
                (rows, cols),
                "`panes` must report the grid `dims` reads for this pane: {line}"
            );
            rows_out.push((sid, rows, cols));
        }
        assert_eq!(focused, 1, "exactly one focused pane: {lines:?}");
        rows_out
    }

    #[test]
    fn panes_agrees_with_the_engines_where_the_tree_layout_does_not() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
        app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
        let cols = app.windows[&wid].cols;
        app.windows.get_mut(&wid).expect("window 0").rows = 6;
        app.resize_panes(wid);

        let reported = panes_rows_matching_the_engines(&app, 3);

        let tree = app
            .active_tree(wid)
            .expect("an all-terminal tab has a tree");
        let old = tree.compute_layout(6, cols);
        assert!(
            old.iter().any(|rect| {
                reported
                    .iter()
                    .find(|(sid, ..)| *sid == rect.session)
                    .is_some_and(|&(_, rows, cols)| (rows, cols) != (rect.rows, rect.cols))
            }),
            "negative control: the tree's layout must disagree with the engines \
             here, or this test proves nothing ({old:?} vs {reported:?})"
        );
    }

    #[test]
    fn panes_reports_a_co_viewed_sessions_shared_grid_in_every_viewer() {
        let mut app = App::headless_for_test(); // window 0: 24x80, session 0
        let big = app
            .open_active_session_in_new_window_logical()
            .expect("share session 0 into a second window");
        assert_eq!(app.pool.views(0), Some(2));
        {
            let ws = app.windows.get_mut(&big).expect("the second window");
            ws.rows = 60;
            ws.cols = 200;
        }
        app.resize_panes(big);
        app.resize_panes(WindowId(0));

        let plan = app
            .active_visible_leaf_plan(big)
            .expect("the big window's plan");
        assert_eq!(
            App::leaf_grid_cells(&plan.leaves[0].rect),
            (60, 200),
            "negative control: the larger viewer's leaf is bigger than the shared grid"
        );
        for wid in [WindowId(0), big] {
            app.frontmost_window = Some(wid);
            assert_eq!(
                panes_rows_matching_the_engines(&app, 1),
                vec![(0, 24, 80)],
                "window {wid:?} reports the shared minimum"
            );
        }
    }

    #[test]
    fn dims_snapshot_tracks_live_zoom_and_raw_surface_remainder() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.backend.set_pad(12);
        app.backend.set_pad_top(2);
        app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
        app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
        let initial = app.dims_snapshot(0, 24, 80);
        assert_eq!((initial.pad_top, initial.pad_bottom), (2, 12));
        assert_eq!(
            initial.frame_h,
            initial
                .composed_rows
                .saturating_mul(initial.cell_h)
                .saturating_add(initial.head)
                .saturating_add(initial.pad_top)
                .saturating_add(initial.pad_bottom),
            "dims exposes the same explicit top/bottom frame law as presentation",
        );
        assert_ne!(
            initial.frame_h,
            initial
                .composed_rows
                .saturating_mul(initial.cell_h)
                .saturating_add(initial.head)
                .saturating_add(initial.pad.saturating_mul(2)),
            "negative control: dims must not retain the old conserved-height law",
        );
        assert_eq!(initial.present_retry_state, "ready");
        app.windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_recovery_redraw_requested();
        assert_eq!(app.dims_snapshot(0, 24, 80).present_retry_state, "redraw");
        let _ = app
            .windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_external_stimulus();

        app.windows.get_mut(&wid).unwrap().present_retry.on_drop(
            crate::metrics::PresentDropReason::GpuTimeout,
            Instant::now(),
        );
        let backing_off = app.dims_snapshot(0, 24, 80);
        assert_eq!(backing_off.present_retry_state, "backoff");
        assert_eq!(backing_off.present_retry_count, 1);
        assert!(backing_off.present_retry_in_ms.is_some());
        let _ = app
            .windows
            .get_mut(&wid)
            .unwrap()
            .present_retry
            .on_external_stimulus();

        let surface = PhysicalSize::new(initial.frame_w + 11, initial.frame_h + 28);
        app.windows.get_mut(&wid).unwrap().win_px = Some(surface);
        let banded = app.dims_snapshot(0, 24, 80);
        assert_eq!(banded.geometry, "window");
        assert_eq!(
            (banded.surface_w, banded.surface_h),
            (surface.width, surface.height)
        );
        assert_eq!(
            banded.band_top + banded.band_bottom,
            banded.surface_h - banded.frame_h,
        );

        let old_font_size = app.font_size;
        app.windows.get_mut(&wid).unwrap().present_retry.on_drop(
            crate::metrics::PresentDropReason::GpuOccluded,
            Instant::now(),
        );
        let parked = app.dims_snapshot(0, 24, 80);
        assert_eq!(parked.present_retry_state, "parked");
        assert_eq!(parked.present_retry_in_ms, None);
        app.set_font_size(old_font_size + 8.0);
        let zoomed = app.dims_snapshot(0, 24, 80);
        assert_eq!(
            zoomed.present_retry_state, "ready",
            "font rebuild is an explicit recovery stimulus"
        );
        assert!(zoomed.cell_w > banded.cell_w && zoomed.cell_h > banded.cell_h);
        assert!(zoomed.frame_w > banded.frame_w && zoomed.frame_h > banded.frame_h);
        assert_eq!(
            (zoomed.surface_w, zoomed.surface_h),
            (surface.width, surface.height),
            "the raw surface is an independent live fact"
        );
        assert_eq!(
            zoomed.crop_left + zoomed.crop_right,
            zoomed.frame_w.saturating_sub(zoomed.surface_w)
        );
        assert_eq!(
            zoomed.crop_top + zoomed.crop_bottom,
            zoomed.frame_h.saturating_sub(zoomed.surface_h)
        );
    }
}

#[cfg(test)]
mod split_capture_tests {

    use crate::{App, WindowId, term_lock};
    use std::time::{Duration, Instant};

    fn configure_pane_backgrounds(app: &App, sid: u64) {
        let bg = aterm_render::Theme::default().bg;
        for session in [0, sid] {
            let term = &app.pool.get(session).expect("pane session").term;
            term_lock(term).set_default_background(aterm_core::terminal::Rgb {
                r: (bg >> 16) as u8,
                g: (bg >> 8) as u8,
                b: bg as u8,
            });
        }
    }

    #[test]
    fn first_headless_capture_applies_the_configured_pet_species() {
        use aterm_effects::kitty_pet::PetSpecies;

        let capture = |style: &str, split: bool| {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            if split {
                let focus = app.split_active_stub_tab(wid);
                configure_pane_backgrounds(&app, focus);
            }
            app.config.cursor_trail = Some(true);
            app.config.cursor_trail_style = Some(style.into());
            app.config.motion = Some("full".into());
            app.recompute_sparkle();
            app.sparkle = None;
            if split {
                app.windows.get_mut(&wid).expect("window").focused = true;
            }
            app.capture_words(wid, Instant::now());
            let ws = app.windows.get(&wid).expect("window");
            let sprites = aterm_render::sprites::published(&ws.input_scratch.fx_layers);
            assert!(
                !sprites.is_empty(),
                "the first capture emits the configured pet body"
            );
            (
                ws.fx.companion().brain().species(),
                sprites
                    .iter()
                    .map(|s| s.art.data().to_vec())
                    .collect::<Vec<_>>(),
            )
        };

        for split in [false, true] {
            let cat = capture("rainbow kitty pet", split);
            let dog = capture("rainbow dog pet", split);
            assert_eq!(
                cat.0,
                PetSpecies::Cat,
                "split={split}: negative control: cat stays cat"
            );
            assert_eq!(dog.0, PetSpecies::Dog, "split={split}");
            assert!(
                dog.1 != cat.1,
                "split={split}: the first captured arts must be the configured dog skin"
            );
        }
    }

    #[test]
    fn a_split_paints_a_streak_anchored_in_the_pane_that_produced_it() {
        let run = |enabled: bool| {
            let t0 = Instant::now();
            let mut app = App::headless_for_test();
            app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
                enabled: Some(enabled),
                ..Default::default()
            });
            let wid = WindowId(0);
            let sid = app.split_active_stub_tab(wid);
            app.recompute_sparkle();
            {
                let ws = app.windows.get_mut(&wid).expect("window");
                ws.focused = true;
            }
            app.capture_words(wid, t0);
            let term = app.pool.get(sid).expect("pane session").term.clone();
            term_lock(&term).process(b"\r\noutput in the focused pane\r\n");
            app.capture_words(wid, t0 + Duration::from_millis(120));
            let ws = app.windows.get(&wid).expect("window");
            let xs: Vec<u16> = ws.fx.nova().iter().map(|q| q.x).collect();
            let half = u32::from(ws.cols) * app.backend.cell_size().0 as u32 / 2;
            (xs, half)
        };
        let (xs, half) = run(true);
        assert!(
            !xs.is_empty(),
            "a split must not go dark: the composed pass owes streak light"
        );
        let min_x = xs.iter().copied().min().expect("min");
        assert!(
            u32::from(min_x) >= half,
            "streak light must be anchored inside the pane that earned it: \
             x {min_x} is left of the divider at {half}"
        );
        let (off, _) = run(false);
        assert!(
            off.is_empty(),
            "NON-VACUITY: with the streak off the same split draws none of it"
        );
    }

    #[test]
    fn a_capture_carries_the_output_streak_and_an_off_config_carries_none() {
        let streak_quads = |enabled: bool| {
            let t0 = Instant::now();
            let mut app = App::headless_for_test();
            app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
                enabled: Some(enabled),
                ..Default::default()
            });
            let wid = WindowId(0);
            {
                let ws = app.windows.get_mut(&wid).expect("window");
                ws.focused = true;
            }
            let term = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            app.capture_words(wid, t0);
            let before = app.windows[&wid].fx.nova().len();
            term_lock(&term).process(b"\r\nprogram output arrives here\r\n");
            app.capture_words(wid, t0 + Duration::from_millis(120));
            app.windows[&wid].fx.nova().len().saturating_sub(before)
        };
        let on = streak_quads(true);
        let off = streak_quads(false);
        assert!(
            on > 0,
            "an enabled streak must put light in the capture's nova channel"
        );
        assert_eq!(
            off, 0,
            "NON-VACUITY: with the streak off the same script draws none of it"
        );
    }

    #[test]
    fn split_capture_splices_decorations() {
        let t0 = Instant::now();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid = app.split_active_stub_tab(wid);
        app.recompute_sparkle();
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.focused = true;
        }
        app.capture_words(wid, t0);
        for s in [0u64, sid] {
            let term = app.pool.get(s).expect("pane session").term.clone();
            term_lock(&term).process(b"\r\nhello kitty friend\r\n");
        }
        app.windows
            .get_mut(&wid)
            .expect("window")
            .pending_deco_birth = Some(t0);
        app.capture_words(wid, t0);
        app.capture_words(wid, t0 + Duration::from_millis(600));
        let ws = app.windows.get(&wid).expect("window");
        let sprites = aterm_render::sprites::published(&ws.input_scratch.fx_layers);
        assert!(
            !sprites.is_empty(),
            "a split capture carries the cats the glass draws (deco={} ink={})",
            ws.input_scratch.word_decorations.len(),
            ws.input_scratch.ink.len()
        );
        assert!(
            sprites.iter().all(|s| !s.art.is_empty()),
            "and the arts those sprites draw"
        );
    }
}

#[cfg(test)]
mod chrome_output_tests {
    #[cfg(target_os = "macos")]
    use super::decode_native_chrome_png;
    use super::{
        BitmapDesc, CapturePixels, CapturePng, RgbaOverlay, WindowClientRect, bgrx_to_opaque_rgba8,
        composite_straight_rgba_overlay, multiply_platform_outer_shape_alpha, normalize_bitmap,
        stitch_presented_client_into_window_rgba, unpremultiply_rgba8,
    };
    use aterm_render::Frame;

    #[test]
    fn strip_caption_line_reports_the_frame_and_its_buttons() {
        use super::strip_caption_chrome_line;
        use crate::client_frame::ClientFrame;
        use crate::tab_bar::{CaptionButton, CaptionSlot, VersionSlotCols};
        let frame = ClientFrame {
            minimize: true,
            maximize: true,
            window_menu: true,
            maximized: false,
            edges: true,
            header: true,
            layout: crate::client_frame::ButtonLayout::FALLBACK,
        };
        let slot = CaptionSlot::measure(Some(frame), 9, 1.0);
        let segs = crate::tab_bar::layout_segments_with_caption(
            80,
            1,
            &[],
            0,
            false,
            VersionSlotCols::NONE,
            slot,
        );
        assert_eq!(
            strip_caption_chrome_line(&segs, Some(frame), Some(CaptionButton::Close)),
            "strip-caption frame=client buttons=minimize:68..72,maximize:72..76,close:76..80 \
             hovered=close maximized=false edges=true menu=true"
        );
        let maximized = ClientFrame {
            maximized: true,
            edges: false,
            ..frame
        };
        let slot = CaptionSlot::measure(Some(maximized), 9, 1.0);
        let segs = crate::tab_bar::layout_segments_with_caption(
            80,
            1,
            &[],
            0,
            false,
            VersionSlotCols::NONE,
            slot,
        );
        assert_eq!(
            strip_caption_chrome_line(&segs, Some(maximized), None),
            "strip-caption frame=client buttons=minimize:68..72,restore:72..76,close:76..80 \
             hovered=none maximized=true edges=false menu=true"
        );
        assert_eq!(
            strip_caption_chrome_line(&[], Some(frame), None),
            "strip-caption frame=client buttons=none hovered=none maximized=false edges=true \
             menu=true",
            "no strip laid out"
        );
        assert_eq!(
            strip_caption_chrome_line(&segs, None, None),
            "strip-caption frame=system"
        );
    }

    #[test]
    fn ime_line_reports_the_composition_and_its_spot() {
        use super::ime_chrome_line;
        assert_eq!(
            ime_chrome_line("", None, None),
            "ime preedit=\"\" caret=- spot=-"
        );
        assert_eq!(
            ime_chrome_line("\u{4f60}\u{597d}", Some(6), Some((66, 36, 9, 20))),
            "ime preedit=\"\u{4f60}\u{597d}\" caret=6 spot=66,36,9x20"
        );
        assert_eq!(
            ime_chrome_line("ni hao\n", Some(0), Some((-4, 0, 1, 1))),
            "ime preedit=\"ni hao\\n\" caret=0 spot=-4,0,1x1"
        );
    }

    #[test]
    fn strip_version_line_reports_what_was_laid_out() {
        use super::{StripVersionAbsence, strip_version_chrome_line};
        use crate::menu::{VersionRung, strip_version_label};
        use crate::tab_bar::{TabHit, TabSegment};
        let slot = |rung: VersionRung, start_col: u16, end_col: u16| TabSegment {
            start_col,
            end_col,
            close_col: None,
            kind: TabHit::Version {
                rung,
                attention: false,
            },
            solo: false,
        };
        let chip = TabSegment {
            start_col: 0,
            end_col: 28,
            close_col: Some(26),
            kind: TabHit::Select(0),
            solo: false,
        };
        let full = strip_version_label(VersionRung::Full, false).expect("a full rung");
        assert_eq!(
            strip_version_chrome_line(
                &[chip, slot(VersionRung::Full, 110, 132)],
                false,
                StripVersionAbsence::StripFull,
                false
            ),
            format!(
                "strip-version shown=yes rung=full cols=110..132 text={full:?} \
                 attention=false hovered=false action=Version"
            )
        );
        let short = strip_version_label(VersionRung::Short, false).expect("a dev short rung");
        assert_eq!(
            strip_version_chrome_line(
                &[chip, slot(VersionRung::Short, 119, 132)],
                true,
                StripVersionAbsence::StripFull,
                false
            ),
            format!(
                "strip-version shown=yes rung=short cols=119..132 text={short:?} \
                 attention=false hovered=true action=Version"
            )
        );
        for (absent, reason) in [
            (StripVersionAbsence::NoStrip, "no-strip"),
            (StripVersionAbsence::NotLaidOut, "not-laid-out"),
            (StripVersionAbsence::NoUiFace, "no-ui-face"),
            (StripVersionAbsence::StripFull, "strip-full"),
        ] {
            assert_eq!(
                strip_version_chrome_line(&[chip], false, absent, false),
                format!(
                    "strip-version shown=no reason={reason} text={full:?} attention=false \
                     action=Version"
                )
            );
        }
        let marked = strip_version_label(VersionRung::Full, true).expect("a full rung");
        let action = crate::menu::version_slot_click(true);
        assert_eq!(
            strip_version_chrome_line(&[], false, StripVersionAbsence::StripFull, true),
            format!(
                "strip-version shown=no reason=strip-full text={marked:?} attention=true \
                 action={action:?}"
            )
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_strip_version_reason_reads_the_layout_it_reports() {
        let line = |app: &crate::App| {
            app.read_native_chrome()
                .into_iter()
                .find(|line| line.starts_with("strip-version "))
                .expect("Windows reports the strip's version label")
        };
        crate::tray_raster::clear_ui_fonts_for_test();
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 0;
        assert!(
            line(&app).contains("shown=no reason=no-strip"),
            "{}",
            line(&app)
        );
        app.tab_strip_rows = 1;
        assert!(
            line(&app).contains("shown=no reason=not-laid-out"),
            "nothing has laid the strip out yet: {}",
            line(&app)
        );
        app.splice_tab_strip_with(wid, 1);
        assert!(
            line(&app).contains("shown=no reason=no-ui-face"),
            "laid out before the UI face: {}",
            line(&app)
        );
        crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
        assert!(
            line(&app).contains("shown=no reason=no-ui-face"),
            "the face landing re-lays nothing by itself — the layout reported is \
             still the faceless one: {}",
            line(&app)
        );
        app.splice_tab_strip_with(wid, 1);
        assert!(
            line(&app).contains("shown=yes rung=full"),
            "the next frame lays the label out in the face: {}",
            line(&app)
        );
        app.windows.get_mut(&wid).expect("window").last_strip_fp = None;
        assert!(line(&app).contains("shown=yes rung=full"), "{}", line(&app));
        app.windows.get_mut(&wid).expect("window").cols = 32;
        app.splice_tab_strip_with(wid, 1);
        assert!(
            line(&app).contains("shown=no reason=strip-full"),
            "{}",
            line(&app)
        );
        app.windows
            .get_mut(&wid)
            .expect("window")
            .tab_segments
            .clear();
        assert!(
            line(&app).contains("shown=no reason=not-laid-out"),
            "{}",
            line(&app)
        );
        crate::tray_raster::clear_ui_fonts_for_test();
    }

    #[test]
    fn mac_window_stitch_keeps_fresh_client_and_outer_shape_alpha_only() {
        let (width, height) = (5_u32, 5_u32);
        let mut platform_alpha = vec![255_u8; (width * height) as usize];
        for (x, y) in [(0, 0), (4, 0), (0, 4), (4, 4)] {
            platform_alpha[(y * width + x) as usize] = 0;
        }
        for (x, y) in [
            (1, 0),
            (0, 1),
            (3, 0),
            (4, 1),
            (0, 3),
            (1, 4),
            (4, 3),
            (3, 4),
        ] {
            platform_alpha[(y * width + x) as usize] = 128;
        }
        platform_alpha[(2 * width + 2) as usize] = 31;

        let mut window = vec![0_u8; (width * height * 4) as usize];
        let mut fresh = Vec::with_capacity(window.len());
        for i in 0..(width * height) {
            fresh.extend_from_slice(&[(100 + i) as u8, 70, 40, 200]);
        }
        let client = CapturePixels::rgba8(width, height, fresh.clone()).expect("a 5x5 client");
        stitch_presented_client_into_window_rgba(
            &mut window,
            width,
            height,
            WindowClientRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            &client,
        )
        .unwrap();
        multiply_platform_outer_shape_alpha(&mut window, width, height, &platform_alpha).unwrap();

        let at = |x: u32, y: u32| ((y * width + x) * 4) as usize;
        for (x, y) in [(0, 0), (4, 0), (0, 4), (4, 4)] {
            let index = at(x, y);
            assert_eq!(
                &window[index..index + 3],
                &fresh[index..index + 3],
                "shape retention never restores stale platform RGB"
            );
            assert_eq!(window[index + 3], 0);
        }
        assert_eq!(window[at(1, 0) + 3], 100);
        let center = at(2, 2);
        assert_eq!(&window[center..center + 3], &fresh[center..center + 3]);
        assert_eq!(
            window[center + 3],
            200,
            "isolated platform alpha in an ordinary row is stale and ignored"
        );
    }

    #[test]
    fn mac_window_stitch_keeps_what_the_platform_did_not_photograph() {
        let (width, height) = (8_u32, 4_u32);
        let mut platform_alpha = vec![255_u8; (width * height) as usize];
        for y in 0..height {
            for x in 5..width {
                platform_alpha[(y * width + x) as usize] = 0;
            }
        }
        for y in [0, height - 1] {
            platform_alpha[(y * width) as usize] = 0;
            platform_alpha[(y * width + 1) as usize] = 128;
        }
        let mut window = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            window.extend_from_slice(&[10, 20, 30, 200]);
        }
        multiply_platform_outer_shape_alpha(&mut window, width, height, &platform_alpha).unwrap();
        let alpha = |x: u32, y: u32| window[((y * width + x) * 4 + 3) as usize];
        assert_eq!((alpha(6, 1), alpha(7, 2), alpha(5, 1)), (200, 200, 200));
        assert_eq!((alpha(7, 0), alpha(7, 3)), (0, 0));
        assert_eq!((alpha(6, 0), alpha(6, 3)), (100, 100));
        assert_eq!((alpha(0, 0), alpha(1, 0), alpha(2, 1)), (0, 100, 200));
    }

    #[test]
    fn sigusr_windowed_snapshot_roundtrips_exact_non_cell_destination_and_present_passes() {
        let expected = vec![
            11, 12, 13, 255, // semantic base
            90, 80, 70, 255, // present-only crown transformed pixel
            3, 4, 5, 255, // right remainder band
            21, 31, 41, 255, // semantic base
            210, 160, 120, 192, // present-only glow + alpha
            6, 7, 8, 255, // right remainder band
        ];
        let stride = 256;
        let mut mapped = vec![0xA5_u8; stride * 2];
        for (index, &[r, g, b, a]) in expected.as_chunks::<4>().0.iter().enumerate() {
            let at = (index / 3) * stride + (index % 3) * 4;
            mapped[at..at + 4].copy_from_slice(&[b, g, r, a]);
        }
        let raw =
            aterm_gpu::video_tap::RawCapture::bgra8_srgb_for_test(mapped.clone(), 3, 2, stride)
                .expect("a whole destination");
        let pixels = CapturePixels::raw(raw);
        assert_eq!(pixels.dims(), (3, 2));

        let encoded = CapturePng::encode(&pixels).expect("encodes");
        assert_eq!((encoded.width, encoded.height), (3, 2));
        let (decoded, w, h) = aterm_render::decode_png_rgba8(&encoded.png).expect("decodes");
        assert_eq!((w, h), (3, 2));
        assert_eq!(
            decoded, expected,
            "the snapshot cannot rerender, crop, or recolor the exact destination"
        );

        let short = mapped[..stride + 3 * 4 - 1].to_vec();
        assert!(
            aterm_gpu::video_tap::RawCapture::bgra8_srgb_for_test(short, 3, 2, stride).is_err()
        );
    }

    #[test]
    fn cpu_softbuffer_capture_is_its_rgb_and_opaque() {
        let pixels = CapturePixels::softbuffer(vec![0x7f11_2233, 0xff44_5566], 2, 1)
            .expect("a well-shaped softbuffer");
        let mut row = Vec::new();
        pixels.rgba_row(0, &mut row);
        assert_eq!(
            row,
            [0x11, 0x22, 0x33, 255, 0x44, 0x55, 0x66, 255],
            "softbuffer ignores renderer transmittance and presents opaque RGB"
        );
        assert!(CapturePixels::softbuffer(vec![0], 2, 1).is_err());
        assert!(CapturePixels::softbuffer(vec![0; 3], 2, 1).is_err());
    }

    fn packed_fixture(width: usize, height: usize) -> Frame {
        Frame {
            width,
            height,
            pixels: (0..width * height)
                .map(|i| {
                    let i = i as u32;
                    (((i * 37) % 256) << 24)
                        | (((i * 11) % 256) << 16)
                        | (((i * 5 + 3) % 256) << 8)
                        | ((i * 3 + 1) % 256)
                })
                .collect(),
        }
    }

    fn every_form(frame: &Frame) -> Vec<(&'static str, CapturePixels)> {
        let (width, height) = (frame.width, frame.height);
        let rgba = frame.rgba_bytes();
        let stride = (width * 4).div_ceil(256) * 256;
        let mut mapped = vec![0x5A_u8; stride * height];
        for (index, &[r, g, b, a]) in rgba.as_chunks::<4>().0.iter().enumerate() {
            let at = (index / width) * stride + (index % width) * 4;
            mapped[at..at + 4].copy_from_slice(&[b, g, r, a]);
        }
        let raw = aterm_gpu::video_tap::RawCapture::bgra8_srgb_for_test(
            mapped,
            width as u32,
            height as u32,
            stride,
        )
        .expect("a whole mapping");
        let mut forms = vec![
            (
                "rgba8",
                CapturePixels::rgba8(width as u32, height as u32, rgba).expect("straight"),
            ),
            (
                "packed",
                CapturePixels::packed(frame.clone()).expect("packed"),
            ),
            ("raw", CapturePixels::raw(raw)),
        ];
        if frame.pixels.iter().all(|pixel| pixel >> 24 == 0) {
            forms.push((
                "softbuffer",
                CapturePixels::softbuffer(frame.pixels.clone(), width, height).expect("softbuffer"),
            ));
        }
        forms
    }

    #[test]
    fn packed_frame_encodes_like_its_rgba_bytes() {
        for (width, height) in [(1usize, 1usize), (3, 2), (37, 29)] {
            let frame = packed_fixture(width, height);
            let rgba = frame.rgba_bytes();
            let forms = every_form(&frame);
            let packed = CapturePng::encode(&forms[1].1).expect("packed encodes");
            for (form, pixels) in &forms {
                let png = CapturePng::encode(pixels).expect("encodes");
                assert_eq!(png.png, packed.png, "{width}x{height} {form}");
            }
            for pixels in [width * height + 1, width * height - 1] {
                let mut wrong = frame.clone();
                wrong.pixels.resize(pixels, 0);
                let mut bytes = rgba.clone();
                bytes.resize(pixels * 4, 0);
                assert!(
                    CapturePixels::packed(wrong).is_err()
                        && CapturePixels::rgba8(width as u32, height as u32, bytes).is_err(),
                    "{width}x{height}: {pixels} pixels are refused in both forms"
                );
            }
            let (decoded, w, h) = aterm_render::decode_png_rgba8(&packed.png).expect("decodes");
            assert_eq!((w, h, decoded), (width, height, rgba), "{width}x{height}");

            let mut nudged = frame;
            nudged.pixels[width * height / 2] ^= 1;
            let other = CapturePixels::packed(nudged)
                .and_then(|pixels| CapturePng::encode(&pixels))
                .expect("encodes");
            assert_ne!(
                other.png, packed.png,
                "{width}x{height}: one channel differs"
            );
        }
    }

    #[test]
    fn rgba8_and_packed_pixels_share_one_fingerprint() {
        for (width, height) in [(1usize, 1usize), (32, 32), (37, 29), (1031, 3)] {
            for opaque in [false, true] {
                let mut frame = packed_fixture(width, height);
                if opaque {
                    frame
                        .pixels
                        .iter_mut()
                        .for_each(|pixel| *pixel &= 0x00FF_FFFF);
                }
                let expected = crate::App::image_pixel_fingerprint(width, height, &frame.pixels);
                for (form, pixels) in every_form(&frame) {
                    assert_eq!(
                        pixels.pixel_fingerprint(),
                        expected,
                        "{width}x{height} {form}"
                    );
                }
                let mut rgba = frame.rgba_bytes();
                rgba[3] ^= 1;
                let nudged = CapturePixels::rgba8(width as u32, height as u32, rgba)
                    .expect("straight")
                    .pixel_fingerprint();
                assert_ne!(nudged, expected, "{width}x{height}: one alpha step");
            }
        }
    }

    #[test]
    fn a_cut_capture_is_its_rect() {
        let frame = packed_fixture(9, 7);
        let rgba = frame.rgba_bytes();
        let rect = aterm_render::PixelRect {
            x: 2,
            y: 1,
            width: 5,
            height: 4,
        };
        let own = |rect: aterm_render::PixelRect| {
            let bytes: Vec<u8> = (rect.y..rect.y + rect.height)
                .flat_map(|y| {
                    let start = (y * 9 + rect.x) * 4;
                    rgba[start..start + rect.width * 4].to_vec()
                })
                .collect();
            CapturePixels::rgba8(rect.width as u32, rect.height as u32, bytes).expect("own")
        };
        let expected = own(rect);
        let expected_png = CapturePng::encode(&expected).expect("encodes").png;
        for (form, pixels) in every_form(&frame) {
            let cut = pixels
                .cut_to(aterm_render::PixelRect {
                    x: 1,
                    y: 0,
                    width: 7,
                    height: 6,
                })
                .and_then(|pixels| {
                    pixels.cut_to(aterm_render::PixelRect {
                        x: 1,
                        y: 1,
                        width: 5,
                        height: 4,
                    })
                })
                .expect("inside");
            assert_eq!(cut.dims(), (5, 4), "{form}");
            assert_eq!(
                CapturePng::encode(&cut).expect("encodes").png,
                expected_png,
                "{form}"
            );
            assert_eq!(
                cut.pixel_fingerprint(),
                expected.pixel_fingerprint(),
                "{form}"
            );
            let past = cut.cut_to(aterm_render::PixelRect {
                x: 1,
                y: 0,
                width: 5,
                height: 4,
            });
            assert!(past.is_err(), "{form}: a rect past the pixels");
        }
        let shifted = own(aterm_render::PixelRect { x: 3, ..rect });
        assert_ne!(
            shifted.pixel_fingerprint(),
            expected.pixel_fingerprint(),
            "one pixel right is other pixels"
        );
    }

    #[test]
    fn window_stitch_uses_explicit_asymmetric_client_origin() {
        let mut window = vec![9_u8; 5 * 4 * 4];
        let rgba: Vec<u8> = (0_u8..16).collect();
        let client = CapturePixels::rgba8(2, 2, rgba.clone()).expect("a 2x2 client");
        stitch_presented_client_into_window_rgba(
            &mut window,
            5,
            4,
            WindowClientRect {
                x: 2,
                y: 1,
                width: 2,
                height: 2,
            },
            &client,
        )
        .unwrap();
        let at = |row: usize, col: usize| (row * 5 + col) * 4;
        assert_eq!(&window[at(1, 2)..at(1, 4)], &rgba[..8]);
        assert_eq!(&window[at(2, 2)..at(2, 4)], &rgba[8..]);
        assert!(window[..at(1, 2)].iter().all(|&byte| byte == 9));
        assert!(window[at(3, 0)..].iter().all(|&byte| byte == 9));
    }

    #[test]
    fn window_stitch_and_unpremultiply_fail_closed_and_cover_alpha_endpoints() {
        let honest = CapturePixels::rgba8(2, 1, vec![0; 8]).expect("a 2x1 client");
        let rect = WindowClientRect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        assert!(
            stitch_presented_client_into_window_rgba(&mut [0; 7], 2, 1, rect, &honest).is_err()
        );
        let wrong_shape = CapturePixels::rgba8(1, 1, vec![0; 4]).expect("a 1x1 client");
        assert!(
            stitch_presented_client_into_window_rgba(&mut [0; 8], 2, 1, rect, &wrong_shape)
                .is_err()
        );
        let outside = WindowClientRect { x: 1, ..rect };
        assert!(
            stitch_presented_client_into_window_rgba(&mut [0; 8], 2, 1, outside, &honest).is_err()
        );

        let mut premultiplied = vec![
            9, 8, 7, 0, // transparent normalizes colour
            64, 32, 16, 128, // half-alpha unassociates to approximately 128/64/32
            4, 5, 6, 255, // opaque is byte-identical
        ];
        unpremultiply_rgba8(&mut premultiplied);
        assert_eq!(premultiplied, [0, 0, 0, 0, 128, 64, 32, 128, 4, 5, 6, 255]);
    }

    #[test]
    fn appkit_chrome_uses_straight_alpha_source_over_and_exact_placement() {
        let mut destination = vec![
            9, 8, 7, 255, // untouched left
            0, 0, 255, 128, // translucent blue beneath chrome
            4, 5, 6, 255, // transparent source leaves this exact
            1, 2, 3, 255, // second row untouched
            1, 2, 3, 255, //
            1, 2, 3, 255, //
        ];
        let chrome = RgbaOverlay {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
            rgba: vec![
                255, 0, 0, 128, // half-red over half-blue
                200, 100, 50, 0, // transparent payload must not alter destination
            ],
        };
        let clip = WindowClientRect {
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        };
        composite_straight_rgba_overlay(&mut destination, 3, 2, &chrome, clip).unwrap();
        assert_eq!(&destination[..4], &[9, 8, 7, 255]);
        assert_eq!(
            &destination[4..8],
            &[170, 0, 85, 192],
            "straight-alpha source-over must weight both source and destination alpha"
        );
        assert_eq!(&destination[8..12], &[4, 5, 6, 255]);
        let (second_row, _) = destination[12..].as_chunks::<4>();
        assert!(second_row.iter().all(|px| *px == [1, 2, 3, 255]));
    }

    #[test]
    fn appkit_chrome_is_clipped_to_replaced_client_and_fails_closed_on_bad_shape() {
        let chrome = RgbaOverlay {
            x: 0,
            y: 0,
            width: 3,
            height: 1,
            rgba: vec![11, 12, 13, 255, 21, 22, 23, 255, 31, 32, 33, 255],
        };
        let clip = WindowClientRect {
            x: 1,
            y: 0,
            width: 1,
            height: 1,
        };
        let mut destination = vec![0_u8; 3 * 2 * 4];
        composite_straight_rgba_overlay(&mut destination, 3, 2, &chrome, clip).unwrap();
        assert_eq!(&destination[..4], &[0, 0, 0, 0]);
        assert_eq!(&destination[4..8], &[21, 22, 23, 255]);
        assert_eq!(&destination[8..], &[0; 16]);

        let wrong_buffer = RgbaOverlay {
            rgba: vec![0; 11],
            ..chrome.clone()
        };
        assert!(
            composite_straight_rgba_overlay(&mut destination, 3, 2, &wrong_buffer, clip).is_err()
        );
        let outside = RgbaOverlay {
            x: 2,
            width: 2,
            rgba: vec![0; 8],
            ..chrome
        };
        assert!(composite_straight_rgba_overlay(&mut destination, 3, 2, &outside, clip).is_err());
        let invalid_clip = WindowClientRect {
            x: 3,
            width: 1,
            ..clip
        };
        let inside = RgbaOverlay {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        };
        assert!(
            composite_straight_rgba_overlay(&mut destination, 3, 2, &inside, invalid_clip).is_err()
        );
    }

    fn bitmap_of(pixels: &[[u8; 4]; 6], offsets: [usize; 4], bytes_per_row: usize) -> Vec<u8> {
        let mut bytes = vec![0xC3_u8; bytes_per_row * 2];
        for (index, pixel) in pixels.iter().enumerate() {
            let at = (index / 3) * bytes_per_row + (index % 3) * 4;
            for (channel, offset) in offsets.iter().enumerate() {
                bytes[at + offset] = pixel[channel];
            }
        }
        bytes
    }

    #[test]
    fn normalize_bitmap_covers_every_supported_layout() {
        let premultiplied = [
            [0x40, 0x20, 0x10, 0x80],
            [1, 2, 3, 255],
            [9, 8, 7, 0],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0x10, 0x08, 0x04, 0x20],
        ];
        let unassociated = [
            128, 64, 32, 128, 1, 2, 3, 255, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 128, 64,
            32, 32,
        ];
        let straight: Vec<u8> = premultiplied.iter().flatten().copied().collect();
        let layouts = [
            (1, 0, [0, 1, 2, 3]),       // premultiplied last: R G B A
            (1, 4 << 12, [0, 1, 2, 3]), // the same, 32-bit big-endian
            (1, 2 << 12, [3, 2, 1, 0]), // little-endian: A B G R
            (2, 0, [1, 2, 3, 0]),       // premultiplied first: A R G B
            (2, 2 << 12, [2, 1, 0, 3]), // little-endian: B G R A
            (3, 0, [0, 1, 2, 3]),       // straight last
            (3, 2 << 12, [3, 2, 1, 0]),
            (4, 0, [1, 2, 3, 0]), // straight first
            (4, 2 << 12, [2, 1, 0, 3]),
        ];
        let desc = |bitmap_info: u32| BitmapDesc {
            width: 3,
            height: 2,
            bytes_per_row: 16,
            bits_per_component: 8,
            bits_per_pixel: 32,
            bitmap_info,
            rgb: true,
        };
        for (alpha, order, offsets) in layouts {
            let bytes = bitmap_of(&premultiplied, offsets, 16);
            let expected = if alpha <= 2 {
                unassociated.to_vec()
            } else {
                straight.clone()
            };
            assert_eq!(
                normalize_bitmap(&bytes, desc(alpha | order)),
                Ok(expected),
                "alpha info {alpha}, byte order {order:#x}"
            );
        }

        let bytes = bitmap_of(&premultiplied, [0, 1, 2, 3], 16);
        let sixteen = BitmapDesc {
            bits_per_component: 16,
            bits_per_pixel: 64,
            bytes_per_row: 32,
            ..desc(1)
        };
        for (label, refused) in [
            ("16-bit", sixteen),
            ("float", desc(1 | 1 << 8)),
            ("no alpha", desc(5)),
            ("16-bit byte order", desc(1 | 1 << 12)),
            (
                "not RGB",
                BitmapDesc {
                    rgb: false,
                    ..desc(1)
                },
            ),
            (
                "row narrower than its pixels",
                BitmapDesc {
                    bytes_per_row: 8,
                    ..desc(1)
                },
            ),
        ] {
            assert!(refused.direct_layout().is_err(), "{label}");
            assert!(normalize_bitmap(&bytes, refused).is_err(), "{label}");
        }
        assert!(
            normalize_bitmap(&bytes[..16 + 11], desc(1)).is_err(),
            "short of its last row"
        );
    }

    #[cfg(target_os = "macos")]
    fn srgb_chrome_rep(
        (width, height): (isize, isize),
        bits: isize,
        straight: bool,
        tagged_srgb: bool,
        fill: impl FnOnce(&mut [u8], usize),
    ) -> (aterm_objc::Obj, aterm_objc::Id) {
        use aterm_objc::{Bool, Id, Obj, Sel, class, sel};

        use crate::appkit;
        use crate::appkit::consts::NS_COLOR_RENDERING_INTENT_PERCEPTUAL;

        const ALPHA_NONPREMULTIPLIED: usize = 1 << 1;
        type Init = unsafe extern "C-unwind" fn(
            Id,
            Sel,
            *mut *mut u8,
            isize,
            isize,
            isize,
            isize,
            Bool,
            Bool,
            Id,
            usize,
            isize,
            isize,
        ) -> Id;
        let space = appkit::nsstring("NSDeviceRGBColorSpace").expect("a colour space name");
        // SAFETY: `+alloc` and designated `NSBitmapImageRep` initializer with NULL planes allocate an owned buffer.
        let rep = unsafe {
            let init: Init = aterm_objc::checked_msg();
            let alloc = appkit::send_id(class(c"NSBitmapImageRep").as_id(), sel!(alloc));
            Obj::from_owned(init(
                alloc,
                sel!(initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bitmapFormat:bytesPerRow:bitsPerPixel:),
                std::ptr::null_mut(),
                width,
                height,
                bits,
                4,
                Bool::YES,
                Bool::NO,
                space.id(),
                if straight { ALPHA_NONPREMULTIPLIED } else { 0 },
                0,
                0,
            ))
        }
        .expect("an NSBitmapImageRep");
        // SAFETY: `-bytesPerRow` and `-bitmapData` on the live `rep` owning `bytesPerRow * height` bytes.
        let (plane, row) = unsafe {
            let data: unsafe extern "C-unwind" fn(Id, Sel) -> *mut u8 = aterm_objc::checked_msg();
            let row = usize::try_from(appkit::send_isize(rep.id(), sel!(bytesPerRow)))
                .expect("a row length");
            let rows = usize::try_from(height).expect("a height");
            (
                std::slice::from_raw_parts_mut(data(rep.id(), sel!(bitmapData)), row * rows),
                row,
            )
        };
        fill(plane, row);
        // SAFETY: `+sRGBColorSpace` and color-space conversion sends on the live `NSBitmapImageRep`.
        let converted = unsafe {
            let srgb = appkit::send_id(class(c"NSColorSpace").as_id(), sel!(sRGBColorSpace));
            let source = if tagged_srgb {
                appkit::send_id_id(
                    rep.id(),
                    sel!(bitmapImageRepByRetaggingWithColorSpace:),
                    srgb,
                )
            } else {
                rep.id()
            };
            appkit::send_id_id_isize(
                source,
                sel!(bitmapImageRepByConvertingToColorSpace:renderingIntent:),
                srgb,
                NS_COLOR_RENDERING_INTENT_PERCEPTUAL,
            )
        };
        assert!(!converted.is_null(), "AppKit converts the rep to sRGB");
        (rep, converted)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_appkit_chrome_bitmap_is_copied_as_its_png_would_read() {
        use super::{ChromeLayer, ChromePixels, chrome_pixels_of, chrome_png_of};

        aterm_objc::autoreleasepool(|_| {
            let (_rep, converted) = srgb_chrome_rep((256, 256), 8, false, false, |plane, row| {
                for alpha in 0..256 {
                    for x in 0..256 {
                        let colour = x * alpha / 255;
                        let pixel = &mut plane[alpha * row + x * 4..][..4];
                        pixel.copy_from_slice(&[
                            colour as u8,
                            (colour * 3 % (alpha + 1)) as u8,
                            (alpha - colour) as u8,
                            alpha as u8,
                        ]);
                    }
                }
            });
            let (pixels, width, height) = chrome_pixels_of(converted).expect("the chrome bytes");
            assert_eq!((width, height), (256, 256));
            let ChromePixels::Bitmap { bytes, desc } = pixels else {
                panic!("an 8-bit sRGB rep is copied directly, not re-encoded");
            };
            let png = chrome_png_of(converted).expect("AppKit's PNG of the rep");
            let (expected, png_width, png_height) =
                decode_native_chrome_png(&png).expect("the PNG decodes");
            assert_eq!((png_width, png_height), (width, height));
            assert!(
                expected
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| (1..255).contains(&pixel[3])),
                "the rep has translucent pixels"
            );
            let as_straight = BitmapDesc {
                bitmap_info: (desc.bitmap_info & !0x1F) | 3,
                ..desc
            };
            assert_ne!(
                normalize_bitmap(&bytes, as_straight).expect("a straight reading"),
                expected,
                "negative control: the copy's premultiplication matters"
            );
            let overlay = ChromeLayer {
                x: 0,
                y: 0,
                width,
                height,
                pixels: ChromePixels::Bitmap { bytes, desc },
            }
            .overlay()
            .expect("the overlay");
            assert_eq!(overlay.rgba.len(), expected.len());
            let first_difference = overlay
                .rgba
                .iter()
                .zip(&expected)
                .position(|(copied, png)| copied != png);
            assert_eq!(
                first_difference, None,
                "the direct copy reads as AppKit's PNG of the rep"
            );
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_sixteen_bit_appkit_chrome_bitmap_goes_through_its_png() {
        use super::{ChromeLayer, ChromePixels, chrome_pixels_of};

        let samples: Vec<u16> = (0..3 * 2 * 4).map(|i| 0x1234 + i * 0x0A0B).collect();
        aterm_objc::autoreleasepool(|_| {
            let (_rep, converted) = srgb_chrome_rep((3, 2), 16, false, false, |_, _| {});
            let (pixels, ..) = chrome_pixels_of(converted).expect("the chrome bytes");
            assert!(
                matches!(pixels, ChromePixels::Png(_)),
                "a converted 16-bit rep stays 16-bit and is not read directly"
            );
            let (_rep, converted) = srgb_chrome_rep((3, 2), 16, true, true, |plane, row| {
                for (pixel, rgba) in samples.chunks(4).enumerate() {
                    let at = pixel / 3 * row + pixel % 3 * 8;
                    for (channel, sample) in rgba.iter().enumerate() {
                        plane[at + channel * 2..][..2].copy_from_slice(&sample.to_ne_bytes());
                    }
                }
            });
            let (pixels, width, height) = chrome_pixels_of(converted).expect("the chrome bytes");
            assert_eq!((width, height), (3, 2));
            assert!(
                matches!(pixels, ChromePixels::Png(_)),
                "a 16-bit rep is not read directly"
            );
            let overlay = ChromeLayer {
                x: 0,
                y: 0,
                width,
                height,
                pixels,
            }
            .overlay()
            .expect("the PNG decodes");
            let high: Vec<u8> = samples
                .iter()
                .map(|sample| sample.to_be_bytes()[0])
                .collect();
            assert_eq!(overlay.rgba, high);
        });
        assert!(decode_native_chrome_png(&[0; 16]).is_err());
    }

    #[test]
    fn bgrx_photograph_becomes_opaque_rgba() {
        let mut pixels = vec![1, 2, 3, 99, 4, 5, 6, 0];
        bgrx_to_opaque_rgba8(&mut pixels);
        assert_eq!(pixels, [3, 2, 1, 255, 6, 5, 4, 255]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_window_is_stitched_on_the_encode_worker() {
        use super::{
            ChromeLayer, ChromePixels, EncodeJob, PlatformShot, ShotEncoding, WindowStitch,
        };

        let (width, height) = (4_u32, 3_u32);
        let stitch = || {
            let mut photograph = Vec::new();
            for index in 0..width * height {
                let corner = [0, 3, 8, 11].contains(&index);
                photograph.extend(if corner { [0; 4] } else { [9, 9, 9, 255] });
            }
            let client: Vec<u32> = (0..width * height).map(|i| 0x0010_2030 + i).collect();
            let mut chrome = vec![0x80, 0, 0x80, 0x80, 3, 2, 1, 255];
            chrome.extend([0xEE; 4]);
            WindowStitch {
                shot: PlatformShot {
                    pixels: photograph,
                    width,
                    height,
                    encoding: ShotEncoding::PremultipliedRgba,
                    chrome: Some(ChromeLayer {
                        x: 1,
                        y: 0,
                        width: 2,
                        height: 1,
                        pixels: ChromePixels::Bitmap {
                            bytes: chrome,
                            desc: BitmapDesc {
                                width: 2,
                                height: 1,
                                bytes_per_row: 12,
                                bits_per_component: 8,
                                bits_per_pixel: 32,
                                bitmap_info: 2 | 2 << 12,
                                rgb: true,
                            },
                        },
                    }),
                },
                client: CapturePixels::softbuffer(client, 4, 3).expect("a 4x3 client"),
                rect: WindowClientRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
            }
        };
        let dir = std::env::temp_dir().join(format!(
            "aterm-window-stitch-worker-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().expect("clock").as_nanos()
        ));
        crate::control_auth::ensure_private_dir(&dir).expect("capture dir");
        let mut app = crate::App::headless_for_test();
        let (reply, rx) = std::sync::mpsc::channel();
        app.submit_encode_job(EncodeJob::Window {
            stitch: stitch(),
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::for_test(&dir, "window.png"),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        let answer = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the window job answers")
            .expect("the window job succeeds");
        assert_eq!(answer.value, (width, height));
        let png = std::fs::read(dir.join("window.png")).expect("the written PNG");
        drop(answer);
        let here = std::thread::current().id();
        assert!(!super::ran_pixel_work(here), "the stitch ran on the worker");

        let (rgba, w, h) = aterm_render::decode_png_rgba8(&png).expect("a PNG");
        assert_eq!((w, h), (4, 3));
        let at = |x: usize, y: usize| &rgba[(y * 4 + x) * 4..][..4];
        assert_eq!(
            at(0, 0)[3],
            0,
            "the photograph's corner is transparent again"
        );
        assert_eq!(at(1, 1), [0x10, 0x20, 0x35, 255], "a client pixel");
        assert_eq!(at(2, 0), [1, 2, 3, 255], "the opaque chrome pixel");
        assert_eq!(
            at(1, 0)[3],
            255,
            "the translucent chrome is over an opaque client"
        );
        assert_ne!(at(1, 0), [0x10, 0x20, 0x31, 255], "and blended into it");

        let here_pixels = stitch().finish().expect("finishes");
        assert!(super::ran_pixel_work(here));
        let here_png = CapturePng::encode(&here_pixels).expect("encodes");
        assert_eq!(here_png.png, png);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
#[path = "capture_barrier_tests.rs"]
mod capture_barrier_tests;

#[cfg(test)]
mod terminal_split_capture_tests {
    use super::{App, image_req_for_test, terminal_capture_text};
    use crate::{WindowId, control_auth, term_lock};
    use std::time::{Duration, Instant};

    fn split_fixture() -> (App, WindowId, u64) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let right = app.split_active_stub_tab(wid);
        let left_term = app.pool.get(0).expect("left pane").term.clone();
        let right_term = app.pool.get(right).expect("right pane").term.clone();
        term_lock(&left_term).process(b"\x1b]11;#102030\x07LEFT");
        term_lock(&right_term).process(b"\x1b]11;#405060\x07RIGHT");
        (app, wid, right)
    }

    fn confined(dir: &std::path::Path, name: &str) -> control_auth::ConfinedImage {
        control_auth::ConfinedImage::for_test(dir, name)
    }

    #[test]
    fn recording_image_keeps_the_exact_successful_present_cursor_layers() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let font_px = app.windows[&wid].metrics.font_px;
        let single = app
            .prepare_terminal_capture_grid(wid)
            .expect("single-pane model");
        assert!(
            !single.composed,
            "regression covers the normal one-pane route"
        );
        let capture_leaf_snapshot_seqs: Vec<_> =
            single.leaves.iter().map(|leaf| leaf.snapshot_seq).collect();
        let capture_leaf_content_seqs: Vec<_> =
            single.leaves.iter().map(|leaf| leaf.content_seq).collect();
        assert_eq!(capture_leaf_snapshot_seqs.len(), 1);
        let window = app.windows.get_mut(&wid).unwrap();
        window.capture_leaf_snapshot_seqs = capture_leaf_snapshot_seqs;
        window.capture_leaf_content_seqs = capture_leaf_content_seqs;

        let mut gpu = match aterm_gpu::GpuRenderer::new(font_px, app.theme) {
            Ok(gpu) => gpu,
            Err(error) => {
                aterm_spec::host::could_not_run(format_args!(
                    "no GPU renderer on this host: {error}"
                ));
            }
        };
        gpu.set_pad(12);
        gpu.set_pad_top(2);
        assert_eq!((gpu.pad(), gpu.pad_top()), (12, 2));
        app.backend = crate::BackendSlot::Ready(crate::Backend::Gpu(crate::GpuBackend::new(gpu)));
        app.bind_window_renderer_state(wid);
        let (input_rows, input_cols) = {
            let input = &app.windows[&wid].input_scratch;
            (input.rows, input.cols)
        };
        let (width, height) = app.backend.frame_size(input_rows, input_cols);
        let (_, cell_h) = app.backend.cell_size();
        let mut window_gpu = aterm_gpu::WindowGpu::new();
        app.backend
            .gpu_mut()
            .expect("installed GPU fixture")
            .virtual_begin(
                &mut window_gpu,
                u32::try_from(width).expect("fixture width"),
                u32::try_from(height).expect("fixture height"),
                aterm_gpu::video_tap::CaptureOpts {
                    half_res: false,
                    budget_bytes: aterm_gpu::video_tap::DEFAULT_BUDGET,
                    fps_cap: None,
                    requested_ms: 0,
                },
            )
            .expect("real virtual recording target");
        app.windows.get_mut(&wid).unwrap().present =
            Some(crate::PresentTarget::Virtual { window_gpu });

        let root = std::env::temp_dir().join(format!(
            "aterm-recording-image-retain-{}-{}",
            std::process::id(),
            crate::metrics::now_us()
        ));
        let _ = std::fs::remove_dir_all(&root);
        control_auth::ensure_private_dir(&root).expect("private capture root");
        let video_dir = control_auth::confine_video_dir(&root).expect("recording directory");
        let (video_reply, _video_rx) = std::sync::mpsc::channel();
        app.video_rec = Some(crate::VideoRec {
            window: wid,
            deadline: Instant::now() + Duration::from_secs(3_600),
            started_us: 0,
            keys: false,
            key_log: Vec::new(),
            pace_ticks: Vec::new(),
            unseamed_at_begin: 0,
            unlogged_other_window: 0,
            unlogged_over_cap: 0,
            mode: crate::VideoMode::OffscreenPresentReal,
            next_frame: None,
            presented: None,
            dir: video_dir,
            handoff: None,
            cancel: crate::VideoCancellation::new(),
            reply: video_reply,
        });

        let (early_reply, early_rx) = std::sync::mpsc::channel();
        app.render_image(image_req_for_test(
            confined(&root, "too-early.png"),
            false,
            true,
            None,
            early_reply,
        ));
        early_rx
            .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
            .expect("pre-baseline image reply")
            .expect_err("a recording with no successful frame must defer");

        let trail_color = 0x00FA_01B5;
        let kitty_color = [0x22, 0xDD, 0x66, 0xFF];
        {
            let window = app.windows.get_mut(&wid).expect("recorded window");
            window.input_scratch.cursor_trail = vec![aterm_render::TrailCell {
                row: 4,
                col: 12,
                alpha: 255,
            }];
            window.input_scratch.cursor_trail_color = trail_color;
            window.input_scratch.cursor_glow_add = vec![aterm_render::GlowQuad {
                row: 4,
                x: 1,
                y: 1,
                w: 1,
                h: 1,
                color: 0x0001_0101,
                alpha: 0,
                color2: 0x0001_0101,
                alpha2: 0,
            }];
            let kitty = aterm_render::fx::bitmap(
                aterm_core::render::FxTexels::Rgba8Straight,
                2,
                2,
                kitty_color.repeat(4),
            )
            .expect("a 2x2 kitty art");
            aterm_render::sprites::SpriteLayers::new(aterm_render::sprites::WORD_SPRITE_SALT)
                .publish(
                    &[aterm_render::sprites::FreeSprite::whole(
                        30,
                        i32::try_from(6 * cell_h).expect("fixture kitty y"),
                        kitty,
                    )],
                    &mut window.input_scratch.fx_layers,
                );
        }
        app.bind_window_renderer_state(wid);
        app.present_input_scratch_for_test(wid, false, None)
            .expect("the real virtual submission succeeds");
        let route = app.active_visible_content_route(wid).unwrap();
        app.finalize_successful_terminal_present_for_test(
            wid,
            route,
            crate::app_render::HostVisualState::default(),
        );
        let successful_serial = app.windows[&wid].capture_present_serial;
        app.windows.get_mut(&wid).unwrap().last_present = None;

        app.prepare_terminal_capture_grid(wid)
            .expect("interleaved terminal snapshot");
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.capture_leaf_snapshot_seqs.clear();
            window.capture_leaf_content_seqs.clear();
            let scratch = &mut window.input_scratch;
            scratch.cursor_trail.clear();
            scratch.cursor_glow_add.clear();
            scratch
                .fx_layers
                .retain(|l| aterm_render::sprites::FreeSprite::of_layer(l).is_none());
        }
        let retained = app
            .recording_presented_frame_capture(wid, true, false)
            .expect("resident successful frame survives scratch interleave");
        assert_eq!(retained.input.cursor_trail.len(), 1);
        assert_eq!(retained.input.cursor_trail_color, trail_color);
        assert_eq!(retained.input.cursor_glow_add.len(), 1);
        assert_eq!(
            retained.input.cursor_glow_add[0].y, 1,
            "resident semantic capture undoes the nonzero pad transport shift"
        );
        assert_eq!(
            aterm_render::sprites::published(&retained.input.fx_layers).len(),
            1
        );
        drop(retained);

        app.windows.get_mut(&wid).unwrap().blink_phase = false;
        assert!(
            app.recording_presented_frame_capture(wid, true, false)
                .is_err(),
            "renderer-state drift must defer rather than reraster old input with a new blink phase"
        );
        assert!(
            app.recording_presented_destination_capture(wid).is_ok(),
            "live renderer drift cannot invalidate already-presented destination pixels"
        );
        app.windows.get_mut(&wid).unwrap().blink_phase = true;

        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(image_req_for_test(
            confined(&root, "retained.png"),
            false,
            true,
            Some(&metadata),
            reply,
        ));
        let (_, _, png) = rx
            .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
            .expect("image worker reply")
            .expect("recording still succeeds")
            .value;
        let png = png.expect("the retained model is encoded");
        let (rgba, _, _) = aterm_render::decode_png_rgba8(&png).expect("encoded RGBA8");
        let expected = [0xFA, 0x01, 0xB5];
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[..3] == expected),
            "the exact encoded pixels retain the opaque rainbow-trail sentinel"
        );
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel.as_slice() == kitty_color.as_slice()),
            "the exact encoded pixels retain the free-sprite/kitty sentinel"
        );
        assert_eq!(
            metadata.get().expect("image metadata").capture_serial,
            successful_serial,
            "the still identifies the same successful present as the recording"
        );
        let staged = &app.windows[&wid].input_scratch;
        assert!(staged.cursor_trail.is_empty());
        assert!(staged.cursor_glow_add.is_empty());

        let scratch_bg = 0x0013_57A9;
        app.windows.get_mut(&wid).unwrap().input_scratch.default_bg = scratch_bg;
        let plain_still = |app: &mut App, name: &str| {
            let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
            let (reply, rx) = std::sync::mpsc::channel();
            app.render_image(crate::control::ImageReq {
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target: confined(&root, name),
                clean: true,
                session: None,
                want_bytes: true,
                want_metadata: true,
                frame_metadata: std::sync::Arc::clone(&metadata),
                cancel: crate::control::CaptureCancellation::new(),
                reply,
            });
            let (_, _, png) = rx
                .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
                .expect("plain still reply")
                .expect("the plain recording still succeeds")
                .value;
            let (rgba, width, height) =
                aterm_render::decode_png_rgba8(&png.expect("the plain still is encoded"))
                    .expect("encoded RGBA8");
            let pixels: Vec<u32> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
                .collect();
            let serial = metadata.get().expect("plain still metadata").capture_serial;
            (width, height, pixels, serial)
        };
        let (width, height, absent, serial) = plain_still(&mut app, "plain.png");
        assert_eq!(
            serial, successful_serial,
            "the plain still is bound to the recorded present"
        );
        assert!(
            !absent.contains(&trail_color),
            "the plain still strips the rainbow-trail sentinel"
        );
        assert!(
            !absent.contains(&scratch_bg),
            "the plain still rasterizes the resident model, not the newer scratch"
        );
        assert_eq!(
            app.recording_presented_frame_capture(wid, true, false)
                .expect("resident frame after the plain still")
                .input
                .cursor_trail
                .len(),
            1,
            "the strip took an owned copy; the resident model keeps its trail"
        );

        app.level_up = Some(crate::level_up::LevelUp::charging_continued(
            9,
            Instant::now(),
            0.0,
        ));
        let overlay = app
            .host_visual_state(wid, Instant::now())
            .overlay
            .expect("a live glow");
        let (_, _, glowing, serial) = plain_still(&mut app, "plain-glow.png");
        assert_eq!(serial, successful_serial);
        assert_eq!(
            glowing[0], absent[0],
            "a retained None overlay is authoritative: the live glow is not resampled"
        );
        app.level_up = None;

        let mut resampled = absent.clone();
        crate::app_render::apply_overlay_at(
            &mut resampled,
            width,
            height,
            0,
            0,
            width,
            height,
            overlay,
        );
        assert_ne!(resampled[0], absent[0]);
        let mut scratch = app.take_rerender_input(wid);
        scratch.clear_overlays();
        let scratch_frame = app
            .rerender_capture(
                wid,
                &mut scratch,
                crate::app_render::HostVisualState::default(),
            )
            .expect("a rerender of the scratch");
        assert!(
            scratch_frame
                .pixels
                .iter()
                .any(|&pixel| pixel & 0x00FF_FFFF == scratch_bg)
        );

        if let Some(crate::PresentTarget::Virtual { window_gpu }) =
            app.windows.get_mut(&wid).unwrap().present.as_mut()
        {
            window_gpu.invalidate_present();
        }
        assert!(
            app.recording_presented_destination_capture(wid).is_ok(),
            "semantic invalidation leaves the last successful virtual destination readable"
        );
        app.present_input_scratch_for_test(wid, false, None)
            .expect("stage a newer virtual destination without success authority");
        assert!(
            app.recording_presented_destination_capture(wid).is_err(),
            "a newer destination generation cannot pair with the old successful serial"
        );

        let _ = app.video_rec.take();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn single_capture_dynamic_cursor_tracks_osc10_in_state_and_pixels() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let terminal = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut term = term_lock(&terminal);
            term.process(b"\x1b]21;cursor=\x07\x1b]10;#21C365\x07\x1b[2 q");
            assert_eq!(term.cursor_color(), None);
        }

        let capture = app
            .prepare_terminal_capture_grid(wid)
            .expect("single terminal capture");
        assert!(!capture.composed);
        let input = app.windows[&wid].input_scratch.clone();
        assert_eq!(input.cursor_color, 0x0021_C365);

        let Some(mut renderer) =
            aterm_render::Renderer::from_system(16.0, aterm_render::Theme::default())
        else {
            aterm_spec::host::could_not_run("no system monospace font on this host");
        };
        let (cw, ch) = renderer.cell_size();
        let frame = renderer.render_input(&input);
        assert_eq!(
            frame
                .pixels
                .iter()
                .filter(|&&pixel| pixel == 0x0021_C365)
                .count(),
            cw * ch,
            "the capture's steady blank cursor pixels follow OSC 10 after OSC 21 cursor="
        );
    }

    #[test]
    fn split_capture_grid_composes_sparse_panes_without_present_side_effects() {
        let (mut app, wid, right) = split_fixture();
        let plan = app
            .active_visible_leaf_plan(wid)
            .expect("visible split plan");
        let rects: Vec<_> = plan
            .leaves
            .iter()
            .map(|leaf| crate::pane::PaneRect {
                session: app
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    .expect("terminal leaf"),
                row_off: leaf.rect.origin.y.round() as u16,
                col_off: leaf.rect.origin.x.round() as u16,
                rows: leaf.rect.size.height.round() as u16,
                cols: leaf.rect.size.width.round() as u16,
            })
            .collect();
        let left_rect = rects.iter().find(|rect| rect.session == 0).unwrap();
        let right_rect = rects.iter().find(|rect| rect.session == right).unwrap();
        let left_term = app.pool.get(0).unwrap().term.clone();
        let left_epoch = term_lock(&left_term).damage_epoch();
        let left_coords = term_lock(&left_term)
            .content_scroll_state()
            .coordinate_generation();
        let predictor_now = Instant::now();
        let predictor_deadline = {
            let window = app.windows.get_mut(&wid).unwrap();
            window
                .predictor
                .set_mode(crate::predict::PredictMode::Always);
            assert!(window.predictor.predict_char(
                '?',
                (0, 0),
                left_coords,
                window.cols,
                predictor_now
            ));
            window.predictor.next_deadline()
        };

        assert!(app.windows[&wid].last_present.is_none());
        let capture = app
            .prepare_terminal_capture_grid(wid)
            .expect("terminal split capture");
        assert!(capture.composed);
        assert_eq!(capture.leaves.len(), 2);
        assert_eq!(
            capture
                .leaves
                .iter()
                .filter(|leaf| leaf.focused)
                .map(|leaf| leaf.session)
                .collect::<Vec<_>>(),
            vec![right],
            "capture inventory identifies exactly the focused pane"
        );
        let ws = &app.windows[&wid];
        let content_row = usize::from(left_rect.row_off);
        assert_eq!(left_rect.row_off, right_rect.row_off);
        let row = &ws.input_scratch.cells[content_row];
        assert_eq!(row[usize::from(left_rect.col_off)].ch, 'L');
        assert_eq!(row[usize::from(right_rect.col_off)].ch, 'R');
        assert_eq!(
            row[usize::from(left_rect.col_off + left_rect.cols - 1)].bg,
            [0x10, 0x20, 0x30],
            "left sparse tail uses the left terminal's implicit OSC-11 blank"
        );
        assert_eq!(
            row[usize::from(right_rect.col_off + right_rect.cols - 1)].bg,
            [0x40, 0x50, 0x60],
            "right sparse tail uses the right terminal's implicit OSC-11 blank"
        );
        assert_eq!(ws.input_scratch.cursor_row, usize::from(right_rect.row_off));
        assert!(
            (usize::from(right_rect.col_off)..usize::from(right_rect.col_off + right_rect.cols))
                .contains(&ws.input_scratch.cursor_col),
            "the only visible cursor is offset into the focused pane"
        );
        assert_eq!(
            ws.input_scratch.default_bg_spans[content_row].len(),
            2,
            "both panes retain independent live default-background provenance"
        );
        assert_eq!(
            ws.input_scratch.default_bg_at(
                content_row,
                usize::from(left_rect.col_off + left_rect.cols - 1),
            ),
            0x0010_2030,
        );
        assert_eq!(
            ws.input_scratch.default_bg_at(
                content_row,
                usize::from(right_rect.col_off + right_rect.cols - 1),
            ),
            0x0040_5060,
        );
        assert!(
            ws.last_present.is_none(),
            "capture is not an application-present decision"
        );
        assert_eq!(
            ws.predictor.next_deadline(),
            predictor_deadline,
            "capture does not advance predictor state"
        );
        assert!(!ws.predictor.idle());

        term_lock(&left_term).process(b"!");
        assert_eq!(
            term_lock(&left_term).damage_epoch(),
            left_epoch,
            "capture does not consume terminal damage"
        );
    }

    #[test]
    fn focused_split_selection_is_identical_in_live_and_capture_composites() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let (mut app, wid, right) = split_fixture();
        let (rows, cols) = {
            let window = &app.windows[&wid];
            (usize::from(window.rows), usize::from(window.cols))
        };
        let rects = app
            .active_tree(wid)
            .expect("split tree")
            .compute_layout(rows as u16, cols as u16);
        let right_rect = rects.iter().find(|rect| rect.session == right).unwrap();
        let right_term = app.pool.get(right).expect("right terminal").term.clone();
        {
            let mut terminal = term_lock(&right_term);
            terminal.process(b"\x1b]17;rgb:12/34/56\x07\x1b]19;rgb:fe/cd/32\x07");
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(2, 2, SelectionSide::Right);
            selection.complete_selection();
        }

        let assert_projection = |input: &aterm_render::RenderInput, lane: &str| {
            let row0 = usize::from(right_rect.row_off);
            let col0 = usize::from(right_rect.col_off);
            let row_end = row0 + usize::from(right_rect.rows);
            let col_end = col0 + usize::from(right_rect.cols);
            assert_eq!(
                input.selection_clip,
                Some(aterm_render::SelectionClip::new(
                    row0, row_end, col0, col_end,
                )),
                "{lane}: the renderer clip is the focused pane rectangle"
            );
            assert_eq!(input.selection_bg, 0x0012_3456, "{lane}: OSC 17");
            assert_eq!(input.selection_fg, 0x00fe_cd32, "{lane}: OSC 19");
            assert!(input.selection_contains_cell(row0, col0 + 1, false, false));
            assert!(input.selection_contains_cell(row0 + 2, col0 + 2, false, false));
            assert!(
                !input.selection_contains_cell(row0 + 2, col0 + 3, false, false),
                "{lane}: the final endpoint remains exact"
            );
        };

        assert!(
            app.redraw_compose(
                wid,
                rows,
                cols,
                false,
                false,
                None,
                0,
                std::time::Instant::now(),
            )
            .is_some(),
            "the focused selection change presents"
        );
        assert_projection(&app.windows[&wid].input_scratch, "live");

        let capture = app
            .prepare_terminal_capture_grid(wid)
            .expect("split capture");
        assert!(capture.composed);
        assert_projection(&app.windows[&wid].input_scratch, "capture");
    }

    #[test]
    fn the_capture_model_hash_sees_every_panes_selection() {
        use aterm_core::selection::{SelectionSide, SelectionType, TextSelection};
        use std::hash::Hasher as _;
        let hash = |input: &aterm_render::RenderInput| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            crate::App::hash_terminal_render_model(input, &mut h);
            h.finish()
        };
        let pane = |c1: u16| {
            let mut selection = TextSelection::default();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, c1, SelectionSide::Right);
            selection.complete_selection();
            aterm_core::render::PaneSelection {
                selection,
                clip: aterm_core::render::SelectionClip {
                    row_start: 0,
                    row_end: 2,
                    col_start: 0,
                    col_end: 8,
                },
                bg: aterm_core::render::COLOR_UNSET,
                fg: aterm_core::render::COLOR_UNSET,
                inactive: true,
            }
        };
        let mut term = aterm_core::terminal::Terminal::new(2, 8);
        term.process(b"abcdefgh");
        let base = term.cell_frame(2, 8);
        let mut one = base.clone();
        one.selections = vec![pane(2)];
        let mut moved = base.clone();
        moved.selections = vec![pane(5)];
        let mut bare = base.clone();
        bare.selections = vec![aterm_core::render::PaneSelection {
            selection: TextSelection::default(),
            ..pane(2)
        }];
        assert_eq!(hash(&one), hash(&one.clone()), "deterministic");
        assert_ne!(
            hash(&one),
            hash(&moved),
            "an unfocused pane's highlight moved"
        );
        assert_ne!(hash(&base), hash(&bare), "an entry is not the empty list");
        assert_ne!(hash(&bare), hash(&one), "a selection is not a bare entry");
    }

    #[test]
    fn the_capture_model_hash_sees_a_rows_bidi_map() {
        use aterm_core::selection::{SelectionSide, SelectionType};
        use std::hash::Hasher as _;
        let hash = |input: &aterm_render::RenderInput| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            crate::App::hash_terminal_render_model(input, &mut h);
            h.finish()
        };
        let mut term = aterm_core::terminal::Terminal::new(2, 8);
        term.process("\u{05D0}\u{05D0}\u{05D0}\u{05D0}".as_bytes());
        let sel = term.text_selection_mut();
        sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 0, SelectionSide::Right);
        sel.complete_selection();
        let reordered = term.cell_frame(2, 8);
        assert_eq!(reordered.visual_to_logical_row(0), [3, 2, 1, 0]);
        let mut unmapped = reordered.clone();
        unmapped.visual_to_logical.clear();
        assert_eq!(reordered.cells, unmapped.cells, "the same cells");
        assert!(reordered.selection_contains_cell(0, 3, false, false));
        assert!(unmapped.selection_contains_cell(0, 0, false, false));
        assert_ne!(hash(&reordered), hash(&unmapped), "a different highlight");
        let mut empty_rows = unmapped.clone();
        empty_rows.visual_to_logical = vec![Vec::new(); 2];
        assert_eq!(hash(&unmapped), hash(&empty_rows), "both the identity");
    }

    #[test]
    fn an_unfocused_pane_selection_paints_and_is_what_a_copy_resolves() {
        use crate::menu::{MenuAction, menu_live, set_menu_live};
        use aterm_core::selection::{SelectionSide, SelectionType};

        let before = menu_live();
        let mut readiness_failures = Vec::new();
        let mut check_readiness = |app: &App, label: &str, expected: bool| {
            app.publish_menu_live();
            let native = menu_live().has_selection;
            let live = app.palette_live().has_selection;
            let palette =
                app.palette_snapshot(WindowId(0)).action_by_name("Copy") == Ok(MenuAction::Copy);
            if native != expected || live != expected || palette != expected {
                readiness_failures.push(format!(
                    "{label}: expected={expected}, native={native}, live={live}, palette={palette}"
                ));
            }
        };
        let (mut app, wid, right) = split_fixture();
        check_readiness(&app, "no split selection", false);
        let (rows, cols) = {
            let window = &app.windows[&wid];
            (usize::from(window.rows), usize::from(window.cols))
        };
        let rects = app
            .active_tree(wid)
            .expect("split tree")
            .compute_layout(rows as u16, cols as u16);
        let left_rect = *rects.iter().find(|rect| rect.session == 0).unwrap();
        assert_eq!(
            app.active_tree(wid).unwrap().focus(),
            right,
            "the split fixture focuses the NEW pane, so session 0 is unfocused"
        );

        let left_term = app.pool.get(0).expect("left pane").term.clone();
        {
            let mut terminal = term_lock(&left_term);
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 3, SelectionSide::Right);
            selection.complete_selection();
        }

        assert!(
            app.redraw_compose(
                wid,
                rows,
                cols,
                false,
                false,
                None,
                0,
                std::time::Instant::now(),
            )
            .is_some(),
            "an unfocused pane's new selection must reach the present early-out"
        );
        let input = &app.windows[&wid].input_scratch;
        assert!(
            input.selection_contains_cell(
                usize::from(left_rect.row_off),
                usize::from(left_rect.col_off),
                false,
                false
            ),
            "the unfocused pane's selection PAINTS (this is the whole item)"
        );

        assert_eq!(
            app.window_selection_text(wid).as_deref(),
            Some("LEFT"),
            "⌘-C resolves the pane whose highlight is on screen"
        );
        check_readiness(&app, "unfocused visible selection", true);

        for prior in [true, false] {
            set_menu_live(crate::menu::MenuLive {
                has_selection: prior,
                ..menu_live()
            });
            {
                let _held = term_lock(&left_term);
                app.publish_menu_live();
                assert_eq!(
                    menu_live().has_selection,
                    prior,
                    "an unread sibling keeps the prior menu bit without blocking"
                );
            }
        }
        check_readiness(&app, "unlocked visible sibling", true);

        assert!(
            app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .active_mut()
                .unwrap()
                .toggle_zoom()
        );
        assert_eq!(app.active_visible_leaf_plan(wid).unwrap().leaves.len(), 1);
        assert_eq!(
            app.window_selection_text(wid),
            None,
            "zoom hides the selected sibling from Copy"
        );
        check_readiness(&app, "selected sibling hidden by zoom", false);
        assert!(
            !app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .active_mut()
                .unwrap()
                .toggle_zoom()
        );
        assert_eq!(app.window_selection_text(wid).as_deref(), Some("LEFT"));
        check_readiness(&app, "selected sibling visible after unzoom", true);

        let right_term = app.pool.get(right).expect("right pane").term.clone();
        {
            let mut terminal = term_lock(&right_term);
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 4, SelectionSide::Right);
            selection.complete_selection();
        }
        assert_eq!(
            app.window_selection_text(wid).as_deref(),
            Some("RIGHT"),
            "the focused pane wins whenever it has a selection at all"
        );
        check_readiness(&app, "focused visible selection", true);

        {
            let mut terminal = term_lock(&right_term);
            let selection = terminal.text_selection_mut();
            selection.start_selection(3, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(3, 6, SelectionSide::Right);
            selection.complete_selection();
            assert!(
                terminal.selection_to_string().is_none(),
                "precondition: this selection is real but resolves to no text"
            );
            assert!(
                terminal.text_selection().has_selection(),
                "precondition: …and the pane genuinely holds it"
            );
        }
        assert_eq!(
            app.window_selection_text(wid),
            None,
            "a focused pane holding a whitespace-only selection copies NOTHING — it \
             must never fall through to a sibling's text"
        );
        check_readiness(&app, "focused blank selection still owns Copy", true);

        let mut mixed = App::headless_for_test();
        assert!(mixed.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, native_view) = mixed.active_native_view(wid).expect("Settings view");
        let (session, _) =
            mixed.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let terminal = mixed.pool.get(session).unwrap().term.clone();
        {
            let mut terminal = term_lock(&terminal);
            terminal.process(b"VISIBLE");
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 6, SelectionSide::Right);
            selection.complete_selection();
        }
        assert_eq!(mixed.window_selection_text(wid).as_deref(), Some("VISIBLE"));
        check_readiness(&mixed, "focused terminal in a mixed tab", true);
        assert!(
            mixed
                .windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .active_mut()
                .unwrap()
                .set_focus(native_view)
        );
        mixed.sync_window(wid);
        assert!(mixed.active_native_view(wid).is_some());
        assert_eq!(
            mixed.window_selection_text(wid).as_deref(),
            Some("VISIBLE"),
            "the selected terminal sibling remains visible"
        );
        assert_eq!(mixed.native_selection_text(wid), None);
        assert!(
            !mixed.copy_selection_in(wid),
            "native focus cannot borrow the terminal sibling's selection"
        );
        check_readiness(
            &mixed,
            "native focus above a selected terminal sibling",
            false,
        );

        set_menu_live(before);
        assert!(
            readiness_failures.is_empty(),
            "{}",
            readiness_failures.join("\n")
        );
    }

    #[test]
    fn unchanged_split_and_zoom_captures_keep_stable_model_identity() {
        let (mut app, wid, _) = split_fixture();
        let first = app.prepare_terminal_capture_grid(wid).unwrap();
        let first_fp = App::terminal_capture_fingerprint(&first, &app.windows[&wid].input_scratch);
        let second = app.prepare_terminal_capture_grid(wid).unwrap();
        assert_eq!(
            App::terminal_capture_fingerprint(&second, &app.windows[&wid].input_scratch),
            first_fp
        );
        let left = app.pool.get(0).unwrap().term.clone();
        crate::term_lock(&left).process(b"!");
        let changed = app.prepare_terminal_capture_grid(wid).unwrap();
        assert_ne!(
            App::terminal_capture_fingerprint(&changed, &app.windows[&wid].input_scratch),
            first_fp
        );

        assert!(
            app.windows
                .get_mut(&wid)
                .unwrap()
                .tab_set
                .active_mut()
                .unwrap()
                .toggle_zoom()
        );
        let zoom_first = app.prepare_terminal_capture_grid(wid).unwrap();
        assert!(zoom_first.composed);
        assert_eq!(zoom_first.leaves.len(), 1, "zoom exposes one split leaf");
        let zoom_fp =
            App::terminal_capture_fingerprint(&zoom_first, &app.windows[&wid].input_scratch);
        let zoom_second = app.prepare_terminal_capture_grid(wid).unwrap();
        assert_eq!(
            App::terminal_capture_fingerprint(&zoom_second, &app.windows[&wid].input_scratch),
            zoom_fp
        );
    }

    #[test]
    fn image_capture_reports_split_composite_and_splices_chrome_once() {
        let (mut app, wid, right) = split_fixture();
        app.config.cursor_trail = Some(true);
        app.recompute_sparkle();
        app.sparkle = None;
        app.windows.get_mut(&wid).expect("window").focused = true;
        app.tab_strip_rows = 1;
        let base_rows = usize::from(app.windows[&wid].rows);
        let cols = usize::from(app.windows[&wid].cols);
        let plan = app
            .active_visible_leaf_plan(wid)
            .expect("visible split plan");
        let content_row = plan.leaves[0].rect.origin.y.round() as usize;
        assert!(
            plan.leaves
                .iter()
                .all(|leaf| leaf.rect.origin.y.round() as usize == content_row)
        );

        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(image_req_for_test(
            control_auth::ConfinedImage::in_memory(),
            false,
            true,
            Some(&metadata),
            tx,
        ));
        let (_, _, png) = rx
            .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
            .expect("image worker reply")
            .expect("split image succeeds")
            .value;
        assert!(png.is_some(), "real image path encoded the composed frame");

        let metadata = metadata.get().expect("image metadata");
        assert_eq!(metadata.frame_kind, "composite");
        assert_eq!(
            metadata.view, None,
            "a composite makes no singular-view claim"
        );
        assert_eq!(metadata.leaves.len(), 2);
        assert_eq!(
            metadata
                .leaves
                .iter()
                .map(|leaf| (leaf.session.unwrap(), leaf.focused))
                .collect::<Vec<_>>(),
            vec![(0, false), (right, true)]
        );
        assert!(
            metadata
                .leaves
                .iter()
                .all(|leaf| leaf.snapshot_seq.is_some()),
            "each visible terminal binds its own engine snapshot"
        );

        let ws = &app.windows[&wid];
        assert_eq!(
            ws.input_scratch.rows,
            base_rows + 1,
            "one tab-strip row, exactly once"
        );
        assert_eq!(ws.input_scratch.cells.len(), base_rows + 1);
        let text = terminal_capture_text(&ws.input_scratch, 1, cols);
        assert!(text.lines().nth(content_row).unwrap().contains("LEFT"));
        assert!(text.lines().nth(content_row).unwrap().contains("RIGHT"));
        assert!(
            ws.fx.companion().brain().is_active(),
            "the production composed-image route advances the default resident pet"
        );
        assert_eq!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
            1,
            "the split capture carries exactly one resident companion, never zero or a pet+kitty pair"
        );
        assert_eq!(
            text.lines().count(),
            base_rows,
            "SIGUSR1 text projection excludes chrome but retains the full composite grid"
        );
        assert!(
            ws.last_present.is_none(),
            "image capture does not stamp application-present"
        );
    }
}

#[cfg(test)]
mod capture_overlay_reset_tests {
    use super::App;
    use crate::WindowId;
    use std::time::Instant;

    #[test]
    fn disabled_word_decorations_cannot_leak_prior_capture_layers() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(false);
        app.prepare_terminal_capture_grid(wid).unwrap();
        app.sparkle_dirty = false;
        app.sparkle = None;
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            let art = aterm_render::fx::bitmap(
                aterm_core::render::FxTexels::Rgba8Straight,
                1,
                1,
                vec![255; 4],
            )
            .expect("a 1x1 art");
            let tiles = aterm_effects::matrix_rain::RainBaker::default();
            ws.fx.publish_for_test(
                &[aterm_render::sprites::FreeSprite::whole(0, 0, art)],
                (&[], &tiles),
                &mut ws.input_scratch,
            );
            assert_eq!(
                aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
                1,
                "fixture: a prior capture left a companion layer"
            );
            let input = &mut ws.input_scratch;
            input.word_decorations.push(aterm_render::WordDecoration {
                row: 0,
                col: 0,
                dx: 0,
                dy: 0,
                glyph: aterm_render::DecoGlyph::Star4,
                blend: aterm_render::DecoBlend::Add,
                color: 0x00FF_FFFF,
                alpha: 255,
            });
            input.ink.push(aterm_render::InkCell {
                row: 0,
                col: 0,
                color: [1, 2, 3],
            });
            input.nova_add.push(aterm_render::GlowQuad::default());
        }

        app.capture_words(wid, Instant::now());

        let input = &app.windows[&wid].input_scratch;
        assert!(input.word_decorations.is_empty());
        assert!(input.ink.is_empty());
        assert!(aterm_render::sprites::published(&input.fx_layers).is_empty());
        assert!(input.nova_add.is_empty());
    }

    #[test]
    fn decoration_refill_restamps_live_default_and_cursor_colors() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.prepare_terminal_capture_grid(wid).unwrap();
        {
            let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
            input.default_bg = 0x00AA_BBCC;
            input.cursor_color = 0x00CC_BBAA;
        }
        let terminal = app.front_terminal(wid).unwrap().term.clone();
        crate::term_lock(&terminal).process(b"\x1b]11;#123456\x07\x1b]12;#ABCDEF\x07");

        app.capture_words(wid, Instant::now());

        let input = &app.windows[&wid].input_scratch;
        assert_eq!(input.default_bg, 0x0012_3456);
        assert_eq!(input.cursor_color, 0x00AB_CDEF);

        crate::term_lock(&terminal).process(b"\x1b]21;cursor=\x07\x1b]10;#FEDCBA\x07");
        app.capture_words(wid, Instant::now());
        let input = &app.windows[&wid].input_scratch;
        assert_eq!(
            input.cursor_color, 0x00FE_DCBA,
            "the introspection refill resolves a dynamic cursor from live OSC 10"
        );
    }
}

#[cfg(test)]
mod encode_worker_tests {
    use super::*;
    use crate::app_render::apply_overlay_at;
    use crate::control_auth::ensure_private_dir;
    use std::time::Duration;

    #[cfg(target_os = "macos")]
    #[test]
    fn image_renders_the_frame_and_never_photographs_the_window() {
        let mut app = App::headless_for_test();
        let photographs = || OS_WINDOW_PHOTOGRAPHS.with(std::cell::Cell::get);
        let before = photographs();
        let dir = aterm_tempfile::TempDir::new().expect("an image dir");
        ensure_private_dir(dir.path()).expect("a private dir");
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: control_auth::ConfinedImage::for_test(dir.path(), "frame.png"),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        let (width, height, png) = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the image reply")
            .expect("a headless image succeeds")
            .value;
        assert!(width > 0 && height > 0, "{width}x{height}");
        assert!(
            png.is_some_and(|png| png.starts_with(b"\x89PNG")),
            "the PNG over the wire"
        );
        assert_eq!(
            photographs(),
            before,
            "`image` asked the OS for no photograph"
        );
        assert!(
            app.window_rgba_of(WindowId(0)).is_err(),
            "a headless window has nothing on screen to photograph"
        );
        assert_eq!(
            photographs(),
            before + 1,
            "control: a photograph asked for counts"
        );
    }

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let nonce = aterm_time::since_epoch()
            .expect("clock after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aterm-{label}-{}-{nonce}", std::process::id()))
    }

    fn confined(dir: &std::path::Path, name: &str) -> control_auth::ConfinedImage {
        control_auth::ConfinedImage::for_test(dir, name)
    }

    fn retained_capture(
        app: &App,
        input: aterm_render::RenderInput,
        visuals: crate::app_render::HostVisualState,
    ) -> ResolvedCapture {
        let (plan, route) = app
            .active_visible_frame_layout(crate::WindowId(0))
            .expect("a visible layout");
        ResolvedCapture {
            source: CaptureSource::Rerender {
                serial: Some(1),
                visuals,
            },
            model: Some(input),
            context: CaptureContext {
                plan,
                route,
                terminal_grid: None,
                recorded: None,
            },
        }
    }

    fn visual_capture_pixels(
        app: &mut App,
        presented: Option<(
            aterm_render::RenderInput,
            crate::app_render::HostVisualState,
        )>,
    ) -> Frame {
        let (reply, rx) = std::sync::mpsc::channel();
        let req = image_req_for_test(
            control_auth::ConfinedImage::in_memory(),
            true,
            true,
            None,
            reply,
        );
        if let Some((input, visuals)) = presented {
            let capture = retained_capture(app, input, visuals);
            app.image_tail(crate::WindowId(0), capture, req);
        } else {
            app.render_image(req);
        }
        let (_, _, png) = rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("visual capture worker reply")
            .expect("visual capture succeeds")
            .value;
        rgb_frame_of_png(&png.expect("capture PNG bytes"))
    }

    fn snapshot_capture_pixels(app: &mut App, dir: &std::path::Path, name: &str) -> Frame {
        let path = dir.join(name);
        let done = snapshot_sidecar_path(&path, ".done");
        app.snapshot_at_path(path.display().to_string());
        let deadline = std::time::Instant::now() + WORKER_PATIENCE;
        while !done.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the snapshot was published"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        rgb_frame_of_png(&std::fs::read(&path).expect("snapshot PNG"))
    }

    fn rgb_frame_of_png(png: &[u8]) -> Frame {
        let (rgba, width, height) = aterm_render::decode_png_rgba8(png).unwrap();
        let (pixels, remainder) = rgba.as_chunks::<4>();
        assert!(remainder.is_empty());
        Frame {
            width,
            height,
            pixels: pixels
                .iter()
                .map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]))
                .collect(),
        }
    }

    #[test]
    fn capture_visual_policy_terminal_fallback_keeps_charging_over_a_palette() {
        type Capture = fn(&mut App, &std::path::Path, &str) -> Frame;
        let image: Capture = |app, _, _| visual_capture_pixels(app, None);
        let snapshot: Capture = snapshot_capture_pixels;
        for (verb, capture) in [("image", image), ("snapshot", snapshot)] {
            charging_stays_over_a_palette(verb, capture);
        }
    }

    fn charging_stays_over_a_palette(
        verb: &str,
        capture: fn(&mut App, &std::path::Path, &str) -> Frame,
    ) {
        let dir = unique_dir("capture-visual-charging");
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.palette_enter();
        assert!(app.windows[&wid].overlay_open());
        assert!(app.windows[&wid].os_window.is_none());
        assert_eq!(app.windows[&wid].capture_present_serial, 0);
        let baseline = capture(&mut app, &dir, "baseline.png");

        let now = Instant::now();
        app.level_up = Some(crate::level_up::LevelUp::charging_continued(7, now, 0.0));
        let window = app.windows.get_mut(&wid).unwrap();
        window.drag_hover = true;
        window.bell_flash.ring(now);
        let visuals = app.host_visual_state(wid, now);
        assert!(!visuals.invert, "a modal still suppresses the bell");
        let overlay = visuals.overlay.expect("charging remains admitted");
        let charged = capture(&mut app, &dir, "charging.png");
        assert_eq!(
            (charged.width, charged.height),
            (baseline.width, baseline.height),
            "{verb}"
        );
        let mut expected = baseline.clone();
        apply_overlay_at(
            &mut expected.pixels,
            expected.width,
            expected.height,
            0,
            0,
            expected.width,
            expected.height,
            overlay,
        );
        assert_ne!(expected.pixels[0], baseline.pixels[0], "{verb}");
        assert_eq!(
            charged.pixels[0], expected.pixels[0],
            "{verb}: the charging rim, over the palette, with no bell"
        );

        app.level_up = None;
        assert_eq!(app.host_visual_state(wid, Instant::now()).overlay, None);
        let committed = capture(&mut app, &dir, "committed.png");
        assert_eq!(
            committed.pixels[0], baseline.pixels[0],
            "{verb}: the drag yields to the modal"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_visual_policy_retained_absence_ignores_a_new_live_glow() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(app.prepare_native_input_scratch(wid));
        let retained = app.windows[&wid].input_scratch.clone();
        let absent = crate::app_render::HostVisualState::default();
        let baseline = visual_capture_pixels(&mut app, Some((retained.clone(), absent)));
        app.level_up = Some(crate::level_up::LevelUp::charging_continued(
            8,
            Instant::now(),
            0.0,
        ));
        let overlay = app
            .host_visual_state(wid, Instant::now())
            .overlay
            .expect("live glow");
        let captured = visual_capture_pixels(&mut app, Some((retained.clone(), absent)));
        assert_eq!(
            captured.pixels, baseline.pixels,
            "retained None is authoritative, not a request to resample"
        );

        let painted = visual_capture_pixels(
            &mut app,
            Some((
                retained,
                crate::app_render::HostVisualState {
                    overlay: Some(overlay),
                    ..absent
                },
            )),
        );
        assert_ne!(
            painted.pixels[0], baseline.pixels[0],
            "the retained overlay path really reaches encoded pixels"
        );
    }

    #[test]
    fn encode_worker_replies_fifo_and_only_after_write() {
        let root = unique_dir("encode-worker");
        ensure_private_dir(&root).unwrap();
        let mut app = App::headless_for_test();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut paths = Vec::new();
        for (i, side) in [1usize, 2, 3].into_iter().enumerate() {
            let target = control_auth::confine_automatic_image_path(&root, &format!("shot{i}"))
                .expect("confine automatic image target");
            paths.push(target.display_path());
            app.submit_encode_job(EncodeJob::Image {
                pixels: CapturePixels::packed(Frame {
                    width: side,
                    height: side,
                    pixels: vec![0u32; side * side],
                })
                .expect("a whole frame"),
                metadata: None,
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target,
                want_bytes: false,
                cancel: crate::control::CaptureCancellation::new(),
                reply: tx.clone(),
            });
        }
        for (i, side) in [1u32, 2, 3].into_iter().enumerate() {
            let mut retained = rx
                .recv_timeout(WORKER_PATIENCE)
                .expect("worker reply")
                .expect("encode succeeds");
            assert_eq!(
                retained.value,
                (side, side, None),
                "reply {i} out of FIFO order"
            );
            retained
                .retention
                .as_mut()
                .expect("file reply carries exact handles")
                .prepare_write()
                .expect("wire-edge identity remains exact");
            let bytes = std::fs::read(&paths[i]).expect("file written before reply");
            assert!(
                bytes.starts_with(&[0x89, b'P', b'N', b'G']),
                "PNG signature missing"
            );
        }
        assert!(app.encode_tx.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_saturated_encoder_answers_busy_before_capturing() {
        let dir = unique_dir("capture-permit-busy");
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let held = CapturePermit::try_reserve(&app.capture_bytes, CAPTURE_BYTE_BUDGET)
            .expect("the budget is empty");
        let bodies = CAPTURE_BODIES.with(std::cell::Cell::get);
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(image_req_for_test(
            confined(&dir, "busy.png"),
            true,
            true,
            None,
            reply,
        ));
        let error = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the refusal replies at once")
            .expect_err("a saturated encoder refuses the capture");
        assert!(error.contains(ENCODE_QUEUE_FULL), "{error}");
        assert_eq!(
            CAPTURE_BODIES.with(std::cell::Cell::get),
            bodies,
            "no capture body ran: nothing was photographed"
        );
        assert!(app.encode_tx.is_none(), "nothing reached the encoder");
        assert!(app.capture_permit.is_none());
        assert_eq!(
            app.capture_bytes.load(std::sync::atomic::Ordering::Acquire),
            CAPTURE_BYTE_BUDGET,
            "the refusal reserved nothing"
        );

        let path = dir.join("snapshot.png");
        let done = snapshot_sidecar_path(&path, ".done");
        std::fs::write(&done, b"stale\n").unwrap();
        app.snapshot_at_path(path.display().to_string());
        assert!(!done.exists(), "a refused snapshot publishes no completion");
        assert!(!path.exists(), "and writes no pixels");
        assert_eq!(CAPTURE_BODIES.with(std::cell::Cell::get), bodies);

        drop(held);
        let frame = visual_capture_pixels(&mut app, None);
        assert!(
            frame.width > 0 && frame.height > 0,
            "admitted once the budget frees"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[derive(Clone, Copy, Debug)]
    enum CaptureKind {
        Image,
        Snapshot,
        #[cfg(any(target_os = "macos", windows))]
        Window,
    }

    impl CaptureKind {
        const ALL: &[CaptureKind] = &[
            CaptureKind::Image,
            CaptureKind::Snapshot,
            #[cfg(any(target_os = "macos", windows))]
            CaptureKind::Window,
        ];

        fn enter(
            self,
            app: &mut App,
            dir: &std::path::Path,
            name: &str,
            admitted: bool,
        ) -> Option<Result<(), String>> {
            match self {
                CaptureKind::Image => {
                    let (reply, rx) = std::sync::mpsc::channel();
                    app.render_image(crate::control::ImageReq {
                        handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                        target: confined(dir, name),
                        clean: true,
                        session: None,
                        want_bytes: false,
                        want_metadata: false,
                        frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
                        cancel: crate::control::CaptureCancellation::new(),
                        reply,
                    });
                    Some(
                        rx.recv_timeout(WORKER_PATIENCE)
                            .expect("the image answers")
                            .map(drop),
                    )
                }
                CaptureKind::Snapshot => {
                    let path = dir.join(name);
                    let done = snapshot_sidecar_path(&path, ".done");
                    app.snapshot_at_path(path.display().to_string());
                    if admitted {
                        let deadline = std::time::Instant::now() + WORKER_PATIENCE;
                        while !done.exists() {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "the admitted snapshot was published"
                            );
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    } else {
                        assert!(!done.exists() && !path.exists(), "nothing was written");
                    }
                    None
                }
                #[cfg(any(target_os = "macos", windows))]
                CaptureKind::Window => {
                    let (reply, rx) = std::sync::mpsc::channel();
                    app.capture_window(
                        crate::control::ReplyRetentionPermit::from_own_budget(),
                        confined(dir, name),
                        crate::control::CaptureCancellation::new(),
                        reply,
                    );
                    Some(
                        rx.recv_timeout(WORKER_PATIENCE)
                            .expect("the window capture answers")
                            .map(drop),
                    )
                }
            }
        }
    }

    #[test]
    fn run_capture_admits_every_kind_once() {
        let dir = unique_dir("capture-door");
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let held = CapturePermit::try_reserve(&app.capture_bytes, CAPTURE_BYTE_BUDGET)
            .expect("the budget is empty");
        for &kind in CaptureKind::ALL {
            let bodies = capture_bodies();
            if let Some(reply) = kind.enter(&mut app, &dir, &format!("busy-{kind:?}.png"), false) {
                let error = reply.expect_err("a saturated encoder refuses the capture");
                assert!(error.contains(ENCODE_QUEUE_FULL), "{kind:?}: {error}");
            }
            assert_eq!(capture_bodies(), bodies, "{kind:?}: no body ran");
        }
        drop(held);
        for &kind in CaptureKind::ALL {
            let bodies = capture_bodies();
            let reply = kind.enter(&mut app, &dir, &format!("{kind:?}.png"), true);
            assert_eq!(
                capture_bodies(),
                bodies + 1,
                "{kind:?}: admitted once ({reply:?})"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn feed_a_blocked_encoder(
        app: &mut App,
        mut admit: impl FnMut(&mut App) -> bool,
        mut job: impl FnMut(u32) -> EncodeJob,
    ) -> (Vec<u32>, Vec<usize>, std::sync::mpsc::Receiver<EncodeWork>) {
        let (tx, blocked) = std::sync::mpsc::sync_channel::<EncodeWork>(64);
        app.encode_tx = Some(tx);
        let mut admitted = Vec::new();
        let mut in_flight = Vec::new();
        for i in 0..20_u32 {
            if !admit(app) {
                continue;
            }
            app.submit_encode_job(job(i));
            app.capture_permit = None;
            admitted.push(i);
            in_flight.push(app.capture_bytes.load(std::sync::atomic::Ordering::Acquire));
        }
        (admitted, in_flight, blocked)
    }

    fn image_job(
        pixels: CapturePixels,
        reply: &std::sync::mpsc::Sender<crate::control::ImageReply>,
    ) -> EncodeJob {
        EncodeJob::Image {
            pixels,
            metadata: None,
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: control_auth::ConfinedImage::for_test(&std::env::temp_dir(), "unused"),
            want_bytes: true,
            cancel: crate::control::CaptureCancellation::new(),
            reply: reply.clone(),
        }
    }

    fn assert_each_holds_its_peak(label: &str, in_flight: &[usize], peak: usize) {
        for (n, &bytes) in in_flight.iter().enumerate() {
            assert_eq!(bytes, (n + 1) * peak, "{label}: capture {n} holds its peak");
            assert!(
                bytes <= CAPTURE_BYTE_BUDGET.max(peak),
                "{label}: {bytes} bytes in flight past the budget"
            );
        }
        assert_eq!(
            in_flight.len(),
            (CAPTURE_BYTE_BUDGET / peak).max(1),
            "{label}: exactly as many as the budget holds"
        );
    }

    #[test]
    fn a_blocked_encoder_holds_at_most_its_byte_budget() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let (width, height) = (5120_usize, 2880_usize);
        let peak_bytes = width * height * 4 + capture_encode_overhead(width, height);
        app.windows.get_mut(&wid).unwrap().win_px = Some(aterm_window::dpi::PhysicalSize::new(
            u32::try_from(width).unwrap(),
            u32::try_from(height).unwrap(),
        ));
        let charge = CaptureCharge::Frame(DestinationDemand::Preferred);
        assert_eq!(app.capture_bytes_estimate(wid, charge), peak_bytes);
        let (reply, _replies) = std::sync::mpsc::channel();
        let (admitted, in_flight, blocked) = feed_a_blocked_encoder(
            &mut app,
            |app| app.admit_capture(Some(wid), charge),
            |i| {
                let frame = Frame {
                    width,
                    height,
                    pixels: vec![i; width * height],
                };
                image_job(CapturePixels::packed(frame).expect("a whole frame"), &reply)
            },
        );
        assert!(admitted.len() < 20, "the budget refused once full");
        assert_each_holds_its_peak("5K", &in_flight, peak_bytes);
        for expect in &admitted {
            let work = blocked.try_recv().expect("an admitted capture was queued");
            match &work.job {
                EncodeJob::Image { pixels, .. } => {
                    let mut row = Vec::new();
                    pixels.rgba_row(0, &mut row);
                    let blue = u8::try_from(*expect).expect("a small index");
                    assert_eq!(row[..4], [0, 0, blue, 255], "FIFO order");
                }
                _ => panic!("an image job"),
            }
            drop(work);
        }
        assert!(blocked.try_recv().is_err(), "no refused capture was queued");
        assert_eq!(
            app.capture_bytes.load(std::sync::atomic::Ordering::Acquire),
            0,
            "draining the encoder returned every byte"
        );
    }

    #[test]
    fn an_admitted_capture_is_charged_what_admission_reserved() {
        use aterm_gpu::video_tap::RawCapture;

        let (reply, _replies) = std::sync::mpsc::channel();
        let mapped = |width: usize, height: usize, bpp: usize| {
            let row = (width * bpp).div_ceil(256) * 256;
            (vec![0_u8; row * height], row)
        };

        let (width, height) = (1440_usize, 900_usize);
        let f16 = || {
            let (bytes, row) = mapped(width, height, 8);
            CapturePixels::raw(
                RawCapture::scrgb_f16_for_test(bytes, width as u32, height as u32, row)
                    .expect("an f16 destination"),
            )
        };
        let f16_peak = f16().peak_bytes();
        let f16_estimate = frame_capture_peak(width, height, Some(f16().allocation()));
        assert_eq!(f16_estimate, f16_peak, "the f16 estimate is its charge");
        let mut app = App::headless_for_test();
        let (_, in_flight, _queued) = feed_a_blocked_encoder(
            &mut app,
            |app| app.admit_capture_bytes(f16_estimate),
            |_| image_job(f16(), &reply),
        );
        assert_each_holds_its_peak("f16", &in_flight, f16_peak);

        let frame_only = frame_capture_peak(width, height, None);
        assert!(frame_only < f16_peak);
        let mut app = App::headless_for_test();
        let (_, in_flight, _queued) = feed_a_blocked_encoder(
            &mut app,
            |app| app.admit_capture_bytes(frame_only),
            |_| image_job(f16(), &reply),
        );
        assert!(
            in_flight.iter().any(|&bytes| bytes > CAPTURE_BYTE_BUDGET),
            "negative control: the frame-only estimate overruns the budget"
        );

        let cut = || {
            let (bytes, row) = mapped(width + 1, height + 1, 4);
            let raw =
                RawCapture::bgra8_srgb_for_test(bytes, width as u32 + 1, height as u32 + 1, row)
                    .expect("an 8-bit destination");
            CapturePixels::raw(raw)
                .cut_to(aterm_render::PixelRect {
                    x: 1,
                    y: 0,
                    width,
                    height,
                })
                .expect("the frame is inside its surface")
        };
        let cut_peak = cut().peak_bytes();
        assert_eq!(
            frame_capture_peak(width, height, Some(cut().allocation())),
            cut_peak,
            "the cut destination's estimate is its charge"
        );
        assert!(cut_peak > frame_capture_peak(width, height, None));
        let mut app = App::headless_for_test();
        let (_, in_flight, _queued) = feed_a_blocked_encoder(
            &mut app,
            |app| app.admit_capture_bytes(cut_peak),
            |_| image_job(cut(), &reply),
        );
        assert_each_holds_its_peak("cut", &in_flight, cut_peak);

        #[cfg(any(target_os = "macos", windows))]
        {
            let mut app = App::headless_for_test();
            let wid = crate::WindowId(0);
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.win_px = Some(aterm_window::dpi::PhysicalSize::new(
                width as u32,
                height as u32,
            ));
            #[cfg(target_os = "macos")]
            let chrome_height = 56;
            #[cfg(target_os = "macos")]
            {
                ws.metrics.head = chrome_height;
            }
            let (window_reply, _window_replies) = std::sync::mpsc::channel();
            let stitch = || WindowStitch {
                shot: PlatformShot {
                    pixels: vec![0; width * height * 4],
                    width: width as u32,
                    height: height as u32,
                    encoding: ShotEncoding::NATIVE,
                    #[cfg(target_os = "macos")]
                    chrome: Some(ChromeLayer {
                        x: 0,
                        y: 0,
                        width: width as u32,
                        height: chrome_height as u32,
                        pixels: ChromePixels::Bitmap {
                            bytes: vec![0; width * chrome_height * 4],
                            desc: BitmapDesc {
                                width,
                                height: chrome_height,
                                bytes_per_row: width * 4,
                                bits_per_component: 8,
                                bits_per_pixel: 32,
                                bitmap_info: 1,
                                rgb: true,
                            },
                        },
                    }),
                },
                client: CapturePixels::softbuffer(vec![0; width * height], width, height)
                    .expect("a whole client"),
                rect: WindowClientRect {
                    x: 0,
                    y: 0,
                    width: width as u32,
                    height: height as u32,
                },
            };
            let window_peak = stitch().peak_bytes();
            assert_eq!(
                app.capture_bytes_estimate(wid, CaptureCharge::Window),
                window_peak,
                "the window estimate is its stitch's charge"
            );
            // The AppKit chrome and outer shape cost more than a frame; a
            // Windows stitch carries neither, so its client copy and row
            // fit inside the encoder's own peak.
            #[cfg(target_os = "macos")]
            assert!(window_peak > frame_capture_peak(width, height, None));
            #[cfg(windows)]
            assert_eq!(window_peak, frame_capture_peak(width, height, None));
            let (_, in_flight, _queued) = feed_a_blocked_encoder(
                &mut app,
                |app| app.admit_capture(Some(wid), CaptureCharge::Window),
                |_| EncodeJob::Window {
                    stitch: stitch(),
                    handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                    target: control_auth::ConfinedImage::for_test(&std::env::temp_dir(), "unused"),
                    cancel: crate::control::CaptureCancellation::new(),
                    reply: window_reply.clone(),
                },
            );
            assert_each_holds_its_peak("window", &in_flight, window_peak);
        }
    }

    fn blank_metadata() -> crate::control::ImageFrameMetadata {
        crate::control::ImageFrameMetadata {
            frame_kind: "terminal",
            phase: "staged",
            window: 0,
            view: None,
            generation: None,
            config_revision: None,
            update_revision: None,
            document_seq: None,
            presentation_revision: None,
            paint_revision: None,
            capture_serial: 0,
            width: 0,
            height: 0,
            pixel_fingerprint: 0,
            compiled_fingerprint: None,
            raster_fingerprint: 0,
            raster_model_fingerprint: 0,
            raster_geometry: 0,
            overlay_fingerprint: 0,
            theme_fingerprint: 0,
            leaves: Vec::new(),
        }
    }

    #[test]
    fn a_captures_bytes_are_free_before_its_answer() {
        let held = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for case in ["encoded", "cancelled", "unencodable", "republished"] {
            let (width, height) = if case == "unencodable" {
                (usize::try_from(u64::from(u32::MAX) + 1).expect("64-bit"), 0)
            } else {
                (64_usize, 64_usize)
            };
            let pixels = CapturePixels::packed(Frame {
                width,
                height,
                pixels: vec![0u32; width * height],
            })
            .expect("a whole frame");
            let metadata = (case == "republished").then(|| {
                let cell = std::sync::Arc::new(std::sync::OnceLock::new());
                let published = blank_metadata();
                cell.set(published.clone()).expect("first");
                Box::new(PendingMetadata {
                    metadata: published,
                    raster_is_pixels: false,
                    cell,
                })
            });
            let permit = CapturePermit::try_reserve(&held, pixels.peak_bytes())
                .expect("the budget is empty");
            let cancel = crate::control::CaptureCancellation::new();
            if case == "cancelled" {
                assert!(cancel.cancel());
            }
            let (reply, rx) = std::sync::mpsc::channel::<Result<_, String>>();
            let observed = std::sync::Arc::clone(&held);
            let reader = std::thread::spawn(move || {
                let answer = rx.recv().expect("an answer");
                (
                    answer.map(drop),
                    observed.load(std::sync::atomic::Ordering::Acquire),
                )
            });
            run_encode_job(
                EncodeJob::Image {
                    pixels,
                    metadata,
                    handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                    target: control_auth::ConfinedImage::for_test(&std::env::temp_dir(), "unused"),
                    want_bytes: true,
                    cancel,
                    reply,
                },
                Some(permit),
            );
            let (answer, at_answer) = reader.join().expect("reader");
            match case {
                "encoded" => assert!(answer.is_ok(), "{case}: {answer:?}"),
                "cancelled" => assert!(answer.is_err_and(|e| e.contains("cancelled")), "{case}"),
                "unencodable" => assert!(
                    answer.is_err_and(|e| e.contains("image PNG encode failed")),
                    "{case}"
                ),
                _ => assert!(
                    answer.is_err_and(|e| e.contains("already initialized")),
                    "{case}"
                ),
            }
            assert_eq!(
                at_answer, 0,
                "{case}: the capture's bytes were free when the answer arrived"
            );
        }
    }

    #[test]
    fn no_png_deflate_runs_on_the_submitting_thread() {
        let mut app = App::headless_for_test();
        let here = std::thread::current().id();
        let frame = visual_capture_pixels(&mut app, None);
        assert!(frame.width > 0);
        let held = CapturePermit::try_reserve(&app.capture_bytes, CAPTURE_BYTE_BUDGET * 2)
            .or_else(|| CapturePermit::try_reserve(&app.capture_bytes, CAPTURE_BYTE_BUDGET));
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(image_req_for_test(
            control_auth::ConfinedImage::in_memory(),
            true,
            true,
            None,
            reply,
        ));
        let _ = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        drop(held);
        let encoders = ENCODE_THREADS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(!encoders.is_empty(), "the capture was encoded");
        assert!(
            !encoders.contains(&here),
            "a PNG deflate ran on the submitting (event-loop) thread"
        );
    }

    #[test]
    fn rejected_encode_jobs_publish_nothing() {
        let root = unique_dir("encode-queue-reject");
        ensure_private_dir(&root).unwrap();
        let target =
            control_auth::confine_automatic_image_path(&root, "rejected").expect("rejected target");
        let image_path = target.display_path();
        let (reply, rx) = std::sync::mpsc::channel();
        reject_encode_job(
            EncodeJob::Image {
                pixels: CapturePixels::packed(Frame {
                    width: 2,
                    height: 2,
                    pixels: vec![0; 4],
                })
                .expect("a whole frame"),
                metadata: None,
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target,
                want_bytes: false,
                cancel: crate::control::CaptureCancellation::new(),
                reply,
            },
            ENCODE_QUEUE_FULL,
        );
        let error = rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("rejection reply")
            .expect_err("a saturated capture is rejected");
        assert!(error.contains(ENCODE_QUEUE_FULL), "{error}");
        assert!(!image_path.exists(), "rejection publishes no image");

        let path = root.join("snapshot.png");
        let done = snapshot_sidecar_path(&path, ".done");
        std::fs::write(&done, b"stale\n").unwrap();
        let transaction = begin_snapshot_generation(&path).expect("snapshot transaction");
        assert!(!done.exists(), "begin removes the stale marker");
        reject_encode_job(
            EncodeJob::Snapshot {
                pixels: CapturePixels::packed(Frame {
                    width: 1,
                    height: 1,
                    pixels: vec![0],
                })
                .expect("a whole frame"),
                text: "unpublished".to_string(),
                transaction,
            },
            ENCODE_QUEUE_FULL,
        );
        assert!(!path.exists(), "rejection must not write snapshot pixels");
        assert!(!done.exists(), "rejection must not publish `.done`");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn capture_regression_snapshot_done_is_a_commit_marker() {
        let dir = unique_dir("snapshot-commit");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snapshot.png");
        let text_path = snapshot_sidecar_path(&path, ".txt");
        let done_path = snapshot_sidecar_path(&path, ".done");
        let frame = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x0011_2233],
        };

        std::fs::write(&done_path, b"stale\n").unwrap();
        std::fs::create_dir(&text_path).unwrap();
        let transaction = begin_snapshot_generation(&path).unwrap();
        let error =
            write_snapshot_artifacts(&encode_frame_png(&frame), "visible text", &transaction)
                .expect_err("a failed payload write must abort the snapshot");
        assert!(
            error.contains("text write failed"),
            "precise error: {error}"
        );
        assert!(
            !done_path.exists(),
            "the stale/partial attempt must own no completion marker"
        );
        assert!(
            !path.exists(),
            "a payload written before the failure is cleaned up"
        );

        std::fs::remove_dir(&text_path).unwrap();
        let transaction = begin_snapshot_generation(&path).unwrap();
        write_snapshot_artifacts(&encode_frame_png(&frame), "visible text", &transaction)
            .expect("a complete snapshot commits");
        assert!(path.is_file() && text_path.is_file() && done_path.is_file());
        assert_eq!(std::fs::read_to_string(&text_path).unwrap(), "visible text");
        let done = std::fs::read_to_string(&done_path).unwrap();
        assert!(done.starts_with("1x1\ngeneration="));
        assert!(done.contains("png_sha256=") && done.contains("text_sha256="));
        assert!(done.contains(&format!(
            "png_sha256={}",
            sha256_hex(&std::fs::read(&path).unwrap())
        )));
        assert!(done.contains(&format!(
            "text_sha256={}",
            sha256_hex(&std::fs::read(&text_path).unwrap())
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_begin_unlinks_stale_marker_before_directory_sync() {
        let dir = unique_dir("snapshot-clear-sync-order");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snapshot.png");
        let done = sidecar_name(path.file_name().unwrap(), ".done");
        std::fs::write(dir.join(&done), b"stale").unwrap();
        let target = SnapshotTarget {
            path,
            dir: crate::pinned_dir::PinnedDir::open_resolved(&dir).unwrap(),
            png: std::ffi::OsString::from("snapshot.png"),
            text: std::ffi::OsString::from("snapshot.png.txt"),
            done,
        };
        let sync_observed = std::cell::Cell::new(false);

        clear_snapshot_completion_with_sync(&target, |pinned| {
            assert!(
                !target.dir.path().join(&target.done).exists(),
                "the stale marker must be absent before the durability barrier"
            );
            sync_observed.set(true);
            pinned.sync()
        })
        .unwrap();

        assert!(sync_observed.get(), "the containing directory was synced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn image_reply_barrier_keeps_exact_guard_and_fails_closed_on_ancestor_swap() {
        use std::os::unix::fs::symlink;

        let root = unique_dir("image-reply-barrier");
        let images = root.join("images");
        let outside = unique_dir("image-reply-outside");
        ensure_private_dir(&images).unwrap();
        ensure_private_dir(&outside).unwrap();
        let target = crate::control_auth::ConfinedImage::for_test(&images, "shot.png");
        let file = target.write_private(b"png").unwrap();
        let moved = root.join("images-moved");
        let images_for_hook = images.clone();
        let moved_for_hook = moved.clone();
        let outside_for_hook = outside.clone();
        let (reply, result) = std::sync::mpsc::channel();

        send_capture_reply_after_validation_with_hook(
            PendingCaptureReply {
                target,
                file,
                lease: None,
                permit: crate::control::ReplyRetentionPermit::from_own_budget(),
            },
            &reply,
            (1_u32, 1_u32, None::<Vec<u8>>),
            "image",
            move || {
                std::fs::rename(&images_for_hook, &moved_for_hook).unwrap();
                symlink(&outside_for_hook, &images_for_hook).unwrap();
            },
        );
        assert!(result.recv().unwrap().is_err());
        assert!(
            std::fs::read_dir(&moved).unwrap().next().is_none(),
            "the exact failed artifact is removed through its retained handle"
        );
        assert!(std::fs::read_dir(&outside).unwrap().next().is_none());

        let _ = std::fs::remove_file(images);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn image_reply_guard_defers_prune_and_preserves_its_exact_path() {
        let sock = unique_dir("image-reply-before-prune");
        ensure_private_dir(&sock).unwrap();
        let target = control_auth::confine_automatic_image_path(&sock, "image").unwrap();
        let path = target.display_path().to_path_buf();
        let parent = path.parent().unwrap().to_path_buf();
        let lease = control_auth::acquire_capture_name_lease(&target, || false)
            .expect("automatic lease acquisition")
            .expect("lease queued automatic capture");
        let file = target.write_private(b"fresh-png").unwrap();
        for _ in 0..(control_auth::AUTO_IMAGE_KEEP + 5) {
            let newer = control_auth::automatic_capture_name("image");
            std::fs::write(parent.join(newer), b"newer").unwrap();
        }
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        send_capture_reply_after_validation(
            PendingCaptureReply {
                target,
                file,
                lease: Some(lease),
                permit: crate::control::ReplyRetentionPermit::from_own_budget(),
            },
            &reply_tx,
            (1_u32, 1_u32, None::<Vec<u8>>),
            "image",
        );
        let mut retained = reply_rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("worker handed the exact guard to the control reply")
            .expect("capture reply remains valid");
        assert_eq!(
            retained.value,
            (1, 1, None),
            "the guard carries the original wire payload"
        );
        assert!(
            std::fs::read_dir(&parent).unwrap().count() > control_auth::AUTO_IMAGE_KEEP,
            "retention has not run while the reply guard is queued"
        );
        retained
            .retention
            .as_mut()
            .expect("file reply carries a guard")
            .prepare_write()
            .expect("wire-edge identity validates");
        drop(retained);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"fresh-png",
            "the post-wire sweep reserves one keep slot for this exact path"
        );

        let _ = std::fs::remove_dir_all(sock);
    }

    #[test]
    fn explicit_capture_namespace_stays_busy_across_distinct_names_until_wire_release() {
        let root = unique_dir("image-namespace-wire-lease");
        ensure_private_dir(&root).unwrap();
        let first_target = crate::control_auth::ConfinedImage::for_test(&root, "shot.png");
        let first_path = first_target.display_path();
        let first_cancel = crate::control::CaptureCancellation::new();
        let first_lease = crate::control_auth::acquire_capture_name_lease(&first_target, || {
            first_cancel.is_cancelled()
        })
        .expect("first lease acquisition")
        .expect("first name lease");
        let first_file = first_target
            .write_private_authorized(b"first", || first_cancel.authorize_commit())
            .unwrap();
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        send_capture_reply_after_validation(
            PendingCaptureReply {
                target: first_target,
                file: first_file,
                lease: Some(first_lease),
                permit: crate::control::ReplyRetentionPermit::from_own_budget(),
            },
            &first_tx,
            (),
            "image",
        );
        let mut first = first_rx.recv().unwrap().unwrap();

        let second_target = crate::control_auth::ConfinedImage::for_test(&root, "other.png");
        let second_path = second_target.display_path();
        let second_cancel = crate::control::CaptureCancellation::new();
        let busy = crate::control_auth::acquire_capture_name_lease_with_wait(
            &second_target,
            || second_cancel.is_cancelled(),
            std::time::Duration::ZERO,
        )
        .expect_err("a differently named explicit capture still contends on the namespace");
        assert_eq!(busy.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(
            std::fs::read(&first_path).unwrap(),
            b"first",
            "a busy successor cannot mutate the first guarded artifact"
        );
        assert!(
            !second_path.exists(),
            "the distinct successor cannot publish while the namespace is leased"
        );
        first
            .retention
            .as_mut()
            .unwrap()
            .prepare_write()
            .expect("first identity validates at its wire edge");
        assert_eq!(std::fs::read(&first_path).unwrap(), b"first");
        drop(first);

        let second_lease =
            crate::control_auth::acquire_capture_name_lease(&second_target, || false)
                .expect("retry after release")
                .expect("second name lease");
        let second_file = second_target
            .write_private_authorized(b"second", || true)
            .unwrap();
        let (second_tx, second_rx) = std::sync::mpsc::channel();
        send_capture_reply_after_validation(
            PendingCaptureReply {
                target: second_target,
                file: second_file,
                lease: Some(second_lease),
                permit: crate::control::ReplyRetentionPermit::from_own_budget(),
            },
            &second_tx,
            (),
            "image",
        );
        let mut second = second_rx.recv().unwrap().unwrap();
        second
            .retention
            .as_mut()
            .unwrap()
            .prepare_write()
            .expect("second identity validates at its wire edge");
        assert_eq!(std::fs::read(&second_path).unwrap(), b"second");
        assert_eq!(
            std::fs::read(&first_path).unwrap(),
            b"first",
            "a distinct successor never overwrites the first result"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn capture_timeout_cancellation_wins_before_final_name_publish() {
        let root = unique_dir("image-cancel-before-publish");
        ensure_private_dir(&root).unwrap();
        let target = crate::control_auth::ConfinedImage::for_test(&root, "shot.png");
        let path = target.display_path();
        let cancel = crate::control::CaptureCancellation::new();
        assert!(cancel.cancel());

        let error = target
            .write_private_authorized(b"never-visible", || cancel.authorize_commit())
            .expect_err("cancelled publication cannot acquire the final name");

        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(!path.exists());
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "the fully written private temporary is cleaned when cancellation wins"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn video_publication_bundle_drops_guards_before_unpublished_directory_on_panic() {
        let sock = unique_dir("video-windows-unwind");
        ensure_private_dir(&sock).unwrap();
        let recording = control_auth::confine_video_dir(&sock).unwrap();
        let path = recording.path().to_path_buf();
        let frame = recording
            .write_sealed_frame(std::ffi::OsStr::new("frame-000000.png"), b"png")
            .unwrap();
        let index = recording
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
            )
            .unwrap();

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _publication = VideoPublication {
                frame_seals: vec![frame],
                index_file: index,
                published_marker: None,
                dir: recording,
            };
            panic!("injected before publication");
        }));
        assert!(unwind.is_err());
        assert!(
            !path.exists(),
            "the index deny-delete handle must release before directory cleanup"
        );

        let retained = control_auth::confine_video_dir(&sock).unwrap();
        let retained_path = retained.path().to_path_buf();
        let frame = retained
            .write_sealed_frame(std::ffi::OsStr::new("frame-000000.png"), b"png")
            .unwrap();
        let index = retained
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
            )
            .unwrap();
        let mut publication = VideoPublication {
            frame_seals: vec![frame],
            index_file: index,
            published_marker: None,
            dir: retained,
        };
        publication.prepare().unwrap();
        publication.publish_marker().unwrap();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _publication = publication;
            panic!("injected after publication");
        }));
        assert!(unwind.is_err());
        assert!(
            retained_path.join("index.json").is_file(),
            "publication is irrevocably retained before any observable reply"
        );

        let _ = std::fs::remove_dir_all(sock);
    }

    #[test]
    fn video_reply_guard_owns_publication_until_wire_prepare() {
        let sock = unique_dir("video-reply-before-prune");
        ensure_private_dir(&sock).unwrap();
        let recording = control_auth::confine_video_dir(&sock).unwrap();
        let path = recording.path().to_path_buf();
        let frame = recording
            .write_sealed_frame(std::ffi::OsStr::new("frame-000000.png"), b"png")
            .unwrap();
        let index = recording
            .write_new_private(
                std::ffi::OsStr::new("index.json"),
                b"{\"frames\":[{\"file\":\"frame-000000.png\"}]}",
            )
            .unwrap();
        let mut publication = VideoPublication {
            frame_seals: vec![frame],
            index_file: index,
            published_marker: None,
            dir: recording,
        };
        publication.prepare().unwrap();
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        send_video_reply_with_retention(
            publication,
            crate::control::ReplyRetentionPermit::from_own_budget(),
            &reply_tx,
            "OK video\n".to_string(),
        );
        let mut retained = reply_rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("worker handed publication to the control writer");
        assert_eq!(retained.value, "OK video\n");
        assert!(path.join("index.json").is_file());
        for _ in 0..11 {
            let mut later = control_auth::confine_video_dir(&sock).unwrap();
            let later_index = later
                .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
                .unwrap();
            later.publish(&[], &later_index).unwrap();
            let later_marker = later.publish_marker().unwrap();
            drop(later_marker);
            drop(later_index);
            drop(later);
        }
        assert!(
            path.join("index.json").is_file(),
            "later recording sweeps cannot quarantine a queued video OK"
        );
        retained
            .retention
            .as_mut()
            .expect("video OK carries its publication retention")
            .prepare_write()
            .expect("wire-edge video validation succeeds");
        drop(retained);
        assert!(
            path.join("index.json").is_file(),
            "wire preparation makes publication irrevocable before OK bytes"
        );

        let _ = std::fs::remove_dir_all(sock);
    }

    #[cfg(unix)]
    #[test]
    fn failed_video_marker_validation_still_runs_bounded_prune() {
        let sock = unique_dir("video-post-publish-prune");
        ensure_private_dir(&sock).unwrap();
        let recording = control_auth::confine_video_dir(&sock).unwrap();
        let path = recording.path().to_path_buf();
        let root = path.parent().unwrap().to_path_buf();
        let oldest = root.join("rec-00000000000000000000-100");
        for sequence in 100..112 {
            let fixture = root.join(format!("rec-00000000000000000000-{sequence:03}"));
            std::fs::create_dir(&fixture).unwrap();
            std::fs::write(fixture.join("index.json"), b"{\"frames\":[]}").unwrap();
            std::fs::write(
                fixture.join(control_auth::VIDEO_PUBLISHED_FILE),
                b"published",
            )
            .unwrap();
        }

        let index = recording
            .write_new_private(std::ffi::OsStr::new("index.json"), b"{\"frames\":[]}")
            .unwrap();
        let mut publication = VideoPublication {
            frame_seals: Vec::new(),
            index_file: index,
            published_marker: None,
            dir: recording,
        };
        publication.prepare().unwrap();

        let marker = path.join(control_auth::VIDEO_PUBLISHED_FILE);
        let displaced_marker = path.join(".published-displaced");
        let marker_for_hook = marker.clone();
        let mut retention = VideoReplyRetention { publication };
        let error = retention
            .prepare_write_with(move |publication| {
                publication.publish_marker_with_test_hook(move || {
                    std::fs::rename(&marker_for_hook, &displaced_marker).unwrap();
                    std::fs::write(&marker_for_hook, b"replacement").unwrap();
                })
            })
            .expect_err("replacement after visibility must fail marker identity validation");
        assert!(error.contains("video publish marker failed"));
        assert!(
            retention.publication.is_published(),
            "the atomic visibility boundary, not the fallible return, owns publication state"
        );
        assert!(
            marker.is_file(),
            "the once-visible recording remains complete"
        );

        drop(retention);
        control_auth::wait_for_artifact_cleanup_for_test(&path);
        assert!(
            !oldest.exists(),
            "a post-publication failure still runs the bounded retention sweep"
        );
        assert!(
            path.join("index.json").is_file() && marker.is_file(),
            "the irrevocable fresh recording survives its own sweep"
        );

        let _ = std::fs::remove_dir_all(sock);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_transaction_rejects_ancestor_replacement_without_leaking_payload() {
        use std::os::unix::fs::symlink;

        let root = unique_dir("snapshot-ancestor-swap");
        let outside = unique_dir("snapshot-ancestor-outside");
        let snapshots = root.join("snapshots");
        ensure_private_dir(&snapshots).unwrap();
        ensure_private_dir(&outside).unwrap();
        let path = snapshots.join("snapshot.png");
        let transaction = begin_snapshot_generation(&path).unwrap();
        let frame = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x0011_2233],
        };
        let moved = root.join("snapshots-moved");
        let snapshots_for_hook = snapshots.clone();
        let outside_for_hook = outside.clone();
        let moved_for_hook = moved.clone();

        let error = write_snapshot_artifacts_with_hook(
            &encode_frame_png(&frame),
            "private text",
            &transaction,
            move || {
                std::fs::rename(&snapshots_for_hook, &moved_for_hook).unwrap();
                symlink(&outside_for_hook, &snapshots_for_hook).unwrap();
            },
        )
        .expect_err("a replaced ancestor cannot commit a completion marker");
        assert!(error.contains("identity changed"), "precise error: {error}");
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "the replacement directory receives no payload or marker"
        );
        assert!(
            std::fs::read_dir(&moved).unwrap().next().is_none(),
            "the retained original directory is cleaned handle-relatively"
        );

        let _ = std::fs::remove_file(&snapshots);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn snapshot_generation_fence_rejects_old_worker_after_new_begin() {
        let model = aterm_models_misc::snapshot_generation_commit_model();
        let mut state = model.init_state();
        assert_eq!(snapshot_commit_plan(1, 1), SnapshotCommitPlan::Publish);
        assert!(model.fire("CommitCurrent", &mut state));
        assert!(model.fire("BeginNew", &mut state));
        assert_eq!(snapshot_commit_plan(1, 2), SnapshotCommitPlan::DiscardStale);
        assert!(model.fire("CommitOld", &mut state));
        assert!(model.check_invariant("CommittedPayloadIsCurrent", &state));

        let dir = unique_dir("snapshot-generation");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snapshot.png");
        let done_path = snapshot_sidecar_path(&path, ".done");
        let frame = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x0011_2233],
        };
        let transaction_a = begin_snapshot_generation(&path).unwrap();
        let transaction_b = begin_snapshot_generation(&path).unwrap();
        assert!(
            !done_path.exists(),
            "B begin invalidates every prior marker"
        );
        let stale = write_snapshot_artifacts(&encode_frame_png(&frame), "A", &transaction_a)
            .expect_err("A cannot commit after B begins");
        assert!(stale.contains("superseded"));
        assert!(
            !done_path.exists(),
            "once B begin returns, A can never republish the fixed marker"
        );
        write_snapshot_artifacts(&encode_frame_png(&frame), "B", &transaction_b)
            .expect("the current generation commits");
        assert!(done_path.is_file());

        assert!(model.fire("SelectCurrent", &mut state));
        assert_eq!(snapshot_commit_plan(2, 2), SnapshotCommitPlan::Publish);
        assert!(model.fire("CommitCurrent", &mut state));
        let mut bad = state.clone();
        bad.insert("payload", 1);
        assert!(
            !model.check_invariant("CommittedPayloadIsCurrent", &bad),
            "negative control: a current marker certifying stale payload is rejected"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_generation_transaction_cannot_certify_stale_payload() {
        let dir = unique_dir("snapshot-payload-identity");
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("snapshot.png");
        let text_path = snapshot_sidecar_path(&path, ".txt");
        let done_path = snapshot_sidecar_path(&path, ".done");
        let frame_a = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x00AA_1020],
        };
        let frame_b = Frame {
            width: 1,
            height: 1,
            pixels: vec![0x0010_BB30],
        };
        let transaction_a = begin_snapshot_generation(&path).unwrap();
        let transaction_b = begin_snapshot_generation(&path).unwrap();

        let (attempted_tx, attempted_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        write_snapshot_artifacts_with_hook(
            &encode_frame_png(&frame_b),
            "B payload",
            &transaction_b,
            move || {
                std::thread::spawn(move || {
                    attempted_tx.send(()).unwrap();
                    let result = write_snapshot_artifacts(
                        &encode_frame_png(&frame_a),
                        "A payload",
                        &transaction_a,
                    );
                    result_tx.send(result).unwrap();
                });
                attempted_rx.recv().unwrap();
            },
        )
        .expect("B transaction commits");
        let stale = result_rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("stale A finishes after B unlocks")
            .expect_err("A remains stale");
        assert!(stale.contains("superseded"));

        let png = std::fs::read(&path).unwrap();
        let (rgba, width, height) = aterm_render::decode_png_rgba8(&png).unwrap();
        assert_eq!((width, height), (1, 1));
        assert_eq!(rgba, frame_b.rgba_bytes(), "B marker certifies B pixels");
        assert_eq!(std::fs::read_to_string(text_path).unwrap(), "B payload");
        let done = std::fs::read_to_string(done_path).unwrap();
        assert!(done.starts_with("1x1\ngeneration="));
        assert!(done.contains("png_sha256=") && done.contains("text_sha256="));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_regression_platform_neutral_rgba_encoder_is_available() {
        let png = encode_rgba8_png(&[0x11, 0x22, 0x33, 0x80], 1, 1)
            .expect("the video/window encoder is available on every host");
        let mut reader = aterm_png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .expect("PNG header");
        assert_eq!(
            reader.info().srgb,
            Some(aterm_png::SrgbRenderingIntent::Perceptual)
        );
        let mut rgba = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut rgba).expect("PNG pixels");
        assert_eq!(&rgba[..info.buffer_size()], &[0x11, 0x22, 0x33, 0x80]);
    }

    fn pixel_take(frames: u64) -> aterm_gpu::video_tap::VideoTake {
        aterm_gpu::video_tap::VideoTake {
            frames: (1..=frames)
                .map(|seq| aterm_gpu::video_tap::CapturedFrame {
                    seq,
                    t_us: seq,
                    w: 1,
                    h: 1,
                    rgba: vec![
                        u8::try_from(seq).expect("fixture sequence fits u8"),
                        0,
                        0,
                        255,
                    ],
                })
                .collect(),
            dropped: 0,
            evicted: 0,
            decimated: 0,
            fps_cap: None,
            budget_bytes: 16 << 20,
            requested_ms: 100,
            w: 1,
            h: 1,
            device_px: (1, 1),
            half_res: false,
            format: "rgba8",
            resized_early_stop: false,
        }
    }

    fn run_video_dump(
        dir: control_auth::ConfinedVideoDir,
        take: aterm_gpu::video_tap::VideoTake,
        cancelled: bool,
        started_us: u64,
    ) -> (
        std::sync::mpsc::Receiver<crate::control::Retained<String>>,
        std::sync::Arc<crate::VideoExportState>,
    ) {
        let (reply, rx) = std::sync::mpsc::channel();
        let cancel = crate::VideoCancellation::new();
        let export = std::sync::Arc::new(crate::VideoExportState::default());
        let permit = export
            .try_begin(cancel.clone())
            .expect("fresh export permit");
        if cancelled {
            let _ = cancel.cancel();
        }
        run_encode_job(
            EncodeJob::VideoDump {
                take: take.into(),
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                mode: crate::VideoMode::SwapchainTap,
                keys_enabled: false,
                inputs: Vec::new(),
                pace_ticks: Vec::new(),
                unlogged_inputs: 0,
                unlogged_other_window: 0,
                unlogged_over_cap: 0,
                started_us,
                dir: Box::new(dir),
                reply,
                cancel,
                permit,
            },
            None,
        );
        (rx, export)
    }

    #[test]
    fn capture_regression_video_write_failure_has_no_completion_artifact() {
        let root = unique_dir("video-export-failure");
        ensure_private_dir(&root).unwrap();
        let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
        let dir_path = dir.path().to_path_buf();
        std::fs::create_dir(dir_path.join("frame_0001.png")).unwrap();
        let (rx, _export) = run_video_dump(dir, pixel_take(1), false, 0);
        let error = rx.recv().expect("failure reply");
        assert!(error.starts_with("ERR video:"), "protocol error: {error}");
        assert!(
            !dir_path.join("index.json").exists(),
            "a failed export never publishes its completion artifact"
        );
        assert!(
            !dir_path.exists(),
            "the server-owned partial export is removed rather than leaked forever"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn video_export_frame_seals_keep_file_descriptors_bounded() {
        if !crate::control_auth::enter_low_nofile_test_child(16) {
            return;
        }

        let root = crate::test_scratch::short_dir("the video root");
        ensure_private_dir(root.path()).unwrap();
        let dir =
            crate::control_auth::confine_video_dir(root.path()).expect("server-minted video dir");
        let dir_path = dir.path().to_path_buf();
        let (rx, _export) = run_video_dump(dir, pixel_take(32), false, 0);
        let mut retained = rx.recv().expect("video reply");
        assert!(
            retained.value.starts_with("OK frames=32 "),
            "bounded export completes: {}",
            retained.value
        );
        retained
            .retention
            .as_mut()
            .expect("successful video reply owns publication retention")
            .prepare_write()
            .expect("all sealed frames revalidate at the wire edge");
        assert!(dir_path.join("index.json").is_file());
        assert!(dir_path.join("frame_0032.png").is_file());
        drop(retained);
    }

    #[test]
    fn cancelled_video_export_aborts_dir_and_releases_busy_permit() {
        let root = unique_dir("video-export-cancel");
        ensure_private_dir(&root).unwrap();
        let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
        let dir_path = dir.path().to_path_buf();
        let (rx, export) = run_video_dump(dir, pixel_take(1), true, 0);
        assert!(
            rx.recv()
                .expect("cancel reply")
                .contains("request cancelled before export")
        );
        assert!(
            !dir_path.exists(),
            "cancellation removes the unpublished partial directory"
        );
        assert!(
            !export.is_busy(),
            "worker acknowledgement drops the sole export permit"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_image_metadata_binds_exact_snapshot_and_encoded_pixels() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let terminal = app.front_terminal_mirror(wid).expect("terminal front");
        crate::term_lock(&terminal.term).process(b"exact terminal metadata");
        let (view, session) = match app.windows[&wid].front_content.unwrap() {
            crate::front_content::FrontContent::Terminal { view, session } => (view, session),
            crate::front_content::FrontContent::Native { .. } => panic!("terminal front expected"),
        };

        let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        crate::reset_visible_leaf_plan_builds();
        app.render_image(image_req_for_test(
            control_auth::ConfinedImage::in_memory(),
            false,
            true,
            Some(&frame_metadata),
            tx,
        ));
        assert_eq!(
            crate::visible_leaf_plan_builds(),
            1,
            "full terminal image routing, preparation, hover and metadata share one visible plan",
        );
        let (width, height, png) = rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("terminal image reply")
            .expect("terminal image succeeds")
            .value;
        let png = png.expect("wire capture includes PNG bytes");
        let metadata = frame_metadata
            .get()
            .expect("terminal frame metadata is bound");
        assert_eq!(metadata.frame_kind, "terminal");
        assert_eq!(
            metadata.phase, "staged",
            "a headless frame is staged in its turn"
        );
        assert_eq!(metadata.view, Some(view.get()));
        assert_eq!((metadata.width, metadata.height), (width, height));
        assert_eq!(metadata.compiled_fingerprint, None);
        assert_eq!(metadata.leaves.len(), 1);
        let leaf = &metadata.leaves[0];
        assert_eq!(leaf.kind, "terminal");
        assert_eq!(leaf.view, view.get());
        assert_eq!(leaf.session, Some(session));
        assert!(leaf.focused);

        let retained = &app.windows[&wid].input_scratch;
        assert_eq!(leaf.snapshot_seq, Some(retained.snapshot_seq));
        assert_eq!(
            metadata.raster_model_fingerprint,
            App::terminal_snapshot_fingerprint(view.get(), session, retained),
        );
        assert_eq!(
            metadata.raster_geometry,
            App::terminal_geometry_fingerprint(width as usize, height as usize, retained),
        );

        let (rgba, decoded_width, decoded_height) =
            aterm_render::decode_png_rgba8(&png).expect("encoded terminal PNG decodes");
        assert_eq!(
            (decoded_width as u32, decoded_height as u32),
            (width, height)
        );
        let (rgba_pixels, remainder) = rgba.as_chunks::<4>();
        assert!(remainder.is_empty(), "decoded RGBA8 has complete pixels");
        let pixels = rgba_pixels
            .iter()
            .map(|pixel| {
                (u32::from(pixel[0]) << 16) | (u32::from(pixel[1]) << 8) | u32::from(pixel[2])
            })
            .collect::<Vec<_>>();
        let encoded_pixel_fingerprint =
            App::image_pixel_fingerprint(decoded_width, decoded_height, &pixels);
        assert_eq!(metadata.pixel_fingerprint, encoded_pixel_fingerprint);
        assert_eq!(metadata.raster_fingerprint, encoded_pixel_fingerprint);
        assert_eq!(leaf.raster_fingerprint, Some(encoded_pixel_fingerprint));

        let wire = metadata.wire_fields();
        assert!(wire.contains("frame-kind=terminal"));
        assert!(wire.contains(&format!("view={}", view.get())));
        assert!(wire.contains(&format!("v{}:terminal:s{}:f1:", view.get(), session)));
        assert!(wire.contains(&format!("seq{}", retained.snapshot_seq)));
    }

    fn image_metadata_now(
        app: &mut App,
        dir: &std::path::Path,
    ) -> crate::control::ImageFrameMetadata {
        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: confined(dir, "meta.png"),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(&metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        rx.recv_timeout(WORKER_PATIENCE)
            .expect("image reply")
            .expect("image succeeds");
        metadata.get().cloned().expect("frame metadata is bound")
    }

    #[test]
    fn native_raster_fingerprints_are_hashed_once_per_raster_pass() {
        let dir = unique_dir("native-raster-fingerprint-once");
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings view");

        let first = image_metadata_now(&mut app, &dir);
        let leaf_fingerprint = first.leaves[0].raster_fingerprint.expect("native leaf");
        let window = &app.windows[&wid];
        let raster = window.leaf_render_cache[&view].native.as_ref().unwrap();
        assert_eq!(
            raster.raster_fingerprint_once.get(),
            Some(&leaf_fingerprint),
            "the leaf raster keeps its hash"
        );
        assert_eq!(
            leaf_fingerprint,
            hash_raster(raster.width, raster.height, &raster.rgba)
        );
        let card = window
            .route_card
            .as_ref()
            .or(window.settings_card.as_ref())
            .expect("the native card");
        assert_eq!(
            card.raster_fingerprint_once.get(),
            Some(&first.raster_fingerprint),
            "the card keeps its hash"
        );
        assert_eq!(
            first.raster_fingerprint,
            hash_raster(card.pw, card.ph, &card.rgba)
        );

        let second = image_metadata_now(&mut app, &dir);
        assert_eq!(second.leaves[0].raster_fingerprint, Some(leaf_fingerprint));
        assert_eq!(second.raster_fingerprint, first.raster_fingerprint);

        app.invalidate_native_view_cache(
            wid,
            view,
            crate::native_app::DamageRegion::Rect {
                x: 7,
                y: 11,
                width: 19,
                height: 23,
            },
        );
        assert!(app.prepare_native_input_scratch(wid));
        let raster = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap();
        assert!(
            matches!(raster.last_work, crate::NativeRasterWork::Region { .. }),
            "the damage took a regional pass"
        );
        assert_eq!(
            raster.raster_fingerprint_once.get(),
            None,
            "the pass took the stored hash"
        );
        let third = image_metadata_now(&mut app, &dir);
        let raster = app.windows[&wid].leaf_render_cache[&view]
            .native
            .as_ref()
            .unwrap();
        assert_eq!(
            third.leaves[0].raster_fingerprint,
            Some(hash_raster(raster.width, raster.height, &raster.rgba)),
            "the next capture hashes the patched pixels"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn native_image_metadata_binds_dimensions_to_the_same_audit_frame() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config.show_build_badge = Some(true);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        let (_, view) = app.active_native_view(wid).expect("Settings view");
        let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(&metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        let (width, height, png) = rx
            .recv_timeout(WORKER_PATIENCE)
            .expect("native image reply")
            .expect("native image succeeds")
            .value;
        assert!(width > 0 && height > 0);
        assert!(png.is_some());

        let metadata = metadata.get().expect("native frame metadata is bound");
        let route_card = app.windows[&wid]
            .route_card
            .as_ref()
            .expect("enabled build badge creates a composed route card");
        let native_card = app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("native card remains retained");
        assert_ne!(
            route_card.fp, native_card.fp,
            "negative control: route and base native cards have distinct identities"
        );
        assert_eq!(
            metadata.raster_model_fingerprint, route_card.fp,
            "metadata names the route card actually passed to the renderer"
        );
        assert_eq!(
            metadata.raster_geometry, route_card.geom,
            "metadata geometry comes from the rendered route card"
        );
        assert_eq!(
            metadata.phase, "staged",
            "headless capture has no OS application-present target"
        );
        assert_eq!(metadata.frame_kind, "native");
        assert_eq!(metadata.view, Some(view.get()));
        assert_eq!((metadata.width, metadata.height), (width, height));
        assert_ne!(metadata.pixel_fingerprint, 0);
        let generation = metadata.generation.unwrap();
        let config_revision = metadata.config_revision.unwrap();
        let update_revision = metadata.update_revision.unwrap();
        let presentation_revision = metadata.presentation_revision.unwrap();
        let paint_revision = metadata.paint_revision.unwrap();
        let geometry = metadata.leaves[0].geometry.unwrap();
        let compiled_fingerprint = metadata.compiled_fingerprint.unwrap();
        let audit = app
            .inspect_app(crate::app_control::InspectRequest::View {
                view,
                projection: aterm_types::app_inspection::InspectionProjection::Audit,
            })
            .unwrap();
        let source = &audit[1];
        for field in [
            format!("source={}", metadata.phase),
            format!("view={}", metadata.view.unwrap()),
            format!("generation={generation}"),
            format!("geometry={geometry:016x}"),
            format!("config-revision={config_revision}"),
            format!("update-revision={update_revision}"),
            format!("presentation-revision={presentation_revision}"),
            format!("paint-revision={paint_revision:016x}"),
            "model-current=true".to_string(),
            format!("capture-serial={}", metadata.capture_serial),
            format!("compiled-fingerprint={compiled_fingerprint:016x}"),
        ] {
            assert!(
                source.contains(&field),
                "Audit/source missing {field}: {source}"
            );
        }
        assert!(audit.iter().any(|line| {
            line.starts_with("paint-audit ")
                && line.contains(&format!("compiled-fingerprint={compiled_fingerprint:016x}"))
        }));
    }

    #[test]
    fn native_image_metadata_changes_for_paint_only_pixel_identity() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));

        let capture = |app: &mut App| {
            let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
            let (tx, rx) = std::sync::mpsc::channel();
            crate::reset_visible_leaf_plan_builds();
            app.render_image(image_req_for_test(
                control_auth::ConfinedImage::in_memory(),
                false,
                true,
                Some(&metadata),
                tx,
            ));
            assert_eq!(
                crate::visible_leaf_plan_builds(),
                1,
                "full native image routing, preparation, hover and metadata share one visible plan",
            );
            let image = rx
                .recv_timeout(WORKER_PATIENCE)
                .expect("paint identity image reply")
                .expect("paint identity image succeeds")
                .value;
            assert!(image.2.is_some());
            metadata.get().cloned().unwrap()
        };

        let before = capture(&mut app);
        app.theme.bg ^= 0x0008_1018;
        app.theme.fg ^= 0x0010_0804;
        app.installed_colors
            .tint_theme_for_test(0x0008_1018, 0x0010_0804);
        let after = capture(&mut app);
        assert_eq!(before.frame_kind, "native");
        assert_eq!(before.view, after.view);
        assert_eq!(before.generation, after.generation);
        assert_eq!(before.config_revision, after.config_revision);
        assert_eq!(
            before.compiled_fingerprint, after.compiled_fingerprint,
            "theme-only paint does not invent a different semantic tree"
        );
        assert_ne!(before.paint_revision, after.paint_revision);
        assert_ne!(before.theme_fingerprint, after.theme_fingerprint);
        assert_ne!(
            before.raster_fingerprint, after.raster_fingerprint,
            "retained native raster bytes identify paint-only changes"
        );
        assert_ne!(
            before.pixel_fingerprint, after.pixel_fingerprint,
            "the metadata binds the exact final RGBA frame"
        );
    }

    #[test]
    fn snapshot_text_tracks_terminal_native_and_mixed_visible_routes() {
        let wid = WindowId(0);

        let mut terminal_app = App::headless_for_test();
        terminal_app.tab_strip_rows = 0;
        let terminal = terminal_app.front_terminal(wid).unwrap().term.clone();
        crate::term_lock(&terminal).process(b"TERMINAL_SNAPSHOT_MARKER");
        terminal_app
            .prepare_terminal_capture_grid(wid)
            .expect("terminal route stages");
        let terminal_route = terminal_app.active_visible_content_route(wid).unwrap();
        let terminal_input = terminal_app.windows[&wid].input_scratch.clone();
        let terminal_text = terminal_app.snapshot_visible_text(
            wid,
            terminal_route,
            &terminal_app.active_visible_leaf_plan(wid).unwrap(),
            &terminal_input,
            0,
            usize::from(terminal_app.windows[&wid].cols),
        );
        assert_eq!(
            terminal_text,
            terminal_capture_text(
                &terminal_input,
                0,
                usize::from(terminal_app.windows[&wid].cols)
            ),
            "terminal-only SIGUSR1 text remains byte-for-byte compatible"
        );
        assert!(terminal_text.contains("TERMINAL_SNAPSHOT_MARKER"));

        let mut native_app = App::headless_for_test();
        assert!(native_app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        assert!(native_app.prepare_native_input_scratch(wid));
        let (_, native_view) = native_app.active_native_view(wid).unwrap();
        let native_route = native_app.active_visible_content_route(wid).unwrap();
        let native_input = native_app.windows[&wid].input_scratch.clone();
        let native_text = native_app.snapshot_visible_text(
            wid,
            native_route,
            &native_app.active_visible_leaf_plan(wid).unwrap(),
            &native_input,
            usize::from(native_app.tab_strip_rows),
            usize::from(native_app.windows[&wid].cols),
        );
        assert!(native_text.contains(&format!("[native view={} focused=true]", native_view.get())));
        assert!(
            native_text.contains("text key="),
            "native SIGUSR1 text projects the retained semantic tree: {native_text}"
        );

        let (session, terminal_view) = native_app
            .split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let mixed_terminal = native_app.pool.get(session).unwrap().term.clone();
        crate::term_lock(&mixed_terminal).process(b"MIXED_SNAPSHOT_MARKER");
        assert!(
            native_app
                .prepare_heterogeneous_input_scratch(wid)
                .is_some()
        );
        let mixed_route = native_app.active_visible_content_route(wid).unwrap();
        let mixed_input = native_app.windows[&wid].input_scratch.clone();
        let mixed_text = native_app.snapshot_visible_text(
            wid,
            mixed_route,
            &native_app.active_visible_leaf_plan(wid).unwrap(),
            &mixed_input,
            usize::from(native_app.tab_strip_rows),
            usize::from(native_app.windows[&wid].cols),
        );
        assert!(mixed_text.contains("MIXED_SNAPSHOT_MARKER"));
        assert!(mixed_text.contains(&format!("[native view={}", native_view.get())));
        assert!(mixed_text.contains("text key="));
        assert_ne!(
            native_app.windows[&wid].tab_set.active().unwrap().focus,
            native_view,
            "negative control: native semantics are retained even while terminal view {} owns focus",
            terminal_view.get()
        );

        let base_card_fp = native_app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("mixed native layer is rasterized")
            .fp;
        native_app.palette_enter();
        let palette_lines = native_app.windows[&wid]
            .palette()
            .expect("palette is visibly open")
            .controls_lines();
        assert!(
            native_app
                .prepare_heterogeneous_input_scratch(wid)
                .is_some()
        );
        let palette_card_fp = native_app.windows[&wid]
            .settings_card
            .as_ref()
            .expect("visible palette is lowered into the mixed PNG route card")
            .fp;
        assert_ne!(
            base_card_fp, palette_card_fp,
            "negative control: opening the palette changes the raster captured by the PNG route"
        );
        let palette_route = native_app.active_visible_content_route(wid).unwrap();
        let palette_input = native_app.windows[&wid].input_scratch.clone();
        let palette_text = native_app.snapshot_visible_text(
            wid,
            palette_route,
            &native_app.active_visible_leaf_plan(wid).unwrap(),
            &palette_input,
            usize::from(native_app.tab_strip_rows),
            usize::from(native_app.windows[&wid].cols),
        );
        assert!(palette_text.contains("MIXED_SNAPSHOT_MARKER"));
        assert!(palette_text.contains(&format!("[native view={}", native_view.get())));
        let expected_palette_suffix = format!("{}\n", palette_lines.join("\n"));
        assert!(
            palette_text.ends_with(&expected_palette_suffix),
            "SIGUSR1 text must append the exact serializer for the palette painted in the PNG: {palette_text}"
        );
    }

    #[test]
    fn mixed_composite_paints_every_terminal_selection_and_anchors_the_focused_one() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let retained_at = Instant::now();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, native_view) = app.active_native_view(wid).expect("Settings view");
        let (terminal_session, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        let terminal = app
            .pool
            .get(terminal_session)
            .expect("terminal session")
            .term
            .clone();
        {
            let mut terminal = crate::term_lock(&terminal);
            terminal.process(b"\x1b]17;rgb:21/43/65\x07\x1b]19;rgb:fe/dc/ba\x07");
            let selection = terminal.text_selection_mut();
            selection.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(2, 2, SelectionSide::Right);
            selection.complete_selection();
        }
        let plan = app.active_visible_leaf_plan(wid).expect("mixed plan");
        let terminal_leaf = plan
            .leaves
            .iter()
            .find(|leaf| leaf.view == terminal_view)
            .expect("terminal leaf");
        let native_leaf = plan
            .leaves
            .iter()
            .find(|leaf| leaf.view == native_view)
            .expect("native leaf");
        let terminal_rect = (
            terminal_leaf.rect.origin.y.round().max(0.0) as usize,
            terminal_leaf.rect.origin.x.round().max(0.0) as usize,
            terminal_leaf.rect.size.height.round().max(1.0) as usize,
            terminal_leaf.rect.size.width.round().max(1.0) as usize,
        );
        let native_point = (
            native_leaf.rect.origin.y.round().max(0.0) as usize,
            native_leaf.rect.origin.x.round().max(0.0) as usize,
        );
        {
            let window = app.windows.get_mut(&wid).unwrap();
            window.fx.cursor_mut().glow_scratch = vec![aterm_render::GlowQuad {
                row: u16::try_from(terminal_rect.0).unwrap(),
                x: 0,
                y: 0,
                w: u16::MAX,
                h: u16::MAX,
                color: 0x0042_84C6,
                alpha: 0,
                color2: 0x0042_84C6,
                alpha2: 0,
            }];
            window.fx.cursor_mut().trail_scratch = vec![
                aterm_render::TrailCell {
                    row: terminal_rect.0,
                    col: terminal_rect.1,
                    alpha: 210,
                },
                aterm_render::TrailCell {
                    row: native_point.0,
                    col: native_point.1,
                    alpha: 1,
                },
            ];
            window.composed_cursor_effect_valid = true;
            window.composed_cursor_effect_session = Some(terminal_session);
            window.composed_cursor_fill = Some(0x0012_3456);
            window.composed_cursor_trail_color = 0x0065_4321;
        }

        assert!(
            app.prepare_heterogeneous_input_scratch_with_cursor_fx(
                wid,
                Some(crate::app_render::ComposedCursorFxClock::Retain {
                    observed_at: retained_at,
                }),
            )
            .is_some()
        );
        let input = &app.windows[&wid].input_scratch;
        let (row, col, pane_rows, pane_cols) = terminal_rect;
        assert_eq!(
            input.selection_clip,
            Some(aterm_render::SelectionClip::new(
                row,
                row + pane_rows,
                col,
                col + pane_cols,
            ))
        );
        assert_eq!(input.selection_bg, 0x0021_4365);
        assert_eq!(input.selection_fg, 0x00fe_dcba);
        assert!(input.selection_contains_cell(row + 1, col + pane_cols - 1, false, false));
        assert_eq!(input.cursor_glow_add.len(), 1);
        assert_eq!(
            input.cursor_trail,
            vec![aterm_render::TrailCell {
                row: terminal_rect.0,
                col: terminal_rect.1,
                alpha: 210,
            }],
            "a mixed frame projects effects only into its focused terminal leaf"
        );
        assert_eq!(input.cursor_fill_override, Some(0x0012_3456));

        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(native_view);
        app.sync_window(wid);
        assert!(
            app.prepare_heterogeneous_input_scratch_with_cursor_fx(
                wid,
                Some(crate::app_render::ComposedCursorFxClock::Retain {
                    observed_at: retained_at,
                }),
            )
            .is_some()
        );
        let input = &app.windows[&wid].input_scratch;
        assert!(!input.selection.has_selection());
        assert_eq!(input.selection_clip, None);
        assert_eq!(input.selections[0].bg, 0x0021_4365);
        assert!(
            input.selection_contains_cell(row + 1, col + pane_cols - 1, false, false),
            "the unfocused terminal pane keeps painting its selection"
        );
        assert!(
            input.cursor_glow_add.is_empty()
                && input.cursor_trail.is_empty()
                && input.cursor_fill_override.is_none(),
            "native focus clears the terminal-only cursor-effect authority"
        );
        assert!(
            !app.windows[&wid].composed_cursor_effect_valid,
            "a later terminal focus cannot resurrect native-focused stale effects"
        );
    }

    #[test]
    fn headless_native_capture_composes_every_mixed_leaf_for_either_focus() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, native_view) = app.active_native_view(wid).expect("Settings view");
        let (_, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert_eq!(
            app.windows[&wid].tab_set.active().unwrap().focus,
            terminal_view
        );

        let capture = |app: &mut App| {
            let (tx, rx) = std::sync::mpsc::channel();
            let frame_metadata = std::sync::Arc::new(std::sync::OnceLock::new());
            crate::reset_visible_leaf_plan_builds();
            app.render_image(image_req_for_test(
                control_auth::ConfinedImage::in_memory(),
                false,
                true,
                Some(&frame_metadata),
                tx,
            ));
            assert_eq!(
                crate::visible_leaf_plan_builds(),
                1,
                "full mixed image routing, preparation, hover and metadata share one visible plan",
            );
            let image = rx
                .recv_timeout(WORKER_PATIENCE)
                .expect("mixed capture reply")
                .expect("mixed capture succeeds")
                .value;
            (image, frame_metadata.get().cloned().unwrap())
        };
        let (terminal_focused, terminal_metadata) = capture(&mut app);
        assert!(terminal_focused.0 > 0 && terminal_focused.1 > 0);
        assert!(terminal_focused.2.is_some());
        assert_eq!(terminal_metadata.frame_kind, "composite");
        assert_eq!(terminal_metadata.view, None);
        assert_eq!(terminal_metadata.leaves.len(), 2);
        let terminal_wire = terminal_metadata.wire_fields();
        assert!(terminal_wire.contains("frame-kind=composite"));
        assert!(terminal_wire.contains("view=-"));
        assert!(terminal_wire.contains("compiled-fingerprint=-"));
        assert!(terminal_wire.contains("leaf-count=2"));
        assert!(terminal_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "terminal" && leaf.focused && leaf.view == terminal_view.get()
        }));
        assert!(terminal_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "native" && !leaf.focused && leaf.view == native_view.get()
        }));
        assert!(app.windows[&wid].settings_card.is_some());
        assert!(
            app.windows[&wid].leaf_render_cache[&native_view]
                .native
                .is_some(),
            "terminal focus still captures the native sibling"
        );

        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(native_view);
        app.sync_window(wid);
        let (native_focused, native_metadata) = capture(&mut app);
        assert_eq!(
            (native_focused.0, native_focused.1),
            (terminal_focused.0, terminal_focused.1)
        );
        assert!(native_focused.2.is_some());
        assert_eq!(native_metadata.frame_kind, "composite");
        assert_eq!(native_metadata.view, None);
        assert!(native_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "native" && leaf.focused && leaf.view == native_view.get()
        }));
        assert!(native_metadata.leaves.iter().any(|leaf| {
            leaf.kind == "terminal" && !leaf.focused && leaf.view == terminal_view.get()
        }));
        assert!(
            app.windows[&wid]
                .leaf_render_cache
                .contains_key(&terminal_view),
            "native focus still captures the terminal sibling"
        );
    }

    #[test]
    fn mixed_plain_capture_strips_effects_from_an_owned_copy_only() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, terminal_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        let terminal_leaf = app
            .active_visible_leaf_plan(wid)
            .unwrap()
            .leaf(terminal_view)
            .expect("terminal leaf")
            .clone();
        let trail = aterm_render::TrailCell {
            row: usize::from(app.tab_strip_rows)
                + terminal_leaf.rect.origin.y.round().max(0.0) as usize
                + (terminal_leaf.rect.size.height * 0.5).floor().max(0.0) as usize,
            col: terminal_leaf.rect.origin.x.round().max(0.0) as usize
                + (terminal_leaf.rect.size.width * 0.5).floor().max(0.0) as usize,
            alpha: 255,
        };
        let decoration = aterm_render::WordDecoration {
            row: u16::try_from(trail.row).expect("fixture row fits"),
            col: u16::try_from(trail.col).expect("fixture column fits"),
            dx: 0,
            dy: 0,
            glyph: aterm_render::DecoGlyph::Paw,
            blend: aterm_render::DecoBlend::Over,
            color: 0x00FF_00FF,
            alpha: 255,
        };
        {
            let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
            assert!(trail.row < input.rows && trail.col < input.cols);
            input.cursor_trail = vec![trail];
            input.cursor_trail_color = 0x00FF_FFFF;
            input.word_decorations = vec![decoration];
        }
        let presented = app.windows[&wid].input_scratch.clone();
        let capture = |app: &mut App, clean: bool, input: aterm_render::RenderInput| {
            let (tx, rx) = std::sync::mpsc::channel();
            let capture =
                retained_capture(app, input, crate::app_render::HostVisualState::default());
            app.image_tail(
                wid,
                capture,
                crate::control::ImageReq {
                    handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                    target: control_auth::ConfinedImage::in_memory(),
                    clean,
                    session: None,
                    want_bytes: true,
                    want_metadata: false,
                    frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
                    cancel: crate::control::CaptureCancellation::new(),
                    reply: tx,
                },
            );
            rx.recv_timeout(WORKER_PATIENCE)
                .expect("mixed plain reply")
                .expect("mixed plain capture")
                .value
                .2
                .expect("inline PNG bytes")
        };

        let styled = capture(&mut app, false, presented.clone());
        let plain = capture(&mut app, true, presented);
        let png_fingerprint = |bytes: &[u8]| {
            use std::hash::{Hash, Hasher};

            let mut hash = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut hash);
            hash.finish()
        };
        assert_ne!(
            png_fingerprint(&styled),
            png_fingerprint(&plain),
            "the styled mixed frame contains terminal effects that plain removes"
        );
        assert_eq!(
            app.windows[&wid].input_scratch.cursor_trail,
            vec![trail],
            "plain capture never mutates the retained application-present frame"
        );
        assert_eq!(
            app.windows[&wid].input_scratch.word_decorations,
            vec![decoration],
            "plain capture never mutates retained sparkle decorations"
        );
    }

    #[test]
    fn video_dump_reply_keeps_path_last_and_reports_honest_coverage() {
        let root = unique_dir("video-dump");
        ensure_private_dir(&root).unwrap();
        let dir = crate::control_auth::confine_video_dir(&root).expect("server-minted video dir");
        let dir_path = dir.path().to_path_buf();
        let frame = |seq: u64, t_us: u64, rgba: Vec<u8>| aterm_gpu::video_tap::CapturedFrame {
            seq,
            t_us,
            w: 2,
            h: 2,
            rgba,
        };
        let mut lit = vec![0u8; 16];
        lit[5] = 1;
        let frames = [frame(4, 1_000, vec![0u8; 16]), frame(5, 2_000, lit)];
        let fps = frames
            .each_ref()
            .map(crate::video_key_analysis::frame_fingerprint);
        assert_ne!(fps[0], fps[1]);
        let rows = [
            format!(
                "{{\"n\":1,\"seq\":4,\"t_us\":1000,\"fp\":{},\"delta\":0,",
                fps[0]
            ),
            format!(
                "{{\"n\":2,\"seq\":5,\"t_us\":2000,\"fp\":{},\"delta\":1,",
                fps[1]
            ),
        ];
        let take = aterm_gpu::video_tap::VideoTake {
            frames: frames.into(),
            dropped: 1,   // ring skips (mid-stream)
            evicted: 3,   // budget evictions (head truncation)
            decimated: 2, // fps= gate (deliberate, not a loss)
            fps_cap: Some(10),
            budget_bytes: 64 << 20,
            requested_ms: 8_000,
            w: 2,
            h: 2,
            device_px: (4, 4),
            half_res: true,
            format: "rgba8",
            resized_early_stop: false,
        };
        let (rx, _export) = run_video_dump(dir, take, false, 500);
        let reply = rx.recv().expect("dump reply");
        assert!(reply.starts_with("OK "), "reply: {reply}");
        let toks: Vec<&str> = reply.split_whitespace().collect();
        assert_eq!(
            toks.last().copied(),
            Some(dir_path.join("index.json").display().to_string().as_str()),
            "the path must remain the LAST whitespace token"
        );
        assert!(toks.contains(&"frames=2"), "reply: {reply}");
        assert!(
            toks.contains(&"dropped=4"),
            "dropped stays the TOTAL loss (1 ring + 3 evicted): {reply}"
        );
        let ht = toks.iter().position(|t| *t == "head_truncated=true");
        assert!(
            ht.is_some_and(|i| i < toks.len() - 1),
            "head_truncated goes strictly BEFORE the path: {reply}"
        );
        let index =
            std::fs::read_to_string(dir_path.join("index.json")).expect("index.json written");
        for key in [
            "\"head_truncated\": true",
            "\"evicted_frames\": 3",
            "\"ring_skipped\": 1",
            "\"decimated_frames\": 2",
            "\"requested_ms\": 8000",
            "\"covered_us\": [1000, 2000]",
            "\"fps_cap\": 10",
            "\"budget_mib\": 64",
            "\"mode\": \"swapchain-tap\"",
        ] {
            assert!(index.contains(key), "index.json missing {key}:\n{index}");
        }
        for row in &rows {
            assert!(
                index.contains(row.as_str()),
                "index.json missing {row}:\n{index}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn headless_capture_emits_and_collects_an_eligible_cat() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        app.config.cursor_trail = Some(false);
        let t0 = Instant::now();
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.pending_deco_birth = Some(t0);
            let mut term = crate::term_lock(&terminal);
            term.process(b"\r\n\r\n\r\nhello kitty friend");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }

        let first_capture = t0 + Duration::from_secs(15);
        app.capture_words(wid, first_capture);

        {
            let ws = &app.windows[&wid];
            assert!(
                !aterm_render::sprites::published(&ws.input_scratch.fx_layers).is_empty(),
                "first headless capture after output includes the peeking cat"
            );
            assert!(
                aterm_render::sprites::published(&ws.input_scratch.fx_layers)
                    .iter()
                    .all(|s| !s.art.is_empty()),
                "a visible cat must publish its atlas"
            );
        }
        assert_eq!(app.kitty_log.log().sightings, 1);
        assert!(
            app.kitty_log
                .log()
                .collectibles
                .iter()
                .any(|item| item.count > 0),
            "the same visible sprite must be observed as a collectible"
        );
        assert_eq!(
            app.kitty_log.favourite_look(),
            None,
            "an ambient discovery never becomes the companion"
        );
        assert!(
            !app.windows[&wid].fx.companions().cursor_cat.is_active(),
            "and no discovery hello starts from a capture"
        );

        app.capture_words(wid, first_capture + Duration::from_secs(30));
        assert!(
            aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
                .iter()
                .all(|sprite| sprite.z != aterm_render::sprites::FreeZ::OverText),
            "an ambient discovery presents no over-text cursor hello"
        );
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "a no-damage capture continues the episode without recollecting it"
        );

        app.capture_words(wid, first_capture + Duration::from_secs(40));
        assert!(
            aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
                .iter()
                .all(|sprite| sprite.z != aterm_render::sprites::FreeZ::UnderText),
            "the preview is created only by a damaged rescan"
        );
        assert_eq!(app.kitty_log.log().sightings, 1);
    }

    #[test]
    fn a_capture_of_another_tab_carries_only_that_tabs_words() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let t0 = Instant::now();
        app.recompute_sparkle();
        let session_a = app.front_terminal(wid).expect("front terminal").session;
        let write = |app: &App, session: u64, bytes: &[u8]| {
            let term = app.pool.get(session).expect("pane session").term.clone();
            crate::term_lock(&term).process(bytes);
        };
        write(&app, session_a, b"\r\n\r\n\r\nhello kitty friend");
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.pending_deco_birth = Some(t0);
        }
        app.capture_words(wid, t0);
        let (cell_w, cell_h) = {
            let (w, h) = app.win_cell_size(wid);
            (w as i32, h as i32)
        };
        assert!(
            cell_h > 0 && cell_w > 0,
            "PRECONDITION: the fixture has real cell metrics"
        );
        let cat_at_col = |app: &App, col: u16| {
            aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
                .iter()
                .any(|s| (s.x - i32::from(col) * cell_w).abs() <= cell_w)
        };
        let first_word_col: u16 = 6;
        assert!(
            cat_at_col(&app, first_word_col),
            "PRECONDITION: tab A's own cat really stands at its own word"
        );

        let session_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(session_b));
        assert_eq!(
            app.front_terminal(wid).expect("front terminal").session,
            session_b,
            "PRECONDITION: the new tab is the one on glass now"
        );
        let tab_b_col: u16 = 30;
        write(
            &app,
            session_b,
            format!("\x1b[6;{}Hkitty waits", tab_b_col + 1).as_bytes(),
        );
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.pending_deco_birth = Some(t0);
        }
        let at = t0 + Duration::from_millis(16);
        app.capture_words(wid, at);
        assert!(
            cat_at_col(&app, tab_b_col),
            "tab B's OWN cat must be the cat in this image"
        );
        assert!(
            !cat_at_col(&app, first_word_col),
            "and tab A's word list must not have survived the switch — this \
             capture is of tab B's grid, where that column is blank"
        );

        app.switch_tab_in(wid, 0);
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.pending_deco_birth = Some(at);
        }
        assert_eq!(
            app.front_terminal(wid).expect("front terminal").session,
            session_a,
            "PRECONDITION: tab A is front again"
        );
        app.capture_words(wid, at + Duration::from_millis(16));
        assert!(
            cat_at_col(&app, first_word_col),
            "and tab A's own capture has its own cat back"
        );
    }

    #[test]
    fn headless_capture_preserves_complete_curse_wince_for_next_frame() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let t0 = Instant::now();
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.fx
                .companions_mut()
                .cursor_cat
                .on_collect(t0, aterm_effects::kitty_registry::KittyLook::default());
            ws.fx
                .companions_mut()
                .cursor_cat
                .set_collection_presentable(t0, false);
            ws.pending_deco_birth = Some(t0);
            ws.fx.decos_mut().note_typed_edit(t0, None);
            let mut term = crate::term_lock(&terminal);
            term.process(b"fuck");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }

        let capture_at = t0 + Duration::from_millis(16);
        app.capture_words(wid, capture_at);
        let frame = app
            .windows
            .get_mut(&wid)
            .expect("window 0")
            .fx
            .companions_mut()
            .cursor_cat
            .static_frame(capture_at + Duration::from_millis(1));
        assert_eq!(
            frame.reaction,
            aterm_effects::kitty_cursor::CatReaction::Wince,
            "the silent capture drain must retain the visual wince"
        );
        assert_eq!(
            frame.pose,
            aterm_effects::kitty_cursor::CatPose::STILL,
            "headless reduced-motion capture keeps the wince expression static"
        );
    }

    #[test]
    fn capture_rescan_refreshes_cells_after_snapshot_race() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let now = Instant::now();
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            let mut term = crate::term_lock(&terminal);
            term.process(b"ordinary text");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);

            ws.pending_deco_birth = Some(now);
            term.process(b"\r\n\r\n\r\nhello kitty friend");
        }

        app.capture_words(wid, now);

        let ws = &app.windows[&wid];
        assert!(
            !aterm_render::sprites::published(&ws.input_scratch.fx_layers).is_empty(),
            "the splice rescans the post-snapshot word from fresh cells"
        );
        assert_eq!(
            app.kitty_log.log().sightings,
            1,
            "the fresh word's collectible is not consumed against stale cells"
        );
    }

    #[test]
    fn capture_birth_stamp_is_bounded_and_headless_preview_is_stable() {
        let now = Instant::now();
        assert_eq!(bounded_capture_birth(now, None), None);
        assert_eq!(
            bounded_capture_birth(now, Some(now + Duration::from_secs(1))),
            Some(now),
            "a future stamp sanitizes to capture time"
        );
        assert_eq!(
            now.duration_since(
                bounded_capture_birth(now, Some(now - Duration::from_secs(60)))
                    .expect("bounded stamp"),
            ),
            CAPTURE_BIRTH_MAX_AGE,
            "stale stamps clamp to the episode-retention horizon"
        );
        assert_eq!(
            now.duration_since(capture_rescan_birth(
                now,
                Some(now - Duration::from_secs(60)),
                true,
            )),
            CAPTURE_PREVIEW_AGE,
            "windowless first presentation samples a visible pose, not wall time"
        );
        assert_eq!(
            now.duration_since(capture_rescan_birth(
                now,
                Some(now - Duration::from_secs(6)),
                false,
            )),
            CAPTURE_PREVIEW_AGE,
            "an occluded window's capture-first presentation cannot age unseen past Done"
        );
        assert_eq!(
            now.duration_since(capture_rescan_birth(
                now,
                Some(now - Duration::from_millis(100)),
                false,
            )),
            Duration::from_millis(100),
            "a genuinely fresh windowed capture preserves its output timing"
        );
    }

    #[test]
    fn capture_during_freeze_preserves_and_resumes_episodes() {
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        let t0 = Instant::now();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty pet".into());
        app.recompute_sparkle();
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            let mut term = crate::term_lock(&terminal);
            term.process(b"a happy kitty naps");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }
        app.capture_words(wid, t0);
        assert!(
            app.windows[&wid].fx.decos().is_active(t0),
            "the fixture's feline effect is animating (sparkle defaults on)"
        );
        {
            let ws = app.windows.get_mut(&wid).expect("window 0");
            ws.fx
                .companions_mut()
                .cursor_cat
                .on_collect(t0, aterm_effects::kitty_registry::KittyLook::default());
            ws.fx
                .companions_mut()
                .cursor_cat
                .set_collection_presentable(t0, false);
        }
        app.perf_reduced = true;
        app.capture_words(wid, t0 + Duration::from_millis(100));
        {
            let ws = &app.windows[&wid];
            assert!(ws.input_scratch.word_decorations.is_empty());
            assert!(ws.input_scratch.ink.is_empty());
            assert!(ws.input_scratch.nova_add.is_empty());
            assert_eq!(
                ws.fx
                    .companions()
                    .cursor_cat
                    .static_deadline(t0 + Duration::from_millis(100)),
                None,
                "a suppressed capture keeps the collection hello paused"
            );
        }
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("window 0");
            let mut term = crate::term_lock(&terminal);
            term.process(b" zzz");
            term.cell_frame_into(&mut ws.input_scratch, ws.rows as usize, ws.cols as usize);
        }
        app.capture_words(wid, t0 + Duration::from_secs(20));
        assert!(
            app.windows[&wid].input_scratch.word_decorations.is_empty(),
            "still suspended: cleared channels"
        );
        assert_eq!(
            app.windows[&wid]
                .fx
                .companions()
                .cursor_cat
                .static_deadline(t0 + Duration::from_secs(20)),
            None,
            "twenty seconds of suppressed captures consume no hello time"
        );
        app.perf_reduced = false;
        let t1 = t0 + Duration::from_secs(20) + Duration::from_millis(50);
        app.capture_words(wid, t1);
        assert!(
            app.windows[&wid].fx.decos().is_active(t1),
            "episodes resume where they paused (freeze/thaw, not reset)"
        );
        assert!(
            aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
                .iter()
                .any(|sprite| sprite.z == aterm_render::sprites::FreeZ::UnderText
                    && *sprite != captured_resident_body_for_test(&app, wid)),
            "the resumed ambient episode emits its own cat; the resident cannot buy this pass"
        );
    }

    #[test]
    fn load_shed_capture_retires_an_unowned_pet_before_its_early_return() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let t0 = Instant::now();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty pet".into());
        app.recompute_sparkle();
        app.sparkle = None;
        app.capture_words(wid, t0);
        assert!(
            app.windows[&wid].fx.companion().brain().is_active(),
            "negative control: an owned capture materializes the resident pet"
        );

        app.perf_reduced = true;
        assert!(
            app.load_shed_active(),
            "fixture reaches the early-return arm"
        );
        app.capture_words(wid, t0 + Duration::from_millis(16));
        assert!(
            app.windows[&wid].fx.companion().brain().is_active(),
            "load shedding keeps the resident without revoking ownership"
        );
        assert!(
            app.windows[&wid]
                .fx
                .companion()
                .next_change_deadline(t0 + Duration::from_millis(16))
                .is_some()
        );

        app.config.cursor_trail = Some(false);
        assert!(
            app.load_shed_active(),
            "owner removal does not end load shedding"
        );
        app.capture_words(wid, t0 + Duration::from_millis(32));
        let ws = &app.windows[&wid];
        assert!(
            !ws.fx.companion().brain().is_active() && !ws.fx.companion().brain().needs_frames(),
            "owner removal must retire the brain before load shedding returns"
        );
        assert!(
            ws.fx.free_sprites().is_empty()
                && aterm_render::sprites::published(&ws.input_scratch.fx_layers).is_empty(),
            "the retired capture publishes no companion projection"
        );
        assert_eq!(
            ws.fx.companion().hit_rect(),
            None,
            "owner removal clears body publication too"
        );
    }

    #[test]
    fn encode_worker_survives_dropped_reply_receiver() {
        let dir = std::env::temp_dir().join(format!("aterm-encode-dead-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        let (dead_tx, dead_rx) = std::sync::mpsc::channel();
        drop(dead_rx);
        app.submit_encode_job(EncodeJob::Image {
            pixels: CapturePixels::packed(Frame {
                width: 1,
                height: 1,
                pixels: vec![0u32; 1],
            })
            .expect("a whole frame"),
            metadata: None,
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: confined(&dir, "dead.png"),
            want_bytes: false,
            cancel: crate::control::CaptureCancellation::new(),
            reply: dead_tx,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        app.submit_encode_job(EncodeJob::Image {
            pixels: CapturePixels::packed(Frame {
                width: 2,
                height: 2,
                pixels: vec![0u32; 4],
            })
            .expect("a whole frame"),
            metadata: None,
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: confined(&dir, "live.png"),
            want_bytes: false,
            cancel: crate::control::CaptureCancellation::new(),
            reply: tx,
        });
        let mut retained = rx
            .recv()
            .expect("worker alive after dead client")
            .expect("second encode succeeds");
        assert_eq!(retained.value, (2, 2, None));
        retained
            .retention
            .as_mut()
            .expect("file reply carries exact handles")
            .prepare_write()
            .expect("live reply validates at the wire edge");
        assert!(!dir.join("dead.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_image_retains_admission_through_the_wire_handoff() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-inline-image-retention-{}",
            std::process::id()
        ));
        ensure_private_dir(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        run_encode_job(
            EncodeJob::Image {
                pixels: CapturePixels::packed(Frame {
                    width: 2,
                    height: 2,
                    pixels: vec![0u32; 4],
                })
                .expect("a whole frame"),
                metadata: None,
                handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
                target: confined(&dir, "unused.png"),
                want_bytes: true,
                cancel: crate::control::CaptureCancellation::new(),
                reply: tx,
            },
            None,
        );
        let mut retained = rx.recv().expect("inline reply").expect("inline encode");
        assert!(retained.value.2.is_some(), "inline reply carries PNG bytes");
        retained
            .retention
            .as_mut()
            .expect("byte reply retains its admission permit")
            .prepare_write()
            .expect("in-memory wire payload is ready");
        assert!(
            !dir.join("unused.png").exists(),
            "inline capture never publishes a file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn needs_borders_reach_the_sacred_image_path_steady() {
        use crate::presence::Rim;
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(app.windows[&wid].os_window.is_none());
        let baseline = visual_capture_pixels(&mut app, None);
        assert_eq!(app.host_visual_state(wid, Instant::now()).overlay, None);
        let (w, h) = (baseline.width, baseline.height);
        let set_rim = |app: &mut App, rim: Rim| {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.presence.rim = rim;
            ws.presence.seed += 1;
        };
        let mut shots: Vec<(&str, Frame, crate::app_render::OverlayGlow)> = Vec::new();
        for (name, rim) in [
            ("stuck", Rim::Stuck),
            ("decide", Rim::Decide),
            ("yours", Rim::Yours),
        ] {
            set_rim(&mut app, rim);
            let overlay = app
                .host_visual_state(wid, Instant::now())
                .overlay
                .unwrap_or_else(|| panic!("{name}: the third arm"));
            let shot = visual_capture_pixels(&mut app, None);
            assert_eq!((shot.width, shot.height), (w, h));
            let mut expected = baseline.clone();
            apply_overlay_at(&mut expected.pixels, w, h, 0, 0, w, h, overlay);
            assert_ne!(expected.pixels, baseline.pixels, "{name}: a rim is visible");
            assert_eq!(
                shot.pixels, expected.pixels,
                "{name}: the sacred path paints the border"
            );
            assert_ne!(shot.pixels, baseline.pixels, "{name}: a border is visible");
            assert_ne!(
                shot.pixels[0], baseline.pixels[0],
                "{name}: the corner is border"
            );
            shots.push((name, shot, overlay));
        }
        let glow = |n: &str| shots.iter().find(|(m, _, _)| *m == n).unwrap();
        for name in ["stuck", "decide", "yours"] {
            assert_eq!(glow(name).2.wash_a, 0, "{name}: no wash");
            let mid = (h / 2) * w + w / 2;
            assert_eq!(
                glow(name).1.pixels[mid],
                baseline.pixels[mid],
                "{name}: the body is untouched"
            );
        }
        assert_eq!(
            glow("stuck").2.border_scale_q4,
            glow("decide").2.border_scale_q4
        );
        assert!(glow("yours").2.border_scale_q4 < glow("stuck").2.border_scale_q4);
        assert_ne!(glow("stuck").2.accent, glow("decide").2.accent);
        assert_ne!(glow("decide").2.accent, glow("yours").2.accent);

        set_rim(&mut app, Rim::None);
        assert_eq!(app.host_visual_state(wid, Instant::now()).overlay, None);
        assert_eq!(
            visual_capture_pixels(&mut app, None).pixels,
            baseline.pixels,
            "quiet again: the baseline"
        );

        set_rim(&mut app, Rim::Decide);
        app.config.motion = Some("reduced".to_string());
        let again = visual_capture_pixels(&mut app, None);
        assert_eq!(again.pixels, glow("decide").1.pixels, "the same image");
    }
}

#[cfg(test)]
mod window_render_context_capture_tests {
    use super::{App, image_req_for_test};
    use crate::{Backend, MetricsView, WindowId, control_auth};
    use aterm_core::terminal::CursorStyle;

    #[derive(Debug, PartialEq)]
    struct BoundContext {
        font_px: f32,
        cell_size: (usize, usize),
        pad: usize,
        pad_top: usize,
        head: usize,
        blink_phase: bool,
        cursor_style: Option<CursorStyle>,
        selection_inactive: bool,
    }

    fn bound_context(app: &App) -> BoundContext {
        let Backend::Cpu(renderer) = app.backend.ready() else {
            panic!("headless test backend is CPU")
        };
        BoundContext {
            font_px: app.font_px,
            cell_size: renderer.cell_size(),
            pad: renderer.pad(),
            pad_top: renderer.pad_top(),
            head: renderer.head(),
            blink_phase: renderer.cursor_blink_phase(),
            cursor_style: renderer.cursor_style_override(),
            selection_inactive: renderer.selection_inactive(),
        }
    }

    fn expected_frame_size(app: &App, wid: WindowId) -> (u32, u32) {
        let window = app.windows.get(&wid).expect("capture window");
        let metrics = window.metrics;
        let (cell_w, cell_h) = app.win_cell_size(wid);
        (
            u32::try_from(usize::from(window.cols) * cell_w + 2 * metrics.pad).unwrap(),
            u32::try_from(
                usize::from(window.rows) * cell_h + metrics.pad + metrics.pad_top + metrics.head,
            )
            .unwrap(),
        )
    }

    fn capture(app: &mut App, wid: WindowId, dir: &std::path::Path, name: &str) -> (u32, u32) {
        app.frontmost_window = Some(wid);
        let (tx, rx) = std::sync::mpsc::channel();
        app.render_image(image_req_for_test(
            control_auth::ConfinedImage::for_test(dir, name),
            false,
            false,
            None,
            tx,
        ));
        let mut retained = rx
            .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
            .expect("image worker reply")
            .expect("image capture succeeds");
        retained
            .retention
            .as_mut()
            .expect("file capture retains its exact path")
            .prepare_write()
            .expect("capture identity survives to the simulated wire edge");
        let (width, height, _) = retained.value;
        (width, height)
    }

    #[test]
    fn two_window_capture_rebinds_unequal_complete_render_contexts() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-window-render-context-{}",
            std::process::id()
        ));
        control_auth::ensure_private_dir(&dir).unwrap();
        let mut app = App::headless_for_test();
        app.render_knobs.selection_inactive = true;

        let first = WindowId(0);
        let session = app.next_session_id;
        let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
        let mut first_metrics = MetricsView::for_scale(1.0);
        first_metrics.head = 3;
        let mut second_metrics = MetricsView::for_scale(2.0);
        second_metrics.head = 11;
        assert_ne!(
            first_metrics, second_metrics,
            "the regression must exercise genuinely unequal render contexts"
        );
        {
            let window = app.windows.get_mut(&first).unwrap();
            window.scale = 1.0;
            window.metrics = first_metrics;
            window.blink_phase = false;
            window.focused = false;
        }
        {
            let window = app.windows.get_mut(&second).unwrap();
            window.scale = 2.0;
            window.metrics = second_metrics;
            window.blink_phase = true;
            window.focused = true;
        }

        app.font_px = second_metrics.font_px;
        app.backend.activate_px(second_metrics.font_px);
        app.backend.set_pad(second_metrics.pad);
        app.backend.set_pad_top(second_metrics.pad_top);
        app.backend.set_head(second_metrics.head);
        app.backend.set_cursor_blink_phase(true);
        app.backend
            .set_cursor_style_override(Some(CursorStyle::HollowBlock));
        app.backend.set_selection_inactive(false);

        assert_eq!(
            capture(&mut app, first, &dir, "first.png"),
            expected_frame_size(&app, first),
            "A's capture dimensions come from A's 1x metrics"
        );
        assert_eq!(
            bound_context(&app),
            BoundContext {
                font_px: first_metrics.font_px,
                cell_size: app.win_cell_size(first),
                pad: first_metrics.pad,
                pad_top: first_metrics.pad_top,
                head: first_metrics.head,
                blink_phase: false,
                cursor_style: None,
                selection_inactive: true,
            },
            "A owns every renderer-global channel at its raster seam"
        );

        assert_eq!(
            capture(&mut app, second, &dir, "second.png"),
            expected_frame_size(&app, second),
            "B's capture dimensions come from B's 2x metrics"
        );
        assert_eq!(
            bound_context(&app),
            BoundContext {
                font_px: second_metrics.font_px,
                cell_size: app.win_cell_size(second),
                pad: second_metrics.pad,
                pad_top: second_metrics.pad_top,
                head: second_metrics.head,
                blink_phase: true,
                cursor_style: None,
                selection_inactive: false,
            },
            "B cannot inherit A's typography or cursor/selection globals"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod headless_cursor_fx_tests {
    use super::{CaptureCompanionSongSync, image_req_for_test, sync_capture_companion_song};
    use crate::{App, WindowId, control_auth, term_lock};
    use std::time::{Duration, Instant};

    fn pet_app(motion: &str, split: bool) -> (App, WindowId) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty pet".into());
        app.config.motion = Some(motion.into());
        app.recompute_sparkle();
        app.sparkle = None;
        if split {
            app.split_active_stub_tab(wid);
        }
        app.windows.get_mut(&wid).unwrap().focused = true;
        (app, wid)
    }

    fn render_front_image(app: &mut App) {
        let (tx, rx) = std::sync::mpsc::channel();
        let target = control_auth::ConfinedImage::in_memory();
        app.render_image(image_req_for_test(target, false, true, None, tx));
        rx.recv_timeout(crate::app_introspect::WORKER_PATIENCE)
            .expect("image worker reply")
            .expect("headless image succeeds");
    }

    #[test]
    fn capture_owned_clock_syncs_and_retires_the_visual_singer_without_audio() {
        use aterm_effects::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
        use aterm_effects::kitty_sing::SING_ARM_REPEATS;

        let now = Instant::now();
        let start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
        let mut detector = aterm_effects::kitty_sing::KittySing::default();
        for i in 0..SING_ARM_REPEATS {
            detector.note_char(start + Duration::from_millis(u64::from(i) * 10), 0, 'a');
        }
        let mut cat = crate::kitty_cursor::CursorCat::default();
        let tenure_start = now - Duration::from_secs(7);
        for i in 0..160u64 {
            cat.on_pet_mode_motion_pulse(CursorCatMotionPulse {
                at: tenure_start + Duration::from_millis(i * 40),
                kind: CursorCatMotionKind::Advance,
            });
        }
        assert!(!cat.is_active(), "pet tenure stays hidden before the song");
        let mut glow = crate::cursor_glow::CursorGlow::default();
        let mut riff_bar = None;
        assert_eq!(
            sync_capture_companion_song(CaptureCompanionSongSync {
                style: crate::cursor_glow::GlowStyle::RainbowKitty,
                cursor_trail_enabled: true,
                cursor_companions_allowed: true,
                pet_mode: true,
                now,
                detector: &mut detector,
                cat: &mut cat,
                glow: &mut glow,
                riff_bar: &mut riff_bar,
            }),
            1.0
        );
        assert!(
            cat.is_active(),
            "the capture-owned song spends hidden tenure"
        );
        assert_eq!(cat.static_frame(now).sing, 1.0);
        assert!(riff_bar.is_some(), "the silent visual bar still advances");

        let drained = now + Duration::from_secs(2);
        assert_eq!(
            sync_capture_companion_song(CaptureCompanionSongSync {
                style: crate::cursor_glow::GlowStyle::RainbowKitty,
                cursor_trail_enabled: true,
                cursor_companions_allowed: true,
                pet_mode: true,
                now: drained,
                detector: &mut detector,
                cat: &mut cat,
                glow: &mut glow,
                riff_bar: &mut riff_bar,
            }),
            0.0
        );
        assert!(
            !cat.is_active(),
            "capture-owned pet mode grounds a fully drained singer"
        );
        assert_eq!(riff_bar, None);
    }

    #[test]
    fn single_and_split_capture_retain_the_pet_watching_selected_history() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        for split in [false, true] {
            let (mut app, wid) = pet_app("reduced", split);
            let start = Instant::now();
            let sid = app.focused_session_id(wid).unwrap();
            let term = app.pool.get(sid).unwrap().term.clone();
            let plan = app.active_visible_leaf_plan(wid).unwrap();
            let leaf = plan.leaves.iter().find(|leaf| leaf.focused).unwrap();
            let rows = leaf.rect.size.height.round() as usize;
            {
                let mut terminal = term_lock(&term);
                for _ in 0..rows + 20 {
                    terminal.process(b"line\r\n");
                }
            }
            app.capture_words(wid, start);
            assert!(app.windows[&wid].fx.companion().brain().is_active());
            term_lock(&term).scroll_display(10);

            for (step, selected) in [true, false].into_iter().enumerate() {
                {
                    let mut terminal = term_lock(&term);
                    let selection = terminal.text_selection_mut();
                    if selected {
                        selection.start_selection(
                            -4,
                            10,
                            SelectionSide::Left,
                            SelectionType::Simple,
                        );
                        selection.update_selection(-3, 16, SelectionSide::Right);
                    } else {
                        selection.clear();
                    }
                }
                let now = start + Duration::from_millis((step as u64 + 1) * 16);
                let capture = app
                    .prepare_terminal_capture_grid_with_cursor_fx(
                        wid,
                        crate::app_render::ComposedCursorFxClock::Advance(now),
                    )
                    .expect("real terminal capture extraction");
                let focus = capture.focus.unwrap();
                assert_eq!(capture.composed, split);
                assert!(!focus.live_viewport && focus.cursor.is_none());
                app.capture_words_sampled(wid, now, focus);
                assert_eq!(
                    app.windows[&wid]
                        .fx
                        .companion()
                        .brain()
                        .has_reading_interest(),
                    selected
                );
                let (cw, ch) = app.win_cell_size(wid);
                let sprites =
                    &aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers);
                if selected {
                    assert_eq!(sprites.len(), 1, "one full reading pet, split={split}");
                    assert!(usize::from(sprites[0].w) >= 5 * cw);
                    assert!(usize::from(sprites[0].h) >= ch);
                    assert_eq!(sprites[0].alpha, 255);
                } else {
                    assert!(
                        sprites.is_empty(),
                        "history without reading interest remains nonpresentable"
                    );
                }
            }
        }
    }

    #[test]
    fn headless_capture_retains_hidden_caret_home_and_clears_real_reset() {
        use aterm_core::terminal::{ContentScrollDelta, ContentScrollState};

        for split in [false, true] {
            let (mut app, wid) = pet_app("reduced", split);
            let start = Instant::now();
            let sid = app.focused_session_id(wid).unwrap();
            let term = app.pool.get(sid).unwrap().term.clone();
            let rows = {
                let mut terminal = term_lock(&term);
                assert!(terminal.grid().rows() >= 8 && terminal.grid().cols() >= 24);
                terminal.process(b"\x1b[6;12H");
                terminal.grid().rows()
            };
            for ago in (1..=6).rev() {
                app.capture_words(wid, start - Duration::from_millis(ago * 100));
            }
            app.capture_words(wid, start);
            let (cw, ch) = app.win_cell_size(wid);
            let full_body = |app: &App| {
                let ws = &app.windows[&wid];
                let bodies: Vec<_> = aterm_render::sprites::published(&ws.input_scratch.fx_layers)
                    .into_iter()
                    .filter(|s| usize::from(s.w) >= 5 * cw && usize::from(s.h) >= ch)
                    .collect();
                assert_eq!(bodies.len(), 1, "one resident, split={split}");
                let body = &bodies[0];
                assert_eq!(body.alpha, 255, "split={split}");
                let (ox, oy, _, _, _) =
                    app.effects_origin_win(wid, usize::from(ws.rows), usize::from(ws.cols), ch);
                let x = body.x + i32::from(ox);
                let y = body.y + i32::from(oy);
                assert_eq!(
                    ws.fx.companion().hit_rect(),
                    Some((x, x + i32::from(body.w), y, y + i32::from(body.h))),
                    "the published body must identify the actual emitted sprite"
                );
                (body.x, body.y, body.w, body.h)
            };
            let home = full_body(&app);
            let feet =
                |(x, y, w, h): (i32, i32, u16, u16)| (x + i32::from(w) / 2, y + i32::from(h));
            let input_seq = app.windows[&wid].fx.companion().brain().console_input_seq();
            term_lock(&term).process(b"\x1b[?25l");
            for (step, elapsed_ms) in [16, 600, 2000].into_iter().enumerate() {
                if step == 1 {
                    let mut terminal = term_lock(&term);
                    let before = terminal.content_scroll_state();
                    terminal.process(
                        format!("\x1b7\x1b[1;{}r\x1b[{};1H\n\x1b[r\x1b8", rows - 2, rows - 2)
                            .as_bytes(),
                    );
                    assert!(matches!(
                        ContentScrollState::delta_since(
                            Some(before),
                            terminal.content_scroll_state()
                        ),
                        ContentScrollDelta::Bands { .. }
                    ));
                }
                app.capture_words(wid, start + Duration::from_millis(elapsed_ms));
                assert_eq!(
                    feet(full_body(&app)),
                    feet(home),
                    "a hidden home remains screen-local"
                );
                assert!(!app.windows[&wid].fx.companion().brain().needs_frames());
                assert_eq!(
                    app.windows[&wid].fx.companion().brain().console_input_seq(),
                    input_seq
                );
            }
            let captured_at = start + Duration::from_millis(2016);
            let capture = app
                .prepare_terminal_capture_grid_with_cursor_fx_interleaved(
                    wid,
                    crate::app_render::ComposedCursorFxClock::Advance(captured_at),
                    || term_lock(&term).process(b"\x1bc\x1b[?25l"),
                )
                .expect("the already-authorized snapshot survives a later reset");
            let focus = capture.focus.unwrap();
            assert_ne!(
                focus.pet_world.stamp.surface.scroll_invalidation,
                term_lock(&term).content_scroll_state().invalidation_epoch,
                "the control must change the real terminal after extraction"
            );
            app.capture_words_sampled(wid, captured_at, focus);
            assert_eq!(
                feet(full_body(&app)),
                feet(home),
                "capture must consume only its owned snapshot"
            );

            for step in 0..20 {
                app.capture_words(wid, start + Duration::from_millis(2100 + step * 100));
            }
            assert_ne!(
                feet(full_body(&app)),
                feet(home),
                "reset must clear the former home, split={split}"
            );
            assert_eq!(
                app.windows[&wid].fx.companion().brain().console_input_seq(),
                input_seq
            );

            term_lock(&term).process(b"\x1b[?25h\x1b[6;12H");
            for (control, owner_removed) in [false, true].into_iter().enumerate() {
                app.windows.get_mut(&wid).unwrap().focused = true;
                app.config.cursor_trail = Some(true);
                let now = start + Duration::from_millis(5000 + control as u64 * 1000);
                for ago in (0..=6).rev() {
                    app.capture_words(wid, now - Duration::from_millis(ago * 100));
                }
                full_body(&app);
                if owner_removed {
                    app.config.cursor_trail = Some(false);
                } else {
                    app.windows.get_mut(&wid).unwrap().focused = false;
                }
                app.capture_words(wid, now + Duration::from_millis(16));
                assert!(
                    aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
                        .is_empty()
                );
                assert_eq!(app.windows[&wid].fx.companion().hit_rect(), None);
            }
        }
    }

    #[test]
    fn native_and_capture_keep_the_full_pet_under_load_without_idle_frames() {
        for split in [false, true] {
            let (mut app, wid) = pet_app("auto", split);
            app.config.load_adaptive_motion = Some(true);
            let now = Instant::now();
            for frame in 0..3 {
                app.note_present_cost(
                    30_000_000,
                    Duration::from_millis(16),
                    now - Duration::from_secs(2) + Duration::from_millis(frame * 16),
                );
            }
            assert!(
                app.load_shed_active(),
                "real present-cost samples engage the policy"
            );
            assert_eq!(
                app.effective_shed_envelope(app.config.motion_mode(), now),
                0.0
            );
            if split {
                let ws = &app.windows[&wid];
                let (rows, cols) = (usize::from(ws.rows), usize::from(ws.cols));
                let mut at = now;
                for _ in 0..60 {
                    assert!(
                        app.redraw_compose(wid, rows, cols, false, false, None, 0, at)
                            .is_some()
                    );
                    if app.windows[&wid].fx.companion().alpha() == 255 {
                        break;
                    }
                    at += aterm_effects::companion::RESIDENT_SHED_CADENCE;
                }
                let ws = &app.windows[&wid];
                assert!(
                    ws.fx.companion().hit_rect().is_some() && ws.fx.companion().alpha() == 255,
                    "native split retains the full body's hit target"
                );
                assert!(ws.fx.companion().brain().is_active());
                assert!(
                    !ws.fx.companion().needs_frames(),
                    "the resident under load owes no frame train"
                );
            }
            assert!(
                app.load_shed_active(),
                "capture must exercise the load-suspended branch"
            );
            render_front_image(&mut app);
            assert!(
                app.load_shed_active(),
                "the capture did not escape the pressure fixture"
            );
            let (cw, ch) = app.win_cell_size(wid);
            let ws = &app.windows[&wid];
            let bodies: Vec<_> = aterm_render::sprites::published(&ws.input_scratch.fx_layers)
                .into_iter()
                .filter(|sprite| usize::from(sprite.w) >= 5 * cw && usize::from(sprite.h) >= ch)
                .collect();
            assert_eq!(
                bodies.len(),
                1,
                "capture retains one full pet under load, split={split}"
            );
            assert_eq!(bodies[0].alpha, 255);
            assert_eq!(
                aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
                1,
                "no head or animated attachment under load"
            );
            assert!(
                ws.input_scratch.word_decorations.is_empty()
                    && ws.input_scratch.ink.is_empty()
                    && ws
                        .input_scratch
                        .nova_add
                        .iter()
                        .all(|q| ws.pane_overlay.painted_quad(q)),
                "decorations remain shed"
            );

            assert!(ws.fx.companion().brain().is_active());
            assert!(
                !ws.fx.companion().needs_frames(),
                "capture shares the shed cadence: no frame train"
            );
        }
    }

    #[test]
    fn production_headless_image_keeps_one_full_pet_during_reduced_motion_singing() {
        use aterm_effects::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
        use aterm_effects::kitty_sing::SING_ARM_REPEATS;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.motion = Some("reduced".into());
        app.recompute_sparkle();
        app.sparkle = None;
        let now = Instant::now() + Duration::from_secs(60);
        {
            let ws = app.windows.get_mut(&wid).expect("headless window 0");
            ws.focused = true;
            let tenure_start = now - Duration::from_secs(7);
            for i in 0..160u64 {
                ws.fx
                    .companions_mut()
                    .cursor_cat
                    .on_pet_mode_motion_pulse(CursorCatMotionPulse {
                        at: tenure_start + Duration::from_millis(i * 40),
                        kind: CursorCatMotionKind::Advance,
                    });
            }
            let sing_start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
            for i in 0..SING_ARM_REPEATS {
                ws.fx.companions_mut().kitty_sing.note_char(
                    sing_start + Duration::from_millis(u64::from(i) * 10),
                    0,
                    'a',
                );
            }
        }

        render_front_image(&mut app);

        let (cell_w, cell_h) = app.win_cell_size(wid);
        let ws = app.windows.get_mut(&wid).expect("headless window 0");
        assert!(
            ws.fx.companions_mut().cursor_cat.is_active(),
            "the capture-owned detector sync spends the authenticated hidden tenure"
        );
        assert!(
            ws.fx.companion_mut().brain().is_active(),
            "reduced motion keeps the full resident visible and caret-fed during the song"
        );
        assert_eq!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
            1,
            "the production still contains one full resident, never a replacement or second head"
        );
        assert!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers)
                .iter()
                .all(|s| !s.art.is_empty())
        );
        let body = &aterm_render::sprites::published(&ws.input_scratch.fx_layers)[0];
        assert!(
            usize::from(body.w) >= 5 * cell_w && usize::from(body.h) >= cell_h,
            "capture must show the full animal, not a replacement head"
        );
        assert_eq!(
            ws.fx
                .companions_mut()
                .cursor_cat
                .static_frame(Instant::now())
                .sing,
            1.0
        );
        assert!(
            ws.input_scratch.word_decorations.is_empty()
                && ws.input_scratch.ink.is_empty()
                && ws.input_scratch.nova_add.is_empty(),
            "the disabled word feed stays byte-empty while its independent singing companion draws"
        );
    }

    #[test]
    fn production_headless_image_keeps_classic_kitty_without_sparkle_words() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty flying".into());
        app.config.motion = Some("full".into());
        app.recompute_sparkle();
        app.sparkle = None;
        let now = Instant::now();
        {
            let ws = app.windows.get_mut(&wid).expect("headless window 0");
            ws.focused = true;
            let start = now - Duration::from_millis(95 * 40);
            for i in 0..96u64 {
                ws.fx
                    .companions_mut()
                    .cursor_cat
                    .on_key(start + Duration::from_millis(i * 40), true);
            }
            assert!(
                ws.fx.companions_mut().cursor_cat.is_active(),
                "the fixture earns the real classic cursor-cat lifecycle"
            );
        }

        render_front_image(&mut app);

        let ws = app.windows.get(&wid).expect("headless window 0");
        assert!(ws.fx.companions().cursor_cat.is_active());
        assert!(!ws.fx.companion().brain().is_active());
        assert_eq!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
            1,
            "the config-free capture carries exactly the one earned classic kitty"
        );
        assert!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers)
                .iter()
                .all(|s| !s.art.is_empty())
        );
        assert!(
            ws.input_scratch.word_decorations.is_empty()
                && ws.input_scratch.ink.is_empty()
                && ws.input_scratch.nova_add.is_empty(),
            "no word-owned channel is synthesized to host the independent kitty"
        );
    }

    #[test]
    fn serious_mode_capture_hard_retires_a_song_rearmed_after_transition() {
        use aterm_effects::kitty_sing::SING_ARM_REPEATS;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".into());
        app.recompute_sparkle();
        app.sparkle = None;
        assert!(app.set_serious_mode(true));

        let now = Instant::now();
        let start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
        {
            let ws = app.windows.get_mut(&wid).expect("headless window 0");
            for i in 0..SING_ARM_REPEATS {
                ws.fx.companions_mut().kitty_sing.note_char(
                    start + Duration::from_millis(u64::from(i) * 10),
                    0,
                    'a',
                );
            }
            assert_eq!(
                ws.fx.companions_mut().kitty_sing.drive(now),
                1.0,
                "negative control: the post-transition input really re-armed the singer"
            );
        }

        app.capture_words(wid, now);
        let ws = app.windows.get(&wid).expect("headless window 0");
        assert_eq!(ws.fx.companions().kitty_sing.drive(now), 0.0);
        assert!(!ws.fx.companions().cursor_cat.is_active());
        assert!(!ws.fx.companion().brain().is_active());
        assert!(!ws.fx.companions().music_notes.is_active());
        assert!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers).is_empty(),
            "Serious Mode capture must publish no companion pixels"
        );
    }

    #[test]
    fn alt_screen_word_suppression_keeps_the_independent_capture_pet() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
            enabled: Some(true),
            suppress_in_alt_screen: Some(true),
            ..Default::default()
        });
        app.prepared_sparkle = app.config.prepare_sparkle_runtime();
        app.sparkle_dirty = true;
        app.recompute_sparkle();
        assert!(
            app.sparkle.is_some(),
            "negative control: word sparkles resolved on"
        );
        let terminal = app.front_terminal(wid).expect("front terminal");
        term_lock(&terminal.term).process(b"\x1b[?1049h");

        app.capture_words(wid, Instant::now());
        let ws = app.windows.get(&wid).expect("headless window 0");
        assert_eq!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
            1,
            "the word-only alt-screen switch cannot blank the resident cursor pet"
        );
        assert!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers)
                .iter()
                .all(|s| !s.art.is_empty())
        );
        assert!(
            ws.input_scratch.word_decorations.is_empty()
                && ws.input_scratch.ink.is_empty()
                && ws.input_scratch.nova_add.is_empty(),
            "the suppressed word-owned channels remain empty"
        );
    }

    #[test]
    fn composed_capture_decorations_use_the_extracted_screen_sample() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".into());
        let wid = WindowId(0);
        let focused_session = app.split_active_stub_tab(wid);
        let term = app
            .pool
            .get(focused_session)
            .expect("focused terminal")
            .term
            .clone();
        term_lock(&term).process(b"\x1b[?1049h\x1b[4;7H");
        let terminal_id = term_lock(&term).render_identity();
        let switch_term = term.clone();
        let now = Instant::now();
        let capture = app
            .prepare_terminal_capture_grid_with_cursor_fx_interleaved(
                wid,
                crate::app_render::ComposedCursorFxClock::Advance(now),
                move || term_lock(&switch_term).process(b"\x1b[?1049l\x1b[2;2H"),
            )
            .expect("composed capture");
        assert!(capture.focus.is_some_and(|sample| {
            sample.terminal_id == terminal_id && sample.alternate_screen
        }));
        assert!(
            !term_lock(&term).is_alternate_screen(),
            "negative control: the live terminal moved to a newer screen"
        );

        app.windows
            .get_mut(&wid)
            .expect("test window")
            .fx
            .forget_owner_space();
        app.capture_words_sampled(wid, now, capture.focus.expect("focused capture sample"));
        assert_eq!(
            app.windows[&wid].fx.owner_space(),
            Some((terminal_id, true)),
            "decoration custody must stay on the screen paired with captured cells"
        );
    }

    #[test]
    fn headless_capture_ticks_fire_live_then_decays_to_zero() {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true); // master switch (default platform-split)
        app.config.cursor_trail_style = Some("fire".into());
        let wid = WindowId(0);
        let t0 = Instant::now();
        let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(t0),
        );
        {
            let ws = app.windows.get_mut(&wid).expect("headless window 0");
            ws.fx.cursor_mut().glow.note_synthetic_move(t0);
            ws.fx.cursor_mut().trail.note_synthetic_move(t0);
        }
        {
            let terminal = app.front_terminal(wid).expect("front terminal");
            term_lock(&terminal.term).grid_mut().cursor_forward(1);
        }
        let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(t0 + Duration::from_millis(16)),
        );
        {
            let ws = app.windows.get(&wid).expect("headless window 0");
            assert!(
                !ws.input_scratch.glow_add_laid().is_empty(),
                "a headless capture right after a cursor move carries live fire quads"
            );
        }
        let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(t0 + Duration::from_secs(31)),
        );
        let ws = app.windows.get(&wid).expect("headless window 0");
        assert!(
            ws.input_scratch.glow_add_laid().is_empty(),
            "31 s idle: the fire must decay to exactly zero quads in the capture"
        );
        let ember = ws
            .input_scratch
            .cursor_fill_override
            .expect("31 s idle: the fire cursor rests as a warm ember, not the theme fill");
        let (r, b) = ((ember >> 16) & 0xFF, ember & 0xFF);
        assert!(
            r > b,
            "the rest cursor is warm metal (r > b), got {ember:#08x}"
        );
    }

    #[test]
    fn headless_capture_hands_no_neighbor_rows_before_rainbow_kitty_owns_the_frame() {
        let mut app = App::headless_for_test();
        app.config.motion = Some("full".into());
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".into());
        app.config.trail_sounds = Some(false);
        let wid = WindowId(0);
        {
            let terminal = app.front_terminal(wid).expect("front terminal");
            term_lock(&terminal.term)
                .process(b"\x1b[5;1HABOVE\x1b[6;1HCURRENT\x1b[7;1HBELOW\x1b[6;8H");
        }
        let t0 = Instant::now();
        assert!(!app.windows[&wid].fx.cursor().glow.v2_owns_frame());
        let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(t0),
        );
        let glow = &app.windows[&wid].fx.cursor().glow;
        assert!(glow.v2_owns_frame(), "the first capture's tick engages v2");
        assert_eq!(
            glow.neighbor_rows_probed(),
            Some((false, false)),
            "the engaging tick's capture skipped both flanking rows"
        );
        let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(t0 + Duration::from_millis(16)),
        );
        let ws = &app.windows[&wid];
        assert_eq!(
            ws.fx.cursor().glow.neighbor_rows_probed(),
            Some((true, true)),
            "once v2 owns the frame the capture's rows reach the witnesses"
        );
        for (probe, marker) in [
            (&ws.poof_row_above_buf, "ABOVE"),
            (&ws.poof_row_below_buf, "BELOW"),
        ] {
            let actual: String = probe[..marker.len()].iter().collect();
            assert_eq!(actual, marker, "the witnesses' rows are the real ones");
        }
    }
}

#[cfg(test)]
mod deferred_gpu_capture_tests {
    use super::{
        App, CaptureRequest, DEFERRED_GPU_CAPTURE_LIMIT, DeferredGpuCapture,
        deferred_capture_identity_matches, enqueue_deferred_gpu_capture,
    };
    use crate::control::{CaptureCancellation, ImageReq, ReplyRetentionPermit};
    use crate::{WindowId, control_auth};
    use std::collections::VecDeque;
    use std::sync::{Arc, OnceLock, mpsc};

    fn capture(
        surface: u64,
    ) -> (
        DeferredGpuCapture,
        mpsc::Receiver<crate::control::ImageReply>,
        CaptureCancellation,
    ) {
        let (reply, receiver) = mpsc::channel();
        let cancel = CaptureCancellation::new();
        (
            DeferredGpuCapture {
                window: WindowId(0),
                surface,
                exact_tap: false,
                budget: super::CapturePresentBudget::default(),
                request: CaptureRequest::Image(ImageReq {
                    target: control_auth::ConfinedImage::in_memory(),
                    clean: false,
                    session: None,
                    want_bytes: true,
                    want_metadata: false,
                    frame_metadata: Arc::new(OnceLock::new()),
                    cancel: cancel.clone(),
                    reply,
                    handoff: ReplyRetentionPermit::from_own_budget(),
                }),
            },
            receiver,
            cancel,
        )
    }

    fn capture_queue_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            DeferredGpuCaptureQueue {
                const Buggy = 0;
                const Cap = 8;
                var queued = 0;
                var current = 1;
                var cancelled = 0;
                var resumed = 0;
                action Push when (queued <= Cap) {
                    queued = if queued <= Cap - 1 || Buggy == 1 { queued + 1 } else { queued };
                }
                action Close { current = 0; resumed = 0; }
                action Replace { current = 2; resumed = 0; }
                action Restore { current = 1; cancelled = 0; resumed = 0; }
                action Cancel { cancelled = 1; resumed = 0; }
                action Resume when (queued > 0 && (Buggy == 1 || (current == 1 && cancelled == 0))) {
                    queued = queued - 1;
                    resumed = 1;
                }
                invariant BoundedOwnership: queued <= Cap;
                invariant ResumeNeedsCurrentLiveRequest: resumed == 0 || (current == 1 && cancelled == 0);
            }
        }
    }

    #[test]
    fn deferred_gpu_capture_queue_real_admission_and_identity_conform() {
        let model = capture_queue_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let mut state = model.init_state();
        let mut queue = VecDeque::new();
        let mut receivers = Vec::new();
        for surface in 1..=DEFERRED_GPU_CAPTURE_LIMIT + 1 {
            let (request, receiver, _) = capture(surface as u64);
            let admitted = enqueue_deferred_gpu_capture(&mut queue, request, false);
            assert!(model.fire("Push", &mut state));
            assert_eq!(queue.len() as i64, state["queued"]);
            assert!(model.check_invariant("BoundedOwnership", &state));
            if surface <= DEFERRED_GPU_CAPTURE_LIMIT {
                assert!(admitted.is_ok());
                assert!(
                    matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
                    "pending retains the original reply without answering early"
                );
                receivers.push(receiver);
            } else {
                let rejected = match admitted {
                    Err(request) => request,
                    Ok(()) => panic!("overflow was admitted"),
                };
                rejected.request.reject("queue full");
                assert_eq!(receiver.recv().unwrap().unwrap_err(), "queue full");
                assert!(matches!(
                    receiver.try_recv(),
                    Err(mpsc::TryRecvError::Disconnected)
                ));
            }
        }
        let head = queue.pop_front().unwrap();
        assert_eq!(head.surface, 1);
        assert!(enqueue_deferred_gpu_capture(&mut queue, head, true).is_ok());
        for (index, receiver) in receivers.into_iter().enumerate() {
            let request = queue.pop_front().unwrap();
            assert_eq!(
                request.surface,
                (index + 1) as u64,
                "later requests cannot overtake a resumed head"
            );
            request.request.reject("finished once");
            assert_eq!(receiver.recv().unwrap().unwrap_err(), "finished once");
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
        }
        for current in [None, Some(1), Some(2)] {
            for cancelled in [false, true] {
                let mut observed = model.init_state();
                observed.insert("queued", 1);
                observed.insert("current", current.unwrap_or(0) as i64);
                observed.insert("cancelled", i64::from(cancelled));
                assert_eq!(
                    deferred_capture_identity_matches(1, current, cancelled),
                    model.action_enabled("Resume", &observed),
                );
            }
        }
        let mut bad = model.init_state();
        bad.insert("queued", DEFERRED_GPU_CAPTURE_LIMIT as i64 + 1);
        assert!(
            !model.check_invariant("BoundedOwnership", &bad),
            "negative control: an unbounded queue must fail"
        );
        bad.insert("queued", 1);
        bad.insert("current", 2);
        bad.insert("resumed", 1);
        assert!(
            !model.check_invariant("ResumeNeedsCurrentLiveRequest", &bad),
            "negative control: completion on a replaced surface must fail"
        );
    }

    fn capture_resume_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            CaptureReadyRedrawOwnership {
                const Buggy = 0;
                var submitted = 0;
                var pending = 0;
                var retry_open = 1;
                var decided = 0;
                var redraw = 0;
                action Reject { submitted = 0; pending = 0; retry_open = 1; decided = 0; }
                action Present { submitted = 1; pending = 0; retry_open = 1; decided = 0; }
                action Wait { submitted = 0; pending = 1; retry_open = 1; decided = 0; }
                action Fail { submitted = 0; pending = 0; retry_open = 0; decided = 0; }
                action Decide {
                    decided = 1;
                    redraw = if Buggy == 1 { 0 } else {
                        if submitted == 0 && pending == 0 && retry_open == 1 { 1 } else { 0 }
                    };
                }
                invariant RejectionCannotStrandReadyFrame:
                    decided == 0 || submitted == 1 || pending == 1 || retry_open == 0 || redraw == 1;
            }
        }
    }

    #[test]
    fn capture_resume_rejection_redraw_guard_conforms_and_catches_stranding() {
        let model = capture_resume_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        for (action, after, pending, retry_open) in [
            ("Reject", 10, false, true),
            ("Present", 11, false, true),
            ("Wait", 10, true, true),
            ("Fail", 10, false, false),
        ] {
            let mut state = model.init_state();
            assert!(model.fire(action, &mut state));
            assert!(model.fire("Decide", &mut state));
            let redraw = !super::capture_resume_owns_next_frame(10, after, pending, retry_open);
            assert_eq!(i64::from(redraw), state["redraw"], "{action}");
        }
        let mut rejected = model.init_state();
        assert!(model.fire("Reject", &mut rejected));
        assert!(model.fire("Decide", &mut rejected));
        rejected.insert("redraw", 0);
        assert!(
            !model.check_invariant("RejectionCannotStrandReadyFrame", &rejected),
            "negative control: merely calling a rejected capture is not presentation progress"
        );
    }

    fn capture_failure_budget_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            CaptureCompletedFailureBudget {
                const Buggy = 0;
                var remaining = 3;
                var failures = 0;
                var waiting = 0;
                var done = 0;
                action Demand when (remaining > 0 && waiting == 0 && done == 0) {
                    remaining = if Buggy == 1 { 3 } else { remaining };
                    waiting = 1;
                }
                action Timeout when (waiting == 1) {
                    remaining = remaining - 1;
                    failures = failures + 1;
                    waiting = 0;
                    done = if remaining <= 1 { 1 } else { 0 };
                }
                action Success when (waiting == 1) { waiting = 0; done = 1; }
                invariant OriginalBudgetConserved: remaining + failures == 3;
                invariant CompletedFailuresBounded: failures <= 3;
                invariant ExhaustionTerminates: remaining > 0 || done == 1;
            }
        }
    }

    #[test]
    fn original_capture_failure_budget_survives_pending_and_requeue() {
        let model = capture_failure_budget_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let mut state = model.init_state();
        let mut queue = VecDeque::new();
        let (mut original, original_reply, _) = capture(17);
        let (follower, follower_reply, _) = capture(18);
        queue.push_back(follower);
        for completed in 1..=crate::NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT {
            assert!(model.fire("Demand", &mut state));
            let budget_before = original.budget;
            original.budget.record_attempt(true, false, true);
            assert_eq!(original.budget, budget_before);
            assert!(enqueue_deferred_gpu_capture(&mut queue, original, true).is_ok());
            original = queue.pop_front().unwrap();
            assert_eq!(
                original.surface, 17,
                "replaying head retains its ownership before followers"
            );
            assert_eq!(i64::from(original.budget.remaining), state["remaining"]);
            assert!(
                matches!(original_reply.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "the same caller still awaits this acquisition"
            );

            original.budget.record_attempt(false, false, true);
            assert!(model.fire("Timeout", &mut state));
            assert_eq!(i64::from(original.budget.remaining), state["remaining"]);
            assert_eq!(i64::from(completed), state["failures"]);
            assert!(model.check_invariant("OriginalBudgetConserved", &state));
        }
        assert_eq!(original.budget.remaining, 0);
        assert!(
            !crate::run_capture_present_barrier(original.budget.remaining, || {
                panic!("exhausted original request attempted another acquisition")
            }),
            "the real capture barrier must not execute at budget zero"
        );
        assert!(
            !model.action_enabled("Demand", &state),
            "snapshot has no caller timer: the exhausted original budget itself must stop retries"
        );
        original
            .request
            .reject("capture exhausted its completed present attempts");
        assert!(original_reply.recv().unwrap().is_err());
        assert!(matches!(
            original_reply.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(
            queue.front().unwrap().surface,
            18,
            "exhausted head no longer starves its live follower"
        );
        assert!(matches!(
            follower_reply.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let mut bad = state.clone();
        bad.insert("remaining", 3);
        assert!(
            !model.check_invariant("OriginalBudgetConserved", &bad),
            "negative control: resetting the budget on replay recreates unlimited timeouts"
        );
        let mut success = super::CapturePresentBudget::default();
        success.record_attempt(false, true, true);
        assert_eq!(
            success,
            super::CapturePresentBudget::default(),
            "a successful serial is never charged as failure"
        );
    }

    #[test]
    fn deferred_gpu_capture_original_timeout_and_closed_window_reply_once() {
        let mut app = App::headless_for_test();
        let (request, receiver, cancellation) = capture(17);
        assert!(
            enqueue_deferred_gpu_capture(&mut app.deferred_gpu_captures, request, false).is_ok()
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(cancellation.cancel());
        assert!(
            app.deferred_gpu_captures[0]
                .request
                .cancellation()
                .is_some(),
            "the original caller cancellation remains attached"
        );
        app.prune_deferred_gpu_captures();
        assert!(receiver.recv().unwrap().is_err());
        app.prune_deferred_gpu_captures();
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(app.deferred_gpu_captures.is_empty());

        let (request, receiver, _) = capture(17);
        app.deferred_gpu_captures.push_back(request);
        app.windows.remove(&WindowId(0));
        app.prune_deferred_gpu_captures();
        assert!(receiver.recv().unwrap().is_err());
        assert!(app.deferred_gpu_captures.is_empty());
    }

    #[test]
    fn cancelling_a_queued_follower_preserves_the_heads_exact_tap() {
        let mut app = App::headless_for_test();
        let ws = app.windows.get_mut(&WindowId(0)).unwrap();
        ws.capture_acquire_armed = true;
        ws.capture_client_requested = true;
        let (follower, receiver, cancellation) = capture(17);
        assert!(!follower.exact_tap);
        assert!(cancellation.cancel());
        app.deferred_gpu_captures.push_back(follower);
        app.prune_deferred_gpu_captures();
        assert!(receiver.recv().unwrap().is_err());
        assert!(app.windows[&WindowId(0)].capture_acquire_armed);
        assert!(app.windows[&WindowId(0)].capture_client_requested);
        let (mut owner, receiver, cancellation) = capture(17);
        owner.exact_tap = true;
        assert!(cancellation.cancel());
        app.deferred_gpu_captures.push_back(owner);
        app.prune_deferred_gpu_captures();
        assert!(receiver.recv().unwrap().is_err());
        assert!(app.windows[&WindowId(0)].capture_acquire_armed);
        assert!(app.windows[&WindowId(0)].capture_client_requested);
        assert!(!super::deferred_capture_owns_current_tap(
            false,
            17,
            Some(17)
        ));
        assert!(!super::deferred_capture_owns_current_tap(
            true,
            17,
            Some(18)
        ));
        assert!(!super::deferred_capture_owns_current_tap(true, 17, None));
        assert!(super::deferred_capture_owns_current_tap(true, 17, Some(17)));
        if super::deferred_capture_owns_current_tap(true, 17, Some(17)) {
            app.discard_presented_client_capture(WindowId(0));
        }
        assert!(!app.windows[&WindowId(0)].capture_acquire_armed);
        assert!(!app.windows[&WindowId(0)].capture_client_requested);
    }
}

#[cfg(test)]
mod headless_settings_preview_capture_tests {
    use super::image_req_for_test;
    use crate::native_settings::SettingsRoute;
    use crate::settings_preview::{PreviewAnimation, SettingsPreviewSpec};
    use crate::{App, WindowId, control_auth};
    use std::time::{Duration, Instant};

    const WID: WindowId = WindowId(0);

    fn image_at(app: &mut App, elapsed_ms: u64) -> Vec<u8> {
        let now = app.lat_epoch + Duration::from_millis(elapsed_ms);
        crate::app_native::with_native_preview_instant_for_test(now, || {
            let (tx, rx) = std::sync::mpsc::channel();
            let target = control_auth::ConfinedImage::in_memory();
            app.render_image(image_req_for_test(target, false, true, None, tx));
            let (_, _, png) = rx
                .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
                .expect("image worker reply")
                .expect("headless image succeeds")
                .value;
            png.expect("PNG bytes")
        })
    }

    fn drawn_preview(app: &App) -> SettingsPreviewSpec {
        let (_, view) = app.active_native_view(WID).expect("the Settings view");
        app.windows[&WID].leaf_render_cache[&view]
            .native
            .as_ref()
            .expect("a staged native raster")
            .compiled
            .paint
            .iter()
            .find_map(|paint| match &paint.content {
                crate::native_ui::UiContent::SettingsPreview(spec) => Some((**spec).clone()),
                _ => None,
            })
            .expect("the page paints its cursor preview")
    }

    #[test]
    fn a_headless_capture_samples_a_live_settings_preview_at_its_instant() {
        let mut renderer = aterm_render::Renderer::from_bytes(
            aterm_render::embedded_font(),
            14.0,
            aterm_render::Theme::default(),
        )
        .expect("the embedded face");
        renderer.set_runtime_font_discovery(false);
        crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
        let mut app = App::headless_for_test();
        // The cursor preview loops while a trail draws, which Windows keeps
        // off until one is chosen.
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            crate::app_config::trail_drawn(""),
        )
        .unwrap();
        app.lat_epoch = Instant::now();
        {
            let window = app.windows.get_mut(&WID).unwrap();
            window.focused = true;
            window.rows = 50;
            window.cols = 120;
        }
        crate::app_native::with_native_preview_instant_for_test(app.lat_epoch, || {
            assert!(app.open_settings_tab(SettingsRoute::CursorMotion));
        });

        let early_png = image_at(&mut app, 420);
        let early = drawn_preview(&app);
        assert_eq!(
            early.animation(),
            PreviewAnimation::Continuous,
            "the page's cursor preview loops"
        );
        assert_eq!(
            app.native_settings_preview_for_tick(WID, 700, false),
            None,
            "the park never ticks an unwatched headless preview"
        );
        let late_png = image_at(&mut app, 980);
        let late = drawn_preview(&app);
        assert_eq!((early.phase_ms, late.phase_ms), (420, 980));
        assert_ne!(early_png, late_png, "the preview moved between captures");

        app.windows.get_mut(&WID).unwrap().focused = false;
        let still_png = image_at(&mut app, 1_400);
        let still = drawn_preview(&app);
        assert_ne!(still.animation(), PreviewAnimation::Continuous);
        assert_eq!(
            image_at(&mut app, 2_600),
            still_png,
            "a still preview is the same frame at every capture"
        );
        assert_eq!(drawn_preview(&app).phase_ms, still.phase_ms);
    }
}

#[cfg(test)]
mod headless_capture_band_tests {
    use crate::{App, WindowId};
    use std::time::{Duration, Instant};

    /// A headless capture of a native or mixed front prepares the message band at the capture
    /// instant, as a terminal front's does, instead of splicing whatever frame the band last
    /// held (a live row's elapsed, ETA and stalled state frozen at it).
    #[test]
    fn a_headless_capture_of_any_front_prepares_the_band_now() {
        for mixed in [false, true] {
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.announce_toolchain_pass("installing 10 ALab program(s)", true);
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
            if mixed {
                let _ = app
                    .split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
            }
            let (plan, route) = app
                .active_visible_frame_layout(wid)
                .expect("a visible frame");
            assert!(
                matches!(
                    (mixed, route),
                    (false, crate::VisibleContentRoute::Native { .. })
                        | (true, crate::VisibleContentRoute::Heterogeneous)
                ),
                "PRECONDITION: the front's route ({route:?}, mixed={mixed})"
            );
            let stale = Instant::now() + Duration::from_secs(120);
            app.prepare_band_motion(wid, stale);
            let held = app.windows[&wid]
                .band
                .motion()
                .expect("PRECONDITION: a live row's band frame")
                .at;
            assert!(
                app.stage_headless_capture(wid, plan, route).is_ok(),
                "the capture stages (mixed={mixed})"
            );
            let at = app.windows[&wid]
                .band
                .motion()
                .expect("the band's frame")
                .at;
            assert!(
                at < held,
                "the capture prepared the band now, not at the held frame (mixed={mixed})"
            );
        }
    }
}
