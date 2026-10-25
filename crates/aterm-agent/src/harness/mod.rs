// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness core: the pure judgments behind the aterm wrapper (`docs/DESIGN-aterm-wrapper-2026-09-17.md`) and the read verbs over them.

/// [`upgrade::HARNESS_MARK`] as a literal for `concat!`: every turn the harness types into an agent's conversation begins with it.
macro_rules! harness_mark {
    () => {
        "[aterm harness]"
    };
}
pub(crate) use harness_mark;

pub mod account_stats;
pub mod accounts;
pub mod accounts_overlay;
pub mod accounts_pool;
pub mod accounts_sync;
pub mod acts;
pub mod agents;
pub mod agy_quota;
pub mod agy_quota_store;
pub mod align;
pub mod browser_window;
pub mod cli;
pub mod codex_alab;
pub mod codex_app_login;
pub mod codex_facts;
pub mod codex_rollout;
pub mod codex_steps;
pub mod codex_tap;
pub mod disk;
pub mod drive_retention;
pub mod excerpt;
pub mod flight;
pub mod footer;
pub mod goal;
pub mod headless_oauth;
pub mod lanes;
pub mod laps;
pub mod lights;
pub mod login_drive;
pub mod login_latch;
pub mod netwatch;
pub mod open_url;
pub mod profile;
pub mod relaunch;
pub mod repo_work;
pub mod restart_stamp;
pub mod result;
pub mod resume;
pub mod self_claimed;
pub mod session_usage;
pub(crate) mod sign_in_ladder;
pub mod source;
pub mod spend;
pub mod stats;
pub mod steps;
pub mod tap;
/// The harness's clock: every bounded wait of the live upgrade and the relaunch, as one value ([`Timings`]).
pub mod timings;
pub use timings::Timings;
pub(crate) mod transcript;
pub use transcript::{claude_reads, codex_reads};
pub mod upgrade;
pub mod upgrade_agy;
pub mod upgrade_agy_live;
pub mod upgrade_catalog;
pub mod upgrade_drive;
pub mod upgrade_gemini;
pub mod upgrade_models;
pub mod upgrade_muse;
pub mod upgrade_muse_live;
pub mod upgrade_wake;
pub mod usage;
pub mod worker;

/// The longest prefix of `s` that is at most `max` bytes and ends on a character boundary.
pub(crate) fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// A caller's text as one bounded line: cut to `max` bytes on a character boundary, then folded through [`crate::supervise::limit::one_line`] (every line break and control character, `U+2028` included, a space; runs of spaces one; the ends trimmed).
pub(crate) fn one_line(text: &str, max: usize) -> String {
    if text.len() <= max && is_one_line_already(text) {
        return text.to_owned();
    }
    crate::supervise::limit::one_line(truncate_bytes(text, max))
        .trim()
        .to_string()
}

/// Whether [`one_line`] would hand `text` back as it is: printable ASCII, no
/// space at either end and no two together.
fn is_one_line_already(text: &str) -> bool {
    let mut spaced = true;
    for &b in text.as_bytes() {
        if !(b' '..=b'~').contains(&b) || (b == b' ' && spaced) {
            return false;
        }
        spaced = b == b' ';
    }
    !spaced || text.is_empty()
}

#[cfg(all(test, unix))]
struct FakeInstanceLease;

/// How many fixtures may be open at once ([`FakeInstanceLease`]): four under
/// a soft descriptor limit below a thousand — macOS's default is 256, and the
/// 18-worker suite peaked at 226 descriptors with no bound — and no bound
/// where the process may hold a thousand or more.
#[cfg(all(test, unix))]
fn fake_instance_bound() -> usize {
    static BOUND: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BOUND.get_or_init(|| {
        const WANT: libc::rlim_t = 4096;
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `getrlimit` writes the one `rlimit` it is handed, which lives on this frame.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut lim) } != 0 {
            return 4;
        }
        if lim.rlim_cur < WANT && lim.rlim_max > lim.rlim_cur {
            let raised = libc::rlimit {
                rlim_cur: lim.rlim_max.min(WANT),
                rlim_max: lim.rlim_max,
            };
            // SAFETY: `setrlimit` reads the one `rlimit` it is handed; a refusal leaves the limit as it was.
            if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const raised) } == 0 {
                lim.rlim_cur = raised.rlim_cur;
            }
        }
        if lim.rlim_cur >= 1024 { usize::MAX } else { 4 }
    })
}

