// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Original-image main for the real control/native host fixture.

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv = std::env::args_os().skip(1).collect::<Vec<_>>();
    // This must precede every fixture role and option. The production retained
    // command names this same executable as `harness native-launch ...`, and
    // System::own_launcher compares the actual current executable's identity.
    if argv.first().is_some_and(|word| word == "harness") {
        #[cfg(all(unix, debug_assertions))]
        if let Some(exit) =
            aterm_agent::harness::native_launcher_fixture_entry(&argv[1..], &|stage, phase| {
                use aterm_agent::harness::{
                    NativeLauncherFixtureStage as Stage, native_act::ImagePhase,
                };
                let home = std::env::var_os("HOME")
                    .ok_or_else(|| std::io::Error::other("fixture HOME"))?;
                let root = std::path::PathBuf::from(home).join(".native-renewal-fixture");
                if !root.is_dir() {
                    return Err(std::io::Error::other("fixture checkpoints missing"));
                }
                let phase = match phase {
                    ImagePhase::Forward => "forward",
                    ImagePhase::Rollback => "rollback",
                };
                let stage = match stage {
                    Stage::BeforeAttach => "before-attach",
                    Stage::AfterAttach => "after-attach",
                    Stage::AfterGo => "after-go",
                };
                let name = format!("{phase}-{stage}");
                aterm_uds::atomic_file::write(
                    &root.join(format!("{name}.reached")),
                    std::process::id().to_string().as_bytes(),
                    0o600,
                )?;
                let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while root.join(format!("{name}.pause")).exists()
                    && !root.join(format!("{name}.continue")).exists()
                {
                    if std::time::Instant::now() >= until {
                        return Err(std::io::Error::other("fixture checkpoint expired"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(())
            })
        {
            return exit;
        }
        return aterm_agent::harness::cli::main_entry(argv.into_iter().skip(1).collect());
    }
    #[cfg(all(unix, debug_assertions))]
    if argv.len() == 2 && argv[0] == "--renewal-config" {
        let result = std::fs::read_to_string(&argv[1])
            .map_err(|error| error.to_string())
            .and_then(|text| {
                aterm_json::from_str::<aterm_gui::NativeRenewalFixture>(&text)
                    .map_err(|error| error.to_string())
            })
            .and_then(aterm_gui::run_native_renewal_host_fixture);
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("native_host: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if !argv.is_empty() {
        eprintln!("native_host: expected no arguments or --renewal-config <owned.json>");
        return ExitCode::from(2);
    }
    #[cfg(all(unix, debug_assertions))]
    {
        match aterm_gui::run_native_host_fixture() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("native_host: FAIL: {error}");
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(all(unix, debug_assertions)))]
    {
        eprintln!("native_host: NOT RUN: requires a Unix debug build");
        ExitCode::from(2)
    }
}
