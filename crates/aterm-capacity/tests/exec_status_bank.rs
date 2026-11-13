// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native group/session-only status-failure custody, not escaped-descendant closure.

#[cfg(all(target_os = "macos", debug_assertions))]
mod native {
    use aterm_capacity::build_cpu::{Owner, RetainedPool};
    use aterm_capacity::cohort::{Lifetime, launch::LaunchError};
    use aterm_capacity::cohort_namespace::Namespace;
    use aterm_capacity::leadership::Fence;
    use aterm_capacity::ledger::{CohortClaimState, Ledger};
    use aterm_capacity::retained_cohort::RetainedCohort;
    use aterm_capacity::{Accounting, Fixed, Need, Polled, Reading, Waiter};
    use aterm_spec::wait::{MUST_HAPPEN, until_within, within};
    use aterm_uds::spawnfd::{self, Spawned, Stdio, Tether as _};
    use std::io::{Read as _, Write as _};
    use std::num::NonZeroU32;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    const WORLD: &str = "ATERM_EXEC_STATUS_BANK_WORLD";

    struct Running(Spawned);
    impl Drop for Running {
        fn drop(&mut self) {
            let child = self.0.child_mut();
            drop(child.stdin.take());
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            if within(MUST_HAPPEN, Duration::from_millis(10), || {
                child.try_wait().ok().flatten()
            })
            .is_none()
            {
                eprintln!(
                    "status-bank: owning cleanup Unknown; external world preserved; fixture unqualified"
                );
            }
        }
    }

    pub(super) fn run() {
        let selected_world = std::env::var_os(WORLD).map(PathBuf::from);
        if let Some(world) = &selected_world {
            aterm_uds::tether::image_cache_for_tests(world.join("helper-cache.noindex")).unwrap();
        }
        if let Some(code) = aterm_uds::tether::entry() {
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            });
        }
        // Early retained-helper roles return above without creating a world. Ordinary
        // Cargo execution needs no ambient selector and keeps its private native evidence.
        let world = selected_world.unwrap_or_else(|| aterm_tempfile::tempdir().unwrap().keep());
        eprintln!("status-bank: preserved private world {}", world.display());
        if std::env::args().nth(1).as_deref() == Some("live-target") {
            let mut proof = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(world.join("target-proof"))
                .unwrap();
            writeln!(proof, "executed retained image").unwrap();
            drop(proof);
            std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
            return;
        }
        assert!(aterm_uds::tether::prepared_executable().unwrap().is_some());
        let home = world.join("private-home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(&world, &home).unwrap();
        let ledger = Ledger::at(namespace.fence());
        // A private test namespace with actual native claim and CPU locks. This is not a
        // grant from the account's live ledger or a reconstruction from context numbers.
        let machine = Fixed::new(Reading::of_gib(64, 40, 8));
        let claim =
            match Waiter::enter(&ledger, &machine, Need::new("exec-status-bank").mem_gib(30))
                .unwrap()
                .poll(&machine)
            {
                Polled::Admitted(claim) => claim,
                other => panic!("private fixture claim refused: {other:?}"),
            };
        let memory = claim.cohort_closure_ref().unwrap();
        let pool =
            RetainedPool::create_private(&world.join("cpu.noindex"), NonZeroU32::new(1).unwrap())
                .unwrap();
        let owner = Owner::current(Some(123));
        let fence = Fence::acquire(&namespace, Instant::now()).unwrap();
        let mut retained =
            RetainedCohort::new(&fence, claim, pool.try_acquire(owner).unwrap().unwrap()).unwrap();
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        command
            .arg("live-target")
            .env(WORLD, &world)
            .stdin(Stdio::piped())
            .tether();
        let result = spawnfd::with_exec_status_observation_fault_for_tests(
            &world.join("target-proof"),
            &mut command,
            |command| retained.launch(&fence, command),
        );
        let failure = match result {
            Err(failure) => failure,
            Ok(launched) => {
                let (spawned, _) = launched.into_parts();
                let _running = Running(spawned);
                panic!("observation fault did not refuse the actual launch");
            }
        };
        let LaunchError::Unbound { spawned, error } = failure else {
            panic!("postbirth observation fault lost the actual child");
        };
        let mut running = Running(*spawned);
        assert!(
            error
                .to_string()
                .contains("observation lost after execution proof")
        );
        assert!(running.0.is_original_child());
        assert!(!running.0.child_mut().exited().unwrap());
        assert_eq!(
            std::fs::read_to_string(world.join("target-proof")).unwrap(),
            "executed retained image\n"
        );
        assert!(retained.try_finish(&fence).is_err());
        assert!(pool.try_acquire(owner).unwrap().is_none());
        let rows = ledger.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].accounting, Accounting::Whole);
        assert_eq!(rows[0].need.mem, 30 * 1024 * 1024 * 1024);
        eprintln!(
            "status-bank: live original Unbound; CPU contender denied; whole memory claim retained"
        );

        let cohort = retained.retry_bind(&fence, &mut running.0).unwrap();
        assert_eq!(cohort.lifetime(), Lifetime::Present);
        assert!(retained.try_finish(&fence).is_err());
        assert!(pool.try_acquire(owner).unwrap().is_none());
        drop(running.0.child_mut().stdin.take());
        let status = until_within(MUST_HAPPEN, "original target exit and owning reap", || {
            running.0.child_mut().try_wait().unwrap()
        });
        assert!(status.success());
        until_within(MUST_HAPPEN, "original native group drains", || {
            (cohort.lifetime() == Lifetime::Drained).then_some(())
        });
        retained.try_finish(&fence).unwrap();
        assert!(retained.is_settled());
        pool.try_acquire(owner)
            .unwrap()
            .expect("settled CPU bank free")
            .settle()
            .unwrap();
        // Durable cohort finish does not drop the original memory guard still owned by
        // RetainedCohort. Release that settled original owner through its normal Drop.
        assert_eq!(ledger.list().unwrap().len(), 1);
        drop(retained);
        assert_eq!(
            ledger.cohort_claim_state(&memory).unwrap(),
            CohortClaimState::Closed
        );
        assert!(ledger.list().unwrap().is_empty());
        eprintln!(
            "status-bank: original owning wait and native group drain; CPU and memory banks free"
        );
        // The external world and retained helper cache deliberately survive all outcomes.
    }
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn main() {
    native::run();
}

#[cfg(not(all(target_os = "macos", debug_assertions)))]
fn main() {
    eprintln!("exec-status bank: native macOS debug control unavailable on this target");
}
