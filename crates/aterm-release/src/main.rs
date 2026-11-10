// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-release`: the release cutter behind the `targo --unverified ship` alias.

mod accepted_measure;
mod apple;

mod artifact;
mod build_inputs;
mod buildplan;
mod bump_claim;
mod bundle;
mod changelog;
mod check_content;
mod check_record;
mod checkout_bin;
mod input_record;
mod postlink;

mod channel;

mod clean;
mod cli;

mod credential_wait;

mod cut_timing;
mod cutter;

mod dev_app;

mod dev_identity;
mod dmg;

mod durable;

mod fixtures;
mod gates;

mod harness_versions;

mod github;
mod github_remote;

mod hook;

mod interrupt;
mod ledger;
mod line;
mod line_claim;

mod machines;
mod main_lane;
mod manifest_out;
mod service_qualification;

mod profile_pilot;
mod provision;
mod publish;
mod qualify_pilot;
mod qualify_replay;

mod cutter_inputs;
mod release;
mod release_disk;
mod release_keychain;
mod release_status;
mod release_steps;
mod release_target;
mod release_timeline;
mod release_tools;

mod managed_prerequisites;
mod sign;
mod source_signing;
mod stage_local;

mod site;
mod slice_units;

mod tap;

mod snapshot;

mod transforms;

mod trust_credit;
mod trust_facts;

mod trust_lane;

/// The candidate's `trust.lock` and the prover it names (A14's release seam).
#[expect(
    dead_code,
    reason = "the release chain's prover() and checks() adopt it (release.rs, release_steps.rs)"
)]
mod trust_lock;

mod trust_oblig;
mod verify;

fn main() -> std::process::ExitCode {
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    cut_timing::image_arrived();
    // Cargo's compiler wrapper for a release slice runs nothing else of this tool.
    if let Some(code) = slice_units::entry(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
        return std::process::ExitCode::from(code);
    }
    #[cfg(any(test, debug_assertions))]
    if let Some(result) = interrupt::exec_probe_if_asked() {
        return match result {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(why) => {
                eprintln!("release signal exec probe: {why}");
                std::process::ExitCode::from(2)
            }
        };
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(code) = release_target::finish_if_asked(&args) {
        return code;
    }
    if args.len() == 1 && args[0] == "--version" {
        println!("{}", release_tools::CUTTER_VERSION);
        return std::process::ExitCode::SUCCESS;
    }
    match github_remote::helper_arguments(&args) {
        Ok(Some(args)) => return agh::cli::main_entry(args),
        Ok(None) => {}
        Err(why) => {
            eprintln!("aterm-release: {why}");
            return std::process::ExitCode::from(2);
        }
    }
    // POSIX resets caught dispositions when the frozen cutter replaces the launcher's image.
    // The old image carried HUP/INT/TERM blocked across exec; install handlers before selectively
    // restoring that mask, and do both before content proof or resource adoption.
    if std::env::var_os(interrupt::EXEC_MASK_ENV).is_some()
        && let Err(why) = interrupt::install_exec_handoff()
    {
        eprintln!("release signal handover: {why}");
        return std::process::ExitCode::from(2);
    }
    cutter::judge_at_start();
    #[cfg(all(debug_assertions, not(test)))]
    if let Some(result) = release_steps::cutter_probe_if_asked(&args) {
        return match result {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(why) => {
                eprintln!("cutter child probe: {why}");
                std::process::ExitCode::from(2)
            }
        };
    }
    std::process::exit(cli::run());
}

/// Create a short `.noindex` temporary directory for unit tests in this crate.
#[cfg(test)]
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let short = &tag[..tag.floor_char_boundary(8)];
    let builder = aterm_tempfile::Builder::new()
        .prefix(&format!("arl-{short}-"))
        .suffix(".noindex");
    #[cfg(unix)]
    let made = builder.tempdir_in("/tmp");
    #[cfg(not(unix))]
    let made = builder.tempdir();
    made.expect("test scratch dir")
}
