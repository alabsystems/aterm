// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! wayland-scan: the dev-only generator of aterm-wayland's protocol modules.
//!
//! It reads the protocol descriptions under `crates/aterm-wayland/protocols/`
//! and writes `src/protocols/<protocol>.rs` for each, plus `mod.rs` and
//! `stubs.rs` (an empty table for an interface a protocol names but no file
//! here defines, such as cursor-shape's `zwp_tablet_tool_v2`). Each module
//! holds the C tables libwayland reads (`crate::iface`), the opcode and enum
//! constants, and one `unsafe fn` per request over `crate::proxy`.
//!
//! The tables are held to wayland-scanner 0.31.10's by
//! `tests/recorded/wayland-scanner-0.31.10-interfaces.tsv`; this program is
//! in no workspace graph (see Cargo.toml). From `crates/aterm-wayland/scan`:
//!
//!   targo --unverified run --release
//!
//! The output is deterministic and the tables sit in a `#[rustfmt::skip]`
//! module, so `tests/generated.rs` holds the committed files to a fresh run.

mod generate;
mod xml;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let root = root.as_path();
    let files = match generate::generate(root) {
        Ok(files) => files,
        Err(e) => {
            eprintln!("wayland-scan: {e}");
            std::process::exit(1);
        }
    };
    for (rel, text) in files {
        let path = root.join(&rel);
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("wayland-scan: {}: {e}", path.display());
            std::process::exit(1);
        }
        println!("wrote {rel}");
    }
}
