// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: a scripted CPU presenter, so a headless `App` runs the real
//! redraw path (compose, damage consume, CPU present, dropped-present re-arm,
//! bounded retry) and a test reads back exactly what reached the glass.
//!
//! In a test build [`super::CpuSurface`] is [`TestCpuSurface`]: the platform
//! presenter (what `CpuPresenter::new` builds) or a [`ScriptedPresenter`] a
//! test installs directly, with no `winit` window and no WindowServer.
//!
//! The scripted glass models a retaining surface: after a successful commit the
//! next acquisition returns a buffer of `age() == 1` that still holds what the
//! glass shows, and a failed commit discards what was painted into it. A retry
//! that trusted a stale renderer cache over such a buffer would copy nothing and
//! leave the dropped frame off the glass.

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use aterm_window::window::Window;

use super::{CpuFrameBuffer, CpuPresenter, DamageRect};

#[cfg(target_os = "macos")]
type PlatformSurface = super::mac::MacCpuPresenter;
#[cfg(target_os = "linux")]
type PlatformSurface = super::linux::LinuxPresenter;
#[cfg(windows)]
type PlatformSurface = super::win32::Win32Presenter;

/// One scripted failure, consumed by the present it applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScriptedOutcome {
    /// `buffer_mut` fails: nothing is painted, nothing reaches glass.
    FailAcquire,
    /// The buffer is painted but its commit fails: nothing reaches glass.
    FailCommit,
}

/// The scripted presenter. Presents succeed unless the script says otherwise.
#[derive(Debug, Default)]
pub(crate) struct ScriptedPresenter {
    width: usize,
    height: usize,
    /// What the last successful commit put on screen.
    glass: Vec<u32>,
    /// The retained buffer `buffer_mut` hands out.
    back: Vec<u32>,
    /// `1` once a commit succeeded and `back` still equals `glass`.
    age: u8,
    script: VecDeque<ScriptedOutcome>,
    commits: usize,
    /// The window system asks for part of the glass to be drawn again
    /// ([`Self::owe_repaint`]); a successful commit settles it.
    owed: bool,
}

impl ScriptedPresenter {
    /// Queue the outcome of the next present that reaches this surface.
    pub(crate) fn fail_next(&mut self, outcome: ScriptedOutcome) {
        self.script.push_back(outcome);
    }

    /// The pixels on glass (`0x00RRGGBB`, `width * height`).
    pub(crate) fn glass(&self) -> &[u32] {
        &self.glass
    }

    /// `(width, height)` of the glass.
    pub(crate) fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Successful commits so far.
    pub(crate) fn commits(&self) -> usize {
        self.commits
    }

    /// The window gains `rows` its last commit never reached, as Windows
    /// grows a window under a present: the glass there holds the window
    /// system's white, and the surface owes a repaint until a commit draws
    /// the frame it retains.
    pub(crate) fn owe_repaint(&mut self, rows: std::ops::Range<usize>) {
        for row in rows {
            let at = row * self.width;
            if let Some(line) = self.glass.get_mut(at..at + self.width) {
                line.fill(0x00ff_ffff);
            }
        }
        self.owed = true;
    }

    /// The window system drops what the glass showed, as an X server does
    /// before it sends an `Expose` (a VT switch, a video-memory purge): the
    /// glass goes black, and the surface owes nothing it knows of. The retained
    /// buffer goes black too, though its age still says it holds the last
    /// present: a scripted commit copies the whole buffer, so this stands for
    /// a surface that sends only the damage it is given (X11's ShmPutImage per
    /// rectangle), where only a full copy puts the frame back.
    #[cfg(target_os = "linux")]
    pub(crate) fn lose_glass(&mut self) {
        self.glass.fill(0);
        self.back.fill(0);
    }
}

/// A scripted surface refusal.
#[derive(Debug)]
pub(crate) struct ScriptedError;

impl std::fmt::Display for ScriptedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("scripted present failure")
    }
}

/// The test build's CPU surface: the platform presenter or a scripted one.
pub(crate) enum TestCpuSurface {
    Platform(PlatformSurface),
    Scripted(ScriptedPresenter),
}

