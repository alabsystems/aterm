// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Causal native fixture; the private fault cannot be armed by ordinary spawn callers.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

thread_local! {
    static FAULT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub(super) fn with_fault<R>(proof: &Path, action: impl FnOnce() -> R) -> R {
    struct Reset(Option<PathBuf>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FAULT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let reset = Reset(FAULT.with(|slot| slot.replace(Some(proof.to_owned()))));
    assert!(reset.0.is_none(), "observation fault scopes cannot nest");
    action()
}

pub(super) fn after_read() -> io::Result<()> {
    let Some(proof) = FAULT.with(|slot| slot.borrow_mut().take()) else {
        return Ok(());
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if std::fs::read_to_string(&proof).is_ok_and(|text| text == "executed retained image\n") {
            return Err(io::Error::other(
                "fixture: exec-status observation lost after execution proof",
            ));
        }
        assert!(
            Instant::now() < deadline,
            "target did not produce its execution proof"
        );
        crate::deadline::poll_yield();
    }
}

pub(super) fn run(world: &Path) {
    use crate::spawnfd::{BirthAttempt, SpawnFailure, Tether as _, command};

    // Qualify the real retained helper before arming any observation fault. The fixture's
    // main calls tether::entry before threads; the target retains its Cargo binary name.
    assert!(crate::tether::prepared_executable().unwrap().is_some());
    let executable = std::env::current_exe().unwrap();
    let mut results = Vec::new();
    for case in ["healthy", "exec-refusal", "observation-fault"] {
        let root = world.join(case);
        std::fs::create_dir(&root).unwrap();
        if case == "observation-fault" {
            FAULT.with(|slot| *slot.borrow_mut() = Some(root.join("probe")));
        }
        let program = if case == "exec-refusal" {
            root.join("absent")
        } else {
            executable.clone()
        };
        let mut cmd = command(program);
        cmd.arg("probe")
            .env("ATERM_TETHER_IMAGE_TEST_WORLD", &root)
            .tether();
        let attempt = BirthAttempt::new().unwrap();
        let key = attempt.key().clone();
        let outcome = cmd.spawn_cohort(attempt);
        let (class, retired) = match outcome {
            Ok(mut spawned) => ("spawned", spawned.child_mut().wait().unwrap().success()),
            Err(SpawnFailure::NotSpawned(_)) => ("not-spawned", false),
            Err(SpawnFailure::Refused(_)) => ("refused", false),
            Err(SpawnFailure::Unbound { mut spawned, error }) => {
                assert_eq!(spawned.key(), &key);
                assert!(spawned.is_original_child());
                eprintln!("exec-status control {case}: original observation error: {error}");
                ("unbound", spawned.child_mut().wait().is_ok())
            }
        };
        let proved = std::fs::read_to_string(root.join("probe"))
            .is_ok_and(|text| text == "executed retained image\n");
        eprintln!(
            "exec-status control {case}: class={class} execution_proof={proved} positive_owned_wait={retired}"
        );
        results.push((case, class, proved, retired));
    }
    assert_eq!(results[0], ("healthy", "spawned", true, true));
    assert_eq!(results[1].1, "unbound");
    assert!(!results[1].2);
    assert!(
        results[1].3,
        "exec refusal must traverse status observation and a positive original wait"
    );
    assert!(
        results[2].2 && results[2].3,
        "fault requires actual execution and positive original wait"
    );
    assert_eq!(
        results[2].1, "unbound",
        "an observation error after proven execution cannot certify no child"
    );
}
