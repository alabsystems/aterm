// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Diagnose Core Text font creation before versus after AppKit registration.
//! Run each order in a fresh process: `font_bridge_order early` or `late`.
//! This catches and reports the exception instead of opening a window or aborting.

#[cfg(target_os = "macos")]
#[path = "../tests/support/font_bridge.rs"]
mod probe;

#[cfg(target_os = "macos")]
fn main() {
    let early = match std::env::args().nth(1).as_deref() {
        Some("early") => true,
        Some("late") => false,
        _ => {
            eprintln!("usage: font_bridge_order early|late");
            std::process::exit(2);
        }
    };
    let observed = probe::inspect(early);
    println!("early={early} created={}", observed.created_class);
    println!("is_nsfont={}", observed.is_nsfont);
    println!("result={:?}", observed.system_class);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("font_bridge_order requires macOS");
    std::process::exit(2);
}
