// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live tab discovery across this account's aterm instances, gated by [`is_live_account`].

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Returns whether this process runs in the account's own home directory.
#[must_use]
pub fn is_live_account() -> bool {
    if aterm_types::dirs::runs_under_private_state_root() {
        return false;
    }
    let Some(env_home) = aterm_types::dirs::home_dir() else {
        return false;
    };
    let Some(pw_home) = aterm_uds::account::home() else {
        return false;
    };
    env_home == pw_home
}

/// Percent-decode a control-socket field (`%XX` hex pairs).
#[must_use]
pub fn pct_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push(((hi << 4) | lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses one non-exited `sessions` line from an `aterm` control socket into a [`crate::protocol::HostedSession`].
#[must_use]
pub fn parse_aterm_roster_line(
    instance_pid: u32,
    line: &str,
) -> Option<crate::protocol::HostedSession> {
    let mut f = line.trim_end_matches(['\r', '\n']).split(' ');
    let (id_str, sid, _parent, state, raw_title, _meta) = (
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
    let local_id = crate::parse_uint::<u64>(id_str)?;
    let mut s = crate::protocol::HostedSession {
        instance_pid,
        local_id,
        sid: sid.to_owned(),
        state: state.to_owned(),
        title: String::new(),
        ..crate::protocol::HostedSession::default()
    };
    let mut user_title: Option<String> = None;
    for tok in f {
        let Some((key, val)) = tok.split_once('=') else {
            continue;
        };
        let opt = || (val != "-" && !val.is_empty()).then(|| pct_decode(val));
        match key {
            "window" => s.window = val.to_owned(),
            "active" => s.active = val == "1",
            "wfocus" => s.focused = val == "1",
            "program" => s.program = opt(),
            "agent" => s.agent = opt(),
            "agent_detail" => s.agent_detail = opt(),
            "agent_since_ms" if val != "-" => s.agent_since_ms = crate::parse_uint(val),
            "program_state" => s.program_state = opt(),
            "human_ms" if val != "-" => s.human_ms = crate::parse_uint(val),
            "autopilot" => s.autopilot = opt(),
            "role" => s.role = opt(),
            "attention" => s.attention = opt(),
            "supervisor" => s.supervisor = opt(),
            "task" => s.task = opt(),
            "user_title" => user_title = opt(),
            "cols" if val != "-" => s.cols = crate::parse_uint(val),
            "rows" if val != "-" => s.rows = crate::parse_uint(val),
            "cwd" => s.cwd = opt(),
            _ => {}
        }
    }
    s.title = user_title.unwrap_or_else(|| pct_decode(raw_title));
    Some(s)
}

/// Replace the account's home directory prefix with `~` for compact display.
#[must_use]
pub fn shorten_home(path: &str) -> String {
    let home = aterm_uds::account::home().or_else(aterm_types::dirs::home_dir);
    shorten_home_with(path, home.as_deref().and_then(Path::to_str).unwrap_or(""))
}

/// [`shorten_home`] for the home directory `home`, read with or without a trailing `/`; an
/// empty home, or `/`, shortens nothing.
fn shorten_home_with(path: &str, home: &str) -> String {
    let trimmed = path.trim();
    let h = home.trim_end_matches('/');
    if !h.is_empty() {
        if trimmed == h {
            return "~".to_owned();
        }
        if let Some(rest) = trimmed.strip_prefix(h)
            && rest.starts_with('/')
        {
            return format!("~{rest}");
        }
    }
    trimmed.to_owned()
}

#[cfg(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
pub(crate) fn same_user_peer(stream: &UnixStream) -> bool {
    aterm_uds::peer::peer_uid(stream) == Some(aterm_uds::peer::our_uid())
}

#[cfg(not(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
pub(crate) fn same_user_peer(_stream: &UnixStream) -> bool {
    true
}

pub(crate) fn read_owned_text(path: &Path, max_bytes: usize) -> Option<String> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let bound = max_bytes.checked_add(1)?;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.uid() != aterm_uds::peer::our_uid() || meta.len() > max_bytes as u64
    {
        return None;
    }
    let cap = usize::try_from(meta.len())
        .unwrap_or(0)
        .min(max_bytes)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(cap);
    f.take(bound as u64).read_to_end(&mut bytes).ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

pub(crate) fn read_sock_token(sock_path: &Path) -> Option<String> {
    let sock_dir = sock_path.parent()?;
    let sock_name = sock_path.file_name()?.to_str()?;
    aterm_types::control_socket::token_names_for_sock(sock_name)
        .into_iter()
        .find_map(|name| {
            read_owned_text(&sock_dir.join(name), 4096)
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_graphic()))
        })
}

fn same_hosted_session(
    a: &crate::protocol::HostedSession,
    b: &crate::protocol::HostedSession,
) -> bool {
    (!a.sid.is_empty() && a.sid == b.sid)
        || (a.instance_pid != 0 && a.instance_pid == b.instance_pid && a.local_id == b.local_id)
}

/// Each read and write of the batch waits at most `wait`: the roster's 80 ms, or a test's
/// one-minute hang detector where the test's own server always answers.
fn enrich_sessions_dims_and_cwd(
    sock_path: &Path,
    token: &str,
    sessions: &mut [crate::protocol::HostedSession],
    wait: Duration,
) {
    if sessions.is_empty()
        || sessions
            .iter()
            .all(|s| s.cols.is_some() && s.rows.is_some() && s.cwd.is_some())
    {
        return;
    }
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return;
    };
    if !same_user_peer(&stream) {
        return;
    }
    let timeout = Some(wait);
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    let mut batch = format!("AUTH {token}\n");
    for s in sessions.iter() {
        batch.push_str(&format!("@{} dims\n@{} cwd\n", s.local_id, s.local_id));
    }
    if stream
        .write_all(batch.as_bytes())
        .and_then(|()| stream.flush())
        .is_err()
    {
        return;
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut reader = BufReader::new(stream);
    for s in sessions.iter_mut() {
        let mut dims_line = String::new();
        if reader
            .by_ref()
            .take(1024)
            .read_line(&mut dims_line)
            .is_err()
            || !dims_line.ends_with('\n')
        {
            break;
        }
        if let Some(rest) = dims_line.trim().strip_prefix("OK ") {
            let mut parts = rest.split_whitespace();
            if let (Some(r), Some(c)) = (
                parts.next().and_then(crate::parse_uint::<u32>),
                parts.next().and_then(crate::parse_uint::<u32>),
            ) && c > 0
                && r > 0
            {
                s.cols = Some(c);
                s.rows = Some(r);
            }
        }
        let mut cwd_line = String::new();
        if reader.by_ref().take(4096).read_line(&mut cwd_line).is_err() || !cwd_line.ends_with('\n')
        {
            break;
        }
        if let Some(rest) = cwd_line.trim().strip_prefix("OK ") {
            let decoded = pct_decode(rest.trim());
            if !decoded.is_empty() && decoded != "-" {
                s.cwd = Some(shorten_home(&decoded));
            }
        }
    }
}

const SOCKET_WAIT: Duration = Duration::from_millis(400);

fn query_instance_sessions(
    instance_pid: u32,
    sock_path: &Path,
    wait: Duration,
) -> Vec<crate::protocol::HostedSession> {
    let Some(token) = read_sock_token(sock_path) else {
        return Vec::new();
    };
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return Vec::new();
    };
    if !same_user_peer(&stream) {
        return Vec::new();
    }
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));
    if stream
        .write_all(format!("AUTH {token}\nsessions\n").as_bytes())
        .and_then(|()| stream.flush())
        .is_err()
    {
        return Vec::new();
    }
    let mut reader = BufReader::new(stream);
    let mut header = String::new();
    if reader.by_ref().take(1024).read_line(&mut header).is_err() || !header.ends_with('\n') {
        return Vec::new();
    }
    let Some(count) = header
        .trim_end()
        .strip_prefix("OK ")
        .and_then(crate::parse_uint::<usize>)
        .filter(|&c| c <= 1024)
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let mut line = String::new();
        if reader
            .by_ref()
            .take(64 * 1024)
            .read_line(&mut line)
            .is_err()
            || !line.ends_with('\n')
        {
            break;
        }
        if let Some(session) = parse_aterm_roster_line(instance_pid, &line) {
            out.push(session);
        }
    }
    if wait <= Duration::from_secs(1) {
        enrich_sessions_dims_and_cwd(sock_path, &token, &mut out, Duration::from_millis(80));
    }
    out
}

