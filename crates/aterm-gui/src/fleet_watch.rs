// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Background discovery of SIBLING aterm instances for the menu-bar status
//! item: enumerate the live control sockets the shared control-socket dir
//! knows about, dial each peer's `sessions` roster (with typed-meta
//! follow-ups so a sibling's `role`/`attention` state travels), classify with
//! the SAME pure classifier — and the same live-session universe (`exited`
//! rows excluded) — as the local glance, and post the summarized rows back to
//! the event loop as a `Wake`.
//!
//! House rules (the `config_watcher` discipline): never on the UI thread;
//! self-terminating when the loop is gone; and every peer is bounded by a
//! REAL wall-clock budget — the socket timeout is re-armed with the budget's
//! remainder before every read/write, so a drip-feeding peer cannot stretch
//! per-syscall idle timeouts into an unbounded capture (each line is also
//! byte-capped, and the roster line-count is refused outright over its cap).
//! The scan-in-flight flag clears on EVERY exit path via a drop guard.
//!
//! Trust posture: a graph entry is same-uid-writable, so a dialed socket path
//! is untrusted. Dialing it presents only THAT PEER's own token (read from
//! beside its socket, the `aterm-ctl` discovery contract) — no authority of
//! ours crosses — and everything read back is bounded, so out-of-dir
//! explicit-`$ATERM_CONTROL_SOCK` instances are summarized just like
//! `aterm-ctl instances` lists them. (Contrast `resolve_sibling`, which
//! CONFINES paths because it relays a client's bytes onward.)

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use crate::Wake;
use crate::status_item::{InstanceRow, SessionRow};

/// Per-peer WALL-CLOCK budget covering the whole dial (connect, auth, roster,
/// meta follow-ups). Every read/write re-arms the socket timeout with the
/// remainder, so this is a hard bound, not a per-syscall idle window.
const PEER_BUDGET: Duration = Duration::from_secs(2);
/// Hard cap on one reply line — roster lines and meta readouts are short;
/// a newline-less flood hits this long before the budget matters.
const MAX_LINE_BYTES: u64 = 64 * 1024;
/// Roster line-count cap: a header claiming more is REFUSED (not clamped —
/// a clamp would leave unread lines desyncing every later reply).
const MAX_SESSIONS_PER_PEER: usize = 1024;
/// How many `meta=1` sessions per peer get the typed-meta follow-up read.
/// Generous because the peer budget bounds the true cost; past the cap a
/// session's escalation still surfaces via the `⚠`-title fallback.
const MAX_META_LOOKUPS: usize = 256;

