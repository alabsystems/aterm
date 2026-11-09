// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Opt-in, local release/dev contention instrument; executes no release product.

use aterm_capacity::tree_meter::TreeMeter;
use aterm_capacity::{Admit, Ledger, Need, Rank};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn output(command: &mut aterm_uds::spawnfd::Command, path: &Path) {
    let file = std::fs::File::create(path).unwrap();
    command.stdout(file.try_clone().unwrap()).stderr(file);
}

fn repo() -> PathBuf {
    let root = std::env::current_dir().unwrap();
    assert!(
        root.join("crates/aterm-capacity/Cargo.toml").is_file(),
        "run the pilot from the checkout root"
    );
    root
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

#[test]
fn release_worker_if_requested() {
    let Some(at) = std::env::var_os("ATPKG_PILOT_RELEASE_ROOT").map(PathBuf::from) else {
        return;
    };
    let claim = aterm_capacity::admit(
        Need::new(format!(
            "admission-pilot release {}",
            at.file_name().unwrap().to_string_lossy()
        ))
        .rank(Rank::Release)
        .cores(aterm_capacity::cores())
        .mem_gib(4),
        Admit::Wait,
    )
    .unwrap();
    let pool = aterm_capacity::Jobserver::new(&at.join("jobs"), claim.cores()).unwrap();
    let mut command = aterm_uds::spawnfd::command(std::env::var_os("ATPKG_PILOT_TARGO").unwrap());
    command
        .args([
            "--unverified",
            "build",
            "--release",
            "--lib",
            "-p",
            "aterm-capacity",
            "--locked",
            "--offline",
        ])
        .arg("--target-dir")
        .arg(at.join("slice.noindex"))
        .current_dir(repo())
        .env(aterm_capacity::RELEASE_RUN_ENV, "1");
    let _token = pool.attach_cargo(&mut command, &|| false).unwrap();
    output(&mut command, &at.join("slice.log"));
    let started_ms = now();
    let start = Instant::now();
    let mut release = command.spawn().unwrap();
    let meter = TreeMeter::start(release.id());
    std::fs::write(at.join("ready"), "ready").unwrap();
    let usage = release.wait_with_rusage().unwrap();
    let wall = start.elapsed().as_secs_f64();
    let peak = meter.stop(usage.maxrss);
    assert!(usage.status.success(), "{}", at.join("slice.log").display());
    let row = aterm_json::json!({"slots":claim.cores(),"slice_wall_s":wall,"slice_cpu_s":usage.cpu.as_secs_f64(),"slice_peak_bytes":peak,"slice_started_unix_ms":started_ms,"slice_ended_unix_ms":now()});
    std::fs::write(at.join("slice-result.json"), row.to_string()).unwrap();
}

/// Run with ATPKG_PILOT_FRONT naming the newly built dev atpkg, and
/// ATPKG_PILOT_TARGO naming the pinned store's targo. Distinct cold target dirs
/// keep the fixed library graph identical across ABBA. The baseline dev uses
/// the installed shim, providing the pre-change negative control.
#[test]
#[ignore = "real local build measurement, requires explicit tool paths"]
fn release_and_dev_abba() {
    let front = PathBuf::from(std::env::var_os("ATPKG_PILOT_FRONT").unwrap());
    let targo = PathBuf::from(std::env::var_os("ATPKG_PILOT_TARGO").unwrap());
    let repo = repo();
    let root = repo
        .join(".ubd")
        .join(format!("admission-pilot-{}.noindex", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let ledger = Ledger::user().unwrap();
    for (index, mode) in ["alone", "before", "after", "after", "before", "alone"]
        .iter()
        .enumerate()
    {
        let at = root.join(format!("{index}-{mode}"));
        std::fs::create_dir(&at).unwrap();
        // Sibling processes: an ancestor's core pool deliberately does not constrain
        // a branch borrowing that pool. A dev build in another session is not a branch.
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "build_admit::tests::release_pilot::release_worker_if_requested",
            ])
            .env("ATPKG_PILOT_RELEASE_ROOT", &at);
        output(&mut command, &at.join("worker.log"));
        let start = Instant::now();
        let mut release = command.spawn().unwrap();
        let mut dev = None;
        let mut dev_start = None;
        let mut admitted_ms = None;
        let mut asked_ms = None;
        let mut max_load1 = 0.0_f64;
        loop {
            max_load1 = max_load1.max(aterm_capacity::Reading::now(&repo).load1.unwrap_or(0.0));
            if *mode != "alone"
                && dev.is_none()
                && at.join("ready").exists()
                && start.elapsed() >= Duration::from_secs(1)
            {
                let mut command = if *mode == "before" {
                    aterm_uds::spawnfd::command("targo")
                } else {
                    let mut command = aterm_uds::spawnfd::command(&front);
                    command.arg(super::super::VERB).arg(&targo);
                    command
                };
                command
                    .args([
                        "--unverified",
                        "build",
                        "--lib",
                        "-p",
                        "aterm-capacity",
                        "--locked",
                        "--offline",
                    ])
                    .arg("--target-dir")
                    .arg(at.join("dev.noindex"))
                    .current_dir(&repo);
                output(&mut command, &at.join("dev.log"));
                dev_start = Some(Instant::now());
                dev = Some(command.spawn().unwrap());
            }
            if let Some(dev) = &dev {
                for record in ledger.list().unwrap().iter().filter(|r| r.pid == dev.id()) {
                    asked_ms = Some(record.start_ms);
                    if let Some(held) = record.held {
                        admitted_ms = Some(held.admitted_ms);
                    }
                }
            }
            if release.try_wait_with_rusage().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let usage = release.wait_with_rusage().unwrap();
        assert!(
            usage.status.success(),
            "{}",
            at.join("worker.log").display()
        );
        let dev_row = dev.map(|mut dev| {
            while dev.try_wait_with_rusage().unwrap().is_none() {
                for record in ledger.list().unwrap().iter().filter(|r| r.pid == dev.id()) {
                    asked_ms = Some(record.start_ms);
                    if let Some(held) = record.held {
                        admitted_ms = Some(held.admitted_ms);
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let usage = dev.wait_with_rusage().unwrap();
            assert!(usage.status.success(), "{}", at.join("dev.log").display());
            aterm_json::json!({"wall_s":dev_start.unwrap().elapsed().as_secs_f64(),"cpu_s":usage.cpu.as_secs_f64(),"peak_bytes":usage.maxrss,"asked_unix_ms":asked_ms,"admitted_unix_ms":admitted_ms})
        });
        let mut row: aterm_json::Value =
            aterm_json::from_str(&std::fs::read_to_string(at.join("slice-result.json")).unwrap())
                .unwrap();
        let fields = row.as_object_mut().unwrap();
        fields.insert("mode".into(), aterm_json::json!(mode));
        fields.insert("max_load1".into(), aterm_json::json!(max_load1));
        fields.insert("dev".into(), aterm_json::json!(dev_row));
        if *mode != "alone" {
            let admitted = admitted_ms.expect("the dev admission must be observed");
            let ended = row["slice_ended_unix_ms"].as_u64().unwrap();
            assert_eq!(
                admitted >= ended,
                *mode == "after",
                "negative control and fix must differ"
            );
        }
        println!("admission-pilot: {row}");
        std::fs::write(at.join("result.json"), row.to_string()).unwrap();
    }
    println!("admission-pilot-root: {}", root.display());
}
