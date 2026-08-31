// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metal / QuartzCore, straight over the Objective-C runtime.
//!
//! This is the object layer the macOS renderer needs and NOTHING else: a
//! device, a command queue, shader libraries compiled from MSL source, render
//! pipeline states, buffers, textures, samplers, and the `CAMetalLayer`
//! swapchain. Metal and QuartzCore are OS frameworks, so nothing is vendored
//! and nothing is compiled — only the entry points below are declared.
//!
//! # Why this file is shaped differently from `keychain.rs`
//!
//! The house style for Apple FFI in this repo (`aterm-http/src/verifier/
//! apple.rs`, `aterm-gui/src/net_connections/keychain.rs`,
//! `aterm-gui/src/trail_audio.rs`, `keymap.rs`, `secure_input.rs`) is FLAT C:
//! Security, IOKit, AudioToolbox and CoreGraphics all export plain C entry
//! points, so those files declare `unsafe extern "C" fn` and call them.
//!
//! **Metal exports no such surface.** `MTLDevice`, `MTLCommandQueue`,
//! `MTLBuffer`, `MTLTexture` and friends are Objective-C *protocols*; the only
//! C function in the whole framework is `MTLCreateSystemDefaultDevice`.
//! Everything else is a message send. So this module is the first in the tree
//! to talk to the Objective-C runtime directly, and it establishes the
//! convention rather than inheriting one:
//!
//! * `objc_msgSend` is declared ONCE, as a bare `extern "C" fn()` with no
//!   argument list, and every call site casts it to a fully-typed function
//!   pointer through [`msg`]. On `aarch64-apple-darwin` there is no variadic
//!   ABI for message sends — passing the wrong prototype silently corrupts
//!   registers — so a typed cast per selector is the ONLY sound form.
//! * Every `new*`/`alloc` result is +1 and lands in [`Obj`], the single owning
//!   wrapper, which is the only place `objc_release` is called and calls it
//!   exactly once on drop. This mirrors `apple.rs`'s `CfOwned` discipline.
//! * Autoreleased returns (`colorAttachments`, `objectAtIndexedSubscript:`,
//!   the `MTLVertexDescriptor` accessors) are BORROWED as bare [`Id`] and
//!   never released here; they live until the enclosing [`AutoreleasePool`]
//!   pops. Every entry point that can produce one takes a pool.
//!
//! # Selector safety
//!
//! Selectors are resolved through [`sel`], which takes a NUL-terminated byte
//! literal so a missing NUL is a compile error rather than a read past the end
//! of a string. A selector that does not exist resolves fine and then raises an
//! ObjC "unrecognized selector" exception on send, which unwinds through Rust
//! as an abort; the tests in `super` cover every selector this module sends.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;

/// An Objective-C object pointer. Null is the ObjC `nil` and is always a valid
/// value to send a message to (it returns zero), so this is deliberately a raw
/// pointer rather than a `NonNull`.
pub(crate) type Id = *mut c_void;
/// An Objective-C selector — an interned, immortal string the runtime owns.
pub(crate) type Sel = *const c_void;
/// An Objective-C class object. A class is itself an object, so `Id` and this
/// are the same shape; the alias exists to make the message target obvious.
pub(crate) type ClassPtr = *mut c_void;

#[link(name = "objc")]
unsafe extern "C" {
    /// Declared with NO parameter list on purpose — see the module docs. Every
    /// call goes through [`msg`], which casts this to the exact prototype of
    /// the selector being sent.
    fn objc_msgSend();
    fn objc_getClass(name: *const c_char) -> ClassPtr;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_retain(obj: Id) -> Id;
    fn objc_release(obj: Id);
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    /// The one C entry point in Metal. `CF_RETURNS_RETAINED`: the caller owns
    /// the +1 and must release it, which [`Device`] does on drop.
    fn MTLCreateSystemDefaultDevice() -> Id;
}

// QuartzCore is linked for `CAMetalLayer`, which is reached by class name
// through the ObjC runtime rather than by symbol. The `#[link]` is what puts
// the framework (and therefore that class) in the image.
#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {}

