// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Background reads of SIBLING aterm instances on this Mac for the menu-bar
//! status item (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3): enumerate the
//! live control sockets the shared control-socket dir knows about, ask each
//! peer `survey facts` — whose rows carry every session's PUBLISHED need
//! (`needs=`), so a sibling's verdict travels in that one read and is never
//! re-guessed here — and post the summarized rows back to the event loop as
//! [`Wake::SiblingScan`]. [`raise_sibling`] dials one sibling's `raise <sid>`,
//! which selects that exact tab there.
//!
//! WHEN. At the status item's install and at every open of its menu, and
//! then on a cadence of its own — every [`RESCAN_KNOWN`] while the last scan
//! found a sibling, every [`RESCAN_NONE`] while it found none — so a
//! sibling need that arises between two opens counts on the badge, and one
//! resolved, or whose sibling quit, leaves it within seconds. Every scan is
//! stamped ([`SiblingInstance::scanned_ms`]), and the glance counts a need
//! only from a young one (`status_item::SIBLING_FRESH`). One cadence at a
//! time: each request takes it over ([`CADENCE`]), and a scan still
//! sleeping for an older one ends when it wakes.
//!
//! House rules (the `config_watcher` discipline): never on the UI thread;
//! self-terminating when the loop is gone; and every peer is bounded by a
//! REAL wall-clock budget — the socket timeout is re-armed with the budget's
//! remainder before every read/write, so a drip-feeding peer cannot stretch
//! per-syscall idle timeouts into an unbounded capture (each line is also
//! byte-capped, and the reply's line count is refused outright over its cap).
//! The scan-in-flight flag clears on EVERY exit path via a drop guard.
//!
//! Trust posture: a graph entry is same-uid-writable, so a dialed socket path
//! is untrusted. Dialing it presents only THAT PEER's own token (read from
//! beside its socket, the `aterm-ctl` discovery contract) — no authority of
//! ours crosses — and everything read back is bounded, so out-of-dir
//! explicit-`--control-sock` instances are summarized just like
//! `aterm-ctl instances` lists them. (Contrast `resolve_sibling`, which
//! CONFINES paths because it relays a client's bytes onward.)
//!
//! A sibling whose rows carry no `needs=` runs an older aterm: its sessions
//! are counted under This Mac and none of its rows is a need — the fleet's
//! rule for an older peer (§12), so an older build's guess never lights the
//! badge.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aterm_types::needs::NeedWord;
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;
use crate::status_item::{SiblingInstance, SiblingNeed};

/// Per-peer WALL-CLOCK budget covering the whole dial (connect, auth, the
/// survey). Every read/write re-arms the socket timeout with the remainder,
/// so this is a hard bound, not a per-syscall idle window.
const PEER_BUDGET: Duration = Duration::from_secs(2);
/// Hard cap on one reply line — a facts row is a few hundred bytes; a
/// newline-less flood hits this long before the budget matters.
const MAX_LINE_BYTES: u64 = 64 * 1024;
/// Reply line-count cap: a header claiming more is REFUSED (not clamped — a
/// clamp would leave unread lines desyncing every later reply).
const MAX_SESSIONS_PER_PEER: usize = 1024;

/// One scan at a time: menu-open spam coalesces onto the in-flight scan (its
/// result is at most a menu-track old, and the NEXT open kicks a fresh one).
static SCAN_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// The cadence's owner: each request that starts a scan takes the next
/// ticket, and a scan thread sleeping for an older one ends when it wakes,
/// so one cadence runs however many opens requested scans.
static CADENCE: AtomicU64 = AtomicU64::new(0);

/// How soon the scan runs again while the last one found a sibling: a need
/// there counts on the badge within this, and leaves it within this of
/// being resolved.
pub(crate) const RESCAN_KNOWN: Duration = Duration::from_secs(15);
/// How soon it runs again while the last one found none: a sibling started
/// since is found within this (a read of the socket directory alone).
pub(crate) const RESCAN_NONE: Duration = Duration::from_secs(60);

