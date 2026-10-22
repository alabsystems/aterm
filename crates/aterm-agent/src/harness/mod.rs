// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness core: the pure judgments behind the aterm wrapper
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md`) and the read verbs over them.
//! The session loop lives in [`crate::supervise`]; this module supplies its
//! read views and policy, plus the relaunch and upgrade adapters that act on
//! a session through the control protocol.
//!
//! * [`source`] — the one [`source::Source`] vocabulary for where a fact came from.
//! * [`usage`] — the transcript's usage rows as one view: the HUD line, the
//!   `harness usage --json` shape, and the windows Claude Code's `/usage` panel and
//!   Codex's `/status` card paint, which `harness limits` prints.
//! * [`disk`] — free space and stale targets, each row carrying the witness that
//!   makes it safe to remove. Report-only is the shape of the function, not a flag.
//! * [`align`] — the bounded child runner: a child read to a deadline.
//! * [`footer`] — the Claude Code footer aterm paints in place of the vendor's
//!   permission-mode row: model + effort, working directory, branch, read
//!   from the files Claude Code already keeps.
//! * [`spend`] — THE DOLLAR FIGURES of the account stats: every transcript
//!   of a Claude Code home folded into list-price dollars per time bucket
//!   (the shipped, dated `prices.toml`, injected), kept with byte cursors in
//!   a spend file so a second fold reads only appended bytes, and a window's
//!   capacity (`C ≥ $ ÷ used`) and what is left (`≈ C × (1 − used)`). A leaf
//!   that nothing deciding anything imports.
//! * [`session_usage`] — THIS session's usage, one implementation for the
//!   footer and `harness usage`: its tokens per model, folded incrementally
//!   from its own transcript and its subagents' (live), and the limit wall
//!   Claude Code wrote into its transcript when it hit one, while its reset
//!   is ahead. Nothing of it is read off the screen.
//! * [`lights`] — the light at the footer's end (the permission mode), drawn
//!   only while it differs from what the owner expects: read from Claude
//!   Code's mode pill, toggled through its own shift+tab, read back after
//!   every press. Fast mode has no light of aterm's: Claude draws its own `↯`.
//! * [`cli`] — THE COMMAND: `aterm harness usage|limits|disk|ledger`, the
//!   read views, `accounts` (the roster's report and its `add`, `login` and
//!   `use`, over [`accounts`]), `upgrade` (one hand-run pass of the live
//!   upgrade) and `upgrade models` (its model priority list), plus the
//!   retired hook-bridge verbs answered as tombstones.
//! * [`upgrade`], [`upgrade_drive`], [`upgrade_wake`] — THE LIVE UPGRADE of a
//!   running Claude Code onto a newer build (a codex-alab's restart is
//!   carried by [`codex_alab`]'s relaunch, never moved live): the pure plan, its steps
//!   over one tab or every session, and atpkg's activation notice as a push.
//!   The window's supervisor host takes the steps at each session's idle
//!   points.
//! * [`upgrade_models`], [`upgrade_catalog`] — THE MODEL HALF of the live
//!   upgrade: the priority list (grown from Claude Code's own
//!   recommendations), the managed build's baked catalog, and THE MODEL
//!   LADDER that moves a session onto the newest model of ITS OWN family the
//!   build offers — and only with none newer, up the list for a model nobody
//!   chose; never down — on its relaunch line (`--model`, never `/model`).
//! * [`lanes`] — WHICH LANES ARE WRITTEN FOR AN AGENT: the relaunch on exit,
//!   the live upgrade and the restart in place, one [`lanes::Lanes`] per
//!   program. Gemini CLI has the relaunch on exit (and so the restore) alone:
//!   it is never moved onto a newer build live. Muse Code and agy have the
//!   relaunch on exit and the live upgrade's quiet flow.
//! * [`result`] — A WORKER'S RESULT: the report a Claude Code worker ended
//!   its task with, read from its transcript and snapshotted once at the
//!   done edge under `<state>/tasks/`, and the `result [full]` reply over
//!   that snapshot (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.3).
//! * [`upgrade_muse`] — the MUSE CODE lane's pure half (relaunch on exit and
//!   restore): its flag table, the UUID-only session id, its own exit table
//!   (a start it refused is no crash), the session-log fallback and the
//!   `.session.lock` witness; its I/O half is `upgrade_drive`'s `muse` module.
//! * [`upgrade_muse_live`] — MUSE CODE'S LIVE UPGRADE, its pure half (the
//!   quiet flow alone): the revision-aware order of two store builds, its
//!   session log's part of a process and the work open in it, and the gate;
//!   its I/O half is `upgrade_drive`'s `muse::live` module.
//! * [`upgrade_agy`] — the AGY lane's pure half (relaunch on exit and
//!   restore): its Go flag table (the hidden config flags carried), the
//!   UUIDv4 conversation id, its exit table read with the dead pid's crash
//!   file (Ctrl-Z alive, a refused start no crash), and the presence-lock
//!   witness; its I/O half is `upgrade_drive`'s `agy` module.
//! * [`upgrade_agy_live`] — AGY'S LIVE UPGRADE, its pure half (the quiet flow
//!   alone): atpkg's order of two store builds, the background work open in
//!   the conversation's transcript, its summary row, the wakes pending, and
//!   the gate; its I/O half is `upgrade_drive`'s `agy::live` module.
//! * [`upgrade_gemini`] — the GEMINI CLI lane's pure half (relaunch on exit
//!   and restore, no live upgrade): its yargs flag table, the full-UUID
//!   session id, its exit table (a start it refused is no crash), the
//!   start-time witness (the one session header of its project begun since
//!   it started) and the read-only pre-check of what `--resume` would load;
//!   its I/O half is `upgrade_drive`'s `gemini` module.
//! * [`relaunch`] — THE RELAUNCH PRIMITIVE both the upgrade and relaunch on
//!   exit use: an agent that no longer runs started again in its own tab, on
//!   its own conversation (Claude Code's `--resume`, codex-alab's validated
//!   line, Muse Code's `resume <uuid>`, agy's `--conversation=<uuid>`, Gemini
//!   CLI's `--resume <uuid>`), and the host's per-session back-off.
//! * [`resume`] — THE RESUME COMMAND a person is told to run for an agent
//!   the harness cannot bring back itself: for Claude Code, `claude --resume
//!   <this process's own conversation>` with its launch flags carried, read
//!   from Claude Code's own `sessions/<pid>.json` — never `claude
//!   --continue`, which resumes the directory's newest conversation, a
//!   sibling tab's where two share it; for Muse Code, its own `muse resume
//!   <uuid>` ([`upgrade_muse::told_line`]) behind the data and config homes
//!   it ran under; for agy, its own `agy --conversation=<uuid>`
//!   ([`upgrade_agy::told_line`]) after the flags that moved its config and
//!   behind the `HOME` it ran under; for Gemini CLI, its own `gemini --resume
//!   <uuid>` ([`upgrade_gemini::told_line`]) in its project, where what that
//!   resume would load holds every turn. None for a Codex: codex-alab comes
//!   back on the line aterm's copy of its own record validated.
//! * [`netwatch`] — THE API'S REACH, pure: which route an agent takes, what
//!   one probe of the default route may conclude, and the one instance's
//!   measure and schedule while a supervised session waits at a wall the
//!   network answers (the window's host carries it out: aterm-gui
//!   `harness_netprobe`).
//! * [`accounts`] — CLAUDE CODE ACCOUNTS, the pure core
//!   (`docs/DESIGN-claude-accounts-2026-10-01.md`): the roster the owner
//!   wrote (`accounts.toml`), what this machine has seen of each account,
//!   the condition that follows, the advice a session gets at a turn end
//!   (`Stay | Probe | Move | Wait`), and the store for its state and ledger.
//!   It touches no session and reads no credential; it chooses a login and
//!   leaves the Claude home alone.
//! * [`profile`] — CLAUDE CODE'S PROFILE, READ BY AN ALLOWLIST
//!   (`docs/DESIGN-account-stats-2026-10-15.md`): the plan and its tier, the
//!   usage-credits word, the cached `/usage` body and how the plan bills
//!   Fable, from the `.claude.json` of a home, and the account's key hashed
//!   from its id — never a field off the list, so never the credentials
//!   that file can hold. `accounts stats` prints it.
//! * [`account_stats`] — THE ACCOUNT STATS OF ONE MACHINE
//!   (`docs/DESIGN-account-stats-2026-10-15.md` §3, §7): the view `accounts
//!   stats` prints — each window's figure from every source by one merge
//!   rule, current or void at its own reset, cut by a limit reset used inside
//!   it, with its `as of`, source and machine; the plan, the credits, the
//!   Fable week as last seen, list-price dollars, the sessions on it, its
//!   login — every empty figure with the closed word that says why. A leaf
//!   that nothing deciding anything imports.
//! * [`acts`] — WHAT THE HARNESS JUST DID, AND HOW A LANDING ENDED, in the
//!   words the window shows (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//!   §9): an act the Autopilot chip lights for a moment, the page's counter
//!   a ledger row adds to, and the task's face under Land that `status
//!   task=` prints.
//! * [`stats`] — WHAT THE HARNESS DID, for Settings ▸ Harness
//!   (`docs/DESIGN-harness-autopilot-2026-10-15.md` §4): boxes approved,
//!   questions answered, continuations, Land's wrap-ups, resumes after walls,
//!   relaunches, upgrades, escalations and the time they waited on a person,
//!   folded from the ledgers the supervisor and the live upgrade already
//!   write — each fact from one file, read bounded, nothing created.
//!
//! [`crate::supervise`] is the one supervisor; the second harness stack this
//! module once held is gone (design §0.4 is the record).
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested; the bounded machines
//! modelled by the `harness_*` entries in `aterm_spec::xref::model_registry`
//! each carry a Tier-1 bind to the real code, which that entry's comment
//! names (in this module's tests, the window host's, or
//! `aterm-agent/tests/conformance_*`). The footer reader's machine is
//! `ClaudeFooterModel` (`tests/conformance_footer_model.rs`); the disk
//! cleaner ([`disk`]) has no derived model. codex-alab's watch beside its own
//! harness (the proven claim, the crash relaunch on its validated line) runs
//! end to end against the REAL package through a private headless aterm in
//! `targo --unverified lab codex-alab-proof` (`docs/measured/`); no vendor
//! Codex is supervised (the Codex screen lane's retirement, 2026-10-16).
//! Nothing here has run against a REAL exhausted window: the `/usage` panel
//! reader is exercised against captured and hand-built fixtures.

