// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Additive-parity tests isolated from `gpu_suite`: `common::backends_fontdue` sets
//! `ATERM_RASTERIZER=fontdue` process-wide for deterministic GPU == CPU compositing, which would
//! otherwise switch the rasterizer for every subsequent renderer in `gpu_suite`.

mod common;
#[allow(dead_code)]
mod rain_common;

mod empty_channels_gpu;
mod glow_parity;
mod nova_parity;
