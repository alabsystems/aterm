// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The hand-written binding to EGL 1.5, GLX 1.4 and OpenGL ES 3.0: the types,
//! the constants the GL arm names, and the function tables.
//!
//! Nothing links against a GL library. On Linux `Egl::open` `dlopen`s
//! `libEGL.so.1` (the vendor-neutral dispatcher every Linux GL stack ships:
//! Mesa, NVIDIA, glvnd) and reads its entry points with `dlsym`; every GL
//! function then comes from `eglGetProcAddress`, which EGL 1.5 (and
//! `EGL_KHR_get_all_proc_addresses` before it) defines for core functions too,
//! so `libGLESv2` is never opened. This is how wgpu's GL backend loads as well;
//! aterm's own table names only the ~65 functions the renderer calls. An X11
//! window system's device is GLX's instead (`super::glx`): `Glx::open`
//! `dlopen`s `libGLX.so.0` and `libX11.so.6` the same way, and GL comes from
//! `glXGetProcAddressARB`.
//!
//! On wasm32 there is no EGL. The same GL table is filled from the module's
//! `aterm_gl` imports (`GlFns::imported`): each GL function under its GL name
//! and C signature, which the glue `verify glue` writes implements over
//! WebGL2 (`crates/aterm-verify/src/wasm_glue/webgl.rs`), and `webgl` names
//! the context calls of its own (docs/AUDIT-third-party-2026-10-15.md,
//! wasm-platform WP.8). A pointer is an offset into the module's memory there,
//! as it is an address natively.
//!
//! The tables are plain structs of `unsafe extern "C" fn` pointers, filled once
//! by a `load` that refuses a missing symbol by name (on wasm32, from imports
//! the module cannot instantiate without). Calling one is `unsafe`: the caller
//! holds the context current on this thread (`super::device`) and passes
//! arguments GL defines.

#![allow(
    clippy::upper_case_acronyms,
    reason = "the Khronos type names (GLenum, EGLint, ...) keep their spelling so a \
              declaration reads against the registry"
)]

use std::ffi::{CStr, c_char, c_void};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use egl::*;
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) use glx::*;

// ---------------------------------------------------------------- GL types

pub(crate) type GLenum = u32;
pub(crate) type GLuint = u32;
pub(crate) type GLint = i32;
pub(crate) type GLsizei = i32;
pub(crate) type GLboolean = u8;
pub(crate) type GLfloat = f32;
pub(crate) type GLintptr = isize;
pub(crate) type GLsizeiptr = isize;
pub(crate) type GLchar = c_char;

