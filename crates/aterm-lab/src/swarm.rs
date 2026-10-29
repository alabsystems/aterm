// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `swarm rotate`: a private `lab instance` with two enrolled Claude Code logins walls the first
//! through the usage-tap seam (`<aterm> harness tap`), rotates `active` onto the second
//! (`rotations[]` grows), and serves a turn on the second (`proven == true`).

use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::accounts::{self, LOGIN_ENV, Roster, Store};
use aterm_agent::harness::restart_stamp::kernel_start;
use aterm_agent::harness::tap;
use aterm_codec::json::escape;

use crate::fleet_fixture::Mode;
use crate::instance::{Instance, Launch, World, resolve_aterm, take_value};
use crate::pyjson::{COMPACT, J};
use crate::screen::field;
use crate::util::{atomic_write, now_ms, now_secs, scratch_text, shell_quote, utc_stamp};
use crate::{Fail, LAB, Res, sys};

#[path = "swarm_mailbox.rs"]
pub mod mailbox;

#[path = "swarm_partition.rs"]
pub mod partition;

/// Hang detector for an event that must happen.
const HANG: Duration = Duration::from_secs(60);

/// First enrolled login label.
pub const LABEL_A: &str = "swarm-a";
/// Second enrolled login label.
pub const LABEL_B: &str = "swarm-b";
/// Conversation ID resumed across the account rotation.
const CONVERSATION: &str = "0badf00d-c17b-4000-8000-111122223333";
/// Marker file under `$HOME/.claude/` configuring the pure-Rust `claude` stand-in.
const STANDIN_MARKER: &str = ".claude/swarm-rotate.json";

/// Instance configuration: automatic background lanes off, harness + early account rotation +
/// statusLine usage tap on, API `/usage` probe off so only the usage-tap seam drives rotation.
const SWARM_CONFIG: &str = "agents_auto_prime = false\n\
                            [update]\nenabled = false\nauto_apply = false\n\
                            [packages]\nenabled = false\n\
                            [machine]\nuniversal_control = \"leave\"\n\
                            [harness]\nenabled = true\naccounts = \"early\"\n\
                            human_grace_s = 0\nusage_tap = true\nprobe_api = false\n";

/// Parsed CLI options for `lab swarm rotate`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RotateOpts {
    pub aterm: Option<String>,
    pub keep: bool,
}

/// Parse `lab swarm <subcommand> ...` arguments.
pub fn parse_args(args: &[String]) -> Result<RotateOpts, Fail> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(Fail::usage(
            "swarm rotate|mailbox-signin|partition [--aterm PATH] [--keep]",
        ));
    };
    if sub == "-h" || sub == "--help" {
        return Err(Fail::usage(
            "swarm rotate|mailbox-signin|partition [--aterm PATH] [--keep]",
        ));
    }
    if sub != "rotate" {
        return Err(Fail::usage(format!(
            "swarm: unknown subcommand '{sub}' (expected 'rotate', 'mailbox-signin' or 'partition')"
        )));
    }
    let mut opts = RotateOpts::default();
    let mut rem = rest.to_vec();
    opts.aterm = take_value(&mut rem, "--aterm")?;
    for arg in rem {
        match arg.as_str() {
            "--keep" => opts.keep = true,
            other => {
                return Err(Fail::usage(format!(
                    "swarm rotate: unexpected '{other}' ({LAB} swarm --help)"
                )));
            }
        }
    }
    Ok(opts)
}

/// Candidate-selection policy compared by the proof's negative control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidatePolicy {
    /// Production policy (`accounts::advise_past_probe`).
    Production,
    /// Negative-control mutant: candidate filter that always stays on the current login.
    AlwaysStay,
}

/// Step one usage-tap wall + turn-end decision under `policy`, returning `(moved_to, rotations_len)`.
#[must_use]
pub fn simulate_tap_wall_step(
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    state: &mut accounts::State,
    sid: &str,
    from_label: &str,
    now: i64,
    policy: CandidatePolicy,
) -> (Option<String>, usize) {
    let tap_read = accounts::TapRead {
        five_hour: Some(accounts::WindowRead {
            pct_x10: 1000,
            reset: now + 3600,
            refilled: None,
        }),
        seven_day: Some(accounts::WindowRead {
            pct_x10: 150,
            reset: now + 86_400,
            refilled: None,
        }),
        at: now,
        seen_before: true,
        born: Some(now - 10),
        key: None,
    };
    let _ = accounts::note_reading_held(state, from_label, &tap_read, now);
    let advice = match policy {
        CandidatePolicy::Production => {
            let me = accounts::Me {
                account: Some(from_label),
                default_home: true,
                stored_login: true,
                left: None,
            };
            accounts::advise_past_probe(
                roster,
                state,
                enrolled,
                &me,
                &accounts::Point::TurnEnd,
                accounts::Mode::Early,
                now,
            )
        }
        CandidatePolicy::AlwaysStay => accounts::Advice::Stay(accounts::StayWhy::NoFresh),
    };
    if let accounts::Advice::Move { to, rotates, .. } = advice {
        if rotates {
            let expect = state.active.clone();
            let mover = accounts::Mover {
                sid,
                account: from_label,
            };
            if accounts::begin_rotation(state, roster, enrolled, expect.as_deref(), mover, &to, now)
                .is_ok()
            {
                return (Some(to), state.rotations.len());
            }
        }
        return (Some(to), state.rotations.len());
    }
    (None, state.rotations.len())
}

