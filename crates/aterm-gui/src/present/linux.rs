// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Linux CPU presenter dispatching to [`super::x11`] (MIT-SHM) or [`super::wayland`] (`wl_shm`).

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use aterm_types::native_window::{HasNativeWindow, NativeWindow};
use aterm_window::window::Window;

use super::wayland::{WaylandFrame, WaylandPresentError, WaylandPresenter};
use super::x11::{X11Frame, X11PresentError, X11Presenter};
use super::{CpuFrameBuffer, CpuPresenter, DamageRect};

/// One window's presenter, by display server.
pub(crate) enum LinuxPresenter {
    X11(X11Presenter),
    Wayland(WaylandPresenter),
}

/// Either presenter's failure.
#[derive(Debug)]
pub(crate) enum LinuxPresentError {
    X11(X11PresentError),
    Wayland(WaylandPresentError),
    /// Neither an X11 nor a Wayland window.
    Unsupported,
}

impl std::fmt::Display for LinuxPresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::X11(e) => write!(f, "{e}"),
            Self::Wayland(e) => write!(f, "{e}"),
            Self::Unsupported => f.write_str("neither an X11 nor a Wayland window"),
        }
    }
}

/// One acquired frame of either presenter.
pub(crate) enum LinuxFrame<'a> {
    X11(X11Frame<'a>),
    Wayland(WaylandFrame<'a>),
}

impl CpuPresenter for LinuxPresenter {
    type Error = LinuxPresentError;
    type Buffer<'a>
        = LinuxFrame<'a>
    where
        Self: 'a;

    fn new(window: Arc<Window>) -> Result<Self, Self::Error> {
        match window.native_window() {
            Some(NativeWindow::Xlib { .. }) => X11Presenter::new(window)
                .map(Self::X11)
                .map_err(LinuxPresentError::X11),
            Some(NativeWindow::Wayland { .. }) => WaylandPresenter::new(window)
                .map(Self::Wayland)
                .map_err(LinuxPresentError::Wayland),
            None => Err(LinuxPresentError::Unsupported),
        }
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error> {
        match self {
            Self::X11(p) => p.resize(width, height).map_err(LinuxPresentError::X11),
            Self::Wayland(p) => p.resize(width, height).map_err(LinuxPresentError::Wayland),
        }
    }

    fn capture_submit_fence(&mut self) -> Result<Box<dyn super::SubmitFence>, String> {
        match self {
            Self::X11(p) => p.capture_submit_fence(),
            Self::Wayland(p) => p.capture_submit_fence(),
        }
    }

    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error> {
        match self {
            Self::X11(p) => p
                .buffer_mut()
                .map(LinuxFrame::X11)
                .map_err(LinuxPresentError::X11),
            Self::Wayland(p) => p
                .buffer_mut()
                .map(LinuxFrame::Wayland)
                .map_err(LinuxPresentError::Wayland),
        }
    }
}

impl Deref for LinuxFrame<'_> {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        match self {
            Self::X11(f) => f,
            Self::Wayland(f) => f,
        }
    }
}

impl DerefMut for LinuxFrame<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        match self {
            Self::X11(f) => f,
            Self::Wayland(f) => f,
        }
    }
}

impl CpuFrameBuffer for LinuxFrame<'_> {
    type Error = LinuxPresentError;

    fn age(&self) -> u8 {
        match self {
            Self::X11(f) => f.age(),
            Self::Wayland(f) => f.age(),
        }
    }

    fn present(self) -> Result<(), Self::Error> {
        match self {
            Self::X11(f) => f.present().map_err(LinuxPresentError::X11),
            Self::Wayland(f) => f.present().map_err(LinuxPresentError::Wayland),
        }
    }

    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error> {
        match self {
            Self::X11(f) => f
                .present_with_damage(damage)
                .map_err(LinuxPresentError::X11),
            Self::Wayland(f) => f
                .present_with_damage(damage)
                .map_err(LinuxPresentError::Wayland),
        }
    }
}

/// A capture's hang detector observes only its native display peer. GUI timers,
/// this worker's polling and unrelated descendants cannot keep an idle peer alive.
pub(super) struct SubmitProgress {
    pid: u32,
    birth: Option<u64>,
    previous: Option<PeerWork>,
    worked: std::time::Instant,
    sampled: std::time::Instant,
}

