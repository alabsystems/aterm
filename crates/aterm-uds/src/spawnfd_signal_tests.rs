// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::command;
use super::tests::{STAGE_ENV, run_staged};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static ENTRY_SIGPIPE: AtomicUsize = AtomicUsize::new(libc::SIG_ERR);

/// Observe the exec boundary before Rust's runtime ignores SIGPIPE. This constructor only
/// reads signal state; it changes no disposition and needs no runtime initialization.
extern "C" fn read_entry_sigpipe() {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: a null new action reads the disposition into a live output slot.
    if unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), action.as_mut_ptr()) } == 0 {
        // SAFETY: the successful query initialized the action.
        ENTRY_SIGPIPE.store(
            unsafe { action.assume_init() }.sa_sigaction,
            Ordering::Relaxed,
        );
    }
}

#[used]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")
)]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
static READ_ENTRY_SIGPIPE: extern "C" fn() = read_entry_sigpipe;

/// A check runner can discard inherited ignores without changing an ordinary command's
/// policy or the parent. The loader-time capture checks SIGPIPE before Rust changes it.
#[test]
fn a_command_can_default_every_catchable_signal() {
    // Retain the constructor by its function too: chained-fixup links can transform its slot.
    std::hint::black_box(read_entry_sigpipe as extern "C" fn());
    const NAME: &str = "spawnfd::signal_tests::a_command_can_default_every_catchable_signal";
    const SIGNALS: &[i32] = &[
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGALRM,
        libc::SIGTSTP,
        libc::SIGTTIN,
        libc::SIGTTOU,
        libc::SIGCONT,
        libc::SIGWINCH,
        libc::SIGURG,
        libc::SIGXCPU,
        libc::SIGXFSZ,
        libc::SIGVTALRM,
        libc::SIGPROF,
    ];
    // Exercise the first application real-time signal and the kernel's upper endpoint too.
    #[cfg(target_os = "linux")]
    let signals: Vec<_> = SIGNALS.iter().copied().chain([34, 64]).collect();
    #[cfg(not(target_os = "linux"))]
    let signals = SIGNALS.to_vec();
    let disposition = |sig| {
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
        // SAFETY: a null new action only reads into the live output slot.
        assert_eq!(
            unsafe { libc::sigaction(sig, std::ptr::null(), action.as_mut_ptr()) },
            0
        );
        // SAFETY: the successful query initialized the action.
        unsafe { action.assume_init() }.sa_sigaction
    };
    match std::env::var(STAGE_ENV).as_deref() {
        Ok("default-signals-probe") => {
            let ignored = std::env::var("ATERM_SIGNAL_EXPECT_IGNORED").as_deref() == Ok("1");
            let expected = if ignored {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            for &sig in &signals {
                assert_eq!(disposition(sig), expected, "signal {sig}");
            }
            assert_eq!(
                ENTRY_SIGPIPE.load(Ordering::Relaxed),
                libc::SIG_DFL,
                "SIGPIPE at exec"
            );
            println!("\nSIGNALS_CHECKED");
        }
        Ok("default-signals-parent") => {
            for sig in signals.iter().copied().chain([libc::SIGPIPE]) {
                // SAFETY: this isolated subprocess changes only its own dispositions.
                assert_ne!(unsafe { libc::signal(sig, libc::SIG_IGN) }, libc::SIG_ERR);
            }
            for reset in [false, true] {
                let mut child = command(std::env::current_exe().unwrap());
                child
                    .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                    .env(STAGE_ENV, "default-signals-probe")
                    .env("ATERM_SIGNAL_EXPECT_IGNORED", if reset { "0" } else { "1" });
                if reset {
                    child.default_signals();
                }
                let out = crate::deadline::ends_within(&mut child, Duration::from_secs(60));
                assert!(out.status.success(), "reset={reset}: {out:?}");
                assert!(
                    String::from_utf8_lossy(&out.stdout).contains("\nSIGNALS_CHECKED\n"),
                    "{out:?}"
                );
            }
            for &sig in &signals {
                assert_eq!(disposition(sig), libc::SIG_IGN, "parent signal {sig}");
            }
            println!("\nSTAGED default-signals");
        }
        _ => {
            let out = run_staged(NAME, "default-signals-parent");
            assert!(out.contains("\nSTAGED default-signals\n"), "{out}");
        }
    }
}
