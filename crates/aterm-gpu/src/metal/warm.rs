// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first window's shaders, compiled once on a worker and discarded.
//!
//! `MetalArmLive::new` compiles `cell.metal` (`newLibraryWithSource:`) on the
//! main thread inside the first window's surface attach, and the first frame
//! then builds its PSOs and present libraries there too. On the owner's first
//! launch of 0.104 (2026-10-15) that was 234.77 ms of surface create (the
//! compiler service's library build alone 206 ms, a cold front-end module
//! inside it) and 135.94 ms of first-frame transaction. Every update whose
//! shader bytes changed pays the library half again, inside its freeze.
//!
//! Metal keeps compiled libraries and pipelines in a per-user cache keyed by
//! source and descriptor, so the same compile on ANY thread makes the main
//! thread's later one a cache hit (0.4 ms measured, against 51 ms for a
//! changed source and 214 ms with a cold front-end module). [`FirstFrameWarm`]
//! does that compile on a worker the launch spawns beside its backend worker:
//! the libraries and the PSOs the first frame binds, every object dropped
//! before it returns. Nothing crosses threads, so the arm's thread pin, its
//! `Send` rules and the mint-failure route (`metal_mint_error`, disarm, CPU
//! fallback) are untouched, and a failed warm changes nothing but the cache it
//! did not fill. Its device is [`Device::preferred`], the arm's own, which
//! never asks the window server where the device listing decides.
//!
//! The one coordination is [`WarmLatch`]: the first mint waits for the warm's
//! `cell.metal` compile instead of racing a duplicate of it, and never past
//! [`CELL_DEADLINE`] after the warm began.

use std::sync::{Condvar, Mutex, PoisonError};

use aterm_time::{Duration, Instant};

use super::ffi::{AutoreleasePool, Device, Library, PixelFormat};
use super::pipelines;
use crate::pipeline_table::{Pipeline, ShaderLibrary};

/// The `cell.metal` rows a first frame binds. `MetalArmLive::pso` builds
/// every frame row at `Bgra8Unorm`, which an offscreen target ignores.
const FIRST_FRAME_ROWS: [Pipeline; 3] = [Pipeline::Bg, Pipeline::Glyph, Pipeline::ColorGlyph];

/// The present blit's swapchain formats. Which one a window gets depends on
/// its screen's EDR potential, which only the attach can read, so both.
const PRESENT_FORMATS: [PixelFormat; 2] = [PixelFormat::Bgra8Unorm, PixelFormat::Rgba16Float];

/// How long after a warm began the first mint will still wait for it. The warm
/// is the mint's own compile started earlier, so waiting beats compiling again
/// unless the warm is wedged or starved; this caps that case. A cold library
/// build with a cold front-end module measured 213.6 ms (2026-10-15, M4 Max),
/// and the XPC and device setup around it add about 20 ms more.
pub(crate) const CELL_DEADLINE: Duration = Duration::from_millis(300);

/// Where a warm's `cell.metal` compile stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No warm began: headless, a CPU launch, a test.
    Idle,
    /// A warm is compiling `cell.metal`; it began at this instant.
    Compiling(Instant),
    /// The warm's `cell.metal` compile returned, or the warm ended without it.
    Settled,
}

/// The rendezvous between a warm and the first mint. A process has one
/// ([`FIRST_FRAME`]); the tests build their own.
pub(crate) struct WarmLatch {
    phase: Mutex<Phase>,
    settled: Condvar,
    /// How long after a warm began [`Self::await_cell`] still waits for it.
    deadline: Duration,
}

/// The process's one first-frame warm, which the first mint awaits.
pub(crate) static FIRST_FRAME: WarmLatch = WarmLatch::new(CELL_DEADLINE);

impl WarmLatch {
    /// A latch whose mint waits at most `deadline` after the warm began:
    /// [`CELL_DEADLINE`] in the process, a hang detector in a test that holds
    /// the warm and so must not race the shipped window.
    pub(crate) const fn new(deadline: Duration) -> Self {
        Self {
            phase: Mutex::new(Phase::Idle),
            settled: Condvar::new(),
            deadline,
        }
    }

    /// Claim this latch's one warm: `None` once one has begun, so a launch
    /// that warms its config twice (`warm_config_moved`) compiles once.
    pub(crate) fn begin(&'static self) -> Option<FirstFrameWarm> {
        // Past a poisoned lock throughout: every writer stores one whole value.
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        if *phase != Phase::Idle {
            return None;
        }
        *phase = Phase::Compiling(Instant::now());
        Some(FirstFrameWarm {
            latch: self,
            cell_pending: true,
        })
    }

