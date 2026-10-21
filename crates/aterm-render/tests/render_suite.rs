// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-render's integration suite: one test binary for every file listed
//! below. Each file keeps its path and its `#[test]`s, named `<file>::<test>`;
//! a file gated on a platform or feature keeps its inner `#![cfg(..)]`, which
//! gates the module.
//!
//! No file below sets or removes an environment variable, installs a hook or
//! an allocator, or reads the process's resident-set size or a process-global
//! counter a sibling's renderer would move, except from its own process: the
//! two that read those (`seal_maps_discovered_faces`,
//! `sealed_never_consults_coverage_index`) re-run this binary on their one test
//! (`common/alone.rs`) and read them there. The allocation-counting files
//! install a `#[global_allocator]` and are their own `[[test]]`s in
//! `Cargo.toml`, each with its reason.
//!
//! `autotests = false`: a `tests/*.rs` file that is neither a `mod` line here
//! nor a `[[test]]` of its own never compiles. Run one file's tests with
//! `targo --unverified test -p aterm-render --test render_suite -- <file>::`;
//! an ignored perf gate with `-- <file>:: --ignored --nocapture` in `--release`,
//! never a bare `--ignored`.

#[path = "common/alone.rs"]
mod alone;

mod anchor_reuse;
mod asymmetric_pad_layout;
mod band_seam_clearance;
mod bg_base_elision;
mod bg_fill_count;
mod cat_sprites;
mod combining_stack;
mod contrast_floor;
mod ct_fractional_bearing;
mod cursor_ink;
mod cursor_shapes;
mod damage_differential;
mod deco_lines;
mod display_faces;
mod emoji_resample;
mod empty_channels;
mod fallback_coverage_is_truthful;
mod fallback_harmony;
mod fallback_symbol_placement;
mod fire_halo;
mod font_chain_conservation;
mod font_hinting_config;
mod font_oracle;
mod font_subpixel_config;
mod free_composite;
mod free_overlay;
mod fuzz_render_never_panics;
mod fx_layers;
mod glow_halo;
mod glow_under;
mod incoming_row_apron;
mod ink;
mod inline_image;
mod leading_law;
mod ligature_slice;
mod ligatures;
mod missing_font_classes;
mod nova;
mod overline_color;
mod pad_absorption;
mod presentation_gate;
mod procedural_aa_edges;
mod procedural_orthogonal_regression;
mod procedural_seams;
mod rain_render;
mod raster_accuracy_survey;
mod row_overshoot;
mod row_revision_oracle;
mod rtl_runs;
mod script_support_matrix;
mod scroll_blit;
mod scroll_flood;
mod scroll_frac_translate;
mod seal_maps_discovered_faces;
mod sealed_keeps_embedded_backstop;
mod sealed_never_consults_coverage_index;
mod selection_highlight;
mod set_px_rerasterizes;
mod shade_phase;
mod styled_faces;
mod synthetic_symbols;
mod text_blending;
mod variation_instantiation;
mod vibrancy_contrast;
mod vibrancy_ink;
mod visual_regression;
mod w5_floors;
mod weight_invariance;
mod windows_grid_fit;
mod word_decorations;