/// Captured facts from a `lab swarm rotate` run, judged purely from data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub enrolled: BTreeSet<String>,
    pub rotations_before_wall: usize,
    pub rotations_after: usize,
    pub active_after: Option<String>,
    pub proven_after: bool,
    pub a_limited: bool,
    pub launches: Vec<(u32, String, String)>,
    pub turns_on_b: usize,
    pub ledger_proven_b: bool,
}

/// Pure judge over [`Evidence`]: returns every failed invariant (empty = PASS).
#[must_use]
pub fn judge_evidence(ev: &Evidence, login_a: &str, login_b: &str) -> Vec<String> {
    let mut bad = Vec::new();
    let expected_enrolled = BTreeSet::from([LABEL_A.to_owned(), LABEL_B.to_owned()]);
    if ev.enrolled != expected_enrolled {
        bad.push(format!(
            "enrolled set {:?} != expected {:?}",
            ev.enrolled, expected_enrolled
        ));
    }
    if ev.rotations_before_wall != 0 {
        bad.push(format!(
            "rotations[] grew before usage-tap wall ({})",
            ev.rotations_before_wall
        ));
    }
    if !ev.a_limited {
        bad.push(format!(
            "{LABEL_A} was not marked limited by the usage-tap reading"
        ));
    }
    if ev.rotations_after == 0 {
        bad.push("rotations[] did not grow after the usage-tap wall".into());
    }
    if ev.active_after.as_deref() != Some(LABEL_B) {
        bad.push(format!(
            "active account after rotation is {:?}, expected Some({LABEL_B:?})",
            ev.active_after
        ));
    }
    if ev.launches.len() < 2 {
        bad.push(format!(
            "expected at least 2 claude launches (initial + rotated), saw {:?}",
            ev.launches
        ));
    } else {
        let (pid_a, dir_a, _) = &ev.launches[0];
        let (pid_b, dir_b, args_b) = &ev.launches[1];
        if pid_a == pid_b {
            bad.push(format!("second launch reused pid {pid_a}"));
        }
        if dir_a != login_a {
            bad.push(format!(
                "first launch login {dir_a:?} != {LABEL_A} dir {login_a:?}"
            ));
        }
        if dir_b != login_b {
            bad.push(format!(
                "rotated launch login {dir_b:?} != {LABEL_B} dir {login_b:?}"
            ));
        }
        if !args_b.contains(&format!("--resume {CONVERSATION}")) {
            bad.push(format!(
                "rotated launch args {args_b:?} did not carry --resume {CONVERSATION}"
            ));
        }
    }
    if ev.turns_on_b == 0 {
        bad.push(format!("{LABEL_B} served 0 turns after rotation"));
    }
    if !ev.proven_after {
        bad.push(format!(
            "state.proven is false after {LABEL_B} served a turn"
        ));
    }
    if !ev.ledger_proven_b {
        bad.push(format!(
            "accounts/ledger.jsonl recorded no proven event for {LABEL_B}"
        ));
    }
    bad
}

/// Whether `$HOME/.claude/swarm-rotate.json` exists, selecting [`claude_main`] when `argv[0]` is
/// `claude`.
#[must_use]
pub fn standin_configured() -> bool {
    std::env::var_os("HOME")
        .map(|h| Path::new(&h).join(STANDIN_MARKER).is_file())
        .unwrap_or(false)
}

/// Extract `statusLine.command` from `--settings <json>` or `--settings=<json>` in `args`.
#[must_use]
pub fn status_command_from_args(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            break;
        }
        let val = if arg == "--settings" {
            it.next().map(String::as_str)
        } else {
            arg.strip_prefix("--settings=")
        };
        if let Some(json) = val
            && let Ok(parsed) = J::parse(json)
            && let Some(cmd) = parsed
                .get("statusLine")
                .and_then(|s| s.get("command"))
                .and_then(J::as_str)
        {
            return Some(cmd.to_owned());
        }
    }
    None
}

/// Extract `--resume <id>` or `--resume=<id>` from `args`.
#[must_use]
fn resume_from_args(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            break;
        }
        if arg == "--resume" {
            return it.next().cloned();
        }
        if let Some(id) = arg.strip_prefix("--resume=") {
            return Some(id.to_owned());
        }
    }
    None
}

