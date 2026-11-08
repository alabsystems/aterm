// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stand-in `claude` and `build-loop` session fixtures for `fleet-sim`.
//!
//! The `claude` stand-in runs the statusLine command its `--settings` names ([`status_line`]) as
//! Claude Code 2.1.318 does: `/bin/sh -c <command>` with a payload on its stdin (its conversation,
//! its model and the `rate_limits` seeded at [`RATE_LIMITS`]), at its start and every
//! `refreshInterval` seconds, and draws the command's first line under the composer's bottom
//! rule from column 2, cut with `…` where it would reach the last two columns, in the
//! statusLine's ink (`#999999`, its rules in Claude's `#888888`); a command that prints nothing
//! loses the row (aterm-phase's `claude-2.1.318-tap-row-*.txt`,
//! `docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §5.2, §6 S5).

use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::pyjson::{COMPACT, J};

/// Each fixture session's prompt, report and tool, picked by `<n>`: each text near the
/// 400-byte excerpt record, so a fleet read is over its budget and the client's cut is
/// what the simulation judges (`fleet_judge::judge_text`).
const WORDS: &[(&str, &str, &str)] = &[
    (
        "Fix the flaky reconnect test in crates/net: run it fifty times with the seed printed, \
         find which timer it races, and make the test deterministic without slowing the happy \
         path; keep the fix to the reconnect module and its tests, and say what the race was in \
         one sentence before you change anything so I can check it against the logs from CI.",
        "Ran it fifty times: 7 failures, all with the keepalive firing inside the backoff window. \
         The retry loop never resets its backoff after a success, so the second reconnect waits \
         the grown delay and the test's 2 s bound trips. Fix: reset on success in dial_loop and \
         in resume. Result: 50 of 50 green, the happy path unchanged at 41 ms median.",
        "Bash",
    ),
    (
        "Summarize what changed in the renderer since Monday: read the merged commits under \
         crates/aterm-gpu and crates/aterm-gui/src/render, group them by what a person would \
         notice on screen, and flag any change that touches the damage tracker or the glyph \
         cache, since those are the two places last week's flicker report pointed at.",
        "Eleven commits since Monday. What a person would notice: glyph cache eviction now keeps \
         the last frame's glyphs (less pop-in on scroll), the damage tracker merges adjacent \
         rects (fewer partial repaints), and the cursor blink timer pauses while typing. Two \
         touch the damage tracker, one the glyph cache; the flicker report matches the merge.",
        "Read",
    ),
    (
        "Add a --json flag to the export command: every field the text form prints, one object \
         per session, the same order; wire it through the CLI parser and the help, update the \
         tests in export_tests.rs, and do not change the text form's output by a byte, since \
         two scripts in tools/ parse it and I do not want to touch them in this change.",
        "The flag is wired through the parser, the help and the exporter; the text form is \
         byte-identical (checked against the golden files). Two tests in export_tests.rs needed \
         the new field order; both updated, plus one new test for --json with an empty session \
         list. Result: 214 passed, 0 failed, the two tools/ scripts untouched.",
        "Edit",
    ),
    (
        "Audit the reconnect path end to end: read crates/net/src/reconnect.rs and its tests, \
         list every place a backoff timer is created or reset, say which of them a success path \
         skips, and propose the smallest change that makes the flaky test deterministic without \
         slowing the happy path down.",
        "Read reconnect.rs, backoff.rs and the four reconnect tests. Timers are created in three \
         places: the dial loop, the keepalive watchdog and the resume path after a server \
         restart. The dial loop resets its timer on success; the watchdog and the resume path \
         never do, so after one failure every later reconnect waits the grown delay. Smallest \
         fix: reset in both. Result: 50 of 50 runs pass.",
        "Bash",
    ),
    (
        "Profile a cold start of the app on this machine five times, compare the median of each \
         startup phase against last week's numbers in docs/perf/startup.md, and say which phase \
         regressed, by how much, and which commit in that range most likely caused it, with the \
         evidence for that guess.",
        "Five cold starts, medians against docs/perf/startup.md: font discovery 182 ms (was \
         121), config parse 14 ms (was 15), shell spawn 39 ms (was 38), first frame 22 ms (was \
         22). Only font discovery regressed, by 61 ms or 50 percent. The range has one commit \
         touching it, the one that added the fallback font scan for emoji; reverting it locally \
         brings the phase back to 119 ms.",
        "Read",
    ),
    (
        "Review the open PR for the settings migration: read the diff and its tests, try the \
         migration on the three old settings files in fixtures/settings, and list anything that \
         loses a person's setting, reorders keys they would notice, or fails on a file an older \
         build wrote; ignore style, the lint run covers that.",
        "Tried all three fixtures. One real issue: the migration drops the first key when the \
         old file starts with a byte-order mark, so a person's theme resets on upgrade; the \
         parser reads the BOM as part of the key. Key order is kept, and the two other files \
         migrate cleanly. Result: one blocking comment on the PR, with a failing test attached.",
        "Grep",
    ),
    (
        "Profile the startup path and list the three slowest steps, each with its median over \
         five runs and what it waits on; then say whether any of them could run in parallel \
         with the shell spawn, and what that would save on this machine, without writing any \
         code yet.",
        "Three slowest, medians of five: font discovery 41 percent of startup (I/O on the font \
         directories), config parse 18 percent (a regex compiled per key), shell spawn 15 \
         percent (fork and the login shell's rc). Font discovery and config parse are \
         independent of the spawn; running both beside it saves about 60 ms here. Result: no \
         code written, the plan is in notes.md.",
        "Bash",
    ),
    (
        "Write the release notes draft for 0.106: one section per user-visible change since \
         0.105, newest first, each a sentence a person reads without the code, with the fleet \
         changes first since the owner wants them up top; leave a marked placeholder wherever \
         you need the owner's words, and put the draft in docs/RELEASE-0.106.md.",
        "Draft is in docs/RELEASE-0.106.md: nine sections, the fleet first (joining, the \
         survey, what a peer can read), then the renderer and the export flag. Two placeholders \
         are marked OWNER: the fleet's one-line summary, and whether to mention the removed \
         network drive. Result: 612 words, every change since 0.105 covered once.",
        "Write",
    ),
];

