// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GLSL ES 3.00 shader sources and binding checks.

use crate::pipeline_table::ShaderLibrary;

/// The GLSL twin of one library.
#[inline]
pub(crate) const fn source(library: ShaderLibrary) -> &'static str {
    match library {
        ShaderLibrary::Cell => include_str!("../../shaders/cell.glsl"),
        ShaderLibrary::Blit => include_str!("../../shaders/blit.glsl"),
        ShaderLibrary::HdrGlow => include_str!("../../shaders/hdr_glow.glsl"),
        ShaderLibrary::Tray => include_str!("../../shaders/tray.glsl"),
        ShaderLibrary::Bloom => include_str!("../../shaders/bloom.glsl"),
        ShaderLibrary::Shimmer => include_str!("../../shaders/shimmer.glsl"),
        // Metal's alone (`pipeline_table::TRAY_PRIMS`): this module compiles on macOS for its
        // binding check, which reads the shared rows, never this library.
        #[cfg(target_os = "macos")]
        ShaderLibrary::TrayPrims => "",
    }
}

/// The binding check.
#[cfg(all(test, any(target_os = "macos", gl_arm)))]
#[path = "shaders_tests.rs"]
mod tests;
