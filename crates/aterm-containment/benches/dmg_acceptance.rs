// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit macOS mounted-volume acceptance. Run with `--profile test`.

#[path = "../../aterm-conformance/benches/support/mod.rs"]
mod support;

fn main() -> std::process::ExitCode {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    {
        let rows = [(
            "mounted-bundle-boundary",
            aterm_containment::consent::a_real_volume_mounted_in_the_bundle_is_not_crossed
                as fn(&std::path::Path) -> std::io::Result<()>,
        )];
        support::run("aterm-containment-dmg", &rows, |take| {
            let result = aterm_tempfile::tempdir().and_then(|scratch| take(scratch.path()));
            match result {
                Ok(()) => support::Outcome::Pass("real mounted-volume controls completed".into()),
                Err(why) => support::Outcome::CouldNotRun(why.to_string()),
            }
        })
    }
    #[cfg(not(all(debug_assertions, target_os = "macos")))]
    support::run(
        "aterm-containment-dmg",
        &[("requires-macos-debug", ())],
        |_| support::Outcome::CouldNotRun("requires macOS and --profile test".into()),
    )
}
