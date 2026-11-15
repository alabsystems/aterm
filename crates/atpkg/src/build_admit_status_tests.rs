// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual ordinary-claim signal cleanup across a postbirth observation failure.

use super::*;
use aterm_uds::{deadline, process, spawnfd};
use spawnfd::Tether as _;
use std::io::Write as _;

const WORLD: &str = "ATPKG_STATUS_OBSERVATION_WORLD";
const FAULT: &str = "ATPKG_STATUS_OBSERVATION_FAULT";
const WAIT_LOSS: &str = "ATPKG_STATUS_OWNING_WAIT_LOSS";
const HELPER: &str = "ATPKG_STATUS_OBSERVATION_HELPER";
const OBSERVATION_ERROR: &str = "fixture: exec-status observation lost after execution proof";
const ACTOR: &str = "build_admit::tests::status_observation::ordinary_status_actor_if_requested";
const ROOT: &str = "build_admit::tests::status_observation::ordinary_status_root_if_requested";

fn wait_for<T>(why: &str, observe: impl FnMut() -> Option<T>) -> T {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, why, observe)
}

#[test]
fn ordinary_status_root_if_requested() {
    let Some(world) = std::env::var_os(WORLD) else {
        return;
    };
    let world = PathBuf::from(world);
    let output = std::fs::File::create(world.join("member-output")).unwrap();
    let defaults = crate::platform::Starting::at_defaults(&[libc::SIGTERM]);
    let member = defaults.spawning(|| {
        spawnfd::command("/bin/cat")
            .stdin(spawnfd::Stdio::inherit())
            .stdout(output)
            .spawn()
            .unwrap()
    });
    drop(defaults);
    // The parent owns the original stdin writer. This child inherits its reader;
    // the leader's SIGKILL cannot close the parent's lifeline.
    std::fs::write(world.join("member-pid"), member.id().to_string()).unwrap();
    wait_for("the actual cat echoes its execution proof", || {
        std::fs::read(world.join("member-output"))
            .is_ok_and(|bytes| bytes == b"actual member executed\n")
            .then_some(())
    });
    std::fs::write(world.join("probe"), "executed retained image\n").unwrap();
    wait_for(
        "the original parent requests leader self-termination",
        || world.join("end-root").exists().then_some(()),
    );
    // A signal death, unlike a normal exit, requires ordinary supervision to
    // drain what it observed before releasing the build's claim.
    process::kill_self_now();
}

