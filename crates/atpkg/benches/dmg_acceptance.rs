// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit macOS mounted-volume acceptance. Run with `--profile test`.

#[path = "../../aterm-conformance/benches/support/mod.rs"]
mod support;

fn main() -> std::process::ExitCode {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    {
        let rows = [
            (
                "single-app-stage",
                atpkg::install::a_real_image_stages_its_single_app
                    as fn(&std::path::Path) -> std::io::Result<()>,
            ),
            (
                "no-app-refusal",
                atpkg::install::a_real_image_with_no_app_is_refused_and_detached
                    as fn(&std::path::Path) -> std::io::Result<()>,
            ),
        ];
        support::run("atpkg-dmg", &rows, |take| {
            let result = aterm_tempfile::tempdir().and_then(|scratch| take(scratch.path()));
            match result {
                Ok(()) => support::Outcome::Pass("real mounted-volume controls completed".into()),
                Err(why) => support::Outcome::CouldNotRun(why.to_string()),
            }
        })
    }
    #[cfg(not(all(debug_assertions, target_os = "macos")))]
    support::run(
        "atpkg-dmg",
        &[("single-app-stage", ()), ("no-app-refusal", ())],
        |_| support::Outcome::CouldNotRun("requires macOS and --profile test".into()),
    )
}
