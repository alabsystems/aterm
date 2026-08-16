// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `gen_robi_glyphs` — regenerate the checked-in Robi robot-pose const drawlists.
//!
//! Reads every pose asset TOML under `crates/aterm-effects/art/robi/` and writes
//! `crates/aterm-effects/src/robi_glyphs_gen.rs`. Run it after touching any pose;
//! the `robi_glyphs_gen_matches_assets` drift test fails until the checked-in
//! file matches. The cat twin of this generator is `gen_cat_glyphs`, the pet twin
//! `gen_pet_glyphs` — same parser, separate rosters (the robot poses must never
//! land in the collectible `GLYPHS` table).
//!
//! ```text
//! cargo run -p aterm-effects --example gen_robi_glyphs
//! ```

use std::path::Path;
use std::process::ExitCode;

use aterm_effects::cat_glyphs_codegen::generate_robi_from_dir;

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let robi_dir = root.join("art/robi");
    let out = root.join("src/robi_glyphs_gen.rs");

    let source = match generate_robi_from_dir(&robi_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gen_robi_glyphs: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&out, &source) {
        eprintln!("gen_robi_glyphs: write {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    println!(
        "gen_robi_glyphs: wrote {} ({} bytes)",
        out.display(),
        source.len()
    );
    ExitCode::SUCCESS
}
