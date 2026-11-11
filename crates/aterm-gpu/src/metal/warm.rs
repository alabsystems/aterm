// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first window's shaders, compiled once on a worker and discarded (a
//! headless launch's too, with the rest of what its frames can bind, ahead of
//! its first pixel demand), and the rest of what a live arm can bind, built on
//! a worker and handed to it.

use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};

use aterm_time::{Duration, Instant};

use super::ffi::{AutoreleasePool, Device, Library, Obj, PixelFormat};
use super::pipelines;
use crate::FetchProbe;
use crate::pipeline_table::{ALL_PIPELINES, FetchTwin, Pipeline, ShaderLibrary};

/// What decides a first window's first frames beyond their text: the launch's config, as the
/// window will pin it onto its renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstFrameKnobs {
    /// The cursor aurora (a `cursor_trail_style` other than Off, or `cursor_momentum_glow`):
    /// the first prompt's cursor lights it, so that frame binds the additive glow.
    pub aurora: bool,
    /// The aurora's bloom (`cursor_trail_bloom`).
    pub bloom: bool,
    /// The aurora's crown on an EDR screen (`hdr_glow`).
    pub hdr_glow: bool,
    /// The aurora's crown on an SDR screen (`cursor_glow_sdr_boost` above 0).
    pub sdr_glow: bool,
    /// An fx layer on the first frames (the Claude card, `[claude_footer] card`): the Exact row.
    pub fx: bool,
}

/// What a first window builds over its first frames under `knobs`, in the order it needs
/// them, as `(row, swapchain format)`: the text frame's rows, the present blit at both
/// swapchain formats (which one a window gets depends on its screen's EDR potential, which
/// only the attach can read), then the aurora's rows (with its bloom, the blit at the
/// Rgba8Unorm present copy too: the compose copy the bloom draws over) and the fx Exact row.
/// Where `fetch`, glow and fx draw through their twins ([`first_frame_twins`]), so the glow
/// row serves only the bloom's extract and the fx row is not built.
pub(crate) fn first_frame_rows(
    knobs: FirstFrameKnobs,
    fetch: bool,
) -> Vec<(Pipeline, PixelFormat)> {
    use PixelFormat::{Bgra8Unorm, Rgba8Unorm, Rgba16Float};
    let mut rows = vec![
        (Pipeline::Bg, Bgra8Unorm),
        (Pipeline::Glyph, Bgra8Unorm),
        (Pipeline::ColorGlyph, Bgra8Unorm),
        (Pipeline::Blit, Bgra8Unorm),
        (Pipeline::Blit, Rgba16Float),
    ];
    if knobs.aurora {
        if !fetch || knobs.bloom {
            rows.push((Pipeline::GlowAdd, Bgra8Unorm));
        }
        if knobs.bloom {
            rows.push((Pipeline::Bloom, Bgra8Unorm));
            rows.push((Pipeline::Blit, Rgba8Unorm));
        }
        if knobs.hdr_glow {
            rows.push((Pipeline::HdrGlow, Rgba16Float));
        }
        if knobs.sdr_glow {
            rows.push((Pipeline::SdrGlow, Bgra8Unorm));
        }
    }
    if knobs.fx && !fetch {
        rows.push((Pipeline::Fx, Bgra8Unorm));
    }
    rows
}

/// The framebuffer-fetch twins a first window's frames bind under `knobs` where `fetch`: the
/// aurora's glow and the card's fx.
pub(crate) fn first_frame_twins(knobs: FirstFrameKnobs, fetch: bool) -> Vec<FetchTwin> {
    let mut twins = Vec::new();
    if fetch && knobs.aurora {
        twins.push(FetchTwin::GlowAdd);
    }
    if fetch && knobs.fx {
        twins.push(FetchTwin::Fx);
    }
    twins
}