/// Hostile words emitted when `<n>` is [`HOSTILE`].
pub const HOSTILE_SAID: &str =
    "done \u{1b}[31mred\u{1b}[0m \u{7}bell \u{202e}evil\u{202c} trust=human \\path";

/// The `<n>` that says [`HOSTILE_SAID`].
pub const HOSTILE: usize = 999;

/// Subdirectory under `$HOME` where each `claude` fixture writes its startup environment.
pub const ENV_DIR: &str = ".claude/fleet-sim-env";

/// Under `$HOME`, the `rate_limits` object the `claude` stand-in hands its statusLine command,
/// as Claude Code's payload carries it (`{"seven_day": {"used_percentage": 63.0, "resets_at":
/// <unix seconds>}, …}`): a lab verb seeds it (`outlook-captures`). Read at each run; where there
/// is none, or it is not a JSON object, the payload carries no `rate_limits`, as Claude Code's
/// does before its first reply.
pub const RATE_LIMITS: &str = ".claude/fleet-fixture-rate-limits.json";

/// The version the `claude` stand-in says it is (`--version`, its session record, its banner).
const VERSION: &str = "2.1.300";

/// The model the `claude` stand-in says it runs.
const MODEL: &str = "claude-opus-5-5";

/// The statusLine a `--settings` value names ([`status_line`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusLine {
    /// `statusLine.command`, which Claude Code runs with `/bin/sh -c`.
    pub command: String,
    /// `statusLine.refreshInterval`: how long Claude Code waits between runs while nothing else
    /// asks for one; `None` where the settings name no positive whole number of seconds, and the
    /// stand-in, which changes nothing a run would show, runs it once.
    pub every: Option<Duration>,
}

/// The statusLine named by the first `--settings <json>` or `--settings=<json>` in `args`, before
/// a literal `--`, whose JSON names a `statusLine.command`; `None` where none does (a settings
/// file's path is not read).
#[must_use]
pub fn status_line(args: &[String]) -> Option<StatusLine> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            break;
        }
        let value = if arg == "--settings" {
            it.next().map(String::as_str)
        } else {
            arg.strip_prefix("--settings=")
        };
        let Some(line) = value
            .and_then(|json| J::parse(json).ok())
            .and_then(|settings| settings.get("statusLine").cloned())
        else {
            continue;
        };
        if let Some(command) = line.get("command").and_then(J::as_str) {
            let every = line
                .get("refreshInterval")
                .and_then(J::as_i64)
                .and_then(|s| u64::try_from(s).ok())
                .filter(|s| *s > 0)
                .map(Duration::from_secs);
            return Some(StatusLine {
                command: command.to_owned(),
                every,
            });
        }
    }
    None
}

/// The payload Claude Code pipes into its statusLine command, as the stand-in builds it: its
/// conversation `session` (`null` where its records could not be written), `cwd`, model and
/// version, no API time, and `rate_limits` where `limits` is a JSON object.
#[must_use]
pub fn status_payload(session: Option<&str>, cwd: &Path, limits: Option<&str>) -> String {
    let mut payload = J::obj([
        ("session_id", session.map_or(J::Null, J::s)),
        ("cwd", J::s(cwd.to_string_lossy())),
        (
            "model",
            J::obj([("id", J::s(MODEL)), ("display_name", J::s("Opus 5.5"))]),
        ),
        ("version", J::s(VERSION)),
        ("cost", J::obj([("total_api_duration_ms", J::Int(0))])),
    ]);
    if let Some(limits) = limits
        .and_then(|text| J::parse(text).ok())
        .filter(|limits| matches!(limits, J::Obj(_)))
    {
        payload.set("rate_limits", limits);
    }
    payload.dump(COMPACT)
}

