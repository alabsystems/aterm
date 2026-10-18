// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates
// Copyright © 2008-2011 Kristian Høgsberg
// Copyright © 2010-2011 Intel Corporation
// Copyright © 2012-2013 Collabora, Ltd.
//
// Generated from protocols/wayland.xml (MIT, see protocols/COPYING.wayland) by
// `targo --unverified run --release` in crates/aterm-wayland/scan. Do not edit.

//! The `wayland` protocol.

pub use self::tables::*;

#[rustfmt::skip]
mod tables {
/// core global object
pub mod wl_display {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_display", 1, &REQUESTS, &EVENTS);
    static REQUESTS_SYNC_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_callback::INTERFACE),
    ];
    static REQUESTS_GET_REGISTRY_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_registry::INTERFACE),
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"sync", c"n", &REQUESTS_SYNC_TYPES),
        Message::new(c"get_registry", c"n", &REQUESTS_GET_REGISTRY_TYPES),
    ];
    static EVENTS: [Message; 2] = [
        Message::new(c"error", c"ous", &NULLS),
        Message::new(c"delete_id", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const SYNC: u32 = 0;
        pub const GET_REGISTRY: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
        pub const ERROR: u32 = 0;
        pub const DELETE_ID: u32 = 1;
    }
    /// The `error` enum.
    pub mod error {
        /// server couldn't find object
        pub const INVALID_OBJECT: u32 = 0x0;
        /// method doesn't exist on the specified interface or malformed request
        pub const INVALID_METHOD: u32 = 0x1;
        /// server is out of memory
        pub const NO_MEMORY: u32 = 0x2;
        /// implementation error in compositor
        pub const IMPLEMENTATION: u32 = 0x3;
    }
    /// asynchronous roundtrip
    ///
    /// # Safety
    /// `this` is a live `wl_display`; every object argument is live.
    pub unsafe fn sync(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_callback::INTERFACE, this.version())), false) }
    }
    /// get global registry object
    ///
    /// # Safety
    /// `this` is a live `wl_display`; every object argument is live.
    pub unsafe fn get_registry(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_registry::INTERFACE, this.version())), false) }
    }
}

/// global registry object
pub mod wl_registry {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_registry", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 1] = [
        Message::new(c"bind", c"usun", &NULLS),
    ];
    static EVENTS: [Message; 2] = [
        Message::new(c"global", c"usu", &NULLS),
        Message::new(c"global_remove", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const BIND: u32 = 0;
    }
    /// Event opcodes.
    pub mod evt {
        pub const GLOBAL: u32 = 0;
        pub const GLOBAL_REMOVE: u32 = 1;
    }
    /// bind an object to the display
    ///
    /// # Safety
    /// `this` is a live `wl_registry`; every object argument is live.
    pub unsafe fn bind(this: Proxy, name: u32, interface: &'static Interface, version: u32) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::uint(name), Arg::string(Some(interface.name())), Arg::uint(version), Arg::new_id()], Some((interface, version)), false) }
    }
}

/// callback object
pub mod wl_callback {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_callback", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 0] = [
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"done", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
    }
    /// Event opcodes.
    pub mod evt {
        pub const DONE: u32 = 0;
    }
}

/// the compositor singleton
pub mod wl_compositor {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 7;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_compositor", 7, &REQUESTS, &EVENTS);
    static REQUESTS_CREATE_SURFACE_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS_CREATE_REGION_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_region::INTERFACE),
    ];
    static REQUESTS: [Message; 3] = [
        Message::new(c"create_surface", c"n", &REQUESTS_CREATE_SURFACE_TYPES),
        Message::new(c"create_region", c"n", &REQUESTS_CREATE_REGION_TYPES),
        Message::new(c"release", c"7", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const CREATE_SURFACE: u32 = 0;
        pub const CREATE_REGION: u32 = 1;
        pub const RELEASE: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// create new surface
    ///
    /// # Safety
    /// `this` is a live `wl_compositor`; every object argument is live.
    pub unsafe fn create_surface(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_surface::INTERFACE, this.version())), false) }
    }
    /// create new region
    ///
    /// # Safety
    /// `this` is a live `wl_compositor`; every object argument is live.
    pub unsafe fn create_region(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_region::INTERFACE, this.version())), false) }
    }
    /// destroy wl_compositor
    ///
    /// # Safety
    /// `this` is a live `wl_compositor`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [], None, true) };
    }
}

/// a shared memory pool
pub mod wl_shm_pool {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 2;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_shm_pool", 2, &REQUESTS, &EVENTS);
    static REQUESTS_CREATE_BUFFER_TYPES: [TypeRef; 6] = [
        TypeRef::to(&crate::protocols::wayland::wl_buffer::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS: [Message; 3] = [
        Message::new(c"create_buffer", c"niiiiu", &REQUESTS_CREATE_BUFFER_TYPES),
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"resize", c"i", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const CREATE_BUFFER: u32 = 0;
        pub const DESTROY: u32 = 1;
        pub const RESIZE: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// create a buffer from the pool
    ///
    /// # Safety
    /// `this` is a live `wl_shm_pool`; every object argument is live.
    pub unsafe fn create_buffer(this: Proxy, offset: i32, width: i32, height: i32, stride: i32, format: u32) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::new_id(), Arg::int(offset), Arg::int(width), Arg::int(height), Arg::int(stride), Arg::uint(format)], Some((&crate::protocols::wayland::wl_buffer::INTERFACE, this.version())), false) }
    }
    /// destroy the pool
    ///
    /// # Safety
    /// `this` is a live `wl_shm_pool`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [], None, true) };
    }
    /// change the size of the pool mapping
    ///
    /// # Safety
    /// `this` is a live `wl_shm_pool`; every object argument is live.
    pub unsafe fn resize(this: Proxy, size: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::int(size)], None, false) };
    }
}

