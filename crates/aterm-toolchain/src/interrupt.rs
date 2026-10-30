// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN INTERRUPT ENDS A WAIT (`docs/DESIGN-adaptive-build-2026-10-24.md`, WAIT-INTERRUPT).
//!
//! A run that waits for room in the capacity ledger or for a named lock has started nothing, so
//! SIGINT, SIGTERM or SIGHUP then should end it at once as NOT RUN, its place in the queue given
//! up. The tools catch interrupts their own ways (verify ends its stage groups and dies of the
//! signal, the release steps and the lab set a flag their work looks at, a bench keeps the
//! default), and none of those reaches a loop that sleeps between looks, nor leaves its queue
//! entry behind it. So a wait holds a [`Watch`]: while one lives, the process's own handling of
//! those three signals is set aside for this module's, which notes the first that comes; the
//! wait looks at it between short naps ([`Watch::nap`]); and the last watch to end puts the
//! process's own handling back. A signal the process ignores (a background job's SIGINT) stays
//! ignored. The wait that ends on the signal takes it ([`Watch::take`]), and its caller ends as
//! NOT RUN; one no wait took (it came as the wait ended otherwise, or after a wait took one and
//! before its watch ended) is sent again once the process's own handling is back, so no
//! interrupt is lost and a second one ends the process as its own handling would.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The longest a wait naps between two looks at the interrupt: it ends well within a second.
pub const NAP: Duration = Duration::from_millis(50);

/// The first interrupt no wait has taken ([`Watch::take`]) since the outermost live watch began;
/// 0 for none.
static CAUGHT: AtomicI32 = AtomicI32::new(0);

/// The live watches, and what the outermost set aside.
static HELD: Mutex<Held> = Mutex::new(Held {
    depth: 0,
    #[cfg(unix)]
    saved: Vec::new(),
});

struct Held {
    depth: usize,
    /// Each signal this module catches, with the action it set aside.
    #[cfg(unix)]
    saved: Vec<(libc::c_int, libc::sigaction)>,
}

/// The signals a wait ends on.
#[cfg(unix)]
const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    let _ = CAUGHT.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
}

/// `SIGTERM`, `SIGINT`, `SIGHUP`, else `signal N`.
#[must_use]
pub fn name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".to_owned(),
        2 => "SIGINT".to_owned(),
        15 => "SIGTERM".to_owned(),
        n => format!("signal {n}"),
    }
}

/// A wait in progress: SIGINT, SIGTERM and SIGHUP, each the process does not ignore, are noted
/// for it ([`Watch::caught`]) instead of reaching the process's own handling, until the last
/// live watch ends.
#[derive(Debug)]
pub struct Watch(());

