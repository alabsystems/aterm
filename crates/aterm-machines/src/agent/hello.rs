// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agent hello`: host identity, OS facts, serial floors, cached artifacts, and run state.

use super::{FAMILIES, Layout, state};
use crate::port::System;
use crate::protocol::Hello;
use std::io::Write;
use std::process::ExitCode;

/// The hello this host answers.
#[must_use]
pub fn hello(sys: &dyn System, layout: &Layout) -> Hello {
    let facts = crate::facts::gather_without_keyboards(sys);
    let hardware = crate::hardware::describe(&facts)
        .map(|d| d.parts().to_vec())
        .unwrap_or_default();
    let vitals = crate::facts::gather_vitals(sys, &facts);
    let floors = state::floors_or_refuse(sys, layout).unwrap_or_default();
    Hello {
        schema: crate::SCHEMA,
        agent_version: aterm_stamp::app_version().to_owned(),
        agent_sha256: std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .ok()
            .and_then(|exe| sys.sha256(&exe).ok())
            .unwrap_or_default(),
        families: FAMILIES.iter().map(|f| (*f).to_owned()).collect(),
        uuid: facts.uuid,
        serial_number: facts.serial,
        computer_name: facts.computer_name,
        local_host_name: facts.local_host_name,
        os_version: facts.os_version,
        os_build: facts.os_build,
        arch: facts.arch,
        hardware,
        console_user: facts.console_user,
        floors,
        cached: cached(sys, layout),
        busy: state::busy(sys, layout),
        last_run: state::last_run(sys, layout),
        sessions: if sys.own_uuid().is_some() && crate::sessions::is_live_account() {
            crate::sessions::scan_local_aterm_sessions()
        } else {
            crate::protocol::SessionSnapshot::default()
        },
        vitals,
    }
}

/// The SHA-256 of every artifact in the root cache.
#[must_use]
pub fn cached(sys: &dyn System, layout: &Layout) -> Vec<String> {
    sys.list_dir(&layout.cache())
        .unwrap_or_default()
        .into_iter()
        .filter(|n| super::is_sha256(n))
        .collect()
}

/// Print the hello as one JSON line.
pub fn print(sys: &dyn System, layout: &Layout, out: &mut dyn Write) -> ExitCode {
    let hello = hello(sys, layout);
    match aterm_json::to_string(&hello) {
        Ok(line) => {
            let _ = writeln!(out, "{line}");
            ExitCode::SUCCESS
        }
        Err(e) => super::refuse(out, &format!("hello: {e}")),
    }
}

#[cfg(test)]
#[path = "hello_tests.rs"]
mod tests;