/// One compile or build a warm can have in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WarmItem {
    /// A library compile.
    Library(ShaderLibrary),
    /// A PSO build, keyed by its row and the colour attachment it resolves to
    /// (`pipelines::metal_format`), as Metal's pipeline cache keys it: a frame row built
    /// against any swapchain format is one item.
    Pipeline(Pipeline, PixelFormat),
    /// The device's framebuffer-fetch probe ([`super::fetch::verdict`]), which the cell lane
    /// runs once it has built the rows its frames bind under either verdict.
    FetchProbe,
    /// A framebuffer-fetch twin's PSO build.
    Twin(FetchTwin),
}

impl WarmItem {
    /// The build of `row` against a `present`-format swapchain.
    #[inline]
    pub(crate) const fn pipeline(row: Pipeline, present: PixelFormat) -> Self {
        Self::Pipeline(row, pipelines::metal_format(row.spec().target, present))
    }
}

/// How long after a warm began the first window will still wait for it.
pub(crate) const DEADLINE: Duration = Duration::from_millis(300);

/// Where a warm stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No warm began: headless, a CPU launch, a test.
    Idle,
    /// A warm began at `began` and has `in_flight` under way, by lane
    /// ([`Lane`]; `cell.metal` from the claim on, before its worker even
    /// runs); `owed` holds the [`OWED`] libraries it has not compiled yet, by
    /// index, and [`OWED_PROBE`] until its framebuffer-fetch probe ran.
    Running {
        began: Instant,
        in_flight: [Option<WarmItem>; 2],
        owed: u8,
    },
    /// The warm ended, done or not.
    Settled,
}

/// A warm's two lanes: `cell.metal` and its rows, and the present side's
/// libraries and rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Cell = 0,
    Present = 1,
}

impl Lane {
    #[inline]
    const fn of(library: ShaderLibrary) -> Self {
        if matches!(library, ShaderLibrary::Cell) {
            Self::Cell
        } else {
            Self::Present
        }
    }
}

/// The libraries the first window always needs, which the warm compiles first and the window
/// waits for from the claim on, as [`Phase::Running`]'s `owed` bits: otherwise a window
/// arriving between the two compiles compiles `blit.metal` a second time (12.6 ms of main
/// thread).
const OWED: [ShaderLibrary; 2] = [ShaderLibrary::Cell, ShaderLibrary::Blit];

/// [`Phase::Running`]'s `owed` bit for the device's framebuffer-fetch probe,
/// which the warm owes from the claim on: a window that drew Unorm-law light
/// before it landed routes that frame on the split rather than probing a second
/// time or waiting ([`WarmLatch::probe_pending`]).
const OWED_PROBE: u8 = 1 << OWED.len();

/// The rendezvous between a warm and the first window.
pub(crate) struct WarmLatch {
    phase: Mutex<Phase>,
    settled: Condvar,
    /// How long after a warm began [`Self::await_item`] still waits for it.
    deadline: Duration,
    /// Callers blocked in [`Self::await_item`] now: what a test reads to know
    /// a wait is under way before it lets the warm go.
    #[cfg(test)]
    waiting: std::sync::atomic::AtomicUsize,
}

/// The process's one first-frame warm, which the first window awaits.
pub(crate) static FIRST_FRAME: WarmLatch = WarmLatch::new(DEADLINE);

/// Offscreen callers never wait on or start the windowed warm's device selection.
pub(crate) static HEADLESS_FIRST_FRAME: WarmLatch = WarmLatch::new(DEADLINE);

