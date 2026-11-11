// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native controls demonstrate this final exec only, not a policy receipt or
//! protection against later exec, nor a combined first-main no-fork-or-exec seal.
use super::*;
use std::io::Write as _;
use std::os::unix::process::ExitStatusExt as _;

const PAYLOAD: &str = "metadata_prelude::footprint_tests::native_footprint_target";
const ACTOR: &str = "metadata_prelude::tests::native_prelude_actor";
const ROLE: &str = "ATERM_TEST_PRELUDE_FOOTPRINT";
const ENV_ROLE: &str = "ATERM_TEST_PRELUDE_LOADER_ENVIRONMENT";
const ENV_CANARY: &str = "LD_ATERM_FINAL_ENV_CANARY";

pub(super) fn observe_first_image_environment() {
    if std::env::var_os(ENV_ROLE).is_some() {
        println!(
            "first-image-loader-present={}",
            std::env::var_os(ENV_CANARY).is_some()
        );
        std::io::stdout().flush().unwrap();
    }
}

#[test]
fn native_final_loader_environment_target() {
    if std::env::var_os(ENV_ROLE).is_some() {
        assert_eq!(
            std::env::var(ENV_CANARY).as_deref(),
            Ok("selected-image-only")
        );
        println!("final-image-loader-value=selected-image-only");
    }
}

#[test]
fn actual_prelude_keeps_selected_loader_environment_for_final_exec_only() {
    const TEST: &str = "metadata_prelude::footprint_tests::actual_prelude_keeps_selected_loader_environment_for_final_exec_only";
    const FINAL: &str = "metadata_prelude::footprint_tests::native_final_loader_environment_target";
    if !aterm_uds::test_binary::isolated_lifecycle(TEST) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let mut target = spawnfd::command(std::env::current_exe().unwrap());
    target
        .args(["--exact", FINAL, "--nocapture"])
        .env_clear()
        .env(ENV_ROLE, "observe")
        .env(ENV_CANARY, "selected-image-only")
        .current_dir(std::env::current_dir().unwrap());
    let deadline = crate::fixture_deadline(Instant::now());
    let (mut invalid_owner, mut invalid_prelude) = Owner::new(&target, &[], deadline).unwrap();
    invalid_prelude.env(ENV_CANARY, "late-first-image-value");
    assert!(invalid_owner.configured(&mut invalid_prelude).is_err());
    assert!(invalid_owner.payload.is_empty());
    drop((invalid_owner, invalid_prelude));
    let (mut owner, mut prelude) = Owner::new(&target, &[], deadline).unwrap();
    prelude
        .inherit_guards_at(vec![owner.extra().unwrap()])
        .unwrap();
    owner.configured(&mut prelude).unwrap();
    prelude.stdout(spawnfd::Stdio::piped());
    let mut child = OwnedChild(prelude.spawn().unwrap());
    let original = process::read_lineage(child.0.id() as i32).unwrap();
    loop {
        if owner
            .poll(&mut child.0, |pid| {
                assert_eq!(pid, original.identity.pid as u32);
                Ok(())
            })
            .unwrap()
        {
            break;
        }
        assert!(Instant::now() < deadline, "actual final native hold");
        std::thread::sleep(Duration::from_millis(2));
    }
    let selected = process::read_lineage(child.0.id() as i32).unwrap();
    assert_eq!(selected.identity.birth, original.identity.birth);
    assert_eq!(selected.uniqueid, original.uniqueid);
    assert_ne!(selected.identity.idversion, original.identity.idversion);
    child.0.resume().unwrap();
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "actual final image exited");
        std::thread::sleep(Duration::from_millis(2));
    };
    let mut stdout = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert!(status.success(), "{status:?}: {stdout}");
    assert!(
        stdout
            .lines()
            .any(|line| line == "first-image-loader-present=false"),
        "{stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "final-image-loader-value=selected-image-only"),
        "{stdout}"
    );
}

#[test]
fn native_footprint_target() {
    if std::env::var_os(ROLE).is_none() {
        return;
    }
    let pid = std::process::id();
    assert!(no_fork(pid).unwrap());
    // Actual post-exec denial, in addition to the parent's native observation.
    // SAFETY: this isolated selected test process only probes the installed
    // no-fork policy; an unexpectedly created child immediately calls _exit.
    let forked = unsafe { libc::fork() };
    if forked == 0 {
        // SAFETY: terminate the unexpected post-fork child without running
        // allocator-backed Rust cleanup or touching inherited synchronization.
        unsafe { libc::_exit(93) };
    }
    assert_eq!(forked, -1);
    println!(
        "final-footprint {pid} {}",
        process::read_birth(pid as i32).unwrap().0
    );
    std::io::stdout().flush().unwrap();
    // Same finite work with and without policy; never an unbounded OOM probe.
    let mut pages = vec![0_u8; 256 * 1024 * 1024];
    for page in pages.chunks_mut(4096) {
        page[0] = 1;
    }
    std::hint::black_box(&pages);
    std::thread::sleep(Duration::from_millis(250));
}

