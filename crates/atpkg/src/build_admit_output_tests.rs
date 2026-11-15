// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_capacity::{Admit, Fixed, Ledger, Need, Pace, Reading};
use std::io::Read as _;
use std::path::PathBuf;

const WORLD: &str = "ATPKG_CAPTURE_TEST_WORLD";
const BYTES: usize = 256 << 10;

fn machine() -> Fixed {
    Fixed::new(Reading::of_gib(32, 28, 8))
}

fn seat<'a>(root: &Path, machine: &'a Fixed) -> Seat<'a> {
    Seat {
        ledger: Some(Ledger::at(root.join("ledger"))),
        machine,
        own: Some(root.join("build-memory.json")),
        verify: None,
        mode: Admit::Wait,
        pace: Pace::default(),
        pid: std::process::id(),
        marker: None,
    }
}

fn actor(root: &Path) -> Command {
    let mut command = aterm_uds::spawnfd::command(aterm_tempfile::bin_exe("atpkg-status-fixture"));
    command
        .arg("__captured_compiler")
        .arg("--jobs=64")
        .env(WORLD, root)
        .current_dir(root);
    command
}

fn claimed(root: &Path, seat: &Seat<'_>) -> (Build, Held) {
    let command = actor(root);
    let args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_str().unwrap().to_owned())
        .collect();
    let build = classify(&args, Tool::Tippy).unwrap();
    match admit(&build, root, seat, &mut |_| {}) {
        Admission::Claimed(held) => (build, *held),
        other => panic!("expected a private claim, got {other:?}"),
    }
}

#[test]
fn admission_refuses_before_the_compiler_has_any_side_effect() {
    for refuse in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let machine = machine();
        let seat = seat(root.path(), &machine);
        if refuse {
            std::fs::write(root.path().join("ledger"), b"not a directory").unwrap();
        }
        // Tippy's explicit tool identity classifies the compiler adapter's command, whose
        // accepted --jobs option must remain under the admitted width.
        let result = output_at(
            &mut actor(root.path()),
            Tool::Tippy,
            root.path(),
            &seat,
            &mut |_| {},
        );
        if refuse {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            assert!(!root.path().join("started").exists());
        } else {
            let output = result.unwrap();
            assert!(output.status.success());
            assert!(root.path().join("started").exists());
        }
    }
}

#[test]
fn a_unit_graph_probe_runs_without_admission_or_a_memory_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let mut seat = seat(root.path(), &machine);
    seat.ledger = None;
    let mut command =
        aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/bin/echo")));
    command.args(["--unverified", "bench", "--unit-graph"]);
    let output = output_at(&mut command, Tool::Targo, root.path(), &seat, &mut |_| {}).unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"--unverified bench --unit-graph\n");
    assert!(!root.path().join("ledger").exists());
    assert!(!root.path().join("build-memory.json").exists());
}

struct ReleaseGate(PathBuf);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"go");
    }
}

