// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metal, straight over the Objective-C runtime.
//!
//! An offscreen render-and-compute FFI: device, command queue, MSL libraries,
//! render and compute pipeline states, buffers, textures, samplers, the
//! pipeline and encoder state the renderer sets, and one-shot encoders that
//! render into a texture and copy it back. Metal is an OS framework, so only
//! entry points are declared.
//!
//! There is no swapchain here: every destination is a texture this file
//! allocated or a drawable texture [`super::swapchain`] hands over, and the
//! QuartzCore link lives in that module.
//!
//! # Conventions
//!
//! Metal is Objective-C protocols; the only C function is
//! `MTLCreateSystemDefaultDevice`. So this module talks to the runtime
//! directly:
//!
//! * Every send casts `objc_msgSend` to a fully typed `"C-unwind"` prototype
//!   through [`checked_msg`] (`aterm_objc`'s): there is no variadic ABI for
//!   message sends on arm64, so a wrong prototype corrupts registers, and a
//!   debug build checks each send's prototype against the runtime's encoding
//!   of the method before sending (once per class, selector and prototype, so
//!   the per-frame encoder sends pay a hash probe). `"C-unwind"`, because an
//!   `NSException` Metal raises inside a send must unwind through this frame,
//!   not skip its destructors.
//! * Every `new*`/`alloc` result is +1 and lands in [`Obj`], the only place
//!   `objc_release` is called.
//! * Autoreleased returns (`colorAttachments`, `objectAtIndexedSubscript:`,
//!   vertex-descriptor accessors) are borrowed as bare [`Id`] and live until
//!   the enclosing [`AutoreleasePool`] pops. Every entry point that can
//!   produce one takes a pool, including ones returning +1, because the
//!   framework autoreleases privately (device creation, library compile,
//!   pipeline creation, queue and function lookup). The `error:` entry points
//!   also need it because the `NSError` out-param is autoreleased into the
//!   caller's frame.
//! * Releasing a Metal object also autoreleases (see [`Obj::drop`]); a caller
//!   holding one [`AutoreleasePool`] across its scope, as a render loop must,
//!   leaves no unpooled autoreleases.
//!
//! # Selector safety
//!
//! [`sel`] takes a NUL-terminated byte literal, so a missing NUL is a compile
//! error. An unknown selector raises "unrecognized selector" on send, which
//! aborts; the tests in `super` cover every selector sent here.

use std::ffi::{CStr, c_void};
use std::ptr;

/// The runtime's pointer-shaped types and the checked send, `aterm_objc`'s:
/// an object pointer (null is `nil`, always a valid receiver), an interned
/// selector, a class object, and the typed `objc_msgSend` cast every send in
/// this crate goes through.
pub(crate) use aterm_objc::{ClassPtr, Encode, Id, Sel, checked_msg};

/// The Objective-C `BOOL` for this target, used for every `BOOL` in a send
/// prototype: `aterm_objc::Bool`, `bool` on arm64 and `i8` (`signed char`) on
/// x86_64, read as `!= 0`.
///
/// Rust's `bool` is wrong on x86_64: it may assume the byte is 0 or 1, and
/// some `BOOL` getters return a stored non-0/1 byte after it was set, which
/// would be undefined behaviour through a `bool` slot. On arm64 this is `bool`,
/// so codegen is unchanged there.
use aterm_objc::Bool as ObjcBool;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_retain(obj: Id) -> Id;
    fn objc_release(obj: Id);
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    /// Every Metal device, as a +1 `NSArray<id<MTLDevice>>`; elements are +0
    /// until retained. (`MTLCreateSystemDefaultDevice` is declared inside its
    /// one caller, [`window_server_door`].)
    fn MTLCopyAllDevices() -> Id;
}

/// Resolve a selector from a checked C-string literal, e.g. `sel(c"length")`.
#[inline]
pub(crate) fn sel(name: &'static CStr) -> Sel {
    aterm_objc::sel_uncached(name)
}

/// Look up a class from a checked C-string literal, e.g. `class(c"NSString")`.
#[inline]
pub(crate) fn class(name: &'static CStr) -> ClassPtr {
    aterm_objc::class(name)
}

/// An owned (+1) Objective-C object; the only place `objc_release` is called.
#[derive(Debug)]
pub(crate) struct Obj(Id);

impl Obj {
    /// Adopt a +1 reference (an `alloc`/`new*`/`copy*` return).
    ///
    /// # Safety
    /// `id` must be a +1 reference the caller is handing over, or null.
    pub(crate) const unsafe fn from_owned(id: Id) -> Option<Self> {
        if id.is_null() { None } else { Some(Self(id)) }
    }

    /// Retain a BORROWED reference (an autoreleased or +0 return) to +1.
    ///
    /// # Safety
    /// `id` must be a live object pointer, or null.
    pub(crate) unsafe fn retain(id: Id) -> Option<Self> {
        if id.is_null() {
            return None;
        }
        // SAFETY: caller pins `id` as live; `objc_retain` returns the same
        // pointer at +1, which `Drop` balances exactly once.
        Some(Self(unsafe { objc_retain(id) }))
    }

    #[inline]
    pub(crate) const fn id(&self) -> Id {
        self.0
    }

    /// A second +1 handle to the same object (`objc_retain`). Not a `Clone`
    /// impl, so every retain is visible at the call site.
    pub(crate) fn clone_retained(&self) -> Self {
        // SAFETY: `self.0` is non-null and live by construction (this holder
        // owns a +1 reference); `objc_retain` is thread-safe.
        unsafe { Self::retain(self.0).expect("retaining a live non-null object") }
    }
}

impl Drop for Obj {
    /// Releasing a Metal object runs the driver's dealloc path, which
    /// autoreleases one object into the owner's current frame. `drop` does not
    /// push its own pool: a render loop already wraps each frame in a pool, so
    /// the caller owns this obligation.
    fn drop(&mut self) {
        // SAFETY: `self.0` is non-null by construction (both constructors
        // reject null) and holds exactly one +1 reference, released once here.
        unsafe { objc_release(self.0) }
    }
}

/// An `@autoreleasepool` scope. Without a pool, Metal's autoreleased returns
/// accumulate for the life of the thread.
///
/// Pools must be dropped innermost first; `!Send`/`!Clone` does not enforce
/// that (`drop(outer); drop(inner);` aborts on the stale token). It holds here
/// only because every use is a scope-guard `let _pool = …` inside this private
/// module. `aterm-objc` offers the enforced `autoreleasepool(|_| …)` form.
pub(crate) struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    pub(crate) fn new() -> Self {
        // SAFETY: push/pop are the documented runtime entry points and are
        // balanced by `Drop`; the token is opaque and only handed back to pop.
        Self(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the token from the matching push, and this is the
        // innermost live pool (every holder is a scope-guard local).
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

/// Build a +1 `NSString` from a Rust `&str` via `alloc` +
/// `initWithBytes:length:encoding:`, so it is owned rather than autoreleased.
pub(crate) fn ns_string(s: &str) -> Option<Obj> {
    const NS_UTF8: usize = 4;
    let cls = class(c"NSString");
    // SAFETY: `alloc` returns a +1 uninitialized instance that
    // `initWithBytes:length:encoding:` consumes, returning +1 or nil. The byte
    // pointer is read for `len` bytes during the call only.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
        let raw = alloc(cls, sel(c"alloc"));
        if raw.is_null() {
            return None;
        }
        let init: unsafe extern "C-unwind" fn(Id, Sel, *const u8, usize, usize) -> Id =
            checked_msg();
        let obj = init(
            raw,
            sel(c"initWithBytes:length:encoding:"),
            s.as_ptr(),
            s.len(),
            NS_UTF8,
        );
        Obj::from_owned(obj)
    }
}

/// Copy an `NSString` (or a toll-free bridged `CFString`) into an owned Rust
/// `String` by its counted UTF-16 code units: aterm-objc's
/// [`aterm_objc::ns_string_to_rust`], the one reader every NSString read in
/// aterm goes through. Never `-UTF8String`, which stops at the first U+0000
/// and answers NULL for a string holding an unpaired surrogate (that one
/// becomes U+FFFD here; everything else is exact).
///
/// # Safety
/// `s` must be a live `NSString`, or nil (an empty string).
pub(crate) unsafe fn ns_string_text(s: Id) -> String {
    // SAFETY: the caller's contract is the shared reader's.
    unsafe { aterm_objc::ns_string_to_rust(s) }
}

/// Read an `NSError`'s `localizedDescription` into an owned Rust `String`
/// by its counted UTF-16 units ([`aterm_objc::ns_error_string`]);
/// `"(nil NSError)"` for nil.
pub(crate) fn ns_error_string(err: Id) -> String {
    // SAFETY: every `err` handed here is a live `NSError` (a Metal send's
    // out-param or a command buffer's `-error`, alive in the caller's pool)
    // or nil, which the reader reports as `"(nil NSError)"`.
    unsafe { aterm_objc::ns_error_string(err) }
}

// ---------------------------------------------------------------------------
// Enumerations, with values from the Metal headers. Every one used here is
// exercised by a `super` test.
// ---------------------------------------------------------------------------

/// `MTLPixelFormat`. Only the formats aterm actually asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum PixelFormat {
    R8Unorm = 10,
    Rgba8Unorm = 70,
    /// The sRGB-typed VIEW of an `Rgba8Unorm` texture — see [`super::shaders`].
    Rgba8UnormSrgb = 71,
    Bgra8Unorm = 80,
    /// The sRGB-typed VIEW of a `Bgra8Unorm` texture.
    Bgra8UnormSrgb = 81,
    Rgba16Float = 115,
}

