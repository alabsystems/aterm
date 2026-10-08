// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `win_scripts`: this checkout carries the Windows install scripts
//! (`apps/aterm-win/build.ps1` and `install.ps1`). The development checkout
//! does; the public source snapshot does not (publish/manifest.txt exports no
//! `apps/`). `WINDOWS_UPDATE_LANE` and `aterm help update` name those scripts
//! only where they exist, and otherwise a `targo` rebuild plus the README's
//! fact that Trust for Windows is not published yet.

use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(win_scripts)");
    println!("cargo:rerun-if-changed=build.rs");
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