/// shared memory support
pub mod wl_shm {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 2;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_shm", 2, &REQUESTS, &EVENTS);
    static REQUESTS_CREATE_POOL_TYPES: [TypeRef; 3] = [
        TypeRef::to(&crate::protocols::wayland::wl_shm_pool::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"create_pool", c"nhi", &REQUESTS_CREATE_POOL_TYPES),
        Message::new(c"release", c"2", &NULLS),
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"format", c"u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const CREATE_POOL: u32 = 0;
        pub const RELEASE: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
        pub const FORMAT: u32 = 0;
    }
    /// The `error` enum.
    pub mod error {
        /// buffer format is not known
        pub const INVALID_FORMAT: u32 = 0x0;
        /// invalid size or stride during pool or buffer creation
        pub const INVALID_STRIDE: u32 = 0x1;
        /// mmapping the file descriptor failed
        pub const INVALID_FD: u32 = 0x2;
    }
    /// The `format` enum.
    pub mod format {
        /// 32-bit ARGB format, \[31:0\] A:R:G:B 8:8:8:8 little endian
        pub const ARGB8888: u32 = 0x0;
        /// 32-bit RGB format, \[31:0\] x:R:G:B 8:8:8:8 little endian
        pub const XRGB8888: u32 = 0x1;
        /// 8-bit color index format, \[7:0\] C
        pub const C8: u32 = 0x20203843;
        /// 8-bit RGB format, \[7:0\] R:G:B 3:3:2
        pub const RGB332: u32 = 0x38424752;
        /// 8-bit BGR format, \[7:0\] B:G:R 2:3:3
        pub const BGR233: u32 = 0x38524742;
        /// 16-bit xRGB format, \[15:0\] x:R:G:B 4:4:4:4 little endian
        pub const XRGB4444: u32 = 0x32315258;
        /// 16-bit xBGR format, \[15:0\] x:B:G:R 4:4:4:4 little endian
        pub const XBGR4444: u32 = 0x32314258;
        /// 16-bit RGBx format, \[15:0\] R:G:B:x 4:4:4:4 little endian
        pub const RGBX4444: u32 = 0x32315852;
        /// 16-bit BGRx format, \[15:0\] B:G:R:x 4:4:4:4 little endian
        pub const BGRX4444: u32 = 0x32315842;
        /// 16-bit ARGB format, \[15:0\] A:R:G:B 4:4:4:4 little endian
        pub const ARGB4444: u32 = 0x32315241;
        /// 16-bit ABGR format, \[15:0\] A:B:G:R 4:4:4:4 little endian
        pub const ABGR4444: u32 = 0x32314241;
        /// 16-bit RBGA format, \[15:0\] R:G:B:A 4:4:4:4 little endian
        pub const RGBA4444: u32 = 0x32314152;
        /// 16-bit BGRA format, \[15:0\] B:G:R:A 4:4:4:4 little endian
        pub const BGRA4444: u32 = 0x32314142;
        /// 16-bit xRGB format, \[15:0\] x:R:G:B 1:5:5:5 little endian
        pub const XRGB1555: u32 = 0x35315258;
        /// 16-bit xBGR 1555 format, \[15:0\] x:B:G:R 1:5:5:5 little endian
        pub const XBGR1555: u32 = 0x35314258;
        /// 16-bit RGBx 5551 format, \[15:0\] R:G:B:x 5:5:5:1 little endian
        pub const RGBX5551: u32 = 0x35315852;
        /// 16-bit BGRx 5551 format, \[15:0\] B:G:R:x 5:5:5:1 little endian
        pub const BGRX5551: u32 = 0x35315842;
        /// 16-bit ARGB 1555 format, \[15:0\] A:R:G:B 1:5:5:5 little endian
        pub const ARGB1555: u32 = 0x35315241;
        /// 16-bit ABGR 1555 format, \[15:0\] A:B:G:R 1:5:5:5 little endian
        pub const ABGR1555: u32 = 0x35314241;
        /// 16-bit RGBA 5551 format, \[15:0\] R:G:B:A 5:5:5:1 little endian
        pub const RGBA5551: u32 = 0x35314152;
        /// 16-bit BGRA 5551 format, \[15:0\] B:G:R:A 5:5:5:1 little endian
        pub const BGRA5551: u32 = 0x35314142;
        /// 16-bit RGB 565 format, \[15:0\] R:G:B 5:6:5 little endian
        pub const RGB565: u32 = 0x36314752;
        /// 16-bit BGR 565 format, \[15:0\] B:G:R 5:6:5 little endian
        pub const BGR565: u32 = 0x36314742;
        /// 24-bit RGB format, \[23:0\] R:G:B little endian
        pub const RGB888: u32 = 0x34324752;
        /// 24-bit BGR format, \[23:0\] B:G:R little endian
        pub const BGR888: u32 = 0x34324742;
        /// 32-bit xBGR format, \[31:0\] x:B:G:R 8:8:8:8 little endian
        pub const XBGR8888: u32 = 0x34324258;
        /// 32-bit RGBx format, \[31:0\] R:G:B:x 8:8:8:8 little endian
        pub const RGBX8888: u32 = 0x34325852;
        /// 32-bit BGRx format, \[31:0\] B:G:R:x 8:8:8:8 little endian
        pub const BGRX8888: u32 = 0x34325842;
        /// 32-bit ABGR format, \[31:0\] A:B:G:R 8:8:8:8 little endian
        pub const ABGR8888: u32 = 0x34324241;
        /// 32-bit RGBA format, \[31:0\] R:G:B:A 8:8:8:8 little endian
        pub const RGBA8888: u32 = 0x34324152;
        /// 32-bit BGRA format, \[31:0\] B:G:R:A 8:8:8:8 little endian
        pub const BGRA8888: u32 = 0x34324142;
        /// 32-bit xRGB format, \[31:0\] x:R:G:B 2:10:10:10 little endian
        pub const XRGB2101010: u32 = 0x30335258;
        /// 32-bit xBGR format, \[31:0\] x:B:G:R 2:10:10:10 little endian
        pub const XBGR2101010: u32 = 0x30334258;
        /// 32-bit RGBx format, \[31:0\] R:G:B:x 10:10:10:2 little endian
        pub const RGBX1010102: u32 = 0x30335852;
        /// 32-bit BGRx format, \[31:0\] B:G:R:x 10:10:10:2 little endian
        pub const BGRX1010102: u32 = 0x30335842;
        /// 32-bit ARGB format, \[31:0\] A:R:G:B 2:10:10:10 little endian
        pub const ARGB2101010: u32 = 0x30335241;
        /// 32-bit ABGR format, \[31:0\] A:B:G:R 2:10:10:10 little endian
        pub const ABGR2101010: u32 = 0x30334241;
        /// 32-bit RGBA format, \[31:0\] R:G:B:A 10:10:10:2 little endian
        pub const RGBA1010102: u32 = 0x30334152;
        /// 32-bit BGRA format, \[31:0\] B:G:R:A 10:10:10:2 little endian
        pub const BGRA1010102: u32 = 0x30334142;
        /// packed YCbCr format, \[31:0\] Cr0:Y1:Cb0:Y0 8:8:8:8 little endian
        pub const YUYV: u32 = 0x56595559;
        /// packed YCbCr format, \[31:0\] Cb0:Y1:Cr0:Y0 8:8:8:8 little endian
        pub const YVYU: u32 = 0x55595659;
        /// packed YCbCr format, \[31:0\] Y1:Cr0:Y0:Cb0 8:8:8:8 little endian
        pub const UYVY: u32 = 0x59565955;
        /// packed YCbCr format, \[31:0\] Y1:Cb0:Y0:Cr0 8:8:8:8 little endian
        pub const VYUY: u32 = 0x59555956;
        /// packed AYCbCr format, \[31:0\] A:Y:Cb:Cr 8:8:8:8 little endian
        pub const AYUV: u32 = 0x56555941;
        /// 2 plane YCbCr Cr:Cb format, 2x2 subsampled Cr:Cb plane
        pub const NV12: u32 = 0x3231564e;
        /// 2 plane YCbCr Cb:Cr format, 2x2 subsampled Cb:Cr plane
        pub const NV21: u32 = 0x3132564e;
        /// 2 plane YCbCr Cr:Cb format, 2x1 subsampled Cr:Cb plane
        pub const NV16: u32 = 0x3631564e;
        /// 2 plane YCbCr Cb:Cr format, 2x1 subsampled Cb:Cr plane
        pub const NV61: u32 = 0x3136564e;
        /// 3 plane YCbCr format, 4x4 subsampled Cb (1) and Cr (2) planes
        pub const YUV410: u32 = 0x39565559;
        /// 3 plane YCbCr format, 4x4 subsampled Cr (1) and Cb (2) planes
        pub const YVU410: u32 = 0x39555659;
        /// 3 plane YCbCr format, 4x1 subsampled Cb (1) and Cr (2) planes
        pub const YUV411: u32 = 0x31315559;
        /// 3 plane YCbCr format, 4x1 subsampled Cr (1) and Cb (2) planes
        pub const YVU411: u32 = 0x31315659;
        /// 3 plane YCbCr format, 2x2 subsampled Cb (1) and Cr (2) planes
        pub const YUV420: u32 = 0x32315559;
        /// 3 plane YCbCr format, 2x2 subsampled Cr (1) and Cb (2) planes
        pub const YVU420: u32 = 0x32315659;
        /// 3 plane YCbCr format, 2x1 subsampled Cb (1) and Cr (2) planes
        pub const YUV422: u32 = 0x36315559;
        /// 3 plane YCbCr format, 2x1 subsampled Cr (1) and Cb (2) planes
        pub const YVU422: u32 = 0x36315659;
        /// 3 plane YCbCr format, non-subsampled Cb (1) and Cr (2) planes
        pub const YUV444: u32 = 0x34325559;
        /// 3 plane YCbCr format, non-subsampled Cr (1) and Cb (2) planes
        pub const YVU444: u32 = 0x34325659;
        /// \[7:0\] R
        pub const R8: u32 = 0x20203852;
        /// \[15:0\] R little endian
        pub const R16: u32 = 0x20363152;
        /// \[15:0\] R:G 8:8 little endian
        pub const RG88: u32 = 0x38384752;
        /// \[15:0\] G:R 8:8 little endian
        pub const GR88: u32 = 0x38385247;
        /// \[31:0\] R:G 16:16 little endian
        pub const RG1616: u32 = 0x32334752;
        /// \[31:0\] G:R 16:16 little endian
        pub const GR1616: u32 = 0x32335247;
        /// \[63:0\] x:R:G:B 16:16:16:16 little endian
        pub const XRGB16161616F: u32 = 0x48345258;
        /// \[63:0\] x:B:G:R 16:16:16:16 little endian
        pub const XBGR16161616F: u32 = 0x48344258;
        /// \[63:0\] A:R:G:B 16:16:16:16 little endian
        pub const ARGB16161616F: u32 = 0x48345241;
        /// \[63:0\] A:B:G:R 16:16:16:16 little endian
        pub const ABGR16161616F: u32 = 0x48344241;
        /// \[31:0\] X:Y:Cb:Cr 8:8:8:8 little endian
        pub const XYUV8888: u32 = 0x56555958;
        /// \[23:0\] Cr:Cb:Y 8:8:8 little endian
        pub const VUY888: u32 = 0x34325556;
        /// Y followed by U then V, 10:10:10. Non-linear modifier only
        pub const VUY101010: u32 = 0x30335556;
        /// \[63:0\] Cr0:0:Y1:0:Cb0:0:Y0:0 10:6:10:6:10:6:10:6 little endian per 2 Y pixels
        pub const Y210: u32 = 0x30313259;
        /// \[63:0\] Cr0:0:Y1:0:Cb0:0:Y0:0 12:4:12:4:12:4:12:4 little endian per 2 Y pixels
        pub const Y212: u32 = 0x32313259;
        /// \[63:0\] Cr0:Y1:Cb0:Y0 16:16:16:16 little endian per 2 Y pixels
        pub const Y216: u32 = 0x36313259;
        /// \[31:0\] A:Cr:Y:Cb 2:10:10:10 little endian
        pub const Y410: u32 = 0x30313459;
        /// \[63:0\] A:0:Cr:0:Y:0:Cb:0 12:4:12:4:12:4:12:4 little endian
        pub const Y412: u32 = 0x32313459;
        /// \[63:0\] A:Cr:Y:Cb 16:16:16:16 little endian
        pub const Y416: u32 = 0x36313459;
        /// \[31:0\] X:Cr:Y:Cb 2:10:10:10 little endian
        pub const XVYU2101010: u32 = 0x30335658;
        /// \[63:0\] X:0:Cr:0:Y:0:Cb:0 12:4:12:4:12:4:12:4 little endian
        pub const XVYU12_16161616: u32 = 0x36335658;
        /// \[63:0\] X:Cr:Y:Cb 16:16:16:16 little endian
        pub const XVYU16161616: u32 = 0x38345658;
        /// \[63:0\] A3:A2:Y3:0:Cr0:0:Y2:0:A1:A0:Y1:0:Cb0:0:Y0:0 1:1:8:2:8:2:8:2:1:1:8:2:8:2:8:2 little endian
        pub const Y0L0: u32 = 0x304c3059;
        /// \[63:0\] X3:X2:Y3:0:Cr0:0:Y2:0:X1:X0:Y1:0:Cb0:0:Y0:0 1:1:8:2:8:2:8:2:1:1:8:2:8:2:8:2 little endian
        pub const X0L0: u32 = 0x304c3058;
        /// \[63:0\] A3:A2:Y3:Cr0:Y2:A1:A0:Y1:Cb0:Y0 1:1:10:10:10:1:1:10:10:10 little endian
        pub const Y0L2: u32 = 0x324c3059;
        /// \[63:0\] X3:X2:Y3:Cr0:Y2:X1:X0:Y1:Cb0:Y0 1:1:10:10:10:1:1:10:10:10 little endian
        pub const X0L2: u32 = 0x324c3058;
        pub const YUV420_8BIT: u32 = 0x38305559;
        pub const YUV420_10BIT: u32 = 0x30315559;
        pub const XRGB8888_A8: u32 = 0x38415258;
        pub const XBGR8888_A8: u32 = 0x38414258;
        pub const RGBX8888_A8: u32 = 0x38415852;
        pub const BGRX8888_A8: u32 = 0x38415842;
        pub const RGB888_A8: u32 = 0x38413852;
        pub const BGR888_A8: u32 = 0x38413842;
        pub const RGB565_A8: u32 = 0x38413552;
        pub const BGR565_A8: u32 = 0x38413542;
        /// non-subsampled Cr:Cb plane
        pub const NV24: u32 = 0x3432564e;
        /// non-subsampled Cb:Cr plane
        pub const NV42: u32 = 0x3234564e;
        /// 2x1 subsampled Cr:Cb plane, 10 bit per channel
        pub const P210: u32 = 0x30313250;
        /// 2x2 subsampled Cr:Cb plane 10 bits per channel
        pub const P010: u32 = 0x30313050;
        /// 2x2 subsampled Cr:Cb plane 12 bits per channel
        pub const P012: u32 = 0x32313050;
        /// 2x2 subsampled Cr:Cb plane 16 bits per channel
        pub const P016: u32 = 0x36313050;
        /// \[63:0\] A:x:B:x:G:x:R:x 10:6:10:6:10:6:10:6 little endian
        pub const AXBXGXRX106106106106: u32 = 0x30314241;
        /// 2x2 subsampled Cr:Cb plane
        pub const NV15: u32 = 0x3531564e;
        pub const Q410: u32 = 0x30313451;
        pub const Q401: u32 = 0x31303451;
        /// \[63:0\] x:R:G:B 16:16:16:16 little endian
        pub const XRGB16161616: u32 = 0x38345258;
        /// \[63:0\] x:B:G:R 16:16:16:16 little endian
        pub const XBGR16161616: u32 = 0x38344258;
        /// \[63:0\] A:R:G:B 16:16:16:16 little endian
        pub const ARGB16161616: u32 = 0x38345241;
        /// \[63:0\] A:B:G:R 16:16:16:16 little endian
        pub const ABGR16161616: u32 = 0x38344241;
        /// \[7:0\] C0:C1:C2:C3:C4:C5:C6:C7 1:1:1:1:1:1:1:1 eight pixels/byte
        pub const C1: u32 = 0x20203143;
        /// \[7:0\] C0:C1:C2:C3 2:2:2:2 four pixels/byte
        pub const C2: u32 = 0x20203243;
        /// \[7:0\] C0:C1 4:4 two pixels/byte
        pub const C4: u32 = 0x20203443;
        /// \[7:0\] D0:D1:D2:D3:D4:D5:D6:D7 1:1:1:1:1:1:1:1 eight pixels/byte
        pub const D1: u32 = 0x20203144;
        /// \[7:0\] D0:D1:D2:D3 2:2:2:2 four pixels/byte
        pub const D2: u32 = 0x20203244;
        /// \[7:0\] D0:D1 4:4 two pixels/byte
        pub const D4: u32 = 0x20203444;
        /// \[7:0\] D
        pub const D8: u32 = 0x20203844;
        /// \[7:0\] R0:R1:R2:R3:R4:R5:R6:R7 1:1:1:1:1:1:1:1 eight pixels/byte
        pub const R1: u32 = 0x20203152;
        /// \[7:0\] R0:R1:R2:R3 2:2:2:2 four pixels/byte
        pub const R2: u32 = 0x20203252;
        /// \[7:0\] R0:R1 4:4 two pixels/byte
        pub const R4: u32 = 0x20203452;
        /// \[15:0\] x:R 6:10 little endian
        pub const R10: u32 = 0x20303152;
        /// \[15:0\] x:R 4:12 little endian
        pub const R12: u32 = 0x20323152;
        /// \[31:0\] A:Cr:Cb:Y 8:8:8:8 little endian
        pub const AVUY8888: u32 = 0x59555641;
        /// \[31:0\] X:Cr:Cb:Y 8:8:8:8 little endian
        pub const XVUY8888: u32 = 0x59555658;
        /// 2x2 subsampled Cr:Cb plane 10 bits per channel packed
        pub const P030: u32 = 0x30333050;
        /// \[47:0\] R:G:B 16:16:16 little endian
        pub const RGB161616: u32 = 0x38344752;
        /// \[47:0\] B:G:R 16:16:16 little endian
        pub const BGR161616: u32 = 0x38344742;
        /// \[15:0\] R 16 little endian
        pub const R16F: u32 = 0x48202052;
        /// \[31:0\] G:R 16:16 little endian
        pub const GR1616F: u32 = 0x48205247;
        /// \[47:0\] B:G:R 16:16:16 little endian
        pub const BGR161616F: u32 = 0x48524742;
        /// \[31:0\] R 32 little endian
        pub const R32F: u32 = 0x46202052;
        /// \[63:0\] R:G 32:32 little endian
        pub const GR3232F: u32 = 0x46205247;
        /// \[95:0\] R:G:B 32:32:32 little endian
        pub const BGR323232F: u32 = 0x46524742;
        /// \[127:0\] R:G:B:A 32:32:32:32 little endian
        pub const ABGR32323232F: u32 = 0x46384241;
        /// 2x1 subsampled Cr:Cb plane
        pub const NV20: u32 = 0x3032564e;
        /// non-subsampled Cr:Cb plane
        pub const NV30: u32 = 0x3033564e;
        /// 2x2 subsampled Cb (1) and Cr (2) planes 10 bits per channel
        pub const S010: u32 = 0x30313053;
        /// 2x1 subsampled Cb (1) and Cr (2) planes 10 bits per channel
        pub const S210: u32 = 0x30313253;
        /// non-subsampled Cb (1) and Cr (2) planes 10 bits per channel
        pub const S410: u32 = 0x30313453;
        /// 2x2 subsampled Cb (1) and Cr (2) planes 12 bits per channel
        pub const S012: u32 = 0x32313053;
        /// 2x1 subsampled Cb (1) and Cr (2) planes 12 bits per channel
        pub const S212: u32 = 0x32313253;
        /// non-subsampled Cb (1) and Cr (2) planes 12 bits per channel
        pub const S412: u32 = 0x32313453;
        /// 2x2 subsampled Cb (1) and Cr (2) planes 16 bits per channel
        pub const S016: u32 = 0x36313053;
        /// 2x1 subsampled Cb (1) and Cr (2) planes 16 bits per channel
        pub const S216: u32 = 0x36313253;
        /// non-subsampled Cb (1) and Cr (2) planes 16 bits per channel
        pub const S416: u32 = 0x36313453;
    }
    /// create a shm pool
    ///
    /// # Safety
    /// `this` is a live `wl_shm`; every object argument is live.
    pub unsafe fn create_pool(this: Proxy, fd: c_int, size: i32) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::new_id(), Arg::fd(fd), Arg::int(size)], Some((&crate::protocols::wayland::wl_shm_pool::INTERFACE, this.version())), false) }
    }
    /// release the shm object
    ///
    /// # Safety
    /// `this` is a live `wl_shm`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [], None, true) };
    }
}

