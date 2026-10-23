// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wayland CPU presenter using double-buffered `wl_shm` memfd pools.

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aterm_types::native_window::{HasNativeWindow, NativeWindow};
use aterm_wayland::globals::Registry;
use aterm_wayland::protocols::wayland::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use aterm_wayland::proxy::{Display, Listener, Proxy, Queue};
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

/// The pool size for a `width` x `height` buffer.
fn pool_size(width: i32, height: i32) -> Option<usize> {
    let bytes = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    let len = bytes.checked_next_power_of_two()?;
    let fits = width.checked_mul(4).is_some() && i32::try_from(len).is_ok();
    fits.then_some(len)
}

/// A rectangle as `damage_buffer` takes it, or `None` past `i32`.
fn wl_rect(rect: &DamageRect) -> Option<(i32, i32, i32, i32)> {
    let fits = |v: u32| i32::try_from(v).ok();
    Some((
        fits(rect.x)?,
        fits(rect.y)?,
        fits(rect.width.get())?,
        fits(rect.height.get())?,
    ))
}

/// A live `wl_buffer` and its release handler.
struct Live {
    buffer: Proxy,
    /// Freed only after `buffer` is destroyed ([`Buffer::destroy_buffer`]).
    listener: Listener,
}

/// One pool, and the `wl_buffer` over it at the current size.
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

/// XRGB8888, `wl_shm.format`'s 1.
const FORMAT: u32 = wl_shm::format::XRGB8888;

impl Buffer {
    fn new(shm: Proxy, width: i32, height: i32) -> Result<Buffer, WaylandPresentError> {
        let size_err = || WaylandPresentError::Size(width.unsigned_abs(), height.unsigned_abs());
        let len = pool_size(width, height).ok_or_else(size_err)?;
        let len_i32 = i32::try_from(len).map_err(|_| size_err())?;
        let mem = ShmMem::new(c"aterm-wayland", len).map_err(WaylandPresentError::Shm)?;
        // SAFETY: `shm` is the presenter's live wl_shm.
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

    /// A `wl_buffer` of `width` x `height` over the pool, with its release handler ([`pool_size`]).
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
        // SAFETY: A buffer this presenter made, on its queue.
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

    /// Destroy the live `wl_buffer`, if there is one, then free its listener.
    fn destroy_buffer(&mut self) {
        if let Some(Live { buffer, listener }) = self.live.take() {
            // SAFETY: The live wl_buffer, taken out of `live`, so destroyed once.
            unsafe { wl_buffer::destroy(buffer) };
            drop(listener);
        }
    }

    /// Hold a `width` x `height` buffer.
    fn resize(&mut self, width: i32, height: i32) -> Result<(), WaylandPresentError> {
        if (self.width, self.height) == (width, height) && self.live.is_some() {
            return Ok(());
        }
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

// SAFETY: Libwayland's calls are thread-safe.
unsafe impl Send for WaylandPresenter {}

impl WaylandPresenter {
    /// Attach to `window`, a Wayland window of winit's.
    pub(crate) fn new(window: Arc<Window>) -> Result<Self, WaylandPresentError> {
        let Some(NativeWindow::Wayland { display, surface }) = window.native_window() else {
            return Err(WaylandPresentError::NotWayland);
        };
        // SAFETY: Winit's live display and surface.
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
        self.size = Some((w, h));
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
        // SAFETY: The wl_shm this presenter bound (version 1 has no release request).
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