/// One scan at a time: menu-open spam coalesces onto the in-flight scan (its
/// result is at most a menu-track old, and the NEXT open kicks a fresh one).
static SCAN_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Kick a background sibling scan; the result arrives as
/// [`Wake::FleetInstances`]. Coalesces while one is already running.
pub(crate) fn request_scan(proxy: EventLoopProxy<Wake>) {
    if SCAN_IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    /// Clears the in-flight flag on EVERY exit from the scan thread — panic
    /// included — so a wedged or crashed scan can never freeze future scans.
    struct ClearInFlight;
    impl Drop for ClearInFlight {
        fn drop(&mut self) {
            SCAN_IN_FLIGHT.store(false, Ordering::Release);
        }
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-fleet-scan".to_string())
        .spawn(move || {
            let _clear = ClearInFlight;
            let rows = scan_siblings();
            // Loop gone (shutdown) ⇒ the send fails and that is the end.
            let _ = proxy.send_event(Wake::FleetInstances(rows));
        });
    if spawned.is_err() {
        SCAN_IN_FLIGHT.store(false, Ordering::Release);
    }
}

/// Enumerate live sibling instances and summarize each — sorted by pid so the
/// glance fingerprint is deterministic.
fn scan_siblings() -> Vec<InstanceRow> {
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
    // Pass 2: explicit-$ATERM_CONTROL_SOCK instances via graph/<sid> entries —
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

    let mut rows: Vec<InstanceRow> = targets
        .into_iter()
        .filter_map(|(pid, sock)| summarize_peer(pid, &sock))
        .collect();
    rows.sort_by_key(|r| r.pid);
    rows
}

/// One roster line: `<local> <sid> <parent|-> <state> <title-pct> meta=<0|1>`.
struct PeerSession {
    id: u64,
    sid: String,
    title: String,
    has_meta: bool,
}

/// Parse one `sessions` roster line, tolerantly: a malformed line reads as
/// `None` (skip, never abort the peer), and an `exited` session reads as
/// `None` too — the LOCAL glance excludes the dead (a dead operator is not a
/// running operator), so the sibling summary must count by the same rule.
/// Fields split POSITIONALLY on single spaces (the wire format), so an
/// empty pct-encoded title survives as an empty token instead of collapsing
/// the field count; tokens after `meta=` are additive-tolerated.
fn parse_roster_line(line: &str) -> Option<PeerSession> {
    let mut f = line.trim_end_matches(['\r', '\n']).split(' ');
    let (id, sid, _parent, state, title, meta) = (
        f.next()?,
        f.next()?,
        f.next()?,
        f.next()?,
        f.next()?,
        f.next()?,
    );
    if state == "exited" {
        return None;
    }
    Some(PeerSession {
        id: id.parse::<u64>().ok()?,
        sid: sid.to_string(),
        title: aterm_control::wire::pct_decode(title),
        has_meta: meta == "meta=1",
    })
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

/// Dial one sibling, read its roster (+ typed-meta follow-ups), classify.
/// A refusal, budget exhaustion, or malformed frame in the ROSTER phase skips
/// the peer silently (the `run_discovery` posture); an anomaly in the META
/// phase only STOPS the follow-ups — the roster rows still classify, with
/// title conventions carrying any escalation the typed read didn't reach.
fn summarize_peer(pid: u32, sock: &str) -> Option<InstanceRow> {
    let deadline = Instant::now() + PEER_BUDGET;
    let token = crate::proxy::read_sibling_token(sock)?;
    let stream = aterm_uds::CtlStream::connect(sock).ok()?;
    arm(&stream, deadline)?;
    (&stream)
        .write_all(format!("AUTH {token}\nsessions\n").as_bytes())
        .ok()?;
    (&stream).flush().ok()?;
    let mut reader = BufReader::new(&stream);
    let header = read_bounded_line(&stream, &mut reader, deadline)?;
    let count: usize = header.trim_end().strip_prefix("OK ")?.parse().ok()?;
    if count > MAX_SESSIONS_PER_PEER {
        // Refuse, never clamp: reading fewer lines than the peer sent would
        // leave the leftovers desyncing every later reply on this stream.
        return None;
    }
    let mut sessions: Vec<PeerSession> = Vec::with_capacity(count);
    for _ in 0..count {
        let line = read_bounded_line(&stream, &mut reader, deadline)?;
        if let Some(s) = parse_roster_line(&line) {
            sessions.push(s);
        }
    }

    // Typed-meta follow-ups on the SAME authenticated connection: only
    // sessions flagged meta=1, capped — the roster line carries no role/
    // attention, and typed escalation state is the point of the exercise.
    // Any failure here stops the phase (nothing further is read from the
    // stream, so sync no longer matters) and the rows classify as-is.
    let mut rows: Vec<SessionRow> = Vec::with_capacity(sessions.len());
    let mut lookups = 0usize;
    let mut meta_alive = true;
    for s in &sessions {
        let mut row = SessionRow {
            id: s.id,
            title: s.title.clone(),
            role: None,
            attention: None,
        };
        if meta_alive && s.has_meta && lookups < MAX_META_LOOKUPS {
            lookups += 1;
            match read_peer_meta(&stream, &mut reader, &s.sid, deadline) {
                Some(Some((user_title, role, attention))) => {
                    if let Some(t) = user_title {
                        row.title = t;
                    }
                    row.role = role;
                    row.attention = attention;
                }
                // ERR reply: one line consumed, stream still in sync.
                Some(None) => {}
                // Stream/budget anomaly: stop reading, keep what we have.
                None => meta_alive = false,
            }
        }
        rows.push(row);
    }

    let glance = crate::status_item::classify(&rows);
    Some(InstanceRow {
        pid,
        sessions: glance.sessions,
        warnings: glance.warnings.len(),
        operator: glance.operator_session.is_some(),
    })
}

/// `@<sid> meta` on the open connection. Outer `None` = stream/budget anomaly
/// (caller must stop reading); `Some(None)` = clean non-OK reply (in sync);
/// `Some(Some((user_title, role, attention)))` = parsed, each `None` if unset.
type PeerMeta = (Option<String>, Option<String>, Option<String>);
fn read_peer_meta(
    stream: &aterm_uds::CtlStream,
    reader: &mut BufReader<&aterm_uds::CtlStream>,
    sid: &str,
    deadline: Instant,
) -> Option<Option<PeerMeta>> {
    arm(stream, deadline)?;
    { stream }
        .write_all(format!("@{sid} meta\n").as_bytes())
        .ok()?;
    { stream }.flush().ok()?;
    let line = read_bounded_line(stream, reader, deadline)?;
    let line = line.trim_end();
    if !line.starts_with("OK ") {
        return Some(None);
    }
    let field = |key: &str| -> Option<String> {
        line.split_whitespace()
            .find_map(|tok| tok.strip_prefix(key))
            .filter(|v| *v != "-")
            .map(aterm_control::wire::pct_decode)
    };
    Some(Some((
        field("user_title="),
        field("role="),
        field("attention="),
    )))
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
    use super::parse_roster_line;

    /// The roster parser is tolerant (malformed ⇒ skip) and counts by the
    /// local glance's rule: an exited session is not summarized. Positional
    /// splitting keeps an EMPTY title field alive (pct_encode("") is a
    /// zero-width token a whitespace split would swallow).
    #[test]
    fn roster_lines_parse_tolerantly_and_exclude_the_dead() {
        let s = parse_roster_line("3 s-abc - alive vim%20main.rs meta=1\n").expect("well-formed");
        assert_eq!((s.id, s.sid.as_str()), (3, "s-abc"));
        assert_eq!(s.title, "vim main.rs");
        assert!(s.has_meta);
        assert!(
            parse_roster_line("4 s-dead - exited zsh meta=0").is_none(),
            "a dead session is not a fleet fact"
        );
        assert!(parse_roster_line("not a roster line").is_none());
        assert!(
            parse_roster_line("x s-a - alive t meta=0").is_none(),
            "non-numeric id skips"
        );
        let empty = parse_roster_line("5 s-e - alive  meta=1").expect("empty title survives");
        assert_eq!(empty.title, "");
        assert!(empty.has_meta);
        // Additive trailing tokens after meta= are tolerated.
        assert!(parse_roster_line("6 s-f - alive zsh meta=0 future=x").is_some());
    }

    /// A scripted fake peer over a real Unix socket serving AUTH, the
    /// `sessions` roster, and one `@sid meta` follow-up — proving the whole
    /// dial path end-to-end: typed attention travels, warnings and the
    /// operator mark land in the summarized row, exited rows are excluded.
    #[cfg(unix)]
    #[test]
    fn summarize_peer_reads_a_scripted_sibling() {
        use std::io::{BufRead, BufReader, Write};
        let dir = std::env::temp_dir().join(format!("aterm-fleet-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("peer.sock");
        let _ = std::fs::remove_file(&sock);
        // The sibling token read beside the socket (non-pid name ⇒ aterm.token).
        std::fs::write(dir.join("aterm.token"), "feedbeef\n").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "AUTH feedbeef\n");
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "sessions\n");
            let mut w = conn.try_clone().unwrap();
            w.write_all(
                b"OK 3\n\
                  0 s-op - alive operator%3A%20busy meta=1\n\
                  1 s-w - alive zsh meta=0\n\
                  2 s-x - exited gone meta=0\n",
            )
            .unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "@s-op meta\n");
            w.write_all(
                b"OK title=zsh user_title=- description=- icon=- role=operator attention=stuck%20on%20CI cwd=- state=alive\n",
            )
            .unwrap();
        });
        let row = super::summarize_peer(4242, sock.to_str().unwrap()).expect("summarized");
        server.join().unwrap();
        assert_eq!(row.pid, 4242);
        assert_eq!(row.sessions, 2, "exited row excluded");
        assert_eq!(row.warnings, 1, "typed attention travels");
        assert!(row.operator, "typed role travels");
    }
}