/// Cast the untyped `objc_msgSend` to a concrete prototype.
///
/// # Safety
/// `F` must be the EXACT C prototype of the selector about to be sent,
/// including the implicit `(self, _cmd)` leading pair. Getting this wrong is
/// undefined behaviour on every Apple ABI.
#[inline]
pub(crate) unsafe fn msg<F>() -> F {
    // SAFETY: `objc_msgSend` is a function symbol, so its address is a valid
    // function pointer; the caller's `F` supplies the prototype. `size_of`
    // equality is asserted so a non-function `F` cannot be transmuted in.
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    unsafe { std::mem::transmute_copy(&(objc_msgSend as *const c_void)) }
}

/// Resolve a selector from a NUL-terminated literal, e.g. `sel(b"length\0")`.
#[inline]
pub(crate) fn sel(name: &'static [u8]) -> Sel {
    debug_assert!(
        CStr::from_bytes_with_nul(name).is_ok(),
        "selector literal must be NUL-terminated exactly once"
    );
    // SAFETY: `name` is a 'static byte literal whose final byte the debug
    // assertion above pins as the single NUL; `sel_registerName` copies it into
    // the runtime's immortal selector table and never retains the pointer.
    unsafe { sel_registerName(name.as_ptr().cast::<c_char>()) }
}

/// Look up a class from a NUL-terminated literal, e.g. `class(b"NSString\0")`.
#[inline]
pub(crate) fn class(name: &'static [u8]) -> ClassPtr {
    // SAFETY: same NUL-termination contract as `sel`. Classes are immortal;
    // the returned pointer is never released.
    unsafe { objc_getClass(name.as_ptr().cast::<c_char>()) }
}

/// An owned (+1) Objective-C object. The ONLY place `objc_release` is called.
///
/// Wrapping every `new*` result is what keeps a renderer that rebuilds
/// pipelines on every resize from leaking one `MTLRenderPipelineState` per
/// event — the Metal twin of the `CfOwned` argument in `apple.rs`.
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
}

impl Drop for Obj {
    fn drop(&mut self) {
        // SAFETY: `self.0` is non-null by construction (both constructors
        // reject null) and holds exactly one +1 reference, released once here.
        unsafe { objc_release(self.0) }
    }
}

/// An `@autoreleasepool` scope. Metal's descriptor accessors return
/// autoreleased objects; without a pool on the calling thread they accumulate
/// until the thread dies, which for the render thread means "forever".
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
        // SAFETY: `self.0` is the token from the matching push. Pools are
        // strictly nested because this type is neither `Send` nor cloneable and
        // the token never escapes.
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

/// Build a +1 `NSString` from a Rust `&str`.
///
/// Uses `alloc` + `initWithBytes:length:encoding:` rather than the familiar
/// `+stringWithUTF8String:` so the result is OWNED rather than autoreleased —
/// no pool is required and the lifetime is explicit.
pub(crate) fn ns_string(s: &str) -> Option<Obj> {
    const NS_UTF8: usize = 4;
    let cls = class(b"NSString\0");
    // SAFETY: `alloc` returns a +1 uninitialized instance; the follow-up
    // `initWithBytes:length:encoding:` consumes it and returns the initialized
    // +1 object (or nil, which `from_owned` maps to `None`). The byte pointer
    // is only read for `len` bytes during the call and is not retained.
    unsafe {
        let alloc: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
        let raw = alloc(cls, sel(b"alloc\0"));
        if raw.is_null() {
            return None;
        }
        let init: unsafe extern "C" fn(Id, Sel, *const u8, usize, usize) -> Id = msg();
        let obj = init(
            raw,
            sel(b"initWithBytes:length:encoding:\0"),
            s.as_ptr(),
            s.len(),
            NS_UTF8,
        );
        Obj::from_owned(obj)
    }
}

