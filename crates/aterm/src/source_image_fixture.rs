// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native consumer qualification before setup, credentials or fleet discovery.

use aterm_uds::spawnfd::Tether as _;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

fn result(world: &std::path::Path, text: String) -> std::io::Result<()> {
    std::fs::write(world.join("result.pending"), text)?;
    std::fs::rename(world.join("result.pending"), world.join("result"))
}

pub fn entry(argv: &[OsString]) -> Option<ExitCode> {
    if argv.get(1)?.to_str()? != "__source_image_fixture" {
        return None;
    }
    Some(run(argv).unwrap_or_else(|error| {
        eprintln!("source image fixture: {error}");
        ExitCode::FAILURE
    }))
}

fn run(argv: &[OsString]) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let world = std::fs::canonicalize(PathBuf::from(argv.get(2).ok_or("missing world")?))?;
    if std::env::var_os("HOME").as_deref() != Some(world.as_os_str()) {
        return Err("fixture requires its private world as HOME".into());
    }
    let original = world.join("aterm");
    if !atpkg::native_launcher_for_test(&original) {
        return Err("original producer was not admitted".into());
    }
    std::fs::write(world.join("ready"), "original producer executed\n")?;
    // A hang detector, never a latency budget.
    let hang = std::time::Duration::from_secs(60);
    let deadline = std::time::Instant::now() + hang;
    while !world.join("go").exists() {
        if std::time::Instant::now() >= deadline {
            return Err("controller did not release fixture".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let retained = aterm_uds::tether::prepared_executable()?.ok_or("no prepared helper")?;
    if atpkg::native_launcher_for_test(retained) {
        return Err("retained helper was admitted as original producer".into());
    }
    let lost = argv.get(3).is_some_and(|arg| arg == "lost");
    if lost {
        if !retained.starts_with(&world) || retained == original {
            return Err("retained image is outside this fixture's world".into());
        }
        // prepared_executable has reaped its READY child: the copied Mach-O has
        // proved execution before this negative control removes its pathname.
        std::fs::remove_file(retained)?;
    }
    let admitted = atpkg::native_launcher_for_test(&original);
    let probe = aterm_agent::harness::upgrade_drive::first_exec_probe_for_test(
        &world.join("managed"),
        hang,
        &world.join("holds"),
    );
    if lost {
        if atpkg::build_admit::hand_over_for_test(&world).is_ok() {
            return Err("claim handover accepted a lost helper".into());
        }
        result(
            &world,
            format!("original={admitted} probe={probe} handover=refused\n"),
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let claim = atpkg::build_admit::hand_over_for_test(&world)?;
    let mut worker = aterm_uds::spawnfd::command("/bin/sh");
    worker
        .args([
            "-c",
            "printf 'worker executed\\n' > \"$1\"; exec /bin/sleep 600",
            "worker",
        ])
        .arg(world.join("worker"))
        .tether();
    let mut worker = worker.spawn()?;
    result(
        &world,
        format!(
            "original={admitted} probe={probe}\n{}\n{}\n",
            worker.id(),
            claim.display()
        ),
    )?;
    // The controller kills this original producer. Its worker and detached claim holder
    // must retire independently, while the controller's unrelated bystander survives.
    let status = worker.wait()?;
    Err(format!("controller did not end producer before worker ended: {status}").into())
}
