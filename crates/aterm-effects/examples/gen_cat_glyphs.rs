// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `gen_cat_glyphs` — regenerate the checked-in cat-glyph const drawlists.
//!
//! Reads every roster asset TOML under `crates/aterm-effects/art/glyphs/` and writes
//! `crates/aterm-effects/src/cat_glyphs_gen.rs` (docs/cat-art-v4-design.md §1). Run it
//! after touching any glyph asset; the `cat_glyphs_gen_matches_assets` drift test fails
//! until the checked-in file matches.
//!
//! ```text
//! cargo run -p aterm-effects --example gen_cat_glyphs
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_effects::cat_glyphs_codegen::generate_from_assets;

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
