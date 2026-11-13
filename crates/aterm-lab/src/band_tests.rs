// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The lifeline as [`band_main`] holds it, a non-blocking read end of a FIFO, beside the
/// launcher's held read-write end, as [`Job::start`] opens them.
fn lifeline() -> (aterm_tempfile::TempDir, File, File) {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let fifo = dir.path().join(LIFELINE);
    crate::sys::make_fifo(&fifo).expect("mkfifo");
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .expect("the held end");
    let job = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .expect("the job's end");
    (dir, held, job)
}

/// The go is a byte; a launcher gone without it is the end of file; one that holds its writer
/// and writes nothing leaves the job waiting until the bound.
#[test]
fn the_job_waits_for_its_go_and_sees_a_launcher_gone_without_it() {
    let (_dir, mut held, job) = lifeline();
    held.write_all(&[GO]).expect("the go");
    assert!(
        matches!(await_go(&job, crate::instance::BOOT), Go::Given),
        "the go"
    );

    let (_dir, held, job) = lifeline();
    drop(held);
    assert!(
        matches!(await_go(&job, crate::instance::BOOT), Go::Gone),
        "a launcher gone"
    );

    let (_dir, _held, job) = lifeline();
    assert!(
        matches!(await_go(&job, Duration::ZERO), Go::Late),
        "a launcher that holds its writer and writes nothing"
    );
}

#[test]
fn an_instance_defaults_to_the_app_band_and_takes_either_band_by_name() {
    assert_eq!(Band::pick(None).ok(), Some(Band::App));
    assert_eq!(Band::pick(Some("agent")).ok(), Some(Band::Agent));
    assert_eq!(Band::pick(Some("app")).ok(), Some(Band::App));
    assert!(Band::pick(Some("own")).is_err());
}

#[test]
fn qualification_cleanup_binds_the_original_owner_and_world() {
    let world = Path::new("/private/tmp/.ubd/at_original.noindex");
    let label = label_for(world, 42);
    assert!(checked_owned_label(world, 42, &label).is_ok());
    for (dir, owner, label) in [
        (world, 43, label.as_str()),
        (
            Path::new("/private/tmp/.ubd/at_other.noindex"),
            42,
            label.as_str(),
        ),
        (world, 42, "other-job"),
        (world, 0, label.as_str()),
    ] {
        assert!(checked_owned_label(dir, owner, label).is_err());
        // Refusal happens before filesystem or launchd access, even for a nonexistent
        // path; an arbitrary label never supplies permission to remove a job.
        assert!(remove_owned_for_qualification(dir, owner, label).is_err());
    }
}

#[test]
fn missing_registration_is_exact_to_the_job_domain_and_native_result() {
    use std::os::unix::process::ExitStatusExt as _;
    let label = "aterm-lab.instance.original.42";
    let missing =
        format!("Bad request.\nCould not find service \"{label}\" in domain for user gui: 501\n");
    let output = |code: i32, stdout: &[u8], stderr: &[u8]| std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    };
    assert_eq!(
        decode_registration(&output(113, b"", missing.as_bytes()), label, 501).unwrap(),
        Registration::Absent
    );
    assert_eq!(
        decode_registration(&output(0, b"job = {\n\tpid = 42\n}\n", b""), label, 501).unwrap(),
        Registration::Present
    );
    for (observed, job, uid) in [
        (output(1, b"", missing.as_bytes()), label, 501),
        (output(113, b"foreign", missing.as_bytes()), label, 501),
        (output(113, b"", b"permission denied\n"), label, 501),
        (output(113, b"", missing.as_bytes()), "other-job", 501),
        (output(113, b"", missing.as_bytes()), label, 502),
        (
            output(113, b"", format!("{missing}foreign\n").as_bytes()),
            label,
            501,
        ),
    ] {
        assert!(decode_registration(&observed, job, uid).is_err());
    }
}

#[test]
fn a_guard_is_injected_only_into_the_original_keeper_prefix() {
    let own_program = paths().exe.to_string_lossy().into_owned();
    let line = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        keeper_line(&line(&[&own_program, "keeper", "--", "gui", "--window"])).unwrap(),
        2
    );
    assert_eq!(
        keeper_line(&line(&[
            &own_program,
            "keeper",
            "--disclaim",
            "--",
            "gui",
            "--window"
        ]))
        .unwrap(),
        3
    );
    for args in [
        vec!["--", "keeper"],
        vec!["other-program", "keeper", "--", "gui"],
        vec!["lab", "other", "--", "gui"],
        vec!["lab", "keeper", "--"],
        vec!["lab", "keeper", "--lab-window-guard", "4", "--", "gui"],
    ] {
        assert!(keeper_line(&line(&args)).is_err());
    }
}