#[cfg(all(test, unix))]
static FAKE_INSTANCE_ADMISSION: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

#[cfg(all(test, unix))]
std::thread_local! {
    static FAKE_INSTANCE_LEASE: std::cell::RefCell<std::sync::Weak<FakeInstanceLease>> =
        const { std::cell::RefCell::new(std::sync::Weak::new()) };
}

#[cfg(all(test, unix))]
impl FakeInstanceLease {
    fn take() -> std::sync::Arc<Self> {
        FAKE_INSTANCE_LEASE.with(|current| {
            if let Some(lease) = current.borrow().upgrade() {
                return lease;
            }
            let (admission, changed) = &FAKE_INSTANCE_ADMISSION;
            let mut active = admission.lock().unwrap_or_else(|e| e.into_inner());
            let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            let bound = fake_instance_bound();
            while *active >= bound {
                let remaining = until
                    .checked_duration_since(std::time::Instant::now())
                    .expect("a control fixture did not release its resource lease");
                (active, _) = changed
                    .wait_timeout(active, remaining)
                    .unwrap_or_else(|e| e.into_inner());
            }
            *active += 1;
            let lease = std::sync::Arc::new(Self);
            *current.borrow_mut() = std::sync::Arc::downgrade(&lease);
            lease
        })
    }
}

#[cfg(all(test, unix))]
impl Drop for FakeInstanceLease {
    fn drop(&mut self) {
        let (admission, changed) = &FAKE_INSTANCE_ADMISSION;
        let mut active = admission.lock().unwrap_or_else(|e| e.into_inner());
        *active -= 1;
        changed.notify_one();
    }
}

/// A test control socket at `sock` standing in for an aterm instance: `token` written where a client reads it, each connection required to open with `AUTH <token>` (acknowledged with nothing, as the server does; any other first line drops it), and every later request answered with `answer`'s reply and logged in order into the returned log.
#[cfg(all(test, unix))]
pub(crate) fn fake_instance(
    sock: &std::path::Path,
    token: &str,
    mut answer: impl FnMut(&str) -> String + Send + 'static,
) -> (String, FakeInstanceLog) {
    use std::io::{BufRead as _, Write as _};
    let lease = FakeInstanceLease::take();
    let path = sock.to_string_lossy().into_owned();
    let token_file = aterm_uds::latest::token_path_for_sock(&path).expect("token path");
    std::fs::write(token_file, format!("{token}\n")).expect("token");
    let listener = std::os::unix::net::UnixListener::bind(sock).expect("bind");
    // The suite spawns children beside every fixture: a scan of one mid-spawn would drain an
    // inherited copy of these sockets (`aterm_uds::keep_out_of_children`).
    aterm_uds::keep_out_of_children(&listener).expect("keep the listener out of children");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let (cancelled, cancel) = std::io::pipe().expect("server cancellation");
    let active = std::sync::Arc::new(std::sync::Mutex::new(None));
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&asked);
    let auth = format!("AUTH {token}");
    let (serving, ended) = (
        std::sync::Arc::clone(&active),
        std::sync::Arc::clone(&stopped),
    );
    let join = std::thread::spawn(move || {
        use std::os::fd::AsRawFd as _;
        loop {
            let mut ready = [
                libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: cancelled.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: both initialized pollfds borrow descriptors kept open by this thread.
            let polled = unsafe { libc::poll(ready.as_mut_ptr(), 2, -1) };
            if polled < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            if polled < 0 || ready[1].revents != 0 {
                break;
            }
            let conn = match listener.accept() {
                Ok((conn, _)) => conn,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(_) => break,
            };
            let _ = conn.set_nonblocking(false);
            let _ = aterm_uds::keep_out_of_children(&conn);
            let Ok(cut) = conn.try_clone() else {
                continue;
            };
            let _ = aterm_uds::keep_out_of_children(&cut);
            {
                let mut active = serving.lock().unwrap_or_else(|p| p.into_inner());
                if ended.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                *active = Some(cut);
            }
            let Ok(mut out) = conn.try_clone() else {
                drop(serving.lock().unwrap_or_else(|p| p.into_inner()).take());
                continue;
            };
            let _ = aterm_uds::keep_out_of_children(&out);
            let mut lines = std::io::BufReader::new(conn).lines();
            if !lines.next().is_some_and(|l| l.is_ok_and(|l| l == auth)) {
                drop(serving.lock().unwrap_or_else(|p| p.into_inner()).take());
                continue;
            }
            for line in lines {
                if ended.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let Ok(line) = line else { break };
                let reply = answer(&line);
                if let Ok(mut log) = log.lock() {
                    log.push(line);
                }
                if writeln!(out, "{reply}").is_err() {
                    break;
                }
            }
            drop(serving.lock().unwrap_or_else(|p| p.into_inner()).take());
        }
    });
    (
        path,
        FakeInstanceLog {
            log: asked,
            _server: std::sync::Arc::new(FakeInstanceServer {
                active,
                stopped,
                cancel: Some(cancel),
                join: Some(join),
                _lease: lease,
            }),
        },
    )
}

