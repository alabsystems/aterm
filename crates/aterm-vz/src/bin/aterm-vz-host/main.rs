// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-vz-host`: the Linux lane's VM helper (`docs/DESIGN-vz-guest-2026-10-23.md` §5,
//! §5.1), dev-only. The lab places a signed copy at `vz.noindex/host/<sha256>/aterm-vz-host`
//! ([`aterm_vz::sign`]) and starts it detached ([`aterm_vz::client::Detached`]).
//!
//! `serve <state>` is its one mode: it holds its placed copy in use, takes the live disk's
//! `flock`, binds `<state>/run/aterm-linux.sock`, writes its record, and serves sessions until
//! its release rule ends it ([`aterm_vz::helper::run`]); Virtualization.framework is opened
//! only when a session asks it to `Boot`. The escalation tests' hung helper is their own test
//! binary in a stand-in mode (`tests/stand_in.rs`, §13), so this entitled binary carries no
//! test mode.

use std::path::Path;
use std::process::ExitCode;

#[cfg(target_os = "macos")]
mod vz;

const USAGE: &str = "usage: aterm-vz-host serve <state-dir>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["serve", state] => serve(Path::new(state)),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// This process's identity, from the executable it runs: its directory's content address
/// when it runs from a placed copy, whose `ran` marker it takes and writes first, and holds
/// for its whole life (the returned guard), so no placing reclaims the copy it runs from.
#[cfg(target_os = "macos")]
fn ident(
    paths: &aterm_vz::record::Paths,
) -> Result<(aterm_vz::helper::Ident, Option<aterm_vz::sign::InUse>), String> {
    let exe = std::env::current_exe().map_err(|e| format!("this executable's path: {e}"))?;
    let in_use = aterm_vz::sign::mark_ran(&exe);
    let sha = aterm_vz::sign::helper_sha256(&exe)?;
    Ok((
        aterm_vz::helper::this_process(sha, paths.live_disk()),
        in_use,
    ))
}

#[cfg(target_os = "macos")]
fn serve(state: &Path) -> ExitCode {
    let paths = aterm_vz::record::Paths::at(state);
    let run = ident(&paths).and_then(|(ident, in_use)| {
        let machine = vz::Vz::new()?;
        let ran = aterm_vz::helper::run(&paths, machine, ident, aterm_vz::helper::HOLD_CONNS);
        drop(in_use);
        ran
    });
    match run {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aterm-vz-host: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn serve(_state: &Path) -> ExitCode {
    eprintln!("aterm-vz-host: the helper runs Virtualization.framework, on macOS only");
    ExitCode::from(1)
}