/// Pipe a Claude Code `statusLine` JSON payload into `command` (`/bin/sh -c <command>`) with
/// `CLAUDE_PID` and `ATERM_PARENT_SESSION_ID` set as Claude Code sets them.
fn fire_status_tap(
    command: &str,
    session_id: &str,
    five_pct: u32,
    seven_pct: u32,
    api_ms: u64,
    now: i64,
) {
    let payload = format!(
        "{{\"session_id\":\"{}\",\"version\":\"2.1.300\",\"model\":{{\"id\":\"claude-opus-4-6\"}},\
         \"cost\":{{\"total_api_duration_ms\":{api_ms}}},\
         \"rate_limits\":{{\"five_hour\":{{\"used_percentage\":{five_pct},\"resets_at\":{}}},\
         \"seven_day\":{{\"used_percentage\":{seven_pct},\"resets_at\":{}}}}}}}\n",
        escape(session_id),
        now + 3600,
        now + 7 * 86_400,
    );
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.args(["-c", command])
        .env("CLAUDE_PID", std::process::id().to_string())
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    if std::env::var_os("ATERM_PARENT_SESSION_ID").is_none()
        && let Ok(sid) = std::env::var("ATERM_SESSION_ID")
    {
        cmd.env("ATERM_PARENT_SESSION_ID", sid);
    }
    if let Ok(mut child) = cmd.spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        let _ = child.wait();
    }
}

fn write_standin_records(
    home: &Path,
    cwd: &Path,
    pid: u32,
    proc_start: &str,
    session: &str,
    status: &str,
    append_turn: Option<(&str, &str)>,
) -> std::io::Result<()> {
    let claude_dir = home.join(".claude");
    let cwd_text = cwd.to_string_lossy();
    let slug: String = cwd_text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let project = claude_dir.join("projects").join(slug);
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(claude_dir.join("sessions"))?;
    let transcript_path = project.join(format!("{session}.jsonl"));
    if !transcript_path.exists() {
        let stamp = utc_stamp(now_secs());
        let seed = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"Rotate across enrolled swarm logins.\"}},\"cwd\":\"{}\",\"sessionId\":\"{session}\",\"timestamp\":\"{stamp}\"}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"id\":\"msg_seed_1\",\"role\":\"assistant\",\"model\":\"claude-opus-4-6\",\"content\":[{{\"type\":\"text\",\"text\":\"Working on the task.\"}}]}},\"cwd\":\"{}\",\"sessionId\":\"{session}\",\"timestamp\":\"{stamp}\"}}\n",
            escape(&cwd_text),
            escape(&cwd_text),
        );
        std::fs::write(&transcript_path, seed)?;
    }
    if let Some((prompt, reply)) = append_turn {
        let stamp = utc_stamp(now_secs());
        let rows = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{}\"}},\"cwd\":\"{}\",\"sessionId\":\"{session}\",\"timestamp\":\"{stamp}\"}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"id\":\"msg_{}\",\"role\":\"assistant\",\"model\":\"claude-opus-4-6\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}},\"cwd\":\"{}\",\"sessionId\":\"{session}\",\"timestamp\":\"{stamp}\"}}\n",
            escape(prompt),
            escape(&cwd_text),
            now_ms().unsigned_abs(),
            escape(reply),
            escape(&cwd_text),
        );
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path)?;
        f.write_all(rows.as_bytes())?;
    }
    let session_json = format!(
        "{{\"pid\":{pid},\"sessionId\":\"{session}\",\"cwd\":\"{}\",\"version\":\"2.1.300\",\
         \"status\":\"{status}\",\"statusUpdatedAt\":{},\"procStart\":\"{}\",\
         \"kind\":\"interactive\",\"entrypoint\":\"cli\"}}\n",
        escape(&cwd_text),
        now_ms(),
        escape(proc_start),
    );
    atomic_write(
        &claude_dir.join("sessions").join(format!("{pid}.json")),
        session_json.as_bytes(),
    )
}

fn draw_claude_frame(
    mode: Mode,
    tick: u64,
    served_turns: u64,
    cwd: &Path,
    prompt: &str,
    said: &str,
) -> bool {
    let (cols, rows) = sys::winsize(1).map_or((120, 32), |ws| {
        (
            usize::from(ws.ws_col).max(40),
            usize::from(ws.ws_row).max(16),
        )
    });
    let rule = "─".repeat(cols);
    let clip = |s: &str| -> String { s.chars().take(cols).collect() };
    let mut top = vec![
        String::new(),
        " ▐▛███▛█   Claude Code v2.1.300".to_owned(),
        "▝▜██████▀  Opus 4.6 · Claude Max".to_owned(),
        clip(&format!(" ▝▝   ▝▝   {}", cwd.display())),
        String::new(),
        clip(&format!("❯ {prompt}")),
        String::new(),
        clip(&format!("⏺ {said}")),
    ];
    if mode == Mode::Idle && served_turns > 0 {
        top.push(String::new());
        top.push(clip(&format!(
            "✻ Worked for {}s · done 4:24 PM",
            10 + served_turns
        )));
    }
    let mut bottom = Vec::new();
    if mode == Mode::Busy {
        const GLYPHS: [&str; 6] = ["✢", "✳", "✶", "✻", "✽", "✻"];
        let glyph = GLYPHS[usize::try_from(tick % 6).unwrap_or(0)];
        bottom.push(clip(&format!(
            "{glyph} Working… ({}s · ↓ 1.2k tokens)",
            tick / 4
        )));
        bottom.push(String::new());
    }
    bottom.push(rule.clone());
    bottom.push("❯ ".to_owned());
    bottom.push(rule);
    bottom.push(
        if mode == Mode::Busy {
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt"
        } else {
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)"
        }
        .to_owned(),
    );
    while top.len() + bottom.len() < rows {
        top.push(String::new());
    }
    top.extend(bottom);
    top.truncate(rows);
    let mut frame = String::from("\x1b[H");
    for (i, row) in top.iter().enumerate() {
        frame.push_str(&format!("\x1b[{};1H\x1b[2K{}", i + 1, clip(row)));
    }
    if let Some(r) = top.iter().rposition(|row| row == "❯ ") {
        frame.push_str(&format!("\x1b[{};3H", r + 1));
    }
    let mut out = std::io::stdout().lock();
    out.write_all(frame.as_bytes())
        .and_then(|()| out.flush())
        .is_ok()
}

