// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit macOS mounted-volume acceptance of the updater's no-zip lane and install.sh's
//! image path. Run with `--profile test`; `targo --unverified lab mac dmg` runs it in a
//! disposable macOS guest. It carries the install.sh it was built with, so it runs where
//! no checkout is.

#[path = "../../aterm-conformance/benches/support/mod.rs"]
mod support;

fn main() -> std::process::ExitCode {
    #[cfg(all(debug_assertions, target_os = "macos"))]
    {
        use aterm_update::dmg_acceptance as acceptance;
        type Row = fn(&std::path::Path) -> std::io::Result<()>;
        const INSTALL_SH: &str = include_str!("../../../tools/install.sh");
        let rows: [(&str, Row); 4] = [
            (
                "no-zip-stage",
                acceptance::a_real_image_without_a_zip_stages_its_app,
            ),
            (
                "no-zip-refusal",
                acceptance::a_real_image_without_aterm_app_is_refused_and_detached,
            ),
            ("install-sh-image", |dir| {
                acceptance::install_sh_installs_from_a_real_image(dir, INSTALL_SH)
            }),
            ("install-sh-no-app", |dir| {
                acceptance::install_sh_refuses_an_image_with_no_app_and_detaches(dir, INSTALL_SH)
            }),
        ];
        support::run("aterm-update-dmg", &rows, |take| {
            let result = aterm_tempfile::tempdir().and_then(|scratch| take(scratch.path()));
            match result {
                Ok(()) => support::Outcome::Pass("real mounted-volume controls completed".into()),
                Err(why) => support::Outcome::CouldNotRun(why.to_string()),
            }
        })
    }
    #[cfg(not(all(debug_assertions, target_os = "macos")))]
    support::run(
        "aterm-update-dmg",
        &[
            ("no-zip-stage", ()),
            ("no-zip-refusal", ()),
            ("install-sh-image", ()),
            ("install-sh-no-app", ()),
        ],
        |_| support::Outcome::CouldNotRun("requires macOS and --profile test".into()),
    )
}