#[cfg(all(test, unix))]
#[derive(Clone)]
pub(crate) struct FakeInstanceLog {
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    _server: std::sync::Arc<FakeInstanceServer>,
}

#[cfg(all(test, unix))]
impl std::ops::Deref for FakeInstanceLog {
    type Target = std::sync::Mutex<Vec<String>>;

    fn deref(&self) -> &Self::Target {
        &self.log
    }
}

#[cfg(all(test, unix))]
impl std::fmt::Debug for FakeInstanceLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.log, f)
    }
}

#[cfg(all(test, unix))]
struct FakeInstanceServer {
    active: std::sync::Arc<std::sync::Mutex<Option<std::os::unix::net::UnixStream>>>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cancel: Option<std::io::PipeWriter>,
    join: Option<std::thread::JoinHandle<()>>,
    _lease: std::sync::Arc<FakeInstanceLease>,
}

#[cfg(all(test, unix))]
impl Drop for FakeInstanceServer {
    fn drop(&mut self) {
        use std::io::Write as _;
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(client) = self.active.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = client.shutdown(std::net::Shutdown::Write);
            let _ = client.shutdown(std::net::Shutdown::Read);
        }
        if let Some(cancel) = self.cancel.as_mut() {
            let _ = cancel.write_all(&[1]);
        }
        drop(self.cancel.take());
        if let Some(join) = self.join.take()
            && let Err(panic) = join.join()
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

#[cfg(test)]
#[path = "alab_marker_cross_tests.rs"]
mod alab_marker_cross_tests;

/// How the harness tests spell this host's paths.
#[cfg(test)]
pub(crate) mod host_path {
    /// `posix`, a test's absolute POSIX path, as one this host reads as
    /// absolute: itself on Unix, on Windows the same path on drive `C:`
    /// (`/s/main.1` is `C:/s/main.1`, its separators kept).
    pub(crate) fn abs(posix: &str) -> String {
        debug_assert!(
            posix.starts_with('/'),
            "{posix:?} is not a POSIX absolute path"
        );
        if cfg!(windows) {
            format!("C:{posix}")
        } else {
            posix.to_owned()
        }
    }

