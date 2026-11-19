// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wayland CPU presenter using double-buffered `wl_shm` memfd pools.

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::time::Instant;

use aterm_types::native_window::{HasNativeWindow, NativeWindow};
use aterm_wayland::globals::Registry;
use aterm_wayland::protocols::wayland::{
    wl_buffer, wl_callback, wl_display, wl_shm, wl_shm_pool, wl_surface,
};
use aterm_wayland::proxy::{Display, Flushed, Listener, Proxy, Queue};
use aterm_window::window::Window;

use super::shm_mem::ShmMem;
use super::{CpuFrameBuffer, DamageRect};

/// Why the Wayland presenter failed.
#[derive(Debug)]
pub(crate) enum WaylandPresentError {
    /// The window is not a Wayland window.
    NotWayland,
    /// The compositor offers no `wl_shm`.
    NoShm,
    /// A size past `i32`.
    Size(u32, u32),
    /// A damage rectangle past `i32` (softbuffer's `DamageOutOfRange`).
    Damage(DamageRect),
    /// libwayland or the display refused.
    Wayland(aterm_wayland::proxy::Error),
    /// The shared memory could not be made.
    Shm(std::io::Error),
}

impl std::fmt::Display for WaylandPresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWayland => f.write_str("not a Wayland window"),
            Self::NoShm => f.write_str("the compositor offers no wl_shm"),
            Self::Size(w, h) => write!(f, "{w}x{h} is past Wayland's sizes"),
            Self::Damage(r) => write!(
                f,
                "damage {}x{} at {},{} is past Wayland's coordinates",
                r.width, r.height, r.x, r.y
            ),
            Self::Wayland(e) => write!(f, "{e}"),
            Self::Shm(e) => write!(f, "shared memory: {e}"),
        }
    }
}

impl From<aterm_wayland::proxy::Error> for WaylandPresentError {
    fn from(e: aterm_wayland::proxy::Error) -> Self {
        Self::Wayland(e)
    }
}

fn pool_size(width: i32, height: i32) -> Option<usize> {
    let bytes = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    let len = bytes.checked_next_power_of_two()?;
    let fits = width.checked_mul(4).is_some() && i32::try_from(len).is_ok();
    fits.then_some(len)
}

fn wl_rect(rect: &DamageRect) -> Option<(i32, i32, i32, i32)> {
    let fits = |v: u32| i32::try_from(v).ok();
    Some((
        fits(rect.x)?,
        fits(rect.y)?,
        fits(rect.width.get())?,
        fits(rect.height.get())?,
    ))
}

struct Live {
    buffer: Proxy,
    /// Freed only after `buffer` is destroyed ([`Buffer::destroy_buffer`]).
    listener: Listener,
}

struct Buffer {
    mem: ShmMem,
    pool: Proxy,
    /// The `wl_buffer` at `width` x `height`.
    live: Option<Live>,
    width: i32,
    height: i32,
    released: Arc<AtomicBool>,
    age: u8,
}

const FORMAT: u32 = wl_shm::format::XRGB8888;

/// A same-connection processing barrier, placed after the captured surface commit.
/// Only this value dispatches its queue, including while its callback is retired.
struct SubmitSync<Owner> {
    display: Display,
    callback: Option<Proxy>,
    listener: Option<Listener>,
    queue: Queue,
    done: Arc<AtomicBool>,
    // Dropped after the callback, listener and queue: the borrowed display stays live.
    _owner: Owner,
}

impl<Owner> SubmitSync<Owner> {
    /// # Safety
    /// `owner` keeps `display` live through its own drop. All requests for the captured
    /// present were marshalled before this call; no later present may replace it while
    /// this barrier is pending.
    unsafe fn after_submit(display: Display, owner: Owner) -> Result<Self, String> {
        let queue = Queue::new(display).map_err(|e| e.to_string())?;
        let wrapper = queue.display_wrapper().map_err(|e| e.to_string())?;
        // SAFETY: the wrapper borrows the live display and assigns this new callback to
        // the private queue, which is dispatched only by this value.
        let callback = unsafe { wl_display::sync(wrapper.proxy()) }
            .ok_or("Wayland could not create the capture's processing callback")?;
        let done = Arc::new(AtomicBool::new(false));
        let heard = Arc::clone(&done);
        // SAFETY: this newly created callback has no other listener. It and the listener
        // stay together; Drop destroys the proxy before freeing the listener's closure.
        let listener = unsafe {
            callback.listen(move |_, opcode, args| {
                if opcode == wl_callback::evt::DONE
                    && matches!(args, [aterm_wayland::proxy::EventArg::Uint(_)])
                {
                    heard.store(true, Ordering::Release);
                }
            })
        };
        let listener = match listener {
            Ok(listener) => listener,
            Err(error) => {
                // SAFETY: no dispatcher can run on this undispatched private queue.
                unsafe { callback.destroy() };
                return Err(error.to_string());
            }
        };
        Ok(Self {
            display,
            callback: Some(callback),
            listener: Some(listener),
            queue,
            done,
            _owner: owner,
        })
    }

