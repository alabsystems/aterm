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

/// A roster line whose required fields cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnreadRosterLine;

impl std::fmt::Display for UnreadRosterLine {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("unread session roster line")
    }
}

impl std::error::Error for UnreadRosterLine {}

/// Parse a roster line without confusing an unread line with an exited session.
///
/// # Errors
/// A required field is missing or its local session id is not an unsigned integer.
pub fn parse_aterm_roster_line(
    instance_pid: u32,
    line: &str,
) -> Result<Option<crate::protocol::HostedSession>, UnreadRosterLine> {
    let mut f = line.trim_end_matches(['\r', '\n']).split(' ');
    let (Some(id_str), Some(sid), Some(_parent), Some(state), Some(raw_title), Some(_meta)) =
        (f.next(), f.next(), f.next(), f.next(), f.next(), f.next())
    else {
        return Err(UnreadRosterLine);
    };
    let Some(local_id) = crate::parse_uint::<u64>(id_str) else {
        return Err(UnreadRosterLine);
    };
    if state == "exited" {
        return Ok(None);
    }
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
    Ok(Some(s))
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
) -> crate::protocol::SessionSnapshot {
    query_instance_sessions_after(instance_pid, sock_path, wait, Duration::ZERO)
}

fn query_instance_sessions_after(
    instance_pid: u32,
    sock_path: &Path,
    wait: Duration,
    elapsed: Duration,
) -> crate::protocol::SessionSnapshot {
    let Some(token) = read_sock_token(sock_path) else {
        return crate::protocol::SessionSnapshot::default();
    };
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return crate::protocol::SessionSnapshot::default();
    };
    if !same_user_peer(&stream) {
        return crate::protocol::SessionSnapshot::default();
    }
    let remaining = wait.saturating_sub(elapsed);
    if remaining.is_zero() {
        if stream.set_nonblocking(true).is_err() {
            return crate::protocol::SessionSnapshot::default();
        }
    } else if stream.set_read_timeout(Some(remaining)).is_err()
        || stream.set_write_timeout(Some(remaining)).is_err()
    {
        return crate::protocol::SessionSnapshot::default();
    }
    if stream
        .write_all(format!("AUTH {token}\nsessions\n").as_bytes())
        .and_then(|()| stream.flush())
        .is_err()
    {
        return crate::protocol::SessionSnapshot::default();
    }
    let mut reader = BufReader::new(stream);
    let mut header = String::new();
    if reader.by_ref().take(1024).read_line(&mut header).is_err() || !header.ends_with('\n') {
        return crate::protocol::SessionSnapshot::default();
    }
    let Some(count) = header
        .trim_end()
        .strip_prefix("OK ")
        .and_then(crate::parse_uint::<usize>)
        .filter(|&c| c <= 1024)
    else {
        return crate::protocol::SessionSnapshot::default();
    };
    let mut out = Vec::with_capacity(count);
    let mut complete = true;
    for _ in 0..count {
        let mut line = String::new();
        if reader
            .by_ref()
            .take(64 * 1024)
            .read_line(&mut line)
            .is_err()
            || !line.ends_with('\n')
        {
            complete = false;
            break;
        }
        match parse_aterm_roster_line(instance_pid, &line) {
            Ok(Some(session)) => out.push(session),
            Ok(None) => {}
            Err(UnreadRosterLine) => complete = false,
        }
    }
    if wait <= Duration::from_secs(1) {
        enrich_sessions_dims_and_cwd(sock_path, &token, &mut out, Duration::from_millis(80));
    }
    crate::protocol::SessionSnapshot {
        rows: out,
        complete,
    }
}

/// Enumerate this directory's control sockets, retaining unread and partial observations.
#[must_use]
pub fn scan_aterm_sessions_in_dir(dir: &Path, wait: Duration) -> crate::protocol::SessionSnapshot {
    let mut seen = std::collections::BTreeSet::new();
    let mut targets: Vec<(u32, PathBuf)> = Vec::new();
    let mut remember = |pid: u32, path: PathBuf| {
        let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            targets.push((pid, path));
        }
    };

    let mut complete = true;
    let entries = std::fs::read_dir(dir);
    if entries
        .as_ref()
        .is_err_and(|error| error.kind() != std::io::ErrorKind::NotFound)
    {
        complete = false;
    }
    if let Ok(entries) = entries {
        for entry in entries {
            let Ok(e) = entry else {
                complete = false;
                continue;
            };
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
    let entries = std::fs::read_dir(dir.join("graph"));
    if entries
        .as_ref()
        .is_err_and(|error| error.kind() != std::io::ErrorKind::NotFound)
    {
        complete = false;
    }
    if let Ok(entries) = entries {
        for entry in entries {
            let Ok(e) = entry else {
                complete = false;
                continue;
            };
            let Ok(kind) = e.file_type() else {
                complete = false;
                continue;
            };
            if !kind.is_file() {
                continue;
            }
            let Some(body) = read_owned_text(&e.path(), 64 * 1024) else {
                complete = false;
                continue;
            };
            let Some(sock) = aterm_types::control_socket::graph_entry_sock(&body) else {
                complete = false;
                continue;
            };
            let path = PathBuf::from(&sock);
            if !path.is_absolute() {
                complete = false;
                continue;
            }
            let pid = aterm_types::control_socket::graph_entry_pid(&body).unwrap_or(0);
            remember(pid, path);
        }
    }
    targets.sort_by_key(|(pid, _)| *pid);
    let mut sessions = Vec::new();
    for (pid, sock) in targets {
        let observation = query_instance_sessions(pid, &sock, wait);
        complete &= observation.complete;
        sessions.extend(observation.rows);
    }
    sessions.sort_by_key(|s| (s.instance_pid, s.local_id));
    crate::protocol::SessionSnapshot {
        rows: sessions,
        complete,
    }
}

/// Read the account's session inventory without treating an unread instance as empty.
#[must_use]
pub fn scan_local_aterm_sessions() -> crate::protocol::SessionSnapshot {
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
    let mut complete = !dirs.is_empty();
    for d in dirs {
        let observation = scan_aterm_sessions_in_dir(&d, SOCKET_WAIT);
        complete &= observation.complete;
        for s in observation.rows {
            if !out.iter().any(|x| same_hosted_session(x, &s)) {
                out.push(s);
            }
        }
    }
    out.sort_by_key(|s| (s.instance_pid, s.local_id));
    crate::protocol::SessionSnapshot {
        rows: out,
        complete,
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
