// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows CPU presenter: GDI.

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::ops::{Deref, DerefMut};
use std::ptr::{self, NonNull};
use std::sync::Arc;

use aterm_win32::Win32::Foundation::{HWND, RECT};
use aterm_win32::Win32::Graphics::Gdi::{
    BI_BITFIELDS, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GdiFlush, GetDC, GetUpdateRect, HBITMAP, HDC, HGDIOBJ, ReleaseDC,
    SRCCOPY, SelectObject, ValidateRect,
};
use aterm_window::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use aterm_window::window::Window;

use super::{CpuFrameBuffer, CpuPresenter, DamageRect};

/// Why the GDI presenter failed.
#[derive(Debug)]
pub(crate) enum Win32PresentError {
    /// The window has no Win32 handle.
    NotWin32,
    /// `GetDC` gave the window no DC.
    WindowDc(std::io::Error),
    /// A size past GDI's `i32` extents, or a buffer past the address space.
    Size(u32, u32),
    /// GDI refused this call while making a buffer (its handles or memory are spent).
    Gdi(&'static str),
    /// A damage rectangle past GDI's `i32` coordinates (softbuffer's `DamageOutOfRange`).
    Damage(DamageRect),
    /// `BitBlt` refused this rectangle.
    Blit(DamageRect),
    /// `GdiFlush` reported that a batched `BitBlt` of this present failed.
    Flush,
    /// An acquire before the first resize: there is no buffer yet.
    Unsized,
}

impl std::fmt::Display for Win32PresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotWin32 => f.write_str("not a Win32 window"),
            Self::WindowDc(e) => write!(f, "GetDC gave the window no DC: {e}"),
            Self::Size(w, h) => write!(f, "{w}x{h} is past GDI's sizes"),
            Self::Gdi(call) => write!(f, "GDI refused {call}"),
            Self::Damage(r) => write!(
                f,
                "damage {}x{} at {},{} is past GDI's coordinates",
                r.width, r.height, r.x, r.y
            ),
            Self::Blit(r) => write!(
                f,
                "BitBlt refused {}x{} at {},{}",
                r.width, r.height, r.x, r.y
            ),
            Self::Flush => f.write_str("GdiFlush: a batched BitBlt failed"),
            Self::Unsized => f.write_str("no buffer: the surface was never sized"),
        }
    }
}

/// `BITMAPINFO` for `BI_BITFIELDS`: the header, then the red, green and blue
/// masks where its colour table would be.
#[repr(C)]
struct BitfieldsInfo {
    header: BITMAPINFOHEADER,
    masks: [u32; 3],
}

/// Red, green and blue in aterm's `0x00RRGGBB` words.
const MASKS: [u32; 3] = [0x00ff_0000, 0x0000_ff00, 0x0000_00ff];

/// A rectangle as `BitBlt` takes it, or `None` past `i32`.
fn gdi_rect(rect: &DamageRect) -> Option<(i32, i32, i32, i32)> {
    let fits = |v: u32| i32::try_from(v).ok();
    Some((
        fits(rect.x)?,
        fits(rect.y)?,
        fits(rect.width.get())?,
        fits(rect.height.get())?,
    ))
}

/// Flush even when a later copy failed: an earlier batched copy may still
/// read the section.
fn flush_present_blits(
    copy: impl FnOnce() -> Result<(), Win32PresentError>,
    flush: impl FnOnce() -> bool,
) -> Result<(), Win32PresentError> {
    let copied = copy();
    let flushed = flush().then_some(()).ok_or(Win32PresentError::Flush);
    copied.and(flushed)
}

/// The part of `hwnd`'s update region's bounding rectangle inside a `width` x
/// `height` buffer: what Windows asks to be repainted (rows the window gained,
/// say) and the validation that ends a present would otherwise mark drawn.
fn exposed_rect(hwnd: HWND, width: i32, height: i32) -> Option<DamageRect> {
    let mut r = RECT::default();
    // SAFETY: this surface's window and an out rectangle; FALSE erases nothing.
    if unsafe { GetUpdateRect(hwnd, &mut r, 0) } == 0 {
        return None;
    }
    let (x0, y0) = (r.left.max(0), r.top.max(0));
    let (x1, y1) = (r.right.min(width), r.bottom.min(height));
    let side = |lo: i32, hi: i32| NonZeroU32::new(u32::try_from(hi - lo).ok()?);
    Some(DamageRect {
        x: x0.unsigned_abs(),
        y: y0.unsigned_abs(),
        width: side(x0, x1)?,
        height: side(y0, y1)?,
    })
}

/// One DIB section, selected into a memory DC of its own.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    /// What `dc` held before `bitmap`, put back before `bitmap` is deleted.
    held: HGDIOBJ,
    pixels: NonNull<u32>,
    width: i32,
    height: i32,
    /// `width * height` words.
    len: usize,
    /// Whether it has been presented since it was made.
    presented: bool,
}