impl PixelFormat {
    /// Bytes one texel occupies in a linear (buffer) layout; readback row
    /// strides derive from it (a wrong stride copies silently with Metal
    /// validation off).
    pub(crate) const fn bytes_per_texel(self) -> usize {
        match self {
            Self::R8Unorm => 1,
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Bgra8Unorm | Self::Bgra8UnormSrgb => 4,
            Self::Rgba16Float => 8,
        }
    }

    /// Recover a format from an `MTLPixelFormat` raw value (e.g. `-pixelFormat`
    /// off a live texture). `None` for formats not modelled here, whose row
    /// stride cannot be checked.
    pub(crate) const fn from_raw(raw: usize) -> Option<Self> {
        match raw {
            10 => Some(Self::R8Unorm),
            70 => Some(Self::Rgba8Unorm),
            71 => Some(Self::Rgba8UnormSrgb),
            80 => Some(Self::Bgra8Unorm),
            81 => Some(Self::Bgra8UnormSrgb),
            115 => Some(Self::Rgba16Float),
            _ => None,
        }
    }
}

/// `MTLVertexFormat`, for the five instance layouts in `cell.metal` /
/// `hdr_glow.metal`.
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub(crate) enum VertexFormat {
    /// `Uint8x4` — the fire `tsl` bytes and the glow colour pair.
    UChar4 = 3,
    /// `Unorm8x4` — every packed RGBA colour.
    UChar4Normalized = 9,
    /// `Uint16x4` — pixel rects, rain falloff, fire geometry.
    UShort4 = 15,
    /// `Float32x4` — glyph rect and UV.
    Float4 = 31,
    /// `Uint32` — the fire churn phase, the fx palette base and flags.
    UInt = 36,
    /// `Uint16x2` — the fx sprite's destination size.
    UShort2 = 13,
    /// `Sint16x2` — the fx item origin.
    Short2 = 16,
}

/// `MTLBlendFactor`: exactly the factors `crate::pipeline_table::Factor`
/// declares, mapped exhaustively in `crate::metal::pipelines`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum BlendFactor {
    Zero = 0,
    One = 1,
    /// `MTLBlendFactorOneMinusSourceColor`, the screen operator's destination
    /// factor (`pipeline_table::Blend::SCREEN`) used by the bloom composite and
    /// the SDR crown.
    OneMinusSourceColor = 3,
    SourceAlpha = 4,
    OneMinusSourceAlpha = 5,
}

/// `MTLBlendOperation`, separate from [`BlendFactor`] so a table blend maps
/// its operation explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum BlendOperation {
    Add = 0,
}

/// One Metal colour attachment's complete fixed-function blend equation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlendState {
    pub(crate) source_rgb: BlendFactor,
    pub(crate) destination_rgb: BlendFactor,
    pub(crate) rgb_operation: BlendOperation,
    pub(crate) source_alpha: BlendFactor,
    pub(crate) destination_alpha: BlendFactor,
    pub(crate) alpha_operation: BlendOperation,
}

/// `MTLColorWriteMask`: which channels a colour attachment may write.
///
/// The bit order is not the channel order (alpha is bit 0, red bit 3), so each
/// value is spelled from the header, and
/// [`super::tests::color_write_mask_gates_each_channel_on_the_gpu`] pins all
/// four on the GPU. Metal's default is [`Self::ALL`], so every table row states
/// its mask (`crate::metal::pipelines::metal_write_mask` is the only mapping).
/// Under the default, an additive effect quad emitting alpha 1.0 would drive a
/// translucent window's alpha opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColorWriteMask(usize);

impl ColorWriteMask {
    #[cfg(test)]
    pub(crate) const NONE: Self = Self(0);
    #[cfg(test)]
    pub(crate) const RED: Self = Self(1 << 3);
    #[cfg(test)]
    pub(crate) const GREEN: Self = Self(1 << 2);
    #[cfg(test)]
    pub(crate) const BLUE: Self = Self(1 << 1);
    #[cfg(test)]
    pub(crate) const ALPHA: Self = Self(1);
    /// `wgpu::ColorWrites::COLOR` — RGB only, alpha left exactly as loaded.
    pub(crate) const COLOR: Self = Self((1 << 3) | (1 << 2) | (1 << 1));
    /// `wgpu::ColorWrites::ALL`, and Metal's default.
    pub(crate) const ALL: Self = Self(0xf);

    #[inline]
    const fn bits(self) -> usize {
        self.0
    }
}

impl std::ops::BitOr for ColorWriteMask {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// `MTLSamplerMinMagFilter`. The glyph atlas sampler must be nearest; the
/// bloom, tray and shimmer samplers are linear (shimmer samples sub-texel
/// displacements).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum SamplerFilter {
    Nearest = 0,
    Linear = 1,
}

/// `MTLSamplerMipFilter`, separate from [`SamplerFilter`] because "no mipmap
/// chain" is a third state; every aterm texture is single-level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum SamplerMipFilter {
    NotMipmapped = 0,
}

/// `MTLSamplerAddressMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum SamplerAddressMode {
    ClampToEdge = 0,
}

/// Everything [`Device::new_sampler`] sets, so the two combinations the
/// renderer uses (`NEAREST_CLAMP`, `LINEAR_CLAMP`) are named once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SamplerDesc {
    pub(crate) min: SamplerFilter,
    pub(crate) mag: SamplerFilter,
    pub(crate) mip: SamplerMipFilter,
    pub(crate) s_address: SamplerAddressMode,
    pub(crate) t_address: SamplerAddressMode,
}

impl SamplerDesc {
    /// The glyph atlas and blit sampler: nearest, clamped (the bare
    /// `MTLSamplerDescriptor` defaults).
    pub(crate) const NEAREST_CLAMP: Self = Self {
        min: SamplerFilter::Nearest,
        mag: SamplerFilter::Nearest,
        mip: SamplerMipFilter::NotMipmapped,
        s_address: SamplerAddressMode::ClampToEdge,
        t_address: SamplerAddressMode::ClampToEdge,
    };

    /// The bloom/shimmer and tray sampler: linear min and mag, clamped.
    /// `NotMipmapped` matches the single-level textures (wgpu's `Nearest` mip
    /// filter is never consulted there).
    pub(crate) const LINEAR_CLAMP: Self = Self {
        min: SamplerFilter::Linear,
        mag: SamplerFilter::Linear,
        mip: SamplerMipFilter::NotMipmapped,
        s_address: SamplerAddressMode::ClampToEdge,
        t_address: SamplerAddressMode::ClampToEdge,
    };
}

/// `MTLLanguageVersion`, encoded by the header as `(major << 16) | minor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum LanguageVersion {
    V2_3 = (2 << 16) | 3,
}

/// `MTLResourceOptions`: `MTLResourceStorageModeShared` is `0 << 4`.
pub(crate) const RESOURCE_STORAGE_MODE_SHARED: usize = 0;
/// `MTLStorageModeManaged` (`1`; Shared 0, Private 2, Memoryless 3). The mode
/// every [`Device::new_texture_2d`] texture uses; its docs say why.
pub(crate) const TEXTURE_STORAGE_MODE_MANAGED: usize = 1;

/// `MTLTextureUsage` bits.
pub(crate) const TEXTURE_USAGE_SHADER_READ: usize = 1;
pub(crate) const TEXTURE_USAGE_RENDER_TARGET: usize = 4;
/// Required on any texture reinterpreted through
/// `newTextureViewWithPixelFormat:` (the Unorm/sRGB pair); Metal refuses the
/// view without it.
#[cfg(test)]
pub(crate) const TEXTURE_USAGE_PIXEL_FORMAT_VIEW: usize = 16;

// ---------------------------------------------------------------------------
// The object wrappers.
// ---------------------------------------------------------------------------

/// An `MTLCompileOptions`: how MSL source becomes an `MTLLibrary`. Both
/// options are set, as wgpu-hal does:
///
/// * `preserveInvariance`: otherwise each library may contract the
///   `[[position]]` arithmetic differently, and `cell.metal`'s `to_ndc` and
///   `hdr_glow.metal`'s inline NDC math compute the same quad corners (the HDR
///   crown re-emits aurora quads).
/// * `languageVersion`: pinned to [`LanguageVersion::V2_3`], the floor macOS 11
///   provides, so shaders are checked against the oldest compiler a shipped
///   copy meets rather than whatever the OS calls newest.
pub(crate) struct CompileOptions(Obj);

impl CompileOptions {
    /// A bare `MTLCompileOptions`, compiler defaults untouched.
    pub(crate) fn new() -> Option<Self> {
        let _pool = AutoreleasePool::new();
        // SAFETY: alloc/init is +1; `Obj` releases it exactly once on drop.
        unsafe {
            let cls = class(c"MTLCompileOptions");
            let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
            let raw = alloc(cls, sel(c"alloc"));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            Obj::from_owned(init(raw, sel(c"init"))).map(Self)
        }
    }

    /// The options every aterm library is compiled with — see the type docs.
    pub(crate) fn aterm_default() -> Option<Self> {
        let o = Self::new()?;
        o.set_preserve_invariance(true);
        o.set_language_version(LanguageVersion::V2_3);
        Some(o)
    }

    pub(crate) fn set_preserve_invariance(&self, on: bool) {
        // SAFETY: plain `BOOL` property write on a live `MTLCompileOptions`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, ObjcBool) = checked_msg();
            f(self.0.id(), sel(c"setPreserveInvariance:"), on.into());
        }
    }

    pub(crate) fn set_language_version(&self, v: LanguageVersion) {
        // SAFETY: plain `NSUInteger` property write on a live object.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            f(self.0.id(), sel(c"setLanguageVersion:"), v as usize);
        }
    }

    /// Read back, so a wrong setter PROTOTYPE (the failure mode `msg` exists to
    /// prevent) is a test failure rather than a silently ignored write.
    #[cfg(test)]
    pub(crate) fn preserve_invariance(&self) -> bool {
        // SAFETY: `-preserveInvariance` is a `BOOL` getter on a live object.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            f(self.0.id(), sel(c"preserveInvariance")).as_bool()
        }
    }

    /// Read back, as [`Self::preserve_invariance`].
    #[cfg(test)]
    pub(crate) fn language_version(&self) -> usize {
        // SAFETY: `-languageVersion` is an `NSUInteger` getter on a live object.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            f(self.0.id(), sel(c"languageVersion"))
        }
    }
}

