// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness core: the pure judgments behind the aterm wrapper
//! (`docs/DESIGN-aterm-wrapper-2026-09-17.md`) and the read verbs over them.
//! The session loop lives in [`crate::supervise`]; this module supplies its
//! read views and policy, plus the relaunch and upgrade adapters that act on
//! a session through the control protocol.
//!
//! * [`source`] — WHERE DID THIS FACT COME FROM, once: the one [`source::Source`]
//!   vocabulary [`usage`] and `harness limits` print.
//! * [`usage`] — the transcript's usage rows, read into one view; the HUD
//!   line and the `harness usage --json` shape (design §5.2; `"windows"`
//!   stays an empty object there); and the windows Claude Code's `/usage`
//!   panel and Codex's `/status` card paint, which `harness limits` prints
//!   beside the wall the engine's own reader names (`aterm_phase::wall`, the
//!   one wall classifier).
//! * [`disk`] — DISK WATCH AND CLEANUP (design §5.5): the free-space figure
//!   and the stale targets, each row carrying the WITNESS that makes it safe
//!   to remove. Report-only is the shape of the function, not a flag.
//! * [`align`] — the bounded child runner [`disk`] reads `df` through.
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
//!   running Claude Code or Codex onto a newer build: the pure plan, its steps
//!   over one tab or every session, and atpkg's activation notice as a push.
//!   The window's supervisor host takes the steps at each session's idle
//!   points.
//! * [`goal_hold`] — THE ONE RECORD OF A CODEX GOAL ATERM PAUSED, per tab:
//!   which lane holds it (the live upgrade, for its move; the save-then-wait
//!   switch) and what it still owes — written before each key, so a
//!   restarted host resumes the goal once, and the two lanes never both hold
//!   it.
//! * [`upgrade_codex`] — the same step's CODEX branch, its pure half (the
//!   daemon rule, the flag table, the exit hint, the rollout and composer
//!   readers); its I/O half is `upgrade_drive`'s `codex` module. Codex's
//!   shared daemon is moved first by the vendor's own verb, then each TUI by
//!   a typed `/exit` and `codex resume` through [`relaunch`]'s line — never a
//!   signal.
//! * [`upgrade_models`], [`upgrade_catalog`] — THE MODEL HALF of the live
//!   upgrade: the priority list (grown from Claude Code's own
//!   recommendations), the managed build's baked catalog, and THE MODEL
//!   LADDER that moves a session onto the newest model of ITS OWN family the
//!   build offers — and only with none newer, up the list for a model nobody
//!   chose; never down — on its relaunch line (`--model`, never `/model`).
//! * [`lanes`] — WHICH LANES ARE WRITTEN FOR AN AGENT: the relaunch on exit,
//!   the live upgrade and the restart in place, one [`lanes::Lanes`] per
//!   program. agy is supervised with none (its exit is not relaunched, its
//!   build not moved live), and Muse Code is not supervised.
//! * [`relaunch`] — THE RELAUNCH PRIMITIVE both the upgrade and relaunch on
//!   exit use: an agent that no longer runs started again in its own tab, on
//!   its own conversation (Claude Code's `--resume`, Codex's `resume`), and
//!   the host's per-session back-off.
//! * [`resume`] — THE RESUME COMMAND a person is told to run for an agent
//!   the harness cannot bring back itself: for Claude Code, `claude --resume
//!   <this process's own conversation>` with its launch flags carried, read
//!   from Claude Code's own `sessions/<pid>.json` — never `claude
//!   --continue`, which resumes the directory's newest conversation, a
//!   sibling tab's where two share it; for Codex, `codex resume <its kept
//!   flags> <its thread>` ([`resume::codex_command`]), none where the thread
//!   is not its own beyond doubt.
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
//! cleaner ([`disk`]) has no derived model. The Codex branch has run end to end
//! against a REAL Codex through the window's own host
//! (`targo --unverified lab codex-upgrade`); one of three full runs left an
//! embedded session's carry-on owed, cause not captured. Nothing here has run
//! against a REAL exhausted window: the `/usage` panel reader is exercised
//! against captured and hand-built fixtures.