#[derive(Clone, Copy)]
struct PeerWork {
    birth: u64,
    counters: [u64; 9],
}

enum PeerReading {
    Ended,
    Unknown(Option<u64>),
    Work(PeerWork),
}

impl SubmitProgress {
    pub(super) fn new(fd: std::os::fd::RawFd) -> Result<Self, String> {
        let pid = aterm_uds::fdpass::peer_pid_of_fd(fd)
            .map_err(|error| format!("the display peer identity is unread: {error}"))?;
        let now = std::time::Instant::now();
        let mut progress = Self {
            pid,
            birth: None,
            previous: None,
            worked: now,
            sampled: now,
        };
        progress.observe(peer_work(pid), now)?;
        Ok(progress)
    }

    pub(super) fn check(&mut self) -> Result<(), String> {
        let now = std::time::Instant::now();
        if now.duration_since(self.sampled) >= std::time::Duration::from_millis(500) {
            self.observe(peer_work(self.pid), now)?;
        }
        Ok(())
    }

    fn observe(&mut self, reading: PeerReading, now: std::time::Instant) -> Result<(), String> {
        self.sampled = now;
        let (birth, work) = match reading {
            PeerReading::Ended => return Err("the display peer ended before confirmation".into()),
            PeerReading::Unknown(birth) => (birth, None),
            PeerReading::Work(work) => (Some(work.birth), Some(work)),
        };
        if let Some(birth) = birth {
            if self.birth.is_some_and(|original| original != birth) {
                return Err("the display peer identity changed before confirmation".into());
            }
            self.birth = Some(birth);
        }
        match (self.previous, work) {
            (Some(before), Some(after))
                if after
                    .counters
                    .iter()
                    .zip(before.counters)
                    .all(|(&new, old)| new >= old) =>
            {
                if after.counters != before.counters {
                    self.worked = now;
                }
                if now.duration_since(self.worked) >= std::time::Duration::from_secs(60) {
                    return Err("the display peer made no CPU or I/O progress for a minute".into());
                }
            }
            // A missing or incomparable observation disarms the detector. The first
            // later complete reading establishes a new baseline, never past idle time.
            _ => self.worked = now,
        }
        self.previous = work;
        Ok(())
    }
}

fn peer_work(pid: u32) -> PeerReading {
    fn stat(pid: u32) -> Result<Option<(u64, [u64; 2])>, std::io::Error> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let Some((owner, fields)) = text.rsplit_once(") ") else {
            return Ok(None);
        };
        if !owner.starts_with(&format!("{pid} (")) {
            return Ok(None);
        }
        let fields: Vec<_> = fields.split_whitespace().collect();
        Ok((|| {
            Some((
                fields.get(19)?.parse().ok()?,
                [fields.get(11)?.parse().ok()?, fields.get(12)?.parse().ok()?],
            ))
        })())
    }
    fn read_stat(pid: u32) -> Result<(u64, [u64; 2]), PeerReading> {
        match stat(pid) {
            Ok(Some(value)) => Ok(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(PeerReading::Ended),
            _ => Err(PeerReading::Unknown(None)),
        }
    }
    let (birth, _) = match read_stat(pid) {
        Ok(stat) => stat,
        Err(reading) => return reading,
    };
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok();
    let counters = io.and_then(|text| {
        let mut lines = text.lines();
        let mut counters = [0; 9];
        for (at, key) in [
            "rchar:",
            "wchar:",
            "syscr:",
            "syscw:",
            "read_bytes:",
            "write_bytes:",
            "cancelled_write_bytes:",
        ]
        .iter()
        .enumerate()
        {
            counters[at + 2] = lines.next()?.strip_prefix(key)?.trim().parse().ok()?;
        }
        lines.next().is_none().then_some(counters)
    });
    let (again, cpu) = match read_stat(pid) {
        Ok(stat) => stat,
        Err(reading) => return reading,
    };
    if birth != again {
        return PeerReading::Unknown(Some(again));
    }
    match counters {
        Some(mut counters) => {
            counters[..2].copy_from_slice(&cpu);
            PeerReading::Work(PeerWork { birth, counters })
        }
        None => PeerReading::Unknown(Some(birth)),
    }
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
