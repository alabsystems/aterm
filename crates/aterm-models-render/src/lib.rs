// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Formal models of rendering and resize.

mod models_gpu;
mod models_render;
mod models_resize_render;

pub use models_gpu::{gpu_encode_model, metal_atlas_residency_model, metal_tray_card_ring_model};
pub use models_render::*;
pub use models_resize_render::{repaint_nudge_liveness, repaint_nudge_model, resize_render_model};