/// Pure-Rust `claude` stand-in invoked when `$HOME/.claude/swarm-rotate.json` exists.
pub fn claude_main(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("auth") {
        return mailbox::claude_auth_main(&args[1..]);
    }
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("2.1.300 (Claude Code)");
        return 0;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: claude [options] [prompt]");
        return 0;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return 1;
    };
    let marker = scratch_text(&home.join(STANDIN_MARKER));
    let Ok(cfg) = J::parse(&marker) else {
        return 1;
    };
    let Some(ctl_dir) = cfg
        .get("control_dir")
        .and_then(J::as_str)
        .map(PathBuf::from)
    else {
        return 1;
    };
    let login_a = cfg.get("login_a").and_then(J::as_str).unwrap_or("");
    let login = std::env::var(LOGIN_ENV).unwrap_or_default();
    let on_a = login == login_a;
    let pid = std::process::id();
    let proc_start = kernel_start(pid).unwrap_or_default();
    let session = resume_from_args(args).unwrap_or_else(|| CONVERSATION.to_owned());
    let tap_cmd = status_command_from_args(args);
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());

    let _ = std::fs::create_dir_all(&ctl_dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ctl_dir.join("launches.tsv"))
    {
        let _ = writeln!(f, "{pid}\t{login}\t{}", args.join(" "));
    }

    let Ok(signals) = sys::SignalPipe::install(&[libc::SIGTERM, libc::SIGINT, libc::SIGHUP]) else {
        return 1;
    };
    let _raw = sys::RawMode::enter(0);
    {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(b"\x1b[?1049h\x1b[?1004h\x1b[2J\x1b[H");
        let _ = out.flush();
    }

    let mut prompt_shown = "Continue the swarm rotation task.".to_owned();
    let mut said_shown = if on_a {
        "Ready on swarm-a.".to_owned()
    } else {
        "Resumed on swarm-b.".to_owned()
    };
    let _ = write_standin_records(&home, &cwd, pid, &proc_start, &session, "idle", None);
    let mut api_ms: u64 = 500;
    if let Some(cmd) = tap_cmd.as_deref() {
        let initial_pct = if on_a { 20 } else { 10 };
        fire_status_tap(cmd, &session, initial_pct, 10, api_ms, now_secs());
    }

    let mut pending = String::new();
    let mut tick = 0u64;
    let mut served_turns = 0u64;
    loop {
        if !draw_claude_frame(
            Mode::Idle,
            tick,
            served_turns,
            &cwd,
            &prompt_shown,
            &said_shown,
        ) {
            return 0;
        }
        tick = tick.wrapping_add(1);
        let mut fds = [sys::pollfd(0), sys::pollfd(signals.read_fd)];
        match sys::poll(&mut fds, 200) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(_) => return 0,
        }
        if signals
            .drain()
            .iter()
            .any(|s| matches!(*s, libc::SIGTERM | libc::SIGINT | libc::SIGHUP))
        {
            return 0;
        }
        if fds[0].revents != 0 {
            let mut buf = [0u8; 4096];
            let Ok(n) = sys::read(0, &mut buf) else {
                return 0;
            };
            if n == 0 {
                return 0;
            }
            let chunk = String::from_utf8_lossy(&buf[..n]);
            let cleaned = chunk.replace("\x1b[I", "").replace("\x1b[O", "");
            pending.push_str(&cleaned);
            if pending.contains('\r') || pending.contains('\n') {
                let submitted = pending
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("continue")
                    .trim()
                    .to_owned();
                pending.clear();
                let _ =
                    write_standin_records(&home, &cwd, pid, &proc_start, &session, "busy", None);
                // Paint busy for ~1.4s so the supervisor's turn-end loop observes worked > 0.
                for busy_tick in 0..7u64 {
                    if !draw_claude_frame(
                        Mode::Busy,
                        busy_tick,
                        served_turns,
                        &cwd,
                        &submitted,
                        &said_shown,
                    ) {
                        return 0;
                    }
                    let mut sfds = [sys::pollfd(signals.read_fd)];
                    let _ = sys::poll(&mut sfds, 200);
                    if signals
                        .drain()
                        .iter()
                        .any(|s| matches!(*s, libc::SIGTERM | libc::SIGINT | libc::SIGHUP))
                    {
                        return 0;
                    }
                }
                served_turns = served_turns.saturating_add(1);
                prompt_shown = submitted;
                api_ms += 1000;
                let wall_now = on_a && ctl_dir.join("wall-a").exists();
                said_shown = if on_a {
                    if wall_now {
                        "Completed turn on swarm-a (5h window now at 100%).".to_owned()
                    } else {
                        "Served warm-up turn on swarm-a.".to_owned()
                    }
                } else {
                    format!(
                        "Served turn {served_turns} on swarm-b; keeping session active for checks."
                    )
                };
                let _ = write_standin_records(
                    &home,
                    &cwd,
                    pid,
                    &proc_start,
                    &session,
                    "idle",
                    Some((&prompt_shown, &said_shown)),
                );
                if let Some(cmd) = tap_cmd.as_deref() {
                    let five_pct = if wall_now {
                        100
                    } else if on_a {
                        25
                    } else {
                        12
                    };
                    fire_status_tap(cmd, &session, five_pct, 10, api_ms, now_secs());
                }
                let turn_log = if on_a { "turns-a.log" } else { "turns-b.log" };
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(ctl_dir.join(turn_log))
                {
                    let _ = writeln!(f, "{pid}\t{prompt_shown}");
                }
            }
        }
    }
}

