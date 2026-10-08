// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-render's integration suite: ONE test binary for every file listed
//! below, instead of one linked and started per file.
//!
//! Each file keeps its path (every `tests/<file>.rs` citation still resolves)
//! and its own `#[test]`s; a test is named `<file>::<test>` here. Files gated
//! on a platform or a feature keep their inner `#![cfg(..)]`, which now gates
//! the module.
//!
//! Sharing a process was checked, not assumed: no file below sets or removes an
//! environment variable, installs a hook or an allocator, re-executes itself,
//! reads the process's resident-set size, or reads a process-global counter a
//! sibling's renderer would move. The files that do are their own `[[test]]`s
//! in `Cargo.toml`, each with its reason.
//!
//! `autotests = false`: a `tests/*.rs` file that is neither a `mod` line here
//! nor a `[[test]]` of its own NEVER COMPILES. Run one file's tests with
//! `targo --unverified test -p aterm-render --test render_suite -- <file>::`;
//! an ignored perf gate with `-- <file>:: --ignored --nocapture` in `--release`,
//! never a bare `--ignored`.

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
mod font_subpixel_config;
mod free_composite;
mod free_overlay;
mod fuzz_render_never_panics;
mod glow_halo;
mod glow_under;
mod glyph_key_injectivity;
mod grid_integrity;
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
mod row_revision_oracle;
mod script_support_matrix;
mod scroll_blit;
mod scroll_flood;
mod scroll_frac_translate;
mod scroll_raster_invariance;
mod sealed_keeps_embedded_backstop;
mod selection_highlight;
mod set_px_rerasterizes;
mod shade_phase;
mod stream_fade_blend;
mod styled_faces;
mod synthetic_symbols;
mod text_blending;
mod variation_instantiation;
mod vibrancy_ink;
mod visual_regression;
mod w5_floors;
mod weight_invariance;
mod windows_grid_fit;
mod word_decorations;
