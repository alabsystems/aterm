// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Ownership of one exact presented client until its independent photograph ends.

use std::sync::{Arc, Mutex};

/// The material presentation named by a held client capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Receipt {
    pub window: u64,
    pub serial: u64,
    pub surface: u64,
    pub width: u32,
    pub height: u32,
}

impl Receipt {
    pub(crate) fn wire_fields(self) -> String {
        format!(
            "window={} serial={} surface={} width={} height={}",
            self.window, self.serial, self.surface, self.width, self.height
        )
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Requested,
    Submitted(Receipt),
    Held(Receipt),
    Invalid(&'static str),
    Released,
}

struct Inner {
    phase: Mutex<Phase>,
    wake: Box<dyn Fn() + Send + Sync>,
}

/// The main thread's request and the window's held presentation. Neither keeps the
/// controller alive: only [`Owner`] does, independently of artifact quarantine.
#[derive(Clone)]
pub(crate) struct Lease(Arc<Inner>);

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PresentationLease")
            .field(&self.phase())
            .finish()
    }
}

/// The controller's sole lifetime guard. Every end, including disconnect or unwind,
/// releases the presentation while the artifact may remain quarantined.
pub(crate) struct Owner {
    lease: Lease,
}

impl std::fmt::Debug for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PresentationOwner")
            .field(&self.lease)
            .finish()
    }
}

impl Lease {
    pub(crate) fn new(
        proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    ) -> (Self, Owner) {
        let proxy = proxy.clone();
        Self::with_wake(move || {
            let _ = proxy.send_event(crate::Wake::PresentationReleased);
        })
    }

    pub(crate) fn with_wake(wake: impl Fn() + Send + Sync + 'static) -> (Self, Owner) {
        let lease = Self(Arc::new(Inner {
            phase: Mutex::new(Phase::Requested),
            wake: Box::new(wake),
        }));
        let owner = Owner {
            lease: lease.clone(),
        };
        (lease, owner)
    }

    fn phase(&self) -> Phase {
        *self.0.phase.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn receipt(&self) -> Result<Receipt, String> {
        match self.phase() {
            Phase::Held(receipt) => Ok(receipt),
            Phase::Submitted(_) => {
                Err("the native display has not confirmed the presentation".into())
            }
            Phase::Invalid(reason) => Err(reason.into()),
            Phase::Requested => Err("the presentation lease has not captured a client".into()),
            Phase::Released => Err("the presentation lease was released".into()),
        }
    }

    pub(crate) fn requested(&self) -> bool {
        matches!(self.phase(), Phase::Requested)
    }

    pub(crate) fn pin(&self, receipt: Receipt) -> Result<Held, String> {
        let mut phase = self.0.phase.lock().unwrap_or_else(|p| p.into_inner());
        if !matches!(*phase, Phase::Requested) {
            return Err("the presentation lease is no longer an unfulfilled request".into());
        }
        if receipt.serial == 0 || receipt.surface == 0 || receipt.width == 0 || receipt.height == 0
        {
            return Err("the presentation lease has no complete presented identity".into());
        }
        *phase = Phase::Submitted(receipt);
        Ok(Held {
            lease: self.clone(),
            receipt,
        })
    }

    pub(crate) fn confirm(&self, receipt: Receipt) -> Result<(), String> {
        let mut phase = self.0.phase.lock().unwrap_or_else(|p| p.into_inner());
        if !matches!(*phase, Phase::Submitted(current) if current == receipt) {
            return Err("the submitted presentation ended before native confirmation".into());
        }
        *phase = Phase::Held(receipt);
        Ok(())
    }

    fn holds(&self, receipt: Receipt) -> bool {
        matches!(self.phase(), Phase::Submitted(current) | Phase::Held(current) if current == receipt)
    }

    fn invalidate(&self, reason: &'static str) {
        let changed = {
            let mut phase = self.0.phase.lock().unwrap_or_else(|p| p.into_inner());
            if matches!(
                *phase,
                Phase::Requested | Phase::Submitted(_) | Phase::Held(_)
            ) {
                *phase = Phase::Invalid(reason);
                true
            } else {
                false
            }
        };
        if changed {
            (self.0.wake)();
        }
    }

    fn release(&self) -> Result<Receipt, String> {
        let result = {
            let mut phase = self.0.phase.lock().unwrap_or_else(|p| p.into_inner());
            let result = match *phase {
                Phase::Held(receipt) => Ok(receipt),
                Phase::Invalid(reason) => Err(reason.into()),
                Phase::Requested => Err("the presentation lease captured no client".into()),
                Phase::Submitted(_) => {
                    Err("the native display did not confirm the presentation".into())
                }
                Phase::Released => return Err("the presentation lease was already released".into()),
            };
            *phase = Phase::Released;
            result
        };
        (self.0.wake)();
        result
    }
}

/// The encode worker owns native confirmation and retires the fence before it
/// can expose pixels. Queue rejection and unwind invalidate the pinned frame.
pub(crate) struct Confirmation {
    lease: Lease,
    receipt: Receipt,
    fence: Option<Box<dyn crate::present::SubmitFence>>,
    confirmed: bool,
}

impl Confirmation {
    pub(crate) fn wait(
        mut self,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<(), String> {
        let lease = self.lease.clone();
        let receipt = self.receipt;
        let stopped = || cancelled() || !lease.holds(receipt);
        self.fence
            .take()
            .expect("one native confirmation")
            .wait(&stopped)?;
        if stopped() {
            return Err("the presentation ended during native confirmation".into());
        }
        self.lease.confirm(self.receipt)?;
        self.confirmed = true;
        Ok(())
    }
}

impl Drop for Confirmation {
    fn drop(&mut self) {
        if !self.confirmed {
            self.lease
                .invalidate("native presentation confirmation did not complete");
        }
    }
}

impl Owner {
    /// Validate the exact captured presentation and release it in the same election.
    pub(crate) fn finish(self) -> Result<Receipt, String> {
        self.lease.release()
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.lease.release();
    }
}

/// Installed on one window before its capture can be encoded or published.
pub(crate) struct Held {
    lease: Lease,
    receipt: Receipt,
}

impl Held {
    pub(crate) fn active(&self) -> bool {
        self.lease.holds(self.receipt)
    }

