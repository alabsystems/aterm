// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual native supervisor over explicitly admitted private cohort work.

use super::*;
use aterm_uds::spawnfd::{self, Stdio};

fn admitted(dir: &Path, selected: &Seat<'_>) -> Held {
    match admit_cohort(&test_build(), dir, selected, &mut |_| {}) {
        Admission::Claimed(held) => *held,
        other => panic!("private native admission refused: {other:?}"),
    }
}

#[test]
fn native_cohort_is_bound_before_the_supervisor_waits() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let selected = cohort_seat(dir.path(), &machine);
    let ledger = selected.ledger.as_ref().unwrap();
    let held = admitted(dir.path(), &selected);
    let mut command = spawnfd::command("/bin/cat");
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let status = supervise_with(
        Some(held),
        &mut command,
        &|| 0,
        &mut |_| {},
        || None,
        |child, _| {
            let input = aterm_uds::deadline::cat_echoes(child);
            let rows = ledger.list().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].accounting, aterm_capacity::Accounting::Whole);
            // SAFETY: this is the actual owned unreaped child, so its pid is pinned.
            assert_eq!(
                unsafe { libc::getpgid(i32::try_from(child.id()).unwrap()) },
                i32::try_from(child.id()).unwrap()
            );
            drop(input);
        },
    )
    .unwrap();
    assert!(status.success());
    assert!(
        ledger.list().unwrap().is_empty(),
        "actual group drain settles custody"
    );
}

#[test]
fn native_cohort_missing_target_cancels_its_actual_unused_arm() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let selected = cohort_seat(dir.path(), &machine);
    let held = admitted(dir.path(), &selected);
    let error = supervise(
        Some(held),
        &mut spawnfd::command(dir.path().join("missing")),
        &|| 0,
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(selected.ledger.as_ref().unwrap().list().unwrap().is_empty());
}

#[test]
fn native_cohort_cannot_transfer_to_the_ordinary_detached_holder() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let selected = cohort_seat(dir.path(), &machine);
    let held = admitted(dir.path(), &selected);
    assert!(
        hand_over(&held)
            .unwrap_err()
            .contains("native cohort custody")
    );
    drop(held);
    assert!(selected.ledger.as_ref().unwrap().list().unwrap().is_empty());
}

/// Exercise the native controller with the actual toolchain running this suite,
/// rather than inferring build integration from a process stand-in alone.
#[test]
fn native_cohort_supervises_a_real_targo_compile_and_emitted_program() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let selected = cohort_seat(dir.path(), &machine);
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname='native_cohort_probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/main.rs"),
        "fn main() { assert_eq!((0..10).sum::<u64>(), 45); }\n",
    )
    .unwrap();
    let target = dir.path().join("target.noindex");
    let args = vec![
        "--unverified".into(),
        "build".into(),
        "--offline".into(),
        "--target-dir".into(),
        target.to_str().unwrap().into(),
        "--config".into(),
        format!(
            "build.build-dir='{}'",
            dir.path().join("build.noindex").display()
        ),
    ];
    let build = classify(&args, Tool::Targo).unwrap();
    let Admission::Claimed(held) = admit_cohort(&build, dir.path(), &selected, &mut |_| {}) else {
        panic!("native compile admission refused")
    };
    let mut command =
        spawnfd::transparent(std::env::var_os("CARGO").expect("the suite's actual targo"))
            .into_command();
    command
        .args(&args)
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // Drain diagnostics through an actual reader while the native supervisor owns
    // the compiler; compiler output must not fill a pipe and create a false hang.
    let mut diagnostics = None;
    let status = supervise_with(
        Some(*held),
        &mut command,
        &|| 0,
        &mut |_| {},
        || None,
        |child, _| {
            let mut stderr = child.stderr.take().unwrap();
            diagnostics = Some(std::thread::spawn(move || {
                use std::io::Read as _;
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).unwrap();
                bytes
            }));
        },
    )
    .unwrap();
    let diagnostics = diagnostics.unwrap().join().unwrap();
    assert!(
        status.success(),
        "{}",
        String::from_utf8_lossy(&diagnostics)
    );
    assert!(selected.ledger.as_ref().unwrap().list().unwrap().is_empty());
    let mut program = spawnfd::command(target.join("debug/native_cohort_probe"))
        .spawn()
        .unwrap();
    assert!(program.wait().unwrap().success());
}

