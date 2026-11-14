// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `setup-token-lab [a|b|c|all] [tag] [--keep]`: the lab gate of Claude Code setup-tokens in the
//! fleet vault (`docs/DESIGN-claude-setup-token-2026-11-05.md` S9, rerun with the route on in
//! S13), on a lab of its own in this tree's debug build, with no real sign-in.
//!
//! One `up` (the stub's base URL the default route, `ATERM_DEBUG_LAB_DEFAULT_ROUTE`) lays a world
//! in the lab's scratch alone: settings with no `apiKeyHelper` and no proxy, a roster row
//! ([`LABEL`], [`EMAIL`]) on the default login, a scratch vault this lab process serves on
//! loopback with the instance's home seated in it, and a store build of Claude Code in the scratch
//! home's atpkg prefix that is lane B's stand-in. The sentinel token is the stub's labelled lab
//! token for [`LABEL`] ([`sentinel`]), which the stub reads as `oat:lab-t` and no credential holds.
//!
//! - **A, the real build's frames.** The real `claude setup-token` runs on the capture's own private
//!   terminal (`aterm_agent::harness::setup_token::capture`) inside the lab's jail, which allows
//!   only loopback, its `open` the lab's recorder (which logs the URL's shape and opens nothing)
//!   and its `security` the keychain tripwire. It must reach an authorize URL the capture admits
//!   and the paste prompt; the lane's person then stops it at the prompt (no code is entered) and
//!   records what the child wrote by then in its scratch folder and secure storage.
//! - **B, the stand-in end to end.** `aterm harness accounts token lab-t` runs in a tab of the lab,
//!   its setup-token the stand-in ([`crate::local_signins::setup_token_main`]) showing 2.1.318's
//!   frames with the sentinel; then `token-run lab-t` serves the stored token to the stand-in,
//!   which compares its descriptor with the sentinel. Every place is scanned for the sentinel: the
//!   tab's scrollback and cast, the lab's whole run and socket folders (the journal, the fabric
//!   stores, aterm's logs, the scratch config folder and `~/.secrets` among them), `$TMPDIR`, and
//!   every process's argv and environment (`ps -E -ww`) while the capture holds the stand-in and
//!   while the served stand-in runs. Each scanner has a positive control: the sentinel planted in
//!   a place of its kind, found.
//! - **C, the descriptor on a real Claude Code.** The real Claude Code starts through the wrapper
//!   on the sentinel against the stub. A SessionStart hook, a stdio MCP server and the statusLine
//!   command (each this lab binary as [`PROBE_VERB`]) try every descriptor they inherited and look
//!   at their environment and their input; none may find the sentinel or the descriptor variable.
//!   The positive control is the same probe started by the lab with the sentinel on an inherited
//!   socket, in its environment and on its input, which it must find each way. A second launch,
//!   under the wrapper's debug-only spare socket
//!   (`aterm_agent::harness::accounts_token_run::SPARE_SEAM`, an unread socket holding the token
//!   left open in the session), shows whether a descriptor the session never closes reaches its
//!   children; on 2.1.318 none does, since it hands them descriptors 0 to 2 alone. The session's
//!   exec-time environment must read as the vault token's route, and the stub must have answered
//!   its turn as `oat:lab-t`. Then the crediting check (S13): the row's stored login
//!   ([`STORED`]) is laid on the default login and a session on it serves a turn under aterm's
//!   usage tap, whose windows (the stub's chosen resets) the harness folds into the row as the
//!   basis; a token session under the tap must then be credited to the row (this Mac's token
//!   table's first served reading), answered as `oat:lab-t`, and its windows, the same resets,
//!   must make verification by windows write `verified` into the token's meta.
//!
//! A verdict table, exit 1 on a FAIL, 3 when a lane could not run. The record of what each lane
//! saw goes to `setup-token-lab.txt` in the kept logs; no byte of the sentinel is printed, logged
//! or recorded (a scanner reports a path, a pid or a descriptor number).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::harness::accounts_tokens::Windows;
use aterm_agent::harness::setup_token::{self, Capture, Person, Program};
use aterm_agent::harness::wake_deadline::WakeDeadline;

use crate::codex_proof::Verdict;
use crate::door::Door;
use crate::lab::UpOpts;
use crate::local_signins as ls;
use crate::pyjson::J;
use crate::run::Run;
use crate::util::{atomic_write, scratch_text, shell_quote};
use crate::{Fail, Res, stop};

/// The roster row every lane's token is for: the default login's.
pub const LABEL: &str = "lab-t";

/// Its email: the account the token is made for.
pub const EMAIL: &str = "lab-t@example.test";

/// The label of the row's stored login lane C's crediting check lays on the default login
/// ([`crate::stub::labelled_oauth_token`]): the stub answers its turns as `oat:lab-s`.
pub const STORED: &str = "lab-s";

/// The hidden verb a lane C probe runs as: `setup-token-probe <hook|mcp|statusline> <root>`.
pub const PROBE_VERB: &str = "setup-token-probe";

/// A wait for something that must happen: the hang detector.
const HANG: Duration = Duration::from_secs(60);

/// The longest file a tree scan reads; a longer one is read in its first part and counted.
const SCAN_FILE_MAX: u64 = 64 << 20;

/// The most entries a tree scan walks before it says it stopped.
const SCAN_ENTRIES_MAX: usize = 400_000;

/// The lab token for [`LABEL`] ([`crate::stub::labelled_oauth_token`]): what every lane makes,
/// stores and serves, and no credential anywhere.
#[must_use]
pub fn sentinel() -> String {
    crate::stub::labelled_oauth_token(LABEL)
}

/// What every scanner looks for: the sentinel's head through its label's end mark, which only the
/// sentinel holds (the stub's log names it `oat:lab-t`, never by its bytes).
#[must_use]
pub fn needle() -> String {
    format!("{}{LABEL}-LAB", crate::stub::LABELLED_OAUTH)
}

/// Whether `hay` holds `needle`.
#[must_use]
pub fn holds(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

// ---------------------------------------------------------------------------------------------
// The scanners.

/// What a tree scan saw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeScan {
    /// Regular files read.
    pub files: usize,
    /// The files that hold the needle.
    pub hits: Vec<PathBuf>,
    /// Entries it could not read (refused, gone).
    pub unread: usize,
    /// Whether it stopped at [`SCAN_ENTRIES_MAX`].
    pub cut: bool,
}

/// Scan `root` for `needle`: every regular file under it, links not followed, `skip`'s subtrees
/// left out, and only files changed at `since` or later where it is given. A file is read in
/// pieces that overlap by the needle's length, up to [`SCAN_FILE_MAX`].
#[must_use]
pub fn scan_tree(
    root: &Path,
    needle: &[u8],
    since: Option<SystemTime>,
    skip: &[PathBuf],
) -> TreeScan {
    use std::io::Read as _;
    let mut out = TreeScan::default();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            out.unread += 1;
            continue;
        };
        for entry in entries {
            seen += 1;
            if seen > SCAN_ENTRIES_MAX {
                out.cut = true;
                return out;
            }
            let Ok(entry) = entry else {
                out.unread += 1;
                continue;
            };
            let path = entry.path();
            if skip.iter().any(|s| path.starts_with(s)) {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                out.unread += 1;
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            if since.is_some_and(|since| meta.modified().is_ok_and(|m| m < since)) {
                continue;
            }
            let Ok(file) = std::fs::File::open(&path) else {
                out.unread += 1;
                continue;
            };
            out.files += 1;
            let mut reader = file.take(SCAN_FILE_MAX);
            let mut buf = vec![0u8; 1 << 20];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                let Ok(n) = reader.read(&mut buf) else {
                    out.unread += 1;
                    break;
                };
                if n == 0 {
                    break;
                }
                carry.extend_from_slice(&buf[..n]);
                if holds(&carry, needle) {
                    out.hits.push(path.clone());
                    break;
                }
                let keep = needle.len().saturating_sub(1).min(carry.len());
                carry.drain(..carry.len() - keep);
            }
        }
    }
    out.hits.sort();
    out
}

/// The pids of `ps -A -E -ww`'s rows (`text`, each `<pid> <command and environment>`) that hold
/// `needle`.
#[must_use]
pub fn ps_hits(text: &str, needle: &str) -> Vec<i32> {
    text.lines()
        .filter(|l| l.contains(needle))
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .collect()
}

/// Every process's argv and environment as `ps -A -E -ww` shows them, scanned for `needle`: the
/// pids that hold it, and how many rows were read.
///
/// # Errors
/// `ps` could not run.
pub fn scan_ps(needle: &str) -> Result<(Vec<i32>, usize), String> {
    let out = aterm_uds::spawnfd::command("/bin/ps")
        .args(["-A", "-E", "-ww", "-o", "pid=,command="])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| format!("ps: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok((ps_hits(&text, needle), text.lines().count()))
}

/// The scrollback and screen of tab `sid`, row by absolute row (`line <n>` from row 0 until
/// `out of range`; `lines` counts only the history above the screen, so a short tab's is 0),
/// scanned for `needle`: the rows that hold it, and how many rows were read (an evicted row is
/// neither).
fn scan_scrollback(door: &Door<'_>, sid: &str, needle: &str) -> (Vec<usize>, usize) {
    let at = format!("@{sid}");
    let mut hits = Vec::new();
    let mut read = 0;
    for n in 0..SCROLLBACK_ROWS_MAX {
        let reply = door.out(&[&at, "line", &n.to_string()]);
        if reply.starts_with("ERR out of range")
            || !(reply.starts_with("OK") || reply.starts_with("ERR evicted"))
        {
            break;
        }
        if reply.starts_with("OK") {
            read += 1;
            if reply.contains(needle) {
                hits.push(n);
            }
        }
    }
    (hits, read)
}

/// The most rows [`scan_scrollback`] reads from one tab: past any lane's history.
const SCROLLBACK_ROWS_MAX: usize = 20_000;

/// `$TMPDIR` with its links resolved (`/var` is a link to `/private/var`): the plant refuses a
/// path through a link, and the scan and the plant name the same folder.
fn temp_root() -> PathBuf {
    let tmp = std::env::temp_dir();
    std::fs::canonicalize(&tmp).unwrap_or(tmp)
}

/// Whether tab `sid`'s recording (`cast`) holds `needle`, and its size.
fn scan_cast(door: &Door<'_>, sid: &str, needle: &str) -> (bool, usize) {
    let cast = door
        .ctl(&[&format!("@{sid}"), "cast"])
        .map(|r| r.stdout)
        .unwrap_or_default();
    (cast.contains(needle), cast.len())
}

// ---------------------------------------------------------------------------------------------
// The probe: lane C's SessionStart hook, stdio MCP server and statusLine command.

/// The file in the probes' root naming the folder the current launch's reports go to.
pub const PROBE_CURRENT: &str = "current";

/// What one probe found, as its report says it ([`ProbeReport::render`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeReport {
    /// `hook`, `mcp` or `statusline`.
    pub role: String,
    /// Its pid.
    pub pid: i32,
    /// Each descriptor it inherited, with its kind (`3:sock`).
    pub fds: Vec<String>,
    /// The descriptors (3 and up) it read anything from, with the bytes read (`3:107`).
    pub read: Vec<String>,
    /// The descriptor whose bytes held the sentinel, if one did.
    pub found_fd: Option<i32>,
    /// Whether an environment value held the sentinel.
    pub found_env: bool,
    /// Whether what it was handed on its stdin held the sentinel (the hook's and the statusLine
    /// command's JSON, the MCP server's requests): the one channel Claude Code passes its
    /// children besides the environment (the lab saw it pass them descriptors 0 to 2 alone).
    pub found_input: bool,
    /// Whether `CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` was in its environment.
    pub descriptor_env: bool,
    /// The marker's value (`ATERM_CLAUDE_TOKEN`, an entry's name), if it had one.
    pub marker: Option<String>,
    /// The top-level keys of what the statusLine command was handed (its stdin's JSON).
    pub input_keys: Vec<String>,
}