    fn poll_once(
        &mut self,
        timeout: i32,
        stopped: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<bool, String> {
        if stopped() {
            return Err("the Wayland capture was cancelled before submission was confirmed".into());
        }
        self.queue.dispatch_pending().map_err(|e| e.to_string())?;
        if self.done.load(Ordering::Acquire) {
            return if stopped() {
                Err("the Wayland capture was cancelled".into())
            } else {
                Ok(true)
            };
        }
        let Some(reading) = self.queue.prepare_read().map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let queued = self.display.try_flush().map_err(|e| e.to_string())? == Flushed::Queued;
        let mut fd = libc::pollfd {
            fd: self.display.fd().map_err(|e| e.to_string())?,
            events: libc::POLLIN | if queued { libc::POLLOUT } else { 0 },
            revents: 0,
        };
        // SAFETY: one initialized pollfd outlives the bounded call. Its descriptor belongs
        // to the display, kept live by _owner; polling takes no ownership of it.
        let polled = unsafe { libc::poll(&mut fd, 1, timeout) };
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(format!("Wayland capture readiness: {error}"));
        }
        if stopped() {
            return Err("the Wayland capture was cancelled".into());
        }
        if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(
                "the Wayland compositor connection ended before capture confirmation".into(),
            );
        }
        if fd.revents & libc::POLLIN != 0 {
            reading.read().map_err(|e| e.to_string())?;
        } else {
            // A writable socket only permits another flush; it proves no server processing.
            drop(reading);
        }
        self.queue.dispatch_pending().map_err(|e| e.to_string())?;
        if stopped() {
            return Err("the Wayland capture was cancelled".into());
        }
        Ok(self.done.load(Ordering::Acquire))
    }
}

impl<Owner> Drop for SubmitSync<Owner> {
    fn drop(&mut self) {
        if let Some(callback) = self.callback.take() {
            // SAFETY: this value alone dispatches the private queue. No callback can be
            // executing concurrently, and destroying the proxy retires queued events.
            unsafe { callback.destroy() };
        }
        self.listener.take();
    }
}

impl<Owner: Send + 'static> super::SubmitFence for SubmitSync<Owner> {
    fn wait(mut self: Box<Self>, stopped: &(dyn Fn() -> bool + Send + Sync)) -> Result<(), String> {
        if stopped() {
            return Err("the Wayland capture was cancelled".into());
        }
        let mut progress =
            super::linux::SubmitProgress::new(self.display.fd().map_err(|e| e.to_string())?)?;
        loop {
            if self.poll_once(100, stopped)? {
                return Ok(());
            }
            progress.check()?;
        }
    }
}

impl Buffer {
    fn new(shm: Proxy, width: i32, height: i32) -> Result<Buffer, WaylandPresentError> {
        let size_err = || WaylandPresentError::Size(width.unsigned_abs(), height.unsigned_abs());
        let len = pool_size(width, height).ok_or_else(size_err)?;
        let len_i32 = i32::try_from(len).map_err(|_| size_err())?;
        let mem = ShmMem::new(c"aterm-wayland", len).map_err(WaylandPresentError::Shm)?;
        // SAFETY: `shm` is the presenter's live wl_shm; the descriptor is
        // duplicated by libwayland for the send.
        let pool = unsafe { wl_shm::create_pool(shm, mem.fd().as_raw_fd(), len_i32) }
            .ok_or(aterm_wayland::proxy::Error::NoObject)?;
        let mut b = Buffer {
            mem,
            pool,
            live: None,
            width,
            height,
            released: Arc::new(AtomicBool::new(true)),
            age: 0,
        };
        b.make_buffer(width, height)?;
        Ok(b)
    }