    pub(crate) fn blocks(&self, current: Receipt) -> bool {
        if current != self.receipt {
            self.invalidate("the held presentation's surface, dimensions or serial changed");
            return false;
        }
        self.active()
    }

    pub(crate) fn invalidate(&self, reason: &'static str) {
        self.lease.invalidate(reason);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.invalidate("the window stopped holding the captured presentation");
    }
}

impl crate::WindowState {
    fn presentation_receipt(&self, window: crate::WindowId) -> Option<Receipt> {
        self.present.as_ref()?;
        let size = self.win_px.map(|size| (size.width, size.height));
        #[cfg(any(test, all(unix, feature = "bench-support", debug_assertions)))]
        let size = size.or_else(|| match self.present.as_ref() {
            Some(crate::PresentTarget::Cpu { surface }) => surface.scripted().map(|surface| {
                let (width, height) = surface.size();
                (
                    u32::try_from(width).unwrap_or(0),
                    u32::try_from(height).unwrap_or(0),
                )
            }),
            _ => None,
        });
        let (width, height) = size?;
        Some(Receipt {
            window: window.0,
            serial: self.capture_present_serial,
            surface: self.presentation_surface,
            width,
            height,
        })
    }

    pub(crate) fn invalidate_resized_presentation(&mut self, window: crate::WindowId) {
        if let Some(held) = self.presentation_lease.as_ref() {
            if let Some(receipt) = self.presentation_receipt(window) {
                let _ = held.blocks(receipt);
            } else {
                held.invalidate("the captured presentation has no current surface");
            }
        }
    }
}

impl crate::App {
    /// Keep event dispatch and model updates running, owing their paint until the
    /// controller finishes. Every release invalidates the raster decision so even
    /// an unchanged frame pays the debt on its next redraw.
    pub(crate) fn presentation_blocked(&mut self, window: crate::WindowId) -> bool {
        let Some(mut state) = self.windows.get_mut(&window) else {
            return false;
        };
        let Some(held) = state.presentation_lease.as_ref() else {
            return false;
        };
        let blocks = match state.presentation_receipt(window) {
            Some(current) => held.blocks(current),
            None => {
                held.invalidate("the captured presentation has no current surface");
                false
            }
        };
        // The installed lease carries the debt. Leaving the ordinary frame-cap
        // clock armed would keep an already-due deadline spinning the event loop.
        state.redraw_pending = !blocks;
        if !blocks {
            state.presentation_lease.take();
            state.invalidate_raster_caches();
            let _ = state.present_retry.on_external_stimulus();
        }
        blocks
    }

    pub(crate) fn install_presentation(
        &mut self,
        window: crate::WindowId,
        lease: &Lease,
        serial: u64,
        width: usize,
        height: usize,
    ) -> Result<Confirmation, String> {
        if self.presentation_blocked(window) {
            return Err("the window already has a held presentation".into());
        }
        let mut state = self
            .windows
            .get_mut(&window)
            .ok_or("the captured window ended")?;
        let receipt = state
            .presentation_receipt(window)
            .ok_or("the capture has no presentation surface")?;
        if receipt.serial != serial
            || usize::try_from(receipt.width).ok() != Some(width)
            || usize::try_from(receipt.height).ok() != Some(height)
        {
            return Err("the capture no longer names the current presentation".into());
        }
        state.presentation_lease = Some(lease.pin(receipt)?);
        let mut confirmation = Confirmation {
            lease: lease.clone(),
            receipt,
            fence: None,
            confirmed: false,
        };
        use crate::present::CpuPresenter as _;
        confirmation.fence = Some(match state.present.as_mut() {
            Some(crate::PresentTarget::Cpu { surface }) => surface.capture_submit_fence()?,
            _ => {
                return Err(
                    "this presentation backend has no native display submission confirmation"
                        .into(),
                );
            }
        });
        Ok(confirmation)
    }

    pub(crate) fn release_presentations(&mut self) {
        let held: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&window, state)| state.presentation_lease.is_some().then_some(window))
            .collect();
        for window in held {
            if !self.presentation_blocked(window)
                && let Some(state) = self.windows.get(&window)
                && let Some(window) = state.os_window.as_ref()
            {
                window.request_redraw();
            }
        }
    }
}

#[cfg(test)]
#[path = "presentation_lease_tests.rs"]
mod tests;
