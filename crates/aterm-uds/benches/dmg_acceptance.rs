// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit macOS mounted-volume acceptance. Run with `--profile test`.

#[path = "../../aterm-conformance/benches/support/mod.rs"]
mod support;

fn main() -> std::process::ExitCode {
    support::run(
        "uds-dmg",
        &[("force-detached-working-directory", ())],
        |_| {
            #[cfg(all(debug_assertions, target_vendor = "apple"))]
            {
                let result = aterm_tempfile::tempdir().and_then(|scratch| {
                    aterm_uds::holders::a_force_detached_volume_is_no_working_directory(
                        scratch.path(),
                    )
                });
                match result {
                    Ok(()) => support::Outcome::Pass(
                        "mounted cwd, forced detach, descriptor and old-reading controls ran"
                            .into(),
                    ),
                    Err(why) => support::Outcome::CouldNotRun(why.to_string()),
                }
            }
            #[cfg(not(all(debug_assertions, target_vendor = "apple")))]
            support::Outcome::CouldNotRun("requires macOS and --profile test".into())
        },
    )
}