/// [`upgrade::HARNESS_MARK`] as a literal for `concat!`: every turn the harness
/// types into an agent's conversation begins with it.
macro_rules! harness_mark {
    () => {
        "[aterm harness]"
    };
}
// Exported by path for texts built on the mark outside this module
// (`supervise::policy::turn_end::LAND`).
pub(crate) use harness_mark;

pub mod account_stats;
pub mod accounts;
pub mod accounts_sync;
pub mod acts;
pub mod agents;
pub mod align;
pub mod cli;
pub mod codex_alab;
pub mod codex_rollout;
pub mod codex_steps;
pub mod codex_tap;
pub mod disk;
pub mod drive_retention;
pub mod excerpt;
pub mod flight;
pub mod footer;
pub mod lanes;
pub mod lights;
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
pub mod source;
pub mod spend;
pub mod stats;
pub mod steps;
pub mod tap;
/// The harness's clock: every bounded wait of the live upgrade and the
/// relaunch, as one value ([`Timings`]).
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

/// The longest prefix of `s` that is at most `max` bytes and ends on a
/// character boundary. A caller that wants an ellipsis appends its own.
pub(crate) fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    // A `str` is UTF-8, so index 0 is always a boundary: this cannot panic.
    s.get(..end).unwrap_or("")
}

