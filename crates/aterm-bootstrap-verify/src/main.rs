// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-bootstrap-verify`: tools/install.sh's Linux verifier ([`aterm_bootstrap_verify`]).
//! Exit 0 admits (and prints the signed target's line), exit 1 refuses with one line on
//! stderr.

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let now = aterm_bootstrap_verify::unix_now_fail_closed(std::time::SystemTime::now());
    let said = aterm_bootstrap_verify::run(&args, now).and_then(|line| {
        let Some(line) = line else {
            return Ok(());
        };
        let mut out = std::io::stdout().lock();
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(|e| {
                aterm_bootstrap_verify::Refusal::new(format!("cannot print the verdict: {e}"))
            })
    });
    match said {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            // One line, whatever a refusal's words hold: install.sh reads one.
            let _ = writeln!(
                std::io::stderr(),
                "{}{}",
                aterm_bootstrap_verify::REFUSED,
                why.0.replace(['\r', '\n'], " ")
            );
            ExitCode::from(1)
        }
    }
}
