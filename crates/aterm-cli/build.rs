// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sets `win_scripts` when the checkout carries the Windows install scripts
//! (`apps/aterm-win/`); the public source snapshot exports no `apps/`.
//! `WINDOWS_UPDATE_LANE` names the scripts only where they exist. And
//! `cfg(anchor_rules)` for the setup ledger's anchor attributes
//! (crates/aterm-spec-anchor/anchor_rules.rs).

use std::path::PathBuf;

#[path = "../aterm-spec-anchor/anchor_rules.rs"]
mod anchor_rules;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(win_scripts)");
    println!("cargo:rerun-if-changed=build.rs");
    anchor_rules::emit();
    let Some(dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        return;
    };
    let script = PathBuf::from(dir).join("../../apps/aterm-win/install.ps1");
    if script.is_file() {
        // Watched only while present: cargo re-runs a build script on every
        // build while a path it watches is missing. So a script that appears
        // later is seen only once build.rs changes or the crate is cleaned.
        println!("cargo:rerun-if-changed={}", script.display());
        println!("cargo:rustc-cfg=win_scripts");
    }
}