#[test]
fn captured_compilation_holds_coverage_until_reap_and_drains_both_large_streams() {
    let root = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let seat = seat(root.path(), &machine);
    std::fs::write(root.path().join("gated"), b"").unwrap();
    let observer = std::thread::scope(|scope| {
        let observed = scope.spawn(|| {
            let _release = ReleaseGate(root.path().join("go"));
            let pid = aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the compiler started",
                || {
                    std::fs::read_to_string(root.path().join("started"))
                        .ok()?
                        .parse::<u32>()
                        .ok()
                },
            );
            let rows = seat.ledger.as_ref().unwrap().list().unwrap();
            assert_eq!(rows.len(), 1);
            let held = &rows[0];
            assert!(held.need.covers);
            assert_eq!(
                std::fs::read_to_string(root.path().join("cover")).unwrap(),
                held.cover_marker()
            );
            pid
        });
        let mut command = actor(root.path());
        command.stdin(aterm_uds::spawnfd::Stdio::piped());
        let output = output_at(&mut command, Tool::Tippy, root.path(), &seat, &mut |_| {}).unwrap();
        assert!(output.status.success(), "{:?}", output.status);
        assert!(
            output
                .stdout
                .windows(BYTES)
                .any(|bytes| bytes.iter().all(|&byte| byte == b'o'))
        );
        assert!(
            output
                .stderr
                .windows(BYTES)
                .any(|bytes| bytes.iter().all(|&byte| byte == b'e'))
        );
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("{\"reason\":\"build-finished\",\"success\":true}")
        );
        assert_eq!(std::fs::read(root.path().join("input")).unwrap(), b"");
        assert_eq!(
            std::fs::read_to_string(root.path().join("jobs"))
                .unwrap()
                .trim(),
            "8"
        );
        observed.join().unwrap()
    });
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
    assert!(root.path().join("build-memory.json").is_file());
    // The supervisor consumed this exact root's exit; no live or unreaped child remains.
    let mut status = 0;
    // SAFETY: WNOHANG only asks for this recorded child's exit and writes `status`.
    let waited =
        unsafe { libc::waitpid(i32::try_from(observer).unwrap(), &mut status, libc::WNOHANG) };
    assert_eq!(waited, -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn existing_cover_and_own_claims_are_not_duplicated_or_released() {
    for cover in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let machine = machine();
        let mut seat = seat(root.path(), &machine);
        let need = Need::new("the caller's retained work").mem(1 << 30);
        let need = if cover { need.covers() } else { need };
        let parent = aterm_capacity::admit_with(
            seat.ledger.as_ref().unwrap(),
            &machine,
            need,
            Admit::Wait,
            Pace::default(),
            &mut |_| {},
        )
        .unwrap();
        let marker = parent.record().cover_marker();
        seat.marker = cover.then(|| marker.clone());
        let mut command = actor(root.path());
        command.env("CAPTURE_PREPARED_VALUE", "kept");
        if cover {
            command.env(aterm_capacity::COVER_ENV, &marker);
        }
        let output = output_at(&mut command, Tool::Tippy, root.path(), &seat, &mut |_| {}).unwrap();
        assert!(output.status.success());
        assert_eq!(seat.ledger.as_ref().unwrap().list().unwrap().len(), 1);
        assert!(!root.path().join("build-memory.json").exists());
        if cover {
            assert_eq!(
                std::fs::read_to_string(root.path().join("cover")).unwrap(),
                marker
            );
        }
        drop(parent);
        assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
    }
}

#[test]
fn spawn_failure_releases_the_unborn_claim_and_joins_waiting_readers() {
    let root = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let seat = seat(root.path(), &machine);
    let mut command = aterm_uds::spawnfd::command(root.path().join("missing-compiler"));
    command.args(["--unverified", "bench", "--no-run"]);
    assert!(output_at(&mut command, Tool::Targo, root.path(), &seat, &mut |_| {}).is_err());
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
    assert!(!root.path().join("started").exists());
}

#[test]
fn reader_error_or_panic_still_joins_both_streams_and_settles_the_actual_root() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for panic in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let machine = machine();
        let seat = seat(root.path(), &machine);
        let (compiler, held) = claimed(root.path(), &seat);
        let stderr_joined = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&stderr_joined);
        let stdout = Drain::with_reader("faulted-stdout", move |mut pipe: ChildStdout| {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes)?;
            assert!(!bytes.is_empty());
            if panic {
                panic!("injected reader unwind after draining");
            }
            Err(io::Error::other("injected read failure after draining"))
        })
        .unwrap();
        let stderr = Drain::with_reader("observed-stderr", move |mut pipe: ChildStderr| {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes)?;
            seen.store(true, Ordering::SeqCst);
            Ok(bytes)
        })
        .unwrap();
        let result = capture_with(
            Some(held),
            &mut actor(root.path()),
            &mut |_| {},
            stdout,
            stderr,
            &compiler,
        );
        assert!(result.is_err());
        assert!(stderr_joined.load(Ordering::SeqCst));
        assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
        assert!(root.path().join("build-memory.json").is_file());
    }
}

#[test]
fn explicit_cargo_jobs_use_the_admitted_limit_without_program_reclassification() {
    for captured in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let machine = machine();
        let seat = seat(root.path(), &machine);
        let mut command =
            aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(Path::new("/usr/bin/true")));
        command.args(["build", "--jobs=64"]);
        let status = if captured {
            output_at(&mut command, Tool::Cargo, root.path(), &seat, &mut |_| {})
                .unwrap()
                .status
        } else {
            // Negative control: ordinary arbitrary-command supervision still infers from
            // the program; Cargo-like words do not redefine /usr/bin/true as a compiler.
            let args: Vec<String> = command
                .get_args()
                .map(|arg| arg.to_str().unwrap().to_owned())
                .collect();
            let build = classify(&args, Tool::Cargo).unwrap();
            let Admission::Claimed(held) = admit(&build, root.path(), &seat, &mut |_| {}) else {
                panic!("expected a private claim");
            };
            super::super::supervise(Some(*held), &mut command, &mut |_| {}).unwrap()
        };
        assert!(status.success());
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                std::ffi::OsStr::new("build"),
                std::ffi::OsStr::new(if captured { "--jobs=8" } else { "--jobs=64" })
            ]
        );
        assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
    }
}
