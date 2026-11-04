// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `verify perf cli-start` control: an empty program that links every dylib the aterm the lane
//! measures links (build.rs, from aterm's load commands). Over the trivial start it is what
//! loading them costs; aterm over it is aterm's own start.

fn main() {}