fn write_enrolled_login(dir: &Path, label: &str, now: i64) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {}: {e}", dir.display()))?;
    let expires_ms = (now + 86_400) * 1000;
    let creds = format!(
        "{{\"claudeAiOauth\":{{\"accessToken\":\"tok-{label}\",\"refreshToken\":\"ref-{label}\",\
         \"expiresAt\":{expires_ms},\"scopes\":[\"user:inference\",\"user:profile\"],\
         \"subscriptionType\":\"max\"}}}}\n"
    );
    sys::put(
        &dir.join(".credentials.json"),
        0o600,
        &sys::PutSrc::Bytes(creds.as_bytes()),
    )
    .map_err(|e| format!("write .credentials.json in {}: {e}", dir.display()))?;
    let profile = format!(
        "{{\"oauthAccount\":{{\"accountUuid\":\"uuid-{label}\",\
         \"emailAddress\":\"{label}@example.test\",\"organizationType\":\"claude_max\"}},\
         \"hasCompletedOnboarding\":true}}\n"
    );
    sys::put(
        &dir.join(".claude.json"),
        0o600,
        &sys::PutSrc::Bytes(profile.as_bytes()),
    )
    .map_err(|e| format!("write .claude.json in {}: {e}", dir.display()))?;
    Ok(())
}

fn ctl_ok(inst: &Instance, args: &[&str]) -> Result<String, Fail> {
    let r = inst.ctl(args);
    if r.code != 0 {
        return Err(Fail::die(format!(
            "ctl {args:?} exited {}: {} {}",
            r.code,
            r.stdout.trim(),
            r.stderr.trim()
        )));
    }
    Ok(r.stdout)
}

fn parse_launches(text: &str) -> Vec<(u32, String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let pid = parts.next()?.parse::<u32>().ok()?;
            let login = parts.next()?.to_owned();
            let args = parts.next().unwrap_or("").to_owned();
            Some((pid, login, args))
        })
        .collect()
}

fn ledger_has_proven(ledger_text: &str, label: &str) -> bool {
    ledger_text.lines().any(|line| {
        J::parse(line).is_ok_and(|row| {
            row.get("kind").and_then(J::as_str) == Some("proven")
                && row.get("account").and_then(J::as_str) == Some(label)
        })
    })
}

