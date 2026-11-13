// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn text_reads_the_header_then_the_rows() {
    let ran = Ran {
        code: Some(0),
        stdout: "global  1 of 16 cores busy\n".to_string(),
        stderr: "aterm-ctl: OK 1 sys v=1 view=overview\n".to_string(),
    };
    assert!(ran.ok());
    assert_eq!(
        ran.text(),
        "aterm-ctl: OK 1 sys v=1 view=overview\nglobal  1 of 16 cores busy"
    );
    let failed = Ran {
        code: Some(1),
        ..ran
    };
    assert!(!failed.ok());
}

#[cfg(windows)]
fn system32(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join(name)
}

#[cfg(windows)]
#[test]
fn run_captures_a_child_in_the_place_it_was_given() {
    let env = vec![
        (
            "SystemRoot".to_string(),
            std::env::var("SystemRoot").expect("SystemRoot"),
        ),
        ("WINSHIP_PROBE".to_string(), "only-this".to_string()),
    ];
    let place = Place {
        env: Some(&env),
        cwd: None,
    };
    let ran = run(
        &system32("cmd.exe"),
        &["/d", "/c", "set"],
        place,
        Duration::from_secs(60),
    )
    .expect("cmd runs");
    assert!(ran.ok(), "{ran:?}");
    assert!(
        ran.stdout.contains("WINSHIP_PROBE=only-this"),
        "{}",
        ran.stdout
    );
    // Negative control: the caller's own environment did not reach the child.
    assert!(
        !ran.stdout.to_ascii_uppercase().contains("USERPROFILE="),
        "{}",
        ran.stdout
    );
}

/// The job ends a child and the process that child started at once, by the
/// handle this process holds, and both were running until then.
///
/// The grandchild is watched through a pipe, never by a pid: `cmd` starts
/// `ping` as its first act and both hold the pipe's write end, so the first
/// bytes read say `ping` runs and the end of file says no holder is left.
#[cfg(windows)]
#[test]
fn job_ends_the_child_and_what_it_started() {
    use std::io::Read as _;

    let mut cmd = command(
        &system32("cmd.exe"),
        &["/d", "/c", "ping -n 600 127.0.0.1"],
        Place::default(),
    );
    cmd.stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let (mut child, job) = Job::start(&mut cmd).expect("cmd starts in a job");
    // The command keeps no copy of the pipe's write end once it is dropped.
    drop(cmd);
    let mut pipe = child.stdout.take().expect("the pipe");

    let (said, hears) = std::sync::mpsc::channel::<Result<usize, String>>();
    let reader = std::thread::spawn(move || {
        let mut first = [0u8; 64];
        let _ = said.send(pipe.read(&mut first).map_err(|e| e.to_string()));
        let mut rest = Vec::new();
        let _ = said.send(pipe.read_to_end(&mut rest).map_err(|e| e.to_string()));
    });
    let hang = Duration::from_secs(60);
    let first = hears.recv_timeout(hang).expect("ping says something");
    assert!(matches!(first, Ok(n) if n > 0), "ping wrote: {first:?}");
    // Negative controls: nothing has ended them yet, and the grandchild is in the job.
    assert_eq!(child.try_wait().expect("try_wait").map(|s| s.code()), None);
    assert!(
        job.active().expect("the count") >= 2,
        "cmd and the ping it started are both in the job"
    );

    assert!(job.end(hang).expect("the job ends"), "the job emptied");
    assert_eq!(job.active().expect("the count"), 0);
    let status = child.wait().expect("cmd is reaped");
    assert_eq!(status.code(), Some(1), "ended by the job's exit code");
    // The end of file: ping, the last holder of the write end, is gone too.
    let rest = hears
        .recv_timeout(hang)
        .expect("the pipe closes: the grandchild ended");
    assert!(rest.is_ok(), "{rest:?}");
    reader.join().expect("the reader ends");
}