pub(crate) const GL_FALSE: GLboolean = 0;
pub(crate) const GL_TRUE: GLboolean = 1;
pub(crate) const GL_NO_ERROR: GLenum = 0;
/// `KHR_robustness` (core in GLES 3.2): the context was lost.
pub(crate) const GL_CONTEXT_LOST: GLenum = 0x0507;
/// WebGL's spelling of a lost context (`CONTEXT_LOST_WEBGL`), which
/// `getError` answers once when the browser loses the context.
#[cfg(target_arch = "wasm32")]
pub(crate) const GL_CONTEXT_LOST_WEBGL: GLenum = 0x9242;
/// `KHR_robustness` (core in GLES 3.2): what `glGetGraphicsResetStatus`
/// answers after a reset the context caused, did not cause, or cannot place.
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) const GL_GUILTY_CONTEXT_RESET: GLenum = 0x8253;
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) const GL_INNOCENT_CONTEXT_RESET: GLenum = 0x8254;
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) const GL_UNKNOWN_CONTEXT_RESET: GLenum = 0x8255;
/// `NV_robustness_video_memory_purge`: the driver purged the context's video
/// memory (a VT switch, a mode switch, suspend and resume), which a context
/// made with `GLX_GENERATE_RESET_ON_VIDEO_MEMORY_PURGE_NV` hears as a reset.
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) const GL_PURGED_CONTEXT_RESET_NV: GLenum = 0x92BB;
/// `KHR_robustness`: the context's reset notification strategy, and its two
/// answers (the GLX context's test reads it).
#[cfg(all(test, not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) const GL_RESET_NOTIFICATION_STRATEGY: GLenum = 0x8256;
#[cfg(all(test, not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) const GL_LOSE_CONTEXT_ON_RESET: GLint = 0x8252;
/// `glGetGraphicsResetStatus` (GLES 3.2), or its `KHR_robustness`,
/// `EXT_robustness` or `ARB_robustness` spelling, read through the device's
/// proc-address lookup.
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) type GetGraphicsResetStatusFn = unsafe extern "C" fn() -> GLenum;
pub(crate) const GL_ZERO: GLenum = 0;
pub(crate) const GL_ONE: GLenum = 1;
pub(crate) const GL_ONE_MINUS_SRC_COLOR: GLenum = 0x0301;
pub(crate) const GL_SRC_ALPHA: GLenum = 0x0302;
pub(crate) const GL_ONE_MINUS_SRC_ALPHA: GLenum = 0x0303;
pub(crate) const GL_FUNC_ADD: GLenum = 0x8006;
pub(crate) const GL_TRIANGLES: GLenum = 0x0004;
pub(crate) const GL_TRIANGLE_STRIP: GLenum = 0x0005;
pub(crate) const GL_BLEND: GLenum = 0x0BE2;
pub(crate) const GL_SCISSOR_TEST: GLenum = 0x0C11;
pub(crate) const GL_DITHER: GLenum = 0x0BD0;
pub(crate) const GL_CULL_FACE: GLenum = 0x0B44;
pub(crate) const GL_DEPTH_TEST: GLenum = 0x0B71;
pub(crate) const GL_COLOR: GLenum = 0x1800;
pub(crate) const GL_RENDERER: GLenum = 0x1F01;
/// `WEBGL_debug_renderer_info`'s `UNMASKED_RENDERER_WEBGL`: the GPU's name,
/// where `GL_RENDERER` is the browser's.
#[cfg(target_arch = "wasm32")]
pub(crate) const GL_UNMASKED_RENDERER_WEBGL: GLenum = 0x9246;
pub(crate) const GL_VERSION: GLenum = 0x1F02;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const GL_EXTENSIONS: GLenum = 0x1F03;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const GL_NUM_EXTENSIONS: GLenum = 0x821D;
pub(crate) const GL_MAX_TEXTURE_SIZE: GLenum = 0x0D33;
pub(crate) const GL_TEXTURE_2D: GLenum = 0x0DE1;
pub(crate) const GL_TEXTURE0: GLenum = 0x84C0;
pub(crate) const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
pub(crate) const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
pub(crate) const GL_TEXTURE_WRAP_S: GLenum = 0x2802;
pub(crate) const GL_TEXTURE_WRAP_T: GLenum = 0x2803;
pub(crate) const GL_TEXTURE_MAX_LEVEL: GLenum = 0x813D;
pub(crate) const GL_NEAREST: GLint = 0x2600;
pub(crate) const GL_LINEAR: GLint = 0x2601;
pub(crate) const GL_CLAMP_TO_EDGE: GLint = 0x812F;
pub(crate) const GL_R8: GLenum = 0x8229;
pub(crate) const GL_RGBA8: GLenum = 0x8058;
pub(crate) const GL_SRGB8_ALPHA8: GLenum = 0x8C43;
pub(crate) const GL_RGBA16F: GLenum = 0x881A;
pub(crate) const GL_RED: GLenum = 0x1903;
pub(crate) const GL_RGBA: GLenum = 0x1908;
pub(crate) const GL_UNSIGNED_BYTE: GLenum = 0x1401;
pub(crate) const GL_SHORT: GLenum = 0x1402;
pub(crate) const GL_UNSIGNED_SHORT: GLenum = 0x1403;
pub(crate) const GL_UNSIGNED_INT: GLenum = 0x1405;
pub(crate) const GL_FLOAT: GLenum = 0x1406;
pub(crate) const GL_HALF_FLOAT: GLenum = 0x140B;
pub(crate) const GL_UNPACK_ROW_LENGTH: GLenum = 0x0CF2;
pub(crate) const GL_UNPACK_ALIGNMENT: GLenum = 0x0CF5;
pub(crate) const GL_PACK_ROW_LENGTH: GLenum = 0x0D02;
pub(crate) const GL_PACK_ALIGNMENT: GLenum = 0x0D05;
pub(crate) const GL_ARRAY_BUFFER: GLenum = 0x8892;
pub(crate) const GL_UNIFORM_BUFFER: GLenum = 0x8A11;
pub(crate) const GL_PIXEL_PACK_BUFFER: GLenum = 0x88EB;
pub(crate) const GL_STREAM_READ: GLenum = 0x88E1;
pub(crate) const GL_STREAM_DRAW: GLenum = 0x88E0;
pub(crate) const GL_DYNAMIC_DRAW: GLenum = 0x88E8;
pub(crate) const GL_FRAMEBUFFER: GLenum = 0x8D40;
pub(crate) const GL_READ_FRAMEBUFFER: GLenum = 0x8CA8;
#[cfg(not(wgpu_arm))]
pub(crate) const GL_DRAW_FRAMEBUFFER: GLenum = 0x8CA9;
#[cfg(not(wgpu_arm))]
pub(crate) const GL_COLOR_BUFFER_BIT: GLenum = 0x4000;
pub(crate) const GL_COLOR_ATTACHMENT0: GLenum = 0x8CE0;
pub(crate) const GL_FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
pub(crate) const GL_VERTEX_SHADER: GLenum = 0x8B31;
pub(crate) const GL_FRAGMENT_SHADER: GLenum = 0x8B30;
pub(crate) const GL_COMPILE_STATUS: GLenum = 0x8B81;
pub(crate) const GL_LINK_STATUS: GLenum = 0x8B82;
pub(crate) const GL_INFO_LOG_LENGTH: GLenum = 0x8B84;
pub(crate) const GL_INVALID_INDEX: GLuint = 0xFFFF_FFFF;
/// `EXT_sRGB_write_control`: toggles the sRGB encode (and the linear blend) on
/// an sRGB colour attachment, which is how one `SRGB8_ALPHA8` texture serves as
/// both the "sRGB view" and the "Unorm view" of the native format plan.
pub(crate) const GL_FRAMEBUFFER_SRGB_EXT: GLenum = 0x8DB9;
/// `EXT_texture_sRGB_decode`: sampling an sRGB texture raw (the Unorm view's
/// read half).
pub(crate) const GL_TEXTURE_SRGB_DECODE_EXT: GLenum = 0x8A48;
pub(crate) const GL_DECODE_EXT: GLint = 0x8A49;
pub(crate) const GL_SKIP_DECODE_EXT: GLint = 0x8A4A;

