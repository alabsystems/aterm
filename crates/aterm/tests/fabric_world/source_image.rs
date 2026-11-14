// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Consumers in an actual debug frontend after its executable pathname changes.

use aterm_spec::wait::{FIRST_EXEC, MUST_HAPPEN, until_within};
use aterm_uds::spawnfd::{Child, command};

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn qualify(change: &str) {
    let scratch = aterm_tempfile::tempdir().unwrap().keep();
    let world = std::fs::canonicalize(&scratch).unwrap();
    let mut kept = aterm_uds::deadline::KeptUntilRan::new(&world);
    // Cover original/replacement/retained preparation and the consumer/retirement windows.
    let parked = FIRST_EXEC * 3 + MUST_HAPPEN * 6;
    let mut bystander = Owned(
        command("/bin/sleep")
            .arg(parked.as_secs().to_string())
            .spawn()
            .unwrap(),
    );
    let image = world.join("aterm");
    // The package's debug frontend, never the installed app or a release with fresh HOME.
    std::fs::copy(super::harness::aterm(), &image).unwrap();
    let managed = world.join("managed");
    atpkg::platform::install_twin_to_env(
        &managed,
        std::path::Path::new("/usr/bin/false"),
        &atpkg::shim_env::ShimEnv::NONE,
        "",
    )
    .unwrap();
    let mut front = Owned(
        command(&image)
            .arg("__source_image_fixture")
            .arg(&world)
            .arg(change)
            .env("HOME", &world)
            .env("ATERM_STATE_HOME", world.join("state"))
            .env("XDG_CONFIG_HOME", world.join("config"))
            .env("XDG_RUNTIME_DIR", world.join("runtime"))
            .env(atpkg::build_admit::HOLDER_ARMS_WITHIN_ENV, "60000")
            .spawn()
            .unwrap(),
    );
    until_within(FIRST_EXEC, "original frontend execution", || {
        let ready = std::fs::read_to_string(world.join("ready"))
            .ok()
            .filter(|s| s == "original producer executed\n");
        if ready.is_none() {
            assert!(
                front.0.try_wait().unwrap().is_none(),
                "frontend exited before proving its execution"
            );
        }
        ready
    });
    match change {
        "replace" | "lost" => {
            let replacement = world.join("replacement");
            aterm_uds::deadline::ad_hoc_copy(std::path::Path::new("/bin/echo"), &replacement);
            std::fs::rename(&replacement, &image).unwrap();
            let mut control = command(&image);
            control.arg("replacement executed");
            let output = aterm_uds::deadline::output_within(&mut control, FIRST_EXEC).unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"replacement executed\n");
        }
        "unlink" => std::fs::remove_file(&image).unwrap(),
        "unchanged" => {}
        _ => unreachable!(),
    }
    std::fs::write(world.join("go"), "go\n").unwrap();
    until_within(FIRST_EXEC, "late retained helper execution", || {
        if std::fs::read_to_string(world.join("consumer-ready"))
            .ok()
            .is_some_and(|line| line == "retained helper executed\n")
        {
            return Some(());
        }
        assert!(
            front.0.try_wait().unwrap().is_none(),
            "frontend exited before its retained helper was ready"
        );
        None
    });
    std::fs::write(world.join("consume"), "consume\n").unwrap();
    let result = until_within(MUST_HAPPEN, "native consumer result", || {
        std::fs::read_to_string(world.join("result")).ok()
    });
    if change == "lost" {
        assert_eq!(result, "original=false probe=no-exec handover=refused\n");
        assert!(
            until_within(MUST_HAPPEN, "refusing frontend exit", || front
                .0
                .try_wait()
                .unwrap())
            .success()
        );
        assert_eq!(
            std::fs::read_dir(world.join("ledger"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "claim"))
                .count(),
            0
        );
        assert!(bystander.0.try_wait().unwrap().is_none());
        kept.ran();
        return;
    }
    let mut lines = result.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!("original={} probe=exited", change == "unchanged")
    );
    let worker: i32 = lines.next().unwrap().parse().unwrap();
    let claim = std::path::PathBuf::from(lines.next().unwrap());
    until_within(MUST_HAPPEN, "worker execution receipt", || {
        std::fs::read_to_string(world.join("worker"))
            .ok()
            .filter(|s| s == "worker executed\n")
    });
    let identity = aterm_uds::process::read_identity(worker).expect("live worker");
    assert!(claim.exists(), "armed holder retains the claim");
    assert!(front.0.try_wait().unwrap().is_none());
    front.0.kill().unwrap();
    front.0.wait().unwrap();
    until_within(MUST_HAPPEN, "worker retires with producer", || {
        (aterm_uds::process::read_identity(worker) != Some(identity)).then_some(())
    });
    until_within(MUST_HAPPEN, "claim holder drains", || {
        (!claim.exists()).then_some(())
    });
    assert!(
        bystander.0.try_wait().unwrap().is_none(),
        "unrelated child survives"
    );
    kept.ran();
}

#[test]
fn unchanged_frontend_consumers() {
    qualify("unchanged");
}

#[test]
fn replaced_frontend_consumers() {
    qualify("replace");
}

#[test]
fn unlinked_frontend_consumers() {
    qualify("unlink");
}

#[test]
fn lost_helper_consumers_refuse_replacement() {
    qualify("lost");
}
