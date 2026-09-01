// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NO-SECOND-SPELLING GUARD.
//!
//! `crates/aterm-gpu/src/pipeline_table.rs` is the one declaration of every
//! render pipeline aterm builds, and both backends read it. That makes drift
//! between them impossible — but only for as long as nobody writes a pipeline's
//! state down a second time. This test is what stops that: it scans the
//! renderer's source for the spellings that a re-inlined descriptor would need,
//! and fails if any of them come back.
//!
//! # Why this is a source scan and not a type
//!
//! Nothing in Rust's type system forbids `write_mask: wgpu::ColorWrites::ALL`
//! next to a table lookup — the descriptor is a plain struct literal and always
//! will be. The defect this file guards against is not a type error; it is
//! somebody adding a nineteenth pipeline the honest-looking way, by copying the
//! forty lines above it. The scan is the only thing that can see that.
//!
//! It is armed like every other guard here: re-inline ONE literal at ONE
//! construction site and this test goes red.
//!
//! # What it deliberately does not cover
//!
//! `metal/mod.rs`'s `StateProbe` harness calls `set_color_attachment` with
//! arbitrary masks and blend factors on purpose — it exists to prove on the GPU
//! that the FFI's write mask and blend setters are honoured, which needs states
//! no shipping pipeline uses. It is test-only and is not scanned. The shipping
//! Metal path (`metal/blit.rs`) is.

/// The renderer, as text. `include_str!` rather than a file read so the test
/// binary cannot be run against a stale or missing tree.
const RENDERER: &str = include_str!("../src/renderer.rs");
/// The shipping first-party Metal blit, as text.
const METAL_BLIT: &str = include_str!("../src/metal/blit.rs");

/// Fail loudly if `needle` appears in `src` at all, naming the lines.
fn forbid(src: &str, file: &str, needle: &str, why: &str) {
    let hits: Vec<String> = src
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, l)| format!("  {}:{}: {}", file, i + 1, l.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "`{needle}` is spelled in {file}, and it must not be: {why}\n{}",
        hits.join("\n")
    );
}

/// THE GUARD. Every piece of pipeline state a `wgpu` descriptor can carry has
/// exactly one spelling in this crate, and it is in `pipeline_table.rs`.
#[test]
fn the_renderer_spells_no_pipeline_state_of_its_own() {
    // POSITIVE CONTROL FIRST. A scan that reads the wrong file, or a file whose
    // shape changed out from under it, passes trivially — so prove the text is
    // the renderer and that it still builds pipelines at all before concluding
    // anything from an absence.
    assert!(
        RENDERER.len() > 100_000,
        "the renderer source did not load (got {} bytes)",
        RENDERER.len()
    );
    assert!(
        RENDERER.contains("fn build_table_pipeline("),
        "the single table-driven pipeline builder is gone; this guard is \
         checking for the absence of literals in a file that no longer builds \
         pipelines the way it describes"
    );

    // ONE construction site. Every pipeline in the crate is built by
    // `build_table_pipeline`, which is the only function that may assemble a
    // `RenderPipelineDescriptor`.
    let sites = RENDERER.matches("create_render_pipeline(").count();
    assert_eq!(
        sites, 1,
        "there must be exactly ONE `create_render_pipeline(` call in the \
         renderer (inside `build_table_pipeline`); found {sites}. A new call \
         site is a new place for a blend factor to be spelled by hand — the \
         defect this crate's pipeline table exists to make impossible."
    );

    // The literals a re-inlined descriptor needs. Each is a state THE PIPELINE
    // TABLE declares, and each has a documented drift to its name.
    for (needle, why) in [
        (
            "wgpu::BlendState",
            "a pipeline's blend state is `PipelineSpec::blend`; the Metal side \
             reads the same field, and a second spelling is how the fire and \
             EDR pipeline tests came to certify a state the renderer does not use",
        ),
        (
            "wgpu::BlendComponent",
            "blend halves are `pipeline_table::BlendComponent`",
        ),
        (
            "wgpu::BlendFactor",
            "blend factors are `pipeline_table::Factor`, a closed set the Metal \
             mapping is total over",
        ),
        (
            "wgpu::BlendOperation",
            "the blend equation is `pipeline_table::BlendOp`",
        ),
        (
            "wgpu::ColorWrites",
            "the write mask is `PipelineSpec::write_mask`. Both backends default \
             it to ALL, so a forgotten one is silent until a translucent window \
             goes opaque",
        ),
        (
            "wgpu::ColorTargetState",
            "the colour target comes from `PipelineSpec::wgpu_color_target`, \
             which resolves the row's `TargetRole` against the context's formats",
        ),
        (
            "wgpu::VertexBufferLayout",
            "vertex layouts are `pipeline_table::VertexLayout`; the wgpu arrays \
             are derived from the same declaration the Metal descriptor is",
        ),
        (
            "wgpu::VertexAttribute",
            "attributes are `pipeline_table::VertexAttr`",
        ),
        (
            "vertex_attr_array!",
            "the attribute arrays are derived from the table, not spelled by macro",
        ),
        (
            "wgpu::PrimitiveTopology",
            "topology is `PipelineSpec::topology` — the tray's triangle strip is \
             a table row, not a call-site override",
        ),
        (
            "write_mask:",
            "see `wgpu::ColorWrites` above; this catches the field even when the \
             value is written unqualified",
        ),
        (
            "array_stride:",
            "the instance stride is `VertexLayoutSpec::stride`, which the Rust \
             structs are const-asserted against",
        ),
        (
            "step_mode:",
            "every aterm vertex buffer steps per INSTANCE; the table says so once",
        ),
    ] {
        forbid(RENDERER, "src/renderer.rs", needle, why);
    }

    // Entry points, specifically. `entry_point:` itself is legitimate — it is
    // how `build_table_pipeline` passes `spec.vs` / `spec.fs` — so what is
    // forbidden is a STRING LITERAL in that slot. This is the drift that let
    // `renderer.rs` ask for `vs_fs` while the MSL defined `vs_fs_bloom`.
    forbid(
        RENDERER,
        "src/renderer.rs",
        "entry_point: Some(\"",
        "entry points are `PipelineSpec::vs` / `::fs`, which the WGSL and MSL \
         rosters are both checked against",
    );
}

/// The shipping Metal path reads the same rows. `metal/blit.rs` used to spell
/// the blit's write mask and blend by hand, citing `renderer.rs` LINE NUMBERS
/// in a comment — which is documentation, not coupling, and goes stale silently.
#[test]
fn the_metal_blit_spells_no_pipeline_state_of_its_own() {
    assert!(
        METAL_BLIT.contains("Pipeline::Blit.spec()"),
        "the Metal blit no longer builds from the pipeline table"
    );
    for (needle, why) in [
        (
            "set_color_attachment",
            "the colour attachment is configured by `metal::pipelines::build` \
             from the pipeline table row",
        ),
        (
            "ColorWriteMask::",
            "the write mask is `PipelineSpec::write_mask`, mapped by \
             `metal::pipelines::metal_write_mask`",
        ),
        (
            "BlendFactor::",
            "blend factors are `pipeline_table::Factor`, mapped by \
             `metal::pipelines::metal_blend`",
        ),
    ] {
        forbid(METAL_BLIT, "src/metal/blit.rs", needle, why);
    }
}