impl WarmLatch {
    /// A latch whose window waits at most `deadline` after the warm began: [`DEADLINE`] in the
    /// process, a hang detector in a test that holds the warm and so must not race the
    /// shipped window.
    #[inline]
    pub(crate) const fn new(deadline: Duration) -> Self {
        Self {
            phase: Mutex::new(Phase::Idle),
            settled: Condvar::new(),
            deadline,
            #[cfg(test)]
            waiting: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Claim this latch's one warm: `None` once one has begun, so a launch that warms its
    /// config twice (`warm_config_moved`) compiles once.
    pub(crate) fn begin(&'static self) -> Option<FirstFrameWarm> {
        self.begin_with_policy(crate::DevicePolicy::Windowed)
    }

    pub(crate) fn begin_with_policy(
        &'static self,
        device_policy: crate::DevicePolicy,
    ) -> Option<FirstFrameWarm> {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if *phase != Phase::Idle {
            return None;
        }
        *phase = Phase::Running {
            began: Instant::now(),
            in_flight: [Some(WarmItem::Library(ShaderLibrary::Cell)), None],
            owed: ((1 << OWED.len()) - 1) | OWED_PROBE,
        };
        Some(FirstFrameWarm {
            latch: self,
            device_policy,
        })
    }

    /// The warm's `lane` has `item` under way now.
    fn start(&self, lane: Lane, item: WarmItem) {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if let Phase::Running { in_flight, .. } = &mut *phase {
            in_flight[lane as usize] = Some(item);
        }
    }

    /// The warm's `lane` finished `item`, done or failed: nothing in flight
    /// there, and an [`OWED`] library no longer owed.
    fn finish(&self, lane: Lane, item: WarmItem) {
        self.hand_off(lane, item, None);
    }

    /// [`Self::finish`] `item` and start `next` on `lane` under one lock, so
    /// a window released by `item` finds `next` in flight rather than
    /// running it a second time.
    fn hand_off(&self, lane: Lane, item: WarmItem, next: Option<WarmItem>) {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if let Phase::Running {
            in_flight, owed, ..
        } = &mut *phase
        {
            in_flight[lane as usize] = next;
            if let WarmItem::Library(lib) = item
                && let Some(i) = OWED.iter().position(|o| *o == lib)
            {
                *owed &= !(1 << i);
            }
            if item == WarmItem::FetchProbe {
                *owed &= !OWED_PROBE;
            }
        }
        drop(phase);
        self.settled.notify_all();
    }

    /// Release window waits if this lane returns an error or unwinds. A
    /// successful lane leaves the other lane's work in flight.
    fn run_lane(&self, run: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        struct SettleUnlessOk<'a> {
            latch: &'a WarmLatch,
            ok: bool,
        }
        impl Drop for SettleUnlessOk<'_> {
            fn drop(&mut self) {
                if !self.ok {
                    self.latch.settle();
                }
            }
        }
        let mut guard = SettleUnlessOk {
            latch: self,
            ok: false,
        };
        let result = run();
        guard.ok = result.is_ok();
        result
    }

