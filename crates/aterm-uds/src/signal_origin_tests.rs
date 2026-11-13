// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::wait::{MUST_HAPPEN, until_within};

const MODE: &str = "ATERM_SIGNAL_ORIGIN_TEST";

extern "C" fn handler(sig: i32, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    // SAFETY: the test installs SA_SIGINFO below.
    unsafe { capture(sig, info) };
}

#[test]
fn signalled_child() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    start().unwrap();
    // SAFETY: initialized action, live static handler and empty signal mask.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_flags = libc::SA_SIGINFO;
        action.sa_sigaction = handler as *const () as libc::sighandler_t;
        libc::sigemptyset(&raw mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()),
            0
        );
    }
    println!("ready");
    std::io::stdout().flush().unwrap();
    std::fs::write(
        std::env::var_os("ATERM_SIGNAL_ORIGIN_READY").unwrap(),
        "ready",
    )
    .unwrap();
    if mode == "self" {
        // SAFETY: signals only this stand-in; the handler above records it.
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let rows = records();
        if !rows.is_empty() {
            println!("record {}", rows.join("\n"));
            return;
        }
        assert!(std::time::Instant::now() < until, "signal was not recorded");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn names_the_sender_and_self_signal_is_not_a_foreign_sender() {
    let exe = std::env::current_exe().unwrap();
    for mode in ["foreign", "self"] {
        let directory = aterm_tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let mut child = crate::spawnfd::command(&exe)
            .args([
                "--exact",
                "signal_origin::tests::signalled_child",
                "--nocapture",
            ])
            .env(MODE, mode)
            .env("ATERM_SIGNAL_ORIGIN_READY", &ready)
            .stdout(crate::spawnfd::Stdio::piped())
            .spawn()
            .unwrap();
        until_within(MUST_HAPPEN, "the child's installed signal handler", || {
            ready.exists().then_some(())
        });
        let sender = if mode == "foreign" {
            // SAFETY: the child proved it installed its handler and is alive.
            assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
            std::process::id()
        } else {
            child.id()
        };
        let status = until_within(MUST_HAPPEN, "the signalled child's report", || {
            child.try_wait().unwrap()
        });
        let mut text = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut text).unwrap();
        assert!(status.success(), "{text}");
        assert!(text.contains(&format!("sender_pid={sender} ")), "{text}");
        assert!(
            text.contains(&format!("sender_uid={} ", unsafe { libc::getuid() })),
            "{text}"
        );
        assert!(
            text.contains(&format!("executable=Some({:?})", exe)),
            "{text}"
        );
        assert!(text.contains("argv=Some("), "{text}");
        if mode == "self" {
            assert!(
                !text.contains(&format!("sender_pid={} ", std::process::id())),
                "{text}"
            );
        }
    }
}
