// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first commit after the GPU went quiet (ATERM-KI-1027-4,
//! `docs/measured/idle-memory-rest-2026-10-29.md`, *Left*). A resting window's woken key submits
//! its echo frame slower than an awake window's, after the driver freed its render-pass block.
//! This times one encode session's full-size clear pass after a quiet with no GPU work, either
//! cold or a lead after a 1x1 clear pass on the same queue (the rest's pre-warm arm). It opens no
//! window and reaches no window server: `benches/quiet_commit.rs` runs it.

use std::sync::Arc;

use aterm_time::{Duration, Instant};

use super::encoder::{EncodeSession, RenderPassDesc, StoreAction, Submitted};
use super::ffi::{
    AutoreleasePool, ClearColor, Device, LoadAction, PixelFormat, TEXTURE_USAGE_RENDER_TARGET,
};
use super::loss::{CbOutcome, LossLatch};
use super::resources::{MetalResourceDevice, SealedTexture};

/// One take's plan: how long the GPU has no work before the timed pass, and how long before it
/// the 1x1 pre-warm pass is committed (`None`: no pre-warm, a cold commit).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuietTake {
    /// The quiet before the take, measured from the previous take's completion.
    pub quiet: Duration,
    /// The pre-warm's lead over the timed pass, or `None` for none.
    pub lead: Option<Duration>,
}

/// What one take measured, each span on the CPU's monotonic clock.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct QuietCommit {
    /// The take's plan.
    pub take: QuietTake,
    /// From the pre-warm's commit returning to the timed pass's `commandBuffer`: the lead as it
    /// came (a sleep overshoots its ask).
    pub lead: Option<Duration>,
    /// The pre-warm pass's own `commandBuffer` through `-commit`.
    pub warm: Option<Duration>,
    /// The timed pass's `commandBuffer` through its `endEncoding`.
    pub encode: Duration,
    /// Its `-commit` alone.
    pub commit: Duration,
    /// From its commit returning to `waitUntilCompleted` returning.
    pub complete: Duration,
    /// How long the GPU ran it (`GPUEndTime - GPUStartTime`): the rest of `complete` is the wait
    /// before it ran and the wake after.
    pub gpu: Duration,
    /// What `footprint` answered once the quiet ended, before either pass.
    pub footprint_quiet: Option<u64>,
    /// What `footprint` answered after the timed pass completed.
    pub footprint_done: Option<u64>,
}

/// Run `takes` in order on one encode session against a `width` x `height` Bgra8 target,
/// reading the process's footprint through `footprint` (the caller's: this crate links no
/// `proc_pid_rusage`) at each quiet's end and each pass's completion. One untimed pass on each
/// target comes first, so no take pays a queue's or a texture's first use.
/// # Errors
/// No Metal device, an allocation that fails, or a pass that does not complete.
#[doc(hidden)]
pub fn quiet_commit_probe(
    (width, height): (usize, usize),
    takes: &[QuietTake],
    footprint: &dyn Fn() -> Option<u64>,
) -> Result<Vec<QuietCommit>, String> {
    let _pool = AutoreleasePool::new();
    let dev = Device::preferred_listed().ok_or_else(|| "no listed Metal device".to_owned())?;
    let latch = Arc::new(LossLatch::new());
    let session = EncodeSession::new(&dev, Arc::clone(&latch))?;
    let res = MetalResourceDevice::new(&dev, latch);
    let frame = res.texture_2d(
        PixelFormat::Bgra8Unorm,
        width,
        height,
        TEXTURE_USAGE_RENDER_TARGET,
    )?;
    let dot = res.texture_2d(PixelFormat::Bgra8Unorm, 1, 1, TEXTURE_USAGE_RENDER_TARGET)?;
    for target in [&frame, &dot] {
        completed(&clear(&session, target)?)?;
    }
    let mut out = Vec::with_capacity(takes.len());
    for &take in takes {
        let _pool = AutoreleasePool::new();
        std::thread::sleep(take.quiet);
        let footprint_quiet = footprint();
        let warmed = match take.lead {
            None => None,
            Some(lead) => {
                let began = Instant::now();
                let sub = clear(&session, &dot)?;
                let committed = Instant::now();
                std::thread::sleep(lead);
                Some((committed - began, committed, sub))
            }
        };
        let began = Instant::now();
        let mut cb = session.begin()?;
        drop(cb.render_pass(&pass(&frame))?);
        let encoded = Instant::now();
        let sub = cb.commit();
        let committed = Instant::now();
        let outcome = sub.wait_outcome();
        let done = Instant::now();
        expect_completed(outcome)?;
        if let Some((_, _, warm)) = &warmed {
            completed(warm)?;
        }
        out.push(QuietCommit {
            take,
            lead: warmed.as_ref().map(|(_, at, _)| began - *at),
            warm: warmed.as_ref().map(|(took, _, _)| *took),
            encode: encoded - began,
            commit: committed - encoded,
            complete: done - committed,
            gpu: sub.gpu_span(),
            footprint_quiet,
            footprint_done: footprint(),
        });
    }
    Ok(out)
}

/// A whole-target clear pass that stores what it cleared.
fn pass(target: &SealedTexture) -> RenderPassDesc<'_> {
    RenderPassDesc {
        target,
        load: LoadAction::Clear(ClearColor {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }),
        store: StoreAction::Store,
        viewport: None,
        scissor: None,
    }
}

/// One command buffer holding [`pass`] on `target`, committed.
fn clear(session: &EncodeSession, target: &SealedTexture) -> Result<Submitted, String> {
    let mut cb = session.begin()?;
    drop(cb.render_pass(&pass(target))?);
    Ok(cb.commit())
}

/// Wait for `sub` and require it completed.
fn completed(sub: &Submitted) -> Result<(), String> {
    expect_completed(sub.wait_outcome())
}

fn expect_completed(outcome: CbOutcome) -> Result<(), String> {
    match outcome {
        CbOutcome::Completed => Ok(()),
        other => Err(format!("a probe pass did not complete: {other:?}")),
    }
}
