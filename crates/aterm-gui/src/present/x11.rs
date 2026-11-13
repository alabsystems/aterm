// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! X11 CPU presenter using MIT-SHM `AttachFd`/`ShmPutImage` with `PutImage` fallback.

use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use aterm_types::native_window::{HasNativeWindow, NativeWindow};
use aterm_window::platform::x11::WindowExtX11;
use aterm_window::window::Window;
use aterm_x11::Connection;
use aterm_x11::proto::xproto::{
    self, GetInputFocusReply, Setup, gc, image_format, image_order, visual_class,
};
use aterm_x11::proto::{ValueList, shm};

use super::shm_mem::ShmMem;
use super::{CpuFrameBuffer, DamageRect};

/// Why the X11 presenter failed.
#[derive(Debug)]
pub(crate) enum X11PresentError {
    /// The window is not an X11 window, or its handle is incomplete.
    NotX11,
    /// The window's visual does not store pixels as `0x00RRGGBB` words.
    Visual(u32),
    /// A size past X11's 16-bit coordinates.
    Size(u32, u32),
    /// A damage rectangle past X11's 16-bit coordinates (softbuffer's `DamageOutOfRange`).
    Damage(DamageRect),
    /// The transport or the server refused.
    X11(aterm_x11::Error),
    /// The shared-memory segment could not be made.
    Shm(std::io::Error),
}

impl std::fmt::Display for X11PresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotX11 => f.write_str("not an X11 window"),
            Self::Visual(v) => write!(
                f,
                "visual {v:#x} does not store 0x00RRGGBB words and is unsupported"
            ),
            Self::Size(w, h) => write!(f, "{w}x{h} is past X11's 16-bit sizes"),
            Self::Damage(r) => write!(
                f,
                "damage {}x{} at {},{} is past X11's 16-bit coordinates",
                r.width, r.height, r.x, r.y
            ),
            Self::X11(e) => write!(f, "{e}"),
            Self::Shm(e) => write!(f, "shared memory: {e}"),
        }
    }
}

impl From<aterm_x11::Error> for X11PresentError {
    fn from(e: aterm_x11::Error) -> Self {
        Self::X11(e)
    }
}

/// The segment attached to the server, and its id there.
struct Segment {
    mem: ShmMem,
    id: shm::Seg,
}

enum Store {
    /// MIT-SHM, at the extension's major opcode.
    Shm {
        major: u8,
        seg: Option<Segment>,
        pending: Option<u64>,
    },
    /// No MIT-SHM: the pixels go on the wire.
    Wire(Vec<u32>),
}

/// One window's X11 presenter.
pub(crate) struct X11Presenter {
    conn: Connection,
    window: u32,
    gc: u32,
    depth: u8,
    store: Store,
    size: Option<(u16, u16)>,
    presented: bool,
    /// The winit window, which holds the Xlib display `conn` borrows (so it is dropped last) and
    /// takes the note of each post of the whole frame (`WindowExtX11::x11_area_posted`).
    owner: Arc<Window>,
}

// SAFETY: the connection is libxcb's, which is thread-safe; the segment is
// owned. The presenter moves between threads only with its window state.
unsafe impl Send for X11Presenter {}

/// Pixel layouts supported by the X11 presenter.
fn visual_supported(setup: &Setup, visual: u32, window_depth: u8) -> bool {
    let pixels_are_words = setup
        .pixmap_formats
        .iter()
        .any(|f| f.depth == window_depth && f.bits_per_pixel == 32);
    if !matches!(window_depth, 24 | 32) || !pixels_are_words {
        return false;
    }
    let native = if cfg!(target_endian = "little") {
        image_order::LSB_FIRST
    } else {
        image_order::MSB_FIRST
    };
    let masks = if setup.image_byte_order == native {
        (0x00ff_0000, 0x0000_ff00, 0x0000_00ff)
    } else {
        (0x0000_ff00, 0x00ff_0000, 0xff00_0000)
    };
    setup.roots.iter().any(|screen| {
        screen
            .allowed_depths
            .iter()
            .filter(|d| d.depth == window_depth)
            .flat_map(|d| &d.visuals)
            .any(|v| {
                v.visual_id == visual
                    && matches!(
                        v.class,
                        visual_class::TRUE_COLOR | visual_class::DIRECT_COLOR
                    )
                    && (v.red_mask, v.green_mask, v.blue_mask) == masks
            })
    })
}