#[test]
fn ordinary_status_actor_if_requested() {
    let Some(world) = std::env::var_os(WORLD) else {
        return;
    };
    let world = PathBuf::from(world);
    aterm_uds::tether::image_cache_for_tests(world.join("helper-images")).unwrap();
    aterm_uds::tether::through(PathBuf::from(
        std::env::var_os(HELPER).expect("declared inert helper"),
    ));
    assert!(
        aterm_uds::tether::prepared_executable_within(aterm_uds::deadline::FIRST_EXEC_HANG)
            .unwrap()
            .is_some(),
        "native status route is prepared"
    );
    let machine = machine();
    let seat = seat(&world, &machine, std::process::id());
    let Admission::Claimed(held) = admit(&test_build(), &world, &seat, &mut |_| {}) else {
        panic!("expected an actual private ordinary claim")
    };
    let mut command = spawnfd::transparent(std::env::current_exe().unwrap()).into_command();
    command.args(["--exact", ROOT, "--test-threads=1", "--nocapture"]);
    command.tether();
    let fault = std::env::var_os(FAULT).is_some();
    let supervise = |command: &mut spawnfd::Command| {
        supervise_with(
            Some(*held),
            command,
            &mut |_| {
                if std::env::var_os(WAIT_LOSS).is_some() {
                    std::fs::write(world.join("retaining"), "observed\n").unwrap();
                }
            },
            || None,
            |child, meter| {
                wait_for("leader and member produced actual execution proof", || {
                    world.join("probe").exists().then_some(())
                });
                let pid: u32 = std::fs::read_to_string(world.join("member-pid"))
                    .unwrap()
                    .parse()
                    .unwrap();
                let meter = meter.expect("the actual ordinary claim has its tree meter");
                wait_for("the native meter booked the actual member", || {
                    meter.read();
                    matches!(meter.left_observed(), aterm_capacity::tree_meter::Left::Read(ref rows)
                    if rows.iter().any(|&(seen, _)| seen == pid))
                    .then_some(())
                });
                std::fs::write(world.join("observed"), "ready\n").unwrap();
                if std::env::var_os(WAIT_LOSS).is_some() {
                    std::fs::write(world.join("end-root"), "go\n").unwrap();
                    let pid = i32::try_from(child.id()).unwrap();
                    let mut status = 0;
                    // SAFETY: this isolated actor deliberately consumes its actual child's
                    // owning wait outside the handle. The next production wait must get
                    // the kernel's ECHILD, while the booked member remains alive.
                    assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
                    assert!(libc::WIFSIGNALED(status));
                    std::fs::write(world.join("wait-lost"), "reaped\n").unwrap();
                }
            },
        )
    };
    let outcome = if fault {
        spawnfd::with_exec_status_observation_fault_for_tests(
            &world.join("probe"),
            &mut command,
            supervise,
        )
    } else {
        supervise(&mut command)
    };
    let rows = seat.ledger.as_ref().unwrap().list().unwrap().len();
    if let Err(error) = &outcome {
        aterm_uds::atomic_file::write(
            &world.join("observation-error"),
            error.to_string().as_bytes(),
            0o600,
        )
        .unwrap();
    }
    aterm_uds::atomic_file::write(
        &world.join("result"),
        format!("{} {rows}\n", if outcome.is_ok() { "ok" } else { "error" }).as_bytes(),
        0o600,
    )
    .unwrap();
    // Keep the real supervisor alive while the parent observes its result;
    // parent-death tether cleanup must not hide an early claim release.
    wait_for("the original parent completed its live observation", || {
        world.join("finish-actor").exists().then_some(())
    });
}

struct Actor {
    child: std::mem::ManuallyDrop<spawnfd::Child>,
    world: PathBuf,
}
impl Drop for Actor {
    fn drop(&mut self) {
        let _ = std::fs::write(self.world.join("end-root"), "go\n");
        let _ = std::fs::write(self.world.join("finish-actor"), "go\n");
        self.child.stdin.take();
        if matches!(self.child.exited(), Ok(false)) {
            let _ = self.child.kill();
        }
        if let Err(error) = self.child.wait() {
            eprintln!(
                "ordinary status cleanup Unknown: {error}; original child and world retained; fixture unqualified"
            );
            return;
        }
        // SAFETY: the same original child was positively reaped. This is
        // its sole manual drop; an unknown wait instead retains the owner.
        unsafe { std::mem::ManuallyDrop::drop(&mut self.child) };
    }
}

