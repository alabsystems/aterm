// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first window's shaders, compiled once on a worker and discarded.

use std::sync::{Condvar, Mutex, PoisonError};

use aterm_time::{Duration, Instant};

use super::ffi::{AutoreleasePool, Device, Library, PixelFormat};
use super::pipelines;
use crate::pipeline_table::{Pipeline, ShaderLibrary};

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

/// What a first window builds over its first frames under `knobs`, in the order it needs them, as `(row, swapchain format)`: the text frame's rows (the colour-glyph row for a prompt that carries an emoji), the present blit at both swapchain formats (which one a window gets depends on its screen's EDR potential, which only the attach can read), then what the first prompt's aurora binds: its additive glow, the bloom and the crown, then the fx Exact row a card's layers bind.
pub(crate) fn first_frame_rows(knobs: FirstFrameKnobs) -> Vec<(Pipeline, PixelFormat)> {
    use PixelFormat::{Bgra8Unorm, Rgba16Float};
    let mut rows = vec![
        (Pipeline::Bg, Bgra8Unorm),
        (Pipeline::Glyph, Bgra8Unorm),
        (Pipeline::ColorGlyph, Bgra8Unorm),
        (Pipeline::Blit, Bgra8Unorm),
        (Pipeline::Blit, Rgba16Float),
    ];
    if knobs.aurora {
        rows.push((Pipeline::GlowAdd, Bgra8Unorm));
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
    if knobs.fx {
        rows.push((Pipeline::Fx, Bgra8Unorm));
    }
    rows
}

/// One compile or build a warm can have in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WarmItem {
    /// A library compile.
    Library(ShaderLibrary),
    /// A PSO build, keyed by its row and the colour attachment it resolves to (`pipelines::metal_format`): what Metal's pipeline cache keys on, so a frame row built against any swapchain format is the same item.
    Pipeline(Pipeline, PixelFormat),
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
    /// index.
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
            owed: (1 << OWED.len()) - 1,
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
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if let Phase::Running {
            in_flight, owed, ..
        } = &mut *phase
        {
            in_flight[lane as usize] = None;
            if let WarmItem::Library(lib) = item
                && let Some(i) = OWED.iter().position(|o| *o == lib)
            {
                *owed &= !(1 << i);
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
            WarmItem::Pipeline(..) => None,
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
        let rows = first_frame_rows(knobs);
        let latch = self.latch;
        let lane = |lane: Lane| -> Vec<(Pipeline, PixelFormat)> {
            rows.iter()
                .copied()
                .filter(|(row, _)| Lane::of(row.spec().library) == lane)
                .collect()
        };
        let (cell, present) = (lane(Lane::Cell), lane(Lane::Present));
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
            let cell = latch.run_lane(|| walk_lane(latch, Lane::Cell, dev, &cell, compile));
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

impl Drop for FirstFrameWarm {
    fn drop(&mut self) {
        self.latch.settle();
    }
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
        let src = format!("{}\n// bench nonce {nonce}\n", super::shaders::source(lib));
        dev.new_library_with_options(&src, &opts)
    }
}

/// THE FIRST-FRAME COMPILE MEASUREMENT's warm half (`benches/first_frame_compile.rs`, which runs the job on a thread of its own): begin the warm — with the launch's default knobs, or with the aurora off, the text frame and the blit alone, as the warm was before 2026-10-17 — and hand back its job, which answers how long the warm took.
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

/// THE FIRST-FRAME COMPILE MEASUREMENT's main half: the first mint's compiles on the calling thread (`cell.metal`, the text frame's rows, the present blit), each after waiting out the same item in flight on a warm the caller began, as the arm's own do, then what the first prompt's aurora binds (the glow row, the bloom, the SDR crown).
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
    for (row, present) in first_frame_rows(FirstFrameKnobs::default())
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
    }
}