/// Kick a background sibling scan on thread `aterm-sibling-scan`; the result
/// arrives as [`Wake::SiblingScan`], and the thread then keeps the cadence
/// (module doc) until a later request takes it over or the loop is gone.
/// Coalesces while one is already running.
pub(crate) fn request_scan(proxy: EventLoopProxy<Wake>) {
    if SCAN_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    /// Clears the in-flight flag on EVERY exit from one scan — panic
    /// included — so a wedged or crashed scan can never freeze future scans.
    struct ClearInFlight;
    impl Drop for ClearInFlight {
        fn drop(&mut self) {
            SCAN_IN_FLIGHT.store(false, Ordering::Release);
        }
    }
    let ticket = CADENCE.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    let spawned = std::thread::Builder::new()
        .name("aterm-sibling-scan".to_string())
        .spawn(move || {
            loop {
                let known = {
                    let _clear = ClearInFlight;
                    let rows = scan_siblings();
                    let known = !rows.is_empty();
                    // Loop gone (shutdown) ⇒ the send fails and that is the end.
                    if proxy.send_event(Wake::SiblingScan(rows)).is_err() {
                        return;
                    }
                    known
                };
                std::thread::sleep(if known { RESCAN_KNOWN } else { RESCAN_NONE });
                // A later request carries the cadence; or a scan runs now.
                if CADENCE.load(Ordering::Acquire) != ticket
                    || SCAN_IN_FLIGHT.swap(true, Ordering::AcqRel)
                {
                    return;
                }
            }
        });
    if spawned.is_err() {
        SCAN_IN_FLIGHT.store(false, Ordering::Release);
    }
}

/// Raise session `sid` of the sibling at `sock` (pid `pid`) on a thread of
/// its own: `raise <sid>` selects that exact tab there and raises its window.
/// Whatever it answered, [`Wake::SiblingRaised`] then brings the sibling's
/// process forward (`App::apprt.activate_instance`, main thread): a window
/// another app raises stays behind the active app, and a sibling that
/// refused or did not answer is at least raised as a process. macOS only:
/// its one caller is the status item's dispatch.
#[cfg(target_os = "macos")]
pub(crate) fn raise_sibling(proxy: EventLoopProxy<Wake>, pid: u32, sock: String, sid: String) {
    let _ = std::thread::Builder::new()
        .name("aterm-sibling-raise".to_string())
        .spawn(move || {
            let raised = raise_on(&sock, &sid).is_some();
            if !raised {
                aterm_log::info!("status item: sibling {pid} did not raise {sid}");
            }
            let _ = proxy.send_event(Wake::SiblingRaised { pid });
        });
}