/// Enumerates live `aterm` control sockets in `dir` and returns all live sessions.
#[must_use]
pub fn scan_aterm_sessions_in_dir(
    dir: &Path,
    wait: Duration,
) -> Vec<crate::protocol::HostedSession> {
    let mut seen = std::collections::BTreeSet::new();
    let mut targets: Vec<(u32, PathBuf)> = Vec::new();
    let mut remember = |pid: u32, path: PathBuf| {
        let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            targets.push((pid, path));
        }
    };

    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let file_name = e.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !name.ends_with(".sock") {
                continue;
            }
            if let Some(pid) = aterm_types::control_socket::instance_pid(name) {
                remember(pid, dir.join(name));
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir.join("graph")) {
        for e in entries.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let Some(body) = read_owned_text(&e.path(), 64 * 1024) else {
                continue;
            };
            let Some(sock) = aterm_types::control_socket::graph_entry_sock(&body) else {
                continue;
            };
            let path = PathBuf::from(&sock);
            if !path.is_absolute() {
                continue;
            }
            let pid = aterm_types::control_socket::graph_entry_pid(&body).unwrap_or(0);
            remember(pid, path);
        }
    }
    targets.sort_by_key(|(pid, _)| *pid);
    let mut sessions = Vec::new();
    for (pid, sock) in targets {
        sessions.extend(query_instance_sessions(pid, &sock, wait));
    }
    sessions.sort_by_key(|s| (s.instance_pid, s.local_id));
    sessions
}