/// content for a wl_surface
pub mod wl_buffer {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_buffer", 1, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 1] = [
        Message::new(c"destroy", c"", &NULLS),
    ];
    static EVENTS: [Message; 1] = [
        Message::new(c"release", c"", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
    }
    /// Event opcodes.
    pub mod evt {
        pub const RELEASE: u32 = 0;
    }
    /// destroy a buffer
    ///
    /// # Safety
    /// `this` is a live `wl_buffer`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
}

/// offer to transfer data
pub mod wl_data_offer {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 4;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_data_offer", 4, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 5] = [
        Message::new(c"accept", c"u?s", &NULLS),
        Message::new(c"receive", c"sh", &NULLS),
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"finish", c"3", &NULLS),
        Message::new(c"set_actions", c"3uu", &NULLS),
    ];
    static EVENTS: [Message; 3] = [
        Message::new(c"offer", c"s", &NULLS),
        Message::new(c"source_actions", c"3u", &NULLS),
        Message::new(c"action", c"3u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const ACCEPT: u32 = 0;
        pub const RECEIVE: u32 = 1;
        pub const DESTROY: u32 = 2;
        pub const FINISH: u32 = 3;
        pub const SET_ACTIONS: u32 = 4;
    }
    /// Event opcodes.
    pub mod evt {
        pub const OFFER: u32 = 0;
        pub const SOURCE_ACTIONS: u32 = 1;
        pub const ACTION: u32 = 2;
    }
    /// The `error` enum.
    pub mod error {
        /// finish request was called untimely
        pub const INVALID_FINISH: u32 = 0x0;
        /// action mask contains invalid values
        pub const INVALID_ACTION_MASK: u32 = 0x1;
        /// action argument has an invalid value
        pub const INVALID_ACTION: u32 = 0x2;
        /// offer doesn't accept this request
        pub const INVALID_OFFER: u32 = 0x3;
    }
    /// accept one of the offered mime types
    ///
    /// # Safety
    /// `this` is a live `wl_data_offer`; every object argument is live.
    pub unsafe fn accept(this: Proxy, serial: u32, mime_type: Option<&CStr>) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::uint(serial), Arg::string(mime_type)], None, false) };
    }
    /// request that the data is transferred
    ///
    /// # Safety
    /// `this` is a live `wl_data_offer`; every object argument is live.
    pub unsafe fn receive(this: Proxy, mime_type: &CStr, fd: c_int) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::string(Some(mime_type)), Arg::fd(fd)], None, false) };
    }
    /// destroy data offer
    ///
    /// # Safety
    /// `this` is a live `wl_data_offer`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [], None, true) };
    }
    /// the offer will no longer be used
    ///
    /// # Safety
    /// `this` is a live `wl_data_offer`; every object argument is live.
    pub unsafe fn finish(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [], None, false) };
    }
    /// set the available/preferred drag-and-drop actions
    ///
    /// # Safety
    /// `this` is a live `wl_data_offer`; every object argument is live.
    pub unsafe fn set_actions(this: Proxy, dnd_actions: u32, preferred_action: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [Arg::uint(dnd_actions), Arg::uint(preferred_action)], None, false) };
    }
}