impl ProbeReport {
    /// Its text: one `key=value` line each.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "role={}\npid={}\nfds={}\nread={}\nfound_fd={}\nfound_env={}\nfound_input={}\n\
             descriptor_env={}\nmarker={}\ninput_keys={}\n",
            self.role,
            self.pid,
            self.fds.join(","),
            self.read.join(","),
            self.found_fd
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            u8::from(self.found_env),
            u8::from(self.found_input),
            u8::from(self.descriptor_env),
            self.marker.as_deref().unwrap_or("-"),
            self.input_keys.join(","),
        )
    }

    /// The report `text` says, where it says a role.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let get = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .unwrap_or("")
        };
        let list = |key: &str| {
            get(key)
                .split(',')
                .filter(|w| !w.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let role = get("role");
        if role.is_empty() {
            return None;
        }
        Some(Self {
            role: role.to_string(),
            pid: get("pid").parse().unwrap_or(0),
            fds: list("fds"),
            read: list("read"),
            found_fd: get("found_fd").parse().ok(),
            found_env: get("found_env") == "1",
            found_input: get("found_input") == "1",
            descriptor_env: get("descriptor_env") == "1",
            marker: Some(get("marker"))
                .filter(|m| !m.is_empty() && *m != "-")
                .map(str::to_string),
            input_keys: list("input_keys"),
        })
    }

    /// Whether it found the sentinel anywhere.
    #[must_use]
    pub const fn found(&self) -> bool {
        self.found_fd.is_some() || self.found_env || self.found_input
    }
}

/// What a probe of `role` sees of this process: every inherited descriptor, each one 3 and up
/// read without taking anything ([`crate::sys::peek_fd`]) and looked through for `needle`, and the
/// environment.
#[must_use]
pub fn probe_self(role: &str, needle: &[u8]) -> ProbeReport {
    let mut report = ProbeReport {
        role: role.to_string(),
        pid: i32::try_from(std::process::id()).unwrap_or(0),
        ..ProbeReport::default()
    };
    let mut buf = vec![0u8; 64 * 1024];
    for (fd, kind) in crate::sys::open_fds(1024) {
        report.fds.push(format!("{fd}:{}", kind.word()));
        if fd < 3 {
            continue;
        }
        let n = crate::sys::peek_fd(fd, kind, &mut buf);
        if n > 0 {
            report.read.push(format!("{fd}:{n}"));
            if report.found_fd.is_none() && holds(&buf[..n], needle) {
                report.found_fd = Some(fd);
            }
        }
        buf[..n].fill(0);
    }
    for (name, value) in std::env::vars_os() {
        if holds(value.as_encoded_bytes(), needle) {
            report.found_env = true;
        }
        let name = name.to_string_lossy();
        if name == aterm_agent::harness::token_route::DESCRIPTOR_ENV {
            report.descriptor_env = true;
        }
        if name == aterm_agent::harness::token_route::MARKER_ENV {
            report.marker = Some(value.to_string_lossy().into_owned());
        }
    }
    report
}

/// `setup-token-probe <hook|mcp|statusline> <root>`: look at this process ([`probe_self`]) and,
/// for the hook and the statusLine command, at all of its stdin, write the report into the folder
/// `<root>/current` names, then do the role's work: a hook ends, the statusLine command prints
/// one line (its input's top-level keys recorded), the MCP server answers `initialize`,
/// `tools/list` and `ping` on its stdio until its input ends, its report rewritten if a request
/// holds the sentinel.
#[must_use]
pub fn probe_main(args: &[String]) -> i32 {
    use std::io::{BufRead as _, Read as _, Write as _};
    let [role, root, ..] = args else {
        eprintln!("usage: aterm-lab {PROBE_VERB} <hook|mcp|statusline> <root>");
        return 2;
    };
    let whole = needle();
    let mut report = probe_self(role, whole.as_bytes());
    if role != "mcp" {
        let mut input = Vec::new();
        let _ = std::io::stdin().take(1 << 20).read_to_end(&mut input);
        report.found_input = holds(&input, whole.as_bytes());
        if role == "statusline"
            && let Ok(J::Obj(map)) = J::parse(&String::from_utf8_lossy(&input))
        {
            report.input_keys = map.into_iter().map(|(k, _)| k).collect();
        }
    }
    let root = Path::new(root);
    let word = crate::util::read_regular_text(&root.join(PROBE_CURRENT)).unwrap_or_default();
    let dir = root.join(word.trim());
    let path = dir.join(format!("{role}-{}.txt", report.pid));
    let write = |report: &ProbeReport| {
        if !word.trim().is_empty() && crate::util::create_private_dir_all(&dir).is_ok() {
            let _ = atomic_write(&path, report.render().as_bytes());
        }
    };
    write(&report);
    match role.as_str() {
        "statusline" => {
            println!("lab probe");
            0
        }
        "mcp" => {
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout().lock();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if !report.found_input && holds(line.as_bytes(), whole.as_bytes()) {
                    report.found_input = true;
                    write(&report);
                }
                if let Some(reply) = mcp_reply(&line) {
                    let _ = writeln!(stdout, "{reply}");
                    let _ = stdout.flush();
                }
            }
            0
        }
        _ => 0,
    }
}

/// The MCP server's answer to one JSON-RPC line: `initialize` (its own protocol version echoed),
/// `tools/list` (none), `ping`, and an error for any other request; nothing for a notification
/// or a line that is no request.
#[must_use]
pub fn mcp_reply(line: &str) -> Option<String> {
    let req = J::parse(line).ok()?;
    let id = req.get("id")?.clone();
    let method = req.get("method").and_then(J::as_str).unwrap_or("");
    let result = match method {
        "initialize" => J::obj([
            (
                "protocolVersion",
                req.get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .cloned()
                    .unwrap_or_else(|| J::s("2024-11-05")),
            ),
            ("capabilities", J::obj([("tools", J::Obj(Vec::new()))])),
            (
                "serverInfo",
                J::obj([("name", J::s("lab-probe")), ("version", J::s("1"))]),
            ),
        ]),
        "tools/list" => J::obj([("tools", J::Arr(Vec::new()))]),
        "ping" => J::Obj(Vec::new()),
        _ => {
            return Some(
                J::obj([
                    ("jsonrpc", J::s("2.0")),
                    ("id", id),
                    (
                        "error",
                        J::obj([
                            ("code", J::Int(-32601)),
                            ("message", J::s("no such method")),
                        ]),
                    ),
                ])
                .dump(crate::pyjson::COMPACT),
            );
        }
    };
    Some(
        J::obj([("jsonrpc", J::s("2.0")), ("id", id), ("result", result)])
            .dump(crate::pyjson::COMPACT),
    )
}

/// The reports in `dir` ([`ProbeReport::parse`]), in name order.
#[must_use]
pub fn reports_in(dir: &Path) -> Vec<ProbeReport> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|it| it.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    names
        .iter()
        .filter_map(|p| ProbeReport::parse(&scratch_text(p)))
        .collect()
}

/// The roles lane C waits for, each of which must report.
pub const ROLES: [&str; 3] = ["hook", "mcp", "statusline"];

/// Lane C's verdict on one launch's `reports`: every role reported and none found the sentinel
/// (in a descriptor, its environment or its input) or inherited the descriptor variable. The
/// words say why.
#[must_use]
pub fn judge_children(reports: &[ProbeReport]) -> (Verdict, String) {
    let missing: Vec<&str> = ROLES
        .iter()
        .copied()
        .filter(|role| !reports.iter().any(|r| r.role == *role))
        .collect();
    if !missing.is_empty() {
        return (
            Verdict::Unproven,
            format!("no report from {}", missing.join(", ")),
        );
    }
    let found: Vec<String> = reports
        .iter()
        .filter(|r| r.found())
        .map(|r| {
            let mut wheres = Vec::new();
            if let Some(fd) = r.found_fd {
                wheres.push(format!("descriptor {fd}"));
            }
            if r.found_env {
                wheres.push("its environment".to_string());
            }
            if r.found_input {
                wheres.push("its input".to_string());
            }
            format!("{} (pid {}: {})", r.role, r.pid, wheres.join(", "))
        })
        .collect();
    let with_var: Vec<&str> = reports
        .iter()
        .filter(|r| r.descriptor_env)
        .map(|r| r.role.as_str())
        .collect();
    if found.is_empty() && with_var.is_empty() {
        (
            Verdict::Ok,
            format!(
                "{} reports, none holds the token or the descriptor variable",
                reports.len()
            ),
        )
    } else {
        let mut why = Vec::new();
        if !found.is_empty() {
            why.push(format!("the token found by {}", found.join("; ")));
        }
        if !with_var.is_empty() {
            why.push(format!(
                "the descriptor variable inherited by {}",
                with_var.join(", ")
            ));
        }
        (Verdict::Fail, why.join("; "))
    }
}

