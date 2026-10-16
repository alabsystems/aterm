// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gpu's integration suite: one test binary for every file listed below;
//! a test is named `<file>::<test>`. The shared kits mount once here — `common`
//! (the backend pair and pixel comparisons) and `rain_common` (the rain scene) —
//! and each file reaches them as `crate::<kit>`.
//!
//! The files share a process, so none writes the process environment (the
//! ligature files pass their font as a family), installs a hook or an
//! allocator, or re-executes itself. Files that force the fontdue rasterizer
//! through the environment belong to `gpu_fontdue`.
//!
//! `autotests = false`: a `tests/*.rs` file that is neither a `mod` here, a
//! `mod` of `gpu_fontdue.rs`, nor a `[[test]]` of its own never compiles. Run
//! one file's tests with `targo --unverified test -p aterm-gpu --test gpu_suite -- <file>::`.

mod common;
#[allow(dead_code)]
mod rain_common;

mod atlas_follows_the_cpu_rasters;
mod blit_bands;
mod blit_invert;
mod capture_tap_conformance;
mod cat_parity;
mod conformance_gpuencode;
mod cursor_shapes_gpu;
mod dirty_gate;
mod effect_pipeline_demand;
mod feature_flip_parity;
mod fire_halo_gpu;
mod fire_patch_parity;
mod free_parity;
mod free_sprite_gpu;
mod glow_halo_parity;
mod glow_under_parity;
mod gpu_matches_cpu;
mod gpu_memory_floor;
mod hdr_gate;
mod heat_shimmer;
mod inline_image_parity;
mod ligature_parity;
mod merged_ligature_parity;
mod oversized_resize;
mod parity_fuzz;
mod parity_fuzz_adversarial;
mod pipeline_table_guard;
mod present_real;
mod rain_parity;
mod scissor_repaint;
mod scroll_blit_gpu;
mod selection_highlight;
mod snapshot_is_final;
mod tray_present_scissor;