#[test]
fn native_cohort_normal_root_exit_keeps_a_live_descendants_entire_budget() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let selected = cohort_seat(dir.path(), &machine);
    let ledger = selected.ledger.as_ref().unwrap();
    let held = admitted(dir.path(), &selected);
    let expected = held.claim.record().need.mem;
    let reference = held.claim.cohort_closure_ref().unwrap();
    let mut input = None;
    let mut said = Vec::new();
    // The root starts its descendant and exits only once this test holds its input: a root
    // that exited during the launch is reaped before `before_wait`, whose input then closes
    // with the root and drains the group for real (8 copies at once, 2026-11-04: 47 of 48).
    let mut command = spawnfd::command("/bin/sh");
    command
        .args(["-c", "exec 3<&0; read go; /bin/cat <&3 & exit 0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = supervise_with(
        Some(held),
        &mut command,
        &|| 1,
        &mut |line| said.push(line.to_owned()),
        || None,
        |child, _| {
            let mut stdin = child.stdin.take().expect("the root's input");
            std::io::Write::write_all(&mut stdin, b"go\n").expect("the root's go");
            input = Some(stdin);
        },
    )
    .unwrap();
    assert!(input.is_some(), "the root waited for its go");
    let rows = ledger.list().unwrap();
    let recorded_incomplete_peak = selected.own.as_ref().unwrap().exists();
    drop(input);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "actual cohort drains",
        || {
            (ledger.reap_cohort_claim(&reference).unwrap()
                == aterm_capacity::CohortClaimState::Closed)
                .then_some(())
        },
    );
    assert!(status.success(), "normal exit preserves the build's status");
    assert_eq!(rows.len(), 1, "a descendant still holds its group");
    assert_eq!(rows[0].need.mem, expected);
    assert_eq!(rows[0].accounting, aterm_capacity::Accounting::Whole);
    assert!(
        !recorded_incomplete_peak,
        "partial memory is not a successful whole-build sample"
    );
    assert!(
        said.iter()
            .any(|line| line.contains("reservation retained")),
        "{said:?}"
    );
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn native_cohort_admission_refuses_legacy_and_unqualified_nested_fallbacks() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let ordinary = seat(dir.path(), &machine, std::process::id());
    assert!(matches!(
        admit_cohort(&test_build(), dir.path(), &ordinary, &mut |_| {}),
        Admission::Refused(_)
    ));
    assert!(ordinary.ledger.as_ref().unwrap().list().unwrap().is_empty());
    for covers in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut selected = cohort_seat(dir.path(), &machine);
        machine.set_procs(Some(Procs::of(vec![proc(std::process::id(), 1)])));
        let mut need = Need::new("existing private root").mem(GIB);
        if covers {
            need = need.covers();
        }
        let claim = aterm_capacity::admit_with(
            selected.ledger.as_ref().unwrap(),
            &machine,
            need,
            Admit::Now,
            Pace::default(),
            &mut |_| {},
        )
        .unwrap();
        selected.marker = Some(claim.record().cover_marker());
        assert!(matches!(
            admit_cohort(&test_build(), dir.path(), &selected, &mut |_| {}),
            Admission::Refused(_)
        ));
        assert_eq!(selected.ledger.as_ref().unwrap().list().unwrap().len(), 1);
    }
}

const COHORT_WORLD: &str = "ATPKG_NATIVE_COHORT_WORLD";

#[test]
fn native_cohort_controller_actor_if_requested() {
    let Some(world) = std::env::var_os(COHORT_WORLD) else {
        return;
    };
    let world = PathBuf::from(world);
    let machine = machine();
    let selected = cohort_seat(&world, &machine);
    let held = admitted(&world, &selected);
    std::fs::write(
        world.join("fence"),
        selected
            .ledger
            .as_ref()
            .unwrap()
            .dir()
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    std::fs::write(
        world.join("reference"),
        held.claim
            .cohort_closure_ref()
            .unwrap()
            .to_json()
            .to_string(),
    )
    .unwrap();
    // Its stdin is the parent's still-open pipe. Killing this actor must not end
    // the actual compiler stand-in or release its durable reservation.
    let mut command = spawnfd::transparent("/bin/cat").into_command();
    let status = supervise_with(
        Some(held),
        &mut command,
        &|| 0,
        &mut |_| {},
        || None,
        |child, _| std::fs::write(world.join("bound"), child.id().to_string()).unwrap(),
    )
    .unwrap();
    assert!(status.success());
}

#[test]
fn native_cohort_survives_its_supervisors_sigkill_until_actual_group_drain() {
    struct Actor(spawnfd::Child);
    impl Drop for Actor {
        fn drop(&mut self) {
            self.0.stdin.take();
            if matches!(self.0.exited(), Ok(false)) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    let world = aterm_tempfile::tempdir().unwrap();
    let mut actor = Actor(
        spawnfd::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "build_admit::tests::cohort::native_cohort_controller_actor_if_requested",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(COHORT_WORLD, world.path())
            .setsid(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "native cohort bound", || {
        world.path().join("bound").exists().then_some(())
    });
    let ledger = Ledger::at(PathBuf::from(
        std::fs::read_to_string(world.path().join("fence")).unwrap(),
    ));
    let reference = aterm_capacity::CohortClosureRef::from_json(
        &aterm_json::from_str(&std::fs::read_to_string(world.path().join("reference")).unwrap())
            .unwrap(),
    )
    .unwrap();
    let before = ledger.list().unwrap();
    // Child::wait closes its stdin. Keep this writer independently of the actor
    // before reaping it, so EOF cannot masquerade as surviving controller death.
    let input = actor.0.stdin.take().unwrap();
    actor.0.kill().unwrap();
    actor.0.wait().unwrap();
    // The parent retains stdin until AFTER these reads: only durable custody,
    // never a guard in this owner, protects the compiler that still runs.
    let after = ledger.list().unwrap();
    let state = ledger.reap_cohort_claim(&reference).unwrap();
    drop(input);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "orphaned native group drains",
        || {
            (ledger.reap_cohort_claim(&reference).unwrap()
                == aterm_capacity::CohortClaimState::Closed)
                .then_some(())
        },
    );
    assert_eq!(before.len(), 1);
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].need, before[0].need,
        "the entire original reservation survives"
    );
    assert_eq!(after[0].accounting, aterm_capacity::Accounting::Whole);
    assert_eq!(state, aterm_capacity::CohortClaimState::Retained);
    assert!(ledger.list().unwrap().is_empty());
}
