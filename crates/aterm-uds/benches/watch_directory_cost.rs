// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual macOS directory-registration work, with native invalidation checked before measuring.
//! Run with `targo --unverified bench -p aterm-uds --bench watch_directory_cost`.
//! Add `-- --layouts` for shallow/deep and shallow/linked lookup comparisons.

#[cfg(target_os = "macos")]
#[path = "support/watch_directory.rs"]
mod work;

#[cfg(target_os = "macos")]
fn main() -> std::io::Result<()> {
    work::backend!(Native, make, aterm_uds::file_watch);
    if std::env::args().any(|argument| argument == "--layouts") {
        return work::layout_matrix(make);
    }
    // The shipped API against itself establishes the paired workload's own variation.
    work::paired(make, make, "current_a", "current_b")
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("watch_directory_cost requires macOS process instruction counters");
    std::process::exit(2);
}
