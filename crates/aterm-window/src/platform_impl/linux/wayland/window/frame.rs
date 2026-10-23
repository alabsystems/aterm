// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Synchronized `wl_subsurface` shadow border parts around a client-decorated Wayland window.

use std::io::Write as _;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aterm_wayland::protocols::wayland::{
    wl_buffer, wl_compositor, wl_region, wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface,
    wl_surface,
};
use aterm_wayland::proxy::Proxy;
use tracing::warn;

use crate::frame_shadow::{self, Look, Part, Shadows};
use crate::platform_impl::wayland::wire::Owned;
use crate::platform_impl::wayland::{make_wid, WindowId};

/// How many replaced buffers wait for their release at most.
const MAX_RETIRED: usize = 16;

/// The globals a frame makes its parts and buffers from.
#[derive(Clone)]
pub(crate) struct FrameGlobals {
    pub(crate) compositor: Arc<Owned>,
    pub(crate) subcompositor: Arc<Owned>,
    pub(crate) shm: Arc<Owned>,
}

/// A buffer a part showed or shows, and whether the compositor has released it.
struct Buffer {
    owned: Owned,
    released: Arc<AtomicBool>,
}

/// One border part: its subsurface role (destroyed first), its surface, and the buffer it shows.
struct PartSurface {
    subsurface: Owned,
    surface: Owned,
    buffer: Option<Buffer>,
}

/// The four border parts around one window.
pub(crate) struct ShadowFrame {
    parts: [PartSurface; 4],
    /// Buffers a part showed before its current one, until the compositor releases each.
    retired: Vec<Buffer>,
    /// What the parts show; `None` while hidden.
    shown: Option<Look>,
    /// The shadow as rendered for each scale and focus shown so far.
    shadows: Shadows,
    globals: FrameGlobals,
}

impl ShadowFrame {
    /// Creates the four border subsurfaces of `parent` in hidden state.
    pub(crate) fn new(globals: &FrameGlobals, parent: Proxy) -> Option<ShadowFrame> {
        let make = || -> Option<PartSurface> {
            // SAFETY: the live compositor; the surface is this part's.
            let surface = unsafe { wl_compositor::create_surface(globals.compositor.proxy()) }?;
            // SAFETY: a surface made here, owned by the part; its events have no listener.
            let surface = unsafe { Owned::new(surface, wl_surface::destroy) };
            // SAFETY: the live subcompositor, the role-less surface made above and the window's
            // live surface.
            let subsurface = unsafe {
                wl_subcompositor::get_subsurface(
                    globals.subcompositor.proxy(),
                    surface.proxy(),
                    parent,
                )
            }?;
            // SAFETY: a subsurface made here, owned by the part, destroyed before its surface.
            let subsurface = unsafe { Owned::new(subsurface, wl_subsurface::destroy) };
            Some(PartSurface { subsurface, surface, buffer: None })
        };
        Some(ShadowFrame {
            parts: [make()?, make()?, make()?, make()?],
            retired: Vec::new(),
            shown: None,
            shadows: Shadows::default(),
            globals: globals.clone(),
        })
    }

    /// The part whose surface is `surface`, if one of this frame's is.
    pub(crate) fn part_of(&self, surface: WindowId) -> Option<Part> {
        Part::ALL
            .into_iter()
            .find(|part| make_wid(self.parts[part.index()].surface.proxy()) == surface)
    }

    /// What the parts show; `None` while hidden.
    pub(crate) fn shown(&self) -> Option<Look> {
        self.shown
    }

    /// Show `look` (`None`: hide the parts). False when the parts already show it.
    pub(crate) fn show(&mut self, look: Option<Look>) -> bool {
        if self.shown == look {
            return false;
        }
        self.retired.retain(|buffer| !buffer.released.load(Ordering::Relaxed));
        let excess = self.retired.len().saturating_sub(MAX_RETIRED);
        self.retired.drain(..excess);
        match look {
            None => {
                for part in &mut self.parts {
                    let surface = part.surface.proxy();
                    // SAFETY: the part's live surface: no buffer, so no pixels and no input.
                    unsafe {
                        wl_surface::attach(surface, None, 0, 0);
                        wl_surface::commit(surface);
                    }
                    self.retired.extend(part.buffer.take());
                }
            },
            Some(look) => {
                for part in Part::ALL {
                    if let Err(_why) = self.draw(part, look) {
                        warn!("wayland: the frame's {part:?} part was not drawn: {_why}");
                    }
                }
            },
        }
        self.shown = look;
        true
    }

