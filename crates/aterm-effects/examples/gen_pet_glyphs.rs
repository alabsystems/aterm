// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `gen_pet_glyphs` — regenerate the checked-in pet-pose const drawlists.
//!
//! Reads every full-body pose asset TOML under `crates/aterm-effects/art/pet/` and
//! writes `crates/aterm-effects/src/pet_glyphs_gen.rs`. Run it after touching any pose;
//! the `pet_glyphs_gen_matches_assets` drift test fails until the checked-in file
//! matches. The cat twin of this generator is `gen_cat_glyphs` — same parser, separate
//! roster (the pet must never land in the collectible `GLYPHS` table).
//!
//! ```text
//! cargo run -p aterm-effects --example gen_pet_glyphs
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_effects::cat_glyphs_codegen::generate_pet_from_dir;

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pet_dir = root.join("art/pet");
    let out = root.join("src/pet_glyphs_gen.rs");

    let source = match generate_pet_from_dir(&pet_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gen_pet_glyphs: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&out, &source) {
        eprintln!("gen_pet_glyphs: write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    println!(
        "gen_pet_glyphs: wrote {} ({} bytes)",
        out.display(),
        source.len()
    );
    ExitCode::SUCCESS
}