    fn make_buffer(&mut self, width: i32, height: i32) -> Result<(), WaylandPresentError> {
        let stride = width.checked_mul(4).ok_or(WaylandPresentError::Size(
            width.unsigned_abs(),
            height.unsigned_abs(),
        ))?;
        // SAFETY: The live pool; the arguments lie inside it.
        let buffer =
            unsafe { wl_shm_pool::create_buffer(self.pool, 0, width, height, stride, FORMAT) }
                .ok_or(aterm_wayland::proxy::Error::NoObject)?;
        let released = Arc::clone(&self.released);
        // SAFETY: a buffer this presenter made, on its queue; the listener is
        // kept beside it and freed only after the buffer is destroyed.
        let listener = unsafe {
            buffer.listen(move |_, opcode, _| {
                if opcode == wl_buffer::evt::RELEASE {
                    released.store(true, Ordering::SeqCst);
                }
            })
        };
        let listener = match listener {
            Ok(l) => l,
            Err(e) => {
                // SAFETY: The buffer made above, without a listener, destroyed once.
                unsafe { wl_buffer::destroy(buffer) };
                return Err(e.into());
            }
        };
        self.live = Some(Live { buffer, listener });
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn destroy_buffer(&mut self) {
        if let Some(Live { buffer, listener }) = self.live.take() {
            // SAFETY: the live wl_buffer, taken out of `live`, so destroyed
            // once; its listener goes after.
            unsafe { wl_buffer::destroy(buffer) };
            drop(listener);
        }
    }

    fn resize(&mut self, width: i32, height: i32) -> Result<(), WaylandPresentError> {
        if (self.width, self.height) == (width, height) && self.live.is_some() {
            return Ok(());
        }
        self.age = 0;
        let size_err = || WaylandPresentError::Size(width.unsigned_abs(), height.unsigned_abs());
        let len = pool_size(width, height).ok_or_else(size_err)?;
        let len_i32 = i32::try_from(len).map_err(|_| size_err())?;
        let grow = len > self.mem.len();
        if grow {
            self.mem.set_len(len).map_err(WaylandPresentError::Shm)?;
        }
        self.destroy_buffer();
        if grow {
            // SAFETY: The live pool, grown to the file's new size.
            unsafe { wl_shm_pool::resize(self.pool, len_i32) };
        }
        self.make_buffer(width, height)
    }

    fn words(&self) -> usize {
        usize::try_from(self.width).unwrap_or(0) * usize::try_from(self.height).unwrap_or(0)
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.destroy_buffer();
        // SAFETY: The live pool, destroyed once (its buffers are gone).
        unsafe { wl_shm_pool::destroy(self.pool) };
    }
}

/// One window's Wayland presenter.
pub(crate) struct WaylandPresenter {
    display: Display,
    surface: Proxy,
    buffers: Option<(Buffer, Buffer)>,
    size: Option<(i32, i32)>,
    shm: Proxy,
    /// Holds the globals the presenter bound from; destroyed before the queue.
    registry: Option<Registry>,
    /// Every object above is on this queue; dropped after them.
    queue: Queue,
    /// Holds the winit window, and with it the display and the surface; dropped last.
    _window: Arc<Window>,
}

// SAFETY: libwayland's calls are thread-safe; the presenter moves between
// threads only with its window state.
unsafe impl Send for WaylandPresenter {}

impl WaylandPresenter {
    /// Fence only a capture's submitted frame, on the surface's existing connection.
    /// The owning window keeps that connection alive while the worker waits off the UI thread.
    pub(crate) fn capture_submit_fence(&mut self) -> Result<Box<dyn super::SubmitFence>, String> {
        // SAFETY: the retained window owns this borrowed display, and the capture path pins
        // its exact submitted presentation before asking for this following sync callback.
        let fence = unsafe { SubmitSync::after_submit(self.display, Arc::clone(&self._window)) }?;
        Ok(Box::new(fence))
    }
    /// Attach to `window`, a Wayland window of winit's.
    pub(crate) fn new(window: Arc<Window>) -> Result<Self, WaylandPresentError> {
        let Some(NativeWindow::Wayland { display, surface }) = window.native_window() else {
            return Err(WaylandPresentError::NotWayland);
        };
        // SAFETY: winit's live display and surface, which `_window` keeps alive for as long as this
        // presenter lives.
        let (display, surface) = unsafe { (Display::from_ptr(display), Proxy::from_ptr(surface)) };
        let queue = Queue::new(display)?;
        let registry = Registry::new(&queue)?;
        let global = registry.find(c"wl_shm").ok_or(WaylandPresentError::NoShm)?;
        let shm = registry
            .bind(&global, &wl_shm::INTERFACE, 1)
            .ok_or(aterm_wayland::proxy::Error::NoObject)?;
        Ok(Self {
            display,
            surface,
            buffers: None,
            size: None,
            shm,
            registry: Some(registry),
            queue,
            _window: window,
        })
    }

