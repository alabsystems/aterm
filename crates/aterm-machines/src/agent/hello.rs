// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agent hello`: everything the controller needs before it signs anything, read as the
//! admin (no root): the host's identity and OS, the serial floors, the cached artifacts,
//! and whether a run is busy (design §4.4 step 1).

use super::{FAMILIES, Layout, state};
use crate::port::System;
use crate::protocol::Hello;
use std::io::Write;
use std::process::ExitCode;

/// The hello this host answers.
#[must_use]
pub fn hello(sys: &dyn System, layout: &Layout) -> Hello {
    let facts = crate::facts::gather(sys);
    let hardware = crate::hardware::describe(&facts)
        .map(|d| d.parts().to_vec())
        .unwrap_or_default();
    let vitals = crate::facts::gather_vitals(sys, &facts);
    let floors = state::floors_or_refuse(sys, layout).unwrap_or_default();
    Hello {
        schema: crate::SCHEMA,
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        // The agent runs as bin/aterm, a link the port's hash (O_NOFOLLOW) will not open:
        // hash the file it names.
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
            Vec::new()
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
mod tests {
    use super::*;
    use crate::port::{Fake, Ran};

    #[test]
    fn hello_reports_the_floors_cache_and_busy_run_from_root_owned_state() {
        let fake = Fake::root();
        let layout = Layout { root: "/A".into() };
        fake.file(
            "/A/state/floors.toml",
            b"[[floor]]\nprincipal = \"signing@ctl\"\nserial = 41\n",
        );
        fake.file(&format!("/A/cache/{}", "ab".repeat(32)), b"x");
        fake.file("/A/cache/.partial", b"x");
        fake.file("/A/state/run.lock", b"pid=77 run=1800000000-41\n");
        fake.alive(77);
        fake.file("/A/state/last_run", b"1799999000-40\n");
        let h = hello(&fake, &layout);
        assert_eq!(h.schema, 1);
        assert_eq!(h.floors.len(), 1);
        assert_eq!(h.floors[0].serial, 41);
        assert_eq!(h.cached, vec!["ab".repeat(32)]);
        assert_eq!(h.busy.as_deref(), Some("1800000000-41"));
        assert_eq!(h.last_run.as_deref(), Some("1799999000-40"));
        assert!(h.families.iter().any(|f| f == "keyboard"));
        // No hardware fact answered: no description, never a partial one.
        assert!(h.hardware.is_empty());
        let mut out = Vec::new();
        assert_eq!(print(&fake, &layout, &mut out), ExitCode::SUCCESS);
        let line = String::from_utf8(out).unwrap();
        let back: Hello = aterm_json::from_str(line.trim()).unwrap();
        assert_eq!(back.floors, h.floors);
    }

    #[test]
    fn hello_describes_the_mac_as_names_fills_hardware() {
        // The VM's own answers (27.0, VirtualMac2,1).
        let fake = Fake::root();
        fake.on("/usr/sbin/sysctl -n hw.model", Ran::ok("VirtualMac2,1\n"))
            .on(
                &crate::hardware::product_cmd().display(),
                Ran::ok(crate::hardware::tests::VM_PRODUCT),
            )
            .on(
                "/usr/sbin/sysctl -n machdep.cpu.brand_string",
                Ran::ok("Apple M5 (Virtual)\n"),
            )
            .on("/usr/sbin/sysctl -n hw.memsize", Ran::ok("8589934592\n"))
            .on(
                &crate::hardware::disk_cmd().display(),
                Ran::ok(crate::hardware::tests::VM_DISKS),
            );
        let h = hello(&fake, &Layout { root: "/A".into() });
        assert_eq!(h.hardware, ["Virtual Machine", "M5", "8GB", "90GB"]);
    }
}
