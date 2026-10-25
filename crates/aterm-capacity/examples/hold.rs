// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stand-in caller, dev-only: admits one need in this user's ledger on the live machine, as a
//! tool will, holds the claim for `--for` seconds, and ends, which lets it go.
//!
//! ```text
//! targo --unverified run -p aterm-capacity --example hold -- \
//!     --what "demo" [--mem-gib N] [--cores N] [--disk-gib N] [--quiet LOAD] [--admit now] [--for S]
//! ```

use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let mut need = aterm_capacity::Need::new("hold");
    let mut mode = aterm_capacity::Admit::Wait;
    let mut hold = Duration::from_secs(60);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let Some(value) = it.next() else {
            eprintln!("hold: {flag} takes a value");
            return ExitCode::from(2);
        };
        let num = || value.parse::<u64>().ok();
        let ok = match flag.as_str() {
            "--what" => {
                need.what.clone_from(value);
                true
            }
            "--mem-gib" => num().map(|g| need.mem = g << 30).is_some(),
            "--cores" => num()
                .and_then(|c| u32::try_from(c).ok())
                .map(|c| need.cores = c)
                .is_some(),
            "--disk-gib" => num()
                .zip(std::env::current_dir().ok())
                .map(|(g, cwd)| need = need.clone().disk(g << 30, cwd))
                .is_some(),
            "--quiet" => value
                .parse::<f64>()
                .ok()
                .map(|q| need.quiet = Some(q))
                .is_some(),
            "--admit" => aterm_capacity::Admit::parse(value)
                .map(|m| mode = m)
                .is_some(),
            "--for" => num().map(|s| hold = Duration::from_secs(s)).is_some(),
            _ => false,
        };
        if !ok {
            eprintln!("hold: cannot read {flag} {value}");
            return ExitCode::from(2);
        }
    }
    match aterm_capacity::admit(need, mode) {
        Ok(claim) => {
            println!(
                "hold: admitted pid {} cores {} after {} ({})",
                std::process::id(),
                claim.cores(),
                aterm_capacity::span(claim.waited()),
                claim.path().display()
            );
            std::thread::sleep(hold);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("hold: {e}");
            ExitCode::from(1)
        }
    }
}