/// `raise <sid>` on the sibling at `sock`, under [`PEER_BUDGET`]: `Some` when
/// it answered `OK`.
#[cfg(target_os = "macos")]
fn raise_on(sock: &str, sid: &str) -> Option<()> {
    if !sid.starts_with("s-") || sid.bytes().any(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let deadline = Instant::now() + PEER_BUDGET;
    let (stream, token) = aterm_ctl::connect_then_read_token(sock).ok()?;
    let token = token.ok()?;
    arm(&stream, deadline)?;
    (&stream)
        .write_all(format!("AUTH {token}\nraise {sid}\n").as_bytes())
        .ok()?;
    (&stream).flush().ok()?;
    let mut reader = BufReader::new(&stream);
    let line = read_bounded_line(&stream, &mut reader, deadline)?;
    line.starts_with("OK").then_some(())
}

/// Enumerate live sibling instances and summarize each — sorted by pid so the
/// glance fingerprint is deterministic.
fn scan_siblings() -> Vec<SiblingInstance> {
    let mut rows: Vec<SiblingInstance> = sibling_targets()
        .into_iter()
        .filter_map(|(pid, sock)| summarize_peer(pid, &sock))
        .collect();
    rows.sort_by_key(|r| r.pid);
    rows
}

/// How many sibling instances run for this user — the control sockets
/// [`sibling_targets`] finds, whose pid is a live process — this one not
/// counted: the survey's `instances=`, so a fleet reader, which reads this
/// instance alone, says the others were not read. A socket that names no
/// pid, or a gone one, is none. A directory read, no dial.
pub(crate) fn live_siblings() -> usize {
    sibling_targets()
        .into_iter()
        .filter(|&(pid, _)| pid != 0 && aterm_uds::process::pid_alive(pid))
        .count()
}

/// Every sibling instance's `(pid, socket)` the shared control-socket dir
/// knows about, this instance's own left out, each socket once.
fn sibling_targets() -> Vec<(u32, String)> {
    let Some(dir) = aterm_uds::control_socket_dir() else {
        return Vec::new();
    };
    let self_sock = crate::proxy::self_sock_path();
    let own_pid = std::process::id();
    let mut seen: HashSet<String> = HashSet::new();
    let remember = |seen: &mut HashSet<String>, sock: &str| -> bool {
        // Canonical-path dedup (macOS aliases /var to /private/var) doubling
        // as self-exclusion: `self_sock_path` is already canonical.
        let key = std::fs::canonicalize(sock)
            .map_or_else(|_| sock.to_string(), |p| p.to_string_lossy().into_owned());
        if self_sock.as_deref() == Some(key.as_str()) {
            return false;
        }
        seen.insert(key)
    };

    let mut targets: Vec<(u32, String)> = Vec::new();
    // Pass 1: per-instance sockets named aterm-<pid>.sock in the shared dir.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(pid) = aterm_types::control_socket::instance_pid(&name) else {
                continue;
            };
            if !name.ends_with(".sock") || pid == own_pid {
                continue;
            }
            let sock = dir.join(&name).to_string_lossy().into_owned();
            if remember(&mut seen, &sock) {
                targets.push((pid, sock));
            }
        }
    }
    // Pass 2: instances on an explicit `--control-sock`, via graph/<sid> entries —
    // these live OUTSIDE the shared dir by definition (that is the whole point
    // of the pass), so the path is accepted un-confined, absolute-only: the
    // `aterm-ctl instances` contract. Safe because the dial presents only the
    // target's own beside-the-socket token and bounded reads (module doc).
    if let Ok(entries) = std::fs::read_dir(dir.join("graph")) {
        for e in entries.flatten() {
            let Ok(body) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let Some(sock) = aterm_types::control_socket::graph_entry_sock(&body) else {
                continue;
            };
            if !Path::new(&sock).is_absolute() {
                continue;
            }
            let pid = aterm_types::control_socket::graph_entry_pid(&body).unwrap_or(0);
            if pid == own_pid {
                continue;
            }
            if remember(&mut seen, &sock) {
                targets.push((pid, sock));
            }
        }
    }
    targets
}

/// One `session …` row of a `survey facts` reply, read by key: the session
/// counted, and its need where the row publishes one. `None` for a line that
/// is no session row, and for an `exited` session — the LOCAL glance counts
/// only the live, so the sibling summary counts by the same rule.
fn parse_session_row(line: &str) -> Option<(Option<SiblingNeed>, bool)> {
    let rest = line
        .trim_end_matches(['\r', '\n'])
        .strip_prefix("session ")?;
    let field = |key: &str| {
        rest.split(' ')
            .find_map(|tok| tok.strip_prefix(key)?.strip_prefix('='))
            .filter(|v| *v != "-")
            .map(aterm_control::wire::pct_decode)
    };
    if field("state").as_deref() == Some("exited") {
        return None;
    }
    let sid = field("sid")?;
    let Some(needs) = field("needs") else {
        // `needs=-` (nothing needed) reads `None` above too: tell it from a
        // row with no such key, an older aterm's.
        let published = rest.split(' ').any(|tok| tok.starts_with("needs="));
        return Some((None, published));
    };
    let Some(word) = NeedWord::parse(&needs) else {
        // A word this build does not know: a newer aterm's, never guessed.
        return Some((None, true));
    };
    let label = field("role")
        .or_else(|| field("program"))
        .unwrap_or_else(|| crate::presence::short_sid(&sid));
    // A stall's age is its task's; a wall's or a box's the agent
    // publication's. An ask's is neither: the row carries no age of the ask,
    // and the agent's would date a fresh ask by an idle agent's hours, so it
    // has none (it shows no age and sorts last). A frozen or stopped program
    // dates by the agent publication only where it is an identified agent,
    // whose verdict the stall sets; a plain program's `agent_ms` is its
    // session's age, so its freeze has none.
    let agent = field("program_kind").as_deref() == Some("agent");
    let age_ms = match word {
        NeedWord::Stalled => field("task_ms"),
        NeedWord::Ask => None,
        NeedWord::Frozen | NeedWord::Stopped if !agent => None,
        _ => field("agent_ms"),
    }
    .and_then(|ms| ms.parse::<u64>().ok());
    Some((
        Some(SiblingNeed {
            sid,
            word,
            label: crate::needs::fold_clip(&label, 40),
            age_ms,
        }),
        true,
    ))
}

