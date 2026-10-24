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

/// What decides a first window's first frames beyond their text: the launch's config, as the window will pin it onto its renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstFrameKnobs {
    /// The cursor aurora (a `cursor_trail_style` other than Off, or `cursor_momentum_glow`): the first prompt's cursor lights it, so that frame binds the additive glow.
    pub aurora: bool,
    /// The aurora's bloom (`cursor_trail_bloom`).
    pub bloom: bool,
    /// The aurora's crown on an EDR screen (`hdr_glow`).
    pub hdr_glow: bool,
    /// The aurora's crown on an SDR screen (`cursor_glow_sdr_boost` above 0).
    pub sdr_glow: bool,
    /// An fx layer on the first frames (the Claude card, `[claude_footer] card`): its Exact row.
    pub fx: bool,
}

/// What a first window builds over its first frames under `knobs`, in the order it needs them, as `(row, swapchain format)`: the text frame's rows (the colour-glyph row for a prompt that carries an emoji), the present blit at both swapchain formats (which one a window gets depends on its screen's EDR potential, which only the attach can read), then what the first prompt's aurora binds: its additive glow, the bloom and the crown, then the fx Exact row a card's layers bind. Where `fetch` (the device passed the framebuffer-fetch probe) the frame draws its glow and fx through their twins ([`first_frame_twins`]), so the glow row is the bloom's extract alone and the fx row is never bound.
pub(crate) fn first_frame_rows(
    knobs: FirstFrameKnobs,
    fetch: bool,
) -> Vec<(Pipeline, PixelFormat)> {
    use PixelFormat::{Bgra8Unorm, Rgba16Float};
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

/// The framebuffer-fetch twins a first window's frames bind under `knobs` where `fetch`: the aurora's glow and the card's fx.
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
    /// A PSO build, keyed by its row and the colour attachment it resolves to (`pipelines::metal_format`): what Metal's pipeline cache keys on, so a frame row built against any swapchain format is the same item.
    Pipeline(Pipeline, PixelFormat),
    /// The device's framebuffer-fetch probe ([`super::fetch::verdict`]), which the cell lane runs once the rows its frames bind whatever the verdict are built.
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

/// The libraries the first window always needs, which the warm compiles first
/// and the window waits for from the claim on, as [`Phase::Running`]'s `owed`
/// bits: a window that arrived while the warm was between them compiled
/// `blit.metal` a second time (the CS-1 bench: 12.6 ms of main thread with the
/// window at +60 ms).
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

impl WarmLatch {
    /// A latch whose window waits at most `deadline` after the warm began: [`DEADLINE`] in the process, a hang detector in a test that holds the warm and so must not race the shipped window.
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

    /// Claim this latch's one warm: `None` once one has begun, so a launch that warms its config twice (`warm_config_moved`) compiles once.
    pub(crate) fn begin(&'static self) -> Option<FirstFrameWarm> {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if *phase != Phase::Idle {
            return None;
        }
        *phase = Phase::Running {
            began: Instant::now(),
            in_flight: [Some(WarmItem::Library(ShaderLibrary::Cell)), None],
            owed: ((1 << OWED.len()) - 1) | OWED_PROBE,
        };
        Some(FirstFrameWarm { latch: self })
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

    /// Wait while the warm still owes `item` (an [`OWED`] library) or has it in flight, never past this latch's deadline ([`DEADLINE`] for [`FIRST_FRAME`]) after the warm began.
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

    /// Whether the warm still owes the device's framebuffer-fetch verdict: it is running and has not finished its probe. A window asks rather than waits, so the probe never sits on a frame's path.
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

/// The right to run the process's first-frame warm, from [`crate::begin_first_frame_warm`].
pub struct FirstFrameWarm {
    latch: &'static WarmLatch,
}

impl FirstFrameWarm {
    /// Compile and build what a first window builds over its first frames ([`first_frame_rows`] under `knobs`), each library before its first row, and drop it all.
    /// # Errors
    /// No Metal device, or the first compile or build that failed.
    pub fn run(mut self, knobs: FirstFrameKnobs) -> Result<(), String> {
        let _pool = AutoreleasePool::new();
        let dev =
            Device::preferred().ok_or_else(|| "no Metal device on this machine".to_owned())?;
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
        let present: Vec<(Pipeline, PixelFormat)> = first_frame_rows(knobs, false)
            .into_iter()
            .filter(|(row, _)| Lane::of(row.spec().library) == Lane::Present)
            .collect();
        std::thread::scope(|scope| {
            let compile = &compile;
            let present = scope.spawn(move || {
                latch.run_lane(|| {
                    let _pool = AutoreleasePool::new();
                    let dev = Device::preferred()
                        .ok_or_else(|| "no Metal device on this machine".to_owned())?;
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

/// The device's framebuffer-fetch verdict through `cell` (its `cell.metal`), probed on a session of its own and settled for the process ([`super::fetch::verdict`]), so an arm minted later reads it ([`super::fetch::known`]); `None` where no session could open.
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

/// Whether the process's headless launch warm was claimed.
static SHADER_CACHE_BEGAN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The right to run the process's headless launch warm, from [`crate::begin_shader_cache_warm`]: once per process, as the first-frame warm is.
pub struct ShaderCacheWarm(());

impl ShaderCacheWarm {
    /// Claim the process's one headless launch warm.
    #[inline]
    pub(crate) fn begin() -> Option<Self> {
        (!SHADER_CACHE_BEGAN.swap(true, std::sync::atomic::Ordering::SeqCst)).then_some(Self(()))
    }

    /// Whether the process's headless launch warm was claimed.
    #[inline]
    pub(crate) fn began() -> bool {
        SHADER_CACHE_BEGAN.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Compile and build, in order, what a headless launch's renderer builds once its first pixel demand mints it, and drop it all: the first frames' rows under `knobs` (the split's, and the twins where the device has framebuffer fetch: its verdict is the renderer's to settle; an f16 destination only where `knobs.hdr_glow`), then, where `rest`, every other row and twin a frame or present can bind under it ([`bindable`]). Nothing waits on it, and it submits no GPU work: a process's first submission costs it the driver's footprint (320 MB measured on an M5 Max), which a headless run that never draws a pixel must not pay.
    /// # Errors
    /// No Metal device the device listing names (the window server, which only a renderer may ask, would have to pick it), or the compiles and builds that failed.
    pub fn run(self, knobs: FirstFrameKnobs, rest: Option<PresentKnobs>) -> Result<(), String> {
        let _pool = AutoreleasePool::new();
        let dev = Device::preferred_listed()
            .ok_or_else(|| "no Metal device the device listing names".to_owned())?;
        let twins = super::fetch::capable(&dev);
        let mut items: Vec<WarmItem> = first_frame_rows(knobs, false)
            .into_iter()
            .filter(|&(_, present)| knobs.hdr_glow || present != PixelFormat::Rgba16Float)
            .map(|(row, present)| WarmItem::pipeline(row, present))
            .chain(
                first_frame_twins(knobs, twins)
                    .into_iter()
                    .map(WarmItem::Twin),
            )
            .collect();
        for item in rest.map(|rest| bindable(rest, twins)).unwrap_or_default() {
            if !items.contains(&item) {
                items.push(item);
            }
        }
        let mut failed = Vec::new();
        build_each(&dev, &FIRST_FRAME, &items, |warmed| {
            if warmed.pso.is_none() {
                failed.push(warmed.item);
            }
            true
        });
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!("{failed:?} did not build"))
        }
    }
}

/// The switches that decide which of a renderer's present passes can run, as the renderer holds them: a pass they turn off binds no row, so a pipeline warm builds none for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentKnobs {
    /// The bloom composite (`set_bloom`).
    pub bloom: bool,
    /// The heat shimmer (`set_shimmer`).
    pub shimmer: bool,
    /// The aurora's crown on an EDR screen and the f16 swapchain it draws into (`set_hdr_glow`, on a renderer with a window on screen: a headless one presents to a Bgra8Unorm target alone).
    pub hdr_glow: bool,
    /// The aurora's crown on an SDR screen (`set_sdr_glow_boost` above 0).
    pub sdr_glow: bool,
}

impl PresentKnobs {
    /// Whether a frame or present may bind `row` under these switches.
    #[inline]
    const fn bind(self, row: Pipeline) -> bool {
        match row {
            Pipeline::Bloom => self.bloom,
            Pipeline::Shimmer => self.shimmer,
            Pipeline::HdrGlow => self.hdr_glow,
            Pipeline::SdrGlow => self.sdr_glow,
            _ => true,
        }
    }
}

/// The destinations a present binds `row` against under `knobs`, as its callers pass them: the tray card the Rgba8Unorm present copy, the blit and the SDR crown the swapchain, at f16 too where one may exist ([`PresentKnobs::hdr_glow`]). Any other row draws into an offscreen, whose format is its own whatever the destination: one build.
#[inline]
const fn destinations(row: Pipeline, knobs: PresentKnobs) -> &'static [PixelFormat] {
    match row {
        Pipeline::Tray => &[PixelFormat::Rgba8Unorm],
        Pipeline::Blit | Pipeline::SdrGlow if knobs.hdr_glow => {
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

/// Every build a live arm's frames and presents can bind under `knobs`: each row at the destinations its present meets ([`destinations`]; one item for a row that draws into an offscreen, as Metal keys it) and, where `twins`, each framebuffer-fetch twin. In the order a frame comes to need them once its first frames drew: what a frame binds whatever the device's verdict, then the twins, in table order; then, where `twins`, each twinned row, which a frame binds only on the split; and last what a first-frame warm builds, which the first frames build for themselves.
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

/// One build a pipeline warm hands its arm: the item, and its pipeline state, `None` where it failed (the frame that binds it builds it and says why) or, for the device's framebuffer-fetch probe, always (the verdict is the process's, [`super::fetch::known`]).
pub(crate) struct Warmed {
    pub(crate) item: WarmItem,
    pub(crate) pso: Option<Obj>,
}

// SAFETY: a `Warmed` carries one `MTLRenderPipelineState`, immutable once built, which Metal documents as usable from any thread; objc retain/release are thread-safe, and the +1 moves once.
unsafe impl Send for Warmed {}

/// The arm's `MTLDevice`, moved to the worker that builds for it.
struct WorkerDevice(Device);

// SAFETY: an `MTLDevice` may be used from several threads at once (Metal's threading model: devices, libraries and pipeline states are thread-safe), and this holder moves its +1 once.
unsafe impl Send for WorkerDevice {}

impl WorkerDevice {
    #[inline]
    fn into_inner(self) -> Device {
        self.0
    }
}

/// Start a pipeline warm: build `items` (the device's framebuffer-fetch probe where the arm owes it, then [`bindable`]) for `dev` on a worker of their own, sending each to `to` as it lands, each library compiled before its first row and the first-frame warm (`latch`) waited out where it has the same compile or build in flight. The worker ends early once `to`'s receiver is gone; it is detached unless a test joins it: how many items it handed over.
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

/// Build each of `items` in order and hand it over, until `hand` refuses one: how many it handed over, the refused one with them. The device's framebuffer-fetch probe ([`WarmItem::FetchProbe`]) settles the process's verdict ([`settle_verdict`]).
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

/// THE FIRST-FRAME COMPILE MEASUREMENT's warm half (`benches/first_frame_compile.rs`, which runs the job on a thread of its own): begin the warm — with the launch's default knobs, or with the aurora off, the text frame and the blit alone — and hand back its job, which answers how long the warm took.
/// # Errors
/// A second call in one process.
#[doc(hidden)]
pub fn first_frame_bench_warm(
    nonce: u128,
    aurora: bool,
) -> Result<impl FnOnce() -> Result<Duration, String> + Send + 'static, String> {
    let mut warm = BENCH_LATCH
        .begin()
        .ok_or_else(|| "one warm per process".to_owned())?;
    Ok(move || {
        let _pool = AutoreleasePool::new();
        let began = Instant::now();
        let dev = Device::preferred().ok_or_else(|| "no Metal device".to_owned())?;
        let knobs = if aurora {
            BENCH_KNOBS
        } else {
            FirstFrameKnobs::default()
        };
        warm.run_on(&dev, knobs, bench_compile(nonce))?;
        Ok(began.elapsed())
    })
}

/// THE FIRST-FRAME COMPILE MEASUREMENT's main half: the first mint's compiles on the calling thread (`cell.metal`, the text frame's rows, the present blit; the device's framebuffer-fetch verdict is not on a mint's path, which routes Unorm-law light on the split while the warm owes it), each after waiting out the same item in flight on a warm the caller began, as the arm's own do, then what the first prompt's aurora binds (the glow row, the bloom, the SDR crown).
/// # Errors
/// No Metal device, or a shipped shader that does not compile.
#[doc(hidden)]
pub fn first_frame_bench_main(nonce: u128) -> Result<[Duration; 5], String> {
    let compile = bench_compile(nonce);
    let _pool = AutoreleasePool::new();
    let arrived = Instant::now();
    BENCH_LATCH.await_item(WarmItem::Library(ShaderLibrary::Cell));
    let waited = arrived.elapsed();
    let dev = Device::preferred().ok_or_else(|| "no Metal device".to_owned())?;
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
mod tests {
    use super::*;
    use crate::metal::test_support::device;
    use std::sync::mpsc;

    /// The one minute a wait for something that must happen gets.
    const HANG: Duration = Duration::from_secs(60);

    /// Wait (a hang detector) until `latch` has `n` callers blocked in
    /// `await_item`.
    fn until_waiting(latch: &WarmLatch, n: usize) {
        let began = Instant::now();
        while latch.waiting.load(std::sync::atomic::Ordering::SeqCst) < n {
            assert!(began.elapsed() < HANG, "no caller ever waited on the warm");
            std::thread::yield_now();
        }
    }

    /// A warm whose first compile fails releases every wait on it as it
    /// fails and reports the failure; a second warm on the same latch is
    /// refused.
    #[test]
    fn a_failed_warm_releases_the_window_and_is_not_retried() {
        static OK: WarmLatch = WarmLatch::new(DEADLINE);
        let ok_warm = OK.begin().expect("the positive control's first warm");
        OK.start(Lane::Present, WarmItem::Library(ShaderLibrary::Blit));
        assert_eq!(
            OK.run_lane(|| {
                OK.finish(Lane::Cell, WarmItem::Library(ShaderLibrary::Cell));
                Ok(())
            }),
            Ok(())
        );
        assert!(matches!(
            OK.current_phase(),
            Phase::Running {
                in_flight: [None, Some(WarmItem::Library(ShaderLibrary::Blit))],
                ..
            }
        ));
        drop(ok_warm);
        assert_eq!(OK.current_phase(), Phase::Settled);

        static ERROR: WarmLatch = WarmLatch::new(DEADLINE);
        let error_warm = ERROR.begin().expect("the error control's first warm");
        ERROR.start(Lane::Present, WarmItem::Library(ShaderLibrary::Blit));
        let error = ERROR.run_lane(|| {
            ERROR.finish(Lane::Cell, WarmItem::Library(ShaderLibrary::Cell));
            Err("captured cell failure".to_owned())
        });
        assert_eq!(error, Err("captured cell failure".to_owned()));
        let error_phase = ERROR.current_phase();
        drop(error_warm);

        static PANIC: WarmLatch = WarmLatch::new(DEADLINE);
        let panic_warm = PANIC.begin().expect("the unwind control's first warm");
        PANIC.start(Lane::Present, WarmItem::Library(ShaderLibrary::Blit));
        let panicked =
            std::panic::catch_unwind(|| PANIC.run_lane(|| panic!("captured present failure")));
        assert!(panicked.is_err());
        let panic_phase = PANIC.current_phase();
        drop(panic_warm);
        assert!(
            error_phase == Phase::Settled && panic_phase == Phase::Settled,
            "GPU_LANE_FAILURE_MUST_RELEASE_OTHER_LANE_WAIT"
        );

        static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
        let dev = device();
        let mut warm = LATCH.begin().expect("the latch's first warm");
        assert!(matches!(
            LATCH.current_phase(),
            Phase::Running {
                in_flight: [Some(WarmItem::Library(ShaderLibrary::Cell)), _],
                ..
            }
        ));
        let failed = warm.run_on(&dev, BENCH_KNOBS, |_, _| {
            Err("injected compile failure".to_owned())
        });
        assert_eq!(failed, Err("injected compile failure".to_owned()));
        assert_eq!(
            LATCH.current_phase(),
            Phase::Settled,
            "the failure released every wait, the blit it owed too"
        );
        LATCH.await_item(WarmItem::Library(ShaderLibrary::Blit));
        drop(warm);
        assert_eq!(LATCH.current_phase(), Phase::Settled);
        assert!(LATCH.begin().is_none(), "one warm per latch");
        assert!(LATCH.began());
    }

    /// A warm dropped before it ran (its thread never spawned, or panicked)
    /// releases the first window.
    #[test]
    fn a_warm_dropped_unrun_releases_the_window() {
        static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
        assert!(!LATCH.began(), "no warm, nothing to wait for");
        LATCH.await_item(WarmItem::Library(ShaderLibrary::Cell));
        drop(LATCH.begin().expect("the latch's first warm"));
        assert_eq!(LATCH.current_phase(), Phase::Settled);
    }

    /// The window waits for the libraries the warm owes it — `cell.metal`
    /// and `blit.metal`, from the claim on — and for anything else the warm
    /// has IN FLIGHT, and nothing more: held inside the cell compile, a wait on
    /// the cell library ends when that compile returns, a wait on the blit
    /// library only when the blit compile does; held inside the blit compile,
    /// a wait on a library the warm finished, or on one it has not reached
    /// and does not owe, returns at once.
    #[test]
    fn the_window_waits_for_what_the_warm_owes_or_has_in_flight() {
        static LATCH: WarmLatch = WarmLatch::new(HANG);
        device();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (blit_tx, blit_rx) = mpsc::channel::<()>();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let (go_rx, finish_rx) = (Mutex::new(go_rx), Mutex::new(finish_rx));
        let mut warm = LATCH.begin().expect("the latch's first warm");
        let worker = std::thread::spawn(move || {
            let _pool = AutoreleasePool::new();
            let dev = Device::preferred().expect("the device the test found");
            warm.run_on(&dev, FirstFrameKnobs::default(), |dev, lib| {
                let held = |rx: &Mutex<mpsc::Receiver<()>>, what: &str| {
                    rx.lock()
                        .expect("the test's channel")
                        .recv_timeout(HANG)
                        .unwrap_or_else(|_| panic!("the test's {what}"));
                };
                match lib {
                    ShaderLibrary::Cell => held(&go_rx, "go"),
                    _ => {
                        blit_tx.send(()).expect("the test is listening");
                        held(&finish_rx, "finish");
                    }
                }
                pipelines::compile_library(dev, lib)
            })
        });
        let cell_window = std::thread::spawn(|| {
            LATCH.await_item(WarmItem::Library(ShaderLibrary::Cell));
            LATCH.current_phase()
        });
        let blit_window = std::thread::spawn(|| {
            LATCH.await_item(WarmItem::Library(ShaderLibrary::Blit));
            LATCH.current_phase()
        });
        until_waiting(&LATCH, 2);
        go_tx.send(()).expect("the warm is listening");
        assert!(
            !matches!(
                cell_window.join().expect("the cell window"),
                Phase::Running {
                    in_flight: [Some(WarmItem::Library(ShaderLibrary::Cell)), _],
                    ..
                }
            ),
            "released once the cell compile returned"
        );
        blit_rx
            .recv_timeout(HANG)
            .expect("the warm went on to the present library");
        assert!(
            !blit_window.is_finished(),
            "the blit library is owed while the warm compiles it"
        );
        LATCH.await_item(WarmItem::Library(ShaderLibrary::Cell));
        LATCH.await_item(WarmItem::Library(ShaderLibrary::Bloom));
        finish_tx.send(()).expect("the warm is listening");
        assert!(
            !matches!(
                blit_window.join().expect("the blit window"),
                Phase::Running {
                    in_flight: [_, Some(WarmItem::Library(ShaderLibrary::Blit))],
                    ..
                }
            ),
            "released once the blit compile returned"
        );
        worker
            .join()
            .expect("the warm thread")
            .expect("the shipped shaders compile");
    }

    /// The warm compiles and builds every library and row it names on the
    /// shipped sources: a row or format it cannot build would fail every launch.
    #[test]
    fn the_warm_builds_the_shipped_first_frame_shaders() {
        static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
        let dev = device();
        let mut warm = LATCH.begin().expect("the latch's first warm");
        warm.run_on(&dev, BENCH_KNOBS, pipelines::compile_library)
            .expect("the first-frame warm on the shipped shaders");
        assert!(!LATCH.probe_pending(), "the warm ran the probe it owed");
        let cell = pipelines::compile_library(&dev, ShaderLibrary::Cell).expect("cell.metal");
        assert!(
            crate::metal::fetch::known(&dev, &cell).is_some(),
            "the warm left the device's verdict for the window to read"
        );
    }

    /// The warm owes the device's framebuffer-fetch verdict from the claim until its probe ran, and a window only asks: pending while owed or in flight, not once the probe finished, before a warm began, or after it ended.
    #[test]
    fn the_probe_is_pending_from_the_claim_until_the_warm_ran_it() {
        static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
        assert!(!LATCH.probe_pending(), "no warm began");
        let warm = LATCH.begin().expect("the latch's first warm");
        assert!(LATCH.probe_pending(), "owed from the claim");
        LATCH.finish(Lane::Cell, WarmItem::Library(ShaderLibrary::Cell));
        LATCH.start(Lane::Cell, WarmItem::FetchProbe);
        assert!(LATCH.probe_pending(), "under way");
        LATCH.finish(Lane::Cell, WarmItem::FetchProbe);
        assert!(!LATCH.probe_pending(), "the probe ran");
        drop(warm);
        assert!(!LATCH.probe_pending());

        static ENDED: WarmLatch = WarmLatch::new(DEADLINE);
        drop(ENDED.begin().expect("the latch's first warm"));
        assert!(!ENDED.probe_pending(), "a warm that ended owes nothing");
    }

    /// THE PIPELINE WARM'S REACH: with every present switch on it names every row of the table, at every destination its present meets, and every framebuffer-fetch twin, so nothing a frame or present binds is left for the render thread to build, and it names what a first-frame warm builds after all else, so a warm that races the first frames spends its first builds on what they leave. A switch turned off drops its pass's row alone (the EDR crown's, the f16 destinations with it), and no twins drops the twins alone.
    #[test]
    fn the_pipeline_warm_names_every_row_and_twin_a_frame_can_bind() {
        let on = PresentKnobs {
            bloom: true,
            shimmer: true,
            hdr_glow: true,
            sdr_glow: true,
        };
        let all = bindable(on, true);
        for row in ALL_PIPELINES {
            assert!(
                all.iter()
                    .any(|i| matches!(i, WarmItem::Pipeline(r, _) if *r == row)),
                "`{}` is not warmed",
                row.name()
            );
        }
        for twin in FetchTwin::ALL {
            assert!(
                all.contains(&WarmItem::Twin(twin)),
                "{twin:?}'s twin is not warmed"
            );
        }
        let first: Vec<WarmItem> = first_frame_rows(EVERY_FIRST_FRAME, true)
            .into_iter()
            .map(|(row, present)| WarmItem::pipeline(row, present))
            .chain(
                first_frame_twins(EVERY_FIRST_FRAME, true)
                    .into_iter()
                    .map(WarmItem::Twin),
            )
            .collect();
        let at = all
            .iter()
            .position(|i| first.contains(i))
            .expect("the first frames' rows are bindable");
        assert!(
            all[at..].iter().all(|i| first.contains(i)) && at > 0,
            "the first frames' rows come last: {all:?}"
        );
        let f16 = |i: &WarmItem| matches!(i, WarmItem::Pipeline(_, PixelFormat::Rgba16Float));
        for (off, row) in [
            (PresentKnobs { bloom: false, ..on }, Pipeline::Bloom),
            (
                PresentKnobs {
                    shimmer: false,
                    ..on
                },
                Pipeline::Shimmer,
            ),
            (
                PresentKnobs {
                    hdr_glow: false,
                    ..on
                },
                Pipeline::HdrGlow,
            ),
            (
                PresentKnobs {
                    sdr_glow: false,
                    ..on
                },
                Pipeline::SdrGlow,
            ),
        ] {
            let left = bindable(off, true);
            let dropped: Vec<&WarmItem> = all.iter().filter(|i| !left.contains(i)).collect();
            assert!(
                !dropped.is_empty()
                    && dropped.iter().all(|i| {
                        matches!(i, WarmItem::Pipeline(r, _) if *r == row)
                            || (!off.hdr_glow && f16(i))
                    }),
                "{off:?} dropped {dropped:?}"
            );
            assert!(
                off.hdr_glow || !left.iter().any(f16),
                "no f16 swapchain, no f16 destination"
            );
        }
        let untwinned = bindable(on, false);
        assert!(
            all.iter()
                .all(|i| untwinned.contains(i) == !matches!(i, WarmItem::Twin(_))),
            "no twins drops the twins alone"
        );
    }

    /// A pipeline warm hands every item it is given over as it lands, built on the shipped shaders (the device's framebuffer-fetch probe settled for the process), and its worker ends once it built them; a worker whose arm is gone builds the one item in hand and stops.
    #[test]
    fn the_pipeline_warm_hands_over_each_build_and_stops_when_its_arm_is_gone() {
        static LATCH: WarmLatch = WarmLatch::new(DEADLINE);
        let dev = device();
        let on = PresentKnobs {
            bloom: true,
            shimmer: true,
            hdr_glow: true,
            sdr_glow: true,
        };
        let mut items = vec![WarmItem::FetchProbe];
        items.extend(bindable(on, super::super::fetch::capable(&dev)));
        let (tx, rx) = mpsc::channel();
        let worker = spawn(&dev, &LATCH, items.clone(), tx).expect("the worker starts");
        let mut handed = Vec::new();
        while handed.len() < items.len() {
            let Warmed { item, pso } = rx.recv_timeout(HANG).expect("the warm hands over");
            assert!(
                pso.is_some() || item == WarmItem::FetchProbe,
                "{item:?} did not build on the shipped shaders"
            );
            handed.push(item);
        }
        assert_eq!(handed, items, "handed over in order, each once");
        assert!(
            matches!(
                rx.recv_timeout(HANG),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ),
            "the worker ended once it built its items"
        );
        assert_eq!(worker.join().expect("the worker"), items.len());
        let cell = pipelines::compile_library(&dev, ShaderLibrary::Cell).expect("cell.metal");
        assert!(
            crate::metal::fetch::known(&dev, &cell).is_some(),
            "the warm left the device's verdict for an arm to read"
        );

        let (tx, rx) = mpsc::channel();
        drop(rx);
        let gone = spawn(&dev, &LATCH, items, tx).expect("the worker starts");
        let began = Instant::now();
        while !gone.is_finished() {
            assert!(began.elapsed() < HANG, "the armless worker never stopped");
            std::thread::yield_now();
        }
        assert_eq!(
            gone.join().expect("the worker"),
            1,
            "a worker whose arm is gone stops at its first hand-over"
        );
    }

    /// A headless launch's warm compiles the first frames' rows and the rest a frame can bind on the shipped shaders.
    #[test]
    fn a_headless_launch_warm_builds_the_shipped_shaders() {
        device();
        let rest = PresentKnobs {
            bloom: true,
            shimmer: true,
            hdr_glow: false,
            sdr_glow: true,
        };
        let knobs = FirstFrameKnobs {
            hdr_glow: false,
            ..BENCH_KNOBS
        };
        ShaderCacheWarm(())
            .run(knobs, Some(rest))
            .expect("the headless warm on the shipped shaders");
    }
}