#[test]
fn an_ordinary_status_error_cannot_release_a_signalled_builds_live_member() {
    let mut results = Vec::new();
    let helper = aterm_tempfile::bin_exe("atpkg-status-fixture");
    for fault in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap().keep();
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .args(["--exact", ACTOR, "--test-threads=1", "--nocapture"])
            .env(WORLD, &world)
            .env(HELPER, &helper)
            .env(TREE_END_GRACE_ENV, "0")
            .setsid(true)
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::null())
            .stderr(spawnfd::Stdio::inherit());
        if fault {
            command.env(FAULT, "1");
        } else {
            command.env_remove(FAULT);
        }
        let mut actor = Actor {
            child: std::mem::ManuallyDrop::new(command.spawn().unwrap()),
            world,
        };
        let pid: i32 = wait_for("the actual leader started its member", || {
            std::fs::read_to_string(actor.world.join("member-pid"))
                .ok()?
                .parse()
                .ok()
        });
        let identity = process::read_identity(pid).expect("actual live member identity");
        actor
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"actual member executed\n")
            .unwrap();
        wait_for("actual member execution is proved", || {
            actor.world.join("probe").exists().then_some(())
        });
        // The root ends only once the supervisor's meter booked its member, faulted or not:
        // a member first read after its root died is reparented away and never booked
        // (`aterm_capacity::tree_meter`), so ending the root on the probe alone races the
        // meter's first read and leaves the supervisor waiting on a booking that cannot come.
        wait_for("the supervisor observed the live member", || {
            actor.world.join("observed").exists().then_some(())
        });
        assert_eq!(
            Ledger::at(actor.world.join("ledger")).list().unwrap().len(),
            1
        );
        std::fs::write(actor.world.join("end-root"), "go\n").unwrap();
        let result = wait_for(
            "original supervisor produces its result while alive",
            || std::fs::read_to_string(actor.world.join("result")).ok(),
        );
        assert!(
            matches!(actor.child.exited(), Ok(false)),
            "the actual supervisor remains alive"
        );
        let error = std::fs::read_to_string(actor.world.join("observation-error")).ok();
        let live = process::read_identity(pid) == Some(identity)
            && process::process_presence(pid) == process::ProcessPresence::Present;
        // Only the actual original pipe is used to retire a surviving member.
        actor.child.stdin.take();
        wait_for("member retires after its original lifeline closes", || {
            matches!(
                process::process_presence(pid),
                process::ProcessPresence::Absent | process::ProcessPresence::Exited
            )
            .then_some(())
        });
        std::fs::write(actor.world.join("finish-actor"), "go\n").unwrap();
        let status = wait_for("the original supervisor is positively reaped", || {
            actor.child.try_wait().unwrap()
        });
        assert!(status.success(), "isolated supervisor fixture: {status}");
        eprintln!(
            "ordinary status control fault={fault}: result={} live_member={live}",
            result.trim()
        );
        results.push((fault, result, live, error));
        deadline::poll_yield();
    }
    assert_eq!(results[0], (false, "ok 0\n".to_owned(), false, None));
    assert_eq!(
        results[1].3.as_deref(),
        Some(OBSERVATION_ERROR),
        "the real native observation fault must be consumed and preserved"
    );
    assert!(
        !results[1].2 || results[1].1 != "error 0\n",
        "a postbirth status error released its ordinary claim while the signalled leader's actual member still ran"
    );
}

#[test]
fn a_lost_owning_wait_keeps_the_original_claim_while_its_member_lives() {
    let world = aterm_tempfile::tempdir().unwrap().keep();
    let mut command = spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", ACTOR, "--test-threads=1", "--nocapture"])
        .env(WORLD, &world)
        .env(HELPER, aterm_tempfile::bin_exe("atpkg-status-fixture"))
        .env(WAIT_LOSS, "1")
        .env_remove(FAULT)
        .setsid(true)
        .stdin(spawnfd::Stdio::piped())
        .stdout(spawnfd::Stdio::null())
        .stderr(spawnfd::Stdio::inherit());
    let mut actor = Actor {
        child: std::mem::ManuallyDrop::new(command.spawn().unwrap()),
        world,
    };
    let member: i32 = wait_for("the actual member started", || {
        std::fs::read_to_string(actor.world.join("member-pid"))
            .ok()?
            .parse()
            .ok()
    });
    actor
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"actual member executed\n")
        .unwrap();
    wait_for(
        "the kernel owning wait was consumed outside its handle",
        || actor.world.join("wait-lost").exists().then_some(()),
    );
    wait_for("supervision either retains the error or returns", || {
        (actor.world.join("retaining").exists() || actor.world.join("result").exists())
            .then_some(())
    });
    assert!(
        matches!(actor.child.exited(), Ok(false)),
        "the original supervisor is alive"
    );
    assert_eq!(
        process::process_presence(member),
        process::ProcessPresence::Present
    );
    assert_eq!(
        Ledger::at(actor.world.join("ledger")).list().unwrap().len(),
        1,
        "a native ECHILD must not release the original reservation while the actual member lives"
    );
    assert!(
        !actor.world.join("result").exists(),
        "no retirement receipt was produced"
    );
    // Only the original pipe retires the real survivor. After owning-wait loss the
    // supervisor cannot manufacture a receipt from its old PID, even once this ends.
    actor.child.stdin.take();
    wait_for(
        "the actual member ended after its original lifeline closed",
        || {
            matches!(
                process::process_presence(member),
                process::ProcessPresence::Absent | process::ProcessPresence::Exited
            )
            .then_some(())
        },
    );
    actor.child.kill().unwrap();
    assert!(!actor.child.wait().unwrap().success());
}
