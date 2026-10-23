// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A build script's step that has the package's executables run as their invoker on Windows.
//!
//! Windows' installer detection asks elevation of an unmanifested executable whose name says
//! `update`, `install`, `setup` or `patch` (os error 740, "The requested operation requires
//! elevation"), and a test binary takes its crate's or its file's name
//! (`aterm_update_core-<hash>.exe`, `install_sh-<hash>.exe`). A manifest that requests
//! `asInvoker` opts the binary out. A build script mounts this file by path
//! (`#[path = "../aterm-winres/as_invoker.rs"] mod as_invoker;`) and calls [`emit`]; an rlib
//! links nothing, so the link arguments reach only the package's own executables.

/// Embed an `asInvoker` manifest in every executable this package links, on an MSVC target.
pub fn emit() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        println!("cargo::rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo::rustc-link-arg=/MANIFESTUAC:level='asInvoker'");
    }
}