/// One run of the statusLine `command` as Claude Code runs it: `/bin/sh -c <command>` with
/// `payload` on its stdin and `CLAUDE_PID` naming this process, in this process's directory and
/// environment (the tab's, `ATERM_PARENT_SESSION_ID` among it). Its row: the first line it
/// printed, control characters dropped; `None` where it printed nothing (or only blanks), could
/// not start, or its output was not read, which loses the row.
#[must_use]
fn run_status_line(command: &str, payload: &str) -> Option<String> {
    use aterm_uds::spawnfd::Stdio;
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.args(["-c", command])
        .env("CLAUDE_PID", std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let row: String = text
        .lines()
        .next()?
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    (!row.trim().is_empty()).then_some(row)
}

/// The statusLine's row as Claude Code 2.1.318 draws it under the composer's bottom rule in a
/// pane `cols` wide: indented two cells, and where it would reach the last two columns, cut
/// with `…` at column `cols − 3` (aterm-phase's `claude-2.1.318-tap-row-week-30col.txt`).
#[must_use]
fn status_row(text: &str, cols: usize) -> String {
    let room = cols.saturating_sub(4);
    if text.chars().count() <= room {
        return format!("  {text}");
    }
    let kept: String = text.chars().take(room.saturating_sub(1)).collect();
    format!("  {kept}\u{2026}")
}

/// Run `line` now and then every [`StatusLine::every`], on a thread of its own, each run's
/// payload naming `session`, `cwd` and the `rate_limits` at [`RATE_LIMITS`] as it reads then:
/// the row the latest run printed, `None` until the first run ends and after one that printed
/// nothing.
fn keep_status(line: StatusLine, session: Option<String>, cwd: std::path::PathBuf) -> Shown {
    let shown: Shown = Arc::new(Mutex::new(None));
    let kept = Arc::clone(&shown);
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    std::thread::spawn(move || {
        loop {
            let limits = home
                .as_ref()
                .and_then(|home| crate::util::read_regular_text(&home.join(RATE_LIMITS)).ok());
            let payload = status_payload(session.as_deref(), &cwd, limits.as_deref());
            let row = run_status_line(&line.command, &payload);
            *kept
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = row;
            let Some(every) = line.every else {
                return;
            };
            std::thread::sleep(every);
        }
    });
    shown
}

/// The row the stand-in's statusLine printed last ([`keep_status`]).
type Shown = Arc<Mutex<Option<String>>>;

/// Returns `(prompt, said, tool)` for fixture index `n`.
#[must_use]
pub fn words(n: usize) -> (String, String, &'static str) {
    let (prompt, said, tool) = WORDS[n % WORDS.len()];
    if n == HOSTILE {
        return (prompt.to_owned(), HOSTILE_SAID.to_owned(), tool);
    }
    (prompt.to_owned(), said.to_owned(), tool)
}

/// The screen states the fixture paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// At work: a spinner over the composer, `esc to interrupt`.
    Busy,
    /// An `AskUserQuestion` dialog.
    Question,
    /// A Bash approval box.
    Prompt,
    /// At rest: an empty composer.
    Idle,
}

impl Mode {
    /// Command-line mode name (`claude <word> <n>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Mode::Busy => "busy",
            Mode::Question => "question",
            Mode::Prompt => "prompt",
            Mode::Idle => "idle",
        }
    }

    /// The `agent=` word the server reads from that screen.
    #[must_use]
    pub const fn agent_word(self) -> &'static str {
        match self {
            Mode::Busy => "busy",
            Mode::Question | Mode::Prompt => "prompt",
            Mode::Idle => "idle",
        }
    }

    fn of(word: &str) -> Option<Mode> {
        [Mode::Busy, Mode::Question, Mode::Prompt, Mode::Idle]
            .into_iter()
            .find(|m| m.word() == word)
    }
}

/// Runs the `claude <mode> [<n>]` fixture.
pub fn claude(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--version") {
        println!("{VERSION} (Claude Code fixture of aterm-lab fleet-sim)");
        return 0;
    }
    if args.first().map(String::as_str) == Some("follow") {
        let Some(file) = args.get(1) else {
            eprintln!("claude (fleet-sim fixture): usage: claude follow <mode file>");
            return 2;
        };
        return follow(std::path::Path::new(file), args);
    }
    let Some(mode) = args.first().and_then(|w| Mode::of(w)) else {
        eprintln!(
            "claude (fleet-sim fixture): usage: claude busy|question|prompt|idle [<n>] | follow <mode file>"
        );
        return 2;
    };
    let n = args.get(1).and_then(|n| n.parse().ok()).unwrap_or(0);
    let (prompt, said, tool) = words(n);
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let session = write_session(&cwd, mode, &prompt, &said, tool).ok();
    let _ = write_env();
    let shown = status_line(args).map(|line| keep_status(line, session, cwd.clone()));
    if args.iter().any(|arg| arg == "--subscription-fixture")
        && let Some(home) = std::env::var_os("HOME")
    {
        let _ = write_subscription(std::path::Path::new(&home), crate::util::now_secs());
    }
    let summary: Vec<&str> = prompt.split_whitespace().take(6).collect();
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b]2;\u{2733} {}\x07", summary.join(" "));
    let _ = out.flush();
    drop(out);
    paint_until_gone(|tick, cols, rows| {
        let status = last_row(shown.as_ref());
        let words = Words {
            prompt: &prompt,
            said: &said,
            status: status.as_deref(),
        };
        screen(mode, tick, cols, rows, &cwd, &words)
    })
}