/// Converts a damage rectangle into 16-bit `ShmPutImage` coordinates, or `None` on overflow.
fn shm_rect(rect: &DamageRect) -> Option<(u16, u16, u16, u16, i16, i16)> {
    let fits = |v: u32| u16::try_from(v).ok();
    let (x, y) = (fits(rect.x)?, fits(rect.y)?);
    Some((
        x,
        y,
        fits(rect.width.get())?,
        fits(rect.height.get())?,
        i16::try_from(x).ok()?,
        i16::try_from(y).ok()?,
    ))
}

/// Whether the server takes AttachFd.
fn shm_major(conn: &Connection) -> Option<u8> {
    let ext = conn.extension(shm::NAME).ok()??;
    let version = conn
        .send(shm::query_version(ext.major_opcode))
        .ok()?
        .reply()
        .ok()?;
    if (version.major_version, version.minor_version) < (1, 2) {
        return None;
    }
    let mem = ShmMem::new(c"aterm-x11-probe", 4096).ok()?;
    let fd = mem.fd().try_clone_to_owned().ok()?;
    let id = conn.generate_id().ok()?;
    let attach = conn
        .send_void_with_fds(shm::attach_fd(ext.major_opcode, id, true), vec![fd])
        .ok()?;
    let detach = conn.send_void(shm::detach(ext.major_opcode, id)).ok()?;
    (attach.check().is_ok() && detach.check().is_ok()).then_some(ext.major_opcode)
}

impl X11Presenter {
    /// Attach to `window`, an X11 window of winit's.
    pub(crate) fn new(window: Arc<Window>) -> Result<Self, X11PresentError> {
        let Some(NativeWindow::Xlib {
            display,
            window: xid,
            visual,
            ..
        }) = window.native_window()
        else {
            return Err(X11PresentError::NotX11);
        };
        if xid == 0 {
            return Err(X11PresentError::NotX11);
        }
        // SAFETY: Winit's live `Display *`, which `owner` keeps open for as long as this
        // presenter (and so this connection) lives.
        let conn = unsafe { Connection::from_xlib_display(display) }?;
        let geometry = conn.send(xproto::get_geometry(xid))?;
        let visual = match (visual != 0).then_some(visual) {
            Some(v) => v,
            None => {
                conn.send(xproto::get_window_attributes(xid))?
                    .reply()?
                    .visual
            }
        };
        let depth = geometry.reply()?.depth;
        if !visual_supported(conn.setup(), visual, depth) {
            return Err(X11PresentError::Visual(visual));
        }
        let gc_id = conn.generate_id()?;
        conn.send_void(xproto::create_gc(
            gc_id,
            xid,
            &ValueList::new().set(gc::GRAPHICS_EXPOSURES, 0),
        ))?
        .check()?;
        let store = match shm_major(&conn) {
            Some(major) => Store::Shm {
                major,
                seg: None,
                pending: None,
            },
            None => {
                aterm_log::warn!("x11 present: no MIT-SHM 1.2; frames go over the wire");
                Store::Wire(Vec::new())
            }
        };
        Ok(Self {
            conn,
            window: xid,
            gc: gc_id,
            depth,
            store,
            size: None,
            presented: false,
            owner: window,
        })
    }