/// offer to transfer data
pub mod wl_data_source {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 4;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_data_source", 4, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 3] = [
        Message::new(c"offer", c"s", &NULLS),
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"set_actions", c"3u", &NULLS),
    ];
    static EVENTS: [Message; 6] = [
        Message::new(c"target", c"?s", &NULLS),
        Message::new(c"send", c"sh", &NULLS),
        Message::new(c"cancelled", c"", &NULLS),
        Message::new(c"dnd_drop_performed", c"3", &NULLS),
        Message::new(c"dnd_finished", c"3", &NULLS),
        Message::new(c"action", c"3u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const OFFER: u32 = 0;
        pub const DESTROY: u32 = 1;
        pub const SET_ACTIONS: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
        pub const TARGET: u32 = 0;
        pub const SEND: u32 = 1;
        pub const CANCELLED: u32 = 2;
        pub const DND_DROP_PERFORMED: u32 = 3;
        pub const DND_FINISHED: u32 = 4;
        pub const ACTION: u32 = 5;
    }
    /// The `error` enum.
    pub mod error {
        /// action mask contains invalid values
        pub const INVALID_ACTION_MASK: u32 = 0x0;
        /// source doesn't accept this request
        pub const INVALID_SOURCE: u32 = 0x1;
    }
    /// add an offered mime type
    ///
    /// # Safety
    /// `this` is a live `wl_data_source`; every object argument is live.
    pub unsafe fn offer(this: Proxy, mime_type: &CStr) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::string(Some(mime_type))], None, false) };
    }
    /// destroy the data source
    ///
    /// # Safety
    /// `this` is a live `wl_data_source`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [], None, true) };
    }
    /// set the available drag-and-drop actions
    ///
    /// # Safety
    /// `this` is a live `wl_data_source`; every object argument is live.
    pub unsafe fn set_actions(this: Proxy, dnd_actions: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::uint(dnd_actions)], None, false) };
    }
}

/// data transfer device
pub mod wl_data_device {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 4;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_data_device", 4, &REQUESTS, &EVENTS);
    static REQUESTS_START_DRAG_TYPES: [TypeRef; 4] = [
        TypeRef::to(&crate::protocols::wayland::wl_data_source::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::NULL,
    ];
    static REQUESTS_SET_SELECTION_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::wayland::wl_data_source::INTERFACE),
        TypeRef::NULL,
    ];
    static REQUESTS: [Message; 3] = [
        Message::new(c"start_drag", c"?oo?ou", &REQUESTS_START_DRAG_TYPES),
        Message::new(c"set_selection", c"?ou", &REQUESTS_SET_SELECTION_TYPES),
        Message::new(c"release", c"2", &NULLS),
    ];
    static EVENTS_DATA_OFFER_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_data_offer::INTERFACE),
    ];
    static EVENTS_ENTER_TYPES: [TypeRef; 5] = [
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_data_offer::INTERFACE),
    ];
    static EVENTS_SELECTION_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_data_offer::INTERFACE),
    ];
    static EVENTS: [Message; 6] = [
        Message::new(c"data_offer", c"n", &EVENTS_DATA_OFFER_TYPES),
        Message::new(c"enter", c"uoff?o", &EVENTS_ENTER_TYPES),
        Message::new(c"leave", c"", &NULLS),
        Message::new(c"motion", c"uff", &NULLS),
        Message::new(c"drop", c"", &NULLS),
        Message::new(c"selection", c"?o", &EVENTS_SELECTION_TYPES),
    ];
    /// Request opcodes.
    pub mod req {
        pub const START_DRAG: u32 = 0;
        pub const SET_SELECTION: u32 = 1;
        pub const RELEASE: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
        pub const DATA_OFFER: u32 = 0;
        pub const ENTER: u32 = 1;
        pub const LEAVE: u32 = 2;
        pub const MOTION: u32 = 3;
        pub const DROP: u32 = 4;
        pub const SELECTION: u32 = 5;
    }
    /// The `error` enum.
    pub mod error {
        /// given wl_surface has another role
        pub const ROLE: u32 = 0x0;
        /// source has already been used
        pub const USED_SOURCE: u32 = 0x1;
    }
    /// start drag-and-drop operation
    ///
    /// # Safety
    /// `this` is a live `wl_data_device`; every object argument is live.
    pub unsafe fn start_drag(this: Proxy, source: Option<Proxy>, origin: Proxy, icon: Option<Proxy>, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::object(source), Arg::object(Some(origin)), Arg::object(icon), Arg::uint(serial)], None, false) };
    }
    /// copy data to the selection
    ///
    /// # Safety
    /// `this` is a live `wl_data_device`; every object argument is live.
    pub unsafe fn set_selection(this: Proxy, source: Option<Proxy>, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::object(source), Arg::uint(serial)], None, false) };
    }
    /// destroy data device
    ///
    /// # Safety
    /// `this` is a live `wl_data_device`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [], None, true) };
    }
}