impl Dib {
    /// A `width` x `height` buffer compatible with `window_dc`.
    fn new(window_dc: HDC, width: i32, height: i32) -> Result<Self, Win32PresentError> {
        let too_big = || Win32PresentError::Size(width.unsigned_abs(), height.unsigned_abs());
        let len = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(w, h)| w.checked_mul(h))
            .filter(|len| {
                len.checked_mul(4)
                    .is_some_and(|b| isize::try_from(b).is_ok())
            })
            .ok_or_else(too_big)?;
        // SAFETY: `window_dc` is the window's DC, got for this call.
        let dc = unsafe { CreateCompatibleDC(window_dc) };
        if dc.is_null() {
            return Err(Win32PresentError::Gdi("CreateCompatibleDC"));
        }
        let info = BitfieldsInfo {
            header: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_BITFIELDS,
                ..BITMAPINFOHEADER::default()
            },
            masks: MASKS,
        };
        let mut bits: *mut c_void = ptr::null_mut();
        // SAFETY: `info` is a BITMAPINFOHEADER followed by its three masks,
        // as BI_BITFIELDS reads it; `bits` receives the section's pixels.
        let bitmap = unsafe {
            CreateDIBSection(
                dc,
                (&raw const info).cast(),
                DIB_RGB_COLORS,
                &mut bits,
                ptr::null_mut(),
                0,
            )
        };
        let pixels = NonNull::new(bits.cast::<u32>());
        let held = match pixels {
            // SAFETY: a memory DC and a bitmap compatible with it, both ours.
            Some(_) if !bitmap.is_null() => unsafe { SelectObject(dc, bitmap) },
            _ => ptr::null_mut(),
        };
        match pixels {
            Some(pixels) if !held.is_null() => Ok(Self {
                dc,
                bitmap,
                held,
                pixels,
                width,
                height,
                len,
                presented: false,
            }),
            _ => {
                // SAFETY: what was made above, never selected or already
                // refused, so each delete succeeds or frees nothing.
                unsafe {
                    if !bitmap.is_null() {
                        DeleteObject(bitmap);
                    }
                    DeleteDC(dc);
                }
                Err(Win32PresentError::Gdi(
                    if bitmap.is_null() || pixels.is_none() {
                        "CreateDIBSection"
                    } else {
                        "SelectObject"
                    },
                ))
            }
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: the DC and bitmap are this buffer's; `held` is what the DC held.
        unsafe {
            SelectObject(self.dc, self.held);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

/// The window's DC for one use: `GetDC`'s common DC, released when this is
/// dropped.
struct WindowDc {
    hwnd: HWND,
    dc: HDC,
}

impl WindowDc {
    /// `hwnd`'s DC, which a window of this thread gives.
    fn get(hwnd: HWND) -> Result<Self, Win32PresentError> {
        // SAFETY: the surface's window, on its thread (`GdiSurface::new`).
        let dc = unsafe { GetDC(hwnd) };
        if dc.is_null() {
            return Err(Win32PresentError::WindowDc(std::io::Error::last_os_error()));
        }
        Ok(Self { hwnd, dc })
    }
}

impl Drop for WindowDc {
    fn drop(&mut self) {
        // SAFETY: the DC `GetDC` gave for this window, on its thread.
        unsafe {
            ReleaseDC(self.hwnd, self.dc);
        }
    }
}

/// A GDI surface on one window: its buffer, presented through the window's DC got for each present.
pub(crate) struct GdiSurface {
    hwnd: HWND,
    buffer: Option<Dib>,
}

impl GdiSurface {
    /// A surface on `hwnd`, which must give a DC now (and is asked again for each present).
    /// # Safety
    /// `hwnd` is a window of this thread that outlives the surface.
    pub(crate) unsafe fn new(hwnd: HWND) -> Result<Self, Win32PresentError> {
        drop(WindowDc::get(hwnd)?);
        Ok(Self { hwnd, buffer: None })
    }

    /// The buffer `width` x `height`.
    pub(crate) fn resize(
        &mut self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<(), Win32PresentError> {
        let size = i32::try_from(width.get())
            .ok()
            .zip(i32::try_from(height.get()).ok());
        let Some((w, h)) = size else {
            return Err(Win32PresentError::Size(width.get(), height.get()));
        };
        if self
            .buffer
            .as_ref()
            .is_some_and(|b| (b.width, b.height) == (w, h))
        {
            return Ok(());
        }
        let window = WindowDc::get(self.hwnd)?;
        self.buffer = Some(Dib::new(window.dc, w, h)?);
        Ok(())
    }

    /// Whether the window has an update region.
    pub(crate) fn owes_repaint(&self) -> bool {
        // SAFETY: this surface's window; a null rectangle asks only whether.
        unsafe { GetUpdateRect(self.hwnd, ptr::null_mut(), 0) != 0 }
    }

    /// The buffer, to paint and present.
    pub(crate) fn buffer_mut(&mut self) -> Result<Win32Frame<'_>, Win32PresentError> {
        if self.buffer.is_none() {
            return Err(Win32PresentError::Unsized);
        }
        Ok(Win32Frame { surface: self })
    }
}

/// One window's GDI presenter.
pub(crate) struct Win32Presenter {
    surface: GdiSurface,
    /// Holds the winit window, and with it the HWND `surface` draws on;
    /// dropped last.
    _window: Arc<Window>,
}

/// The window's HWND, when it is a Win32 window.
fn hwnd_of(window: &Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(ptr::without_provenance_mut(h.hwnd.get() as usize)),
        _ => None,
    }
}

/// The acquired buffer of a [`GdiSurface`], which holds one.
pub(crate) struct Win32Frame<'a> {
    surface: &'a mut GdiSurface,
}

