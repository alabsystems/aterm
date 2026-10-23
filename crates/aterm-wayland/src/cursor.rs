// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wayland cursors via cursor-shape-v1 and `libwayland-cursor.so.0`.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

use crate::protocols::cursor_shape_v1::wp_cursor_shape_device_v1::shape;
use crate::proxy::{Error, Proxy};

/// A pointer shape aterm shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Default,
    Pointer,
    ColResize,
    RowResize,
    Text,
    Grabbing,
}

impl Icon {
    /// Every icon.
    pub const ALL: [Icon; 6] = [
        Icon::Default,
        Icon::Pointer,
        Icon::ColResize,
        Icon::RowResize,
        Icon::Text,
        Icon::Grabbing,
    ];

    /// Its cursor-shape-v1 shape.
    #[inline]
    pub const fn shape(self) -> u32 {
        match self {
            Icon::Default => shape::DEFAULT,
            Icon::Pointer => shape::POINTER,
            Icon::ColResize => shape::COL_RESIZE,
            Icon::RowResize => shape::ROW_RESIZE,
            Icon::Text => shape::TEXT,
            Icon::Grabbing => shape::GRABBING,
        }
    }

    /// Its theme names, the CSS name first.
    #[inline]
    pub const fn names(self) -> &'static [&'static CStr] {
        match self {
            Icon::Default => &[
                c"default",
                c"left_ptr",
                c"arrow",
                c"top_left_arrow",
                c"left_arrow",
            ],
            Icon::Pointer => &[c"pointer", c"hand2", c"hand1", c"hand", c"pointing_hand"],
            Icon::ColResize => &[
                c"col-resize",
                c"split_h",
                c"h_double_arrow",
                c"sb_h_double_arrow",
            ],
            Icon::RowResize => &[
                c"row-resize",
                c"split_v",
                c"v_double_arrow",
                c"sb_v_double_arrow",
            ],
            Icon::Text => &[c"text", c"xterm", c"ibeam"],
            Icon::Grabbing => &[c"grabbing", c"closedhand"],
        }
    }
}

/// `struct wl_cursor_image`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct WlCursorImage {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub delay: u32,
}

/// `struct wl_cursor`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct WlCursor {
    pub image_count: c_uint,
    pub images: *mut *mut WlCursorImage,
    pub name: *mut c_char,
}

aterm_dl::table! {
    /// libwayland-cursor.
    pub struct CursorLib {
        theme_load = c"wl_cursor_theme_load":
            unsafe extern "C" fn(*const c_char, c_int, *mut c_void) -> *mut c_void;
        theme_destroy = c"wl_cursor_theme_destroy": unsafe extern "C" fn(*mut c_void);
        theme_get_cursor = c"wl_cursor_theme_get_cursor":
            unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut WlCursor;
        image_get_buffer = c"wl_cursor_image_get_buffer":
            unsafe extern "C" fn(*mut WlCursorImage) -> *mut c_void;
        frame = c"wl_cursor_frame": unsafe extern "C" fn(*mut WlCursor, u32) -> c_int;
    }
}

static CURSOR_SONAMES: &[&CStr] = &[c"libwayland-cursor.so.0", c"libwayland-cursor.so"];

/// libwayland-cursor, loaded once per process.
pub fn cursor_lib() -> Result<&'static CursorLib, aterm_dl::Error> {
    static LIB: OnceLock<Result<CursorLib, aterm_dl::Error>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = aterm_dl::Library::open(CURSOR_SONAMES)?;
        // SAFETY: every row transcribes wayland-cursor.h.
        unsafe { CursorLib::load(&lib) }
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// One frame of a themed cursor: its buffer (owned by the theme) and geometry.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub buffer: Proxy,
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
}

/// A loaded cursor theme at one size; destroyed (with its buffers) on drop.
pub struct Theme {
    lib: &'static CursorLib,
    raw: NonNull<c_void>,
}

// SAFETY: the theme is used from one thread at a time by its owner; its buffers are libwayland
// proxies, which are thread-safe.
unsafe impl Send for Theme {}

impl Theme {
    /// Load `name` (or the default theme when `None`) at `size` pixels using `shm`.
    /// # Safety
    /// `shm` is a live `wl_shm` that outlives the theme.
    pub unsafe fn load(name: Option<&CStr>, size: u32, shm: Proxy) -> Result<Theme, Error> {
        let lib = cursor_lib().map_err(Error::Load)?;
        let size = c_int::try_from(size).unwrap_or(c_int::MAX);
        // SAFETY: a NUL-terminated name or null, and the caller's live wl_shm.
        let raw = unsafe {
            (lib.theme_load)(
                name.map_or(std::ptr::null(), CStr::as_ptr),
                size,
                shm.as_ptr(),
            )
        };
        NonNull::new(raw)
            .map(|raw| Theme { lib, raw })
            .ok_or(Error::NoObject)
    }

    /// The first frame of `icon`, under the first of its names the theme has.
    #[inline]
    pub fn frame(&self, icon: Icon) -> Option<Frame> {
        self.frame_named(icon.names())
    }

    /// The first frame of the cursor under the first of `names` the theme has.
    pub fn frame_named(&self, names: &[&CStr]) -> Option<Frame> {
        names.iter().find_map(|name| {
            // SAFETY: a live theme and a NUL-terminated name; the cursor and its images live as
            // long as the theme.
            unsafe {
                let cursor =
                    (self.lib.theme_get_cursor)(self.raw.as_ptr(), name.as_ptr()).as_ref()?;
                if cursor.image_count == 0 || cursor.images.is_null() {
                    return None;
                }
                let image = *cursor.images;
                let geometry = image.as_ref()?;
                let buffer = NonNull::new((self.lib.image_get_buffer)(image))?;
                Some(Frame {
                    buffer: Proxy::from_ptr(buffer),
                    width: geometry.width,
                    height: geometry.height,
                    hotspot_x: geometry.hotspot_x,
                    hotspot_y: geometry.hotspot_y,
                })
            }
        })
    }
}

impl Drop for Theme {
    fn drop(&mut self) {
        // SAFETY: the theme this value loaded, destroyed once.
        unsafe { (self.lib.theme_destroy)(self.raw.as_ptr()) }
    }
}
