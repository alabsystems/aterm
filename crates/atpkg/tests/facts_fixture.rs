// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A development-only executable with nominal fixture facts and actual binary assertions/arch.

use std::io::Write as _;
use std::process::ExitCode;

// The compiler identity and profile label are deliberately fixture data. Only this binary's
// assertion and architecture facts come from its own compile; this makes no toolchain claim.
#[used]
#[cfg_attr(target_vendor = "apple", unsafe(link_section = "__TEXT,__aterm_facts"))]
static FACTS: [u8; aterm_stamp::facts::LEN] = aterm_stamp::facts::encode(aterm_stamp::Stamp {
    build_profile: "fixture",
    debug_assertions: Some(cfg!(debug_assertions)),
    target_arch: std::env::consts::ARCH,
    compiler_commit: "0123456789012345678901234567890123456789",
    compiler_host: "fixture-host",
    compiler_flavor: "r",
    trust_verify: "off",
    ..aterm_stamp::Stamp::UNSTAMPED
});

pub(super) fn entry() -> Option<ExitCode> {
    let args: Vec<_> = std::env::args_os().collect();
    if args
        .get(1)
        .is_some_and(|arg| arg == aterm_stamp::facts::ARG)
    {
        return Some(
            if args.len() == 2 && std::io::stdout().write_all(&FACTS).is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            },
        );
    }
    if args.len() == 2 && args[1] == "__snapshot_identity" {
        return Some(match std::env::current_exe() {
            Ok(path) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Err(_) => ExitCode::from(1),
        });
    }
    None
}