/// Lane C's positive control on the probe the lab ran itself (`report`, the one in `<probes>/
/// control`), handed the sentinel three ways: a socket at descriptor `fd`, an environment value
/// and its stdin. Each of its three scanners must have found it there.
#[must_use]
pub fn judge_control(report: Option<&ProbeReport>, fd: i32) -> (Verdict, String) {
    let Some(r) = report else {
        return (
            Verdict::Unproven,
            "the control probe wrote no report".to_string(),
        );
    };
    let mut missed = Vec::new();
    if r.found_fd != Some(fd) {
        missed.push(format!("the socket at descriptor {fd}"));
    }
    if !r.found_env {
        missed.push("the environment value".to_string());
    }
    if !r.found_input {
        missed.push("its input".to_string());
    }
    if missed.is_empty() {
        (
            Verdict::Ok,
            format!("found at descriptor {fd}, in its environment and in its input"),
        )
    } else {
        (Verdict::Fail, format!("missed {}", missed.join(", ")))
    }
}

/// `lsof -F ft`'s fields (`text`) as one process's numbered descriptors with their types
/// (`3:unix`); `cwd`, `txt` and the other named rows are left out.
#[must_use]
pub fn lsof_table(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fd: Option<&str> = None;
    for line in text.lines() {
        if let Some(n) = line.strip_prefix('f') {
            fd = n
                .bytes()
                .all(|b| b.is_ascii_digit())
                .then_some(n)
                .filter(|n| !n.is_empty());
        } else if let (Some(t), Some(n)) = (line.strip_prefix('t'), fd.take()) {
            out.push(format!("{n}:{t}"));
        }
    }
    out
}

/// Process `pid`'s descriptors as `lsof` lists them ([`lsof_table`]); empty where it could not run.
fn descriptor_table(pid: i32) -> Vec<String> {
    aterm_uds::spawnfd::command("/usr/sbin/lsof")
        .args(["-a", "-n", "-P", "-F", "ft", "-p", &pid.to_string()])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map(|out| lsof_table(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// The world.

/// What the lanes lay in the lab's scratch and serve.
struct World {
    /// The instance's home.
    home: PathBuf,
    /// The store build that is lane B's stand-in: `<prefix>/store/claude/9/bin/claude`.
    build: PathBuf,
    /// The stand-in's folder ([`crate::local_signins::SETUP_TOKEN_LABEL`] and its handshakes).
    standin: PathBuf,
    /// The probes' root ([`PROBE_CURRENT`]).
    probes: PathBuf,
    /// The account's lab key.
    key: aterm_secrets::claude_token::Key,
    /// The instance home's vault.
    vault: aterm_secrets::client::Paths,
    /// The vault's server, on loopback, for as long as the lanes run.
    _server: aterm_secrets::server::Running,
}

fn die(what: &str, e: impl std::fmt::Display) -> Fail {
    Fail::die(format!("setup-token-lab: {what}: {e}"))
}

/// The account's lab key, as the harness keys it ([`aterm_agent::harness::journal::claude_email_key`]).
fn account_key() -> Option<aterm_secrets::claude_token::Key> {
    let key = aterm_agent::harness::journal::with_keyer(
        |realm, email| aterm_db::typed::keys::account_pk(realm, email).ok(),
        || aterm_agent::harness::journal::claude_email_key(EMAIL),
    )?;
    aterm_secrets::claude_token::Key::parse(key.as_str())
}

impl World {
    /// Lay the world in `run`'s scratch (nothing outside it is written): the settings, the roster,
    /// the vault and its server, the stand-in's build, twin and folder, and the probes' root.
    fn lay(run: &Run) -> Result<World, Fail> {
        let home = run.w.join("home");
        let lab = std::env::current_exe().map_err(|e| die("this lab binary", e))?;
        let mut env = J::Obj(Vec::new());
        for k in crate::lab::PROXY_KEYS {
            env.set(k, J::Null);
        }
        env.set("ANTHROPIC_BASE_URL", J::s(crate::lab::stub_base_url(run)));
        env.set("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", J::s("1"));
        crate::verbs::settings_merge(run, &J::obj([("apiKeyHelper", J::Null), ("env", env)]))?;
        let roster = run
            .w
            .join("cfg/aterm")
            .join(aterm_agent::harness::accounts::ACCOUNTS_FILE);
        put(
            &roster,
            &format!(
                "[[account]]\nlabel = \"{LABEL}\"\nclass = \"dedicated\"\nemail = \"{EMAIL}\"\n"
            ),
        )?;
        let standin = run.w.join("setup-token-standin");
        put(&standin.join(ls::SETUP_TOKEN_LABEL), &format!("{LABEL}\n"))?;
        let prefix = atpkg::store::default_prefix(&home);
        let build = prefix.join("store/claude/9/bin/claude");
        put_exec(
            &build,
            &format!(
                "#!/bin/sh\nexec {} {} {} \"$@\"\n",
                shell_quote(&lab.to_string_lossy()),
                ls::SETUP_TOKEN_VERB,
                shell_quote(&standin.to_string_lossy())
            ),
        )?;
        put_exec(
            &prefix.join("agents/claude"),
            &format!("#!/bin/sh\nexec '{}' \"$@\"\n", build.display()),
        )?;
        let probes = run.w.join("setup-token-probes");
        crate::util::create_private_dir_all(&probes).map_err(|e| die("the probes' root", e))?;
        let key = account_key().ok_or_else(|| die("the account", "it cannot be keyed"))?;
        let (vault, server) = seat_scratch_vault(&run.w.join("vault-server"), &home)
            .map_err(|why| Fail::die(format!("setup-token-lab: {why}")))?;
        Ok(World {
            home,
            build,
            standin,
            probes,
            key,
            vault,
            _server: server,
        })
    }
}

/// The scratch vault of `home`, created and seated on a server this process runs on loopback
/// with its store under `root`, as the harness's own tests make one: this Mac its first member
/// and admin, its roster and genesis seeded and its log mirrored. The server runs for as long
/// as the caller keeps it.
/// # Errors
/// What could not be made, in words.
pub(crate) fn seat_scratch_vault(
    root: &Path,
    home: &Path,
) -> Result<(aterm_secrets::client::Paths, aterm_secrets::server::Running), String> {
    let said = |what: &str, e: &dyn std::fmt::Display| format!("{what}: {e}");
    crate::util::create_private_dir_all(root).map_err(|e| said("the vault server's folder", &e))?;
    let socket =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| said("the vault's port", &e))?;
    let server = aterm_secrets::server::start(
        socket,
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0"
                .parse()
                .map_err(|e| said("the vault's address", &e))?,
            store: root.join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(50),
            keep_awake: false,
        },
    )
    .map_err(|e| said("the vault server", &e))?;
    let url = format!("http://{}/v1/lab", server.addr);
    let paths = aterm_secrets::client::Paths::for_home(home);
    aterm_secrets::client::create(&paths, &url, None, Some("lab-mac"))
        .map_err(|e| said("the vault", &e))?;
    aterm_secrets::client::admin_seed_swarm_roster(&paths, &[])
        .map_err(|e| said("its roster", &e))?;
    aterm_secrets::client::seed_vault_genesis(&paths, &root.join("lab.genesis"))
        .map_err(|e| said("its genesis", &e))?;
    aterm_secrets::client::admin_mirror_log(&paths, &[]).map_err(|e| said("its log", &e))?;
    let route = aterm_secrets::client::write_route(&paths, Instant::now() + HANG)
        .map_err(|e| said("its write route", &e))?;
    if route != aterm_secrets::client::WriteRoute::Seated {
        return Err(format!("the vault: this Mac is not seated ({route:?})"));
    }
    Ok((paths, server))
}

fn put(path: &Path, text: &str) -> Result<(), Fail> {
    if let Some(parent) = path.parent() {
        crate::util::create_private_dir_all(parent)
            .map_err(|e| die(&parent.display().to_string(), e))?;
    }
    atomic_write(path, text.as_bytes()).map_err(|e| die(&path.display().to_string(), e))
}

fn put_exec(path: &Path, text: &str) -> Result<(), Fail> {
    if let Some(parent) = path.parent() {
        crate::util::create_private_dir_all(parent)
            .map_err(|e| die(&parent.display().to_string(), e))?;
    }
    crate::util::atomic_write_mode(path, text.as_bytes(), 0o755)
        .map_err(|e| die(&path.display().to_string(), e))
}

/// Waits, within [`HANG`], for `path` to be there.
fn wait_for(path: &Path) -> Result<bool, Fail> {
    let until = Instant::now() + HANG;
    while Instant::now() < until {
        if path.exists() {
            return Ok(true);
        }
        stop::sleep(Duration::from_millis(100))?;
    }
    Ok(false)
}

/// Waits, within [`HANG`], for `done` to hold.
fn wait_until(mut done: impl FnMut() -> bool) -> Result<bool, Fail> {
    let until = Instant::now() + HANG;
    while Instant::now() < until {
        if done() {
            return Ok(true);
        }
        stop::sleep(Duration::from_millis(200))?;
    }
    Ok(done())
}

/// One row of the verdict table, and what the record says of it.
struct Row {
    lane: char,
    verdict: Verdict,
    what: String,
}

/// What every lane notes for the record.
#[derive(Default)]
struct Record {
    rows: Vec<Row>,
    lines: Vec<String>,
}

impl Record {
    fn check(&mut self, lane: char, verdict: Verdict, what: impl Into<String>) {
        self.rows.push(Row {
            lane,
            verdict,
            what: what.into(),
        });
    }

    fn ok(&mut self, lane: char, ok: bool, what: impl Into<String>) {
        self.check(lane, if ok { Verdict::Ok } else { Verdict::Fail }, what);
    }

    fn saw(&mut self, line: impl Into<String>) {
        let line = line.into();
        println!("  {line}");
        self.lines.push(line);
    }
}

// ---------------------------------------------------------------------------------------------
// Lane A: the real build's frames.

/// Lane A's person: it reads what the capture says, and at the paste prompt lists the child's
/// scratch folder and stops the capture (Ctrl-C), entering no code.
struct StopAtPrompt {
    store: PathBuf,
    said: Mutex<Vec<String>>,
    asked: AtomicBool,
    prompt_ok: AtomicBool,
    listing: Mutex<Vec<String>>,
    at: Mutex<Option<Duration>>,
    started: Instant,
}

impl Person for StopAtPrompt {
    fn say(&self, line: &str) {
        if let Ok(mut said) = self.said.lock() {
            said.push(line.to_string());
        }
    }

    fn paste(
        &self,
        prompt: &str,
        _cancelled: &AtomicBool,
    ) -> std::io::Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
        self.prompt_ok
            .store(prompt == setup_token::PASTE_PROMPT, Ordering::SeqCst);
        if let Ok(mut at) = self.at.lock() {
            *at = Some(self.started.elapsed());
        }
        if let Ok(mut listing) = self.listing.lock() {
            *listing = scratch_listing(&self.store);
        }
        self.asked.store(true, Ordering::SeqCst);
        Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
    }

    fn stopped(&self) -> bool {
        false
    }
}

