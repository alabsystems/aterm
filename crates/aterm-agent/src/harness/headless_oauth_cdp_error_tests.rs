// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The late write's test reads its pipe's readiness, which only unix has here.
#[cfg(unix)]
mod late_write {
    use super::*;
    use std::os::fd::AsFd as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// More than a pipe holds unread, so a command this long cannot finish going out until its
    /// reader reads.
    const UNREAD_FRAME: usize = 1024 * 1024;

    /// A stand-in browser that reads nothing until `go`. Once the first command starts to arrive,
    /// it makes `slept` say the host slept two hours, which ends a deadline that counts sleep on
    /// it after that write began and before it can finish. After `go` it answers each command
    /// with its id until its pipe ends, and returns each command's id and expression length.
    fn unread_until(
        mut commands: std::io::PipeReader,
        mut replies: std::io::PipeWriter,
        slept: Arc<AtomicU64>,
        go: Receiver<()>,
    ) -> std::thread::JoinHandle<Vec<(u64, usize)>> {
        std::thread::spawn(move || {
            let began = aterm_uds::ready::any_until(
                &[commands.as_fd()],
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            )
            .is_ok_and(|ready| !ready.is_empty());
            assert!(began, "the first command reached its pipe");
            slept.store(7200 * 1_000_000_000, Ordering::Release);
            let mut seen = Vec::new();
            if go.recv().is_err() {
                return seen;
            }
            let mut pending = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match commands.read(&mut buf) {
                    Ok(0) | Err(_) => return seen,
                    Ok(n) => pending.extend_from_slice(&buf[..n]),
                }
                for msg in split_nul_messages(&mut pending) {
                    let id = msg["id"].as_u64().unwrap_or(0);
                    let expression = msg
                        .pointer("/params/expression")
                        .and_then(Value::as_str)
                        .map_or(0, str::len);
                    seen.push((id, expression));
                    let mut reply =
                        aterm_json::to_vec(&json!({"id": id, "result": {"id": id}})).unwrap();
                    reply.push(0);
                    if replies.write_all(&reply).is_err() {
                        return seen;
                    }
                }
            }
        })
    }

    #[test]
    fn a_late_write_holds_later_commands_back_until_it_finishes_and_poisons_none() {
        let (commands, command_write) = std::io::pipe().unwrap();
        let (event_read, replies) = std::io::pipe().unwrap();
        let slept = Arc::new(AtomicU64::new(0));
        let (go, start) = std::sync::mpsc::channel();
        let browser = unread_until(commands, replies, slept.clone(), start);
        let mut client = CdpClient::new(command_write, event_read);
        let continuous = || Some(slept.load(Ordering::Acquire));
        let first = client.call_by(
            None,
            "Runtime.evaluate",
            json!({"expression": "x".repeat(UNREAD_FRAME)}),
            Deadline::with_clocks(aterm_spec::wait::MUST_HAPPEN, &Instant::now, &continuous),
        );
        // A call with no time left reports the unfinished write and sends nothing.
        let held = client.call(
            None,
            "Runtime.evaluate",
            json!({"expression": "held"}),
            Duration::ZERO,
        );
        let _ = go.send(());
        let next = client.call(
            None,
            "Runtime.evaluate",
            json!({"expression": "next"}),
            aterm_spec::wait::MUST_HAPPEN,
        );
        // Its pipe's end ends the stand-in.
        drop(client);
        let seen = browser.join().unwrap();
        assert_eq!(first, Err("timed out writing to browser DevTools pipe"));
        assert_eq!(held, Err("timed out writing to browser DevTools pipe"));
        assert_eq!(next, Ok(json!({"id": 2})));
        assert_eq!(seen, vec![(1, UNREAD_FRAME), (2, "next".len())]);
    }
}

#[test]
fn a_failed_write_refuses_every_later_command() {
    let (commands, command_write) = std::io::pipe().unwrap();
    let (event_read, _replies) = std::io::pipe().unwrap();
    // The browser is gone: a write fails.
    drop(commands);
    let mut client = CdpClient::new(command_write, event_read);
    let first = client.call(
        None,
        "Browser.getVersion",
        json!({}),
        aterm_spec::wait::MUST_HAPPEN,
    );
    let next = client.call(
        None,
        "Browser.getVersion",
        json!({}),
        aterm_spec::wait::MUST_HAPPEN,
    );
    assert_eq!(first, Err("failed writing to browser DevTools pipe"));
    assert_eq!(next, Err("browser DevTools pipe is unavailable"));
}

#[test]
fn a_word_the_writer_gave_counts_after_its_deadline_passed() {
    let (done, ack) = std::sync::mpsc::sync_channel(1);
    done.send(true).unwrap();
    drop(done);
    let gone = Deadline::after(Duration::ZERO);
    assert!(matches!(receive_written(&ack, gone, None), Ok(true)));
}

#[test]
fn navigation_errors_are_distinct_from_protocol_failures() {
    for message in [
        "Execution context was destroyed.",
        "Cannot find context with specified id",
        "Cannot find default execution context",
    ] {
        assert_eq!(
            cdp_response_error(&json!({"code":-32000,"message":message})),
            "browser execution context changed"
        );
    }
    for message in [
        "Session with given id not found.",
        "No target with given id found",
        "Target closed",
        "Session closed",
    ] {
        assert_eq!(
            cdp_response_error(&json!({"code":-32001,"message":message})),
            "browser DevTools session closed"
        );
    }
    for error in [
        json!({"code":-32602,"message":"Invalid parameters"}),
        json!({"code":-32000,"message":"Arbitrary private page error"}),
    ] {
        assert_eq!(
            cdp_response_error(&error),
            "browser DevTools protocol returned an error"
        );
    }
}
