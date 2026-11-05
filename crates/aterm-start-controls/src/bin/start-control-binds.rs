// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A `verify perf cli-start` control: a program that links what the `all` control links (build.rs
//! passes it the same flags, and on Apple silicon chained fixups, as aterm is linked) and holds the
//! address of every symbol the lane names (`ATERM_START_CONTROL_IMPORTS`, the imports of the aterm
//! it measures), each one a bind dyld resolves at load. Over `all` it is what resolving aterm's
//! imports costs a start.

include!(concat!(env!("OUT_DIR"), "/imports.rs"));

fn main() {
    std::hint::black_box(&IMPORTS);
}