/// Enumerate every live `aterm` tab/session in this user's control-socket directory.
#[must_use]
pub fn scan_local_aterm_sessions() -> Vec<crate::protocol::HostedSession> {
    let mut dirs = Vec::new();
    if let Some(d) = aterm_uds::control_socket_dir() {
        dirs.push(d);
    }
    if let Some(home) = aterm_uds::account::home().or_else(aterm_types::dirs::home_dir) {
        let app_support = home.join("Library/Application Support/aterm");
        if !dirs.contains(&app_support) {
            dirs.push(app_support);
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        for s in scan_aterm_sessions_in_dir(&d, SOCKET_WAIT) {
            if !out.iter().any(|x| same_hosted_session(x, &s)) {
                out.push(s);
            }
        }
    }
    out.sort_by_key(|s| (s.instance_pid, s.local_id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn roster_line_and_socket_dir_scanner_capture_live_tabs_and_ai_cli_telemetry() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("at-lan-tabs-")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("aterm-4242.sock");
        std::fs::write(dir.path().join("aterm-4242.token"), "feedbeef\n").unwrap();
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "AUTH feedbeef\n");
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "sessions\n");
            let mut w = conn;
            w.write_all(
                b"OK 2\n\
                  0 s-ai - alive claude%20code meta=1 nonce=aa window=0 active=1 wfocus=1 \
                  program=claude agent=working agent_detail=thinking agent_since_ms=4200 \
                  human_ms=15000 role=operator attention=- user_title=Fleet%20Work \
                  supervisor=aterm autopilot=cruise task=build%20fleet\n\
                  1 s-dead - exited zsh meta=0\n",
            )
            .unwrap();
        });

        let rows = scan_aterm_sessions_in_dir(dir.path(), Duration::from_secs(60));
        server.join().unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.instance_pid, 4242);
        assert_eq!(r.local_id, 0);
        assert_eq!(r.sid, "s-ai");
        assert_eq!(r.title, "Fleet Work");
        assert!(r.active && r.focused);
        assert_eq!(r.program.as_deref(), Some("claude"));
        assert_eq!(r.agent.as_deref(), Some("working"));
        assert_eq!(r.agent_detail.as_deref(), Some("thinking"));
        assert_eq!(r.agent_since_ms, Some(4200));
        assert_eq!(r.human_ms, Some(15000));
        assert_eq!(r.autopilot.as_deref(), Some("cruise"));
        assert_eq!(r.role.as_deref(), Some("operator"));
        assert_eq!(r.task.as_deref(), Some("build fleet"));
        assert_eq!(r.program_state, None, "a line with no report carries none");

        let real_tok = dir.path().join("real.token");
        std::fs::write(&real_tok, "cafef00d\n").unwrap();
        let sym_sock = dir.path().join("aterm-9999.sock");
        std::os::unix::fs::symlink(&real_tok, dir.path().join("aterm-9999.token")).unwrap();
        assert_eq!(read_sock_token(&sym_sock), None);
        let injected_sock = dir.path().join("aterm-8888.sock");
        std::fs::write(dir.path().join("aterm-8888.token"), "bad\nsessions\n").unwrap();
        assert_eq!(read_sock_token(&injected_sock), None);
    }

    /// A codex's own report (`program_state=`) rides the fleet row, so another Mac's Settings can
    /// read it where the codex reader says `unknown`.
    #[test]
    fn a_roster_line_carries_the_programs_own_report() {
        let row = |state: &str| {
            parse_aterm_roster_line(
                7,
                &format!(
                    "2 s-cx - alive codex meta=1 program=codex agent=unknown program_state={state}"
                ),
            )
            .expect("a row")
            .program_state
        };
        assert_eq!(
            row("blocked:permission").as_deref(),
            Some("blocked:permission")
        );
        assert_eq!(row("-"), None);
    }

    /// BOUNDED READ FRAMING AND ZERO-PID DEDUP: an oversized `cwd` reply without `\n` stops
    /// enrichment rather than shifting its tail into the next session's `dims`, and `pid == 0`
    /// graph rows with distinct `sid`s do not collapse on `local_id`.
    /// NEGATIVE CONTROL: well-formed newline-terminated replies enrich the preceding session and
    /// duplicate `sid`s still deduplicate.
    #[test]
    fn enrich_stops_on_unterminated_reply_and_dedup_preserves_distinct_zero_pid_sids() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("at-ses-")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("aterm-77.sock");
        std::fs::write(dir.path().join("aterm-77.token"), "abcd\n").unwrap();
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let _ = conn.read_to_end(&mut req);
            let mut reply = b"OK 24 80\nOK ".to_vec();
            reply.extend(std::iter::repeat_n(b'a', 4100));
            reply.extend_from_slice(b"\nOK 50 160\nOK /Users//ok\n");
            let _ = conn.write_all(&reply);
            conn
        });

        let mut sessions = vec![
            parse_aterm_roster_line(77, "0 s-first - alive zsh meta=0").unwrap(),
            parse_aterm_roster_line(77, "1 s-second - alive zsh meta=0").unwrap(),
        ];
        // The server above always answers: a hang detector, never the roster's 80 ms.
        enrich_sessions_dims_and_cwd(&sock, "abcd", &mut sessions, Duration::from_secs(60));
        let _held = server.join().unwrap();
        assert_eq!(sessions[0].rows, Some(24));
        assert_eq!(sessions[0].cols, Some(80));
        assert_eq!(
            sessions[0].cwd, None,
            "oversized unterminated cwd must be dropped"
        );
        assert_eq!(
            sessions[1].cols, None,
            "tail of oversized cwd line must not desynchronize into session 1"
        );
        assert_eq!(sessions[1].cwd, None);

        let a = parse_aterm_roster_line(0, "0 s-one - alive zsh meta=0").unwrap();
        let b = parse_aterm_roster_line(0, "0 s-two - alive zsh meta=0").unwrap();
        let a_dup = parse_aterm_roster_line(0, "0 s-one - alive zsh meta=0").unwrap();
        assert!(!same_hosted_session(&a, &b));
        assert!(same_hosted_session(&a, &a_dup));
    }

    /// A home read with a trailing `/` shortens as the same home does without it. NEGATIVE
    /// CONTROL: a sibling that shares its prefix, and a `/` or empty home, shorten nothing.
    #[test]
    fn a_home_with_a_trailing_slash_shortens_like_the_same_home_without() {
        for home in ["/Users//alice", "/Users//alice/"] {
            assert_eq!(shorten_home_with(" /Users//alice/aterm ", home), "~/aterm");
            assert_eq!(shorten_home_with("/Users//alice", home), "~");
            assert_eq!(
                shorten_home_with("/Users//alice2/aterm", home),
                "/Users//alice2/aterm"
            );
        }
        assert_eq!(shorten_home_with("/usr/bin", "/"), "/usr/bin");
        assert_eq!(shorten_home_with("/", "/"), "/");
        assert_eq!(shorten_home_with("/usr/bin", ""), "/usr/bin");
    }
}
