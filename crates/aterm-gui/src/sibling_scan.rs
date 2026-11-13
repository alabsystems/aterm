// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Background sibling instance scanner (`survey facts` -> [`Wake::SiblingScan`]) and [`raise_sibling`] dialer.
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
/// survey).
const PEER_BUDGET: Duration = Duration::from_secs(2);
/// Hard cap on one reply line — a facts row is a few hundred bytes; a
/// newline-less flood hits this long before the budget matters.
const MAX_LINE_BYTES: u64 = 64 * 1024;
/// Maximum session rows accepted from one peer; account facts have no row quota.
const MAX_SESSIONS_PER_PEER: usize = 1024;
/// Bound an entire survey reply by bytes, rather than assuming an account count.
/// Each line contains at least its newline, so this also bounds the frame count.
const MAX_REPLY_BYTES: usize = 8 * 1024 * 1024;

/// One scan at a time: menu-open spam coalesces onto the in-flight scan (its
/// result is at most a menu-track old, and the NEXT open kicks a fresh one).
static SCAN_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// The cadence's owner: each request that starts a scan takes the next
/// ticket, and a scan thread sleeping for an older one ends when it wakes,
/// so one cadence runs however many opens requested scans.
static CADENCE: AtomicU64 = AtomicU64::new(0);

/// Rescan interval when the previous scan found a live sibling.
pub(crate) const RESCAN_KNOWN: Duration = Duration::from_secs(15);
/// Rescan interval when the previous scan found no siblings.
pub(crate) const RESCAN_NONE: Duration = Duration::from_secs(60);

/// Starts or coalesces a background sibling scan sending [`Wake::SiblingScan`].
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
                    if proxy.send_event(Wake::SiblingScan(rows)).is_err() {
                        return;
                    }
                    known
                };
                std::thread::sleep(if known { RESCAN_KNOWN } else { RESCAN_NONE });
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

/// Dials `raise <sid>` on the sibling at `sock` (`pid`) on a background thread.
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
    let mut reader = BufReader::new(PeerRead {
        stream: &stream,
        deadline,
    });
    let line = read_bounded_line(&mut reader, deadline)?;
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

/// Counts live sibling `aterm` instances for the current user.
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
        let key = std::fs::canonicalize(sock)
            .map_or_else(|_| sock.to_string(), |p| p.to_string_lossy().into_owned());
        if self_sock.as_deref() == Some(key.as_str()) {
            return false;
        }
        seen.insert(key)
    };

    let mut targets: Vec<(u32, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let file_name = e.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(pid) = aterm_types::control_socket::instance_pid(name) else {
                continue;
            };
            if !name.ends_with(".sock") || pid == own_pid {
                continue;
            }
            let sock = dir.join(name).to_string_lossy().into_owned();
            if remember(&mut seen, &sock) {
                targets.push((pid, sock));
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("graph")) {
        for e in entries.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let Ok(body) = crate::proxy::read_private_text(&e.path()) else {
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
/// counted, and its need where the row publishes one.
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
        let published = rest.split(' ').any(|tok| tok.starts_with("needs="));
        return Some((None, published));
    };
    let Some(word) = NeedWord::parse(&needs) else {
        // Unknown need word from a newer build.
        return Some((None, true));
    };
    let label = field("role")
        .or_else(|| field("program"))
        .unwrap_or_else(|| crate::presence::short_sid(&sid));
    let agent = field("program_kind").as_deref() == Some("agent");
    let by = field("needs_by");
    let claim = (by.as_deref() == Some("program"))
        .then(|| crate::needs::claim_words_of_row(word, field("agent").as_deref()))
        .flatten();
    let age_ms = match word {
        NeedWord::Stalled => field("task_ms"),
        NeedWord::Ask => None,
        NeedWord::Frozen | NeedWord::Stopped if !agent => None,
        _ => field("agent_ms"),
    }
    .filter(|ms| !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()))
    .and_then(|ms| ms.parse::<u64>().ok());
    Some((
        Some(SiblingNeed {
            sid,
            word,
            label: crate::needs::fold_clip(&label, 40),
            age_ms,
            claim,
            carries_on: by.as_deref() == Some("harness"),
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

/// Bound the request's writes before sending it.
fn arm(stream: &aterm_uds::CtlStream, deadline: Instant) -> Option<()> {
    let left = budget_left(deadline)?;
    stream.set_write_timeout(Some(left)).ok()?;
    Some(())
}

/// Dial one sibling and read its `survey facts`. A refusal, budget exhaustion
/// or malformed frame skips the peer silently (the `run_discovery` posture).
fn summarize_peer(pid: u32, sock: &str) -> Option<SiblingInstance> {
    summarize_peer_within(pid, sock, PEER_BUDGET)
}

/// [`summarize_peer`] within `budget` of wall clock, not [`PEER_BUDGET`].
fn summarize_peer_within(pid: u32, sock: &str, budget: Duration) -> Option<SiblingInstance> {
    let deadline = Instant::now() + budget;
    let (stream, token) = aterm_ctl::connect_then_read_token(sock).ok()?;
    let token = token.ok()?;
    arm(&stream, deadline)?;
    (&stream)
        .write_all(format!("AUTH {token}\nsurvey facts\n").as_bytes())
        .ok()?;
    (&stream).flush().ok()?;
    let mut reader = BufReader::new(PeerRead {
        stream: &stream,
        deadline,
    });
    let header = read_bounded_line(&mut reader, deadline)?;
    let count: usize = header
        .trim_end()
        .strip_prefix("OK ")
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()?;
    if count > MAX_REPLY_BYTES {
        // Refuse, never clamp: reading fewer lines than the peer sent would
        // leave the leftovers desyncing every later reply on this stream.
        return None;
    }
    let mut sessions = 0;
    let mut needs = Vec::new();
    let mut published = true;
    let mut bytes = 0usize;
    for _ in 0..count {
        let line = read_bounded_line(&mut reader, deadline)?;
        bytes = bytes.checked_add(line.len())?;
        if bytes > MAX_REPLY_BYTES {
            return None;
        }
        if let Some((need, publishes)) = parse_session_row(&line) {
            sessions += 1;
            if sessions > MAX_SESSIONS_PER_PEER {
                return None;
            }
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

struct PeerRead<'a> {
    stream: &'a aterm_uds::CtlStream,
    deadline: Instant,
}

impl Read for PeerRead<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        aterm_uds::hangup::read_until(self.stream, bytes, self.deadline)
    }
}

/// Read one `\n`-terminated line with the byte cap and the peer budget:
/// `None` on EOF, error, exhausted budget, or an
/// unterminated line at the cap (a flood, not a frame).
fn read_bounded_line(reader: &mut BufReader<PeerRead<'_>>, deadline: Instant) -> Option<String> {
    budget_left(deadline)?;
    let mut buf = Vec::new();
    let mut limited = Read::by_ref(reader).take(MAX_LINE_BYTES);
    limited.read_until(b'\n', &mut buf).ok()?;
    if buf.is_empty() || !buf.ends_with(b"\n") {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
#[path = "sibling_scan_tests.rs"]
mod tests;