/// Entry point for `targo --unverified lab swarm rotate`.
pub fn cmd_swarm(args: &[String]) -> Res {
    if args.first().map(String::as_str) == Some("mailbox-signin") {
        return mailbox::cmd_mailbox_signin(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("partition") {
        return partition::cmd_partition(&args[1..]);
    }
    let opts = parse_args(args)?;
    let aterm = resolve_aterm(opts.aterm)?;
    let mut world = World::new(SWARM_CONFIG)?;
    if opts.keep {
        world.keep();
    }

    let now = now_secs();
    let harness = world.state().join("harness/claude-harness");
    let store = Store::at(&harness);
    let login_a = store
        .login_dir(LABEL_A)
        .ok_or_else(|| Fail::die("swarm-a has no login_dir"))?;
    let login_b = store
        .login_dir(LABEL_B)
        .ok_or_else(|| Fail::die("swarm-b has no login_dir"))?;
    write_enrolled_login(&login_a, LABEL_A, now)?;
    write_enrolled_login(&login_b, LABEL_B, now)?;

    let claude_home = world.home.join(".claude");
    std::fs::create_dir_all(&claude_home).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&claude_home, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    atomic_write(
        &world.home.join(".claude.json"),
        b"{\"hasCompletedOnboarding\":true}\n",
    )
    .map_err(|e| e.to_string())?;
    atomic_write(
        &claude_home.join("settings.json"),
        b"{\"env\":{\"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC\":\"1\"}}\n",
    )
    .map_err(|e| e.to_string())?;

    let roster_toml = format!(
        "[[account]]\nlabel = \"{LABEL_A}\"\nclass = \"dedicated\"\n\
         email = \"{LABEL_A}@example.test\"\ndir = \"{}\"\n\n\
         [[account]]\nlabel = \"{LABEL_B}\"\nclass = \"dedicated\"\n\
         email = \"{LABEL_B}@example.test\"\ndir = \"{}\"\n",
        login_a.display(),
        login_b.display(),
    );
    let roster = Roster::parse(&roster_toml).map_err(|e| format!("parse roster: {e}"))?;
    let enrolled = accounts::enrolled(&roster);
    let roster_path = world.cfg.join("aterm/accounts.toml");
    atomic_write(&roster_path, roster_toml.as_bytes()).map_err(|e| e.to_string())?;

    // Verify production policy and negative control first on the exact 2-login roster: production
    // rotates `swarm-a -> swarm-b` (`rotations == 1`), whereas a candidate filter that always stays
    // leaves `rotations[]` empty under the same 100% usage-tap reading.
    let mut prod_state = accounts::State::default();
    let (prod_move, prod_rotations) = simulate_tap_wall_step(
        &roster,
        &enrolled,
        &mut prod_state,
        "s-control",
        LABEL_A,
        now,
        CandidatePolicy::Production,
    );
    if prod_move.as_deref() != Some(LABEL_B) || prod_rotations != 1 {
        return Err(Fail::die(format!(
            "production preflight broken: Production produced ({prod_move:?}, rotations={prod_rotations})"
        )));
    }
    let mut stay_state = accounts::State::default();
    let (stay_move, stay_rotations) = simulate_tap_wall_step(
        &roster,
        &enrolled,
        &mut stay_state,
        "s-control",
        LABEL_A,
        now,
        CandidatePolicy::AlwaysStay,
    );
    if stay_move.is_some() || stay_rotations != 0 {
        return Err(Fail::die(format!(
            "negative control broken: AlwaysStay produced ({stay_move:?}, rotations={stay_rotations})"
        )));
    }

    // Ensure `tap::settings_value` has an admissible path without spaces.
    let tap_exe = if tap::exe_admissible(&aterm) {
        aterm.clone()
    } else {
        let link = world.dir.join("aterm-tap");
        std::os::unix::fs::symlink(&aterm, &link).map_err(|e| e.to_string())?;
        link
    };
    let tap_settings = tap::settings_value(&tap_exe)
        .ok_or_else(|| Fail::die("could not build tap --settings value"))?;

    let lab_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let standin_bin = world.dir.join("standin-bin");
    std::fs::create_dir_all(&standin_bin).map_err(|e| e.to_string())?;
    let standin_dash = standin_bin.join("dash");
    std::os::unix::fs::symlink(&lab_exe, &standin_dash).map_err(|e| e.to_string())?;
    let agents_dir = atpkg::store::default_prefix(&world.home).join("agents");
    std::fs::create_dir_all(&agents_dir).map_err(|e| e.to_string())?;
    let claude_bin = agents_dir.join("claude");
    let shebang = format!("#!{}\n", standin_dash.display());
    sys::put(&claude_bin, 0o755, &sys::PutSrc::Bytes(shebang.as_bytes()))
        .map_err(|e| format!("write claude shim {}: {e}", claude_bin.display()))?;
    let work_dir = world.dir.join("work");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let ctl_dir = world.dir.join("swarm");
    std::fs::create_dir_all(&ctl_dir).map_err(|e| e.to_string())?;

    let login_a_str = login_a.to_string_lossy().into_owned();
    let login_b_str = login_b.to_string_lossy().into_owned();
    let marker = J::obj([
        ("control_dir", J::s(ctl_dir.to_string_lossy().into_owned())),
        ("login_a", J::s(&login_a_str)),
        ("login_b", J::s(&login_b_str)),
    ]);
    atomic_write(
        &world.home.join(STANDIN_MARKER),
        marker.dump(COMPACT).as_bytes(),
    )
    .map_err(|e| e.to_string())?;

    let mut env = world.env();
    env.push(("ATERM_LOG".into(), "info".into()));
    env.push((aterm_types::domain::ENV_DEV_FAST_TIMERS.into(), "1".into()));
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.clone(),
            headless: true,
            args: vec![
                "--lines".into(),
                "36".into(),
                "--columns".into(),
                "160".into(),
                "-e".into(),
                "/bin/sh".into(),
            ],
            env,
            cwd: work_dir.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    inst.await_socket(HANG)?;

    let sessions_out = ctl_ok(&inst, &["sessions"])?;
    let sid = sessions_out
        .lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .and_then(|l| l.split_whitespace().nth(1))
        .ok_or_else(|| Fail::die(format!("no initial session in: {sessions_out}")))?
        .to_owned();
    let sel = format!("@{sid}");

    // Enter an interactive job-control bash shell so `relaunch::AccountRestart` has a foreground
    // job shell to resume into.
    ctl_ok(&inst, &[&sel, "send", "/bin/bash --norc --noprofile -i"])?;
    ctl_ok(&inst, &[&sel, "key", "enter"])?;
    if !crate::wait_until(HANG, || {
        inst.ctl(&[&sel, "status"]).stdout.contains("program=bash")
    }) {
        world.keep();
        return Err(Fail::die("tab did not enter interactive bash"));
    }

    ctl_ok(&inst, &[&sel, "meta", "set", "autopilot", "cruise"])?;
    ctl_ok(&inst, &[&sel, "task", "mark", "working"])?;

    let launch_cmd = format!(
        "cd {} && {LOGIN_ENV}={} {} --settings {}",
        shell_quote(&work_dir.to_string_lossy()),
        shell_quote(&login_a_str),
        shell_quote(&claude_bin.to_string_lossy()),
        shell_quote(&tap_settings),
    );
    ctl_ok(&inst, &[&sel, "send", &launch_cmd])?;
    ctl_ok(&inst, &[&sel, "key", "enter"])?;

    if !crate::wait_until(HANG, || {
        let s = inst.ctl(&[&sel, "status"]).stdout;
        field(&s, "program").as_deref() == Some("claude")
            && field(&s, "supervisor")
                .as_deref()
                .is_some_and(|v| v.starts_with("aterm-harness"))
    }) {
        world.keep();
        return Err(Fail::die("supervisor did not attach to claude on swarm-a"));
    }

    // Wait until the initial healthy tap reading on swarm-a is recorded in state.json.
    if !crate::wait_until(HANG, || {
        store
            .load(now_secs())
            .ok()
            .is_some_and(|(st, _)| st.accounts.contains_key(LABEL_A))
    }) {
        world.keep();
        return Err(Fail::die(
            "initial usage-tap reading for swarm-a was not recorded",
        ));
    }
    let rotations_before_wall = store
        .load(now_secs())
        .map(|(st, _)| st.rotations.len())
        .unwrap_or(0);

    // Arm the 100% usage-tap wall for swarm-a's next turn and submit a turn if Cruise hasn't
    // already driven one.
    atomic_write(&ctl_dir.join("wall-a"), b"1\n").map_err(|e| e.to_string())?;
    let _ = inst.ctl(&[&sel, "send", "Continue and check usage tap wall."]);
    let _ = inst.ctl(&[&sel, "key", "enter"]);

    // Wait for `swarm-a` to wall via the usage tap, rotate `active` to `swarm-b`, relaunch
    // `claude` on `login_b` with `--resume`, and serve + prove a turn on `swarm-b`.
    let mut last_step_b: Option<Instant> = None;
    let mut sent_b_turns = 0usize;
    let rotated_and_proven = crate::wait_until(HANG, || {
        let launches = parse_launches(&scratch_text(&ctl_dir.join("launches.tsv")));
        let turns_b = scratch_text(&ctl_dir.join("turns-b.log")).lines().count();
        if launches.len() >= 2 {
            let since = *last_step_b.get_or_insert_with(Instant::now);
            if sent_b_turns < 2
                && turns_b == sent_b_turns
                && since.elapsed() >= Duration::from_secs(3)
            {
                let s = inst.ctl(&[&sel, "status"]).stdout;
                if field(&s, "program").as_deref() == Some("claude") {
                    let prompt = if sent_b_turns == 0 {
                        "Serve a turn on swarm-b."
                    } else {
                        "Confirm proven turn on swarm-b."
                    };
                    let _ = inst.ctl(&[&sel, "send", prompt]);
                    let _ = inst.ctl(&[&sel, "key", "enter"]);
                    sent_b_turns += 1;
                    last_step_b = Some(Instant::now());
                }
            }
        }
        store.load(now_secs()).ok().is_some_and(|(st, _)| {
            !st.rotations.is_empty()
                && st.active.as_deref() == Some(LABEL_B)
                && st.proven
                && turns_b >= 1
        })
    });

    let (final_state, _) = store
        .load(now_secs())
        .map_err(|e| format!("load final accounts state: {e}"))?;
    let now_end = now_secs();
    let a_limited = final_state.accounts.get(LABEL_A).is_some_and(|a| {
        a.low_until(now_end).is_some() && a.sample.as_ref().and_then(|s| s.session_pct) == Some(100)
    });
    let launches = parse_launches(&scratch_text(&ctl_dir.join("launches.tsv")));
    let turns_on_b = scratch_text(&ctl_dir.join("turns-b.log")).lines().count();
    let ledger_text = scratch_text(&harness.join("accounts/ledger.jsonl"));
    let ev = Evidence {
        enrolled,
        rotations_before_wall,
        rotations_after: final_state.rotations.len(),
        active_after: final_state.active.clone(),
        proven_after: final_state.proven,
        a_limited,
        launches,
        turns_on_b,
        ledger_proven_b: ledger_has_proven(&ledger_text, LABEL_B),
    };
    let failures = judge_evidence(&ev, &login_a_str, &login_b_str);
    inst.stop();

    println!("swarm rotate:");
    println!(
        "  enrolled: {:?}  rotations: {} -> {}  active: {:?}  proven: {}",
        ev.enrolled, ev.rotations_before_wall, ev.rotations_after, ev.active_after, ev.proven_after
    );
    println!(
        "  launches: {}  turns_on_{LABEL_B}: {}  stay_control_rotations: {stay_rotations}",
        ev.launches.len(),
        ev.turns_on_b
    );

    if !rotated_and_proven || !failures.is_empty() {
        world.keep();
        let host_log = scratch_text(&world.state().join("logs/aterm.log"));
        let tail: Vec<&str> = host_log
            .lines()
            .filter(|l| l.contains("harness"))
            .rev()
            .take(25)
            .collect();
        for f in &failures {
            println!("  FAIL {f}");
        }
        eprintln!(
            "aterm-lab: swarm rotate harness log tail:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
        return Ok(1);
    }

    println!("swarm rotate: PASS");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_login_fixture(dir: &Path) -> (Roster, BTreeSet<String>, String, String) {
        let now = 1_793_150_000;
        let a = dir.join("login-a");
        let b = dir.join("login-b");
        write_enrolled_login(&a, LABEL_A, now).expect("enroll a");
        write_enrolled_login(&b, LABEL_B, now).expect("enroll b");
        let toml = format!(
            "[[account]]\nlabel = \"{LABEL_A}\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             [[account]]\nlabel = \"{LABEL_B}\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            a.display(),
            b.display(),
        );
        let roster = Roster::parse(&toml).expect("valid roster");
        let enrolled = accounts::enrolled(&roster);
        (
            roster,
            enrolled,
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        )
    }

    /// Production candidate selection rotates `swarm-a -> swarm-b` and grows `rotations[]` when a
    /// 100% usage-tap reading lands; a mutant candidate filter that always stays leaves
    /// `rotations[]` empty and fails [`judge_evidence`].
    #[test]
    fn usage_tap_wall_grows_rotations_and_always_stay_control_fails() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
        let (roster, enrolled, login_a, login_b) = two_login_fixture(&root);
        let now = 1_793_150_100;

        let mut prod_state = accounts::State::default();
        let (moved, rotations) = simulate_tap_wall_step(
            &roster,
            &enrolled,
            &mut prod_state,
            "s-1",
            LABEL_A,
            now,
            CandidatePolicy::Production,
        );
        assert_eq!(moved.as_deref(), Some(LABEL_B));
        assert_eq!(rotations, 1, "rotations[] must grow on production policy");
        let proven_events = accounts::prove(&mut prod_state, LABEL_B, now + 5);
        assert!(
            proven_events
                .iter()
                .any(|e| e.kind == accounts::Kind::Proven),
            "{proven_events:?}"
        );

        let good = Evidence {
            enrolled: enrolled.clone(),
            rotations_before_wall: 0,
            rotations_after: prod_state.rotations.len(),
            active_after: prod_state.active.clone(),
            proven_after: prod_state.proven,
            a_limited: prod_state.accounts.get(LABEL_A).is_some_and(|a| {
                a.low_until(now).is_some()
                    && a.sample.as_ref().and_then(|s| s.session_pct) == Some(100)
            }),
            launches: vec![
                (101, login_a.clone(), "--settings {}".into()),
                (
                    202,
                    login_b.clone(),
                    format!("--settings {{}} --resume {CONVERSATION}"),
                ),
            ],
            turns_on_b: 1,
            ledger_proven_b: true,
        };
        assert_eq!(
            judge_evidence(&good, &login_a, &login_b),
            Vec::<String>::new()
        );

        // Negative control: candidate filter that always stays.
        let mut stay_state = accounts::State::default();
        let (stay_moved, stay_rotations) = simulate_tap_wall_step(
            &roster,
            &enrolled,
            &mut stay_state,
            "s-1",
            LABEL_A,
            now,
            CandidatePolicy::AlwaysStay,
        );
        assert_eq!(stay_moved, None);
        assert_eq!(stay_rotations, 0, "AlwaysStay never grows rotations[]");

        let stay_ev = Evidence {
            rotations_after: stay_state.rotations.len(),
            active_after: stay_state.active.clone(),
            proven_after: stay_state.proven,
            launches: vec![(101, login_a.clone(), "--settings {}".into())],
            turns_on_b: 0,
            ledger_proven_b: false,
            ..good
        };
        let bad = judge_evidence(&stay_ev, &login_a, &login_b);
        assert!(
            bad.iter().any(|m| m.contains("rotations[] did not grow")),
            "AlwaysStay control must fail judge on rotations[]: {bad:?}"
        );
    }

    #[test]
    fn cli_and_settings_parsing_cover_flags_and_negative_cases() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(parse_args(&s(&["rotate"])).unwrap(), RotateOpts::default());
        assert_eq!(
            parse_args(&s(&["rotate", "--aterm", "/tmp/aterm", "--keep"])).unwrap(),
            RotateOpts {
                aterm: Some("/tmp/aterm".into()),
                keep: true,
            }
        );
        assert!(parse_args(&s(&[])).is_err());
        assert!(parse_args(&s(&["other"])).is_err());
        assert!(parse_args(&s(&["rotate", "--unknown"])).is_err());

        let tap_val = tap::settings_value(Path::new("/usr/local/bin/aterm")).expect("tap value");
        assert_eq!(
            status_command_from_args(&s(&["--settings", &tap_val])).as_deref(),
            Some("/usr/local/bin/aterm harness tap")
        );
        assert_eq!(status_command_from_args(&s(&["--model", "opus"])), None);
    }
}