/// The remaining slice of a peer's budget, `None` once exhausted (also `None`
/// for a sub-millisecond sliver — the socket API refuses a zero timeout).
fn budget_left(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| *d >= Duration::from_millis(1))
}

/// Re-arm the socket deadline with the budget remainder before one I/O step.
fn arm(stream: &aterm_uds::CtlStream, deadline: Instant) -> Option<()> {
    let left = budget_left(deadline)?;
    stream.set_read_timeout(Some(left)).ok()?;
    stream.set_write_timeout(Some(left)).ok()?;
    Some(())
}

/// Dial one sibling and read its `survey facts`. A refusal, budget exhaustion
/// or malformed frame skips the peer silently (the `run_discovery` posture).
/// Its live sessions are counted; its needs are the rows' published `needs=`,
/// none at all where its rows publish none (an older aterm, module doc).
fn summarize_peer(pid: u32, sock: &str) -> Option<SiblingInstance> {
    let deadline = Instant::now() + PEER_BUDGET;
    // The sibling's token, read once the connect succeeded: the listener's
    // that took it, never one its update's successor refuses.
    let (stream, token) = aterm_ctl::connect_then_read_token(sock).ok()?;
    let token = token.ok()?;
    arm(&stream, deadline)?;
    (&stream)
        .write_all(format!("AUTH {token}\nsurvey facts\n").as_bytes())
        .ok()?;
    (&stream).flush().ok()?;
    let mut reader = BufReader::new(&stream);
    let header = read_bounded_line(&stream, &mut reader, deadline)?;
    let count: usize = header.trim_end().strip_prefix("OK ")?.parse().ok()?;
    if count > MAX_SESSIONS_PER_PEER + 1 {
        // Refuse, never clamp: reading fewer lines than the peer sent would
        // leave the leftovers desyncing every later reply on this stream.
        return None;
    }
    let mut sessions = 0;
    let mut needs = Vec::new();
    let mut published = true;
    for _ in 0..count {
        let line = read_bounded_line(&stream, &mut reader, deadline)?;
        if let Some((need, publishes)) = parse_session_row(&line) {
            sessions += 1;
            published &= publishes;
            needs.extend(need);
        }
    }
    if !published {
        needs.clear();
    }
    Some(SiblingInstance {
        pid,
        sock: sock.to_string(),
        sessions,
        needs,
        scanned_ms: aterm_time::unix_millis().unwrap_or(0),
    })
}