/// An `MTLDevice`.
#[derive(Debug)]
pub(crate) struct Device(Obj);

/// The two facts device selection reads off one `MTLCopyAllDevices` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ListedGpu {
    /// `-isLowPower`: the integrated GPU of a dual-GPU Intel Mac; NO on Apple
    /// silicon.
    pub(crate) low_power: bool,
    /// `-isHeadless`: a GPU that cannot drive a display.
    pub(crate) headless: bool,
}

/// Which `MTLCopyAllDevices` entry [`Device::preferred`] renders on, and the
/// word its report uses for why, or `None` when only
/// `MTLCreateSystemDefaultDevice` (a WindowServer call) can decide. `None`
/// entries are nil slots and are never picked. Pure, so every shape is tested
/// (`listing_decides_every_shape_it_can`):
///
/// * not `high`: the first low-power display GPU, else the only display GPU
///   (which is the system default by construction);
/// * `high` (the development seam): the only display GPU. With two display
///   GPUs, `high` means the system default, which on a dual-GPU Mac also
///   switches the display mux, so it returns `None` and the system is asked;
/// * anything else (several indistinguishable candidates, or no display GPU)
///   is `None`: which one drives the display is the display server's fact.
pub(crate) fn choose_listed(
    gpus: &[Option<ListedGpu>],
    want_high: bool,
) -> Option<(usize, &'static str)> {
    /// The index of the one item `it` yields, or `None` for zero or several.
    fn sole(mut it: impl Iterator<Item = usize>) -> Option<usize> {
        match (it.next(), it.next()) {
            (Some(i), None) => Some(i),
            _ => None,
        }
    }
    let display = || {
        gpus.iter()
            .enumerate()
            .filter_map(|(i, g)| g.filter(|g| !g.headless).map(|g| (i, g)))
    };
    if !want_high && let Some((i, _)) = display().find(|(_, g)| g.low_power) {
        return Some((i, "low-power"));
    }
    sole(display().map(|(i, _)| i)).map(|i| (i, "the only display GPU"))
}

/// THE METAL DOOR: `MTLCreateSystemDefaultDevice`, the one Metal call that
/// makes this process a WindowServer client (WindowServer then runs a
/// synchronous TCC preflight of the binary on its main thread; a unit test that
/// made that call outran WindowServer's watchdog on 2026-09-01). It is declared
/// inside [`Device::system_default_asks_window_server`], which is private to
/// this module, so [`Device::preferred`] is its only caller: no other code in
/// the crate, test code included, can name either.
mod window_server_door {
    use super::{AutoreleasePool, Device, Id, Obj};

    impl Device {
        /// The GPU aterm renders on.
        ///
        /// A terminal needs no discrete GPU, and on a dual-GPU MacBook Pro the
        /// system default is the discrete one. Like the wgpu arm's
        /// `PowerPreference::LowPower`, this picks the first low-power, non-headless
        /// device `MTLCopyAllDevices` lists, unless the development seam
        /// `ATERM_GPU_POWER=high` asks otherwise. Per `MTLDevice.h`,
        /// `MTLCreateSystemDefaultDevice` switches an automatic-switching Mac to the
        /// high-power GPU while `MTLCopyAllDevices` does not, so the system default
        /// is not created when a low-power device exists.
        ///
        /// `MTLCreateSystemDefaultDevice` also makes the process a WindowServer
        /// client (WindowServer runs a tccd preflight on it); `MTLCopyAllDevices`
        /// does not. When the listing has exactly one display GPU, that GPU is the
        /// system default, so it is returned from the listing and headless
        /// processes (and test binaries) never touch WindowServer.
        ///
        /// The system default is created only when the listing cannot decide
        /// ([`choose_listed`](super::choose_listed)): several non-low-power
        /// display GPUs, none at all, or the `high` seam with two or more
        /// display GPUs.
        pub(crate) fn preferred() -> Option<Self> {
            let want_high = aterm_types::dev_seam!("ATERM_GPU_POWER")
                .is_some_and(|v| v.eq_ignore_ascii_case("high"));
            if let Some(listed) = Self::listed(want_high) {
                return Some(listed);
            }
            Self::system_default_asks_window_server()
        }

        /// The system default GPU (raw `MTLCreateSystemDefaultDevice`, the
        /// discrete GPU on a dual-GPU Mac), or `None` without a Metal device.
        fn system_default_asks_window_server() -> Option<Self> {
            #[link(name = "Metal", kind = "framework")]
            unsafe extern "C" {
                /// `CF_RETURNS_RETAINED`: the caller owns the +1 and must
                /// release it, which [`Device`] does on drop.
                fn MTLCreateSystemDefaultDevice() -> Id;
            }
            #[cfg(test)]
            super::tests::WINDOW_SERVER_ASKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Device creation autoreleases driver objects into the caller's
            // frame; the +1 device is owned and unaffected by the pop.
            let _pool = AutoreleasePool::new();
            // SAFETY: `MTLCreateSystemDefaultDevice` is `CF_RETURNS_RETAINED`,
            // so the +1 is ours and `Obj` releases it exactly once.
            unsafe { Obj::from_owned(MTLCreateSystemDefaultDevice()) }.map(Self)
        }
    }
}

impl Device {
    /// The GPU [`choose_listed`] picks from `MTLCopyAllDevices`, or `None`.
    /// Reports its pick once when the machine has more than one GPU.
    fn listed(want_high: bool) -> Option<Self> {
        static SAID: std::sync::Once = std::sync::Once::new();
        let _pool = AutoreleasePool::new();
        let (all, gpus) = Self::listing()?;
        let (i, why) = choose_listed(&gpus, want_high)?;
        // SAFETY: `-objectAtIndexedSubscript:` with `i < count`; the +0 element
        // is retained to +1 before `all` drops.
        let dev = unsafe {
            let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            Obj::retain(at(all.id(), sel(c"objectAtIndexedSubscript:"), i))
        }
        .map(Self)?;
        if gpus.len() > 1 {
            SAID.call_once(|| {
                crate::stderr_line!(
                    "aterm-gpu: {} Metal devices on this machine; rendering on {} ({why})",
                    gpus.len(),
                    dev.name()
                );
            });
        }
        Some(dev)
    }

    /// `MTLCopyAllDevices` (+1, owned) and, per element, the two facts
    /// [`choose_listed`] reads (`None` for a nil slot). `None` overall without
    /// Metal. Does not contact the display server.
    fn listing() -> Option<(Obj, Vec<Option<ListedGpu>>)> {
        // SAFETY: `MTLCopyAllDevices` is `CF_RETURNS_RETAINED`, so `Obj`
        // releases the array once. `-count` and `-objectAtIndexedSubscript:` are
        // NSArray getters; elements are +0 and only read. `-isLowPower` /
        // `-isHeadless` are `BOOL` getters read through [`ObjcBool`]. The caller
        // holds the pool.
        unsafe {
            let all = Obj::from_owned(MTLCopyAllDevices())?;
            let count: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            let flag: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            let gpus = (0..count(all.id(), sel(c"count")))
                .map(|i| {
                    let d = at(all.id(), sel(c"objectAtIndexedSubscript:"), i);
                    (!d.is_null()).then(|| ListedGpu {
                        low_power: flag(d, sel(c"isLowPower")).as_bool(),
                        headless: flag(d, sel(c"isHeadless")).as_bool(),
                    })
                })
                .collect();
            Some((all, gpus))
        }
    }

    #[inline]
    pub(crate) const fn id(&self) -> Id {
        self.0.id()
    }

    /// A second +1 reference to the same `MTLDevice`, for holders that outlive
    /// its creator.
    pub(crate) fn clone_ref(&self) -> Self {
        // SAFETY: retaining a live +1 object is always sound; the new `Obj`
        // releases its own +1 on drop.
        Self(unsafe { Obj::retain(self.0.id()) }.expect("retain of a live MTLDevice"))
    }