impl Win32Frame<'_> {
    fn dib(&self) -> &Dib {
        self.surface
            .buffer
            .as_ref()
            .unwrap_or_else(|| unreachable!("a frame is acquired over a buffer"))
    }
}

impl CpuPresenter for Win32Presenter {
    type Error = Win32PresentError;
    type Buffer<'a>
        = Win32Frame<'a>
    where
        Self: 'a;

    fn new(window: Arc<Window>) -> Result<Self, Self::Error> {
        let hwnd = hwnd_of(&window).ok_or(Win32PresentError::NotWin32)?;
        // SAFETY: `window` owns `hwnd` and is dropped after the surface; the
        // window is made on the thread that presents to it.
        let surface = unsafe { GdiSurface::new(hwnd) }?;
        Ok(Self {
            surface,
            _window: window,
        })
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), Self::Error> {
        self.surface.resize(width, height)
    }

    fn buffer_mut(&mut self) -> Result<Self::Buffer<'_>, Self::Error> {
        self.surface.buffer_mut()
    }

    fn owes_repaint(&self) -> bool {
        self.surface.owes_repaint()
    }
}

impl Deref for Win32Frame<'_> {
    type Target = [u32];

    fn deref(&self) -> &[u32] {
        let dib = self.dib();
        // SAFETY: the section's `len` words, alive while the buffer is.
        unsafe { std::slice::from_raw_parts(dib.pixels.as_ptr(), dib.len) }
    }
}

impl DerefMut for Win32Frame<'_> {
    fn deref_mut(&mut self) -> &mut [u32] {
        let dib = self.dib();
        let (pixels, len) = (dib.pixels, dib.len);
        // SAFETY: as `deref`, borrowed mutably through the frame, which holds the surface.
        unsafe { std::slice::from_raw_parts_mut(pixels.as_ptr(), len) }
    }
}

impl CpuFrameBuffer for Win32Frame<'_> {
    type Error = Win32PresentError;

    fn age(&self) -> u8 {
        u8::from(self.dib().presented)
    }

    fn present(self) -> Result<(), Self::Error> {
        let dib = self.dib();
        let whole = DamageRect {
            x: 0,
            y: 0,
            width: NonZeroU32::new(dib.width.unsigned_abs())
                .unwrap_or_else(|| unreachable!("a buffer is at least 1x1")),
            height: NonZeroU32::new(dib.height.unsigned_abs())
                .unwrap_or_else(|| unreachable!("a buffer is at least 1x1")),
        };
        self.present_with_damage(&[whole])
    }

    fn present_with_damage(self, damage: &[DamageRect]) -> Result<(), Self::Error> {
        // Every rectangle is checked before any is drawn.
        if let Some(r) = damage.iter().find(|r| gdi_rect(r).is_none()) {
            return Err(Win32PresentError::Damage(*r));
        }
        let surface = self.surface;
        let Some(dib) = surface.buffer.as_mut() else {
            return Err(Win32PresentError::Unsized);
        };
        let window = WindowDc::get(surface.hwnd)?;
        let exposed = exposed_rect(surface.hwnd, dib.width, dib.height);
        let copied = flush_present_blits(
            || {
                for rect in damage.iter().chain(&exposed) {
                    let Some((x, y, w, h)) = gdi_rect(rect) else {
                        return Err(Win32PresentError::Damage(*rect));
                    };
                    // SAFETY: the window's DC and the buffer's, both live.
                    let blitted = unsafe { BitBlt(window.dc, x, y, w, h, dib.dc, x, y, SRCCOPY) };
                    if blitted == 0 {
                        return Err(Win32PresentError::Blit(*rect));
                    }
                }
                Ok(())
            },
            || {
                // SAFETY: a call on this thread's GDI batch.
                unsafe { GdiFlush() != 0 }
            },
        );
        if let Err(error) = copied {
            dib.presented = false;
            return Err(error);
        }
        drop(window);
        // SAFETY: this surface's window; a null rectangle is its whole client area.
        unsafe {
            ValidateRect(surface.hwnd, ptr::null());
        }
        dib.presented = true;
        Ok(())
    }
}

#[cfg(test)]
#[path = "win32_tests.rs"]
mod tests;
