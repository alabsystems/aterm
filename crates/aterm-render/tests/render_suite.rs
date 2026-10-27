// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-render` integration test suite binary.

#[path = "common/alone.rs"]
mod alone;
#[path = "common/sprites.rs"]
mod sprites;
#[path = "common/wcag.rs"]
mod wcag;

mod anchor_reuse;
mod asymmetric_pad_layout;
mod band_seam_clearance;
mod bg_base_elision;
mod bg_fill_count;
mod burst_companion_reads;
mod combining_stack;
mod companion_knock_out;
mod companion_off_letters;
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
mod sprite_layer_laws;
mod sprite_oracle;
mod styled_faces;
mod synthetic_symbols;
mod text_blending;
mod trail_letters_read;
mod variation_instantiation;
mod vibrancy_contrast;
mod vibrancy_ink;
mod visual_regression;
mod w5_floors;
mod weight_invariance;
mod windows_grid_fit;
mod word_decorations;