impl TestCpuSurface {
    /// The scripted presenter, when this surface is one.
    pub(crate) fn scripted(&self) -> Option<&ScriptedPresenter> {
        match self {
            Self::Scripted(s) => Some(s),
            Self::Platform(_) => None,
        }
    }

    /// Mutable twin of [`Self::scripted`].
    pub(crate) fn scripted_mut(&mut self) -> Option<&mut ScriptedPresenter> {
        match self {
            Self::Scripted(s) => Some(s),
            Self::Platform(_) => None,
        }
    }
}

/// The error of either backend, carried as its message.
#[derive(Debug)]
pub(crate) struct TestSurfaceError(String);

impl std::fmt::Display for TestSurfaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn wrap<E: std::fmt::Display>(e: E) -> TestSurfaceError {
    TestSurfaceError(e.to_string())
}

/// The acquired frame of either backend.
pub(crate) enum TestCpuFrame<'a> {
    Platform(<PlatformSurface as CpuPresenter>::Buffer<'a>),
    Scripted(&'a mut ScriptedPresenter),
}

impl CpuPresenter for TestCpuSurface {
    type Error = TestSurfaceError;
    type Buffer<'a>
        = TestCpuFrame<'a>
    where
        Self: 'a;

    fn new(window: Arc<Window>) -> Result<Self, Self::Error> {
        PlatformSurface::new(window)
            .map(Self::Platform)
            .map_err(wrap)
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error> {
        match self {
            Self::Platform(p) => p.resize(width, height).map_err(wrap),
            Self::Scripted(s) => {
                let (w, h) = (width.get() as usize, height.get() as usize);
                if (w, h) != (s.width, s.height) {
                    s.width = w;
                    s.height = h;
                    s.glass = vec![0; w * h];
                    s.back = vec![0; w * h];
                    s.age = 0;
                }
                Ok(())
            }
        }
    }

    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error> {
        match self {
            Self::Platform(p) => p.buffer_mut().map(TestCpuFrame::Platform).map_err(wrap),
            Self::Scripted(s) => {
                if s.script.front() == Some(&ScriptedOutcome::FailAcquire) {
                    s.script.pop_front();
                    return Err(wrap(ScriptedError));
                }
                Ok(TestCpuFrame::Scripted(s))
            }
        }
    }

    fn owes_repaint(&self) -> bool {
        match self {
            Self::Platform(p) => p.owes_repaint(),
            Self::Scripted(s) => s.owed,
        }
    }
}

impl Deref for TestCpuFrame<'_> {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        match self {
            Self::Platform(b) => b,
            Self::Scripted(s) => &s.back,
        }
    }
}

impl DerefMut for TestCpuFrame<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        match self {
            Self::Platform(b) => b,
            Self::Scripted(s) => &mut s.back,
        }
    }
}

impl TestCpuFrame<'_> {
    fn commit_scripted(s: &mut ScriptedPresenter) -> Result<(), TestSurfaceError> {
        if s.script.front() == Some(&ScriptedOutcome::FailCommit) {
            s.script.pop_front();
            // What was painted never reached glass; the retained buffer is
            // still the glass the surface shows.
            s.back.clone_from(&s.glass);
            return Err(wrap(ScriptedError));
        }
        s.glass.clone_from(&s.back);
        s.age = 1;
        s.commits += 1;
        s.owed = false;
        Ok(())
    }
}

impl CpuFrameBuffer for TestCpuFrame<'_> {
    type Error = TestSurfaceError;

    fn age(&self) -> u8 {
        match self {
            Self::Platform(b) => b.age(),
            Self::Scripted(s) => s.age,
        }
    }

    fn present(self) -> Result<(), Self::Error> {
        match self {
            Self::Platform(b) => b.present().map_err(wrap),
            Self::Scripted(s) => Self::commit_scripted(s),
        }
    }

    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error> {
        match self {
            Self::Platform(b) => b.present_with_damage(damage).map_err(wrap),
            Self::Scripted(s) => Self::commit_scripted(s),
        }
    }
}