/// Declares a function table: one `unsafe extern "C" fn` field per entry
/// point; natively a `load` that fills every field from a symbol lookup and
/// refuses the first missing one by name, and on wasm32 an `imported` that
/// fills them from the `aterm_gl` imports of the same names.
macro_rules! fn_table {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$fmeta:meta])*
                $field:ident = $sym:literal : fn($($arg:ty),* $(,)?) $(-> $ret:ty)?;
            )*
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $( $(#[$fmeta])* pub(crate) $field: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
        }

        #[cfg(not(target_arch = "wasm32"))]
        impl $name {
            /// Fill the table from `lookup` (a symbol name to its address,
            /// null when absent).
            ///
            /// # Safety
            /// `lookup` must return either null or the address of the named
            /// function with exactly the signature declared here.
            pub(crate) unsafe fn load(
                lookup: &mut dyn FnMut(&CStr) -> *const c_void,
            ) -> Result<Self, String> {
                Ok(Self {
                    $( $(#[$fmeta])* $field: {
                        const SYM: &CStr = match CStr::from_bytes_with_nul(
                            concat!($sym, "\0").as_bytes(),
                        ) {
                            Ok(sym) => sym,
                            Err(_) => panic!("a symbol name holds no NUL"),
                        };
                        let p = lookup(SYM);
                        if p.is_null() {
                            return Err(format!(
                                "{} is missing {}",
                                stringify!($name),
                                $sym
                            ));
                        }
                        // SAFETY: a non-null address of this symbol, whose
                        // signature the caller vouches for; a data pointer and
                        // a fn pointer have the same size on every target.
                        unsafe {
                            std::mem::transmute::<
                                *const c_void,
                                unsafe extern "C" fn($($arg),*) $(-> $ret)?,
                            >(p)
                        }
                    }, )*
                })
            }
        }

        #[cfg(target_arch = "wasm32")]
        impl $name {
            /// The table over the module's `aterm_gl` imports, which the glue
            /// provides under these names and signatures.
            pub(crate) fn imported() -> Self {
                #[link(wasm_import_module = "aterm_gl")]
                unsafe extern "C" {
                    $(
                        $(#[$fmeta])*
                        #[link_name = $sym]
                        fn $field($(_: $arg),*) $(-> $ret)?;
                    )*
                }
                Self { $( $(#[$fmeta])* $field, )* }
            }
        }
    };
}
// The X11 binding's windowed test declares its Xlib with the same macro.
#[cfg(all(test, not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) use fn_table;

fn_table! {
    /// The OpenGL ES 3.0 entry points the GL arm calls, read through
    /// `eglGetProcAddress` or `glXGetProcAddressARB` (on wasm32, the module's
    /// `aterm_gl` imports).
    pub(crate) struct GlFns {
        get_error = "glGetError": fn() -> GLenum;
        get_string = "glGetString": fn(GLenum) -> *const u8;
        // The extension list, which a WebGL2 context does not enumerate this
        // way (and the browser arm does not read).
        #[cfg(not(target_arch = "wasm32"))]
        get_stringi = "glGetStringi": fn(GLenum, GLuint) -> *const u8;
        get_integerv = "glGetIntegerv": fn(GLenum, *mut GLint);
        enable = "glEnable": fn(GLenum);
        disable = "glDisable": fn(GLenum);
        blend_func_separate = "glBlendFuncSeparate": fn(GLenum, GLenum, GLenum, GLenum);
        blend_equation_separate = "glBlendEquationSeparate": fn(GLenum, GLenum);
        color_mask = "glColorMask": fn(GLboolean, GLboolean, GLboolean, GLboolean);
        scissor = "glScissor": fn(GLint, GLint, GLsizei, GLsizei);
        viewport = "glViewport": fn(GLint, GLint, GLsizei, GLsizei);
        clear_bufferfv = "glClearBufferfv": fn(GLenum, GLint, *const GLfloat);
        create_shader = "glCreateShader": fn(GLenum) -> GLuint;
        shader_source = "glShaderSource":
            fn(GLuint, GLsizei, *const *const GLchar, *const GLint);
        compile_shader = "glCompileShader": fn(GLuint);
        get_shaderiv = "glGetShaderiv": fn(GLuint, GLenum, *mut GLint);
        get_shader_info_log = "glGetShaderInfoLog":
            fn(GLuint, GLsizei, *mut GLsizei, *mut GLchar);
        delete_shader = "glDeleteShader": fn(GLuint);
        create_program = "glCreateProgram": fn() -> GLuint;
        attach_shader = "glAttachShader": fn(GLuint, GLuint);
        link_program = "glLinkProgram": fn(GLuint);
        get_programiv = "glGetProgramiv": fn(GLuint, GLenum, *mut GLint);
        get_program_info_log = "glGetProgramInfoLog":
            fn(GLuint, GLsizei, *mut GLsizei, *mut GLchar);
        delete_program = "glDeleteProgram": fn(GLuint);
        use_program = "glUseProgram": fn(GLuint);
        get_uniform_block_index = "glGetUniformBlockIndex": fn(GLuint, *const GLchar) -> GLuint;
        uniform_block_binding = "glUniformBlockBinding": fn(GLuint, GLuint, GLuint);
        get_uniform_location = "glGetUniformLocation": fn(GLuint, *const GLchar) -> GLint;
        uniform1i = "glUniform1i": fn(GLint, GLint);
        gen_textures = "glGenTextures": fn(GLsizei, *mut GLuint);
        delete_textures = "glDeleteTextures": fn(GLsizei, *const GLuint);
        bind_texture = "glBindTexture": fn(GLenum, GLuint);
        tex_storage_2d = "glTexStorage2D": fn(GLenum, GLsizei, GLenum, GLsizei, GLsizei);
        tex_sub_image_2d = "glTexSubImage2D": fn(
            GLenum, GLint, GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, *const c_void,
        );
        tex_parameteri = "glTexParameteri": fn(GLenum, GLenum, GLint);
        active_texture = "glActiveTexture": fn(GLenum);
        pixel_storei = "glPixelStorei": fn(GLenum, GLint);
        gen_samplers = "glGenSamplers": fn(GLsizei, *mut GLuint);
        delete_samplers = "glDeleteSamplers": fn(GLsizei, *const GLuint);
        sampler_parameteri = "glSamplerParameteri": fn(GLuint, GLenum, GLint);
        bind_sampler = "glBindSampler": fn(GLuint, GLuint);
        gen_buffers = "glGenBuffers": fn(GLsizei, *mut GLuint);
        delete_buffers = "glDeleteBuffers": fn(GLsizei, *const GLuint);
        bind_buffer = "glBindBuffer": fn(GLenum, GLuint);
        buffer_data = "glBufferData": fn(GLenum, GLsizeiptr, *const c_void, GLenum);
        buffer_sub_data = "glBufferSubData": fn(GLenum, GLintptr, GLsizeiptr, *const c_void);
        bind_buffer_base = "glBindBufferBase": fn(GLenum, GLuint, GLuint);
        gen_vertex_arrays = "glGenVertexArrays": fn(GLsizei, *mut GLuint);
        delete_vertex_arrays = "glDeleteVertexArrays": fn(GLsizei, *const GLuint);
        bind_vertex_array = "glBindVertexArray": fn(GLuint);
        enable_vertex_attrib_array = "glEnableVertexAttribArray": fn(GLuint);
        vertex_attrib_pointer = "glVertexAttribPointer":
            fn(GLuint, GLint, GLenum, GLboolean, GLsizei, *const c_void);
        vertex_attrib_i_pointer = "glVertexAttribIPointer":
            fn(GLuint, GLint, GLenum, GLsizei, *const c_void);
        vertex_attrib_divisor = "glVertexAttribDivisor": fn(GLuint, GLuint);
        draw_arrays_instanced = "glDrawArraysInstanced": fn(GLenum, GLint, GLsizei, GLsizei);
        gen_framebuffers = "glGenFramebuffers": fn(GLsizei, *mut GLuint);
        delete_framebuffers = "glDeleteFramebuffers": fn(GLsizei, *const GLuint);
        bind_framebuffer = "glBindFramebuffer": fn(GLenum, GLuint);
        framebuffer_texture_2d = "glFramebufferTexture2D":
            fn(GLenum, GLenum, GLenum, GLuint, GLint);
        check_framebuffer_status = "glCheckFramebufferStatus": fn(GLenum) -> GLenum;
        read_pixels = "glReadPixels":
            fn(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, *mut c_void);
        copy_tex_sub_image_2d = "glCopyTexSubImage2D":
            fn(GLenum, GLint, GLint, GLint, GLint, GLint, GLsizei, GLsizei);
        flush = "glFlush": fn();
        // The window and canvas posts; a build that also carries wgpu attaches
        // windows on wgpu.
        #[cfg_attr(
            wgpu_arm,
            allow(dead_code, reason = "the shipped builds' window and canvas posts")
        )]
        blit_framebuffer = "glBlitFramebuffer": fn(
            GLint, GLint, GLint, GLint, GLint, GLint, GLint, GLint, GLenum, GLenum,
        );
    }
}

/// The registry name of a GL error code.
pub(crate) fn gl_error_name(code: GLenum) -> String {
    match code {
        0x0500 => "GL_INVALID_ENUM".to_owned(),
        0x0501 => "GL_INVALID_VALUE".to_owned(),
        0x0502 => "GL_INVALID_OPERATION".to_owned(),
        0x0505 => "GL_OUT_OF_MEMORY".to_owned(),
        0x0506 => "GL_INVALID_FRAMEBUFFER_OPERATION".to_owned(),
        0x0507 => "GL_CONTEXT_LOST".to_owned(),
        0x9242 => "CONTEXT_LOST_WEBGL".to_owned(),
        other => format!("GL error {other:#x}"),
    }
}

/// The registry name of a `glGetGraphicsResetStatus` answer.
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) fn gl_reset_name(code: GLenum) -> String {
    match code {
        GL_NO_ERROR => "GL_NO_ERROR".to_owned(),
        GL_GUILTY_CONTEXT_RESET => "GL_GUILTY_CONTEXT_RESET".to_owned(),
        GL_INNOCENT_CONTEXT_RESET => "GL_INNOCENT_CONTEXT_RESET".to_owned(),
        GL_UNKNOWN_CONTEXT_RESET => "GL_UNKNOWN_CONTEXT_RESET".to_owned(),
        GL_PURGED_CONTEXT_RESET_NV => "GL_PURGED_CONTEXT_RESET_NV".to_owned(),
        other => format!("reset status {other:#x}"),
    }
}