    fn settle(&self) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = Phase::Settled;
        self.settled.notify_all();
    }

    /// Wait out a warm's `cell.metal` compile, never past this latch's deadline
    /// ([`CELL_DEADLINE`] for [`FIRST_FRAME`]) after the warm began. Returns at
    /// once when no warm is compiling.
    pub(crate) fn await_cell(&self) {
        let mut phase = self.phase.lock().unwrap_or_else(PoisonError::into_inner);
        while let Phase::Compiling(began) = *phase {
            let Some(left) = (began + self.deadline).checked_duration_since(Instant::now()) else {
                return;
            };
            phase = self
                .settled
                .wait_timeout(phase, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Whether a warm began on this latch.
    pub(crate) fn began(&self) -> bool {
        self.current_phase() != Phase::Idle
    }

    /// The phase as of now.
    fn current_phase(&self) -> Phase {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The right to run the process's first-frame warm, from
/// [`crate::begin_first_frame_warm`]. Dropping it unrun (a failed spawn, a
/// panic) releases the first mint as finishing it does.
pub struct FirstFrameWarm {
    latch: &'static WarmLatch,
    cell_pending: bool,
}

impl FirstFrameWarm {
    /// Compile the first frame's libraries and PSOs and drop them all:
    /// `cell.metal` (then the latch releases the first mint) and its first-frame
    /// rows, the present blit at both swapchain formats, and the HDR crown's
    /// library when `hdr_glow` is on. The first failure ends the warm; the
    /// caller only reports it, since the first window compiles whatever the
    /// warm did not.
    ///
    /// # Errors
    ///
    /// No Metal device, or the first compile or build that failed.
    pub fn run(mut self, hdr_glow: bool) -> Result<(), String> {
        // Device creation, compiles and every release autorelease into this
        // pool (`ffi`'s rule); declared first, so it pops last.
        let _pool = AutoreleasePool::new();
        let dev =
            Device::preferred().ok_or_else(|| "no Metal device on this machine".to_owned())?;
        self.run_on(&dev, hdr_glow, pipelines::compile_library)
    }

    /// [`Self::run`]'s body over a given compile, so a test can fail it.
    pub(crate) fn run_on(
        &mut self,
        dev: &Device,
        hdr_glow: bool,
        compile: impl Fn(&Device, ShaderLibrary) -> Result<Library, String>,
    ) -> Result<(), String> {
        let cell = compile(dev, ShaderLibrary::Cell);
        self.release_cell();
        let cell = cell?;
        for row in FIRST_FRAME_ROWS {
            pipelines::build(dev, &cell, row.spec(), PixelFormat::Bgra8Unorm)?;
        }
        let blit = compile(dev, ShaderLibrary::Blit)?;
        for present in PRESENT_FORMATS {
            pipelines::build(dev, &blit, Pipeline::Blit.spec(), present)?;
        }
        if hdr_glow {
            compile(dev, ShaderLibrary::HdrGlow)?;
        }
        Ok(())
    }

    fn release_cell(&mut self) {
        if std::mem::take(&mut self.cell_pending) {
            self.latch.settle();
        }
    }
}

impl Drop for FirstFrameWarm {
    fn drop(&mut self) {
        self.release_cell();
    }
}

/// The latch the first-frame compile measurement (`benches/first_frame_compile.rs`)
/// warms and waits on: one per process, as a launch's is.
static BENCH_LATCH: WarmLatch = WarmLatch::new(CELL_DEADLINE);

/// The measurement's compile: the shipped source plus a per-run `nonce`
/// comment, so every library is a cache miss.
fn bench_compile(nonce: u128) -> impl Fn(&Device, ShaderLibrary) -> Result<Library, String> + Copy {
    move |dev: &Device, lib: ShaderLibrary| -> Result<Library, String> {
        let opts = super::ffi::CompileOptions::aterm_default()
            .ok_or_else(|| "no MTLCompileOptions".to_owned())?;
        let src = format!("{}\n// bench nonce {nonce}\n", super::shaders::source(lib));
        dev.new_library_with_options(&src, &opts)
    }
}

/// THE FIRST-FRAME COMPILE MEASUREMENT's warm half
/// (`benches/first_frame_compile.rs`, which runs the job on a thread of its
/// own): begin the warm and hand back its job, which answers how long the warm
/// took. One per process.
///
/// # Errors
/// A second call in one process.
#[doc(hidden)]
pub fn first_frame_bench_warm(
    nonce: u128,
) -> Result<impl FnOnce() -> Result<Duration, String> + Send + 'static, String> {
    let mut warm = BENCH_LATCH
        .begin()
        .ok_or_else(|| "one warm per process".to_owned())?;
    Ok(move || {
        let _pool = AutoreleasePool::new();
        let began = Instant::now();
        let dev = Device::preferred().ok_or_else(|| "no Metal device".to_owned())?;
        warm.run_on(&dev, true, bench_compile(nonce))?;
        Ok(began.elapsed())
    })
}

/// THE FIRST-FRAME COMPILE MEASUREMENT's main half: the first mint's compiles
/// on the calling thread (`cell.metal`, the first-frame rows, the present
/// blit), after waiting for a warm the caller began, if any. Answers the
/// times since the call at which it stopped waiting, built the cell library,
/// built the frame rows, and finished the present blit.
///
/// # Errors
/// No Metal device, or a shipped shader that does not compile.
#[doc(hidden)]
pub fn first_frame_bench_main(nonce: u128) -> Result<[Duration; 4], String> {
    let compile = bench_compile(nonce);
    let _pool = AutoreleasePool::new();
    let arrived = Instant::now();
    BENCH_LATCH.await_cell();
    let waited = arrived.elapsed();
    let dev = Device::preferred().ok_or_else(|| "no Metal device".to_owned())?;
    let cell = compile(&dev, ShaderLibrary::Cell)?;
    let lib = arrived.elapsed();
    for row in FIRST_FRAME_ROWS {
        pipelines::build(&dev, &cell, row.spec(), PixelFormat::Bgra8Unorm)?;
    }
    let rows = arrived.elapsed();
    let blit = compile(&dev, ShaderLibrary::Blit)?;
    pipelines::build(&dev, &blit, Pipeline::Blit.spec(), PixelFormat::Bgra8Unorm)?;
    Ok([waited, lib, rows, arrived.elapsed()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metal::test_support::device;
    use std::sync::mpsc;

    /// The one minute a wait for something that must happen gets.
    const HANG: Duration = Duration::from_secs(60);

    /// A warm whose first compile fails releases the first mint as it fails
    /// and reports the failure; a second warm on the same latch is refused.
    #[test]
    fn a_failed_warm_releases_the_mint_and_is_not_retried() {
        static LATCH: WarmLatch = WarmLatch::new(CELL_DEADLINE);
        let dev = device();
        let mut warm = LATCH.begin().expect("the latch's first warm");
        assert!(matches!(LATCH.current_phase(), Phase::Compiling(_)));
        let failed = warm.run_on(
            &dev,
            true,
            |_, _| Err("injected compile failure".to_owned()),
        );
        assert_eq!(failed, Err("injected compile failure".to_owned()));
        assert_eq!(
            LATCH.current_phase(),
            Phase::Settled,
            "the failure released the mint"
        );
        LATCH.await_cell();
        drop(warm);
        assert!(LATCH.begin().is_none(), "one warm per latch");
        assert!(LATCH.began());
    }

    /// A warm dropped before it ran (its thread never spawned, or panicked)
    /// releases the first mint.
    #[test]
    fn a_warm_dropped_unrun_releases_the_mint() {
        static LATCH: WarmLatch = WarmLatch::new(CELL_DEADLINE);
        assert!(!LATCH.began(), "no warm, nothing to wait for");
        LATCH.await_cell();
        drop(LATCH.begin().expect("the latch's first warm"));
        assert_eq!(LATCH.current_phase(), Phase::Settled);
    }

    /// The mint waits for the warm's `cell.metal` compile and no further: the
    /// latch releases while the warm is still building the rest.
    #[test]
    fn the_mint_waits_for_the_cell_compile_and_not_the_rest() {
        // The hold below compiles the real `cell.metal`, which a cold cache on
        // a loaded machine can take past `CELL_DEADLINE`: the mint's wait here
        // is a hang detector, so only a latch that never settles fails it.
        static LATCH: WarmLatch = WarmLatch::new(HANG);
        // COULD NOT RUN without a Metal device; the worker opens its own.
        device();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (blit_tx, blit_rx) = mpsc::channel::<()>();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let mut warm = LATCH.begin().expect("the latch's first warm");
        let worker = std::thread::spawn(move || {
            let _pool = AutoreleasePool::new();
            let dev = Device::preferred().expect("the device the test found");
            warm.run_on(&dev, false, |dev, lib| {
                match lib {
                    ShaderLibrary::Cell => go_rx.recv_timeout(HANG).expect("the test's go"),
                    _ => {
                        blit_tx.send(()).expect("the test is listening");
                        finish_rx.recv_timeout(HANG).expect("the test's finish");
                    }
                }
                pipelines::compile_library(dev, lib)
            })
        });
        assert!(
            matches!(LATCH.current_phase(), Phase::Compiling(_)),
            "the warm is held inside its cell compile"
        );
        go_tx.send(()).expect("the warm is listening");
        LATCH.await_cell();
        assert_eq!(
            LATCH.current_phase(),
            Phase::Settled,
            "released at the cell compile"
        );
        blit_rx
            .recv_timeout(HANG)
            .expect("the warm went on to the present library");
        assert_eq!(
            LATCH.current_phase(),
            Phase::Settled,
            "still released while the warm builds the rest"
        );
        finish_tx.send(()).expect("the warm is listening");
        worker
            .join()
            .expect("the warm thread")
            .expect("the shipped shaders compile");
    }

    /// The warm compiles and builds every library and row it names on the
    /// shipped sources: a row or format it cannot build would fail every launch.
    #[test]
    fn the_warm_builds_the_shipped_first_frame_shaders() {
        static LATCH: WarmLatch = WarmLatch::new(CELL_DEADLINE);
        let dev = device();
        let mut warm = LATCH.begin().expect("the latch's first warm");
        warm.run_on(&dev, true, pipelines::compile_library)
            .expect("the first-frame warm on the shipped shaders");
    }
}
