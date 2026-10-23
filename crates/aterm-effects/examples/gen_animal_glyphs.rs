// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Regenerate `src/animal_glyphs_gen.rs` from `art/animal/*.toml`
//! (`targo --unverified run -p aterm-effects --example gen_animal_glyphs`).

use std::path::Path;
use std::process::ExitCode;

#[path = "../src/cat_glyphs_codegen.rs"]
pub mod cat_glyphs_codegen;

use cat_glyphs_codegen::generate_animal_from_assets;

/// Shared asset-dir reader (the engine reads no files; the generator does).
#[path = "support/asset_dir.rs"]
mod asset_dir;

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let animal_dir = root.join("art/animal");
    let out = root.join("src/animal_glyphs_gen.rs");

    let assets = match asset_dir::read_toml_dir(&animal_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gen_animal_glyphs: {e}");
            return ExitCode::FAILURE;
        }
    };
    let source = match generate_animal_from_assets(&assets) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gen_animal_glyphs: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&out, &source) {
        eprintln!("gen_animal_glyphs: write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    println!(
        "gen_animal_glyphs: wrote {} ({} bytes)",
        out.display(),
        source.len()
    );
    ExitCode::SUCCESS
}