    /// Set the frame size; the buffers follow on the next acquire.
    pub(crate) fn resize(
        &mut self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<(), WaylandPresentError> {
        let size_err = || WaylandPresentError::Size(width.get(), height.get());
        let w = i32::try_from(width.get()).map_err(|_| size_err())?;
        let h = i32::try_from(height.get()).map_err(|_| size_err())?;
        if self.size != Some((w, h)) {
            if let Some((front, back)) = &mut self.buffers {
                front.age = 0;
                back.age = 0;
            }
            self.size = Some((w, h));
        }
        Ok(())
    }

    /// The back buffer, once the compositor has released it, at the current size.
    pub(crate) fn buffer_mut(&mut self) -> Result<WaylandFrame<'_>, WaylandPresentError> {
        let Some((w, h)) = self.size else {
            return Err(WaylandPresentError::Size(0, 0));
        };
        match &mut self.buffers {
            Some((_, back)) => {
                while !back.released.load(Ordering::SeqCst) {
                    self.queue.dispatch()?;
                }
                back.resize(w, h)?;
            }
            None => {
                self.buffers = Some((Buffer::new(self.shm, w, h)?, Buffer::new(self.shm, w, h)?));
            }
        }
        Ok(WaylandFrame { p: self })
    }

    fn present(&mut self, damage: &[DamageRect]) -> Result<(), WaylandPresentError> {
        if let Some(bad) = damage.iter().find(|r| wl_rect(r).is_none()) {
            return Err(WaylandPresentError::Damage(*bad));
        }
        self.queue.dispatch_pending()?;
        let Some((front, back)) = &mut self.buffers else {
            return Ok(());
        };
        let Some(attach) = back.live.as_ref().map(|l| l.buffer) else {
            return Err(aterm_wayland::proxy::Error::NoObject.into());
        };
        std::mem::swap(front, back);
        front.age = 1;
        if back.age != 0 {
            back.age = back.age.saturating_add(1);
        }
        front.released.store(false, Ordering::SeqCst);
        // SAFETY: Winit's live surface and this presenter's live buffer.
        unsafe {
            wl_surface::attach(self.surface, Some(attach), 0, 0);
            if self.surface.version() < 4 {
                wl_surface::damage(self.surface, 0, 0, i32::MAX, i32::MAX);
            } else {
                for (x, y, w, h) in damage.iter().filter_map(wl_rect) {
                    wl_surface::damage_buffer(self.surface, x, y, w, h);
                }
            }
            wl_surface::commit(self.surface);
        }
        match self.display.flush() {
            Err(e) if self.display.error().is_some() => Err(e.into()),
            _ => Ok(()),
        }
    }
}

impl Drop for WaylandPresenter {
    fn drop(&mut self) {
        self.buffers = None;
        // SAFETY: The wl_shm this presenter bound, destroyed once (version 1 has no release request).
        unsafe { self.shm.destroy() };
        self.registry = None;
        let _ = self.display.flush();
    }
}

/// One acquired Wayland frame: the back buffer.
pub(crate) struct WaylandFrame<'a> {
    p: &'a mut WaylandPresenter,
}

impl Deref for WaylandFrame<'_> {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        match &self.p.buffers {
            Some((_, back)) => back.mem.words(0, back.words()).unwrap_or_default(),
            None => &[],
        }
    }
}

impl DerefMut for WaylandFrame<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        match &mut self.p.buffers {
            Some((_, back)) => {
                let n = back.words();
                back.mem.words_mut(0, n).unwrap_or_default()
            }
            None => &mut [],
        }
    }
}

impl CpuFrameBuffer for WaylandFrame<'_> {
    type Error = WaylandPresentError;

    fn age(&self) -> u8 {
        self.p.buffers.as_ref().map_or(0, |(_, back)| back.age)
    }

    fn present(self) -> Result<(), Self::Error> {
        let Some((w, h)) = self.p.size else {
            return Ok(());
        };
        let whole = DamageRect {
            x: 0,
            y: 0,
            width: NonZeroU32::new(w.unsigned_abs()).unwrap_or(NonZeroU32::MIN),
            height: NonZeroU32::new(h.unsigned_abs()).unwrap_or(NonZeroU32::MIN),
        };
        self.p.present(&[whole])
    }

    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error> {
        self.p.present(damage)
    }
}

#[cfg(test)]
#[path = "wayland_tests.rs"]
mod tests;
