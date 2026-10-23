// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Regenerates `crates/aterm-effects/src/cat_glyphs_gen.rs` from `art/glyphs/` TOML assets.

use std::path::Path;
use std::process::ExitCode;

#[path = "../src/cat_glyphs_codegen.rs"]
pub mod cat_glyphs_codegen;

use cat_glyphs_codegen::generate_from_assets;

/// The shared asset-dir reader: the engine reads no file, the generator does.
#[path = "support/asset_dir.rs"]
mod asset_dir;

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let glyph_dir = root.join("art/glyphs");
    let out = root.join("src/cat_glyphs_gen.rs");

    let assets = match asset_dir::read_toml_dir(&glyph_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gen_cat_glyphs: {e}");
            return ExitCode::FAILURE;
        }
    };
    let source = match generate_from_assets(&assets) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gen_cat_glyphs: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&out, &source) {
        eprintln!("gen_cat_glyphs: write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    println!(
        "gen_cat_glyphs: wrote {} ({} bytes)",
        out.display(),
        source.len()
    );
    ExitCode::SUCCESS
}
