// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Historical input discovery without moving refs or changing the checkout's files.

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: release_input_audit BASE TIP".into());
    }
    let start = std::time::Instant::now();
    let input = aterm_verify::release_input::discover(
        &std::env::current_dir().map_err(|e| e.to_string())?,
        &std::env::var_os("PATH").unwrap_or_default(),
        &args[0],
        &args[1],
    )?;
    println!(
        "commits={} paths={} discovery_s={:.3}",
        input.commits.len(),
        input.paths.len(),
        start.elapsed().as_secs_f64()
    );
    for commit in input.commits {
        println!("commit {commit}");
    }
    for path in input.paths {
        println!("path {path}");
    }
    Ok(())
}
