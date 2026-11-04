// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::io::{Read as _, Write as _};
use std::time::{Duration, Instant};

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
const FIXTURE: &str = "store::first_exec_tests::first_exec_holder_process";
const BUILD_ENV: &str = "ATPKG_FIRST_EXEC_HOLDER_BUILD";
const READY_ENV: &str = "ATPKG_FIRST_EXEC_HOLDER_READY";
const CLOCK: u64 = 1_000;

fn until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now().checked_add(HANG).expect("hang detector");
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// The same Rust image, run in place. The target below is inert owned data, never an executable.
#[test]
fn first_exec_holder_process() {
    let Some(build) = std::env::var_os(BUILD_ENV) else {
        return;
    };
    let ready = PathBuf::from(std::env::var_os(READY_ENV).expect("ready"));
    let hold = hold_first_exec_at(&PathBuf::from(build).join("bin/tool"), Some(CLOCK)).unwrap();
    std::fs::write(ready, b"held").unwrap();
    let mut release = [0];
    std::io::stdin().read_exact(&mut release).unwrap();
    hold.ran();
}

struct Child(aterm_uds::spawnfd::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn an_own_exit_never_releases_another_process_at_the_same_clock_reading() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let build = scratch.path().join("store/tool/1");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    let exe = build.join("bin/tool");
    std::fs::write(&exe, b"inert owned data").unwrap();

    let alone = hold_first_exec_at(&exe, Some(CLOCK)).unwrap();
    assert!(held_unproven_at(&build, Some(CLOCK)));
    alone.ran();
    assert!(
        !held_unproven_at(&build, Some(CLOCK)),
        "ordinary own exit releases"
    );
    let first = hold_first_exec_at(&exe, Some(CLOCK)).unwrap();
    let ready = scratch.path().join("ready");
    let mut child = Child(
        aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", FIXTURE, "--nocapture"])
            .env(BUILD_ENV, &build)
            .env(READY_ENV, &ready)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("same Rust image"),
    );
    let entered = until(|| ready.is_file());
    first.ran();
    let other_still_held = held_unproven_at(&build, Some(CLOCK));
    if let Some(mut stdin) = child.0.stdin.take() {
        let _ = stdin.write_all(b"x");
    }
    let ended = until(|| child.0.try_wait().is_ok_and(|status| status.is_some()));
    let own_exit = ended && child.0.wait().is_ok_and(|status| status.success());
    assert!(
        entered && own_exit,
        "distinct Rust holder entered and exited before judgment"
    );
    assert!(
        !held_unproven_at(&build, Some(CLOCK)),
        "all ordinary exits release their holds"
    );
    assert!(
        other_still_held,
        "first.ran erased another process's hold at the same fixed clock"
    );
}

/// A bundle's resolve check registers every exposed binary of one build at once: siblings in one
/// process take their turns at the program's gate, so each registers and each release takes its
/// mark (nux round 9 found the trust bundle's flip failing on `first exec registration or removal
/// is busy`). A registration while a removal holds the gate still refuses at once: the negative
/// control, so the turn is no wait on a removal.
#[test]
fn siblings_in_one_process_each_register_and_release_while_a_removal_still_refuses() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let build = scratch.path().join("store/tool/1");
    std::fs::create_dir_all(build.join("bin")).unwrap();
    let bins: Vec<PathBuf> = (0..16)
        .map(|i| {
            let bin = build.join(format!("bin/tool{i}"));
            std::fs::write(&bin, b"inert owned data").unwrap();
            bin
        })
        .collect();
    for _ in 0..4 {
        let held: Vec<std::io::Result<FirstExec>> = std::thread::scope(|scope| {
            let started: Vec<_> = bins
                .iter()
                .map(|bin| scope.spawn(move || hold_first_exec_at(bin, Some(CLOCK))))
                .collect();
            started.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let held: Vec<FirstExec> = held
            .into_iter()
            .map(|hold| hold.expect("each sibling registers"))
            .collect();
        assert!(held_unproven_at(&build, Some(CLOCK)));
        std::thread::scope(|scope| {
            for hold in held {
                scope.spawn(move || hold.ran());
            }
        });
        assert!(
            !held_unproven_at(&build, Some(CLOCK)),
            "every sibling's release took its mark"
        );
    }
    let removing = first_exec_removal(&build).unwrap();
    let started = Instant::now();
    assert!(matches!(
        hold_first_exec_at(&bins[0], Some(CLOCK)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    assert!(
        started.elapsed() < HANG,
        "CONTROL: a removal is refused at once"
    );
    drop(removing);
}
