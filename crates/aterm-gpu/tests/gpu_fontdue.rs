// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gpu's ADDITIVE-PARITY files, one test binary apart from `gpu_suite`:
//! they build their renderers through `common::backends_fontdue`, which sets
//! `ATERM_RASTERIZER=fontdue` for the whole process (byte-exact GPU == CPU
//! additive compositing is defined on the deterministic rasterizer), and a
//! renderer any `gpu_suite` file built after that write would silently change
//! rasterizer. `empty_channels_gpu` builds pairs through both constructors.
//!
//! `autotests = false`: see `gpu_suite.rs`. Run one file's tests with
//! `targo --unverified test -p aterm-gpu --test gpu_fontdue -- <file>::`.

mod common;
#[allow(dead_code)]
mod rain_common;

mod empty_channels_gpu;
mod glow_parity;
mod nova_parity;
