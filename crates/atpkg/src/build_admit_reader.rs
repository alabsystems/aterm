// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The normal build front's retained-reservation reader contract. A response is
//! schema and namespace evidence only: it acquires no resources or claim locks.

/// Data selector on the existing safe `__build_admit --capabilities` entry.
/// Older readers ignore it and answer their unchanged hop capability.
pub const QUERY_ENV: &str = "ATERM_BUILD_ADMIT_READER_QUERY";
/// The precise query grammar; this value grants no resource authority.
pub const QUERY: &str = "retained-reservation-v1";

pub(super) fn selected_answer() -> Option<std::process::ExitCode> {
    let selected = std::env::var_os(QUERY_ENV)?;
    if selected != QUERY {
        eprintln!("build-admit reader: unsupported capability selector");
        return Some(std::process::ExitCode::from(75));
    }
    Some(answer())
}

fn answer() -> std::process::ExitCode {
    match capabilities() {
        Ok(value) => {
            println!("{value}");
            std::process::ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("build-admit reader: {why}");
            std::process::ExitCode::from(75)
        }
    }
}

fn capabilities() -> Result<aterm_json::Value, String> {
    let ledger = super::live_ledger().ok_or("the reader's capacity namespace is unavailable")?;
    capabilities_at(&ledger)
}

#[cfg(target_os = "macos")]
fn normal_capabilities() -> Result<aterm_json::Value, String> {
    let ledger =
        aterm_capacity::Ledger::user().ok_or("the normal capacity namespace is unavailable")?;
    capabilities_at(&ledger)
}

fn capabilities_at(ledger: &aterm_capacity::Ledger) -> Result<aterm_json::Value, String> {
    // A stored refusal still has a diagnostic path, which is not an available namespace.
    let _ = ledger.cohort_aware().map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        let machine = aterm_uds::process::host_id().ok_or("the machine identity is unavailable")?;
        let boot = aterm_uds::process::boot_id().ok_or("the boot identity is unavailable")?;
        let marker =
            std::str::from_utf8(aterm_capacity::RetainedClaim::reservation_protocol_marker())
                .map_err(|error| error.to_string())?;
        Ok(aterm_json::json!({
            "schema": 1,
            "protocol": marker,
            "machine": machine,
            "boot": boot,
            "ledger": ledger.dir(),
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = ledger;
        Err("retained reservation records are unsupported on this platform".into())
    }
}

#[cfg(target_os = "macos")]
#[path = "build_admit_reader_native.rs"]
mod native;
#[cfg(target_os = "macos")]
pub use native::Readiness;

#[cfg(test)]
#[path = "build_admit_reader_tests.rs"]
mod tests;