/// A NUL-terminated C string at `p` as an owned `String` (empty for null).
///
/// # Safety
/// `p` is null or points at a NUL-terminated string that outlives the call.
pub(crate) unsafe fn c_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: non-null and NUL-terminated by the caller's contract.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// The WebGL2 context calls the glue's `aterm_gl` imports add to GL's own
/// (`crates/aterm-verify/src/wasm_glue/webgl.rs`): a context is made on a
/// canvas the module holds as a JS object, and GL calls act on the one made
/// current, as they act on the thread's current context natively.
#[cfg(target_arch = "wasm32")]
pub(crate) mod webgl {
    #[link(wasm_import_module = "aterm_gl")]
    unsafe extern "C" {
        /// A WebGL2 context on the canvas in object-table slot `canvas`
        /// (`getContext('webgl2', { antialias: false })`, the options wgpu's
        /// WebGL2 backend asked for): its id, or 0 when the canvas gives none
        /// (no WebGL2, another context already on it, not a canvas).
        pub(crate) safe fn context_create(canvas: u32) -> u32;
        /// Forget context `ctx` (unbinding it if current).
        pub(crate) safe fn context_drop(ctx: u32);
        /// Make `ctx` the context GL calls act on; 0 when it names none.
        pub(crate) safe fn make_current(ctx: u32) -> u32;
        /// 1 when the current context is lost (`isContextLost()`), or none
        /// is current.
        pub(crate) safe fn context_lost() -> u32;
        /// Size the current context's canvas, its default framebuffer, to
        /// `width` x `height` (a no-op when it already is).
        pub(crate) safe fn canvas_resize(width: u32, height: u32);
    }
}

