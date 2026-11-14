// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_bounded_child_is_read_or_killed_at_its_cap() {
    let scratch = aterm_tempfile::TempDir::new().expect("owned reader setup scratch");
    let absent = scratch.path().join("not-a-program");
    for cap in [Some(Duration::from_secs(60)), None] {
        for refuse in [1, 2] {
            let mut starts = Vec::new();
            let refused = std::thread::scope(|scope| {
                let mut workers = Vec::new();
                let ran = run_with_readers(&absent, &[], cap, |which, work| {
                    starts.push(which.to_string());
                    if starts.len() == refuse {
                        return Err(io::Error::other("injected reader refusal"));
                    }
                    workers.push(
                        std::thread::Builder::new()
                            .name("bounded-reader-setup-control".to_string())
                            .spawn_scoped(scope, work)?,
                    );
                    Ok(())
                });
                for worker in workers {
                    assert!(worker.join().is_ok(), "the cancelled idle reader ended");
                }
                ran
            });
            assert_eq!(starts, ["stdout", "stderr"][..refuse]);
            assert!(
                refused.is_err_and(|e| e.contains("injected reader refusal")),
                "reader refusal, not the missing program's spawn"
            );
        }
    }
    let past = Instant::now();
    let (finished, output) = sync_channel(1);
    finished.send(b"ready".to_vec()).unwrap();
    assert_eq!(collect(output, Some(past), "stdout").unwrap(), "ready");
    let (held_open, output) = sync_channel(1);
    assert!(collect(output, Some(past), "stderr").is_err());
    drop(held_open);
    let (finished, output) = sync_channel(1);
    finished.send(b"uncut".to_vec()).unwrap();
    assert_eq!(collect(output, None, "stdout").unwrap(), "uncut");

    let ran = run_bounded(
        Path::new("/bin/sh"),
        &[
            "-c".to_string(),
            "echo out; echo err >&2; exit 3".to_string(),
        ],
        Some(Duration::from_secs(60)),
    )
    .expect("it answered");
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (Some(3), "out\n", "err\n")
    );
    let uncut = run_bounded(
        Path::new("/bin/sh"),
        &[
            "-c".to_string(),
            "printf uncut; printf diagnostic >&2".to_string(),
        ],
        None,
    )
    .expect("the uncut command answered on its own");
    assert_eq!(
        (uncut.code, uncut.stdout.as_str(), uncut.stderr.as_str()),
        (Some(0), "uncut", "diagnostic")
    );
    let started = Instant::now();
    let hung = run_bounded(
        Path::new("/bin/sleep"),
        &["600".to_string()],
        Some(Duration::from_millis(200)),
    );
    assert!(hung.is_err(), "{hung:?}");
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the cap ended it"
    );
    assert!(
        run_bounded(
            Path::new("/no/such/aterm"),
            &[],
            Some(Duration::from_secs(1))
        )
        .is_err()
    );
}

/// A COMMAND THE WINDOW RUNS OF ITS OWN NAMES NO TAB: its child goes without the
/// `ATERM_PARENT_SESSION_ID` the window may have inherited from the tab that started it, so a
/// menu's `aterm ctl --host … hold` names no requester (ATERM-KI-1109-3).
/// NEGATIVE CONTROL: a bare spawn command removes nothing, and the child keeps every argument.
#[test]
fn a_command_the_window_runs_carries_no_parent_session() {
    let removed = |command: &aterm_uds::spawnfd::Command| {
        command.get_envs().any(|(key, value)| {
            key == std::ffi::OsStr::new(aterm_types::domain::ENV_PARENT_SESSION_ID)
                && value.is_none()
        })
    };
    let args = [
        "ctl",
        "--host",
        "m9",
        "hold",
        "s-0123456789abcdef0123",
        "on",
    ]
    .map(str::to_string)
    .to_vec();
    let owned = owned_command(Path::new("aterm"), &args);
    assert!(
        removed(&owned),
        "the window's own child drops the tab's session"
    );
    assert_eq!(
        owned.get_args().collect::<Vec<_>>(),
        args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>()
    );
    assert!(
        !removed(&aterm_uds::spawnfd::command("aterm")),
        "CONTROL: a bare spawn command keeps it"
    );
}