/// data transfer interface
pub mod wl_data_device_manager {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 4;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_data_device_manager", 4, &REQUESTS, &EVENTS);
    static REQUESTS_CREATE_DATA_SOURCE_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_data_source::INTERFACE),
    ];
    static REQUESTS_GET_DATA_DEVICE_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::wayland::wl_data_device::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
    ];
    static REQUESTS: [Message; 3] = [
        Message::new(c"create_data_source", c"n", &REQUESTS_CREATE_DATA_SOURCE_TYPES),
        Message::new(c"get_data_device", c"no", &REQUESTS_GET_DATA_DEVICE_TYPES),
        Message::new(c"release", c"4", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const CREATE_DATA_SOURCE: u32 = 0;
        pub const GET_DATA_DEVICE: u32 = 1;
        pub const RELEASE: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `dnd_action` enum.
    pub mod dnd_action {
        /// no action
        pub const NONE: u32 = 0x0;
        /// copy action
        pub const COPY: u32 = 0x1;
        /// move action
        pub const MOVE: u32 = 0x2;
        /// ask action
        pub const ASK: u32 = 0x4;
    }
    /// create a new data source
    ///
    /// # Safety
    /// `this` is a live `wl_data_device_manager`; every object argument is live.
    pub unsafe fn create_data_source(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_data_source::INTERFACE, this.version())), false) }
    }
    /// create a new data device
    ///
    /// # Safety
    /// `this` is a live `wl_data_device_manager`; every object argument is live.
    pub unsafe fn get_data_device(this: Proxy, seat: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id(), Arg::object(Some(seat))], Some((&crate::protocols::wayland::wl_data_device::INTERFACE, this.version())), false) }
    }
    /// destroy wl_data_device_manager
    ///
    /// # Safety
    /// `this` is a live `wl_data_device_manager`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [], None, true) };
    }
}

/// create desktop-style surfaces
pub mod wl_shell {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_shell", 1, &REQUESTS, &EVENTS);
    static REQUESTS_GET_SHELL_SURFACE_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::wayland::wl_shell_surface::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS: [Message; 1] = [
        Message::new(c"get_shell_surface", c"no", &REQUESTS_GET_SHELL_SURFACE_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const GET_SHELL_SURFACE: u32 = 0;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `error` enum.
    pub mod error {
        /// given wl_surface has another role
        pub const ROLE: u32 = 0x0;
    }
    /// create a shell surface from a surface
    ///
    /// # Safety
    /// `this` is a live `wl_shell`; every object argument is live.
    pub unsafe fn get_shell_surface(this: Proxy, surface: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::new_id(), Arg::object(Some(surface))], Some((&crate::protocols::wayland::wl_shell_surface::INTERFACE, this.version())), false) }
    }
}

/// desktop-style metadata interface
pub mod wl_shell_surface {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_shell_surface", 1, &REQUESTS, &EVENTS);
    static REQUESTS_MOVE_TYPES: [TypeRef; 2] = [
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
        TypeRef::NULL,
    ];
    static REQUESTS_RESIZE_TYPES: [TypeRef; 3] = [
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS_SET_TRANSIENT_TYPES: [TypeRef; 4] = [
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS_SET_FULLSCREEN_TYPES: [TypeRef; 3] = [
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_output::INTERFACE),
    ];
    static REQUESTS_SET_POPUP_TYPES: [TypeRef; 6] = [
        TypeRef::to(&crate::protocols::wayland::wl_seat::INTERFACE),
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS_SET_MAXIMIZED_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_output::INTERFACE),
    ];
    static REQUESTS: [Message; 10] = [
        Message::new(c"pong", c"u", &NULLS),
        Message::new(c"move", c"ou", &REQUESTS_MOVE_TYPES),
        Message::new(c"resize", c"ouu", &REQUESTS_RESIZE_TYPES),
        Message::new(c"set_toplevel", c"", &NULLS),
        Message::new(c"set_transient", c"oiiu", &REQUESTS_SET_TRANSIENT_TYPES),
        Message::new(c"set_fullscreen", c"uu?o", &REQUESTS_SET_FULLSCREEN_TYPES),
        Message::new(c"set_popup", c"ouoiiu", &REQUESTS_SET_POPUP_TYPES),
        Message::new(c"set_maximized", c"?o", &REQUESTS_SET_MAXIMIZED_TYPES),
        Message::new(c"set_title", c"s", &NULLS),
        Message::new(c"set_class", c"s", &NULLS),
    ];
    static EVENTS: [Message; 3] = [
        Message::new(c"ping", c"u", &NULLS),
        Message::new(c"configure", c"uii", &NULLS),
        Message::new(c"popup_done", c"", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const PONG: u32 = 0;
        pub const MOVE: u32 = 1;
        pub const RESIZE: u32 = 2;
        pub const SET_TOPLEVEL: u32 = 3;
        pub const SET_TRANSIENT: u32 = 4;
        pub const SET_FULLSCREEN: u32 = 5;
        pub const SET_POPUP: u32 = 6;
        pub const SET_MAXIMIZED: u32 = 7;
        pub const SET_TITLE: u32 = 8;
        pub const SET_CLASS: u32 = 9;
    }
    /// Event opcodes.
    pub mod evt {
        pub const PING: u32 = 0;
        pub const CONFIGURE: u32 = 1;
        pub const POPUP_DONE: u32 = 2;
    }
    /// The `resize` enum.
    pub mod resize {
        /// no edge
        pub const NONE: u32 = 0x0;
        /// top edge
        pub const TOP: u32 = 0x1;
        /// bottom edge
        pub const BOTTOM: u32 = 0x2;
        /// left edge
        pub const LEFT: u32 = 0x4;
        /// top and left edges
        pub const TOP_LEFT: u32 = 0x5;
        /// bottom and left edges
        pub const BOTTOM_LEFT: u32 = 0x6;
        /// right edge
        pub const RIGHT: u32 = 0x8;
        /// top and right edges
        pub const TOP_RIGHT: u32 = 0x9;
        /// bottom and right edges
        pub const BOTTOM_RIGHT: u32 = 0xa;
    }
    /// The `transient` enum.
    pub mod transient {
        /// do not set keyboard focus
        pub const INACTIVE: u32 = 0x1;
    }
    /// The `fullscreen_method` enum.
    pub mod fullscreen_method {
        /// no preference, apply default policy
        pub const DEFAULT: u32 = 0x0;
        /// scale, preserve the surface's aspect ratio and center on output
        pub const SCALE: u32 = 0x1;
        /// switch output mode to the smallest mode that can fit the surface, add black borders to compensate size mismatch
        pub const DRIVER: u32 = 0x2;
        /// no upscaling, center on output and add black borders to compensate size mismatch
        pub const FILL: u32 = 0x3;
    }
    /// respond to a ping event
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn pong(this: Proxy, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::uint(serial)], None, false) };
    }
    /// start an interactive move
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn r#move(this: Proxy, seat: Proxy, serial: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::object(Some(seat)), Arg::uint(serial)], None, false) };
    }
    /// start an interactive resize
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn resize(this: Proxy, seat: Proxy, serial: u32, edges: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::object(Some(seat)), Arg::uint(serial), Arg::uint(edges)], None, false) };
    }
    /// make the surface a toplevel surface
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn set_toplevel(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [], None, false) };
    }
    /// make the surface a transient surface
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn set_transient(this: Proxy, parent: Proxy, x: i32, y: i32, flags: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [Arg::object(Some(parent)), Arg::int(x), Arg::int(y), Arg::uint(flags)], None, false) };
    }
    /// make the surface a fullscreen surface
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn set_fullscreen(this: Proxy, method: u32, framerate: u32, output: Option<Proxy>) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(5, &mut [Arg::uint(method), Arg::uint(framerate), Arg::object(output)], None, false) };
    }
    /// make the surface a popup surface
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn set_popup(this: Proxy, seat: Proxy, serial: u32, parent: Proxy, x: i32, y: i32, flags: u32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(6, &mut [Arg::object(Some(seat)), Arg::uint(serial), Arg::object(Some(parent)), Arg::int(x), Arg::int(y), Arg::uint(flags)], None, false) };
    }
    /// make the surface a maximized surface
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn set_maximized(this: Proxy, output: Option<Proxy>) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(7, &mut [Arg::object(output)], None, false) };
    }
    /// set surface title
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn set_title(this: Proxy, title: &CStr) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(8, &mut [Arg::string(Some(title))], None, false) };
    }
    /// set surface class
    ///
    /// # Safety
    /// `this` is a live `wl_shell_surface`; every object argument is live.
    pub unsafe fn set_class(this: Proxy, class_: &CStr) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(9, &mut [Arg::string(Some(class_))], None, false) };
    }
}