/// The harness's mark as a literal ([`upgrade::HARNESS_MARK`] is this), for
/// the texts `concat!` builds on it: every turn the harness types into an
/// agent's conversation begins with it.
macro_rules! harness_mark {
    () => {
        "[aterm harness]"
    };
}
// By path too, for the texts built outside this module on the same mark (the
// turn-end policy's land instruction, `supervise::policy::turn_end::LAND`).
pub(crate) use harness_mark;

pub mod accounts;
pub mod acts;
pub mod align;
pub mod cli;
pub mod disk;
pub mod footer;
pub mod goal_hold;
pub mod lanes;
pub mod lights;
pub mod netwatch;
pub mod open_url;
pub mod profile;
pub mod relaunch;
pub mod restart_stamp;
pub mod resume;
pub mod self_claimed;
pub mod session_usage;
pub mod source;
pub mod spend;
pub mod stats;
pub mod tap;
pub(crate) mod transcript;
pub mod upgrade;
pub mod upgrade_catalog;
pub mod upgrade_codex;
pub mod upgrade_drive;
pub mod upgrade_models;
pub mod upgrade_wake;
pub mod usage;

/// The longest prefix of `s` that is at most `max` BYTES and ends on a
/// character boundary — never a split character, whatever the input.
///
/// Every reader in this module quotes text it did not write (a command line,
/// a banner, a transcript field) into a bounded ledger row, and each one used
/// to carry its own copy of this walk-back. This is the one copy; a caller
/// that wants an ellipsis appends its own.
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

/// Whether [`one_line`] would hand `text` back as it is: printable ASCII
/// (nothing a fold turns into a space, nothing `trim` takes), no space at
/// either end and no two together. A roster label, a session id or a
/// window's name is such a word, and the roster folds each one every time it
/// is read or written.
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

// Each control fixture owns a listener, cancellation pipe and connected
// sockets, alongside the processes its caller may be observing. Bound those
// fixtures before opening descriptors; the default test runner can otherwise
// crowd out the real process/record reads that guard a restart. One caller
// may keep several instances alive, so its instances share a reentrant lease.
#[cfg(all(test, unix))]
struct FakeInstanceLease;

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
            while *active >= 4 {
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

/// A control socket at `sock` standing in for an aterm instance, for tests:
/// its token, `token`, written where a client reads it, each connection
/// opened with `AUTH <token>` (acknowledged with nothing, as the server does;
/// any other first line drops the connection), and every request after it
/// answered with `answer`'s reply and logged, in order, into the list
/// returned beside the socket's path. That log owns the server: retain it
/// through the fixture's client uses, then dropping its last copy closes the
/// listener and any active client and joins the server, including on unwind.
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
    // Released after Drop has canceled and joined the descriptor owner.
    _lease: std::sync::Arc<FakeInstanceLease>,
}

#[cfg(all(test, unix))]
impl Drop for FakeInstanceServer {
    fn drop(&mut self) {
        use std::io::Write as _;
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(client) = self.active.lock().unwrap_or_else(|p| p.into_inner()).take() {
            // Each half separately: on Darwin Both may refuse a half-closed
            // peer and shut neither half, leaving its BufReader parked.
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
mod tests {
    use super::{one_line, truncate_bytes};

    #[test]
    fn one_line_folds_what_a_hand_rolled_control_sweep_misses() {
        assert_eq!(one_line("  a \n b  ", 99), "a b");
        assert_eq!(one_line("with\ta tab", 99), "with a tab");
        // Negative control: `U+2028` is Zl, not Cc, so `char::is_control()` is
        // false for it and a local sweep would pass it through.
        assert!(!'\u{2028}'.is_control());
        assert_eq!(one_line("a\u{2028}b", 99), "a b");
        assert_eq!(one_line("a\u{2029}b", 99), "a b");
        // The cut is a BYTE cut on a character boundary, taken before the
        // fold, and it never splits a character.
        assert_eq!(one_line("abcdef", 3), "abc");
        assert_eq!(one_line("é😀", 2), "é");
        assert_eq!(one_line("", 0), "");
        // A word handed back as it is reads exactly as the fold reads it, and
        // every near miss (a space at an end, two together, a control, a
        // non-ASCII space, a cut) takes the fold.
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
