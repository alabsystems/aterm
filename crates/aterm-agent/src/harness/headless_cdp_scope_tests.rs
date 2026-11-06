// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::account_work::{Drained, Lifetime, Pause};
use std::os::fd::AsFd as _;

fn retire(pause: Pause) -> Drained {
    let (done, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = done.send(pause.wait());
    });
    let drained = result.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
    worker.join().unwrap();
    drained
}

#[test]
fn a_detached_cdp_reader_retains_the_scope_until_its_real_pipe_closes() {
    let lifetime = Lifetime::default();
    let (command_read, command_write) = std::io::pipe().unwrap();
    let (output_read, output_write) = std::io::pipe().unwrap();
    let client = CdpClient::new_scoped(command_write, output_read, lifetime.enter()).unwrap();
    drop(client);
    let held = lifetime.pause().unwrap().try_wait();
    let was_held = held.is_err();
    drop(output_write);
    drop(command_read);
    let drained = match held {
        Ok(drained) => drained,
        Err(pause) => retire(pause),
    };
    assert!(was_held, "the real reader still owns an open browser pipe");
    assert!(drained.resume().is_ok());
}

#[test]
fn a_backpressured_cdp_writer_retains_its_command_buffer_and_scope() {
    let lifetime = Lifetime::default();
    let (mut command_read, command_write) = std::io::pipe().unwrap();
    let (output_read, output_write) = std::io::pipe().unwrap();
    let client = CdpClient::new_scoped(command_write, output_read, lifetime.enter()).unwrap();
    let (done, written) = std::sync::mpsc::sync_channel(1);
    // A private ordinary pipe cannot accept this buffer while only one byte is read.
    let sent = client.to.send((vec![b'x'; 8 * 1024 * 1024], done)).is_ok();
    let ready = aterm_uds::ready::any_until(
        &[command_read.as_fd()],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .is_ok_and(|ready| !ready.is_empty());
    let mut first = [0];
    let entered_write = ready && command_read.read_exact(&mut first).is_ok();
    let pause = lifetime.pause().unwrap();
    drop(client);
    drop(output_write);
    let held = pause.try_wait();
    let was_held = held.is_err();
    // Closing our read end releases the real writer, not a fake completion notification.
    drop(command_read);
    let drained = match held {
        Ok(drained) => drained,
        Err(pause) => retire(pause),
    };
    let answer = written.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    assert!(sent && entered_write);
    assert_eq!(first, [b'x']);
    assert!(was_held);
    assert!(!answer.unwrap());
    assert!(drained.resume().is_ok());
}

#[test]
fn a_closed_scope_refuses_cdp_workers_and_retires_the_supplied_pipes() {
    let lifetime = Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let pause = lifetime.pause().unwrap();
    let (mut command_read, command_write) = std::io::pipe().unwrap();
    let (output_read, output_write) = std::io::pipe().unwrap();
    let refused = CdpClient::new_scoped(command_write, output_read, Some(scope)).is_err();
    drop(output_write);
    let mut byte = [0];
    let readable = aterm_uds::ready::any_until(
        &[command_read.as_fd()],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let ended = !readable.is_empty() && command_read.read(&mut byte).unwrap() == 0;
    drop(command_read);
    let drained = pause
        .try_wait()
        .expect("refused constructor retired its actual resources");
    assert!(refused && ended);
    assert!(drained.resume().is_ok());
}

#[test]
fn a_scoped_cdp_client_preserves_real_command_and_response_delivery() {
    let lifetime = Lifetime::default();
    let (mut command_read, command_write) = std::io::pipe().unwrap();
    let (output_read, mut output_write) = std::io::pipe().unwrap();
    let client = CdpClient::new_scoped(command_write, output_read, lifetime.enter()).unwrap();
    let worker = std::thread::spawn(move || {
        let mut client = client;
        client.call(
            None,
            "Browser.getVersion",
            json!({}),
            aterm_spec::wait::MUST_HAPPEN,
        )
    });
    let mut pending = Vec::new();
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let request = loop {
        if pending.len() > 64 * 1024
            || !aterm_uds::ready::any_until(&[command_read.as_fd()], deadline)
                .is_ok_and(|ready| !ready.is_empty())
        {
            break None;
        }
        let mut byte = [0];
        if command_read.read_exact(&mut byte).is_err() {
            break None;
        }
        if byte[0] == 0 {
            break aterm_json::from_slice::<Value>(&pending).ok();
        }
        pending.push(byte[0]);
    };
    let delivered = if let Some(request) = &request {
        let mut reply =
            aterm_json::to_vec(&json!({"id":request["id"],"result":{"delivered":true}})).unwrap();
        reply.push(0);
        output_write.write_all(&reply).is_ok()
    } else {
        false
    };
    drop(output_write);
    drop(command_read);
    let answer = worker.join().unwrap();
    let drained = retire(lifetime.pause().unwrap());
    assert!(request.is_some());
    assert!(delivered);
    assert_eq!(answer.unwrap(), json!({"delivered":true}));
    assert!(drained.resume().is_ok());
}

#[test]
fn cancelling_a_cdp_response_wait_does_not_release_the_actual_reader() {
    let lifetime = Lifetime::default();
    let (mut command_read, command_write) = std::io::pipe().unwrap();
    let (output_read, output_write) = std::io::pipe().unwrap();
    let mut client = CdpClient::new_scoped(command_write, output_read, lifetime.enter()).unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let answer = client.call(
            None,
            "Browser.synthetic",
            json!({}),
            Duration::from_secs(3600),
        );
        drop(client);
        let _ = done.send(answer);
    });
    let ready = aterm_uds::ready::any_until(
        &[command_read.as_fd()],
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .is_ok_and(|ready| !ready.is_empty());
    let mut byte = [0];
    let requested = ready && command_read.read_exact(&mut byte).is_ok();
    let pause = lifetime.pause().unwrap();
    let answer = finished.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let returned = answer.is_ok();
    let held = pause.try_wait();
    let was_held = held.is_err();
    drop(output_write);
    drop(command_read);
    let answer = answer.unwrap_or_else(|_| {
        finished
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap()
    });
    worker.join().unwrap();
    let drained = match held {
        Ok(drained) => drained,
        Err(pause) => retire(pause),
    };
    assert!(requested && returned);
    assert!(answer.is_err());
    assert!(
        was_held,
        "a cancelled call did not retire its still-open reader pipe"
    );
    assert!(drained.resume().is_ok());
}