/// Read one `\n`-terminated line with the byte cap AND the peer budget
/// re-armed first: `None` on EOF, error, exhausted budget, or an
/// unterminated line at the cap (a flood, not a frame).
fn read_bounded_line(
    stream: &aterm_uds::CtlStream,
    reader: &mut BufReader<&aterm_uds::CtlStream>,
    deadline: Instant,
) -> Option<String> {
    arm(stream, deadline)?;
    let mut buf = Vec::new();
    let mut limited = Read::by_ref(reader).take(MAX_LINE_BYTES);
    limited.read_until(b'\n', &mut buf).ok()?;
    if buf.is_empty() || !buf.ends_with(b"\n") {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::parse_session_row;
    use aterm_types::needs::NeedWord;

    /// A `survey facts` row is read by key: its published need, its role
    /// (else its program) as the label, and the age of the fact it stands on
    /// (none for an ask, whose row carries only the agent publication's);
    /// `needs=-` is a counted session needing nothing; an exited session and
    /// a line that is no session row are not counted. A row with no `needs=`
    /// key is an older aterm's (NEGATIVE CONTROL: its sessions count, and its
    /// attention is no need).
    #[test]
    fn survey_facts_rows_carry_the_published_need() {
        let (need, published) = parse_session_row(
            "session local=3 sid=s-abc state=alive program=claude program_kind=agent \
             agent=wall:unresponsive agent_ms=4000 task=- task_ms=- role=review \
             attention=1 needs=frozen hold_by=-\n",
        )
        .expect("a live row");
        assert!(published);
        let need = need.expect("its need");
        assert_eq!(
            (
                need.word,
                need.sid.as_str(),
                need.label.as_str(),
                need.age_ms
            ),
            (NeedWord::Frozen, "s-abc", "review", Some(4000))
        );
        let (need, _) = parse_session_row(
            "session local=4 sid=s-def state=alive program=claude task=stalled task_ms=9000 \
             agent_ms=1 role=- needs=stalled hold_by=-",
        )
        .expect("a live row");
        let need = need.expect("its need");
        assert_eq!((need.label.as_str(), need.age_ms), ("claude", Some(9000)));
        let (need, _) = parse_session_row(
            "session local=9 sid=s-ask state=alive program=claude agent=idle \
             agent_ms=10800000 needs=ask hold_by=-",
        )
        .expect("a live row");
        assert_eq!(need.expect("its need").age_ms, None, "an ask has no age");
        // A plain program's freeze has no age: its `agent_ms` is the
        // session's (the frozen agent above keeps its own).
        let (need, _) = parse_session_row(
            "session local=10 sid=s-sleep state=alive program=sleep program_kind=program \
             agent=- agent_ms=240912 needs=frozen hold_by=-",
        )
        .expect("a live row");
        assert_eq!(need.expect("its need").age_ms, None, "a shell's freeze");
        assert_eq!(
            parse_session_row("session local=5 sid=s-x state=alive needs=- hold_by=-"),
            Some((None, true))
        );
        assert_eq!(
            parse_session_row("session local=6 sid=s-y state=exited needs=ask"),
            None
        );
        assert_eq!(parse_session_row("instance pid=1 host=h"), None);
        assert_eq!(
            parse_session_row("session local=7 sid=s-z state=alive attention=1"),
            Some((None, false)),
            "an older aterm's row"
        );
        assert_eq!(
            parse_session_row("session local=8 sid=s-w state=alive needs=someday"),
            Some((None, true)),
            "a newer word is never guessed"
        );
    }

    /// A scripted sibling over a real Unix socket serving AUTH and `survey
    /// facts` — the whole dial in ONE read: its live sessions are counted,
    /// its published needs travel, an exited row is not counted. NEGATIVE
    /// CONTROL: an older sibling whose rows carry no `needs=` counts its
    /// sessions and raises no need, whatever attention it carries.
    #[cfg(unix)]
    #[test]
    fn summarize_peer_reads_a_scripted_sibling() {
        use std::io::{BufRead, BufReader, Write};
        for (needs, published) in [(" needs=ask", true), ("", false)] {
            // `/tmp`, never the test's TMPDIR: a socket path must fit
            // SUN_LEN, and another test may point TMPDIR somewhere long.
            let scratch = aterm_tempfile::Builder::new()
                .prefix("atsib")
                .tempdir_in("/tmp")
                .unwrap();
            let dir = scratch.path();
            let sock = dir.join("peer.sock");
            // The sibling token read beside the socket (non-pid name ⇒
            // aterm.token).
            std::fs::write(dir.join("aterm.token"), "feedbeef\n").unwrap();
            let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
            let reply = format!(
                "OK 4\ninstance pid=9 host=h version=1 sessions=3 level=facts cut=0\n\
                 session local=0 sid=s-op state=alive program=zsh role=- attention=1{needs}\n\
                 session local=1 sid=s-w state=alive program=zsh role=- attention=-{}\n\
                 session local=2 sid=s-x state=exited program=zsh role=- attention=-{needs}\n",
                if published { " needs=-" } else { "" }
            );
            let server = std::thread::spawn(move || {
                let (conn, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "AUTH feedbeef\n");
                line.clear();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "survey facts\n");
                let mut w = conn.try_clone().unwrap();
                w.write_all(reply.as_bytes()).unwrap();
                // Held open until the client hangs up, as a live sibling
                // holds it: macOS refuses a socket option on a socket whose
                // peer has closed, and the client re-arms its timeout before
                // every line.
                line.clear();
                assert_eq!(reader.read_line(&mut line).unwrap(), 0, "{line:?}");
            });
            let row = super::summarize_peer(4242, sock.to_str().unwrap()).expect("summarized");
            server.join().unwrap();
            assert_eq!(row.pid, 4242);
            assert_eq!(row.sessions, 2, "exited row excluded");
            assert_eq!(row.needs.len(), usize::from(published), "{needs:?}");
        }
    }
}