    /// The GPU's marketing name, for diagnostics.
    pub(crate) fn name(&self) -> String {
        let _pool = AutoreleasePool::new();
        // SAFETY: `-name` returns an autoreleased `NSString` (or nil) owned by
        // the pool above; it is copied out before the pool pops.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            ns_string_text(get(self.id(), sel(c"name")))
        }
    }

    /// Compile MSL source into an `MTLLibrary` with Metal's runtime compiler
    /// (no build-time Xcode dependency). A syntax error is a startup failure,
    /// so `super`'s tests compile every library.
    #[cfg(test)]
    pub(crate) fn new_library(&self, source: &str) -> Result<Library, String> {
        let opts = CompileOptions::aterm_default()
            .ok_or_else(|| "MTLCompileOptions allocation failed".to_owned())?;
        self.new_library_with_options(source, &opts)
    }

    /// `Self::new_library` with explicit compile options, so a test can compile
    /// against a different language version.
    pub(crate) fn new_library_with_options(
        &self,
        source: &str,
        options: &CompileOptions,
    ) -> Result<Library, String> {
        // Push the pool before the call: the `NSError` out-param is
        // autoreleased into the frame live when Metal writes it.
        let _pool = AutoreleasePool::new();
        let src = ns_string(source).ok_or_else(|| "NSString allocation failed".to_owned())?;
        let mut err: Id = Id::NIL;
        // SAFETY: `newLibraryWithSource:options:error:` is a `new*` family
        // method returning +1 (or nil with `err` set to an autoreleased
        // NSError). `options` is read during the call and not retained.
        let lib = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, Id, *mut Id) -> Id = checked_msg();
            f(
                self.id(),
                sel(c"newLibraryWithSource:options:error:"),
                src.id(),
                options.0.id(),
                &raw mut err,
            )
        };
        // SAFETY: `lib` is the +1 return described above, and outlives `_pool`
        // because it is owned rather than autoreleased.
        match unsafe { Obj::from_owned(lib) } {
            Some(o) => Ok(Library(o)),
            // Reads `err` while `_pool` is still live, which is what makes the
            // borrow valid.
            None => Err(ns_error_string(err)),
        }
    }

    /// Build an `MTLRenderPipelineState` from a configured descriptor.
    pub(crate) fn new_render_pipeline(
        &self,
        desc: &RenderPipelineDescriptor,
    ) -> Result<Obj, String> {
        // As in `new_library_with_options`: the pool precedes the call so the
        // `NSError` lands in it; pipeline construction also autoreleases driver
        // objects.
        let _pool = AutoreleasePool::new();
        let mut err: Id = Id::NIL;
        // SAFETY: `new*` family, +1 or nil-with-error. The descriptor is only
        // read during the call; Metal copies what it needs.
        let pso = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id, *mut Id) -> Id = checked_msg();
            f(
                self.id(),
                sel(c"newRenderPipelineStateWithDescriptor:error:"),
                desc.0.id(),
                &raw mut err,
            )
        };
        // SAFETY: `pso` is the +1 return described above.
        unsafe { Obj::from_owned(pso) }.ok_or_else(|| ns_error_string(err))
    }

    /// A shared-storage `MTLBuffer` of `len` bytes.
    pub(crate) fn new_buffer(&self, len: usize) -> Option<Obj> {
        // SAFETY: `new*` family, +1. Shared storage means the CPU may map it
        // via `-contents` for the lifetime of the buffer.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, usize, usize) -> Id = checked_msg();
            Obj::from_owned(f(
                self.id(),
                sel(c"newBufferWithLength:options:"),
                len,
                RESOURCE_STORAGE_MODE_SHARED,
            ))
        }
    }

    /// A 2-D `MTLTexture` in `MTLStorageModeManaged`, set explicitly although
    /// it is the macOS texture default. Consequences the SAFETY comments rely
    /// on:
    ///
    /// * Managed is non-Private, so `replaceRegion:` ([`texture_upload`]) is
    ///   legal. It is CPU-synchronous, outside command-queue hazard tracking;
    ///   callers must settle prior GPU users before modifying a resident texture.
    /// * Managed is not CPU-coherent: on a discrete GPU a CPU read of the system
    ///   copy after GPU writes is stale without `synchronizeResource:`. This
    ///   crate never calls `getBytes:`; every readback copies into a Shared
    ///   `MTLBuffer`, which reads the GPU copy. Keep it that way.
    /// * Shared textures are an Apple-GPU feature; Intel and AMD Macs refuse
    ///   them under validation, so they are not a portable choice.
    pub(crate) fn new_texture_2d(
        &self,
        format: PixelFormat,
        width: usize,
        height: usize,
        usage: usize,
    ) -> Option<Obj> {
        let _pool = AutoreleasePool::new();
        // SAFETY: the `+texture2DDescriptorWith...` factory returns an
        // autoreleased object, borrowed and left to the pool.
        // `newTextureWithDescriptor:` is +1 and is the only value that escapes.
        unsafe {
            let dcls = class(c"MTLTextureDescriptor");
            let mk: unsafe extern "C-unwind" fn(
                ClassPtr,
                Sel,
                usize,
                usize,
                usize,
                ObjcBool,
            ) -> Id = checked_msg();
            let d = mk(
                dcls,
                sel(c"texture2DDescriptorWithPixelFormat:width:height:mipmapped:"),
                format as usize,
                width,
                height,
                false.into(),
            );
            if d.is_null() {
                return None;
            }
            let set_usize: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            set_usize(d, sel(c"setUsage:"), usage);
            set_usize(d, sel(c"setStorageMode:"), TEXTURE_STORAGE_MODE_MANAGED);
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            Obj::from_owned(f(self.id(), sel(c"newTextureWithDescriptor:"), d))
        }
    }

    /// An `MTLSamplerState` with `desc`'s filters and address modes. Every
    /// field is written, including default values (see [`SamplerFilter`]).
    pub(crate) fn new_sampler(&self, desc: SamplerDesc) -> Option<Obj> {
        let _pool = AutoreleasePool::new();
        // SAFETY: `MTLSamplerDescriptor` is alloc/init (+1, released by `Obj`);
        // every setter is an `NSUInteger` property write;
        // `newSamplerStateWithDescriptor:` is +1 and escapes.
        unsafe {
            let dcls = class(c"MTLSamplerDescriptor");
            let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
            let raw = alloc(dcls, sel(c"alloc"));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let d = Obj::from_owned(init(raw, sel(c"init")))?;
            let set: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            set(d.id(), sel(c"setMinFilter:"), desc.min as usize);
            set(d.id(), sel(c"setMagFilter:"), desc.mag as usize);
            set(d.id(), sel(c"setMipFilter:"), desc.mip as usize);
            set(d.id(), sel(c"setSAddressMode:"), desc.s_address as usize);
            set(d.id(), sel(c"setTAddressMode:"), desc.t_address as usize);
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            Obj::from_owned(f(self.id(), sel(c"newSamplerStateWithDescriptor:"), d.id()))
        }
    }

    /// `-[MTLDevice maxBufferLength]`, the Metal equivalent of wgpu's
    /// `limits().max_buffer_size`. Read by the device layer's
    /// `max_buffer_size`, whose only production caller is the wgpu arm's
    /// stream growth, so it compiles where that arm does.
    #[cfg(wgpu_arm)]
    pub(crate) fn max_buffer_length(&self) -> usize {
        // SAFETY: `-maxBufferLength` is an `NSUInteger` getter on a live
        // `MTLDevice`.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            f(self.id(), sel(c"maxBufferLength"))
        }
    }

    /// An `MTLCommandQueue`.
    pub(crate) fn new_command_queue(&self) -> Option<Obj> {
        // `newCommandQueue` builds an `MTLCommandQueueDescriptorInternal` and
        // autoreleases it; the queue itself is +1 and survives the pop.
        let _pool = AutoreleasePool::new();
        // SAFETY: `new*` family, +1.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            Obj::from_owned(f(self.id(), sel(c"newCommandQueue")))
        }
    }
}

/// An `MTLLibrary` — one compiled MSL translation unit.
#[derive(Debug)]
pub(crate) struct Library(Obj);

impl Library {
    /// Resolve a vertex/fragment entry point by name.
    pub(crate) fn function(&self, name: &str) -> Option<Obj> {
        // Function lookup autoreleases strings and arrays; keep them out of the
        // caller's frame.
        let _pool = AutoreleasePool::new();
        let n = ns_string(name)?;
        // SAFETY: `newFunctionWithName:` is +1 (or nil when the entry point is
        // absent). The NSString is only read during the call.
        unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
            Obj::from_owned(f(self.0.id(), sel(c"newFunctionWithName:"), n.id()))
        }
    }
}

/// An `MTLRenderPipelineDescriptor` under construction.
pub(crate) struct RenderPipelineDescriptor(Obj);

impl RenderPipelineDescriptor {
    pub(crate) fn new() -> Option<Self> {
        // SAFETY: alloc/init is +1; `Obj` releases it once on drop.
        unsafe {
            let cls = class(c"MTLRenderPipelineDescriptor");
            let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
            let raw = alloc(cls, sel(c"alloc"));
            let init: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            Obj::from_owned(init(raw, sel(c"init"))).map(Self)
        }
    }

    pub(crate) fn set_vertex_function(&self, f: &Obj) {
        // SAFETY: the descriptor RETAINS the function; both outlive the call.
        unsafe {
            let s: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            s(self.0.id(), sel(c"setVertexFunction:"), f.id());
        }
    }

    pub(crate) fn set_fragment_function(&self, f: &Obj) {
        // SAFETY: as above.
        unsafe {
            let s: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            s(self.0.id(), sel(c"setFragmentFunction:"), f.id());
        }
    }

    /// Colour attachment 0: pixel format, write mask and an optional blend
    /// state (`None` disables blending, the replace pipelines).
    ///
    /// `write_mask` is required, not defaulted: Metal's default is
    /// [`ColorWriteMask::ALL`], many pipelines need [`ColorWriteMask::COLOR`],
    /// and a forgotten mask is invisible until a translucent window goes opaque.
    pub(crate) fn set_color_attachment(
        &self,
        format: PixelFormat,
        write_mask: ColorWriteMask,
        blend: Option<BlendState>,
    ) {
        let _pool = AutoreleasePool::new();
        // SAFETY: `colorAttachments` and its subscript return borrowed
        // (autoreleased) objects owned by the pool; only mutated here, never
        // released.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let atts = get(self.0.id(), sel(c"colorAttachments"));
            let sub: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            let a = sub(atts, sel(c"objectAtIndexedSubscript:"), 0);
            let setf: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            setf(a, sel(c"setPixelFormat:"), format as usize);
            setf(a, sel(c"setWriteMask:"), write_mask.bits());
            let setb: unsafe extern "C-unwind" fn(Id, Sel, ObjcBool) = checked_msg();
            setb(a, sel(c"setBlendingEnabled:"), blend.is_some().into());
            if let Some(blend) = blend {
                let set: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
                set(
                    a,
                    sel(c"setSourceRGBBlendFactor:"),
                    blend.source_rgb as usize,
                );
                set(
                    a,
                    sel(c"setDestinationRGBBlendFactor:"),
                    blend.destination_rgb as usize,
                );
                set(
                    a,
                    sel(c"setRgbBlendOperation:"),
                    blend.rgb_operation as usize,
                );
                set(
                    a,
                    sel(c"setSourceAlphaBlendFactor:"),
                    blend.source_alpha as usize,
                );
                set(
                    a,
                    sel(c"setDestinationAlphaBlendFactor:"),
                    blend.destination_alpha as usize,
                );
                set(
                    a,
                    sel(c"setAlphaBlendOperation:"),
                    blend.alpha_operation as usize,
                );
            }
        }
    }

    /// Attach a vertex layout.
    pub(crate) fn set_vertex_descriptor(&self, vd: &VertexDescriptor) {
        // SAFETY: the descriptor copies the vertex descriptor on assignment.
        unsafe {
            let s: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
            s(self.0.id(), sel(c"setVertexDescriptor:"), vd.0.id());
        }
    }
}