/// `claude follow <mode file>`: the screen of the mode the file names at each frame.
fn follow(file: &std::path::Path, args: &[String]) -> i32 {
    let (prompt, said, tool) = words(0);
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let session = write_session(&cwd, Mode::Idle, &prompt, &said, tool).ok();
    let shown = status_line(args).map(|line| keep_status(line, session, cwd.clone()));
    paint_until_gone(|tick, cols, rows| {
        let status = last_row(shown.as_ref());
        let words = Words {
            prompt: &prompt,
            said: &said,
            status: status.as_deref(),
        };
        screen(followed_mode(file), tick, cols, rows, &cwd, &words)
    })
}

/// The row the statusLine printed last, where the stand-in runs one.
fn last_row(shown: Option<&Shown>) -> Option<String> {
    shown?
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The mode word `file` holds, trimmed; a missing, unreadable or unknown word is [`Mode::Idle`].
#[must_use]
pub fn followed_mode(file: &std::path::Path) -> Mode {
    crate::util::read_regular_text(file)
        .ok()
        .and_then(|word| Mode::of(word.trim()))
        .unwrap_or(Mode::Idle)
}

fn write_subscription(home: &std::path::Path, now: i64) -> std::io::Result<()> {
    let mut file = crate::util::create_new_regular_file(&home.join(".claude.json"), 0o600)?;
    let reset = crate::util::utc_stamp(now + 7 * 24 * 60 * 60);
    let profile = aterm_json::json!({
        "oauthAccount": {
            "accountUuid": "aterm-subscription-fixture",
            "emailAddress": "fixture@example.com",
            "organizationType": "claude_max",
            "userRateLimitTier": "default_claude_max_5x",
            "planDisplayName": "Max 5x"
        },
        "cachedUsageUtilization": {
            "fetchedAtMs": now * 1000,
            "accountUuid": "aterm-subscription-fixture",
            "utilization": {
                "seven_day": { "utilization": 71, "resets_at": reset }
            }
        }
    });
    file.write_all(
        aterm_json::to_string(&profile)
            .map_err(std::io::Error::other)?
            .as_bytes(),
    )
}

/// `build-loop [<n>]`: progress lines until the terminal goes.
pub fn build_loop(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--version") {
        println!("build-loop (aterm-lab fleet-sim fixture)");
        return 0;
    }
    let n: usize = args.first().and_then(|n| n.parse().ok()).unwrap_or(0);
    let mut out = std::io::stdout().lock();
    let start = Instant::now();
    for step in 0u64.. {
        let unit = step % 300 + 1;
        let line = format!(
            "[{:3}%] compiling unit {unit}/300 of target-{n:02} ({}s)\n",
            unit * 100 / 300,
            start.elapsed().as_secs()
        );
        if out
            .write_all(line.as_bytes())
            .and_then(|()| out.flush())
            .is_err()
        {
            return 0;
        }
        if stdin_closed(Duration::from_millis(400)) {
            return 0;
        }
    }
    0
}

fn stdin_closed(wait: Duration) -> bool {
    let mut fds = [crate::sys::pollfd(0)];
    let ms = i32::try_from(wait.as_millis()).unwrap_or(i32::MAX);
    match crate::sys::poll(&mut fds, ms) {
        Ok(0) => false,
        Ok(_) => {
            let mut buf = [0u8; 256];
            !matches!(crate::sys::read(0, &mut buf), Ok(n) if n > 0)
        }
        Err(_) => true,
    }
}

/// The inks Claude Code 2.1.318 draws two kinds of row in on its dark theme, which the stand-in
/// wears so the glass reads its rules and its statusLine's row as it reads Claude's (the glass's
/// arcs and tracks start from the rule's grey, `aterm_effects::glass_inks`); every other row is in
/// the terminal's own ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ink {
    /// A rule: `#888888`.
    Rule,
    /// The statusLine's row: `#999999`.
    Status,
}

