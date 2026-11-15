// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HANG: Duration = Duration::from_secs(60);

#[test]
fn exact_binary_streams_keep_failed_status_and_each_overflow_is_a_refusal() {
    let world = aterm_tempfile::tempdir().unwrap();
    let stdout = world.path().join("stdout");
    let stderr = world.path().join("stderr");
    let sh = stand_in(std::path::Path::new("/bin/sh"));
    let cat = stand_in(std::path::Path::new("/bin/cat"));
    let limit = 64 * 1024;
    for (out_len, err_len) in [(limit, limit), (limit + 1, 0), (0, limit + 1)] {
        let out_bytes = vec![0x80; out_len];
        let err_bytes = vec![0xfe; err_len];
        std::fs::write(&stdout, &out_bytes).unwrap();
        std::fs::write(&stderr, &err_bytes).unwrap();
        let mut command = crate::spawnfd::command(&sh);
        command
            .args(["-c", "\"$1\" \"$2\"; \"$1\" \"$3\" >&2; exit 7", "sh"])
            .arg(&cat)
            .arg(&stdout)
            .arg(&stderr);
        let output = output_while_process_progressing_complete_with_limit(
            &mut command,
            HANG,
            "complete binary streams",
            limit,
        );
        if out_len <= limit && err_len <= limit {
            let output = output.unwrap();
            assert_eq!(output.status.code(), Some(7));
            assert_eq!(output.stdout, out_bytes);
            assert_eq!(output.stderr, err_bytes);
        } else if out_len > limit {
            assert!(
                matches!(output, Err(CompleteProgressError::Complete(CompleteOutputError::StdoutTooLarge { limit: found })) if found == limit)
            );
        } else {
            assert!(
                matches!(output, Err(CompleteProgressError::Complete(CompleteOutputError::Run(RunError::Capture(error)))) if error.kind() == io::ErrorKind::InvalidData)
            );
        }
    }
    let mut absent = crate::spawnfd::command(world.path().join("not-an-executable"));
    assert!(matches!(
        output_while_process_progressing_complete_with_limit(
            &mut absent, HANG, "must refuse before dispatch", MAX_COMPLETE_STDOUT_BYTES + 1,
        ),
        Err(CompleteProgressError::Complete(CompleteOutputError::Run(RunError::Spawn(error))))
            if error.kind() == io::ErrorKind::InvalidInput
    ));
}

#[test]
fn either_overflow_retires_a_producer_that_does_not_close_its_streams() {
    let sh = stand_in(std::path::Path::new("/bin/sh"));
    let sleep = stand_in(std::path::Path::new("/bin/sleep"));
    for stream in [ChildOutput::Stdout, ChildOutput::Stderr] {
        let script = match stream {
            ChildOutput::Stdout => "printf overflow; exec \"$1\" 600",
            ChildOutput::Stderr => "printf overflow >&2; exec \"$1\" 600",
        };
        let mut command = crate::spawnfd::command(&sh);
        command.args(["-c", script, "sh"]).arg(&sleep);
        let output = output_while_process_progressing_complete_with_limit(
            &mut command,
            HANG,
            "a live producer beyond its cap",
            1,
        );
        match stream {
            ChildOutput::Stdout => assert!(matches!(
                output,
                Err(CompleteProgressError::Complete(
                    CompleteOutputError::StdoutTooLarge { limit: 1 }
                ))
            )),
            ChildOutput::Stderr => assert!(
                matches!(output, Err(CompleteProgressError::Complete(CompleteOutputError::Run(RunError::Capture(error)))) if error.kind() == io::ErrorKind::InvalidData)
            ),
        }
    }
}

fn collector(
    command: &mut crate::spawnfd::Command,
    now: &mut dyn FnMut() -> Instant,
    work: &mut dyn FnMut(u32) -> Option<u64>,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Vec<u8>, String> {
    progress_complete_with(
        command,
        HANG,
        "complete streaming fixture".into(),
        OUTPUT_KEEP,
        now,
        work,
        Some(stopped),
    )
    .and_then(|output| {
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(
                CompleteOutputError::Run(RunError::Wait(io::Error::other(format!(
                    "fixture exited {}",
                    output.status
                ))))
                .into(),
            )
        }
    })
    .map_err(|error| format!("{error:?}"))
}

#[test]
fn progress_and_stop_observation_read_the_tree_once_a_tick() {
    super::tests::a_collector_holding_a_stop_reads_once_a_tick(
        "stream-complete-progress-stop-cadence",
        collector,
    );
}

#[test]
fn natural_exit_stops_process_tree_reads_before_retirement() {
    super::tests::a_collector_reads_no_reaped_tree("stream-complete-progress-reaped", collector);
}

#[test]
fn advancing_cpu_progress_keeps_a_quiet_child_past_its_window() {
    super::tests::a_collector_keeps_a_quiet_child_past_its_window(
        "stream-complete-progress-advancing",
        collector,
    );
}