/// An `MTLVertexDescriptor` — the instance-buffer layout.
pub(crate) struct VertexDescriptor(Obj);

impl VertexDescriptor {
    pub(crate) fn new() -> Option<Self> {
        // `+vertexDescriptor` and its attribute/layout arrays are autoreleased.
        let _pool = AutoreleasePool::new();
        // SAFETY: `+vertexDescriptor` is autoreleased, so it is retained to +1
        // before the pool pops and released once by `Obj`.
        unsafe {
            let cls = class(c"MTLVertexDescriptor");
            let mk: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
            let raw = mk(cls, sel(c"vertexDescriptor"));
            Obj::retain(raw).map(Self)
        }
    }

    /// Declare attribute `index` at `offset` in buffer `buffer`.
    pub(crate) fn attribute(
        &self,
        index: usize,
        format: VertexFormat,
        offset: usize,
        buffer: usize,
    ) {
        let _pool = AutoreleasePool::new();
        // SAFETY: `attributes` and its subscript return BORROWED autoreleased
        // objects owned by the pool above.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let arr = get(self.0.id(), sel(c"attributes"));
            let sub: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            let a = sub(arr, sel(c"objectAtIndexedSubscript:"), index);
            let set: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            set(a, sel(c"setFormat:"), format as usize);
            set(a, sel(c"setOffset:"), offset);
            set(a, sel(c"setBufferIndex:"), buffer);
        }
    }

    /// Declare buffer `index`'s stride, stepping once per instance
    /// (`MTLVertexStepFunctionPerInstance` = 2), as every `cell.metal` layout
    /// does.
    pub(crate) fn layout_per_instance(&self, index: usize, stride: usize) {
        const STEP_PER_INSTANCE: usize = 2;
        let _pool = AutoreleasePool::new();
        // SAFETY: as `attribute` — borrowed autoreleased accessors.
        unsafe {
            let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
            let arr = get(self.0.id(), sel(c"layouts"));
            let sub: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            let l = sub(arr, sel(c"objectAtIndexedSubscript:"), index);
            let set: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
            set(l, sel(c"setStride:"), stride);
            set(l, sel(c"setStepFunction:"), STEP_PER_INSTANCE);
            set(l, sel(c"setStepRate:"), 1);
        }
    }
}

// ---------------------------------------------------------------------------
// Compute. The renderer issues no compute work; these entry points exist so
// the parity tests can evaluate the shipped MSL field functions and diff them
// against the CPU (`super::tests::fire_field_matches_the_cpu_bit_for_bit`).
// ---------------------------------------------------------------------------

/// `MTLSize`, passed BY VALUE to the dispatch selectors.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct MtlSize {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) depth: usize,
}

// SAFETY: `#[repr(C)]` three `NSUInteger`s, the SDK's anonymous `MTLSize`
// typedef, which clang encodes with no tag.
unsafe impl Encode for MtlSize {
    const ENCODING: &'static str = "{?=QQQ}";
}

impl Device {
    /// Build an `MTLComputePipelineState` from a kernel function.
    #[cfg(test)]
    pub(crate) fn new_compute_pipeline(&self, f: &Obj) -> Result<Obj, String> {
        // As `new_render_pipeline`, error out-param included.
        let _pool = AutoreleasePool::new();
        let mut err: Id = Id::NIL;
        // SAFETY: `new*` family, +1 or nil-with-error.
        let pso = unsafe {
            let m: unsafe extern "C-unwind" fn(Id, Sel, Id, *mut Id) -> Id = checked_msg();
            m(
                self.id(),
                sel(c"newComputePipelineStateWithFunction:error:"),
                f.id(),
                &raw mut err,
            )
        };
        // SAFETY: `pso` is the +1 return described above.
        unsafe { Obj::from_owned(pso) }.ok_or_else(|| ns_error_string(err))
    }
}

/// Read a shared-storage buffer's mapped bytes as `u32`s.
///
/// # Safety
/// `buf` must be a shared-storage `MTLBuffer` of at least `count * 4` bytes
/// whose GPU writes have completed.
#[cfg(test)]
pub(crate) unsafe fn buffer_u32s(buf: &Obj, count: usize) -> Vec<u32> {
    // SAFETY: `-contents` on a shared buffer returns a CPU-visible pointer
    // valid for the buffer's lifetime; the caller pins the length and the
    // completion ordering.
    unsafe {
        let c: unsafe extern "C-unwind" fn(Id, Sel) -> *const u32 = checked_msg();
        let p = c(buf.id(), sel(c"contents"));
        if p.is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(p, count).to_vec()
    }
}

/// Write `bytes` into a shared-storage buffer at offset 0.
///
/// # Safety
/// `buf` must be a shared-storage `MTLBuffer` of at least `bytes.len()` bytes,
/// not concurrently in use by the GPU.
pub(crate) unsafe fn buffer_write(buf: &Obj, bytes: &[u8]) {
    // SAFETY: as `buffer_u32s`, plus the caller's exclusivity guarantee.
    unsafe {
        let c: unsafe extern "C-unwind" fn(Id, Sel) -> *mut u8 = checked_msg();
        let p = c(buf.id(), sel(c"contents"));
        if !p.is_null() {
            ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        }
    }
}

/// Run a one-shot compute dispatch and block until it completes.
///
/// `buffers` are bound at indices `0..buffers.len()`.
#[cfg(test)]
pub(crate) fn dispatch_compute(
    queue: &Obj,
    pso: &Obj,
    buffers: &[&Obj],
    grid: MtlSize,
) -> Result<(), String> {
    let _pool = AutoreleasePool::new();
    // SAFETY: `commandBuffer` and `computeCommandEncoder` return autoreleased
    // objects owned by the pool, not released here and alive until
    // `waitUntilCompleted` returns. Every setter is a void message on live
    // objects.
    unsafe {
        let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
        let cb = get(queue.id(), sel(c"commandBuffer"));
        if cb.is_null() {
            return Err("MTLCommandQueue returned no command buffer".to_owned());
        }
        let enc = get(cb, sel(c"computeCommandEncoder"));
        if enc.is_null() {
            return Err("MTLCommandBuffer returned no compute encoder".to_owned());
        }
        let set_pso: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
        set_pso(enc, sel(c"setComputePipelineState:"), pso.id());
        let set_buf: unsafe extern "C-unwind" fn(Id, Sel, Id, usize, usize) = checked_msg();
        for (i, b) in buffers.iter().enumerate() {
            set_buf(enc, sel(c"setBuffer:offset:atIndex:"), b.id(), 0, i);
        }
        // A 8x8 threadgroup is safely under the 1024-thread minimum every Metal
        // device guarantees, so no per-device query is needed.
        let tg = MtlSize {
            width: 8,
            height: 8,
            depth: 1,
        };
        let disp: unsafe extern "C-unwind" fn(Id, Sel, MtlSize, MtlSize) = checked_msg();
        disp(
            enc,
            sel(c"dispatchThreads:threadsPerThreadgroup:"),
            grid,
            tg,
        );
        let void_msg: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
        void_msg(enc, sel(c"endEncoding"));
        void_msg(cb, sel(c"commit"));
        wait_for_command_buffer(cb)?;
    }
    Ok(())
}

/// Wait for a committed command buffer and turn every non-success terminal
/// state into a diagnostic. `waitUntilCompleted` only blocks; it does not
/// report device loss, timeout, page fault, or allocation failure.
#[cfg(test)]
fn wait_for_command_buffer(command_buffer: Id) -> Result<(), String> {
    const STATUS_COMPLETED: usize = 4;
    // SAFETY: callers pass a live command buffer retained by their surrounding
    // autorelease pool. All messages are property reads or the blocking wait.
    unsafe {
        let wait: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
        wait(command_buffer, sel(c"waitUntilCompleted"));
        let get_status: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        let status = get_status(command_buffer, sel(c"status"));
        if status == STATUS_COMPLETED {
            return Ok(());
        }
        let get_error: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
        let error = get_error(command_buffer, sel(c"error"));
        Err(format!(
            "Metal command buffer finished with status {status}: {}",
            ns_error_string(error)
        ))
    }
}

// ---------------------------------------------------------------------------
// Render encoding: a colour attachment, a draw, and a texture -> buffer copy
// for CPU readback. The default draw is a vertex-bufferless fullscreen
// triangle (`vs_blit` synthesises positions from `[[vertex_id]]`); a
// [`DrawCall`] override draws a table row instanced from a bound stream.
// ---------------------------------------------------------------------------

