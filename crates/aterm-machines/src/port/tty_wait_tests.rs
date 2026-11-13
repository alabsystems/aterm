// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{BufRead as _, BufReader, Read as _};
use std::sync::atomic::{AtomicI32, Ordering};

static CAUGHT: AtomicI32 = AtomicI32::new(0);

extern "C" fn note(signal: i32) {
    CAUGHT.store(signal, Ordering::Release);
}

#[test]
fn attached_child_stand_in() {
    if std::env::var_os("ATERM_ATTACHED_CHILD_STAND_IN").is_none() {
        return;
    }
    // SAFETY: this stand-in owns its process; note stores one atomic integer only.
    unsafe { libc::signal(libc::SIGINT, note as *const () as libc::sighandler_t) };
    println!("attached child ready");
    let until = std::time::Instant::now() + Duration::from_secs(60);
    while CAUGHT.load(Ordering::Acquire) == 0 {
        assert!(std::time::Instant::now() < until, "no forwarded interrupt");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(CAUGHT.load(Ordering::Acquire), libc::SIGINT);
    println!("attached child stopped");
}

#[test]
fn an_exact_cli_interrupt_reaches_its_attached_child_without_killing_a_group() {
    for injected_error in [false, true] {
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "port::tty_wait::tests::attached_child_stand_in",
                "--nocapture",
            ])
            .env("ATERM_ATTACHED_CHILD_STAND_IN", "1")
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (send, ready) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            let mut line = String::new();
            while output.read_line(&mut line).unwrap() > 0 {
                if line.contains("attached child ready") {
                    let _ = send.send(output);
                    return;
                }
            }
        });
        let mut output = match ready.recv_timeout(Duration::from_secs(60)) {
            Ok(output) => output,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                reader.join().unwrap();
                panic!("attached child did not prove it was ready: {error}");
            }
        };
        reader.join().unwrap();
        let result = if injected_error {
            let mut first = true;
            wait_with(
                &mut child,
                || Some(libc::SIGINT),
                |child| {
                    if std::mem::take(&mut first) {
                        Err(io::Error::other("injected attached owning wait failure"))
                    } else {
                        child.try_wait()
                    }
                },
            )
        } else {
            wait(&mut child, || Some(libc::SIGINT))
        };
        assert!(
            child.is_reaped(),
            "a wait error cannot release the attached child"
        );
        let mut remaining = String::new();
        output.read_to_string(&mut remaining).unwrap();
        assert!(remaining.contains("attached child stopped"), "{remaining}");
        if injected_error {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Other);
        } else {
            assert!(result.unwrap().success());
        }
    }
}