/// Read an `NSError`'s `localizedDescription` into an owned Rust `String`.
pub(crate) fn ns_error_string(err: Id) -> String {
    if err.is_null() {
        return "(nil NSError)".to_owned();
    }
    let _pool = AutoreleasePool::new();
    // SAFETY: `err` is a live `NSError` (Metal only writes a valid object or
    // leaves the out-param nil). `localizedDescription` and `UTF8String` both
    // return autoreleased/interior pointers valid until the pool pops, and the
    // bytes are copied into an owned `String` before that happens.
    unsafe {
        let desc: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let d = desc(err, sel(b"localizedDescription\0"));
        if d.is_null() {
            return "(no localizedDescription)".to_owned();
        }
        let utf8: unsafe extern "C" fn(Id, Sel) -> *const c_char = msg();
        let p = utf8(d, sel(b"UTF8String\0"));
        if p.is_null() {
            return "(no UTF8String)".to_owned();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

// ---------------------------------------------------------------------------
// Enumerations. Values are from the Metal headers; every one this module uses
// is exercised by a `super` test, so a wrong constant fails the suite rather
// than silently selecting a different format.
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

/// `MTLVertexFormat`, for the four instance layouts in `cell.metal`.
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub(crate) enum VertexFormat {
    /// `Uint8x4` — the fire `tsl` bytes.
    UChar4 = 3,
    /// `Unorm8x4` — every packed RGBA colour.
    UChar4Normalized = 9,
    /// `Uint16x4` — pixel rects, rain falloff, fire geometry.
    UShort4 = 15,
    /// `Float32x4` — glyph rect and UV.
    Float4 = 31,
    /// `Uint32` — the fire churn phase.
    UInt = 36,
}

/// `MTLBlendFactor`.
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub(crate) enum BlendFactor {
    Zero = 0,
    One = 1,
    SourceAlpha = 4,
    OneMinusSourceAlpha = 5,
}

/// `MTLResourceOptions`: `MTLResourceStorageModeShared` is `0 << 4`.
pub(crate) const RESOURCE_STORAGE_MODE_SHARED: usize = 0;

/// `MTLTextureUsage` bits.
pub(crate) const TEXTURE_USAGE_SHADER_READ: usize = 1;
pub(crate) const TEXTURE_USAGE_RENDER_TARGET: usize = 4;
/// Required on any texture that will be reinterpreted through
/// `newTextureViewWithPixelFormat:` — the Unorm/sRGB pair the base passes rely
/// on. Metal REFUSES the view without it, so it is not optional.
pub(crate) const TEXTURE_USAGE_PIXEL_FORMAT_VIEW: usize = 16;

// ---------------------------------------------------------------------------
// The object wrappers.
// ---------------------------------------------------------------------------

/// An `MTLDevice`.
#[derive(Debug)]
pub(crate) struct Device(Obj);

impl Device {
    /// The system default GPU, or `None` when the process has no Metal device
    /// (a headless CI box with no GPU, or a denied sandbox).
    pub(crate) fn system_default() -> Option<Self> {
        // SAFETY: `MTLCreateSystemDefaultDevice` is `CF_RETURNS_RETAINED`, so
        // the +1 is ours and `Obj` releases it exactly once.
        unsafe { Obj::from_owned(MTLCreateSystemDefaultDevice()) }.map(Self)
    }

    #[inline]
    pub(crate) const fn id(&self) -> Id {
        self.0.id()
    }

    /// The GPU's marketing name, for diagnostics.
    pub(crate) fn name(&self) -> String {
        let _pool = AutoreleasePool::new();
        // SAFETY: `-name` returns an autoreleased `NSString` owned by the pool
        // above; its UTF-8 bytes are copied before the pool pops.
        unsafe {
            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let s = get(self.id(), sel(b"name\0"));
            if s.is_null() {
                return String::new();
            }
            let utf8: unsafe extern "C" fn(Id, Sel) -> *const c_char = msg();
            let p = utf8(s, sel(b"UTF8String\0"));
            if p.is_null() {
                return String::new();
            }
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }

    /// Compile MSL source into an `MTLLibrary`.
    ///
    /// This is the RUNTIME compiler that ships inside Metal.framework, not the
    /// offline `xcrun metal` tool — deliberately, because it removes any build
    /// -time Xcode dependency (the CI images here carry Command Line Tools
    /// only, which has no `metal` binary at all). The cost is that a shader
    /// syntax error becomes a startup failure rather than a build failure,
    /// which is why `super`'s tests compile every library on every run.
    pub(crate) fn new_library(&self, source: &str) -> Result<Library, String> {
        let src = ns_string(source).ok_or_else(|| "NSString allocation failed".to_owned())?;
        let mut err: Id = ptr::null_mut();
        // SAFETY: `newLibraryWithSource:options:error:` is a `new*` family
        // method returning +1 (or nil with `err` set to an autoreleased
        // NSError). A nil `options` selects the compiler defaults.
        let lib = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id = msg();
            f(
                self.id(),
                sel(b"newLibraryWithSource:options:error:\0"),
                src.id(),
                ptr::null_mut(),
                &raw mut err,
            )
        };
        // SAFETY: `lib` is the +1 return described above.
        match unsafe { Obj::from_owned(lib) } {
            Some(o) => Ok(Library(o)),
            None => Err(ns_error_string(err)),
        }
    }

    /// Build an `MTLRenderPipelineState` from a configured descriptor.
    pub(crate) fn new_render_pipeline(
        &self,
        desc: &RenderPipelineDescriptor,
    ) -> Result<Obj, String> {
        let mut err: Id = ptr::null_mut();
        // SAFETY: `new*` family, +1 or nil-with-error. The descriptor is only
        // read during the call; Metal copies what it needs.
        let pso = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id = msg();
            f(
                self.id(),
                sel(b"newRenderPipelineStateWithDescriptor:error:\0"),
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
            let f: unsafe extern "C" fn(Id, Sel, usize, usize) -> Id = msg();
            Obj::from_owned(f(
                self.id(),
                sel(b"newBufferWithLength:options:\0"),
                len,
                RESOURCE_STORAGE_MODE_SHARED,
            ))
        }
    }

    /// A 2-D `MTLTexture`.
    pub(crate) fn new_texture_2d(
        &self,
        format: PixelFormat,
        width: usize,
        height: usize,
        usage: usize,
    ) -> Option<Obj> {
        let _pool = AutoreleasePool::new();
        // SAFETY: the descriptor factory returns an AUTORELEASED object (it is
        // a `+texture2DDescriptorWith...` convenience, not a `new*`), so it is
        // borrowed and left to the pool above. `newTextureWithDescriptor:` is
        // +1 and is the only value that escapes.
        unsafe {
            let dcls = class(b"MTLTextureDescriptor\0");
            let mk: unsafe extern "C" fn(ClassPtr, Sel, usize, usize, usize, bool) -> Id = msg();
            let d = mk(
                dcls,
                sel(b"texture2DDescriptorWithPixelFormat:width:height:mipmapped:\0"),
                format as usize,
                width,
                height,
                false,
            );
            if d.is_null() {
                return None;
            }
            let set_usage: unsafe extern "C" fn(Id, Sel, usize) = msg();
            set_usage(d, sel(b"setUsage:\0"), usage);
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
            Obj::from_owned(f(self.id(), sel(b"newTextureWithDescriptor:\0"), d))
        }
    }

    /// A default `MTLSamplerState` (nearest/clamp — the atlas sampler).
    pub(crate) fn new_sampler(&self) -> Option<Obj> {
        let _pool = AutoreleasePool::new();
        // SAFETY: `MTLSamplerDescriptor` is alloc/init (+1, released by the
        // `Obj` below); `newSamplerStateWithDescriptor:` is +1 and escapes.
        unsafe {
            let dcls = class(b"MTLSamplerDescriptor\0");
            let alloc: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
            let raw = alloc(dcls, sel(b"alloc\0"));
            let init: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let d = Obj::from_owned(init(raw, sel(b"init\0")))?;
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
            Obj::from_owned(f(
                self.id(),
                sel(b"newSamplerStateWithDescriptor:\0"),
                d.id(),
            ))
        }
    }

    /// An `MTLCommandQueue`.
    pub(crate) fn new_command_queue(&self) -> Option<Obj> {
        // SAFETY: `new*` family, +1.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            Obj::from_owned(f(self.id(), sel(b"newCommandQueue\0")))
        }
    }
}

/// An `MTLLibrary` — one compiled MSL translation unit.
#[derive(Debug)]
pub(crate) struct Library(Obj);

impl Library {
    /// Resolve a vertex/fragment entry point by name.
    pub(crate) fn function(&self, name: &str) -> Option<Obj> {
        let n = ns_string(name)?;
        // SAFETY: `newFunctionWithName:` is +1 (or nil when the entry point is
        // absent). The NSString is only read during the call.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
            Obj::from_owned(f(self.0.id(), sel(b"newFunctionWithName:\0"), n.id()))
        }
    }
}

/// An `MTLRenderPipelineDescriptor` under construction.
pub(crate) struct RenderPipelineDescriptor(Obj);

impl RenderPipelineDescriptor {
    pub(crate) fn new() -> Option<Self> {
        // SAFETY: alloc/init is +1; `Obj` releases it once on drop.
        unsafe {
            let cls = class(b"MTLRenderPipelineDescriptor\0");
            let alloc: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
            let raw = alloc(cls, sel(b"alloc\0"));
            let init: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            Obj::from_owned(init(raw, sel(b"init\0"))).map(Self)
        }
    }

    pub(crate) fn set_vertex_function(&self, f: &Obj) {
        // SAFETY: the descriptor RETAINS the function; both outlive the call.
        unsafe {
            let s: unsafe extern "C" fn(Id, Sel, Id) = msg();
            s(self.0.id(), sel(b"setVertexFunction:\0"), f.id());
        }
    }

    pub(crate) fn set_fragment_function(&self, f: &Obj) {
        // SAFETY: as above.
        unsafe {
            let s: unsafe extern "C" fn(Id, Sel, Id) = msg();
            s(self.0.id(), sel(b"setFragmentFunction:\0"), f.id());
        }
    }

    /// Colour attachment 0: pixel format plus an optional blend state.
    ///
    /// `blend` is `(src_rgb, dst_rgb, src_alpha, dst_alpha)`; `None` disables
    /// blending (the REPLACE pipelines).
    pub(crate) fn set_color_attachment(
        &self,
        format: PixelFormat,
        blend: Option<(BlendFactor, BlendFactor, BlendFactor, BlendFactor)>,
    ) {
        let _pool = AutoreleasePool::new();
        // SAFETY: `colorAttachments` and its subscript both return BORROWED
        // (autoreleased) objects owned by the pool above; they are only
        // mutated here and never released by this module.
        unsafe {
            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let atts = get(self.0.id(), sel(b"colorAttachments\0"));
            let sub: unsafe extern "C" fn(Id, Sel, usize) -> Id = msg();
            let a = sub(atts, sel(b"objectAtIndexedSubscript:\0"), 0);
            let setf: unsafe extern "C" fn(Id, Sel, usize) = msg();
            setf(a, sel(b"setPixelFormat:\0"), format as usize);
            let setb: unsafe extern "C" fn(Id, Sel, bool) = msg();
            setb(a, sel(b"setBlendingEnabled:\0"), blend.is_some());
            if let Some((srgb, drgb, sa, da)) = blend {
                let set: unsafe extern "C" fn(Id, Sel, usize) = msg();
                set(a, sel(b"setSourceRGBBlendFactor:\0"), srgb as usize);
                set(a, sel(b"setDestinationRGBBlendFactor:\0"), drgb as usize);
                set(a, sel(b"setSourceAlphaBlendFactor:\0"), sa as usize);
                set(a, sel(b"setDestinationAlphaBlendFactor:\0"), da as usize);
            }
        }
    }

    /// Attach a vertex layout.
    pub(crate) fn set_vertex_descriptor(&self, vd: &VertexDescriptor) {
        // SAFETY: the descriptor copies the vertex descriptor on assignment.
        unsafe {
            let s: unsafe extern "C" fn(Id, Sel, Id) = msg();
            s(self.0.id(), sel(b"setVertexDescriptor:\0"), vd.0.id());
        }
    }
}

/// An `MTLVertexDescriptor` — the instance-buffer layout.
pub(crate) struct VertexDescriptor(Obj);

impl VertexDescriptor {
    pub(crate) fn new() -> Option<Self> {
        // SAFETY: `+vertexDescriptor` is an autoreleased convenience, so it is
        // RETAINED here to +1 and released once by `Obj` — the borrow would
        // otherwise die with the caller's pool.
        unsafe {
            let cls = class(b"MTLVertexDescriptor\0");
            let mk: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
            let raw = mk(cls, sel(b"vertexDescriptor\0"));
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
            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let arr = get(self.0.id(), sel(b"attributes\0"));
            let sub: unsafe extern "C" fn(Id, Sel, usize) -> Id = msg();
            let a = sub(arr, sel(b"objectAtIndexedSubscript:\0"), index);
            let set: unsafe extern "C" fn(Id, Sel, usize) = msg();
            set(a, sel(b"setFormat:\0"), format as usize);
            set(a, sel(b"setOffset:\0"), offset);
            set(a, sel(b"setBufferIndex:\0"), buffer);
        }
    }

    /// Declare buffer `index`'s stride, stepping once per INSTANCE.
    ///
    /// `MTLVertexStepFunctionPerInstance` is `2`. Every aterm quad is 6 (or 4)
    /// vertices of one instance, so this is the step function for every layout
    /// in `cell.metal`.
    pub(crate) fn layout_per_instance(&self, index: usize, stride: usize) {
        const STEP_PER_INSTANCE: usize = 2;
        let _pool = AutoreleasePool::new();
        // SAFETY: as `attribute` — borrowed autoreleased accessors.
        unsafe {
            let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let arr = get(self.0.id(), sel(b"layouts\0"));
            let sub: unsafe extern "C" fn(Id, Sel, usize) -> Id = msg();
            let l = sub(arr, sel(b"objectAtIndexedSubscript:\0"), index);
            let set: unsafe extern "C" fn(Id, Sel, usize) = msg();
            set(l, sel(b"setStride:\0"), stride);
            set(l, sel(b"setStepFunction:\0"), STEP_PER_INSTANCE);
            set(l, sel(b"setStepRate:\0"), 1);
        }
    }
}

// ---------------------------------------------------------------------------
// Compute. The SHIPPED renderer issues no compute work at all (`required_
// features` is `Features::empty()`, and there is not one `ComputePipeline` in
// `renderer.rs`). These entry points exist ONLY so the parity test can evaluate
// the shipped MSL field functions at arbitrary coordinates and diff them
// against the CPU — see `super::tests::fire_field_matches_the_cpu_bit_for_bit`.
// ---------------------------------------------------------------------------

/// `MTLSize`, passed BY VALUE to the dispatch selectors.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct MtlSize {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) depth: usize,
}