/// A caller's text as one bounded line: cut to `max` bytes on a character
/// boundary, then folded through [`crate::supervise::limit::one_line`] (every
/// line break and control character, `U+2028` included, a space; runs of
/// spaces one; the ends trimmed).
pub(crate) fn one_line(text: &str, max: usize) -> String {
    if text.len() <= max && is_one_line_already(text) {
        return text.to_owned();
    }
    crate::supervise::limit::one_line(truncate_bytes(text, max))
        .trim()
        .to_string()
}

/// Whether [`one_line`] would hand `text` back as it is: printable ASCII, no
/// space at either end and no two together. The fast path for roster labels,
/// session ids and window names, folded on every read and write.
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

// Each control fixture holds a listener, a cancellation pipe and connected
// sockets. Admission bounds how many are live at once, taken before any
// descriptor opens, so parallel tests cannot crowd out the descriptors the real
// process and record reads that guard a restart need. The lease is per thread
// and reentrant: one test may hold several fixtures.
#[cfg(all(test, unix))]
struct FakeInstanceLease;

/// How many fixtures may be open at once ([`FakeInstanceLease`]): four under
/// a soft descriptor limit below a thousand — macOS's default is 256, and the
/// 18-worker suite peaked at 226 descriptors with no bound — and no bound
/// where the process may hold a thousand or more. The soft limit is raised
/// toward 4096, within the hard limit, the first time a fixture is asked for:
/// the bound was the descriptors', and four fixtures at a time held every
/// other fixture test in a queue.
#[cfg(all(test, unix))]
fn fake_instance_bound() -> usize {
    static BOUND: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BOUND.get_or_init(|| {
        const WANT: libc::rlim_t = 4096;
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `getrlimit` writes the one `rlimit` it is handed, which
        // lives on this frame.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut lim) } != 0 {
            return 4;
        }
        if lim.rlim_cur < WANT && lim.rlim_max > lim.rlim_cur {
            let raised = libc::rlimit {
                rlim_cur: lim.rlim_max.min(WANT),
                rlim_max: lim.rlim_max,
            };
            // SAFETY: `setrlimit` reads the one `rlimit` it is handed; a
            // refusal leaves the limit as it was.
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

/// A test control socket at `sock` standing in for an aterm instance: `token`
/// written where a client reads it, each connection required to open with
/// `AUTH <token>` (acknowledged with nothing, as the server does; any other
/// first line drops it), and every later request answered with `answer`'s reply
/// and logged in order into the returned log. The log owns the server: keep it
/// alive while the test uses the socket; dropping its last copy closes the
/// listener and any client and joins the server thread, on unwind too.
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
            // SAFETY: both initialized pollfds borrow descriptors kept open
            // by this thread. Cancellation closes the pipe's write half.
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
            let Ok(cut) = conn.try_clone() else {
                continue;
            };
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
    // Released only after `drop` has cancelled and joined the server thread.
    _lease: std::sync::Arc<FakeInstanceLease>,
}

#[cfg(all(test, unix))]
impl Drop for FakeInstanceServer {
    fn drop(&mut self) {
        use std::io::Write as _;
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(client) = self.active.lock().unwrap_or_else(|p| p.into_inner()).take() {
            // Each half separately: on Darwin `Both` may refuse a half-closed
            // peer and shut neither half, leaving the server's reader parked.
            let _ = client.shutdown(std::net::Shutdown::Write);
            let _ = client.shutdown(std::net::Shutdown::Read);
        }
        // A byte wakes poll even if a concurrent fork briefly holds another
        // copy of the pipe writer before exec; closing alone would not.
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

/// How the harness tests spell this host's paths. They are written with POSIX
/// absolute paths (`/logins/work-2`), which Windows reads as relative: its
/// absolute paths start with a drive.
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

#[cfg(test)]
mod tests {
    use super::{one_line, truncate_bytes};

    #[test]
    fn one_line_folds_what_a_hand_rolled_control_sweep_misses() {
        assert_eq!(one_line("  a \n b  ", 99), "a b");
        assert_eq!(one_line("with\ta tab", 99), "with a tab");
        // `U+2028` is Zl, not Cc: a `char::is_control()` sweep would pass it.
        assert_eq!(one_line("a\u{2028}b", 99), "a b");
        assert_eq!(one_line("a\u{2029}b", 99), "a b");
        // The cut is a byte cut on a character boundary, taken before the
        // fold, and it never splits a character.
        assert_eq!(one_line("abcdef", 3), "abc");
        assert_eq!(one_line("é😀", 2), "é");
        assert_eq!(one_line("", 0), "");
        // The fast path agrees with the fold, and every near miss takes the fold.
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
        // Negative cases: a cut that lands inside a multi-byte character
        // walks back, and a cut inside a four-byte one walks back three.
        assert_eq!(truncate_bytes("é", 1), "", "a two-byte character, cut at 1");
        assert_eq!(truncate_bytes("aé", 2), "a");
        for max in 0..4 {
            assert_eq!(truncate_bytes("😀", max), "", "max {max}");
        }
        assert_eq!(truncate_bytes("😀", 4), "😀");
        // Every prefix of a mixed string is valid UTF-8 and no longer than
        // asked for.
        let mixed = "a😀é b\u{2028}ω";
        for max in 0..=mixed.len() + 2 {
            let cut = truncate_bytes(mixed, max);
            assert!(cut.len() <= max.min(mixed.len()), "max {max}");
            assert!(mixed.starts_with(cut), "max {max}");
        }
    }
}
