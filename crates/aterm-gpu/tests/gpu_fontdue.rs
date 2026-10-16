// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-gpu's additive-parity files, in a test binary apart from `gpu_suite`: they build their
//! renderers through `common::backends_fontdue`, which sets `ATERM_RASTERIZER=fontdue` for the
//! whole process (byte-exact GPU == CPU additive compositing is defined on the deterministic
//! rasterizer); inside `gpu_suite` that write would silently switch the rasterizer of every
//! renderer built after it. `empty_channels_gpu` builds pairs through both constructors.
//!
//! `autotests = false`: see `gpu_suite.rs`. Run one file's tests with
//! `targo --unverified test -p aterm-gpu --test gpu_fontdue -- <file>::`.

mod common;
#[allow(dead_code)]
mod rain_common;

mod empty_channels_gpu;
mod glow_parity;
mod nova_parity;