impl Device {
    /// Build an `MTLComputePipelineState` from a kernel function.
    pub(crate) fn new_compute_pipeline(&self, f: &Obj) -> Result<Obj, String> {
        let mut err: Id = ptr::null_mut();
        // SAFETY: `new*` family, +1 or nil-with-error.
        let pso = unsafe {
            let m: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id = msg();
            m(
                self.id(),
                sel(b"newComputePipelineStateWithFunction:error:\0"),
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
/// whose GPU writes have already completed (the caller must have waited on the
/// command buffer).
pub(crate) unsafe fn buffer_u32s(buf: &Obj, count: usize) -> Vec<u32> {
    // SAFETY: `-contents` on a shared buffer returns a CPU-visible pointer
    // valid for the buffer's lifetime; the caller pins the length and the
    // completion ordering.
    unsafe {
        let c: unsafe extern "C" fn(Id, Sel) -> *const u32 = msg();
        let p = c(buf.id(), sel(b"contents\0"));
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
        let c: unsafe extern "C" fn(Id, Sel) -> *mut u8 = msg();
        let p = c(buf.id(), sel(b"contents\0"));
        if !p.is_null() {
            ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        }
    }
}

/// Run a one-shot compute dispatch and block until it completes.
///
/// `buffers` are bound at indices `0..buffers.len()`.
pub(crate) fn dispatch_compute(queue: &Obj, pso: &Obj, buffers: &[&Obj], grid: MtlSize) {
    let _pool = AutoreleasePool::new();
    // SAFETY: `commandBuffer` and `computeCommandEncoder` return AUTORELEASED
    // objects owned by the pool above — they must NOT be released here, and
    // they stay alive until `waitUntilCompleted` returns because the pool
    // outlives it. Every setter below is a plain void message on live objects.
    unsafe {
        let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let cb = get(queue.id(), sel(b"commandBuffer\0"));
        let enc = get(cb, sel(b"computeCommandEncoder\0"));
        let set_pso: unsafe extern "C" fn(Id, Sel, Id) = msg();
        set_pso(enc, sel(b"setComputePipelineState:\0"), pso.id());
        let set_buf: unsafe extern "C" fn(Id, Sel, Id, usize, usize) = msg();
        for (i, b) in buffers.iter().enumerate() {
            set_buf(enc, sel(b"setBuffer:offset:atIndex:\0"), b.id(), 0, i);
        }
        // A 8x8 threadgroup is safely under the 1024-thread minimum every Metal
        // device guarantees, so no per-device query is needed.
        let tg = MtlSize {
            width: 8,
            height: 8,
            depth: 1,
        };
        let disp: unsafe extern "C" fn(Id, Sel, MtlSize, MtlSize) = msg();
        disp(
            enc,
            sel(b"dispatchThreads:threadsPerThreadgroup:\0"),
            grid,
            tg,
        );
        let void_msg: unsafe extern "C" fn(Id, Sel) = msg();
        void_msg(enc, sel(b"endEncoding\0"));
        void_msg(cb, sel(b"commit\0"));
        void_msg(cb, sel(b"waitUntilCompleted\0"));
    }
}

// ---------------------------------------------------------------------------
// Render encoding. Everything above this line builds objects; this section is
// what actually puts work on the GPU — a colour attachment, a draw, and a
// texture -> buffer copy so the result can be read on the CPU.
//
// The shipped blit is a THREE-VERTEX fullscreen triangle with no vertex buffer
// (`vs_blit` synthesises its own positions from `[[vertex_id]]`), which is why
// there is no vertex-buffer binding here at all.
// ---------------------------------------------------------------------------

/// `MTLLoadAction::Clear`.
pub(crate) const LOAD_ACTION_CLEAR: usize = 2;
/// `MTLStoreAction::Store`.
pub(crate) const STORE_ACTION_STORE: usize = 1;
/// `MTLPrimitiveType::Triangle`.
pub(crate) const PRIMITIVE_TYPE_TRIANGLE: usize = 3;

/// `MTLClearColor`, passed BY VALUE to `-setClearColor:`.
///
/// Four `double`s is a homogeneous floating-point aggregate on
/// `aarch64-apple-darwin`, so it travels in `v0..v3` — `repr(C)` reproduces
/// that exactly, which is why this must not be a tuple or an array of `f64`
/// behind a reference.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClearColor {
    pub(crate) r: f64,
    pub(crate) g: f64,
    pub(crate) b: f64,
    pub(crate) a: f64,
}

/// `MTLOrigin`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct MtlOrigin {
    pub(crate) x: usize,
    pub(crate) y: usize,
    pub(crate) z: usize,
}

/// `MTLRegion` — an origin plus a size, passed BY VALUE to `-replaceRegion:…`.
/// 48 bytes, so the AAPCS passes it indirectly; `repr(C)` makes Rust do the
/// same thing the Objective-C caller does.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct MtlRegion {
    pub(crate) origin: MtlOrigin,
    pub(crate) size: MtlSize,
}

impl MtlRegion {
    /// The whole of a 2-D mip level.
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
    // SAFETY: the caller pins the format/extent/length agreement above;
    // `replaceRegion:` copies out of `bytes` before it returns and does not
    // retain the pointer.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, MtlRegion, usize, *const u8, usize) = msg();
        f(
            tex.id(),
            sel(b"replaceRegion:mipmapLevel:withBytes:bytesPerRow:\0"),
            region,
            0,
            bytes.as_ptr(),
            bytes_per_row,
        );
    }
}

