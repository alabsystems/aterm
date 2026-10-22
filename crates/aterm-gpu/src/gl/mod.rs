// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The first-party OpenGL ES 3.0 backend: Linux's renderer
//! (docs/AUDIT-third-party-2026-10-15.md GW.4), beside `metal` on macOS, and
//! the browser's over WebGL2 (wasm-platform WP.8). On Linux wgpu's GL and
//! Vulkan backends stay as the test oracle every GL row and frame is
//! byte-compared against on the same driver, the way the Metal arm's wgpu
//! oracle works on macOS; in a browser the oracle is the frames wgpu's WebGL2
//! backend drew, recorded before it left (`verify browser`).
//!
//! * [`ffi`]: EGL 1.5, GLX 1.4 and GLES 3.0, loaded at run time (`dlopen` of
//!   `libEGL.so.1` and `libGLX.so.0`, GL through `eglGetProcAddress` or
//!   `glXGetProcAddressARB`); nothing links a GL library. On wasm32 the same
//!   GL table over the module's `aterm_gl` imports.
//! * [`device`]: the EGL display, config and context, or on X11 the GLX
//!   context (or the canvas's WebGL2 context), its capabilities, and the
//!   guard that holds the context current.
//! * `glx`: an X11 window system's context, which is GLX's: NVIDIA's X11
//!   EGL posts nothing on an Xorg server its own driver runs.
//! * [`resources`]: textures, buffers and samplers.
//! * [`shaders`]: the GLSL ES twins of the WGSL, `shaders/<library>.glsl`.
//! * [`pipelines`]: programs and fixed-function state from the pipeline table.
//! * [`encoder`]: the frame and pass verbs the device layer forwards to.
//! * `surface`: window surfaces (`GLXWindow`s on X11, EGL surfaces on
//!   Wayland) and the post; `canvas`, its twin on a WebGL2 canvas.
//!
//! # Coordinates and colour
//!
//! Every texture the arm draws into stores its top row at row 0, as wgpu and
//! Metal do: the vertex stage flips clip-space y (naga's
//! `ADJUST_COORDINATE_SPACE`), so readbacks, copies, scissors and
//! `gl_FragCoord` need no conversion.
//!
//! The offscreen is `SRGB8_ALPHA8`. With `EXT_sRGB_write_control` and
//! `EXT_texture_sRGB_decode` (Mesa and the desktop drivers have both) one
//! texture serves both views of the native format plan: the base passes encode
//! and blend in linear light, the additive passes write raw bytes. Without them
//! the arm runs the downlevel plan wgpu's GL backend runs (`srgb_offscreen`
//! false).

pub(crate) mod device;
pub(crate) mod encoder;
pub(crate) mod ffi;
pub(crate) mod pipelines;
pub(crate) mod resources;
pub(crate) mod shaders;
// The X11 windows' GLX context and the window surfaces: the shipped build's
// swapchain (a build that also carries wgpu attaches windows on wgpu).
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) mod glx;
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) mod surface;
// The WebGL2 canvas: the browser's swapchain.
#[cfg(target_arch = "wasm32")]
pub(crate) mod canvas;

#[cfg(target_arch = "wasm32")]
pub(crate) use canvas::GlCanvasSurface as WindowSurface;
/// The swapchain a `GpuSurface` holds on the GL arm: a GLX or EGL window
/// surface, or a WebGL2 canvas.
#[cfg(all(not(wgpu_arm), not(target_arch = "wasm32")))]
pub(crate) use surface::GlWindowSurface as WindowSurface;

/// A headless device for a GL test; a machine without EGL or a GLES 3 driver
/// could not run the test.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn test_device() -> device::GlDevice {
    device::GlDevice::headless()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GL device: {e}")))
}
