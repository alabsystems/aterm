// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::process::ExitStatusExt as _;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use super::*;

const CHILD: &str = "ATERM_TEST_CAFFEINE_SIGTERM";
const HANG: Duration = Duration::from_secs(60);
const ENDING: &str = "ending by the system's SIGTERM";
static HEARD: OnceLock<mpsc::Sender<libc::pid_t>> = OnceLock::new();

fn heard(sender: libc::pid_t) {
    if let Some(tx) = HEARD.get() {
        let _ = tx.send(sender);
    }
}

fn disposition() -> libc::sigaction {
    // SAFETY: all-zero is valid storage; a null new action only reads.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &raw mut action) },
        0
    );
    action
}

fn set_disposition(handler: libc::sighandler_t) {
    // SAFETY: all-zero is a valid empty mask and no flags.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler;
    // SAFETY: changes only this exact-test child's disposition.
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGTERM, &raw const action, std::ptr::null_mut()) },
        0
    );
}

fn isolated(test: &str) -> Option<std::process::Output> {
    let test = module_path!()
        .split_once("::")
        .map_or_else(|| test.to_owned(), |(_, path)| format!("{path}::{test}"));
    if std::env::var(CHILD).as_deref() == Ok(test.as_str()) {
        set_disposition(libc::SIG_DFL);
        return None;
    }
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test exe"));
    cmd.args([test.as_str(), "--exact", "--nocapture"])
        .env(CHILD, &test);
    Some(aterm_uds::deadline::output_within(&mut cmd, HANG).expect("the child ends"))
}

fn passed(out: &std::process::Output) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("test result: ok. 1 passed;")),
        "the child ran no test:\n{stdout}"
    );
}

#[test]
fn only_launchd_and_the_kernel_are_the_system() {
    assert!(sent_by_the_system(0));
    assert!(sent_by_the_system(1));
    assert!(!sent_by_the_system(2));
    assert!(!sent_by_the_system(4242));
}

#[test]
fn a_persons_sigterm_is_handed_over_with_its_pid() {
    let Some(out) = isolated("a_persons_sigterm_is_handed_over_with_its_pid") else {
        let (tx, rx) = mpsc::channel();
        HEARD.set(tx).expect("one test sets it");
        let watching = watch(heard).expect("SIGTERM is watched");
        assert_ne!(disposition().sa_sigaction, libc::SIG_DFL);
        let me = libc::pid_t::try_from(std::process::id()).expect("a pid");
        // SAFETY: signals this process, whose SIGTERM the watch now takes.
        assert_eq!(unsafe { libc::kill(me, libc::SIGTERM) }, 0);
        assert_eq!(rx.recv_timeout(HANG), Ok(me));
        drop(watching);
        assert_eq!(disposition().sa_sigaction, libc::SIG_DFL);
        return;
    };
    passed(&out);
}

#[test]
fn the_systems_sigterm_ends_the_process_as_its_default_does() {
    let Some(out) = isolated("the_systems_sigterm_ends_the_process_as_its_default_does") else {
        let (tx, rx) = mpsc::channel();
        HEARD.set(tx).expect("one test sets it");
        let _watching = watch(heard).expect("SIGTERM is watched");
        // SAFETY: all-zero is a valid `siginfo_t`; the sender is launchd.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        info.si_signo = libc::SIGTERM;
        info.si_pid = 1;
        println!("{ENDING}");
        on_term(libc::SIGTERM, &raw mut info, std::ptr::null_mut());
        panic!("still running; quit heard {:?}", rx.recv_timeout(HANG));
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(ENDING),
        "the child never signalled:\n{stdout}"
    );
    assert_eq!(
        out.status.signal(),
        Some(libc::SIGTERM),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_ignored_sigterm_is_left_alone() {
    let Some(out) = isolated("an_ignored_sigterm_is_left_alone") else {
        set_disposition(libc::SIG_IGN);
        assert!(watch(heard).is_none());
        assert_eq!(disposition().sa_sigaction, libc::SIG_IGN);
        return;
    };
    passed(&out);
}