    /// `path` as the inside of a JSON string (its quotes not included), as
    /// Claude Code and Codex write a path: a Windows path's backslashes
    /// escaped.
    pub(crate) fn json(path: &std::path::Path) -> String {
        let quoted = aterm_json::to_string(&path.display().to_string()).expect("a string");
        quoted[1..quoted.len() - 1].to_owned()
    }
}

/// The `/bin/sh` stand-ins the harness tests run under a budget.
#[cfg(all(test, unix))]
pub(crate) mod stand_in {
    /// Writes the owner-only `/bin/sh` stand-in `path` running `body`, then runs it once with
    /// no arguments until it writes `ran` ([`aterm_uds::deadline::prints`]); `body` runs only
    /// when it is given arguments. macOS's program check assesses a new file at its first exec,
    /// and a loaded host holds that exec for seconds, which a bounded run of the stand-in (agy's
    /// usage fetch, a version probe) would count against its budget: the bounded run must meet
    /// a stand-in that has already run, and the stand-in's directory may go once this returns.
    pub(crate) fn proven(path: &std::path::Path, body: &str) {
        use aterm_uds::spawnfd::Stdio;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(
            path,
            format!("#!/bin/sh\nif [ $# -eq 0 ]; then echo ran; exit 0; fi\n{body}\n"),
        )
        .expect("write the stand-in");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let mut child = aterm_uds::spawnfd::command(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the stand-in");
        aterm_uds::deadline::prints(&mut child, "ran", aterm_uds::deadline::ECHO_HANG);
        aterm_uds::deadline::kill_within(&mut child, aterm_uds::deadline::ECHO_HANG);
    }
}

#[cfg(test)]
mod tests {
    use super::{one_line, truncate_bytes};

    #[test]
    fn one_line_folds_what_a_hand_rolled_control_sweep_misses() {
        assert_eq!(one_line("  a \n b  ", 99), "a b");
        assert_eq!(one_line("with\ta tab", 99), "with a tab");
        assert_eq!(one_line("a\u{2028}b", 99), "a b");
        assert_eq!(one_line("a\u{2029}b", 99), "a b");
        assert_eq!(one_line("abcdef", 3), "abc");
        assert_eq!(one_line("é😀", 2), "é");
        assert_eq!(one_line("", 0), "");
        for (text, max) in [
            ("work", 80),
            ("a@b.example", 80),
            ("five hours left", 80),
            ("(x) ~y_z-1:2", 80),
            (" lead", 80),
            ("trail ", 80),
            ("two  spaces", 80),
            ("tab\there", 80),
            ("nb\u{a0}sp", 80),
            ("\u{3000}ideo", 80),
            ("cut here", 3),
            ("", 80),
        ] {
            let folded = crate::supervise::limit::one_line(truncate_bytes(text, max))
                .trim()
                .to_string();
            assert_eq!(one_line(text, max), folded, "{text:?} at {max}");
        }
    }

    #[test]
    fn truncate_bytes_never_splits_a_character_and_never_grows_a_string() {
        assert_eq!(truncate_bytes("abc", 8), "abc");
        assert_eq!(truncate_bytes("abc", 3), "abc");
        assert_eq!(truncate_bytes("abc", 2), "ab");
        assert_eq!(truncate_bytes("abc", 0), "");
        assert_eq!(truncate_bytes("é", 1), "", "a two-byte character, cut at 1");
        assert_eq!(truncate_bytes("aé", 2), "a");
        for max in 0..4 {
            assert_eq!(truncate_bytes("😀", max), "", "max {max}");
        }
        assert_eq!(truncate_bytes("😀", 4), "😀");
        let mixed = "a😀é b\u{2028}ω";
        for max in 0..=mixed.len() + 2 {
            let cut = truncate_bytes(mixed, max);
            assert!(cut.len() <= max.min(mixed.len()), "max {max}");
            assert!(mixed.starts_with(cut), "max {max}");
        }
    }
}