impl Ink {
    /// The SGR that sets it as the foreground.
    const fn sgr(self) -> &'static str {
        match self {
            Ink::Rule => "\x1b[38;2;136;136;136m",
            Ink::Status => "\x1b[38;2;153;153;153m",
        }
    }

    /// The ink of row `at` of `rows`, a frame [`screen`] drew: a rule (a row of `─` alone) in
    /// [`Ink::Rule`]; the statusLine's row, which stands directly under the composer's bottom
    /// rule (the rule under `❯ `) where there is one and is not the mode row, in [`Ink::Status`];
    /// `None` for the rest.
    fn of(rows: &[String], at: usize) -> Option<Ink> {
        let rule = |row: &str| !row.is_empty() && row.chars().all(|c| c == '\u{2500}');
        let row = rows.get(at)?;
        if rule(row) {
            return Some(Ink::Rule);
        }
        let status = at >= 2
            && rule(&rows[at - 1])
            && rows[at - 2] == "\u{276f} "
            && !row.trim().is_empty()
            && !row.starts_with("  \u{23f5}\u{23f5}");
        status.then_some(Ink::Status)
    }
}

fn paint_until_gone(draw: impl Fn(u64, usize, usize) -> Vec<String>) -> i32 {
    let mut out = std::io::stdout().lock();
    if out.write_all(b"\x1b[?1049h\x1b[2J").is_err() {
        return 0;
    }
    for tick in 0u64.. {
        let (cols, rows) = crate::sys::winsize(1).map_or((100, 30), |ws| {
            (usize::from(ws.ws_col), usize::from(ws.ws_row))
        });
        let drawn = draw(tick, cols.max(20), rows.max(12));
        let mut frame = String::from("\x1b[H");
        for (r, row) in drawn.iter().enumerate() {
            match Ink::of(&drawn, r) {
                Some(ink) => frame.push_str(&format!(
                    "\x1b[{};1H\x1b[2K{}{row}\x1b[39m",
                    r + 1,
                    ink.sgr()
                )),
                None => frame.push_str(&format!("\x1b[{};1H\x1b[2K{row}", r + 1)),
            }
        }
        if let Some(r) = drawn.iter().rposition(|row| row == "❯ ") {
            frame.push_str(&format!("\x1b[{};3H", r + 1));
        }
        if out
            .write_all(frame.as_bytes())
            .and_then(|()| out.flush())
            .is_err()
        {
            return 0;
        }
        if stdin_closed(Duration::from_millis(250)) {
            return 0;
        }
    }
    0
}

fn clip(text: &str, cols: usize) -> String {
    text.chars().take(cols).collect()
}

/// What a frame of the `claude` stand-in says: its prompt, its reply, and its statusLine's row
/// ([`keep_status`]), drawn under the composer's bottom rule where there is one.
struct Words<'a> {
    prompt: &'a str,
    said: &'a str,
    status: Option<&'a str>,
}

fn screen(
    mode: Mode,
    tick: u64,
    cols: usize,
    rows: usize,
    cwd: &std::path::Path,
    words: &Words<'_>,
) -> Vec<String> {
    let (prompt, said) = (words.prompt, words.said);
    let rule = "─".repeat(cols);
    let said_shown: String = said
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut top = vec![
        String::new(),
        format!(" ▐▛███▛█   Claude Code v{VERSION}"),
        "▝▜██████▀  Opus 5.5 with xhigh effort · Claude Max".to_owned(),
        clip(&format!(" ▝▝   ▝▝   {}", cwd.display()), cols),
        String::new(),
        clip(&format!("❯ {prompt}"), cols),
        String::new(),
    ];
    let mut bottom: Vec<String> = Vec::new();
    match mode {
        Mode::Busy | Mode::Idle => {
            top.push(clip(&format!("⏺ {said_shown}"), cols));
            if mode == Mode::Busy {
                const GLYPHS: [&str; 6] = ["✢", "✳", "✶", "✻", "✽", "✻"];
                let glyph = GLYPHS[usize::try_from(tick % 6).unwrap_or(0)];
                bottom.push(clip(
                    &format!(
                        "{glyph} Caramelizing… ({}s · ↓ {:.1}k tokens)",
                        tick / 4,
                        tick as f64 / 40.0
                    ),
                    cols,
                ));
                bottom.push(String::new());
            }
            bottom.push(rule.clone());
            bottom.push("❯ ".to_owned());
            bottom.push(rule);
            bottom.extend(words.status.map(|text| status_row(text, cols)));
            bottom.push(
                if mode == Mode::Busy {
                    "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · ← for agents"
                } else {
                    "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents"
                }
                .to_owned(),
            );
        }
        Mode::Question => {
            top.push("⏺ One question before I start.".to_owned());
            top.push(rule);
            top.extend(
                [
                    " ☐ Scope",
                    "",
                    "Which backend should the change cover?",
                    "",
                    "❯ 1. Both backends (Recommended)",
                    "     Change the macOS and the Linux paths together.",
                    "  2. macOS only",
                    "     Leave the Linux backend for a follow-up.",
                    "  3. High-risk rewrite",
                    "     Replace the path entirely.",
                    "  4. Type something.",
                ]
                .map(str::to_owned),
            );
            top.push("─".repeat(40));
            top.extend(
                [
                    "  5. Chat about this",
                    "",
                    "Enter to select · ↑/↓ to navigate · Esc to cancel",
                ]
                .map(str::to_owned),
            );
        }
        Mode::Prompt => {
            top.push(clip(&format!("⏺ {said_shown}"), cols));
            top.push("  ⎿  $ rm -rf target/fleet-sim-scratch".to_owned());
            top.push(String::new());
            top.push(rule);
            top.extend(
                [
                    " Bash command",
                    "",
                    "   rm -rf target/fleet-sim-scratch",
                    "   Remove the scratch build directory",
                    "",
                    " Do you want to proceed?",
                    " ❯ 1. Yes",
                    "   2. Yes, and don't ask again for rm commands in this project",
                    "   3. No",
                    "",
                    " Esc to cancel · Tab to amend",
                ]
                .map(str::to_owned),
            );
        }
    }
    let mut rows_out = top;
    while rows_out.len() + bottom.len() < rows {
        rows_out.push(String::new());
    }
    rows_out.extend(bottom);
    rows_out.truncate(rows);
    rows_out.into_iter().map(|row| clip(&row, cols)).collect()
}