/// Every file under the capture's scratch folders in `store` (`setup-token-<pid>-<birth>/…`): its
/// path below the folder and its size, never its bytes.
#[must_use]
pub fn scratch_listing(store: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = std::fs::read_dir(store)
        .map(|it| {
            it.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(setup_token::SCRATCH_PREFIX))
                })
                .collect()
        })
        .unwrap_or_default();
    let roots = stack.clone();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = roots
                .iter()
                .find_map(|r| path.strip_prefix(r).ok())
                .map_or_else(|| path.display().to_string(), |p| p.display().to_string());
            out.push(format!("{rel} ({} B)", meta.len()));
        }
    }
    out.sort();
    out
}

/// How long lane A's own look at the frames runs ([`frames_of`]).
const FRAMES_FOR: Duration = Duration::from_secs(20);

/// What `program` shows on a terminal of the capture's size in its first `secs`, cleaned as the
/// capture cleans it ([`setup_token::Cleaner`]), for the record: each distinct line once, a URL's
/// line as its shape ([`crate::recorder::url_shape`]), a line naming a token dropped. The child
/// runs with the capture's own environment on a scratch folder in `store`, and its group is
/// killed at the end; nothing is typed to it.
fn frames_of(
    program: &Program,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    store: &Path,
    secs: Duration,
) -> Result<Vec<String>, String> {
    use std::io::Read as _;
    let scratch = setup_token::Scratch::make(store).map_err(|e| format!("its scratch: {e}"))?;
    let var = |name: &str| {
        env.iter()
            .find(|(k, _)| k.to_str() == Some(name))
            .map(|(_, v)| v.clone())
    };
    let child_env = setup_token::child_env(&var, scratch.path());
    let (master, slave, mut child) = aterm_pty::spawn_session_leader_on_pty_sized(
        setup_token::ROWS,
        setup_token::COLS,
        |slave| {
            let tty = || slave.try_clone().map(std::process::Stdio::from);
            #[expect(
                clippy::disallowed_types,
                reason = "the controlling-terminal spawn seam takes StdCommand, as the capture's \
                          own spawn does"
            )]
            let mut cmd = std::process::Command::new(&program.path);
            cmd.env_clear()
                .envs(child_env.iter().map(|(k, v)| (k, v)))
                .args(&program.args)
                .current_dir(scratch.path())
                .stdout(tty()?)
                .stderr(tty()?);
            Ok(cmd)
        },
    )
    .map_err(|e| format!("spawn: {e}"))?;
    drop(slave);
    let mut master = std::fs::File::from(master);
    let reader = std::thread::spawn(move || {
        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(n) = master.read(&mut chunk) {
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..n]);
        }
        raw
    });
    std::thread::sleep(secs);
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = aterm_uds::process::kill_group_until_empty(pid);
    }
    let _ = child.kill();
    let _ = child.wait();
    let raw = reader.join().unwrap_or_default();
    let escapes = escape_kinds(&raw);
    let mut cleaner = setup_token::Cleaner::default();
    let mut out = Vec::with_capacity(setup_token::BUFFER);
    let _ = cleaner.feed(&raw, &mut out);
    let mut lines: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&out).lines() {
        let line = line.trim();
        if line.is_empty() || holds(line.as_bytes(), b"sk-ant-oat") {
            continue;
        }
        let line = match line.find("https://") {
            Some(at) => {
                crate::recorder::url_shape(line[at..].split_whitespace().next().unwrap_or(""))
                    .map_or_else(
                        || "<a URL>".to_string(),
                        |shape| format!("{}<{shape}>", &line[..at]),
                    )
            }
            None => line.to_string(),
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines.insert(0, format!("{} bytes; escapes {escapes}", raw.len()));
    Ok(lines)
}

/// The escape sequences `raw` holds, by kind, most first: `CSI <final>` (with its parameters where
/// they are few, as `CSI 1C`), `OSC <n>` and `ESC <byte>`, each with its count.
#[must_use]
pub fn escape_kinds(raw: &[u8]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut bump = |kind: String| match counts.iter_mut().find(|(k, _)| *k == kind) {
        Some((_, n)) => *n += 1,
        None => counts.push((kind, 1)),
    };
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != 0x1b || i + 1 >= raw.len() {
            i += 1;
            continue;
        }
        match raw[i + 1] {
            b'[' => {
                let start = i + 2;
                let mut j = start;
                while j < raw.len() && !(0x40..=0x7e).contains(&raw[j]) {
                    j += 1;
                }
                let params = String::from_utf8_lossy(&raw[start..j.min(raw.len())]).into_owned();
                let fin = raw.get(j).map_or('?', |b| char::from(*b));
                bump(if params.len() <= 3 && fin != 'm' {
                    format!("CSI {params}{fin}")
                } else {
                    format!("CSI {fin}")
                });
                i = j + 1;
            }
            b']' => {
                let start = i + 2;
                let mut j = start;
                while j < raw.len() && raw[j].is_ascii_digit() {
                    j += 1;
                }
                bump(format!("OSC {}", String::from_utf8_lossy(&raw[start..j])));
                while j < raw.len()
                    && raw[j] != 0x07
                    && !(raw[j] == 0x1b && raw.get(j + 1) == Some(&b'\\'))
                {
                    j += 1;
                }
                i = j + 1;
            }
            other => {
                bump(format!("ESC {}", char::from(other)));
                i += 2;
            }
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
        .iter()
        .take(16)
        .map(|(k, n)| format!("{k}×{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Lane A's verdict: the capture ended `Stopped` at the paste prompt, having told the person the
/// sign-in opened on a host it admits; anything else, with why.
#[must_use]
pub fn judge_frames(
    result: &Result<setup_token::Minted, setup_token::Error>,
    asked: bool,
    prompt_ok: bool,
    said: &[String],
) -> (Verdict, String) {
    let told = said
        .iter()
        .find(|l| l.starts_with("a browser window opened on "));
    match result {
        Ok(_) => (
            Verdict::Fail,
            "a token was taken though no code was entered".to_string(),
        ),
        Err(setup_token::Error::Stopped) if asked && prompt_ok && told.is_some() => (
            Verdict::Ok,
            format!(
                "the URL was admitted ({}) and the paste prompt read; stopped there",
                told.map(|l| l.split(':').next().unwrap_or(l))
                    .unwrap_or_default()
            ),
        ),
        Err(setup_token::Error::Stopped) => (
            Verdict::Fail,
            format!(
                "stopped, but asked={asked} prompt={prompt_ok} told={}",
                told.is_some()
            ),
        ),
        Err(e) => (Verdict::Fail, format!("the capture ended: {e:?}")),
    }
}

fn lane_a(run: &Run, rec: &mut Record) -> Result<(), Fail> {
    println!("== lane A: the real `claude setup-token` on the capture's terminal, in the jail");
    crate::procs::need_witness(run).map_err(|e| die("the egress witness", e))?;
    let store = run.w.join("setup-token-a");
    crate::util::create_private_dir_all(&store).map_err(|e| die("lane A's store", e))?;
    let (open_log, shape_log, security_log) = (
        run.w.join("setup-token-open.log"),
        run.w.join("setup-token-open-shape.log"),
        run.w.join("setup-token-security.log"),
    );
    let shim_path = format!(
        "PATH={}:{}",
        run.run.join("shim").display(),
        setup_token::CHILD_PATH
    );
    let program = Program {
        path: PathBuf::from("/usr/bin/sandbox-exec"),
        args: [
            "-f".to_string(),
            run.run.join("jail.sb").to_string_lossy().into_owned(),
            "/usr/bin/env".to_string(),
            shim_path,
            format!("LAB_OPEN_LOG={}", open_log.display()),
            format!("{}={}", crate::recorder::SHAPE_LOG, shape_log.display()),
            format!("LAB_SECURITY_LOG={}", security_log.display()),
            "xcrun_nocache=1".to_string(),
            run.claude_bin.clone(),
            "setup-token".to_string(),
        ]
        .map(std::ffi::OsString::from)
        .to_vec(),
    };
    let user = std::env::var("USER").unwrap_or_default();
    let env: Vec<(std::ffi::OsString, std::ffi::OsString)> = [
        ("HOME", run.w.join("home").to_string_lossy().into_owned()),
        ("USER", user.clone()),
        ("LOGNAME", user),
        ("LANG", "en_US.UTF-8".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let person = StopAtPrompt {
        store: store.clone(),
        said: Mutex::new(Vec::new()),
        asked: AtomicBool::new(false),
        prompt_ok: AtomicBool::new(false),
        listing: Mutex::new(Vec::new()),
        at: Mutex::new(None),
        started: Instant::now(),
    };
    let result = setup_token::capture(
        &Capture {
            program: &program,
            store: &store,
            sign_in_as: EMAIL,
            env: &env,
            deadline: WakeDeadline::after(HANG),
            window: None,
        },
        &person,
    );
    let said = person.said.lock().map(|s| s.clone()).unwrap_or_default();
    let (v, why) = judge_frames(
        &result,
        person.asked.load(Ordering::SeqCst),
        person.prompt_ok.load(Ordering::SeqCst),
        &said,
    );
    rec.saw(format!(
        "A: Claude Code {} ({})",
        run.claude_ver, run.claude_bin
    ));
    if let Ok(at) = person.at.lock()
        && let Some(at) = *at
    {
        rec.saw(format!(
            "A: the paste prompt stood {} ms after the spawn",
            at.as_millis()
        ));
    }
    for line in &said {
        rec.saw(format!(
            "A: the capture said: {}",
            line.split(':').next().unwrap_or(line)
        ));
    }
    let listing = person.listing.lock().map(|l| l.clone()).unwrap_or_default();
    rec.saw(format!(
        "A: the child's scratch folder at the prompt: {}",
        if listing.is_empty() {
            "empty".to_string()
        } else {
            listing.join("; ")
        }
    ));
    let shapes = scratch_text(&shape_log);
    for line in shapes.lines() {
        rec.saw(format!(
            "A: the opener was handed {}",
            line.split_once(' ').map_or(line, |(_, s)| s)
        ));
    }
    if shapes.trim().is_empty() {
        rec.saw("A: the opener recorder was handed nothing (an `open` by absolute path is the jail's to deny)");
    }
    let security = scratch_text(&security_log);
    for line in security.lines() {
        let (cmd, svc) = tripwire_words(line);
        rec.saw(format!("A: secure storage asked: {cmd} service={svc}"));
    }
    if security.trim().is_empty() {
        rec.saw("A: secure storage: nothing asked of `security` (the keychain's daemons are the jail's to deny)");
    }
    match frames_of(&program, &env, &store, FRAMES_FOR) {
        Ok(lines) => {
            for line in lines.iter().take(60) {
                rec.saw(format!(
                    "A: the frame ({} s, cleaned): {line}",
                    FRAMES_FOR.as_secs()
                ));
            }
        }
        Err(e) => rec.saw(format!("A: the frame could not be read: {e}")),
    }
    let left = store.read_dir().map(|it| it.flatten().count()).unwrap_or(0);
    rec.saw(format!(
        "A: scratch folders left after the capture ended: {left}"
    ));
    let holds_token = holds(listing.join("\n").as_bytes(), b"sk-ant-oat");
    rec.check(
        'A',
        v,
        format!("the real setup-token's frames match the capture's grammar: {why}"),
    );
    rec.ok(
        'A',
        left == 0 && !holds_token,
        "the capture removed its scratch folder and nothing it held names a token",
    );
    Ok(())
}

/// A tripwire line's sub-command and service, never its account.
fn tripwire_words(line: &str) -> (String, String) {
    let cmd = line.split_whitespace().nth(1).unwrap_or("?").to_string();
    let svc = line
        .split_once("service=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map_or_else(|| "-".to_string(), |(s, _)| s.to_string());
    (cmd, svc)
}

// ---------------------------------------------------------------------------------------------
// Lane B: the stand-in end to end.

/// The exports every line the lab types into a tab carries: the stub's route the default, the
/// lab's `open` and `security` stand-ins first on `PATH`.
fn exports(run: &Run) -> String {
    let q = |p: &Path| shell_quote(&p.to_string_lossy());
    format!(
        "export PATH={}:\"$PATH\" LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={} \
         DISABLE_AUTOUPDATER=1 {}={}",
        q(&run.run.join("shim")),
        q(&run.w.join("security.log")),
        q(&run.w.join("open.log")),
        q(&run.run.join("shim/open")),
        aterm_agent::harness::netwatch::LAB_ROUTE_SEAM,
        shell_quote(&crate::lab::stub_base_url(run)),
    )
}

/// `aterm ctl spawn`: a new tab's session id.
fn spawn_tab(door: &Door<'_>) -> Result<String, Fail> {
    let out = door.out(&["spawn"]);
    out.lines()
        .next()
        .and_then(|l| l.strip_prefix("OK "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .ok_or_else(|| die("spawn", format!("no session: {}", out.trim())))
}

fn type_line(door: &Door<'_>, sid: &str, line: &str) {
    let at = format!("@{sid}");
    let _ = door.out(&[&at, "send", line]);
    let _ = door.out(&[&at, "key", "enter"]);
}

/// Closes tab `sid`.
fn close_tab(door: &Door<'_>, sid: &str) {
    let _ = door.out(&[&format!("@{sid}"), "close"]);
}

/// The status a line typed with `; echo "<word>=$?"` ended with, once the tab shows it.
fn exit_word(door: &Door<'_>, sid: &str, word: &str) -> Result<Option<i32>, Fail> {
    let at = format!("@{sid}");
    let pat = format!("{word}=[0-9]+");
    let re = aterm_regex::Regex::new(&pat).map_err(|e| die("a pattern", e))?;
    let mut found = None;
    wait_until(|| {
        let text = door.out(&[&at, "text"]);
        found = text.lines().rev().find_map(|l| {
            re.find(l)
                .and_then(|m| l[m.start() + word.len() + 1..m.end()].parse().ok())
        });
        found.is_some()
    })?;
    Ok(found)
}

/// One `ps -E` scan, noted.
fn ps_scan(rec: &mut Record, when: &str) -> Vec<i32> {
    match scan_ps(&needle()) {
        Ok((hits, rows)) => {
            rec.saw(format!(
                "B: ps -E -ww {when}: {rows} rows, {} hold the token",
                hits.len()
            ));
            hits
        }
        Err(e) => {
            rec.saw(format!("B: ps -E -ww {when}: could not run ({e})"));
            vec![-1]
        }
    }
}

/// The category of a path a tree scan found the token in, for the record.
#[must_use]
pub fn place_of(path: &Path) -> &'static str {
    let s = path.to_string_lossy();
    if s.contains("/.secrets") {
        "~/.secrets"
    } else if s.contains("journal") {
        "the journal"
    } else if s.contains("fabric") {
        "the fabric stores"
    } else if s.ends_with(".log") || s.contains("/logs/") {
        "aterm's logs"
    } else if s.contains("/.claude") || s.contains("/cfg/") {
        "the scratch config folder"
    } else if s.contains("cast") {
        "a cast"
    } else {
        "another file"
    }
}

fn lane_b(run: &Run, door: &Door<'_>, world: &World, rec: &mut Record) -> Result<(), Fail> {
    println!("== lane B: the verb and the wrapper on the stand-in, every place scanned");
    // A kept run folder may hold an earlier run's control plant; the scan below must not read it.
    let _ = std::fs::remove_dir_all(run.run.join("setup-token-plant"));
    let _ = std::fs::remove_dir_all(run.run.join("setup-token-ps-control"));
    let t0 = SystemTime::now();
    let sid = spawn_tab(door)?;
    let aterm = shell_quote(&run.aterm);
    type_line(
        door,
        &sid,
        &format!(
            "{}; {aterm} harness accounts token {LABEL}; echo \"st-verb-exit=$?\"",
            exports(run)
        ),
    );
    let waiting = wait_for(&world.standin.join(ls::SETUP_TOKEN_WAITING))?;
    rec.ok(
        'B',
        waiting,
        "the verb ran the store build's setup-token (the stand-in's frame shown)",
    );
    let during = if waiting {
        ps_scan(rec, "while the capture holds the stand-in")
    } else {
        Vec::new()
    };
    put(&world.standin.join(ls::SETUP_TOKEN_GO), "1\n")?;
    let verb_exit = exit_word(door, &sid, "st-verb-exit")?;
    rec.saw(format!("B: the verb exited {verb_exit:?}"));
    let stored = matches!(
        aterm_secrets::claude_token::read(&world.vault, &world.key, HANG),
        Ok(aterm_secrets::claude_token::Lookup::Found(read))
            if read.token().is_some_and(|t| t.as_slice() == sentinel().as_bytes())
    );
    rec.ok(
        'B',
        verb_exit == Some(0) && stored,
        "the verb stored the stand-in's token in the vault (exit 0, the entry holds it)",
    );
    type_line(
        door,
        &sid,
        &format!(
            "{}; {aterm} harness accounts token-run {LABEL} -- {}; echo \"st-run-exit=$?\"",
            exports(run),
            shell_quote(&world.build.to_string_lossy())
        ),
    );
    let served_at = world.standin.join(ls::SETUP_TOKEN_SERVED);
    let served = wait_for(&served_at)?;
    let served_line = scratch_text(&served_at);
    rec.saw(format!(
        "B: the served stand-in noted: {}",
        served_line.trim()
    ));
    rec.ok(
        'B',
        served
            && served_line.contains("matched=1")
            && served_line.contains("marker=aterm-claude-token-"),
        "the wrapper served the token to the store build on a descriptor, with the marker",
    );
    let while_served = if served {
        ps_scan(rec, "while the served stand-in runs")
    } else {
        Vec::new()
    };
    put(&world.standin.join(ls::SETUP_TOKEN_RELEASE), "1\n")?;
    let run_exit = exit_word(door, &sid, "st-run-exit")?;
    rec.saw(format!("B: the wrapper's line exited {run_exit:?}"));
    let after = ps_scan(rec, "after both");
    rec.ok(
        'B',
        during.is_empty() && while_served.is_empty() && after.is_empty() && waiting && served,
        "no process's argv or environment held the token (ps -E -ww, three times)",
    );
    let (lines, count) = scan_scrollback(door, &sid, &needle());
    rec.saw(format!(
        "B: the tab's scrollback: {count} lines, {} hold the token",
        lines.len()
    ));
    rec.ok(
        'B',
        count > 0 && lines.is_empty(),
        "the tab's scrollback holds no token",
    );
    let (in_cast, cast_len) = scan_cast(door, &sid, &needle());
    rec.saw(format!("B: the tab's cast: {cast_len} bytes"));
    rec.ok(
        'B',
        cast_len > 0 && !in_cast,
        "the tab's cast holds no token",
    );
    let needle_b = needle();
    let run_scan = scan_tree(&run.run, needle_b.as_bytes(), None, &[]);
    let sock_scan = scan_tree(Path::new(&run.sockdir), needle_b.as_bytes(), None, &[]);
    rec.saw(format!(
        "B: the run folder: {} files read ({} unread{}), the socket folder: {} files",
        run_scan.files,
        run_scan.unread,
        if run_scan.cut { ", cut short" } else { "" },
        sock_scan.files
    ));
    for hit in run_scan.hits.iter().chain(&sock_scan.hits) {
        rec.saw(format!(
            "B: the token is in {} ({})",
            hit.display(),
            place_of(hit)
        ));
    }
    rec.ok(
        'B',
        run_scan.files > 0
            && run_scan.hits.is_empty()
            && sock_scan.hits.is_empty()
            && !run_scan.cut,
        "no file of the lab holds the token: the journal, the fabric stores, aterm's logs, the \
         scratch config folder and ~/.secrets among them",
    );
    let tmp = temp_root();
    let tmp_scan = scan_tree(&tmp, needle_b.as_bytes(), Some(t0), &[]);
    rec.saw(format!(
        "B: $TMPDIR ({}): {} files changed during the lane read{}",
        tmp.display(),
        tmp_scan.files,
        if tmp_scan.cut { ", cut short" } else { "" }
    ));
    for hit in &tmp_scan.hits {
        rec.saw(format!("B: the token is in {}", hit.display()));
    }
    rec.ok(
        'B',
        tmp_scan.hits.is_empty() && !tmp_scan.cut,
        "$TMPDIR holds no token",
    );
    let home_secrets = world.home.join(".secrets");
    rec.saw(format!(
        "B: the scratch ~/.secrets {} (scanned with the run folder)",
        if home_secrets.exists() {
            "exists"
        } else {
            "is absent"
        }
    ));
    controls_b(run, door, rec, t0)?;
    let _ = door.out(&[&format!("@{sid}"), "close"]);
    Ok(())
}

/// Lane B's positive controls, after every real scan: the sentinel planted in a file of the run
/// folder and of `$TMPDIR`, in a process's argv and environment, and in a control tab's
/// scrollback and cast (`cat` of the planted file), each found by its scanner.
fn controls_b(run: &Run, door: &Door<'_>, rec: &mut Record, t0: SystemTime) -> Result<(), Fail> {
    let needle_b = needle();
    let plant = run.run.join("setup-token-plant");
    put(&plant.join("planted.txt"), &format!("{}\n", sentinel()))?;
    let found = scan_tree(&run.run, needle_b.as_bytes(), None, &[]);
    rec.ok(
        'B',
        found.hits == vec![plant.join("planted.txt")],
        "control: a token planted in the run folder is found there (and only there)",
    );
    let tmp_plant = temp_root().join(format!("setup-token-plant-{}.noindex", std::process::id()));
    let planted = put(&tmp_plant.join("planted.txt"), &format!("{}\n", sentinel()));
    let found = scan_tree(&temp_root(), needle_b.as_bytes(), Some(t0), &[]);
    let _ = std::fs::remove_dir_all(&tmp_plant);
    planted?;
    rec.ok(
        'B',
        found.hits.contains(&tmp_plant.join("planted.txt")),
        "control: a token planted in $TMPDIR is found there",
    );
    // The process is the lab's own stand-in, not `/bin/sleep`: `ps -E` reads no platform
    // binary's environment (macOS keeps a platform binary's arguments area from other
    // processes), so a control on one finds nothing whatever the scanner does.
    let token = sentinel();
    let held = run.run.join("setup-token-ps-control");
    put(
        &held.join(crate::local_signins::SETUP_TOKEN_LABEL),
        &format!("{LABEL}\n"),
    )?;
    let exe = std::env::current_exe().map_err(|e| die("the lab's own path", e))?;
    let child = aterm_uds::spawnfd::command(&exe)
        .arg(crate::local_signins::SETUP_TOKEN_VERB)
        .arg(&held)
        .arg("control")
        .env("LAB_PLANTED", &token)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn();
    match child {
        Ok(mut child) => {
            let pid = i32::try_from(child.id()).unwrap_or(0);
            let started = wait_for(&held.join(crate::local_signins::SETUP_TOKEN_SERVED))?;
            let hits = scan_ps(&needle_b).map(|(h, _)| h).unwrap_or_default();
            let _ = child.kill();
            let _ = child.wait();
            rec.ok(
                'B',
                started && hits.contains(&pid),
                "control: a process holding the token in its environment is found by ps -E -ww",
            );
        }
        Err(e) => rec.check(
            'B',
            Verdict::Unproven,
            format!("control: ps -E's could not start ({e})"),
        ),
    }
    let _ = std::fs::remove_dir_all(&held);
    let sid = spawn_tab(door)?;
    type_line(
        door,
        &sid,
        &format!(
            "cat {}",
            shell_quote(&plant.join("planted.txt").to_string_lossy())
        ),
    );
    let at = format!("@{sid}");
    let mut shown = false;
    wait_until(|| {
        shown = door.out(&[&at, "text"]).contains(&needle_b);
        shown
    })?;
    let (lines, _) = scan_scrollback(door, &sid, &needle_b);
    rec.ok(
        'B',
        shown && !lines.is_empty(),
        "control: a token a tab showed is found in its scrollback",
    );
    let (in_cast, _) = scan_cast(door, &sid, &needle_b);
    rec.ok(
        'B',
        in_cast,
        "control: a token a tab showed is found in its cast",
    );
    let _ = door.out(&[&at, "close"]);
    let _ = std::fs::remove_dir_all(&plant);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Lane C: the descriptor on a real Claude Code.

/// The token in the vault for lane C: lane B's where it stored one, else written here as a
/// member write, minted on this Mac a moment ago and a year from its end.
fn ensure_token(world: &World, rec: &mut Record) -> Result<(), Fail> {
    use aterm_secrets::claude_token::{self, ExpiryFrom, Lookup, Meta};
    if matches!(
        claude_token::read(&world.vault, &world.key, HANG),
        Ok(Lookup::Found(read)) if read.token().is_some_and(|t| t.as_slice() == sentinel().as_bytes())
    ) {
        rec.saw("C: the token is lane B's, in the vault");
        return Ok(());
    }
    let now = u64::try_from(crate::util::now_secs()).unwrap_or(0);
    let meta = Meta {
        email: EMAIL.to_string(),
        account_id: String::new(),
        minted_at: now,
        expires_at: now + setup_token::LIFETIME - 43_200,
        expiry_from: ExpiryFrom::Printed,
        minted_on: claude_token::seat_here(&world.vault)
            .ok()
            .flatten()
            .unwrap_or_else(|| "lab-mac".to_string()),
        cli: "2.1.318".to_string(),
        verified: None,
    };
    let written = claude_token::write(
        &world.vault,
        &world.key,
        &aterm_secrets::SecretBytes::from(sentinel()),
        &meta,
        aterm_secrets::client::OnBusy::Refuse(Instant::now() + HANG),
    )
    .map_err(|e| die("the token's write", e))?;
    rec.saw(format!("C: the token written for the lane: {written:?}"));
    Ok(())
}

/// The settings and the user's MCP server lane C's probes run as.
fn lay_probes(run: &Run, world: &World) -> Result<(), Fail> {
    let lab = std::env::current_exe().map_err(|e| die("this lab binary", e))?;
    let cmd = |role: &str| {
        format!(
            "{} {PROBE_VERB} {role} {}",
            shell_quote(&lab.to_string_lossy()),
            shell_quote(&world.probes.to_string_lossy())
        )
    };
    crate::verbs::settings_merge(
        run,
        &J::obj([
            (
                "hooks",
                J::obj([(
                    "SessionStart",
                    J::Arr(vec![J::obj([(
                        "hooks",
                        J::Arr(vec![J::obj([
                            ("type", J::s("command")),
                            ("command", J::s(cmd("hook"))),
                        ])]),
                    )])]),
                )]),
            ),
            (
                "statusLine",
                J::obj([
                    ("type", J::s("command")),
                    ("command", J::s(cmd("statusline"))),
                ]),
            ),
        ]),
    )?;
    // The wrapper refuses a `CLAUDE_CONFIG_DIR` (it judges the default one alone), so the session
    // reads `~/.claude.json`, not the lab's `home/.claude/.claude.json`: it starts from that one's
    // onboarding and trust, or it opens on the first-run screen, whose check of
    // api.anthropic.com no stub answers.
    let config = world.home.join(".claude.json");
    let mut text = scratch_text(&config);
    if text.trim().is_empty() {
        text = scratch_text(&world.home.join(".claude/.claude.json"));
    }
    let mut j = J::parse(if text.trim().is_empty() { "{}" } else { &text })
        .map_err(|e| die("the scratch .claude.json", e))?;
    j.set(
        "mcpServers",
        J::obj([(
            "lab-probe",
            J::obj([
                ("type", J::s("stdio")),
                ("command", J::s(lab.to_string_lossy())),
                (
                    "args",
                    J::Arr(vec![
                        J::s(PROBE_VERB),
                        J::s("mcp"),
                        J::s(world.probes.to_string_lossy()),
                    ]),
                ),
            ]),
        )]),
    );
    atomic_write(&config, j.dump(crate::pyjson::COMPACT).as_bytes())
        .map_err(|e| die("the scratch .claude.json", e))
}

/// The pid of the Claude Code the lab launched by `bin`, among the jail's members: the one whose
/// exec path is `bin` and whose exec-time environment holds `marker_env`.
fn claude_pid(run: &Run, bin: &str, seen: &[i32]) -> Option<i32> {
    crate::sys::jail_pids(&run.mark)
        .ok()?
        .into_iter()
        .find(|pid| {
            !seen.contains(pid)
                && u32::try_from(*pid)
                    .ok()
                    .and_then(aterm_uds::procargs::read)
                    .is_some_and(|buf| {
                        aterm_uds::procargs::parse(&buf)
                            .is_some_and(|p| p.exec_path == bin.as_bytes())
                    })
        })
}

/// `pid`'s exec-time environment, as `KEY=value` strings.
fn exec_env(pid: i32) -> Vec<String> {
    u32::try_from(pid)
        .ok()
        .and_then(aterm_uds::procargs::read)
        .map(|buf| {
            aterm_uds::procargs::parse(&buf)
                .map(|p| {
                    p.env()
                        .map(|e| String::from_utf8_lossy(e).into_owned())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// One launch of the real Claude Code through the wrapper, its probes' reports in `<probes>/<word>`;
/// `spare` turns the wrapper's spare socket on, and `tap` puts aterm's usage tap on its line
/// (`--settings`, the carrying tap, so the probes' statusLine still runs under it). What its
/// exec-time environment read as, and its reports; the caller closes its tab ([`Launched::sid`]).
fn launch_c(
    run: &Run,
    door: &Door<'_>,
    world: &World,
    word: &str,
    (spare, tap): (bool, Option<&str>),
    rec: &mut Record,
) -> Result<Launched, Fail> {
    put(&world.probes.join(PROBE_CURRENT), &format!("{word}\n"))?;
    let dir = world.probes.join(word);
    let stub_before = scratch_text(&run.w.join("stub.log")).lines().count();
    let sid = spawn_tab(door)?;
    let spare_word = spare_export(spare);
    type_line(
        door,
        &sid,
        &format!(
            "{}{spare_word}; {} harness accounts token-run {LABEL} -- {}{}; echo \"token-run-exit=$?\"",
            exports(run),
            shell_quote(&run.aterm),
            shell_quote(&run.claude_bin),
            tap.map_or_else(String::new, |tap| format!(
                " --settings {}",
                shell_quote(tap)
            ))
        ),
    );
    let composer = crate::forever::empty_composer(door, &sid, "claude")?;
    let at = format!("@{sid}");
    if let Some(why) = composer.why("Claude Code") {
        rec.saw(format!("C({word}): {why}; its screen:"));
        for line in door
            .out(&[&at, "text"])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(40)
        {
            rec.saw(format!("C({word}):   {}", line.trim_end()));
        }
    }
    let _ = door.out(&[&at, "send", "hi"]);
    let _ = door.out(&[&at, "key", "enter"]);
    let label = format!("auth=oat:{LABEL}");
    let answered = wait_until(|| {
        scratch_text(&run.w.join("stub.log"))
            .lines()
            .skip(stub_before)
            .any(|l| l.contains("kind=main") && l.contains(&label))
    })?;
    let reported = wait_until(|| {
        let reports = reports_in(&dir);
        ROLES
            .iter()
            .all(|role| reports.iter().any(|r| r.role == *role))
    })?;
    if !reported {
        rec.saw(format!(
            "C({word}): not every probe reported within {} s",
            HANG.as_secs()
        ));
    }
    let pid = claude_pid(run, &run.claude_bin, &[]);
    let env = pid.map(exec_env);
    let table = pid.map(descriptor_table).unwrap_or_default();
    let descriptor = env.as_ref().and_then(|env| {
        env.iter().find_map(|e| {
            e.strip_prefix(aterm_agent::harness::token_route::DESCRIPTOR_ENV)?
                .strip_prefix('=')
                .map(str::to_string)
        })
    });
    rec.saw(format!(
        "C({word}): Claude Code's descriptors as the probes reported: [{}]; its descriptor \
         variable names {}, {}",
        table.join(" "),
        descriptor.as_deref().unwrap_or("-"),
        descriptor.as_deref().map_or_else(
            || "unread".to_string(),
            |n| match table
                .iter()
                .find_map(|t| t.strip_prefix(n)?.strip_prefix(':'))
            {
                Some("unix") =>
                    "a socket then (the token's, or one reusing its number)".to_string(),
                Some(kind) =>
                    format!("a {kind} then (the token's socket closed, its number reused)"),
                None => "closed by then".to_string(),
            }
        )
    ));
    let reports = reports_in(&dir);
    for r in &reports {
        rec.saw(format!(
            "C({word}): {} pid {} fds [{}] read [{}] found_fd={} found_env={} found_input={} \
             descriptor_env={} marker={}{}",
            r.role,
            r.pid,
            r.fds.join(" "),
            r.read.join(" "),
            r.found_fd
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            u8::from(r.found_env),
            u8::from(r.found_input),
            u8::from(r.descriptor_env),
            r.marker.as_deref().unwrap_or("-"),
            if r.input_keys.is_empty() {
                String::new()
            } else {
                format!(" input_keys=[{}]", r.input_keys.join(" "))
            }
        ));
    }
    Ok(Launched {
        sid,
        reports,
        env,
        answered,
    })
}

/// What one [`launch_c`] saw.
struct Launched {
    /// The tab it runs in, which its caller closes.
    sid: String,
    /// Its probes' reports.
    reports: Vec<ProbeReport>,
    /// The session's exec-time environment, where its process was found.
    env: Option<Vec<String>>,
    /// Whether the stub answered its turn as the lane's token.
    answered: bool,
}

/// The positive control: the probe the hook runs (the same binary and words), started by the lab
/// with the sentinel on a socket it inherits (one guard, as the wrapper hands its descriptor), in
/// an environment value and on its stdin. Its report, and the descriptor's number.
fn control_probe(world: &World) -> Result<(Option<ProbeReport>, i32), Fail> {
    use std::io::Write as _;
    put(&world.probes.join(PROBE_CURRENT), "control\n")?;
    let token = sentinel();
    let (mut ours, theirs) =
        std::os::unix::net::UnixStream::pair().map_err(|e| die("the control's socket", e))?;
    ours.write_all(format!("{token}\n").as_bytes())
        .map_err(|e| die("the control's socket", e))?;
    let exe = std::env::current_exe().map_err(|e| die("this lab binary", e))?;
    let mut command = aterm_uds::spawnfd::command(&exe);
    command
        .arg(PROBE_VERB)
        .arg("hook")
        .arg(&world.probes)
        .env("LAB_PLANTED", &token)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    command
        .inherit_guards(vec![std::os::fd::OwnedFd::from(theirs)])
        .map_err(|e| die("the control's descriptor", e))?;
    let fd = command.guard_targets().next().unwrap_or(-1);
    let mut child = command.spawn().map_err(|e| die("the control probe", e))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{{\"lab\":\"{token}\"}}\n").as_bytes());
    }
    let _ = child.wait();
    drop(ours);
    let report = reports_in(&world.probes.join("control"))
        .into_iter()
        .find(|r| r.role == "hook");
    Ok((report, fd))
}

/// The words the spare launch's line adds: the wrapper's spare socket on (a debug build's seam
/// alone).
#[cfg(debug_assertions)]
fn spare_export(spare: bool) -> String {
    if spare {
        format!(
            " {}=1",
            aterm_agent::harness::accounts_token_run::SPARE_SEAM
        )
    } else {
        String::new()
    }
}

fn lane_c(run: &Run, door: &Door<'_>, world: &World, rec: &mut Record) -> Result<(), Fail> {
    println!(
        "== lane C: the real Claude Code on the wrapper's descriptor; what its children inherit"
    );
    ensure_token(world, rec)?;
    let now = crate::util::now_secs();
    // The token and the row's stored login show the same windows, as one account's two
    // credentials do: the crediting check's verification compares them ([`credit_c`]).
    let resets = (now + 7_200, now + 5 * 86_400);
    for label in [LABEL, STORED] {
        crate::verbs::write_headers_for(
            &run.w,
            &format!("oat:{label}"),
            crate::stub::chosen_resets(("0.25", resets.0), ("0.40", resets.1)),
        )?;
    }
    lay_probes(run, world)?;
    let (control, fd) = control_probe(world)?;
    let (v, why) = judge_control(control.as_ref(), fd);
    rec.check(
        'C',
        v,
        format!(
            "control: the probe, handed the token on an inherited socket, in its environment and \
             on its input, finds it each way: {why}"
        ),
    );
    let gate = launch_c(run, door, world, "gate", (false, None), rec)?;
    close_tab(door, &gate.sid);
    let (v, why) = judge_children(&gate.reports);
    rec.check(
        'C',
        v,
        format!("no tool child of the token session can read the token: {why}"),
    );
    rec.ok(
        'C',
        gate.answered,
        format!("the stub answered the session's turn as oat:{LABEL} (the descriptor's token)"),
    );
    match gate.env {
        Some(env) => {
            let login = aterm_agent::harness::accounts::ProcessLogin::of_exec_env(&env);
            let route = login
                .as_ref()
                .map(|l| format!("{:?}", l.route))
                .unwrap_or_default();
            let has = |name: &str| {
                env.iter()
                    .any(|e| e.split_once('=').is_some_and(|(k, _)| k == name))
            };
            let entry = format!("aterm-claude-token-{}", world.key.as_str());
            let marker_ok = env.iter().any(|e| {
                e.split_once('=').is_some_and(|(k, v)| {
                    k == aterm_agent::harness::token_route::MARKER_ENV && v == entry
                })
            });
            rec.saw(format!(
                "C: the session's exec-time environment reads as {route}"
            ));
            rec.ok(
                'C',
                has(aterm_agent::harness::token_route::DESCRIPTOR_ENV)
                    && marker_ok
                    && route.starts_with("VaultToken"),
                "the session's exec-time environment holds the descriptor variable and the marker \
                 naming the account's entry, and reads as the vault token's route",
            );
            rec.ok(
                'C',
                !env.iter().any(|e| holds(e.as_bytes(), needle().as_bytes())),
                "the session's exec-time environment holds no token",
            );
        }
        None => rec.check(
            'C',
            Verdict::Unproven,
            "the session's process was not found in the jail",
        ),
    }
    // The wrapper's spare arm leaves a second socket holding the token open and unread in the
    // session: a child that inherited a descriptor of the session's would find it there. Claude
    // Code 2.1.318 hands its children descriptors 0 to 2 alone, so none does; were one to, the
    // token's safety would rest on Claude Code closing its descriptor, and the design is revised.
    let spare = launch_c(run, door, world, "spare", (true, None), rec)?;
    close_tab(door, &spare.sid);
    let (v, why) = judge_children(&spare.reports);
    rec.check(
        'C',
        v,
        format!(
            "with the wrapper's spare socket left open and unread in the session, no child \
             inherits it either (Claude Code passes its children stdio alone): {why}"
        ),
    );
    credit_c(run, door, world, resets, rec)
}

/// The windows the row's stored login shows in the harness's accounts state now, where it has a
/// sample.
fn row_windows(store: &aterm_agent::harness::accounts::Store) -> Option<Windows> {
    let now = crate::util::now_secs();
    let (state, _) = store.load(now).ok()?;
    let sample = state.accounts.get(LABEL)?.sample.as_ref()?;
    Some(Windows::current(sample, now))
}

/// The row's stored login on the default login, as a sign-in leaves it: [`STORED`]'s live login
/// in `home/.claude/.credentials.json` and the profile (`home/.claude.json`, its MCP server kept)
/// naming [`EMAIL`]'s account.
fn lay_stored_login(world: &World) -> Result<(), Fail> {
    let dir = world.home.join(".claude");
    crate::util::create_private_dir_all(&dir).map_err(|e| die(&dir.display().to_string(), e))?;
    let creds = dir.join(".credentials.json");
    let login = crate::claude_rotation_proof::live_login(STORED, crate::util::now_ms());
    crate::sys::put(&creds, 0o600, &crate::sys::PutSrc::Bytes(login.as_bytes()))
        .map_err(|e| die(&creds.display().to_string(), e))?;
    let config = world.home.join(".claude.json");
    let text = scratch_text(&config);
    let mut j = J::parse(if text.trim().is_empty() { "{}" } else { &text })
        .map_err(|e| die("the scratch .claude.json", e))?;
    let mut account = crate::stub::account::composed_oauth_account(crate::util::now_ms());
    account.set("emailAddress", J::s(EMAIL));
    j.set("oauthAccount", account);
    atomic_write(&config, j.dump(crate::pyjson::COMPACT).as_bytes())
        .map_err(|e| die("the scratch .claude.json", e))
}

/// Lane C's crediting check (S13): the harness's usage tap credits the row for a token session,
/// and verification by windows writes `verified`.
///
/// The row's stored login ([`STORED`]) is laid on the default login and a plain session on it,
/// under aterm's usage tap, serves a turn, whose reading the harness folds into the row: its
/// windows, the stub's chosen `resets`, are the basis. Then a token session under the tap serves
/// one: the tap must credit the row (the table's `first_served_at`), the stub must answer it as
/// the token (`oat:lab-t`, the stored login beside it), and its windows, the same resets, must make
/// verification find them equal and write `verified` into the token's meta in the vault.
fn credit_c(
    run: &Run,
    door: &Door<'_>,
    world: &World,
    (session_reset, week_reset): (i64, i64),
    rec: &mut Record,
) -> Result<(), Fail> {
    use aterm_agent::harness::accounts_tokens::{self, Verdict as Found};
    use aterm_secrets::claude_token::{self, Lookup};
    let tap = aterm_agent::harness::tap::settings_value_carrying(Path::new(&run.aterm), true)
        .ok_or_else(|| {
            die(
                "the usage tap",
                format!("{} cannot be named in it", run.aterm),
            )
        })?;
    lay_stored_login(world)?;
    let store = aterm_agent::harness::accounts::Store::at(
        &run.state().join("harness").join("claude-harness"),
    );
    let want = Windows {
        week_reset: Some(week_reset),
        session_reset: Some(session_reset),
    };
    put(&world.probes.join(PROBE_CURRENT), "basis\n")?;
    let stub_before = scratch_text(&run.w.join("stub.log")).lines().count();
    let sid = spawn_tab(door)?;
    type_line(
        door,
        &sid,
        &format!(
            "{}; {} --settings {}; echo \"basis-exit=$?\"",
            exports(run),
            shell_quote(&run.claude_bin),
            shell_quote(&tap)
        ),
    );
    let composer = crate::forever::empty_composer(door, &sid, "claude")?;
    if let Some(why) = composer.why("Claude Code") {
        rec.saw(format!("C(basis): {why}"));
    }
    let at = format!("@{sid}");
    let _ = door.out(&[&at, "send", "hi"]);
    let _ = door.out(&[&at, "key", "enter"]);
    let stored = format!("auth=oat:{STORED}");
    let answered = wait_until(|| {
        scratch_text(&run.w.join("stub.log"))
            .lines()
            .skip(stub_before)
            .any(|l| l.contains("kind=main") && l.contains(&stored))
    })?;
    let sampled = wait_until(|| row_windows(&store) == Some(want))?;
    rec.saw(format!(
        "C(basis): the row's windows in the accounts state: {:?}",
        row_windows(&store)
    ));
    close_tab(door, &sid);
    rec.ok(
        'C',
        answered && sampled,
        format!(
            "basis: a session on the row's stored login (oat:{STORED}) served a turn under the \
             usage tap, and the harness folded its windows (the stub's chosen resets) into the row"
        ),
    );
    let token = launch_c(run, door, world, "credit", (false, Some(&tap)), rec)?;
    let row = || {
        accounts_tokens::read_table(&store)
            .ok()
            .and_then(|table| table.rows.get(world.key.as_str()).cloned())
    };
    let served = wait_until(|| {
        row().is_some_and(|r| {
            r.used_on.first_served_at.is_some() && r.verification.verdict.is_some()
        })
    })?;
    let verified = wait_until(|| {
        matches!(
            claude_token::standing(&world.vault, &world.key),
            Ok(Lookup::Found(standing)) if standing.verified()
        )
    })?;
    close_tab(door, &token.sid);
    let row = row();
    rec.saw(format!(
        "C(credit): the table's row: served {:?}, verification {:?}",
        row.as_ref()
            .map(|r| (&r.used_on.first_served_at, &r.used_on.last_served_at)),
        row.as_ref().map(|r| &r.verification)
    ));
    rec.ok(
        'C',
        token.answered,
        format!(
            "the token session, the row's stored login beside it, was answered as oat:{LABEL} \
             (the descriptor's token)"
        ),
    );
    rec.ok(
        'C',
        served,
        "the tap credited the row for the token session: this Mac's token table records its \
         first served reading, and verification compared it with the basis",
    );
    rec.ok(
        'C',
        verified
            && row
                .as_ref()
                .is_some_and(|r| r.verification.verdict == Some(Found::Equal)),
        "the same windows on the token and the stored login made verification find them equal \
         and write `verified` into the token's meta in the vault",
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The verb.

/// Which lanes `setup-token-lab` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lanes {
    pub a: bool,
    pub b: bool,
    pub c: bool,
}

/// `[a|b|c|all]... [tag] [--keep]`: the lanes (all when none is named), the tag and `--keep`.
///
/// # Errors
/// An unknown flag, or a tag the lab refuses.
pub fn parse(args: &[String]) -> Result<(Lanes, String, bool), String> {
    let mut lanes = Lanes {
        a: false,
        b: false,
        c: false,
    };
    let (mut tag, mut keep) = ("setup-token".to_string(), false);
    for a in args {
        match a.as_str() {
            "--keep" => keep = true,
            "a" | "A" => lanes.a = true,
            "b" | "B" => lanes.b = true,
            "c" | "C" => lanes.c = true,
            "all" => {
                lanes = Lanes {
                    a: true,
                    b: true,
                    c: true,
                }
            }
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            other => tag = other.to_string(),
        }
    }
    if !lanes.a && !lanes.b && !lanes.c {
        lanes = Lanes {
            a: true,
            b: true,
            c: true,
        };
    }
    if !crate::paths::valid_tag(&tag) {
        return Err(format!("bad tag '{tag}'"));
    }
    Ok((lanes, tag, keep))
}

/// `setup-token-lab [a|b|c|all] [tag] [--keep]` (the module's summary says what it does).
pub fn cmd(args: &[String]) -> Res {
    let (lanes, tag, keep) =
        parse(args).map_err(|e| Fail::usage(format!("setup-token-lab: {e}")))?;
    crate::lab::find_claude().map_err(|e| {
        Fail::die(format!(
            "setup-token-lab: no Claude Code to run ({e}): install it (aterm pkg install claude) or \
             name one with LAB_CLAUDE=/path/to/claude"
        ))
    })?;
    stop::install();
    let o = UpOpts {
        trust: true,
        lifeline: !keep,
        route_seam: true,
        window: false,
        reroute: false,
        clock: false,
        restore: false,
        ..UpOpts::from_env()
    };
    let run = crate::lab::up(&tag, &o).map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            Fail::die(format!(
                "setup-token-lab: up failed: {}",
                f.msg.unwrap_or_default()
            ))
        }
    })?;
    println!(
        "setup-token-lab: Claude Code {} on the stub {} in lab '{tag}' ({})",
        run.claude_ver,
        crate::lab::stub_base_url(&run),
        run.aterm_build
    );
    let mut rec = Record::default();
    let ran = World::lay(&run).and_then(|world| {
        let door = Door::new(&run).map_err(|e| die("the lab's door", e))?;
        if lanes.a {
            lane_a(&run, &mut rec)?;
        }
        if lanes.b {
            lane_b(&run, &door, &world, &mut rec)?;
        }
        if lanes.c {
            lane_c(&run, &door, &world, &mut rec)?;
        }
        Ok(())
    });
    let kept = if keep {
        println!(
            "setup-token-lab: lab '{tag}' left up ({} down {tag})",
            crate::LAB
        );
        None
    } else {
        Some(crate::lab::down(&tag, false)?)
    };
    let mut text = format!(
        "setup-token-lab: Claude Code {} ({}), aterm {}\n",
        run.claude_ver, run.claude_bin, run.aterm_build
    );
    for line in &rec.lines {
        text.push_str(line);
        text.push('\n');
    }
    if let Err(f) = &ran {
        text.push_str(&format!(
            "could not finish: {}\n",
            f.msg.clone().unwrap_or_default()
        ));
    }
    println!("== verdicts");
    let (mut fail, mut unproven) = (0, 0);
    for row in &rec.rows {
        let word = match row.verdict {
            Verdict::Ok => "ok  ",
            Verdict::Fail => {
                fail += 1;
                "FAIL"
            }
            Verdict::Unproven => {
                unproven += 1;
                "??  "
            }
        };
        let line = format!("  {word} {}: {}", row.lane, row.what);
        println!("{line}");
        text.push_str(&line);
        text.push('\n');
    }
    if let Some(kept) = &kept {
        let _ = crate::util::write_fresh(&kept.join("setup-token-lab.txt"), &text);
        println!("lab '{tag}' is down; logs kept in {}", kept.display());
    } else {
        let _ = crate::util::write_fresh(&run.run.join("setup-token-lab.txt"), &text);
    }
    ran?;
    Ok(if fail > 0 {
        1
    } else if unproven > 0 || rec.rows.is_empty() {
        3
    } else {
        0
    })
}

#[cfg(test)]
#[path = "setup_token_lab_tests.rs"]
mod tests;
