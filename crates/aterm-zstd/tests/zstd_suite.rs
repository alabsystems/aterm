// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unified integration test suite for `aterm-zstd`.

mod support;

mod cli_decode;
mod cli_frames;
mod decode_frames;
mod encode_cli;
mod encode_frames;
mod sweep;
mod xxh64_recorded;
