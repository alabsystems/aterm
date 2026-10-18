// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GLSL ES 3.00 twins (`shaders/<library>.glsl`), compiled into the binary.
//! `crate::shader_twins` holds them to the pipeline table's entry points and
//! bind column; `pipelines` composes one stage of one row from them.

use crate::pipeline_table::ShaderLibrary;

/// The GLSL twin of one library.
pub(crate) const fn source(library: ShaderLibrary) -> &'static str {
    match library {
        ShaderLibrary::Cell => include_str!("../../shaders/cell.glsl"),
        ShaderLibrary::Blit => include_str!("../../shaders/blit.glsl"),
        ShaderLibrary::HdrGlow => include_str!("../../shaders/hdr_glow.glsl"),
        ShaderLibrary::Tray => include_str!("../../shaders/tray.glsl"),
        ShaderLibrary::Bloom => include_str!("../../shaders/bloom.glsl"),
        ShaderLibrary::Shimmer => include_str!("../../shaders/shimmer.glsl"),
    }
}