impl Watch {
    /// Begin a wait. The outermost watch sets aside the process's handling of each signal it
    /// does not ignore, as it reads at this moment; the last one to end puts it back, unless a
    /// handler was installed over this module's meanwhile, which stays.
    #[must_use]
    pub fn enter() -> Watch {
        let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
        if held.depth == 0 {
            CAUGHT.store(0, Ordering::SeqCst);
            #[cfg(unix)]
            for sig in SIGNALS {
                // SAFETY: a zeroed `sigaction` is a valid value; with a null new action the call
                // only reads the current one into `old`, which outlives it.
                let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
                if unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut old) } != 0
                    || old.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                // SAFETY: as above for the zeroed value; `on_signal` is an `extern "C" fn(c_int)`
                // that lives for the whole program and touches one atomic, which is
                // async-signal-safe; `sa_mask` is emptied by `sigemptyset` before the call reads it.
                let installed = unsafe {
                    let mut new: libc::sigaction = std::mem::zeroed();
                    new.sa_sigaction =
                        on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    new.sa_flags = libc::SA_RESTART;
                    libc::sigemptyset(&raw mut new.sa_mask);
                    libc::sigaction(sig, &raw const new, std::ptr::null_mut()) == 0
                };
                if installed {
                    held.saved.push((sig, old));
                }
            }
        }
        held.depth += 1;
        Watch(())
    }

    /// The interrupt that came since the outermost live watch began and no wait took, if one did.
    #[must_use]
    pub fn caught(&self) -> Option<i32> {
        match CAUGHT.load(Ordering::SeqCst) {
            0 => None,
            sig => Some(sig),
        }
    }

    /// [`Self::caught`], taken by this wait, which ends on it: it is not sent again when the
    /// last watch ends. One that comes after it is noted afresh, and sent again then.
    #[must_use]
    pub fn take(&self) -> Option<i32> {
        match CAUGHT.swap(0, Ordering::SeqCst) {
            0 => None,
            sig => Some(sig),
        }
    }

    /// Sleep `d`, ending early once an interrupt came or `stop` says to, looking every [`NAP`]:
    /// `true` when it ended early.
    pub fn nap(&self, d: Duration, stop: &dyn Fn() -> bool) -> bool {
        let end = Instant::now().checked_add(d);
        loop {
            if self.caught().is_some() || stop() {
                return true;
            }
            let now = Instant::now();
            if let Some(end) = end {
                if now >= end {
                    return false;
                }
                std::thread::sleep((end - now).min(NAP));
            } else {
                std::thread::sleep(NAP);
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
        held.depth = held.depth.saturating_sub(1);
        if held.depth > 0 {
            return;
        }
        #[cfg(unix)]
        for (sig, old) in held.saved.drain(..).rev() {
            // SAFETY: a zeroed `sigaction` is a valid value, and a null new action only reads the
            // current one into `now`; `old` is the action `sigaction` itself read for `sig` when
            // the watch began.
            unsafe {
                let mut now: libc::sigaction = std::mem::zeroed();
                let ours = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                if libc::sigaction(sig, std::ptr::null(), &raw mut now) == 0
                    && now.sa_sigaction == ours
                {
                    libc::sigaction(sig, &raw const old, std::ptr::null_mut());
                }
            }
        }
        let sig = CAUGHT.swap(0, Ordering::SeqCst);
        if sig != 0 {
            resend(sig);
        }
    }
}

/// Send `sig` to this process again, now that its own handling is back.
#[cfg(unix)]
fn resend(sig: i32) {
    // SAFETY: `kill` takes two integers and touches no memory of ours; the pid is this process's.
    unsafe {
        libc::kill(libc::getpid(), sig);
    }
}

#[cfg(not(unix))]
fn resend(_sig: i32) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use aterm_spec::wait::{MUST_HAPPEN, until_within};

    /// Set, the test binary is [`standin_waits_on_a_watch`]: it writes its pid into this
    /// directory's `waiting` once its watch is live.
    const STANDIN: &str = "ATERM_TOOLCHAIN_INTERRUPT_STANDIN";
    /// Set, the stand-in first puts SIGINT back to its default action, as a foreground process
    /// has it; else it keeps the disposition it was started with.
    const STANDIN_SIGINT: &str = "ATERM_TOOLCHAIN_INTERRUPT_SIGINT";
    /// Set, the stand-in ignores SIGINT before its watch begins, as a background job of a
    /// non-interactive shell starts.
    const STANDIN_IGNORE: &str = "ATERM_TOOLCHAIN_INTERRUPT_IGNORE";
    /// Set, a second SIGTERM reaches the stand-in after its wait took the first and before its
    /// watch ends: it sends it to itself there, holding that point of the race.
    const STANDIN_TWICE: &str = "ATERM_TOOLCHAIN_INTERRUPT_TWICE";

    /// Run by the tests below in a process of its own: naps on a watch for twice its parent's
    /// one-minute hang detector, so only an interrupt the watch takes ends it in time, then
    /// exits 3, as a tool's NOT RUN, naming the signal.
    #[test]
    fn standin_waits_on_a_watch() {
        let Some(dir) = std::env::var_os(STANDIN).map(PathBuf::from) else {
            return;
        };
        // SAFETY: SIG_DFL and SIG_IGN are dispositions, no handler of ours.
        unsafe {
            if std::env::var_os(STANDIN_SIGINT).is_some() {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
            }
            if std::env::var_os(STANDIN_IGNORE).is_some() {
                libc::signal(libc::SIGINT, libc::SIG_IGN);
            }
        }
        let watch = Watch::enter();
        std::fs::write(dir.join("waiting"), std::process::id().to_string()).expect("said");
        assert!(
            watch.nap(MUST_HAPPEN * 2, &|| false),
            "no interrupt ended the watch"
        );
        let sig = watch.take().expect("an interrupt ended the nap");
        let twice = std::env::var_os(STANDIN_TWICE).is_some();
        if twice {
            resend(libc::SIGTERM);
            until_within(MUST_HAPPEN, "the second SIGTERM noted", || watch.caught());
        }
        drop(watch);
        if twice {
            // The second SIGTERM, sent again at its default action, ends the process while it
            // naps (a process-directed signal reaches some thread soon after, not at once); a
            // lost one leaves it to its parent's hang detector.
            std::thread::sleep(MUST_HAPPEN * 2);
        }
        println!("stopped by {}", name(sig));
        std::process::exit(3);
    }

    /// A stand-in waiting on a watch, its pid, and its scratch directory. One still running
    /// when it is dropped (a test that failed before it ended) is killed, so none outlives its
    /// test.
    struct Standin {
        child: aterm_uds::spawnfd::Child,
        pid: i32,
        dir: aterm_tempfile::TempDir,
    }

    impl Drop for Standin {
        fn drop(&mut self) {
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    /// Start [`standin_waits_on_a_watch`] with `env` set, once its watch is live.
    fn standin(env: &[&str]) -> Standin {
        let dir = aterm_tempfile::Builder::new()
            .prefix("atc-interrupt-")
            .tempdir()
            .expect("a scratch dir");
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        for key in env {
            cmd.env(key, "1");
        }
        let child = cmd
            .args([
                "--exact",
                "interrupt::tests::standin_waits_on_a_watch",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(STANDIN, dir.path())
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the stand-in");
        let mut standin = Standin { child, pid: 0, dir };
        let waiting = standin.dir.path().join("waiting");
        standin.pid = until_within(MUST_HAPPEN, "the stand-in's watch", || {
            std::fs::read_to_string(&waiting).ok()?.trim().parse().ok()
        });
        standin
    }

    /// Send `sig` to the stand-in and read how it ended, within its one-minute hang detector.
    fn signal(
        standin: &mut Standin,
        sig: libc::c_int,
    ) -> (std::process::ExitStatus, String, Duration) {
        let sent = Instant::now();
        // SAFETY: `kill` takes two integers; the pid is this test's own child.
        assert_eq!(
            unsafe { libc::kill(standin.pid, sig) },
            0,
            "signalled the stand-in"
        );
        let status = until_within(MUST_HAPPEN, "the stand-in's end", || {
            standin.child.try_wait().ok().flatten()
        });
        let took = sent.elapsed();
        let mut out = String::new();
        if let Some(mut o) = standin.child.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut o, &mut out);
        }
        (status, out, took)
    }

    /// SIGTERM, and SIGINT where the process does not ignore it, end a wait as NOT RUN well
    /// inside the one-minute hang detector (the time is reported), the wait naming the signal;
    /// the process's own handling would have ended it by the signal instead (default action).
    #[test]
    fn an_interrupt_ends_a_watched_wait() {
        for (env, sig, word) in [
            (&[][..], libc::SIGTERM, "SIGTERM"),
            (&[STANDIN_SIGINT][..], libc::SIGINT, "SIGINT"),
        ] {
            let mut standin = standin(env);
            let (status, out, took) = signal(&mut standin, sig);
            assert_eq!(status.code(), Some(3), "{word}: {status} {out}");
            assert!(out.contains(&format!("stopped by {word}")), "{out}");
            eprintln!("{word} ended the stand-in's wait in {took:?}");
        }
    }

    /// An ignored SIGINT stays ignored through a watch: SIGINT, then SIGTERM, end the wait on
    /// SIGTERM. A watch notes only the first signal it catches, and the kernel delivers SIGINT
    /// no later than SIGTERM, so a caught SIGINT would have been the one named.
    #[test]
    fn an_ignored_interrupt_stays_ignored() {
        let mut standin = standin(&[STANDIN_IGNORE]);
        // SAFETY: as in `signal`.
        assert_eq!(unsafe { libc::kill(standin.pid, libc::SIGINT) }, 0);
        let (status, out, _) = signal(&mut standin, libc::SIGTERM);
        assert_eq!(status.code(), Some(3), "{status} {out}");
        assert!(out.contains("stopped by SIGTERM"), "{out}");
    }

    /// A second interrupt that comes after a wait took the first, before its watch ends, is no
    /// wait's: it is sent again once the process's own handling is back, which ends the
    /// process at once (here the default action, killed by SIGTERM), not after the NOT RUN the
    /// first one led to. Were a taken signal's mark kept instead, the second would be lost and
    /// the stand-in would exit 3.
    #[test]
    fn a_second_interrupt_after_the_wait_took_one_is_not_lost() {
        use std::os::unix::process::ExitStatusExt as _;
        let mut standin = standin(&[STANDIN_TWICE]);
        let (status, out, _) = signal(&mut standin, libc::SIGTERM);
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status} {out}");
        assert!(!out.contains("stopped by"), "{out}");
    }

    #[test]
    fn an_unbounded_nap_does_not_overflow_instant() {
        let watch = Watch::enter();
        assert!(watch.nap(Duration::MAX, &|| true));
    }
}