/// `MTLLoadAction::Load`.
pub(crate) const LOAD_ACTION_LOAD: usize = 1;
/// `MTLLoadAction::Clear`.
pub(crate) const LOAD_ACTION_CLEAR: usize = 2;
/// `MTLStoreAction::Store`.
pub(crate) const STORE_ACTION_STORE: usize = 1;

/// `MTLPrimitiveType`, the topology passed to `drawPrimitives:…` (draw state
/// in Metal). `crate::metal::pipelines::metal_primitive_type` maps the table's
/// `Topology` onto it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum PrimitiveType {
    /// `MTLPrimitiveTypeTriangle`.
    Triangle = 3,
    /// `MTLPrimitiveTypeTriangleStrip` — the tray's 4-vertex quad.
    TriangleStrip = 4,
}

/// The vertex-buffer slot the per-instance stream binds at.
///
/// MSL vertex functions taking `[[stage_in]]` declare their uniform block at
/// `[[buffer(0)]]`, and Metal shares one vertex buffer argument table between
/// stage-in attributes and constant buffers, so a stream at slot 0 lands on
/// the uniforms and draws nothing, silently.
///
/// Streams count down from the top of the 31-entry table while uniforms count
/// up from 0 (wgpu-hal's principle, though its own slot is 15); 30 keeps the
/// maximum distance. `metal_vertex_descriptor` lays attributes and stride
/// here and `draw_and_read` binds `DrawCall::stream` here.
pub(crate) const INSTANCE_STREAM_SLOT: usize = 30;

/// `MTLClearColor`, passed by value to `-setClearColor:`: four doubles, a
/// homogeneous float aggregate in `v0..v3` on arm64, which `repr(C)`
/// reproduces.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClearColor {
    pub(crate) r: f64,
    pub(crate) g: f64,
    pub(crate) b: f64,
    pub(crate) a: f64,
}

// SAFETY: `#[repr(C)]` four `double`s, the SDK's anonymous `MTLClearColor`.
unsafe impl Encode for ClearColor {
    const ENCODING: &'static str = "{?=dddd}";
}

/// `MTLScissorRect`, passed by value to `-setScissorRect:`: four
/// `NSUInteger`s (32 bytes), passed indirectly like [`MtlRegion`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MtlScissorRect {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

// SAFETY: `#[repr(C)]` four `NSUInteger`s, the SDK's anonymous
// `MTLScissorRect`.
unsafe impl Encode for MtlScissorRect {
    const ENCODING: &'static str = "{?=QQQQ}";
}

/// `MTLViewport`, passed by value to `-setViewport:`: six doubles, too many
/// for a float aggregate, so 48 bytes passed indirectly like [`MtlRegion`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MtlViewport {
    pub(crate) origin_x: f64,
    pub(crate) origin_y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) znear: f64,
    pub(crate) zfar: f64,
}

// SAFETY: `#[repr(C)]` six `double`s, the SDK's anonymous `MTLViewport`.
unsafe impl Encode for MtlViewport {
    const ENCODING: &'static str = "{?=dddddd}";
}

impl MtlViewport {
    /// The whole of a `width` x `height` attachment, depth `0..1` — what
    /// `renderer.rs:8846` sets before every scissored pass.
    pub(crate) fn full_2d(width: usize, height: usize) -> Self {
        Self {
            origin_x: 0.0,
            origin_y: 0.0,
            #[expect(
                clippy::cast_precision_loss,
                reason = "attachment extents are framebuffer pixels, far inside f64's exact \
                          integer range"
            )]
            width: width as f64,
            #[expect(clippy::cast_precision_loss, reason = "as above")]
            height: height as f64,
            znear: 0.0,
            zfar: 1.0,
        }
    }
}

/// What a render pass does with the destination's existing contents. No
/// `DontCare`: nothing here wants undefined starting pixels.
#[derive(Clone, Copy, Debug)]
pub(crate) enum LoadAction {
    /// Overwrite every texel with this colour first.
    Clear(ClearColor),
    /// Keep the existing contents; the only way to prove a write mask or
    /// scissor did anything.
    Load,
}

/// `MTLOrigin`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct MtlOrigin {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) z: usize,
}

// SAFETY: `#[repr(C)]` three `NSUInteger`s, the SDK's anonymous `MTLOrigin`.
unsafe impl Encode for MtlOrigin {
    const ENCODING: &'static str = "{?=QQQ}";
}

/// `MTLRegion`, an origin plus a size passed by value to `-replaceRegion:…`
/// (48 bytes, passed indirectly).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct MtlRegion {
    pub(crate) origin: MtlOrigin,
    pub(crate) size: MtlSize,
}

// SAFETY: `#[repr(C)]` an `MTLOrigin` then an `MTLSize`, the SDK's anonymous
// `MTLRegion`.
unsafe impl Encode for MtlRegion {
    const ENCODING: &'static str = "{?={?=QQQ}{?=QQQ}}";
}

impl MtlRegion {
    /// The whole of a 2-D mip level (the tests' upload shape; production
    /// uploads go through the device layer's row-ranged verb).
    #[cfg(test)]
    pub(crate) const fn full_2d(width: usize, height: usize) -> Self {
        Self {
            origin: MtlOrigin { x: 0, y: 0, z: 0 },
            size: MtlSize {
                width,
                height,
                depth: 1,
            },
        }
    }
}

/// Upload CPU bytes into a 2-D texture's level 0.
///
/// # Safety
/// `tex` must be a 2-D `MTLTexture` whose storage mode is not `Private`, whose
/// dimensions are at least `region`, and whose pixel format has exactly
/// `bytes_per_row / region.size.width` bytes per texel. `bytes` must hold at
/// least `bytes_per_row * region.size.height` bytes.
pub(crate) unsafe fn texture_upload(
    tex: &Obj,
    region: MtlRegion,
    bytes: &[u8],
    bytes_per_row: usize,
) {
    // SAFETY: the caller guarantees format, extent and length agree;
    // `replaceRegion:` copies out of `bytes` before returning.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, MtlRegion, usize, *const u8, usize) =
            checked_msg();
        f(
            tex.id(),
            sel(c"replaceRegion:mipmapLevel:withBytes:bytesPerRow:"),
            region,
            0,
            bytes.as_ptr(),
            bytes_per_row,
        );
    }
}

/// `newTextureViewWithPixelFormat:`: the Unorm/sRGB aliasing the base passes
/// rely on (see [`super::shaders`]). Base passes attach the sRGB view of an
/// `Rgba8Unorm` offscreen to blend in linear light; additive passes and the
/// blit read the Unorm view of the same storage.
///
/// The Unorm/sRGB pair is Metal's sRGB-variant exemption and needs no
/// `TEXTURE_USAGE_PIXEL_FORMAT_VIEW`; other pairs do. An illegal view aborts
/// under the validation layer; the `None` arm is reachable only with
/// validation off.
pub(crate) fn texture_view(tex: &Obj, format: PixelFormat) -> Option<Obj> {
    // Pool per the module rule: `new*` returns +1, but the driver autoreleases
    // privately en route.
    let _pool = AutoreleasePool::new();
    // SAFETY: `new*` family, +1 or nil; the view holds its own reference to
    // the base texture, so the two lifetimes are independent.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
        Obj::from_owned(f(
            tex.id(),
            sel(c"newTextureViewWithPixelFormat:"),
            format as usize,
        ))
    }
}

/// Read `-[MTLTexture pixelFormat]` as its raw value, so `draw_and_read` can
/// validate a row stride against the destination it was handed.
pub(crate) fn texture_pixel_format_raw(tex: &Obj) -> usize {
    // SAFETY: `-pixelFormat` is an `NSUInteger` getter on a live `MTLTexture`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        f(tex.id(), sel(c"pixelFormat"))
    }
}

/// `-[MTLTexture width]`.
pub(crate) fn texture_width(tex: &Obj) -> usize {
    // SAFETY: `-width` is an `NSUInteger` getter on a live `MTLTexture`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        f(tex.id(), sel(c"width"))
    }
}

/// `-[MTLTexture height]`.
pub(crate) fn texture_height(tex: &Obj) -> usize {
    // SAFETY: `-height` is an `NSUInteger` getter on a live `MTLTexture`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        f(tex.id(), sel(c"height"))
    }
}

/// `-[MTLTexture usage]`, the `MTLTextureUsage` bitmask. The encoder refuses a
/// render pass onto a texture without [`TEXTURE_USAGE_RENDER_TARGET`], which
/// otherwise completes silently without the validation layer.
pub(crate) fn texture_usage(tex: &Obj) -> usize {
    // SAFETY: `-usage` is an `NSUInteger` getter on a live `MTLTexture`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        f(tex.id(), sel(c"usage"))
    }
}

/// `-[MTLBuffer length]`, in bytes.
pub(crate) fn buffer_length(buf: &Obj) -> usize {
    // SAFETY: `-length` is an `NSUInteger` getter on a live `MTLBuffer`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        f(buf.id(), sel(c"length"))
    }
}

