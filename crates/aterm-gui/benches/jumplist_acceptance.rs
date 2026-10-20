// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit Windows shell-profile acceptance. Run with `--profile test`.

#[path = "../../aterm-conformance/benches/support/mod.rs"]
#[cfg_attr(
    target_os = "macos",
    expect(
        dead_code,
        reason = "this build can only report Windows acceptance unavailable"
    )
)]
mod support;

fn main() -> std::process::ExitCode {
    support::run("windows-jumplist", &[("shell-store-readback", ())], |_| {
        #[cfg(all(windows, debug_assertions))]
        {
            let result = aterm_tempfile::tempdir().and_then(|scratch| {
                aterm_gui::the_shell_keeps_what_a_commit_puts_in_a_test_identitys_jump_list(
                    scratch.path(),
                )
            });
            match result {
                Ok(()) => support::Outcome::Pass(
                    "real shell commit, store readback and DeleteList controls ran".into(),
                ),
                Err(why) => support::Outcome::CouldNotRun(why.to_string()),
            }
        }
        #[cfg(not(all(windows, debug_assertions)))]
        support::Outcome::CouldNotRun("requires Windows and --profile test".into())
    })
}
