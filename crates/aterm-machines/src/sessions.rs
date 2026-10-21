// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live tabs of this account's aterms, for `hello` and `tabs` (design §14.3), and the
//! check that this process runs in the account's own home ([`is_live_account`]), which
//! gates reading them.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Whether this process runs in the account's own home (not a test harness's synthetic
/// `$HOME` or private `ATERM_STATE_HOME`): only then does aterm read its tabs or retire the
/// LaunchAgent earlier builds installed (`cli::retire_lan_agent`).
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

// ── Local aterm session & tab telemetry scanner ─────────────────────────────────────────

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

/// Parse one `sessions` line from an `aterm` control socket into a [` crate::protocol::HostedSession`].
/// Exited sessions are skipped (`None`).
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
    let local_id = id_str.parse::<u64>().ok()?;
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
            "agent_since_ms" if val != "-" => s.agent_since_ms = val.parse().ok(),
            "human_ms" if val != "-" => s.human_ms = val.parse().ok(),
            "autopilot" => s.autopilot = opt(),
            "role" => s.role = opt(),
            "attention" => s.attention = opt(),
            "supervisor" => s.supervisor = opt(),
            "task" => s.task = opt(),
            "user_title" => user_title = opt(),
            "cols" if val != "-" => s.cols = val.parse().ok(),
            "rows" if val != "-" => s.rows = val.parse().ok(),
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
    let trimmed = path.trim();
    if let Some(home) = aterm_uds::account::home().or_else(aterm_types::dirs::home_dir)
        && let Some(h) = home.to_str().filter(|h| !h.is_empty())
    {
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

/// Enrich parsed `HostedSession` rows from `sock_path` with live terminal grid dimensions
/// (`@<id> dims`) and working directory (`@<id> cwd`).
fn enrich_sessions_dims_and_cwd(
    sock_path: &Path,
    token: &str,
    sessions: &mut [crate::protocol::HostedSession],
) {
    if sessions.is_empty() {
        return;
    }
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return;
    };
    let timeout = Some(Duration::from_millis(80));
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
        if reader.by_ref().take(1024).read_line(&mut dims_line).is_ok()
            && let Some(rest) = dims_line.trim().strip_prefix("OK ")
        {
            let mut parts = rest.split_whitespace();
            if let (Some(r), Some(c)) = (
                parts.next().and_then(|x| x.parse::<u32>().ok()),
                parts.next().and_then(|x| x.parse::<u32>().ok()),
            ) && c > 0
                && r > 0
            {
                s.cols = Some(c);
                s.rows = Some(r);
            }
        }
        let mut cwd_line = String::new();
        if reader.by_ref().take(4096).read_line(&mut cwd_line).is_ok()
            && let Some(rest) = cwd_line.trim().strip_prefix("OK ")
        {
            let decoded = pct_decode(rest.trim());
            if !decoded.is_empty() && decoded != "-" {
                s.cwd = Some(shorten_home(&decoded));
            }
        }
    }
}

/// How long `hello` waits on each aterm's control socket for its roster.
const SOCKET_WAIT: Duration = Duration::from_millis(400);

/// Dial one `aterm` control socket at `sock_path` and read its live `sessions` roster,
/// waiting up to `wait` for each read and write.
fn query_instance_sessions(
    instance_pid: u32,
    sock_path: &Path,
    wait: Duration,
) -> Vec<crate::protocol::HostedSession> {
    let sock_dir = match sock_path.parent() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let sock_name = match sock_path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return Vec::new(),
    };
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return Vec::new();
    };
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));

    let token = aterm_types::control_socket::token_names_for_sock(sock_name)
        .into_iter()
        .find_map(|name| {
            std::fs::read_to_string(sock_dir.join(name))
                .ok()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
        });
    let Some(token) = token else {
        return Vec::new();
    };
    if stream
        .write_all(format!("AUTH {token}\nsessions\n").as_bytes())
        .and_then(|()| stream.flush())
        .is_err()
    {
        return Vec::new();
    }
    let mut reader = BufReader::new(stream);
    let mut header = String::new();
    if reader.by_ref().take(1024).read_line(&mut header).is_err() {
        return Vec::new();
    }
    let Some(count) = header
        .trim_end()
        .strip_prefix("OK ")
        .and_then(|n| n.parse::<usize>().ok())
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
            || line.is_empty()
        {
            break;
        }
        if let Some(session) = parse_aterm_roster_line(instance_pid, &line) {
            out.push(session);
        }
    }
    if wait <= Duration::from_secs(1) {
        enrich_sessions_dims_and_cwd(sock_path, &token, &mut out);
    }
    out
}

/// Enumerate every live `aterm` control socket in `dir` and return all live tabs/sessions,
/// waiting up to `wait` on each socket.
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
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".sock") {
                continue;
            }
            if let Some(pid) = aterm_types::control_socket::instance_pid(&name) {
                remember(pid, dir.join(name));
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
            if !out.iter().any(|x: &crate::protocol::HostedSession| {
                x.instance_pid == s.instance_pid && x.local_id == s.local_id
            }) {
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

        // A hang detector, not a latency budget: a descheduled stand-in still answers.
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
    }
}
