// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn native_execs_refuse_process_reads_and_reopen_after_failure() {
    const NAME: &str =
        "spawnfd::exec_gate_tests::native_execs_refuse_process_reads_and_reopen_after_failure";
    const BYPASS: &str = "native-exec-bypassed-held-read";
    let stage = std::env::var(super::tests::STAGE_ENV).unwrap_or_default();
    if stage != "exec-read-bypass" && stage != "exec-read-guarded" {
        let bypass = super::tests::run_staged_hup_ignored(NAME, "exec-read-bypass");
        assert!(bypass.contains(BYPASS), "{bypass}");
        let guarded = super::tests::run_staged_hup_ignored(NAME, "exec-read-guarded");
        assert!(
            guarded.contains("native-exec-read-refused-and-reopened"),
            "{guarded}"
        );
        assert!(!guarded.contains(BYPASS), "{guarded}");
        return;
    }
    let marker = crate::deadline::stand_in(std::path::Path::new("/usr/bin/printf"));
    let read = aterm_sysprobe::exec_gate::reading().expect("the helper's gate is open");
    if stage == "exec-read-bypass" {
        use std::os::unix::process::CommandExt as _;
        // This token models a held read without entering the kernel. The bypass therefore
        // demonstrates actual replacement safely, rather than creating a kernel deadlock.
        #[expect(
            clippy::disallowed_methods,
            reason = "the ungated control: it shows the bypass the gate refuses"
        )]
        let error = std::process::Command::new(&marker).arg(BYPASS).exec();
        panic!("the ungated control did not replace the helper: {error}");
    }
    let file = std::fs::File::open("/dev/null").unwrap();
    let before_flags = super::tests::flags(file.as_raw_fd());
    // SAFETY: this isolated helper changes only its own process-wide disposition.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let sigpipe = || {
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: this read-only query initializes the live, correctly sized action slot.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), action.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful query initialized the action.
        unsafe { action.assume_init() }.sa_sigaction
    };
    let mut ordinary = command(&marker);
    ordinary.arg(BYPASS);
    let error = ordinary.exec();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error}");
    let mut suspended = command("/nonexistent/aterm-exec-read-probe");
    let error = suspended.exec_suspended();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error}");
    let mut pass = transparent(&marker);
    pass.arg(BYPASS);
    let error = pass.exec();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error}");
    let error = pass.prepare_exec().unwrap().exec_until(u64::MAX);
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error}");
    assert_eq!(sigpipe(), libc::SIG_IGN);
    assert_eq!(super::tests::flags(file.as_raw_fd()), before_flags);
    assert!(
        aterm_sysprobe::exec_gate::reading().is_some(),
        "a refused close reopened the gate"
    );
    drop(read);

    // Refusal did not arm suspension: an ordinary attempt still reaches the missing image.
    assert_eq!(suspended.exec().raw_os_error(), Some(libc::ENOENT));
    assert!(aterm_sysprobe::exec_gate::reading().is_some());
    let error = transparent("/nonexistent/aterm-exec-read-probe").exec();
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    assert!(aterm_sysprobe::exec_gate::reading().is_some());
    let error = transparent("/nonexistent/aterm-exec-read-probe")
        .prepare_exec()
        .unwrap()
        .exec_until(u64::MAX);
    assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
    assert!(aterm_sysprobe::exec_gate::reading().is_some());
    let error = pass.prepare_exec().unwrap().exec_until(0);
    assert_eq!(
        error.kind(),
        io::ErrorKind::TimedOut,
        "the original deadline is retained"
    );
    assert!(aterm_sysprobe::exec_gate::reading().is_some());
    assert_eq!(sigpipe(), libc::SIG_IGN);
    assert_eq!(super::tests::flags(file.as_raw_fd()), before_flags);
    println!("native-exec-read-refused-and-reopened");
}