/// Everything one render-and-read pass binds and sets. The draw is the
/// 3-vertex fullscreen triangle unless [`Self::draw`] says otherwise. A struct
/// because `dst_w`/`dst_h`/`bytes_per_row` constrain each other.
#[cfg(test)]
pub(crate) struct Pass<'a> {
    /// The `MTLRenderPipelineState` to draw with.
    pub(crate) pso: &'a Obj,
    /// The colour attachment. Must be at least `dst_w` x `dst_h`.
    pub(crate) dst: &'a Obj,
    pub(crate) dst_w: usize,
    pub(crate) dst_h: usize,
    /// What happens to `dst`'s existing texels before the draw.
    pub(crate) load: LoadAction,
    /// `-setViewport:`, or the attachment's full extent when `None`.
    pub(crate) viewport: Option<MtlViewport>,
    /// `-setScissorRect:`, or the attachment's full extent when `None`.
    pub(crate) scissor: Option<MtlScissorRect>,
    /// Fragment texture and its `[[texture(n)]]` index, bound when present.
    /// For a table row the index comes from `spec.binds.fragment_textures`.
    pub(crate) src_tex: Option<(&'a Obj, usize)>,
    /// Fragment sampler and its `[[sampler(n)]]` index, bound only when
    /// present (`spec.binds.fragment_samplers` for a table row).
    pub(crate) sampler: Option<(&'a Obj, usize)>,
    /// Fragment buffer and its `[[buffer(n)]]` index, bound when present
    /// (`spec.binds.fragment_buffers` for a table row).
    pub(crate) uniform: Option<(&'a Obj, usize)>,
    /// Vertex-stage uniform buffer and its index, bound when present.
    /// Caller-spelled because rows differ: `cell.metal` and `hdr_glow.metal`
    /// use `[[buffer(0)]]` (safe because the stream sits at
    /// [`INSTANCE_STREAM_SLOT`]), `tray.metal` uses `[[buffer(2)]]`.
    pub(crate) vertex_uniform: Option<(&'a Obj, usize)>,
    /// What to draw, or `None` for the default 3-vertex fullscreen triangle.
    pub(crate) draw: Option<DrawCall<'a>>,
}

/// One real draw: topology, vertex count, instance count and the instance
/// stream, bound at [`INSTANCE_STREAM_SLOT`]. Needed for instanced
/// `[[stage_in]]` rows and the tray's strip.
#[cfg(test)]
pub(crate) struct DrawCall<'a> {
    /// The row's topology — `pipelines::metal_primitive_type(spec.topology)`,
    /// never spelled at a call site.
    pub(crate) primitive: PrimitiveType,
    /// Vertices per instance: 6 for the two-triangle cell quads, 4 for the
    /// tray strip.
    pub(crate) vertices: usize,
    /// `instanceCount`.
    pub(crate) instances: usize,
    /// The per-instance stream, bound at [`INSTANCE_STREAM_SLOT`]; `None` for
    /// a `[[vertex_id]]`-only row (`VertexLayout::None`) like the tray.
    pub(crate) stream: Option<&'a Obj>,
}

/// Run the pass (the fullscreen triangle, or [`Pass::draw`]'s geometry) and
/// copy the result into `readback`.
///
/// One command buffer holds the render pass and the texture -> buffer blit, so
/// encoder order sequences them; `waitUntilCompleted` makes the bytes visible.
/// `readback` must be Shared storage (no `synchronizeResource:` here).
///
/// Binding indices are caller-spelled, from the table's `BindSpec` for a table
/// row.
///
/// Safe and checked, unlike `texture_upload`, `buffer_write`, `buffer_u32s`
/// and `buffer_bytes`, whose preconditions depend on GPU/CPU ordering. This
/// function owns its ordering (creates, commits and waits the command buffer)
/// and validates the destination format and extent and the readback length
/// against the live objects, returning `Err` on a mismatch.
#[cfg(test)]
pub(crate) fn draw_and_read(
    queue: &Obj,
    pass: &Pass<'_>,
    readback: &Obj,
    bytes_per_row: usize,
) -> Result<(), String> {
    let (dw, dh) = (pass.dst_w, pass.dst_h);
    if dw == 0 || dh == 0 {
        return Err(format!("destination extent {dw}x{dh} is empty"));
    }
    if let Some(d) = &pass.draw
        && (d.vertices == 0 || d.instances == 0)
    {
        return Err(format!(
            "a draw of {} vertices x {} instances is empty",
            d.vertices, d.instances
        ));
    }
    let (tw, th) = (texture_width(pass.dst), texture_height(pass.dst));
    if dw > tw || dh > th {
        return Err(format!(
            "destination extent {dw}x{dh} exceeds the attachment's {tw}x{th}"
        ));
    }
    let raw = texture_pixel_format_raw(pass.dst);
    let bpt = PixelFormat::from_raw(raw)
        .ok_or_else(|| {
            format!("destination MTLPixelFormat {raw} is not one this module models, so its row stride cannot be checked")
        })?
        .bytes_per_texel();
    let min_row = dw * bpt;
    if bytes_per_row < min_row {
        return Err(format!(
            "destinationBytesPerRow({bytes_per_row}) must be >= {min_row} \
             ({dw} texels x {bpt} bytes for MTLPixelFormat {raw})"
        ));
    }
    let need = bytes_per_row
        .checked_mul(dh)
        .ok_or_else(|| format!("readback size {bytes_per_row} x {dh} overflows usize"))?;
    let have = buffer_length(readback);
    if have < need {
        return Err(format!(
            "readback buffer holds {have} bytes, needs {need} ({bytes_per_row} x {dh})"
        ));
    }
    if let Some(sc) = pass.scissor
        && (sc.width > tw || sc.height > th || sc.x > tw - sc.width || sc.y > th - sc.height)
    {
        return Err(format!(
            "scissor {}x{}+{}+{} leaves the {tw}x{th} attachment",
            sc.width, sc.height, sc.x, sc.y
        ));
    }

    let _pool = AutoreleasePool::new();
    // SAFETY: the pass descriptor, `colorAttachments`, its subscript,
    // `commandBuffer`, and both encoders return autoreleased objects owned by
    // the pool, not released here and alive past `waitUntilCompleted`. Other
    // sends are void messages on live objects with full prototypes. The copy's
    // extent, stride and length were validated above.
    unsafe {
        let rp_cls = class(c"MTLRenderPassDescriptor");
        let mk: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = checked_msg();
        let rp = mk(rp_cls, sel(c"renderPassDescriptor"));

        let get: unsafe extern "C-unwind" fn(Id, Sel) -> Id = checked_msg();
        let atts = get(rp, sel(c"colorAttachments"));
        let sub: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
        let a0 = sub(atts, sel(c"objectAtIndexedSubscript:"), 0);

        let set_obj: unsafe extern "C-unwind" fn(Id, Sel, Id) = checked_msg();
        set_obj(a0, sel(c"setTexture:"), pass.dst.id());
        let set_usize: unsafe extern "C-unwind" fn(Id, Sel, usize) = checked_msg();
        set_usize(a0, sel(c"setStoreAction:"), STORE_ACTION_STORE);
        match pass.load {
            LoadAction::Clear(c) => {
                set_usize(a0, sel(c"setLoadAction:"), LOAD_ACTION_CLEAR);
                let set_clear: unsafe extern "C-unwind" fn(Id, Sel, ClearColor) = checked_msg();
                set_clear(a0, sel(c"setClearColor:"), c);
            }
            LoadAction::Load => set_usize(a0, sel(c"setLoadAction:"), LOAD_ACTION_LOAD),
        }

        let cb = get(queue.id(), sel(c"commandBuffer"));
        let mk_enc: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = checked_msg();
        let enc = mk_enc(cb, sel(c"renderCommandEncoderWithDescriptor:"), rp);

        set_obj(enc, sel(c"setRenderPipelineState:"), pass.pso.id());
        if let Some(vp) = pass.viewport {
            let set_vp: unsafe extern "C-unwind" fn(Id, Sel, MtlViewport) = checked_msg();
            set_vp(enc, sel(c"setViewport:"), vp);
        }
        if let Some(sc) = pass.scissor {
            let set_sc: unsafe extern "C-unwind" fn(Id, Sel, MtlScissorRect) = checked_msg();
            set_sc(enc, sel(c"setScissorRect:"), sc);
        }
        let set_tex: unsafe extern "C-unwind" fn(Id, Sel, Id, usize) = checked_msg();
        if let Some((t, index)) = pass.src_tex {
            set_tex(enc, sel(c"setFragmentTexture:atIndex:"), t.id(), index);
        }
        if let Some((sm, index)) = pass.sampler {
            set_tex(
                enc,
                sel(c"setFragmentSamplerState:atIndex:"),
                sm.id(),
                index,
            );
        }
        let set_buf: unsafe extern "C-unwind" fn(Id, Sel, Id, usize, usize) = checked_msg();
        if let Some((u, index)) = pass.uniform {
            set_buf(
                enc,
                sel(c"setFragmentBuffer:offset:atIndex:"),
                u.id(),
                0,
                index,
            );
        }
        if let Some((u, index)) = pass.vertex_uniform {
            set_buf(
                enc,
                sel(c"setVertexBuffer:offset:atIndex:"),
                u.id(),
                0,
                index,
            );
        }
        // The stream binds only at INSTANCE_STREAM_SLOT; slot 0 belongs to the
        // MSL uniform blocks.
        let draw: unsafe extern "C-unwind" fn(Id, Sel, usize, usize, usize, usize) = checked_msg();
        match &pass.draw {
            Some(d) => {
                if let Some(s) = d.stream {
                    set_buf(
                        enc,
                        sel(c"setVertexBuffer:offset:atIndex:"),
                        s.id(),
                        0,
                        INSTANCE_STREAM_SLOT,
                    );
                }
                draw(
                    enc,
                    sel(c"drawPrimitives:vertexStart:vertexCount:instanceCount:"),
                    d.primitive as usize,
                    0,
                    d.vertices,
                    d.instances,
                );
            }
            None => draw(
                enc,
                sel(c"drawPrimitives:vertexStart:vertexCount:instanceCount:"),
                PrimitiveType::Triangle as usize,
                0,
                3,
                1,
            ),
        }
        let void_msg: unsafe extern "C-unwind" fn(Id, Sel) = checked_msg();
        void_msg(enc, sel(c"endEncoding"));

        // Texture -> buffer, in the SAME command buffer, so it is ordered after
        // the pass above without an explicit barrier.
        let blit = get(cb, sel(c"blitCommandEncoder"));
        let copy: unsafe extern "C-unwind" fn(
            Id,
            Sel,
            Id,
            usize,
            usize,
            MtlOrigin,
            MtlSize,
            Id,
            usize,
            usize,
            usize,
        ) = checked_msg();
        copy(
            blit,
            sel(c"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:"),
            pass.dst.id(),
            0,
            0,
            MtlOrigin { x: 0, y: 0, z: 0 },
            MtlSize {
                width: dw,
                height: dh,
                depth: 1,
            },
            readback.id(),
            0,
            bytes_per_row,
            need,
        );
        void_msg(blit, sel(c"endEncoding"));

        void_msg(cb, sel(c"commit"));
        wait_for_command_buffer(cb)?;
    }
    Ok(())
}

/// Read a shared-storage buffer's mapped bytes.
///
/// # Safety
/// As `buffer_u32s`, for `len` bytes.
pub(crate) unsafe fn buffer_bytes(buf: &Obj, len: usize) -> Vec<u8> {
    // SAFETY: `-contents` on a shared buffer is CPU-visible for the buffer's
    // lifetime; the caller pins the length and the completion ordering.
    unsafe {
        let c: unsafe extern "C-unwind" fn(Id, Sel) -> *const u8 = checked_msg();
        let p = c(buf.id(), sel(c"contents"));
        if p.is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(p, len).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metal::test_support::{C_STRING_HOLES, ns_string_of_units};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// How many times this process has called
    /// [`Device::system_default_asks_window_server`]. Test builds only.
    pub(super) static WINDOW_SERVER_ASKS: AtomicUsize = AtomicUsize::new(0);

    /// Every device `MTLCopyAllDevices` lists as `(name, isLowPower,
    /// isHeadless)`, read independently of [`Device::listing`]. `None` without
    /// Metal.
    fn listing_read_independently() -> Option<Vec<(String, bool, bool)>> {
        // SAFETY: as in `Device::listing`: a +1 array released by `Obj`, +0
        // elements retained for the name while the array lives, `BOOL` getters
        // through `ObjcBool`.
        unsafe {
            let all = Obj::from_owned(MTLCopyAllDevices())?;
            let count: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
            let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
            let flag: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
            let mut out = Vec::new();
            for i in 0..count(all.id(), sel(c"count")) {
                let d = at(all.id(), sel(c"objectAtIndexedSubscript:"), i);
                let Some(dev) = Obj::retain(d).map(Device) else {
                    continue;
                };
                out.push((
                    dev.name(),
                    flag(d, sel(c"isLowPower")).as_bool(),
                    flag(d, sel(c"isHeadless")).as_bool(),
                ));
            }
            Some(out)
        }
    }

    /// [`Device::preferred`] calls `MTLCreateSystemDefaultDevice` only when
    /// the listing cannot name the GPU, and otherwise returns exactly the GPU
    /// the listing names (expected value derived from the raw listing, not
    /// [`choose_listed`]). The ask counter is process-wide, so the decided arm
    /// asserts zero asks and the undecided arm only a lower bound.
    #[test]
    fn preferred_asks_the_window_server_only_when_the_listing_cannot_decide() {
        let _pool = AutoreleasePool::new();
        let Some(gpus) = listing_read_independently() else {
            aterm_spec::host::could_not_run("no Metal device on this machine");
        };
        let want_high = aterm_types::dev_seam!("ATERM_GPU_POWER")
            .is_some_and(|v| v.eq_ignore_ascii_case("high"));
        let display: Vec<&(String, bool, bool)> = gpus.iter().filter(|g| !g.2).collect();
        // Without `high`, the first low-power display GPU; otherwise only a
        // display GPU alone in the listing.
        let expected = match display.iter().find(|g| g.1) {
            Some(low) if !want_high => Some(low.0.clone()),
            _ => (display.len() == 1).then(|| display[0].0.clone()),
        };

        let before = WINDOW_SERVER_ASKS.load(Ordering::SeqCst);
        let picked = Device::preferred().map(|d| d.name());
        let asked = WINDOW_SERVER_ASKS.load(Ordering::SeqCst) - before;
        crate::stderr_line!(
            "aterm-gpu: listing {gpus:?} (want_high={want_high}); preferred() = {picked:?}, \
             window-server asks {asked}"
        );
        match expected {
            Some(name) => {
                assert_eq!(
                    asked, 0,
                    "the listing names the GPU ({name}), so nothing may ask the display server"
                );
                assert_eq!(picked, Some(name), "and preferred() is that GPU");
            }
            None => assert!(
                asked >= 1,
                "the listing cannot decide here, so the choice is the display server's"
            ),
        }
    }

    /// Every listing shape [`choose_listed`] can meet, including dual- and
    /// multi-GPU shapes. The `high` column is `None` for every listing with two
    /// or more display GPUs, because there `high` means the mux-switching
    /// system default.
    #[test]
    fn listing_decides_every_shape_it_can() {
        /// What [`choose_listed`] answers: the index it picked and its word.
        type Pick = Option<(usize, &'static str)>;
        let gpu = |low_power, headless| {
            Some(ListedGpu {
                low_power,
                headless,
            })
        };
        let apple = gpu(false, false);
        let integrated = gpu(true, false);
        let discrete = gpu(false, false);
        let compute = gpu(false, true);
        let only = "the only display GPU";
        #[rustfmt::skip]
        let rows: [(&str, Vec<Option<ListedGpu>>, Pick, Pick); 11] = [
            // shape                         listing                          default                  high
            ("no Metal",                     vec![],                          None,                    None),
            ("a nil slot only",              vec![None],                      None,                    None),
            ("Apple silicon",                vec![apple],                     Some((0, only)),         Some((0, only))),
            ("nil slot, then Apple silicon", vec![None, apple],               Some((1, only)),         Some((1, only))),
            ("dual-GPU laptop",              vec![integrated, discrete],      Some((0, "low-power")),  None),
            ("dual-GPU, discrete first",     vec![discrete, integrated],      Some((1, "low-power")),  None),
            ("integrated only",              vec![integrated],                Some((0, "low-power")),  Some((0, only))),
            ("one display GPU + compute",    vec![discrete, compute],         Some((0, only)),         Some((0, only))),
            ("compute only",                 vec![compute],                   None,                    None),
            ("two discrete (Mac Pro)",       vec![discrete, discrete],        None,                    None),
            ("laptop + eGPU",                vec![integrated, discrete, discrete], Some((0, "low-power")), None),
        ];
        // Every row is judged before anything fails, so one run names every
        // shape a policy change moved, not just the first.
        let mut wrong = Vec::new();
        for (shape, listing, default, high) in rows {
            for (policy, want_high, want) in [("default", false, default), ("high", true, high)] {
                let got = choose_listed(&listing, want_high);
                if got != want {
                    wrong.push(format!("{shape}, {policy}: got {got:?}, want {want:?}"));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "choose_listed moved:\n{}",
            wrong.join("\n")
        );
    }

    /// An NSError's description is read by its counted UTF-16 units, so
    /// neither of the strings a C-string read gets wrong truncates or blanks a
    /// Metal compile or pipeline diagnostic. Headless: Foundation objects only.
    #[test]
    fn an_error_description_is_read_whole() {
        let _pool = AutoreleasePool::new();
        let key = ns_string("NSLocalizedDescription").expect("key");
        let domain = ns_string("aterm.test").expect("domain");
        let read = C_STRING_HOLES.map(|(units, _)| {
            let description = ns_string_of_units(units);
            // SAFETY: `+dictionaryWithObject:forKey:` (`@32@0:8@16@24`) and
            // `+errorWithDomain:code:userInfo:` (`@40@0:8@16q24@32`) return
            // autoreleased objects the pool above owns; every argument is a
            // live object for the call, and the dictionary retains its value.
            let error = unsafe {
                let dict: unsafe extern "C-unwind" fn(ClassPtr, Sel, Id, Id) -> Id = checked_msg();
                let info = dict(
                    class(c"NSDictionary"),
                    sel(c"dictionaryWithObject:forKey:"),
                    description.id(),
                    key.id(),
                );
                let make: unsafe extern "C-unwind" fn(ClassPtr, Sel, Id, isize, Id) -> Id =
                    checked_msg();
                make(
                    class(c"NSError"),
                    sel(c"errorWithDomain:code:userInfo:"),
                    domain.id(),
                    1,
                    info,
                )
            };
            assert!(!error.is_null(), "NSError for {units:04X?}");
            ns_error_string(error)
        });
        assert_eq!(read, C_STRING_HOLES.map(|(_, want)| want));
    }

    /// [`Device::name`] reads `-name` by its counted UTF-16 units. The OS names
    /// a real `MTLDevice`, so an `NSNotification`, which answers `-name` with
    /// the `NSString` it was made with, stands in for one.
    #[test]
    fn a_device_name_is_read_whole() {
        let _pool = AutoreleasePool::new();
        let read = C_STRING_HOLES.map(|(units, _)| {
            let name = ns_string_of_units(units);
            // SAFETY: `+notificationWithName:object:` (`@32@0:8@16@24`) returns
            // an autoreleased NSNotification the pool above owns; `Obj::retain`
            // takes it to +1, which the stand-in `Device` releases on drop.
            let stand_in = unsafe {
                let f: unsafe extern "C-unwind" fn(ClassPtr, Sel, Id, Id) -> Id = checked_msg();
                Obj::retain(f(
                    class(c"NSNotification"),
                    sel(c"notificationWithName:object:"),
                    name.id(),
                    Id::NIL,
                ))
            }
            .map(Device)
            .expect("NSNotification");
            stand_in.name()
        });
        assert_eq!(read, C_STRING_HOLES.map(|(_, want)| want));
    }
}
