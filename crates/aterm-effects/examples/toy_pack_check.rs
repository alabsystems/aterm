// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Validate and summarize one strict aterm Toy Pack manifest.

use std::path::PathBuf;
use std::process::ExitCode;

use aterm_effects::spec::{compile_toy_pack_toml, read_toy_pack_file};

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: cargo run -p aterm-effects --example toy_pack_check -- <pack.toml>");
        return ExitCode::from(2);
    };
    let source = match read_toy_pack_file(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("toy_pack_check: read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    match compile_toy_pack_toml(&source) {
        Ok(pack) => {
            println!(
                "OK {} v{}: {} recipes, {} word surfaces ({})",
                pack.metadata().id,
                pack.metadata().version,
                pack.toy_count(),
                pack.word_count(),
                pack.metadata().name
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("toy_pack_check: {} is invalid:\n{error}", path.display());
            ExitCode::FAILURE
        }
    }
}