/// an onscreen surface
pub mod wl_surface {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 7;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_surface", 7, &REQUESTS, &EVENTS);
    static REQUESTS_ATTACH_TYPES: [TypeRef; 3] = [
        TypeRef::to(&crate::protocols::wayland::wl_buffer::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS_FRAME_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_callback::INTERFACE),
    ];
    static REQUESTS_SET_OPAQUE_REGION_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_region::INTERFACE),
    ];
    static REQUESTS_SET_INPUT_REGION_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_region::INTERFACE),
    ];
    static REQUESTS_GET_RELEASE_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_callback::INTERFACE),
    ];
    static REQUESTS: [Message; 12] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"attach", c"?oii", &REQUESTS_ATTACH_TYPES),
        Message::new(c"damage", c"iiii", &NULLS),
        Message::new(c"frame", c"n", &REQUESTS_FRAME_TYPES),
        Message::new(c"set_opaque_region", c"?o", &REQUESTS_SET_OPAQUE_REGION_TYPES),
        Message::new(c"set_input_region", c"?o", &REQUESTS_SET_INPUT_REGION_TYPES),
        Message::new(c"commit", c"", &NULLS),
        Message::new(c"set_buffer_transform", c"2i", &NULLS),
        Message::new(c"set_buffer_scale", c"3i", &NULLS),
        Message::new(c"damage_buffer", c"4iiii", &NULLS),
        Message::new(c"offset", c"5ii", &NULLS),
        Message::new(c"get_release", c"7n", &REQUESTS_GET_RELEASE_TYPES),
    ];
    static EVENTS_ENTER_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_output::INTERFACE),
    ];
    static EVENTS_LEAVE_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_output::INTERFACE),
    ];
    static EVENTS: [Message; 4] = [
        Message::new(c"enter", c"o", &EVENTS_ENTER_TYPES),
        Message::new(c"leave", c"o", &EVENTS_LEAVE_TYPES),
        Message::new(c"preferred_buffer_scale", c"6i", &NULLS),
        Message::new(c"preferred_buffer_transform", c"6u", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const ATTACH: u32 = 1;
        pub const DAMAGE: u32 = 2;
        pub const FRAME: u32 = 3;
        pub const SET_OPAQUE_REGION: u32 = 4;
        pub const SET_INPUT_REGION: u32 = 5;
        pub const COMMIT: u32 = 6;
        pub const SET_BUFFER_TRANSFORM: u32 = 7;
        pub const SET_BUFFER_SCALE: u32 = 8;
        pub const DAMAGE_BUFFER: u32 = 9;
        pub const OFFSET: u32 = 10;
        pub const GET_RELEASE: u32 = 11;
    }
    /// Event opcodes.
    pub mod evt {
        pub const ENTER: u32 = 0;
        pub const LEAVE: u32 = 1;
        pub const PREFERRED_BUFFER_SCALE: u32 = 2;
        pub const PREFERRED_BUFFER_TRANSFORM: u32 = 3;
    }
    /// The `error` enum.
    pub mod error {
        /// buffer scale value is invalid
        pub const INVALID_SCALE: u32 = 0x0;
        /// buffer transform value is invalid
        pub const INVALID_TRANSFORM: u32 = 0x1;
        /// buffer size is invalid
        pub const INVALID_SIZE: u32 = 0x2;
        /// buffer offset is invalid
        pub const INVALID_OFFSET: u32 = 0x3;
        /// surface was destroyed before its role object
        pub const DEFUNCT_ROLE_OBJECT: u32 = 0x4;
        /// no buffer was attached
        pub const NO_BUFFER: u32 = 0x5;
    }
    /// delete surface
    ///
    /// # Safety
    /// `this` is a live `wl_surface`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// set the surface contents
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn attach(this: Proxy, buffer: Option<Proxy>, x: i32, y: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::object(buffer), Arg::int(x), Arg::int(y)], None, false) };
    }
    /// mark part of the surface damaged
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn damage(this: Proxy, x: i32, y: i32, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::int(x), Arg::int(y), Arg::int(width), Arg::int(height)], None, false) };
    }
    /// request a frame throttling hint
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn frame(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(3, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_callback::INTERFACE, this.version())), false) }
    }
    /// set opaque region
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn set_opaque_region(this: Proxy, region: Option<Proxy>) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [Arg::object(region)], None, false) };
    }
    /// set input region
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn set_input_region(this: Proxy, region: Option<Proxy>) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(5, &mut [Arg::object(region)], None, false) };
    }
    /// commit pending surface state
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn commit(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(6, &mut [], None, false) };
    }
    /// sets the buffer transformation
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn set_buffer_transform(this: Proxy, transform: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(7, &mut [Arg::int(transform)], None, false) };
    }
    /// sets the buffer scaling factor
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn set_buffer_scale(this: Proxy, scale: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(8, &mut [Arg::int(scale)], None, false) };
    }
    /// mark part of the surface damaged using buffer coordinates
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn damage_buffer(this: Proxy, x: i32, y: i32, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(9, &mut [Arg::int(x), Arg::int(y), Arg::int(width), Arg::int(height)], None, false) };
    }
    /// set the surface contents offset
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn offset(this: Proxy, x: i32, y: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(10, &mut [Arg::int(x), Arg::int(y)], None, false) };
    }
    /// get a release callback
    ///
    /// # Safety
    /// `this` is a live `wl_surface`; every object argument is live.
    pub unsafe fn get_release(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(11, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_callback::INTERFACE, this.version())), false) }
    }
}

/// group of input devices
pub mod wl_seat {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 10;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_seat", 10, &REQUESTS, &EVENTS);
    static REQUESTS_GET_POINTER_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_pointer::INTERFACE),
    ];
    static REQUESTS_GET_KEYBOARD_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_keyboard::INTERFACE),
    ];
    static REQUESTS_GET_TOUCH_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_touch::INTERFACE),
    ];
    static REQUESTS: [Message; 4] = [
        Message::new(c"get_pointer", c"n", &REQUESTS_GET_POINTER_TYPES),
        Message::new(c"get_keyboard", c"n", &REQUESTS_GET_KEYBOARD_TYPES),
        Message::new(c"get_touch", c"n", &REQUESTS_GET_TOUCH_TYPES),
        Message::new(c"release", c"5", &NULLS),
    ];
    static EVENTS: [Message; 2] = [
        Message::new(c"capabilities", c"u", &NULLS),
        Message::new(c"name", c"2s", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const GET_POINTER: u32 = 0;
        pub const GET_KEYBOARD: u32 = 1;
        pub const GET_TOUCH: u32 = 2;
        pub const RELEASE: u32 = 3;
    }
    /// Event opcodes.
    pub mod evt {
        pub const CAPABILITIES: u32 = 0;
        pub const NAME: u32 = 1;
    }
    /// The `capability` enum.
    pub mod capability {
        /// the seat has pointer devices
        pub const POINTER: u32 = 0x1;
        /// the seat has one or more keyboards
        pub const KEYBOARD: u32 = 0x2;
        /// the seat has touch devices
        pub const TOUCH: u32 = 0x4;
    }
    /// The `error` enum.
    pub mod error {
        /// get_pointer, get_keyboard or get_touch called on seat without the matching capability
        pub const MISSING_CAPABILITY: u32 = 0x0;
    }
    /// return pointer object
    ///
    /// # Safety
    /// `this` is a live `wl_seat`; every object argument is live.
    pub unsafe fn get_pointer(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(0, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_pointer::INTERFACE, this.version())), false) }
    }
    /// return keyboard object
    ///
    /// # Safety
    /// `this` is a live `wl_seat`; every object argument is live.
    pub unsafe fn get_keyboard(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_keyboard::INTERFACE, this.version())), false) }
    }
    /// return touch object
    ///
    /// # Safety
    /// `this` is a live `wl_seat`; every object argument is live.
    pub unsafe fn get_touch(this: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(2, &mut [Arg::new_id()], Some((&crate::protocols::wayland::wl_touch::INTERFACE, this.version())), false) }
    }
    /// release the seat object
    ///
    /// # Safety
    /// `this` is a live `wl_seat`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [], None, true) };
    }
}