    /// Set the frame size; the store grows (never shrinks) to hold it.
    pub(crate) fn resize(
        &mut self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<(), X11PresentError> {
        let size_err = || X11PresentError::Size(width.get(), height.get());
        let w = u16::try_from(width.get()).map_err(|_| size_err())?;
        let h = u16::try_from(height.get()).map_err(|_| size_err())?;
        if self.size == Some((w, h)) {
            return Ok(());
        }
        self.presented = false;
        let words = usize::from(w) * usize::from(h);
        match &mut self.store {
            Store::Wire(buf) => buf.resize(words, 0),
            Store::Shm {
                major,
                seg,
                pending,
            } => {
                let want = (words * 4).next_power_of_two();
                if seg.as_ref().is_none_or(|s| s.mem.len() < want) {
                    let mem = ShmMem::new(c"aterm-x11", want).map_err(X11PresentError::Shm)?;
                    let fd = mem
                        .fd()
                        .try_clone_to_owned()
                        .map_err(X11PresentError::Shm)?;
                    let id = self.conn.generate_id()?;
                    drop(
                        self.conn
                            .send_void_with_fds(shm::attach_fd(*major, id, true), vec![fd])?,
                    );
                    if let Some(old) = seg.replace(Segment { mem, id }) {
                        // The server may still be reading the old segment.
                        if let Some(seq) = pending.take() {
                            self.conn.wait_reply::<GetInputFocusReply>(seq)?;
                        }
                        drop(self.conn.send_void(shm::detach(*major, old.id))?);
                    }
                }
            }
        }
        self.size = Some((w, h));
        Ok(())
    }

    /// Wait until the server has read the last present, then hand the buffer out.
    pub(crate) fn buffer_mut(&mut self) -> Result<X11Frame<'_>, X11PresentError> {
        if let Store::Shm { pending, .. } = &mut self.store
            && let Some(seq) = pending.take()
        {
            self.conn.wait_reply::<GetInputFocusReply>(seq)?;
        }
        Ok(X11Frame { p: self })
    }

    fn words(&self) -> usize {
        self.size
            .map_or(0, |(w, h)| usize::from(w) * usize::from(h))
    }

    fn present(&mut self, damage: &[DamageRect]) -> Result<(), X11PresentError> {
        let Some((w, h)) = self.size else {
            return Ok(());
        };
        let mut whole = None;
        match &mut self.store {
            Store::Wire(buf) => {
                let row_bytes = usize::from(w) * 4;
                let limit = self.conn.maximum_request_bytes().saturating_sub(32);
                let rows = (limit / row_bytes.max(1)).clamp(1, usize::from(h));
                for (band, chunk) in buf.chunks(rows * usize::from(w)).enumerate() {
                    let y = band * rows;
                    let band_rows = chunk.len() / usize::from(w).max(1);
                    let bytes: Vec<u8> = chunk.iter().flat_map(|p| p.to_ne_bytes()).collect();
                    let posted = self.conn.send_void(xproto::put_image(
                        image_format::Z_PIXMAP,
                        self.window,
                        self.gc,
                        w,
                        u16::try_from(band_rows).unwrap_or(h),
                        0,
                        i16::try_from(y).unwrap_or(i16::MAX),
                        0,
                        self.depth,
                        &bytes,
                    ))?;
                    // The bands draw the whole frame: an `Expose` sent before the first band's
                    // request is drawn again by every band.
                    whole.get_or_insert(posted.sequence());
                }
            }
            Store::Shm {
                major,
                seg,
                pending,
            } => {
                let Some(seg) = seg else {
                    return Ok(());
                };
                if let Some(bad) = damage.iter().find(|r| shm_rect(r).is_none()) {
                    return Err(X11PresentError::Damage(*bad));
                }
                for (x, y, rw, rh, dx, dy) in damage.iter().filter_map(shm_rect) {
                    let posted = self.conn.send_void(shm::put_image(
                        *major,
                        self.window,
                        self.gc,
                        w,
                        h,
                        x,
                        y,
                        rw,
                        rh,
                        dx,
                        dy,
                        self.depth,
                        image_format::Z_PIXMAP,
                        false,
                        seg.id,
                        0,
                    ))?;
                    if (x, y, rw, rh) == (0, 0, w, h) {
                        whole = Some(posted.sequence());
                    }
                }
                *pending = Some(self.conn.send(xproto::get_input_focus())?.detach());
            }
        }
        self.conn.flush()?;
        self.presented = true;
        if let Some(sequence) = whole {
            self.owner
                .x11_area_posted(sequence, [0, 0, u32::from(w), u32::from(h)]);
        }
        Ok(())
    }
}

impl Drop for X11Presenter {
    fn drop(&mut self) {
        if let Store::Shm {
            major,
            seg,
            pending,
        } = &mut self.store
        {
            if let Some(seq) = pending.take() {
                let _ = self.conn.wait_reply::<GetInputFocusReply>(seq);
            }
            if let Some(seg) = seg.take() {
                if let Ok(c) = self.conn.send_void(shm::detach(*major, seg.id)) {
                    drop(c);
                }
                drop(seg.mem);
            }
        }
        if let Ok(c) = self.conn.send_void(xproto::free_gc(self.gc)) {
            drop(c);
        }
        let _ = self.conn.flush();
    }
}

