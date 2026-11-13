// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const ROLE: &str = "ATERM_OWNED_EXCHANGE_FIXTURE_ROLE";
const PROOF: &str = "ATERM_OWNED_EXCHANGE_FIXTURE_PROOF";

fn fixture(role: &str, proof: &std::path::Path) -> Command {
    let mut command = crate::spawnfd::command(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            "deadline::input_windows::tests::owned_exchange_fixture",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, role)
        .env(PROOF, proof);
    command
}

fn witness(proof: &std::path::Path) -> Option<(i32, crate::process::Birth)> {
    let text = std::fs::read_to_string(proof).ok()?;
    let (pid, birth) = text.trim().split_once(' ')?;
    Some((
        pid.parse().ok()?,
        crate::process::Birth(birth.parse().ok()?),
    ))
}

#[test]
fn owned_exchange_fixture() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let proof = std::path::PathBuf::from(std::env::var_os(PROOF).expect("proof path"));
    if role == "parent" {
        let mut command = fixture("retainer", &proof);
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[expect(
            clippy::zombie_processes,
            reason = "the parent role exits with its descendant alive on purpose: the test that \
                      spawned it proves the exchange retires that descendant"
        )]
        let retainer = command.spawn().expect("owned descendant");
        let by = Instant::now() + CLEANUP;
        loop {
            if let Some((pid, birth)) = witness(&proof) {
                assert_eq!(u32::try_from(pid).unwrap(), retainer.id());
                assert_eq!(crate::process::read_birth(pid), Some(birth));
                std::process::exit(67);
            }
            assert!(Instant::now() < by, "descendant execution proof");
            std::thread::sleep(STEP);
        }
    }
    assert!(matches!(role.as_str(), "retainer" | "nonreader"));
    let pid = i32::try_from(std::process::id()).unwrap();
    let birth = crate::process::read_birth(pid).expect("this live fixture's birth");
    println!("owned-fixture-output:{pid}");
    eprintln!("owned-fixture-diagnostic:{pid}");
    std::io::stdout().flush().expect("stdout proof");
    std::io::stderr().flush().expect("stderr proof");
    std::fs::write(proof, format!("{pid} {}\n", birth.0)).expect("execution proof");
    loop {
        std::thread::park_timeout(CLEANUP);
    }
}

#[test]
fn direct_exit_retires_the_witnessed_descendant_holding_all_three_pipes() {
    let dir = aterm_tempfile::tempdir().expect("owned proof directory");
    let proof = dir.path().join("descendant");
    let output = output_with_input_until(
        &mut fixture("parent", &proof),
        Secret::new(&[b'x'; 256 * 1024]),
        Instant::now() + CLEANUP,
        16 * 1024,
    )
    .expect("the direct exit cannot wait for its descendant's pipe handles");
    let (pid, birth) = witness(&proof).expect("descendant proved execution");
    assert_eq!(output.status.code(), Some(67));
    assert!(String::from_utf8_lossy(&output.stdout).contains("owned-fixture-output:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("owned-fixture-diagnostic:"));
    assert_ne!(crate::process::read_birth(pid), Some(birth));
}

#[test]
fn a_witnessed_nonreading_child_is_retired_when_the_deadline_crosses() {
    let dir = aterm_tempfile::tempdir().expect("owned proof directory");
    let proof = dir.path().join("child");
    let path = proof.clone();
    let began = Instant::now();
    let deadline = began + CLEANUP;
    super::super::set_deadline_clock_for_test(Some(Box::new(move || {
        if witness(&path).is_some() || began.elapsed() >= CLEANUP {
            deadline + Duration::from_secs(1)
        } else {
            began
        }
    })));
    struct Clock;
    impl Drop for Clock {
        fn drop(&mut self) {
            super::super::set_deadline_clock_for_test(None);
        }
    }
    let clock = Clock;
    let error = output_with_input_until(
        &mut fixture("nonreader", &proof),
        Secret::new(&[b'x'; 256 * 1024]),
        deadline,
        16 * 1024,
    )
    .expect_err("a full stdin cannot block the deadline");
    drop(clock);
    let (pid, birth) = witness(&proof).expect("child ran before its deadline crossed");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_ne!(crate::process::read_birth(pid), Some(birth));
}

fn cmd(script: &str) -> Command {
    let root = std::env::var_os("SystemRoot").expect("Windows SystemRoot");
    let mut command =
        crate::spawnfd::command(std::path::Path::new(&root).join("System32").join("cmd.exe"));
    command.args(["/D", "/S", "/C", script]);
    command
}

/// The drain tells an empty pipe from a closed writer. Negative control: read through
/// std's `File::read`, which returns `Ok(0)` for both, the closed writer is taken for an
/// empty pipe, so the stream never ends and the closed-writer assertion fails.
#[test]
fn the_parent_pipe_does_not_wait_for_input_or_capacity() {
    let (reader, mut client) = pipe(false).expect("read pipe");
    let mut reader = Some(reader);
    let mut bytes = Vec::new();
    let moved = drain(&mut reader, &mut bytes, 16).expect("an empty nonblocking pipe");
    assert!(
        !moved && reader.is_some(),
        "an empty pipe is not the writer's end"
    );
    client.write_all(b"x").expect("the writer writes");
    let moved = drain(&mut reader, &mut bytes, 16).expect("a byte in the pipe");
    assert!(
        moved && reader.is_some(),
        "a byte read is progress, not the end"
    );
    assert_eq!(bytes, b"x");
    drop(client);
    let moved = drain(&mut reader, &mut bytes, 16).expect("a closed writer");
    assert!(
        moved && reader.is_none(),
        "a closed writer is the end of its stream"
    );

    let (mut writer, _client) = pipe(true).expect("write pipe");
    let block = [1_u8; 8192];
    let mut full = false;
    for _ in 0..128 {
        match writer.write(&block) {
            Ok(0) => {
                full = true;
                break;
            }
            Ok(_) => {}
            Err(error) if error.raw_os_error() == Some(ERROR_NO_DATA) => {
                full = true;
                break;
            }
            Err(error) => panic!("pipe write: {error}"),
        }
    }
    assert!(
        full,
        "the unread pipe must report backpressure without a blocking write"
    );
}

#[test]
fn an_expired_exchange_does_not_spawn_the_missing_program() {
    let mut command = crate::spawnfd::command("aterm-deliberately-missing-helper.exe");
    let error = output_with_input_until(
        &mut command,
        Secret::new(b"inert input"),
        Instant::now(),
        1024,
    )
    .expect_err("deadline before spawn");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn a_direct_exit_keeps_its_status_and_both_complete_streams() {
    let output = output_with_input_until(
        &mut cmd("echo output& echo diagnostic 1>&2& exit /B 67"),
        Secret::new(&[b'x'; 256 * 1024]),
        Instant::now() + CLEANUP,
        1024,
    )
    .expect("the child exits without consuming all input");
    assert_eq!(output.status.code(), Some(67));
    assert!(String::from_utf8_lossy(&output.stdout).contains("output"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("diagnostic"));
}

#[test]
fn either_complete_stream_refuses_overflow() {
    for script in ["echo too-long", "echo too-long 1>&2"] {
        let error = output_with_input_until(
            &mut cmd(script),
            Secret::new(b""),
            Instant::now() + CLEANUP,
            1,
        )
        .expect_err("complete output cannot be truncated into success");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