/// pointer input device
pub mod wl_pointer {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 10;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_pointer", 10, &REQUESTS, &EVENTS);
    static REQUESTS_SET_CURSOR_TYPES: [TypeRef; 4] = [
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"set_cursor", c"u?oii", &REQUESTS_SET_CURSOR_TYPES),
        Message::new(c"release", c"3", &NULLS),
    ];
    static EVENTS_ENTER_TYPES: [TypeRef; 4] = [
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static EVENTS_LEAVE_TYPES: [TypeRef; 2] = [
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static EVENTS: [Message; 11] = [
        Message::new(c"enter", c"uoff", &EVENTS_ENTER_TYPES),
        Message::new(c"leave", c"uo", &EVENTS_LEAVE_TYPES),
        Message::new(c"motion", c"uff", &NULLS),
        Message::new(c"button", c"uuuu", &NULLS),
        Message::new(c"axis", c"uuf", &NULLS),
        Message::new(c"frame", c"5", &NULLS),
        Message::new(c"axis_source", c"5u", &NULLS),
        Message::new(c"axis_stop", c"5uu", &NULLS),
        Message::new(c"axis_discrete", c"5ui", &NULLS),
        Message::new(c"axis_value120", c"8ui", &NULLS),
        Message::new(c"axis_relative_direction", c"9uu", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const SET_CURSOR: u32 = 0;
        pub const RELEASE: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
        pub const ENTER: u32 = 0;
        pub const LEAVE: u32 = 1;
        pub const MOTION: u32 = 2;
        pub const BUTTON: u32 = 3;
        pub const AXIS: u32 = 4;
        pub const FRAME: u32 = 5;
        pub const AXIS_SOURCE: u32 = 6;
        pub const AXIS_STOP: u32 = 7;
        pub const AXIS_DISCRETE: u32 = 8;
        pub const AXIS_VALUE120: u32 = 9;
        pub const AXIS_RELATIVE_DIRECTION: u32 = 10;
    }
    /// The `error` enum.
    pub mod error {
        /// given wl_surface has another role
        pub const ROLE: u32 = 0x0;
    }
    /// The `button_state` enum.
    pub mod button_state {
        /// the button is not pressed
        pub const RELEASED: u32 = 0x0;
        /// the button is pressed
        pub const PRESSED: u32 = 0x1;
    }
    /// The `axis` enum.
    pub mod axis {
        /// vertical axis
        pub const VERTICAL_SCROLL: u32 = 0x0;
        /// horizontal axis
        pub const HORIZONTAL_SCROLL: u32 = 0x1;
    }
    /// The `axis_source` enum.
    pub mod axis_source {
        /// a physical wheel rotation
        pub const WHEEL: u32 = 0x0;
        /// finger on a touch surface
        pub const FINGER: u32 = 0x1;
        /// continuous coordinate space
        pub const CONTINUOUS: u32 = 0x2;
        /// a physical wheel tilt
        pub const WHEEL_TILT: u32 = 0x3;
    }
    /// The `axis_relative_direction` enum.
    pub mod axis_relative_direction {
        /// physical motion matches axis direction
        pub const IDENTICAL: u32 = 0x0;
        /// physical motion is the inverse of the axis direction
        pub const INVERTED: u32 = 0x1;
    }
    /// set the pointer surface
    ///
    /// # Safety
    /// `this` is a live `wl_pointer`; every object argument is live.
    pub unsafe fn set_cursor(this: Proxy, serial: u32, surface: Option<Proxy>, hotspot_x: i32, hotspot_y: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [Arg::uint(serial), Arg::object(surface), Arg::int(hotspot_x), Arg::int(hotspot_y)], None, false) };
    }
    /// release the pointer object
    ///
    /// # Safety
    /// `this` is a live `wl_pointer`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [], None, true) };
    }
}

/// keyboard input device
pub mod wl_keyboard {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 10;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_keyboard", 10, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 1] = [
        Message::new(c"release", c"3", &NULLS),
    ];
    static EVENTS_ENTER_TYPES: [TypeRef; 3] = [
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::NULL,
    ];
    static EVENTS_LEAVE_TYPES: [TypeRef; 2] = [
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static EVENTS: [Message; 6] = [
        Message::new(c"keymap", c"uhu", &NULLS),
        Message::new(c"enter", c"uoa", &EVENTS_ENTER_TYPES),
        Message::new(c"leave", c"uo", &EVENTS_LEAVE_TYPES),
        Message::new(c"key", c"uuuu", &NULLS),
        Message::new(c"modifiers", c"uuuuu", &NULLS),
        Message::new(c"repeat_info", c"4ii", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const RELEASE: u32 = 0;
    }
    /// Event opcodes.
    pub mod evt {
        pub const KEYMAP: u32 = 0;
        pub const ENTER: u32 = 1;
        pub const LEAVE: u32 = 2;
        pub const KEY: u32 = 3;
        pub const MODIFIERS: u32 = 4;
        pub const REPEAT_INFO: u32 = 5;
    }
    /// The `keymap_format` enum.
    pub mod keymap_format {
        /// no keymap; client must understand how to interpret the raw keycode
        pub const NO_KEYMAP: u32 = 0x0;
        /// libxkbcommon compatible, null-terminated string; to determine the xkb keycode, clients must add 8 to the key event keycode
        pub const XKB_V1: u32 = 0x1;
    }
    /// The `key_state` enum.
    pub mod key_state {
        /// key is not pressed
        pub const RELEASED: u32 = 0x0;
        /// key is pressed
        pub const PRESSED: u32 = 0x1;
        /// key was repeated
        pub const REPEATED: u32 = 0x2;
    }
    /// release the keyboard object
    ///
    /// # Safety
    /// `this` is a live `wl_keyboard`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
}

/// touchscreen input device
pub mod wl_touch {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 10;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_touch", 10, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 1] = [
        Message::new(c"release", c"3", &NULLS),
    ];
    static EVENTS_DOWN_TYPES: [TypeRef; 6] = [
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::NULL,
        TypeRef::NULL,
        TypeRef::NULL,
    ];
    static EVENTS: [Message; 7] = [
        Message::new(c"down", c"uuoiff", &EVENTS_DOWN_TYPES),
        Message::new(c"up", c"uui", &NULLS),
        Message::new(c"motion", c"uiff", &NULLS),
        Message::new(c"frame", c"", &NULLS),
        Message::new(c"cancel", c"", &NULLS),
        Message::new(c"shape", c"6iff", &NULLS),
        Message::new(c"orientation", c"6if", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const RELEASE: u32 = 0;
    }
    /// Event opcodes.
    pub mod evt {
        pub const DOWN: u32 = 0;
        pub const UP: u32 = 1;
        pub const MOTION: u32 = 2;
        pub const FRAME: u32 = 3;
        pub const CANCEL: u32 = 4;
        pub const SHAPE: u32 = 5;
        pub const ORIENTATION: u32 = 6;
    }
    /// release the touch object
    ///
    /// # Safety
    /// `this` is a live `wl_touch`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
}

/// compositor output region
pub mod wl_output {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 4;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_output", 4, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 1] = [
        Message::new(c"release", c"3", &NULLS),
    ];
    static EVENTS: [Message; 6] = [
        Message::new(c"geometry", c"iiiiissi", &NULLS),
        Message::new(c"mode", c"uiii", &NULLS),
        Message::new(c"done", c"2", &NULLS),
        Message::new(c"scale", c"2i", &NULLS),
        Message::new(c"name", c"4s", &NULLS),
        Message::new(c"description", c"4s", &NULLS),
    ];
    /// Request opcodes.
    pub mod req {
        pub const RELEASE: u32 = 0;
    }
    /// Event opcodes.
    pub mod evt {
        pub const GEOMETRY: u32 = 0;
        pub const MODE: u32 = 1;
        pub const DONE: u32 = 2;
        pub const SCALE: u32 = 3;
        pub const NAME: u32 = 4;
        pub const DESCRIPTION: u32 = 5;
    }
    /// The `subpixel` enum.
    pub mod subpixel {
        /// unknown geometry
        pub const UNKNOWN: u32 = 0x0;
        /// no geometry
        pub const NONE: u32 = 0x1;
        /// horizontal RGB
        pub const HORIZONTAL_RGB: u32 = 0x2;
        /// horizontal BGR
        pub const HORIZONTAL_BGR: u32 = 0x3;
        /// vertical RGB
        pub const VERTICAL_RGB: u32 = 0x4;
        /// vertical BGR
        pub const VERTICAL_BGR: u32 = 0x5;
    }
    /// The `transform` enum.
    pub mod transform {
        /// no transform
        pub const NORMAL: u32 = 0x0;
        /// 90 degrees counter-clockwise
        pub const _90: u32 = 0x1;
        /// 180 degrees counter-clockwise
        pub const _180: u32 = 0x2;
        /// 270 degrees counter-clockwise
        pub const _270: u32 = 0x3;
        /// 180 degree flip around a vertical axis
        pub const FLIPPED: u32 = 0x4;
        /// flip and rotate 90 degrees counter-clockwise
        pub const FLIPPED_90: u32 = 0x5;
        /// flip and rotate 180 degrees counter-clockwise
        pub const FLIPPED_180: u32 = 0x6;
        /// flip and rotate 270 degrees counter-clockwise
        pub const FLIPPED_270: u32 = 0x7;
    }
    /// The `mode` enum.
    pub mod mode {
        /// indicates this is the current mode
        pub const CURRENT: u32 = 0x1;
        /// indicates this is the preferred mode
        pub const PREFERRED: u32 = 0x2;
    }
    /// release the output object
    ///
    /// # Safety
    /// `this` is a live `wl_output`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn release(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
}

