// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    assert!(
        !fake.calls().iter().any(|call| call.contains("hidutil")),
        "hello has no keyboard fact to report and must not search live devices: {:?}",
        fake.calls()
    );
    assert_eq!(h.schema, 1);
    assert_eq!(h.floors.len(), 1);
    assert_eq!(h.floors[0].serial, 41);
    assert_eq!(h.cached, vec!["ab".repeat(32)]);
    assert_eq!(h.busy.as_deref(), Some("1800000000-41"));
    assert_eq!(h.last_run.as_deref(), Some("1799999000-40"));
    assert!(h.families.iter().any(|f| f == "keyboard"));
    assert!(h.hardware.is_empty());
    let mut out = Vec::new();
    assert_eq!(print(&fake, &layout, &mut out), ExitCode::SUCCESS);
    let line = String::from_utf8(out).unwrap();
    let back: Hello = aterm_json::from_str(line.trim()).unwrap();
    assert_eq!(back.floors, h.floors);
}

#[test]
fn hello_describes_the_mac_as_names_fills_hardware() {
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
