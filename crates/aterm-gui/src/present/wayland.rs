// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Wayland CPU presenter: two `wl_shm` buffers in memfd pools, attached
//! to the window's own `wl_surface` with buffer damage and a commit. It
//! replaces softbuffer 0.4.8's Wayland backend
//! (docs/AUDIT-third-party-2026-10-15.md, linux-windowing LW.6) and keeps its
//! protocol, so the compositor sees what it saw:
//!
//! - its own event queue on winit's display, so the `wl_buffer.release`
//!   events it waits for are dispatched only here;
//! - `wl_shm` at version 1, XRGB8888 (aterm's `0x00RRGGBB` words);
//! - a front and a back buffer: an acquire hands out the back one, waiting
//!   (dispatching the private queue) until the compositor released it; a
//!   present swaps them, attaches, damages and commits;
//! - `damage_buffer` per rectangle on a surface of version 4 and later, the
//!   whole surface (`damage` to `i32::MAX`) before;
//! - a pool grows (`wl_shm_pool.resize` over a grown memfd) to the next
//!   power of two of a new size and never shrinks;
//! - a buffer's age is 1 when it holds the last present, more when older,
//!   and 0 when new.

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aterm_types::native_window::{HasNativeWindow, NativeWindow};
use aterm_wayland::globals::Registry;
use aterm_wayland::protocols::wayland::{wl_buffer, wl_shm, wl_shm_pool, wl_surface};
use aterm_wayland::proxy::{Display, Listener, Proxy, Queue};
use winit::window::Window;

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

/// The pool size for a `width` x `height` buffer: its bytes, rounded up to a
/// power of two (softbuffer's `get_pool_size`). `None` past what a pool's
/// `i32` size or a row's `i32` stride can say.
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
    /// The `wl_buffer` at `width` x `height`. `None` between a destroy and
    /// the next buffer made, so a resize that failed half way leaves nothing
    /// to attach or destroy twice, and the next resize makes one.
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

    /// A `wl_buffer` of `width` x `height` over the pool, with its release
    /// handler; the size is the buffer's only once it is made. The pool holds
    /// it (the caller grew the pool to [`pool_size`] first).
    fn make_buffer(&mut self, width: i32, height: i32) -> Result<(), WaylandPresentError> {
        let stride = width.checked_mul(4).ok_or(WaylandPresentError::Size(
            width.unsigned_abs(),
            height.unsigned_abs(),
        ))?;
        // SAFETY: the live pool; the arguments lie inside it.
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
                // SAFETY: the buffer made above, without a listener, destroyed
                // once.
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
            // SAFETY: the live wl_buffer, taken out of `live`, so destroyed
            // once; its listener goes after.
            unsafe { wl_buffer::destroy(buffer) };
            drop(listener);
        }
    }

    /// Hold a `width` x `height` buffer. The steps that can fail on this side
    /// (the size, growing the file and its mapping) come before the old
    /// buffer is destroyed, so a failure leaves the buffer and the pool as
    /// they were and the next frame retries; from the destroy on, the
    /// requests are softbuffer's, in its order.
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
            // SAFETY: the live pool, grown to the file's new size.
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
        // SAFETY: the live pool, destroyed once (its buffers are gone).
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
    /// Holds the winit window, and with it the display and the surface;
    /// dropped last.
    _window: Arc<Window>,
}

// SAFETY: libwayland's calls are thread-safe; the presenter moves between
// threads only with its window state.
unsafe impl Send for WaylandPresenter {}

impl WaylandPresenter {
    /// Attach to `window`, a Wayland window of winit's.
    pub(crate) fn new(window: Arc<Window>) -> Result<Self, WaylandPresentError> {
        let Some(NativeWindow::Wayland { display, surface }) = window.native_window() else {
            return Err(WaylandPresentError::NotWayland);
        };
        // SAFETY: winit's live display and surface, which `_window` keeps
        // alive for as long as this presenter lives.
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

    /// The back buffer, once the compositor has released it, at the current
    /// size.
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
        // Every rectangle is checked before anything is sent, so a refused
        // present leaves the buffers and the surface as they were.
        if let Some(bad) = damage.iter().find(|r| wl_rect(r).is_none()) {
            return Err(WaylandPresentError::Damage(*bad));
        }
        // Releases already read, applied before the swap.
        self.queue.dispatch_pending()?;
        let Some((front, back)) = &mut self.buffers else {
            return Ok(());
        };
        // `buffer_mut` hands a frame out only over a live buffer.
        let Some(attach) = back.live.as_ref().map(|l| l.buffer) else {
            return Err(aterm_wayland::proxy::Error::NoObject.into());
        };
        std::mem::swap(front, back);
        front.age = 1;
        if back.age != 0 {
            back.age = back.age.saturating_add(1);
        }
        front.released.store(false, Ordering::SeqCst);
        // SAFETY: winit's live surface and this presenter's live buffer.
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
        // A flush that could not write everything (EAGAIN: the compositor has
        // not drained the socket) keeps the rest in libwayland for the next
        // flush, winit's or this presenter's; the commit is sent, so only a
        // dead display fails the present.
        match self.display.flush() {
            Err(e) if self.display.error().is_some() => Err(e.into()),
            _ => Ok(()),
        }
    }
}

impl Drop for WaylandPresenter {
    fn drop(&mut self) {
        self.buffers = None;
        // SAFETY: the wl_shm this presenter bound (version 1 has no release
        // request), destroyed once, before its queue.
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