/// Run the fullscreen-triangle pass and copy the result into `readback`.
///
/// One command buffer holds BOTH the render pass and the texture -> buffer
/// blit, so the copy is ordered after the draw by Metal's own encoder ordering
/// and no explicit barrier is needed; `waitUntilCompleted` is what makes the
/// bytes visible to the CPU. `readback` must be a SHARED-storage buffer, so
/// there is no `synchronizeResource:` here — the managed path would need one
/// and this deliberately never takes it.
///
/// Bindings mirror `blit.metal` exactly: fragment texture 0, fragment sampler
/// 0, fragment buffer 2 (the `Blit` uniform). Those indices are the
/// hand-maintained twin of the WGSL `@group(0) @binding(n)` map — see
/// `super::blit`.
#[expect(
    clippy::too_many_arguments,
    reason = "one-shot encoder: every Metal object the pass binds is an independent input, and \
              grouping them into a struct would only move the same list one layer out"
)]
pub(crate) fn draw_fullscreen_and_read(
    queue: &Obj,
    pso: &Obj,
    dst: &Obj,
    dst_w: usize,
    dst_h: usize,
    src_tex: &Obj,
    sampler: &Obj,
    uniform: &Obj,
    readback: &Obj,
    bytes_per_row: usize,
) {
    let _pool = AutoreleasePool::new();
    // SAFETY: `renderPassDescriptor`, `colorAttachments`, the subscript,
    // `commandBuffer` and `renderCommandEncoderWithDescriptor:` all return
    // AUTORELEASED objects owned by the pool above — none is released here, and
    // all outlive `waitUntilCompleted` because the pool does. Every other send
    // is a plain void message on a live object, with the prototype written out
    // in full at the call site.
    unsafe {
        let rp_cls = class(b"MTLRenderPassDescriptor\0");
        let mk: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
        let rp = mk(rp_cls, sel(b"renderPassDescriptor\0"));

        let get: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        let atts = get(rp, sel(b"colorAttachments\0"));
        let sub: unsafe extern "C" fn(Id, Sel, usize) -> Id = msg();
        let a0 = sub(atts, sel(b"objectAtIndexedSubscript:\0"), 0);

        let set_obj: unsafe extern "C" fn(Id, Sel, Id) = msg();
        set_obj(a0, sel(b"setTexture:\0"), dst.id());
        let set_usize: unsafe extern "C" fn(Id, Sel, usize) = msg();
        set_usize(a0, sel(b"setLoadAction:\0"), LOAD_ACTION_CLEAR);
        set_usize(a0, sel(b"setStoreAction:\0"), STORE_ACTION_STORE);
        // `wgpu::LoadOp::Clear(wgpu::Color::BLACK)` is opaque black; the pass
        // writes every pixel, so this only decides what an aborted pass shows.
        let set_clear: unsafe extern "C" fn(Id, Sel, ClearColor) = msg();
        set_clear(
            a0,
            sel(b"setClearColor:\0"),
            ClearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );

        let cb = get(queue.id(), sel(b"commandBuffer\0"));
        let mk_enc: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
        let enc = mk_enc(cb, sel(b"renderCommandEncoderWithDescriptor:\0"), rp);

        set_obj(enc, sel(b"setRenderPipelineState:\0"), pso.id());
        let set_tex: unsafe extern "C" fn(Id, Sel, Id, usize) = msg();
        set_tex(enc, sel(b"setFragmentTexture:atIndex:\0"), src_tex.id(), 0);
        set_tex(
            enc,
            sel(b"setFragmentSamplerState:atIndex:\0"),
            sampler.id(),
            0,
        );
        let set_buf: unsafe extern "C" fn(Id, Sel, Id, usize, usize) = msg();
        set_buf(
            enc,
            sel(b"setFragmentBuffer:offset:atIndex:\0"),
            uniform.id(),
            0,
            2,
        );
        let draw: unsafe extern "C" fn(Id, Sel, usize, usize, usize) = msg();
        draw(
            enc,
            sel(b"drawPrimitives:vertexStart:vertexCount:\0"),
            PRIMITIVE_TYPE_TRIANGLE,
            0,
            3,
        );
        let void_msg: unsafe extern "C" fn(Id, Sel) = msg();
        void_msg(enc, sel(b"endEncoding\0"));

        // Texture -> buffer, in the SAME command buffer, so it is ordered after
        // the pass above without an explicit barrier.
        let blit = get(cb, sel(b"blitCommandEncoder\0"));
        let copy: unsafe extern "C" fn(
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
        ) = msg();
        copy(
            blit,
            sel(b"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:\0"),
            dst.id(),
            0,
            0,
            MtlOrigin { x: 0, y: 0, z: 0 },
            MtlSize {
                width: dst_w,
                height: dst_h,
                depth: 1,
            },
            readback.id(),
            0,
            bytes_per_row,
            bytes_per_row * dst_h,
        );
        void_msg(blit, sel(b"endEncoding\0"));

        void_msg(cb, sel(b"commit\0"));
        void_msg(cb, sel(b"waitUntilCompleted\0"));
    }
}

/// Read a shared-storage buffer's mapped bytes.
///
/// # Safety
/// As [`buffer_u32s`], for `len` bytes.
pub(crate) unsafe fn buffer_bytes(buf: &Obj, len: usize) -> Vec<u8> {
    // SAFETY: `-contents` on a shared buffer is CPU-visible for the buffer's
    // lifetime; the caller pins the length and the completion ordering.
    unsafe {
        let c: unsafe extern "C" fn(Id, Sel) -> *const u8 = msg();
        let p = c(buf.id(), sel(b"contents\0"));
        if p.is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(p, len).to_vec()
    }
}
