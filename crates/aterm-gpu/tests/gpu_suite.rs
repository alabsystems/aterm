// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration test suite harness for `aterm-gpu`.

mod common;
#[allow(dead_code)]
mod rain_common;

mod capture_tap_conformance;
mod card_parity;
mod cat_parity;
mod companion_knock_gpu;
mod conformance_gpuencode;
mod conformance_tray_card_ring;
mod cursor_shapes_gpu;
mod dirty_gate;
mod feature_flip_parity;
mod fetch_route;
mod fire_halo_gpu;
mod fire_patch_parity;
mod free_parity;
mod free_sprite_gpu;
mod fx_parity;
mod glow_halo_parity;
mod glow_ramps_parity;
mod glow_under_parity;
mod gpu_matches_cpu;
mod hdr_gate;
mod heat_shimmer;
mod inline_image_parity;
mod ligature_parity;
mod merged_ligature_parity;
mod oversized_resize;
mod parity_fuzz;
mod parity_fuzz_adversarial;
mod rain_parity;
mod row_overshoot_gpu;
mod scissor_repaint;
mod scroll_blit_gpu;
mod selection_highlight;
mod snapshot_is_final;
mod tray_present_scissor;