    /// Draw `part` as `look` has it, placed beside the window with its input region.
    fn draw(&mut self, part: Part, look: Look) -> Result<(), String> {
        let pixmap = self.shadows.paint(part, look);
        let (width, height) = (pixmap.width, pixmap.height);
        let buffer = shm_buffer(&self.globals.shm, &pixmap.argb8888(), width, height)?;
        let rect = frame_shadow::surface_rect(part, look.width, look.height);
        let input = frame_shadow::input_rect(part, look.width, look.height);
        let objects = &mut self.parts[part.index()];
        let surface = objects.surface.proxy();
        let scale = i32::try_from(look.scale).unwrap_or(1);
        // SAFETY: the part's live subsurface and surface, the compositor, and the buffer made
        // above, which the part keeps until the compositor releases it.
        unsafe {
            wl_subsurface::set_position(objects.subsurface.proxy(), rect.x, rect.y);
            if surface.version() >= 3 {
                wl_surface::set_buffer_scale(surface, scale);
            }
            wl_surface::attach(surface, Some(buffer.owned.proxy()), 0, 0);
            if surface.version() >= 4 {
                wl_surface::damage_buffer(surface, 0, 0, i32::MAX, i32::MAX);
            } else {
                wl_surface::damage(surface, 0, 0, i32::MAX, i32::MAX);
            }
            if let Some(region) = wl_compositor::create_region(self.globals.compositor.proxy()) {
                let (w, h) = (
                    i32::try_from(input.width).unwrap_or(i32::MAX),
                    i32::try_from(input.height).unwrap_or(i32::MAX),
                );
                wl_region::add(region, input.x, input.y, w, h);
                wl_surface::set_input_region(surface, Some(region));
                wl_region::destroy(region);
            }
            wl_surface::commit(surface);
        }
        self.retired.extend(objects.buffer.replace(buffer));
        Ok(())
    }
}

/// A `width`×`height` ARGB8888 `wl_buffer` holding `bytes`, in a pool of its own (the pool goes at once; the buffer keeps its memory), with its `release` heard.
fn shm_buffer(shm: &Owned, bytes: &[u8], width: usize, height: usize) -> Result<Buffer, String> {
    let len = i32::try_from(bytes.len()).map_err(|_| "a buffer over 2 GiB".to_string())?;
    let (w, h, stride) = (
        i32::try_from(width).map_err(|e| e.to_string())?,
        i32::try_from(height).map_err(|e| e.to_string())?,
        i32::try_from(width * 4).map_err(|e| e.to_string())?,
    );
    if len == 0 {
        return Err("an empty buffer".into());
    }
    // SAFETY: memfd_create with a NUL-terminated name; a non-negative return is a descriptor
    // this process owns alone.
    let fd = unsafe { libc::memfd_create(c"aterm-frame".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("memfd_create: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: the descriptor made above, owned here alone.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes).map_err(|e| format!("the buffer's memory: {e}"))?;
    // SAFETY: the live wl_shm and a descriptor of `len` bytes (libwayland sends a copy of it).
    let pool = unsafe { wl_shm::create_pool(shm.proxy(), file.as_raw_fd(), len) }
        .ok_or("wl_shm.create_pool failed")?;
    // SAFETY: the pool made above, destroyed once here after its one buffer (the buffer keeps
    // the pool's memory).
    let buffer = unsafe {
        let buffer = wl_shm_pool::create_buffer(pool, 0, w, h, stride, wl_shm::format::ARGB8888);
        wl_shm_pool::destroy(pool);
        buffer
    }
    .ok_or("wl_shm_pool.create_buffer failed")?;
    let released = Arc::new(AtomicBool::new(false));
    let heard = Arc::clone(&released);
    // SAFETY: the buffer made above, owned here alone, with no listener yet; its `release`
    // carries no descriptor.
    let owned = unsafe {
        Owned::handled(buffer, wl_buffer::destroy, move |_, opcode, _| {
            if opcode == wl_buffer::evt::RELEASE {
                heard.store(true, Ordering::Relaxed);
            }
        })
    };
    Ok(Buffer { owned, released })
}
