// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm_sysprobe::gpu_if_read` on Windows opens nothing: in a process that has not read the
//! GPU it answers nothing, so a capacity reading taken by a one-shot verb never pays to open
//! PDH's query; once a read has opened it, it answers as `gpu` does. Its own test binary, with
//! one test, so no other test has read the GPU in this process first.
//!
//! STOCK EXCEPTION: Windows only, and Trust has no Windows std, so only stock Rust builds this (the
//! Windows lane's guest pins its version: `aterm-lab/src/windows_guest.rs`).

// STOCK EXCEPTION: Windows 64-bit has no Trust build; this target runs with stock Rust.
#![cfg(all(windows, target_pointer_width = "64"))]

#[test]
fn a_reading_carries_the_gpu_only_once_this_process_reads_it() {
    assert_eq!(
        aterm_sysprobe::gpu_if_read(),
        None,
        "nothing read the GPU yet, so nothing is opened for it"
    );
    // NEGATIVE CONTROL: once the GPU is read, a reading carries it. A machine with no hardware
    // adapter (a VM on the Basic Render Driver) reads none either way.
    if aterm_sysprobe::gpu().is_some() {
        assert!(
            aterm_sysprobe::gpu_if_read().is_some(),
            "the reader is open and read a moment ago"
        );
    }
}
