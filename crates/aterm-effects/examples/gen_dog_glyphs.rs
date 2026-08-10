// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `gen_dog_glyphs` — regenerate the checked-in dog-breed const drawlists.
//!
//! Reads every breed head asset TOML under `crates/aterm-effects/art/dogs/` and
//! writes `crates/aterm-effects/src/dog_glyphs_gen.rs`. Run it after touching any
//! breed; the `dog_glyphs_gen_matches_assets` drift test fails until the checked-in
//! file matches. The cat twin of this generator is `gen_cat_glyphs`, the pet twin
//! `gen_pet_glyphs` — same parser, separate rosters (the dogs must never land in
//! the collectible `GLYPHS` table).
//!
//! ```text
//! cargo run -p aterm-effects --example gen_dog_glyphs
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_effects::cat_glyphs_codegen::generate_dog_from_dir;

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dog_dir = root.join("art/dogs");
    let out = root.join("src/dog_glyphs_gen.rs");

    let source = match generate_dog_from_dir(&dog_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gen_dog_glyphs: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&out, &source) {
        eprintln!("gen_dog_glyphs: write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    println!(
        "gen_dog_glyphs: wrote {} ({} bytes)",
        out.display(),
        source.len()
    );
    ExitCode::SUCCESS
}
