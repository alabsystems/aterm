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
/// Reply line-count cap: a header claiming more is REFUSED (not clamped — a
/// clamp would leave unread lines desyncing every later reply).
const MAX_SESSIONS_PER_PEER: usize = 1024;
/// The lines a reply's facts carry beside its sessions, at most: an `account`
/// line for each row of every pooled CLI's roster, full, and the
/// `server_heard` lines of a vault it serves.
const MAX_FACTS_LINES: usize = aterm_agent::harness::accounts::Cli::ALL.len()
    * aterm_agent::harness::accounts::MAX_ACCOUNTS
    + MAX_HEARD_LINES;
/// The `server_heard` lines of one vault ([`MAX_FACTS_LINES`]).
#[cfg(unix)]
const MAX_HEARD_LINES: usize = aterm_secrets::server::MAX_HEARD;
/// No vault is served off unix.
#[cfg(not(unix))]
const MAX_HEARD_LINES: usize = 0;

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

/// Re-arm the socket deadline with the budget remainder before one I/O step.
fn arm(stream: &aterm_uds::CtlStream, deadline: Instant) -> Option<()> {
    let left = budget_left(deadline)?;
    stream.set_read_timeout(Some(left)).ok()?;
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
    let mut reader = BufReader::new(&stream);
    let header = read_bounded_line(&stream, &mut reader, deadline)?;
    let count: usize = header.trim_end().strip_prefix("OK ")?.parse().ok()?;
    if count > MAX_SESSIONS_PER_PEER + 1 + MAX_FACTS_LINES {
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
    /// a line that is no session row are not counted.
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

    /// A need the row says is the program's own report (`needs_by=program`) keeps the words
    /// that say so: an approval, a sign-in (the survey's `agent=wall:auth`) and a question each
    /// in their own; CONTROL: the same need with no `needs_by=` (a box a reader saw, or an older
    /// aterm's row) has none.
    #[test]
    fn a_row_the_programs_own_report_raised_says_so() {
        use crate::needs::{CODEX_APPROVAL_WORDS, CODEX_QUESTION_WORDS, CODEX_SIGN_IN_WORDS};
        let claim = |needs: &str, agent: &str, by: &str| {
            parse_session_row(&format!(
                "session local=3 sid=s-abc state=alive program=codex program_kind=agent \
                 agent={agent} agent_ms=4000 needs={needs} needs_by={by} hold_by=-"
            ))
            .and_then(|(need, _)| need)
            .expect("its need")
            .claim
        };
        assert_eq!(
            claim("approval", "prompt", "program"),
            Some(CODEX_APPROVAL_WORDS)
        );
        assert_eq!(
            claim("approval", "wall:auth", "program"),
            Some(CODEX_SIGN_IN_WORDS)
        );
        assert_eq!(
            claim("question", "question", "program"),
            Some(CODEX_QUESTION_WORDS)
        );
        assert_eq!(claim("approval", "prompt", "-"), None, "CONTROL");
    }

    /// A need the row says is a harness's `NEEDS YOU:` report (`needs_by=harness`) carries on,
    /// so Needs you lists it after the stopped work (§14.7); CONTROL: the same ask with
    /// `needs_by=-` (another owner's direct ask, or an older aterm's row) and a program's own
    /// report have stopped their agents.
    #[test]
    fn a_row_a_harness_reported_carries_on() {
        let carries_on = |by: &str| {
            parse_session_row(&format!(
                "session local=3 sid=s-abc state=alive program=claude program_kind=agent \
                 agent=busy agent_ms=4000 needs=ask needs_by={by} hold_by=-"
            ))
            .and_then(|(need, _)| need)
            .expect("its need")
            .carries_on
        };
        assert!(carries_on("harness"));
        assert!(!carries_on("-"), "CONTROL: a direct ask");
        assert!(!carries_on("program"), "CONTROL: the program's own report");
    }

    /// A scripted sibling over a real Unix socket serving AUTH and `survey
    /// facts` — the whole dial in ONE read: its live sessions are counted,
    /// its published needs travel, an exited row is not counted.
    #[cfg(unix)]
    #[test]
    fn summarize_peer_reads_a_scripted_sibling() {
        use std::io::{BufRead, BufReader, Write};
        for (needs, published) in [(" needs=ask", true), ("", false)] {
            let scratch = crate::test_scratch::short_dir("the peer's socket");
            let dir = scratch.path();
            let sock = dir.join("peer.sock");
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
                line.clear();
                assert_eq!(reader.read_line(&mut line).unwrap(), 0, "{line:?}");
            });
            let row = super::summarize_peer_within(
                4242,
                sock.to_str().unwrap(),
                aterm_spec::wait::MUST_HAPPEN,
            )
            .expect("summarized");
            server.join().unwrap();
            assert_eq!(row.pid, 4242);
            assert_eq!(row.sessions, 2, "exited row excluded");
            assert_eq!(row.needs.len(), usize::from(published), "{needs:?}");
        }
    }
}