#[test]
fn repeated_configuration_preserves_the_committed_plan_and_pending_policy() {
    crate::runtime::test_endpoint().unwrap();
    let mut target = spawnfd::command(std::env::current_exe().unwrap());
    target.current_dir(std::env::current_dir().unwrap());
    for initial in [None, Some(32)] {
        if let Some(mib) = initial {
            target.footprint_limit(spawnfd::FootprintLimit::new(mib).unwrap());
        }
        let (mut owner, mut prelude) =
            Owner::new(&target, &[], Instant::now() + Duration::from_secs(60)).unwrap();
        assert!(prelude.get_footprint_limit().is_none());
        prelude.footprint_limit(spawnfd::FootprintLimit::new(64).unwrap());
        prelude.env("ATERM_TEST_INVALID_ENV", OsString::from("bad\0value"));
        assert!(owner.configured(&mut prelude).is_err());
        assert!(owner.payload.is_empty());
        assert_eq!(prelude.get_footprint_limit().unwrap().mib(), 64);
        prelude.env_remove("ATERM_TEST_INVALID_ENV");
        owner.configured(&mut prelude).unwrap();
        assert!(prelude.get_footprint_limit().is_none());
        let committed = owner.payload.clone();
        prelude.footprint_limit(spawnfd::FootprintLimit::new(16).unwrap());
        assert!(owner.configured(&mut prelude).is_err());
        assert_eq!(prelude.get_footprint_limit().unwrap().mib(), 16);
        assert_eq!(owner.payload, committed);
    }
}

struct OwnedChild(spawnfd::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn actual_prelude_transfers_final_footprint_policy() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "metadata_prelude::footprint_tests::actual_prelude_transfers_final_footprint_policy",
    ) {
        return;
    }
    crate::runtime::test_endpoint().unwrap();
    let run = |initial: Option<u32>, configured: Option<u32>| {
        let mut target = spawnfd::command(std::env::current_exe().unwrap());
        target
            .args(["--exact", PAYLOAD, "--nocapture"])
            .env_clear()
            .current_dir(std::env::current_dir().unwrap());
        if let Some(mib) = initial {
            target.footprint_limit(spawnfd::FootprintLimit::new(mib).unwrap());
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let (mut owner, _) = Owner::new(&target, &[], deadline).unwrap();
        let mut prelude = spawnfd::command(std::env::current_exe().unwrap());
        prelude
            .args(["--exact", ACTOR, "--nocapture"])
            .env_clear()
            .env(ROLE, "allocate")
            .env("ATERM_TEST_METADATA_PRELUDE_FD", owner.target.to_string())
            .current_dir(std::env::current_dir().unwrap())
            .setsid(true)
            .start_suspended(true)
            .stdout(spawnfd::Stdio::piped())
            .stderr(spawnfd::Stdio::piped());
        prelude
            .inherit_guards_at(vec![owner.extra().unwrap()])
            .unwrap();
        if let Some(mib) = configured {
            prelude.footprint_limit(spawnfd::FootprintLimit::new(mib).unwrap());
        }
        owner.configured(&mut prelude).unwrap();
        assert!(prelude.get_footprint_limit().is_none());
        let mut owned = OwnedChild(prelude.spawn().unwrap());
        let child = &mut owned.0;
        let original = process::read_lineage(child.id() as i32).unwrap();
        loop {
            if owner
                .poll(child, |pid| {
                    assert_eq!(pid, original.identity.pid as u32);
                    assert!(no_fork(pid)?);
                    Ok(())
                })
                .unwrap()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "second native stop hang detector"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let selected = process::read_lineage(child.id() as i32).unwrap();
        assert_eq!(selected.identity.birth, original.identity.birth);
        assert_eq!(selected.uniqueid, original.uniqueid);
        assert_eq!(selected.pgid, original.pgid);
        assert_ne!(selected.identity.idversion, original.identity.idversion);
        child.resume().unwrap();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "finite allocation exit hang detector"
            );
            std::thread::sleep(Duration::from_millis(2));
        };
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(
            stdout.lines().any(|line| line
                == format!(
                    "final-footprint {} {}",
                    original.identity.pid, original.identity.birth.0
                )),
            "actual selected image must enter with original PID/birth: {stdout}\n{stderr}"
        );
        (status, stderr)
    };
    let (baseline, stderr) = run(None, None);
    assert!(
        baseline.success(),
        "uncapped finite allocation: {baseline:?}\n{stderr}"
    );
    let (limited, stderr) = run(Some(64), None);
    // Test-only native behavioral control. Production never attributes a generic
    // SIGKILL to footprint policy and never obtains Child ownership from a PID.
    assert_eq!(
        limited.signal(),
        Some(libc::SIGKILL),
        "capped finite allocation: {limited:?}\n{stderr}"
    );
    let (lowered, stderr) = run(Some(512), Some(64));
    assert_eq!(
        lowered.signal(),
        Some(libc::SIGKILL),
        "final resource configuration must lower the selected cap: {lowered:?}\n{stderr}"
    );
    let (raised, stderr) = run(Some(64), Some(512));
    assert!(
        raised.success(),
        "final resource configuration must replace the earlier cap: {raised:?}\n{stderr}"
    );
}
