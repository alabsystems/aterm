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
    let generation = Generation::parse("11111111111111111111111111111111").unwrap();
    let other = Generation::parse("22222222222222222222222222222222").unwrap();
    let label = label_for(world, 42, &generation);
    assert!(checked_owned_label(world, 42, &generation, &label).is_ok());
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
        assert!(checked_owned_label(dir, owner, &generation, label).is_err());
        // Refusal happens before filesystem or launchd access, even for a nonexistent
        // path; an arbitrary label never supplies permission to remove a job.
        assert!(remove_owned_for_qualification(dir, owner, &generation, label).is_err());
    }
    assert!(checked_owned_label(world, 42, &other, &label).is_err());
    assert!(remove_owned_for_qualification(world, 42, &other, &label).is_err());
}

#[test]
fn job_cleanup_refuses_a_substituted_generation_before_native_access() {
    let world = PathBuf::from("/private/tmp/.ubd/at_original.noindex");
    let generation = Generation::parse("11111111111111111111111111111111").unwrap();
    let foreign = Generation::parse("22222222222222222222222222222222").unwrap();
    let job = Job {
        label: label_for(&world, 42, &foreign),
        generation,
        world,
        owner: 42,
        pid: 43,
        held: None,
    };
    let error = job
        .remove_until(Instant::now() + Duration::from_secs(60))
        .expect_err("a foreign generation cannot be removed");
    assert_eq!(error.kind, cleanup::CleanupFailure::Unknown);
}

#[test]
fn generations_have_disjoint_state_and_cannot_rearm_an_existing_launch() {
    let world = aterm_tempfile::tempdir().unwrap();
    let first = Generation::fresh().unwrap();
    let second = Generation::fresh().unwrap();
    assert_ne!(first, second);
    assert_ne!(
        label_for(world.path(), 42, &first),
        label_for(world.path(), 42, &second)
    );
    let dir = first.create_directory(world.path()).unwrap();
    let marker = dir.join("already-ran");
    std::fs::write(&marker, b"first launch").unwrap();
    assert!(first.create_directory(world.path()).is_err());
    assert_eq!(std::fs::read(&marker).unwrap(), b"first launch");
    let next = second.create_directory(world.path()).unwrap();
    assert_ne!(dir, next);
    for name in [ENV_FILE, LIFELINE, PID_FILE, PLIST, guard::SOCKET] {
        assert_ne!(dir.join(name), next.join(name));
    }
    for invalid in [
        "",
        "../a",
        "1111111111111111111111111111111G",
        "111111111111111111111111111111111",
    ] {
        assert!(Generation::parse(invalid).is_err());
    }
}

#[test]
fn a_generation_guard_binds_at_the_native_socket_path_boundary() {
    use std::os::unix::ffi::OsStrExt as _;
    let root = aterm_uds::test_binary::short_root().expect("Cargo test short root");
    let generation = Generation::fresh().unwrap();
    // macOS sockaddr_un has 104 bytes, including the final NUL.
    let suffix = 1 + 33 + 1 + guard::SOCKET.len();
    let fill = 103_usize
        .checked_sub(root.as_os_str().as_bytes().len() + 1 + suffix)
        .expect("test root leaves room for a world and generation");
    assert!(fill > 0);
    let name: String = generation.word().chars().cycle().take(fill).collect();
    let world = root.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&world)
        .unwrap();
    let dir = generation.create_directory(&world).unwrap();
    let socket = dir.join(guard::SOCKET);
    assert_eq!(socket.as_os_str().as_bytes().len(), 103);
    let listener = aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&socket)).unwrap();
    drop(listener);
    let too_long = dir.join("gg");
    assert!(aterm_uds::kept_out(|| aterm_uds::CtlListener::bind(&too_long)).is_err());
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
