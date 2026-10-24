// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stand-in `claude` and `build-loop` session fixtures for `fleet-sim`.

use std::io::Write as _;
use std::time::{Duration, Instant};

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
        println!("2.1.300 (Claude Code fixture of aterm-lab fleet-sim)");
        return 0;
    }
    if args.first().map(String::as_str) == Some("follow") {
        let Some(file) = args.get(1) else {
            eprintln!("claude (fleet-sim fixture): usage: claude follow <mode file>");
            return 2;
        };
        return follow(std::path::Path::new(file));
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
    let _ = write_session(&cwd, mode, &prompt, &said, tool);
    let _ = write_env();
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
    paint_until_gone(|tick, cols, rows| screen(mode, tick, cols, rows, &cwd, &prompt, &said))
}

fn follow(file: &std::path::Path) -> i32 {
    let (prompt, said, tool) = words(0);
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let _ = write_session(&cwd, Mode::Idle, &prompt, &said, tool);
    paint_until_gone(|tick, cols, rows| {
        screen(followed_mode(file), tick, cols, rows, &cwd, &prompt, &said)
    })
}

/// The mode word `file` holds, trimmed; a missing, unreadable or unknown word is [`Mode::Idle`].
#[must_use]
pub fn followed_mode(file: &std::path::Path) -> Mode {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|word| Mode::of(word.trim()))
        .unwrap_or(Mode::Idle)
}

fn write_subscription(home: &std::path::Path, now: i64) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(home.join(".claude.json"))?;
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
            frame.push_str(&format!("\x1b[{};1H\x1b[2K{row}", r + 1));
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

fn screen(
    mode: Mode,
    tick: u64,
    cols: usize,
    rows: usize,
    cwd: &std::path::Path,
    prompt: &str,
    said: &str,
) -> Vec<String> {
    let rule = "─".repeat(cols);
    let said_shown: String = said
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut top = vec![
        String::new(),
        " ▐▛███▛█   Claude Code v2.1.300".to_owned(),
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
    use std::os::unix::fs::OpenOptionsExt as _;
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
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(dir.join(std::process::id().to_string()))?
        .write_all(&body)
}

fn write_session(
    cwd: &std::path::Path,
    mode: Mode,
    prompt: &str,
    said: &str,
    tool: &str,
) -> std::io::Result<()> {
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
            "{{\"id\":\"msg_fleet_sim_1\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}",
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
                "{{\"id\":\"msg_fleet_sim_2\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"toolu_fleet_sim\",\"name\":\"{name}\",\"input\":{{}}}}]}}"
            ),
        ));
    }
    std::fs::write(project.join(format!("{session}.jsonl")), transcript)?;
    std::fs::write(
        dir.join("sessions").join(format!("{pid}.json")),
        format!(
            "{{\"pid\":{pid},\"sessionId\":\"{session}\",\"cwd\":\"{}\",\"startedAt\":{},\"version\":\"2.1.300\",\"kind\":\"interactive\"}}\n",
            escape(&cwd_text),
            crate::util::now_ms()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_fills_its_screen_and_busy_says_esc_to_interrupt() {
        let cwd = std::path::Path::new("/tmp/af/m00/home");
        for cols in [20, 46, 60, 100] {
            for mode in [Mode::Busy, Mode::Question, Mode::Prompt, Mode::Idle] {
                let rows = screen(mode, 7, cols, 30, cwd, "a prompt", HOSTILE_SAID);
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
        let busy = screen(Mode::Busy, 7, 100, 30, cwd, "p", "s");
        assert!(busy[29].contains("esc to interrupt"), "{busy:?}");
        let idle = screen(Mode::Idle, 7, 100, 30, cwd, "p", "s");
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