/// EGL, the Linux arm's display, context and surface layer (no EGL on
/// wasm32, where the browser's canvas is all three).
#[cfg(not(target_arch = "wasm32"))]
mod egl {
    use std::ffi::{CStr, c_char, c_void};

    // ---------------------------------------------------------------- EGL types

    pub(crate) type EGLint = i32;
    pub(crate) type EGLBoolean = u32;
    pub(crate) type EGLenum = u32;
    pub(crate) type EGLAttrib = isize;
    pub(crate) type EGLDisplay = *mut c_void;
    pub(crate) type EGLConfig = *mut c_void;
    pub(crate) type EGLContext = *mut c_void;
    pub(crate) type EGLSurface = *mut c_void;

    pub(crate) const EGL_TRUE: EGLBoolean = 1;
    pub(crate) const EGL_NONE: EGLint = 0x3038;
    pub(crate) const EGL_ALPHA_SIZE: EGLint = 0x3021;
    pub(crate) const EGL_BLUE_SIZE: EGLint = 0x3022;
    pub(crate) const EGL_GREEN_SIZE: EGLint = 0x3023;
    pub(crate) const EGL_RED_SIZE: EGLint = 0x3024;
    pub(crate) const EGL_SURFACE_TYPE: EGLint = 0x3033;
    pub(crate) const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
    pub(crate) const EGL_PBUFFER_BIT: EGLint = 0x0001;
    pub(crate) const EGL_WINDOW_BIT: EGLint = 0x0004;
    #[cfg(not(wgpu_arm))]
    pub(crate) const EGL_NATIVE_VISUAL_ID: EGLint = 0x302E;
    pub(crate) const EGL_CONTEXT_LOST: EGLint = 0x300E;
    /// `EGL_KHR_platform_x11`: a display on an Xlib `Display*`, with the screen as
    /// an attribute.
    pub(crate) const EGL_PLATFORM_X11_KHR: EGLenum = 0x31D5;
    pub(crate) const EGL_PLATFORM_X11_SCREEN_KHR: EGLAttrib = 0x31D6;
    /// `EGL_KHR_platform_wayland`: a display on a `wl_display*`.
    pub(crate) const EGL_PLATFORM_WAYLAND_KHR: EGLenum = 0x31D8;
    /// `EGL_KHR_no_config_context`: a context made with no config can be made
    /// current with any surface of its display, whatever config made it.
    pub(crate) const EGL_NO_CONFIG_KHR: EGLConfig = std::ptr::null_mut();
    pub(crate) const EGL_OPENGL_ES3_BIT: EGLint = 0x0040;
    pub(crate) const EGL_OPENGL_ES_API: EGLenum = 0x30A0;
    pub(crate) const EGL_CONTEXT_MAJOR_VERSION: EGLint = 0x3098;
    pub(crate) const EGL_CONTEXT_MINOR_VERSION: EGLint = 0x30FB;
    pub(crate) const EGL_WIDTH: EGLint = 0x3057;
    pub(crate) const EGL_HEIGHT: EGLint = 0x3056;
    pub(crate) const EGL_EXTENSIONS: EGLint = 0x3055;
    /// `EGL_MESA_platform_surfaceless`: a display with no window system, for the
    /// headless context.
    pub(crate) const EGL_PLATFORM_SURFACELESS_MESA: EGLenum = 0x31DD;
    /// `EGL_EXT_platform_device`: a display on one enumerated device.
    pub(crate) const EGL_PLATFORM_DEVICE_EXT: EGLenum = 0x313F;
    /// `EGLDeviceEXT` (`EGL_EXT_device_base`).
    pub(crate) type EGLDeviceEXT = *mut c_void;
    /// `eglQueryDevicesEXT` (`EGL_EXT_device_enumeration`), read through
    /// `eglGetProcAddress`.
    pub(crate) type QueryDevicesFn =
        unsafe extern "C" fn(EGLint, *mut EGLDeviceEXT, *mut EGLint) -> EGLBoolean;
    /// `eglQueryDeviceStringEXT` (`EGL_EXT_device_query`).
    pub(crate) type QueryDeviceStringFn =
        unsafe extern "C" fn(EGLDeviceEXT, EGLint) -> *const c_char;
    /// `EGL_KHR_display_reference`: reference-counted `eglInitialize` /
    /// `eglTerminate`, and a display distinct from one opened without it, so the
    /// GL arm and wgpu's GL backend (the test oracle) never terminate each other's
    /// display.
    pub(crate) const EGL_TRACK_REFERENCES_KHR: EGLAttrib = 0x3352;