    fn settle(&self) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = Phase::Settled;
        self.settled.notify_all();
    }

    /// Wait while the warm still owes `item` (an [`OWED`] library) or has it in flight, never
    /// past this latch's deadline ([`DEADLINE`] for [`FIRST_FRAME`]) after the warm began.
    pub(crate) fn await_item(&self, item: WarmItem) {
        let owed_bit = match item {
            WarmItem::Library(lib) => OWED.iter().position(|o| *o == lib).map(|i| 1 << i),
            WarmItem::Pipeline(..) | WarmItem::FetchProbe | WarmItem::Twin(_) => None,
        };
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        while let Phase::Running {
            began,
            in_flight,
            owed,
        } = *phase
            && (in_flight.contains(&Some(item)) || owed_bit.is_some_and(|bit| owed & bit != 0))
        {
            let Some(left) = (began + self.deadline).checked_duration_since(Instant::now()) else {
                return;
            };
            #[cfg(test)]
            self.waiting
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            phase = self
                .settled
                .wait_timeout(phase, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            #[cfg(test)]
            self.waiting
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Whether the warm still owes the device's framebuffer-fetch verdict: it is running and
    /// has not finished its probe. A window asks rather than waits, so the probe never sits on
    /// a frame's path.
    pub(crate) fn probe_pending(&self) -> bool {
        matches!(
            self.current_phase(),
            Phase::Running { owed, .. } if owed & OWED_PROBE != 0
        )
    }

    /// Whether a warm began on this latch.
    #[inline]
    pub(crate) fn began(&self) -> bool {
        self.current_phase() != Phase::Idle
    }

    /// The phase as of now.
    fn current_phase(&self) -> Phase {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The right to run a first-frame warm, retaining its windowed or headless device policy.
pub struct FirstFrameWarm {
    latch: &'static WarmLatch,
    device_policy: crate::DevicePolicy,
}

impl FirstFrameWarm {
    /// Compile and build what a first window builds over its first frames ([`first_frame_rows`]
    /// under `knobs`), each library before its first row, and drop it all.
    /// # Errors
    /// No Metal device, or the first compile or build that failed.
    pub fn run(mut self, knobs: FirstFrameKnobs) -> Result<(), String> {
        let _pool = AutoreleasePool::new();
        let dev = Device::for_policy(self.device_policy)
            .ok_or_else(|| "no Metal device admitted by the warm's selection policy".to_owned())?;
        self.run_on(&dev, knobs, pipelines::compile_library)
    }

    /// [`Self::run`]'s body over a given compile, so a test can fail it.
    pub(crate) fn run_on(
        &mut self,
        dev: &Device,
        knobs: FirstFrameKnobs,
        compile: impl Fn(&Device, ShaderLibrary) -> Result<Library, String> + Sync,
    ) -> Result<(), String> {
        let walked = self.walk(dev, knobs, compile);
        if walked.is_err() {
            self.latch.settle();
        }
        walked
    }

    fn walk(
        &mut self,
        dev: &Device,
        knobs: FirstFrameKnobs,
        compile: impl Fn(&Device, ShaderLibrary) -> Result<Library, String> + Sync,
    ) -> Result<(), String> {
        let latch = self.latch;
        let present_device = WorkerDevice(dev.clone_ref());
        let present: Vec<(Pipeline, PixelFormat)> = first_frame_rows(knobs, false)
            .into_iter()
            .filter(|(row, _)| Lane::of(row.spec().library) == Lane::Present)
            .collect();
        std::thread::scope(|scope| {
            let compile = &compile;
            let present = scope.spawn(move || {
                let dev = present_device.into_inner();
                latch.run_lane(|| {
                    let _pool = AutoreleasePool::new();
                    walk_lane(latch, Lane::Present, &dev, &present, compile)
                })
            });
            let cell = latch.run_lane(|| walk_cell_lane(latch, dev, knobs, compile));
            let present = present
                .join()
                .unwrap_or_else(|_| Err("the warm's present lane panicked".to_owned()));
            cell.and(present)
        })
    }
}

/// One lane's rows, in order, each library compiled before its first row,
/// each compile and build in flight on `latch` while it runs.
fn walk_lane(
    latch: &WarmLatch,
    lane: Lane,
    dev: &Device,
    rows: &[(Pipeline, PixelFormat)],
    compile: &(impl Fn(&Device, ShaderLibrary) -> Result<Library, String> + Sync),
) -> Result<(), String> {
    let mut libs: Vec<(ShaderLibrary, Library)> = Vec::new();
    for &(row, present) in rows {
        let spec = row.spec();
        if !libs.iter().any(|(l, _)| *l == spec.library) {
            let item = WarmItem::Library(spec.library);
            latch.start(lane, item);
            let lib = compile(dev, spec.library);
            latch.finish(lane, item);
            libs.push((spec.library, lib?));
        }
        let lib = libs
            .iter()
            .find(|(l, _)| *l == spec.library)
            .map(|(_, lib)| lib)
            .expect("compiled above");
        let item = WarmItem::pipeline(row, present);
        latch.start(lane, item);
        let built = pipelines::build(dev, lib, spec, present);
        latch.finish(lane, item);
        built?;
    }
    Ok(())
}

/// The cell lane: `cell.metal`, the rows its frames bind whatever the verdict,
/// then the device's framebuffer-fetch probe, then the glow and fx rows or
/// twins its verdict leaves the first window to bind. The probe runs after the
/// text frame's rows, off its path: a window routes its Unorm-law light on the split
/// while the warm still owes the verdict ([`WarmLatch::probe_pending`]).
fn walk_cell_lane(
    latch: &WarmLatch,
    dev: &Device,
    knobs: FirstFrameKnobs,
    compile: &(impl Fn(&Device, ShaderLibrary) -> Result<Library, String> + Sync),
) -> Result<(), String> {
    let cell_rows = |fetch| {
        first_frame_rows(knobs, fetch)
            .into_iter()
            .filter(|(row, _)| Lane::of(row.spec().library) == Lane::Cell)
    };
    let either: Vec<(Pipeline, PixelFormat)> = cell_rows(true).collect();
    let item = WarmItem::Library(ShaderLibrary::Cell);
    latch.start(Lane::Cell, item);
    let lib = compile(dev, ShaderLibrary::Cell);
    let next = either
        .first()
        .filter(|_| lib.is_ok())
        .map(|&(row, present)| WarmItem::pipeline(row, present));
    latch.hand_off(Lane::Cell, item, next);
    let lib = lib?;
    let build_row = |row: Pipeline, present: PixelFormat| {
        let item = WarmItem::pipeline(row, present);
        latch.start(Lane::Cell, item);
        let built = pipelines::build(dev, &lib, row.spec(), present);
        latch.finish(Lane::Cell, item);
        built.map(drop)
    };
    for &(row, present) in &either {
        build_row(row, present)?;
    }
    latch.start(Lane::Cell, WarmItem::FetchProbe);
    let fetch = super::fetch::compiled_with_twins(dev, &lib)
        && settle_verdict(dev, &lib) == Some(FetchProbe::Passed);
    latch.finish(Lane::Cell, WarmItem::FetchProbe);
    for (row, present) in cell_rows(fetch).filter(|r| !either.contains(r)) {
        build_row(row, present)?;
    }
    for twin in first_frame_twins(knobs, fetch) {
        let item = WarmItem::Twin(twin);
        latch.start(Lane::Cell, item);
        let built = pipelines::build(dev, &lib, &twin.spec(), PixelFormat::Bgra8Unorm);
        latch.finish(Lane::Cell, item);
        built?;
    }
    Ok(())
}

/// The device's framebuffer-fetch verdict through `cell` (its `cell.metal`), probed on a
/// session of its own and settled for the process ([`super::fetch::verdict`]) so an arm minted
/// later reads it ([`super::fetch::known`]); `None` where no session could open.
fn settle_verdict(dev: &Device, cell: &Library) -> Option<FetchProbe> {
    let loss = Arc::new(super::loss::LossLatch::new());
    let session = super::encoder::EncodeSession::new(dev, Arc::clone(&loss)).ok()?;
    let mint = super::resources::MetalResourceDevice::new(dev, loss);
    Some(super::fetch::verdict(&mint, &session, cell))
}

impl Drop for FirstFrameWarm {
    fn drop(&mut self) {
        self.latch.settle();
    }
}

/// The switches that decide which of a renderer's present passes can run, as the renderer holds them: a pass they turn off binds no row, so a pipeline warm builds none for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentKnobs {
    /// The bloom composite (`set_bloom`).
    pub bloom: bool,
    /// The heat shimmer (`set_shimmer`).
    pub shimmer: bool,
    /// The aurora's crown on an EDR screen and the f16 swapchain it draws into
    /// (`set_hdr_glow`, on a renderer with a window on screen: a headless one presents to a
    /// Bgra8Unorm target alone).
    pub hdr_glow: bool,
    /// The aurora's crown on an SDR screen (`set_sdr_glow_boost` above 0).
    pub sdr_glow: bool,
}

impl PresentKnobs {
    /// Whether a frame or present may bind `row` under these switches.
    #[inline]
    const fn bind(self, row: Pipeline) -> bool {
        match row {
            Pipeline::Bloom | Pipeline::GlowItems => self.bloom,
            Pipeline::Shimmer => self.shimmer,
            Pipeline::HdrGlow | Pipeline::HdrGlowItems => self.hdr_glow,
            Pipeline::SdrGlow | Pipeline::SdrGlowItems => self.sdr_glow,
            _ => true,
        }
    }
}

/// The destinations a present binds `row` against under `knobs`: the tray card the Rgba8Unorm
/// present copy; the blit and the SDR crowns the swapchain, at f16 too where one may exist
/// ([`PresentKnobs::hdr_glow`]), and the blit the present copy as well (the compose copy,
/// `present::encode_present_sequence`). Any other row draws into an offscreen whose format does
/// not depend on the destination: one build.
#[inline]
const fn destinations(row: Pipeline, knobs: PresentKnobs) -> &'static [PixelFormat] {
    match row {
        Pipeline::Tray => &[PixelFormat::Rgba8Unorm],
        Pipeline::Blit if knobs.hdr_glow => &[
            PixelFormat::Bgra8Unorm,
            PixelFormat::Rgba16Float,
            PixelFormat::Rgba8Unorm,
        ],
        Pipeline::Blit => &[PixelFormat::Bgra8Unorm, PixelFormat::Rgba8Unorm],
        Pipeline::SdrGlow | Pipeline::SdrGlowItems if knobs.hdr_glow => {
            &[PixelFormat::Bgra8Unorm, PixelFormat::Rgba16Float]
        }
        _ => &[PixelFormat::Bgra8Unorm],
    }
}

/// Every first-frame knob on: what a launch's first-frame warm builds at most.
const EVERY_FIRST_FRAME: FirstFrameKnobs = FirstFrameKnobs {
    aurora: true,
    bloom: true,
    hdr_glow: true,
    sdr_glow: true,
    fx: true,
};

/// Every build a live arm's frames and presents can bind under `knobs`: each row at the
/// destinations its present meets ([`destinations`]) and, where `twins`, each framebuffer-fetch
/// twin. Ordered as a frame comes to need them: what it binds whatever the verdict, then the
/// twins, then the twinned rows (bound only on the split), and last what a first-frame warm
/// builds, which the first frames build for themselves.
pub(crate) fn bindable(knobs: PresentKnobs, twins: bool) -> Vec<WarmItem> {
    let first: Vec<WarmItem> = first_frame_rows(EVERY_FIRST_FRAME, twins)
        .into_iter()
        .map(|(row, present)| WarmItem::pipeline(row, present))
        .chain(
            first_frame_twins(EVERY_FIRST_FRAME, twins)
                .into_iter()
                .map(WarmItem::Twin),
        )
        .collect();
    let mut items = Vec::new();
    for row in ALL_PIPELINES.into_iter().filter(|row| knobs.bind(*row)) {
        for &present in destinations(row, knobs) {
            items.push(WarmItem::pipeline(row, present));
        }
        if twins && let Some(twin) = FetchTwin::of(row) {
            items.push(WarmItem::Twin(twin));
        }
    }
    items.sort_by_key(|item| match *item {
        _ if first.contains(item) => 3,
        WarmItem::Pipeline(row, _) if twins && FetchTwin::of(row).is_some() => 2,
        WarmItem::Twin(_) => 1,
        _ => 0,
    });
    items
}

/// One build a pipeline warm hands its arm: the item and its pipeline state, `None` where the
/// build failed (the frame that binds it builds it and says why) and always for the
/// framebuffer-fetch probe (its verdict is the process's, [`super::fetch::known`]).
pub(crate) struct Warmed {
    pub(crate) item: WarmItem,
    pub(crate) pso: Option<Obj>,
}

// SAFETY: a `Warmed` carries one `MTLRenderPipelineState`, immutable once built, which Metal
// documents as usable from any thread; objc retain/release are thread-safe, and the +1 moves
// once.
unsafe impl Send for Warmed {}

/// The arm's `MTLDevice`, moved to the worker that builds for it.
struct WorkerDevice(Device);

// SAFETY: an `MTLDevice` may be used from several threads at once (Metal's threading model:
// devices, libraries and pipeline states are thread-safe), and this holder moves its +1 once.
unsafe impl Send for WorkerDevice {}

impl WorkerDevice {
    #[inline]
    fn into_inner(self) -> Device {
        self.0
    }
}

/// Start a pipeline warm: build `items` for `dev` on a worker of its own, each library compiled
/// before its first row, sending each build to `to` as it lands and waiting out any compile or
/// build the first-frame warm (`latch`) has in flight. The worker ends early once `to`'s
/// receiver is gone and returns how many items it handed over.
/// # Errors
/// The worker could not start.
pub(crate) fn spawn(
    dev: &Device,
    latch: &'static WarmLatch,
    items: Vec<WarmItem>,
    to: mpsc::Sender<Warmed>,
) -> std::io::Result<std::thread::JoinHandle<usize>> {
    let dev = WorkerDevice(dev.clone_ref());
    std::thread::Builder::new()
        .name("aterm-metal-pipeline-warm".to_owned())
        .spawn(move || {
            let dev = dev.into_inner();
            let _pool = AutoreleasePool::new();
            build_each(&dev, latch, &items, |warmed| to.send(warmed).is_ok())
        })
}

/// Build each of `items` in order and hand it over until `hand` refuses one: how many it
/// handed over, the refused one with them. The framebuffer-fetch probe
/// ([`WarmItem::FetchProbe`]) settles the process's verdict ([`settle_verdict`]).
fn build_each(
    dev: &Device,
    latch: &WarmLatch,
    items: &[WarmItem],
    mut hand: impl FnMut(Warmed) -> bool,
) -> usize {
    let mut libs: Vec<(ShaderLibrary, Option<Library>)> = Vec::new();
    let mut handed = 0;
    for &item in items {
        let library = match item {
            WarmItem::Pipeline(row, _) => row.spec().library,
            WarmItem::Twin(_) | WarmItem::FetchProbe => ShaderLibrary::Cell,
            WarmItem::Library(_) => continue,
        };
        if !libs.iter().any(|(l, _)| *l == library) {
            latch.await_item(WarmItem::Library(library));
            libs.push((library, pipelines::compile_library(dev, library).ok()));
        }
        let lib = libs
            .iter()
            .find(|(l, _)| *l == library)
            .and_then(|(_, lib)| lib.as_ref());
        latch.await_item(item);
        let pso = lib.and_then(|lib| match item {
            WarmItem::Pipeline(row, present) => {
                pipelines::build(dev, lib, row.spec(), present).ok()
            }
            WarmItem::Twin(twin) => {
                pipelines::build(dev, lib, &twin.spec(), PixelFormat::Bgra8Unorm).ok()
            }
            WarmItem::FetchProbe => {
                let _ = settle_verdict(dev, lib);
                None
            }
            WarmItem::Library(_) => None,
        });
        handed += 1;
        if !hand(Warmed { item, pso }) {
            break;
        }
    }
    handed
}

/// The latch the first-frame compile measurement (`benches/first_frame_compile.rs`)
/// warms and waits on: one per process, as a launch's is.
static BENCH_LATCH: WarmLatch = WarmLatch::new(DEADLINE);

/// The launch's default knobs (`aterm-gui`'s config defaults on macOS): the
/// aurora on, with its bloom and both crowns, and the Claude card's fx row.
const BENCH_KNOBS: FirstFrameKnobs = FirstFrameKnobs {
    aurora: true,
    bloom: true,
    hdr_glow: true,
    sdr_glow: true,
    fx: true,
};

/// The measurement's compile: the shipped source plus a per-run `nonce`
/// comment, so every library is a cache miss.
fn bench_compile(
    nonce: u128,
) -> impl Fn(&Device, ShaderLibrary) -> Result<Library, String> + Copy + Sync {
    move |dev: &Device, lib: ShaderLibrary| -> Result<Library, String> {
        let opts = super::ffi::CompileOptions::aterm_default()
            .ok_or_else(|| "no MTLCompileOptions".to_owned())?;
        let src = format!(
            "{}\n// bench nonce {nonce}\n",
            super::shaders::source_for(lib, super::fetch::capable(dev))
        );
        dev.new_library_with_options(&src, &opts)
    }
}

/// The first-frame compile measurement's warm half: begin the warm (the launch's default knobs,
/// or every knob off: the text frame and the blit alone) and hand back its job, which
/// `benches/first_frame_compile.rs` runs on a thread of its own and which answers how long the
/// warm took.
/// # Errors
/// A second call in one process.
#[doc(hidden)]
pub fn first_frame_bench_warm(
    nonce: u128,
    aurora: bool,
) -> Result<impl FnOnce() -> Result<Duration, String> + Send + 'static, String> {
    let mut warm = BENCH_LATCH
        .begin_with_policy(crate::DevicePolicy::Headless)
        .ok_or_else(|| "one warm per process".to_owned())?;
    Ok(move || {
        let _pool = AutoreleasePool::new();
        let began = Instant::now();
        let dev = Device::preferred_listed().ok_or_else(|| "no listed Metal device".to_owned())?;
        let knobs = if aurora {
            BENCH_KNOBS
        } else {
            FirstFrameKnobs::default()
        };
        warm.run_on(&dev, knobs, bench_compile(nonce))?;
        Ok(began.elapsed())
    })
}

/// The first-frame compile measurement's main half: the first mint's compiles on the calling
/// thread (`cell.metal`, the text frame's rows, the present blit), each after waiting out the
/// same item on the caller's warm, as an arm does, then the aurora's glow, bloom and SDR
/// crown rows.
/// # Errors
/// No Metal device, or a shipped shader that does not compile.
#[doc(hidden)]
pub fn first_frame_bench_main(nonce: u128) -> Result<[Duration; 5], String> {
    let compile = bench_compile(nonce);
    let _pool = AutoreleasePool::new();
    let arrived = Instant::now();
    BENCH_LATCH.await_item(WarmItem::Library(ShaderLibrary::Cell));
    let waited = arrived.elapsed();
    let dev = Device::preferred_listed().ok_or_else(|| "no listed Metal device".to_owned())?;
    let cell = compile(&dev, ShaderLibrary::Cell)?;
    let lib = arrived.elapsed();
    for (row, present) in first_frame_rows(FirstFrameKnobs::default(), false)
        .into_iter()
        .filter(|(row, _)| row.spec().library == ShaderLibrary::Cell)
    {
        BENCH_LATCH.await_item(WarmItem::pipeline(row, present));
        pipelines::build(&dev, &cell, row.spec(), present)?;
    }
    let rows = arrived.elapsed();
    BENCH_LATCH.await_item(WarmItem::Library(ShaderLibrary::Blit));
    let blit = compile(&dev, ShaderLibrary::Blit)?;
    BENCH_LATCH.await_item(WarmItem::pipeline(Pipeline::Blit, PixelFormat::Bgra8Unorm));
    pipelines::build(&dev, &blit, Pipeline::Blit.spec(), PixelFormat::Bgra8Unorm)?;
    let present = arrived.elapsed();
    let mut libs: Vec<(ShaderLibrary, Library)> = vec![(ShaderLibrary::Cell, cell)];
    for row in [Pipeline::GlowAdd, Pipeline::Bloom, Pipeline::SdrGlow] {
        let spec = row.spec();
        if !libs.iter().any(|(l, _)| *l == spec.library) {
            BENCH_LATCH.await_item(WarmItem::Library(spec.library));
            libs.push((spec.library, compile(&dev, spec.library)?));
        }
        let lib = libs
            .iter()
            .find(|(l, _)| *l == spec.library)
            .map(|(_, lib)| lib)
            .expect("compiled above");
        BENCH_LATCH.await_item(WarmItem::pipeline(row, PixelFormat::Bgra8Unorm));
        pipelines::build(&dev, lib, spec, PixelFormat::Bgra8Unorm)?;
    }
    Ok([waited, lib, rows, present, arrived.elapsed()])
}

#[cfg(test)]
#[path = "warm_tests.rs"]
mod tests;
