// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `gen_animal_glyphs` — regenerate the checked-in species-head const drawlists.
//!
//! Reads every species-head asset TOML under `crates/aterm-effects/art/animal/` and
//! writes `crates/aterm-effects/src/animal_glyphs_gen.rs`. Run it after touching any
//! head; the `animal_glyphs_gen_matches_assets` drift test fails until the checked-in
//! file matches. The cat/pet twins of this generator are `gen_cat_glyphs` /
//! `gen_pet_glyphs` — same parser, separate rosters (a species head must never land
//! in the collectible `GLYPHS` table).
//!
//! ```text
//! cargo run -p aterm-effects --example gen_animal_glyphs
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_effects::cat_glyphs_codegen::generate_animal_from_assets;

/// The shared asset-dir reader: the engine reads no file, the generator does.
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