    pub(crate) const EGL_NO_DISPLAY: EGLDisplay = std::ptr::null_mut();
    pub(crate) const EGL_NO_CONTEXT: EGLContext = std::ptr::null_mut();
    pub(crate) const EGL_NO_SURFACE: EGLSurface = std::ptr::null_mut();
    pub(crate) const EGL_DEFAULT_DISPLAY: *mut c_void = std::ptr::null_mut();

    fn_table! {
        /// The EGL 1.5 entry points, read from `libEGL.so.1` by `dlsym`.
        pub(crate) struct EglFns {
            get_proc_address = "eglGetProcAddress": fn(*const c_char) -> *const c_void;
            get_error = "eglGetError": fn() -> EGLint;
            get_platform_display = "eglGetPlatformDisplay":
                fn(EGLenum, *mut c_void, *const EGLAttrib) -> EGLDisplay;
            initialize = "eglInitialize": fn(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean;
            terminate = "eglTerminate": fn(EGLDisplay) -> EGLBoolean;
            query_string = "eglQueryString": fn(EGLDisplay, EGLint) -> *const c_char;
            bind_api = "eglBindAPI": fn(EGLenum) -> EGLBoolean;
            choose_config = "eglChooseConfig":
                fn(EGLDisplay, *const EGLint, *mut EGLConfig, EGLint, *mut EGLint) -> EGLBoolean;
            create_context = "eglCreateContext":
                fn(EGLDisplay, EGLConfig, EGLContext, *const EGLint) -> EGLContext;
            destroy_context = "eglDestroyContext": fn(EGLDisplay, EGLContext) -> EGLBoolean;
            make_current = "eglMakeCurrent":
                fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> EGLBoolean;
            create_pbuffer_surface = "eglCreatePbufferSurface":
                fn(EGLDisplay, EGLConfig, *const EGLint) -> EGLSurface;
            destroy_surface = "eglDestroySurface": fn(EGLDisplay, EGLSurface) -> EGLBoolean;
            get_display = "eglGetDisplay": fn(*mut c_void) -> EGLDisplay;
        }
    }

    #[cfg(not(wgpu_arm))]
    fn_table! {
        /// The EGL 1.5 entry points a window surface needs, read through
        /// `eglGetProcAddress` when the first window attaches (the shipped build's
        /// swapchain; `super::surface`).
        pub(crate) struct EglWindowFns {
            get_config_attrib = "eglGetConfigAttrib":
                fn(EGLDisplay, EGLConfig, EGLint, *mut EGLint) -> EGLBoolean;
            create_platform_window_surface = "eglCreatePlatformWindowSurface":
                fn(EGLDisplay, EGLConfig, *mut c_void, *const EGLAttrib) -> EGLSurface;
            swap_buffers = "eglSwapBuffers": fn(EGLDisplay, EGLSurface) -> EGLBoolean;
            swap_interval = "eglSwapInterval": fn(EGLDisplay, EGLint) -> EGLBoolean;
            query_surface = "eglQuerySurface":
                fn(EGLDisplay, EGLSurface, EGLint, *mut EGLint) -> EGLBoolean;
        }
    }

    /// `libEGL.so.1`, opened once per process and never closed (a GL driver
    /// registers atexit handlers and thread-local state a `dlclose` would orphan).
    pub(crate) struct Egl {
        pub(crate) fns: EglFns,
    }

    // SAFETY: the table is immutable function pointers into a library that stays
    // loaded for the process's life; EGL's entry points are thread-safe.
    unsafe impl Send for Egl {}
    // SAFETY: as above; `&Egl` hands out only the immutable table.
    unsafe impl Sync for Egl {}

    impl Egl {
        /// The process's EGL, opened on first use. The error is the loader's
        /// (no `libEGL.so.1` on this machine, or a library missing an EGL 1.5
        /// entry point), cached so every caller reads the same answer.
        pub(crate) fn get() -> Result<&'static Egl, String> {
            static EGL: std::sync::OnceLock<Result<Egl, String>> = std::sync::OnceLock::new();
            EGL.get_or_init(Self::open).as_ref().map_err(Clone::clone)
        }

        fn open() -> Result<Egl, String> {
            let mut handle = std::ptr::null_mut();
            for name in [c"libEGL.so.1", c"libEGL.so"] {
                // SAFETY: a NUL-terminated library name; RTLD_LAZY binds on first
                // call. The handle is never closed (see the type's note).
                handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY) };
                if !handle.is_null() {
                    break;
                }
            }
            if handle.is_null() {
                return Err("no EGL library (libEGL.so.1) on this machine".to_owned());
            }
            // SAFETY: `dlsym` on a live handle returns the symbol's address or
            // null; the table declares each EGL 1.5 function's registry signature.
            let fns = unsafe {
                EglFns::load(&mut |sym: &CStr| libc::dlsym(handle, sym.as_ptr()).cast_const())
            }?;
            Ok(Egl { fns })
        }

        /// `eglGetError()`, spelled as the registry name for messages.
        pub(crate) fn error_name(&self) -> String {
            // SAFETY: eglGetError takes no arguments and is callable at any time.
            let code = unsafe { (self.fns.get_error)() };
            egl_error_name(code)
        }
    }

    /// The registry name of an EGL error code.
    pub(crate) fn egl_error_name(code: EGLint) -> String {
        match code {
            0x3000 => "EGL_SUCCESS".to_owned(),
            0x3001 => "EGL_NOT_INITIALIZED".to_owned(),
            0x3002 => "EGL_BAD_ACCESS".to_owned(),
            0x3003 => "EGL_BAD_ALLOC".to_owned(),
            0x3004 => "EGL_BAD_ATTRIBUTE".to_owned(),
            0x3005 => "EGL_BAD_CONFIG".to_owned(),
            0x3006 => "EGL_BAD_CONTEXT".to_owned(),
            0x3007 => "EGL_BAD_CURRENT_SURFACE".to_owned(),
            0x3008 => "EGL_BAD_DISPLAY".to_owned(),
            0x3009 => "EGL_BAD_MATCH".to_owned(),
            0x300A => "EGL_BAD_NATIVE_PIXMAP".to_owned(),
            0x300B => "EGL_BAD_NATIVE_WINDOW".to_owned(),
            0x300C => "EGL_BAD_PARAMETER".to_owned(),
            0x300D => "EGL_BAD_SURFACE".to_owned(),
            0x300E => "EGL_CONTEXT_LOST".to_owned(),
            other => format!("EGL error {other:#x}"),
        }
    }
}