/// region interface
pub mod wl_region {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 7;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_region", 7, &REQUESTS, &EVENTS);
    static REQUESTS: [Message; 3] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"add", c"iiii", &NULLS),
        Message::new(c"subtract", c"iiii", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const ADD: u32 = 1;
        pub const SUBTRACT: u32 = 2;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// destroy region
    ///
    /// # Safety
    /// `this` is a live `wl_region`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// add rectangle to region
    ///
    /// # Safety
    /// `this` is a live `wl_region`; every object argument is live.
    pub unsafe fn add(this: Proxy, x: i32, y: i32, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::int(x), Arg::int(y), Arg::int(width), Arg::int(height)], None, false) };
    }
    /// subtract rectangle from region
    ///
    /// # Safety
    /// `this` is a live `wl_region`; every object argument is live.
    pub unsafe fn subtract(this: Proxy, x: i32, y: i32, width: i32, height: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::int(x), Arg::int(y), Arg::int(width), Arg::int(height)], None, false) };
    }
}

/// sub-surface compositing
pub mod wl_subcompositor {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_subcompositor", 1, &REQUESTS, &EVENTS);
    static REQUESTS_GET_SUBSURFACE_TYPES: [TypeRef; 3] = [
        TypeRef::to(&crate::protocols::wayland::wl_subsurface::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"get_subsurface", c"noo", &REQUESTS_GET_SUBSURFACE_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const GET_SUBSURFACE: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `error` enum.
    pub mod error {
        /// the to-be sub-surface is invalid
        pub const BAD_SURFACE: u32 = 0x0;
        /// the to-be sub-surface parent is invalid
        pub const BAD_PARENT: u32 = 0x1;
    }
    /// unbind from the subcompositor interface
    ///
    /// # Safety
    /// `this` is a live `wl_subcompositor`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// give a surface the role sub-surface
    ///
    /// # Safety
    /// `this` is a live `wl_subcompositor`; every object argument is live.
    pub unsafe fn get_subsurface(this: Proxy, surface: Proxy, parent: Proxy) -> Option<Proxy> {
        // SAFETY: the caller's contract; the arguments follow the signature.
        unsafe { this.marshal(1, &mut [Arg::new_id(), Arg::object(Some(surface)), Arg::object(Some(parent))], Some((&crate::protocols::wayland::wl_subsurface::INTERFACE, this.version())), false) }
    }
}

/// sub-surface interface to a wl_surface
pub mod wl_subsurface {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_subsurface", 1, &REQUESTS, &EVENTS);
    static REQUESTS_PLACE_ABOVE_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS_PLACE_BELOW_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_surface::INTERFACE),
    ];
    static REQUESTS: [Message; 6] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"set_position", c"ii", &NULLS),
        Message::new(c"place_above", c"o", &REQUESTS_PLACE_ABOVE_TYPES),
        Message::new(c"place_below", c"o", &REQUESTS_PLACE_BELOW_TYPES),
        Message::new(c"set_sync", c"", &NULLS),
        Message::new(c"set_desync", c"", &NULLS),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const SET_POSITION: u32 = 1;
        pub const PLACE_ABOVE: u32 = 2;
        pub const PLACE_BELOW: u32 = 3;
        pub const SET_SYNC: u32 = 4;
        pub const SET_DESYNC: u32 = 5;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// The `error` enum.
    pub mod error {
        /// wl_surface is not a sibling or the parent
        pub const BAD_SURFACE: u32 = 0x0;
    }
    /// remove sub-surface interface
    ///
    /// # Safety
    /// `this` is a live `wl_subsurface`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// reposition the sub-surface
    ///
    /// # Safety
    /// `this` is a live `wl_subsurface`; every object argument is live.
    pub unsafe fn set_position(this: Proxy, x: i32, y: i32) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::int(x), Arg::int(y)], None, false) };
    }
    /// restack the sub-surface
    ///
    /// # Safety
    /// `this` is a live `wl_subsurface`; every object argument is live.
    pub unsafe fn place_above(this: Proxy, sibling: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(2, &mut [Arg::object(Some(sibling))], None, false) };
    }
    /// restack the sub-surface
    ///
    /// # Safety
    /// `this` is a live `wl_subsurface`; every object argument is live.
    pub unsafe fn place_below(this: Proxy, sibling: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(3, &mut [Arg::object(Some(sibling))], None, false) };
    }
    /// set sub-surface to synchronized mode
    ///
    /// # Safety
    /// `this` is a live `wl_subsurface`; every object argument is live.
    pub unsafe fn set_sync(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(4, &mut [], None, false) };
    }
    /// set sub-surface to desynchronized mode
    ///
    /// # Safety
    /// `this` is a live `wl_subsurface`; every object argument is live.
    pub unsafe fn set_desync(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(5, &mut [], None, false) };
    }
}

/// wayland protocol fixes
pub mod wl_fixes {
    #[allow(unused_imports)]
    use crate::iface::{Interface, Message, NULLS, TypeRef};
    #[allow(unused_imports)]
    use crate::proxy::{Arg, Fixed, Proxy, borrowed_array};
    #[allow(unused_imports)]
    use std::ffi::{CStr, c_int};

    /// The highest version this table describes.
    pub const VERSION: u32 = 1;
    /// The interface table.
    pub static INTERFACE: Interface = Interface::new(c"wl_fixes", 1, &REQUESTS, &EVENTS);
    static REQUESTS_DESTROY_REGISTRY_TYPES: [TypeRef; 1] = [
        TypeRef::to(&crate::protocols::wayland::wl_registry::INTERFACE),
    ];
    static REQUESTS: [Message; 2] = [
        Message::new(c"destroy", c"", &NULLS),
        Message::new(c"destroy_registry", c"o", &REQUESTS_DESTROY_REGISTRY_TYPES),
    ];
    static EVENTS: [Message; 0] = [
    ];
    /// Request opcodes.
    pub mod req {
        pub const DESTROY: u32 = 0;
        pub const DESTROY_REGISTRY: u32 = 1;
    }
    /// Event opcodes.
    pub mod evt {
    }
    /// destroys this object
    ///
    /// # Safety
    /// `this` is a live `wl_fixes`, destroyed by this request and not used again; every object argument is live.
    pub unsafe fn destroy(this: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(0, &mut [], None, true) };
    }
    /// destroy a wl_registry
    ///
    /// # Safety
    /// `this` is a live `wl_fixes`; every object argument is live.
    pub unsafe fn destroy_registry(this: Proxy, registry: Proxy) {
        // SAFETY: the caller's contract; the arguments follow the signature.
        let _ = unsafe { this.marshal(1, &mut [Arg::object(Some(registry))], None, false) };
    }
}
}