/// A window's X11 presenter as the GL arm's [`aterm_gpu::FrameSink`].
pub(crate) struct X11FrameSink {
    window: Arc<Window>,
    presenter: Option<X11Presenter>,
}

impl X11FrameSink {
    /// A sink for `window`, attached only if the GL arm asks.
    pub(crate) fn new(window: Arc<Window>) -> Self {
        Self {
            window,
            presenter: None,
        }
    }
}

impl aterm_gpu::FrameSink for X11FrameSink {
    fn attach(&mut self) -> Result<(), String> {
        let presenter = X11Presenter::new(Arc::clone(&self.window)).map_err(|e| e.to_string())?;
        if !matches!(presenter.store, Store::Shm { .. }) {
            return Err("the X server takes no MIT-SHM 1.2 segment from this client".to_owned());
        }
        self.presenter = Some(presenter);
        Ok(())
    }

    fn post(
        &mut self,
        width: u32,
        height: u32,
        damage: Option<[u32; 4]>,
        draw: &mut aterm_gpu::FrameDraw<'_>,
    ) -> Result<(), String> {
        let presenter = self
            .presenter
            .as_mut()
            .ok_or("a post before the presenter attached")?;
        let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            return Ok(());
        };
        presenter.resize(w, h).map_err(|e| e.to_string())?;
        let rect = post_rect(damage, presenter.presented, (width, height));
        let [x0, y0, x1, y1] = rect;
        let (Some(rw), Some(rh)) = (
            NonZeroU32::new(x1.saturating_sub(x0)),
            NonZeroU32::new(y1.saturating_sub(y0)),
        ) else {
            return Ok(());
        };
        let mut frame = presenter.buffer_mut().map_err(|e| e.to_string())?;
        draw(&mut frame, rect)?;
        frame
            .present_with_damage(&[DamageRect {
                x: x0,
                y: y0,
                width: rw,
                height: rh,
            }])
            .map_err(|e| e.to_string())
    }
}

/// The rectangle a post of a `width` x `height` frame draws and sends.
fn post_rect(damage: Option<[u32; 4]>, held: bool, (width, height): (u32, u32)) -> [u32; 4] {
    damage
        .filter(|_| held)
        .map_or([0, 0, width, height], |[x0, y0, x1, y1]| {
            [x0.min(width), y0.min(height), x1.min(width), y1.min(height)]
        })
}

/// One acquired X11 frame.
pub(crate) struct X11Frame<'a> {
    p: &'a mut X11Presenter,
}

impl Deref for X11Frame<'_> {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        let words = self.p.words();
        match &self.p.store {
            Store::Wire(buf) => &buf[..words.min(buf.len())],
            Store::Shm { seg, .. } => seg
                .as_ref()
                .and_then(|s| s.mem.words(0, words))
                .unwrap_or_default(),
        }
    }
}

impl DerefMut for X11Frame<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        let words = self.p.words();
        match &mut self.p.store {
            Store::Wire(buf) => {
                let n = words.min(buf.len());
                &mut buf[..n]
            }
            Store::Shm { seg, .. } => seg
                .as_mut()
                .and_then(|s| s.mem.words_mut(0, words))
                .unwrap_or_default(),
        }
    }
}

impl CpuFrameBuffer for X11Frame<'_> {
    type Error = X11PresentError;

    fn age(&self) -> u8 {
        u8::from(self.p.presented)
    }

    fn present(self) -> Result<(), Self::Error> {
        let Some((w, h)) = self.p.size else {
            return Ok(());
        };
        let whole = DamageRect {
            x: 0,
            y: 0,
            width: NonZeroU32::from(
                std::num::NonZeroU16::new(w).unwrap_or(std::num::NonZeroU16::MIN),
            ),
            height: NonZeroU32::from(
                std::num::NonZeroU16::new(h).unwrap_or(std::num::NonZeroU16::MIN),
            ),
        };
        self.p.present(&[whole])
    }

    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error> {
        self.p.present(damage)
    }
}

#[cfg(test)]
#[path = "x11_tests.rs"]
mod tests;