/// GLX 1.4 and the three libX11 calls the X11 binding makes (`super::glx`):
/// the shipped build's X11 windows post through GLX.
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
mod glx {
    use std::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};

    // ---------------------------------------------------------------- GLX types

    /// An Xlib `Display *`.
    pub(crate) type XDisplay = *mut c_void;
    pub(crate) type GLXFBConfig = *mut c_void;
    pub(crate) type GLXContext = *mut c_void;
    /// A GLX drawable's XID (a `GLXWindow`, a `GLXPbuffer`); 0 is `None`.
    pub(crate) type GLXDrawable = c_ulong;

    pub(crate) const GLX_DOUBLEBUFFER: c_int = 5;
    pub(crate) const GLX_RED_SIZE: c_int = 8;
    pub(crate) const GLX_GREEN_SIZE: c_int = 9;
    pub(crate) const GLX_BLUE_SIZE: c_int = 10;
    pub(crate) const GLX_ALPHA_SIZE: c_int = 11;
    pub(crate) const GLX_X_VISUAL_TYPE: c_int = 0x22;
    pub(crate) const GLX_TRUE_COLOR: c_int = 0x8002;
    pub(crate) const GLX_VISUAL_ID: c_int = 0x800B;
    pub(crate) const GLX_DRAWABLE_TYPE: c_int = 0x8010;
    pub(crate) const GLX_RENDER_TYPE: c_int = 0x8011;
    pub(crate) const GLX_X_RENDERABLE: c_int = 0x8012;
    pub(crate) const GLX_WINDOW_BIT: c_int = 0x1;
    pub(crate) const GLX_PBUFFER_BIT: c_int = 0x4;
    pub(crate) const GLX_RGBA_BIT: c_int = 0x1;
    /// `GLX_ARB_multisample`.
    pub(crate) const GLX_SAMPLE_BUFFERS: c_int = 100_000;
    pub(crate) const GLX_PBUFFER_HEIGHT: c_int = 0x8040;
    pub(crate) const GLX_PBUFFER_WIDTH: c_int = 0x8041;
    /// `GLX_ARB_create_context` and `GLX_ARB_create_context_profile`.
    pub(crate) const GLX_CONTEXT_MAJOR_VERSION_ARB: c_int = 0x2091;
    pub(crate) const GLX_CONTEXT_MINOR_VERSION_ARB: c_int = 0x2092;
    pub(crate) const GLX_CONTEXT_PROFILE_MASK_ARB: c_int = 0x9126;
    /// `GLX_EXT_create_context_es2_profile`: an OpenGL ES context of the
    /// version asked (3.0 here), not only ES 2.
    pub(crate) const GLX_CONTEXT_ES2_PROFILE_BIT_EXT: c_int = 0x4;
    /// `GLX_ARB_create_context_robustness`: the context flags with their
    /// robust-access bit, and the reset notification strategy that loses the
    /// context on a reset (and says so through `glGetGraphicsResetStatus`).
    pub(crate) const GLX_CONTEXT_FLAGS_ARB: c_int = 0x2094;
    pub(crate) const GLX_CONTEXT_ROBUST_ACCESS_BIT_ARB: c_int = 0x4;
    pub(crate) const GLX_CONTEXT_RESET_NOTIFICATION_STRATEGY_ARB: c_int = 0x8256;
    pub(crate) const GLX_LOSE_CONTEXT_ON_RESET_ARB: c_int = 0x8252;
    /// `GLX_NV_robustness_video_memory_purge`: a purge of the context's video
    /// memory is reported as a reset (`GL_PURGED_CONTEXT_RESET_NV`).
    pub(crate) const GLX_GENERATE_RESET_ON_VIDEO_MEMORY_PURGE_NV: c_int = 0x20F7;

    /// `glXCreateContextAttribsARB` (`GLX_ARB_create_context`), read through
    /// `glXGetProcAddressARB`.
    pub(crate) type CreateContextAttribsFn =
        unsafe extern "C" fn(XDisplay, GLXFBConfig, GLXContext, c_int, *const c_int) -> GLXContext;
    /// `glXSwapIntervalEXT` (`GLX_EXT_swap_control`): one drawable's interval.
    pub(crate) type SwapIntervalExtFn = unsafe extern "C" fn(XDisplay, GLXDrawable, c_int);
    /// `glXSwapIntervalMESA` (`GLX_MESA_swap_control`): the current drawable's.
    pub(crate) type SwapIntervalMesaFn = unsafe extern "C" fn(c_uint) -> c_int;
    /// `glXSwapIntervalSGI` (`GLX_SGI_swap_control`): the current drawable's,
    /// 1 or more.
    pub(crate) type SwapIntervalSgiFn = unsafe extern "C" fn(c_int) -> c_int;

    fn_table! {
        /// The GLX 1.4 entry points, read from libGLX by `dlsym`.
        pub(crate) struct GlxFns {
            query_extensions_string = "glXQueryExtensionsString":
                fn(XDisplay, c_int) -> *const c_char;
            choose_fb_config = "glXChooseFBConfig":
                fn(XDisplay, c_int, *const c_int, *mut c_int) -> *mut GLXFBConfig;
            get_fb_config_attrib = "glXGetFBConfigAttrib":
                fn(XDisplay, GLXFBConfig, c_int, *mut c_int) -> c_int;
            get_proc_address = "glXGetProcAddressARB": fn(*const u8) -> *const c_void;
            make_context_current = "glXMakeContextCurrent":
                fn(XDisplay, GLXDrawable, GLXDrawable, GLXContext) -> c_int;
            destroy_context = "glXDestroyContext": fn(XDisplay, GLXContext);
            create_window = "glXCreateWindow":
                fn(XDisplay, GLXFBConfig, c_ulong, *const c_int) -> GLXDrawable;
            destroy_window = "glXDestroyWindow": fn(XDisplay, GLXDrawable);
            create_pbuffer = "glXCreatePbuffer":
                fn(XDisplay, GLXFBConfig, *const c_int) -> GLXDrawable;
            destroy_pbuffer = "glXDestroyPbuffer": fn(XDisplay, GLXDrawable);
            swap_buffers = "glXSwapBuffers": fn(XDisplay, GLXDrawable);
        }
    }

    fn_table! {
        /// The libX11 calls GLX needs beside it: freeing the config list
        /// `glXChooseFBConfig` returns, naming the screen's default visual,
        /// asking whether a compositing manager runs (`_NET_WM_CM_S<n>`), and
        /// a window's size.
        pub(crate) struct XlibFns {
            free = "XFree": fn(*mut c_void) -> c_int;
            default_visual = "XDefaultVisual": fn(XDisplay, c_int) -> *mut c_void;
            visual_id_from_visual = "XVisualIDFromVisual": fn(*mut c_void) -> c_ulong;
            intern_atom = "XInternAtom": fn(XDisplay, *const c_char, c_int) -> c_ulong;
            get_selection_owner = "XGetSelectionOwner": fn(XDisplay, c_ulong) -> c_ulong;
            get_geometry = "XGetGeometry": fn(
                XDisplay,
                c_ulong,
                *mut c_ulong,
                *mut c_int,
                *mut c_int,
                *mut c_uint,
                *mut c_uint,
                *mut c_uint,
                *mut c_uint,
            ) -> c_int;
        }
    }

    /// libGLX and libX11, opened once per process and never closed (as `Egl`).
    pub(crate) struct Glx {
        pub(crate) fns: GlxFns,
        pub(crate) x: XlibFns,
    }

    // SAFETY: immutable function pointers into libraries that stay loaded for
    // the process's life; Xlib is thread-safe once `XInitThreads` ran, which
    // the windowing crate's connection does first.
    unsafe impl Send for Glx {}
    // SAFETY: as above; `&Glx` hands out only the immutable tables.
    unsafe impl Sync for Glx {}

    impl Glx {
        /// The process's GLX, opened on first use; the error (no library, or
        /// one missing a GLX 1.4 entry point) is cached like `Egl::get`'s.
        pub(crate) fn get() -> Result<&'static Glx, String> {
            static GLX: std::sync::OnceLock<Result<Glx, String>> = std::sync::OnceLock::new();
            GLX.get_or_init(Self::open).as_ref().map_err(Clone::clone)
        }

        fn open() -> Result<Glx, String> {
            // glvnd's dispatcher first (every current Linux GL stack), then a
            // pre-glvnd stack's libGL.
            let gl = open_first(&[c"libGLX.so.0", c"libGL.so.1"])
                .ok_or("no GLX library (libGLX.so.0, libGL.so.1) on this machine")?;
            let x = open_first(&[c"libX11.so.6"]).ok_or("no libX11.so.6 on this machine")?;
            // SAFETY: `dlsym` on live handles returns the symbol's address or
            // null; the tables declare each function's registry signature.
            let fns = unsafe {
                GlxFns::load(&mut |sym: &CStr| libc::dlsym(gl, sym.as_ptr()).cast_const())
            }?;
            // SAFETY: as above, for libX11's declarations (Xlib.h).
            let x = unsafe {
                XlibFns::load(&mut |sym: &CStr| libc::dlsym(x, sym.as_ptr()).cast_const())
            }?;
            Ok(Glx { fns, x })
        }
    }

    /// The first of `names` `dlopen` opens (never closed; see `Glx`).
    fn open_first(names: &[&CStr]) -> Option<*mut c_void> {
        names.iter().find_map(|name| {
            // SAFETY: a NUL-terminated library name; RTLD_LAZY binds on first
            // call.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY) };
            (!handle.is_null()).then_some(handle)
        })
    }
}
