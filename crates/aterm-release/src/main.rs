// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-release`: the release cutter behind the `targo --unverified ship` alias.
//!
//! The cutter is Unix's (a cut runs on the release Mac), so each of its modules is
//! `cfg(unix)` and on Windows the bin only says so.

#[cfg(unix)]
mod accepted_measure;
#[cfg(unix)]
mod allowance;
#[cfg(unix)]
mod apple;

#[cfg(unix)]
mod artifact;
#[cfg(unix)]
mod build_inputs;
#[cfg(unix)]
mod buildplan;
#[cfg(unix)]
mod bump_claim;
#[cfg(unix)]
mod bundle;
#[cfg(unix)]
mod changelog;
#[cfg(unix)]
mod check_content;
#[cfg(unix)]
mod check_record;
#[cfg(unix)]
mod checkout_bin;
#[cfg(unix)]
mod input_record;
#[cfg(unix)]
mod postlink;

#[cfg(unix)]
mod channel;

#[cfg(unix)]
mod clean;
#[cfg(unix)]
mod cli;

#[cfg(unix)]
mod credential_wait;

#[cfg(unix)]
mod cut_timing;
#[cfg(unix)]
mod cutter;

#[cfg(unix)]
mod dev_app;

#[cfg(unix)]
mod dev_identity;
#[cfg(unix)]
mod dmg;

#[cfg(unix)]
mod durable;

#[cfg(unix)]
mod fixtures;
#[cfg(unix)]
mod gates;

#[cfg(unix)]
mod harness_versions;

#[cfg(unix)]
mod github;
#[cfg(unix)]
mod github_remote;

#[cfg(unix)]
mod hook;
#[cfg(unix)]
mod host_fit;

#[cfg(unix)]
mod interrupt;
#[cfg(unix)]
mod ledger;
#[cfg(unix)]
mod line;
#[cfg(unix)]
mod line_claim;

#[cfg(unix)]
mod machines;
#[cfg(unix)]
mod manifest_out;
#[cfg(unix)]
mod service_qualification;

#[cfg(unix)]
mod profile_pilot;
#[cfg(unix)]
mod provision;
#[cfg(unix)]
mod publish;
#[cfg(unix)]
mod qualify_pilot;
#[cfg(unix)]
mod qualify_replay;

#[cfg(unix)]
mod cutter_inputs;
#[cfg(unix)]
mod release;
#[cfg(unix)]
mod release_disk;
#[cfg(unix)]
mod release_keychain;
#[cfg(unix)]
mod release_leftovers;
#[cfg(unix)]
mod release_pump;
#[cfg(unix)]
mod release_status;
#[cfg(unix)]
mod release_steps;
#[cfg(unix)]
mod release_target;
#[cfg(unix)]
mod release_timeline;
#[cfg(unix)]
mod release_tools;

#[cfg(unix)]
mod managed_prerequisites;
#[cfg(unix)]
mod sign;
#[cfg(unix)]
mod source_signing;
#[cfg(unix)]
mod stage_local;

#[cfg(unix)]
mod site;
#[cfg(unix)]
mod slice_units;

#[cfg(unix)]
mod tap;

#[cfg(unix)]
mod snapshot;

#[cfg(unix)]
mod transforms;

#[cfg(unix)]
mod trust_credit;
#[cfg(unix)]
mod trust_facts;

#[cfg(unix)]
mod trust_lane;

/// The candidate's `trust.lock`: the stock release its STOCK EXCEPTION slices build with, and the
/// prover it names (A14's release seam).
#[cfg(unix)]
mod trust_lock;

#[cfg(unix)]
mod trust_oblig;
#[cfg(unix)]
mod verify;
#[cfg(unix)]
mod windows_pin;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    // SAFETY: the first operation in this fresh image, before any helper, proof or reader.
    if let Err(why) = unsafe { release_steps::capture_exec_handoff_environment() } {
        eprintln!("release startup handover: {why}");
        return std::process::ExitCode::from(2);
    }
    if let Err(error) = agh::git::HelperHost::Subcommand.record() {
        eprintln!("{error}");
        return std::process::ExitCode::FAILURE;
    }
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    cut_timing::image_arrived();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // SAFETY: this fresh CLI has not started any other thread.
    if let Some(code) = unsafe { aterm_verify::remote_cli::entry(&args) } {
        return code;
    }
    // Cargo's compiler wrapper for a release slice runs nothing else of this tool.
    if let Some(code) = slice_units::entry(&args) {
        return std::process::ExitCode::from(code);
    }
    #[cfg(debug_assertions)]
    slice_units::linker_probe_if_asked();
    if let Some(code) = qualify_pilot::slice_child_entry(&args) {
        return code;
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
    #[cfg(not(test))]
    if let Some(result) = release_steps::step_one_probe_if_asked(&args) {
        return match result {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(why) => {
                eprintln!("step 1 source instrument: {why}");
                std::process::ExitCode::from(2)
            }
        };
    }
    let remote_host = match aterm_verify::remote_host::pin() {
        Ok(host) => host,
        Err(why) => {
            eprintln!("aterm-release: cannot capture remote transport host: {why}");
            return std::process::ExitCode::from(2);
        }
    };
    let code = cli::run();
    drop(remote_host);
    std::process::exit(code);
}

/// Off Unix there is no cutter: a cut runs on the release Mac, and a Windows host hands its
/// build to one (`aterm-winship release-build`, read here by `--windows-artifacts`).
#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "aterm-release: the release cutter runs on the release Mac (and its Unix workers), never \
         on Windows: a Windows host hands its build to a cut with `aterm-winship release-build`"
    );
    std::process::ExitCode::from(2)
}

#[cfg(all(test, unix))]
#[path = "test_vault_tests.rs"]
mod test_vault;

/// Create a short `.noindex` temporary directory for unit tests in this crate.
#[cfg(all(test, unix))]
pub(crate) fn test_scratch_dir(tag: &str) -> aterm_tempfile::TempDir {
    let short = &tag[..tag.floor_char_boundary(8)];
    aterm_tempfile::Builder::new()
        .prefix(&format!("arl-{short}-"))
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("test scratch dir")
}