fn write_env() -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let home = std::env::var_os("HOME").ok_or(std::io::ErrorKind::NotFound)?;
    let dir = std::path::Path::new(&home).join(ENV_DIR);
    std::fs::create_dir_all(&dir)?;
    let mut body = Vec::new();
    for (k, v) in std::env::vars_os() {
        body.extend_from_slice(k.as_bytes());
        body.push(b'=');
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    aterm_uds::atomic_file::write(&dir.join(std::process::id().to_string()), &body, 0o600)
}

fn write_session(
    cwd: &std::path::Path,
    mode: Mode,
    prompt: &str,
    said: &str,
    tool: &str,
) -> std::io::Result<String> {
    use aterm_codec::json::escape;
    let home = std::env::var_os("HOME").ok_or(std::io::ErrorKind::NotFound)?;
    let dir = std::path::Path::new(&home).join(".claude");
    let pid = std::process::id();
    let session = format!(
        "{:08x}-0000-4000-8000-{:012x}",
        pid,
        crate::util::now_ms().unsigned_abs()
    );
    let cwd_text = cwd.to_string_lossy();
    let slug: String = cwd_text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let project = dir.join("projects").join(slug);
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(dir.join("sessions"))?;
    let stamp = crate::util::utc_stamp(crate::util::now_secs());
    let row = |kind: &str, message: String| {
        format!(
            "{{\"type\":\"{kind}\",\"message\":{message},\"cwd\":\"{}\",\"sessionId\":\"{session}\",\"timestamp\":\"{stamp}\"}}\n",
            escape(&cwd_text)
        )
    };
    let mut transcript = row(
        "user",
        format!("{{\"role\":\"user\",\"content\":\"{}\"}}", escape(prompt)),
    );
    transcript.push_str(&row(
        "assistant",
        format!(
            "{{\"id\":\"msg_fleet_sim_1\",\"role\":\"assistant\",\"model\":\"{MODEL}\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}",
            escape(said)
        ),
    ));
    let call = match mode {
        Mode::Idle => None,
        Mode::Question => Some("AskUserQuestion"),
        Mode::Prompt => Some("Bash"),
        Mode::Busy => Some(tool),
    };
    if let Some(name) = call {
        transcript.push_str(&row(
            "assistant",
            format!(
                "{{\"id\":\"msg_fleet_sim_2\",\"role\":\"assistant\",\"model\":\"{MODEL}\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"toolu_fleet_sim\",\"name\":\"{name}\",\"input\":{{}}}}]}}"
            ),
        ));
    }
    crate::util::write_fresh(&project.join(format!("{session}.jsonl")), transcript)?;
    crate::util::write_fresh(
        &dir.join("sessions").join(format!("{pid}.json")),
        format!(
            "{{\"pid\":{pid},\"sessionId\":\"{session}\",\"cwd\":\"{}\",\"startedAt\":{},\"version\":\"{VERSION}\",\"kind\":\"interactive\"}}\n",
            escape(&cwd_text),
            crate::util::now_ms()
        ),
    )?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::harness::tap;

    fn said<'a>(prompt: &'a str, said: &'a str, status: Option<&'a str>) -> Words<'a> {
        Words {
            prompt,
            said,
            status,
        }
    }

    /// The row under the last composer bottom rule of `rows` (the rule spans the whole width).
    fn under_the_rule(rows: &[String]) -> Option<&str> {
        let rule = rows
            .iter()
            .rposition(|r| !r.is_empty() && r.chars().all(|c| c == '\u{2500}'))?;
        rows.get(rule + 1).map(String::as_str)
    }

    /// The stand-in draws its statusLine's row where Claude Code 2.1.318 drew it in every
    /// measured screen of aterm-phase's `claude-2.1.318-tap-row-*` fixtures (the row under the
    /// composer's bottom rule, from column 2, cut with `…` at `cols − 3` in the 30-column pane),
    /// and a silent command's screen has no row there. The drawn rows read as the tap's own words
    /// ([`tap::parse_line`], the glass's claim) at 170, 100 and 70 columns and not at 30.
    /// NEGATIVE CONTROLS: the row drawn uncut at 30 columns is not the measured one, and with a
    /// row the silent screen's mode row would not sit under the rule.
    #[test]
    fn the_status_row_is_where_claude_2_1_318_draws_it() {
        const WEEK: &str = "week 63% used \u{b7} resets Sat 10:00";
        let cwd = std::path::Path::new("/tmp/af/m00/home");
        let fixtures = aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures");
        for (name, words, claimed) in [
            ("claude-2.1.318-tap-row-week.txt", Some(WEEK), true),
            ("claude-2.1.318-tap-row-week-100col.txt", Some(WEEK), true),
            ("claude-2.1.318-tap-row-week-70col.txt", Some(WEEK), true),
            ("claude-2.1.318-tap-row-week-30col.txt", Some(WEEK), false),
            (
                "claude-2.1.318-tap-row-no-figure.txt",
                Some(tap::NO_WEEK),
                true,
            ),
            ("claude-2.1.318-tap-row-silent-command.txt", None, false),
        ] {
            let path = fixtures.join(name);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let measured: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            let measured_row = under_the_rule(&measured).expect(name);
            let cols = measured
                .iter()
                .rev()
                .find(|r| r.starts_with('\u{2500}'))
                .map(|r| r.chars().count())
                .expect(name);
            let drawn = screen(Mode::Idle, 3, cols, 40, cwd, &said("p", "s", words));
            let row = under_the_rule(&drawn).expect(name);
            match words {
                Some(_) => assert_eq!(row, measured_row, "{name} at {cols} columns"),
                None => {
                    assert!(row.starts_with("  \u{23f5}\u{23f5}"), "{name}: {row:?}");
                    assert!(measured_row.starts_with("  \u{23f5}\u{23f5}"), "{name}");
                    let with = screen(Mode::Idle, 3, cols, 40, cwd, &said("p", "s", Some(WEEK)));
                    assert_ne!(
                        under_the_rule(&with),
                        Some(measured_row),
                        "CONTROL: a row would stand where the silent screen has none"
                    );
                }
            }
            let parsed = row.strip_prefix("  ").and_then(tap::parse_line);
            assert_eq!(parsed.is_some(), claimed, "{name}: {row:?}");
        }
        assert_ne!(
            format!("  {WEEK}"),
            status_row(WEEK, 30),
            "CONTROL: uncut, the 30-column row would run into the last two columns"
        );
    }

    /// The stand-in draws its rules in Claude's rule grey and its statusLine's row in the
    /// statusLine's ink, and the rest in the terminal's own: the composer's two rules, the row
    /// under the bottom one where a statusLine printed one, never the prompt or the mode row.
    /// NEGATIVE CONTROL: with no statusLine row the mode row stands under the bottom rule, and it
    /// keeps the terminal's ink.
    #[test]
    fn the_rules_and_the_status_row_wear_claudes_inks() {
        let cwd = std::path::Path::new("/tmp/af/m00/home");
        for mode in [Mode::Idle, Mode::Busy] {
            let rows = screen(mode, 3, 100, 30, cwd, &said("p", "s", Some("week 5% used")));
            let inks: Vec<Option<Ink>> = (0..rows.len()).map(|at| Ink::of(&rows, at)).collect();
            assert_eq!(
                inks[25..],
                [
                    Some(Ink::Rule),
                    None,
                    Some(Ink::Rule),
                    Some(Ink::Status),
                    None
                ],
                "{mode:?}: {rows:?}"
            );
            assert_eq!(rows[28], "  week 5% used");
            let silent = screen(mode, 3, 100, 30, cwd, &said("p", "s", None));
            assert!(silent[29].starts_with("  \u{23f5}\u{23f5}"), "{silent:?}");
            assert_eq!(
                Ink::of(&silent, 29),
                None,
                "CONTROL: {mode:?}: the mode row"
            );
            assert_eq!(Ink::of(&silent, 28), Some(Ink::Rule));
        }
    }

    /// `status_line` reads the statusLine command and its refresh from either spelling of
    /// `--settings` (aterm's tap value among them), not past a literal `--`; a settings value that
    /// names no command, or a flag that is not `--settings`, names none.
    #[test]
    fn the_settings_name_the_status_command_and_its_refresh() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        let tap_val =
            tap::settings_value(std::path::Path::new("/usr/local/bin/aterm")).expect("tap value");
        let named = StatusLine {
            command: "/usr/local/bin/aterm harness tap".into(),
            every: Some(Duration::from_secs(10)),
        };
        assert_eq!(
            status_line(&s(&["idle", "--settings", &tap_val])),
            Some(named.clone())
        );
        assert_eq!(
            status_line(&s(&[&format!("--settings={tap_val}"), "idle"])),
            Some(named)
        );
        assert_eq!(
            status_line(&s(&[
                "--settings",
                r#"{"statusLine":{"type":"command","command":"x"}}"#
            ])),
            Some(StatusLine {
                command: "x".into(),
                every: None,
            })
        );
        assert_eq!(status_line(&s(&["--", "--settings", &tap_val])), None);
        assert_eq!(
            status_line(&s(&["--settings", r#"{"model":"opus"}"#])),
            None
        );
        assert_eq!(status_line(&s(&["--model", "opus"])), None);
    }

    /// One run hands the command the payload on its stdin and `CLAUDE_PID` naming the stand-in,
    /// and its row is the first line it printed; the payload carries the seeded `rate_limits` as
    /// the tap reads them ([`tap::Reading::of_stdin`]). A command that prints nothing, or only a
    /// blank line, has no row (Claude Code then loses the row). NEGATIVE CONTROL: a seed that is
    /// not a JSON object leaves the payload without `rate_limits`, and the tap reads no figure.
    #[test]
    fn a_run_hands_the_payload_and_shows_the_first_line() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let kept = scratch.path().join("payload");
        let limits = r#"{"seven_day":{"used_percentage":63.0,"resets_at":1793000000}}"#;
        let payload = status_payload(Some("s"), scratch.path(), Some(limits));
        let row = run_status_line(
            &format!(
                "cat > '{}'; printf 'week %s\\nsecond\\n' \"$CLAUDE_PID\"",
                kept.display()
            ),
            &payload,
        );
        assert_eq!(row, Some(format!("week {}", std::process::id())));
        assert_eq!(std::fs::read_to_string(&kept).expect("handed"), payload);
        let vars = tap::TapVars {
            sid: Some("s-01".into()),
            pid: Some("7".into()),
            ..tap::TapVars::default()
        };
        let read = tap::Reading::of_stdin(&payload, &vars, 1_792_900_000).expect("a reading");
        let week = read.seven_day.expect("the seeded week");
        assert_eq!((week.x10(), week.resets_at), (630, 1_793_000_000));
        assert_eq!(run_status_line("true", &payload), None);
        assert_eq!(run_status_line("printf '\\n'", &payload), None);
        let unseeded = status_payload(None, scratch.path(), Some("[1, 2]"));
        assert!(!unseeded.contains("rate_limits"), "{unseeded}");
        let read = tap::Reading::of_stdin(&unseeded, &vars, 1_792_900_000).expect("a reading");
        assert!(!read.has_figures(), "CONTROL: no seed, no figure");
    }

    #[test]
    fn every_frame_fills_its_screen_and_busy_says_esc_to_interrupt() {
        let cwd = std::path::Path::new("/tmp/af/m00/home");
        for cols in [20, 46, 60, 100] {
            for mode in [Mode::Busy, Mode::Question, Mode::Prompt, Mode::Idle] {
                let rows = screen(
                    mode,
                    7,
                    cols,
                    30,
                    cwd,
                    &said("a prompt", HOSTILE_SAID, None),
                );
                assert_eq!(rows.len(), 30, "{mode:?}: {cols}");
                assert!(
                    rows.iter().all(|r| r.chars().count() <= cols),
                    "{mode:?}: {cols}"
                );
                if matches!(mode, Mode::Busy | Mode::Idle) {
                    assert_eq!(rows.iter().rposition(|row| row == "❯ "), Some(27));
                    assert!(rows[28].chars().all(|ch| ch == '─'));
                }
                assert!(
                    rows.iter().all(|r| !r.chars().any(char::is_control)),
                    "{mode:?}"
                );
            }
        }
        let busy = screen(Mode::Busy, 7, 100, 30, cwd, &said("p", "s", None));
        assert!(busy[29].contains("esc to interrupt"), "{busy:?}");
        let idle = screen(Mode::Idle, 7, 100, 30, cwd, &said("p", "s", None));
        assert!(!idle[29].contains("esc to interrupt"), "{idle:?}");
    }

    #[test]
    fn a_followed_mode_is_its_files_word_and_idle_without_one() {
        let scratch = aterm_tempfile::tempdir().expect("exclusive mode file");
        let file = scratch.path().join("mode");
        assert_eq!(followed_mode(&file), Mode::Idle, "no file yet");
        for (word, mode) in [
            ("busy\n", Mode::Busy),
            ("prompt", Mode::Prompt),
            ("question\n", Mode::Question),
            ("idle", Mode::Idle),
            ("busy", Mode::Busy),
            ("asleep", Mode::Idle),
        ] {
            std::fs::write(&file, word).expect("write the mode");
            assert_eq!(followed_mode(&file), mode, "{word:?}");
        }
    }

    #[test]
    fn the_hostile_index_says_the_hostile_words() {
        assert_eq!(words(HOSTILE).1, HOSTILE_SAID);
        assert_ne!(words(1).1, HOSTILE_SAID);
    }
}
